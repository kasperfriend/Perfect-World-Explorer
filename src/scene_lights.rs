//! Retail CECSceneLights version 0x80000001 file records.
//! Client scene-light grid and candidate selection in 0x425470 / 0x4250c0.
//! Published runtime lights feed ordinary world-model queries. GFX controller
//! playback/production is a separate, still unfinished integration path.
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq)]
pub struct SceneLight {
    pub position: [f32; 3],
    pub range: f32,
    pub stored_attenuation: [f32; 3],
    pub diffuse: [f32; 4],
    pub ambient: [f32; 4],
    pub bounds_center: [f32; 3],
    pub bounds_half_extent: [f32; 3],
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub flags: u32,
}
#[derive(Debug, Clone, Default)]
pub struct SceneLights {
    pub lights: Vec<SceneLight>,
}
fn u32_at(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("truncated scene-light record")?
            .try_into()
            .unwrap(),
    ))
}
fn f32_at(b: &[u8], p: usize) -> Result<f32, String> {
    let f = f32::from_bits(u32_at(b, p)?);
    if !f.is_finite() {
        return Err("non-finite scene-light value".into());
    }
    Ok(f)
}
fn floats<const N: usize>(b: &[u8], p: usize) -> Result<[f32; N], String> {
    let mut a = [0.; N];
    for (i, v) in a.iter_mut().enumerate() {
        *v = f32_at(b, p + i * 4)?;
    }
    Ok(a)
}
impl SceneLights {
    pub fn load(path: &Path) -> Result<Self, String> {
        let b = fs::read(path).map_err(|e| e.to_string())?;
        Self::parse(&b)
    }
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        if u32_at(b, 0)? != 0x80000001 {
            return Err("unsupported scene-light version".into());
        }
        let n = u32_at(b, 4)? as usize;
        let required = n
            .checked_mul(112)
            .and_then(|v| v.checked_add(8))
            .ok_or("scene-light size overflow")?;
        if n > 1_000_000 || required != b.len() {
            return Err(format!(
                "scene-light size mismatch: count {n}, bytes {}",
                b.len()
            ));
        }
        let mut lights = Vec::with_capacity(n);
        for i in 0..n {
            let r = &b[8 + i * 112..8 + (i + 1) * 112];
            let light = SceneLight {
                position: floats(r, 0)?,
                range: f32_at(r, 0x0c)?,
                stored_attenuation: floats(r, 0x10)?,
                diffuse: floats(r, 0x1c)?,
                ambient: floats(r, 0x2c)?,
                bounds_center: floats(r, 0x3c)?,
                bounds_half_extent: floats(r, 0x48)?,
                bounds_min: floats(r, 0x54)?,
                bounds_max: floats(r, 0x60)?,
                flags: u32_at(r, 0x6c)?,
            };
            if light.range < 0. || light.bounds_half_extent.iter().any(|x| *x < 0.) {
                return Err(format!("invalid scene light bounds at {i}"));
            }
            lights.push(light);
        }
        Ok(Self { lights })
    }
}
/// Runtime effect light fields read by 0x42592f–0x425b09.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicLight {
    pub position: [f32; 3],
    pub radius: f32,
    /// RGBA vector at runtime-light offset +4; alpha is an intensity factor.
    pub color: [f32; 4],
}

/// Per-consumption GFX publication queue, not the set of live FX objects.
/// Retail AddLightParam suppresses duplicate insertion by object identity;
/// params may change while the first insertion position is retained. The
/// consumer clears publication membership after building dynamic cells
/// (0x425bf8..0x425c2c), so live objects must publish again next update.
#[derive(Default, Debug)]
pub struct LightPublications {
    entries: Vec<(u64, DynamicLight)>,
}
impl LightPublications {
    /// Pending publications for this frame. Membership is not live-light state:
    /// it is cleared by `consume_into` after the dynamic cells are built.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn publish(&mut self, identity: u64, light: DynamicLight) {
        if let Some((_, current)) = self.entries.iter_mut().find(|(id, _)| *id == identity) {
            *current = light;
        } else {
            self.entries.push((identity, light));
        }
    }
    /// Destroy/unregister before consumption, preserving other insertion order.
    pub fn unregister(&mut self, identity: u64) {
        self.entries.retain(|(id, _)| *id != identity);
    }
    pub fn consume_into(&mut self, grid: &mut StaticGrid) {
        let lights: Vec<_> = self.entries.drain(..).map(|(_, light)| light).collect();
        grid.update_dynamic(&lights);
    }
}

/// Static file-light grid. The client rebuilds a 10x10 grid of 12-unit
/// cells centered on the tracked position when it moves over one cell in X/Z.
/// A separate runtime list is rebuilt by update_dynamic.
#[derive(Debug)]
pub struct StaticGrid {
    center: [f32; 3],
    initialized: bool,
    static_records: Vec<SceneLight>,
    cells: [Vec<usize>; 100],
    dynamic_cells: [Vec<usize>; 100],
    dynamic: Vec<DynamicLight>,
}
impl Default for StaticGrid {
    fn default() -> Self {
        Self {
            center: [0.; 3],
            initialized: false,
            static_records: Vec::new(),
            cells: std::array::from_fn(|_| Vec::new()),
            dynamic_cells: std::array::from_fn(|_| Vec::new()),
            dynamic: Vec::new(),
        }
    }
}
impl StaticGrid {
    pub fn update(&mut self, lights: &SceneLights, center: [f32; 3]) {
        // The cheap tests come first: the retail rule is a move of one 12-unit
        // cell in X or Z, and comparing the captured corpus is a record-by-record
        // comparison of every light on the map. Flying fast crosses a cell nearly
        // every frame, so the old order paid that whole comparison — and then
        // rebuilt anyway — on each of those frames. The corpus test only has to
        // answer "did the map change under a camera that did not", which cannot
        // matter while the camera is moving.
        if self.initialized
            && (center[0] - self.center[0]).abs() < 12.
            && (center[2] - self.center[2]).abs() < 12.
            && self.static_records == lights.lights
        {
            return;
        }
        // A replacement scene corpus invalidates cached indices even if the
        // camera did not move. Stable client data retains the retail threshold.
        self.static_records.clone_from(&lights.lights);
        self.center = center;
        self.initialized = true;
        for cell in &mut self.cells {
            cell.clear();
        }
        for cell in &mut self.dynamic_cells {
            cell.clear();
        }
        self.dynamic.clear();
        let min_x = center[0] - 60.;
        let max_x = center[0] + 60.;
        let min_z = center[2] - 60.;
        let max_z = center[2] + 60.;
        let index = |v: f32| (v as i32).clamp(0, 9) as usize;
        for (id, l) in lights.lights.iter().enumerate() {
            if l.bounds_min[0] > max_x
                || l.bounds_max[0] < min_x
                || l.bounds_min[2] > max_z
                || l.bounds_max[2] < min_z
            {
                continue;
            }
            let x0 = index((l.bounds_min[0] - min_x) / 12.);
            let x1 = index((l.bounds_max[0] - min_x) / 12.);
            let z0 = index((max_z - l.bounds_max[2]) / 12.);
            let z1 = index((max_z - l.bounds_min[2]) / 12.);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    self.cells[z * 10 + x].push(id);
                }
            }
        }
    }
    /// Rebuild the dynamic list each frame. The client takes at most three
    /// runtime candidates per cell in source order (0x425b09).
    pub fn update_dynamic(&mut self, lights: &[DynamicLight]) {
        self.dynamic.clear();
        self.dynamic.extend_from_slice(lights);
        for cell in &mut self.dynamic_cells {
            cell.clear();
        }
        if !self.initialized {
            return;
        }
        let min_x = self.center[0] - 60.;
        let max_x = self.center[0] + 60.;
        let min_z = self.center[2] - 60.;
        let max_z = self.center[2] + 60.;
        let index = |v: f32| (v as i32).clamp(0, 9) as usize;
        for (id, l) in self.dynamic.iter().enumerate() {
            if !l.radius.is_finite()
                || l.radius < 0.
                || l.position
                    .iter()
                    .chain(l.color.iter())
                    .any(|x| !x.is_finite())
            {
                continue;
            }
            let [x, _, z] = l.position;
            let r = l.radius;
            if x - r > max_x || x + r < min_x || z - r > max_z || z + r < min_z {
                continue;
            }
            let x0 = index((x - r - min_x) / 12.);
            let x1 = index((x + r - min_x) / 12.);
            let z0 = index((max_z - z - r) / 12.);
            let z1 = index((max_z - z + r) / 12.);
            for cz in z0..=z1 {
                for cx in x0..=x1 {
                    let cell = &mut self.dynamic_cells[cz * 10 + cx];
                    if cell.len() < 3 {
                        cell.push(id);
                    }
                }
            }
        }
    }
    /// Nearest among the cell's static and runtime candidates, not globally nearest.
    /// Returns none outside the client's grid bounds (including y < 0 or > 800).
    /// The 0x4250c0 path scales flag-1 colors by the current night factor,
    /// forces attenuation to (0,.4,0) and color alpha to 1.
    pub fn query(&self, point: [f32; 3], night: f32) -> Option<SceneLight> {
        if !self.initialized || point.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let min_x = self.center[0] - 60.;
        let max_x = self.center[0] + 60.;
        let min_z = self.center[2] - 60.;
        let max_z = self.center[2] + 60.;
        if point[0] < min_x
            || point[0] > max_x
            || point[2] < min_z
            || point[2] > max_z
            || point[1] < 0.
            || point[1] > 800.
        {
            return None;
        }
        let x = (((point[0] - min_x) / 12.) as usize).min(9);
        let z = (((max_z - point[2]) / 12.) as usize).min(9);
        let mut best = None;
        let mut best_d = 99999.8984375_f32;
        for &id in &self.cells[z * 10 + x] {
            let l = &self.static_records[id];
            let d = ((l.position[0] - point[0]).powi(2)
                + (l.position[1] - point[1]).powi(2)
                + (l.position[2] - point[2]).powi(2))
            .sqrt();
            if d < best_d {
                best_d = d;
                best = Some(l);
            }
        }
        let mut result = best.cloned();
        if let Some(l) = &mut result {
            l.stored_attenuation = [0., 0.4, 0.];
            if l.flags & 1 != 0 {
                for v in &mut l.diffuse {
                    *v *= night;
                }
                for v in &mut l.ambient {
                    *v *= night;
                }
            }
        }
        for &id in &self.dynamic_cells[z * 10 + x] {
            let d = &self.dynamic[id];
            let distance = ((d.position[0] - point[0]).powi(2)
                + (d.position[1] - point[1]).powi(2)
                + (d.position[2] - point[2]).powi(2))
            .sqrt();
            if distance < best_d {
                best_d = distance;
                let scale = d.color[3];
                let diff = [
                    2. * d.color[0] * scale,
                    2. * d.color[1] * scale,
                    2. * d.color[2] * scale,
                    1.,
                ];
                let ambient = [
                    0.2 * d.color[0] * scale,
                    0.2 * d.color[1] * scale,
                    0.2 * d.color[2] * scale,
                    1.,
                ];
                result = Some(SceneLight {
                    position: d.position,
                    range: 10.,
                    stored_attenuation: [0., 0.4, 0.],
                    diffuse: diff,
                    ambient,
                    bounds_center: d.position,
                    bounds_half_extent: [d.radius; 3],
                    bounds_min: std::array::from_fn(|i| d.position[i] - d.radius),
                    bounds_max: std::array::from_fn(|i| d.position[i] + d.radius),
                    flags: 0,
                });
            }
        }
        if let Some(l) = &mut result {
            l.diffuse[3] = 1.;
            l.ambient[3] = 1.;
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(position: [f32; 3], range: f32) -> SceneLight {
        SceneLight {
            position,
            range,
            stored_attenuation: [1., 2., 3.],
            diffuse: [0.8, 0.4, 0.2, 0.5],
            ambient: [0.2, 0.1, 0.05, 0.25],
            bounds_center: position,
            bounds_half_extent: [100.; 3],
            bounds_min: position.map(|v| v - 100.),
            bounds_max: position.map(|v| v + 100.),
            flags: 1,
        }
    }

    #[test]
    fn replacing_static_corpus_invalidates_cells_without_camera_motion() {
        let mut lights = SceneLights {
            lights: vec![fixture([0.; 3], 10.)],
        };
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0.; 3]);
        assert!(grid.query([0.; 3], 1.).is_some());
        lights.lights.clear();
        // Queries own the captured corpus, never caller-mutated record IDs.
        assert!(grid.query([0.; 3], 1.).is_some());
        grid.update(&lights, [0.; 3]);
        assert!(grid.query([0.; 3], 1.).is_none());
        lights.lights.push(fixture([0.; 3], 10.));
        grid.update(&lights, [0.; 3]);
        assert!(grid.query([0.; 3], 1.).is_some());
        // Same-size replacement must also rebuild bounds, not reuse ID 0's cell.
        lights.lights[0] = fixture([1000.; 3], 10.);
        grid.update(&lights, [0.; 3]);
        assert!(grid.query([0.; 3], 1.).is_none());
    }
    #[test]
    fn publications_overwrite_in_first_order_and_clear_after_consumption() {
        let light = |x| DynamicLight {
            position: [x, 0., 0.],
            radius: 20.,
            color: [1.; 4],
        };
        let mut q = LightPublications::default();
        let mut grid = StaticGrid::default();
        grid.update(&SceneLights::default(), [0.; 3]);
        q.publish(7, light(1.));
        q.publish(8, light(2.));
        q.publish(7, light(3.));
        q.consume_into(&mut grid);
        assert_eq!(
            grid.dynamic
                .iter()
                .map(|l| l.position[0])
                .collect::<Vec<_>>(),
            [3., 2.]
        );
        assert!(q.entries.is_empty());
        // Still-live object is eligible for a fresh insertion after consume.
        q.publish(7, light(4.));
        q.consume_into(&mut grid);
        assert_eq!(grid.dynamic.len(), 1);
        assert_eq!(grid.dynamic[0].position[0], 4.);
        q.consume_into(&mut grid);
        assert!(grid.dynamic.is_empty());
        assert!(grid.dynamic_cells.iter().all(Vec::is_empty));
    }
    #[test]
    fn publication_destruction_and_cell_cap_preserve_source_order() {
        let mut q = LightPublications::default();
        let mut grid = StaticGrid::default();
        grid.update(&SceneLights::default(), [0.; 3]);
        for id in 0..5 {
            q.publish(
                id,
                DynamicLight {
                    position: [id as f32, 0., 0.],
                    radius: 100.,
                    color: [1.; 4],
                },
            );
        }
        q.unregister(1);
        q.unregister(999);
        q.publish(
            1,
            DynamicLight {
                position: [9., 0., 0.],
                radius: 100.,
                color: [1.; 4],
            },
        );
        q.consume_into(&mut grid);
        assert_eq!(
            grid.dynamic
                .iter()
                .map(|l| l.position[0])
                .collect::<Vec<_>>(),
            [0., 2., 3., 4., 9.]
        );
        assert!(grid.dynamic_cells.iter().all(|c| c == &[0, 1, 2]));
        assert!(q.entries.is_empty());
    }
    #[test]
    fn candidate_range_is_not_a_query_cutoff_and_night_does_not_change_priority() {
        let first = fixture([0., 100., 0.], 1.);
        let mut second = first.clone();
        second.flags = 0;
        second.diffuse = [1.; 4];
        let lights = SceneLights {
            lights: vec![first.clone(), second],
        };
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        // The chosen cell's AABBs overlap the query even though range does not.
        for night in [0., 0.5, 1.] {
            let chosen = grid.query([45., 100., 0.], night).unwrap();
            assert_eq!(chosen.range, 1.);
            assert_eq!(
                chosen.flags, 1,
                "equal-distance static ties keep source order"
            );
            assert_eq!(chosen.diffuse[0], first.diffuse[0] * night);
            assert_eq!(chosen.ambient[0], first.ambient[0] * night);
            assert_eq!(chosen.diffuse[3], 1.);
            assert_eq!(chosen.ambient[3], 1.);
        }
        assert_eq!(
            lights.lights[0], first,
            "queries must not rescale stored colors"
        );
    }

    #[test]
    fn runtime_light_replaces_static_only_when_strictly_closer() {
        let lights = SceneLights {
            lights: vec![fixture([8., 100., 0.], 20.)],
        };
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        for (x, expected) in [(9., 8.), (8., 8.), (7., 7.)] {
            grid.update_dynamic(&[DynamicLight {
                position: [x, 100., 0.],
                radius: 20.,
                color: [0.25, 0.5, 1., 0.5],
            }]);
            let chosen = grid.query([0., 100., 0.], 1.).unwrap();
            assert_eq!(chosen.position[0], expected);
            if x == 7. {
                assert_eq!(chosen.diffuse, [0.25, 0.5, 1., 1.]);
                assert_eq!(chosen.ambient, [0.025, 0.05, 0.1, 1.]);
            }
        }
        grid.update(&lights, [12., 100., 0.]);
        assert!(
            grid.dynamic.is_empty(),
            "rebuild must discard old-frame runtime cells"
        );
    }

    #[test]
    fn query_bounds_are_inclusive_and_edge_indices_are_clamped() {
        let lights = SceneLights {
            lights: vec![fixture([0., 100., 0.], 1.)],
        };
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        for x in [-60., 60.] {
            for z in [-60., 60.] {
                for y in [0., 800.] {
                    assert!(grid.query([x, y, z], 1.).is_some());
                }
            }
        }
        for p in [
            [-60.001, 100., 0.],
            [60.001, 100., 0.],
            [0., -0.001, 0.],
            [0., 800.001, 0.],
            [0., 100., -60.001],
            [0., 100., 60.001],
            [f32::NAN, 100., 0.],
            [0., f32::INFINITY, 0.],
        ] {
            assert!(grid.query(p, 1.).is_none());
        }
    }

    #[test]
    fn invalid_runtime_candidates_do_not_consume_the_cell_cap() {
        let lights = SceneLights::default();
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        let valid = DynamicLight {
            position: [0., 100., 0.],
            radius: 10.,
            color: [1.; 4],
        };
        let mut bad_radius = valid;
        bad_radius.radius = -1.;
        let mut bad_position = valid;
        bad_position.position[0] = f32::NAN;
        let mut bad_color = valid;
        bad_color.color[3] = f32::INFINITY;
        grid.update_dynamic(&[bad_radius, bad_position, bad_color, valid]);
        assert_eq!(
            grid.query(valid.position, 1.).unwrap().position,
            valid.position
        );
    }

    #[test]
    fn static_grid_scales_night_and_uses_cell_not_global_nearest() {
        fn light(x: f32, range: f32, flag: u32) -> SceneLight {
            SceneLight {
                position: [x, 100., 0.],
                range,
                stored_attenuation: [1., 2., 3.],
                diffuse: [1., 0.5, 0.25, 2.],
                ambient: [0.2, 0.1, 0.05, 0.4],
                bounds_center: [x, 100., 0.],
                bounds_half_extent: [range; 3],
                bounds_min: [x - range, 100. - range, -range],
                bounds_max: [x + range, 100. + range, range],
                flags: flag,
            }
        }
        let lights = SceneLights {
            lights: vec![light(0., 20., 1), light(30., 5., 0)],
        };
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        let l = grid.query([0., 100., 0.], 0.5).unwrap();
        assert_eq!(l.diffuse, [0.5, 0.25, 0.125, 1.]);
        assert_eq!(l.stored_attenuation, [0., 0.4, 0.]);
        let l = grid.query([30., 100., 0.], 0.).unwrap();
        assert_eq!(l.diffuse, [1., 0.5, 0.25, 1.]);
        assert!(grid.query([0., 801., 0.], 1.).is_none());
        assert!(grid.query([59., 100., 59.], 1.).is_none());
        grid.update(&lights, [100., 100., 0.]);
        assert!(grid.query([0., 100., 0.], 1.).is_none());
    }
    #[test]
    fn dynamic_candidates_replace_nearer_static_and_are_capped_at_three() {
        let lights = SceneLights::default();
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        let mut dynamic = vec![];
        for x in [8., 7., 6., 0.] {
            dynamic.push(DynamicLight {
                position: [x, 100., 0.],
                radius: 20.,
                color: [0.5, 0.25, 0.125, 0.5],
            });
        }
        grid.update_dynamic(&dynamic);
        let chosen = grid.query([0., 100., 0.], 0.).unwrap();
        assert_eq!(chosen.position, [6., 100., 0.]);
        assert_eq!(chosen.diffuse, [0.5, 0.25, 0.125, 1.]);
        assert_eq!(chosen.ambient, [0.05, 0.025, 0.0125, 1.]);
        assert_eq!(chosen.range, 10.);
        assert_eq!(chosen.stored_attenuation, [0., 0.4, 0.]);
        grid.update_dynamic(&[]);
        assert!(grid.query([0., 100., 0.], 0.).is_none());
    }
    #[test]
    fn camera_cell_rebuilds_at_exact_boundary_and_dynamic_tie_keeps_static() {
        let mut b = vec![0u8; 120];
        b[0..4].copy_from_slice(&0x80000001u32.to_le_bytes());
        b[4..8].copy_from_slice(&1u32.to_le_bytes());
        for (o, v) in [
            (4, 100f32),
            (12, 50.),
            (28, 0.75),
            (0x54, -50.),
            (0x58, 50.),
            (0x5c, -50.),
            (0x60, 50.),
            (0x64, 150.),
            (0x68, 50.),
        ] {
            b[8 + o..12 + o].copy_from_slice(&v.to_le_bytes());
        }
        let lights = SceneLights::parse(&b).unwrap();
        let mut grid = StaticGrid::default();
        grid.update(&lights, [0., 100., 0.]);
        grid.update_dynamic(&[DynamicLight {
            position: [0., 100., 0.],
            radius: 10.,
            color: [1., 1., 1., 1.],
        }]);
        assert_eq!(grid.query([0., 100., 0.], 0.).unwrap().diffuse[0], 0.75);
        grid.update(&lights, [11.999, 100., 0.]);
        assert_eq!(grid.center[0], 0.);
        grid.update(&lights, [12., 100., 0.]);
        assert_eq!(grid.center[0], 12.);
    }
    #[test]
    fn parses_record_and_rejects_bad_inputs() {
        let mut b = vec![0; 120];
        b[0..4].copy_from_slice(&0x80000001u32.to_le_bytes());
        b[4..8].copy_from_slice(&1u32.to_le_bytes());
        let put =
            |b: &mut Vec<u8>, o: usize, v: f32| b[8 + o..12 + o].copy_from_slice(&v.to_le_bytes());
        put(&mut b, 0, 1.);
        put(&mut b, 0xc, 25.);
        put(&mut b, 0x1c, 0.5);
        put(&mut b, 0x28, 2.);
        put(&mut b, 0x48, 25.);
        b[116..120].copy_from_slice(&1u32.to_le_bytes());
        let s = SceneLights::parse(&b).unwrap();
        assert_eq!(s.lights[0].position[0], 1.);
        assert_eq!(s.lights[0].diffuse, [0.5, 0., 0., 2.]);
        assert_eq!(s.lights[0].flags, 1);
        assert!(SceneLights::parse(&b[..119]).is_err());
        put(&mut b, 0, f32::NAN);
        assert!(SceneLights::parse(&b).is_err());
    }
}
