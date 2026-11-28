//! Short-operation ledger in the existing tasks database, with no executor of
//! its own. Domain transactions return stable evidence IDs for reconciliation.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantOperation {
    pub operation_id: String, pub capability_id: String, pub source_turn: String,
    pub request_hash: String, pub state: String, pub result: Value,
}

pub(super) fn migrate(db: &Connection) -> Result<(), String> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS assistant_operations(
        operation_id TEXT PRIMARY KEY,capability_id TEXT NOT NULL,household_id TEXT NOT NULL,
        source_turn TEXT NOT NULL,source_revision INTEGER NOT NULL,private_profile_id TEXT,
        request_hash TEXT NOT NULL,state TEXT NOT NULL,result_json TEXT NOT NULL DEFAULT '{}',
        created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS assistant_operation_source ON assistant_operations(source_turn);
        UPDATE assistant_operations SET state='unknown' WHERE state='accepted_durable';").map_err(db_error)
}

impl TaskService {
    pub fn accept_operation(&self, id:&str, capability:&str, source:&super::super::contracts::SourceRef,
                            profile:Option<&str>, hash:&str) -> Result<AssistantOperation,String> {
        for value in [id,capability,source.household_id.as_str(),source.conversation_turn_id.as_str()] {valid_id(value)?;}
        if source.source_revision==0 || hash.len()!=64 || !hash.bytes().all(|b|b.is_ascii_hexdigit()) {return Err("operation_binding_invalid".into());}
        let mut db=self.db.lock().map_err(|_|"task_database_lock")?;
        let tx=db.transaction().map_err(db_error)?;
        if let Some(existing)=read(&tx,id)? {
            let binding:(String,u64,Option<String>)=tx.query_row("SELECT household_id,source_revision,private_profile_id FROM assistant_operations WHERE operation_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(db_error)?;
            if existing.request_hash!=hash || existing.capability_id!=capability || existing.source_turn!=source.conversation_turn_id
                || binding!=(source.household_id.clone(),source.source_revision,profile.map(str::to_owned)) {return Err("operation_intent_conflict".into());}
            return Ok(existing);
        }
        let count:usize=tx.query_row("SELECT count(*) FROM assistant_operations",[],|r|r.get(0)).map_err(db_error)?;
        if count>=self.config.max_tasks {return Err("operation_capacity".into());}
        if source_invalidated(&tx,&source.conversation_turn_id)? {return Err("operation_source_revoked".into());}
        tx.execute("INSERT INTO assistant_operations(operation_id,capability_id,household_id,source_turn,source_revision,private_profile_id,request_hash,state,created_at_ms,updated_at_ms)
            VALUES(?1,?2,?3,?4,?5,?6,?7,'accepted_durable',?8,?8)",params![id,capability,source.household_id,source.conversation_turn_id,source.source_revision,profile,hash,now()]).map_err(db_error)?;
        let operation=read(&tx,id)?.ok_or("operation_missing")?;
        tx.commit().map_err(db_error)?;
        Ok(operation)
    }
    pub fn finish_operation(&self,id:&str,hash:&str,state:&str,result:&Value)->Result<AssistantOperation,String> {
        if !matches!(state,"applied"|"failed"|"unknown") {return Err("operation_state_invalid".into());}
        let raw=serde_json::to_string(result).map_err(|e|e.to_string())?;
        if raw.len()>8192 {return Err("operation_receipt_limit".into());}
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let existing=read(&db,id)?.ok_or("operation_missing")?;
        if existing.request_hash!=hash {return Err("operation_intent_conflict".into());}
        if existing.state=="applied" || existing.state=="failed" {return Ok(existing);}
        db.execute("UPDATE assistant_operations SET state=?2,result_json=?3,updated_at_ms=?4 WHERE operation_id=?1 AND state IN ('accepted_durable','unknown')",params![id,state,raw,now()]).map_err(db_error)?;
        read(&db,id)?.ok_or_else(||"operation_missing".into())
    }
    pub fn unresolved_operations(&self)->Result<Vec<AssistantOperation>,String> {
        let db=self.db.lock().map_err(|_|"task_database_lock")?;
        let ids={let mut q=db.prepare("SELECT operation_id FROM assistant_operations WHERE state='unknown' ORDER BY created_at_ms LIMIT 32").map_err(db_error)?;
            let rows=q.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?;rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?};
        ids.iter().map(|id|read(&db,id)?.ok_or_else(||"operation_missing".into())).collect()
    }
}
fn read(db:&Connection,id:&str)->Result<Option<AssistantOperation>,String> {
    db.query_row("SELECT operation_id,capability_id,source_turn,request_hash,state,result_json FROM assistant_operations WHERE operation_id=?1",[id],|r|{
        let raw:String=r.get(5)?;
        Ok(AssistantOperation{operation_id:r.get(0)?,capability_id:r.get(1)?,source_turn:r.get(2)?,request_hash:r.get(3)?,state:r.get(4)?,result:serde_json::from_str(&raw).unwrap_or(Value::Null)})
    }).optional().map_err(db_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_admission_is_idempotent_and_restart_never_replays_unknown_effects() {
        let root=std::env::temp_dir().join(format!("xiaodai-operations-{}",uuid::Uuid::new_v4()));
        let service=TaskService::open(root.clone(),TaskConfig::default(),Arc::new(|_|{})).unwrap();
        let source=super::super::super::contracts::SourceRef{household_id:"house".into(),daily_conversation_id:"day".into(),episode_id:"episode".into(),conversation_turn_id:"turn".into(),utterance_id:"utterance".into(),source_revision:1};
        let hash="a".repeat(64);
        let accepted=service.accept_operation("op-test","reminder.create",&source,None,&hash).unwrap();
        assert_eq!(accepted.state,"accepted_durable");
        assert_eq!(service.accept_operation("op-test","reminder.create",&source,None,&hash).unwrap().operation_id,accepted.operation_id);
        assert!(service.accept_operation("op-test","reminder.create",&source,None,&"b".repeat(64)).is_err());
        drop(service);
        let service=TaskService::open(root.clone(),TaskConfig::default(),Arc::new(|_|{})).unwrap();
        assert_eq!(service.unresolved_operations().unwrap()[0].state,"unknown");
        assert_eq!(service.accept_operation("op-test","reminder.create",&source,None,&hash).unwrap().state,"unknown");
        let receipt=json!({"reminder_id":"reminder-real"});
        service.finish_operation("op-test",&hash,"applied",&receipt).unwrap();
        assert_eq!(service.finish_operation("op-test",&hash,"failed",&json!({})).unwrap().result,receipt);
        assert!(service.unresolved_operations().unwrap().is_empty());
        drop(service);let _=std::fs::remove_dir_all(root);
    }
}
