//! Small RGBA texture tiles with explicit wrap/clamp and bilinear filtering.
//! Packed texels are shared by both backends (no descriptor indexing required).
use std::collections::HashMap;

pub const MAX_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
pub const COLOR_SIDE: u32 = 128;
pub const MASK_SIDE: u32 = 32;

#[derive(Clone)]
pub struct Textures {
    pub words: Vec<u32>,
    archive: HashMap<&'static str, HashMap<String, Option<(u32, bool)>>>,
    pub missing: usize,
}

impl Default for Textures {
    fn default() -> Self {
        Self {
            words: vec![1, 1, u32::MAX],
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

    pub fn tile(
        &mut self,
        size: [u32; 2],
        limit: u32,
        mut pixel: impl FnMut(u32, u32) -> u32,
    ) -> Result<u32, String> {
        let [w, h] = size;
        if w == 0 || h == 0 {
            return Err("empty ray material texture".into());
        }
        let [sw, sh] = size.map(|n| n.min(limit));
        let addr = self.reserve((sw * sh) as usize + 2)?;
        self.words[addr] = sw;
        self.words[addr + 1] = sh;
        for y in 0..sh {
            for x in 0..sw {
                let px = ((x as u64 * w as u64 + w as u64 / 2) / sw as u64) as u32;
                let py = ((y as u64 * h as u64 + h as u64 / 2) / sh as u64) as u32;
                self.words[addr + 2 + (y * sw + x) as usize] = pixel(px.min(w - 1), py.min(h - 1));
            }
        }
        Ok(addr as u32)
    }

    pub fn rgba(&mut self, size: [u32; 2], data: &[u8]) -> Result<u32, String> {
        let [w, h] = size;
        if w as u64 * h as u64 > (data.len() / 4) as u64 {
            return Err("truncated ray material texture".into());
        }
        self.tile(size, COLOR_SIDE, |x, y| {
            let i = (y as usize * w as usize + x as usize) * 4;
            u32::from_le_bytes(data[i..i + 4].try_into().unwrap())
        })
    }

    pub fn archive(
        &mut self,
        namespace: &'static str,
        name: &str,
        decode: &mut dyn FnMut(&str) -> Result<(image::RgbaImage, bool), String>,
    ) -> Result<Option<(u32, bool)>, String> {
        if name.is_empty() {
            return Ok(Some((0, false)));
        }
        if let Some(v) = self.archive.get(namespace).and_then(|a| a.get(name)) {
            return Ok(*v);
        }
        let value = match decode(name) {
            Ok((image, has_alpha)) => Some((
                self.rgba([image.width(), image.height()], image.as_raw())?,
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
            .insert(name.into(), value);
        Ok(value)
    }
}
