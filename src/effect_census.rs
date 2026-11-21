//! The activation census behind the World tab's effect line.
//!
//! Every effect root the world places passes three gates before it plays: it has
//! to be near the camera, inside the resident terrain window while the cutoff is
//! on, and inside its own authored day/night window. The status line used to
//! print three counters, one per gate, and each was incremented independently —
//! so a root beyond the radius *and* outside its day/night window was counted
//! twice, and the numbers could never be reconciled against the number of roots.
//! The line exists to answer "where did the rest of my effects go?", and three
//! overlapping tallies cannot answer it.
//!
//! This module is that decision, on its own, so it can be tested without a GPU:
//! `world_models_gpu` owns the producers and the per-frame work, and asks here
//! which bucket a root belongs in. The buckets are disjoint, first refusal wins,
//! and the four counts sum to the producer total.

/// Which bucket of the activation census one root falls in.
///
/// Order matches the status line: playing, beyond the radius, outside the
/// resident window, outside the day/night window. The first failing gate decides,
/// which is what makes the buckets disjoint.
pub fn bucket(near: bool, resident: bool, visible: bool) -> usize {
    if !near {
        1
    } else if !resident {
        2
    } else if !visible {
        3
    } else {
        0
    }
}

/// Add one root's gates to a running census.
pub fn record(census: &mut [usize; 4], near: bool, resident: bool, visible: bool) {
    census[bucket(near, resident, visible)] += 1;
}

/// Whether the gates let a root play. True exactly for bucket 0, so the counts
/// and the playing set cannot disagree.
pub fn plays(near: bool, resident: bool, visible: bool) -> bool {
    near && resident && visible
}

/// The four bucket names, in the order the status line prints them.
pub const LABELS: [&str; 4] = [
    "playing",
    "beyond the radius",
    "outside the resident window",
    "off by the day/night window",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of the three gates lands in exactly one bucket, and the
    /// buckets sum to the number of roots. This is the invariant the status line
    /// promises; it is the thing the old three-counter version broke.
    #[test]
    fn the_buckets_partition_every_combination() {
        let mut census = [0usize; 4];
        for bits in 0u8..8 {
            let (near, resident, visible) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            record(&mut census, near, resident, visible);
        }
        assert_eq!(
            census.iter().sum::<usize>(),
            8,
            "every case lands somewhere"
        );
        assert_eq!(census[0], 1, "only all three gates open is playing");
        assert_eq!(census[1], 4, "half the cases are beyond the radius");
    }

    /// `plays` and bucket 0 are the same predicate, so the playing count and the
    /// drawn set cannot drift apart.
    #[test]
    fn playing_is_exactly_the_first_bucket() {
        for bits in 0u8..8 {
            let (near, resident, visible) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            assert_eq!(
                plays(near, resident, visible),
                bucket(near, resident, visible) == 0,
                "bits {bits}"
            );
        }
    }

    /// First refusal wins, in the printed order: a root failing two gates is
    /// counted under the first one, never under both.
    #[test]
    fn the_first_failing_gate_names_the_bucket() {
        assert_eq!(bucket(false, true, true), 1, "radius first");
        assert_eq!(
            bucket(false, false, false),
            1,
            "and it wins over both others"
        );
        assert_eq!(bucket(true, false, false), 2, "window before the clock");
        assert_eq!(bucket(true, true, false), 3, "the clock is last");
        assert_eq!(bucket(true, true, true), 0, "nothing refused");
    }

    #[test]
    fn every_bucket_has_a_label() {
        assert_eq!(LABELS.len(), 4);
        assert!(LABELS.iter().all(|label| !label.is_empty()));
    }
}
