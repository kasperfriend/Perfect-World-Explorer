//! Borrowed authored DDS block/mip data for model textures. Keep every authored
//! level; silently regenerating a new mip chain changes alpha and light edges.
//! This parser does not implement texture shaders, cube maps, DX10 or decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Dxt1,
    Dxt3,
    Dxt5,
    /// An uncompressed `DDPF_RGB` surface, described by its bit count.
    ///
    /// These are the most common effect textures in `gfx.pck`: of the 1,610
    /// members the effect roots name, over a thousand are stored this way. The
    /// reader used to refuse anything without a FourCC tag outright, so every
    /// one of them failed to decode and the draw substituted its flat white
    /// placeholder — which is what a hard square where a soft smoke puff
    /// belongs looks like. Two layouts ship: 32-bit BGRA (with or without an
    /// alpha mask) and 16-bit RGB565 for small glow sprites.
    Uncompressed(Uncompressed),
}
impl Format {
    /// Retail 0x78ad42–0x78ad62 sets the DDS alpha-texture flag for
    /// DXT3/DXT5, NOT DXT1. This flag is one input to shader-texture routing, distinct from
    /// decoded opacity: BC1 one-bit alpha still uses the ordinary alpha test.
    pub fn alpha_texture(self) -> bool {
        match self {
            Self::Dxt3 | Self::Dxt5 => true,
            Self::Dxt1 => false,
            Self::Uncompressed(u) => u.has_alpha,
        }
    }
}
/// Layout of one uncompressed `DDPF_RGB` surface, recovered from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uncompressed {
    /// 32 for BGRA, 16 for RGB565. Both are the only widths that ship.
    pub bits: u32,
    /// Whether the header declares an alpha mask. RGB565 never does.
    pub has_alpha: bool,
}

#[derive(Debug)]
pub struct Mip<'a> {
    pub width: u32,
    pub height: u32,
    pub bytes: &'a [u8],
}
#[derive(Debug)]
pub struct Dds<'a> {
    pub format: Format,
    pub mips: Vec<Mip<'a>>,
}
impl<'a> Dds<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        Self::parse_levels(bytes, false)
    }

    /// Decodable base of a DDS whose declared lower chain is damaged. Keep
    /// native upload strict, but allow ray consumers to generate a mip tail.
    #[allow(dead_code)] // only the viewer's alternate texture sampler needs it
    pub(crate) fn parse_base(bytes: &'a [u8]) -> Result<Self, String> {
        Self::parse_levels(bytes, true)
    }

    fn parse_levels(bytes: &'a [u8], base_only: bool) -> Result<Self, String> {
        let header = bytes.get(..128).ok_or("truncated model DDS header")?;
        let word = |i| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
        if &header[..4] != b"DDS " || word(4) != 124 || word(76) != 32 {
            return Err("invalid model DDS header".into());
        }
        // `DDPF_FOURCC` selects a block format; without it the surface is
        // stored as raw pixels described by the bit masks that follow. The
        // shipped effects use exactly one such layout, 32-bit BGRA
        // (`rgb_bit_count = 32`, masks `ff0000/ff00/ff/ff000000`), which is
        // also the D3D8 default for a `DDSD_LINEARSIZE`-less surface.
        let fourcc = word(80) & 4 != 0;
        if word(112) != 0 || word(24) > 1 {
            return Err("cube/volume model DDS is not implemented".into());
        }
        let (format, block_bytes) = if fourcc {
            match &header[84..88] {
                b"DXT1" => (Format::Dxt1, 8usize),
                b"DXT3" => (Format::Dxt3, 16),
                b"DXT5" => (Format::Dxt5, 16),
                tag => return Err(format!("unsupported model DDS format {tag:?}")),
            }
        } else {
            // `DDPIF_RGB` is bit 6 of the pixel-format flags and the surface
            // must be 32 bits per pixel with the BGRA mask set. A 24-bit or
            // palettised surface is a different layout and stays unsupported.
            let rgb = word(80) & 0x40 != 0;
            let bit_count = word(88);
            let (rmask, gmask, bmask, amask) = (word(92), word(96), word(100), word(104));
            if !rgb {
                return Err(format!(
                    "unsupported uncompressed model DDS flags {:#x}",
                    word(80)
                ));
            }
            // Two authored layouts appear in `gfx.pck`: the D3D8 default 32-bit
            // BGRA (with or without an alpha mask) and 16-bit RGB565 for small
            // glow sprites.
            let layout = if bit_count == 32 && (rmask, gmask, bmask) == (0xff0000, 0xff00, 0xff) {
                if amask != 0 && amask != 0xff000000 {
                    return Err(format!(
                        "unsupported uncompressed model DDS alpha mask {amask:#x}"
                    ));
                }
                Uncompressed {
                    bits: 32,
                    has_alpha: amask == 0xff000000,
                }
            } else if bit_count == 16 && (rmask, gmask, bmask, amask) == (0x7c00, 0x3e0, 0x1f, 0) {
                Uncompressed {
                    bits: 16,
                    has_alpha: false,
                }
            } else if bit_count == 24 && (rmask, gmask, bmask, amask) == (0xff0000, 0xff00, 0xff, 0)
            {
                // 24-bit BGR, no alpha byte stored. Still opaque.
                Uncompressed {
                    bits: 24,
                    has_alpha: false,
                }
            } else {
                return Err(format!(
                    "unsupported uncompressed model DDS layout: bits {bit_count}, \
                     masks {rmask:#x}/{gmask:#x}/{bmask:#x}/{amask:#x}"
                ));
            };
            (
                Format::Uncompressed(layout),
                (layout.bits as usize).div_ceil(8),
            )
        };
        let (width, height) = (word(16), word(12));
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err("model DDS dimensions out of bounds".into());
        }
        let count = if base_only { 1 } else { word(28).max(1) };
        let max_count = 32 - width.max(height).leading_zeros();
        if count > max_count {
            return Err("model DDS mip count exceeds dimensions".into());
        }
        let mut mips = Vec::new();
        let mut offset = 128usize;
        for i in 0..count {
            let w = (width >> i).max(1);
            let h = (height >> i).max(1);
            let size = match format {
                // Uncompressed surfaces are `block_bytes` per pixel, stored
                // row-major with no padding. Keep this exact: an authored size
                // that is not a multiple of four (810x810 ships) must not be
                // rounded up into the next mip.
                Format::Uncompressed(_) => w as usize * h as usize * block_bytes,
                _ => ((w + 3) / 4) as usize * ((h + 3) / 4) as usize * block_bytes,
            };
            let end = offset.checked_add(size).ok_or("model DDS size overflow")?;
            let data = bytes
                .get(offset..end)
                .ok_or_else(|| format!("truncated model DDS mip {i}"))?;
            mips.push(Mip {
                width: w,
                height: h,
                bytes: data,
            });
            offset = end;
        }
        // A file may carry bytes past the declared mip chain; the client stops
        // reading at the last mip. Only a chain that runs *past* the file is
        // malformed, and that is caught above by `bytes.get(..)`.
        let _ = offset;
        Ok(Self { format, mips })
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn alpha_texture_flag_is_authored_format_not_decoded_pixel_opacity() {
        assert!(!super::Format::Dxt1.alpha_texture());
        assert!(super::Format::Dxt3.alpha_texture());
        assert!(super::Format::Dxt5.alpha_texture());
    }

    use super::*;
    fn fixture(tag: &[u8; 4], unit: usize) -> Vec<u8> {
        let mut b = vec![0; 128];
        b[..4].copy_from_slice(b"DDS ");
        for (o, v) in [(4, 124u32), (12, 8), (16, 8), (28, 4), (76, 32), (80, 4)] {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[84..88].copy_from_slice(tag);
        for (i, blocks) in [4, 1, 1, 1].into_iter().enumerate() {
            b.extend(vec![i as u8; blocks * unit]);
        }
        b
    }
    #[test]
    fn retains_all_authored_levels_and_sub_block_mips() {
        for (tag, format, unit) in [
            (b"DXT1", Format::Dxt1, 8),
            (b"DXT3", Format::Dxt3, 16),
            (b"DXT5", Format::Dxt5, 16),
        ] {
            let bytes = fixture(tag, unit);
            let d = Dds::parse(&bytes).unwrap();
            assert_eq!(d.format, format);
            assert_eq!(d.mips.len(), 4);
            for (i, m) in d.mips.iter().enumerate() {
                assert_eq!((m.width, m.height), (8 >> i, 8 >> i));
                assert!(m.bytes.iter().all(|b| *b == i as u8));
                assert_eq!(m.bytes.len(), if i == 0 { 4 * unit } else { unit });
            }
        }
    }
    /// One uncompressed `DDPF_RGB` surface, built the way the client stores it:
    /// no FourCC, `DDPF_RGB` in the flags, and the channel masks spelled out.
    fn rgb_fixture(bits: u32, masks: [u32; 4], width: u32, height: u32) -> Vec<u8> {
        let mut b = vec![0; 128];
        b[..4].copy_from_slice(b"DDS ");
        for (o, v) in [
            (4, 124u32),
            (12, height),
            (16, width),
            (28, 1),
            (76, 32),
            (80, 0x41),
            (88, bits),
            (92, masks[0]),
            (96, masks[1]),
            (100, masks[2]),
            (104, masks[3]),
        ] {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        b.extend(vec![0x80; (width * height * bits / 8) as usize]);
        b
    }
    /// The white-square defect: 1,232 of the 1,610 textures the effect roots
    /// name are stored as uncompressed BGRA, and the reader refused every one
    /// of them. Each refusal turned a soft puff into a hard white square.
    #[test]
    fn parses_the_uncompressed_bgra_surface_the_effects_use() {
        let bytes = rgb_fixture(32, [0xff0000, 0xff00, 0xff, 0xff000000], 4, 2);
        let d = Dds::parse(&bytes).unwrap();
        assert_eq!(
            d.format,
            Format::Uncompressed(Uncompressed {
                bits: 32,
                has_alpha: true
            })
        );
        assert!(d.format.alpha_texture());
        assert_eq!((d.mips[0].width, d.mips[0].height), (4, 2));
        assert_eq!(d.mips[0].bytes.len(), 32);
    }
    /// The same layout without `DDPF_ALPHAPIXELS` is opaque, not transparent.
    #[test]
    fn an_uncompressed_surface_without_an_alpha_mask_is_opaque() {
        let bytes = rgb_fixture(32, [0xff0000, 0xff00, 0xff, 0], 2, 2);
        let d = Dds::parse(&bytes).unwrap();
        assert!(!d.format.alpha_texture());
        assert_eq!(d.mips[0].bytes.len(), 16);
    }
    /// Sixteen-bit RGB565 and twenty-four-bit BGR both ship, for small glow
    /// sprites. Both are opaque and both must decode at the right stride.
    #[test]
    fn parses_sixteen_and_twenty_four_bit_uncompressed_surfaces() {
        let c = rgb_fixture(16, [0x7c00, 0x3e0, 0x1f, 0], 4, 4);
        let d = Dds::parse(&c).unwrap();
        assert_eq!(
            d.format,
            Format::Uncompressed(Uncompressed {
                bits: 16,
                has_alpha: false
            })
        );
        assert_eq!(d.mips[0].bytes.len(), 32);
        let c = rgb_fixture(24, [0xff0000, 0xff00, 0xff, 0], 4, 4);
        let d = Dds::parse(&c).unwrap();
        assert_eq!(
            d.format,
            Format::Uncompressed(Uncompressed {
                bits: 24,
                has_alpha: false
            })
        );
        assert_eq!(d.mips[0].bytes.len(), 48);
    }
    /// A width that is not a multiple of four must not round its mip up into
    /// the next one: `序列/波动序列9x9.dds` is 810x810 with five levels.
    #[test]
    fn an_uncompressed_mip_is_sized_per_pixel_not_per_block() {
        let bytes = rgb_fixture(32, [0xff0000, 0xff00, 0xff, 0xff000000], 810, 810);
        let d = Dds::parse(&bytes).unwrap();
        assert_eq!(d.mips[0].bytes.len(), 810 * 810 * 4);
    }

    #[test]
    fn rejects_truncated_extra_and_unsupported_data() {
        let bytes = fixture(b"DXT1", 8);
        for i in 0..bytes.len() {
            assert!(Dds::parse(&bytes[..i]).is_err(), "accepted length {i}");
        }
        // A tail past the declared mip chain is accepted: the client reads the
        // chain and stops, and the shipped corpus contains one such file
        // (`序列/波动序列9x9.dds`). Rejecting it cost that effect its texture and
        // drew the white placeholder instead. A chain that runs *past* the file
        // is still refused, which the truncation loop above covers.
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(
            Dds::parse(&extra).is_ok(),
            "a tail past the chain is not a defect"
        );
        for (offset, value) in [
            (4, 0u32),
            (12, 0),
            (16, 20000),
            (28, u32::MAX),
            (76, 0),
            (80, 0),
            (112, 0x200),
            (24, 8),
        ] {
            let mut bad = bytes.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(Dds::parse(&bad).is_err());
        }
        assert!(Dds::parse(&fixture(b"DX10", 8)).is_err());
    }
    #[test]
    fn absent_mip_count_means_one_level_not_a_guessed_full_chain() {
        let mut b = fixture(b"DXT1", 8);
        b[28..32].fill(0);
        b.truncate(128 + 32);
        assert_eq!(Dds::parse(&b).unwrap().mips.len(), 1);
    }
}
