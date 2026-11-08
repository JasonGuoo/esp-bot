//! Explicit local USB development preview; no network listener or model calls.
use std::{collections::VecDeque, fs::{File, OpenOptions}, io::{Read, Write},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc::{self, SyncSender}},
    thread::{self, JoinHandle}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::{Value, json};

const MAGIC: &[u8; 8] = b"\xd79XDBG\x01\xa5";
const MAX_PAYLOAD: usize = 96 * 1024;
const HEADER: usize = 32;
const BOOT_MARKER: &[u8] = b"ESP-ROM:esp32s31-";
// This is the exact ESP-IDF 6.1 S31 RGB driver's early ISR log, which bypasses
// normal log queues and can interrupt a binary JPEG packet on the UART.
const LCD_UNDERRUN: &[u8] = b"lcd.rgb: LCD underrun";

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }

#[derive(Clone, Serialize)]
struct Frame {
    source: &'static str, width: u16, height: u16, sequence: u32,
    device_ms: u32, received_ms: u64, data_url: String,
    #[serde(skip)] bytes: Vec<u8>,
}
#[derive(Clone, Serialize)]
struct Event { id: u64, received_ms: u64, device_ms: Option<u32>, kind: String, detail: String }
#[derive(Default, Serialize)]
struct Snapshot {
    requested: bool, connected: bool, preview: bool, camera_requested: bool, port: Option<String>, error: Option<String>,
    camera_orientation: Option<u8>,
    screen: Option<Frame>, camera: Option<Frame>, events: VecDeque<Event>, last_crash: VecDeque<Event>,
    invalid_packets: u64, received_frames: u64, lcd_underruns: u64, next_event: u64,
    #[serde(skip)] crash_remaining: usize,
}
impl Snapshot {
    fn event(&mut self, kind: &str, detail: &str, device_ms: Option<u32>) {
        self.next_event += 1;
        let event=Event {id:self.next_event,received_ms:now_ms(),device_ms,
            kind:kind.into(),detail:detail.chars().take(1200).collect()};
        let lower=detail.to_ascii_lowercase();
        if lower.contains("guru meditation") || lower.contains("stack overflow") ||
            lower.contains("assert failed") || lower.contains("abort()") {
            self.last_crash.clear(); self.crash_remaining=128;
        }
        if self.crash_remaining>0 {
            self.last_crash.push_back(event.clone()); self.crash_remaining-=1;
        }
        self.events.push_back(event);
        while self.events.len() > 300 { self.events.pop_front(); }
    }
    fn clear_frames(&mut self) { self.screen=None; self.camera=None; }
}

struct Worker { cancel: Arc<AtomicBool>, tx: SyncSender<(String,Instant)>, task: JoinHandle<()> }
#[derive(Default)]
pub struct UsbDebug { state: Arc<Mutex<Snapshot>>, worker: Mutex<Option<Worker>> }

impl UsbDebug {
    pub fn stop(&self) {
        if let Some(worker)=self.worker.lock().unwrap_or_else(|e|e.into_inner()).take() {
            worker.cancel.store(true,Ordering::Relaxed);
            let _=worker.task.join();
        }
        let mut state=self.state.lock().unwrap_or_else(|e|e.into_inner());
        state.requested=false; state.connected=false; state.preview=false; state.camera_requested=false; state.camera_orientation=None; state.clear_frames();
    }
    fn connect(&self, port:&str) -> Result<Value,String> {
        if !ports().contains(&port.to_owned()) {return Err("请选择当前可见的 USB 串口".into());}
        self.stop();
        let cancel=Arc::new(AtomicBool::new(false));
        let (tx,rx)=mpsc::sync_channel::<(String,Instant)>(8);
        let state=Arc::clone(&self.state); let path=port.to_owned(); let flag=Arc::clone(&cancel);
        {let mut state=state.lock().map_err(|_|"USB 状态不可用")?;
            *state=Snapshot::default();state.requested=true;state.preview=true;state.port=Some(path.clone());}
        let task=thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match open_serial(&path) {
                    Ok(mut serial)=>{
                        // A command queued for an earlier connection cannot wake a rebooted Bot.
                        while rx.try_recv().is_ok() {}
                        {let mut s=state.lock().unwrap();s.connected=true;s.error=None;s.clear_frames();
                            s.event("usb.connected","USB 已连接；等待设备启动",None);}
                        let mut parser=Parser::default(); let mut buffer=[0u8;8192];
                        let mut started=Instant::now();let mut last_ping=Instant::now();
                        let mut ready=false;
                        let (mut preview,mut camera_requested)={let s=state.lock().unwrap();(s.preview,s.camera_requested)};
                        while !flag.load(Ordering::Relaxed) {
                            for (command,queued) in rx.try_iter() {
                                if queued.elapsed()>Duration::from_secs(2) {continue;}
                                if command=="preview on" {preview=true;}
                                if command=="preview off" {preview=false;camera_requested=false;state.lock().unwrap().clear_frames();}
                                if command=="camera on" {camera_requested=true;}
                                if command=="camera off" {camera_requested=false;}
                                let _=serial.write_all(format!("{command}\n").as_bytes());
                                state.lock().unwrap().event("usb.command",&command,None);
                            }
                            if !ready && started.elapsed()>Duration::from_secs(5) {
                                if preview {
                                    let _=serial.write_all(b"preview on\n");
                                    if camera_requested {let _=serial.write_all(b"camera on\n");}
                                }
                                let _=serial.write_all(b"camera orient?\n");
                                ready=true;last_ping=Instant::now();
                            } else if ready && preview && last_ping.elapsed()>Duration::from_secs(5) {
                                // Reassert, rather than only renew, the lease: a Bot
                                // reboot can reset its lease without closing this port.
                                let _=serial.write_all(b"preview on\n");
                                if camera_requested {let _=serial.write_all(b"camera on\n");}
                                if state.lock().unwrap().camera_orientation.is_none() {
                                    let _=serial.write_all(b"camera orient?\n");
                                }
                                last_ping=Instant::now();
                            }
                            match serial.read(&mut buffer) {
                                Ok(0)=>{},
                                Ok(length)=>{
                                    for item in parser.push(&buffer[..length]) {
                                        let mut s=state.lock().unwrap();
                                        match item {
                                            Parsed::Log(line)=>{if let Some(line)=safe_log(&line) {
                                                if line.contains("ESP-ROM:esp32s31-") {
                                                    s.clear_frames();s.camera_orientation=None;s.event("usb.reboot","检测到设备重新启动；等待预览恢复",None);
                                                    started=Instant::now();ready=false;
                                                }
                                                if let Some(value)=camera_orientation_from_log(&line) {s.camera_orientation=Some(value);}
                                                s.event("serial.log",&line,log_clock(&line));
                                            }},
                                            Parsed::Invalid=>s.invalid_packets+=1,
                                            Parsed::LcdUnderrun=>{
                                                s.lcd_underruns+=1;
                                                s.event("lcd.underrun","ESP-IDF RGB 驱动报告 LCD underrun（串口早期日志）",None);
                                            },
                                            Parsed::Packet{source,encoding,width,height,sequence,device_ms,payload}=>{
                                                if !s.preview {continue;}
                                                if encoding==1 && (source==1 || source==2) && width>0 && height>0 &&
                                                    payload.starts_with(&[0xff,0xd8]) && payload.ends_with(&[0xff,0xd9]) {
                                                    let frame=Frame {source:if source==1 {"screen"} else {"camera"},width,height,sequence,
                                                        device_ms,received_ms:now_ms(),data_url:format!("data:image/jpeg;base64,{}",STANDARD.encode(&payload)),bytes:payload};
                                                    if source==1 {s.screen=Some(frame);} else {s.camera=Some(frame);}
                                                    s.received_frames+=1;
                                                } else if source==3 && encoding==2 {
                                                    if let Ok(event)=serde_json::from_slice::<Value>(&payload) {
                                                        s.event(event.get("kind").and_then(Value::as_str).unwrap_or("runtime.event"),&event.to_string(),Some(device_ms));
                                                    } else {s.invalid_packets+=1;}
                                                } else {s.invalid_packets+=1;}
                                            }
                                        }
                                    }
                                },
                                Err(e) if e.kind()==std::io::ErrorKind::WouldBlock || e.kind()==std::io::ErrorKind::Interrupted=>{},
                                Err(e)=>{state.lock().unwrap().error=Some(format!("USB 读取失败：{e}"));break;},
                            }
                            if !std::path::Path::new(&path).exists() {break;}
                            thread::sleep(Duration::from_millis(10));
                        }
                        let _=serial.write_all(b"preview off\n");
                        thread::sleep(Duration::from_millis(50));
                    },
                    Err(e)=>{state.lock().unwrap().error=Some(e);},
                }
                {let mut s=state.lock().unwrap();s.connected=false;s.camera_orientation=None;s.clear_frames();
                    s.event("usb.disconnected","USB 已断开；等待原端口恢复",None);}
                for _ in 0..20 {if flag.load(Ordering::Relaxed){break;}thread::sleep(Duration::from_millis(100));}
            }
        });
        *self.worker.lock().map_err(|_|"USB 状态不可用")?=Some(Worker {cancel,tx,task});
        Ok(json!({"connecting":true}))
    }
    fn command(&self, command:&str) -> Result<Value,String> {
        let face_valid = command.strip_prefix("face ")
            .is_some_and(|id| crate::face_catalog_generated::EXPRESSION_IDS.contains(&id));
        let orientation_valid = ["camera orient 0","camera orient 1","camera orient 2","camera orient 3"].contains(&command);
        if !face_valid && !orientation_valid && !["wake","stop","preview on","preview off","camera on","camera off","eye left","eye center","eye right","screen frame","screen snap"].contains(&command) {return Err("不支持的 USB 调试命令".into());}
        {let s=self.state.lock().map_err(|_|"USB 状态不可用")?;
            if !s.connected {return Err("USB 尚未连接".into());}
            if (command=="camera on" || face_valid || orientation_valid) && !s.preview {return Err("请先恢复 USB 预览".into());}}
        let worker=self.worker.lock().map_err(|_|"USB 状态不可用")?;
        let worker=worker.as_ref().ok_or("请先连接 USB")?;
        worker.tx.try_send((command.to_owned(),Instant::now())).map_err(|_|"USB 命令队列繁忙")?;
        if command.starts_with("preview ") {
            let mut s=self.state.lock().map_err(|_|"USB 状态不可用")?;
            s.preview=command=="preview on";if !s.preview {s.camera_requested=false;s.clear_frames();}
        } else if command=="camera on" || command=="camera off" {
            let mut s=self.state.lock().map_err(|_|"USB 状态不可用")?;
            s.camera_requested=command=="camera on";if !s.camera_requested {s.camera=None;}
        }
        Ok(json!({"queued":true}))
    }
    fn snapshot(&self, after:u64) -> Result<Value,String> {
        let state=self.state.lock().map_err(|_|"USB 状态不可用")?;
        let mut value=serde_json::to_value(&*state).map_err(|e|e.to_string())?;
        value["events"]=serde_json::to_value(state.events.iter().filter(|e|e.id>after).collect::<Vec<_>>()).unwrap_or_default();
        Ok(value)
    }
    fn export(&self, source:&str) -> Result<Value,String> {
        let frame={let s=self.state.lock().map_err(|_|"USB 状态不可用")?;
            match source {"screen"=>s.screen.clone(),"camera"=>s.camera.clone(),_=>None}.ok_or("暂无画面")?};
        let dir=std::env::temp_dir().join(format!("espbot-debug-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).map_err(|e|e.to_string())?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&dir,std::fs::Permissions::from_mode(0o700)).map_err(|e|e.to_string())?;}
        let path=dir.join(format!("{source}-{}.jpg",frame.sequence));
        let mut options=OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        options.open(&path).and_then(|mut f|f.write_all(&frame.bytes)).map_err(|e|e.to_string())?;
        Ok(json!({"path":path}))
    }
    fn export_events(&self) -> Result<Value,String> {
        let content={let s=self.state.lock().map_err(|_|"USB 状态不可用")?;
            serde_json::to_vec_pretty(&json!({"port":s.port,"events":s.events,"last_crash":s.last_crash,
                "invalid_packets":s.invalid_packets,"received_frames":s.received_frames,"lcd_underruns":s.lcd_underruns})).map_err(|e|e.to_string())?};
        let dir=std::env::temp_dir().join(format!("espbot-debug-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).map_err(|e|e.to_string())?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&dir,std::fs::Permissions::from_mode(0o700)).map_err(|e|e.to_string())?;}
        let path=dir.join("events.json");
        let mut options=OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        options.open(&path).and_then(|mut f|f.write_all(&content)).map_err(|e|e.to_string())?;
        Ok(json!({"path":path}))
    }
}
impl Drop for UsbDebug {fn drop(&mut self){self.stop();}}

fn ports()->Vec<String> {
    let mut result=std::fs::read_dir("/dev").into_iter().flatten().flatten().filter_map(|entry|{
        let name=entry.file_name().to_string_lossy().into_owned();
        (name.starts_with("cu.usbserial") || name.starts_with("cu.usbmodem") || name.starts_with("cu.SLAB_USBtoUART"))
            .then(||format!("/dev/{name}"))
    }).collect::<Vec<_>>();result.sort();result
}

#[cfg(target_os="macos")]
fn open_serial(path:&str)->Result<File,String> {
    use std::os::{fd::AsRawFd,unix::fs::OpenOptionsExt};
    let file=OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY|libc::O_NONBLOCK|libc::O_NOFOLLOW)
        .open(path).map_err(|e|format!("无法打开 USB 串口：{e}"))?;
    let fd=file.as_raw_fd();
    unsafe {
        let mut tty:libc::termios=std::mem::zeroed();
        if libc::tcgetattr(fd,&mut tty)!=0 {return Err("USB 串口参数读取失败".into());}
        libc::cfmakeraw(&mut tty);libc::cfsetspeed(&mut tty,libc::B115200);
        tty.c_cflag|=libc::CLOCAL|libc::CREAD;tty.c_cflag&=!libc::CRTSCTS;
        tty.c_cc[libc::VMIN]=0;tty.c_cc[libc::VTIME]=0;
        if libc::tcsetattr(fd,libc::TCSANOW,&tty)!=0 {return Err("USB 串口配置失败".into());}
        if libc::ioctl(fd,libc::TIOCEXCL as libc::c_ulong)!=0 {return Err("USB 串口已被占用".into());}
        let flags=libc::TIOCM_DTR|libc::TIOCM_RTS;
        libc::ioctl(fd,libc::TIOCMBIC,&flags);
    }
    Ok(file)
}
#[cfg(not(target_os="macos"))]
fn open_serial(_: &str)->Result<File,String>{Err("当前 USB 调试适配器面向 macOS".into())}

fn safe_log(bytes:&[u8])->Option<String> {
    let raw=std::str::from_utf8(bytes).ok()?;
    let mut clean=String::new();let mut escape=false;
    for ch in raw.chars() {if ch=='\x1b' {escape=true;continue;}if escape {if ch=='m'{escape=false;}continue;}
        if !ch.is_control() || ch=='\t' {clean.push(ch);}}
    let lower=clean.to_ascii_lowercase();
    if lower.contains("wifi:") || lower.contains("password") || lower.contains("authorization") || lower.contains("token=") {return None;}
    let clean=clean.trim();
    (!clean.is_empty() && clean.len()<=2000).then(||clean.to_owned())
}
fn log_clock(line:&str)->Option<u32>{line.split_once('(')?.1.split_once(')')?.0.parse().ok()}
fn camera_orientation_from_log(line:&str)->Option<u8>{
    let value=line.split_once("Camera orientation current=")?.1.split_whitespace().next()?;
    let value=value.parse::<u8>().ok()?;
    (value<=3).then_some(value)
}

#[derive(Default)]
struct Parser {buffer:Vec<u8>, underrun_match:usize}
enum Parsed {Log(Vec<u8>),Invalid,LcdUnderrun,Packet{source:u8,encoding:u8,width:u16,height:u16,sequence:u32,device_ms:u32,payload:Vec<u8>}}
fn u16le(bytes:&[u8],at:usize)->u16{u16::from_le_bytes(bytes[at..at+2].try_into().unwrap())}
fn u32le(bytes:&[u8],at:usize)->u32{u32::from_le_bytes(bytes[at..at+4].try_into().unwrap())}
impl Parser {
    fn push(&mut self, bytes:&[u8])->Vec<Parsed>{
        let mut output=Vec::new();
        // Search raw bytes before packet parsing, including messages injected
        // into binary frames and markers split across serial reads. The marker
        // has no proper prefix that is also a suffix.
        for &byte in bytes {
            if byte==LCD_UNDERRUN[self.underrun_match] {
                self.underrun_match+=1;
                if self.underrun_match==LCD_UNDERRUN.len() {
                    self.underrun_match=0;output.push(Parsed::LcdUnderrun);
                }
            } else {self.underrun_match=usize::from(byte==LCD_UNDERRUN[0]);}
        }
        self.buffer.extend_from_slice(bytes);
        loop {
            if self.buffer.starts_with(MAGIC) {
                if self.buffer.len()<HEADER {break;}
                let length=u32le(&self.buffer,24) as usize;
                if length>MAX_PAYLOAD || length==0 || !(1..=3).contains(&self.buffer[8]) ||
                    u16le(&self.buffer,10)>1920 || u16le(&self.buffer,12)>1080 {
                    self.buffer.drain(..1);output.push(Parsed::Invalid);continue;
                }
                if self.buffer.len()<HEADER+length {
                    // A reboot may interrupt a frame before its advertised end.
                    // The full ROM marker is unique to this device and lets us
                    // recover without waiting for the old payload to arrive.
                    if let Some(pos)=self.buffer[HEADER..].windows(BOOT_MARKER.len()).position(|p|p==BOOT_MARKER) {
                        self.buffer.drain(..HEADER+pos);output.push(Parsed::Invalid);continue;
                    }
                    break;
                }
                let payload=&self.buffer[HEADER..HEADER+length];
                if crc32fast::hash(payload)!=u32le(&self.buffer,28) {
                    if let Some(pos)=self.buffer[HEADER..].windows(BOOT_MARKER.len()).position(|p|p==BOOT_MARKER) {
                        self.buffer.drain(..HEADER+pos);
                    } else {self.buffer.drain(..1);}
                    output.push(Parsed::Invalid);continue;
                }
                output.push(Parsed::Packet{source:self.buffer[8],encoding:self.buffer[9],width:u16le(&self.buffer,10),height:u16le(&self.buffer,12),
                    sequence:u32le(&self.buffer,16),device_ms:u32le(&self.buffer,20),payload:payload.to_vec()});
                self.buffer.drain(..HEADER+length);continue;
            }
            let magic=self.buffer.windows(MAGIC.len()).position(|part|part==MAGIC);
            let newline=self.buffer.iter().position(|b|*b==b'\n');
            if let Some(end)=newline.filter(|end|magic.is_none_or(|start|*end<start)) {
                output.push(Parsed::Log(self.buffer.drain(..=end).collect()));continue;
            }
            if let Some(start)=magic {self.buffer.drain(..start);continue;}
            if self.buffer.len()>4096 {let keep=self.buffer.len()-7;self.buffer.drain(..keep);output.push(Parsed::Invalid);}
            break;
        }
        output
    }
}

#[tauri::command]
pub fn usb_debug_request(hub:tauri::State<'_,Arc<UsbDebug>>,method:String,payload:Option<Value>)->Result<Value,String>{
    let payload=payload.unwrap_or_default();
    match method.as_str(){
        "ports"=>Ok(json!(ports())),
        "connect"=>hub.connect(payload["port"].as_str().ok_or("请选择 USB 串口")?),
        "disconnect"=>{hub.stop();Ok(json!({"disconnected":true}))},
        "command"=>hub.command(payload["command"].as_str().ok_or("缺少命令")?),
        "state"=>hub.snapshot(payload["after"].as_u64().unwrap_or(0)),
        "export"=>hub.export(payload["source"].as_str().ok_or("缺少画面类型")?),
        "export_events"=>hub.export_events(),
        _=>Err("不支持的 USB 调试操作".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn orientation_status_requires_a_valid_device_value() {
        for value in 0..=3 {assert_eq!(camera_orientation_from_log(&format!("I (5) xiaodai: Camera orientation current={value} saved=1")),Some(value));}
        assert_eq!(camera_orientation_from_log("I (5) xiaodai: Camera orientation current=4"),None);
        assert_eq!(camera_orientation_from_log("Camera orientation not saved or changed"),None);
    }
    fn packet(payload:&[u8])->Vec<u8>{let mut p=vec![0u8;HEADER];p[..8].copy_from_slice(MAGIC);p[8]=2;p[9]=1;
        p[24..28].copy_from_slice(&(payload.len() as u32).to_le_bytes());p[28..32].copy_from_slice(&crc32fast::hash(payload).to_le_bytes());p.extend(payload);p}
    #[test] fn split_packet_and_logs(){let bytes=packet(b"\xff\xd8\x00\x0a\xff\xd9");let mut p=Parser::default();
        assert!(p.push(&bytes[..5]).is_empty());let r=p.push(&bytes[5..]);assert!(matches!(&r[0],Parsed::Packet{payload,..} if payload.len()==6));
        assert!(matches!(&p.push(b"I (40) xiaodai: hello\n")[0],Parsed::Log(_)));}
    #[test] fn bad_crc_resynchronizes(){let mut bad=packet(b"bad");bad[HEADER]=0;let good=packet(b"good");bad.extend(good);
        let r=Parser::default().push(&bad);assert!(r.iter().any(|x|matches!(x,Parsed::Invalid)));
        assert!(r.iter().any(|x|matches!(x,Parsed::Packet{payload,..} if payload==b"good")));}
    #[test] fn oversized_length_does_not_allocate(){let mut p=packet(b"ok");p[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut parser=Parser::default();assert!(parser.push(&p).iter().any(|x|matches!(x,Parsed::Invalid)));assert!(parser.buffer.len()<4096);}
    #[test] fn interrupted_packet_recovers_on_reboot(){let mut bytes=packet(&vec![0x55;200]);bytes.truncate(HEADER+12);
        bytes.extend_from_slice(b"ESP-ROM:esp32s31-20251218\r\n");bytes.extend(packet(b"new"));
        let r=Parser::default().push(&bytes);
        assert!(r.iter().any(|x|matches!(x,Parsed::Log(line) if line.starts_with(BOOT_MARKER))));
        assert!(r.iter().any(|x|matches!(x,Parsed::Packet{payload,..} if payload==b"new")));}
    #[test] fn corrupted_complete_packet_recovers_boot_marker(){let mut bytes=packet(&vec![0x55;40]);bytes.truncate(HEADER+12);
        bytes.extend_from_slice(b"ESP-ROM:esp32s31-20251218\r\n");bytes.extend(packet(b"new"));
        let r=Parser::default().push(&bytes);
        assert!(r.iter().any(|x|matches!(x,Parsed::Log(line) if line.starts_with(BOOT_MARKER))));
        assert!(r.iter().any(|x|matches!(x,Parsed::Packet{payload,..} if payload==b"new")));}
    #[test] fn crash_survives_event_eviction(){let mut s=Snapshot::default();s.event("serial.log","Guru Meditation Error",None);
        s.event("serial.log","PC: 0x1234",None);for _ in 0..400 {s.event("other","routine",None);}
        assert_eq!(s.events.len(),300);assert_eq!(s.last_crash.len(),128);
        assert!(s.last_crash.iter().any(|e|e.detail.contains("PC: 0x1234")));}
    #[test] fn underrun_is_seen_even_inside_corrupted_jpeg_and_split_reads(){
        let mut bytes=packet(&vec![0x55;80]);
        bytes.splice(HEADER+12..HEADER+12,b"E (42) lcd.rgb: LCD underrun\r\n".iter().copied());
        bytes.extend(packet(b"next"));
        let mut parser=Parser::default();let mut output=Vec::new();
        for part in bytes.chunks(3) {output.extend(parser.push(part));}
        assert_eq!(output.iter().filter(|x|matches!(x,Parsed::LcdUnderrun)).count(),1);
        assert!(output.iter().any(|x|matches!(x,Parsed::Invalid)));
        assert!(output.iter().any(|x|matches!(x,Parsed::Packet{payload,..} if payload==b"next")));
    }
}
