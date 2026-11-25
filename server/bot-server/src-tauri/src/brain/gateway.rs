//! Authenticated WSS v3 service. Separate from diagnostic, plaintext LAN v1.
//! Transport framing is ours; TLS and WebSocket are the upstream libraries.
use std::{collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, net::SocketAddr, sync::{Arc, Mutex as StdMutex, RwLock}, time::{Duration, Instant}};
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
/// Bounded per-peer turn-update queue: room for every live sentence a frozen plan can
/// hold (`max_segments` ≤ 32), the wait acknowledgement and the completion.
pub(super) const LIVE_QUEUE: usize = 36;

/// How many final segments the live prefix already spoke. Exact when the final plan
/// repeats the live sentences (or regroups the same text at a segment boundary);
/// otherwise only the segments after the spoken count are delivered, never a repeat.
pub(super) fn live_prefix_skip(final_speech:&[&str], spoken:&[String]) -> (usize, bool) {
    if final_speech.len()>=spoken.len() && final_speech.iter().zip(spoken).all(|(a,b)|*a==b) { return (spoken.len(),true); }
    let squash=|s:&str|s.chars().filter(|c|!c.is_whitespace()).collect::<String>();
    let said:String=spoken.iter().map(|s|squash(s)).collect();
    let mut covered=String::new();
    for (index,part) in final_speech.iter().enumerate() {
        covered.push_str(&squash(part));
        if covered==said { return (index+1,true); }
        if !said.starts_with(&covered) { break; }
    }
    (spoken.len().min(final_speech.len()),false)
}

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

struct OnlinePeer { id:String, devices:Arc<StdMutex<BTreeMap<String, usize>>>, brain:Arc<Brain> }
impl OnlinePeer {
    fn new(id:String, devices:Arc<StdMutex<BTreeMap<String, usize>>>, brain:Arc<Brain>) -> Self {
        if let Ok(mut entries)=devices.lock() { *entries.entry(id.clone()).or_default()+=1; }
        Self {id,devices,brain}
    }
}
impl Drop for OnlinePeer {
    fn drop(&mut self) {
        let state=json!({"device_id":self.id,"phase":"disconnected","cancellable":false});
        let cleared=if let Ok(mut entries)=self.devices.lock() {
            if let Some(count)=entries.get_mut(&self.id) {
                *count-=1;
                if *count==0 {
                    entries.remove(&self.id);
                    // Serialize cache cleanup with a replacement peer's
                    // registration. It may publish activity as soon as new()
                    // releases this same membership lock.
                    if let Ok(mut states)=self.brain.device_states.lock() {
                        states.insert(self.id.clone(),state.clone());true
                    }else{false}
                }else{false}
            }else{false}
        }else{false};
        // Covers errors and task cancellation too. No external sink is called
        // while either lock is held; status remains the authoritative snapshot.
        if cleared {self.brain.emit("device.activity",None,state);}
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

impl RuntimeToken {
    fn directly_follows(self, previous: Self) -> bool {
        self.session_id == previous.session_id && previous.turn_id.checked_add(1) == Some(self.turn_id)
            && previous.epoch.checked_add(1) == Some(self.epoch)
    }
}

struct FirstSpeech {
    token: RuntimeToken,
    admitted: Instant,
    device_commit_ms: Option<u64>,
    vad_end_ms: Option<u64>,
    /// The first sound (ack or answer) of this turn was already recorded.
    sound_recorded: bool,
}

struct PendingReminder {
    brain: Arc<super::Brain>,
    device_id: String,
    offer: super::proactive::Offer,
    token: RuntimeToken,
    settled: bool,
}

impl PendingReminder {
    fn settle(&mut self, spoken:bool, reason:Option<&str>) {
        if self.settled { return; }
        self.settled=true;
        let Some(id)=self.offer.reminder_id.as_deref() else { return };
        let result=self.brain.memory.settle_reminder_offer(self.offer.offer_id,&self.device_id,id,
            spoken,reason,crate::time::now_ms() as i64);
        if let Err(error)=result { eprintln!("reminder receipt failed: {error}"); return; }
        self.brain.emit("proactive.line",None,json!({"offer_id":self.offer.offer_id,
            "moment":"reminder","trigger":self.offer.trigger,"line":self.offer.line,
            "status":if spoken {"spoken"} else {"lost"},"reason":reason}));
    }
}
impl Drop for PendingReminder {
    fn drop(&mut self) { self.settle(false,Some("interrupted")); }
}

struct Engagement {
    conversation_id:String,
    token:RuntimeToken,
    last_turn:Option<u32>,
    last_user:Instant,
    /// When the explicit wake opened the session; the device plays its own wake reply then.
    opened:Instant,
    output:Option<DeviceOutput>,
    /// One spoken retry is enough; repeated noise must not keep the mic open.
    input_failures:u8,
    /// An unprompted line's offer id: a speaker-only session that never takes input.
    proactive:Option<u32>,
}

/// A spoken acknowledgement is skipped this soon after the device's wake reply.
const ACK_AFTER_WAKE: Duration = Duration::from_secs(5);

fn slow_ack_eligible(live_voice_commit: bool, since_open: Duration, since_ack: Option<Duration>) -> bool {
    live_voice_commit && since_open >= ACK_AFTER_WAKE
        && since_ack.is_none_or(|elapsed| elapsed >= Duration::from_secs(20))
}

#[derive(Clone, Debug)]
struct DeviceOutput { images:bool, audio:bool, presentation:bool, live:bool }

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
        let live = presentation && output["live_turns"] == true;
        Ok(Some(Self {images,audio,presentation,live}))
    }

    fn confirmed(&self) -> Value {
        let mut value=if self.images {
            json!({"version":1,"audio":self.audio,"cards":["text","image"],"image":{"mime":"image/jpeg","max_width":480,"max_height":272,"max_bytes":65536}})
        } else {
            json!({"version":1,"audio":self.audio,"cards":["text"]})
        };
        if self.presentation {value["presentation"]=json!({"version":1,"max_segments":64,"max_duration_ms":120000,"frame_ack":true});}
        if self.live { value["live_turns"] = json!(true); }
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
    fn failed_input_exhausted(&mut self,error:&str)->bool {
        self.input_failures=if empty_input_error(error){self.input_failures.saturating_add(1)}else{0};
        self.input_failures>=2
    }

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

pub(super) enum TurnUpdate {
    Sentence { turn_id:String, plan:super::presentation::PresentationPlan, segment:Box<super::orchestration::PreparedSegment>, acknowledgement:bool },
    Completed(Box<Completed>),
}

pub(super) struct Completed {
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
    prefix:Vec<String>, acknowledgement_sent:bool,
    /// A waiting result was sent as this turn's first segment.
    lead_sent:bool,
    progress_seq:u32, last_stage:Option<FeedbackStage>, last_progress:Instant,
}

fn live_presentation_index(prefix_len: usize, acknowledgement_sent: bool, lead_sent: bool) -> u32 {
    prefix_len as u32 + u32::from(acknowledgement_sent) + 1 + u32::from(lead_sent)
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
    let _online_peer=OnlinePeer::new(device_id.clone(),online,Arc::clone(&brain));
    let volume_connection=uuid::Uuid::new_v4().simple().to_string();
    let _volume_peer=VolumePeer{brain:Arc::clone(&brain),device:device_id.clone(),connection:volume_connection.clone()};
    let mut cancellations=brain.cancellations();
    let mut progress=brain.progress();
    send(&mut socket, Message::Text(json!({"v":3,"type":"welcome","device_id":device_id,"capabilities":["assistant.text","audio.pcm16","speaker.candidate","scene.cards",DISPLAY_REPLY_CAPABILITY,AUDIO_REPLY_CAPABILITY,"presentation.segments.v1","conversation.live.v1","proactive.v1"],"sample_rate":16000,"max_audio_seconds":30,"requires_activation":true}).to_string())).await.map_err(|_|"ws_write_failed")?;
    let mut engagement:Option<Engagement>=None;
    let mut supplement:Option<(RuntimeToken,super::Supplement)>=None;
    let mut first_speech:Option<FirstSpeech>=None;
    let mut last_ack:Option<Instant>=None;
    let mut context:Option<super::LiveTurnContext>=None;
    let mut audio:Option<(media::AudioInput,RuntimeToken)>=None;
    let mut plain:Option<plain_audio::PlainAudio>=None;
    let mut recent_ids:VecDeque<String>=VecDeque::new();
    let (completion_tx,mut completion_rx)=mpsc::channel::<TurnUpdate>(LIVE_QUEUE);
    let mut active:Option<ActivePeer>=None;
    let mut playback:Option<Playback>=None;
    let mut playback_receipt:Option<PlaybackReceipt>=None;
    let mut display_result:Option<DisplayResult>=None;
    let mut delivery:Option<segments::Delivery>=None;
    let (task_ready_tx,mut task_ready_rx)=mpsc::channel::<segments::TaskReady>(4);
    let mut fence_gen: u64 = 1;
    let mut pending_commit: Option<PendingCommit> = None;
    let mut pending_preps: Vec<InFlightPrep> = Vec::new();
    let mut pending_lead: Option<PreparedLead> = None;
    let mut inflight_lead: Option<PreparedLead> = None;
    let mut held: HashMap<String, String> = HashMap::new();
    let mut lead_preparing: Option<String> = None;
    let mut spoken_leads: BTreeSet<String> = BTreeSet::new();
    let (prep_tx, mut prep_rx) = mpsc::channel::<BackgroundPrep>(4);
    let (cleanup_tx, mut cleanup_rx) = mpsc::channel::<TurnTask>(4);
    let cleanup_worker = tokio::spawn(async move {
        while let Some(mut task) = cleanup_rx.recv().await { task.stop().await; }
    });
    let mut task_clock=interval(Duration::from_secs(1));
    let mut foreground_clock=interval(Duration::from_secs(1));
    let mut audio_clock=interval(Duration::from_millis(20));
    audio_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat=interval(Duration::from_secs(15)); heartbeat.tick().await;
    let mut closing_clock=interval(Duration::from_millis(500));
    // Unprompted lines (§12.2): offered only to firmware that said proactive.ready,
    // one outstanding offer at a time, and only while the device is idle.
    let mut proactive_ready=false;
    let mut offered:Option<(super::proactive::Offer,Instant)>=None;
    let mut pending_reminder:Option<PendingReminder>=None;
    let mut proactive_clock=interval(Duration::from_secs(20));
    let mut last_packet=Instant::now();
    loop {
        if let Some((capture,_))=audio.as_mut(){capture.start_candidate(&brain.models);}
        let quick=brain.config.responsive.quick_decouple;
        let phase=if playback.is_some(){"speaking"}else if active.is_some(){"thinking"}else if !quick && delivery.as_ref().is_some_and(|d|!d.tasks.is_empty()){"waiting_task"}else if delivery.is_some(){"presenting"}else if audio.is_some(){"listening"}else{"idle"};
        let hint=brain.tasks.hint_pending();
        brain.device_activity(&device_id,json!({"phase":phase,"conversation_id":engagement.as_ref().map(|e|&e.conversation_id),
            "turn_id":active.as_ref().map(|a|a.turn_id.as_str()).or_else(||delivery.as_ref().map(|d|d.turn_id.as_str())).or_else(||playback.as_ref().map(|p|p.turn_id.as_str())),
            "cancellable":phase!="idle","runtime":engagement.as_ref().map(|e|e.token),
            "result_hint":hint,"result_expression":if hint {super::task_delivery::HINT_EXPRESSION} else {""}}));
        // A result waits until this turn's words are known. A missing-input question
        // can lead a live turn: the device is already COMMITTED.
        if quick && delivery.is_none() && pending_lead.as_ref().is_some_and(|lead| !lead.kind_result)
            && active.as_ref().is_some_and(|turn| !turn.lead_sent)
            && engagement.as_ref().is_some_and(|session| session.output.as_ref().is_some_and(|output| output.live)) {
            if let Some(lead)=pending_lead.take() {
                let token=active.as_ref().map(|turn| turn.token);
                let turn_id=active.as_ref().map(|turn| turn.turn_id.clone());
                let request_id=active.as_ref().map(|turn| turn.request_id.clone());
                let activation=engagement.as_ref().map(|session| session.conversation_id.clone()).unwrap_or_default();
                if let (Some(token), Some(turn_id), Some(request_id))=(token, turn_id, request_id) {
                    match segments::Delivery::new(&brain,&device_id,&activation,token,turn_id,lead.plan.clone(),lead.segments.clone(),Vec::new(),true,1) {
                        Ok(mut next)=>{
                            next.live_content=true;
                            next.request_id=request_id;
                            claim_sent_lead(&brain, &mut held, &lead, &next.lease_id, &next.playback_id);
                            if let Some(turn)=active.as_mut() { turn.lead_sent=true; }
                            inflight_lead=Some(lead);
                            delivery=Some(next);
                        },
                        Err(_)=>{ pending_lead=Some(lead); },
                    }
                } else { pending_lead=Some(lead); }
            }
        }
        if delivery.as_ref().is_some_and(|state| state.report_expired()) {
            let state=delivery.take().ok_or("presentation_missing")?;
            release_unheard_lead(&brain, &mut held, &mut inflight_lead);
            let _=brain.tasks.revoke_outputs(Some(&state.activation_id));
            playback=None;plain=None;display_result=None;
            brain.emit("device.presentation.unconfirmed",Some(&state.turn_id),json!({"code":"playback_report_timeout"}));
        }
        if let Some(state)=delivery.as_mut() {
            match state.next(&brain) {
                Ok(Some((event,clip,result)))=>{display_result=Some(result);playback=clip;playback_receipt=None;
                    if send(&mut socket,Message::Text(event.to_string())).await.is_err(){break;}audio_clock.reset();},
                Ok(None)=>{},Err(error)=>{
                    let token=state.token;let activation=state.activation_id.clone();
                    if active.as_ref().is_some_and(|a|a.turn_id==state.turn_id) {active.take();}
                    brain.emit("device.presentation.failed",Some(&state.turn_id),json!({"code":error}));
                    if pending_reminder.as_ref().is_some_and(|r|r.token==token) {pending_reminder=None;}
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
            _=task_clock.tick(), if (!brain.config.responsive.quick_decouple && delivery.as_ref().is_some_and(|d|d.current.is_none()&&d.pending.is_empty()&&!d.tasks.is_empty()))
                || (brain.config.responsive.quick_decouple && active.is_none() && audio.is_none() && playback.is_none() && delivery.is_none() && engagement.is_some())=>{
                if brain.config.responsive.quick_decouple {
                    // Prepare only. The S31 accepts a segment in COMMITTED, which it
                    // enters on the next device turn, so nothing is sent while idle.
                    if pending_lead.is_none() && lead_preparing.is_none() && inflight_lead.is_none()
                        && engagement.as_ref().is_some_and(|session| session.proactive.is_none() && session.output.as_ref().is_some_and(|output| output.presentation)) {
                        if let Some(session)=engagement.as_ref() {
                            let token=session.token;let activation_id=session.conversation_id.clone();
                            let brain_for_query=Arc::clone(&brain);let activation_for_query=activation_id.clone();
                            let ready=tokio::task::spawn_blocking(move || {
                                let household=brain_for_query.memory.household_settings().ok().and_then(|settings| settings["household_id"].as_str().map(str::to_owned))?;
                                let speaker=brain_for_query.named_person(&activation_for_query);
                                brain_for_query.tasks.next_for_delivery(&household,&activation_for_query,speaker.as_deref()).ok().flatten()
                            }).await.ok().flatten();
                            if let Some(ready)=ready {
                                let key=ready.delivery_id.clone();
                                if !spoken_leads.contains(&key) {
                                    let task_id=ready.task.task_id.clone();
                                    let delivery_id=ready.delivery_id.clone();
                                    let delivery_revision=ready.delivery_revision;
                                    let kind_result=ready.kind=="result";
                                    let promise_full=ready.promise_full;
                                    lead_preparing=Some(task_id.clone());
                                    let tx=task_ready_tx.clone();let brain2=Arc::clone(&brain);
                                    let task=ready.task;
                                    tokio::spawn(async move{let result=segments::prepare_task(brain2,task).await;let _=tx.send(segments::TaskReady{token,activation_id,task_id,progress_only:false,delivery_id,delivery_revision,kind_result,promise_full,result}).await;});
                                }
                            }
                        }
                    }
                    continue;
                }
                let state=delivery.as_mut().ok_or("presentation_missing")?;
                if state.preparing_task {continue;}
                let task_id=state.tasks.front().cloned().ok_or("task_missing")?;
                let task=match brain.tasks.get(&task_id){Ok(task)=>task,Err(error)=>{brain.emit("device.task.unavailable",Some(&state.turn_id),json!({"code":error}));continue;}};
                if !brain.tasks.source_current(&task.conversation_turn_id)? {delivery=None;playback=None;plain=None;continue;}
                if matches!(task.state,super::tasks::TaskState::Succeeded|super::tasks::TaskState::Partial|super::tasks::TaskState::Failed|super::tasks::TaskState::Cancelled|super::tasks::TaskState::Unknown|super::tasks::TaskState::AwaitingInput){
                    state.preparing_task=true;
                    let brain=Arc::clone(&brain);let tx=task_ready_tx.clone();let token=state.token;let activation_id=state.activation_id.clone();
                    tokio::spawn(async move{let result=segments::prepare_task(brain,task).await;let _=tx.send(segments::TaskReady{token,activation_id,task_id,progress_only:false,delivery_id:String::new(),delivery_revision:0,kind_result:false,promise_full:true,result}).await;});
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
                        tokio::spawn(async move{let result=segments::prepare_progress(brain,task).await;let _=tx.send(segments::TaskReady{token,activation_id,task_id,progress_only:true,delivery_id:String::new(),delivery_revision:0,kind_result:false,promise_full:true,result}).await;});
                    }
                }
            },
            Some(ready)=task_ready_rx.recv()=>{
                if lead_preparing.as_deref()==Some(ready.task_id.as_str()) && delivery.as_ref().is_none_or(|state| state.tasks.front()!=Some(&ready.task_id)) {
                    lead_preparing=None;
                    let session_ok=engagement.as_ref().is_some_and(|session| session.conversation_id==ready.activation_id && session.proactive.is_none());
                    if session_ok && pending_lead.is_none() {
                        if let Ok((plan,segments))=ready.result {
                            let speech=plan.segments.iter().map(|segment|segment.speech.as_str()).filter(|text|!text.is_empty()).collect::<Vec<_>>().join(" ");
                            let task=brain.tasks.get(&ready.task_id).ok();
                            let awaiting=task.as_ref().is_some_and(|task| task.state==super::tasks::TaskState::AwaitingInput);
                            let key=if ready.delivery_id.is_empty(){task.as_ref().map(|task| format!("{}:{}",task.task_id,task.state.as_str())).unwrap_or_else(|| ready.task_id.clone())}else{ready.delivery_id.clone()};
                            if !speech.is_empty() && !spoken_leads.contains(&key) {
                                let segment_ids=segments.iter().map(|segment|segment.segment_id.clone()).collect();
                                pending_lead=Some(PreparedLead{task_id:ready.task_id,key,plan,segments,speech,awaiting,segment_ids,delivery_id:ready.delivery_id,delivery_revision:ready.delivery_revision,kind_result:ready.kind_result,promise_full:ready.promise_full,expansion:false});
                            }
                        }
                    }
                    continue;
                }
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
            Some(event)=prep_rx.recv()=>{
                if failure_releases_current(&event, fence_gen, active.as_ref(), engagement.as_ref()) {
                    release_unheard_lead(&brain, &mut held, &mut inflight_lead);
                    if let Some(offer)=pending_reminder.as_ref().map(|reminder| reminder.offer.clone()) {
                        release_offer_row(&brain, &mut held, &offer);
                    }
                }
                let (turn_id, fence) = match &event {
                    BackgroundPrep::Failure { turn_id, fence, .. } | BackgroundPrep::Proactive { turn_id, fence, .. } => (turn_id, fence),
                };
                pending_preps.retain(|prep| !(prep.fence == *fence && prep.turn_id == *turn_id));
                if !apply_background_prep(&brain,&mut socket,&device_id,fence_gen,&active,&mut engagement,&mut delivery,&mut playback,&mut plain,&mut display_result,&mut pending_reminder,&mut held,event).await {break;}
            },
            _=async {
                let wait=delivery.as_ref().and_then(|state| state.report_since.map(|sent| state.report_budget.saturating_sub(sent.elapsed())))
                    .unwrap_or(Duration::from_secs(3600));
                tokio::time::sleep(wait).await;
            }, if delivery.as_ref().is_some_and(|state| state.current.is_some())=>{
                if delivery.as_ref().is_some_and(|state| state.report_expired()) {
                    if let Some(state)=delivery.take() {
                        release_unheard_lead(&brain, &mut held, &mut inflight_lead);
                        let _=brain.tasks.revoke_outputs(Some(&state.activation_id));
                        playback=None;plain=None;display_result=None;
                        brain.emit("device.presentation.unconfirmed",Some(&state.turn_id),json!({"code":"playback_report_timeout"}));
                    }
                }
            },
            _=async {
                match pending_commit.as_ref() {
                    Some(commit)=>tokio::time::sleep_until(commit.deadline).await,
                    None=>std::future::pending().await,
                }
            }, if pending_commit.is_some()=>{
                if let Some(commit)=pending_commit.take() {
                    // The capture that missed its tail must not keep the next listen busy.
                    audio=None;plain=None;
                    let code=if commit.fence==fence_gen {"plain_audio_drain_timeout"} else {"cancelled"};
                    if send(&mut socket,Message::Text(response_error(&commit.request_id,code))).await.is_err(){break;}
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
                if brain.config.responsive.quick_decouple {
                    let commit_turn=pending_commit.as_ref().and_then(|commit| conversation.map(|id| format!("{id}-{}", commit.token.turn_id)));
                    let matches_active=active.as_ref().is_some_and(|turn| cancellation.matches_turn(&turn.turn_id, conversation));
                    if matches_active || retire_deferred(&pending_preps, commit_turn.as_deref(), &cancellation, conversation) {
                        fence_gen = fence_gen.saturating_add(1);
                        if answer_pending_commit(&mut socket, &mut pending_commit, "cancelled").await.is_err(){break;}
                        pending_preps.clear();
                    }
                }
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
                if supplement.as_ref().is_some_and(|(_,s)|cancellation.matches_turn(&s.previous,conversation)) {
                    supplement=None;
                }
                if close_session || cancel_delivery { pending_reminder=None; }
                if close_session {
                    context=None;first_speech=None;supplement=None;audio=None;plain=None;engagement=None;display_result=None;delivery=None;
                }
            },
            _=audio_clock.tick(), if playback.is_some()=>{
                let clip=playback.as_mut().ok_or("playback_missing")?;
                if clip.expired() {
                    let event=json!({"v":3,"type":"event","event":"audio.cancelled",
                        "stream_id":clip.stream_id,"runtime":clip.token,"code":"playback_timeout"});
                    brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                        "status":"unconfirmed","code":"playback_timeout","runtime":clip.token}));
                    pending_reminder=None;playback=None;plain=None;
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
            // After a goodbye's sign-off has played, return to idle instead of waiting
            // out the follow-up window.
            _=closing_clock.tick(), if active.is_none() && playback.is_none() && delivery.is_none() && engagement.is_some()=>{
                if engagement.as_ref().is_some_and(|e|brain.closing.take_due(&e.conversation_id)) {
                    let token=audio.as_ref().map(|(_,token)|*token).or_else(||engagement.as_ref().map(|session|session.token));
                    supplement=None;engagement=None;audio=None;plain=None;display_result=None;
                    brain.emit("device.session_closed",None,json!({"device_id":device_id,"reason":"goodbye"}));
                    if send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.expired","runtime":token,"code":"goodbye"}).to_string())).await.is_err(){break;}
                }
            },
            _=proactive_clock.tick(), if proactive_ready && engagement.is_none() && active.is_none() && playback.is_none() && delivery.is_none()=>{
                if offered.as_ref().is_some_and(|(offer,at)|at.elapsed()<Duration::from_millis(offer.ttl_ms as u64+30_000)) {continue;}
                // The device never reported this one (e.g. it rebooted): close it in the journal.
                if let Some((offer,_))=offered.take() {brain.proactive_journal(&device_id,&offer,"lost",None);}
                match brain.proactive_next(&device_id) {
                    Ok(Some(offer))=>{
                        if !super::task_delivery::current_firmware_accepts_moment(offer.moment) {
                            brain.emit("proactive.moment", None, json!({"moment": offer.moment, "installed_firmware": "drops"}));
                            if let Some(id)=offer.delivery_id.as_deref() { let _=brain.tasks.note_offer_backoff(id); }
                        }
                        let event=json!({"v":3,"type":"event","event":"proactive.offer","payload":{"offer_id":offer.offer_id,
                            "text":offer.line,"ttl_ms":offer.ttl_ms,"show_if_absent":offer.show_if_absent,"moment":offer.moment}});
                        offered=Some((offer,Instant::now()));
                        if send(&mut socket, Message::Text(event.to_string())).await.is_err(){break;}
                    },
                    Ok(None)=>{},
                    Err(error)=>brain.emit("proactive.failed",None,json!({"device_id":device_id,"code":error})),
                }
            },
            _=heartbeat.tick()=>{
                if last_packet.elapsed()>Duration::from_secs(50){break;}
                if active.is_none() && playback.is_none() && delivery.is_none() && engagement.as_ref().is_some_and(|e|e.last_user.elapsed().as_secs()>brain.config.conversation.idle_session_seconds){
                    let token=audio.as_ref().map(|(_,token)|*token).or_else(||engagement.as_ref().map(|session|session.token));
                    supplement=None;engagement=None;audio=None;plain=None;display_result=None;
                    if send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.expired","runtime":token}).to_string())).await.is_err(){break;}
                }
                if audio.as_ref().is_some_and(|(a,_)|a.sequence>0 && a.started.elapsed().as_secs()>35){audio=None;plain=None;}
                if send(&mut socket, Message::Ping(Vec::new())).await.is_err(){break;}
            },
            Some(update)=completion_rx.recv(), if !delivery.as_ref().is_some_and(|d|
                d.live_content && (d.current.is_some() || !d.pending.is_empty()))=>{
                let completed = match update {
                    TurnUpdate::Sentence { turn_id, plan, segment, acknowledgement } => {
                        let Some(turn) = active.as_mut().filter(|a| a.turn_id == turn_id) else { continue };
                        let Some(session) = engagement.as_ref() else { continue };
                        // The slow "听到了" must not play over a result that is this turn's first segment.
                        if acknowledgement && (turn.lead_sent || pending_lead.is_some()) { continue; }
                        let index = live_presentation_index(turn.prefix.len(), turn.acknowledgement_sent, turn.lead_sent);
                        if index > 1 { brain.tasks.revoke_outputs(Some(&session.conversation_id))?; }
                        let speech = segment.scene.speech.clone();
                        let mut next = match segments::Delivery::new(&brain, &device_id, &session.conversation_id,
                            turn.token, turn_id, plan, vec![*segment], vec![], true, index) {
                            Ok(next) => next,
                            Err(error) => {
                                let token=turn.token;
                                brain.emit("device.presentation.failed",Some(&turn.turn_id),json!({"code":error}));
                                active.take();delivery=None;playback=None;plain=None;display_result=None;
                                send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled",
                                    "runtime":token,"code":error}).to_string())).await?;
                                continue;
                            }
                        };
                        if acknowledgement {
                            turn.acknowledgement_sent=true;
                            last_ack=Some(Instant::now());
                            next.progress_only=true;
                        } else {
                            turn.prefix.push(speech);
                        }
                        next.live_content = true;
                        next.total_sent = index - 1;
                        next.request_id = turn.request_id.clone();
                        delivery = Some(next);
                        continue;
                    }
                    TurnUpdate::Completed(done) => *done,
                };
                // A cancel clears active; stale completion is discarded.
                if active.as_ref().is_some_and(|turn|completed.turn_id==turn.turn_id) {
                    let turn = active.take().expect("matching active turn");
                    match completed.result {
                        Ok(mut output)=>{
                            if let Some(session)=engagement.as_mut(){session.input_failures=0;}
                            if engagement.as_ref().and_then(|e|e.output.as_ref()).is_some_and(|o|o.presentation) {
                                let activation=engagement.as_ref().ok_or("explicit_session_required")?.conversation_id.clone();
                                brain.emit("device.turn.completed",Some(&completed.turn_id),json!({"device_id":device_id,"transcript":output.transcript,
                                    "scene":output.scene,"presentation_id":output.presentation.presentation_id,"task_ids":output.task_ids,
                                    "timings":output.timings,"outcome":output.device_outcome,"delivery_pending":true}));
                                let index = live_presentation_index(turn.prefix.len(), turn.acknowledgement_sent, turn.lead_sent);
                                if turn.acknowledgement_sent && turn.prefix.is_empty() {
                                    brain.tasks.revoke_outputs(Some(&activation))?;
                                    delivery=None;
                                }
                                if !turn.prefix.is_empty() {
                                    let state = delivery.as_ref().ok_or("live_presentation_missing")?;
                                    let final_speech=output.prepared_segments.iter().map(|s|s.scene.speech.as_str()).collect::<Vec<_>>();
                                    let (skip,exact)=live_prefix_skip(&final_speech,&turn.prefix);
                                    if !exact {
                                        // Keep talking: deliver only what has not been said, and leave a trace.
                                        brain.emit("device.presentation.prefix_diverged",Some(&completed.turn_id),json!({
                                            "spoken_segments":turn.prefix.len(),"final_segments":final_speech.len(),"skipped":skip}));
                                    }
                                    if output.provider_id != super::fast_core::PROVIDER_ID
                                        || !brain.tasks.output_active(&state.lease_id)?
                                        || !brain.tasks.source_current(&state.plan.context.conversation_turn_id)? {
                                        brain.tasks.revoke_outputs(Some(&activation))?;
                                        send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.cancelled",
                                            "runtime":completed.token,"code":"live_presentation_changed"}).to_string())).await?;
                                        delivery=None; playback=None; plain=None; display_result=None;
                                        continue;
                                    }
                                    output.prepared_segments.drain(..skip);
                                    if output.prepared_segments.is_empty() {
                                        // Its last immutable sentence already has its actual hardware/display receipt.
                                        let end = json!({"v":3,"type":"event","event":"presentation.end","runtime":completed.token,
                                            "payload":{"presentation_id":state.plan.presentation_id,"output_lease_id":state.lease_id,
                                                "presentation_index":state.index,"seq":state.current_seq}});
                                        let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,
                                            "runtime":completed.token,"payload":{"presentation_pending":true}});
                                        send(&mut socket, Message::Text(response.to_string())).await?;
                                        send(&mut socket, Message::Text(end.to_string())).await?;
                                        delivery=None; plain=None; context=None;
                                        continue;
                                    }
                                    brain.tasks.revoke_outputs(Some(&activation))?;
                                }
                                let delegated=if brain.config.responsive.quick_decouple { output.task_ids.clone() } else { Vec::new() };
                                let attached_tasks=if brain.config.responsive.quick_decouple { Vec::new() } else { output.task_ids };
                                let mut carried=None;
                                if brain.config.responsive.quick_decouple {
                                    apply_turn_delivery(&brain, &activation, &output.transcript, &mut pending_lead).await;
                                    if turn.prefix.is_empty() {
                                        if let Some(lead)=pending_lead.take() {
                                            match segments::prepend_lead(&brain,lead.plan.clone(),lead.segments.clone(),output.presentation.clone(),output.prepared_segments.clone()) {
                                                Ok((plan,segments))=>{output.presentation=plan;output.prepared_segments=segments;carried=Some(lead);},
                                                Err(_)=>{pending_lead=Some(lead);},
                                            }
                                        }
                                    }
                                }
                                let next=segments::Delivery::new(&brain,&device_id,&activation,completed.token,completed.turn_id.clone(),output.presentation,
                                    output.prepared_segments,attached_tasks,completed.audio_requested,index);
                                match next {Ok(mut next)=>{
                                    if let Some(lead)=carried.as_ref() { claim_sent_lead(&brain, &mut held, lead, &next.lease_id, &next.playback_id); }
                                    if let Some(task_id)=delegated.first() {
                                        next.foreground_task=Some(task_id.clone());
                                        let _=brain.tasks.note_foreground(&completed.turn_id, u64::from(completed.token.turn_id), true, None, Some(task_id));
                                    }
                                    inflight_lead=carried;next.total_sent=index-1;next.request_id=completed.id.clone();delivery=Some(next);
                                    let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":{"presentation_pending":true}});
                                    if send(&mut socket,Message::Text(response.to_string())).await.is_err(){break;}
                                },Err(error)=>{if pending_lead.is_none(){pending_lead=carried;}if send(&mut socket,Message::Text(response_error(&completed.id,&error))).await.is_err(){break;}}}
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
                            if engagement.as_mut().is_some_and(|session|session.failed_input_exhausted(&error)) {
                                brain.emit("device.turn.failed",Some(&completed.turn_id),json!({"device_id":device_id,
                                    "code":error.split(':').next().unwrap_or("turn_failed"),"spoken":false,"retry_exhausted":true}));
                                if let Some(session)=&engagement {
                                    brain.tasks.revoke_outputs(Some(&session.conversation_id))?;
                                    brain.closing.clear(&session.conversation_id);
                                }
                                // A final session event retires the pending commit on the device.
                                // A normal error response would arm yet another automatic follow-up.
                                send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.expired",
                                    "runtime":completed.token,"code":"asr_retry_exhausted"}).to_string())).await?;
                                pending_reminder=None;delivery=None;playback=None;plain=None;display_result=None;
                                context=None;first_speech=None;supplement=None;audio=None;engagement=None;
                                continue;
                            }
                            // Nothing of the answer was said yet: say an in-character line instead
                            // of a silent technical caption. The code stays in the journal.
                            // A lead already sent counts as spoken, so a failure uses the same
                            // live_answer_interrupted cancel as an acknowledgement.
                            let presentation=engagement.as_ref().and_then(|e|e.output.as_ref()).is_some_and(|o|o.presentation);
                            let wants_failure_speech=completed.audio_requested && presentation && turn.prefix.is_empty();
                            let heard=remembered_transcript(&brain,&completed.turn_id);
                            let activation_id=engagement.as_ref().map(|session| session.conversation_id.clone()).unwrap_or_default();
                            apply_turn_delivery(&brain, &activation_id, &heard, &mut pending_lead).await;
                            if wants_failure_speech && brain.config.responsive.quick_decouple && turn.prefix.is_empty() && !turn.acknowledgement_sent {
                                if let Some(lead)=pending_lead.take() {
                                    let activation_id=engagement.as_ref().map(|session|session.conversation_id.clone()).unwrap_or_default();
                                    match segments::Delivery::new(&brain,&device_id,&activation_id,completed.token,completed.turn_id.clone(),lead.plan.clone(),lead.segments.clone(),Vec::new(),true,1) {
                                        Ok(mut next)=>{
                                            next.request_id=completed.id.clone();
                                            claim_sent_lead(&brain, &mut held, &lead, &next.lease_id, &next.playback_id);
                                            inflight_lead=Some(lead);
                                            delivery=Some(next);
                                            let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":{"presentation_pending":true}});
                                            if send(&mut socket,Message::Text(response.to_string())).await.is_err(){break;}
                                            continue;
                                        },
                                        Err(_)=>{pending_lead=Some(lead);},
                                    }
                                }
                            }
                            if wants_failure_speech && brain.config.responsive.quick_decouple {
                                // Revoke and clear only once the line comes back, and only if an
                                // acknowledgement, a partial answer, or the live lead was already
                                // sent. A failed preparation after that sends live_answer_interrupted.
                                let spoke=turn.acknowledgement_sent || !turn.prefix.is_empty() || turn.lead_sent;
                                let activation_id=engagement.as_ref().map(|session|session.conversation_id.clone()).unwrap_or_default();
                                let tx=prep_tx.clone();let brain2=Arc::clone(&brain);let turn_id=completed.turn_id.clone();
                                let token=completed.token;let request_id=completed.id.clone();let error_text=error.clone();
                                let index=live_presentation_index(0, turn.acknowledgement_sent, turn.lead_sent);let fence=fence_gen;
                                pending_preps.push(InFlightPrep{fence,turn_id:turn_id.clone()});
                                tokio::spawn(async move {
                                    let result=segments::prepare_failure(&brain2,&turn_id,&error_text).await;
                                    let _=tx.send(BackgroundPrep::Failure{fence,token,turn_id,request_id,index,activation_id,error:error_text,spoke,result}).await;
                                });
                                continue;
                            }
                            let spoken=if wants_failure_speech {
                                segments::prepare_failure(&brain,&completed.turn_id,&error).await.unwrap_or_else(|code|{
                                    brain.emit("device.failure_speech.unavailable",Some(&completed.turn_id),json!({"code":code}));None})
                            } else {None};
                            if turn.acknowledgement_sent || !turn.prefix.is_empty() || turn.lead_sent {
                                if let Some(session)=&engagement {brain.tasks.revoke_outputs(Some(&session.conversation_id))?;}
                                if spoken.is_none() {
                                    send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled",
                                        "runtime":completed.token,"code":"live_answer_interrupted"}).to_string())).await?;
                                }
                                delivery=None;playback=None;display_result=None;
                                // The ACK's actual receipt has already drained its segment. A
                                // replacement failure line uses the same negotiated PCM socket;
                                // dropping it here would send unnegotiated WSS binary audio.
                                if spoken.is_none(){plain=None;}
                            }
                            brain.emit("device.turn.failed",Some(&completed.turn_id),json!({"device_id":device_id,
                                "code":error.split(':').next().unwrap_or("turn_failed"),"spoken":spoken.is_some()}));
                            if let (Some((plan,prepared)),Some(session))=(spoken,engagement.as_ref()) {
                                let index=u32::from(turn.acknowledgement_sent)+1;
                                if let Ok(mut next)=segments::Delivery::new(&brain,&device_id,&session.conversation_id,completed.token,
                                    completed.turn_id.clone(),plan,prepared,vec![],true,index) {
                                    next.total_sent=index-1;next.request_id=completed.id.clone();delivery=Some(next);
                                    let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":{"presentation_pending":true}});
                                    if send(&mut socket,Message::Text(response.to_string())).await.is_err(){break;}
                                    continue;
                                }
                            }
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
                    if let Err(error)=seal_pending_audio(&brain,&mut socket,&device_id,&mut engagement,&mut audio,&mut supplement,&mut context,&mut first_speech,&mut active,&mut display_result,last_ack,&completion_tx,&mut pending_commit,&mut fence_gen,&mut pending_preps).await {
                        let token=audio.as_ref().map(|(_,token)|*token);
                        audio=None;plain=None;
                        if answer_pending_commit(&mut socket,&mut pending_commit,"cancelled").await.is_err(){break;}
                        let _=send(&mut socket,Message::Text(json!({"v":3,"type":"error","code":error,"runtime":token}).to_string())).await;
                    }
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
                        else {
                            display_result=None;
                            if let Some(session)=engagement.as_mut(){session.last_user=Instant::now();}
                            if let Err(error)=seal_pending_audio(&brain,&mut socket,&device_id,&mut engagement,&mut audio,&mut supplement,&mut context,&mut first_speech,&mut active,&mut display_result,last_ack,&completion_tx,&mut pending_commit,&mut fence_gen,&mut pending_preps).await {
                                let token=audio.as_ref().map(|(_,token)|*token);
                                audio=None;
                                if answer_pending_commit(&mut socket,&mut pending_commit,"cancelled").await.is_err(){break;}
                                let _=send(&mut socket,Message::Text(json!({"v":3,"type":"error","code":error,"runtime":token}).to_string())).await;
                            }
                        }
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
                                "proactive.ready"=>{proactive_ready=true;Ok(json!({"accepted":true}))},
                                "proactive.report"=>{
                                    let id=frame.payload.get("offer_id").and_then(Value::as_u64);
                                    let status=match frame.payload.get("status").and_then(Value::as_str) {Some("shown")=>"shown",Some("expired")=>"expired",_=>return Err("invalid_proactive_report".into())};
                                    if let Some((offer,_))=offered.take_if(|(offer,_)|Some(offer.offer_id as u64)==id) {brain.proactive_journal(&device_id,&offer,status,None);}
                                    Ok(json!({"recorded":true}))
                                },
                                "session.open"=>{
                                    if engagement.is_some() || active.is_some() || playback.is_some(){return Err("session_already_open".into());}
                                    let activation=frame.payload.get("activation").and_then(Value::as_str).unwrap_or("");
                                    // The device chose the moment for an outstanding offer: a speaker-only session.
                                    let proactive=if activation=="proactive" {
                                        let id=frame.payload.get("offer_id").and_then(Value::as_u64);
                                        let (offer,_)=offered.as_ref().filter(|(offer,_)|Some(offer.offer_id as u64)==id).ok_or("proactive_offer_unknown")?;
                                        Some(offer.offer_id)
                                    } else {None};
                                    if proactive.is_none() && activation!="touch" && !(activation=="wake_word" && frame.payload.get("keyword").and_then(Value::as_str)==Some("小呆小呆")){return Err("explicit_activation_required".into());}
                                    let token:RuntimeToken=parse_runtime(&frame.payload)?;
                                    let output=DeviceOutput::negotiate(&frame.payload)?;
                                    if proactive.is_some() && !output.as_ref().is_some_and(|o|o.audio && o.presentation) {return Err("proactive_requires_spoken_presentation".into());}
                                    let conversation_id=format!("device-{}-{}",device_id,uuid::Uuid::new_v4().simple());
                                    let mut response=json!({"conversation_id":conversation_id,"runtime":token});
                                    if let Some(options)=&output {
                                        response["output"]=options.confirmed();
                                        if options.audio && options.live {
                                            // Publish the opening activation before spawning idle work;
                                            // capture starts before Brain.active contains a turn.
                                            brain.device_activity(&device_id,json!({"phase":"idle","conversation_id":conversation_id,
                                                "turn_id":null,"cancellable":false,"runtime":token}));
                                            brain.warm_acknowledgements().await;
                                        }
                                    }
                                    context=None;first_speech=None;supplement=None;
                                    engagement=Some(Engagement{conversation_id:conversation_id.clone(),token,last_turn:None,last_user:Instant::now(),opened:Instant::now(),output,input_failures:0,proactive});
                                    Ok(response)
                                },
                                "assistant.supplement"|"assistant.cancel"|"session.close"=>{
                                    let is_supplement=frame.method=="assistant.supplement";
                                    if is_supplement {
                                        let session=engagement.as_ref().ok_or("explicit_session_required")?;
                                        let token=parse_runtime(&frame.payload)?;
                                        if !session.output.as_ref().is_some_and(|o|o.live) || session.token!=token ||
                                            session.last_turn!=Some(token.turn_id) ||
                                            !(active.as_ref().is_some_and(|a|a.token==token) || delivery.as_ref().is_some_and(|d|d.token==token)
                                                || playback.as_ref().is_some_and(|p|p.token==token)) {return Err("stale_supplement".into());}
                                    }
                                    // Older clients omit runtime and use ordered WSS. A tagged stop must
                                    // match exactly; never revoke the next question because of a late tap.
                                    if frame.payload.get("runtime").is_some() {
                                        let token=parse_runtime(&frame.payload)?;
                                        if engagement.as_ref().is_none_or(|e| e.token!=token) {return Err("stale_runtime".into());}
                                    }
                                    supplement=if is_supplement {
                                        let session=engagement.as_ref().expect("validated session");
                                        let previous=format!("{}-{}",session.conversation_id,session.token.turn_id);
                                        Some((session.token,super::Supplement{reservation:brain.fast.reserve_followup(&previous),live:context.take(),previous}))
                                    } else {None};
                                    if let Some(mut turn)=active.take(){
                                        // Fence first. The producer's join waits off this branch so a slow
                                        // provider cannot occupy the device control socket. Dropping the
                                        // task still aborts it if the bounded cleanup queue is full.
                                        fence_gen = fence_gen.saturating_add(1);
                                        answer_pending_commit(&mut socket, &mut pending_commit, "cancelled").await?;
                                        pending_preps.clear();
                                        let turn_id = turn.turn_id.clone();
                                        let response = cancelled_response(&turn);
                                        if brain.config.responsive.quick_decouple {
                                            turn.task.abort();
                                            if cleanup_tx.try_send(turn.task).is_err() { /* Drop aborts. */ }
                                        } else {
                                            turn.task.stop().await;
                                        }
                                        brain.emit("turn.cancelled",Some(&turn_id),json!({"code":if is_supplement {"supplemented"} else {"device_stop"}}));
                                        send(&mut socket,Message::Text(response)).await?;
                                    } else if brain.config.responsive.quick_decouple {
                                        fence_gen = fence_gen.saturating_add(1);
                                        answer_pending_commit(&mut socket, &mut pending_commit, "cancelled").await?;
                                        pending_preps.clear();
                                    }
                                    if let Some(session)=&engagement{brain.tasks.revoke_outputs(Some(&session.conversation_id))?;}
                                    release_unheard_lead(&brain, &mut held, &mut inflight_lead);
                                    if let Some(offer)=pending_reminder.as_ref().map(|reminder| reminder.offer.clone()) {
                                        release_offer_row(&brain, &mut held, &offer);
                                    }
                                    pending_lead=None;
                                    pending_reminder=None;
                                    audio=None;playback=None;plain=None;display_result=None;delivery=None;
                                    context=None;first_speech=None;
                                    if frame.method=="session.close"{engagement=None;}
                                    Ok(json!({"cancelled":true}))
                                },
                                "audio.pause"=>{
                                    let session=engagement.as_ref().ok_or("explicit_session_required")?;
                                    let (capture,token)=audio.as_mut().ok_or("audio_session_required")?;
                                    if !session.output.as_ref().is_some_and(|o|o.live) || parse_runtime(&frame.payload)?!=*token ||
                                        stream_id(&frame.payload)?!=capture.stream_id {return Err("audio_session_mismatch".into());}
                                    let frames=frame.payload["frames"].as_u64().filter(|n|*n<=48000).ok_or("audio_frame_count_required")? as u32;
                                    capture.note_pause(frames);
                                    Ok(json!({"accepted":true}))
                                },
                                "proactive.accept"=>{
                                    let session=engagement.as_ref().ok_or("explicit_session_required")?;
                                    if active.is_some() || playback.is_some() || foreground_delivery_blocks(delivery.as_ref(), brain.config.responsive.quick_decouple){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    let id=frame.payload.get("offer_id").and_then(Value::as_u64);
                                    if session.proactive.map(u64::from)!=id {return Err("proactive_offer_unknown".into());}
                                    let (mut offer,offered_at)=offered.take_if(|(offer,_)|Some(offer.offer_id as u64)==id).ok_or("proactive_offer_unknown")?;
                                    if offered_at.elapsed()>Duration::from_millis(offer.ttl_ms as u64) {
                                        brain.proactive_journal(&device_id,&offer,"expired",Some("offer_expired"));
                                        return Err("proactive_offer_expired".into());
                                    }
                                    // Final Mac-side guardrails at the moment the device chose.
                                    if let Err(reason)=brain.proactive_admit(&offer) {
                                        brain.proactive_journal(&device_id,&offer,"declined",Some(&reason));
                                        return Err(format!("proactive_declined:{reason}"));
                                    }
                                    if offer.moment == "task_result" {
                                        let delivery=offer.delivery_id.clone().ok_or("result_missing")?;
                                        let row=brain.tasks.inbox_by_id(&delivery)?.ok_or("result_missing")?;
                                        if !row.tell_me || row.private_scope { return Err("result_missing".into()); }
                                        let reserve=format!("reserve-{}", offer.offer_id);
                                        let revision=brain.tasks.claim_delivery(&delivery, row.revision, &reserve, &reserve)?;
                                        held.insert(delivery, reserve);
                                        offer.delivery_revision=revision;
                                    } else {
                                        let reminder_id=offer.reminder_id.as_deref().ok_or("reminder_required")?;
                                        if !brain.memory.claim_reminder_offer(offer.offer_id,&device_id,reminder_id,crate::time::now_ms() as i64)? {
                                            brain.proactive_journal(&device_id,&offer,"declined",Some("reminder_in_progress"));
                                            return Err("reminder_in_progress".into());
                                        }
                                    }
                                    let reminder=PendingReminder{brain:Arc::clone(&brain),device_id:device_id.clone(),
                                        offer:offer.clone(),token,settled:false};
                                    let turn_id=format!("{}-{}",session.conversation_id,token.turn_id);
                                    let conversation=session.conversation_id.clone();
                                    if brain.config.responsive.quick_decouple {
                                        pending_reminder=Some(reminder);
                                        let tx=prep_tx.clone();let brain2=Arc::clone(&brain);let line=offer.line.clone();let local_date=offer.local_date.clone();
                                        let request_id=frame.id.clone();let fence=fence_gen;
                                        pending_preps.push(InFlightPrep{fence,turn_id:turn_id.clone()});
                                        tokio::spawn(async move {
                                            let result=segments::prepare_proactive(&brain2,&turn_id,&line,&local_date).await;
                                            let _=tx.send(BackgroundPrep::Proactive{fence,token,turn_id,request_id,conversation_id:conversation,offer,result}).await;
                                        });
                                        pending=true;return Ok(json!({}));
                                    }
                                    let (plan,prepared)=match segments::prepare_proactive(&brain,&turn_id,&offer.line,&offer.local_date).await {
                                        Ok(prepared)=>prepared,
                                        Err(error)=>return Err(error),
                                    };
                                    // The reminder may have been cancelled or quiet hours may have started
                                    // while TTS was running.
                                    brain.proactive_admit(&offer)?;
                                    let mut next=segments::Delivery::new(&brain,&device_id,&conversation,token,turn_id,plan,prepared,vec![],true,1)?;
                                    if let Some(id)=offer.delivery_id.as_deref() {
                                        if brain.tasks.rebind_lease(id, offer.delivery_revision, &next.lease_id, &next.playback_id).is_ok() {
                                            held.insert(id.to_owned(), next.lease_id.clone());
                                        }
                                    }
                                    next.request_id=frame.id.clone();delivery=Some(next);
                                    pending_reminder=Some(reminder);
                                    let response=json!({"v":3,"type":"response","id":frame.id,"ok":true,"runtime":token,"payload":{"presentation_pending":true}});
                                    send(&mut socket,Message::Text(response.to_string())).await?;
                                    pending=true;Ok(json!({}))
                                },
                                "audio.begin"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if session.proactive.is_some(){return Err("proactive_session_is_speaker_only".into());}
                                    if active.is_some() || audio.is_some() || playback.is_some() || foreground_delivery_blocks(delivery.as_ref(), brain.config.responsive.quick_decouple){return Err("session_busy".into());}
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
                                    if supplement.as_ref().is_some_and(|(previous,_)|!token.directly_follows(*previous)) {
                                        supplement=None;
                                    }
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
                                    let written=frame.payload.get("sample_position").map(|v|v.as_u64()
                                        .filter(|n|*n<=clip.offset as u64/2).ok_or("audio_playback_position_invalid")).transpose()?;
                                    if status=="finished" && written.is_some_and(|n|n!=clip.pcm.len() as u64/2) {
                                        return Err("audio_playback_incomplete".into());
                                    }
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
                                    let observed=frame.payload["device_monotonic_ms"].as_u64();
                                    let content=delivery.as_ref().is_none_or(|d|d.current_spoken.is_some() && (brain.config.responsive.quick_decouple || d.tasks.is_empty()) && !d.progress_only);
                                    let ack_ms=if status=="started" && delivery.as_ref().is_some_and(|d|d.live_content && d.progress_only) {
                                        first_speech.as_mut().filter(|clock|clock.token==token).and_then(|clock|{
                                            if !clock.sound_recorded {
                                                clock.sound_recorded=true;
                                                if let Some(ms)=observed.and_then(|o|o.checked_sub(clock.vad_end_ms?)).filter(|ms|*ms<240_000) {brain.latency.record("first_sound_ms",ms);}
                                            }
                                            observed?.checked_sub(clock.device_commit_ms?)
                                        })
                                    } else {None};
                                    let first=if status=="started" && content {
                                        first_speech.take_if(|clock|clock.token==token).map(|clock| {
                                            let elapsed=|from:Option<u64>|observed?.checked_sub(from?).filter(|ms|*ms<240_000);
                                            if let Some(ms)=elapsed(clock.vad_end_ms) {
                                                brain.latency.record("first_word_ms",ms);
                                                if !clock.sound_recorded {brain.latency.record("first_sound_ms",ms);}
                                            }
                                            json!({"server_commit_to_report_ms":clock.admitted.elapsed().as_millis(),
                                                "device_commit_to_write_ms":elapsed(clock.device_commit_ms),
                                                "device_vad_end_to_write_ms":elapsed(clock.vad_end_ms)})
                                        })
                                    } else {None};
                                    let code=frame.payload["code"].as_str().filter(|code|!code.is_empty() && code.len()<=40 &&
                                        code.bytes().all(|byte|byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte==b'_'));
                                    brain.emit("device.audio",Some(&clip.turn_id),json!({"device_id":device_id,
                                        "stream_id":stream,"status":status,"code":code,"runtime":token,
                                        "frames_sent":clip.sequence,"pcm_bytes":clip.pcm.len(),"frames_reported":frames,"bytes_reported":bytes,"written_samples":written,"first_speech":first,"device_commit_to_ack_ms":ack_ms,"device_monotonic_ms":observed}));
                                    if status=="finished" {
                                        let lead_heard=delivery.as_ref().and_then(|state| state.current.clone()).filter(|id| inflight_lead.as_ref().is_some_and(|lead| lead.segment_ids.iter().any(|segment| segment==id)));
                                        if lead_heard.is_some() {
                                            if let Some(lead)=inflight_lead.take() {
                                                let activation=delivery.as_ref().map(|state| state.activation_id.clone()).unwrap_or_default();
                                                let lease=delivery.as_ref().map(|state| state.lease_id.clone()).unwrap_or_default();
                                                let playback=delivery.as_ref().map(|state| state.playback_id.clone()).unwrap_or_default();
                                                spoken_leads.insert(lead.key.clone());
                                                record_played_lead(&brain,&activation,&lead,&lease,&playback);
                                            }
                                        }
                                        if let (Some(current),Some(text))=(&context,delivery.as_ref().filter(|d|!d.progress_only).and_then(|d|d.current_spoken.as_deref())) {
                                            if let Ok(mut current)=current.lock() {
                                                let room=650usize.saturating_sub(current.delivered.chars().count());
                                                current.delivered.extend(text.chars().take(room));
                                            }
                                        }
                                    }
                                    let _=brain.memory.journal_receipt(&clip.turn_id,"audio",json!({"stream_id":stream,"status":status,"frames":frames,"bytes":bytes,"code":code,
                                        "sample_position":written,"device_monotonic_ms":observed,"first_speech":first,"device_commit_to_ack_ms":ack_ms,
                                        "rendered_text":if status=="finished"{delivery.as_ref().filter(|d|!d.progress_only).and_then(|d|d.current_spoken.as_deref())}else{None}}),
                                        &format!("audio-{}-{stream}-{status}",clip.turn_id));
                                    if let Some(state)=delivery.as_mut(){
                                        state.record_hardware(&brain,match status{"started"=>"audio_started","finished"=>"audio_completed","failed"=>"failed",_=>"cancelled"},
                                            json!({"stream_id":stream,"frames":frames,"bytes":bytes,"code":code,"device_monotonic_ms":frame.payload["device_monotonic_ms"],"sample_position":frame.payload["sample_position"]}))?;
                                    }
                                    if status=="started" {
                                        if let Some(reminder)=pending_reminder.as_ref().filter(|r|r.token==token) {
                                            brain.proactive_journal(&device_id,&reminder.offer,"playing",None);
                                        }
                                    } else if pending_reminder.as_ref().is_some_and(|r|r.token==token) {
                                        if let Some(mut reminder)=pending_reminder.take() {
                                            if let Some(id)=reminder.offer.delivery_id.clone() {
                                                let played=status=="finished";
                                                let _=brain.tasks.settle_claimed(&id, played, if played {reminder.offer.line.as_str()} else {""});
                                                brain.proactive_journal(&device_id, &reminder.offer, if played {"spoken"} else {"lost"}, code);
                                            }
                                            reminder.settle(status=="finished",code);
                                        }
                                    }
                                    if status!="started" {
                                        playback_receipt=Some(PlaybackReceipt{token,stream_id:stream,status:status.to_owned()});
                                        playback=None;if delivery.is_none(){plain=None;}
                                    }
                                    Ok(json!({"recorded":true}))
                                },
                                "presentation.segment.report"=>{
                                    if playback.is_some() && frame.payload["status"]=="completed" {return Err("presentation_audio_not_completed".into());}
                                    let lead_heard=delivery.as_ref().and_then(|state| state.current.clone()).filter(|id| inflight_lead.as_ref().is_some_and(|lead| lead.segment_ids.iter().any(|segment| segment==id)));
                                    let lead_activation=delivery.as_ref().map(|state| state.activation_id.clone());
                                    let state=delivery.as_mut().ok_or("presentation_not_active")?;
                                    let finished=match state.report(&brain,&frame.payload){
                                        Ok(done)=>done,
                                        Err(error) if matches!(error.as_str(),"presentation_segment_failed"|"presentation_lease_revoked")=>{
                                            let token=state.token;let activation=state.activation_id.clone();
                                            if active.as_ref().is_some_and(|a|a.turn_id==state.turn_id) {active.take();}
                                            if pending_reminder.as_ref().is_some_and(|r|r.token==token) {pending_reminder=None;}
                                            let _=brain.tasks.revoke_outputs(Some(&activation));
                                            release_unheard_lead(&brain, &mut held, &mut inflight_lead);
                                            delivery=None;playback=None;plain=None;display_result=None;
                                            send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled","runtime":token,"code":error}).to_string())).await?;
                                            return Err(error);
                                        },Err(error)=>return Err(error),
                                    };
                                    let finished = finished && !state.live_content;
                                    if frame.payload["status"]=="completed" && lead_heard.is_some() {
                                        if let Some(lead)=inflight_lead.take() {
                                            spoken_leads.insert(lead.key.clone());
                                            record_played_lead(&brain, lead_activation.as_deref().unwrap_or(""), &lead, &state.lease_id, &state.playback_id);
                                        }
                                    }
                                    if finished {
                                        if let Some(task_id)=state.foreground_task.clone() {
                                            let _=brain.tasks.note_foreground(&state.turn_id, u64::from(state.token.turn_id), true, Some("delegated"), Some(&task_id));
                                        }
                                        delivery=None;plain=None;context=None;
                                    }
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
                                    if session.proactive.is_some(){return Err("proactive_session_is_speaker_only".into());}
                                    if active.is_some() || playback.is_some() || foreground_delivery_blocks(delivery.as_ref(), brain.config.responsive.quick_decouple){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    display_result=None;
                                    let from_audio=frame.method=="audio.commit";
                                    let text=if from_audio {
                                        let (capture,capture_token)=audio.as_mut().ok_or("audio_session_required")?;
                                        if token!=*capture_token || capture.stream_id!=stream_id(&frame.payload)? {return Err("audio_session_mismatch".into());}
                                        if let Some(transport)=plain.as_mut() {
                                            let expected=frame.payload.get("frames").and_then(Value::as_u64)
                                                .and_then(|n|u32::try_from(n).ok()).ok_or("audio_frame_count_required")?;
                                            // The tail may still be on the plain socket. Fence it as a
                                            // later event instead of waiting inside this branch.
                                            if brain.config.responsive.quick_decouple && capture.sequence<expected {
                                                if expected==0 || expected as usize>media::MAX_INPUT_PCM/2 || capture.sequence>expected {
                                                    return Err("audio_frame_count_invalid".into());
                                                }
                                                pending_commit=Some(PendingCommit{request_id:frame.id.clone(),token,stream_id:capture.stream_id,expected,
                                                    deadline:tokio::time::Instant::now()+Duration::from_secs(3),fence:fence_gen,
                                                    device_commit_ms:frame.payload["device_monotonic_ms"].as_u64(),vad_end_ms:frame.payload["vad_end_ms"].as_u64()});
                                                pending=true;return Ok(json!({}));
                                            }
                                            if !brain.config.responsive.quick_decouple {
                                                finish_plain_input(transport,capture,expected).await?;
                                            }
                                        }
                                        None
                                    } else {
                                        if audio.is_some(){return Err("audio_capture_in_progress".into());}
                                        Some(frame.payload.get("text").and_then(Value::as_str).ok_or("text_required")?.to_owned())
                                    };
                                    let question=text.as_deref().unwrap_or("").trim().to_owned();
                                    let activation_id=session.conversation_id.clone();
                                    apply_turn_delivery(&brain, &activation_id, &question, &mut pending_lead).await;
                                    begin_foreground_turn(&brain,&mut socket,&device_id,session,&mut audio,&mut supplement,&mut context,&mut first_speech,&mut active,&mut display_result,last_ack,&completion_tx,&mut fence_gen,&mut pending_preps,frame.id.clone(),token,text,from_audio,frame.payload["device_monotonic_ms"].as_u64(),frame.payload["vad_end_ms"].as_u64()).await?;
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
    release_unheard_lead(&brain, &mut held, &mut inflight_lead);
    if let Some(offer)=pending_reminder.as_ref().map(|reminder| reminder.offer.clone()) {
        release_offer_row(&brain, &mut held, &offer);
    }
    release_all_held(&brain, &mut held);
    let _=send(&mut socket, Message::Close(None)).await;
    cleanup_worker.abort();
    Ok(())
}

fn foreground_delivery_blocks(delivery: Option<&segments::Delivery>, quick_decouple: bool) -> bool {
    delivery.is_some_and(|state| state.blocks_new_input(quick_decouple))
}

/// A result or clarification prepared for the next device-started turn.
/// It is not a delivery and it is not journalled as played until a receipt.
#[derive(Clone)]
struct PreparedLead {
    task_id: String,
    key: String,
    plan: super::presentation::PresentationPlan,
    segments: Vec<super::orchestration::PreparedSegment>,
    speech: String,
    awaiting: bool,
    segment_ids: Vec<String>,
    delivery_id: String,
    delivery_revision: i64,
    kind_result: bool,
    promise_full: bool,
    expansion: bool,
}

async fn apply_turn_delivery(brain: &Arc<Brain>, activation: &str, transcript: &str, pending_lead: &mut Option<PreparedLead>) {
    if !brain.config.responsive.quick_decouple { return; }
    let kind = pending_lead.as_ref().map(|lead| if lead.kind_result { "result" } else { "needs_input" }).unwrap_or("result");
    if kind == "result" && super::task_delivery::declines_expansion(transcript) {
        if let Some(household) = brain.memory.household_settings().ok().and_then(|settings| settings["household_id"].as_str().map(str::to_owned)) {
            let _ = brain.tasks.settle_partials(&household);
        }
        if pending_lead.as_ref().is_some_and(|lead| lead.kind_result) { *pending_lead = None; }
        return;
    }
    if super::task_delivery::on_committed_turn(transcript, kind) == super::task_delivery::TurnDelivery::Defer {
        let activation_id = activation.to_owned();
        let brain2 = Arc::clone(brain);
        let _ = tokio::task::spawn_blocking(move || {
            let household = brain2.memory.household_settings().ok().and_then(|settings| settings["household_id"].as_str().map(str::to_owned))?;
            brain2.tasks.defer_results(&household, &activation_id).ok()?;
            brain2.tasks.settle_partials(&household).ok()
        }).await;
        if pending_lead.as_ref().is_some_and(|lead| lead.kind_result) { *pending_lead = None; }
        return;
    }
    if super::task_delivery::asks_to_expand(transcript) {
        if let Some(household) = brain.memory.household_settings().ok().and_then(|settings| settings["household_id"].as_str().map(str::to_owned)) {
            let speaker = brain.named_person(activation);
            if let Ok(Some(row)) = brain.tasks.expansion_target(&household, speaker.as_deref()) {
                if let Ok(task) = brain.tasks.task_for_delivery(&row.delivery_id) {
                    let task_id = task.task_id.clone();
                    if let Ok((plan, segments)) = segments::prepare_line(Arc::clone(brain), task, &row.result_text).await {
                        let segment_ids = segments.iter().map(|segment| segment.segment_id.clone()).collect();
                        let speech = row.result_text.clone();
                        *pending_lead = Some(PreparedLead {
                            task_id, key: row.delivery_id.clone(), plan, segments, speech, awaiting: false, segment_ids,
                            delivery_id: row.delivery_id, delivery_revision: row.revision, kind_result: true, promise_full: true, expansion: true,
                        });
                    }
                }
            }
        }
    } else if super::task_delivery::declines_expansion(transcript) {
        if let Some(household) = brain.memory.household_settings().ok().and_then(|settings| settings["household_id"].as_str().map(str::to_owned)) {
            let _ = brain.tasks.settle_partials(&household);
        }
    }
}

fn release_offer_row(brain: &Brain, held: &mut HashMap<String, String>, offer: &super::proactive::Offer) {
    if let Some(id) = offer.delivery_id.as_deref() {
        release_held(brain, held, id);
    }
}

fn release_held(brain: &Brain, held: &mut HashMap<String, String>, delivery_id: &str) {
    if let Some(lease) = held.remove(delivery_id) {
        let _ = brain.tasks.release_attempt(delivery_id, &lease, "");
    }
}

fn release_all_held(brain: &Brain, held: &mut HashMap<String, String>) {
    let ids: Vec<String> = held.keys().cloned().collect();
    for id in ids { release_held(brain, held, &id); }
}

/// A stale failure (an old fence, or a session that has already moved on) does not
/// release. Interrupt and Speak do, and only the rows this connection claimed.
fn failure_releases_current(event: &BackgroundPrep, fence_now: u64, active: Option<&ActivePeer>, engagement: Option<&Engagement>) -> bool {
    let BackgroundPrep::Failure { fence, token, activation_id, spoke, result, .. } = event else { return false; };
    let speech_ready = matches!(result, Ok(Some(_)));
    let fresh = *fence == fence_now && active.is_none()
        && engagement.is_some_and(|session| session.token == *token && session.conversation_id == *activation_id);
    matches!(failure_disposition(*spoke, speech_ready, fresh), FailureDisposition::Interrupt | FailureDisposition::Speak)
}

fn remembered_transcript(brain: &Brain, turn_id: &str) -> String {
    if let Some(text) = brain.understood_text(turn_id).filter(|text| !text.is_empty()) {
        return text;
    }
    brain.memory.household_rpc("household.source", &json!({"conversation_turn_id": turn_id}))
        .ok().flatten()
        .and_then(|value| value["text"].as_str().or_else(|| value["input_text"].as_str()).map(str::to_owned))
        .unwrap_or_default()
}

fn claim_sent_lead(brain: &Brain, held: &mut HashMap<String, String>, lead: &PreparedLead, lease: &str, playback: &str) {
    if lead.delivery_id.is_empty() { return; }
    if brain.tasks.claim_delivery(&lead.delivery_id, lead.delivery_revision, lease, playback).is_ok() {
        held.insert(lead.delivery_id.clone(), lease.to_owned());
    }
}

/// A segment the device never confirmed does not stay on this connection's lease.
/// A different connection's newer claim is left alone.
fn release_unheard_lead(brain: &Brain, held: &mut HashMap<String, String>, inflight: &mut Option<PreparedLead>) {
    if let Some(lead) = inflight.take() {
        release_held(brain, held, &lead.delivery_id);
    }
}

/// The played result is its own household row. The user's turn keeps the answer
/// orchestration already stored, including a partial outcome.
fn record_played_lead(brain: &Brain, activation: &str, lead: &PreparedLead, lease_id: &str, playback_id: &str) {
    if activation.is_empty() || lead.speech.trim().is_empty() { return; }
    let Ok(task) = brain.tasks.get(&lead.task_id) else { return };
    let state = task.state.as_str();
    // A clarification and a later success for the same task are different rows.
    let turn_id = super::task_delivery::played_row_id(activation, &lead.task_id, state, lead.expansion);
    if turn_id.len() > 160 { return; }
    let allow_cloud = brain.memory.household_settings().ok().and_then(|settings| settings["allow_cloud"].as_bool()).unwrap_or(false);
    let _ = brain.memory.journal_input(&crate::memory::JournalInput {
        turn_id: turn_id.clone(), activation_id: activation.into(), utterance_id: None, text: String::new(),
        occurred_at_ms: crate::time::now_ms() as i64, input_kind: "text".into(), speaker_person_id: None,
        private_profile_id: None, allow_cloud, metadata: json!({"task_result": &lead.task_id, "task_state": state}),
    });
    let _ = brain.memory.journal_finish(&turn_id, "completed", Some(&lead.speech), json!({"spoken_device": true}));
    let _ = brain.memory.journal_receipt(&turn_id, "audio", json!({"status": "finished", "rendered_text": lead.speech, "playback_id": playback_id, "lease_id": lease_id}), &format!("lead-{}-{state}-{}", lead.task_id, playback_id));
    let mut result = if task.result.is_object() { task.result.clone() } else { json!({}) };
    result["lead_spoken_state"] = json!(state);
    if lead.awaiting { result["clarification_turn_id"] = json!(turn_id); }
    let _ = brain.tasks.set_state(&lead.task_id, task.state, &task.stage, Some(result));
    if !lead.delivery_id.is_empty() && !lease_id.is_empty() {
        if let Ok(Some(row)) = brain.tasks.inbox_row(&lead.task_id) {
            if row.delivery_id == lead.delivery_id {
                let _ = brain.tasks.apply_playback(&lead.delivery_id, row.revision, lease_id, true, &lead.speech, lead.promise_full);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FailureDisposition { Speak, Interrupt, ErrorOnly, Drop }

/// Nothing was said: an error response is enough. An acknowledgement or a partial
/// answer with no replacement line cancels that live answer. A stale fence,
/// a new foreground turn, or a different session token drops the line.
fn failure_disposition(spoke: bool, speech_ready: bool, fresh: bool) -> FailureDisposition {
    if !fresh { return FailureDisposition::Drop; }
    if speech_ready { return FailureDisposition::Speak; }
    if spoke { FailureDisposition::Interrupt } else { FailureDisposition::ErrorOnly }
}

async fn answer_pending_commit(socket: &mut GatewaySocket, pending: &mut Option<PendingCommit>, code: &str) -> Result<(), String> {
    if let Some(commit) = pending.take() {
        send(socket, Message::Text(response_error(&commit.request_id, code))).await?;
    }
    Ok(())
}

struct PendingCommit {
    request_id: String,
    token: RuntimeToken,
    stream_id: u32,
    expected: u32,
    deadline: tokio::time::Instant,
    fence: u64,
    device_commit_ms: Option<u64>,
    vad_end_ms: Option<u64>,
}

/// Speech or a plain-audio commit still running after its foreground owner was cleared.
struct InFlightPrep { fence: u64, turn_id: String }

/// A matching cancel retires every deferred result spawned at this fence.
/// An unrelated turn leaves the fence alone.
fn retire_deferred(preps: &[InFlightPrep], commit_turn: Option<&str>, cancellation: &super::Cancellation, conversation: Option<&str>) -> bool {
    cancellation.closes_session(conversation)
        || preps.iter().any(|prep| cancellation.matches_turn(&prep.turn_id, conversation))
        || commit_turn.is_some_and(|turn| cancellation.matches_turn(turn, conversation))
}

enum BackgroundPrep {
    Failure {
        fence: u64,
        token: RuntimeToken,
        turn_id: String,
        request_id: String,
        index: u32,
        activation_id: String,
        error: String,
        spoke: bool,
        result: Result<Option<(super::presentation::PresentationPlan, Vec<super::orchestration::PreparedSegment>)>, String>,
    },
    Proactive {
        fence: u64,
        token: RuntimeToken,
        turn_id: String,
        request_id: String,
        conversation_id: String,
        offer: super::proactive::Offer,
        result: Result<(super::presentation::PresentationPlan, Vec<super::orchestration::PreparedSegment>), String>,
    },
}

type GatewaySocket = tokio_tungstenite::WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>;

#[allow(clippy::too_many_arguments)]
async fn begin_foreground_turn(
    brain: &Arc<Brain>,
    socket: &mut GatewaySocket,
    device_id: &str,
    session: &mut Engagement,
    audio: &mut Option<(media::AudioInput, RuntimeToken)>,
    supplement: &mut Option<(RuntimeToken, super::Supplement)>,
    context: &mut Option<super::LiveTurnContext>,
    first_speech: &mut Option<FirstSpeech>,
    active: &mut Option<ActivePeer>,
    display_result: &mut Option<DisplayResult>,
    last_ack: Option<Instant>,
    completion_tx: &mpsc::Sender<TurnUpdate>,
    fence: &mut u64,
    pending_preps: &mut Vec<InFlightPrep>,
    request_id: String,
    token: RuntimeToken,
    text: Option<String>,
    from_audio: bool,
    device_commit_ms: Option<u64>,
    vad_end_ms: Option<u64>,
) -> Result<(), String> {
    *display_result = None;
    // A new foreground turn retires a failure or reminder line prepared for the previous one.
    if brain.config.responsive.quick_decouple {
        *fence = fence.saturating_add(1);
        pending_preps.clear();
    }
    let (candidate, wav, input_error) = if from_audio {
        let (mut capture, _) = audio.take().ok_or("audio_session_required")?;
        let candidate = capture.candidate.take();
        match capture.wav_base64() {
            Ok(wav) => (candidate, Some(wav), None),
            Err(error) => (candidate, None, Some(error)),
        }
    } else { (None, None, None) };
    session.token = token;
    session.last_turn = Some(token.turn_id);
    session.last_user = Instant::now();
    let id = format!("{}-{}", session.conversation_id, token.turn_id);
    let input = device_turn_input(session, id.clone(), text, wav);
    let compact = session.output.is_some();
    let audio_requested = input.speak.unwrap_or(false);
    let turn_brain = Arc::clone(brain);
    let tx = completion_tx.clone();
    let completion_id = id.clone();
    let live_output = session.output.as_ref().filter(|o| o.live && o.audio).map(|_| tx.clone());
    *first_speech = Some(FirstSpeech { token, admitted: Instant::now(), device_commit_ms, vad_end_ms, sound_recorded: false });
    let supplement = supplement.take().filter(|(previous, _)| token.directly_follows(*previous)).map(|(_, item)| item);
    *context = session.output.as_ref().filter(|o| o.live).map(|_| super::LiveTurnContext::default());
    let shared_context = context.clone();
    let acknowledge = slow_ack_eligible(live_output.is_some() && from_audio, session.opened.elapsed(), last_ack.map(|when| when.elapsed()));
    let started_id = id.clone();
    let accepted_id = request_id.clone();
    let task = tokio::spawn(async move {
        let started = Instant::now();
        let result = match input_error {
            Some(error) => Err(error),
            None => turn_brain.turn_with(input, false, super::DeviceWork {
                output: live_output, candidate, supplement, acknowledge, context: shared_context,
                spoken_device: audio_requested && compact,
            }).await,
        };
        let _ = tx.send(TurnUpdate::Completed(Box::new(Completed {
            id: request_id, turn_id: completion_id, token, compact, audio_requested,
            processing_time: started.elapsed(), result,
        }))).await;
    });
    brain.emit("device.turn.started", Some(&started_id), json!({"device_id": device_id}));
    *active = Some(ActivePeer {
        turn_id: started_id, request_id: accepted_id.clone(), token, task: TurnTask(task),
        prefix: Vec::new(), acknowledgement_sent: false, lead_sent: false,
        progress_seq: 0, last_stage: None, last_progress: Instant::now(),
    });
    send(socket, Message::Text(json!({"v":3,"type":"event","event":"turn.accepted","id":accepted_id,"runtime":token}).to_string())).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn seal_pending_audio(
    brain: &Arc<Brain>,
    socket: &mut GatewaySocket,
    device_id: &str,
    engagement: &mut Option<Engagement>,
    audio: &mut Option<(media::AudioInput, RuntimeToken)>,
    supplement: &mut Option<(RuntimeToken, super::Supplement)>,
    context: &mut Option<super::LiveTurnContext>,
    first_speech: &mut Option<FirstSpeech>,
    active: &mut Option<ActivePeer>,
    display_result: &mut Option<DisplayResult>,
    last_ack: Option<Instant>,
    completion_tx: &mpsc::Sender<TurnUpdate>,
    pending_commit: &mut Option<PendingCommit>,
    fence_gen: &mut u64,
    pending_preps: &mut Vec<InFlightPrep>,
) -> Result<(), String> {
    let Some(commit) = pending_commit.as_ref() else { return Ok(()) };
    if commit.fence != *fence_gen {
        answer_pending_commit(socket, pending_commit, "cancelled").await?;
        return Ok(());
    }
    let ready = audio.as_ref().is_some_and(|(capture, token)| *token == commit.token && capture.stream_id == commit.stream_id && capture.sequence >= commit.expected);
    if !ready { return Ok(()) }
    let commit = pending_commit.take().ok_or("audio_commit_missing")?;
    let Some(session) = engagement.as_mut() else {
        send(socket, Message::Text(response_error(&commit.request_id, "cancelled"))).await?;
        return Err("explicit_session_required".into());
    };
    let request_id = commit.request_id.clone();
    match begin_foreground_turn(brain, socket, device_id, session, audio, supplement, context, first_speech, active, display_result, last_ack, completion_tx, fence_gen, pending_preps, commit.request_id, commit.token, None, true, commit.device_commit_ms, commit.vad_end_ms).await {
        Ok(()) => Ok(()),
        Err(error) if active.is_none() => {
            send(socket, Message::Text(response_error(&request_id, "cancelled"))).await?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_background_prep(
    brain: &Arc<Brain>,
    socket: &mut GatewaySocket,
    device_id: &str,
    fence_now: u64,
    active: &Option<ActivePeer>,
    engagement: &mut Option<Engagement>,
    delivery: &mut Option<segments::Delivery>,
    playback: &mut Option<Playback>,
    plain: &mut Option<plain_audio::PlainAudio>,
    display_result: &mut Option<DisplayResult>,
    pending_reminder: &mut Option<PendingReminder>,
    held: &mut HashMap<String, String>,
    event: BackgroundPrep,
) -> bool {
    match event {
        BackgroundPrep::Failure { fence, token, turn_id, request_id, index, activation_id, error, spoke, result } => {
            let spoken = match result {
                Ok(value) => value,
                Err(code) => {
                    brain.emit("device.failure_speech.unavailable", Some(&turn_id), json!({"code": code}));
                    None
                }
            };
            let fresh = fence == fence_now
                && active.is_none()
                && engagement.as_ref().is_some_and(|session| session.token == token && session.conversation_id == activation_id);
            match failure_disposition(spoke, spoken.is_some(), fresh) {
                FailureDisposition::Drop => send(socket, Message::Text(response_error(&request_id, "cancelled"))).await.is_ok(),
                FailureDisposition::ErrorOnly => {
                    brain.emit("device.turn.failed", Some(&turn_id), json!({"device_id": device_id, "code": error.split(':').next().unwrap_or("turn_failed"), "spoken": false}));
                    send(socket, Message::Text(response_error(&request_id, &error))).await.is_ok()
                }
                FailureDisposition::Interrupt => {
                    if let Some(session) = engagement.as_ref() { let _ = brain.tasks.revoke_outputs(Some(&session.conversation_id)); }
                    *delivery = None; *playback = None; *display_result = None; *plain = None;
                    brain.emit("device.turn.failed", Some(&turn_id), json!({"device_id": device_id, "code": error.split(':').next().unwrap_or("turn_failed"), "spoken": false}));
                    let event = json!({"v":3,"type":"event","event":"session.cancelled","runtime":token,"code":"live_answer_interrupted"});
                    send(socket, Message::Text(event.to_string())).await.is_ok()
                        && send(socket, Message::Text(response_error(&request_id, &error))).await.is_ok()
                }
                FailureDisposition::Speak => {
                    let Some((plan, prepared)) = spoken else { return false };
                    if spoke {
                        if let Some(session) = engagement.as_ref() { let _ = brain.tasks.revoke_outputs(Some(&session.conversation_id)); }
                        *delivery = None; *playback = None; *display_result = None;
                    }
                    brain.emit("device.turn.failed", Some(&turn_id), json!({"device_id": device_id, "code": error.split(':').next().unwrap_or("turn_failed"), "spoken": true}));
                    let Some(session) = engagement.as_ref() else {
                        return send(socket, Message::Text(response_error(&request_id, &error))).await.is_ok();
                    };
                    match segments::Delivery::new(brain, device_id, &session.conversation_id, token, turn_id, plan, prepared, vec![], true, index) {
                        Ok(mut next) => {
                            next.total_sent = index.saturating_sub(1);
                            next.request_id = request_id.clone();
                            *delivery = Some(next);
                            let response = json!({"v":3,"type":"response","id":request_id,"ok":true,"runtime":token,"payload":{"presentation_pending":true}});
                            send(socket, Message::Text(response.to_string())).await.is_ok()
                        }
                        Err(install) => send(socket, Message::Text(response_error(&request_id, &install))).await.is_ok(),
                    }
                }
            }
        }
        BackgroundPrep::Proactive { fence, token, turn_id, request_id, conversation_id, offer, result } => {
            if fence != fence_now || active.is_some() || engagement.as_ref().is_none_or(|session| session.token != token) {
                if pending_reminder.as_ref().is_some_and(|reminder| reminder.token == token) {
                    if let Some(mut reminder) = pending_reminder.take() { reminder.settle(false, Some("interrupted")); }
                }
                release_offer_row(brain, held, &offer);
                return send(socket, Message::Text(response_error(&request_id, "cancelled"))).await.is_ok();
            }
            if let Err(reason) = brain.proactive_admit(&offer) {
                if let Some(mut reminder) = pending_reminder.take() { reminder.settle(false, Some(&reason)); }
                brain.proactive_journal(device_id, &offer, "declined", Some(&reason));
                release_offer_row(brain, held, &offer);
                return send(socket, Message::Text(response_error(&request_id, &format!("proactive_declined:{reason}")))).await.is_ok();
            }
            match result {
                Ok((plan, prepared)) => match segments::Delivery::new(brain, device_id, &conversation_id, token, turn_id, plan, prepared, vec![], true, 1) {
                    Ok(mut next) => {
                        if let Some(id) = offer.delivery_id.as_deref() {
                            let rebound = brain.tasks.rebind_lease(id, offer.delivery_revision, &next.lease_id, &next.playback_id).is_ok()
                                || brain.tasks.claim_delivery(id, offer.delivery_revision, &next.lease_id, &next.playback_id).is_ok();
                            if !rebound {
                                let _ = brain.tasks.revoke_outputs(Some(&conversation_id));
                                brain.proactive_journal(device_id, &offer, "declined", Some("delivery_conflict"));
                                release_offer_row(brain, held, &offer);
                                return send(socket, Message::Text(response_error(&request_id, "delivery_conflict"))).await.is_ok();
                            }
                            held.insert(id.to_owned(), next.lease_id.clone());
                        }
                        next.request_id = request_id.clone();
                        *delivery = Some(next);
                        let response = json!({"v":3,"type":"response","id":request_id,"ok":true,"runtime":token,"payload":{"presentation_pending":true}});
                        send(socket, Message::Text(response.to_string())).await.is_ok()
                    }
                    Err(error) => {
                        if let Some(mut reminder) = pending_reminder.take() { reminder.settle(false, Some("presentation_failed")); }
                        release_offer_row(brain, held, &offer);
                        send(socket, Message::Text(response_error(&request_id, &error))).await.is_ok()
                    }
                },
                Err(error) => {
                    if let Some(mut reminder) = pending_reminder.take() { reminder.settle(false, Some("speech_unavailable")); }
                    release_offer_row(brain, held, &offer);
                    send(socket, Message::Text(response_error(&request_id, &error))).await.is_ok()
                }
            }
        }
    }
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
fn empty_input_error(error:&str)->bool {
    matches!(error.split(':').next().unwrap_or("").trim(),"asr_no_speech"|"no_speech"|"audio_too_quiet"|"audio_too_short")
}
fn response_error(id: &str, error: &str) -> String {
    let reason = error.split(':').next().unwrap_or("request_failed");
    let code = if empty_input_error(error){"asr_no_speech"}else{reason};
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
    fn a_turn_cancel_retires_an_inflight_failure_prep() {
        let prep = [InFlightPrep { fence: 1, turn_id: "device-1-4".into() }];
        let turn = super::super::Cancellation { scope: super::super::CancelScope::Turn("device-1-4".into()), reason: "parent_stop" };
        let other = super::super::Cancellation { scope: super::super::CancelScope::Turn("device-1-9".into()), reason: "parent_stop" };
        let session = super::super::Cancellation { scope: super::super::CancelScope::DeviceSession("device-1".into()), reason: "parent_stop" };
        assert!(retire_deferred(&prep, None, &turn, Some("device-1")), "the console stop targets the prep whose foreground owner is already gone");
        assert!(!retire_deferred(&prep, None, &other, Some("device-1")));
        assert!(retire_deferred(&prep, None, &session, Some("device-1")));
        assert!(retire_deferred(&[], Some("device-1-3"), &super::super::Cancellation {
            scope: super::super::CancelScope::Turn("device-1-3".into()), reason: "parent_stop",
        }, Some("device-1")), "a late plain-audio commit is the same kind of deferred work");
        assert!(!retire_deferred(&[], Some("device-1-3"), &other, Some("device-1")));
    }

    #[test]
    fn a_failed_prep_after_an_ack_interrupts_only_the_live_answer() {
        assert_eq!(failure_disposition(true, false, true), FailureDisposition::Interrupt);
        assert_eq!(failure_disposition(false, false, true), FailureDisposition::ErrorOnly, "a failure before any speech does not revoke the display");
        assert_eq!(failure_disposition(true, true, true), FailureDisposition::Speak);
        assert_eq!(failure_disposition(false, true, true), FailureDisposition::Speak);
        assert_eq!(failure_disposition(true, false, false), FailureDisposition::Drop, "a new turn or a cancel drops the stale line");
    }

    #[test]
    fn slow_ack_respects_wake_and_recent_speech() {
        let wake = Duration::from_secs(5);
        let cooldown = Duration::from_secs(20);
        let tick = Duration::from_millis(1);
        assert!(!slow_ack_eligible(true, wake - tick, None));
        assert!(slow_ack_eligible(true, wake, None));
        assert!(!slow_ack_eligible(true, wake, Some(cooldown - tick)));
        assert!(slow_ack_eligible(true, wake, Some(cooldown)));
        assert!(!slow_ack_eligible(false, wake, None));
    }

    #[test]
    fn last_peer_drop_clears_activity_on_every_exit() {
        let dir=std::env::temp_dir().join(format!("xiaodai-peer-drop-{}",uuid::Uuid::new_v4()));
        let brain=Brain::new(dir.clone(),None,Arc::new(|_|{})).unwrap();
        let online=Arc::new(StdMutex::new(BTreeMap::new()));
        let first=OnlinePeer::new("test-device".into(),Arc::clone(&online),Arc::clone(&brain));
        let second=OnlinePeer::new("test-device".into(),Arc::clone(&online),Arc::clone(&brain));
        brain.device_activity("test-device",json!({"phase":"listening","conversation_id":"active"}));
        drop(first);
        assert_eq!(brain.device_states.lock().unwrap()["test-device"]["conversation_id"],"active");
        drop(second);
        assert!(online.lock().unwrap().is_empty());
        assert_eq!(brain.device_states.lock().unwrap()["test-device"]["phase"],"disconnected");
        assert!(!super::super::acks::device_session_active(&brain.device_states.lock().unwrap()["test-device"]));
        drop(brain);
        let _=std::fs::remove_dir_all(dir);
    }

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
        for reason in ["no_speech: empty transcription", "audio_too_quiet: low energy", "audio_too_short: short recording", "asr_no_speech"] {
            let response: Value = serde_json::from_str(&response_error("r1", reason)).unwrap();
            assert_eq!(response["id"], "r1");
            assert_eq!(response["error"]["code"], "asr_no_speech");
            assert_eq!(response["error"]["message"], reason);
        }
        let response: Value = serde_json::from_str(&response_error("r1", "brain_busy")).unwrap();
        assert_eq!(response["error"]["code"], "brain_busy");
    }

    #[test]
    fn consecutive_empty_input_ends_after_one_retry_but_other_failures_reset_it() {
        let token=RuntimeToken{epoch:7,session_id:12,turn_id:1,focus_generation:0};
        let mut session=Engagement{conversation_id:"device-test".into(),token,opened:Instant::now(),
            proactive:None,last_turn:None,last_user:Instant::now(),output:None,input_failures:0};
        assert!(!session.failed_input_exhausted("no_speech: empty transcription"));
        assert!(session.failed_input_exhausted("audio_too_quiet: low energy"));
        assert!(!session.failed_input_exhausted("all_routes_unavailable"));
        assert!(!session.failed_input_exhausted("asr_no_speech"));
        assert!(session.failed_input_exhausted("audio_too_short: short recording"));
    }

    #[test]
    fn runtime_leases_advance_without_reopening_the_conversation() {
        let first = RuntimeToken { epoch: 7, session_id: 12, turn_id: 1, focus_generation: 0 };
        let mut session = Engagement { conversation_id: "device-test".into(), token: first, opened: Instant::now(), proactive: None,
            last_turn: None, last_user: Instant::now(), output: None, input_failures:0 };
        assert!(session.check_token(first).is_ok());
        session.last_turn = Some(1);
        assert!(session.check_token(first).is_err());
        let next = RuntimeToken { epoch: 8, turn_id: 2, ..first };
        assert!(session.check_token(next).is_ok());
        assert!(next.directly_follows(first));
        assert!(!RuntimeToken {turn_id:3,..next}.directly_follows(first));
        assert!(!RuntimeToken {epoch:9,..next}.directly_follows(first));
        assert!(!RuntimeToken {session_id:13,..next}.directly_follows(first));
        assert!(session.check_token(RuntimeToken { epoch: 7, ..next }).is_err());
        assert!(session.check_token(RuntimeToken { session_id: 13, ..next }).is_err());
        session.token = next;
        session.last_turn = Some(2);
        assert!(session.check_token(RuntimeToken { epoch: 7, turn_id: 3, ..next }).is_err());
    }

    #[test]
    fn silent_output_is_explicit_and_caption_respects_utf8_boundary() {
        let token = RuntimeToken { epoch: 7, session_id: 12, turn_id: 1, focus_generation: 0 };
        let mut session = Engagement { conversation_id: "device-test".into(), token, opened: Instant::now(), proactive: None,
            last_turn: None, last_user: Instant::now(), output: None, input_failures:0 };
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
        // The plain-audio tail is not here yet. Stop fences the commit immediately;
        // frames that arrive afterwards must not admit a model turn.
        let late_token=json!({"epoch":9,"session_id":12,"turn_id":3,"focus_generation":0});
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"late-begin","method":"audio.begin",
            "payload":{"stream_id":130,"format":"pcm_s16le","sample_rate":16000,"channels":1,
                "transport":"plain_tcp_v1","runtime":late_token}}).to_string())).await.unwrap();
        let late_begun:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(late_begun["ok"],true,"{late_begun}");
        let late_audio=&late_begun["payload"]["plain_audio"];
        let late_hex=late_audio["key"].as_str().unwrap();
        let late_secret=(0..32).map(|i|u8::from_str_radix(&late_hex[i*2..i*2+2],16).unwrap()).collect::<Vec<_>>();
        let late_mac=ring::hmac::Key::new(ring::hmac::HMAC_SHA256,&late_secret);
        let mut late_pcm=tokio::net::TcpStream::connect((late_audio["host"].as_str().unwrap(),late_audio["port"].as_u64().unwrap() as u16)).await.unwrap();
        let started=Instant::now();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"late-commit","method":"audio.commit",
            "payload":{"stream_id":130,"frames":3,"runtime":late_token}}).to_string())).await.unwrap();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"late-cancel","method":"assistant.cancel",
            "payload":{"runtime":late_token}}).to_string())).await.unwrap();
        let mut saw_accepted=false;
        let mut cancel_ok=false;
        let mut commit_terminal=0u32;
        let wait_until=Instant::now()+Duration::from_millis(800);
        while Instant::now()<wait_until && !(cancel_ok && commit_terminal==1) {
            let message=match timeout(Duration::from_millis(800),socket.next()).await {
                Ok(Some(Ok(message)))=>message,
                _=>break,
            };
            let Message::Text(text)=message else {continue};
            let reply:Value=serde_json::from_str(&text).unwrap();
            if reply["event"]=="turn.accepted" {saw_accepted=true;}
            if reply["id"]=="late-commit" {
                commit_terminal+=1;
                assert_eq!(reply["ok"], false, "{reply}");
                assert_eq!(reply["error"]["code"], "cancelled", "{reply}");
            }
            if reply["id"]=="late-cancel" && reply["ok"]==true {cancel_ok=true;}
        }
        assert!(cancel_ok && commit_terminal==1 && !saw_accepted && started.elapsed()<Duration::from_millis(800), "a discarded commit gets one cancelled response and no turn, elapsed {:?}", started.elapsed());
        for sequence in 0..3u32 {
            let samples=vec![0u8;640];
            let data=media::output_frame(130,sequence,&samples);
            let mut body=vec![b'I'];body.extend_from_slice(&data);
            let tag=ring::hmac::sign(&late_mac,&body);body.extend_from_slice(tag.as_ref());
            let _=late_pcm.write_all(&(body.len() as u32).to_be_bytes()).await;
            let _=late_pcm.write_all(&body).await;
        }
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"late-close","method":"session.close","payload":{}}).to_string())).await.unwrap();
        let late_closed:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(late_closed["event"],Value::Null,"late frames must not admit a turn: {late_closed}");
        assert_eq!((&late_closed["id"],&late_closed["ok"]),(&json!("late-close"),&json!(true)),"{late_closed}");
        // Unprompted lines (§12.2) as a fake device: offered only after proactive.ready,
        // the session is speaker-only, and the Mac re-checks its guardrails on accept.
        assert!(welcome["capabilities"].as_array().unwrap().iter().any(|v|v=="proactive.v1"));
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"close2","method":"session.close","payload":{}}).to_string())).await.unwrap();
        let _closed:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let zone:chrono_tz::Tz=brain.memory.household_settings().unwrap()["timezone"].as_str().unwrap().parse().unwrap();
        let local=chrono::DateTime::from_timestamp_millis(crate::time::now_ms() as i64).unwrap().with_timezone(&zone);
        let quiet=|hours:i64|(local+chrono::Duration::hours(hours)).format("%H:%M").to_string();
        brain.memory.set_proactive_settings(&json!({"enabled":true,"quiet_start":quiet(2),"quiet_end":quiet(3)})).unwrap();
        brain.memory.add_reminder(&json!({"text":"该喝水啦","due_at_ms":crate::time::now_ms() as i64})).unwrap();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ready1","method":"proactive.ready","payload":{"version":1}}).to_string())).await.unwrap();
        let mut offer=Value::Null;
        for _ in 0..2 {
            let message:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            if message["event"]=="proactive.offer" {offer=message["payload"].clone();} else {assert_eq!((&message["id"],&message["ok"]),(&json!("ready1"),&json!(true)));}
        }
        assert_eq!((offer["text"].as_str(),offer["show_if_absent"].as_bool(),offer["moment"].as_str()),(Some("提醒一下：该喝水啦"),Some(false),Some("reminder")));
        let offer_id=offer["offer_id"].as_u64().unwrap();
        let proactive_token=json!({"epoch":9,"session_id":13,"turn_id":1,"focus_generation":0});
        let output=json!({"version":1,"audio":true,"cards":["text","image"],"presentation":{"version":1,"frame_ack":true},"live_turns":true});
        for (id,method,payload,error) in [
            ("p-open-bad","session.open",json!({"activation":"proactive","offer_id":offer_id+1,"runtime":proactive_token,"output":output}),Some("proactive_offer_unknown")),
            ("p-open-silent","session.open",json!({"activation":"proactive","offer_id":offer_id,"runtime":proactive_token,
                "output":{"version":1,"audio":false,"cards":["text"],"presentation":{"version":1,"frame_ack":true}}}),Some("proactive_requires_spoken_presentation")),
            ("p-open","session.open",json!({"activation":"proactive","offer_id":offer_id,"runtime":proactive_token,"output":output}),None),
            ("p-listen","audio.begin",json!({"stream_id":125,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":proactive_token}),Some("proactive_session_is_speaker_only")),
            ("p-text","assistant.text",json!({"text":"你好","runtime":proactive_token}),Some("proactive_session_is_speaker_only")),
        ] {
            socket.send(Message::Text(json!({"v":3,"type":"request","id":id,"method":method,"payload":payload}).to_string())).await.unwrap();
            let reply:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!((&reply["id"],&reply["ok"]),(&json!(id),&json!(error.is_none())),"{reply}");
            assert_eq!(reply["error"]["code"].as_str(),error);
        }
        // The owner switched it off in the meantime: the device's moment is refused and journalled.
        brain.memory.set_proactive_settings(&json!({"enabled":false})).unwrap();
        for (id,error) in [("p-accept","proactive_declined"),("p-accept-again","proactive_offer_unknown")] {
            socket.send(Message::Text(json!({"v":3,"type":"request","id":id,"method":"proactive.accept","payload":{"offer_id":offer_id,"runtime":proactive_token}}).to_string())).await.unwrap();
            let reply:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!((&reply["id"],&reply["ok"],reply["error"]["code"].as_str()),(&json!(id),&json!(false),Some(error)),"{reply}");
        }
        let journal=brain.memory.proactive_journal(None,10).unwrap();
        assert_eq!((journal[0].status.as_str(),journal[0].reason.as_deref(),journal[0].moment.as_str()),("declined",Some("disabled"),"reminder"));
        socket.close(None).await.unwrap();brain.stop().await.unwrap();
        assert!(TcpStream::connect(&address).await.is_err());drop(brain);
    }

    #[tokio::test]
    async fn a_delegated_task_is_spoken_on_the_next_committed_turn() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        brain.memory.update_household_settings(&json!({"allow_cloud": true, "history_enabled": true})).unwrap();
        let summary = "还缺资料的年份";
        let task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::AwaitingInput);
        let begun = send_request(&mut socket, "follow-up", "audio.begin", &json!({"stream_id":140,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":token})).await;
        assert_eq!(begun["ok"], true, "a follow-up while the task is outstanding is not session_busy: {begun}");
        let _ = send_request(&mut socket, "follow-stop", "assistant.cancel", &json!({"runtime": token.clone()})).await;
        // The firmware is in WAIT_NEXT after the previous final segment. It drops
        // presentation.segment until the device commits the next turn.
        let early = read_until(&mut socket, Duration::from_secs(3), |_| true).await;
        assert!(early.iter().all(|event| event["event"] != "presentation.segment"), "an idle Mac must not push a segment the device would drop: {early:?}");
        let mut committed = token.clone();
        committed["epoch"] = json!(8);
        committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-cold","method":"assistant.text","payload":{"text":"今天好冷","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment" && event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary)).await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment" && event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary))
            .cloned().unwrap_or_else(|| panic!("the clarification is the first speech of the committed turn: {messages:?}"));
        assert_eq!(segment["runtime"]["turn_id"], 2, "the segment uses the device's new token");
        assert!(brain.tasks.get(&task).unwrap().result.get("clarification_turn_id").is_none(), "unplayed speech is not a delivered clarification");
        let answer = "树叶是绿的，因为里面有叶绿素。";
        let user_turn = format!("{conversation}-2");
        // The harness is not accepting, so this turn never reaches execute.
        // Store the answer the way a completed orchestration turn would.
        brain.memory.journal_input(&crate::memory::JournalInput {
            turn_id: user_turn.clone(), activation_id: conversation.clone(), utterance_id: None,
            text: "今天好冷".into(), occurred_at_ms: crate::time::now_ms() as i64, input_kind: "text".into(),
            speaker_person_id: None, private_profile_id: None, allow_cloud: true, metadata: json!({}),
        }).unwrap();
        brain.memory.journal_finish(&user_turn, "partial", Some(answer), json!({})).unwrap();
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        let recorded = send_request(&mut socket, "lead-report", "presentation.segment.report", &report).await;
        assert_eq!(recorded["ok"], true, "{recorded}");
        let user = brain.memory.household_rpc("household.source", &json!({"conversation_turn_id": user_turn})).unwrap().unwrap();
        assert_eq!(user["status"], "partial");
        assert_eq!(user["assistant_generated"], answer, "the lead must not replace the turn's own answer");
        let played = brain.tasks.get(&task).unwrap();
        let clarification = played.result["clarification_turn_id"].as_str().unwrap();
        assert_ne!(clarification, user_turn);
        assert!(clarification.ends_with("awaiting_input"), "a later success uses a different row: {clarification}");
        let lead = brain.memory.household_rpc("household.source", &json!({"conversation_turn_id": clarification})).unwrap().unwrap();
        assert!(lead["assistant_generated"].as_str().unwrap_or("").contains(summary));
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_live_turn_speaks_the_result_before_the_answer() {
        let (brain, mut socket, conversation, token) = presented_session(true, true).await;
        let summary = "还缺资料的年份";
        let _task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::AwaitingInput);
        let _quiet = read_until(&mut socket, Duration::from_secs(3), |_| false).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8);
        committed["turn_id"] = json!(2);
        let mut device = FirmwareView::committed();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-live","method":"assistant.text","payload":{"text":"树叶为什么是绿的","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        let segments: Vec<_> = messages.iter().filter(|event| event["event"]=="presentation.segment").cloned().collect();
        assert_eq!(segments.len(), 1, "the lead is the only segment; a slow acknowledgement must not play on top: {messages:?}");
        let segment = segments[0].clone();
        assert!(device.accept(&segment), "the first segment must be index 1 seq 1 while COMMITTED: {segment}");
        assert_eq!(segment["payload"]["purpose"], "content");
        assert_eq!(segment["payload"]["final"], false, "a non-final lead leaves the device in COMMITTED for the answer");
        assert!(segment["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary));
        assert!(!device.accept(&json!({"event":"presentation.segment","payload":{"presentation_index":1,"seq":1,"purpose":"content","final":false,"presentation_id":"other","output_lease_id":"other"}})), "a second index-1 segment is rejected");
        let mut follow = segment["payload"].clone();
        follow["presentation_index"] = json!(2);
        follow["seq"] = json!(1);
        follow["purpose"] = json!("content");
        follow["final"] = json!(true);
        follow["presentation_id"] = json!("presentation_answer");
        follow["output_lease_id"] = json!("lease_answer");
        device.report_completed();
        assert!(device.accept(&json!({"event":"presentation.segment","payload":follow})), "after a non-final content segment, index+1 seq 1 is the answer");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_live_turn_does_not_keep_the_lead_busy() {
        let (brain, mut socket, conversation, token) = presented_session(true, true).await;
        let summary = "还缺资料的年份";
        let _task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::AwaitingInput);
        let _quiet = read_until(&mut socket, Duration::from_secs(3), |_| false).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8);
        committed["turn_id"] = json!(2);
        let mut device = FirmwareView::committed();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-fail","method":"assistant.text","payload":{"text":"树叶为什么是绿的","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment").cloned()
            .unwrap_or_else(|| panic!("the live lead is sent before the turn fails: {messages:?}"));
        assert!(device.accept(&segment), "the lead is accepted while COMMITTED: {segment}");
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"lead-report","method":"presentation.segment.report","payload":report}).to_string())).await.unwrap();
        let after = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="session.cancelled").await;
        assert!(after.iter().any(|event| event["id"]=="lead-report" && event["ok"]==true), "the lead receipt is recorded: {after:?}");
        let cancel = after.iter().find(|event| event["event"]=="session.cancelled")
            .unwrap_or_else(|| panic!("a spoken lead makes the failed turn live_answer_interrupted: {after:?}"));
        assert_eq!(cancel["code"], "live_answer_interrupted");
        for event in &after { device.on_event(event); }
        assert!(!device.committed, "session.cancelled takes the device out of COMMITTED");
        let mut later = committed.clone();
        later["epoch"] = json!(9);
        later["turn_id"] = json!(3);
        let begun = send_request(&mut socket, "listen-after-failure", "audio.begin", &json!({"stream_id":143,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":later})).await;
        assert_eq!(begun["ok"], true, "the lead must not stay session_busy after the turn fails: {begun}");
        brain.stop().await.unwrap();
    }

    /// The S31 rules for accepting a presentation segment (brain_secure.c).
    struct FirmwareView {
        committed: bool,
        segment_active: bool,
        presentation_valid: bool,
        index: u64,
        sequence: u64,
        purpose: String,
        presentation_id: String,
        lease: String,
        live: bool,
    }
    impl FirmwareView {
        fn committed() -> Self {
            Self { committed: true, segment_active: false, presentation_valid: false, index: 0, sequence: 0,
                purpose: String::new(), presentation_id: String::new(), lease: String::new(), live: true }
        }
        fn accept(&mut self, event: &Value) -> bool {
            if event["event"] != "presentation.segment" || !self.committed || self.segment_active { return false; }
            let payload = &event["payload"];
            let index = payload["presentation_index"].as_u64().unwrap_or(0);
            let seq = payload["seq"].as_u64().unwrap_or(0);
            let purpose = payload["purpose"].as_str().unwrap_or("");
            let presentation = payload["presentation_id"].as_str().unwrap_or("");
            let lease = payload["output_lease_id"].as_str().unwrap_or("");
            let fresh = !self.presentation_valid && seq == 1 && index == 1;
            let same = self.presentation_valid && index == self.index && seq == self.sequence + 1
                && presentation == self.presentation_id && lease == self.lease;
            let next = self.presentation_valid && seq == 1 && index == self.index + 1
                && (self.purpose == "task_ack" || self.purpose == "progress" || (self.live && self.purpose == "content"))
                && presentation != self.presentation_id && lease != self.lease;
            if !(fresh || same || next) { return false; }
            self.presentation_valid = true;
            self.segment_active = true;
            self.index = index;
            self.sequence = seq;
            self.purpose = purpose.to_owned();
            self.presentation_id = presentation.to_owned();
            self.lease = lease.to_owned();
            true
        }
        fn report_completed(&mut self) {
            if self.segment_active {
                self.segment_active = false;
                if self.purpose == "final" { self.committed = false; }
            }
        }
        /// session.cancelled / session.expired end the device turn (brain_secure.c).
        fn on_event(&mut self, event: &Value) {
            if event["event"] == "session.cancelled" || event["event"] == "session.expired" {
                self.committed = false;
                self.segment_active = false;
            }
        }
    }

    #[tokio::test]
    async fn an_ignored_segment_releases_the_foreground() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let summary = "还缺资料的年份";
        let _task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::AwaitingInput);
        let _quiet = read_until(&mut socket, Duration::from_secs(3), |_| false).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8);
        committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-ignore","method":"assistant.text","payload":{"text":"在吗","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        assert!(messages.iter().any(|event| event["event"]=="presentation.segment"), "the device is committed, so the segment is offered: {messages:?}");
        let mut later = committed.clone();
        later["epoch"] = json!(9);
        later["turn_id"] = json!(3);
        let blocked = send_request(&mut socket, "busy-while-unconfirmed", "audio.begin", &json!({"stream_id":141,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":later})).await;
        assert_eq!(blocked["error"]["code"], "session_busy", "an unconfirmed segment still owns the foreground: {blocked}");
        tokio::time::sleep(segments::segment_report_budget(0, 3_000) + Duration::from_millis(500)).await;
        let released = send_request(&mut socket, "listen-after-timeout", "audio.begin", &json!({"stream_id":142,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":later})).await;
        assert_eq!(released["ok"], true, "a segment the device never confirms must not stay session_busy: {released}");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn switch_off_keeps_the_task_on_the_old_delivery_path() {
        let (brain, mut socket, conversation, token) = presented_session(false, false).await;
        let summary = "还缺资料的年份";
        let _task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::AwaitingInput);
        let household = brain.memory.household_settings().unwrap()["household_id"].as_str().unwrap().to_owned();
        for index in 0..40 {
            let task = brain.tasks.submit(super::super::tasks::NewTask {
                household_id: household.clone(), daily_conversation_id: "day".into(), conversation_turn_id: "turn-off".into(),
                capability: "agent.work".into(), input: json!({"prompt": "回退开关下继续收任务"}),
                intent_key: format!("intent-off-{index}"),
            }, false).unwrap();
            brain.tasks.set_state(&task.task_id, super::super::tasks::TaskState::Succeeded, "done", Some(json!({"summary": format!("结果{index}")}))).unwrap();
        }
        let deadline = Instant::now() + Duration::from_millis(1600);
        while Instant::now() < deadline {
            if let Some(message) = timeout(Duration::from_millis(200), socket.next()).await.ok().and_then(|item| item).and_then(|item| item.ok()) {
                let Message::Text(text) = message else { continue };
                let reply: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(reply["event"], "presentation.segment", "switch off does not speak a task that is not on a delivery: {reply}");
            }
        }
        let begun = send_request(&mut socket, "begin-off", "audio.begin", &json!({"stream_id":141,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":token})).await;
        assert_eq!(begun["ok"], true, "without a delivery, the task record alone is not session_busy: {begun}");
        assert_ne!(brain.device_states.lock().unwrap()["test-bot"]["phase"], "waiting_task");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_question_defers_a_ready_result_and_ten_turns_stay_open() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let summary = "明天有雨。";
        let task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::Succeeded);
        let queued = brain.tasks.submit(super::super::tasks::NewTask {
            household_id: brain.memory.household_settings().unwrap()["household_id"].as_str().unwrap().into(),
            daily_conversation_id: "day".into(), conversation_turn_id: "turn-running".into(),
            capability: "agent.work".into(), input: json!({"prompt": "还在查", "activation_id": conversation}),
            intent_key: format!("intent-running-{}", uuid::Uuid::new_v4().simple()),
        }, false).unwrap();
        for step in 0..10 {
            let turn = 2 + step;
            let mut runtime = token.clone();
            runtime["epoch"] = json!(8 + step); runtime["turn_id"] = json!(turn);
            let begun = send_request(&mut socket, &format!("begin-{turn}"), "audio.begin", &json!({"stream_id":200+turn,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":runtime})).await;
            assert_eq!(begun["ok"], true, "turn {turn} is not session_busy while work is outstanding: {begun}");
            let _ = send_request(&mut socket, &format!("stop-{turn}"), "assistant.cancel", &json!({"runtime": runtime})).await;
        }
        assert_eq!(brain.tasks.get(&queued.task_id).unwrap().state, super::super::tasks::TaskState::Queued);
        let mut committed = token.clone();
        committed["epoch"] = json!(20); committed["turn_id"] = json!(20);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-new","method":"assistant.text","payload":{"text":"树叶为什么是绿的","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(4), |event| event["event"]=="presentation.segment" && event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary)).await;
        assert!(messages.iter().all(|event| !event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary)), "a new question does not play the result: {messages:?}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("deferred"));
        assert!(brain.device_states.lock().unwrap()["test-bot"]["result_hint"].as_bool().unwrap_or(false) || brain.tasks.hint_pending());
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_silent_turn_speaks_the_result_and_records_it_on_the_receipt() {
        let (brain, mut socket, conversation, token) = presented_session(true, true).await;
        let summary = "明天有雨。";
        let task = activation_task(&brain, &conversation, summary, super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        let mut device = FirmwareView::committed();
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"silent","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment" && event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary)).await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment" && event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(summary))
            .cloned().unwrap_or_else(|| panic!("silence speaks the stored result: {messages:?}"));
        assert!(device.accept(&segment), "the result is a segment while COMMITTED: {segment}");
        assert_eq!(segment["payload"]["presentation_index"], 1);
        assert_eq!(segment["payload"]["seq"], 1);
        assert!(brain.tasks.inbox_state(&task).unwrap().as_deref()!=Some("delivered"), "the row is not delivered before the receipt");
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        let recorded = send_request(&mut socket, "result-report", "presentation.segment.report", &report).await;
        assert_eq!(recorded["ok"], true, "{recorded}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("delivered"));
        let row = brain.tasks.inbox_row(&task).unwrap().unwrap();
        assert!(row.receipts.contains("delivered"));
        device.report_completed();
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_task_result_offer_is_dropped_by_the_current_firmware() {
        let (brain, mut socket, conversation, _token) = presented_session_opts(true, false, true).await;
        brain.memory.update_household_settings(&json!({"allow_cloud": true})).unwrap();
        let _task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        // The task was created without the tell-me flag. Set it by rewriting through a told task.
        let household = brain.memory.household_settings().unwrap()["household_id"].as_str().unwrap().to_owned();
        let told = brain.tasks.submit(super::super::tasks::NewTask {
            household_id: household, daily_conversation_id: "day".into(), conversation_turn_id: "turn-told".into(),
            capability: "agent.work".into(), input: json!({"prompt": "查好告诉我", "tell_when_done": true}),
            intent_key: format!("intent-told-{}", uuid::Uuid::new_v4().simple()),
        }, false).unwrap();
        brain.tasks.set_state(&told.task_id, super::super::tasks::TaskState::Succeeded, "done", Some(json!({"summary": "明天有雨。"}))).unwrap();
        let _ = send_request(&mut socket, "close-for-offer", "session.close", &json!({"runtime": _token})).await;
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ready-result","method":"proactive.ready","payload":{}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(25), |event| event["event"]=="proactive.offer" && event["payload"]["moment"]=="task_result").await;
        let offer = messages.iter().find(|event| event["event"]=="proactive.offer" && event["payload"]["moment"]=="task_result")
            .cloned().unwrap_or_else(|| panic!("the Mac offers the result, the board decides: {messages:?}"));
        assert!(!super::super::task_delivery::current_firmware_accepts_moment(offer["payload"]["moment"].as_str().unwrap_or("")));
        assert!(messages.iter().all(|event| event["event"]!="presentation.segment"), "a dropped moment does not start playback");
        let begun = send_request(&mut socket, "no-mic", "audio.begin", &json!({"stream_id":9,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":{"epoch":1,"session_id":1,"turn_id":1,"focus_generation":0}})).await;
        assert_ne!(begun["error"]["code"], "session_busy");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_spoken_offer_returns_the_row_and_the_foreground() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"silent-cancel","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        assert!(messages.iter().any(|event| event["event"]=="presentation.segment"), "the result is offered: {messages:?}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("offered"));
        let _ = send_request(&mut socket, "cancel-offer", "assistant.cancel", &json!({"runtime": committed})).await;
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("pending_delivery"));
        let mut later = committed.clone();
        later["epoch"] = json!(9); later["turn_id"] = json!(3);
        let begun = send_request(&mut socket, "after-cancel", "audio.begin", &json!({"stream_id":151,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":later})).await;
        assert_eq!(begun["ok"], true, "cancel clears the foreground: {begun}");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn closing_the_session_releases_an_offered_row() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"silent-close","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        assert!(messages.iter().any(|event| event["event"]=="presentation.segment"), "{messages:?}");
        let _ = send_request(&mut socket, "close-offer", "session.close", &json!({"runtime": committed})).await;
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("pending_delivery"));
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_releases_an_offered_row() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"silent-drop","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        assert!(messages.iter().any(|event| event["event"]=="presentation.segment"), "{messages:?}");
        drop(socket);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && brain.tasks.inbox_state(&task).unwrap().as_deref() == Some("offered") {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("pending_delivery"));
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_lead_does_not_stay_offered() {
        let (brain, mut socket, conversation, token) = presented_session(true, true).await;
        let task = activation_task(&brain, &conversation, "还缺资料的年份", super::super::tasks::TaskState::AwaitingInput);
        let _quiet = read_until(&mut socket, Duration::from_secs(3), |_| false).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"ask-interrupt","method":"assistant.text","payload":{"text":"树叶为什么是绿的","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment").cloned()
            .unwrap_or_else(|| panic!("the lead is offered before it fails: {messages:?}"));
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("offered"));
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("failed");
        let _ = send_request(&mut socket, "fail-lead", "presentation.segment.report", &report).await;
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("pending_delivery"), "a failed lead goes back to pending");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_yes_speaks_the_rest_of_a_summary() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let full = "资".repeat(120);
        let task = activation_task(&brain, &conversation, &full, super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"summary","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment").cloned()
            .unwrap_or_else(|| panic!("the summary is spoken: {messages:?}"));
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        let recorded = send_request(&mut socket, "summary-report", "presentation.segment.report", &report).await;
        assert_eq!(recorded["ok"], true, "{recorded}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("partial"));
        let mut yes = token.clone();
        yes["epoch"] = json!(9); yes["turn_id"] = json!(3);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"yes-full","method":"assistant.text","payload":{"text":"好","runtime":yes}}).to_string())).await.unwrap();
        let rest = read_until(&mut socket, Duration::from_secs(8), |event| event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(&"资".repeat(80))).await;
        assert!(rest.iter().any(|event| event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains(&"资".repeat(80))), "yes speaks the frozen full text: {rest:?}");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_spoken_question_on_audio_and_live_defers_the_result() {
        let script_dir = std::env::temp_dir().join(format!("xiaodai-asr-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("fake_worker.py");
        std::fs::write(&script, r#"import json,sys
for line in sys.stdin:
    line=line.strip()
    if not line:
        continue
    msg=json.loads(line)
    text="树叶为什么是绿的"
    sys.stdout.write(json.dumps({"id":msg.get("id"),"ok":True,"result":{"text":text,"model_id":"fake","segments":[],"quality":{"duration_ms":500}}})+"\n")
    sys.stdout.flush()
"#).unwrap();
        for live in [false, true] {
            let (brain, mut socket, conversation, token) = presented_session_asr(live, &script, &script_dir).await;
            brain.accepting.store(true, std::sync::atomic::Ordering::Release);
            let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let mut committed = token.clone();
            committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
            let begun = send_request(&mut socket, "begin-q", "audio.begin", &json!({"stream_id":160,"format":"pcm_s16le","sample_rate":16000,"channels":1,"runtime":committed})).await;
            assert_eq!(begun["ok"], true, "{begun}");
            for sequence in 0..3u32 {
                socket.send(Message::Binary(super::super::media::output_frame(160, sequence, &[0u8; 640]).into())).await.unwrap();
            }
            socket.send(Message::Text(json!({"v":3,"type":"request","id":"commit-q","method":"audio.commit","payload":{"stream_id":160,"frames":3,"runtime":committed}}).to_string())).await.unwrap();
            let messages = read_until(&mut socket, Duration::from_secs(12), |event| {
                event["event"]=="presentation.segment" || event["event"]=="session.cancelled" || event["event"]=="session.expired"
                    || event["type"]=="error" || (event["id"]=="commit-q" && event["ok"]==false)
            }).await;
            let heard = brain.memory.household_rpc("household.source", &json!({"conversation_turn_id": format!("{conversation}-2")})).ok().flatten();
            assert!(messages.iter().all(|event| !event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains("明天有雨")), "live={live} a spoken question does not play the result: {messages:?} heard={heard:?}");
            assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("deferred"), "live={live} heard={heard:?} messages={messages:?} configured={}", brain.models.configured());
            brain.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(script_dir);
    }

    #[test]
    fn a_stale_failure_does_not_ask_for_a_release() {
        let token = RuntimeToken { epoch: 1, session_id: 1, turn_id: 1, focus_generation: 0 };
        let event = BackgroundPrep::Failure {
            fence: 1, token, turn_id: "turn".into(), request_id: "req".into(), index: 1,
            activation_id: "activation".into(), error: "turn_failed".into(), spoke: true, result: Ok(None),
        };
        assert!(!failure_releases_current(&event, 2, None, None), "an old fence is dropped and must not release");
        assert_eq!(failure_disposition(true, false, false), FailureDisposition::Drop);
        assert_eq!(failure_disposition(true, false, true), FailureDisposition::Interrupt);
    }

    #[tokio::test]
    async fn a_stale_release_leaves_the_playing_result_delivered_once() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        brain.memory.update_household_settings(&json!({"history_enabled": true})).unwrap();
        let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"play","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment").cloned()
            .unwrap_or_else(|| panic!("the result is playing: {messages:?}"));
        let row = brain.tasks.inbox_row(&task).unwrap().unwrap();
        assert_eq!(row.state, "offered");
        brain.tasks.release_attempt(&row.delivery_id, "lease-stale", "").unwrap();
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("offered"), "a lease this connection does not hold stays claimed");
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        let recorded = send_request(&mut socket, "played", "presentation.segment.report", &report).await;
        assert_eq!(recorded["ok"], true, "{recorded}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("delivered"));
        let lead = format!("{conversation}-lead-{task}-succeeded");
        let source = brain.memory.household_rpc("household.source", &json!({"conversation_turn_id": lead})).unwrap().unwrap();
        assert!(source["assistant_generated"].as_str().unwrap_or("").contains("明天有雨"), "one journal row for the play: {source}");
        let mut again = token.clone();
        again["epoch"] = json!(9); again["turn_id"] = json!(3);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"again","method":"assistant.text","payload":{"text":"","runtime":again}}).to_string())).await.unwrap();
        let later = read_until(&mut socket, Duration::from_secs(3), |event| event["id"]=="again" && event["ok"]==false).await;
        assert!(later.iter().all(|event| !event["payload"]["scene"]["speech"].as_str().unwrap_or("").contains("明天有雨")), "the delivered result is not spoken again: {later:?}");
        brain.stop().await.unwrap();
    }

    #[tokio::test]
    async fn one_connection_exiting_leaves_the_other_claim() {
        let (brain, mut socket, conversation, token) = presented_session(true, false).await;
        let task = activation_task(&brain, &conversation, "明天有雨。", super::super::tasks::TaskState::Succeeded);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut committed = token.clone();
        committed["epoch"] = json!(8); committed["turn_id"] = json!(2);
        socket.send(Message::Text(json!({"v":3,"type":"request","id":"play-b","method":"assistant.text","payload":{"text":"","runtime":committed}}).to_string())).await.unwrap();
        let messages = read_until(&mut socket, Duration::from_secs(8), |event| event["event"]=="presentation.segment").await;
        assert!(messages.iter().any(|event| event["event"]=="presentation.segment"), "{messages:?}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("offered"));
        let mut other = connect_peer(&brain, "test-bot-b").await;
        let opened = send_request(&mut other, "open-b", "session.open", &json!({"activation":"touch","runtime":{"epoch":1,"session_id":3,"turn_id":1,"focus_generation":0},"output":{"version":1,"audio":true,"cards":["text"],"presentation":{"version":1,"frame_ack":true}}})).await;
        assert_eq!(opened["ok"], true, "{opened}");
        drop(other);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let online = brain.gateway.lock().await.as_ref().unwrap().status()["devices"].as_array().map(|rows| rows.len()).unwrap_or(2);
            if online < 2 { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("offered"), "the other connection's claim survives this exit");
        let segment = messages.iter().find(|event| event["event"]=="presentation.segment").cloned().unwrap();
        let mut report = segment["payload"].clone();
        report["runtime"] = segment["runtime"].clone();
        report["status"] = json!("completed");
        let recorded = send_request(&mut socket, "played-b", "presentation.segment.report", &report).await;
        assert_eq!(recorded["ok"], true, "{recorded}");
        assert_eq!(brain.tasks.inbox_state(&task).unwrap().as_deref(), Some("delivered"));
        brain.stop().await.unwrap();
    }

    async fn presented_session(quick: bool, live: bool) -> (Arc<Brain>, tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String, Value) {
        presented_session_opts(quick, live, false).await
    }

    async fn presented_session_opts(quick: bool, live: bool, speak_results: bool) -> (Arc<Brain>, tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String, Value) {
        presented_session_with(quick, live, speak_results, None).await
    }

    async fn presented_session_asr(live: bool, script: &std::path::Path, model_dir: &std::path::Path) -> (Arc<Brain>, tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String, Value) {
        presented_session_with(true, live, false, Some((script.to_path_buf(), model_dir.to_path_buf()))).await
    }

    async fn presented_session_with(quick: bool, live: bool, speak_results: bool, worker: Option<(std::path::PathBuf, std::path::PathBuf)>) -> (Arc<Brain>, tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String, Value) {
        use tokio_tungstenite::{connect_async_tls_with_config, Connector, tungstenite::{client::IntoClientRequest, http::header::{HeaderName, HeaderValue}}};
        let dir = std::env::temp_dir().join(format!("xiaodai-idle-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).unwrap();
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/create_gateway_certificate.py");
        assert!(std::process::Command::new("python3").arg(script).arg("--directory").arg(dir.join("tls")).args(["--ip", "127.0.0.1"]).status().unwrap().success());
        let mut config = super::super::settings::BrainConfig::default();
        config.responsive.quick_decouple = quick;
        config.responsive.speak_task_results = speak_results;
        config.gateway.enabled = true;
        config.gateway.bind = "127.0.0.1:0".into();
        config.gateway.certificate_pem = Some(dir.join("tls/gateway-cert.pem"));
        config.gateway.private_key_pem = Some(dir.join("tls/gateway-key.pem"));
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.harness.base_url = format!("http://{}", occupied.local_addr().unwrap());
        if let Some((script, model_dir)) = worker {
            config.models.script = Some(script);
            config.models.model_dir = Some(model_dir);
            config.brain_core.enabled = false;
        }
        super::super::settings::save(&dir, &config).unwrap();
        let brain = super::super::Brain::new(dir, None, Arc::new(|_| {})).unwrap();
        let issued = brain.credentials.issue("test-bot").unwrap();
        let bearer = issued["token"].as_str().unwrap().to_owned();
        assert!(brain.start().await.unwrap_err().contains("configured port is already in use"));
        let address = brain.gateway.lock().await.as_ref().unwrap().status()["bind"].as_str().unwrap().to_owned();
        let url = format!("wss://{address}{PATH}");
        let cert = CertificateDer::pem_file_iter(dir_of(&brain).join("tls/gateway-ca.pem")).unwrap().next().unwrap().unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(HeaderName::from_static("x-espbot-device"), HeaderValue::from_static("test-bot"));
        request.headers_mut().insert(HeaderName::from_static("authorization"), HeaderValue::from_str(&format!("Bearer {bearer}")).unwrap());
        let (mut socket, _) = connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(Arc::new(tls)))).await.unwrap();
        let _welcome: Value = serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let token = json!({"epoch":7,"session_id":21,"turn_id":1,"focus_generation":0});
        let mut output = json!({"version":1,"audio":true,"cards":["text","image"],"presentation":{"version":1,"frame_ack":true}});
        if live { output["live_turns"] = json!(true); }
        let opened = send_request(&mut socket, "open-presented", "session.open", &json!({"activation":"touch","runtime":token,"output":output})).await;
        assert_eq!(opened["ok"], true, "{opened}");
        let conversation = opened["payload"]["conversation_id"].as_str().unwrap().to_owned();
        (brain, socket, conversation, token)
    }

    fn dir_of(brain: &Brain) -> std::path::PathBuf { brain.config_dir.clone() }

    async fn connect_peer(brain: &Brain, device: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        use tokio_tungstenite::{connect_async_tls_with_config, Connector, tungstenite::{client::IntoClientRequest, http::header::{HeaderName, HeaderValue}}};
        let issued = brain.credentials.issue(device).unwrap();
        let bearer = issued["token"].as_str().unwrap();
        let address = brain.gateway.lock().await.as_ref().unwrap().status()["bind"].as_str().unwrap().to_owned();
        let url = format!("wss://{address}{PATH}");
        let cert = CertificateDer::pem_file_iter(dir_of(brain).join("tls/gateway-ca.pem")).unwrap().next().unwrap().unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(HeaderName::from_static("x-espbot-device"), HeaderValue::from_str(device).unwrap());
        request.headers_mut().insert(HeaderName::from_static("authorization"), HeaderValue::from_str(&format!("Bearer {bearer}")).unwrap());
        let (socket, _) = connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(Arc::new(tls)))).await.unwrap();
        socket
    }

    fn activation_task(brain: &Brain, conversation: &str, summary: &str, state: super::super::tasks::TaskState) -> String {
        let now = crate::time::now_ms() as i64;
        let receipt = brain.memory.journal_input(&crate::memory::JournalInput {
            turn_id: format!("{conversation}-1"), activation_id: conversation.into(), utterance_id: None,
            text: "帮我查一下资料".into(), occurred_at_ms: now, input_kind: "text".into(),
            speaker_person_id: None, private_profile_id: None, allow_cloud: false, metadata: json!({}),
        }).unwrap();
        let household = brain.memory.household_settings().unwrap()["household_id"].as_str().unwrap().to_owned();
        let task = brain.tasks.submit(super::super::tasks::NewTask {
            household_id: household, daily_conversation_id: receipt.daily_conversation_id,
            conversation_turn_id: receipt.conversation_turn_id, capability: "agent.work".into(),
            input: json!({"prompt": "查资料", "activation_id": conversation}), intent_key: format!("intent-{}", uuid::Uuid::new_v4().simple()),
        }, false).unwrap();
        if state != super::super::tasks::TaskState::Queued {
            brain.tasks.set_state(&task.task_id, state, "needs_input", Some(json!({"summary": summary}))).unwrap();
        }
        task.task_id
    }

    async fn send_request(socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, id: &str, method: &str, payload: &Value) -> Value {
        socket.send(Message::Text(json!({"v":3,"type":"request","id":id,"method":method,"payload":payload}).to_string())).await.unwrap();
        loop {
            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else { continue };
            let reply: Value = serde_json::from_str(&text).unwrap();
            if reply["id"].as_str() == Some(id) { return reply; }
        }
    }

    async fn read_until(socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, budget: Duration, done: impl Fn(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + budget;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let message = match timeout(Duration::from_millis(400), socket.next()).await {
                Ok(Some(Ok(message))) => message,
                _ => continue,
            };
            let Message::Text(text) = message else { continue };
            let reply: Value = serde_json::from_str(&text).unwrap();
            let finished = done(&reply);
            seen.push(reply);
            if finished { break; }
        }
        seen
    }
}
