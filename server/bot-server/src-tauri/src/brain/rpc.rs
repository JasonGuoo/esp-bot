//! Parent-console API. The device gateway deliberately does NOT expose this dispatcher.
use super::*;
use serde::de::DeserializeOwned;

impl BrainHost {
    pub async fn dispatch(&self, method: &str, payload: Value) -> Result<Value, String> {
        if method == "service.status" {
            let mut status = match self.get() {Ok(brain)=>brain.status().await,Err(error)=>json!({"ready":false,"error":error,"config_path":self.config_dir.join("brain.json")})};
            status["device_diagnostics"] = self.diagnostics.as_ref().map(|snapshot| snapshot())
                .unwrap_or_else(|| json!({"running":false,"bots":[],"error":"diagnostics_not_built"}));
            return Ok(status);
        }
        if method == "config.read" {
            return std::fs::read(self.config_dir.join("brain.json")).map_err(|e|e.to_string())
                .and_then(|bytes|serde_json::from_slice(&bytes).map_err(|e|e.to_string()));
        }
        if method == "config.save" {
            let config: BrainConfig = parse(payload)?;
            settings::save(&self.config_dir,&config)?;
            return Ok(json!({"saved":true,"reload_required":true}));
        }
        if method == "service.reload" { return self.reload().await; }
        if method == "api_key.save" || method == "api_key.delete" {
            self.suppress_auto_start();
            let _lock = self.lifecycle.lock().await;
            let brain = self.get()?;
            let route_id = required(&payload, "route_id")?.to_owned();
            let account = brain.harness.api_key_account(&route_id)?;
            let key = if method == "api_key.save" {
                let mut payload = payload;
                let key = payload.get_mut("api_key").ok_or("api_key_required")?.take();
                let Value::String(key) = key else { return Err("api_key_required".into()); };
                let key = zeroize::Zeroizing::new(key);
                crate::harness::secrets::validate_key(key.trim())?;
                Some(key)
            } else { None };
            // Stop before mutation so the old child cannot keep using an outdated key.
            // The lifecycle gate serializes this with start/reload/shutdown.
            brain.stop().await?;
            tokio::task::spawn_blocking(move || match key {
                Some(key) => crate::harness::secrets::save(&account, key.trim()),
                None => crate::harness::secrets::delete(&account),
            }).await.map_err(|_| "api_key_keychain_task_failed")??;
            brain.harness.reset_route(&route_id).await?;
            return Ok(json!({"saved":method == "api_key.save","restart_required":true}));
        }
        if method == "service.start" { return self.start().await; }
        if method == "service.stop" { self.stop().await?; return Ok(json!({"stopped":true})); }
        let brain = self.get()?;
        match method {
            "models.status" => Ok(brain.models.health().await),
            "models.unload" => {
                brain.cancel_scoped(CancelScope::All,"models_unloaded").await?;
                brain.models.stop().await; Ok(json!({"unloaded":true}))
            },
            "assistant.turn" => value(brain.turn(parse(payload)?,true).await?),
            "assistant.cancel" => {
                let scope = match payload.get("scope").and_then(Value::as_str) {
                    Some("all") => CancelScope::All,
                    Some("device_session") => {
                        let id = required(&payload,"conversation_id")?;
                        if !id.starts_with("device-") || !valid_turn_id(id) { return Err("device_session_id_invalid".into()); }
                        CancelScope::DeviceSession(id.to_owned())
                    },
                    Some("turn") | None => CancelScope::Turn(required(&payload,"turn_id")?.to_owned()),
                    _ => return Err("cancel_scope_invalid".into()),
                };
                brain.cancel_scoped(scope,"parent_stop").await?;
                Ok(json!({"cancelled":true}))
            },
            "identity.enroll" => brain.enroll_media(parse(payload)?).await,
            "identity.revoke" => {
                brain.cancel_scoped(CancelScope::All,"identity_revoked").await?;
                let _gate = brain.turn_gate.lock().await;
                brain.memory.revoke_enrollment(required(&payload,"profile_id")?,parse(payload.get("kind").cloned().ok_or("identity_kind_required")?)?)?;
                Ok(json!({"revoked":true}))
            },
            "identity.recognize" => {
                let _gate = brain.turn_gate.try_lock().map_err(|_|"brain_busy")?;
                let kind: memory::IdentityKind = parse(payload.get("kind").cloned().ok_or("identity_kind_required")?)?;
                let media = if kind == memory::IdentityKind::Face { json!({"image_base64":required(&payload,"image_base64")?}) }
                    else {json!({"wav_base64":required(&payload,"wav_base64")?})};
                brain.identify_media(kind,media).await
            },
            "harness.permissions" => brain.harness.permissions().await,
            "harness.questions" => brain.harness.questions().await,
            "harness.question.reply" => {
                let answer = payload.get("answer").cloned().filter(|value| !value.is_null());
                brain.harness.reply_question(required(&payload,"session_id")?,required(&payload,"form_id")?,answer).await?;
                Ok(json!({"replied":true}))
            },
            "harness.permission.reply" => {
                brain.harness.reply_permission(required(&payload,"session_id")?,required(&payload,"request_id")?,payload.get("allow").and_then(Value::as_bool).unwrap_or(false)).await?;
                Ok(json!({"replied":true}))
            },
            "harness.route.reset" => { brain.harness.reset_route(required(&payload,"route_id")?).await?; Ok(json!({"reset":true})) },
            "device.credentials.list" => brain.credentials.list(),
            "device.credentials.issue" => brain.credentials.issue(required(&payload,"device_id")?),
            "device.credentials.revoke" => {brain.credentials.revoke(required(&payload,"device_id")?)?;Ok(json!({"revoked":true}))},
            "conversation.forget" => {
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate = brain.turn_gate.lock().await;
                let id = required(&payload,"conversation_id")?;
                brain.harness.forget_conversation(id).await?;
                brain.memory.forget_conversation(id)?;
                Ok(json!({"forgotten":true}))
            },
            "conversation.forget_all" => {brain.forget_all().await?;Ok(json!({"forgotten":true}))},
            // Editing/revoking a note removes its old injected copies without
            // deleting local history, conversation ownership or extraction jobs.
            "memory.save" | "memory.delete" | "profile.delete" | "profile.update" | "memory.purge" => {
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate = brain.turn_gate.lock().await;
                brain.invalidate_contexts_locked().await?;
                // Finish the short mutation while the turn gate is held.
                // A cancelled IPC future must not leave a detached writer.
                memory_dispatch_now(&brain.memory,method,payload)
            },
            "profile.create" => memory_dispatch_now(&brain.memory,method,payload),
            "memory.unattributed.assign" => {
                let _gate=brain.turn_gate.lock().await;
                brain.memory.assign_unattributed(required(&payload,"id")?,required(&payload,"profile_id")?)?;
                Ok(json!({"assigned":true}))
            },
            "memory.unattributed.delete" => {brain.memory.delete_unattributed(required(&payload,"id")?)?;Ok(json!({"deleted":true}))},
            "profile.list"|"memory.list"|"memory.status"|"identity.list"|"memory.unattributed.list" =>
                memory_dispatch(Arc::clone(&brain.memory),method.to_owned(),payload).await,
            _ => Err("unsupported_parent_console_method".into()),
        }
    }
}

impl Brain {
    async fn forget_all(&self) -> Result<(), String> {
        self.cancel_scoped(CancelScope::All,"memory_changed").await?;
        let _gate = self.turn_gate.lock().await;
        self.invalidate_contexts_locked().await?;
        self.memory.forget_all_conversations(true)?;
        Ok(())
    }

    async fn invalidate_contexts_locked(&self) -> Result<(), String> {
        let ids = self.memory.conversation_ids()?;
        // Fail honestly and retain the ledger if an external session could not be deleted.
        for id in ids { self.harness.forget_conversation(&id).await?; }
        Ok(())
    }
}

async fn memory_dispatch(store: Arc<Store>, method: String, payload: Value) -> Result<Value,String> {
    tokio::task::spawn_blocking(move || memory_dispatch_now(&store,&method,payload))
        .await.map_err(|_|"memory_task_failed".to_string())?
}

fn memory_dispatch_now(store: &Store, method: &str, payload: Value) -> Result<Value,String> {
        match method {
            "profile.list" => value(store.list_profiles()?),
            "profile.create" => value(store.create_profile(parse(payload)?)?),
            "profile.update" => value(store.update_profile(required(&payload,"profile_id")?,parse(payload.get("profile").cloned().ok_or("profile_required")?)?)?),
            "profile.delete" => {store.delete_profile(required(&payload,"profile_id")?)?;Ok(json!({"deleted":true}))},
            "memory.list" => value(store.list_memories(payload.get("profile_id").and_then(Value::as_str))?),
            "memory.unattributed.list" => store.list_unattributed(),
            "memory.save" => value(store.save_memory(parse(payload)?)?),
            "memory.delete" => {store.delete_memory(required(&payload,"id")?)?;Ok(json!({"deleted":true}))},
            "memory.status" => store.status(),
            "memory.purge" => value(store.purge_expired()?),
            "identity.list" => value(store.list_enrollments(payload.get("profile_id").and_then(Value::as_str))?),
            _ => Err("unsupported_memory_method".into()),
        }
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T,String> { serde_json::from_value(value).map_err(|e|format!("request_invalid: {e}")) }
fn value<T:Serialize>(value:T)->Result<Value,String>{serde_json::to_value(value).map_err(|e|e.to_string())}
fn required<'a>(payload:&'a Value,key:&str)->Result<&'a str,String>{payload.get(key).and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or_else(||format!("{key}_required"))}
