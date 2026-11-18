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
    if tree_type_count > 10000 {
        return Err(format!(
            "{scene_path} declares {tree_type_count} tree types; the SpeedTree bridge supports at most 10000",
            scene_path = scene_path.display()
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
        // Keep a padded 59-type source table so the legacy checked-in bridge
        // still works when the selected map authors fewer types. Extra PWTs are
        // harmless: `Trees::open` reads only the scene's authored table.
        let source_count = if tree_type_count == 0 {
            0
        } else {
            tree_type_count.max(59)
        };
        crate::tree_source::extract_for_scene(element, scene_path, &data, source_count)?;
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
                if tree_type_count > 59 {
                    return Err(format!(
                        "{}\nThis map has {} tree types; rebuild tools/speedtree/speedtree-bake.exe from bake.cpp.",
                        bridge_error(&variable),
                        tree_type_count
                    ));
                }
                eprintln!("Variable-count SpeedTree bridge unavailable; using the legacy 59-type protocol.");
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
                expected_count = source_count;
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
fn directstorage_state(stats: crate::pck::direct_storage::ReadStats) -> String {
    if !crate::pck::direct_storage::compiled_in() {
        return "DirectStorage not built in; standard file I/O is in use".into();
    }
    if stats.successes > 0 {
        return format!(
            "Using DirectStorage in loading — ACTIVE ({} successful reads)",
            stats.successes
        );
    }
    if stats.fallbacks > 0 {
        return format!(
            "DirectStorage attempts fell back to standard I/O ({} so far)",
            stats.fallbacks
        );
    }
    if stats.active > 0 {
        return format!("DirectStorage read in progress ({} active)", stats.active);
    }
    "DirectStorage ready; waiting for eligible archive reads (64 KiB+)".into()
}

pub struct LoadingWindow {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl LoadingWindow {
    pub fn start() -> Self {
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
        let thread = std::thread::spawn(move || unsafe {
            let class = wide("STATIC");
            let text = wide("Perfect World Explorer — Loading");
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                0x00c00000 | 0x10000000,
                0x80000000u32 as i32,
                0x80000000u32 as i32,
                620,
                230,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            let (status_label, ds_state_label, ds_graph_label, ds_totals_label) = if hwnd.is_null()
            {
                (
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } else {
                let instance = GetModuleHandleW(std::ptr::null());
                let status = control(
                    hwnd,
                    instance,
                    "STATIC",
                    "Loading the world...",
                    0x50000000,
                    24,
                    20,
                    560,
                    25,
                    0,
                );
                control(
                    hwnd,
                    instance,
                    "STATIC",
                    "First launch may take several minutes while trees are prepared.",
                    0x50000000,
                    24,
                    51,
                    560,
                    25,
                    0,
                );
                let ds_state = control(
                    hwnd,
                    instance,
                    "STATIC",
                    "DirectStorage status is being checked...",
                    0x50000000,
                    24,
                    87,
                    560,
                    25,
                    0,
                );
                let ds_graph = control(
                    hwnd,
                    instance,
                    "STATIC",
                    "Read throughput (MiB/s): ······························",
                    0x50000000,
                    24,
                    115,
                    560,
                    28,
                    0,
                );
                let ds_totals = control(
                    hwnd,
                    instance,
                    "STATIC",
                    "DirectStorage: 0 reads / 0 MiB | attempts: 0 | fallback: 0 | active: 0",
                    0x50000000,
                    24,
                    148,
                    560,
                    38,
                    0,
                );
                ShowWindow(hwnd, 5);
                UpdateWindow(hwnd);
                (status, ds_state, ds_graph, ds_totals)
            };
            let _ = ready_tx.send(());

            let mut samples = std::collections::VecDeque::<f64>::with_capacity(32);
            let mut previous_bytes = 0u64;
            let mut last_sample = std::time::Instant::now();
            let mut last_state = String::new();
            let mut last_graph = String::new();
            let mut last_totals = String::new();

            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let mut msg: WinMessage = std::mem::zeroed();
                while PeekMessageW(&mut msg, hwnd, 0, 0, 1) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                while let Ok(text) = status_rx.try_recv() {
                    if !status_label.is_null() {
                        SetWindowTextW(status_label, wide(&text).as_ptr());
                    }
                }

                let now = std::time::Instant::now();
                let stats = crate::pck::direct_storage::read_stats();
                let state = directstorage_state(stats);
                if !ds_state_label.is_null() && state != last_state {
                    SetWindowTextW(ds_state_label, wide(&state).as_ptr());
                    last_state = state;
                }
                let totals = format!(
                    "DirectStorage: {} reads / {:.2} MiB | attempts: {} | fallback: {} | active: {}",
                    stats.successes,
                    stats.bytes as f64 / (1024.0 * 1024.0),
                    stats.attempts,
                    stats.fallbacks,
                    stats.active
                );
                if !ds_totals_label.is_null() && totals != last_totals {
                    SetWindowTextW(ds_totals_label, wide(&totals).as_ptr());
                    last_totals = totals;
                }

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
                    let graph = format!(
                        "MiB/s (last 16s; peak {:.1}): {}",
                        peak,
                        crate::loading_graph::sparkline(samples.make_contiguous())
                    );
                    if !ds_graph_label.is_null() && graph != last_graph {
                        SetWindowTextW(ds_graph_label, wide(&graph).as_ptr());
                        last_graph = graph;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            if !hwnd.is_null() {
                DestroyWindow(hwnd);
            }
        });
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(2));
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
    fn SendMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn SetTimer(hwnd: Hwnd, id: usize, interval: u32, callback: *const c_void) -> usize;
    fn KillTimer(hwnd: Hwnd, id: usize) -> i32;
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
        0x113 => {
            KillTimer(hwnd, 1);
            LAUNCH_ACTION.store(2, Ordering::SeqCst);
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
fn detection_label(c: &crate::client_detect::Client) -> &'static str {
    if c.known_142 {
        "Detected PW 1.4.2 Client"
    } else {
        "Unrecognized client executable — loading is experimental"
    }
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
            match crate::client_detect::detect(&path) {
                Ok(client) => {
                    crate::launcher_ui::set_client(
                        hwnd,
                        detection_label(&client),
                        &client.element.to_string_lossy(),
                        true,
                        client.known_142,
                    );
                    selected = Some(client);
                }
                Err(_) => crate::launcher_ui::set_client(
                    hwnd,
                    "Saved folder unavailable — select it again",
                    "",
                    false,
                    false,
                ),
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
                    KillTimer(hwnd, 1);
                    let choice =
                        choose_client(hwnd, selected.as_ref().map(|c| c.element.as_path()))?;
                    if let Some(path) = choice {
                        match crate::client_detect::detect(&path) {
                            Ok(client) => {
                                if let Err(e) = remember(&client.element) {
                                    show_error(&format!("{e}\nYou can still load the world."));
                                }
                                crate::launcher_ui::set_client(
                                    hwnd,
                                    detection_label(&client),
                                    &client.element.to_string_lossy(),
                                    true,
                                    client.known_142,
                                );
                                UpdateWindow(hwnd);
                                selected = Some(client);
                                // Keep the detection result visible before showing loading.
                                SetTimer(hwnd, 1, 1600, std::ptr::null());
                            }
                            Err(e) => {
                                selected = None;
                                crate::launcher_ui::set_client(
                                    hwnd,
                                    "Client not detected — select a valid folder",
                                    "",
                                    false,
                                    false,
                                );
                                show_error(&e);
                            }
                        }
                    }
                }
                2 => {
                    KillTimer(hwnd, 1);
                    if let Some(client) = selected {
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
