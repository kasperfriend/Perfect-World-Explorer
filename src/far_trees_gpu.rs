//! GPU-instanced, runtime-derived far-tree LOD covering all scene placements.
use crate::{gpu::Vertex, trees::Trees};
#[path = "terrain_cutoff.rs"]
mod cutoff_residency;
use crate::indirect_args::{base_instance, multi_draw, record, DrawIndexedArgs, DRAW_ARGS};
use bytemuck::{Pod, Zeroable};
use std::cell::RefCell;
use wgpu::util::DeviceExt;
/// One instance record: `[position.xyz, 0]`, then `[fraction, mode, 0, 0]`.
///
/// Two `vec4`s rather than a `vec3` + `vec2`: WGSL pads a storage-array element
/// to a 16-byte boundary, so `info` would land at offset 16 there but at 12 in a
/// packed 20-byte struct. Writing the record as two `vec4`s makes the compute
/// pass that fills it and the vertex attributes that read it agree by
/// construction. Mode is 0 (single level), 1 (outgoing) or 2 (incoming).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    data: [f32; 8],
}
impl Instance {
    fn new(position: [f32; 3], info: [f32; 2]) -> Self {
        Self {
            data: [
                position[0],
                position[1],
                position[2],
                0.,
                info[0],
                info[1],
                0.,
                0.,
            ],
        }
    }
}
/// One tree placement, immutable for the whole session: the LOD compute pass
/// reads these and nothing else has to be uploaded per frame.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Placement {
    pub(crate) pos_height: [f32; 4],
    pub(crate) group: u32,
    pub(crate) pad: [u32; 3],
}
/// Per-group constants the LOD pass needs, plus where its instances live.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GroupInfo {
    pub(crate) height: f32,
    /// First indirect-args record of this group (`first_batch`, three levels).
    pub(crate) args_base: u32,
    /// First instance slot of level 0 in the instance buffer.
    pub(crate) first_instance: u32,
    /// Placements per level, i.e. the capacity of each level's slice.
    pub(crate) capacity: u32,
}

const TREE_SPATIAL_CELL_SIZE: f32 = 256.0;

struct TreeSpatialCell {
    min: [f32; 3],
    max: [f32; 3],
    placement_indices: Vec<u32>,
}

fn build_tree_spatial_cells(
    placements: &[Placement],
    groups: &[GroupInfo],
) -> Vec<TreeSpatialCell> {
    let mut cells = std::collections::BTreeMap::<(i32, i32), TreeSpatialCell>::new();
    for (index, placement) in placements.iter().enumerate() {
        let Some(group) = groups.get(placement.group as usize) else {
            continue;
        };
        if group.capacity == 0 {
            continue;
        }
        let position = [
            placement.pos_height[0],
            placement.pos_height[1],
            placement.pos_height[2],
        ];
        if !position.iter().all(|value| value.is_finite()) {
            continue;
        }
        let radius = group.height.max(5.0) * 2.0;
        let min = [
            position[0] - radius,
            position[1] - radius,
            position[2] - radius,
        ];
        let max = [
            position[0] + radius,
            position[1] + radius,
            position[2] + radius,
        ];
        let key = (
            (position[0] / TREE_SPATIAL_CELL_SIZE).floor() as i32,
            (position[2] / TREE_SPATIAL_CELL_SIZE).floor() as i32,
        );
        let cell = cells.entry(key).or_insert_with(|| TreeSpatialCell {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            placement_indices: Vec::new(),
        });
        for axis in 0..3 {
            cell.min[axis] = cell.min[axis].min(min[axis]);
            cell.max[axis] = cell.max[axis].max(max[axis]);
        }
        cell.placement_indices.push(index as u32);
    }
    cells.into_values().collect()
}

/// Conservative AABB/frustum rejection kept local because this module is also
/// compiled by standalone tree diagnostics that intentionally omit world_objects.
fn tree_cell_intersects_frustum(planes: &[[f32; 4]; 6], min: [f32; 3], max: [f32; 3]) -> bool {
    if min.iter().chain(max.iter()).any(|value| !value.is_finite())
        || (0..3).any(|axis| min[axis] > max[axis])
    {
        return true;
    }
    for plane in planes {
        let positive = [
            if plane[0] >= 0.0 { max[0] } else { min[0] },
            if plane[1] >= 0.0 { max[1] } else { min[1] },
            if plane[2] >= 0.0 { max[2] } else { min[2] },
        ];
        if plane[0] * positive[0] + plane[1] * positive[1] + plane[2] * positive[2] + plane[3] < 0.0
        {
            return false;
        }
    }
    true
}

/// Reject only when the complete tree-cell AABB is on the clipped side.
fn tree_cell_survives_clip(clip_plane: Option<(f32, f32)>, min: [f32; 3], max: [f32; 3]) -> bool {
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

/// Append only cells whose conservative tree bounds can intersect this view.
/// The per-tree shader tests still run on every selected placement.
fn append_visible_tree_indices(
    cells: &[TreeSpatialCell],
    frustum: &[[f32; 4]; 6],
    clip_plane: Option<(f32, f32)>,
    bounds: Option<[f32; 4]>,
    out: &mut Vec<u32>,
) {
    for cell in cells {
        if let Some([x0, x1, z0, z1]) = bounds {
            if cell.max[0] < x0 || cell.min[0] >= x1 || cell.max[2] < z0 || cell.min[2] >= z1 {
                continue;
            }
        }
        if !tree_cell_survives_clip(clip_plane, cell.min, cell.max)
            || !tree_cell_intersects_frustum(frustum, cell.min, cell.max)
        {
            continue;
        }
        out.extend_from_slice(&cell.placement_indices);
    }
}

/// Per-frame inputs of the LOD compute pass.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct LodParams {
    /// Eye position; `.w` is the LOD scale the frame uses.
    pub(crate) camera: [f32; 4],
    /// Admitted XZ square (`x0, x1, z0, z1`); ignored when `counts.z` is 0.
    pub(crate) bounds: [f32; 4],
    /// Normalized view-frustum planes; tree spheres are tested conservatively.
    pub(crate) frustum: [[f32; 4]; 6],
    /// Optional water clip plane: height, sign, enabled, unused.
    pub(crate) clip: [f32; 4],
    /// `active placements, active-index word offset (0 = full scan), bounds_active, 0`.
    pub(crate) counts: [u32; 4],
    /// The exact view-projection used by the depth-producing pass.
    pub(crate) view_projection: [[f32; 4]; 4],
    /// `(width, height, element offset, 0)` for each ceil-halved mip.
    pub(crate) hiz_mips: [[u32; 4]; 16],
    /// Width, height, mip count, enabled.
    pub(crate) hiz_info: [u32; 4],
    /// Positive reversed-depth comparison bias and padding.
    pub(crate) hiz_bias: [f32; 4],
}

/// The LOD selection pass.
///
/// This is `tree_lod::client_distance` + `tree_lod::select` transcribed into
/// WGSL and run for the view's spatially selected candidates on the GPU, so a
/// camera move no longer costs
/// a walk over every tree placement in the world plus a full instance-buffer
/// upload. Both functions are pure arithmetic and are reproduced operation for
/// operation: the same fast square root on the squared distance
/// (`(bits >> 1) + 0x1fc00000`), the same 10 % transition windows around
/// `height * 5 * scale` and `height * 16 * scale`. The only difference is that
/// the CPU sums the squared deltas in f64 and rounds once; the GPU sums in f32,
/// which can move a distance by ~1e-7 relative — inside a transition window that
/// changes the fade fraction, never whether the tree is drawn.
pub(crate) const LOD_COMPUTE: &str = r#"
struct LodParamsGpu {
 camera:vec4<f32>, bounds:vec4<f32>, frustum:array<vec4<f32>,6>, clip:vec4<f32>, counts:vec4<u32>,
 view_projection:mat4x4<f32>, hiz_mips:array<vec4<u32>,16>, hiz_info:vec4<u32>, hiz_bias:vec4<f32>
};
struct Placement {pos_height:vec4<f32>,group:u32,pad0:u32,pad1:u32,pad2:u32};
struct GroupInfo {height:f32,args_base:u32,first_instance:u32,capacity:u32};
struct Args {index_count:u32,instance_count:atomic<u32>,first_index:u32,base_vertex:i32,first_instance:u32};
struct Instance {data0:vec4<f32>,data1:vec4<f32>};
@group(0) @binding(0) var<uniform> p:LodParamsGpu;
@group(0) @binding(1) var<storage,read> placement_words:array<u32>;
@group(0) @binding(2) var<storage,read> groups:array<GroupInfo>;
@group(0) @binding(3) var<storage,read_write> instances:array<Instance>;
@group(0) @binding(4) var<storage,read_write> args:array<Args>;
@group(0) @binding(5) var hiz:texture_2d<f32>;
fn load_placement(index:u32)->Placement {
 let base=index*8u;
 return Placement(
  vec4<f32>(
   bitcast<f32>(placement_words[base]),
   bitcast<f32>(placement_words[base+1u]),
   bitcast<f32>(placement_words[base+2u]),
   bitcast<f32>(placement_words[base+3u])
  ),
  placement_words[base+4u],
  placement_words[base+5u],
  placement_words[base+6u],
  placement_words[base+7u]
 );
}
fn is_finite(v:f32)->bool {return v>=-3.0e38 && v<=3.0e38;}
// SpeedTreeRT 0x100282b0's estimate: f32 squared distance, then the float-bit
// square root. Matches `tree_lod::client_distance`.
fn client_distance(position:vec3<f32>,eye:vec3<f32>)->f32 {
 let d=position-eye;
 let d2=d.x*d.x+d.y*d.y+d.z*d.z;
 if !is_finite(d2) {return -1.;}
 return bitcast<f32>((bitcast<u32>(d2)>>1u)+0x1fc00000u);
}
// Returns (primary, secondary, fraction, has_secondary); matches
// `tree_lod::select` including the order the two transition windows are tested.
fn select_level(distance:f32,height:f32,scale:f32)->vec4<f32> {
 let h=max(height,5.);
 let s=clamp(scale,0.25,4.);
 let a=h*5.*s;
 let b=h*16.*s;
 for(var i=0u;i<2u;i=i+1u){
  let boundary=select(a,b,i==1u);
  let start=boundary*0.9;
  let end=boundary*1.1;
  if distance>=start && distance<end {
   return vec4<f32>(f32(i),f32(i+1u),clamp((distance-start)/(end-start),0.,1.),1.);
  }
 }
 var primary=2.;
 if distance<a {primary=0.;} else if distance<b {primary=1.;}
 return vec4<f32>(primary,3.,0.,0.);
}
// Plane normals are normalized on the CPU. The radius uses twice the
// authored tree height, conservatively enclosing the trunk and broad canopies.
fn sphere_in_frustum(position:vec3<f32>,radius:f32)->bool {
 for(var i=0u;i<6u;i=i+1u) {
  let plane=p.frustum[i];
  if dot(plane.xyz,position)+plane.w < -radius {return false;}
 }
 return true;
}
// Reversed-Z conservative occlusion. The placement sphere is projected as
// an AABB; any near/far-plane ambiguity or invalid projection keeps it visible.
fn sphere_occluded(position:vec3<f32>,radius:f32)->bool {
 if p.hiz_info.w==0u || p.hiz_info.z==0u {return false;}
 let extent=vec2<f32>(f32(p.hiz_info.x),f32(p.hiz_info.y));
 var min_pixel=vec2<f32>(1.0e30);
 var max_pixel=vec2<f32>(-1.0e30);
 var nearest_depth=0.0;
 for(var corner=0u;corner<8u;corner=corner+1u) {
  let sx=select(-1.0,1.0,(corner&1u)!=0u);
  let sy=select(-1.0,1.0,(corner&2u)!=0u);
  let sz=select(-1.0,1.0,(corner&4u)!=0u);
  let clip=p.view_projection*vec4<f32>(position+vec3<f32>(sx,sy,sz)*radius,1.0);
  if !is_finite(clip.x) || !is_finite(clip.y) || !is_finite(clip.z) || !is_finite(clip.w) || clip.w<=1.0 {return false;}
  let ndc=clip.xyz/clip.w;
  // A box intersecting either depth plane is uncertain and must be drawn.
  if !is_finite(ndc.x) || !is_finite(ndc.y) || !is_finite(ndc.z) || ndc.z<0.0 || ndc.z>=1.0 {return false;}
  let pixel=vec2<f32>((ndc.x*0.5+0.5)*extent.x,(0.5-ndc.y*0.5)*extent.y);
  min_pixel=min(min_pixel,pixel);
  max_pixel=max(max_pixel,pixel);
  nearest_depth=max(nearest_depth,ndc.z);
 }
 // No overlap with the viewport means the frustum path owns this decision.
 if max_pixel.x<0.0 || max_pixel.y<0.0 || min_pixel.x>=extent.x || min_pixel.y>=extent.y {return false;}
 let screen_max=extent-vec2<f32>(1.0);
 let rect_min=vec2<u32>(clamp(floor(min_pixel-vec2<f32>(2.0)),vec2<f32>(0.0),screen_max));
 let rect_max=vec2<u32>(clamp(ceil(max_pixel+vec2<f32>(2.0)),vec2<f32>(0.0),screen_max));
 let threshold=nearest_depth+p.hiz_bias.x;
 if !is_finite(threshold) || threshold>=1.0 {return false;}
 var mip=0u;
 loop {
  let scale=1u<<mip;
  let lo=rect_min/vec2<u32>(scale);
  let hi=rect_max/vec2<u32>(scale);
  // At most 2x2 conservative Hi-Z cells. The coarser minimum pyramid can
  // only turn an occlusion into a visible candidate; it cannot hide a hole.
  if (hi.x-lo.x)<2u && (hi.y-lo.y)<2u {break;}
  if mip+1u>=p.hiz_info.z {break;}
  mip=mip+1u;
 }
 let info=p.hiz_mips[mip];
 if info.x==0u || info.y==0u {return false;}
 let scale=1u<<mip;
 let lo=min(rect_min/vec2<u32>(scale),info.xy-vec2<u32>(1u));
 let hi=min(rect_max/vec2<u32>(scale),info.xy-vec2<u32>(1u));
 for(var y=lo.y;y<=hi.y;y=y+1u) {
  for(var x=lo.x;x<=hi.x;x=x+1u) {
   let depth=textureLoad(hiz,vec2<i32>(i32(x),i32(y)),i32(mip)).r;
   if !is_finite(depth) || depth<0.0 || depth>1.0 || depth<=threshold {return false;}
  }
 }
 return true;
}
fn append(group_index:u32,level:u32,position:vec3<f32>,info:vec2<f32>) {
 let g=groups[group_index];
 if level>2u {return;}
 // A group whose cache carries fewer than three levels has no records of its
 // own, so it is uploaded with a zero capacity and never counted: taking a slot
 // first would bump the next group's record.
 if g.capacity==0u {return;}
 let slot=atomicAdd(&args[g.args_base+level].instance_count,1u);
 if slot>=g.capacity {return;}
 instances[g.first_instance+level*g.capacity+slot]=Instance(vec4<f32>(position,0.),vec4<f32>(info,0.,1.));
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 let active_slot=id.x;
 if active_slot>=p.counts.x {return;}
 let index=select(active_slot,placement_words[p.counts.y+active_slot],p.counts.y!=0u);
 let placement=load_placement(index);
 let position=placement.pos_height.xyz;
 if !is_finite(position.x) || !is_finite(position.y) || !is_finite(position.z) {return;}
 if p.counts.z!=0u {
  if position.x<p.bounds.x || position.x>=p.bounds.y {return;}
  if position.z<p.bounds.z || position.z>=p.bounds.w {return;}
 }
 let constants=groups[placement.group];
 let radius=max(constants.height,5.)*2.;
 if !sphere_in_frustum(position,radius) {return;}
 if p.clip.z>0.5 {
  if p.clip.y>0. && position.y+radius<p.clip.x {return;}
  if p.clip.y<0. && position.y>p.clip.x {return;}
 }
 if sphere_occluded(position,radius) {return;}
 let distance=client_distance(position,p.camera.xyz);
 if distance<0. {return;}
 let selection=select_level(distance,constants.height,p.camera.w);
 let fraction=selection.z;
 let secondary=selection.w>0.5;
 append(placement.group,u32(selection.x),position,vec2<f32>(fraction,select(0.,1.,secondary)));
 if secondary {
  append(placement.group,u32(selection.y),position,vec2<f32>(fraction,2.));
 }
}
"#;
struct Batch {
    indices: std::ops::Range<u32>,
    instances: std::ops::Range<u32>,
}
struct Group {
    positions: Vec<[f32; 3]>,
    height: f32,
    first_batch: usize,
}
/// Per-group constants for the LOD pass.
///
/// The indirect-args buffer holds **one record per batch**, and a group owns its
/// three level batches back to back, so `args_base` is that first batch index and
/// level `L` is `args_base + L`. Scaling it (the old `first_batch * 3`) sends
/// every group but the first into a later group's records: the level that should
/// draw keeps a zero count, and the group whose record was borrowed draws with a
/// count it never earned.
///
/// Only a group with the three levels the selection chooses between has records
/// of its own; anything shorter is uploaded with a zero capacity so the pass
/// writes nothing for it instead of spilling into the next group.
fn group_table(groups: &[Group], batches: &[Batch]) -> Vec<GroupInfo> {
    groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let end = groups
                .get(index + 1)
                .map_or(batches.len(), |next| next.first_batch);
            let levels = end.saturating_sub(group.first_batch);
            if levels < 3 {
                eprintln!(
                    "Tree group {index} has {levels} LOD level(s); the LOD pass needs three, so the group draws nothing instead of borrowing the next group's records"
                );
            }
            GroupInfo {
                height: group.height,
                args_base: group.first_batch as u32,
                first_instance: batches[group.first_batch].instances.start,
                capacity: if levels >= 3 {
                    group.positions.len() as u32
                } else {
                    0
                },
            }
        })
        .collect()
}
/// The cutoff test the LOD pass applies to a placement, exposed so the checks
/// can compute the same expectation the GPU does.
#[allow(dead_code)]
pub fn lod_admits(bounds: Option<[f32; 4]>, p: [f32; 3]) -> bool {
    cutoff_residency::contains(bounds, p)
}

/// World- or local-space tree triangle retained for ray-tracing harvest.
/// Kept local to `far_trees_gpu` so standalone GPU check binaries that include
/// this file directly compile without pulling in `crate::modules`.
#[derive(Clone, Copy, Debug)]
pub struct RtTreeTriangle {
    pub a: glam::Vec3,
    pub b: glam::Vec3,
    pub c: glam::Vec3,
    pub normal: glam::Vec3,
    pub albedo: [f32; 3],
    pub roughness: f32,
    pub opacity: f32,
    /// Real 7x7 (49-bit) texture-sampled alpha cutout mask (`0` = opaque/unmasked).
    pub alpha_mask: u64,
    /// Camera-facing leaf quad pivot in local tree space; zero and rho=-1 for branches/fronds.
    pub leaf_center: glam::Vec3,
    /// Original SpeedTree leaf rho, applied at per-frame harvest; -1 means this is not a leaf.
    pub leaf_rho: f32,
}

/// Per-LOD RT harvest diagnostics for one SpeedTree species.
#[derive(Clone, Copy, Debug, Default)]
pub struct RtTreeLodDiag {
    pub branch_tris: usize,
    pub frond_tris: usize,
    pub frond_skipped: usize,
    pub fixed_3d_leaves: usize,
    pub billboard_leaves: usize,
    pub leaves_skipped: usize,
    pub masked_tris: usize,
    pub unmasked_foliage_tris: usize,
}

/// Local-space SpeedTree LOD meshes retained for Raywatcher RT harvesting.
pub struct RtTreeSpecies {
    pub species_id: usize,
    pub height: f32,
    pub composite_size: (u32, u32),
    pub composite_alpha_min: u8,
    pub composite_alpha_max: u8,
    pub positions: Vec<[f32; 3]>,
    pub lods: Vec<Vec<RtTreeTriangle>>,
    pub lod_diags: Vec<RtTreeLodDiag>,
}

/// Sample linear RGB + alpha coverage from an RGBA8 texture at normalized UV.
///
/// Match the far-tree raster sampler's Repeat + Linear filtering at mip 0.
/// Nearest-texel sampling made the RT cutout mask depend on arbitrary source
/// texel boundaries, exaggerating small transparent gaps into jagged card edges.
fn sample_rgba_uv(img: &image::RgbaImage, uv: [f32; 2]) -> ([f32; 3], f32) {
    let w = img.width().max(1) as i64;
    let h = img.height().max(1) as i64;
    let u = uv[0].rem_euclid(1.0);
    let v = uv[1].rem_euclid(1.0);
    // GPU normalized linear sampling addresses texel centres at (n + 0.5)/size.
    let x = u * w as f32 - 0.5;
    let y = v * h as f32 - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let sample = |px: i64, py: i64| {
        img.get_pixel(px.rem_euclid(w) as u32, py.rem_euclid(h) as u32)
            .0
    };
    let p00 = sample(x0, y0);
    let p10 = sample(x0 + 1, y0);
    let p01 = sample(x0, y0 + 1);
    let p11 = sample(x0 + 1, y0 + 1);
    let bilinear = |channel: usize| {
        let top = (p00[channel] as f32 * (1.0 - fx) + p10[channel] as f32 * fx) / 255.0;
        let bottom = (p01[channel] as f32 * (1.0 - fx) + p11[channel] as f32 * fx) / 255.0;
        top * (1.0 - fy) + bottom * fy
    };
    let to_lin = |c: f32| c.powf(2.2);
    (
        [
            to_lin(bilinear(0)),
            to_lin(bilinear(1)),
            to_lin(bilinear(2)),
        ],
        bilinear(3),
    )
}

/// Convert a SpeedTree leaf corner with the exact view-facing transform used by the raster vertex shader.
/// SpeedTree's source coordinates are Z-up; the renderer's world is Y-up.
fn speedtree_leaf_corner(corner: [f32; 3], view_direction: glam::Vec3, rho: f32) -> glam::Vec3 {
    if rho > 1000.0 {
        return glam::Vec3::new(corner[0], corner[2], corner[1]);
    }
    let d = view_direction.normalize_or_zero();
    let azimuth = d.z.atan2(d.x);
    let elevation = d.y.clamp(-1.0, 1.0).asin();
    let (sin_elevation, cos_elevation) = elevation.sin_cos();
    let q = glam::Vec3::new(
        cos_elevation * corner[0] - sin_elevation * corner[2],
        corner[1],
        sin_elevation * corner[0] + cos_elevation * corner[2],
    );
    let (sin_angle, cos_angle) = (azimuth + rho).sin_cos();
    let v = glam::Vec3::new(
        cos_angle * q.x - sin_angle * q.y,
        sin_angle * q.x + cos_angle * q.y,
        q.z,
    );
    glam::Vec3::new(v.x, v.z, v.y)
}

#[inline]
fn raster_alpha_cutoff(authored_mips: bool, authored_reference: Option<f32>) -> f32 {
    if authored_mips {
        authored_reference
            .filter(|reference| reference.is_finite() && *reference >= 0.0)
            .map(|reference| (reference / 255.0).clamp(0.0, 1.0))
            .unwrap_or(0.5)
    } else {
        0.5
    }
}

#[inline]
fn leaf_alpha_cutoff(authored_mips: bool) -> f32 {
    raster_alpha_cutoff(authored_mips, Some(84.0))
}

#[inline]
fn foliage_opacity(hits: u32) -> f32 {
    if hits >= 49 {
        // A fully covered 7x7 mask is encoded as unmasked; it must therefore be fully opaque too.
        1.0
    } else {
        (0.45 + 0.48 * (hits as f32 / 49.0)).clamp(0.45, 0.92)
    }
}

/// The primary SpeedTree LOD selected by the same distance/height/scale bands as the raster compute shader.
fn raster_lod_index(distance: f32, height: f32, scale: f32, lod_count: usize) -> usize {
    let count = lod_count.min(3);
    if count <= 1 {
        return 0;
    }
    let h = height.max(5.0);
    let s = scale.clamp(0.25, 4.0);
    let lod0 = h * 5.0 * s;
    let lod1 = h * 16.0 * s;
    for (index, boundary) in [lod0, lod1].into_iter().enumerate() {
        if distance >= boundary * 0.9 && distance < boundary * 1.1 {
            return index.min(count - 1);
        }
    }
    if distance < lod0 {
        0
    } else if distance < lod1 {
        1.min(count - 1)
    } else {
        2.min(count - 1)
    }
}

/// Select the first real, non-sparse LOD that fits the available triangle
/// budget. With an unlimited budget this is the baseline LOD used to decide
/// whether candidate ordering can affect the harvest.
fn select_harvest_lod(
    species: &RtTreeSpecies,
    start_lod: usize,
    lod_count: usize,
    remaining: usize,
) -> Option<usize> {
    (start_lod..lod_count).find(|&candidate| {
        let triangles = &species.lods[candidate];
        let sparse_outer_lod =
            candidate > 0 && triangles.len() < 12 && !species.lods[candidate - 1].is_empty();
        !triangles.is_empty() && !sparse_outer_lod && triangles.len() <= remaining
    })
}

#[inline]
fn approximate_client_distance(d2: f32) -> f32 {
    f32::from_bits((d2.to_bits() >> 1).wrapping_add(0x1fc0_0000))
}

/// Convert one species LOD's camera-facing leaf vertices once per scene build.
/// The transformed local-space triangles are shared by every placement of the
/// same species/LOD; placement-specific work is then only a translation.
fn transform_tree_lod_for_view(
    source: &[RtTreeTriangle],
    view_direction: glam::Vec3,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<Vec<RtTreeTriangle>> {
    // Direction normalization, azimuth and elevation are constant for every
    // leaf in this scene. The previous per-vertex helper recalculated all three
    // (including atan2/asin) for each of a triangle's three corners.
    let direction = view_direction.normalize_or_zero();
    let azimuth = direction.z.atan2(direction.x);
    let elevation = direction.y.clamp(-1.0, 1.0).asin();
    let (sin_elevation, cos_elevation) = elevation.sin_cos();

    let mut transformed = Vec::with_capacity(source.len());
    for (index, triangle) in source.iter().enumerate() {
        if index & 0x3ff == 0
            && cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            return None;
        }
        let mut moved = *triangle;
        if moved.leaf_rho >= 0.0 {
            let pivot = moved.leaf_center;
            let rho = moved.leaf_rho;
            // Rho is shared by all three vertices, so evaluate its azimuth
            // rotation once per triangle rather than once per vertex.
            let (sin_angle, cos_angle) = if rho > 1000.0 {
                (0.0, 1.0)
            } else {
                (azimuth + rho).sin_cos()
            };
            let transform = |vertex: glam::Vec3| {
                let relative = vertex - pivot;
                let corner = [relative.x, relative.z, relative.y];
                if rho > 1000.0 {
                    return pivot + glam::Vec3::new(corner[0], corner[2], corner[1]);
                }
                let qx = cos_elevation * corner[0] - sin_elevation * corner[2];
                let qz = sin_elevation * corner[0] + cos_elevation * corner[2];
                let x = cos_angle * qx - sin_angle * corner[1];
                let y = sin_angle * qx + cos_angle * corner[1];
                pivot + glam::Vec3::new(x, qz, y)
            };
            moved.a = transform(moved.a);
            moved.b = transform(moved.b);
            moved.c = transform(moved.c);
            moved.normal = glam::Vec3::Y;
            moved.leaf_center = glam::Vec3::ZERO;
            moved.leaf_rho = -1.0;
        }
        transformed.push(moved);
    }
    Some(transformed)
}

/// Append a local-space tree LOD at one placement. Billboard transforms have
/// already been resolved (and cached) before this hot per-instance loop.
fn append_tree_lod_at(
    out: &mut Vec<RtTreeTriangle>,
    triangles: &[RtTreeTriangle],
    offset: glam::Vec3,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> bool {
    out.reserve(triangles.len());
    for (index, triangle) in triangles.iter().enumerate() {
        if index & 0x3ff == 0
            && cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            return false;
        }
        let mut moved = *triangle;
        moved.a += offset;
        moved.b += offset;
        moved.c += offset;
        out.push(moved);
    }
    true
}

/// Sample a real 7x7 (49-cell) barycentric alpha cutout bitmask and alpha-weighted
/// linear albedo from `img` over triangle UVs `(u0, u1, u2)`.
/// Matches the exact `(s, t)` barycentric mapping decoded by `accept_opacity` in HLSL/WGSL.
fn sample_tri_alpha_mask49(
    img: &image::RgbaImage,
    u0: [f32; 2],
    u1: [f32; 2],
    u2: [f32; 2],
    cutoff: f32,
) -> (u64, [f32; 3], u32) {
    let mut mask = 0u64;
    let mut rgb_sum = [0.0f32; 3];
    let mut w_sum = 0.0f32;
    let mut hits = 0u32;
    for iy in 0..7u32 {
        let t = (iy as f32 + 0.5) / 7.0;
        for ix in 0..7u32 {
            let s = (ix as f32 + 0.5) / 7.0;
            let bx = s;
            let by = t * (1.0 - s);
            let bw = 1.0 - bx - by;
            let uv = [
                u0[0] * bw + u1[0] * bx + u2[0] * by,
                u0[1] * bw + u1[1] * bx + u2[1] * by,
            ];
            let (rgb, a) = sample_rgba_uv(img, uv);
            if a >= cutoff {
                mask |= 1u64 << (iy * 7 + ix);
                hits += 1;
                rgb_sum[0] += rgb[0] * a;
                rgb_sum[1] += rgb[1] * a;
                rgb_sum[2] += rgb[2] * a;
                w_sum += a;
            }
        }
    }
    let albedo = if w_sum > 1e-4 {
        [
            (rgb_sum[0] / w_sum).clamp(0.04, 0.92),
            (rgb_sum[1] / w_sum).clamp(0.04, 0.92),
            (rgb_sum[2] / w_sum).clamp(0.04, 0.92),
        ]
    } else {
        [0.14, 0.28, 0.11]
    };
    (mask, albedo, hits)
}

/// Build the same isolated, coverage-preserving composite mip chain used by
/// the raster atlas. RT triangles cannot evaluate fragment derivatives, so the
/// tree's discrete geometry LOD selects the matching alpha mip as a stable
/// approximation instead of every LOD sampling high-frequency mip 0.
fn rt_foliage_mip_images(
    base: &image::RgbaImage,
    authored_mips: &[image::RgbaImage],
    width: u32,
    height: u32,
    use_authored: bool,
) -> Vec<image::RgbaImage> {
    let mut level_zero = image::RgbaImage::new(width.max(1), height.max(1));
    for y in 0..height.max(1) {
        let sy = (y as u64 * base.height().max(1) as u64 / height.max(1) as u64)
            .min(base.height().saturating_sub(1) as u64) as u32;
        for x in 0..width.max(1) {
            let sx = (x as u64 * base.width().max(1) as u64 / width.max(1) as u64)
                .min(base.width().saturating_sub(1) as u64) as u32;
            level_zero.put_pixel(x, y, *base.get_pixel(sx, sy));
        }
    }
    let mut images = vec![level_zero];
    if use_authored
        && crate::vegetation_textures::chain_fits(authored_mips, width.max(1), height.max(1), 7)
    {
        images.extend(authored_mips.iter().take(7).cloned());
        return images;
    }

    let source_coverage = crate::foliage_mips::coverage(images[0].as_raw());
    let mut raw = images[0].as_raw().clone();
    let (mut mip_width, mut mip_height) = (width.max(1) as usize, height.max(1) as usize);
    for _ in 1..8 {
        if mip_width < 2 || mip_height < 2 {
            break;
        }
        raw = crate::foliage_mips::downsample(&raw, mip_width, mip_height, true);
        mip_width /= 2;
        mip_height /= 2;
        let corrected = crate::foliage_mips::preserve_coverage(&raw, source_coverage);
        if let Some(image) =
            image::RgbaImage::from_raw(mip_width as u32, mip_height as u32, corrected)
        {
            images.push(image);
        } else {
            break;
        }
    }
    images
}

type ResidentUpdateKey = (
    [f32; 3],
    f32,
    Option<[f32; 4]>,
    [[f32; 4]; 6],
    Option<(f32, f32)>,
);

/// Whether the spatial candidate set is still valid. The small camera epsilon
/// matches the old no-Hi-Z early-out; frustum and residency remain exact.
fn same_resident_update(
    previous: &ResidentUpdateKey,
    camera: [f32; 3],
    scale: f32,
    bounds: Option<[f32; 4]>,
    frustum: [[f32; 4]; 6],
    clip_plane: Option<(f32, f32)>,
) -> bool {
    let (prev_cam, prev_scale, prev_bounds, prev_frustum, prev_clip) = previous;
    let dx = camera[0] - prev_cam[0];
    let dy = camera[1] - prev_cam[1];
    let dz = camera[2] - prev_cam[2];
    dx * dx + dy * dy + dz * dz < 1e-4
        && *prev_scale == scale
        && *prev_bounds == bounds
        && *prev_frustum == frustum
        && *prev_clip == clip_plane
}

pub struct FarTrees {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    reflected_bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    instances: wgpu::Buffer,
    batches: Vec<Batch>,
    groups: Vec<Group>,
    rt_species: std::sync::Arc<[RtTreeSpecies]>,
    /// Slots in the instance buffer: three level slices per placement.
    capacity: usize,
    /// Coarse CPU spatial index; only indices from cells intersecting this view
    /// are uploaded before the per-tree GPU LOD/occlusion pass.
    spatial_cells: Vec<TreeSpatialCell>,
    active_placement_indices: RefCell<Vec<u32>>,
    /// Byte offset of the optional active-index tail packed after placements.
    /// None means the combined buffer would exceed the adapter's storage limit;
    /// the shader then scans the static placement prefix directly.
    active_index_offset: Option<u64>,
    last_resident_update: RefCell<
        Option<(
            [f32; 3],
            f32,
            Option<[f32; 4]>,
            [[f32; 4]; 6],
            Option<(f32, f32)>,
        )>,
    >,
    /// The LOD selection pass: a camera move dispatches this instead of walking
    /// every placement on the CPU and re-uploading the instance buffer.
    lod_pipeline: wgpu::ComputePipeline,
    lod_bind: wgpu::BindGroup,
    lod_layout: wgpu::BindGroupLayout,
    placement_buffer: wgpu::Buffer,
    group_buffer: wgpu::Buffer,
    lod_params: wgpu::Buffer,
    placements: u32,
    /// Indirect args, one record per batch (`DRAW_ARGS` bytes each), plus the
    /// static part of each record with `instance_count` zeroed.
    lod_args: wgpu::Buffer,
    lod_args_reset: Vec<u8>,
    /// `multi_draw_indexed_indirect` + `first_instance` are optional device
    /// features; when they are present the whole batch list is one call.
    multi_draw: bool,
}
const SHADER: &str = r#"
struct Camera {matrix:mat4x4<f32>,direction:vec4<f32>,clip:vec4<f32>,bounds:vec4<f32>,tree_bounds:vec4<f32>,sun:vec4<f32>,light:vec4<f32>,region_ambient:vec4<f32>,region_diffuse:vec4<f32>,region_direction:vec4<f32>,grass_eye:vec4<f32>,grass_wind:vec4<f32>,grass_history:array<vec4<f32>,80>,fog_range:vec4<f32>,fog_color:vec4<f32>,lod:vec4<f32>};
@group(0) @binding(0) var<uniform> camera:Camera; @group(0) @binding(1) var atlas:texture_2d<f32>; @group(0) @binding(2) var atlas_sampler:sampler;
@group(0) @binding(3) var<storage,read> rocking:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read> leaf_normals:array<vec4<f32>>;
fn rock_sample(corner:u32,time:f32)->vec3<f32>{
 let phase=fract(time/16.)*64.;let idx=u32(floor(phase));let next=(idx+1u)%64u;let f=fract(phase);let p=corner*64u;
 var result=mix(rocking[p+idx].xyz,rocking[p+next].xyz,f);
 // Bring the end of the measured interval smoothly back to its start.
 result=mix(result,rocking[p].xyz,clamp((phase-60.)/4.,0.,1.));return result;
}
fn fog_view_depth(world:vec3<f32>)->f32 {
 // Re-evaluate clip-space W from the perspective-correct world position.
 return max((camera.matrix*vec4<f32>(world,1.)).w,0.);
}
fn water_transmittance(world:vec3<f32>)->f32 {
 // Existing camera clip/eye constants are populated for every render view.
 // Only clipped refraction from an above-water camera consumes water.
 if camera.clip.y>=-0.5 || camera.grass_eye.y<=camera.clip.x {return 1.;}
 let depth=max(camera.clip.x-world.y,0.);
 if depth<=0. {return 1.;}
 let delta=camera.grass_eye.xyz-world;
 let distance=length(delta);
 if delta.y<=0.000001 || distance<=0.000001 {return 0.;}
 return exp2(-0.075*depth*distance/delta.y);
}
fn fogged(linear:vec3<f32>,world:vec3<f32>)->vec3<f32> {
 var f=0.;
 if camera.fog_range.z>0.5 {
  let view_z=fog_view_depth(world);
  let span=camera.fog_range.y-camera.fog_range.x;
  f=select(clamp((view_z-camera.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,view_z>=camera.fog_range.y),abs(span)<0.001);
 }
 // Water transmission is independent of regional distance fog: even when a
 // region has no table-fog record, submerged tree colour still absorbs light.
 let water=water_transmittance(world);
 let combined_fog=1.-(1.-f)*water;
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(encoded,camera.fog_color.rgb,combined_fog);
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
struct In {@builtin(vertex_index) vertex_id:u32,@location(0) pos:vec3<f32>,@location(1) normal:vec3<f32>,@location(2) uv:vec2<f32>,@location(3) color:vec4<f32>,@location(4) kind:f32,@location(5) instance:vec3<f32>,@location(6) info:vec2<f32>,@location(7) tex_h:u32,@location(8) alpha_ref:u32};
struct Out {@builtin(position) pos:vec4<f32>,@location(0) normal:vec3<f32>,@location(1) uv:vec2<f32>,@location(2) color:vec4<f32>,@location(3) @interpolate(flat) kind:f32,@location(4) world:vec3<f32>,@location(5) @interpolate(flat) tree:vec3<f32>,@location(6) @interpolate(flat) tex_h:f32,@location(7) @interpolate(flat) fade:vec2<f32>,@location(8) @interpolate(flat) alpha_ref:u32};
@vertex fn vs(i:In)->Out {var o:Out;var pos=i.pos+i.instance;var norm=i.normal;
 if i.kind>4.5 {let d=normalize(camera.direction.xyz);let az=atan2(d.z,d.x);let el=asin(clamp(d.y,-1.,1.));let rho=i.color.w;var corner=i.normal;let rock_index=i.tex_h>>8u;if rock_index>0u {corner=mix(corner,rock_sample(rock_index-1u,camera.tree_bounds.w),clamp(camera.tree_bounds.z,0.,1.));}let q=vec3<f32>(cos(el)*corner.x-sin(el)*corner.z,corner.y,sin(el)*corner.x+cos(el)*corner.z);let a=az+rho;var v=vec3<f32>(cos(a)*q.x-sin(a)*q.y,sin(a)*q.x+cos(a)*q.y,q.z);if rho>1000. {v=corner;}pos+=vec3<f32>(v.x,v.z,v.y);norm=vec3<f32>(0.,1.,0.);if i.vertex_id<arrayLength(&leaf_normals) {let n=leaf_normals[i.vertex_id];if n.w>0.5 {norm=n.xyz;}}}
 o.pos=camera.matrix*vec4<f32>(pos,1.);o.world=pos;o.normal=norm;o.uv=i.uv;o.color=i.color;o.kind=i.kind;o.tree=i.instance;o.tex_h=f32(i.tex_h & 127u)*128.;o.fade=i.info;o.alpha_ref=i.alpha_ref;return o;}
@fragment fn fs(i:Out)->@location(0) vec4<f32>{if camera.clip.y!=0. && (i.world.y-camera.clip.x)*camera.clip.y<0. {discard;}

 let dims=vec2<f32>(textureDimensions(atlas));let scale=vec2<f32>(i.color.z,i.tex_h/dims.y);var dx=dpdx(i.uv);var dy=dpdy(i.uv);let lod_bias=exp2(camera.lod.x);dx*=lod_bias;dy*=lod_bias;let mip=clamp(log2(max(max(length(dx*dims),length(dy*dims)),1.)),0.,7.);let halfpixel=min(vec2<f32>(0.5*exp2(mip))/dims,scale*0.499);let uv=i.color.xy+clamp(fract((i.uv-i.color.xy)/scale)*scale,halfpixel,scale-halfpixel);let tex=textureSampleGrad(atlas,atlas_sampler,uv,dx,dy);if i.alpha_ref!=0xffffffffu {let threshold=bitcast<f32>(i.alpha_ref);if tex.a<=threshold {discard;}}else if i.kind>3.5 && tex.a<0.5 {discard;}
 // Complementary ordered coverage: old and new snapshots never both draw
 // at the same pixel, while the tree is fully visible at the endpoints.
 if i.fade.y>0.5 {let xy=vec2<u32>(i.pos.xy)%vec2<u32>(4u);let b=(((xy.x&1u)^(xy.y&1u))*8u)+((xy.y&1u)*4u)+((((xy.x>>1u)&1u)^((xy.y>>1u)&1u))*2u)+((xy.y>>1u)&1u);let d=(f32(b)+0.5)/16.;if (i.fade.y<1.5 && d<i.fade.x)||(i.fade.y>1.5 && d>=i.fade.x){discard;}}
 if camera.region_ambient.w>0.5 && camera.sun.w>=0. {
  let lambert=max(dot(normalize(i.normal),normalize(camera.region_direction.xyz)),0.);
  // These are regional light coefficients, not the original SpeedTree
  // material constants. The branch-only PS 1.4 shader's x2 operation is
  // not justified for every leaf/frond; applying it clipped bright foliage.
  // Keep this fallback bounded and mostly neutral until material constants,
  // normals and the separate client passes are recovered.
  let rgb=clamp(camera.region_ambient.rgb+camera.region_diffuse.rgb*lambert,vec3<f32>(0.),vec3<f32>(1.));
  let luminance=dot(rgb,vec3<f32>(0.2126,0.7152,0.0722));
  let light=mix(vec3<f32>(luminance),rgb,0.25);
  return vec4<f32>(fogged(tex.rgb*light,i.world),1.);
 }
 let ndl_raw=dot(normalize(i.normal),normalize(camera.sun.xyz));let ndl=select(max(ndl_raw,0.),abs(ndl_raw)*0.85+0.15,camera.sun.w<0. && i.kind>3.5);let light=camera.light.x+camera.light.y*ndl;return vec4<f32>(fogged(tex.rgb*light*camera.light.zww,i.world),1.);}
"#;
impl FarTrees {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        trees: &Trees,
        uniform: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        authored_mips: bool,
    ) -> Self {
        // Real composite/bark textures are mostly 512 and some 1024px.
        // Pack rectangular native-size tiles into an 8192² atlas instead of
        // reducing everything to 256². Keep the low-memory diagnostic mode.
        let small = std::env::var_os("PW_SMALL_TREE_ATLAS").is_some()
            || device.limits().max_texture_dimension_2d < 8192;
        let side = if small { 2048usize } else { 8192usize };
        let grid = side / 128;
        let mut occupied = vec![false; grid * grid];
        // Grow the tree atlas dynamically by rows instead of allocating the
        // full 8192x8192x4 (256 MiB) up front; empty or sparse tree maps then
        // allocate only the rows they actually pack.
        let mut atlas: Vec<u8> = Vec::new();
        let mut used_rows: usize = 0;
        let mut vertices = Vec::new();
        let mut rocking_samples: Vec<[f32; 4]> = Vec::new();
        let mut leaf_normals: Vec<[f32; 4]> = Vec::new();
        let mut indices = Vec::new();
        let mut instances = Vec::with_capacity(trees.placements.len() * 3);
        let mut batches = Vec::with_capacity(trees.types.len() * 3);
        let mut groups = Vec::with_capacity(trees.types.len());
        let mut placements_by_type: Vec<Vec<[f32; 3]>> = vec![Vec::new(); trees.types.len()];
        for placement in &trees.placements {
            if let Some(bucket) = placements_by_type.get_mut(placement.tree_type as usize) {
                bucket.push(placement.position);
            }
        }
        let mut rt_species = Vec::new();
        let mut packed_count = 0usize;
        let mut mip_slots: Vec<(
            usize,
            usize,
            usize,
            usize,
            bool,
            Option<Vec<image::RgbaImage>>,
        )> = Vec::new();
        if authored_mips {
            let request = crate::vegetation_material::Request {
                mips: crate::vegetation_material::RequestedMips::Authored,
                anisotropy: crate::vegetation_material::policy(
                    crate::vegetation_material::Material::TreeLeaf,
                )
                .renderer_sampler
                .anisotropy,
                cutout_reference: Some(crate::vegetation_material::LEGACY_TREE_ALPHA_REF),
            };
            if let Err(rejection) = crate::vegetation_material::validate(
                crate::vegetation_material::Material::TreeLeaf,
                request,
            ) {
                eprintln!(
                    "P4.07 tree authored-mip experiment: unsupported native sampler pair kept explicit: {rejection:?}; retaining the viewer's approved sampler"
                );
            }
            let cached_alpha_lods = trees
                .types
                .iter()
                .flatten()
                .flat_map(|tree| &tree.lods)
                .filter(|lod| lod.branch_alpha_ref.is_some() && lod.frond_alpha_ref.is_some())
                .count();
            if cached_alpha_lods == 0 {
                eprintln!(
                    "P4.07 tree authored-mip experiment: cache contains no PWTREE08 branch/frond alpha references; legacy tree cutout remains active"
                );
            }
        }
        for (id, maybe) in trees.types.iter().enumerate() {
            let Some(tree) = maybe else { continue };
            let positions = std::mem::take(&mut placements_by_type[id]);
            if positions.is_empty() {
                continue;
            }
            let height = tree.lods[0]
                .branch
                .vertices
                .iter()
                .chain(tree.lods[0].frond.vertices.iter())
                .map(|v| v.pos[2])
                .chain(tree.lods[0].leaves.iter().map(|l| l.center[2]))
                .fold(5.0f32, f32::max);
            let mut slots = Vec::with_capacity(2);
            for (part_index, img) in [&tree.bark, &tree.composite].into_iter().enumerate() {
                let w = if small {
                    128
                } else {
                    (img.width() as usize).next_power_of_two().clamp(128, side)
                };
                let h = if small {
                    128
                } else {
                    (img.height() as usize).next_power_of_two().clamp(128, side)
                };
                let (gw, gh) = (w / 128, h / 128);
                let mut found = None;
                'scan: for gy in 0..=grid - gh {
                    for gx in 0..=grid - gw {
                        if (gy..gy + gh).all(|y| (gx..gx + gw).all(|x| !occupied[y * grid + x])) {
                            found = Some((gx, gy));
                            break 'scan;
                        }
                    }
                }
                let Some((gx, gy)) = found else {
                    eprintln!("Tree atlas exhausted; type {id} omitted");
                    break;
                };
                for y in gy..gy + gh {
                    for x in gx..gx + gw {
                        occupied[y * grid + x] = true;
                    }
                }
                let (ox, oy) = (gx * 128, gy * 128);
                let need_rows = oy + h;
                if need_rows > used_rows {
                    used_rows = need_rows;
                    let needed_bytes = used_rows * side * 4;
                    if atlas.len() < needed_bytes {
                        let grown = (atlas.len().max(side * 128 * 4) * 2)
                            .max(needed_bytes)
                            .min(side * side * 4);
                        atlas.resize(grown, 0);
                    }
                }
                for y in 0..h {
                    for x in 0..w {
                        let sx = (x as u32 * img.width() / w as u32).min(img.width() - 1);
                        let sy = (y as u32 * img.height() / h as u32).min(img.height() - 1);
                        let dst = ((oy + y) * side + ox + x) * 4;
                        atlas[dst..dst + 4].copy_from_slice(&img.get_pixel(sx, sy).0);
                    }
                }
                slots.push((ox, oy, w, h));
                let authored = if authored_mips {
                    let mips = if part_index == 0 {
                        &tree.bark_mips
                    } else {
                        &tree.composite_mips
                    };
                    if crate::vegetation_textures::chain_fits(mips, w as u32, h as u32, 7) {
                        if mips.len() > 7 {
                            eprintln!(
                                "P4.07 tree type {id} part {part_index}: {} authored tail mip(s) beyond atlas LOD 7 retained in cache but not separately uploadable",
                                mips.len() - 7
                            );
                        }
                        Some(mips[..7].to_vec())
                    } else {
                        eprintln!(
                            "P4.07 tree type {id} part {part_index}: authored mip chain does not fit {w}x{h} atlas slot; keeping generated mips for this tile"
                        );
                        None
                    }
                } else {
                    None
                };
                mip_slots.push((ox, oy, w, h, part_index == 1, authored));
                packed_count += 1;
            }
            if slots.len() != 2 {
                continue;
            }
            let first_batch = batches.len();
            let mut comp_min_a = 255u8;
            let mut comp_max_a = 0u8;
            for p in tree.composite.pixels() {
                comp_min_a = comp_min_a.min(p[3]);
                comp_max_a = comp_max_a.max(p[3]);
            }
            let mut species_rt_lods: Vec<Vec<RtTreeTriangle>> = Vec::with_capacity(tree.lods.len());
            let mut species_lod_diags: Vec<RtTreeLodDiag> = Vec::with_capacity(tree.lods.len());
            let composite_slot = slots[1];
            let rt_composite_mips = rt_foliage_mip_images(
                &tree.composite,
                &tree.composite_mips,
                composite_slot.2 as u32,
                composite_slot.3 as u32,
                authored_mips,
            );
            for (lod_index, lod) in tree.lods.iter().enumerate() {
                // LOD 0 uses the atlas base; LOD 1/2 use progressively coarser
                // alpha mips. The ray scene has no raster derivatives, so this
                // keeps the foliage cutout frequency aligned with geometry LOD.
                let rocking_base = rocking_samples.len() / 64;
                rocking_samples.extend(lod.rocking.iter().map(|v| [v[0], v[1], v[2], 0.]));
                let start_inst = instances.len() as u32;
                instances.extend(
                    positions
                        .iter()
                        .map(|&position| Instance::new(position, [0., 0.])),
                );
                let end_inst = instances.len() as u32;
                let start_idx = indices.len() as u32;
                for (part, slot_id, kind, native_alpha) in [
                    (&lod.branch, slots[0], 3., lod.branch_alpha_ref),
                    (&lod.frond, slots[1], 4., lod.frond_alpha_ref),
                ] {
                    let alpha_ref = if authored_mips {
                        native_alpha
                            .filter(|reference| *reference >= 0.)
                            .map_or(u32::MAX, |reference| (reference / 255.).to_bits())
                    } else {
                        u32::MAX
                    };
                    let ox = slot_id.0 as f32 / side as f32;
                    let oy = slot_id.1 as f32 / side as f32;
                    let scale = slot_id.2 as f32 / side as f32;
                    let sy = slot_id.3 as f32 / side as f32;
                    let base = vertices.len() as u32;
                    for v in &part.vertices {
                        vertices.push(Vertex {
                            pos: [v.pos[0], v.pos[2], v.pos[1]],
                            normal: [v.normal[0], v.normal[2], v.normal[1]],
                            uv: [ox + v.uv[0] * scale, oy + v.uv[1] * sy],
                            color: [ox, oy, scale, 0.],
                            kind,
                            diffuse: (slot_id.3 as u32 / 128) | if small { 128 } else { 0 },
                            night_diffuse: alpha_ref,
                        });
                    }
                    indices.extend(part.indices.iter().map(|i| base + *i));
                }
                let slot_id = slots[1];
                let ox = slot_id.0 as f32 / side as f32;
                let oy = slot_id.1 as f32 / side as f32;
                let scale = slot_id.2 as f32 / side as f32;
                let sy = slot_id.3 as f32 / side as f32;
                for leaf in &lod.leaves {
                    let base = vertices.len() as u32;
                    if let Some(normal) = leaf.normal {
                        leaf_normals.resize(vertices.len() + 4, [0.; 4]);
                        for value in &mut leaf_normals[vertices.len()..vertices.len() + 4] {
                            *value = [normal[0], normal[2], normal[1], 1.];
                        }
                    }
                    for j in 0..4 {
                        vertices.push(Vertex {
                            pos: [leaf.center[0], leaf.center[2], leaf.center[1]],
                            normal: leaf.corners[j],
                            uv: [ox + leaf.uv[j][0] * scale, oy + leaf.uv[j][1] * sy],
                            color: [ox, oy, scale, leaf.rho],
                            kind: 5.,
                            diffuse: (slot_id.3 as u32 / 128)
                                | if small { 128 } else { 0 }
                                | if lod.rocking_clusters > 0 {
                                    ((rocking_base + leaf.cluster as usize * 4 + j + 1) as u32) << 8
                                } else {
                                    0
                                },
                            night_diffuse: if authored_mips {
                                (84.0f32 / 255.0f32).to_bits()
                            } else {
                                u32::MAX
                            },
                        });
                    }
                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                }
                // Phase 4: Harvest this runtime SpeedTree LOD's real branch,
                // frond, and leaf triangles with per-material alpha coverage.
                {
                    let mut lod_tris = Vec::new();
                    let mut ldiag = RtTreeLodDiag::default();
                    let foliage_image = &rt_composite_mips
                        [lod_index.min(rt_composite_mips.len().saturating_sub(1))];
                    for (part, img, is_foliage) in [
                        (&lod.branch, &tree.bark, false),
                        (&lod.frond, foliage_image, true),
                    ] {
                        for tri in part.indices.chunks_exact(3) {
                            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                            if i0 >= part.vertices.len()
                                || i1 >= part.vertices.len()
                                || i2 >= part.vertices.len()
                            {
                                continue;
                            }
                            let v0 = &part.vertices[i0];
                            let v1 = &part.vertices[i1];
                            let v2 = &part.vertices[i2];
                            let a = glam::Vec3::new(v0.pos[0], v0.pos[2], v0.pos[1]);
                            let b = glam::Vec3::new(v1.pos[0], v1.pos[2], v1.pos[1]);
                            let c = glam::Vec3::new(v2.pos[0], v2.pos[2], v2.pos[1]);
                            let cross = (b - a).cross(c - a);
                            if cross.length_squared() <= 1e-8 {
                                continue;
                            }
                            let norm = cross.normalize();
                            if is_foliage {
                                let frond_cutoff =
                                    raster_alpha_cutoff(authored_mips, lod.frond_alpha_ref);
                                let (mask, albedo, hits) =
                                    sample_tri_alpha_mask49(img, v0.uv, v1.uv, v2.uv, frond_cutoff);
                                if hits == 0 {
                                    ldiag.frond_skipped += 1;
                                    continue;
                                }
                                let coverage = hits as f32 / 49.0;
                                ldiag.frond_tris += 1;
                                if hits < 49 {
                                    ldiag.masked_tris += 1;
                                } else {
                                    ldiag.unmasked_foliage_tris += 1;
                                }
                                lod_tris.push(RtTreeTriangle {
                                    a,
                                    b,
                                    c,
                                    normal: norm,
                                    albedo,
                                    roughness: 0.85,
                                    opacity: foliage_opacity(hits),
                                    alpha_mask: mask,
                                    leaf_center: glam::Vec3::ZERO,
                                    leaf_rho: -1.0,
                                });
                            } else {
                                let uv_mid = [
                                    (v0.uv[0] + v1.uv[0] + v2.uv[0]) / 3.0,
                                    (v0.uv[1] + v1.uv[1] + v2.uv[1]) / 3.0,
                                ];
                                let (albedo, _) = sample_rgba_uv(img, uv_mid);
                                ldiag.branch_tris += 1;
                                lod_tris.push(RtTreeTriangle {
                                    a,
                                    b,
                                    c,
                                    normal: norm,
                                    albedo,
                                    roughness: 0.90,
                                    opacity: 1.0,
                                    alpha_mask: 0,
                                    leaf_center: glam::Vec3::ZERO,
                                    leaf_rho: -1.0,
                                });
                            }
                        }
                    }
                    for leaf in &lod.leaves {
                        let center =
                            glam::Vec3::new(leaf.center[0], leaf.center[2], leaf.center[1]);
                        // Sample exact 7x7 (49-bit) alpha cutout silhouettes from `tree.composite`
                        // for both triangles of this SpeedTree leaf card so shadows and reflections
                        // reproduce real individual leaves instead of flat boxy quads/diamonds.
                        let leaf_cutoff = leaf_alpha_cutoff(authored_mips);
                        let (m0, a0, h0) = sample_tri_alpha_mask49(
                            foliage_image,
                            leaf.uv[0],
                            leaf.uv[1],
                            leaf.uv[2],
                            leaf_cutoff,
                        );
                        let (m1, a1, h1) = sample_tri_alpha_mask49(
                            foliage_image,
                            leaf.uv[0],
                            leaf.uv[2],
                            leaf.uv[3],
                            leaf_cutoff,
                        );
                        if h0 + h1 == 0 {
                            ldiag.leaves_skipped += 1;
                            continue;
                        }
                        if leaf.rho > 1000.0 {
                            ldiag.fixed_3d_leaves += 1;
                        } else {
                            ldiag.billboard_leaves += 1;
                        }
                        // The raster vertex shader emits one leaf quad (two triangles), facing the current camera.
                        // Do not invent a second crossed card for RT shadows/reflections.
                        // Keep source corners in local Y-up space; apply the camera-facing
                        // transform when this cached species is harvested for the current frame.
                        let p = [0, 1, 2, 3].map(|j| {
                            let c = leaf.corners[j];
                            center + glam::Vec3::new(c[0], c[2], c[1])
                        });
                        // Match the raster vertex shader: it uses the source leaf normal when present,
                        // otherwise its forced world-up normal.
                        let leaf_normal = leaf
                            .normal
                            .map(|n| glam::Vec3::new(n[0], n[2], n[1]).normalize_or_zero())
                            .filter(|n| n.length_squared() > 0.25)
                            .unwrap_or(glam::Vec3::Y);
                        if h0 > 0 && (p[1] - p[0]).cross(p[2] - p[0]).length_squared() > 1e-8 {
                            if h0 < 49 {
                                ldiag.masked_tris += 1;
                            } else {
                                ldiag.unmasked_foliage_tris += 1;
                            }
                            lod_tris.push(RtTreeTriangle {
                                a: p[0],
                                b: p[1],
                                c: p[2],
                                normal: leaf_normal,
                                albedo: a0,
                                roughness: 0.84,
                                opacity: foliage_opacity(h0),
                                alpha_mask: m0,
                                leaf_center: center,
                                leaf_rho: leaf.rho,
                            });
                        }
                        if h1 > 0 && (p[2] - p[0]).cross(p[3] - p[0]).length_squared() > 1e-8 {
                            if h1 < 49 {
                                ldiag.masked_tris += 1;
                            } else {
                                ldiag.unmasked_foliage_tris += 1;
                            }
                            lod_tris.push(RtTreeTriangle {
                                a: p[0],
                                b: p[2],
                                c: p[3],
                                normal: leaf_normal,
                                albedo: a1,
                                roughness: 0.84,
                                opacity: foliage_opacity(h1),
                                alpha_mask: m1,
                                leaf_center: center,
                                leaf_rho: leaf.rho,
                            });
                        }
                    }
                    species_rt_lods.push(lod_tris);
                    species_lod_diags.push(ldiag);
                }
                batches.push(Batch {
                    indices: start_idx..indices.len() as u32,
                    instances: start_inst..end_inst,
                });
            }
            rt_species.push(RtTreeSpecies {
                species_id: id,
                height,
                composite_size: tree.composite.dimensions(),
                composite_alpha_min: comp_min_a,
                composite_alpha_max: comp_max_a,
                positions: positions.clone(),
                lods: species_rt_lods,
                lod_diags: species_lod_diags,
            });
            groups.push(Group {
                positions,
                height,
                first_batch,
            });
        }
        // A valid non-empty allocation is required even for an empty user tree cache.
        if vertices.is_empty() {
            vertices.push(Vertex::zeroed())
        }
        if indices.is_empty() {
            indices.push(0)
        }
        if instances.is_empty() {
            instances.push(Instance::zeroed())
        }
        // Trim the tree atlas GPU texture to the exact packed row extent
        // (multiple of 128 so all 7 lower mip levels stay tile-aligned). Maps
        // with no trees allocate a minimal 128x128 placeholder (~64 KiB)
        // instead of 8192x8192 (~341 MiB VRAM).
        let (atlas_w, atlas_h) = if mip_slots.is_empty() {
            atlas.clear();
            atlas.resize(128 * 128 * 4, 0);
            (128usize, 128usize)
        } else {
            let trimmed_h = ((used_rows + 127) & !127).clamp(128, side);
            atlas.truncate(trimmed_h * side * 4);
            atlas.resize(trimmed_h * side * 4, 0);
            if trimmed_h < side {
                let v_scale = side as f32 / trimmed_h as f32;
                for v in &mut vertices {
                    v.uv[1] *= v_scale;
                    v.color[1] *= v_scale;
                }
            }
            (side, trimmed_h)
        };
        let image = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("far tree textures"),
            size: wgpu::Extent3d {
                width: atlas_w as u32,
                height: atlas_h as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 8,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &image,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some((atlas_w * 4) as u32),
                rows_per_image: Some(atlas_h as u32),
            },
            wgpu::Extent3d {
                width: atlas_w as u32,
                height: atlas_h as u32,
                depth_or_array_layers: 1,
            },
        );
        // Generate each rectangle separately so lower mip levels cannot blend
        // neighboring trees. Keep unadjusted alpha for the next reduction;
        // preserve approximate cutout coverage only in the uploaded copy.
        for &(ox, oy, w, h, cutout, ref authored) in &mip_slots {
            if let Some(chain) = authored {
                for (index, img) in chain.iter().enumerate() {
                    let level = index as u32 + 1;
                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &image,
                            mip_level: level,
                            origin: wgpu::Origin3d {
                                x: (ox >> level) as u32,
                                y: (oy >> level) as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        img.as_raw(),
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(img.width() * 4),
                            rows_per_image: Some(img.height()),
                        },
                        wgpu::Extent3d {
                            width: img.width(),
                            height: img.height(),
                            depth_or_array_layers: 1,
                        },
                    );
                }
                continue;
            }
            // Generated in parallel below.
            let _ = (ox, oy, w, h, cutout);
        }
        // Generate all non-authored foliage mip chains concurrently across CPU
        // cores before submitting write_texture calls.
        let gen_slots: Vec<(usize, usize, usize, usize, bool)> = mip_slots
            .iter()
            .filter_map(|(ox, oy, w, h, cutout, authored)| {
                if authored.is_none() {
                    Some((*ox, *oy, *w, *h, *cutout))
                } else {
                    None
                }
            })
            .collect();
        if !gen_slots.is_empty() {
            let mip_workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(32)
                .min(gen_slots.len());
            let build_slot = |ox: usize,
                              oy: usize,
                              w: usize,
                              h: usize,
                              cutout: bool,
                              atlas_ref: &[u8]|
             -> Vec<(u32, u32, u32, u32, u32, Vec<u8>)> {
                let mut raw = vec![0u8; w * h * 4];
                for y in 0..h {
                    let src = ((oy + y) * side + ox) * 4;
                    raw[y * w * 4..(y + 1) * w * 4].copy_from_slice(&atlas_ref[src..src + w * 4]);
                }
                let source_coverage = crate::foliage_mips::coverage(&raw);
                let (mut mw, mut mh) = (w, h);
                let mut levels = Vec::with_capacity(7);
                for level in 1..8u32 {
                    raw = crate::foliage_mips::downsample(&raw, mw, mh, cutout);
                    mw /= 2;
                    mh /= 2;
                    let corrected = if cutout {
                        crate::foliage_mips::preserve_coverage(&raw, source_coverage)
                    } else {
                        raw.clone()
                    };
                    levels.push((
                        level,
                        (ox >> level) as u32,
                        (oy >> level) as u32,
                        mw as u32,
                        mh as u32,
                        corrected,
                    ));
                }
                levels
            };
            let generated: Vec<Vec<(u32, u32, u32, u32, u32, Vec<u8>)>> = if mip_workers > 1 {
                let chunk_size = (gen_slots.len() + mip_workers - 1) / mip_workers;
                let atlas_ref = &atlas;
                std::thread::scope(|s| {
                    let handles: Vec<_> = gen_slots
                        .chunks(chunk_size)
                        .map(|chunk| {
                            s.spawn(move || {
                                chunk
                                    .iter()
                                    .map(|&(ox, oy, w, h, cutout)| {
                                        build_slot(ox, oy, w, h, cutout, atlas_ref)
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap())
                        .collect()
                })
            } else {
                gen_slots
                    .iter()
                    .map(|&(ox, oy, w, h, cutout)| build_slot(ox, oy, w, h, cutout, &atlas))
                    .collect()
            };
            for levels in generated {
                for (level, dx, dy, mw, mh, corrected) in levels {
                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &image,
                            mip_level: level,
                            origin: wgpu::Origin3d { x: dx, y: dy, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &corrected,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(mw * 4),
                            rows_per_image: Some(mh),
                        },
                        wgpu::Extent3d {
                            width: mw,
                            height: mh,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }
        drop(atlas);
        let view = image.create_view(&Default::default());
        // The authored-mip experiment changes mip sources only. Keep the
        // already-approved renderer sampler (including 16x anisotropy) intact.
        let anisotropy_clamp = crate::vegetation_material::LEGACY_ANISOTROPY;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp,
            ..Default::default()
        });
        if rocking_samples.is_empty() {
            rocking_samples.resize(64, [0.; 4]);
        }
        let rocking_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("client sampled SpeedTree leaf rocking"),
            contents: bytemuck::cast_slice(&rocking_samples),
            usage: wgpu::BufferUsages::STORAGE,
        });
        if leaf_normals.is_empty() {
            leaf_normals.push([0.; 4]);
        }
        let leaf_normal_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("SpeedTree leaf normals"),
            contents: bytemuck::cast_slice(&leaf_normals),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("far trees"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let make_bind = |b: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("far trees bindings"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: b.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: rocking_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: leaf_normal_buffer.as_entire_binding(),
                    },
                ],
            })
        };
        let bind = make_bind(uniform);
        let reflected_bind = make_bind(reflected);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("far trees shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("far trees"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        const ATTR: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32,7=>Uint32,8=>Uint32];
        // `Instance` is two `vec4`s (see its doc): position at 0, transition
        // info at 16. `vertex_attr_array!` would pack them at 0 and 12.
        const INST: [wgpu::VertexAttribute; 2] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 16,
                shader_location: 6,
            },
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("instanced far trees"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &ATTR,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INST,
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("far tree vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("far tree indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let instance_count = instances.len() / 3;
        // The loaded records seed the buffer and are then released: production
        // never rebuilds them on the CPU again (the compute pass owns the
        // buffer), so keeping a second world-sized copy in RAM would be dead
        // weight.
        let seed = instances;
        let capacity = seed.len();
        // The instance buffer is the LOD compute pass's output as well as the
        // vertex input, so it keeps every (group, level) slice allocated once.
        let instances = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tree instances (LOD compute target)"),
            contents: bytemuck::cast_slice(&seed),
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        });
        // Placement table, per-group constants and the indirect args are built
        // once here. Nothing per-frame is uploaded any more: a camera move only
        // dispatches `LOD_COMPUTE` over the placement table.
        let total_placements: usize = groups.iter().map(|g| g.positions.len()).sum();
        let mut placement_data = Vec::with_capacity(total_placements);
        for (index, group) in groups.iter().enumerate() {
            for &position in &group.positions {
                placement_data.push(Placement {
                    pos_height: [position[0], position[1], position[2], 0.],
                    group: index as u32,
                    pad: [0; 3],
                });
            }
        }
        // `args_base` is a batch index (`first_batch + level`), not a group index.
        let mut group_data = group_table(&groups, &batches);
        if placement_data.is_empty() {
            placement_data.push(Placement {
                pos_height: [0.; 4],
                group: 0,
                pad: [0; 3],
            });
        }
        if group_data.is_empty() {
            group_data.push(GroupInfo {
                height: 5.,
                args_base: 0,
                first_instance: 0,
                capacity: 0,
            });
        }
        let spatial_cells = build_tree_spatial_cells(&placement_data, &group_data);
        let placement_count = placement_data.len() as u32;
        // WebGPU guarantees four storage buffers per compute stage. Pack the
        // visible-index tail into the placement buffer so tree LOD uses exactly
        // four storage bindings (placements, groups, instances and indirect args).
        // On an adapter whose maximum storage binding cannot hold both regions,
        // keep only the placement prefix and let the shader conservatively scan
        // every placement instead of requesting an unsupported fifth binding.
        let placement_bytes = bytemuck::cast_slice::<Placement, u8>(&placement_data);
        let active_index_offset = placement_bytes.len() as u64;
        let active_index_bytes = (placement_data.len().max(1) * std::mem::size_of::<u32>()) as u64;
        let combined_size = active_index_offset.saturating_add(active_index_bytes);
        let can_pack_active_indices = combined_size
            <= u64::from(device.limits().max_storage_buffer_binding_size)
            && combined_size <= device.limits().max_buffer_size;
        let mut placement_storage = placement_bytes.to_vec();
        if can_pack_active_indices {
            placement_storage.resize(combined_size as usize, 0);
        }
        let placement_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tree placements and visible-index tail"),
            contents: &placement_storage,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let active_index_offset = can_pack_active_indices.then_some(active_index_offset);
        let lod_group_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tree lod groups"),
            contents: bytemuck::cast_slice(&group_data),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let lod_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tree lod params"),
            size: std::mem::size_of::<LodParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // One indirect call for the whole batch list needs `multi_draw`, and
        // pointing each record at its own slice of the instance buffer needs
        // `first_instance` inside an indirect call. Both are optional features;
        // without them the batches are drawn one at a time, with the instance
        // buffer sliced per draw instead.
        let one_call = multi_draw(device) && base_instance(device);
        let mut args = Vec::with_capacity(batches.len().max(1));
        for batch in &batches {
            let mut args_record = record(batch.indices.clone(), batch.instances.clone());
            // The compute pass owns the counts: every record starts empty and is
            // bumped per frame.
            args_record.instance_count = 0;
            if !one_call {
                // Without `first_instance` inside an indirect call, the slice
                // comes from the vertex buffer instead.
                args_record.first_instance = 0;
            }
            args.push(args_record);
        }
        if args.is_empty() {
            args.push(record(0..0, 0..0));
        }
        let lod_args_reset = bytemuck::cast_slice::<DrawIndexedArgs, u8>(&args).to_vec();
        let lod_args = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tree lod indirect args"),
            contents: &lod_args_reset,
            usage: wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        });
        let lod_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tree lod compute"),
            source: wgpu::ShaderSource::Wgsl(LOD_COMPUTE.into()),
        });
        let lod_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tree lod"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
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
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let lod_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("tree lod selection"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("tree lod"),
                    bind_group_layouts: &[&lod_layout],
                    push_constant_ranges: &[],
                }),
            ),
            module: &lod_module,
            entry_point: "main",
        });
        // A sampled texture keeps the non-Hi-Z fallback valid on devices
        // where Hi-Z allocation or R32Float storage textures are unavailable.
        let hiz_dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("disabled Hi-Z fallback texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let hiz_dummy_view = hiz_dummy_texture.create_view(&Default::default());
        let lod_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tree lod"),
            layout: &lod_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: lod_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: placement_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: lod_group_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: lod_args.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&hiz_dummy_view),
                },
            ],
        });
        eprintln!(
            "Uploaded {} tree placements in {} GPU LOD batches ({} textures, {}px atlas{} with foliage mips, {})",
            instance_count,
            batches.len(),
            packed_count,
            side,
            if small { " low-memory" } else { " source-size" },
            if one_call {
                "one multi-draw"
            } else {
                "per-batch indirect draws"
            }
        );
        Self {
            pipeline,
            bind,
            reflected_bind,
            vertices,
            indices,
            instances,
            batches,
            groups,
            rt_species: rt_species.into(),
            capacity,
            spatial_cells,
            active_placement_indices: RefCell::new(Vec::new()),
            active_index_offset,
            last_resident_update: RefCell::new(None),
            lod_pipeline,
            lod_bind,
            lod_layout,
            placement_buffer,
            group_buffer: lod_group_buffer,
            lod_params,
            placements: placement_count,
            lod_args,
            lod_args_reset,
            multi_draw: one_call,
        }
    }
    /// Select every placement's LOD on the GPU.
    ///
    /// The CPU used to walk the whole placement table here — distance, LOD
    /// choice and a full instance-buffer upload — on every frame the camera
    /// moved, which on a whole-world load is tens of thousands of placements.
    /// Now the placement table is uploaded once at load and a single compute
    /// dispatch (`LOD_COMPUTE`) writes the visible instances and the per-batch
    /// instance counts the indirect draws read. Nothing is uploaded per frame,
    /// and the pass is skipped entirely when the camera, scale and cutoff are
    /// unchanged, exactly like the old staging rebuild.
    /// Cheap immutable snapshot for the Raywatcher scene worker. The backing
    /// species/LOD geometry is shared; no tree mesh data is copied on the
    /// render thread when RT scene construction starts.
    pub fn rt_species_snapshot(&self) -> std::sync::Arc<[RtTreeSpecies]> {
        std::sync::Arc::clone(&self.rt_species)
    }

    /// Harvest runtime-generated SpeedTree LOD geometry (branches, fronds,
    /// and leaves with per-material alpha) near `eye`, selecting the raster-
    /// matching LOD. Tries progressively coarser real LODs under budget pressure
    /// and skips an instance if no real LOD can fit.
    ///
    /// Returns `(real_tree_instances, real_tree_tris, budget_skips)`.
    pub fn harvest_rt_triangles(
        &self,
        eye: glam::Vec3,
        view_direction: glam::Vec3,
        max_dist: f32,
        lod_scale: f32,
        tri_budget: usize,
        out: &mut Vec<RtTreeTriangle>,
    ) -> (usize, usize, usize) {
        // Opt #30: Pre-allocate RT tree triangle staging Vec to eliminate multi-MB reallocs during harvest.
        out.reserve(tri_budget.min(65_536));
        harvest_rt_tree_species(
            self.rt_species.as_ref(),
            eye,
            view_direction,
            max_dist,
            lod_scale,
            tri_budget,
            out,
        )
    }

    /// Build a human-readable diagnostic report of all SpeedTree species near `eye`
    /// for `raywatcher-diag.log` (flagging leaf-less, opaque-composite, or skipped foliage).
    pub fn diagnostics_report(&self, eye: glam::Vec3, max_dist: f32) -> String {
        use std::fmt::Write;
        let max_d2 = max_dist * max_dist;
        let mut rows: Vec<(f32, usize, [f32; 3], &RtTreeSpecies)> = Vec::new();
        for sp in self.rt_species.iter() {
            let mut near_cnt = 0usize;
            let mut nearest_d2 = f32::INFINITY;
            let mut nearest_pos = [0.0f32; 3];
            for &p in &sp.positions {
                let d2 = (p[0] - eye.x).powi(2) + (p[1] - eye.y).powi(2) + (p[2] - eye.z).powi(2);
                if d2 <= max_d2 {
                    near_cnt += 1;
                    if d2 < nearest_d2 {
                        nearest_d2 = d2;
                        nearest_pos = p;
                    }
                }
            }
            if near_cnt > 0 {
                rows.push((nearest_d2.sqrt(), near_cnt, nearest_pos, sp));
            }
        }
        rows.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut out = String::new();
        let _ = writeln!(
            out,
            "[SpeedTree RT Diagnostics] species_total={} nearby_species={} max_dist={:.0}",
            self.rt_species.len(),
            rows.len(),
            max_dist
        );
        // Aggregate leaf alpha-mask coverage across all nearby species to help
        // diagnose "patches" in reflections/shadows: sparse masks leave holes.
        let mut total_leaf_tris = 0usize;
        let mut total_low_coverage = 0usize;
        let mut total_zero_coverage = 0usize;
        for (_, _, _, sp) in &rows {
            for ld in &sp.lod_diags {
                let leaf_tris = ld.fixed_3d_leaves + ld.billboard_leaves + ld.frond_tris;
                total_leaf_tris += leaf_tris;
            }
        }
        for (_, _, _, sp) in &rows {
            for (li, ld) in sp.lod_diags.iter().enumerate() {
                if li > 0 {
                    continue;
                }
                let leaf_tris = ld.fixed_3d_leaves + ld.billboard_leaves + ld.frond_tris;
                if leaf_tris > 0 {
                    // Count triangles with zero alpha coverage (fully transparent
                    // after mask sampling) — these produce invisible leaf patches.
                    let zero_cov = ld.leaves_skipped;
                    total_zero_coverage += zero_cov;
                    if (zero_cov as f64) > (leaf_tris as f64) * 0.3 {
                        total_low_coverage += 1;
                    }
                }
            }
        }
        let leaf_cov_pct = if total_leaf_tris > 0 {
            (total_zero_coverage as f64 / total_leaf_tris as f64) * 100.0
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "  [LEAF COVERAGE] total_leaf_tris={} zero_coverage_tris={} ({:.1}%) low_coverage_species={}",
            total_leaf_tris,
            total_zero_coverage,
            leaf_cov_pct,
            total_low_coverage,
        );
        for (nearest_d, near_cnt, np, sp) in rows {
            let d0 = sp.lod_diags.first().copied().unwrap_or_default();
            let warn = if d0.frond_tris + d0.fixed_3d_leaves + d0.billboard_leaves == 0 {
                " [WARN: NO_FOLIAGE_IN_LOD0]"
            } else if sp.composite_alpha_min == 255 && sp.composite_alpha_max == 255 {
                " [WARN: OPAQUE_COMPOSITE_NO_ALPHA]"
            } else if d0.leaves_skipped > (d0.fixed_3d_leaves + d0.billboard_leaves) {
                " [WARN: MANY_LEAVES_SKIPPED]"
            } else {
                ""
            };
            let has_skipped_leaves = d0.leaves_skipped > 0;
            let has_leaf_tris = (d0.fixed_3d_leaves + d0.billboard_leaves) > 0;
            let leaf_cov_warn = if has_skipped_leaves && has_leaf_tris {
                let denom = (d0.fixed_3d_leaves + d0.billboard_leaves + d0.frond_tris) as f64;
                let skip_pct = (d0.leaves_skipped as f64 / denom) * 100.0;
                if skip_pct > 30.0 {
                    format!(" [WARN: LEAF_PATCHES_SPARSE_MASK {:.0}%_SKIPPED]", skip_pct)
                } else {
                    "".to_string()
                }
            } else {
                "".to_string()
            };
            let _ = writeln!(
                out,
                "  species #{:03}: near_inst={} nearest={:.1}u at=({:.1},{:.1},{:.1}) h={:.1} comp={}x{} alpha=[{}..{}] lods={}{}{}",
                sp.species_id,
                near_cnt,
                nearest_d,
                np[0], np[1], np[2],
                sp.height,
                sp.composite_size.0,
                sp.composite_size.1,
                sp.composite_alpha_min,
                sp.composite_alpha_max,
                sp.lods.len(),
                warn,
                leaf_cov_warn,
            );
            for (li, ld) in sp.lod_diags.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "    LOD{}: branch={} frond={} frond_skip={} 3d_leaf={} bb_leaf={} leaf_skip={} masked={} unmasked_foliage={}",
                    li,
                    ld.branch_tris,
                    ld.frond_tris,
                    ld.frond_skipped,
                    ld.fixed_3d_leaves,
                    ld.billboard_leaves,
                    ld.leaves_skipped,
                    ld.masked_tris,
                    ld.unmasked_foliage_tris,
                );
            }
        }
        out
    }

    pub fn set_hiz_view(&mut self, device: &wgpu::Device, view: &wgpu::TextureView) {
        self.lod_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tree LOD with current Hi-Z texture"),
            layout: &self.lod_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.lod_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.placement_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.group_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.lod_args.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(view),
                },
            ],
        });
    }

    /// Compatibility entry point for diagnostics that run only the LOD/frustum
    /// pass. Production encodes this dispatch in the view's render command buffer.
    pub fn update_resident_lods(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: [f32; 3],
        scale: f32,
        bounds: Option<[f32; 4]>,
        frustum: [[f32; 4]; 6],
        clip_plane: Option<(f32, f32)>,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tree LOD diagnostic encoder"),
        });
        self.encode_resident_lods(
            device,
            queue,
            &mut encoder,
            camera,
            scale,
            bounds,
            frustum,
            clip_plane,
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            [[1, 1, 0, 0]; 16],
            None,
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Encode the frustum, optional Hi-Z occlusion, LOD compaction and indirect
    /// count generation into the caller's view command buffer. Hi-Z is forced
    /// for every enabled view because main/reflection/refraction depth differs.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_resident_lods(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: [f32; 3],
        scale: f32,
        bounds: Option<[f32; 4]>,
        frustum: [[f32; 4]; 6],
        clip_plane: Option<(f32, f32)>,
        view_projection: [[f32; 4]; 4],
        hiz_mips: [[u32; 4]; 16],
        hiz_extent: Option<(u32, u32, u32)>,
    ) {
        if self.placements == 0 {
            return;
        }
        let frustum = normalize_frustum(frustum);
        let key = (camera, scale, bounds, frustum, clip_plane);
        let hiz_enabled = hiz_extent.is_some();
        // The spatial broad phase depends on camera/frustum/residency, not on
        // the depth image. Hi-Z still needs a fresh compute dispatch each frame,
        // but rebuilding and re-uploading an identical candidate list does not.
        let same_resident = self
            .last_resident_update
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                same_resident_update(previous, camera, scale, bounds, frustum, clip_plane)
            });
        if same_resident && !hiz_enabled {
            return;
        }
        // A cheap spatial-cell test reduces the compute dispatch from the full
        // map's tree count to the subset whose conservative cell bounds overlap
        // this view. The shader still performs exact per-tree frustum, clip,
        // distance/LOD and optional Hi-Z tests.
        let active_count = if let Some(offset) = self.active_index_offset {
            if !same_resident {
                let mut active_indices = self.active_placement_indices.borrow_mut();
                active_indices.clear();
                append_visible_tree_indices(
                    &self.spatial_cells,
                    &frustum,
                    clip_plane,
                    bounds,
                    &mut active_indices,
                );
                if active_indices.is_empty() {
                    queue.write_buffer(&self.lod_args, 0, &self.lod_args_reset);
                    *self.last_resident_update.borrow_mut() = Some(key);
                    return;
                }
                queue.write_buffer(
                    &self.placement_buffer,
                    offset,
                    bytemuck::cast_slice(active_indices.as_slice()),
                );
            }
            let active_count = self.active_placement_indices.borrow().len();
            if active_count == 0 {
                queue.write_buffer(&self.lod_args, 0, &self.lod_args_reset);
                *self.last_resident_update.borrow_mut() = Some(key);
                return;
            }
            active_count
        } else {
            // The packed tail would exceed this adapter's binding limit. Keep
            // the correctness path by dispatching over every placement; the
            // shader's original frustum/clip/LOD/Hi-Z tests remain authoritative.
            self.placements as usize
        };
        let (hiz_width, hiz_height, hiz_levels) = hiz_extent.unwrap_or((0, 0, 0));
        let params = LodParams {
            camera: [camera[0], camera[1], camera[2], scale],
            bounds: bounds.unwrap_or([0.; 4]),
            frustum,
            clip: clip_plane.map_or([0.; 4], |(height, sign)| [height, sign, 1., 0.]),
            counts: [
                active_count as u32,
                self.active_index_offset.map_or(0, |offset| {
                    (offset / std::mem::size_of::<u32>() as u64) as u32
                }),
                u32::from(bounds.is_some()),
                0,
            ],
            view_projection,
            hiz_mips,
            hiz_info: [hiz_width, hiz_height, hiz_levels, u32::from(hiz_enabled)],
            // Positive depth-space slack prevents near-equal surfaces from
            // being classified as occluders due to floating-point rounding.
            hiz_bias: [0.00005, 0., 0., 0.],
        };
        queue.write_buffer(&self.lod_args, 0, &self.lod_args_reset);
        queue.write_buffer(&self.lod_params, 0, bytemuck::bytes_of(&params));
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(if hiz_enabled {
                "tree Hi-Z + LOD culling"
            } else {
                "tree LOD selection"
            }),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.lod_pipeline);
        pass.set_bind_group(0, &self.lod_bind, &[]);
        pass.dispatch_workgroups((active_count as u32).div_ceil(64), 1, 1);
        drop(pass);
        *self.last_resident_update.borrow_mut() = Some(key);
    }
    /// CPU LOD packing, kept for the checks that place one tree at a distance.
    ///
    /// Production uses `update_resident_lods`; this path writes the same
    /// instance slices by hand and publishes their counts as indirect args, so
    /// `draw` is one implementation for both.
    #[allow(dead_code)]
    pub fn update_lods(&mut self, queue: &wgpu::Queue, camera: [f32; 3], scale: f32) {
        // Only the checks reach this path, so the packer allocates its staging
        // copy per call instead of keeping one resident.
        let mut staging = vec![Instance::zeroed(); self.capacity];
        // Counts go into a copy: the compute pass restores `lod_args_reset` on
        // every dispatch, and a check that runs both paths must not have this
        // one leave its counts behind.
        let mut args = self.lod_args_reset.clone();
        for group in &self.groups {
            let mut counts = [0u32; 3];
            for &p in &group.positions {
                let Some(dist) = crate::tree_lod::client_distance(p, camera) else {
                    continue;
                };
                let selection = crate::tree_lod::select(dist, group.height, scale);
                let choices = [
                    (
                        selection.primary,
                        if selection.secondary.is_some() {
                            [selection.fraction, 1.]
                        } else {
                            [0., 0.]
                        },
                    ),
                    (
                        selection.secondary.unwrap_or(selection.primary),
                        [selection.fraction, 2.],
                    ),
                ];
                for (slot, (level, info)) in choices.into_iter().enumerate() {
                    if slot == 1 && selection.secondary.is_none() {
                        break;
                    }
                    let batch = &self.batches[group.first_batch + level];
                    let index = (batch.instances.start + counts[level]) as usize;
                    staging[index] = Instance::new(p, info);
                    counts[level] += 1;
                }
            }
            for (i, count) in counts.into_iter().enumerate() {
                // The indirect args carry the count now; each batch keeps its
                // full-capacity slice, so the instance offsets are the ones the
                // compute pass uses too.
                let at = (group.first_batch + i) * DRAW_ARGS as usize;
                for (offset, byte) in count.to_ne_bytes().iter().enumerate() {
                    args[at + 4 + offset] = *byte;
                }
            }
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&staging));
        queue.write_buffer(&self.lod_args, 0, &args);
    }
    pub fn residency_stats(&self) -> (usize, u64) {
        (self.capacity, self.instances.size())
    }
    /// The per-group height the LOD pass uses, in group order.
    #[allow(dead_code)]
    pub fn group_heights(&self) -> Vec<f32> {
        self.groups.iter().map(|group| group.height).collect()
    }
    /// Per-batch instance counts the LOD pass wrote, in indirect-args order.
    ///
    /// Reads the args buffer back, so a check can compare the GPU's selection
    /// against `tree_lod::select` directly instead of inferring it from pixels.
    #[allow(dead_code)]
    pub fn lod_instance_counts(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u32> {
        let bytes = self.read_back(device, queue, &self.lod_args, 0, self.lod_args.size());
        bytes
            .chunks_exact(DRAW_ARGS as usize)
            .map(|record| u32::from_ne_bytes([record[4], record[5], record[6], record[7]]))
            .collect()
    }
    /// The `(fraction, mode)` the LOD pass wrote for one instance slot, so a
    /// check can compare the cross-fade against `tree_lod::select` as well.
    #[allow(dead_code)]
    pub fn lod_instance_info(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        group: usize,
        level: usize,
        slot: usize,
    ) -> [f32; 2] {
        let size = std::mem::size_of::<Instance>() as u64;
        let start = self.batches[self.groups[group].first_batch + level]
            .instances
            .start as u64;
        let bytes = self.read_back(
            device,
            queue,
            &self.instances,
            (start + slot as u64) * size,
            size,
        );
        let data: &[f32; 8] = bytemuck::from_bytes(&bytes);
        [data[4], data[5]]
    }
    /// Copy `size` bytes out of `buffer` at `offset` and map them back.
    fn read_back(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        buffer: &wgpu::Buffer,
        offset: u64,
        size: u64,
    ) -> Vec<u8> {
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tree lod readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        e.copy_buffer_to_buffer(buffer, offset, &read, 0, size);
        queue.submit(Some(e.finish()));
        let slice = read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range().to_vec();
        read.unmap();
        bytes
    }
    pub fn draw<'a>(&'a self, p: &mut wgpu::RenderPass<'a>, reflected: bool) {
        if self.placements == 0 || self.batches.is_empty() {
            return;
        }
        p.set_pipeline(&self.pipeline);
        p.set_bind_group(
            0,
            if reflected {
                &self.reflected_bind
            } else {
                &self.bind
            },
            &[],
        );
        p.set_vertex_buffer(0, self.vertices.slice(..));
        p.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        // The counts come from the LOD pass, so the CPU never learns what is
        // visible: every batch is one record in a single indirect buffer, and an
        // empty level is a record with a zero instance count.
        if self.multi_draw && !self.batches.is_empty() {
            // The instance buffer is bound once; each record's `first_instance`
            // picks that batch's slice, so the whole forest is one call.
            p.set_vertex_buffer(1, self.instances.slice(..));
            p.multi_draw_indexed_indirect(&self.lod_args, 0, self.batches.len() as u32);
        } else {
            let stride = std::mem::size_of::<Instance>() as u64;
            for (index, batch) in self.batches.iter().enumerate() {
                p.set_vertex_buffer(
                    1,
                    self.instances
                        .slice(batch.instances.start as u64 * stride..),
                );
                p.draw_indexed_indirect(&self.lod_args, index as u64 * DRAW_ARGS);
            }
        }
    }
}

/// Normalize extracted frustum planes once per view so the compute shader
/// can compare sphere radii without six square roots per tree.
fn normalize_frustum(mut planes: [[f32; 4]; 6]) -> [[f32; 4]; 6] {
    for plane in &mut planes {
        let length = (plane[0] * plane[0] + plane[1] * plane[1] + plane[2] * plane[2]).sqrt();
        if length > 1e-8 && length.is_finite() {
            for value in plane {
                *value /= length;
            }
        }
    }
    planes
}

/// CPU reference for the conservative sphere/frustum test mirrored in WGSL.
fn sphere_visible(position: [f32; 3], radius: f32, planes: &[[f32; 4]; 6]) -> bool {
    planes
        .iter()
        .all(|p| p[0] * position[0] + p[1] * position[1] + p[2] * position[2] + p[3] >= -radius)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_tree_view_reuses_spatial_candidates_even_when_hiz_is_enabled() {
        let key = (
            [10.0, 20.0, 30.0],
            1.0,
            Some([-100.0, 100.0, -100.0, 100.0]),
            [[1.0, 0.0, 0.0, 1.0]; 6],
            Some((5.0, 1.0)),
        );
        assert!(super::same_resident_update(
            &key,
            [10.0, 20.0, 30.0],
            1.0,
            key.2,
            key.3,
            key.4,
        ));
        assert!(super::same_resident_update(
            &key,
            [10.001, 20.0, 30.0],
            1.0,
            key.2,
            key.3,
            key.4,
        ));
        assert!(!super::same_resident_update(
            &key,
            [10.0, 20.0, 30.0],
            1.0,
            key.2,
            [[0.0, 1.0, 0.0, 1.0]; 6],
            key.4,
        ));
        assert!(!super::same_resident_update(
            &key,
            [10.0, 20.0, 30.0],
            1.0,
            None,
            key.3,
            key.4,
        ));
    }
    use super::*;

    #[test]
    fn tree_spatial_cells_keep_intersections_and_reject_distant_regions() {
        let groups = [GroupInfo {
            height: 5.0,
            args_base: 0,
            first_instance: 0,
            capacity: 3,
        }];
        let placements = [
            Placement {
                pos_height: [95.0, 0.0, 0.0, 0.0],
                group: 0,
                pad: [0; 3],
            },
            Placement {
                pos_height: [120.0, 0.0, 0.0, 0.0],
                group: 0,
                pad: [0; 3],
            },
            Placement {
                pos_height: [1024.0, 0.0, 0.0, 0.0],
                group: 0,
                pad: [0; 3],
            },
        ];
        let cells = build_tree_spatial_cells(&placements, &groups);
        let cube = [
            [1.0, 0.0, 0.0, 100.0],
            [-1.0, 0.0, 0.0, 100.0],
            [0.0, 1.0, 0.0, 100.0],
            [0.0, -1.0, 0.0, 100.0],
            [0.0, 0.0, 1.0, 100.0],
            [0.0, 0.0, -1.0, 100.0],
        ];
        let mut visible = Vec::new();
        append_visible_tree_indices(&cells, &cube, None, None, &mut visible);
        // The first two trees share a cell. The second tree's sphere reaches
        // the view, so retaining that entire cell is intentionally conservative.
        assert_eq!(visible, vec![0, 1]);
        assert!(LOD_COMPUTE.contains("placement_words[p.counts.y+active_slot]"));
        assert!(!LOD_COMPUTE.contains("@group(0) @binding(6)"));
        assert!(LOD_COMPUTE.contains("load_placement(index)"));
    }

    #[test]
    fn tree_frustum_spheres_reject_outside_and_keep_intersecting_canopies() {
        let cube = [
            [1., 0., 0., 10.],
            [-1., 0., 0., 10.],
            [0., 1., 0., 10.],
            [0., -1., 0., 10.],
            [0., 0., 1., 10.],
            [0., 0., -1., 10.],
        ];
        assert!(sphere_visible([0., 0., 0.], 2., &cube));
        assert!(sphere_visible([12., 0., 0.], 3., &cube));
        assert!(!sphere_visible([14., 0., 0.], 2., &cube));
    }

    #[test]
    fn frustum_normalization_scales_plane_distance_too() {
        let mut planes = [[0.; 4]; 6];
        planes[0] = [3., 0., 0., 30.];
        let normalized = normalize_frustum(planes);
        assert_eq!(normalized[0], [1., 0., 0., 10.]);
    }

    #[test]
    fn legacy_cutout_still_covers_fronds_and_leaves_without_native_refs() {
        assert!(super::SHADER.contains("else if i.kind>3.5 && tex.a<0.5 {discard;}"));
    }

    /// Three groups of three levels, one placement per group: the layout the
    /// client's cache has and the one the shader walks. Every group's records are
    /// its own `first_batch + level`, and each level's instance slice starts
    /// exactly where `first_instance + level * capacity` points.
    #[test]
    fn the_group_table_points_every_level_at_its_own_record() {
        let mut batches = Vec::new();
        let mut groups = Vec::new();
        for group in 0..3usize {
            let first_batch = batches.len();
            for level in 0..3usize {
                let start = ((group * 3 + level) * 4) as u32;
                batches.push(Batch {
                    indices: (group * 3 + level) as u32 * 6..(group * 3 + level) as u32 * 6 + 6,
                    instances: start..start + 4,
                });
            }
            groups.push(Group {
                positions: vec![[0.; 3]; 4],
                height: 5. * (group as f32 + 1.),
                first_batch,
            });
        }
        let table = group_table(&groups, &batches);
        assert_eq!(table.len(), groups.len());
        for (index, info) in table.iter().enumerate() {
            let group = &groups[index];
            assert_eq!(info.args_base as usize, group.first_batch, "group {index}");
            assert_eq!(info.capacity, group.positions.len() as u32, "group {index}");
            for level in 0..3usize {
                let batch = &batches[info.args_base as usize + level];
                assert_eq!(
                    info.args_base as usize + level,
                    group.first_batch + level,
                    "group {index} level {level} must not borrow another group's record"
                );
                assert_eq!(
                    info.first_instance + level as u32 * info.capacity,
                    batch.instances.start,
                    "group {index} level {level} instance slice"
                );
            }
        }
        // The first group is where a scaled base would still have been right;
        // the second is what the old `first_batch * 3` broke.
        assert_eq!(table[0].args_base, 0);
        assert_eq!(table[1].args_base, 3);
        assert_eq!(table[2].args_base, 6);
        assert!(table[2].args_base as usize + 2 < batches.len());
    }

    /// A group with fewer than three levels must not claim capacity: the pass
    /// would otherwise count into the next group's records.
    #[test]
    fn a_group_without_three_levels_is_left_out_of_the_forest() {
        let batches = vec![
            Batch {
                indices: 0..6,
                instances: 0..2,
            },
            Batch {
                indices: 6..12,
                instances: 2..4,
            },
            Batch {
                indices: 12..18,
                instances: 4..6,
            },
            Batch {
                indices: 18..24,
                instances: 6..8,
            },
        ];
        let groups = vec![
            Group {
                positions: vec![[0.; 3]; 2],
                height: 5.,
                first_batch: 0,
            },
            Group {
                positions: vec![[0.; 3]; 2],
                height: 9.,
                first_batch: 3,
            },
        ];
        let table = group_table(&groups, &batches);
        assert_eq!(table[0].capacity, 2);
        assert_eq!(table[1].capacity, 0, "one level is not three");
    }
}

/// Standalone SpeedTree LOD harvester so unit tests can exercise LOD selection,
/// per-material alpha, and non-box canopy fallback without a GPU device.
pub fn harvest_rt_tree_species(
    species_list: &[RtTreeSpecies],
    eye: glam::Vec3,
    view_direction: glam::Vec3,
    max_dist: f32,
    lod_scale: f32,
    tri_budget: usize,
    out: &mut Vec<RtTreeTriangle>,
) -> (usize, usize, usize) {
    harvest_rt_tree_species_cancellable(
        species_list,
        eye,
        view_direction,
        max_dist,
        lod_scale,
        tri_budget,
        out,
        None,
    )
}

/// Cancellable form used by the asynchronous Raywatcher scene worker. The
/// ordinary helper remains convenient for diagnostics and unit tests.
pub fn harvest_rt_tree_species_cancellable(
    species_list: &[RtTreeSpecies],
    eye: glam::Vec3,
    view_direction: glam::Vec3,
    max_dist: f32,
    lod_scale: f32,
    tri_budget: usize,
    out: &mut Vec<RtTreeTriangle>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> (usize, usize, usize) {
    let start_len = out.len();
    let cancelled = |cancel: Option<&std::sync::atomic::AtomicBool>| {
        cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    };
    let max_d2 = max_dist * max_dist;
    let scale = lod_scale.clamp(0.25, 4.0);
    // Boost #20: Fast XZ bounding reject before 3D distance + squared LOD thresholds (zero sqrt).
    let mut candidates: Vec<(f32, usize, [f32; 3], usize)> = Vec::with_capacity(512);
    let mut desired_triangles = 0usize;
    for (si, sp) in species_list.iter().enumerate() {
        if sp.lods.is_empty() {
            continue;
        }
        for (position_index, &pos) in sp.positions.iter().enumerate() {
            if position_index & 0x3ff == 0 && cancelled(cancel) {
                out.truncate(start_len);
                return (0, 0, 0);
            }
            let dx = pos[0] - eye.x;
            if dx.abs() > max_dist {
                continue;
            }
            let dz = pos[2] - eye.z;
            if dz.abs() > max_dist {
                continue;
            }
            let dy = pos[1] - eye.y;
            let d2 = dx * dx + dy * dy + dz * dz;
            if d2 <= max_d2 {
                // Resolve the raster-compatible LOD while the placement is
                // already hot in the distance scan. If all desired LODs fit
                // the budget together, candidate order cannot affect output
                // and the O(n log n) nearest-first sort is unnecessary.
                let distance = approximate_client_distance(d2);
                let lod_count = sp.lods.len().min(3);
                let mut lod_idx = raster_lod_index(distance, sp.height, scale, lod_count);
                while lod_idx > 0 && sp.lods[lod_idx].len() < 12 && !sp.lods[lod_idx - 1].is_empty()
                {
                    lod_idx -= 1;
                }
                if let Some(preferred_lod) = select_harvest_lod(sp, lod_idx, lod_count, usize::MAX)
                {
                    desired_triangles =
                        desired_triangles.saturating_add(sp.lods[preferred_lod].len());
                }
                candidates.push((d2, si, pos, lod_idx));
            }
        }
    }
    // When over budget, nearest trees must retain priority because a farther
    // tree may be omitted or forced to a coarser LOD. Otherwise preserve scan
    // order and avoid sorting entirely.
    if desired_triangles > tri_budget {
        candidates.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    }

    // Each species/LOD has identical local geometry for every placement. Cache
    // its camera-facing leaf transform once, rather than repeating three vertex
    // transforms for every leaf triangle of every tree instance.
    let mut transformed_lods: Vec<[Option<Vec<RtTreeTriangle>>; 3]> = species_list
        .iter()
        .map(|_| std::array::from_fn(|_| None))
        .collect();
    let mut billboard_lods: Vec<[Option<bool>; 3]> = species_list
        .iter()
        .map(|_| std::array::from_fn(|_| None))
        .collect();

    let mut real_instances = 0usize;
    let mut real_tris = 0usize;
    let mut budget_skips = 0usize;
    for (candidate_index, (_, si, pos, lod_idx)) in candidates.into_iter().enumerate() {
        if candidate_index & 0x3f == 0 && cancelled(cancel) {
            out.truncate(start_len);
            return (0, 0, 0);
        }
        let sp = &species_list[si];
        let lod_count = sp.lods.len().min(3);
        let remaining = tri_budget.saturating_sub(out.len() - start_len);
        // Under pressure, try only progressively coarser *real* SpeedTree LODs.
        // If none fits, omit the instance rather than injecting synthetic card geometry.
        let Some(chosen_lod_idx) = select_harvest_lod(sp, lod_idx, lod_count, remaining) else {
            budget_skips += 1;
            continue;
        };
        let src_tris = &sp.lods[chosen_lod_idx];
        let offset = glam::Vec3::from_array(pos);
        let has_billboards = *billboard_lods[si][chosen_lod_idx]
            .get_or_insert_with(|| src_tris.iter().any(|triangle| triangle.leaf_rho >= 0.0));
        let local_triangles = if has_billboards {
            let cache = &mut transformed_lods[si][chosen_lod_idx];
            if cache.is_none() {
                *cache = transform_tree_lod_for_view(src_tris, view_direction, cancel);
            }
            let Some(transformed) = cache.as_deref() else {
                out.truncate(start_len);
                return (0, 0, 0);
            };
            transformed
        } else {
            src_tris
        };
        if !append_tree_lod_at(out, local_triangles, offset, cancel) {
            out.truncate(start_len);
            return (0, 0, 0);
        }
        real_instances += 1;
        real_tris += src_tris.len();
    }
    (real_instances, real_tris, budget_skips)
}

#[cfg(test)]
mod raywatcher_tree_tests {
    use super::*;

    #[test]
    fn rt_foliage_mips_follow_the_raster_atlas_chain_and_geometry_lod() {
        let mut base = image::RgbaImage::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let alpha = if (x + y) % 2 == 0 { 255 } else { 0 };
                base.put_pixel(x, y, image::Rgba([40, 120, 30, alpha]));
            }
        }
        let mips = rt_foliage_mip_images(&base, &[], 8, 8, false);
        assert_eq!(mips.len(), 4);
        assert_eq!(mips[0].dimensions(), (8, 8));
        assert_eq!(mips[1].dimensions(), (4, 4));
        assert_eq!(mips[2].dimensions(), (2, 2));
        assert_eq!(mips[3].dimensions(), (1, 1));
        let authored = vec![
            image::RgbaImage::new(4, 4),
            image::RgbaImage::new(2, 2),
            image::RgbaImage::new(1, 1),
            image::RgbaImage::new(1, 1),
            image::RgbaImage::new(1, 1),
            image::RgbaImage::new(1, 1),
            image::RgbaImage::new(1, 1),
        ];
        let authored_chain = rt_foliage_mip_images(&base, &authored, 8, 8, true);
        assert_eq!(authored_chain[1].dimensions(), (4, 4));
        assert_eq!(authored_chain[2].dimensions(), (2, 2));
    }

    #[test]
    fn rt_foliage_alpha_sampling_matches_linear_repeat_texture_filtering() {
        let mut image = image::RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 0]));
        image.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        // At the midpoint between texel centres, linear filtering returns
        // half coverage rather than snapping to one side of the leaf edge.
        let (_, alpha) = sample_rgba_uv(&image, [0.5, 0.5]);
        assert!((alpha - 0.5).abs() < 1e-6, "alpha={alpha}");
        // Repeat addressing must interpolate across the texture seam too.
        let (_, seam_alpha) = sample_rgba_uv(&image, [0.0, 0.5]);
        assert!((seam_alpha - 0.5).abs() < 1e-6, "alpha={seam_alpha}");
    }

    #[test]
    fn rt_leaf_and_frond_cutoffs_match_the_raster_shader_references() {
        assert!((leaf_alpha_cutoff(false) - 0.5).abs() < f32::EPSILON);
        assert!((leaf_alpha_cutoff(true) - 84.0 / 255.0).abs() < f32::EPSILON);
        assert!((raster_alpha_cutoff(false, Some(120.0)) - 0.5).abs() < f32::EPSILON);
        assert!((raster_alpha_cutoff(true, Some(120.0)) - 120.0 / 255.0).abs() < f32::EPSILON);
        assert!((raster_alpha_cutoff(true, None) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn tree_harvest_stops_promptly_when_the_background_scene_is_cancelled() {
        let species = RtTreeSpecies {
            species_id: 1,
            height: 10.0,
            composite_size: (1, 1),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[0.0, 0.0, 0.0]],
            lods: vec![Vec::new()],
            lod_diags: vec![RtTreeLodDiag::default()],
        };
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let mut out = Vec::new();
        let result = harvest_rt_tree_species_cancellable(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::Z,
            100.0,
            1.0,
            100,
            &mut out,
            Some(&cancel),
        );
        assert_eq!(result, (0, 0, 0));
        assert!(out.is_empty());
    }

    #[test]
    fn trees_with_lod_geometry_are_harvested_as_real_alpha_triangles_not_boxes() {
        let branch = RtTreeTriangle {
            a: glam::Vec3::new(0.0, 0.0, 0.0),
            b: glam::Vec3::new(0.4, 0.0, 0.0),
            c: glam::Vec3::new(0.2, 4.0, 0.0),
            normal: glam::Vec3::Z,
            albedo: [0.25, 0.16, 0.08],
            roughness: 0.9,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let leaf = RtTreeTriangle {
            a: glam::Vec3::new(-1.0, 4.0, 0.0),
            b: glam::Vec3::new(1.0, 4.0, 0.0),
            c: glam::Vec3::new(0.0, 6.0, 0.0),
            normal: glam::Vec3::Y,
            albedo: [0.18, 0.42, 0.14],
            roughness: 0.84,
            opacity: 0.65,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: 0.0,
        };
        let species = RtTreeSpecies {
            species_id: 0,
            height: 8.0,
            composite_size: (256, 256),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[10.0, 0.0, 10.0], [20.0, 0.0, 10.0]],
            lods: vec![vec![branch, leaf], vec![branch]],
            lod_diags: vec![RtTreeLodDiag::default(); 2],
        };
        let mut out = Vec::new();
        let (real_inst, real_tris, budget_skips) = harvest_rt_tree_species(
            &[species],
            glam::Vec3::new(0.0, 2.0, 0.0),
            glam::Vec3::X,
            200.0,
            1.0,
            10_000,
            &mut out,
        );
        assert_eq!(real_inst, 2);
        assert_eq!(real_tris, 4);
        assert_eq!(budget_skips, 0);
        assert_eq!(out.len(), 4);
        assert!(
            (out[2].a - out[0].a - glam::Vec3::X * 10.0).length() < 1e-5,
            "the cached billboard transform must be identical across placements"
        );
        assert!(
            out.iter().any(|t| t.opacity < 0.99),
            "leaves must carry per-material alpha"
        );
        assert!(
            out.iter().any(|t| (t.opacity - 1.0).abs() < 1e-4),
            "branches must stay opaque"
        );
    }

    #[test]
    fn budget_fallback_uses_the_actual_lod_for_billboard_transform_cache() {
        let branch = RtTreeTriangle {
            a: glam::Vec3::ZERO,
            b: glam::Vec3::X,
            c: glam::Vec3::Y,
            normal: glam::Vec3::Z,
            albedo: [0.2, 0.3, 0.1],
            roughness: 0.8,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let leaf = RtTreeTriangle {
            leaf_rho: 0.25,
            opacity: 0.6,
            ..branch
        };
        let species = RtTreeSpecies {
            species_id: 2,
            height: 8.0,
            composite_size: (64, 64),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[1.0, 0.0, 1.0]],
            // The outer LOD must be a real, non-sparse LOD (12+ triangles)
            // or the raster-compatible selector deliberately rejects it.
            lods: vec![vec![branch; 13], vec![leaf; 12]],
            lod_diags: vec![RtTreeLodDiag::default(); 2],
        };
        let mut out = Vec::new();
        let result = harvest_rt_tree_species(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::X,
            100.0,
            1.0,
            12,
            &mut out,
        );
        assert_eq!(result, (1, 12, 0));
        assert_eq!(out.len(), 12);
        assert_eq!(
            out[0].leaf_rho, -1.0,
            "fallback LOD billboard must be transformed"
        );
        assert_eq!(out[0].normal, glam::Vec3::Y);
    }

    #[test]
    fn tree_harvest_skips_nearest_sort_when_all_desired_lods_fit() {
        let tri = RtTreeTriangle {
            a: glam::Vec3::ZERO,
            b: glam::Vec3::X,
            c: glam::Vec3::Y,
            normal: glam::Vec3::Z,
            albedo: [0.2, 0.3, 0.1],
            roughness: 0.8,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let species = RtTreeSpecies {
            species_id: 3,
            height: 8.0,
            composite_size: (64, 64),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[20.0, 0.0, 0.0], [5.0, 0.0, 0.0]],
            lods: vec![vec![tri]],
            lod_diags: vec![RtTreeLodDiag::default()],
        };
        let mut out = Vec::new();
        let result = harvest_rt_tree_species(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::X,
            100.0,
            1.0,
            2,
            &mut out,
        );
        assert_eq!(result, (2, 2, 0));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].a.x, 20.0, "fitting harvests retain scan order");
        assert_eq!(out[1].a.x, 5.0);
    }

    #[test]
    fn tree_harvest_keeps_nearest_first_order_when_budget_is_exceeded() {
        let tri = RtTreeTriangle {
            a: glam::Vec3::ZERO,
            b: glam::Vec3::X,
            c: glam::Vec3::Y,
            normal: glam::Vec3::Z,
            albedo: [0.2, 0.3, 0.1],
            roughness: 0.8,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let species = RtTreeSpecies {
            species_id: 4,
            height: 8.0,
            composite_size: (64, 64),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[20.0, 0.0, 0.0], [5.0, 0.0, 0.0]],
            lods: vec![vec![tri]],
            lod_diags: vec![RtTreeLodDiag::default()],
        };
        let mut out = Vec::new();
        let result = harvest_rt_tree_species(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::X,
            100.0,
            1.0,
            1,
            &mut out,
        );
        assert_eq!(result, (1, 1, 1));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].a.x, 5.0, "nearest tree retains budget priority");
    }

    #[test]
    fn tree_harvest_counts_fallback_lods_before_skipping_nearest_sort() {
        let tri = RtTreeTriangle {
            a: glam::Vec3::ZERO,
            b: glam::Vec3::X,
            c: glam::Vec3::Y,
            normal: glam::Vec3::Z,
            albedo: [0.2, 0.3, 0.1],
            roughness: 0.8,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let species = RtTreeSpecies {
            species_id: 5,
            height: 8.0,
            composite_size: (64, 64),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[20.0, 0.0, 0.0], [5.0, 0.0, 0.0]],
            // The selected near LOD is empty, so the first usable fallback
            // must be included in the total used to decide whether sorting is safe.
            lods: vec![Vec::new(), vec![tri]],
            lod_diags: vec![RtTreeLodDiag::default(); 2],
        };
        let mut out = Vec::new();
        let result = harvest_rt_tree_species(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::X,
            100.0,
            1.0,
            1,
            &mut out,
        );
        assert_eq!(result, (1, 1, 1));
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].a.x, 5.0,
            "fallback geometry still prioritizes nearest tree"
        );
    }

    #[test]
    fn raster_lod_selection_uses_species_height_scale_and_transition_windows() {
        assert_eq!(raster_lod_index(49.0, 10.0, 1.0, 3), 0);
        assert_eq!(raster_lod_index(54.0, 10.0, 1.0, 3), 0);
        assert_eq!(raster_lod_index(56.0, 10.0, 1.0, 3), 1);
        assert_eq!(raster_lod_index(170.0, 10.0, 1.0, 3), 1);
        assert_eq!(raster_lod_index(180.0, 10.0, 1.0, 3), 2);
        assert_eq!(raster_lod_index(56.0, 10.0, 2.0, 3), 0);
    }

    #[test]
    fn exhausted_tree_budget_skips_a_tree_instead_of_creating_proxy_cards() {
        let tri = RtTreeTriangle {
            a: glam::Vec3::ZERO,
            b: glam::Vec3::X,
            c: glam::Vec3::Y,
            normal: glam::Vec3::Z,
            albedo: [0.2, 0.3, 0.1],
            roughness: 0.9,
            opacity: 1.0,
            alpha_mask: 0,
            leaf_center: glam::Vec3::ZERO,
            leaf_rho: -1.0,
        };
        let species = RtTreeSpecies {
            species_id: 1,
            height: 8.0,
            composite_size: (64, 64),
            composite_alpha_min: 0,
            composite_alpha_max: 255,
            positions: vec![[5.0, 0.0, 5.0]],
            lods: vec![vec![tri, tri], vec![tri, tri]],
            lod_diags: vec![RtTreeLodDiag::default(); 2],
        };
        let mut out = Vec::new();
        let (instances, triangles, skipped) = harvest_rt_tree_species(
            &[species],
            glam::Vec3::ZERO,
            glam::Vec3::X,
            100.0,
            1.0,
            1,
            &mut out,
        );
        assert_eq!((instances, triangles, skipped), (0, 0, 1));
        assert!(
            out.is_empty(),
            "no synthetic geometry may be emitted when a LOD cannot fit"
        );
    }

    #[test]
    fn cached_leaf_transform_matches_the_original_per_vertex_reference() {
        let pivot = glam::Vec3::new(1.25, -2.0, 0.75);
        let vertices = [
            pivot + glam::Vec3::new(-1.0, 4.0, 0.25),
            pivot + glam::Vec3::new(1.5, 4.5, -0.5),
            pivot + glam::Vec3::new(0.0, 6.0, 0.75),
        ];
        for view in [
            glam::Vec3::X,
            glam::Vec3::new(0.3, 0.8, -0.4),
            glam::Vec3::new(-0.2, -0.1, 0.9),
        ] {
            for rho in [0.0, 0.37, 2.1, 1000.0, 1001.0] {
                let source = [RtTreeTriangle {
                    a: vertices[0],
                    b: vertices[1],
                    c: vertices[2],
                    normal: glam::Vec3::Z,
                    albedo: [0.2, 0.4, 0.1],
                    roughness: 0.8,
                    opacity: 0.7,
                    alpha_mask: 0,
                    leaf_center: pivot,
                    leaf_rho: rho,
                }];
                let actual = transform_tree_lod_for_view(&source, view, None).unwrap()[0];
                let reference = |vertex: glam::Vec3| {
                    let relative = vertex - pivot;
                    pivot + speedtree_leaf_corner([relative.x, relative.z, relative.y], view, rho)
                };
                for (got, expected) in [
                    (actual.a, reference(source[0].a)),
                    (actual.b, reference(source[0].b)),
                    (actual.c, reference(source[0].c)),
                ] {
                    assert!(
                        (got - expected).length() < 2e-5,
                        "cached transform diverged for rho={rho}: {got:?} vs {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn billboard_leaf_vertices_match_the_raster_shader_transform() {
        let corner = [1.0, 2.0, 3.0];
        let billboard = speedtree_leaf_corner(corner, glam::Vec3::Z, 0.0);
        assert!((billboard - glam::Vec3::new(-2.0, 3.0, 1.0)).length() < 1e-5);
        assert_eq!(
            speedtree_leaf_corner(corner, glam::Vec3::Z, 1001.0),
            glam::Vec3::new(1.0, 3.0, 2.0)
        );
        let pivot = glam::Vec3::new(4.0, 5.0, 6.0);
        let local = pivot + glam::Vec3::new(corner[0], corner[2], corner[1]);
        let relative = local - pivot;
        let transformed =
            pivot + speedtree_leaf_corner([relative.x, relative.z, relative.y], glam::Vec3::Z, 0.0);
        assert!((transformed - (pivot + glam::Vec3::new(-2.0, 3.0, 1.0))).length() < 1e-5);
    }
}
