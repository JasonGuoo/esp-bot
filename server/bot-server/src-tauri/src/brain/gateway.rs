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

mod segments;

pub const PATH: &str = "/v3/brain/ws";
const MAX_JSON: usize = 4_500_000;
const DEVICE_CAPTION_BYTES: usize = 383;
const DISPLAY_REPLY_CAPABILITY: &str = "display.reply.v1";
const AUDIO_REPLY_CAPABILITY: &str = "audio.reply.v1";

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
impl TurnTask {
    fn abort(&self) { self.0.abort(); }
    async fn stop(&mut self) {
        self.abort();
        let _ = (&mut self.0).await;
    }
}
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

struct VolumePeer { brain:Arc<Brain>, device:String, connection:String }
impl Drop for VolumePeer {
    fn drop(&mut self) {
        if self.brain.device_volumes.disconnect(&self.device,&self.connection) {
            self.brain.emit("device.volume",None,json!({"device_id":self.device,"disconnected":true}));
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
struct DeviceOutput { images:bool, audio:bool, presentation:bool }

impl DeviceOutput {
    fn negotiate(payload: &Value) -> Result<Option<Self>, String> {
        let Some(output) = payload.get("output") else { return Ok(None) };
        let audio = output.get("audio").and_then(Value::as_bool).ok_or("display_output_unsupported")?;
        if output.get("version").and_then(Value::as_u64) != Some(1)
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
        let presentation=output.pointer("/presentation/version").and_then(Value::as_u64)==Some(1)
            && output.pointer("/presentation/frame_ack").and_then(Value::as_bool)==Some(true);
        Ok(Some(Self {images,audio,presentation}))
    }

    fn confirmed(&self) -> Value {
        let mut value=if self.images {
            json!({"version":1,"audio":self.audio,"cards":["text","image"],"image":{"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536}})
        } else {
            json!({"version":1,"audio":self.audio,"cards":["text"]})
        };
        if self.presentation {value["presentation"]=json!({"version":1,"max_segments":64,"max_duration_ms":120000,"frame_ack":true});}
        value
    }
}

fn device_turn_input(session: &Engagement, turn_id: String, text: Option<String>, wav_base64: Option<String>) -> TurnInput {
    TurnInput {
        turn_id,
        conversation_id: session.conversation_id.clone(),
        text,
        wav_base64,
        profile_id: None,
        speaker_person_id: None, utterance_id: None,
        // Old clients without output negotiation retain the original v3 path.
        // Modern devices explicitly opt in; the normal device default is silent.
        speak: Some(session.output.as_ref().map_or(true, |output|output.audio)),
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
    audio_requested: bool,
    processing_time: Duration,
    result: Result<TurnOutput, String>,
}
struct DisplayResult {
    token:RuntimeToken,
    turn_id:String,
    source_turn_id:Option<String>,
    output_lease_id:Option<String>,
    image:Option<(String,super::display::PreparedImage)>,
    delivered:Instant,
}
impl DisplayResult {
    fn ensure_source_current(&self,brain:&Brain)->Result<(),String>{
        if let Some(id)=&self.source_turn_id {if !brain.tasks.source_current(id)?{return Err("asset_source_revoked".into());}}
        if let Some(id)=&self.output_lease_id {if !brain.tasks.output_active(id)?{return Err("asset_lease_revoked".into());}}
        Ok(())
    }
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
struct Playback {
    turn_id:String, stream_id:u32, token:RuntimeToken, pcm:Vec<u8>, offset:usize, sequence:u32,
    hardware_receipts:bool, ready:bool, sent_complete:bool, hardware_started:bool,
    created:Instant, ready_at:Option<Instant>,ready_timeout:Duration,
}
impl Playback {
    fn current(&self, token:RuntimeToken, stream:u32)->bool {
        self.token==token && self.stream_id==stream
    }
    fn expired(&self)->bool {
        match self.ready_at {
            Some(ready)=>ready.elapsed()>Duration::from_millis((self.pcm.len() as u64/32)+10_000),
            None=>self.created.elapsed()>self.ready_timeout,
        }
    }
    fn transport_lost(&mut self)->Value {
        // Stop sending PCM, but retain the control connection and the exact
        // stream lease until its hardware terminal report or timeout. Closing
        // the media socket during a local stop can race that WSS report.
        self.ready=false;
        json!({"v":3,"type":"event","event":"audio.cancelled","stream_id":self.stream_id,
            "runtime":self.token,"code":"plain_audio_disconnected"})
    }
}
struct PlaybackReceipt {token:RuntimeToken,stream_id:u32,status:String}
// A failed or just-started clip may have received only a prefix. Missing
// counters from older clients stay unknown; the planned clip length is not evidence.
fn device_audio_counts(payload:&Value, sent_frames:u32, sent_bytes:usize)->Result<(Option<u64>,Option<u64>),String> {
    let count=|name:&str,max:u64| match payload.get(name) {
        None|Some(Value::Null)=>Ok(None),
        Some(n)=>n.as_u64().filter(|v|*v<=max).map(Some).ok_or_else(||"audio_playback_counts_invalid".to_owned()),
    };
    Ok((count("frames",sent_frames as u64)?,count("bytes",sent_bytes as u64)?))
}
struct ActivePeer {
    turn_id:String, request_id:String, token:RuntimeToken, task:TurnTask,
    progress_seq:u32, last_stage:Option<FeedbackStage>, last_progress:Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FeedbackStage(&'static str, Option<&'static str>);
impl FeedbackStage {
    fn payload(self, seq:u32, preview:Option<&str>, heartbeat:bool)->Value {
        let mut payload=json!({"seq":seq,"stage":self.0});
        if let Some(activity)=self.1 {payload["activity"]=json!(activity);}
        if heartbeat {payload["still_working"]=json!(true);}
        else if self.0=="understood" || self.1==Some("answer") {
            if let Some(text)=preview {payload["preview"]=json!(text.chars().take(80).collect::<String>());}
        }
        payload
    }
}
fn progress_stage(event: &Value) -> Option<FeedbackStage> {
    let (stage,activity)=match event["kind"].as_str()? {
        "turn.transcribing" => ("transcribing",None),
        "turn.understood" => ("understood",None),
        "turn.preparing_image" => ("preparing_image",None),
        "turn.synthesizing" => ("synthesizing",None),
        "turn.thinking" | "harness_permission_replied" => ("working",None),
        "harness_prompt_delivered" => ("working",Some("model")),
        "harness_first_text" => ("working",Some("composing")),
        "turn.answer_preview" => ("working",Some("answer")),
        "harness_tool_activity" => match event["phase"].as_str()? {
            "session.tool.called" | "session.shell.started" => ("working",Some("tool")),
            "session.tool.success" | "session.tool.failed" => ("working",None),
            _=>return None,
        },
        "harness_permission_required" | "harness_question_required" => ("awaiting_parent",None),
        _ => return None,
    };
    Some(FeedbackStage(stage,activity))
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
    let volume_connection=uuid::Uuid::new_v4().simple().to_string();
    let _volume_peer=VolumePeer{brain:Arc::clone(&brain),device:device_id.clone(),connection:volume_connection.clone()};
    let mut cancellations=brain.cancellations();
    let mut progress=brain.progress();
    send(&mut socket, Message::Text(json!({"v":3,"type":"welcome","device_id":device_id,"capabilities":["assistant.text","audio.pcm16","speaker.candidate","scene.cards",DISPLAY_REPLY_CAPABILITY,AUDIO_REPLY_CAPABILITY,"presentation.segments.v1"],"sample_rate":16000,"max_audio_seconds":30,"requires_activation":true}).to_string())).await.map_err(|_|"ws_write_failed")?;
    let mut engagement:Option<Engagement>=None;
    let mut audio:Option<(media::AudioInput,RuntimeToken)>=None;
    let mut plain:Option<plain_audio::PlainAudio>=None;
    let mut recent_ids:VecDeque<String>=VecDeque::new();
    let (completion_tx,mut completion_rx)=mpsc::channel::<Completed>(1);
    let mut active:Option<ActivePeer>=None;
    let mut playback:Option<Playback>=None;
    let mut playback_receipt:Option<PlaybackReceipt>=None;
    let mut display_result:Option<DisplayResult>=None;
    let mut delivery:Option<segments::Delivery>=None;
    let (task_ready_tx,mut task_ready_rx)=mpsc::channel::<segments::TaskReady>(2);
    let mut task_clock=interval(Duration::from_secs(1));
    let mut foreground_clock=interval(Duration::from_secs(1));
    let mut audio_clock=interval(Duration::from_millis(20));
    audio_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat=interval(Duration::from_secs(15)); heartbeat.tick().await;
    let mut last_packet=Instant::now();
    loop {
        let phase=if active.is_some(){"thinking"}else if playback.is_some(){"speaking"}else if delivery.as_ref().is_some_and(|d|!d.tasks.is_empty()){"waiting_task"}else if delivery.is_some(){"presenting"}else if audio.is_some(){"listening"}else{"idle"};
        brain.device_activity(&device_id,json!({"phase":phase,"conversation_id":engagement.as_ref().map(|e|&e.conversation_id),
            "turn_id":active.as_ref().map(|a|a.turn_id.as_str()).or_else(||delivery.as_ref().map(|d|d.turn_id.as_str())).or_else(||playback.as_ref().map(|p|p.turn_id.as_str())),
            "cancellable":phase!="idle","runtime":engagement.as_ref().map(|e|e.token)}));
        if let Some(state)=delivery.as_mut() {
            match state.next(&brain) {
                Ok(Some((event,clip,result)))=>{display_result=Some(result);playback=clip;playback_receipt=None;
                    if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}audio_clock.reset();},
                Ok(None)=>{},Err(error)=>{
                    let token=state.token;let activation=state.activation_id.clone();
                    brain.emit("device.presentation.failed",Some(&state.turn_id),json!({"code":error}));
                    let _=brain.tasks.revoke_outputs(Some(&activation));delivery=None;playback=None;plain=None;display_result=None;
                    if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled","runtime":token,"code":error}).to_string())).await.is_err(){break;}
                }
            }
        }
        tokio::select! {
            _=shutdown.changed()=>break,
            _=revoked.changed()=>{if !brain.credentials.authenticate(&device_id,&credential){break;}},
            event=progress.recv()=>{
                if let Ok(event)=event {
                    if let (Some(turn),Some(stage))=(active.as_mut(),progress_stage(&event)) {
                        if event["turn_id"].as_str()==Some(turn.turn_id.as_str()) &&
                            engagement.as_ref().is_some_and(|session|session.output.is_some()) {
                            if stage.1==Some("answer") {
                                let Some(id)=event.pointer("/payload/presentation_id").and_then(Value::as_str) else {continue;};
                                if !brain.memory.pending_household_cleanup()?.is_empty() || brain.tasks.plan(id,None).is_err() {continue;}
                            }
                            if turn.last_stage==Some(stage) && turn.last_progress.elapsed()<Duration::from_secs(1) {continue;}
                            turn.last_stage=Some(stage);turn.last_progress=Instant::now();
                            turn.progress_seq=turn.progress_seq.saturating_add(1);
                            let payload=stage.payload(turn.progress_seq,event.pointer("/payload/preview").and_then(Value::as_str),false);
                            let update=json!({"v":3,"type":"event","event":"turn.progress",
                                "id":turn.request_id,"runtime":turn.token,"payload":payload});
                            if send(&mut socket,Message::Text(update.to_string())).await.is_err(){break;}
                        }
                    }
                }
            },
            _=foreground_clock.tick(),if active.is_some()=>{
                let turn=active.as_mut().ok_or("active_turn_missing")?;
                if turn.last_progress.elapsed()>=Duration::from_secs(8) && engagement.as_ref().is_some_and(|session|session.output.is_some()) {
                    turn.last_progress=Instant::now();turn.progress_seq=turn.progress_seq.saturating_add(1);
                    let stage=turn.last_stage.unwrap_or(FeedbackStage("working",None));
                    let event=json!({"v":3,"type":"event","event":"turn.progress","id":turn.request_id,"runtime":turn.token,
                        "payload":stage.payload(turn.progress_seq,None,true)});
                    if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                }
            },
            _=task_clock.tick(), if delivery.as_ref().is_some_and(|d|d.current.is_none()&&d.pending.is_empty()&&!d.tasks.is_empty())=>{
                let state=delivery.as_mut().ok_or("presentation_missing")?;
                if state.preparing_task {continue;}
                let task_id=state.tasks.front().cloned().ok_or("task_missing")?;
                let task=match brain.tasks.get(&task_id){Ok(task)=>task,Err(error)=>{brain.emit("device.task.unavailable",Some(&state.turn_id),json!({"code":error}));continue;}};
                if !brain.tasks.source_current(&task.conversation_turn_id)? {delivery=None;playback=None;plain=None;continue;}
                if matches!(task.state,super::tasks::TaskState::Succeeded|super::tasks::TaskState::Partial|super::tasks::TaskState::Failed|super::tasks::TaskState::Cancelled|super::tasks::TaskState::Unknown|super::tasks::TaskState::AwaitingInput){
                    state.preparing_task=true;
                    let brain=Arc::clone(&brain);let tx=task_ready_tx.clone();let token=state.token;let activation_id=state.activation_id.clone();
                    tokio::spawn(async move{let result=segments::prepare_task(brain,task).await;let _=tx.send(segments::TaskReady{token,activation_id,task_id,progress_only:false,result}).await;});
                }else if state.last_progress.elapsed()>Duration::from_secs(8){
                    state.last_progress=Instant::now();
                    let stage=if task.state==super::tasks::TaskState::AwaitingInput{"awaiting_parent"}else if matches!(task.stage.as_str(),"rendering"|"preparing_media"|"drawing"){"preparing_image"}else{"working"};
                    state.progress_seq+=1;
                    let event=json!({"v":3,"type":"event","event":"turn.progress","id":state.request_id,"runtime":state.token,
                        "payload":{"seq":state.progress_seq,"stage":stage,"task_id":task_id,"task_stage":task.stage}});
                    if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                    if state.audio_requested && state.index<40 && state.total_sent<48 {
                        state.preparing_task=true;
                        let brain=Arc::clone(&brain);let tx=task_ready_tx.clone();let token=state.token;let activation_id=state.activation_id.clone();
                        tokio::spawn(async move{let result=segments::prepare_progress(brain,task).await;let _=tx.send(segments::TaskReady{token,activation_id,task_id,progress_only:true,result}).await;});
                    }
                }
            },
            Some(ready)=task_ready_rx.recv()=>{
                if delivery.as_ref().is_some_and(|d|d.token==ready.token&&d.activation_id==ready.activation_id&&d.tasks.front()==Some(&ready.task_id)) {
                    let mut previous=delivery.take().ok_or("presentation_missing")?;
                    if previous.total_sent>=64 || previous.index>=64 {
                        brain.tasks.revoke_outputs(Some(&previous.activation_id))?;playback=None;plain=None;display_result=None;
                        brain.emit("device.presentation.deferred",Some(&previous.turn_id),json!({"task_id":ready.task_id,"reason":"device_segment_budget","available_on_mac":true}));
                        if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled","runtime":previous.token,"code":"presentation_budget_exhausted"}).to_string())).await.is_err(){break;}
                        continue;
                    }
                    if ready.progress_only {
                        let still_working=brain.tasks.get(&ready.task_id).is_ok_and(|task|matches!(task.state,super::tasks::TaskState::Queued|super::tasks::TaskState::Running));
                        if !still_working {previous.preparing_task=false;delivery=Some(previous);continue;}
                    }else{previous.tasks.pop_front();}
                    match ready.result {
                        Ok((plan,segments))=>{
                            let remaining=64usize.saturating_sub(previous.total_sent as usize);
                            if segments.len()>remaining {
                                brain.tasks.revoke_outputs(Some(&previous.activation_id))?;playback=None;plain=None;display_result=None;
                                brain.emit("device.presentation.deferred",Some(&previous.turn_id),json!({"task_id":ready.task_id,"presentation_id":plan.presentation_id,"reason":"device_segment_budget","available_on_mac":true}));
                                if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled","runtime":previous.token,"code":"presentation_budget_exhausted"}).to_string())).await.is_err(){break;}
                                continue;
                            }
                            brain.tasks.revoke_outputs(Some(&previous.activation_id))?;
                            let mut next=segments::Delivery::new(&brain,&device_id,&previous.activation_id,previous.token,previous.turn_id.clone(),
                                plan,segments,previous.tasks.into(),previous.audio_requested,previous.index+1)?;
                            next.total_sent=previous.total_sent;next.request_id=previous.request_id;next.progress_seq=previous.progress_seq;next.progress_only=ready.progress_only;
                            delivery=Some(next);
                        },
                        Err(error)=>{
                            if ready.progress_only {previous.preparing_task=false;delivery=Some(previous);continue;}
                            brain.tasks.revoke_outputs(Some(&previous.activation_id))?;display_result=None;playback=None;
                            brain.emit("device.presentation.failed",Some(&previous.turn_id),json!({"code":error}));
                            let event=json!({"v":3,"type":"event","event":"session.cancelled","runtime":previous.token,"code":"presentation_failed"});
                            if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                            plain=None;
                        }
                    }
                }
            },
            cancellation=cancellations.recv()=>{
                let cancellation=match cancellation {
                    Ok(target)=>target,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>super::Cancellation {
                        scope:super::CancelScope::All,reason:"cancel_events_lost"},
                    Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,
                };
                let conversation=engagement.as_ref().map(|session|session.conversation_id.as_str());
                let close_session=cancellation.closes_session(conversation);
                let cancel_delivery=delivery.as_ref().is_some_and(|d|cancellation.matches_turn(&d.turn_id,conversation));
                let cancelled_token=audio.as_ref().map(|(_,token)|*token)
                    .or_else(||active.as_ref().map(|turn|turn.token))
                    .or_else(||playback.as_ref().map(|clip|clip.token))
                    .or_else(||engagement.as_ref().map(|session|session.token));
                // Send the terminal reason before a cancelled reply can arm a
                // prepared follow-up token and make that reason appear stale.
                if close_session || cancel_delivery {
                    if let Some(token)=cancelled_token {
                        if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled",
                            "runtime":token,"code":cancellation.reason}).to_string())).await.is_err(){break;}
                    }
                }
                if active.as_ref().is_some_and(|turn|cancellation.matches_turn(&turn.turn_id,conversation)) {
                    if let Some(turn)=active.take(){
                        turn.task.abort();
                        if send(&mut socket,Message::Text(cancelled_response(&turn))).await.is_err(){break;}
                    }
                }
                if playback.as_ref().is_some_and(|clip|cancellation.matches_turn(&clip.turn_id,conversation)) {
                    if let Some(clip)=playback.take(){
                        let event=json!({"v":3,"type":"event","event":"audio.cancelled","stream_id":clip.stream_id,
                            "runtime":clip.token,"code":cancellation.reason,"session_cancelled":close_session || cancel_delivery});
                        if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                    }
                }
                if cancel_delivery {
                    if let Some(d)=delivery.take(){
                        let _=brain.tasks.revoke_outputs(Some(&d.activation_id));
                        // A completed foreground turn can still own a silent
                        // image segment or wait for a durable task. There is no
                        // active reply/audio event to stop that device lease;
                        // the token-bound session event above does so explicitly.
                        playback=None;plain=None;display_result=None;
                    }
                }
                if close_session {
                    audio=None;plain=None;engagement=None;display_result=None;delivery=None;
                }
            },
            _=audio_clock.tick(), if playback.is_some()=>{
                let clip=playback.as_mut().ok_or("playback_missing")?;
                if clip.expired() {
                    let event=json!({"v":3,"type":"event","event":"audio.cancelled",
                        "stream_id":clip.stream_id,"runtime":clip.token,"code":"playback_timeout"});
                    brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                        "status":"unconfirmed","code":"playback_timeout","runtime":clip.token}));
                    playback=None;plain=None;
                    if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                    continue;
                }
                // A new client reserves its one hardware queue before any TCP
                // samples leave the Mac. WSS and PCM use independent sockets.
                if !clip.ready || clip.sent_complete {continue;}
                let end=(clip.offset+640).min(clip.pcm.len());
                let frame=media::output_frame(clip.stream_id,clip.sequence,&clip.pcm[clip.offset..end]);
                if let Some(transport)=plain.as_mut() {
                    match timeout(Duration::from_secs(2),transport.outgoing.send(plain_audio::Output::Frame(frame))).await {
                        Ok(Ok(()))=>{},_=>{
                            brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                                "status":"unconfirmed","code":"plain_audio_disconnected","runtime":clip.token}));
                            let event=clip.transport_lost();
                            if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                            continue;
                        },
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
                        if !matches!(timeout(Duration::from_secs(2),transport.outgoing.send(finish)).await,Ok(Ok(()))){
                            brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                                "status":"unconfirmed","code":"plain_audio_disconnected","runtime":clip.token}));
                            let event=clip.transport_lost();
                            if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}
                            continue;
                        }
                    }
                    let complete=json!({"v":3,"type":"event","event":"audio.complete","stream_id":clip.stream_id,"runtime":clip.token});
                    clip.sent_complete=true;
                    if !clip.hardware_receipts {playback=None;}
                    if send(&mut socket, Message::Text(complete.to_string())).await.is_err(){break;}
                }
            },
            _=heartbeat.tick()=>{
                if last_packet.elapsed()>Duration::from_secs(50){break;}
                if active.is_none() && playback.is_none() && delivery.is_none() && engagement.as_ref().is_some_and(|e|e.last_user.elapsed().as_secs()>brain.config.conversation.idle_session_seconds){
                    let token=audio.as_ref().map(|(_,token)|*token).or_else(||engagement.as_ref().map(|session|session.token));
                    engagement=None;audio=None;plain=None;display_result=None;
                    if send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.expired","runtime":token}).to_string())).await.is_err(){break;}
                }
                if audio.as_ref().is_some_and(|(a,_)|a.sequence>0 && a.started.elapsed().as_secs()>35){audio=None;plain=None;}
                if send(&mut socket, Message::Ping(Vec::new())).await.is_err(){break;}
            },
            Some(completed)=completion_rx.recv()=>{
                // A cancel clears active; stale completion is discarded.
                if active.as_ref().is_some_and(|turn|completed.turn_id==turn.turn_id) {
                    active=None;
                    match completed.result {
                        Ok(mut output)=>{
                            if engagement.as_ref().and_then(|e|e.output.as_ref()).is_some_and(|o|o.presentation) {
                                let activation=engagement.as_ref().ok_or("explicit_session_required")?.conversation_id.clone();
                                brain.emit("device.turn.completed",Some(&completed.turn_id),json!({"device_id":device_id,"transcript":output.transcript,
                                    "scene":output.scene,"presentation_id":output.presentation.presentation_id,"task_ids":output.task_ids,
                                    "timings":output.timings,"outcome":output.device_outcome,"delivery_pending":true}));
                                let next=segments::Delivery::new(&brain,&device_id,&activation,completed.token,completed.turn_id.clone(),output.presentation,
                                    output.prepared_segments,output.task_ids,completed.audio_requested,1);
                                match next {Ok(mut next)=>{next.request_id=completed.id.clone();delivery=Some(next);
                                    let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":{"presentation_pending":true}});
                                    if send(&mut socket,Message::Text(response.to_string())).await.is_err(){break;}
                                },Err(error)=>{if send(&mut socket,Message::Text(response_error(&completed.id,&error))).await.is_err(){break;}}}
                                continue;
                            }
                            let (pcm,audio_error)=if !completed.audio_requested { output.audio=None; (None,None) } else {
                                let decoded=output.audio.take().ok_or_else(||"tts_unavailable".to_string())
                                    .and_then(|audio|audio.get("wav_base64").and_then(Value::as_str)
                                        .ok_or_else(||"tts_wav_missing".to_string()).and_then(media::decode_output));
                                match decoded {Ok(pcm)=>(Some(pcm),None),Err(code)=>(None,Some(code))}
                            };
                            if let Some(code)=&audio_error {
                                brain.emit("device.audio",Some(&completed.turn_id),json!({"device_id":device_id,
                                    "status":"unavailable","code":code,"runtime":completed.token}));
                            }
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
                                payload["audio_status"]=json!({"requested":completed.audio_requested,
                                    "available":pcm.is_some(),"code":audio_error.as_deref().unwrap_or("")});
                                let image=output.display_image.take().map(|image|(format!("img-{}",uuid::Uuid::new_v4().simple()),image));
                                if let Some((id,image))=&image {
                                    payload["scene"]["cards"]=json!([{"kind":"image","title":image.title,"body":"",
                                        "asset":{"id":id,"mime":"image/jpeg","width":image.width,"height":image.height,
                                        "bytes":image.bytes.len(),"sha256":image.digest}}]);
                                }
                                display_result=Some(DisplayResult{token:completed.token,turn_id:completed.turn_id.clone(),source_turn_id:Some(output.journal.conversation_turn_id.clone()),output_lease_id:None,image,delivered:Instant::now()});
                                payload
                            } else {serde_json::to_value(output).map_err(|_|"turn_response_invalid")?};
                            let hardware_receipts=completed.compact && completed.audio_requested;
                            let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":payload,
                                "audio":pcm.as_ref().map(|data|{
                                    let mut descriptor=json!({"stream_id":stream_id,"format":"pcm_s16le","sample_rate":16000,"channels":1,"bytes":data.len(),"frame_samples":320});
                                    if hardware_receipts {descriptor["playback_ack"]=json!("hardware_v1");}
                                    descriptor
                                })});
                            if send(&mut socket, Message::Text(response.to_string())).await.is_err(){break;}
                            if let Some(pcm)=pcm {
                                playback_receipt=None;
                                let now=Instant::now();
                                playback=Some(Playback{turn_id:completed.turn_id,stream_id,token:completed.token,pcm,offset:0,sequence:0,
                                    hardware_receipts,ready:!hardware_receipts,sent_complete:false,hardware_started:false,
                                    created:now,ready_at:if hardware_receipts{None}else{Some(now)},
                                    ready_timeout:Duration::from_secs(if display_result.as_ref().is_some_and(|r|r.image.is_some()){30}else{5})});
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
                    let token=audio.as_ref().map(|(_,token)|*token);
                    audio=None;plain=None;
                    let _=send(&mut socket,Message::Text(json!({"v":3,"type":"error","code":error,"runtime":token}).to_string())).await;
                } else {
                    // Opening the next transport does not dismiss the last
                    // result. Only actual, authorized new speech does.
                    display_result=None;
                    if let Some(session)=engagement.as_mut(){session.last_user=Instant::now();}
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
                        if let Err(error)=result{
                            let token=audio.as_ref().map(|(_,token)|*token);audio=None;
                            let _=send(&mut socket, Message::Text(json!({"v":3,"type":"error","code":error,"runtime":token}).to_string())).await;
                        }
                        else {display_result=None;if let Some(session)=engagement.as_mut(){session.last_user=Instant::now();}}
                    },
                    Message::Text(text)=>{
                        let frame:RequestFrame=match serde_json::from_str(&text){Ok(frame)=>frame,Err(_)=>{let _=send(&mut socket, Message::Text(response_error("","invalid_request"))).await;continue;}};
                        if frame.v!=3 || frame.kind!="request" || !super::valid_turn_id(&frame.id){let _=send(&mut socket, Message::Text(response_error(&frame.id,"invalid_request"))).await;continue;}
                        if recent_ids.contains(&frame.id){let _=send(&mut socket, Message::Text(response_error(&frame.id,"duplicate_request"))).await;continue;}
                        recent_ids.push_back(frame.id.clone());if recent_ids.len()>128{recent_ids.pop_front();}
                        let mut pending=false;
                        let result:Result<Value,String>=async {
                            match frame.method.as_str(){
                                "device.audio.status"=>{
                                    let (result,changed)=brain.device_volumes.observe(&device_id,&volume_connection,&frame.payload)?;
                                    if changed {brain.emit("device.volume",None,json!({"device_id":device_id}));}
                                    Ok(result)
                                },
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
                                    // Older clients omit runtime and use ordered WSS. A tagged stop must
                                    // match exactly; never revoke the next question because of a late tap.
                                    if frame.payload.get("runtime").is_some() {
                                        let token=parse_runtime(&frame.payload)?;
                                        if engagement.as_ref().is_none_or(|e| e.token!=token) {return Err("stale_runtime".into());}
                                    }
                                    if let Some(mut turn)=active.take(){
                                        // Abort/join only the local producer. Dropping its guards retires
                                        // the owned OpenCode session and schedules its exact interrupt;
                                        // a slow provider must not occupy the device control socket.
                                        turn.task.stop().await;
                                        brain.emit("turn.cancelled",Some(&turn.turn_id),json!({"code":"device_stop"}));
                                        send(&mut socket,Message::Text(cancelled_response(&turn))).await?;
                                    }
                                    if let Some(session)=&engagement{brain.tasks.revoke_outputs(Some(&session.conversation_id))?;}
                                    audio=None;playback=None;plain=None;display_result=None;delivery=None;
                                    if frame.method=="session.close"{engagement=None;}
                                    Ok(json!({"cancelled":true}))
                                },
                                "audio.begin"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || audio.is_some() || playback.is_some() || delivery.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
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
                                    audio=Some((media::AudioInput::new(stream),token));
                                    session.token=token;
                                    session.last_user=Instant::now();
                                    Ok(json!({"stream_id":stream,"runtime":token,"max_bytes":media::MAX_INPUT_PCM,"plain_audio":plain_config}))
                                },
                                "audio.playback.ready"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    let stream=stream_id(&frame.payload)?;
                                    let clip=playback.as_mut().filter(|clip|clip.hardware_receipts && clip.current(token,stream))
                                        .ok_or("audio_playback_stale")?;
                                    if !clip.ready {
                                        clip.ready=true;clip.ready_at=Some(Instant::now());audio_clock.reset();
                                        brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                                            "stream_id":stream,"status":"ready","runtime":token}));
                                    }
                                    Ok(json!({"ready":true,"runtime":token,"stream_id":stream}))
                                },
                                "audio.playback.report"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    let stream=stream_id(&frame.payload)?;
                                    let status=frame.payload["status"].as_str()
                                        .filter(|status|matches!(*status,"started"|"finished"|"failed"|"cancelled"))
                                        .ok_or("audio_playback_status_invalid")?;
                                    if playback_receipt.as_ref().is_some_and(|receipt|
                                        receipt.token==token && receipt.stream_id==stream && receipt.status==status) {
                                        return Ok(json!({"recorded":true,"duplicate":true}));
                                    }
                                    let clip=playback.as_mut().filter(|clip|clip.hardware_receipts && clip.current(token,stream))
                                        .ok_or("audio_playback_stale")?;
                                    let (frames,bytes)=device_audio_counts(&frame.payload,clip.sequence,clip.offset)?;
                                    if status=="started" {
                                        if !clip.ready || clip.sequence==0 {return Err("audio_playback_not_sent".into());}
                                        if clip.hardware_started {return Ok(json!({"recorded":true,"duplicate":true}));}
                                        clip.hardware_started=true;
                                    } else if status=="finished" {
                                        if !clip.sent_complete || !clip.hardware_started ||
                                            frames!=Some(clip.sequence as u64) || bytes!=Some(clip.pcm.len() as u64) {
                                            return Err("audio_playback_incomplete".into());
                                        }
                                    }
                                    let code=frame.payload["code"].as_str().filter(|code|!code.is_empty() && code.len()<=40 &&
                                        code.bytes().all(|byte|byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte==b'_'));
                                    brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                                        "stream_id":stream,"status":status,"code":code,"runtime":token,
                                        "frames_sent":clip.sequence,"pcm_bytes":clip.pcm.len(),"frames_reported":frames,"bytes_reported":bytes}));
                                    let _=brain.memory.journal_receipt(&clip.turn_id,"audio",json!({"stream_id":stream,"status":status,"frames":frames,"bytes":bytes,"code":code}),
                                        &format!("audio-{}-{stream}-{status}",clip.turn_id));
                                    if let Some(state)=delivery.as_mut(){
                                        state.record_hardware(&brain,match status{"started"=>"audio_started","finished"=>"audio_completed","failed"=>"failed",_=>"cancelled"},
                                            json!({"stream_id":stream,"frames":frames,"bytes":bytes,"code":code,"device_monotonic_ms":frame.payload["device_monotonic_ms"],"sample_position":frame.payload["sample_position"]}))?;
                                    }
                                    if status!="started" {
                                        playback_receipt=Some(PlaybackReceipt{token,stream_id:stream,status:status.to_owned()});
                                        playback=None;if delivery.is_none(){plain=None;}
                                    }
                                    Ok(json!({"recorded":true}))
                                },
                                "presentation.segment.report"=>{
                                    if playback.is_some() && frame.payload["status"]=="completed" {return Err("presentation_audio_not_completed".into());}
                                    let state=delivery.as_mut().ok_or("presentation_not_active")?;
                                    let finished=match state.report(&brain,&frame.payload){
                                        Ok(done)=>done,
                                        Err(error) if matches!(error.as_str(),"presentation_segment_failed"|"presentation_lease_revoked")=>{
                                            let token=state.token;let activation=state.activation_id.clone();let _=brain.tasks.revoke_outputs(Some(&activation));delivery=None;playback=None;plain=None;display_result=None;
                                            send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled","runtime":token,"code":error}).to_string())).await?;
                                            return Err(error);
                                        },Err(error)=>return Err(error),
                                    };
                                    if finished {delivery=None;plain=None;}
                                    Ok(json!({"recorded":true,"finished":finished}))
                                },
                                "display.asset.read"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    let result=display_result.as_ref().ok_or("asset_expired")?;result.ensure_source_current(&brain)?;
                                    result.chunk(token,&frame.payload)
                                },
                                "display.report"=>{
                                    let token=parse_runtime(&frame.payload)?;
                                    let result=display_result.as_ref().filter(|result|result.current(token)).ok_or("display_result_expired")?;
                                    result.ensure_source_current(&brain)?;
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
                                    let _=brain.memory.journal_receipt(&result.turn_id,"display",json!({"part":part,"status":status,"asset_id":frame.payload["asset_id"],"code":code}),&format!("display-{}",uuid::Uuid::new_v4().simple()));
                                    if let Some(state)=delivery.as_mut(){
                                        state.record_display(&brain,part,status,json!({"part":part,"asset_id":frame.payload["asset_id"],"code":code,"device_monotonic_ms":frame.payload["device_monotonic_ms"]}))?;
                                    }
                                    brain.emit("device.display",Some(&result.turn_id),json!({"device_id":device_id,"part":part,"status":status,"code":code,"runtime":token}));
                                    Ok(json!({"recorded":true}))
                                },
                                "assistant.text"|"audio.commit"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || playback.is_some() || delivery.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    display_result=None;
                                    let (text,wav)=if frame.method=="assistant.text"{
                                        if audio.is_some(){return Err("audio_capture_in_progress".into());}
                                        (Some(frame.payload.get("text").and_then(Value::as_str).ok_or("text_required")?.to_owned()),None)
                                    }else{
                                        let (capture,capture_token)=audio.as_mut().ok_or("audio_session_required")?;
                                        if token!=*capture_token || capture.stream_id!=stream_id(&frame.payload)? {return Err("audio_session_mismatch".into());}
                                        if let Some(transport)=plain.as_mut() {
                                            let expected=frame.payload.get("frames").and_then(Value::as_u64)
                                                .and_then(|n|u32::try_from(n).ok()).ok_or("audio_frame_count_required")?;
                                            finish_plain_input(transport,capture,expected).await?;
                                        }
                                        let (capture,_)=audio.take().ok_or("audio_session_required")?;
                                        (None,Some(capture.wav_base64()?))
                                    };
                                    session.token=token;session.last_turn=Some(token.turn_id);session.last_user=Instant::now();
                                    let id=format!("{}-{}",session.conversation_id,token.turn_id);
                                    let input=device_turn_input(session,id.clone(),text,wav);
                                    let compact=session.output.is_some();
                                    let audio_requested=input.speak.unwrap_or(false);
                                    let turn_brain=Arc::clone(&brain);let tx=completion_tx.clone();let request_id=frame.id.clone();
                                    let completion_id=id.clone();
                                    let task=tokio::spawn(async move {
                                        let started = Instant::now();
                                        let result = turn_brain.turn(input, false).await;
                                        let _ = tx.send(Completed {
                                            id: request_id, turn_id: completion_id, token, compact, audio_requested,
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
    if let Some(turn)=active.take(){turn.task.abort();}
    if let Some(session)=&engagement{let _=brain.tasks.revoke_outputs(Some(&session.conversation_id));}
    brain.device_activity(&device_id,json!({"phase":"disconnected","cancellable":false}));
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
    fn only_transcripts_and_frozen_answers_carry_preview_text() {
        let full="字".repeat(100);
        for (kind,expected,caption) in [
            ("turn.understood",FeedbackStage("understood",None),true),
            ("harness_prompt_delivered",FeedbackStage("working",Some("model")),false),
            ("harness_first_text",FeedbackStage("working",Some("composing")),false),
            ("turn.answer_preview",FeedbackStage("working",Some("answer")),true),
        ] {
            let stage=progress_stage(&json!({"kind":kind})).unwrap();
            assert_eq!(stage,expected);
            let value=stage.payload(3,Some(&full),false);
            assert_eq!(value["preview"].as_str().map(|s|s.chars().count()),caption.then_some(80));
            let heartbeat=stage.payload(4,Some(&full),true);
            assert_eq!(heartbeat["activity"],value["activity"]);
            assert_eq!(heartbeat["still_working"],true);
            assert!(heartbeat.get("preview").is_none());
        }
        assert!(progress_stage(&json!({"kind":"harness_text_delta","delta":"raw JSON"})).is_none());
    }


    #[test]
    fn playback_receipts_distinguish_device_counts_from_transmitted_and_planned_audio() {
        assert_eq!(device_audio_counts(&json!({"frames":0,"bytes":0}),0,0).unwrap(),(Some(0),Some(0)));
        assert_eq!(device_audio_counts(&json!({"frames":1,"bytes":640}),4,2560).unwrap(),(Some(1),Some(640)));
        assert_eq!(device_audio_counts(&json!({}),4,2560).unwrap(),(None,None));
        assert!(device_audio_counts(&json!({"frames":1,"bytes":2562}),4,2560).is_err());
    }

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
        session.output=DeviceOutput::negotiate(&json!({"output":{"version":1,"audio":true,"cards":["text"]}})).unwrap();
        assert_eq!(device_turn_input(&session,"spoken".into(),Some("hello".into()),None).speak,Some(true));
        assert_eq!(session.output.as_ref().unwrap().confirmed()["audio"],true);
        assert!(DeviceOutput::negotiate(&json!({"output":{"version":1,"cards":["text"]}})).is_err());
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
            journal:crate::memory::JournalReceipt{household_id:"h".into(),daily_conversation_id:"d".into(),episode_id:"e".into(),conversation_turn_id:"ct".into(),utterance_id:"u".into(),source_revision:1,retained:false,policy_revision:1},
            presentation:crate::brain::presentation::PresentationPlan{schema_version:1,presentation_id:"p".into(),revision:1,digest:String::new(),context:crate::brain::presentation::PlanContext{household_id:"h".into(),daily_conversation_id:"d".into(),conversation_turn_id:"ct".into(),task_id:None},segments:vec![],created_at_ms:0},
            task_ids:vec![],prepared_segments:vec![],
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
            source_turn_id:None,output_lease_id:None,
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
        assert!(welcome["capabilities"].as_array().unwrap().iter().any(|v|v==AUDIO_REPLY_CAPABILITY));
        assert_eq!(brain.gateway.lock().await.as_ref().unwrap().status()["devices"].as_array().unwrap().len(),1);
        let mut sound=json!({"volume":30,"muted":false,"settings_open":false,"hardware_applied":true,"last_id":0,"last_result":0});
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"sound1","method":"device.audio.status","payload":sound}).to_string())).await.unwrap();
        let initial:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(initial["payload"]["command"].is_null());
        brain.device_volumes.set("test-bot",Some(40),None).unwrap();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"sound2","method":"device.audio.status","payload":sound}).to_string())).await.unwrap();
        let command:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let command_id=command["payload"]["command"]["id"].as_u64().unwrap();
        assert_eq!(command["payload"]["command"]["volume"],40);
        sound["volume"]=json!(40);sound["last_id"]=json!(command_id);sound["last_result"]=json!(1);sound["hardware_applied"]=json!(false);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"sound3","method":"device.audio.status","payload":sound}).to_string())).await.unwrap();
        let waiting:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(waiting["payload"]["command"]["id"],command_id);
        assert_eq!(brain.device_volumes.status()[0]["pending"],true);
        sound["hardware_applied"]=json!(true);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"sound4","method":"device.audio.status","payload":sound}).to_string())).await.unwrap();
        let applied:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(applied["payload"]["command"].is_null());
        assert_eq!(brain.device_volumes.status()[0]["result"],"applied");
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
                "output":{"version":2,"audio":true,"cards":["text"]}}}).to_string())).await.unwrap();
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
        // A re-ask owns its new token from audio.begin, including before commit.
        let new_token=json!({"epoch":8,"session_id":12,"turn_id":2,"focus_generation":0});
        let old_token=json!({"epoch":7,"session_id":12,"turn_id":1,"focus_generation":0});
        let fresh_audio=json!({"stream_id":124,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":new_token});
        for (id,method,payload,error) in [
            ("begin2","audio.begin",fresh_audio.clone(),None),
            ("late-stop","assistant.cancel",json!({"runtime":old_token}),Some("stale_runtime")),
            ("bad-commit","audio.commit",json!({"stream_id":123,"runtime":new_token}),Some("audio_session_mismatch")),
            ("duplicate-begin","audio.begin",fresh_audio,Some("session_busy")),
            ("stop2","assistant.cancel",json!({"runtime":new_token}),None),
        ] {
            socket.send(Message::Text(json!({"v":3,"type":"request","id":id,"method":method,"payload":payload}).to_string())).await.unwrap();
            let reply:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(reply["id"],id);
            assert_eq!(reply["ok"],error.is_none(),"{reply}");
            assert_eq!(reply["error"]["code"].as_str(),error);
        }
        socket.close(None).await.unwrap();brain.stop().await.unwrap();
        assert!(TcpStream::connect(&address).await.is_err());drop(brain);
    }
}
