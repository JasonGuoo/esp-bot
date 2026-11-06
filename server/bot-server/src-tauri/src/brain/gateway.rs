//! Authenticated WSS v3 service. Separate from diagnostic, plaintext LAN v1.
//! Transport framing is ours; TLS and WebSocket are the upstream libraries.
use std::{collections::VecDeque, net::SocketAddr, sync::{Arc, RwLock}, time::{Duration, Instant}};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{net::{TcpListener, TcpStream}, sync::{mpsc, watch, Semaphore}, task::{JoinHandle, JoinSet}, time::{interval, timeout}};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_hdr_async_with_config, tungstenite::{handshake::server::{Request, Response, ErrorResponse}, http::StatusCode, protocol::{Message, WebSocketConfig}}};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use super::{Brain, TurnInput, TurnOutput, media};

pub const PATH: &str = "/v3/brain/ws";
const MAX_JSON: usize = 4_500_000;

pub struct RunningGateway {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    state: Arc<RwLock<Value>>,
}

impl RunningGateway {
    pub fn status(&self) -> Value { self.state.read().map(|s|s.clone()).unwrap_or_else(|_|json!({"error":"gateway_state_poisoned"})) }
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
    let state = Arc::new(RwLock::new(json!({"running":true,"bind":local.to_string(),"path":PATH,"tls":true,"protocol":3})));
    let state_task = Arc::clone(&state);
    let capacity = Arc::new(Semaphore::new(config.max_connections.clamp(1,16)));
    let (shutdown, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut peers = JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                accepted = listener.accept() => {
                    let Ok((stream,_peer)) = accepted else {break};
                    let Ok(permit) = Arc::clone(&capacity).try_acquire_owned() else {drop(stream);continue};
                    let acceptor = acceptor.clone(); let brain = Arc::clone(&brain); let shutdown = stopped.clone();
                    peers.spawn(async move {let _permit=permit; let _ = serve_peer(stream,acceptor,brain,shutdown).await;});
                },
                _ = peers.join_next(), if !peers.is_empty() => {},
            }
        }
        if let Ok(mut value) = state_task.write() {value["running"]=json!(false);}
        // Receivers observe the same shutdown generation; let them cancel their turns.
        let _ = timeout(Duration::from_secs(5),async {while peers.join_next().await.is_some() {}}).await;
        peers.abort_all();
    });
    Ok(RunningGateway {shutdown,task:Some(task),state})
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeToken { epoch:u32, session_id:u32, turn_id:u32, focus_generation:u32 }

struct Engagement {
    conversation_id:String,
    token:RuntimeToken,
    last_turn:Option<u32>,
    last_user:Instant,
}

impl Engagement {
    fn check_token(&self, token:RuntimeToken) -> Result<(),String> {
        if token.epoch!=self.token.epoch || token.session_id!=self.token.session_id {return Err("runtime_session_stale".into());}
        if token.turn_id<self.token.turn_id {return Err("runtime_turn_stale".into());}
        if self.last_turn.is_some_and(|last|token.turn_id<=last) {return Err("runtime_turn_stale".into());}
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {v:u8,#[serde(rename="type")]kind:String,id:String,method:String,#[serde(default)]payload:Value}

struct Completed {id:String,turn_id:String,token:RuntimeToken,result:Result<TurnOutput,String>}
struct Playback {turn_id:String,stream_id:u32,token:RuntimeToken,pcm:Vec<u8>,offset:usize,sequence:u32}
struct ActivePeer {turn_id:String,request_id:String,token:RuntimeToken,task:TurnTask}

fn reject(status:StatusCode,reason:&str)->ErrorResponse {
    let mut response=ErrorResponse::new(Some(reason.to_owned())); *response.status_mut()=status; response
}

async fn serve_peer(stream:TcpStream,acceptor:TlsAcceptor,brain:Arc<Brain>,mut shutdown:watch::Receiver<bool>)->Result<(),String> {
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
    let mut cancellations=brain.cancellations();
    send(&mut socket, Message::Text(json!({"v":3,"type":"welcome","device_id":device_id,"capabilities":["assistant.text","audio.pcm16","speaker.candidate","scene.cards"],"sample_rate":16000,"max_audio_seconds":30,"requires_activation":true}).to_string())).await.map_err(|_|"ws_write_failed")?;
    let mut engagement:Option<Engagement>=None;
    let mut audio:Option<(media::AudioInput,RuntimeToken)>=None;
    let mut recent_ids:VecDeque<String>=VecDeque::new();
    let (completion_tx,mut completion_rx)=mpsc::channel::<Completed>(1);
    let mut active:Option<ActivePeer>=None;
    let mut playback:Option<Playback>=None;
    let mut audio_clock=interval(Duration::from_millis(20));
    audio_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat=interval(Duration::from_secs(15)); heartbeat.tick().await;
    let mut last_packet=Instant::now();
    loop {
        tokio::select! {
            _=shutdown.changed()=>break,
            _=revoked.changed()=>{if !brain.credentials.authenticate(&device_id,&credential){break;}},
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
                    audio=None;engagement=None;
                    if send(&mut socket,Message::Text(json!({"v":3,"type":"event","event":"session.cancelled"}).to_string())).await.is_err(){break;}
                }
            },
            _=audio_clock.tick(), if playback.is_some()=>{
                let clip=playback.as_mut().ok_or("playback_missing")?;
                let end=(clip.offset+640).min(clip.pcm.len());
                let frame=media::output_frame(clip.stream_id,clip.sequence,&clip.pcm[clip.offset..end]);
                match timeout(Duration::from_secs(2),send(&mut socket, Message::Binary(frame))).await{
                    Ok(Ok(()))=>{},_=>break,
                }
                clip.offset=end;clip.sequence+=1;
                if clip.offset==clip.pcm.len(){
                    let complete=json!({"v":3,"type":"event","event":"audio.complete","stream_id":clip.stream_id,"runtime":clip.token});
                    playback=None;
                    if send(&mut socket, Message::Text(complete.to_string())).await.is_err(){break;}
                }
            },
            _=heartbeat.tick()=>{
                if last_packet.elapsed()>Duration::from_secs(50){break;}
                if active.is_none() && playback.is_none() && engagement.as_ref().is_some_and(|e|e.last_user.elapsed().as_secs()>brain.config.conversation.idle_session_seconds){
                    engagement=None;audio=None;
                    if send(&mut socket, Message::Text(json!({"v":3,"type":"event","event":"session.expired"}).to_string())).await.is_err(){break;}
                }
                if audio.as_ref().is_some_and(|(a,_)|a.started.elapsed().as_secs()>35){audio=None;}
                if send(&mut socket, Message::Ping(Vec::new())).await.is_err(){break;}
            },
            Some(completed)=completion_rx.recv()=>{
                // A cancel clears active; stale completion is discarded.
                if active.as_ref().is_some_and(|turn|completed.turn_id==turn.turn_id) {
                    active=None;
                    match completed.result {
                        Ok(mut output)=>{
                            let pcm=output.audio.take().and_then(|a|a.get("wav_base64").and_then(Value::as_str).and_then(|wav|media::decode_output(wav).ok()));
                            let stream_id=u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().map_err(|_|"stream_id")?).max(1);
                            let response=json!({"v":3,"type":"response","id":completed.id,"ok":true,"runtime":completed.token,"payload":output,
                                "audio":pcm.as_ref().map(|data|json!({"stream_id":stream_id,"format":"pcm_s16le","sample_rate":16000,"channels":1,"bytes":data.len(),"frame_samples":320}))});
                            if send(&mut socket, Message::Text(response.to_string())).await.is_err(){break;}
                            if let Some(pcm)=pcm {
                                playback=Some(Playback{turn_id:completed.turn_id,stream_id,token:completed.token,pcm,offset:0,sequence:0});
                                audio_clock.reset();
                            }
                        },
                        Err(error)=>{if send(&mut socket, Message::Text(response_error(&completed.id,&error))).await.is_err(){break;}},
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
                        let result=audio.as_mut().ok_or_else(||"audio_session_required".to_string()).and_then(|(audio,_)|audio.push(&bytes));
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
                                    let conversation_id=format!("device-{}-{}",device_id,uuid::Uuid::new_v4().simple());
                                    engagement=Some(Engagement{conversation_id:conversation_id.clone(),token,last_turn:None,last_user:Instant::now()});
                                    Ok(json!({"conversation_id":conversation_id,"runtime":token}))
                                },
                                "assistant.cancel"|"session.close"=>{
                                    if let Some(turn)=active.take(){
                                        let _=brain.cancel(Some(&turn.turn_id)).await;turn.task.abort();
                                        send(&mut socket,Message::Text(cancelled_response(&turn))).await?;
                                    }
                                    audio=None;playback=None;
                                    if frame.method=="session.close"{engagement=None;}
                                    Ok(json!({"cancelled":true}))
                                },
                                "audio.begin"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || audio.is_some() || playback.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    if frame.payload.get("format").and_then(Value::as_str)!=Some("pcm_s16le") || frame.payload.get("sample_rate").and_then(Value::as_u64)!=Some(16000) || frame.payload.get("channels").and_then(Value::as_u64)!=Some(1){return Err("audio_format_unsupported".into());}
                                    let stream=stream_id(&frame.payload)?;audio=Some((media::AudioInput::new(stream),token));session.last_user=Instant::now();
                                    Ok(json!({"stream_id":stream,"max_bytes":media::MAX_INPUT_PCM}))
                                },
                                "assistant.text"|"audio.commit"=>{
                                    let session=engagement.as_mut().ok_or("explicit_session_required")?;
                                    if active.is_some() || playback.is_some(){return Err("session_busy".into());}
                                    let token=parse_runtime(&frame.payload)?;session.check_token(token)?;
                                    let (text,wav)=if frame.method=="assistant.text"{
                                        if audio.is_some(){return Err("audio_capture_in_progress".into());}
                                        (Some(frame.payload.get("text").and_then(Value::as_str).ok_or("text_required")?.to_owned()),None)
                                    }else{
                                        let (capture,capture_token)=audio.take().ok_or("audio_session_required")?;
                                        if token!=capture_token || capture.stream_id!=stream_id(&frame.payload)?{return Err("audio_session_mismatch".into());}
                                        (None,Some(capture.wav_base64()?))
                                    };
                                    session.last_turn=Some(token.turn_id);session.last_user=Instant::now();
                                    let id=format!("{}-{}",session.conversation_id,token.turn_id);
                                    let input=TurnInput{turn_id:id.clone(),conversation_id:session.conversation_id.clone(),text,wav_base64:wav,profile_id:None,speak:Some(true)};
                                    let brain=Arc::clone(&brain);let tx=completion_tx.clone();let request_id=frame.id.clone();
                                    let completion_id=id.clone();
                                    let task=tokio::spawn(async move{let result=brain.turn(input,false).await;let _=tx.send(Completed{id:request_id,turn_id:completion_id,token,result}).await;});
                                    active=Some(ActivePeer{turn_id:id,request_id:frame.id.clone(),token,task:TurnTask(task)});pending=true;Ok(json!({}))
                                },
                                _=>Err("unsupported_device_method".into()),
                            }
                        }.await;
                        if !pending {
                            let response=match result{Ok(payload)=>json!({"v":3,"type":"response","id":frame.id,"ok":true,"payload":payload}).to_string(),Err(error)=>response_error(&frame.id,&error)};
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

fn parse_runtime(payload:&Value)->Result<RuntimeToken,String>{serde_json::from_value(payload.get("runtime").cloned().ok_or("runtime_token_required")?).map_err(|_|"runtime_token_invalid".into())}
fn stream_id(payload:&Value)->Result<u32,String>{payload.get("stream_id").and_then(Value::as_u64).and_then(|v|u32::try_from(v).ok()).filter(|v|*v>0).ok_or("stream_id_invalid".into())}
fn response_error(id:&str,error:&str)->String{json!({"v":3,"type":"response","id":id,"ok":false,"error":{"code":error.split(':').next().unwrap_or("request_failed"),"message":error.chars().take(500).collect::<String>()}}).to_string()}
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
