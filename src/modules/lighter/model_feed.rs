//! Feed posed meshes and effect cards into the same moving ray scene. Each
//! module keeps its native archive lookup and animation ownership. No draw
//! queues, framebuffer data, or main-view frustum are needed here.
use super::{
    coverage::{Class, Inventory},
    scene::*,
    textures::{Filter, Textures},
};
use crate::{
    gfx_player::EffectQuad,
    model_asset::{FaceShader, ModelAsset, PosedMesh},
    skeleton_pose::{self, Pose},
};

pub(crate) type Decode<'a> = dyn FnMut(&str) -> Result<(Vec<image::RgbaImage>, bool), String> + 'a;

pub(crate) struct Posed<'a> {
    pub asset: &'a ModelAsset,
    pub pose: &'a Pose,
    pub meshes: &'a [PosedMesh],
    /// This model's frame follows a parent (face, hanger, flight attachment,
    /// or mounted rider). Keep even its weighted skin in that child frame.
    pub attached: bool,
}

pub(crate) struct Moving<'a> {
    pub builder: Builder,
    pub coverage: Inventory,
    effects: Mesh,
    textures: &'a mut Textures,
    layer: u32,
}

impl<'a> Moving<'a> {
    pub fn new(textures: &'a mut Textures) -> Self {
        Self {
            builder: Builder::default(),
            coverage: Inventory::default(),
            effects: Mesh::default(),
            textures,
            layer: 0,
        }
    }

    pub fn model(
        &mut self,
        namespace: &'static str,
        owner: u64,
        model: Posed<'_>,
        primary_visible: impl Fn(usize) -> bool,
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        // Use the normal renderer's actual topology/pose/texture resolver.
        // A broken rig isn't a reason to drop the rest of this scene.
        let Posed {
            asset,
            pose,
            meshes,
            attached,
        } = model;
        let class = match namespace {
            "world" => Class::WorldModels,
            "walker" => Class::Walker,
            "populator" => Class::Populator,
            _ => return Err(format!("unknown Lighter model namespace {namespace:?}")),
        };
        let counts = &mut self.coverage[class];
        let Ok(prepared) = crate::model_draw::prepare(asset, meshes) else {
            counts.invalid += 1;
            return Ok(());
        };
        if prepared.is_empty() {
            counts.empty += 1;
            return Ok(());
        }
        let settings = asset.descriptor.render_settings();
        let mut geometry = Mesh::default();
        // An authored zero-scale/singular bone can collapse its skin. Do
        // not invent an inverse: leave that part in the evaluated world mesh.
        let parent = attached.then(|| Attachment::new(pose.model)).flatten();
        for part in prepared {
            // Match MTL_SCALED: the mesh override scales each coefficient,
            // not just the directional term. Dedicated face stages get white.
            let tint = argb_to_rgba(part.color)
                .to_le_bytes()
                .map(|b| f32::from(b) / 255.);
            let scale = |c| {
                if part.color == u32::MAX {
                    c
                } else {
                    crate::model_material::scale_rgba(c, tint)
                }
            };
            let d = scale(part.material.diffuse);
            let mut flags = if settings.destination_blend == 2 {
                BLEND | ADDITIVE
            } else if settings.source_blend != 5 || settings.destination_blend != 6 || d[3] < 0.99 {
                BLEND
            } else {
                0
            };
            if !part.material.two_sided {
                flags |= SINGLE_SIDED;
            }
            if !primary_visible(part.skin) {
                flags |= HIDE_PRIMARY;
            }
            let body = matches!(part.face_shader, Some(FaceShader::Body { .. }));
            let filter = if body || part.diffuse_alpha_only {
                flags |= IGNORE_TEXTURE_ALPHA;
                if body {
                    Filter::ColorMask
                } else {
                    Filter::Color
                }
            } else if flags & BLEND != 0 {
                Filter::Alpha
            } else {
                Filter::cutout(0.5, false)
            };
            let mut loaded = None;
            for path in &part.texture_paths {
                loaded = self.textures.archive(namespace, path, filter, decode)?;
                if loaded.is_some() {
                    break;
                }
            }
            counts.texture_fallback +=
                usize::from(loaded.is_none() && !part.texture_paths.is_empty());
            let (texture, alpha) = loaded.unwrap_or((0, false));
            let tint = rgba([1., 1., 1., d[3]]);
            let gloss = Gloss::authored(scale(part.material.specular), part.material.power);
            if alpha && flags & (BLEND | IGNORE_TEXTURE_ALPHA) == 0 {
                flags |= CUTOUT;
            }
            let mut surface = Surface::new(texture, flags, 0.5, 0.)
                .with_gloss(gloss)
                .with_lighting(
                    d,
                    scale(part.material.ambient),
                    scale(part.material.emissive),
                );
            if let Some(stage) = part.face_shader {
                let mut load_stage = |name, fallback| -> Result<u32, String> {
                    let loaded = self.textures.archive(namespace, name, filter, decode)?;
                    counts.texture_fallback += usize::from(loaded.is_none());
                    Ok(loaded.map_or(fallback, |(addr, _)| addr))
                };
                let hi = if let Some(name) = stage.highlight() {
                    load_stage(name, 0)?
                } else {
                    0
                };
                let gloss = if let Some(name) = stage.gloss() {
                    load_stage(name, 0)?
                } else {
                    0
                };
                let (mode, tone, skin) = match stage {
                    FaceShader::Face { color } => (1, *color, 0),
                    FaceShader::Body { color } => (2, *color, 0),
                    FaceShader::EyeBall { color, .. } => (3, *color, 0),
                    FaceShader::Brow {
                        color, skin_color, ..
                    }
                    | FaceShader::Eye {
                        color, skin_color, ..
                    }
                    | FaceShader::Mouth {
                        color, skin_color, ..
                    }
                    | FaceShader::Moustache {
                        color, skin_color, ..
                    } => (4, *color, *skin_color),
                };
                surface = surface.face_stage(mode, tone, skin, hi, gloss);
            }
            let id = self.builder.surface(surface);
            let source = &asset.skins[part.skin].data.meshes[part.mesh];
            if let Some(placement) = source
                .rigid_bone
                .and_then(|bone| skeleton_pose::rigid_matrix(pose, bone).ok())
                .and_then(Attachment::new)
            {
                // Rigid SKI vertices already live in the direct blend-matrix
                // bind space. A moving hook changes the TLAS, not this BLAS.
                let mut child = Mesh::default();
                child.vertices.extend(
                    source
                        .vertices
                        .iter()
                        .map(|v| Vertex::new(v.position, v.normal, v.uv).tint(tint, tint)),
                );
                append_triangles(&mut child, &part.indices, 0, id);
                self.builder.rigid_part(
                    namespace,
                    owner,
                    (part.skin, part.mesh),
                    &child,
                    placement,
                )?;
                continue;
            }
            let base = geometry.vertices.len() as u32;
            geometry.vertices.extend(part.vertices.iter().map(|v| {
                let (p, n) = parent.map_or((v.position, v.normal), |a| {
                    (a.local_point(v.position), a.local_vector(v.normal))
                });
                Vertex::new(p, n, v.uv).tint(tint, tint)
            }));
            append_triangles(&mut geometry, &part.indices, base, id);
        }
        self.builder
            .posed_mesh(namespace, owner, &geometry, parent)?;
        counts.loaded += 1;
        Ok(())
    }

    fn blend(&mut self, src: u32, dst: u32) -> Option<u32> {
        // HDR front-to-back compositing can preserve expressions A+B*dst.
        // Destination alpha and nonlinear destination-color factors need a
        // separate encoded framebuffer pass: report them, don't substitute 5/6.
        if matches!(src, 1..=6 | 9 | 10) && matches!(dst, 1..=6) {
            Some(src | (dst << 4))
        } else {
            self.coverage[Class::EffectDraws].unsupported += 1;
            None
        }
    }

    pub fn effects(
        &mut self,
        namespace: &'static str,
        quads: &[EffectQuad],
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        // Same priority bracket as the native effect slot manager. Stable
        // sort also preserves draw order within its equal-state buckets.
        let mut sorted: Vec<_> = quads.iter().collect();
        sorted.sort_by_key(|q| {
            let (texture, write) = q
                .mesh
                .as_ref()
                .map_or((&q.texture, false), |m| (&m.texture, m.write_depth));
            (
                q.layer,
                !write,
                q.priority,
                !q.depth_test,
                q.source_blend,
                q.destination_blend,
                texture,
                q.tile,
            )
        });
        for q in sorted {
            let Some(blend) = self.blend(q.source_blend, q.destination_blend) else {
                continue;
            };
            let flags = BLEND
                | ORDERED_GFX
                | if q.tile { 0 } else { CLAMP_UV }
                | if !q.depth_test {
                    OVERLAY | PRIMARY_ONLY
                } else {
                    0
                }
                | if q.ray_placement == crate::gfx_player::RayPlacement::Screen {
                    PRIMARY_ONLY
                } else {
                    0
                };
            let filter = if q.source_blend == 5 {
                Filter::Alpha
            } else {
                Filter::ColorMask
            };
            if q.ray_placement == crate::gfx_player::RayPlacement::Ground && q.depth_test {
                let loaded = self
                    .textures
                    .archive(namespace, &q.texture, filter, decode)?;
                let id = loaded.map_or(0, |v| v.0);
                let c = self.builder.project_decal(
                    q.positions,
                    q.uv,
                    q.vertex_colors.unwrap_or([q.color; 4]).map(rgba),
                    id,
                    blend | (flags & CLAMP_UV),
                );
                let row = &mut self.coverage[Class::EffectDraws];
                match c {
                    Ok(()) => {
                        row.loaded += 1;
                        row.texture_fallback += usize::from(loaded.is_none());
                    }
                    Err(true) => row.budget += 1,
                    Err(false) => row.invalid += 1,
                }
                continue;
            }
            if let Some(skin) = &q.mesh {
                if skin.vertices.len() < 3 {
                    self.coverage[Class::EffectDraws].empty += 1;
                    continue;
                }
                let loaded = self
                    .textures
                    .archive(namespace, &skin.texture, filter, decode)?;
                self.coverage[Class::EffectDraws].texture_fallback += usize::from(loaded.is_none());
                self.coverage[Class::EffectDraws].loaded += 1;
                let (texture, has_alpha) = loaded.unwrap_or((0, true));
                let mut flags = flags
                    | if skin.write_depth && q.depth_test {
                        WRITE_DEPTH
                    } else {
                        0
                    }
                    | if skin.material.is_none() { EMISSIVE } else { 0 };
                let scale = |c| crate::model_material::scale_rgba(c, q.color);
                let (color, lighting, gloss) = skin.material.map_or_else(
                    || (rgba(q.color), None, Gloss::default()),
                    |m| {
                        let diffuse = scale(m.diffuse);
                        (
                            rgba([1., 1., 1., diffuse[3]]),
                            Some((diffuse, scale(m.ambient), scale(m.emissive))),
                            Gloss::authored(scale(m.specular), m.power),
                        )
                    },
                );
                // Fully opaque standard-alpha, depth-writing GFX skins have
                // the same visibility as a solid. Do not promote translucent
                // or additive effects out of the ordered sheet queue.
                if gloss.reflective()
                    && skin.write_depth
                    && q.depth_test
                    && !has_alpha
                    && color >> 24 == 255
                    && q.source_blend == 5
                    && q.destination_blend == 6
                {
                    flags &= !(BLEND | WRITE_DEPTH | ORDERED_GFX);
                }
                let mut surface = Surface::new(texture, flags, 0., 0.)
                    .with_gloss(gloss)
                    .with_blend(blend);
                if let Some((diffuse, ambient, emission)) = lighting {
                    surface = surface.with_lighting(diffuse, ambient, emission);
                }
                let id = self.builder.surface(surface);
                self.layer += 1;
                for tri in skin.vertices.chunks_exact(3) {
                    let base = self.effects.vertices.len() as u32;
                    self.effects.vertices.extend(
                        tri.iter()
                            .map(|v| Vertex::new(v.position, v.normal, v.uv).tint(color, color)),
                    );
                    self.effects
                        .tri_layer([base, base + 1, base + 2], id, self.layer);
                }
            } else {
                self.card(
                    namespace,
                    &q.texture,
                    flags,
                    blend,
                    filter,
                    q.positions,
                    q.uv,
                    q.vertex_colors.unwrap_or([q.color; 4]),
                    decode,
                )?;
            }
        }
        Ok(())
    }

    pub fn decals(
        &mut self,
        namespace: &'static str,
        decals: &[crate::gfx_decal::Quad],
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        for q in decals {
            let Some(blend) = self.blend(q.source_blend, q.destination_blend) else {
                continue;
            };
            let flags = BLEND
                | ORDERED_GFX
                | CLAMP_UV
                | if q.depth_test {
                    0
                } else {
                    OVERLAY | PRIMARY_ONLY
                };
            let filter = if q.source_blend == 5 {
                Filter::Alpha
            } else {
                Filter::ColorMask
            };
            self.card(
                namespace,
                &q.texture,
                flags,
                blend,
                filter,
                q.vertices.each_ref().map(|v| v.position),
                q.vertices.each_ref().map(|v| v.uv),
                q.vertices.each_ref().map(|v| v.color),
                decode,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn card(
        &mut self,
        namespace: &'static str,
        name: &str,
        flags: u32,
        blend: u32,
        filter: Filter,
        positions: [[f32; 3]; 4],
        uv: [[f32; 2]; 4],
        color: [[f32; 4]; 4],
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        let loaded = self.textures.archive(namespace, name, filter, decode)?;
        self.coverage[Class::EffectDraws].texture_fallback += usize::from(loaded.is_none());
        self.coverage[Class::EffectDraws].loaded += 1;
        let tex = loaded.map_or(0, |v| v.0);
        let base = self.effects.vertices.len() as u32;
        let id = self
            .builder
            .surface(Surface::new(tex, flags | EMISSIVE, 0., 0.).with_blend(blend));
        for j in 0..4 {
            let color = rgba(color[j]);
            self.effects
                .vertices
                .push(Vertex::new(positions[j], [0., 1., 0.], uv[j]).tint(color, color));
        }
        self.layer += 1;
        // Match the native effect pass's diagonal for authored corner colours.
        self.effects
            .tri_layer([base, base + 1, base + 3], id, self.layer);
        self.effects
            .tri_layer([base, base + 3, base + 2], id, self.layer);
        Ok(())
    }

    pub(crate) fn finish_cached(mut self, cache: &mut AccelCache) -> Result<Packet, String> {
        // The aggregate GFX batch may change its primitive count as particles
        // are born and die. It cannot invalidate any resident actor BLAS.
        self.builder.posed_world_mesh("gfx", 0, &self.effects)?;
        Ok(self.builder.finish_cached(cache))
    }

    #[cfg(test)]
    fn finish(self) -> Result<Packet, String> {
        self.finish_cached(&mut AccelCache::default())
    }
}

fn append_triangles(geometry: &mut Mesh, indices: &[u16], base: u32, surface: u32) {
    for i in indices.chunks_exact(3) {
        geometry.tri(
            [
                base + u32::from(i[0]),
                base + u32::from(i[1]),
                base + u32::from(i[2]),
            ],
            surface,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx_player::{EffectMesh, EffectVertex, MeshMaterial};

    #[test]
    fn opaque_gfx_skins_keep_authored_power_without_promoting_sheets() {
        let mut quad = EffectQuad {
            ray_placement: Default::default(),
            texture: String::new(),
            source_blend: 5,
            destination_blend: 6,
            depth_test: true,
            layer: 0,
            priority: 0,
            tile: false,
            positions: [[0.; 3]; 4],
            uv: [[0.; 2]; 4],
            color: [1.; 4],
            vertex_colors: None,
            mesh: Some(EffectMesh {
                texture: "fixture.png".into(),
                vertices: [[0., 0., -1.], [1., 0., -1.], [0., 1., -1.]]
                    .map(|position| EffectVertex {
                        position,
                        uv: [0.; 2],
                        normal: [0., 0., 1.],
                    })
                    .to_vec(),
                write_depth: true,
                material: Some(MeshMaterial {
                    ambient: [0.5; 4],
                    diffuse: [0.8, 1., 1., 1.],
                    emissive: [0.125, 0., 0., 1.],
                    specular: [1.; 4],
                    power: 37.,
                }),
            }),
        };
        let pack = |q: &EffectQuad, has_alpha| {
            let mut bank = Textures::default();
            let mut moving = Moving::new(&mut bank);
            moving
                .effects("fixture", std::slice::from_ref(q), &mut |_| {
                    Ok((
                        vec![image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]))],
                        has_alpha,
                    ))
                })
                .unwrap();
            moving.finish().unwrap().words
        };
        let solid = pack(&quad, false);
        let material = solid[3] as usize;
        assert_eq!(solid[11], u32::from(MASK_SOLID | MASK_SHADOW));
        assert_eq!(f32::from_bits(solid[material + 3]), 0.25);
        assert_eq!(f32::from_bits(solid[material + 4]), 37.);
        assert_eq!(solid[material + 10], rgba([0.5; 4]));
        assert_eq!(solid[material + 20], rgba([0.8, 1., 1., 1.]));
        assert_eq!(f32::from_bits(solid[material + 7]), 0.125);
        let alpha = pack(&quad, true);
        assert_eq!(alpha[11], u32::from(MASK_BLEND | MASK_SHADOW));
        quad.color[3] = 0.5;
        let sheet = pack(&quad, false);
        assert_eq!(sheet[11], u32::from(MASK_BLEND | MASK_SHADOW));
        assert_ne!(sheet[sheet[3] as usize + 1] & WRITE_DEPTH, 0);
    }
}
