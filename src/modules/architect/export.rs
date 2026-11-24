//! Import existing Perfect World maps into Architect and export Architect worlds
//! as genuine, playable game world maps (`<id>.trn2`, `<id>_0.t2bk`, `<id>_0.t2mk`,
//! `<id>.ecwld`, `<id>.ecbsd`, `precinct.clt`) that `world_catalog::discover`
//! and the core viewer load and travel inside natively.

use super::world::{
    sanitize_world_slug, EditableTerrain, ObjectKind, PaintLayer, PlacedObject, WorldDocument,
    DEFAULT_TERRAIN_CELLS,
};
use crate::{pck, scene::Scene, terrain_block, world_catalog};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const TRN2_MAGIC: u32 = 0x5452_4e32;
const T2BK_MAGIC: u32 = 0x5442_4b42;
const T2MK_MAGIC: u32 = 0x544d_4b42;
const ECWLD_MAGIC: u32 = 0x4543_5744;
const ECBSD_MAGIC_0: u32 = 0x4258_4f4d;
const ECBSD_MAGIC_1: u32 = 0x4253_4446;
const BLOCK_GRID: usize = 32;

/// Summary returned after exporting a real game world map.
#[derive(Clone, Debug)]
pub struct ExportedMap {
    pub map_id: String,
    pub dir: PathBuf,
    pub ornaments: usize,
    pub models: usize,
    pub trees: usize,
    pub effects: usize,
    pub sounds: usize,
}

fn push_gbk_string(out: &mut Vec<u8>, s: &str) {
    let win_path = s.replace('/', "\\");
    let (encoded, _, _) = encoding_rs::GBK.encode(&win_path);
    out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    out.extend_from_slice(&encoded);
}

fn push_vec3(out: &mut Vec<u8>, v: [f32; 3]) {
    for c in v {
        out.extend_from_slice(&c.to_le_bytes());
    }
}

fn zlib_compress(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(raw).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// Find a real valid terrain DDS texture inside `element/textures.pck` so the
/// exported map's `TRN2` texture table resolves cleanly when the viewer loads it.
fn discover_default_terrain_texture(element: &Path) -> String {
    let pck_path = element.join("textures.pck");
    if let Ok(mut file) = File::open(&pck_path) {
        if let Ok(entries) = pck::directory(&mut file) {
            // Prefer a grass/ground/terrain DDS texture if present.
            for preferred in ["grass", "ground", "terrain", "earth", "dirt"] {
                if let Some(entry) = entries.iter().find(|e| {
                    let lower = e.name.to_ascii_lowercase();
                    lower.ends_with(".dds") && lower.contains(preferred)
                }) {
                    if let Ok(raw) = pck::read_entry(&mut file, entry) {
                        if crate::terrain_sampling::parse_terrain_dds(&raw).is_ok() {
                            return entry.name.clone();
                        }
                    }
                }
            }
            // Fallback: first valid DXT terrain DDS in textures.pck.
            for entry in entries.iter().take(256) {
                if entry.name.to_ascii_lowercase().ends_with(".dds") {
                    if let Ok(raw) = pck::read_entry(&mut file, entry) {
                        if crate::terrain_sampling::parse_terrain_dds(&raw).is_ok() {
                            return entry.name.clone();
                        }
                    }
                }
            }
        }
    }
    "textures/terrain/grass.dds".into()
}

/// Export an Architect world as a real game map under `element/maps/<map_id>/`.
///
/// Produces:
/// - `<map_id>.trn2` (TRN2 v1)
/// - `<map_id>_0.t2bk` (T2BK v5, zlib heights & float normals)
/// - `<map_id>_0.t2mk` (T2MK v2, 1 base layer per mask area)
/// - `<map_id>.ecwld` (ECWLD v12)
/// - `<map_id>.ecbsd` (ECBSD v12)
/// - `precinct.clt` (display name for `world_catalog::discover`)
pub fn export_as_game_map(
    element: &Path,
    doc: &WorldDocument,
    requested_name: &str,
) -> Result<ExportedMap, String> {
    let display_name = if requested_name.trim().is_empty() {
        doc.name.trim()
    } else {
        requested_name.trim()
    };
    let map_id = sanitize_world_slug(display_name);
    if !world_catalog::valid_world_id(&map_id) {
        return Err(format!("invalid game map ID '{map_id}'"));
    }
    if map_id.eq_ignore_ascii_case("world") {
        return Err(
            "cannot overwrite the main 'world' map directly; choose a custom map name".into(),
        );
    }

    // Ensure dimensions are multiples of BLOCK_GRID (32).
    let terrain = &doc.terrain;
    let cols = (terrain.cells_x / BLOCK_GRID).max(1);
    let rows = (terrain.cells_z / BLOCK_GRID).max(1);
    let cells_x = cols * BLOCK_GRID;
    let cells_z = rows * BLOCK_GRID;
    let spacing = terrain.spacing.max(0.5);
    // Keep one subterrain covering the entire editable map (subgrid must be square and divisible by 32).
    let subgrid = cells_x.max(cells_z);
    let sub_cols = (subgrid / BLOCK_GRID).max(1);
    let export_cols = sub_cols;
    let export_rows = sub_cols;
    let export_w = export_cols * BLOCK_GRID;
    let export_h = export_rows * BLOCK_GRID;
    let mask_grid = if subgrid >= 64 { 64 } else { BLOCK_GRID };
    let sub_name = format!("{map_id}_0");

    let map_dir = if element.as_os_str().is_empty() {
        PathBuf::from("maps").join(&map_id)
    } else {
        element.join("maps").join(&map_id)
    };
    fs::create_dir_all(&map_dir).map_err(|e| format!("create {}: {e}", map_dir.display()))?;

    // 1. Write <map_id>.trn2
    let texture_path = discover_default_terrain_texture(element);
    let trn2_bytes = build_trn2(
        export_rows,
        export_cols,
        subgrid,
        mask_grid,
        spacing,
        &texture_path,
        &sub_name,
    );
    fs::write(map_dir.join(format!("{map_id}.trn2")), &trn2_bytes)
        .map_err(|e| format!("write {map_id}.trn2: {e}"))?;

    // 2. Write <map_id>_0.t2bk
    let t2bk_bytes = build_t2bk(terrain, export_rows, export_cols, spacing)?;
    fs::write(map_dir.join(format!("{sub_name}.t2bk")), &t2bk_bytes)
        .map_err(|e| format!("write {sub_name}.t2bk: {e}"))?;

    // 3. Write <map_id>_0.t2mk
    let sub_areas = (subgrid / mask_grid).max(1);
    let t2mk_bytes = build_t2mk(sub_areas);
    fs::write(map_dir.join(format!("{sub_name}.t2mk")), &t2mk_bytes)
        .map_err(|e| format!("write {sub_name}.t2mk: {e}"))?;

    // 4 & 5. Write <map_id>.ecbsd and <map_id>.ecwld
    let world_width = export_w as f32 * spacing;
    let world_length = export_h as f32 * spacing;
    let (ecwld_bytes, ecbsd_bytes, stats) = build_ecwld_and_ecbsd(
        doc,
        world_width,
        world_length,
        export_rows,
        export_cols,
        BLOCK_GRID as f32 * spacing,
    );
    fs::write(map_dir.join(format!("{map_id}.ecbsd")), &ecbsd_bytes)
        .map_err(|e| format!("write {map_id}.ecbsd: {e}"))?;
    fs::write(map_dir.join(format!("{map_id}.ecwld")), &ecwld_bytes)
        .map_err(|e| format!("write {map_id}.ecwld: {e}"))?;

    // 6. Write precinct.clt with display name so world_catalog shows a clean name
    let safe_display = display_name.replace('"', "'");
    let precinct = format!("// Generated by Architect\n\"{safe_display}\"\n{{\n}}\n");
    let _ = fs::write(map_dir.join("precinct.clt"), precinct);

    Ok(ExportedMap {
        map_id,
        dir: map_dir,
        ornaments: stats.0,
        models: stats.1,
        trees: stats.2,
        effects: stats.3,
        sounds: stats.4,
    })
}

fn build_trn2(
    rows: usize,
    cols: usize,
    subgrid: usize,
    mask_grid: usize,
    grid_spacing: f32,
    texture_path: &str,
    subterrain_name: &str,
) -> Vec<u8> {
    let mut out = vec![0u8; 88];
    let put_u32 = |buf: &mut [u8], off: usize, v: u32| {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    };
    let put_f32 = |buf: &mut [u8], off: usize, v: f32| {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    };

    put_u32(&mut out, 0, TRN2_MAGIC);
    put_u32(&mut out, 4, 1); // version 1
    put_u32(&mut out, 20, BLOCK_GRID as u32);
    put_u32(&mut out, 28, rows as u32);
    put_u32(&mut out, 32, cols as u32);
    put_u32(&mut out, 36, 1); // texture count = 1
    put_u32(&mut out, 48, mask_grid as u32);
    put_u32(&mut out, 52, 1); // subterrain count = 1
    put_u32(&mut out, 64, subgrid as u32);
    put_f32(&mut out, 68, grid_spacing);

    let tex_off = out.len() as u32;
    put_u32(&mut out, 72, tex_off);
    push_gbk_string(&mut out, texture_path);

    put_u32(&mut out, 76, 1); // loader = 1
    let loader_off = out.len() as u32;
    put_u32(&mut out, 80, loader_off);
    out.extend_from_slice(&1u32.to_le_bytes()); // loader subterrain count header word
    push_gbk_string(&mut out, subterrain_name);

    out
}

fn build_t2bk(
    terrain: &EditableTerrain,
    rows: usize,
    cols: usize,
    spacing: f32,
) -> Result<Vec<u8>, String> {
    let block_count = rows * cols;
    let nv = (BLOCK_GRID + 1) * (BLOCK_GRID + 1);
    let header_len = 32usize;
    let table_end = 16 + block_count * 4;
    let mut out = vec![0u8; table_end];

    out[0..4].copy_from_slice(&T2BK_MAGIC.to_le_bytes());
    out[4..8].copy_from_slice(&5u32.to_le_bytes()); // T2BK v5
    out[8..12].copy_from_slice(&(block_count as u32).to_le_bytes());
    // Compressed heights (0x04) + compressed float3 normals (0x10).
    out[12] = terrain_block::FLAG_COMPRESSED | terrain_block::FLAG_COMPRESSED_NORMALS;

    let half_w = (cols * BLOCK_GRID) as f32 * spacing * 0.5;
    let half_h = (rows * BLOCK_GRID) as f32 * spacing * 0.5;

    for bi in 0..block_count {
        let br = bi / cols;
        let bc = bi % cols;
        let block_offset = out.len() as u32;
        out[16 + bi * 4..20 + bi * 4].copy_from_slice(&block_offset.to_le_bytes());

        let mut raw_heights = Vec::with_capacity(nv * 4);
        let mut raw_normals = Vec::with_capacity(nv * 12);
        for ly in 0..=BLOCK_GRID {
            for lx in 0..=BLOCK_GRID {
                let gx = bc * BLOCK_GRID + lx;
                let gz = br * BLOCK_GRID + ly;
                let wx = gx as f32 * spacing - half_w;
                let wz = gz as f32 * spacing - half_h;
                let h = terrain.sample_height(wx, wz);
                let n = terrain.sample_normal(wx, wz);
                raw_heights.extend_from_slice(&h.to_le_bytes());
                raw_normals.extend_from_slice(&n.x.to_le_bytes());
                raw_normals.extend_from_slice(&n.y.to_le_bytes());
                raw_normals.extend_from_slice(&n.z.to_le_bytes());
            }
        }

        let comp_h = zlib_compress(&raw_heights)?;
        let comp_n = zlib_compress(&raw_normals)?;

        let mut blk_head = vec![0u8; header_len];
        blk_head[16..20].copy_from_slice(&(comp_h.len() as u32).to_le_bytes());
        blk_head[20..24].copy_from_slice(&(comp_n.len() as u32).to_le_bytes());
        out.extend_from_slice(&blk_head);
        out.extend_from_slice(&comp_h);
        out.extend_from_slice(&comp_n);
    }

    Ok(out)
}

fn build_t2mk(sub_areas: usize) -> Vec<u8> {
    let area_count = sub_areas * sub_areas;
    let layer_count = 1usize;
    let layer_table_start = 20 + area_count * 4;
    let area_records_start = layer_table_start + layer_count * 28;
    let mut out = vec![0u8; area_records_start];

    out[0..4].copy_from_slice(&T2MK_MAGIC.to_le_bytes());
    out[4..8].copy_from_slice(&2u32.to_le_bytes()); // version 2
    out[8..12].copy_from_slice(&(area_count as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(layer_count as u32).to_le_bytes());
    out[16..20].copy_from_slice(&0u32.to_le_bytes()); // uncompressed

    // Single layer descriptor (28 bytes): texture=0, mask_size=0, axis=1 (XZ), scale u=0.1, v=0.1
    let lp = layer_table_start;
    out[lp..lp + 4].copy_from_slice(&0u32.to_le_bytes()); // texture index 0
    out[lp + 8..lp + 12].copy_from_slice(&0u32.to_le_bytes()); // mask size (base layer = 0)
    out[lp + 12] = 1; // axis = 1 (XZ projection)
    out[lp + 16..lp + 20].copy_from_slice(&0.1f32.to_le_bytes()); // u scale
    out[lp + 20..lp + 24].copy_from_slice(&0.1f32.to_le_bytes()); // v scale

    for ai in 0..area_count {
        let off = out.len() as u32;
        out[20 + ai * 4..24 + ai * 4].copy_from_slice(&off.to_le_bytes());
        // Area record: n=1 layer, followed by 12-byte layer ref (idx=0, off=0, len=0)
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }

    out
}

fn build_ecwld_and_ecbsd(
    doc: &WorldDocument,
    width: f32,
    length: f32,
    rows: usize,
    cols: usize,
    block_size: f32,
) -> (Vec<u8>, Vec<u8>, (usize, usize, usize, usize, usize)) {
    let block_count = rows * cols;
    let half_w = width * 0.5;
    let half_l = length * 0.5;

    // Build ECBSD header (12 bytes: MOXB, BSDF, version 12)
    let mut ecbsd = Vec::new();
    ecbsd.extend_from_slice(&ECBSD_MAGIC_0.to_le_bytes());
    ecbsd.extend_from_slice(&ECBSD_MAGIC_1.to_le_bytes());
    ecbsd.extend_from_slice(&12u32.to_le_bytes());

    let mut per_block_ornaments: Vec<Vec<(u32, u32)>> = vec![Vec::new(); block_count];
    let mut per_block_models: Vec<Vec<&PlacedObject>> = vec![Vec::new(); block_count];
    let mut per_block_trees: Vec<Vec<(u32, [f32; 3])>> = vec![Vec::new(); block_count];
    let mut per_block_effects: Vec<Vec<&PlacedObject>> = vec![Vec::new(); block_count];
    let mut per_block_sounds: Vec<Vec<&PlacedObject>> = vec![Vec::new(); block_count];

    let mut tree_types: Vec<String> = Vec::new();
    let mut tree_type_map: HashMap<String, u32> = HashMap::new();

    let mut n_orn = 0usize;
    let mut n_mod = 0usize;
    let mut n_tree = 0usize;
    let mut n_fx = 0usize;
    let mut n_sfx = 0usize;
    let mut export_id = 1u32;

    for obj in &doc.objects {
        if !obj.visible {
            continue;
        }
        let bx = (((obj.position[0] + half_w) / block_size).floor() as isize)
            .clamp(0, cols.saturating_sub(1) as isize) as usize;
        let bz = (((obj.position[2] + half_l) / block_size).floor() as isize)
            .clamp(0, rows.saturating_sub(1) as isize) as usize;
        let bi = bz * cols + bx;

        match obj.kind {
            ObjectKind::BuildingBmd if obj.asset_path.to_ascii_lowercase().ends_with(".bmd") => {
                let off = ecbsd.len() as u32;
                let (dir, up) = obj.direction_and_up();
                push_vec3(&mut ecbsd, obj.position);
                push_vec3(&mut ecbsd, dir);
                push_vec3(&mut ecbsd, up);
                ecbsd.extend_from_slice(&0u32.to_le_bytes()); // extra
                push_gbk_string(&mut ecbsd, &obj.asset_path);
                per_block_ornaments[bi].push((export_id, off));
                export_id += 1;
                n_orn += 1;
            }
            ObjectKind::ModelEcm
                if obj.asset_path.to_ascii_lowercase().ends_with(".ecm")
                    || obj.asset_path.to_ascii_lowercase().ends_with(".smd") =>
            {
                per_block_models[bi].push(obj);
                n_mod += 1;
            }
            ObjectKind::Tree if obj.asset_path.to_ascii_lowercase().ends_with(".spt") => {
                let key = obj.asset_path.to_ascii_lowercase();
                let tid = *tree_type_map.entry(key).or_insert_with(|| {
                    let id = tree_types.len() as u32;
                    tree_types.push(obj.asset_path.clone());
                    id
                });
                per_block_trees[bi].push((tid, obj.position));
                n_tree += 1;
            }
            ObjectKind::EffectGfx if obj.asset_path.to_ascii_lowercase().ends_with(".gfx") => {
                per_block_effects[bi].push(obj);
                n_fx += 1;
            }
            ObjectKind::SoundSfx
                if obj.asset_path.to_ascii_lowercase().ends_with(".wav")
                    || obj.asset_path.to_ascii_lowercase().ends_with(".mp3") =>
            {
                per_block_sounds[bi].push(obj);
                n_sfx += 1;
            }
            _ => {}
        }
    }

    // Build ECWLD v12
    let mut ecwld = vec![0u8; 116 + block_count * 4];
    let put_u32 = |buf: &mut [u8], off: usize, v: u32| {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    };
    let put_f32 = |buf: &mut [u8], off: usize, v: f32| {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    };

    put_u32(&mut ecwld, 0, ECWLD_MAGIC);
    put_u32(&mut ecwld, 4, 12); // version 12
    put_f32(&mut ecwld, 8, width);
    put_f32(&mut ecwld, 12, length);
    put_f32(&mut ecwld, 16, block_size);
    put_u32(&mut ecwld, 24, block_count as u32);
    put_u32(&mut ecwld, 28, rows as u32);
    put_u32(&mut ecwld, 32, cols as u32);

    let mut next_inline_id = export_id;
    for bi in 0..block_count {
        let row = (bi / cols) as u16;
        let col = (bi % cols) as u16;
        let block_off = ecwld.len() as u32;
        put_u32(&mut ecwld, 116 + bi * 4, block_off);

        ecwld.extend_from_slice(&row.to_le_bytes());
        ecwld.extend_from_slice(&col.to_le_bytes());

        let counts: [u32; 10] = [
            per_block_trees[bi].len() as u32,     // 0: trees
            0,                                    // 1: water
            0,                                    // 2: grass
            per_block_ornaments[bi].len() as u32, // 3: ornaments
            0,                                    // 4: boxes
            per_block_effects[bi].len() as u32,   // 5: effects
            per_block_models[bi].len() as u32,    // 6: models
            0,                                    // 7: critters
            0,                                    // 8: beziers
            per_block_sounds[bi].len() as u32,    // 9: sounds
        ];
        for c in counts {
            ecwld.extend_from_slice(&c.to_le_bytes());
        }

        // Order in ECWLD v12 block: trees, water, ornaments, boxes, grass, effects, models, critters, beziers, sounds
        for &(tid, pos) in &per_block_trees[bi] {
            ecwld.extend_from_slice(&tid.to_le_bytes());
            push_vec3(&mut ecwld, pos);
        }
        for &(eid, off) in &per_block_ornaments[bi] {
            ecwld.extend_from_slice(&eid.to_le_bytes());
            ecwld.extend_from_slice(&off.to_le_bytes());
        }
        for obj in &per_block_effects[bi] {
            let (dir, up) = obj.direction_and_up();
            let scale = ((obj.scale[0] + obj.scale[1] + obj.scale[2]) / 3.0).max(0.05);
            ecwld.extend_from_slice(&next_inline_id.to_le_bytes());
            next_inline_id += 1;
            ecwld.extend_from_slice(&scale.to_le_bytes());
            push_vec3(&mut ecwld, obj.position);
            push_vec3(&mut ecwld, dir);
            push_vec3(&mut ecwld, up);
            ecwld.extend_from_slice(&1.0f32.to_le_bytes()); // speed
            ecwld.extend_from_slice(&2i32.to_le_bytes()); // valid_time = all day
            ecwld.extend_from_slice(&1.0f32.to_le_bytes()); // alpha
            ecwld.extend_from_slice(&[1u8, 0, 0, 0]); // attenuation + padding
            push_gbk_string(&mut ecwld, &obj.asset_path);
        }
        for obj in &per_block_models[bi] {
            let (dir, up) = obj.direction_and_up();
            ecwld.extend_from_slice(&next_inline_id.to_le_bytes());
            next_inline_id += 1;
            push_vec3(&mut ecwld, obj.position);
            push_vec3(&mut ecwld, dir);
            push_vec3(&mut ecwld, up);
            push_gbk_string(&mut ecwld, &obj.asset_path);
            push_gbk_string(&mut ecwld, ""); // default action
        }
        for obj in &per_block_sounds[bi] {
            ecwld.extend_from_slice(&next_inline_id.to_le_bytes());
            next_inline_id += 1;
            push_vec3(&mut ecwld, obj.position);
            ecwld.extend_from_slice(&5.0f32.to_le_bytes()); // min_distance
            ecwld.extend_from_slice(&45.0f32.to_le_bytes()); // max_distance
            ecwld.extend_from_slice(&0u32.to_le_bytes()); // reserved
            ecwld.extend_from_slice(&0i32.to_le_bytes()); // mode
            push_gbk_string(&mut ecwld, &obj.asset_path);
        }
    }

    let tree_off = ecwld.len() as u32;
    for path in &tree_types {
        push_gbk_string(&mut ecwld, path);
        push_gbk_string(&mut ecwld, "");
    }
    let grass_off = ecwld.len() as u32;

    put_u32(&mut ecwld, 36, tree_off);
    put_u32(&mut ecwld, 40, grass_off);
    put_u32(&mut ecwld, 44, tree_types.len() as u32);
    put_u32(&mut ecwld, 48, 0); // grass_count

    (ecwld, ecbsd, (n_orn, n_mod, n_tree, n_fx, n_sfx))
}

// ---------------------------------------------------------------------------
// Import an existing client map into Architect
// ---------------------------------------------------------------------------

fn u32at(b: &[u8], p: usize) -> Result<u32, String> {
    let end = p.checked_add(4).ok_or("overflow")?;
    Ok(u32::from_le_bytes(
        b.get(p..end).ok_or("truncated")?.try_into().unwrap(),
    ))
}

fn f32at(b: &[u8], p: usize) -> Result<f32, String> {
    let v = f32::from_bits(u32at(b, p)?);
    if !v.is_finite() {
        return Err("non-finite float".into());
    }
    Ok(v)
}

fn read_gbk(b: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = u32at(b, *pos)? as usize;
    *pos += 4;
    if len > 4096 {
        return Err("oversized TRN2 string".into());
    }
    let end = pos.checked_add(len).ok_or("overflow")?;
    let bytes = b.get(*pos..end).ok_or("truncated string")?;
    *pos = end;
    let (s, _, _) = encoding_rs::GBK.decode(bytes);
    Ok(s.replace('\\', "/"))
}

/// Load a client map (`WorldMap`) into an editable Architect `WorldDocument`.
///
/// Downsamples very large worlds (like the 8x11 mainland) onto Architect's
/// interactive 128x128 editing grid while keeping full real-world scale and
/// importing placed ornaments, models, trees, effects, and sounds (capped to
/// keep the interactive viewport responsive).
pub fn import_game_map(map: &world_catalog::WorldMap) -> Result<WorldDocument, String> {
    let dir = &map.dir;
    let trn_path = world_catalog::data_file(dir, "trn2");
    let trn = fs::read(&trn_path).map_err(|e| format!("{}: {e}", trn_path.display()))?;
    if u32at(&trn, 0)? != TRN2_MAGIC || u32at(&trn, 4)? > 1 {
        return Err("unsupported TRN2 header".into());
    }
    let grid = u32at(&trn, 20)? as usize;
    let rows = u32at(&trn, 28)? as usize;
    let cols = u32at(&trn, 32)? as usize;
    let count = u32at(&trn, 52)? as usize;
    let subgrid = u32at(&trn, 64)? as usize;
    let grid_spacing = f32at(&trn, 68)?;
    let loff = u32at(&trn, 80)? as usize;
    if grid == 0 || subgrid == 0 || subgrid % grid != 0 || rows == 0 || cols == 0 {
        return Err("invalid TRN2 terrain dimensions".into());
    }
    let per = subgrid / grid;
    let subs_x = (cols / per).max(1);
    let src_w = cols * grid;
    let src_h = rows * grid;
    let world_w = src_w as f32 * grid_spacing;
    let world_h = src_h as f32 * grid_spacing;

    let mut pos = loff + 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(read_gbk(&trn, &mut pos)?);
    }

    // Target editable grid: preserve native size if <= 128, else resample onto 128x128.
    let dst_cells_x = src_w.min(DEFAULT_TERRAIN_CELLS).max(32);
    let dst_cells_z = src_h.min(DEFAULT_TERRAIN_CELLS).max(32);
    let dst_spacing = (world_w / dst_cells_x as f32).max(grid_spacing);
    let mut terrain = EditableTerrain::new_flat(dst_cells_x, dst_cells_z, dst_spacing);

    // Sample block heights into the editable grid.
    let vx = terrain.verts_x();
    let mut sum_h = vec![0.0f32; terrain.heights.len()];
    let mut cnt_h = vec![0u32; terrain.heights.len()];

    for (si, name) in names.iter().enumerate() {
        let t2bk_path = dir.join(format!("{name}.t2bk"));
        let Ok(data) = fs::read(&t2bk_path) else {
            continue;
        };
        let Ok(layout) = terrain_block::layout(&data, grid) else {
            continue;
        };
        let nv = (grid + 1) * (grid + 1);
        let mut decoded = Vec::new();
        for bi in 0..(per * per).min(layout.blocks.len()) {
            let sec = layout.blocks[bi];
            let raw = &data[sec.heights.0..sec.heights.1];
            decoded.clear();
            if layout.flags & terrain_block::FLAG_COMPRESSED != 0 {
                if ZlibDecoder::new(raw)
                    .take((nv * 4 + 64) as u64)
                    .read_to_end(&mut decoded)
                    .is_err()
                {
                    continue;
                }
            } else {
                decoded.extend_from_slice(raw);
            }
            if decoded.len() < nv * 4 {
                continue;
            }
            let br = (si / subs_x) * per + bi / per;
            let bc = (si % subs_x) * per + bi % per;
            let stride = ((src_w / dst_cells_x).max(1)).min(grid);
            for ly in (0..=grid).step_by(stride) {
                for lx in (0..=grid).step_by(stride) {
                    let gx = bc * grid + lx;
                    let gz = br * grid + ly;
                    let dx = (gx * dst_cells_x / src_w.max(1)).min(dst_cells_x);
                    let dz = (gz * dst_cells_z / src_h.max(1)).min(dst_cells_z);
                    let j = ly * (grid + 1) + lx;
                    if let Ok(h) = f32at(&decoded, j * 4) {
                        let idx = dz * vx + dx;
                        sum_h[idx] += h;
                        cnt_h[idx] += 1;
                    }
                }
            }
        }
    }

    for i in 0..terrain.heights.len() {
        if cnt_h[i] > 0 {
            terrain.heights[i] = sum_h[i] / cnt_h[i] as f32;
            terrain.paint_layers[i] = if terrain.heights[i] > 180.0 {
                PaintLayer::Snow as u8
            } else if terrain.heights[i] > 90.0 {
                PaintLayer::Rock as u8
            } else if terrain.heights[i] < 2.0 {
                PaintLayer::Sand as u8
            } else {
                PaintLayer::Grass as u8
            };
        }
    }

    // Load scene objects (ornaments, models, trees, effects, sounds).
    let mut objects = Vec::new();
    let mut next_id = 1u64;
    let max_import_objects = 2500usize;

    let ecwld_path = world_catalog::data_file(dir, "ecwld");
    if let Ok(scene) = Scene::load(&ecwld_path) {
        if let Ok(ornaments) = scene.load_ornaments(&world_catalog::ecbsd_path(dir)) {
            for orn in ornaments.into_iter().take(max_import_objects) {
                let mut obj = PlacedObject::new(next_id, orn.path, glam::Vec3::from(orn.position));
                obj.kind = ObjectKind::BuildingBmd;
                obj.rotation_deg = [
                    0.0,
                    orn.direction[0].atan2(orn.direction[2]).to_degrees(),
                    0.0,
                ];
                next_id += 1;
                objects.push(obj);
            }
        }
        'blocks: for block in &scene.blocks {
            for m in &block.models {
                if objects.len() >= max_import_objects {
                    break 'blocks;
                }
                let mut obj =
                    PlacedObject::new(next_id, m.path.clone(), glam::Vec3::from(m.position));
                obj.kind = ObjectKind::ModelEcm;
                next_id += 1;
                objects.push(obj);
            }
            for t in &block.trees {
                if objects.len() >= max_import_objects {
                    break 'blocks;
                }
                let path = scene
                    .tree_types
                    .get(t.tree_type as usize)
                    .map(|tt| tt.tree.clone())
                    .unwrap_or_else(|| format!("trees/type_{}.spt", t.tree_type));
                let mut obj = PlacedObject::new(next_id, path, glam::Vec3::from(t.position));
                obj.kind = ObjectKind::Tree;
                next_id += 1;
                objects.push(obj);
            }
            for fx in &block.effects {
                if objects.len() >= max_import_objects {
                    break 'blocks;
                }
                let mut obj =
                    PlacedObject::new(next_id, fx.path.clone(), glam::Vec3::from(fx.position));
                obj.kind = ObjectKind::EffectGfx;
                obj.scale = [fx.scale.clamp(0.1, 20.0); 3];
                next_id += 1;
                objects.push(obj);
            }
            for s in &block.sounds {
                if objects.len() >= max_import_objects {
                    break 'blocks;
                }
                let mut obj =
                    PlacedObject::new(next_id, s.path.clone(), glam::Vec3::from(s.position));
                obj.kind = ObjectKind::SoundSfx;
                next_id += 1;
                objects.push(obj);
            }
        }
    }

    let _ = world_h;
    Ok(WorldDocument::from_parts(
        map.display_name.clone(),
        Some(map.id.clone()),
        terrain,
        objects,
        next_id,
    ))
}
