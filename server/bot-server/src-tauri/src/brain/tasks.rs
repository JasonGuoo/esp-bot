//! Durable Mac work. A task owns neither a microphone activation nor a display.
use std::{collections::HashMap, future::Future, path::PathBuf, pin::Pin, sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU32,Ordering}}, time::Duration};
use rusqlite::{params,Connection,OptionalExtension};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use tokio::sync::{watch,Semaphore};
use super::{EventSink,artifacts::{ArtifactStore,valid_id},presentation::{PresentationPlan,OutputLease,PlaybackReceipt}};

#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct TaskConfig{
    pub max_concurrent:usize,pub max_tasks:usize,pub max_asset_bytes:u64,pub timeout_seconds:u64,
    pub ffmpeg:String,pub ffprobe:String,pub python:String,pub media_script:Option<PathBuf>,
    pub allowed_media_hosts:Vec<String>,pub default_printer:Option<String>,pub print_monitor_seconds:u64,pub stroke_data_path:Option<PathBuf>,
}
impl Default for TaskConfig{fn default()->Self{Self{max_concurrent:2,max_tasks:10_000,max_asset_bytes:64*1024*1024,timeout_seconds:300,
    ffmpeg:"ffmpeg".into(),ffprobe:"ffprobe".into(),python:"python3".into(),media_script:None,allowed_media_hosts:Vec::new(),default_printer:None,print_monitor_seconds:120,stroke_data_path:None}}}
impl TaskConfig{pub fn validate(&self)->Result<(),String>{
    if !(1..=8).contains(&self.max_concurrent)||!(100..=100_000).contains(&self.max_tasks)||!(65_536..=512*1024*1024).contains(&self.max_asset_bytes)
        ||!(15..=1800).contains(&self.timeout_seconds)||!(5..=600).contains(&self.print_monitor_seconds){return Err("task_config_limits".into());}
    if self.ffmpeg.is_empty()||self.ffprobe.is_empty()||self.python.is_empty()||self.media_script.as_ref().is_some_and(|p|!p.is_absolute())||self.stroke_data_path.as_ref().is_some_and(|p|!p.is_absolute()){
        return Err("task_config_paths".into());}
    if self.allowed_media_hosts.len()>32||self.allowed_media_hosts.iter().any(|s|s.is_empty()||s.len()>253||s.contains('/')||s.contains(':')||s.contains('*')||s.chars().any(char::is_whitespace)){
        return Err("task_media_hosts".into());}Ok(())}}

#[derive(Clone,Copy,Debug,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="snake_case")]
pub enum TaskState{Queued,Running,AwaitingInput,Succeeded,Partial,Failed,CancelRequested,Cancelled,Unknown}
impl TaskState{pub fn as_str(self)->&'static str{match self{Self::Queued=>"queued",Self::Running=>"running",Self::AwaitingInput=>"awaiting_input",Self::Succeeded=>"succeeded",Self::Partial=>"partial",Self::Failed=>"failed",Self::CancelRequested=>"cancel_requested",Self::Cancelled=>"cancelled",Self::Unknown=>"unknown"}}
    fn parse(s:&str)->Self{match s{"queued"=>Self::Queued,"running"=>Self::Running,"awaiting_input"=>Self::AwaitingInput,"succeeded"=>Self::Succeeded,"partial"=>Self::Partial,"failed"=>Self::Failed,"cancel_requested"=>Self::CancelRequested,"cancelled"=>Self::Cancelled,_=>Self::Unknown}}}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTask{
    pub household_id:String,pub daily_conversation_id:String,pub conversation_turn_id:String,
    pub capability:String,pub input:Value,pub intent_key:String,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct TaskRecord{
    pub task_id:String,pub household_id:String,pub daily_conversation_id:String,pub conversation_turn_id:String,
    pub capability:String,pub input:Value,pub intent_key:String,pub state:TaskState,pub stage:String,
    pub result:Value,pub attempt:u32,pub created_at_ms:i64,pub updated_at_ms:i64,pub authorized_effect:bool,
    pub task_revision:u64,pub result_revision:u64,
}
#[derive(Clone,Debug)]
pub struct ReadyDelivery{
    pub task:TaskRecord,pub delivery_id:String,pub delivery_revision:i64,pub kind:String,pub promise_full:bool,
}
#[derive(Clone,Debug)]
pub struct InboxView{
    pub delivery_id:String,pub revision:i64,pub state:String,pub kind:String,pub promise_full:bool,
    pub summary:String,pub result_text:String,pub tell_me:bool,pub private_scope:bool,pub receipts:String,
}
pub struct TaskResult{pub state:TaskState,pub summary:String,pub artifact_ids:Vec<String>,pub data:Value}
impl TaskResult{pub fn success(summary:impl Into<String>,artifact_ids:Vec<String>,data:Value)->Self{Self{state:TaskState::Succeeded,summary:summary.into(),artifact_ids,data}}}
pub type TaskFuture=Pin<Box<dyn Future<Output=Result<TaskResult,String>>+Send>>;
pub type TaskHandler=Arc<dyn Fn(TaskContext)->TaskFuture+Send+Sync>;
#[derive(Clone)]
pub struct TaskContext{pub service:Arc<TaskService>,pub task:TaskRecord,pub cancelled:watch::Receiver<bool>}
impl TaskContext{
    pub fn is_cancelled(&self)->bool{*self.cancelled.borrow()}
    pub fn check_cancelled(&self)->Result<(),String>{if self.is_cancelled(){Err("cancelled".into())}else{Ok(())}}
    pub fn progress(&self,stage:&str,payload:Value)->Result<(),String>{self.service.progress(&self.task.task_id,stage,payload)}
}

pub struct TaskService{
    pub config:TaskConfig,pub artifacts:Arc<ArtifactStore>,pub root:PathBuf,
    db:Mutex<Connection>,sink:EventSink,handlers:Mutex<HashMap<String,TaskHandler>>,
    accepting:AtomicBool,redacting:AtomicBool,slots:Arc<Semaphore>,active:Mutex<HashMap<String,watch::Sender<bool>>>,
    scheduler:Mutex<Option<tokio::task::JoinHandle<()>>>,reconciler:Mutex<Option<tokio::task::JoinHandle<()>>>,runners:Mutex<HashMap<String,tokio::task::JoinHandle<()>>>,
    /// Test fault: the next durable result write fails before commit.
    fail_result_writes:AtomicU32,
    persist_pending:Mutex<Vec<UnstoredResult>>,
    hint_pending:AtomicBool,
}
impl TaskService{
    pub fn open(root:PathBuf,config:TaskConfig,sink:EventSink)->Result<Arc<Self>,String>{
        config.validate()?;super::settings::private_dir(&root)?;
        // A crashed renderer must not leave an untracked copy of private
        // captions/media behind. Producers are owned by this service process.
        let temporary=root.join("temporary");if temporary.exists(){std::fs::remove_dir_all(&temporary).map_err(|e|format!("task_temporary_cleanup: {e}"))?;}
        let path=root.join("tasks.sqlite3");let db=Connection::open(&path).map_err(db_error)?;
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600)).map_err(|e|e.to_string())?;}
        db.busy_timeout(Duration::from_secs(2)).map_err(db_error)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
          CREATE TABLE IF NOT EXISTS tasks(
           id TEXT PRIMARY KEY,household_id TEXT NOT NULL,daily_id TEXT NOT NULL,turn_id TEXT NOT NULL,
           capability TEXT NOT NULL,input_json TEXT NOT NULL,intent_key TEXT NOT NULL,state TEXT NOT NULL,
           stage TEXT NOT NULL,result_json TEXT NOT NULL DEFAULT '{}',attempt INTEGER NOT NULL DEFAULT 0,
           created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,authorized_effect INTEGER NOT NULL,
           UNIQUE(household_id,intent_key));
          CREATE INDEX IF NOT EXISTS task_queue ON tasks(state,created_at_ms);
          CREATE TABLE IF NOT EXISTS task_attempts(id TEXT PRIMARY KEY,task_id TEXT NOT NULL REFERENCES tasks(id),attempt INTEGER NOT NULL,started_at_ms INTEGER NOT NULL,ended_at_ms INTEGER,state TEXT NOT NULL,result_json TEXT NOT NULL DEFAULT '{}',UNIQUE(task_id,attempt));
          CREATE TABLE IF NOT EXISTS task_events(
           sequence INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL UNIQUE,task_id TEXT NOT NULL REFERENCES tasks(id),
           kind TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS presentations(id TEXT NOT NULL,revision INTEGER NOT NULL,plan_json TEXT NOT NULL,PRIMARY KEY(id,revision));
          CREATE TABLE IF NOT EXISTS output_leases(id TEXT PRIMARY KEY,lease_json TEXT NOT NULL,expires_at_ms INTEGER NOT NULL,revoked INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS playbacks(id TEXT PRIMARY KEY,presentation_id TEXT NOT NULL,revision INTEGER NOT NULL,lease_id TEXT NOT NULL,last_seq INTEGER NOT NULL DEFAULT -1,state TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS playback_receipts(receipt_id TEXT PRIMARY KEY,playback_id TEXT NOT NULL,sequence INTEGER NOT NULL,receipt_json TEXT NOT NULL,received_at_ms INTEGER NOT NULL,UNIQUE(playback_id,sequence));
          CREATE TABLE IF NOT EXISTS source_receipts(id TEXT PRIMARY KEY,source_url TEXT NOT NULL,evidence_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS invalidated_sources(turn_id TEXT PRIMARY KEY,reason TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS presentation_expiry(id TEXT PRIMARY KEY,expires_at_ms INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS expired_presentations(id TEXT PRIMARY KEY);
          CREATE TABLE IF NOT EXISTS presentation_sources(presentation_id TEXT NOT NULL,source_id TEXT NOT NULL,context_turn_id TEXT NOT NULL,PRIMARY KEY(presentation_id,source_id));
          CREATE INDEX IF NOT EXISTS presentation_source_lookup ON presentation_sources(source_id);
          UPDATE output_leases SET revoked=1;
          UPDATE task_attempts SET state='interrupted',ended_at_ms=CAST(strftime('%s','now') AS INTEGER)*1000 WHERE ended_at_ms IS NULL;").map_err(db_error)?;
        migrate_delivery(&db)?;
        settle_interrupted(&db,"recovery_required")?;
        release_offered_rows(&db)?;
        let hint=inbox_hint_due(&db)?;
        let artifacts=Arc::new(ArtifactStore::open(root.join("artifacts"),config.max_asset_bytes)?);
        let service=Arc::new(Self{slots:Arc::new(Semaphore::new(config.max_concurrent)),config,artifacts,root,db:Mutex::new(db),sink,
            handlers:Mutex::new(HashMap::new()),accepting:AtomicBool::new(false),redacting:AtomicBool::new(false),active:Mutex::new(HashMap::new()),scheduler:Mutex::new(None),reconciler:Mutex::new(None),runners:Mutex::new(HashMap::new()),
            fail_result_writes:AtomicU32::new(0),persist_pending:Mutex::new(Vec::new()),hint_pending:AtomicBool::new(hint)});
        service.purge_expired()?;Ok(service)
    }

    pub fn register_handler(&self,capability:&str,handler:TaskHandler)->Result<(),String>{
        if capability.is_empty()||capability.len()>80{return Err("task_capability_invalid".into());}
        self.handlers.lock().map_err(|_|"task_handlers_lock")?.insert(capability.into(),handler);Ok(())
    }

    pub async fn start(self:&Arc<Self>)->Result<(),String>{
        if self.accepting.swap(true,Ordering::AcqRel){return Ok(());}
        let weak=Arc::downgrade(self);
        let handle=tokio::spawn(async move{loop{
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(service)=weak.upgrade() else{break};
            if !service.accepting.load(Ordering::Acquire){break;}
            if service.redacting.load(Ordering::Acquire){continue;}
            if let Ok(mut runners)=service.runners.lock(){runners.retain(|_,r|!r.is_finished());}
            let Ok(records)=service.queued() else{continue};
            for task in records{
                let Ok(permit)=Arc::clone(&service.slots).try_acquire_owned() else{break};
                let Ok(mut active)=service.active.lock() else{continue};
                if active.contains_key(&task.task_id){continue;}
                if service.claim(&task.task_id).unwrap_or(false){
                    let (cancelled,receiver)=watch::channel(false);
                    active.insert(task.task_id.clone(),cancelled);
                    let svc=Arc::clone(&service);let id=task.task_id.clone();
                    let handle=tokio::spawn(async move{let _permit=permit;svc.run_task(&id,receiver).await;});
                    if let Ok(mut runners)=service.runners.lock(){runners.insert(task.task_id.clone(),handle);}
                }
            }
        }});
        *self.scheduler.lock().map_err(|_|"task_scheduler_lock")?=Some(handle);
        let weak=Arc::downgrade(self);
        let reconciler=tokio::spawn(async move{loop{
            tokio::time::sleep(Duration::from_secs(15)).await;let Some(service)=weak.upgrade() else{break};
            if !service.accepting.load(Ordering::Acquire){break;}
            if service.redacting.load(Ordering::Acquire){continue;}
            let _=service.purge_expired();
            service.retry_persist_pending();
            let Ok(records)=service.pending_prints() else{continue};
            for task in records{
                let active=service.active.lock().map(|a|a.contains_key(&task.task_id)).unwrap_or(true);if active{continue;}
                let _=super::capabilities::reconcile_print(&service,&task).await;
            }
        }});
        *self.reconciler.lock().map_err(|_|"task_scheduler_lock")?=Some(reconciler);Ok(())
    }

    pub async fn stop(&self){
        self.accepting.store(false,Ordering::Release);
        let scheduler=self.scheduler.lock().ok().and_then(|mut h|h.take());
        // abort is cooperative: wait until the scheduler has really stopped
        // registering runners before taking the runner collection below.
        if let Some(handle)=scheduler{handle.abort();let _=handle.await;}
        let reconciler=self.reconciler.lock().ok().and_then(|mut h|h.take());if let Some(handle)=reconciler{handle.abort();let _=handle.await;}
        // Stopping the application revokes all output but is not proof that a
        // remote print was undone. Interrupted effect attempts recover unknown.
        let _=self.revoke_outputs(None);
        let runners=self.runners.lock().map(|mut hs|std::mem::take(&mut *hs)).unwrap_or_default();
        for h in runners.values(){h.abort();}for(_,h)in runners{let _=h.await;}
        if let Ok(mut active)=self.active.lock(){active.clear();}
        if let Ok(db)=self.db.lock(){
            let _=settle_interrupted(&db,"service_stopped");
            let _=release_offered_rows(&db);
            let _=db.execute("UPDATE task_attempts SET state='interrupted',ended_at_ms=?1 WHERE ended_at_ms IS NULL",[now()]);
        }
    }

    /// effect_authorized is a host decision, never a deserialized LLM field.
    pub fn submit(self:&Arc<Self>,mut request:NewTask,effect_authorized:bool)->Result<TaskRecord,String>{
        for id in [&request.household_id,&request.daily_conversation_id,&request.conversation_turn_id,&request.intent_key]{valid_id(id)?;}
        if !matches!(request.capability.as_str(),"print.submit"|"media.prepare"|"presentation.render"|"agent.work")
            && !self.handlers.lock().map_err(|_|"task_handlers_lock")?.contains_key(&request.capability){return Err("task_capability_unsupported".into());}
        if request.capability=="print.submit"&&!effect_authorized{return Err("print_requires_explicit_intent".into());}
        if serde_json::to_vec(&request.input).map_err(|e|e.to_string())?.len()>262_144{return Err("task_input_limit".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        if self.redacting.load(Ordering::Acquire){return Err("task_source_update_in_progress".into());}
        self.inherit_artifact_sources(&tx,&mut request.input)?;
        let input=serde_json::to_string(&request.input).map_err(|e|e.to_string())?;if input.len()>262_144{return Err("task_input_limit".into());}
        if source_invalidated(&tx,&request.conversation_turn_id)?{return Err("task_source_invalidated".into());}
        if let Some(sources)=request.input.get("source_turn_ids").and_then(Value::as_array){
            if sources.len()>2048{return Err("task_source_limit".into());}for source in sources{let source=source.as_str().ok_or("task_source_invalid")?;valid_id(source)?;if source_invalidated(&tx,source)?{return Err("task_source_invalidated".into());}}
        }
        if let Some(existing)=tx.query_row("SELECT id FROM tasks WHERE household_id=?1 AND intent_key=?2",params![request.household_id,request.intent_key],|r|r.get::<_,String>(0)).optional().map_err(db_error)?{
            let record=query_task(&tx,&existing)?;
            if record.capability!=request.capability||record.input!=request.input||record.conversation_turn_id!=request.conversation_turn_id{return Err("task_intent_conflict".into());}
            return Ok(record);
        }
        prune_inbox(&tx)?;
        let count:i64=tx.query_row("SELECT count(*) FROM tasks",[],|r|r.get(0)).map_err(db_error)?;
        if count>=self.config.max_tasks as i64{return Err("task_capacity_reached".into());}
        let id=format!("task_{}",uuid::Uuid::new_v4().simple());let now=now();
        tx.execute("INSERT INTO tasks(id,household_id,daily_id,turn_id,capability,input_json,intent_key,state,stage,created_at_ms,updated_at_ms,authorized_effect) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued','accepted',?8,?8,?9)",
            params![id,request.household_id,request.daily_conversation_id,request.conversation_turn_id,request.capability,input,request.intent_key,now,effect_authorized]).map_err(db_error)?;
        append_event(&tx,&id,"task.accepted",&json!({"capability":request.capability}))?;
        let record=query_task(&tx,&id)?;tx.commit().map_err(db_error)?;drop(db);
        self.emit(&record,"task.accepted",json!({"stage":"accepted"}));Ok(record)
    }

    pub fn get(&self,id:&str)->Result<TaskRecord,String>{valid_id(id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;query_task(&db,id)}
    pub fn task_for_delivery(&self,delivery_id:&str)->Result<TaskRecord,String>{
        valid_id(delivery_id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let task_id:String=db.query_row("SELECT task_id FROM delivery_inbox WHERE delivery_id=?1",[delivery_id],|r|r.get(0)).map_err(db_error)?;
        query_task(&db,&task_id)
    }
    /// Resume the same intent after the user supplies missing parameters. An
    /// accepted or unknown CUPS submission can only be reconciled, never resumed.
    pub fn resume(self:&Arc<Self>,id:&str,patch:Value,effect_authorized:bool)->Result<TaskRecord,String>{
        valid_id(id)?;let patch=patch.as_object().ok_or("task_input_patch_invalid")?;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;let task=query_task(&tx,id)?;
        if self.redacting.load(Ordering::Acquire)||source_invalidated(&tx,&task.conversation_turn_id)?{return Err("task_source_invalidated".into());}
        if task.state!=TaskState::AwaitingInput||task.result["submission_started"]==true||task.result.pointer("/data/submission_started").and_then(Value::as_bool).unwrap_or(false){return Err("task_cannot_resume".into());}
        if task.capability=="print.submit"&&!task.authorized_effect&&!effect_authorized{return Err("print_requires_explicit_intent".into());}
        let mut input=task.input.clone();if !input.is_object(){return Err("task_input_invalid".into());}
        for(key,value)in patch{if key!="source_turn_ids"{input[key]=value.clone();}}
        if let Some(additional)=patch.get("source_turn_ids"){
            let mut sources=input["source_turn_ids"].as_array().cloned().unwrap_or_default();
            for source in additional.as_array().ok_or("task_source_invalid")?{let source=source.as_str().ok_or("task_source_invalid")?;valid_id(source)?;
                if source_invalidated(&tx,source)?{return Err("task_source_invalidated".into());}if !sources.iter().any(|v|v.as_str()==Some(source)){sources.push(json!(source));}}
            if sources.len()>2048{return Err("task_source_limit".into());}input["source_turn_ids"]=json!(sources);
        }
        if task.capability=="print.submit"&&!patch.contains_key("artifact_id")&&!patch.contains_key("visual"){
            if let Some(id)=task.result["artifact_id"].as_str(){input["artifact_id"]=json!(id);}
        }
        self.inherit_artifact_sources(&tx,&mut input)?;
        let serialized=serde_json::to_string(&input).map_err(|e|e.to_string())?;if serialized.len()>262_144{return Err("task_input_limit".into());}
        tx.execute("UPDATE tasks SET input_json=?2,state='queued',stage='resumed',result_json='{}',authorized_effect=?3,updated_at_ms=?4,task_revision=task_revision+1 WHERE id=?1",params![id,serialized,task.authorized_effect||effect_authorized,now()]).map_err(db_error)?;
        tx.execute("UPDATE delivery_inbox SET state='invalidated',delivery_revision=delivery_revision+1 WHERE task_id=?1 AND state NOT IN ('invalidated','delivered')",[id]).map_err(db_error)?;
        append_event(&tx,id,"task.resumed",&json!({"attempt_next":task.attempt+1}))?;let record=query_task(&tx,id)?;tx.commit().map_err(db_error)?;drop(db);
        self.publish_hint();
        self.emit(&record,"task.state",json!({"state":"queued","stage":"resumed"}));Ok(record)
    }
    fn inherit_artifact_sources(&self,db:&Connection,input:&mut Value)->Result<(),String>{
        let Some(id)=input["artifact_id"].as_str() else{return Ok(())};
        let asset=self.artifacts.verify(id)?;
        let mut sources=input["source_turn_ids"].as_array().cloned().unwrap_or_default();
        if let Some(id)=asset.source["conversation_turn_id"].as_str().or_else(||asset.source.pointer("/context/conversation_turn_id").and_then(Value::as_str)){sources.push(json!(id));}
        if let Some(task_id)=asset.source["task_id"].as_str(){
            let origin=query_task(db,task_id)?;if source_invalidated(db,&origin.conversation_turn_id)?{return Err("artifact_source_invalidated".into());}
            sources.push(json!(origin.conversation_turn_id));if let Some(ids)=origin.input["source_turn_ids"].as_array(){sources.extend(ids.iter().cloned());}
        }
        if let Some(presentation)=asset.source["presentation_id"].as_str(){
            let mut statement=db.prepare("SELECT source_id FROM presentation_sources WHERE presentation_id=?1").map_err(db_error)?;
            let rows=statement.query_map([presentation],|r|r.get::<_,String>(0)).map_err(db_error)?;for row in rows{sources.push(json!(row.map_err(db_error)?));}
        }
        let mut ids=sources.iter().map(|s|s.as_str().map(str::to_owned).ok_or_else(||"task_source_invalid".to_owned())).collect::<Result<Vec<_>,_>>()?;
        ids.sort();ids.dedup();if ids.len()>2048{return Err("task_source_limit".into());}
        for id in &ids{valid_id(id)?;if source_invalidated(db,id)?{return Err("artifact_source_invalidated".into());}}
        input["source_turn_ids"]=json!(ids);Ok(())
    }
    pub fn list(&self,household_id:Option<&str>,state:Option<&str>,limit:usize)->Result<Vec<TaskRecord>,String>{
        if let Some(id)=household_id{valid_id(id)?;}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let mut statement=db.prepare("SELECT id FROM tasks WHERE (?1 IS NULL OR household_id=?1) AND (?2 IS NULL OR state=?2) ORDER BY created_at_ms DESC LIMIT ?3").map_err(db_error)?;
        let rows=statement.query_map(params![household_id,state,limit.clamp(1,200) as i64],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;ids.iter().map(|id|query_task(&db,id)).collect()
    }
    /// One ready result for this household and device activation. The activation
    /// is stored on the task input when the turn submits it, so this does not
    /// scan another household's newest tasks.
    pub fn ready_for_activation(&self,household_id:&str,activation_id:&str)->Result<Option<TaskRecord>,String>{
        valid_id(household_id)?;valid_id(activation_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let mut statement=db.prepare("SELECT id FROM tasks WHERE household_id=?1 AND json_extract(input_json,'$.activation_id')=?2 AND state IN ('succeeded','partial','failed','cancelled','unknown','awaiting_input') AND (json_extract(result_json,'$.lead_spoken_state') IS NULL OR json_extract(result_json,'$.lead_spoken_state') != state) ORDER BY updated_at_ms,id LIMIT 4").map_err(db_error)?;
        let rows=statement.query_map(params![household_id,activation_id],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        drop(statement);
        for id in ids {
            let task=query_task(&db,&id)?;
            if !source_invalidated(&db,&task.conversation_turn_id)? { return Ok(Some(task)); }
        }
        Ok(None)
    }
    pub fn events(&self,id:&str,after:u64,limit:usize)->Result<Vec<Value>,String>{
        valid_id(id)?;if after>i64::MAX as u64{return Err("task_event_sequence_invalid".into());}let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let mut statement=db.prepare("SELECT sequence,event_id,kind,payload_json,created_at_ms FROM task_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(db_error)?;
        let rows=statement.query_map(params![id,after as i64,limit.clamp(1,200) as i64],|r|{let payload:String=r.get(3)?;
            Ok(json!({"sequence":r.get::<_,i64>(0)?,"event_id":r.get::<_,String>(1)?,"task_id":id,"kind":r.get::<_,String>(2)?,"payload":serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),"recorded_at_ms":r.get::<_,i64>(4)?}))}).map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }
    pub fn progress(&self,id:&str,stage:&str,payload:Value)->Result<(),String>{
        if stage.len()>80||serde_json::to_vec(&payload).map_err(|e|e.to_string())?.len()>65_536{return Err("task_progress_limit".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let task=query_task(&tx,id)?;
        // A slower concurrent CUPS query may still report queued/unknown after
        // another query has persisted an authoritative terminal job receipt.
        if confirmed_print_terminal(&task).is_some(){return Ok(());}
        let event=json!({"stage":stage,"evidence":if task.result["content_deleted"]==true{json!({})}else{payload}});
        let serialized=serde_json::to_string(&event).map_err(|e|e.to_string())?;
        let previous:Option<String>=tx.query_row("SELECT payload_json FROM task_events WHERE task_id=?1 AND kind='task.progress' ORDER BY sequence DESC LIMIT 1",[id],|r|r.get(0)).optional().map_err(db_error)?;
        if task.stage==stage&&previous.as_deref()==Some(serialized.as_str()){return Ok(());}
        // Effect evidence remains valid after cancellation. A late receipt
        // never reacquires a device lease or changes a different task.
        tx.execute("UPDATE tasks SET stage=?2,updated_at_ms=?3,task_revision=task_revision+1 WHERE id=?1",params![id,stage,now()]).map_err(db_error)?;
        append_event(&tx,id,"task.progress",&event)?;tx.commit().map_err(db_error)?;drop(db);
        self.emit(&task,"task.progress",json!({"stage":stage}));Ok(())
    }
    pub fn checkpoint(&self,id:&str,data:Value)->Result<(),String>{
        if serde_json::to_vec(&data).map_err(|e|e.to_string())?.len()>131_072{return Err("task_result_limit".into());}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let current=query_task(&db,id)?;let mut data=data;
        if confirmed_print_terminal(&current).is_some(){return Ok(());}
        preserve_cancel_request(&current.result,&mut data);
        let data=if current.result["content_deleted"]==true{effect_tombstone(&data)}else{data};
        db.execute("UPDATE tasks SET result_json=?2,updated_at_ms=?3,task_revision=task_revision+1 WHERE id=?1",params![id,serde_json::to_string(&data).map_err(|e|e.to_string())?,now()]).map_err(db_error)?;Ok(())
    }
    pub fn bind_sources(&self,id:&str,turn_ids:&[String])->Result<(),String>{
        valid_id(id)?;if turn_ids.len()>2048{return Err("task_source_limit".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;let task=query_task(&tx,id)?;
        if self.redacting.load(Ordering::Acquire)||task.state!=TaskState::Running||source_invalidated(&tx,&task.conversation_turn_id)?{return Err("task_source_invalidated".into());}
        let mut input=task.input.clone();let mut sources=input["source_turn_ids"].as_array().cloned().unwrap_or_default();
        for id in turn_ids{valid_id(id)?;if source_invalidated(&tx,id)?{return Err("task_source_invalidated".into());}if !sources.iter().any(|s|s.as_str()==Some(id.as_str())){sources.push(json!(id));}}
        if sources.len()>2048{return Err("task_source_limit".into());}input["source_turn_ids"]=json!(sources);
        let serialized=serde_json::to_string(&input).map_err(|e|e.to_string())?;if serialized.len()>262_144{return Err("task_input_limit".into());}
        tx.execute("UPDATE tasks SET input_json=?2,updated_at_ms=?3 WHERE id=?1",params![id,serialized,now()]).map_err(db_error)?;
        append_event(&tx,id,"task.sources_bound",&json!({"source_count":sources.len()}))?;tx.commit().map_err(db_error)?;Ok(())
    }
    pub(crate) fn begin_effect(&self,id:&str,data:Value)->Result<(),String>{
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;let task=query_task(&tx,id)?;
        if self.redacting.load(Ordering::Acquire)||source_invalidated(&tx,&task.conversation_turn_id)?||task.state!=TaskState::Running||!task.authorized_effect{return Err("print_effect_revoked".into());}
        if task.result["submission_started"]==true{return Err("print_effect_already_started".into());}
        tx.execute("UPDATE tasks SET result_json=?2,stage='submitting',updated_at_ms=?3 WHERE id=?1",params![id,serde_json::to_string(&data).map_err(|e|e.to_string())?,now()]).map_err(db_error)?;
        append_event(&tx,id,"task.progress",&json!({"stage":"submitting","adapter":"cups"}))?;tx.commit().map_err(db_error)?;drop(db);
        self.emit(&task,"task.progress",json!({"stage":"submitting"}));Ok(())
    }
    pub async fn cancel(self:&Arc<Self>,id:&str)->Result<TaskRecord,String>{
        valid_id(id)?;
        self.revoke_task_outputs(id)?;
        // The scheduler claims/registers work under active -> db. Use that same
        // order so a queued task cannot cross its effect boundary between our
        // running check and cancellation. Read the current result in the same
        // transaction: a concurrent CUPS checkpoint may already contain a job ID.
        let (task,running,external_started)={
            let active=self.active.lock().map_err(|_|"task_active_lock")?;
            let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
            let task=query_task(&tx,id)?;
            if matches!(task.state,TaskState::Succeeded|TaskState::Failed|TaskState::Cancelled){return Ok(task);}
            let running=active.get(id).cloned();
            let external_started=task.capability=="print.submit"&&(task.result["submission_started"]==true
                ||task.result.pointer("/data/submission_started").and_then(Value::as_bool)==Some(true)
                ||task.result["job_id"].is_string()||task.result.pointer("/data/job_id").is_some_and(Value::is_string));
            let state=if running.is_some()||external_started{TaskState::CancelRequested}else{TaskState::Cancelled};
            let stage=if state==TaskState::CancelRequested{"cancel_requested"}else{"cancelled"};
            let mut result=task.result.clone();if !result.is_object(){result=json!({});}result["cancel_requested"]=json!(true);
            let revision=store_terminal_inbox(&tx,&task,state,&result)?;
            tx.execute("UPDATE tasks SET state=?2,stage=?3,result_json=?4,updated_at_ms=?5,task_revision=task_revision+1,result_revision=?6 WHERE id=?1",
                params![id,state.as_str(),stage,serde_json::to_string(&result).map_err(|e|e.to_string())?,now(),revision]).map_err(db_error)?;
            append_event(&tx,id,"task.state",&json!({"state":state,"stage":stage,"task_revision":task.task_revision.saturating_add(1),"result_revision":revision}))?;
            let task=query_task(&tx,id)?;tx.commit().map_err(db_error)?;
            (task,running,external_started)
        };
        self.publish_hint();
        self.emit(&task,"task.state",json!({"state":task.state,"stage":task.stage}));
        if let Some(cancel)=running{cancel.send_replace(true);}
        else if external_started{super::capabilities::cancel_print(self,&task).await?;}
        self.get(id)
    }
    pub async fn cancel_all(self:&Arc<Self>)->Result<(),String>{
        let ids={let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT id FROM tasks WHERE state IN ('queued','running','awaiting_input')").map_err(db_error)?;
            let rows=statement.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        for id in ids{self.cancel(&id).await?;}Ok(())}
    pub async fn reconcile(self:&Arc<Self>,id:&str)->Result<TaskRecord,String>{
        let task=self.get(id)?;
        if task.capability!="print.submit"{return Err("task_reconcile_unsupported".into());}
        if task.result["submission_started"]!=true&&task.result.pointer("/data/submission_started").and_then(Value::as_bool)!=Some(true){return Err("print_attempt_missing".into());}
        if self.active.lock().map_err(|_|"task_active_lock")?.contains_key(id){return Err("print_reconciliation_already_running".into());}
        super::capabilities::reconcile_print(self,&task).await?;self.get(id)
    }
    pub fn set_state(&self,id:&str,mut state:TaskState,stage:&str,result:Option<Value>)->Result<(),String>{
        if deliverable(state) {
            let faults=self.fail_result_writes.load(Ordering::Acquire);
            if faults>0 && self.fail_result_writes.compare_exchange(faults,faults-1,Ordering::AcqRel,Ordering::Acquire).is_ok() {
                self.note_unstored(id,None,"",None);
                return Err("task_store_full".into());
            }
        }
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let task=query_task(&tx,id)?;let mut result=result.unwrap_or_else(||task.result.clone());
        if confirmed_print_terminal(&task).is_some_and(|confirmed|confirmed!=state){return Ok(());}
        preserve_cancel_request(&task.result,&mut result);
        // Completion can race an explicit stop after run_task's last snapshot.
        // Make the final decision while holding the database transaction.
        let effect_started=task.capability=="print.submit"&&(task.result["submission_started"]==true
            ||task.result.pointer("/data/submission_started").and_then(Value::as_bool)==Some(true));
        let stage=if !effect_started&&matches!(task.state,TaskState::CancelRequested|TaskState::Cancelled)
            &&!matches!(state,TaskState::CancelRequested|TaskState::Cancelled){
            state=TaskState::Cancelled;if !result.is_object(){result=json!({});}result["summary"]=json!("任务已取消。");"cancelled"
        }else if effect_started&&result["cancel_requested"]==true
            &&matches!(state,TaskState::Queued|TaskState::Running|TaskState::AwaitingInput|TaskState::Partial){
            state=TaskState::CancelRequested;"awaiting_cancel_receipt"
        }else{stage};
        let result=if task.result["content_deleted"]==true{effect_tombstone(&result)}else{result};
        let changed=task.state!=state||task.stage!=stage||task.result["summary"]!=result["summary"]||task.result["artifact_ids"]!=result["artifact_ids"];
        let data=serde_json::to_string(&result).map_err(|e|e.to_string())?;if data.len()>262_144{return Err("task_result_limit".into());}
        let revision=if changed && deliverable(state) {store_terminal_inbox(&tx,&task,state,&result)?} else {task.result_revision as i64};
        let task_revision=if changed {task.task_revision.saturating_add(1)} else {task.task_revision};
        tx.execute("UPDATE tasks SET state=?2,stage=?3,result_json=?4,updated_at_ms=?5,task_revision=?6,result_revision=?7 WHERE id=?1",params![id,state.as_str(),stage,data,now(),task_revision as i64,revision as i64]).map_err(db_error)?;
        if changed{append_event(&tx,id,"task.state",&json!({"state":state,"stage":stage,"task_revision":task_revision,"result_revision":revision}))?;}
        let hint=inbox_hint_due(&tx)?;
        tx.commit().map_err(db_error)?;drop(db);
        self.hint_pending.store(hint,Ordering::Release);
        self.clear_unstored(id);
        if changed{self.emit(&task,"task.state",json!({"state":state,"stage":stage,"task_revision":task_revision,"result_revision":revision}));}Ok(())
    }
    async fn commit_state_retry(&self,id:&str,state:TaskState,stage:&str,result:Option<Value>){
        for attempt in 0..3 {
            match self.set_state(id,state,stage,result.clone()) {
                Ok(())=>return,
                Err(error) if attempt<2 && storage_retryable(&error) => tokio::time::sleep(Duration::from_millis(20*(attempt as u64+1))).await,
                Err(_)=>{self.note_unstored(id,Some(state.as_str()),stage,result);return;}
            }
        }
    }
    /// Writes that failed three times are tried again from the task clock.
    pub fn retry_persist_pending(&self){
        let batch=self.persist_pending.lock().map(|mut pending|std::mem::take(&mut *pending)).unwrap_or_default();
        let mut again=Vec::new();
        for item in batch {
            let Some(state)=item.state.as_deref() else { again.push(item); continue; };
            if self.set_state(&item.id,TaskState::parse(state),&item.stage,item.result.clone()).is_err() {
                again.push(item);
            }
        }
        if let Ok(mut pending)=self.persist_pending.lock(){
            for item in again {
                if pending.len()>=super::task_delivery::MAX_PERSIST_PENDING {break;}
                if pending.iter().any(|have|have.id==item.id) {continue;}
                pending.push(item);
            }
        }
    }
    pub fn fail_next_result_write(&self){self.fail_result_writes.fetch_add(1,Ordering::AcqRel);}
    pub fn persist_pending(&self)->Vec<String>{self.persist_pending.lock().map(|v|v.iter().map(|item|item.id.clone()).collect()).unwrap_or_default()}
    pub fn hint_pending(&self)->bool{self.hint_pending.load(Ordering::Acquire)}
    fn note_unstored(&self,id:&str,state:Option<&str>,stage:&str,result:Option<Value>){
        if let Ok(mut pending)=self.persist_pending.lock(){
            if let Some(existing)=pending.iter_mut().find(|item|item.id==id){
                if state.is_some(){existing.state=state.map(str::to_owned);existing.stage=stage.to_owned();existing.result=result;}
                return;
            }
            if pending.len()>=super::task_delivery::MAX_PERSIST_PENDING{return;}
            pending.push(UnstoredResult{id:id.to_owned(),state:state.map(str::to_owned),stage:stage.to_owned(),result});
        }
    }
    fn clear_unstored(&self,id:&str){
        if let Ok(mut pending)=self.persist_pending.lock(){pending.retain(|item|item.id!=id);}
    }
    fn publish_hint(&self){
        if let Ok(db)=self.db.lock(){if let Ok(hint)=inbox_hint_due(&db){self.hint_pending.store(hint,Ordering::Release);}}
    }
    /// The next result or question this household may speak. A private scope is
    /// returned only for that person. Migrated history and a hold from this
    /// activation stay out.
    pub fn next_for_delivery(&self,household:&str,activation:&str,speaker:Option<&str>)->Result<Option<ReadyDelivery>,String>{
        valid_id(household)?;if !activation.is_empty(){valid_id(activation)?;}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        fence_invalid_inbox(&tx)?;
        let mut statement=tx.prepare("SELECT i.delivery_id FROM delivery_inbox i JOIN tasks t ON t.id=i.task_id WHERE t.household_id=?1 AND i.migrated=0 AND i.state IN ('pending_delivery','deferred') AND (i.hold_activation IS NULL OR i.hold_activation!=?2) ORDER BY i.created_at_ms, i.delivery_id LIMIT 8").map_err(db_error)?;
        let rows=statement.query_map(params![household,activation],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        drop(statement);
        for id in ids{
            let row=load_inbox(&tx,&id)?;
            if !scope_visible(&row.scope,row.private_scope,speaker){continue;}
            let task=query_task(&tx,&row.task_id)?;
            if source_invalidated(&tx,&task.conversation_turn_id)?{continue;}
            tx.commit().map_err(db_error)?;drop(db);self.publish_hint();
            return Ok(Some(ReadyDelivery{task,delivery_id:row.delivery_id,delivery_revision:row.revision,kind:row.kind,promise_full:row.promise=="full"}));
        }
        tx.commit().map_err(db_error)?;Ok(None)
    }
    /// The frozen full text of a summary that is still waiting for yes or no.
    pub fn expansion_target(&self,household:&str,speaker:Option<&str>)->Result<Option<InboxView>,String>{
        valid_id(household)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let fresh=now()-super::task_delivery::PARTIAL_WAIT_MS;
        let mut statement=db.prepare("SELECT i.delivery_id FROM delivery_inbox i JOIN tasks t ON t.id=i.task_id WHERE t.household_id=?1 AND i.migrated=0 AND i.state='partial' AND i.kind='result' AND i.created_at_ms>=?2 ORDER BY i.created_at_ms DESC LIMIT 8").map_err(db_error)?;
        let rows=statement.query_map(params![household,fresh],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        drop(statement);
        for id in ids {
            let row=load_inbox(&db,&id)?;
            if scope_visible(&row.scope,row.private_scope,speaker) {
                return Ok(Some(view_of(&row)));
            }
        }
        Ok(None)
    }
    /// A no, or the wait expiring, means the summary was the delivery.
    pub fn settle_partials(&self,household:&str)->Result<usize,String>{
        valid_id(household)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let n=db.execute("UPDATE delivery_inbox SET state='delivered',delivery_revision=delivery_revision+1,promise='full' WHERE state='partial' AND task_id IN (SELECT id FROM tasks WHERE household_id=?1)",[household]).map_err(db_error)?;
        drop(db);self.publish_hint();Ok(n)
    }
    pub fn defer_results(&self,household:&str,activation:&str)->Result<usize,String>{
        valid_id(household)?;if !activation.is_empty(){valid_id(activation)?;}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let n=db.execute("UPDATE delivery_inbox SET state='deferred',hold_activation=?2,delivery_revision=delivery_revision+1 WHERE migrated=0 AND kind='result' AND state IN ('pending_delivery','deferred') AND (hold_activation IS NULL OR hold_activation!=?2) AND task_id IN (SELECT id FROM tasks WHERE household_id=?1)",params![household,activation]).map_err(db_error)?;
        drop(db);self.publish_hint();Ok(n)
    }
    pub fn claim_delivery(&self,delivery_id:&str,expected_revision:i64,lease_id:&str,playback_id:&str)->Result<i64,String>{
        valid_id(delivery_id)?;valid_id(lease_id)?;valid_id(playback_id)?;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let n=tx.execute("UPDATE delivery_inbox SET state='offered',delivery_revision=delivery_revision+1,attempted_lease_id=?3,playback_id=?4 WHERE delivery_id=?1 AND delivery_revision=?2 AND state IN ('pending_delivery','deferred','partial')",params![delivery_id,expected_revision,lease_id,playback_id]).map_err(db_error)?;
        if n!=1{return Err("delivery_conflict".into());}
        tx.commit().map_err(db_error)?;Ok(expected_revision+1)
    }
    pub fn apply_playback(&self,delivery_id:&str,expected_revision:i64,lease_id:&str,completed:bool,prefix:&str,played_full:bool)->Result<String,String>{
        valid_id(delivery_id)?;valid_id(lease_id)?;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let row=load_inbox(&tx,delivery_id)?;
        if row.state=="delivered" && row.lease.as_deref()==Some(lease_id){return Ok("delivered".into());}
        if row.revision!=expected_revision{return Err("delivery_conflict".into());}
        let outcome=super::task_delivery::settle_playback(row.promise=="full",played_full,completed,!prefix.is_empty(),row.lease.as_deref()==Some(lease_id));
        let state=match outcome{
            super::task_delivery::ReceiptOutcome::Rejected=>return Err("delivery_lease_mismatch".into()),
            super::task_delivery::ReceiptOutcome::Delivered=>"delivered",
            super::task_delivery::ReceiptOutcome::Partial=>"partial",
            super::task_delivery::ReceiptOutcome::Deferred=>"deferred",
        };
        let mut refs:Vec<Value>=serde_json::from_str(&row.receipts).unwrap_or_default();
        refs.push(json!({"lease_id":lease_id,"status":state,"prefix":prefix}));
        if serde_json::to_vec(&refs).map_err(|e|e.to_string())?.len()>16_384{return Err("delivery_receipt_limit".into());}
        let hold=if state=="deferred"{Some(row.hold.unwrap_or_default())}else{None};
        tx.execute("UPDATE delivery_inbox SET state=?2,delivery_revision=delivery_revision+1,confirmed_prefix=?3,receipt_refs=?4,hold_activation=?5 WHERE delivery_id=?1",
            params![delivery_id,state,prefix,serde_json::to_string(&refs).map_err(|e|e.to_string())?,hold]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;drop(db);self.publish_hint();Ok(state.to_owned())
    }
    /// Stop or a missing receipt. Nothing already delivered is reopened, and this
    /// activation does not immediately take a new lease.
    /// Settle the attempt that already holds this row's lease. A second call with
    /// the same lease does not open another one.
    pub fn settle_claimed(&self,delivery_id:&str,completed:bool,prefix:&str)->Result<String,String>{
        valid_id(delivery_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let row=load_inbox(&db,delivery_id)?;
        let lease=row.lease.clone().unwrap_or_default();
        let revision=row.revision;
        let full=row.promise=="full";
        drop(db);
        if lease.is_empty(){return Err("delivery_lease_mismatch".into());}
        self.apply_playback(delivery_id,revision,lease.as_str(),completed,prefix,completed&&full)
    }
    pub fn release_offered(&self)->Result<usize,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let n=release_offered_rows(&db)?;
        drop(db);self.publish_hint();Ok(n)
    }
    pub fn age_row(&self,delivery_id:&str,created_at_ms:i64)->Result<(),String>{
        valid_id(delivery_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("UPDATE delivery_inbox SET created_at_ms=?2 WHERE delivery_id=?1",params![delivery_id,created_at_ms]).map_err(db_error)?;
        drop(db);self.publish_hint();Ok(())
    }
    pub fn retain(&self)->Result<(),String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        prune_inbox(&db)?;
        drop(db);self.publish_hint();Ok(())
    }
    pub fn inbox_count(&self)->Result<i64,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.query_row("SELECT count(*) FROM delivery_inbox",[],|r|r.get(0)).map_err(db_error)
    }
    pub fn outbox_count(&self)->Result<i64,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.query_row("SELECT count(*) FROM result_outbox",[],|r|r.get(0)).map_err(db_error)
    }
    pub fn offer_ready(&self,delivery_id:&str)->bool{
        let Ok(db)=self.db.lock() else {return false};
        db.query_row("SELECT offer_not_before_ms IS NULL OR offer_not_before_ms<=?2 FROM delivery_inbox WHERE delivery_id=?1",params![delivery_id,now()],|r|r.get::<_,bool>(0)).unwrap_or(false)
    }
    pub fn note_offer_backoff(&self,delivery_id:&str)->Result<(),String>{
        if delivery_id.is_empty(){return Ok(());}
        valid_id(delivery_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("UPDATE delivery_inbox SET offer_not_before_ms=?2 WHERE delivery_id=?1",params![delivery_id,now()+super::task_delivery::OFFER_BACKOFF_MS]).map_err(db_error)?;
        Ok(())
    }
    pub fn rebind_lease(&self,delivery_id:&str,revision:i64,lease_id:&str,playback_id:&str)->Result<(),String>{
        valid_id(delivery_id)?;valid_id(lease_id)?;valid_id(playback_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let n=db.execute("UPDATE delivery_inbox SET attempted_lease_id=?3,playback_id=?4 WHERE delivery_id=?1 AND delivery_revision=?2 AND state='offered'",params![delivery_id,revision,lease_id,playback_id]).map_err(db_error)?;
        if n!=1{return Err("delivery_conflict".into());}
        Ok(())
    }
    /// Release this claim only. A newer lease, or a row that already settled, stays.
    pub fn release_attempt(&self,delivery_id:&str,lease_id:&str,prefix:&str)->Result<(),String>{
        if delivery_id.is_empty()||lease_id.is_empty(){return Ok(());}
        valid_id(delivery_id)?;valid_id(lease_id)?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let state=if prefix.is_empty(){"pending_delivery"}else{"partial"};
        let n=db.execute("UPDATE delivery_inbox SET state=?3,hold_activation=NULL,attempted_lease_id=NULL,playback_id=NULL,delivery_revision=delivery_revision+1,confirmed_prefix=CASE WHEN ?4='' THEN confirmed_prefix ELSE ?4 END WHERE delivery_id=?1 AND attempted_lease_id=?2 AND state='offered'",
            params![delivery_id,lease_id,state,prefix]).map_err(db_error)?;
        drop(db);
        if n==1{self.publish_hint();}
        Ok(())
    }
    pub fn inbox_state(&self,task_id:&str)->Result<Option<String>,String>{
        valid_id(task_id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.query_row("SELECT state FROM delivery_inbox WHERE task_id=?1 AND migrated=0 ORDER BY result_revision DESC LIMIT 1",[task_id],|r|r.get(0)).optional().map_err(db_error)
    }
    pub fn inbox_by_id(&self,delivery_id:&str)->Result<Option<InboxView>,String>{
        valid_id(delivery_id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        if db.query_row("SELECT EXISTS(SELECT 1 FROM delivery_inbox WHERE delivery_id=?1)",[delivery_id],|r|r.get::<_,bool>(0)).map_err(db_error)? {
            let row=load_inbox(&db,delivery_id)?;
            return Ok(Some(InboxView{delivery_id:row.delivery_id,revision:row.revision,state:row.state,kind:row.kind,promise_full:row.promise=="full",summary:row.summary,result_text:row.result_text,tell_me:row.tell_me,private_scope:row.private_scope,receipts:row.receipts}));
        }
        Ok(None)
    }
    pub fn inbox_row(&self,task_id:&str)->Result<Option<InboxView>,String>{
        valid_id(task_id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let id:Option<String>=db.query_row("SELECT delivery_id FROM delivery_inbox WHERE task_id=?1 AND migrated=0 ORDER BY result_revision DESC LIMIT 1",[task_id],|r|r.get(0)).optional().map_err(db_error)?;
        let Some(id)=id else{return Ok(None)};
        let row=load_inbox(&db,&id)?;
        Ok(Some(InboxView{delivery_id:row.delivery_id,revision:row.revision,state:row.state,kind:row.kind,promise_full:row.promise=="full",summary:row.summary,result_text:row.result_text,tell_me:row.tell_me,private_scope:row.private_scope,receipts:row.receipts}))
    }
    pub fn rescan_inbox(&self,limit:usize)->Result<Vec<String>,String>{
        let limit=limit.clamp(1,super::task_delivery::RESCAN_BATCH) as i64;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let mut statement=tx.prepare("SELECT o.id,o.delivery_id FROM result_outbox o JOIN delivery_inbox i ON i.delivery_id=o.delivery_id WHERE i.migrated=0 AND i.state IN ('pending_delivery','deferred','partial') ORDER BY o.id LIMIT ?1").map_err(db_error)?;
        let rows=statement.query_map([limit],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;
        let pairs=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        drop(statement);
        if !pairs.is_empty() {
            for (id,_) in &pairs { tx.execute("DELETE FROM result_outbox WHERE id=?1", [id]).map_err(db_error)?; }
            let ids=pairs.into_iter().map(|(_,delivery)|delivery).collect::<Vec<_>>();
            tx.commit().map_err(db_error)?;
            return Ok(ids);
        }
        let mut statement=tx.prepare("SELECT delivery_id FROM delivery_inbox WHERE migrated=0 AND state IN ('pending_delivery','deferred','partial') ORDER BY created_at_ms,delivery_id LIMIT ?1").map_err(db_error)?;
        let rows=statement.query_map([limit],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        drop(statement);
        tx.commit().map_err(db_error)?;
        Ok(ids)
    }
    pub fn clear_outbox(&self)->Result<(),String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("DELETE FROM result_outbox",[]).map_err(db_error)?;Ok(())
    }
    pub fn note_foreground(&self,turn_id:&str,generation:u64,producer_closed:bool,outcome:Option<&str>,task_id:Option<&str>)->Result<(),String>{
        valid_id(turn_id)?;if let Some(id)=task_id{valid_id(id)?;}
        if let Some(outcome)=outcome{if !matches!(outcome,"answered"|"delegated"|"partial"|"failed"|"cancelled"){return Err("foreground_outcome_invalid".into());}}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("INSERT INTO foreground_responses(turn_id,generation,producer_closed,outcome,task_id,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6)
            ON CONFLICT(turn_id) DO UPDATE SET generation=excluded.generation,producer_closed=max(foreground_responses.producer_closed,excluded.producer_closed),outcome=COALESCE(excluded.outcome,foreground_responses.outcome),task_id=COALESCE(excluded.task_id,foreground_responses.task_id),updated_at_ms=excluded.updated_at_ms",
            params![turn_id,generation as i64,producer_closed,outcome,task_id,now()]).map_err(db_error)?;Ok(())
    }
    pub fn foreground(&self,turn_id:&str)->Result<Option<Value>,String>{
        valid_id(turn_id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.query_row("SELECT generation,producer_closed,outcome,task_id FROM foreground_responses WHERE turn_id=?1",[turn_id],|r|Ok(json!({"generation":r.get::<_,i64>(0)?,"producer_closed":r.get::<_,i64>(1)?==1,"outcome":r.get::<_,Option<String>>(2)?,"task_id":r.get::<_,Option<String>>(3)?}))).optional().map_err(db_error)
    }
    fn claim(&self,id:&str)->Result<bool,String>{
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let task=query_task(&tx,id)?;if self.redacting.load(Ordering::Acquire)||source_invalidated(&tx,&task.conversation_turn_id)?{return Ok(false);}
        let claimed=tx.execute("UPDATE tasks SET state='running',stage='starting',attempt=attempt+1,updated_at_ms=?2,task_revision=task_revision+1 WHERE id=?1 AND state='queued'",params![id,now()]).map_err(db_error)?==1;
        if claimed{tx.execute("INSERT INTO task_attempts(id,task_id,attempt,started_at_ms,state) VALUES(?1,?2,?3,?4,'running')",params![format!("attempt_{}",uuid::Uuid::new_v4().simple()),id,task.attempt+1,now()]).map_err(db_error)?;append_event(&tx,id,"task.started",&json!({"attempt":task.attempt+1}))?;}
        tx.commit().map_err(db_error)?;Ok(claimed)
    }
    fn queued(&self)->Result<Vec<TaskRecord>,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT id FROM tasks WHERE state='queued' ORDER BY created_at_ms,id LIMIT 32").map_err(db_error)?;
        let rows=statement.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?;let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        ids.iter().map(|id|query_task(&db,id)).collect()
    }
    fn pending_prints(&self)->Result<Vec<TaskRecord>,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        // Filter before LIMIT: recent partial renders must not crowd an older
        // unresolved print out of every pass. Oldest checked jobs rotate first.
        // CUPS history older than seven days remains manually reconcilable.
        let mut statement=db.prepare("SELECT id FROM tasks WHERE capability='print.submit' AND state IN ('unknown','partial','cancel_requested') AND created_at_ms>=?1 ORDER BY updated_at_ms,id LIMIT 32").map_err(db_error)?;
        let rows=statement.query_map([now().saturating_sub(7*24*60*60*1000)],|r|r.get::<_,String>(0)).map_err(db_error)?;
        let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;ids.iter().map(|id|query_task(&db,id)).collect()
    }
    pub fn attempts(&self,id:&str)->Result<Vec<Value>,String>{
        valid_id(id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT id,attempt,started_at_ms,ended_at_ms,state FROM task_attempts WHERE task_id=?1 ORDER BY attempt").map_err(db_error)?;
        let rows=statement.query_map([id],|r|Ok(json!({"attempt_id":r.get::<_,String>(0)?,"attempt":r.get::<_,u32>(1)?,"started_at_ms":r.get::<_,i64>(2)?,"ended_at_ms":r.get::<_,Option<i64>>(3)?,"state":r.get::<_,String>(4)?}))).map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }
    async fn run_task(self:Arc<Self>,id:&str,cancelled:watch::Receiver<bool>){
        let Ok(task)=self.get(id) else{return};
        if task.state!=TaskState::Running{if task.state==TaskState::CancelRequested{let _=self.set_state(id,TaskState::Cancelled,"cancelled",None);}let _=self.finish_attempt(id);if let Ok(mut active)=self.active.lock(){active.remove(id);}return;}
        let mut interrupt=cancelled.clone();let context=TaskContext{service:Arc::clone(&self),task:task.clone(),cancelled};
        let handler=self.handlers.lock().ok().and_then(|h|h.get(&task.capability).cloned());let managed=handler.is_some();
        let work:TaskFuture=if let Some(handler)=handler{handler(context)}else{Box::pin(super::capabilities::execute(context))};
        let outcome=if managed{
            // Registered OpenCode handlers own remote cancellation. Give their
            // receiver a chance to call the official session-abort endpoint;
            // dropping the local request alone does not stop remote tools.
            let mut work=work;
            tokio::select!{result=&mut work=>result,_=tokio::time::sleep(Duration::from_secs(self.config.timeout_seconds))=>{
                if let Ok(active)=self.active.lock(){if let Some(sender)=active.get(id){sender.send_replace(true);}}
                match tokio::time::timeout(Duration::from_secs(10),work).await{Ok(Ok(result))=>Ok(result),_=>Err("task_timeout".into())}
            }}
        }else{
            let timeout=tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds),work);
            if task.capability=="print.submit"{timeout.await.unwrap_or_else(|_|Err("task_timeout".into()))}
            else{tokio::select!{result=timeout=>result.unwrap_or_else(|_|Err("task_timeout".into())),_=interrupt.changed()=>Err("cancelled".into())}}
        };
        match outcome{
            Ok(result)=>{
                let latest=self.get(id).ok();let cancelled=task.capability!="print.submit"&&latest.as_ref().is_some_and(|t|matches!(t.state,TaskState::CancelRequested|TaskState::Cancelled));
                let mut combined=latest.map(|t|t.result).unwrap_or_else(||json!({}));if !combined.is_object(){combined=json!({});}
                combined["summary"]=json!(if cancelled{"任务已取消。".to_owned()}else{result.summary});combined["artifact_ids"]=json!(result.artifact_ids);combined["data"]=result.data;
                self.commit_state_retry(id,if cancelled{TaskState::Cancelled}else{result.state},"finished",Some(combined)).await;},
            Err(code)=>{
                let latest=self.get(id).ok();
                let external_uncertain=task.capability=="print.submit"&&latest.as_ref().is_some_and(|t|t.stage=="submitting"||t.result.get("submission_started").is_some()||t.result.get("job_id").is_some());
                let state=if external_uncertain{TaskState::Unknown}else if code=="cancelled"||latest.as_ref().is_some_and(|t|matches!(t.state,TaskState::CancelRequested|TaskState::Cancelled)){TaskState::Cancelled}else{TaskState::Failed};
                let mut data=latest.map(|t|t.result).unwrap_or_else(||json!({}));if !data.is_object(){data=json!({});}data["error"]=json!(code);
                self.commit_state_retry(id,state,if external_uncertain{"reconcile_required"}else if state==TaskState::Cancelled{"cancelled"}else{"failed"},Some(data)).await;
            }
        }
        let _=self.finish_attempt(id);
        if let Ok(mut active)=self.active.lock(){active.remove(id);}
    }
    fn finish_attempt(&self,id:&str)->Result<(),String>{let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("UPDATE task_attempts SET ended_at_ms=?2,state=(SELECT state FROM tasks WHERE id=?1) WHERE task_id=?1 AND ended_at_ms IS NULL",params![id,now()]).map_err(db_error)?;Ok(())}
    fn emit(&self,task:&TaskRecord,kind:&str,payload:Value){(self.sink)(json!({"kind":kind,"turn_id":task.conversation_turn_id,"at_ms":now(),"payload":{"task_id":task.task_id,"household_id":task.household_id,"daily_conversation_id":task.daily_conversation_id,"capability":task.capability,"source_invalidated":!self.source_current(&task.conversation_turn_id).unwrap_or(false),"status":payload}}));}

    pub fn source_current(&self,turn_id:&str)->Result<bool,String>{let db=self.db.lock().map_err(|_|"task_database_lock")?;Ok(!source_invalidated(&db,turn_id)?)}
    fn source_tasks(&self,turn_ids:&[String],all:bool)->Result<Vec<TaskRecord>,String>{
        for id in turn_ids{valid_id(id)?;}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT id,turn_id,input_json FROM tasks").map_err(db_error)?;
        let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(db_error)?;
        let mut ids=Vec::new();for row in rows{let(id,turn,input)=row.map_err(db_error)?;let input:Value=serde_json::from_str(&input).unwrap_or(Value::Null);
            if all||turn_ids.contains(&turn)||input["source_turn_ids"].as_array().is_some_and(|sources|sources.iter().any(|s|s.as_str().is_some_and(|id|turn_ids.iter().any(|turn|turn==id)))){ids.push(id);}}
        drop(statement);ids.iter().map(|id|query_task(&db,id)).collect()
    }
    async fn retire_source_tasks(&self,tasks:&[TaskRecord],forget:bool)->Result<(),String>{
        // Registering a runner holds the same active lock until its handle is
        // installed. After the state fence no queued task can be newly claimed.
        for task in tasks{
            let current=self.get(&task.task_id)?;
            // begin_effect checks the source fence in the same database lock,
            // so no new external submission can cross it after block_sources.
            if current.capability=="print.submit"&&!forget&&current.result["submission_started"]==true{continue;}
            let state=if current.capability=="print.submit"&&current.result["submission_started"]==true{TaskState::Unknown}else{TaskState::Cancelled};
            self.set_state(&task.task_id,state,"source_invalidated",None)?;
            let handle={let mut active=self.active.lock().map_err(|_|"task_active_lock")?;
                if let Some(sender)=active.remove(&task.task_id){if task.capability!="print.submit"{sender.send_replace(true);}}
                self.runners.lock().map_err(|_|"task_runners_lock")?.remove(&task.task_id)};
            if let Some(mut handle)=handle{
                if task.capability=="print.submit"||tokio::time::timeout(Duration::from_secs(10),&mut handle).await.is_err(){handle.abort();let _=handle.await;}
            }
            self.finish_attempt(&task.task_id)?;
        }Ok(())
    }
    /// Correction invalidates old derived output. It never cancels an accepted
    /// physical print: CUPS receipts remain useful facts after source correction.
    pub async fn invalidate_sources(self:&Arc<Self>,turn_ids:&[String],all:bool)->Result<(),String>{
        if self.redacting.swap(true,Ordering::AcqRel){return Err("task_source_update_in_progress".into());}let _guard=RedactionGuard(&self.redacting);
        let seed=self.source_tasks(turn_ids,all)?;let affected=self.block_sources(turn_ids,&seed,all,"source_changed")?;let tasks=self.source_tasks(&affected,all)?;self.revoke_outputs(None)?;
        self.retire_source_tasks(&tasks,false).await?;let ids=tasks.iter().map(|t|t.task_id.clone()).collect::<Vec<_>>();
        self.artifacts.invalidate_sources(&affected,&ids,all)
    }
    /// Erase content only after all corresponding producer futures have left.
    /// Retain minimal external job identifiers for truthful effect reconciliation.
    pub async fn forget_sources(self:&Arc<Self>,turn_ids:&[String],all:bool)->Result<(),String>{
        if self.redacting.swap(true,Ordering::AcqRel){return Err("task_source_update_in_progress".into());}let _guard=RedactionGuard(&self.redacting);
        let seed=self.source_tasks(turn_ids,all)?;let affected=self.block_sources(turn_ids,&seed,all,"source_forgotten")?;let tasks=self.source_tasks(&affected,all)?;self.revoke_outputs(None)?;
        self.retire_source_tasks(&tasks,true).await?;let ids=tasks.iter().map(|t|t.task_id.clone()).collect::<Vec<_>>();
        self.artifacts.forget_sources(&affected,&ids,all)?;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        for task in &tasks{
            let current=query_task(&tx,&task.task_id)?;let tombstone=if task.capability=="print.submit"{effect_tombstone(&current.result)}else{json!({"content_deleted":true})};
            if let Some(receipt)=task.input["source_receipt_id"].as_str(){tx.execute("DELETE FROM source_receipts WHERE id=?1",[receipt]).map_err(db_error)?;}
            tx.execute("UPDATE tasks SET input_json='{}',result_json=?2,stage='content_deleted' WHERE id=?1",params![task.task_id,serde_json::to_string(&tombstone).map_err(|e|e.to_string())?]).map_err(db_error)?;
            tx.execute("DELETE FROM task_events WHERE task_id=?1",[&task.task_id]).map_err(db_error)?;
            tx.execute("UPDATE task_attempts SET result_json='{}' WHERE task_id=?1",[&task.task_id]).map_err(db_error)?;
            tx.execute("DELETE FROM result_outbox WHERE delivery_id IN (SELECT delivery_id FROM delivery_inbox WHERE task_id=?1)",[&task.task_id]).map_err(db_error)?;
            if task.capability=="print.submit" {
                tx.execute("UPDATE delivery_inbox SET result_ref='',summary_ref='',confirmed_prefix='',receipt_refs='[]',source_refs='[]',state='invalidated' WHERE task_id=?1",[&task.task_id]).map_err(db_error)?;
            } else {
                tx.execute("DELETE FROM delivery_inbox WHERE task_id=?1",[&task.task_id]).map_err(db_error)?;
            }
            if task.capability!="print.submit"{tx.execute("DELETE FROM task_attempts WHERE task_id=?1",[&task.task_id]).map_err(db_error)?;tx.execute("DELETE FROM tasks WHERE id=?1",[&task.task_id]).map_err(db_error)?;}
        }
        let plans={let mut statement=tx.prepare("SELECT id,revision,plan_json FROM presentations").map_err(db_error)?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u32>(1)?,r.get::<_,String>(2)?))).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        for(id,revision,raw)in plans{let value:Value=serde_json::from_str(&raw).unwrap_or(Value::Null);
            let matches=all||value.pointer("/context/conversation_turn_id").and_then(Value::as_str).is_some_and(|t|affected.iter().any(|id|id==t))
                ||value.pointer("/context/task_id").and_then(Value::as_str).is_some_and(|t|ids.iter().any(|id|id==t));
            if matches{tx.execute("DELETE FROM playback_receipts WHERE playback_id IN (SELECT id FROM playbacks WHERE presentation_id=?1 AND revision=?2)",params![id,revision]).map_err(db_error)?;
                tx.execute("DELETE FROM playbacks WHERE presentation_id=?1 AND revision=?2",params![id,revision]).map_err(db_error)?;
                tx.execute("DELETE FROM presentations WHERE id=?1 AND revision=?2",params![id,revision]).map_err(db_error)?;}}
        for id in &affected{tx.execute("DELETE FROM presentation_sources WHERE context_turn_id=?1",[id]).map_err(db_error)?;}
        if all{tx.execute("DELETE FROM source_receipts",[]).map_err(db_error)?;tx.execute("DELETE FROM output_leases",[]).map_err(db_error)?;tx.execute("DELETE FROM presentation_sources",[]).map_err(db_error)?;}
        tx.commit().map_err(db_error)?;Ok(())
    }
    fn block_sources(&self,turn_ids:&[String],tasks:&[TaskRecord],all:bool,reason:&str)->Result<Vec<String>,String>{
        let mut queue=std::collections::VecDeque::from(turn_ids.to_vec());queue.extend(tasks.iter().map(|t|t.conversation_turn_id.clone()));
        let mut edges:HashMap<String,Vec<String>>=HashMap::new();let mut seen=std::collections::HashSet::new();
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        {let mut statement=db.prepare("SELECT source_id,context_turn_id FROM presentation_sources").map_err(db_error)?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;
            for row in rows{let(source,context)=row.map_err(db_error)?;if all{queue.push_back(context.clone());}edges.entry(source).or_default().push(context);}}
        {let mut statement=db.prepare("SELECT turn_id,input_json FROM tasks").map_err(db_error)?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;
            for row in rows{let(context,raw)=row.map_err(db_error)?;if all{queue.push_back(context.clone());}let input:Value=serde_json::from_str(&raw).unwrap_or(Value::Null);
                for source in input["source_turn_ids"].as_array().into_iter().flatten().filter_map(Value::as_str){edges.entry(source.into()).or_default().push(context.clone());}}}
        if all{let mut statement=db.prepare("SELECT plan_json FROM presentations").map_err(db_error)?;let rows=statement.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?;
            for row in rows{let raw=row.map_err(db_error)?;if let Ok(plan)=serde_json::from_str::<PresentationPlan>(&raw){queue.push_back(plan.context.conversation_turn_id);}}}
        // Only version keys already referenced by a persisted plan/task are
        // expanded. A later correction's new version is not prospectively
        // fenced merely because its canonical turn has obsolete output.
        let mut existing_versions:HashMap<String,Vec<String>>=HashMap::new();
        for source in edges.keys(){if let Some((canonical,revision))=source.strip_prefix("source-ref:").and_then(|s|s.rsplit_once(":v")){
            if !revision.is_empty()&&revision.bytes().all(|b|b.is_ascii_digit()){existing_versions.entry(canonical.into()).or_default().push(source.clone());}}}
        while let Some(id)=queue.pop_front(){if !seen.insert(id.clone()){continue;}valid_id(&id)?;
            if let Some(dependents)=edges.get(&id){queue.extend(dependents.iter().cloned());}
            if let Some(versions)=existing_versions.get(&id){queue.extend(versions.iter().cloned());}}
        let mut ids=seen.into_iter().collect::<Vec<_>>();ids.sort();
        for id in &ids{db.execute("INSERT INTO invalidated_sources(turn_id,reason) VALUES(?1,?2) ON CONFLICT(turn_id) DO UPDATE SET reason=excluded.reason",params![id,reason]).map_err(db_error)?;}
        invalidate_inbox_turns(&db,&ids)?;
        Ok(ids)
    }

    /// Only the trusted host calls this after matching a completed tool result.
    /// Models receive the receipt ID, never permission to invent provenance.
    pub(crate) fn register_source(&self,url:&str,evidence:Value)->Result<String,String>{
        let text=serde_json::to_string(&evidence).map_err(|e|e.to_string())?;if url.len()>4096||text.len()>16_384{return Err("source_receipt_limit".into());}
        let id=format!("source_{}",uuid::Uuid::new_v4().simple());let db=self.db.lock().map_err(|_|"task_database_lock")?;
        if self.redacting.load(Ordering::Acquire){return Err("task_source_update_in_progress".into());}
        db.execute("INSERT INTO source_receipts(id,source_url,evidence_json,created_at_ms) VALUES(?1,?2,?3,?4)",params![id,url,text,now()]).map_err(db_error)?;Ok(id)
    }
    pub(crate) fn source(&self,id:&str)->Result<(String,Value),String>{
        valid_id(id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let (url,evidence):(String,String)=db.query_row("SELECT source_url,evidence_json FROM source_receipts WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        Ok((url,serde_json::from_str(&evidence).map_err(|e|e.to_string())?))
    }
    pub fn status(&self)->Value{
        let counts=(||->Result<Value,String>{let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT state,count(*) FROM tasks GROUP BY state").map_err(db_error)?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).map_err(db_error)?;let mut result=json!({});for row in rows{let(k,v)=row.map_err(db_error)?;result[k]=json!(v);}Ok(result)})();
        json!({"running":self.accepting.load(Ordering::Acquire),"counts":counts.unwrap_or_else(|e|json!({"error":e})),"capabilities":super::capabilities::availability(&self.config)})
    }

    pub fn save_plan(&self,plan:&PresentationPlan)->Result<(),String>{
        valid_id(&plan.presentation_id)?;if plan.schema_version!=1||plan.revision==0||plan.segments.is_empty()||plan.segments.len()>64{return Err("presentation_invalid".into());}
        let json=serde_json::to_string(plan).map_err(|e|e.to_string())?;if json.len()>262_144{return Err("presentation_limit".into());}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        if source_invalidated(&db,&plan.context.conversation_turn_id)?{return Err("presentation_source_invalidated".into());}
        if presentation_sources_invalidated(&db,&plan.presentation_id)?{return Err("presentation_source_invalidated".into());}
        if db.query_row("SELECT EXISTS(SELECT 1 FROM expired_presentations WHERE id=?1)",[&plan.presentation_id],|r|r.get::<_,bool>(0)).map_err(db_error)?{return Err("presentation_expired".into());}
        let existing:Option<String>=db.query_row("SELECT plan_json FROM presentations WHERE id=?1 AND revision=?2",params![plan.presentation_id,plan.revision],|r|r.get(0)).optional().map_err(db_error)?;
        if let Some(existing)=existing{if existing!=json{return Err("presentation_revision_immutable".into());}return Ok(());}
        let latest:Option<String>=db.query_row("SELECT plan_json FROM presentations WHERE id=?1 ORDER BY revision DESC LIMIT 1",[&plan.presentation_id],|r|r.get(0)).optional().map_err(db_error)?;
        if let Some(latest)=latest{let previous:PresentationPlan=serde_json::from_str(&latest).map_err(|e|e.to_string())?;
            if previous.revision.checked_add(1)!=Some(plan.revision)||semantic_content(&previous)?!=semantic_content(plan)?{return Err("presentation_semantics_immutable".into());}}
        else if plan.revision!=1{return Err("presentation_revision_invalid".into());}
        let refs={let mut statement=db.prepare("SELECT source_id FROM presentation_sources WHERE presentation_id=?1").map_err(db_error)?;
            let rows=statement.query_map([&plan.presentation_id],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        if json.len()+serde_json::to_vec(&refs).map_err(|e|e.to_string())?.len()>262_144{return Err("presentation_source_limit".into());}
        db.execute("INSERT INTO presentations(id,revision,plan_json) VALUES(?1,?2,?3)",params![plan.presentation_id,plan.revision,json]).map_err(db_error)?;Ok(())
    }
    pub fn bind_presentation_sources(&self,id:&str,source_ids:&[String])->Result<(),String>{
        valid_id(id)?;if source_ids.len()>2048{return Err("presentation_source_limit".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let raw:String=tx.query_row("SELECT plan_json FROM presentations WHERE id=?1 ORDER BY revision DESC LIMIT 1",[id],|r|r.get(0)).map_err(db_error)?;
        let plan:PresentationPlan=serde_json::from_str(&raw).map_err(|e|e.to_string())?;
        if self.redacting.load(Ordering::Acquire)||source_invalidated(&tx,&plan.context.conversation_turn_id)?||presentation_sources_invalidated(&tx,id)?{return Err("presentation_source_invalidated".into());}
        let mut sources={let mut statement=tx.prepare("SELECT source_id FROM presentation_sources WHERE presentation_id=?1").map_err(db_error)?;
            let rows=statement.query_map([id],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        // A reused poster/image carries its own source closure, even when it
        // came from another day and was not among this turn's recalled text.
        // Bind it before copying bytes into a newly rendered presentation.
        let mut assets=Vec::new();
        for segment in &plan.segments{
            assets.extend(segment.asset_ids.iter().cloned());assets.extend(segment.speech_asset_id.iter().cloned());
            for field in ["artifact_id","poster_artifact_id"]{if let Some(asset)=segment.visual[field].as_str(){assets.push(asset.to_owned());}}
        }
        assets.sort();assets.dedup();
        for asset in assets{
            let mut input=json!({"artifact_id":asset});self.inherit_artifact_sources(&tx,&mut input)?;
            if let Some(inherited)=input["source_turn_ids"].as_array(){sources.extend(inherited.iter().filter_map(Value::as_str).map(str::to_owned));}
        }
        sources.extend(source_ids.iter().cloned());sources.push(plan.context.conversation_turn_id.clone());sources.sort();sources.dedup();
        if sources.len()>2049||raw.len()+serde_json::to_vec(&sources).map_err(|e|e.to_string())?.len()>262_144{return Err("presentation_source_limit".into());}
        for source in &sources{valid_id(source)?;if source_invalidated(&tx,source)?{return Err("presentation_source_invalidated".into());}
            tx.execute("INSERT OR IGNORE INTO presentation_sources(presentation_id,source_id,context_turn_id) VALUES(?1,?2,?3)",params![id,source,plan.context.conversation_turn_id]).map_err(db_error)?;}
        tx.commit().map_err(db_error)?;Ok(())
    }
    pub fn plan(&self,id:&str,revision:Option<u32>)->Result<PresentationPlan,String>{
        if self.redacting.load(Ordering::Acquire){return Err("task_source_update_in_progress".into());}
        let plan=self.load_plan(id,revision)?;
        if !self.source_current(&plan.context.conversation_turn_id)?{return Err("presentation_source_invalidated".into());}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;if presentation_sources_invalidated(&db,id)?{return Err("presentation_source_invalidated".into());}Ok(plan)
    }
    /// Short-lived foreground plans are still immutable while playing, but are
    /// not an implicit transcript archive when history recording is disabled.
    pub fn schedule_ephemeral(&self,id:&str,expires_at_ms:i64)->Result<(),String>{
        let plan=self.plan(id,None)?;if plan.context.task_id.is_some(){return Err("task_presentation_requires_retention".into());}
        if expires_at_ms<=now()||expires_at_ms>now()+30*60*1000{return Err("presentation_expiry_invalid".into());}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("INSERT INTO presentation_expiry(id,expires_at_ms) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET expires_at_ms=min(expires_at_ms,excluded.expires_at_ms)",params![id,expires_at_ms]).map_err(db_error)?;Ok(())
    }
    pub fn expire_ephemeral(&self,id:&str)->Result<(),String>{
        valid_id(id)?;
        {let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT plan_json FROM presentations WHERE id=?1").map_err(db_error)?;
            let rows=statement.query_map([id],|r|r.get::<_,String>(0)).map_err(db_error)?;for raw in rows{let plan:PresentationPlan=serde_json::from_str(&raw.map_err(db_error)?).map_err(|e|e.to_string())?;if plan.context.task_id.is_some(){return Err("task_presentation_requires_retention".into());}}}
        self.artifacts.block_sources(&[],&[id.to_owned()])?;
        let provenance_assets=self.artifacts.source_ids(&[],&[id.to_owned()],false)?;
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
            let plans={let mut statement=tx.prepare("SELECT plan_json FROM presentations WHERE id=?1").map_err(db_error)?;let rows=statement.query_map([id],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
            let mut assets=provenance_assets;for raw in plans{let plan:PresentationPlan=serde_json::from_str(&raw).map_err(|e|e.to_string())?;
                if plan.context.task_id.is_some(){return Err("task_presentation_requires_retention".into());}
                for segment in plan.segments{assets.extend(segment.asset_ids);assets.extend(segment.speech_asset_id);}}
            assets.sort();assets.dedup();
            tx.execute("INSERT OR IGNORE INTO expired_presentations(id) VALUES(?1)",[id]).map_err(db_error)?;
            tx.execute("UPDATE output_leases SET revoked=1 WHERE id IN (SELECT lease_id FROM playbacks WHERE presentation_id=?1)",[id]).map_err(db_error)?;
            tx.execute("DELETE FROM playback_receipts WHERE playback_id IN (SELECT id FROM playbacks WHERE presentation_id=?1)",[id]).map_err(db_error)?;
            tx.execute("DELETE FROM playbacks WHERE presentation_id=?1",[id]).map_err(db_error)?;
            tx.execute("DELETE FROM presentations WHERE id=?1",[id]).map_err(db_error)?;
            tx.execute("DELETE FROM presentation_expiry WHERE id=?1",[id]).map_err(db_error)?;
            let mut unreferenced=Vec::new();for asset in assets{
                // Explicit user tasks may legitimately retain a preview as a
                // print/media input. Keep those references and shared plans.
                let pattern=format!("%\"{asset}\"%");let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE input_json LIKE ?1 OR result_json LIKE ?1 UNION ALL SELECT 1 FROM presentations WHERE plan_json LIKE ?1)",[pattern],|r|r.get(0)).map_err(db_error)?;
                if !used{unreferenced.push(asset);}}
            tx.commit().map_err(db_error)?;
        // Keep the task admission lock until deletion finishes: an explicit
        // task cannot acquire an asset after this reference check but before
        // the corresponding bytes disappear.
        for asset in unreferenced{if let Err(error)=self.artifacts.forget(&asset){if error!="artifact_missing_or_expired"{return Err(error);}}}drop(db);Ok(())
    }
    pub fn purge_expired(&self)->Result<(),String>{
        let ids={let db=self.db.lock().map_err(|_|"task_database_lock")?;let mut statement=db.prepare("SELECT id FROM presentation_expiry WHERE expires_at_ms<=?1 LIMIT 200").map_err(db_error)?;
            let rows=statement.query_map([now()],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        for id in ids{self.expire_ephemeral(&id)?;}Ok(())
    }
    fn load_plan(&self,id:&str,revision:Option<u32>)->Result<PresentationPlan,String>{
        valid_id(id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let text:String=db.query_row("SELECT plan_json FROM presentations WHERE id=?1 AND (?2 IS NULL OR revision=?2) ORDER BY revision DESC LIMIT 1",params![id,revision],|r|r.get(0)).map_err(db_error)?;
        serde_json::from_str(&text).map_err(|e|e.to_string())
    }
    pub fn grant_output(&self,lease:OutputLease)->Result<(),String>{
        valid_id(&lease.output_lease_id)?;valid_id(&lease.activation_id)?;
        if lease.expires_at_ms<=now()||lease.expires_at_ms>now()+30*60*1000||lease.revoked{return Err("output_lease_invalid".into());}
        let text=serde_json::to_string(&lease).map_err(|e|e.to_string())?;
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        db.execute("INSERT INTO output_leases(id,lease_json,expires_at_ms) VALUES(?1,?2,?3)",params![lease.output_lease_id,text,lease.expires_at_ms]).map_err(db_error)?;Ok(())
    }
    pub fn output_active(&self,id:&str)->Result<bool,String>{valid_id(id)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;Ok(require_lease(&db,id).is_ok())}
    fn revoke_task_outputs(&self,task_id:&str)->Result<(),String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let plans={let mut statement=db.prepare("SELECT id,plan_json FROM presentations").map_err(db_error)?;let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        for(id,raw)in plans{if serde_json::from_str::<PresentationPlan>(&raw).is_ok_and(|p|p.context.task_id.as_deref()==Some(task_id)){
            db.execute("UPDATE output_leases SET revoked=1 WHERE id IN (SELECT lease_id FROM playbacks WHERE presentation_id=?1)",[id]).map_err(db_error)?;}}
        Ok(())
    }
    pub fn revoke_outputs(&self,activation:Option<&str>)->Result<(),String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        if let Some(activation)=activation{
            let mut statement=db.prepare("SELECT id,lease_json FROM output_leases WHERE revoked=0").map_err(db_error)?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(db_error)?;
            let leases=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;drop(statement);
            for(id,text)in leases{if serde_json::from_str::<OutputLease>(&text).is_ok_and(|l|l.activation_id==activation){db.execute("UPDATE output_leases SET revoked=1 WHERE id=?1",[id]).map_err(db_error)?;}}
        }else{db.execute("UPDATE output_leases SET revoked=1",[]).map_err(db_error)?;}Ok(())
    }
    pub fn begin_playback(&self,plan:&PresentationPlan,lease_id:&str)->Result<String,String>{
        self.save_plan(plan)?;let db=self.db.lock().map_err(|_|"task_database_lock")?;
        require_lease(&db,lease_id)?;let id=format!("playback_{}",uuid::Uuid::new_v4().simple());
        db.execute("INSERT INTO playbacks(id,presentation_id,revision,lease_id,state) VALUES(?1,?2,?3,?4,'prepared')",params![id,plan.presentation_id,plan.revision,lease_id]).map_err(db_error)?;Ok(id)
    }
    pub fn record_receipt(&self,receipt:PlaybackReceipt)->Result<bool,String>{
        if !matches!(receipt.state.as_str(),"prepared"|"dispatched"|"displayed"|"audio_started"|"audio_progress"|"audio_completed"|"completed"|"failed"|"cancelled"){return Err("playback_receipt_state".into());}
        let plan=self.load_plan(&receipt.presentation_id,Some(receipt.presentation_revision))?;
        if !plan.segments.iter().any(|s|s.segment_id==receipt.segment_id){return Err("playback_receipt_segment".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;let tx=db.transaction().map_err(db_error)?;
        let (pid,revision,lease,last):(String,u32,String,i64)=tx.query_row("SELECT presentation_id,revision,lease_id,last_seq FROM playbacks WHERE id=?1",[&receipt.playback_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db_error)?;
        if pid!=receipt.presentation_id||revision!=receipt.presentation_revision||lease!=receipt.output_lease_id{return Err("playback_receipt_binding".into());}
        let serialized=serde_json::to_string(&receipt).map_err(|e|e.to_string())?;
        if serialized.len()>16_384{return Err("playback_receipt_limit".into());}
        let existing:Option<String>=tx.query_row("SELECT receipt_json FROM playback_receipts WHERE receipt_id=?1",[&receipt.receipt_id],|r|r.get(0)).optional().map_err(db_error)?;
        if let Some(existing)=existing{if existing!=serialized{return Err("playback_receipt_conflict".into());}return Ok(false);}
        if receipt.sequence>i64::MAX as u64||receipt.sequence as i64<=last{return Err("playback_receipt_sequence".into());}
        // Historical device evidence is retained after expiry; the return flag
        // tells callers whether it may update the active user-facing view.
        let current=require_lease(&tx,&receipt.output_lease_id).is_ok();
        tx.execute("INSERT INTO playback_receipts(receipt_id,playback_id,sequence,receipt_json,received_at_ms) VALUES(?1,?2,?3,?4,?5)",params![receipt.receipt_id,receipt.playback_id,receipt.sequence as i64,serialized,now()]).map_err(db_error)?;
        tx.execute("UPDATE playbacks SET last_seq=?2,state=?3 WHERE id=?1",params![receipt.playback_id,receipt.sequence as i64,receipt.state]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;Ok(current)
    }
    pub fn receipts(&self,presentation_id:&str)->Result<Vec<Value>,String>{
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let mut statement=db.prepare("SELECT r.receipt_json FROM playback_receipts r JOIN playbacks p ON p.id=r.playback_id WHERE p.presentation_id=?1 ORDER BY r.received_at_ms,r.sequence LIMIT 1000").map_err(db_error)?;
        let rows=statement.query_map([presentation_id],|r|r.get::<_,String>(0)).map_err(db_error)?;
        rows.map(|r|r.map(|text|serde_json::from_str::<Value>(&text).unwrap_or(Value::Null))).collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }
}
fn now()->i64{crate::time::now_ms() as i64}
fn deliverable(state:TaskState)->bool{matches!(state,TaskState::Succeeded|TaskState::Partial|TaskState::Failed|TaskState::Cancelled|TaskState::Unknown|TaskState::AwaitingInput)}
fn result_body(state:TaskState,result:&Value)->String{
    if let Some(text)=result["summary"].as_str().map(str::trim).filter(|text|!text.is_empty()){return text.to_owned();}
    match state{
        TaskState::AwaitingInput=>"还需要补充一些信息。".into(),
        TaskState::Failed=>"这件事暂时没有完成。".into(),
        TaskState::Cancelled=>"这项任务已经取消。".into(),
        TaskState::Unknown=>"暂时还不能确认结果。".into(),
        _=>"任务有了新的结果。".into(),
    }
}
fn recipient_scope(input:&Value)->(String,i64){
    let person=input["private_profile_id"].as_str().filter(|text|!text.is_empty()).or_else(||input["recipient_person_id"].as_str().filter(|text|!text.is_empty()));
    if let Some(person)=person{if super::artifacts::valid_id(person).is_ok(){return (format!("person:{person}"),1);}}
    ("household".into(),0)
}
struct InboxRow{delivery_id:String,revision:i64,task_id:String,kind:String,scope:String,result_text:String,summary:String,promise:String,state:String,lease:Option<String>,hold:Option<String>,receipts:String,tell_me:bool,private_scope:bool}
fn load_inbox(db:&Connection,id:&str)->Result<InboxRow,String>{
    db.query_row("SELECT delivery_id,delivery_revision,task_id,kind,recipient_scope,result_ref,summary_ref,promise,state,attempted_lease_id,hold_activation,receipt_refs,tell_me,private_scope FROM delivery_inbox WHERE delivery_id=?1",[id],|r|Ok(InboxRow{
        delivery_id:r.get(0)?,revision:r.get(1)?,task_id:r.get(2)?,kind:r.get(3)?,scope:r.get(4)?,result_text:r.get(5)?,summary:r.get(6)?,promise:r.get(7)?,state:r.get(8)?,lease:r.get(9)?,hold:r.get(10)?,receipts:r.get(11)?,tell_me:r.get::<_,i64>(12)?!=0,private_scope:r.get::<_,i64>(13)?!=0,
    })).map_err(db_error)
}
fn scope_visible(scope:&str,private_scope:bool,speaker:Option<&str>)->bool{
    if !private_scope{return scope=="household";}
    speaker.is_some_and(|speaker|scope==format!("person:{speaker}"))
}
fn inbox_hint_due(db:&Connection)->Result<bool,String>{
    let fresh=now()-super::task_delivery::PARTIAL_WAIT_MS;
    db.query_row("SELECT EXISTS(SELECT 1 FROM delivery_inbox WHERE migrated=0 AND private_scope=0 AND (state IN ('pending_delivery','offered','deferred') OR (state='partial' AND created_at_ms>=?1)))",[fresh],|r|r.get(0)).map_err(db_error)
}
struct UnstoredResult{id:String,state:Option<String>,stage:String,result:Option<Value>}
fn prune_inbox(db:&Connection)->Result<(),String>{
    let now=now();
    let cutoff=now-super::task_delivery::RETENTION_MS;
    db.execute("DELETE FROM delivery_inbox WHERE state IN ('delivered','invalidated') AND created_at_ms < ?1",[cutoff]).map_err(db_error)?;
    db.execute("DELETE FROM delivery_inbox WHERE private_scope=1 AND state!='offered' AND created_at_ms < ?1",[cutoff]).map_err(db_error)?;
    db.execute("DELETE FROM delivery_inbox WHERE state!='offered' AND expires_at_ms IS NOT NULL AND expires_at_ms < ?1",[now]).map_err(db_error)?;
    db.execute("UPDATE delivery_inbox SET state='delivered',promise='full',delivery_revision=delivery_revision+1 WHERE state='partial' AND created_at_ms < ?1",[now-super::task_delivery::PARTIAL_WAIT_MS]).map_err(db_error)?;
    for _ in 0..8 {
        let open:i64=db.query_row("SELECT count(*) FROM delivery_inbox WHERE migrated=0 AND state IN ('pending_delivery','partial','deferred')",[],|r|r.get(0)).map_err(db_error)?;
        if open<super::task_delivery::MAX_UNDELIVERED {break;}
        let oldest:Option<String>=db.query_row("SELECT delivery_id FROM delivery_inbox WHERE migrated=0 AND state IN ('pending_delivery','partial','deferred') ORDER BY CASE WHEN private_scope=1 OR tell_me=0 THEN 0 WHEN state='deferred' THEN 1 WHEN state='partial' THEN 2 ELSE 3 END, created_at_ms, delivery_id LIMIT 1",[],|r|r.get(0)).optional().map_err(db_error)?;
        let Some(oldest)=oldest else {break;};
        db.execute("DELETE FROM delivery_inbox WHERE delivery_id=?1",[oldest]).map_err(db_error)?;
    }
    for _ in 0..64 {
        let count:i64=db.query_row("SELECT count(*) FROM delivery_inbox",[],|r|r.get(0)).map_err(db_error)?;
        if count<super::task_delivery::MAX_INBOX_ROWS {break;}
        let oldest:Option<String>=db.query_row("SELECT delivery_id FROM delivery_inbox WHERE state!='offered' ORDER BY CASE state WHEN 'delivered' THEN 0 WHEN 'invalidated' THEN 1 WHEN 'deferred' THEN 2 WHEN 'partial' THEN 3 ELSE 4 END, CASE WHEN migrated=1 OR private_scope=1 OR tell_me=0 THEN 0 ELSE 1 END, created_at_ms, delivery_id LIMIT 1",[],|r|r.get(0)).optional().map_err(db_error)?;
        let Some(oldest)=oldest else {break;};
        db.execute("DELETE FROM delivery_inbox WHERE delivery_id=?1",[oldest]).map_err(db_error)?;
    }
    db.execute("DELETE FROM result_outbox WHERE delivery_id NOT IN (SELECT delivery_id FROM delivery_inbox)",[]).map_err(db_error)?;
    db.execute("DELETE FROM result_outbox WHERE id NOT IN (SELECT id FROM result_outbox ORDER BY id DESC LIMIT ?1)",[super::task_delivery::MAX_OUTBOX]).map_err(db_error)?;
    Ok(())
}
fn store_terminal_inbox(db:&Connection,task:&TaskRecord,state:TaskState,result:&Value)->Result<i64,String>{
    if !deliverable(state){return Ok(task.result_revision as i64);}
    let body=super::task_delivery::bound_text(&result_body(state,result),super::task_delivery::MAX_RESULT_BYTES);
    let (spoken,promise_full)=if matches!(state,TaskState::Succeeded|TaskState::Partial){super::task_delivery::spoken_result(&body)}else{(body.clone(),true)};
    let spoken=super::task_delivery::bound_text(&spoken,super::task_delivery::MAX_RESULT_BYTES);
    let kind=if state==TaskState::AwaitingInput{"needs_input"}else{"result"};
    let (scope,private)=recipient_scope(&task.input);
    let tell=task.input["tell_when_done"].as_bool().unwrap_or(false)||super::task_delivery::asks_to_be_told(task.input["prompt"].as_str().unwrap_or(""));
    let invalidated=source_invalidated(db,&task.conversation_turn_id)?;
    let same:Option<i64>=db.query_row("SELECT result_revision FROM delivery_inbox WHERE task_id=?1 AND kind=?2 AND result_ref=?3 AND recipient_scope=?4 AND state NOT IN ('invalidated','delivered') ORDER BY result_revision DESC LIMIT 1",params![task.task_id,kind,body,scope],|r|r.get(0)).optional().map_err(db_error)?;
    if let Some(revision)=same{return Ok(revision);}
    prune_inbox(db)?;
    let revision=(task.result_revision as i64).saturating_add(1).max(1);
    let delivery_id=format!("delivery_{}",uuid::Uuid::new_v4().simple());
    let inbox_state=if invalidated{"invalidated"}else{"pending_delivery"};
    let promise=if promise_full{"full"}else{"summary"};
    let expires=now()+super::task_delivery::RETENTION_MS;
    db.execute("INSERT INTO delivery_inbox(delivery_id,delivery_revision,task_id,result_revision,kind,recipient_scope,result_ref,summary_ref,source_refs,promise,state,receipt_refs,confirmed_prefix,created_at_ms,expires_at_ms,tell_me,private_scope,migrated) VALUES(?1,1,?2,?3,?4,?5,?6,?7,'[]',?8,?9,'[]','',?10,?11,?12,?13,0)",
        params![delivery_id,task.task_id,revision,kind,scope,body,spoken,promise,inbox_state,now(),expires,if tell {1i64} else {0},private]).map_err(db_error)?;
    if inbox_state=="pending_delivery"{db.execute("INSERT INTO result_outbox(delivery_id,delivery_revision,created_at_ms) VALUES(?1,1,?2)",params![delivery_id,now()]).map_err(db_error)?;}
    Ok(revision)
}
fn migrate_delivery(db:&Connection)->Result<(),String>{
    ensure_column(db,"tasks","task_revision","INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(db,"tasks","result_revision","INTEGER NOT NULL DEFAULT 0")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS delivery_inbox(
        delivery_id TEXT PRIMARY KEY,delivery_revision INTEGER NOT NULL,task_id TEXT NOT NULL,result_revision INTEGER NOT NULL,
        kind TEXT NOT NULL,recipient_scope TEXT NOT NULL,result_ref TEXT NOT NULL,summary_ref TEXT NOT NULL,
        source_refs TEXT NOT NULL DEFAULT '[]',promise TEXT NOT NULL,state TEXT NOT NULL,
        attempted_lease_id TEXT,playback_id TEXT,receipt_refs TEXT NOT NULL DEFAULT '[]',confirmed_prefix TEXT NOT NULL DEFAULT '',
        created_at_ms INTEGER NOT NULL,due_at_ms INTEGER,expires_at_ms INTEGER,tell_me INTEGER NOT NULL DEFAULT 0,
        hold_activation TEXT,migrated INTEGER NOT NULL DEFAULT 0,private_scope INTEGER NOT NULL DEFAULT 0,
        UNIQUE(task_id,result_revision,recipient_scope,kind));
      CREATE TABLE IF NOT EXISTS result_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,delivery_id TEXT NOT NULL,delivery_revision INTEGER NOT NULL,created_at_ms INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS foreground_responses(turn_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,producer_closed INTEGER NOT NULL,outcome TEXT,task_id TEXT,updated_at_ms INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS inbox_migration(id INTEGER PRIMARY KEY CHECK(id=1));
      CREATE INDEX IF NOT EXISTS inbox_ready ON delivery_inbox(state,created_at_ms);").map_err(db_error)?;
    ensure_column(db,"delivery_inbox","offer_not_before_ms","INTEGER")?;
    let done:i64=db.query_row("SELECT count(*) FROM inbox_migration",[],|r|r.get(0)).map_err(db_error)?;
    if done==0 {
        backfill_inbox(db)?;
        db.execute("INSERT INTO inbox_migration(id) VALUES(1)",[]).map_err(db_error)?;
    }
    Ok(())
}
fn ensure_column(db:&Connection,table:&str,column:&str,decl:&str)->Result<(),String>{
    let mut statement=db.prepare(&format!("PRAGMA table_info({table})")).map_err(db_error)?;
    let names=statement.query_map([],|r|r.get::<_,String>(1)).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    if names.iter().any(|name|name==column){return Ok(());}
    db.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),[]).map_err(db_error)?;Ok(())
}
fn backfill_inbox(db:&Connection)->Result<(),String>{
    let room:i64=db.query_row("SELECT count(*) FROM delivery_inbox",[],|r|r.get(0)).map_err(db_error)?;
    let limit=(super::task_delivery::MAX_INBOX_ROWS-room).max(0);
    if limit==0{return Ok(());}
    let mut statement=db.prepare("SELECT id FROM tasks WHERE state IN ('succeeded','partial','failed','cancelled','unknown','awaiting_input') AND id NOT IN (SELECT task_id FROM delivery_inbox) ORDER BY updated_at_ms DESC LIMIT ?1").map_err(db_error)?;
    let rows=statement.query_map([limit],|r|r.get::<_,String>(0)).map_err(db_error)?;
    let ids=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    drop(statement);
    for id in ids{
        let task=query_task(db,&id)?;
        let body=super::task_delivery::bound_text(&result_body(task.state,&task.result),super::task_delivery::MAX_RESULT_BYTES);
        let kind=if task.state==TaskState::AwaitingInput{"needs_input"}else{"result"};
        let (scope,private)=recipient_scope(&task.input);
        let invalidated=source_invalidated(db,&task.conversation_turn_id)?;
        let revision=(task.result_revision as i64).max(1);
        let delivery_id=format!("delivery_{}",uuid::Uuid::new_v4().simple());
        let state=if invalidated{"invalidated"}else{"deferred"};
        db.execute("INSERT OR IGNORE INTO delivery_inbox(delivery_id,delivery_revision,task_id,result_revision,kind,recipient_scope,result_ref,summary_ref,source_refs,promise,state,receipt_refs,confirmed_prefix,created_at_ms,tell_me,private_scope,migrated) VALUES(?1,1,?2,?3,?4,?5,?6,?6,'[]','full',?7,'[]','',?8,0,?9,1)",
            params![delivery_id,task.task_id,revision,kind,scope,body,state,now(),private]).map_err(db_error)?;
        if task.result_revision==0{db.execute("UPDATE tasks SET result_revision=1 WHERE id=?1 AND result_revision=0",[&task.task_id]).map_err(db_error)?;}
    }
    Ok(())
}
fn fence_invalid_inbox(db:&Connection)->Result<(),String>{
    db.execute("UPDATE delivery_inbox SET state='invalidated',delivery_revision=delivery_revision+1 WHERE state!='invalidated' AND task_id IN (SELECT t.id FROM tasks t JOIN invalidated_sources s ON s.turn_id=t.turn_id)",[]).map_err(db_error)?;Ok(())
}
fn invalidate_inbox_turns(db:&Connection,turns:&[String])->Result<(),String>{
    for turn in turns{
        db.execute("UPDATE delivery_inbox SET state='invalidated',delivery_revision=delivery_revision+1 WHERE state!='invalidated' AND task_id IN (SELECT id FROM tasks WHERE turn_id=?1)",[turn]).map_err(db_error)?;
    }
    Ok(())
}
fn view_of(row:&InboxRow)->InboxView{
    InboxView{delivery_id:row.delivery_id.clone(),revision:row.revision,state:row.state.clone(),kind:row.kind.clone(),promise_full:row.promise=="full",summary:row.summary.clone(),result_text:row.result_text.clone(),tell_me:row.tell_me,private_scope:row.private_scope,receipts:row.receipts.clone()}
}
fn storage_retryable(error:&str)->bool{error.contains("task_store")||error.contains("database is locked")||error.contains("disk")}
fn release_offered_rows(db:&Connection)->Result<usize,String>{
    db.execute("UPDATE delivery_inbox SET state='pending_delivery',attempted_lease_id=NULL,playback_id=NULL,hold_activation=NULL,delivery_revision=delivery_revision+1 WHERE state='offered'",[]).map_err(db_error)
}
fn settle_interrupted(db:&Connection,stage:&str)->Result<(),String>{
    let mut statement=db.prepare("SELECT id,state,capability FROM tasks WHERE state IN ('running','cancel_requested')").map_err(db_error)?;
    let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(db_error)?;
    let found=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    drop(statement);
    for (id,state,capability) in found {
        let task=query_task(db,&id)?;
        let media=capability=="media.prepare"||capability=="presentation.render";
        let next=if state=="cancel_requested" && media {TaskState::Cancelled} else if media {TaskState::Queued} else {TaskState::Unknown};
        let mut result=task.result.clone();
        if !result.is_object(){result=json!({});}
        if result["summary"].as_str().is_none_or(|text|text.trim().is_empty()){result["summary"]=json!(result_body(next,&result));}
        let revision=if deliverable(next) {store_terminal_inbox(db,&task,next,&result)?} else {task.result_revision as i64};
        db.execute("UPDATE tasks SET state=?2,stage=?3,result_json=?4,updated_at_ms=?5,result_revision=?6,task_revision=task_revision+1 WHERE id=?1",
            params![id,next.as_str(),stage,serde_json::to_string(&result).map_err(|e|e.to_string())?,now(),revision]).map_err(db_error)?;
    }
    Ok(())
}
fn query_task(db:&Connection,id:&str)->Result<TaskRecord,String>{
    db.query_row("SELECT id,household_id,daily_id,turn_id,capability,input_json,intent_key,state,stage,result_json,attempt,created_at_ms,updated_at_ms,authorized_effect,task_revision,result_revision FROM tasks WHERE id=?1",[id],|r|{
        let input:String=r.get(5)?;let result:String=r.get(9)?;let state:String=r.get(7)?;
        Ok(TaskRecord{task_id:r.get(0)?,household_id:r.get(1)?,daily_conversation_id:r.get(2)?,conversation_turn_id:r.get(3)?,capability:r.get(4)?,
            input:serde_json::from_str(&input).unwrap_or(Value::Null),intent_key:r.get(6)?,state:TaskState::parse(&state),stage:r.get(8)?,result:serde_json::from_str(&result).unwrap_or(Value::Null),attempt:r.get(10)?,created_at_ms:r.get(11)?,updated_at_ms:r.get(12)?,authorized_effect:r.get(13)?,
            task_revision:r.get::<_,i64>(14)? as u64,result_revision:r.get::<_,i64>(15)? as u64})
    }).map_err(db_error)
}
fn append_event(db:&Connection,id:&str,kind:&str,payload:&Value)->Result<(),String>{
    db.execute("INSERT INTO task_events(event_id,task_id,kind,payload_json,created_at_ms) VALUES(?1,?2,?3,?4,?5)",params![format!("event_{}",uuid::Uuid::new_v4().simple()),id,kind,serde_json::to_string(payload).map_err(|e|e.to_string())?,now()]).map_err(db_error)?;Ok(())}
fn require_lease(db:&Connection,id:&str)->Result<(),String>{
    let valid:bool=db.query_row("SELECT revoked=0 AND expires_at_ms>?2 FROM output_leases WHERE id=?1",params![id,now()],|r|r.get(0)).optional().map_err(db_error)?.unwrap_or(false);
    if valid{Ok(())}else{Err("output_lease_expired".into())}}
fn db_error(e:impl std::fmt::Display)->String{format!("task_store: {e}")}
fn source_invalidated(db:&Connection,id:&str)->Result<bool,String>{db.query_row("SELECT EXISTS(SELECT 1 FROM invalidated_sources WHERE turn_id=?1)",[id],|r|r.get(0)).map_err(db_error)}
fn presentation_sources_invalidated(db:&Connection,id:&str)->Result<bool,String>{db.query_row("SELECT EXISTS(SELECT 1 FROM presentation_sources p JOIN invalidated_sources s ON s.turn_id=p.source_id WHERE p.presentation_id=?1)",[id],|r|r.get(0)).map_err(db_error)}
struct RedactionGuard<'a>(&'a AtomicBool);
impl Drop for RedactionGuard<'_>{fn drop(&mut self){self.0.store(false,Ordering::Release);}}
fn effect_tombstone(value:&Value)->Value{
    let mut tombstone=json!({"content_deleted":true});for field in ["job_id","printer","job_title","submission_started","cancel_requested","reconciled_at_ms"]{
        if let Some(value)=value.get(field).or_else(||value.get("data").and_then(|data|data.get(field))){tombstone[field]=value.clone();}}
    if let Some(state)=value.pointer("/cups_receipt/job-state").or_else(||value.get("cups_job_state")){tombstone["cups_job_state"]=state.clone();}tombstone
}
fn preserve_cancel_request(current:&Value,next:&mut Value){
    if current["cancel_requested"]==true||current.pointer("/data/cancel_requested").and_then(Value::as_bool)==Some(true){
        if !next.is_object(){*next=json!({});}next["cancel_requested"]=json!(true);
    }
}
fn confirmed_print_terminal(task:&TaskRecord)->Option<TaskState>{
    if task.capability!="print.submit"{return None;}
    let code=task.result.pointer("/cups_receipt/job-state").or_else(||task.result.pointer("/data/cups_receipt/job-state"))
        .or_else(||task.result.get("cups_job_state")).and_then(Value::as_i64)?;
    let state=match code{7=>TaskState::Cancelled,8=>TaskState::Failed,9=>TaskState::Succeeded,_=>return None};
    (task.state==state).then_some(state)
}
fn semantic_content(plan:&PresentationPlan)->Result<Value,String>{
    let mut value=serde_json::to_value(plan).map_err(|e|e.to_string())?;if let Some(object)=value.as_object_mut(){object.remove("revision");}
    if let Some(segments)=value["segments"].as_array_mut(){for segment in segments{if let Some(object)=segment.as_object_mut(){object.remove("asset_ids");object.remove("speech_asset_id");}}}Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_cancelled_running_task_cannot_finish_successfully_or_be_replayed() {
        let dir = std::env::temp_dir().join(format!("xiaodai-task-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let entered = started.clone();
        service.register_handler("agent.work", Arc::new(move |mut context| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.notify_one();
                context.cancelled.changed().await.unwrap();
                // A late upstream success must not overwrite the user's cancellation.
                Ok(TaskResult::success("late result", vec![], json!({})))
            })
        })).unwrap();
        service.start().await.unwrap();
        let request = NewTask { household_id: "house".into(), daily_conversation_id: "day".into(),
            conversation_turn_id: "turn".into(), capability: "agent.work".into(),
            input: json!({}), intent_key: "intent".into() };
        let id = service.submit(request.clone(), false).unwrap().task_id;
        tokio::time::timeout(Duration::from_secs(5), started.notified()).await.unwrap();
        service.cancel(&id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while service.attempts(&id).unwrap()[0]["ended_at_ms"].is_null() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert_eq!(service.get(&id).unwrap().state, TaskState::Cancelled);
        service.stop().await;
        drop(service);
        let reopened = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let duplicate = reopened.submit(request, false).unwrap();
        assert_eq!(duplicate.task_id, id);
        assert_eq!(duplicate.state, TaskState::Cancelled);
        assert_eq!(reopened.attempts(&id).unwrap().len(), 1);
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_spoken_task_does_not_hide_the_next_result() {
        let dir = std::env::temp_dir().join(format!("xiaodai-task-lead-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let first = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn-first".into(),
            capability: "agent.work".into(), input: json!({"prompt": "第一件", "activation_id": "device-session"}),
            intent_key: "intent-first".into(),
        }, false).unwrap();
        service.set_state(&first.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "第一件做好了"}))).unwrap();
        let second = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn-second".into(),
            capability: "agent.work".into(), input: json!({"prompt": "第二件", "activation_id": "device-session"}),
            intent_key: "intent-second".into(),
        }, false).unwrap();
        service.set_state(&second.task_id, TaskState::AwaitingInput, "needs_input", Some(json!({"summary": "还缺第二件的年份"}))).unwrap();
        service.set_state(&first.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "第一件做好了", "lead_spoken_state": "succeeded"}))).unwrap();
        let next = service.ready_for_activation("house", "device-session").unwrap().unwrap();
        assert_eq!(next.task_id, second.task_id, "the already spoken result must not block the next one");
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn sample(service: &Arc<TaskService>, intent: &str, prompt: &str) -> TaskRecord {
        service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
            capability: "agent.work".into(), input: json!({"prompt": prompt, "activation_id": "session-a"}),
            intent_key: intent.into(),
        }, false).unwrap()
    }

    #[test]
    fn a_result_and_its_inbox_commit_together_and_survive_a_lost_notice() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-result", "查资料");
        let before = service.attempts(&task.task_id).unwrap().len();
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "明天有雨。"}))).unwrap();
        let stored = service.get(&task.task_id).unwrap();
        assert_eq!(stored.state, TaskState::Succeeded);
        assert!(stored.result_revision >= 1);
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert_eq!(row.state, "pending_delivery");
        assert!(row.promise_full);
        service.clear_outbox().unwrap();
        drop(service);
        let reopened = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let found = reopened.rescan_inbox(8).unwrap();
        assert_eq!(found, vec![row.delivery_id.clone()]);
        assert_eq!(reopened.attempts(&task.task_id).unwrap().len(), before, "recovery must not run the task again");
        assert_eq!(reopened.get(&task.task_id).unwrap().task_id, task.task_id);
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_store_does_not_claim_the_result_was_saved() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-full-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-full", "查资料");
        service.fail_next_result_write();
        let error = service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "有结果"}))).unwrap_err();
        assert_eq!(error, "task_store_full");
        assert_eq!(service.get(&task.task_id).unwrap().state, TaskState::Queued);
        assert!(service.inbox_state(&task.task_id).unwrap().is_none());
        assert_eq!(service.persist_pending(), vec![task.task_id.clone()]);
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "有结果"}))).unwrap();
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_claim_and_a_stale_lease_cannot_finish_delivery() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-claim-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-claim", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "短结果。"}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        let next = service.claim_delivery(&row.delivery_id, row.revision, "lease-a", "playback-a").unwrap();
        assert!(service.claim_delivery(&row.delivery_id, row.revision, "lease-b", "playback-b").is_err());
        assert_eq!(service.apply_playback(&row.delivery_id, row.revision, "lease-old", true, "短结果。", true).unwrap_err(), "delivery_conflict");
        assert_eq!(service.apply_playback(&row.delivery_id, next, "lease-other", true, "短结果。", true).unwrap_err(), "delivery_lease_mismatch");
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("offered"));
        assert_eq!(service.apply_playback(&row.delivery_id, next, "lease-a", false, "", false).unwrap(), "deferred");
        assert_eq!(service.apply_playback(&row.delivery_id, next + 1, "lease-other", true, "短结果。", true).unwrap_err(), "delivery_lease_mismatch");
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("deferred"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn summary_and_full_text_are_different_receipts() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-partial-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let long = "资".repeat(120);
        let task = sample(&service, "intent-long", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": long}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert!(!row.promise_full);
        assert!(row.summary.contains("要听完整的吗？"));
        assert!(!row.summary.contains(&"资".repeat(80)));
        let offered = service.claim_delivery(&row.delivery_id, row.revision, "lease-sum", "playback-sum").unwrap();
        assert_eq!(service.apply_playback(&row.delivery_id, offered, "lease-sum", true, &row.summary, false).unwrap(), "partial");
        let partial = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert!(partial.receipts.contains("lease-sum"));
        let offered = service.claim_delivery(&partial.delivery_id, partial.revision, "lease-full", "playback-full").unwrap();
        assert_eq!(service.apply_playback(&partial.delivery_id, offered, "lease-full", true, &partial.result_text, true).unwrap(), "delivered");
        let done = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert!(done.receipts.contains("lease-sum") && done.receipts.contains("lease-full"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn capacity_rejects_new_work_and_a_private_result_stays_with_its_person() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-cap-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        for index in 0..crate::brain::task_delivery::MAX_UNDELIVERED {
            service.submit(NewTask {
                household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
                capability: "agent.work".into(), input: json!({"prompt": "排队"}),
                intent_key: format!("intent-{index}"),
            }, false).unwrap();
        }
        let extra = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
            capability: "agent.work".into(), input: json!({"prompt": "再来一件"}), intent_key: "intent-overflow".into(),
        }, false).unwrap();
        assert!(extra.task_id.starts_with("task_"), "a full result cap evicts; it does not refuse the task");
        let again = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
            capability: "agent.work".into(), input: json!({"prompt": "排队"}), intent_key: "intent-0".into(),
        }, false).unwrap();
        assert!(again.task_id.starts_with("task_"));
        drop(service);
        let _ = std::fs::remove_dir_all(&dir);
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let private = service.submit(NewTask {
            household_id: "other".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn-p".into(),
            capability: "agent.work".into(), input: json!({"prompt": "私人", "private_profile_id": "person-a"}),
            intent_key: "intent-private".into(),
        }, false).unwrap();
        service.set_state(&private.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "只给甲。"}))).unwrap();
        assert!(service.next_for_delivery("other", "session-b", None).unwrap().is_none());
        assert!(!service.hint_pending(), "a private result is not a household hint");
        let visible = service.next_for_delivery("other", "session-b", Some("person-a")).unwrap().unwrap();
        assert_eq!(visible.task.task_id, private.task_id);
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancelling_one_task_does_not_drop_another_and_foreground_stays_open_until_the_receipt() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-life-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let kept = sample(&service, "intent-kept", "继续查");
        service.set_state(&kept.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "还在。"}))).unwrap();
        let other = sample(&service, "intent-stop", "另一件");
        service.cancel(&other.task_id).await.unwrap();
        assert_eq!(service.get(&other.task_id).unwrap().state, TaskState::Cancelled);
        assert_eq!(service.get(&kept.task_id).unwrap().state, TaskState::Succeeded);
        assert_eq!(service.inbox_state(&kept.task_id).unwrap().as_deref(), Some("pending_delivery"));
        service.note_foreground("turn-1", 1, true, None, Some(&kept.task_id)).unwrap();
        let open = service.foreground("turn-1").unwrap().unwrap();
        assert!(open["producer_closed"].as_bool().unwrap());
        assert!(open["outcome"].is_null());
        service.note_foreground("turn-1", 1, true, Some("delegated"), Some(&kept.task_id)).unwrap();
        assert_eq!(service.foreground("turn-1").unwrap().unwrap()["outcome"], "delegated");
        assert_eq!(service.get(&kept.task_id).unwrap().state, TaskState::Succeeded, "delegated does not finish the task");
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_revoked_source_invalidates_the_inbox_and_keeps_the_task_id() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-source-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-source", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "只属于原来的来源。"}))).unwrap();
        service.invalidate_sources(&["turn".into()], false).await.unwrap();
        assert_eq!(service.get(&task.task_id).unwrap().task_id, task.task_id);
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("invalidated"));
        assert!(service.next_for_delivery("house", "session-b", None).unwrap().is_none());
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn three_hundred_completions_leave_a_bounded_inbox() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-retain-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        for index in 0..320 {
            let task = service.submit(NewTask {
                household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
                capability: "agent.work".into(), input: json!({"prompt": "排队"}),
                intent_key: format!("intent-{index}"),
            }, false).unwrap();
            service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": format!("结果{index}")}))).unwrap();
        }
        let inbox = service.inbox_count().unwrap();
        let outbox = service.outbox_count().unwrap();
        assert!(inbox <= crate::brain::task_delivery::MAX_INBOX_ROWS, "inbox {inbox}");
        assert!(inbox <= crate::brain::task_delivery::MAX_UNDELIVERED, "undelivered rows were not evicted: {inbox}");
        assert!(outbox <= crate::brain::task_delivery::MAX_OUTBOX, "outbox {outbox}");
        assert!(service.next_for_delivery("house", "session-b", None).unwrap().is_some());
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_offered_row_returns_to_pending_on_release_and_restart() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-release-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-offer", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "短结果。"}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        service.claim_delivery(&row.delivery_id, row.revision, "lease-a", "playback-a").unwrap();
        assert_eq!(service.release_offered().unwrap(), 1);
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        let again = service.inbox_row(&task.task_id).unwrap().unwrap();
        service.claim_delivery(&again.delivery_id, again.revision, "lease-b", "playback-b").unwrap();
        drop(service);
        let reopened = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        assert_eq!(reopened.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        assert!(reopened.next_for_delivery("house", "session-b", None).unwrap().is_some());
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_summary_can_be_finished_declined_or_aged_out() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-expand-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let long = "资".repeat(120);
        let task = sample(&service, "intent-expand", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": long}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        let offered = service.claim_delivery(&row.delivery_id, row.revision, "lease-sum", "playback-sum").unwrap();
        assert_eq!(service.apply_playback(&row.delivery_id, offered, "lease-sum", true, &row.summary, false).unwrap(), "partial");
        let full = service.expansion_target("house", None).unwrap().unwrap();
        assert_eq!(full.result_text.chars().count(), 120);
        service.settle_partials("house").unwrap();
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("delivered"));
        assert!(service.expansion_target("house", None).unwrap().is_none());
        let second = sample(&service, "intent-age", "另一件");
        service.set_state(&second.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "资".repeat(90)}))).unwrap();
        let aged = service.inbox_row(&second.task_id).unwrap().unwrap();
        let offered = service.claim_delivery(&aged.delivery_id, aged.revision, "lease-age", "playback-age").unwrap();
        service.apply_playback(&aged.delivery_id, offered, "lease-age", true, &aged.summary, false).unwrap();
        service.age_row(&aged.delivery_id, crate::time::now_ms() as i64 - crate::brain::task_delivery::PARTIAL_WAIT_MS - 1).unwrap();
        assert!(service.expansion_target("house", None).unwrap().is_none(), "a summary older than the wait is not expanded");
        assert!(!service.hint_pending(), "a stale summary does not keep the curious hint");
        service.retain().unwrap();
        assert_eq!(service.inbox_state(&second.task_id).unwrap().as_deref(), Some("delivered"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_private_result_expires_when_nobody_can_hear_it() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-private-age-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let private = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn-p".into(),
            capability: "agent.work".into(), input: json!({"prompt": "私人", "private_profile_id": "person-a"}),
            intent_key: "intent-private-age".into(),
        }, false).unwrap();
        service.set_state(&private.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "只给甲。"}))).unwrap();
        let row = service.inbox_row(&private.task_id).unwrap().unwrap();
        service.age_row(&row.delivery_id, crate::time::now_ms() as i64 - crate::brain::task_delivery::RETENTION_MS - 1).unwrap();
        service.retain().unwrap();
        assert!(service.inbox_row(&private.task_id).unwrap().is_none());
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_resumed_task_with_the_same_text_gets_a_new_row() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-resume-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-resume", "查资料");
        service.set_state(&task.task_id, TaskState::AwaitingInput, "needs_input", Some(json!({"summary": "还缺年份"}))).unwrap();
        let first = service.inbox_row(&task.task_id).unwrap().unwrap();
        service.resume(&task.task_id, json!({"year": "2024"}), false).unwrap();
        service.set_state(&task.task_id, TaskState::AwaitingInput, "needs_input", Some(json!({"summary": "还缺年份"}))).unwrap();
        let second = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert_ne!(first.delivery_id, second.delivery_id);
        assert_eq!(service.inbox_by_id(&first.delivery_id).unwrap().unwrap().state, "invalidated");
        assert_eq!(second.state, "pending_delivery");
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shutdown_and_crash_recovery_write_a_speakable_row_once() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-recover-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-crash", "查资料");
        service.set_state(&task.task_id, TaskState::Running, "working", None).unwrap();
        drop(service);
        let recovered = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        assert_eq!(recovered.get(&task.task_id).unwrap().state, TaskState::Unknown);
        let row = recovered.inbox_row(&task.task_id).unwrap().unwrap();
        assert_eq!(row.state, "pending_delivery");
        assert!(recovered.next_for_delivery("house", "session-b", None).unwrap().is_some());
        drop(recovered);
        let again = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        assert_eq!(again.inbox_count().unwrap(), 1, "backfill does not add another row");
        assert_eq!(again.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        drop(again);
        let stopped = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let running = sample(&stopped, "intent-stop", "另一件");
        stopped.set_state(&running.task_id, TaskState::Running, "working", None).unwrap();
        // stop is async; the crash path above is the one that used to skip the inbox.
        drop(stopped);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stopping_the_service_files_the_running_task() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-stop-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-service-stop", "查资料");
        service.set_state(&task.task_id, TaskState::Running, "working", None).unwrap();
        service.stop().await;
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dropped_offer_is_not_planned_again_immediately() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-backoff-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = service.submit(NewTask {
            household_id: "house".into(), daily_conversation_id: "day".into(), conversation_turn_id: "turn".into(),
            capability: "agent.work".into(), input: json!({"prompt": "查好告诉我", "tell_when_done": true}),
            intent_key: "intent-backoff".into(),
        }, false).unwrap();
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "明天有雨。"}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert!(service.offer_ready(&row.delivery_id));
        service.note_offer_backoff(&row.delivery_id).unwrap();
        assert!(!service.offer_ready(&row.delivery_id));
        assert!(service.next_for_delivery("house", "session-b", None).unwrap().is_some(), "a wake can still speak it");
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn forgetting_a_source_erases_the_inbox_copy() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-forget-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-forget", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "不该留下的句子。"}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        service.forget_sources(&["turn".into()], false).await.unwrap();
        assert!(service.inbox_by_id(&row.delivery_id).unwrap().is_none());
        assert_eq!(service.outbox_count().unwrap(), 0);
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_late_release_cannot_clear_a_newer_claim() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-lease-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-lease", "查资料");
        service.set_state(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "短结果。"}))).unwrap();
        let row = service.inbox_row(&task.task_id).unwrap().unwrap();
        service.claim_delivery(&row.delivery_id, row.revision, "lease-live", "playback-live").unwrap();
        service.release_attempt(&row.delivery_id, "lease-stale", "").unwrap();
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("offered"));
        service.release_attempt(&row.delivery_id, "lease-live", "").unwrap();
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_heard_question_is_asked_again_after_resume() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-heard-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-heard", "查资料");
        service.set_state(&task.task_id, TaskState::AwaitingInput, "needs_input", Some(json!({"summary": "还缺年份"}))).unwrap();
        let first = service.inbox_row(&task.task_id).unwrap().unwrap();
        let offered = service.claim_delivery(&first.delivery_id, first.revision, "lease-heard", "playback-heard").unwrap();
        assert_eq!(service.apply_playback(&first.delivery_id, offered, "lease-heard", true, "还缺年份", true).unwrap(), "delivered");
        service.resume(&task.task_id, json!({"year": "2024"}), false).unwrap();
        service.set_state(&task.task_id, TaskState::AwaitingInput, "needs_input", Some(json!({"summary": "还缺年份"}))).unwrap();
        let second = service.inbox_row(&task.task_id).unwrap().unwrap();
        assert_ne!(first.delivery_id, second.delivery_id);
        assert_eq!(second.state, "pending_delivery");
        assert_eq!(service.inbox_by_id(&first.delivery_id).unwrap().unwrap().state, "delivered");
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_result_that_failed_to_store_is_retried_without_a_restart() {
        let dir = std::env::temp_dir().join(format!("xiaodai-inbox-retry-{}", uuid::Uuid::new_v4()));
        let service = TaskService::open(dir.clone(), TaskConfig::default(), Arc::new(|_| {})).unwrap();
        let task = sample(&service, "intent-retry", "查资料");
        service.fail_result_writes.store(3, std::sync::atomic::Ordering::SeqCst);
        service.commit_state_retry(&task.task_id, TaskState::Succeeded, "done", Some(json!({"summary": "有结果"}))).await;
        assert!(service.inbox_state(&task.task_id).unwrap().is_none());
        assert!(service.persist_pending().contains(&task.task_id));
        service.retry_persist_pending();
        assert_eq!(service.inbox_state(&task.task_id).unwrap().as_deref(), Some("pending_delivery"));
        assert!(!service.persist_pending().contains(&task.task_id));
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }
}
