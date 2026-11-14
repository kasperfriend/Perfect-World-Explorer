//! Persisted viewer controls; malformed values fall back safely.
use crate::client_weather::WeatherOverride;
use crate::sky_transition::SkyOverride;

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
    pub resolution_scale: f32,
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
            // Both overrides default to the client's own behaviour; the World
            // tab offers them only so the recovered sets and particle
            // constants can be inspected anywhere.
            weather_override: WeatherOverride::Auto,
            sky_override: SkyOverride::Auto,
            // Off by default: logging is a tool the user reaches for, not a cost
            // every session pays.
            logging_mask: 0,
            logging_dir: String::new(),
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
        s
    }
    fn encode(&self) -> String {
        format!(concat!("# Perfect World Explorer settings, version 1\n",
            "radius={}\nterrain_cutoff={}\ntree_lod_scale={}\nscenery_quality={}\ncamera_speed={}\nboost={}\nsensitivity={}\nfar={}\ntime_of_day={}\ntime_pass_enabled={}\ntime_pass_speed={}\nall_to_vram={}\n",
            "whole_world={}\nwater_reflections={}\nworld_effects={}\natmospheric_fog={}\nbloom={}\nbloom_strength={}\nbloom_mode={}\nbloom_tint={}\nbloom_brightness_fade={}\nbloom_opacity_fade={}\nclient_gamma={}\ngamma_setting={}\n",
            "terrain_lightmaps={}\nmodel_backface_culling={}\nmusic_enabled={}\nmusic_volume={}\nambience_enabled={}\nsound_volume={}\neffects_enabled={}\neffects_volume={}\nfullscreen={}\nshow_zone_entry={}\n",
            "show_minimap={}\nminimap_range={}\ntree_wind_strength={}\nclient_tree_wind_preview={}\nregional_tree_lighting={}\ngrass_distance_fade={}\ngrass_wind_enabled={}\ngrass_wind_amplitude={}\nstreamed_regions={}\narea_grass_lightmaps={}\nvegetation_authored_mips={}\ncelestial_discs={}\ncelestial_brightness={}\nignore_celestial_flags={}\ncloud_detail={}\nsharpen={}\nresolution_scale={}\nworld_map_style={}\nminimap_style={}\nminimap_shape={}\nweather_override={}\nsky_override={}\neffect_shaft={}\neffect_distance={}\nlogging_mask={}\nlogging_dir={}\n"),
            self.detail.radius,self.detail.terrain_cutoff,self.detail.tree_lod_scale,self.detail.scenery_quality,self.camera_speed,self.boost,self.sensitivity,self.far,self.time_of_day,self.time_pass_enabled,self.time_pass_speed,self.all_to_vram,
            self.whole_world,self.water_reflections,self.world_effects,self.atmospheric_fog,self.bloom,self.bloom_strength,self.bloom_mode,self.bloom_tint,self.bloom_brightness_fade,self.bloom_opacity_fade,self.client_gamma,self.gamma_setting,
            self.terrain_lightmaps,self.model_backface_culling,self.music_enabled,self.music_volume,self.ambience_enabled,self.sound_volume,self.effects_enabled,self.effects_volume,self.fullscreen,self.show_zone_entry,
            self.show_minimap,self.minimap_range,self.tree_wind_strength,self.client_tree_wind_preview,self.regional_tree_lighting,self.grass_distance_fade,self.grass_wind_enabled,self.grass_wind_amplitude,self.streamed_regions,self.area_grass_lightmaps,self.vegetation_authored_mips,self.celestial_discs,self.celestial_brightness,self.ignore_celestial_flags,self.cloud_detail,self.sharpen,self.resolution_scale,
            self.world_map_style.label(),self.minimap_style.label(),
            self.minimap_shape.label(),
            self.weather_override.token(),self.sky_override.token(),self.effect_shaft.token(),self.effect_distance,self.logging_mask,self.logging_dir)
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
