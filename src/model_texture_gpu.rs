//! Upload authored model textures without applying an sRGB sampling conversion.
//! DDS keeps every authored mip; ordinary images such as TGA upload their one
//! decoded RGBA level. Material multiplication still operates in encoded color
//! space. This is a texture stage, not a complete model material shader.
use crate::model_dds::{Dds, Format};
use wgpu::util::DeviceExt;
#[derive(Clone, Copy)]
pub enum UploadMode {
    Auto,
    Decoded,
}

fn append_mip(
    bytes: &[u8],
    format: Format,
    mip: &crate::model_dds::Mip<'_>,
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
    let width = (mip.width + 3) / 4 * 4;
    let height = (mip.height + 3) / 4 * 4;
    let mut single = Vec::with_capacity(128 + mip.bytes.len());
    single.extend_from_slice(&bytes[..128]);
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

fn decode_rgba(bytes: &[u8]) -> Result<(image::RgbaImage, bool), String> {
    let image = match image::load_from_memory(bytes) {
        Ok(image) => image,
        Err(generic) => {
            // TGA has no magic bytes, so image's generic format sniffing cannot
            // recognize valid client textures. Try the explicitly supported
            // decoder before rejecting the model part that references one.
            image::load_from_memory_with_format(bytes, image::ImageFormat::Tga)
                .map_err(|tga| format!("model image decode: {generic}; model TGA decode: {tga}"))?
        }
    };
    let alpha_texture = image.color().has_alpha();
    Ok((image.into_rgba8(), alpha_texture))
}

fn upload_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
) -> Result<(wgpu::Texture, bool), String> {
    let (image, alpha_texture) = decode_rgba(bytes)?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err("model image has zero dimensions".into());
    }
    if width.max(height) > device.limits().max_texture_dimension_2d {
        return Err("model image exceeds device texture dimensions".into());
    }
    let data = image.into_raw();
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("authored model image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::MipMajor,
        &data,
    );
    Ok((texture, alpha_texture))
}

/// Upload one model texture and retain the channel metadata used by native
/// model queue classification. DDS keeps its authored DXT alpha flag; an
/// ordinary image reports whether its decoded source has an alpha channel.
pub fn upload_with_alpha(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
    mode: UploadMode,
) -> Result<(wgpu::Texture, bool), String> {
    if bytes.starts_with(b"DDS ") {
        upload_dds(device, queue, bytes, mode)
    } else {
        upload_image(device, queue, bytes)
    }
}

pub fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
    mode: UploadMode,
) -> Result<wgpu::Texture, String> {
    upload_with_alpha(device, queue, bytes, mode).map(|(texture, _)| texture)
}

fn upload_dds(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
    mode: UploadMode,
) -> Result<(wgpu::Texture, bool), String> {
    let d = Dds::parse(bytes)?;
    let first = &d.mips[0];
    if first.width.max(first.height) > device.limits().max_texture_dimension_2d {
        return Err("model DDS exceeds device texture dimensions".into());
    }
    // `native` means "upload the stored bytes untouched", which is only true for
    // block-compressed surfaces. An uncompressed BGRA surface is a plain texel
    // array: it must go through the channel swap below, not straight to BC.
    let native = matches!(mode, UploadMode::Auto)
        && !matches!(d.format, Format::Uncompressed(_))
        && device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        && first.width % 4 == 0
        && first.height % 4 == 0;
    let format = if native {
        match d.format {
            Format::Dxt1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            Format::Dxt3 => wgpu::TextureFormat::Bc2RgbaUnorm,
            Format::Dxt5 => wgpu::TextureFormat::Bc3RgbaUnorm,
            // Unreachable: `native` excludes uncompressed surfaces. Spelled out
            // so the match stays exhaustive if that gate is ever loosened.
            Format::Uncompressed(_) => wgpu::TextureFormat::Rgba8Unorm,
        }
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    };
    let decoded;
    let data = if native {
        &bytes[128..]
    } else {
        let total: usize = d
            .mips
            .iter()
            .map(|m| (m.width as usize) * (m.height as usize) * 4)
            .sum();
        let mut out = Vec::with_capacity(total);
        for m in &d.mips {
            append_mip(bytes, d.format, m, &mut out)?;
        }
        decoded = out;
        &decoded
    };
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("authored model DDS mip chain"),
            size: wgpu::Extent3d {
                width: first.width,
                height: first.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: d.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::MipMajor,
        data,
    );
    Ok((texture, d.format.alpha_texture()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tga(bits: u8, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 18];
        bytes[2] = 2; // uncompressed true-color image
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
        bytes[16] = bits;
        bytes[17] = if bits == 32 { 8 } else { 0 };
        bytes.extend_from_slice(pixels);
        bytes
    }

    #[test]
    fn tga_decodes_as_rgba_without_a_dds_header() {
        let mut with_footer = tga(32, &[3, 2, 1, 4]);
        with_footer.extend_from_slice(b"\0\0\0\0\0\0\0\0TRUEVISION-XFILE.\0");
        let (image, alpha) = decode_rgba(&with_footer).unwrap();
        assert!(alpha);
        assert_eq!(image.dimensions(), (1, 1));
        assert_eq!(image.into_raw(), [1, 2, 3, 4]);

        let (image, alpha) = decode_rgba(&tga(24, &[3, 2, 1])).unwrap();
        assert!(!alpha);
        assert_eq!(image.into_raw(), [1, 2, 3, 255]);
    }

    #[test]
    fn tiny_bc1_alpha_mip_keeps_its_authored_transparency() {
        let mut bytes = vec![0; 128];
        bytes[..4].copy_from_slice(b"DDS ");
        for (o, v) in [
            (4, 124u32),
            (8, 0x81007),
            (12, 1),
            (16, 1),
            (20, 8),
            (76, 32),
            (80, 4),
            (108, 0x1000),
        ] {
            bytes[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        bytes[84..88].copy_from_slice(b"DXT1");
        bytes.extend_from_slice(&[0, 0, 0, 248, 255, 255, 255, 255]);
        let d = Dds::parse(&bytes).unwrap();
        let mut rgba = Vec::new();
        append_mip(&bytes, d.format, &d.mips[0], &mut rgba).unwrap();
        assert_eq!(rgba.len(), 4);
        assert_eq!(rgba[3], 0);
    }
}
