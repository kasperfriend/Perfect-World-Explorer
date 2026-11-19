//! Paced resident transfers and BLAS construction. The candidate's buffers
//! and newly built BLAS are never bound by a frame until publication. The
//! live TLAS remains active; compose the new membership from the latest
//! moving packet at the publication draw, not a stale animation snapshot.
use super::*;
use crate::modules::lighter::residency::{texel_capacity, Cursor, Source, Span, Stats, Target};

const BLAS_PER_BATCH: usize = 8;
const MESHES_PER_POLL: usize = 512;
const PRIMITIVES_PER_BATCH: usize = 32 * 1024;

/// A second command buffer on the same provisioned Vulkan queue. Fence
/// polling controls staging reuse; there is no queue wait or ownership
/// transfer on the interactive path. Only the render thread submits.
struct Lane {
    ctx: Arc<Context>,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    query: vk::QueryPool,
    sizes: vk::QueryPool,
    busy: bool,
}

impl Lane {
    fn new(ctx: &Arc<Context>, startup: bool, compact: bool) -> R<Self> {
        let mut lane = Self {
            ctx: ctx.clone(),
            pool: vk::CommandPool::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            query: vk::QueryPool::null(),
            sizes: vk::QueryPool::null(),
            busy: false,
        };
        let d = &ctx.device;
        unsafe {
            lane.pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder().queue_family_index(ctx.family),
                    None,
                )
                .map_err(vkerr("create residency command pool"))?;
            lane.cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(lane.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(vkerr("allocate residency commands"))?[0];
            lane.fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(vkerr("create residency fence"))?;
        }
        if startup && ctx.timestamp_bits != 0 {
            lane.query = unsafe {
                d.create_query_pool(
                    &vk::QueryPoolCreateInfo::builder()
                        .query_type(vk::QueryType::TIMESTAMP)
                        .query_count(2),
                    None,
                )
            }
            .unwrap_or_default();
        }
        if compact {
            lane.sizes = unsafe {
                d.create_query_pool(
                    &vk::QueryPoolCreateInfo::builder()
                        .query_type(vk::QueryType::ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR)
                        .query_count(BLAS_PER_BATCH as u32),
                    None,
                )
            }
            .unwrap_or_default();
        }
        Ok(lane)
    }

    fn poll(&mut self) -> R<bool> {
        if self.busy {
            self.busy = !unsafe { self.ctx.device.get_fence_status(self.fence) }
                .map_err(vkerr("poll residency fence"))?;
            if !self.busy && self.query != vk::QueryPool::null() {
                let mut ticks = [0u64; 2];
                let d = &self.ctx.device;
                if unsafe {
                    d.get_query_pool_results(
                        self.query,
                        0,
                        2,
                        &mut ticks,
                        vk::QueryResultFlags::TYPE_64,
                    )
                }
                .is_ok()
                {
                    let ms = timestamp_ms(
                        ticks[0],
                        ticks[1],
                        self.ctx.timestamp_bits,
                        self.ctx.timestamp_period,
                    );
                    eprintln!("Lighter startup GPU submit: {ms:.6} ms");
                }
            }
        }
        Ok(!self.busy)
    }

    fn begin(&self) -> R<vk::CommandBuffer> {
        assert!(!self.busy);
        let d = &self.ctx.device;
        unsafe {
            d.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .map_err(vkerr("reset residency commands"))?;
            d.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(vkerr("begin residency commands"))?;
            if self.query != vk::QueryPool::null() {
                d.cmd_reset_query_pool(self.cmd, self.query, 0, 2);
                d.cmd_write_timestamp(self.cmd, vk::PipelineStageFlags::TOP_OF_PIPE, self.query, 0);
            }
        }
        Ok(self.cmd)
    }

    fn submit(&mut self) -> R<()> {
        let d = &self.ctx.device;
        unsafe {
            if self.query != vk::QueryPool::null() {
                d.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    self.query,
                    1,
                );
            }
            d.end_command_buffer(self.cmd)
                .map_err(vkerr("end residency commands"))?;
            d.reset_fences(&[self.fence])
                .map_err(vkerr("reset residency fence"))?;
            d.queue_submit(
                self.ctx.queue,
                &[vk::SubmitInfo::builder()
                    .command_buffers(std::slice::from_ref(&self.cmd))
                    .build()],
                self.fence,
            )
            .map_err(vkerr("submit residency commands"))?;
        }
        self.busy = true;
        Ok(())
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        // Normal disposal is after a signaled fence (or before any submit).
        // Renderer teardown drains the device before any raw resources drop.
        unsafe {
            self.ctx.device.destroy_query_pool(self.query, None);
            self.ctx.device.destroy_query_pool(self.sizes, None);
            self.ctx.device.destroy_fence(self.fence, None);
            self.ctx.device.destroy_command_pool(self.pool, None);
        }
    }
}

pub(super) struct ResidentUpload {
    lane: Lane,
    staging: Buffer,
    world: Upload,
    texels: Upload,
    preceding: Option<Arc<Buffer>>,
    prefix: usize,
    cursor: Cursor,
    mesh_at: usize,
    blas: HashMap<ResidentKey, ResidentBlas>,
    scratch: Option<Buffer>,
    compaction_sizes: Vec<ResidentKey>,
    compaction_copies: Vec<Structure>,
    pub revision: u64,
    done: bool,
    cancelled: bool,
    stats: Stats,
    completed: Stats,
    startup: bool,
}

impl ResidentUpload {
    fn new(
        renderer: &Renderer,
        revision: u64,
        packet: &Packet,
        texels: &Textures,
        limit: usize,
        startup: bool,
    ) -> R<Self> {
        let ctx = &renderer.core.ctx;
        let prefix = if renderer.scene_upload_pending || renderer.revision.is_none() {
            0
        } else {
            texels.shared_bytes(&renderer.texture_prefix)
        };
        let preceding = if prefix == 0 {
            None
        } else {
            Some(renderer.textures.as_ref().unwrap().gpu.clone())
        };
        let bytes = packet.bytes().len();
        let texture_bytes = texels.byte_len();
        if (bytes.max(texture_bytes) as u64) > ctx.storage_limit {
            return Err("Lighter staged Vulkan scene exceeds binding limit".into());
        }
        Ok(Self {
            lane: Lane::new(ctx, startup, renderer.compact_blas)?,
            world: Upload::device_local(ctx, bytes as u64, STORAGE | BUILD_INPUT | ADDRESS)?,
            texels: Upload::device_local(
                ctx,
                (texel_capacity(texels) as u64).min(ctx.storage_limit),
                STORAGE,
            )?,
            staging: Buffer::new(
                ctx,
                (bytes + texture_bytes - prefix).min(limit) as u64,
                vk::BufferUsageFlags::TRANSFER_SRC,
                HOST,
            )?,
            preceding,
            prefix,
            cursor: Cursor::default(),
            mesh_at: 0,
            blas: HashMap::new(),
            scratch: None,
            compaction_sizes: Vec::new(),
            compaction_copies: Vec::new(),
            revision,
            done: false,
            cancelled: false,
            stats: Stats::default(),
            completed: Stats::default(),
            startup,
        })
    }

    fn copy(&mut self, spans: &[Span<'_>]) -> R<()> {
        let mut staging_offset = 0;
        self.staging
            .write_segments(spans.iter().filter_map(|span| {
                let Source::Host(bytes) = span.source else {
                    return None;
                };
                let offset = staging_offset;
                staging_offset += bytes.len();
                Some((offset, bytes))
            }))?;
        let cmd = self.lane.begin()?;
        let d = &self.lane.ctx.device;
        staging_offset = 0;
        unsafe {
            // Prefix writes and the copy may be in different submissions.
            // Both go through the same queue, but still need memory visibility.
            if spans
                .iter()
                .any(|span| matches!(span.source, Source::Resident(_)))
            {
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[vk::MemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .build()],
                    &[],
                    &[],
                );
            }
            for span in spans {
                let dst = match span.target {
                    Target::Packet => self.world.gpu.handle,
                    Target::Texels => self.texels.gpu.handle,
                };
                let (src, offset) = match span.source {
                    Source::Host(bytes) => {
                        let offset = staging_offset;
                        staging_offset += bytes.len();
                        (self.staging.handle, offset)
                    }
                    Source::Resident(_) => (self.preceding.as_ref().unwrap().handle, span.offset),
                };
                d.cmd_copy_buffer(
                    cmd,
                    src,
                    dst,
                    &[vk::BufferCopy {
                        src_offset: offset as u64,
                        dst_offset: span.offset as u64,
                        size: span.len() as u64,
                    }],
                );
            }
            // Preserve visibility for both later resident BLAS builds and
            // the first ray or hybrid draw after the candidate is installed.
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR
                    | vk::PipelineStageFlags::COMPUTE_SHADER
                    | if self.lane.ctx.raster_capable {
                        vk::PipelineStageFlags::VERTEX_SHADER
                            | vk::PipelineStageFlags::FRAGMENT_SHADER
                    } else {
                        vk::PipelineStageFlags::empty()
                    },
                vk::DependencyFlags::empty(),
                &[vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(
                        vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR
                            | vk::AccessFlags::SHADER_READ,
                    )
                    .build()],
                &[],
                &[],
            );
        }
        self.lane.submit()?;
        self.stats.copied(spans);
        Ok(())
    }

    fn build(&mut self, packet: &Packet, active: &HashMap<ResidentKey, ResidentBlas>) -> R<bool> {
        let ctx = &self.lane.ctx;
        let mut builds = Vec::new();
        let mut primitives = 0;
        let mut scratch_bytes = 0;
        let base = self.world.gpu.address();
        let end = (self.mesh_at + MESHES_PER_POLL).min(packet.meshes.len());
        while self.mesh_at < end {
            let mesh = &packet.meshes[self.mesh_at];
            let key = resident_key(0, mesh);
            if self
                .blas
                .get(&key)
                .or_else(|| active.get(&key))
                .is_some_and(|b| b.shape == Some(mesh.shape))
            {
                self.mesh_at += 1;
                continue;
            }
            let count = mesh.rt_triangles as usize;
            // A single indivisible BLAS may exceed the triangle target.
            if !builds.is_empty()
                && (self.startup
                    || builds.len() == BLAS_PER_BATCH
                    || primitives + count > PRIMITIVES_PER_BATCH)
            {
                break;
            }
            let mut build = Renderer::blas(ctx, packet, mesh, base, false)?;
            build.allow_compaction =
                self.lane.sizes != vk::QueryPool::null() && matches!(mesh.key, MeshKey::Static(..));
            let mut resident = ResidentBlas::default();
            scratch_bytes = scratch_bytes.max(build.prepare(ctx, &mut resident.accel)?);
            resident.shape = Some(mesh.shape);
            self.blas.insert(key, resident);
            builds.push((key, build));
            primitives += count;
            self.mesh_at += 1;
        }
        if builds.is_empty() {
            return Ok(self.mesh_at == packet.meshes.len());
        }
        let needed = scratch_bytes + ctx.scratch_align;
        if self.scratch.as_ref().is_none_or(|b| b.size < needed) {
            self.scratch = Some(Buffer::new(ctx, needed, STORAGE | ADDRESS, LOCAL)?);
        }
        let scratch = self
            .scratch
            .as_ref()
            .unwrap()
            .address()
            .div_ceil(ctx.scratch_align)
            * ctx.scratch_align;
        let cmd = self.lane.begin()?;
        for (key, build) in &builds {
            unsafe { build.encode(ctx, cmd, self.blas[key].accel.as_ref().unwrap(), scratch) }
        }
        if self.lane.sizes != vk::QueryPool::null() {
            let mut handles = Vec::new();
            for (key, build) in &builds {
                if build.allow_compaction {
                    handles.push(self.blas[key].accel.as_ref().unwrap().handle);
                    self.compaction_sizes.push(*key);
                }
            }
            if !handles.is_empty() {
                unsafe {
                    ctx.device
                        .cmd_reset_query_pool(cmd, self.lane.sizes, 0, handles.len() as u32);
                    ctx.acceleration
                        .cmd_write_acceleration_structures_properties(
                            cmd,
                            &handles,
                            vk::QueryType::ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR,
                            self.lane.sizes,
                            0,
                        );
                }
            }
        }
        self.lane.submit()?;
        self.stats.blas_batches += 1;
        self.stats.blas_builds += builds.len() as u64;
        Ok(false)
    }

    fn compact_built_batch(&mut self) -> R<bool> {
        self.compaction_copies.clear(); // previous copy fence has retired
        let keys = std::mem::take(&mut self.compaction_sizes);
        if keys.is_empty() {
            return Ok(false);
        }
        let ctx = &self.lane.ctx;
        let mut sizes = [0u64; BLAS_PER_BATCH];
        match unsafe {
            ctx.device.get_query_pool_results(
                self.lane.sizes,
                0,
                keys.len() as u32,
                &mut sizes[..keys.len()],
                vk::QueryResultFlags::TYPE_64,
            )
        } {
            Ok(()) => (),
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                return Err("Lighter Vulkan compaction query: ERROR_DEVICE_LOST".into())
            }
            Err(error) => {
                eprintln!("Lighter static compaction queries unavailable: {error:?}");
                return Ok(false);
            }
        }
        let mut replacements = Vec::new();
        for (key, size) in keys.into_iter().zip(sizes) {
            let old = self.blas[&key].accel.as_ref().unwrap();
            if size == 0 || size >= old.storage.size {
                continue;
            }
            match Structure::new(ctx, size, vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL) {
                Ok(new) => {
                    // An unchanged allocation size is not a memory win. The
                    // driver may round both buffer allocations to one page.
                    if new.storage.charged < old.storage.charged {
                        replacements.push((key, new, old.storage.charged));
                    }
                }
                Err(e) if e.contains("ERROR_DEVICE_LOST") => return Err(e),
                Err(_) => continue, // not enough transient headroom: keep the valid build
            }
        }
        if replacements.is_empty() {
            return Ok(false);
        }
        let cmd = self.lane.begin()?;
        unsafe {
            for (key, new, _) in &replacements {
                ctx.acceleration.cmd_copy_acceleration_structure(
                    cmd,
                    &vk::CopyAccelerationStructureInfoKHR::builder()
                        .src(self.blas[key].accel.as_ref().unwrap().handle)
                        .dst(new.handle)
                        .mode(vk::CopyAccelerationStructureModeKHR::COMPACT),
                );
            }
            let stage = vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR;
            ctx.device.cmd_pipeline_barrier(
                cmd,
                stage,
                stage,
                vk::DependencyFlags::empty(),
                &[vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR)
                    .dst_access_mask(vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR)
                    .build()],
                &[],
                &[],
            );
        }
        self.lane.submit()?;
        for (key, new, previous) in replacements {
            self.stats.blas_compacted += 1;
            self.stats.blas_saved_bytes += previous - new.storage.charged;
            self.compaction_copies
                .push(self.blas.get_mut(&key).unwrap().accel.replace(new).unwrap());
        }
        Ok(true)
    }

    fn poll(
        &mut self,
        packet: &Packet,
        texels: &Textures,
        active: &HashMap<ResidentKey, ResidentBlas>,
        limit: usize,
    ) -> R<bool> {
        if self.done {
            return Ok(true);
        }
        if !self.lane.poll()? {
            return Ok(false);
        }
        self.completed = self.stats;
        if self.compact_built_batch()? {
            return Ok(false);
        }
        let spans = self.cursor.next(packet.bytes(), texels, self.prefix, limit);
        if !spans.is_empty() {
            self.copy(&spans)?;
            return Ok(false);
        }
        self.preceding = None;
        self.done = self.build(packet, active)?;
        Ok(self.done)
    }
}

impl Renderer {
    pub(in super::super) fn poll_resident_upload(&mut self) -> R<bool> {
        if let Some(stream) = &mut self.resident_upload {
            if !stream.lane.poll()? {
                return Ok(false);
            }
            if stream.cancelled {
                self.resident_upload = None;
            }
        }
        Ok(true)
    }

    pub(in super::super) fn abandon_residency(&mut self) {
        if let Some(stream) = &mut self.resident_upload {
            if stream.lane.busy {
                stream.cancelled = true;
            } else {
                self.resident_upload = None;
            }
        }
    }

    pub(in super::super) fn stage_resident(
        &mut self,
        revision: u64,
        packet: &Packet,
        texels: &Textures,
        limit: usize,
    ) -> R<bool> {
        self.stage_resident_inner(revision, packet, texels, limit, false)
    }

    pub(super) fn finished_upload_stats(&self) -> Stats {
        self.resident_upload
            .as_ref()
            .map_or(Stats::default(), |u| u.completed)
    }

    pub(super) fn stage_resident_inner(
        &mut self,
        revision: u64,
        packet: &Packet,
        texels: &Textures,
        limit: usize,
        startup: bool,
    ) -> R<bool> {
        if self.resident_upload.is_none() {
            self.resident_upload = Some(ResidentUpload::new(
                self, revision, packet, texels, limit, startup,
            )?);
        }
        let stream = self.resident_upload.as_mut().unwrap();
        assert_eq!(
            stream.revision, revision,
            "one resident candidate at a time"
        );
        match stream.poll(packet, texels, &self.blas, limit) {
            Ok(true) => self.poll_frame(),
            Ok(false) => Ok(false),
            Err(e) => {
                // Errors occur before submitting a batch. A lost-device fence
                // can also error and is retired by device recovery.
                if !stream.lane.busy {
                    self.resident_upload = None;
                }
                Err(e)
            }
        }
    }

    pub(in super::super) fn publish_resident(
        &mut self,
        revision: u64,
        texels: &Textures,
    ) -> Option<Stats> {
        let stream = self.resident_upload.as_ref()?;
        if stream.revision != revision || !stream.done || stream.cancelled || self.submitted {
            return None;
        }
        let stream = self.resident_upload.take().unwrap();
        self.world = Some(stream.world);
        self.textures = Some(stream.texels);
        self.revision = Some(revision);
        self.texture_len = texels.byte_len();
        texels.remember_prefix(&mut self.texture_prefix);
        self.blas.extend(stream.blas);
        self.as_builds[0] += stream.stats.blas_builds;
        if stream.stats.blas_compacted > 0 {
            eprintln!(
                "Lighter static BLAS: compacted {} in resident revision {}, saved {} charged bytes",
                stream.stats.blas_compacted, revision, stream.stats.blas_saved_bytes
            );
        }
        self.resident_dirty = true;
        Some(stream.stats)
    }
}
