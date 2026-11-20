//! The client's own loading-screen artwork.
//!
//! Retail Perfect World keeps its loading screens in `surfaces.pck` at the
//! archive root as loose JPEGs next to `background.jpg` - the picture the client
//! shows while a map loads. This module finds those entries in the user's own
//! archive, picks one at random for a world load, and decodes it for a
//! full-window fill.
//!
//! Two rules keep this honest:
//!
//! * The archive is the *only* source. There is no embedded fallback picture; a
//!   client without `surfaces.pck` (or without root JPGs) reports why the
//!   artwork is unavailable and the caller keeps its plain background.
//! * This is transport and selection only. The archive reader (`pck`), the DDS
//!   path (`image` plus `dxt`) and the JPEG decoder are the same ones the rest
//!   of the viewer uses. The loading window does not parse PCK itself.
//!
//! The module is platform-neutral so the lookup, the random choice and the
//! decoding are unit-testable on any host and reusable by the Windows GDI
//! loading window and by a command-line fixture writer.

use crate::pck;
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
};

/// The archive the client keeps its menu, map and loading-screen artwork in.
pub const ARCHIVE_NAME: &str = "surfaces.pck";

/// File extensions the client uses for full-screen artwork in `surfaces.pck`.
/// The census of the retail archive counted 39 root `.jpg` entries plus BMP and
/// TGA surfaces; DDS is included because a modified client may ship one.
const PICTURE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "bmp", "dds"];

/// A decode that produced a picture larger than this is refused instead of
/// allocated: a 16,384 x 16,384 RGBA image is 1 GiB, far past any loading
/// artwork the client ships and enough to take the process down on a
/// corrupted or hostile archive.
const MAX_PIXELS: u64 = 8192 * 8192;

/// One archived loading-screen picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Normalized archive path, for example `surfaces/loading01.jpg`.
    pub path: String,
    /// Uncompressed payload size in bytes, used only for ordering and reports.
    pub size: u32,
}

/// True when a directory entry is a candidate loading-screen picture.
///
/// The client puts these at the archive root (`surfaces/<name>.jpg`), beside
/// `background.jpg`; art in subfolders is UI, map, minimap or icon material and
/// is deliberately excluded, because a random icon is not a loading screen.
fn is_root_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase().replace('\\', "/");
    let relative = lower.trim_start_matches("surfaces/");
    if relative.contains('/') {
        return false;
    }
    let (stem, extension) = match relative.rsplit_once('.') {
        Some(parts) => parts,
        None => return false,
    };
    !stem.is_empty()
        && PICTURE_EXTENSIONS
            .iter()
            .any(|candidate| extension == *candidate)
}

/// Choose one picture at random from a list, given a random word.
///
/// Split out from the caller so the choice is testable and so an empty list can
/// never index out of bounds. The index is `word % len`, and the caller passes an
/// entropy value derived from the process, clock and address space, so a world
/// loaded twice does not necessarily show the same picture.
pub fn pick(pictures: &[Picture], random_word: u64) -> Option<&Picture> {
    if pictures.is_empty() {
        return None;
    }
    let index = (random_word % pictures.len() as u64) as usize;
    pictures.get(index)
}

/// Every root-level picture in `surfaces.pck`, sorted by path.
///
/// The archive is optional at runtime: a client whose `surfaces.pck` is missing
/// or unreadable returns the error so the caller can name the reason instead of
/// showing a blank or an invented image.
pub fn list(element: &Path) -> Result<Vec<Picture>, String> {
    let path = element.join(ARCHIVE_NAME);
    let mut file = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let entries =
        pck::directory(&mut file).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut pictures: Vec<Picture> = entries
        .into_iter()
        .filter(|entry| is_root_picture(&entry.name))
        .map(|entry| Picture {
            path: entry.name.to_ascii_lowercase().replace('\\', "/"),
            size: entry.size,
        })
        .collect();
    pictures.sort_by(|a, b| a.path.cmp(&b.path));
    pictures.dedup_by(|a, b| a.path == b.path);
    Ok(pictures)
}

/// Environment override used by the fixture writer and by a user who wants a
/// specific picture instead of a random one. A name without a directory is
/// resolved against the archive root, so `loading01.jpg` and
/// `surfaces/loading01.jpg` both work.
pub fn forced_picture_name() -> Option<String> {
    std::env::var("PWE_LOADING_SCREEN")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Pick the picture the loading window should show.
///
/// An explicit `PWE_LOADING_SCREEN` name wins when the archive actually holds it;
/// the caller logs when it does not and falls back to the random choice, because
/// a typo in an environment variable must not blank the loading screen.
pub fn choose(element: &Path, random_word: u64) -> Result<Picture, String> {
    let pictures = list(element)?;
    if pictures.is_empty() {
        return Err(format!(
            "{} has no root-level loading-screen pictures",
            element.join(ARCHIVE_NAME).display()
        ));
    }
    if let Some(name) = forced_picture_name() {
        let normalized = name.to_ascii_lowercase().replace('\\', "/");
        let normalized = if normalized.starts_with("surfaces/") {
            normalized
        } else {
            format!("surfaces/{normalized}")
        };
        if let Some(found) = pictures
            .iter()
            .find(|picture| picture.path.eq_ignore_ascii_case(&normalized))
        {
            return Ok(found.clone());
        }
        eprintln!(
            "PWE_LOADING_SCREEN={name} is not a loading-screen picture in {}; choosing another",
            element.join(ARCHIVE_NAME).display()
        );
    }
    pick(&pictures, random_word)
        .cloned()
        .ok_or_else(|| "no loading-screen picture available".to_string())
}

/// Read and decode one picture to tightly packed RGBA8.
///
/// Returns the image and its pixel dimensions. Every failure names the archive
/// entry so a broken asset is identifiable from the log alone.
pub fn decode(element: &Path, picture: &Picture) -> Result<image::RgbaImage, String> {
    let path = element.join(ARCHIVE_NAME);
    let mut file = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let entries =
        pck::directory(&mut file).map_err(|error| format!("{}: {error}", path.display()))?;
    let by_name: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.name.to_ascii_lowercase().replace('\\', "/"), index))
        .collect();
    let index = by_name
        .get(&picture.path)
        .copied()
        .ok_or_else(|| format!("{} no longer holds {}", path.display(), picture.path))?;
    let bytes = pck::read_entry(&mut file, &entries[index])
        .map_err(|error| format!("{}: {error}", picture.path))?;
    decode_rgba(&bytes).map_err(|error| format!("{}: {error}", picture.path))
}

/// Decode an image payload the client stores as JPEG, BMP, TGA or DDS.
///
/// TGA has no magic bytes, so a payload the generic sniffer cannot identify is
/// retried as TGA before the entry is rejected - the same order the model
/// texture path uses.
pub fn decode_rgba(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let image = if bytes.starts_with(b"DDS ") {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Dds)
            .map_err(|error| format!("DDS decode: {error}"))?
    } else {
        match image::load_from_memory(bytes) {
            Ok(image) => image,
            Err(generic) => image::load_from_memory_with_format(bytes, image::ImageFormat::Tga)
                .map_err(|tga| format!("image decode: {generic}; TGA decode: {tga}"))?,
        }
    };
    let (width, height) = (image.width() as u64, image.height() as u64);
    if width == 0 || height == 0 {
        return Err("decoded picture has zero dimensions".into());
    }
    if width * height > MAX_PIXELS {
        return Err(format!(
            "decoded picture is {width}x{height}; larger than the {MAX_PIXELS}-pixel limit"
        ));
    }
    Ok(image.into_rgba8())
}

/// Nearest-neighbour cover fit: scale `source` so it fills `width` x `height`
/// exactly, cropping the overflow on the long axis.
///
/// This is what makes the artwork fill the window instead of sitting in a box:
/// aspect ratio is preserved, so a 4:3 picture in a 16:9 window is cropped top
/// and bottom rather than letterboxed. Nearest-neighbour keeps the blit cheap
/// enough for the loading thread and never introduces a second resampling
/// dependency for a picture that is only on screen while the world loads.
pub fn fill(
    source: &image::RgbaImage,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage, String> {
    if width == 0 || height == 0 {
        return Err("fill target has zero dimensions".into());
    }
    let (source_width, source_height) = source.dimensions();
    if source_width == 0 || source_height == 0 {
        return Err("fill source has zero dimensions".into());
    }
    let scale_x = width as f64 / source_width as f64;
    let scale_y = height as f64 / source_height as f64;
    // Cover: the larger scale fills both axes; the source rect that maps onto
    // the target is the visible crop.
    let scale = scale_x.max(scale_y);
    let crop_width = (width as f64 / scale).min(source_width as f64);
    let crop_height = (height as f64 / scale).min(source_height as f64);
    let left = (source_width as f64 - crop_width) * 0.5;
    let top = (source_height as f64 - crop_height) * 0.5;
    let mut target = image::RgbaImage::new(width, height);
    // `ImageBuffer` derefs to its sample slice; it has no `AsMut<[u8]>` impl, so
    // the write goes through `DerefMut`.
    let target_pixels: &mut [u8] = &mut target;
    let pixels = source.as_raw();
    let stride = source_width as usize * 4;
    for y in 0..height {
        let source_y = ((top + (y as f64 + 0.5) * crop_height / height as f64) as usize)
            .min(source_height as usize - 1);
        let row = source_y * stride;
        for x in 0..width {
            let source_x = ((left + (x as f64 + 0.5) * crop_width / width as f64) as usize)
                .min(source_width as usize - 1);
            let offset = row + source_x * 4;
            let target_offset = (y as usize * width as usize + x as usize) * 4;
            target_pixels[target_offset..target_offset + 4]
                .copy_from_slice(&pixels[offset..offset + 4]);
        }
    }
    Ok(target)
}

/// [`fill`] applied to packed RGBA8 bytes, which is the form the Windows blit
/// and the fixture writer both hold.
pub fn fill_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let expected = source_width as usize * source_height as usize * 4;
    if source.len() != expected {
        return Err(format!(
            "RGBA source is {} bytes, expected {expected} for {source_width}x{source_height}",
            source.len()
        ));
    }
    if width == 0 || height == 0 {
        return Err("fill target has zero dimensions".into());
    }
    if source_width == 0 || source_height == 0 {
        return Err("fill source has zero dimensions".into());
    }
    let scale = (width as f64 / source_width as f64).max(height as f64 / source_height as f64);
    let crop_width = (width as f64 / scale).min(source_width as f64);
    let crop_height = (height as f64 / scale).min(source_height as f64);
    let left = (source_width as f64 - crop_width) * 0.5;
    let top = (source_height as f64 - crop_height) * 0.5;
    let mut target = vec![0u8; width as usize * height as usize * 4];
    let stride = source_width as usize * 4;
    for y in 0..height {
        let source_y = ((top + (y as f64 + 0.5) * crop_height / height as f64) as usize)
            .min(source_height as usize - 1);
        let row = source_y * stride;
        let target_row = y as usize * width as usize * 4;
        for x in 0..width {
            let source_x = ((left + (x as f64 + 0.5) * crop_width / width as f64) as usize)
                .min(source_width as usize - 1);
            let offset = row + source_x * 4;
            let target_offset = target_row + x as usize * 4;
            target[target_offset..target_offset + 4].copy_from_slice(&source[offset..offset + 4]);
        }
    }
    Ok(target)
}

/// Composite a dark, bottom-heavy gradient behind the loading overlay.
///
/// GDI has no alpha blending, so the whole strip is blended against the artwork
/// here, once, when the picture is decoded. The overlay keeps the DirectStorage
/// status, the throughput graph and the loading bar legible over artwork of any
/// brightness without hiding the picture: the strip is fully transparent at its
/// top edge and reaches `max_alpha` at the bottom, blending toward black.
///
/// `source` is RGBA8, `width` x `height`; `strip_height` rows at the bottom are
/// affected, with the top `fade_height` of them fading in. Returns packed RGBA8.
pub fn overlay_strip(
    source: &[u8],
    width: u32,
    height: u32,
    strip_height: u32,
    fade_height: u32,
    max_alpha: f32,
) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 4;
    if source.len() != expected {
        return Err(format!(
            "RGBA source is {} bytes, expected {expected} for {width}x{height}",
            source.len()
        ));
    }
    let max_alpha = if max_alpha.is_finite() {
        max_alpha.clamp(0., 1.)
    } else {
        0.
    };
    let strip_height = strip_height.min(height);
    let fade_height = fade_height.min(strip_height);
    let strip_top = height - strip_height;
    let fade_top = strip_top + fade_height;
    let mut result = source.to_vec();
    for y in 0..height {
        if y < strip_top {
            continue;
        }
        let alpha = if y < fade_top && fade_height > 0 {
            max_alpha * (y - strip_top) as f32 / fade_height as f32
        } else {
            max_alpha
        };
        let keep = 1.0 - alpha;
        let row = y as usize * width as usize * 4;
        for x in 0..width as usize {
            let offset = row + x * 4;
            for channel in 0..3 {
                result[offset + channel] = (result[offset + channel] as f32 * keep) as u8;
            }
        }
    }
    Ok(result)
}

/// BGRA8 for a Windows `BITMAPINFO`-style DIB blit with 32-bit pixels.
///
/// GDI's `StretchDIBits` reads DIB rows bottom-up unless the header gives a
/// negative height; the caller passes `top_down = true` and gets rows in image
/// order, which is the same convention the launcher's own artwork uses.
pub fn to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    bgra
}

/// Best-effort, bounded entropy for the random choice.
///
/// A loading screen does not need cryptographic randomness, but it should not
/// repeat every launch either. Mixing the process id, the wall clock and the
/// address of a stack local gives a different word for practically every run
/// without pulling in a random-number dependency.
pub fn random_word() -> u64 {
    let mut word = std::process::id() as u64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    word = word
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(now.as_nanos() as u64);
    word = word
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(now.subsec_nanos() as u64);
    // A boxed local's address is stable and distinct per run; taking the
    // address of a plain `let` would be a reference the optimiser may fold, and
    // it reads as if the address mattered more than it does.
    let local = Box::new(0u8);
    word = word
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add((&*local) as *const u8 as usize as u64);
    // A final mix so the low bits `pick` uses are not dominated by the
    // multiplication chain's low-bit structure.
    word ^= word >> 29;
    word = word.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    word ^ (word >> 32)
}

/// Write a decoded, window-fitted picture as a PNG fixture.
///
/// This is the headless proof that the shipped picture path works on a real
/// client folder: the binary `loading-screen-check` calls this, and the image can
/// be inspected without a Windows desktop.
pub fn write_fixture(picture: &image::RgbaImage, path: &Path) -> Result<(), String> {
    let file = File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        picture.width(),
        picture.height(),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer
        .write_image_data(picture.as_raw())
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())
}

/// Resolve the `element` folder from a client root or an `element` path.
///
/// The launcher returns the element folder, but a command-line argument may be
/// the client root, the maps folder or a single map directory, and the loading
/// window is started before the map resolves. This lets every entry point find
/// the archive without a world load.
pub fn element_folder(path: &Path) -> PathBuf {
    let element = path.join("element");
    if element.join(ARCHIVE_NAME).is_file() {
        return element;
    }
    if path.join(ARCHIVE_NAME).is_file() {
        return path.to_path_buf();
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("maps") {
        if let Some(parent) = path.parent() {
            if parent.join(ARCHIVE_NAME).is_file() {
                return parent.to_path_buf();
            }
        }
    }
    if let Some(parent) = path.parent() {
        if let Some(grandparent) = parent.parent() {
            if grandparent.join(ARCHIVE_NAME).is_file()
                && parent.file_name().map_or(false, |name| name == "maps")
            {
                return grandparent.to_path_buf();
            }
        }
    }
    if path.join("maps").is_dir() {
        return path.to_path_buf();
    }
    // No archive anywhere: return the most likely element folder so the error
    // message names a real candidate rather than the raw argument.
    if path.join("element").is_dir() {
        element
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(path: &str) -> Picture {
        Picture {
            path: path.into(),
            size: 1,
        }
    }

    #[test]
    fn root_pictures_are_loading_screens_and_subfolder_art_is_not() {
        for name in [
            "surfaces/loading01.jpg",
            "surfaces/background.jpg",
            "surfaces/Loading02.JPG",
            "surfaces/载入1.bmp",
            "Surfaces\\loading3.dds",
            "surfaces/logo.jpeg",
        ] {
            assert!(is_root_picture(name), "{name} should be a loading screen");
        }
        for name in [
            "surfaces/iconset/iconlist_ivtrm.dds",
            "surfaces/maps/000.dds",
            "surfaces/minimaps/world/0000.dds",
            "surfaces/loading.jpg.txt",
            "surfaces/.jpg",
            "surfaces/town/loading01.jpg",
            "textures/sky/clear/01.bmp",
        ] {
            assert!(
                !is_root_picture(name),
                "{name} must not be a loading screen"
            );
        }
    }

    #[test]
    fn pick_never_indexes_out_of_bounds_and_uses_the_random_word() {
        assert_eq!(pick(&[], 123), None);
        let pictures = vec![picture("surfaces/a.jpg"), picture("surfaces/b.jpg")];
        assert_eq!(pick(&pictures, 0).unwrap().path, "surfaces/a.jpg");
        assert_eq!(pick(&pictures, 1).unwrap().path, "surfaces/b.jpg");
        assert_eq!(pick(&pictures, 2).unwrap().path, "surfaces/a.jpg");
        assert_eq!(pick(&pictures, u64::MAX).unwrap().path, "surfaces/b.jpg");
    }

    #[test]
    fn random_words_vary_between_calls_and_stay_non_negative() {
        let first = random_word();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = random_word();
        assert_ne!(first, second);
    }

    #[test]
    fn element_folder_accepts_a_root_an_element_folder_and_a_maps_folder() {
        let root = std::env::temp_dir().join(format!("pwe-loading-element-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let element = root.join("element");
        std::fs::create_dir_all(element.join("maps/world")).unwrap();
        std::fs::write(element.join(ARCHIVE_NAME), b"fixture").unwrap();
        assert_eq!(element_folder(&root), element);
        assert_eq!(element_folder(&element), element);
        assert_eq!(element_folder(&element.join("maps")), element);
        assert_eq!(element_folder(&element.join("maps/world")), element);
        std::fs::remove_file(element.join(ARCHIVE_NAME)).unwrap();
        // Without an archive the folder is still picked by its `maps` child so
        // the caller's error names a real candidate.
        assert_eq!(element_folder(&root), element);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn fill_covers_the_target_and_crops_the_long_axis() {
        // A 4x2 picture in a 2x2 target must crop horizontally and keep both
        // source rows, so the result is fully opaque and 2x2.
        let mut source = image::RgbaImage::new(4, 2);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            let value = (x * 10 + y) as u8;
            *pixel = image::Rgba([value, 0, 0, 255]);
        }
        let filled = fill(&source, 2, 2).unwrap();
        assert_eq!(filled.dimensions(), (2, 2));
        // Centre crop of the 4-wide source keeps columns 1 and 2.
        assert_eq!(filled.get_pixel(0, 0).0, [10, 0, 0, 255]);
        assert_eq!(filled.get_pixel(1, 0).0, [20, 0, 0, 255]);
        assert_eq!(filled.get_pixel(0, 1).0, [11, 0, 0, 255]);
    }

    #[test]
    fn fill_rgba_matches_the_image_path_and_rejects_bad_input() {
        let source = vec![9u8; 3 * 2 * 4];
        let bytes = fill_rgba(&source, 3, 2, 2, 4).unwrap();
        assert_eq!(bytes.len(), 2 * 4 * 4);
        assert!(fill_rgba(&source, 3, 2, 0, 4).is_err());
        assert!(fill_rgba(&[0u8; 5], 3, 2, 2, 4).is_err());
        let image = image::RgbaImage::from_raw(3, 2, source).unwrap();
        let filled = fill(&image, 2, 4).unwrap();
        assert_eq!(filled.as_raw(), &bytes);
    }

    #[test]
    fn overlay_strip_leaves_the_picture_alone_above_the_strip() {
        let width = 2u32;
        let height = 10u32;
        let source: Vec<u8> = (0..width * height)
            .flat_map(|_| [200u8, 180, 160, 55])
            .collect();
        let out = overlay_strip(&source, width, height, 4, 2, 1.0).unwrap();
        assert_eq!(out.len(), source.len());
        // Rows above the strip are untouched, alpha included.
        assert_eq!(&out[..6 * 4 * 2], &source[..6 * 4 * 2]);
        // The strip's bottom row is blended to black but keeps its alpha.
        let bottom = (height as usize - 1) * width as usize * 4;
        assert_eq!(&out[bottom..bottom + 4], &[0, 0, 0, 55]);
        // The fade row is partly darkened.
        let fade = 8 * width as usize * 4;
        assert!(out[fade] < source[fade] && out[fade] > 0);
        assert!(overlay_strip(&source[..3], width, height, 4, 2, 1.0).is_err());
    }

    #[test]
    fn overlay_strip_clamps_alpha_and_zero_strip_is_identity() {
        let source = vec![90u8; 4 * 3 * 4];
        assert_eq!(overlay_strip(&source, 4, 3, 0, 0, 5.0).unwrap(), source);
        let out = overlay_strip(&source, 4, 3, 9, 9, f32::NAN).unwrap();
        assert_eq!(out, source);
        let out = overlay_strip(&source, 4, 3, 1, 1, 0.5).unwrap();
        let bottom = 2 * 4 * 4;
        assert_eq!(out[bottom], 45);
    }

    #[test]
    fn bgra_swap_keeps_alpha_and_orders_channels_for_gdi() {
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(to_bgra(&rgba), vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn jpeg_payloads_decode_to_rgba() {
        // A 2x2 JPEG built once here keeps the decode path (the same `image`
        // crate feature the launcher art uses) covered without a client.
        let mut source = image::RgbImage::new(2, 2);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 200) as u8, (y * 200) as u8, 64]);
        }
        let mut jpeg = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut jpeg);
            let mut writer = image::codecs::jpeg::JpegEncoder::new_with_quality(cursor, 90);
            writer.encode_image(&source).unwrap();
        }
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let decoded = decode_rgba(&jpeg).unwrap();
        assert_eq!(decoded.dimensions(), (2, 2));
    }

    #[test]
    fn a_broken_payload_is_an_error_not_a_blank_picture() {
        assert!(decode_rgba(b"not a picture").is_err());
        assert!(decode_rgba(&[]).is_err());
    }
}
