//! The wet-cell triangles are the native grid, not a filled rectangle. Keep
//! export identity even when native coalesces equal-height batches.
use super::{rgba, span, MeshData, Terrain};
use crate::modules::lighter::{
    scene::*,
    textures::{Filter, Textures},
};
use glam::Vec3;

struct WetBody {
    center: [f32; 3],
    height: f32,
    cols: usize,
    rows: usize,
    grid: f32,
    // Match the triangle predicate (mask bit 0), not the native fog query's
    // nonzero test. A dry/flag-only cell cannot provide an exit intersection.
    wet: Vec<u32>,
}

impl WetBody {
    fn new(a: &crate::water::WaterArea) -> Self {
        let wet = a
            .mask
            .chunks(32)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0, |w, (i, flag)| w | (u32::from(flag & 1) << i))
            })
            .collect();
        Self {
            center: a.center,
            height: a.height,
            cols: a.columns,
            rows: a.rows,
            grid: a.grid_size,
            wet,
        }
    }

    fn contains(&self, eye: Vec3) -> bool {
        eye.y < self.height
            && crate::water::cell_at(eye.x, eye.z, self.center, self.grid, self.cols, self.rows)
                .is_some_and(|i| self.wet[i / 32] & (1 << (i % 32)) != 0)
    }
}

#[derive(Default)]
pub(super) struct Water {
    bodies: Vec<WetBody>,
    simple: Vec<u32>,
    shore: Option<(Mesh, Surface)>,
}

impl Water {
    pub(super) fn load(t: &Terrain, tex: &mut Textures) -> Result<Self, String> {
        let bodies = t.water.iter().map(WetBody::new).collect();
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
        Ok(Self {
            bodies,
            simple,
            shore,
        })
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
        for &(export, lo, hi, simple) in &m.ray_water {
            let surface = b.surface(Surface::new(
                0,
                WATER | if simple { SIMPLE_WATER } else { 0 },
                0.,
                0.,
            ));
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
