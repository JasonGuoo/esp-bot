//! Native V2 compaction. An accepted inbox item is not a completed summary.
//! Source: anomalyco/opencode v2.2.5 protocol session.ts, schema/session-inbox.ts
//! and schema/session-message.ts. Never hold the conversation gate while waiting
//! for the native execution loop to become idle.
use super::*;
use sha2::{Digest, Sha256};

impl Harness {
    pub async fn compact(&self, conversation_id: &str, force: bool) -> Result<Value, String> {
        if !self.ready.load(Ordering::Acquire) { return Err("opencode_not_started".into()); }
        let _lifecycle = self.turn_gate.read().await;
        let gate = self.conversation_gate(conversation_id)?;
        let conversation_guard = gate.try_lock().map_err(|_| "opencode_conversation_busy")?;
        let api = self.api()?;
        let (session, message, newly_admitted, target) = {
            let mut ledger = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
            let record = ledger.conversations.get_mut(conversation_id).ok_or("opencode_conversation_missing")?;
            if record.abandoned || record.delete_pending { return Err("opencode_context_unavailable".into()); }
            checked_id(&record.current_session, "ses_")?;
            if !force && !record.compact_pending && record.completed_turns.saturating_sub(record.compacted_through) < 8 {
                return Ok(json!({"state":"not_due","completed_turns":record.completed_turns}));
            }
            let new = !record.compact_pending;
            let before = record.clone();
            if new {
                record.compact_pending = true;
                record.compact_target = record.completed_turns;
                record.compact_message_id = Some(format!("msg_{}", Uuid::new_v4().simple()));
            }
            let result = (record.current_session.clone(), record.compact_message_id.clone().ok_or("compact_ledger_missing_id")?, new, record.compact_target);
            // Record intent before HTTP. Recovery reads this exact message; it never resubmits a prompt.
            if new {
                if let Err(error) = write_state(&self.data_dir.join("sessions.json"), &*ledger) {
                    ledger.conversations.insert(conversation_id.to_owned(), before);
                    return Err(error);
                }
            }
            result
        };
        checked_id(&session, "ses_")?;
        checked_id(&message, "msg_")?;
        if newly_admitted {
            // This is only inbox admission. A timeout must leave the persisted
            // ID pending because dropping HTTP cannot revoke a durable input.
            let admission = tokio::time::timeout(Duration::from_millis(1500), api.json(
                Method::POST, &format!("/api/session/{session}/compact"), Some(&json!({"id":message})),
            )).await;
            match admission {
                Ok(Ok(value)) if value.pointer("/data/id").and_then(Value::as_str) == Some(message.as_str())
                    && value.pointer("/data/sessionID").and_then(Value::as_str) == Some(session.as_str())
                    && value.pointer("/data/type").and_then(Value::as_str) == Some("compaction") => {},
                other => {
                    let code = match other { Ok(Err(error)) => error.public_message(),
                        Err(_) => "compact_admission_timeout".into(), _ => "compact_admission_mismatch".into() };
                    self.emit(json!({"kind":"harness_compact_pending","conversation_id":conversation_id,"message_id":message,"code":code}));
                    return Ok(json!({"state":"pending","message_id":message,"admission_unknown":true,"reconcile_required":true}));
                }
            }
        }
        // Foreground input can now acquire its gate while this bounded read
        // checks the original projection. No native /wait or repeated polling.
        drop(conversation_guard);
        let projected = match tokio::time::timeout(Duration::from_millis(1000), api.json(
            Method::GET, &format!("/api/session/{session}/message/{message}"), None,
        )).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) if error.status != Some(404) => return Err(error.public_message()),
            _ => return Ok(json!({"state":"pending","message_id":message,"reconcile_required":true})),
        };
        let item = &projected["data"];
        if item["id"].as_str() != Some(message.as_str()) || item["type"] != "compaction" || item["reason"] != "manual" {
            return Err("compact_projection_invalid".into());
        }
        if item["status"] == "running" { return Ok(json!({"state":"running","message_id":message})); }
        let succeeded = item["status"] == "completed";
        if !succeeded && item["status"] != "failed" { return Err("compact_projection_invalid".into()); }
        if succeeded && !item["summary"].is_string() { return Err("compact_projection_invalid".into()); }
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(item).map_err(|e|e.to_string())?));
        {
            let mut ledger = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
            let record = ledger.conversations.get_mut(conversation_id).ok_or("opencode_conversation_missing")?;
            // Deletion, context replacement, another reconciliation or an
            // active foreground retirement may have happened during GET.
            if record.current_session != session || record.compact_message_id.as_deref() != Some(message.as_str())
                || record.compact_target != target || !record.compact_pending || record.abandoned || record.delete_pending {
                return Err("compact_stale_revision".into());
            }
            let before = record.clone();
            record.compact_pending = false;
            if succeeded {
                record.compacted_through = record.compacted_through.max(target);
                record.compact_context_digest = Some(digest.clone()); record.compacted_at_ms = Some(now_ms());
            }
            if let Err(error) = write_state(&self.data_dir.join("sessions.json"), &*ledger) {
                ledger.conversations.insert(conversation_id.to_owned(), before);
                return Err(error);
            }
        }
        if !succeeded { return Err("compact_failed: original journal remains available".into()); }
        self.emit(json!({"kind":"harness_compacted","conversation_id":conversation_id,"message_id":message,"watermark":target}));
        Ok(json!({"state":"completed","session_id":session,"message_id":message,"watermark":target,"digest":digest}))
    }

    pub async fn conversation_snapshot(&self, id: &str) -> Result<Value, String> {
        let record = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?.conversations.get(id).cloned().ok_or("opencode_conversation_missing")?;
        let api = self.api()?;
        checked_id(&record.current_session,"ses")?;
        let history = api.json(Method::GET,&format!("/api/session/{}/message?limit=100",record.current_session),None).await.map_err(|e|e.public_message())?;
        Ok(json!({"ledger":record,"history":history,"stream_replay":false}))
    }
}
