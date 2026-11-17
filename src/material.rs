//! Main-world T2MK v2 material layers and DDS texture access.
//! Format informed by the GPL-2.0-or-later GameForge Angelica importer.
use crate::{f32at, pck, u32at};
use flate2::read::ZlibDecoder;
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
};

fn small_mask_atlas() -> bool {
    static SMALL_MASK_ATLAS: OnceLock<bool> = OnceLock::new();
    *SMALL_MASK_ATLAS.get_or_init(|| std::env::var_os("PW_SMALL_MASK_ATLAS").is_some())
}

/// Shared PCK archives belong to the `element` folder that owns the selected
/// map. A direct map-directory argument may have that folder as its immediate
/// parent instead of `element/maps/<id>`.
fn archive_root(dir: &Path) -> Result<PathBuf, String> {
    let parent = dir.parent().ok_or("bad map path")?;
    let mut candidates = vec![parent.to_path_buf()];
    if parent
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("maps"))
    {
        if let Some(element) = parent.parent() {
            candidates.insert(0, element.to_path_buf());
        }
    }
    candidates
        .into_iter()
        .find(|root| root.join("textures.pck").is_file())
        .ok_or_else(|| format!("textures.pck not found beside {}", dir.display()))
}
pub(crate) struct Texture {
    pub(crate) w: usize,
    pub(crate) h: usize,
    /// Retain DDS blocks, not a world-sized decoded RGBA copy.
    pub(crate) source: Vec<u8>,
    pub(crate) levels: std::sync::OnceLock<Vec<crate::dxt::Level>>,
}
impl Texture {
    pub(crate) fn decode(&self) -> Result<Vec<crate::dxt::Level>, String> {
        let info = crate::terrain_sampling::parse_terrain_dds(&self.source)?;
        crate::dxt::decode_chain(
            info.format.four_cc(),
            &self.source[128..],
            info.width,
            info.height,
            info.authored_mips,
        )
    }
    fn cpu_levels(&self) -> &[crate::dxt::Level] {
        self.levels
            .get_or_init(|| self.decode().expect("validated terrain DDS"))
    }
}

/// Original zlib payload stays compressed until upload. CPU preview sampling
/// memoizes only masks it actually visits; desktop uploads use a temporary.
pub(crate) struct Mask {
    bytes: Vec<u8>,
    compressed: bool,
    decoded: std::sync::OnceLock<Vec<u8>>,
}
impl Mask {
    fn raw(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            compressed: false,
            decoded: Default::default(),
        }
    }
    pub(crate) fn decode(&self, side: usize) -> Result<std::borrow::Cow<'_, [u8]>, String> {
        if !self.compressed {
            return Ok(std::borrow::Cow::Borrowed(&self.bytes));
        }
        let mut out = Vec::with_capacity(side * side);
        ZlibDecoder::new(self.bytes.as_slice())
            .take((side * side + 1) as u64)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        if out.len() != side * side {
            return Err("invalid decoded mask size".into());
        }
        Ok(std::borrow::Cow::Owned(out))
    }
    fn cpu_bytes(&self, side: usize) -> &[u8] {
        if !self.compressed {
            return &self.bytes;
        }
        self.decoded
            .get_or_init(|| self.decode(side).expect("validated mask").into_owned())
    }
}
pub(crate) struct Layer {
    pub(crate) texture: usize,
    pub(crate) axis: u8,
    pub(crate) u: f32,
    pub(crate) v: f32,
    pub(crate) mask_size: usize,
    pub(crate) mask: Mask,
}
pub(crate) struct Area {
    pub(crate) layers: Vec<Layer>,
}
pub struct Materials {
    pub(crate) textures: Vec<Texture>,
    pub(crate) areas: Vec<Area>,
    pub(crate) areas_x: usize,
    pub(crate) mask_grid: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) spacing: f32,
}
fn gbk_string(b: &[u8], p: &mut usize) -> Result<String, String> {
    let len = u32at(b, *p)? as usize;
    *p += 4;
    if len > 4096 {
        return Err("oversized name".into());
    }
    let bytes = b.get(*p..*p + len).ok_or("truncated name")?;
    *p += len;
    let (s, _, bad) = encoding_rs::GBK.decode(bytes);
    if bad {
        return Err("invalid GBK path".into());
    }
    Ok(s.replace('\\', "/"))
}
/// The layer texcoord projection keyed by the record's axis byte (`+0x0c`).
///
/// `x`/`z` are the vertex's world X/Z and `y` its world height, i.e. exactly the
/// `i.world` the terrain fragment shader sees. The mapping is
/// `0 -> (z, -y)`, `1 -> (x, -z)`, `2 -> (x, -y)`; the WGSL in
/// `src/terrain_gpu.rs` uses the same three expressions and the same default, so
/// the CPU sampler and the GPU path cannot drift apart. Axis 1 is what every one
/// of the 1,408 surveyed area bases uses.
///
/// This is the *viewer's* contract, pinned by
/// `axis_projection_matches_the_shader`. It is not yet evidence that the native
/// client projects identically: the client builds its texcoords in the CPU mesh
/// path and no address in the surveyed range emits them.
pub(crate) fn layer_uv(axis: u8, x: f32, y: f32, z: f32) -> (f32, f32) {
    match axis {
        0 => (z, -y),
        2 => (x, -y),
        _ => (x, -z),
    }
}

fn in_window(id: usize, cols: usize, window: Option<[usize; 4]>) -> bool {
    window.map_or(true, |[x0, y0, x1, y1]| {
        let (x, y) = (id % cols, id / cols);
        x >= x0 && x < x1 && y >= y0 && y < y1
    })
}
impl Materials {
    pub fn load(
        dir: &Path,
        trn: &[u8],
        names: &[String],
        width: usize,
        height: usize,
        spacing: f32,
    ) -> Result<Self, String> {
        Self::load_window(dir, trn, names, width, height, spacing, None, true)
    }
    /// Load only masks used by the resident terrain. Global area indices stay
    /// unchanged; the GPU packs the surviving masks into a compact atlas.
    pub fn load_window(
        dir: &Path,
        trn: &[u8],
        names: &[String],
        width: usize,
        height: usize,
        spacing: f32,
        window: Option<[usize; 4]>,
        colors: bool,
    ) -> Result<Self, String> {
        let mask_grid = u32at(trn, 48)? as usize;
        let subgrid = u32at(trn, 64)? as usize;
        if mask_grid == 0
            || subgrid % mask_grid != 0
            || width % mask_grid != 0
            || height % mask_grid != 0
        {
            return Err("unsupported mask grid".into());
        }
        let areas_x = width / mask_grid;
        let areas_y = height / mask_grid;
        let sub_areas = subgrid / mask_grid;
        let subs_x = width / subgrid;
        let mut pos = u32at(trn, 72)? as usize;
        let count = u32at(trn, 36)? as usize;
        if count > 4096 {
            return Err("too many textures".into());
        }
        let mut paths = Vec::with_capacity(count);
        for _ in 0..count {
            paths.push(gbk_string(trn, &mut pos)?)
        }
        let element = archive_root(dir)?;
        let mut file =
            File::open(element.join("textures.pck")).map_err(|e| format!("textures.pck: {e}"))?;
        let entries = if colors {
            pck::directory(&mut file)?
        } else {
            Vec::new()
        };
        let entry_index: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.to_ascii_lowercase(), i))
            .collect();
        let mut textures = Vec::with_capacity(paths.len());
        for path in paths.iter().filter(|_| colors) {
            let entry = entry_index
                .get(&path.to_ascii_lowercase())
                .map(|&i| &entries[i])
                .ok_or_else(|| format!("missing texture {path}"))?;
            let raw = pck::read_entry(&mut file, entry)?;
            // Terrain sources must be one of the compressed formats the client
            // can create; anything else is an explicit error naming the file
            // instead of an `image` crate fallback that silently decodes a
            // format the native device would have rejected.
            let info = crate::terrain_sampling::parse_terrain_dds(&raw)
                .map_err(|e| format!("texture {path}: {e}"))?;
            let (w, h) = (info.width as usize, info.height as usize);
            textures.push(Texture {
                w,
                h,
                source: raw,
                levels: Default::default(),
            });
        }
        let mut areas: Vec<Area> = (0..areas_x * areas_y)
            .map(|_| Area { layers: Vec::new() })
            .collect();
        let small_mask_atlas = small_mask_atlas();
        for (si, name) in names.iter().enumerate() {
            if let Some([x0, y0, x1, y1]) = window {
                let (sx, sy) = ((si % subs_x) * sub_areas, (si / subs_x) * sub_areas);
                if sx >= x1 || sy >= y1 || sx + sub_areas <= x0 || sy + sub_areas <= y0 {
                    continue;
                }
            }
            let data = fs::read(dir.join(format!("{name}.t2mk"))).map_err(|e| e.to_string())?;
            if u32at(&data, 0)? != 0x544d4b42 || u32at(&data, 4)? != 2 {
                return Err(format!("{name}: unsupported T2MK"));
            }
            let area_count = u32at(&data, 8)? as usize;
            let layer_count = u32at(&data, 12)? as usize;
            if area_count != sub_areas * sub_areas || layer_count > 4096 {
                return Err(format!("{name}: invalid layer count"));
            }
            let layerstart = 20 + area_count * 4;
            for ai in 0..area_count {
                let p = u32at(&data, 20 + ai * 4)? as usize;
                let n = u32at(&data, p)? as usize;
                if n > 256 {
                    return Err("too many layers".into());
                }
                let dst = ((si / subs_x) * sub_areas + ai / sub_areas) * areas_x
                    + (si % subs_x) * sub_areas
                    + ai % sub_areas;
                if !in_window(dst, areas_x, window) {
                    continue;
                }
                let area = areas.get_mut(dst).ok_or("mask area outside world")?;
                for li in 0..n {
                    let r = p + 4 + li * 12;
                    let idx = u32at(&data, r)? as usize;
                    if idx >= layer_count {
                        return Err("layer index out of bounds".into());
                    }
                    let lp = layerstart + idx * 28;
                    let texture = u32at(&data, lp)? as usize;
                    if texture >= count {
                        return Err("texture index out of bounds".into());
                    }
                    let mut size = if li == 0 {
                        0
                    } else {
                        u32at(&data, lp + 8)? as usize
                    };
                    if size > 8192 || (li > 0 && size == 0) {
                        return Err("invalid mask size".into());
                    }
                    let mut mask = Vec::with_capacity(size * size);
                    let mut packed = None;
                    if li > 0 {
                        let off = u32at(&data, r + 4)? as usize;
                        let len = u32at(&data, r + 8)? as usize;
                        let bytes = data
                            .get(off..off.checked_add(len).ok_or("mask overflow")?)
                            .ok_or("truncated mask")?;
                        if u32at(&data, 16)? != 0 {
                            if !(size == 256 && small_mask_atlas) {
                                packed = Some(bytes.to_vec());
                            }
                            ZlibDecoder::new(bytes)
                                .take((size * size + 1) as u64)
                                .read_to_end(&mut mask)
                                .map_err(|e| e.to_string())?;
                        } else {
                            mask.extend_from_slice(bytes)
                        }
                        if mask.len() != size * size {
                            return Err("invalid decoded mask size".into());
                        }
                        // Only downsample in the explicitly selected low-memory mode.
                        if size == 256 && small_mask_atlas {
                            let mut half = vec![0u8; 128 * 128];
                            for y in 0..128 {
                                for x in 0..128 {
                                    let i = 2 * y * 256 + 2 * x;
                                    half[y * 128 + x] = ((mask[i] as u32
                                        + mask[i + 1] as u32
                                        + mask[i + 256] as u32
                                        + mask[i + 257] as u32
                                        + 2)
                                        / 4)
                                        as u8;
                                }
                            }
                            packed = None;
                            mask = half;
                            size = 128;
                        }
                    }
                    area.layers.push(Layer {
                        texture,
                        axis: *data.get(lp + 12).ok_or("truncated layer")?,
                        u: f32at(&data, lp + 16)?,
                        v: f32at(&data, lp + 20)?,
                        mask_size: size,
                        mask: match packed {
                            Some(bytes) => Mask {
                                bytes,
                                compressed: true,
                                decoded: Default::default(),
                            },
                            None => Mask::raw(mask),
                        },
                    });
                }
            }
        }
        let packed_bytes: usize = textures.iter().map(|t| t.source.len()).sum::<usize>()
            + areas
                .iter()
                .flat_map(|a| &a.layers)
                .map(|l| l.mask.bytes.len())
                .sum::<usize>();
        let expanded_bytes: usize = textures
            .iter()
            .map(|t| {
                let info =
                    crate::terrain_sampling::parse_terrain_dds(&t.source).expect("validated DDS");
                (0..info.authored_mips)
                    .map(|mip| {
                        ((info.width >> mip).max(1) as usize)
                            * ((info.height >> mip).max(1) as usize)
                            * 4
                    })
                    .sum::<usize>()
            })
            .sum::<usize>()
            + areas
                .iter()
                .flat_map(|a| &a.layers)
                .map(|l| l.mask_size * l.mask_size)
                .sum::<usize>();
        eprintln!("Terrain CPU sources: {:.1} MiB packed; eager RGBA/masks would use {:.1} MiB (temporary decode per upload)",
            packed_bytes as f64 / 1048576., expanded_bytes as f64 / 1048576.);
        Ok(Self {
            textures,
            areas,
            areas_x,
            mask_grid,
            width,
            height,
            spacing,
        })
    }
    /// The desktop GPU owns uploaded copies and no longer CPU-bakes terrain.
    /// Transfer upload ownership without retaining DDS/mask payloads on the worker.
    pub fn take_window(&mut self, window: Option<[usize; 4]>) -> Self {
        for (id, area) in self.areas.iter_mut().enumerate() {
            if !in_window(id, self.areas_x, window) {
                area.layers = Vec::new();
            }
        }
        Self {
            textures: std::mem::take(&mut self.textures),
            areas: std::mem::take(&mut self.areas),
            areas_x: self.areas_x,
            mask_grid: self.mask_grid,
            width: self.width,
            height: self.height,
            spacing: self.spacing,
        }
    }
    pub fn release_gpu_source(&mut self) {
        self.areas.clear();
        self.textures.clear();
        self.areas.shrink_to_fit();
        self.textures.shrink_to_fit();
        #[cfg(target_os = "linux")]
        unsafe {
            unsafe extern "C" {
                fn malloc_trim(pad: usize) -> i32;
            }
            malloc_trim(0);
        }
    }
    pub fn sample(&self, x: f32, y: f32, h: f32) -> [f32; 3] {
        let gx = x.clamp(0., self.width as f32 - 1.);
        let gy = y.clamp(0., self.height as f32 - 1.);
        let ax = gx as usize / self.mask_grid;
        let ay = gy as usize / self.mask_grid;
        let area = &self.areas[ay * self.areas_x + ax];
        let mut color = [0.; 3];
        for (i, l) in area.layers.iter().enumerate() {
            let tex = &self.textures[l.texture];
            let world_x = (gx - self.width as f32 * 0.5) * self.spacing;
            let world_z = (self.height as f32 * 0.5 - gy) * self.spacing;
            let (u, v) = layer_uv(l.axis, world_x, h, world_z);
            let ux = (u * l.u).rem_euclid(1.) * tex.w as f32 - 0.5;
            let vy = (v * l.v).rem_euclid(1.) * tex.h as f32 - 0.5;
            let x0 = ux.floor() as isize;
            let y0 = vy.floor() as isize;
            let fx = ux - ux.floor();
            let fy = vy - vy.floor();
            let texel = |x: isize, y: isize, k: usize| -> f32 {
                let xx = x.rem_euclid(tex.w as isize) as usize;
                let yy = y.rem_euclid(tex.h as isize) as usize;
                // CPU terrain colouring samples level 0 of the authored chain.
                tex.cpu_levels()[0].rgba[(yy * tex.w + xx) * 4 + k] as f32
            };
            let alpha = if i == 0 {
                1.
            } else {
                let mx = ((gx % self.mask_grid as f32) / self.mask_grid as f32)
                    * l.mask_size as f32
                    - 0.5;
                let my = ((gy % self.mask_grid as f32) / self.mask_grid as f32)
                    * l.mask_size as f32
                    - 0.5;
                let ix = mx.floor() as isize;
                let iy = my.floor() as isize;
                let dx = mx - mx.floor();
                let dy = my - my.floor();
                let mask = |x: isize, y: isize| -> f32 {
                    let x = x.clamp(0, l.mask_size as isize - 1) as usize;
                    let y = y.clamp(0, l.mask_size as isize - 1) as usize;
                    l.mask.cpu_bytes(l.mask_size)[y * l.mask_size + x] as f32 / 255.
                };
                let top = mask(ix, iy) * (1. - dx) + mask(ix + 1, iy) * dx;
                let bottom = mask(ix, iy + 1) * (1. - dx) + mask(ix + 1, iy + 1) * dx;
                top * (1. - dy) + bottom * dy
            };
            for k in 0..3 {
                let top = texel(x0, y0, k) * (1. - fx) + texel(x0 + 1, y0, k) * fx;
                let bottom = texel(x0, y0 + 1, k) * (1. - fx) + texel(x0 + 1, y0 + 1, k) * fx;
                let value = top * (1. - fy) + bottom * fy;
                color[k] = color[k] * (1. - alpha) + value * alpha;
            }
        }
        color
    }
}

/// Asset-free production-upload fixture: one base plus one distinct 256² mask
/// per area, enough areas to cross an atlas page boundary.
#[cfg(test)]
pub(crate) fn cutoff_mask_fixture() -> Materials {
    Materials {
        textures: vec![],
        areas_x: 20,
        mask_grid: 128,
        width: 2560,
        height: 2560,
        spacing: 2.,
        areas: (0..400)
            .map(|id| Area {
                layers: vec![
                    Layer {
                        texture: 0,
                        axis: 1,
                        u: 1.,
                        v: 1.,
                        mask_size: 0,
                        mask: Mask::raw(vec![]),
                    },
                    Layer {
                        texture: 0,
                        axis: 1,
                        u: 1.,
                        v: 1.,
                        mask_size: 256,
                        mask: Mask::raw(vec![id as u8; 256 * 256]),
                    },
                ],
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_masks_upload_losslessly_without_retaining_expanded_pixels() {
        use std::io::Write;
        let expected: Vec<u8> = (0..256 * 256)
            .map(|i| ((i / 256 + i % 256) % 256) as u8)
            .collect();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&expected).unwrap();
        let mask = Mask {
            bytes: encoder.finish().unwrap(),
            compressed: true,
            decoded: Default::default(),
        };
        assert!(mask.bytes.len() < expected.len() / 4);
        assert_eq!(mask.decode(256).unwrap().as_ref(), expected);
        assert!(mask.decoded.get().is_none(), "GPU decode must be temporary");
        assert_eq!(mask.cpu_bytes(256), expected);
        assert!(
            mask.decode(128).is_err(),
            "wrong size is rejected, not truncated"
        );
    }

    #[test]
    fn blends_real_mask_weight() {
        let m = Materials {
            textures: vec![
                Texture {
                    w: 1,
                    h: 1,
                    source: vec![],
                    levels: std::sync::OnceLock::from(vec![crate::dxt::Level {
                        width: 1,
                        height: 1,
                        rgba: vec![200, 0, 0, 255],
                    }]),
                },
                Texture {
                    w: 1,
                    h: 1,
                    source: vec![],
                    levels: std::sync::OnceLock::from(vec![crate::dxt::Level {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 200, 255],
                    }]),
                },
            ],
            areas: vec![Area {
                layers: vec![
                    Layer {
                        texture: 0,
                        axis: 1,
                        u: 1.,
                        v: 1.,
                        mask_size: 0,
                        mask: Mask::raw(vec![]),
                    },
                    Layer {
                        texture: 1,
                        axis: 1,
                        u: 1.,
                        v: 1.,
                        mask_size: 1,
                        mask: Mask::raw(vec![128]),
                    },
                ],
            }],
            areas_x: 1,
            mask_grid: 4,
            width: 4,
            height: 4,
            spacing: 2.,
        };
        let v = m.sample(1., 1., 0.);
        assert!((v[0] - 99.6).abs() < 1.0 && v[1] == 0. && (v[2] - 100.4).abs() < 1.0);
    }

    /// The axis byte's projection, and that the GPU path states the same thing.
    /// Axis 1 is the one every surveyed area base uses, so it is the case that
    /// matters most; the shader spells it as the default arm, which is why this
    /// test also checks the *shape* of the WGSL rather than only the numbers.
    #[test]
    fn axis_projection_matches_the_shader() {
        let (x, y, z) = (3.0f32, -7.0f32, 1.5f32);
        // 0 -> (z, -y), 1 -> (x, -z), 2 -> (x, -y).
        assert_eq!(layer_uv(0, x, y, z), (z, -y));
        assert_eq!(layer_uv(1, x, y, z), (x, -z));
        assert_eq!(layer_uv(2, x, y, z), (x, -y));
        // The default arm is axis 1, so an unknown byte cannot silently pick a
        // third projection that neither reader expresses.
        assert_eq!(layer_uv(1, x, y, z), layer_uv(9, x, y, z));

        let wgsl = include_str!("terrain_gpu.rs");
        // The shader's base vector and its per-axis overrides, verbatim.
        assert!(wgsl.contains("var v=vec2<f32>(i.world.x,-i.world.z);"));
        assert!(wgsl.contains("if l.axis==0u {v=vec2<f32>(i.world.z,-i.world.y);}"));
        assert!(wgsl.contains("else if l.axis==2u {v=vec2<f32>(i.world.x,-i.world.y);}"));
        // `i.world.x` is world X, `i.world.y` the height and `i.world.z` world Z,
        // matching the (x, y, z) arguments above - so the two agree term for term.
        assert_eq!(layer_uv(0, x, y, z).0, z);
        assert_eq!(layer_uv(2, x, y, z).1, -y);
    }
    #[test]
    fn cutoff_transfers_only_resident_payloads_without_renumbering_areas() {
        let mut m = Materials {
            textures: vec![],
            areas: (0..16)
                .map(|id| Area {
                    layers: vec![Layer {
                        texture: 0,
                        axis: 1,
                        u: 1.,
                        v: 1.,
                        mask_size: 1,
                        mask: Mask::raw(vec![id as u8]),
                    }],
                })
                .collect(),
            areas_x: 4,
            mask_grid: 128,
            width: 512,
            height: 512,
            spacing: 2.,
        };
        let upload = m.take_window(Some([1, 1, 3, 3]));
        assert!(m.areas.is_empty());
        assert!(m.textures.is_empty());
        assert_eq!(m.mask_grid, 128);
        assert_eq!(upload.areas.len(), 16);
        for (id, a) in upload.areas.iter().enumerate() {
            if [5, 6, 9, 10].contains(&id) {
                assert_eq!(a.layers[0].mask.decode(1).unwrap().as_ref(), &[id as u8]);
            } else {
                assert!(a.layers.is_empty());
            }
        }
    }
}
