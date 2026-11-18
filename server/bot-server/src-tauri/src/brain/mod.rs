//! Mac composition: explicit turn -> local ASR -> OpenCode -> scene/local TTS.
//! The ESP runtime remains the only authority for wake, focus and actuators.
pub mod settings;
pub mod worker;
pub mod scene;
pub mod credentials;
pub mod media;
mod plain_audio;
pub mod gateway;
mod discovery;
mod device_volume;
mod display;
mod memory_worker;
mod rpc;
mod timing;
pub mod contracts;
pub mod tasks;
pub mod presentation;
pub mod artifacts;
mod capabilities;
mod orchestration;
mod acceptance;

use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex as StdMutex, RwLock, atomic::{AtomicBool, Ordering}}, time::Instant};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex, RwLock as AsyncRwLock};
use crate::{harness::{Harness, TurnRequest}, memory::{self, Store}};
use settings::BrainConfig;
use worker::ModelWorker;
pub use timing::TurnTimings;

pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
pub type DiagnosticSnapshot = Arc<dyn Fn() -> Value + Send + Sync>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInput {
    pub turn_id: String,
    pub conversation_id: String,
    #[serde(default)] pub text: Option<String>,
    #[serde(default)] pub wav_base64: Option<String>,
    /// Only trusted console callers may set a profile. Recognition never sets it.
    #[serde(default)] pub profile_id: Option<String>,
    #[serde(default)] pub speak: Option<bool>,
    /// Console attribution is an observation; private scope remains explicit above.
    #[serde(default)] pub speaker_person_id: Option<String>,
    #[serde(default)] pub utterance_id: Option<String>,
    #[serde(skip)] pub(crate) device_display: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurnOutput {
    pub turn_id: String,
    pub conversation_id: String,
    pub transcript: String,
    pub scene: scene::Scene,
    pub provider_id: String,
    pub model_id: String,
    pub harness_session_id: String,
    pub audio: Option<Value>,
    pub identity_candidate: Option<Value>,
    pub warnings: Vec<String>,
    pub timings: TurnTimings,
    pub journal: memory::JournalReceipt,
    pub presentation: presentation::PresentationPlan,
    pub task_ids: Vec<String>,
    #[serde(skip)] pub(crate) prepared_segments: Vec<orchestration::PreparedSegment>,
    #[serde(skip)] pub(crate) tts_calls: u8,
    #[serde(skip)] pub(crate) display_image: Option<display::PreparedImage>,
    #[serde(skip)] pub(crate) device_outcome: String,
}

struct ActiveTurn { id: String, conversation_id: String, cancelled: watch::Sender<bool> }

#[derive(Clone, Debug)]
pub(crate) enum CancelScope {
    Turn(String),
    DeviceSession(String), // authenticated gateway conversation ID
    All,
}

#[derive(Clone, Debug)]
pub(crate) struct Cancellation { pub scope: CancelScope, pub reason: &'static str }

impl Cancellation {
    pub fn matches_turn(&self, turn_id: &str, conversation_id: Option<&str>) -> bool {
        match &self.scope {
            CancelScope::Turn(id) => id == turn_id,
            CancelScope::DeviceSession(id) => conversation_id == Some(id.as_str()),
            CancelScope::All => true,
        }
    }

    pub fn closes_session(&self, conversation_id: Option<&str>) -> bool {
        match &self.scope {
            CancelScope::Turn(_) => false,
            CancelScope::DeviceSession(id) => conversation_id == Some(id.as_str()),
            CancelScope::All => true,
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::{CancelScope,Cancellation};
    #[test]
    fn console_cancel_cannot_close_device_session() {
        let turn=Cancellation{scope:CancelScope::Turn("console-turn".into()),reason:"parent_stop"};
        assert!(turn.matches_turn("console-turn",Some("console-conversation")));
        assert!(!turn.matches_turn("device-turn",Some("device-conversation")));
        assert!(!turn.closes_session(Some("device-conversation")));
        let device=Cancellation{scope:CancelScope::DeviceSession("device-one".into()),reason:"parent_stop"};
        assert!(device.matches_turn("any",Some("device-one")));
        assert!(device.closes_session(Some("device-one")));
        assert!(!device.matches_turn("console-turn",Some("console-conversation")));
        assert!(!device.closes_session(Some("device-two")));
        let all=Cancellation{scope:CancelScope::All,reason:"memory_changed"};
        assert!(all.matches_turn("any",None) && all.closes_session(None));
    }
}

struct TurnLease<'a> { brain: &'a Brain, id: String, completed: bool }
impl Drop for TurnLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.brain.active.lock() {
            active.remove(&self.id);
        }
        if !self.completed {
            let _ = self.brain.memory.journal_finish(&self.id, "cancelled", None, json!({"reason":"request_dropped"}));
            let harness=Arc::clone(&self.brain.harness); let id=self.id.clone();
            tauri::async_runtime::spawn(async move {let _=harness.cancel(&id).await;});
        }
    }
}

pub struct Brain {
    pub config: BrainConfig,
    pub config_dir: PathBuf,
    pub harness: Arc<Harness>,
    pub models: Arc<ModelWorker>,
    pub memory: Arc<Store>,
    pub credentials: Arc<credentials::Credentials>,
    sink: EventSink,
    turn_gate: AsyncRwLock<()>,
    active: StdMutex<HashMap<String, ActiveTurn>>,
    identity_jobs: StdMutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    pub tasks: Arc<tasks::TaskService>,
    device_states: StdMutex<HashMap<String,Value>>,
    device_volumes: device_volume::DeviceVolumes,
    progress_audio: Mutex<HashMap<String,Value>>,
    pub(super) graphiti: Arc<memory::graphiti::GraphitiClient>,
    accepting: AtomicBool,
    cancellation_events: broadcast::Sender<Cancellation>,
    progress_events: broadcast::Sender<Value>,
    foreground_generation: watch::Sender<u64>,
    background: Mutex<Option<tokio::task::JoinHandle<()>>>,
    memory_script: Option<PathBuf>,
    gateway: Mutex<Option<gateway::RunningGateway>>,
}

impl Brain {
    #[cfg(test)]
    fn new(config_dir: PathBuf, worker_resource: Option<PathBuf>, sink: EventSink) -> Result<Arc<Self>, String> {
        Self::with_keys(config_dir, worker_resource, sink, Arc::default())
    }

    fn with_keys(config_dir: PathBuf, worker_resource: Option<PathBuf>, sink: EventSink, keys: Arc<crate::harness::secrets::SessionKeys>) -> Result<Arc<Self>, String> {
        let memory_script = worker_resource.as_ref().and_then(|p|p.parent()).map(|p|p.join("memory_extract.py")).filter(|p|p.is_file());
        let graphiti_script=worker_resource.as_ref().and_then(|p|p.parent()).map(|p|p.join("graphiti_worker.py")).unwrap_or_else(||config_dir.join("engines/local-models/graphiti_worker.py"));
        let config = settings::load_or_create(&config_dir, worker_resource)?;
        let runtime_dir = config_dir.join("runtime");
        settings::private_dir(&runtime_dir)?;
        let (progress_events, _) = broadcast::channel(64);
        let progress = progress_events.clone();
        let sink: EventSink = Arc::new(move |event| {
            let kind = event["kind"].as_str().unwrap_or("");
            if matches!(kind, "turn.transcribing" | "turn.understood" | "turn.thinking"
                | "turn.preparing_image" | "turn.synthesizing" | "task.accepted" | "task.progress" | "task.state" | "harness_tool_activity" | "harness_permission_required"
                | "harness_permission_replied" | "harness_question_required") {
                let _ = progress.send(event.clone());
            }
            sink(event);
        });
        let harness = Harness::new(config.harness.clone(), runtime_dir.join("opencode"), config_dir.clone(), Arc::clone(&sink), keys)?;
        let memory = Store::open(runtime_dir.join("memory"), config.memory.clone())?;
        let credentials = credentials::Credentials::open(runtime_dir.join("devices"))?;
        let (cancellation_events, _) = broadcast::channel(64);
        let (foreground_generation, _) = watch::channel(0);
        let tasks = tasks::TaskService::open(runtime_dir.join("tasks"), config.tasks.clone(), Arc::clone(&sink))?;
        let graphiti=Arc::new(memory::graphiti::GraphitiClient::new(&config.memory.graphiti,graphiti_script));
        let brain = Arc::new(Self { models: ModelWorker::new(config.models.clone(), config_dir.clone()), config, config_dir,
            harness, memory, credentials, sink, turn_gate:AsyncRwLock::new(()), active:StdMutex::new(HashMap::new()), identity_jobs:StdMutex::new(HashMap::new()), tasks, device_states:StdMutex::new(HashMap::new()), device_volumes:Default::default(), progress_audio:Mutex::new(HashMap::new()), graphiti, accepting:AtomicBool::new(false),
            cancellation_events, progress_events, foreground_generation, background:Mutex::new(None), memory_script, gateway:Mutex::new(None) });
        brain.install_task_handlers()?;
        Ok(brain)
    }

    pub async fn start(self: &Arc<Self>) -> Result<Value, String> {
        {
            let mut gateway = self.gateway.lock().await;
            if self.config.gateway.enabled && gateway.is_none() {
                *gateway = Some(gateway::start(Arc::clone(self)).await?);
            }
        }
        // A paired device can reconnect while the local dialogue engine starts
        // or is unavailable. Explicit service.stop still closes the listener.
        let harness = match self.harness.start().await {
            Ok(harness) => harness,
            Err(error) => { self.accepting.store(false, Ordering::Release); return Err(error); }
        };
        self.tasks.start().await?;
        self.accepting.store(true,Ordering::Release);
        if self.memory.retry_failed_memory_jobs().is_err() {
            self.emit("memory.retry_unavailable", None, json!({}));
        }
        let mut background=self.background.lock().await;
        if background.as_ref().is_none_or(|task|task.is_finished()) {
            *background=Some(tokio::spawn(memory_worker::run(Arc::downgrade(self))));
        }
        self.emit("service.started", None, json!({}));
        Ok(json!({"harness":harness,"gateway_enabled":self.config.gateway.enabled}))
    }

    pub async fn stop(&self) -> Result<(), String> {
        self.accepting.store(false,Ordering::Release);
        if let Some(task)=self.background.lock().await.take() {task.abort();}
        let cancellation = self.cancel(None).await;
        let _turn = self.turn_gate.write().await;
        if let Some(gateway) = self.gateway.lock().await.take() { gateway.stop().await; }
        self.graphiti.stop().await;
        self.tasks.stop().await;
        self.models.stop().await;
        let stopped = self.harness.stop().await;
        self.emit("service.stopped", None, json!({}));
        stopped.and(cancellation)
    }

    pub async fn status(&self) -> Value {
        let active = self.active.lock().map(|v|v.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let gateway = self.gateway.lock().await.as_ref().map(|g|g.status());
        let memory_model=self.config_dir.join("models/memory-qwen3-4b");
        let memory_ready=self.memory_script.as_ref().is_some_and(|p|p.is_file()) &&
            ["config.json","model.safetensors","chat_template.jinja"].iter().all(|name|memory_model.join(name).is_file());
        let task_states=self.tasks.list(None,None,50).unwrap_or_default().into_iter().map(|task|json!({"task_id":task.task_id,"capability":task.capability,"state":task.state,"stage":task.stage})).collect::<Vec<_>>();
        json!({"ready":true,"accepting_turns":self.accepting.load(Ordering::Acquire),"active_turn":active.first(),"active_turns":active,"tasks":task_states,"contract":contracts::capabilities(),"harness":self.harness.status().await,
            "models_configured":self.models.configured(),"memory":self.memory.status().unwrap_or_else(|e|json!({"error":e})),
            "background_memory_model_ready":memory_ready,
            "device_activity":self.device_states.lock().map(|s|s.values().cloned().collect::<Vec<_>>()).unwrap_or_default(),
            "device_volumes":self.device_volumes.status(),
            "gateway":gateway,"gateway_enabled":self.config.gateway.enabled,
            "config_path":self.config_dir.join("brain.json"),"legacy_lan_is_authenticated":false,
            "device_media_requires_v3":true})
    }

    pub(crate) fn device_activity(&self, device_id:&str, mut state:Value) {
        state["device_id"]=json!(device_id);
        let changed=if let Ok(mut states)=self.device_states.lock(){
            if states.get(device_id)==Some(&state){false}else{states.insert(device_id.into(),state.clone());true}
        }else{false};
        if changed{self.emit("device.activity",state["turn_id"].as_str(),state.clone());}
    }

    pub fn emit(&self, kind: &str, turn_id: Option<&str>, payload: Value) {
        (self.sink)(json!({"kind":kind,"turn_id":turn_id,"at_ms":crate::time::now_ms(),"payload":payload}));
    }

    pub async fn cancel(&self, turn_id: Option<&str>) -> Result<(), String> {
        self.cancel_scoped(turn_id.map(|id|CancelScope::Turn(id.to_owned())).unwrap_or(CancelScope::All),
            if turn_id.is_some() {"turn_cancelled"} else {"service_stopped"}).await
    }

    pub(crate) async fn cancel_scoped(&self, scope: CancelScope, reason: &'static str) -> Result<(), String> {
        let cancellation = Cancellation {scope, reason};
        // Audio delivery can outlive inference; its owner must see Stop too.
        let _ = self.cancellation_events.send(cancellation.clone());
        let ids = {
            let active = self.active.lock().map_err(|_|"brain_state_poisoned")?;
            active.values().filter(|a|cancellation.matches_turn(&a.id,Some(&a.conversation_id)))
                .map(|a|{a.cancelled.send_replace(true); a.id.clone()}).collect::<Vec<_>>()
        };
        if matches!(cancellation.scope, CancelScope::All) {
            self.foreground_generation.send_modify(|value|*value=value.wrapping_add(1));
            // The Stop-interaction button does not cancel durable task TTS or
            // media preparation sharing these lanes. Dropping the cancelled
            // foreground future already kills only its own worker exchange.
            if reason!="parent_stop"{self.models.cancel_current();}
            self.graphiti.stop().await;
            let jobs=self.identity_jobs.lock().map(|mut jobs|jobs.drain().map(|(_,job)|job).collect::<Vec<_>>()).unwrap_or_default();
            for job in &jobs{job.abort();}
            // Abort is cooperative. A source edit must not race a last
            // synchronous identity write after its revision preflight.
            for job in jobs{let _=job.await;}
            self.tasks.revoke_outputs(None)?;
        } else if let CancelScope::DeviceSession(id)=&cancellation.scope { self.tasks.revoke_outputs(Some(id))?; }
        for id in ids {
            let result=self.harness.cancel(&id).await;
            self.emit("turn.cancelled",Some(&id),json!({"code":reason}));
            result?;
        }
        Ok(())
    }

    pub(crate) fn cancellations(&self) -> broadcast::Receiver<Cancellation> { self.cancellation_events.subscribe() }

    fn progress(&self) -> broadcast::Receiver<Value> { self.progress_events.subscribe() }

    pub fn foreground_busy(&self) -> bool { self.active.lock().map(|a|!a.is_empty()).unwrap_or(true) }

    pub async fn turn(self: &Arc<Self>, input: TurnInput, trusted_console: bool) -> Result<TurnOutput, String> {
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_not_started: start the service from the console".into()); }
        if !valid_turn_id(&input.turn_id) || !valid_turn_id(&input.conversation_id) { return Err("turn_or_conversation_id_invalid".into()); }
        if !trusted_console && input.profile_id.is_some() { return Err("profile_requires_local_console".into()); }
        if input.text.is_some() == input.wav_base64.is_some() { return Err("provide_exactly_one_text_or_audio_input".into()); }
        if input.text.as_ref().is_some_and(|v|v.trim().is_empty() || v.chars().count() > 4000) { return Err("text_must_be_1_to_4000_characters".into()); }
        if input.wav_base64.as_ref().is_some_and(|v|v.len() > 1_300_000) { return Err("audio_input_limit_30_seconds".into()); }
        let _gate = self.turn_gate.read().await;
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_stopping".into()); }
        self.foreground_generation.send_modify(|value|*value=value.wrapping_add(1));
        let (cancelled, mut cancellation) = watch::channel(false);
        {
            let mut active=self.active.lock().map_err(|_|"brain_state_poisoned")?;
            if active.len()>=8 {return Err("brain_capacity_exceeded".into());}
            if active.contains_key(&input.turn_id) {return Err("duplicate_turn".into());}
            if active.values().any(|a|a.conversation_id==input.conversation_id) {return Err("activation_busy".into());}
            active.insert(input.turn_id.clone(),ActiveTurn {id:input.turn_id.clone(),conversation_id:input.conversation_id.clone(),cancelled});
        }
        let mut lease = TurnLease {brain:self,id:input.turn_id.clone(),completed:false};
        let started = Instant::now();
        self.emit("turn.started", Some(&input.turn_id), json!({"input":if input.text.is_some(){"text"}else{"audio"}}));
        let result = tokio::select! {
            biased;
            _ = cancellation.changed() => Err("cancelled".into()),
            result = self.execute(&input, trusted_console) => result,
        };
        let result={
            // cancel_scoped sends while holding this same membership lock.
            // Checking and removing together chooses one completion boundary.
            let mut active=self.active.lock().map_err(|_|"brain_state_poisoned")?;
            let cancelled=*cancellation.borrow();
            active.remove(&input.turn_id);
            if cancelled{Err("cancelled".into())}else{result}
        };
        if let Err(error)=&result {let _=self.memory.journal_finish(&input.turn_id,if error.contains("cancelled"){"cancelled"}else{"failed"},None,json!({"error":error.split(':').next()}));}
        lease.completed=true;
        self.emit(if result.is_ok(){"turn.completed"}else{"turn.failed"}, Some(&input.turn_id),
            json!({"error":result.as_ref().err().map(|e|e.split(':').next().unwrap_or("turn_failed")),
                "elapsed_ms":started.elapsed().as_millis() as u64,
                "timings":result.as_ref().ok().map(|output|&output.timings)}));
        result
    }

    /// Return a local candidate. Never mutate current profile or send embeddings to OpenCode.
    pub async fn identify_media(&self, kind: memory::IdentityKind, payload: Value) -> Result<Value, String> {
        let operation = if kind == memory::IdentityKind::Face {"face_embed"} else {"speaker_embed"};
        let result = self.models.call(operation, payload).await?;
        let request = identity_request(kind, &result)?;
        let memory = Arc::clone(&self.memory);
        let candidate = tokio::task::spawn_blocking(move ||memory.identify(request)).await.map_err(|_|"memory_task_failed")??;
        serde_json::to_value(candidate).map_err(|e|e.to_string())
    }

    /// Local parent console submits distinct media samples; embeddings never traverse IPC back to UI.
    pub async fn enroll_media(&self, request: EnrollmentMedia) -> Result<Value, String> {
        let _gate = self.turn_gate.try_write().map_err(|_|"brain_busy: finish the current turn before enrollment")?;
        if !request.guardian_confirmed { return Err("enrollment_requires_guardian_confirmation".into()); }
        if request.samples.len() < self.config.memory.min_enrollment_samples || request.samples.len() > self.config.memory.max_enrollment_samples {
            return Err("enrollment_sample_count_invalid".into());
        }
        let generation = self.models.generation();
        let mut samples = Vec::new(); let mut model_id = None;
        for sample in request.samples {
            let (op,payload) = if request.kind == memory::IdentityKind::Face {("face_embed",json!({"image_base64":sample}))} else {("speaker_embed",json!({"wav_base64":sample}))};
            let value = self.models.call_at_generation(op,payload,generation).await?;
            let embedding = identity_request(request.kind,&value)?;
            if model_id.as_ref().is_some_and(|id|id != &embedding.model_id) { return Err("enrollment_model_changed".into()); }
            model_id = Some(embedding.model_id);
            samples.push(memory::EmbeddingSample { embedding:embedding.embedding, quality_ok:embedding.quality_ok });
        }
        if self.models.generation()!=generation { return Err("cancelled".into()); }
        let enrollment = memory::EnrollmentRequest { profile_id:request.profile_id, kind:request.kind,
            model_id:model_id.ok_or("enrollment_missing_model")?, samples, guardian_confirmed:true, replace_existing:request.replace_existing };
        // The final enrollment write shares the gate with revocation; do not
        // detach it into a task which could outlive this operation.
        let result = self.memory.enroll(enrollment)?;
        serde_json::to_value(result).map_err(|e|e.to_string())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentMedia {
    profile_id: String,
    kind: memory::IdentityKind,
    samples: Vec<String>,
    guardian_confirmed: bool,
    #[serde(default)] replace_existing: bool,
}

fn identity_request(kind: memory::IdentityKind, result: &Value) -> Result<memory::IdentityRequest, String> {
    Ok(memory::IdentityRequest { kind,
        model_id:result.get("model_id").and_then(Value::as_str).ok_or("model_identity_missing")?.to_owned(),
        embedding:serde_json::from_value(result.get("embedding").cloned().ok_or("model_embedding_missing")?).map_err(|_|"model_embedding_invalid")?,
        quality_ok:result.get("quality_ok").and_then(Value::as_bool).unwrap_or(false) })
}

fn valid_turn_id(id: &str) -> bool { !id.is_empty() && id.len() <= 128 && id.bytes().all(|c|c.is_ascii_alphanumeric() || b"-_".contains(&c)) }

/// Reloadable host remains present even when a malformed config prevents startup.
pub struct BrainHost {
    keys: Arc<crate::harness::secrets::SessionKeys>,
    pub config_dir: PathBuf,
    resource: Option<PathBuf>,
    sink: EventSink,
    diagnostics: Option<DiagnosticSnapshot>,
    inner: RwLock<Result<Arc<Brain>,String>>,
    lifecycle: Mutex<()>,
    auto_start_pending: AtomicBool,
}

impl BrainHost {
    pub fn new(config_dir: PathBuf, resource: Option<PathBuf>, sink: EventSink, diagnostics: Option<DiagnosticSnapshot>) -> Arc<Self> {
        let keys = Arc::default();
        let inner = Brain::with_keys(config_dir.clone(), resource.clone(), Arc::clone(&sink), Arc::clone(&keys));
        Arc::new(Self {config_dir, resource, sink, keys, diagnostics, inner:RwLock::new(inner), lifecycle:Mutex::new(()),
            auto_start_pending:AtomicBool::new(true)})
    }

    pub fn get(&self) -> Result<Arc<Brain>, String> { self.inner.read().map_err(|_|"brain_host_poisoned".to_owned())?.clone() }

    fn suppress_auto_start(&self) { self.auto_start_pending.store(false, Ordering::Release); }

    pub async fn auto_start(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        // An explicit lifecycle action wins even when this startup task has
        // not been polled yet. Resolve the current runtime only under the gate.
        if !self.auto_start_pending.swap(false, Ordering::AcqRel) { return; }
        let Ok(brain) = self.get() else { return; };
        if !brain.config.auto_start { return; }
        if let Err(error) = brain.start().await {
            brain.emit("service.start_failed", None, json!({"error":error}));
        }
    }

    pub async fn start(&self) -> Result<Value, String> {
        self.suppress_auto_start();
        let _lifecycle = self.lifecycle.lock().await;
        self.get()?.start().await
    }

    pub async fn stop(&self) -> Result<(), String> {
        self.suppress_auto_start();
        let _lifecycle = self.lifecycle.lock().await;
        self.get()?.stop().await
    }

    pub async fn reload(&self) -> Result<Value, String> {
        self.suppress_auto_start();
        let _lifecycle = self.lifecycle.lock().await;
        if let Ok(brain) = self.get() { brain.stop().await?; }
        let next = Brain::with_keys(self.config_dir.clone(), self.resource.clone(), Arc::clone(&self.sink), Arc::clone(&self.keys));
        *self.inner.write().map_err(|_|"brain_host_poisoned")? = next.clone();
        let brain = next?;
        Ok(brain.status().await)
    }

    pub async fn shutdown(&self) { let _ = self.stop().await; }
}

#[tauri::command]
pub async fn companion_request(host: tauri::State<'_, Arc<BrainHost>>, method: String, payload: Option<Value>) -> Result<Value,String> {
    host.dispatch(&method,payload.unwrap_or_else(||json!({}))).await
}

#[cfg(test)]
mod key_lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn key_survives_config_reload_but_not_an_app_restart() {
        let dir = std::env::temp_dir().join(format!("xiaodai-session-test-{}", uuid::Uuid::new_v4()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let open = || BrainHost::new(dir.clone(), None, Arc::new(|_|{}), None);
        let host = open();
        let key = uuid::Uuid::new_v4().simple().to_string();
        let payload = json!({"route_id":"glm-coding-plan", "api_key":key});
        host.dispatch("api_key.save",payload.clone()).await.unwrap();
        let status = host.reload().await.unwrap();
        assert_eq!(status["harness"]["routes"][0]["credential_source"],"session");
        assert!(!status.to_string().contains(&key));
        let config = host.dispatch("config.read",json!({})).await.unwrap();
        assert!(!config.to_string().contains(&key));
        let brain = host.get().unwrap();
        let turn = brain.turn_gate.read().await;
        assert_eq!(host.dispatch("api_key.delete",json!({"route_id":"glm-coding-plan"})).await.unwrap_err(),"api_key_turn_in_progress");
        drop(turn);
        host.dispatch("api_key.delete",json!({"route_id":"glm-coding-plan"})).await.unwrap();
        assert_eq!(brain.harness.status().await["routes"][0]["session_key_set"],false);
        host.dispatch("api_key.save",payload).await.unwrap();
        drop(brain); drop(host);
        assert_eq!(open().get().unwrap().harness.status().await["routes"][0]["session_key_set"],false);
    }
}
