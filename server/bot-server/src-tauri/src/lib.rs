mod identity;
#[cfg(feature = "lan-diagnostics")]
mod lan;
#[cfg(feature = "lan-diagnostics")]
mod pairing;
mod time;
mod harness;
mod memory;
mod brain;
mod face_catalog_generated;
mod usb_debug;
mod data_root;

use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Current companion console and the diagnostic link used by the current firmware.
pub fn run() {
    tauri::Builder::default()
        .setup(move |app| {
            let legacy_dir = app.path().app_config_dir()?;
            let config_dir = data_root::prepare(&app.path().home_dir()?, &legacy_dir)?;
            app.manage(Arc::new(usb_debug::UsbDebug::new(config_dir.clone())));
            #[cfg(feature = "lan-diagnostics")]
            let diagnostics: Option<brain::DiagnosticSnapshot> = {
                let state = start_lan_diagnostics(app.handle(), &config_dir);
                Some(Arc::new(move || serde_json::to_value(state.snapshot()).unwrap_or_default()))
            };
            #[cfg(not(feature = "lan-diagnostics"))]
            let diagnostics = None;

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
            let brain_host = brain::BrainHost::new(config_dir, resource_worker, brain_sink, diagnostics);
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![brain::companion_request, usb_debug::usb_debug_request])
        .on_page_load(|_webview, payload| {
            eprintln!("[page] {:?} {}", payload.event(), payload.url());
        })
        .build(tauri::generate_context!())
        .expect("ESP Bot Server console failed to start")
        .run(|app, event| {
            // Native macOS Quit can go straight from applicationWillTerminate to
            // RunEvent::Exit. Wait for owned child processes before returning to Cocoa.
            if let tauri::RunEvent::Exit = event {
                if let Some(debug) = app.try_state::<Arc<usb_debug::UsbDebug>>() { debug.stop(); }
                if let Some(host) = app.try_state::<Arc<brain::BrainHost>>() {
                    tauri::async_runtime::block_on(async {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(12), host.shutdown()).await;
                    });
                }
            }
        });
}

/// Discovery/status only in the current console. No private media or manual face commands.
#[cfg(feature = "lan-diagnostics")]
fn start_lan_diagnostics(app: &tauri::AppHandle, config_dir: &std::path::Path) -> Arc<lan::LanState> {
    let state = Arc::new(lan::LanState::default());
    let app_handle = app.clone();
    let sink: lan::EventSink = Arc::new(move |event| {
        eprintln!("[lan] {} {}", event.kind, event.detail);
        let _ = app_handle.emit("lan:event", event);
    });
    let server_id = match identity::load_or_create_server_id(config_dir) {
        Ok(id) => id,
        Err(message) => { eprintln!("[lan] {message}"); state.fail(&message); return state; }
    };
    let pairing = Arc::new(pairing::PairingStore::new(config_dir, Arc::clone(&sink)));
    let pairing_for_gate = Arc::clone(&pairing);
    let hub = lan::CommandHub::with_approval(Arc::clone(&sink), Box::new(move |id| pairing_for_gate.is_approved(id)));
    let state_for_server = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        if let Err(message) = lan::serve(Arc::clone(&state_for_server), Arc::clone(&sink), server_id, hub, pairing).await {
            state_for_server.fail(&message);
            sink(lan::LanEvent::new("server_error", None, &message, None));
        }
    });
    state
}
