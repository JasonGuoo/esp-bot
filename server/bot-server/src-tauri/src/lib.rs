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
mod diagnostics;
mod version;
mod capture_permission;

use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Current companion console and the diagnostic link used by the current firmware.
pub fn run() {
    let _ = version::info();
    tauri::Builder::default()
        .on_permission_request(|webview, kind| {
            use tauri::webview::{PermissionKind, PermissionResponse};
            // Tauri 2's default maps to WKPermissionDecision::Prompt. On the
            // packaged custom origin that prompt can remain pending forever.
            // Only our primary local UI may request media; macOS still owns
            // the separate app-level TCC consent. Permissions-Policy also
            // excludes the remote media-card frames from capture access.
            let local = webview.label() == "main" && webview.url().is_ok_and(|url|
                url.username().is_empty() && url.password().is_none() &&
                ((url.scheme() == "tauri" && url.host_str() == Some("localhost") && url.port().is_none()) ||
                 (cfg!(debug_assertions) && url.scheme() == "http" &&
                  url.host_str() == Some("127.0.0.1") && url.port() == Some(1421))));
            if local && matches!(kind, PermissionKind::Microphone | PermissionKind::Camera) {
                PermissionResponse::Allow
            } else {
                PermissionResponse::Deny
            }
        })
        .setup(move |app| {
            let legacy_dir = app.path().app_config_dir()?;
            let config_dir = data_root::prepare(&app.path().home_dir()?, &legacy_dir)?;
            if diagnostics::init(&config_dir).is_err() { eprintln!("[diagnostics] init_failed"); }
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
                diagnostics::event(&event);
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
            tauri::async_runtime::spawn(async move { brain_host.auto_start().await; });

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
        .invoke_handler(tauri::generate_handler![brain::companion_request, usb_debug::usb_debug_request,
            capture_permission::request_microphone_access])
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
