//! Unprompted lines (UX review §12.2): morning hello, dated events, guardian
//! reminders and "missed you". The Mac only *proposes* a line; the device's
//! bot_runtime decides the moment (presence, quiet room, no conversation,
//! daytime) and asks for it to be spoken. The Mac re-checks its own guardrails
//! (switches, quiet hours, household daily cap, gap, no repeat, busy, tasks)
//! when the device asks. Speaking out only: nothing here ever listens, and
//! pre-wake audio or images are never stored or uploaded. Without pre-wake
//! identity every line is nameless; the per-person bucket is "anyone".

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
    /// Today's journal (household local date), any status.
    pub today: Vec<JournalLine>,
    pub events_today: Vec<String>,
    pub due_reminder: Option<Reminder>,
    pub last_turn_ms: Option<i64>,
    pub last_missed_you_ms: Option<i64>,
    pub history_enabled: bool,
    /// A foreground turn or a queued/running task.
    pub busy: bool,
}

const MORNING: [&str; 4] = ["早上好呀！", "早呀，今天也一起加油吧！", "早上好！昨晚睡得好吗？", "早安！今天想做点什么好玩的？"];
const MISSED: [&str; 3] = ["好久不见呀，我好想你！", "你回来啦！我一直在等你呢。", "好几天没见啦，最近还好吗？"];
const MORNING_END_MIN: u32 = 11 * 60;
const EVENTS_END_MIN: u32 = 21 * 60;
const MISSED_AFTER_MS: i64 = 2 * 86_400_000;
const OFFER_TTL_MS: u32 = 30 * 60 * 1000;
const REMINDER_TTL_MS: u32 = 10 * 60 * 1000;

fn spoken(line: &JournalLine) -> bool { line.status == "started" }

pub fn quiet(settings: &ProactiveSettings, local: &NaiveDateTime) -> bool {
    let (Some(start), Some(end)) = (parse_hhmm(&settings.quiet_start), parse_hhmm(&settings.quiet_end)) else { return true };
    let minute = local.hour() * 60 + local.minute();
    if start < end { (start..end).contains(&minute) } else { minute >= start || minute < end }
}

fn short(text: &str) -> String { text.chars().take(40).collect() }

/// Guardrails shared by planning and the final check when the device asks.
fn blocked(settings: &ProactiveSettings, facts: &Facts, reminder: bool) -> Option<&'static str> {
    if !settings.enabled { return Some("disabled"); }
    if facts.busy { return Some("busy"); }
    if quiet(settings, &facts.local) { return Some("quiet_hours"); }
    if facts.today.iter().filter(|l| spoken(l)).count() >= settings.daily_cap as usize { return Some("daily_cap"); }
    // Reminders are exempt from the gap (a guardian set the time) but not the cap.
    let last = facts.today.iter().filter(|l| spoken(l)).map(|l| l.at_ms).max();
    if !reminder && last.is_some_and(|at| facts.now_ms - at < settings.gap_minutes as i64 * 60_000) { return Some("gap"); }
    None
}

fn moment_enabled(settings: &ProactiveSettings, moment: &str) -> bool {
    match moment {
        "morning" => settings.morning_hello,
        "event" => settings.dated_events,
        "reminder" => settings.reminders,
        "missed_you" => settings.missed_you,
        _ => false,
    }
}

fn pick(pool: &[&str], facts: &Facts, random: u32) -> Option<String> {
    let fresh: Vec<&&str> = pool.iter().filter(|line| !facts.today.iter().any(|l| spoken(l) && l.line.starts_with(**line))).collect();
    (!fresh.is_empty()).then(|| fresh[random as usize % fresh.len()].to_string())
}

fn mentioned_today(facts: &Facts, event: &str) -> bool {
    let event = short(event);
    facts.today.iter().any(|l| spoken(l) && l.line.contains(&event))
}

/// The next line worth proposing, if any. `random` picks among fresh lines.
pub fn plan(settings: &ProactiveSettings, facts: &Facts, offer_id: u32, random: u32) -> Option<Offer> {
    let local_date = facts.local.format("%Y-%m-%d").to_string();
    let offer = |moment, trigger: String, line: String, ttl_ms, show_if_absent, reminder_id| Offer {
        offer_id, moment, trigger, line, ttl_ms, show_if_absent, reminder_id, local_date: local_date.clone() };
    if let Some(reminder) = facts.due_reminder.as_ref().filter(|_| settings.reminders) {
        if blocked(settings, facts, true).is_none() {
            return Some(offer("reminder", format!("reminder:{}", reminder.id), format!("提醒一下：{}", reminder.text),
                REMINDER_TTL_MS, true, Some(reminder.id.clone())));
        }
    }
    if blocked(settings, facts, false).is_some() { return None; }
    let minute = facts.local.hour() * 60 + facts.local.minute();
    let away = facts.last_turn_ms.filter(|at| facts.now_ms - at >= MISSED_AFTER_MS);
    if settings.missed_you && facts.history_enabled {
        if let Some(last) = away {
            if facts.last_missed_you_ms.is_none_or(|missed| missed < last) {
                if let Some(line) = pick(&MISSED, facts, random) {
                    return Some(offer("missed_you", format!("away_since:{last}"), line, OFFER_TTL_MS, false, None));
                }
            }
        }
    }
    let quiet_end = parse_hhmm(&settings.quiet_end).unwrap_or(0);
    let morning_done = facts.today.iter().any(|l| spoken(l) && l.moment == "morning");
    if settings.morning_hello && !morning_done && minute >= quiet_end && minute < MORNING_END_MIN {
        if let Some(mut line) = pick(&MORNING, facts, random) {
            let mut trigger = "morning".to_string();
            if settings.dated_events {
                if let Some(event) = facts.events_today.iter().find(|e| !mentioned_today(facts, e)) {
                    line.push_str(&format!("今天有件事：{}。", short(event)));
                    trigger = format!("morning+event:{}", short(event));
                }
            }
            let ttl = ((MORNING_END_MIN - minute) * 60_000).min(OFFER_TTL_MS);
            return Some(offer("morning", trigger, line, ttl, false, None));
        }
    }
    if settings.dated_events && minute < EVENTS_END_MIN {
        if let Some(event) = facts.events_today.iter().find(|e| !mentioned_today(facts, e)) {
            let ttl = ((EVENTS_END_MIN - minute) * 60_000).min(OFFER_TTL_MS);
            return Some(offer("event", format!("event:{}", short(event)), format!("记得哦，今天有件事：{}。", short(event)), ttl, false, None));
        }
    }
    None
}

/// The final check when the device has found a moment and asks to speak.
pub fn admit(settings: &ProactiveSettings, facts: &Facts, offer: &Offer) -> Result<(), &'static str> {
    if !moment_enabled(settings, offer.moment) { return Err("moment_off"); }
    if let Some(reason) = blocked(settings, facts, offer.moment == "reminder") { return Err(reason); }
    if facts.today.iter().any(|l| spoken(l) && l.line == offer.line) { return Err("repeated"); }
    if offer.reminder_id.is_some() && facts.due_reminder.as_ref().map(|r| &r.id) != offer.reminder_id.as_ref() {
        return Err("reminder_gone");
    }
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
        let history_enabled = household["history_enabled"] == true;
        // Only memories that may leave the Mac: the line is synthesised by the TTS service.
        let events_today = if history_enabled {
            self.memory.household_dated_items_on(local.date(), 0, true)?.into_iter().map(|(_, content)| content).collect()
        } else { Vec::new() };
        let all = self.memory.proactive_journal(None, 200)?;
        let last_missed_you_ms = all.iter().filter(|l| l.moment == "missed_you" && l.status == "started").map(|l| l.at_ms).max();
        let busy = self.foreground_busy()
            || ["queued", "running"].iter().any(|state| self.tasks.list(None, Some(state), 1).map(|t| !t.is_empty()).unwrap_or(true));
        Ok((settings, Facts {
            now_ms, local,
            today: self.memory.proactive_journal(Some(&date), 200)?,
            events_today,
            due_reminder: self.memory.due_reminder(now_ms)?,
            last_turn_ms: self.memory.last_household_turn_ms()?,
            last_missed_you_ms, history_enabled, busy,
        }))
    }

    /// Propose the next line for an idle, proactive-capable device, journalled as offered.
    pub(crate) fn proactive_next(&self, device_id: &str) -> Result<Option<Offer>, String> {
        let (settings, facts) = self.proactive_facts()?;
        let random = uuid::Uuid::new_v4().as_u128();
        let offer_id = (random as u32).max(1);
        let Some(offer) = plan(&settings, &facts, offer_id, (random >> 64) as u32) else { return Ok(None) };
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

    fn at(h: u32, m: u32) -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, 18).unwrap().and_hms_opt(h, m, 0).unwrap() }
    fn on() -> ProactiveSettings { ProactiveSettings { enabled: true, ..Default::default() } }
    fn facts(h: u32, m: u32) -> Facts {
        Facts { now_ms: 1_000_000_000_000, local: at(h, m), last_turn_ms: Some(1_000_000_000_000 - 3_600_000), history_enabled: true, ..Default::default() }
    }
    fn said(facts: &mut Facts, offer: &Offer, minutes_ago: i64) {
        facts.today.push(JournalLine { offer_id: offer.offer_id, at_ms: facts.now_ms - minutes_ago * 60_000, moment: offer.moment.into(),
            trigger_source: offer.trigger.clone(), line: offer.line.clone(), status: "started".into(), reason: None });
    }
    fn reminder() -> Reminder {
        Reminder { id: "reminder-1".into(), profile_id: None, text: "该喝水啦".into(), due_at_ms: 0, state: "pending".into(), attempts: 0, next_at_ms: 0 }
    }

    #[test]
    fn defaults_are_off_and_nothing_is_ever_offered_until_enabled() {
        let mut f = facts(8, 0);
        f.due_reminder = Some(reminder());
        f.events_today = vec!["小明生日".into()];
        assert!(plan(&ProactiveSettings::default(), &f, 1, 0).is_none());
        assert!(plan(&on(), &f, 1, 0).is_some());
    }

    #[test]
    fn morning_hello_is_nameless_once_a_day_and_carries_todays_event() {
        let mut f = facts(8, 0);
        f.events_today = vec!["小明生日".into()];
        let offer = plan(&on(), &f, 5, 1).unwrap();
        assert_eq!(offer.moment, "morning");
        assert!(MORNING.iter().any(|l| offer.line.starts_with(l)) && offer.line.ends_with("今天有件事：小明生日。"), "{}", offer.line);
        assert!(offer.ttl_ms <= 30 * 60_000 && !offer.show_if_absent);
        assert!(admit(&on(), &f, &offer).is_ok());
        said(&mut f, &offer, 0);
        assert_eq!(admit(&on(), &f, &offer), Err("gap"));
        f.now_ms += 4 * 3_600_000;
        f.local = at(10, 30);
        // No second morning hello and the event already mentioned: nothing to say.
        assert!(plan(&on(), &f, 6, 1).is_none());
        assert_eq!(admit(&on(), &f, &offer), Err("repeated"));
        // Outside the morning window there is no hello at all.
        let late = facts(11, 0);
        assert!(plan(&on(), &late, 7, 0).is_none());
    }

    #[test]
    fn quiet_hours_cap_gap_busy_and_toggles_all_block() {
        let s = on();
        for (h, m) in [(21, 30), (23, 0), (3, 0), (7, 29)] { assert!(plan(&s, &facts(h, m), 1, 0).is_none(), "{h}:{m}"); }
        assert!(quiet(&ProactiveSettings { quiet_start: "13:00".into(), quiet_end: "14:00".into(), ..on() }, &at(13, 30)));
        let mut busy = facts(8, 0);
        busy.busy = true;
        assert!(plan(&s, &busy, 1, 0).is_none());
        let morning = plan(&s, &facts(8, 0), 1, 0).unwrap();
        assert_eq!(admit(&s, &busy, &morning), Err("busy"));
        assert_eq!(admit(&ProactiveSettings { morning_hello: false, ..on() }, &facts(8, 0), &morning), Err("moment_off"));
        assert!(plan(&ProactiveSettings { morning_hello: false, dated_events: false, ..on() }, &facts(8, 0), 1, 0).is_none());
        let mut capped = facts(12, 0);
        capped.events_today = vec!["去公园".into()];
        for i in 0..5 {
            let line = JournalLine { offer_id: i, at_ms: capped.now_ms - (600 - i as i64) * 60_000, moment: "event".into(),
                trigger_source: "x".into(), line: format!("线{i}"), status: "started".into(), reason: None };
            capped.today.push(line);
        }
        assert!(plan(&s, &capped, 9, 0).is_none(), "household daily cap");
        capped.due_reminder = Some(reminder());
        assert!(plan(&s, &capped, 9, 0).is_none(), "the cap binds reminders too");
        capped.today.truncate(4);
        assert_eq!(plan(&s, &capped, 9, 0).unwrap().moment, "reminder");
    }

    #[test]
    fn reminders_skip_the_gap_are_shown_if_absent_and_must_still_be_due() {
        let s = on();
        let mut f = facts(15, 0);
        let event = Offer { offer_id: 1, moment: "event", trigger: "x".into(), line: "早上好呀！".into(), ttl_ms: 1, show_if_absent: false, reminder_id: None, local_date: String::new() };
        said(&mut f, &event, 5);
        f.due_reminder = Some(reminder());
        let offer = plan(&s, &f, 2, 0).unwrap();
        assert_eq!((offer.moment, offer.line.as_str(), offer.show_if_absent), ("reminder", "提醒一下：该喝水啦", true));
        assert_eq!(offer.ttl_ms, 10 * 60_000);
        assert!(admit(&s, &f, &offer).is_ok());
        f.due_reminder = None;
        assert_eq!(admit(&s, &f, &offer), Err("reminder_gone"));
        assert!(plan(&ProactiveSettings { reminders: false, ..on() }, &Facts { due_reminder: Some(reminder()), ..facts(15, 0) }, 3, 0)
            .is_none_or(|o| o.moment != "reminder"));
    }

    #[test]
    fn dated_events_once_per_item_and_not_in_the_evening() {
        let s = on();
        let mut f = facts(14, 0);
        f.events_today = vec!["小明生日".into(), "交作业".into()];
        let first = plan(&s, &f, 1, 0).unwrap();
        assert_eq!((first.moment, first.line.as_str()), ("event", "记得哦，今天有件事：小明生日。"));
        said(&mut f, &first, 200);
        let second = plan(&s, &f, 2, 0).unwrap();
        assert_eq!(second.line, "记得哦，今天有件事：交作业。");
        said(&mut f, &second, 190);
        assert!(plan(&s, &f, 3, 0).is_none());
        let mut evening = facts(21, 0);
        evening.events_today = vec!["小明生日".into()];
        assert!(plan(&ProactiveSettings { quiet_start: "22:00".into(), ..on() }, &evening, 4, 0).is_none());
    }

    #[test]
    fn missed_you_after_two_days_once_per_absence_and_only_with_history() {
        let s = on();
        let mut f = facts(16, 0);
        f.last_turn_ms = Some(f.now_ms - 2 * 86_400_000 - 1);
        let offer = plan(&s, &f, 1, 2).unwrap();
        assert_eq!(offer.moment, "missed_you");
        assert!(MISSED.contains(&offer.line.as_str()));
        f.last_missed_you_ms = Some(f.now_ms - 60_000);
        assert!(plan(&s, &f, 2, 0).is_none(), "once per absence");
        f.last_missed_you_ms = None;
        f.history_enabled = false;
        assert!(plan(&s, &f, 3, 0).is_none());
        let mut recent = facts(16, 0);
        recent.last_turn_ms = Some(recent.now_ms - 86_400_000);
        assert!(plan(&s, &recent, 4, 0).is_none());
    }
}
