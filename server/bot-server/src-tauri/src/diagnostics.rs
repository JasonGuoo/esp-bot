//! Bounded, owner-only metadata logs. Arbitrary event payloads and child output
//! are never persisted: they may contain speech, images, prompts or credentials.
use std::{fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf},
    sync::{mpsc::{self, SyncSender}, OnceLock, atomic::{AtomicU64, Ordering}}};
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, process::ChildStderr};

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINE: usize = 1024;
static LOG: OnceLock<SyncSender<String>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

pub fn init(data_root: &Path) -> Result<(), String> {
    if LOG.get().is_some() { return Ok(()); }
    let directory = data_root.join("runtime/logs");
    crate::brain::settings::private_dir(&directory)?;
    let path = directory.join("server.jsonl");
    let file = open(&path).map_err(|_|"diagnostics_open_failed")?;
    let (sender, receiver) = mpsc::sync_channel::<String>(128);
    std::thread::Builder::new().name("bot-diagnostics".into()).spawn(move || {
        let mut file = file;
        let mut bytes = file.metadata().map(|meta|meta.len()).unwrap_or(0);
        for line in receiver {
            if bytes.saturating_add(line.len() as u64) > MAX_BYTES {
                // Only this thread owns the files. Rotation never waits on the
                // async runtime, the UI, or child stdout protocol readers.
                match rotate(&path) {
                    Ok(next) => { file = next; bytes = 0; },
                    Err(_) => { eprintln!("[diagnostics] rotation_failed"); continue; },
                }
            }
            if file.write_all(line.as_bytes()).is_ok() { bytes += line.len() as u64; }
            else { eprintln!("[diagnostics] write_failed"); }
        }
    }).map_err(|_|"diagnostics_thread_failed")?;
    LOG.set(sender).map_err(|_|"diagnostics_already_started".to_owned())?;
    Ok(())
}

fn open(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)] {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(not(unix))] { options.open(path) }
}

fn rotate(path: &Path) -> std::io::Result<File> {
    let backup = |index| PathBuf::from(format!("{}.{index}",path.display()));
    for (from, to) in [(backup(1), backup(2)), (path.to_owned(), backup(1))] {
        match fs::rename(from, to) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    open(path)
}

fn safe_code(value: &Value) -> Option<&str> {
    value.as_str().filter(|code| !code.is_empty() && code.len() <= 80 &&
        code.bytes().all(|byte|byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte)))
}

fn record(mut entry: Value) {
    let Some(sender) = LOG.get() else { return; };
    entry["at_ms"] = json!(crate::time::now_ms());
    let lost = DROPPED.swap(0, Ordering::Relaxed);
    if lost > 0 { entry["dropped_records"] = json!(lost); }
    let mut encoded = entry.to_string();
    if encoded.len() > MAX_LINE { DROPPED.fetch_add(lost + 1, Ordering::Relaxed); return; }
    encoded.push('\n');
    if sender.try_send(encoded).is_err() { DROPPED.fetch_add(lost + 1, Ordering::Relaxed); }
}

pub fn event(event: &Value) {
    let mut entry = json!({"source":"brain","kind":safe_code(&event["kind"]).unwrap_or("event")});
    for key in ["code", "stage", "phase", "status", "reason"] {
        if let Some(value) = safe_code(&event[key]).or_else(||safe_code(&event["payload"][key])) {
            entry[key] = json!(value);
        }
    }
    for key in ["elapsed_ms", "prepare_ms", "attempt", "safe_to_fallback"] {
        let value = if !event[key].is_null() { &event[key] } else { &event["payload"][key] };
        if value.is_u64() || value.is_boolean() { entry[key] = value.clone(); }
    }
    // Timing schemas are numeric. Never serialize any sibling payload fields.
    for key in ["asr_ms", "speaker_ms", "llm_ms", "image_ms", "tts_ms", "total_ms"] {
        if let Some(value) = event.pointer(&format!("/payload/timings/{key}")).and_then(Value::as_u64) {
            entry[key] = json!(value);
        }
    }
    record(entry);
}

fn child_entry(source: &'static str, line: &[u8], oversized: bool) -> Value {
    let mut entry = json!({"source":source,"kind":"child_stderr","bytes":line.len(),"oversized":oversized});
    let text = std::str::from_utf8(line).unwrap_or("");
    // worker.py deliberately suppresses library output and emits only these
    // fields. Even here, omit IDs and free text. OpenCode stderr is untrusted.
    if ((source == "local_models" && text.starts_with("local-models ")) ||
        (source == "memory_extractor" && text.starts_with("memory-extract "))) && !oversized {
        for field in text.split_ascii_whitespace() {
            let Some((key,value)) = field.split_once('=') else { continue; };
            if key == "elapsed_ms" {
                if let Ok(ms) = value.parse::<u64>() { entry[key] = json!(ms); }
            } else if matches!(key,"op" | "result" | "type") && safe_code(&json!(value)).is_some() {
                entry[key] = json!(value);
            }
        }
    } else if let Some(code) = ["EADDRINUSE", "ECONNREFUSED", "ETIMEDOUT", "ENOTFOUND",
        "ERR_MODULE_NOT_FOUND", "ENOMEM", "EACCES", "ENOSPC"].into_iter().find(|code|text.contains(*code)) {
        entry["code"] = json!(code);
    }
    entry
}

pub fn capture_stderr(stderr: Option<ChildStderr>, source: &'static str) {
    let Some(mut stderr) = stderr else { return; };
    tauri::async_runtime::spawn(async move {
        let mut chunk = [0u8; 512];
        let mut line = Vec::with_capacity(MAX_LINE);
        let mut oversized = false;
        loop {
            let count = match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            for byte in &chunk[..count] {
                if *byte == b'\n' {
                    record(child_entry(source,&line,oversized)); line.clear(); oversized = false;
                } else if line.len() < MAX_LINE { line.push(*byte); }
                else { oversized = true; }
            }
        }
        if !line.is_empty() || oversized { record(child_entry(source,&line,oversized)); }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_free_text_and_credentials_never_reach_the_log() {
        let raw=b"Authorization: Bearer secret-token; transcript=private-words ECONNREFUSED";
        let entry=child_entry("opencode",raw,false);
        assert_eq!(entry["code"],"ECONNREFUSED");
        let text=entry.to_string();
        assert!(!text.contains("secret-token") && !text.contains("private-words"));
        let worker=child_entry("local_models",b"local-models id=private-id op=asr result=ok elapsed_ms=42 text=private-words",false);
        assert_eq!(worker["op"],"asr");assert_eq!(worker["elapsed_ms"],42);
        assert!(!worker.to_string().contains("private-"));
        let long=child_entry("local_models",b"local-models op=asr result=ok",true);
        assert!(long.get("op").is_none());
    }
    #[test]
    fn rotation_is_bounded_and_files_remain_private() {
        let dir=std::env::temp_dir().join(format!("espbot-log-{}",uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();let path=dir.join("server.jsonl");
        for i in 0..5 {let mut file=rotate(&path).unwrap();writeln!(file,"record-{i}").unwrap();}
        assert_eq!(fs::read_to_string(&path).unwrap(),"record-4\n");
        assert_eq!(fs::read_to_string(dir.join("server.jsonl.2")).unwrap(),"record-2\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(),3);
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777,0o600);}
        fs::remove_dir_all(dir).unwrap();
    }
}
