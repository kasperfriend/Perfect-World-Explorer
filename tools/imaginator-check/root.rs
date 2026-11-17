//! Crate root for the offline Imaginator check.
//!
//! `src/imaginator/hardware/mod.rs` reaches for four things the crate provides:
//! `crate::fsr::raw_dx12`, the `wgpu` device, the `d3d12` wrapper and the scene
//! types in `bvh`. The first three are shimmed here (`tools/imaginator-check`);
//! the scene types are the real ones, `#[path]`-included, so a change to
//! `bvh.rs` that breaks the hardware path fails this check.
//!
//! `tools/imaginator-check.sh` builds and tests this. It is not part of the
//! crate.

#![allow(non_snake_case, non_camel_case_types, dead_code, unused_imports)]

pub use winapi::shared::winerror::SUCCEEDED;

/// The real ray-tracing declarations under test.
///
/// It is mounted as `crate::sys` rather than at its real path because
/// `hardware/mod.rs` says `use super::sys` — which is the crate root here. The
/// file itself only names `winapi` types, so the move costs nothing.
#[path = "/home/user/Perfect-World-Explorer/src/imaginator/hardware/sys.rs"]
pub mod sys;

/// The `wgpu`/`d3d12` crates are extern to this root. `hardware/mod.rs` names
/// them through `crate::`, so they are mounted under the same names here.
pub mod d3d12 {
    pub use ::d3d12::*;
}
pub mod wgpu {
    pub use ::wgpu::*;
}

pub mod fsr {
    use crate::{d3d12, wgpu};
    /// The real one walks `as_hal::<wgpu_hal::dx12::Api>`; offline there is no
    /// device to walk, so the shim hands back nulls and the probe reports the
    /// device as unusable instead of pretending otherwise.
    pub fn raw_dx12(
        _device: &wgpu::Device,
    ) -> Result<(d3d12::Device, d3d12::CommandQueue), String> {
        Err("offline shim: no DX12 device".into())
    }
}

pub mod imaginator {
    /// The real `bvh.rs`, so a change to `Bvh::triangles` breaks this check.
    #[path = "/home/user/Perfect-World-Explorer/src/imaginator/bvh.rs"]
    pub mod bvh;

    #[path = "/home/user/Perfect-World-Explorer/src/imaginator/math.rs"]
    pub mod math;
}

/// The module under test: `hardware/dx12.rs`, the Windows-only implementation.
/// It is mounted at the crate root so its `super::sys` finds `sys` above; that is
/// also how `hardware/mod.rs` mounts it on Windows, so the file is checked in the
/// configuration that ships.
#[path = "/home/user/Perfect-World-Explorer/src/imaginator/hardware/dx12.rs"]
pub mod hardware;
