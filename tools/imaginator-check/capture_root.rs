//! Crate root for the offline capture check: `src/imaginator/capture.rs`
//! against stand-ins for the engine types it reads.
//!
//! Every stand-in here mirrors the real type's *fields that `capture.rs` uses*,
//! taken from the real files. The point is to compile and test the capture
//! arithmetic — stride, normals, budget, radius, material ordering — without the
//! image/bmd/pck dependency closure the real types drag in.
//!
//! `tools/imaginator-check.sh capture` builds and tests this.

#![allow(dead_code, unused_imports)]

#[path = "../../src/imaginator/math.rs"]
pub mod math;

#[path = "../../src/imaginator/bvh.rs"]
pub mod bvh;

pub mod navigation {
    pub struct Heights {
        pub data: Vec<f32>,
    }
    impl Heights {
        pub fn at(&self, index: usize) -> f32 {
            self.data.get(index).copied().unwrap_or(0.0)
        }
    }
    /// Real: `src/navigation.rs:67`.
    pub struct MapSpace {
        pub width: usize,
        pub height: usize,
        pub spacing: f32,
        pub heights: Heights,
    }
}

pub mod water {
    /// Real: `src/water.rs:16`.
    pub struct WaterArea {
        pub export_id: u32,
        pub color: u32,
        pub simple: bool,
        pub height: f32,
        pub grid_size: f32,
        pub columns: usize,
        pub rows: usize,
        pub center: [f32; 3],
        pub mask: Vec<u8>,
    }
}

pub mod bmd {
    /// Real: `src/bmd.rs:4`.
    pub struct Vertex {
        pub position: [f32; 3],
        pub uv: [f32; 2],
        pub normal: [f32; 3],
    }
    pub struct Mesh {
        pub vertices: Vec<Vertex>,
        pub indices: Vec<u16>,
    }
    /// Real: `src/bmd.rs:30`.
    pub struct Model {
        pub scale: [f32; 3],
        pub direction: [f32; 3],
        pub up: [f32; 3],
        pub position: [f32; 3],
        pub meshes: Vec<Mesh>,
    }
}

pub mod scene {
    /// Real: `src/scene.rs:399`.
    pub struct Ornament {
        pub export_id: u32,
        pub offset: u32,
        pub position: [f32; 3],
        pub direction: [f32; 3],
        pub up: [f32; 3],
        pub extra: u32,
        pub path: String,
    }
    pub struct Scene;
    /// Real: `src/scene.rs:22`.
    pub struct Tree {
        pub tree_type: u32,
        pub position: [f32; 3],
    }
}

pub mod objects {
    use crate::bmd;
    pub struct Objects;
    impl Objects {
        pub fn get(&self, _idx: usize) -> Option<&bmd::Model> {
            None
        }
    }
}

pub mod trees {
    pub struct Vert {
        pub pos: [f32; 3],
        pub normal: [f32; 3],
        pub uv: [f32; 2],
    }
    pub struct Part {
        pub vertices: Vec<Vert>,
        pub indices: Vec<u32>,
    }
    /// Real: `src/trees.rs:20`.
    pub struct Leaf {
        pub center: [f32; 3],
        pub normal: Option<[f32; 3]>,
        pub rho: f32,
        pub corners: [[f32; 3]; 4],
        pub uv: [[f32; 2]; 4],
        pub cluster: u32,
    }
    /// Real: `src/trees.rs:30`.
    pub struct TreeLod {
        pub branch: Part,
        pub frond: Part,
        pub branch_alpha_ref: Option<f32>,
        pub frond_alpha_ref: Option<f32>,
        pub leaves: Vec<Leaf>,
        pub rocking: Vec<[f32; 3]>,
        pub rocking_clusters: usize,
    }
    /// Real: `src/trees.rs:42`.
    pub struct TreeMesh {
        pub lods: Vec<TreeLod>,
    }
    /// Real: `src/trees.rs:51`.
    pub struct Trees {
        pub types: Vec<Option<TreeMesh>>,
        pub placements: Vec<crate::scene::Tree>,
    }
}

/// `capture.rs` says `super::bvh` and `super::math`, which is `crate` here, so
/// it is mounted at the root next to them.
#[path = "../../src/imaginator/capture.rs"]
pub mod capture;
