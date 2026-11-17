//! The hardware tracer: DXR acceleration structures on `wgpu`'s own device.
//!
//! Split in two so the module compiles everywhere the viewer does:
//!
//! * [`dx12`] is the real implementation, Windows only — the same rule
//!   `crate::fsr` follows, because `d3d12`/`wgpu-hal`'s native handles only exist
//!   there.
//! * `support` (below) is the small, platform-neutral half: the status
//!   vocabulary and a tracer that can only report why it is not running. It is
//!   what a Linux build compiles, so the page still answers "why is hardware not
//!   running" instead of being blank.
//!
//! The module page only ever calls [`HardwareStatus`] and
//! `HardwareTracer::{probe, ensure_built, status, built_triangles}`, so both
//! halves satisfy it and nothing else in the module has to know which one
//! compiled.

#[cfg(windows)]
mod dx12;
/// The raw D3D12 interfaces `winapi` 0.3.9 does not declare, declared by hand
/// against `d3d12.h`. Windows only: nothing else has a `ID3D12Device5`.
#[cfg(windows)]
mod sys;

#[cfg(windows)]
pub use dx12::*;

#[cfg(not(windows))]
pub use support::*;

/// Where the compiled RayQuery shader lives inside the module's folder.
#[cfg(not(windows))]
pub const DXIL_FILE: &str = "imaginator/dxr_trace.cso";

/// The shader's `Params` block in 32-bit floats: ten `float4`/`uint4` members.
#[cfg(not(windows))]
pub const PARAMS_FLOATS: usize = 40;

#[cfg(not(windows))]
pub mod support {
    use crate::imaginator::bvh::SharedScene;

    /// Why the hardware tracer is not running, or what it is doing.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum HardwareStatus {
        /// Nothing has been probed yet.
        Untried,
        /// The device and the build path work.
        Ready(String),
        /// The device or its driver cannot do DXR at the required tier.
        Unsupported(String),
        /// A build step failed.
        Failed(String),
    }

    impl HardwareStatus {
        pub fn available(&self) -> bool {
            matches!(self, HardwareStatus::Ready(_))
        }

        pub fn label(&self) -> String {
            match self {
                HardwareStatus::Untried => "not probed yet".into(),
                HardwareStatus::Ready(note) => format!("acceleration structures ready: {note}"),
                HardwareStatus::Unsupported(why) => format!("unavailable: {why}"),
                HardwareStatus::Failed(why) => format!("failed: {why}"),
            }
        }
    }

    /// The DX12 device behind `wgpu` does not exist here, so probing can only
    /// fail — with the reason rather than a silence.
    pub struct HardwareTracer {
        pub status: HardwareStatus,
        pub built_triangles: usize,
    }

    impl HardwareTracer {
        pub fn probe(_device: &wgpu::Device) -> Self {
            Self {
                status: HardwareStatus::Unsupported(
                    "DXR needs the D3D12 backend, which only exists on Windows".into(),
                ),
                built_triangles: 0,
            }
        }

        /// Build the acceleration structures for `scene`.
        ///
        /// # Errors
        /// Always, with the platform reason. Never a panic and never a fake
        /// success: the module page says "unavailable", and the software tracer
        /// keeps producing the image.
        pub fn ensure_built(&mut self, _scene: &SharedScene) -> Result<u64, String> {
            Err(self.status.label())
        }

        /// No DXR here, so there is never a RayQuery blob to find.
        pub fn dxil_present(&self, _directory: &std::path::Path) -> bool {
            false
        }

        /// No DXR here, so no pipeline is ever built from a blob.
        pub fn pipeline_ready(&self) -> bool {
            false
        }

        /// No DXR here, so there is nothing to dispatch. The signature mirrors
        /// the Windows one so the module page compiles on both halves.
        #[allow(clippy::too_many_arguments)]
        pub fn trace_frame(
            &mut self,
            _dxil: &[u8],
            _tlas: u64,
            _width: u32,
            _height: u32,
            _params: &[u8],
            _normals: &[u8],
            _materials: &[u8],
        ) -> Result<Vec<f32>, String> {
            Err("this build has no D3D12 ray-tracing device".into())
        }
    }
}
