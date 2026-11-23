//! P3.06 / P3.07: per-placement static-model visibility, the client's regional
//! load/evict lifecycle and the pass gate that decides whether a placement is
//! submitted at all.
//!
//! Native basis (`elementclient.exe`, MD5 `5f02895688fa580506e869156340a664`,
//! PE i386 image base `0x400000`):
//!
//! * `CECSceneBlock::Load` (`0x422390`) builds one block object. Its `+0x10`
//!   extent is copied from the world's `+0x560` float at `0x42209a`
//!   (`0x41b4ae`); `0x424020` is the per-frame class tick.
//! * `0x424020..0x4241dd` walks ten classes. For class `i` it loads when the
//!   eye-to-block distance is <= `0x951158 + 4*i` (near table `0x4241e0`,
//!   index 2 is `0x422f00` = `CECSceneBlock::LoadOrnamentData`) and evicts when
//!   the distance is greater than `0x951158 + 4*i` plus half the block extent
//!   (`0x42402a` multiplies `+0x10` by `0x8d5894` = 0.5). The resident bits
//!   live at block `+0x64` (`0x424060`, `0x4240b4`); the far branch clears them
//!   through the unload jump table at `0x424208`.
//! * Six quality presets overwrite `0x951158..0x95117c` at `0x424248`,
//!   `0x42428e`, `0x424482`, `0x424676`, `0x42486a`, `0x424a5e`. The switch is
//!   `0x424230` and the `Sight+1` argument is formed at `0x40e155..0x40e15a`;
//!   `Sight` is parsed from the `Sight` key at `0x40d151` (`0x94eb80`). The
//!   shipped `configs/angelica.cfg` sets `detail_scene = 1`, so preset 2 is the
//!   viewer default (ornament distance 120.0 at `0x42449c`).
//! * `CECOrnament::Load` (`0x489e70`) reads 40 bytes of EC-BSD record and
//!   stores only the two low bytes of the word at record `+36` into the
//!   ornament's `+0x44` / `+0x45` flags (`0x489f0b`, `0x489f12`). The
//!   constructor clears four flags at `0x489dea..0x489df3`. The upper two
//!   bytes are nonzero legacy padding in the shipped `world.ecbsd`; they are
//!   not flags and must never be interpreted as one.
//! * `0x5b1e20` and `0x5b2110` are consecutive virtual slots at `0x8e37d8` /
//!   `0x8e37dc`. Their gates are `0x5b1eaf` (`+0x44`, reflect) and `0x5b21d0`
//!   (`+0x45`, refract); `0x5b1ea3` / `0x5b21c4` / `0x5b21bd` are the
//!   `WaterEffect == 2` and underwater-camera variants that use `+0x46` /
//!   `+0x47` instead. In the 1.5.2 research source these are
//!   `CECOrnamentMan::RenderForReflect` / `RenderForRefract`; the 1.4.2 gates
//!   are the authority. The main view draws every resident placement.
//!
//! The 1.5.2 source is research only, never a porting target.

use std::collections::HashSet;

/// Native write sites and constants, kept for traceability.
#[allow(dead_code)]
pub mod native {
    /// `CECSceneBlock` per-frame class tick.
    pub const CLASS_TICK: u32 = 0x42_4020;
    /// Base of the ten per-class load distances (`0x951158 + 4 * class`).
    pub const CLASS_DISTANCE_BASE: u32 = 0x95_1158;
    /// Near (load) jump table; entry 2 is `0x422f00`.
    pub const LOAD_JUMP_TABLE: u32 = 0x42_41e0;
    /// Far (evict) jump table.
    pub const EVICT_JUMP_TABLE: u32 = 0x42_4208;
    /// `CECSceneBlock::LoadOrnamentData`.
    pub const LOAD_ORNAMENTS: u32 = 0x42_2f00;
    /// `CECOrnament::Load`, 40-byte EC-BSD record.
    pub const ORNAMENT_LOAD: u32 = 0x48_9e70;
    /// `mov %al,0x44(%esi)` — reflect flag.
    pub const REFLECT_FLAG_WRITE: u32 = 0x48_9f0b;
    /// `mov %ah,0x45(%esi)` — refract flag.
    pub const REFRACT_FLAG_WRITE: u32 = 0x48_9f12;
    /// `cmp %bl,0x44(%ecx)` — reflection-pass gate.
    pub const REFLECT_PASS_GATE: u32 = 0x5b_1eaf;
    /// `cmp %bl,0x45(%ecx)` — refraction-pass gate.
    pub const REFRACT_PASS_GATE: u32 = 0x5b_21d0;
    /// `WaterEffect == 2` reflection variant (`+0x46`).
    pub const WATER_REFLECT_GATE: u32 = 0x5b_1ea3;
    /// `WaterEffect == 2` refraction variant (`+0x46`).
    pub const WATER_REFRACT_GATE: u32 = 0x5b_21c4;
    /// Underwater-camera refraction variant (`+0x47`).
    pub const UNDERWATER_REFRACT_GATE: u32 = 0x5b_21bd;
    /// Four baked-scenery passes that call `0x7a9730`.
    pub const SCENERY_PASS_CALLS: [u32; 4] = [0x42_b677, 0x42_be02, 0x42_c351, 0x42_c683];
    /// Four `ALPHABLENDENABLE(1)` writes, one per scenery pass.
    pub const PASS_BLEND_ON: [u32; 4] = [0x42_b5a5, 0x42_bc17, 0x42_c32e, 0x42_c650];
    /// Four `ALPHATEST(0)` writes, one per scenery pass.
    pub const PASS_ALPHA_TEST_OFF: [u32; 4] = [0x42_b595, 0x42_bc07, 0x42_c31e, 0x42_c660];
    /// Client minimap / full-map birdview path formatters.
    pub const MAP_TILE_OLM: u32 = 0x42_9786;
    pub const MAP_TILE_1024: u32 = 0x42_97d9;
    pub const MAP_TILE_256: u32 = 0x42_97ed;
}

/// A static-model view or pass. `Fog` is the ordinary main view with the
/// client's per-material fog uniforms; `Minimap` / `FullMap` are the viewer's
/// north-up orthographic passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Main,
    Fog,
    Reflection,
    Refraction,
    Minimap,
    FullMap,
}

impl Pass {
    pub const ALL: [Pass; 6] = [
        Pass::Main,
        Pass::Fog,
        Pass::Reflection,
        Pass::Refraction,
        Pass::Minimap,
        Pass::FullMap,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Pass::Main => "main",
            Pass::Fog => "fog",
            Pass::Reflection => "reflection",
            Pass::Refraction => "refraction",
            Pass::Minimap => "minimap",
            Pass::FullMap => "full-map",
        }
    }
    /// Secondary views reuse the one per-frame snapshot: no playback tick, no
    /// second scene-light publication.
    pub fn is_secondary(self) -> bool {
        !matches!(self, Pass::Main | Pass::Fog)
    }
    /// The reflected uniform/bind group is shared by reflection and refraction.
    pub fn uses_reflected_uniform(self) -> bool {
        matches!(self, Pass::Reflection | Pass::Refraction)
    }
}

/// The two EC-BSD per-placement flags. Everything else in the record's `extra`
/// word is padding and is deliberately ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub reflect: bool,
    pub refract: bool,
}

impl Visibility {
    /// `0x489f0b` / `0x489f12`: only the low two bytes of record `+36` reach
    /// the ornament object.
    pub fn from_extra(extra: u32) -> Self {
        Self {
            reflect: extra & 0xff != 0,
            refract: (extra >> 8) & 0xff != 0,
        }
    }
    pub fn hidden(self) -> bool {
        !self.reflect && !self.refract
    }
}

/// Data-driven pass gate. A placement the client hides is never submitted;
/// distance is not used here. The water-quality and underwater variants are
/// named unsupported cases, not guessed booleans.
/// Default-context wrapper. The shipped `configs/angelica.cfg` has
/// `WaterEffect = 0`; [`allows_with_context`] carries the level-2 and
/// underwater variants that need explicit object-state flags we do not author.
pub fn allows(pass: Pass, v: Visibility) -> bool {
    allows_with_context(pass, v, RenderContext::default())
}

/// Context-aware per-placement gate. `+0x44` / `+0x45` are the only flags
/// recovered from EC-BSD. `WaterEffect == 2` (`0x5b1ea3`) and the underwater
/// refraction branch (`0x5b21bd`/`0x5b21c4`) read `+0x46` / `+0x47`, which are
/// runtime object state, not asset bytes. Until those flags are authored the
/// viewer conservatively admits nothing in those unsupported contexts instead
/// of silently guessing from the low bytes.
pub fn allows_with_context(pass: Pass, v: Visibility, ctx: RenderContext) -> bool {
    match pass {
        Pass::Main | Pass::Fog | Pass::Minimap | Pass::FullMap => true,
        Pass::Reflection => {
            if ctx.water_effect == NATIVE_DEFAULT_WATER_EFFECT {
                v.reflect
            } else {
                false
            }
        }
        Pass::Refraction => {
            if ctx.water_effect == NATIVE_DEFAULT_WATER_EFFECT && !ctx.underwater {
                v.refract
            } else {
                false
            }
        }
    }
}

/// `WaterEffect` parsed from the `Video` section at `0x40d16c`; the config
/// parser passes a default of zero (`push $0x0` before `0x76cf50`), so the
/// shipped default is zero, not the water-quality level 2.
pub const NATIVE_DEFAULT_WATER_EFFECT: u8 = 0;

/// Classes owned by `CECOrnamentMan`. `0x5b1f32..0x5b20fd` (reflection) and
/// `0x5b2254..0x5b2407` (refraction) skip home ornaments and EC models unless
/// `WaterEffect == 2`; the shipped default therefore hides those two classes
/// in both secondary views. Baked scenery is still gated per placement by
/// [`allows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelClass {
    BakedScenery,
    HomeOrnament,
    EcModel,
}

/// Default-context wrapper for the shipped `WaterEffect == 0` path.
pub fn allows_class(pass: Pass, class: ModelClass) -> bool {
    allows_class_with_context(pass, class, RenderContext::default())
}

/// Native class admission for secondary views. At level 2 home ornaments enter
/// reflection/refraction; EC models enter reflection at level 2 and refraction
/// only when the camera is underwater at level 2 (`0x5b2022`, `0x5b233f`).
/// The per-placement `+0x46`/`+0x47` flags remain a named unsupported gap.
pub fn allows_class_with_context(pass: Pass, class: ModelClass, ctx: RenderContext) -> bool {
    match class {
        ModelClass::BakedScenery => true,
        ModelClass::HomeOrnament | ModelClass::EcModel => {
            if !matches!(pass, Pass::Reflection | Pass::Refraction) {
                return true;
            }
            if ctx.water_effect != 2 {
                return false;
            }
            if class == ModelClass::EcModel && pass == Pass::Refraction && !ctx.underwater {
                return false;
            }
            true
        }
    }
}

/// The ten classes walked by `CECSceneBlock` at `0x424020`, in native table
/// order. `PRESETS[q][class.index()]` is that class's load distance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Trees,
    Water,
    Ornaments,
    BoxAreas,
    GrassAreas,
    Effects,
    EcModels,
    Critters,
    Beziers,
    Sounds,
}

impl Class {
    pub const ALL: [Class; 10] = [
        Class::Trees,
        Class::Water,
        Class::Ornaments,
        Class::BoxAreas,
        Class::GrassAreas,
        Class::Effects,
        Class::EcModels,
        Class::Critters,
        Class::Beziers,
        Class::Sounds,
    ];
    pub fn index(self) -> usize {
        match self {
            Class::Trees => 0,
            Class::Water => 1,
            Class::Ornaments => 2,
            Class::BoxAreas => 3,
            Class::GrassAreas => 4,
            Class::Effects => 5,
            Class::EcModels => 6,
            Class::Critters => 7,
            Class::Beziers => 8,
            Class::Sounds => 9,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Class::Trees => "trees",
            Class::Water => "water",
            Class::Ornaments => "ornaments",
            Class::BoxAreas => "box-areas",
            Class::GrassAreas => "grass-areas",
            Class::Effects => "effects",
            Class::EcModels => "ec-models",
            Class::Critters => "critters",
            Class::Beziers => "beziers",
            Class::Sounds => "sounds",
        }
    }
}

/// Recovered video/camera state for the secondary-view gates. The shipped
/// default is `WaterEffect == 0`, not the level-2 variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderContext {
    pub water_effect: u8,
    pub underwater: bool,
}

impl Default for RenderContext {
    fn default() -> Self {
        Self {
            water_effect: NATIVE_DEFAULT_WATER_EFFECT,
            underwater: false,
        }
    }
}

impl RenderContext {
    pub fn new(water_effect: u8, underwater: bool) -> Self {
        Self {
            water_effect,
            underwater,
        }
    }
}

/// Native detail reduction: preset `quality` relative to the widest preset 5.
pub fn lod_scale(quality: u8, class: Class) -> f32 {
    let q = (quality as usize).min(5);
    PRESETS[q][class.index()] / PRESETS[5][class.index()]
}

/// Index of the ornament class in `0x424020`'s ten-class walk. Near table
/// `0x4241e0` entry 2 calls `0x422f00` (`CECSceneBlock::LoadOrnamentData`).
pub const ORNAMENT_CLASS: usize = 2;

/// The six native quality presets, in world units. Read from the ten `movl`
/// immediates at `0x424248`, `0x42428e`, `0x424482`, `0x424676`, `0x42486a`,
/// `0x424a5e`; preset 0 is the `rep stos` default of 1000.0 at `0x424248`.
pub const PRESETS: [[f32; 10]; 6] = [
    [
        1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0,
    ],
    [
        140.0, 600.0, 80.0, 300.0, 40.0, 60.0, 60.0, 60.0, 300.0, 30.0,
    ],
    [
        160.0, 600.0, 120.0, 300.0, 80.0, 120.0, 120.0, 120.0, 300.0, 30.0,
    ],
    [
        200.0, 600.0, 140.0, 300.0, 40.0, 150.0, 150.0, 150.0, 300.0, 60.0,
    ],
    [
        300.0, 600.0, 180.0, 300.0, 40.0, 220.0, 220.0, 220.0, 300.0, 60.0,
    ],
    [
        400.0, 1000.0, 260.0, 500.0, 55.0, 260.0, 260.0, 260.0, 600.0, 80.0,
    ],
];

pub fn ornament_distance(quality: u8) -> f32 {
    PRESETS[(quality as usize).min(5)][ORNAMENT_CLASS]
}

/// One camera-independent placement. `index` is the index into the scene's
/// ornament array so the caller can hand the selected set to `Objects::load_ids`.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub index: usize,
    pub center: [f32; 2],
    pub visibility: Visibility,
    pub class: Class,
}

/// Centre of the ECWLD block that owns an ornament position. `world.ecwld`
/// header: width 8192, length 11264, block size 64, 176 rows x 128 columns.
/// The native block object stores this centre and its `+0x10` extent.
pub fn block_center(width: f32, length: f32, block_size: f32, position: [f32; 3]) -> [f32; 2] {
    let col = ((position[0] + width * 0.5) / block_size).floor();
    let row = ((length * 0.5 - position[2]) / block_size).floor();
    [
        (col + 0.5) * block_size - width * 0.5,
        length * 0.5 - (row + 0.5) * block_size,
    ]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The client's regional lifecycle, reproduced with the `+0x64` resident bits
/// and the `threshold + half-extent` hysteresis at `0x424020`. Loads, evictions
/// and distance-culls are counted so a test can prove that eviction happened.
#[derive(Clone, Debug, Default)]
pub struct RegionResidency {
    resident: HashSet<usize>,
    pub loads: usize,
    pub evictions: usize,
    pub distance_culls: usize,
}

impl RegionResidency {
    /// Select placements per native class for this eye.
    /// `margin` is the native `+0x10 * 0.5` half-extent (32.0 for a 64-unit
    /// block). The returned indices are passed to `Objects::load_ids`, whose
    /// model/cache retain is the production eviction.
    pub fn select(
        &mut self,
        placements: &[Placement],
        eye: [f32; 2],
        quality: u8,
        margin: f32,
    ) -> Vec<usize> {
        let q = (quality as usize).min(5);
        let mut selected = Vec::with_capacity(self.resident.len());
        for p in placements {
            // 0x424020 uses `0x951158 + 4*class` for each of the ten classes.
            let threshold = PRESETS[q][p.class.index()];
            let dx = eye[0] - p.center[0];
            let dz = eye[1] - p.center[1];
            let d_sq = dx * dx + dz * dz;
            let was = self.resident.contains(&p.index);
            let mut is_resident = was;
            if !was && d_sq <= threshold * threshold {
                self.resident.insert(p.index);
                self.loads += 1;
                is_resident = true;
            } else if was && d_sq > (threshold + margin) * (threshold + margin) {
                self.resident.remove(&p.index);
                self.evictions += 1;
                is_resident = false;
            } else if !was {
                self.distance_culls += 1;
            }
            if is_resident {
                selected.push(p.index);
            }
        }
        selected.sort_unstable();
        selected
    }
    pub fn resident(&self, index: usize) -> bool {
        self.resident.contains(&index)
    }
    pub fn resident_len(&self) -> usize {
        self.resident.len()
    }
    pub fn reset(&mut self) {
        self.resident.clear();
        self.loads = 0;
        self.evictions = 0;
        self.distance_culls = 0;
    }
}

/// The single per-frame snapshot. `render()` samples wall time, ticks the glow,
/// wind, celestial state and model playback once, then every secondary view
/// (reflection, refraction, minimap, full map) reuses `elapsed` and a zero
/// logic delta. The counters make an accidental extra tick visible in a test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameSnapshot {
    pub elapsed: f32,
    pub logic_delta_ms: u32,
    pub time_advances: u32,
    pub light_publications: u32,
}

impl FrameSnapshot {
    pub fn new(elapsed: f32, logic_delta_ms: u32) -> Self {
        Self {
            elapsed,
            logic_delta_ms,
            time_advances: 1,
            light_publications: 1,
        }
    }
    pub fn secondary(&self, pass: Pass) -> SecondaryView {
        SecondaryView {
            pass,
            elapsed: self.elapsed,
            logic_delta_ms: 0,
            advances_time: false,
            consumes_light_publications: false,
        }
    }
    pub fn idle() -> Self {
        Self {
            elapsed: 0.,
            logic_delta_ms: 0,
            time_advances: 0,
            light_publications: 0,
        }
    }
}

/// One secondary view of an already-computed frame. The renderer samples time
/// and publishes scene lights exactly once; every secondary view carries the
/// same `elapsed` and a zero logic delta.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecondaryView {
    pub pass: Pass,
    pub elapsed: f32,
    pub logic_delta_ms: u32,
    pub advances_time: bool,
    pub consumes_light_publications: bool,
}

pub fn secondary_view(pass: Pass, elapsed: f32) -> SecondaryView {
    SecondaryView {
        pass,
        elapsed,
        logic_delta_ms: 0,
        advances_time: false,
        consumes_light_publications: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecbsd_flags_are_the_low_two_bytes_only() {
        // world.ecbsd records read in this session. The upper two bytes are
        // nonzero legacy padding (0x4400, 0xc27f, ...) and are not flags.
        assert_eq!(
            Visibility::from_extra(0x0000_0000),
            Visibility {
                reflect: false,
                refract: false
            }
        );
        assert_eq!(
            Visibility::from_extra(0x4400_0000),
            Visibility {
                reflect: false,
                refract: false
            }
        );
        assert_eq!(
            Visibility::from_extra(0x0000_0001),
            Visibility {
                reflect: true,
                refract: false
            }
        );
        assert_eq!(
            Visibility::from_extra(0xc27f_0100),
            Visibility {
                reflect: false,
                refract: true
            }
        );
        assert_eq!(
            Visibility::from_extra(0xc27f_0101),
            Visibility {
                reflect: true,
                refract: true
            }
        );
    }

    #[test]
    fn hidden_placement_is_not_drawn_in_reflect_or_refract() {
        let hidden = Visibility {
            reflect: false,
            refract: false,
        };
        assert!(hidden.hidden());
        assert!(!allows(Pass::Reflection, hidden));
        assert!(!allows(Pass::Refraction, hidden));
        assert!(allows(Pass::Main, hidden));
        assert!(allows(Pass::Fog, hidden));
        assert!(allows(Pass::Minimap, hidden));
        assert!(allows(Pass::FullMap, hidden));
        let reflect_only = Visibility {
            reflect: true,
            refract: false,
        };
        assert!(allows(Pass::Reflection, reflect_only));
        assert!(!allows(Pass::Refraction, reflect_only));
        let refract_only = Visibility {
            reflect: false,
            refract: true,
        };
        assert!(!allows(Pass::Reflection, refract_only));
        assert!(allows(Pass::Refraction, refract_only));
    }

    #[test]
    fn distance_selection_loads_and_evicts_with_hysteresis() {
        let placements = [
            Placement {
                index: 0,
                center: [0.0, 0.0],
                visibility: Visibility {
                    reflect: false,
                    refract: false,
                },
                class: Class::Ornaments,
            },
            Placement {
                index: 1,
                center: [1000.0, 0.0],
                visibility: Visibility {
                    reflect: false,
                    refract: false,
                },
                class: Class::Ornaments,
            },
        ];
        let mut r = RegionResidency::default();
        // Preset 2: ornament threshold 120.0, half-extent 32.0.
        assert_eq!(ornament_distance(2), 120.0);
        assert_eq!(r.select(&placements, [0.0, 0.0], 2, 32.0), vec![0]);
        assert_eq!(r.loads, 1);
        assert_eq!(r.distance_culls, 1, "the far placement is distance-culled");
        assert!(!r.resident(1));
        // Inside the hysteresis band (130) it stays resident, as the native
        // `+0x64` bit does until `threshold + half-extent` is exceeded.
        assert_eq!(r.select(&placements, [130.0, 0.0], 2, 32.0), vec![0]);
        assert_eq!(r.evictions, 0);
        // Beyond 152 the bit is cleared: eviction is exercised.
        assert!(r.select(&placements, [200.0, 0.0], 2, 32.0).is_empty());
        assert_eq!(r.evictions, 1);
        assert_eq!(r.resident_len(), 0);
        // Re-entering loads again through the same near-table rule.
        assert_eq!(r.select(&placements, [0.0, 0.0], 2, 32.0), vec![0]);
        assert_eq!(r.loads, 2);
    }

    #[test]
    fn hidden_model_classes_are_not_drawn_in_secondary_views() {
        // Native default WaterEffect is 0 (0x40d16c), so home ornaments and EC
        // models are hidden in reflection/refraction (0x5b2015, 0x5b2336).
        assert_eq!(NATIVE_DEFAULT_WATER_EFFECT, 0);
        for pass in [Pass::Reflection, Pass::Refraction] {
            assert!(allows_class(pass, ModelClass::BakedScenery));
            assert!(!allows_class(pass, ModelClass::HomeOrnament));
            assert!(!allows_class(pass, ModelClass::EcModel));
        }
        for pass in [Pass::Main, Pass::Fog, Pass::Minimap, Pass::FullMap] {
            assert!(allows_class(pass, ModelClass::BakedScenery));
            assert!(allows_class(pass, ModelClass::HomeOrnament));
            assert!(allows_class(pass, ModelClass::EcModel));
        }
    }

    #[test]
    fn secondary_views_never_advance_time_or_republish_lights() {
        for pass in Pass::ALL {
            if !pass.is_secondary() {
                continue;
            }
            let view = secondary_view(pass, 12.5);
            assert_eq!(view.elapsed, 12.5);
            assert_eq!(view.logic_delta_ms, 0);
            assert!(!view.advances_time);
            assert!(!view.consumes_light_publications);
        }
        assert!(Pass::Reflection.uses_reflected_uniform());
        assert!(Pass::Refraction.uses_reflected_uniform());
        assert!(!Pass::Minimap.uses_reflected_uniform());
    }

    #[test]
    fn one_frame_snapshot_serves_every_secondary_view() {
        let frame = FrameSnapshot::new(42.0, 16);
        assert_eq!(frame.time_advances, 1);
        assert_eq!(frame.light_publications, 1);
        for pass in Pass::ALL {
            let view = frame.secondary(pass);
            assert_eq!(view.elapsed, 42.0);
            assert_eq!(view.logic_delta_ms, 0);
            assert!(!view.advances_time);
            assert!(!view.consumes_light_publications);
        }
        // The snapshot is immutable: deriving views did not tick or publish.
        assert_eq!(frame, FrameSnapshot::new(42.0, 16));
        assert_eq!(FrameSnapshot::idle().time_advances, 0);
    }

    #[test]
    fn real_ecbsd_placements_have_the_expected_flags() {
        // Records read from Games/pw142/element/maps/world/world.ecbsd.
        // 0x2742 export 500: no flags (hidden in both secondary views).
        // 0x283ce export 3822: reflect only.
        // 0x5c2b8 export 8907: refract only.
        // 0x5d3aa export 9020: both.
        let cases = [
            (0x0000_0000u32, false, false),
            (0x0000_0001, true, false),
            (0xc27f_0100, false, true),
            (0xc27f_0101, true, true),
        ];
        for (extra, reflect, refract) in cases {
            let v = Visibility::from_extra(extra);
            assert_eq!(v.reflect, reflect, "{extra:#x}");
            assert_eq!(v.refract, refract, "{extra:#x}");
            for pass in [Pass::Reflection, Pass::Refraction] {
                let expected = if pass == Pass::Reflection {
                    reflect
                } else {
                    refract
                };
                assert_eq!(allows(pass, v), expected, "{extra:#x} {pass:?}");
            }
        }
    }

    #[test]
    fn native_preset_table_matches_the_disassembly() {
        // 0x42449c / 0x424690 / 0x424884 / 0x424a78 / 0x42424e.
        assert_eq!(PRESETS[0][ORNAMENT_CLASS], 1000.0);
        assert_eq!(PRESETS[1][ORNAMENT_CLASS], 80.0);
        assert_eq!(PRESETS[2][ORNAMENT_CLASS], 120.0);
        assert_eq!(PRESETS[3][ORNAMENT_CLASS], 140.0);
        assert_eq!(PRESETS[4][ORNAMENT_CLASS], 180.0);
        assert_eq!(PRESETS[5][ORNAMENT_CLASS], 260.0);
        assert_eq!(
            native::CLASS_DISTANCE_BASE + 4 * ORNAMENT_CLASS as u32,
            0x951160
        );
    }
    #[test]
    fn ten_classes_and_per_class_thresholds() {
        assert_eq!(Class::ALL.len(), 10);
        for (i, c) in Class::ALL.iter().enumerate() {
            assert_eq!(c.index(), i, "{}", c.name());
        }
        assert_eq!(lod_scale(5, Class::Trees), 1.0);
        assert_eq!(lod_scale(2, Class::Trees), 160.0 / 400.0);
        assert_eq!(lod_scale(2, Class::Ornaments), 120.0 / 260.0);
        let hidden = Visibility {
            reflect: false,
            refract: false,
        };
        let ps = [
            Placement {
                index: 0,
                center: [130.0, 0.0],
                visibility: hidden,
                class: Class::Trees,
            },
            Placement {
                index: 1,
                center: [130.0, 0.0],
                visibility: hidden,
                class: Class::Ornaments,
            },
        ];
        let mut r = RegionResidency::default();
        // Preset 2: trees load at 160, ornaments at 120.
        assert_eq!(r.select(&ps, [0.0, 0.0], 2, 32.0), vec![0]);
        assert_eq!(r.loads, 1);
        assert_eq!(r.distance_culls, 1);
    }

    #[test]
    fn render_context_gates_the_recovered_secondary_view_rules() {
        let default = RenderContext::default();
        assert_eq!(default.water_effect, NATIVE_DEFAULT_WATER_EFFECT);
        assert!(!default.underwater);
        assert!(!allows_class(Pass::Reflection, ModelClass::HomeOrnament));
        assert!(!allows_class(Pass::Refraction, ModelClass::EcModel));
        let level2 = RenderContext::new(2, true);
        assert!(allows_class_with_context(
            Pass::Reflection,
            ModelClass::HomeOrnament,
            level2
        ));
        assert!(allows_class_with_context(
            Pass::Reflection,
            ModelClass::EcModel,
            level2
        ));
        assert!(allows_class_with_context(
            Pass::Refraction,
            ModelClass::EcModel,
            level2
        ));
        let level2_dry = RenderContext::new(2, false);
        assert!(!allows_class_with_context(
            Pass::Refraction,
            ModelClass::EcModel,
            level2_dry
        ));
        let visible = Visibility {
            reflect: true,
            refract: true,
        };
        assert!(allows(Pass::Reflection, visible));
        assert!(allows(Pass::Refraction, visible));
        // +0x46 / +0x47 are unauthored, so non-default contexts fail closed.
        assert!(!allows_with_context(Pass::Reflection, visible, level2));
        assert!(!allows_with_context(Pass::Refraction, visible, level2));
    }
}
