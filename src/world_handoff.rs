//! Parent/child handoff for changing worlds without tearing down the current
//! renderer.
//!
//! The old, foreground process owns the handoff. It writes a focus file from
//! its own `WindowEvent::Focused` events, grants the replacement process
//! foreground permission at spawn, and — when the child reports its first usable
//! frame — activates the child's published Win32 HWND from the old process. The
//! child never calls `SetForegroundWindow` itself and never steals focus after
//! the user Alt-Tabs away.
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command},
    sync::{mpsc::Sender, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const READY_ENV: &str = "PWE_WORLD_HANDOFF_READY_FILE";
const STATUS_ENV: &str = "PWE_WORLD_HANDOFF_STATUS_FILE";
const FOCUS_ENV: &str = "PWE_WORLD_HANDOFF_FOCUS_FILE";
const HWND_ENV: &str = "PWE_WORLD_HANDOFF_HWND_FILE";
const FAILED_PREFIX: &str = "error:";

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    #[link_name = "AllowSetForegroundWindow"]
    fn allow_set_foreground_window(process_id: u32) -> i32;
    #[link_name = "SetForegroundWindow"]
    fn set_foreground_window(window: isize) -> i32;
}

/// Default upper bound for a replacement world to become visible. A driver
/// stall, a dead child or a missing readiness signal must never leave the old
/// viewer on an unbounded, non-interactive loading overlay.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Progress(String),
    /// The replacement has drawn its first usable frame. `activated` is false
    /// when the user Alt-Tabbed away or no Win32 HWND was published.
    Ready {
        activated: bool,
    },
    Failed(String),
}

/// A headless decision about one watcher poll. `watch` turns filesystem and
/// child-process facts into this value; the policy is unit-testable without a
/// Windows desktop or a running game client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Wait,
    Ready { activate_hwnd: Option<u64> },
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSnapshot {
    pub ready: bool,
    pub focused: bool,
    pub hwnd: Option<u64>,
    pub status: Option<String>,
    pub child_exit: Option<String>,
    pub timed_out: bool,
}

/// Decide the next handoff action. An explicit child error, a child that exits
/// early, and a bounded timeout are all recoverable failures; only a ready file
/// ends the overlay successfully. The old process activates the replacement
/// only while it still owns foreground and the child published a real HWND.
pub fn decide(snapshot: &WatchSnapshot) -> Decision {
    if let Some(status) = snapshot.status.as_deref() {
        if let Some(reason) = status.strip_prefix(FAILED_PREFIX) {
            return Decision::Fail(reason.trim().to_string());
        }
    }
    if snapshot.ready {
        let activate_hwnd = if snapshot.focused {
            snapshot.hwnd.filter(|hwnd| *hwnd != 0)
        } else {
            None
        };
        return Decision::Ready { activate_hwnd };
    }
    if let Some(exit) = snapshot.child_exit.as_deref() {
        return Decision::Fail(format!(
            "The new world process exited before it was ready ({exit})."
        ));
    }
    if snapshot.timed_out {
        return Decision::Fail(
            "The new world did not become ready within the loading timeout.".into(),
        );
    }
    Decision::Wait
}

#[derive(Clone, Debug)]
pub struct Files {
    ready: PathBuf,
    status: PathBuf,
    focus: PathBuf,
    hwnd: PathBuf,
}

impl Files {
    pub fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let stem = format!("pwe-world-handoff-{}-{nonce}", std::process::id());
        let root = std::env::temp_dir();
        let files = Self {
            ready: root.join(format!("{stem}.ready")),
            status: root.join(format!("{stem}.status")),
            focus: root.join(format!("{stem}.focus")),
            hwnd: root.join(format!("{stem}.hwnd")),
        };
        let _ = fs::write(&files.focus, b"focused");
        let _ = fs::remove_file(&files.hwnd);
        let _ = fs::remove_file(&files.ready);
        let _ = fs::remove_file(&files.status);
        files
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .env(READY_ENV, &self.ready)
            .env(STATUS_ENV, &self.status)
            .env(FOCUS_ENV, &self.focus)
            .env(HWND_ENV, &self.hwnd);
    }

    /// Called by the old window's focus handler. An Alt-Tab makes this false;
    /// the replacement stays visible but is not forced to the foreground.
    pub fn set_focused(&self, focused: bool) {
        let value: &[u8] = if focused { b"focused" } else { b"unfocused" };
        let _ = fs::write(&self.focus, value);
    }

    fn focused(&self) -> bool {
        fs::read(&self.focus)
            .map(|status| status == b"focused")
            .unwrap_or(false)
    }

    fn published_hwnd(&self) -> Option<u64> {
        fs::read_to_string(&self.hwnd)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    }

    pub fn cleanup(&self) {
        for path in [&self.ready, &self.status, &self.focus, &self.hwnd] {
            let _ = fs::remove_file(path);
        }
    }

    /// Watch a replacement process from the old foreground owner. All temporary
    /// files are removed on success, failure and timeout; a stalled or failed
    /// child is killed so no orphan keeps the handoff alive.
    pub fn watch(self, child: Child, tx: Sender<Message>, timeout: Duration) -> Result<(), String> {
        // This call is made on the old process's foreground thread, immediately
        // after spawning the child. The child itself does not rely on inheriting
        // this short-lived permission.
        let child = Arc::new(Mutex::new(child));
        let child_id = child
            .lock()
            .map_err(|error| format!("child process lock poisoned: {error}"))?
            .id();
        allow_child_foreground(child_id);
        let watch_child = child.clone();
        let files = Arc::new(self.clone());
        let watch_files = files.clone();
        let spawned = std::thread::Builder::new()
            .name("world-handoff".into())
            .spawn(move || {
                let files = &*watch_files;
                let mut child = watch_child
                    .lock()
                    .expect("world handoff child lock poisoned");
                let mut previous_status = String::new();
                let started = Instant::now();
                loop {
                    let status = fs::read_to_string(&files.status).ok();
                    if let Some(status) = status.as_ref() {
                        if status != &previous_status && !status.starts_with(FAILED_PREFIX) {
                            previous_status.clone_from(status);
                            let _ = tx.send(Message::Progress(status.clone()));
                        }
                    }
                    let snapshot = WatchSnapshot {
                        ready: files.ready.is_file(),
                        focused: files.focused(),
                        hwnd: files.published_hwnd(),
                        status: status.clone(),
                        child_exit: match child.try_wait() {
                            Ok(Some(exit)) => Some(exit.to_string()),
                            Ok(None) => None,
                            Err(error) => Some(format!("monitor error: {error}")),
                        },
                        timed_out: started.elapsed() >= timeout,
                    };
                    match decide(&snapshot) {
                        Decision::Wait => {}
                        Decision::Ready { activate_hwnd } => {
                            let activated = activate_hwnd
                                .map(|hwnd| activate_window_from_parent(hwnd))
                                .unwrap_or(false);
                            if !activated && snapshot.focused {
                                eprintln!(
                                    "Replacement world is ready, but no usable Win32 HWND was published; leaving its visible window unforced."
                                );
                            }
                            files.cleanup();
                            let _ = tx.send(Message::Ready { activated });
                            return;
                        }
                        Decision::Fail(reason) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            files.cleanup();
                            let _ = tx.send(Message::Failed(reason));
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            });
        if let Err(error) = spawned {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            files.cleanup();
            return Err(format!("could not start world handoff watcher: {error}"));
        }
        Ok(())
    }
}

/// Bounded timeout. An environment override lets CI exercise the failure path
/// without waiting twenty minutes; production has a generous tree-bake budget.
pub fn timeout() -> Duration {
    std::env::var("PWE_WORLD_HANDOFF_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=7200).contains(seconds))
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

pub fn is_handoff_child() -> bool {
    std::env::var_os(READY_ENV).is_some()
}

/// Child publishes its Win32 HWND as soon as the visible window exists. Only
/// the old foreground process reads this file and may activate that HWND.
pub fn publish_window_handle(hwnd: usize) {
    if let Some(path) = std::env::var_os(HWND_ENV).map(PathBuf::from) {
        let _ = fs::write(path, hwnd.to_string());
    }
}

/// Publish loading progress from the child for the old window's non-interactive
/// overlay. A short direct write avoids a second IPC service or pipe lifetime.
pub fn publish_progress(message: &str) {
    if let Some(path) = std::env::var_os(STATUS_ENV).map(PathBuf::from) {
        let _ = fs::write(path, message.as_bytes());
    }
}

/// Report a child-side load error or caught panic. The parent watcher treats
/// this status as a failure even if the child has not exited yet.
pub fn publish_failure(reason: &str) {
    if let Some(path) = std::env::var_os(STATUS_ENV).map(PathBuf::from) {
        let _ = fs::write(path, format!("{FAILED_PREFIX}{reason}"));
    }
}

/// A post-load main-world restart is a fresh independent viewer, not another
/// handoff child. Remove the parent's temporary-file environment so the new
/// process does not publish readiness or failure into the old overlay.
pub fn clear_child_environment(command: &mut Command) {
    for name in [READY_ENV, STATUS_ENV, FOCUS_ENV, HWND_ENV] {
        command.env_remove(name);
    }
}

/// Called only after a visible window and its first usable frame are presented.
pub fn signal_ready() {
    if let Some(path) = std::env::var_os(READY_ENV).map(PathBuf::from) {
        let _ = fs::write(path, b"ready");
    }
}

#[cfg(windows)]
pub fn allow_child_foreground(process_id: u32) {
    if unsafe { allow_set_foreground_window(process_id) } == 0 {
        eprintln!("Windows denied foreground permission for replacement map process {process_id}");
    }
}

#[cfg(not(windows))]
pub fn allow_child_foreground(_process_id: u32) {}

/// Parent-side activation. The old process is still alive when this runs; the
/// replacement never calls SetForegroundWindow itself.
#[cfg(windows)]
fn activate_window_from_parent(hwnd: u64) -> bool {
    let ok = unsafe { set_foreground_window(hwnd as isize) } != 0;
    if !ok {
        eprintln!("Windows refused to focus the replacement map window");
    }
    ok
}

#[cfg(not(windows))]
fn activate_window_from_parent(_hwnd: u64) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> WatchSnapshot {
        WatchSnapshot {
            ready: false,
            focused: true,
            hwnd: Some(0x1234),
            status: None,
            child_exit: None,
            timed_out: false,
        }
    }

    #[test]
    fn ready_and_focused_activates_the_published_child_hwnd() {
        let mut state = snapshot();
        state.ready = true;
        assert_eq!(
            decide(&state),
            Decision::Ready {
                activate_hwnd: Some(0x1234)
            }
        );
    }

    #[test]
    fn alt_tab_blocks_forced_activation_but_still_completes_handoff() {
        let mut state = snapshot();
        state.ready = true;
        state.focused = false;
        assert_eq!(
            decide(&state),
            Decision::Ready {
                activate_hwnd: None
            }
        );
    }

    #[test]
    fn missing_hwnd_is_not_a_failure() {
        let mut state = snapshot();
        state.ready = true;
        state.hwnd = None;
        assert_eq!(
            decide(&state),
            Decision::Ready {
                activate_hwnd: None
            }
        );
    }

    #[test]
    fn child_error_exit_and_timeout_are_recoverable_failures() {
        let mut state = snapshot();
        state.status = Some("error:bad instance terrain".into());
        assert_eq!(
            decide(&state),
            Decision::Fail("bad instance terrain".into())
        );
        state.status = None;
        state.child_exit = Some("exit status: 1".into());
        assert!(
            matches!(decide(&state), Decision::Fail(reason) if reason.contains("exited before"))
        );
        state.child_exit = None;
        state.timed_out = true;
        assert!(matches!(decide(&state), Decision::Fail(reason) if reason.contains("timeout")));
    }

    #[test]
    fn progress_and_wait_are_not_terminal_decisions() {
        let mut state = snapshot();
        state.status = Some("Loading terrain tiles 4/10".into());
        assert_eq!(decide(&state), Decision::Wait);
    }

    #[test]
    fn a_restart_progress_notice_completes_handoff_as_ready() {
        let mut state = snapshot();
        state.ready = true;
        state.hwnd = None;
        state.status = Some(
            "Map a01 failed after loading. The explorer is restarting in the main world.".into(),
        );
        assert_eq!(
            decide(&state),
            Decision::Ready {
                activate_hwnd: None
            }
        );
    }

    #[test]
    fn child_environment_and_focus_file_carry_the_handoff_state() {
        let files = Files::new();
        let mut command = Command::new("ignored");
        files.configure(&mut command);
        let env: std::collections::HashMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.is_some()))
            .collect();
        assert_eq!(env.get(READY_ENV), Some(&true));
        assert_eq!(env.get(STATUS_ENV), Some(&true));
        assert_eq!(env.get(FOCUS_ENV), Some(&true));
        assert_eq!(env.get(HWND_ENV), Some(&true));
        assert!(files.focused());
        files.set_focused(false);
        assert!(!files.focused());
        files.cleanup();
    }

    #[test]
    fn a_restart_command_does_not_inherit_the_handoff_environment() {
        let files = Files::new();
        let mut command = Command::new("ignored");
        files.configure(&mut command);
        clear_child_environment(&mut command);
        let env: std::collections::HashMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.is_some()))
            .collect();
        for name in [READY_ENV, STATUS_ENV, FOCUS_ENV, HWND_ENV] {
            assert!(!env.get(name).copied().unwrap_or(false), "{name} inherited");
        }
        files.cleanup();
    }

    #[test]
    fn temporary_files_are_removed_on_cleanup() {
        let files = Files::new();
        fs::write(&files.ready, b"ready").unwrap();
        fs::write(&files.hwnd, b"123").unwrap();
        files.cleanup();
        assert!(!files.ready.exists());
        assert!(!files.status.exists());
        assert!(!files.focus.exists());
        assert!(!files.hwnd.exists());
    }
}
