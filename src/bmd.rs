//! Binary A3DLitModel reader for world BMDs from this client.
//! Layout informed by GameForge's GPL-2.0-or-later Angelica importer.
#[derive(Clone)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub normal: [f32; 3],
    pub diffuse: u32,
    pub day_diffuse: u32,
    pub night_diffuse: u32,
}
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub emissive: [f32; 4],
    pub specular: [f32; 4],
    pub power: f32,
    pub two_sided: bool,
}
/// Which colour/lightmap-UV branch a v300+ (`A3DLITMODEL_VERSION_EX`) mesh took,
/// driven directly by `bSupportLightMap` and `bUseLightMap` per Angelica 2.2
/// `A3DLitModel.cpp:2727-2822`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V300ColorBranch {
    /// `!bSupportLightMap`: baked day/night vertex colours only (`n * 8` bytes).
    VertexColorsOnly,
    /// `bSupportLightMap && !bUseLightMap`: lightmap UVs (`n * 8`) followed by
    /// baked day/night vertex colours (`n * 8`).
    LightmapUvAndVertexColors,
    /// `bSupportLightMap && bUseLightMap`: lightmap UVs (`n * 8`) only; neutral
    /// `0xFF808080` fallback (1.0x under `MODULATE2X`).
    LightmapUvNeutralFallback,
}

impl V300ColorBranch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VertexColorsOnly => "colours_only",
            Self::LightmapUvAndVertexColors => "uv_and_colours",
            Self::LightmapUvNeutralFallback => "lightmap_neutral",
        }
    }
}

/// Diagnostic counters for each v300 colour/lightmap branch taken while parsing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V300BranchCounters {
    pub flags_read: usize,
    pub uv_block_skipped: usize,
    pub colours_read: usize,
    pub neutral_fallback: usize,
}

/// Returns true when `PWE_BMD_COLOUR_LOG=1` is set in the environment.
pub fn colour_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("PWE_BMD_COLOUR_LOG").is_some_and(|v| v == "1"))
}

pub struct Mesh {
    pub name: String,
    pub version: u32,
    pub material: Option<Material>,
    pub texture: String,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
    /// Flag-selected colour branch for v300+ meshes (`None` for legacy v2..=v6).
    pub color_branch: Option<V300ColorBranch>,
    /// `bSupportLightMap` flag read from v300+ mesh header (false for legacy).
    pub b_support_lightmap: bool,
    /// `bUseLightMap` flag read from v300+ mesh header (false for legacy).
    pub b_use_lightmap: bool,
    /// Count of vertices with `(day_diffuse >> 24) == 0xff`.
    pub opaque_day_words: usize,
    /// Count of vertices with `(night_diffuse >> 24) == 0xff`.
    pub opaque_night_words: usize,
    /// True when the mesh label contained invalid GBK bytes and was lossily decoded.
    pub name_lossy_gbk: bool,
    /// True when the texture path contained invalid GBK bytes and was lossily decoded.
    pub texture_lossy_gbk: bool,
}

impl Mesh {
    /// Format one diagnostic line for `PWE_BMD_COLOUR_LOG=1`.
    pub fn colour_log_line(&self, model_name: &str, mesh_index: usize) -> String {
        let branch = self.color_branch.map_or("legacy", V300ColorBranch::as_str);
        let first_day = self.vertices.first().map_or(0, |v| v.day_diffuse);
        let first_night = self.vertices.first().map_or(0, |v| v.night_diffuse);
        format!(
            "bmd_colour model={model_name} mesh={mesh_index} branch={branch} opaque_day={}/{} opaque_night={}/{} first_day={first_day:#010x} first_night={first_night:#010x} tex={}",
            self.opaque_day_words,
            self.vertices.len(),
            self.opaque_night_words,
            self.vertices.len(),
            self.texture,
        )
    }
}

pub struct Model {
    pub scale: [f32; 3],
    pub direction: [f32; 3],
    pub up: [f32; 3],
    pub position: [f32; 3],
    pub meshes: Vec<Mesh>,
    /// Native `CELBrushBuilding` convex collision brushes stored after the
    /// lit-model meshes. These are world-baked just like the BMD vertices.
    pub collision: Vec<CollisionBrush>,
    /// Diagnostic lines for mesh or texture names that needed lossy GBK decoding.
    pub lossy_gbk_diagnostics: Vec<String>,
    /// Counts of v300 colour/lightmap branches taken across meshes in this model.
    pub v300_branch_counters: V300BranchCounters,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrushSide {
    pub normal: [f32; 3],
    pub distance: f32,
    pub bevel: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollisionBrush {
    pub center: [f32; 3],
    pub extents: [f32; 3],
    pub flags: u32,
    pub sides: Vec<BrushSide>,
}

impl CollisionBrush {
    /// `CHBasedCD::CCDBrush::Load`: A3DAABB (four vectors), flags, side
    /// count, then 17-byte plane/bevel records. Sharing this reader with TBH
    /// tree collision keeps both native containers on the same layout.
    pub fn read(b: &[u8], p: &mut usize) -> Result<Self, String> {
        let center = v3(b, *p)?;
        let extents = v3(b, *p + 12)?;
        let _mins = v3(b, *p + 24)?;
        let _maxs = v3(b, *p + 36)?;
        if extents.iter().any(|v| *v < 0.0) {
            return Err("negative collision-brush extent".into());
        }
        *p = p.checked_add(48).ok_or("collision-brush overflow")?;
        let flags = u(b, *p)?;
        *p += 4;
        let count = u(b, *p)? as usize;
        *p += 4;
        if count == 0 || count > 200 {
            return Err(format!("invalid collision-brush side count {count}"));
        }
        let mut sides = Vec::with_capacity(count);
        for _ in 0..count {
            let normal = v3(b, *p)?;
            let distance = f(b, *p + 12)?;
            if normal.iter().map(|v| v * v).sum::<f32>() < 1e-12 {
                return Err("zero collision-plane normal".into());
            }
            let bevel = *b.get(*p + 16).ok_or("truncated collision plane")? != 0;
            *p = p.checked_add(17).ok_or("collision-plane overflow")?;
            sides.push(BrushSide {
                normal,
                distance,
                bevel,
            });
        }
        Ok(Self {
            center,
            extents,
            flags,
            sides,
        })
    }
}
fn u(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p.checked_add(4).ok_or("overflow")?)
            .ok_or("truncated BMD")?
            .try_into()
            .unwrap(),
    ))
}
fn f(b: &[u8], p: usize) -> Result<f32, String> {
    let v = f32::from_bits(u(b, p)?);
    if !v.is_finite() {
        return Err("non-finite BMD float".into());
    }
    Ok(v)
}
fn v3(b: &[u8], p: usize) -> Result<[f32; 3], String> {
    Ok([f(b, p)?, f(b, p + 4)?, f(b, p + 8)?])
}
fn fixed_lossy_tracked(b: &[u8], p: usize, n: usize) -> Result<(String, bool), String> {
    let s = b
        .get(p..p.checked_add(n).ok_or("overflow")?)
        .ok_or("truncated string")?;
    let end = s.iter().position(|&c| c == 0).unwrap_or(n);
    let (t, _, bad) = encoding_rs::GBK.decode(&s[..end]);
    Ok((t.replace('\\', "/"), bad))
}
fn parse_material(b: &[u8], p: &mut usize) -> Result<Material, String> {
    let tail = b.get(*p..).ok_or("truncated material")?;
    let len = tail
        .iter()
        .position(|&c| c == 0)
        .ok_or("unterminated material name")?;
    if len > 4096 {
        return Err("oversized BMD material name".into());
    }
    let (name, _) = fixed_lossy_tracked(b, *p, len)?;
    *p = p.checked_add(len + 1).ok_or("material overflow")?;
    let color = |off| -> Result<[f32; 4], String> {
        Ok([f(b, off)?, f(b, off + 4)?, f(b, off + 8)?, f(b, off + 12)?])
    };
    let m = Material {
        name,
        ambient: color(*p)?,
        diffuse: color(*p + 16)?,
        emissive: color(*p + 32)?,
        specular: color(*p + 48)?,
        power: f(b, *p + 64)?,
        two_sided: *b.get(*p + 68).ok_or("truncated material")? != 0,
    };
    *p = p.checked_add(69).ok_or("material overflow")?;
    Ok(m)
}
fn parse_collision(b: &[u8], mut p: usize) -> Result<Vec<CollisionBrush>, String> {
    let mut collision = Vec::new();
    let hull_count = u(b, p)? as usize;
    p += 4;
    if hull_count > 65_536 {
        return Err(format!("BMD collision-hull limit exceeded: {hull_count}"));
    }
    for _ in 0..hull_count {
        let count = u(b, p)? as usize;
        p += 4;
        if count > 1_000_000 {
            return Err(format!("BMD collision mesh-list limit exceeded: {count}"));
        }
        let bytes = count.checked_mul(4).ok_or("BMD collision list overflow")?;
        p = p.checked_add(bytes).ok_or("BMD collision list overflow")?;
        if p > b.len() {
            return Err("truncated BMD collision mesh list".into());
        }
    }
    for _ in 0..hull_count {
        collision.push(CollisionBrush::read(b, &mut p)?);
    }
    Ok(collision)
}
fn count_opaque_words(vertices: &[Vertex]) -> (usize, usize) {
    let mut day = 0usize;
    let mut night = 0usize;
    for v in vertices {
        if (v.day_diffuse >> 24) == 0xff {
            day += 1;
        }
        if (v.night_diffuse >> 24) == 0xff {
            night += 1;
        }
    }
    (day, night)
}
impl Model {
    /// Mirrors the retail loaders exactly:
    /// - file magic "MOXB" (u32 0x42584f4d) is the AFileImage binary marker,
    ///   sniffed by AFileImage::Open at 0x76da7c;
    /// - the optional chunk tag 0x80000001 plus one flag byte is read by the
    ///   scenery loader 0x64e200 (0x64e261/0x64e278); the byte is stored but
    ///   never used (corpus values: 0 and 1 only);
    /// - A3DLitModel::Load (0x7bb380) accepts model versions 0x10000001
    ///   (0x7bb3de) and 0x10000002 (0x7bb58b) with an identical header and
    ///   logs "A3DLitModel::Load(), Wrong version!" for anything else;
    /// - A3DLitMesh::Load (0x7ba070) implements mesh versions 0x10000002
    ///   (0x7ba0b8), 0x10000003 (0x7ba30d), 0x10000004 (0x7ba573) and
    ///   0x10000005 (0x7ba7a7). v2/v3 use 28-byte vertex records and have no
    ///   material tail; v3 carries the same two packed per-vertex light arrays
    ///   as v4/v5, which v2 does not. The normal block (n*12) is separate and
    ///   present in every version. These advance sizes are the same ones
    ///   `src/bin/bmd-corpus-census.rs` walks, so the two cannot drift apart.
    ///   Version 0x10000001 returns false without logging (0x7baa64); any
    ///   other version logs "Unknown version [%x]!" (0x7baa51).
    ///
    ///   Corpus status: v2/v3 have zero occurrences in `litmodels.pck`,
    ///   `building.pck` and `models.pck` (the last contains no BMDs at all),
    ///   so their handling is specified from the client's advance sizes and
    ///   covered by synthetic tests rather than by real data. That is stated
    ///   here so the distinction between "implemented" and "seen in the wild"
    ///   stays visible.
    /// - After the last mesh, `CELBrushBuilding::Load` reads its native convex
    ///   collision trailer: u32 hull count, then per hull [u32 n][n u32 mesh
    ///   ids], followed by one `CCDBrush` per hull. Each brush is an A3DAABB
    ///   (48 bytes), flags, side count, and 17-byte plane/bevel records. This
    ///   is the same layout previously misidentified as scenery light groups;
    ///   `EL_BrushBuilding.cpp` and `CHBasedCD/aabbcd.cpp` establish its real
    ///   purpose. Old BMDs without even a hull-count word remain valid.
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        if u(b, 0)? != 0x42584f4d {
            return Err("not a binary AFile (missing MOXB marker, client sniff 0x76da7c)".into());
        }
        let mut p = 4;
        let mut version = u(b, p)?;
        if version == 0x80000001 {
            p += 5;
            version = u(b, p)?
        }
        let is_v300 = matches!(version, 0x10000300..=0x10000305);
        if version != 0x10000001 && version != 0x10000002 && !is_v300 {
            return Err(format!(
                "A3DLitModel::Load(), Wrong version! (model version {version:#x}, client 0x7bb79e)"
            ));
        }
        p += 4;
        let scale = v3(b, p)?;
        let direction = v3(b, p + 12)?;
        let up = v3(b, p + 24)?;
        let position = v3(b, p + 36)?;
        p += 48;
        let count = u(b, p)? as usize;
        p += 4;
        if count > 4096 {
            return Err("too many meshes".into());
        }
        let mut meshes = Vec::with_capacity(count);
        let mut lossy_gbk_diagnostics = Vec::new();
        let mut v300_branch_counters = V300BranchCounters::default();
        if is_v300 {
            // 1.8.x A3DLitModel (0x10000300..=0x10000305): model-level
            // `bLightMap` (1 B) + `bSharedData` (1 B), then per mesh:
            // `bSupportLightMap` (1 B) + `bUseLightMap` (1 B), length-prefixed
            // GBK name and texture, vertex and face counts, 24-byte vertices,
            // 32-bit indices (12 B per face), normals, day/night vertex
            // colours and/or lightmap UVs, and AABB + MATERIAL + dwExtraSize.
            const MAT_TAG: &[u8] = b"MATERIAL: \0";
            let _b_lightmap = *b.get(p).ok_or("truncated BMD v300 bLightMap")? != 0;
            let b_shared_data = *b.get(p + 1).ok_or("truncated BMD v300 bSharedData")? != 0;
            p += 2;
            if b_shared_data {
                let share_len = u(b, p)? as usize;
                p += 4;
                if share_len > 4096 {
                    return Err("oversized BMD v300 shared-data path".into());
                }
                let (share_file, _) = fixed_lossy_tracked(b, p, share_len)?;
                return Err(format!(
                    "A3DLitModel v300 shared data file {share_file:?} is unsupported"
                ));
            }
            for mesh_idx in 0..count {
                let b_support_lm = *b.get(p).ok_or("truncated BMD v300 bSupportLightMap")? != 0;
                let b_use_lm = *b.get(p + 1).ok_or("truncated BMD v300 bUseLightMap")? != 0;
                p += 2;
                v300_branch_counters.flags_read += 1;
                let name_len = u(b, p)? as usize;
                p += 4;
                if name_len > 4096 {
                    return Err(format!(
                        "oversized BMD v300 mesh {mesh_idx} name len={name_len:#x} flags=({},{})",
                        u8::from(b_support_lm),
                        u8::from(b_use_lm),
                    ));
                }
                let (name, name_lossy_gbk) = fixed_lossy_tracked(b, p, name_len)?;
                if name_lossy_gbk {
                    lossy_gbk_diagnostics.push(format!("mesh {mesh_idx} name lossy GBK: {name}"));
                }
                p = p.checked_add(name_len).ok_or("BMD v300 name overflow")?;
                let tex_len = u(b, p)? as usize;
                p += 4;
                if tex_len > 4096 {
                    return Err("oversized BMD v300 texture name".into());
                }
                let (texture, texture_lossy_gbk) = fixed_lossy_tracked(b, p, tex_len)?;
                if texture_lossy_gbk {
                    lossy_gbk_diagnostics
                        .push(format!("mesh {mesh_idx} texture lossy GBK: {texture}"));
                }
                p = p.checked_add(tex_len).ok_or("BMD v300 texture overflow")?;
                let n = u(b, p)? as usize;
                let faces = u(b, p + 4)? as usize;
                p += 8;
                if n > 1_000_000 || faces > 2_000_000 {
                    return Err("invalid BMD mesh size".into());
                }
                let needed = n
                    .checked_mul(24)
                    .and_then(|v| v.checked_add(faces.checked_mul(12)?))
                    .and_then(|v| v.checked_add(n.checked_mul(12)?))
                    .and_then(|v| v.checked_add(n.checked_mul(8)?))
                    .ok_or("BMD size overflow")?;
                if p.checked_add(needed).is_none_or(|e| e > b.len()) {
                    return Err("truncated BMD mesh data".into());
                }
                let mut vertices = Vec::with_capacity(n);
                for i in 0..n {
                    let q = p + i * 24;
                    vertices.push(Vertex {
                        position: v3(b, q)?,
                        diffuse: u(b, q + 12)?,
                        day_diffuse: 0,
                        night_diffuse: 0,
                        uv: [f(b, q + 16)?, f(b, q + 20)?],
                        normal: [0.; 3],
                    });
                }
                p += n * 24;
                let mut indices = Vec::with_capacity(faces * 3);
                for _ in 0..faces * 3 {
                    let idx = u(b, p)?;
                    if idx as usize >= n {
                        return Err("BMD vertex index out of range".into());
                    }
                    let idx16 = u16::try_from(idx).map_err(|_| "BMD vertex index exceeds u16")?;
                    indices.push(idx16);
                    p += 4;
                }
                for v in &mut vertices {
                    v.normal = v3(b, p)?;
                    p += 12;
                }
                let n8 = n.checked_mul(8).ok_or("BMD size overflow")?;
                let mut tail_advance = None;
                let mut material = None;
                let mat_at_p = b.get(p..p + MAT_TAG.len()) == Some(MAT_TAG);
                let has_mat_at = |off: usize| -> bool {
                    off.checked_add(48 + MAT_TAG.len()).is_some_and(|e| {
                        e <= b.len() && &b[off + 48..off + 48 + MAT_TAG.len()] == MAT_TAG
                    })
                };
                let n16 = n.checked_mul(16).ok_or("BMD size overflow")?;
                let double_block = !mat_at_p
                    && p.checked_add(n16).is_some_and(has_mat_at)
                    && !p.checked_add(n8).is_some_and(has_mat_at);
                let color_branch = if mat_at_p {
                    if p.checked_add(80).is_none_or(|e| e > b.len()) {
                        return Err("truncated material".into());
                    }
                    let mut mat_p = p;
                    material = parse_material(b, &mut mat_p).ok();
                    p = if material.is_some() { mat_p } else { p + 80 };
                    if p.checked_add(n8).is_none_or(|e| e > b.len()) {
                        return Err("truncated BMD mesh data".into());
                    }
                    for v in &mut vertices {
                        v.day_diffuse = u(b, p)?;
                        p += 4;
                    }
                    for v in &mut vertices {
                        v.night_diffuse = u(b, p)?;
                        p += 4;
                    }
                    v300_branch_counters.colours_read += 1;
                    if p.checked_add(52).is_some_and(|e| e <= b.len()) {
                        let dw_pos = p + 48;
                        let extra_size = u(b, dw_pos)? as usize;
                        if (4..=65536).contains(&extra_size)
                            && dw_pos.checked_add(extra_size).is_some_and(|e| e <= b.len())
                        {
                            tail_advance = Some(48 + extra_size);
                        }
                    }
                    V300ColorBranch::VertexColorsOnly
                } else if double_block {
                    p += n8;
                    v300_branch_counters.uv_block_skipped += 1;
                    for v in &mut vertices {
                        v.day_diffuse = u(b, p)?;
                        p += 4;
                    }
                    for v in &mut vertices {
                        v.night_diffuse = u(b, p)?;
                        p += 4;
                    }
                    v300_branch_counters.colours_read += 1;
                    V300ColorBranch::LightmapUvAndVertexColors
                } else {
                    for v in &mut vertices {
                        v.day_diffuse = u(b, p)?;
                        p += 4;
                    }
                    for v in &mut vertices {
                        v.night_diffuse = u(b, p)?;
                        p += 4;
                    }
                    let (opaque_day, _) = count_opaque_words(&vertices);
                    if n > 0 && opaque_day * 10 < n * 9 {
                        let following_opaque = p
                            .checked_add(n8)
                            .is_some_and(|e| e <= b.len())
                            .then(|| {
                                let mut c = 0usize;
                                for i in 0..n {
                                    let d_ok = u(b, p + i * 4).is_ok_and(|w| (w >> 24) == 0xff);
                                    let n_ok =
                                        u(b, p + n * 4 + i * 4).is_ok_and(|w| (w >> 24) == 0xff);
                                    if d_ok && n_ok {
                                        c += 1;
                                    }
                                }
                                c
                            })
                            .unwrap_or(0);
                        v300_branch_counters.uv_block_skipped += 1;
                        if following_opaque * 10 >= n * 9 {
                            for v in &mut vertices {
                                v.day_diffuse = u(b, p)?;
                                p += 4;
                            }
                            for v in &mut vertices {
                                v.night_diffuse = u(b, p)?;
                                p += 4;
                            }
                            v300_branch_counters.colours_read += 1;
                            V300ColorBranch::LightmapUvAndVertexColors
                        } else {
                            for v in &mut vertices {
                                v.day_diffuse = 0xff80_8080;
                                v.night_diffuse = 0xff80_8080;
                            }
                            v300_branch_counters.neutral_fallback += 1;
                            V300ColorBranch::LightmapUvNeutralFallback
                        }
                    } else {
                        v300_branch_counters.colours_read += 1;
                        V300ColorBranch::VertexColorsOnly
                    }
                };
                let (opaque_day_words, opaque_night_words) = count_opaque_words(&vertices);
                // Tail: 48 B AABB + 80 B A3DMaterial ("MATERIAL: \0" + 69 B) +
                // dwExtraSize (counts its own 4 B; 22 B when strDetailMap is empty -> 150 B total).
                // When MATERIAL preceded the day/night colours (`mat_at_p`), only 48 B AABB +
                // dwExtraSize remain here.
                if !mat_at_p
                    && p.checked_add(132).is_some_and(|e| e <= b.len())
                    && &b[p + 48..p + 48 + MAT_TAG.len()] == MAT_TAG
                {
                    let mut mat_p = p + 48;
                    material = parse_material(b, &mut mat_p).ok();
                    let dw_pos = if material.is_some() { mat_p } else { p + 128 };
                    let extra_size = u(b, dw_pos)? as usize;
                    if (4..=65536).contains(&extra_size)
                        && dw_pos.checked_add(extra_size).is_some_and(|e| e <= b.len())
                    {
                        tail_advance = Some((dw_pos - p) + extra_size);
                    }
                }
                if let Some(step) = tail_advance {
                    p += step;
                } else if mat_at_p && p.checked_add(70).is_some_and(|e| e <= b.len()) {
                    p += 70;
                } else if !mat_at_p && p.checked_add(150).is_some_and(|e| e <= b.len()) {
                    let mut mat_p = p + 48;
                    if b.get(mat_p..mat_p + MAT_TAG.len()) == Some(MAT_TAG) {
                        material = parse_material(b, &mut mat_p).ok();
                    }
                    p += 150;
                } else if mesh_idx + 1 < count {
                    return Err("truncated BMD v300 mesh tail".into());
                } else {
                    p = b.len();
                }
                meshes.push(Mesh {
                    name,
                    version,
                    material,
                    texture,
                    vertices,
                    indices,
                    color_branch: Some(color_branch),
                    b_support_lightmap: b_support_lm,
                    b_use_lightmap: b_use_lm,
                    opaque_day_words,
                    opaque_night_words,
                    name_lossy_gbk,
                    texture_lossy_gbk,
                });
            }
        } else {
            for mesh_idx in 0..count {
                let ver = u(b, p)?;
                p += 4;
                match ver {
                    0x10000004 | 0x10000005 | 0x10000006 => {}
                    // v2 (0x7ba0b8) and v3 (0x7ba30d) are older layouts: 28-byte
                    // vertex records and no normal array. The advance sizes are
                    // recorded in `src/bin/bmd-corpus-census.rs`, which walks these
                    // same versions, so the shape is known rather than guessed.
                    0x10000002 | 0x10000003 => {}
                    0x10000001 => {
                        return Err(
                            "mesh version 0x10000001: client A3DLitMesh::Load rejects it without logging (0x7baa64)".into(),
                        )
                    }
                    other => {
                        return Err(format!(
                            "A3DLitMesh::Load(), Unknown version [{other:x}]! (client 0x7baa51)"
                        ))
                    }
                }
                let (name, name_lossy_gbk) = fixed_lossy_tracked(b, p, 64)?;
                if name_lossy_gbk {
                    lossy_gbk_diagnostics.push(format!("mesh {mesh_idx} name lossy GBK: {name}"));
                }
                let (texture, texture_lossy_gbk) = fixed_lossy_tracked(b, p + 64, 256)?;
                if texture_lossy_gbk {
                    lossy_gbk_diagnostics
                        .push(format!("mesh {mesh_idx} texture lossy GBK: {texture}"));
                }
                p += 320;
                let n = u(b, p)? as usize;
                let faces = u(b, p + 4)? as usize;
                p += 8;
                let has_extra_colors = if ver == 0x10000006 {
                    let flag = *b.get(p).ok_or("truncated BMD")? != 0;
                    p += 1;
                    flag
                } else {
                    false
                };
                if n > 1_000_000 || faces > 2_000_000 {
                    return Err("invalid BMD mesh size".into());
                }
                // v2/v3 use 28-byte vertex records; v4/v5/v6 use 24-byte
                // records plus a trailing normal array. v3 additionally
                // carries two packed per-vertex light arrays, like v4/v5/v6.
                let stride: usize = if ver >= 0x10000004 { 24 } else { 28 };
                let light_arrays = ver != 0x10000002;
                let mut needed = n
                    .checked_mul(stride)
                    .and_then(|v| v.checked_add(faces * 6))
                    .and_then(|v| v.checked_add(n * 12))
                    .and_then(|v| v.checked_add(48))
                    .ok_or("BMD size overflow")?;
                if light_arrays {
                    needed = needed
                        .checked_add(n * 4)
                        .and_then(|v| v.checked_add(n * 4))
                        .ok_or("BMD size overflow")?;
                }
                if has_extra_colors {
                    needed = needed
                        .checked_add(n * 4)
                        .and_then(|v| v.checked_add(n * 4))
                        .ok_or("BMD size overflow")?;
                }
                if p.checked_add(needed).is_none_or(|e| e > b.len()) {
                    return Err("truncated BMD mesh data".into());
                }
                let mut vertices = Vec::with_capacity(n);
                for i in 0..n {
                    let q = p + i * stride;
                    vertices.push(Vertex {
                        position: v3(b, q)?,
                        diffuse: u(b, q + 12)?,
                        day_diffuse: 0,
                        night_diffuse: 0,
                        uv: [f(b, q + 16)?, f(b, q + 20)?],
                        normal: [0.; 3],
                    });
                }
                p += n * stride;
                let mut indices = Vec::with_capacity(faces * 3);
                for _ in 0..faces * 3 {
                    let s = b.get(p..p + 2).ok_or("truncated BMD indices")?;
                    let i = u16::from_le_bytes([s[0], s[1]]);
                    if i as usize >= n {
                        return Err("BMD vertex index out of range".into());
                    }
                    indices.push(i);
                    p += 2;
                }
                // The normal array is present in every version (n*12 bytes);
                // the 28-byte v2/v3 record simply has no room for one inline,
                // so the block is where it always is. This matches the advance
                // sizes in `src/bin/bmd-corpus-census.rs` for all versions.
                for v in &mut vertices {
                    v.normal = v3(b, p)?;
                    p += 12;
                }
                // The client loads two packed vertex-light arrays into mesh
                // members +0x284/+0x288 and blends them at 0x7b9bc0. v2 has
                // neither; v3 has both.
                if light_arrays {
                    for v in &mut vertices {
                        v.day_diffuse = u(b, p)?;
                        p += 4;
                    }
                    for v in &mut vertices {
                        v.night_diffuse = u(b, p)?;
                        p += 4;
                    }
                }
                if has_extra_colors {
                    p += n * 8;
                }
                p += 48;
                let material = if ver >= 0x10000005 {
                    Some(parse_material(b, &mut p)?)
                } else {
                    None
                };
                let (opaque_day_words, opaque_night_words) = count_opaque_words(&vertices);
                meshes.push(Mesh {
                    name,
                    version: ver,
                    material,
                    texture,
                    vertices,
                    indices,
                    color_branch: None,
                    b_support_lightmap: false,
                    b_use_lightmap: false,
                    opaque_day_words,
                    opaque_night_words,
                    name_lossy_gbk,
                    texture_lossy_gbk,
                });
            }
        }
        let collision = if p < b.len() {
            if is_v300 {
                match parse_collision(b, p) {
                    Ok(brushes) => brushes,
                    Err(_) => Vec::new(),
                }
            } else {
                parse_collision(b, p)?
            }
        } else {
            Vec::new()
        };
        Ok(Self {
            scale,
            direction,
            up,
            position,
            meshes,
            collision,
            lossy_gbk_diagnostics,
            v300_branch_counters,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_short_file() {
        assert!(Model::parse(&[0; 8]).is_err());
    }
    #[test]
    fn reads_both_baked_light_arrays() {
        let mut b = Vec::new();
        let u = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        u(&mut b, 0x42584f4d);
        u(&mut b, 0x10000002);
        b.extend_from_slice(&[0; 48]);
        u(&mut b, 1);
        u(&mut b, 0x10000004);
        b.extend_from_slice(&[0; 320]);
        u(&mut b, 2);
        u(&mut b, 0);
        b.extend_from_slice(&[0; 48]); // two vertex records
        b.extend_from_slice(&[0; 24]); // two normals
        u(&mut b, 0xff123456);
        u(&mut b, 0xff789abc);
        u(&mut b, 0xfffedcba);
        u(&mut b, 0xff654321);
        b.extend_from_slice(&[0; 48]);
        let m = Model::parse(&b).unwrap();
        let v = &m.meshes[0].vertices;
        assert_eq!(
            (v[0].day_diffuse, v[0].night_diffuse),
            (0xff123456, 0xfffedcba)
        );
        assert_eq!(
            (v[1].day_diffuse, v[1].night_diffuse),
            (0xff789abc, 0xff654321)
        );
    }
    #[test]
    fn reads_version_five_material_and_rejects_truncation() {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, 0x10000002);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 1);
        put(&mut b, 0x10000005);
        b.extend_from_slice(&[0; 320]);
        put(&mut b, 0);
        put(&mut b, 0);
        b.extend_from_slice(&[0; 48]);
        b.extend_from_slice(b"MATERIAL: \0");
        for x in 0..17 {
            put(&mut b, (x as f32 / 16.0).to_bits());
        }
        b.push(1);
        let model = Model::parse(&b).unwrap();
        let m = model.meshes[0].material.as_ref().unwrap();
        assert_eq!(m.name, "MATERIAL: ");
        assert_eq!(m.diffuse, [0.25, 0.3125, 0.375, 0.4375]);
        assert_eq!(m.power, 1.0);
        assert!(m.two_sided);
        b.pop();
        assert!(Model::parse(&b).is_err());
    }
    #[test]
    fn rejects_nan_geometry() {
        assert!(f(&f32::NAN.to_le_bytes(), 0).is_err());
    }
    fn model_skeleton(model_version: u32, mesh_version: u32) -> Vec<u8> {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, model_version);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 1);
        put(&mut b, mesh_version);
        b.extend_from_slice(&[0; 320]);
        put(&mut b, 0);
        put(&mut b, 0);
        b.extend_from_slice(&[0; 48]);
        b
    }
    fn parse_err(b: &[u8]) -> String {
        match Model::parse(b) {
            Ok(_) => panic!("expected a parse error"),
            Err(e) => e,
        }
    }
    /// v2 (0x7ba0b8) and v3 (0x7ba30d) use 28-byte vertex records and no
    /// separate normal array; v3 adds the two packed light arrays. The sizes
    /// below are the same ones `src/bin/bmd-corpus-census.rs` walks, so this
    /// test and that tool cannot drift apart silently.
    #[test]
    fn parses_mesh_v2_and_v3_with_their_own_strides() {
        let build = |mesh_version: u32, n: u32, faces: u32| {
            let mut b = Vec::new();
            let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
            put(&mut b, 0x42584f4d);
            put(&mut b, 0x10000002);
            b.extend_from_slice(&[0; 48]);
            put(&mut b, 1);
            put(&mut b, mesh_version);
            b.extend_from_slice(&[0; 320]);
            put(&mut b, n);
            put(&mut b, faces);
            let stride = if mesh_version >= 0x10000004 { 24 } else { 28 };
            b.extend_from_slice(&vec![0u8; (n * stride) as usize]);
            b.extend_from_slice(&vec![0u8; (faces * 6) as usize]);
            // Normals are a separate block for every version.
            b.extend_from_slice(&vec![0u8; (n * 12) as usize]);
            if mesh_version != 0x10000002 {
                b.extend_from_slice(&vec![0u8; (n * 4) as usize]);
                b.extend_from_slice(&vec![0u8; (n * 4) as usize]);
            }
            // The 48-byte block comes first; v5 then adds a material tail of a
            // NUL-terminated name plus 4x16 bytes of colours, a float and a
            // two-sided flag. Order matters: the parser advances past the 48
            // bytes before it looks for a name.
            b.extend_from_slice(&[0; 48]);
            if mesh_version >= 0x10000005 {
                b.extend_from_slice(b"mat\0");
                b.extend_from_slice(&[0u8; 69]);
            }
            b
        };
        for v in [0x10000002u32, 0x10000003] {
            let m = Model::parse(&build(v, 4, 2)).unwrap();
            assert_eq!(m.meshes.len(), 1, "v{v:#x}");
            assert_eq!(m.meshes[0].version, v);
            assert_eq!(m.meshes[0].vertices.len(), 4);
            assert_eq!(m.meshes[0].indices.len(), 6);
            // v2/v3 carry no material tail; `material` is an Option, so absence
            // is None rather than an empty value.
            assert!(m.meshes[0].material.is_none(), "v{v:#x}");
        }
        // v4/v5 must be untouched by the v2/v3 handling: same input shape
        // still parses and still records the version it read.
        for v in [0x10000004u32, 0x10000005] {
            let m = Model::parse(&build(v, 4, 2)).unwrap();
            assert_eq!(m.meshes[0].version, v);
            assert_eq!(m.meshes[0].vertices.len(), 4);
            // v4 stops before the material; v5 must consume the tail it added.
            if v == 0x10000005 {
                assert_eq!(
                    m.meshes[0].material.as_ref().map(|x| x.name.as_str()),
                    Some("mat")
                );
            } else {
                assert!(m.meshes[0].material.is_none(), "v{v:#x}");
            }
        }
        // Truncating one byte must still be caught rather than misread.
        let mut short = build(0x10000002, 4, 2);
        short.pop();
        assert!(Model::parse(&short).is_err());
    }
    #[test]
    fn accepts_both_client_model_versions() {
        // A3DLitModel::Load reads the identical header for 0x10000001
        // (0x7bb3de) and 0x10000002 (0x7bb58b).
        for mv in [0x10000001u32, 0x10000002] {
            let m = Model::parse(&model_skeleton(mv, 0x10000004)).unwrap();
            assert_eq!(m.meshes.len(), 1);
        }
        let err = parse_err(&model_skeleton(0x10000003, 0x10000004));
        assert!(err.contains("Wrong version"), "{err}");
    }
    #[test]
    fn names_every_unsupported_mesh_version() {
        let err = parse_err(&model_skeleton(0x10000002, 0x10000001));
        assert!(err.contains("without logging"), "{err}");
        let err = parse_err(&model_skeleton(0x10000002, 0x10000007));
        assert!(err.contains("Unknown version [10000007]"), "{err}");
    }

    #[test]
    fn parses_mesh_v6_with_and_without_extra_colors() {
        for has_extra in [false, true] {
            let mut b = Vec::new();
            let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
            put(&mut b, 0x42584f4d);
            put(&mut b, 0x10000002);
            b.extend_from_slice(&[0; 48]);
            put(&mut b, 1);
            put(&mut b, 0x10000006);
            b.extend_from_slice(&[0; 320]);
            put(&mut b, 2); // vertices
            put(&mut b, 1); // faces
            b.push(u8::from(has_extra));
            b.extend_from_slice(&[0; 48]); // 2 * 24 vertices
            b.extend_from_slice(&[0; 6]); // 1 face (3 u16)
            b.extend_from_slice(&[0; 24]); // 2 normals
            put(&mut b, 0xff112233);
            put(&mut b, 0xff445566);
            put(&mut b, 0xff778899);
            put(&mut b, 0xffaabbcc);
            if has_extra {
                b.extend_from_slice(&[0; 16]); // 2 * 8 extra day/night colors
            }
            b.extend_from_slice(&[0; 48]); // AABB
            b.extend_from_slice(b"MATERIAL: \0");
            b.extend_from_slice(&[0; 69]);
            let model = Model::parse(&b).unwrap();
            assert_eq!(model.meshes.len(), 1);
            assert_eq!(model.meshes[0].version, 0x10000006);
            assert_eq!(model.meshes[0].vertices[0].day_diffuse, 0xff112233);
            assert_eq!(model.meshes[0].vertices[1].night_diffuse, 0xffaabbcc);
        }
    }

    fn push_v300_mesh(
        b: &mut Vec<u8>,
        has_uv_block: bool,
        has_color_block: bool,
        name: &[u8],
        tex: &[u8],
        day: [u32; 3],
        night: [u32; 3],
        extra_size: u32,
    ) {
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        b.push(u8::from(has_uv_block));
        b.push(u8::from(has_uv_block && !has_color_block));
        put(b, name.len() as u32);
        b.extend_from_slice(name);
        put(b, tex.len() as u32);
        b.extend_from_slice(tex);
        put(b, 3);
        put(b, 1);
        for i in 0..3u32 {
            put(b, (i as f32).to_bits());
            put(b, 0.0f32.to_bits());
            put(b, 1.0f32.to_bits());
            put(b, 0xffffffff);
            put(b, 0.0f32.to_bits());
            put(b, 1.0f32.to_bits());
        }
        put(b, 0);
        put(b, 1);
        put(b, 2);
        b.extend_from_slice(&[0; 36]);
        if has_uv_block {
            for uv in [0.25f32, 0.5, 0.75, 0.125, 0.5, 0.5] {
                put(b, uv.to_bits());
            }
        }
        if has_color_block {
            for c in day {
                put(b, c);
            }
            for c in night {
                put(b, c);
            }
        }
        b.extend_from_slice(&[0; 48]);
        b.extend_from_slice(b"MATERIAL: \0");
        b.extend_from_slice(&[0; 69]);
        put(b, extra_size);
        b.extend_from_slice(&vec![0u8; extra_size.saturating_sub(4) as usize]);
    }

    #[test]
    fn parses_v300_branches_and_advances_by_dw_extra_size() {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, 0x80000001);
        b.push(0);
        put(&mut b, 0x10000302);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 3);
        b.extend_from_slice(&[0, 0]);
        push_v300_mesh(
            &mut b,
            false,
            true,
            b"branch_colors_only",
            b"Building\\a.dds",
            [0xff102030, 0xff203040, 0xff304050],
            [0xff010203, 0xff020304, 0xff030405],
            16,
        );
        push_v300_mesh(
            &mut b,
            true,
            true,
            b"branch_uv_and_colors",
            b"Building\\b.dds",
            [0xff887766, 0xff998877, 0xffaa9988],
            [0xff223344, 0xff334455, 0xff445566],
            30,
        );
        push_v300_mesh(
            &mut b,
            true,
            false,
            b"branch_uv_neutral",
            b"Building\\c.dds",
            [0; 3],
            [0; 3],
            22,
        );
        let model = Model::parse(&b).unwrap();
        assert_eq!(model.meshes.len(), 3);
        assert_eq!(
            model.v300_branch_counters,
            V300BranchCounters {
                flags_read: 3,
                uv_block_skipped: 2,
                colours_read: 2,
                neutral_fallback: 1,
            }
        );
        assert_eq!(
            model.meshes[0].color_branch,
            Some(V300ColorBranch::VertexColorsOnly)
        );
        assert_eq!(model.meshes[0].vertices[0].day_diffuse, 0xff102030);
        assert_eq!(model.meshes[0].vertices[0].night_diffuse, 0xff010203);
        assert_eq!(
            model.meshes[1].color_branch,
            Some(V300ColorBranch::LightmapUvAndVertexColors)
        );
        assert_eq!(model.meshes[1].vertices[0].day_diffuse, 0xff887766);
        assert_eq!(model.meshes[1].vertices[0].night_diffuse, 0xff223344);
        assert_eq!(
            model.meshes[2].color_branch,
            Some(V300ColorBranch::LightmapUvNeutralFallback)
        );
        assert_eq!(model.meshes[2].vertices[0].day_diffuse, 0xff80_8080);
        assert_eq!(model.meshes[2].vertices[0].night_diffuse, 0xff80_8080);
        let log_line = model.meshes[2].colour_log_line("litmodels/world/1/litmodel_1.bmd", 2);
        assert!(log_line.contains("branch=lightmap_neutral"));
        assert!(log_line.contains("first_day=0xff808080"));
    }

    #[test]
    fn parses_v300_colours_only_with_material_before_colors_and_non_default_extra_size() {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, 0x80000001);
        b.push(0);
        put(&mut b, 0x10000302);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 2);
        b.extend_from_slice(&[0, 0]);

        // Mesh 0: MATERIAL: \0 before day/night colours, followed by 48 B AABB + dwExtraSize=40.
        b.extend_from_slice(&[0, 0]);
        put(&mut b, 2);
        b.extend_from_slice(b"m0");
        put(&mut b, 5);
        b.extend_from_slice(b"a.dds");
        put(&mut b, 3);
        put(&mut b, 1);
        for i in 0..3u32 {
            put(&mut b, (i as f32).to_bits());
            put(&mut b, 0.0f32.to_bits());
            put(&mut b, 1.0f32.to_bits());
            put(&mut b, 0xffffffff);
            put(&mut b, 0.0f32.to_bits());
            put(&mut b, 1.0f32.to_bits());
        }
        put(&mut b, 0);
        put(&mut b, 1);
        put(&mut b, 2);
        b.extend_from_slice(&[0; 36]);
        b.extend_from_slice(b"MATERIAL: \0");
        b.extend_from_slice(&[0; 69]);
        for c in [0xff112233u32, 0xff445566, 0xff778899] {
            put(&mut b, c);
        }
        for c in [0xff010203u32, 0xff040506, 0xff070809] {
            put(&mut b, c);
        }
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 40);
        b.extend_from_slice(&[0x55; 36]);

        // Mesh 1: immediately follows dwExtraSize=40.
        push_v300_mesh(&mut b, true, false, b"m1", b"b.dds", [0; 3], [0; 3], 22);

        let model = Model::parse(&b).unwrap();
        assert_eq!(model.meshes.len(), 2);
        assert_eq!(
            model.meshes[0].color_branch,
            Some(V300ColorBranch::VertexColorsOnly)
        );
        assert!(model.meshes[0].material.is_some());
        assert_eq!(model.meshes[0].opaque_day_words, 3);
        assert_eq!(model.meshes[0].opaque_night_words, 3);
        assert_eq!(model.meshes[0].vertices[0].day_diffuse, 0xff112233);
        assert_eq!(model.meshes[0].vertices[2].day_diffuse, 0xff778899);
        assert_eq!(model.meshes[0].vertices[0].night_diffuse, 0xff010203);
        assert_eq!(model.meshes[1].name, "m1");
        assert_eq!(
            model.meshes[1].color_branch,
            Some(V300ColorBranch::LightmapUvNeutralFallback)
        );
    }

    #[test]
    fn parses_v300_tail_dw_extra_size_20_even_with_nonfinite_material_float() {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, 0x80000001);
        b.push(0);
        put(&mut b, 0x10000302);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 2);
        b.extend_from_slice(&[0, 0]);

        // Mesh 0: lightmap_neutral with a NaN float inside MATERIAL and extra_size=20.
        b.extend_from_slice(&[1, 1]);
        put(&mut b, 2);
        b.extend_from_slice(b"m0");
        put(&mut b, 5);
        b.extend_from_slice(b"a.dds");
        put(&mut b, 3);
        put(&mut b, 1);
        for i in 0..3u32 {
            put(&mut b, (i as f32).to_bits());
            put(&mut b, 0.0f32.to_bits());
            put(&mut b, 1.0f32.to_bits());
            put(&mut b, 0xffffffff);
            put(&mut b, 0.0f32.to_bits());
            put(&mut b, 1.0f32.to_bits());
        }
        put(&mut b, 0);
        put(&mut b, 1);
        put(&mut b, 2);
        b.extend_from_slice(&[0; 36]);
        for uv in [0.25f32, 0.5, 0.75, 0.125, 0.5, 0.5] {
            put(&mut b, uv.to_bits());
        }
        b.extend_from_slice(&[0; 48]);
        b.extend_from_slice(b"MATERIAL: \0");
        put(&mut b, f32::NAN.to_bits());
        b.extend_from_slice(&[0; 65]);
        put(&mut b, 20);
        b.extend_from_slice(&[0; 16]);

        // Mesh 1: immediately follows dwExtraSize=20.
        push_v300_mesh(&mut b, true, false, b"m1", b"b.dds", [0; 3], [0; 3], 22);

        let model = Model::parse(&b).unwrap();
        assert_eq!(model.meshes.len(), 2);
        assert!(model.meshes[0].material.is_none());
        assert_eq!(model.meshes[1].name, "m1");
    }

    #[test]
    fn lossy_gbk_texture_name_produces_mesh_and_records_diagnostic() {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        put(&mut b, 0x42584f4d);
        put(&mut b, 0x10000305);
        b.extend_from_slice(&[0; 48]);
        put(&mut b, 1);
        b.extend_from_slice(&[0, 0]);
        push_v300_mesh(
            &mut b,
            false,
            true,
            b"m0",
            b"Building\\bad\xfftex.dds",
            [0xff808080; 3],
            [0xff404040; 3],
            16,
        );
        let model = Model::parse(&b).unwrap();
        assert_eq!(model.meshes.len(), 1);
        assert!(model.meshes[0].texture_lossy_gbk);
        assert!(!model.lossy_gbk_diagnostics.is_empty());
        assert!(model.meshes[0].texture.contains("Building/bad"));
    }
    #[test]
    fn accepts_chunk_wrapper_flag_bytes() {
        // Scenery loader 0x64e200: tag 0x80000001 then one flag byte that is
        // stored but never read; corpus histogram: {0: 7943, 1: 119}.
        for flag in [0u8, 1] {
            let mut b = Vec::new();
            b.extend_from_slice(&0x42584f4du32.to_le_bytes());
            b.extend_from_slice(&0x80000001u32.to_le_bytes());
            b.push(flag);
            let mut rest = model_skeleton(0x10000002, 0x10000004);
            b.extend_from_slice(&rest.split_off(4));
            assert_eq!(Model::parse(&b).unwrap().meshes.len(), 1);
        }
    }

    #[test]
    fn reads_native_collision_hulls_after_lit_meshes() {
        let mut b = model_skeleton(0x10000002, 0x10000004);
        assert!(Model::parse(&b).unwrap().collision.is_empty());
        let put = |b: &mut Vec<u8>, value: u32| b.extend_from_slice(&value.to_le_bytes());
        put(&mut b, 1); // hull count
        put(&mut b, 2); // mesh ids associated with this hull
        put(&mut b, 3);
        put(&mut b, 7);
        for value in [
            10.0f32, 20.0, 30.0, // center
            1.0, 2.0, 3.0, // extents
            9.0, 18.0, 27.0, // mins
            11.0, 22.0, 33.0, // maxs
        ] {
            put(&mut b, value.to_bits());
        }
        put(&mut b, 4); // CHDATA_SKIP_COLLISION
        put(&mut b, 1); // one side is enough to verify the serialized layout
        for value in [0.0f32, 1.0, 0.0, 12.5] {
            put(&mut b, value.to_bits());
        }
        b.push(1); // bevel bool
        let model = Model::parse(&b).unwrap();
        assert_eq!(model.collision.len(), 1);
        let brush = &model.collision[0];
        assert_eq!(brush.center, [10.0, 20.0, 30.0]);
        assert_eq!(brush.extents, [1.0, 2.0, 3.0]);
        assert_eq!(brush.flags, 4);
        assert_eq!(brush.sides.len(), 1);
        assert_eq!(brush.sides[0].normal, [0.0, 1.0, 0.0]);
        assert_eq!(brush.sides[0].distance, 12.5);
        assert!(brush.sides[0].bevel);
        b.pop();
        assert!(Model::parse(&b).is_err());
    }
}
