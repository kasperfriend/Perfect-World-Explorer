//! Conservative hierarchical depth (Hi-Z) pyramid for reversed-Z depth buffers.
//!
//! Level zero copies source depth. Every later level stores the minimum of its
//! 2x2 source footprint. Clear depth is zero, so holes propagate and remain
//! visible to the conservative occlusion test.
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

const MAX_LEVELS: usize = 16;
const WORKGROUP: u32 = 8;
const REDUCE_WGSL: &str = r#"
struct Params {
    src_width: u32, src_height: u32, dst_width: u32, dst_height: u32,
    src_offset: u32, dst_offset: u32, mode: u32, _pad: u32,
};
@group(0) @binding(0) var source_depth: texture_depth_2d;
@group(1) @binding(0) var<storage, read_write> values: array<f32>;
@group(2) @binding(0) var<uniform> p: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.dst_width || id.y >= p.dst_height { return; }
    var result = 1.0;
    if p.mode == 0u {
        result = textureLoad(source_depth, vec2<i32>(id.xy), 0);
        if !isFinite(result) { result = 0.0; }
    } else {
        // Ceil-halving keeps odd right/bottom edges in the hierarchy.
        for (var oy = 0u; oy < 2u; oy += 1u) {
            for (var ox = 0u; ox < 2u; ox += 1u) {
                let sx = id.x * 2u + ox;
                let sy = id.y * 2u + oy;
                if sx < p.src_width && sy < p.src_height {
                    let value = values[p.src_offset + sy * p.src_width + sx];
                    // Invalid depth is treated exactly like a clear pixel.
                    if !isFinite(value) || value < 0.0 || value > 1.0 {
                        result = 0.0;
                    } else {
                        result = min(result, value);
                    }
                }
            }
        }
    }
    values[p.dst_offset + id.y * p.dst_width + id.x] = result;
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
    /// Offset in 32-bit float elements in the persistent storage buffer.
    pub offset: u32,
}

pub struct Pyramid {
    extent: (u32, u32),
    levels: Vec<LevelInfo>,
    buffer: wgpu::Buffer,
    pipeline: wgpu::ComputePipeline,
    depth_layout: wgpu::BindGroupLayout,
    values_bind: wgpu::BindGroup,
    params_bind: Vec<wgpu::BindGroup>,
    _params: Vec<wgpu::Buffer>,
}

impl Pyramid {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("Hi-Z extent must be non-zero".into());
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
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        let byte_size = next_offset
            .checked_mul(4)
            .ok_or("Hi-Z byte size overflow")?;
        let limits = device.limits();
        if byte_size > u64::from(limits.max_storage_buffer_binding_size)
            || byte_size > limits.max_buffer_size
        {
            return Err(format!(
                "Hi-Z buffer requires {byte_size} bytes; device limits are {} bytes per storage binding and {} bytes per buffer",
                limits.max_storage_buffer_binding_size, limits.max_buffer_size
            ));
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("reversed-Z minimum depth pyramid"),
            size: byte_size.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hi-Z source depth"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let values_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hi-Z pyramid storage"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Hi-Z reduction parameters"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Hi-Z minimum reduction WGSL"),
            source: wgpu::ShaderSource::Wgsl(REDUCE_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Hi-Z reduction pipeline layout"),
            bind_group_layouts: &[&depth_layout, &values_layout, &params_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Hi-Z minimum reduction"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "main",
        });
        let values_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Hi-Z pyramid storage binding"),
            layout: &values_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        let mut params = Vec::with_capacity(levels.len());
        let mut params_bind = Vec::with_capacity(levels.len());
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
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Hi-Z level parameters binding"),
                layout: &params_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            });
            params.push(uniform);
            params_bind.push(bind);
        }
        Ok(Self {
            extent: (width, height),
            levels,
            buffer,
            pipeline,
            depth_layout,
            values_bind,
            params_bind,
            _params: params,
        })
    }

    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }
    pub fn levels(&self) -> &[LevelInfo] {
        &self.levels
    }
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    pub fn buffer_size(&self) -> u64 {
        self.buffer.size()
    }

    /// Per-mip (width, height, float-element-offset, 0) table for WGSL.
    pub fn shader_mips(&self) -> [[u32; 4]; MAX_LEVELS] {
        let mut out = [[1, 1, 0, 0]; MAX_LEVELS];
        for (slot, level) in out.iter_mut().zip(&self.levels) {
            *slot = [level.width, level.height, level.offset, 0];
        }
        out
    }

    /// Encode one compute pass per level into the caller's command buffer.
    /// Pass boundaries make each level's storage writes visible to the next.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth_view: &wgpu::TextureView,
    ) {
        let depth_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Hi-Z current-view depth binding"),
            layout: &self.depth_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth_view),
            }],
        });
        for (index, level) in self.levels.iter().enumerate() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(if index == 0 {
                    "Hi-Z copy current depth"
                } else {
                    "Hi-Z reduce minimum depth"
                }),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &depth_bind, &[]);
            pass.set_bind_group(1, &self.values_bind, &[]);
            pass.set_bind_group(2, &self.params_bind[index], &[]);
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
    let dst_width = width.div_ceil(2);
    let dst_height = height.div_ceil(2);
    let mut destination = vec![1.0f32; dst_width as usize * dst_height as usize];
    for y in 0..dst_height {
        for x in 0..dst_width {
            let mut minimum = 1.0f32;
            for oy in 0..2 {
                for ox in 0..2 {
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
        if x1 - x0 < 4 && y1 - y0 < 4 {
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
        assert_eq!((width, height), (3, 2));
        assert_eq!(
            level,
            vec![source[0], source[2], source[4], source[10], source[12], source[14]]
        );
    }

    #[test]
    fn clear_depth_hole_propagates_through_every_reversed_z_mip() {
        let mut source = vec![0.8; 5 * 3];
        source[2 * 5 + 4] = 0.0;
        let (levels, dimensions) = pyramid(source, 5, 3);
        assert_eq!(dimensions, vec![(5, 3), (3, 2), (2, 1), (1, 1)]);
        assert_eq!(levels[1][5], 0.0);
        assert_eq!(levels[3], vec![0.0]);
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
        assert_eq!(dimensions, vec![(7, 5), (4, 3), (2, 2), (1, 1)]);
        assert_eq!(levels.last().unwrap(), &[0.0]);
    }
}
