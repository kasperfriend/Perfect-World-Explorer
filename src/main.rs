#![cfg_attr(windows, windows_subsystem = "windows")]
use flate2::read::ZlibDecoder;
use std::{
    env, fs,
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    let s = b.get(i..i + 4).ok_or("truncated data")?;
    Ok(u32::from_le_bytes(s.try_into().unwrap()))
}
fn f32at(b: &[u8], i: usize) -> Result<f32, String> {
    Ok(f32::from_bits(u32at(b, i)?))
}
fn read_string(b: &[u8], pos: &mut usize) -> Result<String, String> {
    let n = u32at(b, *pos)? as usize;
    *pos += 4;
    if n > 4096 {
        return Err("invalid string length".into());
    }
    let s = b.get(*pos..*pos + n).ok_or("truncated string")?;
    *pos += n;
    std::str::from_utf8(s)
        .map(str::to_owned)
        .map_err(|e| format!("invalid string: {e}"))
}
mod angelica_pck;
mod animation;
mod bmd;
mod bmd_baked;
// The folder scan is filesystem + byte reading only, so `--diagnose-client`
// works on any host with a client staged. It does read `pck`/`angelica_pck`,
// and those are Windows-only in this binary, so the module follows them; the
// `client-check` binary is the cross-platform form of the same report.
#[cfg(windows)]
mod client_detect;
mod client_sun;
#[allow(dead_code)]
mod client_weather;
mod cloud_gpu;
#[allow(dead_code)]
mod cloud_surface;
mod color_chain;
mod diagnostics;
mod sound_key;
// The effect activation census: which gate leaves a root out, as a partition.
mod direct_storage_readout;
mod effect_census;
// A preview is a test-only harness. The skip is on the module: its literals
// are authored GFX text and its tables are read top to bottom, neither of
// which formatting improves, and every other module stays checked.
mod display_gamma;
mod dlss;
mod dxt;
mod ecmodel;
mod effect_preview;
mod environment;
mod far_trees_gpu;
mod fog_matrix;
mod foliage_mips;
mod fsr;
mod fsr1_gpu;
#[allow(dead_code)]
mod gfx_catalog;
mod gfx_color_track;
mod gfx_decal;
mod gfx_decal_gpu;
/// The wgpu receiver for the animated roots. Its diagnostic accessors are part
/// of the module's contract even while only its own tests call them.
#[allow(dead_code)]
mod gfx_effect_gpu;
/// The full `.gfx` grammar: every field the container can carry is parsed and
/// documented, and the runtime player consumes only the visual subset.
#[allow(dead_code)]
mod gfx_file;
#[allow(dead_code)]
mod gfx_light;
mod gfx_owner;
/// The runtime that turns a parsed root into this frame's quads. `reset`,
/// `textures` and `time_ms` are public contract for callers that are not wired
/// up yet, so the module carries the same allowance `gfx_file` does.
#[allow(dead_code)]
mod gfx_player;
mod gpu;
mod grass;
mod grass_instances;
mod grass_mips;
mod grass_wind;
mod indirect_args;
mod latency;
#[cfg(windows)]
mod launcher_ui;
mod light_consumers;
mod load_status;
// The throughput sparkline is drawn by the standalone loading window and by the
// in-viewer overlay a world change shows, and the overlay is compiled on every
// host, so this module is no longer Windows-only.
mod loading_graph;
// The loading UI's frame composition: the standalone window and the in-viewer
// overlay a world change shows both go through it, so the two screens cannot
// drift apart. Platform-neutral, so it is compiled and tested on every host.
mod loading_overlay;
// The client's own loading artwork: the Windows loading window fills itself from
// `surfaces.pck`, and the headless fixture check exercises the same lookup and
// decode. Declared unconditionally so the path stays tested on every host.
mod loading_screen;
mod material;
mod model_asset;
mod model_attachment;
mod model_dds;
mod model_draw;
#[cfg(test)]
#[path = "../tools/fixtures/model_draw.rs"]
mod model_draw_fixture;
mod model_draw_gpu;
mod model_light;
mod model_material;
mod model_playback;
mod model_queue;
mod model_receiver_class;
mod model_receiver_gpu;
mod model_texture_gpu;
mod modes;
mod modules;
mod music;
mod navigation;
mod objects;
mod pck;
mod post_gpu;
#[cfg(test)]
mod post_reference;
mod present_encode;
mod render_scale;
#[allow(dead_code)]
mod scene;
#[allow(dead_code)]
mod scene_lights;
mod scenery_visibility;
mod shore_gpu;
mod simple_water_gpu;
mod skeleton;
mod skeleton_pose;
mod skin;
mod skin_pose;
mod sky_celestial;
mod sky_config;
mod sky_geometry;
mod sky_gpu;
mod sky_time;
mod world_audio;
// Accessors that only the Phase 5 diagnostic and the future weather wiring use
// are kept public and unreferenced here, like `client_sun`.
#[allow(dead_code)]
mod sky_transition;
mod speedtree_version;
mod terrain_block;
mod terrain_cutoff;
mod terrain_gpu;
mod terrain_lightmaps;
#[allow(dead_code)]
mod terrain_lod;
mod terrain_residency;
mod terrain_sampling;
#[allow(dead_code)]
mod terrain_shading;
mod terrain_store;
mod terrain_upload;
mod texture_atlas;
mod texture_resolution;
mod tree_lod;
#[cfg(windows)]
mod tree_source;
#[allow(dead_code)]
mod tree_wind;
mod trees;
mod vegetation_material;
mod vegetation_textures;
mod view_settings;
mod water;
mod water_gpu;
mod weather_gpu;
#[cfg(windows)]
mod windows_setup;
mod world_catalog;
mod world_handoff;
mod world_load;
mod world_models;
mod world_models_gpu;
mod world_objects;
mod world_page_cache;
mod world_place_names;
mod xess;
struct Terrain {
    element: PathBuf,
    /// Client root used to launch a replacement process for another map.
    client_root: PathBuf,
    /// Selected `element/maps/<id>` folder (or a direct map directory).
    map_dir: PathBuf,
    /// Catalog metadata for the selected map, including its instance IDs and
    /// authored region-grid hint.
    map: world_catalog::WorldMap,
    /// Recoverable notice from a failed selected-map load that fell back here.
    startup_notice: Option<String>,
    /// Cells per T2BK block edge (`world.trn2` header dword @20).
    grid: usize,
    width: usize,
    height: usize,
    heights: terrain_store::Heights,
    normals: terrain_store::Normals,
    grid_spacing: f32,
    materials: material::Materials,
    subterrain_names: Vec<String>,
    objects: objects::Objects,
    water: Vec<water::WaterArea>,
    environments: environment::Environments,
    scene_lights: std::sync::Arc<scene_lights::SceneLights>,
    /// Retained ECWLD so the renderer can own its effect light producers.
    scene: std::sync::Arc<scene::Scene>,
    /// Native `CECSceneBlock` regional load/evict bits for the ornament class
    /// (`0x424020`). Owned here because `Terrain` lives on the streaming worker.
    scenery_residency: scenery_visibility::RegionResidency,
    terrain_material_window: Option<Option<[usize; 4]>>,
    models: world_models::Assets,
    water_normals: Vec<Vec<image::RgbaImage>>,
    water_simple_frames: Vec<Vec<image::RgbaImage>>,
    trees: trees::Trees,
    grass: grass::Grass,
    /// Parsed native world cloud grid, if this map ships `world.cld` loose.
    cloud_surface: Option<std::sync::Arc<cloud_surface::CloudSurface>>,
    /// The ten-class scenery placement list, built once from the map's own
    /// blocks and ornament rows.
    ///
    /// It used to be rebuilt for every block-window crossing: a hash map over
    /// every ornament, then one placement per tree, water cell and ornament in
    /// the whole scene — tens of thousands of entries, world-sized work repeated
    /// for a camera move of one block. Nothing in it depends on the camera, the
    /// window or the detail settings, so it is built once and shared (`Arc`
    /// because `build` needs it while other fields of `Terrain` are borrowed
    /// mutably, and the streaming worker owns `Terrain`).
    scenery_placements: Option<std::sync::Arc<Vec<scenery_visibility::Placement>>>,
}

/// Everything one tile needs to decode its blocks: the tile's own bytes plus the
/// shared tables. Bundled so a worker only needs a borrow of it.
struct TileDecode<'a> {
    data: &'a [u8],
    name: &'a str,
    version: u32,
    flags: u8,
    layout: &'a terrain_block::Layout,
    grid: usize,
    sin_table: &'a [f32],
    cos_table: &'a [f32],
    blocks: usize,
}

/// One decoded block on its way back from a worker: its index in the tile and,
/// when the decode succeeded, the `grid * grid` arrays.
type BlockPayload = (usize, Result<(Vec<f32>, Vec<[i8; 3]>), String>);

#[derive(Default)]
struct DecodeWindow {
    next: usize,
    next_expected: usize,
    cancelled: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum DecodeClaim {
    Block(usize),
    Wait,
    Done,
}

impl DecodeWindow {
    fn claim(&mut self, blocks: usize, window: usize) -> DecodeClaim {
        if self.cancelled || self.next >= blocks {
            return DecodeClaim::Done;
        }
        if self.next >= self.next_expected.saturating_add(window) {
            return DecodeClaim::Wait;
        }
        let block = self.next;
        self.next += 1;
        DecodeClaim::Block(block)
    }
}

/// Per-block scratch, owned by whoever decodes a block.
///
/// Blocks are decoded on several threads (see `decode_blocks`), so each worker
/// keeps its own scratch and only the two finished arrays travel back to the
/// builder. Reusing the rest keeps the per-block allocation count at two.
struct BlockScratch {
    decoded: Vec<u8>,
    values: Vec<f32>,
    normal_data: Vec<u8>,
    block_normals: Vec<[i8; 3]>,
    decoded_colour: Vec<u8>,
    heights: Vec<f32>,
    normals: Vec<[i8; 3]>,
}

impl Default for BlockScratch {
    fn default() -> Self {
        Self {
            decoded: Vec::with_capacity(65 * 65 * 4),
            values: Vec::with_capacity(65 * 65),
            normal_data: Vec::with_capacity(65 * 65 * 4),
            block_normals: Vec::with_capacity(65 * 65),
            decoded_colour: Vec::with_capacity(65 * 65 * 4),
            heights: Vec::new(),
            normals: Vec::new(),
        }
    }
}
/// Decode one block and move its two arrays out of the worker's scratch.
fn decode_block_owned(
    tile: &TileDecode,
    scratch: &mut BlockScratch,
    bi: usize,
) -> Result<(Vec<f32>, Vec<[i8; 3]>), String> {
    decode_block(tile, scratch, bi)?;
    Ok((
        std::mem::take(&mut scratch.heights),
        std::mem::take(&mut scratch.normals),
    ))
}

/// Decode every block of one tile, handing the finished arrays to `insert` in
/// block order.
///
/// One thread per core (capped) decodes independently; finished blocks are held
/// in a map and pushed into `insert` only once every lower index has gone in,
/// so the builder sees a bit-identical sequence to the old sequential loop.
/// Scheduling stays at most two worker waves ahead of the next insertion, which
/// bounds decoded-but-not-yet-inserted memory if an early block is unusually
/// slow. `insert` runs on the calling thread, so the builder needs no locking.
/// `PW_TERRAIN_THREADS=1` restores the plain loop.
fn decode_blocks<F>(tile: &TileDecode, threads: usize, mut insert: F) -> Result<(), String>
where
    F: FnMut(usize, &[f32], &[[i8; 3]]) -> Result<(), String>,
{
    if tile.blocks == 0 {
        return Ok(());
    }
    if threads.min(tile.blocks) <= 1 {
        let mut scratch = BlockScratch::default();
        for bi in 0..tile.blocks {
            decode_block(tile, &mut scratch, bi)?;
            insert(bi, &scratch.heights, &scratch.normals)?;
        }
        return Ok(());
    }
    let worker_count = threads.min(tile.blocks);
    // Keep only a small window of decoded blocks ahead of the next ordered
    // insertion. Without this bound, a single slow early block lets every other
    // worker decode the whole tile into the channel/pending map at once.
    let window = worker_count.saturating_mul(2).max(worker_count);
    let state = std::sync::Mutex::new(DecodeWindow {
        next: 0,
        next_expected: 0,
        cancelled: false,
    });
    let wake = std::sync::Condvar::new();
    let (tx, rx) = std::sync::mpsc::channel::<BlockPayload>();
    std::thread::scope(|scope| -> Result<(), String> {
        for _ in 0..worker_count {
            let tx = tx.clone();
            let state = &state;
            let wake = &wake;
            scope.spawn(move || {
                let mut scratch = BlockScratch::default();
                loop {
                    let bi = {
                        let mut scheduler = state.lock().unwrap();
                        loop {
                            match scheduler.claim(tile.blocks, window) {
                                DecodeClaim::Block(bi) => break bi,
                                DecodeClaim::Done => return,
                                DecodeClaim::Wait => {
                                    scheduler = wake.wait(scheduler).unwrap();
                                }
                            }
                        }
                    };
                    let decoded = decode_block_owned(tile, &mut scratch, bi);
                    let failed = decoded.is_err();
                    if tx.send((bi, decoded)).is_err() {
                        return;
                    }
                    if failed {
                        let mut scheduler = state.lock().unwrap();
                        scheduler.cancelled = true;
                        wake.notify_all();
                        return;
                    }
                }
            });
        }
        drop(tx);
        let result = (|| -> Result<(), String> {
            let mut next_expected = 0usize;
            let mut pending = std::collections::BTreeMap::new();
            while next_expected < tile.blocks {
                let Ok((bi, decoded)) = rx.recv() else {
                    break;
                };
                match decoded {
                    Ok(pair) => {
                        pending.insert(bi, pair);
                    }
                    Err(e) => return Err(e),
                }
                while let Some((heights, normals)) = pending.remove(&next_expected) {
                    insert(next_expected, &heights, &normals)?;
                    next_expected += 1;
                    let mut scheduler = state.lock().unwrap();
                    scheduler.next_expected = next_expected;
                    wake.notify_one();
                    if next_expected >= tile.blocks {
                        break;
                    }
                }
            }
            if next_expected < tile.blocks {
                return Err(format!("{}: terrain decode stopped early", tile.name));
            }
            Ok(())
        })();
        if result.is_err() {
            let mut scheduler = state.lock().unwrap();
            scheduler.cancelled = true;
            wake.notify_all();
        }
        result
    })
}

/// Decode one T2BK block into `scratch.heights` / `scratch.normals` (both
/// `grid * grid`).
///
/// The function is pure with respect to the world: it only borrows the tile
/// bytes and the shared trig tables, which is what lets the loader run several
/// blocks of one tile at the same time. The returned arrays are exactly what
/// the sequential loop used to hand to `Builder::block`.
fn decode_block(tile: &TileDecode, scratch: &mut BlockScratch, bi: usize) -> Result<(), String> {
    let TileDecode {
        data,
        name,
        version,
        flags,
        layout,
        grid,
        sin_table,
        cos_table,
        ..
    } = *tile;
    let BlockScratch {
        decoded,
        values,
        normal_data,
        block_normals,
        decoded_colour,
        heights,
        normals,
    } = scratch;

    let off = u32at(&data, 16 + bi * 4)? as usize;
    let hs = u32at(&data, off + 16)? as usize;
    let ns = u32at(&data, off + 20)? as usize;
    let hdr = if version >= 6 {
        36
    } else if version >= 4 {
        32
    } else if version >= 3 {
        28
    } else {
        24
    };
    let raw = data
        .get(off + hdr..off + hdr + hs)
        .ok_or("truncated height data")?;
    decoded.clear();
    if flags & 4 != 0 {
        let mut d = ZlibDecoder::new(raw);
        d.by_ref()
            .take(((grid + 1) * (grid + 1) * 4 + 64) as u64)
            .read_to_end(&mut *decoded)
            .map_err(|e| format!("{name} block {bi}: {e}"))?;
    } else {
        decoded.extend_from_slice(raw)
    }
    let nv = (grid + 1) * (grid + 1);
    values.clear();
    if flags & 8 != 0 {
        let min = f32at(&decoded, 4)?;
        let max = f32at(&decoded, 8)?;
        if u32at(&decoded, 12)? as usize != nv {
            return Err("bad incremental height count".into());
        }
        let mut acc = 0i32;
        for j in 0..nv {
            let p = 16 + j * 2;
            let s = decoded.get(p..p + 2).ok_or("truncated deltas")?;
            acc += i16::from_le_bytes([s[0], s[1]]) as i32;
            values.push(min + acc as f32 * (max - min) / 32767.0);
        }
    } else {
        if decoded.len() != nv * 4 {
            return Err(format!(
                "{name} block {bi}: bad height size {} expected {}",
                decoded.len(),
                nv * 4
            ));
        }
        for j in 0..nv {
            values.push(f32at(&decoded, j * 4)?)
        }
    }
    let noff = off
        .checked_add(hdr)
        .and_then(|x| x.checked_add(hs))
        .ok_or("normal offset overflow")?;
    let normal_raw = data
        .get(noff..noff.checked_add(ns).ok_or("normal size overflow")?)
        .ok_or("truncated normal data")?;
    normal_data.clear();
    if flags & 0x10 != 0 {
        ZlibDecoder::new(normal_raw)
            .take((nv * 12 + 64) as u64)
            .read_to_end(&mut *normal_data)
            .map_err(|e| format!("{name} block {bi} normals: {e}"))?;
    } else {
        normal_data.extend_from_slice(normal_raw)
    }
    block_normals.clear();
    if flags & 0x20 != 0 {
        let base = if u32at(&normal_data, 4).ok() == Some(nv as u32) {
            8
        } else if u32at(&normal_data, 1).ok() == Some(nv as u32) {
            5
        } else {
            return Err(format!("{name} block {bi}: invalid normal header"));
        };
        for j in 0..nv {
            let az = *normal_data.get(base + 2 * j).ok_or("truncated normals")? as usize;
            let el = *normal_data
                .get(base + 2 * j + 1)
                .ok_or("truncated normals")? as usize;
            let se = sin_table[el];
            block_normals.push([
                (se * cos_table[az] * 127.) as i8,
                (cos_table[el] * 127.) as i8,
                (se * sin_table[az] * 127.) as i8,
            ]);
        }
    } else {
        for j in 0..nv {
            let mut n = [0i8; 3];
            for k in 0..3 {
                let v = f32at(&normal_data, j * 12 + k * 4)?;
                if !v.is_finite() {
                    return Err("nonfinite normal".into());
                }
                n[k] = (v.clamp(-1., 1.) * 127.) as i8;
            }
            block_normals.push(n);
        }
    }
    // Third section: per-vertex RGBA, present when flag bit 0x1 is set.
    if let Some((cs, ce)) = layout.blocks[bi].colour {
        let raw = data.get(cs..ce).ok_or("truncated vertex colour data")?;
        decoded_colour.clear();
        ZlibDecoder::new(raw)
            .take((nv * 4 + 64) as u64)
            .read_to_end(&mut *decoded_colour)
            .map_err(|e| format!("{name} block {bi} vertex colour: {e}"))?;
        if decoded_colour.len() != nv * 4 {
            return Err(format!(
                "{name} block {bi}: vertex colour is {} bytes, expected {}",
                decoded_colour.len(),
                nv * 4
            ));
        }
    }
    heights.clear();
    normals.clear();
    for y in 0..grid {
        for x in 0..grid {
            let val = values[y * (grid + 1) + x];
            if !val.is_finite() {
                return Err("nonfinite height".into());
            }
            heights.push(val);
            normals.push(block_normals[y * (grid + 1) + x]);
        }
    }
    Ok(())
}

/// Load the world.
///
/// `all_to_vram` is the persisted "keep everything resident" choice: it selects
/// the fully decoded terrain grid (no disk paging, no block decode) instead of
/// the paged cache, which is much faster at load and while flying and uses the
/// whole grid in RAM.
fn terrain(
    root: &Path,
    all_to_vram: bool,
    preferred: Option<&str>,
    startup_notice: Option<String>,
) -> Result<Terrain, String> {
    if let Some(id) = preferred {
        world_catalog::validate_selection(id)?;
    }
    let map = world_catalog::resolve(root, preferred)?;
    let dir = map.dir.clone();
    let element = map.element.as_path();
    let client_root = world_catalog::client_root(root);
    eprintln!(
        "Loading map {} ({}) from {}",
        map.display_name,
        map.id,
        dir.display()
    );
    if let Some(warning) = world_catalog::discover(element).warning {
        eprintln!("World catalog: {warning}");
    }
    world_handoff::publish_progress(&format!("Loading {} ({})...", map.display_name, map.id));
    let terrain_path = world_catalog::data_file(&dir, "trn2");
    let t = fs::read(&terrain_path).map_err(|e| format!("{}: {e}", terrain_path.display()))?;
    if u32at(&t, 0)? != 0x54524e32 || u32at(&t, 4)? > 1 {
        return Err("unsupported TRN2".into());
    }
    let grid = u32at(&t, 20)? as usize;
    let grid_spacing = f32at(&t, 68)?;
    if !grid_spacing.is_finite() || grid_spacing <= 0.0 {
        return Err("invalid terrain grid spacing".into());
    }
    let rows = u32at(&t, 28)? as usize;
    let cols = u32at(&t, 32)? as usize;
    let count = u32at(&t, 52)? as usize;
    let subgrid = u32at(&t, 64)? as usize;
    let loader = u32at(&t, 76)?;
    let loff = u32at(&t, 80)? as usize;
    if loader != 1
        || grid == 0
        || grid > 256
        || subgrid == 0
        || subgrid % grid != 0
        || rows == 0
        || cols == 0
        || rows > 4096
        || cols > 4096
        || count > 10000
    {
        return Err("unsupported or invalid world terrain header".into());
    }
    let per = subgrid / grid;
    if rows % per != 0 || cols % per != 0 || rows / per * (cols / per) != count {
        return Err("inconsistent subterrain dimensions".into());
    }
    let w = cols.checked_mul(grid).ok_or("size overflow")?;
    let h = rows.checked_mul(grid).ok_or("size overflow")?;
    if w < 2 || h < 2 {
        return Err("map terrain dimensions are too small".into());
    }
    if w * h > 100_000_000 {
        return Err("terrain too large".into());
    }
    let mut pos = loff + 4;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(read_string(&t, &mut pos)?)
    }
    let cache_root = std::env::var_os("PW_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cache"))
        .join("terrain-v1");
    // Residency choice, made before a single block is read:
    //
    // * All to VRAM keeps the decoded grid in memory and never writes a cache.
    // * Otherwise the warm-start index is tried first; a hit skips the block
    //   decode and the whole recompression pass, and only a miss builds pages.
    let identity = if all_to_vram {
        String::new()
    } else {
        terrain_store::source_identity(&t, &dir, &names)?
    };
    let cached = if all_to_vram {
        None
    } else {
        terrain_store::restore(&cache_root, &identity, w, h, grid, terrain_store::budget())?
    };
    let mut terrain_pages = if cached.is_some() {
        None
    } else if all_to_vram {
        Some(terrain_store::Builder::new_resident(w, h, grid)?)
    } else {
        Some(terrain_store::Builder::new(&cache_root, w, h, grid)?.with_identity(&identity))
    };
    // Colour sections are validated per block below. Neither current CPU nor
    // GPU renderer consumes them; do not allocate a second world-sized RGBA grid.
    let sin_table: Vec<f32> = (0..256)
        .map(|i| (i as f32 * std::f32::consts::TAU / 256.).sin())
        .collect();
    let cos_table: Vec<f32> = (0..256)
        .map(|i| (i as f32 * std::f32::consts::TAU / 256.).cos())
        .collect();
    let subs_x = cols / per;
    // Decoding threads for the block loop below: one per core unless pinned.
    // Each block is independent, so this is pure throughput; the builder still
    // sees the blocks in exactly the order the sequential loop produced them.
    let threads = std::env::var("PW_TERRAIN_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (1..=64).contains(n))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });
    // A warm start already owns the grid, so the block loop is skipped entirely.
    if terrain_pages.is_some() {
        for (si, name) in names.iter().enumerate() {
            if name.contains('/') || name.contains('\\') || name.contains("..") {
                return Err("unsafe subterrain path".into());
            }
            // A terrain block that cannot be read costs that block, not the
            // map: the rest of the terrain, the scenery, the water and the
            // effects are all still worth showing, and a named block is
            // something an author can act on. The version check and the layout
            // check below both report by block name.
            let Ok(data) = fs::read(dir.join(format!("{name}.t2bk"))) else {
                eprintln!("terrain block {name}: missing or unreadable, skipped");
                continue;
            };
            if u32at(&data, 0)? != 0x54424b42 {
                eprintln!("terrain block {name}: not a T2BK, skipped");
                continue;
            };
            let v = u32at(&data, 4)?;
            if !terrain_block::KNOWN_VERSIONS.contains(&v) {
                eprintln!(
                    "terrain block {name}: unsupported T2BK version {v} \
                     (this build reads {}..={}), skipped",
                    terrain_block::KNOWN_VERSIONS.start(),
                    terrain_block::KNOWN_VERSIONS.end()
                );
                continue;
            }
            let n = u32at(&data, 8)? as usize;
            if n < per * per {
                return Err(format!("{name}: missing blocks"));
            };
            let flags = if v < 5 {
                if data[12] != 0 {
                    0x15
                } else {
                    0
                }
            } else {
                data[12]
            };
            // Validate the whole offset table and section layout before reading any
            // heights: this is what catches a flags bit the reader does not implement,
            // and it is what found the missing per-vertex colour section.
            let block_layout =
                terrain_block::layout(&data, grid).map_err(|e| format!("{name}: {e}"))?;
            let tile = TileDecode {
                data: &data,
                name,
                version: v,
                flags,
                layout: &block_layout,
                grid,
                sin_table: &sin_table,
                cos_table: &cos_table,
                blocks: per * per,
            };
            decode_blocks(&tile, threads, |bi, heights, normals| {
                let br = (si / subs_x) * per + bi / per;
                let bc = (si % subs_x) * per + bi % per;
                terrain_pages
                    .as_mut()
                    .expect("terrain builder present while parsing blocks")
                    .block(bc, br, heights, normals)
            })?;
            eprintln!("Loaded terrain tile {}/{}: {}", si + 1, count, name);
            #[cfg(windows)]
            windows_setup::loading_progress(format!("Loading terrain tiles {}/{}", si + 1, count));
        }
    }
    #[cfg(windows)]
    windows_setup::loading_progress("Loading world scenery and water...".into());
    let (heights, normals) = match (cached, terrain_pages) {
        (Some(grid), _) => grid,
        (None, Some(pages)) => pages.finish(terrain_store::budget())?,
        (None, None) => return Err("terrain cache was not built".into()),
    };
    let scene_path = world_catalog::data_file(&dir, "ecwld");
    let world_scene = scene::Scene::load(&scene_path)?;
    let cloud_path = world_catalog::data_file(&dir, "cld");
    let cloud_surface = match fs::read(&cloud_path) {
        Ok(bytes) => {
            let cloud = cloud_surface::parse(&bytes)
                .map_err(|e| format!("{}: {e}", cloud_path.display()))?;
            let (cell_count, vertex_count, group_count, index_count) =
                cloud.cells.iter().flatten().fold(
                    (0usize, 0usize, 0usize, 0usize),
                    |(cells, vertices, groups, indices), cell| {
                        (
                            cells + usize::from(!cell.vertices.is_empty()),
                            vertices + cell.vertices.len(),
                            groups + cell.groups.len(),
                            indices + cell.groups.iter().map(|g| g.indices.len()).sum::<usize>(),
                        )
                    },
                );
            eprintln!("Loaded native MOXB cloud grid {}x{}: {cell_count} populated cells, {vertex_count} vertices, {group_count} groups, {index_count} indices", cloud.width, cloud.height);
            Some(std::sync::Arc::new(cloud))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "{} unavailable; native cloud surface omitted",
                cloud_path.display()
            );
            None
        }
        Err(e) => return Err(format!("{}: {e}", cloud_path.display())),
    };
    if (world_scene.width - w as f32 * grid_spacing).abs() > 0.1
        || (world_scene.length - h as f32 * grid_spacing).abs() > 0.1
    {
        return Err("ECWLD and TRN2 world dimensions differ".into());
    }
    let ornaments = world_scene.load_ornaments(&world_catalog::ecbsd_path(&dir))?;
    eprintln!(
        "Loaded {} unique world ornament placements; nearby BMD meshes will stream during flight",
        ornaments.len()
    );
    let count_trees: usize = world_scene.blocks.iter().map(|b| b.trees.len()).sum();
    let count_models: usize = world_scene.blocks.iter().map(|b| b.models.len()).sum();
    eprintln!("Loaded scene placements: {count_trees} trees, {count_models} EC models");
    eprintln!("Loading world terrain textures and masks...");
    #[cfg(windows)]
    windows_setup::loading_progress("Loading terrain materials and masks...".into());
    // Shared PCK archives stay rooted at the owning `element` folder, while
    // terrain, scene and object data come from the selected map directory.
    let placements: Vec<_> = world_scene
        .blocks
        .iter()
        .flat_map(|b| b.models.iter().cloned())
        .collect();
    #[cfg(windows)]
    windows_setup::loading_progress(
        "Preparing trees (first run may take several minutes)...".into(),
    );
    let tree_cache = if let Some(dir) = std::env::var_os("PW_TREE_CACHE") {
        PathBuf::from(dir)
    } else {
        #[cfg(windows)]
        {
            windows_setup::prepare_trees_for_scene(
                element,
                &scene_path,
                world_scene.tree_types.len(),
            )?
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("tree-cache")
        }
    };
    let (water, materials, (water_normals, water_simple_frames), objects, models, grass, trees) =
        if all_to_vram {
            std::thread::scope(|s| {
                let water_task =
                    s.spawn(|| water::load(&world_scene, &dir).map_err(|e| e.to_string()));
                let materials_task = s.spawn(|| {
                    material::Materials::load(&dir, &t, &names, w, h, grid_spacing)
                        .map_err(|e| e.to_string())
                });
                let water_frames_task = s.spawn(|| -> Result<_, String> {
                    Ok((
                        water::load_normals(element).map_err(|e| e.to_string())?,
                        water::load_simple_frames(element).map_err(|e| e.to_string())?,
                    ))
                });
                let objects_task = s.spawn(|| {
                    objects::Objects::open(element, ornaments).map_err(|e| e.to_string())
                });
                let models_task =
                    s.spawn(
                        || match world_models::Assets::open(element, placements.clone()) {
                            Ok(models) => models,
                            Err(e) => {
                                world_models::Assets::load_with(placements, |_| Err(e.clone()))
                            }
                        },
                    );
                let grass_task = s.spawn(|| {
                    grass::Grass::load(&world_scene, &dir, element).map_err(|e| e.to_string())
                });
                let trees_task = s.spawn(|| {
                    trees::Trees::open(element, &world_scene, &tree_cache)
                        .map_err(|e| e.to_string())
                });

                Ok::<_, String>((
                    water_task.join().unwrap()?,
                    materials_task.join().unwrap()?,
                    water_frames_task.join().unwrap()?,
                    objects_task.join().unwrap()?,
                    models_task.join().unwrap(),
                    grass_task.join().unwrap()?,
                    trees_task.join().unwrap()?,
                ))
            })?
        } else {
            let (water, materials, (water_normals, water_simple_frames), objects) =
                std::thread::scope(|s| {
                    let water_task =
                        s.spawn(|| water::load(&world_scene, &dir).map_err(|e| e.to_string()));
                    let materials_task = s.spawn(|| {
                        material::Materials::load(&dir, &t, &names, w, h, grid_spacing)
                            .map_err(|e| e.to_string())
                    });
                    let water_frames_task = s.spawn(|| -> Result<_, String> {
                        Ok((
                            water::load_normals(element).map_err(|e| e.to_string())?,
                            water::load_simple_frames(element).map_err(|e| e.to_string())?,
                        ))
                    });
                    let objects_task = s.spawn(|| {
                        objects::Objects::open(element, ornaments).map_err(|e| e.to_string())
                    });

                    Ok::<_, String>((
                        water_task.join().unwrap()?,
                        materials_task.join().unwrap()?,
                        water_frames_task.join().unwrap()?,
                        objects_task.join().unwrap()?,
                    ))
                })?;
            let (models, grass, trees) = std::thread::scope(|s| {
                let models_task =
                    s.spawn(
                        || match world_models::Assets::open(element, placements.clone()) {
                            Ok(models) => models,
                            Err(e) => {
                                world_models::Assets::load_with(placements, |_| Err(e.clone()))
                            }
                        },
                    );
                let grass_task = s.spawn(|| {
                    grass::Grass::load(&world_scene, &dir, element).map_err(|e| e.to_string())
                });
                let trees_task = s.spawn(|| {
                    trees::Trees::open(element, &world_scene, &tree_cache)
                        .map_err(|e| e.to_string())
                });

                Ok::<_, String>((
                    models_task.join().unwrap(),
                    grass_task.join().unwrap()?,
                    trees_task.join().unwrap()?,
                ))
            })?;
            (
                water,
                materials,
                (water_normals, water_simple_frames),
                objects,
                models,
                grass,
                trees,
            )
        };
    eprintln!(
        "Loaded {} water areas from client BSData grids",
        water.len()
    );
    eprintln!(
        "Retained {} EC model assets for {} placements; {} unloaded placements (ordinary animated submission enabled unless PW_DISABLE_WORLD_MODELS is set)",
        models.assets.len(),
        models.instances.len(),
        models.unloaded.len()
    );
    for failure in &models.failures {
        eprintln!("EC model dependency: {failure}");
    }
    #[cfg(windows)]
    if std::env::var_os("PW_TREE_CACHE").is_none() && trees.types.iter().any(|tree| tree.is_none())
    {
        return Err(format!("Some prepared tree types could not be loaded. Close Perfect World Explorer, remove this cache folder, and try again:\n{}",tree_cache.display()));
    }
    let environments = environment::Environments::load(&world_scene, &dir)?;
    // Ordinary world-model receivers query these actual client records.
    // Do not also add them to static baked BMDs or terrain: those surfaces
    // use their separately traced baked-light paths.
    let scene_lights = match scene_lights::SceneLights::load(&world_catalog::named_file(
        &dir,
        "scenelights.dat",
    )) {
        Ok(lights) => {
            eprintln!(
                "Loaded {} client scene lights (static query only)",
                lights.lights.len()
            );
            lights
        }
        Err(e) => {
            eprintln!("Scene lights unavailable: {e}");
            scene_lights::SceneLights::default()
        }
    };
    Ok(Terrain {
        models,
        environments,
        scene_lights: std::sync::Arc::new(scene_lights),
        scene: std::sync::Arc::new(world_scene),
        scenery_residency: scenery_visibility::RegionResidency::default(),
        scenery_placements: None,
        terrain_material_window: None,
        element: element.to_path_buf(),
        client_root,
        map_dir: dir,
        map,
        startup_notice,
        grass,
        water_normals,
        water_simple_frames,
        trees,
        cloud_surface,
        water,
        objects,
        materials,
        subterrain_names: names,
        grid,
        width: w,
        height: h,
        heights,
        normals,
        grid_spacing,
    })
}
fn export_png(terrain: &Terrain, output: &Path) -> Result<(), String> {
    export_height_png(terrain.width, terrain.height, &terrain.heights, output)
}
fn export_height_png(
    w: usize,
    h: usize,
    values: &terrain_store::Heights,
    output: &Path,
) -> Result<(), String> {
    let mut heights = values.reader();
    let f = fs::File::create(output).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let mut stream = writer.stream_writer().map_err(|e| e.to_string())?;
    let mut rgb = vec![0u8; w * 3];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let z = heights.height(i);
            let dx = heights.height(y * w + (x + 1).min(w - 1))
                - heights.height(y * w + x.saturating_sub(1));
            let dy = heights.height((y + 1).min(h - 1) * w + x)
                - heights.height(y.saturating_sub(1) * w + x);
            let light = (0.74 + (-dx * 0.025 + dy * 0.025)).clamp(0.30, 1.15);
            let base = if z < 0.0 {
                [50., 83., 94.]
            } else if z < 80.0 {
                [104., 127., 82.]
            } else if z < 250.0 {
                [131., 133., 94.]
            } else {
                [153., 150., 136.]
            };
            for c in 0..3 {
                rgb[x * 3 + c] = (base[c] * light).clamp(0., 255.) as u8;
            }
        }
        stream.write_all(&rgb).map_err(|e| e.to_string())?;
    }
    stream.finish().map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    io::stderr().flush().ok();
    Ok(())
}
// Upscaling and frame-generation option state machine, SDK discovery and the
// provider contract (Graphics tab → "Upscaling" and "Frame generation").
//
// ## What is real and what is not in this build
//
// The gating, settings and SDK-probe logic below is real and unit-tested.
// The vendor SDK calls live in gpu.rs (the render loop owns the device):
// FSR, XeSS, DLSS and the three frame generators are integrated there
// (`src/fsr`, `src/xess`, `src/dlss`). The parts that are implemented for
// real:
//
// * the option state machine and the settings tokens (`upscaling`,
//   `upscale_quality`, `frame_generation`, `upscale_sdk_dir`);
// * per-provider hardware/SDK gating with visible disabled reasons;
// * the pairing table for frame generation (which upscaler may be combined
//   with which frame generator, per the vendors' documentation as of the
//   knowledge available when this was written — see `docs/blockers.md` and
//   the "Unsupported pairings" section of the report);
// * per-provider internal-resolution factors and quality modes;
// * the jitter (Halton 2,3) sequence and matrix helper the providers need;
// * discovery of the vendor DLLs in a user-chosen directory.
//
// When FSR's hardware/SDK gate passes, the world renders at FSR's recommended
// internal resolution and the upscale runs inside the frame loop; otherwise
// the existing bilinear `render_scale` path applies. No temporal accumulation
// is faked anywhere.
//
// ## Settings tokens
//
// `upscaling=off|dlss|fsr1|fsr2|fsr3|fsr4|xess`,
// `upscale_quality=nativeaa|ultraquality|quality|balanced|performance|ultraperformance`
// (which of them a provider actually offers lives in `modes`: `nativeaa` is
// DLAA on DLSS, Native AA on FSR 3.1/4 and XeSS; `ultraquality` is XeSS only;
// a saved mode the selected provider does not offer degrades to the nearest
// ratio it does offer and `--diagnose` prints the whole matrix),
// `frame_generation=off|dlss|fsr|xess`, `frame_multiplier=2|3|4|6`,
// `fsr1_sharpness=<0..2 stops>`,
// `fsr3_sharpness=<0..1>` (FFX `sharpness` for FSR 2/3.1/4),
// `dlss_sharpness=<0..1>` (NGX `Sharpness`; DLSS sharpens nothing at 0, which
// is what made Quality look blurry next to FSR),
// `xess_sharpness=<0..1>` (the viewer's own RCAS-style pass after XeSS, whose
// SDK has no sharpening control),
// `latency=vsync|reflex|xell`, `upscale_sdk_dir=<path>`.
//
// This logic is intentionally free of GPU and windowing dependencies so it
// can be unit-tested headless. The provider FFI lives in gpu.rs.

/// Vendor IDs as reported by `wgpu::AdapterInfo::vendor`.
pub const VENDOR_NVIDIA: u32 = 0x10de;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_AMD_LEGACY: u32 = 0x1022;
pub const VENDOR_INTEL: u32 = 0x8086;

/// Facts about the adapter that are needed for provider gating. Cheap to
/// copy; captured once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFacts {
    pub vendor: u32,
    pub device: u32,
    pub name: String,
    /// Human-readable backend name (e.g. `"dx12"`, `"vulkan"`), for reports.
    pub backend: String,
    /// True on software/CPU adapters (lavapipe, llvmpipe, WARP).
    pub cpu_fallback: bool,
}

impl GpuFacts {
    pub fn from_parts(
        vendor: u32,
        device: u32,
        name: &str,
        backend: &str,
        cpu_fallback: bool,
    ) -> Self {
        Self {
            vendor,
            device,
            name: name.to_string(),
            backend: backend.to_string(),
            cpu_fallback,
        }
    }

    pub fn is_nvidia(&self) -> bool {
        self.vendor == VENDOR_NVIDIA
    }

    pub fn is_amd(&self) -> bool {
        self.vendor == VENDOR_AMD || self.vendor == VENDOR_AMD_LEGACY
    }

    pub fn is_intel(&self) -> bool {
        self.vendor == VENDOR_INTEL
    }

    /// Heuristic for DLSS Multi Frame Generation (more than one interpolated
    /// frame per present).
    ///
    /// The runtime is the authority and reports it as
    /// `DLSSG.MultiFrameCountMax`: 1 on Ada (RTX 40 series), so a 40-series
    /// card can only interpolate the single frame that 2x produces, and up to
    /// 5 on Blackwell (RTX 50 series) — which is what x3/x4/x6 need. This
    /// decides whether the higher multipliers are offered at all; it never
    /// unlocks anything the DLL does not already allow.
    pub fn supports_dlss_multi_frame_generation(name: &str) -> bool {
        let n = name.to_lowercase();
        const NEEDLES: [&str; 6] = ["rtx 50", "rtx 5888", "b100", "b200", "gb200", "gb300"];
        NEEDLES.iter().any(|needle| n.contains(needle))
    }

    /// Heuristic for DLSS Frame Generation eligibility (Ada/Lovelace-class,
    /// i.e. RTX 40 series and later). The NVIDIA SDK is the authority; this
    /// only decides whether the option is offered before the SDK says so.
    pub fn supports_dlss_frame_generation(name: &str) -> bool {
        let n = name.to_lowercase();
        const NEEDLES: [&str; 14] = [
            "rtx 40",
            "rtx 50",
            "rtx a4000",
            "rtx a4500",
            "rtx a5000",
            "rtx a5500",
            "rtx a6000",
            "rtx 4000 ada",
            "rtx 4500 ada",
            "rtx 5000 ada",
            "rtx 5888",
            "l4",
            "l40",
            "l40s",
        ];
        NEEDLES.iter().any(|needle| n.contains(needle))
    }
}

/// Where the SDK probe looked and what it found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SdkSource {
    /// No SDK folder was configured or auto-detected.
    #[default]
    Unset,
    /// `PW_UPSCALER_SDK_DIR` environment variable.
    Env,
    /// `upscale_sdk_dir` settings key (Settings → Graphics).
    Settings,
    /// `<exe directory>\sdk\` auto-detected beside the viewer.
    ExeFolder,
    /// `<exe directory>\..\..\sdk\` — the vendored SDK at the repository
    /// root when the binary runs from `target\<profile>\`.
    RepoFolder,
    /// `sdk\` relative to the current working directory.
    WorkingDir,
}

impl SdkSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unset => "no SDK folder configured",
            Self::Env => "PW_UPSCALER_SDK_DIR",
            Self::Settings => "upscale_sdk_dir (settings)",
            Self::ExeFolder => "sdk folder next to the executable",
            Self::RepoFolder => "sdk folder at the repository root",
            Self::WorkingDir => "sdk folder in the working directory",
        }
    }
}

/// Presence of the vendor DLLs in one SDK folder. Presence-only: this build
/// never loads a DLL, so a "present" flag means "the file exists", not
/// "the SDK works". Names vary between SDK releases; the report names the
/// file that was matched so a wrong-version SDK is diagnosable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SdkReport {
    /// The folder that was probed, if any.
    pub dir: Option<PathBuf>,
    pub source: SdkSource,
    /// The probed path exists and is a directory.
    pub dir_is_dir: bool,
    /// `nvngx_dlss.dll` — DLSS Super Resolution.
    pub dlss: bool,
    /// `nvngx_dlssg.dll` — DLSS Frame Generation.
    pub dlss_frame_generation: bool,
    /// `sl.interposer.dll` — Streamline host (DLSS goes through it in most
    /// integrations).
    pub streamline: bool,
    /// `amd_fidelityfx_loader_dx12.dll` — FSR API loader (effect DLLs are
    /// dispatched through it; required for any SDK 2.x FSR effect).
    pub fsr_loader: bool,
    /// `amd_fidelityfx_upscaler_dx12.dll` — FSR Upscaling 4 / FSR 3 / FSR 2
    /// (all provided by this one DLL since FSR SDK 2.0).
    pub fsr_upscale: bool,
    /// `amd_fidelityfx_framegeneration_dx12.dll` — FSR Frame Generation 4 /
    /// FSR Frame Generation (Swapchain) 4 / FSR 3 frame generation.
    pub fsr_frame_generation: bool,
    /// `libxess.dll` — XeSS Super Resolution (SM 6.4; Intel Arc or DP4a
    /// fallback on other vendors).
    pub xess: bool,
    /// `libxess_fg.dll` — XeSS 2 Frame Generation.
    pub xess_frame_generation: bool,
    /// `libxell.dll` — Intel Xe Low Latency. Mandatory for XeSS Frame
    /// Generation: the proxy swapchain refuses to initialize without it.
    pub xell: bool,
}

const DLSS_SR_NAMES: [&str; 1] = ["nvngx_dlss.dll"];
const DLSS_FG_NAMES: [&str; 1] = ["nvngx_dlssg.dll"];
const STREAMLINE_NAMES: [&str; 1] = ["sl.interposer.dll"];
const FSR_LOADER_NAMES: [&str; 1] = ["amd_fidelityfx_loader_dx12.dll"];
const FSR_UPSCALE_NAMES: [&str; 1] = ["amd_fidelityfx_upscaler_dx12.dll"];
const FSR_FG_NAMES: [&str; 1] = ["amd_fidelityfx_framegeneration_dx12.dll"];
const XESS_NAMES: [&str; 1] = ["libxess.dll"];
const XESS_FG_NAMES: [&str; 1] = ["libxess_fg.dll"];
const XELL_NAMES: [&str; 1] = ["libxell.dll"];

fn probe_any(dir: &Path, names: &[&str]) -> bool {
    names.iter().any(|name| dir.join(name).is_file())
}

/// Probe the SDK folders in priority order: settings, then
/// `PW_UPSCALER_SDK_DIR`, then `<exe>\sdk\`. Never loads a DLL.
pub fn probe_sdk(settings_dir: &str) -> SdkReport {
    let trimmed = settings_dir.trim();
    let exe_sdk = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("sdk")));
    // A release package carries `sdk\` beside the executable; running the
    // cargo build from the repository root (`target\<profile>\*.exe`) has
    // the repo's own `sdk\` two levels up. Keep the loader candidates in
    // `fsr/imp.rs` symmetric with these.
    let repo_sdk = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .and_then(|dir| dir.parent())
            .and_then(|dir| dir.parent())
            .map(|dir| dir.join("sdk"))
    });
    let cwd_sdk = std::path::PathBuf::from("sdk");
    let autodetect = exe_sdk
        .map(|dir| (dir, SdkSource::ExeFolder))
        .into_iter()
        .chain(repo_sdk.map(|dir| (dir, SdkSource::RepoFolder)))
        .chain(std::iter::once((cwd_sdk, SdkSource::WorkingDir)))
        .find(|(dir, _)| dir.is_dir());
    let (dir, source) = if !trimmed.is_empty() {
        (Some(PathBuf::from(trimmed)), SdkSource::Settings)
    } else if let Some(env) = std::env::var_os("PW_UPSCALER_SDK_DIR") {
        (Some(PathBuf::from(env)), SdkSource::Env)
    } else if let Some((detected, source)) = autodetect {
        (Some(detected), source)
    } else {
        (None, SdkSource::Unset)
    };
    let dir_is_dir = dir.as_ref().is_some_and(|dir| dir.is_dir());
    // Borrow the path, not the Option, so `dir` can move into the report;
    // the probe closures must be finished before the struct is built.
    let dir_path = dir.as_deref();
    let probe = |names: &[&str]| dir_path.map(|dir| probe_any(dir, names)).unwrap_or(false);
    let dlss = probe(&DLSS_SR_NAMES);
    let dlss_frame_generation = probe(&DLSS_FG_NAMES);
    let streamline = probe(&STREAMLINE_NAMES);
    let fsr_loader = probe(&FSR_LOADER_NAMES);
    let fsr_upscale = probe(&FSR_UPSCALE_NAMES);
    let fsr_frame_generation = probe(&FSR_FG_NAMES);
    let xess = probe(&XESS_NAMES);
    let xess_frame_generation = probe(&XESS_FG_NAMES);
    let xell = probe(&XELL_NAMES);
    SdkReport {
        dir,
        source,
        dir_is_dir,
        dlss,
        dlss_frame_generation,
        streamline,
        fsr_loader,
        fsr_upscale,
        fsr_frame_generation,
        xess,
        xess_frame_generation,
        xell,
    }
}

impl SdkReport {
    /// One-line human summary for the UI and the diagnostics.
    pub fn summary(&self) -> String {
        let mut found: Vec<&str> = Vec::new();
        if self.dlss {
            found.push("DLSS SR");
        }
        if self.dlss_frame_generation {
            found.push("DLSS FG");
        }
        if self.streamline {
            found.push("Streamline");
        }
        if self.fsr_loader {
            found.push("FSR loader");
        }
        if self.fsr_upscale {
            found.push("FSR upscaler");
        }
        if self.fsr_frame_generation {
            found.push("FSR FG");
        }
        if self.xess {
            found.push("XeSS");
        }
        if self.xess_frame_generation {
            found.push("XeSS FG");
        }
        if self.xell {
            found.push("XeLL");
        }
        match &self.dir {
            Some(dir) if !self.dir_is_dir => format!(
                "SDK folder {} (from {}) does not exist",
                dir.display(),
                self.source.label()
            ),
            Some(dir) => format!(
                "SDK folder {} (from {}): {}",
                dir.display(),
                self.source.label(),
                if found.is_empty() {
                    "no recognised DLLs".to_string()
                } else {
                    found.join(", ")
                }
            ),
            None => {
                "no SDK folder configured (set PW_UPSCALER_SDK_DIR or upscale_sdk_dir)".to_string()
            }
        }
    }
}

/// The upscaler chosen in Settings → Graphics → "Upscaling". Persisted with
/// [`UpscaleMethod::token`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpscaleMethod {
    #[default]
    Off,
    Dlss,
    Fsr1,
    Fsr2,
    Fsr3,
    Fsr4,
    Xess,
}

impl UpscaleMethod {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Dlss,
        Self::Fsr1,
        Self::Fsr2,
        Self::Fsr3,
        Self::Fsr4,
        Self::Xess,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Dlss => "DLSS Super Resolution",
            Self::Fsr1 => "FSR 1 (EASU + RCAS)",
            Self::Fsr2 => "FSR 2 (FFX temporal)",
            Self::Fsr3 => "FSR 3.1 (FFX temporal)",
            Self::Fsr4 => "FSR 4.1 (FFX ML)",
            Self::Xess => "XeSS",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dlss => "dlss",
            Self::Fsr1 => "fsr1",
            Self::Fsr2 => "fsr2",
            Self::Fsr3 => "fsr3",
            Self::Fsr4 => "fsr4",
            Self::Xess => "xess",
        }
    }

    /// `fsr` is deliberately rejected: settings written by earlier builds
    /// carry it together with `fsr_version`, and the caller (settings
    /// decode) resolves that legacy pair explicitly so the user's saved
    /// choice is honoured instead of silently becoming the newest provider.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "dlss" => Some(Self::Dlss),
            "fsr1" | "fsr 1" | "fsr-1" => Some(Self::Fsr1),
            "fsr2" | "fsr 2" | "fsr-2" => Some(Self::Fsr2),
            "fsr3" | "fsr 3" | "fsr-3" => Some(Self::Fsr3),
            "fsr4" | "fsr 4" | "fsr-4" | "fsr4.1" | "fsr 4.1" => Some(Self::Fsr4),
            "xess" => Some(Self::Xess),
            _ => None,
        }
    }

    /// FFX provider major version requested from the loader, or `None` for
    /// the FSR 1 spatial path (no FFX context is involved).
    pub fn ffx_major(self) -> Option<u32> {
        match self {
            Self::Fsr2 => Some(2),
            Self::Fsr3 => Some(3),
            Self::Fsr4 => Some(4),
            _ => None,
        }
    }

    /// True for the three FFX SDK providers (FSR 2/3/4).
    pub fn is_ffx_fsr(self) -> bool {
        matches!(self, Self::Fsr2 | Self::Fsr3 | Self::Fsr4)
    }
}

/// The frame generator chosen in Settings → Graphics → "Frame generation".
/// Independent of the upscaler; see [`pairing`] for the legal combinations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameGenMethod {
    #[default]
    Off,
    Dlss,
    Fsr,
    Xess,
}

impl FrameGenMethod {
    pub const ALL: [Self; 4] = [Self::Off, Self::Dlss, Self::Fsr, Self::Xess];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Dlss => "DLSS Frame Generation",
            Self::Fsr => "FSR Frame Generation",
            Self::Xess => "XeSS Frame Generation",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dlss => "dlss",
            Self::Fsr => "fsr",
            Self::Xess => "xess",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "dlss" => Some(Self::Dlss),
            "fsr" => Some(Self::Fsr),
            "xess" => Some(Self::Xess),
            _ => None,
        }
    }
}

/// The pacing strategy chosen in Settings → Graphics → **Latency**.
///
/// The two options are exclusive because they answer the same question — who
/// decides when this frame's CPU work and its present happen:
///
/// * [`LatencyMode::Vsync`] — the display paces presents and nothing else is
///   involved (wgpu `PresentMode::Fifo`). This is what every build before this
///   setting did, so it is the default.
/// * [`LatencyMode::Xell`] — Intel Xe Low Latency (`libxell.dll`) decides:
///   `xellSleep` holds the CPU back until the GPU queue is nearly empty and the
///   per-frame markers report simulation / render-submit / present timing, so
///   the frame is submitted as late as possible without losing throughput.
///   Present stays vsync'd with sync interval 1 — Intel requires exactly that
///   while low-latency mode is on — so this is a latency improvement *relative
///   to VSync*, not a replacement for it.
///
/// XeLL is not tied to XeSS: it works with any upscaler, with frame generation
/// and with Upscaling Off. The one hard constraint is that XeSS Frame
/// Generation needs its own XeLL context, and Intel's cross-vendor rule is that
/// a non-Intel GPU only gets XeLL latency reduction while XeSS-FG is running;
/// [`support_latency`] and [`latency_owner`] encode both rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LatencyMode {
    /// Vsync'd present, no latency library (the previous behaviour).
    #[default]
    Vsync,
    /// Intel Xe Low Latency: xellSleep + per-frame markers over a vsync'd
    /// present with sync interval 1.
    Xell,
    /// NVIDIA-style low latency without Streamline: the CPU is held back so a
    /// frame is submitted just before the display is ready for it, instead of a
    /// frame or two early. Decoupled from frame generation and from the
    /// upscaler, and it needs no vendor library.
    Reflex,
}

impl LatencyMode {
    pub const ALL: [Self; 3] = [Self::Vsync, Self::Reflex, Self::Xell];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vsync => "VSync",
            Self::Reflex => "Reflex (decoupled CPU pacing)",
            Self::Xell => "XeLL (Intel Xe Low Latency)",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Vsync => "vsync",
            Self::Reflex => "reflex",
            Self::Xell => "xell",
        }
    }

    /// Persisted spelling; anything else keeps the default instead of guessing.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "vsync" | "vsync (on)" => Some(Self::Vsync),
            "reflex" | "reflex (decoupled)" => Some(Self::Reflex),
            "xell" | "xell (low latency)" => Some(Self::Xell),
            _ => None,
        }
    }

    /// Present mode this strategy asks the swapchain for.
    ///
    /// Both are `Fifo`: XeLL's latency reduction only engages when DXGI is
    /// called with sync interval 1, and `Immediate` (tearing) would trade the
    /// tearing away for the very staleness a latency setting is supposed to
    /// remove. The choice is still expressed here, and applied to the swapchain
    /// whenever it changes, so the setting is wired rather than decorative.
    pub fn present_mode(self) -> wgpu::PresentMode {
        match self {
            // Reflex only moves the CPU wake-up, XeLL needs sync interval 1;
            // all three latency modes present with vsync.
            Self::Vsync | Self::Reflex | Self::Xell => wgpu::PresentMode::Fifo,
        }
    }
}

/// Who owns the XeLL context for this session. XeLL allows one context per
/// device/window, so the two owners below are mutually exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyOwner {
    /// No latency library; the swapchain paces presents.
    Vsync,
    /// The viewer's own session in [`crate::latency`], created for the
    /// Graphics → Latency → XeLL mode.
    Independent,
    /// XeSS Frame Generation creates XeLL itself (it refuses to initialise its
    /// proxy swapchain without one), so it is the owner and the viewer must not
    /// create a second context.
    FrameGeneration,
}

/// Decide who runs XeLL for this settings pair. Pure; unit-tested.
pub fn latency_owner(mode: LatencyMode, frame_generation: FrameGenMethod) -> LatencyOwner {
    match mode {
        LatencyMode::Vsync => LatencyOwner::Vsync,
        // Reflex-style pacing is the viewer's own: no vendor library, and no
        // interaction with the frame generator's own present queue.
        LatencyMode::Reflex => LatencyOwner::Independent,
        LatencyMode::Xell => match frame_generation {
            FrameGenMethod::Xess => LatencyOwner::FrameGeneration,
            _ => LatencyOwner::Independent,
        },
    }
}

/// How many frames the frame generator produces per rendered frame: one real
/// frame followed by `multiplier - 1` interpolated ones.
///
/// It is a separate setting from [`FrameGenMethod`] because the vendors differ
/// in what they can do:
///
/// * DLSS Frame Generation reports `DLSSG.MultiFrameCountMax` from the GPU
///   architecture — 1 (2x only) on Ada, up to 5 (6x) on Blackwell.
/// * XeSS Frame Generation has `xefgSwapChainSetNumInterpolatedFrames` and
///   reports its own maximum through `xefgSwapChainD3D12GetProperties`; the
///   viewer asks for the setting and the runtime clamps to what it supports.
/// * FSR Frame Generation interpolates exactly one frame per present
///   (`ffxDispatchDescFrameGeneration`), so it is 2x only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameMultiplier {
    #[default]
    X2,
    X3,
    X4,
    X6,
}

impl FrameMultiplier {
    pub const ALL: [Self; 4] = [Self::X2, Self::X3, Self::X4, Self::X6];

    /// Presented frames per rendered frame.
    pub fn multiplier(self) -> u32 {
        match self {
            Self::X2 => 2,
            Self::X3 => 3,
            Self::X4 => 4,
            Self::X6 => 6,
        }
    }

    /// Frames the generator has to interpolate per rendered frame.
    pub fn generated(self) -> u32 {
        self.multiplier() - 1
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::X2 => "x2 (1 generated frame)",
            Self::X3 => "x3 (2 generated frames)",
            Self::X4 => "x4 (3 generated frames)",
            Self::X6 => "x6 (5 generated frames)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::X2 => "x2",
            Self::X3 => "x3",
            Self::X4 => "x4",
            Self::X6 => "x6",
        }
    }

    /// Persisted spelling.
    pub fn token(self) -> &'static str {
        match self {
            Self::X2 => "2",
            Self::X3 => "3",
            Self::X4 => "4",
            Self::X6 => "6",
        }
    }

    /// Accepted spellings; anything else keeps the default instead of guessing.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "2" | "x2" | "2x" => Some(Self::X2),
            "3" | "x3" | "3x" => Some(Self::X3),
            "4" | "x4" | "4x" => Some(Self::X4),
            "6" | "x6" | "6x" => Some(Self::X6),
            _ => None,
        }
    }
}

/// Whether `method` can present `multiplier` frames per rendered frame.
///
/// 2x is whatever the frame generation gate already allows; the higher
/// multipliers are vendor- and GPU-specific, so each one carries its own
/// reason when it is not available.
pub fn multiplier_support(
    method: FrameGenMethod,
    multiplier: FrameMultiplier,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> Support {
    if multiplier == FrameMultiplier::X2 {
        return Support::Available("every frame generator interpolates one frame per present");
    }
    let base = support_framegen(method, UpscaleMethod::Off, gpu, sdk);
    match method {
        FrameGenMethod::Off => Support::Disabled("frame generation is off".to_string()),
        FrameGenMethod::Fsr => Support::Disabled(
            "AMD's FSR frame generation interpolates exactly one frame per present (2x)"
                .to_string(),
        ),
        FrameGenMethod::Dlss => {
            if !base.is_available() {
                return base;
            }
            if !GpuFacts::supports_dlss_multi_frame_generation(&gpu.name) {
                Support::Disabled(format!(
                    "DLSS Multi Frame Generation needs an RTX 50-series (Blackwell) GPU; on this GPU the runtime reports one generated frame (detected: {})",
                    gpu.name
                ))
            } else {
                Support::Available("DLSS Multi Frame Generation (DLSSG.MultiFrameCount)")
            }
        }
        FrameGenMethod::Xess => {
            if !base.is_available() {
                return base;
            }
            Support::Available(
                "XeSS Frame Generation sets the count through xefgSwapChainSetNumInterpolatedFrames; the runtime clamps to its own maximum (logged)",
            )
        }
    }
}

/// Hardware + SDK gate for one latency strategy. VSync is always available.
///
/// XeLL itself — the library, the per-frame markers, `xellSleep` — loads and
/// runs on every vendor; only the driver-side latency *reduction* is gated by
/// Intel. In the XeLL developer guide that reduction on a third-party GPU is
/// enabled only while XeSS Frame Generation is active ("application must
/// ensure that XeSS-FG is enabled if running in cross-vendor mode"). The
/// option therefore stays selectable on NVIDIA/AMD hardware — the user asked
/// for it, and a session that starts and reports markers is still useful — but
/// the reason string says plainly what the driver will and will not do, and
/// the HUD badge reports a failed session if `xellSleep` is refused at
/// runtime.
pub fn support_latency(
    mode: LatencyMode,
    frame_generation: FrameGenMethod,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> Support {
    match mode {
        LatencyMode::Vsync => Support::Available("vsync'd present, no latency library"),
        LatencyMode::Reflex => {
            if gpu.cpu_fallback {
                Support::Disabled("Requires a real GPU (software adapter detected)".to_string())
            } else {
                // No SDK and no vendor check: the pacer only needs a present
                // cadence to lock onto, so it works on every GPU, with or
                // without frame generation, and with any upscaler.
                Support::Available(
                    "CPU pacing to the present cadence (NVIDIA's Reflex algorithm, no Streamline)",
                )
            }
        }
        LatencyMode::Xell => {
            if !cfg!(windows) {
                Support::Disabled(
                    "XeLL needs a Windows DX12 flip-model swapchain (this build is not Windows)"
                        .to_string(),
                )
            } else if gpu.cpu_fallback {
                Support::Disabled("Requires a real GPU (software adapter detected)".to_string())
            } else if !sdk.xell {
                Support::Disabled(sdk_reason(sdk, "XeLL SDK (libxell.dll)"))
            } else {
                match latency_owner(mode, frame_generation) {
                    LatencyOwner::FrameGeneration => Support::Available(
                        "XeLL runs through XeSS Frame Generation's own context (latency reduction included)",
                    ),
                    LatencyOwner::Independent => {
                        if gpu.is_intel() {
                            Support::Available(
                                "XeLL found (xellSleep + per-frame markers; present stays vsync'd)",
                            )
                        } else {
                            // Selectable by request: the session still runs and
                            // reports markers. Intel only turns the sleep hint
                            // into real latency reduction on a non-Intel GPU
                            // while XeSS Frame Generation owns the context, and
                            // the note says so instead of greying the option.
                            Support::Available(
                                "XeLL found; on a non-Intel GPU Intel applies its latency reduction only while XeSS Frame Generation is active — the session still runs xellSleep + per-frame markers",
                            )
                        }
                    }
                    LatencyOwner::Vsync => Support::Available("vsync'd present"),
                }
            }
        }
    }
}

/// Quality mode for the upscaler, in portable terms. What each provider makes
/// of it lives in [`modes`] — the same word does not mean the same thing
/// everywhere:
///
/// * `NativeAa` is "render at 1.0× and only run the provider's temporal
///   antialiasing": DLAA on DLSS (`PerfQualityValue 5`), `Native AA` on FSR
///   3.1/4 (`FFX_UPSCALE_QUALITY_MODE_NATIVEAA`) and on XeSS
///   (`XESS_QUALITY_SETTING_AA`). FSR 1 is spatial and FSR 2 predates the enum
///   value, so neither offers it.
/// * `UltraQuality` is 1.3×. XeSS has it (`XESS_QUALITY_SETTING_ULTRA_QUALITY`);
///   DLSS does not (the NGX core rejects `PerfQualityValue 4`) and the FFX
///   upscaler has no such preset, so only XeSS lists it.
/// * `Quality`/`Balanced`/`Performance`/`UltraPerformance` are 1.5×/1.7×/2.0×/
///   3.0× everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpscaleQuality {
    NativeAa,
    UltraQuality,
    #[default]
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}

impl UpscaleQuality {
    pub const ALL: [Self; 6] = [
        Self::NativeAa,
        Self::UltraQuality,
        Self::Quality,
        Self::Balanced,
        Self::Performance,
        Self::UltraPerformance,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NativeAa => "Native AA (1.0× render)",
            Self::UltraQuality => "Ultra Quality (1.3× render)",
            Self::Quality => "Quality",
            Self::Balanced => "Balanced",
            Self::Performance => "Performance",
            Self::UltraPerformance => "Ultra Performance",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::NativeAa => "nativeaa",
            Self::UltraQuality => "ultraquality",
            Self::Quality => "quality",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
            Self::UltraPerformance => "ultraperformance",
        }
    }

    /// The name the *provider* gives this mode — "DLAA" on DLSS, "Native AA"
    /// on FSR 3.1/4 and XeSS, "Ultra Quality" on XeSS — falling back to the
    /// portable name when the provider does not offer it.
    pub fn label_for(self, method: UpscaleMethod) -> &'static str {
        modes::find(method, self)
            .map(|mode| mode.name)
            .unwrap_or_else(|| self.label())
    }

    /// `None` when `method` offers this mode, otherwise the reason it does not
    /// (the UI shows it as a disabled entry, the diagnostics print it).
    pub fn availability_note(self, method: UpscaleMethod) -> Option<String> {
        if quality_available(method, self) {
            None
        } else {
            Some(match modes::rejection(method, self) {
                Some(reason) => {
                    format!(
                        "{} does not offer {}: {reason}",
                        method.label(),
                        self.label()
                    )
                }
                None => format!("{} does not offer {}", method.label(), self.label()),
            })
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "nativeaa" | "native_aa" | "native aa" => Some(Self::NativeAa),
            "ultraquality" | "ultra_quality" => Some(Self::UltraQuality),
            "quality" => Some(Self::Quality),
            "balanced" => Some(Self::Balanced),
            "performance" => Some(Self::Performance),
            "ultraperformance" | "ultra_performance" => Some(Self::UltraPerformance),
            _ => None,
        }
    }
}

/// Re-exported from [`modes`], which owns the provider × mode table.
///
/// Quality modes are **not** one shared list: DLSS offers DLAA and the four
/// upscaling modes, the FFX providers offer Native AA plus the four upscaling
/// modes and no Ultra Quality, XeSS offers Native AA *and* Ultra Quality (plus
/// the rest), FSR 1 offers the upscaling modes only. `modes::modes` is the
/// list, `modes::require` is what the providers call at create time, and
/// `modes::nearest_available` is how a settings value that outlived its
/// provider degrades.
pub use modes::{internal_factor, nearest_available, quality_available};

/// The support verdict for one provider on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Available(&'static str),
    Disabled(String),
}

impl Support {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// The disabled reason, if any — what the UI shows next to the option.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available(_) => None,
            Self::Disabled(reason) => Some(reason),
        }
    }

    /// The explanation for this option whether it is available or not: a
    /// disabled reason, or the caveat that comes with an available option
    /// (XeLL on a non-Intel GPU, for instance).
    pub fn note(&self) -> Option<&str> {
        match self {
            Self::Available(note) if !note.is_empty() => Some(note),
            Self::Available(_) => None,
            Self::Disabled(reason) => Some(reason),
        }
    }
}

fn sdk_reason(sdk: &SdkReport, what: &str) -> String {
    match (&sdk.dir, sdk.dir_is_dir) {
        (Some(dir), false) => format!(
            "SDK folder {} (from {}) does not exist",
            dir.display(),
            sdk.source.label()
        ),
        (Some(dir), true) => format!(
            "{what} not found in {} (from {})",
            dir.display(),
            sdk.source.label()
        ),
        (None, _) => {
            format!("{what} not found; no SDK folder configured (set PW_UPSCALER_SDK_DIR)")
        }
    }
}

/// Name-based gate for the RDNA 4 generation that FSR 4's ML provider
/// targets (RX 9060/9070 family, desktop and mobile). AMD exposes no
/// feature-query API in the in-tree FFX headers, so this is deliberately
/// conservative: anything ambiguous is reported as unsupported, and a false
/// positive still fails safe at context creation (the frame loop falls back
/// to bilinear with a log line).
pub fn is_rdna4(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("rx 90") || name.contains("rx90") || name.contains("radeon 90")
}

/// EASU is specified for up to 4x area upscaling (at most 2x per axis), not
/// downsampling. Outside that range, or if clamping to the device limit
/// makes even one input axis larger than the output, feed bilinear instead.
pub fn fsr1_eligible(input: (u32, u32), output: (u32, u32)) -> bool {
    input.0 > 0
        && input.1 > 0
        && output.0 > 0
        && output.1 > 0
        && input.0 <= output.0
        && input.1 <= output.1
        && u64::from(input.0) * 2 >= u64::from(output.0)
        && u64::from(input.1) * 2 >= u64::from(output.1)
        && input != output
}

/// Hardware + SDK gate for one upscaler. `Off` is always available.
pub fn support(method: UpscaleMethod, gpu: &GpuFacts, sdk: &SdkReport) -> Support {
    match method {
        UpscaleMethod::Off => Support::Available("always available"),
        UpscaleMethod::Dlss => {
            if gpu.cpu_fallback {
                Support::Disabled("Requires an NVIDIA RTX GPU (software adapter detected)".into())
            } else if !gpu.is_nvidia() {
                Support::Disabled(format!(
                    "Requires an NVIDIA RTX GPU (detected: {})",
                    gpu.name
                ))
            } else if !sdk.dlss {
                Support::Disabled(sdk_reason(
                    sdk,
                    "DLSS Super Resolution SDK (nvngx_dlss.dll)",
                ))
            } else {
                Support::Available("DLSS SDK found")
            }
        }
        UpscaleMethod::Fsr1 => {
            // FSR 1 is a spatial EASU+RCAS fragment pass, not an FFX
            // context; it ships in-tree and needs no vendor SDK.
            if gpu.cpu_fallback {
                Support::Disabled("Requires a real GPU (software adapter detected)".into())
            } else {
                Support::Available("FSR 1 EASU + RCAS, in-tree (AMD, NVIDIA and Intel GPUs)")
            }
        }
        UpscaleMethod::Fsr2 | UpscaleMethod::Fsr3 | UpscaleMethod::Fsr4 => {
            let sdk_missing = if gpu.cpu_fallback {
                Some("Requires a real GPU (software adapter detected)".to_string())
            } else if !sdk.fsr_upscale {
                Some(sdk_reason(
                    sdk,
                    "FSR SDK (amd_fidelityfx_upscaler_dx12.dll)",
                ))
            } else if !sdk.fsr_loader {
                Some(sdk_reason(
                    sdk,
                    "FSR loader (amd_fidelityfx_loader_dx12.dll)",
                ))
            } else {
                None
            };
            if let Some(reason) = sdk_missing {
                Support::Disabled(reason)
            } else {
                match method {
                    UpscaleMethod::Fsr2 => Support::Available(
                        "FSR 2 (provider 2.3.4) via FFX — AMD, NVIDIA and Intel GPUs",
                    ),
                    UpscaleMethod::Fsr3 => Support::Available(
                        "FSR 3.1 (provider 3.1.6) via FFX — AMD, NVIDIA and Intel GPUs",
                    ),
                    UpscaleMethod::Fsr4 => {
                        // FSR 4's ML path is only exposed on RDNA 4
                        // (RX 9000 series); AMD hardware below that falls
                        // back to bilinear at context creation, but the
                        // option is marked unavailable up front instead.
                        if !gpu.is_amd() {
                            Support::Disabled(format!(
                                "FSR 4 ML requires an AMD Radeon RX 9000 series GPU (detected: {})",
                                gpu.name
                            ))
                        } else if !is_rdna4(&gpu.name) {
                            Support::Disabled(format!(
                                "FSR 4 ML requires an AMD Radeon RX 9000 series GPU (detected: {})",
                                gpu.name
                            ))
                        } else {
                            Support::Available("FSR 4.1 (provider 4.1.2) via FFX on RDNA 4")
                        }
                    }
                    _ => unreachable!("matched above"),
                }
            }
        }
        UpscaleMethod::Xess => {
            if gpu.cpu_fallback {
                Support::Disabled(
                    "Requires an Intel Arc or DP4a-capable GPU (software adapter detected)".into(),
                )
            } else if !sdk.xess {
                Support::Disabled(sdk_reason(sdk, "XeSS SDK (libxess.dll)"))
            } else if gpu.is_intel() {
                Support::Available("XeSS SDK found (Intel Arc)")
            } else {
                Support::Available("XeSS SDK found (DP4a fallback path; best results on Intel Arc)")
            }
        }
    }
}

/// Hardware + SDK gate for one frame generator. `Off` is always available.
/// Frame generation is independent of the upscaler; see [`pairing`].
pub fn support_framegen(
    method: FrameGenMethod,
    upscale: UpscaleMethod,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> Support {
    match method {
        FrameGenMethod::Off => Support::Available("always available"),
        FrameGenMethod::Dlss => {
            if gpu.cpu_fallback {
                Support::Disabled(
                    "Requires an NVIDIA RTX 40 series GPU (software adapter detected)".into(),
                )
            } else if !gpu.is_nvidia() {
                Support::Disabled(format!(
                    "Requires an NVIDIA RTX 40 series or later GPU (detected: {})",
                    gpu.name
                ))
            } else if !GpuFacts::supports_dlss_frame_generation(&gpu.name) {
                Support::Disabled(format!(
                    "Requires NVIDIA RTX 40 series or later (detected: {})",
                    gpu.name
                ))
            } else if !sdk.dlss_frame_generation {
                Support::Disabled(sdk_reason(
                    sdk,
                    "DLSS Frame Generation SDK (nvngx_dlssg.dll)",
                ))
            } else {
                Support::Available("DLSS Frame Generation SDK found")
            }
        }
        FrameGenMethod::Fsr => {
            if gpu.cpu_fallback {
                Support::Disabled("Requires a real GPU (software adapter detected)".into())
            } else if !sdk.fsr_frame_generation {
                Support::Disabled(sdk_reason(
                    sdk,
                    "FSR Frame Generation SDK (amd_fidelityfx_framegeneration_dx12.dll)",
                ))
            } else if !sdk.fsr_loader {
                Support::Disabled(sdk_reason(
                    sdk,
                    "FSR loader (amd_fidelityfx_loader_dx12.dll)",
                ))
            } else {
                let _ = upscale;
                Support::Available("FSR Frame Generation SDK found (upscaler-agnostic per AMD)")
            }
        }
        FrameGenMethod::Xess => {
            if gpu.cpu_fallback {
                Support::Disabled("Requires an Intel Arc GPU (software adapter detected)".into())
            } else if matches!(
                pairing(upscale, FrameGenMethod::Xess),
                PairingVerdict::Unsupported(_)
            ) {
                Support::Disabled(
                    "XeSS Frame Generation is exposed with XeSS upscaling or Off only".into(),
                )
            } else if !sdk.xess_frame_generation || !sdk.xell {
                Support::Disabled(sdk_reason(
                    sdk,
                    "XeSS 2 Frame Generation SDK (libxess_fg.dll and libxell.dll)",
                ))
            } else {
                Support::Available(
                    "XeSS 2 Frame Generation SDK found (XeLL latency reduction included)",
                )
            }
        }
    }
}

/// Verdict on combining an upscaler with a frame generator. Based on the
/// vendors' documentation as of the knowledge available when this was written;
/// **not verified against the SDKs**, which cannot be downloaded here. Unverified
/// or undocumented combinations are rejected rather than faked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingVerdict {
    Supported,
    Unsupported(&'static str),
}

impl PairingVerdict {
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// The pairing table. DLSS Frame Generation (Streamline `sl.dlss_g`) and FSR 3
/// Frame Generation are both documented by their vendors as usable with other
/// temporal upscalers; XeSS Frame Generation is only documented for use with
/// XeSS itself. Cross-vendor XeSS pairings are therefore rejected.
pub fn pairing(upscale: UpscaleMethod, frame_generation: FrameGenMethod) -> PairingVerdict {
    match frame_generation {
        FrameGenMethod::Off => PairingVerdict::Supported,
        FrameGenMethod::Dlss => PairingVerdict::Supported,
        FrameGenMethod::Fsr => PairingVerdict::Supported,
        FrameGenMethod::Xess => match upscale {
            UpscaleMethod::Off | UpscaleMethod::Xess => PairingVerdict::Supported,
            UpscaleMethod::Dlss
            | UpscaleMethod::Fsr1
            | UpscaleMethod::Fsr2
            | UpscaleMethod::Fsr3
            | UpscaleMethod::Fsr4 => PairingVerdict::Unsupported(
                "Intel documents XeSS Frame Generation for use with XeSS only",
            ),
        },
    }
}

/// The name of the vendor field a mode's `vendor` value is written to, for the
/// diagnostics print. FSR 1's EASU takes a scale factor and has no enum.
fn vendor_parameter_name(method: UpscaleMethod) -> &'static str {
    match method {
        UpscaleMethod::Dlss => "PerfQualityValue",
        UpscaleMethod::Fsr2 | UpscaleMethod::Fsr3 | UpscaleMethod::Fsr4 => "FFX qualityMode",
        UpscaleMethod::Xess => "XeSS qualitySetting",
        UpscaleMethod::Fsr1 | UpscaleMethod::Off => "(extent only)",
    }
}

/// Coerce a persisted triple into a legal one, dropping only values this
/// build cannot honour. Used at settings-decode time; never rewrites what the
/// user sees beyond these rules.
pub fn sanitise(
    upscale: UpscaleMethod,
    quality: UpscaleQuality,
    frame_generation: FrameGenMethod,
) -> (UpscaleMethod, UpscaleQuality, FrameGenMethod) {
    // A mode the selected provider does not advertise degrades to the nearest
    // ratio it *does* advertise (XeSS "Ultra Quality" -> Quality on DLSS,
    // "Native AA" -> Quality on FSR 2). Never drop it to a fixed default: that
    // is how a 1.0x request could turn into a 1.5x render with no trace.
    let quality = nearest_available(upscale, quality);
    let frame_generation = if pairing(upscale, frame_generation).is_supported() {
        frame_generation
    } else {
        FrameGenMethod::Off
    };
    (upscale, quality, frame_generation)
}

/// The provider the settings ask for, if hardware and SDK allow it.
pub fn resolve_active(
    upscale: UpscaleMethod,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> Option<UpscaleMethod> {
    (upscale != UpscaleMethod::Off && support(upscale, gpu, sdk).is_available()).then_some(upscale)
}

/// Recommended internal render extent for `(w, h)` output. Pure; used by the
/// frame loop and by the diagnostics.
pub fn provider_extent(
    upscale: UpscaleMethod,
    quality: UpscaleQuality,
    w: u32,
    h: u32,
    max_dimension: u32,
) -> (u32, u32) {
    let factor = internal_factor(upscale, quality);
    let limit = u64::from(max_dimension.max(1));
    let scale = |v: u32| ((f64::from(v.max(1)) * factor).round() as u64).clamp(1, limit) as u32;
    (scale(w), scale(h))
}

/// The extent the world should render at. `off_extent` is the extent the
/// resolution-scale slider resolves to (computed by the caller with the
/// existing `render_scale::scaled_extent`, which this module does not depend
/// on so it stays headless-testable).
pub fn render_extent(
    off_extent: (u32, u32),
    output: (u32, u32),
    max_dimension: u32,
    upscale: UpscaleMethod,
    quality: UpscaleQuality,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> (u32, u32) {
    match resolve_active(upscale, gpu, sdk) {
        Some(active) => provider_extent(active, quality, output.0, output.1, max_dimension),
        None => off_extent,
    }
}

/// The frame generator the settings ask for, if hardware, SDK and pairing
/// allow it.
pub fn resolve_active_framegen(
    upscale: UpscaleMethod,
    frame_generation: FrameGenMethod,
    gpu: &GpuFacts,
    sdk: &SdkReport,
) -> Option<FrameGenMethod> {
    let paired = pairing(upscale, frame_generation).is_supported();
    (frame_generation != FrameGenMethod::Off
        && paired
        && support_framegen(frame_generation, upscale, gpu, sdk).is_available())
    .then_some(frame_generation)
}

/// Radical inverse for Halton (2,3) jitter. `index` starts at 1 for frame 0.
pub fn halton(index: u32, base: u32) -> f32 {
    let mut f = 1.0f64;
    let mut r = 0.0f64;
    let mut i = index.max(1);
    while i > 0 {
        f /= f64::from(base);
        r += f * f64::from(i % base);
        i /= base;
    }
    r as f32
}

/// Length of the temporal jitter sequence each provider is configured with.
/// DLSS uses a fixed 64-frame Halton-like sequence; FSR 2/3 and XeSS both
/// document Halton(2,3) with period 8/16 — 64 is a superset that satisfies
/// all of them.
pub const JITTER_PERIOD: u32 = 64;

/// Subpixel jitter for `frame_index`, in pixel units centred on zero
/// (i.e. add it to the pixel-centre offset, or scale by `1/extent` for clip
/// space). Deterministic; identical for the same frame index.
pub fn jitter_offset(frame_index: u32) -> (f32, f32) {
    let index = (frame_index % JITTER_PERIOD) + 1;
    (halton(index, 2) - 0.5, halton(index, 3) - 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Selecting a folder scans it and shows the card; only **Enter world**
    /// launches. The launcher used to arm a 1800 ms `SetTimer` after a scan and
    /// launch on `WM_TIMER`, so choosing a folder started the viewer by itself.
    /// The window procedure cannot be driven from a test, so this guards its
    /// source: it runs on every platform, which is also why it lives here and
    /// not beside the `#[cfg(windows)]` code it describes.
    #[test]
    fn selecting_a_folder_never_launches_on_a_timer() {
        let launcher = include_str!("windows_setup.rs");
        assert!(
            !launcher.contains("SetTimer"),
            "the launcher must not arm a timer after a folder scan"
        );
        assert!(
            !launcher.contains("0x113"),
            "WM_TIMER must not be handled, or an armed timer launches the viewer"
        );
        // The two deliberate launches stay: the button id, and the action it sets.
        assert!(launcher.contains("102 => LAUNCH_ACTION.store(2"));
        assert!(launcher.contains("2 => {"));
    }

    fn nvidia_rtx4070() -> GpuFacts {
        GpuFacts::from_parts(
            VENDOR_NVIDIA,
            0x2784,
            "NVIDIA GeForce RTX 4070",
            "dx12",
            false,
        )
    }

    fn nvidia_gtx1080() -> GpuFacts {
        GpuFacts::from_parts(
            VENDOR_NVIDIA,
            0x1b80,
            "NVIDIA GeForce GTX 1080",
            "dx12",
            false,
        )
    }

    fn amd_rx9070() -> GpuFacts {
        GpuFacts::from_parts(VENDOR_AMD, 0x7550, "AMD Radeon RX 9070 XT", "dx12", false)
    }

    fn amd_rx7900xtx() -> GpuFacts {
        GpuFacts::from_parts(VENDOR_AMD, 0x744c, "AMD Radeon RX 7900 XTX", "dx12", false)
    }

    fn intel_arc_a770() -> GpuFacts {
        GpuFacts::from_parts(
            VENDOR_INTEL,
            0x56a0,
            "Intel Arc A770 Graphics",
            "dx12",
            false,
        )
    }

    fn software() -> GpuFacts {
        GpuFacts::from_parts(0xffff, 0, "llvmpipe (LLVM 15.0.7)", "vulkan", true)
    }

    fn no_sdk() -> SdkReport {
        SdkReport::default()
    }

    fn full_sdk() -> SdkReport {
        SdkReport {
            dir: Some(PathBuf::from("C:/sdks/vendor")),
            source: SdkSource::Settings,
            dir_is_dir: true,
            dlss: true,
            dlss_frame_generation: true,
            streamline: true,
            fsr_loader: true,
            fsr_upscale: true,
            fsr_frame_generation: true,
            xess: true,
            xess_frame_generation: true,
            xell: true,
        }
    }

    #[test]
    fn tokens_round_trip() {
        for method in UpscaleMethod::ALL {
            assert_eq!(UpscaleMethod::parse(method.token()), Some(method));
        }
        for method in FrameGenMethod::ALL {
            assert_eq!(FrameGenMethod::parse(method.token()), Some(method));
        }
        for quality in UpscaleQuality::ALL {
            assert_eq!(UpscaleQuality::parse(quality.token()), Some(quality));
        }
        assert_eq!(UpscaleMethod::parse("bogus"), None);
        assert_eq!(FrameGenMethod::parse(""), None);
        assert_eq!(
            UpscaleQuality::parse("ultra_quality"),
            Some(UpscaleQuality::UltraQuality)
        );
        assert_eq!(
            UpscaleQuality::parse("ULTRAPERFORMANCE"),
            Some(UpscaleQuality::UltraPerformance)
        );
    }

    #[test]
    fn off_is_always_available_everywhere() {
        for gpu in [nvidia_rtx4070(), amd_rx9070(), intel_arc_a770(), software()] {
            assert!(support(UpscaleMethod::Off, &gpu, &no_sdk()).is_available());
            let fg = support_framegen(FrameGenMethod::Off, UpscaleMethod::Off, &gpu, &no_sdk());
            assert!(fg.is_available());
        }
    }

    #[test]
    fn dlss_requires_nvidia_and_sdk() {
        assert_eq!(
            support(UpscaleMethod::Dlss, &amd_rx9070(), &full_sdk())
                .reason()
                .unwrap(),
            "Requires an NVIDIA RTX GPU (detected: AMD Radeon RX 9070 XT)"
        );
        assert_eq!(
            support(UpscaleMethod::Dlss, &software(), &full_sdk())
                .reason()
                .unwrap(),
            "Requires an NVIDIA RTX GPU (software adapter detected)"
        );
        assert!(support(UpscaleMethod::Dlss, &nvidia_rtx4070(), &no_sdk())
            .reason()
            .unwrap()
            .contains("nvngx_dlss.dll"));
        assert!(support(UpscaleMethod::Dlss, &nvidia_rtx4070(), &full_sdk()).is_available());
    }

    #[test]
    fn fsr1_is_in_tree_and_needs_no_sdk() {
        for gpu in [nvidia_rtx4070(), amd_rx9070(), intel_arc_a770()] {
            assert!(support(UpscaleMethod::Fsr1, &gpu, &no_sdk()).is_available());
        }
        assert!(support(UpscaleMethod::Fsr1, &software(), &no_sdk())
            .reason()
            .unwrap()
            .contains("software adapter"));
    }

    #[test]
    fn fsr2_and_fsr3_are_cross_vendor_but_need_the_sdk() {
        for gpu in [nvidia_rtx4070(), amd_rx9070(), intel_arc_a770()] {
            for method in [UpscaleMethod::Fsr2, UpscaleMethod::Fsr3] {
                assert!(support(method, &gpu, &no_sdk())
                    .reason()
                    .unwrap()
                    .contains("FSR SDK"));
                assert!(support(method, &gpu, &full_sdk()).is_available());
            }
        }
        for method in [UpscaleMethod::Fsr2, UpscaleMethod::Fsr3] {
            assert!(support(method, &software(), &full_sdk())
                .reason()
                .unwrap()
                .contains("software adapter"));
        }
    }

    #[test]
    fn fsr4_is_gated_to_rdna4() {
        let rdna4 =
            GpuFacts::from_parts(VENDOR_AMD, 0x744c, "AMD Radeon RX 9070 XT", "dx12", false);
        assert!(support(UpscaleMethod::Fsr4, &rdna4, &full_sdk()).is_available());
        // Pre-RDNA-4 AMD is disabled with a reason, not silently substituted.
        assert!(support(UpscaleMethod::Fsr4, &amd_rx7900xtx(), &full_sdk())
            .reason()
            .unwrap()
            .contains("RX 9000"));
        // Non-AMD is disabled too.
        assert!(support(UpscaleMethod::Fsr4, &nvidia_rtx4070(), &full_sdk())
            .reason()
            .unwrap()
            .contains("RX 9000"));
        // SDK still required on supported hardware.
        assert!(support(UpscaleMethod::Fsr4, &rdna4, &no_sdk())
            .reason()
            .unwrap()
            .contains("FSR SDK"));
    }

    #[test]
    fn fsr_tokens_parse_and_roundtrip() {
        for (method, token) in [
            (UpscaleMethod::Off, "off"),
            (UpscaleMethod::Fsr1, "fsr1"),
            (UpscaleMethod::Fsr2, "fsr2"),
            (UpscaleMethod::Fsr3, "fsr3"),
            (UpscaleMethod::Fsr4, "fsr4"),
        ] {
            assert_eq!(UpscaleMethod::parse(token), Some(method));
            assert_eq!(method.token(), token);
        }
        // The bare legacy token is rejected; the settings decoder resolves
        // `upscaling=fsr` + `fsr_version` explicitly (see view_settings).
        assert_eq!(UpscaleMethod::parse("fsr"), None);
        assert_eq!(UpscaleMethod::parse("FSR4.1"), Some(UpscaleMethod::Fsr4));
    }

    #[test]
    fn xess_needs_sdk_available_on_intel_and_elsewhere() {
        assert!(support(UpscaleMethod::Xess, &intel_arc_a770(), &no_sdk())
            .reason()
            .unwrap()
            .contains("XeSS SDK"));
        assert!(support(UpscaleMethod::Xess, &intel_arc_a770(), &full_sdk()).is_available());
        assert!(support(UpscaleMethod::Xess, &nvidia_rtx4070(), &full_sdk()).is_available());
    }

    #[test]
    fn dlss_frame_generation_requires_rtx_40_series() {
        let support = support_framegen(
            FrameGenMethod::Dlss,
            UpscaleMethod::Off,
            &nvidia_gtx1080(),
            &full_sdk(),
        );
        assert!(support.reason().unwrap().contains("40 series"));
        let support = support_framegen(
            FrameGenMethod::Dlss,
            UpscaleMethod::Dlss,
            &amd_rx9070(),
            &full_sdk(),
        );
        assert!(support.reason().unwrap().contains("NVIDIA"));
        let support = support_framegen(
            FrameGenMethod::Dlss,
            UpscaleMethod::Dlss,
            &nvidia_rtx4070(),
            &no_sdk(),
        );
        assert!(support.reason().unwrap().contains("nvngx_dlssg.dll"));
        assert!(support_framegen(
            FrameGenMethod::Dlss,
            UpscaleMethod::Dlss,
            &nvidia_rtx4070(),
            &full_sdk()
        )
        .is_available());
    }

    #[test]
    fn fsr_frame_generation_needs_fg_and_loader() {
        let mut sdk = no_sdk();
        sdk.fsr_frame_generation = true;
        assert!(support_framegen(
            FrameGenMethod::Fsr,
            UpscaleMethod::Fsr3,
            &amd_rx9070(),
            &sdk
        )
        .reason()
        .unwrap()
        .contains("amd_fidelityfx_loader_dx12.dll"));
        sdk.fsr_loader = true;
        assert!(support_framegen(
            FrameGenMethod::Fsr,
            UpscaleMethod::Dlss,
            &amd_rx9070(),
            &sdk
        )
        .is_available());
    }

    #[test]
    fn xess_frame_generation_pairing_gate() {
        let support = support_framegen(
            FrameGenMethod::Xess,
            UpscaleMethod::Dlss,
            &intel_arc_a770(),
            &full_sdk(),
        );
        assert!(support.reason().unwrap().contains("XeSS upscaling"));
        assert!(support_framegen(
            FrameGenMethod::Xess,
            UpscaleMethod::Xess,
            &intel_arc_a770(),
            &full_sdk()
        )
        .is_available());
    }

    /// The Latency setting: token round-trip, and the owner rule that decides
    /// whether the viewer creates its own XeLL context, hands the job to XeSS
    /// Frame Generation, or does nothing.
    #[test]
    fn latency_mode_tokens_and_xell_ownership() {
        // Default is the previous behaviour: vsync'd present, no library.
        assert_eq!(LatencyMode::default(), LatencyMode::Vsync);
        for mode in LatencyMode::ALL {
            assert_eq!(LatencyMode::parse(mode.token()), Some(mode));
            assert_eq!(LatencyMode::parse(&mode.token().to_uppercase()), Some(mode));
            // All three present on the display refresh.
            assert_eq!(mode.present_mode(), wgpu::PresentMode::Fifo);
        }
        // Unknown spellings keep the default instead of guessing.
        assert_eq!(LatencyMode::parse("reflexx"), None);
        assert_eq!(LatencyMode::parse(""), None);
        // The persisted key round-trips through the settings file (the
        // view_settings tests cover the file format itself).
        assert_eq!(LatencyMode::parse("xell"), Some(LatencyMode::Xell));
        assert_eq!(LatencyMode::parse("vsync"), Some(LatencyMode::Vsync));
        // VSync never involves XeLL, whatever frame generation is selected.
        for fg in FrameGenMethod::ALL {
            assert_eq!(latency_owner(LatencyMode::Vsync, fg), LatencyOwner::Vsync);
        }
        // XeLL is independent of the upscaler and of every frame generator
        // except the one that must own a context itself.
        for fg in [
            FrameGenMethod::Off,
            FrameGenMethod::Dlss,
            FrameGenMethod::Fsr,
        ] {
            assert_eq!(
                latency_owner(LatencyMode::Xell, fg),
                LatencyOwner::Independent
            );
        }
        assert_eq!(
            latency_owner(LatencyMode::Xell, FrameGenMethod::Xess),
            LatencyOwner::FrameGeneration
        );
    }

    /// The gate keeps XeLL selectable on every vendor, and reports the
    /// cross-vendor caveat instead of greying the option out.
    #[test]
    fn latency_support_gate() {
        assert!(support_latency(
            LatencyMode::Vsync,
            FrameGenMethod::Off,
            &amd_rx7900xtx(),
            &SdkReport::default()
        )
        .is_available());
        if !cfg!(windows) {
            // The platform gate is checked before the SDK probe, so off Windows
            // even a perfectly good Arc + SDK pair is disabled — and that is the
            // only thing testable here.
            let off_platform = support_latency(
                LatencyMode::Xell,
                FrameGenMethod::Off,
                &intel_arc_a770(),
                &full_sdk(),
            );
            assert!(!off_platform.is_available());
            assert!(off_platform.reason().unwrap().contains("Windows"));
            return;
        }
        // No libxell.dll -> disabled, and the reason names the file.
        let no_sdk = support_latency(
            LatencyMode::Xell,
            FrameGenMethod::Off,
            &intel_arc_a770(),
            &SdkReport::default(),
        );
        assert!(!no_sdk.is_available());
        assert!(no_sdk.reason().unwrap().contains("libxell.dll"));
        // Intel Arc + SDK + no frame generation: the viewer owns the context.
        assert!(support_latency(
            LatencyMode::Xell,
            FrameGenMethod::Off,
            &intel_arc_a770(),
            &full_sdk()
        )
        .is_available());
        // Non-Intel GPU without XeSS-FG: selectable, with Intel's cross-vendor
        // caveat spelled out.
        let cross = support_latency(
            LatencyMode::Xell,
            FrameGenMethod::Off,
            &amd_rx7900xtx(),
            &full_sdk(),
        );
        assert!(cross.is_available());
        let note = cross.note().unwrap();
        assert!(note.contains("non-Intel"));
        assert!(note.contains("XeSS Frame Generation"));
        // ... but with XeSS-FG selected it is available, through that context.
        assert!(support_latency(
            LatencyMode::Xell,
            FrameGenMethod::Xess,
            &amd_rx7900xtx(),
            &full_sdk()
        )
        .is_available());
    }

    #[test]
    fn pairing_table_legal_and_illegal() {
        for up in UpscaleMethod::ALL {
            assert!(pairing(up, FrameGenMethod::Off).is_supported());
            assert!(pairing(up, FrameGenMethod::Dlss).is_supported());
            assert!(pairing(up, FrameGenMethod::Fsr).is_supported());
        }
        assert!(pairing(UpscaleMethod::Xess, FrameGenMethod::Xess).is_supported());
        assert!(pairing(UpscaleMethod::Off, FrameGenMethod::Xess).is_supported());
        assert!(!pairing(UpscaleMethod::Dlss, FrameGenMethod::Xess).is_supported());
        for fsr in [
            UpscaleMethod::Fsr1,
            UpscaleMethod::Fsr2,
            UpscaleMethod::Fsr3,
            UpscaleMethod::Fsr4,
        ] {
            assert!(!pairing(fsr, FrameGenMethod::Xess).is_supported());
        }
    }

    #[test]
    fn sanitise_fixes_illegal_combinations() {
        // DLSS has no Ultra Quality mode (the NGX core rejects
        // PerfQualityValue 4); the saved choice degrades to the nearest ratio
        // DLSS does offer instead of being forced onto one fixed default.
        let (_, quality, _) = sanitise(
            UpscaleMethod::Dlss,
            UpscaleQuality::UltraQuality,
            FrameGenMethod::Off,
        );
        assert_eq!(quality, UpscaleQuality::Quality);
        // XeSS offers its whole ladder, Ultra Performance included (SDK 3.0.2).
        let (_, quality, _) = sanitise(
            UpscaleMethod::Xess,
            UpscaleQuality::UltraPerformance,
            FrameGenMethod::Off,
        );
        assert_eq!(quality, UpscaleQuality::UltraPerformance);
        // Cross-vendor XeSS frame generation is not exposed.
        let (_, _, fg) = sanitise(
            UpscaleMethod::Fsr2,
            UpscaleQuality::Quality,
            FrameGenMethod::Xess,
        );
        assert_eq!(fg, FrameGenMethod::Off);
        // Legal combinations survive untouched.
        let (u, q, f) = sanitise(
            UpscaleMethod::Dlss,
            UpscaleQuality::Performance,
            FrameGenMethod::Dlss,
        );
        assert_eq!(
            (u, q, f),
            (
                UpscaleMethod::Dlss,
                UpscaleQuality::Performance,
                FrameGenMethod::Dlss
            )
        );
    }

    #[test]
    fn ffx_major_maps_only_the_sdk_providers() {
        assert_eq!(UpscaleMethod::Off.ffx_major(), None);
        assert_eq!(UpscaleMethod::Fsr1.ffx_major(), None);
        assert_eq!(UpscaleMethod::Fsr2.ffx_major(), Some(2));
        assert_eq!(UpscaleMethod::Fsr3.ffx_major(), Some(3));
        assert_eq!(UpscaleMethod::Fsr4.ffx_major(), Some(4));
        assert_eq!(UpscaleMethod::Dlss.ffx_major(), None);
        assert_eq!(UpscaleMethod::Xess.ffx_major(), None);
        assert!(UpscaleMethod::Fsr2.is_ffx_fsr());
        assert!(UpscaleMethod::Fsr3.is_ffx_fsr());
        assert!(UpscaleMethod::Fsr4.is_ffx_fsr());
        assert!(!UpscaleMethod::Fsr1.is_ffx_fsr());
    }

    #[test]
    fn fsr1_eligible_bounds() {
        assert!(fsr1_eligible((960, 540), (1920, 1080)));
        assert!(fsr1_eligible((1, 1), (2, 2)));
        assert!(!fsr1_eligible((192, 108), (1920, 1080)));
        assert!(!fsr1_eligible((999, 540), (2000, 1080)));
        assert!(!fsr1_eligible((1920, 1080), (1920, 1080)));
        assert!(!fsr1_eligible((3840, 2160), (1920, 1080)));
        assert!(!fsr1_eligible((2048, 540), (1920, 1080)));
        assert!(!fsr1_eligible((0, 100), (1920, 1080)));
        assert!(!fsr1_eligible((960, 540), (0, 0)));
    }

    #[test]
    fn is_rdna4_matches_rx_9_models_only() {
        assert!(is_rdna4("AMD Radeon RX 9070 XT"));
        assert!(is_rdna4("AMD Radeon RX 9060"));
        assert!(is_rdna4("AMD Radeon RX 9010M XT"));
        assert!(!is_rdna4("AMD Radeon RX 7900 XTX"));
        assert!(!is_rdna4("NVIDIA GeForce RTX 4070"));
        assert!(!is_rdna4("Intel Arc A770"));
    }

    #[test]
    fn internal_factors_shrink_the_image_monotonically() {
        // Only the modes a provider actually advertises: an unadvertised mode
        // is not part of its ladder, and `modes::require` refuses it.
        for method in [
            UpscaleMethod::Dlss,
            UpscaleMethod::Fsr1,
            UpscaleMethod::Fsr2,
            UpscaleMethod::Fsr3,
            UpscaleMethod::Fsr4,
            UpscaleMethod::Xess,
        ] {
            let factors: Vec<f64> = modes::modes(method)
                .iter()
                .map(|mode| mode.factor)
                .collect();
            assert!(factors.iter().all(|factor| *factor > 0.0 && *factor <= 1.0));
            for pair in factors.windows(2) {
                assert!(
                    pair[0] >= pair[1],
                    "{method:?}: factor must not grow with lower quality"
                );
            }
            // The table and the extent helper agree, mode for mode.
            for mode in modes::modes(method) {
                assert_eq!(internal_factor(method, mode.quality), mode.factor);
            }
        }
        assert_eq!(
            internal_factor(UpscaleMethod::Off, UpscaleQuality::Quality),
            1.0
        );
        // DLAA renders at native resolution, and it is DLSS's `NativeAa`.
        assert_eq!(
            internal_factor(UpscaleMethod::Dlss, UpscaleQuality::NativeAa),
            1.0
        );
        // XeSS Ultra Performance is 3.0x, not the 2.0x it used to borrow from
        // Performance.
        assert!(
            (internal_factor(UpscaleMethod::Xess, UpscaleQuality::UltraPerformance) - 1.0 / 3.0)
                .abs()
                < 1e-9
        );
    }

    /// Native AA is "1.0× render, provider's own temporal antialiasing". It is
    /// offered only where the provider has such a mode — DLAA on DLSS, Native
    /// AA on FSR 3.1/4 and on XeSS — and it must render at exactly the output
    /// resolution wherever it is offered.
    #[test]
    fn native_aa_is_native_where_the_provider_offers_it() {
        assert_eq!(
            UpscaleQuality::parse("native_aa"),
            Some(UpscaleQuality::NativeAa)
        );
        assert_eq!(
            UpscaleQuality::parse("Native AA"),
            Some(UpscaleQuality::NativeAa)
        );
        assert_eq!(
            UpscaleQuality::parse(UpscaleQuality::NativeAa.token()),
            Some(UpscaleQuality::NativeAa)
        );
        for method in [
            UpscaleMethod::Dlss,
            UpscaleMethod::Fsr3,
            UpscaleMethod::Fsr4,
            UpscaleMethod::Xess,
        ] {
            assert!(
                quality_available(method, UpscaleQuality::NativeAa),
                "{method:?} must offer its native-AA mode"
            );
            assert_eq!(internal_factor(method, UpscaleQuality::NativeAa), 1.0);
            assert_eq!(
                provider_extent(method, UpscaleQuality::NativeAa, 1920, 1080, 8192),
                (1920, 1080),
                "{method:?} native AA must render at native resolution"
            );
        }
        // FSR 1 is spatial, FSR 2 predates the enum value, and Off is off.
        for method in [UpscaleMethod::Off, UpscaleMethod::Fsr1, UpscaleMethod::Fsr2] {
            assert!(
                !quality_available(method, UpscaleQuality::NativeAa),
                "{method:?} must not offer native AA"
            );
        }
        // Each provider calls its native-AA mode by its own name: the UI is
        // showing the provider's name, not a portable one.
        assert_eq!(
            UpscaleQuality::NativeAa.label_for(UpscaleMethod::Dlss),
            "DLAA"
        );
        assert_eq!(
            UpscaleQuality::NativeAa.label_for(UpscaleMethod::Fsr3),
            "Native AA"
        );
        assert_eq!(
            UpscaleQuality::NativeAa.label_for(UpscaleMethod::Xess),
            "Native AA"
        );
        // A persisted native-AA choice degrades to the nearest ratio when the
        // upscaler changes to one that cannot dispatch a 1.0x ratio, and
        // survives untouched where it can.
        let (_, quality, _) = sanitise(
            UpscaleMethod::Fsr2,
            UpscaleQuality::NativeAa,
            FrameGenMethod::Off,
        );
        assert_eq!(quality, UpscaleQuality::Quality);
        let (_, quality, _) = sanitise(
            UpscaleMethod::Dlss,
            UpscaleQuality::NativeAa,
            FrameGenMethod::Dlss,
        );
        assert_eq!(quality, UpscaleQuality::NativeAa);
        let (_, quality, _) = sanitise(
            UpscaleMethod::Fsr3,
            UpscaleQuality::NativeAa,
            FrameGenMethod::Fsr,
        );
        assert_eq!(quality, UpscaleQuality::NativeAa);
    }

    #[test]
    fn provider_extent_scales_and_clamps() {
        assert_eq!(
            provider_extent(
                UpscaleMethod::Fsr3,
                UpscaleQuality::Performance,
                1920,
                1080,
                8192
            ),
            (960, 540)
        );
        assert_eq!(
            provider_extent(
                UpscaleMethod::Dlss,
                UpscaleQuality::NativeAa,
                1920,
                1080,
                8192
            ),
            (1920, 1080)
        );
        // XeSS Ultra Quality is the 1.3x rung; it must not be 1.0x (that is
        // `XESS_QUALITY_SETTING_AA`, i.e. Native AA) nor 1.5x.
        assert_eq!(
            provider_extent(
                UpscaleMethod::Xess,
                UpscaleQuality::UltraQuality,
                1920,
                1080,
                8192
            ),
            (1477, 831)
        );
        let (w, h) = provider_extent(UpscaleMethod::Xess, UpscaleQuality::Balanced, 1, 1, 8192);
        assert_eq!((w, h), (1, 1));
        let (w, h) = provider_extent(
            UpscaleMethod::Fsr2,
            UpscaleQuality::UltraPerformance,
            4000,
            3000,
            4096,
        );
        assert!(w <= 4096 && h <= 4096);
    }

    #[test]
    fn render_extent_keeps_off_path_exact() {
        let gpu = nvidia_rtx4070();
        let sdk = no_sdk();
        // With Off (or an unavailable provider), the caller's slider extent wins.
        assert_eq!(
            render_extent(
                (960, 540),
                (1920, 1080),
                8192,
                UpscaleMethod::Off,
                UpscaleQuality::Quality,
                &gpu,
                &sdk,
            ),
            (960, 540)
        );
        assert_eq!(
            render_extent(
                (960, 540),
                (1920, 1080),
                8192,
                UpscaleMethod::Dlss,
                UpscaleQuality::Quality,
                &gpu,
                &sdk,
            ),
            (960, 540)
        );
        // An available provider overrides the slider extent.
        assert_eq!(
            render_extent(
                (1221, 717),
                (1920, 1080),
                8192,
                UpscaleMethod::Fsr3,
                UpscaleQuality::Performance,
                &amd_rx9070(),
                &full_sdk(),
            ),
            (960, 540)
        );
    }

    #[test]
    fn jitter_is_deterministic_and_bounded() {
        for frame in 0..JITTER_PERIOD * 2 {
            let (x, y) = jitter_offset(frame);
            assert!(x.abs() <= 0.5 && y.abs() <= 0.5);
            assert_eq!(jitter_offset(frame), (x, y));
        }
        // The sequence actually moves (first and second frames differ).
        assert_ne!(jitter_offset(0), jitter_offset(1));
        assert_eq!(jitter_offset(0), jitter_offset(JITTER_PERIOD));
    }

    #[test]
    fn halton_matches_known_radical_inverses() {
        assert!((halton(1, 2) - 0.5).abs() < 1e-6);
        assert!((halton(2, 2) - 0.25).abs() < 1e-6);
        assert!((halton(1, 3) - 1.0 / 3.0).abs() < 1e-6);
        assert!((halton(4, 2) - 0.125).abs() < 1e-6);
    }

    #[test]
    fn probe_empty_folder_reports_nothing() {
        let dir = std::env::temp_dir().join(format!("pwe-sdk-probe-{}-empty", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Nothing in the folder: every DLL missing, folder valid.
        let sdk = probe_sdk(dir.to_str().unwrap());
        assert_eq!(sdk.dir.as_deref(), Some(dir.as_path()));
        assert_eq!(sdk.source, SdkSource::Settings);
        assert!(sdk.dir_is_dir);
        assert!(!sdk.dlss && !sdk.fsr_upscale && !sdk.xess && !sdk.streamline);
        std::fs::remove_dir_all(&dir).ok();
        // A missing folder is reported, not fatal.
        let missing =
            std::env::temp_dir().join(format!("pwe-sdk-probe-{}-missing", std::process::id()));
        let sdk = probe_sdk(missing.to_str().unwrap());
        assert!(!sdk.dir_is_dir);
        assert!(support(UpscaleMethod::Dlss, &nvidia_rtx4070(), &sdk)
            .reason()
            .unwrap()
            .contains("does not exist"));
    }

    #[test]
    fn fallback_with_no_sdks_disables_every_provider() {
        // The exact fallback the viewer relies on when no SDK is shipped.
        let sdk = SdkReport::default();
        for gpu in [nvidia_rtx4070(), amd_rx9070(), intel_arc_a770(), software()] {
            for method in UpscaleMethod::ALL {
                // FSR 1 is in-tree (no SDK); everything else needs one.
                let expected = match method {
                    UpscaleMethod::Off => true,
                    UpscaleMethod::Fsr1 => !gpu.cpu_fallback,
                    _ => false,
                };
                assert_eq!(support(method, &gpu, &sdk).is_available(), expected);
            }
            for method in FrameGenMethod::ALL {
                let expected = matches!(method, FrameGenMethod::Off);
                assert_eq!(
                    support_framegen(method, UpscaleMethod::Dlss, &gpu, &sdk).is_available(),
                    expected
                );
            }
        }
        assert!(support(UpscaleMethod::Dlss, &nvidia_rtx4070(), &sdk)
            .reason()
            .unwrap()
            .contains("PW_UPSCALER_SDK_DIR"));
    }

    fn nvidia_rtx5090() -> GpuFacts {
        GpuFacts::from_parts(
            VENDOR_NVIDIA,
            0x2b85,
            "NVIDIA GeForce RTX 5090",
            "dx12",
            false,
        )
    }

    /// Every multiplier keeps its token, its label spelling and the frame
    /// arithmetic the generators are driven with.
    #[test]
    fn frame_multiplier_round_trips() {
        for multiplier in FrameMultiplier::ALL {
            assert_eq!(FrameMultiplier::parse(multiplier.token()), Some(multiplier));
            assert_eq!(
                FrameMultiplier::parse(multiplier.short_label()),
                Some(multiplier)
            );
            assert_eq!(multiplier.multiplier(), multiplier.generated() + 1);
            assert!(multiplier.label().starts_with(multiplier.short_label()));
        }
        assert_eq!(FrameMultiplier::default(), FrameMultiplier::X2);
        assert_eq!(FrameMultiplier::X6.generated(), 5);
        assert_eq!(FrameMultiplier::parse("8"), None);
        assert_eq!(FrameMultiplier::parse("x5"), None);
        // Both spellings a user might type are accepted; the persisted form is
        // the bare number, so a round trip cannot drift.
        assert_eq!(FrameMultiplier::parse("3x"), Some(FrameMultiplier::X3));
    }

    /// 2x is always available; the rest follow the vendor and the GPU, and
    /// every refusal says why (the UI greys the option out with that text).
    #[test]
    fn multiplier_support_matrix() {
        let sdk = full_sdk();
        // 2x never depends on anything: it is what every generator does.
        for method in FrameGenMethod::ALL {
            assert!(
                multiplier_support(method, FrameMultiplier::X2, &nvidia_rtx4070(), &sdk)
                    .is_available()
            );
        }
        // Ada reports `MultiFrameCountMax = 1`, so nothing above 2x there.
        let ada = multiplier_support(
            FrameGenMethod::Dlss,
            FrameMultiplier::X4,
            &nvidia_rtx4070(),
            &sdk,
        );
        assert!(!ada.is_available());
        assert!(ada.reason().unwrap().contains("RTX 50-series"));
        // Blackwell does, up to 6x (5 generated frames).
        for multiplier in [
            FrameMultiplier::X3,
            FrameMultiplier::X4,
            FrameMultiplier::X6,
        ] {
            assert!(
                multiplier_support(FrameGenMethod::Dlss, multiplier, &nvidia_rtx5090(), &sdk)
                    .is_available()
            );
        }
        // FSR interpolates exactly one frame per present, by design.
        let fsr = multiplier_support(
            FrameGenMethod::Fsr,
            FrameMultiplier::X3,
            &amd_rx9070(),
            &sdk,
        );
        assert!(!fsr.is_available());
        assert!(fsr.reason().unwrap().contains("exactly one frame"));
        // XeSS is offered and clamped at runtime.
        assert!(multiplier_support(
            FrameGenMethod::Xess,
            FrameMultiplier::X4,
            &intel_arc_a770(),
            &sdk
        )
        .is_available());
        // Without the SDK the underlying generator is unavailable, and that
        // reason wins over the multiplier-specific one.
        let no_xess = multiplier_support(
            FrameGenMethod::Xess,
            FrameMultiplier::X4,
            &intel_arc_a770(),
            &no_sdk(),
        );
        assert!(!no_xess.is_available());
        assert!(!no_xess.reason().unwrap().is_empty());
        // Frame generation off disables every multiplier above 2x.
        assert!(!multiplier_support(
            FrameGenMethod::Off,
            FrameMultiplier::X3,
            &nvidia_rtx5090(),
            &sdk
        )
        .is_available());
    }

    /// Reflex is the viewer's own pacer: independent of the frame generator,
    /// and available without any vendor library.
    #[test]
    fn reflex_is_an_independent_latency_owner() {
        for frame_generation in FrameGenMethod::ALL {
            assert_eq!(
                latency_owner(LatencyMode::Reflex, frame_generation),
                LatencyOwner::Independent
            );
            let support = support_latency(
                LatencyMode::Reflex,
                frame_generation,
                &amd_rx9070(),
                &no_sdk(),
            );
            assert!(support.is_available());
        }
        assert!(!support_latency(
            LatencyMode::Reflex,
            FrameGenMethod::Off,
            &software(),
            &no_sdk()
        )
        .is_available());
        assert_eq!(LatencyMode::parse("reflex"), Some(LatencyMode::Reflex));
        assert_eq!(LatencyMode::Reflex.token(), "reflex");
        assert_eq!(LatencyMode::Reflex.present_mode(), wgpu::PresentMode::Fifo);
    }
}

// ----------------------------------------------------------------
// Provider diagnostics — formerly the `dx12-upscale-check` and
// `dx12-framegen-check` bins. The flags print the same reports from the
// same code the viewer gates on, so the diagnostic cannot drift from the
// build. No client root is required.
//
//   perfect-world-explorer --diagnose-upscale [--sdk-dir <path>] [--require-sdk] [--require-gpu]
//   perfect-world-explorer --diagnose-framegen [--sdk-dir <path>] [--require-sdk] [--require-gpu]
//
// `--require-sdk` exits 4 when no vendor DLL is found, `--require-gpu` exits 5
// when no adapter can be created; unavailability is a report, not an error.
// ----------------------------------------------------------------
struct DiagnoseArgs {
    sdk_dir: String,
    require_sdk: bool,
    require_gpu: bool,
}

fn parse_diagnose_args(args: &[String]) -> Result<DiagnoseArgs, i32> {
    let mut sdk_dir = String::new();
    let mut require_sdk = false;
    let mut require_gpu = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--sdk-dir" => sdk_dir = iter.next().cloned().unwrap_or_default(),
            "--require-sdk" => require_sdk = true,
            "--require-gpu" => require_gpu = true,
            "--help" | "-h" => return Err(0),
            other => {
                eprintln!("ignored unknown argument: {other}");
            }
        }
    }
    Ok(DiagnoseArgs {
        sdk_dir,
        require_sdk,
        require_gpu,
    })
}

/// Scan one client folder and print its versions. No GPU, no window, no world
/// load, so it also runs on a machine that only has the client staged.
///
/// Exit status: `0` loadable (unknown versions are reported, not refused),
/// `1` missing world/required archive, `2` bad usage.
///
/// Windows-only because it shares `pck`/`angelica_pck` with the viewer; the
/// `client-check` binary prints exactly the same report on any host.
#[cfg(windows)]
fn diagnose_client(args: &[String]) -> i32 {
    let mut json = false;
    let mut path: Option<&str> = None;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "usage: perfect-world-explorer --diagnose-client <client root or element folder> [--json]"
                );
                return 0;
            }
            other => path = Some(other),
        }
    }
    let Some(path) = path else {
        eprintln!(
            "usage: perfect-world-explorer --diagnose-client <client root or element folder> [--json]"
        );
        return 2;
    };
    let scan = client_detect::scan(Path::new(path));
    if json {
        print_client_scan_json(&scan);
    } else {
        println!("Perfect World Explorer — diagnose-client");
        println!();
        print!("{}", scan.report());
        println!();
        println!("Result: {}", scan.headline());
    }
    if scan.loadable() {
        0
    } else {
        1
    }
}

/// The same flat JSON object `client-check --json` prints, hand-written because
/// the crate has no serializer.
#[cfg(windows)]
fn print_client_scan_json(scan: &client_detect::Scan) {
    fn quote(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
    let data_files: Vec<String> = scan
        .data_files
        .iter()
        .map(|info| {
            format!(
                "    {{\"name\": {}, \"version\": {}}}",
                quote(&info.name),
                quote(info.version.as_deref().unwrap_or(""))
            )
        })
        .collect();
    let archives: Vec<String> = scan
        .archives
        .iter()
        .map(|archive| {
            format!(
                "    {{\"name\": {}, \"container\": {}, \"entries\": {}, \"problem\": {}}}",
                quote(&archive.name),
                quote(archive.kind.label()),
                archive
                    .entries
                    .map(|entries| entries.to_string())
                    .unwrap_or_else(|| "null".into()),
                archive
                    .problem
                    .as_deref()
                    .map(quote)
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect();
    let errors: Vec<String> = scan.errors.iter().map(|error| quote(error)).collect();
    let notes: Vec<String> = scan.notes.iter().map(|error| quote(error)).collect();
    let mut fields = vec![
        format!("  \"element\": {}", quote(&scan.element.to_string_lossy())),
        format!("  \"loadable\": {}", scan.loadable()),
        format!("  \"headline\": {}", quote(&scan.headline())),
    ];
    if let Some(exe) = &scan.executable {
        fields.push(format!("  \"executable\": {}", quote(&exe.label())));
        fields.push(format!("  \"executable_known\": {}", exe.is_known()));
        fields.push(format!("  \"executable_size\": {}", exe.size));
        fields.push(format!("  \"executable_sha256\": {}", quote(&exe.sha256)));
    }
    // The data version is the `(elements, tasks)` pair; the two halves are
    // printed separately and together so a script can read either.
    let (elements_version, tasks_version) = scan.data_versions();
    fields.push(format!(
        "  \"elements_version\": {}",
        elements_version
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into())
    ));
    fields.push(format!(
        "  \"tasks_version\": {}",
        tasks_version
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into())
    ));
    fields.push(format!(
        "  \"data_version\": {}",
        quote(&scan.data_version_summary())
    ));
    if let Some(elements) = &scan.elements {
        fields.push(format!(
            "  \"elements_raw\": {}",
            elements
                .raw
                .map(|raw| format!("{raw}"))
                .unwrap_or_else(|| "null".into())
        ));
        fields.push(format!(
            "  \"elements_label\": {}",
            quote(elements.version.as_deref().unwrap_or(""))
        ));
    }
    fields.push(format!(
        "  \"data_files\": [\n{}\n  ]",
        data_files.join(",\n")
    ));
    fields.push(format!("  \"archives\": [\n{}\n  ]", archives.join(",\n")));
    fields.push(format!("  \"errors\": [{}]", errors.join(", ")));
    fields.push(format!("  \"notes\": [{}]", notes.join(", ")));
    println!("{{\n{}\n}}", fields.join(",\n"));
}

fn diagnose_adapter() -> Option<GpuFacts> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let info = adapter.get_info();
    Some(GpuFacts::from_parts(
        info.vendor,
        info.device,
        &info.name,
        &format!("{:?}", info.backend),
        info.device_type == wgpu::DeviceType::Cpu,
    ))
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "found"
    } else {
        "absent"
    }
}

fn describe_support(support: &Support) -> String {
    match support {
        Support::Available(note) => format!("available ({note})"),
        Support::Disabled(reason) => format!("unavailable — {reason}"),
    }
}

fn diagnose_print_gpu(gpu: &GpuFacts) {
    println!(
        "GPU: {} (vendor 0x{:04x}, device 0x{:04x}, backend {}{})",
        gpu.name,
        gpu.vendor,
        gpu.device,
        gpu.backend,
        if gpu.cpu_fallback {
            ", software adapter"
        } else {
            ""
        }
    );
    println!();
}

fn diagnose_upscale(args: &[String]) -> i32 {
    println!("Perfect World Explorer — diagnose-upscale");
    println!("platform: {}", std::env::consts::OS);
    println!(
        "requested backends: {}",
        std::env::var("WGPU_BACKEND").unwrap_or_else(|_| {
            if cfg!(windows) {
                "dx12 (Windows default on this branch)".to_string()
            } else {
                "all supported by wgpu (Windows uses dx12)".to_string()
            }
        })
    );
    println!();
    let parsed = match parse_diagnose_args(args) {
        Ok(parsed) => parsed,
        Err(code) => {
            println!("perfect-world-explorer --diagnose-upscale [--sdk-dir <path>] [--require-sdk] [--require-gpu]");
            return code;
        }
    };

    let sdk = probe_sdk(&parsed.sdk_dir);
    println!("SDK probe: {}", sdk.summary());
    println!(
        "  nvngx_dlss.dll: {}   nvngx_dlssg.dll: {}   sl.interposer.dll: {}",
        yes_no(sdk.dlss),
        yes_no(sdk.dlss_frame_generation),
        yes_no(sdk.streamline)
    );
    println!(
        "  FSR loader: {}   FSR upscaler: {}   FSR frame generation: {}",
        yes_no(sdk.fsr_loader),
        yes_no(sdk.fsr_upscale),
        yes_no(sdk.fsr_frame_generation)
    );
    println!(
        "  libxess.dll: {}   libxess_fg.dll: {}   libxell.dll: {}",
        yes_no(sdk.xess),
        yes_no(sdk.xess_frame_generation),
        yes_no(sdk.xell)
    );
    println!();

    match diagnose_adapter() {
        Some(gpu) => {
            diagnose_print_gpu(&gpu);
            println!("Upscaler support on this machine:");
            for method in UpscaleMethod::ALL {
                let support = support(method, &gpu, &sdk);
                println!("  {} — {}", method.label(), describe_support(&support));
            }
            println!();
            println!("Frame generation support (with Upscaling = Off):");
            for method in FrameGenMethod::ALL {
                let fg = support_framegen(method, UpscaleMethod::Off, &gpu, &sdk);
                println!("  {} — {}", method.label(), describe_support(&fg));
            }
            println!();
            println!("Latency support (Graphics -> Latency):");
            for mode in LatencyMode::ALL {
                let support = support_latency(mode, FrameGenMethod::Off, &gpu, &sdk);
                println!("  {} — {}", mode.label(), describe_support(&support));
            }
            // XeLL is also reachable through XeSS Frame Generation's mandatory
            // context; that is the only cross-vendor route Intel documents.
            println!(
                "  {} + Frame generation = XeSS — {}",
                LatencyMode::Xell.label(),
                describe_support(&support_latency(
                    LatencyMode::Xell,
                    FrameGenMethod::Xess,
                    &gpu,
                    &sdk
                ))
            );
            println!();
            println!("Quality modes and internal render factors (1920×1080 window):");
            for method in UpscaleMethod::ALL {
                if method == UpscaleMethod::Off {
                    continue;
                }
                println!("  {}:", method.label());
                for mode in modes::modes(method) {
                    let (w, h) = provider_extent(method, mode.quality, 1920, 1080, 8192);
                    println!(
                        "    {} — internal {}×{} (factor {:.4}, {} {})",
                        mode.name,
                        w,
                        h,
                        mode.factor,
                        vendor_parameter_name(method),
                        mode.vendor
                    );
                }
                for quality in UpscaleQuality::ALL {
                    if let Some(note) = quality.availability_note(method) {
                        println!("    [not offered] {note}");
                    }
                }
            }
            println!();
            println!("Temporal jitter (Halton 2,3, period 64), first 8 frames:");
            for frame in 0..8 {
                let (x, y) = jitter_offset(frame);
                println!("  frame {frame:2}: ({x:+.5}, {y:+.5}) px");
            }
        }
        None => {
            println!("GPU: no adapter could be created (headless or no driver).");
            println!("The support matrix needs a GPU; the SDK probe above still ran.");
            if parsed.require_gpu {
                return 5;
            }
        }
    }

    let any_sdk = sdk.dlss || sdk.fsr_upscale || sdk.xess || sdk.xell;
    if parsed.require_sdk && !any_sdk {
        println!();
        println!("FAIL: --require-sdk and no vendor SDK DLL was found in the probed folder(s).");
        return 4;
    }
    println!();
    println!("Done. FSR, XeSS and XeLL are integrated in this build (see README -> Graphics); unavailable providers are reported, not faked.");
    0
}

fn diagnose_framegen(args: &[String]) -> i32 {
    println!("Perfect World Explorer — diagnose-framegen");
    println!("platform: {}", std::env::consts::OS);
    println!();
    let parsed = match parse_diagnose_args(args) {
        Ok(parsed) => parsed,
        Err(code) => {
            println!("perfect-world-explorer --diagnose-framegen [--sdk-dir <path>] [--require-sdk] [--require-gpu]");
            return code;
        }
    };

    let sdk = probe_sdk(&parsed.sdk_dir);
    println!("SDK probe: {}", sdk.summary());
    println!(
        "  DLSS FG: {}   FSR FG: {} (loader: {})   XeSS 2 FG: {}   XeLL: {}",
        yes_no(sdk.dlss_frame_generation),
        yes_no(sdk.fsr_frame_generation),
        yes_no(sdk.fsr_loader),
        yes_no(sdk.xess_frame_generation),
        yes_no(sdk.xell)
    );
    println!();

    println!("Pairing matrix (vendor documentation; the FSR leg is wired into this build):");
    println!(
        "                       {:>22} {:>22} {:>22}",
        "DLSS FG", "FSR FG", "XeSS FG"
    );
    for upscale in UpscaleMethod::ALL {
        let cells: Vec<&'static str> = [
            FrameGenMethod::Dlss,
            FrameGenMethod::Fsr,
            FrameGenMethod::Xess,
        ]
        .iter()
        .map(|fg| match pairing(upscale, *fg) {
            PairingVerdict::Supported => "supported",
            PairingVerdict::Unsupported(_) => "not supported",
        })
        .collect();
        println!(
            "  {:>18} {:>22} {:>22} {:>22}",
            upscale.label(),
            cells[0],
            cells[1],
            cells[2]
        );
    }
    for upscale in UpscaleMethod::ALL {
        for frame_gen in FrameGenMethod::ALL {
            if let PairingVerdict::Unsupported(reason) = pairing(upscale, frame_gen) {
                println!(
                    "unsupported pairing: {} + {} — {reason}",
                    upscale.label(),
                    frame_gen.label()
                );
            }
        }
    }
    println!();

    match diagnose_adapter() {
        Some(gpu) => {
            diagnose_print_gpu(&gpu);
            println!("Frame generation support per pairing on this machine:");
            for upscale in UpscaleMethod::ALL {
                for frame_gen in [
                    FrameGenMethod::Dlss,
                    FrameGenMethod::Fsr,
                    FrameGenMethod::Xess,
                ] {
                    let fg = support_framegen(frame_gen, upscale, &gpu, &sdk);
                    println!(
                        "  {} + {} — {}",
                        upscale.label(),
                        frame_gen.label(),
                        describe_support(&fg)
                    );
                }
            }
        }
        None => {
            println!("GPU: no adapter could be created (headless or no driver).");
            println!("The per-pairing matrix needs a GPU; the SDK probe above still ran.");
            if parsed.require_gpu {
                return 5;
            }
        }
    }

    println!();
    println!("Frame pacing contract (held when the FSR generator is active):");
    println!("  - generated frames present in order, after the real frame that produced them;");
    println!("  - the UI is drawn on the real frame only and never interpolated;");
    println!("  - the UI input rate is not halved by interpolation.");
    println!();

    let any_sdk =
        sdk.dlss_frame_generation || sdk.fsr_frame_generation || sdk.xess_frame_generation;
    if parsed.require_sdk && !any_sdk {
        println!();
        println!(
            "FAIL: --require-sdk and no frame-generation SDK DLL was found in the probed folder(s)."
        );
        return 4;
    }
    println!();
    println!("Done. FSR Frame Generation is integrated in this build (see README → Graphics); unavailable pairings are reported, not faked.");
    0
}

mod viewer;

fn log_model_load(key: &str, line: &str) {
    diagnostics::emit_keyed(diagnostics::Channel::ModelLoads, key, 0, line);
}

fn main() {
    #[cfg(windows)]
    std::panic::set_hook(Box::new(|info| {
        // A recoverable map panic is retried on the main world by
        // `world_load`; a modal error here would block the fallback.
        if !world_load::recovering_from_map_failure() {
            windows_setup::show_error(&format!(
                "Perfect World Explorer encountered an unexpected error:\n{info}"
            ))
        }
    }));
    world_load::install_recoverable_panic_hook();
    #[allow(unused_mut)]
    let mut args: Vec<_> = env::args_os().collect();
    // `--world <id>` is an explicit startup override from a replacement-process
    // handoff. Validate it before any path or process argument is constructed.
    let mut requested_world: Option<String> = None;
    let mut index = 1usize;
    while index < args.len() {
        if args[index].to_string_lossy() == "--world" {
            if index + 1 >= args.len() {
                eprintln!("Error: --world requires a map ID");
                std::process::exit(2);
            }
            requested_world = Some(args[index + 1].to_string_lossy().into_owned());
            args.drain(index..index + 2);
        } else {
            index += 1;
        }
    }
    if let Some(id) = requested_world.as_deref() {
        if let Err(error) = world_catalog::validate_selection(id) {
            eprintln!("Error: {error}");
            std::process::exit(2);
        }
    }
    if args.len() >= 2 {
        let flag = args[1].to_string_lossy().into_owned();
        if flag == "--diagnose-client" {
            #[cfg(windows)]
            {
                let rest: Vec<String> = args[2..]
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                std::process::exit(diagnose_client(&rest));
            }
            // The scan itself needs no Windows API; it shares the archive
            // readers with the viewer, which are Windows-only in this binary.
            #[cfg(not(windows))]
            {
                eprintln!(
                    "--diagnose-client is built into the Windows viewer; on this host run \
                     `cargo run --bin client-check -- <folder>` for the same report"
                );
                std::process::exit(2);
            }
        }
        if flag == "--diagnose-upscale" || flag == "--diagnose-framegen" {
            let rest: Vec<String> = args[2..]
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let code = if flag == "--diagnose-upscale" {
                diagnose_upscale(&rest)
            } else {
                diagnose_framegen(&rest)
            };
            std::process::exit(code);
        }
    }
    #[cfg(windows)]
    if args.len() == 1 {
        match windows_setup::start_window() {
            Ok(Some(path)) => args.push(path.into_os_string()),
            Ok(None) => return,
            Err(e) => {
                windows_setup::show_error(&e);
                return;
            }
        }
    }
    if args.len() < 2 || (args.len() > 4 && args.len() != 9) {
        eprintln!(
            "Usage: perfect-world-explorer <client-root> [--world <map-id>] [terrain-output.png | --preview-3d output.png]"
        );
        std::process::exit(2)
    }
    let path = PathBuf::from(&args[1]);
    // A direct map directory still belongs to the containing client; logs must
    // resolve against that root rather than nesting inside the map folder.
    let client_root = world_catalog::client_root(&path);
    diagnostics::set_client_root(&client_root);
    // Publish the saved residency choice before anything opens a cache: the
    // terrain store and the decoded-texture caches both read it.
    let mut saved = view_settings::ViewSettings::load();
    // A direct map-directory CLI argument overrides the persisted selection;
    // an explicit `--world` from a replacement process overrides both. A raw
    // folder name that is not a safe ID is still loadable directly, but cannot
    // be persisted or passed to a replacement process.
    let direct_map = if requested_world.is_none() {
        world_catalog::readable_map_id(&path)
    } else {
        None
    };
    let direct_override = direct_map
        .as_deref()
        .filter(|id| world_catalog::valid_world_id(id));
    if let Some(id) = requested_world.as_deref().or(direct_override) {
        saved.selected_world = id.to_ascii_lowercase();
        if let Err(error) = saved.save() {
            eprintln!("Settings save: {error}");
        }
    }
    let direct_without_safe_id = direct_map.is_some() && direct_override.is_none();
    let selected_world = match world_catalog::validate_selection(&saved.selected_world) {
        Ok(id) => id.to_ascii_lowercase(),
        Err(error) => {
            eprintln!(
                "Invalid persisted map {}: {error}; using world",
                saved.selected_world
            );
            saved.selected_world = "world".into();
            let _ = saved.save();
            "world".into()
        }
    };
    // Live logging goes before everything else, so the first load can already be
    // logged: the folder defaults to the explorer's own `pwe-logs`, next to the
    // executable rather than inside the client install, the mask is whatever the
    // settings tab left ticked, and the session line names the run.
    // The client root comes first: it is what "do not write the logs in there"
    // is measured against, both for the default and for a folder the settings
    // file still remembers from a build that wrote next to the game.
    diagnostics::set_default_dir(diagnostics::default_log_dir());
    if !saved.logging_dir.trim().is_empty()
        && diagnostics::inside_client(std::path::Path::new(saved.logging_dir.trim()))
    {
        saved.logging_dir.clear();
    }
    diagnostics::apply_dir(&saved.logging_dir);
    diagnostics::set_mask(saved.logging_mask);
    // `PWE_LOG=EffectDecals,EffectElements` is the headless way to the same
    // channels the settings tab ticks, and it is how a run with no settings file
    // — the effect preview — can say which builder drew a card.
    diagnostics::apply_env();
    diagnostics::set_session(format!(
        "client root {} | explorer {} | version {} | world {} | mask {:#018x}",
        client_root.display(),
        diagnostics::explorer_root().display(),
        env!("CARGO_PKG_VERSION"),
        selected_world,
        saved.logging_mask
    ));
    objects::set_model_load_logger(log_model_load);
    objects::set_unbounded_textures(saved.all_to_vram);
    let all_to_vram = saved.all_to_vram;
    let viewer_mode = args.len() == 2;
    // The old process's non-interactive overlay is the only loading UI for a
    // handoff child. Create the standalone loading window once, before terrain
    // fallback, so a second attempt never opens another Win32 window.
    //
    // The window opens before the map resolves, so no `Terrain` exists yet to
    // name the client's `element` folder: the user's selection is resolved here
    // instead, because `surfaces.pck` - the archive the loading-screen pictures
    // come from - lives in `element`. A folder that cannot be resolved leaves
    // the window on its plain background and says so, instead of failing the
    // load over artwork.
    //
    // The window also opens for a handoff child, because a world change from the
    // Teleport List used to leave nothing on screen but the old process's name.
    // The child runs this window on its own thread while it loads terrain, so
    // the picture and the DirectStorage readout stay live during the part of the
    // change that actually takes the time. The old process hides its own copy as
    // soon as the child publishes its HWND.
    #[cfg(windows)]
    let loading = {
        let resolved = world_catalog::resolve(&path, Some(&selected_world)).ok();
        let element = resolved
            .as_ref()
            .map(|map| map.element.clone())
            .unwrap_or_else(|| loading_screen::element_folder(&path));
        // The destination's own names, so the picture is matched to the place
        // instead of only drawn at random.
        let location = resolved
            .as_ref()
            .map(|map| loading_screen::Location::new(&map.id, &map.display_name, &map.name));
        Some(windows_setup::LoadingWindow::start_for(
            Some(&element),
            location.as_ref(),
        ))
    };
    // Loading terrain is the only operation that can be retried in-process.
    // PR #40 showed that retrying `gpu::run` after a visible window has been
    // created fails with "EventLoop can't be recreated"; a post-load failure
    // therefore restarts a fresh process on the main world.
    let mut preferred_world = selected_world.clone();
    let load_terrain = |world_id: &str, notice: Option<String>| -> Result<Terrain, String> {
        let fallback = notice.is_some();
        let progress = if fallback {
            "Selected map failed; preparing the main world...".to_string()
        } else {
            format!("Loading world {world_id}...")
        };
        world_handoff::publish_progress(&progress);
        // An unsafe direct folder name is resolved from the path itself on the
        // first attempt; the fallback retry uses the owning client root and the
        // safe main-world ID, so a broken instance can still return to `world`.
        let preferred = if direct_without_safe_id && !fallback {
            None
        } else {
            Some(world_id)
        };
        let load_root = if fallback {
            client_root.as_path()
        } else {
            path.as_path()
        };
        terrain(load_root, all_to_vram, preferred, notice)
    };
    let (t, fallback_notice) =
        match world_load::load_with_main_world_fallback_notified(&preferred_world, load_terrain) {
            Ok((loaded, notice)) => (loaded, notice),
            Err(error) => {
                // A replacement child must report failure before showing its
                // own error; otherwise the old window would wait for a
                // readiness signal that will never arrive.
                world_handoff::publish_failure(&error);
                eprintln!("Error: {error}");
                #[cfg(windows)]
                drop(loading);
                #[cfg(windows)]
                if !world_handoff::is_handoff_child() {
                    windows_setup::show_error(&error);
                }
                std::process::exit(1);
            }
        };
    if let Some(notice) = fallback_notice.as_deref() {
        preferred_world = "world".into();
        // The fallback is a recovery, not a choice: `selected_world` stays the
        // map the user picked, so the next launch tries it again. Overwriting it
        // here made one bad load permanent — a client whose map failed once then
        // opened on the mainland forever, with no way back but the map list.
        if let Err(error) = saved.save() {
            eprintln!("Could not persist the recovered main-world session: {error}");
        }
        eprintln!("{notice}");
        #[cfg(windows)]
        if !world_handoff::is_handoff_child() {
            windows_setup::show_error(notice);
        }
        world_handoff::publish_progress("Selected map failed; preparing the main world...");
    }
    let run_result = world_load::recoverable_map_operation(move || {
        let mut t = t;
        match args.len() {
            #[cfg(windows)]
            2 => gpu::run(t, loading),
            #[cfg(not(windows))]
            2 => gpu::run(t),
            3 => export_png(&t, Path::new(&args[2])),
            4 if args[2] == "--preview-3d" => viewer::preview(&mut t, Path::new(&args[3])),
            4 if args[2] == "--gpu-preview" => gpu::preview(t, Path::new(&args[3]), None),
            9 if args[2] == "--gpu-preview-at" => {
                let mut coords = [0f32; 5];
                for i in 0..5 {
                    coords[i] = args[3 + i]
                        .to_string_lossy()
                        .parse::<f32>()
                        .map_err(|_| "invalid camera number")?;
                }
                gpu::preview(t, Path::new(&args[8]), Some(coords))
            }
            9 if args[2] == "--preview-at" => {
                let mut coords = [0f32; 5];
                for i in 0..5 {
                    coords[i] = args[3 + i]
                        .to_string_lossy()
                        .parse::<f32>()
                        .map_err(|_| "invalid camera number")?;
                }
                viewer::preview_at(
                    &mut t,
                    Path::new(&args[8]),
                    coords[0],
                    coords[1],
                    coords[2],
                    coords[3],
                    coords[4],
                )
            }
            _ => Err("invalid arguments".into()),
        }
    })
    .and_then(|result| result);
    if let Err(error) = run_result {
        if world_load::post_load_failure_decision(&preferred_world, viewer_mode)
            == world_load::PostLoadDecision::RestartMainWorld
        {
            // Same rule as the terrain fallback above: restart on the main
            // world, but leave `selected_world` alone so the map the user chose
            // is tried again on the next launch instead of being forgotten.
            if let Err(save_error) = saved.save() {
                eprintln!("Could not persist the recovered main-world session: {save_error}");
            }
            let notice = format!(
                "Map {} failed after loading. The explorer is restarting in the main world.\n\n{error}",
                preferred_world
            );
            eprintln!("{notice}");
            let restarted = env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    let mut command = std::process::Command::new(exe);
                    command.arg(&client_root).arg("--world").arg("world");
                    world_handoff::clear_child_environment(&mut command);
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        // Do not flash a console window behind the GUI restart.
                        command.creation_flags(0x08000000);
                    }
                    command
                        .spawn()
                        .map(|_| ())
                        .map_err(|restart_error| restart_error.to_string())
                });
            match restarted {
                Ok(()) => {
                    // Publish after the fresh process exists. The parent sees a
                    // progress notice and readiness, so it exits its overlay
                    // instead of leaving a second window with a failure dialog.
                    // The restart deliberately inherits no handoff files.
                    world_handoff::publish_progress(&notice);
                    world_handoff::signal_ready();
                    #[cfg(windows)]
                    if !world_handoff::is_handoff_child() {
                        windows_setup::show_error(&notice);
                    }
                    return;
                }
                Err(restart_error) => {
                    let fatal =
                        format!("{notice}\n\nCould not restart the explorer: {restart_error}");
                    world_handoff::publish_failure(&fatal);
                    eprintln!("Error: {fatal}");
                    #[cfg(windows)]
                    if !world_handoff::is_handoff_child() {
                        windows_setup::show_error(&fatal);
                    }
                    std::process::exit(1);
                }
            }
        }
        world_handoff::publish_failure(&error);
        eprintln!("Error: {error}");
        #[cfg(windows)]
        if !world_handoff::is_handoff_child() {
            windows_setup::show_error(&error);
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod ram_export_tests {
    #[test]
    fn terrain_decode_window_bounds_work_ahead_and_releases_in_order() {
        let mut window = super::DecodeWindow::default();
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Block(0));
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Block(1));
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Block(2));
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Wait);

        window.next_expected = 2;
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Block(3));
        window.cancelled = true;
        assert_eq!(window.claim(10, 3), super::DecodeClaim::Done);
    }

    #[test]
    fn streamed_png_preserves_decoded_rgb() {
        let path =
            std::env::temp_dir().join(format!("pw-streamed-export-{}.png", std::process::id()));
        let heights = vec![0.; 12].into();
        super::export_height_png(4, 3, &heights, &path).unwrap();
        let decoder = png::Decoder::new(std::fs::File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((frame.width, frame.height), (4, 3));
        assert_eq!(frame.color_type, png::ColorType::Rgb);
        assert_eq!(&pixels[..frame.buffer_size()], &[76u8, 93, 60].repeat(12));
        drop(reader);
        std::fs::remove_file(path).unwrap();
    }
}
