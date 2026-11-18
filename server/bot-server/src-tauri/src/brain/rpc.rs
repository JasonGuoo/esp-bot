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
                crate::harness::secrets::validate_key(&key)?;
                Some(key)
            } else { None };
            // Swap the child's environment only between turns. The gate also prevents
            // a new device turn from entering while OpenCode restarts; WSS stays up.
            let _turn = brain.turn_gate.try_write().map_err(|_| "api_key_turn_in_progress")?;
            let running = brain.accepting.load(Ordering::Acquire);
            if running { brain.harness.stop().await?; }
            self.keys.set(&account, key)?;
            brain.harness.reset_route(&route_id).await?;
            brain.fast.refresh_keys(|account| brain.harness.session_key(account));
            if running {
                if let Err(error) = brain.harness.start().await {
                    brain.accepting.store(false, Ordering::Release);
                    return Err(format!("api_key_applied_restart_failed: {error}"));
                }
            }
            return Ok(json!({"session_key_set":method == "api_key.save","running":running}));
        }
        if method == "service.start" { return self.start().await; }
        if method == "service.stop" { self.stop().await?; return Ok(json!({"stopped":true})); }
        let brain = self.get()?;
        if matches!(method,"artifact.get"|"artifact.read"|"artifact.import"|"presentation.get"|"presentation.receipts"|"tasks.get"|"tasks.list"|"tasks.events"|"tasks.submit"|"tasks.resume") {
            // The server can be stopped while the local console remains open.
            // Expired/forgotten material must not wait for its worker to restart.
            brain.flush_source_cleanup().await?;
        }
        if method.starts_with("acceptance.") {return acceptance::dispatch(&self.config_dir,method,&payload);}
        if method=="household.graph.status" {return Ok(tokio::time::timeout(std::time::Duration::from_secs(3),brain.graphiti.health()).await.unwrap_or(json!({"ready":false,"state":"busy"})));}
        if method=="household.voices" {
            let mut album=brain.memory.household_rpc(method,&payload)?.ok_or("unsupported_household_method")?;
            let models=&brain.config.models;
            album["models_configured"]=json!(brain.models.configured()
                && models.diarization_model.as_ref().is_some_and(|p|p.join("config.yaml").is_file())
                && models.model_dir.as_ref().is_some_and(|p|p.join("speaker/ecapa-voxceleb/embedding_model.ckpt").is_file())
                && models.speaker_python.as_ref().is_none_or(|p|p.is_file()));
            return Ok(album);
        }
        if method.starts_with("proactive.") {
            // Owner settings, reminders and the journal of unprompted lines (§12.2).
            return brain.memory.proactive_rpc(method,&payload)?.ok_or_else(||"unsupported_proactive_method".into());
        }
        if method.starts_with("household.") {
            let mutation=matches!(method,"household.settings.set"|"household.voice.name"|"household.voice.separate"|"household.annotate"|"household.correct"|"household.forget"|"household.memory.confirm"|"household.memory.forget");
            if mutation {
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate=brain.turn_gate.write().await;
                let sources=brain.memory.household_mutation_sources(method,&payload)?;
                let old_settings=if method=="household.settings.set"{Some(brain.memory.household_settings()?)}else{None};
                // Validate and commit the source transaction before destructive
                // cross-store work. Rejected settings or stale revisions must
                // never erase/fence otherwise valid tasks and artifacts. The
                // source transaction's durable outbox survives cleanup failure.
                let result=brain.memory.household_rpc(method,&payload)?.ok_or("unsupported_household_method")?;
                if method=="household.forget" {brain.tasks.forget_sources(&sources,false).await?;}
                else if method!="household.memory.confirm" && !sources.is_empty(){brain.tasks.invalidate_sources(&sources,false).await?;}
                if let Some(current)=old_settings {
                    if current["history_enabled"]==true && payload["history_enabled"]==false {brain.tasks.forget_sources(&[],true).await?;}
                    else if current["allow_cloud"]==true && payload["allow_cloud"]==false {brain.tasks.invalidate_sources(&[],true).await?;}
                }
                brain.flush_source_cleanup().await?;
                // Fence source-dependent work before waiting on native HTTP
                // cleanup; it must not submit an effect during that wait.
                brain.invalidate_contexts_locked().await?;
                return Ok(result);
            }
            return brain.memory.household_rpc(method,&payload)?.ok_or_else(||"unsupported_household_method".into());
        }
        match method {
            "contracts.get" => Ok(contracts::capabilities()),
            "tasks.status" => Ok(brain.tasks.status()),
            "tasks.list" => value(brain.tasks.list(payload["household_id"].as_str(),payload["state"].as_str(),payload["limit"].as_u64().unwrap_or(100) as usize)?),
            "tasks.get" => value(brain.tasks.get(required(&payload,"task_id")?)?),
            "tasks.events" => value(brain.tasks.events(required(&payload,"task_id")?,payload["after"].as_u64().unwrap_or(0),payload["limit"].as_u64().unwrap_or(100) as usize)?),
            "tasks.cancel" => value(brain.tasks.cancel(required(&payload,"task_id")?).await?),
            "tasks.reconcile" => value(brain.tasks.reconcile(required(&payload,"task_id")?).await?),
            "tasks.submit" => {
                let mut request:tasks::NewTask=parse(payload.get("task").cloned().ok_or("task_required")?)?;
                if !request.input.is_object(){return Err("task_input_object_required".into());}
                let source=brain.memory.household_rpc("household.source",&json!({"conversation_turn_id":request.conversation_turn_id}))?.ok_or("source_missing")?;
                if source["household_id"]!=request.household_id || source["daily_conversation_id"]!=request.daily_conversation_id
                    || source["conversation_turn_id"]!=request.conversation_turn_id || source["source_expired"]==true {return Err("task_source_binding_invalid".into());}
                request.input["private_profile_id"]=source["private_profile_id"].clone();
                value(brain.tasks.submit(request,payload["confirm_effect"].as_bool().unwrap_or(false))?)
            },
            "tasks.resume" => {
                let id=required(&payload,"task_id")?;let task=brain.tasks.get(id)?;
                let source=brain.memory.household_rpc("household.source",&json!({"conversation_turn_id":task.conversation_turn_id}))?.ok_or("source_missing")?;
                if source["source_expired"]==true || !brain.tasks.source_current(&task.conversation_turn_id)?{return Err("task_source_binding_invalid".into());}
                let mut patch=payload.get("input_patch").cloned().filter(Value::is_object).ok_or("input_patch_required")?;
                patch["private_profile_id"]=source["private_profile_id"].clone();
                value(brain.tasks.resume(id,patch,payload["confirm_effect"].as_bool().unwrap_or(false))?)
            },
            "presentation.get" => value(brain.tasks.plan(required(&payload,"presentation_id")?,payload["revision"].as_u64().and_then(|v|u32::try_from(v).ok()))?),
            "presentation.receipts" => value(brain.tasks.receipts(required(&payload,"presentation_id")?)?),
            "artifact.get" => value(brain.tasks.artifacts.get(required(&payload,"artifact_id")?)?),
            "artifact.read" => {
                use base64::{engine::general_purpose::STANDARD,Engine};
                let id=required(&payload,"artifact_id")?;let offset=payload["offset"].as_u64().unwrap_or(0);
                let bytes=brain.tasks.artifacts.read(id,offset,payload["length"].as_u64().unwrap_or(262144).min(1048576) as usize)?;
                let metadata=brain.tasks.artifacts.get(id)?;
                Ok(json!({"artifact_id":id,"offset":offset,"bytes":bytes.len(),"data_base64":STANDARD.encode(&bytes),"eof":offset+bytes.len() as u64>=metadata.bytes,"mime_type":metadata.mime_type}))
            },
            "artifact.import" => {
                use base64::{engine::general_purpose::STANDARD,Engine};
                let encoded=required(&payload,"data_base64")?;if encoded.len()>16*1024*1024{return Err("artifact_import_limit".into());}
                let bytes=STANDARD.decode(encoded).map_err(|_|"artifact_base64_invalid")?;
                value(brain.tasks.artifacts.put(&bytes,required(&payload,"mime_type")?,payload["title"].as_str().unwrap_or("本地文件"),json!({"origin":"console_file_selection"}))?)
            },
            "harness.compact" => brain.harness.compact(required(&payload,"conversation_id")?,true).await,
            "harness.conversation" => brain.harness.conversation_snapshot(required(&payload,"conversation_id")?).await,
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
                let _gate = brain.turn_gate.write().await;
                brain.memory.revoke_enrollment(required(&payload,"profile_id")?,parse(payload.get("kind").cloned().ok_or("identity_kind_required")?)?)?;
                Ok(json!({"revoked":true}))
            },
            "identity.recognize" => {
                let _gate = brain.turn_gate.try_read().map_err(|_|"brain_busy")?;
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
            "harness.route.probe_direct" => brain.harness.probe_direct(required(&payload,"route_id")?).await,
            "harness.route.reset" => { brain.harness.reset_route(required(&payload,"route_id")?).await?; Ok(json!({"reset":true})) },
            "device.credentials.list" => brain.credentials.list(),
            "device.credentials.issue" => brain.credentials.issue(required(&payload,"device_id")?),
            "device.credentials.revoke" => {brain.credentials.revoke(required(&payload,"device_id")?)?;Ok(json!({"revoked":true}))},
            "device.volume.set" => {
                let device=required(&payload,"device_id")?;
                let volume=match payload.get("volume") {None|Some(Value::Null)=>None,Some(value)=>Some(value.as_u64().filter(|v|*v<=60).ok_or("volume_out_of_range")? as u8)};
                let muted=match payload.get("muted") {None|Some(Value::Null)=>None,Some(value)=>Some(value.as_bool().ok_or("mute_invalid")?)};
                let status=brain.device_volumes.set(device,volume,muted)?;
                brain.emit("device.volume",None,json!({"device_id":device}));
                Ok(status)
            },
            "conversation.forget" => {
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate = brain.turn_gate.write().await;
                let id = required(&payload,"conversation_id")?;
                // The console sends an activation ID. Daily/private native
                // contexts can contain several activations; retire those
                // contexts, then erase only this activation's source turns.
                for conversation in brain.memory.household_activation_conversations(id)? {
                    brain.harness.forget_conversation(&conversation).await?;
                }
                brain.harness.forget_conversation(id).await?;
                brain.memory.forget_conversation(id)?;
                brain.flush_source_cleanup().await?;
                Ok(json!({"forgotten":true}))
            },
            "conversation.forget_all" => {brain.forget_all().await?;Ok(json!({"forgotten":true}))},
            "profile.merge" => {
                if payload["guardian_confirmed"]!=true{return Err("profile_merge_confirmation_required".into());}
                let source=required(&payload,"source_profile_id")?;let target=required(&payload,"target_profile_id")?;
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate=brain.turn_gate.write().await;
                let sources=brain.memory.profile_merge_sources(source,target)?;
                brain.invalidate_contexts_locked().await?;
                if !sources.is_empty(){brain.tasks.invalidate_sources(&sources,false).await?;}
                brain.memory.merge_profile(source,target,true)
            },
            "profile.aliases" => brain.memory.profile_aliases(),
            // Editing/revoking a note removes its old injected copies without
            // deleting local history, conversation ownership or extraction jobs.
            "memory.save" | "memory.delete" | "profile.delete" | "profile.update" | "memory.purge" => {
                brain.cancel_scoped(CancelScope::All,"memory_changed").await?;
                let _gate = brain.turn_gate.write().await;
                brain.invalidate_contexts_locked().await?;
                // Finish the short mutation while the turn gate is held.
                // A cancelled IPC future must not leave a detached writer.
                let result=memory_dispatch_now(&brain.memory,method,payload)?;
                brain.flush_source_cleanup().await?;
                Ok(result)
            },
            "profile.create" => memory_dispatch_now(&brain.memory,method,payload),
            "memory.unattributed.assign" => {
                let _gate=brain.turn_gate.write().await;
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
    pub(crate) async fn flush_source_cleanup(&self)->Result<(),String>{
        self.memory.purge_expired()?;self.memory.purge_household_expired()?;
        // Bound one UI request, leaving durable work queued if a very large
        // archive needs more batches. Fail closed instead of serving old bytes.
        for _ in 0..32 {
            let sources=self.memory.pending_household_cleanup()?;
            if sources.is_empty(){self.tasks.purge_expired()?;return Ok(());}
            self.tasks.forget_sources(&sources,false).await?;
            self.memory.complete_household_cleanup(&sources)?;
        }
        Err("source_cleanup_pending".into())
    }
    async fn forget_all(&self) -> Result<(), String> {
        self.cancel_scoped(CancelScope::All,"memory_changed").await?;
        let _gate = self.turn_gate.write().await;
        self.invalidate_contexts_locked().await?;
        self.tasks.forget_sources(&[],true).await?;
        self.memory.forget_all_conversations(true)?;
        self.memory.forget_all_household()?;
        Ok(())
    }

    async fn invalidate_contexts_locked(&self) -> Result<(), String> {
        let ids = self.harness.conversation_ids()?;
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
