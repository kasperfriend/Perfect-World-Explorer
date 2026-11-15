//! Incremental whole-world BMD preload. Native-size texture atlases are shared
//! globally across geometry chunks, rather than duplicated at every page turn.
use crate::{
    gpu::Vertex,
    objects::Objects,
    scene::Ornament,
    scenery_visibility::{allows_with_context, Pass, RenderContext, Visibility},
};
use glam::Vec3;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    },
};
const SIDE: usize = 4096;
const MAX_VERTICES: usize = 1_000_000;

/// One ornament's index range inside a page. The two EC-BSD flags decide
/// whether this range is submitted in reflection/refraction; the main and map
/// passes always submit it. `start`/`end` are indices into `Page::indices`.
#[derive(Clone, Debug)]
pub struct PlacementRange {
    pub start: u32,
    pub end: u32,
    pub export_id: u32,
    /// Original model/mesh encounter order, independent of storage chunking.
    pub order: u64,
    pub visibility: Visibility,
    /// Conservative world-space mesh bounds; `None` keeps legacy or diagnostic
    /// ranges uncullable rather than risking a false rejection.
    pub bounds: Option<[f32; 6]>,
}

pub fn empty_bounds() -> [f32; 6] {
    [
        f32::INFINITY,
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ]
}

/// Extend `[min_x, min_y, min_z, max_x, max_y, max_z]`; return false for a
/// non-finite vertex so callers can conservatively mark the range uncullable.
pub fn extend_bounds(bounds: &mut [f32; 6], point: [f32; 3]) -> bool {
    if !point.iter().all(|value| value.is_finite()) {
        return false;
    }
    for axis in 0..3 {
        bounds[axis] = bounds[axis].min(point[axis]);
        bounds[axis + 3] = bounds[axis + 3].max(point[axis]);
    }
    true
}

/// The part of a page the command builder reads. Kept separate from `GpuPage`
/// so the fold below is testable without a device.
pub struct PageSpans<'a> {
    pub count: u32,
    pub placements: &'a [PlacementRange],
}

#[derive(Default)]
pub struct WorldCullIndex {
    nodes: Vec<CullNode>,
    items: Vec<SpatialItem>,
    always_visible: Vec<usize>,
    root: Option<usize>,
}

#[derive(Clone, Copy)]
struct CullNode {
    bounds: [f32; 6],
    left: usize,
    right: usize,
    first: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct SpatialItem {
    order_index: usize,
    bounds: [f32; 6],
}

impl WorldCullIndex {
    /// Build once, off the render path, from submission-ordered mesh bounds.
    /// Invalid or absent bounds remain conservative and are always included.
    pub fn from_ordered_bounds(bounds: &[Option<[f32; 6]>]) -> Self {
        let mut index = Self::default();
        for (order_index, bounds) in bounds.iter().copied().enumerate() {
            match bounds.filter(|bounds| valid_bounds(*bounds)) {
                Some(bounds) => index.items.push(SpatialItem {
                    order_index,
                    bounds,
                }),
                None => index.always_visible.push(order_index),
            }
        }
        if !index.items.is_empty() {
            let end = index.items.len();
            index.root = Some(build_cull_node(&mut index.nodes, &mut index.items, 0, end));
        }
        index
    }

    /// Return exact frustum-visible indices in original draw order. Internal
    /// nodes prune whole groups; leaf bounds retain the prior per-mesh test.
    pub fn visible_order_indices(&self, intersects: impl FnMut([f32; 6]) -> bool) -> Vec<usize> {
        let mut visible = Vec::with_capacity(self.always_visible.len());
        let mut stack = Vec::new();
        self.visible_order_indices_into(intersects, &mut visible, &mut stack);
        visible
    }

    /// Reuse caller-owned scratch storage for per-frame frustum queries.
    /// Both vectors retain capacity between views/frames, avoiding repeated
    /// heap allocation while preserving the same ordered visibility result.
    pub fn visible_order_indices_into(
        &self,
        mut intersects: impl FnMut([f32; 6]) -> bool,
        visible: &mut Vec<usize>,
        stack: &mut Vec<usize>,
    ) {
        visible.clear();
        visible.extend_from_slice(&self.always_visible);
        stack.clear();
        let Some(root) = self.root else {
            return;
        };
        stack.push(root);
        while let Some(node_index) = stack.pop() {
            let node = self.nodes[node_index];
            if !intersects(node.bounds) {
                continue;
            }
            if node.first < node.end {
                for item in &self.items[node.first..node.end] {
                    if intersects(item.bounds) {
                        visible.push(item.order_index);
                    }
                }
            } else {
                stack.push(node.left);
                stack.push(node.right);
            }
        }
        visible.sort_unstable();
    }
}

fn valid_bounds(bounds: [f32; 6]) -> bool {
    bounds.iter().all(|v| v.is_finite()) && (0..3).all(|axis| bounds[axis] <= bounds[axis + 3])
}

fn build_cull_node(
    nodes: &mut Vec<CullNode>,
    items: &mut [SpatialItem],
    first: usize,
    end: usize,
) -> usize {
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for item in &items[first..end] {
        for axis in 0..3 {
            bounds[axis] = bounds[axis].min(item.bounds[axis]);
            bounds[axis + 3] = bounds[axis + 3].max(item.bounds[axis + 3]);
        }
    }
    let index = nodes.len();
    nodes.push(CullNode {
        bounds,
        left: 0,
        right: 0,
        first: 0,
        end: 0,
    });
    if end - first <= 8 {
        nodes[index].first = first;
        nodes[index].end = end;
        return index;
    }
    let axis = (0..3)
        .max_by(|&a, &b| (bounds[a + 3] - bounds[a]).total_cmp(&(bounds[b + 3] - bounds[b])))
        .unwrap();
    items[first..end].sort_unstable_by(|a, b| {
        let ac = a.bounds[axis] + a.bounds[axis + 3];
        let bc = b.bounds[axis] + b.bounds[axis + 3];
        ac.total_cmp(&bc)
    });
    let middle = first + (end - first) / 2;
    let left = build_cull_node(nodes, items, first, middle);
    let right = build_cull_node(nodes, items, middle, end);
    nodes[index].left = left;
    nodes[index].right = right;
    index
}

/// Fold the frozen submission order into the minimum set of draws for `view`.
///
/// Placements inside a page are stored back to back, so two consecutive order
/// entries that belong to the same page usually *touch*
/// (`last.end == next.start`). Drawing them as one span draws exactly the same
/// indices in exactly the same order — but as one draw call instead of one per
/// placement, which is what the pass used to issue: a whole-world load is
/// thousands of placements over a few hundred pages, per view.
///
/// A span is only ever extended across a placement that touches it, so a
/// placement the view hides can never be swallowed by a neighbour: it is
/// skipped and leaves a gap the merge rule refuses to cross.
pub fn fold_commands(pages: &[PageSpans], order: &[(usize, usize)], view: Pass) -> Vec<DrawCmd> {
    fold_commands_visible(pages, order, view, |_| true)
}

/// Frustum-aware twin of `fold_commands`. Missing bounds are conservatively kept.
pub fn fold_commands_culled(
    pages: &[PageSpans],
    order: &[(usize, usize)],
    view: Pass,
    mut intersects: impl FnMut([f32; 6]) -> bool,
) -> Vec<DrawCmd> {
    fold_commands_visible(pages, order, view, |bounds| match bounds {
        Some(bounds) => intersects(bounds),
        None => true,
    })
}

/// Build the opaque and SDR-curtain lists together. Each SDR range corresponds
/// to the same placement slot as its opaque range, so one ordered walk and one
/// bounds test can safely generate both pass lists.
pub fn fold_commands_culled_pair(
    pages: &[PageSpans],
    sdr_pages: &[PageSpans],
    order: &[(usize, usize)],
    view: Pass,
    mut intersects: impl FnMut([f32; 6]) -> bool,
) -> (Vec<DrawCmd>, Vec<DrawCmd>) {
    let mut visible = Vec::with_capacity(order.len());
    for (order_index, &(page, range)) in order.iter().enumerate() {
        if range == usize::MAX {
            visible.push(order_index);
            continue;
        }
        let placement = &pages[page].placements[range];
        if allows_with_context(view, placement.visibility, RenderContext::default())
            && placement.bounds.map_or(true, |bounds| intersects(bounds))
        {
            visible.push(order_index);
        }
    }
    fold_commands_pair_indices(pages, sdr_pages, order, &visible, view)
}

/// Fold the exact visible subset returned by `WorldCullIndex` into the same
/// ordered draws as a linear scan, without repeating per-placement culling.
pub fn fold_commands_pair_indices(
    pages: &[PageSpans],
    sdr_pages: &[PageSpans],
    order: &[(usize, usize)],
    visible_order: &[usize],
    view: Pass,
) -> (Vec<DrawCmd>, Vec<DrawCmd>) {
    let mut opaque = Vec::with_capacity(visible_order.len());
    let mut sdr = Vec::with_capacity(visible_order.len());
    for &order_index in visible_order {
        let (page, range) = order[order_index];
        let p = &pages[page];
        let s = &sdr_pages[page];
        if range == usize::MAX {
            append_folded(&mut opaque, page, 0, p.count);
            append_folded(&mut sdr, page, 0, s.count);
            continue;
        }
        let placement = &p.placements[range];
        if !allows_with_context(view, placement.visibility, RenderContext::default()) {
            continue;
        }
        append_folded(&mut opaque, page, placement.start, placement.end);
        let curtain = &s.placements[range];
        append_folded(&mut sdr, page, curtain.start, curtain.end);
    }
    (opaque, sdr)
}

fn append_folded(commands: &mut Vec<DrawCmd>, page: usize, start: u32, end: u32) {
    if start == end {
        return;
    }
    if let Some(last) = commands.last_mut() {
        if last.page == page && last.end == start {
            last.end = end;
            return;
        }
    }
    commands.push(DrawCmd { page, start, end });
}

fn fold_commands_visible(
    pages: &[PageSpans],
    order: &[(usize, usize)],
    view: Pass,
    mut keep_bounds: impl FnMut(Option<[f32; 6]>) -> bool,
) -> Vec<DrawCmd> {
    let mut cmds: Vec<DrawCmd> = Vec::with_capacity(order.len());
    for &(page, range) in order {
        let p = &pages[page];
        let (start, end) = if range == usize::MAX {
            (0, p.count)
        } else {
            let r = &p.placements[range];
            if !allows_with_context(view, r.visibility, RenderContext::default())
                || !keep_bounds(r.bounds)
            {
                continue;
            }
            (r.start, r.end)
        };
        if start == end {
            continue;
        }
        if let Some(last) = cmds.last_mut() {
            if last.page == page && last.end == start {
                last.end = end;
                continue;
            }
        }
        cmds.push(DrawCmd { page, start, end });
    }
    cmds
}

/// Fold one page's placements into the spans a view draws, in order.
///
/// The builder stores placements back to back, so the visible ones usually
/// touch and collapse into a single span: the same indices in the same order,
/// one draw instead of one per placement. A hidden placement is skipped and its
/// indices stay out of the spans, because a span is only extended across a
/// neighbour that touches it.
fn fold_page_spans(placements: &[PlacementRange], view: Pass) -> Vec<(u32, u32)> {
    let mut spans: Vec<(u32, u32)> = Vec::with_capacity(placements.len());
    for r in placements {
        if !allows_with_context(view, r.visibility, RenderContext::default()) {
            continue;
        }
        if r.start == r.end {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.1 == r.start => last.1 = r.end,
            _ => spans.push((r.start, r.end)),
        }
    }
    spans
}

/// One opaque scenery draw: a page and a contiguous span of its index buffer.
///
/// Several `PlacementRange`s can fold into one command (see
/// `GpuPage::draw_commands`), which is what keeps the whole-world pass at one
/// draw per page run instead of one per placed object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DrawCmd {
    pub page: usize,
    pub start: u32,
    pub end: u32,
}

/// Command lists kept for the whole-world pages: main, reflection, refraction.
pub const WORLD_VIEWS: usize = 3;

pub struct Page {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub atlas: Vec<u8>,
    /// Earlier emitted page whose immutable atlas this geometry reuses.
    pub shared_atlas: Option<usize>,
    pub processed: usize,
    pub used_height: usize,
    pub mips: Vec<Vec<u8>>,
    pub placements: Vec<PlacementRange>,
}
impl Page {
    pub fn finish(mut self) -> Self {
        // Save unused rows in the last page. Keep all seven mip reductions valid.
        let height = self.used_height.max(crate::texture_atlas::UNIT).min(SIDE);
        let factor = SIDE as f32 / height as f32;
        for v in &mut self.vertices {
            v.uv[1] *= factor;
            v.color[1] *= factor;
            v.color[3] *= factor;
        }
        self.atlas.truncate(SIDE * height * 4);
        self.atlas.shrink_to_fit();
        self.used_height = height;
        if self.shared_atlas.is_some() {
            assert!(self.atlas.is_empty() && self.mips.is_empty());
            return self;
        }
        // Mip generation is done by the background scene worker, not the UI thread.
        let (mut w, mut h) = (SIDE, height);
        for level in 1..8 {
            let source = if level == 1 {
                &self.atlas
            } else {
                &self.mips[level - 2]
            };
            let (nw, nh) = (w / 2, h / 2);
            let mut out = vec![0u8; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    let a = (2 * y * w + 2 * x) * 4;
                    let b = a + w * 4;
                    let dst = (y * nw + x) * 4;
                    for c in 0..4 {
                        out[dst + c] = ((source[a + c] as u32
                            + source[a + 4 + c] as u32
                            + source[b + c] as u32
                            + source[b + 4 + c] as u32
                            + 2)
                            / 4) as u8;
                    }
                }
            }
            self.mips.push(out);
            w = nw;
            h = nh;
        }
        self
    }
}
/// Measured residency and resolution outcome of one whole-world preload
/// (P3.08: measured numbers, not estimates).
#[derive(Clone, Debug)]
pub struct DoneStats {
    pub residency: crate::objects::ResidencyStats,
    pub resolver: crate::texture_resolution::ResolverStats,
    /// Register rows recorded for references that resolved nowhere or were
    /// rejected by name (.sdr. shader registry, decode failures, missing).
    pub unresolved_rows: usize,
    /// Distinct BMD assets that could not be opened or parsed. Placements using
    /// these assets do not produce geometry and must remain visible in the UI.
    pub unavailable_models: Vec<String>,
    /// A few direct lookup/parse causes, so an all-empty world can be diagnosed
    /// without relying on the process log.
    pub unavailable_examples: Vec<String>,
    /// Pages emitted by this preload.
    pub pages: usize,
    pub atlas_pages: usize,
    pub atlas_bytes: usize,
}
pub enum Message {
    Page(Page),
    Done {
        processed: usize,
        skipped: usize,
        stats: DoneStats,
        world_order: Vec<(usize, usize)>,
        cull_index: WorldCullIndex,
    },
    Error(String),
}
fn basis(a: [f32; 3]) -> Vec3 {
    let v = Vec3::from_array(a);
    if v.length_squared() < 1e-8 {
        Vec3::Y
    } else {
        v.normalize()
    }
}
struct Builder {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    atlas: Vec<u8>,
    shared_atlas: Option<usize>,
    slots: HashMap<String, crate::texture_atlas::Slot>,
    packing: crate::texture_atlas::Atlas,
    used_height: usize,
    placements: Vec<PlacementRange>,
}
#[derive(Clone, Copy)]
struct OrderedPlacement {
    order: u64,
    page: usize,
    range: usize,
    bounds: Option<[f32; 6]>,
}

fn send_page(
    tx: &SyncSender<Message>,
    page_index: usize,
    page: Page,
    placements: &mut Vec<OrderedPlacement>,
) -> Result<(), ()> {
    if page.placements.is_empty() {
        placements.push(OrderedPlacement {
            order: u64::MAX,
            page: page_index,
            range: usize::MAX,
            bounds: None,
        });
    } else {
        placements.extend(
            page.placements
                .iter()
                .enumerate()
                .map(|(range, placement)| OrderedPlacement {
                    order: placement.order,
                    page: page_index,
                    range,
                    bounds: placement.bounds,
                }),
        );
    }
    tx.send(Message::Page(page)).map_err(|_| ())
}

impl Builder {
    fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            atlas: vec![0; SIDE * SIDE * 4],
            shared_atlas: None,
            slots: HashMap::new(),
            packing: crate::texture_atlas::Atlas::new(),
            used_height: 0,
            placements: Vec::new(),
        }
    }
    fn shared(source: usize, height: usize) -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            atlas: Vec::new(),
            shared_atlas: Some(source),
            slots: HashMap::new(),
            packing: crate::texture_atlas::Atlas::new(),
            used_height: height,
            placements: Vec::new(),
        }
    }
    fn page(self, processed: usize) -> Page {
        Page {
            vertices: self.vertices,
            indices: self.indices,
            atlas: self.atlas,
            shared_atlas: self.shared_atlas,
            processed,
            used_height: self.used_height,
            mips: Vec::new(),
            placements: self.placements,
        }
        .finish()
    }
}
#[derive(Clone, Copy)]
struct SharedSlot {
    source: usize,
    height: usize,
    slot: crate::texture_atlas::Slot,
}
fn remember_atlas(page: &Builder, source: usize, slots: &mut HashMap<String, SharedSlot>) {
    assert!(page.shared_atlas.is_none());
    let height = page.used_height.clamp(crate::texture_atlas::UNIT, SIDE);
    for (key, &slot) in &page.slots {
        // Atlas content, slot coordinates and mip chain are immutable after
        // emission. Later chunks reuse them without resampling/re-encoding.
        slots.insert(
            key.clone(),
            SharedSlot {
                source,
                height,
                slot,
            },
        );
    }
}
fn atlas_mip_bytes(height: usize) -> usize {
    let height = height.clamp(crate::texture_atlas::UNIT, SIDE);
    (0..8)
        .map(|level| (SIDE >> level) * (height >> level) * 4)
        .sum()
}
/// Allocates (or reuses) the atlas slot for one texture inside a page and
/// blits native-resolution pixels. The slot key is the texture's CONTENT
/// key (Objects::texture_key), not the reference name: distinct names with
/// identical pixels share one slot, which measurably reduces slot count and
/// page turns. When the page is full and already holds geometry, a fresh
/// page replaces it in place and the finished page is returned for sending.
fn allocate_slot(
    page: &mut Builder,
    key: &str,
    tex: &crate::objects::Texture,
) -> Result<(crate::texture_atlas::Slot, Option<Builder>), String> {
    if let Some(&s) = page.slots.get(key) {
        return Ok((s, None));
    }
    let mut slot = page.packing.allocate(tex.width, tex.height, 0, false);
    let mut turned = None;
    if slot.is_none() && !page.indices.is_empty() {
        turned = Some(std::mem::replace(page, Builder::new()));
        slot = page.packing.allocate(tex.width, tex.height, 0, false);
    }
    let s = slot.ok_or("scenery texture exceeds atlas capacity")?;
    for py in 0..s.h {
        for px in 0..s.w {
            let sample = tex.sample_rgba(
                (px as f32 + 0.5) / s.w as f32,
                (py as f32 + 0.5) / s.h as f32,
            );
            let dst = ((s.y + py) * SIDE + s.x + px) * 4;
            page.atlas[dst..dst + 4].copy_from_slice(&sample);
        }
    }
    page.used_height = page.used_height.max(s.y + s.h);
    page.slots.insert(key.to_string(), s);
    Ok((s, turned))
}
pub fn build(
    element: PathBuf,
    ornaments: Vec<Ornament>,
    tx: SyncSender<Message>,
    cancel: Arc<AtomicBool>,
) {
    let result = run(element, ornaments, &tx, &cancel);
    if let Err(e) = result {
        let _ = tx.send(Message::Error(e));
    }
}
fn run(
    element: PathBuf,
    ornaments: Vec<Ornament>,
    tx: &SyncSender<Message>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut ornaments = ornaments;
    // Group related models to reuse textures within a page. The supplied client
    // fell from 101 to 66 native-resolution pages in the full-world survey.
    if std::env::var_os("PW_WORLD_KEEP_ORDER_TEST").is_none() {
        ornaments.sort_by(|a, b| a.path.cmp(&b.path));
    }
    let mut objects = Objects::open(&element, ornaments)?;
    let first = std::env::var("PW_WORLD_TEST_START")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(objects.ornaments.len());
    let count = std::env::var("PW_WORLD_TEST_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(objects.ornaments.len());
    let limit = first.saturating_add(count).min(objects.ornaments.len());
    let mut page = Builder::new();
    let mut shared_slots = HashMap::new();
    let mut shared_geometry = HashMap::<usize, Builder>::new();
    let mut skipped = 0usize;
    let mut pages = 0usize;
    let mut ordered_placements = Vec::new();
    let mut atlas_pages = 0usize;
    let mut atlas_bytes = 0usize;
    let fallback = crate::objects::Texture {
        width: 1,
        height: 1,
        rgba: vec![160, 160, 160, 255],
    };
    for start in (first..limit).step_by(32) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let end = (start + 32).min(limit);
        let ids: Vec<_> = (start..end).collect();
        objects.load_ids(&ids)?;
        for id in ids {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let Some(model) = objects.get(id) else {
                continue;
            };
            // EC-BSD record +36 low bytes, recovered at 0x489f0b/0x489f12.
            // Keep the flags with every index range so a hidden placement is
            // never submitted just because its page is resident.
            let visibility = Visibility::from_extra(objects.ornaments[id].extra);
            let z = basis(model.direction);
            let y = basis(model.up);
            let x = y.cross(z).normalize_or_zero();
            for (mesh_index, mesh) in model.meshes.iter().enumerate() {
                // Stage, culling and alpha state come from the single recovered
                // table, not from per-asset guesses.
                let mut kind = match crate::bmd_baked::classify(
                    mesh.version,
                    !mesh.texture.trim().is_empty(),
                    mesh.material.as_ref().is_some_and(|m| m.two_sided),
                ) {
                    Ok(state) => state.kind,
                    Err(e) => {
                        // No silent skip: an unrepresentable mesh is an explicit
                        // error for the whole-world builder, not a dropped
                        // triangle. The BMD parser already rejects unsupported
                        // versions by name before this point.
                        return Err(format!("unsupported baked mesh {}: {e}", mesh.name));
                    }
                };
                if crate::objects::SdrUvAnim::from_texture_path(&mesh.texture)
                    .is_some_and(|a| a.tc_scroll[1] < 0.0)
                {
                    kind += 0.02;
                }
                // Content-keyed slot sharing (P3.08 cross-name dedup):
                // references with identical pixels collapse onto one atlas
                // slot regardless of how many names point at them.
                let key = match objects.texture_key(&mesh.texture) {
                    Some(hash) => format!("h{hash:016x}"),
                    None => String::from("<missing-client-texture>"),
                };
                let tex = if let Some(t) = objects.texture(&mesh.texture) {
                    t
                } else {
                    skipped += 1;
                    &fallback
                };
                if page.vertices.len() + mesh.vertices.len() > MAX_VERTICES
                    && !page.indices.is_empty()
                {
                    let finished = std::mem::replace(&mut page, Builder::new());
                    atlas_pages += 1;
                    atlas_bytes += atlas_mip_bytes(finished.used_height);
                    remember_atlas(&finished, pages, &mut shared_slots);
                    pages += 1;
                    if send_page(&tx, pages - 1, finished.page(id), &mut ordered_placements)
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                let (page, slot) = if let Some(shared) = shared_slots.get(&key).copied() {
                    let target = shared_geometry
                        .entry(shared.source)
                        .or_insert_with(|| Builder::shared(shared.source, shared.height));
                    if target.vertices.len() + mesh.vertices.len() > MAX_VERTICES
                        && !target.indices.is_empty()
                    {
                        let ready = std::mem::replace(
                            target,
                            Builder::shared(shared.source, shared.height),
                        );
                        pages += 1;
                        if send_page(&tx, pages - 1, ready.page(id), &mut ordered_placements)
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    (target, shared.slot)
                } else {
                    let (slot, turned) = allocate_slot(&mut page, &key, tex)?;
                    if let Some(finished) = turned {
                        atlas_pages += 1;
                        atlas_bytes += atlas_mip_bytes(finished.used_height);
                        remember_atlas(&finished, pages, &mut shared_slots);
                        pages += 1;
                        if send_page(&tx, pages - 1, finished.page(id), &mut ordered_placements)
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    (&mut page, slot)
                };
                let base = page.vertices.len() as u32;
                let range_start = page.indices.len() as u32;
                let ox = slot.x as f32 / SIDE as f32;
                let oy = slot.y as f32 / SIDE as f32;
                let sx = slot.w as f32 / SIDE as f32;
                let sy = slot.h as f32 / SIDE as f32;
                let mut bounds = empty_bounds();
                let mut bounds_valid = !mesh.vertices.is_empty();
                for v in &mesh.vertices {
                    let pos = Vec3::from_array(model.position)
                        + x * (v.position[0] * model.scale[0])
                        + y * (v.position[1] * model.scale[1])
                        + z * (v.position[2] * model.scale[2]);
                    bounds_valid &= extend_bounds(&mut bounds, pos.to_array());
                    let normal =
                        (x * v.normal[0] + y * v.normal[1] + z * v.normal[2]).normalize_or_zero();
                    page.vertices.push(Vertex {
                        pos: pos.to_array(),
                        normal: normal.to_array(),
                        uv: [ox + v.uv[0] * sx, oy + v.uv[1] * sy],
                        color: [ox, oy, sx, sy],
                        kind,
                        diffuse: v.day_diffuse,
                        night_diffuse: v.night_diffuse,
                    });
                }
                page.indices
                    .extend(mesh.indices.iter().map(|&n| base + n as u32));
                let range_end = page.indices.len() as u32;
                if range_end > range_start {
                    page.placements.push(PlacementRange {
                        start: range_start,
                        end: range_end,
                        export_id: objects.ornaments[id].export_id,
                        order: ((id as u64) << 32) | mesh_index as u64,
                        visibility,
                        bounds: bounds_valid.then_some(bounds),
                    });
                }
            }
        }
        // A batch holds at most 32 placements' reused-atlas geometry; unlike
        // keeping every atlas builder alive this never retains world pixels.
        let mut ready: Vec<_> = shared_geometry.drain().collect();
        ready.sort_by_key(|(source, _)| *source);
        for (_, builder) in ready {
            if !builder.indices.is_empty() {
                pages += 1;
                if send_page(&tx, pages - 1, builder.page(end), &mut ordered_placements).is_err() {
                    return Ok(());
                }
            }
        }
    }
    if !page.indices.is_empty() {
        atlas_pages += 1;
        atlas_bytes += atlas_mip_bytes(page.used_height);
        pages += 1;
        if send_page(&tx, pages - 1, page.page(limit), &mut ordered_placements).is_err() {
            return Ok(());
        }
    }
    ordered_placements
        .sort_unstable_by_key(|placement| (placement.order, placement.page, placement.range));
    let world_order: Vec<_> = ordered_placements
        .iter()
        .map(|placement| (placement.page, placement.range))
        .collect();
    let ordered_bounds: Vec<_> = ordered_placements
        .iter()
        .map(|placement| placement.bounds)
        .collect();
    let cull_index = WorldCullIndex::from_ordered_bounds(&ordered_bounds);
    let _ = tx.send(Message::Done {
        processed: limit,
        skipped,
        stats: DoneStats {
            residency: objects.residency(),
            resolver: objects.resolver_stats(),
            unresolved_rows: objects.unresolved().len(),
            unavailable_models: objects.unavailable_models(),
            unavailable_examples: objects.unavailable_examples().to_vec(),
            pages,
            atlas_pages,
            atlas_bytes,
        },
        world_order,
        cull_index,
    });
    Ok(())
}
fn sdr_band(kind: f32) -> bool {
    (kind > 2.01 && kind < 2.03) || (kind > 2.11 && kind < 2.13)
}

pub(crate) fn filter_sdr_indices(
    vertices: &[Vertex],
    indices: &[u32],
    range: std::ops::Range<u32>,
) -> Vec<u32> {
    let mut out = Vec::new();
    append_sdr_triangles(vertices, indices, range, &mut out);
    out
}

fn append_sdr_triangles(
    vertices: &[Vertex],
    indices: &[u32],
    range: std::ops::Range<u32>,
    out: &mut Vec<u32>,
) {
    for triangle in indices[range.start as usize..range.end as usize].chunks_exact(3) {
        // The vertex shader passes `kind` flat. Baked meshes use one kind for
        // every vertex, but include a mixed triangle conservatively so this
        // CPU filter can never hide a fragment the SDR shader would keep.
        let contains_sdr = triangle.iter().any(|&index| {
            vertices
                .get(index as usize)
                .map_or(true, |vertex| sdr_band(vertex.kind))
        });
        if contains_sdr {
            out.extend_from_slice(triangle);
        }
    }
}

pub struct GpuPage {
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    count: u32,
    sdr_index: wgpu::Buffer,
    sdr_count: u32,
    sdr_placements: Vec<PlacementRange>,
    normal: Arc<wgpu::BindGroup>,
    reflected: Arc<wgpu::BindGroup>,
    placements: Vec<PlacementRange>,
}
impl GpuPage {
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        page: Page,
    ) -> Self {
        assert!(
            page.shared_atlas.is_none(),
            "shared atlas requires upload_world"
        );
        let image = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("whole-world object atlas"),
            size: wgpu::Extent3d {
                width: SIDE as u32,
                height: page.used_height as u32,
                depth_or_array_layers: 1,
            },
            // Tiles start on 128-pixel boundaries; seven reductions preserve
            // those boundaries and avoid cross-material mip contamination.
            mip_level_count: 8,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Match the D3D8 encoded-color sampling used by the local atlas.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut width = SIDE;
        let mut height = page.used_height;
        for mip in 0..8u32 {
            let level = if mip == 0 {
                &page.atlas
            } else {
                &page.mips[(mip - 1) as usize]
            };
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &image,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                level,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some((width * 4) as u32),
                    rows_per_image: Some(height as u32),
                },
                wgpu::Extent3d {
                    width: width as u32,
                    height: height as u32,
                    depth_or_array_layers: 1,
                },
            );
            width /= 2;
            height /= 2;
        }
        let view = image.create_view(&Default::default());
        let make = |buf: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("whole-world object material"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let normal = Arc::new(make(normal));
        let reflected = Arc::new(make(reflected));
        Self::geometry(device, page, normal, reflected)
    }
    /// Upload only geometry for a reused atlas. Bind groups keep the exact
    /// original texture/view alive; no second allocation or texture upload.
    pub fn upload_world(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        normal: &wgpu::Buffer,
        reflected: &wgpu::Buffer,
        existing: &[Self],
        page: Page,
    ) -> Self {
        if let Some(source) = page.shared_atlas {
            let source = existing
                .get(source)
                .expect("shared atlas must precede its geometry");
            Self::geometry(
                device,
                page,
                Arc::clone(&source.normal),
                Arc::clone(&source.reflected),
            )
        } else {
            Self::upload(device, queue, layout, sampler, normal, reflected, page)
        }
    }
    fn geometry(
        device: &wgpu::Device,
        page: Page,
        normal: Arc<wgpu::BindGroup>,
        reflected: Arc<wgpu::BindGroup>,
    ) -> Self {
        use wgpu::util::DeviceExt;
        let vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world object vertices"),
            contents: bytemuck::cast_slice(&page.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world object indices"),
            contents: bytemuck::cast_slice(&page.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let mut sdr_indices = Vec::new();
        let mut sdr_placements = Vec::with_capacity(page.placements.len());
        if page.placements.is_empty() {
            append_sdr_triangles(
                &page.vertices,
                &page.indices,
                0..page.indices.len() as u32,
                &mut sdr_indices,
            );
        } else {
            for placement in &page.placements {
                let start = sdr_indices.len() as u32;
                append_sdr_triangles(
                    &page.vertices,
                    &page.indices,
                    placement.start..placement.end,
                    &mut sdr_indices,
                );
                let mut filtered = placement.clone();
                filtered.start = start;
                filtered.end = sdr_indices.len() as u32;
                sdr_placements.push(filtered);
            }
        }
        let sdr_count = sdr_indices.len() as u32;
        let sdr_contents = if sdr_indices.is_empty() {
            &[0u32][..]
        } else {
            &sdr_indices
        };
        let sdr_index = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("whole-world SDR curtain indices"),
            contents: bytemuck::cast_slice(sdr_contents),
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            vertex,
            index,
            count: page.indices.len() as u32,
            sdr_index,
            sdr_count,
            sdr_placements,
            normal,
            reflected,
            placements: page.placements,
        }
    }
    /// Submits only the placements the recovered EC-BSD flags admit in this
    /// view. A page built without per-placement ranges is a diagnostic page:
    /// draw it in full, but say so instead of silently dropping it.
    pub fn draw_selected<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
    ) {
        pass.set_bind_group(
            0,
            if reflected {
                &self.reflected
            } else {
                &self.normal
            },
            &[],
        );
        pass.set_vertex_buffer(0, self.vertex.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        if self.placements.is_empty() {
            eprintln!("scenery page has no placement visibility ranges; drawing all indices");
            pass.draw_indexed(0..self.count, 0, 0..1);
            return;
        }
        for (start, end) in fold_page_spans(&self.placements, view) {
            pass.draw_indexed(start..end, 0, 0..1);
        }
    }
    pub fn has_sdr(&self) -> bool {
        self.sdr_count != 0
    }
    /// Draw only `.sdr.` curtain triangles. Their relative placement/mesh order
    /// is unchanged; non-curtain indices are excluded rather than sent through
    /// a second vertex/fragment pass just to be discarded by `fs_sdr`.
    pub fn draw_selected_sdr<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
    ) {
        if self.sdr_count == 0 {
            return;
        }
        pass.set_bind_group(
            0,
            if reflected {
                &self.reflected
            } else {
                &self.normal
            },
            &[],
        );
        pass.set_vertex_buffer(0, self.vertex.slice(..));
        pass.set_index_buffer(self.sdr_index.slice(..), wgpu::IndexFormat::Uint32);
        if self.placements.is_empty() {
            pass.draw_indexed(0..self.sdr_count, 0, 0..1);
            return;
        }
        for (start, end) in fold_page_spans(&self.sdr_placements, view) {
            pass.draw_indexed(start..end, 0, 0..1);
        }
    }
    /// Freeze the original submission sequence once loading is complete.
    /// Texture sharing changes storage, never coplanar/depth-tie winners.
    pub fn draw_order(pages: &[Self]) -> Vec<(usize, usize)> {
        let mut order = Vec::new();
        for (page, p) in pages.iter().enumerate() {
            if p.placements.is_empty() {
                order.push((u64::MAX, page, usize::MAX));
            } else {
                for (range, r) in p.placements.iter().enumerate() {
                    order.push((r.order, page, range));
                }
            }
        }
        order.sort_unstable();
        order
            .into_iter()
            .map(|(_, page, range)| (page, range))
            .collect()
    }
    /// Commands for one view, built from the live pages.
    pub fn draw_commands(pages: &[Self], order: &[(usize, usize)], view: Pass) -> Vec<DrawCmd> {
        let spans: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.count,
                placements: &p.placements,
            })
            .collect();
        fold_commands(&spans, order, view)
    }
    /// Curtain-only command list, built once alongside the opaque page lists.
    pub fn sdr_draw_commands(pages: &[Self], order: &[(usize, usize)], view: Pass) -> Vec<DrawCmd> {
        let spans: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.sdr_count,
                placements: &p.sdr_placements,
            })
            .collect();
        fold_commands(&spans, order, view)
    }
    /// Build per-view whole-world commands from conservative mesh bounds.
    pub fn draw_commands_culled(
        pages: &[Self],
        order: &[(usize, usize)],
        view: Pass,
        intersects: impl FnMut([f32; 6]) -> bool,
    ) -> Vec<DrawCmd> {
        let spans: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.count,
                placements: &p.placements,
            })
            .collect();
        fold_commands_culled(&spans, order, view, intersects)
    }
    pub fn sdr_draw_commands_culled(
        pages: &[Self],
        order: &[(usize, usize)],
        view: Pass,
        intersects: impl FnMut([f32; 6]) -> bool,
    ) -> Vec<DrawCmd> {
        let spans: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.sdr_count,
                placements: &p.sdr_placements,
            })
            .collect();
        fold_commands_culled(&spans, order, view, intersects)
    }
    pub fn draw_commands_pair_visible(
        pages: &[Self],
        order: &[(usize, usize)],
        visible_order: &[usize],
        view: Pass,
    ) -> (Vec<DrawCmd>, Vec<DrawCmd>) {
        // Walk GpuPage data directly: the previous implementation built two
        // temporary PageSpans vectors for every camera view just to borrow the
        // same placement arrays.
        let mut opaque = Vec::with_capacity(visible_order.len());
        let mut sdr = Vec::with_capacity(visible_order.len());
        for &order_index in visible_order {
            let (page, range) = order[order_index];
            let p = &pages[page];
            if range == usize::MAX {
                append_folded(&mut opaque, page, 0, p.count);
                append_folded(&mut sdr, page, 0, p.sdr_count);
                continue;
            }
            let placement = &p.placements[range];
            if !allows_with_context(view, placement.visibility, RenderContext::default()) {
                continue;
            }
            append_folded(&mut opaque, page, placement.start, placement.end);
            let curtain = &p.sdr_placements[range];
            append_folded(&mut sdr, page, curtain.start, curtain.end);
        }
        (opaque, sdr)
    }

    pub fn draw_commands_pair_culled(
        pages: &[Self],
        order: &[(usize, usize)],
        view: Pass,
        intersects: impl FnMut([f32; 6]) -> bool,
    ) -> (Vec<DrawCmd>, Vec<DrawCmd>) {
        let opaque: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.count,
                placements: &p.placements,
            })
            .collect();
        let sdr: Vec<PageSpans> = pages
            .iter()
            .map(|p| PageSpans {
                count: p.sdr_count,
                placements: &p.sdr_placements,
            })
            .collect();
        fold_commands_culled_pair(&opaque, &sdr, order, view, intersects)
    }
    /// The command lists the viewer keeps for the whole-world pages: the main
    /// (unfiltered) list, then the reflection and refraction gates.
    pub fn world_commands(pages: &[Self]) -> [Vec<DrawCmd>; WORLD_VIEWS] {
        let order = Self::draw_order(pages);
        [
            Self::draw_commands(pages, &order, Pass::Main),
            Self::draw_commands(pages, &order, Pass::Reflection),
            Self::draw_commands(pages, &order, Pass::Refraction),
        ]
    }
    pub fn world_sdr_commands(pages: &[Self]) -> [Vec<DrawCmd>; WORLD_VIEWS] {
        let order = Self::draw_order(pages);
        [
            Self::sdr_draw_commands(pages, &order, Pass::Main),
            Self::sdr_draw_commands(pages, &order, Pass::Reflection),
            Self::sdr_draw_commands(pages, &order, Pass::Refraction),
        ]
    }
    /// Which `world_commands` list a pass reads. Fog, minimap and full-map see
    /// exactly what the main view sees, so they share its list.
    pub fn world_view_slot(pass: Pass) -> usize {
        match pass {
            Pass::Reflection => 1,
            Pass::Refraction => 2,
            Pass::Main | Pass::Fog | Pass::Minimap | Pass::FullMap => 0,
        }
    }
    /// Issue a command list: bind on a page change, one draw per command.
    pub fn draw_world<'a>(
        pages: &'a [Self],
        cmds: &[DrawCmd],
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
    ) {
        let mut bound = None;
        for cmd in cmds {
            let p = &pages[cmd.page];
            if bound != Some(cmd.page) {
                pass.set_bind_group(0, if reflected { &p.reflected } else { &p.normal }, &[]);
                pass.set_vertex_buffer(0, p.vertex.slice(..));
                pass.set_index_buffer(p.index.slice(..), wgpu::IndexFormat::Uint32);
                bound = Some(cmd.page);
            }
            pass.draw_indexed(cmd.start..cmd.end, 0, 0..1);
        }
    }
    pub fn draw_world_sdr<'a>(
        pages: &'a [Self],
        cmds: &[DrawCmd],
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
    ) {
        let mut bound = None;
        for cmd in cmds {
            let p = &pages[cmd.page];
            if bound != Some(cmd.page) {
                pass.set_bind_group(0, if reflected { &p.reflected } else { &p.normal }, &[]);
                pass.set_vertex_buffer(0, p.vertex.slice(..));
                pass.set_index_buffer(p.sdr_index.slice(..), wgpu::IndexFormat::Uint32);
                bound = Some(cmd.page);
            }
            pass.draw_indexed(cmd.start..cmd.end, 0, 0..1);
        }
    }
    /// Convenience wrapper for callers that hold the raw order (the atlas
    /// check): build the view's commands, then draw them.
    pub fn draw_ordered<'a>(
        pages: &'a [Self],
        order: &[(usize, usize)],
        pass: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        view: Pass,
    ) {
        let cmds = Self::draw_commands(pages, order, view);
        Self::draw_world(pages, &cmds, pass, reflected);
    }
    pub fn shares_atlas_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.normal, &other.normal) && Arc::ptr_eq(&self.reflected, &other.reflected)
    }
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, reflected: bool) {
        self.draw_selected(pass, reflected, Pass::Main);
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;

    #[test]
    fn sdr_index_filter_keeps_only_curtain_triangles_in_order() {
        let kinds = [
            2.0, 2.0, 2.0, // ordinary opaque triangle
            2.02, 2.02, 2.02, // water curtain
            2.12, 2.12, 2.12, // lava curtain
            2.0, 2.02, 2.0, // mixed flat-kind input: conservatively retained
        ];
        let vertices: Vec<_> = kinds
            .into_iter()
            .map(|kind| Vertex {
                pos: [0.; 3],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [0.; 4],
                kind,
                diffuse: 0,
                night_diffuse: 0,
            })
            .collect();
        let indices: Vec<u32> = (0..12).collect();
        assert_eq!(
            filter_sdr_indices(&vertices, &indices, 0..12),
            (3..12).collect::<Vec<_>>()
        );
    }

    fn solid(fill: u8, side: usize) -> crate::objects::Texture {
        crate::objects::Texture {
            width: side,
            height: side,
            rgba: vec![fill; side * side * 4],
        }
    }
    #[test]
    fn emitted_atlas_reuses_slots_without_pixels_or_mip_allocations() {
        let mut original = Builder::new();
        let texture = solid(71, 128);
        let (slot, _) = allocate_slot(&mut original, "shared", &texture).unwrap();
        let mut slots = HashMap::new();
        remember_atlas(&original, 3, &mut slots);
        let reference = slots["shared"];
        assert_eq!(reference.source, 3);
        assert_eq!(reference.height, 128);
        assert_eq!(reference.slot.x, slot.x);
        let vertex = Vertex {
            pos: [0.; 3],
            normal: [0.; 3],
            uv: [0.01, 0.015625],
            color: [0., 0., 0.03125, 0.03125],
            kind: 2.,
            diffuse: 0,
            night_diffuse: 0,
        };
        let mut geometry = Builder::shared(reference.source, reference.height);
        geometry.vertices.push(vertex);
        geometry.indices.push(0);
        let page = geometry.page(32);
        assert_eq!(page.shared_atlas, Some(3));
        assert!(page.atlas.is_empty() && page.mips.is_empty());
        assert_eq!(page.vertices[0].uv[1], 0.5);
        assert_eq!(page.vertices[0].color[3], 1.);
        // Geometry chunks no longer multiply the source texture's pixel cost.
        assert_eq!(
            atlas_mip_bytes(128),
            (0..8).map(|l| (4096 >> l) * (128 >> l) * 4).sum::<usize>()
        );
    }
    #[test]
    fn content_dedup_reduces_page_turns() {
        // 512x512 textures occupy 16 atlas cells each; a 4096^2 page holds
        // 1024 cells = 64 of them. Push 128 references through both keying
        // schemes and count page turns.
        let textures: Vec<_> = (0..64).map(|i| solid(i as u8 + 1, 512)).collect();
        // Content-keyed: the 128 references are 64 contents twice, so every
        // second reference reuses a slot and ONE page suffices.
        let mut page = Builder::new();
        page.indices.push(0);
        let mut dedup_turns = 0;
        for i in 0..128 {
            let tex = &textures[i % 64];
            let (_slot, turned) =
                allocate_slot(&mut page, &format!("h{:016x}", i % 64), tex).unwrap();
            if turned.is_some() {
                dedup_turns += 1;
            }
        }
        assert_eq!(dedup_turns, 0);
        assert_eq!(page.slots.len(), 64);
        // Name-keyed with 65 distinct contents: the 65th overflows the page
        // and forces a turn — exactly what content dedup avoids when the
        // extra references are aliases.
        let mut page = Builder::new();
        page.indices.push(0);
        let mut unique_turns = 0;
        let extra = solid(200, 512);
        for i in 0..65 {
            let tex = if i < 64 { &textures[i] } else { &extra };
            let (_slot, turned) = allocate_slot(&mut page, &format!("name{i}"), tex).unwrap();
            if turned.is_some() {
                unique_turns += 1;
            }
        }
        assert_eq!(unique_turns, 1);
        assert!(dedup_turns < unique_turns);
    }
    #[test]
    fn missing_references_share_one_fallback_slot() {
        let mut page = Builder::new();
        page.indices.push(0);
        let fallback = solid(160, 1);
        for name in ["<missing-client-texture>"; 3] {
            let (_slot, turned) = allocate_slot(&mut page, name, &fallback).unwrap();
            assert!(turned.is_none());
        }
        assert_eq!(page.slots.len(), 1);
    }
    #[test]
    fn trim_scales_y_and_builds_independent_mips() {
        let vertex = Vertex {
            pos: [0.; 3],
            normal: [0.; 3],
            uv: [0.25, 0.03125],
            color: [0., 0.03125, 0.125, 0.03125],
            kind: 2.,
            diffuse: 0xffffffff,
            night_diffuse: 0xffff_ffff,
        };
        let page = Page {
            shared_atlas: None,
            vertices: vec![vertex],
            indices: vec![0],
            atlas: vec![255; SIDE * 256 * 4],
            processed: 0,
            used_height: 256,
            mips: Vec::new(),
            placements: Vec::new(),
        }
        .finish();
        assert_eq!(page.mips.len(), 7);
        assert_eq!(page.atlas.len(), SIDE * 256 * 4);
        assert_eq!(page.vertices[0].uv[1], 0.5);
        assert_eq!(page.vertices[0].color[3], 0.5);
        assert_eq!(page.mips[6].len(), (SIDE / 128) * 2 * 4);
    }
    /// A placement span as the builder writes them: touching, in order.
    fn span(start: u32, end: u32, order: u64, reflect: bool, refract: bool) -> PlacementRange {
        PlacementRange {
            start,
            end,
            export_id: 0,
            order,
            visibility: Visibility { reflect, refract },
            bounds: None,
        }
    }

    fn cmd(page: usize, start: u32, end: u32) -> DrawCmd {
        DrawCmd { page, start, end }
    }

    fn pages<'a>(spans: &[&'a [PlacementRange]], counts: &[u32]) -> Vec<PageSpans<'a>> {
        spans
            .iter()
            .zip(counts)
            .map(|(placements, count)| PageSpans {
                count: *count,
                placements: *placements,
            })
            .collect()
    }

    /// Every index a command list draws, expanded back out: the property the
    /// fold has to preserve, whatever it does to the command count.
    fn drawn(cmds: &[DrawCmd]) -> Vec<(usize, u32)> {
        cmds.iter()
            .flat_map(|c| (c.start..c.end).map(move |i| (c.page, i)))
            .collect()
    }

    #[test]
    fn a_page_run_folds_into_one_draw() {
        // Four touching placements of one page, submitted back to back, are one
        // draw. The whole-world pass used to issue four.
        let p = [
            span(0, 30, 10, true, true),
            span(30, 60, 11, true, true),
            span(60, 100, 12, true, true),
            span(100, 140, 13, true, true),
        ];
        let pages = pages(&[&p], &[140]);
        let order = [(0, 0), (0, 1), (0, 2), (0, 3)];
        let cmds = fold_commands(&pages, &order, Pass::Main);
        assert_eq!(cmds, vec![cmd(0, 0, 140)]);
        assert_eq!(drawn(&cmds).len(), 140);
    }

    #[test]
    fn frustum_culling_keeps_visible_mesh_order_and_gaps() {
        let mut near_a = span(0, 30, 0, true, true);
        near_a.bounds = Some([-2., -1., -5., 2., 1., -3.]);
        let mut far = span(30, 60, 1, true, true);
        far.bounds = Some([100., -1., -5., 104., 1., -3.]);
        let mut near_b = span(60, 90, 2, true, true);
        near_b.bounds = Some([-1., -1., -8., 1., 1., -6.]);
        let p = [near_a, far, near_b];
        let pages = pages(&[&p], &[90]);
        let order = [(0, 0), (0, 1), (0, 2)];
        let commands = fold_commands_culled(&pages, &order, Pass::Main, |bounds| bounds[0] < 10.);
        assert_eq!(commands, vec![cmd(0, 0, 30), cmd(0, 60, 90)]);
        assert_eq!(
            drawn(&commands),
            (0..30).chain(60..90).map(|i| (0, i)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn world_cull_index_matches_linear_visibility_in_submission_order() {
        let mut bounds = (0..64)
            .map(|x| Some([x as f32, 0., 0., x as f32 + 0.5, 1., 1.]))
            .collect::<Vec<_>>();
        bounds.push(None);
        bounds.push(Some([f32::NAN, 0., 0., 1., 1., 1.]));
        let index = WorldCullIndex::from_ordered_bounds(&bounds);
        let mut tests = 0;
        let visible = index.visible_order_indices(|aabb| {
            tests += 1;
            aabb[0] < 4.
        });
        assert_eq!(visible, vec![0, 1, 2, 3, 64, 65]);
        assert!(tests < 64, "spatial query made {tests} bounds tests");

        let mut reused = Vec::with_capacity(128);
        let mut stack = Vec::with_capacity(128);
        index.visible_order_indices_into(|aabb| aabb[0] < 4., &mut reused, &mut stack);
        assert_eq!(reused, visible);
        let visible_capacity = reused.capacity();
        let stack_capacity = stack.capacity();
        index.visible_order_indices_into(|_| true, &mut reused, &mut stack);
        assert_eq!(reused.len(), bounds.len());
        assert_eq!(reused.capacity(), visible_capacity);
        assert_eq!(stack.capacity(), stack_capacity);
    }

    #[test]
    fn paired_culling_reuses_one_bounds_test_for_both_world_passes() {
        let mut first = span(0, 30, 0, true, true);
        first.bounds = Some([-2., -1., -5., 2., 1., -3.]);
        let mut hidden = span(30, 60, 1, true, true);
        hidden.bounds = Some([100., -1., -5., 104., 1., -3.]);
        let mut last = span(60, 90, 2, true, true);
        last.bounds = Some([-1., -1., -8., 1., 1., -6.]);
        let opaque_spans = [first, hidden, last];
        let sdr_spans = [
            span(0, 12, 0, true, true),
            span(12, 12, 1, true, true),
            span(12, 24, 2, true, true),
        ];
        let opaque_pages = pages(&[&opaque_spans], &[90]);
        let sdr_pages = pages(&[&sdr_spans], &[24]);
        let order = [(0, 0), (0, 1), (0, 2)];
        let mut bounds_tests = 0;
        let (opaque, sdr) = fold_commands_culled_pair(
            &opaque_pages,
            &sdr_pages,
            &order,
            Pass::Reflection,
            |bounds| {
                bounds_tests += 1;
                bounds[0] < 10.
            },
        );
        assert_eq!(bounds_tests, 3);
        assert_eq!(opaque, vec![cmd(0, 0, 30), cmd(0, 60, 90)]);
        assert_eq!(sdr, vec![cmd(0, 0, 24)]);
    }

    #[test]
    fn a_local_page_folds_its_visible_placements_and_skips_the_hidden_ones() {
        let p = [
            span(0, 30, 0, true, true),
            span(30, 60, 1, false, true),
            span(60, 100, 2, true, true),
            span(100, 140, 3, true, true),
        ];
        assert_eq!(fold_page_spans(&p, Pass::Main), vec![(0, 140)]);
        assert_eq!(
            fold_page_spans(&p, Pass::Reflection),
            vec![(0, 30), (60, 140)]
        );
        assert_eq!(fold_page_spans(&p, Pass::Refraction), vec![(0, 140)]);
        // The hidden placement's indices stay out of the reflection spans.
        let drawn: u32 = fold_page_spans(&p, Pass::Reflection)
            .iter()
            .map(|(start, end)| end - start)
            .sum();
        assert_eq!(drawn, 30 + 80);
    }

    #[test]
    fn a_hidden_placement_splits_the_run_and_is_never_drawn() {
        let p = [
            span(0, 30, 10, true, true),
            span(30, 60, 11, false, true),
            span(60, 100, 12, true, true),
            span(100, 140, 13, true, true),
        ];
        let pages = pages(&[&p], &[140]);
        let order = [(0, 0), (0, 1), (0, 2), (0, 3)];
        // Reflection hides the second placement, so no span may cross it.
        let reflection = fold_commands(&pages, &order, Pass::Reflection);
        assert_eq!(reflection, vec![cmd(0, 0, 30), cmd(0, 60, 140)]);
        assert!(!drawn(&reflection)
            .iter()
            .any(|&(_, i)| (30..60).contains(&i)));
        // Refraction sees all four and folds them into one draw.
        let refraction = fold_commands(&pages, &order, Pass::Refraction);
        assert_eq!(refraction, vec![cmd(0, 0, 140)]);
    }

    #[test]
    fn interleaved_pages_keep_their_own_draws() {
        // The frozen order is a global sort, so pages interleave. A fold never
        // crosses a page boundary and the drawn sequence stays as submitted.
        let a = [span(0, 30, 10, true, true), span(30, 60, 30, true, true)];
        let b = [span(0, 40, 20, true, true)];
        let pages = pages(&[&a, &b], &[60, 40]);
        let order = [(0, 0), (1, 0), (0, 1)];
        let cmds = fold_commands(&pages, &order, Pass::Main);
        assert_eq!(cmds, vec![cmd(0, 0, 30), cmd(1, 0, 40), cmd(0, 30, 60)]);
        assert_eq!(drawn(&cmds).len(), 100);
    }

    #[test]
    fn a_page_without_placements_draws_every_index() {
        let pages = pages(&[&[]], &[512]);
        let cmds = fold_commands(&pages, &[(0, usize::MAX)], Pass::Reflection);
        assert_eq!(cmds, vec![cmd(0, 0, 512)]);
    }

    #[test]
    fn the_fold_draws_exactly_what_the_per_placement_loop_drew() {
        // Touching spans over three pages, submitted in a global order, under
        // every view: the folded spans must equal the per-placement spans the
        // visibility gate allows, and the fold must never need more draws.
        let mut spans: Vec<Vec<PlacementRange>> = Vec::new();
        for page in 0..3u32 {
            let mut list = Vec::new();
            let mut start = 0u32;
            for i in 0..6u32 {
                let len = 3 + (page * 7 + i * 5) % 11;
                let order = (page * 100 + i) as u64;
                list.push(span(start, start + len, order, i % 2 == 0, i % 3 != 0));
                start += len;
            }
            spans.push(list);
        }
        let refs: Vec<&[PlacementRange]> = spans.iter().map(|list| list.as_slice()).collect();
        let counts: Vec<u32> = spans.iter().map(|list| list.last().unwrap().end).collect();
        let pages = pages(&refs, &counts);
        let mut order: Vec<(u64, usize, usize)> = Vec::new();
        for (page, list) in spans.iter().enumerate() {
            for (range, r) in list.iter().enumerate() {
                order.push((r.order, page, range));
            }
        }
        order.sort_unstable();
        let order: Vec<(usize, usize)> = order.into_iter().map(|(_, p, r)| (p, r)).collect();
        for view in [Pass::Main, Pass::Reflection, Pass::Refraction] {
            let mut expected = Vec::new();
            for &(page, range) in &order {
                let r = &spans[page][range];
                if allows_with_context(view, r.visibility, RenderContext::default()) {
                    expected.extend((r.start..r.end).map(|i| (page, i)));
                }
            }
            let cmds = fold_commands(&pages, &order, view);
            assert_eq!(drawn(&cmds), expected, "view {view:?}");
            assert!(cmds.iter().all(|c| c.start <= c.end));
            assert!(cmds.len() <= order.len());
        }
    }
}
