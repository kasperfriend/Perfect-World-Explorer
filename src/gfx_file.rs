//! Reader for the retail `.gfx` text container: every element class, every
//! controller class, and the exact repeated-key sequences the client writes.
//!
//! # Evidence
//!
//! The grammar below is not guessed. It was recovered from
//! `elementclient.exe` (32-bit PE, image base 0x400000) and cross-checked
//! against all 5,458 `.gfx` members of `gfx.pck`:
//!
//! * Root loader `0x6e3470` (wrapper `0x6e3c00`): reads `MOXTVersion` first and
//!   rejects anything above 79 (`cmp 0x4f`). Its version gates are
//!   `cmp 0x18/0x19/0x2e/0x32/0x35/0x3f`, i.e. 24/25/46/50/53/63, which is why
//!   `FaceToViewer`, `UseAABB`, `NoChangeDir`/`2DRender`/`2DBackLayer` and
//!   `PhysExist` are optional keys. Live copies of `DedaultScale`/`PlaySpeed`/
//!   `DefaultAlpha` land at root+0x1e8/0x1ec/0x1f0; the AABB centre and half
//!   extent at +0x20c..+0x220; `UseAABB` at +0x26c; `FadeByDist`/`FadeStart`/
//!   `FadeEnd` at +0x26e/+0x270/+0x274; the three 2D flags at +0x2b4..+0x2b6.
//! * Element loader `0x6f99f0` (shared keys) and the sprite loader at
//!   `0x71dc07`: every optional key is version gated, and the gate ladder is
//!   `cmp` against 4/8/9/22/28/32/33/34/41/45/57/58/74/75 for the shared keys
//!   and 9/26/36/42/54/55/61 for `RotFromView`/`GrndNormOnly`/`NoScale`+`OrgPt`/
//!   `ZOffset`/`MatchSurface`/`MaxExtent`/`YawEffect` — the sprite loader walks
//!   the key table at `0x97f1d4`..`0x97f204`, thirteen entries from `Width` to
//!   `YawEffect`, so the table order and the gate order are the same order. The
//!   format strings are `%d` or `%f`, which fixes each native type: `BindEle`
//!   and `DummyEle` are `%s` (element *names*, copied to element+0x9c by
//!   `0x7638a0`), `GroundHeight` is `%d` reduced to a byte by `value != 0` at
//!   element+0x132, `MatchGrnd` is the byte at element+0x131 and `ZEnable` the
//!   byte at element+0x130 — a byte every version below 76 then forces to 1,
//!   which is why `Common::depth_test` keeps the authored value and the rule
//!   lives in `gfx_player::depth_test`. The sprite loader's tail (`0x71e0ff`)
//!   replaces a zero `MaxExtent` with `1.5 * max(Width, Height)` (`fmuls
//!   0x8d75cc`, which holds 1.5) and derives the four corner terms at element
//!   +0x15c/+0x160/+0x164/+0x168 as `2W*OrgPt1`, `2W - that`, `2H*OrgPt2`,
//!   `2H - that`; the corner builder at `0x71ea70` multiplies each by the
//!   runtime scale unless the matching `NoScale` byte (element+0x151/+0x152) is
//!   set, which is the split `quad_corners` reproduces.
//! * Controller factory `0x6ea910` (`jmp [eax*4+0x6eaddc]`, `eax = type-100`)
//!   gives the 13 controller classes, their object sizes and their field lists;
//!   the field order here matches each class `Load` (105 `0x6ec090`,
//!   106 `0x6ecb40`, 107 `0x6ed070`, 108 `0x6ec240`, 109/112 `0x6ecca0`,
//!   110 `0x6ed4a0`, 111 `0x6ec7f0`, 101 `0x6eb290`, 102 `0x6eb5e0`,
//!   104 `0x6ebd70`).
//! * Particle element `Load 0x7166d0` / `Save 0x716cd0` give the emitter field
//!   order and the offset map (+0x1a4 `Quota`, +0x1ac/+0x1b0 particle size,
//!   +0x1b4 `3DParticle`, +0x1b5 `Facing`, +0x1f8 `ScaleNoOff`, +0x1f9/+0x1fa
//!   `NoScale`, +0x1fc/+0x200 `OrgPt`, +0x23d `IsUseParUV`, +0x23e
//!   `IsStartOnGrnd`, +0x144/+0x150 affector array and count).
//! * The corpus census fixes the ambiguous parts the disassembly cannot: which
//!   keys are written twice under the same name (`TexSpeed`, `NoScale`,
//!   `OrgPt`, `Width`, `Alpha`, `Amplitude`, `WaveLen`, `Pos`, `Amp`,
//!   `EnablePos`, `Attenuation0`, `dwColor`), that `ColorDelta` is four
//!   integers, that `ScaleChage` is rate/min/max, that grid colours are hex
//!   strings, and that element 125 carries a generation-sequence keypoint
//!   block before its affector list.
//!
//! # Reading strategy
//!
//! Keys are read positionally where the client reads positionally (counts drive
//! keypoints, controllers, affectors, path points and grid colours). Inside an
//! element's *specification* block the client's own writers emit the same keys
//! in 493 different orders for element 100 alone, because every optional key is
//! version gated. Those blocks are therefore collected in file order and read
//! by key, with repeated keys addressed by occurrence index. An unknown key is
//! still an error: nothing in the container may be skipped silently.

use glam::{Quat, Vec3};

/// Highest `MOXTVersion` the client accepts (`cmp 0x4f` in the root loader).
pub const MAX_VERSION: u32 = 79;

/// One decoded `Key: value` line, in file order.
pub type Field = (String, String);

/// Decode a container from its raw bytes. The client stores GBK text; names and
/// texture paths are Chinese, so a lossy decode keeps the file readable while
/// the flag reports damage instead of hiding it.
pub fn decode(bytes: &[u8]) -> (String, bool) {
    let (text, _, had_errors) = encoding_rs::GBK.decode(bytes);
    (text.into_owned(), had_errors)
}

/// Split the container into `Key: value` fields. The first colon separates the
/// key, so values may contain colons and commas; blank lines carry no field.
pub fn tokenize(source: &str) -> Vec<Field> {
    let mut fields = Vec::new();
    for line in source.lines() {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once(':') else {
            // A grid (210/211) writes one bare vertex per cell between the
            // colour list and `fGridSize`, and the colour list itself is
            // `dwColor:` lines. Dropping the bare rows loses the whole
            // authored surface: all 216 corpus grid elements carry them, and
            // they are what make a waterfall curtain 23 units tall and 36
            // wide instead of a 0.5-unit flat patch on the floor.
            let row = line.trim();
            // Only inside an element can a bare row be a grid vertex; anything
            // before the first `GFXELEMENTID` is a malformed container and must
            // still be reported as one.
            if !row.is_empty() && fields.iter().any(|(key, _)| key == "GFXELEMENTID") {
                fields.push((GRID_VERTEX.to_string(), row.to_string()));
            }
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        fields.push((key.to_string(), value.trim().to_string()));
    }
    fields
}

/// A parsed `.gfx` root: playback settings, bounds and every element.
#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    pub version: u32,
    pub default_scale: f32,
    pub play_speed: f32,
    pub default_alpha: f32,
    pub raytrace: bool,
    /// Present from version 39 on; absent roots keep the default.
    pub face_to_viewer: bool,
    pub fade_by_dist: bool,
    pub fade_start: f32,
    pub fade_end: f32,
    /// First `Vec:` line of the pair.
    pub aabb_center: Vec3,
    /// Second `Vec:` line: the half extent, not the maximum corner.
    pub aabb_half: Vec3,
    /// Present from version 57 on.
    pub use_aabb: bool,
    /// The authored `ShakeCam` value. The client does not keep it as a bitfield:
    /// `0x6e38d9` tests the scanned integer against itself and `0x6e38de` stores
    /// the `setne` result in a single byte at root+0x1f6, so any nonzero value
    /// means "shake". The corpus agrees — of the 5,457 roots that parse, 5,216
    /// author 0 and 241 author 1, never a bit pattern. `0x6e38e6` then builds the
    /// root-level controller below, and only when this flag is set.
    pub shake_cam: u32,
    /// Root-level controllers, read straight after `ShakeCam` while the next key
    /// is another `CtrlType`. All 241 of the retail ones are `CtrlType: 112`, the
    /// fractal-noise controller the factory table at `0x6eaddc` maps to the camera
    /// shake, and they belong to exactly the roots that set `ShakeCam`.
    /// [`crate::gfx_player::Player::camera_shake`] is what consumes them.
    pub controls: Vec<Control>,
    /// Present from version 50 on.
    pub no_change_dir: bool,
    pub render_2d: bool,
    pub back_layer_2d: bool,
    /// Present from version 64 on.
    pub phys_exist: bool,
    pub elements: Vec<Element>,
}

impl Root {
    /// `0x6e38de` stores `setne(ShakeCam)` in root+0x1f6 and `0x6e4dc9` gates the
    /// shake routine on that byte, so the client's reading is "nonzero", with no
    /// position/scale split to read out of it. The routine it gates is
    /// [`crate::gfx_player::Player::camera_shake`].
    pub fn shakes_camera(&self) -> bool {
        self.shake_cam != 0
    }

    /// Authored bounds. Roots without `UseAABB` still carry the `Vec:` pair the
    /// editor wrote, so the radius is derived the way the loader derives it:
    /// from the half extent, falling back to the elements' own reach.
    pub fn bounds_radius(&self) -> f32 {
        self.aabb_half.length()
    }

    /// Total authored duration in milliseconds, from the keypoint clocks. A
    /// forever keypoint (`TimeSpan: -1`) makes the root loop indefinitely.
    pub fn duration_ms(&self) -> Option<f32> {
        let mut total = 0f32;
        for element in &self.elements {
            let mut clock = element.start_time;
            let mut forever = false;
            for key in &element.keypoints {
                if key.time_span < 0. {
                    forever = true;
                    break;
                }
                clock += key.time_span;
            }
            if forever {
                return None;
            }
            total = total.max(clock);
        }
        Some(total)
    }

    /// True when at least one element draws something this viewer can render.
    /// Light elements are handled by `gfx_light`, sound elements by the audio
    /// path, and model elements need the model queue; the rest are geometry.
    pub fn has_visual_elements(&self) -> bool {
        self.elements.iter().any(|element| element.spec.is_visual())
    }
}

/// One `GFXELEMENTID` record: shared keys, its class payload and its clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub id: u32,
    pub common: Common,
    pub spec: Spec,
    /// Element clock offset in milliseconds.
    pub start_time: f32,
    pub keypoints: Vec<KeyPoint>,
    /// Particle affectors (`AffectorCount`), applied to every live particle.
    pub affectors: Vec<Control>,
    /// Element 125 only: the generation-sequence keypoint that carries the
    /// path controller particles are spawned along.
    pub generation_sequence: Option<Box<KeyPoint>>,
}

impl Element {
    /// Duration of the element clock in milliseconds, or `None` when a keypoint
    /// runs forever.
    pub fn duration_ms(&self) -> Option<f32> {
        let mut clock = 0f32;
        for key in &self.keypoints {
            if key.time_span < 0. {
                // A negative span is "hold this keypoint forever", the same
                // reading `sample` already gives it by turning it into
                // `INFINITY`. Returning `None` here is what it must do: the
                // waterfall curtain is a single keypoint at `-1`, and summing
                // it as a duration produced `cycle = -1`, which `element_phase`
                // reads as "finished before it started" and pins to phase 0.
                // 775 elements across 180 roots are authored this way, so this
                // one line was freezing every one of them at their first frame.
                return None;
            }
            clock += key.time_span;
        }
        Some(clock)
    }

    /// The keypoint in effect at `time` milliseconds into the element clock,
    /// plus how far into that keypoint the clock is.
    pub fn sample(&self, time: f32) -> Option<(&KeyPoint, f32)> {
        let mut clock = 0f32;
        for key in &self.keypoints {
            let span = if key.time_span < 0. {
                f32::INFINITY
            } else {
                key.time_span
            };
            if time < clock + span || span.is_infinite() {
                return Some((key, (time - clock).max(0.)));
            }
            clock += span;
        }
        None
    }
}

/// The keys every element class shares, in the order the client writes them.
#[derive(Clone, Debug, PartialEq)]
pub struct Common {
    pub name: String,
    /// D3D8 `D3DBLEND` values: 1 zero, 2 one, 3 srccolor, 4 invsrccolor,
    /// 5 srcalpha, 6 invsrcalpha, 7 destalpha, 8 invdestalpha, 9 destcolor,
    /// 10 invdestcolor, 11 srcalpha saturation.
    pub source_blend: u32,
    pub destination_blend: u32,
    /// 1 plays once, 0 loops; the client's editor default is 1.
    pub repeat_count: i32,
    pub repeat_delay: f32,
    /// Texture path relative to `Gfx\Textures`, empty for untextured elements.
    pub texture: String,
    /// Name of another element this one binds to, empty when unbound.
    pub bind_element: String,
    /// The authored `ZEnable`, kept verbatim: the client reads it into the byte
    /// at element+0x130 from version 8 on and then overwrites that byte with 1
    /// for every version below 76 (`0x6f9e3a`: `cmp $0x4c,%eax; jae …; movb
    /// $0x1,0x130(%edi)`). 334 of the corpus's 390 `ZEnable: 0` elements sit in
    /// those older roots, so the version rule decides most of them — it is
    /// applied where the quad is built (`gfx_player::depth_test`) and by
    /// `gfx_decal`, not here, so this field stays the authored value.
    pub depth_test: bool,
    /// Match the ground. The client reads it from version 22 on into the byte at
    /// element+0x131; that gate is unreachable because a root below version 24
    /// is rejected first, so the key is read unconditionally.
    pub match_ground: bool,
    /// Read from version 57 on as `GroundHeight: %d` and reduced to the byte at
    /// element+0x132 by `value != 0`, so natively this is a flag rather than a
    /// height. The authored number is kept so the 28 corpus elements that set
    /// it stay inspectable.
    pub ground_height: f32,
    /// Flipbook rows; 1 disables the flipbook.
    pub tex_row: u32,
    /// Flipbook columns; 1 disables the flipbook.
    pub tex_col: u32,
    /// Milliseconds per flipbook frame.
    pub tex_interval: u32,
    pub priority: i32,
    pub is_dummy: bool,
    pub dummy_element: String,
    pub warp: bool,
    pub tile_mode: u32,
    /// Two values under the same key: the U and V scroll speeds.
    pub tex_speed: [f32; 2],
    pub reverse_u: bool,
    pub reverse_v: bool,
    pub exchange_uv: bool,
    pub render_layer: i32,
    pub no_down_sample: bool,
    pub reset_loop_end: bool,
    pub tex_anim_max_time: f32,
    /// Two `NoScale` values under the same key: width, then height.
    pub no_scale: [bool; 2],
    /// Two `OrgPt` values under the same key: the width and height anchors.
    pub origin: [f32; 2],
}

impl Default for Common {
    fn default() -> Self {
        Self {
            name: String::new(),
            // The client's slot manager defaults to the additive pair that
            // 30,569 of the 31,019 authored elements also write explicitly.
            source_blend: 5,
            destination_blend: 2,
            repeat_count: 1,
            repeat_delay: 0.,
            texture: String::new(),
            bind_element: String::new(),
            depth_test: false,
            match_ground: false,
            ground_height: 0.,
            tex_row: 1,
            tex_col: 1,
            tex_interval: 0,
            priority: 0,
            is_dummy: false,
            dummy_element: String::new(),
            warp: false,
            tile_mode: 0,
            tex_speed: [0.; 2],
            reverse_u: false,
            reverse_v: false,
            exchange_uv: false,
            render_layer: 0,
            no_down_sample: false,
            reset_loop_end: false,
            tex_anim_max_time: 0.,
            no_scale: [false; 2],
            origin: [0.5; 2],
        }
    }
}

/// One keypoint of the element clock.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPoint {
    pub interpolate_mode: u32,
    /// Milliseconds; -1 keeps the keypoint active forever.
    pub time_span: f32,
    pub position: Vec3,
    /// Packed 0xAARRGGBB, stored signed in the file (`Color: -1` is white).
    pub color: u32,
    pub scale: f32,
    /// Quaternion as `x, y, z, w`.
    pub direction: Quat,
    pub rad_2d: f32,
    pub controls: Vec<Control>,
}

impl Default for KeyPoint {
    fn default() -> Self {
        Self {
            interpolate_mode: 0,
            time_span: -1.,
            position: Vec3::ZERO,
            color: 0xffff_ffff,
            scale: 1.,
            direction: Quat::IDENTITY,
            rad_2d: 0.,
            controls: Vec::new(),
        }
    }
}

impl KeyPoint {
    pub fn alpha(&self) -> f32 {
        ((self.color >> 24) & 0xff) as f32 / 255.
    }

    /// Linear RGB in 0..1, without the alpha byte.
    pub fn rgb(&self) -> [f32; 3] {
        [
            ((self.color >> 16) & 0xff) as f32 / 255.,
            ((self.color >> 8) & 0xff) as f32 / 255.,
            (self.color & 0xff) as f32 / 255.,
        ]
    }
}

/// One controller record. `start`/`end` are milliseconds on the keypoint clock
/// and `end < 0` means "until the element stops".
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub kind: u32,
    pub start: f32,
    pub end: f32,
    pub data: ControlData,
}

/// Which noise controller a `Fractal` payload belongs to; `ID_KPCTRL_CL_NOISE`
/// (107), `ID_KPCTRL_SCA_NOISE` (109) or `ID_KPCTRL_NOISE_BASE` (112) in
/// `A3DGFXKeyPoint.h`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoiseKind {
    Color,
    Scale,
    Base,
}

/// The 13 controller payloads, with the client's own field names.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlData {
    /// 100: straight-line motion along `dir`.
    Motion { dir: Vec3, vel: f32, acc: f32 },
    /// 101: scalar speed change (particle speed over life).
    Speed { vel: f32, acc: f32 },
    /// 102: rotation about `axis` through `pos` — spins the keypoint
    /// direction (`A3DGFXCtrlRotAxis::Process` rotates `m_vDir`) and orbits
    /// the position around the line.
    Orbit {
        pos: Vec3,
        axis: Vec3,
        vel: f32,
        acc: f32,
    },
    /// 103: revolution about `axis` through `pos`. `A3DGFXCtrlRevol::Process`
    /// moves only the position; the direction is left alone (only 102 spins
    /// it), so this is a separate variant.
    Revol {
        pos: Vec3,
        axis: Vec3,
        vel: f32,
        acc: f32,
    },
    /// 104: `A3DGFXCtrlCentriMove::Process` steps the position toward (or,
    /// with a negative step, away from) the centre `pos + m_vAxisOff` by the
    /// integrated distance, clamping at the centre on overshoot. The old
    /// `vel == -1` / `dspl == -1` sentinels came from a disassembly reading
    /// and are not in the source.
    Attract { pos: Vec3, vel: f32, acc: f32 },
    /// 105: per-channel colour rate, four signed integers per second.
    ColorDelta { delta: [i32; 4] },
    /// 106: scale rate per second with the authored clamp.
    ScaleChange { rate: f32, min: f32, max: f32 },
    /// 107, 109 and 112: fractal noise. The three share the payload but act
    /// differently in `Process`: 109 (`A3DGFXCtrlScaleNoise`) writes the
    /// `PROC_DATA::m_fScaleNoise` slot the keypoint read adds to the output
    /// scale, 107 (`A3DGFXCtrlColorNoise`) drives the alpha of a colour whose
    /// RGB come from `BaseColor`, and 112 (`A3DGFXCtrlNoiseBase`) is the
    /// shared generator holder and does nothing on its own.
    Fractal {
        kind: NoiseKind,
        buffer_length: u32,
        amplitude: f32,
        wave_length: f32,
        persistence: f32,
        octaves: u32,
        base_color: Option<u32>,
    },
    /// 108: colour ramp, up to ten `(color, milliseconds)` stops.
    ColorRamp {
        original: u32,
        stops: Vec<(u32, f32)>,
    },
    /// 110: path through space; `calc_dir` re-aims the element along it.
    Path { calc_dir: bool, points: Vec<Vec3> },
    /// 111: scale ramp, one leading scale then `(scale, milliseconds)` stops.
    ScaleRamp { first: f32, stops: Vec<(f32, f32)> },
}

/// The per-class payload of an element.
#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    /// 100, 101, 102, 103, 104: one textured quad.
    Sprite(Sprite),
    /// 110: a ribbon that follows the element and lives for `segment_life`.
    Ribbon(Ribbon),
    /// 120..125: a particle emitter.
    Particle(Particle),
    /// 130: a point light. `gfx_light` owns the rendering of this class.
    Light(Light),
    /// 140: an open cylinder.
    Cylinder(Cylinder),
    /// 150: a fractal ribbon (magic circles, ground waves).
    Fractal(Fractal),
    /// 151: a branching lightning bolt.
    Lightning(Lightning),
    /// 152: a fractal trail with append/tail behaviour (weapon trails).
    Trail(Trail),
    /// 160: an embedded model.
    Model(ModelRef),
    /// 170: a sound. Not a visual effect; parsed so the clock stays exact.
    Sound(SoundRef),
    /// 180: a wave trail.
    Wave(Wave),
    /// 190: a coefficient-driven curve.
    Curve(Curve),
    /// 200..204: a nested `.gfx` root.
    Child(Child),
    /// 210, 211: a coloured cell grid.
    Grid(Grid),
    /// An element id the container may carry but the client never instantiates.
    Unknown { id: u32 },
}

impl Spec {
    /// True for the classes this viewer rasterises itself.
    pub fn is_visual(&self) -> bool {
        !matches!(
            self,
            Spec::Light(_) | Spec::Sound(_) | Spec::Model(_) | Spec::Unknown { .. }
        )
    }
}

/// Sprite quad geometry. `no_scale` and `origin` carry the two values the
/// client writes under the same key: one for width, one for height.
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub width: f32,
    pub height: f32,
    pub rotate_from_view: bool,
    pub ground_normal_only: bool,
    pub no_scale: [bool; 2],
    /// Anchor fractions in 0..1; 0.5 centres the quad on the element origin.
    pub origin: [f32; 2],
    /// Read from version 42 on; element+0x16c.
    pub z_offset: f32,
    /// Read from version 54 on; the byte at element+0x170.
    pub match_surface: bool,
    /// Read from version 55 on into element+0x174, then replaced by
    /// `1.5 * max(width, height)` whenever it is still zero — a tail the loader
    /// runs after every gate, so it also covers the 3,250 corpus sprites that
    /// carry no `MaxExtent` key and the 133 that author zero. 3,038 of the
    /// authored values are exactly this expression, the editor writing the same
    /// default back out.
    pub max_extent: f32,
    /// Read from version 61 on; the byte at element+0x195. Selects the yaw
    /// orientation path at `0x71f36d`/`0x71f84e`.
    pub yaw_effect: bool,
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            width: 0.,
            height: 0.,
            rotate_from_view: false,
            ground_normal_only: false,
            no_scale: [false; 2],
            origin: [0.5; 2],
            z_offset: 0.,
            match_surface: false,
            max_extent: 0.,
            yaw_effect: false,
        }
    }
}

/// Ribbon (element 110): two anchor points, a material flag and a segment life.
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    pub origin_1: Vec3,
    pub origin_2: Vec3,
    pub enable_material: bool,
    pub enable_origin_1: bool,
    pub enable_origin_2: bool,
    /// Milliseconds a ribbon segment stays alive after it is emitted.
    pub segment_life: f32,
    pub bind: i32,
}

impl Default for Ribbon {
    fn default() -> Self {
        Self {
            origin_1: Vec3::ZERO,
            origin_2: Vec3::ZERO,
            enable_material: false,
            enable_origin_1: false,
            enable_origin_2: false,
            segment_life: 0.,
            bind: 0,
        }
    }
}

/// Particle emitter (elements 120..125). Field names follow the container.
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    /// Hard cap on live particles; -1 is unlimited.
    pub quota: i32,
    pub width: f32,
    pub height: f32,
    pub is_3d: bool,
    /// 0 faces the camera, 1 keeps the emission direction.
    pub facing: u32,
    pub scale_no_offset: bool,
    pub no_scale: [bool; 2],
    pub origin: [f32; 2],
    pub use_particle_uv: bool,
    pub start_on_ground: bool,
    /// Particles per second.
    pub emission_rate: f32,
    /// Emission cone half angle in radians.
    pub angle: f32,
    pub speed: f32,
    /// Acceleration along the particle's own direction.
    pub particle_acc: f32,
    pub acc_dir: Vec3,
    /// Global acceleration magnitude applied along `acc_dir`.
    pub acc: f32,
    /// Seconds.
    pub ttl: f32,
    pub color_min: u32,
    pub color_max: u32,
    pub scale_min: f32,
    pub scale_max: f32,
    /// Radians.
    pub rot_min: f32,
    pub rot_max: f32,
    pub is_surface: bool,
    pub is_bind: bool,
    pub is_drag: bool,
    pub drag_power: f32,
    pub initial_direction: Vec3,
    pub use_hsv_interpolation: bool,
    /// Emitters 121, 123 and 124: the *diameters* of the spawn ellipsoid. The
    /// client fills its shape matrix as `diag(AreaSize * 0.5)` (`0x729670`) and
    /// multiplies a uniform unit-ball sample by it (`0x728270`), so the volume is
    /// an ellipsoid of semi-axes `AreaSize / 2`, not a box of those half extents.
    pub area_size: Option<Vec3>,
    /// Sphere emitter (125): the spawn radius.
    pub radius: Option<f32>,
    pub is_average_generation: Option<bool>,
    pub alpha_segments: Option<u32>,
    pub beta_segments: Option<u32>,
    pub average_generation_segments: Option<u32>,
    pub is_average_generation_sequence: Option<bool>,
}

impl Default for Particle {
    fn default() -> Self {
        Self {
            quota: -1,
            width: 0.,
            height: 0.,
            is_3d: false,
            facing: 0,
            scale_no_offset: false,
            no_scale: [false; 2],
            origin: [0.5; 2],
            use_particle_uv: false,
            start_on_ground: false,
            emission_rate: 0.,
            angle: 0.,
            speed: 0.,
            particle_acc: 0.,
            acc_dir: Vec3::new(0., 0., -1.),
            acc: 0.,
            ttl: 0.,
            color_min: 0xffff_ffff,
            color_max: 0xffff_ffff,
            scale_min: 1.,
            scale_max: 1.,
            rot_min: 0.,
            rot_max: 0.,
            is_surface: false,
            is_bind: false,
            is_drag: false,
            drag_power: 1.,
            initial_direction: Vec3::ZERO,
            use_hsv_interpolation: false,
            area_size: None,
            radius: None,
            is_average_generation: None,
            alpha_segments: None,
            beta_segments: None,
            average_generation_segments: None,
            is_average_generation_sequence: None,
        }
    }
}

/// Point light (element 130). `gfx_light` renders this class; the fields are
/// kept here so the container reader is complete.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub light_type: u32,
    pub diffuse: u32,
    pub specular: u32,
    pub ambient: u32,
    pub position: Vec3,
    pub direction: Vec3,
    pub range: f32,
    pub falloff: f32,
    /// Constant, linear and quadratic attenuation, all written as
    /// `Attenuation0`.
    pub attenuation: [f32; 3],
    pub theta: f32,
    pub phi: f32,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            light_type: 1,
            diffuse: 0xffff_ffff,
            specular: 0,
            ambient: 0,
            position: Vec3::ZERO,
            direction: Vec3::ZERO,
            range: 10.,
            falloff: 0.,
            attenuation: [0.; 3],
            theta: 0.,
            phi: 0.,
        }
    }
}

/// Open cylinder (element 140).
#[derive(Clone, Debug, PartialEq)]
pub struct Cylinder {
    pub radius: f32,
    pub height: f32,
    /// Texture pitch in radians.
    pub pitch: f32,
    pub sections: u32,
    pub no_radius_scale: bool,
    pub no_height_scale: bool,
    pub origin_at_center: bool,
}

impl Default for Cylinder {
    fn default() -> Self {
        Self {
            radius: 0.,
            height: 0.,
            pitch: 0.,
            sections: 8,
            no_radius_scale: false,
            no_height_scale: false,
            origin_at_center: false,
        }
    }
}

/// Shared fractal payload of elements 150, 151 and 152.
#[derive(Clone, Debug, PartialEq)]
pub struct Fractal {
    pub buffer_length: u32,
    /// First `Amplitude`: the noise amplitude.
    pub noise_amplitude: f32,
    /// First `WaveLen`: the noise wavelength, written as an integer.
    pub noise_wave_length: f32,
    pub persistence: f32,
    pub octaves: u32,
    pub start_position: Vec3,
    pub end_position: Vec3,
    pub segments: u32,
    pub light_number: u32,
    /// Second `WaveLen`: the ribbon wavelength, written as a float.
    pub wave_length: f32,
    /// Milliseconds between rebuilds.
    pub interval: u32,
    /// The three authored `Width` lines, in file order: see
    /// [`Fractal::width_at`] for what each one means.
    pub widths: Vec<f32>,
    /// The three authored `Alpha` lines, in file order.
    pub alphas: Vec<f32>,
    /// Second `Amplitude`: the ribbon displacement amplitude.
    pub amplitude: f32,
    pub position_1_enable: bool,
    pub position_2_enable: bool,
    pub use_normal: bool,
    pub normal: Vec3,
    pub filter: u32,
}

impl Default for Fractal {
    fn default() -> Self {
        Self {
            buffer_length: 256,
            noise_amplitude: 1.,
            noise_wave_length: 1.,
            persistence: 1.,
            octaves: 5,
            start_position: Vec3::ZERO,
            end_position: Vec3::ZERO,
            segments: 10,
            light_number: 0,
            wave_length: 1.,
            interval: 100,
            widths: vec![0.1],
            alphas: vec![1.],
            amplitude: 0.,
            position_1_enable: false,
            position_2_enable: false,
            use_normal: false,
            normal: Vec3::ZERO,
            filter: 1,
        }
    }
}

impl Fractal {
    /// The authored cross-section width at fraction `t` of the strip.
    ///
    /// The client does not treat the three `Width` lines as independent
    /// channels. Its strip builder reads `+0x2c0` and `+0x2c8` — the first and
    /// third lines — as the width at the two *ends* of the strip and `+0x2c4`,
    /// the second line, as the width at its midpoint:
    ///
    /// * for `t >= 0.5` (disassembly `0x709a95`) the pair it interpolates is
    ///   `Width[2] -> Width[0]`, i.e. end-to-end,
    /// * for `t < 0.5` (`0x709a9d`, `0x709aeb`, `0x709ad5`) it interpolates
    ///   `Width[1] -> Width[0]`, a plain linear ramp.
    ///
    /// The one `call` in the second branch (`0x709ad8` -> `0x633700`) is not an
    /// easing function: that routine multiplies a three-float vector in `ecx` by
    /// the scalar it was passed (`fld [esp+4]` / `fmul [eax..+8]` / `ret 4`) and
    /// the caller uses it to scale a direction, not the profile fraction. No
    /// `fsin` or `fsin`-bearing helper is reachable from either branch, so both
    /// segments are linear and the apex is a corner rather than a curve.
    ///
    /// So the authored order is `[end, midpoint, start]`, not `[head, mid,
    /// tail]`. A file that writes a single `Width` repeats it, and the two
    /// shipped shapes collapse to the old two-point ramp — which is why the
    /// 356 corpus fractals that author only one width read correctly either
    /// way. The 623 that author three do not: 76 carry their widest cross
    /// section in the middle, and dropping the midpoint loses 17.5% of the
    /// strip's area on average across the whole corpus.
    ///
    /// Note the two ends are *not* interchangeable — `t = 0` is `Width[2]`, the
    /// third line — so a strip whose end widths differ and whose segments are
    /// not symmetric draws differently from the naive head/tail reading.
    pub fn width_at(&self, t: f32) -> f32 {
        profile_at(&self.widths, t)
    }

    /// The authored alpha at fraction `t` of the strip, on the same three-point
    /// profile and in the same `[start, end, midpoint]` file order as
    /// [`Fractal::width_at`]: the loader fills `+0x2cc`/`+0x2d0`/`+0x2d4` from
    /// the three `Alpha` lines in file order.
    pub fn alpha_at(&self, t: f32) -> f32 {
        profile_at(&self.alphas, t)
    }
}

/// Sample a three-point authored profile.
///
/// Order is the file's own: `[start, end, midpoint]`. `A3DGFXLightning::Load`
/// reads the lines in exactly that order (`A3DGFXLightning.cpp:467-500`):
///
///     Width -> m_fWidthStart   // line 1
///     Width -> m_fWidthEnd     // line 2
///     ... (version >= 7) Alpha -> m_fAlphaStart, Alpha -> m_fAlphaEnd
///     ... (version >= 35) Width -> m_fWidthMid, Alpha -> m_fAlphaMid   // line 3
///
/// and `FillVertexBuffer` ramps `Start -> Mid` over the first half of the path
/// and `Mid -> End` over the second (`:888-903`). So the third line is the
/// apex, not the second. One value is constant, two interpolate start to end
/// with no midpoint.
fn profile_at(values: &[f32], t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    match values {
        [] => 0.,
        [only] => *only,
        [start, end] => start + (end - start) * t,
        [start, end, mid, ..] => {
            if t <= 0.5 {
                start + (mid - start) * (t / 0.5)
            } else {
                mid + (end - mid) * ((t - 0.5) / 0.5)
            }
        }
    }
}

/// Branching lightning (element 151).
#[derive(Clone, Debug, PartialEq)]
pub struct Lightning {
    pub deviation: f32,
    pub step_min: f32,
    pub step_max: f32,
    pub width_start: f32,
    pub width_end: f32,
    pub alpha_start: f32,
    pub alpha_end: f32,
    pub stroke_amplitude: f32,
    pub max_steps: u32,
    pub max_branches: u32,
    pub interval: u32,
    pub bolts_per_interval: u32,
    pub circles: u32,
}

impl Default for Lightning {
    fn default() -> Self {
        Self {
            deviation: 0.,
            step_min: 0.,
            step_max: 0.,
            width_start: 0.,
            width_end: 0.,
            alpha_start: 1.,
            alpha_end: 0.,
            stroke_amplitude: 0.,
            max_steps: 1,
            max_branches: 0,
            interval: 100,
            bolts_per_interval: 1,
            circles: 1,
        }
    }
}

/// Fractal trail (element 152): the fractal payload plus the tail keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Trail {
    /// The strip geometry shared with element 150. `Segs` doubles as the vertex
    /// count the ring is filled to, element+0x2b8.
    pub fractal: Fractal,
    /// Lay vertices down over time and let them expire, instead of rebuilding the
    /// whole strip every frame. Byte at element+0x304, read from version 67 and
    /// true in the constructor at `0x7137ea`. 138 of the 156 corpus trails set
    /// it, 18 clear it.
    pub is_append: bool,
    /// Read from version 67 into element+0x308 and clamped to 0, 1 or 2 by
    /// `0x713ae3` — then never read again. Nothing in the effect region touches
    /// +0x308 outside the loader and the clone, and all 156 corpus trails author
    /// 0, so this is inert in the client itself rather than merely unimplemented
    /// here.
    pub render_side: u32,
    /// Drop a vertex once its age reaches its life. Byte at element+0x305, read
    /// from version 68, true by default; `0x713fbe` is the pass that acts on it.
    pub is_tail_disappear: bool,
    /// Milliseconds a trail vertex lives: element+0x30c, read from version 68 as
    /// `vertslife: %d` and defaulting to 1000 (`movl $0x3e8,0x30c`). Every ring
    /// record is born carrying it; the corpus ranges from 100 to 2000.
    pub vertices_life: f32,
    /// Read from version 69 into the byte at element+0x306, so natively this is a
    /// flag reduced with `setne` rather than the fraction its name suggests. 155
    /// of the 156 corpus trails author 0 and one authors 1, and for those two
    /// values the flag reading and `1.0 - value` agree, which is what the player
    /// applies.
    pub tail_fade_out: f32,
}

/// Embedded model (element 160).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRef {
    /// Path relative to the model archive root, e.g. `地图\水晶.SMD`.
    pub path: String,
    /// `ModelActName`, the action `PlayActionByNameDC` starts (version 19+).
    /// The viewer poses the bind pose, so the action is not played yet.
    pub action: String,
    /// `Loops`, passed straight to `PlayActionByNameDC`: `-1` repeats forever,
    /// `1` plays once and holds. 174 shipped models author `-1`, 72 author `1`
    /// and 201 predate the version-31 key, whose constructor value is 1.
    pub loops: i32,
    /// `AlphaCmp`, fed to `A3DSkinModel::EnableAlphaComp` (version 29+). Only 9
    /// of the 447 shipped models set it.
    pub alpha_compare: bool,
    /// `WriteZ`, the `ZWRITEENABLE` state `PrepareRenderSkinModel` installs
    /// (version 49+). 115 shipped models author `1`; the constructor value and
    /// therefore the meaning of an absent key is false.
    pub write_z: bool,
    /// `Use3DCam` (version 77+). Read and saved by the client but dead in it:
    /// `A3DGFXModel::IsForceUse3DCamera` returns `false` unconditionally, with
    /// `m_bIsUse3DCamera` commented out of both that return and its
    /// `GetProperty`. Nothing may act on this value.
    pub use_3d_camera: bool,
}

/// Sound (element 170).
#[derive(Clone, Debug, PartialEq)]
pub struct SoundRef {
    /// The first of `paths`, what single-consumers key on.
    pub path: String,
    /// Every `Path` row of the record. The client keeps a random file
    /// container here: `GFXSOUNDIMP::Load` (A3DGFXSoundImp.cpp) reads
    /// `PathNum: %d` and then that many `Path: %s` rows at root version 88
    /// or later — one `Path` row before 88 — into a `RandStringContainer(5)`,
    /// and `ChangeSoundFile` draws one at random per sound change. The editor
    /// calls this list DropFileNames; the rows are the sound's alternate
    /// takes, not a single path with junk after it.
    pub paths: Vec<String>,
    pub version: u32,
    pub force_2d: bool,
    pub is_loop: bool,
    /// 0..100.
    pub volume: u32,
    pub min_distance: f32,
    pub max_distance: f32,
}

/// Wave trail (element 180). Two `Pos`, two `Amp` and two `EnablePos` values.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    pub positions: Vec<Vec3>,
    pub segment_life: f32,
    pub amplitudes: Vec<f32>,
    pub bind: i32,
    pub enable_material: bool,
    pub enable_positions: Vec<bool>,
}

impl Default for Wave {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            segment_life: 0.,
            amplitudes: Vec::new(),
            bind: 0,
            enable_material: false,
            enable_positions: Vec::new(),
        }
    }
}

/// Curve (element 190).
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub coefficient: f32,
    pub height: f32,
}

/// Nested root (element 200..204).
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    /// Path relative to the `gfx` archive root.
    pub path: String,
    /// 1 tints the child with this element's keypoint colour.
    pub out_color: bool,
    pub loop_flag: bool,
    pub play_speed: f32,
}

/// Key the tokenizer gives a bare vertex row. No `.gfx` key can collide with it:
/// every real key is an identifier and this one starts with a space.
const GRID_VERTEX: &str = " vertex";

/// Cell grid (elements 210, 211). Colours are hex strings without a prefix, so
/// a six digit colour really does carry alpha 0, exactly as the client reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub columns: u32,
    pub rows: u32,
    pub colors: Vec<u32>,
    /// One authored corner per cell, row-major and already in element-local
    /// space. Empty only for a grid that writes none, which no corpus file does.
    pub vertices: Vec<Vec3>,
    pub cell_size: f32,
    pub z_offset: f32,
}

/// Positional reader over the tokenized container.
struct Reader {
    fields: Vec<Field>,
    position: usize,
}

/// Keys that terminate an element specification block.
const SPEC_TERMINATORS: [&str; 5] = [
    "StartTime",
    "KEYPOINTCOUNT",
    "InterpolateMode",
    "AffectorCount",
    "GFXELEMENTID",
];

impl Reader {
    fn new(fields: Vec<Field>) -> Self {
        Self {
            fields,
            position: 0,
        }
    }

    fn line(&self) -> usize {
        self.position + 1
    }

    fn peek(&self) -> Option<&str> {
        self.fields.get(self.position).map(|(key, _)| key.as_str())
    }

    fn at_end(&self) -> bool {
        self.position >= self.fields.len()
    }

    fn raw(&mut self) -> Result<(String, String), String> {
        let (key, value) = self
            .fields
            .get(self.position)
            .cloned()
            .ok_or_else(|| "unexpected end of the GFX container".to_string())?;
        self.position += 1;
        Ok((key, value))
    }

    /// Consume the next field, which must carry `key`.
    fn take(&mut self, key: &str) -> Result<String, String> {
        let found = self.peek().unwrap_or("<end of file>").to_string();
        if found != key {
            return Err(format!(
                "GFX line {}: expected {key}, found {found}",
                self.line()
            ));
        }
        let (_, value) = self.raw()?;
        Ok(value)
    }

    /// Consume the next field only when it carries `key`.
    fn take_if(&mut self, key: &str) -> Option<String> {
        if self.peek() == Some(key) {
            let (_, value) = self.raw().ok()?;
            Some(value)
        } else {
            None
        }
    }

    fn number(&mut self, key: &str) -> Result<f32, String> {
        let value = self.take(key)?;
        float(key, &value)
    }

    fn optional_number(&mut self, key: &str, default: f32) -> Result<f32, String> {
        match self.take_if(key) {
            Some(value) => float(key, &value),
            None => Ok(default),
        }
    }

    fn integer(&mut self, key: &str) -> Result<i64, String> {
        let value = self.take(key)?;
        integer(key, &value)
    }

    fn optional_integer(&mut self, key: &str, default: i64) -> Result<i64, String> {
        match self.take_if(key) {
            Some(value) => integer(key, &value),
            None => Ok(default),
        }
    }

    fn flag(&mut self, key: &str, default: bool) -> Result<bool, String> {
        match self.take_if(key) {
            Some(value) => boolean(key, &value),
            None => Ok(default),
        }
    }

    fn vector(&mut self, key: &str) -> Result<Vec3, String> {
        let value = self.take(key)?;
        let parts = components(key, &value)?;
        if parts.len() != 3 {
            return Err(format!("GFX {key} needs three components"));
        }
        Ok(Vec3::new(parts[0], parts[1], parts[2]))
    }
}

fn float(key: &str, value: &str) -> Result<f32, String> {
    value
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("GFX {key} is not a number: {value:?}"))
}

fn integer(key: &str, value: &str) -> Result<i64, String> {
    let value = value.trim();
    // The container writes counts as integers and clocks as floats; both must
    // survive a shared reader without losing the -1 "forever" sentinel.
    if let Ok(parsed) = value.parse::<i64>() {
        return Ok(parsed);
    }
    let parsed = float(key, value)?;
    if !parsed.is_finite() {
        return Err(format!("GFX {key} is not finite: {value}"));
    }
    Ok(parsed as i64)
}

fn boolean(key: &str, value: &str) -> Result<bool, String> {
    match integer(key, value)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(format!("GFX {key} is not a flag: {other}")),
    }
}

/// Split a comma separated value list into floats.
fn components(key: &str, value: &str) -> Result<Vec<f32>, String> {
    value
        .split(',')
        .map(|part| float(key, part))
        .collect::<Result<Vec<_>, _>>()
}

/// Split a comma separated value list into signed integers.
fn signed_components(key: &str, value: &str) -> Result<Vec<i64>, String> {
    value
        .split(',')
        .map(|part| integer(key, part))
        .collect::<Result<Vec<_>, _>>()
}

/// Packed colour: the file stores a signed 32-bit 0xAARRGGBB.
fn color(key: &str, value: &str) -> Result<u32, String> {
    let raw = integer(key, value)?;
    #[allow(clippy::cast_sign_loss)]
    Ok(raw as u32)
}

/// Hex colour cell of the grid elements: `ffffff` or `69ffffff`, no prefix.
fn hex_color(key: &str, value: &str) -> Result<u32, String> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or(value.strip_prefix("0X"))
        .unwrap_or(value);
    u32::from_str_radix(value, 16).map_err(|_| format!("GFX {key} is not a hex colour: {value:?}"))
}

fn quaternion(key: &str, value: &str) -> Result<Quat, String> {
    let parts = components(key, value)?;
    if parts.len() != 4 {
        return Err(format!("GFX {key} needs four components"));
    }
    let quat = Quat::from_xyzw(parts[0], parts[1], parts[2], parts[3]);
    if !quat.is_finite() {
        return Err(format!("GFX {key} is not finite"));
    }
    // The editor writes normalized directions; a degenerate one would poison
    // every billboard built from it.
    Ok(if quat.length_squared() < 1e-12 {
        Quat::IDENTITY
    } else {
        quat.normalize()
    })
}

/// Parse a whole container.
pub fn parse(source: &str) -> Result<Root, String> {
    let fields = tokenize(source);
    if fields.is_empty() {
        return Err("GFX container is empty".into());
    }
    if fields[0].0 != "MOXTVersion" {
        return Err(format!(
            "GFX container starts with {:?}, not MOXTVersion",
            fields[0].0
        ));
    }
    let mut reader = Reader::new(fields);
    let version = reader.integer("MOXTVersion")?;
    if version < 0 {
        return Err(format!("negative GFX version {version}"));
    }
    #[allow(clippy::cast_sign_loss)]
    let version = version as u32;
    if version > MAX_VERSION {
        return Err(format!(
            "GFX version {version} is newer than the client's {MAX_VERSION}"
        ));
    }
    if version < 24 {
        // Version 4 records in the corpus are skill missile paths (`MoveMode`,
        // `TargetMode`, `FlyTime`), not effect roots. The client's own gate is
        // `cmp 0x18`, so anything below it is not this container.
        return Err(format!(
            "GFX version {version} predates the effect container (client gate 24)"
        ));
    }
    let root = Root {
        version,
        default_scale: reader.number("DedaultScale")?,
        play_speed: reader.number("PlaySpeed")?,
        default_alpha: reader.number("DefaultAlpha")?,
        raytrace: boolean("Raytrace", &reader.take("Raytrace")?)?,
        face_to_viewer: reader.flag("FaceToViewer", false)?,
        fade_by_dist: boolean("FadeByDist", &reader.take("FadeByDist")?)?,
        fade_start: reader.number("FadeStart")?,
        fade_end: reader.number("FadeEnd")?,
        aabb_center: reader.vector("Vec")?,
        aabb_half: reader.vector("Vec")?,
        use_aabb: reader.flag("UseAABB", false)?,
        shake_cam: clamp_u32(reader.integer("ShakeCam")?),
        controls: Vec::new(),
        no_change_dir: false,
        render_2d: false,
        back_layer_2d: false,
        phys_exist: false,
        elements: Vec::new(),
    };
    let mut root = root;
    while reader.peek() == Some("CtrlType") {
        root.controls.push(control(&mut reader)?);
    }
    // Version 50 adds the 2D flags, version 64 adds the physics marker. Both
    // are read in file order and only when present.
    if reader.peek() == Some("NoChangeDir") {
        root.no_change_dir = reader.flag("NoChangeDir", false)?;
        root.render_2d = reader.flag("2DRender", false)?;
        root.back_layer_2d = reader.flag("2DBackLayer", false)?;
    }
    root.phys_exist = reader.flag("PhysExist", false)?;
    // With `FadeByDist: 0` the pair is dead data, so only an enabled fade is
    // validated. No retail root inverts it either way.
    if root.fade_by_dist && root.fade_end <= root.fade_start {
        return Err(format!(
            "GFX fade range {}..{} is inverted",
            root.fade_start, root.fade_end
        ));
    }
    if !root.play_speed.is_finite() || root.play_speed < 0. {
        return Err(format!("invalid GFX PlaySpeed {}", root.play_speed));
    }
    if !root.default_scale.is_finite() || root.default_scale <= 0. {
        return Err(format!("invalid GFX DedaultScale {}", root.default_scale));
    }
    // `DefaultAlpha` is a multiplier, not a normalized opacity: two retail roots
    // (`gfx/人物/技能/刺客/斩杀_刀光主体.gfx` and its child) author 1.2 to push
    // their additive quads past the authored keypoint alpha. Only non-finite and
    // negative values would corrupt the per-channel fold.
    if !root.default_alpha.is_finite() || root.default_alpha < 0. {
        return Err(format!("invalid GFX DefaultAlpha {}", root.default_alpha));
    }
    let count = reader.integer("GFXELEMENTCOUNT")?;
    if count < 0 || count > 4096 {
        return Err(format!("GFX element count {count} is out of range"));
    }
    for index in 0..count {
        let id = clamp_u32(reader.integer("GFXELEMENTID")?);
        let element = element(&mut reader, id, version)
            .map_err(|error| format!("GFX element {index} (id {id}): {error}"))?;
        root.elements.push(element);
    }
    if !reader.at_end() {
        return Err(format!(
            "GFX line {}: trailing {:?} after the last element",
            reader.line(),
            reader.peek().unwrap_or_default()
        ));
    }
    Ok(root)
}

fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn element(reader: &mut Reader, id: u32, version: u32) -> Result<Element, String> {
    // Everything up to the element clock, the affector list, the element-125
    // generation sequence or the next element belongs to this record. The
    // client writes its shared keys before the class payload, but 493 distinct
    // orders exist for element 100 alone because each optional key is version
    // gated, so the block is split by key rather than read as two runs.
    let mut block: Vec<Field> = Vec::new();
    while let Some(key) = reader.peek() {
        if SPEC_TERMINATORS.contains(&key) || key == "CtrlType" {
            break;
        }
        block.push(reader.raw()?);
    }
    let (shared, spec_fields): (Vec<Field>, Vec<Field>) = block
        .into_iter()
        .partition(|(key, _)| SHARED_KEYS.contains(&key.as_str()));
    let common = common(&shared, id, version)?;
    let payload = spec(id, &spec_fields, version)?;
    let mut affectors = Vec::new();
    let mut generation_sequence = None;
    if reader.peek() == Some("InterpolateMode") {
        // Element 125 writes one keypoint-shaped generation sequence, carrying
        // the path controller its particles are spawned along, before the
        // affector list. No other class reaches this branch.
        generation_sequence = Some(Box::new(keypoint(reader)?));
    }
    if let Some(value) = reader.take_if("AffectorCount") {
        let count = integer("AffectorCount", &value)?;
        if count < 0 || count > 64 {
            return Err(format!("affector count {count} is out of range"));
        }
        for _ in 0..count {
            affectors.push(control(reader)?);
        }
    }
    let start_time = reader.optional_number("StartTime", 0.)?;
    let mut keypoints = Vec::new();
    if let Some(value) = reader.take_if("KEYPOINTCOUNT") {
        let count = integer("KEYPOINTCOUNT", &value)?;
        if count < 0 || count > 256 {
            return Err(format!("keypoint count {count} is out of range"));
        }
        for _ in 0..count {
            keypoints.push(keypoint(reader)?);
        }
    }
    if !start_time.is_finite() || start_time < 0. {
        return Err(format!("invalid element StartTime {start_time}"));
    }
    Ok(Element {
        id,
        common,
        spec: payload,
        start_time,
        keypoints,
        affectors,
        generation_sequence,
    })
}

/// The shared keys, read by name because every one of them is version gated.
fn common(fields: &[Field], id: u32, version: u32) -> Result<Common, String> {
    if fields.is_empty() {
        return Err(format!("element {id} has no shared keys"));
    }
    let lookup = Lookup::new(&fields);
    let mut tex_speed = [0f32; 2];
    for (index, value) in lookup.all("TexSpeed").into_iter().enumerate().take(2) {
        tex_speed[index] = float("TexSpeed", value)?;
    }
    let mut no_scale = [false; 2];
    for (index, value) in lookup.all("NoScale").into_iter().enumerate().take(2) {
        no_scale[index] = boolean("NoScale", value)?;
    }
    let mut origin = [0.5f32; 2];
    for (index, value) in lookup.all("OrgPt").into_iter().enumerate().take(2) {
        origin[index] = float("OrgPt", value)?;
    }
    let common = Common {
        name: lookup.one("Name").unwrap_or_default().to_string(),
        source_blend: clamp_u32(lookup.integer("SrcBlend", 5)?),
        destination_blend: clamp_u32(lookup.integer("DestBlend", 2)?),
        repeat_count: lookup.integer("RepeatCount", 1)? as i32,
        repeat_delay: lookup.number("RepeatDelay", 0.)?,
        texture: lookup.one("TexFile").unwrap_or_default().to_string(),
        bind_element: lookup.one("BindEle").unwrap_or_default().to_string(),
        depth_test: lookup.flag("ZEnable", false)?,
        match_ground: lookup.flag("MatchGrnd", false)?,
        ground_height: if version >= 57 {
            lookup.number("GroundHeight", 0.)?
        } else {
            0.
        },
        tex_row: clamp_u32(lookup.integer("TexRow", 1)?),
        tex_col: clamp_u32(lookup.integer("TexCol", 1)?),
        tex_interval: clamp_u32(lookup.integer("TexInterval", 0)?),
        priority: lookup.integer("Priority", 0)? as i32,
        is_dummy: lookup.flag("IsDummy", false)?,
        dummy_element: lookup.one("DummyEle").unwrap_or_default().to_string(),
        warp: lookup.flag("Warp", false)?,
        tile_mode: clamp_u32(lookup.integer("TileMode", 0)?),
        tex_speed,
        reverse_u: lookup.flag("UReverse", false)?,
        reverse_v: lookup.flag("VReverse", false)?,
        exchange_uv: lookup.flag("UVExchg", false)?,
        render_layer: lookup.integer("RenderLayer", 0)? as i32,
        no_down_sample: lookup.flag("NoDownSample", false)?,
        reset_loop_end: lookup.flag("ResetLoopEnd", false)?,
        tex_anim_max_time: lookup.number("TexAnimMaxTime", 0.)?,
        no_scale,
        origin,
    };
    if common.name.len() > 64 {
        return Err("element name is longer than the client's 64 bytes".into());
    }
    for value in common.tex_speed {
        if !value.is_finite() {
            return Err("non-finite TexSpeed".into());
        }
    }
    for value in origin {
        // `OrgPt` is a pivot, not a normalized coordinate: 17 weapon-effect
        // roots in the retail corpus author `OrgPt: -0.500000`, which moves the
        // pivot half a quad outside the sprite. Only non-finite values are
        // damage, because they would poison the corner arithmetic.
        if !value.is_finite() {
            return Err(format!("OrgPt {value} is not finite"));
        }
    }
    if common.tex_row == 0 || common.tex_col == 0 {
        return Err("TexRow and TexCol must be at least one".into());
    }
    Ok(common)
}

/// Keys the client writes for every element class, in file order. `StartTime`
/// is deliberately absent: it belongs to the element clock, not to this block.
const SHARED_KEYS: &[&str] = &[
    "Name",
    "SrcBlend",
    "DestBlend",
    "RepeatCount",
    "RepeatDelay",
    "TexFile",
    "BindEle",
    "ZEnable",
    "MatchGrnd",
    "GroundHeight",
    "TexRow",
    "TexCol",
    "TexInterval",
    "Priority",
    "IsDummy",
    "DummyEle",
    "Warp",
    "TileMode",
    "TexSpeed",
    "UReverse",
    "VReverse",
    "UVExchg",
    "RenderLayer",
    "NoDownSample",
    "ResetLoopEnd",
    "TexAnimMaxTime",
];

/// Read-by-name access over a specification block, preserving repeated keys in
/// the order the client wrote them.
struct Lookup<'a> {
    fields: &'a [Field],
}

impl<'a> Lookup<'a> {
    fn new(fields: &'a [Field]) -> Self {
        Self { fields }
    }

    fn all(&self, key: &str) -> Vec<&'a str> {
        self.fields
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn one(&self, key: &str) -> Option<&'a str> {
        self.all(key).first().copied()
    }

    fn required(&self, key: &str) -> Result<&'a str, String> {
        self.one(key)
            .ok_or_else(|| format!("missing required key {key}"))
    }

    fn number(&self, key: &str, default: f32) -> Result<f32, String> {
        match self.one(key) {
            Some(value) => float(key, value),
            None => Ok(default),
        }
    }

    fn integer(&self, key: &str, default: i64) -> Result<i64, String> {
        match self.one(key) {
            Some(value) => integer(key, value),
            None => Ok(default),
        }
    }

    fn flag(&self, key: &str, default: bool) -> Result<bool, String> {
        match self.one(key) {
            Some(value) => boolean(key, value),
            None => Ok(default),
        }
    }

    /// A flag the client reads as "not zero" rather than "equal to one".
    /// `IsUseHSVInterp` is parsed that way: `0x724416` tests the scanned value
    /// against itself and `0x72441f` stores the `setne` result in emitter+0xb0,
    /// so any nonzero value selects the HSV sampler. Rejecting 2 here would be a
    /// stricter reader than the binary has.
    fn nonzero_flag(&self, key: &str, default: bool) -> Result<bool, String> {
        match self.one(key) {
            Some(value) => Ok(integer(key, value)? != 0),
            None => Ok(default),
        }
    }

    fn vector(&self, key: &str) -> Result<Vec3, String> {
        let value = self.required(key)?;
        let parts = components(key, value)?;
        if parts.len() != 3 {
            return Err(format!("{key} needs three components"));
        }
        Ok(Vec3::new(parts[0], parts[1], parts[2]))
    }

    fn optional_vector(&self, key: &str) -> Result<Option<Vec3>, String> {
        match self.one(key) {
            Some(value) => {
                let parts = components(key, value)?;
                if parts.len() != 3 {
                    return Err(format!("{key} needs three components"));
                }
                Ok(Some(Vec3::new(parts[0], parts[1], parts[2])))
            }
            None => Ok(None),
        }
    }
}

/// Build the class payload from the specification block.
fn spec(id: u32, fields: &[Field], version: u32) -> Result<Spec, String> {
    let lookup = Lookup::new(fields);
    for key in lookup.fields.iter().map(|(key, _)| key.as_str()) {
        if key != GRID_VERTEX && !KNOWN_KEYS.contains(&key) {
            return Err(format!("element {id} carries the unknown key {key:?}"));
        }
    }
    Ok(match id {
        100..=104 => Spec::Sprite(sprite(&lookup, id, version)?),
        110 => Spec::Ribbon(ribbon(&lookup)?),
        120..=125 => Spec::Particle(particle(&lookup, id)?),
        130 => Spec::Light(light(&lookup)?),
        140 => Spec::Cylinder(cylinder(&lookup)?),
        150 => Spec::Fractal(fractal(&lookup)?),
        151 => Spec::Lightning(Lightning {
            deviation: lookup.number("Deviation", 0.)?,
            step_min: lookup.number("StepMin", 0.)?,
            step_max: lookup.number("StepMax", 0.)?,
            width_start: lookup.number("WidthStart", 0.)?,
            width_end: lookup.number("WidthEnd", 0.)?,
            alpha_start: lookup.number("AlphaStart", 1.)?,
            alpha_end: lookup.number("AlphaEnd", 0.)?,
            stroke_amplitude: lookup.number("StrokeAmp", 0.)?,
            max_steps: clamp_u32(lookup.integer("MaxSteps", 1)?),
            max_branches: clamp_u32(lookup.integer("MaxBranches", 0)?),
            interval: clamp_u32(lookup.integer("Interval", 100)?),
            bolts_per_interval: clamp_u32(lookup.integer("PerBolts", 1)?),
            circles: clamp_u32(lookup.integer("Circles", 1)?),
        }),
        152 => Spec::Trail(Trail {
            fractal: fractal(&lookup)?,
            // The trail loader gates its keys in three steps: `0x713a63` needs
            // version 67 for isappend and renderside, `0x713aff` needs 68 for
            // istaildisappear and vertslife, `0x713b69` needs 69 for tailfadeout.
            // Below a gate the constructor's default at `0x7137ea` stands, and
            // those are not zeros: `movb $0x1,0x304` and `movb $0x1,0x305` make
            // both flags true and `movl $0x3e8,0x30c` makes vertslife 1000. All
            // 156 corpus trails author every one of these keys, so the corrected
            // defaults change no retail file — only a hand-written one.
            is_append: if version >= 67 {
                lookup.flag("isappend", true)?
            } else {
                true
            },
            // `0x713ae3` writes the value back as 0 unless it is 0, 1 or 2.
            render_side: if version >= 67 {
                match clamp_u32(lookup.integer("renderside", 0)?) {
                    side @ (0 | 1 | 2) => side,
                    _ => 0,
                }
            } else {
                0
            },
            is_tail_disappear: if version >= 68 {
                lookup.flag("istaildisappear", true)?
            } else {
                true
            },
            vertices_life: if version >= 68 {
                lookup.number("vertslife", 1000.)?
            } else {
                1000.
            },
            tail_fade_out: if version >= 69 {
                lookup.number("tailfadeout", 0.)?
            } else {
                0.
            },
        }),
        160 => Spec::Model(ModelRef {
            path: lookup.one("ModelPath").unwrap_or_default().to_string(),
            action: lookup.one("ModelActName").unwrap_or_default().to_string(),
            loops: lookup.integer("Loops", 1)? as i32,
            alpha_compare: lookup.flag("AlphaCmp", false)?,
            // `A3DGFXModel`'s constructor starts at `m_bZWriteEnable(false)` and
            // the key only arrived at version 49: of the 447 models in the
            // shipped `gfx.pck`, 115 author `WriteZ: 1`, 131 author `0` and 201
            // predate the key.
            write_z: lookup.flag("WriteZ", false)?,
            use_3d_camera: lookup.flag("Use3DCam", false)?,
        }),
        170 => Spec::Sound(SoundRef {
            path: lookup.one("Path").unwrap_or_default().to_string(),
            paths: {
                // `PathNum` is the count the client reads at version 88 or
                // later and then consumes exactly that many `Path` rows
                // (A3DGFXSoundImp.cpp, `GFXSOUNDIMP::Load`); a record that
                // authors the count takes exactly its rows, and an older
                // single-path record takes the one row it has.
                let mut paths: Vec<String> =
                    lookup.all("Path").into_iter().map(str::to_string).collect();
                if let Ok(count) = lookup.integer("PathNum", 0) {
                    if count > 0 {
                        paths.truncate(count as usize);
                    }
                }
                paths
            },
            version: clamp_u32(lookup.integer("SoundVer", 1)?),
            force_2d: lookup.flag("Force2D", false)?,
            is_loop: lookup.flag("IsLoop", false)?,
            volume: clamp_u32(lookup.integer("Volume", 100)?),
            min_distance: lookup.number("MinDist", 1.)?,
            max_distance: lookup.number("MaxDist", 1000.)?,
        }),
        180 => Spec::Wave(Wave {
            positions: {
                let mut points = Vec::new();
                for value in lookup.all("Pos") {
                    let parts = components("Pos", value)?;
                    if parts.len() != 3 {
                        return Err("wave Pos needs three components".into());
                    }
                    points.push(Vec3::new(parts[0], parts[1], parts[2]));
                }
                points
            },
            segment_life: lookup.number("SegLife", 0.)?,
            amplitudes: {
                let mut values = Vec::new();
                for value in lookup.all("Amp") {
                    values.push(float("Amp", value)?);
                }
                values
            },
            bind: lookup.integer("Bind", 0)? as i32,
            enable_material: lookup.flag("EnableMat", false)?,
            enable_positions: {
                let mut values = Vec::new();
                for value in lookup.all("EnablePos") {
                    values.push(boolean("EnablePos", value)?);
                }
                values
            },
        }),
        190 => Spec::Curve(Curve {
            coefficient: lookup.number("Coeff", 1.)?,
            height: lookup.number("height", 0.)?,
        }),
        200..=204 => Spec::Child(Child {
            path: lookup.required("GfxPath")?.to_string(),
            out_color: lookup.flag("OutColor", false)?,
            loop_flag: lookup.flag("LoopFlag", true)?,
            play_speed: lookup.number("PlaySpeed", 1.)?,
        }),
        210 | 211 => Spec::Grid(grid(&lookup)?),
        other => Spec::Unknown { id: other },
    })
}

fn sprite(lookup: &Lookup, id: u32, version: u32) -> Result<Sprite, String> {
    // Every retail element 100, 101 and 102 writes exactly one `Width` and one
    // `Height`; a missing one is damage, not a default. Elements 103 and 104
    // exist in the client's class switch but not in the corpus, so they keep
    // the neutral default instead of failing on absent evidence.
    if id <= 102 {
        for key in ["Width", "Height"] {
            if lookup.one(key).is_none() {
                return Err(format!("element {id} is missing its {key} key"));
            }
        }
    }
    // Negative dimensions are authored, not damage: 216 `Width`/`Height` lines
    // in the retail corpus are negative (for example the mirrored halves of
    // `gfx/人物/技能/刺客/丢飞刀_手上效果.gfx`). The sprite geometry multiplies
    // them straight into the corner offsets, so a negative width mirrors the
    // quad about its origin, which is what those roots draw.
    let width = lookup.number("Width", 0.)?;
    let height = lookup.number("Height", 0.)?;
    // Version gates from the sprite loader `0x71dc07`, which walks the key
    // table at `0x97f1d4`..`0x97f204` with a `cmp $imm,%ebx; jb` before each
    // group: `Width`/`Height` are ungated, `RotFromView` needs 9 (`0x71dd61`),
    // `GrndNormOnly` 26 (`0x71deaa`), both `NoScale` and both `OrgPt` values 36
    // (`0x71def0`), then `ZOffset` 42 (`cmp $0x2a`), `MatchSurface` 54
    // (`$0x36`), `MaxExtent` 55 (`$0x37`) and `YawEffect` 61 (`$0x3d`). An older
    // root keeps the constructed defaults even if the key is present.
    // `TexRow`/`TexCol`/`TexInterval` share the version-9 gate but are read
    // once here, from the shared block, which is where the client's second read
    // of them lands too.
    let rotate_from_view = version >= 9 && lookup.flag("RotFromView", false)?;
    let ground_normal_only = version >= 26 && lookup.flag("GrndNormOnly", false)?;
    let no_scale = if version >= 36 {
        [
            lookup.all("NoScale").first().is_some_and(|v| *v == "1"),
            lookup.all("NoScale").get(1).is_some_and(|v| *v == "1"),
        ]
    } else {
        [false; 2]
    };
    let origin = if version >= 36 {
        [
            lookup
                .all("OrgPt")
                .first()
                .map(|v| float("OrgPt", v))
                .transpose()?
                .unwrap_or(0.5),
            lookup
                .all("OrgPt")
                .get(1)
                .map(|v| float("OrgPt", v))
                .transpose()?
                .unwrap_or(0.5),
        ]
    } else {
        [0.5; 2]
    };
    let z_offset = if version >= 42 {
        lookup.number("ZOffset", 0.)?
    } else {
        0.
    };
    let match_surface = version >= 54 && lookup.flag("MatchSurface", false)?;
    let yaw_effect = version >= 61 && lookup.flag("YawEffect", false)?;
    // The loader's tail runs after every gate: a zero `MaxExtent` becomes
    // `1.5 * max(Width, Height)`, taking the width branch when `Width > Height`
    // (`0x71e121`) and the height branch otherwise (`0x71e141`).
    let authored_extent = if version >= 55 {
        lookup.number("MaxExtent", 0.)?
    } else {
        0.
    };
    let max_extent = if authored_extent == 0. {
        1.5 * width.max(height)
    } else {
        authored_extent
    };
    Ok(Sprite {
        width,
        height,
        rotate_from_view,
        ground_normal_only,
        no_scale,
        origin,
        z_offset,
        match_surface,
        max_extent,
        yaw_effect,
    })
}

fn ribbon(lookup: &Lookup) -> Result<Ribbon, String> {
    Ok(Ribbon {
        origin_1: lookup.vector("OrgPos1")?,
        origin_2: lookup.vector("OrgPos2")?,
        enable_material: lookup.flag("EnableMat", false)?,
        enable_origin_1: lookup.flag("EnableOrgPos1", false)?,
        enable_origin_2: lookup.flag("EnableOrgPos2", false)?,
        segment_life: lookup.number("SegLife", 0.)?,
        bind: lookup.integer("Bind", 0)? as i32,
    })
}

fn particle(lookup: &Lookup, id: u32) -> Result<Particle, String> {
    let mut particle = Particle {
        quota: lookup.integer("Quota", -1)? as i32,
        width: lookup.number("ParticleWidth", 0.)?,
        height: lookup.number("ParticleHeight", 0.)?,
        is_3d: lookup.flag("3DParticle", false)?,
        facing: clamp_u32(lookup.integer("Facing", 0)?),
        scale_no_offset: lookup.flag("ScaleNoOff", false)?,
        no_scale: [
            lookup.all("NoScale").first().is_some_and(|v| *v == "1"),
            lookup.all("NoScale").get(1).is_some_and(|v| *v == "1"),
        ],
        origin: [
            lookup
                .all("OrgPt")
                .first()
                .map(|v| float("OrgPt", v))
                .transpose()?
                .unwrap_or(0.5),
            lookup
                .all("OrgPt")
                .get(1)
                .map(|v| float("OrgPt", v))
                .transpose()?
                .unwrap_or(0.5),
        ],
        use_particle_uv: lookup.flag("IsUseParUV", false)?,
        start_on_ground: lookup.flag("IsStartOnGrnd", false)?,
        emission_rate: lookup.number("EmissionRate", 0.)?,
        angle: lookup.number("Angle", 0.)?,
        speed: lookup.number("Speed", 0.)?,
        particle_acc: lookup.number("ParAcc", 0.)?,
        acc_dir: lookup
            .optional_vector("AccDir")?
            .unwrap_or(Vec3::new(0., 0., -1.)),
        acc: lookup.number("Acc", 0.)?,
        ttl: lookup.number("TTL", 0.)?,
        color_min: color("ColorMin", lookup.required("ColorMin")?)?,
        color_max: color("ColorMax", lookup.one("ColorMax").unwrap_or("-1"))?,
        scale_min: lookup.number("ScaleMin", 1.)?,
        scale_max: lookup.number("ScaleMax", 1.)?,
        rot_min: lookup.number("RotMin", 0.)?,
        rot_max: lookup.number("RotMax", 0.)?,
        is_surface: lookup.flag("IsSurface", false)?,
        is_bind: lookup.flag("IsBind", false)?,
        is_drag: lookup.flag("IsDrag", false)?,
        drag_power: lookup.number("DragPow", 1.)?,
        initial_direction: lookup.optional_vector("ParIniDir")?.unwrap_or(Vec3::ZERO),
        use_hsv_interpolation: lookup.nonzero_flag("IsUseHSVInterp", false)?,
        area_size: lookup.optional_vector("AreaSize")?,
        radius: match lookup.one("Radius") {
            Some(value) => Some(float("Radius", value)?),
            None => None,
        },
        is_average_generation: match lookup.one("IsAvgGen") {
            Some(value) => Some(boolean("IsAvgGen", value)?),
            None => None,
        },
        alpha_segments: match lookup.one("AlphaSeg") {
            Some(value) => Some(clamp_u32(integer("AlphaSeg", value)?)),
            None => None,
        },
        beta_segments: match lookup.one("BetaSeg") {
            Some(value) => Some(clamp_u32(integer("BetaSeg", value)?)),
            None => None,
        },
        average_generation_segments: match lookup.one("AvgGenSeg") {
            Some(value) => Some(clamp_u32(integer("AvgGenSeg", value)?)),
            None => None,
        },
        is_average_generation_sequence: match lookup.one("IsAvgGenSeq") {
            Some(value) => Some(boolean("IsAvgGenSeq", value)?),
            None => None,
        },
    };
    // Negative particle dimensions are authored: 530 `ParticleWidth`/
    // `ParticleHeight` lines in the retail corpus are negative, which mirrors the
    // quad about its origin exactly as a negative sprite `Width` does.
    if !particle.ttl.is_finite() || particle.ttl < 0. {
        return Err(format!("element {id} has an invalid TTL {}", particle.ttl));
    }
    if !particle.emission_rate.is_finite() || particle.emission_rate < 0. {
        return Err(format!(
            "element {id} has an invalid EmissionRate {}",
            particle.emission_rate
        ));
    }
    // `ScaleMin`/`ScaleMax` are the two ends of the spawn interval, not an
    // ordered pair: 817 elements in the retail corpus author them the other way
    // round (0.5 down to 0.1). `min + (max - min) * u` samples the same interval
    // either way, which is what the player does.
    if !particle.scale_min.is_finite() || !particle.scale_max.is_finite() {
        return Err(format!(
            "element {id} has a non-finite particle scale range"
        ));
    }
    // A sphere emitter without a radius still emits from the element origin.
    if id == 125 && particle.radius.is_none() {
        particle.radius = Some(0.);
    }
    if (121..=124).contains(&id) && particle.area_size.is_none() {
        particle.area_size = Some(Vec3::ZERO);
    }
    Ok(particle)
}

fn light(lookup: &Lookup) -> Result<Light, String> {
    let mut attenuation = [0f32; 3];
    for (index, value) in lookup.all("Attenuation0").into_iter().enumerate().take(3) {
        attenuation[index] = float("Attenuation0", value)?;
    }
    Ok(Light {
        light_type: clamp_u32(lookup.integer("LightType", 1)?),
        diffuse: color("Diffuse", lookup.one("Diffuse").unwrap_or("-1"))?,
        specular: color("Specular", lookup.one("Specular").unwrap_or("0"))?,
        ambient: color("Ambient", lookup.one("Ambient").unwrap_or("0"))?,
        position: lookup.optional_vector("Position")?.unwrap_or(Vec3::ZERO),
        direction: lookup.optional_vector("Direction")?.unwrap_or(Vec3::ZERO),
        range: lookup.number("Range", 10.)?,
        falloff: lookup.number("FallOff", 0.)?,
        attenuation,
        theta: lookup.number("Theta", 0.)?,
        phi: lookup.number("Phi", 0.)?,
    })
}

fn cylinder(lookup: &Lookup) -> Result<Cylinder, String> {
    // `Sects: 0` is the common case, not damage: 1064 of the corpus's cylinder
    // elements author it, and the retail maximum is 50. Zero sectors simply
    // draws no wall, so the only bound that matters is the allocation ceiling.
    let sections = clamp_u32(lookup.integer("Sects", 8)?);
    if sections > 4096 {
        return Err(format!("cylinder Sects {sections} is out of range"));
    }
    Ok(Cylinder {
        radius: lookup.number("Radius", 0.)?,
        height: lookup.number("Height", 0.)?,
        pitch: lookup.number("Pitch", 0.)?,
        sections,
        no_radius_scale: lookup.flag("NoRadScale", false)?,
        no_height_scale: lookup.flag("NoHeiScale", false)?,
        origin_at_center: lookup.flag("OrgAtCenter", false)?,
    })
}

fn fractal(lookup: &Lookup) -> Result<Fractal, String> {
    let widths = lookup
        .all("Width")
        .into_iter()
        .map(|value| float("Width", value))
        .collect::<Result<Vec<_>, _>>()?;
    let alphas = lookup
        .all("Alpha")
        .into_iter()
        .map(|value| float("Alpha", value))
        .collect::<Result<Vec<_>, _>>()?;
    let amplitudes = lookup
        .all("Amplitude")
        .into_iter()
        .map(|value| float("Amplitude", value))
        .collect::<Result<Vec<_>, _>>()?;
    let wave_lengths = lookup
        .all("WaveLen")
        .into_iter()
        .map(|value| float("WaveLen", value))
        .collect::<Result<Vec<_>, _>>()?;
    // `Segs` runs from 0 (two corpus elements, which draw nothing) to 100; the
    // player clamps the strip to at least two cross-sections when it builds one.
    let segments = clamp_u32(lookup.integer("Segs", 10)?);
    if segments > 4096 {
        return Err(format!("fractal Segs {segments} is out of range"));
    }
    Ok(Fractal {
        buffer_length: clamp_u32(lookup.integer("BufLen", 256)?),
        noise_amplitude: amplitudes.first().copied().unwrap_or(1.),
        noise_wave_length: wave_lengths.first().copied().unwrap_or(1.),
        persistence: lookup.number("Persistence", 1.)?,
        octaves: clamp_u32(lookup.integer("OctaveNum", 5)?),
        start_position: lookup.optional_vector("StartPos")?.unwrap_or(Vec3::ZERO),
        end_position: lookup.optional_vector("EndPos")?.unwrap_or(Vec3::ZERO),
        segments,
        light_number: clamp_u32(lookup.integer("LightNum", 0)?),
        wave_length: wave_lengths.get(1).copied().unwrap_or(1.),
        interval: clamp_u32(lookup.integer("Interval", 100)?),
        widths: if widths.is_empty() { vec![0.1] } else { widths },
        alphas: if alphas.is_empty() { vec![1.] } else { alphas },
        amplitude: amplitudes.get(1).copied().unwrap_or(0.),
        position_1_enable: lookup.flag("Pos1Enable", false)?,
        position_2_enable: lookup.flag("Pos2Enable", false)?,
        use_normal: lookup.flag("UseNormal", false)?,
        normal: lookup.optional_vector("Normal")?.unwrap_or(Vec3::ZERO),
        filter: clamp_u32(lookup.integer("Filter", 1)?),
    })
}

fn grid(lookup: &Lookup) -> Result<Grid, String> {
    let columns = clamp_u32(lookup.integer("wNumber", 0)?);
    let rows = clamp_u32(lookup.integer("hNumber", 0)?);
    if columns == 0 || rows == 0 || columns > 64 || rows > 64 {
        return Err(format!("grid {columns}x{rows} is out of range"));
    }
    let colors = lookup
        .all("dwColor")
        .into_iter()
        .map(|value| hex_color("dwColor", value))
        .collect::<Result<Vec<_>, _>>()?;
    let expected = (columns * rows) as usize;
    if colors.len() != expected {
        return Err(format!(
            "grid {columns}x{rows} writes {} colours, expected {expected}",
            colors.len()
        ));
    }
    // The authored surface: `wNumber * hNumber` bare `x y z` rows. A grid is
    // laid out cell by cell, not vertex by vertex, so two columns and four rows
    // write eight corners — the top and bottom edge of each quad column. The
    // quads between them are what the emitter draws.
    let mut vertices = Vec::with_capacity(expected);
    for row in lookup.all(GRID_VERTEX) {
        let mut parts = row.split_whitespace();
        let mut next = |axis: &str| -> Result<f32, String> {
            parts
                .next()
                .ok_or_else(|| format!("grid vertex missing {axis} in {row:?}"))
                .and_then(|value| float("grid vertex", value))
        };
        let point = Vec3::new(next("x")?, next("y")?, next("z")?);
        if !point.is_finite() {
            return Err(format!("non-finite grid vertex {row:?}"));
        }
        vertices.push(point);
    }
    if !vertices.is_empty() && vertices.len() != expected {
        return Err(format!(
            "grid {columns}x{rows} writes {} vertices, expected {expected}",
            vertices.len()
        ));
    }
    Ok(Grid {
        columns,
        rows,
        colors,
        vertices,
        cell_size: lookup.number("fGridSize", 1.)?,
        z_offset: lookup.number("fZOffset", 0.)?,
    })
}

fn keypoint(reader: &mut Reader) -> Result<KeyPoint, String> {
    let interpolate_mode = clamp_u32(reader.integer("InterpolateMode")?);
    let time_span = reader.number("TimeSpan")?;
    let position = reader.vector("Position")?;
    let raw_color = color("Color", &reader.take("Color")?)?;
    let scale = reader.number("Scale")?;
    let direction = quaternion("Direction", &reader.take("Direction")?)?;
    let rad_2d = reader.number("Rad_2D")?;
    let count = reader.integer("CtrlMethodCount")?;
    if count < 0 || count > 64 {
        return Err(format!("controller count {count} is out of range"));
    }
    let mut controls = Vec::with_capacity(count as usize);
    for _ in 0..count {
        controls.push(control(reader)?);
    }
    if !time_span.is_finite() {
        return Err("non-finite keypoint TimeSpan".into());
    }
    // Keypoint `Scale` is whatever the editor left in the slot: 18 corpus
    // keypoints are negative and four are uninitialized garbage (for example
    // `Scale: -94883208.000000` and a `Position` of 1.8e28 in the element-125
    // generation record of `gfx/策划联入/人物技能/击中/巫师_冷凝术_击中.gfx`).
    // The client reads them as floats and renders the result, so only a
    // non-finite value is rejected; the player substitutes 1.0 for a
    // non-positive scale instead of collapsing the quad.
    if !scale.is_finite() {
        return Err(format!("invalid keypoint Scale {scale}"));
    }
    if !position.is_finite() {
        return Err("non-finite keypoint Position".into());
    }
    Ok(KeyPoint {
        interpolate_mode,
        time_span,
        position,
        color: raw_color,
        scale,
        direction,
        rad_2d,
        controls,
    })
}

fn control(reader: &mut Reader) -> Result<Control, String> {
    let kind = clamp_u32(reader.integer("CtrlType")?);
    let start = reader.number("StartTime")?;
    let end = reader.number("EndTime")?;
    if !start.is_finite() || start < 0. {
        return Err(format!(
            "controller {kind} has an invalid StartTime {start}"
        ));
    }
    if !end.is_finite() {
        return Err(format!("controller {kind} has a non-finite EndTime"));
    }
    let data = match kind {
        100 => ControlData::Motion {
            dir: reader.vector("Dir")?,
            vel: reader.number("Vel")?,
            acc: reader.number("Acc")?,
        },
        101 => ControlData::Speed {
            vel: reader.number("Vel")?,
            acc: reader.number("Acc")?,
        },
        102 => ControlData::Orbit {
            pos: reader.vector("Pos")?,
            axis: reader.vector("Axis")?,
            vel: reader.number("Vel")?,
            acc: reader.number("Acc")?,
        },
        103 => ControlData::Revol {
            pos: reader.vector("Pos")?,
            axis: reader.vector("Axis")?,
            vel: reader.number("Vel")?,
            acc: reader.number("Acc")?,
        },
        104 => ControlData::Attract {
            pos: reader.vector("Pos")?,
            vel: reader.number("Vel")?,
            acc: reader.number("Acc")?,
        },
        105 => {
            let value = reader.take("ColorDelta")?;
            let parts = signed_components("ColorDelta", &value)?;
            if parts.len() != 4 {
                return Err("ColorDelta needs four channel rates".into());
            }
            ControlData::ColorDelta {
                delta: [
                    parts[0] as i32,
                    parts[1] as i32,
                    parts[2] as i32,
                    parts[3] as i32,
                ],
            }
        }
        106 => {
            let value = reader.take("ScaleChage")?;
            let parts = components("ScaleChage", &value)?;
            if parts.len() != 3 {
                return Err("ScaleChage needs rate, minimum and maximum".into());
            }
            // No corpus root inverts the pair, and the native clamp is two
            // separate comparisons (low, then high), so an inverted pair would
            // simply pin the scale to `max`. That is a value, not damage.
            for part in &parts {
                if !part.is_finite() {
                    return Err("ScaleChage carries a non-finite component".into());
                }
            }
            ControlData::ScaleChange {
                rate: parts[0],
                min: parts[1],
                max: parts[2],
            }
        }
        // 107, 109 and 112 share one payload; the kind picks the Process law.
        107 | 109 | 112 => ControlData::Fractal {
            kind: match kind {
                107 => NoiseKind::Color,
                109 => NoiseKind::Scale,
                _ => NoiseKind::Base,
            },
            buffer_length: clamp_u32(reader.integer("BufLen")?),
            amplitude: reader.number("Amplitude")?,
            wave_length: reader.number("WaveLen")?,
            persistence: reader.number("Persistence")?,
            octaves: clamp_u32(reader.integer("OctaveNum")?),
            base_color: match kind {
                107 => Some(color("BaseColor", &reader.take("BaseColor")?)?),
                _ => None,
            },
        },
        108 => {
            let original = color("Color", &reader.take("Color")?)?;
            let count = reader.integer("Count")?;
            if count < 0 || count > 10 {
                return Err(format!("colour ramp Count {count} is out of range"));
            }
            let mut stops = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let value = color("Color", &reader.take("Color")?)?;
                // Ramp stop spans are authored as 0 (414 corpus stops), -1 (14)
                // and occasionally as uninitialized garbage (12). The client
                // stores them and builds its table anyway, so they parse; a
                // non-positive span just leaves that stop out of the table the
                // player builds (see `gfx_player::color_track`).
                let span = reader.number("TimeSpan")?;
                if !span.is_finite() {
                    return Err(format!("colour ramp stop TimeSpan {span} is not finite"));
                }
                stops.push((value, span));
            }
            ControlData::ColorRamp { original, stops }
        }
        110 => {
            // `CalcDir` is optional: 129 of the 1,918 retail paths omit it.
            let calc_dir = reader.flag("CalcDir", false)?;
            let count = reader.integer("Count")?;
            if count < 0 || count > 64 {
                return Err(format!("path Count {count} is out of range"));
            }
            let mut points = Vec::with_capacity(count as usize);
            for _ in 0..count {
                points.push(reader.vector("Pos")?);
            }
            ControlData::Path { calc_dir, points }
        }
        111 => {
            let first = reader.number("Scale")?;
            let first = if first.is_finite() {
                first.clamp(-100., 100.)
            } else {
                1.
            };
            let count = reader.integer("Count")?;
            if count < 0 || count > 10 {
                return Err(format!("scale ramp Count {count} is out of range"));
            }
            let mut stops = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let value = reader.number("Scale")?;
                let span = reader.number("TimeSpan")?;
                if !span.is_finite() {
                    return Err(format!("scale ramp stop TimeSpan {span} is not finite"));
                }
                // `Scale` here is a multiplier on the element's authored size.
                // 88 of the corpus's 41,960 stops carry uninitialized editor
                // memory instead — `-4.24901e23`, `8.26e23`, one as large as
                // `1.10e27` — the same class of junk the keypoint `Scale` guard
                // above already names. The largest *legitimate* stop in the whole
                // corpus is 100 (p99.9 is 80), so anything beyond that is junk.
                //
                // Clamping to a bound wider than the authored range is worse than
                // useless: a clamp of 1000 turned `1.10e27` into a perfectly
                // valid multiplier of 1000, and 1000 x a `DedaultScale` of 3 is
                // the 2000-unit flat white quad that covers a map. Clamping to
                // the authored maximum keeps the ramp's shape and timing on every
                // honest stop and reduces the junk ones to the largest size the
                // editor ever shipped.
                const MAX_SCALE_STOP: f32 = 100.;
                let value = if value.is_finite() {
                    value.clamp(-MAX_SCALE_STOP, MAX_SCALE_STOP)
                } else {
                    1.
                };
                stops.push((value, span));
            }
            ControlData::ScaleRamp { first, stops }
        }
        other => return Err(format!("unsupported CtrlType {other}")),
    };
    Ok(Control {
        kind,
        start,
        end,
        data,
    })
}

/// Every key the retail corpus writes inside an element specification block.
/// Anything else is an error rather than a silent skip.
const KNOWN_KEYS: &[&str] = &[
    "3DParticle",
    "Acc",
    "AccDir",
    "Alpha",
    "AlphaCmp",
    "AlphaEnd",
    "AlphaSeg",
    "AlphaStart",
    "Ambient",
    "Amp",
    "Amplitude",
    "Angle",
    "AreaSize",
    "Attenuation0",
    "AvgGenSeg",
    "Axis",
    "BaseColor",
    "BetaSeg",
    "Bind",
    "BufLen",
    "CalcDir",
    "Circles",
    "Coeff",
    "Color",
    "ColorMax",
    "ColorMin",
    "Count",
    "Deviation",
    "Diffuse",
    "Dir",
    "Direction",
    "DragPow",
    "DummyEle",
    "EmissionRate",
    "EnableMat",
    "EnableOrgPos1",
    "EnableOrgPos2",
    "EnablePos",
    "EndPos",
    "Facing",
    "FallOff",
    "Filter",
    "Force2D",
    "GfxPath",
    "GrndNormOnly",
    "GroundHeight",
    "Height",
    "IsAvgGen",
    "IsAvgGenSeq",
    "IsBind",
    "IsDrag",
    "IsDummy",
    "IsLoop",
    "IsStartOnGrnd",
    "IsSurface",
    "IsUseHSVInterp",
    "IsUseParUV",
    "Interval",
    "LightNum",
    "LightType",
    "Loops",
    "MatchSurface",
    "MaxBranches",
    "MaxDist",
    "MaxExtent",
    "MaxSteps",
    "MinDist",
    "ModelActName",
    "ModelPath",
    "Normal",
    "NoHeiScale",
    "NoRadScale",
    "NoScale",
    "OctaveNum",
    "OrgAtCenter",
    "OrgPos1",
    "OrgPos2",
    "OrgPt",
    "OutColor",
    "ParAcc",
    "ParIniDir",
    "ParticleHeight",
    "ParticleWidth",
    "Path",
    "PathNum",
    "PerBolts",
    "Persistence",
    "Phi",
    "Pitch",
    "Pos",
    "Pos1Enable",
    "Pos2Enable",
    "Position",
    "Quota",
    "Rad_2D",
    "Radius",
    "Range",
    "RotFromView",
    "RotMax",
    "RotMin",
    "Scale",
    "ScaleChage",
    "ScaleMax",
    "ScaleMin",
    "ScaleNoOff",
    "Sects",
    "SegLife",
    "Segs",
    "SoundVer",
    "Specular",
    "Speed",
    "StartPos",
    "StepMax",
    "StepMin",
    "StrokeAmp",
    "TTL",
    "TexAnimMaxTime",
    "TimeSpan",
    "Theta",
    "TileMode",
    "Use3DCam",
    "UseNormal",
    "Vel",
    "Volume",
    "Warp",
    "WaveLen",
    "Width",
    "WidthEnd",
    "WidthStart",
    "WriteZ",
    "YawEffect",
    "ZOffset",
    "dwColor",
    "fGridSize",
    "fZOffset",
    "height",
    "isappend",
    "istaildisappear",
    "renderside",
    "tailfadeout",
    "vertslife",
    "wNumber",
    "hNumber",
    "priority",
    "LoopFlag",
    "PlaySpeed",
    "ColorDelta",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 37 header: no `FaceToViewer`, no `UseAABB`, no 2D flags. CRLF
    /// endings, exactly as the retail container is written.
    fn header37(elements: u32) -> String {
        format!(
            "MOXTVersion: 37\r\nDedaultScale: 2\r\nPlaySpeed: 1\r\nDefaultAlpha: 0.5\r\n\
             Raytrace: 0\r\nFadeByDist: 0\r\nFadeStart: 0.000000\r\nFadeEnd: 0.000000\r\n\
             Vec: 0.100000, 0.200000, 0.300000\r\nVec: 1.000000, 2.000000, 3.000000\r\n\
             ShakeCam: 0\r\nGFXELEMENTCOUNT: {elements}\r\n"
        )
    }

    /// The element-100 record every retail sprite carries, verbatim shape from
    /// `gfx/无标题.gfx` and the `gfx_decal` fixture.
    const SPRITE: &str = "GFXELEMENTID: 100\r\nName: glow\r\nSrcBlend: 5\r\nDestBlend: 2\r\n\
        RepeatCount: 1\r\nRepeatDelay: 0\r\nTexFile: 粒子\\glow.dds\r\nBindEle: \r\nZEnable: 0\r\n\
        MatchGrnd: 0\r\nTexRow: 1\r\nTexCol: 1\r\nTexInterval: 50\r\nPriority: 2\r\nIsDummy: 0\r\n\
        DummyEle: \r\nWarp: 0\r\nWidth: 0.25\r\nHeight: 0.5\r\nRotFromView: 0\r\nGrndNormOnly: 0\r\n\
        StartTime: 0\r\nKEYPOINTCOUNT: 1\r\nInterpolateMode: 0\r\nTimeSpan: -1\r\n\
        Position: 1, 2, 3\r\nColor: -1\r\nScale: 2\r\nDirection: 0, 0, 0, 1\r\nRad_2D: 0\r\n\
        CtrlMethodCount: 0\r\n";

    #[test]
    fn version_37_sprite_root_reads_every_shared_and_class_key() {
        let root = parse(&(header37(1) + SPRITE)).unwrap();
        assert_eq!(root.version, 37);
        assert_eq!(root.default_scale, 2.);
        assert_eq!(root.default_alpha, 0.5);
        assert!(!root.face_to_viewer && !root.use_aabb && !root.phys_exist);
        assert!(!root.no_change_dir && !root.render_2d && !root.back_layer_2d);
        assert_eq!(root.aabb_center, Vec3::new(0.1, 0.2, 0.3));
        assert_eq!(root.aabb_half, Vec3::new(1., 2., 3.));
        assert_eq!(root.bounds_radius(), Vec3::new(1., 2., 3.).length());
        assert!(root.controls.is_empty());
        assert_eq!(root.elements.len(), 1);

        let element = &root.elements[0];
        assert_eq!(element.id, 100);
        assert_eq!(element.common.name, "glow");
        assert_eq!(element.common.source_blend, 5);
        assert_eq!(element.common.destination_blend, 2);
        assert_eq!(element.common.texture, "粒子\\glow.dds");
        assert!(element.common.bind_element.is_empty());
        // The authored value; the version-76 rule is applied when the quad is
        // built, not here.
        assert!(!element.common.depth_test);
        assert!(!element.common.match_ground);
        assert_eq!(element.common.tex_interval, 50);
        assert_eq!(element.common.priority, 2);
        assert_eq!(element.common.tex_speed, [0.; 2]);
        assert_eq!(element.common.no_scale, [false; 2]);
        assert_eq!(element.common.origin, [0.5; 2]);
        assert_eq!(element.keypoints.len(), 1);
        assert!(element.affectors.is_empty());
        assert!(element.generation_sequence.is_none());

        let Spec::Sprite(sprite) = &element.spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(sprite.width, 0.25);
        assert_eq!(sprite.height, 0.5);
        assert!(!sprite.rotate_from_view && !sprite.match_surface && !sprite.yaw_effect);
        // No `MaxExtent` key, so the loader's tail computes 1.5 * max(0.25, 0.5).
        assert_eq!(sprite.max_extent, 0.75);
        // Version 37 is below the 42/54/55/61 gates, so these stay default.
        assert_eq!(sprite.z_offset, 0.);

        let key = &element.keypoints[0];
        assert_eq!(key.time_span, -1.);
        assert_eq!(key.position, Vec3::new(1., 2., 3.));
        assert_eq!(key.color, 0xffff_ffff);
        assert_eq!(key.scale, 2.);
        assert_eq!(key.direction, Quat::IDENTITY);
        assert_eq!(key.alpha(), 1.);
        assert_eq!(key.rgb(), [1.; 3]);
        assert!(key.controls.is_empty());
        // A forever keypoint makes the root clock open ended.
        assert_eq!(root.duration_ms(), None);
        assert_eq!(element.duration_ms(), None);
        assert!(root.has_visual_elements());
    }

    /// The sprite loader gates three key groups below the class keys as well:
    /// `RotFromView` needs version 9 (`0x71dd61`), `GrndNormOnly` 26
    /// (`0x71deaa`) and both `NoScale` and both `OrgPt` values 36 (`0x71def0`).
    /// An older root keeps the constructed defaults even when the keys are
    /// present. No corpus root violates any of them — the 943 roots below
    /// version 36, which reach down to version 28, author neither `NoScale` nor
    /// `OrgPt` — so this completes the ladder rather than changing behaviour.
    #[test]
    fn sprite_keys_below_their_version_gate_keep_their_defaults() {
        let authored = SPRITE.replace(
            "RotFromView: 0\r\nGrndNormOnly: 0\r\n",
            "RotFromView: 1\r\nGrndNormOnly: 1\r\nNoScale: 1\r\nNoScale: 1\r\n\
             OrgPt: 0\r\nOrgPt: 0\r\n",
        );
        assert_ne!(authored, SPRITE, "the fixture has to carry the gated keys");
        let sprite_of = |source: String| -> Sprite {
            let root = parse(&source).unwrap();
            match &root.elements[0].spec {
                Spec::Sprite(sprite) => sprite.clone(),
                other => panic!("element 100 is a sprite, got {other:?}"),
            }
        };

        // Version 37 clears every gate the fixture exercises.
        let open = sprite_of(header37(1) + &authored);
        assert!(open.rotate_from_view && open.ground_normal_only);
        assert_eq!(open.no_scale, [true, true]);
        assert_eq!(open.origin, [0., 0.]);

        // Version 30 clears 9 and 26 but not 36.
        let header30 = header37(1).replace("MOXTVersion: 37", "MOXTVersion: 30");
        let gated = sprite_of(header30 + &authored);
        assert!(gated.rotate_from_view && gated.ground_normal_only);
        assert_eq!(gated.no_scale, [false, false], "NoScale needs version 36");
        assert_eq!(gated.origin, [0.5, 0.5], "OrgPt needs version 36");

        // Version 25 is below GrndNormOnly's gate too, but above RotFromView's.
        let header25 = header37(1).replace("MOXTVersion: 37", "MOXTVersion: 25");
        let oldest = sprite_of(header25 + &authored);
        assert!(oldest.rotate_from_view, "RotFromView only needs version 9");
        assert!(!oldest.ground_normal_only, "GrndNormOnly needs version 26");
        assert_eq!(oldest.no_scale, [false, false]);
        assert_eq!(oldest.origin, [0.5, 0.5]);
    }

    /// `MaxExtent` is computed when it is zero or absent, and kept when it is
    /// authored in a root whose version reaches the gate.
    #[test]
    fn max_extent_defaults_to_one_and_a_half_times_the_larger_side() {
        let header79 = header37(1).replace("MOXTVersion: 37", "MOXTVersion: 79");
        // Absent key at version 79: still the computed default.
        let absent = parse(&(header79.clone() + SPRITE)).unwrap();
        let Spec::Sprite(sprite) = &absent.elements[0].spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(sprite.max_extent, 1.5 * 0.5);
        // Authored key at version 79: the author's value wins.
        let authored =
            parse(&(header79 + &SPRITE.replace("Width: 0.25", "MaxExtent: 3.5\r\nWidth: 0.25")))
                .unwrap();
        let Spec::Sprite(sprite) = &authored.elements[0].spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(sprite.max_extent, 3.5);
        // Authored zero is indistinguishable from absent, so it is recomputed.
        let zero = parse(
            &(header37(1).replace("MOXTVersion: 37", "MOXTVersion: 79")
                + &SPRITE.replace("Width: 0.25", "MaxExtent: 0.000000\r\nWidth: 0.25")),
        )
        .unwrap();
        let Spec::Sprite(sprite) = &zero.elements[0].spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(sprite.max_extent, 0.75);
    }

    #[test]
    fn version_79_header_gates_and_root_noise_controller() {
        // Verbatim header of the 241 retail roots that carry a root controller,
        // with `ShakeCam` and the noise keys set to values the corpus does not
        // use — it authors 1, and leaves `BufLen`, `WaveLen`, `Persistence` and
        // `OctaveNum` at the constructor's 256, 1, 0.25 and 5 — so this also
        // covers a root that is not one of them.
        let source = "MOXTVersion: 79\nDedaultScale: 1.000000\nPlaySpeed: 1.000000\n\
            DefaultAlpha: 1.000000\nRaytrace: 0\nFaceToViewer: 0\nFadeByDist: 1\n\
            FadeStart: 100.000000\nFadeEnd: 400.000000\nVec: 0.000000, 0.000000, 0.000000\n\
            Vec: 0.500000, 0.500000, 0.500000\nUseAABB: 1\nShakeCam: 3\nCtrlType: 112\n\
            StartTime: 0.000000\nEndTime: -1.000000\nBufLen: 256\nAmplitude: 1.000000\n\
            WaveLen: 1\nPersistence: 0.500000\nOctaveNum: 4\nNoChangeDir: 0\n2DRender: 0\n\
            2DBackLayer: 0\nPhysExist: 1\nGFXELEMENTCOUNT: 0\n";
        let root = parse(source).unwrap();
        assert_eq!(root.version, 79);
        assert!(root.fade_by_dist);
        assert_eq!((root.fade_start, root.fade_end), (100., 400.));
        assert!(root.use_aabb && root.phys_exist);
        assert!(
            root.shakes_camera(),
            "the fixture authors 3, which the client's setne stores as a plain true"
        );
        assert_eq!(root.controls.len(), 1);
        let control = &root.controls[0];
        assert_eq!(control.kind, 112);
        assert_eq!(control.start, 0.);
        assert_eq!(control.end, -1.);
        assert_eq!(
            control.data,
            ControlData::Fractal {
                kind: NoiseKind::Base,
                buffer_length: 256,
                amplitude: 1.,
                wave_length: 1.,
                persistence: 0.5,
                octaves: 4,
                base_color: None,
            }
        );
        assert_eq!(root.duration_ms(), Some(0.));
        assert!(!root.has_visual_elements());
    }

    #[test]
    fn particle_123_reads_its_affector_list_before_the_clock() {
        // Verbatim element from `gfx/场景/符文阵效果2改.gfx`.
        let element = "GFXELEMENTID: 123\nName: Unnamed1\nSrcBlend: 5\nDestBlend: 2\n\
            RepeatCount: 0\nRepeatDelay: 0\nTexFile: 粒子\\particle001.dds\nBindEle: \nZEnable: 1\n\
            MatchGrnd: 0\nGroundHeight: 0\nTexRow: 1\nTexCol: 1\nTexInterval: 50\nPriority: 0\n\
            IsDummy: 0\nDummyEle: \nWarp: 0\nTileMode: 0\nTexSpeed: 0.100000\nTexSpeed: 0.000000\n\
            UReverse: 0\nVReverse: 0\nUVExchg: 0\nRenderLayer: 0\nNoDownSample: 0\nResetLoopEnd: 0\n\
            TexAnimMaxTime: 0\nQuota: -1\nParticleWidth: 0.200000\nParticleHeight: 0.200000\n\
            3DParticle: 0\nFacing: 0\nScaleNoOff: 0\nNoScale: 0\nNoScale: 0\nOrgPt: 0.500000\n\
            OrgPt: 0.500000\nIsUseParUV: 0\nIsStartOnGrnd: 0\nEmissionRate: 20.000000\nAngle: 0.000000\n\
            Speed: 0.000000\nParAcc: 0.000000\nAccDir: 0.000000, 0.000000, -1.000000\nAcc: 0.000000\n\
            TTL: 0.700000\nColorMin: -544945\nColorMax: -544945\nScaleMin: 0.050000\n\
            ScaleMax: 0.200000\nRotMin: 0.000000\nRotMax: 0.000000\nIsSurface: 0\nIsBind: 0\n\
            IsDrag: 0\nDragPow: 1.000000\nParIniDir: 0.000000, 0.000000, 0.000000\n\
            IsUseHSVInterp: 0\nAreaSize: 0.100000, 0.100000, 0.100000\nIsAvgGen: 0\nAlphaSeg: 10\n\
            BetaSeg: 10\nAffectorCount: 5\nCtrlType: 101\nStartTime: 0.000000\nEndTime: -1.000000\n\
            Vel: 1.745329\nAcc: 0.000000\nCtrlType: 106\nStartTime: 0.000000\nEndTime: -1.000000\n\
            ScaleChage: 0.500000, 0.010000, 9999.000000\nCtrlType: 104\nStartTime: 0.000000\n\
            EndTime: -1.000000\nPos: 0.000000, 0.000000, 0.000000\nVel: 0.500000\nAcc: 0.000000\n\
            CtrlType: 105\nStartTime: 0.000000\nEndTime: -1.000000\nColorDelta: 0, 0, 0, -1000\n\
            CtrlType: 109\nStartTime: 0.000000\nEndTime: -1.000000\nBufLen: 256\nAmplitude: 0.500000\n\
            WaveLen: 50\nPersistence: 0.250000\nOctaveNum: 5\nStartTime: 0\nKEYPOINTCOUNT: 1\n\
            InterpolateMode: 0\nTimeSpan: 700\nPosition: 0.000000, 0.000000, 0.000000\nColor: -1\n\
            Scale: 1.000000\nDirection: 0.000000, 0.000000, 0.000000, 1.000000\nRad_2D: 0.000000\n\
            CtrlMethodCount: 0\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let element = &root.elements[0];
        let Spec::Particle(particle) = &element.spec else {
            panic!("element 123 is a particle emitter");
        };
        assert_eq!(particle.quota, -1);
        assert_eq!((particle.width, particle.height), (0.2, 0.2));
        assert!(!particle.is_3d && particle.facing == 0);
        assert_eq!(particle.emission_rate, 20.);
        assert_eq!(particle.ttl, 0.7);
        assert_eq!(particle.color_min, 0xfff7_af4f);
        assert_eq!(particle.color_min as i32, -544945);
        assert_eq!(particle.color_max, particle.color_min);
        assert_eq!((particle.scale_min, particle.scale_max), (0.05, 0.2));
        assert_eq!(particle.area_size, Some(Vec3::splat(0.1)));
        assert_eq!(particle.radius, None);
        assert_eq!(particle.is_average_generation, Some(false));
        assert_eq!(particle.alpha_segments, Some(10));
        assert_eq!(particle.beta_segments, Some(10));
        assert_eq!(element.common.repeat_count, 0);
        assert_eq!(element.common.tex_speed, [0.1, 0.]);
        assert!(element.common.depth_test);

        let kinds: Vec<u32> = element.affectors.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [101, 106, 104, 105, 109]);
        assert_eq!(
            element.affectors[0].data,
            ControlData::Speed {
                vel: 1.745329,
                acc: 0.
            }
        );
        assert_eq!(
            element.affectors[1].data,
            ControlData::ScaleChange {
                rate: 0.5,
                min: 0.01,
                max: 9999.
            }
        );
        assert_eq!(
            element.affectors[2].data,
            ControlData::Attract {
                pos: Vec3::ZERO,
                vel: 0.5,
                acc: 0.
            }
        );
        assert_eq!(
            element.affectors[3].data,
            ControlData::ColorDelta {
                delta: [0, 0, 0, -1000]
            }
        );
        match &element.affectors[4].data {
            ControlData::Fractal {
                kind,
                buffer_length,
                amplitude,
                wave_length,
                persistence,
                octaves,
                base_color,
            } => {
                assert_eq!(*kind, NoiseKind::Scale);
                assert_eq!(*buffer_length, 256);
                assert_eq!(*amplitude, 0.5);
                assert_eq!(*wave_length, 50.);
                assert_eq!(*persistence, 0.25);
                assert_eq!(*octaves, 5);
                assert_eq!(*base_color, None);
            }
            other => panic!("controller 109 is noise, found {other:?}"),
        }
        assert_eq!(element.keypoints[0].time_span, 700.);
        assert_eq!(element.duration_ms(), Some(700.));
        assert_eq!(root.duration_ms(), Some(700.));
        assert!(element.generation_sequence.is_none());
    }

    #[test]
    fn particle_125_reads_its_generation_sequence_path_first() {
        // Verbatim element from `gfx/npc/女npc.gfx`: the sphere emitter writes a
        // keypoint-shaped generation sequence, then its affectors, then the clock.
        let element = "GFXELEMENTID: 125\nName: Unnamed0\nSrcBlend: 5\nDestBlend: 2\n\
            RepeatCount: 0\nRepeatDelay: 0\nTexFile: 粒子\\particle002.dds\nBindEle: \nZEnable: 1\n\
            MatchGrnd: 0\nGroundHeight: 0\nTexRow: 1\nTexCol: 1\nTexInterval: 50\nPriority: 0\n\
            IsDummy: 0\nDummyEle: \nWarp: 0\nTileMode: 0\nTexSpeed: 0.000000\nTexSpeed: 0.000000\n\
            UReverse: 0\nVReverse: 0\nUVExchg: 0\nRenderLayer: 0\nNoDownSample: 0\nQuota: -1\n\
            ParticleWidth: 0.100000\nParticleHeight: 0.100000\n3DParticle: 1\nFacing: 0\n\
            ScaleNoOff: 0\nNoScale: 0\nNoScale: 0\nOrgPt: 0.500000\nOrgPt: 0.500000\nIsUseParUV: 0\n\
            IsStartOnGrnd: 0\nEmissionRate: 30.000000\nAngle: 0.000000\nSpeed: 0.000000\n\
            ParAcc: 0.000000\nAccDir: 0.000000, 0.000000, -1.000000\nAcc: 0.000000\nTTL: 1.000000\n\
            ColorMin: -1\nColorMax: -1\nScaleMin: 0.500000\nScaleMax: 1.000000\nRotMin: 0.000000\n\
            RotMax: 6.283185\nIsSurface: 0\nIsBind: 1\nIsDrag: 0\nDragPow: 1.000000\n\
            ParIniDir: 0.000000, 0.000000, 0.000000\nIsUseHSVInterp: 0\nRadius: 0.000000\n\
            IsAvgGen: 0\nAvgGenSeg: 0\nIsAvgGenSeq: 1\nInterpolateMode: 0\nTimeSpan: 0\n\
            Position: 0.000000, 0.000000, 0.000000\nColor: -1\nScale: 1.000000\n\
            Direction: 0.000000, 0.000000, 0.000000, 1.000000\nRad_2D: 0.000000\nCtrlMethodCount: 1\n\
            CtrlType: 110\nStartTime: 0.000000\nEndTime: -1.000000\nCalcDir: 0\nCount: 3\n\
            Pos: 0.000000, 0.000000, 0.000000\nPos: -0.022347, 0.331780, -0.027202\n\
            Pos: -0.039753, 0.591980, -0.037096\nAffectorCount: 2\nCtrlType: 106\nStartTime: 0.000000\n\
            EndTime: -1.000000\nScaleChage: 0.500000, 0.010000, 9999.000000\nCtrlType: 108\n\
            StartTime: 0.000000\nEndTime: -1.000000\nColor: 16769968\nCount: 3\nColor: -795458\n\
            TimeSpan: 300.000000\nColor: -7000\nTimeSpan: 300.000000\nColor: 16773322\n\
            TimeSpan: 300.000000\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\n\
            Position: 0.000000, 0.000000, 0.000000\nColor: -1\nScale: 1.000000\n\
            Direction: 0.000000, 0.000000, 0.000000, 1.000000\nRad_2D: 0.000000\nCtrlMethodCount: 0\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let element = &root.elements[0];
        let Spec::Particle(particle) = &element.spec else {
            panic!("element 125 is a sphere emitter");
        };
        assert!(particle.is_3d);
        assert!(particle.is_bind);
        assert_eq!(particle.radius, Some(0.));
        assert_eq!(particle.area_size, None);
        assert_eq!(particle.is_average_generation_sequence, Some(true));
        assert_eq!(particle.average_generation_segments, Some(0));
        assert_eq!(particle.rot_max, 6.283185);

        let sequence = element
            .generation_sequence
            .as_ref()
            .expect("generation sequence");
        assert_eq!(sequence.controls.len(), 1);
        match &sequence.controls[0].data {
            ControlData::Path { calc_dir, points } => {
                assert!(!calc_dir);
                assert_eq!(points.len(), 3);
                assert_eq!(points[1], Vec3::new(-0.022347, 0.331780, -0.027202));
            }
            other => panic!("controller 110 is a path, found {other:?}"),
        }
        assert_eq!(element.affectors.len(), 2);
        match &element.affectors[1].data {
            ControlData::ColorRamp { original, stops } => {
                assert_eq!(*original, 16769968u32);
                assert_eq!(stops.len(), 3);
                assert_eq!(stops[0], ((-795458i32) as u32, 300.));
                assert_eq!(stops[2].1, 300.);
            }
            other => panic!("controller 108 is a colour ramp, found {other:?}"),
        }
    }

    #[test]
    fn fractal_150_keeps_repeated_keys_in_file_order() {
        // Verbatim element from `gfx/models/暗语者营地/暗语者营地.gfx`: `Amplitude`
        // and `WaveLen` each appear twice with different meanings, and the
        // ribbon writes three `Width` and three `Alpha` values.
        let element = "GFXELEMENTID: 150\nName: 11\nSrcBlend: 5\nDestBlend: 2\nRepeatCount: 0\n\
            RepeatDelay: 0\nTexFile: 闪电\\lightning002.dds\nBindEle: \nZEnable: 1\nMatchGrnd: 0\n\
            GroundHeight: 0\nTexRow: 1\nTexCol: 1\nTexInterval: 50\nPriority: 0\nIsDummy: 0\n\
            DummyEle: \nWarp: 0\nTileMode: 0\nTexSpeed: 0.000000\nTexSpeed: 0.000000\nUReverse: 0\n\
            VReverse: 0\nUVExchg: 0\nRenderLayer: 0\nNoDownSample: 0\nResetLoopEnd: 0\n\
            TexAnimMaxTime: 0\nBufLen: 256\nAmplitude: 2.000000\nWaveLen: 1\nPersistence: 1.000000\n\
            OctaveNum: 5\nStartPos: 0.000000, 0.000000, 0.000000\nEndPos: 0.000000, 0.000000, 0.000000\n\
            Segs: 10\nLightNum: 1\nWaveLen: 0.100000\nInterval: 70\nWidth: 0.000000\nWidth: 0.000000\n\
            Alpha: 0.500000\nAlpha: 0.500000\nWidth: 0.400000\nAlpha: 1.000000\nAmplitude: 1.000000\n\
            Pos1Enable: 0\nPos2Enable: 0\nUseNormal: 0\nNormal: 0.000000, 0.000000, 0.000000\nFilter: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\n\
            Position: 0.000000, 0.000000, 0.000000\nColor: -1\nScale: 1.000000\n\
            Direction: 0.000000, 0.000000, 0.000000, 1.000000\nRad_2D: 0.000000\nCtrlMethodCount: 0\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let Spec::Fractal(fractal) = &root.elements[0].spec else {
            panic!("element 150 is a fractal ribbon");
        };
        assert_eq!(fractal.buffer_length, 256);
        assert_eq!(fractal.noise_amplitude, 2.);
        assert_eq!(fractal.noise_wave_length, 1.);
        assert_eq!(fractal.octaves, 5);
        assert_eq!(fractal.segments, 10);
        assert_eq!(fractal.light_number, 1);
        assert_eq!(fractal.wave_length, 0.1);
        assert_eq!(fractal.interval, 70);
        assert_eq!(fractal.widths, vec![0., 0., 0.4]);
        assert_eq!(fractal.alphas, vec![0.5, 0.5, 1.]);
        assert_eq!(fractal.amplitude, 1.);
        assert_eq!(fractal.filter, 1);
    }

    /// The five trail keys arrive one version at a time — `isappend` and
    /// `renderside` at 67, `istaildisappear` and `vertslife` at 68, `tailfadeout`
    /// at 69 — and `0x713ae3` writes `renderside` straight back out as 0 unless
    /// the file authored 0, 1 or 2. Every corpus trail sits at version 73, 76 or
    /// 79, so all five are read in practice; the ladder is here because it is
    /// what the loader does, and the clamp because a value outside it is
    /// silently rewritten rather than rejected.
    #[test]
    fn trail_keys_are_gated_by_version_and_renderside_is_clamped() {
        let element = |renderside: i32| {
            format!(
                "GFXELEMENTID: 152\nName: t\nSrcBlend: 5\nDestBlend: 2\n\
                 TexFile: 轨迹\\trail003.dds\nSegs: 4\nisappend: 0\nrenderside: {renderside}\n\
                 istaildisappear: 0\nvertslife: 250\ntailfadeout: 1\nStartTime: 0\n\
                 KEYPOINTCOUNT: 0\n"
            )
        };
        let read = |version: u32, renderside: i32| {
            let source = header37(1).replace("MOXTVersion: 37", &format!("MOXTVersion: {version}"))
                + &element(renderside);
            let root = parse(&source).unwrap_or_else(|error| panic!("version {version}: {error}"));
            let Spec::Trail(trail) = &root.elements[0].spec else {
                panic!("element 152 is a trail");
            };
            (
                trail.is_append,
                trail.render_side,
                trail.is_tail_disappear,
                trail.vertices_life,
                trail.tail_fade_out,
            )
        };
        // Below 67 nothing is read, so the constructor defaults stand: append on,
        // tail disappearing on, a thousand milliseconds of life.
        for version in [28u32, 37, 66] {
            assert_eq!(
                read(version, 2),
                (true, 0, true, 1000., 0.),
                "version {version} reads no trail key at all"
            );
        }
        // 67 brings `isappend` and the clamped `renderside`.
        assert_eq!(read(67, 2), (false, 2, true, 1000., 0.));
        assert_eq!(
            read(67, 7),
            (false, 0, true, 1000., 0.),
            "a render side outside 0, 1 and 2 is rewritten to 0"
        );
        assert_eq!(read(67, -1), (false, 0, true, 1000., 0.));
        // 68 brings `istaildisappear` and `vertslife`.
        assert_eq!(read(68, 1), (false, 1, false, 250., 0.));
        // 69 brings `tailfadeout`, and 79 — the newest in the corpus — reads all
        // five.
        for version in [69u32, 73, 76, 79] {
            assert_eq!(
                read(version, 0),
                (false, 0, false, 250., 1.),
                "version {version} reads every trail key"
            );
        }
    }

    /// Element 160 starts life at `A3DGFXModel`'s constructor values —
    /// `m_nLoops(1)`, `m_bAlphaCmp(false)`, `m_bZWriteEnable(false)` — and each
    /// key only arrived later (`Loops` at 31, `AlphaCmp` at 29, `WriteZ` at 49),
    /// so a block that omits them must read back as those defaults. The 201
    /// shipped models without `WriteZ` therefore never write depth.
    #[test]
    fn model_defaults_do_not_write_depth() {
        let block = "GFXELEMENTID: 160\nName: m\nSrcBlend: 5\nDestBlend: 2\n\
            ModelPath: 地图\\水晶.SMD\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let model = "GFXELEMENTID: 160\nName: m\nSrcBlend: 5\nDestBlend: 2\n\
            ModelPath: 地图\\水晶.SMD\nModelActName: \nLoops: -1\nAlphaCmp: 1\nWriteZ: 0\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let source = header37(2) + block + model;
        let root = parse(&source).unwrap();
        let Spec::Model(bare) = &root.elements[0].spec else {
            panic!("element 160 is a model");
        };
        assert_eq!(bare.path, "地图\\水晶.SMD");
        assert_eq!(bare.loops, 1, "constructor default is one loop");
        assert!(!bare.alpha_compare, "constructor default is no alpha test");
        assert!(!bare.write_z, "constructor default writes no depth");
        assert!(!bare.use_3d_camera);
        let Spec::Model(authored) = &root.elements[1].spec else {
            panic!("element 160 is a model");
        };
        assert_eq!(authored.loops, -1);
        assert!(authored.alpha_compare);
        assert!(!authored.write_z, "an authored zero stays off");
    }

    #[test]
    /// The sound's file list is `PathNum` rows of `Path` at root version 88
    /// or later: `GFXSOUNDIMP::Load` (A3DGFXSoundImp.cpp) reads the count
    /// and then exactly that many names into its `RandStringContainer(5)`,
    /// and `ChangeSoundFile` draws one at random per sound change. The
    /// editor labels that list DropFileNames; two rows are two takes of one
    /// cue, and the count is the number of rows, not a boolean.
    #[test]
    fn a_sound_carries_its_whole_path_list() {
        let sound = "GFXELEMENTID: 170\nName: s\nSrcBlend: 5\nDestBlend: 2\n\
            PathNum: 2\nPath: Sfx\\a.wav\nPath: Sfx\\b.wav\nSoundVer: 1\nForce2D: 0\n\
            IsLoop: 0\nVolume: 100\nMinDist: 6.000000\nMaxDist: 25.000000\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let root = parse(&(header37(1) + sound)).unwrap();
        let Spec::Sound(sound) = &root.elements[0].spec else {
            panic!("element 170 is a sound");
        };
        assert_eq!(sound.path, "Sfx\\a.wav");
        assert_eq!(sound.paths, ["Sfx\\a.wav", "Sfx\\b.wav"]);
    }

    fn trail_152_cylinders_grids_children_lights_and_sounds() {
        let trail = "GFXELEMENTID: 152\nName: t\nSrcBlend: 5\nDestBlend: 2\nTexFile: 轨迹\\trail003.dds\n\
            BufLen: 256\nAmplitude: 1.000000\nWaveLen: 1\nPersistence: 0.250000\nOctaveNum: 5\n\
            StartPos: 0.000000, 0.000000, 0.000000\nEndPos: 0.000000, 0.000000, 0.000000\nSegs: 20\n\
            LightNum: 1\nWaveLen: 1.000000\nInterval: 100\nWidth: 0.050000\nWidth: 0.050000\n\
            Alpha: 1.000000\nAlpha: 0.000000\nWidth: 0.070000\nAlpha: 1.000000\nAmplitude: 0.000000\n\
            Pos1Enable: 0\nPos2Enable: 0\nUseNormal: 0\nNormal: 0.000000, 0.000000, 0.000000\nFilter: 1\n\
            isappend: 1\nrenderside: 0\nistaildisappear: 1\nvertslife: 100\ntailfadeout: 0\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let cylinder = "GFXELEMENTID: 140\nName: c\nSrcBlend: 5\nDestBlend: 2\nRadius: 2.000000\n\
            Height: 0.500000\nPitch: 1.570796\nSects: 8\nNoRadScale: 0\nNoHeiScale: 0\nOrgAtCenter: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let grid = "GFXELEMENTID: 210\nName: g\nSrcBlend: 5\nDestBlend: 2\nwNumber: 2\nhNumber: 2\n\
            dwColor: ffffff\ndwColor: 69ffffff\ndwColor: 1effffff\ndwColor: 0\nfGridSize: 0.500000\n\
            fZOffset: 0.000000\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let child = "GFXELEMENTID: 200\nName: ch\nSrcBlend: 5\nDestBlend: 2\n\
            GfxPath: NPC\\女npc_蝴蝶子效果.gfx\nOutColor: 1\nLoopFlag: 0\nPlaySpeed: 1.000000\n\
            StartTime: 250\nKEYPOINTCOUNT: 0\n";
        let light = "GFXELEMENTID: 130\nName: l\nSrcBlend: 5\nDestBlend: 2\nLightType: 1\n\
            Diffuse: -16776961\nSpecular: 0\nAmbient: 0\nPosition: 0.000000, 1.000000, 0.000000\n\
            Direction: 0.000000, 0.000000, 0.000000\nRange: 10.000000\nFallOff: 0.000000\n\
            Attenuation0: 0.000000\nAttenuation0: 0.100000\nAttenuation0: 0.000000\nTheta: 0.000000\n\
            Phi: 0.000000\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let sound = "GFXELEMENTID: 170\nName: s\nSrcBlend: 5\nDestBlend: 2\n\
            Path: Effect\\Player\\魔力燃烧.wav\nSoundVer: 1\nForce2D: 0\nIsLoop: 0\nVolume: 100\n\
            MinDist: 6.000000\nMaxDist: 25.000000\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let model = "GFXELEMENTID: 160\nName: m\nSrcBlend: 5\nDestBlend: 2\n\
            ModelPath: 暗语者营地\\水晶.SMD\nModelActName: \nLoops: 1\nAlphaCmp: 0\nWriteZ: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let source = header37(7) + trail + cylinder + grid + child + light + sound + model;
        let root = parse(&source).unwrap();
        assert_eq!(root.elements.len(), 7);

        let Spec::Trail(trail) = &root.elements[0].spec else {
            panic!("element 152 is a trail");
        };
        // This root is version 37, below every trail gate, so the five values the
        // file authors are ignored and the constructor defaults at `0x7137ea`
        // stand instead — which is why `vertslife: 100` reads back as 1000.
        assert!(trail.is_append && trail.is_tail_disappear);
        assert_eq!(trail.render_side, 0);
        assert_eq!(trail.vertices_life, 1000.);
        assert_eq!(trail.tail_fade_out, 0.);
        assert_eq!(trail.fractal.segments, 20);
        assert_eq!(trail.fractal.alphas, vec![1., 0., 1.]);

        let Spec::Cylinder(cylinder) = &root.elements[1].spec else {
            panic!("element 140 is a cylinder");
        };
        assert_eq!(cylinder.radius, 2.);
        assert_eq!(cylinder.height, 0.5);
        assert_eq!(cylinder.sections, 8);
        assert!(cylinder.origin_at_center);

        let Spec::Grid(grid) = &root.elements[2].spec else {
            panic!("element 210 is a grid");
        };
        assert_eq!((grid.columns, grid.rows), (2, 2));
        // Hex cells are read exactly as the client reads them: six digits leave
        // the alpha byte zero, eight digits carry it.
        assert_eq!(grid.colors, vec![0x00ff_ffff, 0x69ff_ffff, 0x1eff_ffff, 0]);
        assert_eq!(grid.cell_size, 0.5);
        // The flat 2x2 sample carries no bare rows, so the fallback is intact.
        assert!(grid.vertices.is_empty());

        let Spec::Child(child) = &root.elements[3].spec else {
            panic!("element 200 is a nested root");
        };
        assert_eq!(child.path, "NPC\\女npc_蝴蝶子效果.gfx");
        assert!(child.out_color && !child.loop_flag);
        assert_eq!(root.elements[3].start_time, 250.);

        let Spec::Light(light) = &root.elements[4].spec else {
            panic!("element 130 is a light");
        };
        assert_eq!(light.light_type, 1);
        assert_eq!(light.diffuse as i32, -16776961);
        assert_eq!(light.attenuation, [0., 0.1, 0.]);
        assert_eq!(light.range, 10.);
        assert!(!root.elements[4].spec.is_visual());

        let Spec::Sound(sound) = &root.elements[5].spec else {
            panic!("element 170 is a sound");
        };
        assert_eq!(sound.path, "Effect\\Player\\魔力燃烧.wav");
        assert_eq!(sound.paths, ["Effect\\Player\\魔力燃烧.wav"]);
        assert_eq!(sound.volume, 100);
        assert!(!root.elements[5].spec.is_visual());

        let Spec::Model(model) = &root.elements[6].spec else {
            panic!("element 160 is a model");
        };
        assert_eq!(model.path, "暗语者营地\\水晶.SMD");
        assert!(model.write_z && !model.alpha_compare);
        assert!(!root.elements[6].spec.is_visual());
        assert!(root.has_visual_elements());
    }

    #[test]
    fn ribbons_waves_curves_lightning_and_ramps() {
        let ribbon = "GFXELEMENTID: 110\nName: r\nSrcBlend: 5\nDestBlend: 2\n\
            OrgPos1: 0.000000, 0.100000, 0.000000\nOrgPos2: 0.000000, -0.100000, 0.000000\n\
            EnableMat: 0\nEnableOrgPos1: 0\nEnableOrgPos2: 0\nSegLife: 600\nBind: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: 500\n\
            Position: 0, 0, 0\nColor: -1\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\n\
            CtrlMethodCount: 2\nCtrlType: 102\nStartTime: 0\nEndTime: -1\nPos: 0, 0, 0\n\
            Axis: 0, 1, 0\nVel: 3.000000\nAcc: 0.000000\nCtrlType: 111\nStartTime: 0\nEndTime: -1\n\
            Scale: 0.500000\nCount: 2\nScale: 1.500000\nTimeSpan: 200\nScale: 0.250000\nTimeSpan: 300\n";
        let wave = "GFXELEMENTID: 180\nName: w\nSrcBlend: 5\nDestBlend: 2\n\
            Pos: 0.030000, 0.030000, 0.030000\nPos: -0.030000, -0.030000, -0.030000\nSegLife: 700\n\
            Amp: 0.100000\nAmp: 0.300000\nBind: 0\nEnableMat: 0\nEnablePos: 0\nEnablePos: 0\n\
            StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let curve = "GFXELEMENTID: 190\nName: cu\nSrcBlend: 5\nDestBlend: 2\nCoeff: 2.000000\n\
            height: 0.200000\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let bolt = "GFXELEMENTID: 151\nName: b\nSrcBlend: 5\nDestBlend: 2\nDeviation: 0.523599\n\
            StepMin: 0.100000\nStepMax: 1.000000\nWidthStart: 0.030000\nWidthEnd: 0.010000\n\
            AlphaStart: 1.000000\nAlphaEnd: 0.000000\nStrokeAmp: 0.050000\nMaxSteps: 2\n\
            MaxBranches: 1\nInterval: 50\nPerBolts: 2\nCircles: 2\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        let root = parse(&(header37(4) + ribbon + wave + curve + bolt)).unwrap();

        let Spec::Ribbon(ribbon) = &root.elements[0].spec else {
            panic!("element 110 is a ribbon");
        };
        assert_eq!(ribbon.origin_1, Vec3::new(0., 0.1, 0.));
        assert_eq!(ribbon.origin_2, Vec3::new(0., -0.1, 0.));
        assert_eq!(ribbon.segment_life, 600.);
        assert_eq!(ribbon.bind, 1);

        let key = &root.elements[0].keypoints[0];
        assert_eq!(key.time_span, 500.);
        assert_eq!(key.controls.len(), 2);
        assert_eq!(
            key.controls[0].data,
            ControlData::Orbit {
                pos: Vec3::ZERO,
                axis: Vec3::Y,
                vel: 3.,
                acc: 0.
            }
        );
        assert_eq!(
            key.controls[1].data,
            ControlData::ScaleRamp {
                first: 0.5,
                stops: vec![(1.5, 200.), (0.25, 300.)]
            }
        );
        assert_eq!(root.elements[0].duration_ms(), Some(500.));
        assert_eq!(root.duration_ms(), Some(500.));
        assert_eq!(root.elements[0].sample(600.), None);
        let (sampled, offset) = root.elements[0].sample(120.).unwrap();
        assert_eq!(offset, 120.);
        assert_eq!(sampled.time_span, 500.);

        let Spec::Wave(wave) = &root.elements[1].spec else {
            panic!("element 180 is a wave trail");
        };
        assert_eq!(wave.positions.len(), 2);
        assert_eq!(wave.amplitudes, vec![0.1, 0.3]);
        assert_eq!(wave.enable_positions, vec![false, false]);
        assert_eq!(wave.segment_life, 700.);

        let Spec::Curve(curve) = &root.elements[2].spec else {
            panic!("element 190 is a curve");
        };
        assert_eq!((curve.coefficient, curve.height), (2., 0.2));

        let Spec::Lightning(bolt) = &root.elements[3].spec else {
            panic!("element 151 is lightning");
        };
        assert_eq!(bolt.max_steps, 2);
        assert_eq!(bolt.bolts_per_interval, 2);
        assert_eq!(bolt.circles, 2);
        assert_eq!(bolt.width_start, 0.03);
    }

    #[test]
    fn motion_speed_and_colour_controllers_match_the_recovered_layout() {
        let element = "GFXELEMENTID: 101\nName: p\nSrcBlend: 5\nDestBlend: 2\n\
            Width: 1\nHeight: 1\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 1\nTimeSpan: 1000\n\
            Position: 0, 0, 0\nColor: -16777216\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0.5\n\
            CtrlMethodCount: 3\nCtrlType: 100\nStartTime: 10\nEndTime: 900\nDir: 0, 1, 0\n\
            Vel: 2.000000\nAcc: -1.000000\nCtrlType: 101\nStartTime: 0\nEndTime: -1\nVel: 1.500000\n\
            Acc: 0.000000\nCtrlType: 105\nStartTime: 0\nEndTime: -1\nColorDelta: 0, 0, 0, -500\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let key = &root.elements[0].keypoints[0];
        assert_eq!(key.interpolate_mode, 1);
        assert_eq!(key.rad_2d, 0.5);
        assert_eq!(key.color, 0xff00_0000);
        assert_eq!(key.alpha(), 1.);
        assert_eq!(key.rgb(), [0.; 3]);
        assert_eq!(
            key.controls[0].data,
            ControlData::Motion {
                dir: Vec3::Y,
                vel: 2.,
                acc: -1.
            }
        );
        assert_eq!(key.controls[0].start, 10.);
        assert_eq!(key.controls[0].end, 900.);
        assert_eq!(
            key.controls[1].data,
            ControlData::Speed { vel: 1.5, acc: 0. }
        );
        assert_eq!(key.controls[1].end, -1.);
        assert_eq!(
            key.controls[2].data,
            ControlData::ColorDelta {
                delta: [0, 0, 0, -500]
            }
        );
    }

    #[test]
    fn noise_controller_107_carries_its_base_colour() {
        let element = "GFXELEMENTID: 101\nName: n\nSrcBlend: 5\nDestBlend: 2\nWidth: 1\nHeight: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\nPosition: 0, 0, 0\n\
            Color: -1\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 1\n\
            CtrlType: 107\nStartTime: 0\nEndTime: -1\nBufLen: 128\nAmplitude: 3.000000\nWaveLen: 2\n\
            Persistence: 0.500000\nOctaveNum: 3\nBaseColor: -65536\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let control = &root.elements[0].keypoints[0].controls[0];
        assert_eq!(control.kind, 107);
        match &control.data {
            ControlData::Fractal {
                kind,
                buffer_length,
                amplitude,
                wave_length,
                persistence,
                octaves,
                base_color,
            } => {
                assert_eq!(*kind, NoiseKind::Color);
                assert_eq!(*buffer_length, 128);
                assert_eq!(*amplitude, 3.);
                assert_eq!(*wave_length, 2.);
                assert_eq!(*persistence, 0.5);
                assert_eq!(*octaves, 3);
                assert_eq!(*base_color, Some(0xffff_0000));
            }
            other => panic!("controller 107 is noise, found {other:?}"),
        }
    }

    #[test]
    fn paths_without_calcdir_and_ramps_with_one_stop() {
        let element =
            "GFXELEMENTID: 101\nName: p\nSrcBlend: 5\nDestBlend: 2\nWidth: 1\nHeight: 1\n\
            StartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\nPosition: 0, 0, 0\n\
            Color: -1\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 2\n\
            CtrlType: 110\nStartTime: 0\nEndTime: -1\nCount: 2\nPos: 1, 0, 0\nPos: 0, 1, 0\n\
            CtrlType: 108\nStartTime: 0\nEndTime: -1\nColor: -1\nCount: 1\nColor: 16711680\n\
            TimeSpan: 100\n";
        let root = parse(&(header37(1) + element)).unwrap();
        let controls = &root.elements[0].keypoints[0].controls;
        match &controls[0].data {
            ControlData::Path { calc_dir, points } => {
                assert!(!calc_dir);
                assert_eq!(points, &vec![Vec3::X, Vec3::Y]);
            }
            other => panic!("controller 110 is a path, found {other:?}"),
        }
        match &controls[1].data {
            ControlData::ColorRamp { original, stops } => {
                assert_eq!(*original, 0xffff_ffff);
                assert_eq!(stops, &vec![(0x00ff_0000, 100.)]);
            }
            other => panic!("controller 108 is a ramp, found {other:?}"),
        }
    }

    #[test]
    fn rejects_versions_the_client_rejects() {
        let too_new = header37(0).replace("MOXTVersion: 37", "MOXTVersion: 80");
        assert!(parse(&too_new)
            .unwrap_err()
            .contains("newer than the client's 79"));

        // `gfx/sgc/刺客_致盲_技能击中.gfx` is a version 4 skill missile record.
        let mover = "MOXTVersion: 4\nPath: \nPath: \nScale: 1\nMoveMode: 0\nTargetMode: 0\n";
        assert!(parse(mover)
            .unwrap_err()
            .contains("predates the effect container"));

        // `gfx/人物/技能/羽芒/翼击击中.gfx` carries no header at all.
        assert!(parse("").unwrap_err().contains("empty"));
        assert!(parse("GFXELEMENTCOUNT: 0\n")
            .unwrap_err()
            .contains("not MOXTVersion"));
    }

    #[test]
    fn rejects_structural_damage_instead_of_skipping_it() {
        let base = header37(1) + SPRITE;
        let missing = base.replace("Width: 0.25\r\n", "");
        assert!(parse(&missing)
            .unwrap_err()
            .contains("missing its Width key"));

        let unknown = base.replace("GrndNormOnly: 0", "GrndNormOnly: 0\nSomethingNew: 1");
        assert!(parse(&unknown).unwrap_err().contains("unknown key"));

        let count = base.replace("KEYPOINTCOUNT: 1", "KEYPOINTCOUNT: 2");
        let error = parse(&count).unwrap_err();
        assert!(
            error.contains("expected InterpolateMode, found <end of file>"),
            "{error}"
        );

        let trailing = base.clone() + "GFXELEMENTID: 100\n";
        assert!(parse(&trailing).unwrap_err().contains("trailing"));

        let bad_blend = base.replace("SrcBlend: 5", "SrcBlend: abc");
        assert!(parse(&bad_blend).unwrap_err().contains("not a number"));

        let bad_colour = base.replace("Color: -1", "Color: 1, 2");
        assert!(parse(&bad_colour).unwrap_err().contains("not a number"));

        let bad_quat = base.replace("Direction: 0, 0, 0, 1", "Direction: 0, 0, 0");
        assert!(parse(&bad_quat).unwrap_err().contains("four components"));

        let inverted = header37(0)
            .replace("FadeByDist: 0", "FadeByDist: 1")
            .replace("FadeStart: 0.000000", "FadeStart: 900.000000");
        assert!(parse(&inverted).unwrap_err().contains("is inverted"));

        let speed = base.replace("PlaySpeed: 1", "PlaySpeed: -2");
        assert!(parse(&speed).unwrap_err().contains("invalid GFX PlaySpeed"));

        let scale = base.replace("DedaultScale: 2", "DedaultScale: 0");
        assert!(parse(&scale)
            .unwrap_err()
            .contains("invalid GFX DedaultScale"));

        // `DefaultAlpha` above one is authored (two roots use 1.2 to push their
        // additive quads past the keypoint alpha); only a negative or non-finite
        // multiplier would corrupt the per-channel fold.
        let bright = base.replace("DefaultAlpha: 0.5", "DefaultAlpha: 1.2");
        assert_eq!(parse(&bright).unwrap().default_alpha, 1.2);
        let alpha = base.replace("DefaultAlpha: 0.5", "DefaultAlpha: -4");
        assert!(parse(&alpha)
            .unwrap_err()
            .contains("invalid GFX DefaultAlpha"));

        // A ramp stop may span zero milliseconds: 414 corpus stops do, and the
        // player simply leaves that stop out of the table it builds.
        let ramp = base.replace(
            "CtrlMethodCount: 0",
            "CtrlMethodCount: 1\nCtrlType: 108\nStartTime: 0\nEndTime: -1\nColor: -1\nCount: 2\n\
             Color: -1\nTimeSpan: 0\nColor: -1\nTimeSpan: 300",
        );
        let ControlData::ColorRamp { stops, .. } =
            &parse(&ramp).unwrap().elements[0].keypoints[0].controls[0].data
        else {
            panic!("ctrl 108 is a colour ramp");
        };
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].1, 0.);

        // An inverted `ScaleChage` pair clamps to its maximum; it is not damage.
        let clamp = base.replace(
            "CtrlMethodCount: 0",
            "CtrlMethodCount: 1\nCtrlType: 106\nStartTime: 0\nEndTime: -1\n\
             ScaleChage: 1.000000, 5.000000, 2.000000",
        );
        let ControlData::ScaleChange { min, max, .. } =
            &parse(&clamp).unwrap().elements[0].keypoints[0].controls[0].data
        else {
            panic!("ctrl 106 is a scale change");
        };
        assert_eq!((*min, *max), (5., 2.));

        let controller = base.replace(
            "CtrlMethodCount: 0",
            "CtrlMethodCount: 1\nCtrlType: 113\nStartTime: 0\nEndTime: -1\n",
        );
        assert!(parse(&controller)
            .unwrap_err()
            .contains("unsupported CtrlType 113"));

        let grid = header37(1)
            + "GFXELEMENTID: 210\nName: g\nwNumber: 2\nhNumber: 2\ndwColor: ffffff\ndwColor: ffffff\n\
               fGridSize: 1\nfZOffset: 0\nStartTime: 0\nKEYPOINTCOUNT: 0\n";
        assert!(parse(&grid).unwrap_err().contains("expected 4"));

        // `Sects: 0` is the corpus's common case (1064 cylinders), so only an
        // allocation-sized value is out of range.
        let sectors = header37(1)
            + "GFXELEMENTID: 140\nName: c\nRadius: 1\nHeight: 1\nPitch: 0\nSects: 0\n\
               StartTime: 0\nKEYPOINTCOUNT: 0\n";
        let Spec::Cylinder(flat) = &parse(&sectors).unwrap().elements[0].spec else {
            panic!("element 140 is a cylinder");
        };
        assert_eq!(flat.sections, 0);
        let sectors = header37(1)
            + "GFXELEMENTID: 140\nName: c\nRadius: 1\nHeight: 1\nPitch: 0\nSects: 99999\n\
               StartTime: 0\nKEYPOINTCOUNT: 0\n";
        assert!(parse(&sectors).unwrap_err().contains("out of range"));

        // Mirrored sprites and out-of-quad pivots are authored, not damage: the
        // corpus carries 216 negative Width/Height lines and 17 `OrgPt: -0.5`.
        let negative = base.replace("Width: 0.25", "Width: -1");
        let Spec::Sprite(mirrored) = &parse(&negative).unwrap().elements[0].spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(mirrored.width, -1.);

        let anchor = base.replace("GrndNormOnly: 0", "GrndNormOnly: 0\nOrgPt: -0.500000");
        let Spec::Sprite(pivoted) = &parse(&anchor).unwrap().elements[0].spec else {
            panic!("element 100 is a sprite");
        };
        assert_eq!(pivoted.origin[0], -0.5);

        let damaged = base.replace("GrndNormOnly: 0", "GrndNormOnly: 0\nOrgPt: 1.#QNAN");
        assert!(parse(&damaged).unwrap_err().contains("OrgPt"));
    }

    #[test]
    fn tokenizer_keeps_values_with_colons_and_drops_blank_lines() {
        let fields = tokenize("MOXTVersion: 37\n\nName: a:b:c\r\nTexFile: \n");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], ("Name".to_string(), "a:b:c".to_string()));
        assert_eq!(fields[2].1, "");
    }

    /// A grid writes its surface as bare `x y z` rows, one per vertex, between
    /// the colour list and `fGridSize` — no key, no colon. The tokenizer used to
    /// drop every such line, which discarded the geometry of all 216 corpus
    /// grids and left each one to be rebuilt from `fGridSize` as a flat patch.
    #[test]
    fn a_grid_keeps_its_authored_vertices() {
        let source = header37(1)
            + "GFXELEMENTID: 210\nName: g\nSrcBlend: 5\nDestBlend: 2\nwNumber: 2\nhNumber: 2\n\
             -58.0 35.0 9.0\ndwColor: ffffffff\n\
             -34.0 35.0 9.0\ndwColor: ffffffff\n\
             -58.0 0.0 9.0\ndwColor: ffffffff\n\
             -34.0 0.0 9.0\ndwColor: ffffffff\n\
             fGridSize: 0.5\nfZOffset: 0.0\nStartTime: 0\nKEYPOINTCOUNT: 1\n\
             InterpolateMode: 0\nTimeSpan: -1\nPosition: 0, 0, 0\nColor: -1\n\
             Scale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n";
        let fields = tokenize(&source);
        assert_eq!(
            fields.iter().filter(|(key, _)| key == GRID_VERTEX).count(),
            4,
            "every bare row is kept"
        );
        let root = parse(&source).expect("grid");
        let Spec::Grid(grid) = &root.elements[0].spec else {
            panic!("element 210 is a grid");
        };
        assert_eq!(grid.vertices.len(), 4);
        assert_eq!(grid.vertices[0], Vec3::new(-58., 35., 9.));
        assert_eq!(grid.vertices[3], Vec3::new(-34., 0., 9.));

        // A count that disagrees with `wNumber * hNumber` is an error, not a
        // silently flattened patch.
        let short = source.replace("-58.0 0.0 9.0\ndwColor", "dwColor");
        assert!(parse(&short).is_err());
    }

    /// A scale-ramp stop carries uninitialized editor memory in a handful of
    /// files: `封引3` in `gfx/人物/技能/刺客/回光返照_左手.gfx` authors
    /// `Scale: 826299995273231830000000.000000`, which stretches one sprite to
    /// 1.57e24 world units and reads on screen as the whole scene turning white.
    /// The stop is clamped rather than the file rejected, so the effect survives.
    #[test]
    fn a_garbage_scale_ramp_stop_is_clamped() {
        let source = header37(1)
            + "GFXELEMENTID: 100\nName: s\nSrcBlend: 5\nDestBlend: 2\n\
               TexFile: 法阵\\封引3.dds\nWidth: 1.0\nHeight: 1.0\n
               StartTime: 0\nKEYPOINTCOUNT: 1\n\
               InterpolateMode: 0\nTimeSpan: 2000\nPosition: 0, 0, 0\nColor: -1\n\
               Scale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 1\n\
               CtrlType: 111\nStartTime: 0\nEndTime: -1\nScale: 0.1\nCount: 2\n\
               Scale: 1.3\nTimeSpan: 100\n\
               Scale: 826299995273231830000000.000000\nTimeSpan: 100\n";
        let root = parse(&source).expect("sprite with a scale ramp");
        let control = &root.elements[0].keypoints[0].controls[0];
        let ControlData::ScaleRamp { stops, .. } = &control.data else {
            panic!("controller 111 is a scale ramp");
        };
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0], (1.3, 100.));
        assert_eq!(
            stops[1].0, 100.,
            "editor garbage must be clamped to the authored maximum, not to a \
             bound wider than anything the editor ever wrote"
        );
    }

    /// `TimeSpan: -1` means "hold forever". Summing it as a duration produced a
    /// negative cycle, which `element_phase` reads as finished-at-once, so the
    /// element sat on its first frame: 775 elements across 180 roots, including
    /// every waterfall curtain, were frozen that way.
    #[test]
    fn a_negative_timespan_is_forever_not_a_negative_duration() {
        let root = parse(&(header37(1)
            + "GFXELEMENTID: 210\nName: g\nSrcBlend: 5\nDestBlend: 2\nwNumber: 1\nhNumber: 1\ndwColor: ffffffff\n\
             0 0 0\nfGridSize: 1\nStartTime: 0\nKEYPOINTCOUNT: 1\n\
             InterpolateMode: 0\nTimeSpan: -1\nPosition: 0, 0, 0\nColor: -1\n\
             Scale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n"))
        .expect("grid");
        assert_eq!(root.elements[0].keypoints[0].time_span, -1.);
        assert_eq!(
            root.elements[0].duration_ms(),
            None,
            "an open-ended keypoint has no duration"
        );
        assert_eq!(root.duration_ms(), None);
    }

    /// `0x724416` scans `IsUseHSVInterp` into an integer, `0x72441d` tests that
    /// value against itself and `0x724422` stores the `setne` result in
    /// emitter+0xb0, so the client selects the HSV sampler on any nonzero value.
    /// The flags beside it are compared against one and stay strict: only this
    /// key has disassembly behind a wider reading.
    #[test]
    fn the_hsv_flag_is_a_nonzero_test_not_an_equality_test() {
        let fields = tokenize("IsUseHSVInterp: 2\nIsBind: 0\nIsSurface: 2\n");
        let lookup = Lookup::new(&fields);
        assert!(lookup.nonzero_flag("IsUseHSVInterp", false).unwrap());
        assert!(
            !lookup.nonzero_flag("IsBind", true).unwrap(),
            "a present zero is false whatever the default"
        );
        assert!(
            !lookup.nonzero_flag("Absent", false).unwrap(),
            "and an absent key takes its default"
        );
        assert!(
            lookup.flag("IsSurface", false).is_err(),
            "a strict flag rejects 2"
        );
    }

    #[test]
    fn gbk_decode_reports_damage() {
        // `Name: 火把` as the client stores it: GBK bytes 0xbbf0 0xb0d1.
        let (text, damaged) = decode(b"Name: \xbb\xf0\xb0\xd1\n");
        assert_eq!(text, "Name: 火把\n");
        assert!(!damaged);
        let (_, damaged) = decode(&[0xff, 0xff, 0x0a]);
        assert!(damaged);
    }
}
