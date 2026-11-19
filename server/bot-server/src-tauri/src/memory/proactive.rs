//! Storage for unprompted lines (UX review §12.2): settings, guardian-set
//! reminders and a journal of every offer with its trigger. Additive tables
//! only (see additive_schema.sql); no schema version change.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use super::Store;
use super::store::{clean_text, db_error, now_ms, profile_exists, validate_id};

/// Owner reminder settings. Legacy moment fields remain readable for existing databases;
/// the planner only offers reminders explicitly created by a family member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProactiveSettings {
    pub enabled: bool,
    pub morning_hello: bool,
    pub dated_events: bool,
    pub reminders: bool,
    pub missed_you: bool,
    /// Listen for an answer after an unprompted line without a wake. Separate,
    /// default off and not available yet; always stored false.
    pub answer_directly: bool,
    pub quiet_start: String,
    pub quiet_end: String,
    pub gap_minutes: u32,
    pub daily_cap: u32,
}

impl Default for ProactiveSettings {
    fn default() -> Self {
        Self { enabled: true, morning_hello: false, dated_events: false, reminders: true, missed_you: false,
            answer_directly: false, quiet_start: "21:30".into(), quiet_end: "07:30".into(),
            gap_minutes: 180, daily_cap: 5 }
    }
}

pub fn parse_hhmm(value: &str) -> Option<u32> {
    let (h, m) = value.split_once(':')?;
    if h.len() != 2 || m.len() != 2 { return None; }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reminder {
    pub id: String,
    pub profile_id: Option<String>,
    pub text: String,
    pub due_at_ms: i64,
    pub state: String,
    pub attempts: u32,
    pub next_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JournalLine {
    pub offer_id: u32,
    pub at_ms: i64,
    pub moment: String,
    pub trigger_source: String,
    pub line: String,
    pub status: String,
    pub reason: Option<String>,
}

/// A reminder offered twice without being heard is shown/missed, not retried.
pub const REMINDER_MAX_ATTEMPTS: u32 = 2;
pub const REMINDER_RETRY_MS: i64 = 30 * 60 * 1000;
/// Reminders that are this late are no longer offered at all.
pub const REMINDER_STALE_MS: i64 = 3 * 60 * 60 * 1000;

fn reminder_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Reminder> {
    Ok(Reminder { id: r.get(0)?, profile_id: r.get(1)?, text: r.get(2)?, due_at_ms: r.get(3)?, state: r.get(4)?,
        attempts: r.get(5)?, next_at_ms: r.get(6)? })
}
const REMINDER_COLUMNS: &str = "id,profile_id,text,due_at_ms,state,attempts,next_at_ms";

impl Store {
    pub fn proactive_settings(&self) -> Result<ProactiveSettings, String> {
        let db = self.connection()?;
        let raw: Option<String> = db.query_row("SELECT settings_json FROM proactive_settings WHERE singleton=1", [], |r| r.get(0))
            .optional().map_err(db_error)?;
        // Missing settings use the owner's default reminder policy. A damaged
        // saved row must not silently turn a previously disabled switch on.
        Ok(match raw {
            None => ProactiveSettings::default(),
            Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| ProactiveSettings {
                enabled: false, ..ProactiveSettings::default()
            }),
        })
    }

    /// Merge a partial update over the current settings and validate it.
    pub fn set_proactive_settings(&self, patch: &Value) -> Result<ProactiveSettings, String> {
        let mut merged = serde_json::to_value(self.proactive_settings()?).map_err(|e| e.to_string())?;
        let Some(patch) = patch.as_object() else { return Err("proactive settings must be an object".into()) };
        for (key, value) in patch {
            if merged.get(key).is_none() { return Err(format!("unknown proactive setting {key}")); }
            merged[key] = value.clone();
        }
        let next: ProactiveSettings = serde_json::from_value(merged).map_err(|e| format!("invalid proactive settings: {e}"))?;
        if next.answer_directly { return Err("answer_directly is not available yet".into()); }
        let (Some(start), Some(end)) = (parse_hhmm(&next.quiet_start), parse_hhmm(&next.quiet_end)) else {
            return Err("quiet hours must be HH:MM".into());
        };
        if start == end { return Err("quiet hours must not be empty or all day".into()); }
        if !(30..=1440).contains(&next.gap_minutes) { return Err("gap_minutes must be 30..1440".into()); }
        if !(1..=10).contains(&next.daily_cap) { return Err("daily_cap must be 1..10".into()); }
        let db = self.connection()?;
        db.execute("INSERT INTO proactive_settings(singleton,settings_json,updated_at_ms) VALUES(1,?1,?2)
            ON CONFLICT(singleton) DO UPDATE SET settings_json=excluded.settings_json,updated_at_ms=excluded.updated_at_ms",
            params![serde_json::to_string(&next).map_err(|e| e.to_string())?, now_ms()]).map_err(db_error)?;
        Ok(next)
    }

    pub fn add_reminder(&self, payload: &Value) -> Result<Reminder, String> {
        let text = clean_text(payload["text"].as_str().unwrap_or(""), 80, false, "reminder text")?;
        let due = payload["due_at_ms"].as_i64().ok_or("due_at_ms is required")?;
        let now = now_ms();
        if due < now - 60_000 || due > now + 366 * 86_400_000 { return Err("reminder time must be in the next year".into()); }
        let db = self.connection()?;
        let profile = payload["profile_id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(profile) = &profile { validate_id(profile)?; profile_exists(&db, profile)?; }
        let id = format!("reminder-{}", Uuid::new_v4().simple());
        db.execute("INSERT INTO proactive_reminders(id,profile_id,text,due_at_ms,state,attempts,next_at_ms,created_at_ms)
            VALUES(?1,?2,?3,?4,'pending',0,?4,?5)", params![id, profile, text, due, now]).map_err(db_error)?;
        Ok(Reminder { id, profile_id: profile, text, due_at_ms: due, state: "pending".into(), attempts: 0, next_at_ms: due })
    }

    pub fn cancel_reminder(&self, id: &str) -> Result<bool, String> {
        validate_id(id)?;
        let changed = self.connection()?.execute("UPDATE proactive_reminders SET state='cancelled' WHERE id=?1 AND state='pending'", [id])
            .map_err(db_error)?;
        Ok(changed == 1)
    }

    pub fn reminders(&self, limit: usize) -> Result<Vec<Reminder>, String> {
        let db = self.connection()?;
        let mut q = db.prepare(&format!("SELECT {REMINDER_COLUMNS} FROM proactive_reminders
            ORDER BY (state='pending') DESC, due_at_ms DESC LIMIT ?1")).map_err(db_error)?;
        let rows = q.query_map([limit as i64], reminder_row).map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }

    /// The earliest reminder due now and not yet stale.
    pub fn due_reminder(&self, at_ms: i64) -> Result<Option<Reminder>, String> {
        let db = self.connection()?;
        db.query_row(&format!("SELECT {REMINDER_COLUMNS} FROM proactive_reminders WHERE state='pending'
            AND next_at_ms<=?1 AND due_at_ms>=?2 ORDER BY next_at_ms LIMIT 1"), params![at_ms, at_ms - REMINDER_STALE_MS], reminder_row)
            .optional().map_err(db_error)
    }

    /// spoken: done. shown/expired: one retry 30 min later, then shown/missed.
    pub fn reminder_outcome(&self, id: &str, outcome: &str, at_ms: i64) -> Result<(), String> {
        validate_id(id)?;
        let db = self.connection()?;
        if outcome == "spoken" {
            db.execute("UPDATE proactive_reminders SET state='spoken',attempts=attempts+1 WHERE id=?1 AND state='pending'", [id]).map_err(db_error)?;
            return Ok(());
        }
        let last = if outcome == "shown" { "shown" } else { "missed" };
        db.execute("UPDATE proactive_reminders SET attempts=attempts+1,
            state=CASE WHEN attempts+1>=?2 THEN ?3 ELSE 'pending' END, next_at_ms=?4 WHERE id=?1 AND state='pending'",
            params![id, REMINDER_MAX_ATTEMPTS, last, at_ms + REMINDER_RETRY_MS]).map_err(db_error)?;
        Ok(())
    }

    /// Reserve a due reminder once across concurrent device connections. A stale
    /// reservation expires after three minutes if a previous App process died.
    pub fn claim_reminder_offer(&self, offer_id:u32, device_id:&str, reminder_id:&str, at_ms:i64) -> Result<bool,String> {
        validate_id(reminder_id)?;
        let db=self.connection()?;
        let trigger=format!("reminder:{reminder_id}");
        let changed=db.execute("UPDATE proactive_journal SET status='preparing',at_ms=?1
            WHERE offer_id=?2 AND device_id=?3 AND trigger_source=?4 AND status='offered'
              AND NOT EXISTS(SELECT 1 FROM proactive_journal j WHERE j.trigger_source=?4
                  AND j.status IN ('preparing','playing') AND j.at_ms>?5)
              AND EXISTS(SELECT 1 FROM proactive_reminders r WHERE r.id=?6 AND r.state='pending'
                  AND r.next_at_ms<=?1 AND r.due_at_ms>=?7)",
            params![at_ms,offer_id,device_id,trigger,at_ms-180_000,reminder_id,at_ms-REMINDER_STALE_MS])
            .map_err(db_error)?;
        Ok(changed==1)
    }

    /// Record the hardware outcome and advance the reminder in one transaction.
    pub fn settle_reminder_offer(&self, offer_id:u32, device_id:&str, reminder_id:&str,
                                 spoken:bool, reason:Option<&str>, at_ms:i64) -> Result<(),String> {
        validate_id(reminder_id)?;
        let mut db=self.connection()?;
        let tx=db.transaction().map_err(db_error)?;
        let status=if spoken {"spoken"} else {"lost"};
        let trigger=format!("reminder:{reminder_id}");
        let changed=tx.execute("UPDATE proactive_journal SET status=?1,reason=?2
            WHERE offer_id=?3 AND device_id=?4 AND trigger_source=?5 AND status IN ('preparing','playing')",
            params![status,reason,offer_id,device_id,trigger]).map_err(db_error)?;
        if changed==1 {
            if spoken {
                tx.execute("UPDATE proactive_reminders SET state='spoken',attempts=attempts+1
                    WHERE id=?1 AND state='pending'",[reminder_id]).map_err(db_error)?;
            } else {
                tx.execute("UPDATE proactive_reminders SET attempts=attempts+1,
                    state=CASE WHEN attempts+1>=?2 THEN 'missed' ELSE 'pending' END,next_at_ms=?3
                    WHERE id=?1 AND state='pending'",params![reminder_id,REMINDER_MAX_ATTEMPTS,at_ms+REMINDER_RETRY_MS]).map_err(db_error)?;
            }
        }
        tx.commit().map_err(db_error)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn journal_proactive(&self, offer_id: u32, at_ms: i64, local_date: &str, moment: &str, trigger: &str, line: &str,
                             device_id: &str, status: &str, reason: Option<&str>) -> Result<(), String> {
        let db = self.connection()?;
        let updated = db.execute("UPDATE proactive_journal SET status=?2,reason=?3 WHERE offer_id=?1 AND device_id=?4",
            params![offer_id, status, reason, device_id]).map_err(db_error)?;
        if updated == 0 {
            db.execute("INSERT INTO proactive_journal(offer_id,at_ms,local_date,moment,trigger_source,line,person_id,device_id,status,reason)
                VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,?8,?9)", params![offer_id, at_ms, local_date, moment, trigger, line, device_id, status, reason])
                .map_err(db_error)?;
        }
        Ok(())
    }

    pub fn proactive_journal(&self, local_date: Option<&str>, limit: usize) -> Result<Vec<JournalLine>, String> {
        let db = self.connection()?;
        let mut q = db.prepare("SELECT offer_id,at_ms,moment,trigger_source,line,status,reason FROM proactive_journal
            WHERE (?1 IS NULL OR local_date=?1) ORDER BY at_ms DESC, id DESC LIMIT ?2").map_err(db_error)?;
        let rows = q.query_map(params![local_date, limit as i64], |r| Ok(JournalLine { offer_id: r.get(0)?, at_ms: r.get(1)?,
            moment: r.get(2)?, trigger_source: r.get(3)?, line: r.get(4)?, status: r.get(5)?, reason: r.get(6)? })).map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }

    /// When anyone last talked with 小呆 (for "missed you"), across all days.
    pub fn last_household_turn_ms(&self) -> Result<Option<i64>, String> {
        self.connection()?.query_row("SELECT MAX(occurred_at_ms) FROM household_turns", [], |r| r.get(0)).map_err(db_error)
    }

    pub fn proactive_rpc(&self, method: &str, payload: &Value) -> Result<Option<Value>, String> {
        let result = match method {
            "proactive.settings.get" => json!(self.proactive_settings()?),
            "proactive.settings.set" => json!(self.set_proactive_settings(payload)?),
            "proactive.reminders" => json!({"reminders": self.reminders(payload["limit"].as_u64().unwrap_or(50).min(200) as usize)?}),
            "proactive.reminder.add" => json!(self.add_reminder(payload)?),
            "proactive.reminder.cancel" => json!({"cancelled": self.cancel_reminder(payload["id"].as_str().ok_or("id is required")?)?}),
            "proactive.journal" => json!({"lines": self.proactive_journal(payload["local_date"].as_str(), payload["limit"].as_u64().unwrap_or(50).min(200) as usize)?}),
            _ => return Ok(None),
        };
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryConfig;

    fn store() -> (std::sync::Arc<Store>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("xiaodai-proactive-{}", Uuid::new_v4()));
        (Store::open(dir.clone(), MemoryConfig::default()).unwrap(), dir)
    }

    #[test]
    fn concurrent_reminder_claims_and_real_receipts_are_idempotent() {
        let (store,dir)=store();
        let now=now_ms();
        let reminder=store.add_reminder(&json!({"text":"测试提醒","due_at_ms":now})).unwrap();
        let trigger=format!("reminder:{}",reminder.id);
        for (offer,device) in [(1,"bot1"),(2,"bot2")] {
            store.journal_proactive(offer,now,"2026-11-19","reminder",&trigger,"提醒一下：测试提醒",device,"offered",None).unwrap();
        }
        assert!(store.claim_reminder_offer(1,"bot1",&reminder.id,now).unwrap());
        assert!(!store.claim_reminder_offer(2,"bot2",&reminder.id,now).unwrap());
        assert_eq!(store.reminders(10).unwrap()[0].state,"pending");
        store.settle_reminder_offer(1,"bot1",&reminder.id,true,None,now+100).unwrap();
        store.settle_reminder_offer(1,"bot1",&reminder.id,true,None,now+101).unwrap();
        let saved=store.reminders(10).unwrap().remove(0);
        assert_eq!((saved.state.as_str(),saved.attempts),("spoken",1));
        assert!(!store.claim_reminder_offer(2,"bot2",&reminder.id,now+200).unwrap());
        drop(store);let _=std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interrupted_reminder_remains_pending_until_its_retry() {
        let (store,dir)=store();
        let now=now_ms();
        let reminder=store.add_reminder(&json!({"text":"测试重试","due_at_ms":now})).unwrap();
        let trigger=format!("reminder:{}",reminder.id);
        store.journal_proactive(3,now,"2026-11-19","reminder",&trigger,"提醒一下：测试重试","bot1","offered",None).unwrap();
        assert!(store.claim_reminder_offer(3,"bot1",&reminder.id,now).unwrap());
        store.settle_reminder_offer(3,"bot1",&reminder.id,false,Some("interrupted"),now+100).unwrap();
        store.settle_reminder_offer(3,"bot1",&reminder.id,false,Some("interrupted"),now+101).unwrap();
        let saved=store.reminders(10).unwrap().remove(0);
        assert_eq!((saved.state.as_str(),saved.attempts,saved.next_at_ms),
                   ("pending",1,now+100+REMINDER_RETRY_MS));
        assert!(store.due_reminder(now+REMINDER_RETRY_MS).unwrap().is_none());
        assert_eq!(store.due_reminder(now+100+REMINDER_RETRY_MS).unwrap().unwrap().id,reminder.id);
        drop(store);let _=std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reminder_settings_merge_partially_and_reject_bad_values() {
        let (store, dir) = store();
        let defaults = store.proactive_settings().unwrap();
        assert!(defaults.enabled && !defaults.morning_hello && defaults.reminders && !defaults.answer_directly);
        assert_eq!((defaults.quiet_start.as_str(), defaults.quiet_end.as_str(), defaults.gap_minutes, defaults.daily_cap), ("21:30", "07:30", 180, 5));
        let on = store.set_proactive_settings(&json!({"enabled": true, "missed_you": false})).unwrap();
        assert!(on.enabled && !on.missed_you && !on.morning_hello);
        assert_eq!(store.proactive_settings().unwrap(), on);
        for bad in [json!({"answer_directly": true}), json!({"quiet_start": "25:00"}), json!({"gap_minutes": 5}),
                    json!({"daily_cap": 0}), json!({"surprise": 1}), json!({"quiet_start": "07:30"})] {
            assert!(store.set_proactive_settings(&bad).is_err(), "{bad}");
        }
        assert_eq!(store.proactive_settings().unwrap(), on, "a rejected update changes nothing");
        // A newer App's unknown field in the stored row does not break this one.
        store.connection().unwrap().execute("UPDATE proactive_settings SET settings_json=?1",
            [r#"{"enabled":true,"future_field":3}"#]).unwrap();
        assert!(store.proactive_settings().unwrap().enabled);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reminders_are_retried_once_then_marked_and_journal_updates_in_place() {
        let (store, dir) = store();
        let now = now_ms();
        let r = store.add_reminder(&json!({"text": "该喝水啦", "due_at_ms": now})).unwrap();
        assert!(store.add_reminder(&json!({"text": "", "due_at_ms": now})).is_err());
        assert!(store.add_reminder(&json!({"text": "太早", "due_at_ms": now - 3_600_000})).is_err());
        assert_eq!(store.due_reminder(now).unwrap().unwrap().id, r.id);
        store.reminder_outcome(&r.id, "expired", now).unwrap();
        assert!(store.due_reminder(now).unwrap().is_none(), "retry waits 30 min");
        let again = store.due_reminder(now + REMINDER_RETRY_MS).unwrap().unwrap();
        assert_eq!(again.attempts, 1);
        store.reminder_outcome(&r.id, "shown", now + REMINDER_RETRY_MS).unwrap();
        assert_eq!(store.reminders(10).unwrap()[0].state, "shown");
        assert!(store.due_reminder(now + 3 * REMINDER_RETRY_MS).unwrap().is_none());
        let spoken = store.add_reminder(&json!({"text": "穿外套", "due_at_ms": now})).unwrap();
        store.reminder_outcome(&spoken.id, "spoken", now).unwrap();
        let cancelled = store.add_reminder(&json!({"text": "取快递", "due_at_ms": now})).unwrap();
        assert!(store.cancel_reminder(&cancelled.id).unwrap());
        assert!(store.due_reminder(now).unwrap().is_none());
        store.journal_proactive(7, now, "2026-11-18", "reminder", "reminder:x", "提醒一下：该喝水啦", "dev", "offered", None).unwrap();
        store.journal_proactive(7, now, "2026-11-18", "reminder", "reminder:x", "提醒一下：该喝水啦", "dev", "declined", Some("quiet_hours")).unwrap();
        let lines = store.proactive_journal(Some("2026-11-18"), 10).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!((lines[0].status.as_str(), lines[0].reason.as_deref()), ("declined", Some("quiet_hours")));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
