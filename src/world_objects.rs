//! Incremental whole-world BMD preload. Native-size texture atlases are shared
//! globally across geometry chunks, rather than duplicated at every page turn.
use crate::{
    gpu::Vertex,
    objects::Objects,
    scene::Ornament,
    scenery_visibility::{allows_with_context, Pass, RenderContext, Visibility},
};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc, OnceLock,
    },
};
use wgpu::util::DeviceExt;
/// Query the host CPU budget once, not once per mip level of every page.
fn mip_worker_limit() -> usize {
    static WORKERS: OnceLock<usize> = OnceLock::new();
    *WORKERS.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(4)
    })
}

pub(crate) const SIDE: usize = 4096;
const DEFAULT_MAX_VERTICES: usize = 2_000_000;
const MAX_PAGE_VERTICES: usize = 8_000_000;

fn page_vertex_limit() -> usize {
    std::env::var("PW_WORLD_PAGE_VERTICES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1_000_000, MAX_PAGE_VERTICES))
        .unwrap_or(DEFAULT_MAX_VERTICES)
}

/// One ornament's index range inside a page. The two EC-BSD flags decide
/// whether this range is submitted in reflection/refraction; the main and map
/// passes always submit it. `start`/`end` are indices into `Page::indices`.
#[derive(Clone, Debug)]
pub struct PlacementRange {
    pub start: u32,
    pub end: u32,
    /// Contiguous transformed vertex block for this mesh. Bounds can be
    /// computed without rescanning its index list.
    pub vertex_start: u32,
    pub vertex_end: u32,
    pub export_id: u32,
    /// Original model/mesh encounter order, independent of storage chunking.
    pub order: u64,
    pub visibility: Visibility,
}

/// The part of a page the command builder reads. Kept separate from `GpuPage`
/// so the fold below is testable without a device.
pub struct PageSpans<'a> {
    pub count: u32,
    pub placements: &'a [PlacementRange],
}

/// Fold the frozen submission order into the minimum set of draws for `view`.
///
/// Placements inside a page are stored back to back, so two consecutive order
/// entries that belong to the same page usually *touch*
/// (`last.end == next.start`). Drawing them as one span draws exactly the same
/// indices in exactly the same order — but as one draw call instead of one per
/// placement, which is what the pass used to issue: a whole-world load is
/// thousands of placements over a few hundred pages, per view.
///
/// A span is only ever extended across a placement that touches it, so a
/// placement the view hides can never be swallowed by a neighbour: it is
/// skipped and leaves a gap the merge rule refuses to cross.
pub fn fold_commands(pages: &[PageSpans], order: &[(usize, usize)], view: Pass) -> Vec<DrawCmd> {
    let mut cmds: Vec<DrawCmd> = Vec::with_capacity(order.len());
    for &(page, range) in order {
        let p = &pages[page];
        let (start, end) = if range == usize::MAX {
            (0, p.count)
        } else {
            let r = &p.placements[range];
            if !allows_with_context(view, r.visibility, RenderContext::default()) {
                continue;
            }
            (r.start, r.end)
        };
        if let Some(last) = cmds.last_mut() {
            if last.page == page && last.end == start {
                last.end = end;
                continue;
            }
        }
        cmds.push(DrawCmd { page, start, end });
    }
    cmds
}

/// Fold one page's placements into the spans a view draws, in order.
///
/// The builder stores placements back to back, so the visible ones usually
/// touch and collapse into a single span: the same indices in the same order,
/// one draw instead of one per placement. A hidden placement is skipped and its
/// indices stay out of the spans, because a span is only extended across a
/// neighbour that touches it.
fn fold_page_spans(placements: &[PlacementRange], view: Pass) -> Vec<(u32, u32)> {
    let mut spans: Vec<(u32, u32)> = Vec::with_capacity(placements.len());
    for r in placements {
        if !allows_with_context(view, r.visibility, RenderContext::default()) {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.1 == r.start => last.1 = r.end,
            _ => spans.push((r.start, r.end)),
        }
    }
    spans
}

/// One opaque scenery draw: a page and a contiguous span of its index buffer.
///
/// Several `PlacementRange`s can fold into one command (see
/// `GpuPage::draw_commands`), which is what keeps the whole-world pass at one
/// draw per page run instead of one per placed object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DrawCmd {
    pub page: usize,
    pub start: u32,
    pub end: u32,
}

/// Command lists kept for the whole-world pages: main, reflection, refraction.
pub const WORLD_VIEWS: usize = 3;
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PlacementBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub has_sdr: bool,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WorldDrawCmd {
    pub page: usize,
    pub start: u32,
    pub end: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub has_sdr: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct WorldDrawChunk {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub has_sdr: bool,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Default)]
pub struct WorldDrawList {
    pub cmds: Vec<WorldDrawCmd>,
    pub chunks: Vec<WorldDrawChunk>,
}

pub fn frustum_planes_from_matrix(m: [f32; 16]) -> [[f32; 4]; 6] {
    let r0 = [m[0], m[4], m[8], m[12]];
    let r1 = [m[1], m[5], m[9], m[13]];
    let r2 = [m[2], m[6], m[10], m[14]];
    let r3 = [m[3], m[7], m[11], m[15]];
    [
        [r3[0] + r0[0], r3[1] + r0[1], r3[2] + r0[2], r3[3] + r0[3]],
        [r3[0] - r0[0], r3[1] - r0[1], r3[2] - r0[2], r3[3] - r0[3]],
        [r3[0] + r1[0], r3[1] + r1[1], r3[2] + r1[2], r3[3] + r1[3]],
        [r3[0] - r1[0], r3[1] - r1[1], r3[2] - r1[2], r3[3] - r1[3]],
        r2,
        [r3[0] - r2[0], r3[1] - r2[1], r3[2] - r2[2], r3[3] - r2[3]],
    ]
}

/// Build a tighter view frustum for a pixel-space scissor rectangle.
/// Water reflection/refraction textures are sampled only beneath the water
/// surface (plus the shader's distortion margin); drawing world geometry
/// outside this rectangle still runs vertex work and CPU submission even
/// though rasterization is scissored away.
pub fn frustum_planes_from_matrix_rect(
    m: [f32; 16],
    rect: [f32; 4], // left, right, bottom, top in NDC
) -> [[f32; 4]; 6] {
    let r0 = [m[0], m[4], m[8], m[12]];
    let r1 = [m[1], m[5], m[9], m[13]];
    let r2 = [m[2], m[6], m[10], m[14]];
    let r3 = [m[3], m[7], m[11], m[15]];
    let [left, right, bottom, top] = rect;
    [
        std::array::from_fn(|i| r0[i] - left * r3[i]),
        std::array::from_fn(|i| right * r3[i] - r0[i]),
        std::array::from_fn(|i| r1[i] - bottom * r3[i]),
        std::array::from_fn(|i| top * r3[i] - r1[i]),
        r2,
        std::array::from_fn(|i| r3[i] - r2[i]),
    ]
}

#[inline]
fn all_chunks_inside_frustum(chunks: &[WorldDrawChunk], planes: &[[f32; 4]; 6]) -> bool {
    !chunks.is_empty()
        && chunks
            .iter()
            .all(|chunk| aabb_inside_frustum(planes, chunk.min, chunk.max))
}

pub fn aabb_in_frustum(planes: &[[f32; 4]; 6], min: [f32; 3], max: [f32; 3]) -> bool {
    if min[0] > max[0] {
        return false;
    }
    for p in planes {
        let px = if p[0] >= 0.0 { max[0] } else { min[0] };
        let py = if p[1] >= 0.0 { max[1] } else { min[1] };
        let pz = if p[2] >= 0.0 { max[2] } else { min[2] };
        if p[0] * px + p[1] * py + p[2] * pz + p[3] < 0.0 {
            return false;
        }
    }
    true
}

/// True when the entire AABB lies strictly inside all six frustum planes,
/// allowing callers to skip per-child frustum tests inside a chunk/page.
#[inline]
pub fn aabb_inside_frustum(planes: &[[f32; 4]; 6], min: [f32; 3], max: [f32; 3]) -> bool {
    if min[0] > max[0] {
        return false;
    }
    for p in planes {
        let nx = if p[0] >= 0.0 { min[0] } else { max[0] };
        let ny = if p[1] >= 0.0 { min[1] } else { max[1] };
        let nz = if p[2] >= 0.0 { min[2] } else { max[2] };
        if p[0] * nx + p[1] * ny + p[2] * nz + p[3] < 0.0 {
            return false;
        }
    }
    true
}

/// Maximum culled index gap bridged into a single `draw_indexed` call on the
/// same `GpuPage` in opaque passes (192 indices = 64 triangles). Bridging tiny
/// culled gaps slashes CPU draw-call overhead 5-20x when frustum/size culling
/// fragments runs across the loaded whole world.
pub const MAX_COALESCE_GAP: u32 = 192;

/// Per-pass visibility parameters shared by whole-world and local scenery
/// culling (frustum, sub-pixel screen-size threshold, and optional water clip
/// plane).
#[derive(Clone, Copy, Debug)]
pub struct CullParams {
    pub frustum: [[f32; 4]; 6],
    pub eye: [f32; 3],
    /// `0.5 * viewport_h / tan(fov_y / 2)` for perspective views; `0.0` for
    /// orthographic/map views to disable sub-pixel culling.
    pub proj_scale: f32,
    /// Minimum projected bounding-sphere diameter in pixels (`~1.5` main,
    /// `~4.0` water reflection/refraction, `0.0` disabled).
    pub min_screen_px: f32,
    /// Optional water clip plane `(water_y, sign)` where `sign > 0` keeps
    /// `y >= water_y` and `sign < 0` keeps `y <= water_y`.
    pub clip_plane: Option<(f32, f32)>,
}

impl CullParams {
    /// Frustum-only culling (used by top-down minimap/full-map views).
    pub const fn frustum_only(frustum: [[f32; 4]; 6]) -> Self {
        Self {
            frustum,
            eye: [0.0; 3],
            proj_scale: 0.0,
            min_screen_px: 0.0,
            clip_plane: None,
        }
    }
}

/// One consecutive page run in the stable whole-world command order.
/// wgpu's indexed-indirect ABI: five tightly packed 32-bit words.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PageDrawIndexedArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}
const PAGE_DRAW_ARGS_SIZE: u64 = std::mem::size_of::<PageDrawIndexedArgs>() as u64;

/// Retain an adequate allocation; grow geometrically only when this frame
/// actually needs more space. None means direct draws are the safe fallback.
fn page_indirect_capacity(current: u64, required: u64, limit: u64) -> Option<u64> {
    if required == 0 {
        return None;
    }
    let capacity = if current >= required {
        current
    } else {
        required.max(current.saturating_mul(2).min(limit))
    };
    (capacity <= limit).then_some(capacity)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorldIndirectRun {
    page: usize,
    first_record: u32,
    record_count: u32,
    first_cmd: usize,
}

/// Reusable per-frame scratch buffers owned by `Renderer` so whole-world
/// culling, indirect packing and draw-call coalescing perform zero heap
/// allocations after their first use. Normal and SDR commands need distinct
/// buffers because both lists can be consumed by one encoded render pass.
const WORLD_GPU_CULL_WGSL: &str = r#"
struct Bounds {
    min: vec4<f32>,
    max: vec4<f32>,
    index_count: u32,
    first_index: u32,
    flags: u32,
    _pad: u32,
};
struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};
struct Run {
    first_cmd: u32,
    command_count: u32,
    first_output: u32,
    _pad: u32,
};
struct Params {
    frustum: array<vec4<f32>, 6>,
    eye_proj: vec4<f32>,
    screen_clip: vec4<f32>,
    flags: vec4<u32>,
};
@group(0) @binding(0) var<storage, read> bounds: array<Bounds>;
@group(0) @binding(1) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var<storage, read> runs: array<Run>;
@group(0) @binding(4) var<storage, read_write> counts: array<u32>;
var<workgroup> prefix: array<u32, 64>;
var<workgroup> cursor: u32;
var<workgroup> output_count: u32;

fn finite(v: f32) -> bool {
    return v >= -3.0e38 && v <= 3.0e38;
}
fn finite3(v: vec3<f32>) -> bool {
    return finite(v.x) && finite(v.y) && finite(v.z);
}
fn visible_aabb(b: Bounds) -> bool {
    let mn = b.min.xyz;
    let mx = b.max.xyz;
    if p.flags.y != 0u && (b.flags & 1u) == 0u {
        return false;
    }
    // Invalid numerical bounds are uncertainty, not evidence of occlusion.
    if !finite3(mn) || !finite3(mx) {
        return true;
    }
    if any(mn > mx) {
        return false;
    }
    // Aabb-vs-plane test: reject only when its positive vertex is outside.
    for (var i = 0u; i < 6u; i += 1u) {
        let plane = p.frustum[i];
        let positive = select(mn, mx, plane.xyz >= vec3<f32>(0.0));
        if dot(plane.xyz, positive) + plane.w < 0.0 {
            return false;
        }
    }
    if p.screen_clip.y > 0.5 {
        let water_y = p.screen_clip.z;
        let sign = p.screen_clip.w;
        if sign > 0.0 && mx.y < water_y {
            return false;
        }
        if sign < 0.0 && mn.y > water_y {
            return false;
        }
    }
    let proj_scale = p.eye_proj.w;
    let min_screen_px = p.screen_clip.x;
    if proj_scale > 0.0 && min_screen_px > 0.0 {
        let center = (mn + mx) * 0.5;
        let extent = (mx - mn) * 0.5;
        let radius_sq = dot(extent, extent);
        let delta = center - p.eye_proj.xyz;
        let distance_sq = dot(delta, delta);
        if distance_sq > radius_sq {
            let lhs = 4.0 * radius_sq * proj_scale * proj_scale;
            let rhs = min_screen_px * min_screen_px * distance_sq;
            if lhs < rhs {
                return false;
            }
        }
    }
    return true;
}
@compute @workgroup_size(64, 1, 1)
fn main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    if group.x >= p.flags.x {
        return;
    }
    let run = runs[group.x];
    let lane = local.x;
    if lane == 0u {
        cursor = 0u;
        output_count = 0u;
    }
    workgroupBarrier();

    loop {
        let command_in_run = cursor + lane;
        var keep = 0u;
        if command_in_run < run.command_count {
            let b = bounds[run.first_cmd + command_in_run];
            if b.index_count > 0u && visible_aabb(b) {
                keep = 1u;
            }
        }
        prefix[lane] = keep;
        workgroupBarrier();

        var step = 1u;
        loop {
            if step >= 64u {
                break;
            }
            var add = 0u;
            if lane >= step {
                add = prefix[lane - step];
            }
            workgroupBarrier();
            prefix[lane] = prefix[lane] + add;
            workgroupBarrier();
            step = step * 2u;
        }

        if keep != 0u {
            let b = bounds[run.first_cmd + command_in_run];
            let output_index = run.first_output + output_count + prefix[lane] - 1u;
            draw_args[output_index].index_count = b.index_count;
            draw_args[output_index].instance_count = 1u;
            draw_args[output_index].first_index = b.first_index;
            draw_args[output_index].base_vertex = 0;
            draw_args[output_index].first_instance = 0u;
        }
        workgroupBarrier();
        if lane == 0u {
            output_count = output_count + prefix[63u];
            cursor = cursor + 64u;
        }
        workgroupBarrier();
        if cursor >= run.command_count {
            break;
        }
    }
    if lane == 0u {
        counts[group.x] = output_count;
    }
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WorldGpuCullBounds {
    min: [f32; 4],
    max: [f32; 4],
    index_count: u32,
    first_index: u32,
    flags: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WorldGpuCullRun {
    first_cmd: u32,
    command_count: u32,
    first_output: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WorldGpuCullParams {
    frustum: [[f32; 4]; 6],
    eye_proj: [f32; 4],
    screen_clip: [f32; 4],
    flags: [u32; 4],
}

impl WorldGpuCullParams {
    fn new(cull: &CullParams, count: usize, sdr_only: bool) -> Self {
        let (clip_enabled, water_y, sign) = cull
            .clip_plane
            .map_or((0.0, 0.0, 0.0), |(y, sign)| (1.0, y, sign));
        Self {
            frustum: cull.frustum,
            eye_proj: [cull.eye[0], cull.eye[1], cull.eye[2], cull.proj_scale],
            screen_clip: [cull.min_screen_px, clip_enabled, water_y, sign],
            flags: [count as u32, if sdr_only { 1 } else { 0 }, 0, 0],
        }
    }
}

struct WorldGpuCullView {
    commands: Vec<WorldDrawCmd>,
    _bounds: wgpu::Buffer,
    indirect: wgpu::Buffer,
    params: wgpu::Buffer,
    _runs: wgpu::Buffer,
    counts: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    runs: Vec<WorldIndirectRun>,
    ready: bool,
}

impl WorldGpuCullView {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        commands: &[WorldDrawCmd],
    ) -> Option<Self> {
        if commands.is_empty() {
            return None;
        }
        let mut bounds = Vec::with_capacity(commands.len());
        let mut args = Vec::with_capacity(commands.len());
        let mut runs = Vec::<WorldIndirectRun>::new();
        for (cmd_index, cmd) in commands.iter().enumerate() {
            bounds.push(WorldGpuCullBounds {
                min: [cmd.min[0], cmd.min[1], cmd.min[2], 0.0],
                max: [cmd.max[0], cmd.max[1], cmd.max[2], 0.0],
                index_count: cmd.end.saturating_sub(cmd.start),
                first_index: cmd.start,
                flags: if cmd.has_sdr { 1 } else { 0 },
                pad: 0,
            });
            args.push(PageDrawIndexedArgs {
                index_count: cmd.end.saturating_sub(cmd.start),
                instance_count: 0,
                first_index: cmd.start,
                base_vertex: 0,
                first_instance: 0,
            });
            let record_index = cmd_index as u32;
            match runs.last_mut() {
                Some(run) if run.page == cmd.page => run.record_count += 1,
                _ => runs.push(WorldIndirectRun {
                    page: cmd.page,
                    first_record: record_index,
                    record_count: 1,
                    first_cmd: cmd_index,
                }),
            }
        }
        let gpu_runs: Vec<WorldGpuCullRun> = runs
            .iter()
            .map(|run| WorldGpuCullRun {
                first_cmd: run.first_cmd as u32,
                command_count: run.record_count,
                first_output: run.first_record,
                pad: 0,
            })
            .collect();
        let bounds = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world GPU culling bounds"),
            contents: bytemuck::cast_slice(&bounds),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let indirect = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world GPU-generated indirect commands"),
            contents: bytemuck::cast_slice(&args),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
        });
        let runs_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world GPU culling page runs"),
            contents: bytemuck::cast_slice(&gpu_runs),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let counts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("whole-world GPU compacted draw counts"),
            size: (runs.len() * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("whole-world GPU culling parameters"),
            size: std::mem::size_of::<WorldGpuCullParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("whole-world GPU culling bindings"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bounds.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: indirect.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: runs_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: counts.as_entire_binding(),
                },
            ],
        });
        Some(Self {
            commands: commands.to_vec(),
            _bounds: bounds,
            indirect,
            params,
            _runs: runs_buffer,
            counts,
            bind_group,
            runs,
            ready: false,
        })
    }
}

struct WorldGpuCuller {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    views: [Option<WorldGpuCullView>; WORLD_VIEWS],
}

impl WorldGpuCuller {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("whole-world GPU culling layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("whole-world GPU culling WGSL"),
            source: wgpu::ShaderSource::Wgsl(WORLD_GPU_CULL_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("whole-world GPU culling pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("whole-world GPU culling pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
        });
        Self {
            pipeline,
            layout,
            views: std::array::from_fn(|_| None),
        }
    }
}

fn world_command_survives_gpu_cull(cmd: &WorldDrawCmd, cull: &CullParams, sdr_only: bool) -> bool {
    if cmd.start >= cmd.end || (sdr_only && !cmd.has_sdr) {
        return false;
    }
    let finite = cmd.min.iter().chain(cmd.max.iter()).all(|v| v.is_finite());
    if !finite {
        return true;
    }
    if (0..3).any(|axis| cmd.min[axis] > cmd.max[axis]) {
        return false;
    }
    aabb_survives_clip(cull.clip_plane, cmd.min, cmd.max)
        && aabb_in_frustum(&cull.frustum, cmd.min, cmd.max)
        && aabb_survives_screen_size(
            cull.eye,
            cull.proj_scale,
            cull.min_screen_px,
            cmd.min,
            cmd.max,
        )
}

#[derive(Default)]
pub struct WorldDrawScratch {
    pub cmds: Vec<DrawCmd>,
    pub page_visible: Vec<bool>,
    indirect_args: Vec<PageDrawIndexedArgs>,
    indirect_runs: Vec<WorldIndirectRun>,
    indirect_buffers: [Option<wgpu::Buffer>; 2],
    gpu_culler: Option<WorldGpuCuller>,
}

/// Returns false when an AABB lies completely on the clipped side of a water
/// plane (`max_y < water_y` when keeping above, `min_y > water_y` when keeping
/// below).
#[inline]
pub fn aabb_survives_clip(clip_plane: Option<(f32, f32)>, min: [f32; 3], max: [f32; 3]) -> bool {
    if let Some((water_y, sign)) = clip_plane {
        if sign > 0.0 && max[1] < water_y {
            return false;
        }
        if sign < 0.0 && min[1] > water_y {
            return false;
        }
    }
    true
}

/// Fast squared-distance projected screen-diameter test (zero `sqrt`).
/// Returns false when an AABB's bounding sphere projects smaller than
/// `min_screen_px` pixels across.
#[inline]
pub fn aabb_survives_screen_size(
    eye: [f32; 3],
    proj_scale: f32,
    min_screen_px: f32,
    min: [f32; 3],
    max: [f32; 3],
) -> bool {
    if min_screen_px <= 0.0 || proj_scale <= 0.0 {
        return true;
    }
    if min[0] > max[0] {
        return false;
    }
    let cx = 0.5 * (min[0] + max[0]);
    let cy = 0.5 * (min[1] + max[1]);
    let cz = 0.5 * (min[2] + max[2]);
    let ex = 0.5 * (max[0] - min[0]);
    let ey = 0.5 * (max[1] - min[1]);
    let ez = 0.5 * (max[2] - min[2]);
    let r_sq = ex * ex + ey * ey + ez * ez;
    let dx = cx - eye[0];
    let dy = cy - eye[1];
    let dz = cz - eye[2];
    let d_sq = dx * dx + dy * dy + dz * dz;
    if d_sq <= r_sq {
        return true;
    }
    // 2 * r * proj_scale / d >= min_screen_px
    // <=> 4 * r_sq * proj_scale^2 >= min_screen_px^2 * d_sq
    let lhs = 4.0 * r_sq * proj_scale * proj_scale;
    let rhs = min_screen_px * min_screen_px * d_sq;
    lhs >= rhs
}

/// Push a visible `(page, start, end)` span into `out`, coalescing contiguous
/// ranges and (when `max_gap > 0`) bridging tiny culled gaps on the same page
/// into a single `draw_indexed` call.
#[inline]
pub fn push_coalesced_draw(
    out: &mut Vec<DrawCmd>,
    page: usize,
    start: u32,
    end: u32,
    max_gap: u32,
) {
    if start >= end {
        return;
    }
    if let Some(prev) = out.last_mut() {
        if prev.page == page && start >= prev.end && start - prev.end <= max_gap {
            prev.end = prev.end.max(end);
            return;
        }
    }
    out.push(DrawCmd { page, start, end });
}

pub struct Page {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub atlas: Vec<u8>,
    /// Earlier emitted page whose immutable atlas this geometry reuses.
    pub shared_atlas: Option<usize>,
    pub processed: usize,
    pub used_height: usize,
    pub mips: Vec<Vec<u8>>,
    pub placements: Vec<PlacementRange>,
    /// Bounds are prepared by the loader worker rather than on the UI thread.
    pub bounds: Vec<PlacementBounds>,
    pub page_min: [f32; 3],
    pub page_max: [f32; 3],
    pub page_has_sdr: bool,
}
impl Page {
    pub fn finish(mut self) -> Self {
        // Save unused rows in the last page. Keep all seven mip reductions valid.
        let height = self.used_height.max(crate::texture_atlas::UNIT).min(SIDE);
        let factor = SIDE as f32 / height as f32;
        for v in &mut self.vertices {
            v.uv[1] *= factor;
            v.color[1] *= factor;
            v.color[3] *= factor;
        }
        // The streamer grows the atlas as textures are packed instead of
        // zero-filling all 4096 rows up front, so make the final buffer exactly
        // the extent the upload reads: `GpuPage::upload` copies `SIDE × height`
        // texels and a short buffer would be an out-of-range upload. Shared-atlas
        // pages own no pixels at all and keep the empty vector they assert on.
        let extent = SIDE * height * 4;
        if !self.atlas.is_empty() {
            if self.atlas.len() < extent {
                self.atlas.resize(extent, 0);
            }
            self.atlas.truncate(extent);
        }
        self.atlas.shrink_to_fit();
        self.used_height = height;
        self.vertices.shrink_to_fit();
        self.indices.shrink_to_fit();
        self.placements.shrink_to_fit();
        self.bounds = Vec::with_capacity(self.placements.len());
        self.page_min = [f32::INFINITY; 3];
        self.page_max = [f32::NEG_INFINITY; 3];
        self.page_has_sdr = false;
        for placement in &self.placements {
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            let vertex_start = placement.vertex_start as usize;
            let vertex_end = placement.vertex_end as usize;
            if vertex_start < vertex_end && vertex_end <= self.vertices.len() {
                let vertices = &self.vertices[vertex_start..vertex_end];
                // The builder appends each mesh's transformed vertices as one
                // contiguous block and assigns one material kind to that mesh.
                // Use that known range directly: no second walk over triangle
                // indices is needed to rediscover the same vertex interval.
                for vertex in vertices {
                    for axis in 0..3 {
                        min[axis] = min[axis].min(vertex.pos[axis]);
                        max[axis] = max[axis].max(vertex.pos[axis]);
                    }
                }
                let kind = vertices[0].kind;
                let has_sdr = (kind > 2.01 && kind < 2.03) || (kind > 2.11 && kind < 2.13);
                self.page_has_sdr |= has_sdr;
                self.bounds.push(PlacementBounds { min, max, has_sdr });
            } else {
                self.bounds.push(PlacementBounds {
                    min,
                    max,
                    has_sdr: false,
                });
            }
            if min[0] <= max[0] {
                for axis in 0..3 {
                    self.page_min[axis] = self.page_min[axis].min(min[axis]);
                    self.page_max[axis] = self.page_max[axis].max(max[axis]);
                }
            }
        }
        if self.shared_atlas.is_some() {
            assert!(self.atlas.is_empty() && self.mips.is_empty());
            return self;
        }
        // Mip generation is done by the background scene worker, not the UI thread.
        let (mut w, mut h) = (SIDE, height);
        for level in 1..8 {
            let source = if level == 1 {
                &self.atlas
            } else {
                &self.mips[level - 2]
            };
            let (nw, nh) = (w / 2, h / 2);
            let mut out = vec![0u8; nw * nh * 4];
            // Parallelize only mip levels with at least 4 MiB of output.
            // Smaller levels are faster to finish inline than to schedule a
            // fresh scoped worker group for every page.
            let threads = if nw * nh * 4 >= 4 * 1024 * 1024 {
                // Page finishing already runs beside cache I/O and GPU uploads.
                // Four workers cap bandwidth contention on desktop CPUs.
                mip_worker_limit().min(nh.div_ceil(2).max(1))
            } else {
                1
            };
            if threads > 1 {
                let chunk_rows = nh.div_ceil(threads);
                std::thread::scope(|s| {
                    for (chunk_idx, out_chunk) in out.chunks_mut(chunk_rows * nw * 4).enumerate() {
                        let y_start = chunk_idx * chunk_rows;
                        let rows = out_chunk.len() / (nw * 4);
                        s.spawn(move || {
                            let src_stride = w * 4;
                            let dst_stride = nw * 4;
                            for dy in 0..rows {
                                let y = y_start + dy;
                                let r0 = &source[2 * y * src_stride..(2 * y + 1) * src_stride];
                                let r1 =
                                    &source[(2 * y + 1) * src_stride..(2 * y + 2) * src_stride];
                                let dst = &mut out_chunk[dy * dst_stride..(dy + 1) * dst_stride];
                                for x in 0..nw {
                                    let sx = x * 8;
                                    let dx = x * 4;
                                    let a0 = &r0[sx..sx + 8];
                                    let a1 = &r1[sx..sx + 8];
                                    let d = &mut dst[dx..dx + 4];
                                    d[0] = ((a0[0] as u32
                                        + a0[4] as u32
                                        + a1[0] as u32
                                        + a1[4] as u32
                                        + 2)
                                        >> 2) as u8;
                                    d[1] = ((a0[1] as u32
                                        + a0[5] as u32
                                        + a1[1] as u32
                                        + a1[5] as u32
                                        + 2)
                                        >> 2) as u8;
                                    d[2] = ((a0[2] as u32
                                        + a0[6] as u32
                                        + a1[2] as u32
                                        + a1[6] as u32
                                        + 2)
                                        >> 2) as u8;
                                    d[3] = ((a0[3] as u32
                                        + a0[7] as u32
                                        + a1[3] as u32
                                        + a1[7] as u32
                                        + 2)
                                        >> 2) as u8;
                                }
                            }
                        });
                    }
                });
            } else {
                for y in 0..nh {
                    for x in 0..nw {
                        let a = (2 * y * w + 2 * x) * 4;
                        let b = a + w * 4;
                        let dst = (y * nw + x) * 4;
                        for c in 0..4 {
                            out[dst + c] = ((source[a + c] as u32
                                + source[a + 4 + c] as u32
                                + source[b + c] as u32
                                + source[b + 4 + c] as u32
                                + 2)
                                >> 2) as u8;
                        }
                    }
                }
            }
            self.mips.push(out);
            w = nw;
            h = nh;
        }
        self
    }
}
/// Measured residency and resolution outcome of one whole-world preload
/// (P3.08: measured numbers, not estimates).
#[derive(Clone, Debug)]
pub struct DoneStats {
    pub residency: crate::objects::ResidencyStats,
    pub resolver: crate::texture_resolution::ResolverStats,
    /// Register rows recorded for references that resolved nowhere or were
    /// rejected by name (.sdr. shader registry, decode failures, missing).
    pub unresolved_rows: usize,
    /// Distinct BMD assets that could not be opened or parsed. Placements
    /// using them produce no geometry and are reported, not fatal.
    pub unavailable_models: Vec<String>,
    /// A few direct lookup/parse causes, so an all-empty world can be
    /// diagnosed without relying on the process log.
    pub unavailable_examples: Vec<String>,
    /// Pages emitted by this preload.
    pub pages: usize,
    pub atlas_pages: usize,
    pub atlas_bytes: usize,
    /// Indicates this build restored completed pages from disk.
    pub cache_hit: bool,
}
pub enum Message {
    Page(Arc<Page>),
    /// A cache page failed validation; discard prior cache pages and use the
    /// cold-build pages that follow.
    CacheInvalid,
    Done {
        processed: usize,
        skipped: usize,
        stats: DoneStats,
        world_draw_cmds: [WorldDrawList; WORLD_VIEWS],
    },
    Error(String),
}
fn basis(a: [f32; 3]) -> Vec3 {
    let v = Vec3::from_array(a);
    if v.length_squared() < 1e-8 {
        Vec3::Y
    } else {
        v.normalize()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TextureKey {
    Content(u64),
    Missing,
}
struct Builder {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    atlas: Vec<u8>,
    shared_atlas: Option<usize>,
    slots: HashMap<TextureKey, crate::texture_atlas::Slot>,
    packing: crate::texture_atlas::Atlas,
    used_height: usize,
    placements: Vec<PlacementRange>,
}
impl Builder {
    fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            // Grow only to occupied rows; a mostly-empty page needn't zero 64 MiB.
            atlas: Vec::new(),
            shared_atlas: None,
            slots: HashMap::new(),
            packing: crate::texture_atlas::Atlas::new(),
            used_height: 0,
            placements: Vec::new(),
        }
    }
    fn shared(source: usize, height: usize) -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            atlas: Vec::new(),
            shared_atlas: Some(source),
            slots: HashMap::new(),
            packing: crate::texture_atlas::Atlas::new(),
            used_height: height,
            placements: Vec::new(),
        }
    }
    fn page(self, processed: usize) -> Page {
        Page {
            vertices: self.vertices,
            indices: self.indices,
            atlas: self.atlas,
            shared_atlas: self.shared_atlas,
            processed,
            used_height: self.used_height,
            mips: Vec::new(),
            placements: self.placements,
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish()
    }
}
#[derive(Clone, Copy)]
struct SharedSlot {
    source: usize,
    height: usize,
    slot: crate::texture_atlas::Slot,
}
fn remember_atlas(page: &Builder, source: usize, slots: &mut HashMap<TextureKey, SharedSlot>) {
    assert!(page.shared_atlas.is_none());
    let height = page.used_height.clamp(crate::texture_atlas::UNIT, SIDE);
    for (key, &slot) in &page.slots {
        // Atlas content, slot coordinates and mip chain are immutable after
        // emission. Later chunks reuse them without resampling/re-encoding.
        slots.insert(
            key.clone(),
            SharedSlot {
                source,
                height,
                slot,
            },
        );
    }
}
fn atlas_mip_bytes(height: usize) -> usize {
    let height = height.clamp(crate::texture_atlas::UNIT, SIDE);
    (0..8)
        .map(|level| (SIDE >> level) * (height >> level) * 4)
        .sum()
}
/// Allocates (or reuses) the atlas slot for one texture inside a page and
/// blits native-resolution pixels. The slot key is the texture's CONTENT
/// key (Objects::texture_key), not the reference name: distinct names with
/// identical pixels share one slot, which measurably reduces slot count and
/// page turns. When the page is full and already holds geometry, a fresh
/// page replaces it in place and the finished page is returned for sending.
fn allocate_slot(
    page: &mut Builder,
    key: TextureKey,
    tex: &crate::objects::Texture,
) -> Result<(crate::texture_atlas::Slot, Option<Builder>), String> {
    if let Some(&s) = page.slots.get(&key) {
        return Ok((s, None));
    }
    let mut slot = page.packing.allocate(tex.width, tex.height, 0, false);
    let mut turned = None;
    if slot.is_none() && !page.indices.is_empty() {
        turned = Some(std::mem::replace(page, Builder::new()));
        slot = page.packing.allocate(tex.width, tex.height, 0, false);
    }
    let s = slot.ok_or("scenery texture exceeds atlas capacity")?;
    let required = (s.y + s.h) * SIDE * 4;
    if page.atlas.len() < required {
        page.atlas.resize(required, 0);
    }
    if tex.width == s.w && tex.height == s.h {
        for py in 0..s.h {
            let src = py * tex.width * 4;
            let dst = ((s.y + py) * SIDE + s.x) * 4;
            page.atlas[dst..dst + s.w * 4].copy_from_slice(&tex.rgba[src..src + s.w * 4]);
        }
    } else {
        // Nearest-neighbour source X coordinates are identical for every row.
        // Precompute them once instead of performing an integer division for
        // every output pixel; the row loop then does only indexed 4-byte copies.
        let source_x: Vec<usize> = (0..s.w)
            .map(|px| (((2 * px + 1) * tex.width) / (2 * s.w)).min(tex.width - 1))
            .collect();
        for py in 0..s.h {
            let sy = (((2 * py + 1) * tex.height) / (2 * s.h)).min(tex.height - 1);
            let source_row = sy * tex.width * 4;
            let target_row = ((s.y + py) * SIDE + s.x) * 4;
            for (px, &sx) in source_x.iter().enumerate() {
                let src = source_row + sx * 4;
                let dst = target_row + px * 4;
                page.atlas[dst..dst + 4].copy_from_slice(&tex.rgba[src..src + 4]);
            }
        }
    }
    page.used_height = page.used_height.max(s.y + s.h);
    page.slots.insert(key, s);
    Ok((s, turned))
}
enum CacheJob {
    Write(Arc<Page>),
    Finish {
        processed: usize,
        skipped: usize,
        atlas_pages: usize,
        atlas_bytes: usize,
    },
}

struct AsyncCacheWriter {
    tx: Option<SyncSender<CacheJob>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AsyncCacheWriter {
    fn start(writer: crate::world_page_cache::Writer) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<CacheJob>(2);
        let handle = std::thread::Builder::new()
            .name("whole-world-cache-writer".into())
            .spawn(move || {
                let mut writer = Some(writer);
                while let Ok(job) = rx.recv() {
                    match job {
                        CacheJob::Write(page) => {
                            if let Some(w) = writer.as_mut() {
                                if let Err(error) = w.write_page(&page) {
                                    eprintln!("Whole-world cache write disabled: {error}");
                                    writer = None;
                                }
                            }
                        }
                        CacheJob::Finish {
                            processed,
                            skipped,
                            atlas_pages,
                            atlas_bytes,
                        } => {
                            if let Some(w) = writer.take() {
                                if let Err(error) =
                                    w.finish(processed, skipped, atlas_pages, atlas_bytes)
                                {
                                    eprintln!("Whole-world cache publish failed: {error}");
                                }
                            }
                            break;
                        }
                    }
                }
            })
            .expect("whole-world cache writer thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }

    fn write(&mut self, page: Arc<Page>) {
        if let Some(tx) = &self.tx {
            if tx.send(CacheJob::Write(page)).is_err() {
                self.tx = None;
            }
        }
    }

    fn finish(mut self, processed: usize, skipped: usize, atlas_pages: usize, atlas_bytes: usize) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(CacheJob::Finish {
                processed,
                skipped,
                atlas_pages,
                atlas_bytes,
            });
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn emit_page(
    tx: &SyncSender<Message>,
    cache: &mut Option<AsyncCacheWriter>,
    metadata: &mut Vec<WorldPageMeta>,
    page: Page,
) -> bool {
    metadata.push(WorldPageMeta::from_page(&page));
    let shared = Arc::new(page);
    if let Some(writer) = cache.as_mut() {
        writer.write(Arc::clone(&shared));
    }
    tx.send(Message::Page(shared)).is_ok()
}

/// Pipelined page finisher: runs UV scaling, per-placement AABB computation,
/// 7-level parallel mipmap generation, cache enqueue, and channel send on a
/// background thread so the builder thread immediately starts decoding the next
/// batch of BMDs and textures in `Objects::load_ids`.
struct PageFinisher {
    tx: Option<SyncSender<(Builder, usize)>>,
    handle: Option<std::thread::JoinHandle<(Vec<WorldPageMeta>, Option<AsyncCacheWriter>, bool)>>,
}
impl PageFinisher {
    fn start(tx_out: SyncSender<Message>, mut cache: Option<AsyncCacheWriter>) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(Builder, usize)>(2);
        let handle = std::thread::Builder::new()
            .name("whole-world-page-finisher".into())
            .spawn(move || {
                let mut metadata = Vec::new();
                let mut ok = true;
                for (builder, processed) in rx {
                    let page = builder.page(processed);
                    if !emit_page(&tx_out, &mut cache, &mut metadata, page) {
                        ok = false;
                        break;
                    }
                }
                (metadata, cache, ok)
            })
            .expect("whole-world page finisher thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }
    fn submit(&mut self, builder: Builder, processed: usize) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.send((builder, processed)).is_ok())
    }
    fn finish(mut self) -> (Vec<WorldPageMeta>, Option<AsyncCacheWriter>, bool) {
        drop(self.tx.take());
        self.handle
            .take()
            .map(|h| h.join().unwrap_or_else(|_| (Vec::new(), None, false)))
            .unwrap_or_else(|| (Vec::new(), None, false))
    }
}

pub fn build<O: Into<Arc<[Ornament]>>>(
    element: PathBuf,
    ornaments: O,
    tx: SyncSender<Message>,
    cancel: Arc<AtomicBool>,
) {
    let result = run(element, ornaments.into(), &tx, &cancel);
    if let Err(e) = result {
        let _ = tx.send(Message::Error(e));
    }
}
fn run(
    element: PathBuf,
    ornaments: Arc<[Ornament]>,
    tx: &SyncSender<Message>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // Sort compact indices instead of cloning/reordering every Ornament on
    // each enable. This preserves the source array shared by the renderer and
    // keeps the draw-order key tied to the original placement index.
    let mut order: Vec<usize> = (0..ornaments.len()).collect();
    if std::env::var_os("PW_WORLD_KEEP_ORDER_TEST").is_none() {
        order.sort_by(|&a, &b| ornaments[a].path.cmp(&ornaments[b].path));
    }
    let vertex_limit = page_vertex_limit();
    let cache_key = crate::world_page_cache::identity(&element, &ornaments, &order, vertex_limit);
    if let Some(key) = cache_key {
        if let Some(mut reader) = crate::world_page_cache::Reader::open(&key) {
            let total_pages = reader.page_count();
            let cache_processed = reader.processed;
            let cache_skipped = reader.skipped;
            let cache_atlas_pages = reader.atlas_pages;
            let cache_atlas_bytes = reader.atlas_bytes;
            let mut metadata = Vec::with_capacity(total_pages);
            let mut valid = true;
            let cache_batch = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
                .clamp(2, 8);
            // Overlap disk I/O + parallel SHA-256 verification of batch N+1 with
            // GPU upload of batch N via a bounded 1-batch-ahead prefetch thread.
            let stop_prefetch = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop_prefetch);
            let (batch_tx, batch_rx) =
                std::sync::mpsc::sync_channel::<Vec<Result<Page, String>>>(1);
            let prefetch_handle = std::thread::Builder::new()
                .name("whole-world-cache-prefetch".into())
                .spawn(move || {
                    let mut read_pages = 0usize;
                    while read_pages < total_pages && !stop_flag.load(Ordering::Relaxed) {
                        let batch = reader.read_pages_batch(cache_batch);
                        let n = batch.len();
                        if batch_tx.send(batch).is_err() || n == 0 {
                            break;
                        }
                        read_pages += n;
                    }
                    reader
                })
                .expect("whole-world cache prefetch thread");
            let mut index = 0usize;
            while index < total_pages && valid {
                if cancel.load(Ordering::Relaxed) {
                    stop_prefetch.store(true, Ordering::Relaxed);
                    drop(batch_rx);
                    let _ = prefetch_handle.join();
                    return Ok(());
                }
                let Ok(batch) = batch_rx.recv() else {
                    valid = false;
                    break;
                };
                if batch.is_empty() {
                    valid = false;
                    break;
                }
                for page_res in batch {
                    match page_res {
                        Ok(page) => {
                            if page.shared_atlas.is_some_and(|source| source >= index) {
                                eprintln!("Whole-world cache rejected: forward/shared atlas reference at page {index}");
                                valid = false;
                                break;
                            }
                            metadata.push(WorldPageMeta::from_page(&page));
                            let mut page = page;
                            page.placements = Vec::new();
                            page.bounds = Vec::new();
                            if tx.send(Message::Page(Arc::new(page))).is_err() {
                                stop_prefetch.store(true, Ordering::Relaxed);
                                drop(batch_rx);
                                let _ = prefetch_handle.join();
                                return Ok(());
                            }
                            index += 1;
                        }
                        Err(error) => {
                            eprintln!("Whole-world cache rejected at page {index}: {error}");
                            valid = false;
                            break;
                        }
                    }
                }
            }
            stop_prefetch.store(true, Ordering::Relaxed);
            drop(batch_rx);
            let mut reader = prefetch_handle.join().unwrap();
            if !valid {
                reader.invalidate();
                let _ = tx.send(Message::CacheInvalid);
            } else if let Err(error) = reader.complete() {
                eprintln!("Whole-world cache rejected after last page: {error}");
                reader.invalidate();
                let _ = tx.send(Message::CacheInvalid);
                valid = false;
            }
            let _ = (
                cache_processed,
                cache_skipped,
                cache_atlas_pages,
                cache_atlas_bytes,
            );
            if valid {
                let world_draw_cmds = world_culled_commands_from_meta(&metadata);
                let _ = tx.send(Message::Done {
                    processed: reader.processed,
                    skipped: reader.skipped,
                    stats: DoneStats {
                        residency: Default::default(),
                        resolver: Default::default(),
                        unresolved_rows: 0,
                        unavailable_models: Vec::new(),
                        unavailable_examples: Vec::new(),
                        pages: reader.page_count(),
                        atlas_pages: reader.atlas_pages,
                        atlas_bytes: reader.atlas_bytes,
                        cache_hit: true,
                    },
                    world_draw_cmds,
                });
                return Ok(());
            }
        }
    }
    let cache = cache_key.and_then(|key| match crate::world_page_cache::Writer::new(key) {
        Ok(writer) => Some(AsyncCacheWriter::start(writer)),
        Err(error) => {
            eprintln!("Whole-world cache unavailable: {error}");
            None
        }
    });
    let mut finisher = PageFinisher::start(tx.clone(), cache);
    let mut objects = Objects::open(&element, Arc::clone(&ornaments))?;
    let first = std::env::var("PW_WORLD_TEST_START")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(objects.ornaments.len());
    let count = std::env::var("PW_WORLD_TEST_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(objects.ornaments.len());
    let limit = first.saturating_add(count).min(objects.ornaments.len());
    let max_vertices = vertex_limit;
    let mut page = Builder::new();
    let mut shared_slots: HashMap<TextureKey, SharedSlot> = HashMap::new();
    let mut shared_geometry = HashMap::<usize, Builder>::new();
    // O(1) aggregate for the shared builders; rescanning every builder after
    // each object batch made world construction scale with the number of
    // distinct shared atlas sources.
    let mut shared_geometry_vertices = 0usize;
    let mut shared_geometry_full = std::collections::HashSet::<usize>::new();
    let mut skipped = 0usize;
    let mut pages = 0usize;
    let mut atlas_pages = 0usize;
    let mut atlas_bytes = 0usize;
    let fallback = crate::objects::Texture {
        width: 1,
        height: 1,
        rgba: vec![160, 160, 160, 255],
    };
    // Resolve the worker budget once per preload, not once per mesh.
    let transform_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4);
    let batch_size = if crate::objects::unbounded_textures() {
        256
    } else {
        128
    };
    for start in (first..limit).step_by(batch_size) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let end = (start + batch_size).min(limit);
        let ids = &order[start..end];
        objects.load_ids(ids)?;
        for &id in ids {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let Some(model) = objects.get(id) else {
                continue;
            };
            // EC-BSD record +36 low bytes, recovered at 0x489f0b/0x489f12.
            // Keep the flags with every index range so a hidden placement is
            // never submitted just because its page is resident.
            let visibility = Visibility::from_extra(objects.ornaments[id].extra);
            let z = basis(model.direction);
            let y = basis(model.up);
            let x = y.cross(z).normalize_or_zero();
            // Placement transform is invariant across every mesh vertex. Hoist
            // the translation conversion and scaled basis vectors out of the
            // vertex loop instead of rebuilding/re-scaling them per vertex.
            let origin = Vec3::from_array(model.position);
            let scaled_x = x * model.scale[0];
            let scaled_y = y * model.scale[1];
            let scaled_z = z * model.scale[2];
            for (mesh_index, mesh) in model.meshes.iter().enumerate() {
                // Stage, culling and alpha state come from the single recovered
                // table, not from per-asset guesses.
                let mut kind = match crate::bmd_baked::classify(
                    mesh.version,
                    !mesh.texture.trim().is_empty(),
                    mesh.material.as_ref().is_some_and(|m| m.two_sided),
                ) {
                    Ok(state) => state.kind,
                    Err(e) => {
                        // No silent skip: an unrepresentable mesh is an explicit
                        // error for the whole-world builder, not a dropped
                        // triangle. The BMD parser already rejects unsupported
                        // versions by name before this point.
                        return Err(format!("unsupported baked mesh {}: {e}", mesh.name));
                    }
                };
                if crate::objects::SdrUvAnim::from_texture_path(&mesh.texture)
                    .is_some_and(|a| a.tc_scroll[1] < 0.0)
                {
                    kind += 0.02;
                }
                // Content-keyed slot sharing (P3.08 cross-name dedup):
                // references with identical pixels collapse onto one atlas
                // slot regardless of how many names point at them.
                let key = objects
                    .texture_key(&mesh.texture)
                    .map(TextureKey::Content)
                    .unwrap_or(TextureKey::Missing);
                let tex = if let Some(t) = objects.texture(&mesh.texture) {
                    t
                } else {
                    skipped += 1;
                    &fallback
                };
                if page.vertices.len() + mesh.vertices.len() > max_vertices
                    && !page.indices.is_empty()
                {
                    let finished = std::mem::replace(&mut page, Builder::new());
                    atlas_pages += 1;
                    atlas_bytes += atlas_mip_bytes(finished.used_height);
                    remember_atlas(&finished, pages, &mut shared_slots);
                    pages += 1;
                    if !finisher.submit(finished, id) {
                        return Ok(());
                    }
                }
                let (page, slot, is_shared_builder) =
                    if let Some(shared) = shared_slots.get(&key).copied() {
                        let target = shared_geometry
                            .entry(shared.source)
                            .or_insert_with(|| Builder::shared(shared.source, shared.height));
                        if target.vertices.len() + mesh.vertices.len() > max_vertices
                            && !target.indices.is_empty()
                        {
                            let ready = std::mem::replace(
                                target,
                                Builder::shared(shared.source, shared.height),
                            );
                            shared_geometry_vertices =
                                shared_geometry_vertices.saturating_sub(ready.vertices.len());
                            shared_geometry_full.remove(&shared.source);
                            pages += 1;
                            if !finisher.submit(ready, id) {
                                return Ok(());
                            }
                        }
                        (target, shared.slot, Some(shared.source))
                    } else {
                        let (slot, turned) = allocate_slot(&mut page, key, tex)?;
                        if let Some(finished) = turned {
                            atlas_pages += 1;
                            atlas_bytes += atlas_mip_bytes(finished.used_height);
                            remember_atlas(&finished, pages, &mut shared_slots);
                            pages += 1;
                            if !finisher.submit(finished, id) {
                                return Ok(());
                            }
                        }
                        (&mut page, slot, None)
                    };
                let base = page.vertices.len() as u32;
                let range_start = page.indices.len() as u32;
                let ox = slot.x as f32 / SIDE as f32;
                let oy = slot.y as f32 / SIDE as f32;
                let sx = slot.w as f32 / SIDE as f32;
                let sy = slot.h as f32 / SIDE as f32;
                // Large meshes are embarrassingly parallel: each output vertex
                // depends only on its source vertex and this placement transform.
                // Reserve the final page range once, then let scoped workers fill
                // disjoint slices. Chunk order is fixed, so the resulting vertex
                // and index buffers remain byte-for-byte ordered as before.
                let vertex_count = mesh.vertices.len();
                let vertex_start = page.vertices.len();
                page.vertices
                    .resize(vertex_start + vertex_count, Vertex::zeroed());
                let workers = if vertex_count >= 32_768 {
                    transform_workers.min(vertex_count.div_ceil(8_192))
                } else {
                    1
                };
                if workers > 1 {
                    let chunk_size = vertex_count.div_ceil(workers);
                    std::thread::scope(|scope| {
                        for (chunk_index, dst) in page.vertices[vertex_start..]
                            .chunks_mut(chunk_size)
                            .enumerate()
                        {
                            let start = chunk_index * chunk_size;
                            let src = &mesh.vertices[start..start + dst.len()];
                            scope.spawn(move || {
                                for (v, out) in src.iter().zip(dst.iter_mut()) {
                                    let pos = origin
                                        + scaled_x * v.position[0]
                                        + scaled_y * v.position[1]
                                        + scaled_z * v.position[2];
                                    let normal =
                                        (x * v.normal[0] + y * v.normal[1] + z * v.normal[2])
                                            .normalize_or_zero();
                                    *out = Vertex {
                                        pos: pos.to_array(),
                                        normal: normal.to_array(),
                                        uv: [ox + v.uv[0] * sx, oy + v.uv[1] * sy],
                                        color: [ox, oy, sx, sy],
                                        kind,
                                        diffuse: v.day_diffuse,
                                        night_diffuse: v.night_diffuse,
                                    };
                                }
                            });
                        }
                    });
                } else {
                    for (v, out) in mesh.vertices.iter().zip(&mut page.vertices[vertex_start..]) {
                        let pos = origin
                            + scaled_x * v.position[0]
                            + scaled_y * v.position[1]
                            + scaled_z * v.position[2];
                        let normal = (x * v.normal[0] + y * v.normal[1] + z * v.normal[2])
                            .normalize_or_zero();
                        *out = Vertex {
                            pos: pos.to_array(),
                            normal: normal.to_array(),
                            uv: [ox + v.uv[0] * sx, oy + v.uv[1] * sy],
                            color: [ox, oy, sx, sy],
                            kind,
                            diffuse: v.day_diffuse,
                            night_diffuse: v.night_diffuse,
                        };
                    }
                }
                if let Some(source) = is_shared_builder {
                    shared_geometry_vertices =
                        shared_geometry_vertices.saturating_add(mesh.vertices.len());
                    if page.vertices.len() >= max_vertices {
                        shared_geometry_full.insert(source);
                    }
                }
                page.indices
                    .extend(mesh.indices.iter().map(|&n| base + n as u32));
                let range_end = page.indices.len() as u32;
                if range_end > range_start {
                    page.placements.push(PlacementRange {
                        start: range_start,
                        end: range_end,
                        vertex_start: base,
                        vertex_end: base + vertex_count as u32,
                        export_id: objects.ornaments[id].export_id,
                        order: ((id as u64) << 32) | mesh_index as u64,
                        visibility,
                    });
                }
            }
        }
        // Shared-atlas builders hold only vertices/indices (zero atlas pixels).
        // Accumulate across batches until a builder reaches `max_vertices` (or
        // total buffered shared vertices exceeds 2x `max_vertices`) instead of
        // emitting tiny fragmented pages after every batch.
        if shared_geometry_vertices >= max_vertices.saturating_mul(2)
            || !shared_geometry_full.is_empty()
        {
            let mut flush_sources: Vec<usize> =
                if shared_geometry_vertices >= max_vertices.saturating_mul(2) {
                    shared_geometry.keys().copied().collect()
                } else {
                    shared_geometry_full.iter().copied().collect()
                };
            flush_sources.sort_unstable();
            for source in flush_sources {
                if let Some(builder) = shared_geometry.remove(&source) {
                    shared_geometry_vertices =
                        shared_geometry_vertices.saturating_sub(builder.vertices.len());
                    shared_geometry_full.remove(&source);
                    if !builder.indices.is_empty() {
                        pages += 1;
                        if !finisher.submit(builder, end) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    if !shared_geometry.is_empty() {
        let mut ready: Vec<_> = shared_geometry.drain().collect();
        ready.sort_by_key(|(source, _)| *source);
        for (_, builder) in ready {
            if !builder.indices.is_empty() {
                pages += 1;
                if !finisher.submit(builder, limit) {
                    return Ok(());
                }
            }
        }
    }
    if !page.indices.is_empty() {
        atlas_pages += 1;
        atlas_bytes += atlas_mip_bytes(page.used_height);
        pages += 1;
        if !finisher.submit(page, limit) {
            return Ok(());
        }
    }
    let (world_metadata, mut cache, finisher_ok) = finisher.finish();
    if !finisher_ok {
        return Ok(());
    }
    let world_draw_cmds = world_culled_commands_from_meta(&world_metadata);
    if let Some(writer) = cache.take() {
        writer.finish(limit, skipped, atlas_pages, atlas_bytes);
    }
    let _ = tx.send(Message::Done {
        processed: limit,
        skipped,
        stats: DoneStats {
            residency: objects.residency(),
            resolver: objects.resolver_stats(),
            unresolved_rows: objects.unresolved().len(),
            unavailable_models: objects.unavailable_models(),
            unavailable_examples: objects.unavailable_examples().to_vec(),
            pages,
            atlas_pages,
            atlas_bytes,
            cache_hit: false,
        },
        world_draw_cmds,
    });
    Ok(())
}
/// Append-only storage shared by streamed whole-world pages.
pub struct WorldGeometryArena {
    chunks: Vec<WorldGeometryChunk>,
}
struct WorldGeometryChunk {
    buffer: Arc<wgpu::Buffer>,
    used: u64,
}
impl Default for WorldGeometryArena {
    fn default() -> Self {
        Self { chunks: Vec::new() }
    }
}
impl WorldGeometryArena {
    const CHUNK_BYTES: u64 = 32 * 1024 * 1024;

    fn allocate(&mut self, device: &wgpu::Device, bytes: u64) -> (Arc<wgpu::Buffer>, u64) {
        let bytes = (bytes.max(4) + 3) & !3;
        let needs_chunk = match self.chunks.last() {
            Some(chunk) => chunk.used.saturating_add(bytes) > chunk.buffer.size(),
            None => true,
        };
        if needs_chunk {
            let buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("whole-world shared geometry arena"),
                size: Self::CHUNK_BYTES.max(bytes),
                usage: wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::INDEX
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.chunks.push(WorldGeometryChunk { buffer, used: 0 });
        }
        let chunk = self.chunks.last_mut().expect("geometry arena chunk");
        let offset = chunk.used;
        chunk.used += bytes;
        (Arc::clone(&chunk.buffer), offset)
    }

    pub fn reset(&mut self) {
        self.chunks.clear();
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Build a page's combined vertex/index buffer directly in its initial mapped
/// allocation. This avoids a second page-sized CPU Vec and its extra copy.
fn create_page_geometry_buffer(
    device: &wgpu::Device,
    vertex_data: &[u8],
    index_data: &[u8],
) -> wgpu::Buffer {
    let vertex_bytes = vertex_data.len() as u64;
    let index_offset = (vertex_bytes + 3) & !3;
    let size = (index_offset + index_data.len() as u64).max(4);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("whole-world object geometry (vertices + indices)"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX,
        mapped_at_creation: true,
    });
    {
        let mut mapped = buffer.slice(..).get_mapped_range_mut();
        mapped[..vertex_data.len()].copy_from_slice(vertex_data);
        if index_offset > vertex_bytes {
            mapped[vertex_bytes as usize..index_offset as usize].fill(0);
        }
        let index_start = index_offset as usize;
        mapped[index_start..index_start + index_data.len()].copy_from_slice(index_data);
    }
    buffer.unmap();
    buffer
}

/// A GPU page can reference a subrange of a shared arena chunk.
pub struct GpuPage {
    geometry: Arc<wgpu::Buffer>,
    vertex_offset: u64,
    vertex_bytes: u64,
    index_offset: u64,
    index_bytes: u64,
    count: u32,
    normal: Arc<wgpu::BindGroup>,
    reflected: Arc<wgpu::BindGroup>,
    placements: Vec<PlacementRange>,
    bounds: Vec<PlacementBounds>,
    page_min: [f32; 3],
    page_max: [f32; 3],
    page_has_sdr: bool,
}
#[derive(Clone)]
struct WorldPageMeta {
    count: u32,
    placements: Vec<PlacementRange>,
    bounds: Vec<PlacementBounds>,
    page_min: [f32; 3],
    page_max: [f32; 3],
    page_has_sdr: bool,
}
impl WorldPageMeta {
    fn from_page(page: &Page) -> Self {
        Self {
            count: page.indices.len() as u32,
            placements: page.placements.clone(),
            bounds: page.bounds.clone(),
            page_min: page.page_min,
            page_max: page.page_max,
            page_has_sdr: page.page_has_sdr,
        }
    }
    fn from_gpu(page: &GpuPage) -> Self {
        Self {
            count: page.count,
            placements: page.placements.clone(),
            bounds: page.bounds.clone(),
            page_min: page.page_min,
            page_max: page.page_max,
            page_has_sdr: page.page_has_sdr,
        }
    }
}

impl GpuPage {
    pub fn release_cpu_ranges(&mut self) {
        self.placements.clear();
        self.placements.shrink_to_fit();
        self.bounds.clear();
        self.bounds.shrink_to_fit();
    }

    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        page: Page,
    ) -> Self {
        assert!(
            page.shared_atlas.is_none(),
            "shared atlas requires upload_world"
        );
        let image = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("whole-world object atlas"),
            size: wgpu::Extent3d {
                width: SIDE as u32,
                height: page.used_height as u32,
                depth_or_array_layers: 1,
            },
            // Tiles start on 128-pixel boundaries; seven reductions preserve
            // those boundaries and avoid cross-material mip contamination.
            mip_level_count: 8,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Match the D3D8 encoded-color sampling used by the local atlas.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut width = SIDE;
        let mut height = page.used_height;
        for mip in 0..8u32 {
            let level = if mip == 0 {
                &page.atlas
            } else {
                &page.mips[(mip - 1) as usize]
            };
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &image,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                level,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some((width * 4) as u32),
                    rows_per_image: Some(height as u32),
                },
                wgpu::Extent3d {
                    width: width as u32,
                    height: height as u32,
                    depth_or_array_layers: 1,
                },
            );
            width /= 2;
            height /= 2;
        }
        let view = image.create_view(&Default::default());
        let make = |buf: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("whole-world object material"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let normal = Arc::new(make(normal));
        let reflected = Arc::new(make(reflected));
        Self::geometry(device, page, normal, reflected)
    }
    /// Upload only geometry for a reused atlas. Bind groups keep the exact
    /// original texture/view alive; no second allocation or texture upload.
    pub fn upload_world(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        existing: &[Self],
        page: Page,
    ) -> Self {
        if let Some(source) = page.shared_atlas {
            let source = existing
                .get(source)
                .expect("shared atlas must precede its geometry");
            Self::geometry(
                device,
                page,
                Arc::clone(&source.normal),
                Arc::clone(&source.reflected),
            )
        } else {
            Self::upload(device, queue, layout, sampler, normal, reflected, page)
        }
    }

    /// Borrowed zero-copy variant used when `Arc<Page>` is shared with the
    /// asynchronous background disk-cache writer thread.
    pub fn upload_world_ref(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        existing: &[Self],
        page: &Page,
    ) -> Self {
        let (normal_bg, reflected_bg) = if let Some(source) = page.shared_atlas {
            let source = existing
                .get(source)
                .expect("shared atlas must precede its geometry");
            (Arc::clone(&source.normal), Arc::clone(&source.reflected))
        } else {
            let image = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("whole-world object atlas"),
                size: wgpu::Extent3d {
                    width: SIDE as u32,
                    height: page.used_height as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 8,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let mut width = SIDE;
            let mut height = page.used_height;
            for mip in 0..8u32 {
                let level = if mip == 0 {
                    &page.atlas
                } else {
                    &page.mips[(mip - 1) as usize]
                };
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &image,
                        mip_level: mip,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    level,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some((width * 4) as u32),
                        rows_per_image: Some(height as u32),
                    },
                    wgpu::Extent3d {
                        width: width as u32,
                        height: height as u32,
                        depth_or_array_layers: 1,
                    },
                );
                width /= 2;
                height /= 2;
            }
            let view = image.create_view(&Default::default());
            let make = |buf: &wgpu::Buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("whole-world object material"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                })
            };
            (Arc::new(make(normal)), Arc::new(make(reflected)))
        };
        let vertex_data = bytemuck::cast_slice(&page.vertices);
        let index_data = bytemuck::cast_slice(&page.indices);
        let vertex_bytes = vertex_data.len() as u64;
        let index_offset = (vertex_bytes + 3) & !3;
        let geometry = create_page_geometry_buffer(device, vertex_data, index_data);
        Self {
            geometry: Arc::new(geometry),
            vertex_offset: 0,
            vertex_bytes,
            index_offset,
            index_bytes: index_data.len() as u64,
            count: page.indices.len() as u32,
            normal: normal_bg,
            reflected: reflected_bg,
            placements: Vec::new(),
            bounds: Vec::new(),
            page_min: page.page_min,
            page_max: page.page_max,
            page_has_sdr: page.page_has_sdr,
        }
    }
    /// Upload geometry through a caller-owned staging belt and command encoder.
    /// Atlas mip writes use queue.write_texture, avoiding an extra intermediate
    /// GPU buffer while queuing writes for the next queue submission.
    pub fn upload_world_ref_staged(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        existing: &[Self],
        arena: &mut WorldGeometryArena,
        belt: &mut wgpu::util::StagingBelt,
        encoder: &mut wgpu::CommandEncoder,
        page: &Page,
    ) -> Self {
        let (normal_bg, reflected_bg) = if let Some(source) = page.shared_atlas {
            let source = existing
                .get(source)
                .expect("shared atlas must precede its geometry");
            (Arc::clone(&source.normal), Arc::clone(&source.reflected))
        } else {
            let image = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("whole-world object atlas"),
                size: wgpu::Extent3d {
                    width: SIDE as u32,
                    height: page.used_height as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 8,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let mut width = SIDE;
            let mut height = page.used_height;
            for mip in 0..8u32 {
                let level = if mip == 0 {
                    &page.atlas
                } else {
                    &page.mips[(mip - 1) as usize]
                };
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &image,
                        mip_level: mip,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    level,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some((width * 4) as u32),
                        rows_per_image: Some(height as u32),
                    },
                    wgpu::Extent3d {
                        width: width as u32,
                        height: height as u32,
                        depth_or_array_layers: 1,
                    },
                );
                width /= 2;
                height /= 2;
            }
            let view = image.create_view(&Default::default());
            let make = |buf: &wgpu::Buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("whole-world object material"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                })
            };
            (Arc::new(make(normal)), Arc::new(make(reflected)))
        };

        let vertex_data = bytemuck::cast_slice(&page.vertices);
        let index_data = bytemuck::cast_slice(&page.indices);
        let vertex_bytes = vertex_data.len() as u64;
        let index_offset = (vertex_bytes + 3) & !3;
        let index_bytes = index_data.len() as u64;
        let page_bytes = index_offset + index_bytes;
        let (geometry, vertex_offset) = arena.allocate(device, page_bytes);
        if page_bytes != 0 {
            // One staging allocation and one GPU copy per page. The former
            // path staged vertices and indices separately, doubling belt
            // bookkeeping and copy commands for every geometry page.
            let size = std::num::NonZeroU64::new(page_bytes).expect("non-empty page upload");
            let mut staged = belt.write_buffer(encoder, &geometry, vertex_offset, size, device);
            let vertex_end = vertex_data.len();
            staged[..vertex_end].copy_from_slice(vertex_data);
            if index_offset > vertex_bytes {
                staged[vertex_bytes as usize..index_offset as usize].fill(0);
            }
            let index_start = index_offset as usize;
            staged[index_start..index_start + index_data.len()].copy_from_slice(index_data);
        }
        Self {
            geometry,
            vertex_offset,
            vertex_bytes,
            index_offset: vertex_offset + index_offset,
            index_bytes,
            count: page.indices.len() as u32,
            normal: normal_bg,
            reflected: reflected_bg,
            placements: Vec::new(),
            bounds: Vec::new(),
            page_min: page.page_min,
            page_max: page.page_max,
            page_has_sdr: page.page_has_sdr,
        }
    }

    fn geometry(
        device: &wgpu::Device,
        page: Page,
        normal: Arc<wgpu::BindGroup>,
        reflected: Arc<wgpu::BindGroup>,
    ) -> Self {
        let vertex_data = bytemuck::cast_slice(&page.vertices);
        let index_data = bytemuck::cast_slice(&page.indices);
        let vertex_bytes = vertex_data.len() as u64;
        let index_offset = (vertex_bytes + 3) & !3;
        let geometry = create_page_geometry_buffer(device, vertex_data, index_data);
        Self {
            geometry: Arc::new(geometry),
            vertex_offset: 0,
            vertex_bytes,
            index_offset,
            index_bytes: index_data.len() as u64,
            count: page.indices.len() as u32,
            normal,
            reflected,
            placements: page.placements,
            bounds: page.bounds,
            page_min: page.page_min,
            page_max: page.page_max,
            page_has_sdr: page.page_has_sdr,
        }
    }
    /// Submits only the placements the recovered EC-BSD flags admit in this
    /// view. A page built without per-placement ranges is a diagnostic page:
    /// draw it in full, but say so instead of silently dropping it.
    pub fn draw_selected<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
    ) {
        pass.set_bind_group(
            0,
            if reflected {
                &self.reflected
            } else {
                &self.normal
            },
            &[],
        );
        pass.set_vertex_buffer(
            0,
            self.geometry
                .slice(self.vertex_offset..self.vertex_offset + self.vertex_bytes),
        );
        pass.set_index_buffer(
            self.geometry
                .slice(self.index_offset..self.index_offset + self.index_bytes),
            wgpu::IndexFormat::Uint32,
        );
        if self.placements.is_empty() {
            eprintln!("scenery page has no placement visibility ranges; drawing all indices");
            pass.draw_indexed(0..self.count, 0, 0..1);
            return;
        }
        // Fold visible placements directly into draw calls. Building a
        // temporary Vec here used to allocate once per page and render pass,
        // even though each span can be emitted as soon as its end is known.
        let mut span: Option<(u32, u32)> = None;
        for placement in &self.placements {
            if !allows_with_context(view, placement.visibility, RenderContext::default()) {
                continue;
            }
            match span {
                Some((start, end)) if end == placement.start => {
                    span = Some((start, placement.end));
                }
                Some((start, end)) => {
                    pass.draw_indexed(start..end, 0, 0..1);
                    span = Some((placement.start, placement.end));
                }
                None => span = Some((placement.start, placement.end)),
            }
        }
        if let Some((start, end)) = span {
            pass.draw_indexed(start..end, 0, 0..1);
        }
    }
    /// Freeze the original submission sequence once loading is complete.
    /// Texture sharing changes storage, never coplanar/depth-tie winners.
    pub fn draw_order(pages: &[Self]) -> Vec<(usize, usize)> {
        let cap: usize = pages.iter().map(|p| p.placements.len().max(1)).sum();
        let mut order = Vec::with_capacity(cap);
        for (page, p) in pages.iter().enumerate() {
            if p.placements.is_empty() {
                order.push((u64::MAX, page, usize::MAX));
            } else {
                for (range, r) in p.placements.iter().enumerate() {
                    order.push((r.order, page, range));
                }
            }
        }
        order.sort_unstable();
        order
            .into_iter()
            .map(|(_, page, range)| (page, range))
            .collect()
    }
    /// Commands for one view, built from the live pages.
    pub fn draw_commands(pages: &[Self], order: &[(usize, usize)], view: Pass) -> Vec<DrawCmd> {
        let spans: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.count,
                placements: &p.placements,
            })
            .collect();
        fold_commands(&spans, order, view)
    }
    /// The command lists the viewer keeps for the whole-world pages: the main
    /// (unfiltered) list, then the reflection and refraction gates.
    pub fn world_commands(pages: &[Self]) -> [Vec<DrawCmd>; WORLD_VIEWS] {
        let order = Self::draw_order(pages);
        [
            Self::draw_commands(pages, &order, Pass::Main),
            Self::draw_commands(pages, &order, Pass::Reflection),
            Self::draw_commands(pages, &order, Pass::Refraction),
        ]
    }
    /// Which `world_commands` list a pass reads. Fog, minimap and full-map see
    /// exactly what the main view sees, so they share its list.
    pub fn world_view_slot(pass: Pass) -> usize {
        match pass {
            Pass::Reflection => 1,
            Pass::Refraction => 2,
            Pass::Main | Pass::Fog | Pass::Minimap | Pass::FullMap => 0,
        }
    }
    /// Issue a command list: bind on a page change, one draw per command.
    pub fn draw_world<'a>(
        pages: &'a [Self],
        cmds: &[DrawCmd],
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
    ) {
        let mut bound = None;
        for cmd in cmds {
            let p = &pages[cmd.page];
            if bound != Some(cmd.page) {
                pass.set_bind_group(0, if reflected { &p.reflected } else { &p.normal }, &[]);
                pass.set_vertex_buffer(
                    0,
                    p.geometry
                        .slice(p.vertex_offset..p.vertex_offset + p.vertex_bytes),
                );
                pass.set_index_buffer(
                    p.geometry
                        .slice(p.index_offset..p.index_offset + p.index_bytes),
                    wgpu::IndexFormat::Uint32,
                );
                bound = Some(cmd.page);
            }
            pass.draw_indexed(cmd.start..cmd.end, 0, 0..1);
        }
    }

    pub fn world_culled_commands(pages: &[Self]) -> [WorldDrawList; WORLD_VIEWS] {
        let metadata = pages
            .iter()
            .map(WorldPageMeta::from_gpu)
            .collect::<Vec<_>>();
        world_culled_commands_from_meta(&metadata)
    }

    /// Encode conservative GPU visibility for a large stable world-page list.
    /// The resulting indexed-indirect records preserve the list's exact global
    /// order; commands that are not proven visible receive instance_count=0.
    /// Unsupported adapters, small lists and all-inside fast paths retain CPU culling.
    pub fn encode_world_culling(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        list: &WorldDrawList,
        cull: &CullParams,
        view_slot: usize,
        sdr_only: bool,
        multi_draw: bool,
        scratch: &mut WorldDrawScratch,
    ) -> bool {
        if view_slot >= WORLD_VIEWS {
            return false;
        }
        if let Some(view) = scratch
            .gpu_culler
            .as_mut()
            .and_then(|culler| culler.views[view_slot].as_mut())
        {
            view.ready = false;
        }
        // GPU compaction needs the native indirect-count feature; otherwise
        // retain the allocation-free CPU hierarchy and direct/ordinary MDI path.
        // Small lists and the all-inside opaque fast path are cheaper on CPU.
        if !multi_draw
            || list.cmds.len() < 64
            || list.chunks.is_empty()
            || (!sdr_only
                && cull.clip_plane.is_none()
                && list
                    .chunks
                    .iter()
                    .all(|chunk| aabb_inside_frustum(&cull.frustum, chunk.min, chunk.max)))
        {
            return false;
        }
        let run_count = 1 + list
            .cmds
            .windows(2)
            .filter(|pair| pair[0].page != pair[1].page)
            .count();
        if run_count > u32::MAX as usize {
            return false;
        }
        let workgroups = run_count as u32;
        let limits = device.limits();
        let bounds_bytes = (list.cmds.len() as u64)
            .saturating_mul(std::mem::size_of::<WorldGpuCullBounds>() as u64);
        let args_bytes = (list.cmds.len() as u64).saturating_mul(PAGE_DRAW_ARGS_SIZE);
        let run_bytes =
            (run_count as u64).saturating_mul(std::mem::size_of::<WorldGpuCullRun>() as u64);
        let count_bytes = (run_count as u64).saturating_mul(std::mem::size_of::<u32>() as u64);
        let binding_limit = u64::from(limits.max_storage_buffer_binding_size);
        if bounds_bytes > binding_limit
            || args_bytes > binding_limit
            || run_bytes > binding_limit
            || count_bytes > binding_limit
            || bounds_bytes > limits.max_buffer_size
            || args_bytes > limits.max_buffer_size
            || run_bytes > limits.max_buffer_size
            || count_bytes > limits.max_buffer_size
            || workgroups > limits.max_compute_workgroups_per_dimension
        {
            return false;
        }

        let culler = scratch
            .gpu_culler
            .get_or_insert_with(|| WorldGpuCuller::new(device));
        let rebuild = culler.views[view_slot].as_ref().map_or(true, |view| {
            view.commands.as_slice() != list.cmds.as_slice()
        });
        if rebuild {
            let Some(view) = WorldGpuCullView::new(device, &culler.layout, &list.cmds) else {
                return false;
            };
            culler.views[view_slot] = Some(view);
        }

        let view = culler.views[view_slot]
            .as_mut()
            .expect("world cull view was just created");
        let params = WorldGpuCullParams::new(cull, view.runs.len(), sdr_only);
        queue.write_buffer(&view.params, 0, bytemuck::bytes_of(&params));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("whole-world placement visibility compaction"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&culler.pipeline);
            pass.set_bind_group(0, &view.bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        view.ready = true;
        true
    }

    pub fn draw_world_culled<'a>(
        pages: &'a [Self],
        list: &WorldDrawList,
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        cull: &CullParams,
        sdr_only: bool,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        multi_draw: bool,
        view_slot: usize,
        scratch: &'a mut WorldDrawScratch,
    ) {
        if pages.is_empty() || list.chunks.is_empty() {
            return;
        }
        // A successful compute dispatch stably compacts visible commands within
        // each page run. The indirect count buffer prevents invisible records
        // from reaching the driver, while page and global run order stay intact.
        let gpu_view = scratch
            .gpu_culler
            .as_ref()
            .and_then(|culler| culler.views.get(view_slot))
            .and_then(Option::as_ref)
            .filter(|view| view.ready && view.commands.as_slice() == list.cmds.as_slice());
        if let Some(view) = gpu_view {
            let mut bound_page: Option<usize> = None;
            for (run_index, run) in view.runs.iter().enumerate() {
                if bound_page != Some(run.page) {
                    let page = &pages[run.page];
                    let same_atlas =
                        bound_page.is_some_and(|prev| pages[prev].shares_atlas_with(page));
                    if !same_atlas {
                        pass.set_bind_group(
                            0,
                            if reflected {
                                &page.reflected
                            } else {
                                &page.normal
                            },
                            &[],
                        );
                    }
                    pass.set_vertex_buffer(
                        0,
                        page.geometry
                            .slice(page.vertex_offset..page.vertex_offset + page.vertex_bytes),
                    );
                    pass.set_index_buffer(
                        page.geometry
                            .slice(page.index_offset..page.index_offset + page.index_bytes),
                        wgpu::IndexFormat::Uint32,
                    );
                    bound_page = Some(run.page);
                }
                let offset = u64::from(run.first_record) * PAGE_DRAW_ARGS_SIZE;
                let count_offset = (run_index as u64) * std::mem::size_of::<u32>() as u64;
                pass.multi_draw_indexed_indirect_count(
                    &view.indirect,
                    offset,
                    &view.counts,
                    count_offset,
                    run.record_count,
                );
            }
            return;
        }
        // When every 32-command chunk is wholly inside the main frustum, the
        // chunk bounds already prove that every command is inside it. Avoid the
        // page-AABB pass and the repeated chunk/placement frustum tests in this
        // common "whole world fits on screen" view. Keep the per-command
        // projected-size test, so this fast path changes no visible-detail
        // threshold; it only removes redundant hierarchy checks.
        let all_chunks_inside = cull.clip_plane.is_none()
            && !sdr_only
            && list
                .chunks
                .iter()
                .all(|chunk| aabb_inside_frustum(&cull.frustum, chunk.min, chunk.max));

        scratch.cmds.clear();
        if all_chunks_inside {
            for cmd in &list.cmds {
                if cmd.start < cmd.end
                    && aabb_survives_screen_size(
                        cull.eye,
                        cull.proj_scale,
                        cull.min_screen_px,
                        cmd.min,
                        cmd.max,
                    )
                {
                    push_coalesced_draw(
                        &mut scratch.cmds,
                        cmd.page,
                        cmd.start,
                        cmd.end,
                        MAX_COALESCE_GAP,
                    );
                }
            }
        } else {
            // 1. Hierarchical page-level rejection: test each GpuPage AABB once
            // against clip plane, frustum, and sub-pixel size into a reused buffer.
            scratch.page_visible.clear();
            scratch.page_visible.reserve(pages.len());
            let mut any_page_visible = false;
            for page in pages {
                let vis = page.count > 0
                    && (!sdr_only || page.page_has_sdr)
                    && aabb_survives_clip(cull.clip_plane, page.page_min, page.page_max)
                    && aabb_in_frustum(&cull.frustum, page.page_min, page.page_max)
                    && aabb_survives_screen_size(
                        cull.eye,
                        cull.proj_scale,
                        cull.min_screen_px,
                        page.page_min,
                        page.page_max,
                    );
                any_page_visible |= vis;
                scratch.page_visible.push(vis);
            }
            if !any_page_visible {
                return;
            }

            // 2. Chunk + placement culling and aggressive gap-bridging draw-call
            // coalescing into the persistent scratch command buffer (zero allocs).
            let max_gap = if sdr_only { 0 } else { MAX_COALESCE_GAP };
            for chunk in &list.chunks {
                if (sdr_only && !chunk.has_sdr)
                    || !aabb_survives_clip(cull.clip_plane, chunk.min, chunk.max)
                    || !aabb_in_frustum(&cull.frustum, chunk.min, chunk.max)
                    || !aabb_survives_screen_size(
                        cull.eye,
                        cull.proj_scale,
                        cull.min_screen_px,
                        chunk.min,
                        chunk.max,
                    )
                {
                    continue;
                }
                let fully_inside = aabb_inside_frustum(&cull.frustum, chunk.min, chunk.max);
                for cmd in &list.cmds[chunk.start..chunk.end] {
                    if !scratch.page_visible[cmd.page]
                        || (sdr_only && !cmd.has_sdr)
                        || cmd.start >= cmd.end
                    {
                        continue;
                    }
                    if !aabb_survives_clip(cull.clip_plane, cmd.min, cmd.max) {
                        continue;
                    }
                    if !fully_inside && !aabb_in_frustum(&cull.frustum, cmd.min, cmd.max) {
                        continue;
                    }
                    if !aabb_survives_screen_size(
                        cull.eye,
                        cull.proj_scale,
                        cull.min_screen_px,
                        cmd.min,
                        cmd.max,
                    ) {
                        continue;
                    }
                    push_coalesced_draw(&mut scratch.cmds, cmd.page, cmd.start, cmd.end, max_gap);
                }
            }
        }

        // 3. Group adjacent commands that already share a page. Their order
        // remains unchanged (including interleaved pages), but a run with two
        // or more commands can now be issued by one native multi-draw call.
        // Keep the original direct path when the optional device feature or
        // an appropriately sized indirect buffer is unavailable.
        let mut use_multi_draw = false;
        if multi_draw && scratch.cmds.len() > 1 {
            scratch.indirect_args.clear();
            scratch.indirect_runs.clear();
            for (cmd_index, cmd) in scratch.cmds.iter().enumerate() {
                let record_index = scratch.indirect_args.len() as u32;
                scratch.indirect_args.push(PageDrawIndexedArgs {
                    index_count: cmd.end - cmd.start,
                    instance_count: 1,
                    first_index: cmd.start,
                    base_vertex: 0,
                    first_instance: 0,
                });
                match scratch.indirect_runs.last_mut() {
                    Some(run) if run.page == cmd.page => {
                        run.record_count += 1;
                    }
                    _ => scratch.indirect_runs.push(WorldIndirectRun {
                        page: cmd.page,
                        first_record: record_index,
                        record_count: 1,
                        first_cmd: cmd_index,
                    }),
                }
            }

            // A multi-draw run only helps when at least two adjacent commands
            // share the same page. Avoid allocating/writing an indirect buffer
            // for a list whose page boundaries already force one draw per run.
            if scratch.indirect_runs.iter().any(|run| run.record_count > 1) {
                let slot = usize::from(sdr_only);
                let required_bytes =
                    (scratch.indirect_args.len() as u64).saturating_mul(PAGE_DRAW_ARGS_SIZE);
                let current_bytes = scratch.indirect_buffers[slot]
                    .as_ref()
                    .map_or(0, wgpu::Buffer::size);
                if let Some(capacity) = page_indirect_capacity(
                    current_bytes,
                    required_bytes,
                    device.limits().max_buffer_size,
                ) {
                    if capacity != current_bytes {
                        scratch.indirect_buffers[slot] =
                            Some(device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some(if sdr_only {
                                    "whole-world SDR indirect commands"
                                } else {
                                    "whole-world opaque indirect commands"
                                }),
                                size: capacity,
                                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                                mapped_at_creation: false,
                            }));
                    }
                    let buffer = scratch.indirect_buffers[slot]
                        .as_ref()
                        .expect("whole-world indirect buffer was just allocated");
                    queue.write_buffer(buffer, 0, bytemuck::cast_slice(&scratch.indirect_args));
                    use_multi_draw = true;
                }
            }
        }

        // 4. Submit in the same stable page/command order. The vertex and index
        // bindings still change only at page boundaries. A one-command run is
        // cheaper as a direct draw; larger runs use one multi-draw submission.
        let indirect_buffer = if use_multi_draw {
            scratch.indirect_buffers[usize::from(sdr_only)].as_ref()
        } else {
            None
        };
        let mut bound_page: Option<usize> = None;
        if let Some(indirect_buffer) = indirect_buffer {
            for run in &scratch.indirect_runs {
                if bound_page != Some(run.page) {
                    let page = &pages[run.page];
                    let same_atlas =
                        bound_page.is_some_and(|prev| pages[prev].shares_atlas_with(page));
                    if !same_atlas {
                        pass.set_bind_group(
                            0,
                            if reflected {
                                &page.reflected
                            } else {
                                &page.normal
                            },
                            &[],
                        );
                    }
                    pass.set_vertex_buffer(
                        0,
                        page.geometry
                            .slice(page.vertex_offset..page.vertex_offset + page.vertex_bytes),
                    );
                    pass.set_index_buffer(
                        page.geometry
                            .slice(page.index_offset..page.index_offset + page.index_bytes),
                        wgpu::IndexFormat::Uint32,
                    );
                    bound_page = Some(run.page);
                }
                if run.record_count > 1 {
                    pass.multi_draw_indexed_indirect(
                        indirect_buffer,
                        u64::from(run.first_record) * PAGE_DRAW_ARGS_SIZE,
                        run.record_count,
                    );
                } else {
                    let cmd = &scratch.cmds[run.first_cmd];
                    pass.draw_indexed(cmd.start..cmd.end, 0, 0..1);
                }
            }
        } else {
            for cmd in &scratch.cmds {
                if bound_page != Some(cmd.page) {
                    let page = &pages[cmd.page];
                    let same_atlas =
                        bound_page.is_some_and(|prev| pages[prev].shares_atlas_with(page));
                    if !same_atlas {
                        pass.set_bind_group(
                            0,
                            if reflected {
                                &page.reflected
                            } else {
                                &page.normal
                            },
                            &[],
                        );
                    }
                    pass.set_vertex_buffer(
                        0,
                        page.geometry
                            .slice(page.vertex_offset..page.vertex_offset + page.vertex_bytes),
                    );
                    pass.set_index_buffer(
                        page.geometry
                            .slice(page.index_offset..page.index_offset + page.index_bytes),
                        wgpu::IndexFormat::Uint32,
                    );
                    bound_page = Some(cmd.page);
                }
                pass.draw_indexed(cmd.start..cmd.end, 0, 0..1);
            }
        }
    }

    pub fn draw_selected_culled<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
        cull: &CullParams,
        sdr_only: bool,
    ) {
        if self.count == 0 {
            return;
        }
        if sdr_only && !self.page_has_sdr {
            return;
        }
        if !aabb_survives_clip(cull.clip_plane, self.page_min, self.page_max)
            || !aabb_in_frustum(&cull.frustum, self.page_min, self.page_max)
            || !aabb_survives_screen_size(
                cull.eye,
                cull.proj_scale,
                cull.min_screen_px,
                self.page_min,
                self.page_max,
            )
        {
            return;
        }
        pass.set_bind_group(
            0,
            if reflected {
                &self.reflected
            } else {
                &self.normal
            },
            &[],
        );
        pass.set_vertex_buffer(
            0,
            self.geometry
                .slice(self.vertex_offset..self.vertex_offset + self.vertex_bytes),
        );
        pass.set_index_buffer(
            self.geometry
                .slice(self.index_offset..self.index_offset + self.index_bytes),
            wgpu::IndexFormat::Uint32,
        );
        if self.placements.is_empty() {
            pass.draw_indexed(0..self.count, 0, 0..1);
            return;
        }
        let fully_inside = aabb_inside_frustum(&cull.frustum, self.page_min, self.page_max);
        let max_gap = if sdr_only { 0 } else { MAX_COALESCE_GAP };
        let mut cursor = None::<(u32, u32)>;
        for (r, b) in self.placements.iter().zip(&self.bounds) {
            if sdr_only && !b.has_sdr {
                continue;
            }
            if !allows_with_context(view, r.visibility, RenderContext::default()) {
                continue;
            }
            if !aabb_survives_clip(cull.clip_plane, b.min, b.max) {
                continue;
            }
            if !fully_inside && !aabb_in_frustum(&cull.frustum, b.min, b.max) {
                continue;
            }
            if !aabb_survives_screen_size(
                cull.eye,
                cull.proj_scale,
                cull.min_screen_px,
                b.min,
                b.max,
            ) {
                continue;
            }
            let start = r.start.min(self.count);
            let end = r.end.min(self.count);
            if start >= end {
                continue;
            }
            match &mut cursor {
                Some((_, prev_end)) if start >= *prev_end && start - *prev_end <= max_gap => {
                    *prev_end = (*prev_end).max(end);
                }
                Some((prev_start, prev_end)) => {
                    pass.draw_indexed(*prev_start..*prev_end, 0, 0..1);
                    cursor = Some((start, end));
                }
                None => cursor = Some((start, end)),
            }
        }
        if let Some((start, end)) = cursor {
            pass.draw_indexed(start..end, 0, 0..1);
        }
    }
    /// Convenience wrapper for callers that hold the raw order (the atlas
    /// check): build the view's commands, then draw them.
    pub fn draw_ordered<'a>(
        pages: &'a [Self],
        order: &[(usize, usize)],
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
    ) {
        let cmds = Self::draw_commands(pages, order, view);
        Self::draw_world(pages, &cmds, pass, reflected);
    }
    pub fn shares_atlas_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.normal, &other.normal) && Arc::ptr_eq(&self.reflected, &other.reflected)
    }
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, reflected: bool) {
        self.draw_selected(pass, reflected, Pass::Main);
    }
}

fn world_culled_commands_from_meta(pages: &[WorldPageMeta]) -> [WorldDrawList; WORLD_VIEWS] {
    let mut order = Vec::new();
    for (page_idx, page) in pages.iter().enumerate() {
        if page.placements.is_empty() {
            order.push((u64::MAX, page_idx, usize::MAX));
        } else {
            for (range_idx, range) in page.placements.iter().enumerate() {
                order.push((range.order, page_idx, range_idx));
            }
        }
    }
    order.sort_unstable();
    let build = |view: Pass| -> WorldDrawList {
        let mut out: Vec<WorldDrawCmd> = Vec::new();
        for &(_, page_idx, range_idx) in &order {
            let page = &pages[page_idx];
            if page.count == 0 {
                continue;
            }
            if range_idx == usize::MAX {
                out.push(WorldDrawCmd {
                    page: page_idx,
                    start: 0,
                    end: page.count,
                    min: page.page_min,
                    max: page.page_max,
                    has_sdr: page.page_has_sdr,
                });
                continue;
            }
            let range = &page.placements[range_idx];
            if !allows_with_context(view, range.visibility, RenderContext::default()) {
                continue;
            }
            let start = range.start.min(page.count);
            let end = range.end.min(page.count);
            if start >= end {
                continue;
            }
            let bound = page.bounds[range_idx];
            if let Some(previous) = out.last_mut() {
                if previous.page == page_idx && previous.end == start {
                    let min = [
                        previous.min[0].min(bound.min[0]),
                        previous.min[1].min(bound.min[1]),
                        previous.min[2].min(bound.min[2]),
                    ];
                    let max = [
                        previous.max[0].max(bound.max[0]),
                        previous.max[1].max(bound.max[1]),
                        previous.max[2].max(bound.max[2]),
                    ];
                    if max[0] - min[0] <= 128.0
                        && max[1] - min[1] <= 128.0
                        && max[2] - min[2] <= 128.0
                    {
                        previous.end = end;
                        previous.min = min;
                        previous.max = max;
                        previous.has_sdr |= bound.has_sdr;
                        continue;
                    }
                }
            }
            out.push(WorldDrawCmd {
                page: page_idx,
                start,
                end,
                min: bound.min,
                max: bound.max,
                has_sdr: bound.has_sdr,
            });
        }
        let mut chunks = Vec::with_capacity(out.len().div_ceil(32));
        for (i, group) in out.chunks(32).enumerate() {
            let start = i * 32;
            let end = start + group.len();
            let mut min = group[0].min;
            let mut max = group[0].max;
            let mut has_sdr = group[0].has_sdr;
            for cmd in &group[1..] {
                for axis in 0..3 {
                    min[axis] = min[axis].min(cmd.min[axis]);
                    max[axis] = max[axis].max(cmd.max[axis]);
                }
                has_sdr |= cmd.has_sdr;
            }
            chunks.push(WorldDrawChunk {
                min,
                max,
                has_sdr,
                start,
                end,
            });
        }
        WorldDrawList { cmds: out, chunks }
    };
    if order.len() >= 1024 {
        std::thread::scope(|s| {
            let main_t = s.spawn(|| build(Pass::Main));
            let refl_t = s.spawn(|| build(Pass::Reflection));
            let refr = build(Pass::Refraction);
            [main_t.join().unwrap(), refl_t.join().unwrap(), refr]
        })
    } else {
        [
            build(Pass::Main),
            build(Pass::Reflection),
            build(Pass::Refraction),
        ]
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;
    fn solid(fill: u8, side: usize) -> crate::objects::Texture {
        crate::objects::Texture {
            width: side,
            height: side,
            rgba: vec![fill; side * side * 4],
        }
    }
    #[test]
    fn emitted_atlas_reuses_slots_without_pixels_or_mip_allocations() {
        let mut original = Builder::new();
        let texture = solid(71, 128);
        let key = TextureKey::Content(71);
        let (slot, _) = allocate_slot(&mut original, key, &texture).unwrap();
        let mut slots = HashMap::new();
        remember_atlas(&original, 3, &mut slots);
        let reference = slots[&key];
        assert_eq!(reference.source, 3);
        assert_eq!(reference.height, 128);
        assert_eq!(reference.slot.x, slot.x);
        let vertex = Vertex {
            pos: [0.; 3],
            normal: [0.; 3],
            uv: [0.01, 0.015625],
            color: [0., 0., 0.03125, 0.03125],
            kind: 2.,
            diffuse: 0,
            night_diffuse: 0,
        };
        let mut geometry = Builder::shared(reference.source, reference.height);
        geometry.vertices.push(vertex);
        geometry.indices.push(0);
        let page = geometry.page(32);
        assert_eq!(page.shared_atlas, Some(3));
        assert!(page.atlas.is_empty() && page.mips.is_empty());
        assert_eq!(page.vertices[0].uv[1], 0.5);
        assert_eq!(page.vertices[0].color[3], 1.);
        // Geometry chunks no longer multiply the source texture's pixel cost.
        assert_eq!(
            atlas_mip_bytes(128),
            (0..8).map(|l| (4096 >> l) * (128 >> l) * 4).sum::<usize>()
        );
    }
    #[test]
    fn integer_blit_preserves_center_nearest_texture_sampling() {
        let texture = crate::objects::Texture {
            width: 3,
            height: 2,
            rgba: (0u8..24).collect(),
        };
        let mut page = Builder::new();
        let (slot, _) = allocate_slot(&mut page, TextureKey::Content(7), &texture).unwrap();
        for (x, y) in [(0, 0), (1, 1), (63, 17), (127, 127)] {
            let expected = texture.sample_rgba(
                (x as f32 + 0.5) / slot.w as f32,
                (y as f32 + 0.5) / slot.h as f32,
            );
            let offset = ((slot.y + y) * SIDE + slot.x + x) * 4;
            assert_eq!(&page.atlas[offset..offset + 4], &expected);
        }
    }
    #[test]
    fn content_dedup_reduces_page_turns() {
        // 512x512 textures occupy 16 atlas cells each; a 4096^2 page holds
        // 1024 cells = 64 of them. Push 128 references through both keying
        // schemes and count page turns.
        let textures: Vec<_> = (0..64).map(|i| solid(i as u8 + 1, 512)).collect();
        // Content-keyed: the 128 references are 64 contents twice, so every
        // second reference reuses a slot and ONE page suffices.
        let mut page = Builder::new();
        page.indices.push(0);
        let mut dedup_turns = 0;
        for i in 0..128 {
            let tex = &textures[i % 64];
            let (_slot, turned) =
                allocate_slot(&mut page, TextureKey::Content((i % 64) as u64), tex).unwrap();
            if turned.is_some() {
                dedup_turns += 1;
            }
        }
        assert_eq!(dedup_turns, 0);
        assert_eq!(page.slots.len(), 64);
        // Name-keyed with 65 distinct contents: the 65th overflows the page
        // and forces a turn — exactly what content dedup avoids when the
        // extra references are aliases.
        let mut page = Builder::new();
        page.indices.push(0);
        let mut unique_turns = 0;
        let extra = solid(200, 512);
        for i in 0..65 {
            let tex = if i < 64 { &textures[i] } else { &extra };
            let (_slot, turned) =
                allocate_slot(&mut page, TextureKey::Content(1000 + i as u64), tex).unwrap();
            if turned.is_some() {
                unique_turns += 1;
            }
        }
        assert_eq!(unique_turns, 1);
        assert!(dedup_turns < unique_turns);
    }
    #[test]
    fn missing_references_share_one_fallback_slot() {
        let mut page = Builder::new();
        page.indices.push(0);
        let fallback = solid(160, 1);
        for _ in 0..3 {
            let (_slot, turned) = allocate_slot(&mut page, TextureKey::Missing, &fallback).unwrap();
            assert!(turned.is_none());
        }
        assert_eq!(page.slots.len(), 1);
    }
    #[test]
    fn page_bounds_use_recorded_mesh_vertex_ranges() {
        let vertex = |pos, kind| Vertex {
            pos,
            normal: [0., 1., 0.],
            uv: [0., 0.],
            color: [0.; 4],
            kind,
            diffuse: 0xffff_ffff,
            night_diffuse: 0xffff_ffff,
        };
        let page = Page {
            vertices: vec![
                vertex([-12., 3., 7.], 2.02),
                vertex([4., -5., 9.], 2.02),
                vertex([8., 6., -2.], 2.02),
            ],
            // The mesh's referenced index range is deliberately tiny; the
            // builder's vertex range is authoritative and conservative.
            indices: vec![1],
            atlas: Vec::new(),
            shared_atlas: Some(0),
            processed: 0,
            used_height: 128,
            mips: Vec::new(),
            placements: vec![PlacementRange {
                start: 0,
                end: 1,
                vertex_start: 0,
                vertex_end: 3,
                export_id: 1,
                order: 0,
                visibility: Visibility {
                    reflect: true,
                    refract: true,
                },
            }],
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish();

        assert_eq!(page.bounds.len(), 1);
        assert_eq!(page.bounds[0].min, [-12., -5., -2.]);
        assert_eq!(page.bounds[0].max, [8., 6., 9.]);
        assert!(page.bounds[0].has_sdr);
        assert!(page.page_has_sdr);
        assert_eq!(page.page_min, page.bounds[0].min);
        assert_eq!(page.page_max, page.bounds[0].max);
    }

    #[test]
    fn trim_scales_y_and_builds_independent_mips() {
        let vertex = Vertex {
            pos: [0.; 3],
            normal: [0.; 3],
            uv: [0.25, 0.03125],
            color: [0., 0.03125, 0.125, 0.03125],
            kind: 2.,
            diffuse: 0xffffffff,
            night_diffuse: 0xffff_ffff,
        };
        let page = Page {
            shared_atlas: None,
            vertices: vec![vertex],
            indices: vec![0],
            atlas: vec![255; SIDE * 256 * 4],
            processed: 0,
            used_height: 256,
            mips: Vec::new(),
            placements: Vec::new(),
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish();
        assert_eq!(page.mips.len(), 7);
        assert_eq!(page.atlas.len(), SIDE * 256 * 4);
        assert_eq!(page.vertices[0].uv[1], 0.5);
        assert_eq!(page.vertices[0].color[3], 0.5);
        assert_eq!(page.mips[6].len(), (SIDE / 128) * 2 * 4);
    }
    /// The streamer grows a page atlas as its rectangles are packed instead of
    /// allocating 4096 square up front, so a page arrives with a buffer that is
    /// exactly as tall as the rows it used. `finish` must hand the upload a
    /// buffer of exactly the extent it copies, and keep all seven mips valid.
    #[test]
    fn a_page_grown_to_its_used_rows_uploads_exactly_its_extent() {
        let rows = SIDE / 8;
        let page = Page {
            shared_atlas: None,
            vertices: Vec::new(),
            indices: Vec::new(),
            atlas: vec![17; SIDE * rows * 4],
            processed: 0,
            used_height: rows,
            mips: Vec::new(),
            placements: Vec::new(),
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish();
        assert_eq!(page.atlas.len(), SIDE * rows * 4);
        assert_eq!(page.mips.len(), 7);
        let (mut w, mut h) = (SIDE, rows);
        for level in 0..7 {
            assert_eq!(page.mips[level].len(), (w / 2) * (h / 2) * 4, "mip {level}");
            w /= 2;
            h /= 2;
        }
    }

    /// A page whose packer reserved less than its reported height — the short
    /// buffer an incremental grow can leave behind — is padded rather than read
    /// past the end.
    #[test]
    fn a_short_page_atlas_is_padded_to_its_upload_extent() {
        let rows = 256;
        let page = Page {
            shared_atlas: None,
            vertices: Vec::new(),
            indices: Vec::new(),
            atlas: vec![3; SIDE * 128 * 4],
            processed: 0,
            used_height: rows,
            mips: Vec::new(),
            placements: Vec::new(),
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish();
        assert_eq!(page.atlas.len(), SIDE * rows * 4);
        assert!(page.atlas[SIDE * 128 * 4..].iter().all(|&v| v == 0));
    }

    /// A placement span as the builder writes them: touching, in order.
    fn span(start: u32, end: u32, order: u64, reflect: bool, refract: bool) -> PlacementRange {
        PlacementRange {
            start,
            end,
            vertex_start: 0,
            vertex_end: 0,
            export_id: 0,
            order,
            visibility: Visibility { reflect, refract },
        }
    }

    fn cmd(page: usize, start: u32, end: u32) -> DrawCmd {
        DrawCmd { page, start, end }
    }

    fn pages<'a>(spans: &[&'a [PlacementRange]], counts: &[u32]) -> Vec<PageSpans<'a>> {
        spans
            .iter()
            .zip(counts)
            .map(|(placements, count)| PageSpans {
                count: *count,
                placements: *placements,
            })
            .collect()
    }

    /// Every index a command list draws, expanded back out: the property the
    /// fold has to preserve, whatever it does to the command count.
    fn drawn(cmds: &[DrawCmd]) -> Vec<(usize, u32)> {
        cmds.iter()
            .flat_map(|c| (c.start..c.end).map(move |i| (c.page, i)))
            .collect()
    }

    #[test]
    fn scissor_frustum_rejects_geometry_outside_the_water_sample_rectangle() {
        let planes = frustum_planes_from_matrix_rect(
            glam::Mat4::IDENTITY.to_cols_array(),
            [-0.5, 0.5, -0.25, 0.75],
        );
        assert!(aabb_in_frustum(&planes, [-0.1, -0.1, 0.1], [0.1, 0.1, 0.9]));
        assert!(!aabb_in_frustum(&planes, [0.6, -0.1, 0.1], [0.8, 0.1, 0.9]));
        assert!(!aabb_in_frustum(&planes, [-0.1, 0.8, 0.1], [0.1, 0.9, 0.9]));
    }

    #[test]
    fn a_page_run_folds_into_one_draw() {
        // Four touching placements of one page, submitted back to back, are one
        // draw. The whole-world pass used to issue four.
        let p = [
            span(0, 30, 10, true, true),
            span(30, 60, 11, true, true),
            span(60, 100, 12, true, true),
            span(100, 140, 13, true, true),
        ];
        let pages = pages(&[&p], &[140]);
        let order = [(0, 0), (0, 1), (0, 2), (0, 3)];
        let cmds = fold_commands(&pages, &order, Pass::Main);
        assert_eq!(cmds, vec![cmd(0, 0, 140)]);
        assert_eq!(drawn(&cmds).len(), 140);
    }

    #[test]
    fn a_local_page_folds_its_visible_placements_and_skips_the_hidden_ones() {
        let p = [
            span(0, 30, 0, true, true),
            span(30, 60, 1, false, true),
            span(60, 100, 2, true, true),
            span(100, 140, 3, true, true),
        ];
        assert_eq!(fold_page_spans(&p, Pass::Main), vec![(0, 140)]);
        assert_eq!(
            fold_page_spans(&p, Pass::Reflection),
            vec![(0, 30), (60, 140)]
        );
        assert_eq!(fold_page_spans(&p, Pass::Refraction), vec![(0, 140)]);
        // The hidden placement's indices stay out of the reflection spans.
        let drawn: u32 = fold_page_spans(&p, Pass::Reflection)
            .iter()
            .map(|(start, end)| end - start)
            .sum();
        assert_eq!(drawn, 30 + 80);
    }

    #[test]
    fn a_hidden_placement_splits_the_run_and_is_never_drawn() {
        let p = [
            span(0, 30, 10, true, true),
            span(30, 60, 11, false, true),
            span(60, 100, 12, true, true),
            span(100, 140, 13, true, true),
        ];
        let pages = pages(&[&p], &[140]);
        let order = [(0, 0), (0, 1), (0, 2), (0, 3)];
        // Reflection hides the second placement, so no span may cross it.
        let reflection = fold_commands(&pages, &order, Pass::Reflection);
        assert_eq!(reflection, vec![cmd(0, 0, 30), cmd(0, 60, 140)]);
        assert!(!drawn(&reflection)
            .iter()
            .any(|&(_, i)| (30..60).contains(&i)));
        // Refraction sees all four and folds them into one draw.
        let refraction = fold_commands(&pages, &order, Pass::Refraction);
        assert_eq!(refraction, vec![cmd(0, 0, 140)]);
    }

    #[test]
    fn interleaved_pages_keep_their_own_draws() {
        // The frozen order is a global sort, so pages interleave. A fold never
        // crosses a page boundary and the drawn sequence stays as submitted.
        let a = [span(0, 30, 10, true, true), span(30, 60, 30, true, true)];
        let b = [span(0, 40, 20, true, true)];
        let pages = pages(&[&a, &b], &[60, 40]);
        let order = [(0, 0), (1, 0), (0, 1)];
        let cmds = fold_commands(&pages, &order, Pass::Main);
        assert_eq!(cmds, vec![cmd(0, 0, 30), cmd(1, 0, 40), cmd(0, 30, 60)]);
        assert_eq!(drawn(&cmds).len(), 100);
    }

    #[test]
    fn a_page_without_placements_draws_every_index() {
        let pages = pages(&[&[]], &[512]);
        let cmds = fold_commands(&pages, &[(0, usize::MAX)], Pass::Reflection);
        assert_eq!(cmds, vec![cmd(0, 0, 512)]);
    }

    #[test]
    fn whole_world_fast_path_requires_every_chunk_inside_frustum() {
        let cube = [
            [1., 0., 0., 10.],
            [-1., 0., 0., 10.],
            [0., 1., 0., 10.],
            [0., -1., 0., 10.],
            [0., 0., 1., 10.],
            [0., 0., -1., 10.],
        ];
        let inside = WorldDrawChunk {
            min: [-5., -5., -5.],
            max: [5., 5., 5.],
            has_sdr: false,
            start: 0,
            end: 1,
        };
        let outside = WorldDrawChunk {
            min: [11., -1., -1.],
            max: [12., 1., 1.],
            has_sdr: false,
            start: 1,
            end: 2,
        };
        assert!(all_chunks_inside_frustum(&[inside], &cube));
        assert!(!all_chunks_inside_frustum(&[inside, outside], &cube));
        assert!(!all_chunks_inside_frustum(&[], &cube));
    }

    #[test]
    fn indirect_buffer_capacity_is_reused_and_only_grows_when_needed() {
        assert_eq!(page_indirect_capacity(1024, 512, 4096), Some(1024));
        assert_eq!(page_indirect_capacity(1024, 1024, 4096), Some(1024));
        assert_eq!(page_indirect_capacity(1024, 1536, 4096), Some(2048));
        assert_eq!(page_indirect_capacity(0, 1536, 4096), Some(1536));
        assert_eq!(page_indirect_capacity(1024, 4097, 4096), None);
        assert_eq!(page_indirect_capacity(1024, 0, 4096), None);
    }

    #[test]
    fn indirect_page_runs_preserve_command_order_and_record_contents() {
        let cmds = [
            DrawCmd {
                page: 0,
                start: 0,
                end: 12,
            },
            DrawCmd {
                page: 0,
                start: 12,
                end: 30,
            },
            DrawCmd {
                page: 1,
                start: 4,
                end: 10,
            },
            DrawCmd {
                page: 0,
                start: 30,
                end: 42,
            },
            DrawCmd {
                page: 1,
                start: 10,
                end: 22,
            },
            DrawCmd {
                page: 1,
                start: 22,
                end: 40,
            },
        ];
        let mut args = Vec::new();
        let mut runs = Vec::<WorldIndirectRun>::new();
        for (cmd_index, cmd) in cmds.iter().enumerate() {
            let record_index = args.len() as u32;
            args.push(PageDrawIndexedArgs {
                index_count: cmd.end - cmd.start,
                instance_count: 1,
                first_index: cmd.start,
                base_vertex: 0,
                first_instance: 0,
            });
            match runs.last_mut() {
                Some(run) if run.page == cmd.page => run.record_count += 1,
                _ => runs.push(WorldIndirectRun {
                    page: cmd.page,
                    first_record: record_index,
                    record_count: 1,
                    first_cmd: cmd_index,
                }),
            }
        }
        assert_eq!(
            runs,
            vec![
                WorldIndirectRun {
                    page: 0,
                    first_record: 0,
                    record_count: 2,
                    first_cmd: 0
                },
                WorldIndirectRun {
                    page: 1,
                    first_record: 2,
                    record_count: 1,
                    first_cmd: 2
                },
                WorldIndirectRun {
                    page: 0,
                    first_record: 3,
                    record_count: 1,
                    first_cmd: 3
                },
                WorldIndirectRun {
                    page: 1,
                    first_record: 4,
                    record_count: 2,
                    first_cmd: 4
                },
            ]
        );
        let expanded: Vec<(usize, u32, u32)> = runs
            .iter()
            .flat_map(|run| {
                args[run.first_record as usize..(run.first_record + run.record_count) as usize]
                    .iter()
                    .map(move |arg| (run.page, arg.first_index, arg.first_index + arg.index_count))
            })
            .collect();
        assert_eq!(
            expanded,
            cmds.iter()
                .map(|cmd| (cmd.page, cmd.start, cmd.end))
                .collect::<Vec<_>>()
        );
        assert!(args
            .iter()
            .all(|arg| arg.instance_count == 1 && arg.base_vertex == 0 && arg.first_instance == 0));
    }

    #[test]
    #[ignore = "requires software Vulkan; exercised by the PR GPU regression step"]
    fn gpu_world_culler_executes_and_updates_indirect_visibility() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: true,
            compatible_surface: None,
        }))
        .expect("no software Vulkan adapter");
        if !adapter
            .features()
            .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT)
        {
            println!("::notice::GPU world culling skipped: software adapter lacks indirect-count support");
            return;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("world culling regression device"),
                required_features: wgpu::Features::MULTI_DRAW_INDIRECT_COUNT,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .expect("failed to create world culling regression device");

        let mut commands = Vec::with_capacity(64);
        for index in 0..64u32 {
            let (min, max) = if index < 32 {
                ([-1.0, 0.0, -5.0], [1.0, 2.0, -3.0])
            } else {
                ([20.0, 0.0, -5.0], [22.0, 2.0, -3.0])
            };
            commands.push(WorldDrawCmd {
                page: 0,
                start: index * 3,
                end: index * 3 + 3,
                min,
                max,
                has_sdr: index % 2 == 0,
            });
        }
        let list = WorldDrawList {
            cmds: commands,
            chunks: vec![WorldDrawChunk {
                min: [-1.0, 0.0, -5.0],
                max: [22.0, 2.0, -3.0],
                has_sdr: true,
                start: 0,
                end: 64,
            }],
        };
        let frustum = [
            [1.0, 0.0, 0.0, 10.0],
            [-1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 10.0],
            [0.0, -1.0, 0.0, 10.0],
            [0.0, 0.0, 1.0, 10.0],
            [0.0, 0.0, -1.0, 10.0],
        ];
        let mut cull = CullParams::frustum_only(frustum);
        let mut scratch = WorldDrawScratch::default();
        let read_counts = |device: &wgpu::Device,
                           queue: &wgpu::Queue,
                           scratch: &WorldDrawScratch,
                           count: usize| {
            let view = scratch.gpu_culler.as_ref().unwrap().views[0]
                .as_ref()
                .unwrap();
            let size = (count as u64) * std::mem::size_of::<u32>() as u64;
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world culling count readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("world culling count readback encoder"),
            });
            encoder.copy_buffer_to_buffer(&view.counts, 0, &readback, 0, size);
            queue.submit(Some(encoder.finish()));
            let slice = readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let mapped = slice.get_mapped_range();
            let counts = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
            drop(mapped);
            readback.unmap();
            counts
        };
        let read_records = |device: &wgpu::Device,
                            queue: &wgpu::Queue,
                            scratch: &WorldDrawScratch,
                            count: usize| {
            let view = scratch.gpu_culler.as_ref().unwrap().views[0]
                .as_ref()
                .unwrap();
            let size = (count as u64) * PAGE_DRAW_ARGS_SIZE;
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("world culling indirect readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("world culling readback encoder"),
            });
            encoder.copy_buffer_to_buffer(&view.indirect, 0, &readback, 0, size);
            queue.submit(Some(encoder.finish()));
            let slice = readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let mapped = slice.get_mapped_range();
            let records = bytemuck::cast_slice::<u8, PageDrawIndexedArgs>(&mapped).to_vec();
            drop(mapped);
            readback.unmap();
            records
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("world culling regression encoder"),
        });
        assert!(GpuPage::encode_world_culling(
            &device,
            &mut encoder,
            &queue,
            &list,
            &cull,
            0,
            false,
            true,
            &mut scratch,
        ));
        queue.submit(Some(encoder.finish()));
        let first = read_records(&device, &queue, &scratch, 64);
        let first_counts = read_counts(&device, &queue, &scratch, 1);
        assert_eq!(first_counts, vec![32]);
        assert!(first[..32].iter().all(|arg| arg.instance_count == 1));
        assert!(first[32..].iter().all(|arg| arg.instance_count == 0));
        assert!(first[..32]
            .iter()
            .enumerate()
            .all(|(index, arg)| arg.first_index == index as u32 * 3));

        // Reusing the same static command/bounds buffers with a changed view
        // must rewrite every count; old occluded bits cannot survive a turn.
        cull.frustum = [
            [1.0, 0.0, 0.0, 100.0],
            [-1.0, 0.0, 0.0, 100.0],
            [0.0, 1.0, 0.0, 100.0],
            [0.0, -1.0, 0.0, 100.0],
            [0.0, 0.0, 1.0, 100.0],
            [0.0, 0.0, -1.0, 100.0],
        ];
        // Keep the GPU path active even though the wider frustum now contains
        // the full chunk: this view-local clip still requires a fresh dispatch.
        cull.clip_plane = Some((0.0, 1.0));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("world culling second-view encoder"),
        });
        assert!(GpuPage::encode_world_culling(
            &device,
            &mut encoder,
            &queue,
            &list,
            &cull,
            0,
            false,
            true,
            &mut scratch,
        ));
        queue.submit(Some(encoder.finish()));
        let second = read_records(&device, &queue, &scratch, 64);
        let second_counts = read_counts(&device, &queue, &scratch, 1);
        assert_eq!(second_counts, vec![64]);
        assert!(second.iter().all(|arg| arg.instance_count == 1));
        println!(
            "::notice::GPU world culling passed: stable compaction, indirect draw counts, and current-view invalidation"
        );
    }

    #[test]
    fn gpu_world_cull_reference_is_conservative_for_frustum_clip_and_screen_size() {
        let frustum = [
            [1., 0., 0., 10.],
            [-1., 0., 0., 10.],
            [0., 1., 0., 10.],
            [0., -1., 0., 10.],
            [0., 0., 1., 10.],
            [0., 0., -1., 10.],
        ];
        let cull = CullParams {
            frustum,
            eye: [0., 0., 0.],
            proj_scale: 100.,
            min_screen_px: 2.,
            clip_plane: Some((0., 1.)),
        };
        let command = |min, max, has_sdr| WorldDrawCmd {
            page: 0,
            start: 0,
            end: 36,
            min,
            max,
            has_sdr,
        };
        assert!(world_command_survives_gpu_cull(
            &command([-1., 1., -5.], [1., 3., -3.], true),
            &cull,
            false
        ));
        // Entirely below the reflection clip plane is rejected.
        assert!(!world_command_survives_gpu_cull(
            &command([-1., -5., -5.], [1., -1., -3.], true),
            &cull,
            false
        ));
        // Straddling the clip plane is kept; the GPU test is conservative.
        assert!(world_command_survives_gpu_cull(
            &command([-1., -1., -5.], [1., 1., -3.], true),
            &cull,
            false
        ));
        // Outside any one frustum plane is rejected.
        assert!(!world_command_survives_gpu_cull(
            &command([20., 1., -5.], [22., 3., -3.], true),
            &cull,
            false
        ));
        // SDR-only compaction must not suppress ordinary opaque-only commands.
        assert!(!world_command_survives_gpu_cull(
            &command([-1., 1., -5.], [1., 3., -3.], false),
            &cull,
            true
        ));
    }

    #[test]
    fn gpu_world_cull_fails_open_for_non_finite_bounds_and_keeps_near_bounds() {
        let cull = CullParams::frustum_only([
            [1., 0., 0., 10.],
            [-1., 0., 0., 10.],
            [0., 1., 0., 10.],
            [0., -1., 0., 10.],
            [0., 0., 1., 10.],
            [0., 0., -1., 10.],
        ]);
        let mut invalid = WorldDrawCmd {
            page: 0,
            start: 0,
            end: 12,
            min: [f32::NAN, 0., 0.],
            max: [1., 1., 1.],
            has_sdr: false,
        };
        assert!(world_command_survives_gpu_cull(&invalid, &cull, false));
        invalid.min = [0., 0., 0.];
        invalid.max = [1., 1., 1.];
        assert!(world_command_survives_gpu_cull(&invalid, &cull, false));
        invalid.end = invalid.start;
        assert!(!world_command_survives_gpu_cull(&invalid, &cull, false));
        assert!(WORLD_GPU_CULL_WGSL.contains("counts[group.x] = output_count;"));
        assert!(WORLD_GPU_CULL_WGSL.contains("draw_args[output_index].instance_count = 1u;"));
    }

    #[test]
    fn clip_plane_rejects_opposite_side_and_keeps_straddling_aabbs() {
        // Reflection (keep y >= 100.0)
        assert!(!aabb_survives_clip(
            Some((100.0, 1.0)),
            [0.0, 10.0, 0.0],
            [10.0, 99.9, 10.0]
        ));
        assert!(aabb_survives_clip(
            Some((100.0, 1.0)),
            [0.0, 95.0, 0.0],
            [10.0, 105.0, 10.0]
        ));
        assert!(aabb_survives_clip(
            Some((100.0, 1.0)),
            [0.0, 101.0, 0.0],
            [10.0, 120.0, 10.0]
        ));
        // Refraction (keep y <= 100.0)
        assert!(!aabb_survives_clip(
            Some((100.0, -1.0)),
            [0.0, 100.1, 0.0],
            [10.0, 120.0, 10.0]
        ));
        assert!(aabb_survives_clip(
            Some((100.0, -1.0)),
            [0.0, 95.0, 0.0],
            [10.0, 105.0, 10.0]
        ));
        assert!(aabb_survives_clip(
            None,
            [0.0, 500.0, 0.0],
            [10.0, 600.0, 10.0]
        ));
    }

    #[test]
    fn sub_pixel_screen_size_culling_rejects_tiny_distant_props_and_keeps_visible() {
        let eye = [0.0, 0.0, 0.0];
        let proj_scale = 770.0; // ~1080p at 70 deg FOV
                                // 1m cube at 2000m distance: diameter ~1.73m -> ~0.67 px (< 1.5 px)
        assert!(!aabb_survives_screen_size(
            eye,
            proj_scale,
            1.5,
            [0.0, 0.0, 2000.0],
            [1.0, 1.0, 2001.0]
        ));
        // Same 1m cube at 200m distance: ~6.7 px (>= 1.5 px)
        assert!(aabb_survives_screen_size(
            eye,
            proj_scale,
            1.5,
            [0.0, 0.0, 200.0],
            [1.0, 1.0, 201.0]
        ));
        // Stricter secondary-pass threshold (4.0 px) rejects 3 px prop
        assert!(!aabb_survives_screen_size(
            eye,
            proj_scale,
            4.0,
            [0.0, 0.0, 500.0],
            [1.0, 1.0, 501.0]
        ));
        // Disabled threshold (0.0) keeps everything
        assert!(aabb_survives_screen_size(
            eye,
            proj_scale,
            0.0,
            [0.0, 0.0, 50000.0],
            [0.1, 0.1, 50000.1]
        ));
    }

    #[test]
    fn coalescing_bridges_small_gaps_and_splits_large_gaps_or_page_changes() {
        let mut cmds = Vec::new();
        push_coalesced_draw(&mut cmds, 0, 0, 100, MAX_COALESCE_GAP);
        // Contiguous
        push_coalesced_draw(&mut cmds, 0, 100, 200, MAX_COALESCE_GAP);
        // Small gap (<= MAX_COALESCE_GAP) on same page -> bridged!
        push_coalesced_draw(&mut cmds, 0, 200 + MAX_COALESCE_GAP, 500, MAX_COALESCE_GAP);
        // Large gap (> MAX_COALESCE_GAP) -> split!
        push_coalesced_draw(
            &mut cmds,
            0,
            500 + MAX_COALESCE_GAP + 1,
            700,
            MAX_COALESCE_GAP,
        );
        // Different page -> split!
        push_coalesced_draw(&mut cmds, 1, 700, 800, MAX_COALESCE_GAP);
        assert_eq!(
            cmds,
            vec![
                DrawCmd {
                    page: 0,
                    start: 0,
                    end: 500
                },
                DrawCmd {
                    page: 0,
                    start: 500 + MAX_COALESCE_GAP + 1,
                    end: 700
                },
                DrawCmd {
                    page: 1,
                    start: 700,
                    end: 800
                },
            ]
        );
    }

    #[test]
    fn the_fold_draws_exactly_what_the_per_placement_loop_drew() {
        // Touching spans over three pages, submitted in a global order, under
        // every view: the folded spans must equal the per-placement spans the
        // visibility gate allows, and the fold must never need more draws.
        let mut spans: Vec<Vec<PlacementRange>> = Vec::new();
        for page in 0..3u32 {
            let mut list = Vec::new();
            let mut start = 0u32;
            for i in 0..6u32 {
                let len = 3 + (page * 7 + i * 5) % 11;
                let order = (page * 100 + i) as u64;
                list.push(span(start, start + len, order, i % 2 == 0, i % 3 != 0));
                start += len;
            }
            spans.push(list);
        }
        let refs: Vec<&[PlacementRange]> = spans.iter().map(|list| list.as_slice()).collect();
        let counts: Vec<u32> = spans.iter().map(|list| list.last().unwrap().end).collect();
        let pages = pages(&refs, &counts);
        let mut order: Vec<(u64, usize, usize)> = Vec::new();
        for (page, list) in spans.iter().enumerate() {
            for (range, r) in list.iter().enumerate() {
                order.push((r.order, page, range));
            }
        }
        order.sort_unstable();
        let order: Vec<(usize, usize)> = order.into_iter().map(|(_, p, r)| (p, r)).collect();
        for view in [Pass::Main, Pass::Reflection, Pass::Refraction] {
            let mut expected = Vec::new();
            for &(page, range) in &order {
                let r = &spans[page][range];
                if allows_with_context(view, r.visibility, RenderContext::default()) {
                    expected.extend((r.start..r.end).map(|i| (page, i)));
                }
            }
            let cmds = fold_commands(&pages, &order, view);
            assert_eq!(drawn(&cmds), expected, "view {view:?}");
            assert!(cmds.iter().all(|c| c.start <= c.end));
            assert!(cmds.len() <= order.len());
        }
    }
}
