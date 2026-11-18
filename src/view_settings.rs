//! Persisted viewer controls; malformed values fall back safely.
use crate::client_weather::WeatherOverride;
use crate::sky_transition::SkyOverride;
use crate::{FrameGenMethod, FrameMultiplier, LatencyMode, UpscaleMethod, UpscaleQuality};

#[path = "post_plan.rs"]
mod post_plan;
/// The renderer must allocate/execute the post chain for each independent
/// scene effect, not only when bloom happens to be enabled. The retained
/// screen-space fog is a diagnostic route and is deliberately not an input:
/// production shading is fogged per material (`fog_matrix`).
pub fn post_required(bloom: bool, sharpen: bool, underwater: bool, gamma: bool) -> bool {
    post_plan::required(post_plan::Inputs {
        bloom,
        sharpen,
        underwater,
        screen_fog: false,
        fog_enabled: false,
        display_gamma: gamma,
        srgb_target: true,
    })
}

/// Which artwork a map surface shows.
///
/// `Realistic` renders the world from above with the live scene. `Original`
/// shows the client's own map textures, read from `surfaces.pck`:
/// `surfaces/maps/` for the world map and `surfaces/minimaps/<zone>/` for the
/// minimap (`Surfaces\\MiniMaps\\%02d%02d`, `0x565220`). The two surfaces select
/// independently because the client draws them from different asset sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MapStyle {
    #[default]
    Realistic,
    Original,
}
impl MapStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Realistic => "Realistic",
            Self::Original => "Original",
        }
    }
    /// Persisted spelling; anything unrecognised keeps the default rather than
    /// silently becoming a different style.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "Realistic" => Some(Self::Realistic),
            "Original" => Some(Self::Original),
            _ => None,
        }
    }
}

/// Minimap HUD outline. The client's own minimap is a square tile crop with a
/// round frame; both are offered because the square keeps the full tile view.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MinimapShape {
    #[default]
    Square,
    Circular,
}
impl MinimapShape {
    pub fn label(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Circular => "Circular",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        match text {
            "Square" => Some(Self::Square),
            "Circular" => Some(Self::Circular),
            _ => None,
        }
    }
}

pub use crate::gfx_player::EffectShaft;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DetailSettings {
    /// Terrain block window half-size in blocks (`world+0x1bc / block size`,
    /// `0x77d720`). The client's range is 420 world units for the shipped
    /// quality presets (`0x40e41f`), i.e. 7 blocks of 64 units.
    pub radius: isize,
    /// Restore the finite native terrain window instead of the viewer far mesh.
    pub terrain_cutoff: bool,
    pub tree_lod_scale: f32,
    /// Native `CECSceneBlock` quality preset (`0x424230`, `Sight+1` at
    /// `0x40e155`). Preset 2 is the shipped `detail_scene = 1` default and
    /// loads the ornament class within 120 world units (`0x42449c`).
    pub scenery_quality: u8,
}
impl Default for DetailSettings {
    fn default() -> Self {
        Self {
            radius: 7,
            terrain_cutoff: false,
            tree_lod_scale: 1.0,
            scenery_quality: 2,
        }
    }
}
#[derive(Clone, PartialEq, Debug)]
pub struct ViewSettings {
    pub detail: DetailSettings,
    pub camera_speed: f32,
    pub boost: f32,
    pub sensitivity: f32,
    pub far: f32,
    pub time_of_day: f32,
    /// Advance `time_of_day` with wall-clock time. Off by default so the slider
    /// stays where it is put; the client's own clock always runs.
    pub time_pass_enabled: bool,
    /// Multiplier on the client's own rate, where 1.0 is one authored day per
    /// four real hours (`sky_time::CLOCK_MS_PER_DAY`). 1.0 reproduces the
    /// shipped speed; larger values fast-forward for inspection.
    pub time_pass_speed: f32,
    /// Keep the whole terrain grid and every decoded scenery texture resident
    /// instead of paging them through disk and the bounded RAM caches. This is
    /// the pre-#22 VRAM/RAM-first behaviour reintroduced as an option: it is
    /// dramatically faster at load and while flying, and it trades memory for
    /// that speed. Applied at startup.
    pub all_to_vram: bool,
    pub whole_world: bool,
    pub water_reflections: bool,
    /// Draw the world's own animated effects: the roots `world.ecwld` and the
    /// model events place in the map — waterfalls, torch fire, glows. On by
    /// default. Off releases the players behind them and keeps their lights, so a
    /// torch whose flame is switched off still lights the wall.
    pub world_effects: bool,
    /// Which rule builds the shaft of a `RotFromView` card. `Client` is the
    /// faithful one; the others are the comparison the field reports asked for.
    pub effect_shaft: EffectShaft,
    /// Radius of the client's effect distance fade, in world units.
    ///
    /// `SCNRES_EFFECT` is 60 in the client's near scheme and 120 in its far one
    /// (`EC_SceneBlock.cpp:1109`, `:1133`); a `bAttenuation` root outside it
    /// fades to nothing over three seconds (`EC_ManOrnament.cpp:279`,
    /// `FadeGFX`). `0` is the explorer's own default and draws every root at its
    /// authored alpha, because a camera that flies over a whole map is not the
    /// player the client measures from.
    pub effect_distance: f32,
    pub atmospheric_fog: bool,
    pub bloom: bool,
    pub sharpen: bool,
    pub bloom_strength: f32,
    pub bloom_mode: u8,
    pub bloom_tint: u32,
    pub bloom_brightness_fade: f32,
    pub bloom_opacity_fade: f32,
    pub client_gamma: bool,
    pub gamma_setting: f32,
    pub terrain_lightmaps: bool,
    pub model_backface_culling: bool,
    pub music_enabled: bool,
    pub music_volume: f32,
    pub ambience_enabled: bool,
    pub sound_volume: f32,
    pub effects_enabled: bool,
    pub effects_volume: f32,
    pub fullscreen: bool,
    pub show_zone_entry: bool,
    pub show_minimap: bool,
    pub minimap_range: f32,
    pub tree_wind_strength: f32,
    pub client_tree_wind_preview: bool,
    /// Native wind has no user scale: `+0x6c/+0x70/+0x74` are authored data
    /// (`0x650b40`) and nothing else writes them. This flag is the on/off gate
    /// the shader tests; `grass_wind_amplitude` is the viewer-only multiplier.
    pub grass_wind_enabled: bool,
    /// Viewer-only multiplier on the published wind history, so the sway can
    /// be dialled down without claiming a retail value that does not exist.
    /// 1.0 is the recovered amplitude; see the grass_wind module docs.
    pub grass_wind_amplitude: f32,
    pub regional_tree_lighting: bool,
    pub streamed_regions: bool,
    pub grass_distance_fade: bool,
    pub area_grass_lightmaps: bool,
    /// Opt-in P4.07 authored vegetation mip experiment. Default false keeps the
    /// approved generated-mip world path (P4.08). Applied at startup; restart to change.
    pub vegetation_authored_mips: bool,
    pub celestial_discs: bool,
    /// Ignore the area's own sun/moon bits and always draw both discs.
    ///
    /// `0x420992` extracts the sun from bit 3 and the moon from bit 4 of the
    /// selected area's flag word (`world.ecbsd` record `+4`), so a zone authored
    /// without bit 3 really has no sun disc. This switch is for telling that
    /// apart from a viewer defect: turn it on in a zone where the sun vanishes.
    pub ignore_celestial_flags: bool,
    /// How strongly the sun and moon discs are drawn. 1.0 is the texture
    /// itself; the client packs the disc diffuse as half the object colour
    /// (`0x4b405f`, 127/255), which read as dim grey circles, so the recovered
    /// value is offered on the slider but is not the default.
    pub celestial_brightness: f32,
    /// Client `CloudDetail` gate for the native `.cld` surface.
    pub cloud_detail: bool,
    /// World map artwork (the full-world page).
    pub world_map_style: MapStyle,
    /// Minimap artwork, chosen separately from the world map.
    pub minimap_style: MapStyle,
    /// Minimap HUD outline.
    pub minimap_shape: MinimapShape,
    /// Internal 3D render resolution as a percentage of the window size.
    /// 100 is native; the Graphics tab exposes 10 (fast/blurry) to 200
    /// (supersampled). UI and presentation always stay at native resolution.
    /// Applies only while `upscaling` is `off`.
    pub resolution_scale: f32,
    /// Graphics: temporal upscaler. `off` keeps the resolution-scale slider;
    /// any provider renders the world at its recommended internal resolution.
    pub upscaling: UpscaleMethod,
    /// Graphics: quality mode for the active upscaler.
    pub upscale_quality: UpscaleQuality,
    /// Graphics: frame generator, chosen independently of the upscaler.
    /// Illegal pairings are refused at decode time (see `crate::pairing`).
    pub frame_generation: FrameGenMethod,
    /// Graphics: presented frames per rendered frame for the frame generator
    /// (`x2` … `x6`). What each vendor can actually do is decided by
    /// `crate::multiplier_support`; FSR frame generation is 2x only and DLSS
    /// multi frame generation needs Blackwell.
    pub frame_multiplier: FrameMultiplier,
    /// Graphics: AMD FSR 1 RCAS sharpness in stops (lower = sharper).
    /// Applies only while `upscaling` is `fsr1`.
    pub fsr1_sharpness: f32,
    /// Graphics: sharpening strength handed to the FSR 3.1 / FSR 4 FFX
    /// upscale dispatch (0 = off, 1 = maximum; FFX calls it `sharpness` and
    /// only applies it while `enableSharpening` is set). Applies while
    /// `upscaling` is `fsr2`, `fsr3` or `fsr4` — all three go through the same
    /// FFX upscale dispatch.
    pub fsr3_sharpness: f32,
    /// Graphics: sharpening strength handed to NGX as `Sharpness`
    /// (`NVSDK_NGX_Parameter_Sharpness`, 0 = off, 1 = maximum). DLSS does no
    /// sharpening of its own when this is 0, which is why the reconstruction
    /// looks soft next to an RCAS-sharpened FSR frame. Applies while
    /// `upscaling` is `dlss`.
    pub dlss_sharpness: f32,
    /// Graphics: sharpening applied by the viewer's own RCAS-style pass after
    /// XeSS (0 = off, 1 = maximum). Intel's SDK 3.0.2 has no sharpening
    /// control — it exposes `xessExecute` and nothing else — so the only way to
    /// give XeSS the same control the other two providers have is to sharpen
    /// its output ourselves. Applies while `upscaling` is `xess`.
    pub xess_sharpness: f32,
    /// Graphics: pacing strategy — `vsync` is the previous behaviour (the
    /// display paces presents and no latency library is involved), `xell`
    /// hands the decision to Intel Xe Low Latency (`xellSleep` plus the
    /// per-frame simulation / render-submit / present markers) while keeping a
    /// vsync'd, sync-interval-1 present. Independent of the upscaler and of
    /// frame generation; XeSS Frame Generation owns its own XeLL context, in
    /// which case that context is the one that runs (see
    /// `crate::latency_owner`).
    pub latency: LatencyMode,
    /// Graphics: folder to load the vendor upscaling SDKs from. Empty means
    /// `PW_UPSCALER_SDK_DIR`, or `<exe>\sdk\` beside the viewer. The SDKs are
    /// not shipped with the viewer and are never committed to the repository.
    pub upscale_sdk_dir: String,
    /// World tab: force one precipitation effect instead of the area flags.
    pub weather_override: WeatherOverride,
    /// World tab: pin one `sky.lst` row instead of the day/night ladder.
    pub sky_override: SkyOverride,
    /// Which live-logging channels are ticked. Bit `n` is
    /// `crate::diagnostics::Channel::ALL[n]`, so the whole Logging tab survives a
    /// restart without a key per channel.
    pub logging_mask: u64,
    /// Where the log files go; empty means `<client root>/pwe-logs`.
    pub logging_dir: String,
    /// Map folder selected in Escape → Teleport List → Change world. A missing,
    /// unsafe or unknown ID falls back to the main world at startup.
    pub selected_world: String,
}
impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            detail: DetailSettings::default(),
            camera_speed: 240.,
            boost: 4.,
            sensitivity: 0.0025,
            far: 16000.,
            time_of_day: 12.,
            time_pass_enabled: false,
            time_pass_speed: 1.,
            all_to_vram: false,
            whole_world: false,
            water_reflections: true,
            world_effects: true,
            effect_shaft: EffectShaft::Client,
            effect_distance: 0.,
            atmospheric_fog: false,
            bloom: false,
            sharpen: false,
            bloom_strength: 1.,
            bloom_mode: 0,
            bloom_tint: 0xffffffff,
            bloom_brightness_fade: 0.,
            bloom_opacity_fade: 0.,
            client_gamma: false,
            gamma_setting: 41.,
            terrain_lightmaps: true,
            model_backface_culling: true,
            music_enabled: true,
            music_volume: 0.5,
            ambience_enabled: true,
            sound_volume: 0.5,
            effects_enabled: true,
            effects_volume: 0.5,
            fullscreen: false,
            show_zone_entry: true,
            show_minimap: true,
            minimap_range: 256.,
            tree_wind_strength: 0.2,
            client_tree_wind_preview: false,
            grass_wind_enabled: true,
            grass_wind_amplitude: 1.,
            regional_tree_lighting: false,
            streamed_regions: false,
            grass_distance_fade: true,
            area_grass_lightmaps: true,
            vegetation_authored_mips: false,
            // The client sun and moon are the only discs the shipped
            // `Sky\clear` set does not paint, so they are on by default.
            celestial_discs: true,
            celestial_brightness: 1.,
            // Courage Path and similar outdoor zones ship with a flag word
            // that has no sun bit (0x420992 reads `+0x80` bit 3), so the native
            // client would hide the sun in the open. The viewer now ignores
            // those flag words by default so outdoor zones keep their sun;
            // the checkbox still lets a tester see the native gating.
            ignore_celestial_flags: true,
            cloud_detail: true,
            world_map_style: MapStyle::Realistic,
            minimap_style: MapStyle::Realistic,
            minimap_shape: MinimapShape::Square,
            resolution_scale: 100.,
            upscaling: UpscaleMethod::Off,
            upscale_quality: UpscaleQuality::Quality,
            frame_generation: FrameGenMethod::Off,
            frame_multiplier: FrameMultiplier::X2,
            fsr1_sharpness: 0.2,
            fsr3_sharpness: 0.75,
            // DLSS and XeSS default to a mild sharpen: both reconstruct softer
            // than an RCAS-sharpened FSR frame, and 0 (no sharpening at all) is
            // what made DLSS look blurry next to it.
            dlss_sharpness: 0.35,
            xess_sharpness: 0.35,
            upscale_sdk_dir: String::new(),
            latency: LatencyMode::Vsync,
            // Both overrides default to the client's own behaviour; the World
            // tab offers them only so the recovered sets and particle
            // constants can be inspected anywhere.
            weather_override: WeatherOverride::Auto,
            sky_override: SkyOverride::Auto,
            // Off by default: logging is a tool the user reaches for, not a cost
            // every session pays.
            logging_mask: 0,
            logging_dir: String::new(),
            selected_world: String::from("world"),
        }
    }
}

impl ViewSettings {
    fn decode(text: &str) -> Self {
        Self::decode_with_fog_default(text, false)
    }
    fn decode_with_fog_default(text: &str, fog_enabled: bool) -> Self {
        let mut s = Self::default();
        s.atmospheric_fog = fog_enabled;
        // Legacy `upscaling=fsr` (pre-FSR-version-selector settings) carries
        // the chosen provider in `fsr_version`; either key may appear before
        // the other in the file, so resolve the pair after the key loop.
        let mut legacy_fsr_bare = false;
        let mut legacy_fsr_version: Option<u32> = None;
        for line in text.lines().take(100) {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            let v = v.trim();
            macro_rules! number {
                ($field:expr,$min:expr,$max:expr) => {
                    if let Ok(x) = v.parse::<f32>() {
                        if x.is_finite() {
                            $field = x.clamp($min, $max)
                        }
                    }
                };
            }
            macro_rules! flag {
                ($field:expr) => {
                    if let Ok(x) = v.parse::<bool>() {
                        $field = x
                    }
                };
            }
            match k {
                "sharpen" => flag!(s.sharpen),
                "terrain_cutoff" => flag!(s.detail.terrain_cutoff),
                "radius" => {
                    if let Ok(x) = v.parse::<isize>() {
                        s.detail.radius = x.clamp(2, 12)
                    }
                }
                "tree_lod_scale" => number!(s.detail.tree_lod_scale, 0.25, 4.),
                "scenery_quality" => {
                    if let Ok(x) = v.parse::<u8>() {
                        s.detail.scenery_quality = x.min(5)
                    }
                }
                "camera_speed" => number!(s.camera_speed, 10., 3000.),
                "boost" => number!(s.boost, 1., 10.),
                "sensitivity" => number!(s.sensitivity, 0.0005, 0.01),
                "far" => number!(s.far, 2000., 24000.),
                "time_of_day" => number!(s.time_of_day, 0., 24.),
                "time_pass_enabled" => flag!(s.time_pass_enabled),
                "time_pass_speed" => number!(s.time_pass_speed, 0.1, 600.),
                "all_to_vram" => flag!(s.all_to_vram),
                "grass_wind_enabled" => flag!(s.grass_wind_enabled),
                "grass_wind_amplitude" => number!(s.grass_wind_amplitude, 0., 2.),
                // Older files stored the removed strength slider: any value
                // above the old gate keeps the wind on.
                "grass_wind_strength" => {
                    if let Ok(x) = v.parse::<f32>() {
                        if x.is_finite() {
                            s.grass_wind_enabled = x > 0.5
                        }
                    }
                }
                "tree_wind_strength" => number!(s.tree_wind_strength, 0., 1.),
                "client_tree_wind_preview" => flag!(s.client_tree_wind_preview),
                "bloom_strength" => number!(s.bloom_strength, 0., 4.),
                "bloom_tint" => {
                    if let Ok(color) = v
                        .strip_prefix("0x")
                        .map_or_else(|| v.parse::<u32>(), |hex| u32::from_str_radix(hex, 16))
                    {
                        s.bloom_tint = color;
                    }
                }
                "bloom_brightness_fade" => number!(s.bloom_brightness_fade, 0., 1.),
                "bloom_opacity_fade" => number!(s.bloom_opacity_fade, 0., 1.),
                "bloom_mode" => {
                    if let Ok(mode) = v.parse::<u8>() {
                        s.bloom_mode = if matches!(mode, 4 | 5) { mode } else { 0 };
                    }
                }
                "effect_shaft" => {
                    if let Some(mode) = EffectShaft::parse(v) {
                        s.effect_shaft = mode;
                    }
                }
                "client_gamma" => flag!(s.client_gamma),
                "gamma_setting" => number!(s.gamma_setting, 0., 100.),
                "music_volume" => number!(s.music_volume, 0., 1.),
                "sound_volume" => number!(s.sound_volume, 0., 1.),
                "effects_volume" => number!(s.effects_volume, 0., 1.),
                "effects_enabled" => flag!(s.effects_enabled),
                "ambience_enabled" => flag!(s.ambience_enabled),
                "minimap_range" => number!(s.minimap_range, 100., 600.),
                "resolution_scale" => number!(s.resolution_scale, 10., 200.),
                "upscaling" => {
                    if let Some(method) = UpscaleMethod::parse(v) {
                        s.upscaling = method;
                    } else if v.trim().eq_ignore_ascii_case("fsr") {
                        legacy_fsr_bare = true;
                    }
                }
                "fsr1_sharpness" => number!(s.fsr1_sharpness, 0., 2.),
                "fsr3_sharpness" => number!(s.fsr3_sharpness, 0., 1.),
                "dlss_sharpness" => number!(s.dlss_sharpness, 0., 1.),
                "xess_sharpness" => number!(s.xess_sharpness, 0., 1.),
                "upscale_quality" => {
                    if let Some(quality) = UpscaleQuality::parse(v) {
                        s.upscale_quality = quality;
                    }
                }
                "frame_generation" => {
                    if let Some(method) = FrameGenMethod::parse(v) {
                        s.frame_generation = method;
                    }
                }
                "frame_multiplier" => {
                    if let Some(multiplier) = FrameMultiplier::parse(v) {
                        s.frame_multiplier = multiplier;
                    }
                }
                "fsr_version" => {
                    // Legacy key, consumed for the `upscaling=fsr` migration.
                    legacy_fsr_version = v.trim().parse::<u32>().ok();
                }
                "upscale_sdk_dir" => {
                    // An empty value is the default (env var or <exe>\sdk), so
                    // it is stored rather than ignored.
                    s.upscale_sdk_dir = v.to_string();
                }
                "latency" => {
                    // Unknown spellings keep VSync: a settings file written by
                    // a newer build must not silently change the present
                    // behaviour of this one.
                    if let Some(mode) = LatencyMode::parse(v) {
                        s.latency = mode;
                    }
                }
                "regional_tree_lighting" => flag!(s.regional_tree_lighting),
                "streamed_regions" => flag!(s.streamed_regions),
                "grass_distance_fade" => flag!(s.grass_distance_fade),
                "area_grass_lightmaps" => flag!(s.area_grass_lightmaps),
                "vegetation_authored_mips" => flag!(s.vegetation_authored_mips),
                "celestial_discs" => flag!(s.celestial_discs),
                "celestial_brightness" => number!(s.celestial_brightness, 0.1, 1.),
                "ignore_celestial_flags" => flag!(s.ignore_celestial_flags),
                "weather_override" => {
                    if let Some(option) = WeatherOverride::parse(v) {
                        s.weather_override = option;
                    }
                }
                "sky_override" => {
                    if let Some(option) = SkyOverride::parse(v) {
                        s.sky_override = option;
                    }
                }
                "cloud_detail" => flag!(s.cloud_detail),
                "whole_world" => flag!(s.whole_world),
                "water_reflections" => flag!(s.water_reflections),
                "logging_mask" => {
                    if let Ok(bits) = v.parse::<u64>() {
                        s.logging_mask = bits;
                    }
                }
                "logging_dir" => {
                    // An empty value is the default directory, so it is stored
                    // rather than ignored: clearing the box must persist too.
                    s.logging_dir = v.to_string();
                }
                "selected_world" => {
                    // Only a single-folder ID can be persisted or later passed
                    // as `--world`. Do not let a settings file inject path
                    // separators or process arguments.
                    if crate::world_catalog::valid_world_id(v) {
                        s.selected_world = v.to_ascii_lowercase();
                    }
                }
                "world_effects" => flag!(s.world_effects),
                "effect_distance" => {
                    if let Ok(metres) = v.parse::<f32>() {
                        s.effect_distance = if metres.is_finite() {
                            metres.clamp(0., 1000.)
                        } else {
                            0.
                        };
                    }
                }
                "atmospheric_fog" => flag!(s.atmospheric_fog),
                "bloom" => flag!(s.bloom),
                "terrain_lightmaps" => flag!(s.terrain_lightmaps),
                "model_backface_culling" => flag!(s.model_backface_culling),
                "music_enabled" => flag!(s.music_enabled),
                "fullscreen" => flag!(s.fullscreen),
                "show_zone_entry" => flag!(s.show_zone_entry),
                "show_minimap" => flag!(s.show_minimap),
                "world_map_style" => {
                    if let Some(style) = MapStyle::parse(v) {
                        s.world_map_style = style;
                    }
                }
                "minimap_style" => {
                    if let Some(style) = MapStyle::parse(v) {
                        s.minimap_style = style;
                    }
                }
                "minimap_shape" => {
                    if let Some(shape) = MinimapShape::parse(v) {
                        s.minimap_shape = shape;
                    }
                }
                _ => {}
            }
        }
        if legacy_fsr_bare {
            s.upscaling = match legacy_fsr_version {
                Some(1) => UpscaleMethod::Fsr1,
                Some(2) => UpscaleMethod::Fsr2,
                Some(3) => UpscaleMethod::Fsr3,
                // "newest advertised" (the old 0/4 default) is FSR 4 today.
                _ => UpscaleMethod::Fsr4,
            };
        }
        // Settings written by another build (or hand-edited) can hold an
        // upscaler/quality/frame-generator triple this build cannot honour;
        // coerce it to a legal one rather than failing the whole file.
        let (upscaling, upscale_quality, frame_generation) =
            crate::sanitise(s.upscaling, s.upscale_quality, s.frame_generation);
        s.upscaling = upscaling;
        s.upscale_quality = upscale_quality;
        s.frame_generation = frame_generation;
        s
    }
    fn encode(&self) -> String {
        format!(concat!("# Perfect World Explorer settings, version 1\n",
            "radius={}\nterrain_cutoff={}\ntree_lod_scale={}\nscenery_quality={}\ncamera_speed={}\nboost={}\nsensitivity={}\nfar={}\ntime_of_day={}\ntime_pass_enabled={}\ntime_pass_speed={}\nall_to_vram={}\n",
            "whole_world={}\nwater_reflections={}\nworld_effects={}\natmospheric_fog={}\nbloom={}\nbloom_strength={}\nbloom_mode={}\nbloom_tint={}\nbloom_brightness_fade={}\nbloom_opacity_fade={}\nclient_gamma={}\ngamma_setting={}\n",
            "terrain_lightmaps={}\nmodel_backface_culling={}\nmusic_enabled={}\nmusic_volume={}\nambience_enabled={}\nsound_volume={}\neffects_enabled={}\neffects_volume={}\nfullscreen={}\nshow_zone_entry={}\n",
            "show_minimap={}\nminimap_range={}\ntree_wind_strength={}\nclient_tree_wind_preview={}\nregional_tree_lighting={}\ngrass_distance_fade={}\ngrass_wind_enabled={}\ngrass_wind_amplitude={}\nstreamed_regions={}\narea_grass_lightmaps={}\nvegetation_authored_mips={}\ncelestial_discs={}\ncelestial_brightness={}\nignore_celestial_flags={}\ncloud_detail={}\nsharpen={}\nresolution_scale={}\nupscaling={}\nupscale_quality={}\nframe_generation={}\nframe_multiplier={}\nfsr1_sharpness={}\nfsr3_sharpness={}\ndlss_sharpness={}\nxess_sharpness={}\nupscale_sdk_dir={}\nlatency={}\nworld_map_style={}\nminimap_style={}\nminimap_shape={}\nweather_override={}\nsky_override={}\neffect_shaft={}\neffect_distance={}\nlogging_mask={}\nlogging_dir={}\nselected_world={}\n"),
            self.detail.radius,self.detail.terrain_cutoff,self.detail.tree_lod_scale,self.detail.scenery_quality,self.camera_speed,self.boost,self.sensitivity,self.far,self.time_of_day,self.time_pass_enabled,self.time_pass_speed,self.all_to_vram,
            self.whole_world,self.water_reflections,self.world_effects,self.atmospheric_fog,self.bloom,self.bloom_strength,self.bloom_mode,self.bloom_tint,self.bloom_brightness_fade,self.bloom_opacity_fade,self.client_gamma,self.gamma_setting,
            self.terrain_lightmaps,self.model_backface_culling,self.music_enabled,self.music_volume,self.ambience_enabled,self.sound_volume,self.effects_enabled,self.effects_volume,self.fullscreen,self.show_zone_entry,
            self.show_minimap,self.minimap_range,self.tree_wind_strength,self.client_tree_wind_preview,self.regional_tree_lighting,self.grass_distance_fade,self.grass_wind_enabled,self.grass_wind_amplitude,self.streamed_regions,self.area_grass_lightmaps,self.vegetation_authored_mips,self.celestial_discs,self.celestial_brightness,self.ignore_celestial_flags,self.cloud_detail,self.sharpen,self.resolution_scale,
            self.upscaling.token(),self.upscale_quality.token(),self.frame_generation.token(),self.frame_multiplier.token(),self.fsr1_sharpness,self.fsr3_sharpness,self.dlss_sharpness,self.xess_sharpness,self.upscale_sdk_dir,self.latency.token(),
            self.world_map_style.label(),self.minimap_style.label(),
            self.minimap_shape.label(),
            self.weather_override.token(),self.sky_override.token(),self.effect_shaft.token(),self.effect_distance,self.logging_mask,self.logging_dir,self.selected_world)
    }
    fn file() -> Option<std::path::PathBuf> {
        #[cfg(windows)]
        {
            std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .map(|p| p.join("PerfectWorldViewer").join("viewer-settings.txt"))
        }
        #[cfg(not(windows))]
        {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config"))
                })
                .map(|p| p.join("PerfectWorldViewer").join("viewer-settings.txt"))
        }
    }
    pub fn load() -> Self {
        Self::load_with_default_fog(false)
    }
    /// Use scene fog enable only as the default for a missing saved choice.
    pub fn load_with_default_fog(fog_enabled: bool) -> Self {
        let mut defaults = Self::default();
        defaults.atmospheric_fog = fog_enabled;
        Self::file()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|b| b.len() <= 16384)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|text| Self::decode_with_fog_default(&text, fog_enabled))
            .unwrap_or(defaults)
    }
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::file() else {
            return Err("settings directory is unavailable".into());
        };
        let parent = path.parent().ok_or("settings directory is unavailable")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        // Write a complete temporary file before replacing the old one. Windows
        // rename cannot replace existing files; remove the old file only after the temp write succeeds.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.encode()).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_world_defaults_to_main_and_validates_persisted_ids() {
        assert_eq!(ViewSettings::default().selected_world, "world");
        let s = ViewSettings::decode("selected_world=A01");
        assert_eq!(s.selected_world, "a01");
        // Upscaling/frame-generation settings are still present and unchanged.
        assert_eq!(s.upscaling, UpscaleMethod::Off);
        assert_eq!(s.frame_generation, FrameGenMethod::Off);
        assert_eq!(ViewSettings::decode(&s.encode()), s);
        for unsafe_id in ["../world", "a01/maps", "a b", ""] {
            assert_eq!(
                ViewSettings::decode(&format!("selected_world={unsafe_id}")).selected_world,
                "world"
            );
        }
    }

    #[test]
    fn authored_sharpen_toggle_round_trips_without_changing_defaults() {
        assert!(!ViewSettings::default().sharpen);
        let s = ViewSettings::decode("sharpen=true");
        assert!(s.sharpen);
        assert_eq!(s, ViewSettings::decode(&s.encode()));
        assert!(!ViewSettings::decode("sharpen=invalid").sharpen);
    }
    #[test]
    fn the_axial_shaft_rule_defaults_to_the_client_and_round_trips() {
        assert_eq!(ViewSettings::default().effect_shaft, EffectShaft::Client);
        // The default is the faithful rule, so a settings file written before
        // the switch existed keeps the client's own answer.
        assert_eq!(
            ViewSettings::decode("sharpen=true").effect_shaft,
            EffectShaft::Client
        );
        for mode in EffectShaft::ALL {
            let text = format!("effect_shaft={}", mode.token());
            let decoded = ViewSettings::decode(&text);
            assert_eq!(decoded.effect_shaft, mode);
            assert_eq!(ViewSettings::decode(&decoded.encode()).effect_shaft, mode);
        }
        // An unknown token keeps the default rather than silently becoming a
        // different rule.
        assert_eq!(
            ViewSettings::decode("effect_shaft=diagonal").effect_shaft,
            EffectShaft::Client
        );
    }

    #[test]
    fn map_styles_default_to_realistic_and_persist_independently() {
        let d = ViewSettings::default();
        assert_eq!(d.world_map_style, MapStyle::Realistic);
        assert_eq!(d.minimap_style, MapStyle::Realistic);
        // The two surfaces are independent: an original minimap does not drag the
        // world map off the live render, and vice versa.
        let s = ViewSettings::decode("minimap_style=Original");
        assert_eq!(s.minimap_style, MapStyle::Original);
        assert_eq!(s.world_map_style, MapStyle::Realistic);
        let back = ViewSettings::decode(&s.encode());
        assert_eq!(back.minimap_style, MapStyle::Original);
        assert_eq!(back.world_map_style, MapStyle::Realistic);
        let both = ViewSettings::decode("world_map_style=Original\nminimap_style=Original");
        assert_eq!(both.world_map_style, MapStyle::Original);
        assert_eq!(both.minimap_style, MapStyle::Original);
        assert_eq!(ViewSettings::decode(&both.encode()), both);
    }

    #[test]
    fn minimap_shape_round_trips_and_defaults_to_square() {
        assert_eq!(ViewSettings::default().minimap_shape, MinimapShape::Square);
        let s = ViewSettings::decode("minimap_shape=Circular");
        assert_eq!(s.minimap_shape, MinimapShape::Circular);
        assert_eq!(
            ViewSettings::decode(&s.encode()).minimap_shape,
            MinimapShape::Circular
        );
        // Independent of the artwork style.
        assert_eq!(s.minimap_style, MapStyle::Realistic);
        assert_eq!(
            ViewSettings::decode("minimap_shape=round").minimap_shape,
            MinimapShape::Square
        );
    }

    #[test]
    fn an_unrecognised_map_style_keeps_the_default_instead_of_switching() {
        // Falling back to the live render is the safe direction: a typo must not
        // silently select a style the user did not ask for.
        let s = ViewSettings::decode("world_map_style=Photorealistic\nminimap_style=original");
        assert_eq!(s.world_map_style, MapStyle::Realistic);
        assert_eq!(s.minimap_style, MapStyle::Realistic);
    }

    #[test]
    fn scene_fog_default_respects_an_explicit_saved_choice() {
        assert!(ViewSettings::decode_with_fog_default("", true).atmospheric_fog);
        assert!(!ViewSettings::decode_with_fog_default("", false).atmospheric_fog);
        assert!(
            !ViewSettings::decode_with_fog_default("atmospheric_fog=false", true).atmospheric_fog
        );
        assert!(
            ViewSettings::decode_with_fog_default("atmospheric_fog=true", false).atmospheric_fog
        );
    }

    #[test]
    fn cloud_detail_gate_defaults_on_and_round_trips() {
        assert!(ViewSettings::default().cloud_detail);
        let disabled = ViewSettings::decode("cloud_detail=false");
        assert!(!disabled.cloud_detail);
        assert!(!ViewSettings::decode(&disabled.encode()).cloud_detail);
        assert!(ViewSettings::decode("cloud_detail=invalid").cloud_detail);
    }

    #[test]
    fn round_trip_all_settings() {
        let mut s = ViewSettings::default();
        s.detail.radius = 11;
        s.detail.terrain_cutoff = true;
        s.bloom = true;
        s.whole_world = true;
        s.camera_speed = 511.;
        s.show_minimap = false;
        s.music_enabled = false;
        s.music_volume = 0.21;
        s.ambience_enabled = false;
        s.sound_volume = 0.37;
        s.effects_enabled = false;
        s.effects_volume = 0.18;
        s.atmospheric_fog = true;
        s.celestial_discs = true;
        s.cloud_detail = false;
        s.regional_tree_lighting = true;
        s.client_tree_wind_preview = true;
        s.grass_distance_fade = false;
        s.grass_wind_enabled = false;
        s.grass_wind_amplitude = 0.43;
        assert_eq!(s, ViewSettings::decode(&s.encode()));
    }
    #[test]
    fn sound_volume_is_independent_bounded_and_finite() {
        let settings =
            ViewSettings::decode("music_volume=0.2\nsound_volume=1.5\nambience_enabled=true");
        assert_eq!(settings.music_volume, 0.2);
        assert_eq!(settings.sound_volume, 1.0);
        assert!(settings.ambience_enabled);
        assert_eq!(ViewSettings::decode("sound_volume=-1").sound_volume, 0.0);
        assert_eq!(ViewSettings::decode("sound_volume=NaN").sound_volume, 0.5);
        assert!(ViewSettings::default().ambience_enabled);
        assert!(ViewSettings::default().effects_enabled);
        assert_eq!(ViewSettings::decode("effects_volume=5").effects_volume, 1.0);
        assert_eq!(
            ViewSettings::decode("effects_volume=NaN").effects_volume,
            0.5
        );
    }
    #[test]
    fn tree_wind_preview_defaults_off_and_persists() {
        assert!(!ViewSettings::default().client_tree_wind_preview);
        assert!(!ViewSettings::decode("tree_wind_strength=0.5").client_tree_wind_preview);
        let enabled = ViewSettings::decode("client_tree_wind_preview=true");
        assert!(enabled.client_tree_wind_preview);
        assert!(ViewSettings::decode(&enabled.encode()).client_tree_wind_preview);
        assert!(!ViewSettings::decode("client_tree_wind_preview=invalid").client_tree_wind_preview);
    }
    #[test]
    fn scenery_quality_preset_round_trips_and_is_bounded() {
        assert_eq!(ViewSettings::default().detail.scenery_quality, 2);
        let s = ViewSettings::decode("scenery_quality=4");
        assert_eq!(s.detail.scenery_quality, 4);
        assert_eq!(ViewSettings::decode(&s.encode()).detail.scenery_quality, 4);
        assert_eq!(
            ViewSettings::decode("scenery_quality=99")
                .detail
                .scenery_quality,
            5
        );
        assert_eq!(
            ViewSettings::decode("scenery_quality=nan")
                .detail
                .scenery_quality,
            2
        );
    }

    #[test]
    fn bad_values_are_safe() {
        let s = ViewSettings::decode(
            "far=NaN\nradius=90000\nboost=-5\nsensitivity=inf\nbloom=bad\nunknown=stuff\n",
        );
        assert_eq!(s.far, 16000.);
        assert_eq!(s.detail.radius, 12);
        assert_eq!(s.boost, 1.);
        assert_eq!(s.sensitivity, 0.0025);
        assert!(!s.bloom);
    }
    /// The strength slider is gone because native wind has no user scale, but
    /// files written by older builds must still load.
    #[test]
    fn grass_wind_is_a_switch_and_legacy_strength_still_loads() {
        assert!(ViewSettings::default().grass_wind_enabled);
        assert!(!ViewSettings::decode("grass_wind_enabled=false").grass_wind_enabled);
        assert!(ViewSettings::decode("grass_wind_enabled=true").grass_wind_enabled);
        assert!(ViewSettings::decode("grass_wind_strength=0.8").grass_wind_enabled);
        assert!(!ViewSettings::decode("grass_wind_strength=0").grass_wind_enabled);
        // An unparsable legacy value leaves the default (on) alone.
        assert!(ViewSettings::decode("grass_wind_strength=bogus").grass_wind_enabled);
        let off = ViewSettings::decode("grass_wind_enabled=false");
        assert!(!ViewSettings::decode(&off.encode()).grass_wind_enabled);
        let on = ViewSettings::decode("grass_wind_enabled=true");
        assert!(ViewSettings::decode(&on.encode()).grass_wind_enabled);
    }
    #[test]
    fn area_center_lightmaps_default_on_but_explicit_legacy_setting_is_preserved() {
        assert!(ViewSettings::default().area_grass_lightmaps);
        assert!(ViewSettings::decode("unknown=ignored").area_grass_lightmaps);
        let s = ViewSettings::decode("area_grass_lightmaps=false");
        assert!(!s.area_grass_lightmaps);
        assert!(!ViewSettings::decode(&s.encode()).area_grass_lightmaps);
    }
    #[test]
    fn vegetation_authored_mips_defaults_off_and_round_trips() {
        assert!(!ViewSettings::default().vegetation_authored_mips);
        assert!(!ViewSettings::decode("unknown=ignored").vegetation_authored_mips);
        let s = ViewSettings::decode("vegetation_authored_mips=true");
        assert!(s.vegetation_authored_mips);
        assert_eq!(s, ViewSettings::decode(&s.encode()));
        assert!(!ViewSettings::decode("vegetation_authored_mips=invalid").vegetation_authored_mips);
    }
    /// The fade radius is an explorer setting, not an authored one: it starts at
    /// 0 (off), survives a save/load pair, and refuses nonsense.
    #[test]
    fn effect_distance_defaults_off_and_round_trips() {
        let defaults = ViewSettings::default();
        assert_eq!(defaults.effect_distance, 0.);
        let decoded = ViewSettings::decode("effect_distance=60");
        assert_eq!(decoded.effect_distance, 60.);
        assert!(decoded.encode().contains("effect_distance=60"));
        let negative = ViewSettings::decode("effect_distance=-5");
        assert_eq!(negative.effect_distance, 0.);
        let undefined = ViewSettings::decode("effect_distance=nan");
        assert_eq!(undefined.effect_distance, 0.);
        let absurd = ViewSettings::decode("effect_distance=10000");
        assert_eq!(absurd.effect_distance, 1000.);
    }

    /// The world's own effects draw unless the explorer switches them off, and a
    /// settings file written before the switch existed keeps them on.
    #[test]
    fn world_effects_default_on_and_round_trip() {
        assert!(ViewSettings::default().world_effects);
        assert!(ViewSettings::decode("unknown=ignored").world_effects);
        let off = ViewSettings::decode("world_effects=false");
        assert!(!off.world_effects);
        assert_eq!(off, ViewSettings::decode(&off.encode()));
        assert!(
            ViewSettings::decode("world_effects=invalid").world_effects,
            "an unparseable value leaves the default, which is on"
        );
        assert!(
            ViewSettings::decode(&off.encode()).encode() == off.encode(),
            "the switch survives a second round trip"
        );
    }
    #[test]
    fn special_glow_preview_is_explicit_persisted_and_bounded() {
        assert_eq!(ViewSettings::default().bloom_mode, 0);
        for mode in [4, 5] {
            let s = ViewSettings::decode(&format!("bloom_mode={mode}"));
            assert_eq!(s.bloom_mode, mode);
            assert_eq!(ViewSettings::decode(&s.encode()).bloom_mode, mode);
        }
        for invalid in ["3", "255", "-1", "bad"] {
            assert_eq!(
                ViewSettings::decode(&format!("bloom_mode={invalid}")).bloom_mode,
                0
            );
        }
    }
    #[test]
    fn glow_caller_color_and_independent_fades_round_trip_safely() {
        let s = ViewSettings::decode(
            "bloom_tint=0x8040a0e0\nbloom_brightness_fade=0.25\nbloom_opacity_fade=0.5",
        );
        assert_eq!(s.bloom_tint, 0x8040a0e0);
        assert_eq!(s.bloom_brightness_fade, 0.25);
        assert_eq!(s.bloom_opacity_fade, 0.5);
        assert_eq!(s, ViewSettings::decode(&s.encode()));
        let invalid = ViewSettings::decode(
            "bloom_tint=bogus\nbloom_brightness_fade=NaN\nbloom_opacity_fade=2",
        );
        assert_eq!(invalid.bloom_tint, 0xffffffff);
        assert_eq!(invalid.bloom_brightness_fade, 0.);
        assert_eq!(invalid.bloom_opacity_fade, 1.);
    }
    #[test]
    fn the_disc_brightness_defaults_to_the_plain_texture_and_is_bounded() {
        // The recovered half colour (0x4b405f: ([+0x48] & 0xfefefe | 0x01010100) >> 1)
        // read as a dim grey disc, so the default is the texture itself and the
        // recovered 127/255 is still a legal setting.
        assert_eq!(ViewSettings::default().celestial_brightness, 1.);
        // The area-flag override is on by default so Courage Path keeps
        // its sun despite a missing sun bit; it still round-trips.
        assert!(ViewSettings::default().ignore_celestial_flags);
        let flags = ViewSettings::decode("ignore_celestial_flags=true");
        assert!(flags.ignore_celestial_flags);
        assert!(ViewSettings::decode(&flags.encode()).ignore_celestial_flags);
        assert_eq!(
            ViewSettings::decode("celestial_brightness=0.49803922").celestial_brightness,
            0.49803922
        );
        let s = ViewSettings::decode("celestial_brightness=0.25");
        assert_eq!(s.celestial_brightness, 0.25);
        assert_eq!(ViewSettings::decode(&s.encode()), s);
        // Out of range, non-numeric and non-finite values fall back safely.
        for (text, expected) in [
            ("celestial_brightness=0", 0.1),
            ("celestial_brightness=4", 1.0),
            ("celestial_brightness=NaN", 1.0),
            ("celestial_brightness=bogus", 1.0),
        ] {
            let s = ViewSettings::decode(text);
            assert!(
                (s.celestial_brightness - expected).abs() < 1e-6,
                "{text} -> {}",
                s.celestial_brightness
            );
        }
    }

    #[test]
    fn the_weather_and_sky_overrides_default_to_the_client_and_round_trip() {
        let d = ViewSettings::default();
        assert_eq!(d.weather_override, WeatherOverride::Auto);
        assert_eq!(d.sky_override, SkyOverride::Auto);
        // A file written before the overrides existed keeps both on Auto.
        let legacy = ViewSettings::decode("celestial_discs=true\nroutine=none");
        assert_eq!(legacy.weather_override, WeatherOverride::Auto);
        assert_eq!(legacy.sky_override, SkyOverride::Auto);
        for weather in WeatherOverride::ALL {
            for sky in SkyOverride::ALL {
                let mut s = ViewSettings::default();
                s.weather_override = weather;
                s.sky_override = sky;
                let back = ViewSettings::decode(&s.encode());
                assert_eq!(back.weather_override, weather);
                assert_eq!(back.sky_override, sky);
            }
        }
        // Unknown spellings leave the defaults alone.
        let bad = ViewSettings::decode("weather_override=Hail\nsky_override=night");
        assert_eq!(bad.weather_override, WeatherOverride::Auto);
        assert_eq!(bad.sky_override, SkyOverride::Auto);
    }

    #[test]
    fn time_pass_and_resident_everything_are_persisted_and_bounded() {
        let d = ViewSettings::default();
        // The shipped behaviour: the slider stays put, and the viewer keeps the
        // bounded caches the optimisations introduced.
        assert!(!d.time_pass_enabled);
        assert_eq!(d.time_pass_speed, 1.0);
        assert!(!d.all_to_vram);
        // A file written before these keys existed keeps the defaults.
        let legacy = ViewSettings::decode("time_of_day=18\nwhole_world=true");
        assert!(!legacy.time_pass_enabled);
        assert_eq!(legacy.time_pass_speed, 1.0);
        assert!(!legacy.all_to_vram);
        let on =
            ViewSettings::decode("time_pass_enabled=true\ntime_pass_speed=240\nall_to_vram=true");
        assert!(on.time_pass_enabled);
        assert_eq!(on.time_pass_speed, 240.0);
        assert!(on.all_to_vram);
        assert_eq!(ViewSettings::decode(&on.encode()), on);
        // Non-numeric and non-finite values fall back to the safe default.
        let bad =
            ViewSettings::decode("time_pass_enabled=bogus\ntime_pass_speed=NaN\nall_to_vram=1");
        assert!(!bad.time_pass_enabled);
        assert_eq!(bad.time_pass_speed, 1.0);
        assert!(!bad.all_to_vram);
        // The speed slider's range matches `sky_time`'s clamps (0.1..600).
        assert_eq!(
            ViewSettings::decode("time_pass_speed=0.001").time_pass_speed,
            0.1
        );
        assert_eq!(
            ViewSettings::decode("time_pass_speed=99999").time_pass_speed,
            600.0
        );
    }

    #[test]
    fn render_resolution_scale_is_persisted_and_bounded() {
        assert_eq!(ViewSettings::default().resolution_scale, 100.);
        for (text, expected) in [
            ("resolution_scale=10", 10.),
            ("resolution_scale=200", 200.),
            ("resolution_scale=63.5", 63.5),
        ] {
            let s = ViewSettings::decode(text);
            assert_eq!(s.resolution_scale, expected);
            assert_eq!(ViewSettings::decode(&s.encode()), s);
        }
        // Out-of-range, non-numeric and non-finite values are safe.
        for bad in ["0", "201", "-5", "NaN", "inf", "bogus"] {
            let s = ViewSettings::decode(&format!("resolution_scale={bad}"));
            assert!(
                (10. ..=200.).contains(&s.resolution_scale),
                "unexpected scale for {bad}: {}",
                s.resolution_scale
            );
        }
        assert_eq!(
            ViewSettings::decode("resolution_scale=0").resolution_scale,
            10.
        );
        assert_eq!(
            ViewSettings::decode("resolution_scale=999").resolution_scale,
            200.
        );
        assert_eq!(
            ViewSettings::decode("resolution_scale=NaN").resolution_scale,
            100.
        );
    }

    /// A settings file written by the previous release (no upscaling keys)
    /// must load unchanged: every old key keeps its value, the new keys take
    /// their documented defaults. The fixture is the pre-upscaling format
    /// byte-for-byte, with non-default values so a silent fallback to the
    /// default would be caught.
    #[test]
    fn settings_file_from_main_loads_unchanged() {
        let legacy = "# Perfect World Explorer settings, version 1\n\
radius=7\nterrain_cutoff=true\ntree_lod_scale=2.5\nscenery_quality=3\n\
camera_speed=321\nboost=3\nsensitivity=0.0045\nfar=12000\ntime_of_day=9.5\n\
time_pass_enabled=true\ntime_pass_speed=42\nall_to_vram=true\n\
whole_world=true\nwater_reflections=false\nworld_effects=false\natmospheric_fog=true\n\
bloom=true\nbloom_strength=2.5\nbloom_mode=4\nbloom_tint=0x80ff00ff\n\
bloom_brightness_fade=0.25\nbloom_opacity_fade=0.5\nclient_gamma=true\ngamma_setting=55\n\
terrain_lightmaps=false\nmodel_backface_culling=false\nmusic_enabled=false\n\
music_volume=0.75\nambience_enabled=false\nsound_volume=0.25\n\
effects_enabled=false\neffects_volume=0.125\nfullscreen=true\nshow_zone_entry=false\n\
show_minimap=false\nminimap_range=333\ntree_wind_strength=0.5\n\
client_tree_wind_preview=true\nregional_tree_lighting=true\ngrass_distance_fade=false\n\
grass_wind_enabled=false\ngrass_wind_amplitude=1.5\nstreamed_regions=true\n\
area_grass_lightmaps=false\nvegetation_authored_mips=true\ncelestial_discs=false\n\
celestial_brightness=0.4\nignore_celestial_flags=false\ncloud_detail=false\nsharpen=true\n\
resolution_scale=63.5\nworld_map_style=Original\nminimap_style=Original\n\
minimap_shape=Circular\nweather_override=Rain\nsky_override=Sunset\n\
effect_shaft=up\neffect_distance=125.5\nlogging_mask=7\nlogging_dir=C:\\pwe-logs\n";
        let s = ViewSettings::decode(legacy);
        assert_eq!(s.detail.radius, 7);
        assert!(s.detail.terrain_cutoff);
        assert_eq!(s.detail.tree_lod_scale, 2.5);
        assert_eq!(s.detail.scenery_quality, 3);
        assert_eq!(s.camera_speed, 321.);
        assert_eq!(s.boost, 3.);
        assert_eq!(s.sensitivity, 0.0045);
        assert_eq!(s.far, 12000.);
        assert_eq!(s.time_of_day, 9.5);
        assert!(s.time_pass_enabled);
        assert_eq!(s.time_pass_speed, 42.);
        assert!(s.all_to_vram);
        assert!(s.whole_world);
        assert!(!s.water_reflections);
        assert!(!s.world_effects);
        assert!(s.atmospheric_fog);
        assert!(s.bloom);
        assert_eq!(s.bloom_strength, 2.5);
        assert_eq!(s.bloom_mode, 4);
        assert_eq!(s.bloom_tint, 0x80ff00ff);
        assert_eq!(s.bloom_brightness_fade, 0.25);
        assert_eq!(s.bloom_opacity_fade, 0.5);
        assert!(s.client_gamma);
        assert_eq!(s.gamma_setting, 55.);
        assert!(!s.terrain_lightmaps);
        assert!(!s.model_backface_culling);
        assert!(!s.music_enabled);
        assert_eq!(s.music_volume, 0.75);
        assert!(!s.ambience_enabled);
        assert_eq!(s.sound_volume, 0.25);
        assert!(!s.effects_enabled);
        assert_eq!(s.effects_volume, 0.125);
        assert!(s.fullscreen);
        assert!(!s.show_zone_entry);
        assert!(!s.show_minimap);
        assert_eq!(s.minimap_range, 333.);
        assert_eq!(s.tree_wind_strength, 0.5);
        assert!(s.client_tree_wind_preview);
        assert!(s.regional_tree_lighting);
        assert!(!s.grass_distance_fade);
        assert!(!s.grass_wind_enabled);
        assert_eq!(s.grass_wind_amplitude, 1.5);
        assert!(s.streamed_regions);
        assert!(!s.area_grass_lightmaps);
        assert!(s.vegetation_authored_mips);
        assert!(!s.celestial_discs);
        assert_eq!(s.celestial_brightness, 0.4);
        assert!(!s.ignore_celestial_flags);
        assert!(!s.cloud_detail);
        assert!(s.sharpen);
        assert_eq!(s.resolution_scale, 63.5);
        assert_eq!(s.world_map_style, MapStyle::Original);
        assert_eq!(s.minimap_style, MapStyle::Original);
        assert_eq!(s.minimap_shape, MinimapShape::Circular);
        assert_eq!(s.weather_override, WeatherOverride::Rain);
        assert_eq!(s.sky_override, SkyOverride::Sunset);
        assert_eq!(s.effect_shaft, EffectShaft::WorldUp);
        assert_eq!(s.effect_distance, 125.5);
        assert_eq!(s.logging_mask, 7);
        assert_eq!(s.logging_dir, "C:\\pwe-logs");
        // New keys take their defaults; encoding re-emits the old keys too.
        assert_eq!(s.upscaling, crate::UpscaleMethod::Off);
        assert_eq!(s.upscale_quality, crate::UpscaleQuality::Quality);
        assert_eq!(s.frame_generation, crate::FrameGenMethod::Off);
        assert_eq!(s.upscale_sdk_dir, "");
        assert_eq!(s.latency, LatencyMode::Vsync);
        let re_encoded = s.encode();
        // `bloom_tint` is the one legacy key whose value is re-encoded in a
        // different spelling (hex input, decimal output).
        assert!(re_encoded.contains("bloom_tint=2164195583"));
        for line in legacy
            .lines()
            .filter(|line| line.contains('=') && !line.starts_with("bloom_tint="))
        {
            assert!(
                re_encoded.contains(line),
                "settings re-encode lost legacy key line {line:?}"
            );
        }
        assert_eq!(ViewSettings::decode(&re_encoded), s);
    }

    /// The new upscaling keys round-trip, and illegal persisted combinations
    /// are coerced to legal ones instead of failing the whole settings file.
    #[test]
    fn upscale_settings_round_trip_and_coercion() {
        let defaults = ViewSettings::default();
        assert_eq!(defaults.upscaling, crate::UpscaleMethod::Off);
        assert_eq!(defaults.upscale_quality, crate::UpscaleQuality::Quality);
        assert_eq!(defaults.frame_generation, crate::FrameGenMethod::Off);
        assert_eq!(defaults.upscale_sdk_dir, "");
        assert_eq!(defaults.latency, LatencyMode::Vsync);
        let s = ViewSettings::decode(
            "upscaling=dlss\nupscale_quality=performance\nframe_generation=dlss\n\
             upscale_sdk_dir=D:\\vendor-sdks\n",
        );
        assert_eq!(s.upscaling, crate::UpscaleMethod::Dlss);
        assert_eq!(s.upscale_quality, crate::UpscaleQuality::Performance);
        assert_eq!(s.frame_generation, crate::FrameGenMethod::Dlss);
        assert_eq!(s.upscale_sdk_dir, "D:\\vendor-sdks");
        assert_eq!(ViewSettings::decode(&s.encode()), s);
        // Unknown tokens fall back to Off/Quality.
        let unknown = ViewSettings::decode(
            "upscaling=vendorx\nupscale_quality=gaga\nframe_generation=vendorx\n",
        );
        assert_eq!(unknown.upscaling, crate::UpscaleMethod::Off);
        assert_eq!(unknown.upscale_quality, crate::UpscaleQuality::Quality);
        assert_eq!(unknown.frame_generation, crate::FrameGenMethod::Off);
        // Each provider keeps the modes it advertises: XeSS has the full
        // ladder (SDK 3.0.2), so Ultra Performance survives…
        let xess = ViewSettings::decode("upscaling=xess\nupscale_quality=ultraperformance\n");
        assert_eq!(
            xess.upscale_quality,
            crate::UpscaleQuality::UltraPerformance
        );
        // …and a mode the provider does not have degrades to the nearest ratio
        // it does (DLSS has no Ultra Quality: 1.3x -> Quality's 1.5x).
        let dlss = ViewSettings::decode("upscaling=dlss\nupscale_quality=ultraquality\n");
        assert_eq!(dlss.upscale_quality, crate::UpscaleQuality::Quality);
        // XeSS Native AA is a real XeSS mode (`XESS_QUALITY_SETTING_AA`).
        let xess_aa = ViewSettings::decode("upscaling=xess\nupscale_quality=nativeaa\n");
        assert_eq!(xess_aa.upscale_quality, crate::UpscaleQuality::NativeAa);
        // Cross-vendor XeSS frame generation is not exposed: coerced to Off.
        let cross = ViewSettings::decode("upscaling=fsr2\nframe_generation=xess\n");
        assert_eq!(cross.frame_generation, crate::FrameGenMethod::Off);
        // DLSS + DLSS FG is legal and survives.
        let legal = ViewSettings::decode("upscaling=dlss\nframe_generation=dlss\n");
        assert_eq!(legal.frame_generation, crate::FrameGenMethod::Dlss);
        // Mixed case tokens parse.
        let mixed = ViewSettings::decode("upscaling=FSR3\nframe_generation=FSR\n");
        assert_eq!(mixed.upscaling, crate::UpscaleMethod::Fsr3);
        assert_eq!(mixed.frame_generation, crate::FrameGenMethod::Fsr);
        // Case-insensitive parse keeps the rest of the file intact.
        assert_eq!(ViewSettings::decode(&mixed.encode()), mixed);
    }

    /// The Graphics -> Frame multiplier key: round-trips, defaults to x2, and
    /// ignores anything it does not recognise (a stale or hand-edited file
    /// must not change how many frames are presented).
    #[test]
    fn frame_multiplier_round_trip() {
        use crate::FrameMultiplier;
        assert_eq!(
            ViewSettings::default().frame_multiplier,
            FrameMultiplier::X2
        );
        for multiplier in FrameMultiplier::ALL {
            let text = format!("frame_multiplier={}\n", multiplier.token());
            let decoded = ViewSettings::decode(&text);
            assert_eq!(decoded.frame_multiplier, multiplier, "{text:?}");
            let encoded = decoded.encode();
            assert!(
                encoded.contains(&format!("frame_multiplier={}", multiplier.token())),
                "encode lost the frame multiplier key: {encoded}"
            );
            assert_eq!(ViewSettings::decode(&encoded).frame_multiplier, multiplier);
        }
        // Accepted spellings and junk.
        assert_eq!(
            ViewSettings::decode("frame_multiplier=x4\n").frame_multiplier,
            FrameMultiplier::X4
        );
        for junk in [
            "frame_multiplier=\n",
            "frame_multiplier=5\n",
            "frame_multiplier=x8\n",
        ] {
            assert_eq!(
                ViewSettings::decode(junk).frame_multiplier,
                FrameMultiplier::X2,
                "{junk:?}"
            );
        }
        // Trailing-`x` spellings are accepted too.
        assert_eq!(
            ViewSettings::decode("frame_multiplier=6x\n").frame_multiplier,
            FrameMultiplier::X6
        );
        // It does not disturb the neighbouring frame generation key (DLSS
        // frame generation is legal with any upscaler, including Off).
        let both = ViewSettings::decode("frame_generation=dlss\nframe_multiplier=3\n");
        assert_eq!(both.frame_multiplier, FrameMultiplier::X3);
        assert_eq!(both.frame_generation, crate::FrameGenMethod::Dlss);
        assert_eq!(ViewSettings::decode(&both.encode()), both);
    }

    /// The Graphics -> Latency key: round-trips, defaults to VSync, and falls
    /// back to VSync on anything it does not recognise (a viewer that loses
    /// its latency setting must not lose its present path).
    #[test]
    fn latency_settings_round_trip() {
        assert_eq!(ViewSettings::default().latency, LatencyMode::Vsync);
        for mode in LatencyMode::ALL {
            let text = format!("latency={}\n", mode.token());
            let decoded = ViewSettings::decode(&text);
            assert_eq!(decoded.latency, mode, "{text:?}");
            let encoded = decoded.encode();
            assert!(
                encoded.contains(&format!("latency={}", mode.token())),
                "encode lost the latency key: {encoded}"
            );
            assert_eq!(ViewSettings::decode(&encoded).latency, mode);
        }
        // `reflex` is a real mode now, so only near-misses stay junk.
        for junk in [
            "latency=\n",
            "latency=reflexx\n",
            "latency=2\n",
            "latency=xel\n",
        ] {
            assert_eq!(
                ViewSettings::decode(junk).latency,
                LatencyMode::Vsync,
                "{junk:?}"
            );
        }
        // Case and surrounding whitespace are tolerated like every other key.
        assert_eq!(
            ViewSettings::decode("latency=XE LL\n").latency,
            LatencyMode::Vsync
        );
        assert_eq!(
            ViewSettings::decode("latency= XeLL \n").latency,
            LatencyMode::Xell
        );
        // The latency choice never disturbs the upscaler / frame generator,
        // and coercion of those does not disturb it.
        let both = ViewSettings::decode("upscaling=fsr2\nframe_generation=xess\nlatency=xell\n");
        assert_eq!(both.frame_generation, FrameGenMethod::Off);
        assert_eq!(both.latency, LatencyMode::Xell);
        assert_eq!(ViewSettings::decode(&both.encode()), both);
    }
    /// The DLSS and XeSS sharpening sliders persist and are clamped too: DLSS
    /// hands its value to NGX, XeSS runs the viewer's own pass.
    #[test]
    fn dlss_and_xess_sharpness_round_trip() {
        // Both default to a mild sharpen: 0 is what made DLSS (and XeSS, which
        // has no sharpening of its own) look soft next to an RCAS-sharpened FSR
        // frame, so the neutral "vendor" look is a deliberate choice, not the
        // default.
        let defaults = ViewSettings::default();
        assert_eq!(defaults.dlss_sharpness, 0.35);
        assert_eq!(defaults.xess_sharpness, 0.35);
        let decoded =
            ViewSettings::decode("upscaling=dlss\ndlss_sharpness=0.6\nxess_sharpness=0.9\n");
        assert_eq!(decoded.dlss_sharpness, 0.6);
        assert_eq!(decoded.xess_sharpness, 0.9);
        let encoded = decoded.encode();
        assert!(encoded.contains("dlss_sharpness=0.6"), "{encoded}");
        assert!(encoded.contains("xess_sharpness=0.9"), "{encoded}");
        let round = ViewSettings::decode(&encoded);
        assert_eq!(round.dlss_sharpness, 0.6);
        assert_eq!(round.xess_sharpness, 0.9);
        // Out-of-range values are clamped, garbage keeps the default.
        assert_eq!(
            ViewSettings::decode("dlss_sharpness=-3\n").dlss_sharpness,
            0.
        );
        assert_eq!(
            ViewSettings::decode("xess_sharpness=9\n").xess_sharpness,
            1.
        );
        assert_eq!(
            ViewSettings::decode("dlss_sharpness=abc\n").dlss_sharpness,
            defaults.dlss_sharpness
        );
        assert_eq!(
            ViewSettings::decode("xess_sharpness=0\n").xess_sharpness,
            0.
        );
    }

    /// The FSR sharpening slider persists, is clamped, and defaults to the
    /// value the dispatch used before the slider existed.
    #[test]
    fn fsr3_sharpness_round_trip() {
        assert_eq!(ViewSettings::default().fsr3_sharpness, 0.75);
        let decoded = ViewSettings::decode("upscaling=fsr3\nfsr3_sharpness=0.25\n");
        assert_eq!(decoded.fsr3_sharpness, 0.25);
        let encoded = decoded.encode();
        assert!(encoded.contains("fsr3_sharpness=0.25"), "{encoded}");
        assert_eq!(ViewSettings::decode(&encoded).fsr3_sharpness, 0.25);
        // Out-of-range values clamp to the 0..1 range, junk keeps the default.
        assert_eq!(
            ViewSettings::decode("fsr3_sharpness=-1\n").fsr3_sharpness,
            0.0
        );
        assert_eq!(
            ViewSettings::decode("fsr3_sharpness=7\n").fsr3_sharpness,
            1.0
        );
        assert_eq!(
            ViewSettings::decode("fsr3_sharpness=abc\n").fsr3_sharpness,
            0.75
        );
        // 0 is a legal value and means "no sharpening", not "unset".
        assert_eq!(
            ViewSettings::decode("fsr3_sharpness=0\n").fsr3_sharpness,
            0.0
        );
    }

    #[test]
    fn legacy_bare_fsr_migrates_via_fsr_version() {
        // Pre-selector settings files: `upscaling=fsr` with an `fsr_version`
        // discriminator, in either key order.
        for (text, expected) in [
            ("upscaling=fsr\n", crate::UpscaleMethod::Fsr4),
            ("upscaling=fsr\nfsr_version=0\n", crate::UpscaleMethod::Fsr4),
            ("upscaling=fsr\nfsr_version=4\n", crate::UpscaleMethod::Fsr4),
            ("upscaling=fsr\nfsr_version=3\n", crate::UpscaleMethod::Fsr3),
            ("upscaling=fsr\nfsr_version=2\n", crate::UpscaleMethod::Fsr2),
            ("upscaling=fsr\nfsr_version=1\n", crate::UpscaleMethod::Fsr1),
            ("fsr_version=3\nupscaling=fsr\n", crate::UpscaleMethod::Fsr3),
            // An explicit modern choice wins; the legacy key is ignored.
            (
                "upscaling=fsr2\nfsr_version=4\n",
                crate::UpscaleMethod::Fsr2,
            ),
        ] {
            assert_eq!(ViewSettings::decode(text).upscaling, expected, "{text:?}");
        }
        // The migrated triple is re-encoded as the modern token.
        let s = ViewSettings::decode("upscaling=fsr\nfsr_version=3\n");
        assert_eq!(ViewSettings::decode(&s.encode()), s);
        assert!(s.encode().contains("upscaling=fsr3"));
        assert!(!s.encode().contains("fsr_version"));
    }
    #[test]
    fn every_independent_post_effect_activates_the_renderer_chain() {
        // bloom, sharpen, underwater and the scene-chain ramp each allocate the
        // chain on their own; the screen-space fog diagnostic does not.
        for bits in 0..16 {
            let f = |i| bits & (1 << i) != 0;
            assert_eq!(super::post_required(f(0), f(1), f(2), f(3)), bits != 0);
        }
        assert!(super::post_required(false, true, false, false));
        assert!(!super::post_required(false, false, false, false));
    }
    #[test]
    fn cutoff_is_independent_of_fog_and_backwards_compatible() {
        assert!(
            !ViewSettings::decode("atmospheric_fog=true")
                .detail
                .terrain_cutoff
        );
        for cutoff in [false, true] {
            for fog in [false, true] {
                let s = ViewSettings::decode(&format!(
                    "terrain_cutoff={cutoff}\natmospheric_fog={fog}"
                ));
                let saved = ViewSettings::decode(&s.encode());
                assert_eq!(saved.detail.terrain_cutoff, cutoff);
                assert_eq!(saved.atmospheric_fog, fog);
            }
        }
        assert!(
            !ViewSettings::decode("terrain_cutoff=garbage")
                .detail
                .terrain_cutoff
        );
    }
}
