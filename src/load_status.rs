//! One counter per content category, so a loader that drops something says so.
//!
//! The failure this exists to prevent is narrow and real: a reader returns an
//! empty list, nothing is printed, and the map draws nothing. That is
//! indistinguishable from "the client authored nothing", and the two have very
//! different repairs. `docs/silent-drops.md` enumerates every place a value can
//! be lost; this module is where a lost value is *counted* so the audit has
//! something to assert on.
//!
//! Design rules, each of which is a rule because the opposite produced a bug:
//!
//! * **A category counter is incremented on the loss, not on the success.** A
//!   loader that reads 59 of 59 tree types increments nothing; one that skips
//!   a record increments `Trees` and records the reason.
//! * **A reason is recorded with the count.** A bare number cannot be acted on;
//!   "water: 3 skipped" is worse than "water: record 6 of 8.dat: unsupported
//!   version 0x1af".
//! * **`dropped()` is the only way to ask, and it returns the reasons.** A
//!   caller cannot check the count and forget the reason.
//!
//! The counters are process-global and monotonic, which is what the survey
//! binaries want: they read the totals once at the end. They are not reset
//! between clients in one process unless `reset()` is called, which the matrix
//! tool does per client.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// The content categories, named once.
///
/// These are the categories `docs/silent-drops.md` classifies against and the
/// ones the compatibility matrix's columns map onto. Adding one here without
/// adding it to the audit is itself a defect: the audit's own test reads this
/// list, so the two cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Water,
    Trees,
    Effects,
    Music,
    Sounds,
    Textures,
    ZoneNames,
    Instances,
    Packs,
    Elements,
    Tasks,
    Terrain,
}

impl Category {
    /// Every category, in the order the matrix prints them.
    pub const ALL: [Category; 12] = [
        Category::Water,
        Category::Trees,
        Category::Effects,
        Category::Music,
        Category::Sounds,
        Category::Textures,
        Category::ZoneNames,
        Category::Instances,
        Category::Packs,
        Category::Elements,
        Category::Tasks,
        Category::Terrain,
    ];

    pub fn index(self) -> usize {
        match self {
            Category::Water => 0,
            Category::Trees => 1,
            Category::Effects => 2,
            Category::Music => 3,
            Category::Sounds => 4,
            Category::Textures => 5,
            Category::ZoneNames => 6,
            Category::Instances => 7,
            Category::Packs => 8,
            Category::Elements => 9,
            Category::Tasks => 10,
            Category::Terrain => 11,
        }
    }

    /// The spelling used in `docs/silent-drops.md` and in the JSON, so a
    /// category has exactly one name everywhere.
    pub fn key(self) -> &'static str {
        match self {
            Category::Water => "water",
            Category::Trees => "trees",
            Category::Effects => "effects",
            Category::Music => "music",
            Category::Sounds => "sounds",
            Category::Textures => "textures",
            Category::ZoneNames => "zone names",
            Category::Instances => "instances",
            Category::Packs => "packs",
            Category::Elements => "elements",
            Category::Tasks => "tasks",
            Category::Terrain => "terrain",
        }
    }
}

static COUNTS: [AtomicUsize; 12] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// The recorded reason strings, capped so a broken archive cannot exhaust
/// memory. The cap is deliberately generous: the point is that the *first*
/// reasons are always kept, because a first reason is what identifies a bug.
const MAX_REASONS: usize = 512;

static REASONS: Mutex<Vec<(Category, String)>> = Mutex::new(Vec::new());

/// Record that one item in `category` was lost, and why.
///
/// Returns `true` on the first loss of this category in this process, so a
/// caller can log loudly once without spamming a per-record loop.
pub fn dropped(category: Category, reason: impl Into<String>) -> bool {
    let previous = COUNTS[category.index()].fetch_add(1, Ordering::Relaxed);
    if let Ok(mut list) = REASONS.lock() {
        if list.len() < MAX_REASONS {
            list.push((category, reason.into()));
        }
    }
    previous == 0
}

/// How many items of `category` have been lost.
pub fn dropped_count(category: Category) -> usize {
    COUNTS[category.index()].load(Ordering::Relaxed)
}

/// Every reason recorded for `category`, in the order it happened.
pub fn reasons(category: Category) -> Vec<String> {
    REASONS
        .lock()
        .map(|list| {
            list.iter()
                .filter(|(c, _)| *c == category)
                .map(|(_, r)| r.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Every category that lost at least one item, with its count and first reason.
///
/// This is what a survey prints and what the matrix tool writes into its JSON:
/// a category that is absent from this list lost nothing.
pub fn summary() -> Vec<(Category, usize, String)> {
    Category::ALL
        .into_iter()
        .filter(|c| dropped_count(*c) > 0)
        .map(|c| {
            let count = dropped_count(c);
            let first = reasons(c)
                .into_iter()
                .next()
                .unwrap_or_else(|| "no reason recorded".into());
            (c, count, first)
        })
        .collect()
}

/// Clear every counter and reason. For a survey that measures several clients in
/// one process: without this, client B's report would inherit client A's losses.
pub fn reset() {
    for c in Category::ALL {
        COUNTS[c.index()].store(0, Ordering::Relaxed);
    }
    if let Ok(mut list) = REASONS.lock() {
        list.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests in this module: the counters are process-global, so
    /// two tests running at once would read each other's numbers.
    static LOCK: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_loss_is_counted_and_its_reason_is_kept() {
        let _g = guard();
        reset();
        assert_eq!(dropped_count(Category::Water), 0);
        assert!(summary().is_empty(), "nothing lost means an empty summary");

        assert!(dropped(Category::Water, "record 6 of 8.dat: version 0x1af"));
        assert!(!dropped(Category::Water, "record 7 of 8.dat: version 0x1af"));
        assert_eq!(dropped_count(Category::Water), 2);
        assert_eq!(
            reasons(Category::Water),
            vec![
                "record 6 of 8.dat: version 0x1af".to_string(),
                "record 7 of 8.dat: version 0x1af".to_string()
            ]
        );
        let s = summary();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, Category::Water);
        assert_eq!(s[0].1, 2);
        assert_eq!(s[0].2, "record 6 of 8.dat: version 0x1af");
        reset();
    }

    /// A category that lost nothing must not appear, and reset must clear both
    /// the count and the reason: a survey that carried a stale reason into the
    /// next client would attribute one client's bug to another.
    #[test]
    fn reset_clears_counts_and_reasons() {
        let _g = guard();
        reset();
        dropped(Category::Trees, "one");
        reset();
        assert_eq!(dropped_count(Category::Trees), 0);
        assert!(reasons(Category::Trees).is_empty());
        assert!(summary().is_empty());
    }

    /// Every category has a distinct index and a distinct key.
    ///
    /// The index is used as an array subscript, so a duplicate would silently
    /// merge two categories into one counter -- and the audit would then report
    /// a clean water row while trees were losing records.
    #[test]
    fn every_category_has_its_own_slot_and_name() {
        let _g = guard();
        let mut seen = std::collections::BTreeSet::new();
        for c in Category::ALL {
            assert!(seen.insert(c.index()), "{c:?} shares an index");
            assert!(c.index() < 12);
        }
        let mut keys = std::collections::BTreeSet::new();
        for c in Category::ALL {
            assert!(keys.insert(c.key()), "{c:?} shares a key");
        }
        assert_eq!(seen.len(), 12);
        assert_eq!(keys.len(), 12);
        assert_eq!(Category::ALL.len(), 12);
    }

    /// The reason list is capped, but the count keeps climbing.
    ///
    /// A broken archive can name the same problem a million times; the count
    /// must stay true while memory stays bounded.
    #[test]
    fn the_reason_list_is_capped_but_the_count_is_not() {
        let _g = guard();
        reset();
        for i in 0..(MAX_REASONS + 25) {
            dropped(Category::Packs, format!("record {i}"));
        }
        assert_eq!(dropped_count(Category::Packs), MAX_REASONS + 25);
        assert_eq!(reasons(Category::Packs).len(), MAX_REASONS);
        // The earliest reasons survive, because the first one identifies the bug.
        assert_eq!(reasons(Category::Packs)[0], "record 0");
        reset();
    }
}
