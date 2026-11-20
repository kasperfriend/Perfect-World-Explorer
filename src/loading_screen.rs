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
/// TGA surfaces; DDS is included because a modified client may ship one. TGA has
/// no magic bytes, which is why `decode_rgba` retries an unidentified payload as
/// TGA after the generic sniffer.
const PICTURE_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "bmp", "dds", "tga"];

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

/// How many pictures the numbered `载入` family holds per resolution.
///
/// The retail archive ships the same eleven artworks three times, sized for a
/// 1024-, a 1280- and a 16:9 screen. The count is fixed by the client, not read
/// from the archive, because a modified client that adds or drops a picture must
/// not shift which artwork a location resolves to.
const LOADING_PICTURES: u32 = 11;

/// A location the archive has (or is believed to have) its own loading art for.
///
/// The retail archive has no zone-named artwork: every loading screen is one of
/// the eleven numbered `载入<size>-<n>.jpg` pictures, and nothing in
/// `configs.pck` pairs them with a map. `Pic` therefore never selects a picture
/// by map name; it is an explicit, user-facing pin for a client whose archive
/// *does* name its art after a place, plus a stable way to ask for one of the
/// numbered pictures by eye ("the mountain one", "the one with the dragon").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pic {
    /// Picture number inside the numbered family, `1..=`[`LOADING_PICTURES`].
    pub number: u32,
    /// Archive file stem the location was matched on, for the log line.
    pub matched: String,
}

/// A destination a loading screen is being shown for.
///
/// Built before the new world's terrain exists, so this carries only what the
/// catalog already knows: the map folder id and the client's own names for it.
/// `needles` is pre-computed because matching runs once per load and the
/// priorities matter more than the raw strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// Map folder id, e.g. `world`, `a01`.
    pub id: String,
    /// Human-readable name for the log line, e.g. `Archosaur (a01)`.
    pub label: String,
    /// Identifiers a picture name may contain, longest first.
    pub needles: Vec<String>,
}

impl Location {
    /// Build a destination from the catalog entry the Teleport List selected.
    pub fn new(id: &str, display_name: &str, client_name: &str) -> Self {
        let label = if display_name.trim().is_empty() {
            id.to_string()
        } else {
            format!("{} ({id})", display_name.trim())
        };
        let needles = location_needles(id, &[display_name, client_name])
            .into_iter()
            .map(str::to_string)
            .collect();
        Self {
            id: id.to_string(),
            label,
            needles,
        }
    }
}

/// Resolve a location name to a picture by matching the archive's *file names*.
///
/// `needles` are the location identifiers in priority order — map folder
/// (`a01`), client name (`Archosaur`), display name. A picture whose file stem
/// contains one of them as a whitespace/underscore-separated token is a match;
/// the shortest matching needle wins so `a01` cannot be shadowed by `a010`.
/// Matching is ASCII-case-insensitive and token-based, so `b05` does not match
/// `b050` and `world` does not match `underworld`.
///
/// Returns `None` when the archive names nothing after the location, which is
/// the retail case: the caller then falls back to a random picture. That is a
/// real answer, not a failure — nothing is invented to fill it.
pub fn match_location<S: AsRef<str>>(pictures: &[Picture], needles: &[S]) -> Option<Picture> {
    let mut best: Option<(usize, &Picture)> = None;
    for picture in pictures {
        let stem = picture
            .path
            .rsplit('/')
            .next()
            .unwrap_or(picture.path.as_str())
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(picture.path.as_str());
        // Both sides are reduced to a run of lowercase alphanumeric tokens so a
        // name written `Heaven's Tear` matches a stem written `heavens-tear`:
        // the client's own names use spaces and punctuation, while archives
        // replace them with underscores and hyphens.
        let stem_tokens = tokens(stem);
        if stem_tokens.is_empty() {
            continue;
        }
        for needle in needles {
            let needle_tokens = tokens(needle.as_ref());
            if needle_tokens.is_empty() || needle_tokens.len() > stem_tokens.len() {
                continue;
            }
            // A contiguous run of whole tokens counts, so `heavens tear` matches
            // and a short id like `b05` still cannot match `b050`.
            let matched = stem_tokens
                .windows(needle_tokens.len())
                .any(|window| window == needle_tokens.as_slice());
            if !matched {
                continue;
            }
            let length = needle_tokens.iter().map(String::len).sum::<usize>();
            if best.map_or(true, |(best_length, _)| length > best_length) {
                best = Some((length, picture));
            }
        }
    }
    best.map(|(_, picture)| picture.clone())
}

/// Lowercase alphanumeric tokens of a name, with every separator dropped.
///
/// Apostrophes are removed rather than treated as separators, so the client's
/// `Heaven's Tear` produces `heavens`, `tear` instead of also producing a
/// stray `s` token.
fn tokens(name: &str) -> Vec<String> {
    name.to_lowercase()
        .replace(['\'', '\u{2019}'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// The picture number of a numbered-family stem, if it names one.
///
/// `载入16-9-7` and `载入1024-7` both name artwork 7, so a location pinned to one
/// resolution shows the same artwork at every resolution.
fn numbered_stem(stem: &str) -> Option<u32> {
    let (_, number) = stem.rsplit_once('-')?;
    let number = number.parse::<u32>().ok()?;
    (1..=LOADING_PICTURES).contains(&number).then_some(number)
}

/// Prefer the pictures that match the screen's aspect ratio.
///
/// The client ships the same artwork three times; showing the 16:9 set on a
/// 16:9 screen and the 4:3 sets on a 4:3 one means the cover fit crops nothing.
/// Every family is still a candidate, so a client that ships only one of them
/// keeps working.
fn prefer_aspect(pictures: &[Picture], width: u32, height: u32) -> Vec<Picture> {
    let wide = (width as f32) / (height.max(1) as f32) >= 1.5;
    let suffix = if wide {
        "16-9-"
    } else if width >= 1280 {
        "1280-"
    } else {
        "1024-"
    };
    let preferred: Vec<Picture> = pictures
        .iter()
        .filter(|picture| picture.path.contains(suffix))
        .cloned()
        .collect();
    if preferred.is_empty() {
        pictures.to_vec()
    } else {
        preferred
    }
}

/// Choose the picture a location should show.
///
/// Order: an explicit pin, then a name match against the archive, then a random
/// picture from the family that suits the window's aspect ratio. A pin that the
/// archive does not hold is reported and skipped rather than blanking the
/// window, the same rule [`choose`] applies to `PWE_LOADING_SCREEN`.
pub fn choose_for_location<S: AsRef<str>>(
    element: &Path,
    needles: &[S],
    pin: Option<&Pic>,
    width: u32,
    height: u32,
    random_word: u64,
) -> Result<(Picture, &'static str), String> {
    let pictures = list(element)?;
    if pictures.is_empty() {
        return Err(format!(
            "{} has no root-level loading-screen pictures",
            element.join(ARCHIVE_NAME).display()
        ));
    }
    let family = prefer_aspect(&pictures, width, height);
    if let Some(pin) = pin {
        if let Some(found) = family.iter().find(|picture| {
            picture
                .path
                .rsplit('/')
                .next()
                .and_then(|name| name.rsplit_once('.').map(|(stem, _)| stem))
                .and_then(numbered_stem)
                == Some(pin.number)
        }) {
            return Ok((found.clone(), "location pin"));
        }
        // The pinned family may not ship at this aspect ratio; the pin still
        // names an artwork, so look for it in every family before giving up.
        if let Some(found) = pictures.iter().find(|picture| {
            picture
                .path
                .rsplit('/')
                .next()
                .and_then(|name| name.rsplit_once('.').map(|(stem, _)| stem))
                .and_then(numbered_stem)
                == Some(pin.number)
        }) {
            return Ok((found.clone(), "location pin"));
        }
        eprintln!(
            "Loading screen pin {} ({}) is not in {}; choosing another picture",
            pin.number,
            pin.matched,
            element.join(ARCHIVE_NAME).display()
        );
    }
    if let Some(found) =
        match_location(&family, needles).or_else(|| match_location(&pictures, needles))
    {
        return Ok((found, "location name"));
    }
    let picked = pick(&family, random_word)
        .cloned()
        .ok_or_else(|| "no loading-screen picture available".to_string())?;
    Ok((picked, "random"))
}

/// Environment pin that names one numbered artwork, e.g.
/// `PWE_LOADING_PICTURE=7`.
///
/// This is the honest way to pick a loading screen by location on the retail
/// client, whose eleven pictures carry no place names: the user looks at the
/// pictures once, writes down which one each destination should show in their
/// own notes, and pins it per session. [`Pic::matched`] says where the number
/// came from so the log is never a bare integer.
pub fn pinned_picture() -> Option<Pic> {
    let raw = std::env::var("PWE_LOADING_PICTURE").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Accept `7`, `载入16-9-7` and `surfaces/载入1024-7.jpg`, so a pin can be
    // copied straight out of a log line or an archive listing.
    let stem = raw
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(raw);
    match numbered_stem(stem).or_else(|| raw.parse::<u32>().ok()) {
        Some(number) if (1..=LOADING_PICTURES).contains(&number) => Some(Pic {
            number,
            matched: raw.to_string(),
        }),
        _ => {
            eprintln!(
                "PWE_LOADING_PICTURE={raw} is not one of the {LOADING_PICTURES} numbered loading pictures (1..={LOADING_PICTURES}); ignoring it"
            );
            None
        }
    }
}

/// The location identifiers a picture name is matched against, in priority
/// order.
///
/// The client's own `instance.txt` names a map in the script the client renders
/// (`Archosaur`, `Heaven's Tear`), while the archive and the folder tree use the
/// ASCII id (`a01`, `b05`). Both are offered, longest first, because a client
/// that names its art after a place may have used either. Empty entries are
/// dropped so a partially resolved catalog entry cannot match everything.
pub fn location_needles<'a>(map_id: &'a str, names: &'a [&'a str]) -> Vec<&'a str> {
    let mut needles: Vec<&str> = names
        .iter()
        .copied()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let id = map_id.trim();
    if !id.is_empty() {
        needles.push(id);
    }
    // Longest first: `Heaven's Tear` must win over a bare `tear` in another name.
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));
    needles.dedup();
    needles
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
    let mut result = source.to_vec();
    overlay_strip_in_place(
        &mut result,
        width,
        height,
        strip_height,
        fade_height,
        max_alpha,
    )?;
    Ok(result)
}

/// [`overlay_strip`] applied to a buffer in place.
///
/// The in-viewer loading overlay owns its frame and must not clone a 4K picture
/// to darken a strip along the bottom of it, so the blend runs once here and
/// both callers share it. The maths is identical to [`overlay_strip`]: alpha
/// ramps linearly across `fade_height` from the top of the strip and then holds
/// at `max_alpha`, and the alpha byte is preserved.
pub fn overlay_strip_in_place(
    frame: &mut [u8],
    width: u32,
    height: u32,
    strip_height: u32,
    fade_height: u32,
    max_alpha: f32,
) -> Result<(), String> {
    let expected = width as usize * height as usize * 4;
    if frame.len() != expected {
        return Err(format!(
            "RGBA frame is {} bytes, expected {expected} for {width}x{height}",
            frame.len()
        ));
    }
    let max_alpha = if max_alpha.is_finite() {
        max_alpha.clamp(0., 1.)
    } else {
        0.
    };
    let strip_height = strip_height.min(height);
    let fade_height = fade_height.min(strip_height);
    if strip_height == 0 {
        return Ok(());
    }
    let strip_top = height - strip_height;
    let fade_top = strip_top + fade_height;
    for y in strip_top..height {
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
                frame[offset + channel] = (frame[offset + channel] as f32 * keep) as u8;
            }
        }
    }
    Ok(())
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
            "surfaces/loading04.tga",
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
        // A single pair could in principle collide; a handful of draws must not
        // all be equal, and the low bits `pick` uses must not be stuck.
        let draws: Vec<u64> = (0..8).map(|_| random_word()).collect();
        assert!(
            draws.windows(2).any(|pair| pair[0] != pair[1]),
            "eight draws all repeated: {draws:?}"
        );
        // A degenerate low bit would make `pick` land on the same picture every
        // time. 64 draws all being even has probability 2^-64, so this cannot
        // flake while still catching a stuck bit.
        let wide: Vec<u64> = (0..64).map(|_| random_word()).collect();
        assert!(
            wide.iter().any(|word| word % 2 == 1),
            "every draw was even, so `pick` could never select an odd index: {wide:?}"
        );
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
        // The last fade row is partly darkened: strip_top is 6, the two-row
        // fade covers rows 6 and 7, and row 8 is already at full alpha. Row 6
        // gets alpha 0 (untouched) and row 7 gets alpha 1/2.
        let fade = 7 * width as usize * 4;
        assert!(out[fade] < source[fade], "row 7 must be darkened");
        assert!(out[fade] > 0, "the fade must not be fully black");
        assert_eq!(out[fade], 100, "200 at alpha 0.5 truncates to 100");
        assert!(overlay_strip(&source[..3], width, height, 4, 2, 1.0).is_err());
    }

    #[test]
    fn overlay_strip_clamps_alpha_and_zero_strip_is_identity() {
        let source = vec![90u8; 4 * 3 * 4];
        assert_eq!(overlay_strip(&source, 4, 3, 0, 0, 5.0).unwrap(), source);
        let out = overlay_strip(&source, 4, 3, 9, 9, f32::NAN).unwrap();
        assert_eq!(out, source);
        // A one-row strip is all fade band, so it stays at alpha 0 and changes
        // nothing; a two-row strip with a one-row fade reaches the full 0.5 at
        // its bottom row (strip_top = 1, fade_top = 2).
        assert_eq!(overlay_strip(&source, 4, 3, 1, 1, 0.5).unwrap(), source);
        let out = overlay_strip(&source, 4, 3, 2, 1, 0.5).unwrap();
        let bottom = 2 * 4 * 4;
        assert_eq!(out[bottom], 45, "90 at alpha 0.5 truncates to 45");
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

    fn picture(path: &str) -> Picture {
        Picture {
            path: path.to_string(),
            size: 1,
        }
    }

    #[test]
    fn a_location_matches_only_a_whole_name_token() {
        let pictures = vec![
            picture("surfaces/载入16-9-1.jpg"),
            picture("surfaces/archosaur.jpg"),
            picture("surfaces/b050.jpg"),
            picture("surfaces/underworld.jpg"),
        ];
        // A whole token matches.
        assert_eq!(
            match_location(&pictures, &["Archosaur"]).unwrap().path,
            "surfaces/archosaur.jpg"
        );
        // `b05` must not match `b050`, and `world` must not match `underworld`.
        assert!(match_location(&pictures, &["b05"]).is_none());
        assert!(match_location(&pictures, &["world"]).is_none());
        // An empty needle can never match everything.
        assert!(match_location(&pictures, &["", "  "]).is_none());
    }

    #[test]
    fn the_longest_matching_needle_wins() {
        let pictures = vec![
            picture("surfaces/tear.jpg"),
            picture("surfaces/heavens-tear.jpg"),
        ];
        // `heavens-tear.jpg` matches both needles and `tear.jpg` matches one, so
        // the specific name has to win or every "Tear" destination would share
        // a single generic picture.
        let found = match_location(&pictures, &["heavens tear", "tear"]).unwrap();
        assert_eq!(found.path, "surfaces/heavens-tear.jpg");
        // A name written with the client's punctuation still matches a stem
        // written with the archive's separators.
        let apostrophe = vec![picture("surfaces/heavens-tear.jpg")];
        assert_eq!(
            match_location(&apostrophe, &["Heaven's Tear"])
                .unwrap()
                .path,
            "surfaces/heavens-tear.jpg"
        );
    }

    #[test]
    fn a_numbered_stem_names_the_same_artwork_at_every_resolution() {
        assert_eq!(numbered_stem("载入16-9-7"), Some(7));
        assert_eq!(numbered_stem("载入1024-7"), Some(7));
        assert_eq!(numbered_stem("载入1280-11"), Some(11));
        // Out of range and non-numeric stems name no artwork.
        assert_eq!(numbered_stem("载入1024-12"), None);
        assert_eq!(numbered_stem("载入1024-0"), None);
        assert_eq!(numbered_stem("background"), None);
    }

    #[test]
    fn the_aspect_preference_never_leaves_a_client_without_pictures() {
        let only_4x3 = vec![picture("surfaces/载入1024-1.jpg")];
        // A 16:9 request on a client that ships only the 4:3 family still gets
        // a picture instead of nothing.
        assert_eq!(prefer_aspect(&only_4x3, 1920, 1080).len(), 1);
        let both = vec![
            picture("surfaces/载入1024-1.jpg"),
            picture("surfaces/载入16-9-1.jpg"),
        ];
        let wide = prefer_aspect(&both, 1920, 1080);
        assert_eq!(wide.len(), 1);
        assert!(wide[0].path.contains("16-9-"));
        let tall = prefer_aspect(&both, 1024, 768);
        assert_eq!(tall.len(), 1);
        assert!(tall[0].path.contains("1024-"));
    }

    #[test]
    fn location_needles_are_deduped_longest_first_and_drop_blanks() {
        let needles = location_needles("a01", &["Archosaur", "  ", "a01"]);
        assert_eq!(needles, vec!["Archosaur", "a01"]);
    }

    #[test]
    fn a_location_labels_itself_from_the_display_name() {
        let location = Location::new("a01", "Archosaur", "祖龙城");
        assert_eq!(location.label, "Archosaur (a01)");
        assert_eq!(location.needles, vec!["Archosaur", "a01"]);
        // A destination with no display name falls back to its id.
        assert_eq!(Location::new("world", "", "").label, "world");
    }
}
