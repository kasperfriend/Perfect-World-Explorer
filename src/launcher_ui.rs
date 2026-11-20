//! Custom-painted native Windows launcher. Artwork is embedded in the executable.
use std::{
    ffi::c_void,
    sync::{Mutex, OnceLock},
};
type Handle = *mut c_void;
#[repr(C)]
pub(crate) struct Rect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}
/// `PAINTSTRUCT`, shared with the loading window's paint handler.
#[repr(C)]
pub(crate) struct Paint {
    pub(crate) dc: Handle,
    pub(crate) erase: i32,
    pub(crate) rect: Rect,
    pub(crate) restore: i32,
    pub(crate) update: i32,
    pub(crate) reserved: [u8; 32],
}
/// `BITMAPINFOHEADER`, the header the loading window prepends to its pixels.
#[repr(C)]
pub(crate) struct BitmapInfo {
    pub(crate) size: u32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) planes: u16,
    pub(crate) bits: u16,
    pub(crate) compression: u32,
    pub(crate) image_size: u32,
    pub(crate) xppm: i32,
    pub(crate) yppm: i32,
    pub(crate) colors: u32,
    pub(crate) important: u32,
}
#[link(name = "user32")]
extern "system" {
    pub(crate) fn BeginPaint(hwnd: Handle, ps: *mut Paint) -> Handle;
    pub(crate) fn EndPaint(hwnd: Handle, ps: *const Paint) -> i32;
    fn FillRect(dc: Handle, rect: *const Rect, brush: Handle) -> i32;
    fn InvalidateRect(hwnd: Handle, rect: *const Rect, erase: i32) -> i32;
}
/// GDI text extent, in pixels.
#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}

/// Fonts the paint path reuses, keyed by `(height, weight)`.
///
/// The loading window repaints about 25 times a second. Creating and deleting a
/// font per text line per frame churned roughly a hundred GDI objects a second
/// through the process handle table, and a repaint that happens to fail while a
/// GDI operation cannot be satisfied is exactly what composited windows render as
/// a black frame. Holding a handful of fonts for the process lifetime removes
/// that churn: a GDI font is a small kernel object, and this process has four
/// distinct sizes.
///
/// Handles are stored as `usize` because raw pointers are not `Send` and a
/// `static Mutex` needs its contents to be.
static FONTS: Mutex<Vec<((i32, i32), usize)>> = Mutex::new(Vec::new());

/// Solid brushes the paint path reuses, keyed by colour.
///
/// Same reasoning as [`FONTS`]: the loading window fills two or three rectangles
/// per frame, and creating a brush for each is needless handle churn on a path
/// that runs continuously while the world loads.
static BRUSHES: Mutex<Vec<(u32, usize)>> = Mutex::new(Vec::new());

/// The process-lifetime font for `size`/`weight`, created on first use.
fn font(size: i32, weight: i32) -> Handle {
    if let Ok(mut fonts) = FONTS.lock() {
        if let Some((_, handle)) = fonts.iter().find(|(key, _)| *key == (size, weight)) {
            return *handle as Handle;
        }
        let face: Vec<u16> = "Segoe UI".encode_utf16().chain(Some(0)).collect();
        // SAFETY: `face` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFontW(
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
            )
        };
        if !handle.is_null() {
            fonts.push(((size, weight), handle as usize));
        }
        return handle;
    }
    std::ptr::null_mut()
}

#[link(name = "gdi32")]
extern "system" {
    fn GetTextExtentPoint32W(dc: Handle, text: *const u16, count: i32, size: *mut Size) -> i32;
    pub(crate) fn StretchDIBits(
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
    pub(crate) fn DeleteObject(obj: Handle) -> i32;
    pub(crate) fn SelectObject(dc: Handle, obj: Handle) -> Handle;
    /// Off-screen device context and its surface, used to compose a frame
    /// before presenting it in one `BitBlt`.
    pub(crate) fn CreateCompatibleDC(dc: Handle) -> Handle;
    pub(crate) fn CreateCompatibleBitmap(dc: Handle, width: i32, height: i32) -> Handle;
    pub(crate) fn DeleteDC(dc: Handle) -> i32;
    pub(crate) fn BitBlt(
        dst: Handle,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        src: Handle,
        src_x: i32,
        src_y: i32,
        rop: u32,
    ) -> i32;
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
    if let Ok(mut s) = state().lock() {
        *s = State {
            status: "No client selected".into(),
            focus: 1,
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
    }
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
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
                s.low_memory = !s.low_memory;
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
                    s.low_memory = !s.low_memory;
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
/// The launcher's accent colour, shared with the loading window's overlay.
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}
/// Fill one rectangle in a device context. The loading window paints its
/// status strip with the launcher's own GDI helpers so both native windows use
/// one text and rectangle path.
pub unsafe fn fill_rect(dc: Handle, x: i32, y: i32, w: i32, h: i32, color: u32) {
    rect(dc, x, y, w, h, color)
}
/// Draw one line of text with the launcher's font and colours.
pub unsafe fn draw_text(dc: Handle, x: i32, y: i32, s: &str, size: i32, weight: i32, color: u32) {
    text(dc, x, y, s, size, weight, color);
}
/// Width of `s` at `size` points in the launcher font.
pub unsafe fn text_width(dc: Handle, s: &str, size: i32) -> i32 {
    let font = font(size, 400);
    if font.is_null() {
        return 0;
    }
    let old = SelectObject(dc, font);
    let mut extent = Size { cx: 0, cy: 0 };
    let data: Vec<u16> = s.encode_utf16().collect();
    let measured = GetTextExtentPoint32W(dc, data.as_ptr(), data.len() as i32, &mut extent);
    SelectObject(dc, old);
    // A failed measurement must not turn a right-aligned label into a negative
    // offset; 0 keeps it at the caller's left margin.
    if measured == 0 {
        0
    } else {
        extent.cx.max(0)
    }
}
unsafe fn rect(dc: Handle, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let r = Rect {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    // SAFETY: `CreateSolidBrush` is only called with a plain `COLORREF`.
    let b = unsafe { brush(color) };
    if b.is_null() {
        return;
    }
    FillRect(dc, &r, b);
}

/// The process-lifetime solid brush for `color`, created on first use.
fn brush(color: u32) -> Handle {
    if let Ok(mut brushes) = BRUSHES.lock() {
        if let Some((_, handle)) = brushes.iter().find(|(key, _)| *key == color) {
            return *handle as Handle;
        }
        // SAFETY: a solid brush has no aliasing requirements.
        let handle = unsafe { CreateSolidBrush(color) };
        if !handle.is_null() {
            brushes.push((color, handle as usize));
        }
        return handle;
    }
    std::ptr::null_mut()
}
unsafe fn text(dc: Handle, x: i32, y: i32, s: &str, size: i32, weight: i32, color: u32) {
    let font = font(size, weight);
    if font.is_null() {
        return;
    }
    let old = SelectObject(dc, font);
    SetBkMode(dc, 1);
    SetTextColor(dc, color);
    let data: Vec<u16> = s.encode_utf16().collect();
    TextOutW(dc, x, y, data.as_ptr(), data.len() as i32);
    SelectObject(dc, old);
    DeleteObject(font);
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
        // Client status card: low-contrast border and inset dark panel.
        rect(dc, 54, 276, 397, 64, rgb(56, 94, 100));
        rect(dc, 55, 277, 395, 62, rgb(14, 36, 44));
        rect(
            dc,
            70,
            292,
            7,
            7,
            if s.ready && s.known {
                rgb(102, 211, 168)
            } else {
                rgb(206, 171, 112)
            },
        );
        let title = if s.ready && s.known {
            "DETECTED PW 1.4.2 CLIENT"
        } else if s.ready {
            "UNRECOGNIZED CLIENT BUILD"
        } else {
            "GAME CLIENT"
        };
        text(
            dc,
            86,
            284,
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
        text(dc, 70, 307, &desc, 12, 400, rgb(190, 209, 210));
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
