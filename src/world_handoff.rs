//! Parent/child handoff for loading a selected map without dropping the current window.
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command},
    sync::mpsc::Sender,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const READY_ENV: &str = "PWE_WORLD_HANDOFF_READY_FILE";
const STATUS_ENV: &str = "PWE_WORLD_HANDOFF_STATUS_FILE";
const FOCUS_ENV: &str = "PWE_WORLD_HANDOFF_FOCUS_FILE";

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    #[link_name = "AllowSetForegroundWindow"]
    fn allow_set_foreground_window(process_id: u32) -> i32;
    #[link_name = "SetForegroundWindow"]
    fn set_foreground_window(window: isize) -> i32;
    #[link_name = "SetFocus"]
    fn set_focus(window: isize) -> isize;
}

#[derive(Debug)]
pub enum Message {
    Progress(String),
    Ready,
    Failed(String),
}

#[derive(Clone)]
pub struct Files {
    ready: PathBuf,
    status: PathBuf,
    focus: PathBuf,
}
impl Files {
    pub fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let stem = format!("pwe-world-handoff-{}-{nonce}", std::process::id());
        let root = std::env::temp_dir();
        let focus = root.join(format!("{stem}.focus"));
        let _ = fs::write(&focus, b"focused");
        Self {
            ready: root.join(format!("{stem}.ready")),
            status: root.join(format!("{stem}.status")),
            focus,
        }
    }
    pub fn configure(&self, command: &mut Command) {
        command
            .env(READY_ENV, &self.ready)
            .env(STATUS_ENV, &self.status)
            .env(FOCUS_ENV, &self.focus);
    }
    pub fn set_focused(&self, focused: bool) {
        let value: &[u8] = if focused { b"focused" } else { b"unfocused" };
        let _ = fs::write(&self.focus, value);
    }
    pub fn watch(self, mut child: Child, tx: Sender<Message>) {
        std::thread::Builder::new()
            .name("world-handoff".into())
            .spawn(move || {
                let mut previous = String::new();
                let started = Instant::now();
                loop {
                    if let Ok(status) = fs::read_to_string(&self.status) {
                        if status != previous {
                            previous.clone_from(&status);
                            let _ = tx.send(Message::Progress(status));
                        }
                    }
                    if self.ready.is_file() {
                        let _ = fs::remove_file(&self.ready);
                        let _ = fs::remove_file(&self.status);
                        let _ = fs::remove_file(&self.focus);
                        let _ = tx.send(Message::Ready);
                        return;
                    }
                    // Never leave the original window on an unbounded loading
                    // overlay if a graphics driver or child event loop stalls.
                    if started.elapsed() >= Duration::from_secs(20 * 60) {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = fs::remove_file(&self.status);
                        let _ = fs::remove_file(&self.focus);
                        let _ = tx.send(Message::Failed(
                            "The new world did not become ready within 20 minutes.".into(),
                        ));
                        return;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let _ = fs::remove_file(&self.status);
                            let _ = fs::remove_file(&self.focus);
                            let _ = tx.send(Message::Failed(format!(
                                "The new world process exited before it was ready ({status})."
                            )));
                            return;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = fs::remove_file(&self.status);
                            let _ = fs::remove_file(&self.focus);
                            let _ = tx.send(Message::Failed(format!(
                                "Could not monitor the new world process: {error}"
                            )));
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            })
            .ok();
    }
}

pub fn is_handoff_child() -> bool {
    std::env::var_os(READY_ENV).is_some()
}

/// Let the replacement map process become foreground when its ready window is
/// shown. Windows normally prevents background processes from stealing focus;
/// the old foreground process grants this permission immediately after spawn.
#[cfg(windows)]
pub fn allow_child_foreground(process_id: u32) {
    if unsafe { allow_set_foreground_window(process_id) } == 0 {
        eprintln!("Windows denied foreground permission for replacement map process {process_id}");
    }
}

/// Activate a replacement map's visible window so movement keys go to the new
/// renderer rather than the old loading window or another foreground app.
#[cfg(windows)]
pub fn activate_child_window(window: &winit::window::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Some(focus_path) = std::env::var_os(FOCUS_ENV).map(PathBuf::from) else {
        return;
    };
    if !fs::read(focus_path).is_ok_and(|status| status == b"focused") {
        eprintln!(
            "Not activating replacement map because the old window lost focus during loading"
        );
        return;
    }

    let Ok(handle) = window.window_handle() else {
        eprintln!("Could not obtain the replacement map window handle for focus activation");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        eprintln!("Replacement map window did not expose a Win32 handle");
        return;
    };

    let hwnd = handle.hwnd.get();
    if unsafe { set_foreground_window(hwnd) } != 0 {
        unsafe {
            set_focus(hwnd);
        }
    } else {
        eprintln!("Windows refused to focus the replacement map window");
    }
}

/// Publish a status from the loading process for the old window's loading overlay.
pub fn publish_progress(message: &str) {
    let Some(path) = std::env::var_os(STATUS_ENV).map(PathBuf::from) else {
        return;
    };
    // Status text is short; a direct write is enough and lets the parent poll
    // without introducing a second IPC service or named-pipe lifetime.
    let _ = fs::write(path, message.as_bytes());
}

/// Called only after the replacement renderer and its hidden window are ready.
pub fn signal_ready() {
    let Some(path) = std::env::var_os(READY_ENV).map(PathBuf::from) else {
        return;
    };
    let _ = fs::write(path, b"ready");
}
