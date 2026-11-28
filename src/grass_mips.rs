//! Independently filtered grass atlas rectangles. Preparation is performed by
//! the background scene streamer; the render thread only uploads ready levels.
pub struct Tile {
    level: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}
pub fn prepare(
    pixels: &[u8],
    slots: &[(crate::texture_atlas::Slot, u8)],
    side: usize,
) -> Vec<Tile> {
    let mut tiles = Vec::with_capacity(slots.len() * 7);
    for &(slot, alpha_ref) in slots {
        let (mut w, mut h) = (slot.w, slot.h);
        let mut raw = vec![0; w * h * 4];
        for y in 0..h {
            let src = ((slot.y + y) * side + slot.x) * 4;
            raw[y * w * 4..(y + 1) * w * 4].copy_from_slice(&pixels[src..src + w * 4]);
        }
        let coverage = crate::foliage_mips::coverage_at(&raw, alpha_ref);
        for level in 1..8 {
            raw = crate::foliage_mips::downsample(&raw, w, h, true);
            w /= 2;
            h /= 2;
            tiles.push(Tile {
                level,
                x: (slot.x >> level) as u32,
                y: (slot.y >> level) as u32,
                width: w as u32,
                height: h as u32,
                pixels: crate::foliage_mips::preserve_coverage_at(&raw, coverage, alpha_ref),
            });
        }
    }
    tiles
}
/// Validate an authored lower-mip chain for a native-size atlas rectangle.
/// Rectangular chains remain level-for-level; the opt-in grass path stores
/// independent X/Y UV scales instead of resampling the source into a square.
pub fn authored_chain(
    mips: &[image::RgbaImage],
    width: u32,
    height: u32,
    levels: u32,
) -> Result<Vec<image::RgbaImage>, String> {
    if width == 0 || height == 0 {
        return Err("grass authored mips require nonzero slot dimensions".into());
    }
    if mips.len() < levels as usize {
        return Err(format!(
            "grass authored chain has {} lower levels, need {levels}",
            mips.len()
        ));
    }
    for level in 1..=levels {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        let img = &mips[(level - 1) as usize];
        if img.width() != w || img.height() != h {
            return Err(format!(
                "grass authored mip {level} is {}x{}, expected {w}x{h}",
                img.width(),
                img.height()
            ));
        }
    }
    Ok(mips[..levels as usize].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, value: u8) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba([value, 2, 3, 255]))
    }

    #[test]
    fn authored_chain_accepts_native_rectangular_mips_without_resampling() {
        let authored = vec![img(128, 64, 1), img(64, 32, 2), img(32, 16, 3)];
        let chain = authored_chain(&authored, 256, 128, 3).unwrap();
        assert_eq!(
            chain
                .iter()
                .map(|m| (m.width(), m.height()))
                .collect::<Vec<_>>(),
            vec![(128, 64), (64, 32), (32, 16)]
        );
        assert_eq!(chain[0].get_pixel(0, 0).0[0], 1);
    }

    #[test]
    fn authored_chain_accepts_non_power_of_two_dimensions_when_levels_match() {
        let authored = vec![
            img(96, 48, 1),
            img(48, 24, 2),
            img(24, 12, 3),
            img(12, 6, 4),
            img(6, 3, 5),
            img(3, 1, 6),
            img(1, 1, 7),
        ];
        assert!(authored_chain(&authored, 192, 96, 7).is_ok());
    }

    #[test]
    fn authored_chain_rejects_resampled_or_truncated_rectangular_levels() {
        let authored = vec![img(128, 64, 1), img(64, 32, 2), img(32, 32, 3)];
        assert!(authored_chain(&authored, 256, 128, 3).is_err());
        assert!(authored_chain(&authored[..2], 256, 128, 3).is_err());
        assert!(authored_chain(&[], 0, 128, 0).is_err());
    }
}

/// Prepare grass atlas tiles, preferring an exact authored chain when supplied.
/// Slots without a supported authored chain fall back to the approved generated
/// coverage-preserving chain; the caller logs the unsupported material.
pub fn prepare_with_authored(
    pixels: &[u8],
    slots: &[(
        crate::texture_atlas::Slot,
        u8,
        Option<Vec<image::RgbaImage>>,
    )],
    side: usize,
) -> Vec<Tile> {
    let mut tiles = Vec::new();
    for (slot, alpha_ref, authored) in slots {
        if let Some(chain) = authored {
            for (index, img) in chain.iter().enumerate() {
                let level = index as u32 + 1;
                tiles.push(Tile {
                    level,
                    x: (slot.x >> level) as u32,
                    y: (slot.y >> level) as u32,
                    width: img.width(),
                    height: img.height(),
                    pixels: img.as_raw().clone(),
                });
            }
        } else {
            tiles.extend(prepare(pixels, &[(*slot, *alpha_ref)], side));
        }
    }
    tiles
}

pub fn upload(
    queue: &wgpu::Queue,
    atlas: &wgpu::Texture,
    pixels: &[u8],
    tiles: &[Tile],
    side: usize,
) {
    if pixels.is_empty() {
        return;
    }
    let rows = (pixels.len() / (side * 4)).clamp(1, side);
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: atlas,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels[..rows * side * 4],
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some((side * 4) as u32),
            rows_per_image: Some(rows as u32),
        },
        wgpu::Extent3d {
            width: side as u32,
            height: rows as u32,
            depth_or_array_layers: 1,
        },
    );
    for t in tiles {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: atlas,
                mip_level: t.level,
                origin: wgpu::Origin3d {
                    x: t.x,
                    y: t.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &t.pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(t.width * 4),
                rows_per_image: Some(t.height),
            },
            wgpu::Extent3d {
                width: t.width,
                height: t.height,
                depth_or_array_layers: 1,
            },
        );
    }
}
