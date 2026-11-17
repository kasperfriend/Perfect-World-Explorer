//! GPU-instanced, runtime-derived far-tree LOD covering all scene placements.
use crate::{gpu::Vertex, trees::Trees};
#[path = "terrain_cutoff.rs"]
mod cutoff_residency;
use crate::indirect_args::{base_instance, multi_draw, record, DrawIndexedArgs, DRAW_ARGS};
use bytemuck::{Pod, Zeroable};
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
/// Per-frame inputs of the LOD compute pass.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct LodParams {
    /// Eye position; `.w` is the LOD scale the frame uses.
    pub(crate) camera: [f32; 4],
    /// Admitted XZ square (`x0, x1, z0, z1`); ignored when `counts.z` is 0.
    pub(crate) bounds: [f32; 4],
    /// `placements, groups, bounds_active, 0`.
    pub(crate) counts: [u32; 4],
}

/// The LOD selection pass.
///
/// This is `tree_lod::client_distance` + `tree_lod::select` transcribed into
/// WGSL and run once per placement on the GPU, so a camera move no longer costs
/// a walk over every tree placement in the world plus a full instance-buffer
/// upload. Both functions are pure arithmetic and are reproduced operation for
/// operation: the same fast square root on the squared distance
/// (`(bits >> 1) + 0x1fc00000`), the same 10 % transition windows around
/// `height * 5 * scale` and `height * 16 * scale`. The only difference is that
/// the CPU sums the squared deltas in f64 and rounds once; the GPU sums in f32,
/// which can move a distance by ~1e-7 relative — inside a transition window that
/// changes the fade fraction, never whether the tree is drawn.
pub(crate) const LOD_COMPUTE: &str = r#"
struct LodParamsGpu {camera:vec4<f32>,bounds:vec4<f32>,counts:vec4<u32>};
struct Placement {pos_height:vec4<f32>,group:u32,pad0:u32,pad1:u32,pad2:u32};
struct GroupInfo {height:f32,args_base:u32,first_instance:u32,capacity:u32};
struct Args {index_count:u32,instance_count:atomic<u32>,first_index:u32,base_vertex:i32,first_instance:u32};
struct Instance {data0:vec4<f32>,data1:vec4<f32>};
@group(0) @binding(0) var<uniform> p:LodParamsGpu;
@group(0) @binding(1) var<storage,read> placements:array<Placement>;
@group(0) @binding(2) var<storage,read> groups:array<GroupInfo>;
@group(0) @binding(3) var<storage,read_write> instances:array<Instance>;
@group(0) @binding(4) var<storage,read_write> args:array<Args>;
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
 let index=id.x;
 if index>=p.counts.x {return;}
 let placement=placements[index];
 let position=placement.pos_height.xyz;
 if !is_finite(position.x) || !is_finite(position.y) || !is_finite(position.z) {return;}
 if p.counts.z!=0u {
  if position.x<p.bounds.x || position.x>=p.bounds.y {return;}
  if position.z<p.bounds.z || position.z>=p.bounds.w {return;}
 }
 let constants=groups[placement.group];
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

pub struct FarTrees {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    reflected_bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    instances: wgpu::Buffer,
    batches: Vec<Batch>,
    groups: Vec<Group>,
    /// Slots in the instance buffer: three level slices per placement.
    capacity: usize,
    last_resident_update: Option<([f32; 3], f32, Option<[f32; 4]>)>,
    /// The LOD selection pass: a camera move dispatches this instead of walking
    /// every placement on the CPU and re-uploading the instance buffer.
    lod_pipeline: wgpu::ComputePipeline,
    lod_bind: wgpu::BindGroup,
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
fn fogged(linear:vec3<f32>, reciprocal_w:f32)->vec3<f32> {
 if camera.fog_range.z<0.5 {return linear;}
 let view_z=1./max(reciprocal_w,0.0000001);
 let span=camera.fog_range.y-camera.fog_range.x;
 let f=select(clamp((view_z-camera.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,view_z>=camera.fog_range.y),abs(span)<0.001);
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(encoded,camera.fog_color.rgb,f);
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
struct In {@builtin(vertex_index) vertex_id:u32,@location(0) pos:vec3<f32>,@location(1) normal:vec3<f32>,@location(2) uv:vec2<f32>,@location(3) color:vec4<f32>,@location(4) kind:f32,@location(5) instance:vec3<f32>,@location(6) info:vec2<f32>,@location(7) tex_h:u32,@location(8) alpha_ref:u32};
struct Out {@builtin(position) pos:vec4<f32>,@location(0) normal:vec3<f32>,@location(1) uv:vec2<f32>,@location(2) color:vec4<f32>,@location(3) @interpolate(flat) kind:f32,@location(4) world:vec3<f32>,@location(5) @interpolate(flat) tree:vec3<f32>,@location(6) @interpolate(flat) tex_h:f32,@location(7) @interpolate(flat) fade:vec2<f32>,@location(8) @interpolate(flat) alpha_ref:u32};
@vertex fn vs(i:In)->Out {var o:Out;var pos=i.pos+i.instance;var norm=i.normal;
 if i.kind>4.5 {let d=normalize(camera.direction.xyz);let az=atan2(d.z,d.x);let el=asin(clamp(d.y,-1.,1.));let rho=i.color.w;var corner=i.normal;let rock_index=i.tex_h>>8u;if rock_index>0u {corner=mix(corner,rock_sample(rock_index-1u,camera.tree_bounds.w),clamp(camera.tree_bounds.z,0.,1.));}let q=vec3<f32>(cos(el)*corner.x-sin(el)*corner.z,corner.y,sin(el)*corner.x+cos(el)*corner.z);let a=az+rho;var v=vec3<f32>(cos(a)*q.x-sin(a)*q.y,sin(a)*q.x+cos(a)*q.y,q.z);if rho>1000. {v=corner;}pos+=vec3<f32>(v.x,v.z,v.y);norm=vec3<f32>(0.,1.,0.);if i.vertex_id<arrayLength(&leaf_normals) {let n=leaf_normals[i.vertex_id];if n.w>0.5 {norm=n.xyz;}}}
 o.pos=camera.matrix*vec4<f32>(pos,1.);o.world=pos;o.normal=norm;o.uv=i.uv;o.color=i.color;o.kind=i.kind;o.tree=i.instance;o.tex_h=f32(i.tex_h & 127u)*select(128./8192.,128./2048.,(i.tex_h & 128u)!=0u);o.fade=i.info;o.alpha_ref=i.alpha_ref;return o;}
@fragment fn fs(i:Out)->@location(0) vec4<f32>{if camera.clip.y!=0. && (i.world.y-camera.clip.x)*camera.clip.y<0. {discard;}

 let scale=vec2<f32>(i.color.z,i.tex_h);var dx=dpdx(i.uv);var dy=dpdy(i.uv);let lod_bias=exp2(camera.lod.x);dx*=lod_bias;dy*=lod_bias;let dims=vec2<f32>(textureDimensions(atlas));let mip=clamp(log2(max(max(length(dx*dims),length(dy*dims)),1.)),0.,7.);let halfpixel=min(vec2<f32>(0.5*exp2(mip))/dims,scale*0.499);let uv=i.color.xy+clamp(fract((i.uv-i.color.xy)/scale)*scale,halfpixel,scale-halfpixel);let tex=textureSampleGrad(atlas,atlas_sampler,uv,dx,dy);if i.alpha_ref!=0xffffffffu {let threshold=bitcast<f32>(i.alpha_ref);if tex.a<=threshold {discard;}}else if i.kind>3.5 && tex.a<0.5 {discard;}
 // Complementary ordered coverage: old and new snapshots never both draw
 // at the same pixel, while the tree is fully visible at the endpoints.
 if i.fade.y>0.5 {let xy=vec2<u32>(i.pos.xy)%vec2<u32>(4u);let b=(((xy.x&1u)^(xy.y&1u))*8u)+((xy.y&1u)*4u)+((((xy.x>>1u)&1u)^((xy.y>>1u)&1u))*2u)+((xy.y>>1u)&1u);let d=(f32(b)+0.5)/16.;if (i.fade.y<1.5 && d<i.fade.x)||(i.fade.y>1.5 && d>=i.fade.x){discard;}}
 if camera.region_ambient.w>0.5 {
  let lambert=max(dot(normalize(i.normal),normalize(camera.region_direction.xyz)),0.);
  // These are regional light coefficients, not the original SpeedTree
  // material constants. The branch-only PS 1.4 shader's x2 operation is
  // not justified for every leaf/frond; applying it clipped bright foliage.
  // Keep this fallback bounded and mostly neutral until material constants,
  // normals and the separate client passes are recovered.
  let rgb=clamp(camera.region_ambient.rgb+camera.region_diffuse.rgb*lambert,vec3<f32>(0.),vec3<f32>(1.));
  let luminance=dot(rgb,vec3<f32>(0.2126,0.7152,0.0722));
  let light=mix(vec3<f32>(luminance),rgb,0.25);
  return vec4<f32>(fogged(tex.rgb*light,i.pos.w),1.);
 }
 let light=camera.light.x+camera.light.y*max(dot(normalize(i.normal),normalize(camera.sun.xyz)),0.);return vec4<f32>(fogged(tex.rgb*light*camera.light.zww,i.pos.w),1.);}
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
        let mut atlas = vec![0u8; side * side * 4];
        let mut vertices = Vec::new();
        let mut rocking_samples: Vec<[f32; 4]> = Vec::new();
        let mut leaf_normals: Vec<[f32; 4]> = Vec::new();
        let mut indices = Vec::new();
        let mut instances = Vec::new();
        let mut batches = Vec::new();
        let mut groups = Vec::new();
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
            let positions: Vec<_> = trees
                .placements
                .iter()
                .filter(|p| p.tree_type as usize == id)
                .map(|p| p.position)
                .collect();
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
            let mut slots = Vec::new();
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
            for lod in &tree.lods {
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
                batches.push(Batch {
                    indices: start_idx..indices.len() as u32,
                    instances: start_inst..end_inst,
                });
            }
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
        let image = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("far tree textures"),
            size: wgpu::Extent3d {
                width: side as u32,
                height: side as u32,
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
                bytes_per_row: Some((side * 4) as u32),
                rows_per_image: Some(side as u32),
            },
            wgpu::Extent3d {
                width: side as u32,
                height: side as u32,
                depth_or_array_layers: 1,
            },
        );
        // Generate each rectangle separately so lower mip levels cannot blend
        // neighboring trees. Keep unadjusted alpha for the next reduction;
        // preserve approximate cutout coverage only in the uploaded copy.
        for (ox, oy, w, h, cutout, authored) in mip_slots {
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
            let mut raw = vec![0u8; w * h * 4];
            for y in 0..h {
                let src = ((oy + y) * side + ox) * 4;
                raw[y * w * 4..(y + 1) * w * 4].copy_from_slice(&atlas[src..src + w * 4]);
            }
            let source_coverage = crate::foliage_mips::coverage(&raw);
            let (mut mw, mut mh) = (w, h);
            for level in 1..8 {
                raw = crate::foliage_mips::downsample(&raw, mw, mh, cutout);
                mw /= 2;
                mh /= 2;
                let corrected = if cutout {
                    crate::foliage_mips::preserve_coverage(&raw, source_coverage)
                } else {
                    raw.clone()
                };
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
                    &corrected,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some((mw * 4) as u32),
                        rows_per_image: Some(mh as u32),
                    },
                    wgpu::Extent3d {
                        width: mw as u32,
                        height: mh as u32,
                        depth_or_array_layers: 1,
                    },
                );
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
        let mut placement_data = Vec::new();
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
        let placement_count = placement_data.len() as u32;
        let placement_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tree placements"),
            contents: bytemuck::cast_slice(&placement_data),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
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
            capacity,
            last_resident_update: None,
            lod_pipeline,
            lod_bind,
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
    pub fn update_resident_lods(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: [f32; 3],
        scale: f32,
        bounds: Option<[f32; 4]>,
    ) {
        if self.placements == 0 {
            return;
        }
        let key = (camera, scale, bounds);
        if self.last_resident_update == Some(key) {
            return;
        }
        let params = LodParams {
            camera: [camera[0], camera[1], camera[2], scale],
            bounds: bounds.unwrap_or([0.; 4]),
            counts: [
                self.placements,
                self.groups.len() as u32,
                u32::from(bounds.is_some()),
                0,
            ],
        };
        // Zero the per-batch instance counts, keep the static draw spans.
        queue.write_buffer(&self.lod_args, 0, &self.lod_args_reset);
        queue.write_buffer(&self.lod_params, 0, bytemuck::bytes_of(&params));
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut pass = e.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("tree lod selection"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.lod_pipeline);
            pass.set_bind_group(0, &self.lod_bind, &[]);
            pass.dispatch_workgroups(self.placements.div_ceil(64), 1, 1);
        }
        queue.submit(Some(e.finish()));
        self.last_resident_update = Some(key);
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
        if self.batches.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
