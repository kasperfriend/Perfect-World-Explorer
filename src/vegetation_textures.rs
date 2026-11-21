//! Authored vegetation DDS mip retention shared by the tree and grass loaders.
//!
//! Level 0 is still decoded by the existing `image` path so the approved default
//! world path is byte-for-byte unchanged. This helper decodes only the *lower*
//! authored levels for the opt-in P4.07 experiment; it never regenerates a chain.

/// Decode authored mip levels 1..N from a vegetation DDS. The returned vector is
/// empty only when the source has no lower levels; parse/decode failures are
/// returned to the caller so they can be logged rather than silently replaced.
pub fn authored_lower_mips(bytes: &[u8]) -> Result<Vec<image::RgbaImage>, String> {
    let dds = crate::model_dds::Dds::parse(bytes)?;
    let fourcc = match dds.format {
        crate::model_dds::Format::Dxt1 => 0x3154_5844u32,
        crate::model_dds::Format::Dxt3 => 0x3354_5844u32,
        crate::model_dds::Format::Dxt5 => 0x3554_5844u32,
        // This helper only rewrites compressed surfaces, because
        // `crate::dxt::decode_level` is the only decoder it has. An
        // uncompressed source has no authored chain to carry into an atlas, so
        // there is nothing for the experiment to preserve.
        crate::model_dds::Format::Bc5
        | crate::model_dds::Format::Bc7
        | crate::model_dds::Format::Uncompressed(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(dds.mips.len().saturating_sub(1));
    for mip in dds.mips.iter().skip(1) {
        let (level, used) = crate::dxt::decode_level(fourcc, mip.bytes, mip.width, mip.height)?;
        if used != mip.bytes.len() {
            return Err(format!(
                "authored vegetation mip {}x{} left {} unused bytes",
                mip.width,
                mip.height,
                mip.bytes.len() - used
            ));
        }
        let img = image::RgbaImage::from_raw(level.width, level.height, level.rgba)
            .ok_or("authored vegetation mip dimensions overflow")?;
        out.push(img);
    }
    Ok(out)
}

/// True when `mips` can fill atlas levels 1..`levels` for a `width`x`height`
/// level-0 slot without resampling. The authored chain must already have the
/// exact halved dimensions; a rectangular source packed into a square slot is
/// rejected instead of being stretched into a different mip chain.
pub fn chain_fits(mips: &[image::RgbaImage], width: u32, height: u32, levels: u32) -> bool {
    if mips.len() < levels as usize {
        return false;
    }
    for level in 1..=levels {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        let img = &mips[(level - 1) as usize];
        if img.width() != w || img.height() != h {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 4]))
    }

    #[test]
    fn chain_fit_requires_exact_halved_dimensions() {
        let mips = vec![
            img(64, 32),
            img(32, 16),
            img(16, 8),
            img(8, 4),
            img(4, 2),
            img(2, 1),
            img(1, 1),
        ];
        assert!(chain_fits(&mips, 128, 64, 7));
        assert!(!chain_fits(&mips, 128, 128, 7));
        assert!(!chain_fits(&mips[..6], 128, 64, 7));
        let mut bad = mips.clone();
        bad[2] = img(16, 16);
        assert!(!chain_fits(&bad, 128, 64, 7));
    }
}
