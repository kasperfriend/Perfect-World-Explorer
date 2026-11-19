use super::*;
use crate::modules::lighter::textures::bc_pages::{Pages, SIDE};

const FORMAT: vk::Format = vk::Format::BC1_RGBA_SRGB_BLOCK;
const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw() | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);
pub(super) const ACTIVE: u32 = 1 << 17;

pub(super) fn supported(device: &wgpu::Device, layers: u32) -> bool {
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
                        FORMAT,
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
    pages: Arc<Pages>,
    image: image::Image,
    range: vk::ImageSubresourceRange,
    copies: Vec<vk::BufferImageCopy>,
    pub visible: bool,
    pub initialized: bool,
    pub uploaded: bool,
}
impl Atlas {
    pub fn new(ctx: &Arc<Context>, pages: &Arc<Pages>) -> R<Self> {
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
            FORMAT,
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
        texels
            .bc_pages
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
    pub fn bind(&self, core: &Core) {
        let info = vk::DescriptorImageInfo::builder()
            .image_view(self.image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .build();
        unsafe {
            core.ctx.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::builder()
                    .dst_set(core.set)
                    .dst_binding(7)
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
