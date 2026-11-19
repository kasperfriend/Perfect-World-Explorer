//! Ray vegetation uses the native sampled SpeedTree motion and grass history.
//! View-aligned leaf cards are only primary geometry; fixed-world proxies
//! receive the same motion but never rotate with the main camera.
use super::{atlas_uv, Counts, FrameSnapshot, MeshData};
use crate::modules::lighter::{
    scene::*,
    textures::{Filter, Textures},
};
use glam::{Mat4, Vec3};
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

#[derive(Clone, Copy)]
struct LeafFacing {
    azimuth: f32,
    sin_el: f32,
    cos_el: f32,
}
impl LeafFacing {
    fn new(direction: Vec3) -> Self {
        let d = direction.normalize_or_zero();
        let azimuth = if d.x == 0. && d.z == 0. {
            0.
        } else {
            d.z.atan2(d.x)
        };
        let (sin_el, cos_el) = d.y.clamp(-1., 1.).asin().sin_cos();
        Self {
            azimuth,
            sin_el,
            cos_el,
        }
    }
    fn turn(self, rho: f32) -> impl Fn(Vec3) -> Vec3 {
        let (sin_az, cos_az) = (self.azimuth + rho).sin_cos();
        move |p: Vec3| {
            let x = self.cos_el * p.x - self.sin_el * p.z;
            Vec3::new(
                cos_az * x - sin_az * p.y,
                self.sin_el * p.x + self.cos_el * p.z,
                sin_az * x + cos_az * p.y,
            )
        }
    }
}

impl TreeLevel {
    fn sample(&self, direction: LeafFacing, time: f32, blend: f32) -> [Mesh; 3] {
        let mut meshes = [self.solid.clone(), Mesh::default(), Mesh::default()];
        let proxy = LeafFacing::new(Vec3::Y);
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
            let append = |mesh: &mut Mesh, material, turn: &dyn Fn(Vec3) -> Vec3| {
                let base = mesh.vertices.len() as u32;
                for (point, uv) in corners.into_iter().zip(leaf.uv) {
                    mesh.vertices
                        .push(Vertex::new((center + turn(point)).to_array(), normal, uv));
                }
                mesh.tri([base, base + 1, base + 2], material);
                mesh.tri([base + 2, base + 1, base + 3], material);
            };
            if leaf.rho > 1000. {
                append(&mut meshes[0], 4, &|p| Vec3::from_array(xyz(p.to_array())));
            } else {
                // Separate instance masks put camera cards and secondary
                // proxies in disjoint queries. Never draw both silhouettes.
                append(&mut meshes[1], 2, &direction.turn(leaf.rho));
                append(&mut meshes[2], 3, &proxy.turn(leaf.rho));
            }
        }
        meshes
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
        Mesh([Option<u32>; 3]),
        Empty,
        Budget,
    }
    let mut templates = HashMap::new();
    let facing = LeafFacing::new(f.direction);
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
        let mut near = lod.primary.min(last);
        let mut far = lod.secondary.map(|i| i.min(last)).filter(|&i| i != near);
        // No Bayer pixel can ever see the unused endpoint. Prune before
        // sampling the template or charging per-placement TLAS space.
        if lod.fraction <= 0.5 / 16. {
            far = None;
        }
        if lod.fraction > 15.5 / 16. {
            near = far.take().unwrap_or(near);
        }
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
                    let meshes = lod_mesh.sample(facing, f.tree_bounds[3], f.tree_bounds[2]);
                    let mut admission = *budget;
                    let id = if meshes.iter().all(|m| m.triangles.is_empty()) {
                        Template::Empty
                    } else if !meshes
                        .iter()
                        .filter(|m| !m.triangles.is_empty())
                        .enumerate()
                        .all(|(i, m)| admission.debit(m, if i == 0 { 5 } else { 0 }))
                    {
                        Template::Budget
                    } else {
                        *budget = admission;
                        let [stem, mut frond, mut leaves] = lod_mesh.materials;
                        frond.flags |= FOLIAGE;
                        leaves.flags |= FOLIAGE;
                        let mut primary = leaves;
                        primary.flags |= PRIMARY_ONLY;
                        let mut secondary = leaves;
                        secondary.flags |= HIDE_PRIMARY;
                        let materials =
                            [stem, frond, primary, secondary, leaves].map(|m| builder.surface(m));
                        let mut ids = [None; 3];
                        for (part, mesh) in meshes.iter().enumerate() {
                            if part == 0 && !lod_mesh.leaves.iter().any(|l| l.rho > 1000.) {
                                // Camera and wind no longer cause a trunk/frond
                                // refit or a fresh upload of those vertices.
                                ids[part] = builder.mesh_mapped(mesh, &materials)?;
                            } else {
                                ids[part] = builder.posed_template(
                                    "tree",
                                    ((type_id as u64) << 32) | ((level as u64) << 2) | part as u64,
                                    mesh,
                                    &materials,
                                )?;
                            }
                        }
                        Template::Mesh(ids)
                    };
                    *e.insert(id)
                }
            };
            match id {
                Template::Mesh(ids) => {
                    let mut admission = *budget;
                    if ids.iter().flatten().all(|_| admission.debit(&instance, 0)) {
                        *budget = admission;
                        live[side] = Some(ids);
                    } else {
                        exceeded = true;
                    }
                }
                Template::Budget => exceeded = true,
                Template::Empty => {}
            }
        }
        let mut place = |ids: [Option<u32>; 3], fraction, side| -> Result<(), String> {
            for id in ids.into_iter().flatten() {
                builder.place_tree(id, position, fraction, side)?;
            }
            Ok(())
        };
        match live {
            [Some(a), Some(b)] => {
                place(a, lod.fraction, 1)?;
                place(b, lod.fraction, 2)?;
                counts.loaded += 1;
            }
            // A partial/budgeted LOD pair keeps full coverage at the retained level.
            [Some(ids), None] | [None, Some(ids)] => {
                place(ids, 0., 0)?;
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
    bend: GrassBend,
    instances: Vec<[f32; 12]>,
}

impl Grass {
    pub(super) fn prepare(
        m: &MeshData,
        textures: &mut Textures,
        authored: bool,
        counts: &mut Counts,
    ) -> Result<Self, String> {
        let (vertices, indices, instances, batches) = m.grass.geometry();
        // Grass shares the streamed suffix with scenery and moving actors.
        let mut bytes = textures.remaining_bytes() / 4;
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
            let Some(tile) = m.ray_grass.get(&at) else {
                counts.unavailable += batch.instances.len();
                continue;
            };
            let texture = match textures_seen.entry(at) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => {
                    if tile.bytes() > bytes {
                        counts.budget += batch.instances.len();
                        continue;
                    }
                    let address = textures.insert(tile)?;
                    bytes -= tile.bytes();
                    *e.insert(address)
                }
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
                bend: GrassBend::fit(&mesh.vertices),
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
        interactive_hardware: bool,
    ) -> Result<(), String> {
        // The weight is quadratic, but cards with only a base and tip are
        // exactly affine at every vertex. More complex meshes get a bounded
        // approximation only in interactive Hardware; otherwise bend exactly.
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
                if enabled && history != Vec3::ZERO && budget.bytes >= 18 * 4 {
                    let error = history.length() * g.bend.error;
                    let allowed = if interactive_hardware { 0.02 } else { 1.0e-6 };
                    if error <= allowed {
                        if let Some(attachment) = g.bend.attachment(transform, history) {
                            budget.bytes -= 18 * 4;
                            b.place_grass_affine(base, attachment, area, slot as u32)?;
                            counts.loaded += 1;
                            continue;
                        }
                    }
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
#[derive(Clone, Copy, Default)]
struct GrassBend {
    slope: f32,
    intercept: f32,
    error: f32,
}
impl GrassBend {
    fn fit(vertices: &[Vertex]) -> Self {
        let Some(first) = vertices.first() else {
            return Self::default();
        };
        let (lo, hi) = vertices.iter().fold((first, first), |(lo, hi), v| {
            (
                if v.pos[1] < lo.pos[1] { v } else { lo },
                if v.pos[1] > hi.pos[1] { v } else { hi },
            )
        });
        let weight = |v: &Vertex| {
            let w = (*v).grass_weight();
            if (0.0..100.).contains(&w) {
                w
            } else {
                0.
            }
        };
        let span = hi.pos[1] - lo.pos[1];
        let slope = if span > 1.0e-6 {
            (weight(hi) - weight(lo)) / span
        } else {
            0.
        };
        let intercept = weight(lo) - slope * lo.pos[1];
        let error = vertices
            .iter()
            .map(|v| (weight(v) - (slope * v.pos[1] + intercept)).abs())
            .fold(0., f32::max);
        Self {
            slope,
            intercept,
            error,
        }
    }
    fn attachment(self, t: Transform, wind: Vec3) -> Option<Attachment> {
        let x = Vec3::new(t.cos, 0., t.sin) * t.scale;
        let y = Vec3::Y * t.scale + wind * self.slope;
        let z = Vec3::new(-t.sin, 0., t.cos) * t.scale;
        let p = Vec3::from_array(t.position) + wind * self.intercept;
        Attachment::new(Mat4::from_cols(
            x.extend(0.),
            y.extend(0.),
            z.extend(0.),
            p.extend(1.),
        ))
    }
}
fn wind_local(t: Transform, v: Vec3) -> Vec3 {
    Vec3::new(t.cos * v.x + t.sin * v.z, v.y, -t.sin * v.x + t.cos * v.z) / t.scale
}

fn tree_references(lod: &crate::trees::TreeLod, authored: bool) -> [Option<(f32, bool)>; 3] {
    let alpha = |value: Option<f32>, default: Option<f32>| {
        if authored {
            value
                .filter(|v| *v >= 0.)
                .map_or(default.map(|r| (r, false)), |v| Some((v / 255., true)))
        } else {
            default.map(|r| (r, false))
        }
    };
    [
        alpha(lod.branch_alpha_ref, None),
        alpha(lod.frond_alpha_ref, Some(0.5)),
        if authored {
            Some((84. / 255., true))
        } else {
            Some((0.5, false))
        },
    ]
}

fn tree_filter(reference: Option<(f32, bool)>) -> Filter {
    reference.map_or(Filter::Color, |(r, strict)| Filter::cutout(r, strict))
}

pub(super) fn tree_texture_bytes(
    source: &crate::trees::TreeMesh,
    side: u32,
    authored: bool,
) -> usize {
    let mut unique = std::collections::HashSet::new();
    let mut bytes = 0usize;
    for lod in &source.lods {
        for (i, reference) in tree_references(lod, authored).into_iter().enumerate() {
            let bark = i == 0;
            if unique.insert((bark, tree_filter(reference))) {
                let image = if bark {
                    &source.bark
                } else {
                    &source.composite
                };
                bytes =
                    bytes.saturating_add(super::chain_bytes([image.width(), image.height()], side));
            }
        }
    }
    bytes
}

pub(super) fn read_tree(
    source: &crate::trees::TreeMesh,
    textures: &mut Textures,
    side: u32,
    authored: bool,
) -> Result<Tree, String> {
    let xyz = |p: [f32; 3]| [p[0], p[2], p[1]];
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
        let thresholds = tree_references(lod, authored);
        let mut materials = [Surface::new(0, 0, 0., 0.); 3];
        for (i, reference) in thresholds.into_iter().enumerate() {
            let (image, mips) = if i == 0 {
                (&source.bark, &source.bark_mips)
            } else {
                (&source.composite, &source.composite_mips)
            };
            let mode = tree_filter(reference);
            let addr = match tiles.entry((i == 0, mode)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    *e.insert(textures.images(std::iter::once(image).chain(mips), side, mode)?)
                }
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
        let x = LeafFacing::new(Vec3::X);
        let first = lod.sample(x, 0., 1.);
        let rotated = lod.sample(LeafFacing::new(Vec3::Z), 0., 1.);
        let rocked = lod.sample(x, 0.125, 1.);
        for i in 0..3 {
            assert_eq!(first[i].triangles, rotated[i].triangles);
            assert_eq!(first[i].triangles, rocked[i].triangles);
        }
        assert!(first[0].triangles.is_empty());
        assert_eq!(first[1].surfaces, [2, 2]);
        assert_eq!(first[2].surfaces, [3, 3]);
        assert_ne!(first[1].vertices[0].pos, rotated[1].vertices[0].pos);
        assert_eq!(first[2].vertices[0].pos, rotated[2].vertices[0].pos);
        assert!((rocked[2].vertices[0].pos[2] - first[2].vertices[0].pos[2] - 0.0625).abs() < 1e-6);
        assert_eq!(
            first[2].vertices[0].pos,
            lod.sample(x, 16., 1.)[2].vertices[0].pos
        );
    }

    #[test]
    fn plant_instances_reuse_one_blas_for_independent_wind_histories() {
        let mut mesh = Mesh::default();
        let points = [[-0.5, 0., 0.], [0.5, 0., 0.], [-0.5, 1., 0.], [0.5, 1., 0.]];
        for p in points {
            mesh.vertices.push(
                Vertex::new(p, [0., 0., 1.], [0.; 2]).grass(100f32.to_bits(), p[1].to_bits()),
            );
        }
        mesh.tri([0, 1, 2], 0);
        mesh.tri([2, 1, 3], 0);
        let bend = GrassBend::fit(&mesh.vertices);
        assert_eq!(bend.error, 0.);
        let grass = Grass(vec![GrassGroup {
            mesh,
            bend,
            material: Surface::new(0, GRASS_MATERIAL | CUTOUT, 0.5, 0.),
            instances: (0..64)
                .map(|i| {
                    let (s, c) = (i as f32 * 0.1).sin_cos();
                    [i as f32, 0., 0., 1.25, c, s, i as f32, 0., 0., 0., 0., 0.]
                })
                .collect(),
        }]);
        let mut f = FrameSnapshot::default();
        f.grass_wind[1] = 1.;
        for (i, h) in f.grass_history.iter_mut().enumerate() {
            *h = [0.2 + 0.004 * i as f32, -0.17, 0.05, 0.];
        }
        let mut b = Builder::for_mode(crate::modules::lighter::Mode::Hardware);
        let mut budget = b.optional_mesh_budget(16 << 20);
        let mut counts = Counts::default();
        grass
            .append(&f, &mut b, &mut counts, &mut budget, false)
            .unwrap();
        let packed = b.finish();
        assert_eq!(
            (packed.meshes.len(), packed.instances.len(), counts.loaded),
            (1, 64, 64)
        );
        for i in 0..packed.instances.len() {
            let offsets = &packed.words;
            let instance = offsets[4] as usize + i * INSTANCE_WORDS as usize;
            assert_eq!(offsets[instance + 3], 0, "expected an affine blade");
            let at = offsets[instance + 4] as usize;
            let history = Vec3::from_slice(&f.grass_history[i]);
            // The Y axis of the deformation is the actual displaced tip.
            let y = Vec3::from_array(
                [offsets[at + 3], offsets[at + 4], offsets[at + 5]].map(f32::from_bits),
            );
            assert!((y - 1.25 * Vec3::Y - history).length() < 1e-6);
        }
        let middle = Vertex::new([0., 0.5, 0.], [0., 0., 1.], [0.; 2]).grass(0, 0.25f32.to_bits());
        let mut complex = grass.0[0].mesh.vertices.clone();
        complex.push(middle);
        assert_eq!(GrassBend::fit(&complex).error, 0.25);
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
