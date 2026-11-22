//! Platform-neutral DirectStorage telemetry for the loading UI.
//!
//! `pck::direct_storage` only exists on Windows, but the loading screen's
//! DirectStorage line is drawn on every platform: the standalone window is
//! Windows-only, while the in-viewer overlay a world change shows is compiled
//! everywhere. This module is the one place that bridges the two, so the overlay
//! has no `cfg` in the middle of its paint code and a host without DirectStorage
//! reports zeroed counters instead of failing to compile.
//!
//! It lives in its own file rather than inside `pck` because some forty check
//! binaries include `pck.rs` directly with `#[path]` and have no
//! `direct_storage` sibling; only the viewer declares this module.

/// The counters the loading UI displays.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub attempts: u64,
    pub successes: u64,
    pub fallbacks: u64,
    pub bytes: u64,
    pub active: usize,
}

#[cfg(windows)]
pub(crate) fn stats() -> Stats {
    let stats = crate::pck::direct_storage::read_stats();
    Stats {
        attempts: stats.attempts,
        successes: stats.successes,
        fallbacks: stats.fallbacks,
        bytes: stats.bytes,
        active: stats.active,
    }
}

#[cfg(not(windows))]
pub(crate) fn stats() -> Stats {
    Stats::default()
}

/// One sentence describing what DirectStorage is doing right now.
#[cfg(windows)]
pub(crate) fn state_line(stats: Stats) -> String {
    crate::pck::direct_storage::state_line(crate::pck::direct_storage::ReadStats {
        attempts: stats.attempts,
        successes: stats.successes,
        fallbacks: stats.fallbacks,
        bytes: stats.bytes,
        active: stats.active,
    })
}

#[cfg(not(windows))]
pub(crate) fn state_line(_stats: Stats) -> String {
    "DirectStorage is a Windows feature; standard file I/O is in use".into()
}

/// The totals line, worded exactly like the loading window's.
pub(crate) fn totals_line(stats: Stats) -> String {
    format!(
        "DirectStorage: {} reads / {:.2} MiB | attempts: {} | fallback: {} | active: {}",
        stats.successes,
        stats.bytes as f64 / (1024.0 * 1024.0),
        stats.attempts,
        stats.fallbacks,
        stats.active
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_totals_line_names_every_counter() {
        let stats = Stats {
            attempts: 5,
            successes: 4,
            fallbacks: 1,
            bytes: 3 * 1024 * 1024,
            active: 2,
        };
        let line = totals_line(stats);
        for expected in [
            "4 reads",
            "3.00 MiB",
            "attempts: 5",
            "fallback: 1",
            "active: 2",
        ] {
            assert!(line.contains(expected), "{line} is missing {expected}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn a_host_without_direct_storage_reports_zeroed_counters() {
        let stats = stats();
        assert_eq!(stats.attempts, 0);
        assert_eq!(stats.successes, 0);
        assert!(state_line(stats).contains("Windows feature"));
    }
}
