//! Item icon atlas reader and lazy `egui` texture cache for Librarian.
//!
//! Retail Perfect World packs item icons into four `surfaces/iconset/` sheets:
//! `iconlist_ivtrm` (male/general inventory), `iconlist_ivtrf` (female
//! inventory), `iconlist_pet`, and `iconlist_elf` (genies). Each `.txt` header
//! gives `(cell_w, cell_h, rows, cols)` followed by GBK basenames in row-major
//! order, while the companion `.dds` holds an unmipped DXT1 or DXT3 sheet.
//! Slicing only the requested cell's 4x4 blocks keeps startup instantaneous and
//! avoids decoding 67 MB of uncompressed RGBA8 sheets upfront.

use super::archive::{normalize_archive_path, LibrarianAssets};
use encoding_rs::GBK;
use std::collections::HashMap;

const DDS_MAGIC: &[u8; 4] = b"DDS ";
const DDS_HEADER_BYTES: usize = 128;
const FOURCC_DXT1: &[u8; 4] = b"DXT1";
const FOURCC_DXT3: &[u8; 4] = b"DXT3";
const FOURCC_DXT5: &[u8; 4] = b"DXT5";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockFormat {
    Dxt1,
    Dxt3,
    Dxt5,
}

impl BlockFormat {
    fn block_bytes(self) -> usize {
        match self {
            Self::Dxt1 => 8,
            Self::Dxt3 | Self::Dxt5 => 16,
        }
    }
}

struct IconSheet {
    cell_width: u32,
    cell_height: u32,
    cols: u32,
    dds_width: u32,
    dds_height: u32,
    format: BlockFormat,
    dds: Vec<u8>,
    by_name: HashMap<String, u32>,
}

impl IconSheet {
    fn load(assets: &mut LibrarianAssets, stem: &str) -> Option<Self> {
        let txt_bytes = assets
            .read_surface(&format!("surfaces/iconset/{stem}.txt"))
            .ok()?;
        let dds = assets
            .read_surface(&format!("surfaces/iconset/{stem}.dds"))
            .ok()?;
        Self::from_parts(&txt_bytes, dds)
    }

    fn from_parts(txt_bytes: &[u8], dds: Vec<u8>) -> Option<Self> {
        if dds.len() < DDS_HEADER_BYTES || &dds[..4] != DDS_MAGIC {
            return None;
        }
        let dds_height = u32::from_le_bytes(dds[12..16].try_into().ok()?);
        let dds_width = u32::from_le_bytes(dds[16..20].try_into().ok()?);
        let fourcc: &[u8; 4] = dds[84..88].try_into().ok()?;
        let format = match fourcc {
            FOURCC_DXT1 => BlockFormat::Dxt1,
            FOURCC_DXT3 => BlockFormat::Dxt3,
            FOURCC_DXT5 => BlockFormat::Dxt5,
            _ => return None,
        };
        let (decoded, _, _) = GBK.decode(txt_bytes);
        let mut lines = decoded
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let cell_width: u32 = lines.next()?.parse().ok()?;
        let cell_height: u32 = lines.next()?.parse().ok()?;
        let rows: u32 = lines.next()?.parse().ok()?;
        let cols: u32 = lines.next()?.parse().ok()?;
        if cell_width == 0
            || cell_height == 0
            || cols == 0
            || rows == 0
            || cell_width.checked_mul(cols)? > dds_width
            || cell_height.checked_mul(rows)? > dds_height
            || cell_width.checked_mul(cell_height)?.checked_mul(4)? > 16 * 1024 * 1024
            || cell_width % 4 != 0
            || cell_height % 4 != 0
            || dds_width % 4 != 0
            || dds_height % 4 != 0
        {
            return None;
        }
        let count = rows.checked_mul(cols)? as usize;
        let mut by_name = HashMap::with_capacity(count);
        for (index, name) in lines.take(count).enumerate() {
            let lower = if name.is_ascii() {
                name.to_ascii_lowercase()
            } else {
                name.to_lowercase()
            };
            by_name.entry(lower).or_insert(index as u32);
        }
        Some(Self {
            cell_width,
            cell_height,
            cols,
            dds_width,
            dds_height,
            format,
            dds,
            by_name,
        })
    }

    fn decode_by_name(&self, leaf_lower: &str) -> Option<(u32, u32, Vec<u8>)> {
        let &index = self.by_name.get(leaf_lower)?;
        self.decode_index(index)
    }

    fn decode_index(&self, index: u32) -> Option<(u32, u32, Vec<u8>)> {
        let row = index / self.cols;
        let col = index % self.cols;
        let cell_bx = (self.cell_width / 4) as usize;
        let cell_by = (self.cell_height / 4) as usize;
        let sheet_bx = (self.dds_width / 4) as usize;
        let sheet_by = (self.dds_height / 4) as usize;
        let start_bx = col as usize * cell_bx;
        let start_by = row as usize * cell_by;
        if start_bx + cell_bx > sheet_bx || start_by + cell_by > sheet_by {
            return None;
        }
        let block_bytes = self.format.block_bytes();
        let mut rgba = vec![0u8; (self.cell_width * self.cell_height * 4) as usize];
        let stride = self.cell_width as usize * 4;
        for by in 0..cell_by {
            for bx in 0..cell_bx {
                let offset =
                    DDS_HEADER_BYTES + ((start_by + by) * sheet_bx + (start_bx + bx)) * block_bytes;
                let block = self.dds.get(offset..offset + block_bytes)?;
                let pixels = decode_dxt_block(block, self.format);
                for py in 0..4usize {
                    let dst_y = by * 4 + py;
                    let dst_row = dst_y * stride + bx * 16;
                    for px in 0..4usize {
                        rgba[dst_row + px * 4..dst_row + px * 4 + 4]
                            .copy_from_slice(&pixels[py * 4 + px]);
                    }
                }
            }
        }
        Some((self.cell_width, self.cell_height, rgba))
    }
}

fn expand_rgb565(color: u16) -> [u8; 3] {
    let r = (color >> 11) & 0x1f;
    let g = (color >> 5) & 0x3f;
    let b = color & 0x1f;
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

fn decode_dxt_block(block: &[u8], format: BlockFormat) -> [[u8; 4]; 16] {
    let color_block = match format {
        BlockFormat::Dxt1 => &block[..8],
        BlockFormat::Dxt3 | BlockFormat::Dxt5 => &block[8..16],
    };
    let c0 = u16::from_le_bytes([color_block[0], color_block[1]]);
    let c1 = u16::from_le_bytes([color_block[2], color_block[3]]);
    let bits = u32::from_le_bytes([
        color_block[4],
        color_block[5],
        color_block[6],
        color_block[7],
    ]);
    let rgb0 = expand_rgb565(c0);
    let rgb1 = expand_rgb565(c1);
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [rgb0[0], rgb0[1], rgb0[2], 255];
    palette[1] = [rgb1[0], rgb1[1], rgb1[2], 255];
    if format != BlockFormat::Dxt1 || c0 > c1 {
        for ch in 0..3 {
            palette[2][ch] = ((2 * rgb0[ch] as u16 + rgb1[ch] as u16) / 3) as u8;
            palette[3][ch] = ((rgb0[ch] as u16 + 2 * rgb1[ch] as u16) / 3) as u8;
        }
        palette[2][3] = 255;
        palette[3][3] = 255;
    } else {
        for ch in 0..3 {
            palette[2][ch] = ((rgb0[ch] as u16 + rgb1[ch] as u16) / 2) as u8;
        }
        palette[2][3] = 255;
        palette[3] = [0, 0, 0, 0];
    }

    let mut out = [[0u8; 4]; 16];
    for i in 0..16usize {
        let code = ((bits >> (i * 2)) & 0x3) as usize;
        out[i] = palette[code];
    }

    match format {
        BlockFormat::Dxt1 => {}
        BlockFormat::Dxt3 => {
            for i in 0..8usize {
                let byte = block[i];
                out[2 * i][3] = (byte & 0x0f) * 17;
                out[2 * i + 1][3] = (byte >> 4) * 17;
            }
        }
        BlockFormat::Dxt5 => {
            let a0 = block[0];
            let a1 = block[1];
            let mut alpha_lut = [0u8; 8];
            alpha_lut[0] = a0;
            alpha_lut[1] = a1;
            if a0 > a1 {
                for j in 0..6u16 {
                    alpha_lut[2 + j as usize] =
                        (((6 - j) * a0 as u16 + (1 + j) * a1 as u16) / 7) as u8;
                }
            } else {
                for j in 0..4u16 {
                    alpha_lut[2 + j as usize] =
                        (((4 - j) * a0 as u16 + (1 + j) * a1 as u16) / 5) as u8;
                }
                alpha_lut[6] = 0;
                alpha_lut[7] = 255;
            }
            let mut abits = 0u64;
            for k in 0..6usize {
                abits |= (block[2 + k] as u64) << (8 * k);
            }
            for i in 0..16usize {
                out[i][3] = alpha_lut[((abits >> (3 * i)) & 0x7) as usize];
            }
        }
    }
    out
}

pub struct IconCache {
    ivtrm: Option<IconSheet>,
    ivtrf: Option<IconSheet>,
    pet: Option<IconSheet>,
    elf: Option<IconSheet>,
    handles: HashMap<String, Option<egui::TextureHandle>>,
    fallback: Option<egui::TextureHandle>,
}

impl IconCache {
    pub fn load(assets: &mut LibrarianAssets) -> Self {
        Self {
            ivtrm: IconSheet::load(assets, "iconlist_ivtrm"),
            ivtrf: IconSheet::load(assets, "iconlist_ivtrf"),
            pet: IconSheet::load(assets, "iconlist_pet"),
            elf: IconSheet::load(assets, "iconlist_elf"),
            handles: HashMap::new(),
            fallback: None,
        }
    }

    pub fn has_atlas(&self) -> bool {
        self.ivtrm.is_some() || self.ivtrf.is_some() || self.pet.is_some() || self.elf.is_some()
    }

    pub fn get(
        &mut self,
        ctx: &egui::Context,
        assets: &mut LibrarianAssets,
        icon_path: &str,
    ) -> Option<egui::TextureId> {
        let key = normalize_archive_path(icon_path);
        if !key.is_empty() && key != "0" {
            if let Some(cached) = self.handles.get(&key) {
                if let Some(handle) = cached {
                    return Some(handle.id());
                }
            } else {
                let decoded = self.decode_icon(assets, &key);
                let handle = decoded.map(|(w, h, rgba)| {
                    let image =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    ctx.load_texture(
                        format!("librarian-icon:{key}"),
                        image,
                        egui::TextureOptions::NEAREST,
                    )
                });
                let id = handle.as_ref().map(egui::TextureHandle::id);
                self.handles.insert(key, handle);
                if id.is_some() {
                    return id;
                }
            }
        }
        self.fallback_id(ctx)
    }

    fn fallback_id(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        if self.fallback.is_none() {
            let decoded = self.ivtrm.as_ref().and_then(|sheet| sheet.decode_index(0));
            if let Some((w, h, rgba)) = decoded {
                let image =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.fallback = Some(ctx.load_texture(
                    "librarian-icon:fallback",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
        }
        self.fallback.as_ref().map(egui::TextureHandle::id)
    }

    fn decode_icon(
        &self,
        assets: &mut LibrarianAssets,
        normalized_path: &str,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let leaf = normalized_path
            .rsplit('/')
            .next()
            .unwrap_or(normalized_path);
        if leaf.is_empty() {
            return None;
        }
        let female_first = normalized_path.contains("女物品") || normalized_path.contains("/女");
        let pet_first = normalized_path.contains("宠物");
        let elf_first = normalized_path.contains("小精灵");

        let order: [&Option<IconSheet>; 4] = if elf_first {
            [&self.elf, &self.ivtrm, &self.ivtrf, &self.pet]
        } else if pet_first {
            [&self.pet, &self.ivtrm, &self.ivtrf, &self.elf]
        } else if female_first {
            [&self.ivtrf, &self.ivtrm, &self.pet, &self.elf]
        } else {
            [&self.ivtrm, &self.ivtrf, &self.pet, &self.elf]
        };

        for sheet in order.into_iter().flatten() {
            if let Some(img) = sheet.decode_by_name(leaf) {
                return Some(img);
            }
        }

        // Loose surface image fallback (.dds, .tga, .bmp, .png) if present outside iconsets.
        if let Ok(bytes) = assets.read_surface(normalized_path) {
            if let Ok(img) = image::load_from_memory(&bytes) {
                let rgba = img.to_rgba8();
                return Some((rgba.width(), rgba.height(), rgba.into_raw()));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentic_client_atlas_when_available() {
        let Ok(root) = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT") else {
            return;
        };
        let mut assets = LibrarianAssets::open(std::path::Path::new(&root));
        let sheet = IconSheet::load(&mut assets, "iconlist_ivtrm").expect("load inventory atlas");
        assert_eq!((sheet.cell_width, sheet.cell_height), (32, 32));
        let (_, _, rgba) = sheet.decode_index(0).expect("decode first atlas cell");
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }

    #[test]
    fn slices_dxt1_icon_sheet_cell_accurately() {
        let mut dds = vec![0u8; DDS_HEADER_BYTES + 4 * 8];
        dds[..4].copy_from_slice(DDS_MAGIC);
        dds[12..16].copy_from_slice(&8u32.to_le_bytes()); // height = 8
        dds[16..20].copy_from_slice(&8u32.to_le_bytes()); // width = 8
        dds[84..88].copy_from_slice(FOURCC_DXT1);

        // Cell 0 at (0,0): pure red (0xf800). Cell 1 at (0,1): pure green (0x07e0).
        let red_block = [0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0];
        let green_block = [0xe0, 0x07, 0xe0, 0x07, 0, 0, 0, 0];
        dds[128..136].copy_from_slice(&red_block);
        dds[136..144].copy_from_slice(&green_block);

        let txt = b"4\n4\n2\n2\nred.dds\ngreen.dds\n";
        let sheet = IconSheet::from_parts(txt, dds).expect("valid synthetic sheet");
        let (w, h, red_rgba) = sheet.decode_by_name("red.dds").expect("red cell");
        assert_eq!((w, h), (4, 4));
        assert_eq!(&red_rgba[..4], &[255, 0, 0, 255]);

        let (_, _, green_rgba) = sheet.decode_by_name("green.dds").expect("green cell");
        assert_eq!(&green_rgba[..4], &[0, 255, 0, 255]);
    }
}
