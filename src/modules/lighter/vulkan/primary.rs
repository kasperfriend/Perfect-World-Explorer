//! Rasterize the *packet*, not a second copy of the Native draw list. The
//! packet's posed vertices, wind transforms, coverage/material rules and
//! extent match the following trace. Ordered sheets remain in transport.
use super::{image::Image, pipeline, shader_entry, vkerr, Context, Frame, Packet, DYNAMIC_BIT, R};
use ash::vk;
use glam::{Mat4, Vec3, Vec4};
use std::{ffi::CStr, sync::Arc};

const COLOR: vk::Format = vk::Format::R32G32B32A32_UINT;
const DEPTH: vk::Format = vk::Format::D32_SFLOAT;

pub(super) fn supported(instance: &ash::Instance, physical: vk::PhysicalDevice) -> bool {
    unsafe {
        let features = |f| {
            instance
                .get_physical_device_format_properties(physical, f)
                .optimal_tiling_features
        };
        features(COLOR).contains(
            vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::SAMPLED_IMAGE,
        ) && features(DEPTH).contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
    }
}

fn attachment(ctx: &Arc<Context>, size: vk::Extent2D, color: bool) -> R<Image> {
    let (format, usage, aspect) = if color {
        (
            COLOR,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            vk::ImageAspectFlags::COLOR,
        )
    } else {
        (
            DEPTH,
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            vk::ImageAspectFlags::DEPTH,
        )
    };
    Image::new(
        ctx,
        size,
        format,
        usage,
        vk::ImageSubresourceRange {
            aspect_mask: aspect,
            level_count: 1,
            layer_count: 1,
            ..Default::default()
        },
        vk::ImageViewType::TYPE_2D,
    )
}

struct Target {
    colors: Image,
    _depth: Image,
    frame: vk::Framebuffer,
    size: vk::Extent2D,
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe { self.colors.ctx.device.destroy_framebuffer(self.frame, None) }
    }
}

#[derive(Clone, Copy)]
struct Draw {
    instance: u32,
    first: u32,
    count: u32,
    center: Vec3,
    radius: Vec3,
}

fn update_draws(draws: &mut Vec<Draw>, packet: &Packet, dynamic: u32) {
    draws.clear();
    // The Hardware packet deliberately hasn't built CPU triangle bounds.
    // Only the opt-in raster path needs these AABBs; keep them off its full
    // ray sibling's packing cost. Cache a bound per shared mesh, not per draw.
    let local: Vec<_> = packet
        .meshes
        .iter()
        .map(|mesh| {
            let first = packet.words[0] as usize;
            let stride = super::VERTEX_WORDS as usize;
            packet.words[first + stride * mesh.vertex.start as usize
                ..first + stride * mesh.vertex.end as usize]
                .chunks_exact(stride)
                .fold(super::super::bvh::Bounds::EMPTY, |b, v| {
                    let p = Vec3::new(
                        f32::from_bits(v[0]),
                        f32::from_bits(v[1]),
                        f32::from_bits(v[2]),
                    );
                    if p.is_finite() {
                        b.union(super::super::bvh::Bounds::point(p))
                    } else {
                        b
                    }
                })
        })
        .collect();
    draws.extend(packet.instances.iter().enumerate().filter_map(|(i, inst)| {
        let m = &packet.meshes[inst.mesh as usize];
        if m.mask & 1 == 0 || m.triangle.is_empty() {
            return None;
        }
        let affine = packet.placement_rows(inst);
        let bounds = local[inst.mesh as usize];
        let local_center = (bounds.min + bounds.max) * 0.5;
        let extents = (bounds.max - bounds.min) * 0.5;
        let row = |k| Vec3::new(affine[k], affine[k + 1], affine[k + 2]);
        Some(Draw {
            instance: i as u32 | dynamic,
            first: 3 * m.triangle.start,
            count: 3 * (m.triangle.end - m.triangle.start),
            center: Vec3::new(
                row(0).dot(local_center) + affine[3],
                row(4).dot(local_center) + affine[7],
                row(8).dot(local_center) + affine[11],
            ),
            radius: Vec3::new(
                row(0).abs().dot(extents),
                row(4).abs().dot(extents),
                row(8).abs().dot(extents),
            ),
        })
    }));
}

pub(super) struct Primary {
    ctx: Arc<Context>,
    render: vk::RenderPass,
    raster: vk::Pipeline,
    pub(super) trace: vk::Pipeline,
    target: Option<Target>,
    draws: [Vec<Draw>; 2],
    revision: Option<u64>,
    // These include GPU-addressed vertex/instance data, not world transforms
    // inferred from a second animation clock.
    moving_current: bool,
}

impl Primary {
    pub(super) fn new(
        ctx: &Arc<Context>,
        layout: vk::PipelineLayout,
        cache: vk::PipelineCache,
        trace_source: &str,
    ) -> R<Self> {
        if !ctx.raster_capable {
            return Err("primary visibility formats or graphics queue unavailable".into());
        }
        let mut out = Self {
            ctx: ctx.clone(),
            render: vk::RenderPass::null(),
            raster: vk::Pipeline::null(),
            trace: vk::Pipeline::null(),
            target: None,
            draws: Default::default(),
            revision: None,
            moving_current: false,
        };
        let source = super::super::raster_source();
        let vs = shader_entry(&source, naga_rt::ShaderStage::Vertex, "visibility_vertex")?;
        let fs = shader_entry(
            &source,
            naga_rt::ShaderStage::Fragment,
            "visibility_fragment",
        )?;
        let compute = shader_entry(trace_source, naga_rt::ShaderStage::Compute, "render_hybrid")?;
        out.trace = pipeline(
            &ctx.device,
            layout,
            cache,
            &compute,
            CStr::from_bytes_with_nul(b"render_hybrid\0").unwrap(),
        )?;
        let d = &ctx.device;
        let attachment = |format, store, final_layout| {
            vk::AttachmentDescription::builder()
                .format(format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(store)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .final_layout(final_layout)
                .build()
        };
        let attachments = [
            attachment(
                COLOR,
                vk::AttachmentStoreOp::STORE,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            ),
            attachment(
                DEPTH,
                vk::AttachmentStoreOp::DONT_CARE,
                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            ),
        ];
        let color = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let depth = vk::AttachmentReference {
            attachment: 1,
            layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        };
        let subpass = [vk::SubpassDescription::builder()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color)
            .depth_stencil_attachment(&depth)
            .build()];
        let writes = vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
            | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
            | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS;
        let compute = vk::PipelineStageFlags::COMPUTE_SHADER;
        let barriers = [
            vk::SubpassDependency::builder()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(compute | writes)
                .dst_stage_mask(writes)
                .src_access_mask(
                    vk::AccessFlags::SHADER_READ | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                )
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                )
                .build(),
            vk::SubpassDependency::builder()
                .src_subpass(0)
                .dst_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_stage_mask(compute)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .build(),
        ];
        unsafe {
            out.render = d
                .create_render_pass(
                    &vk::RenderPassCreateInfo::builder()
                        .attachments(&attachments)
                        .subpasses(&subpass)
                        .dependencies(&barriers),
                    None,
                )
                .map_err(vkerr("create primary render pass"))?;
            out.raster = create_graphics(d, out.render, layout, cache, &vs, &fs)?;
        }
        Ok(out)
    }

    pub(super) fn prepare(
        &mut self,
        frame: &Frame,
        world: &Packet,
        moving: &Packet,
        revision: u64,
        changed: bool,
    ) -> R<vk::ImageView> {
        let [width, height, ..] = frame.extent;
        let size = vk::Extent2D { width, height };
        if self.target.as_ref().is_none_or(|t| t.size != size) {
            // Native fence has signaled; only this queue ever names these.
            self.target = None;
            let colors = attachment(&self.ctx, size, true)?;
            let depth = attachment(&self.ctx, size, false)?;
            let views = [colors.view, depth.view];
            let fb = unsafe {
                self.ctx.device.create_framebuffer(
                    &vk::FramebufferCreateInfo::builder()
                        .render_pass(self.render)
                        .attachments(&views)
                        .width(width)
                        .height(height)
                        .layers(1),
                    None,
                )
            }
            .map_err(vkerr("create primary framebuffer"))?;
            self.target = Some(Target {
                colors,
                _depth: depth,
                frame: fb,
                size,
            });
        }
        if self.revision != Some(revision) {
            update_draws(&mut self.draws[0], world, 0);
            self.revision = Some(revision);
        }
        if !self.moving_current || changed {
            update_draws(&mut self.draws[1], moving, DYNAMIC_BIT);
            self.moving_current = true;
        }
        Ok(self.target.as_ref().unwrap().colors.view)
    }

    pub(super) fn inactive(&mut self) {
        self.target = None;
        self.moving_current = false;
    }

    pub(super) unsafe fn encode(
        &self,
        cmd: vk::CommandBuffer,
        layout: vk::PipelineLayout,
        set: vk::DescriptorSet,
        frame: &Frame,
    ) {
        let d = &self.ctx.device;
        let target = self.target.as_ref().unwrap();
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: target.size,
        };
        let clears = [
            vk::ClearValue {
                color: vk::ClearColorValue {
                    uint32: [u32::MAX, 0, 0, 0],
                },
            },
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 0.,
                    stencil: 0,
                },
            },
        ];
        d.cmd_begin_render_pass(
            cmd,
            &vk::RenderPassBeginInfo::builder()
                .framebuffer(target.frame)
                .render_pass(self.render)
                .render_area(area)
                .clear_values(&clears),
            vk::SubpassContents::INLINE,
        );
        // WGSL clip Y is up; use Vulkan's negative viewport height rather
        // than flipping the shared projection/unprojection transform.
        let height = target.size.height as f32;
        d.cmd_set_viewport(
            cmd,
            0,
            &[vk::Viewport {
                x: 0.,
                y: height,
                width: target.size.width as f32,
                height: -height,
                min_depth: 0.,
                max_depth: 1.,
            }],
        );
        d.cmd_set_scissor(cmd, 0, &[area]);
        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.raster);
        d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, layout, 0, &[set], &[]);
        let p = Mat4::from_cols_array_2d(&frame.vp).transpose();
        let [x, y, z, w] = [p.x_axis, p.y_axis, p.z_axis, p.w_axis];
        let planes = [w + x, w - x, w + y, w - y, z, w - z];
        for bucket in &self.draws {
            let mut run: Option<(Draw, u32)> = None;
            for &draw in bucket.iter().filter(|b| in_view(b, &planes)) {
                if let Some((start, count)) = &mut run {
                    if start.first == draw.first
                        && start.count == draw.count
                        && start.instance + *count == draw.instance
                    {
                        *count += 1;
                        continue;
                    }
                    d.cmd_draw(cmd, start.count, *count, start.first, start.instance);
                }
                run = Some((draw, 1));
            }
            if let Some((start, count)) = run {
                d.cmd_draw(cmd, start.count, count, start.first, start.instance);
            }
        }
        d.cmd_end_render_pass(cmd);
    }
}

fn in_view(draw: &Draw, planes: &[Vec4; 6]) -> bool {
    planes.iter().all(|p| {
        let n = p.truncate();
        // Include contact/roundoff, especially for narrow distant cards.
        let epsilon = 1e-5 * (n.abs().dot(draw.center.abs() + draw.radius) + p.w.abs() + 1.);
        n.dot(draw.center) + p.w + n.abs().dot(draw.radius) >= -epsilon
    })
}

impl Drop for Primary {
    fn drop(&mut self) {
        self.target = None;
        unsafe {
            self.ctx.device.destroy_pipeline(self.trace, None);
            self.ctx.device.destroy_pipeline(self.raster, None);
            self.ctx.device.destroy_render_pass(self.render, None);
        }
    }
}

unsafe fn create_graphics(
    d: &ash::Device,
    render: vk::RenderPass,
    layout: vk::PipelineLayout,
    cache: vk::PipelineCache,
    vs: &[u32],
    fs: &[u32],
) -> R<vk::Pipeline> {
    let create = |code| {
        d.create_shader_module(&vk::ShaderModuleCreateInfo::builder().code(code), None)
            .map_err(vkerr("create visibility shader"))
    };
    let vertex = create(vs)?;
    let fragment = match create(fs) {
        Ok(shader) => shader,
        Err(error) => {
            d.destroy_shader_module(vertex, None);
            return Err(error);
        }
    };
    let stage = |module, stage, name| {
        vk::PipelineShaderStageCreateInfo::builder()
            .module(module)
            .stage(stage)
            .name(name)
            .build()
    };
    let stages = [
        stage(
            vertex,
            vk::ShaderStageFlags::VERTEX,
            CStr::from_bytes_with_nul(b"visibility_vertex\0").unwrap(),
        ),
        stage(
            fragment,
            vk::ShaderStageFlags::FRAGMENT,
            CStr::from_bytes_with_nul(b"visibility_fragment\0").unwrap(),
        ),
    ];
    let attachments = [vk::PipelineColorBlendAttachmentState::builder()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .build()];
    // Ash builder methods consume `&self` and hand out references to temporary
    // structures, so every state must stay alive for the whole driver call;
    // otherwise Vulkan reads stack memory that has already gone out of scope.
    let _builders = (
        vk::PipelineVertexInputStateCreateInfo::default(),
        vk::PipelineInputAssemblyStateCreateInfo::builder()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST),
        vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1),
        vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .line_width(1.),
        vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1),
        vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::GREATER_OR_EQUAL),
        vk::PipelineColorBlendStateCreateInfo::builder().attachments(&attachments),
        vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&[vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]),
    );
    let info = vk::GraphicsPipelineCreateInfo::builder()
        .layout(layout)
        .render_pass(render)
        .stages(&stages)
        .vertex_input_state(&_builders.0)
        .input_assembly_state(&_builders.1)
        .viewport_state(&_builders.2)
        .rasterization_state(&_builders.3)
        .multisample_state(&_builders.4)
        .depth_stencil_state(&_builders.5)
        .color_blend_state(&_builders.6)
        .dynamic_state(&_builders.7)
        .build();
    let result = d.create_graphics_pipelines(cache, &[info], None);
    d.destroy_shader_module(vertex, None);
    d.destroy_shader_module(fragment, None);
    match result {
        Ok(pipelines) => Ok(pipelines[0]),
        Err((pipelines, error)) => {
            for p in pipelines {
                d.destroy_pipeline(p, None);
            }
            Err(vkerr("create visibility graphics pipeline")(error))
        }
    }
}
