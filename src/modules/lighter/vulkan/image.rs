use super::{vkerr, Context, LOCAL, R};
use ash::vk;
use std::sync::Arc;

pub(super) struct Image {
    pub(super) ctx: Arc<Context>,
    pub(super) image: vk::Image,
    memory: vk::DeviceMemory,
    pub(super) view: vk::ImageView,
    pub(super) charged: u64,
}

impl Image {
    pub fn new(
        ctx: &Arc<Context>,
        size: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        range: vk::ImageSubresourceRange,
        view: vk::ImageViewType,
    ) -> R<Self> {
        let d = &ctx.device;
        unsafe {
            let image = d
                .create_image(
                    &vk::ImageCreateInfo::builder()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(format)
                        .extent(vk::Extent3D {
                            width: size.width,
                            height: size.height,
                            depth: 1,
                        })
                        .mip_levels(range.level_count)
                        .array_layers(range.layer_count)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(usage),
                    None,
                )
                .map_err(vkerr("create image"))?;
            let mut out = Self {
                ctx: ctx.clone(),
                image,
                memory: vk::DeviceMemory::null(),
                view: vk::ImageView::null(),
                charged: 0,
            };
            let req = d.get_image_memory_requirements(image);
            let ty = (0..ctx.memory.memory_type_count)
                .find(|&i| {
                    req.memory_type_bits & (1 << i) != 0
                        && ctx.memory.memory_types[i as usize]
                            .property_flags
                            .contains(LOCAL)
                })
                .ok_or("No device-local memory type for Lighter image")?;
            ctx.budget.reserve(req.size)?;
            out.charged = req.size;
            out.memory = d
                .allocate_memory(
                    &vk::MemoryAllocateInfo::builder()
                        .allocation_size(req.size)
                        .memory_type_index(ty),
                    None,
                )
                .map_err(vkerr("allocate image"))?;
            d.bind_image_memory(image, out.memory, 0)
                .map_err(vkerr("bind image"))?;
            out.view = d
                .create_image_view(
                    &vk::ImageViewCreateInfo::builder()
                        .image(image)
                        .view_type(view)
                        .format(format)
                        .subresource_range(range),
                    None,
                )
                .map_err(vkerr("create image view"))?;
            Ok(out)
        }
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            self.ctx.device.destroy_image_view(self.view, None);
            self.ctx.device.destroy_image(self.image, None);
            self.ctx.device.free_memory(self.memory, None);
        }
        self.ctx.budget.release(self.charged);
    }
}
