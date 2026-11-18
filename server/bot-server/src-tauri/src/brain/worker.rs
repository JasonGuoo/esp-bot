//! Bounded independent local inference lanes, official libraries behind JSONL.
//! ASR never queues behind identity; dropping one request kills only its lane.
//! The process never listens on a socket and cannot be called by an MCP tool.

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin, ChildStdout, Command}, sync::{watch, Mutex}, time::timeout};

const MAX_LINE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelWorkerConfig {
    pub python: String,
    /// Absolute installed path, or resolved bundled resource path by composition root.
    pub script: Option<PathBuf>,
    pub model_dir: Option<PathBuf>,
    pub request_timeout_seconds: u64,
    pub voice: Option<String>,
    pub asr_backend: String,
    pub rma_qwen_python: Option<PathBuf>,
    pub rma_qwen_worker: Option<PathBuf>,
    pub rma_qwen_model: Option<PathBuf>,
    /// Local pyannote Community-1 snapshot; never downloaded during a turn.
    pub diarization_model: Option<PathBuf>,
    /// Optional isolated Python with pyannote and SpeechBrain for the voice lane.
    pub speaker_python: Option<PathBuf>,
    /// "auto" (Kokoro when its model and Python are installed, else macOS say),
    /// "kokoro" or "say". Any Kokoro failure falls back to say inside the worker.
    pub tts_backend: String,
    /// Kokoro (sherpa-onnx) model directory; auto: `<data>/engines/kokoro-multi-lang-v1_1`.
    pub kokoro_model_dir: Option<PathBuf>,
    /// Python with sherpa-onnx for the TTS lanes; auto: `<data>/engines/kokoro-python/bin/python`.
    pub tts_python: Option<PathBuf>,
    pub kokoro_threads: u8,
}

impl Default for ModelWorkerConfig {
    fn default() -> Self {
        Self { python: "python3".into(), script: None, model_dir: None, request_timeout_seconds: 120, voice: None,
            asr_backend: "whisper".into(), rma_qwen_python: None, rma_qwen_worker: None, rma_qwen_model: None, diarization_model: None, speaker_python: None,
            tts_backend: "auto".into(), kokoro_model_dir: None, tts_python: None, kokoro_threads: 4 }
    }
}

struct Process {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id().and_then(|id|i32::try_from(id).ok()).filter(|id|*id>0) {
            unsafe { libc::kill(-pid, libc::SIGKILL); }
        }
        let _ = self.child.start_kill();
    }
}

pub struct ModelWorker {
    config: ModelWorkerConfig,
    data_root: PathBuf,
    /// transcribe, voice, tts, image, other, face, and a priority TTS lane for the
    /// listening ack, progress lines and each answer's first sentence.
    lanes: [Mutex<Option<Process>>; 7],
    cancel: watch::Sender<u64>,
}

impl ModelWorker {
    pub fn new(config: ModelWorkerConfig, data_root: PathBuf) -> Arc<Self> {
        let (cancel, _) = watch::channel(0);
        Arc::new(Self { config, data_root, lanes: std::array::from_fn(|_| Mutex::new(None)), cancel })
    }

    pub fn configured(&self) -> bool {
        self.config.script.as_ref().is_some_and(|p| p.is_file())
            && self.config.model_dir.is_some()
    }

    fn lane(&self, op: &str) -> &Mutex<Option<Process>> {
        &self.lanes[match op {
            "transcribe" => 0, "speaker_embed" | "diarize" | "voice_health" => 1, "face_embed" => 5,
            "tts" => 2, "tts_first" => 6, "display_image" | "prepare_image" | "render_chart" => 3, _ => 4,
        }]
    }

    fn spawn(&self, op:&str) -> Result<Process, String> {
        let script = self.config.script.as_ref().filter(|p| p.is_file())
            .ok_or("local_worker_not_configured: set models.script to local-models/worker.py")?;
        let model_dir = self.config.model_dir.as_ref().ok_or("local_worker_model_dir_missing")?;
        let temp_dir = self.data_root.join("runtime/tmp");
        let cache_dir = self.data_root.join("runtime/cache");
        super::settings::private_dir(&temp_dir)?;
        super::settings::private_dir(&cache_dir)?;
        let kokoro=self.kokoro();
        let python=if matches!(op,"diarize"|"speaker_embed"|"voice_health") {
            self.config.speaker_python.as_deref().unwrap_or_else(||std::path::Path::new(&self.config.python))
        } else if let (true,Some((_,python)))=(matches!(op,"tts"|"tts_first"),&kokoro) {python.as_path()}
        else {std::path::Path::new(&self.config.python)};
        let mut command = Command::new(python);
        // The worker may live inside the signed app bundle. Python must not
        // create __pycache__ there and invalidate the app's sealed resources.
        command.arg("-B").arg("-u").arg(script).arg("--model-dir").arg(model_dir)
            .arg("--asr-backend").arg(&self.config.asr_backend)
            .env("HF_HUB_OFFLINE", "1").env("TRANSFORMERS_OFFLINE", "1")
            .env("TOKENIZERS_PARALLELISM", "false")
            .env("TMPDIR", temp_dir).env("XDG_CACHE_HOME", &cache_dir)
            .env("HF_HOME", cache_dir.join("huggingface"))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .kill_on_drop(true);
        for (flag, path) in [("--rma-qwen-python", &self.config.rma_qwen_python),
            ("--rma-qwen-worker", &self.config.rma_qwen_worker), ("--rma-qwen-model", &self.config.rma_qwen_model),
            ("--diarization-model", &self.config.diarization_model)] {
            if let Some(path) = path { command.arg(flag).arg(path); }
        }
        if let Some((dir,_))=&kokoro {
            command.arg("--tts-backend").arg("kokoro").arg("--kokoro-model-dir").arg(dir)
                .arg("--kokoro-threads").arg(self.config.kokoro_threads.clamp(1,16).to_string());
        }
        // Cancellation kills say/ffmpeg descendants too, never a shell command.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| format!("local_worker_start_failed: {e}"))?;
        crate::diagnostics::capture_stderr(child.stderr.take(), "local_models");
        let input = child.stdin.take().ok_or("local_worker_stdin_missing")?;
        let output = BufReader::new(child.stdout.take().ok_or("local_worker_stdout_missing")?);
        Ok(Process { child, input, output })
    }

    /// The Kokoro model directory and TTS Python, when Kokoro is selected and installed.
    fn kokoro(&self) -> Option<(PathBuf,PathBuf)> {
        if self.config.tts_backend=="say" {return None;}
        let engines=self.data_root.join("engines");
        let dir=self.config.kokoro_model_dir.clone().unwrap_or_else(||engines.join("kokoro-multi-lang-v1_1"));
        if !(dir.join("model.onnx").is_file()||dir.join("model.int8.onnx").is_file()) {return None;}
        let python=self.config.tts_python.clone().or_else(||Some(engines.join("kokoro-python/bin/python")).filter(|p|p.is_file()))
            .unwrap_or_else(||PathBuf::from(&self.config.python));
        Some((dir,python))
    }

    /// Each lane is serialized. The Brain bounds admitted inputs; waits are bounded too.
    pub async fn call(&self, op: &str, payload: Value) -> Result<Value, String> {
        self.call_at_generation(op,payload,self.generation()).await
    }

    pub fn generation(&self) -> u64 { *self.cancel.borrow() }

    pub async fn call_at_generation(&self, op:&str, payload:Value, generation:u64) -> Result<Value,String> {
        self.exchange(op,payload,generation,false).await
    }

    pub(super) async fn transcribe_if_idle(&self, wav:String) -> Result<Value,String> {
        self.exchange("transcribe",json!({"wav_base64":wav,"language":"zh"}),self.generation(),true).await
    }

    async fn exchange(&self, op:&str, payload:Value, generation:u64, speculative:bool) -> Result<Value,String> {
        let mut cancelled = self.cancel.subscribe();
        if *cancelled.borrow() != generation { return Err("cancelled".into()); }
        let mut slot = if speculative {
            self.lane(op).try_lock().map_err(|_|"local_worker_lane_busy")?
        } else { tokio::select! {
            slot = timeout(Duration::from_secs(15), self.lane(op).lock()) => slot.map_err(|_| "local_worker_lane_busy")?,
            _ = cancelled.changed() => return Err("cancelled".into()),
        }};
        if self.generation() != generation { return Err("cancelled".into()); }
        if slot.is_none() { *slot = Some(self.spawn(op)?); }
        let request_id = uuid::Uuid::new_v4().to_string();
        let worker_op=match op {"voice_health"=>"health","tts_first"=>"tts",_=>op};
        let mut bytes = serde_json::to_vec(&json!({"id":request_id,"op":worker_op,"payload":payload})).map_err(|e| e.to_string())?;
        if bytes.len() + 1 > MAX_LINE { return Err("local_worker_request_too_large".into()); }
        bytes.push(b'\n');
        // Own the child across await: if the caller future is dropped, Drop
        // terminates its process group. Never put an unread response back in the slot.
        let mut process = slot.take().ok_or("local_worker_missing")?;
        let exchange = async {
            process.input.write_all(&bytes).await.map_err(|_| "local_worker_write_failed".to_string())?;
            process.input.flush().await.map_err(|_| "local_worker_flush_failed".to_string())?;
            let line = bounded_line(&mut process.output).await?;
            let reply: Value = serde_json::from_slice(&line).map_err(|_| "local_worker_invalid_json".to_string())?;
            if reply.get("id").and_then(Value::as_str) != Some(request_id.as_str()) {
                return Err("local_worker_response_id_mismatch".to_string());
            }
            Ok(reply)
        };
        let deadline = Duration::from_secs(self.config.request_timeout_seconds.clamp(5, 300));
        let result = tokio::select! {
            result = timeout(deadline, exchange) => result.map_err(|_| "local_worker_timeout".to_string()).and_then(|v|v),
            _ = cancelled.changed() => Err("cancelled".to_string()),
        };
        match result {
            Ok(reply) => {
                if self.generation() != generation {
                    kill_process(&mut process.child).await;
                    return Err("cancelled".into());
                }
                *slot = Some(process);
                if reply.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    let error = reply.get("error").cloned().unwrap_or(Value::Null);
                    let code = error.get("code").and_then(Value::as_str).unwrap_or("model_error");
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("local model operation failed");
                    Err(format!("{code}: {}", message.chars().take(500).collect::<String>()))
                }
            }
            Err(error) => {
                kill_process(&mut process.child).await;
                Err(error)
            }
        }
    }

    pub async fn health(&self) -> Value {
        if !self.configured() { return json!({"configured":false,"ready":false,"error":"local_worker_not_configured"}); }
        match self.call("health", json!({})).await {
            Ok(mut result) => {
                if self.config.speaker_python.is_some() {
                    match self.call("voice_health",json!({})).await {
                        Ok(voice) => for kind in ["speaker","diarization"] {result["models"][kind]=voice["models"][kind].clone();},
                        Err(error) => for kind in ["speaker","diarization"] {result["models"][kind]=json!({"available":false,"error":error});},
                    }
                }
                json!({"configured":true,"ready":true,"models":result})
            },
            Err(error) => json!({"configured":true,"ready":false,"error":error}),
        }
    }

    pub fn cancel_current(&self) { self.cancel.send_modify(|generation| *generation = generation.wrapping_add(1)); }

    pub async fn stop(&self) {
        self.cancel_current();
        for lane in &self.lanes {
            if let Some(mut process) = lane.lock().await.take() { kill_process(&mut process.child).await; }
        }
    }

    pub fn voice(&self) -> Option<&str> { self.config.voice.as_deref() }
}

async fn bounded_line(reader: &mut BufReader<ChildStdout>) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    loop {
        let buffer = reader.fill_buf().await.map_err(|_| "local_worker_read_failed")?;
        if buffer.is_empty() { return Err("local_worker_exited".into()); }
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(buffer.len(), |i| i + 1);
        if output.len() + count > MAX_LINE { return Err("local_worker_response_too_large".into()); }
        output.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() { return Ok(output); }
    }
}

async fn kill_process(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Only our own child process group, created with process_group(0).
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
    }
    let _ = child.kill().await;
    let _ = timeout(Duration::from_secs(3), child.wait()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_is_used_only_when_installed_and_the_first_sentence_has_its_own_lane() {
        let root=std::env::temp_dir().join(format!("xiaodai-kokoro-{}",uuid::Uuid::new_v4()));
        let auto=ModelWorker::new(ModelWorkerConfig::default(),root.clone());
        assert_eq!(auto.kokoro(),None,"auto without a model keeps macOS say");
        let dir=root.join("engines/kokoro-multi-lang-v1_1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"),b"x").unwrap();
        assert_eq!(auto.kokoro(),Some((dir.clone(),PathBuf::from("python3"))));
        let say=ModelWorker::new(ModelWorkerConfig{tts_backend:"say".into(),..Default::default()},root.clone());
        assert_eq!(say.kokoro(),None);
        assert!(!std::ptr::eq(auto.lane("tts"),auto.lane("tts_first")));
        assert!(std::ptr::eq(auto.lane("tts"),auto.lane("tts")));
        let _=std::fs::remove_dir_all(root);
    }
}
