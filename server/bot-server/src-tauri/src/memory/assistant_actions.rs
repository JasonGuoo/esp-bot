//! Domain evidence, committed atomically with a reminder. The cross-domain
//! operation ledger lives in tasks.sqlite3; it only references these facts.
use rusqlite::{params,OptionalExtension};
use serde_json::{json,Value};
use super::{Store,store::{clean_text,db_error,now_ms,validate_id}};
use crate::brain::control_contracts::TrustedActionContext;

pub enum ReminderAction { Create { text:String, due_at_ms:i64 }, Cancel { id:String } }

impl Store {
    pub fn assistant_reminder(&self,ctx:&TrustedActionContext,hash:&str,action:ReminderAction)->Result<Value,String> {
        let operation=ctx.operation.as_ref().ok_or("operation_required")?;
        validate_id(&operation.operation_id)?;
        let mut db=self.connection()?;let tx=db.transaction().map_err(db_error)?;
        // The model cannot manufacture a SourceRef or a private scope. Check
        // every host binding again in the same transaction as the effect.
        let source=&ctx.source;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM household_turns t
            JOIN household_episodes e ON e.id=t.episode_id JOIN household_days d ON d.id=e.daily_id
            JOIN households h ON h.id=d.household_id
            WHERE t.id=?1 AND t.source_revision=?2 AND t.forgotten=0 AND t.source_expired=0
              AND t.episode_id=?3 AND t.utterance_id=?4 AND d.id=?5 AND h.id=?6 AND h.policy_revision=?7
              AND t.private_profile_id IS ?8)",params![source.conversation_turn_id,source.source_revision,source.episode_id,
                source.utterance_id,source.daily_conversation_id,source.household_id,ctx.policy_epoch,ctx.private_profile_id],|r|r.get(0)).map_err(db_error)?;
        if !valid {return Err("action_source_or_scope_changed".into());}
        if let Some((previous,receipt))=tx.query_row("SELECT request_hash,result_json FROM assistant_domain_receipts WHERE operation_id=?1",[&operation.operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db_error)? {
            if previous!=hash {return Err("operation_intent_conflict".into());}
            return serde_json::from_str(&receipt).map_err(|_|"operation_receipt_invalid".into());
        }
        let receipt=match action {
            ReminderAction::Create{text,due_at_ms}=>{
                let text=clean_text(&text,80,false,"reminder text")?;let now=now_ms();
                if due_at_ms<=now || due_at_ms>now+366*86_400_000 {return Err("reminder_time_out_of_range".into());}
                let count:u64=tx.query_row("SELECT count(*) FROM proactive_reminders WHERE state='pending'",[],|r|r.get(0)).map_err(db_error)?;
                if count>=256 {return Err("reminder_capacity".into());}
                let id=format!("reminder-{}",uuid::Uuid::new_v4().simple());
                tx.execute("INSERT INTO proactive_reminders(id,profile_id,text,due_at_ms,state,attempts,next_at_ms,created_at_ms)
                    VALUES(?1,?2,?3,?4,'pending',0,?4,?5)",params![id,ctx.private_profile_id,text,due_at_ms,now]).map_err(db_error)?;
                json!({"reminder_id":id,"due_at_ms":due_at_ms,"state":"pending","created":true})
            },
            ReminderAction::Cancel{id}=>{
                validate_id(&id)?;
                let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM proactive_reminders WHERE id=?1 AND profile_id IS ?2)",params![id,ctx.private_profile_id],|r|r.get(0)).map_err(db_error)?;
                if !allowed {return Err("reminder_not_in_scope".into());}
                let changed=tx.execute("UPDATE proactive_reminders SET state='cancelled' WHERE id=?1 AND state='pending'",[&id]).map_err(db_error)?==1;
                if changed {tx.execute("UPDATE proactive_journal SET status='declined',reason='reminder_cancelled' WHERE trigger_source=?1 AND status='offered'",[format!("reminder:{id}")]).map_err(db_error)?;}
                let state:String=tx.query_row("SELECT state FROM proactive_reminders WHERE id=?1",[&id],|r|r.get(0)).map_err(db_error)?;
                json!({"reminder_id":id,"state":state,"changed":changed})
            }
        };
        tx.execute("INSERT INTO assistant_domain_receipts(operation_id,request_hash,source_turn,source_revision,reminder_id,result_json,committed_at_ms)
            VALUES(?1,?2,?3,?4,?5,?6,?7)",params![operation.operation_id,hash,source.conversation_turn_id,source.source_revision,
                receipt["reminder_id"].as_str(),receipt.to_string(),now_ms()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        if receipt["changed"]==true { self.revoke_reminder_playback(receipt["reminder_id"].as_str()); }
        Ok(receipt)
    }

    pub fn assistant_domain_receipt(&self,operation_id:&str,hash:&str)->Result<Option<Value>,String> {
        validate_id(operation_id)?;let db=self.connection()?;
        let raw:Option<String>=db.query_row("SELECT r.result_json FROM assistant_domain_receipts r WHERE r.operation_id=?1 AND r.request_hash=?2",params![operation_id,hash],|r|r.get(0)).optional().map_err(db_error)?;
        raw.map(|raw|serde_json::from_str(&raw).map_err(|_|"operation_receipt_invalid".into())).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::{memory::{JournalInput,MemoryConfig},brain::{contracts::SourceRef,control_contracts::OperationRef}};
    fn context(store:&Store,turn:&str,operation:&str)->TrustedActionContext {
        let r=store.journal_input(&JournalInput{turn_id:turn.into(),activation_id:"test-activation".into(),utterance_id:None,text:"一分钟后提醒我喝水".into(),occurred_at_ms:now_ms(),input_kind:"text".into(),speaker_person_id:None,private_profile_id:None,allow_cloud:false,metadata:json!({})}).unwrap();
        TrustedActionContext{request_id:turn.into(),service_epoch:"test-service".into(),source:SourceRef{household_id:r.household_id,daily_conversation_id:r.daily_conversation_id,episode_id:r.episode_id,conversation_turn_id:r.conversation_turn_id,utterance_id:r.utterance_id,source_revision:r.source_revision},private_profile_id:None,policy_epoch:r.policy_revision,snapshot_version:1,catalog_version:1,foreground:None,operation:Some(OperationRef{operation_id:operation.into(),operation_revision:1,task_id:None}),output_lease_id:None}
    }
    #[test]
    fn concurrent_intent_retry_has_one_effect_and_survives_reopen() {
        let root=std::env::temp_dir().join(format!("xiaodai-reminder-actions-{}",uuid::Uuid::new_v4()));
        let store=Store::open(root.clone(),MemoryConfig::default()).unwrap();
        let ctx=context(&store,"turn-1","op-1");let due=now_ms()+60_000;let hash="a".repeat(64);
        let threads=(0..4).map(|_|{let store=Arc::clone(&store);let ctx=ctx.clone();let hash=hash.clone();std::thread::spawn(move||store.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:due}).unwrap())}).collect::<Vec<_>>();
        let results=threads.into_iter().map(|t|t.join().unwrap()).collect::<Vec<_>>();
        assert!(results.iter().all(|r|r["reminder_id"]==results[0]["reminder_id"]));
        assert_eq!(store.reminders(10).unwrap().len(),1);
        assert!(store.assistant_reminder(&ctx,&"b".repeat(64),ReminderAction::Create{text:"别的".into(),due_at_ms:due}).is_err());
        drop(store);let store=Store::open(root.clone(),MemoryConfig::default()).unwrap();
        assert_eq!(store.assistant_domain_receipt("op-1",&hash).unwrap(),Some(results[0].clone()));
        let mut wrong=ctx.clone();wrong.operation.as_mut().unwrap().operation_id="op-wrong".into();wrong.source.utterance_id="unrelated".into();
        assert!(store.assistant_reminder(&wrong,&hash,ReminderAction::Create{text:"越权".into(),due_at_ms:due}).is_err());
        assert_eq!(store.reminders(10).unwrap().len(),1);
        let cancel=context(&store,"turn-2","op-cancel");
        let id=results[0]["reminder_id"].as_str().unwrap().to_owned();
        let result=store.assistant_reminder(&cancel,&hash,ReminderAction::Cancel{id:id.clone()}).unwrap();
        assert_eq!(result["state"],"cancelled");
        assert_eq!(store.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:due}).unwrap(),results[0]);
        assert_eq!(store.reminders(10).unwrap()[0].state,"cancelled");
        drop(store);let _=std::fs::remove_dir_all(root);
    }
    #[test]
    fn archive_expiry_preserves_explicit_reminders_but_source_forgetting_revokes_them(){
        let root=std::env::temp_dir().join(format!("xiaodai-reminder-retention-{}",uuid::Uuid::new_v4()));
        let store=Store::open(root.clone(),MemoryConfig::default()).unwrap();
        let ctx=context(&store,"archived","op-archive");let hash="c".repeat(64);
        store.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:now_ms()+86_400_000}).unwrap();
        // The real archive expiry path sees this source as older than its TTL.
        store.connection().unwrap().execute("UPDATE household_turns SET occurred_at_ms=1 WHERE id=?1",[&ctx.source.conversation_turn_id]).unwrap();
        store.purge_household_expired().unwrap();
        assert_eq!(store.reminders(10).unwrap()[0].state,"pending");
        assert!(store.assistant_domain_receipt("op-archive",&hash).unwrap().is_some());
        store.forget_all_household().unwrap();
        assert_eq!(store.reminders(10).unwrap()[0].state,"cancelled");
        assert!(store.assistant_domain_receipt("op-archive",&hash).unwrap().is_none());
        drop(store);let _=std::fs::remove_dir_all(root);
    }
    #[test]
    fn correction_and_private_scope_are_checked_by_the_domain_transaction(){
        let root=std::env::temp_dir().join(format!("xiaodai-reminder-scope-{}",uuid::Uuid::new_v4()));
        let store=Store::open(root.clone(),MemoryConfig::default()).unwrap();
        store.update_household_settings(&json!({"history_enabled":true})).unwrap();
        let ctx=context(&store,"source","op-original");let hash="d".repeat(64);
        let receipt=store.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:now_ms()+60_000}).unwrap();
        let mut changes=store.reminder_revocations();
        store.household_rpc("household.correct",&json!({"conversation_turn_id":ctx.source.conversation_turn_id,
            "expected_revision":ctx.source.source_revision,"text":"不要提醒我喝水"})).unwrap();
        assert_eq!(changes.try_recv().unwrap(),None);
        assert!(store.assistant_domain_receipt("op-original",&hash).unwrap().is_none());
        assert_eq!(store.reminders(10).unwrap()[0].state,"cancelled");
        assert!(store.assistant_reminder(&ctx,&hash,ReminderAction::Create{text:"喝水".into(),due_at_ms:now_ms()+60_000}).is_err());
        let profile=store.create_profile(crate::memory::NewProfile{display_name:"Jason".into(),is_child:false,age_band:None}).unwrap();
        let private=store.add_reminder(&json!({"text":"私密事项","due_at_ms":now_ms()+60_000,"profile_id":profile.id})).unwrap();
        let ctx=context(&store,"shared","op-shared");
        assert!(store.assistant_reminder(&ctx,&hash,ReminderAction::Cancel{id:private.id.clone()}).is_err());
        assert_eq!(store.reminders(10).unwrap().iter().find(|r|r.id==private.id).unwrap().state,"pending");
        assert_ne!(private.id,receipt["reminder_id"]);
        drop(store);let _=std::fs::remove_dir_all(root);
    }
}
