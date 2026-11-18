//! Isolated mip chains in a shared storage buffer (no descriptor indexing).
//! RGB is premultiplied before filtering so transparent texels cannot bleed
//! black or arbitrary RGB into the visible edges of cutouts and effect cards.
use std::collections::HashMap;

pub const MAX_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
pub const COLOR_SIDE: u32 = 128;
pub const MASK_SIDE: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    Color,
    Alpha,
    // The reference is a float because SpeedTree can author a fractional
    // value, and its `>` test is different from the BMD `>=` test.
    Cutout(u32, bool),
}

impl Filter {
    pub fn cutout(reference: f32, strict: bool) -> Self {
        Self::Cutout(reference.to_bits(), strict)
    }

    fn pack(self, pixel: u32, minified: bool) -> u32 {
        let [r, g, b, a] = pixel.to_le_bytes();
        let a = match self {
            Self::Color => 255,
            Self::Cutout(ref_bits, strict) if minified => {
                let alpha = f32::from(a) / 255.;
                let reference = f32::from_bits(ref_bits);
                if alpha > reference || (!strict && alpha == reference) {
                    255
                } else {
                    0
                }
            }
            _ => a,
        };
        let pm = |v| ((u32::from(v) * u32::from(a) + 127) / 255) as u8;
        u32::from_le_bytes([pm(r), pm(g), pm(b), a])
    }
}

pub struct Tile(Vec<u32>);

impl Tile {
    pub fn bytes(&self) -> usize {
        self.0.len() * std::mem::size_of::<u32>()
    }

    pub fn new(
        size: [u32; 2],
        limit: u32,
        filter: Filter,
        mut pixel: impl FnMut(u32, u32) -> u32,
    ) -> Result<Self, String> {
        let [w, h] = size;
        if w == 0 || h == 0 || limit == 0 {
            return Err("empty ray material texture".into());
        }
        let dims = size.map(|n| n.min(limit));
        let levels = u32::BITS - dims[0].max(dims[1]).leading_zeros();
        let cutout = matches!(filter, Filter::Cutout(..));
        let reduced = dims != size;
        // [width, height, levels, flags, level-relative offsets..., texels...]
        // For cutouts, lower mips carry fractional tested area rather than
        // averaged alpha. An unconstrained level zero retains the original
        // alpha so the magnified silhouette still uses the authored reference.
        let mut words = vec![
            dims[0],
            dims[1],
            levels,
            u32::from(cutout) | (u32::from(cutout && reduced) << 1),
        ];
        words.resize(4 + levels as usize, 0);
        let mut src = dims;
        let mut pixels = resample(size, dims, |x, y| filter.pack(pixel(x, y), reduced));
        for level in 0..levels as usize {
            words[4 + level] = words.len() as u32;
            words.extend_from_slice(&pixels);
            if level + 1 == levels as usize {
                break;
            }
            let dst = src.map(|n| (n / 2).max(1));
            pixels = if cutout && level == 0 && !reduced {
                resample(size, dst, |x, y| filter.pack(pixel(x, y), true))
            } else {
                resample(src, dst, |x, y| pixels[(y * src[0] + x) as usize])
            };
            src = dst;
        }
        Ok(Self(words))
    }
}

// Area filter (including odd-sized and rectangular levels). Source and output
// spans share an integer coordinate system, so no boundary texel is dropped or
// taken from a neighboring atlas rect. No full-resolution scratch copy.
fn resample(
    [w, h]: [u32; 2],
    [sw, sh]: [u32; 2],
    mut pixel: impl FnMut(u32, u32) -> u32,
) -> Vec<u32> {
    let [w, h, sw, sh] = [w, h, sw, sh].map(u64::from);
    let mut out = Vec::with_capacity((sw * sh) as usize);
    if w == sw && h == sh {
        for y in 0..h as u32 {
            for x in 0..w as u32 {
                out.push(pixel(x, y));
            }
        }
        return out;
    }
    let area = w * h;
    for y in 0..sh {
        let (y0, y1) = (y * h, (y + 1) * h);
        for x in 0..sw {
            let (x0, x1) = (x * w, (x + 1) * w);
            let mut sum = [0; 4];
            for py in y0 / sh..y1.div_ceil(sh) {
                let dy = y1.min((py + 1) * sh) - y0.max(py * sh);
                for px in x0 / sw..x1.div_ceil(sw) {
                    let weight = dy * (x1.min((px + 1) * sw) - x0.max(px * sw));
                    for (s, c) in sum
                        .iter_mut()
                        .zip(pixel(px as u32, py as u32).to_le_bytes())
                    {
                        *s += u64::from(c) * weight;
                    }
                }
            }
            out.push(u32::from_le_bytes(
                sum.map(|v| ((v + area / 2) / area) as u8),
            ));
        }
    }
    out
}

type Archive = HashMap<(String, Filter), Option<(u32, bool)>>;

#[derive(Clone)]
pub struct Textures {
    pub words: Vec<u32>,
    archive: HashMap<&'static str, Archive>,
    pub missing: usize,
}

impl Default for Textures {
    fn default() -> Self {
        Self {
            words: vec![1, 1, 1, 0, 5, u32::MAX],
            archive: HashMap::new(),
            missing: 0,
        }
    }
}

impl Textures {
    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.words)
    }

    pub fn reserve(&mut self, len: usize) -> Result<usize, String> {
        let addr = self.words.len();
        if addr.saturating_add(len) > MAX_TEXTURE_BYTES / 4 {
            return Err(
                "Lighter texture budget exceeded (128 MiB); reduce scenery residency".into(),
            );
        }
        self.words.resize(addr + len, 0);
        Ok(addr)
    }

    pub fn insert(&mut self, tile: &Tile) -> Result<u32, String> {
        let addr = self.reserve(tile.0.len())?;
        self.words[addr..].copy_from_slice(&tile.0);
        Ok(addr as u32)
    }

    pub fn tile(
        &mut self,
        size: [u32; 2],
        limit: u32,
        filter: Filter,
        pixel: impl FnMut(u32, u32) -> u32,
    ) -> Result<u32, String> {
        self.insert(&Tile::new(size, limit, filter, pixel)?)
    }

    pub fn rgba(&mut self, size: [u32; 2], data: &[u8], filter: Filter) -> Result<u32, String> {
        let [w, h] = size;
        if w as u64 * h as u64 > (data.len() / 4) as u64 {
            return Err("truncated ray material texture".into());
        }
        self.tile(size, COLOR_SIDE, filter, |x, y| {
            let i = (y as usize * w as usize + x as usize) * 4;
            u32::from_le_bytes(data[i..i + 4].try_into().unwrap())
        })
    }

    pub fn archive(
        &mut self,
        namespace: &'static str,
        name: &str,
        filter: Filter,
        decode: &mut dyn FnMut(&str) -> Result<(image::RgbaImage, bool), String>,
    ) -> Result<Option<(u32, bool)>, String> {
        if name.is_empty() {
            return Ok(Some((0, false)));
        }
        let key = (name.into(), filter);
        if let Some(v) = self.archive.get(namespace).and_then(|a| a.get(&key)) {
            return Ok(*v);
        }
        let value = match decode(name) {
            Ok((image, has_alpha)) => Some((
                self.rgba(
                    [image.width(), image.height()],
                    image.as_raw(),
                    if has_alpha { filter } else { Filter::Color },
                )?,
                has_alpha,
            )),
            Err(error) => {
                self.missing += 1;
                if self.missing <= 8 {
                    eprintln!("Lighter: texture {name:?} unavailable ({error})");
                }
                None
            }
        };
        self.archive
            .entry(namespace)
            .or_default()
            .insert(key, value);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_cutout_stores_area_and_cannot_inherit_hidden_rgb() {
        // 1 of 3 texels passes the strict reference. Neither of the failed
        // texels (blue with nonzero alpha and transparent magenta) can bleed.
        let tile = Tile::new([3, 1], 1, Filter::cutout(84. / 255., true), |x, _| {
            u32::from_le_bytes([[255, 0, 0, 90], [0, 0, 255, 84], [255, 0, 255, 0]][x as usize])
        })
        .unwrap()
        .0;
        assert_eq!(&tile[..5], &[1, 1, 1, 3, 5]);
        assert_eq!(tile[5].to_le_bytes(), [85, 0, 0, 85]);
    }

    #[test]
    fn rectangular_mip_keeps_last_texel_and_original_alpha_reference() {
        let tile = Tile::new([1, 3], 128, Filter::cutout(0.5, false), |_, y| {
            if y == 2 {
                0xff00_ff00
            } else {
                0x8000_0000
            }
        })
        .unwrap()
        .0;
        assert_eq!(&tile[..6], &[1, 3, 2, 1, 6, 9]);
        assert_eq!(tile[6], 0x8000_0000);
        assert_eq!(tile[9].to_le_bytes(), [0, 85, 0, 255]);
    }
}
