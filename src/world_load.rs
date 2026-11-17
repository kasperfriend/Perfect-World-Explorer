//! Recoverable map loading: an unreadable instance must not pin the viewer on a
//! broken loading screen.
//!
//! The policy is deliberately free of client, window and GPU dependencies so it
//! can be tested headless. The host passes a closure that resolves and loads one
//! map ID; this module owns the "try the selected map, then the main world"
//! retry and the panic-to-error conversion.
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::atomic::{AtomicBool, Ordering},
};

static RECOVERABLE_MAP_OPERATION: AtomicBool = AtomicBool::new(false);
static PANIC_LOCATION_HOOK: std::sync::Once = std::sync::Once::new();
thread_local! {
    static RECOVERABLE_PANIC_LOCATION: std::cell::RefCell<Option<String>> =
        std::cell::RefCell::new(None);
}

/// True while a map load/render attempt is inside `recoverable_map_operation`.
/// The Windows panic hook uses this to suppress a modal error dialog for a panic
/// the viewer can recover from by falling back to the main world.
pub fn recovering_from_map_failure() -> bool {
    RECOVERABLE_MAP_OPERATION.load(Ordering::SeqCst)
}

/// Install the location-capturing wrapper around the host's panic hook. `main`
/// calls this after its Windows error hook so a recoverable map panic can be
/// reported in the viewer without opening a modal error dialog.
pub fn install_recoverable_panic_hook() {
    PANIC_LOCATION_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // The thread-local location belongs to the thread that panicked;
            // capturing it unconditionally avoids races with another test or
            // worker clearing the process-wide recoverable flag.
            RECOVERABLE_PANIC_LOCATION.with(|location| {
                *location.borrow_mut() = info
                    .location()
                    .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()));
            });
            previous(info);
        }));
    });
}

/// Run one map operation, turning a panic into an error that carries the panic
/// message and (when available) its source location. The global flag around the
/// catch is process-wide because a panic can happen on a worker thread.
pub fn recoverable_map_operation<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    install_recoverable_panic_hook();
    RECOVERABLE_PANIC_LOCATION.with(|location| *location.borrow_mut() = None);
    RECOVERABLE_MAP_OPERATION.store(true, Ordering::SeqCst);
    let result = catch_unwind(AssertUnwindSafe(operation));
    RECOVERABLE_MAP_OPERATION.store(false, Ordering::SeqCst);
    let location = RECOVERABLE_PANIC_LOCATION.with(|location| location.borrow_mut().take());
    result.map_err(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "unknown panic during map loading".into());
        match location {
            Some(location) => format!(
                "unexpected error while loading/rendering the selected map: {message} ({location})"
            ),
            None => format!("unexpected error while loading/rendering the selected map: {message}"),
        }
    })
}

/// Variant that gives the main-world retry the reason for the first failure.
/// The viewer uses this to show a recoverable startup notice on the fallback
/// world's first frame; simpler callers use `load_with_main_world_fallback`.
pub fn load_with_main_world_fallback_notified<T>(
    preferred: &str,
    mut load: impl FnMut(&str, Option<String>) -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    let attempt =
        |id: &str,
         notice: Option<String>,
         load: &mut dyn FnMut(&str, Option<String>) -> Result<T, String>| {
            recoverable_map_operation(|| load(id, notice))?
        };
    match attempt(preferred, None, &mut load) {
        Ok(value) => Ok((value, None)),
        Err(first_error) if !preferred.eq_ignore_ascii_case("world") => {
            let notice = format!(
                "Could not load map {preferred}; returned to the main world.\n\n{first_error}"
            );
            match attempt("world", Some(notice.clone()), &mut load) {
                Ok(value) => Ok((value, Some(notice))),
                Err(main_error) => Err(format!(
                    "Could not load map {preferred} ({first_error}), and the main world fallback also failed ({main_error})."
                )),
            }
        }
        Err(error) => Err(error),
    }
}

/// Attempt the selected map once; if it returns an error or panics, retry the
/// main world. The returned notice explains the fallback and is persisted/shown
/// by the host. A failing main world is never retried.
pub fn load_with_main_world_fallback<T>(
    preferred: &str,
    mut load: impl FnMut(&str) -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    load_with_main_world_fallback_notified(preferred, |id, _| load(id))
}

/// What to do after a selected map has already loaded but the viewer fails.
/// Winit cannot create a second event loop in the same process, so an instance
/// map must be retried in a fresh process with the safe main-world setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostLoadDecision {
    Fatal,
    RestartMainWorld,
}

pub fn post_load_failure_decision(selected_world: &str, viewer_mode: bool) -> PostLoadDecision {
    if viewer_mode && !selected_world.eq_ignore_ascii_case("world") {
        PostLoadDecision::RestartMainWorld
    } else {
        PostLoadDecision::Fatal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_map_error_returns_to_main_world_with_the_reason() {
        let (loaded, notice) = load_with_main_world_fallback("a01", |id| match id {
            "a01" => Err("bad instance terrain".into()),
            "world" => Ok("mainland"),
            _ => Err("unexpected map".into()),
        })
        .unwrap();
        assert_eq!(loaded, "mainland");
        let notice = notice.unwrap();
        assert!(notice.contains("a01"));
        assert!(notice.contains("bad instance terrain"));
    }

    #[test]
    fn main_world_failure_is_not_retried() {
        let mut calls = Vec::new();
        let error = load_with_main_world_fallback("world", |id| {
            calls.push(id.to_string());
            Err::<(), _>("no client".into())
        })
        .unwrap_err();
        assert_eq!(calls, vec!["world"]);
        assert!(error.contains("no client"));
    }

    #[test]
    fn both_attempts_failing_reports_both_reasons() {
        let error =
            load_with_main_world_fallback("a02", |id| Err::<(), _>(format!("{id} cannot load")))
                .unwrap_err();
        assert!(error.contains("a02 cannot load"));
        assert!(error.contains("world cannot load"));
    }

    #[test]
    fn panic_while_loading_instance_retries_main_world() {
        let (loaded, notice) = load_with_main_world_fallback("a02", |id| {
            if id.eq_ignore_ascii_case("world") {
                Ok("mainland")
            } else {
                panic!("malformed instance terrain")
            }
        })
        .unwrap();
        assert_eq!(loaded, "mainland");
        assert!(notice.unwrap().contains("malformed instance terrain"));
    }

    #[test]
    fn caught_map_panic_reports_its_message_and_location() {
        let error = recoverable_map_operation(|| panic!("fixture panic")).unwrap_err();
        assert!(error.contains("fixture panic"));
        assert!(error.contains("world_load.rs:"), "{error}");
    }

    #[test]
    fn notified_retry_receives_the_first_failure_reason() {
        let (notice, fallback_notice) =
            load_with_main_world_fallback_notified("a01", |id, notice| match id {
                "a01" => Err::<Option<String>, _>("instance data missing".into()),
                "world" => Ok(notice),
                _ => Err("unexpected map".into()),
            })
            .unwrap();
        assert_eq!(notice, fallback_notice);
        assert!(notice.unwrap().contains("instance data missing"));
    }

    #[test]
    fn successful_load_has_no_fallback_notice() {
        let (loaded, notice) =
            load_with_main_world_fallback("world", |_| Ok::<_, String>(42)).unwrap();
        assert_eq!(loaded, 42);
        assert!(notice.is_none());
    }

    #[test]
    fn a_post_load_instance_failure_restarts_in_a_fresh_process() {
        assert_eq!(
            post_load_failure_decision("a01", true),
            PostLoadDecision::RestartMainWorld
        );
        assert_eq!(
            post_load_failure_decision("A01", false),
            PostLoadDecision::Fatal
        );
        assert_eq!(
            post_load_failure_decision("world", true),
            PostLoadDecision::Fatal
        );
    }
}
