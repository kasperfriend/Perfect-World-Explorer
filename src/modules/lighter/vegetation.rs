//! Ray vegetation uses the native sampled SpeedTree motion and grass history.
//! View-aligned leaf cards are only primary geometry; fixed-world proxies
//! receive the same motion but never rotate with the main camera.
use super::{atlas_uv, Counts, FrameSnapshot, MeshData, COLOR_SIDE};
use crate::modules::lighter::{
    scene::*,
    textures::{Filter, Textures},
};
use glam::Vec3;
use std::collections::{hash_map::Entry, HashMap};

fn xyz([x, y, z]: [f32; 3]) -> [f32; 3] {
    [x, z, y]
}

pub(super) struct Tree {
    lods: Vec<TreeLevel>,
    height: f32,
}

struct TreeLevel {
    solid: Mesh,
    materials: [Surface; 3],
    leaves: Vec<crate::trees::Leaf>,
    rocking: Vec<[f32; 3]>,
}

fn corner_sample(rest: Vec3, motion: &[[f32; 3]], time: f32, blend: f32) -> Vec3 {
    if motion.len() != 64 || blend <= 0. {
        return rest;
    }
    // One 16-second DLL sample cycle, with the native viewer's wrap blend.
    let phase = (time / 16.).rem_euclid(1.) * 64.;
    let at = phase.floor() as usize;
    let sample =
        Vec3::from_array(motion[at]).lerp(Vec3::from_array(motion[(at + 1) % 64]), phase.fract());
    rest.lerp(
        sample.lerp(
            Vec3::from_array(motion[0]),
            ((phase - 60.) / 4.).clamp(0., 1.),
        ),
        blend.clamp(0., 1.),
    )
}

fn bend_leaf(corner: Vec3, rho: f32, direction: Vec3) -> Vec3 {
    if rho > 1000. {
        return Vec3::from_array(xyz(corner.to_array()));
    }
    let d = direction.normalize_or_zero();
    let mut az = d.z.atan2(d.x);
    if d.x == 0. && d.z == 0. {
        az = 0.;
    }
    let (sin_el, cos_el) = d.y.clamp(-1., 1.).asin().sin_cos();
    let (sin_az, cos_az) = (az + rho).sin_cos();
    let x = cos_el * corner.x - sin_el * corner.z;
    Vec3::new(
        cos_az * x - sin_az * corner.y,
        sin_el * corner.x + cos_el * corner.z,
        sin_az * x + cos_az * corner.y,
    )
}

impl TreeLevel {
    fn sample(&self, direction: Vec3, time: f32, blend: f32) -> Mesh {
        let mut mesh = self.solid.clone();
        for leaf in &self.leaves {
            let center = Vec3::from_array(xyz(leaf.center));
            let normal = leaf.normal.map_or([0., 1., 0.], xyz);
            let mut corners = [Vec3::ZERO; 4];
            for (j, point) in corners.iter_mut().enumerate() {
                let slot = (leaf.cluster as usize * 4 + j) * 64;
                *point = corner_sample(
                    Vec3::from_array(leaf.corners[j]),
                    self.rocking.get(slot..slot + 64).unwrap_or(&[]),
                    time,
                    blend,
                );
            }
            // Fixed leaves are already world oriented; for camera-dependent
            // leaves a sky-facing proxy gives a cover footprint at noon. It
            // is distinct visibility, not a second leaf for the same query.
            let views: &[(Vec3, u32)] = if leaf.rho > 1000. {
                &[(Vec3::Y, 4)]
            } else {
                &[(direction, 2), (Vec3::Y, 3)]
            };
            for &(view, material) in views {
                let base = mesh.vertices.len() as u32;
                for (point, uv) in corners.into_iter().zip(leaf.uv) {
                    let p = center + bend_leaf(point, leaf.rho, view);
                    mesh.vertices.push(Vertex::new(p.to_array(), normal, uv));
                }
                mesh.tri([base, base + 1, base + 2], material);
                mesh.tri([base + 2, base + 1, base + 3], material);
            }
        }
        mesh
    }
}

pub(super) fn forest(
    assets: &[Option<Tree>],
    placements: &[(usize, [f32; 3])],
    scale: f32,
    f: &FrameSnapshot,
    builder: &mut Builder,
    counts: &mut Counts,
    budget: &mut MeshBudget,
) -> Result<(), String> {
    // A missing cache is handled at snapshot time. At most two geometry
    // LODs are issued for each resident placement; use the live 3D eye.
    let eye = [f.grass_eye[0], f.grass_eye[1], f.grass_eye[2]];
    #[derive(Clone, Copy)]
    enum Template {
        Mesh(u32),
        Empty,
        Budget,
    }
    let mut templates = HashMap::new();
    let instance = Mesh::default();
    for &(type_id, position) in placements {
        let tree = assets[type_id].as_ref().unwrap();
        let Some(distance) = crate::tree_lod::client_distance(position, eye) else {
            counts.invalid += 1;
            continue;
        };
        let Some(last) = tree.lods.len().checked_sub(1) else {
            counts.empty += 1;
            continue;
        };
        let lod = crate::tree_lod::select(distance, tree.height, scale);
        let near = lod.primary.min(last);
        let far = lod
            .secondary
            .map(|i| i.min(last))
            .filter(|&i| i != near && lod.fraction > 0.);
        let mut live = [None; 2];
        let mut exceeded = false;
        for (side, level) in [Some(near), far].into_iter().enumerate() {
            let Some(level) = level else {
                continue;
            };
            let lod_mesh = &tree.lods[level];
            let id = match templates.entry((type_id, level)) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => {
                    let mesh = lod_mesh.sample(f.direction, f.tree_bounds[3], f.tree_bounds[2]);
                    let id = if !budget.debit(&mesh, 5) {
                        Template::Budget
                    } else {
                        let [stem, mut frond, mut leaves] = lod_mesh.materials;
                        frond.flags |= FOLIAGE;
                        leaves.flags |= FOLIAGE;
                        let mut primary = leaves;
                        primary.flags |= PRIMARY_ONLY;
                        let mut secondary = leaves;
                        secondary.flags |= HIDE_PRIMARY;
                        let materials =
                            [stem, frond, primary, secondary, leaves].map(|m| builder.surface(m));
                        let id = if lod_mesh.leaves.is_empty() {
                            builder.mesh_mapped(&mesh, &materials)?
                        } else {
                            builder.posed_template(
                                "tree",
                                ((type_id as u64) << 32) | level as u64,
                                &mesh,
                                &materials,
                            )?
                        };
                        id.map_or(Template::Empty, Template::Mesh)
                    };
                    *e.insert(id)
                }
            };
            match id {
                Template::Mesh(id) if budget.debit(&instance, 0) => live[side] = Some(id),
                Template::Mesh(_) | Template::Budget => exceeded = true,
                Template::Empty => {}
            }
        }
        match live {
            [Some(a), Some(b)] => {
                builder.place_tree(a, position, lod.fraction, 1)?;
                builder.place_tree(b, position, lod.fraction, 2)?;
                counts.loaded += 1;
            }
            // A partial/budgeted LOD pair keeps full coverage at the retained level.
            [Some(id), None] | [None, Some(id)] => {
                builder.place_tree(id, position, 0., 0)?;
                counts.loaded += 1;
            }
            [None, None] if exceeded => counts.budget += 1,
            [None, None] => counts.empty += 1,
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Grass(Vec<GrassGroup>);
struct GrassGroup {
    mesh: Mesh,
    material: Surface,
    instances: Vec<[f32; 12]>,
}

impl Grass {
    pub(super) fn prepare(
        m: &MeshData,
        textures: &mut Textures,
        authored: bool,
    ) -> Result<Self, String> {
        let (vertices, indices, instances, batches) = m.grass.geometry();
        let mut textures_seen = HashMap::new();
        let mut groups = Vec::new();
        for batch in batches {
            let indices = &indices[batch.indices.start as usize..batch.indices.end as usize];
            let Some((&low, &high)) = indices.iter().min().zip(indices.iter().max()) else {
                continue;
            };
            // Native batches are one mesh/material of one species.
            let v = &vertices[low as usize].vertex;
            let [x, y, _, alpha] = v.color;
            let at = [x, y].map(|p| (p * super::super::ATLAS as f32).round() as usize);
            let texture = match textures_seen.entry(at) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => *e.insert(
                    textures.insert(
                        m.ray_grass
                            .get(&at)
                            .ok_or("missing ray grass material source")?,
                    )?,
                ),
            };
            let mut mesh = Mesh::default();
            for g in &vertices[low as usize..=high as usize] {
                let v = &g.vertex;
                mesh.vertices.push(
                    Vertex::new(v.pos, v.normal, atlas_uv(v, g.scale_y))
                        .grass(v.diffuse, v.night_diffuse),
                );
            }
            for tri in indices.chunks_exact(3) {
                mesh.tri([tri[0] - low, tri[1] - low, tri[2] - low], 0);
            }
            groups.push(GrassGroup {
                mesh,
                material: Surface::new(
                    texture,
                    CUTOUT | GRASS_MATERIAL | STRICT_ALPHA,
                    if authored { alpha } else { 84. / 255. },
                    0.,
                ),
                instances: instances[batch.instances.start as usize..batch.instances.end as usize]
                    .to_vec(),
            });
        }
        Ok(Self(groups))
    }

    pub(super) fn append(
        &self,
        f: &FrameSnapshot,
        b: &mut Builder,
        counts: &mut Counts,
        budget: &mut MeshBudget,
    ) -> Result<(), String> {
        // The history vector is per placement but the bend weight is per
        // vertex (quadratic in height), so an affine TLAS transform cannot
        // represent it. Cap exact animated copies; keep a rigid base beyond
        // the cap, and preserve instancing entirely when wind is off.
        const WIND_BYTES: usize = 4 << 20;
        let mut wind = *budget;
        wind.bytes = wind.bytes.min(WIND_BYTES);
        let empty = Mesh::default();
        let enabled = f.grass_wind[1] > 0.5;
        let mut skipped_wind = false;
        for (group, g) in self.0.iter().enumerate() {
            if !budget.debit(&g.mesh, 1) {
                counts.budget += g.instances.len();
                continue;
            }
            let material = [b.surface(g.material)];
            let Some(base) = b.mesh_mapped(&g.mesh, &material)? else {
                counts.empty += g.instances.len();
                continue;
            };
            let mut scratch = None;
            for (index, v) in g.instances.iter().enumerate() {
                let transform = Transform {
                    position: [v[0], v[1], v[2]],
                    scale: v[3],
                    cos: v[4],
                    sin: -v[5],
                };
                let area = [v[8], v[9]];
                let slot = (v[6].round() as i32).rem_euclid(GRASS_WIND_SLOTS as i32) as usize;
                let history = Vec3::from_slice(&f.grass_history[slot]);
                if !budget.debit(&empty, 0) {
                    counts.budget += 1;
                    continue;
                }
                let id = if enabled
                    && history != Vec3::ZERO
                    && wind.debit(&g.mesh, 0)
                    && budget.debit(&g.mesh, 0)
                {
                    let delta = wind_local(transform, history);
                    let scratch = scratch.get_or_insert_with(|| g.mesh.clone());
                    for (p, rest) in scratch.vertices.iter_mut().zip(&g.mesh.vertices) {
                        let weight = rest.grass_weight();
                        let offset = if (0.0..100.0).contains(&weight) {
                            delta * weight
                        } else {
                            Vec3::ZERO
                        };
                        p.pos = (Vec3::from_array(rest.pos) + offset).to_array();
                    }
                    b.posed_template(
                        "grass",
                        ((group as u64) << 32) | index as u64,
                        scratch,
                        &material,
                    )?
                    .unwrap_or(base)
                } else {
                    base
                };
                skipped_wind |= enabled && history != Vec3::ZERO && id == base;
                if id == base {
                    b.place_grass(id, transform, area)?;
                } else {
                    b.place_grass_wind(id, transform, area, slot as u32)?;
                }
                counts.loaded += 1;
            }
        }
        if skipped_wind {
            static WARN: std::sync::Once = std::sync::Once::new();
            WARN.call_once(|| eprintln!("Lighter: grass wind geometry capped at 4 MiB; remaining placements keep rigid coverage"));
        }
        Ok(())
    }
}
fn wind_local(t: Transform, v: Vec3) -> Vec3 {
    Vec3::new(t.cos * v.x + t.sin * v.z, v.y, -t.sin * v.x + t.cos * v.z) / t.scale
}

pub(super) fn read_tree(
    source: &crate::trees::TreeMesh,
    textures: &mut Textures,
    authored: bool,
) -> Result<Tree, String> {
    let xyz = |p: [f32; 3]| [p[0], p[2], p[1]];
    let alpha = |value: Option<f32>, default: Option<f32>| {
        if authored {
            value
                .filter(|v| *v >= 0.)
                .map_or(default.map(|r| (r, false)), |v| Some((v / 255., true)))
        } else {
            default.map(|r| (r, false))
        }
    };
    let height = source.lods.first().map_or(5., |lod| {
        lod.branch
            .vertices
            .iter()
            .chain(&lod.frond.vertices)
            .map(|v| v.pos[2])
            .chain(lod.leaves.iter().map(|l| l.center[2]))
            .fold(5.0f32, f32::max)
    });
    let mut tiles = HashMap::new();
    let mut lods = Vec::with_capacity(source.lods.len());
    for lod in &source.lods {
        let thresholds = [
            alpha(lod.branch_alpha_ref, None),
            alpha(lod.frond_alpha_ref, Some(0.5)),
            if authored {
                Some((84. / 255., true))
            } else {
                Some((0.5, false))
            },
        ];
        let mut materials = [Surface::new(0, 0, 0., 0.); 3];
        for (i, reference) in thresholds.into_iter().enumerate() {
            let (image, mips) = if i == 0 {
                (&source.bark, &source.bark_mips)
            } else {
                (&source.composite, &source.composite_mips)
            };
            let mode = reference.map_or(Filter::Color, |(r, strict)| Filter::cutout(r, strict));
            let addr = match tiles.entry((i == 0, mode)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => *e.insert(textures.images(
                    std::iter::once(image).chain(mips),
                    COLOR_SIDE,
                    mode,
                )?),
            };
            let (cutoff, flags) = reference.map_or((0., 0), |(r, strict)| {
                (r, CUTOUT | if strict { STRICT_ALPHA } else { 0 })
            });
            materials[i] = Surface::new(addr, flags | TREE_MATERIAL, cutoff, 0.);
        }
        let mut mesh = Mesh::default();
        for (i, part) in [&lod.branch, &lod.frond].into_iter().enumerate() {
            let base = mesh.vertices.len() as u32;
            mesh.vertices.extend(
                part.vertices
                    .iter()
                    .map(|v| Vertex::new(xyz(v.pos), xyz(v.normal), v.uv)),
            );
            for tri in part.indices.chunks_exact(3) {
                mesh.tri([base + tri[0], base + tri[1], base + tri[2]], i as u32);
            }
        }
        lods.push(TreeLevel {
            solid: mesh,
            materials,
            leaves: lod.leaves.clone(),
            rocking: lod.rocking.clone(),
        });
    }
    Ok(Tree { lods, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_rocking_and_fixed_leaf_proxy_do_not_depend_on_main_view() {
        let mut table = vec![[0.; 3]; 4 * 64];
        let corners = [[0., -1., -1.], [0., 1., -1.], [0., -1., 1.], [0., 1., 1.]];
        for j in 0..4 {
            for i in 0..64 {
                let mut p = corners[j];
                p[1] += i as f32 * 0.125;
                table[j * 64 + i] = p;
            }
        }
        let lod = TreeLevel {
            solid: Mesh::default(),
            materials: [Surface::new(0, 0, 0., 0.); 3],
            leaves: vec![crate::trees::Leaf {
                center: [0., 0., 2.],
                normal: None,
                rho: 0.,
                corners,
                uv: [[0.; 2]; 4],
                cluster: 0,
            }],
            rocking: table,
        };
        let first = lod.sample(Vec3::X, 0., 1.);
        let rotated = lod.sample(Vec3::Z, 0., 1.);
        let rocked = lod.sample(Vec3::X, 0.125, 1.);
        assert_eq!(first.triangles, rotated.triangles);
        assert_eq!(first.triangles, rocked.triangles);
        assert_eq!(first.surfaces, [2, 2, 3, 3]);
        assert_ne!(first.vertices[0].pos, rotated.vertices[0].pos);
        assert_eq!(first.vertices[4].pos, rotated.vertices[4].pos);
        assert!((rocked.vertices[4].pos[2] - first.vertices[4].pos[2] - 0.0625).abs() < 1e-6);
        assert_eq!(
            first.vertices[4].pos,
            lod.sample(Vec3::X, 16., 1.).vertices[4].pos
        );
    }

    #[test]
    fn grass_wind_history_is_in_world_space_even_for_scaled_yawed_instances() {
        let t = Transform {
            position: [18., 0., 4.],
            scale: 2.6,
            cos: 0.,
            sin: 1.,
        };
        let wind = Vec3::new(0.7, -0.4, 0.1);
        let weight = 0.37;
        let delta = wind_local(t, wind) * weight;
        let rows = t.rows();
        let to_world = |p: Vec3| {
            Vec3::new(
                Vec3::from_slice(&rows[0..3]).dot(p),
                Vec3::from_slice(&rows[4..7]).dot(p),
                Vec3::from_slice(&rows[8..11]).dot(p),
            )
        };
        assert!((to_world(delta) - wind * weight).length() < 1e-6);
    }
}
