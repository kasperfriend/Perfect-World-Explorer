//! The wet-cell triangles are the native grid, not a filled rectangle. Keep
//! export identity even when native coalesces equal-height batches.
use super::{rgba, span, MeshData, Terrain};
use crate::modules::lighter::{
    scene::*,
    textures::{Filter, Textures},
};
use glam::Vec3;
use std::collections::HashMap;

struct WetBody {
    export: u32,
    center: [f32; 3],
    height: f32,
    cols: usize,
    rows: usize,
    grid: f32,
    // Match the triangle predicate (mask bit 0), not the native fog query's
    // nonzero test. A dry/flag-only cell cannot provide an exit intersection.
    wet: Vec<u32>,
    full: bool,
}

impl WetBody {
    fn new(a: &crate::water::WaterArea) -> Self {
        let full = a.mask.len() == a.columns * a.rows && a.mask.iter().all(|m| m & 1 != 0);
        let wet = if full {
            Vec::new()
        } else {
            a.mask
                .chunks(32)
                .map(|chunk| {
                    chunk
                        .iter()
                        .enumerate()
                        .fold(0, |w, (i, flag)| w | (u32::from(flag & 1) << i))
                })
                .collect()
        };
        Self {
            export: a.export_id,
            center: a.center,
            height: a.height,
            cols: a.columns,
            rows: a.rows,
            grid: a.grid_size,
            wet,
            full,
        }
    }

    fn bounds(&self) -> [f32; 4] {
        let x = self.cols as f32 * self.grid;
        let z = self.rows as f32 * self.grid;
        let x0 = self.center[0] - x * 0.5;
        let z1 = self.center[2] + z * 0.5;
        [x0, x0 + x, z1 - z, z1]
    }

    fn pack(&self, bank: &mut Textures, at: usize, neighbors: &[u32]) -> Result<(), String> {
        let mask = if self.full {
            0
        } else {
            let mask = bank.reserve(self.wet.len())?;
            bank.write_words(mask, &self.wet);
            mask
        };
        let links = if neighbors.is_empty() {
            0
        } else {
            let table = bank.reserve(1 + neighbors.len())?;
            bank.write_words(table, &[neighbors.len() as u32]);
            bank.write_words(table + 1, neighbors);
            table
        };
        let [x0, _, _, z0] = self.bounds();
        bank.write_words(
            at,
            &[
                x0.to_bits(),
                z0.to_bits(),
                self.grid.to_bits(),
                self.height.to_bits(),
                self.cols as u32,
                self.rows as u32,
                mask as u32,
                links as u32,
            ],
        );
        Ok(())
    }

    fn contains(&self, eye: Vec3) -> bool {
        if !eye.is_finite() || eye.y >= self.height {
            return false;
        }
        // Unlike the legacy fog test, don't exclude both sides of an exact
        // export boundary. Use the shader's half-open ownership convention.
        let [x0, _, _, z1] = self.bounds();
        let x = ((eye.x - x0) / self.grid).floor() as i32;
        let z = ((z1 - eye.z) / self.grid).floor() as i32;
        if x < 0 || x >= self.cols as i32 || z < 0 || z >= self.rows as i32 {
            return false;
        }
        let i = x as usize + z as usize * self.cols;
        self.full || self.wet[i / 32] & (1 << (i % 32)) != 0
    }
}

#[derive(Default)]
pub(super) struct Water {
    bodies: Vec<WetBody>,
    exports: HashMap<u32, u32>,
    table: u32,
    simple: Vec<u32>,
    shore: Option<(Mesh, Surface)>,
}

impl Water {
    pub(super) fn pack(
        areas: &[crate::water::WaterArea],
        tex: &mut Textures,
    ) -> Result<Self, String> {
        let bodies: Vec<_> = areas.iter().map(WetBody::new).collect();
        let bounds: Vec<_> = bodies.iter().map(WetBody::bounds).collect();
        let mut order: Vec<_> = (0..bodies.len()).collect();
        order.sort_unstable_by(|&i, &j| {
            bodies[i]
                .height
                .total_cmp(&bodies[j].height)
                .then(bounds[i][0].total_cmp(&bounds[j][0]))
        });
        let mut links = vec![Vec::new(); bodies.len()];
        // Only coplanar touching/overlapping grids can continue a wet path.
        // Preserve the original grids (even different pitches) and dry holes.
        for (k, &i) in order.iter().enumerate() {
            let [_, x1, z0, z1] = bounds[i];
            for &j in &order[k + 1..] {
                if bodies[i].height != bodies[j].height {
                    break;
                }
                let [x0, _, other_lo, other_hi] = bounds[j];
                let tolerance = |a: f32, b: f32| a.abs().max(b.abs()).max(1.) * 4. * f32::EPSILON;
                if x0 - x1 > tolerance(x0, x1) {
                    break;
                }
                if other_lo - z1 <= tolerance(other_lo, z1)
                    && z0 - other_hi <= tolerance(z0, other_hi)
                {
                    links[i].push(j as u32 + 1);
                    links[j].push(i as u32 + 1);
                }
            }
        }
        let table = tex.reserve(bodies.len() * 8)?;
        let mut exports = HashMap::with_capacity(bodies.len());
        for (i, body) in bodies.iter().enumerate() {
            body.pack(tex, table + i * 8, &links[i])?;
            exports.insert(body.export, i as u32 + 1);
        }
        Ok(Self {
            bodies,
            exports,
            table: table as u32,
            ..Self::default()
        })
    }

    pub(super) fn load(t: &Terrain, tex: &mut Textures) -> Result<Self, String> {
        let mut water = Self::pack(&t.water, tex)?;
        let mut simple = Vec::new();
        if t.water.iter().any(|a| a.simple) {
            for frame in &t.water_simple_frames {
                simple.push(tex.images(frame, 128, Filter::ColorMask)?);
            }
        }
        let (vertices, indices) = crate::shore_gpu::mesh(&t.water);
        let shore = if indices.is_empty() {
            None
        } else {
            match crate::shore_gpu::wave_images(&t.element) {
                Ok(images) => {
                    let address = tex.images(&images, 128, Filter::ColorMask)?;
                    let mut mesh = Mesh::default();
                    let color = |argb: u32, alpha: f32| {
                        let [b, g, r, _] = argb.to_le_bytes();
                        u32::from_le_bytes([r, g, b, (alpha * 255.) as u8])
                    };
                    mesh.vertices = vertices
                        .into_iter()
                        .map(|v| {
                            Vertex::new(v.p, [0., 1., 0.], v.uv)
                                .tint(color(v.day, v.alpha), color(v.night, v.alpha))
                        })
                        .collect();
                    // Both halves of a strip section share the layer. Distinct
                    // authored sections can overlap and must still blend.
                    for (i, section) in indices.chunks_exact(6).enumerate() {
                        mesh.tri_layer([section[0], section[1], section[2]], 0, i as u32 + 1);
                        mesh.tri_layer([section[3], section[4], section[5]], 0, i as u32 + 1);
                    }
                    Some((
                        mesh,
                        Surface::new(
                            address,
                            SHORE | BLEND | EMISSIVE | PRIMARY_ONLY | IGNORE_TEXTURE_ALPHA,
                            0.,
                            0.,
                        ),
                    ))
                }
                Err(error) => {
                    eprintln!("Lighter: authored shoreline overlay unavailable: {error}");
                    None
                }
            }
        };
        water.simple = simple;
        water.shore = shore;
        Ok(water)
    }

    pub(super) fn at(&self, eye: Vec3, bounds: Option<[f32; 4]>) -> u32 {
        if !crate::terrain_cutoff::contains(bounds, eye.to_array()) {
            return 0;
        }
        self.bodies
            .iter()
            .enumerate()
            .filter(|(_, a)| a.contains(eye))
            .min_by(|(_, a), (_, b)| a.height.total_cmp(&b.height))
            .map_or(0, |(i, _)| i as u32 + 1)
    }

    pub(super) fn append(&self, m: &MeshData, b: &mut Builder) -> Result<(), String> {
        for &(export, lo, hi, simple) in &m.water_areas {
            let &id = self
                .exports
                .get(&export)
                .ok_or_else(|| format!("missing Lighter medium for water export {export}"))?;
            let surface = b.surface(
                Surface::new(0, WATER | if simple { SIMPLE_WATER } else { 0 }, 0., 0.)
                    .with_water_body(id),
            );
            let mesh = span(
                &m.vertices,
                &m.indices[lo as usize..hi as usize],
                surface,
                |v| rgba(v.color),
            )?;
            b.chunk(ChunkKey::Water(export), &mesh)?;
        }
        if let Some((mesh, surface)) = &self.shore {
            let id = b.surface(*surface);
            b.chunk_mapped(ChunkKey::ShoreWaves, mesh, &[id])?;
        }
        Ok(())
    }

    pub(super) fn table(&self) -> [u32; 2] {
        [self.table, self.bodies.len() as u32]
    }

    pub(super) fn simple_frame(&self, seconds: f64) -> u32 {
        self.simple
            .get(crate::simple_water_gpu::frame_index(seconds))
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn shore_offsets(&self, seconds: f32) -> [f32; 4] {
        if self.shore.is_some() {
            crate::shore_gpu::wave_offsets(seconds)
        } else {
            [0.; 4]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_the_nearest_surface_above_only_when_the_cell_is_wet() {
        let area = |height, mask| crate::water::WaterArea {
            export_id: 0,
            color: 0xffffffff,
            simple: false,
            height,
            grid_size: 2.,
            columns: 3,
            rows: 1,
            center: [0., height, 0.],
            mask,
            shore_waves: vec![],
        };
        let water = Water {
            bodies: vec![
                WetBody::new(&area(8., vec![1, 2, 0])),
                WetBody::new(&area(6., vec![1, 0, 1])),
            ],
            ..Default::default()
        };
        assert_eq!(water.at(Vec3::new(-2., 4., 0.), None), 2);
        assert_eq!(water.at(Vec3::new(-2., 7., 0.), None), 1);
        assert_eq!(
            water.at(Vec3::new(0., 4., 0.), None),
            0,
            "non-drawable flag bits must not create a medium"
        );
        assert_eq!(water.at(Vec3::new(2., 4., 0.), Some([-1., 1., -1., 1.])), 0);
        assert_eq!(water.at(Vec3::new(3., 4., 0.), None), 0);
        assert_eq!(water.at(Vec3::new(-2., 8., 0.), None), 0);
    }
}
