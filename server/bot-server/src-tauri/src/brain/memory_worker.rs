//! Independent, preemptible local extraction. It never uses the foreground
//! OpenCode gate, cloud provider, raw media, or an MCP tool.
use std::{process::Stdio, sync::{Weak, atomic::Ordering}, time::Duration};
use serde::Deserialize;
use serde_json::json;
use tokio::{io::AsyncWriteExt, process::Command, time::{sleep, timeout}};
use super::Brain;
use crate::memory::ExtractedFact;

#[derive(Deserialize)]
struct Extraction { facts: Vec<ExtractedFact> }

pub async fn run(brain: Weak<Brain>) {
    loop {
        sleep(Duration::from_secs(10)).await;
        let Some(brain)=brain.upgrade() else {break};
        if !brain.accepting.load(Ordering::Acquire) {break;}
        let Some(script)=brain.memory_script.as_ref().filter(|p|p.is_file()) else {continue};
        let model=brain.config_dir.join("models/memory-qwen3-4b");
        if !["config.json","model.safetensors","chat_template.jinja"].iter().all(|name|model.join(name).is_file()) {continue;}
        if brain.active.lock().map(|a|a.is_some()).unwrap_or(true) {continue;}
        let Some(job)=brain.memory.next_memory_job().ok().flatten() else {continue};
        let mut foreground=brain.foreground_generation.subscribe();
        let generation=*foreground.borrow();
        if brain.active.lock().map(|a|a.is_some()).unwrap_or(true) {continue;}
        let python=brain.config.models.rma_qwen_python.as_ref().map(|p|p.as_os_str())
            .unwrap_or_else(||std::ffi::OsStr::new(&brain.config.models.python));
        let tmp=brain.config_dir.join("runtime/tmp");
        if super::settings::private_dir(&tmp).is_err() {continue;}
        #[cfg(target_os="macos")]
        let mut cmd={let mut command=Command::new("/usr/bin/nice");command.arg("-n").arg("10").arg(python);command};
        #[cfg(not(target_os="macos"))]
        let mut cmd=Command::new(python);
        cmd.arg("-B").arg(script).arg(&model).env("TMPDIR",tmp)
            .env("HF_HOME",brain.config_dir.join("runtime/cache/huggingface"))
            .env("HF_HUB_OFFLINE","1").env("TRANSFORMERS_OFFLINE","1")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let Ok(mut child)=cmd.spawn() else {let _=brain.memory.fail_memory_job(&job.id);continue};
        let Some(mut input)=child.stdin.take() else {let _=brain.memory.fail_memory_job(&job.id);continue};
        let mut bytes=match serde_json::to_vec(&json!({"transcript":job.transcript})) {Ok(v)=>v,Err(_)=>continue};
        bytes.push(b'\n');
        if input.write_all(&bytes).await.is_err() {let _=brain.memory.fail_memory_job(&job.id);continue;}
        drop(input);
        let result=tokio::select! {
            result=timeout(Duration::from_secs(180),child.wait_with_output())=>Some(result),
            _=foreground.changed()=>None,
        };
        // A foreground turn cancels the owned process through kill_on_drop. Do
        // not count preemption as an extraction failure.
        let Some(result)=result else {continue};
        let parsed=result.ok().and_then(Result::ok).filter(|output|output.status.success() && output.stdout.len()<=65536)
            .and_then(|output|serde_json::from_slice::<Extraction>(&output.stdout).ok());
        let Some(extraction)=parsed else {let _=brain.memory.fail_memory_job(&job.id);continue};
        if *foreground.borrow()!=generation {continue;}
        let Ok(_gate)=brain.turn_gate.try_lock() else {continue};
        if *foreground.borrow()!=generation || !brain.accepting.load(Ordering::Acquire) {continue;}
        if let Err(error)=brain.memory.complete_memory_job(&job.id,&extraction.facts) {
            eprintln!("[brain] background memory could not be saved: {}",error.split(':').next().unwrap_or("error"));
        }
    }
}
