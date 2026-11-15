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
mod display;
mod memory_worker;
mod rpc;
mod timing;

use std::{path::PathBuf, sync::{Arc, Mutex as StdMutex, RwLock, atomic::{AtomicBool, Ordering}}, time::Instant};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex};
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
    cancellation_events: broadcast::Sender<Cancellation>,
    progress_events: broadcast::Sender<Value>,
    foreground_generation: watch::Sender<u64>,
    background: Mutex<Option<tokio::task::JoinHandle<()>>>,
    memory_script: Option<PathBuf>,
    gateway: Mutex<Option<gateway::RunningGateway>>,
}

impl Brain {
    fn new(config_dir: PathBuf, worker_resource: Option<PathBuf>, sink: EventSink) -> Result<Arc<Self>, String> {
        let memory_script = worker_resource.as_ref().and_then(|p|p.parent()).map(|p|p.join("memory_extract.py")).filter(|p|p.is_file());
        let config = settings::load_or_create(&config_dir, worker_resource)?;
        let runtime_dir = config_dir.join("runtime");
        settings::private_dir(&runtime_dir)?;
        let (progress_events, _) = broadcast::channel(64);
        let progress = progress_events.clone();
        let sink: EventSink = Arc::new(move |event| {
            let kind = event["kind"].as_str().unwrap_or("");
            if matches!(kind, "turn.transcribing" | "turn.understood" | "turn.thinking"
                | "turn.preparing_image" | "harness_tool_activity" | "harness_permission_required"
                | "harness_permission_replied" | "harness_question_required") {
                let _ = progress.send(json!({
                    "kind":kind, "turn_id":event["turn_id"],
                    "phase":event["phase"], "preview":event["payload"]["preview"]
                }));
            }
            sink(event);
        });
        let harness = Harness::new(config.harness.clone(), runtime_dir.join("opencode"), config_dir.clone(), Arc::clone(&sink))?;
        let memory = Store::open(runtime_dir.join("memory"), config.memory.clone())?;
        let credentials = credentials::Credentials::open(runtime_dir.join("devices"))?;
        let (cancellation_events, _) = broadcast::channel(64);
        let (foreground_generation, _) = watch::channel(0);
        Ok(Arc::new(Self { models: ModelWorker::new(config.models.clone(), config_dir.clone()), config, config_dir,
            harness, memory, credentials, sink, turn_gate:Mutex::new(()), active:StdMutex::new(None), accepting:AtomicBool::new(false),
            cancellation_events, progress_events, foreground_generation, background:Mutex::new(None), memory_script, gateway:Mutex::new(None) }))
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
        let memory_model=self.config_dir.join("models/memory-qwen3-4b");
        let memory_ready=self.memory_script.as_ref().is_some_and(|p|p.is_file()) &&
            ["config.json","model.safetensors","chat_template.jinja"].iter().all(|name|memory_model.join(name).is_file());
        json!({"ready":true,"accepting_turns":self.accepting.load(Ordering::Acquire),"active_turn":active,"harness":self.harness.status().await,
            "models_configured":self.models.configured(),"memory":self.memory.status().unwrap_or_else(|e|json!({"error":e})),
            "background_memory_model_ready":memory_ready,
            "gateway":gateway,"gateway_enabled":self.config.gateway.enabled,
            "config_path":self.config_dir.join("brain.json"),"legacy_lan_is_authenticated":false,
            "device_media_requires_v3":true})
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
        let id = {
            let active = self.active.lock().map_err(|_|"brain_state_poisoned")?;
            if let Some(active) = active.as_ref().filter(|a|cancellation.matches_turn(&a.id, Some(&a.conversation_id))) {
                active.cancelled.send_replace(true);
                Some(active.id.clone())
            } else { None }
        };
        if matches!(cancellation.scope, CancelScope::All) { self.models.cancel_current(); }
        if let Some(id) = id {
            self.models.cancel_current();
            let result = self.harness.cancel(&id).await;
            self.emit("turn.cancelled", Some(&id), json!({"code":reason}));
            result?;
        }
        Ok(())
    }

    pub(crate) fn cancellations(&self) -> broadcast::Receiver<Cancellation> { self.cancellation_events.subscribe() }

    fn progress(&self) -> broadcast::Receiver<Value> { self.progress_events.subscribe() }

    pub async fn turn(self: &Arc<Self>, input: TurnInput, trusted_console: bool) -> Result<TurnOutput, String> {
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_not_started: start the service from the console".into()); }
        if !valid_turn_id(&input.turn_id) || !valid_turn_id(&input.conversation_id) { return Err("turn_or_conversation_id_invalid".into()); }
        if !trusted_console && input.profile_id.is_some() { return Err("profile_requires_local_console".into()); }
        if input.text.is_some() == input.wav_base64.is_some() { return Err("provide_exactly_one_text_or_audio_input".into()); }
        if input.text.as_ref().is_some_and(|v|v.trim().is_empty() || v.chars().count() > 4000) { return Err("text_must_be_1_to_4000_characters".into()); }
        if input.wav_base64.as_ref().is_some_and(|v|v.len() > 1_300_000) { return Err("audio_input_limit_30_seconds".into()); }
        let _gate = self.turn_gate.try_lock().map_err(|_|"brain_busy: stop the current turn first")?;
        if !self.accepting.load(Ordering::Acquire) { return Err("brain_stopping".into()); }
        self.foreground_generation.send_modify(|value|*value=value.wrapping_add(1));
        let (cancelled, mut cancellation) = watch::channel(false);
        *self.active.lock().map_err(|_|"brain_state_poisoned")? = Some(ActiveTurn {
            id:input.turn_id.clone(), conversation_id:input.conversation_id.clone(), cancelled });
        let mut lease = TurnLease {brain:self,id:input.turn_id.clone(),completed:false};
        let started = Instant::now();
        self.emit("turn.started", Some(&input.turn_id), json!({"input":if input.text.is_some(){"text"}else{"audio"}}));
        let result = tokio::select! {
            result = self.execute(&input, trusted_console) => result,
            _ = cancellation.changed() => Err("cancelled".into()),
        };
        if let Ok(mut active) = self.active.lock() { *active = None; }
        lease.completed=true;
        self.emit(if result.is_ok(){"turn.completed"}else{"turn.failed"}, Some(&input.turn_id),
            json!({"error":result.as_ref().err().map(|e|e.split(':').next().unwrap_or("turn_failed")),
                "elapsed_ms":started.elapsed().as_millis() as u64,
                "timings":result.as_ref().ok().map(|output|&output.timings)}));
        result
    }

    async fn execute(&self, input: &TurnInput, trusted_console: bool) -> Result<TurnOutput, String> {
        let started = Instant::now();
        let mut timings = TurnTimings::default();
        // Keep short durable writes inside the turn gate, with no detached
        // spawn_blocking closure that could recreate metadata after Forget.
        self.timed_stage(&input.turn_id, "memory_register", &mut timings, async {
            self.memory.register_conversation(&input.conversation_id, input.profile_id.as_deref())
        }).await?;
        let mut warnings = Vec::new();
        let (transcript, identity_candidate) = if let Some(wav) = input.wav_base64.as_ref() {
            self.emit("turn.transcribing", Some(&input.turn_id), json!({}));
            let result = self.timed_stage(&input.turn_id, "asr", &mut timings,
                self.models.call("transcribe", json!({"wav_base64":wav,"language":"zh"}))).await?;
            timings.input_audio_ms = result.pointer("/quality/duration_ms").and_then(Value::as_u64);
            let text = result.get("text").and_then(Value::as_str).unwrap_or("").trim().to_owned();
            if text.is_empty() { return Err("asr_no_speech".into()); }
            if text.chars().count() > 4000 { return Err("asr_transcript_too_long".into()); }
            let preview: String = text.chars().take(80).collect();
            self.emit("turn.understood",Some(&input.turn_id),json!({"preview":preview}));
            // Matching cannot select a profile or change this turn's memory.
            // Do not load ECAPA for an empty enrollment store or a sample the
            // existing worker's three-second quality gate would reject.
            let skip_identity = if timings.input_audio_ms.is_some_and(|duration|duration < 3_000) {
                Some("audio_under_3_seconds")
            } else {
                match self.memory.has_enrollment(memory::IdentityKind::Speaker) {
                    Ok(true) => None,
                    Ok(false) => Some("no_enrolled_speakers"),
                    Err(error) => {
                        warnings.push(format!("speaker_identification_unavailable: {error}"));
                        Some("enrollment_lookup_failed")
                    }
                }
            };
            let identity = if let Some(reason) = skip_identity {
                self.skip_stage(&input.turn_id, "speaker_identification", reason, &mut timings);
                None
            } else {
                match self.timed_stage(&input.turn_id, "speaker_identification", &mut timings,
                    self.identify_media(memory::IdentityKind::Speaker, json!({"wav_base64":wav}))).await {
                    Ok(result) => Some(result),
                    Err(error) => { warnings.push(format!("speaker_identification_unavailable: {error}")); None }
                }
            };
            (text, identity)
        } else {
            self.skip_stage(&input.turn_id, "asr", "text_input", &mut timings);
            self.skip_stage(&input.turn_id, "speaker_identification", "text_input", &mut timings);
            (input.text.clone().unwrap_or_default().trim().to_owned(), None)
        };

        if let Err(error)=self.timed_stage(&input.turn_id, "memory_enqueue", &mut timings, async {
            self.memory.enqueue_memory_job(&input.turn_id,&input.conversation_id,input.profile_id.as_deref(),&transcript)
        }).await {
            warnings.push(format!("background_memory_queue_failed: {error}"));
        }
        let memory = Arc::clone(&self.memory);
        let request = memory::ContextRequest { profile_id:input.profile_id.clone(), profile_confirmed:trusted_console && input.profile_id.is_some(),
            // Every configured plan can route to a cloud model; never include local-only notes.
            destination:memory::ContextDestination::Cloud, query:transcript.clone(), include_household:true };
        let context = self.timed_stage(&input.turn_id, "memory_context", &mut timings, async move {
            tokio::task::spawn_blocking(move || memory.context(request)).await.map_err(|_|"memory_task_failed".to_owned())?
        }).await?;
        self.emit("turn.thinking", Some(&input.turn_id), json!({"memory_count":context.memory_ids.len()}));
        let mut prompt=scene::system_prompt(&self.config.conversation.language, &context.text);
        if input.device_display {
            prompt.push_str("\n当用户要求看已有图片时，先用工具找到准确文件路径或 HTTPS 图片地址，在 visual 填 photo 和 source，图片字节由宿主处理。用户要求比较图时，先读资料，在 visual 填 bars、unit 和两项 series。无图片请求时 visual.kind 为 none。无关字段用 null 或空数组。直接回答用 outcome=answered，已经通过工具完成操作用 done，需补充信息用 needs_input，未能完成用 partial。effect 填 none、read、write 或 other，按用户实际要求选择。");
            prompt.push_str("\n追问沿用本次对话的上下文。若“那个”等指代有多个合理对象，把一句简短问题直接放在 speech 中，按用户的原称呼列出候选。小呆会显示问题，用户用下一句语音选择。等待选择前不要查找或猜测文件。此轮用 needs_input、effect=none、visual.kind=none。用户选定后再执行。visual.requested 仅在用户要求显示图片或图表时填 true，包括对前一句看图请求的补充。普通算术问答用文字即可。");
        }
        let reply = self.timed_stage(&input.turn_id, "harness", &mut timings, self.harness.run(TurnRequest {
            turn_id:input.turn_id.clone(), conversation_id:input.conversation_id.clone(), user_text:transcript.clone(),
            system_prompt:prompt, response_schema:Some(if input.device_display {scene::device_schema()} else {scene::schema()}),
            clarify_on_device:input.device_display,
        })).await?;
        let mut scene = scene::normalize(&reply.text, reply.structured.as_ref(), self.config.conversation.max_reply_characters, &self.config.conversation.media_hosts);
        if scene.speech.trim().is_empty() { return Err("assistant_empty_reply".into()); }
        let mut device_outcome=reply.structured.as_ref().and_then(|value|value["outcome"].as_str())
            .filter(|value|matches!(*value,"answered"|"done"|"needs_input"|"partial")).unwrap_or("answered").to_owned();
        let effect = reply.structured.as_ref().and_then(|value| value["effect"].as_str()).unwrap_or("none");
        let mut effect_unconfirmed=false;
        if input.device_display && device_outcome=="done" {
            let confirmed=display::confirmed_effect(effect,&reply.evidence);
            if !confirmed {
                effect_unconfirmed=true;
                device_outcome="partial".into();
                scene.speech="暂时无法确认操作是否完成，请在 Mac 上检查。".into();
            }
        }
        let display_image = if input.device_display && device_outcome != "needs_input" {
            let visual=reply.structured.as_ref().and_then(|v|v.get("visual"));
            let required = display::image_required(visual, &device_outcome, effect);
            if visual.and_then(|v|v["kind"].as_str()).is_some_and(|kind|kind!="none") {
                self.emit("turn.preparing_image",Some(&input.turn_id),json!({}));
            }
            match self.timed_stage(&input.turn_id, "image_prepare", &mut timings,
                display::prepare(&self.models, self.harness.workspace(), visual, &reply.evidence, &transcript,
                    &self.config.conversation.media_hosts)).await {
                Ok(Some((image, conclusion))) => {
                    if let Some(conclusion)=conclusion {
                        scene.speech=if effect_unconfirmed {
                            format!("{conclusion} 暂时无法确认操作是否完成，请在 Mac 上检查。")
                        } else { conclusion };
                    }
                    Some(image)
                },
                Ok(None) if required => {
                    warnings.push("display_image_unavailable: missing_visual".into());
                    device_outcome = "partial".into();
                    None
                }
                Ok(None) => None,
                Err(error) => {
                    if required {
                        warnings.push(format!("display_image_unavailable: {error}"));
                        device_outcome = "partial".into();
                    } else {
                        warnings.push(format!("optional_image_unavailable: {error}"));
                    }
                    None
                }
            }
        } else {
            self.skip_stage(&input.turn_id, "image_prepare", "no_device_image", &mut timings);
            None
        };
        let mut tts_calls = 0;
        let audio = if input.speak.unwrap_or(self.config.conversation.default_speak) {
            self.emit("turn.synthesizing", Some(&input.turn_id), json!({}));
            let spoken: String = scene.speech.chars().take(1000).collect();
            if spoken.chars().count() < scene.speech.chars().count() { warnings.push("tts_truncated_to_1000_characters: full reply remains on screen".into()); }
            tts_calls += 1;
            match self.timed_stage(&input.turn_id, "tts", &mut timings,
                self.models.call("tts", json!({"text":spoken,"voice":self.models.voice()}))).await {
                Ok(audio) => Some(audio),
                Err(error) => { warnings.push(format!("tts_unavailable: {error}")); None }
            }
        } else {
            self.skip_stage(&input.turn_id, "tts", "output_disabled", &mut timings);
            None
        };
        let user_turn = memory::TurnRecord { conversation_id:input.conversation_id.clone(), profile_id:input.profile_id.clone(), role:memory::TurnRole::User, content:transcript.clone() };
        let assistant_turn = memory::TurnRecord { conversation_id:input.conversation_id.clone(), profile_id:input.profile_id.clone(), role:memory::TurnRole::Assistant, content:scene.speech.clone() };
        // Cancellation drops async work. It must not leave a queued SQLite
        // writer behind after the gate is released to a deletion request.
        if let Err(error) = self.timed_stage(&input.turn_id, "history", &mut timings, async {
            self.memory.append_turn(user_turn).and_then(|()| self.memory.append_turn(assistant_turn))
        }).await {
            warnings.push(format!("transcript_storage_failed: {error}"));
        }
        timings.total_ms = started.elapsed().as_millis() as u64;
        Ok(TurnOutput { turn_id:input.turn_id.clone(), conversation_id:input.conversation_id.clone(), transcript, scene,
            provider_id:reply.provider_id, model_id:reply.model_id, harness_session_id:reply.session_id, audio, identity_candidate, warnings, timings, tts_calls, display_image, device_outcome })
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
    diagnostics: Option<DiagnosticSnapshot>,
    inner: RwLock<Result<Arc<Brain>,String>>,
    lifecycle: Mutex<()>,
    auto_start_pending: AtomicBool,
}

impl BrainHost {
    pub fn new(config_dir: PathBuf, resource: Option<PathBuf>, sink: EventSink, diagnostics: Option<DiagnosticSnapshot>) -> Arc<Self> {
        let inner = Brain::new(config_dir.clone(), resource.clone(), Arc::clone(&sink));
        Arc::new(Self {config_dir, resource, sink, diagnostics, inner:RwLock::new(inner), lifecycle:Mutex::new(()),
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
        let next = Brain::new(self.config_dir.clone(), self.resource.clone(), Arc::clone(&self.sink));
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
