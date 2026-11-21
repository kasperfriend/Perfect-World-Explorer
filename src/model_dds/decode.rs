//! CPU decode of a validated DDS mip, shared by native uploads and ray feeds.
use super::{Dds, Format, Mip};

pub(crate) fn append_mip(
    bytes: &[u8],
    format: Format,
    mip: &Mip<'_>,
    rgba: &mut Vec<u8>,
) -> Result<(), String> {
    // image's block decoder expects dimensions divisible by four. Decode the
    // complete stored blocks, then crop; tiny authored mips are not discarded.
    // An uncompressed surface needs no decoder: the archive stores exactly the
    // bytes a texture wants, so swap the two colour channels and hand it over.
    // Round-tripping it through the block decoder's synthetic header is what
    // used to fail, and a failure here becomes the draw's white placeholder.
    if let Format::Uncompressed(layout) = format {
        if layout.bits == 16 {
            // RGB565: five or six bits per channel, expanded by replicating the
            // high bits into the low ones so full scale is 255 rather than 248.
            for pixel in mip.bytes.chunks_exact(2) {
                let v = u16::from_le_bytes([pixel[0], pixel[1]]);
                let r = ((v >> 11) & 0x1f) as u32;
                let g = ((v >> 5) & 0x3f) as u32;
                let b = (v & 0x1f) as u32;
                rgba.extend_from_slice(&[
                    ((r * 255 + 15) / 31) as u8,
                    ((g * 255 + 31) / 63) as u8,
                    ((b * 255 + 15) / 31) as u8,
                    255,
                ]);
            }
        } else if layout.bits == 24 {
            for pixel in mip.bytes.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        } else {
            for pixel in mip.bytes.chunks_exact(4) {
                let a = if layout.has_alpha { pixel[3] } else { 255 };
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], a]);
            }
        }
        return Ok(());
    }
    if matches!(format, Format::Bc5 | Format::Bc7) {
        let width = mip.width as usize;
        let height = mip.height as usize;
        let blocks_x = width.div_ceil(4);
        let start = rgba.len();
        rgba.resize(start + width * height * 4, 0);
        for (i, block) in mip.bytes.chunks_exact(16).enumerate() {
            let mut decoded = [0u8; 64];
            if format == Format::Bc7 {
                bcdec_rs::bc7(block, &mut decoded, 16);
            } else {
                let mut rg = [0u8; 32];
                bcdec_rs::bc5(block, &mut rg, 8, false);
                for (src, dst) in rg.chunks_exact(2).zip(decoded.chunks_exact_mut(4)) {
                    dst.copy_from_slice(&[src[0], src[1], 0, 255]);
                }
            }
            for (y, row) in decoded.chunks_exact(16).enumerate() {
                let y = i / blocks_x * 4 + y;
                let x = i % blocks_x * 4;
                if y < height {
                    let at = start + (y * width + x) * 4;
                    let n = (width - x).min(4) * 4;
                    rgba[at..at + n].copy_from_slice(&row[..n]);
                }
            }
        }
        return Ok(());
    }
    let width = (mip.width + 3) / 4 * 4;
    let height = (mip.height + 3) / 4 * 4;
    let mut single = Vec::with_capacity(128 + mip.bytes.len());
    single.extend_from_slice(&bytes[..128]);
    // The image decoder accepts the three legacy tags. A validated DX10
    // surface uses the same blocks; substitute the tag in this temporary
    // single-mip header, never in the actual GPU upload.
    single[84..88].copy_from_slice(match format {
        Format::Dxt1 => b"DXT1",
        Format::Dxt3 => b"DXT3",
        Format::Dxt5 => b"DXT5",
        _ => unreachable!("color block format"),
    });
    for (offset, value) in [
        (12, height),
        (16, width),
        (20, mip.bytes.len() as u32),
        (28, 1),
    ] {
        single[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    single.extend_from_slice(mip.bytes);
    let mut image = image::load_from_memory_with_format(&single, image::ImageFormat::Dds)
        .map_err(|e| format!("model DDS mip decode: {e}"))?
        .into_rgba8();
    // Some image decoder versions expose DXT1 as RGB, losing its one-bit alpha.
    // Restore that authored BC1 mode even when the decoded image is RGB-only.
    if format == Format::Dxt1 {
        let blocks_x = width / 4;
        for (block, data) in mip.bytes.chunks_exact(8).enumerate() {
            let c0 = u16::from_le_bytes([data[0], data[1]]);
            let c1 = u16::from_le_bytes([data[2], data[3]]);
            if c0 > c1 {
                continue;
            }
            let selectors = u32::from_le_bytes(data[4..8].try_into().unwrap());
            let x = (block as u32 % blocks_x) * 4;
            let y = (block as u32 / blocks_x) * 4;
            for p in 0..16 {
                if (selectors >> (p * 2)) & 3 == 3 {
                    image.get_pixel_mut(x + p % 4, y + p / 4).0[3] = 0;
                }
            }
        }
    }
    let pixels = image.as_raw();
    if width == mip.width {
        rgba.extend_from_slice(&pixels[..width as usize * mip.height as usize * 4]);
    } else {
        for row in pixels
            .chunks_exact(width as usize * 4)
            .take(mip.height as usize)
        {
            rgba.extend_from_slice(&row[..mip.width as usize * 4]);
        }
    }
    Ok(())
}

/// Base-level fallback for native BMD atlases when the generic image crate
/// does not recognize a DX10/ATI2 source.
pub(crate) fn decode_base(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let dds = Dds::parse_base(bytes)?;
    let mip = &dds.mips[0];
    let mut pixels = Vec::new();
    append_mip(bytes, dds.format, mip, &mut pixels)?;
    image::RgbaImage::from_raw(mip.width, mip.height, pixels)
        .ok_or_else(|| "invalid DDS base extent".into())
}
