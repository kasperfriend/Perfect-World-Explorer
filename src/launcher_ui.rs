//! Custom-painted native Windows launcher. Artwork is embedded in the executable.
use std::{
    ffi::c_void,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
type Handle = *mut c_void;
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct Paint {
    dc: Handle,
    erase: i32,
    rect: Rect,
    restore: i32,
    update: i32,
    reserved: [u8; 32],
}
#[repr(C)]
struct BitmapInfo {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bits: u16,
    compression: u32,
    image_size: u32,
    xppm: i32,
    yppm: i32,
    colors: u32,
    important: u32,
}
#[link(name = "user32")]
extern "system" {
    fn BeginPaint(hwnd: Handle, ps: *mut Paint) -> Handle;
    fn EndPaint(hwnd: Handle, ps: *const Paint) -> i32;
    fn FillRect(dc: Handle, rect: *const Rect, brush: Handle) -> i32;
    fn InvalidateRect(hwnd: Handle, rect: *const Rect, erase: i32) -> i32;
}
#[link(name = "gdi32")]
extern "system" {
    fn StretchDIBits(
        dc: Handle,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        sx: i32,
        sy: i32,
        sw: i32,
        sh: i32,
        data: *const c_void,
        info: *const BitmapInfo,
        usage: u32,
        rop: u32,
    ) -> i32;
    fn CreateSolidBrush(color: u32) -> Handle;
    fn DeleteObject(obj: Handle) -> i32;
    fn SelectObject(dc: Handle, obj: Handle) -> Handle;
    fn SetTextColor(dc: Handle, color: u32) -> u32;
    fn SetBkMode(dc: Handle, mode: i32) -> i32;
    fn TextOutW(dc: Handle, x: i32, y: i32, text: *const u16, n: i32) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        esc: i32,
        orient: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike: u32,
        charset: u32,
        out: u32,
        clip: u32,
        quality: u32,
        pitch: u32,
        face: *const u16,
    ) -> Handle;
}
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}
struct Art {
    pixels: Vec<u8>,
    w: u32,
    h: u32,
}
static ART: OnceLock<Art> = OnceLock::new();
#[derive(Default)]
struct State {
    status: String,
    path: String,
    ready: bool,
    known: bool,
    low_memory: bool,
    hover: u8,
    focus: u8,
    /// Version rows from the folder scan: the game build, then the `tasks.data`
    /// and `elements.data` revisions. Exactly three, which is what the card has
    /// room for.
    rows: Vec<(String, String)>,
}
/// Where the launcher remembers its checkbox.
///
/// `windows_setup::settings_file` already owns `%LOCALAPPDATA%\PerfectWorldViewer`
/// for the remembered client folder; the toggle lives beside it in its own
/// file so neither write can truncate the other.
fn settings_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("PerfectWorldViewer").join("low-memory"))
}

/// The stored form of the toggle: exactly `b"1"` or `b"0"`.
///
/// Anything else is not a value this code wrote, so it reads as "not set"
/// rather than as false — the difference between a fresh install and a user
/// who deliberately turned the option off is what `None` is for, and a
/// half-written or hand-edited file must not silently pick one.
fn decode(raw: &[u8]) -> Option<bool> {
    match raw {
        b"1" => Some(true),
        b"0" => Some(false),
        _ => None,
    }
}

fn encode(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

fn load_low_memory() -> Option<bool> {
    decode(&fs::read(settings_file()?).ok()?)
}

fn save_low_memory(value: bool) -> Result<(), String> {
    let file = settings_file().ok_or("LOCALAPPDATA is not set")?;
    fs::create_dir_all(file.parent().ok_or("Invalid settings path")?)
        .map_err(|e| format!("Unable to save the low-memory setting: {e}"))?;
    fs::write(file, encode(value))
        .map_err(|e| format!("Unable to save the low-memory setting: {e}"))
}

/// Flip the toggle and persist it.
///
/// The in-memory state is updated first and the write failure is returned
/// rather than swallowed, so the caller can say the checkbox did not stick
/// instead of showing a tick that will be gone next launch.
fn toggle_low_memory(state: &mut State) -> Result<(), String> {
    let next = !state.low_memory;
    save_low_memory(next)?;
    state.low_memory = next;
    Ok(())
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}
pub fn initialize() {
    ART.get_or_init(|| {
        let mut image = image::load_from_memory(include_bytes!("../tools/launcher-art.jpg"))
            .expect("embedded launcher artwork")
            .to_rgba8();
        let (w, h) = image.dimensions();
        for (x, _, pixel) in image.enumerate_pixels_mut() {
            let left = (1.0 - (x as f32 / (w as f32 * 0.49))).clamp(0., 1.);
            let factor = 1.0 - left * 0.32;
            let [r, g, b, _] = pixel.0;
            pixel.0 = [
                (b as f32 * factor) as u8,
                (g as f32 * factor) as u8,
                (r as f32 * factor) as u8,
                255,
            ];
        }
        Art {
            pixels: image.into_raw(),
            w,
            h,
        }
    });
    restore_state();
}

/// The part of [`initialize`] that does not decode the artwork: reset the
/// status and focus, and restore the persisted toggle.
///
/// Split out so the restore is testable without the embedded JPEG, which needs
/// a real decoder (`image` is a stub in the offline harness); `initialize` is
/// just this plus `ART`.
fn restore_state() {
    if let Ok(mut s) = state().lock() {
        *s = State {
            status: "No client selected".into(),
            focus: 1,
            low_memory: load_low_memory().unwrap_or(false),
            ..Default::default()
        };
    }
}
pub fn set_client(hwnd: Handle, status: &str, path: &str, ready: bool, known: bool) {
    if let Ok(mut s) = state().lock() {
        s.status = status.into();
        s.path = path.into();
        s.ready = ready;
        s.known = known;
        s.rows.clear();
    }
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

/// Install the result of a full folder scan: the one-line status plus the
/// version rows drawn under it. The rows come from
/// [`crate::client_detect::Scan::summary_rows`], the same values
/// `--diagnose-client` prints, so the launcher and the diagnostic cannot drift
/// apart.
pub fn set_scan(
    hwnd: Handle,
    status: &str,
    path: &str,
    ready: bool,
    known: bool,
    rows: Vec<(String, String)>,
) {
    if let Ok(mut s) = state().lock() {
        s.status = status.into();
        s.path = path.into();
        s.ready = ready;
        s.known = known;
        s.rows = rows;
    }
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `LOCALAPPDATA` is process-global and libtest runs tests in parallel
    /// threads, so the three that set, remove and depend on it would race. This
    /// is not theoretical: the launcher suite passed on this machine and failed
    /// in CI on this exact step, because the ordering that happened to hold here
    /// did not hold there. One lock, taken by every test that touches the
    /// variable, makes the ordering explicit.
    static ENV: Mutex<()> = Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The stored form is exactly `1` or `0`, and nothing else is a value.
    #[test]
    fn only_the_two_written_bytes_decode() {
        assert_eq!(decode(b"1"), Some(true));
        assert_eq!(decode(b"0"), Some(false));
        assert_eq!(encode(true), b"1");
        assert_eq!(encode(false), b"0");
        // A fresh install has no file; a truncated or hand-edited one is not
        // evidence of a choice either way.
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"true"), None);
        assert_eq!(decode(b"01"), None);
        assert_eq!(decode(b"1\n"), None);
        assert_eq!(decode(b"\x00"), None);
    }

    #[test]
    fn every_written_value_reads_back_as_itself() {
        for value in [true, false] {
            assert_eq!(decode(encode(value)), Some(value), "{value}");
        }
    }

    /// Toggling persists: the value on disk after one click is the opposite of
    /// the value in memory before it.
    #[test]
    fn a_toggle_writes_the_new_value_and_updates_the_state() {
        let _guard = env_guard();
        let mut state = State::default();
        assert!(!state.low_memory);
        // `save_low_memory` needs `LOCALAPPDATA`; point it at a temp directory
        // so the test never touches the real profile.
        let dir = std::env::temp_dir().join("pw-launcher-toggle-test");
        fs::create_dir_all(&dir).unwrap();
        // SAFETY: this test is the only reader of the variable in this binary.
        unsafe { std::env::set_var("LOCALAPPDATA", &dir) };
        toggle_low_memory(&mut state).unwrap();
        assert!(state.low_memory);
        assert_eq!(load_low_memory(), Some(true));
        toggle_low_memory(&mut state).unwrap();
        assert!(!state.low_memory);
        assert_eq!(load_low_memory(), Some(false));
        // A fresh read from the same file agrees with the surviving state.
        assert_eq!(load_low_memory().unwrap(), state.low_memory);
        unsafe { std::env::remove_var("LOCALAPPDATA") };
        fs::remove_dir_all(&dir).ok();
    }

    /// `initialize()` is what puts the stored value back into the state, and it
    /// is the half a toggle test cannot see: a build could write the file on
    /// every click and still start unchecked.
    #[test]
    fn initialize_restores_the_stored_toggle() {
        let _guard = env_guard();
        let dir = std::env::temp_dir().join("pw-launcher-restore-test");
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("LOCALAPPDATA", &dir) };

        // Nothing stored yet: the checkbox starts clear.
        restore_state();
        assert!(!low_memory());

        save_low_memory(true).unwrap();
        restore_state();
        assert!(low_memory(), "a stored 1 must come back ticked");

        save_low_memory(false).unwrap();
        restore_state();
        assert!(!low_memory());

        // An unreadable value is not a choice, so it reads as clear.
        fs::write(settings_file().unwrap(), b"garbage").unwrap();
        restore_state();
        assert!(!low_memory());

        unsafe { std::env::remove_var("LOCALAPPDATA") };
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_environment_is_an_error_not_a_silent_default() {
        let _guard = env_guard();
        unsafe { std::env::remove_var("LOCALAPPDATA") };
        assert!(save_low_memory(true).is_err());
        assert_eq!(load_low_memory(), None);
    }
}

pub fn low_memory() -> bool {
    state().lock().map(|s| s.low_memory).unwrap_or(false)
}
fn hit(x: i32, y: i32, ready: bool) -> u8 {
    if (54..284).contains(&x) && (362..410).contains(&y) {
        1
    } else if ready && (298..450).contains(&x) && (362..410).contains(&y) {
        2
    } else if (54..420).contains(&x) && (444..474).contains(&y) {
        3
    } else {
        0
    }
}
pub fn mouse_move(hwnd: Handle, x: i32, y: i32) {
    if let Ok(mut s) = state().lock() {
        let h = hit(x, y, s.ready);
        if h != s.hover {
            s.hover = h;
            unsafe {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
    }
}
pub fn click(hwnd: Handle, x: i32, y: i32) -> usize {
    if let Ok(mut s) = state().lock() {
        let h = hit(x, y, s.ready);
        s.focus = h;
        match h {
            1 => return 1,
            2 => return 2,
            3 => {
                // A failed write must not leave the tick showing: the toggle
                // reverts and the status line says why.
                if let Err(e) = toggle_low_memory(&mut s) {
                    s.status = e;
                }
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            _ => {}
        }
    }
    0
}
pub fn key(hwnd: Handle, k: usize) -> usize {
    if let Ok(mut s) = state().lock() {
        match k {
            9 => {
                s.focus = match s.focus {
                    1 => {
                        if s.ready {
                            2
                        } else {
                            3
                        }
                    }
                    2 => 3,
                    _ => 1,
                };
                unsafe {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            13 | 32 => match s.focus {
                1 => return 1,
                2 if s.ready => return 2,
                3 => {
                    if let Err(e) = toggle_low_memory(&mut s) {
                        s.status = e;
                    }
                    unsafe {
                        InvalidateRect(hwnd, std::ptr::null(), 0);
                    }
                }
                _ => {}
            },
            27 => return 3,
            _ => {}
        }
    }
    0
}
unsafe fn rect(dc: Handle, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let r = Rect {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let b = CreateSolidBrush(color);
    FillRect(dc, &r, b);
    DeleteObject(b);
}
unsafe fn text(dc: Handle, x: i32, y: i32, s: &str, size: i32, weight: i32, color: u32) {
    let face: Vec<u16> = "Segoe UI".encode_utf16().chain(Some(0)).collect();
    let font = CreateFontW(
        -size,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        face.as_ptr(),
    );
    let old = SelectObject(dc, font);
    SetBkMode(dc, 1);
    SetTextColor(dc, color);
    let data: Vec<u16> = s.encode_utf16().collect();
    TextOutW(dc, x, y, data.as_ptr(), data.len() as i32);
    SelectObject(dc, old);
    DeleteObject(font);
}
/// Keep one label/value row inside the launcher card, ending with an ellipsis
/// so a truncated value is visibly truncated instead of silently wrong.
fn compact_line(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > width {
        format!(
            "{}…",
            chars[..width.saturating_sub(1)].iter().collect::<String>()
        )
    } else {
        text.into()
    }
}

fn compact(path: &str) -> String {
    let cs: Vec<char> = path.chars().collect();
    if cs.len() > 50 {
        format!("…{}", cs[cs.len() - 49..].iter().collect::<String>())
    } else {
        path.into()
    }
}
pub unsafe fn paint(hwnd: Handle) {
    let mut ps: Paint = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    if dc.is_null() {
        return;
    }
    let a = ART.get().unwrap();
    let info = BitmapInfo {
        size: 40,
        width: a.w as i32,
        height: -(a.h as i32),
        planes: 1,
        bits: 32,
        compression: 0,
        image_size: 0,
        xppm: 0,
        yppm: 0,
        colors: 0,
        important: 0,
    };
    StretchDIBits(
        dc,
        0,
        0,
        960,
        570,
        0,
        0,
        a.w as i32,
        a.h as i32,
        a.pixels.as_ptr().cast(),
        &info,
        0,
        0x00cc0020,
    );
    if let Ok(s) = state().lock() {
        text(
            dc,
            54,
            35,
            "P E R F E C T  W O R L D  /  E X P L O R E R",
            12,
            700,
            rgb(129, 199, 197),
        );
        rect(dc, 54, 65, 64, 2, rgb(92, 183, 178));
        text(dc, 50, 90, "The world is", 40, 600, rgb(244, 247, 242));
        text(
            dc,
            50,
            138,
            "yours to explore.",
            40,
            600,
            rgb(244, 247, 242),
        );
        text(
            dc,
            54,
            203,
            "A little closer to a perfect world.",
            16,
            400,
            rgb(183, 203, 206),
        );
        text(
            dc,
            54,
            228,
            "Fly freely through your own game client.",
            14,
            400,
            rgb(151, 177, 184),
        );
        // Client status card: low-contrast border and inset dark panel. It is
        // sized for the identity line, the folder path and exactly three version
        // rows (game, tasks, elements), which end 40 pixels above the card's
        // bottom edge so nothing is clipped or painted over the buttons.
        rect(dc, 54, 252, 397, 104, rgb(56, 94, 100));
        rect(dc, 55, 253, 395, 102, rgb(14, 36, 44));
        rect(
            dc,
            70,
            268,
            7,
            7,
            if s.ready && s.known {
                rgb(102, 211, 168)
            } else {
                rgb(206, 171, 112)
            },
        );
        let title = if s.ready && s.known {
            "DETECTED CLIENT"
        } else if s.ready {
            "DETECTED CLIENT (UNVERIFIED BUILD)"
        } else {
            "GAME CLIENT"
        };
        text(
            dc,
            86,
            260,
            title,
            11,
            700,
            if s.ready && s.known {
                rgb(135, 225, 191)
            } else {
                rgb(224, 192, 142)
            },
        );
        let desc = if s.path.is_empty() {
            s.status.clone()
        } else {
            compact(&s.path)
        };
        text(dc, 70, 283, &desc, 12, 400, rgb(190, 209, 210));
        // Version rows: the game build, then the two data revisions that move
        // independently of it. They are what makes a client other than the
        // inspected 1.4.2 legible instead of "unknown".
        let mut y = 306;
        for (label, value) in s.rows.iter().take(3) {
            let line = if value.is_empty() {
                label.clone()
            } else {
                format!("{label}  {value}")
            };
            text(
                dc,
                70,
                y,
                &compact_line(&line, 76),
                11,
                400,
                rgb(151, 183, 186),
            );
            y += 14;
        }
        let c = if s.hover == 1 || s.focus == 1 {
            rgb(134, 218, 205)
        } else {
            rgb(105, 194, 181)
        };
        rect(dc, 54, 362, 230, 48, c);
        text(dc, 76, 374, "Select Game Folder", 15, 650, rgb(9, 40, 44));
        text(dc, 252, 374, "›", 18, 600, rgb(9, 40, 44));
        if s.ready {
            let c = if s.hover == 2 || s.focus == 2 {
                rgb(41, 88, 93)
            } else {
                rgb(24, 61, 67)
            };
            rect(dc, 298, 362, 152, 48, rgb(92, 151, 148));
            rect(dc, 299, 363, 150, 46, c);
            text(dc, 321, 374, "Enter world", 15, 600, rgb(224, 243, 239));
        }
        rect(dc, 54, 448, 16, 16, rgb(112, 166, 165));
        rect(dc, 55, 449, 14, 14, rgb(12, 39, 46));
        if s.low_memory {
            text(dc, 56, 445, "✓", 14, 700, rgb(140, 224, 201));
        }
        text(
            dc,
            80,
            446,
            "Lower-memory terrain masks",
            13,
            400,
            rgb(183, 206, 207),
        );
        text(
            dc,
            54,
            505,
            "CLIENT ASSETS REQUIRED",
            10,
            700,
            rgb(121, 160, 165),
        );
        text(
            dc,
            54,
            526,
            "Select a trusted Perfect World client folder.",
            11,
            400,
            rgb(133, 165, 170),
        );
        text(
            dc,
            755,
            528,
            "EXPLORER  /  2026.11",
            10,
            600,
            rgb(202, 225, 221),
        );
    }
    EndPaint(hwnd, &ps);
}
