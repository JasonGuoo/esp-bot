//! A varied acknowledgement pool, pre-synthesised in the reply voice and chosen
//! by context, never repeating within the last few turns (review §4.2).

use std::{collections::{HashMap, VecDeque}, sync::{Arc, Mutex, Weak, atomic::Ordering}, time::Duration};
use serde_json::Value;
use tokio::sync::watch;
use super::Brain;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind { Thinking, Question, Story, Evening }

pub const POOL: &[(&str, Kind)] = &[
    ("嗯……让我想想。", Kind::Thinking), ("我想想哦。", Kind::Thinking), ("嗯，等我一下下。", Kind::Thinking),
    ("好问题！", Kind::Question), ("这个问题有意思。", Kind::Question), ("嗯，这个嘛……", Kind::Question),
    ("好呀，听我讲。", Kind::Story), ("好嘞，故事来啦。", Kind::Story),
    ("嗯，我想想。", Kind::Evening), ("好，我想一想。", Kind::Evening),
];
/// Not repeated within this many acknowledgements.
const NO_REPEAT: usize = 3;

pub fn kind(transcript: &str, evening: bool) -> Kind {
    let text = transcript.trim();
    if ["故事", "讲一个", "讲个"].iter().any(|w| text.contains(w)) { return Kind::Story; }
    if evening { return Kind::Evening; }
    let question = text.ends_with(['？', '?', '吗', '呢']) || ["为什么", "怎么", "什么", "是不是", "多少", "哪"].iter().any(|w| text.contains(w));
    if question { Kind::Question } else { Kind::Thinking }
}

#[derive(Default)]
pub struct Acks { audio: Mutex<HashMap<&'static str, Value>>, recent: Mutex<VecDeque<&'static str>> }

impl Acks {
    pub fn missing(&self) -> Vec<&'static str> {
        let audio = self.audio.lock().map(|a| a.keys().copied().collect::<Vec<_>>()).unwrap_or_default();
        POOL.iter().map(|p| p.0).filter(|t| !audio.contains(t)).collect()
    }

    pub fn insert(&self, text: &'static str, audio: Value) {
        if let Ok(mut cache) = self.audio.lock() { cache.insert(text, audio); }
    }

    /// The line for this turn: its own kind first, then a neutral thinking line,
    /// then anything cached, skipping the last `NO_REPEAT` choices.
    pub fn pick(&self, transcript: &str, evening: bool) -> Option<(&'static str, Value)> {
        let wanted = kind(transcript, evening);
        let audio = self.audio.lock().ok()?;
        let mut recent = self.recent.lock().ok()?;
        let fresh = |k: Option<Kind>| POOL.iter().filter(|p| k.is_none_or(|k| p.1 == k))
            .find(|p| audio.contains_key(p.0) && !recent.contains(&p.0)).map(|p| p.0);
        let text = fresh(Some(wanted)).or_else(|| fresh(Some(Kind::Thinking))).or_else(|| fresh(None))?;
        if recent.len() == NO_REPEAT { recent.pop_front(); }
        recent.push_back(text);
        Some((text, audio[text].clone()))
    }
}

/// An open activation also covers capture, playback and the follow-up window.
/// `foreground_busy` alone only sees admitted ASR/inference turns.
pub(super) fn device_session_active(state:&Value)->bool {
    state.get("conversation_id").and_then(Value::as_str).is_some()
}

impl Brain {
    /// One cache job per Brain, owned by service.stop and never by a peer.
    pub(super) async fn warm_acknowledgements(self:&Arc<Self>) {
        let mut job=self.ack_warmup.lock().await;
        if job.as_ref().is_some_and(|task|!task.is_finished()) || self.acks.missing().is_empty() {return;}
        let foreground=self.foreground_generation.subscribe();
        let generation=self.models.generation();
        if !self.accepting.load(Ordering::Acquire) {return;}
        *job=Some(tokio::spawn(warm_pool(Arc::downgrade(self),foreground,generation)));
    }

    fn ack_warmup_busy(&self)->bool {
        self.foreground_busy() || self.device_states.lock()
            .map(|states|states.values().any(device_session_active)).unwrap_or(true)
    }
}

async fn warm_pool(brain:Weak<Brain>,mut foreground:watch::Receiver<u64>,worker_generation:u64) {
    loop {
        // Waiting has no deadline that could force cache work into a live turn.
        // Keep no strong Brain reference while idle, and resume after it clears.
        tokio::select! {
            biased;
            changed=foreground.changed()=>if changed.is_err(){return;},
            _=tokio::time::sleep(Duration::from_millis(500))=>{},
        }
        let Some(brain)=brain.upgrade() else{return;};
        if !brain.accepting.load(Ordering::Acquire) || brain.models.generation()!=worker_generation {return;}
        let generation=*foreground.borrow_and_update();
        if brain.ack_warmup_busy() {continue;}
        let Some(text)=brain.acks.missing().into_iter().next() else{return;};
        let result=tokio::select! {
            biased;
            changed=foreground.changed()=>{
                if changed.is_err(){return;}
                // Dropping this exchange stops only its own child process.
                None
            },
            result=brain.models.tts_if_idle(text,worker_generation)=>Some(result),
        };
        match result {
            None=>continue,
            Some(Ok(audio))=>{
                if !brain.accepting.load(Ordering::Acquire) || brain.models.generation()!=worker_generation {return;}
                if *foreground.borrow()!=generation || brain.ack_warmup_busy() {continue;}
                brain.acks.insert(text,audio);
            },
            Some(Err(code)) if code=="local_worker_lane_busy"=>continue,
            Some(Err(_))=>return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_device_activation_is_busy_even_between_turns() {
        assert!(device_session_active(&serde_json::json!({"phase":"idle","conversation_id":"device-a"})));
        assert!(device_session_active(&serde_json::json!({"phase":"listening","conversation_id":"device-a"})));
        assert!(device_session_active(&serde_json::json!({"phase":"speaking","conversation_id":"device-a"})));
        assert!(!device_session_active(&serde_json::json!({"phase":"idle","conversation_id":null})));
        assert!(!device_session_active(&serde_json::json!({"phase":"disconnected"})));
    }

    #[test]
    fn acks_vary_by_context_and_never_repeat_back_to_back() {
        assert_eq!(kind("给我讲个恐龙故事", false), Kind::Story);
        assert_eq!(kind("天为什么是蓝的", false), Kind::Question);
        assert_eq!(kind("今天好累", false), Kind::Thinking);
        assert_eq!(kind("今天好累", true), Kind::Evening);
        let acks = Acks::default();
        assert_eq!(acks.pick("你好", false), None, "nothing cached yet: no ack");
        for (text, _) in POOL { acks.insert(text, serde_json::json!({"wav_base64":text})); }
        assert!(acks.missing().is_empty());
        let picks: Vec<_> = (0..4).map(|_| acks.pick("天为什么是蓝的", false).unwrap().0).collect();
        assert!(POOL.iter().any(|p| p.0 == picks[0] && p.1 == Kind::Question));
        assert!(picks.windows(2).all(|w| w[0] != w[1]));
        assert_eq!(picks[..3].iter().collect::<std::collections::HashSet<_>>().len(), 3, "{picks:?}");
        assert!(POOL.iter().any(|p| p.0 == picks[3] && p.1 == Kind::Thinking), "question lines used up: neutral");
    }

    #[test]
    fn evening_acknowledgements_do_not_describe_their_own_volume() {
        let acks = Acks::default();
        for (text, _) in POOL { acks.insert(text, serde_json::json!({"wav_base64":text})); }
        for _ in 0..8 {
            let (text, _) = acks.pick("你好", true).unwrap();
            assert!(!["轻轻", "小声", "轻声"].iter().any(|word| text.contains(word)), "{text}");
        }
    }
}
