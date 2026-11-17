//! A single owned, lazy JSONL sidecar. No ambient listener or provider defaults.
//! SQLite validates every result against current source permissions/versions.
use std::{path::{Path,PathBuf},process::Stdio,time::Duration};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use tokio::{io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt,BufReader},process::{Child,ChildStdin,ChildStdout,Command},sync::Mutex,time::timeout};
use super::{GraphitiConfig,HouseholdJob,Store};

struct Process { child:Child,input:ChildStdin,output:BufReader<ChildStdout>,process_group:Option<u32> }

pub struct GraphitiClient {
    config:GraphitiConfig,
    default_script:PathBuf,
    process:Mutex<Option<Process>>,
}

impl GraphitiClient {
    pub fn new(config:&GraphitiConfig,default_script:impl AsRef<Path>)->Self{
        Self{config:config.clone(),default_script:default_script.as_ref().to_owned(),process:Mutex::new(None)}
    }

    pub async fn stop(&self){
        if let Some(mut process)=self.process.lock().await.take(){let _=process.child.kill().await;let _=process.child.wait().await;}
    }

    /// Includes model names, embedding dimensions, preprocessing options and
    /// pinned adapter version. A config edit cannot silently reuse old vectors.
    pub fn configured_signature(&self)->Result<String,String>{
        let path=self.config.config_file.as_deref().ok_or("graphiti_not_configured")?;
        let bytes=std::fs::read(path).map_err(|_|"graphiti_not_configured")?;
        if bytes.len()>32768{return Err("graphiti_configuration_too_large".into());}
        let mut hash=Sha256::new();hash.update(b"espbot-graphiti-0.31.2-v2\0");hash.update(bytes);
        Ok(format!("{:x}",hash.finalize()))
    }

    async fn spawn(&self)->Result<Process,String>{
        if !self.config.enabled{return Err("graphiti_disabled".into());}
        let script=self.config.script.as_deref().unwrap_or(&self.default_script);
        let config=self.config.config_file.as_deref().filter(|p|p.is_file()).ok_or("graphiti_not_configured")?;
        if !script.is_file(){return Err("graphiti_script_missing".into());}
        let python=self.config.python.as_deref().unwrap_or(Path::new("python3"));
        let mut command=Command::new(python);
        command.arg("-B").arg("-u").arg(script).arg(config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(unix)] command.process_group(0);
        let mut child=command.spawn().map_err(|_|"graphiti_worker_unavailable")?;
        let process_group=child.id();
        let input=child.stdin.take().ok_or("graphiti_stdin_missing")?;
        let output=BufReader::new(child.stdout.take().ok_or("graphiti_stdout_missing")?);
        Ok(Process{child,input,output,process_group})
    }

    pub async fn request(&self,mut payload:Value)->Result<Value,String>{
        if !self.config.enabled{return Err("graphiti_disabled".into());}
        let id=format!("graph_{}",uuid::Uuid::new_v4().simple());payload["id"]=json!(id);
        payload["engine_signature"]=json!(self.configured_signature()?);
        // Both the user's graph configuration and source policy must consent.
        payload["allow_cloud"]=json!(self.config.allow_cloud&&payload["allow_cloud"].as_bool().unwrap_or(false));
        let mut bytes=serde_json::to_vec(&payload).map_err(|_|"graphiti_request_encoding")?;bytes.push(b'\n');
        if bytes.len()>524_288{return Err("graphiti_request_too_large".into());}
        let mut guard=self.process.lock().await;
        if guard.is_none(){*guard=Some(self.spawn().await?);}
        // Move the owned process into this future. Cancellation drops it and
        // kills its group; an unfinished request cannot remain in the pool.
        let mut process=guard.take().ok_or("graphiti_worker_unavailable")?;
        let result=timeout(Duration::from_secs(190),async {
            process.input.write_all(&bytes).await.map_err(|_|"graphiti_write_failed".to_owned())?;
            process.input.flush().await.map_err(|_|"graphiti_write_failed".to_owned())?;
            let mut line=Vec::new();
            (&mut process.output).take(524_289).read_until(b'\n',&mut line).await.map_err(|_|"graphiti_read_failed".to_owned())?;
            if line.len()>524_288||line.last()!=Some(&b'\n'){return Err("graphiti_protocol_error".to_owned());}
            let value:Value=serde_json::from_slice(&line).map_err(|_|"graphiti_protocol_error".to_owned())?;
            if value["id"].as_str()!=Some(&id){return Err("graphiti_response_id_mismatch".to_owned());}
            if value["ok"].as_bool()!=Some(true){return Err(value["error"].as_str().filter(|s|s.len()<=80&&s.chars().all(|c|c.is_ascii_alphanumeric()||c=='_')).unwrap_or("graphiti_operation_failed").to_owned());}
            Ok(value["result"].clone())
        }).await.map_err(|_|"graphiti_timeout".to_owned()).and_then(|result|result);
        if result.is_ok(){*guard=Some(process);}else{let _=process.child.kill().await;let _=process.child.wait().await;}
        result
    }

    pub async fn health(&self)->Value{
        match self.request(json!({"op":"health"})).await{Ok(value)=>value,Err(code)=>json!({"ready":false,"configured":false,"state":code,"version":"0.31.2","db_reachable":null,"models_not_probed":true,"model_calls_verified":false})}
    }

    pub async fn apply(&self,job:&HouseholdJob)->Result<Value,String>{
        let mut payload=job.payload.clone();
        payload["op"]=json!(match job.kind.as_str(){"graph_delete"=>"delete","graph_ingest"=>"ingest",_=>return Err("invalid_graph_job".into())});
        payload["household_id"]=json!(job.household_id);payload["allow_cloud"]=json!(job.allow_cloud);
        self.request(payload).await
    }

    pub async fn recall(&self,store:&Store,query:&str,cloud:bool)->Result<Value,String>{
        store.set_graphiti_signature(&self.configured_signature()?)?;
        let settings=store.household_settings()?;
        let policy_before=store.household_policy_revision()?;
        let result=self.request(json!({"op":"search","household_id":settings["household_id"],"query":query,"allow_cloud":cloud&&settings["allow_cloud"].as_bool().unwrap_or(false)})).await?;
        if policy_before!=store.household_policy_revision()?{return Err("household_policy_changed".into());}
        store.validate_graph_recall(&result,cloud)
    }
}

impl Drop for Process {
    fn drop(&mut self){
        // Drop/cancellation owns the complete process group, including any
        // model client subprocess, and cannot leave a stale writer behind.
        #[cfg(unix)] if let Some(id)=self.process_group{unsafe{libc::kill(-(id as i32),libc::SIGKILL);}}
        let _=self.child.start_kill();
    }
}
