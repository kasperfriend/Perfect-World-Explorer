//! Crate root for `tools/imaginator-check.sh module`: the whole Imaginator.
//!
//! Everything real is mounted: `bvh`, `math`, `settings`, `tracer`, `gpu`,
//! `capture`, `composite`, `hardware`, and `mod.rs` on top of them. What is
//! shimmed is exactly what the module borrows from outside the crate — `wgpu`,
//! `egui` and `bytemuck` — plus the `capture`'s engine types, because those drag
//! in the image/bmd/pck closure.
//!
//! The point of this root is that `mod.rs` and `composite.rs` cannot rust through
//! a refactor without the check failing.

#![allow(dead_code, unused_imports, non_snake_case, non_camel_case_types)]

// The module's hardware half is DX12-only, so a non-Windows check never reaches
// for the wrapper. Mounting the crate's name anyway keeps `capture`'s seams
// honest if that ever changes.
#[cfg(windows)]
pub use d3d12_crate as d3d12;
pub mod wgpu {
    pub use ::wgpu::*;
}
pub mod egui {
    pub use ::egui::*;
}
pub mod bytemuck {
    pub use ::bytemuck::*;
}

// --- capture's engine seams ------------------------------------------------

pub mod navigation {
    pub struct Heights {
        pub data: Vec<f32>,
    }
    impl Heights {
        pub fn at(&self, index: usize) -> f32 {
            self.data.get(index).copied().unwrap_or(0.0)
        }
    }
    pub struct MapSpace {
        pub width: usize,
        pub height: usize,
        pub spacing: f32,
        pub heights: Heights,
    }
}

pub mod water {
    pub struct ShoreWave {
        pub phase: f32,
    }
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
        /// Real: `src/water.rs:38`.
        pub shore_waves: Vec<ShoreWave>,
    }
}

pub mod bmd {
    pub struct Vertex {
        pub position: [f32; 3],
        pub uv: [f32; 2],
        pub normal: [f32; 3],
    }
    pub struct Mesh {
        pub vertices: Vec<Vertex>,
        pub indices: Vec<u16>,
    }
    pub struct Model {
        pub scale: [f32; 3],
        pub direction: [f32; 3],
        pub up: [f32; 3],
        pub position: [f32; 3],
        pub meshes: Vec<Mesh>,
    }
}

pub mod scene {
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
    pub struct RgbaImage;
    impl RgbaImage {
        pub fn width(&self) -> u32 {
            0
        }
        pub fn height(&self) -> u32 {
            0
        }
        pub fn as_raw(&self) -> &Vec<u8> {
            unimplemented!()
        }
    }
    pub struct TreeMesh {
        pub lods: Vec<TreeLod>,
        pub bark: RgbaImage,
        pub composite: RgbaImage,
    }
    /// Real: `src/trees.rs:51`.
    pub struct Trees {
        pub types: Vec<Option<TreeMesh>>,
        pub placements: Vec<crate::scene::Tree>,
    }
}

/// `fsr::raw_dx12` hands the hardware path the device wgpu itself runs on. There
/// is no such device here, so the shim can only fail — which is exactly what the
/// probe reports.
pub mod fsr {
    pub fn raw_dx12(
        _device: &crate::wgpu::Device,
    ) -> Result<(d3d12_crate::Device, d3d12_crate::CommandQueue), String> {
        Err("offline shim: no DX12 device".into())
    }
}

// --- the module ------------------------------------------------------------

#[path = "../../src/imaginator/mod.rs"]
pub mod imaginator;
