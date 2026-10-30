mod lan;

use std::sync::Arc;
use tauri::{Emitter, State};

#[tauri::command]
fn lan_snapshot(state: State<'_, Arc<lan::LanState>>) -> lan::LanSnapshot {
    state.snapshot()
}

/// Tauri is only the local console; the LAN transport has no dependency on UI widgets.
pub fn run() {
    let state = Arc::new(lan::LanState::default());
    tauri::Builder::default()
        .manage(Arc::clone(&state))
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let sink: lan::EventSink = Arc::new(move |event| {
                let _ = app_handle.emit("lan:event", event);
            });
            let state_for_server = Arc::clone(&state);
            tauri::async_runtime::spawn(async move {
                if let Err(message) =
                    lan::serve(Arc::clone(&state_for_server), Arc::clone(&sink)).await
                {
                    state_for_server.fail(&message);
                    sink(lan::LanEvent::new("server_error", None, &message, None));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![lan_snapshot])
        .run(tauri::generate_context!())
        .expect("ESP Bot Server console failed to start");
}
