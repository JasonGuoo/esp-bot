//! macOS owns microphone consent. Requests are explicit, shared while pending,
//! bounded for the caller, and contain no capture, model work or media storage.

#[cfg(target_os = "macos")]
mod macos {
    use std::{ffi::c_void, sync::OnceLock, time::Duration};
    use tokio::sync::watch;

    extern "C" {
        fn espbot_microphone_status() -> i32;
        fn espbot_request_microphone(done: extern "C" fn(*mut c_void, bool), context: *mut c_void);
    }
    extern "C" fn completed(context: *mut c_void, granted: bool) {
        // Exactly one completion from AVCaptureDevice owns this allocation.
        let sender = unsafe { Box::from_raw(context.cast::<watch::Sender<Option<bool>>>()) };
        sender.send_replace(Some(granted));
    }

    pub async fn request(app: tauri::AppHandle) -> Result<(), String> {
        match unsafe { espbot_microphone_status() } {
            1 => return Ok(()),
            0 => return Err("请在 macOS 系统设置 → 隐私与安全性 → 麦克风中允许小呆录音。".into()),
            _ => {}
        }
        // One native prompt per process, even if a caller times out/cancels.
        // A late native answer remains valid for later explicit attempts.
        static PENDING: OnceLock<watch::Sender<Option<bool>>> = OnceLock::new();
        let pending = PENDING.get_or_init(|| {
            let (sender, _) = watch::channel(None);
            let completion = sender.clone();
            if app.run_on_main_thread(move || unsafe {
                espbot_request_microphone(completed, Box::into_raw(Box::new(completion)).cast());
            }).is_err() { sender.send_replace(Some(false)); }
            sender
        });
        let mut answer = pending.subscribe();
        let granted = tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                if let Some(value) = *answer.borrow_and_update() { return value; }
                if answer.changed().await.is_err() { return false; }
            }
        }).await.map_err(|_| "系统麦克风授权仍在等待，可取消后重试或选择 WAV 文件。".to_string())?;
        if granted { Ok(()) } else { Err("系统未允许录音，请检查 macOS 麦克风权限。".into()) }
    }
}

#[tauri::command]
pub async fn request_microphone_access(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    { macos::request(app).await }
    #[cfg(not(target_os = "macos"))]
    { let _ = app; Ok(()) }
}
