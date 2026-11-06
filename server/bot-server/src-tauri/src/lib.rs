mod config;
mod identity;
mod lan;
mod pairing;
mod provider;
mod harness;
mod memory;
mod brain;

use std::sync::Arc;
use tauri::{Emitter, Manager, State};

use pairing::PairingStore;
use provider::ProviderRegistry;

/// Mirrors the legacy Python Brain cap (server/espbot/app.py).
const CHAT_MESSAGE_MAX_CHARS: usize = 4000;

#[tauri::command]
fn lan_snapshot(state: State<'_, Arc<lan::LanState>>) -> lan::LanSnapshot {
    state.snapshot()
}

/// Pushes a validated `face.set` command to a connected bot and returns the
/// command id. Gaze fields stay available for manual renderer testing only;
/// the server never generates per-frame gaze itself.
#[tauri::command]
fn send_face_command(
    hub: State<'_, Arc<lan::CommandHub>>,
    device_id: String,
    payload: serde_json::Value,
) -> Result<String, String> {
    hub.send(&device_id, lan::METHOD_FACE_SET, &payload).map_err(|e| e.to_string())
}

/// Live command-channel status for the UI; not part of the wire protocol.
#[tauri::command]
fn command_status(hub: State<'_, Arc<lan::CommandHub>>, device_id: String) -> serde_json::Value {
    serde_json::json!({
        "device_id": device_id,
        "live": hub.has_live_session(&device_id),
        "pending_commands": hub.pending_count(&device_id),
    })
}

#[tauri::command]
async fn provider_status(registry: State<'_, Arc<ProviderRegistry>>) -> Result<serde_json::Value, String> {
    let mut status = registry.status();
    match registry.provider() {
        Some(current) => {
            let health = current.health().await;
            status["health_ok"] = serde_json::json!(health.ok);
            status["health_detail"] = serde_json::json!(health.detail);
        }
        None => {
            status["health_ok"] = serde_json::json!(false);
            status["health_detail"] = serde_json::json!("no provider selected in config.json");
        }
    }
    Ok(status)
}

#[tauri::command]
fn provider_reload(registry: State<'_, Arc<ProviderRegistry>>) -> serde_json::Value {
    registry.reload();
    registry.status()
}

/// One-shot chat with the selected local provider; on success the reply is
/// pushed to the bot as a `face.set` subtitle + happy expression, exactly
/// like the Python prototype's /v1/chat. Delivery problems do not hide the
/// reply: `send_error` is reported alongside it.
#[tauri::command]
async fn chat(
    registry: State<'_, Arc<ProviderRegistry>>,
    hub: State<'_, Arc<lan::CommandHub>>,
    device_id: String,
    message: String,
) -> Result<serde_json::Value, String> {
    if message.chars().count() > CHAT_MESSAGE_MAX_CHARS {
        return Err(format!("message_too_long: max {CHAT_MESSAGE_MAX_CHARS} characters"));
    }
    let reply = registry.chat(&message).await.map_err(|e| format!("{}: {e}", e.code()))?;
    let subtitle: String = reply.chars().take(lan::SUBTITLE_MAX_CHARS).collect();
    let payload = serde_json::json!({ "subtitle": subtitle, "expression": "happy" });
    match hub.send(&device_id, lan::METHOD_FACE_SET, &payload) {
        Ok(command_id) => Ok(serde_json::json!({ "text": reply, "command_id": command_id })),
        Err(e) => Ok(serde_json::json!({ "text": reply, "command_id": null, "send_error": e.to_string() })),
    }
}

#[tauri::command]
fn pairing_list(store: State<'_, Arc<PairingStore>>) -> serde_json::Value {
    serde_json::json!({
        "devices": store
            .list()
            .into_iter()
            .map(|(device_id, record)| serde_json::json!({
                "device_id": device_id,
                "state": record.state,
                "name": record.name,
                "first_seen_ms": record.first_seen_ms,
                "approved_at_ms": record.approved_at_ms,
            }))
            .collect::<Vec<_>>()
    })
}

#[tauri::command]
fn pairing_approve(store: State<'_, Arc<PairingStore>>, device_id: String) -> Result<(), String> {
    store.approve(&device_id)
}

#[tauri::command]
fn pairing_deny(store: State<'_, Arc<PairingStore>>, device_id: String) -> Result<(), String> {
    store.deny(&device_id)
}

#[tauri::command]
fn pairing_remove(store: State<'_, Arc<PairingStore>>, device_id: String) -> Result<(), String> {
    store.remove(&device_id)
}

/// Tauri is only the local console; the LAN transport has no dependency on UI widgets.
pub fn run() {
    let state = Arc::new(lan::LanState::default());
    tauri::Builder::default()
        .manage(Arc::clone(&state))
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let sink: lan::EventSink = Arc::new(move |event| {
                // Console events also go to stderr so headless/debug runs
                // (running the binary directly, log capture) can be diagnosed
                // without the window.
                eprintln!("[{:.3}] {} {} {}", event.at_ms as f64 / 1000.0, event.kind, event.device_id.as_deref().unwrap_or("-"), event.detail);
                let _ = app_handle.emit("lan:event", event);
            });

            // The config dir backs the persistent identity, pairing registry
            // and provider config; without it the console still opens, but
            // the LAN side reports the exact reason and stops.
            let config_dir = match app.path().app_config_dir() {
                Ok(dir) => dir,
                Err(e) => {
                    let message = format!("cannot resolve app config dir: {e}");
                    state.fail(&message);
                    sink(lan::LanEvent::new("server_error", None, &message, None));
                    return Ok(());
                }
            };
            let server_id = match identity::load_or_create_server_id(&config_dir) {
                Ok(server_id) => server_id,
                Err(message) => {
                    state.fail(&message);
                    sink(lan::LanEvent::new("server_error", None, &message, None));
                    return Ok(());
                }
            };
            let pairing = Arc::new(PairingStore::new(&config_dir, Arc::clone(&sink)));
            let pairing_for_gate = Arc::clone(&pairing);
            let hub = lan::CommandHub::with_approval(
                Arc::clone(&sink),
                Box::new(move |device_id| pairing_for_gate.is_approved(device_id)),
            );
            app.manage(Arc::clone(&hub));
            app.manage(Arc::clone(&pairing));
            let registry = ProviderRegistry::new(config_dir.clone(), Arc::clone(&sink));
            app.manage(Arc::clone(&registry));

            let event_app = app.handle().clone();
            let brain_sink: brain::EventSink = Arc::new(move |event| {
                // Do not persist prompts, transcripts, embeddings, tokens or media in logs.
                eprintln!("[brain] {}", event.get("kind").and_then(serde_json::Value::as_str).unwrap_or("event"));
                let _ = event_app.emit("brain:event", event);
            });
            let resource_worker = app.path().resource_dir().ok().map(|dir|dir.join("local-models/worker.py"))
                .filter(|path|path.is_file())
                .or_else(|| {
                    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../local-models/worker.py");
                    path.is_file().then_some(path)
                });
            let brain_host = brain::BrainHost::new(config_dir, resource_worker, brain_sink);
            app.manage(Arc::clone(&brain_host));
            if let Ok(runtime) = brain_host.get() {
                if runtime.config.auto_start {
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = runtime.start().await {
                            runtime.emit("service.start_failed", None, serde_json::json!({"error":error}));
                        }
                    });
                }
            }

            // Window/webview diagnostics: a blank window must never be
            // silent — log what each webview loaded (or failed to).
            for (label, window) in app.webview_windows() {
                match window.url() {
                    Ok(url) => eprintln!("[window] {label} -> {url}"),
                    Err(e) => eprintln!("[window] {label} -> url error: {e}"),
                }
            }

            let state_for_server = Arc::clone(&state);
            let hub_for_server = Arc::clone(&hub);
            let pairing_for_server = Arc::clone(&pairing);
            tauri::async_runtime::spawn(async move {
                if let Err(message) = lan::serve(
                    Arc::clone(&state_for_server),
                    Arc::clone(&sink),
                    server_id,
                    hub_for_server,
                    pairing_for_server,
                )
                .await
                {
                    state_for_server.fail(&message);
                    sink(lan::LanEvent::new("server_error", None, &message, None));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            lan_snapshot,
            send_face_command,
            command_status,
            provider_status,
            provider_reload,
            chat,
            pairing_list,
            pairing_approve,
            pairing_deny,
            pairing_remove,
            brain::companion_request
        ])
        .on_page_load(|_webview, payload| {
            eprintln!("[page] {:?} {}", payload.event(), payload.url());
        })
        .build(tauri::generate_context!())
        .expect("ESP Bot Server console failed to start")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                static CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !CLOSING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    api.prevent_exit();
                    let host = app.try_state::<Arc<brain::BrainHost>>().map(|state|Arc::clone(&state));
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(host) = host {
                            let _ = tokio::time::timeout(std::time::Duration::from_secs(12), host.shutdown()).await;
                        }
                        handle.exit(0);
                    });
                }
            }
        });
}
