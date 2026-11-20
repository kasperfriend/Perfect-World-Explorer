//! The loading UI as a frame the viewer can draw itself.
//!
//! The standalone loading window in [`crate::windows_setup`] paints its picture
//! and overlay with GDI into its own Win32 surface. Changing worlds from the
//! Teleport List cannot reuse that window: the replacement world is a separate
//! process, and the old window must keep presenting the old world's last frame
//! until the new one is ready. This module therefore produces the *same* UI —
//! the client's picture, the DirectStorage readout, the read-throughput graph,
//! the read totals, the indeterminate bar and the status line — as a packed
//! RGBA frame that the existing egui overlay uploads as a texture.
//!
//! Everything here is pure and takes its inputs as arguments, so the geometry
//! and the fallback rules are unit-testable without a Windows desktop, a GPU or
//! a game client.

// `super` resolves to the viewer crate when this module is compiled as part of
// it, and to the check binary's own `loading_screen` module when the headless
// check includes this file, so both builds share one implementation.
use super::loading_screen;

/// Palette and geometry, kept together so the window and the in-viewer overlay
/// cannot drift apart.
pub const BACKGROUND: [u8; 4] = [12, 24, 30, 255];
/// Height of the darkened strip the overlay text sits in.
pub const STRIP_HEIGHT: u32 = 178;
/// How much of the strip's top is a fade rather than flat darkening.
pub const FADE_HEIGHT: u32 = 72;
/// Peak alpha of the darkening at the bottom of the strip, 0..=255.
pub const MAX_ALPHA: u8 = 199;
/// The indeterminate bar's travel, in pixels.
pub const BAR_THICKNESS: u32 = 6;

/// What the overlay says, sampled once per frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The DirectStorage state line.
    pub direct_storage: String,
    /// One line of read-throughput history, newest last.
    pub graph: String,
    /// Totals: reads, MiB, attempts, fallbacks, active.
    pub totals: String,
    /// The status text for this load.
    pub message: String,
    /// Bar sweep position in radians; advanced by the animation tick.
    pub phase: f32,
    /// Which picture this frame is for, shown in the corner.
    pub picture: String,
}

/// The text the overlay draws, in draw order, as `(row, text)` pairs.
///
/// Rows are counted from the top of the strip. Pulling the layout out of the
/// rasteriser means the in-viewer overlay and the standalone window can be
/// checked against the same list.
pub fn lines(status: &Status, height: u32) -> Vec<(i32, String)> {
    let strip_top = height.saturating_sub(STRIP_HEIGHT) as i32;
    vec![
        (strip_top + 14, status.direct_storage.clone()),
        (strip_top + 34, status.graph.clone()),
        (strip_top + 54, status.totals.clone()),
        (strip_top + 82, status.message.clone()),
        (
            height.saturating_sub(48) as i32,
            format!("Picture: {}", status.picture),
        ),
    ]
}

/// Where the bar's filled segment starts and ends, in pixels, for `phase`.
///
/// A sine sweep keeps the bar reading as "working, unknown length" without ever
/// looking stuck at either end. The segment is a fixed fraction of the width and
/// slides across it, clipped so it never leaves the track.
pub fn bar_segment(width: u32, phase: f32) -> (u32, u32) {
    if width == 0 {
        return (0, 0);
    }
    let span = (width as f32 * 0.28).max(1.0);
    let travel = (width as f32 - span).max(0.0);
    let position = (0.5 - 0.5 * phase.cos()) * travel;
    let start = position.round().clamp(0.0, travel) as u32;
    let end = (start + span as u32).min(width);
    (start, end)
}

/// Render the whole loading UI into a tightly packed RGBA8 frame.
///
/// `picture` is the client's artwork already cover-fitted to `width` x
/// `height`; `None` keeps the plain background and shows `status.message`
/// over it, which is what a client with no usable `surfaces.pck` looks like.
/// The returned bytes are row-major, four bytes per pixel, top row first.
pub fn compose(
    picture: Option<&image::RgbaImage>,
    width: u32,
    height: u32,
    status: &Status,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("loading overlay target has zero dimensions".into());
    }
    let mut frame = match picture {
        Some(image) => {
            if image.dimensions() != (width, height) {
                return Err(format!(
                    "loading picture is {}x{}, expected {width}x{height}",
                    image.width(),
                    image.height()
                ));
            }
            image.as_raw().clone()
        }
        None => {
            let mut frame = vec![0u8; width as usize * height as usize * 4];
            for pixel in frame.chunks_exact_mut(4) {
                pixel.copy_from_slice(&BACKGROUND);
            }
            frame
        }
    };
    // Darken the bottom strip so the text stays legible over any artwork, the
    // same blend the standalone window pre-bakes into its bitmap.
    loading_screen::overlay_strip_in_place(
        &mut frame,
        width,
        height,
        STRIP_HEIGHT,
        FADE_HEIGHT,
        MAX_ALPHA as f32 / 255.0,
    )?;
    draw_bar(&mut frame, width, height, status.phase)?;
    Ok(frame)
}

/// Alpha-blend one rectangle into a packed RGBA frame.
///
/// The bar's trough is drawn as a translucent white so the client's picture
/// stays visible under it; the moving segment is nearly opaque so it reads at a
/// glance. Blending rather than overwriting is what keeps the bar from looking
/// like an unrelated grey box on top of the artwork.
fn fill(frame: &mut [u8], width: u32, x: u32, y: u32, w: u32, h: u32, colour: [u8; 4]) {
    let alpha = colour[3] as f32 / 255.0;
    for row in y..y.saturating_add(h) {
        let start = (row as usize * width as usize + x as usize) * 4;
        let end = start + w as usize * 4;
        if end > frame.len() || start >= end {
            continue;
        }
        for pixel in frame[start..end].chunks_exact_mut(4) {
            for channel in 0..3 {
                pixel[channel] =
                    (colour[channel] as f32 * alpha + pixel[channel] as f32 * (1.0 - alpha)) as u8;
            }
            pixel[3] = 255;
        }
    }
}

/// Draw the bar trough and its moving segment.
fn draw_bar(frame: &mut [u8], width: u32, height: u32, phase: f32) -> Result<(), String> {
    let inset = 18u32.min(width / 4);
    if width <= inset * 2 || height < BAR_THICKNESS {
        return Ok(());
    }
    let track_width = width - inset * 2;
    let track_y = height - 26;
    // Trough, then the moving fill clipped to the trough.
    fill(
        frame,
        width,
        inset,
        track_y,
        track_width,
        BAR_THICKNESS,
        [255, 255, 255, 38],
    );
    let (start, end) = bar_segment(track_width, phase);
    if end > start {
        fill(
            frame,
            width,
            inset + start,
            track_y,
            end - start,
            BAR_THICKNESS,
            [255, 214, 120, 235],
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status {
        Status {
            direct_storage: "DirectStorage ready".into(),
            graph: "Read throughput (MiB/s): ...".into(),
            totals: "DirectStorage: 0 reads".into(),
            message: "Loading world a01...".into(),
            phase: 0.0,
            picture: "surfaces/x.jpg".into(),
        }
    }

    #[test]
    fn the_bar_sweeps_without_leaving_its_track() {
        for step in 0..64 {
            let phase = step as f32 / 64.0 * std::f32::consts::TAU;
            let (start, end) = bar_segment(1000, phase);
            assert!(
                end <= 1000,
                "phase {phase} ran off the track: {start}..{end}"
            );
            assert!(end >= start, "phase {phase} inverted: {start}..{end}");
            assert!(start < 1000, "phase {phase} stuck at the end: {start}");
        }
        // Both extremes are reachable, so the sweep never looks frozen.
        assert_eq!(bar_segment(1000, 0.0).0, 0);
        assert!(bar_segment(1000, std::f32::consts::PI).0 > 0);
    }

    #[test]
    fn a_zero_width_track_produces_no_segment() {
        assert_eq!(bar_segment(0, 1.0), (0, 0));
    }

    #[test]
    fn composing_without_a_picture_fills_the_background_and_keeps_the_bar() {
        let (w, h) = (200, 300);
        let frame = compose(None, w, h, &status()).expect("compose");
        assert_eq!(frame.len(), w as usize * h as usize * 4);
        // The top-left pixel is untouched by the strip, so it is the background.
        assert_eq!(&frame[0..4], &BACKGROUND);
        // The trough is drawn as a translucent white row, so the bar row differs
        // from the same columns a few rows above it inside the same strip.
        let stride = w as usize * 4;
        let bar_y = (h - 26) as usize;
        let above_y = bar_y - 4;
        let differs = (0..w as usize).any(|x| {
            frame[bar_y * stride + x * 4..bar_y * stride + x * 4 + 3]
                != frame[above_y * stride + x * 4..above_y * stride + x * 4 + 3]
        });
        assert!(differs, "no bar trough was drawn");
    }

    #[test]
    fn composing_rejects_a_picture_of_the_wrong_size() {
        let picture = image::RgbaImage::new(10, 10);
        assert!(compose(Some(&picture), 20, 20, &status()).is_err());
    }

    #[test]
    fn the_status_lines_sit_inside_the_strip_and_above_the_bar() {
        let h = 720u32;
        let rows: Vec<i32> = lines(&status(), h)
            .into_iter()
            .map(|(row, _)| row)
            .collect();
        let strip_top = (h - STRIP_HEIGHT) as i32;
        assert!(rows[0] >= strip_top, "{} before {strip_top}", rows[0]);
        // The bar occupies height-26 .. height-20; the message must clear it.
        assert!(rows[3] < (h - 26) as i32, "message overlaps the bar");
        assert_eq!(rows[4], h as i32 - 48);
    }
}
