//! Isolated mip chains in a shared storage buffer (no descriptor indexing).
//! Colour levels keep sRGB bytes. Decode and premultiply each tap in linear
//! light before filtering; alpha and data maps never undergo an sRGB transform.
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

pub const MAX_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
pub const COLOR_SIDE: u32 = 512;
pub const MASK_SIDE: u32 = 32;
const AREA_MIPS: u32 = 1;
const AREA_TOP: u32 = 2;
const LINEAR_DATA: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    Color,
    Data,
    Alpha,
    // SpeedTree can author a fractional reference with a strict `>` test.
    Cutout(u32, bool),
}

impl Filter {
    pub fn cutout(reference: f32, strict: bool) -> Self {
        Self::Cutout(reference.to_bits(), strict)
    }

    fn pixel(self, pixel: u32, tested_area: bool) -> u32 {
        let [r, g, b, a] = pixel.to_le_bytes();
        let a = match self {
            Self::Color => 255,
            Self::Cutout(bits, strict) if tested_area => {
                let alpha = f32::from(a) / 255.;
                let reference = f32::from_bits(bits);
                if alpha > reference || (!strict && alpha == reference) {
                    255
                } else {
                    0
                }
            }
            _ => a,
        };
        u32::from_le_bytes([r, g, b, a])
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MipStats {
    pub authored_levels: usize,
    pub generated: usize,
    pub reduced: usize,
    // Tile count (generated OR reduced), not a count of missing textures.
    pub fallback: usize,
}

impl MipStats {
    fn tile(authored_levels: usize, generated: bool, reduced: bool) -> Self {
        Self {
            authored_levels,
            generated: usize::from(generated),
            reduced: usize::from(reduced),
            fallback: usize::from(generated || reduced),
        }
    }
    fn add(&mut self, tile: Self) {
        self.authored_levels += tile.authored_levels;
        self.generated += tile.generated;
        self.reduced += tile.reduced;
        self.fallback += tile.fallback;
    }
}

pub struct Tile(Vec<u32>, MipStats);

fn extent_at(size: [u32; 2], lod: u32) -> [u32; 2] {
    size.map(|n| (n >> lod).max(1))
}
fn level_count(size: [u32; 2]) -> u32 {
    u32::BITS - size[0].max(size[1]).leading_zeros()
}
fn first_lod(size: [u32; 2], limit: u32) -> u32 {
    (0..level_count(size))
        .find(|&lod| size[0].max(size[1]) >> lod <= limit)
        .unwrap_or(0)
}
/// Conservative space estimate used to choose a common admission resolution.
pub fn chain_bytes(size: [u32; 2], limit: u32) -> usize {
    let start = first_lod(size, limit.max(1));
    let mut words = 4usize;
    for lod in start..level_count(size) {
        let [w, h] = extent_at(size, lod);
        words = words.saturating_add((w as usize).saturating_mul(h as usize).saturating_add(1));
    }
    words.saturating_mul(4)
}

impl Tile {
    pub fn bytes(&self) -> usize {
        self.0.len() * std::mem::size_of::<u32>()
    }

    /// Both an explicit single level (e.g. TGA) and an incomplete chain can
    /// fall back. An authored chain is kept byte-for-byte (except that opaque
    /// colour ignores alpha). If capped, prefer dropping its leading levels
    /// over shrinking level zero with a different filter. A short authored
    /// chain remains short; its last resident level is the LOD clamp.
    pub fn levels<'a>(
        levels: impl IntoIterator<Item = ([u32; 2], &'a [u8])>,
        limit: u32,
        filter: Filter,
    ) -> Result<Self, String> {
        let levels: Vec<_> = levels.into_iter().collect();
        let &(top, _) = levels.first().ok_or("empty ray material mip chain")?;
        if top.contains(&0) || limit == 0 || levels.len() > level_count(top) as usize {
            return Err("invalid ray material mip extent or count".into());
        }
        for (lod, &(size, rgba)) in levels.iter().enumerate() {
            let [w, h] = size.map(u64::from);
            if size != extent_at(top, lod as u32)
                || rgba.len() % 4 != 0
                || w * h != (rgba.len() / 4) as u64
            {
                return Err(format!("invalid ray material mip {lod}"));
            }
        }
        let start = first_lod(top, limit) as usize;
        if levels.len() == 1 || start >= levels.len() {
            let &([w, h], rgba) = levels.last().unwrap();
            let mut tile = Self::new([w, h], limit, filter, |x, y| {
                let i = (y as usize * w as usize + x as usize) * 4;
                u32::from_le_bytes(rgba[i..i + 4].try_into().unwrap())
            })?;
            if levels.len() > 1 {
                tile.1 = MipStats::tile(0, true, true);
            }
            return Ok(tile);
        }
        let suffix = &levels[start..];
        let ([w, h], _) = suffix[0];
        let mut words = vec![
            w,
            h,
            suffix.len() as u32,
            u32::from(filter == Filter::Data) * LINEAR_DATA,
        ];
        words.resize(4 + suffix.len(), 0);
        for (lod, (_, rgba)) in suffix.iter().enumerate() {
            words[4 + lod] = words.len() as u32;
            words.extend(
                rgba.chunks_exact(4)
                    .map(|p| filter.pixel(u32::from_le_bytes(p.try_into().unwrap()), false)),
            );
        }
        Ok(Self(words, MipStats::tile(suffix.len(), false, start > 0)))
    }

    pub fn images<'a>(
        images: impl IntoIterator<Item = &'a image::RgbaImage>,
        limit: u32,
        filter: Filter,
    ) -> Result<Self, String> {
        Self::levels(
            images
                .into_iter()
                .map(|i| ([i.width(), i.height()], i.as_raw().as_slice())),
            limit,
            filter,
        )
    }

    pub fn new(
        size: [u32; 2],
        limit: u32,
        filter: Filter,
        mut pixel: impl FnMut(u32, u32) -> u32,
    ) -> Result<Self, String> {
        if size.contains(&0) || limit == 0 {
            return Err("empty ray material texture".into());
        }
        let dims = extent_at(size, first_lod(size, limit));
        let levels = level_count(dims);
        let cutout = matches!(filter, Filter::Cutout(..));
        let reduced = dims != size;
        let data = filter == Filter::Data;
        // [width, height, levels, flags, level-relative offsets..., texels...]
        // Generated cutout mips store tested area. An authored chain never
        // reaches this branch unless no useful authored level fits.
        let mut flags = u32::from(data) * LINEAR_DATA;
        if cutout {
            flags |= AREA_MIPS | (u32::from(reduced) * AREA_TOP);
        }
        let mut words = vec![dims[0], dims[1], levels, flags];
        words.resize(4 + levels as usize, 0);
        let mut src = dims;
        let mut pixels = resample(size, dims, data, |x, y| filter.pixel(pixel(x, y), reduced));
        for level in 0..levels as usize {
            words[4 + level] = words.len() as u32;
            words.extend_from_slice(&pixels);
            if level + 1 == levels as usize {
                break;
            }
            let dst = extent_at(src, 1);
            pixels = resample(src, dst, data, |x, y| {
                let p = pixels[(y as usize * src[0] as usize) + x as usize];
                if cutout && level == 0 && !reduced {
                    filter.pixel(p, true)
                } else {
                    p
                }
            });
            src = dst;
        }
        Ok(Self(
            words,
            MipStats::tile(0, levels > 1 || reduced, reduced),
        ))
    }
}

fn decode_srgb(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn encode_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        c.powf(1. / 2.4) * 1.055 - 0.055
    }
}

// Exact texel overlap in integer coordinates retains odd/rectangular edges.
// Accumulate colours as associated linear RGB, then store unassociated sRGB
// plus linear coverage. Data channels have no associated-alpha operation.
fn resample(
    [w, h]: [u32; 2],
    [sw, sh]: [u32; 2],
    data: bool,
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
    static LINEAR: OnceLock<[f32; 256]> = OnceLock::new();
    let lut = LINEAR.get_or_init(|| std::array::from_fn(|i| decode_srgb(i as f32 / 255.)));
    let area = (w * h) as f32;
    for y in 0..sh {
        let (y0, y1) = (y * h, (y + 1) * h);
        for x in 0..sw {
            let (x0, x1) = (x * w, (x + 1) * w);
            let mut sum = [0.; 4];
            for py in y0 / sh..y1.div_ceil(sh) {
                let dy = y1.min((py + 1) * sh) - y0.max(py * sh);
                for px in x0 / sw..x1.div_ceil(sw) {
                    let weight = (dy * (x1.min((px + 1) * sw) - x0.max(px * sw))) as f32;
                    let c = pixel(px as u32, py as u32).to_le_bytes();
                    let a = f32::from(c[3]) / 255.;
                    sum[3] += a * weight;
                    for i in 0..3 {
                        sum[i] += if data {
                            f32::from(c[i]) / 255.
                        } else {
                            lut[c[i] as usize] * a
                        } * weight;
                    }
                }
            }
            let a = sum[3];
            for value in &mut sum[..3] {
                *value = if data {
                    *value / area
                } else if a > 0. {
                    encode_srgb(*value / a)
                } else {
                    0.
                };
            }
            sum[3] /= area;
            out.push(u32::from_le_bytes(
                sum.map(|v| (v.clamp(0., 1.) * 255.).round() as u8),
            ));
        }
    }
    out
}

/// Views into a single logical array, skipping an already valid GPU prefix.
pub(super) fn suffix_segments<'a>(
    segments: impl IntoIterator<Item = &'a [u8]>,
    start: usize,
) -> impl Iterator<Item = (usize, &'a [u8])> {
    let mut offset = 0;
    segments.into_iter().filter_map(move |part| {
        let base = offset;
        offset += part.len();
        let skip = start.saturating_sub(base);
        (skip < part.len()).then(|| (base + skip, &part[skip..]))
    })
}

type Archive = HashMap<Filter, HashMap<String, Option<(u32, bool)>>>;
pub(super) type TexelPrefix = Vec<Arc<Vec<u32>>>;

#[cfg_attr(test, derive(Clone))]
pub struct Textures {
    // Base colour/sky/vegetation and mask tables are sealed independently.
    // Streaming and animation append only to the private, writable suffix.
    prefix: TexelPrefix,
    prefix_words: usize,
    tail: Vec<u32>,
    archive: HashMap<&'static str, Archive>,
    pub missing: usize,
    pub mips: MipStats,
}

impl Default for Textures {
    fn default() -> Self {
        Self {
            prefix: Vec::new(),
            prefix_words: 0,
            tail: vec![1, 1, 1, 0, 5, u32::MAX],
            archive: HashMap::new(),
            missing: 0,
            mips: MipStats::default(),
        }
    }
}

impl Textures {
    pub fn seal(&mut self) {
        if !self.tail.is_empty() {
            self.prefix_words += self.tail.len();
            // Wrap the owned Vec rather than allocating/copying a new slice.
            self.prefix.push(Arc::new(std::mem::take(&mut self.tail)));
        }
    }

    pub fn fork_prefix(&self) -> Self {
        assert!(self.tail.is_empty() && self.archive.is_empty());
        Self {
            prefix: self.prefix.clone(),
            prefix_words: self.prefix_words,
            tail: Vec::new(),
            archive: HashMap::new(),
            missing: self.missing,
            mips: self.mips,
        }
    }

    pub fn byte_len(&self) -> usize {
        (self.prefix_words + self.tail.len()) * 4
    }

    pub(super) fn byte_segments(&self) -> impl Iterator<Item = &[u8]> {
        self.prefix
            .iter()
            .map(|p| p.as_slice())
            .chain(std::iter::once(self.tail.as_slice()))
            .map(bytemuck::cast_slice)
    }

    pub(super) fn shared_bytes(&self, previous: &TexelPrefix) -> usize {
        self.prefix
            .iter()
            .zip(previous)
            .take_while(|(a, b)| Arc::ptr_eq(a, b))
            .map(|(a, _)| a.len() * 4)
            .sum()
    }

    pub(super) fn remember_prefix(&self, previous: &mut TexelPrefix) {
        previous.clone_from(&self.prefix);
    }

    pub fn write_words(&mut self, addr: usize, words: &[u32]) {
        let i = addr
            .checked_sub(self.prefix_words)
            .expect("sealed texels are immutable");
        self.tail[i..i + words.len()].copy_from_slice(words);
    }

    pub fn reserve(&mut self, len: usize) -> Result<usize, String> {
        let addr = self.prefix_words + self.tail.len();
        if addr.saturating_add(len) > MAX_TEXTURE_BYTES / 4 {
            return Err(
                "Lighter texture budget exceeded (128 MiB); reduce scenery residency".into(),
            );
        }
        self.tail.resize(self.tail.len() + len, 0);
        Ok(addr)
    }

    pub fn insert(&mut self, tile: &Tile) -> Result<u32, String> {
        let addr = self.reserve(tile.0.len())?;
        self.write_words(addr, &tile.0);
        self.mips.add(tile.1);
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

    pub fn images<'a>(
        &mut self,
        images: impl IntoIterator<Item = &'a image::RgbaImage>,
        limit: u32,
        filter: Filter,
    ) -> Result<u32, String> {
        self.insert(&Tile::images(images, limit, filter)?)
    }

    pub fn archive(
        &mut self,
        namespace: &'static str,
        name: &str,
        filter: Filter,
        decode: &mut dyn FnMut(&str) -> Result<(Vec<image::RgbaImage>, bool), String>,
    ) -> Result<Option<(u32, bool)>, String> {
        if name.is_empty() {
            return Ok(Some((0, false)));
        }
        if let Some(v) = self
            .archive
            .get(namespace)
            .and_then(|a| a.get(&filter))
            .and_then(|a| a.get(name))
        {
            return Ok(*v);
        }
        let value = match decode(name) {
            Ok((images, has_alpha)) => Some((
                self.images(
                    &images,
                    COLOR_SIDE,
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
            .entry(filter)
            .or_default()
            .insert(name.into(), value);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_shares_the_immutable_prefix_and_preserves_word_offsets() {
        let mut base = Textures::default();
        base.seal();
        let mut masks = base.fork_prefix();
        let idx = masks.reserve(2).unwrap();
        assert_eq!(idx, 6);
        masks.write_words(idx, &[123, 456]);
        masks.seal();
        let mut live = masks.fork_prefix();
        let mut prev = TexelPrefix::default();
        masks.remember_prefix(&mut prev);
        assert_eq!(live.shared_bytes(&prev), 32);
        let start = live.reserve(1).unwrap();
        live.write_words(start, &[789]);
        assert_eq!(start, 8);
        assert_eq!(live.byte_len(), 36);
        let last: Vec<_> = live
            .byte_segments()
            .flat_map(|p| bytemuck::cast_slice::<u8, u32>(p))
            .skip(6)
            .copied()
            .collect();
        assert_eq!(last, [123, 456, 789]);
        assert_eq!(masks.byte_len(), 32);
    }

    #[test]
    fn retains_a_short_rectangular_chain_and_caps_to_the_exact_lower_level() {
        let base = [12, 30, 180, 80].repeat(15);
        let low = [7, 11, 53, 130].repeat(2);
        let levels = [([5, 3], base.as_slice()), ([2, 1], low.as_slice())];
        let filter = Filter::cutout(0.5, false);
        let full = Tile::levels(levels, 8, filter).unwrap();
        assert_eq!(&full.0[..6], &[5, 3, 2, 0, 6, 21]);
        assert_eq!(full.0[6].to_le_bytes(), base[..4]);
        assert_eq!(full.0[21].to_le_bytes(), low[..4]);
        assert_eq!(full.1, MipStats::tile(2, false, false));
        let capped = Tile::levels(levels, 2, filter).unwrap();
        assert_eq!(&capped.0[..5], &[2, 1, 1, 0, 5]);
        assert_eq!(capped.0[5].to_le_bytes(), low[..4]);
        assert_eq!(capped.1, MipStats::tile(1, false, true));
        let regen = Tile::levels(levels, 1, filter).unwrap();
        assert_eq!(regen.1, MipStats::tile(0, true, true));
        assert!(Tile::levels([([5, 3], &base[..]), ([1, 2], &low[..])], 8, filter).is_err());
    }

    #[test]
    fn generated_colour_mips_filter_linear_rgb_but_do_not_gamma_correct_data() {
        let src = |x, _| if x == 0 { 0xff00_0000 } else { u32::MAX };
        let colour = Tile::new([2, 1], 8, Filter::Color, src).unwrap();
        let data = Tile::new([2, 1], 8, Filter::Data, src).unwrap();
        assert_eq!(colour.0[8].to_le_bytes(), [188, 188, 188, 255]);
        assert_eq!(data.0[8].to_le_bytes(), [128, 128, 128, 255]);
        assert_eq!(data.0[3], LINEAR_DATA);
        let mut bank = Textures::default();
        bank.insert(&colour).unwrap();
        bank.seal();
        let mut view = bank.fork_prefix();
        view.insert(&data).unwrap();
        assert_eq!(view.mips.fallback, 2);
        assert_eq!(view.missing, 0);
        assert_eq!(bank.mips.fallback, 1);
    }

    #[test]
    fn failed_tile_admission_does_not_change_mip_stats() {
        let mut bank = Textures::default();
        // Simulate a full sealed prefix without allocating 128 MiB in a test.
        bank.prefix_words = MAX_TEXTURE_BYTES / 4 - bank.tail.len();
        let tile = Tile::new([2, 1], 2, Filter::Data, |_, _| 0).unwrap();
        assert!(bank.insert(&tile).is_err());
        assert_eq!(bank.mips, MipStats::default());
        assert_eq!(bank.byte_len(), MAX_TEXTURE_BYTES);
    }

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
        assert_eq!(tile[5].to_le_bytes(), [255, 0, 0, 85]);
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
        assert_eq!(tile[9].to_le_bytes(), [0, 156, 0, 255]);
    }
}
