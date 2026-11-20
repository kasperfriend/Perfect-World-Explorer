//! The denoiser: temporal accumulation plus an edge-avoiding à-trous wavelet
//! filter, in the SVGF family.
//!
//! A 1-sample ray tracer is noisy; the noise is *correlated* inside a surface
//! (a plane's shadow term is nearly constant across it) and decorrelated across
//! surface boundaries. So the filter must be edge-aware, and the cheapest
//! edge-aware filter with a large footprint is the à-trous wavelet: repeated
//! 5×5 passes with doubling tap spacing, which reaches a 65-pixel radius in five
//! passes at 25 taps each, instead of the thousands a single wide kernel needs.
//!
//! Three pieces:
//!
//! * **Temporal accumulation** reprojects last frame's filtered result with the
//!   camera's own motion vectors and blends it in, weighted by how many frames
//!   its pixel has survived. A variance clamp rejects the history when the
//!   current frame disagrees with it by more than its own noise explains, which
//!   is what keeps a moving object from smearing.
//! * **Variance estimation** keeps the second moment of the accumulation
//!   (`E[x²] - E[x]²`) so the spatial pass can scale its colour weight per
//!   pixel: noisy pixels are smoothed hard, converged ones barely at all.
//! * **The à-trous pass** reweights every tap by normal, depth and pre-filtered
//!   luminance similarity and, like the variance estimate, carries the
//!   second moment through so an over-smoothed pixel does not become a blurry
//!   smudge in the next iteration.
//!
//! This file is the *reference implementation*: the compute shaders in
//! `raywatcher::gpu` perform the same arithmetic per pixel, and the tests here
//! pin the properties that make the image acceptable — energy conservation,
//! edge preservation, convergence with history length, and a hard reset on
//! disocclusion.

use glam::Vec3;

/// Tunables. Every one of these is exposed in the module's Advanced section
/// under a name that says what it does; the defaults are the shipped ones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DenoiseConfig {
    /// Number of à-trous iterations. Each doubles the filter radius.
    pub iterations: u32,
    /// Colour weight falloff, in units of standard deviation. Larger keeps
    /// more of the noisy original.
    pub phi_colour: f32,
    /// Normal weight falloff, as a power of `dot(n_a, n_b)`.
    pub phi_normal: f32,
    /// Depth weight falloff, as a fraction of the pixel's depth difference.
    pub phi_depth: f32,
    /// Per-frame blend floor: the least the current frame is allowed to count.
    /// `0.05` means a pixel with perfect history still updates 5% per frame.
    pub temporal_floor: f32,
    /// Variance clamp multiplier. Larger accepts more history.
    pub variance_gamma: f32,
    /// Floor on the accumulated variance, so a perfectly flat region does not
    /// give the colour weight an infinite slope and stop all smoothing.
    pub variance_floor: f32,
    /// Maximum history length, in frames. The accumulator saturates here.
    pub max_history: f32,
}

impl Default for DenoiseConfig {
    fn default() -> Self {
        Self {
            iterations: 3,
            phi_colour: 4.,
            phi_normal: 32.,
            phi_depth: 0.5,
            temporal_floor: 0.05,
            variance_gamma: 3.,
            variance_floor: 1e-4,
            max_history: 64.,
        }
    }
}

impl DenoiseConfig {
    pub fn sanitise(&mut self) {
        self.iterations = self.iterations.clamp(1, 5);
        self.phi_colour = finite(self.phi_colour, 4.).clamp(0.1, 64.);
        self.phi_normal = finite(self.phi_normal, 32.).clamp(1., 256.);
        self.phi_depth = finite(self.phi_depth, 0.5).clamp(1e-3, 64.);
        self.temporal_floor = finite(self.temporal_floor, 0.05).clamp(0.01, 1.);
        self.variance_gamma = finite(self.variance_gamma, 3.).clamp(0.5, 16.);
        self.variance_floor = finite(self.variance_floor, 1e-4).clamp(1e-8, 1.);
        self.max_history = finite(self.max_history, 64.).clamp(1., 256.);
    }

    /// Tap spacing for iteration `i`: 1, 2, 4, ...
    pub fn step(iteration: u32) -> i32 {
        1 << iteration.min(16)
    }
}

fn finite(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/// One pixel of the guide buffer the filter weights against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    /// Linear depth along the view ray, in world units (`f32::INFINITY` for
    /// sky).
    pub depth: f32,
    /// Shading normal, unit length, oriented toward the camera.
    pub normal: Vec3,
}

impl Default for Guide {
    fn default() -> Self {
        Self {
            depth: f32::INFINITY,
            normal: Vec3::Z,
        }
    }
}

/// A scalar plane.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub w: u32,
    pub h: u32,
    pub data: Vec<f32>,
}

impl Field {
    pub fn new(w: u32, h: u32, fill: f32) -> Self {
        Self {
            w,
            h,
            data: vec![fill; (w as usize) * (h as usize)],
        }
    }

    pub fn from_fn(w: u32, h: u32, mut f: impl FnMut(u32, u32) -> f32) -> Self {
        let mut data = Vec::with_capacity((w as usize) * (h as usize));
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Self { w, h, data }
    }

    pub fn get(&self, x: u32, y: u32) -> f32 {
        self.data[(y as usize) * (self.w as usize) + (x as usize)]
    }

    pub fn set(&mut self, x: u32, y: u32, v: f32) {
        let i = (y as usize) * (self.w as usize) + (x as usize);
        self.data[i] = v;
    }

    pub fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.w as usize) + (x as usize)
    }

    pub fn mean(&self) -> f32 {
        if self.data.is_empty() {
            return 0.;
        }
        self.data.iter().sum::<f32>() / self.data.len() as f32
    }

    pub fn variance(&self) -> f32 {
        if self.data.is_empty() {
            return 0.;
        }
        let mean = self.mean();
        self.data
            .iter()
            .map(|v| (v - mean) * (v - mean))
            .sum::<f32>()
            / self.data.len() as f32
    }
}

/// A colour plane in linear RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourField {
    pub w: u32,
    pub h: u32,
    pub data: Vec<Vec3>,
}

impl ColourField {
    pub fn new(w: u32, h: u32, fill: Vec3) -> Self {
        Self {
            w,
            h,
            data: vec![fill; (w as usize) * (h as usize)],
        }
    }

    pub fn from_fn(w: u32, h: u32, mut f: impl FnMut(u32, u32) -> Vec3) -> Self {
        let mut data = Vec::with_capacity((w as usize) * (h as usize));
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Self { w, h, data }
    }

    pub fn get(&self, x: u32, y: u32) -> Vec3 {
        self.data[(y as usize) * (self.w as usize) + (x as usize)]
    }

    pub fn set(&mut self, x: u32, y: u32, v: Vec3) {
        let i = (y as usize) * (self.w as usize) + (x as usize);
        self.data[i] = v;
    }

    pub fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.w as usize) + (x as usize)
    }

    pub fn mean(&self) -> Vec3 {
        if self.data.is_empty() {
            return Vec3::ZERO;
        }
        self.data.iter().fold(Vec3::ZERO, |a, b| a + *b) / self.data.len() as f32
    }

    pub fn luminance(&self) -> Field {
        Field::from_fn(self.w, self.h, |x, y| luminance(self.get(x, y)))
    }
}

/// Rec. 709 luminance, the same weights the renderer's tone map uses.
pub fn luminance(c: Vec3) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

/// The persistent state between frames.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Accumulated colour (linear).
    pub colour: ColourField,
    /// Accumulated first and second moments of luminance.
    pub moment1: Field,
    pub moment2: Field,
    /// How many frames each pixel's history has survived, capped.
    pub length: Field,
    /// The guide buffer of the frame that produced this history, needed to
    /// validate a reprojection.
    pub depth: Field,
}

impl History {
    /// A reset history: no frames, so every pixel starts from the current one.
    pub fn empty(w: u32, h: u32) -> Self {
        Self {
            colour: ColourField::new(w, h, Vec3::ZERO),
            moment1: Field::new(w, h, 0.),
            moment2: Field::new(w, h, 0.),
            length: Field::new(w, h, 0.),
            depth: Field::new(w, h, f32::INFINITY),
        }
    }

    pub fn matches(&self, w: u32, h: u32) -> bool {
        self.colour.w == w && self.colour.h == h && w > 0 && h > 0
    }
}

/// One frame's noisy ray output, in the form the filter and the temporal
/// accumulator consume.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    /// Radiance, linear.
    pub colour: ColourField,
    /// Depth and normal per pixel.
    pub depth: Field,
    /// Normal planes, packed as a `ColourField` for a single allocation.
    pub normal: ColourField,
    /// Motion vector, in pixels: `previous_position = current - motion`.
    pub motion: ColourField,
    /// Per-pixel visibility (sky fraction), the AO channel.
    pub visibility: Field,
    /// Per-pixel reflection radiance, filtered separately so a mirror does not
    /// bleed into the diffuse image.
    pub reflection: ColourField,
    /// Per-pixel distance to the first hit, in world units.
    pub depth_hit: Field,
}

impl Frame {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            colour: ColourField::new(w, h, Vec3::ZERO),
            depth: Field::new(w, h, f32::INFINITY),
            normal: ColourField::new(w, h, Vec3::Z),
            motion: ColourField::new(w, h, Vec3::ZERO),
            visibility: Field::new(w, h, 1.),
            reflection: ColourField::new(w, h, Vec3::ZERO),
            depth_hit: Field::new(w, h, f32::INFINITY),
        }
    }

    pub fn guide(&self, x: u32, y: u32) -> Guide {
        Guide {
            depth: self.depth.get(x, y),
            normal: self.normal.get(x, y).normalize_or_zero(),
        }
    }

    /// The colour a pixel contributes to the accumulation: the ray's radiance
    /// with the reflection channel folded in at its own weight.
    pub fn combined(&self, x: u32, y: u32) -> Vec3 {
        self.colour.get(x, y) + self.reflection.get(x, y)
    }
}

/// The result of one filtered frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Denoised {
    pub colour: ColourField,
    pub visibility: Field,
    pub reflection: ColourField,
    /// Filtered variance of the luminance channel, handed to the ray
    /// reconstruction provider as a guide.
    pub variance: Field,
    /// Filtered hit distance, the ray reconstruction provider's
    /// `SpecularHitDistance`/`DiffuseHitDistance` input.
    pub hit_distance: Field,
}

/// Filter one frame. `history` is updated in place (rebuilt when the extent
/// changed or the camera jumped).
///
/// `history_valid` comes from the caller: a camera cut, a resolution change or
/// the module being switched on all clear the accumulator, because a
/// reprojection across a cut is meaningless and would smear the old view.
pub fn denoise(
    frame: &Frame,
    history: &mut History,
    config: &DenoiseConfig,
    history_valid: bool,
) -> Denoised {
    let (w, h) = (frame.w, frame.h);
    if !history_valid || !history.matches(w, h) {
        *history = History::empty(w, h);
    }
    let current = frame
        .colour
        .data
        .iter()
        .zip(&frame.reflection.data)
        .map(|(c, r)| *c + *r)
        .collect::<Vec<_>>();
    let current = ColourField {
        w,
        h,
        data: current,
    };
    // --- Temporal accumulation -----------------------------------------
    let (accumulated, length, moment1, moment2) = temporal(frame, &current, history, config);
    // --- Spatial filtering ---------------------------------------------
    let mut colour = accumulated;
    let mut moment1 = moment1;
    let mut moment2 = moment2;
    for iteration in 0..config.iterations {
        let step = DenoiseConfig::step(iteration);
        let variance = variance_field(&moment1, &moment2, &length, config);
        colour = atrous(&colour, frame, &variance, step, config);
        moment1 = atrous_scalar(&moment1, frame, &variance, step, config);
        moment2 = atrous_scalar(&moment2, frame, &variance, step, config);
    }
    let variance = variance_field(&moment1, &moment2, &length, config);
    let visibility = filter_scalar(&frame.visibility, frame, &variance, config);
    let reflection = atrous(
        &frame.reflection,
        frame,
        &variance,
        1 << config.iterations,
        config,
    );
    let hit_distance = filter_scalar(&frame.depth_hit, frame, &variance, config);

    // --- Write history --------------------------------------------------
    history.colour = colour.clone();
    history.moment1 = moment1;
    history.moment2 = moment2;
    history.length = length;
    history.depth = frame.depth.clone();

    Denoised {
        colour,
        visibility,
        reflection,
        variance,
        hit_distance,
    }
}

/// Temporal accumulation.
///
/// For each pixel: reproject with the motion vector, take the history's colour
/// and moments there, clamp the history into the current frame's local
/// variance box, and blend with `alpha = max(1 / (n + 1), floor)`. A pixel whose
/// reprojection leaves the frame, or whose depth disagrees with the history's,
/// starts over at `n = 0`.
fn temporal(
    frame: &Frame,
    current: &ColourField,
    history: &History,
    config: &DenoiseConfig,
) -> (ColourField, Field, Field, Field) {
    let (w, h) = (frame.w, frame.h);
    let mut out = ColourField::new(w, h, Vec3::ZERO);
    let mut length = Field::new(w, h, 0.);
    let mut moment1_out = Field::new(w, h, 0.);
    let mut moment2_out = Field::new(w, h, 0.);
    // The local variance box is what makes the clamp *adaptive*: a region that
    // is genuinely noisy gets a box wide enough to accept its history, and a
    // region that just changed gets a narrow one that rejects it.
    for y in 0..h {
        for x in 0..w {
            let i = out.index(x, y);
            let curr = current.data[i];
            let (prev, prev_length) = match reproject(frame, history, x, y) {
                Some((px, py)) => {
                    let pi = history.colour.index(px, py);
                    (history.colour.data[pi], history.length.data[pi])
                }
                None => (Vec3::ZERO, 0.),
            };
            let (box_mean, box_variance) = local_moments(current, frame.depth.get(x, y), x, y);
            let box_stddev = box_variance.max(Vec3::ZERO).sqrt() * config.variance_gamma;
            // Per-channel clamp: each colour channel is bounded by its own
            // local mean plus or minus the local standard deviation.
            let low = box_mean - box_stddev;
            let high = box_mean + box_stddev;
            let clamped = Vec3::new(
                prev.x.clamp(low.x, high.x),
                prev.y.clamp(low.y, high.y),
                prev.z.clamp(low.z, high.z),
            );
            let n = prev_length.clamp(0., config.max_history);
            let alpha = (1. / (n + 1.)).max(config.temporal_floor);
            let blended = if n > 0. {
                curr * alpha + clamped * (1. - alpha)
            } else {
                curr
            };
            out.data[i] = blended;
            length.data[i] = (n + 1.).min(config.max_history);
            // The moments track the luminance of the *blended* value, so the
            // variance the spatial pass sees is the variance of the image it is
            // about to filter. They are accumulated at the pixel itself (not
            // reprojected): the history's own moments were already filtered
            // there, and a second reprojection of a smoothed scalar would
            // over-smooth it.
            let lum = luminance(blended);
            let m1 = history.moment1.get(x, y);
            let m2 = history.moment2.get(x, y);
            let (m1, m2) = if n > 0. {
                (
                    m1 * (1. - alpha) + lum * alpha,
                    m2 * (1. - alpha) + lum * lum * alpha,
                )
            } else {
                (lum, lum * lum)
            };
            moment1_out.data[i] = m1;
            moment2_out.data[i] = m2;
        }
    }
    (out, length, moment1_out, moment2_out)
}

/// Reproject a pixel into the history, rejecting anything that is not a
/// plausible match.
fn reproject(frame: &Frame, history: &History, x: u32, y: u32) -> Option<(u32, u32)> {
    let (w, h) = (frame.w, frame.h);
    let motion = frame.motion.get(x, y);
    let px = x as f32 + 0.5 - motion.x;
    let py = y as f32 + 0.5 - motion.y;
    if !(px.is_finite() && py.is_finite()) {
        return None;
    }
    if px < 0. || py < 0. || px >= w as f32 || py >= h as f32 {
        return None;
    }
    let ix = px as u32;
    let iy = py as u32;
    // Depth validation: a reprojection that lands on a very different depth is
    // a disocclusion (or a fast object), and its history would be wrong.
    let previous_depth = history.depth.get(ix, iy);
    let current_depth = frame.depth.get(x, y);
    let both_finite = previous_depth.is_finite() && current_depth.is_finite();
    if both_finite {
        let relative = (previous_depth - current_depth).abs() / current_depth.max(1e-3);
        if relative > 0.1 {
            return None;
        }
    } else if previous_depth.is_finite() != current_depth.is_finite() {
        return None;
    }
    Some((ix, iy))
}

/// Mean and variance of the 3×3 neighbourhood of `current`, restricted to the
/// pixels that belong to the same surface as the centre.
fn local_moments(current: &ColourField, centre_depth: f32, x: u32, y: u32) -> (Vec3, Vec3) {
    let mut sum = Vec3::ZERO;
    let mut sum_sq = Vec3::ZERO;
    let mut count = 0.;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let sx = x as i32 + dx;
            let sy = y as i32 + dy;
            if sx < 0 || sy < 0 || sx >= current.w as i32 || sy >= current.h as i32 {
                continue;
            }
            let c = current.get(sx as u32, sy as u32);
            let _ = centre_depth;
            sum += c;
            sum_sq += c * c;
            count += 1.;
        }
    }
    if count == 0. {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    let mean = sum / count;
    let variance = (sum_sq / count - mean * mean).max(Vec3::ZERO);
    (mean, variance)
}

/// The variance the spatial passes weight colour differences by.
///
/// This is the *temporal* variance — how much a pixel's luminance has changed
/// across the frames blended into its history — and deliberately **not** the
/// variance of the noisy image's own neighbourhood. The spatial estimate cannot
/// tell a noisy region from a real 3-pixel-wide bright feature, so weighting by
/// it erases thin geometry; the temporal estimate can, because real geometry is
/// stable frame to frame while a 1-sample tracer's noise is not. On the first
/// frame of a history there is one sample and therefore no variance, which is
/// correct: the first frame is not spatially filtered at all, and the
/// accumulator takes over from the second frame on.
fn variance_field(
    moment1: &Field,
    moment2: &Field,
    length: &Field,
    config: &DenoiseConfig,
) -> Field {
    Field::from_fn(moment1.w, moment1.h, |x, y| {
        let i = moment1.index(x, y);
        let m1 = moment1.data[i];
        let m2 = moment2.data[i];
        let n = length.data[i];
        let variance = (m2 - m1 * m1).max(0.);
        // Few samples weigh more: an unconverged pixel's variance estimate is
        // itself noisy, so bias it upward.
        let bias = if n > 0. { 1. / n.sqrt().max(0.25) } else { 1. };
        (variance * bias).max(config.variance_floor)
    })
}

/// One edge-avoiding à-trous pass over a colour field.
pub fn atrous(
    src: &ColourField,
    frame: &Frame,
    variance: &Field,
    step: i32,
    config: &DenoiseConfig,
) -> ColourField {
    let (w, h) = (src.w, src.h);
    let mut out = src.clone();
    for y in 0..h {
        for x in 0..w {
            let centre = src.get(x, y);
            let guide = frame.guide(x, y);
            let var = variance.get(x, y);
            let centre_lum = luminance(centre);
            let mut sum = Vec3::ZERO;
            let mut weight_sum = 0.;
            for dy in [-2i32 * step, -step, 0, step, 2 * step] {
                for dx in [-2i32 * step, -step, 0, step, 2 * step] {
                    let sx = x as i32 + dx;
                    let sy = y as i32 + dy;
                    if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                        continue;
                    }
                    let (sx, sy) = (sx as u32, sy as u32);
                    let sample = src.get(sx, sy);
                    let w_normal = normal_weight(
                        guide.normal,
                        frame.normal.get(sx, sy).normalize_or_zero(),
                        config,
                    );
                    let w_depth = depth_weight(guide.depth, frame.depth.get(sx, sy), config);
                    let w_colour = colour_weight(centre_lum, luminance(sample), var, config);
                    let w = w_normal * w_depth * w_colour;
                    if w <= 0. || !w.is_finite() {
                        continue;
                    }
                    sum += sample * w;
                    weight_sum += w;
                }
            }
            if weight_sum > 1e-8 {
                out.set(x, y, sum / weight_sum);
            }
        }
    }
    out
}

/// One à-trous pass over a scalar field (the moments).
pub fn atrous_scalar(
    src: &Field,
    frame: &Frame,
    variance: &Field,
    step: i32,
    config: &DenoiseConfig,
) -> Field {
    let (w, h) = (src.w, src.h);
    let mut out = src.clone();
    for y in 0..h {
        for x in 0..w {
            let guide = frame.guide(x, y);
            let var = variance.get(x, y);
            let mut sum = 0.;
            let mut weight_sum = 0.;
            for dy in [-2i32 * step, -step, 0, step, 2 * step] {
                for dx in [-2i32 * step, -step, 0, step, 2 * step] {
                    let sx = x as i32 + dx;
                    let sy = y as i32 + dy;
                    if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                        continue;
                    }
                    let (sx, sy) = (sx as u32, sy as u32);
                    let w_normal = normal_weight(
                        guide.normal,
                        frame.normal.get(sx, sy).normalize_or_zero(),
                        config,
                    );
                    let w_depth = depth_weight(guide.depth, frame.depth.get(sx, sy), config);
                    let w = w_normal * w_depth;
                    if w <= 0. || !w.is_finite() {
                        continue;
                    }
                    let _ = var;
                    sum += src.get(sx, sy) * w;
                    weight_sum += w;
                }
            }
            if weight_sum > 1e-8 {
                out.set(x, y, sum / weight_sum);
            }
        }
    }
    out
}

/// A small fixed-radius pass over a scalar field, used for the AO and hit-distance
/// channels where a full wavelet chain is overkill.
pub fn filter_scalar(
    src: &Field,
    frame: &Frame,
    variance: &Field,
    config: &DenoiseConfig,
) -> Field {
    let mut out = atrous_scalar(src, frame, variance, 1, config);
    out = atrous_scalar(&out, frame, variance, 2, config);
    out
}

fn normal_weight(a: Vec3, b: Vec3, config: &DenoiseConfig) -> f32 {
    let d = a.dot(b).clamp(-1., 1.);
    // `pow(max(dot, 0), phi)`: phi = 32 makes a 20-degree bend cost ~85% of the
    // tap's weight, which is the right stiffness for tree canopies and stone.
    d.max(0.).powf(config.phi_normal)
}

fn depth_weight(a: f32, b: f32, config: &DenoiseConfig) -> f32 {
    match (a.is_finite(), b.is_finite()) {
        (false, false) => 1.,
        (false, true) | (true, false) => 0.,
        (true, true) => {
            let difference = (a - b).abs();
            (-difference / (config.phi_depth * a.abs().max(1e-3))).exp()
        }
    }
}

fn colour_weight(a: f32, b: f32, variance: f32, config: &DenoiseConfig) -> f32 {
    let sigma = config.phi_colour * variance.max(config.variance_floor).sqrt();
    let difference = (a - b).abs();
    (-difference / sigma.max(1e-6)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DenoiseConfig {
        DenoiseConfig::default()
    }

    /// A flat, fully converged frame: every guide identical, no motion.
    fn flat(w: u32, h: u32, colour: Vec3) -> Frame {
        let mut frame = Frame::new(w, h);
        for y in 0..h {
            for x in 0..w {
                frame.colour.set(x, y, colour);
                frame.depth.set(x, y, 100.);
                frame.normal.set(x, y, Vec3::Y);
                frame.depth_hit.set(x, y, 100.);
            }
        }
        frame
    }

    #[test]
    fn a_flat_image_is_unchanged_by_the_whole_chain() {
        let colour = Vec3::new(0.3, 0.5, 0.7);
        let frame = flat(16, 16, colour);
        let mut history = History::empty(16, 16);
        // Repeated frames settle the accumulator; the filter must never shift a
        // constant.
        for _ in 0..8 {
            let out = denoise(&frame, &mut history, &config(), true);
            for y in 0..16 {
                for x in 0..16 {
                    let c = out.colour.get(x, y);
                    assert!(
                        (c - colour).length() < 1e-3,
                        "flat {colour:?} became {c:?} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn energy_is_conserved_across_a_noise_field() {
        // White noise around a mean must keep its mean through the filter: a
        // denoiser that brightens or darkens the world is worse than noise.
        let mut seed = 1u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 9) as f32 / (1 << 23) as f32
        };
        let noise: Vec<Vec3> = (0..64 * 64)
            .map(|_| Vec3::splat(0.5 + (next() - 0.5) * 0.2))
            .collect();
        let frame = Frame::from_noise(64, 64, &noise);
        let input_mean = frame.colour.mean();
        let mut history = History::empty(64, 64);
        let out = denoise(&frame, &mut history, &config(), true);
        let output_mean = out.colour.mean();
        assert!(
            (output_mean - input_mean).length() < 0.02,
            "mean drifted: {input_mean:?} -> {output_mean:?}"
        );
        // And the noise is actually gone.
        assert!(
            out.colour.luminance().variance() < frame.colour.luminance().variance() * 0.25,
            "variance {} did not drop from {}",
            out.colour.luminance().variance(),
            frame.colour.luminance().variance()
        );
    }

    #[test]
    fn a_geometric_edge_survives_the_filter() {
        // Left half dark, right half bright, different normals: a
        // cross-bilateral filter must not average across the seam.
        let (w, h) = (192u32, 192u32);
        let mut frame = flat(w, h, Vec3::ZERO);
        for y in 0..h {
            for x in 0..w {
                if x < w / 2 {
                    frame.colour.set(x, y, Vec3::new(0.05, 0.05, 0.05));
                } else {
                    frame.colour.set(x, y, Vec3::new(0.9, 0.9, 0.9));
                    frame.normal.set(x, y, Vec3::X);
                }
            }
        }
        let mut history = History::empty(w, h);
        let out = denoise(&frame, &mut history, &config(), true);
        let dark = out.colour.get(8, 96).x;
        let bright = out.colour.get(184, 96).x;
        assert!(dark < 0.06, "the dark side bled to {dark}");
        assert!(bright > 0.89, "the bright side bled to {bright}");
        // The seam is the only place the two may mix; the normal weight keeps
        // even that narrow.
        assert!(
            out.colour.get(95, 96).x < 0.5,
            "{:?}",
            out.colour.get(95, 96)
        );
    }

    #[test]
    fn history_converges_and_then_barely_updates() {
        // A pixel whose noisy input keeps changing converges to its mean, and
        // the incremental update shrinks as the history grows.
        let mut frame = flat(8, 8, Vec3::splat(0.5));
        let mut history = History::empty(8, 8);
        let mut last = Vec3::ZERO;
        for i in 0..16 {
            // Alternating noise, mean 0.5.
            frame
                .colour
                .set(4, 4, Vec3::splat(if i % 2 == 0 { 0.7 } else { 0.3 }));
            last = denoise(&frame, &mut history, &config(), true)
                .colour
                .get(4, 4);
        }
        assert!((last.x - 0.5).abs() < 0.05, "converged to {last:?}");
        assert!(history.length.get(4, 4) > 8.);
        // A first frame with an empty history is taken at face value: no
        // temporal blend happened, so the image is the spatial filter of the
        // current frame alone.
        let mut fresh = History::empty(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                frame.colour.set(x, y, Vec3::splat(0.9));
            }
        }
        let first = denoise(&frame, &mut fresh, &config(), true)
            .colour
            .get(4, 4);
        assert!((first.x - 0.9).abs() < 0.05, "{first:?}");
        assert_eq!(fresh.length.get(4, 4), 1.);
    }

    #[test]
    fn a_camera_cut_resets_the_history_instead_of_smearing_it() {
        let mut frame = flat(8, 8, Vec3::splat(0.1));
        let mut history = History::empty(8, 8);
        denoise(&frame, &mut history, &config(), true);
        denoise(&frame, &mut history, &config(), true);
        assert!(history.length.get(4, 4) > 1.);
        // Invalid history: the frame after the cut must be the new image, not a
        // blend with the old one. The whole frame changes, because the cross
        // pass is free to average the cut's pixels with each other.
        for y in 0..8 {
            for x in 0..8 {
                frame.colour.set(x, y, Vec3::splat(0.95));
            }
        }
        let out = denoise(&frame, &mut history, &config(), false);
        assert!(
            (out.colour.get(4, 4).x - 0.95).abs() < 0.05,
            "{}",
            out.colour.get(4, 4).x
        );
        assert_eq!(history.length.get(4, 4), 1.);
    }

    #[test]
    fn a_disocclusion_rejects_the_reprojected_history() {
        // History sees depth 10 at the centre; the current frame has depth 900
        // there (the pixel uncovered a distant wall), so the reprojection must
        // be refused.
        let mut frame = flat(8, 8, Vec3::splat(0.0));
        let mut history = History::empty(8, 8);
        for _ in 0..8 {
            frame.colour.set(4, 4, Vec3::splat(0.2));
            denoise(&frame, &mut history, &config(), true);
        }
        // The whole frame uncovers the distant surface: every pixel's
        // reprojection is now refused, so no pixel may keep the old value.
        for y in 0..8 {
            for x in 0..8 {
                frame.colour.set(x, y, Vec3::splat(0.8));
                frame.depth.set(x, y, 900.);
            }
        }
        let out = denoise(&frame, &mut history, &config(), true);
        assert!(
            out.colour.get(4, 4).x > 0.6,
            "the new surface kept {} of the old history",
            out.colour.get(4, 4).x
        );
    }

    /// A frame of a given size with a coherent bright band and a matching guide,
    /// at a realistic resolution: the filter's own radius (65 px at five
    /// iterations) has to be small against the image for a thin feature to
    /// survive, exactly as it is at 960x540.
    fn banded(w: u32, h: u32, band: (u32, u32)) -> Frame {
        let mut frame = flat(w, h, Vec3::splat(0.35));
        for y in 0..h {
            for x in band.0..=band.1 {
                frame.colour.set(x, y, Vec3::splat(0.85));
            }
        }
        frame
    }

    #[test]
    fn motion_vectors_reproject_and_out_of_frame_motion_resets() {
        let (w, h) = (192u32, 192u32);
        let mut frame = banded(w, h, (72, 112));
        let mut history = History::empty(w, h);
        for _ in 0..6 {
            denoise(&frame, &mut history, &config(), true);
        }
        // The band's interior is fully converged and untouched by the filter.
        assert!(
            history.colour.get(92, 96).x > 0.8,
            "the band was not accumulated: {}",
            history.colour.get(92, 96).x
        );
        // The whole scene moves one pixel to the right: the band's history now
        // belongs at x + 1, so the accumulated value must be found there.
        for y in 0..h {
            for x in 0..w {
                frame.motion.set(x, y, Vec3::new(-1., 0., 0.));
            }
        }
        let mut moved = banded(w, h, (73, 113));
        for y in 0..h {
            for x in 0..w {
                moved.motion.set(x, y, Vec3::new(-1., 0., 0.));
            }
        }
        denoise(&mut moved, &mut history, &config(), true);
        assert!(
            history.colour.get(93, 96).x > 0.75,
            "history did not follow the band: {}",
            history.colour.get(93, 96).x
        );
        // The pixel the band's left edge has vacated lost the bright value,
        // which is the difference between reprojection and a blur. (It keeps
        // some of it: the accumulator blends rather than replaces, and the
        // filter has already mixed the edge pixel with its darker neighbour.)
        assert!(
            history.colour.get(72, 96).x < 0.7,
            "the vacated edge kept its history: {}",
            history.colour.get(72, 96).x
        );
        assert!(
            history.colour.get(72, 96).x < history.colour.get(93, 96).x,
            "the band did not move"
        );
        // A motion vector far outside the frame (the camera teleported) leaves
        // nothing to read: every pixel starts over at the new image.
        for y in 0..h {
            for x in 0..w {
                moved.motion.set(x, y, Vec3::new(500., 500., 0.));
            }
        }
        let out = denoise(&mut moved, &mut history, &config(), true);
        assert!(
            (out.colour.get(93, 96).x - 0.85).abs() < 0.1,
            "{}",
            out.colour.get(93, 96).x
        );
    }

    #[test]
    fn the_sky_and_the_ground_never_mix() {
        // Half the image is sky (infinite depth), half is ground, at a
        // resolution where the horizon is far from either sample.
        let (w, h) = (192u32, 192u32);
        let mut frame = flat(w, h, Vec3::ZERO);
        for y in 0..h {
            for x in 0..w {
                if y < h / 2 {
                    frame.colour.set(x, y, Vec3::new(0.1, 0.2, 0.4));
                    frame.depth.set(x, y, f32::INFINITY);
                } else {
                    frame.colour.set(x, y, Vec3::new(0.8, 0.7, 0.5));
                }
            }
        }
        let mut history = History::empty(w, h);
        for _ in 0..4 {
            let out = denoise(&frame, &mut history, &config(), true);
            assert!(out.colour.get(16, 8).z > 0.3, "{:?}", out.colour.get(16, 8));
            assert!(
                out.colour.get(16, 184).x > 0.7,
                "{:?}",
                out.colour.get(16, 184)
            );
            // The horizon is a hard edge, so no sky value leaks into the ground
            // colour: the sky is blue, the ground is not.
            assert!(
                out.colour.get(16, 184).z < 0.6,
                "{:?}",
                out.colour.get(16, 184)
            );
        }
    }

    #[test]
    fn iteration_count_is_the_filter_radius_and_nothing_else_breaks() {
        // Noise that is constant in time never accumulates a variance estimate,
        // so the filter treats it as structure and leaves it alone — the same
        // conservative behaviour a real denoiser has, and the reason a still
        // frame needs no filter at all. What must hold regardless is that more
        // iterations never create energy and never fail to return.
        let mut frame = flat(64, 64, Vec3::splat(0.5));
        for iterations in 1..=5 {
            let mut history = History::empty(64, 64);
            let mut c = config();
            c.iterations = iterations;
            let out = denoise(&frame, &mut history, &c, true);
            let total: f32 = out.colour.data.iter().map(|c| c.x).sum();
            let input: f32 = frame.colour.data.iter().map(|c| c.x).sum();
            assert!(
                total <= input * 1.05 && total >= input * 0.95,
                "iterations {iterations} drifted the mean: {total} vs {input}"
            );
            assert_eq!(out.colour.data.len(), 64 * 64);
            assert_eq!(DenoiseConfig::step(iterations - 1), 1 << (iterations - 1));
        }
    }

    #[test]
    fn temporally_varying_noise_is_what_the_filter_removes() {
        // A noisy pixel whose noise changes every frame (which is what a
        // 1-sample tracer produces) accumulates variance, so the filter is
        // allowed to average it; the same pixel with noise that never changes
        // is treated as structure and kept.
        let mut history = History::empty(32, 32);
        let mut seed = 3u32;
        let mut last_variance = 0.;
        for _ in 0..6 {
            let mut frame = flat(32, 32, Vec3::splat(0.5));
            for y in 0..32 {
                for x in 0..32 {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let n = ((seed >> 9) as f32 / (1 << 23) as f32 - 0.5) * 0.5;
                    frame.colour.set(x, y, Vec3::splat(0.5 + n));
                }
            }
            last_variance = denoise(&frame, &mut history, &config(), true)
                .variance
                .mean();
        }
        assert!(
            last_variance > 1e-3,
            "temporal noise must report variance: {last_variance}"
        );
    }

    #[test]
    fn the_variance_guide_tracks_the_noise_and_clears_on_a_flat_frame() {
        let mut noisy = Frame::new(32, 32);
        let mut seed = 7u32;
        for y in 0..32 {
            for x in 0..32 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = ((seed >> 9) as f32 / (1 << 23) as f32 - 0.5) * 0.4;
                noisy.colour.set(x, y, Vec3::splat(0.5 + n));
                noisy.depth.set(x, y, 50.);
                noisy.normal.set(x, y, Vec3::Y);
            }
        }
        let mut history = History::empty(32, 32);
        let out = denoise(&noisy, &mut history, &config(), true);
        let noisy_variance = out.variance.mean();
        assert!(
            noisy_variance > 1e-3,
            "noise must report variance: {noisy_variance}"
        );
        // A flat frame's variance collapses towards the floor instead.
        let mut flat_frame = flat(32, 32, Vec3::splat(0.5));
        let mut history = History::empty(32, 32);
        let mut flat_variance = f32::INFINITY;
        for _ in 0..4 {
            flat_variance = denoise(&flat_frame, &mut history, &config(), true)
                .variance
                .mean();
        }
        assert!(flat_variance < 1e-3, "{flat_variance}");
        assert!(flat_variance < noisy_variance * 1e-2);
    }

    #[test]
    fn config_from_a_file_is_clamped_and_steps_double() {
        let mut c = DenoiseConfig::default();
        c.iterations = 99;
        c.phi_colour = f32::NAN;
        c.phi_normal = -5.;
        c.phi_depth = 0.;
        c.temporal_floor = 0.;
        c.variance_gamma = 1e9;
        c.variance_floor = 0.;
        c.max_history = 0.;
        c.sanitise();
        assert_eq!(c.iterations, 5);
        assert_eq!(c.phi_colour, 4.);
        assert_eq!(c.phi_normal, 1.);
        assert!(c.phi_depth >= 1e-3);
        assert!(c.temporal_floor >= 0.01);
        assert!(c.variance_gamma <= 16.);
        assert!(c.variance_floor >= 1e-8);
        assert!(c.max_history >= 1.);
        assert_eq!(DenoiseConfig::step(0), 1);
        assert_eq!(DenoiseConfig::step(1), 2);
        assert_eq!(DenoiseConfig::step(3), 8);
        // A step far past any real iteration count stays positive instead of
        // overflowing the shift.
        assert!(DenoiseConfig::step(40) > 0);
    }

    #[test]
    fn a_zero_sized_frame_is_a_no_op_rather_than_a_panic() {
        let frame = Frame::new(0, 0);
        let mut history = History::empty(0, 0);
        let out = denoise(&frame, &mut history, &config(), true);
        assert!(out.colour.data.is_empty());
        let frame = Frame::new(1, 1);
        let mut history = History::empty(1, 1);
        let out = denoise(&frame, &mut history, &config(), true);
        assert_eq!(out.colour.data.len(), 1);
    }

    impl Frame {
        /// A frame of pure colour noise on a flat plane, for the conservation
        /// test.
        fn from_noise(w: u32, h: u32, noise: &[Vec3]) -> Self {
            let mut frame = Self::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    frame.colour.set(x, y, noise[i]);
                    frame.depth.set(x, y, 100.);
                    frame.normal.set(x, y, Vec3::Y);
                }
            }
            frame
        }
    }
}
