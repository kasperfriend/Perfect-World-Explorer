//! Conservative hierarchical depth (Hi-Z) pyramid for reversed-Z depth buffers.
//!
//! Level zero copies source depth. Every later level stores the minimum of its
//! 2x2 source footprint in an R32Float mipmapped texture. Clear depth is zero,
//! so holes propagate and remain visible to the conservative occlusion test.
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

const MAX_LEVELS: usize = 16;
const WORKGROUP: u32 = 8;
const REDUCE_WGSL: &str = r#"
struct Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    src_offset: u32,
    dst_offset: u32,
    mode: u32,
    _pad: u32,
};
@group(0) @binding(0) var source_depth: texture_depth_2d;
@group(0) @binding(1) var source_values: texture_2d<f32>;
@group(0) @binding(2) var destination: texture_storage_2d<r32float, write>;
@group(0) @binding(3) var<uniform> p: Params;

fn finite(v: f32) -> bool {
    return v >= -3.0e38 && v <= 3.0e38;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.dst_width || id.y >= p.dst_height {
        return;
    }
    var result = 1.0;
    if p.mode == 0u {
        result = textureLoad(source_depth, vec2<i32>(id.xy), 0);
        if !finite(result) {
            result = 0.0;
        }
    } else {
        // Ceil-halving keeps odd right/bottom edges in the hierarchy.
        var x_count = 2u;
        var y_count = 2u;
        if id.x == p.dst_width - 1u && (p.src_width & 1u) == 1u {
            x_count = 3u;
        }
        if id.y == p.dst_height - 1u && (p.src_height & 1u) == 1u {
            y_count = 3u;
        }
        for (var oy = 0u; oy < 3u; oy += 1u) {
            for (var ox = 0u; ox < 3u; ox += 1u) {
                let sx = id.x * 2u + ox;
                let sy = id.y * 2u + oy;
                if ox < x_count && oy < y_count && sx < p.src_width && sy < p.src_height {
                    let value = textureLoad(
                        source_values,
                        vec2<i32>(i32(sx), i32(sy)),
                        0
                    ).r;
                    // Invalid depth is treated exactly like a clear pixel.
                    if !finite(value) || value < 0.0 || value > 1.0 {
                        result = 0.0;
                    } else {
                        result = min(result, value);
                    }
                }
            }
        }
    }
    textureStore(
        destination,
        vec2<i32>(id.xy),
        vec4<f32>(result, 0.0, 0.0, 0.0)
    );
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DispatchParams {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    src_offset: u32,
    dst_offset: u32,
    mode: u32,
    pad: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelInfo {
    pub width: u32,
    pub height: u32,
    /// Offset in the CPU reference's packed representation; texture mips use
    /// their native mip indices and do not consume this offset.
    pub offset: u32,
}

pub struct Pyramid {
    extent: (u32, u32),
    levels: Vec<LevelInfo>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    level_views: Vec<wgpu::TextureView>,
    _dummy_texture: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    pipeline: wgpu::ComputePipeline,
    bind_layout: wgpu::BindGroupLayout,
    _params: Vec<wgpu::Buffer>,
}

impl Pyramid {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("Hi-Z extent must be non-zero".into());
        }
        let limits = device.limits();
        if width > limits.max_texture_dimension_2d || height > limits.max_texture_dimension_2d {
            return Err(format!(
                "Hi-Z extent {width}x{height} exceeds the {}px texture limit",
                limits.max_texture_dimension_2d
            ));
        }

        let mut levels = Vec::with_capacity(MAX_LEVELS);
        let (mut w, mut h) = (width, height);
        let mut next_offset = 0u64;
        loop {
            if levels.len() == MAX_LEVELS {
                return Err("Hi-Z requires more than 16 mip levels".into());
            }
            if next_offset > u32::MAX as u64 {
                return Err("Hi-Z element offset exceeds WGSL u32 range".into());
            }
            levels.push(LevelInfo {
                width: w,
                height: h,
                offset: next_offset as u32,
            });
            next_offset = next_offset
                .checked_add(u64::from(w) * u64::from(h))
                .ok_or("Hi-Z allocation size overflow")?;
            if w == 1 && h == 1 {
                break;
            }
            // Native texture mip extents floor-halve. The reduction kernel
            // folds any odd trailing row/column into the last destination texel.
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reversed-Z minimum depth pyramid"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Hi-Z full mip texture view"),
            format: Some(wgpu::TextureFormat::R32Float),
            dimension: Some(wgpu::TextureViewDimension::D2),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(levels.len() as u32),
            base_array_layer: 0,
            array_layer_count: Some(1),
        });
        let level_views: Vec<_> = (0..levels.len())
            .map(|index| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Hi-Z single mip view"),
                    format: Some(wgpu::TextureFormat::R32Float),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: index as u32,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(1),
                })
            })
            .collect();
        // Level zero's unused source binding must not alias its destination
        // storage view, even though mode=0 never loads it.
        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Hi-Z unused source mip fallback"),
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
        let dummy_view = dummy_texture.create_view(&Default::default());

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hi-Z reduction bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Hi-Z minimum reduction WGSL"),
            source: wgpu::ShaderSource::Wgsl(REDUCE_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Hi-Z reduction pipeline layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Hi-Z minimum reduction"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "main",
        });

        let mut params = Vec::with_capacity(levels.len());
        for index in 0..levels.len() {
            let dst = levels[index];
            let (src_width, src_height, src_offset, mode) = if index == 0 {
                (width, height, 0, 0)
            } else {
                let src = levels[index - 1];
                (src.width, src.height, src.offset, 1)
            };
            let data = DispatchParams {
                src_width,
                src_height,
                dst_width: dst.width,
                dst_height: dst.height,
                src_offset,
                dst_offset: dst.offset,
                mode,
                pad: 0,
            };
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Hi-Z level parameters"),
                contents: bytemuck::bytes_of(&data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            params.push(uniform);
        }
        Ok(Self {
            extent: (width, height),
            levels,
            texture,
            view,
            level_views,
            _dummy_texture: dummy_texture,
            dummy_view,
            pipeline,
            bind_layout,
            _params: params,
        })
    }

    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }
    pub fn levels(&self) -> &[LevelInfo] {
        &self.levels
    }
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
    pub fn memory_size(&self) -> u64 {
        self.levels
            .iter()
            .map(|level| u64::from(level.width) * u64::from(level.height) * 4)
            .sum()
    }

    /// Per-mip (width, height, 0, 0) table for WGSL.
    pub fn shader_mips(&self) -> [[u32; 4]; MAX_LEVELS] {
        let mut out = [[1, 1, 0, 0]; MAX_LEVELS];
        for (slot, level) in out.iter_mut().zip(&self.levels) {
            *slot = [level.width, level.height, 0, 0];
        }
        out
    }

    /// Encode one compute pass per mip into the caller's command buffer.
    /// Separate views prevent read/write aliasing between adjacent mip levels.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth_view: &wgpu::TextureView,
    ) {
        for (index, level) in self.levels.iter().enumerate() {
            let source_view = if index == 0 {
                &self.dummy_view
            } else {
                &self.level_views[index - 1]
            };
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Hi-Z reduction mip bindings"),
                layout: &self.bind_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(depth_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.level_views[index]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self._params[index].as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(if index == 0 {
                    "Hi-Z copy current depth"
                } else {
                    "Hi-Z reduce minimum depth"
                }),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(
                level.width.div_ceil(WORKGROUP),
                level.height.div_ceil(WORKGROUP),
                1,
            );
        }
    }
}

/// CPU reference for one ceil-halved minimum-reduction level.
pub fn reduce_min_level(source: &[f32], width: u32, height: u32) -> (Vec<f32>, u32, u32) {
    assert_eq!(source.len(), width as usize * height as usize);
    let dst_width = (width / 2).max(1);
    let dst_height = (height / 2).max(1);
    let mut destination = vec![1.0f32; dst_width as usize * dst_height as usize];
    for y in 0..dst_height {
        for x in 0..dst_width {
            let mut minimum = 1.0f32;
            let x_count = if x + 1 == dst_width && width % 2 == 1 {
                3
            } else {
                2
            };
            let y_count = if y + 1 == dst_height && height % 2 == 1 {
                3
            } else {
                2
            };
            for oy in 0..y_count {
                for ox in 0..x_count {
                    let sx = x * 2 + ox;
                    let sy = y * 2 + oy;
                    if sx < width && sy < height {
                        let value = source[(sy * width + sx) as usize];
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            minimum = 0.0;
                        } else {
                            minimum = minimum.min(value);
                        }
                    }
                }
            }
            destination[(y * dst_width + x) as usize] = minimum;
        }
    }
    (destination, dst_width, dst_height)
}

/// Project a conservative AABB. None means uncertain, so the candidate stays
/// visible (near-plane crossings, invalid clips and off-screen bounds included).
pub fn project_aabb(
    view_projection: Mat4,
    center: [f32; 3],
    radius: f32,
    extent: (u32, u32),
    near_plane: f32,
) -> Option<([u32; 4], f32)> {
    if extent.0 == 0
        || extent.1 == 0
        || !radius.is_finite()
        || radius < 0.0
        || !near_plane.is_finite()
        || near_plane < 0.0
        || !center.iter().all(|v| v.is_finite())
    {
        return None;
    }
    let center = Vec3::from_array(center);
    let mut min_pixel = [f32::INFINITY; 2];
    let mut max_pixel = [f32::NEG_INFINITY; 2];
    let mut nearest_depth = 0.0f32;
    for corner in 0..8 {
        let sign = |bit: u32| if corner & bit == 0 { -1.0 } else { 1.0 };
        let world = center + Vec3::new(sign(1), sign(2), sign(4)) * radius;
        let clip = view_projection * world.extend(1.0);
        if !clip.is_finite() || clip.w <= near_plane {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if !ndc.is_finite() || ndc.z < 0.0 || ndc.z >= 1.0 {
            return None;
        }
        let pixel = [
            (ndc.x * 0.5 + 0.5) * extent.0 as f32,
            (0.5 - ndc.y * 0.5) * extent.1 as f32,
        ];
        for axis in 0..2 {
            min_pixel[axis] = min_pixel[axis].min(pixel[axis]);
            max_pixel[axis] = max_pixel[axis].max(pixel[axis]);
        }
        nearest_depth = nearest_depth.max(ndc.z);
    }
    let min_x = (min_pixel[0].floor() as i64 - 2).max(0);
    let min_y = (min_pixel[1].floor() as i64 - 2).max(0);
    let max_x = (max_pixel[0].ceil() as i64 + 2).min(i64::from(extent.0) - 1);
    let max_y = (max_pixel[1].ceil() as i64 + 2).min(i64::from(extent.1) - 1);
    if min_x > max_x || min_y > max_y {
        return None;
    }
    Some((
        [min_x as u32, min_y as u32, max_x as u32, max_y as u32],
        nearest_depth,
    ))
}

/// All overlapped Hi-Z cells must be strictly in front of the candidate.
pub fn rectangle_occluded(
    levels: &[Vec<f32>],
    dimensions: &[(u32, u32)],
    rect: [u32; 4],
    nearest_depth: f32,
    bias: f32,
) -> bool {
    if levels.is_empty()
        || levels.len() != dimensions.len()
        || rect[0] > rect[2]
        || rect[1] > rect[3]
        || !nearest_depth.is_finite()
        || !(0.0..1.0).contains(&nearest_depth)
        || !bias.is_finite()
        || bias < 0.0
    {
        return false;
    }
    let mut selected = levels.len() - 1;
    for index in 0..dimensions.len() {
        let shift = index.min(31) as u32;
        let x0 = rect[0] >> shift;
        let y0 = rect[1] >> shift;
        let x1 = rect[2] >> shift;
        let y1 = rect[3] >> shift;
        if x1 - x0 < 2 && y1 - y0 < 2 {
            selected = index;
            break;
        }
    }
    let (width, height) = dimensions[selected];
    if width == 0 || height == 0 || levels[selected].len() != width as usize * height as usize {
        return false;
    }
    let shift = selected.min(31) as u32;
    let x0 = (rect[0] >> shift).min(width - 1);
    let y0 = (rect[1] >> shift).min(height - 1);
    let x1 = (rect[2] >> shift).min(width - 1);
    let y1 = (rect[3] >> shift).min(height - 1);
    let threshold = nearest_depth + bias;
    if !threshold.is_finite() || threshold >= 1.0 {
        return false;
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let depth = levels[selected][(y * width + x) as usize];
            if !depth.is_finite() || !(0.0..=1.0).contains(&depth) || depth <= threshold {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pyramid(
        mut level: Vec<f32>,
        mut width: u32,
        mut height: u32,
    ) -> (Vec<Vec<f32>>, Vec<(u32, u32)>) {
        let mut levels = vec![level.clone()];
        let mut dimensions = vec![(width, height)];
        while width > 1 || height > 1 {
            let (next, w, h) = reduce_min_level(&level, width, height);
            levels.push(next.clone());
            dimensions.push((w, h));
            level = next;
            width = w;
            height = h;
        }
        (levels, dimensions)
    }

    #[test]
    fn reduction_covers_odd_right_and_bottom_edges() {
        let source: Vec<f32> = (1..=15).map(|v| v as f32 / 16.0).collect();
        let (level, width, height) = reduce_min_level(&source, 5, 3);
        assert_eq!((width, height), (2, 1));
        assert_eq!(level, vec![source[0], source[2]]);
    }

    #[test]
    fn clear_depth_hole_propagates_through_every_reversed_z_mip() {
        let mut source = vec![0.8; 5 * 3];
        source[2 * 5 + 4] = 0.0;
        let (levels, dimensions) = pyramid(source, 5, 3);
        assert_eq!(dimensions, vec![(5, 3), (2, 1), (1, 1)]);
        assert_eq!(levels[1][1], 0.0);
        assert_eq!(levels[2], vec![0.0]);
    }

    #[test]
    fn occlusion_requires_every_overlapped_cell_to_be_in_front() {
        let (levels, dimensions) = pyramid(vec![0.8; 16], 4, 4);
        assert!(rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 3, 3],
            0.6,
            0.002
        ));
        let mut with_hole = vec![0.8; 16];
        with_hole[10] = 0.0;
        let (levels, dimensions) = pyramid(with_hole, 4, 4);
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 3, 3],
            0.6,
            0.002
        ));
    }

    #[test]
    fn depth_ties_and_uncertain_values_are_visible() {
        let (levels, dimensions) = pyramid(vec![0.6; 4], 2, 2);
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 1, 1],
            0.6,
            0.0
        ));
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 1, 1],
            0.6,
            0.01
        ));
        let mut invalid = vec![0.9; 4];
        invalid[1] = f32::NAN;
        let (levels, dimensions) = pyramid(invalid, 2, 2);
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 1, 1],
            0.6,
            0.001
        ));
    }

    #[test]
    fn opaque_building_occludes_covered_bounds_but_clear_holes_stay_visible() {
        // Reversed-Z: terrain is farther (0.35); a large opaque structure writes
        // a nearer surface (0.9). A tree behind its fully covered rectangle is
        // safely occluded, but one clear-depth hole invalidates that evidence.
        let mut depth = vec![0.35; 64 * 64];
        for y in 8..56 {
            for x in 8..56 {
                depth[(y * 64 + x) as usize] = 0.9;
            }
        }
        let (levels, dimensions) = pyramid(depth.clone(), 64, 64);
        assert!(rectangle_occluded(
            &levels,
            &dimensions,
            [12, 12, 20, 20],
            0.7,
            0.001
        ));

        depth[16 * 64 + 16] = 0.0;
        let (levels, dimensions) = pyramid(depth, 64, 64);
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [12, 12, 20, 20],
            0.7,
            0.001
        ));
    }

    #[test]
    fn mip_selection_bounds_sample_work_without_dropping_edge_cells() {
        let (levels, dimensions) = pyramid(vec![0.9; 1024 * 512], 1024, 512);
        assert!(rectangle_occluded(
            &levels,
            &dimensions,
            [129, 33, 831, 477],
            0.5,
            0.001
        ));
        let (levels, dimensions) = pyramid(vec![0.9; 13 * 7], 13, 7);
        assert!(rectangle_occluded(
            &levels,
            &dimensions,
            [11, 5, 12, 6],
            0.5,
            0.001
        ));
    }

    #[test]
    fn coarser_mip_sampling_keeps_clear_depth_holes_conservative() {
        let mut depth = vec![0.9; 8 * 8];
        depth[3 * 8 + 3] = 0.0;
        let (levels, dimensions) = pyramid(depth, 8, 8);
        assert!(!rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 7, 7],
            0.5,
            0.001
        ));

        let (levels, dimensions) = pyramid(vec![0.9; 8 * 8], 8, 8);
        assert!(rectangle_occluded(
            &levels,
            &dimensions,
            [0, 0, 7, 7],
            0.5,
            0.001
        ));
    }

    #[test]
    fn projection_keeps_near_plane_intersections_visible() {
        let projection = Mat4::perspective_rh(70f32.to_radians(), 1.0, 1000.0, 1.0);
        let vp = projection * Mat4::look_to_rh(Vec3::ZERO, -Vec3::Z, Vec3::Y);
        assert!(project_aabb(vp, [0.0, 0.0, -10.0], 1.0, (320, 240), 1.0).is_some());
        assert!(project_aabb(vp, [0.0, 0.0, -1.5], 1.0, (320, 240), 1.0).is_none());
    }

    #[test]
    fn degenerate_or_offscreen_projection_is_not_occlusion_evidence() {
        let projection = Mat4::perspective_rh(70f32.to_radians(), 1.0, 1000.0, 1.0);
        let vp = projection * Mat4::look_to_rh(Vec3::ZERO, -Vec3::Z, Vec3::Y);
        assert!(project_aabb(vp, [0.0, 0.0, -10.0], 0.0, (0, 240), 1.0).is_none());
        assert!(project_aabb(vp, [10000.0, 0.0, -10.0], 1.0, (320, 240), 1.0).is_none());
    }

    #[test]
    fn odd_dimensions_build_a_finite_hierarchy() {
        let source: Vec<f32> = (0..35).map(|v| v as f32 / 35.0).collect();
        let (levels, dimensions) = pyramid(source, 7, 5);
        assert_eq!(dimensions, vec![(7, 5), (3, 2), (1, 1)]);
        assert_eq!(levels.last().unwrap(), &[0.0]);
    }
}
