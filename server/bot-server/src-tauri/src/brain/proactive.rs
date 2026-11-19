//! The only unsolicited speech is an explicitly scheduled reminder.
//! The Mac proposes it and the device decides when speaker playback is safe.

use chrono::{NaiveDateTime, Timelike};
use serde_json::json;
use crate::memory::proactive::{parse_hhmm, JournalLine, ProactiveSettings, Reminder};
use super::Brain;

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub offer_id: u32,
    pub moment: &'static str,
    pub trigger: String,
    pub line: String,
    pub ttl_ms: u32,
    pub show_if_absent: bool,
    pub reminder_id: Option<String>,
    pub local_date: String,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub now_ms: i64,
    pub local: NaiveDateTime,
    pub today: Vec<JournalLine>,
    pub due_reminder: Option<Reminder>,
    pub busy: bool,
}

const REMINDER_TTL_MS: u32 = 10 * 60 * 1000;

pub fn quiet(settings: &ProactiveSettings, local: &NaiveDateTime) -> bool {
    let (Some(start), Some(end)) = (parse_hhmm(&settings.quiet_start), parse_hhmm(&settings.quiet_end)) else { return true };
    let minute = local.hour() * 60 + local.minute();
    if start < end { (start..end).contains(&minute) } else { minute >= start || minute < end }
}

fn blocked(settings: &ProactiveSettings, facts: &Facts) -> Option<&'static str> {
    if !settings.enabled { return Some("disabled"); }
    if !settings.reminders { return Some("moment_off"); }
    if facts.busy { return Some("busy"); }
    if quiet(settings, &facts.local) { return Some("quiet_hours"); }
    if facts.today.iter().filter(|line|matches!(line.status.as_str(),"started"|"spoken")).count() >= settings.daily_cap as usize {
        return Some("daily_cap");
    }
    None
}

/// Only a guardian-created, still-due reminder can become an offer.
pub fn plan(settings: &ProactiveSettings, facts: &Facts, offer_id: u32) -> Option<Offer> {
    if blocked(settings, facts).is_some() { return None; }
    let reminder = facts.due_reminder.as_ref()?;
    Some(Offer {
        offer_id,
        moment: "reminder",
        trigger: format!("reminder:{}", reminder.id),
        line: format!("提醒一下：{}", reminder.text),
        ttl_ms: REMINDER_TTL_MS,
        // The device may hold an offer while the owner cancels it on the Mac.
        // Do not let that stale offer appear on screen after its TTL expires.
        show_if_absent: false,
        reminder_id: Some(reminder.id.clone()),
        local_date: facts.local.format("%Y-%m-%d").to_string(),
    })
}

/// Recheck the actual scheduled reminder before synthesising any audio.
pub fn admit(settings: &ProactiveSettings, facts: &Facts, offer: &Offer) -> Result<(), &'static str> {
    if offer.moment != "reminder" || offer.reminder_id.is_none() { return Err("moment_off"); }
    if let Some(reason) = blocked(settings, facts) { return Err(reason); }
    if facts.today.iter().any(|line| matches!(line.status.as_str(),"started"|"spoken") && line.trigger_source == offer.trigger) {
        return Err("repeated");
    }
    if facts.due_reminder.as_ref().map(|r| &r.id) != offer.reminder_id.as_ref() { return Err("reminder_gone"); }
    Ok(())
}

impl Brain {
    fn proactive_facts(&self) -> Result<(ProactiveSettings, Facts), String> {
        let settings = self.memory.proactive_settings()?;
        let household = self.memory.household_settings()?;
        let zone = household["timezone"].as_str().and_then(|z| z.parse::<chrono_tz::Tz>().ok()).unwrap_or(chrono_tz::Tz::UTC);
        let now_ms = crate::time::now_ms() as i64;
        let local = chrono::DateTime::from_timestamp_millis(now_ms).unwrap_or_default().with_timezone(&zone).naive_local();
        let date = local.format("%Y-%m-%d").to_string();
        let busy = self.foreground_busy()
            || ["queued", "running"].iter().any(|state| self.tasks.list(None, Some(state), 1).map(|t| !t.is_empty()).unwrap_or(true));
        Ok((settings, Facts {
            now_ms, local,
            today: self.memory.proactive_journal(Some(&date), 200)?,
            due_reminder: self.memory.due_reminder(now_ms)?,
            busy,
        }))
    }

    /// Propose the next line for an idle, proactive-capable device, journalled as offered.
    pub(crate) fn proactive_next(&self, device_id: &str) -> Result<Option<Offer>, String> {
        let (settings, facts) = self.proactive_facts()?;
        let random = uuid::Uuid::new_v4().as_u128();
        let offer_id = (random as u32).max(1);
        let Some(offer) = plan(&settings, &facts, offer_id) else { return Ok(None) };
        self.proactive_journal(device_id, &offer, "offered", None);
        Ok(Some(offer))
    }

    pub(crate) fn proactive_admit(&self, offer: &Offer) -> Result<(), String> {
        let (settings, facts) = self.proactive_facts()?;
        admit(&settings, &facts, offer).map_err(str::to_owned)
    }

    /// Journal what became of an offer; spoken/shown/expired reminders advance their retry state.
    pub(crate) fn proactive_journal(&self, device_id: &str, offer: &Offer, status: &str, reason: Option<&str>) {
        let now = crate::time::now_ms() as i64;
        if let Err(error) = self.memory.journal_proactive(offer.offer_id, now, &offer.local_date, offer.moment, &offer.trigger,
                                                          &offer.line, device_id, status, reason) {
            eprintln!("proactive journal failed: {error}");
        }
        if let Some(id) = &offer.reminder_id {
            let outcome = match status { "started" => Some("spoken"), "shown" => Some("shown"), "expired" | "lost" => Some("expired"), _ => None };
            if let Some(outcome) = outcome {
                if let Err(error) = self.memory.reminder_outcome(id, outcome, now) { eprintln!("reminder update failed: {error}"); }
            }
        }
        self.emit("proactive.line", None, json!({"offer_id": offer.offer_id, "moment": offer.moment, "trigger": offer.trigger,
            "line": offer.line, "status": status, "reason": reason}));
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn facts(h: u32, m: u32) -> Facts {
        Facts { now_ms: 1_000_000_000_000,
            local: NaiveDate::from_ymd_opt(2026,11,19).unwrap().and_hms_opt(h,m,0).unwrap(),
            ..Default::default() }
    }
    fn reminder() -> Reminder {
        Reminder { id:"reminder-test".into(), profile_id:None, text:"该喝水啦".into(),
            due_at_ms:1, state:"pending".into(), attempts:0, next_at_ms:1 }
    }

    #[test]
    fn no_unscheduled_greeting_or_event_can_be_offered() {
        let settings=ProactiveSettings::default();
        for (h,m) in [(8,0),(14,0),(18,0)] { assert!(plan(&settings,&facts(h,m),1).is_none()); }
        let offer=Offer { offer_id:1,moment:"morning",trigger:"morning".into(),line:"早上好".into(),
            ttl_ms:1000,show_if_absent:false,reminder_id:None,local_date:"2026-11-19".into() };
        assert_eq!(admit(&settings,&facts(8,0),&offer),Err("moment_off"));
    }

    #[test]
    fn due_reminder_is_rechecked_at_playback() {
        let settings=ProactiveSettings::default();
        let mut f=facts(15,0);
        f.due_reminder=Some(reminder());
        let offer=plan(&settings,&f,2).unwrap();
        assert_eq!((offer.moment,offer.line.as_str(),offer.show_if_absent),("reminder","提醒一下：该喝水啦",false));
        assert_eq!(offer.ttl_ms,REMINDER_TTL_MS);
        assert!(admit(&settings,&f,&offer).is_ok());
        f.due_reminder=None;
        assert_eq!(admit(&settings,&f,&offer),Err("reminder_gone"));
    }

    #[test]
    fn disabled_quiet_busy_and_daily_cap_block_reminders() {
        let mut f=facts(15,0);f.due_reminder=Some(reminder());
        assert!(plan(&ProactiveSettings{enabled:false,..Default::default()},&f,1).is_none());
        assert!(plan(&ProactiveSettings{reminders:false,..Default::default()},&f,1).is_none());
        f.busy=true;assert!(plan(&ProactiveSettings::default(),&f,1).is_none());f.busy=false;
        f.local=facts(23,0).local;assert!(plan(&ProactiveSettings::default(),&f,1).is_none());f.local=facts(15,0).local;
        for id in 0..5 {
            f.today.push(JournalLine {offer_id:id,at_ms:f.now_ms,moment:"reminder".into(),
                trigger_source:format!("old-{id}"),line:"已提醒".into(),status:"started".into(),reason:None});
        }
        assert!(plan(&ProactiveSettings::default(),&f,1).is_none());
    }
}
