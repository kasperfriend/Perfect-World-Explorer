//! A pre-order, stackless BVH. Every node's escape points just beyond its
//! subtree, so neither deep trees nor divergence require a shader stack.
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
}

impl Bvh {
    pub fn build(&mut self, items: &mut [Item]) -> (u32, u32) {
        let first = self.nodes.len() as u32;
        if !items.is_empty() {
            self.split(items);
        }
        (first, self.nodes.len() as u32)
    }

    /// Count the nodes of the same balanced tree without sorting primitives.
    pub fn node_count(items: usize) -> usize {
        if items == 0 {
            return 0;
        }
        let min_leaves = items.div_ceil(LEAF_SIZE);
        let base = 1usize << min_leaves.ilog2();
        let splits = items.saturating_sub(base * LEAF_SIZE).min(base);
        (base + splits) * 2 - 1
    }

    /// Refit all bounds without changing a leaf reference, split or escape.
    /// Children come after their parent, so one reverse pass is sufficient.
    pub fn refit(&mut self, bounds: &[Bounds]) {
        fn of(n: [u32; NODE_WORDS as usize]) -> Bounds {
            Bounds {
                min: Vec3::from_array([n[0], n[1], n[2]].map(f32::from_bits)),
                max: Vec3::from_array([n[3], n[4], n[5]].map(f32::from_bits)),
            }
        }
        for i in (0..self.nodes.len()).rev() {
            let [_, _, _, _, _, _, _, first, count] = self.nodes[i];
            let next = if count == 0 {
                let left = self.nodes[i + 1];
                of(left).union(of(self.nodes[left[6] as usize]))
            } else {
                self.items[first as usize..(first + count) as usize]
                    .iter()
                    .fold(Bounds::EMPTY, |b, &j| b.union(bounds[j as usize]))
            };
            let words = next.min.to_array().into_iter().chain(next.max.to_array());
            for (out, value) in self.nodes[i][..6].iter_mut().zip(words) {
                *out = value.to_bits();
            }
        }
    }

    /// Copy a cached standalone tree into a packet, relocating its offsets.
    pub fn append(&mut self, other: &Self, remap: impl Fn(u32) -> u32) -> (u32, u32) {
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
        let mut bounds = Bounds::EMPTY;
        let mut centers = Bounds::EMPTY;
        for item in items.iter() {
            bounds = bounds.union(item.bounds);
            centers = centers.union(Bounds::point(item.bounds.center()));
        }
        let node = self.nodes.len();
        self.nodes.push([0; NODE_WORDS as usize]);
        let mut begin = 0;
        let mut count = 0;
        if items.len() <= LEAF_SIZE {
            begin = self.items.len() as u32;
            count = items.len() as u32;
            self.items.extend(items.iter().map(|item| item.id));
        } else {
            let d = centers.max - centers.min;
            let axis = if d.x >= d.y && d.x >= d.z {
                0
            } else if d.y >= d.z {
                1
            } else {
                2
            };
            let mid = items.len() / 2;
            items.select_nth_unstable_by(mid, |a, b| {
                a.bounds.center()[axis].total_cmp(&b.bounds.center()[axis])
            });
            let (a, b) = items.split_at_mut(mid);
            self.split(a);
            self.split(b);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refit_keeps_escape_and_leaf_topology_and_relocates_payloads() {
        let positions: Vec<_> = (0..257)
            .map(|i| Bounds::point(Vec3::X * i as f32))
            .collect();
        for n in 0..positions.len() {
            assert_eq!(
                Bvh::from_bounds(&positions[..n]).nodes.len(),
                Bvh::node_count(n)
            );
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
