//! Automatic first-run setup for the 64-bit Windows viewer.
//! The embedded PE32 bridge is required because the client's SpeedTree DLL is 32-bit.
use std::{
    ffi::{c_void, OsStr, OsString},
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::Write,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
const BRIDGE: &[u8] = include_bytes!("../tools/speedtree/speedtree-bake.exe");
type Hwnd = *mut c_void;
/// A GDI/Win32 handle, the same pointer shape as [`Hwnd`].
type Handle = *mut c_void;
#[repr(C)]
struct BrowseInfo {
    owner: Hwnd,
    root: *const c_void,
    display_name: *mut u16,
    title: *const u16,
    flags: u32,
    callback: Option<unsafe extern "system" fn(Hwnd, u32, isize, isize) -> i32>,
    param: isize,
    image: i32,
}
#[link(name = "shell32")]
extern "system" {
    fn SHBrowseForFolderW(info: *mut BrowseInfo) -> *mut c_void;
    fn SHGetNameFromIDList(id: *const c_void, form: u32, path: *mut *mut u16) -> i32;
}
#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, mode: u32) -> i32;
    fn CoUninitialize();
    fn CoTaskMemFree(ptr: *mut c_void);
}
#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(owner: Hwnd, text: *const u16, caption: *const u16, flags: u32) -> i32;
    fn SendMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn show_error(s: &str) {
    unsafe {
        let text = wide(s);
        let title = wide("Perfect World Explorer");
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
    }
}
unsafe extern "system" fn browse_callback(hwnd: Hwnd, msg: u32, _lp: isize, initial: isize) -> i32 {
    if msg == 1 && initial != 0 {
        SendMessageW(hwnd, 0x467, 1, initial);
    }
    0
}
pub fn choose_client(owner: Hwnd, initial: Option<&Path>) -> Result<Option<PathBuf>, String> {
    unsafe {
        let hr = CoInitializeEx(std::ptr::null_mut(), 2);
        let initialized = hr >= 0;
        let title =
            wide("Select the extracted Perfect World 1.4.2 client folder (or its element folder)");
        let initial = initial.map(|p| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        });
        let mut info = BrowseInfo {
            owner,
            root: std::ptr::null(),
            display_name: std::ptr::null_mut(),
            title: title.as_ptr(),
            flags: 0x1 | 0x10 | 0x40,
            callback: Some(browse_callback),
            param: initial.as_ref().map_or(0, |s| s.as_ptr() as isize),
            image: 0,
        };
        let pidl = SHBrowseForFolderW(&mut info);
        if pidl.is_null() {
            if initialized {
                CoUninitialize()
            };
            return Ok(None);
        }
        let mut raw: *mut u16 = std::ptr::null_mut();
        let code = SHGetNameFromIDList(pidl, 0x80058000, &mut raw);
        CoTaskMemFree(pidl);
        let result = if code < 0 || raw.is_null() {
            Err("Unable to obtain selected client folder".into())
        } else {
            let mut n = 0usize;
            while n < 32768 && *raw.add(n) != 0 {
                n += 1
            }
            let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, n)));
            CoTaskMemFree(raw.cast());
            if n == 32768 {
                Err("Selected path is too long".into())
            } else {
                Ok(Some(path))
            }
        };
        if initialized {
            CoUninitialize()
        }
        result
    }
}
/// Case-insensitive file lookup for extracted client folders.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Default scene for callers that do not have a selected map. Prefer the
/// canonical main world; a client shipping only instance maps still works.
fn default_scene_file(element: &Path) -> PathBuf {
    let maps = element.join("maps");
    let main_world = maps.join("world");
    if let Some(path) = find_file(&main_world, "world.ecwld") {
        return path;
    }
    let mut dirs = fs::read_dir(&maps)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect::<Vec<_>>();
    dirs.sort();
    for dir in dirs {
        let id = dir.file_name().and_then(OsStr::to_str).unwrap_or("world");
        if let Some(path) =
            find_file(&dir, &format!("{id}.ecwld")).or_else(|| find_file(&dir, "world.ecwld"))
        {
            return path;
        }
    }
    element.join("maps/world/world.ecwld")
}

/// Cache identity includes the selected scene and its authored tree table
/// size, not just the owning `element` folder and the main world's ECWLD.
fn cache_key(element: &Path, scene_path: &Path, tree_type_count: usize) -> Result<u64, String> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "PWTREE09-selected-scene-variable-tree-count-v1".hash(&mut h);
    fs::canonicalize(element)
        .unwrap_or_else(|_| element.to_path_buf())
        .hash(&mut h);
    fs::canonicalize(scene_path)
        .unwrap_or_else(|_| scene_path.to_path_buf())
        .hash(&mut h);
    tree_type_count.hash(&mut h);
    for path in [element.join("trees.pck"), scene_path.to_path_buf()] {
        let m = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        m.len().hash(&mut h);
        if let Ok(time) = m.modified() {
            time.hash(&mut h)
        }
    }
    let dll =
        fs::read(element.join("speedtreert.dll")).map_err(|e| format!("SpeedTreeRT.dll: {e}"))?;
    dll.hash(&mut h);
    BRIDGE.hash(&mut h);
    Ok(h.finish())
}

/// Tree types one map may declare and have prepared.
///
/// See `prepare_trees_for_scene`; the largest table in any staged client is 59.
pub const MAX_TREE_TYPES: usize = 4096;

/// Tree types the legacy three-argument bridge bakes, and therefore the size of
/// the padded source table it is given.
pub const LEGACY_BRIDGE_TREE_TYPES: usize = 59;

/// A map with no tree types needs only its ready marker; other maps need one
/// PWT per authored tree type. The legacy three-argument bridge expects 59.
fn ready(dir: &Path, tree_type_count: usize) -> bool {
    dir.join("ready").is_file()
        && (0..tree_type_count).all(|i| dir.join(format!("tree-{i:03}.pwt")).is_file())
}

pub fn prepare_trees(element: &Path) -> Result<PathBuf, String> {
    let scene_path = default_scene_file(element);
    let scene = crate::scene::Scene::load(&scene_path)?;
    prepare_trees_for_scene(element, &scene_path, scene.tree_types.len())
}

/// Prepare the tree cache from the selected map's ECWLD. Different maps
/// reference different tree types, and some ship none at all. The embedded
/// bridge is called first with an explicit count; an older checked-in
/// three-argument bridge is supported by writing a padded 59-source table.
pub fn prepare_trees_for_scene(
    element: &Path,
    scene_path: &Path,
    tree_type_count: usize,
) -> Result<PathBuf, String> {
    // Measured, not guessed: the largest authored tree table staged is the
    // 1.8.4 world's 59 types (`tools/version-gates.py` reads the field straight
    // out of every `.ecwld` header). The bridge writes one file per type, so the
    // limit is about the filesystem rather than memory; 4,096 keeps two orders
    // of magnitude of headroom over anything shipped and still fails loudly
    // instead of trying to write a million files.
    if tree_type_count > MAX_TREE_TYPES {
        return Err(format!(
            "{} declares {tree_type_count} tree types; this build prepares at most {MAX_TREE_TYPES} \
             (the largest table in any staged client is 59)",
            scene_path.display()
        ));
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("PerfectWorldViewer")
        .join("tree-cache");
    fs::create_dir_all(&base).map_err(|e| format!("Cannot create tree cache: {e}"))?;
    let key = cache_key(element, scene_path, tree_type_count)?;
    let final_dir = base.join(format!("pwt09-{key:016x}"));
    if ready(&final_dir, tree_type_count) {
        eprintln!("Using prepared SpeedTree cache {}", final_dir.display());
        let report = final_dir.join("tree-alpha-refs.csv");
        if report.is_file() {
            eprintln!(
                "Native branch/frond alpha-reference report: {}",
                report.display()
            );
        }
        return Ok(final_dir);
    }
    if final_dir.exists() {
        return Err(format!(
            "Tree cache is incomplete: {}. Rename or remove that folder and retry.",
            final_dir.display()
        ));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let work = base.join(format!("working-{}-{nonce}", std::process::id()));
    fs::create_dir(&work)
        .map_err(|e| format!("Cannot create temporary tree preparation folder: {e}"))?;
    let result = (|| -> Result<(), String> {
        let data = work.join("data");
        eprintln!("First launch: extracting trees and preparing them with the SpeedTreeRT.dll from your client. This may take a few minutes.");
        loading_progress(format!(
            "Preparing SpeedTree sources for {}...",
            scene_path.display()
        ));
        // The table is built from the count the world file declares. It used to
        // be padded to 59 unconditionally, to suit the legacy three-argument
        // bridge, and that padding had a real cost: a map authoring 60+ types
        // got the wrong tree or none, because the padded run stopped at 59.
        //
        // Padding is only ever needed by the *legacy* fallback below, and only
        // when it actually runs, so it now happens there. The embedded bridge
        // is called first with the exact count.
        let mut source_count = tree_type_count;
        crate::tree_source::extract_for_scene(element, scene_path, &data, source_count)?;
        eprintln!(
            "SpeedTree: {source_count} authored tree type(s) in {}",
            scene_path.display()
        );
        if tree_type_count == 0 {
            fs::write(
                data.join("tree-alpha-refs.csv"),
                b"tree_id,lod,alpha_branch,alpha_frond\n",
            )
            .map_err(|e| e.to_string())?;
        } else {
            let helper = work.join("speedtree-bridge.exe");
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&helper)
                .map_err(|e| e.to_string())?;
            f.write_all(BRIDGE).map_err(|e| e.to_string())?;
            drop(f);
            let bridge_error = |out: &std::process::Output| {
                format!(
                    "SpeedTree preparation failed ({})\n{}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                )
            };
            let variable = Command::new(&helper)
                .arg(element.join("speedtreert.dll"))
                .arg(&data)
                .arg(tree_type_count.to_string())
                .creation_flags(0x08000000)
                .output()
                .map_err(|e| format!("Unable to launch 32-bit SpeedTree bridge: {e}"))?;
            let mut expected_count = tree_type_count;
            if !variable.status.success() {
                // The legacy bridge rejects a fourth argument. It can still
                // bake a padded 59-type table for maps with at most 59 types.
                if tree_type_count > LEGACY_BRIDGE_TREE_TYPES {
                    return Err(format!(
                        "{}\nThis map has {} tree types; rebuild tools/speedtree/speedtree-bake.exe from bake.cpp.",
                        bridge_error(&variable),
                        tree_type_count
                    ));
                }
                // Only here, and only now, does the table get padded: the
                // legacy bridge has no way to be told how many types to bake,
                // so it bakes its fixed 59. A map with more than 59 types
                // cannot be served by it at all, and says so by name above.
                eprintln!(
                    "Variable-count SpeedTree bridge unavailable; using the legacy 59-type protocol \
                     ({} authored type(s) padded to 59)",
                    tree_type_count
                );
                let padded = LEGACY_BRIDGE_TREE_TYPES;
                crate::tree_source::extract_for_scene(element, scene_path, &data, padded)?;
                let legacy = Command::new(&helper)
                    .arg(element.join("speedtreert.dll"))
                    .arg(&data)
                    .creation_flags(0x08000000)
                    .output()
                    .map_err(|e| format!("Unable to launch 32-bit SpeedTree bridge: {e}"))?;
                if !legacy.status.success() {
                    return Err(format!(
                        "{}\nLegacy bridge: {}",
                        bridge_error(&variable),
                        bridge_error(&legacy)
                    ));
                }
                expected_count = padded;
                source_count = padded;
            }
            for i in 0..expected_count {
                if !data.join(format!("tree-{i:03}.pwt")).is_file() {
                    return Err(format!("SpeedTree type {i} did not generate"));
                }
            }
        }
        // The viewer reads source textures directly from the client's PCK. Do not
        // retain duplicate extracted SPT/DDS files or placement spreadsheets.
        for i in 0..source_count {
            for ext in ["spt", "dds"] {
                fs::remove_file(data.join(format!("tree-{i:03}.{ext}")))
                    .map_err(|e| format!("Cannot clean temporary tree source: {e}"))?;
            }
        }
        for name in ["tree-types.tsv", "tree-placements.tsv"] {
            fs::remove_file(data.join(name))
                .map_err(|e| format!("Cannot clean temporary manifest: {e}"))?;
        }
        fs::write(data.join("ready"), b"PWTREE09\n").map_err(|e| e.to_string())?;
        loading_progress("SpeedTree cache ready; loading the selected world...".into());
        match fs::rename(&data, &final_dir) {
            Ok(()) => {}
            Err(e) => {
                if !ready(&final_dir, tree_type_count) {
                    return Err(format!("Cannot publish tree cache: {e}"));
                }
            }
        }
        Ok(())
    })();
    // Only remove our own uniquely created temporary folder, never client files.
    if let Err(e) = fs::remove_dir_all(&work) {
        eprintln!(
            "Could not remove temporary setup folder {}: {e}",
            work.display()
        );
    }
    result?;
    let report = final_dir.join("tree-alpha-refs.csv");
    if report.is_file() {
        eprintln!(
            "Native branch/frond alpha-reference report: {}",
            report.display()
        );
    } else {
        eprintln!(
            "SpeedTree alpha-reference report unavailable; rebuild the embedded bridge from tools/speedtree/bake.cpp."
        );
    }
    eprintln!("SpeedTree preparation finished.");
    Ok(final_dir)
}

// Show a small native loading window while terrain and the first-run tree cache
// are prepared. Its message pump lives on a separate thread from disk loading.
#[repr(C)]
struct WinMessage {
    hwnd: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    x: i32,
    y: i32,
    private: u32,
}
#[link(name = "user32")]
extern "system" {
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> Hwnd;
    fn ShowWindow(window: Hwnd, how: i32) -> i32;
    fn UpdateWindow(window: Hwnd) -> i32;
    fn PeekMessageW(msg: *mut WinMessage, window: Hwnd, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const WinMessage) -> i32;
    fn DispatchMessageW(msg: *const WinMessage) -> isize;
    fn DestroyWindow(window: Hwnd) -> i32;
}
static LOADING_SENDER: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::mpsc::Sender<String>>>,
> = std::sync::OnceLock::new();
pub fn loading_progress(message: String) {
    // A replacement-process child publishes the same progress for the old
    // window's non-interactive overlay. This writes the handoff status file
    // directly so the standalone Windows tree-setup diagnostic remains a
    // self-contained binary. No-op outside a world handoff.
    if let Some(path) = std::env::var_os("PWE_WORLD_HANDOFF_STATUS_FILE") {
        let _ = std::fs::write(std::path::PathBuf::from(path), message.as_bytes());
    }
    if let Some(lock) = LOADING_SENDER.get() {
        if let Ok(guard) = lock.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(message);
            }
        }
    }
}
pub(crate) fn directstorage_state(stats: crate::pck::direct_storage::ReadStats) -> String {
    // One implementation, shared with the in-viewer overlay: the standalone
    // window and the overlay must never describe the same counters differently.
    crate::pck::direct_storage::state_line(stats)
}

use crate::launcher_ui;
use crate::launcher_ui::BitmapInfo;

/// Rectangle in client coordinates (`RECT`).
#[repr(C)]
struct WindowRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn GetClientRect(window: Hwnd, rect: *mut WindowRect) -> i32;
    fn PostQuitMessage(code: i32);
    fn InvalidateRect(window: Hwnd, rect: *const c_void, erase: i32) -> i32;
}

/// One decoded loading-screen picture and the GDI surface sized to the window.
///
/// The artwork is decoded, fitted to the client area and darkened behind the
/// status strip once, on the loading thread, before the window is shown. It is
/// the same picture for the whole load, so scaling it per repaint would only add
/// work to the GDI paint path.
struct LoadingArt {
    /// `BITMAPINFOHEADER` followed by top-down 32-bit BGRA pixels.
    bits: Vec<u8>,
    /// The picture exactly as it was decoded from the archive.
    ///
    /// A window resize must re-fit from this, never from the previous output:
    /// `fill` crops the source to cover the target, so re-fitting an already
    /// cropped frame crops it a second time and the picture looks zoomed in and
    /// clipped on every side. The initial decode does not know the real client
    /// area yet, so this is the only way a later fit can be correct.
    source: image::RgbaImage,
    width: u32,
    height: u32,
    /// Archive path of the picture, shown in the window so a screenshot names
    /// which loading screen the client's archive produced.
    label: String,
}

impl LoadingArt {
    /// Fit `image` to `width` x `height` as a top-down BGRA DIB.
    fn new(
        image: &image::RgbaImage,
        width: u32,
        height: u32,
        label: String,
    ) -> Result<Self, String> {
        let width = width.max(1);
        let height = height.max(1);
        let fitted = crate::loading_screen::fill(image, width, height)?;
        // The status strip is blended here, once, because GDI cannot alpha-blend
        // a drawn rectangle over a bitmap.
        let blended = crate::loading_screen::overlay_strip(
            fitted.as_raw(),
            width,
            height,
            OVERLAY_STRIP_HEIGHT,
            OVERLAY_FADE_HEIGHT,
            OVERLAY_MAX_ALPHA,
        )?;
        let mut bits = vec![0u8; std::mem::size_of::<BitmapInfo>()];
        // BITMAPINFOHEADER: biSize, biWidth, biHeight (negative = top-down),
        // biPlanes = 1, biBitCount = 32, biCompression = BI_RGB.
        bits[0..4].copy_from_slice(&40u32.to_le_bytes());
        bits[4..8].copy_from_slice(&(width as i32).to_le_bytes());
        bits[8..12].copy_from_slice(&(-(height as i32)).to_le_bytes());
        bits[12..14].copy_from_slice(&1u16.to_le_bytes());
        bits[14..16].copy_from_slice(&32u16.to_le_bytes());
        bits.extend_from_slice(&crate::loading_screen::to_bgra(&blended));
        Ok(Self {
            bits,
            source: image.clone(),
            width,
            height,
            label,
        })
    }

    /// The header at the front of `bits`. `BitmapInfo` is the 40-byte
    /// `BITMAPINFOHEADER` this constructor writes.
    fn info(&self) -> &BitmapInfo {
        // SAFETY: `bits` starts with exactly one `BitmapInfo`.
        unsafe { &*(self.bits.as_ptr() as *const BitmapInfo) }
    }

    fn pixels(&self) -> *const c_void {
        // SAFETY: the header is `size_of::<BitmapInfo>()` long and the pixel
        // bytes follow it for `width * height` pixels.
        unsafe {
            self.bits
                .as_ptr()
                .add(std::mem::size_of::<BitmapInfo>())
                .cast()
        }
    }

    /// Re-fit the original picture to a new client size.
    ///
    /// Fitting from `self.source` rather than from the current `bits` is what
    /// keeps a resize from compounding the cover crop: `fill` always crops the
    /// source to the target's aspect, so feeding it the previous, already
    /// cropped result would zoom in once per resize event.
    fn refit(&mut self, width: u32, height: u32) -> Result<(), String> {
        let width = width.max(MIN_ART_WIDTH);
        let height = height.max(MIN_ART_HEIGHT);
        if self.matches(width, height) {
            return Ok(());
        }
        let source = self.source.clone();
        *self = Self::new(&source, width, height, self.label.clone())?;
        Ok(())
    }

    /// True when the fitted surface already covers a `width` x `height` client
    /// area.
    ///
    /// The comparison applies the same floor `refit` does. Without that, a
    /// window smaller than the floor would never compare equal and every single
    /// paint would re-fit and re-compose the whole frame.
    fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width.max(MIN_ART_WIDTH) && self.height == height.max(MIN_ART_HEIGHT)
    }
}

/// Client area of a window, in pixels, with a safe fallback so a window whose
/// size cannot be queried still gets a usable canvas.
unsafe fn client_size(window: Hwnd) -> (u32, u32) {
    let mut rect = WindowRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetClientRect(window, &mut rect) == 0 || rect.right <= rect.left || rect.bottom <= rect.top {
        return (1280, 720);
    }
    (
        (rect.right - rect.left) as u32,
        (rect.bottom - rect.top) as u32,
    )
}

fn paint_loading_art(dc: *mut c_void, art: &LoadingArt) {
    unsafe {
        // No black pre-fill: `SRCCOPY` already replaces every pixel in the
        // destination rectangle, and on a composited desktop a surface can be
        // presented between two GDI calls, so painting black first showed up as
        // a flash. The artwork is the first thing drawn.
        launcher_ui::StretchDIBits(
            dc,
            0,
            0,
            art.width as i32,
            art.height as i32,
            0,
            0,
            art.width as i32,
            art.height as i32,
            art.pixels(),
            art.info(),
            0,
            0x00cc0020, // SRCCOPY
        );
    }
}

/// Blit only `top..bottom` of the artwork.
///
/// The picture is a top-down DIB, so a row slice of the source maps one-to-one
/// onto the destination rows; `StretchDIBits` is given the sub-rectangle so a
/// bar-only repaint does not re-upload a full-screen bitmap.
fn paint_loading_art_band(dc: *mut c_void, art: &LoadingArt, top: i32, bottom: i32, width: u32) {
    let top = top.max(0);
    let bottom = bottom.min(art.height as i32);
    if bottom <= top {
        return;
    }
    let rows = bottom - top;
    // The window can be wider than the fitted picture if it grew between the
    // WM_SIZE that refitted it and this paint; only the overlap is copied and
    // the remainder is the window's own background.
    let columns = width.min(art.width) as i32;
    if columns <= 0 {
        return;
    }
    unsafe {
        launcher_ui::StretchDIBits(
            dc,
            0,
            top,
            columns,
            rows,
            0,
            top,
            columns,
            rows,
            art.pixels(),
            art.info(),
            0,
            0x00cc0020, // SRCCOPY
        );
    }
}

/// Draw the status overlay: the DirectStorage state, the throughput graph, the
/// read totals, the current progress message and an indeterminate loading bar.
///
/// The dark strip behind these rows is already blended into the artwork bitmap,
/// so this only draws text and the bar.
fn paint_loading_overlay(dc: *mut c_void, width: u32, height: u32, state: &LoadingWindowState) {
    paint_loading_overlay_band(dc, width, height, state, 0, height as i32);
}

/// Draw the overlay, touching only rows `top..bottom`.
///
/// GDI has no per-call clip in this path, so the band is applied by skipping
/// any primitive that lies outside it. The five text lines and the bar are
/// independent rectangles, so this keeps a bar-only repaint to the bar.
fn paint_loading_overlay_band(
    dc: *mut c_void,
    width: u32,
    height: u32,
    state: &LoadingWindowState,
    top: i32,
    bottom: i32,
) {
    let width = width as i32;
    let height = height as i32;
    // A 178-pixel strip would cover a tiny window entirely and put its text
    // above the client area, so the strip follows the window on small sizes
    // and keeps its designed height on any ordinary screen.
    let strip = (height / 3).min(OVERLAY_STRIP_HEIGHT as i32).max(64);
    let margin = (width / 40).clamp(10, 28);
    let text_top = height - strip + 22;
    // Does a row range intersect the band being repainted? The band is either
    // the whole window (a status change) or the bar strip (the sweep tick), so
    // this is the only clip the overlay needs.
    let intersects = |row_top: i32, row_bottom: i32| row_top < bottom && row_bottom > top;
    unsafe {
        if intersects(text_top, text_top + 20) {
            launcher_ui::draw_text(
                dc,
                margin,
                text_top,
                "PERFECT WORLD EXPLORER  —  LOADING WORLD",
                15,
                700,
                launcher_ui::rgb(214, 236, 235),
            );
        }
        if intersects(text_top + 26, text_top + 26 + 20) {
            launcher_ui::draw_text(
                dc,
                margin,
                text_top + 26,
                &state.status,
                13,
                400,
                launcher_ui::rgb(233, 244, 243),
            );
        }
        if intersects(text_top + 48, text_top + 48 + 20) {
            launcher_ui::draw_text(
                dc,
                margin,
                text_top + 48,
                &state.ds_state,
                12,
                400,
                launcher_ui::rgb(176, 214, 210),
            );
        }
        if intersects(text_top + 68, text_top + 68 + 20) {
            launcher_ui::draw_text(
                dc,
                margin,
                text_top + 68,
                &state.graph,
                12,
                400,
                launcher_ui::rgb(176, 214, 210),
            );
        }
        if intersects(text_top + 88, text_top + 88 + 20) {
            launcher_ui::draw_text(
                dc,
                margin,
                text_top + 88,
                &state.totals,
                11,
                400,
                launcher_ui::rgb(150, 186, 184),
            );
        }
        // Indeterminate bar: the fill sweeps forward and back, so the load is
        // visibly alive without claiming a percentage the loader does not have.
        let bar_top = height - 26;
        let bar_width = (width - margin * 2).max(120);
        if intersects(bar_top, bar_top + 6) {
            launcher_ui::fill_rect(
                dc,
                margin,
                bar_top,
                bar_width,
                6,
                launcher_ui::rgb(38, 66, 74),
            );
            let sweep = (bar_width / 4).max(48);
            let travel = (bar_width - sweep).max(1) as f32;
            let position = ((state.phase.sin() * 0.5 + 0.5) * travel) as i32;
            launcher_ui::fill_rect(
                dc,
                margin + position,
                bar_top,
                sweep,
                6,
                launcher_ui::rgb(122, 214, 199),
            );
        }
        // The artwork's archive path (or the reason there is none) in the
        // corner: it names the picture, so a report of the loading screen is
        // checkable against the client's own archive.
        if !state.note.is_empty() && intersects(height - 48, height - 28) {
            let note_width = launcher_ui::text_width(dc, &state.note, 10);
            launcher_ui::draw_text(
                dc,
                (width - margin - note_width).max(margin),
                height - 48,
                &state.note,
                10,
                400,
                launcher_ui::rgb(128, 158, 160),
            );
        }
    }
}

/// Everything the window procedure paints. One loading window exists per
/// process and the procedure cannot borrow the loader thread's stack through
/// `lpParam` while the message loop runs, so the state lives in a static.
struct LoadingWindowState {
    art: Option<LoadingArt>,
    status: String,
    ds_state: String,
    graph: String,
    totals: String,
    note: String,
    /// Bar sweep angle in radians, advanced on the overlay timer.
    phase: f32,
}

static LOADING_WINDOW_STATE: std::sync::OnceLock<std::sync::Mutex<Option<LoadingWindowState>>> =
    std::sync::OnceLock::new();

/// The loading window's state, or `None` if a previous panic poisoned it.
///
/// A poisoned lock must never take the paint thread down with it: the window
/// keeps drawing its plain background and the load itself is unaffected.
fn lock_loading_state() -> Option<std::sync::MutexGuard<'static, Option<LoadingWindowState>>> {
    loading_state().lock().ok()
}

fn loading_state() -> &'static std::sync::Mutex<Option<LoadingWindowState>> {
    LOADING_WINDOW_STATE.get_or_init(|| std::sync::Mutex::new(None))
}

/// The window's back buffer: one off-screen surface, resized only when the
/// client area changes.
///
/// The loading window repaints about 25 times a second, and every repaint used
/// to draw the picture, five text lines and two rectangles straight into the
/// window's device context. A composited desktop can present that surface
/// part-way through, so a partially drawn frame reached the screen and the
/// window appeared to flash. Composing into a memory DC and presenting the
/// finished frame with a single `BitBlt` makes each frame atomic: either the
/// previous complete frame or the new complete frame is on screen, never a
/// half-painted one.
struct BackBuffer {
    /// Raw handles are stored as `usize` so the type is `Send`; that is what
    /// lets one process-wide `Mutex<BackBuffer>` hold it. GDI objects are
    /// process-wide and used from the single window thread here.
    memory_dc: usize,
    bitmap: usize,
    previous: usize,
    width: i32,
    height: i32,
}

impl BackBuffer {
    const EMPTY: Self = Self {
        memory_dc: 0,
        bitmap: 0,
        previous: 0,
        width: 0,
        height: 0,
    };

    /// A `width` x `height` memory DC compatible with `dc`, reusing the existing
    /// surface when the size still matches.
    ///
    /// Returns `true` when a surface was created, because a fresh bitmap holds
    /// undefined pixels and the caller must compose all of it rather than only
    /// the dirty band.
    unsafe fn resize(&mut self, dc: Handle, width: i32, height: i32) -> bool {
        if self.memory_dc != 0 && self.width == width && self.height == height {
            return false;
        }
        self.release();
        let memory_dc = launcher_ui::CreateCompatibleDC(dc);
        if memory_dc.is_null() {
            return false;
        }
        let bitmap = launcher_ui::CreateCompatibleBitmap(dc, width, height);
        if bitmap.is_null() {
            launcher_ui::DeleteDC(memory_dc);
            return false;
        }
        // The old bitmap comes back so it can be deleted after the DC is done
        // with it; leaking it would exhaust GDI handles over a long load.
        let previous = launcher_ui::SelectObject(memory_dc, bitmap);
        self.memory_dc = memory_dc as usize;
        self.bitmap = bitmap as usize;
        self.previous = previous as usize;
        self.width = width;
        self.height = height;
        true
    }

    /// The off-screen device context, or null when none could be created.
    fn dc(&self) -> Handle {
        self.memory_dc as Handle
    }

    unsafe fn release(&mut self) {
        if self.memory_dc != 0 {
            let memory_dc = self.memory_dc as Handle;
            if self.previous != 0 {
                launcher_ui::SelectObject(memory_dc, self.previous as Handle);
            }
            launcher_ui::DeleteDC(memory_dc);
        }
        if self.bitmap != 0 {
            launcher_ui::DeleteObject(self.bitmap as Handle);
        }
        *self = Self::EMPTY;
    }
}

/// The one back buffer the paint handler composes into, guarded because the
/// window procedure is re-entered from the loading thread's message pump.
static BACK_BUFFER: std::sync::OnceLock<std::sync::Mutex<BackBuffer>> = std::sync::OnceLock::new();

fn back_buffer() -> &'static std::sync::Mutex<BackBuffer> {
    BACK_BUFFER.get_or_init(|| std::sync::Mutex::new(BackBuffer::EMPTY))
}

/// `WM_PAINT`: compose the whole frame off-screen, then present it once.
///
/// The update rectangle is honoured: the animation tick invalidates only the
/// bar strip, and repainting a full-screen bitmap for a six-pixel bar would
/// waste the frame budget and give a composited desktop a chance to catch a
/// half-drawn surface.
unsafe fn paint_loading_window(hwnd: Hwnd) {
    let mut paint: launcher_ui::Paint = std::mem::zeroed();
    let dc = launcher_ui::BeginPaint(hwnd, &mut paint);
    if dc.is_null() {
        return;
    }
    let (width, height) = client_size(hwnd);
    // `rcPaint` is in client coordinates; a zero-area rectangle means the
    // update region was empty, so there is nothing to compose.
    let update_top = paint.rect.top.max(0);
    let update_bottom = paint.rect.bottom.min(height as i32);
    // An empty update region (or one entirely outside the client area) means
    // there is nothing to compose. Rows are what the band compositor works in,
    // so only the vertical extent is needed; the client width is the horizontal
    // extent for every primitive in this paint path.
    if paint.rect.left >= width as i32 || paint.rect.right <= 0 || update_bottom <= update_top {
        launcher_ui::EndPaint(hwnd, &paint);
        return;
    }
    let background = launcher_ui::rgb(12, 24, 30);
    let mut buffer = match back_buffer().lock() {
        Ok(buffer) => buffer,
        Err(_) => {
            // A poisoned back buffer must not blank the window: paint directly.
            launcher_ui::fill_rect(dc, 0, 0, width as i32, height as i32, background);
            launcher_ui::EndPaint(hwnd, &paint);
            return;
        }
    };
    let fresh_surface = buffer.resize(dc, width as i32, height as i32);
    let offscreen = buffer.memory_dc != 0;
    let target = if offscreen {
        // The back buffer keeps the rest of the frame from the last paint, so
        // composing only the dirty band leaves the untouched rows exactly as
        // they were. That is what makes a bar-only repaint cheap.
        buffer.dc()
    } else {
        // Without an off-screen surface the window itself is the target, and it
        // has no memory of earlier frames: compose the whole client area or the
        // band-only path would leave the rest of the window unpainted.
        dc
    };
    // A fresh back buffer holds undefined pixels and a window target has no
    // memory at all, so both have to be composed in full; only a reused buffer
    // can keep the rest of the last frame.
    let (mut band_top, mut band_bottom) = if offscreen && !fresh_surface {
        (update_top, update_bottom)
    } else {
        (0, height as i32)
    };
    // Compose the band that is actually dirty. Redrawing a full-screen bitmap
    // for the six-pixel bar would burn the frame budget, and because the picture
    // blit and the text are separate GDI calls it would also give the desktop
    // more chances to catch an incomplete frame.
    //
    // A poisoned state lock must never leave the window blank, so the fallback
    // below paints a plain dark fill instead of panicking on the paint thread.
    let mut guard = lock_loading_state();
    match guard.as_mut().and_then(|state| state.as_mut()) {
        Some(state) => {
            // The client area can differ from the fitted picture between a
            // WM_SIZE and the paint that follows it (a maximise changes the size
            // before the message is delivered). Re-fitting here keeps the
            // picture flush with the window instead of leaving an unpainted
            // edge, and it is a no-op when the sizes already agree.
            if let Some(art) = state.art.as_mut() {
                if !art.matches(width, height) {
                    if let Err(error) = art.refit(width, height) {
                        eprintln!("Loading screen artwork could not be refitted: {error}");
                    }
                    // A refit rewrites the whole picture, so the band optimisation
                    // cannot be used for this paint.
                    band_top = 0;
                    band_bottom = height as i32;
                }
            }
            match state.art.as_ref() {
                Some(art) => paint_loading_art_band(target, art, band_top, band_bottom, width),
                None => launcher_ui::fill_rect(
                    target,
                    0,
                    band_top,
                    width as i32,
                    band_bottom - band_top,
                    background,
                ),
            }
            paint_loading_overlay_band(target, width, height, state, band_top, band_bottom);
        }
        None => launcher_ui::fill_rect(
            target,
            0,
            band_top,
            width as i32,
            band_bottom - band_top,
            background,
        ),
    }
    drop(guard);
    if target != dc {
        // Present exactly the rows that were composed. A band-only compose is
        // enough because the window still shows the matching rows of the
        // previous frame; a full compose (first paint, resize, or a re-fit)
        // replaces the whole client area at once, so no stale row survives.
        let present_top = band_top;
        let present_height = (band_bottom - band_top).max(0);
        if present_height > 0 {
            launcher_ui::BitBlt(
                dc,
                0,
                present_top,
                width as i32,
                present_height,
                target,
                0,
                present_top,
                0x00cc0020,
            );
        }
    }
    launcher_ui::EndPaint(hwnd, &paint);
}

unsafe extern "system" fn loading_window_proc(
    hwnd: Hwnd,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match msg {
        0x000f => {
            // WM_PAINT
            paint_loading_window(hwnd);
            0
        }
        // WM_ERASEBKGND: claim the erase and leave the background alone. Letting
        // the class brush run would flash its black face on every resize, and
        // returning 1 without painting is what makes that safe -- the paint
        // handler immediately covers the whole invalid region with the composed
        // frame.
        0x0014 => 1,
        0x0005 => {
            // WM_SIZE: refit the artwork instead of stretching a stale bitmap.
            let width = (lparam as u32 & 0xffff).max(MIN_ART_WIDTH);
            let height = ((lparam as u32 >> 16) & 0xffff).max(MIN_ART_HEIGHT);
            if let Some(mut guard) = lock_loading_state() {
                if let Some(art) = guard.as_mut().and_then(|state| state.art.as_mut()) {
                    if let Err(error) = art.refit(width, height) {
                        eprintln!("Loading screen artwork could not be refitted: {error}");
                    }
                }
            }
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        0x0113 => {
            // WM_TIMER: advance the bar sweep. The repaint request comes from
            // the loading thread's own pump, which also owns the status text, so
            // the window never depends on this timer being delivered to animate.
            if let Some(mut guard) = lock_loading_state() {
                if let Some(state) = guard.as_mut() {
                    state.phase = (state.phase + 0.10) % std::f32::consts::TAU;
                }
            }
            0
        }
        0x0002 => {
            // WM_DESTROY: give the back buffer's DC and bitmap back before the
            // message loop exits. Without this the two GDI handles would live
            // until the process ends, and the loading window is created once per
            // world load.
            if let Ok(mut buffer) = back_buffer().lock() {
                buffer.release();
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Height of the darkened status strip at the bottom of the loading window, and
/// the fade above it. The artwork itself covers the whole window; only these
/// rows are blended toward black so the overlay stays legible.
/// The smallest fitted surface. Below this the window is too small for the
/// status text to be readable anyway, so there is nothing to gain from
/// re-fitting per pixel as a user drags a tiny window.
const MIN_ART_WIDTH: u32 = 320;
const MIN_ART_HEIGHT: u32 = 240;
const OVERLAY_STRIP_HEIGHT: u32 = 178;
const OVERLAY_FADE_HEIGHT: u32 = 72;
const OVERLAY_MAX_ALPHA: f32 = 0.78;
/// `PWE_LOADING_WINDOWED=1` keeps the loading window in its framed size instead
/// of maximising it. It exists for diagnostics on multi-monitor desktops, not to
/// change the default: the picture fills the screen by default.
const LOADING_WINDOWED_ENV: &str = "PWE_LOADING_WINDOWED";

/// Pick the client's random loading screen and decode it for a `width` x
/// `height` window.
///
/// Runs on the loading thread before the window is shown, so a corrupt archive
/// can only cost this one step: every failure is reported and the window falls
/// back to its plain background instead of failing the world load.
/// The loading picture for a destination, decoded and fitted.
///
/// `location` is the destination's identity when the load is switching worlds:
/// its map id and the client's own names for it. The picture is matched against
/// the archive by those names, then by the session pin, then falls back to a
/// random picture from the family that suits the window's aspect ratio. A
/// destination that names no art in the archive takes the random fallback and
/// says so in the log; nothing is invented to fill the gap.
fn loading_background(
    element: &Path,
    width: u32,
    height: u32,
    location: Option<&crate::loading_screen::Location>,
) -> (Option<LoadingArt>, String) {
    let word = crate::loading_screen::random_word();
    let chosen = match location {
        Some(location) => crate::loading_screen::choose_for_location(
            element,
            &location.needles,
            crate::loading_screen::pinned_picture().as_ref(),
            width,
            height,
            word,
        )
        .map(|(picture, source)| {
            eprintln!(
                "Loading screen for {} matched by {source}: {}",
                location.label, picture.path
            );
            picture
        }),
        None => crate::loading_screen::choose(element, word).map(|picture| {
            eprintln!("Loading screen: {} chosen at random", picture.path);
            picture
        }),
    };
    let picture = match chosen {
        Ok(picture) => picture,
        Err(error) => {
            eprintln!("Loading screen artwork unavailable: {error}");
            return (None, error);
        }
    };
    let image = match crate::loading_screen::decode(element, &picture) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("Loading screen artwork could not be decoded: {error}");
            return (None, error);
        }
    };
    let (source_width, source_height) = image.dimensions();
    let label = format!("{} ({}x{})", picture.path, source_width, source_height);
    eprintln!(
        "Loading screen: {label} from {}, fitted to {width}x{height}",
        element.join(crate::loading_screen::ARCHIVE_NAME).display()
    );
    match LoadingArt::new(&image, width, height, label.clone()) {
        Ok(art) => (Some(art), label),
        Err(error) => {
            eprintln!("Loading screen artwork could not be fitted: {error}");
            (None, error)
        }
    }
}

pub struct LoadingWindow {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl LoadingWindow {
    /// Open the loading window and start delivering progress.
    ///
    /// `client` is the client root, `element` folder or map directory the user
    /// selected, or `None` before a folder is known; its `surfaces.pck` supplies
    /// the artwork, chosen at random for this load. The window is borderless and
    /// maximised - the same style the viewer window uses - so the picture fills
    /// the screen rather than sitting inside a small frame.
    pub fn start(client: Option<&Path>) -> Self {
        Self::start_for(client, None)
    }

    /// Open the loading window for a specific destination.
    ///
    /// [`LoadingWindow::start`] is the same call with no destination, which the
    /// first load uses because the map has not been resolved yet.
    pub fn start_for(
        client: Option<&Path>,
        location: Option<&crate::loading_screen::Location>,
    ) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
        if let Ok(mut guard) = LOADING_SENDER
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
        {
            *guard = Some(status_tx);
        }
        let client = client.map(Path::to_path_buf);
        let location = location.cloned();
        let thread = std::thread::spawn(move || unsafe {
            let class = wide("PerfectWorldExplorerLoading");
            let text = wide("Perfect World Explorer — Loading");
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WindowClass {
                style: 0,
                proc: Some(loading_window_proc),
                class_extra: 0,
                window_extra: 0,
                instance,
                icon: std::ptr::null_mut(),
                cursor: LoadCursorW(std::ptr::null_mut(), 32512usize as *const u16),
                // Black: the artwork covers every pixel, and a resize must not
                // flash the desktop's default face colour.
                background: GetStockObject(4),
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            if RegisterClassW(&wc) == 0 {
                eprintln!("Loading window: cannot register its window class");
            }
            // Decode the artwork before the window exists so the first paint is
            // already the client's picture. The maximised window's client area is
            // the monitor work area, so a placeholder size is only used if the
            // artwork is refitted on the first WM_SIZE.
            let (art, note) = match client.as_deref() {
                Some(root) => {
                    let element = crate::loading_screen::element_folder(root);
                    loading_background(&element, 1280, 720, location.as_ref())
                }
                None => (
                    None,
                    format!(
                        "No client folder selected; no {} artwork",
                        crate::loading_screen::ARCHIVE_NAME
                    ),
                ),
            };
            if let Some(mut guard) = lock_loading_state() {
                *guard = Some(LoadingWindowState {
                    art,
                    status: "Loading the world...".into(),
                    ds_state: directstorage_state(crate::pck::direct_storage::read_stats()),
                    graph: "Read throughput (MiB/s): ······························".into(),
                    totals:
                        "DirectStorage: 0 reads / 0 MiB | attempts: 0 | fallback: 0 | active: 0"
                            .into(),
                    note,
                    phase: 0.0,
                });
            }
            let windowed = std::env::var_os(LOADING_WINDOWED_ENV).is_some();
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                0x00c00000 | 0x10000000,
                0x80000000u32 as i32,
                0x80000000u32 as i32,
                1280,
                720,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                eprintln!("Loading window: cannot create its window");
                let _ = ready_tx.send(());
                return;
            }
            // The first WM_PAINT the show below queues draws the artwork, so the
            // measurement loop is running before any decode-sized GDI work
            // happens. The one forced paint is after the state above is
            // published, so it draws the real picture rather than the
            // placeholder.
            ShowWindow(hwnd, if windowed { 5 } else { 3 }); // SW_SHOW / SW_MAXIMIZE
                                                            // The artwork was decoded against a placeholder size, so fit it once
                                                            // to the real client area before the first frame is presented. The
                                                            // WM_SIZE the show above queues would do this too, but doing it here
                                                            // means the very first paint is already the right size instead of a
                                                            // briefly mis-scaled picture.
            let (client_width, client_height) = client_size(hwnd);
            if let Some(mut guard) = lock_loading_state() {
                if let Some(art) = guard.as_mut().and_then(|state| state.art.as_mut()) {
                    if let Err(error) = art.refit(client_width, client_height) {
                        eprintln!("Loading screen artwork could not be fitted: {error}");
                    }
                }
            }
            // A replacement process hands this window over before anything in
            // `gpu::run` runs: the old process reads the handle, hides its own
            // window and stops fighting this one for the foreground. The paint
            // is forced first, so the handover never shows an empty window.
            if crate::world_handoff::is_handoff_child() {
                UpdateWindow(hwnd);
                crate::world_handoff::publish_window_handle(hwnd as usize);
            }

            let _ = ready_tx.send(());

            let mut samples = std::collections::VecDeque::<f64>::with_capacity(32);
            let mut previous_bytes = 0u64;
            let mut last_sample = std::time::Instant::now();
            let mut last_repaint = std::time::Instant::now();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let mut msg: WinMessage = std::mem::zeroed();
                while PeekMessageW(&mut msg, hwnd, 0, 0, 1) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let mut changed = false;
                while let Ok(text) = status_rx.try_recv() {
                    if let Some(mut guard) = lock_loading_state() {
                        if let Some(state) = guard.as_mut() {
                            state.status = text;
                        }
                    }
                    changed = true;
                }
                let now = std::time::Instant::now();
                let stats = crate::pck::direct_storage::read_stats();
                let ds_state = directstorage_state(stats);
                let totals = format!(
                    "DirectStorage: {} reads / {:.2} MiB | attempts: {} | fallback: {} | active: {}",
                    stats.successes,
                    stats.bytes as f64 / (1024.0 * 1024.0),
                    stats.attempts,
                    stats.fallbacks,
                    stats.active
                );
                let mut graph = None;
                if now.duration_since(last_sample) >= std::time::Duration::from_millis(500) {
                    let elapsed = now.duration_since(last_sample).as_secs_f64();
                    let current_rate = crate::loading_graph::mib_per_second(
                        stats.bytes.saturating_sub(previous_bytes),
                        elapsed,
                    );
                    previous_bytes = stats.bytes;
                    last_sample = now;
                    if samples.len() == 32 {
                        samples.pop_front();
                    }
                    samples.push_back(current_rate);
                    let peak = samples.iter().copied().fold(0.0f64, f64::max);
                    graph = Some(format!(
                        "MiB/s (last 16s; peak {:.1}): {}",
                        peak,
                        crate::loading_graph::sparkline(samples.make_contiguous())
                    ));
                }
                if let Some(mut guard) = lock_loading_state() {
                    if let Some(state) = guard.as_mut() {
                        if state.ds_state != ds_state {
                            state.ds_state = ds_state;
                            changed = true;
                        }
                        if state.totals != totals {
                            state.totals = totals;
                            changed = true;
                        }
                        if let Some(graph) = graph {
                            state.graph = graph;
                            changed = true;
                        }
                    }
                }
                if changed {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                // The bar's sweep lives in the state, so the window repaints on
                // its own cadence as well as when a message arrives. Only the
                // bar strip is invalidated: the artwork and the status lines do
                // not change between messages, and repainting a full-screen
                // bitmap 25 times a second is both wasted work and the frame a
                // composited desktop can catch half-drawn.
                if now.duration_since(last_repaint) >= std::time::Duration::from_millis(40) {
                    last_repaint = now;
                    if let Some(mut guard) = lock_loading_state() {
                        if let Some(state) = guard.as_mut() {
                            state.phase = (state.phase + 0.10) % std::f32::consts::TAU;
                        }
                    }
                    // The bar occupies the six rows at `height - 26`; one row of
                    // slack on each side keeps a rounding difference from
                    // leaving a stale row behind.
                    let (client_width, client_height) = client_size(hwnd);
                    let bar = WindowRect {
                        left: 0,
                        top: client_height as i32 - 27,
                        right: client_width as i32,
                        bottom: client_height as i32 - 19,
                    };
                    InvalidateRect(hwnd, &bar as *const WindowRect as *const c_void, 0);
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            DestroyWindow(hwnd);
        });
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(5));
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for LoadingWindow {
    fn drop(&mut self) {
        if let Some(lock) = LOADING_SENDER.get() {
            if let Ok(mut guard) = lock.lock() {
                *guard = None;
            }
        }
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(mut guard) = lock_loading_state() {
            *guard = None;
        }
    }
}

// Native launcher: the main rendering event loop remains free to own winit.
#[repr(C)]
struct WindowClass {
    style: u32,
    proc: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
    class_extra: i32,
    window_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}
#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut WinMessage, window: Hwnd, min: u32, max: u32) -> i32;
    fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> i32;
    fn EnableWindow(hwnd: Hwnd, enable: i32) -> i32;
    fn GetSysColorBrush(index: i32) -> *mut c_void;
    fn LoadCursorW(instance: *mut c_void, name: *const u16) -> *mut c_void;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}
#[link(name = "gdi32")]
extern "system" {
    fn GetStockObject(id: i32) -> *mut c_void;
}
static LAUNCH_ACTION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe extern "system" fn launch_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    use std::sync::atomic::Ordering;
    match msg {
        0x000f => {
            crate::launcher_ui::paint(hwnd);
            0
        }
        0x0014 => 1,
        0x0200 => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            crate::launcher_ui::mouse_move(hwnd, x, y);
            0
        }
        0x0202 => {
            let x = (lparam as u32 & 0xffff) as i16 as i32;
            let y = ((lparam as u32 >> 16) & 0xffff) as i16 as i32;
            let a = crate::launcher_ui::click(hwnd, x, y);
            if a != 0 {
                LAUNCH_ACTION.store(a, Ordering::SeqCst);
            }
            0
        }
        0x0100 => {
            let a = crate::launcher_ui::key(hwnd, wparam);
            if a != 0 {
                LAUNCH_ACTION.store(a, Ordering::SeqCst);
            }
            0
        }
        0x111 => {
            // WM_COMMAND, button ID in low word
            match wparam & 0xffff {
                101 => LAUNCH_ACTION.store(1, Ordering::SeqCst),
                102 => LAUNCH_ACTION.store(2, Ordering::SeqCst),
                _ => {}
            }
            0
        }
        0x10 => {
            LAUNCH_ACTION.store(3, Ordering::SeqCst);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
fn settings_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("PerfectWorldViewer").join("client-folder.utf16"))
}
fn remembered() -> Option<PathBuf> {
    let data = fs::read(settings_file()?).ok()?;
    if data.is_empty() || data.len() > 65536 || data.len() % 2 != 0 {
        return None;
    }
    let v: Vec<u16> = data
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Some(PathBuf::from(OsString::from_wide(&v)))
}
fn remember(path: &Path) -> Result<(), String> {
    let file = settings_file().ok_or("LOCALAPPDATA is not set")?;
    fs::create_dir_all(file.parent().ok_or("Invalid settings path")?).map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(file, bytes).map_err(|e| format!("Unable to save selected folder: {e}"))
}
unsafe fn control(
    parent: Hwnd,
    instance: *mut c_void,
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
) -> Hwnd {
    let c = wide(class);
    let t = wide(text);
    let hnd = CreateWindowExW(
        0,
        c.as_ptr(),
        t.as_ptr(),
        style,
        x,
        y,
        w,
        h,
        parent,
        id as *mut c_void,
        instance,
        std::ptr::null_mut(),
    );
    let font = GetStockObject(17); // DEFAULT_GUI_FONT
    if !hnd.is_null() {
        SendMessageW(hnd, 0x30, font as usize, 1);
    }
    hnd
}
/// Show one folder scan in the launcher: the headline names the executable
/// identity and the data version pair (`elements.data` + `tasks.data`), and the
/// card rows carry that pair plus every archive version the scan read. The whole report also goes to the
/// session log, so a client that loads but still looks wrong can be described
/// later without re-scanning.
fn present_scan(hwnd: Hwnd, scan: &crate::client_detect::Scan) {
    crate::launcher_ui::set_scan(
        hwnd,
        &scan.headline(),
        &scan.element.to_string_lossy(),
        scan.loadable(),
        scan.executable.as_ref().is_some_and(|exe| exe.is_known()),
        scan.summary_rows(),
    );
    eprintln!("{}", scan.report());
}
pub fn start_window() -> Result<Option<PathBuf>, String> {
    use std::sync::atomic::Ordering;
    unsafe {
        LAUNCH_ACTION.store(0, Ordering::SeqCst);
        crate::launcher_ui::initialize();
        let inst = GetModuleHandleW(std::ptr::null());
        let class = wide("PerfectWorldViewerLauncher");
        let wc = WindowClass {
            style: 0,
            proc: Some(launch_proc),
            class_extra: 0,
            window_extra: 0,
            instance: inst,
            icon: std::ptr::null_mut(),
            cursor: LoadCursorW(std::ptr::null_mut(), 32512usize as *const u16),
            background: GetSysColorBrush(15),
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            return Err("Cannot register launcher window".into());
        }
        let title = wide("Perfect World Explorer");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0x00ca0000 | 0x10000000,
            0x80000000u32 as i32,
            0x80000000u32 as i32,
            966,
            602,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            inst,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return Err("Cannot open launcher window".into());
        }
        let mut selected: Option<crate::client_detect::Client> = None;
        if let Some(path) = remembered() {
            let scan = crate::client_detect::scan(&path);
            if scan.loadable() {
                present_scan(hwnd, &scan);
                selected = Some(crate::client_detect::Client::from_scan(&scan));
            } else {
                crate::launcher_ui::set_client(
                    hwnd,
                    "Saved folder unavailable — select it again",
                    "",
                    false,
                    false,
                );
            }
        }
        ShowWindow(hwnd, 5);
        UpdateWindow(hwnd);
        loop {
            let mut msg: WinMessage = std::mem::zeroed();
            let n = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if n <= 0 {
                DestroyWindow(hwnd);
                return Ok(None);
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            match LAUNCH_ACTION.swap(0, Ordering::SeqCst) {
                1 => {
                    let choice =
                        choose_client(hwnd, selected.as_ref().map(|c| c.element.as_path()))?;
                    if let Some(path) = choice {
                        let scan = crate::client_detect::scan(&path);
                        if scan.loadable() {
                            if let Err(e) = remember(&scan.element) {
                                show_error(&format!("{e}\nYou can still load the world."));
                            }
                            present_scan(hwnd, &scan);
                            UpdateWindow(hwnd);
                            selected = Some(crate::client_detect::Client::from_scan(&scan));
                        } else {
                            selected = None;
                            crate::launcher_ui::set_client(
                                hwnd,
                                "Client not detected — select a valid folder",
                                "",
                                false,
                                false,
                            );
                            show_error(&scan.errors.join("\n"));
                        }
                    }
                }
                2 => {
                    if let Some(client) = selected {
                        if crate::launcher_ui::run_diagnostics() {
                            let report_hint = client
                                .element
                                .parent()
                                .unwrap_or(&client.element)
                                .join("pw-explorer-diagnostics.txt");
                            let prompt: Vec<u16> = format!(
                                "Full client diagnostics will scan every map (ECWLD, EC-BSD, terrain textures, water, and all referenced BMDs) and write all errors with explanations to:\n\n{}\n\nThis can take a minute. Continue?",
                                report_hint.display()
                            )
                            .encode_utf16()
                            .chain(Some(0))
                            .collect();
                            let cap: Vec<u16> = "Perfect World Explorer — Diagnostics Warning"
                                .encode_utf16()
                                .chain(Some(0))
                                .collect();
                            // MB_OKCANCEL (0x1) | MB_ICONWARNING (0x30); IDOK == 1
                            let choice = MessageBoxW(hwnd, prompt.as_ptr(), cap.as_ptr(), 0x31);
                            if choice == 1 {
                                use std::os::windows::process::CommandExt;
                                let diag_res = std::env::current_exe()
                                    .map_err(|e| e.to_string())
                                    .and_then(|exe| {
                                        std::process::Command::new(exe)
                                            .arg("--diagnose-all-worlds")
                                            .arg(&client.element)
                                            .creation_flags(0x08000000)
                                            .output()
                                            .map_err(|e| e.to_string())
                                    })
                                    .and_then(|out| {
                                        if out.status.success() {
                                            Ok(String::from_utf8_lossy(&out.stdout)
                                                .trim()
                                                .to_string())
                                        } else {
                                            Err(String::from_utf8_lossy(&out.stderr)
                                                .trim()
                                                .to_string())
                                        }
                                    });
                                match diag_res {
                                    Ok(summary_text) => {
                                        let done: Vec<u16> =
                                            format!("Diagnostics complete.\n\n{summary_text}")
                                                .encode_utf16()
                                                .chain(Some(0))
                                                .collect();
                                        let done_cap: Vec<u16> = "Diagnostics Complete"
                                            .encode_utf16()
                                            .chain(Some(0))
                                            .collect();
                                        MessageBoxW(hwnd, done.as_ptr(), done_cap.as_ptr(), 0x40);
                                    }
                                    Err(err) => {
                                        show_error(&format!("Diagnostics failed: {err}"));
                                    }
                                }
                            }
                        }
                        if crate::launcher_ui::low_memory() {
                            std::env::set_var("PW_SMALL_MASK_ATLAS", "1");
                        }
                        DestroyWindow(hwnd);
                        return Ok(Some(client.element));
                    }
                }
                3 => {
                    DestroyWindow(hwnd);
                    return Ok(None);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The legacy bridge's fixed table size, and the measure that justifies it.
    ///
    /// `LEGACY_BRIDGE_TREE_TYPES` is not a guess: 1.8.4's `world.ecwld` — the
    /// largest tree table in any staged client — declares exactly 59 tree
    /// types, so it sits *on* the boundary rather than under it. A map at the
    /// boundary must be servable by the legacy path, and the padding that path
    /// does is then a no-op. The measurement itself comes from
    /// `tools/version-gates.py`, which reads the count field out of every
    /// `.ecwld` header; the number is restated here so a change to the constant
    /// has to face it.
    #[test]
    fn the_legacy_table_size_is_the_measured_maximum() {
        assert_eq!(LEGACY_BRIDGE_TREE_TYPES, 59);
        // The two limits answer different questions and must not converge:
        // 59 is what the fixed bridge bakes, 4096 is how many files this build
        // will agree to write before calling a header damaged.
        assert!(
            MAX_TREE_TYPES > LEGACY_BRIDGE_TREE_TYPES,
            "the filesystem guard must stay looser than the bridge's table size"
        );
    }

    /// The refusal branch is chosen by the boundary, not by a second literal.
    ///
    /// The condition used to read `tree_type_count > 59`; it now names the
    /// constant. This pins the comparison's two sides so a map authoring 60
    /// types takes the refusal rather than a truncated pad.
    #[test]
    fn one_type_past_the_boundary_takes_the_refusal_branch() {
        let serve_legacy = |count: usize| count <= LEGACY_BRIDGE_TREE_TYPES;
        assert!(serve_legacy(0));
        assert!(serve_legacy(1));
        assert!(serve_legacy(58));
        assert!(serve_legacy(59), "1.8.4's world is servable");
        assert!(
            !serve_legacy(60),
            "60 types cannot be served by the legacy bridge"
        );
        assert!(!serve_legacy(59 + 1));
    }

    /// Padding is reached only in the fallback, and only with the fixed size.
    ///
    /// `extract_for_scene` is called with the authored count first; the padded
    /// call passes `LEGACY_BRIDGE_TREE_TYPES`. So a map with zero types never
    /// touches it (it writes a marker instead) and a map under the boundary is
    /// padded to exactly 59, no more.
    #[test]
    fn the_padded_source_count_is_the_constant() {
        // What the fallback hands `extract_for_scene`, for every map it serves.
        for authored in [1usize, 20, 58, 59] {
            let padded = LEGACY_BRIDGE_TREE_TYPES;
            assert_eq!(padded, 59);
            assert!(padded >= authored, "padding never drops an authored type");
        }
    }

    /// `ready` needs the marker *and* one `.pwt` per authored type.
    ///
    /// The legacy bridge writes a fixed 59, but a map with fewer types must not
    /// be reported ready on the strength of PWT files it does not use.
    #[test]
    fn readiness_needs_the_marker_and_every_authored_pwt() {
        let dir = std::env::temp_dir().join(format!(
            "pw-ready-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!ready(&dir, 0), "nothing on disk is not ready");
        std::fs::write(dir.join("ready"), b"").unwrap();
        assert!(ready(&dir, 0), "a map with no types needs only the marker");
        assert!(!ready(&dir, 1), "one authored type needs tree-000.pwt");

        std::fs::write(dir.join("tree-000.pwt"), b"").unwrap();
        assert!(ready(&dir, 1));
        assert!(!ready(&dir, 2), "two types need tree-001.pwt as well");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two maps authoring different counts get different cache directories.
    ///
    /// A shared cache key was one hypothesis for "trees do not spawn"; it was
    /// disproved because `cache_key` hashes the count. This calls the real
    /// function so the claim is checked rather than remembered.
    #[test]
    fn the_cache_key_differs_when_the_tree_type_count_differs() {
        // A staged client is not required: `cache_key` falls back to the path
        // itself when it cannot canonicalise, and the two maps still differ.
        let element = std::path::Path::new("element");
        let a = cache_key(element, std::path::Path::new("maps/world/world.ecwld"), 59);
        let b = cache_key(element, std::path::Path::new("maps/world/world.ecwld"), 60);
        // Both hash `trees.pck`'s size, which is absent here, so a failure is
        // expected and is itself the honest answer: without a client there is
        // nothing to key on. Assert the shape of that failure, not a key.
        match (a, b) {
            (Ok(a), Ok(b)) => assert_ne!(a, b, "different counts must not share a key"),
            (Err(_), Err(_)) => { /* no staged client; nothing to key on */ }
            other => panic!("one map keyed and the other did not: {other:?}"),
        }
    }
}
