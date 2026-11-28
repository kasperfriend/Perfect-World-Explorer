//! Native scene block admission order (0x41c5b0 / 0x41c900 / 0x41cb20).
//! Initial/teleported windows load synchronously in row-major order. Overlap
//! appends newly exposed blocks to a persistent FIFO; each frame consumes a
//! bounded number of ATTEMPTS, including stale/duplicate candidates. The current
//! fixed budget is a compatibility approximation: native scene `+0x534` is a
//! dynamic fractional quota derived in `0x42ae60..0x42af64`.
use std::collections::VecDeque;
pub type Rect = (usize, usize, usize, usize); // x0,x1,y0,y1, exclusive ends
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Load(usize),
    Unload(usize),
}
pub struct Window {
    rows: usize,
    cols: usize,
    loaded: Vec<bool>,
    rect: Option<Rect>,
    pending: VecDeque<usize>,
    budget: usize,
}
fn contains(r: Rect, x: usize, y: usize) -> bool {
    x >= r.0 && x < r.1 && y >= r.2 && y < r.3
}
impl Window {
    pub fn new(rows: usize, cols: usize, budget: usize) -> Result<Self, String> {
        let count = rows
            .checked_mul(cols)
            .filter(|&n| n > 0)
            .ok_or("invalid block window dimensions")?;
        Ok(Self {
            rows,
            cols,
            loaded: vec![false; count],
            rect: None,
            pending: VecDeque::new(),
            budget,
        })
    }
    pub fn update(&mut self, rect: Rect) -> Result<Vec<Change>, String> {
        if rect.0 >= rect.1 || rect.1 > self.cols || rect.2 >= rect.3 || rect.3 > self.rows {
            return Err("invalid active block rectangle".into());
        }
        let mut changes = Vec::with_capacity(self.budget);
        if self.rect != Some(rect) {
            let old = self.rect;
            let overlap =
                old.is_some_and(|r| r.0 < rect.1 && rect.0 < r.1 && r.2 < rect.3 && rect.2 < r.3);
            if overlap {
                // Preserve null overlapping slots too: their original FIFO
                // entry, not immediate reload, remains responsible for them.
                let old = old.unwrap();
                for y in rect.2..rect.3 {
                    for x in rect.0..rect.1 {
                        if !contains(old, x, y) {
                            self.pending.push_back(y * self.cols + x);
                        }
                    }
                }
            }
            if let Some(old) = old {
                for y in old.2..old.3 {
                    for x in old.0..old.1 {
                        let index = y * self.cols + x;
                        if self.loaded[index] && (!overlap || !contains(rect, x, y)) {
                            self.loaded[index] = false;
                            changes.push(Change::Unload(index));
                        }
                    }
                }
            }
            self.rect = Some(rect);
            if !overlap {
                for y in rect.2..rect.3 {
                    for x in rect.0..rect.1 {
                        let index = y * self.cols + x;
                        self.loaded[index] = true;
                        changes.push(Change::Load(index));
                    }
                }
                // Native does not clear old candidates on a teleport.
            }
        }
        for _ in 0..self.budget.min(self.pending.len()) {
            let index = self.pending.pop_front().unwrap();
            if !contains(rect, index % self.cols, index / self.cols) || self.loaded[index] {
                continue;
            }
            self.loaded[index] = true;
            changes.push(Change::Load(index));
        }
        Ok(changes)
    }
}
#[cfg(test)]
mod tests {
    use super::{Change::*, *};
    #[test]
    fn initial_and_disjoint_windows_admit_every_block_row_major() {
        let mut w = Window::new(2, 8, 1).unwrap();
        assert_eq!(
            w.update((0, 2, 0, 2)).unwrap(),
            vec![Load(0), Load(1), Load(8), Load(9)]
        );
        assert_eq!(
            w.update((5, 7, 0, 2)).unwrap(),
            vec![
                Unload(0),
                Unload(1),
                Unload(8),
                Unload(9),
                Load(5),
                Load(6),
                Load(13),
                Load(14)
            ]
        );
    }
    #[test]
    fn overlap_admits_one_candidate_and_drains_while_stationary() {
        let mut w = Window::new(2, 4, 1).unwrap();
        w.update((0, 2, 0, 2)).unwrap();
        assert_eq!(
            w.update((1, 3, 0, 2)).unwrap(),
            vec![Unload(0), Unload(4), Load(2)]
        );
        assert_eq!(w.update((1, 3, 0, 2)).unwrap(), vec![Load(6)]);
        assert!(w.update((1, 3, 0, 2)).unwrap().is_empty());
    }
    #[test]
    fn fast_direction_changes_keep_fifo_and_spend_budget_on_stale_duplicates() {
        let mut w = Window::new(2, 5, 1).unwrap();
        w.update((0, 3, 0, 2)).unwrap();
        assert_eq!(
            w.update((1, 4, 0, 2)).unwrap(),
            vec![Unload(0), Unload(5), Load(3)]
        );
        // Queue 8 is now stale; it consumes this frame's attempt.
        assert_eq!(w.update((0, 3, 0, 2)).unwrap(), vec![Unload(3)]);
        assert!(w.update((1, 4, 0, 2)).unwrap().is_empty());
        assert_eq!(w.update((0, 3, 0, 2)).unwrap(), vec![Load(5)]);
        assert!(w.update((0, 3, 0, 2)).unwrap().is_empty());
        assert!(w.update((0, 3, 0, 2)).unwrap().is_empty());
        assert_eq!(w.update((0, 3, 0, 2)).unwrap(), vec![Load(0)]);
        assert!(w.update((0, 3, 0, 2)).unwrap().is_empty()); // duplicate 5
        assert!(w.pending.is_empty());
    }
    #[test]
    fn invalid_rectangle_is_atomic_and_zero_budget_preserves_pending() {
        assert!(Window::new(0, 3, 1).is_err());
        assert!(Window::new(usize::MAX, 2, 1).is_err());
        let mut w = Window::new(2, 4, 0).unwrap();
        w.update((0, 2, 0, 2)).unwrap();
        assert!(w.update((2, 1, 0, 2)).is_err());
        assert_eq!(w.rect, Some((0, 2, 0, 2)));
        assert_eq!(w.update((1, 3, 0, 2)).unwrap(), vec![Unload(0), Unload(4)]);
        assert_eq!(w.pending.iter().copied().collect::<Vec<_>>(), vec![2, 6]);
        assert!(w.update((1, 3, 0, 2)).unwrap().is_empty());
    }
}
