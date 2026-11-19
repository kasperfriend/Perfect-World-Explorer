//! A pre-order, stackless BVH. Every node's escape points just beyond its
//! subtree, so neither deep trees nor divergence require a shader stack.
//! Branches have 2–4 children laid out in sequence; no shader ABI change.
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub const EMPTY: Self = Self {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    pub fn point(p: Vec3) -> Self {
        Self { min: p, max: p }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn area(self) -> f32 {
        let e = (self.max - self.min).max(Vec3::ZERO);
        e.x * e.y + e.y * e.z + e.x * e.z
    }

    fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
}

#[derive(Clone, Copy)]
pub struct Item {
    pub id: u32,
    pub bounds: Bounds,
}

pub const NODE_WORDS: u32 = 9;
const LEAF_SIZE: usize = 4;

#[derive(Default)]
pub struct Bvh {
    // min.xyz, max.xyz, escape node index, first leaf item, count (0: branch).
    pub nodes: Vec<[u32; NODE_WORDS as usize]>,
    pub items: Vec<u32>,
    refit: Option<RefitMap>,
}

#[derive(Default)]
struct RefitMap {
    parent: Vec<u32>,
    leaf: Vec<u32>,
    dirty: Vec<u32>,
    marked: Vec<bool>,
}
const NONE: u32 = u32::MAX;
const BINS: usize = 12;

impl Bvh {
    pub fn build(&mut self, items: &mut [Item]) -> (u32, u32) {
        self.refit = None;
        let first = self.nodes.len() as u32;
        if !items.is_empty() {
            self.split(items);
        }
        (first, self.nodes.len() as u32)
    }

    /// Conservative binary upper bound; our wider layout has the same
    /// maximum leaf count and can use fewer interior nodes.
    pub fn node_bound(items: usize) -> usize {
        (2 * items.div_ceil(LEAF_SIZE)).saturating_sub(1)
    }

    fn refit_node(&mut self, i: usize, bounds: &[Bounds]) {
        let [_, _, _, _, _, _, escape, first, count] = self.nodes[i];
        let mut value = Bounds::EMPTY;
        if count == 0 {
            let mut child = i + 1;
            while child < escape as usize {
                let n = self.nodes[child];
                value = value.union(Bounds {
                    min: Vec3::from_array([n[0], n[1], n[2]].map(f32::from_bits)),
                    max: Vec3::from_array([n[3], n[4], n[5]].map(f32::from_bits)),
                });
                child = n[6] as usize;
            }
        } else {
            for &j in &self.items[first as usize..(first + count) as usize] {
                value = value.union(bounds[j as usize]);
            }
        }
        let words = value.min.to_array().into_iter().chain(value.max.to_array());
        for (out, value) in self.nodes[i][..6].iter_mut().zip(words) {
            *out = value.to_bits();
        }
    }

    fn refit(&mut self, bounds: &[Bounds]) {
        for i in (0..self.nodes.len()).rev() {
            self.refit_node(i, bounds);
        }
    }

    /// Cached, standalone trees only. Mark just the changed leaves and their
    /// ancestors. Recurse in reverse preorder so every marked child is ready
    /// before its parent. Switch to the linear refit for broad deformation.
    pub fn refit_changed(&mut self, before: &[Bounds], after: &[Bounds]) -> usize {
        debug_assert_eq!(before.len(), after.len());
        let mut map = self.refit.take().unwrap_or_else(|| {
            let mut parent = vec![NONE; self.nodes.len()];
            let mut leaf = vec![NONE; after.len()];
            for (i, n) in self.nodes.iter().enumerate() {
                if n[8] == 0 {
                    let mut child = i + 1;
                    while child < n[6] as usize {
                        parent[child] = i as u32;
                        child = self.nodes[child][6] as usize;
                    }
                } else {
                    for &item in &self.items[n[7] as usize..(n[7] + n[8]) as usize] {
                        leaf[item as usize] = i as u32;
                    }
                }
            }
            RefitMap {
                parent,
                leaf,
                marked: vec![false; self.nodes.len()],
                ..Default::default()
            }
        });
        let mut full = false;
        for (i, (a, b)) in before.iter().zip(after).enumerate() {
            if a == b {
                continue;
            }
            let mut at = map.leaf[i];
            while at != NONE && !map.marked[at as usize] {
                map.marked[at as usize] = true;
                map.dirty.push(at);
                at = map.parent[at as usize];
            }
            if map.dirty.len() > self.nodes.len() / 3 {
                full = true;
                break;
            }
        }
        let touched = if full {
            self.refit(after);
            self.nodes.len()
        } else {
            map.dirty.sort_unstable_by(|a, b| b.cmp(a));
            for &i in &map.dirty {
                self.refit_node(i as usize, after);
            }
            map.dirty.len()
        };
        for i in map.dirty.drain(..) {
            map.marked[i as usize] = false;
        }
        self.refit = Some(map);
        touched
    }

    /// Copy a cached standalone tree into a packet, relocating its offsets.
    pub fn append(&mut self, other: &Self, remap: impl Fn(u32) -> u32) -> (u32, u32) {
        self.refit = None;
        let first = self.nodes.len() as u32;
        let item = self.items.len() as u32;
        self.nodes.extend(other.nodes.iter().map(|n| {
            let mut n = *n;
            n[6] += first;
            if n[8] != 0 {
                n[7] += item;
            }
            n
        }));
        self.items.extend(other.items.iter().copied().map(remap));
        (first, self.nodes.len() as u32)
    }

    pub fn from_bounds(bounds: &[Bounds]) -> Self {
        let mut items: Vec<_> = bounds
            .iter()
            .enumerate()
            .map(|(i, &bounds)| Item {
                id: i as u32,
                bounds,
            })
            .collect();
        let mut tree = Self::default();
        tree.build(&mut items);
        tree
    }

    fn split(&mut self, items: &mut [Item]) {
        let bounds = items
            .iter()
            .fold(Bounds::EMPTY, |b, item| b.union(item.bounds));
        let node = self.nodes.len();
        self.nodes.push([0; NODE_WORDS as usize]);
        let mut begin = 0;
        let mut count = 0;
        if items.len() <= LEAF_SIZE {
            begin = self.items.len() as u32;
            count = items.len() as u32;
            self.items.extend(items.iter().map(|item| item.id));
        } else {
            let mid = partition(items);
            let (a, b) = items.split_at_mut(mid);
            for group in [a, b] {
                if group.len() > LEAF_SIZE {
                    let mid = partition(group);
                    let (a, b) = group.split_at_mut(mid);
                    self.split(a);
                    self.split(b);
                } else {
                    self.split(group);
                }
            }
        }
        let [x0, y0, z0] = bounds.min.to_array().map(f32::to_bits);
        let [x1, y1, z1] = bounds.max.to_array().map(f32::to_bits);
        self.nodes[node] = [
            x0,
            y0,
            z0,
            x1,
            y1,
            z1,
            self.nodes.len() as u32,
            begin,
            count,
        ];
    }
}

// Binned surface-area heuristic over all three centroid axes. The selected
// rank rounds to a 4-primitive boundary, keeping exact leaf storage bounds.
// Guard against elongated single-child chains and degenerate centroids.
fn partition(items: &mut [Item]) -> usize {
    let centers = items.iter().fold(Bounds::EMPTY, |b, i| {
        b.union(Bounds::point(i.bounds.center()))
    });
    let d = centers.max - centers.min;
    let mut axis = if d.x >= d.y && d.x >= d.z {
        0
    } else if d.y >= d.z {
        1
    } else {
        2
    };
    let mut rank = items.len().div_ceil(2);
    if items.len() >= 2 * BINS {
        let mut best = f32::INFINITY;
        let min_side = items.len() / 8;
        for dim in 0..3 {
            if d[dim] <= 1e-8 {
                continue;
            }
            let mut bins = [(0usize, Bounds::EMPTY); BINS];
            let scale = BINS as f32 / d[dim];
            for item in items.iter() {
                let bin = (((item.bounds.center()[dim] - centers.min[dim]) * scale) as usize)
                    .min(BINS - 1);
                bins[bin].0 += 1;
                bins[bin].1 = bins[bin].1.union(item.bounds);
            }
            let mut tail = [(0usize, 0.); BINS];
            let mut right = Bounds::EMPTY;
            let mut r = 0;
            for i in (1..BINS).rev() {
                r += bins[i].0;
                right = right.union(bins[i].1);
                tail[i] = (r, right.area() * r as f32);
            }
            let mut left = Bounds::EMPTY;
            let mut l = 0;
            for i in 0..BINS - 1 {
                l += bins[i].0;
                left = left.union(bins[i].1);
                if l < min_side || tail[i + 1].0 < min_side {
                    continue;
                }
                let cost = left.area() * l as f32 + tail[i + 1].1;
                if cost < best {
                    best = cost;
                    axis = dim;
                    rank = l;
                }
            }
        }
    }
    let mid = ((rank + LEAF_SIZE / 2) / LEAF_SIZE).clamp(1, items.len().div_ceil(LEAF_SIZE) - 1)
        * LEAF_SIZE;
    items.select_nth_unstable_by(mid, |a, b| {
        a.bounds.center()[axis]
            .total_cmp(&b.bounds.center()[axis])
            .then_with(|| a.id.cmp(&b.id))
    });
    mid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refit_keeps_escape_and_leaf_topology_and_relocates_payloads() {
        let positions: Vec<_> = (0..257)
            .map(|i| Bounds::point(Vec3::X * i as f32))
            .collect();
        for n in 0..positions.len() {
            assert!(Bvh::from_bounds(&positions[..n]).nodes.len() <= Bvh::node_bound(n));
        }
        let mut tree = Bvh::from_bounds(&positions);
        let old = tree.nodes.clone();
        let refs = tree.items.clone();
        let moved: Vec<_> = positions
            .iter()
            .rev()
            .map(|b| Bounds::point(b.min + Vec3::Y))
            .collect();
        tree.refit(&moved);
        assert_eq!(refs, tree.items);
        for (old, new) in old.iter().zip(&tree.nodes) {
            assert_eq!(&old[6..], &new[6..]);
        }
        let mut packed = Bvh::default();
        let (_, first_end) = packed.append(&tree, |i| i);
        let (root, end) = packed.append(&tree, |i| i + 2000);
        assert_eq!(root, first_end);
        assert_eq!(packed.nodes[root as usize][6], end);
        assert_eq!(packed.nodes[root as usize][1], 1.0f32.to_bits());
        assert_eq!(packed.nodes[root as usize][3], 256.0f32.to_bits());
        assert_eq!(
            &packed.items[refs.len()..],
            &refs.iter().map(|i| i + 2000).collect::<Vec<_>>()
        );
    }

    #[test]
    fn four_children_and_sparse_refit_match_the_full_bounds_oracle() {
        let rest: Vec<_> = (0..1024)
            .map(|i| {
                let p = Vec3::new((i % 83) as f32, (i % 17) as f32, i as f32 * 0.125);
                Bounds {
                    min: p,
                    max: p + Vec3::splat(0.5),
                }
            })
            .collect();
        let mut sparse = Bvh::from_bounds(&rest);
        let mut dense = Bvh::from_bounds(&rest);
        let mut child = 1usize;
        let mut children = 0;
        while child < sparse.nodes.len() {
            children += 1;
            child = sparse.nodes[child][6] as usize;
        }
        assert_eq!(children, 4);
        let mut moved = rest.clone();
        moved[377] = Bounds::point(Vec3::splat(-100.));
        moved[14] = Bounds::point(Vec3::splat(500.));
        let touched = sparse.refit_changed(&rest, &moved);
        dense.refit(&moved);
        assert_eq!(dense.nodes, sparse.nodes);
        assert!(
            touched < 12,
            "touched {touched} out of {} nodes",
            sparse.nodes.len()
        );
        assert_eq!(sparse.refit_changed(&moved, &moved), 0);
    }

    #[test]
    fn escape_ranges_and_item_addresses_survive_appending_another_tree() {
        let mut bvh = Bvh::default();
        let mut items: Vec<_> = (0..101)
            .map(|i| Item {
                id: i,
                bounds: Bounds::point(Vec3::new(i as f32, 5., 0.)),
            })
            .collect();
        let (root, end) = bvh.build(&mut items);
        let (root2, end2) = bvh.build(&mut items[..3]);
        assert_eq!(root, 0);
        assert_eq!(root2, end);
        assert_eq!(bvh.nodes[root as usize][6], end);
        assert_eq!(bvh.nodes[root2 as usize][6], end2);
        let mut found = Vec::new();
        for (i, node) in bvh.nodes[..end as usize].iter().enumerate() {
            assert!((i as u32 + 1..=end).contains(&node[6]));
            if node[8] > 0 {
                found.extend_from_slice(&bvh.items[node[7] as usize..(node[7] + node[8]) as usize]);
            }
        }
        found.sort_unstable();
        assert_eq!(found, (0..101).collect::<Vec<_>>());
    }
}
