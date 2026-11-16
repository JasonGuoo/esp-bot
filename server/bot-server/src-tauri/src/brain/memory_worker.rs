//! Durable household organization and Graphiti outbox worker. It never enters
//! the foreground OpenCode runtime; a new foreground generation preempts work.
use std::{process::Stdio,sync::{Weak,atomic::Ordering},time::Duration};
use serde::Deserialize;
use serde_json::{json,Value};
use tokio::{io::{AsyncReadExt,AsyncWriteExt},process::Command,time::{sleep,timeout}};
use super::Brain;
use crate::memory::{ExtractedFact,HouseholdExtraction};

#[derive(Deserialize)]
struct Extraction { facts:Vec<ExtractedFact> }

pub async fn run(brain:Weak<Brain>){
    loop {
        sleep(Duration::from_secs(3)).await;
        let Some(brain)=brain.upgrade()else{break};
        if !brain.accepting.load(Ordering::Acquire){brain.graphiti.stop().await;break;}
        let _=brain.memory.purge_expired();
        let _=brain.memory.purge_household_expired();
        if let Ok(ids)=brain.memory.pending_household_cleanup(){
            if !ids.is_empty()&&brain.tasks.forget_sources(&ids,false).await.is_ok(){let _=brain.memory.complete_household_cleanup(&ids);}
        }
        if brain.foreground_busy(){continue;}
        let mut foreground=brain.foreground_generation.subscribe();
        let generation=*foreground.borrow();
        // Graph deletion/ingestion can progress even when the optional MLX
        // extractor is not installed. A missing backend is visible as blocked.
        if brain.config.memory.graphiti.enabled {
            if let Ok(signature)=brain.graphiti.configured_signature(){let _=brain.memory.set_graphiti_signature(&signature);}
            if let Some(job)=brain.memory.next_household_job(true).ok().flatten(){
                if brain.foreground_busy()||*foreground.borrow()!=generation{let _=brain.memory.release_household_job(&job);continue;}
                {
                    let client=&brain.graphiti;
                    let result=tokio::select!{value=client.apply(&job)=>Some(value),_=foreground.changed()=>None};
                    match result {
                        None=>{client.stop().await;let _=brain.memory.release_household_job(&job);},
                        Some(Ok(value))=>{let _=brain.memory.complete_household_graph_job(&job,&value);},
                        Some(Err(code))=>{let blocked=code.contains("configured")||code.contains("missing")||code.contains("disabled")||code.contains("mismatch")||code.contains("not_allowed")||code.contains("not_enabled");let _=brain.memory.fail_household_job(&job,&code,blocked);},
                    }
                }
                continue;
            }
        }
        let Some(script)=brain.memory_script.as_ref().filter(|p|p.is_file())else{continue};
        let model=brain.config_dir.join("models/memory-qwen3-4b");
        if !["config.json","model.safetensors","chat_template.jinja"].iter().all(|name|model.join(name).is_file()){continue;}
        if let Some(job)=brain.memory.next_household_job(false).ok().flatten(){
            if brain.foreground_busy()||*foreground.borrow()!=generation{let _=brain.memory.release_household_job(&job);continue;}
            let result=tokio::select!{value=extract(&brain,script,&model,&job.payload)=>Some(value),_=foreground.changed()=>None};
            match result {
                None=>{let _=brain.memory.release_household_job(&job);},
                Some(Ok(value))=>{
                    if *foreground.borrow()!=generation||!brain.accepting.load(Ordering::Acquire){let _=brain.memory.release_household_job(&job);continue;}
                    match serde_json::from_value::<HouseholdExtraction>(value){
                        Ok(extraction)=>{if brain.memory.complete_household_job(&job,&extraction).is_err(){let _=brain.memory.fail_household_job(&job,"source_commit_failed",false);}},
                        Err(_)=>{let _=brain.memory.fail_household_job(&job,"invalid_extraction_schema",false);},
                    }
                },
                Some(Err(code))=>{let _=brain.memory.fail_household_job(&job,&code,false);},
            }
            continue;
        }
        // Legacy jobs retain their original profile/privacy scope. They are not
        // imported into the new household diary as a side effect of migration.
        let Some(job)=brain.memory.next_memory_job().ok().flatten()else{continue};
        if brain.foreground_busy()||*foreground.borrow()!=generation{continue;}
        let payload=json!({"transcript":job.transcript});
        let result=tokio::select!{value=extract(&brain,script,&model,&payload)=>Some(value),_=foreground.changed()=>None};
        let Some(result)=result else{continue};
        if *foreground.borrow()!=generation||!brain.accepting.load(Ordering::Acquire){continue;}
        match result.and_then(|value|serde_json::from_value::<Extraction>(value).map_err(|_|"invalid_extraction_schema".into())) {
            Ok(extraction)=>{let _=brain.memory.complete_memory_job(&job.id,&extraction.facts);},
            Err(_)=>{let _=brain.memory.fail_memory_job(&job.id);},
        }
    }
}

async fn extract(brain:&Brain,script:&std::path::Path,model:&std::path::Path,payload:&Value)->Result<Value,String>{
    let python=brain.config.models.rma_qwen_python.as_ref().map(|p|p.as_os_str()).unwrap_or_else(||std::ffi::OsStr::new(&brain.config.models.python));
    let tmp=brain.config_dir.join("runtime/tmp");super::settings::private_dir(&tmp)?;
    #[cfg(target_os="macos")]
    let mut cmd={let mut command=Command::new("/usr/bin/nice");command.arg("-n").arg("10").arg(python);command};
    #[cfg(not(target_os="macos"))]
    let mut cmd=Command::new(python);
    cmd.arg("-B").arg(script).arg(model).env("TMPDIR",tmp).env("HF_HOME",brain.config_dir.join("runtime/cache/huggingface"))
        .env("HF_HUB_OFFLINE","1").env("TRANSFORMERS_OFFLINE","1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]cmd.process_group(0);
    let mut child=cmd.spawn().map_err(|_|"local_extractor_unavailable")?;
    let _owned=ExtractionProcess(child.id());
    let mut input=child.stdin.take().ok_or("local_extractor_stdin")?;
    let mut output=child.stdout.take().ok_or("local_extractor_stdout")?.take(131_073);
    let mut bytes=serde_json::to_vec(payload).map_err(|_|"local_extractor_input")?;bytes.push(b'\n');
    if bytes.len()>524_288{return Err("local_extractor_input_too_large".into());}
    timeout(Duration::from_secs(180),async {
        input.write_all(&bytes).await.map_err(|_|"local_extractor_write")?;drop(input);
        let mut bytes=Vec::new();
        let (status,_)=tokio::try_join!(child.wait(),output.read_to_end(&mut bytes)).map_err(|_|"local_extractor_failed")?;
        if !status.success()||bytes.len()>131_072{return Err("local_extractor_failed".to_owned());}
        serde_json::from_slice(&bytes).map_err(|_|"local_extractor_invalid_json".to_owned())
    }).await.map_err(|_|"local_extractor_timeout".to_owned())?
}

struct ExtractionProcess(Option<u32>);
impl Drop for ExtractionProcess{fn drop(&mut self){#[cfg(unix)]if let Some(pid)=self.0{unsafe{libc::kill(-(pid as i32),libc::SIGKILL);}}}}
