//! Editable world data model, terrain sculpting, object transforms, snapping,
//! undo/redo history, and custom `.pwarch` world persistence for Architect.

use glam::{EulerRot, Mat4, Quat, Vec3};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Default terrain cells per side (4 x 4 T2BK blocks of 32 cells = 128 cells).
pub const DEFAULT_TERRAIN_CELLS: usize = 128;
/// Default world-unit spacing between adjacent terrain vertices.
pub const DEFAULT_GRID_SPACING: f32 = 2.0;

/// Surface material/paint layer painted onto terrain vertices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PaintLayer {
    Grass = 0,
    Dirt = 1,
    Rock = 2,
    Sand = 3,
    Snow = 4,
    Path = 5,
}

impl PaintLayer {
    pub const ALL: [Self; 6] = [
        Self::Grass,
        Self::Dirt,
        Self::Rock,
        Self::Sand,
        Self::Snow,
        Self::Path,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Grass => "Grass",
            Self::Dirt => "Dirt",
            Self::Rock => "Rock",
            Self::Sand => "Sand",
            Self::Snow => "Snow",
            Self::Path => "Stone Path",
        }
    }

    pub fn color(self) -> [f32; 3] {
        match self {
            Self::Grass => [0.34, 0.50, 0.28],
            Self::Dirt => [0.46, 0.36, 0.25],
            Self::Rock => [0.45, 0.46, 0.48],
            Self::Sand => [0.74, 0.67, 0.48],
            Self::Snow => [0.88, 0.91, 0.95],
            Self::Path => [0.56, 0.53, 0.48],
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Dirt,
            2 => Self::Rock,
            3 => Self::Sand,
            4 => Self::Snow,
            5 => Self::Path,
            _ => Self::Grass,
        }
    }
}

/// Terrain sculpting / painting brush mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerrainBrushMode {
    Raise,
    Lower,
    Flatten,
    Smooth,
    Noise,
    Level,
    Paint,
}

impl TerrainBrushMode {
    pub const ALL: [Self; 7] = [
        Self::Raise,
        Self::Lower,
        Self::Flatten,
        Self::Smooth,
        Self::Noise,
        Self::Level,
        Self::Paint,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Raise => "Raise",
            Self::Lower => "Lower",
            Self::Flatten => "Flatten",
            Self::Smooth => "Smooth",
            Self::Noise => "Noise",
            Self::Level => "Level",
            Self::Paint => "Paint",
        }
    }
}

/// Configurable brush parameters for terrain sculpting/painting.
#[derive(Clone, Copy, Debug)]
pub struct TerrainBrush {
    pub mode: TerrainBrushMode,
    pub radius: f32,
    pub strength: f32,
    pub falloff: f32,
    pub target_height: f32,
    pub paint_layer: PaintLayer,
}

impl Default for TerrainBrush {
    fn default() -> Self {
        Self {
            mode: TerrainBrushMode::Raise,
            radius: 14.0,
            strength: 12.0,
            falloff: 0.65,
            target_height: 0.0,
            paint_layer: PaintLayer::Grass,
        }
    }
}

/// Editable heightmap + per-vertex paint layer grid.
///
/// The grid spans `[-width_world/2, +width_world/2]` on X and Z so world
/// coordinates match Perfect World's centered map space.
#[derive(Clone, Debug)]
pub struct EditableTerrain {
    /// Number of cells along X (must be a multiple of 32 for native export).
    pub cells_x: usize,
    /// Number of cells along Z (must be a multiple of 32 for native export).
    pub cells_z: usize,
    /// World units per cell.
    pub spacing: f32,
    /// Vertex heights of length `(cells_x + 1) * (cells_z + 1)`.
    pub heights: Vec<f32>,
    /// Per-vertex surface paint layer (`PaintLayer as u8`).
    pub paint_layers: Vec<u8>,
    /// Optional global water plane height.
    pub water_level: Option<f32>,
    /// Monotonically increasing revision counter so the GPU mesh knows when to rebuild.
    pub revision: u64,
}

impl EditableTerrain {
    pub fn new_flat(cells_x: usize, cells_z: usize, spacing: f32) -> Self {
        let vx = cells_x + 1;
        let vz = cells_z + 1;
        let count = vx * vz;
        Self {
            cells_x,
            cells_z,
            spacing,
            heights: vec![0.0; count],
            paint_layers: vec![PaintLayer::Grass as u8; count],
            water_level: None,
            revision: 1,
        }
    }

    pub fn default_empty() -> Self {
        Self::new_flat(
            DEFAULT_TERRAIN_CELLS,
            DEFAULT_TERRAIN_CELLS,
            DEFAULT_GRID_SPACING,
        )
    }

    pub fn verts_x(&self) -> usize {
        self.cells_x + 1
    }

    pub fn verts_z(&self) -> usize {
        self.cells_z + 1
    }

    pub fn world_width(&self) -> f32 {
        self.cells_x as f32 * self.spacing
    }

    pub fn world_length(&self) -> f32 {
        self.cells_z as f32 * self.spacing
    }

    pub fn half_width(&self) -> f32 {
        self.world_width() * 0.5
    }

    pub fn half_length(&self) -> f32 {
        self.world_length() * 0.5
    }

    pub fn vertex_world_xz(&self, ix: usize, iz: usize) -> (f32, f32) {
        let x = ix as f32 * self.spacing - self.half_width();
        let z = iz as f32 * self.spacing - self.half_length();
        (x, z)
    }

    pub fn height_at_vertex(&self, ix: usize, iz: usize) -> f32 {
        let ix = ix.min(self.cells_x);
        let iz = iz.min(self.cells_z);
        self.heights[iz * self.verts_x() + ix]
    }

    /// Bilinearly sample terrain height at arbitrary world `(x, z)`.
    pub fn sample_height(&self, x: f32, z: f32) -> f32 {
        let fx = ((x + self.half_width()) / self.spacing).clamp(0.0, self.cells_x as f32);
        let fz = ((z + self.half_length()) / self.spacing).clamp(0.0, self.cells_z as f32);
        let x0 = (fx.floor() as usize).min(self.cells_x.saturating_sub(1));
        let z0 = (fz.floor() as usize).min(self.cells_z.saturating_sub(1));
        let x1 = (x0 + 1).min(self.cells_x);
        let z1 = (z0 + 1).min(self.cells_z);
        let tx = (fx - x0 as f32).clamp(0.0, 1.0);
        let tz = (fz - z0 as f32).clamp(0.0, 1.0);
        let h00 = self.height_at_vertex(x0, z0);
        let h10 = self.height_at_vertex(x1, z0);
        let h01 = self.height_at_vertex(x0, z1);
        let h11 = self.height_at_vertex(x1, z1);
        let h0 = h00 + (h10 - h00) * tx;
        let h1 = h01 + (h11 - h01) * tx;
        h0 + (h1 - h0) * tz
    }

    /// Central-difference surface normal at world `(x, z)`.
    pub fn sample_normal(&self, x: f32, z: f32) -> Vec3 {
        let eps = self.spacing.max(0.5);
        let hl = self.sample_height(x - eps, z);
        let hr = self.sample_height(x + eps, z);
        let hd = self.sample_height(x, z - eps);
        let hu = self.sample_height(x, z + eps);
        Vec3::new(hl - hr, 2.0 * eps, hd - hu).normalize_or_zero()
    }

    /// Normal at a specific vertex index.
    pub fn vertex_normal(&self, ix: usize, iz: usize) -> Vec3 {
        let xl = ix.saturating_sub(1);
        let xr = (ix + 1).min(self.cells_x);
        let zd = iz.saturating_sub(1);
        let zu = (iz + 1).min(self.cells_z);
        let dx = (xr - xl).max(1) as f32 * self.spacing;
        let dz = (zu - zd).max(1) as f32 * self.spacing;
        let dhx = (self.height_at_vertex(xr, iz) - self.height_at_vertex(xl, iz)) / dx;
        let dhz = (self.height_at_vertex(ix, zu) - self.height_at_vertex(ix, zd)) / dz;
        Vec3::new(-dhx, 1.0, -dhz).normalize_or_zero()
    }

    /// Apply a terrain brush stroke centered at `(center_x, center_z)`.
    /// Returns true if any vertex changed.
    pub fn apply_brush(
        &mut self,
        center_x: f32,
        center_z: f32,
        brush: &TerrainBrush,
        dt: f32,
    ) -> bool {
        let radius = brush.radius.max(0.5);
        let r2 = radius * radius;
        let min_x = (((center_x - radius + self.half_width()) / self.spacing).floor() as isize)
            .clamp(0, self.cells_x as isize) as usize;
        let max_x = (((center_x + radius + self.half_width()) / self.spacing).ceil() as isize)
            .clamp(0, self.cells_x as isize) as usize;
        let min_z = (((center_z - radius + self.half_length()) / self.spacing).floor() as isize)
            .clamp(0, self.cells_z as isize) as usize;
        let max_z = (((center_z + radius + self.half_length()) / self.spacing).ceil() as isize)
            .clamp(0, self.cells_z as isize) as usize;

        if min_x > max_x || min_z > max_z {
            return false;
        }

        let vx = self.verts_x();
        let dt_clamped = dt.clamp(0.004, 0.1);
        let snapshot = if brush.mode == TerrainBrushMode::Smooth {
            Some(self.heights.clone())
        } else {
            None
        };
        let mut changed = false;

        for iz in min_z..=max_z {
            for ix in min_x..=max_x {
                let (wx, wz) = self.vertex_world_xz(ix, iz);
                let dx = wx - center_x;
                let dz = wz - center_z;
                let d2 = dx * dx + dz * dz;
                if d2 > r2 {
                    continue;
                }
                let t = (d2.sqrt() / radius).clamp(0.0, 1.0);
                let falloff_exp = 0.5 + brush.falloff.clamp(0.0, 1.0) * 2.5;
                let weight = (1.0 - t).powf(falloff_exp);
                if weight <= 1e-4 {
                    continue;
                }

                let idx = iz * vx + ix;
                match brush.mode {
                    TerrainBrushMode::Raise => {
                        self.heights[idx] = (self.heights[idx]
                            + brush.strength * weight * dt_clamped)
                            .clamp(-500.0, 1500.0);
                        changed = true;
                    }
                    TerrainBrushMode::Lower => {
                        self.heights[idx] = (self.heights[idx]
                            - brush.strength * weight * dt_clamped)
                            .clamp(-500.0, 1500.0);
                        changed = true;
                    }
                    TerrainBrushMode::Flatten | TerrainBrushMode::Level => {
                        let target = if brush.mode == TerrainBrushMode::Level {
                            brush.target_height
                        } else {
                            brush.target_height
                        };
                        let alpha = (brush.strength * 0.25 * weight * dt_clamped).clamp(0.0, 1.0);
                        self.heights[idx] += (target - self.heights[idx]) * alpha;
                        changed = true;
                    }
                    TerrainBrushMode::Smooth => {
                        if let Some(src) = snapshot.as_ref() {
                            let mut sum = 0.0f32;
                            let mut count = 0.0f32;
                            for oz in -1isize..=1 {
                                for ox in -1isize..=1 {
                                    let nx =
                                        (ix as isize + ox).clamp(0, self.cells_x as isize) as usize;
                                    let nz =
                                        (iz as isize + oz).clamp(0, self.cells_z as isize) as usize;
                                    let k = if ox == 0 && oz == 0 { 2.0 } else { 1.0 };
                                    sum += src[nz * vx + nx] * k;
                                    count += k;
                                }
                            }
                            let avg = sum / count;
                            let alpha =
                                (brush.strength * 0.35 * weight * dt_clamped).clamp(0.0, 1.0);
                            self.heights[idx] += (avg - self.heights[idx]) * alpha;
                            changed = true;
                        }
                    }
                    TerrainBrushMode::Noise => {
                        // Deterministic pseudo-noise from world coordinates so strokes feel organic.
                        let h = ((wx * 0.37).sin() * (wz * 0.41).cos()
                            + (wx * 0.13 + wz * 0.19).sin() * 0.5)
                            * 0.66;
                        self.heights[idx] = (self.heights[idx]
                            + h * brush.strength * weight * dt_clamped)
                            .clamp(-500.0, 1500.0);
                        changed = true;
                    }
                    TerrainBrushMode::Paint => {
                        if weight >= 0.2 {
                            let new_layer = brush.paint_layer as u8;
                            if self.paint_layers[idx] != new_layer {
                                self.paint_layers[idx] = new_layer;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Ray-terrain intersection by marching along the ray and refining with binary search.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Vec3> {
        let dir = dir.normalize_or_zero();
        if dir.length_squared() < 0.5 {
            return None;
        }
        let step = (self.spacing * 0.75).max(1.0);
        let mut t = 0.0f32;
        let mut prev_t = 0.0f32;
        let mut prev_above = origin.y - self.sample_height(origin.x, origin.z);
        let hw = self.half_width();
        let hl = self.half_length();

        while t <= max_dist {
            let p = origin + dir * t;
            if p.x >= -hw && p.x <= hw && p.z >= -hl && p.z <= hl {
                let ground = self.sample_height(p.x, p.z);
                let above = p.y - ground;
                if above <= 0.0 && prev_above >= 0.0 {
                    // Binary refine between prev_t and t.
                    let mut lo = prev_t;
                    let mut hi = t;
                    for _ in 0..10 {
                        let mid = 0.5 * (lo + hi);
                        let mp = origin + dir * mid;
                        if mp.y - self.sample_height(mp.x, mp.z) > 0.0 {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let hit = origin + dir * (0.5 * (lo + hi));
                    let y = self.sample_height(hit.x, hit.z);
                    return Some(Vec3::new(hit.x, y, hit.z));
                }
                prev_above = above;
            }
            prev_t = t;
            t += step;
        }

        // Fallback: intersect y = 0 plane if camera looks outside or terrain is flat.
        if dir.y.abs() > 1e-4 {
            let plane_t = -origin.y / dir.y;
            if plane_t > 0.0 && plane_t <= max_dist {
                let p = origin + dir * plane_t;
                if p.x.abs() <= hw && p.z.abs() <= hl {
                    return Some(Vec3::new(p.x, self.sample_height(p.x, p.z), p.z));
                }
            }
        }
        None
    }

    pub fn reset_flat(&mut self) {
        self.heights.fill(0.0);
        self.paint_layers.fill(PaintLayer::Grass as u8);
        self.water_level = None;
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Classification of any placeable object in Architect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ObjectKind {
    BuildingBmd = 0,
    ModelEcm = 1,
    Tree = 2,
    EffectGfx = 3,
    SoundSfx = 4,
    Grass = 5,
    Water = 6,
    Light = 7,
    Primitive = 8,
}

impl ObjectKind {
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        if lower.starts_with("builtin://") {
            if lower.contains("tree") {
                return Self::Tree;
            }
            if lower.contains("light") {
                return Self::Light;
            }
            if lower.contains("water") {
                return Self::Water;
            }
            return Self::Primitive;
        }
        if lower.ends_with(".bmd") {
            Self::BuildingBmd
        } else if lower.ends_with(".ecm") || lower.ends_with(".smd") || lower.ends_with(".ski") {
            Self::ModelEcm
        } else if lower.ends_with(".spt") {
            Self::Tree
        } else if lower.ends_with(".gfx") {
            Self::EffectGfx
        } else if lower.ends_with(".wav") || lower.ends_with(".mp3") || lower.ends_with(".ogg") {
            Self::SoundSfx
        } else if lower.ends_with(".grs") {
            Self::Grass
        } else {
            Self::Primitive
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::BuildingBmd,
            1 => Self::ModelEcm,
            2 => Self::Tree,
            3 => Self::EffectGfx,
            4 => Self::SoundSfx,
            5 => Self::Grass,
            6 => Self::Water,
            7 => Self::Light,
            _ => Self::Primitive,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::BuildingBmd => "Building (BMD)",
            Self::ModelEcm => "Model (ECM)",
            Self::Tree => "Tree (SPT)",
            Self::EffectGfx => "Effect (GFX)",
            Self::SoundSfx => "Sound (SFX)",
            Self::Grass => "Grass",
            Self::Water => "Water",
            Self::Light => "Light",
            Self::Primitive => "Primitive",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::BuildingBmd => "BMD",
            Self::ModelEcm => "ECM",
            Self::Tree => "TREE",
            Self::EffectGfx => "GFX",
            Self::SoundSfx => "SFX",
            Self::Grass => "GRS",
            Self::Water => "WTR",
            Self::Light => "LGT",
            Self::Primitive => "PRM",
        }
    }

    pub fn default_color(self) -> [f32; 3] {
        match self {
            Self::BuildingBmd => [0.76, 0.72, 0.65],
            Self::ModelEcm => [0.45, 0.72, 0.92],
            Self::Tree => [0.26, 0.60, 0.30],
            Self::EffectGfx => [0.95, 0.52, 0.22],
            Self::SoundSfx => [0.68, 0.45, 0.90],
            Self::Grass => [0.42, 0.72, 0.34],
            Self::Water => [0.22, 0.56, 0.85],
            Self::Light => [0.98, 0.90, 0.40],
            Self::Primitive => [0.66, 0.69, 0.74],
        }
    }

    pub fn default_bounds_half(self) -> [f32; 3] {
        match self {
            Self::BuildingBmd => [3.0, 3.0, 3.0],
            Self::ModelEcm => [1.0, 1.5, 1.0],
            Self::Tree => [1.8, 4.0, 1.8],
            Self::EffectGfx => [1.0, 1.0, 1.0],
            Self::SoundSfx => [0.9, 0.9, 0.9],
            Self::Grass => [1.2, 0.6, 1.2],
            Self::Water => [8.0, 0.2, 8.0],
            Self::Light => [0.7, 0.7, 0.7],
            Self::Primitive => [1.5, 1.5, 1.5],
        }
    }
}

/// One placed object inside an Architect world.
#[derive(Clone, Debug)]
pub struct PlacedObject {
    pub id: u64,
    pub name: String,
    pub asset_path: String,
    pub kind: ObjectKind,
    pub position: [f32; 3],
    pub rotation_deg: [f32; 3],
    pub scale: [f32; 3],
    pub color_tint: [f32; 3],
    pub bounds_half: [f32; 3],
    pub visible: bool,
    pub locked: bool,
}

impl PlacedObject {
    pub fn new(id: u64, asset_path: String, position: Vec3) -> Self {
        let kind = ObjectKind::from_path(&asset_path);
        let name = asset_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&asset_path)
            .to_string();
        Self {
            id,
            name,
            asset_path,
            kind,
            position: position.to_array(),
            rotation_deg: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            color_tint: kind.default_color(),
            bounds_half: kind.default_bounds_half(),
            visible: true,
            locked: false,
        }
    }

    pub fn rotation_quat(&self) -> Quat {
        Quat::from_euler(
            EulerRot::YXZ,
            self.rotation_deg[1].to_radians(),
            self.rotation_deg[0].to_radians(),
            self.rotation_deg[2].to_radians(),
        )
    }

    pub fn model_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            Vec3::from(self.scale),
            self.rotation_quat(),
            Vec3::from(self.position),
        )
    }

    /// World-space forward (`direction`) and `up` unit vectors for ECWLD/ECBSD export.
    pub fn direction_and_up(&self) -> ([f32; 3], [f32; 3]) {
        let q = self.rotation_quat();
        let dir = (q * Vec3::Z).normalize_or_zero();
        let up = (q * Vec3::Y).normalize_or_zero();
        (
            if dir.length_squared() < 0.5 {
                [0.0, 0.0, 1.0]
            } else {
                dir.to_array()
            },
            if up.length_squared() < 0.5 {
                [0.0, 1.0, 0.0]
            } else {
                up.to_array()
            },
        )
    }

    /// Approximate world-space AABB `(min, max)` used for ray picking, box select, and object snapping.
    pub fn world_aabb(&self) -> (Vec3, Vec3) {
        let half = Vec3::new(
            self.bounds_half[0].abs() * self.scale[0].abs().max(0.1),
            self.bounds_half[1].abs() * self.scale[1].abs().max(0.1),
            self.bounds_half[2].abs() * self.scale[2].abs().max(0.1),
        );
        // Position is placed at the object's base (bottom center) so dropping to terrain sits cleanly.
        let center = Vec3::new(
            self.position[0],
            self.position[1] + half.y,
            self.position[2],
        );
        (center - half, center + half)
    }

    pub fn ray_intersect(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        if !self.visible {
            return None;
        }
        let (min, max) = self.world_aabb();
        ray_aabb_distance(origin, dir, min, max)
    }
}

/// Slab test for ray vs AABB. Returns nearest positive hit distance.
pub fn ray_aabb_distance(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut tmin = 0.0f32;
    let mut tmax = f32::INFINITY;
    for axis in 0..3 {
        let o = origin[axis];
        let d = dir[axis];
        if d.abs() < 1e-6 {
            if o < min[axis] || o > max[axis] {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (min[axis] - o) * inv;
            let mut t2 = (max[axis] - o) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
            if tmin > tmax {
                return None;
            }
        }
    }
    if tmax >= 0.0 {
        Some(tmin.max(0.0))
    } else {
        None
    }
}

/// Snapping behavior when placing or moving objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnappingMode {
    Off,
    Grid,
    Terrain,
    Object,
    Smart,
}

impl SnappingMode {
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::Grid,
        Self::Terrain,
        Self::Object,
        Self::Smart,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Grid => "Grid",
            Self::Terrain => "To Terrain",
            Self::Object => "To Object",
            Self::Smart => "Smart",
        }
    }
}

/// Compute snapped placement/move position for an object.
pub fn snap_position(
    raw: Vec3,
    mode: SnappingMode,
    grid_step: f32,
    terrain: &EditableTerrain,
    objects: &[PlacedObject],
    ignore_ids: &HashSet<u64>,
) -> Vec3 {
    let step = grid_step.max(0.1);
    let snap_grid = |v: f32| (v / step).round() * step;

    let object_top_under = |x: f32, z: f32, ref_y: f32| -> Option<f32> {
        let mut best: Option<f32> = None;
        for obj in objects {
            if !obj.visible || ignore_ids.contains(&obj.id) {
                continue;
            }
            let (min, max) = obj.world_aabb();
            if x >= min.x && x <= max.x && z >= min.z && z <= max.z {
                let top = max.y;
                if best.map_or(true, |b| (top - ref_y).abs() < (b - ref_y).abs()) {
                    best = Some(top);
                }
            }
        }
        best
    };

    match mode {
        SnappingMode::Off => raw,
        SnappingMode::Grid => Vec3::new(snap_grid(raw.x), snap_grid(raw.y), snap_grid(raw.z)),
        SnappingMode::Terrain => Vec3::new(raw.x, terrain.sample_height(raw.x, raw.z), raw.z),
        SnappingMode::Object => {
            if let Some(top) = object_top_under(raw.x, raw.z, raw.y) {
                Vec3::new(raw.x, top, raw.z)
            } else {
                raw
            }
        }
        SnappingMode::Smart => {
            let sx = snap_grid(raw.x);
            let sz = snap_grid(raw.z);
            let y =
                object_top_under(sx, sz, raw.y).unwrap_or_else(|| terrain.sample_height(sx, sz));
            Vec3::new(sx, y, sz)
        }
    }
}

/// Snapshot stored in the Undo/Redo history.
#[derive(Clone)]
struct Snapshot {
    heights: Vec<f32>,
    paint_layers: Vec<u8>,
    water_level: Option<f32>,
    objects: Vec<PlacedObject>,
}

const MAX_UNDO_STEPS: usize = 40;

/// Full editable world document owned by Architect.
pub struct WorldDocument {
    pub name: String,
    pub source_map_id: Option<String>,
    pub terrain: EditableTerrain,
    pub objects: Vec<PlacedObject>,
    pub next_object_id: u64,
    pub dirty: bool,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
}

impl WorldDocument {
    pub fn new_empty() -> Self {
        Self {
            name: "Untitled World".into(),
            source_map_id: None,
            terrain: EditableTerrain::default_empty(),
            objects: Vec::new(),
            next_object_id: 1,
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn from_parts(
        name: String,
        source_map_id: Option<String>,
        terrain: EditableTerrain,
        objects: Vec<PlacedObject>,
        next_object_id: u64,
    ) -> Self {
        Self {
            name,
            source_map_id,
            terrain,
            objects,
            next_object_id,
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn capture(&self) -> Snapshot {
        Snapshot {
            heights: self.terrain.heights.clone(),
            paint_layers: self.terrain.paint_layers.clone(),
            water_level: self.terrain.water_level,
            objects: self.objects.clone(),
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.terrain.heights = snap.heights;
        self.terrain.paint_layers = snap.paint_layers;
        self.terrain.water_level = snap.water_level;
        self.terrain.revision = self.terrain.revision.wrapping_add(1);
        self.objects = snap.objects;
        self.next_object_id = self
            .objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.dirty = true;
    }

    pub fn push_undo(&mut self) {
        self.undo_stack.push(self.capture());
        if self.undo_stack.len() > MAX_UNDO_STEPS {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.capture());
            self.restore(prev);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.capture());
            self.restore(next);
            true
        } else {
            false
        }
    }

    pub fn add_object(&mut self, asset_path: String, position: Vec3) -> u64 {
        self.push_undo();
        let id = self.next_object_id;
        self.next_object_id += 1;
        self.objects
            .push(PlacedObject::new(id, asset_path, position));
        id
    }

    pub fn duplicate_objects(&mut self, ids: &HashSet<u64>) -> Vec<u64> {
        let clones: Vec<PlacedObject> = self
            .objects
            .iter()
            .filter(|o| ids.contains(&o.id))
            .cloned()
            .collect();
        if clones.is_empty() {
            return Vec::new();
        }
        self.push_undo();
        let mut created = Vec::with_capacity(clones.len());
        for mut obj in clones {
            let id = self.next_object_id;
            self.next_object_id += 1;
            obj.id = id;
            obj.position[0] += 2.0;
            obj.position[2] += 2.0;
            self.objects.push(obj);
            created.push(id);
        }
        created
    }

    pub fn delete_objects(&mut self, ids: &HashSet<u64>) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let before = self.objects.len();
        if !self
            .objects
            .iter()
            .any(|o| ids.contains(&o.id) && !o.locked)
        {
            return 0;
        }
        self.push_undo();
        self.objects.retain(|o| !ids.contains(&o.id) || o.locked);
        before - self.objects.len()
    }

    pub fn erase_everything(&mut self) {
        self.push_undo();
        self.terrain.reset_flat();
        self.objects.clear();
        self.source_map_id = None;
    }

    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.dirty = false;
    }
}

// ---------------------------------------------------------------------------
// Custom World Persistence (.pwarch)
// ---------------------------------------------------------------------------

const PWARCH_MAGIC: &[u8; 8] = b"PWARCH01";

/// Directory where Architect saves user-created custom worlds.
pub fn custom_worlds_dir(element: &Path) -> PathBuf {
    if element.as_os_str().is_empty() {
        PathBuf::from("architect_worlds")
    } else {
        element.join("architect_worlds")
    }
}

/// Sanitize a user-provided custom world name into a safe file/map stem.
pub fn sanitize_world_slug(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            slug.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() {
            if !slug.ends_with('_') && !slug.is_empty() {
                slug.push('_');
            }
        }
    }
    let trimmed = slug.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "custom_world".into()
    } else {
        trimmed.chars().take(48).collect()
    }
}

/// Summary of one saved custom `.pwarch` world on disk.
#[derive(Clone, Debug)]
pub struct SavedCustomWorld {
    pub slug: String,
    pub display_name: String,
    pub path: PathBuf,
    pub object_count: usize,
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn read_str(data: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = read_u32(data, pos)? as usize;
    if len > 16_384 {
        return Err("oversized string in custom world file".into());
    }
    let end = pos.checked_add(len).ok_or("string offset overflow")?;
    let bytes = data.get(*pos..end).ok_or("truncated string")?;
    *pos = end;
    String::from_utf8(bytes.to_vec()).map_err(|e| format!("invalid UTF-8: {e}"))
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    let end = pos.checked_add(4).ok_or("overflow")?;
    let slice = data.get(*pos..end).ok_or("truncated u32")?;
    *pos = end;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_f32(data: &[u8], pos: &mut usize) -> Result<f32, String> {
    let v = f32::from_bits(read_u32(data, pos)?);
    if !v.is_finite() {
        return Err("non-finite float in custom world file".into());
    }
    Ok(v)
}

pub fn serialize_custom_world(doc: &WorldDocument) -> Vec<u8> {
    let mut out = Vec::with_capacity(doc.terrain.heights.len() * 5 + doc.objects.len() * 128 + 256);
    out.extend_from_slice(PWARCH_MAGIC);
    write_str(&mut out, &doc.name);
    out.extend_from_slice(&(doc.terrain.cells_x as u32).to_le_bytes());
    out.extend_from_slice(&(doc.terrain.cells_z as u32).to_le_bytes());
    out.extend_from_slice(&doc.terrain.spacing.to_le_bytes());
    match doc.terrain.water_level {
        Some(level) => {
            out.push(1);
            out.extend_from_slice(&level.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend_from_slice(&0.0f32.to_le_bytes());
        }
    }
    for &h in &doc.terrain.heights {
        out.extend_from_slice(&h.to_le_bytes());
    }
    out.extend_from_slice(&doc.terrain.paint_layers);
    out.extend_from_slice(&(doc.objects.len() as u32).to_le_bytes());
    for obj in &doc.objects {
        out.extend_from_slice(&obj.id.to_le_bytes());
        write_str(&mut out, &obj.name);
        write_str(&mut out, &obj.asset_path);
        out.push(obj.kind as u8);
        for v in obj.position {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in obj.rotation_deg {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in obj.scale {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in obj.color_tint {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in obj.bounds_half {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(u8::from(obj.visible));
        out.push(u8::from(obj.locked));
    }
    out
}

pub fn deserialize_custom_world(data: &[u8]) -> Result<WorldDocument, String> {
    if data.len() < 8 || &data[0..8] != PWARCH_MAGIC {
        return Err("not a valid Architect .pwarch world file".into());
    }
    let mut pos = 8;
    let name = read_str(data, &mut pos)?;
    let cells_x = read_u32(data, &mut pos)? as usize;
    let cells_z = read_u32(data, &mut pos)? as usize;
    let spacing = read_f32(data, &mut pos)?;
    if cells_x == 0 || cells_z == 0 || cells_x > 1024 || cells_z > 1024 || spacing <= 0.0 {
        return Err("invalid terrain dimensions in custom world".into());
    }
    let has_water = *data.get(pos).ok_or("truncated water flag")? != 0;
    pos += 1;
    let water_val = read_f32(data, &mut pos)?;
    let water_level = has_water.then_some(water_val);

    let count = (cells_x + 1) * (cells_z + 1);
    let mut heights = Vec::with_capacity(count);
    for _ in 0..count {
        heights.push(read_f32(data, &mut pos)?);
    }
    let paint_end = pos.checked_add(count).ok_or("paint overflow")?;
    let paint_layers = data
        .get(pos..paint_end)
        .ok_or("truncated paint layers")?
        .to_vec();
    pos = paint_end;

    let obj_count = read_u32(data, &mut pos)? as usize;
    if obj_count > 500_000 {
        return Err("too many objects in custom world".into());
    }
    let mut objects = Vec::with_capacity(obj_count.min(4096));
    let mut max_id = 0u64;
    for _ in 0..obj_count {
        let id_bytes = data.get(pos..pos + 8).ok_or("truncated object id")?;
        let id = u64::from_le_bytes(id_bytes.try_into().unwrap());
        pos += 8;
        max_id = max_id.max(id);
        let obj_name = read_str(data, &mut pos)?;
        let asset_path = read_str(data, &mut pos)?;
        let kind = ObjectKind::from_u8(*data.get(pos).ok_or("truncated kind")?);
        pos += 1;
        let position = [
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
        ];
        let rotation_deg = [
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
        ];
        let scale = [
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
        ];
        let color_tint = [
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
        ];
        let bounds_half = [
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
            read_f32(data, &mut pos)?,
        ];
        let visible = *data.get(pos).ok_or("truncated visible")? != 0;
        let locked = *data.get(pos + 1).ok_or("truncated locked")? != 0;
        pos += 2;
        objects.push(PlacedObject {
            id,
            name: obj_name,
            asset_path,
            kind,
            position,
            rotation_deg,
            scale,
            color_tint,
            bounds_half,
            visible,
            locked,
        });
    }

    Ok(WorldDocument {
        name,
        source_map_id: None,
        terrain: EditableTerrain {
            cells_x,
            cells_z,
            spacing,
            heights,
            paint_layers,
            water_level,
            revision: 1,
        },
        objects,
        next_object_id: max_id.saturating_add(1),
        dirty: false,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
    })
}

pub fn save_custom_world(
    element: &Path,
    doc: &mut WorldDocument,
    name: &str,
) -> Result<PathBuf, String> {
    let dir = custom_worlds_dir(element);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        doc.name = trimmed.to_string();
    }
    let slug = sanitize_world_slug(&doc.name);
    let path = dir.join(format!("{slug}.pwarch"));
    let bytes = serialize_custom_world(doc);
    fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    doc.dirty = false;
    Ok(path)
}

pub fn load_custom_world(path: &Path) -> Result<WorldDocument, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    deserialize_custom_world(&bytes)
}

pub fn list_custom_worlds(element: &Path) -> Vec<SavedCustomWorld> {
    let dir = custom_worlds_dir(element);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut worlds = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pwarch") {
            continue;
        }
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("world")
            .to_string();
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(doc) = deserialize_custom_world(&bytes) {
                worlds.push(SavedCustomWorld {
                    slug,
                    display_name: doc.name,
                    path,
                    object_count: doc.objects.len(),
                });
            }
        }
    }
    worlds.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    worlds
}

pub fn delete_custom_world(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|e| format!("delete {}: {e}", path.display()))
}
