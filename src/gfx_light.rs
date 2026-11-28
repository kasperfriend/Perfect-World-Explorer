//! Point-light element decoding and single-keypoint execution for the actual
//! retail corpus. This is not a general GFX graph/attachment/visual player.
//! Parent transforms, active GFX state and scaled tick time are caller inputs.
use crate::{gfx_color_track::ColorTrack, scene_lights::DynamicLight};
#[derive(Debug, Clone)]
pub struct Definition {
    pub priority: i32,
    pub range: f32,
    start_ms: u32,
    duration_ms: Option<u32>,
    position: [f32; 3],
    color: u32,
    track: Option<ColorTrack>,
    /// How many keypoints the element declared, and which one this is.
    ///
    /// The native class keeps a variable-length keypoint array, so a light is
    /// not necessarily one reference point. Every light element in the staged
    /// corpus declares exactly one (`488` of `488`), but the reader no longer
    /// *requires* that, and these two fields are what let a caller tell a whole
    /// light from one stage of a longer one.
    pub keypoint_count: usize,
    pub keypoint_index: usize,
}

/// A light element: one `Priority`/`Range` and the keypoints it walks.
///
/// `Definition` is one keypoint. When a light has a single keypoint -- which is
/// every light element in the staged corpus -- the light *is* that one
/// `Definition`, which is why the older API could treat the two as the same.
#[derive(Debug, Clone)]
pub struct Light {
    pub definitions: Vec<Definition>,
}
impl Definition {
    /// Interior colour-table entries whose two candidate construction
    /// precisions disagree. Reported by the corpus survey because the native
    /// loading-thread precision is not established (see `gfx_color_track`).
    pub fn construction_divergences(&self) -> usize {
        self.track
            .as_ref()
            .map_or(0, ColorTrack::construction_divergences)
    }
}
fn number<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("invalid GFX light number: {s}"))
}
fn packed(s: &str) -> Result<u32, String> {
    let value: i64 = number(s)?;
    if !(-2147483648..=4294967295).contains(&value) {
        return Err("invalid GFX packed color".into());
    }
    Ok(value as u32)
}
fn unique<'a>(fields: &[(&'a str, &'a str)], key: &str) -> Result<&'a str, String> {
    let mut found = fields.iter().filter(|(k, _)| *k == key);
    let value = found
        .next()
        .ok_or_else(|| format!("missing GFX light {key}"))?
        .1;
    if found.next().is_some() {
        return Err(format!("duplicate GFX light {key}"));
    }
    Ok(value)
}
fn take<'a>(fields: &'a [(&'a str, &'a str)], i: &mut usize, key: &str) -> Result<&'a str, String> {
    let (k, v) = *fields.get(*i).ok_or("truncated GFX light keypoint")?;
    if k != key {
        return Err(format!("expected GFX light {key}, got {k}"));
    }
    *i += 1;
    Ok(v)
}
fn vec3(s: &str) -> Result<[f32; 3], String> {
    let v = s
        .split(',')
        .map(|v| number::<f32>(v.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    let v: [f32; 3] = v.try_into().map_err(|_| "invalid GFX light position")?;
    if v.iter().any(|v| !v.is_finite()) {
        return Err("nonfinite GFX position".into());
    }
    Ok(v)
}
/// The element-wide fields, read once and shared by every keypoint.
struct Common {
    priority: i32,
    range: f32,
    start_ms: u32,
    /// Where the keypoint run starts, i.e. just past `StartTime`.
    keypoints_at: usize,
}

fn common(fields: &[(&str, &str)]) -> Result<Common, String> {
    let split = fields
        .iter()
        .position(|(k, _)| *k == "KEYPOINTCOUNT")
        .ok_or("missing GFX keypoint count")?;
    let common = &fields[..split];
    if unique(common, "LightType")? != "1"
        || unique(common, "RepeatCount")? != "1"
        || unique(common, "RepeatDelay")? != "0"
        || unique(common, "BindEle")? != ""
        || unique(common, "DummyEle")? != ""
        || unique(common, "IsDummy")? != "0"
        || unique(common, "MatchGrnd")? != "0"
    {
        return Err("unsupported GFX light type/repeat/binding/ground flags".into());
    }
    for (k, v) in common {
        if matches!(*k, "GroundHeight" | "GroundNormal" | "InnerUse") && *v != "0" {
            return Err("unsupported GFX ground/inner-use light".into());
        }
    }
    let range: f32 = number(unique(common, "Range")?)?;
    if !range.is_finite() || range < 0. {
        return Err("invalid GFX light range".into());
    }
    let priority = number(unique(common, "Priority")?)?;
    let start_ms = number(unique(common, "StartTime")?)?;
    Ok(Common {
        priority,
        range,
        start_ms,
        keypoints_at: split,
    })
}

/// One keypoint: where the light sits, what colour it is, and its own window.
///
/// The element's `StartTime` is the light's own activation and is not repeated
/// here; each keypoint carries a `TimeSpan` (how long it is held, `-1` for
/// "until the light ends") and a `CtrlMethodCount`-driven controller list.
fn keypoint(
    fields: &[(&str, &str)],
    i: &mut usize,
    shared: &Common,
    index: usize,
    count: usize,
) -> Result<Definition, String> {
    if take(fields, i, "InterpolateMode")? != "0" {
        // Every `InterpolateMode` in the staged corpus is `0` (11,844 of
        // 11,844): the light holds a keypoint's point and then switches. A
        // non-zero mode blends between them, which nothing shipped does, so it
        // is named rather than guessed.
        return Err("unsupported GFX light keypoint interpolation mode".into());
    }
    let duration: i64 = number(take(fields, i, "TimeSpan")?)?;
    let duration_ms = if duration == -1 {
        None
    } else {
        Some(u32::try_from(duration).map_err(|_| "invalid GFX light duration")?)
    };
    let position = vec3(take(fields, i, "Position")?)?;
    let color = packed(take(fields, i, "Color")?)?;
    let scale: f32 = number(take(fields, i, "Scale")?)?;
    if !scale.is_finite() {
        return Err("nonfinite GFX keypoint scale".into());
    }
    // Point-light position uses parent TM and keypoint position, not this
    // keypoint's direction, rotation or scale. Range is not scaled by kp.scale.
    let direction = take(fields, i, "Direction")?;
    let q = direction
        .split(',')
        .map(|x| number::<f32>(x.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    if q.len() != 4 || q.iter().any(|v| !v.is_finite()) {
        return Err("invalid GFX keypoint direction".into());
    }
    let rad: f32 = number(take(fields, i, "Rad_2D")?)?;
    if !rad.is_finite() {
        return Err("nonfinite GFX keypoint angle".into());
    }
    let controllers: u32 = number(take(fields, i, "CtrlMethodCount")?)?;
    let track = match controllers {
        0 => None,
        1 => {
            if take(fields, i, "CtrlType")? != "108" {
                return Err("unsupported GFX light controller".into());
            }
            // The container writes milliseconds, but a `ColorTrack` window is
            // in seconds ("Start/end are seconds; segment lengths are
            // milliseconds", `gfx_color_track`) and `Emitter::tick` samples it
            // with the seconds elapsed since the element's own `StartTime`.
            // Passing the raw milliseconds through put the ramp a thousand
            // times later than authored: a controller starting after 0 never
            // left the keypoint colour, and one with an end never expired.
            let start: f32 = number(take(fields, i, "StartTime")?)?;
            let end: f32 = number(take(fields, i, "EndTime")?)?;
            let original = packed(take(fields, i, "Color")?)?;
            let count: usize = number(take(fields, i, "Count")?)?;
            if count == 0 || count > 10 {
                return Err("invalid GFX color destination count".into());
            }
            let mut destinations = Vec::with_capacity(count);
            for _ in 0..count {
                let color = packed(take(fields, i, "Color")?)?;
                let duration = number(take(fields, i, "TimeSpan")?)?;
                destinations.push((color, duration));
            }
            // The segment lengths stay in milliseconds, which is the unit the
            // track walks its 128-entry tables with.
            Some(ColorTrack::new(
                start / 1000.,
                end / 1000.,
                original,
                &destinations,
            )?)
        }
        // Measured on the staged corpus: every one of the 488 light elements
        // has `CtrlMethodCount: 0`, so no controller is written on a light
        // keypoint at all. `108` (a colour ramp, `track` above) is the one this
        // emitter executes. Anything else stays an explicit refusal rather than
        // a silent drop: a controller this build cannot run would change the
        // light's motion, and publishing a stationary light where the artist
        // authored a moving one is a wrong answer, not a smaller one.
        _ => return Err("unsupported GFX light controller count".into()),
    };
    Ok(Definition {
        keypoint_count: count,
        keypoint_index: index,
        priority: shared.priority,
        range: shared.range,
        start_ms: shared.start_ms,
        duration_ms,
        position,
        color,
        track,
    })
}
/// Which light an element actually *is*.
///
/// The previous reader treated any element carrying `LightNum`/`WaveLen`/
/// `Segs`/`Interval`/`Amplitude`/`Pos1Enable`/`Filter` as a light path it could
/// not execute and failed the whole root. Those keys are not light-exclusive:
/// `BufLen: %d`, `Amplitude: %f`, `WaveLen: %d`, `Persistence: %f`,
/// `OctaveNum: %d` are the effect's **noise** block, which the native reader
/// loads for the sprite/particle/beam/trail families exactly as it loads
/// `StartPos`/`EndPos`/`LightNum`/`Filter` for the line and beam families
/// (`0x7078d0` is the range-light loader; `0x8ebbac` `Amplitude: %f` and
/// `0x8ebbbc` `WaveLen: %d` are consumed by the shared header). Measured on the
/// retail `gfx.pck`: 5,878 elements carry one of those keys, but only **546**
/// carry `LightType`, and all 546 are element 130. Counting the noise block as a
/// light rejected 2,014 of 5,459 roots and loaded 0 of the 524 roots that
/// really contain a light.
///
/// The light marker is therefore `LightType: %d`, the only key the light
/// element reader consumes (`0x8ed204`, hit twice: `0x70b50b`/`0x70b7d5`).
/// Element 130 is implemented; any other element that declares `LightType` is
/// still an explicit failure rather than a silent drop, so a future light class
/// cannot disappear without a register entry.
/// Upper bound on a light element's `KEYPOINTCOUNT`.
///
/// Measured on the staged retail `gfx.pck`, one element at a time: **all 488
/// light elements declare `KEYPOINTCOUNT: 1`**, and every one of their
/// keypoints carries `CtrlMethodCount: 0`. The `!= 1` test this replaces was
/// therefore costing nothing in the staged corpus -- a survey that counted
/// `KEYPOINTCOUNT` across a *whole graph* rather than per element finds 7 in
/// sprites and particles, and those are a different element class that this
/// module does not read.
///
/// So the bound is sized from the corpus rather than from the old equality, and
/// the reason to allow more than one is the format, not the data on hand: the
/// native class keeps a variable-length keypoint array, and a client that
/// writes two is describing the same light twice, not a different format. Eight
/// leaves headroom over the measured maximum while still bounding the loop a
/// damaged count could demand.
const MAX_KEYPOINTS: usize = 8;

/// Decode one light element, which is a `Priority`/`Range` and 1..=`MAX_KEYPOINTS`
/// keypoints.
fn decode_all(fields: &[(&str, &str)]) -> Result<Light, String> {
    let shared = common(fields)?;
    let mut i = shared.keypoints_at;
    let count: usize = number(take(fields, &mut i, "KEYPOINTCOUNT")?)?;
    if count == 0 || count > MAX_KEYPOINTS {
        return Err(format!(
            "unsupported GFX light keypoint count {count} (limit {MAX_KEYPOINTS})"
        ));
    }
    let mut definitions = Vec::with_capacity(count);
    for index in 0..count {
        definitions.push(keypoint(fields, &mut i, &shared, index, count)?);
    }
    // A misread keypoint count would otherwise leave the trailing fields to be
    // interpreted as another element's, so the run has to consume the whole
    // record.
    if i != fields.len() {
        return Err("unrecognized trailing GFX light keypoint/controller data".into());
    }
    Ok(Light { definitions })
}

/// The single-keypoint view of [`decode_all`].
///
/// A light with one keypoint is the whole light in one `Definition`. A light
/// with more than one is not -- this returns its first keypoint and the caller
/// that needs the sequence uses [`decode_all`]. Kept so the existing callers
/// and their tests keep their meaning.
fn decode(fields: &[(&str, &str)]) -> Result<Definition, String> {
    let light = decode_all(fields)?;
    light
        .definitions
        .into_iter()
        .next()
        .ok_or_else(|| "GFX light has no keypoint".to_string())
}

const LIGHT_MARKER: &str = "LightType";

fn finish_element(
    current: Option<u32>,
    fields: &[(&str, &str)],
    definitions: &mut Vec<Definition>,
) -> Result<(), String> {
    let marker = fields.iter().find(|(k, _)| *k == LIGHT_MARKER);
    match (current, marker) {
        // Every keypoint is kept. Pushing only the first, as this did while the
        // keypoint count was hard-coded to 1, silently dropped the rest of a
        // multi-keypoint light now that they parse.
        (Some(130), Some(_)) => definitions.extend(decode_all(fields)?.definitions),
        (Some(130), None) => return Err("element 130 is missing its LightType".into()),
        (Some(id), Some((key, _))) => {
            return Err(format!(
                "unsupported GFX light element {id} ({key}); only element 130 point lights are executed"
            ))
        }
        // No element id in scope: the header is not a light record. Retail
        // roots never declare LightType before their first GFXELEMENTID.
        (None, Some((key, _))) => {
            return Err(format!("GFX root declares {key} outside any element"))
        }
        // Non-light elements (particles, sprites, beams, trails, containers)
        // are skipped exactly as the native loader skips their light pass.
        (_, None) => {}
    }
    Ok(())
}
/// Decode light elements only; callers still have to execute the surrounding
/// GFX graph and resolve active parent transforms. Unknown light paths fail.
pub fn parse(text: &str) -> Result<Vec<Definition>, String> {
    let mut lines = text.lines();
    let version: u32 = number(
        lines
            .next()
            .and_then(|l| l.strip_prefix("MOXTVersion: "))
            .ok_or("expected text GFX")?
            .trim(),
    )?;
    // There is no version gate here, and the light element is why.
    //
    // `A3DGFXLight::Load` in the shipped SDK reads fourteen lines — `LightType`,
    // `Diffuse`, `Specular`, `Ambient`, `Position`, `Direction`, `Range`,
    // `Falloff`, `Attenuation0..2`, `Theta`, `Phi` — and never looks at
    // `dwVersion`. A version-93 light record is byte-for-byte the same shape as
    // a version-33 one, so refusing by version published no lights and cost
    // this client every later effect.
    //
    // This used to be a ten-value whitelist, then a `< 80` cap that came from
    // the 1.4.2 corpus maximum rather than from the format: the SDK's own
    // history runs to 103 (`#define GFX_CUR_VERSION 103`) and 1.5.2 ships 86,
    // 90 and 93. Since `gfx_owner` marks the owner `State::Failed` on an
    // `unsupported` error and only buildable owners reach the player, the cap
    // was not merely dropping light glow, it was dropping the geometry of every
    // root that also happened to carry a point light.
    let _ = version;
    let mut expected = None;
    let mut elements = 0;
    let mut current = None;
    let mut fields = Vec::with_capacity(32);
    let mut definitions = Vec::new();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "GFXELEMENTCOUNT" {
            if expected.is_some() {
                return Err("duplicate GFX element count".into());
            }
            let n: usize = number(value)?;
            if n > 100000 {
                return Err("excessive GFX element count".into());
            }
            definitions.reserve(n.min(64));
            expected = Some(n);
        } else if key == "GFXELEMENTID" {
            elements += 1;
            finish_element(current, &fields, &mut definitions)?;
            fields.clear();
            current = Some(number::<u32>(value)?);
        } else {
            fields.push((key, value));
        }
    }
    finish_element(current, &fields, &mut definitions)?;
    if expected != Some(elements) {
        return Err("GFX element count mismatch".into());
    }
    Ok(definitions)
}
#[derive(Debug)]
pub struct Emitter {
    definition: Definition,
    elapsed: u32,
    color: u32,
    finished: bool,
    active: bool,
}
impl Emitter {
    pub fn new(definition: Definition) -> Self {
        let color = definition.color;
        Self {
            definition,
            elapsed: 0,
            color,
            finished: false,
            active: false,
        }
    }
    pub fn reset(&mut self) {
        self.elapsed = 0;
        self.color = self.definition.color;
        self.finished = false;
        self.active = false;
    }
    /// Input delta has already been scaled by the GFX owner's clock. Visibility
    /// and priority gate publication, not keypoint progression (0x70ba00).
    ///
    /// The priority half is a viewer policy and its sense is the *opposite* of
    /// the native gate it sits next to: `0x70ba44` jumps on `Priority > detail`
    /// to `0x70bd0b`, which is `mov $0x1,%al; ret $0x4`, so a higher priority
    /// exempts an element from the visibility test at `0x6fe120` instead of
    /// suppressing it. Reproducing the native rule needs that camera-relative
    /// test, which this emitter is not given, so the limit stays a manager-wide
    /// budget knob. `gfx_catalog::priority_exempts` carries the dumped direction
    /// for whoever adds the test.
    pub fn tick(
        &mut self,
        delta_ms: u32,
        visible: bool,
        manager_priority: i32,
        parent: [f32; 16],
        outer: Option<[f32; 4]>,
    ) -> Result<Option<DynamicLight>, String> {
        if self.finished {
            return Ok(None);
        }
        self.elapsed = self.elapsed.wrapping_add(delta_ms);
        if self.elapsed >= self.definition.start_ms {
            self.active = true;
            let ms = self.elapsed - self.definition.start_ms;
            if self.definition.duration_ms.is_some_and(|d| ms >= d) {
                self.finished = true;
                return Ok(None);
            }
            if let Some(track) = &self.definition.track {
                if let Some(color) = track.sample(ms as f32 / 1000.) {
                    self.color = color;
                }
            }
        } else if !self.active {
            return Ok(None);
        }
        // DWORD wrap before StartTime leaves a previously active keypoint's
        // process data intact in 0x6edd80; it does not unpublish that live key.
        // Viewer policy, not the native comparison: see `tick`'s doc comment and
        // `gfx_catalog::priority_exempts` for the dumped direction.
        if !visible || self.definition.priority > manager_priority {
            return Ok(None);
        }
        // Hidden or low-priority emitters still advance their keypoint clocks,
        // but their parent transform and outer tint are not consumed until a
        // light can actually be published. Avoid validating the same unused
        // matrix once per hidden light on every frame.
        if parent.iter().any(|v| !v.is_finite())
            || outer.is_some_and(|v| v.iter().any(|v| !v.is_finite() || *v < 0.))
        {
            return Err("invalid GFX light parent/outer color".into());
        }
        let mut color = [16, 8, 0, 24].map(|shift| ((self.color >> shift) & 255) as f32 / 255.);
        if let Some(outer) = outer {
            for i in 0..4 {
                color[i] =
                    (((color[i] * outer[i]) as f64 * 255.).min(255.).trunc() as u32) as f32 / 255.;
            }
        }
        let p = self.definition.position;
        let position = std::array::from_fn(|i| {
            parent[i] * p[0] + parent[i + 4] * p[1] + parent[i + 8] * p[2] + parent[i + 12]
        });
        if position.iter().any(|v: &f32| !v.is_finite()) {
            return Err("nonfinite GFX world light position".into());
        }
        Ok(Some(DynamicLight {
            position,
            radius: self.definition.range,
            color,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDENTITY: [f32; 16] = [
        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ];
    fn file() -> String {
        "MOXTVersion: 37\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 130\nRepeatCount: 1\nRepeatDelay: 0\nBindEle: \nDummyEle: \nIsDummy: 0\nMatchGrnd: 0\nLightType: 1\nPriority: 4\nRange: 10\nStartTime: 100\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: 1000\nPosition: 1, 2, 3\nColor: -2139062144\nScale: 7\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n".into()
    }
    /// A light with more than one keypoint must decode as a sequence, not be
    /// refused and not silently collapse to its first keypoint.
    ///
    /// No light element in the staged corpus declares more than one, so this is
    /// the format's shape rather than a shipped case -- which is exactly why it
    /// needs a test: the data cannot catch a regression here.
    #[test]
    fn a_multi_keypoint_light_decodes_as_a_sequence() {
        let two = file()
            .replace("KEYPOINTCOUNT: 1", "KEYPOINTCOUNT: 2")
            .replace(
                "CtrlMethodCount: 0\n",
                "CtrlMethodCount: 0\nInterpolateMode: 0\nTimeSpan: 2000\n\
                 Position: 4, 5, 6\nColor: -1\nScale: 1\n\
                 Direction: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n",
            );
        let all = parse(&two).unwrap();
        assert_eq!(all.len(), 2, "both keypoints must survive");
        assert_eq!(all[0].keypoint_count, 2);
        assert_eq!(all[0].keypoint_index, 0);
        assert_eq!(all[1].keypoint_index, 1);
        // The element-wide fields are shared; the keypoint-specific ones differ.
        assert_eq!(all[0].priority, all[1].priority);
        assert_eq!(all[0].range, all[1].range);
        assert_eq!(all[0].position, [1., 2., 3.]);
        assert_eq!(all[1].position, [4., 5., 6.]);
    }

    /// A count past the bound is named, not looped to.
    #[test]
    fn an_implausible_keypoint_count_names_itself() {
        let many = file().replace("KEYPOINTCOUNT: 1", "KEYPOINTCOUNT: 9");
        let err = parse(&many).unwrap_err();
        assert!(err.contains("keypoint count 9"), "{err}");
        assert!(err.contains("limit 8"), "{err}");
        let zero = file().replace("KEYPOINTCOUNT: 1", "KEYPOINTCOUNT: 0");
        assert!(parse(&zero).unwrap_err().contains("keypoint count 0"));
    }

    /// A non-zero `InterpolateMode` blends between keypoints. Nothing shipped
    /// uses one, so it is refused by name instead of being treated as a step.
    #[test]
    fn a_blending_interpolation_mode_is_refused_by_name() {
        let blend = file().replace("InterpolateMode: 0", "InterpolateMode: 1");
        let err = parse(&blend).unwrap_err();
        assert!(err.contains("interpolation mode"), "{err}");
    }

    #[test]
    fn actual_decoding_start_end_parent_transform_and_scale_semantics() {
        let def = parse(&file()).unwrap().pop().unwrap();
        let mut emitter = Emitter::new(def);
        assert!(emitter.tick(99, true, 4, IDENTITY, None).unwrap().is_none());
        let mut parent = IDENTITY;
        parent[0] = 2.;
        parent[5] = 3.;
        parent[10] = 4.;
        parent[12] = 10.;
        let light = emitter.tick(1, true, 4, parent, None).unwrap().unwrap();
        assert_eq!(light.position, [12., 6., 12.]);
        assert_eq!(light.radius, 10.); // neither parent nor kp scale changes range
        assert_eq!(light.color, [128. / 255.; 4]);
        assert!(emitter.tick(999, true, 4, parent, None).unwrap().is_some());
        assert!(emitter.tick(1, true, 4, parent, None).unwrap().is_none());
        assert!(emitter.tick(0, true, 4, parent, None).unwrap().is_none());
        emitter.reset();
        assert!(emitter.tick(100, true, 4, parent, None).unwrap().is_some());
    }
    /// A controller 108 window is milliseconds in the file and seconds inside
    /// `ColorTrack`, so it has to be converted at construction. With the raw
    /// milliseconds a controller that starts at 500 ms never opened at all — the
    /// emitter kept the keypoint colour (0x80808080) for its whole life — and a
    /// window that ended never expired.
    #[test]
    fn the_color_ramp_window_is_converted_from_milliseconds() {
        let ramp = file().replace("TimeSpan: 1000", "TimeSpan: 3000").replace(
            "CtrlMethodCount: 0\n",
            "CtrlMethodCount: 1\nCtrlType: 108\nStartTime: 500\nEndTime: 2500\n\
                 Color: 0\nCount: 1\nColor: -1\nTimeSpan: 1000\n",
        );
        let mut emitter = Emitter::new(parse(&ramp).unwrap().pop().unwrap());

        // Before the window opens the keypoint colour is preserved.
        let light = emitter.tick(100, true, 4, IDENTITY, None).unwrap().unwrap();
        assert_eq!(light.color, [128. / 255.; 4]);

        // 700 ms later the window is open and the ramp is 700 ms into its one
        // second from black to white.
        let light = emitter.tick(700, true, 4, IDENTITY, None).unwrap().unwrap();
        let want = ColorTrack::new(0.5, 2.5, 0, &[(u32::MAX, 1000.)])
            .unwrap()
            .sample(0.7)
            .expect("0.7 s is inside the converted window");
        let want = [16, 8, 0, 24].map(|shift| ((want >> shift) & 255) as f32 / 255.);
        assert_eq!(light.color, want);
        assert_ne!(
            light.color,
            [128. / 255.; 4],
            "an unconverted window leaves the ramp shut for its whole life"
        );
    }

    #[test]
    fn hidden_lights_defer_parent_validation_until_publication() {
        let mut hidden = Emitter::new(parse(&file()).unwrap().pop().unwrap());
        let mut invalid = IDENTITY;
        invalid[12] = f32::NAN;
        assert!(
            hidden.tick(100, false, 4, invalid, None).unwrap().is_none(),
            "a hidden light advances without using or validating its parent"
        );

        let mut visible = Emitter::new(parse(&file()).unwrap().pop().unwrap());
        assert!(
            visible.tick(100, true, 4, invalid, None).is_err(),
            "the same invalid parent is rejected as soon as the light is publishable"
        );
    }

    #[test]
    fn hidden_and_low_priority_lights_still_progress_before_publication_gate() {
        let mut e = Emitter::new(parse(&file()).unwrap().pop().unwrap());
        assert!(e.tick(100, false, 4, IDENTITY, None).unwrap().is_none());
        assert!(e.tick(500, true, 3, IDENTITY, None).unwrap().is_none());
        assert!(e.tick(500, true, 4, IDENTITY, None).unwrap().is_none());
        let infinite = file().replace("TimeSpan: 1000", "TimeSpan: -1");
        let mut e = Emitter::new(parse(&infinite).unwrap().pop().unwrap());
        let light = e
            .tick(100, true, 4, IDENTITY, Some([0.5, 1., 0., 0.5]))
            .unwrap()
            .unwrap();
        assert_eq!(light.color, [64. / 255., 128. / 255., 0., 64. / 255.]);
    }
    #[test]
    fn unsupported_or_corrupt_light_layouts_are_errors_not_fake_lights() {
        for (a, b) in [
            ("RepeatCount: 1", "RepeatCount: 2"),
            ("KEYPOINTCOUNT: 1", "KEYPOINTCOUNT: 2"),
            ("CtrlMethodCount: 0", "CtrlMethodCount: 2"),
            ("LightType: 1", "LightType: 2"),
            ("Range: 10", "Range: NaN"),
            ("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2"),
            ("BindEle: ", "BindEle: parent"),
        ] {
            assert!(parse(&file().replace(a, b)).is_err(), "{a}");
        }
    }

    /// The light element is version invariant, so the same record must read the
    /// same at any version a shipped client writes.
    ///
    /// `A3DGFXLight::Load` takes `dwVersion` and never tests it: fourteen
    /// unversioned lines, from `LightType` through `Phi`. The reader used to cap
    /// at 79 because that was the newest version in the 1.4.2 corpus, which
    /// made every 1.5.2 effect that carries a point light fail to decode and so
    /// lost its geometry too. The SDK's history runs to V101 with
    /// `GFX_CUR_VERSION 103`, and 1.5.2 ships 86, 90 and 93 roots.
    #[test]
    fn a_light_record_reads_the_same_at_every_shipped_version() {
        for version in [37, 79, 86, 90, 93] {
            let source = file().replace("Version: 37", &format!("Version: {version}"));
            let definitions =
                parse(&source).unwrap_or_else(|e| panic!("version {version} refused: {e}"));
            assert_eq!(definitions.len(), 1);
            assert!(definitions[0].range > 0.);
        }
    }
    #[test]
    fn a_skipped_element_that_declares_lights_is_an_explicit_error() {
        // An element carrying the light marker LightType that this reader
        // cannot execute must fail the root rather than be silently dropped.
        let extra = "GFXELEMENTID: 106\nLightType: 1\nSegs: 3\nLightNum: 4\n";
        let source = file().replace("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2") + extra;
        let error = parse(&source).unwrap_err();
        assert!(
            error.contains("unsupported GFX light element 106"),
            "{error}"
        );
        // A non-light element is skipped, so a root that also carries particles
        // still yields its point lights.
        let particles = "GFXELEMENTID: 200\nParticleCount: 12\nEmitSpeed: 3\n";
        let source = file().replace("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2") + particles;
        assert_eq!(parse(&source).unwrap().len(), 1);
    }

    /// The effect noise block is not a light. Retail element 101/123/150/152
    /// records author `BufLen`/`Amplitude`/`WaveLen: <int>`/`Persistence`/
    /// `OctaveNum`, and the line families add `StartPos`/`EndPos`/`Segs`/
    /// `LightNum`/`Interval`/`Width`/`Alpha`/`Pos1Enable`/`Filter`. Treating
    /// those keys as lights rejected 2,014 of 5,459 retail roots and loaded 0
    /// of the 524 roots that contain element 130.
    #[test]
    fn effect_noise_and_line_blocks_are_not_light_paths() {
        // Element 101 (particle) with the full noise block, verbatim shape from
        // `gfx/models/暗语者营地/暗语者营地.gfx`.
        let particle = "GFXELEMENTID: 101\nName: Unnamed3\nSrcBlend: 5\nDestBlend: 2\n\
                        TexFile: 粒子\\26.dds\nBufLen: 256\nAmplitude: 2.000000\n\
                        WaveLen: 1\nPersistence: 1.000000\nOctaveNum: 5\n";
        let source = file().replace("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2") + particle;
        assert_eq!(parse(&source).unwrap().len(), 1);

        // Element 150 (range light) with its own record, verbatim shape from
        // `gfx/models/暗语者营地/暗语者营地.gfx`. It declares no LightType, so it
        // is not a point light and the root still yields element 130.
        let range = "GFXELEMENTID: 150\nName: 11\nTexFile: 闪电\\lightning002.dds\n\
                     BufLen: 256\nAmplitude: 1.000000\nWaveLen: 1\nPersistence: 1.000000\n\
                     OctaveNum: 5\nStartPos: 0.000000, 0.000000, 0.000000\n\
                     EndPos: 0.000000, 0.000000, 0.000000\nSegs: 10\nLightNum: 1\n\
                     WaveLen: 0.100000\nInterval: 70\nWidth: 0.400000\nAlpha: 1.000000\n\
                     Amplitude: 1.000000\nPos1Enable: 0\nPos2Enable: 0\nUseNormal: 0\n\
                     Normal: 0.000000, 0.000000, 0.000000\nFilter: 1\n";
        let source = file().replace("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2") + range;
        assert_eq!(parse(&source).unwrap().len(), 1);

        // Element 152 (trail) with the lightning/branch block and LightNum: 0.
        let trail = "GFXELEMENTID: 152\nName: Unnamed1\nTexFile: 轨迹\\trail003.dds\n\
                     Segs: 20\nLightNum: 0\nWaveLen: 1.000000\nInterval: 100\n\
                     Width: 0.070000\nAlpha: 1.000000\nAmplitude: 0.000000\n\
                     Pos1Enable: 0\nPos2Enable: 0\nNormal: 0.000000, 0.000000, 0.000000\n\
                     Filter: 2\n";
        let source = file().replace("GFXELEMENTCOUNT: 1", "GFXELEMENTCOUNT: 2") + trail;
        assert_eq!(parse(&source).unwrap().len(), 1);
    }

    /// Element 130 without the marker is malformed: it can no longer be found
    /// by the light pass at all, so it must not be treated as a skipped sprite.
    #[test]
    fn element_130_without_light_type_is_an_error() {
        let source = file().replace("LightType: 1\n", "");
        let error = parse(&source).unwrap_err();
        assert!(error.contains("missing its LightType"), "{error}");
    }

    #[test]
    fn controller_108_is_applied_before_packed_outer_color() {
        let source=file().replace("StartTime: 100","StartTime: 0").replace("TimeSpan: 1000","TimeSpan: -1").replace("CtrlMethodCount: 0", "CtrlMethodCount: 1\nCtrlType: 108\nStartTime: 0\nEndTime: -1\nColor: 0\nCount: 1\nColor: 2139062143\nTimeSpan: 1000");
        let mut e = Emitter::new(parse(&source).unwrap().pop().unwrap());
        // PC24 table index 63 holds byte 63, not the extended-precision 62.
        assert_eq!(
            e.tick(500, true, 4, IDENTITY, None).unwrap().unwrap().color,
            [63. / 255.; 4]
        );
        assert_eq!(
            e.tick(500, true, 4, IDENTITY, None).unwrap().unwrap().color,
            [127. / 255.; 4]
        );
    }
    #[test]
    fn infinite_key_preserves_active_state_across_owner_dword_wrap() {
        let source = file().replace("TimeSpan: 1000", "TimeSpan: -1");
        let mut e = Emitter::new(parse(&source).unwrap().pop().unwrap());
        assert!(e.tick(100, true, 4, IDENTITY, None).unwrap().is_some());
        e.elapsed = u32::MAX;
        assert!(e.tick(1, true, 4, IDENTITY, None).unwrap().is_some());
        e.reset();
        assert!(e.tick(0, true, 4, IDENTITY, None).unwrap().is_none());
    }
}
