//! A bounded, versioned read model for the first typed assistant adapters.
//! Discovery is a cached observation; every mutation rechecks its domain facts.
use std::sync::{atomic::{AtomicU64,Ordering},RwLock};
use serde_json::{json,Value};
use crate::memory::{Store,proactive::Reminder};

pub const CATALOG_VERSION:u32=1;
pub const MAX_SNAPSHOT_AGE_MS:i64=30_000;
#[derive(Clone,Debug)]
pub struct Snapshot {
    pub version:u64,pub fence:u64,pub as_of_ms:i64,pub policy_epoch:u64,
    pub timezone:String,pub timezone_source:String,pub reminders:Vec<Reminder>,
}
pub struct Registry {
    pub service_epoch:String,
    fence:AtomicU64, snapshot:RwLock<Option<Snapshot>>,
}
impl Registry {
    pub fn new()->Self {Self{service_epoch:uuid::Uuid::new_v4().to_string(),fence:AtomicU64::new(1),snapshot:RwLock::new(None)}}
    pub fn invalidate(&self){self.fence.fetch_add(1,Ordering::SeqCst);}
    /// Run only on a bounded storage worker / startup. No caller can mistake
    /// this function for the nonblocking snapshot read below.
    pub fn refresh(&self,memory:&Store)->Result<(),String>{
        let fence=self.fence.load(Ordering::SeqCst);
        let settings=memory.household_settings()?;
        let reminders=memory.reminders(256)?;
        let policy_epoch=settings["policy_revision"].as_u64().ok_or("assistant_policy_missing")?;
        // Re-read the policy after the projection, and reject any intervening
        // source/permission invalidation before publishing it.
        if memory.household_policy_revision()?!=policy_epoch {return Err("assistant_projection_changed".into());}
        let mut current=self.snapshot.write().map_err(|_|"assistant_snapshot_lock")?;
        if self.fence.load(Ordering::SeqCst)!=fence {return Err("assistant_projection_changed".into());}
        let version=current.as_ref().map(|s|s.version+1).unwrap_or(1);
        *current=Some(Snapshot{version,fence,as_of_ms:crate::time::now_ms() as i64,policy_epoch,
            timezone:settings["timezone"].as_str().unwrap_or("UTC").into(),
            timezone_source:settings["timezone_source"].as_str().unwrap_or("fallback").into(),reminders});
        Ok(())
    }
    pub fn snapshot(&self,policy:u64)->Result<Snapshot,String>{
        let snapshot=self.snapshot.read().map_err(|_|"assistant_snapshot_lock")?.clone().ok_or("assistant_snapshot_unavailable")?;
        if snapshot.fence!=self.fence.load(Ordering::SeqCst) || snapshot.policy_epoch!=policy {return Err("assistant_snapshot_revoked".into());}
        let age=(crate::time::now_ms() as i64).saturating_sub(snapshot.as_of_ms);
        if !(0..=MAX_SNAPSHOT_AGE_MS).contains(&age){return Err("assistant_snapshot_expired".into());}
        Ok(snapshot)
    }
    pub fn catalog(&self,policy:u64)->Value {
        let snapshot=self.snapshot(policy).ok();
        let state=if snapshot.is_some(){"ready"}else{"unavailable"};
        json!({"catalog_version":CATALOG_VERSION,"snapshot_version":snapshot.as_ref().map(|s|s.version),
            "checked_at_ms":snapshot.as_ref().map(|s|s.as_of_ms),"valid_for_ms":MAX_SNAPSHOT_AGE_MS,
            "capabilities":[
                {"capability_id":"clock.now","execution_class":"query_local","availability":"ready","receipt":"read_result; timezone falls back explicitly to UTC if configuration is unavailable"},
                {"capability_id":"capability.list","execution_class":"query_snapshot","availability":"ready","receipt":"read_result"},
                {"capability_id":"reminder.list","execution_class":"query_snapshot","availability":state,"receipt":"read_result"},
                {"capability_id":"reminder.create","execution_class":"mutation","availability":state,"receipt":"applied means persisted, not played"},
                {"capability_id":"reminder.cancel","execution_class":"mutation","availability":state,"receipt":"applied means stored cancellation; played speech remains a fact"}
            ],"unsupported":["reminder.update","reminder.repeat"]})
    }
    pub fn prompt(&self,policy:u64,profile:Option<&str>)->String {
        let handles=self.snapshot(policy).map(|s|s.reminders.into_iter().filter(|r|r.profile_id.as_deref()==profile && r.state=="pending")
            .take(12).map(|r|json!({"handle":format!("reminder:{}",r.id),"text":r.text,"due_at_ms":r.due_at_ms})).collect::<Vec<_>>()).unwrap_or_default();
        format!("\n宿主助理能力目录（仅提案，执行和成功由宿主回执决定）：{}\n当前范围可选的提醒：{}\n需要这些功能时只在 actions 提出一个业务动作；不要通过 shell、浏览器或通用 RPC 代替。时区使用家庭设置；相对时间用 after_seconds，明确本地日期和时间用 local_date/local_time。日期、具体时刻或对象有歧义时先问清；提醒修改与循环提醒当前不支持。不要声称保存、取消或播放已经完成。过去叙述、引用、否定不构成创建意图。",self.catalog(policy),json!(handles))
    }
}

pub fn action_schema()->Value {
    let empty=json!({"type":"object","additionalProperties":false,"properties":{}});
    let choice=|id:&str,parameters:Value,target:Value|json!({"type":"object","additionalProperties":false,"properties":{
        "capability_id":{"const":id},"parameters":parameters,"target_handle":target},"required":["capability_id","parameters"]});
    json!({"type":"array","maxItems":1,"items":{"oneOf":[
        choice("clock.now",json!({"type":"object","additionalProperties":false,"properties":{"timezone":{"type":"string","maxLength":80}}}),json!({"type":"null"})),
        choice("capability.list",empty.clone(),json!({"type":"null"})),
        choice("reminder.list",empty.clone(),json!({"type":"null"})),
        choice("reminder.cancel",empty,json!({"type":"string","maxLength":180,"description":"An exact reminder handle in the host snapshot; never invent one."})),
        choice("reminder.create",json!({"type":"object","additionalProperties":false,"properties":{
            "text":{"type":"string","minLength":1,"maxLength":80},
            "after_seconds":{"type":"integer","minimum":1,"maximum":31622400},
            "local_date":{"type":"string","description":"YYYY-MM-DD; only a resolved user date"},
            "local_time":{"type":"string","description":"HH:MM; ask if ambiguous"}},"required":["text"]}),json!({"type":"null"}))
    ]}})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoked_or_expired_projections_never_become_ready_by_reading_them() {
        let registry=Registry::new();let now=crate::time::now_ms() as i64;
        *registry.snapshot.write().unwrap()=Some(Snapshot{version:1,fence:1,as_of_ms:now,policy_epoch:7,timezone:"Australia/Perth".into(),timezone_source:"configured".into(),reminders:vec![]});
        assert!(registry.snapshot(7).is_ok());assert!(registry.snapshot(8).is_err());
        registry.invalidate();assert!(registry.snapshot(7).is_err());
        let mut snapshot=registry.snapshot.write().unwrap();snapshot.as_mut().unwrap().fence=2;snapshot.as_mut().unwrap().as_of_ms=now-31_000;drop(snapshot);
        assert!(registry.snapshot(7).is_err());
        assert_eq!(registry.catalog(7)["capabilities"][3]["availability"],"unavailable");
    }
}
