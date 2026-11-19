//! wgpu 0.19 exposes the Vulkan RT feature bits but does not expose BLAS/TLAS
//! creation in wgpu-core. Keep that interop here, on the viewer's *same* device
//! and queue. No CPU image readback and no second adapter.
use super::scene::{Fingerprint, MeshKey, MeshRange, Packet, DYNAMIC_BIT, VERTEX_WORDS};
use super::{
    grown_capacity, image_limit, profile::INTERVAL, storage_limit, suffix_segments,
    textures::UPLOAD_PAGE_BYTES, Frame, TexelPrefix, Textures, HYBRID_PRIMARY,
    RAY_ALLOCATION_BUDGET,
};
use ash::{extensions::khr, vk};
use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use wgpu::hal::{api::Vulkan, vulkan::Device as HalDevice};

mod bc_pages;
mod cache;
mod image;
mod primary;
mod residency;

pub const FEATURES: wgpu::Features =
    wgpu::Features::RAY_QUERY.union(wgpu::Features::RAY_TRACING_ACCELERATION_STRUCTURE);

pub fn supported(a: &wgpu::Adapter) -> bool {
    let info = a.get_info();
    if info.backend != wgpu::Backend::Vulkan
        || !matches!(
            info.device_type,
            wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
        )
        || !a.features().contains(FEATURES)
    {
        return false;
    }
    // 0.19's adapter advertises the extension names, not these feature values.
    unsafe {
        a.as_hal::<Vulkan, _, _>(|adapter| {
            let Some(a) = adapter else {
                return false;
            };
            let instance = a.shared_instance().raw_instance();
            let physical = a.raw_physical_device();
            let props = instance.get_physical_device_properties(physical);
            let mut acceleration = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
            let mut query = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
            let mut address = vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
            let mut features = vk::PhysicalDeviceFeatures2::builder()
                .push_next(&mut acceleration)
                .push_next(&mut query)
                .push_next(&mut address);
            instance.get_physical_device_features2(physical, &mut features);
            props.api_version >= vk::API_VERSION_1_2
                && acceleration.acceleration_structure != 0
                && query.ray_query != 0
                && address.buffer_device_address != 0
        })
    }
}

pub fn shader(source: &str) -> Result<Vec<u32>, String> {
    shader_entry(source, naga_rt::ShaderStage::Compute, "render")
}

pub(super) fn shader_entry(source: &str, stage: naga_rt::ShaderStage, entry: &str) -> R<Vec<u32>> {
    // Use the guarded getters and separate candidate/committed helpers from
    // Naga 26 without changing wgpu's pinned shader compiler.
    use naga_rt as naga;
    let mut module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    module
        .entry_points
        .retain(|e| e.stage == stage && e.name == entry);
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::RAY_QUERY,
    );
    validator
        .validate(&module)
        .map_err(|e| e.emit_to_string(source))?;
    if module.entry_points.is_empty() {
        return Err(format!("Missing Vulkan shader entry {entry}"));
    }
    naga::compact::compact(&mut module, naga::compact::KeepUnused::No);
    let info = validator
        .validate(&module)
        .map_err(|e| e.emit_to_string(source))?;
    naga::back::spv::write_vec(
        &module,
        &info,
        &naga::back::spv::Options {
            lang_version: (1, 4),
            flags: naga::back::spv::WriterFlags::empty(),
            bounds_check_policies: naga::proc::BoundsCheckPolicies {
                buffer: naga::proc::BoundsCheckPolicy::ReadZeroSkipWrite,
                ..Default::default()
            },
            ..Default::default()
        },
        Some(&naga::back::spv::PipelineOptions {
            shader_stage: stage,
            entry_point: entry.into(),
        }),
    )
    .map_err(|e| format!("Lighter SPIR-V: {e}"))
}

type R<T> = Result<T, String>;
fn vkerr(step: &str) -> impl FnOnce(vk::Result) -> String + '_ {
    move |err| format!("Lighter Vulkan {step}: {err:?}")
}

struct Context {
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    raster_capable: bool,
    identity: (u32, u32, [u8; 16]),
    acceleration: khr::AccelerationStructure,
    addresses: khr::BufferDeviceAddress,
    memory: vk::PhysicalDeviceMemoryProperties,
    heap_probe: Option<(ash::Instance, vk::PhysicalDevice)>,
    scratch_align: u64,
    max_primitives: u64,
    max_instances: u64,
    storage_limit: u64,
    budget: AllocationBudget,
    timestamp_period: f32,
    timestamp_bits: u32,
    buffers_created: AtomicU64,
    structures_created: AtomicU64,
}

struct AllocationBudget {
    limit: u64,
    held: AtomicU64,
}
impl AllocationBudget {
    fn reserve(&self, size: u64) -> R<()> {
        self.held
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(size).filter(|&new| new <= self.limit)
            })
            .map_err(|_| {
                format!(
                    "Lighter Vulkan memory budget ({} MiB) exhausted; reduce scenery or resolution",
                    self.limit >> 20
                )
            })?;
        Ok(())
    }
    fn release(&self, size: u64) {
        self.held.fetch_sub(size, Ordering::Relaxed);
    }
}

pub(super) fn timestamp_ms(start: u64, end: u64, valid_bits: u32, period: f32) -> f64 {
    let mask = u64::MAX >> (64 - valid_bits.clamp(1, 64));
    (end.wrapping_sub(start) & mask) as f64 * f64::from(period) / 1.0e6
}

impl Context {
    fn heap_usage(&self) -> Option<u64> {
        let (instance, physical) = self.heap_probe.as_ref()?;
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        unsafe {
            instance.get_physical_device_memory_properties2(
                *physical,
                &mut vk::PhysicalDeviceMemoryProperties2::builder().push_next(&mut budget),
            );
        }
        Some(
            self.memory.memory_heaps[..self.memory.memory_heap_count as usize]
                .iter()
                .zip(budget.heap_usage)
                .filter(|(h, _)| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
                .map(|(_, bytes)| bytes)
                .sum(),
        )
    }

    // `device` outlives the backend, including every buffer imported into it.
    fn new(device: &wgpu::Device) -> R<Arc<Self>> {
        if !device.features().contains(FEATURES) {
            return Err("Vulkan ray queries were not enabled on this device".into());
        }
        let storage_limit = storage_limit(device) as u64;
        unsafe {
            device
                .as_hal::<Vulkan, _, _>(|hal| {
                    let hal: &HalDevice = hal.ok_or("Viewer does not use Vulkan")?;
                    let instance = hal.shared_instance().raw_instance();
                    let device = hal.raw_device().clone();
                    let physical = hal.raw_physical_device();
                    let mut p = vk::PhysicalDeviceAccelerationStructurePropertiesKHR::default();
                    instance.get_physical_device_properties2(
                        physical,
                        &mut vk::PhysicalDeviceProperties2::builder().push_next(&mut p),
                    );
                    let memory = instance.get_physical_device_memory_properties(physical);
                    let heap_probe = instance
                        .enumerate_device_extension_properties(physical)
                        .ok()
                        .and_then(|exts| {
                            exts.iter()
                                .any(|e| {
                                    CStr::from_ptr(e.extension_name.as_ptr())
                                        == vk::ExtMemoryBudgetFn::name()
                                })
                                .then(|| (instance.clone(), physical))
                        });
                    let local = memory.memory_heaps[..memory.memory_heap_count as usize]
                        .iter()
                        .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
                        .map(|h| h.size)
                        .max()
                        .unwrap_or(RAY_ALLOCATION_BUDGET);
                    let family = hal.queue_family_index();
                    let queues = instance.get_physical_device_queue_family_properties(physical);
                    let timestamp_bits = queues[family as usize].timestamp_valid_bits;
                    let props = instance.get_physical_device_properties(physical);
                    let timestamp_period = props.limits.timestamp_period;
                    Ok(Arc::new(Self {
                        queue: hal.raw_queue(),
                        family,
                        identity: (props.vendor_id, props.device_id, props.pipeline_cache_uuid),
                        raster_capable: queues[family as usize]
                            .queue_flags
                            .contains(vk::QueueFlags::GRAPHICS)
                            && primary::supported(instance, physical),
                        acceleration: khr::AccelerationStructure::new(instance, &device),
                        addresses: khr::BufferDeviceAddress::new(instance, &device),
                        memory,
                        heap_probe,
                        budget: AllocationBudget {
                            limit: local.min(RAY_ALLOCATION_BUDGET),
                            held: AtomicU64::new(0),
                        },
                        timestamp_bits,
                        timestamp_period,
                        scratch_align: u64::from(
                            p.min_acceleration_structure_scratch_offset_alignment,
                        )
                        .max(1),
                        max_primitives: p.max_primitive_count,
                        max_instances: p.max_instance_count,
                        storage_limit,
                        buffers_created: AtomicU64::new(0),
                        structures_created: AtomicU64::new(0),
                        device,
                    }))
                })
                .ok_or("Vulkan HAL is unavailable")?
        }
    }
}

const STORAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::STORAGE_BUFFER;
const ADDRESS: vk::BufferUsageFlags = vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
const BUILD_INPUT: vk::BufferUsageFlags =
    vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
const LOCAL: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
const HOST: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::from_raw(
    vk::MemoryPropertyFlags::HOST_VISIBLE.as_raw()
        | vk::MemoryPropertyFlags::HOST_COHERENT.as_raw(),
);

struct Buffer {
    ctx: Arc<Context>,
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
    charged: u64,
    size: u64,
    owns_handle: bool,
    id: u64,
}

impl Buffer {
    fn new(
        ctx: &Arc<Context>,
        size: u64,
        usage: vk::BufferUsageFlags,
        flags: vk::MemoryPropertyFlags,
    ) -> R<Self> {
        let size = size.max(16);
        let d = &ctx.device;
        unsafe {
            let handle = d
                .create_buffer(
                    &vk::BufferCreateInfo::builder()
                        .size(size)
                        .usage(usage)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE),
                    None,
                )
                .map_err(vkerr("create buffer"))?;
            let mut buffer = Self {
                ctx: ctx.clone(),
                handle,
                memory: vk::DeviceMemory::null(),
                charged: 0,
                size,
                owns_handle: true,
                id: 0,
            };
            let req = d.get_buffer_memory_requirements(handle);
            let index = (0..ctx.memory.memory_type_count)
                .find(|i| {
                    req.memory_type_bits & (1 << i) != 0
                        && ctx.memory.memory_types[*i as usize]
                            .property_flags
                            .contains(flags)
                })
                .ok_or("Lighter: no compatible Vulkan memory type")?;
            let mut info = vk::MemoryAllocateInfo::builder()
                .allocation_size(req.size)
                .memory_type_index(index);
            let mut address = vk::MemoryAllocateFlagsInfo::builder()
                .flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
            if usage.contains(ADDRESS) {
                info = info.push_next(&mut address);
            }
            ctx.budget.reserve(req.size)?;
            buffer.charged = req.size;
            buffer.memory = d
                .allocate_memory(&info, None)
                .map_err(vkerr("allocate buffer"))?;
            d.bind_buffer_memory(handle, buffer.memory, 0)
                .map_err(vkerr("bind buffer"))?;
            buffer.id = ctx.buffers_created.fetch_add(1, Ordering::Relaxed) + 1;
            Ok(buffer)
        }
    }

    fn address(&self) -> vk::DeviceAddress {
        unsafe {
            self.ctx.addresses.get_buffer_device_address(
                &vk::BufferDeviceAddressInfo::builder().buffer(self.handle),
            )
        }
    }

    fn write(&self, offset: u64, data: &[u8]) -> R<()> {
        self.write_segments((!data.is_empty()).then_some((offset as usize, data)))
    }

    fn write_segments<'a>(&self, parts: impl IntoIterator<Item = (usize, &'a [u8])>) -> R<()> {
        let mut parts = parts.into_iter().peekable();
        if parts.peek().is_none() {
            return Ok(());
        }
        unsafe {
            let d = &self.ctx.device;
            let ptr = d
                .map_memory(self.memory, 0, self.size, vk::MemoryMapFlags::empty())
                .map_err(vkerr("map upload"))?;
            for (offset, data) in parts {
                assert!(offset as u64 + data.len() as u64 <= self.size);
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    ptr.cast::<u8>().add(offset),
                    data.len(),
                );
            }
            d.unmap_memory(self.memory);
        }
        Ok(())
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            if self.owns_handle {
                self.ctx.device.destroy_buffer(self.handle, None);
            }
            if self.memory != vk::DeviceMemory::null() {
                self.ctx.device.free_memory(self.memory, None);
            }
        }
        self.ctx.budget.release(self.charged);
    }
}

// Dirty pages amortize decisions over wide comparisons/memcpy. Coalesce
// nearby changes and fall back to one span for very fragmented animation.
const DIRTY_PAGE: usize = 1024;
const MAX_DIRTY_SPANS: usize = 128;
fn copy_range(offset: usize, bytes: usize) -> vk::BufferCopy {
    debug_assert!(offset % 4 == 0 && bytes % 4 == 0);
    vk::BufferCopy {
        src_offset: offset as u64,
        dst_offset: offset as u64,
        size: bytes as u64,
    }
}

fn dirty_copies(before: &[u8], after: &[u8], copies: &mut Vec<vk::BufferCopy>) {
    copies.clear();
    let mut overflow = false;
    for (i, page) in after.chunks(DIRTY_PAGE).enumerate() {
        let at = i * DIRTY_PAGE;
        if before.get(at..at + page.len()) == Some(page) {
            continue;
        }
        if let Some(last) = copies.last_mut() {
            if at as u64 <= last.dst_offset + last.size + DIRTY_PAGE as u64 || overflow {
                last.size = (at + page.len()) as u64 - last.dst_offset;
                continue;
            }
        }
        copies.push(copy_range(at, page.len()));
        if copies.len() == MAX_DIRTY_SPANS {
            let first = copies[0].dst_offset as usize;
            copies.clear();
            copies.push(copy_range(first, at + page.len() - first));
            overflow = true;
        }
    }
}

struct Upload {
    gpu: Arc<Buffer>,
    staging: Option<Buffer>,
    copies: Vec<vk::BufferCopy>,
}
impl Upload {
    fn reserve(
        slot: &mut Option<Self>,
        ctx: &Arc<Context>,
        bytes: usize,
        usage: vk::BufferUsageFlags,
    ) -> R<bool> {
        let needed = (bytes as u64).max(16);
        let old = slot.as_ref().map_or(0, |b| b.gpu.size);
        if old >= needed {
            return Ok(false);
        }
        let limit = if usage.contains(STORAGE) {
            ctx.storage_limit
        } else {
            u64::MAX
        };
        let capacity = grown_capacity(needed, old, limit);
        *slot = Some(Self::device_local(ctx, capacity, usage)?);
        Ok(true)
    }

    fn device_local(ctx: &Arc<Context>, capacity: u64, usage: vk::BufferUsageFlags) -> R<Self> {
        Ok(Self {
            gpu: Arc::new(Buffer::new(
                ctx,
                capacity,
                usage | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC,
                LOCAL,
            )?),
            staging: None,
            copies: Vec::new(),
        })
    }

    fn stage(&mut self, ctx: &Arc<Context>, bytes: u64) -> R<&Buffer> {
        if self.staging.as_ref().is_none_or(|buf| buf.size < bytes) {
            let old = self.staging.as_ref().map_or(0, |buf| buf.size);
            let capacity = grown_capacity(bytes, old, self.gpu.size);
            self.staging = Some(Buffer::new(
                ctx,
                capacity,
                vk::BufferUsageFlags::TRANSFER_SRC,
                HOST,
            )?);
        }
        Ok(self.staging.as_ref().unwrap())
    }

    fn set(
        slot: &mut Option<Self>,
        ctx: &Arc<Context>,
        data: &[u8],
        usage: vk::BufferUsageFlags,
    ) -> R<()> {
        Self::set_segments(slot, ctx, [data], data.len(), usage, 0)
    }

    fn set_segments<'a>(
        slot: &mut Option<Self>,
        ctx: &Arc<Context>,
        segments: impl IntoIterator<Item = &'a [u8]>,
        bytes: usize,
        usage: vk::BufferUsageFlags,
        retained_prefix: usize,
    ) -> R<()> {
        let grew = Self::reserve(slot, ctx, bytes, usage)?;
        let buffer = slot.as_mut().unwrap();
        let prefix = if grew || retained_prefix > bytes {
            0
        } else {
            retained_prefix
        };
        buffer.copies.clear();
        if prefix < bytes {
            // The published static bank has no full-size host mirror. If
            // the moving feed adds a suffix, stage just its new bytes.
            buffer.stage(ctx, (bytes - prefix) as u64)?.write_segments(
                suffix_segments(segments, prefix).map(|(at, part)| (at - prefix, part)),
            )?;
            buffer
                .copies
                .extend(
                    (prefix..bytes)
                        .step_by(UPLOAD_PAGE_BYTES)
                        .map(|at| vk::BufferCopy {
                            src_offset: (at - prefix) as u64,
                            dst_offset: at as u64,
                            size: (bytes - at).min(UPLOAD_PAGE_BYTES) as u64,
                        }),
                );
        }
        Ok(())
    }

    fn set_changed(
        slot: &mut Option<Self>,
        ctx: &Arc<Context>,
        data: &[u8],
        usage: vk::BufferUsageFlags,
        previous: &[u8],
    ) -> R<()> {
        let grew = Self::reserve(slot, ctx, data.len(), usage)?;
        let buffer = slot.as_mut().unwrap();
        if grew {
            buffer.copies.clear();
            if !data.is_empty() {
                buffer.copies.push(copy_range(0, data.len()));
            }
        } else {
            dirty_copies(previous, data, &mut buffer.copies);
        }
        if let Some(end) = buffer.copies.last().map(|c| c.src_offset + c.size) {
            // Reserve before borrowing the dirty-span list. A moving buffer
            // can grow when a changed span extends past its former tail.
            buffer.stage(ctx, end)?;
            buffer
                .staging
                .as_ref()
                .unwrap()
                .write_segments(buffer.copies.iter().map(|copy| {
                    let at = copy.dst_offset as usize;
                    (at, &data[at..at + copy.size as usize])
                }))?;
        }
        Ok(())
    }

    fn copied_bytes(&self) -> u64 {
        self.copies.iter().map(|r| r.size).sum()
    }

    unsafe fn encode(&self, cmd: vk::CommandBuffer) {
        if !self.copies.is_empty() {
            self.gpu.ctx.device.cmd_copy_buffer(
                cmd,
                self.staging.as_ref().unwrap().handle,
                self.gpu.handle,
                &self.copies,
            );
        }
    }
}

struct Structure {
    storage: Buffer,
    handle: vk::AccelerationStructureKHR,
    update_scratch: u64,
}
impl Structure {
    fn new(ctx: &Arc<Context>, bytes: u64, ty: vk::AccelerationStructureTypeKHR) -> R<Self> {
        let storage = Buffer::new(
            ctx,
            bytes,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR | ADDRESS,
            LOCAL,
        )?;
        let handle = unsafe {
            ctx.acceleration.create_acceleration_structure(
                &vk::AccelerationStructureCreateInfoKHR::builder()
                    .buffer(storage.handle)
                    .size(bytes)
                    .ty(ty),
                None,
            )
        }
        .map_err(vkerr("create acceleration structure"))?;
        ctx.structures_created.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            storage,
            handle,
            update_scratch: 0,
        })
    }

    fn address(&self) -> u64 {
        unsafe {
            self.storage
                .ctx
                .acceleration
                .get_acceleration_structure_device_address(
                    &vk::AccelerationStructureDeviceAddressInfoKHR::builder()
                        .acceleration_structure(self.handle),
                )
        }
    }
}
impl Drop for Structure {
    fn drop(&mut self) {
        unsafe {
            self.storage
                .ctx
                .acceleration
                .destroy_acceleration_structure(self.handle, None);
        }
    }
}

struct Build {
    geometry: vk::AccelerationStructureGeometryKHR,
    ty: vk::AccelerationStructureTypeKHR,
    count: u32,
    allow_update: bool,
    update: bool,
}
impl Build {
    fn info(&self) -> vk::AccelerationStructureBuildGeometryInfoKHRBuilder<'_> {
        vk::AccelerationStructureBuildGeometryInfoKHR::builder()
            .ty(self.ty)
            .mode(if self.update {
                vk::BuildAccelerationStructureModeKHR::UPDATE
            } else {
                vk::BuildAccelerationStructureModeKHR::BUILD
            })
            .flags(if self.allow_update {
                vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE
                    | vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD
            } else {
                vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
            })
            .geometries(std::slice::from_ref(&self.geometry))
    }

    fn prepare(&self, ctx: &Arc<Context>, slot: &mut Option<Structure>) -> R<u64> {
        if self.update {
            return Ok(slot
                .as_ref()
                .expect("update uses the original AS")
                .update_scratch);
        }
        let sizes = unsafe {
            ctx.acceleration.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &self.info(),
                &[self.count],
            )
        };
        let capacity = slot.as_ref().map_or(0, |s| s.storage.size);
        if capacity < sizes.acceleration_structure_size || slot.is_none() {
            let capacity = grown_capacity(sizes.acceleration_structure_size, capacity, u64::MAX);
            *slot = Some(Structure::new(ctx, capacity, self.ty)?);
        }
        slot.as_mut().unwrap().update_scratch = sizes.update_scratch_size;
        Ok(sizes.build_scratch_size)
    }

    unsafe fn encode(&self, ctx: &Context, cmd: vk::CommandBuffer, dest: &Structure, scratch: u64) {
        let info = self
            .info()
            .src_acceleration_structure(if self.update {
                dest.handle
            } else {
                vk::AccelerationStructureKHR::null()
            })
            .dst_acceleration_structure(dest.handle)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: scratch,
            })
            .build();
        ctx.acceleration.cmd_build_acceleration_structures(
            cmd,
            &[info],
            &[&[vk::AccelerationStructureBuildRangeInfoKHR {
                primitive_count: self.count,
                ..Default::default()
            }]],
        );
        // Make BLAS writes visible to TLAS and serialize reuse of the scratch
        // address. Also covers the final TLAS -> compute query dependency.
        let access = vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR;
        ctx.device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR,
            vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR
                | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[vk::MemoryBarrier::builder()
                .src_access_mask(access)
                .dst_access_mask(access | vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR)
                .build()],
            &[],
            &[],
        );
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct TlasInstance {
    rows: [f32; 12],
    index_mask: u32,
    sbt_flags: u32,
    address: u64,
}

fn pipeline(
    d: &ash::Device,
    layout: vk::PipelineLayout,
    cache: vk::PipelineCache,
    spirv: &[u32],
    entry: &CStr,
) -> R<vk::Pipeline> {
    unsafe {
        let shader = d
            .create_shader_module(&vk::ShaderModuleCreateInfo::builder().code(spirv), None)
            .map_err(vkerr("create ray-query shader"))?;
        let stage = vk::PipelineShaderStageCreateInfo::builder()
            .module(shader)
            .stage(vk::ShaderStageFlags::COMPUTE)
            .name(entry);
        let result = d.create_compute_pipelines(
            cache,
            &[vk::ComputePipelineCreateInfo::builder()
                .stage(*stage)
                .layout(layout)
                .build()],
            None,
        );
        d.destroy_shader_module(shader, None);
        match result {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((pipelines, error)) => {
                for p in pipelines {
                    d.destroy_pipeline(p, None);
                }
                Err(vkerr("create ray-query pipeline")(error))
            }
        }
    }
}

struct Core {
    cache: cache::PipelineCache,
    ctx: Arc<Context>,
    pool: vk::CommandPool,
    commands: vk::CommandBuffer,
    fence: vk::Fence,
    descriptors: vk::DescriptorPool,
    descriptor_layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    query: vk::QueryPool,
}

impl Core {
    fn new(ctx: Arc<Context>, spirv: &[u32], bc: usize) -> R<Self> {
        let (vendor, device, uuid) = ctx.identity;
        let mut c = Self {
            cache: cache::PipelineCache::new(&ctx.device, vendor, device, uuid),
            ctx,
            pool: vk::CommandPool::null(),
            commands: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            descriptors: vk::DescriptorPool::null(),
            descriptor_layout: vk::DescriptorSetLayout::null(),
            set: vk::DescriptorSet::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            query: vk::QueryPool::null(),
        };
        let d = &c.ctx.device;
        unsafe {
            c.pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder().queue_family_index(c.ctx.family),
                    None,
                )
                .map_err(vkerr("create command pool"))?;
            c.commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(c.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(vkerr("allocate commands"))?[0];
            c.fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(vkerr("create fence"))?;
            let types = [
                vk::DescriptorType::UNIFORM_BUFFER,
                vk::DescriptorType::STORAGE_BUFFER,
                vk::DescriptorType::STORAGE_BUFFER,
                vk::DescriptorType::STORAGE_BUFFER,
                vk::DescriptorType::STORAGE_BUFFER,
                vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
                vk::DescriptorType::SAMPLED_IMAGE,
                vk::DescriptorType::SAMPLED_IMAGE, // optional BC1, BC5 and BC7 atlases
                vk::DescriptorType::SAMPLED_IMAGE,
                vk::DescriptorType::SAMPLED_IMAGE,
            ];
            let bindings: Vec<_> = types[..7 + bc]
                .iter()
                .copied()
                .enumerate()
                .map(|(i, descriptor_type)| {
                    vk::DescriptorSetLayoutBinding::builder()
                        .binding(i as u32)
                        .descriptor_type(descriptor_type)
                        .descriptor_count(1)
                        .stage_flags(
                            vk::ShaderStageFlags::COMPUTE
                                | if i < 4 && c.ctx.raster_capable {
                                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT
                                } else {
                                    vk::ShaderStageFlags::empty()
                                },
                        )
                        .build()
                })
                .collect();
            c.descriptor_layout = d
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings),
                    None,
                )
                .map_err(vkerr("create descriptor layout"))?;
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: types[0],
                    descriptor_count: 1,
                },
                vk::DescriptorPoolSize {
                    ty: types[1],
                    descriptor_count: 4,
                },
                vk::DescriptorPoolSize {
                    ty: types[5],
                    descriptor_count: 1,
                },
                vk::DescriptorPoolSize {
                    ty: types[6],
                    descriptor_count: 1 + bc as u32,
                },
            ];
            c.descriptors = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::builder()
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(vkerr("create descriptor pool"))?;
            c.set = d
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::builder()
                        .descriptor_pool(c.descriptors)
                        .set_layouts(&[c.descriptor_layout]),
                )
                .map_err(vkerr("allocate descriptor set"))?[0];
            c.layout = d
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::builder().set_layouts(&[c.descriptor_layout]),
                    None,
                )
                .map_err(vkerr("create pipeline layout"))?;
            c.pipeline = pipeline(
                d,
                c.layout,
                c.cache.handle,
                spirv,
                CStr::from_bytes_with_nul(b"render\0").unwrap(),
            )?;
        }
        c.cache.save();
        if c.ctx.timestamp_bits > 0 {
            // Profiling is optional; inability to create a query pool does not
            // make an otherwise suitable RT device unavailable.
            c.query = unsafe {
                c.ctx.device.create_query_pool(
                    &vk::QueryPoolCreateInfo::builder()
                        .query_type(vk::QueryType::TIMESTAMP)
                        .query_count(4),
                    None,
                )
            }
            .unwrap_or_default();
        }
        Ok(c)
    }

    /// The caller has already observed the fence before reusing this pool.
    fn begin_commands(&self) -> R<()> {
        let d = &self.ctx.device;
        unsafe {
            d.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .map_err(vkerr("reset commands"))?;
            d.begin_command_buffer(
                self.commands,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(vkerr("begin commands"))
        }
    }

    fn submit(&self) -> R<()> {
        let d = &self.ctx.device;
        unsafe {
            d.end_command_buffer(self.commands)
                .map_err(vkerr("end commands"))?;
            d.reset_fences(&[self.fence])
                .map_err(vkerr("reset fence"))?;
            d.queue_submit(
                self.ctx.queue,
                &[vk::SubmitInfo::builder()
                    .command_buffers(std::slice::from_ref(&self.commands))
                    .build()],
                self.fence,
            )
            .map_err(vkerr("submit ray commands"))
        }
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        unsafe {
            let d = &self.ctx.device;
            d.destroy_query_pool(self.query, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_descriptor_pool(self.descriptors, None);
            d.destroy_descriptor_set_layout(self.descriptor_layout, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

struct Output {
    // wgpu owns VkBuffer after import. We own its allocation. The interop
    // path observes completion of both native and wgpu work on the same
    // VkQueue before replacement/drop. `destroy()` invalidates the import
    // before we free the allocation; no later commands can use it.
    imported: Option<wgpu::Buffer>,
    raw: Buffer,
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Some(buffer) = &self.imported {
            buffer.destroy();
        }
    }
}

type ResidentKey = (u32, MeshKey);

#[derive(Default)]
struct ResidentBlas {
    accel: Option<Structure>,
    // Commit only after successful submission; a newly allocated AS is not
    // yet a valid source for an update.
    shape: Option<Fingerprint>,
}

struct AccelPlan {
    live: HashSet<ResidentKey>,
    blas: Vec<(ResidentKey, Fingerprint, Build)>,
    members: Vec<ResidentKey>,
    instance_data: Vec<TlasInstance>,
    tlas: Option<Build>,
    scratch_bytes: u64,
}

pub struct Renderer {
    primary: Option<primary::Primary>,
    primary_failed: bool,
    primary_error_extent: Option<[u32; 4]>,
    last_raster: bool,
    primary_source: Option<String>,
    core: Core,
    bc_pages: Vec<bc_pages::Atlas>,
    // Kept by the engine until device teardown; raw uses are not tracked by it.
    world: Option<Upload>,
    moving: Option<Upload>,
    textures: Option<Upload>,
    instances: Option<Upload>,
    blas: HashMap<ResidentKey, ResidentBlas>,
    tlas: Option<Structure>,
    members: Vec<ResidentKey>,
    instance_data: Vec<TlasInstance>,
    last_moving_keys: Vec<MeshKey>,
    scratch: Option<Buffer>,
    frame: Buffer,
    output: Option<Output>,
    submitted: bool,
    scene_upload_pending: bool,
    resident_dirty: bool,
    resident_upload: Option<residency::ResidentUpload>,
    timing_pending: Option<([u32; 4], bool, bool, u64, u64)>,
    last_gpu: Option<([u32; 4], [f64; 3])>,
    heap_usage: Option<u64>,
    last_timing: Option<Instant>,
    revision: Option<u64>,
    texture_len: usize,
    texture_prefix: TexelPrefix,
    last_moving: Vec<u32>,
    bound_ids: Option<[u64; 6]>,
    pub(super) as_builds: [u64; 3],
    pub(super) as_refits: [u64; 3],
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        compact: bool,
        reuse: bool,
        pages: Option<&super::textures::bc_pages::Plans>,
        temporal: bool,
    ) -> R<Self> {
        Self::new_tracked(device, compact, reuse, pages, temporal, |_| {})
    }

    pub(super) fn new_tracked(
        device: &wgpu::Device,
        compact: bool,
        reuse: bool,
        pages: Option<&super::textures::bc_pages::Plans>,
        temporal: bool,
        progress: impl Fn(&'static str),
    ) -> R<Self> {
        let ctx = Context::new(device)?;
        use super::textures::bc_pages::{Kind, KINDS};
        let mut bc_pages = Vec::new();
        if let Some(pages) = pages {
            for kind in KINDS {
                let Some(plan) = &pages[kind.index()] else {
                    continue;
                };
                if !bc_pages::supported(device, kind, plan.layers()) {
                    eprintln!("Lighter {kind:?} BC sampling unsupported; using the buffer tile");
                    continue;
                }
                match bc_pages::Atlas::new(&ctx, kind, plan) {
                    Ok(atlas) => bc_pages.push(atlas),
                    Err(error) if error.contains("ERROR_DEVICE_LOST") => return Err(error),
                    Err(error) => eprintln!(
                        "Lighter {kind:?} BC image unavailable; using the buffer tile: {error}"
                    ),
                }
            }
        }
        let build = |bc: &[bc_pages::Atlas]| -> R<(String, Core)> {
            let source = super::trace_source_parts(
                super::HARDWARE,
                compact || pages.is_some(),
                reuse,
                if bc.first().is_some_and(|a| a.kind == Kind::Terrain) {
                    include_str!("bc_pages.wgsl")
                } else {
                    ""
                },
                &bc_pages::material_source(bc),
                temporal,
            );
            let spirv = shader(&source)?;
            progress("Ray shader validated and lowered; creating compute pipeline");
            let core = Core::new(ctx.clone(), &spirv, bc.len())?;
            progress("Ray compute pipelines compiled: 1");
            Ok((source, core))
        };
        let (source, core) = match build(&bc_pages) {
            Err(error) if !bc_pages.is_empty() && !error.contains("ERROR_DEVICE_LOST") => {
                eprintln!("Lighter BC sampling pipeline unavailable; using buffer tiles: {error}");
                bc_pages.clear();
                build(&[])?
            }
            result => result?,
        };
        for (i, atlas) in bc_pages.iter().enumerate() {
            atlas.bind(&core, i as u32 + 7);
        }
        let frame = Buffer::new(
            &ctx,
            std::mem::size_of::<Frame>() as u64,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            HOST,
        )?;
        Ok(Self {
            primary: None,
            primary_failed: false,
            primary_error_extent: None,
            last_raster: false,
            primary_source: Some(source),
            core,
            bc_pages,
            world: None,
            moving: None,
            textures: None,
            instances: None,
            blas: HashMap::new(),
            tlas: None,
            members: Vec::new(),
            instance_data: Vec::new(),
            last_moving_keys: Vec::new(),
            scratch: None,
            frame,
            output: None,
            submitted: false,
            scene_upload_pending: false,
            resident_dirty: false,
            resident_upload: None,
            timing_pending: None,
            last_gpu: None,
            heap_usage: None,
            last_timing: None,
            revision: None,
            texture_len: 0,
            texture_prefix: TexelPrefix::default(),
            last_moving: Vec::new(),
            bound_ids: None,
            as_builds: [0; 3],
            as_refits: [0; 3],
        })
    }

    #[cfg(test)]
    pub(super) fn simulate_primary_allocation_failure(&mut self, extent: [u32; 4]) {
        self.primary_error_extent = Some(extent);
    }

    pub(super) fn initial_residency(
        &mut self,
        revision: u64,
        packet: &Packet,
        textures: &Textures,
    ) -> R<(bool, super::residency::Stats)> {
        if self.revision == Some(revision) {
            return Ok((true, super::residency::Stats::default()));
        }
        let limit = 1 << 20;
        if self.stage_resident_inner(revision, packet, textures, limit, true)? {
            return self
                .publish_resident(revision, textures)
                .map(|stats| (true, stats))
                .ok_or_else(|| "initial ray residency was not ready to publish".into());
        }
        Ok((false, self.finished_upload_stats()))
    }

    pub(super) fn raster_active(&self) -> bool {
        self.last_raster
    }

    pub(super) fn allocation_counts(&self) -> [u64; 3] {
        [
            self.core.ctx.buffers_created.load(Ordering::Relaxed),
            1,
            self.core.ctx.structures_created.load(Ordering::Relaxed),
        ]
    }

    fn blas(ctx: &Context, packet: &Packet, m: &MeshRange, base: u64, update: bool) -> R<Build> {
        let count = m.triangle.len() as u32;
        if u64::from(count) > ctx.max_primitives {
            return Err("Lighter mesh exceeds Vulkan BLAS primitive limit".into());
        }
        let v = packet.words[0] + m.vertex.start * VERTEX_WORDS;
        let i = packet.words[1] + m.triangle.start * 3;
        let triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::builder()
            .vertex_data(vk::DeviceOrHostAddressConstKHR {
                device_address: base + 4 * u64::from(v),
            })
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_stride(u64::from(VERTEX_WORDS) * 4)
            .max_vertex(m.vertex.len() as u32 - 1)
            .index_type(vk::IndexType::UINT32)
            .index_data(vk::DeviceOrHostAddressConstKHR {
                device_address: base + 4 * u64::from(i),
            })
            .build();
        Ok(Build {
            count,
            ty: vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
            // Opacity is per-instance metadata. Keep the same flags on every
            // build/update so changing a material never discards geometry.
            geometry: vk::AccelerationStructureGeometryKHR::builder()
                .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
                .flags(vk::GeometryFlagsKHR::NO_DUPLICATE_ANY_HIT_INVOCATION)
                .geometry(vk::AccelerationStructureGeometryDataKHR { triangles })
                .build(),
            allow_update: matches!(m.key, MeshKey::Posed(..)),
            update,
        })
    }

    fn prepare_accel(&mut self, world: &Packet, moving: &Packet) -> R<AccelPlan> {
        let ctx = &self.core.ctx;
        let mut plan = AccelPlan {
            live: HashSet::new(),
            blas: Vec::new(),
            members: Vec::new(),
            instance_data: Vec::new(),
            tlas: None,
            scratch_bytes: 0,
        };
        plan.live.extend(world.meshes.iter().map(|m| (0, m.key)));
        plan.live
            .extend(moving.meshes.iter().map(|m| (DYNAMIC_BIT, m.key)));
        // The previous fence retired. Evict departed chunks before growing
        // storage for their replacements, rather than peaking at both sets.
        self.blas.retain(|key, _| plan.live.contains(key));
        let mut packed = HashSet::new();
        let mut instances = Vec::with_capacity(world.instances.len() + moving.instances.len());
        for (packet, upload, bit) in [
            (world, self.world.as_ref().unwrap(), 0),
            (moving, self.moving.as_ref().unwrap(), DYNAMIC_BIT),
        ] {
            let base = upload.gpu.address();
            for m in &packet.meshes {
                let key = (bit, m.key);
                if !packed.insert(key) {
                    continue;
                }
                let resident = self.blas.entry(key).or_default();
                if resident.shape != Some(m.shape) {
                    let update = resident.shape.is_some();
                    let build = Self::blas(ctx, packet, m, base, update)?;
                    plan.scratch_bytes = plan
                        .scratch_bytes
                        .max(build.prepare(ctx, &mut resident.accel)?);
                    plan.blas.push((key, m.shape, build));
                }
            }
            for (id, i) in packet.instances.iter().enumerate() {
                let m = &packet.meshes[i.mesh as usize];
                let key = (bit, m.key);
                instances.push((
                    key,
                    TlasInstance {
                        rows: packet.placement_rows(i),
                        index_mask: (id as u32 | bit) | (u32::from(m.mask) << 24),
                        sbt_flags: if m.opaque {
                            vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE.as_raw() << 24
                        } else {
                            0
                        },
                        address: self.blas[&key].accel.as_ref().unwrap().address(),
                    },
                ));
            }
        }
        // Canonical order makes packet compaction/reordering a payload update
        // rather than a change to TLAS topology. Custom indices still address
        // the actual per-packet instance records in the shader.
        instances.sort_by_key(|&(key, _)| key);
        (plan.members, plan.instance_data) = instances.into_iter().unzip();
        let rebuild = self.revision.is_none() || plan.members != self.members;
        if rebuild || plan.instance_data != self.instance_data || !plan.blas.is_empty() {
            if plan.instance_data.len() as u64 > ctx.max_instances {
                return Err("Lighter scene exceeds Vulkan TLAS instance limit".into());
            }
            Upload::set_changed(
                &mut self.instances,
                ctx,
                bytemuck::cast_slice(&plan.instance_data),
                BUILD_INPUT | ADDRESS,
                bytemuck::cast_slice(&self.instance_data),
            )?;
            let tlas = Build {
                ty: vk::AccelerationStructureTypeKHR::TOP_LEVEL,
                count: plan.instance_data.len() as u32,
                geometry: vk::AccelerationStructureGeometryKHR::builder()
                    .geometry_type(vk::GeometryTypeKHR::INSTANCES)
                    .geometry(vk::AccelerationStructureGeometryDataKHR {
                        instances: vk::AccelerationStructureGeometryInstancesDataKHR::builder()
                            .data(vk::DeviceOrHostAddressConstKHR {
                                device_address: self.instances.as_ref().unwrap().gpu.address(),
                            })
                            .build(),
                    })
                    .build(),
                allow_update: true,
                update: !rebuild,
            };
            // Refresh the TLAS bounds after an in-place BLAS refit. Drivers
            // need not trace against the new extents of the old address until
            // the TLAS has also been updated. This is not a topology rebuild.
            plan.scratch_bytes = plan.scratch_bytes.max(tlas.prepare(ctx, &mut self.tlas)?);
            plan.tlas = Some(tlas);
        }
        Ok(plan)
    }

    pub(super) fn gpu_stages(&self) -> Option<([u32; 4], [f64; 3])> {
        self.last_gpu
    }

    pub(super) fn bc_page_stats(&self) -> [u64; 3] {
        let mut sum = [0; 3];
        for atlas in &self.bc_pages {
            let [tiles, layers, charged] = atlas.stats();
            if atlas.visible {
                sum[0] += tiles;
                sum[1] += layers;
            }
            // A replaced bank can fall back without releasing the immutable
            // images. Their live allocations still count toward the cost.
            sum[2] += charged;
        }
        sum
    }

    pub(super) fn bc_material_tiles(&self) -> [u64; 2] {
        std::array::from_fn(|i| {
            self.bc_pages
                .iter()
                .find(|atlas| atlas.kind.index() == i + 1 && atlas.visible)
                .map_or(0, |atlas| atlas.stats()[0])
        })
    }

    pub(super) fn memory_usage(&self) -> (u64, u64, Option<u64>) {
        let b = &self.core.ctx.budget;
        (b.held.load(Ordering::Relaxed), b.limit, self.heap_usage)
    }

    pub(super) fn same_moving_lighting(&self, moving: &Packet) -> bool {
        super::scene::same_moving_lighting(&self.last_moving, &moving.words)
    }

    pub(super) fn moving_unchanged(&self, moving: &Packet) -> bool {
        !self.scene_upload_pending && self.last_moving == moving.words
    }

    /// Later-frame probe for the single command pool/fence/descriptor set and
    /// mutable AS/upload resources. No CPU writes or resets while in flight.
    pub(super) fn poll_frame(&mut self) -> R<bool> {
        if self.submitted {
            let ready = unsafe { self.core.ctx.device.get_fence_status(self.core.fence) }
                .map_err(vkerr("poll ray frame"))?;
            self.submitted = !ready;
            if ready {
                // A missing/unreadable timestamp on this fence must not
                // be paired with the new wgpu compose sample at the same extent.
                self.last_gpu = None;
                if let Some((extent, built, raster, moving_bytes, instance_bytes)) =
                    self.timing_pending.take()
                {
                    let ctx = &self.core.ctx;
                    let mut ticks = [0u64; 4];
                    let result = unsafe {
                        ctx.device.get_query_pool_results(
                            self.core.query,
                            0,
                            4,
                            &mut ticks,
                            vk::QueryResultFlags::TYPE_64,
                        )
                    };
                    if result.is_ok() {
                        let duration = |i: usize| {
                            timestamp_ms(
                                ticks[i],
                                ticks[i + 1],
                                ctx.timestamp_bits,
                                ctx.timestamp_period,
                            )
                        };
                        let build = if built { duration(0) } else { 0. };
                        let primary = if raster { duration(1) } else { 0. };
                        let trace = duration(2);
                        self.last_gpu = Some((extent, [build, primary, trace]));
                        if self.last_timing.is_none_or(|t| t.elapsed() >= INTERVAL) {
                            self.heap_usage = ctx.heap_usage();
                            let bytes = ctx.budget.held.load(Ordering::Relaxed);
                            let heap = self
                                .heap_usage
                                .map_or(String::new(), |n| format!(" heap_usage_bytes={n}"));
                            eprintln!("Lighter gpu stage Vulkan: build_ms={build:.3} primary_ms={primary:.3} traversal_ms={trace:.3} allocation_bytes={bytes} budget_bytes={} moving_copy_bytes={moving_bytes} instance_copy_bytes={instance_bytes}{heap}",ctx.budget.limit);
                            self.last_timing = Some(Instant::now());
                        }
                    }
                }
            }
        }
        Ok(!self.submitted)
    }

    // Called during preflight, before swapchain acquisition. Both the raw
    // Vulkan code and the optional raster sibling share the driver cache.
    pub(super) fn prewarm_primary(&mut self) {
        let Some(source) = self.primary_source.take() else {
            return;
        };
        match primary::Primary::new(
            &self.core.ctx,
            self.core.layout,
            self.core.cache.handle,
            &source,
        ) {
            Ok(primary) => {
                self.primary = Some(primary);
                self.core.cache.save();
            }
            Err(error) => {
                self.primary_failed = true;
                eprintln!("Lighter primary raster unavailable; using full rays: {error}");
            }
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn image_for_test(&self) -> &wgpu::Buffer {
        self.output.as_ref().unwrap().imported.as_ref().unwrap()
    }

    pub(super) fn try_prepare_image(&mut self, device: &wgpu::Device, size: u64) -> R<bool> {
        // Retire a cancelled candidate, but live frames read independent
        // buffers and needn't wait on the residency fence to record/submit.
        self.poll_resident_upload()?;
        if !self.poll_frame()? {
            return Ok(false);
        }
        let ctx = &self.core.ctx;
        let d = &ctx.device;
        let old_capacity = self.output.as_ref().map_or(0, |o| o.raw.size);
        if old_capacity < size {
            // The native fence does not cover the wgpu denoiser/present read
            // submitted *after* it. Don't destroy the imported VkBuffer (or
            // its externally owned allocation) until wgpu has retired it.
            if self.output.as_ref().is_some_and(|o| o.imported.is_some())
                && !device.poll(wgpu::Maintain::Poll).is_queue_empty()
            {
                return Ok(false);
            }
            let capacity = grown_capacity(size, old_capacity, image_limit(device) & !3);
            let raw = Buffer::new(
                ctx,
                capacity,
                STORAGE | vk::BufferUsageFlags::TRANSFER_DST,
                LOCAL,
            )?;
            // wgpu's import contract requires initialized storage. Queue a
            // cheap fill (not a redundant ray dispatch) and import only after
            // its fence signals; this also works for device-local VRAM.
            self.core.begin_commands()?;
            unsafe {
                d.cmd_fill_buffer(self.core.commands, raw.handle, 0, capacity, 0);
                d.cmd_pipeline_barrier(
                    self.core.commands,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[vk::MemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                        .build()],
                    &[],
                    &[],
                );
            }
            self.core.submit()?;
            self.submitted = true;
            self.output = Some(Output {
                imported: None,
                raw,
            });
            return Ok(false);
        }
        let output = self.output.as_mut().unwrap();
        if output.imported.is_none() {
            output.imported = Some(unsafe {
                device.create_buffer_from_hal::<Vulkan>(
                    HalDevice::buffer_from_raw(output.raw.handle),
                    &wgpu::BufferDescriptor {
                        label: Some("Lighter ray-query image"),
                        size: output.raw.size,
                        usage: wgpu::BufferUsages::STORAGE,
                        mapped_at_creation: false,
                    },
                )
            });
            output.raw.owns_handle = false;
        }
        Ok(true)
    }

    pub fn trace(
        &mut self,
        frame: &Frame,
        image_size: u64,
        world: &Packet,
        moving: &Packet,
        texels: &Textures,
        revision: u64,
    ) -> R<&wgpu::Buffer> {
        // Raw submit and wgpu composition both run on the render thread on
        // the same VkQueue. `try_prepare_image` has observed the native fence.
        assert!(!self.submitted);
        if self.scene_upload_pending {
            // An earlier attempt failed before submitting. It may have
            // replaced buffers or evicted ASes. A retry cannot compare
            // against those resources as if the previous bytes survived.
            self.revision = None;
            self.texture_prefix = TexelPrefix::default();
            self.last_moving.clear();
            self.instance_data.clear();
            self.blas.clear();
            self.tlas = None;
        }
        self.scene_upload_pending = true;
        let static_changed = self.revision != Some(revision);
        let textures_changed = static_changed || self.texture_len != texels.byte_len();
        let input = STORAGE | BUILD_INPUT | ADDRESS;
        let ctx = &self.core.ctx;
        if static_changed {
            Upload::set(&mut self.world, ctx, world.bytes(), input)?;
        }
        if textures_changed {
            let prefix = if static_changed {
                texels.shared_bytes(&self.texture_prefix)
            } else {
                self.texture_len
            };
            Upload::set_segments(
                &mut self.textures,
                ctx,
                texels.byte_segments(),
                texels.byte_len(),
                STORAGE,
                prefix,
            )?;
        }
        let moving_changed = self.last_moving != moving.words;
        let moving_keys_changed = !self
            .last_moving_keys
            .iter()
            .copied()
            .eq(moving.meshes.iter().map(|m| m.key));
        if moving_changed {
            Upload::set_changed(
                &mut self.moving,
                ctx,
                moving.bytes(),
                input,
                bytemuck::cast_slice(&self.last_moving),
            )?;
        }
        let plan = if self.resident_dirty || static_changed || moving_changed || moving_keys_changed
        {
            Some(self.prepare_accel(world, moving)?)
        } else {
            None
        };
        let requested = frame.options[0] & HYBRID_PRIMARY != 0;
        let mut raster = None;
        if let Some(primary) = &mut self.primary {
            if requested && !self.primary_failed && self.primary_error_extent != Some(frame.extent)
            {
                match primary.prepare(
                    frame,
                    world,
                    moving,
                    revision,
                    moving_changed || moving_keys_changed,
                ) {
                    Ok(view) => raster = Some((&*primary, view)),
                    Err(error) => {
                        self.primary_error_extent = Some(frame.extent);
                        primary.inactive();
                        eprintln!("Lighter primary raster unavailable; using full rays: {error}");
                    }
                }
            } else {
                primary.inactive();
                if !requested {
                    self.primary_error_extent = None;
                }
            }
        }
        let ctx = &self.core.ctx;
        let d = &ctx.device;
        let cmd = self.core.commands;
        let output = self.output.as_ref().expect("prepare image before tracing");
        let image = output.imported.as_ref().expect("initialized interop image");
        assert!(output.raw.size >= image_size);
        let w = self.world.as_ref().unwrap();
        let m = self.moving.as_ref().unwrap();
        let textures = self.textures.as_ref().unwrap();
        let bc_matches =
            !self.bc_pages.is_empty() && self.bc_pages.iter().all(|p| p.matches(texels));
        let mut uniform = *frame;
        if bc_matches {
            uniform.options[0] |= bc_pages::ACTIVE;
        }
        self.frame.write(0, bytemuck::bytes_of(&uniform))?;
        let scratch_bytes = plan.as_ref().map_or(0, |p| p.scratch_bytes) + ctx.scratch_align;
        if self.scratch.as_ref().is_none_or(|s| s.size < scratch_bytes) {
            let previous = self.scratch.as_ref().map_or(0, |b| b.size);
            let capacity = grown_capacity(scratch_bytes, previous, u64::MAX);
            self.scratch = Some(Buffer::new(ctx, capacity, STORAGE | ADDRESS, LOCAL)?);
        }
        let scratch = self
            .scratch
            .as_ref()
            .unwrap()
            .address()
            .div_ceil(ctx.scratch_align)
            * ctx.scratch_align;
        let tlas = self.tlas.as_ref().unwrap();
        let inputs = [&self.frame, &w.gpu, &m.gpu, &textures.gpu, &output.raw];
        let bound = std::array::from_fn(|i| inputs.get(i).map_or(tlas.storage.id, |b| b.id));
        if self.bound_ids != Some(bound) {
            let infos = inputs.map(|b| vk::DescriptorBufferInfo {
                buffer: b.handle,
                offset: 0,
                range: b.size,
            });
            let mut acceleration = vk::WriteDescriptorSetAccelerationStructureKHR::builder()
                .acceleration_structures(std::slice::from_ref(&tlas.handle));
            let mut writes: [_; 6] = std::array::from_fn(|i| {
                if i == 5 {
                    vk::WriteDescriptorSet::default()
                } else {
                    vk::WriteDescriptorSet::builder()
                        .dst_set(self.core.set)
                        .dst_binding(i as u32)
                        .descriptor_type(if i == 0 {
                            vk::DescriptorType::UNIFORM_BUFFER
                        } else {
                            vk::DescriptorType::STORAGE_BUFFER
                        })
                        .buffer_info(std::slice::from_ref(&infos[i]))
                        .build()
                }
            });
            writes[5] = vk::WriteDescriptorSet::builder()
                .dst_set(self.core.set)
                .dst_binding(5)
                .descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR)
                .push_next(&mut acceleration)
                .build();
            writes[5].descriptor_count = 1;
            unsafe {
                d.update_descriptor_sets(&writes, &[]);
            }
            self.bound_ids = Some(bound);
        }
        if let Some((_, view)) = raster {
            let info = vk::DescriptorImageInfo::builder()
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .build();
            unsafe {
                d.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::builder()
                        .dst_set(self.core.set)
                        .dst_binding(6)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(std::slice::from_ref(&info))
                        .build()],
                    &[],
                );
            }
        }
        let timing = self.core.query != vk::QueryPool::null();
        let built = plan
            .as_ref()
            .is_some_and(|p| !p.blas.is_empty() || p.tlas.is_some());
        unsafe {
            self.core.begin_commands()?;
            if timing {
                d.cmd_reset_query_pool(cmd, self.core.query, 0, 4);
            }
            if static_changed {
                w.encode(cmd);
            }
            if textures_changed {
                textures.encode(cmd);
            }
            for atlas in &self.bc_pages {
                atlas.encode(cmd, bc_matches.then_some(textures.gpu.handle));
            }
            if moving_changed {
                m.encode(cmd);
            }
            if plan.as_ref().is_some_and(|p| p.tlas.is_some()) {
                self.instances.as_ref().unwrap().encode(cmd);
            }
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER
                    | vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR
                    | if raster.is_some() {
                        vk::PipelineStageFlags::VERTEX_SHADER
                            | vk::PipelineStageFlags::FRAGMENT_SHADER
                    } else {
                        vk::PipelineStageFlags::empty()
                    },
                vk::DependencyFlags::empty(),
                &[vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(
                        vk::AccessFlags::SHADER_READ
                            | vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR,
                    )
                    .build()],
                &[],
                &[],
            );
            let build_stage = vk::PipelineStageFlags::BOTTOM_OF_PIPE;
            if timing {
                d.cmd_write_timestamp(cmd, build_stage, self.core.query, 0);
            }
            if let Some(plan) = &plan {
                for (key, _, build) in &plan.blas {
                    build.encode(ctx, cmd, self.blas[key].accel.as_ref().unwrap(), scratch);
                }
                if let Some(build) = &plan.tlas {
                    build.encode(ctx, cmd, tlas, scratch);
                }
            }
            if timing {
                d.cmd_write_timestamp(cmd, build_stage, self.core.query, 1);
            }
            if let Some((primary, _)) = raster {
                primary.encode(cmd, self.core.layout, self.core.set, frame);
            }
            // The previous wgpu denoiser or compositor may still read this
            // buffer. Both execute on this VkQueue, in submission order.
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[vk::BufferMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(output.raw.handle)
                    .size(vk::WHOLE_SIZE)
                    .build()],
                &[],
            );
            if timing {
                d.cmd_write_timestamp(
                    cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    self.core.query,
                    2,
                );
            }
            let pipeline = raster.map_or(self.core.pipeline, |(primary, _)| primary.trace);
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.core.layout,
                0,
                &[self.core.set],
                &[],
            );
            d.cmd_dispatch(
                cmd,
                frame.extent[0].div_ceil(8),
                frame.extent[1].div_ceil(8),
                1,
            );
            if timing {
                d.cmd_write_timestamp(
                    cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    self.core.query,
                    3,
                );
            }
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .build()],
                &[],
                &[],
            );
        }
        self.core.submit()?;
        for atlas in &mut self.bc_pages {
            atlas.initialized = true;
            atlas.uploaded |= bc_matches;
            atlas.visible = bc_matches;
        }
        self.last_raster = raster.is_some();
        self.submitted = true;
        self.scene_upload_pending = false;
        self.resident_dirty = false;
        if timing {
            let moving_bytes = if moving_changed { m.copied_bytes() } else { 0 };
            let instance_bytes = if plan.as_ref().is_some_and(|p| p.tlas.is_some()) {
                self.instances.as_ref().unwrap().copied_bytes()
            } else {
                0
            };
            self.timing_pending = Some((
                frame.extent,
                built,
                raster.is_some(),
                moving_bytes,
                instance_bytes,
            ));
        }
        if moving_changed {
            self.last_moving.resize(moving.words.len(), 0);
            for copy in &m.copies {
                let at = copy.dst_offset as usize / 4;
                let end = at + copy.size as usize / 4;
                self.last_moving[at..end].copy_from_slice(&moving.words[at..end]);
            }
        }
        if moving_keys_changed {
            self.last_moving_keys.clear();
            self.last_moving_keys
                .extend(moving.meshes.iter().map(|m| m.key));
        }
        if let Some(plan) = plan {
            for (key, shape, build) in plan.blas {
                self.blas.get_mut(&key).unwrap().shape = Some(shape);
                let slot = usize::from(key.0 == DYNAMIC_BIT);
                if build.update {
                    self.as_refits[slot] += 1;
                } else {
                    self.as_builds[slot] += 1;
                }
            }
            if let Some(build) = plan.tlas {
                self.members = plan.members;
                self.instance_data = plan.instance_data;
                if build.update {
                    self.as_refits[2] += 1;
                } else {
                    self.as_builds[2] += 1;
                }
            }
        }
        self.revision = Some(revision);
        self.texture_len = texels.byte_len();
        if static_changed {
            texels.remember_prefix(&mut self.texture_prefix);
        }
        Ok(image)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Terminal teardown only, never on a mode switch or resize. There is
        // no next redraw to retire raw work; drain both native and subsequent
        // compositor reads before destroying the imported VkBuffer/allocation.
        if self.output.is_some() || self.resident_upload.is_some() {
            unsafe {
                let _ = self.core.ctx.device.queue_wait_idle(self.core.ctx.queue);
            }
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn dirty_uploads_copy_only_changed_pages_and_bound_fragmentation() {
        let mut copies = Vec::new();
        let before = vec![0u8; 1024 * 1024];
        let mut after = before.clone();
        for offset in [3 * DIRTY_PAGE + 4, 802 * DIRTY_PAGE + 80] {
            after[offset] = 1;
        }
        dirty_copies(&before, &after, &mut copies);
        assert_eq!(copies.len(), 2);
        assert_eq!(
            copies.iter().map(|c| c.size).sum::<u64>(),
            2 * DIRTY_PAGE as u64
        );
        let replay = |a: &mut Vec<u8>, b: &[u8], copies: &[vk::BufferCopy]| {
            a.resize(b.len(), 9);
            for c in copies {
                let at = c.dst_offset as usize;
                a[at..at + c.size as usize].copy_from_slice(&b[at..at + c.size as usize]);
            }
            assert_eq!(a, b);
        };
        replay(&mut before.clone(), &after, &copies);
        after.extend([255; 4]);
        for offset in (0..before.len()).step_by(4 * DIRTY_PAGE) {
            after[offset] = 2;
        }
        dirty_copies(&before, &after, &mut copies);
        assert_eq!(copies.len(), 1);
        replay(&mut before.clone(), &after, &copies);
        dirty_copies(&before, &before[..50004], &mut copies);
        assert!(copies.is_empty());
    }

    #[test]
    fn budget_releases_evictions_and_rejects_transient_overrun() {
        let budget = AllocationBudget {
            limit: 512,
            held: AtomicU64::new(0),
        };
        budget.reserve(384).unwrap();
        assert!(budget.reserve(256).is_err());
        assert_eq!(budget.held.load(Ordering::Relaxed), 384);
        budget.release(384);
        budget.reserve(512).unwrap();
        assert_eq!(budget.held.load(Ordering::Relaxed), 512);
    }
    #[test]
    fn queue_timestamps_wrap_at_valid_bits() {
        assert_eq!(timestamp_ms(250, 4, 8, 100_000.), 1.);
    }
}
