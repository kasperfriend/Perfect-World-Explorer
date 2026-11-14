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
    String::from_utf8(s.to_vec()).map_err(|_| "non-UTF8 subterrain name".into())
}
mod angelica_pck;
mod animation;
mod bmd;
mod bmd_baked;
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
// A preview is a test-only harness. The skip is on the module: its literals
// are authored GFX text and its tables are read top to bottom, neither of
// which formatting improves, and every other module stays checked.
#[cfg(test)]
#[rustfmt::skip]
mod effect_preview;
mod display_gamma;
mod dxt;
mod ecmodel;
mod environment;
mod far_trees_gpu;
mod fog_matrix;
mod foliage_mips;
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
#[cfg(windows)]
mod launcher_ui;
mod light_consumers;
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
mod world_models;
mod world_models_gpu;
mod world_objects;
struct Terrain {
    element: PathBuf,
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

/// Per-block scratch, owned by whoever decodes a block.
///
/// Blocks are decoded on several threads (see `decode_blocks`), so each worker
/// keeps its own scratch and only the two finished arrays travel back to the
/// builder. Reusing the rest keeps the per-block allocation count at two.
#[derive(Default)]
struct BlockScratch {
    decoded: Vec<u8>,
    values: Vec<f32>,
    normal_data: Vec<u8>,
    block_normals: Vec<[i8; 3]>,
    decoded_colour: Vec<u8>,
    heights: Vec<f32>,
    normals: Vec<[i8; 3]>,
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
/// `insert` runs on the calling thread, which is why the builder needs no
/// locking. `PW_TERRAIN_THREADS=1` restores the plain loop.
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
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<BlockPayload>();
    std::thread::scope(|scope| -> Result<(), String> {
        for _ in 0..threads.min(tile.blocks) {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || {
                let mut scratch = BlockScratch::default();
                loop {
                    let bi = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if bi >= tile.blocks {
                        break;
                    }
                    let decoded = decode_block_owned(tile, &mut scratch, bi);
                    let failed = decoded.is_err();
                    if tx.send((bi, decoded)).is_err() || failed {
                        break;
                    }
                }
            });
        }
        drop(tx);
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
                if next_expected >= tile.blocks {
                    break;
                }
            }
        }
        if next_expected < tile.blocks {
            return Err(format!("{}: terrain decode stopped early", tile.name));
        }
        Ok(())
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
fn terrain(root: &Path, all_to_vram: bool) -> Result<Terrain, String> {
    let dir = if root.join("element/maps/world/world.trn2").is_file() {
        root.join("element/maps/world")
    } else if root.join("maps/world/world.trn2").is_file() {
        root.join("maps/world")
    } else if root.join("world.trn2").is_file() {
        root.to_path_buf()
    } else {
        return Err("expected client root, element folder, or maps/world directory".into());
    };
    let t = fs::read(dir.join("world.trn2")).map_err(|e| e.to_string())?;
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
            let data =
                fs::read(dir.join(format!("{name}.t2bk"))).map_err(|e| format!("{name}: {e}"))?;
            if u32at(&data, 0)? != 0x54424b42 {
                return Err(format!("{name}: invalid T2BK"));
            };
            let v = u32at(&data, 4)?;
            if !(2..=6).contains(&v) {
                return Err(format!("{name}: unsupported version {v}"));
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
    let world_scene = scene::Scene::load(&dir.join("world.ecwld"))?;
    let cloud_surface = match fs::read(dir.join("world.cld")) {
        Ok(bytes) => {
            let cloud = cloud_surface::parse(&bytes).map_err(|e| format!("world.cld: {e}"))?;
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
            eprintln!("world.cld unavailable; native cloud surface omitted");
            None
        }
        Err(e) => return Err(format!("world.cld: {e}")),
    };
    if (world_scene.width - w as f32 * grid_spacing).abs() > 0.1
        || (world_scene.length - h as f32 * grid_spacing).abs() > 0.1
    {
        return Err("ECWLD and TRN2 world dimensions differ".into());
    }
    let water = water::load(&world_scene, &dir)?;
    eprintln!(
        "Loaded {} water areas from client BSData grids",
        water.len()
    );
    let ornaments = world_scene.load_ornaments(&dir.join("world.ecbsd"))?;
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
    let materials = material::Materials::load(&dir, &t, &names, w, h, grid_spacing)?;
    let element = dir
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid world path")?;
    let water_normals = water::load_normals(element)?;
    let water_simple_frames = water::load_simple_frames(element)?;
    let objects = objects::Objects::open(element, ornaments)?;
    let placements: Vec<_> = world_scene
        .blocks
        .iter()
        .flat_map(|b| b.models.iter().cloned())
        .collect();
    let models = match world_models::Assets::open(element, placements.clone()) {
        Ok(models) => models,
        Err(e) => world_models::Assets::load_with(placements, |_| Err(e.clone())),
    };
    eprintln!("Retained {} EC model assets for {} placements; {} unloaded placements (ordinary animated submission enabled unless PW_DISABLE_WORLD_MODELS is set)", models.assets.len(), models.instances.len(), models.unloaded.len());
    for failure in &models.failures {
        eprintln!("EC model dependency: {failure}");
    }
    let grass = grass::Grass::load(&world_scene, &dir, element)?;
    #[cfg(windows)]
    windows_setup::loading_progress(
        "Preparing trees (first run may take several minutes)...".into(),
    );
    let tree_cache = if let Some(dir) = std::env::var_os("PW_TREE_CACHE") {
        PathBuf::from(dir)
    } else {
        #[cfg(windows)]
        {
            windows_setup::prepare_trees(element)?
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("tree-cache")
        }
    };
    let trees = trees::Trees::open(element, &world_scene, &tree_cache)?;
    #[cfg(windows)]
    if std::env::var_os("PW_TREE_CACHE").is_none() && trees.types.iter().any(|tree| tree.is_none())
    {
        return Err(format!("Some prepared tree types could not be loaded. Close Perfect World Explorer, remove this cache folder, and try again:\n{}",tree_cache.display()));
    }
    let environments = environment::Environments::load(&world_scene, &dir)?;
    // Ordinary world-model receivers query these actual client records.
    // Do not also add them to static baked BMDs or terrain: those surfaces
    // use their separately traced baked-light paths.
    let scene_lights = match scene_lights::SceneLights::load(&dir.join("scenelights.dat")) {
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
        terrain_material_window: None,
        element: element.to_path_buf(),
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
mod viewer;

fn main() {
    #[cfg(windows)]
    std::panic::set_hook(Box::new(|info| {
        windows_setup::show_error(&format!(
            "Perfect World Explorer encountered an unexpected error:\n{info}"
        ))
    }));
    #[allow(unused_mut)]
    let mut args: Vec<_> = env::args_os().collect();
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
            "Usage: perfect-world-explorer <client-root> [terrain-output.png | --preview-3d output.png]"
        );
        std::process::exit(2)
    }
    let path = PathBuf::from(&args[1]);
    // Publish the saved residency choice before anything opens a cache: the
    // terrain store and the decoded-texture caches both read it.
    let mut saved = view_settings::ViewSettings::load();
    // Live logging goes before everything else, so the first load can already be
    // logged: the folder defaults to the explorer's own `pwe-logs`, next to the
    // executable rather than inside the client install, the mask is whatever the
    // settings tab left ticked, and the session line names the run.
    // The client root comes first: it is what "do not write the logs in there"
    // is measured against, both for the default and for a folder the settings
    // file still remembers from a build that wrote next to the game.
    diagnostics::set_client_root(&path);
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
        "client root {} | explorer {} | version {} | mask {:#018x}",
        path.display(),
        diagnostics::explorer_root().display(),
        env!("CARGO_PKG_VERSION"),
        saved.logging_mask
    ));
    objects::set_unbounded_textures(saved.all_to_vram);
    #[cfg(windows)]
    let loading = windows_setup::LoadingWindow::start();
    let loaded = terrain(&path, saved.all_to_vram);
    let result = loaded.and_then(|mut t| match args.len() {
        #[cfg(windows)]
        2 => gpu::run(t, Some(loading)),
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
    });
    if let Err(e) = result {
        eprintln!("Error: {e}");
        #[cfg(windows)]
        windows_setup::show_error(&e);
        std::process::exit(1)
    }
}

#[cfg(test)]
mod ram_export_tests {
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
