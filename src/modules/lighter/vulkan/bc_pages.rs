use super::*;
use crate::modules::lighter::textures::bc_pages::{Kind, Pages, SIDE};

fn format(kind: Kind) -> vk::Format {
    match kind {
        Kind::Terrain => vk::Format::BC1_RGBA_SRGB_BLOCK,
        Kind::Normal => vk::Format::BC5_UNORM_BLOCK,
        Kind::Color => vk::Format::BC7_SRGB_BLOCK,
    }
}
const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw() | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);
pub(super) const ACTIVE: u32 = 1 << 17;

pub(super) fn supported(device: &wgpu::Device, kind: Kind, layers: u32) -> bool {
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return false;
    }
    unsafe {
        device
            .as_hal::<Vulkan, _, _>(|hal| {
                let Some(hal) = hal else {
                    return false;
                };
                let instance = hal.shared_instance().raw_instance();
                let physical = hal.raw_physical_device();
                instance
                    .get_physical_device_image_format_properties(
                        physical,
                        format(kind),
                        vk::ImageType::TYPE_2D,
                        vk::ImageTiling::OPTIMAL,
                        USAGE,
                        vk::ImageCreateFlags::empty(),
                    )
                    .is_ok_and(|props| {
                        props.max_array_layers >= layers
                            && props.max_extent.width >= SIDE
                            && props.max_extent.height >= SIDE
                    })
            })
            .unwrap_or(false)
    }
}

pub(super) struct Atlas {
    pub kind: Kind,
    pages: Arc<Pages>,
    image: image::Image,
    range: vk::ImageSubresourceRange,
    copies: Vec<vk::BufferImageCopy>,
    pub visible: bool,
    pub initialized: bool,
    pub uploaded: bool,
}
impl Atlas {
    pub fn new(ctx: &Arc<Context>, kind: Kind, pages: &Arc<Pages>) -> R<Self> {
        let layers = pages.layers();
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            level_count: 1,
            layer_count: layers,
            ..Default::default()
        };
        let image = image::Image::new(
            ctx,
            vk::Extent2D {
                width: SIDE,
                height: SIDE,
            },
            format(kind),
            USAGE,
            range,
            vk::ImageViewType::TYPE_2D_ARRAY,
        )?;
        let copies = pages
            .copies
            .iter()
            .map(|copy| {
                let [x, y, layer] = copy.origin;
                let [width, height] = copy.extent;
                vk::BufferImageCopy {
                    buffer_offset: copy.offset as u64,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: range.aspect_mask,
                        base_array_layer: layer,
                        layer_count: 1,
                        ..Default::default()
                    },
                    image_offset: vk::Offset3D {
                        x: x as i32,
                        y: y as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    },
                    ..Default::default()
                }
            })
            .collect();
        Ok(Self {
            kind,
            pages: pages.clone(),
            image,
            range,
            copies,
            visible: false,
            initialized: false,
            uploaded: false,
        })
    }
    pub fn matches(&self, texels: &Textures) -> bool {
        texels.bc_pages[self.kind.index()]
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(p, &self.pages))
    }
    pub fn stats(&self) -> [u64; 3] {
        [
            self.pages.tiles as u64,
            self.pages.layers() as u64,
            self.image.charged,
        ]
    }
    pub fn bind(&self, core: &Core, binding: u32) {
        let info = vk::DescriptorImageInfo::builder()
            .image_view(self.image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .build();
        unsafe {
            core.ctx.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::builder()
                    .dst_set(core.set)
                    .dst_binding(binding)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(std::slice::from_ref(&info))
                    .build()],
                &[],
            );
        }
    }
    pub unsafe fn encode(&self, cmd: vk::CommandBuffer, buffer: Option<vk::Buffer>) {
        let d = &self.image.ctx.device;
        let barrier = || {
            vk::ImageMemoryBarrier::builder()
                .image(self.image.image)
                .subresource_range(self.range)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        };
        let old_layout = if self.initialized {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        };
        let Some(buffer) = buffer.filter(|_| !self.uploaded) else {
            if !self.initialized {
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier()
                        .old_layout(old_layout)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .build()],
                );
            }
            return;
        };
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::COMPUTE_SHADER | vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .build()],
            &[],
            &[barrier()
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .build()],
        );
        d.cmd_copy_buffer_to_image(
            cmd,
            buffer,
            self.image.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &self.copies,
        );
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .build()],
        );
    }
}

/// Generate one binding per installed atlas. Unsupported formats need no
/// dummy images or descriptors; all their samples take the decoded fallback.
pub(super) fn material_source(atlases: &[Atlas]) -> String {
    let kinds = atlases.iter().map(|a| a.kind).collect::<Vec<_>>();
    material_source_for(&kinds)
}

fn material_source_for(kinds: &[Kind]) -> String {
    use std::fmt::Write;
    let mut source = String::new();
    let mut cases = String::new();
    for (i, kind) in kinds.iter().copied().enumerate() {
        let channel = match kind {
            Kind::Terrain => continue,
            // BC5 stores only RG; reconstruction of unit-length Z happens
            // *after* filtering in the existing NORMAL_RG material code.
            Kind::Normal => "vec4<f32>(sample.rg, 0., 1.)",
            // Vulkan BC7 sRGB loads already convert RGB but not opacity.
            // Preserve straight-alpha interpolation via premultiplied taps.
            Kind::Color => "vec4<f32>(sample.rgb * sample.a, sample.a)",
        };
        let id = kind.index();
        let binding = 7 + i;
        writeln!(
            source,
            "@group(0) @binding({binding}) var bc_material{id}: texture_2d_array<f32>;"
        )
        .unwrap();
        writeln!(source, r#"
fn bc_material_level{id}(addr: u32, uv: vec2<f32>, edge: bool, mip: u32) -> vec4<f32> {{
    let here = texels[addr + 4u + texels[addr + 2u] + mip];
    let origin = vec2<i32>(i32(here & 255u), i32((here >> 8u) & 255u));
    let layer = i32(here >> 16u);
    let dims = rgba_mip_dims(addr, mip);
    let tap = bilinear(0u, dims, uv, edge);
    var color = vec4<f32>(0.);
    for (var i = 0u; i < 4u; i++) {{
        let p = i32(tap.index[i]);
        let sample = textureLoad(bc_material{id}, origin + vec2<i32>(p % dims.x, p / dims.x), layer, 0);
        color += ({channel}) * tap.weight[i];
    }}
    return color;
}}"#).unwrap();
        writeln!(
            cases,
            "    if (fmt & 1792u) == {}u {{ return bc_material_level{id}(addr,uv,edge,mip); }}",
            256 | id << 9
        )
        .unwrap();
    }
    if !cases.is_empty() {
        writeln!(
            source,
            r#"
fn level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32, fmt: u32) -> vec4<f32> {{
    if (frame.options.x & {ACTIVE}u) != 0u {{
{cases}    }}
    return buffer_level(addr,uv,edge,mip,fmt);
}}"#
        )
        .unwrap();
    }
    source
}

#[cfg(test)]
#[test]
fn sampled_bc_shader_binds_only_the_admitted_formats() {
    for kinds in [
        vec![Kind::Normal],
        vec![Kind::Color],
        vec![Kind::Terrain, Kind::Normal, Kind::Color],
    ] {
        let terrain = if kinds.contains(&Kind::Terrain) {
            include_str!("../bc_pages.wgsl")
        } else {
            ""
        };
        let source = super::super::trace_source_parts(
            super::super::HARDWARE,
            true,
            false,
            terrain,
            &material_source_for(&kinds),
            false,
        );
        super::shader(&source).unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
#[test]
#[ignore = "requires sampled BC5/BC7 arrays on a Vulkan CPU test adapter"]
fn lighter_bc_material_pages_interop() {
    use crate::modules::lighter::{tests::Harness, textures::bc_pages::material_fixture};
    let h = Harness::new(super::super::Mode::Hardware, true);
    let ctx = Context::new(&h.device).unwrap();
    let mut bank = Textures::default();
    let mut add = |kind| {
        let (raw, tile) = material_fixture(kind);
        bank.insert_paged_material(&tile, &raw, 1 << 20)
            .unwrap()
            .unwrap()
    };
    let [bc5, bc7] = [add(Kind::Normal), add(Kind::Color)];
    bank.seal();
    let atlases: Vec<_> = [Kind::Normal, Kind::Color]
        .into_iter()
        .map(|kind| {
            let plan = bank.bc_pages[kind.index()].as_ref().unwrap();
            assert!(supported(&h.device, kind, plan.layers()));
            Atlas::new(&ctx, kind, plan).unwrap()
        })
        .collect();
    // Exercise the actual upload/descriptor and generated sampling code,
    // with the integrator replaced by a small readback entry point. The
    // full shader's cold JIT is not required to test the image formats.
    let mut source = super::super::trace_source_parts(
        super::super::HARDWARE,
        true,
        false,
        "",
        &material_source(&atlases),
        false,
    )
    .replace("fn render(", "fn scene_render(");
    source.push_str(&format!(
        r#"
@compute @workgroup_size(1)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {{
    let addr = select({bc5}u,{bc7}u,id.x>=8u);
    let sample_index = id.x % 8u;
    let edge = (sample_index & 1u)==0u;
    let uv = vec2<f32>(1.05,-0.13);
    let color = filtered_tile(addr,uv,edge,Taps(vec2<f32>(0.),f32(sample_index / 2u) / 3.,1u));
    image[id.x] = Pixel(bitcast<u32>(color.r),bitcast<u32>(color.g),color.b,color.a,0u,0u,0u);
}}
"#
    ));
    let core = Core::new(
        ctx.clone(),
        &super::shader(&source).unwrap(),
        atlases.len(),
        "bc_pages_test",
        &|_| {},
    )
    .unwrap();
    for (i, atlas) in atlases.iter().enumerate() {
        atlas.bind(&core, i as u32 + 7);
    }
    const TESTS: usize = 16;
    const SAMPLE_BYTES: u64 = TESTS as u64 * 28;
    let frame = Buffer::new(
        &ctx,
        std::mem::size_of::<Frame>() as u64,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        HOST,
    )
    .unwrap();
    let tex = Buffer::new(
        &ctx,
        bank.byte_len() as u64,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC,
        HOST,
    )
    .unwrap();
    let result = Buffer::new(
        &ctx,
        SAMPLE_BYTES,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        HOST,
    )
    .unwrap();
    tex.write_segments(bank.byte_segments().scan(0, |at, bytes| {
        let i = *at;
        *at += bytes.len();
        Some((i, bytes))
    }))
    .unwrap();
    let d = &ctx.device;
    for (binding, buffer, ty) in [
        (0, &frame, vk::DescriptorType::UNIFORM_BUFFER),
        (3, &tex, vk::DescriptorType::STORAGE_BUFFER),
        (4, &result, vk::DescriptorType::STORAGE_BUFFER),
    ] {
        let info = vk::DescriptorBufferInfo::builder()
            .buffer(buffer.handle)
            .range(buffer.size)
            .build();
        unsafe {
            d.update_descriptor_sets(
                &[vk::WriteDescriptorSet::builder()
                    .dst_set(core.set)
                    .dst_binding(binding)
                    .descriptor_type(ty)
                    .buffer_info(std::slice::from_ref(&info))
                    .build()],
                &[],
            );
        }
    }
    use bytemuck::Zeroable;
    let draw = |sampled| {
        let mut f = Frame::zeroed();
        f.options[0] = if sampled { ACTIVE } else { 0 };
        frame.write(0, bytemuck::bytes_of(&f)).unwrap();
        core.begin_commands().unwrap();
        let cmd = core.commands;
        unsafe {
            if sampled {
                for atlas in &atlases {
                    atlas.encode(cmd, Some(tex.handle));
                }
            }
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, core.pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                core.layout,
                0,
                &[core.set],
                &[],
            );
            d.cmd_dispatch(cmd, TESTS as u32, 1, 1);
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .build()],
                &[],
                &[],
            );
            core.submit().unwrap();
            d.wait_for_fences(&[core.fence], true, u64::MAX).unwrap();
            let ptr = d
                .map_memory(result.memory, 0, SAMPLE_BYTES, vk::MemoryMapFlags::empty())
                .unwrap()
                .cast::<f32>();
            let rows = std::array::from_fn::<_, TESTS, _>(|i| {
                std::array::from_fn::<_, 4, _>(|j| *ptr.add(i * 7 + j))
            });
            d.unmap_memory(result.memory);
            rows
        }
    };
    let sampled = draw(true);
    let decoded = draw(false);
    let mut error: f32 = 0.;
    assert_ne!(
        decoded[0], decoded[1],
        "edge and repeat must exercise different texels"
    );
    assert_ne!(
        decoded[0], decoded[4],
        "authored mip must change the result"
    );
    assert!(
        decoded[8][3] > 0.5 && decoded[8][3] < 1.,
        "BC7 fixture must carry opacity"
    );
    for (a, b) in sampled.into_iter().zip(decoded) {
        for (a, b) in a.into_iter().zip(b) {
            assert!(a.is_finite() && b.is_finite());
            error = error.max((a - b).abs());
        }
    }
    assert!(
        error <= 0.004,
        "BC5/7 sampler exceeded a 1/255 linear step: {error}"
    );
    // Corrupt only the decoded level-zero BC7 fallback. A real texture
    // read is unchanged; the fallback-only dispatch must observe the edit.
    let words: &[u32] = bytemuck::cast_slice(bank.byte_segments().next().unwrap());
    let addr = u64::from(bc7 + words[bc7 as usize + 4]);
    tex.write(addr * 4, &[0; 7 * 5 * 4]).unwrap();
    assert_ne!(draw(false), decoded);
    assert_eq!(draw(true), sampled);
    let bytes: u64 = atlases.iter().map(|a| a.image.charged).sum();
    eprintln!("Lighter BC5/BC7: {TESTS} NPOT mip/wrap/clamp samples, max linear RGBA error {error:.7}; {} B mixed bank + {bytes} B charged images", bank.byte_len());
}
