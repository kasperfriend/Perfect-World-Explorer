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

/// How many reason strings one category may record, so a broken archive cannot
/// exhaust memory. The cap is deliberately generous, and it is **per category**:
/// the point is that the *first* reasons are always kept, because a first reason
/// is what identifies a bug. A single shared cap does not do that -- a category
/// that loses a million records evicts every other category's first reason,
/// which is exactly the reason a reader would have wanted.
const MAX_REASONS: usize = 512;

static REASONS: Mutex<Vec<(Category, String)>> = Mutex::new(Vec::new());

/// Record that one item in `category` was lost, and why.
///
/// Returns `true` on the first loss of this category in this process, so a
/// caller can log loudly once without spamming a per-record loop.
pub fn dropped(category: Category, reason: impl Into<String>) -> bool {
    let previous = COUNTS[category.index()].fetch_add(1, Ordering::Relaxed);
    if let Ok(mut list) = REASONS.lock() {
        let held = list.iter().filter(|(c, _)| *c == category).count();
        if held < MAX_REASONS {
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

/// Serialises every test that touches the process-global counters and reasons.
///
/// `#[cfg(test)]` because it exists only to keep tests from reading each
/// other's numbers. It is one lock for the whole binary rather than one per
/// module: the state it guards is global, so a module-local lock would only
/// serialise tests inside that module and leave every caller elsewhere racing.
/// That race is not hypothetical -- `tree_source`'s end-to-end extraction test
/// resets the counters and then asserts a count, and a water test running at the
/// same time on another thread contributed 256 drops to its 537.
#[cfg(test)]
pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        test_guard()
    }

    #[test]
    fn a_loss_is_counted_and_its_reason_is_kept() {
        let _g = guard();
        reset();
        assert_eq!(dropped_count(Category::Water), 0);
        assert!(summary().is_empty(), "nothing lost means an empty summary");

        assert!(dropped(Category::Water, "record 6 of 8.dat: version 0x1af"));
        assert!(!dropped(
            Category::Water,
            "record 7 of 8.dat: version 0x1af"
        ));
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

/// A loader returned no items and counted no loss: the defect this module exists
/// to catch.
///
/// The rule is one sentence: **an empty result and a zero counter cannot both be
/// true.** If a loader hands back nothing, either the client genuinely authored
/// nothing — in which case the caller knows because the input said so — or
/// something was lost, in which case `dropped` must have been called. This
/// function is the assertion, so every loader test can make the same claim
/// instead of each inventing its own.
///
/// `authored` is the caller's own evidence that the input named nothing: the
/// count in the file's header, or `None` when the caller has no such evidence.
/// Passing `Some(0)` says "the input declared zero items", which is the only
/// clean way to return empty.
pub fn assert_not_silently_empty(
    category: Category,
    returned: usize,
    authored: Option<usize>,
) -> Result<(), String> {
    if returned > 0 {
        return Ok(());
    }
    if dropped_count(category) > 0 {
        // Empty is honest here: something was lost and the caller can read why.
        return Ok(());
    }
    if authored == Some(0) {
        // Empty is honest here too: the input itself declared nothing.
        return Ok(());
    }
    Err(format!(
        "{} returned 0 items, recorded no reason, and the input did not declare \
         zero authored items{}: a loader may not be empty and silent at once. \
         Either call load_status::dropped with the reason, or pass the count the \
         file declares as `authored`. The audit list is docs/silent-drops.md.",
        category.key(),
        authored.map_or(String::new(), |a| format!(" (it declares {a})"))
    ))
}

#[cfg(test)]
mod empty_tests {
    use super::*;

    #[test]
    fn empty_and_silent_is_refused() {
        let _g = test_guard();
        reset();
        let err = assert_not_silently_empty(Category::Water, 0, None).unwrap_err();
        // The category name has to be in the message: it is what tells a reader
        // which counter to look at.
        assert!(err.contains("water"), "{err}");
        // And the message has to say what to do about it, because the whole
        // point of this module is that a silent empty result is a bug someone
        // has to go and fix.
        assert!(err.contains("docs/silent-drops.md"), "{err}");
        assert!(err.contains("load_status::dropped"), "{err}");
    }

    #[test]
    fn empty_with_a_recorded_reason_is_allowed() {
        let _g = test_guard();
        reset();
        dropped(
            Category::Water,
            "record 6 of 8.dat: unsupported version 0x1af",
        );
        assert!(assert_not_silently_empty(Category::Water, 0, Some(8)).is_ok());
        reset();
    }

    #[test]
    fn empty_because_the_input_declares_zero_is_allowed() {
        let _g = test_guard();
        reset();
        // a91 and a64 are this case: their ECWLD declares zero tree types, so a
        // tree loader may legitimately return nothing for them.
        assert!(assert_not_silently_empty(Category::Trees, 0, Some(0)).is_ok());
        // But an input that declares 59 and yields 0 is the defect.
        let err = assert_not_silently_empty(Category::Trees, 0, Some(59)).unwrap_err();
        assert!(err.contains("declares 59"), "{err}");
    }

    #[test]
    fn a_non_empty_result_is_always_fine() {
        let _g = test_guard();
        reset();
        assert!(assert_not_silently_empty(Category::Trees, 3649, None).is_ok());
        assert!(assert_not_silently_empty(Category::Trees, 1, Some(3679)).is_ok());
    }
}
