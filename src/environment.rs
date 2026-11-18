//! Version-12 world EC-BSD box-area environment records, as loaded by
//! elementclient 0x41da70 / 0x48a510. No auto-family scene constants here.
use crate::scene::Scene;
#[path = "client_region_map.rs"]
mod client_region_map;
#[path = "client_regional_light.rs"]
mod client_regional_light;
use client_region_map::ActiveBoxMap;
#[path = "client_region_stream.rs"]
mod client_region_stream;
use client_region_stream::{Change, Window};
#[path = "client_region_shade.rs"]
mod client_region_shade;
pub use client_region_shade::frame_ms;
use client_region_shade::{inverse_time, seconds, unpack_rgb, Channel};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Instance maps name their EC-BSD after the map folder (`a01.ecbsd`), while
/// the main world and stand-alone fixtures keep `world.ecbsd`. This local
/// resolver keeps standalone diagnostic binaries independent of the viewer's
/// world catalog module.
fn ecbsd_path(dir: &Path) -> PathBuf {
    let stem = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let named = dir.join(format!("{stem}.ecbsd"));
    if named.is_file() {
        named
    } else {
        dir.join("world.ecbsd")
    }
}

/// Folder owning the shared archives. `dir` is normally `element/maps/<id>`;
/// a direct map directory argument has the archive root as its parent.
fn element_root(dir: &Path) -> PathBuf {
    dir.parent()
        .map(|parent| {
            if parent.file_name() == Some(std::ffi::OsStr::new("maps")) {
                parent.parent().unwrap_or(parent)
            } else {
                parent
            }
        })
        .unwrap_or(dir)
        .to_path_buf()
}

/// Client world day weight (1 = day, 0 = night).
/// `4b60e0` produces the complementary night factor at object +0x70;
/// `41ec86`, `42b4fe`, and `42bd6e` pass it to fog, BMD and terrain.
/// Hour is the world-clock time, normalized by 24 before the client curve.
pub fn daylight(hour: f32) -> f32 {
    1. - night_factor(hour)
}
pub fn night_factor(hour: f32) -> f32 {
    client_regional_light::night_factor(hour)
}

#[derive(Clone, Copy, Debug)]
pub struct Fog {
    pub start: f32,
    pub end: f32,
    pub density: f32,
    pub color: [f32; 3],
}
impl Fog {
    fn blend(a: Self, b: Self, t: f32) -> Self {
        let mix = |x: f32, y: f32| x + (y - x) * t;
        Self {
            start: mix(a.start, b.start),
            end: mix(a.end, b.end),
            density: mix(a.density, b.density),
            color: std::array::from_fn(|i| mix(a.color[i], b.color[i])),
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct FogEndpoints {
    air_day: Fog,
    air_night: Fog,
    water_day: Fog,
    water_night: Fog,
    ambient_day: [f32; 3],
    ambient_night: [f32; 3],
    diffuse_day: [f32; 3],
    diffuse_night: [f32; 3],
    sun_direction: [f32; 3],
    celestial_flags: u32,
}
impl FogEndpoints {
    fn values(self) -> [f32; 36] {
        let mut v = [0.; 36];
        for (i, f) in [
            self.air_day,
            self.air_night,
            self.water_day,
            self.water_night,
        ]
        .into_iter()
        .enumerate()
        {
            v[i * 6..i * 6 + 6].copy_from_slice(&[
                f.start, f.end, f.density, f.color[0], f.color[1], f.color[2],
            ]);
        }
        for (i, c) in [
            self.ambient_day,
            self.ambient_night,
            self.diffuse_day,
            self.diffuse_night,
        ]
        .into_iter()
        .enumerate()
        {
            v[24 + i * 3..27 + i * 3].copy_from_slice(&c);
        }
        v
    }
    fn from_values(v: [f32; 36], sun_direction: [f32; 3], celestial_flags: u32) -> Self {
        let f = |i: usize| {
            let p = i * 6;
            Fog {
                start: v[p],
                end: v[p + 1],
                density: v[p + 2],
                color: [v[p + 3], v[p + 4], v[p + 5]],
            }
        };
        let c = |p: usize| [v[p], v[p + 1], v[p + 2]];
        Self {
            air_day: f(0),
            air_night: f(1),
            water_day: f(2),
            water_night: f(3),
            ambient_day: c(24),
            ambient_night: c(27),
            diffuse_day: c(30),
            diffuse_night: c(33),
            sun_direction,
            celestial_flags,
        }
    }
    fn at_hour(self, hour: f32) -> (Fog, Fog) {
        let day = daylight(hour);
        (
            Fog::blend(self.air_night, self.air_day, day),
            Fog::blend(self.water_night, self.water_day, day),
        )
    }
}

/// Authored material light direction. The native value is the EC-BSD region
/// vector installed through `0x41fd58..0x41fdb1` / `0x79b390`; it is NOT the
/// animated sky-object elevation from `0x4b60b0/0x4b60e0`. Keeping it in a
/// named type makes the consumer boundary explicit: only rows in
/// `light_consumers::MATRIX` with `DirectionSource::Authored` may read it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialDirection([f32; 3]);
impl MaterialDirection {
    /// Direction the light travels (the D3DLIGHT8 `direction` argument).
    pub fn travel(self) -> [f32; 3] {
        self.0
    }
    /// Direction toward the light. Celestial azimuth uses this side; material
    /// lighting uses `travel`, so the two consumers must not share a sign.
    pub fn toward_sun(self) -> [f32; 3] {
        [-self.0[0], -self.0[1], -self.0[2]]
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RegionalLight {
    pub ambient: [f32; 3],
    pub diffuse: [f32; 3],
    pub specular: [f32; 3],
    pub toward_sun: [f32; 3],
    pub celestial_flags: u32,
}
impl RegionalLight {
    /// The native region selector independently reads the rain and snow bits
    /// from the selected default area's authored flags (`0x421520`).
    pub fn weather_selection(&self) -> crate::client_weather::WeatherSelection {
        crate::client_weather::WeatherSelection::from_area_flags(self.celestial_flags)
    }

    /// Authored region direction, normalized exactly like the previous inline
    /// renderer code (glam `normalize_or_zero`, i.e. reciprocal multiply).
    /// Hidden celestial flags never alter this value: they hide discs
    /// (`0x4b2d0c`/`0x4b3b80`), not directional material light.
    pub fn material_direction(&self) -> MaterialDirection {
        let v = self.toward_sun;
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let rcp = 1. / len;
        MaterialDirection(if rcp.is_finite() && rcp > 0. {
            [-v[0] * rcp, -v[1] * rcp, -v[2] * rcp]
        } else {
            [0.; 3]
        })
    }
}
/// One frame's environment selection. `default_present` records whether the
/// independently selected default area was actually found; the SUN/MOON flag
/// word and the material direction come from that area, not from the local box
/// that supplies fog/colour (`0x41ebaf..0x41ec65`, `0x41f244`, `0x41fd58`).
struct Selection {
    key: Option<usize>,
    target: FogEndpoints,
    duration_ms: u32,
    default_present: bool,
}
/// Stored-step transitions between authored region endpoints. Each new
/// box starts from the current in-flight values, with duration in EC-BSD.
#[derive(Clone, Debug)]
pub struct FogTransition {
    key: Option<usize>,
    channels: [Channel; 36],
    sun_direction: [f32; 3],
    celestial_flags: u32,
    /// Diagnostic: false means the world had no selected default box, so the
    /// retained direction/flags were kept instead of inventing new ones.
    default_present: bool,
}
impl FogTransition {
    fn initial(sel: Selection) -> Self {
        Self {
            key: sel.key,
            channels: sel.target.values().map(Channel::new),
            // No selected default box leaves nothing to read at +0xc4/+0x80.
            // Use the explicit viewer direction fallback and the recovered
            // constructor 0x4b2d0c flag default rather than the local box's
            // unrelated direction/flags; the streamed path never routes a
            // local vector or a local flag word into these fields.
            sun_direction: if sel.default_present {
                sel.target.sun_direction
            } else {
                [0., -1., 0.]
            },
            celestial_flags: if sel.default_present {
                sel.target.celestial_flags
            } else {
                0x18
            },
            default_present: sel.default_present,
        }
    }
    pub fn new(env: &Environments, p: [f32; 3]) -> Self {
        Self::initial(env.endpoints(p))
    }
    /// Whether the last frame found an independently selected default area.
    /// The renderer reports this instead of silently presenting the vertical
    /// fallback as recovered data.
    pub fn default_present(&self) -> bool {
        self.default_present
    }
    fn current(&self) -> FogEndpoints {
        FogEndpoints::from_values(
            self.channels.map(|c| c.value),
            self.sun_direction,
            self.celestial_flags,
        )
    }
    pub fn lighting(&self, hour: f32) -> Option<RegionalLight> {
        self.key?;
        let e = self.current();
        let nf = night_factor(hour);
        let colors = client_regional_light::evaluate(hour, e.diffuse_day, e.diffuse_night);
        Some(RegionalLight {
            ambient: client_regional_light::ambient(e.ambient_day, e.ambient_night, nf),
            diffuse: colors.diffuse,
            specular: colors.specular,
            toward_sun: e.sun_direction.map(|x| -x),
            celestial_flags: e.celestial_flags,
        })
    }
    pub fn update(&mut self, env: &Environments, p: [f32; 3], hour: f32, dt: f32) -> (Fog, Fog) {
        self.update_ms(env, p, hour, Self::milliseconds(dt))
    }
    pub fn update_ms(&mut self, env: &Environments, p: [f32; 3], hour: f32, ms: u32) -> (Fog, Fog) {
        self.update_endpoints(env.endpoints(p), hour, ms)
    }
    pub fn update_streamed(
        &mut self,
        env: &Environments,
        active: &mut RegionStreaming,
        p: [f32; 3],
        hour: f32,
        dt: f32,
    ) -> (Fog, Fog) {
        self.update_streamed_ms(env, active, p, hour, Self::milliseconds(dt))
    }
    pub fn update_streamed_ms(
        &mut self,
        env: &Environments,
        active: &mut RegionStreaming,
        p: [f32; 3],
        hour: f32,
        ms: u32,
    ) -> (Fog, Fog) {
        self.update_endpoints(active.endpoints(env, p), hour, ms)
    }
    fn milliseconds(dt: f32) -> u32 {
        if dt.is_finite() {
            (dt.max(0.) as f64 * 1000.).min(u32::MAX as f64) as u32
        } else {
            0
        }
    }
    pub fn new_streamed(env: &Environments, active: &mut RegionStreaming, p: [f32; 3]) -> Self {
        Self::initial(active.endpoints(env, p))
    }
    fn update_endpoints(&mut self, sel: Selection, hour: f32, ms: u32) -> (Fog, Fog) {
        // 0x420992 extracts the SUN/MOON bits from the independently selected
        // default area on EVERY render, including frames where no local region
        // supplies colour/fog. When the enumeration found no default box there
        // is nothing to read, so the previous default's flag word is retained
        // instead of inventing visibility (the initial value is the recovered
        // constructor 0x4b2d0c default). Flags are never a material switch.
        if sel.default_present {
            self.celestial_flags = sel.target.celestial_flags;
        }
        self.default_present = sel.default_present;
        // 0x41ec65: no active region leaves the in-flight colour/fog unchanged.
        if sel.key.is_none() && self.key.is_some() {
            return self.current().at_hour(hour);
        }
        if sel.key != self.key {
            self.key = sel.key;
            // 0x41fd58/0x41fdb1 read the DEFAULT box direction on a key change.
            // A missing default box must not substitute the local box's
            // unrelated vector, so keep the previously published direction.
            if sel.default_present {
                self.sun_direction = sel.target.sun_direction;
            }
            let inv = inverse_time(sel.duration_ms);
            for (channel, value) in self.channels.iter_mut().zip(sel.target.values()) {
                channel.target(value, inv);
            }
            // 0x41fdbe jumps over shading. A changed-area frame only installs
            // targets; its dt must NOT also advance the new transition.
        } else {
            let dt = seconds(ms);
            for channel in &mut self.channels {
                channel.shade(dt);
            }
        }
        self.current().at_hour(hour)
    }
}
impl Default for Fog {
    fn default() -> Self {
        Self {
            start: 10.,
            end: 100.,
            density: 0.5,
            color: [24. / 255., 100. / 255., 124. / 255.],
        }
    }
}
#[derive(Clone, Debug)]
struct BoxArea {
    global: bool,
    transition_ms: u32,
    center: [f32; 3],
    extents: [f32; 3],
    axes: [[f32; 3]; 3],
    day: Fog,
    night: Fog,
    air_day: Fog,
    air_night: Fog,
    ambient_day: [f32; 3],
    ambient_night: [f32; 3],
    diffuse_day: [f32; 3],
    diffuse_night: [f32; 3],
    sun_direction: [f32; 3],
    celestial_flags: u32,
}
#[derive(Clone, Default)]
pub struct Environments {
    areas: Vec<BoxArea>,
    id_to_index: BTreeMap<u32, usize>,
    streaming: Option<RegionLayout>,
    auto_fog: Option<crate::sky_config::AutoFogConfig>,
}
/// Block references retained for runtime regional environment selection.
#[derive(Clone)]
struct RegionLayout {
    width: f32,
    length: f32,
    block_size: f32,
    rows: usize,
    cols: usize,
    boxes: Vec<Vec<u32>>,
}
pub struct RegionStreaming {
    layout: RegionLayout,
    map: ActiveBoxMap,
    window: Window,
}
impl RegionStreaming {
    fn new(layout: RegionLayout) -> Self {
        let window =
            Window::new(layout.rows, layout.cols, 1).expect("validated scene block dimensions");
        Self {
            layout,
            window,
            map: ActiveBoxMap::default(),
        }
    }
    fn endpoints(&mut self, env: &Environments, p: [f32; 3]) -> Selection {
        self.update(p);
        let (local, default) = self.map.select_pair(|id| {
            env.id_to_index.get(&id).and_then(|&i| {
                let area = &env.areas[i];
                area.contains(p).then_some(area.global)
            })
        });
        let index = local
            .or(default)
            .and_then(|id| env.id_to_index.get(&id).copied());
        let (key, mut target, duration_ms) = env.endpoints_for(index);
        // 0x41ebe2 retains the default independently of the local (ebp).
        // 0x41f244 / 0x41fd58 read its +c4 direction on init / key change, and
        // 0x420992 reads its +80 flag word every render. Report whether that
        // box was actually found: a missing default must retain the previous
        // values rather than substitute the local box's unrelated vector.
        let default_area = default
            .and_then(|id| env.id_to_index.get(&id))
            .map(|&i| &env.areas[i]);
        if let Some(area) = default_area {
            target.celestial_flags = area.celestial_flags;
            target.sun_direction = area.sun_direction;
        }
        Selection {
            key,
            target,
            duration_ms,
            default_present: default_area.is_some(),
        }
    }

    fn update(&mut self, p: [f32; 3]) {
        let l = &self.layout;
        if !p.iter().all(|x| x.is_finite()) || l.block_size <= 0. {
            return;
        }
        // 0x41c480 uses ceil(500 / block size), inclusive center and
        // exclusive upper bounds. At edges it shifts the full rectangle in.
        let radius = (500. / l.block_size).ceil() as isize;
        let span = 2 * radius + 1;
        let col = ((p[0] + l.width * 0.5) / l.block_size) as isize;
        let row = -(((p[2] - l.length * 0.5) / l.block_size) as isize);
        fn bounds(center: isize, span: isize, limit: usize) -> (usize, usize) {
            let width = span.min(limit as isize);
            let start = (center - span / 2).clamp(0, limit as isize - width);
            (start as usize, (start + width) as usize)
        }
        let (x0, x1) = bounds(col, span, l.cols);
        let (y0, y1) = bounds(row, span, l.rows);
        let changes = self
            .window
            .update((x0, x1, y0, y1))
            .expect("clamped scene block rectangle");
        for change in changes {
            match change {
                Change::Load(index) => {
                    for &id in &l.boxes[index] {
                        self.map.load(id);
                    }
                }
                Change::Unload(index) => {
                    for &id in &l.boxes[index] {
                        self.map.unload(id);
                    }
                }
            }
        }
    }
}
fn word(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("truncated box area")?
            .try_into()
            .unwrap(),
    ))
}
fn float(b: &[u8], p: usize) -> Result<f32, String> {
    let f = f32::from_bits(word(b, p)?);
    if !f.is_finite() {
        return Err("invalid box-area float".into());
    }
    Ok(f)
}
fn vec3(b: &[u8], p: usize) -> Result<[f32; 3], String> {
    Ok([float(b, p)?, float(b, p + 4)?, float(b, p + 8)?])
}
fn rgb(b: &[u8], p: usize) -> Result<[f32; 3], String> {
    Ok(unpack_rgb(word(b, p)?))
}
fn scaled_rgb(b: &[u8], p: usize, scale: usize) -> Result<[f32; 3], String> {
    let k = float(b, scale)?;
    Ok(rgb(b, p)?.map(|x| x * k))
}
fn fog(b: &[u8], p: usize) -> Result<Fog, String> {
    Ok(Fog {
        start: float(b, p)?,
        end: float(b, p + 4)?,
        density: float(b, p + 8)?,
        color: unpack_rgb(word(b, p + 12)?),
    })
}
impl BoxArea {
    fn parse(b: &[u8], p: usize) -> Result<Self, String> {
        let bytes = b
            .get(p..p.checked_add(0xe0).ok_or("box offset overflow")?)
            .ok_or("truncated EC-BSD box area")?;
        let global = word(bytes, 0)? != 0;
        let area = Self {
            global,
            transition_ms: word(bytes, 0x9c)?,
            celestial_flags: word(bytes, 4)?,
            center: vec3(bytes, 8)?,
            extents: if global {
                [1000.; 3]
            } else {
                vec3(bytes, 0x14)?
            },
            axes: [vec3(bytes, 0x20)?, vec3(bytes, 0x2c)?, vec3(bytes, 0x38)?],
            day: fog(bytes, 0x54)?,
            night: fog(bytes, 0xd0)?,
            air_day: fog(bytes, 0x44)?,
            air_night: fog(bytes, 0x64)?,
            ambient_day: rgb(bytes, 0x80)?,
            ambient_night: rgb(bytes, 0x88)?,
            diffuse_day: scaled_rgb(bytes, 0x84, 0xb8)?,
            diffuse_night: scaled_rgb(bytes, 0x8c, 0xbc)?,
            sun_direction: vec3(bytes, 0x74)?,
        };
        if area.extents.iter().any(|v| *v < 0.) {
            return Err("invalid box-area fog or bounds".into());
        }
        Ok(area)
    }
    fn contains(&self, p: [f32; 3]) -> bool {
        let d = [
            p[0] - self.center[0],
            p[1] - self.center[1],
            p[2] - self.center[2],
        ];
        (0..3).all(|i| {
            let a = self.axes[i];
            (d[0] * a[0] + d[1] * a[1] + d[2] * a[2]).abs() <= self.extents[i]
        })
    }
    fn distance2(&self, p: [f32; 3]) -> f32 {
        (0..3).map(|i| (p[i] - self.center[i]).powi(2)).sum()
    }
}
impl Environments {
    pub fn load(scene: &Scene, dir: &Path) -> Result<Self, String> {
        let b = fs::read(ecbsd_path(dir)).map_err(|e| e.to_string())?;
        let mut refs = BTreeMap::new();
        for block in &scene.blocks {
            for r in &block.boxes {
                refs.insert(r.export_id, r.offset as usize);
            }
        }
        let mut areas = Vec::with_capacity(refs.len());
        let mut id_to_index = BTreeMap::new();
        for (id, p) in refs {
            id_to_index.insert(id, areas.len());
            areas.push(BoxArea::parse(&b, p).map_err(|e| format!("environment box {id}: {e}"))?);
        }
        let mut boxes = vec![Vec::new(); scene.rows * scene.cols];
        for block in &scene.blocks {
            let i = block.row as usize * scene.cols + block.col as usize;
            if i >= boxes.len() {
                return Err("invalid region block coordinates".into());
            }
            boxes[i] = block.boxes.iter().map(|r| r.export_id).collect();
        }
        let streaming = Some(RegionLayout {
            width: scene.width,
            length: scene.length,
            block_size: scene.block_size,
            rows: scene.rows,
            cols: scene.cols,
            boxes,
        });
        // AutoFamily fog is a scene-level fallback only. EC-BSD area fog stays
        // authoritative whenever an area is selected.
        let auto_fog = (|| -> Result<crate::sky_config::AutoFogConfig, String> {
            let mut file =
                fs::File::open(element_root(dir).join("configs.pck")).map_err(|e| e.to_string())?;
            // `configs.pck` is an Angelica File Package, not the retail v2 PCK
            // `textures.pck` uses. The retail reader rejects its guard, which
            // silently disabled the AutoFamily fog fallback.
            let entries = crate::angelica_pck::directory(&mut file)?;
            let entry = entries
                .iter()
                .find(|entry| {
                    entry
                        .name
                        .eq_ignore_ascii_case("configs/autofamilyconfigs/autoscene.cfg")
                })
                .ok_or("missing AutoFamily autoscene.cfg in configs.pck")?;
            crate::sky_config::parse_autofog(&crate::pck::read_entry(&mut file, entry)?)
        })()
        .ok();
        eprintln!("Loaded {} client environment boxes", areas.len());
        Ok(Self {
            areas,
            id_to_index,
            streaming,
            auto_fog,
        })
    }
    pub fn streaming(&self) -> Option<RegionStreaming> {
        self.streaming.clone().map(RegionStreaming::new)
    }
    pub fn auto_fog_enabled(&self) -> bool {
        self.auto_fog.is_some_and(|fog| fog.enabled)
    }
    pub fn underwater(&self, p: [f32; 3], hour: f32) -> Fog {
        self.sample(p, hour, true)
    }
    pub fn atmosphere(&self, p: [f32; 3], hour: f32) -> Fog {
        self.sample(p, hour, false)
    }
    fn endpoints(&self, p: [f32; 3]) -> Selection {
        // Selection order is still a heuristic pending the streaming-order
        // trace, but the two native selections stay separate here exactly as in
        // `RegionStreaming::endpoints`: the nearest containing non-global box
        // owns colour and fog, while the containing GLOBAL box owns the sun/moon
        // flag word and the authored direction. `0x41ebe2` retains the default
        // independently of the local box and `0x420992` reads its `+0x80` word
        // on every render, so a local box can never switch the discs off.
        let mut local: Option<(usize, f32)> = None;
        let mut default: Option<(usize, f32)> = None;
        for (i, a) in self.areas.iter().enumerate() {
            if !a.contains(p) {
                continue;
            }
            let slot = if a.global { &mut default } else { &mut local };
            let d = a.distance2(p);
            let nearer = match *slot {
                Some((_, best)) => d < best,
                None => true,
            };
            if nearer {
                *slot = Some((i, d));
            }
        }
        let index = local.or(default).map(|(i, _)| i);
        let (key, mut target, duration_ms) = self.endpoints_for(index);
        if let Some((i, _)) = default {
            target.celestial_flags = self.areas[i].celestial_flags;
            target.sun_direction = self.areas[i].sun_direction;
        }
        Selection {
            key,
            target,
            duration_ms,
            default_present: default.is_some(),
        }
    }
    fn endpoints_for(&self, index: Option<usize>) -> (Option<usize>, FogEndpoints, u32) {
        if let Some(i) = index {
            let a = &self.areas[i];
            return (
                Some(i),
                FogEndpoints {
                    air_day: a.air_day,
                    air_night: a.air_night,
                    water_day: a.day,
                    water_night: a.night,
                    ambient_day: a.ambient_day,
                    ambient_night: a.ambient_night,
                    diffuse_day: a.diffuse_day,
                    diffuse_night: a.diffuse_night,
                    sun_direction: a.sun_direction,
                    celestial_flags: a.celestial_flags,
                },
                a.transition_ms,
            );
        }
        let (air_day, air_night, water_day, water_night) = if let Some(fog) = self.auto_fog {
            (
                Fog {
                    start: fog.day_start,
                    end: fog.day_end,
                    density: 1.,
                    color: fog.day_color,
                },
                Fog {
                    start: fog.night_start,
                    end: fog.night_end,
                    density: 1.,
                    color: fog.night_color,
                },
                Fog {
                    color: fog.water_color,
                    ..Fog::default()
                },
                Fog {
                    color: fog.water_color,
                    ..Fog::default()
                },
            )
        } else {
            let air = Fog {
                start: 60.,
                end: 400.,
                density: 1.,
                color: [70. / 255., 77. / 255., 91. / 255.],
            };
            let water = Fog::default();
            (air, air, water, water)
        };
        (
            None,
            FogEndpoints {
                air_day,
                air_night,
                water_day,
                water_night,
                ambient_day: [0.; 3],
                ambient_night: [0.; 3],
                diffuse_day: [0.; 3],
                diffuse_night: [0.; 3],
                sun_direction: [0., -1., 0.],
                celestial_flags: 0x18,
            },
            0,
        )
    }
    fn sample(&self, p: [f32; 3], hour: f32, water: bool) -> Fog {
        let (air, water_fog) = self.endpoints(p).target.at_hour(hour);
        if water {
            water_fog
        } else {
            air
        }
    }
}
#[cfg(test)]
mod tests {
    /// The box a region selects, as `(area index, celestial flag word)`.
    ///
    /// Native reads BOTH from the independently selected default area
    /// (`0x420992` extracts the SUN/MOON bits every render, `0x79b390` refreshes
    /// the direction on a region install), so a sky target can only ever be the
    /// box the environment actually selected - never a neighbouring block's.
    fn sky_selection(sel: &Selection) -> (Option<usize>, u32) {
        (sel.key, sel.target.celestial_flags)
    }

    use super::*;
    #[test]
    fn autofamily_fog_is_used_only_as_the_no_area_fallback() {
        let mut env = Environments::default();
        env.auto_fog = Some(crate::sky_config::AutoFogConfig {
            enabled: true,
            day_start: 2500.,
            day_end: 12000.,
            day_color: [0.1, 0.2, 0.3],
            night_start: 1800.,
            night_end: 8000.,
            night_color: [0.4, 0.5, 0.6],
            water_color: [0.02, 0.04, 0.08],
        });
        let (_, fallback, _) = env.endpoints_for(None);
        assert_eq!(fallback.air_day.start, 2500.);
        assert_eq!(fallback.air_day.end, 12000.);
        assert_eq!(fallback.air_day.color, [0.1, 0.2, 0.3]);
        assert_eq!(fallback.air_night.color, [0.4, 0.5, 0.6]);
        assert_eq!(fallback.water_day.color, [0.02, 0.04, 0.08]);
        assert_eq!(fallback.water_night.color, [0.02, 0.04, 0.08]);
    }

    #[test]
    fn client_v12_box_fog_and_priority() {
        let mut b = vec![0u8; 224];
        b[4..8].copy_from_slice(&8u32.to_le_bytes());
        fn put(b: &mut [u8], p: usize, v: f32) {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes())
        }
        for i in 0..3 {
            put(&mut b, 0x14 + 4 * i, 50.);
            put(&mut b, 0x20 + 16 * i, 1.);
        }
        for (p, c) in [
            (0x54, 0xff18647cu32),
            (0xd0, 0xff242448),
            (0x44, 0xff123456),
            (0x64, 0xff654321),
        ] {
            put(&mut b, p, 10.);
            put(&mut b, p + 4, 100.);
            put(&mut b, p + 8, 0.5);
            b[p + 12..p + 16].copy_from_slice(&c.to_le_bytes());
        }
        for (p, c) in [
            (0x80, 0xff59738eu32),
            (0x84, 0xff4b586bu32),
            (0x88, 0xff6c858eu32),
            (0x8c, 0xff383d52u32),
        ] {
            b[p..p + 4].copy_from_slice(&c.to_le_bytes());
        }
        put(&mut b, 0xb8, 1.);
        put(&mut b, 0xbc, 0.5);
        put(&mut b, 0x74, 0.1524305);
        put(&mut b, 0x78, -0.619779);
        put(&mut b, 0x7c, 0.76983047);
        let a = BoxArea::parse(&b, 0).unwrap();
        assert_eq!(a.celestial_flags, 8);
        assert!((a.ambient_day[0] - 89. / 255.).abs() < 1e-6);
        assert!((a.diffuse_night[1] - 0.5 * 61. / 255.).abs() < 1e-6);
        assert!((a.sun_direction[1] + 0.619779).abs() < 1e-6);
        assert!(a.contains([49., 0., 0.]));
        assert!(!a.contains([51., 0., 0.]));
        let e = Environments {
            areas: vec![a],
            ..Default::default()
        };
        let d = e.underwater([0.; 3], 12.);
        let n = e.underwater([0.; 3], 0.);
        assert_eq!(d.start, 10.);
        assert!((d.color[1] - 100. / 255.).abs() < 1e-6);
        assert!((n.color[1] - 36. / 255.).abs() < 1e-6);
        let air = e.atmosphere([0.; 3], 12.);
        assert!((air.color[0] - 18. / 255.).abs() < 1e-6);
        let mut global = e.areas[0].clone();
        global.global = true;
        global.day.start = 80.;
        let mut local = e.areas[0].clone();
        local.day.start = 7.;
        let overlap = Environments {
            areas: vec![global, local],
            ..Default::default()
        };
        assert_eq!(overlap.underwater([0.; 3], 12.).start, 7.);
        // A new region's duration governs a linear transition, including a
        // second crossing before the first transition completes.
        let mut second = overlap.areas[1].clone();
        second.center = [200., 0., 0.];
        second.day.start = 27.;
        second.night.start = 47.;
        second.ambient_day = [1., 0.5, 0.25];
        second.diffuse_day = [0.4, 0.3, 0.2];
        second.transition_ms = 5000;
        // The authored direction and flag word belong to the containing GLOBAL
        // box only (0x41f244/0x420992), so this fixture marks the box that owns
        // the origin as the default one.
        let mut base = overlap.areas[1].clone();
        base.global = true;
        let mut env = Environments {
            areas: vec![base, second],
            ..Default::default()
        };
        let mut transition = FogTransition::new(&env, [0.; 3]);
        let light = transition.lighting(5.).unwrap();
        assert_eq!(light.ambient[0], 98. * f32::from_bits(0x3b808081));
        assert!((light.toward_sun[1] - 0.619779).abs() < 1e-6);
        let (_, unchanged) = transition.update(&env, [200., 0., 0.], 12., 2.5);
        assert_eq!(unchanged.start, 7.);
        let (_, w) = transition.update(&env, [200., 0., 0.], 12., 2.5);
        assert!((w.start - 17.).abs() < 1e-5);
        let mid = transition.lighting(12.).unwrap();
        assert_eq!(mid.ambient[0], 172. * f32::from_bits(0x3b808081));
        assert_eq!(mid.diffuse[0], 88. * f32::from_bits(0x3b808081));
        let (_, outside) = transition.update(&env, [500., 0., 0.], 12., 10.);
        assert!((outside.start - 17.).abs() < 1e-5);
        let (_, night) = transition.update(&env, [200., 0., 0.], 0., 0.);
        assert!((night.start - 28.5).abs() < 1e-5);
        env.areas[0].transition_ms = 5000;
        let (_, unchanged) = transition.update(&env, [0., 0., 0.], 12., 2.5);
        assert!((unchanged.start - 17.).abs() < 1e-5);
        let (_, reversing) = transition.update(&env, [0., 0., 0.], 12., 2.5);
        assert!((reversing.start - 12.).abs() < 1e-5);
        let (_, done) = transition.update(&env, [0., 0., 0.], 12., 2.5);
        assert!((done.start - 7.).abs() < 1e-5);
        transition.update(&env, [200., 0., 0.], 12., 5.);
        env.areas[0].transition_ms = 0;
        let (_, instant) = transition.update(&env, [0.; 3], 12., 0.);
        assert!((instant.start - 7.).abs() < 1e-5);
    }
    #[test]
    fn non_streamed_selection_reads_the_sun_moon_word_from_the_global_box() {
        let fog = Fog::default();
        let area = BoxArea {
            global: false,
            transition_ms: 5000,
            center: [0.; 3],
            extents: [10000.; 3],
            axes: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            day: fog,
            night: fog,
            air_day: fog,
            air_night: fog,
            ambient_day: [1.; 3],
            ambient_night: [0.; 3],
            diffuse_day: [1.; 3],
            diffuse_night: [0.; 3],
            sun_direction: [0., -1., 0.],
            celestial_flags: 0,
        };
        let mut global = area.clone();
        global.global = true;
        global.celestial_flags = 0x18;
        global.sun_direction = [1., 0., 0.];
        let mut local = area.clone();
        local.day.start = 7.;
        local.sun_direction = [0., 0., 1.];
        let env = Environments {
            areas: vec![global, local.clone()],
            ..Default::default()
        };
        // The local box still owns colour, exactly as before.
        assert_eq!(env.underwater([0.; 3], 12.).start, 7.);
        let transition = FogTransition::new(&env, [0.; 3]);
        let light = transition.lighting(12.).unwrap();
        // ...but never the flag word or the authored direction (0x420992).
        assert_eq!(light.celestial_flags, 0x18);
        assert_eq!(light.toward_sun, [-1., 0., 0.]);
        assert!(transition.default_present());
        // A world without a containing global box keeps the recovered
        // constructor default instead of borrowing the local box's word.
        let only_local = Environments {
            areas: vec![local],
            ..Default::default()
        };
        let bare = FogTransition::new(&only_local, [0.; 3]);
        assert_eq!(bare.lighting(12.).unwrap().celestial_flags, 0x18);
        assert!(!bare.default_present());
    }
    #[test]
    fn streamed_region_selection_tracks_blocks_references_and_transitions() {
        let fog = Fog::default();
        let area = BoxArea {
            global: false,
            transition_ms: 5000,
            center: [0.; 3],
            extents: [10000.; 3],
            axes: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            day: fog,
            night: fog,
            air_day: fog,
            air_night: fog,
            ambient_day: [1.; 3],
            ambient_night: [0.; 3],
            diffuse_day: [1.; 3],
            diffuse_night: [0.; 3],
            sun_direction: [0., -1., 0.],
            celestial_flags: 0x18,
        };
        let mut global = area.clone();
        global.global = true;
        global.day.start = 90.;
        global.sun_direction = [1., 0., 0.];
        global.celestial_flags = 8;
        let mut local = area.clone();
        local.day.start = 10.;
        local.sun_direction = [0., 0., 1.];
        local.celestial_flags = 16;
        let mut second = area;
        second.day.start = 30.;
        let mut ids = BTreeMap::new();
        ids.insert(1, 0);
        ids.insert(54, 1);
        ids.insert(2, 2);
        let layout = RegionLayout {
            width: 4000.,
            length: 500.,
            block_size: 500.,
            rows: 1,
            cols: 8,
            boxes: vec![
                vec![1],
                vec![1, 54],
                vec![1, 54],
                vec![1, 54, 2],
                vec![1, 2],
                vec![1],
                vec![1],
                vec![1],
            ],
        };
        let mut env = Environments {
            areas: vec![global, local, second],
            id_to_index: ids,
            streaming: Some(layout),
            auto_fog: None,
        };
        let mut active = env.streaming().unwrap();
        let p = [-1700., 0., 0.];
        let mut transition = FogTransition::new_streamed(&env, &mut active, p);
        assert_eq!(transition.lighting(12.).unwrap().toward_sun, [-1., 0., 0.]);
        let before = transition.lighting(12.).unwrap();
        assert_eq!(before.celestial_flags, 8); // default, NOT local's 16
        env.areas[0].celestial_flags = 16;
        transition.update_streamed_ms(&env, &mut active, p, 12., 0);
        let after = transition.lighting(12.).unwrap();
        assert_eq!(after.celestial_flags, 16); // immediate, same local key
        assert_eq!(before.diffuse, after.diffuse); // display only
        assert_eq!(before.specular, after.specular);

        assert_eq!(
            transition
                .update_streamed(&env, &mut active, p, 12., 0.)
                .1
                .start,
            10.
        );
        // Retained block refs prevent unloading an area when one block leaves.
        let mid = [-100., 0., 0.];
        let changed = transition
            .update_streamed(&env, &mut active, mid, 12., 2.5)
            .1;
        assert_eq!(changed.start, 10.);
        assert_eq!(transition.lighting(12.).unwrap().toward_sun, [-1., 0., 0.]);
        let water = transition
            .update_streamed(&env, &mut active, mid, 12., 2.5)
            .1;
        assert!((water.start - 20.).abs() < 1e-5);
        let water = transition
            .update_streamed(&env, &mut active, mid, 12., 2.5)
            .1;
        assert_eq!(water.start, 30.);
        let end = [1900., 0., 0.];
        let changed = transition
            .update_streamed(&env, &mut active, end, 12., 5.)
            .1;
        assert_eq!(changed.start, 30.);
        let water = transition
            .update_streamed(&env, &mut active, end, 12., 5.)
            .1;
        assert_eq!(water.start, 90.);

        // A region referenced only by the SECOND newly exposed block must
        // not become selectable immediately on movement. Queue draining must
        // continue even when the next frame has identical coordinates.
        let mut delayed_layout = env.streaming.clone().unwrap();
        delayed_layout.boxes[3] = vec![1];
        delayed_layout.boxes[4] = vec![1, 2];
        let delayed_env = Environments {
            areas: env.areas.clone(),
            id_to_index: env.id_to_index.clone(),
            streaming: Some(delayed_layout),
            auto_fog: None,
        };
        let mut delayed = delayed_env.streaming().unwrap();
        assert_eq!(delayed.endpoints(&delayed_env, p).key, Some(1));
        assert_eq!(delayed.endpoints(&delayed_env, mid).key, Some(1));
        assert_eq!(delayed.endpoints(&delayed_env, mid).key, Some(2));
    }
    #[test]
    fn client_clock_fog_and_baked_lighting_weight() {
        for (hour, want) in [
            (-3., 0.),
            (0., 0.),
            (3., 0.),
            (4., 0.25),
            (5., 0.5),
            (6., 0.75),
            (7., 1.),
            (12., 1.),
            (18., 1.),
            (19., 2. / 3.),
            (20., 1. / 3.),
            (21., 0.),
            (24., 0.),
            (28., 0.25),
        ] {
            assert!(
                (daylight(hour) - want).abs() < 0.00001,
                "hour {hour}: {} != {want}",
                daylight(hour)
            );
        }
        for h in [3., 7., 18., 21.] {
            assert!((daylight(h - 0.0001) - daylight(h + 0.0001)).abs() < 0.0001);
        }
    }
    fn area(
        global: bool,
        center: [f32; 3],
        flags: u32,
        direction: [f32; 3],
        fog_start: f32,
        ambient: [f32; 3],
    ) -> BoxArea {
        let fog = Fog {
            start: fog_start,
            ..Fog::default()
        };
        BoxArea {
            global,
            transition_ms: 5000,
            center,
            extents: [10000.; 3],
            axes: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            day: fog,
            night: fog,
            air_day: fog,
            air_night: fog,
            ambient_day: ambient,
            ambient_night: [0.; 3],
            diffuse_day: [0.8; 3],
            diffuse_night: [0.2; 3],
            sun_direction: direction,
            celestial_flags: flags,
        }
    }
    struct Streamed {
        env: Environments,
        active: RegionStreaming,
    }
    /// Blocks 0..4 reference a default box plus a local box. Blocks 4..8
    /// reference a second LOCAL box only, so a position there has no selected
    /// default box at all: the direction/flag retention path.
    fn streamed() -> Streamed {
        streamed_with(vec![
            vec![1, 54],
            vec![1, 54],
            vec![1, 54],
            vec![1, 54],
            vec![2],
            vec![2],
            vec![2],
            vec![2],
        ])
    }
    /// The same authored areas, but no block references the default box, so no
    /// frame selects one and nothing may read its +0xc4/+0x80 fields.
    fn streamed_locals_only() -> Streamed {
        streamed_with(
            vec![vec![54]; 4]
                .into_iter()
                .chain(vec![vec![2]; 4])
                .collect(),
        )
    }
    fn streamed_with(boxes: Vec<Vec<u32>>) -> Streamed {
        let global = area(true, [0.; 3], 8, [1., 0., 0.], 90., [0.5; 3]);
        let local = area(false, [0.; 3], 16, [0., 0., 1.], 10., [0.25; 3]);
        let second = area(false, [5000.; 3], 32, [0., 1., 0.], 30., [0.75; 3]);
        let mut ids = BTreeMap::new();
        ids.insert(1, 0);
        ids.insert(54, 1);
        ids.insert(2, 2);
        let layout = RegionLayout {
            width: 4000.,
            length: 500.,
            block_size: 500.,
            rows: 1,
            cols: 8,
            boxes,
        };
        let env = Environments {
            areas: vec![global, local, second],
            id_to_index: ids,
            streaming: Some(layout),
            auto_fog: None,
        };
        let active = env.streaming().unwrap();
        Streamed { env, active }
    }
    #[test]
    fn default_box_owns_flags_and_direction_even_for_a_local_region() {
        let mut s = streamed();
        let inside_local = [-1700., 0., 0.];
        let sel = s.active.endpoints(&s.env, inside_local);
        // Colour/fog come from the local box (54); flags/direction come from
        // the independently selected default box, not from the local one.
        assert_eq!(sel.key, Some(1));
        assert!(sel.default_present);
        assert_eq!(sel.target.celestial_flags, 8);
        assert_eq!(sel.target.sun_direction, [1., 0., 0.]);
        let mut transition = FogTransition::new_streamed(&s.env, &mut s.active, inside_local);
        let source = transition.lighting(12.).unwrap();
        assert_eq!(source.celestial_flags, 8);
        assert_eq!(source.material_direction().travel(), [1., 0., 0.]);

        // A frame that selects a local box but no default box has nothing to
        // read, so the last ratified direction/flags are retained: the second
        // local box's own +0x74/+0x04 values must never be substituted.
        let without_default = [1900., 0., 0.];
        assert_eq!(s.active.endpoints(&s.env, without_default).key, Some(2));
        assert!(!s.active.endpoints(&s.env, without_default).default_present);
        transition.update_streamed_ms(&s.env, &mut s.active, without_default, 12., 0);
        let retained = transition.lighting(12.).unwrap();
        assert_eq!(retained.celestial_flags, 8);
        assert_eq!(retained.material_direction().travel(), [1., 0., 0.]);
    }
    #[test]
    fn missing_default_box_never_invents_flags_or_direction() {
        let mut s = streamed_locals_only();
        let mut transition = FogTransition::new_streamed(&s.env, &mut s.active, [-1700., 0., 0.]);
        assert!(!transition.default_present());
        // Constructor default 0x4b2d0c, not the local box's 16.
        assert_eq!(transition.lighting(12.).unwrap().celestial_flags, 0x18);
        assert_eq!(
            transition
                .lighting(12.)
                .unwrap()
                .material_direction()
                .travel(),
            [0., -1., 0.]
        );
        // A key change without a default box keeps the retained values, even
        // though the new local box authors 32 and [0, 1, 0].
        transition.update_streamed_ms(&s.env, &mut s.active, [1900., 0., 0.], 12., 0);
        assert_eq!(transition.lighting(12.).unwrap().celestial_flags, 0x18);
        assert_eq!(
            transition
                .lighting(12.)
                .unwrap()
                .material_direction()
                .travel(),
            [0., -1., 0.]
        );
    }
    #[test]
    fn default_visibility_flags_update_independently_of_material_light() {
        let mut s = streamed();
        let p = [-1700., 0., 0.];
        let mut transition = FogTransition::new_streamed(&s.env, &mut s.active, p);
        let before = transition.lighting(12.).unwrap();
        assert_eq!(before.celestial_flags, 8);
        // Same local key, authored default flag word changes (both hidden).
        s.env.areas[0].celestial_flags = 0;
        transition.update_streamed_ms(&s.env, &mut s.active, p, 12., 0);
        let hidden = transition.lighting(12.).unwrap();
        assert_eq!(hidden.celestial_flags, 0);
        // Hiding both discs changes nothing about material light or direction.
        assert_eq!(before.diffuse, hidden.diffuse);
        assert_eq!(before.specular, hidden.specular);
        assert_eq!(before.ambient, hidden.ambient);
        assert_eq!(
            before.material_direction().travel(),
            hidden.material_direction().travel()
        );
        // The flag word is published on that same frame, not faded.
        s.env.areas[0].celestial_flags = 8;
        transition.update_streamed_ms(&s.env, &mut s.active, p, 12., 0);
        assert_eq!(transition.lighting(12.).unwrap().celestial_flags, 8);
    }
    #[test]
    fn teleport_reinitializes_and_interruption_retargets_in_flight_values() {
        let mut s = streamed();
        let start = [-1700., 0., 0.]; // local 54, default present, fog start 10
        let teleport = [1900., 0., 0.]; // local 2 only, fog start 30
        let mut transition = FogTransition::new_streamed(&s.env, &mut s.active, start);
        let (source, _) = transition.update_streamed(&s.env, &mut s.active, start, 12., 0.);
        assert!((source.start - 10.).abs() < 1e-4, "{}", source.start);

        // A fresh transition (native teleport/re-login path) starts at the
        // destination's own endpoints instead of carrying the source's values.
        let mut fresh = FogTransition::new_streamed(&s.env, &mut s.active, teleport);
        let (destination, _) = fresh.update_streamed(&s.env, &mut s.active, teleport, 12., 0.);
        assert!(
            (destination.start - 30.).abs() < 1e-4,
            "{}",
            destination.start
        );
        let mut settled = FogTransition::new_streamed(&s.env, &mut s.active, teleport);
        settled.update_streamed(&s.env, &mut s.active, teleport, 12., 60.);
        assert_eq!(
            fresh.lighting(12.).unwrap().diffuse,
            settled.lighting(12.).unwrap().diffuse
        );

        // The pre-teleport transition is retargeted: the changed-key frame only
        // installs the new targets (native 0x41fdbe skips shading), then the
        // stored steps advance from the in-flight value.
        let (installed, _) = transition.update_streamed(&s.env, &mut s.active, teleport, 12., 0.);
        assert!((installed.start - source.start).abs() < 1e-4);
        let (half, _) = transition.update_streamed(&s.env, &mut s.active, teleport, 12., 2.5);
        assert!(
            half.start > installed.start && half.start < destination.start,
            "{}",
            half.start
        );
        // Interrupt with the source before the destination completed: the new
        // step is computed from the in-flight value, not from the origin.
        let (interrupted, _) = transition.update_streamed(&s.env, &mut s.active, start, 12., 0.);
        assert_eq!(interrupted.start, half.start);
        let (reversed, _) = transition.update_streamed(&s.env, &mut s.active, start, 12., 1.);
        assert!(
            reversed.start < interrupted.start && reversed.start > source.start,
            "{}",
            reversed.start
        );
    }
    #[test]
    fn losing_the_default_box_retains_the_last_ratified_direction_and_flags() {
        let mut s = streamed();
        let p = [-1700., 0., 0.];
        let mut transition = FogTransition::new_streamed(&s.env, &mut s.active, p);
        let lit = transition.lighting(12.).unwrap();
        assert_eq!(lit.celestial_flags, 8);
        assert_eq!(lit.material_direction().travel(), [1., 0., 0.]);
        // The default box leaves the active set while its local neighbour stays.
        while s.active.map.unload(1) {}
        let sel = s.active.endpoints(&s.env, p);
        assert_eq!(sel.key, Some(1));
        assert!(!sel.default_present);
        transition.update_streamed_ms(&s.env, &mut s.active, p, 12., 0);
        let retained = transition.lighting(12.).unwrap();
        // Neither the constructor default nor the surviving local box's own
        // +0x04/+0x74 values may replace what the default last ratified.
        assert_eq!(retained.celestial_flags, 8);
        assert_eq!(retained.material_direction().travel(), [1., 0., 0.]);
    }
    #[test]
    fn time_boundaries_stay_finite_periodic_and_asymmetric() {
        let day = [0.5, 0.4, 0.3];
        let night = [0.1, 0.2, 0.3];
        for hour in [
            -48., -24., -0.001, 0., 3., 3.5, 7., 12., 18., 18.75, 20.25, 21., 23.999, 24., 48.,
        ] {
            let c = client_regional_light::evaluate(hour, day, night);
            assert!(c.diffuse.iter().chain(&c.specular).all(|v| v.is_finite()));
            for i in 0..3 {
                assert!(c.specular[i] <= c.diffuse[i] + 1e-6, "hour {hour}");
            }
        }
        // Sun pitch > 0 selects sun time even when the night factor is zero at
        // both endpoints; the asymmetric sunrise is preserved elsewhere.
        assert!(client_regional_light::evaluate(12., [1.; 3], [0.; 3]).diffuse[0] > 0.);
        assert_eq!(
            client_regional_light::evaluate(0., [1.; 3], [0.; 3]).diffuse,
            [0.; 3]
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let c = client_regional_light::evaluate(bad, day, night);
            assert!(c.diffuse.iter().chain(&c.specular).all(|v| v.is_finite()));
            assert_eq!(
                c.diffuse,
                client_regional_light::evaluate(0., day, night).diffuse
            );
        }
        // Ambient quantization is independent of the directional branch.
        let nf = night_factor(5.);
        let channel = (0.5 * 255.) as u8 as f32 * f32::from_bits(0x3b808081);
        assert_eq!(
            client_regional_light::ambient([1.; 3], [0.; 3], nf),
            [channel; 3]
        );
    }
    #[test]
    fn truncated_rejected() {
        assert!(BoxArea::parse(&[0; 30], 0).is_err())
    }
    #[test]
    fn material_direction_matches_the_previous_renderer_normalization() {
        let l = RegionalLight {
            ambient: [0.; 3],
            diffuse: [0.; 3],
            specular: [0.; 3],
            toward_sun: [0., 3., 4.],
            celestial_flags: 0,
        };
        assert_eq!(l.material_direction().travel(), [0., -0.6, -0.8]);
        // The disc azimuth must stay on the opposite side from light travel.
        assert_eq!(l.material_direction().toward_sun(), [0., 0.6, 0.8]);
        let zero = RegionalLight {
            toward_sun: [0.; 3],
            ..l
        };
        assert_eq!(zero.material_direction().travel(), [0.; 3]);
        let huge = RegionalLight {
            toward_sun: [f32::MAX; 3],
            ..l
        };
        assert_eq!(huge.material_direction().travel(), [0.; 3]);
    }
    #[test]
    fn a_region_crossing_moves_the_sky_target_with_the_fog_key() {
        // P5.07. The sky pair is selected from the same `Selection` the fog
        // channels read: `0x420992` extracts the SUN/MOON bits from the
        // independently selected default area on every render and `0x79b390`
        // refreshes the direction on a region install, so the sky target can
        // never name a different box from the one the fog transition adopted.
        fn area(center: [f32; 3], start: f32, flags: u32) -> BoxArea {
            let fog = Fog {
                start,
                ..Fog::default()
            };
            BoxArea {
                global: false,
                transition_ms: 1000,
                center,
                extents: [1000.; 3],
                axes: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                day: fog,
                night: fog,
                air_day: fog,
                air_night: fog,
                ambient_day: [1.; 3],
                ambient_night: [0.; 3],
                diffuse_day: [1.; 3],
                diffuse_night: [0.; 3],
                sun_direction: [0., -1., 0.],
                celestial_flags: flags,
            }
        }
        let mut global = area([0.; 3], 90., 8);
        global.global = true;
        let env = Environments {
            areas: vec![
                global,
                area([-1500., 0., 0.], 10., 0x18),
                area([1500., 0., 0.], 30., 0x08),
            ],
            id_to_index: [(1u32, 0usize), (54, 1), (2, 2)].into_iter().collect(),
            streaming: Some(RegionLayout {
                width: 4000.,
                length: 500.,
                block_size: 500.,
                rows: 1,
                cols: 8,
                boxes: vec![
                    vec![1],
                    vec![1, 54],
                    vec![1, 54],
                    vec![1, 2],
                    vec![1, 2],
                    vec![1],
                    vec![1],
                    vec![1],
                ],
            }),
            auto_fog: None,
        };
        let mut active = env.streaming().unwrap();
        let mut transition = FogTransition::new_streamed(&env, &mut active, [-1700., 0., 0.]);
        // Walk across the whole strip, sampling as the renderer does.
        for i in 0..40 {
            let p = [-1900. + i as f32 * 100., 0., 0.];
            let sel = active.endpoints(&env, p);
            let (sky_key, sky_flags) = sky_selection(&sel);
            assert_eq!(sky_key, sel.key, "sky target vs fog key at {p:?}");
            assert_eq!(sky_flags, sel.target.celestial_flags);
            transition.update_streamed_ms(&env, &mut active, p, 12., 16);
            let lit = transition.lighting(12.).unwrap();
            // The published flag word always belongs to a box that is either
            // the local one or the selected default; never an unrelated third.
            assert!(
                lit.celestial_flags == sel.target.celestial_flags
                    || lit.celestial_flags == env.areas[0].celestial_flags,
                "flags {:#x} at {p:?} belong to no selected box",
                lit.celestial_flags
            );
        }
    }
}
