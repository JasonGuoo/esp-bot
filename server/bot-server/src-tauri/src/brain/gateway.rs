//! Authenticated WSS v3 service. Separate from diagnostic, plaintext LAN v1.
//! Transport framing is ours; TLS and WebSocket are the upstream libraries.
use std::{collections::{BTreeMap, VecDeque}, net::SocketAddr, sync::{Arc, Mutex as StdMutex, RwLock}, time::{Duration, Instant}};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use base64::{engine::general_purpose::STANDARD, Engine};
use tokio::{net::{TcpListener, TcpStream}, sync::{mpsc, watch, Semaphore}, task::{JoinHandle, JoinSet}, time::{interval, timeout}};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_hdr_async_with_config, tungstenite::{handshake::server::{Request, Response, ErrorResponse}, http::StatusCode, protocol::{Message, WebSocketConfig}}};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use super::{Brain, TurnInput, TurnOutput, media, plain_audio};

pub const PATH: &str = "/v3/brain/ws";
const MAX_JSON: usize = 4_500_000;
const DEVICE_CAPTION_BYTES: usize = 383;
const DISPLAY_REPLY_CAPABILITY: &str = "display.reply.v1";

pub struct RunningGateway {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    state: Arc<RwLock<Value>>,
    online: Arc<StdMutex<BTreeMap<String, usize>>>,
}

impl RunningGateway {
    pub fn status(&self) -> Value {
        let mut state=self.state.read().map(|s|s.clone()).unwrap_or_else(|_|json!({"error":"gateway_state_poisoned"}));
        if let Ok(online)=self.online.lock() { state["devices"]=json!(online.keys().collect::<Vec<_>>()); }
        state
    }
    pub async fn stop(mut self) {
        self.shutdown.send_replace(true);
        if let Some(mut task) = self.task.take() {
            if timeout(Duration::from_secs(8), &mut task).await.is_err() { task.abort(); }
        }
    }
}

impl Drop for RunningGateway {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        if let Some(task)=self.task.as_ref(){task.abort();}
    }
}

struct TurnTask(JoinHandle<()>);
impl TurnTask { fn abort(&self) { self.0.abort(); } }
impl Drop for TurnTask { fn drop(&mut self) { self.0.abort(); } }

struct OnlinePeer { id:String, devices:Arc<StdMutex<BTreeMap<String, usize>>> }
impl OnlinePeer {
    fn new(id:String, devices:Arc<StdMutex<BTreeMap<String, usize>>>) -> Self {
        if let Ok(mut entries)=devices.lock() { *entries.entry(id.clone()).or_default()+=1; }
        Self {id,devices}
    }
}
impl Drop for OnlinePeer {
    fn drop(&mut self) {
        if let Ok(mut entries)=self.devices.lock() {
            if let Some(count)=entries.get_mut(&self.id) { *count-=1; if *count==0 {entries.remove(&self.id);} }
        }
    }
}

pub async fn start(brain: Arc<Brain>) -> Result<RunningGateway,String> {
    let config = &brain.config.gateway;
    let certificate = config.certificate_pem.as_ref().ok_or("gateway_certificate_missing")?;
    let key = config.private_key_pem.as_ref().ok_or("gateway_key_missing")?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(key).map_err(|e|format!("gateway_key_read: {e}"))?.permissions().mode() & 0o077 != 0 {
            return Err("gateway_private_key_requires_mode_0600".into());
        }
    }
    let certificates = CertificateDer::pem_file_iter(certificate).map_err(|e|format!("gateway_certificate_read: {e}"))?
        .collect::<Result<Vec<_>,_>>().map_err(|e|format!("gateway_certificate_invalid: {e}"))?;
    let private_key = PrivateKeyDer::from_pem_file(key).map_err(|e|format!("gateway_key_invalid: {e}"))?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions().map_err(|e|e.to_string())?
        .with_no_client_auth().with_single_cert(certificates,private_key).map_err(|e|format!("gateway_tls_invalid: {e}"))?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let address: SocketAddr = config.bind.parse().map_err(|_|"gateway_bind_invalid")?;
    let listener = TcpListener::bind(address).await.map_err(|e|format!("gateway_bind_failed: {e}"))?;
    let local = listener.local_addr().map_err(|e|e.to_string())?;
    let (advertisement,discovery_error) = if local.ip().is_loopback() {(None,None)} else {
        let ca = config.pairing_ca_pem.clone().unwrap_or_else(|| certificate.with_file_name("gateway-ca.pem"));
        match super::discovery::advertise(&brain.config_dir,&ca,local) {
            Ok(ad) => (Some(ad),None), Err(error) => (None,Some(error))
        }
    };
    let state = Arc::new(RwLock::new(json!({"running":true,"bind":local.to_string(),"path":PATH,"tls":true,"protocol":3,
        "bonjour":advertisement.is_some(),"bonjour_error":discovery_error})));
    let state_task = Arc::clone(&state);
    let online = Arc::new(StdMutex::new(BTreeMap::new()));
    let online_task = Arc::clone(&online);
    let capacity = Arc::new(Semaphore::new(config.max_connections.clamp(1,16)));
    let (shutdown, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut peers = JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                accepted = listener.accept() => {
                    let Ok((stream,peer)) = accepted else {break};
                    if local.ip().is_unspecified() && !lan_peer(peer.ip()) { continue; }
                    let Ok(permit) = Arc::clone(&capacity).try_acquire_owned() else {drop(stream);continue};
                    let acceptor = acceptor.clone(); let brain = Arc::clone(&brain); let shutdown = stopped.clone();
                    let online = Arc::clone(&online_task);
                    peers.spawn(async move {let _permit=permit; let _ = serve_peer(stream,acceptor,brain,shutdown,online).await;});
                },
                _ = peers.join_next(), if !peers.is_empty() => {},
            }
        }
        drop(advertisement);
        if let Ok(mut value) = state_task.write() {value["running"]=json!(false);value["bonjour"]=json!(false);}
        // Receivers observe the same shutdown generation; let them cancel their turns.
        let _ = timeout(Duration::from_secs(5),async {while peers.join_next().await.is_some() {}}).await;
        peers.abort_all();
    });
    Ok(RunningGateway {shutdown,task:Some(task),state,online})
}

fn lan_peer(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.is_private() || v.is_link_local() || v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_unique_local() || v.is_unicast_link_local() || v.is_loopback(),
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeToken { epoch:u32, session_id:u32, turn_id:u32, focus_generation:u32 }

struct Engagement {
    conversation_id:String,
    token:RuntimeToken,
    last_turn:Option<u32>,
    last_user:Instant,
    output:Option<DeviceOutput>,
}

#[derive(Clone, Debug)]
struct DeviceOutput { images:bool }

impl DeviceOutput {
    fn negotiate(payload: &Value) -> Result<Option<Self>, String> {
        let Some(output) = payload.get("output") else { return Ok(None) };
        if output.get("version").and_then(Value::as_u64) != Some(1)
            || output.get("audio").and_then(Value::as_bool) != Some(false)
            || !output.get("cards").and_then(Value::as_array)
                .is_some_and(|cards| cards.iter().any(|card| card.as_str() == Some("text"))) {
            return Err("display_output_unsupported".into());
        }
        let image=output.get("image").unwrap_or(&Value::Null);
        let images=output["cards"].as_array().is_some_and(|cards|cards.iter().any(|card|card.as_str()==Some("image")))
            && image["mime"].as_str()==Some("image/jpeg")
            && image["max_width"].as_u64().is_some_and(|n|n>=480)
            && image["max_height"].as_u64().is_some_and(|n|n>=272)
            && image["max_bytes"].as_u64().is_some_and(|n|n>=65536);
        Ok(Some(Self {images}))
    }

    fn confirmed(&self) -> Value {
        if self.images {
            json!({"version":1,"audio":false,"cards":["text","image"],"image":{"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536}})
        } else {
            json!({"version":1,"audio":false,"cards":["text"]})
        }
    }
}

fn device_turn_input(session: &Engagement, turn_id: String, text: Option<String>, wav_base64: Option<String>) -> TurnInput {
    TurnInput {
        turn_id,
        conversation_id: session.conversation_id.clone(),
        text,
        wav_base64,
        profile_id: None,
        speak: Some(session.output.is_none()),
        device_display: session.output.as_ref().is_some_and(|output|output.images),
    }
}

fn device_scene(output: &TurnOutput) -> Value {
    let mut text=output.scene.speech.clone();
    for card in output.scene.cards.iter().filter(|card|card.kind=="steps" && !card.body.is_empty()) {
        text.push('\n');
        if !card.title.is_empty() {
            text.push_str(&card.title);
            text.push('\n');
        }
        text.push_str(&card.body);
    }
    if output.warnings.iter().any(|warning|warning.starts_with("display_image_unavailable:")) {
        let suffix=" 图片暂时无法显示。";
        let mut end=text.len().min(DEVICE_CAPTION_BYTES-suffix.len());
        while !text.is_char_boundary(end) { end-=1; }
        text.truncate(end);
        text.push_str(suffix);
    }
    let speech = &text;
    let mut end = speech.len().min(DEVICE_CAPTION_BYTES);
    while !speech.is_char_boundary(end) { end -= 1; }
    let speech = if end < speech.len() {
        let suffix="…（详情见 Mac）";
        while end > DEVICE_CAPTION_BYTES - suffix.len() || !speech.is_char_boundary(end) { end -= 1; }
        format!("{}{suffix}", &speech[..end])
    } else {
        speech.clone()
    };
    json!({
        "turn_id":output.turn_id, "outcome":output.device_outcome,
        "scene":{"speech":speech,"expression":output.scene.expression,"cards":[]}
    })
}

impl Engagement {
    fn check_token(&self, token:RuntimeToken) -> Result<(),String> {
        // The device advances epoch when it starts a new turn, while retaining
        // the session ID.  Reject rollback, but do not treat the new lease as a
        // different conversation.  The device still checks the complete token
        // against its *current* state before accepting any returned content.
        if token.session_id!=self.token.session_id || token.epoch<self.token.epoch {return Err("runtime_session_stale".into());}
        if token.turn_id<self.token.turn_id {return Err("runtime_turn_stale".into());}
        if token.turn_id==self.token.turn_id && token.epoch!=self.token.epoch {return Err("runtime_turn_stale".into());}
        if token.turn_id>self.token.turn_id && token.epoch<=self.token.epoch {return Err("runtime_turn_stale".into());}
        if self.last_turn.is_some_and(|last|token.turn_id<=last) {return Err("runtime_turn_stale".into());}
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {v:u8,#[serde(rename="type")]kind:String,id:String,method:String,#[serde(default)]payload:Value}

struct Completed {
    id: String,
    turn_id: String,
    token: RuntimeToken,
    compact: bool,
    processing_time: Duration,
    result: Result<TurnOutput, String>,
}
struct DisplayResult {
    token:RuntimeToken,
    turn_id:String,
    image:Option<(String,super::display::PreparedImage)>,
    delivered:Instant,
}
impl DisplayResult {
    fn current(&self, token:RuntimeToken) -> bool {
        self.token==token && self.delivered.elapsed()<Duration::from_secs(30)
    }

    fn chunk(&self, token:RuntimeToken, payload:&Value) -> Result<Value,String> {
        if !self.current(token) {return Err("asset_expired".into());}
        let (id,image)=self.image.as_ref().ok_or("asset_not_found")?;
        if payload["asset_id"].as_str()!=Some(id.as_str()) {return Err("asset_not_found".into());}
        let offset=payload["offset"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or("asset_offset_invalid")?;
        let length=payload["length"].as_u64().and_then(|n|usize::try_from(n).ok())
            .filter(|n|*n>0 && *n<=3072).ok_or("asset_length_invalid")?;
        if offset>=image.bytes.len() {return Err("asset_offset_invalid".into());}
        let end=(offset+length).min(image.bytes.len());
        Ok(json!({"asset_id":id,"offset":offset,"bytes":end-offset,
            "data_base64":STANDARD.encode(&image.bytes[offset..end]),"eof":end==image.bytes.len()}))
    }
}
struct Playback {turn_id:String,stream_id:u32,token:RuntimeToken,pcm:Vec<u8>,offset:usize,sequence:u32}
struct ActivePeer {
    turn_id:String, request_id:String, token:RuntimeToken, task:TurnTask,
    progress_seq:u32, last_stage:Option<&'static str>, last_progress:Instant,
}

fn progress_stage(event: &Value) -> Option<&'static str> {
    match event["kind"].as_str()? {
        "turn.transcribing" => Some("transcribing"),
        "turn.understood" => Some("understood"),
        "turn.preparing_image" => Some("preparing_image"),
        "turn.thinking" | "harness_permission_replied" => Some("working"),
        "harness_tool_activity" if matches!(event["phase"].as_str(),
            Some("session.tool.called" | "session.tool.success" | "session.tool.failed" | "session.shell.started")) => Some("working"),
        "harness_permission_required" | "harness_question_required" => Some("awaiting_parent"),
        _ => None,
    }
}

fn reject(status:StatusCode,reason:&str)->ErrorResponse {
    let mut response=ErrorResponse::new(Some(reason.to_owned())); *response.status_mut()=status; response
}

async fn serve_peer(stream:TcpStream,acceptor:TlsAcceptor,brain:Arc<Brain>,mut shutdown:watch::Receiver<bool>,online:Arc<StdMutex<BTreeMap<String,usize>>>)->Result<(),String> {
    let local_ip=stream.local_addr().map_err(|_|"local_address_unavailable")?.ip();
    let peer_ip=stream.peer_addr().map_err(|_|"peer_address_unavailable")?.ip();
    let tls=timeout(Duration::from_secs(10),acceptor.accept(stream)).await.map_err(|_|"tls_timeout")?.map_err(|_|"tls_handshake_failed")?;
    let identity=Arc::new(std::sync::Mutex::new(None::<(String,String)>));
    let identity_callback=Arc::clone(&identity); let credentials=Arc::clone(&brain.credentials);
    let callback=move |request:&Request,response:Response| {
        if request.uri().path()!=PATH || request.uri().query().is_some() {return Err(reject(StatusCode::NOT_FOUND,"unknown_endpoint"));}
        // Device API only: browser origins cannot use an ambient credential.
        if request.headers().contains_key("origin") {return Err(reject(StatusCode::FORBIDDEN,"device_api_only"));}
        let device=request.headers().get("x-espbot-device").and_then(|v|v.to_str().ok()).unwrap_or("");
        let token=request.headers().get("authorization").and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer ")).unwrap_or("");
        if !credentials.authenticate(device,token) {return Err(reject(StatusCode::UNAUTHORIZED,"device_credential_required"));}
        *identity_callback.lock().map_err(|_|reject(StatusCode::INTERNAL_SERVER_ERROR,"state_unavailable"))?=Some((device.to_owned(),token.to_owned()));
        Ok(response)
    };
    let mut websocket_config=WebSocketConfig::default();
    websocket_config.max_message_size=Some(MAX_JSON); websocket_config.max_frame_size=Some(MAX_JSON);
    websocket_config.write_buffer_size=0; websocket_config.max_write_buffer_size=MAX_JSON+65536;
    let mut socket=timeout(Duration::from_secs(10),accept_hdr_async_with_config(tls,callback,Some(websocket_config)))
        .await.map_err(|_|"ws_timeout")?.map_err(|_|"ws_handshake_failed")?;
    let (device_id,credential)=identity.lock().map_err(|_|"state_unavailable")?.take().ok_or("device_identity_missing")?;
    let mut revoked=brain.credentials.subscribe();
    // Close the gap between callback authentication and subscribing to rotation.
    if !brain.credentials.authenticate(&device_id,&credential){return Err("device_credential_revoked".into());}
    let _online_peer=OnlinePeer::new(device_id.clone(),online);
    let mut cancellations=brain.cancellations();
    let mut progress=brain.progress();
    send(&mut socket, Message::Text(json!({"v":3,"type":"welcome","device_id":device_id,"capabilities":["assistant.text","audio.pcm16","speaker.candidate","scene.cards",DISPLAY_REPLY_CAPABILITY],"sample_rate":16000,"max_audio_seconds":30,"requires_activation":true}).to_string())).await.map_err(|_|"ws_write_failed")?;
    let mut engagement:Option<Engagement>=None;
    let mut audio:Option<(media::AudioInput,RuntimeToken)>=None;
    let mut plain:Option<plain_audio::PlainAudio>=None;
    let mut recent_ids:VecDeque<String>=VecDeque::new();
    let (completion_tx,mut completion_rx)=mpsc::channel::<Completed>(1);
    let mut active:Option<ActivePeer>=None;
    let mut playback:Option<Playback>=None;
    let mut display_result:Option<DisplayResult>=None;
    let mut audio_clock=interval(Duration::from_millis(20));
    audio_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat=interval(Duration::from_secs(15)); heartbeat.tick().await;
    let mut last_packet=Instant::now();
    loop {
        tokio::select! {
            _=shutdown.changed()=>break,
            _=revoked.changed()=>{if !brain.credentials.authenticate(&device_id,&credential){break;}},
            event=progress.recv()=>{
                if let Ok(event)=event {
                    if let (Some(turn),Some(stage))=(active.as_mut(),progress_stage(&event)) {
                        if event["turn_id"].as_str()==Some(turn.turn_id.as_str()) &&
                            engagement.as_ref().is_some_and(|session|session.output.is_some()) {
                            if turn.last_stage==Some(stage) && turn.last_progress.elapsed()<Duration::from_secs(1) {continue;}
                            turn.last_stage=Some(stage);turn.last_progress=Instant::now();
                            turn.progress_seq=turn.progress_seq.saturating_add(1);
                            let mut payload=json!({"seq":turn.progress_seq,"stage":stage});
                            if stage=="understood" {
                                if let Some(preview)=event["preview"].as_str() {payload["preview"]=json!(preview);}
                            }
                            let update=json!({"v":3,"type":"event","event":"turn.progress",
                                "id":turn.request_id,"runtime":turn.token,"payload":payload});
                            if send(&mut socket,Message::Text(update.to_string())).await.is_err(){break;}
                        }
                    }
                }
            },
            cancellation=cancellations.recv()=>{
                let target=match cancellation {
                    Ok(target)=>target,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,
                };
                if active.as_ref().is_some_and(|turn|target.as_ref().map_or(true,|target|target==&turn.turn_id)) {
                    if let Some(turn)=active.take(){
                        turn.task.abort();
                        if send(&mut socket,Message::Text(cancelled_response(&turn))).await.is_err(){break;}
                    }
                }
                if playback.as_ref().is_some_and(|clip|target.as_ref().map_or(true,|target|target==&clip.turn_id)) {
                    if let Some(clip)=playback.take(){
                        let event=json!({"v":3,"type":"event","event":"audio.cancelled","stream_id":clip.stream_id,"runtime":clip.token});
                        if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                    }
                }
                if target.is_none(){
                    audio=None;plain=None;engagement=None;display_result=None;
                    if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled"}).to_string())).await.is_err(){break;}
                }
            },
            _=audio_clock.tick(), if playback.is_some()=>{
                let clip=playback.as_mut().ok_or("playback_missing")?;
                let end=(clip.offset+640).min(clip.pcm.len());
                let frame=media::output_frame(clip.stream_id,clip.sequence,&clip.pcm[clip.offset..end]);
                if let Some(transport)=plain.as_mut() {
                    match timeout(Duration::from_secs(2),transport.outgoing.send(plain_audio::Output::Frame(frame))).await {
                        Ok(Ok(()))=>{},_=>break,
                    }
                } else {
                    match timeout(Duration::from_secs(2),send(&mut socket, Message::Binary(frame))).await {
                        Ok(Ok(()))=>{},_=>break,
                    }
                }
                clip.offset=end;clip.sequence+=1;
                if clip.offset==clip.pcm.len(){
                    if let Some(transport)=plain.as_mut() {
                        let finish=plain_audio::Output::Finish {stream:clip.stream_id,frames:clip.sequence};
                        if !matches!(timeout(Duration::from_secs(2),transport.outgoing.send(finish)).await,Ok(Ok(()))){break;}
                    }
                    let complete=json!({"v":3,"type":"event","event":"audio.complete","stream_id":clip.stream_id,"runtime":clip.token});
                    playback=None;
                    if send(&mut socket, Message::Text(complete.to_string())).await.is_err(){break;}
                }
            },
            _=heartbeat.tick()=>{
                if last_packet.elapsed()>Duration::from_secs(50){break;}
                if active.is_none() && playback.is_none() && engagement.as_ref().is_some_and(|e|e.last_user.elapsed().as_secs()>brain.config.conversation.idle_session_seconds){
                    engagement=None;audio=None;plain=None;display_result=None;
                    if send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.expired"}).to_string())).await.is_err(){break;}
                }
                if audio.as_ref().is_some_and(|(a,_)|a.started.elapsed().as_secs()>35){audio=None;plain=None;}
                if send(&mut socket, Message::Ping(Vec::new())).await.is_err(){break;}
            },
            Some(completed)=completion_rx.recv()=>{
                // A cancel clears active; stale completion is discarded.
                if active.as_ref().is_some_and(|turn|completed.turn_id==turn.turn_id) {
                    active=None;
                    match completed.result {
                        Ok(mut output)=>{
                            let pcm=if completed.compact { None } else {
                                output.audio.take().and_then(|a|a.get("wav_base64").and_then(Value::as_str).and_then(|wav|media::decode_output(wav).ok()))
                            };
                            brain.emit("device.turn.completed",Some(&completed.turn_id),json!({
                                "device_id":device_id,"transcript":output.transcript,
                                "scene":output.scene,"provider_id":output.provider_id,"model_id":output.model_id,
                                "outcome":output.device_outcome,"tts_calls":output.tts_calls,
                                "reply_pcm_bytes":pcm.as_ref().map_or(0, Vec::len),
                                "processing_ms":completed.processing_time.as_millis() as u64,
                                "timings":output.timings,
                                "image_base64":output.display_image.as_ref().map(|image|STANDARD.encode(&image.bytes))
                            }));
                            let stream_id=u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().map_err(|_|"stream_id")?).max(1);
                            let payload=if completed.compact {
                                let mut payload=device_scene(&output);
                                let image=output.display_image.take().map(|image|(format!("img-{}",uuid::Uuid::new_v4().simple()),image));
                                if let Some((id,image))=&image {
                                    payload["scene"]["cards"]=json!([{"kind":"image","title":image.title,"body":"",
                                        "asset":{"id":id,"mime":"image/jpeg","width":image.width,"height":image.height,
                                        "bytes":image.bytes.len(),"sha256":image.digest}}]);
                                }
                                display_result=Some(DisplayResult{token:completed.token,turn_id:completed.turn_id.clone(),image,delivered:Instant::now()});
                                payload
                            } else {serde_json::to_value(output).map_err(|_|"turn_response_invalid")?};
                            let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":payload,
                                "audio":pcm.as_ref().map(|data|json!({"stream_id":stream_id,"format":"pcm_s16le","sample_rate":16000,"channels":1,"bytes":data.len(),"frame_samples":320}))});
                            if send(&mut socket, Message::Text(response.to_string())).await.is_err(){break;}
                            if let Some(pcm)=pcm {
                                playback=Some(Playback{turn_id:completed.turn_id,stream_id,token:completed.token,pcm,offset:0,sequence:0});
                                audio_clock.reset();
                            }
                        },
                        Err(error)=>{
                            brain.emit("device.turn.failed",Some(&completed.turn_id),json!({"device_id":device_id,"code":error.split(':').next().unwrap_or("turn_failed")}));
                            if send(&mut socket, Message::Text(response_error(&completed.id,&error))).await.is_err(){break;}
                        },
                    }
                }
            },
            event=async {
                match plain.as_mut() {
                    Some(transport)=>transport.incoming.recv().await,
                    None=>std::future::pending().await,
                }
            }, if audio.is_some() && plain.is_some()=>{
                let result=match event {
                    Some(plain_audio::Input::Frame(bytes))=>audio.as_mut().ok_or("audio_session_required".to_owned()).and_then(|(a,_)|a.push(&bytes)),
                    _=>Err("plain_audio_disconnected".into()),
                };
                if let Err(error)=result {
                    audio=None;plain=None;
                    let _=send(&mut socket,Message::Text(json!({"v":3,"type":"error","code":error}).to_string())).await;
                }
            },
            incoming=socket.next()=>{
                let Some(Ok(message))=incoming else{break};last_packet=Instant::now();
                match message {
                    Message::Close(_)=>break,
                    Message::Ping(bytes)=>{if send(&mut socket, Message::Pong(bytes)).await.is_err(){break;}},
                    Message::Pong(_)=>{},
                    Message::Binary(bytes)=>{
                        let result=if plain.is_some(){Err("audio_wrong_transport".to_string())}
                            else {audio.as_mut().ok_or_else(||"audio_session_required".to_string()).and_then(|(audio,_)|audio.push(&bytes))};
                        if let Err(error)=result{audio=None;let _=send(&mut socket, Message::Text(json!({"v":3,"type":"error","code":error}).to_string())).await;}
                    },
                    Message::Text(text)=>{
                        let frame:RequestFrame=match serde_json::from_str(&text){Ok(frame)=>frame,Err(_)=>{let _=send(&mut socket, Message::Text(response_error("","invalid_request"))).await;continue;}};
                        if frame.v!=3 || frame.kind!="request" || !super::valid_turn_id(&frame.id){let _=send(&mut socket, Message::Text(response_error(&frame.id,"invalid_request"))).await;continue;}
                        if recent_ids.contains(&frame.id){let _=send(&mut socket, Message::Text(response_error(&frame.id,"duplicate_request"))).await;continue;}
                        recent_ids.push_back(frame.id.clone());if recent_ids.len()>128{recent_ids.pop_front();}
                        let mut pending=false;
                        let result:Result<Value,String>=async {
                            match frame.method.as_str(){
                                "session.open"=>{
                                    if engagement.is_some() || active.is_some() || playback.is_some(){return Err("session_already_open".into());}
                                    let activation=frame.payload.get("activation").and_then(Value::as_str).unwrap_or("");
                                    if activation!="touch" && !(activation=="wake_word" && frame.payload.get("keyword").and_then(Value::as_str)==Some("小呆小呆")){return Err("explicit_activation_required".into());}
                                    let token:RuntimeToken=parse_runtime(&frame.payload)?;
                                    let output=DeviceOutput::negotiate(&frame.payload)?;
                                    let conversation_id=format!("device-{}-{}",device_id,uuid::Uuid::new_v4().simple());
                                    let mut response=json!({"conversation_id":conversation_id,"runtime":token});
                                    if let Some(options)=&output { response["output"]=options.confirmed(); }
                                    engagement=Some(Engagement{conversation_id:conversation_id.clone(),token,last_turn:None,last_user:Instant::now(),output});
                                    Ok(response)
                                },
                                "assistant.cancel"|"session.close"=>{
                                    if let Some(turn)=active.take(){
                                        let _=brain.cancel(Some(&turn.turn_id)).await;turn.task.abort();
                                        send(&mut socket,Message::Text(cancelled_response(&turn))).await?;
                                    }
                                    audio=None;playback=None;plain=None;display_result=None;
                                    if frame.method=="session.close"{engagement=None;}
                                    Ok(json!({"cancelled":true}))
                                },
                                "audio.begin"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || audio.is_some() || playback.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    display_result=None;
                                    if frame.payload.get("format").and_then(Value::as_str)!=Some("pcm_s16le") || frame.payload.get("sample_rate").and_then(Value::as_u64)!=Some(16000) || frame.payload.get("channels").and_then(Value::as_u64)!=Some(1){return Err("audio_format_unsupported".into());}
                                    let stream=stream_id(&frame.payload)?;
                                    let transport=frame.payload.get("transport").and_then(Value::as_str);
                                    let plain_config=match transport {
                                        Some("plain_tcp_v1")=>{
                                            let created=plain_audio::start(local_ip,peer_ip).await?;
                                            let config=json!({"host":created.host.as_str(),"port":created.port,"key":created.key_hex.as_str()});
                                            plain=Some(created);Some(config)
                                        },
                                        None=>{plain=None;None},
                                        _=>return Err("audio_transport_unsupported".into()),
                                    };
                                    audio=Some((media::AudioInput::new(stream),token));session.last_user=Instant::now();
                                    Ok(json!({"stream_id":stream,"max_bytes":media::MAX_INPUT_PCM,"plain_audio":plain_config}))
                                },
                                "display.asset.read"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    display_result.as_ref().ok_or("asset_expired")?.chunk(token,&frame.payload)
                                },
                                "display.report"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    let result=display_result.as_ref().filter(|result|result.current(token)).ok_or("display_result_expired")?;
                                    let part=frame.payload["part"].as_str().ok_or("display_part_invalid")?;
                                    let status=frame.payload["status"].as_str().filter(|s|matches!(*s,"rendered"|"unavailable")).ok_or("display_status_invalid")?;
                                    match part {
                                        "text"=>{},
                                        "image" if result.image.as_ref().is_some_and(|(id,_)|frame.payload["asset_id"].as_str()==Some(id.as_str()))=>{},
                                        _=>return Err("display_part_invalid".into()),
                                    }
                                    // Optional bounded diagnostic code from newer firmware;
                                    // older clients omit it. Never forward arbitrary payloads.
                                    let code=frame.payload["code"].as_str().filter(|code|status=="unavailable"
                                        && !code.is_empty() && code.len()<=64
                                        && code.bytes().all(|byte|byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte==b'_'));
                                    brain.emit("device.display",Some(&result.turn_id),json!({"device_id":device_id,"part":part,"status":status,"code":code,"runtime":token}));
                                    Ok(json!({"recorded":true}))
                                },
                                "assistant.text"|"audio.commit"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || playback.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    display_result=None;
                                    let (text,wav)=if frame.method=="assistant.text"{
                                        if audio.is_some(){return Err("audio_capture_in_progress".into());}
                                        (Some(frame.payload.get("text").and_then(Value::as_str).ok_or("text_required")?.to_owned()),None)
                                    }else{
                                        if let Some(transport)=plain.as_mut() {
                                            let expected=frame.payload.get("frames").and_then(Value::as_u64)
                                                .and_then(|n|u32::try_from(n).ok()).ok_or("audio_frame_count_required")?;
                                            let capture=&mut audio.as_mut().ok_or("audio_session_required")?.0;
                                            finish_plain_input(transport,capture,expected).await?;
                                        }
                                        let (capture,capture_token)=audio.take().ok_or("audio_session_required")?;
                                        if token!=capture_token || capture.stream_id!=stream_id(&frame.payload)?{return Err("audio_session_mismatch".into());}
                                        (None,Some(capture.wav_base64()?))
                                    };
                                    session.token=token;session.last_turn=Some(token.turn_id);session.last_user=Instant::now();
                                    let id=format!("{}-{}",session.conversation_id,token.turn_id);
                                    let input=device_turn_input(session,id.clone(),text,wav);
                                    let compact=session.output.is_some();
                                    let turn_brain=Arc::clone(&brain);let tx=completion_tx.clone();let request_id=frame.id.clone();
                                    let completion_id=id.clone();
                                    let task=tokio::spawn(async move {
                                        let started = Instant::now();
                                        let result = turn_brain.turn(input, false).await;
                                        let _ = tx.send(Completed {
                                            id: request_id, turn_id: completion_id, token, compact,
                                            processing_time: started.elapsed(), result,
                                        }).await;
                                    });
                                    brain.emit("device.turn.started",Some(&id),json!({"device_id":device_id}));
                                    active=Some(ActivePeer{turn_id:id,request_id:frame.id.clone(),token,task:TurnTask(task),
                                        progress_seq:0,last_stage:None,last_progress:Instant::now()});
                                    // Acknowledges admission, not success. The device may
                                    // show "thinking" while the actual result is pending.
                                    send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"turn.accepted","id":frame.id.clone(),"runtime":token}).to_string())).await?;
                                    pending=true;Ok(json!({}))
                                },
                                _=>Err("unsupported_device_method".into()),
                            }
                        }.await;
                        if !pending {
                            let response=match result{
                                Ok(payload) if frame.method=="display.asset.read"=>json!({"v":3,"type":"response","id":frame.id,"ok":true,"runtime":frame.payload["runtime"],"payload":payload}).to_string(),
                                Ok(payload)=>json!({"v":3,"type":"response","id":frame.id,"ok":true,"payload":payload}).to_string(),
                                Err(error)=>response_error(&frame.id,&error)};
                            if send(&mut socket, Message::Text(response)).await.is_err(){break;}
                        }
                    },
                    _=>{},
                }
            },
        }
    }
    if let Some(turn)=active.take(){let _=brain.cancel(Some(&turn.turn_id)).await;turn.task.abort();}
    let _=send(&mut socket, Message::Close(None)).await;
    Ok(())
}

async fn finish_plain_input(transport:&mut plain_audio::PlainAudio,capture:&mut media::AudioInput,expected:u32)->Result<(),String>{
    if expected==0 || expected as usize>media::MAX_INPUT_PCM/2 || capture.sequence>expected {
        return Err("audio_frame_count_invalid".into());
    }
    timeout(Duration::from_secs(3),async {
        while capture.sequence<expected {
            match transport.incoming.recv().await {
                Some(plain_audio::Input::Frame(bytes))=>capture.push(&bytes)?,
                _=>return Err("plain_audio_disconnected".into()),
            }
        }
        Ok(())
    }).await.map_err(|_|"plain_audio_drain_timeout".to_string())?
}

fn parse_runtime(payload:&Value)->Result<RuntimeToken,String>{serde_json::from_value(payload.get("runtime").cloned().ok_or("runtime_token_required")?).map_err(|_|"runtime_token_invalid".into())}
fn stream_id(payload:&Value)->Result<u32,String>{payload.get("stream_id").and_then(Value::as_u64).and_then(|v|u32::try_from(v).ok()).filter(|v|*v>0).ok_or("stream_id_invalid".into())}
fn response_error(id: &str, error: &str) -> String {
    let reason = error.split(':').next().unwrap_or("request_failed");
    let code = match reason {
        "no_speech" | "audio_too_quiet" => "asr_no_speech",
        other => other,
    };
    json!({"v":3,"type":"response","id":id,"ok":false,
        "error":{"code":code,"message":error.chars().take(500).collect::<String>()}}).to_string()
}
fn cancelled_response(turn:&ActivePeer)->String{json!({"v":3,"type":"response","id":turn.request_id,"ok":false,"runtime":turn.token,"turn_id":turn.turn_id,"error":{"code":"cancelled","message":"The active turn was stopped."}}).to_string()}

// Every write has a deadline, including small JSON frames and heartbeat replies.
// A peer that stops reading cannot hold shutdown or credential revocation open.
async fn send(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>,
    message: Message,
) -> Result<(), String> {
    timeout(Duration::from_secs(3), socket.send(message)).await
        .map_err(|_| "ws_write_timeout".to_string())?
        .map_err(|_| "ws_write_failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_quiet_recordings_use_the_device_retry_prompt() {
        for reason in ["no_speech: empty transcription", "audio_too_quiet: low energy", "asr_no_speech"] {
            let response: Value = serde_json::from_str(&response_error("r1", reason)).unwrap();
            assert_eq!(response["id"], "r1");
            assert_eq!(response["error"]["code"], "asr_no_speech");
            assert_eq!(response["error"]["message"], reason);
        }
        let response: Value = serde_json::from_str(&response_error("r1", "brain_busy")).unwrap();
        assert_eq!(response["error"]["code"], "brain_busy");
    }

    #[test]
    fn runtime_leases_advance_without_reopening_the_conversation() {
        let first = RuntimeToken { epoch: 7, session_id: 12, turn_id: 1, focus_generation: 0 };
        let mut session = Engagement { conversation_id: "device-test".into(), token: first,
            last_turn: None, last_user: Instant::now(), output: None };
        assert!(session.check_token(first).is_ok());
        session.last_turn = Some(1);
        assert!(session.check_token(first).is_err());
        let next = RuntimeToken { epoch: 8, turn_id: 2, ..first };
        assert!(session.check_token(next).is_ok());
        assert!(session.check_token(RuntimeToken { epoch: 7, ..next }).is_err());
        assert!(session.check_token(RuntimeToken { session_id: 13, ..next }).is_err());
        session.token = next;
        session.last_turn = Some(2);
        assert!(session.check_token(RuntimeToken { epoch: 7, turn_id: 3, ..next }).is_err());
    }

    #[test]
    fn silent_output_is_explicit_and_caption_respects_utf8_boundary() {
        let token = RuntimeToken { epoch: 7, session_id: 12, turn_id: 1, focus_generation: 0 };
        let mut session = Engagement { conversation_id: "device-test".into(), token,
            last_turn: None, last_user: Instant::now(), output: None };
        assert_eq!(device_turn_input(&session,"old".into(),Some("hello".into()),None).speak,Some(true));
        assert!(DeviceOutput::negotiate(&json!({"output":{"version":1,"audio":true,"cards":["text"]}})).is_err());
        session.output=DeviceOutput::negotiate(&json!({"output":{"version":1,"audio":false,"cards":["text"]}})).unwrap();
        assert_eq!(device_turn_input(&session,"new".into(),Some("hello".into()),None).speak,Some(false));
        assert!(!device_turn_input(&session,"new".into(),Some("hello".into()),None).device_display);
        session.output=DeviceOutput::negotiate(&json!({"output":{"version":1,"audio":false,"cards":["text","image"],
            "image":{"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536}}})).unwrap();
        assert!(device_turn_input(&session,"new".into(),Some("hello".into()),None).device_display);
        let output=TurnOutput {
            turn_id:"new".into(), conversation_id:"device-test".into(), transcript:"private".into(),
            scene:super::super::scene::Scene { speech:"字".repeat(200), expression:"happy".into(), cards:vec![] },
            provider_id:"provider".into(), model_id:"model".into(), harness_session_id:"session".into(),
            audio:None, identity_candidate:None, warnings:vec![], timings:Default::default(), tts_calls:0, display_image:None, device_outcome:"answered".into(),
        };
        let scene=device_scene(&output);
        let speech=scene["scene"]["speech"].as_str().unwrap();
        assert!(speech.len()<=DEVICE_CAPTION_BYTES && speech.ends_with("（详情见 Mac）"));
        assert!(scene.get("transcript").is_none());
    }

    #[test]
    fn image_chunks_require_the_completed_turn_and_reassemble_exactly() {
        let token=RuntimeToken {epoch:7,session_id:12,turn_id:1,focus_generation:0};
        let bytes=(0..65536).map(|n|(n%251) as u8).collect::<Vec<_>>();
        let mut result=DisplayResult {
            token, turn_id:"device-test-1".into(), delivered:Instant::now(),
            image:Some(("img-test".into(),super::super::display::PreparedImage {
                bytes:bytes.clone(),width:480,height:272,digest:"a".repeat(64),title:"test".into(),
            })),
        };
        let mut received=Vec::new();
        while received.len()<bytes.len() {
            let offset=received.len();
            let payload=json!({"asset_id":"img-test","offset":offset,"length":3072});
            let chunk=result.chunk(token,&payload).unwrap();
            let decoded=STANDARD.decode(chunk["data_base64"].as_str().unwrap()).unwrap();
            assert_eq!(chunk["bytes"].as_u64().unwrap() as usize,decoded.len());
            received.extend_from_slice(&decoded);
            assert_eq!(chunk["eof"],received.len()==bytes.len());
        }
        assert_eq!(received,bytes);
        assert!(result.chunk(RuntimeToken {turn_id:2,..token},&json!({"asset_id":"img-test","offset":0,"length":3072})).is_err());
        assert!(result.chunk(token,&json!({"asset_id":"other","offset":0,"length":3072})).is_err());
        assert!(result.chunk(token,&json!({"asset_id":"img-test","offset":0,"length":3073})).is_err());
        result.delivered=Instant::now()-Duration::from_secs(31);
        assert!(result.chunk(token,&json!({"asset_id":"img-test","offset":0,"length":3072})).is_err());
    }

    #[tokio::test]
    async fn authenticated_tls_session_opens_and_unauthorized_upgrade_fails() {
        use tokio_tungstenite::{connect_async_tls_with_config, Connector,
            tungstenite::{client::IntoClientRequest, http::header::{HeaderName,HeaderValue}}};
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let dir=std::env::temp_dir().join(format!("xiaodai-gateway-test-{}",uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).unwrap();
        let _cleanup=TempDir(dir.clone());
        let script=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/create_gateway_certificate.py");
        let generated=std::process::Command::new("python3").arg(script).arg("--directory").arg(dir.join("tls"))
            .args(["--ip","127.0.0.1"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
        assert!(generated.success());
        let mut config=super::super::settings::BrainConfig::default();
        config.gateway.enabled=true;config.gateway.bind="127.0.0.1:0".into();
        config.gateway.certificate_pem=Some(dir.join("tls/gateway-cert.pem"));
        config.gateway.private_key_pem=Some(dir.join("tls/gateway-key.pem"));
        let occupied=TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.harness.base_url=format!("http://{}",occupied.local_addr().unwrap());
        super::super::settings::save(&dir,&config).unwrap();
        let brain=super::super::Brain::new(dir.clone(),None,Arc::new(|_|{})).unwrap();
        let issued=brain.credentials.issue("test-bot").unwrap();
        let token=issued["token"].as_str().unwrap();
        assert!(brain.start().await.unwrap_err().contains("configured port is already in use"));
        assert!(!brain.accepting.load(std::sync::atomic::Ordering::Acquire));
        let address=brain.gateway.lock().await.as_ref().unwrap().status()["bind"].as_str().unwrap().to_owned();
        let url=format!("wss://{address}{PATH}");
        let cert=CertificateDer::pem_file_iter(dir.join("tls/gateway-ca.pem")).unwrap().next().unwrap().unwrap();
        let mut roots=rustls::RootCertStore::empty();roots.add(cert).unwrap();
        let tls=rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let connector=Connector::Rustls(Arc::new(tls));
        let rejected=connect_async_tls_with_config(url.clone(),None,false,Some(connector.clone())).await;
        assert!(matches!(rejected, Err(tokio_tungstenite::tungstenite::Error::Http(ref response)) if response.status().as_u16()==401));
        let mut request=url.into_client_request().unwrap();
        request.headers_mut().insert(HeaderName::from_static("x-espbot-device"),HeaderValue::from_static("test-bot"));
        request.headers_mut().insert(HeaderName::from_static("authorization"),HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
        let (mut socket,_)=connect_async_tls_with_config(request,None,false,Some(connector)).await.unwrap();
        let welcome:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(welcome["type"],"welcome");assert_eq!(welcome["device_id"],"test-bot");
        assert!(welcome["capabilities"].as_array().unwrap().iter().any(|v|v==DISPLAY_REPLY_CAPABILITY));
        assert_eq!(brain.gateway.lock().await.as_ref().unwrap().status()["devices"].as_array().unwrap().len(),1);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"open1","method":"session.open",
            "payload":{"activation":"touch","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}}}).to_string())).await.unwrap();
        let reply:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(reply["ok"],true);assert_eq!(reply["payload"]["runtime"]["session_id"],12);
        assert!(reply["payload"].get("output").is_none());
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"close1","method":"session.close","payload":{}}).to_string())).await.unwrap();
        let closed:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(closed["ok"],true);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"bad-open","method":"session.open",
            "payload":{"activation":"touch","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},
                "output":{"version":1,"audio":true,"cards":["text"]}}}).to_string())).await.unwrap();
        let bad:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(bad["error"]["code"],"display_output_unsupported");
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"silent-open","method":"session.open",
            "payload":{"activation":"touch","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0},
                "output":{"version":1,"audio":false,"cards":["text","image"]}}}).to_string())).await.unwrap();
        let silent:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(silent["payload"]["output"],json!({"version":1,"audio":false,"cards":["text"]}));
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"begin1","method":"audio.begin",
            "payload":{"stream_id":123,"format":"pcm_s16le","sample_rate":16000,"channels":1,
                "transport":"plain_tcp_v1","runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}}}).to_string())).await.unwrap();
        let begun:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(begun["ok"],true);
        let config=&begun["payload"]["plain_audio"];
        let hex=config["key"].as_str().unwrap();
        let secret=(0..32).map(|i|u8::from_str_radix(&hex[i*2..i*2+2],16).unwrap()).collect::<Vec<_>>();
        let mac=ring::hmac::Key::new(ring::hmac::HMAC_SHA256,&secret);
        use tokio::io::AsyncWriteExt;
        let mut pcm=tokio::net::TcpStream::connect((config["host"].as_str().unwrap(),config["port"].as_u64().unwrap() as u16)).await.unwrap();
        for sequence in 0..3 {
            let samples=vec![0u8;640];
            let data=media::output_frame(123,sequence,&samples);
            let mut body=vec![b'I'];body.extend_from_slice(&data);
            let tag=ring::hmac::sign(&mac,&body);body.extend_from_slice(tag.as_ref());
            pcm.write_all(&(body.len() as u32).to_be_bytes()).await.unwrap();
            pcm.write_all(&body).await.unwrap();
        }
        // The TCP media and WSS commit can arrive in either order. The server
        // must drain the requested frame count before admitting a model turn.
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"commit1","method":"audio.commit",
            "payload":{"stream_id":123,"frames":3,"runtime":{"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0}}}).to_string())).await.unwrap();
        let accepted:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(accepted["event"],"turn.accepted");
        // Brain wasn't started, so the test never invokes ASR or a cloud model.
        let rejected_turn:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(rejected_turn["id"],"commit1");assert_eq!(rejected_turn["ok"],false);
        socket.close(None).await.unwrap();brain.stop().await.unwrap();
        assert!(TcpStream::connect(&address).await.is_err());drop(brain);
    }
}
