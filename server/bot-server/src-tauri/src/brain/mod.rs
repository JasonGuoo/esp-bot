//! Mac composition: explicit turn -> local ASR -> OpenCode -> scene/local TTS.
//! The ESP runtime remains the only authority for wake, focus and actuators.
pub mod settings;
pub mod worker;
pub mod scene;
pub mod credentials;
pub mod media;
pub mod gateway;
mod rpc;

use std::{path::PathBuf, sync::{Arc, Mutex as StdMutex, RwLock, atomic::{AtomicBool, Ordering}}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex};
use crate::{harness::{Harness, TurnRequest}, memory::{self, Store}};
use settings::BrainConfig;
use worker::ModelWorker;

pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;

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
}

struct ActiveTurn { id: String, cancelled: watch::Sender<bool> }

struct TurnLease<'a> { brain: &'a Brain, id: String, completed: bool }
impl Drop for TurnLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.brain.active.lock() {
            if active.as_ref().is_some_and(|turn|turn.id==self.id) { *active=None; }
        }
        if !self.completed {
            self.brain.models.cancel_current();
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
    turn_gate: Mutex<()>,
    active: StdMutex<Option<ActiveTurn>>,
    accepting: AtomicBool,
    cancellation_events: broadcast::Sender<Option<String>>,
    gateway: Mutex<Option<gateway::RunningGateway>>,
}

impl Brain {
    fn new(config_dir: PathBuf, worker_resource: Option<PathBuf>, sink: EventSink) -> Result<Arc<Self>, String> {
        let config = settings::load_or_create(&config_dir, worker_resource)?;
        let runtime_dir = config_dir.join("runtime");
        settings::private_dir(&runtime_dir)?;
        let harness = Harness::new(config.harness.clone(), runtime_dir.join("opencode"), Arc::clone(&sink))?;
        let memory = Store::open(runtime_dir.join("memory"), config.memory.clone())?;
        let credentials = credentials::Credentials::open(runtime_dir.join("devices"))?;
        let (cancellation_events, _) = broadcast::channel(64);
        Ok(Arc::new(Self { models: ModelWorker::new(config.models.clone()), config, config_dir,
            harness, memory, credentials, sink, turn_gate:Mutex::new(()), active:StdMutex::new(None), accepting:AtomicBool::new(false), cancellation_events, gateway:Mutex::new(None) }))
    }

    pub async fn start(self: &Arc<Self>) -> Result<Value, String> {
        let harness = self.harness.start().await?;
        let mut gateway = self.gateway.lock().await;
        if self.config.gateway.enabled && gateway.is_none() {
            *gateway = Some(gateway::start(Arc::clone(self)).await?);
        }
        self.accepting.store(true,Ordering::Release);
        self.emit("service.started", None, json!({}));
        Ok(json!({"harness":harness,"gateway_enabled":self.config.gateway.enabled}))
    }

    pub async fn stop(&self) -> Result<(), String> {
        self.accepting.store(false,Ordering::Release);
        let cancellation = self.cancel(None).await;
        let _turn = self.turn_gate.lock().await;
        if let Some(gateway) = self.gateway.lock().await.take() { gateway.stop().await; }
        self.models.stop().await;
        let stopped = self.harness.stop().await;
        self.emit("service.stopped", None, json!({}));
        stopped.and(cancellation)
    }

    pub async fn status(&self) -> Value {
        let active = self.active.lock().ok().and_then(|v|v.as_ref().map(|a|a.id.clone()));
        let gateway = self.gateway.lock().await.as_ref().map(|g|g.status());
        json!({"ready":true,"accepting_turns":self.accepting.load(Ordering::Acquire),"active_turn":active,"harness":self.harness.status().await,
            "models_configured":self.models.configured(),"memory":self.memory.status().unwrap_or_else(|e|json!({"error":e})),
            "gateway":gateway,"gateway_enabled":self.config.gateway.enabled,
            "config_path":self.config_dir.join("brain.json"),"legacy_lan_is_authenticated":false,
            "device_media_requires_v3":true})
    }

    pub fn emit(&self, kind: &str, turn_id: Option<&str>, payload: Value) {
        (self.sink)(json!({"kind":kind,"turn_id":turn_id,"at_ms":crate::lan::now_ms(),"payload":payload}));
    }

    pub async fn cancel(&self, turn_id: Option<&str>) -> Result<(), String> {
        // Audio delivery can outlive inference; its owner must see Stop too.
        let _ = self.cancellation_events.send(turn_id.map(str::to_owned));
        let id = {
            let active = self.active.lock().map_err(|_|"brain_state_poisoned")?;
            if let Some(active) = active.as_ref().filter(|a|turn_id.is_none() || turn_id == Some(a.id.as_str())) {
                active.cancelled.send_replace(true);
                Some(active.id.clone())
            } else { None }
        };
        if turn_id.is_none() { self.models.cancel_current(); }
        if let Some(id) = id {
            self.models.cancel_current();
            let result = self.harness.cancel(&id).await;
            self.emit("turn.cancelled", Some(&id), json!({}));
            result?;
        }
        Ok(())
    }

    pub fn cancellations(&self) -> broadcast::Receiver<Option<String>> { self.cancellation_events.subscribe() }

    pub async fn turn(self: &Arc<Self>, input: TurnInput, trusted_console: bool) -> Result<TurnOutput, String> {
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_not_started: start the service from the console".into()); }
        if !valid_turn_id(&input.turn_id) || !valid_turn_id(&input.conversation_id) { return Err("turn_or_conversation_id_invalid".into()); }
        if !trusted_console && input.profile_id.is_some() { return Err("profile_requires_local_console".into()); }
        if input.text.is_some() == input.wav_base64.is_some() { return Err("provide_exactly_one_text_or_audio_input".into()); }
        if input.text.as_ref().is_some_and(|v|v.trim().is_empty() || v.chars().count() > 4000) { return Err("text_must_be_1_to_4000_characters".into()); }
        if input.wav_base64.as_ref().is_some_and(|v|v.len() > 1_300_000) { return Err("audio_input_limit_30_seconds".into()); }
        let _gate = self.turn_gate.try_lock().map_err(|_|"brain_busy: stop the current turn first")?;
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_stopping".into()); }
        let (cancelled, mut cancellation) = watch::channel(false);
        *self.active.lock().map_err(|_|"brain_state_poisoned")? = Some(ActiveTurn { id:input.turn_id.clone(), cancelled });
        let mut lease = TurnLease {brain:self,id:input.turn_id.clone(),completed:false};
        self.emit("turn.started", Some(&input.turn_id), json!({"input":if input.text.is_some(){"text"}else{"audio"}}));
        let result = tokio::select! {
            result = self.execute(&input, trusted_console) => result,
            _ = cancellation.changed() => Err("cancelled".into()),
        };
        if let Ok(mut active) = self.active.lock() { *active = None; }
        lease.completed=true;
        self.emit(if result.is_ok(){"turn.completed"}else{"turn.failed"}, Some(&input.turn_id),
            json!({"error":result.as_ref().err().map(|e|e.split(':').next().unwrap_or("turn_failed"))}));
        result
    }

    async fn execute(&self, input: &TurnInput, trusted_console: bool) -> Result<TurnOutput, String> {
        // Keep short durable writes inside the turn gate, with no detached
        // spawn_blocking closure that could recreate metadata after Forget.
        self.memory.register_conversation(&input.conversation_id, input.profile_id.as_deref())?;
        let mut warnings = Vec::new();
        let (transcript, identity_candidate) = if let Some(wav) = input.wav_base64.as_ref() {
            self.emit("turn.transcribing", Some(&input.turn_id), json!({}));
            let result = self.models.call("transcribe", json!({"wav_base64":wav,"language":"zh"})).await?;
            let text = result.get("text").and_then(Value::as_str).unwrap_or("").trim().to_owned();
            if text.is_empty() { return Err("asr_no_speech".into()); }
            if text.chars().count() > 4000 { return Err("asr_transcript_too_long".into()); }
            let identity = match self.identify_media(memory::IdentityKind::Speaker, json!({"wav_base64":wav})).await {
                Ok(result) => Some(result),
                Err(error) => { warnings.push(format!("speaker_identification_unavailable: {error}")); None }
            };
            (text, identity)
        } else { (input.text.clone().unwrap_or_default().trim().to_owned(), None) };

        let memory = Arc::clone(&self.memory);
        let request = memory::ContextRequest { profile_id:input.profile_id.clone(), profile_confirmed:trusted_console && input.profile_id.is_some(),
            // Every configured plan can route to a cloud model; never include local-only notes.
            destination:memory::ContextDestination::Cloud, query:transcript.clone(), include_household:true };
        let context = tokio::task::spawn_blocking(move || memory.context(request)).await.map_err(|_|"memory_task_failed")??;
        self.emit("turn.thinking", Some(&input.turn_id), json!({"memory_count":context.memory_ids.len()}));
        let reply = self.harness.run(TurnRequest {
            turn_id:input.turn_id.clone(), conversation_id:input.conversation_id.clone(), user_text:transcript.clone(),
            system_prompt:scene::system_prompt(&self.config.conversation.language, &context.text), response_schema:Some(scene::schema()),
        }).await?;
        let scene = scene::normalize(&reply.text, reply.structured.as_ref(), self.config.conversation.max_reply_characters, &self.config.conversation.media_hosts);
        if scene.speech.trim().is_empty() { return Err("assistant_empty_reply".into()); }
        let audio = if input.speak.unwrap_or(self.config.conversation.default_speak) {
            self.emit("turn.synthesizing", Some(&input.turn_id), json!({}));
            let spoken: String = scene.speech.chars().take(1000).collect();
            if spoken.chars().count() < scene.speech.chars().count() { warnings.push("tts_truncated_to_1000_characters: full reply remains on screen".into()); }
            match self.models.call("tts", json!({"text":spoken,"voice":self.models.voice()})).await {
                Ok(audio) => Some(audio),
                Err(error) => { warnings.push(format!("tts_unavailable: {error}")); None }
            }
        } else { None };
        let user_turn = memory::TurnRecord { conversation_id:input.conversation_id.clone(), profile_id:input.profile_id.clone(), role:memory::TurnRole::User, content:transcript.clone() };
        let assistant_turn = memory::TurnRecord { conversation_id:input.conversation_id.clone(), profile_id:input.profile_id.clone(), role:memory::TurnRole::Assistant, content:scene.speech.clone() };
        // Cancellation drops async work. It must not leave a queued SQLite
        // writer behind after the gate is released to a deletion request.
        if let Err(error) = self.memory.append_turn(user_turn).and_then(|()| self.memory.append_turn(assistant_turn)) {
            warnings.push(format!("transcript_storage_failed: {error}"));
        }
        Ok(TurnOutput { turn_id:input.turn_id.clone(), conversation_id:input.conversation_id.clone(), transcript, scene,
            provider_id:reply.provider_id, model_id:reply.model_id, harness_session_id:reply.session_id, audio, identity_candidate, warnings })
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
        let _gate = self.turn_gate.try_lock().map_err(|_|"brain_busy: finish the current turn before enrollment")?;
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
    pub config_dir: PathBuf,
    resource: Option<PathBuf>,
    sink: EventSink,
    inner: RwLock<Result<Arc<Brain>,String>>,
    lifecycle: Mutex<()>,
}

impl BrainHost {
    pub fn new(config_dir: PathBuf, resource: Option<PathBuf>, sink: EventSink) -> Arc<Self> {
        let inner = Brain::new(config_dir.clone(), resource.clone(), Arc::clone(&sink));
        Arc::new(Self {config_dir, resource, sink, inner:RwLock::new(inner), lifecycle:Mutex::new(())})
    }

    pub fn get(&self) -> Result<Arc<Brain>, String> { self.inner.read().map_err(|_|"brain_host_poisoned".to_owned())?.clone() }

    pub async fn reload(&self) -> Result<Value, String> {
        let _lifecycle = self.lifecycle.lock().await;
        if let Ok(brain) = self.get() { brain.stop().await?; }
        let next = Brain::new(self.config_dir.clone(), self.resource.clone(), Arc::clone(&self.sink));
        *self.inner.write().map_err(|_|"brain_host_poisoned")? = next.clone();
        let brain = next?;
        Ok(brain.status().await)
    }

    pub async fn shutdown(&self) { if let Ok(brain) = self.get() { let _ = brain.stop().await; } }
}

#[tauri::command]
pub async fn companion_request(host: tauri::State<'_, Arc<BrainHost>>, method: String, payload: Option<Value>) -> Result<Value,String> {
    host.dispatch(&method,payload.unwrap_or_else(||json!({}))).await
}
