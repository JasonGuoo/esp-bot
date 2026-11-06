//! One lazy local inference process, official model libraries behind JSONL.
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
}

impl Default for ModelWorkerConfig {
    fn default() -> Self {
        Self { python: "python3".into(), script: None, model_dir: None, request_timeout_seconds: 120, voice: None }
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
    process: Mutex<Option<Process>>,
    cancel: watch::Sender<u64>,
}

impl ModelWorker {
    pub fn new(config: ModelWorkerConfig) -> Arc<Self> {
        let (cancel, _) = watch::channel(0);
        Arc::new(Self { config, process: Mutex::new(None), cancel })
    }

    pub fn configured(&self) -> bool {
        self.config.script.as_ref().is_some_and(|p| p.is_file())
            && self.config.model_dir.is_some()
    }

    fn spawn(&self) -> Result<Process, String> {
        let script = self.config.script.as_ref().filter(|p| p.is_file())
            .ok_or("local_worker_not_configured: set models.script to local-models/worker.py")?;
        let model_dir = self.config.model_dir.as_ref().ok_or("local_worker_model_dir_missing")?;
        let mut command = Command::new(&self.config.python);
        command.arg("-u").arg(script).arg("--model-dir").arg(model_dir)
            .env("HF_HUB_OFFLINE", "1").env("TRANSFORMERS_OFFLINE", "1")
            .env("TOKENIZERS_PARALLELISM", "false")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true);
        // Cancellation kills say/ffmpeg descendants too, never a shell command.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| format!("local_worker_start_failed: {e}"))?;
        let input = child.stdin.take().ok_or("local_worker_stdin_missing")?;
        let output = BufReader::new(child.stdout.take().ok_or("local_worker_stdout_missing")?);
        Ok(Process { child, input, output })
    }

    /// Inference is serialized; callers get busy rather than retaining unbounded media.
    pub async fn call(&self, op: &str, payload: Value) -> Result<Value, String> {
        self.call_at_generation(op,payload,self.generation()).await
    }

    pub fn generation(&self) -> u64 { *self.cancel.borrow() }

    pub async fn call_at_generation(&self, op:&str, payload:Value, generation:u64) -> Result<Value,String> {
        let mut cancelled = self.cancel.subscribe();
        if *cancelled.borrow() != generation { return Err("cancelled".into()); }
        let mut slot = self.process.try_lock().map_err(|_| "local_worker_busy")?;
        if slot.is_none() { *slot = Some(self.spawn()?); }
        let request_id = uuid::Uuid::new_v4().to_string();
        let mut bytes = serde_json::to_vec(&json!({"id":request_id,"op":op,"payload":payload})).map_err(|e| e.to_string())?;
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
            Ok(result) => json!({"configured":true,"ready":true,"models":result}),
            Err(error) => json!({"configured":true,"ready":false,"error":error}),
        }
    }

    pub fn cancel_current(&self) { self.cancel.send_modify(|generation| *generation = generation.wrapping_add(1)); }

    pub async fn stop(&self) {
        self.cancel_current();
        if let Some(mut process) = self.process.lock().await.take() { kill_process(&mut process.child).await; }
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
