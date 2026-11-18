//! A pre-order, stackless BVH. Every node's escape points just beyond its
//! subtree, so neither deep trees nor divergence require a shader stack.
use glam::Vec3;

#[derive(Clone, Copy, Debug)]
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
