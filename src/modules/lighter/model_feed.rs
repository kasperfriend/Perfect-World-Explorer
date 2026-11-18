//! Feed posed meshes and effect cards into the same moving ray scene. Each
//! module keeps its native archive lookup and animation ownership. No draw
//! queues, framebuffer data, or main-view frustum are needed here.
use super::{scene::*, textures::Textures};
use crate::{
    gfx_player::EffectQuad,
    model_asset::{ModelAsset, PosedMesh},
};

pub(crate) type Decode<'a> = dyn FnMut(&str) -> Result<(image::RgbaImage, bool), String> + 'a;

pub(crate) struct Moving<'a> {
    pub builder: Builder,
    geometry: Mesh,
    textures: &'a mut Textures,
}

impl<'a> Moving<'a> {
    pub fn new(textures: &'a mut Textures) -> Self {
        Self {
            builder: Builder::default(),
            geometry: Mesh::default(),
            textures,
        }
    }

    pub fn model(
        &mut self,
        namespace: &'static str,
        asset: &ModelAsset,
        poses: &[PosedMesh],
        include_skin: impl Fn(usize) -> bool,
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        // Use the normal renderer's actual topology/pose/texture resolver.
        // A broken rig isn't a reason to drop the rest of this scene.
        let Ok(prepared) = crate::model_draw::prepare(asset, poses) else {
            return Ok(());
        };
        let settings = asset.descriptor.render_settings();
        for part in prepared.into_iter().filter(|part| include_skin(part.skin)) {
            let mut loaded = None;
            for path in &part.texture_paths {
                loaded = self.textures.archive(namespace, path, decode)?;
                if loaded.is_some() {
                    break;
                }
            }
            let (texture, alpha) = loaded.unwrap_or((0, false));
            let tint = argb_to_rgba(part.color)
                .to_le_bytes()
                .map(|b| f32::from(b) / 255.);
            let d = part.material.diffuse;
            let tint = rgba([tint[0] * d[0], tint[1] * d[1], tint[2] * d[2], d[3]]);
            let reflect = part.material.specular[..3].iter().sum::<f32>() / 12.;
            let flags = if settings.destination_blend == 2 {
                BLEND | ADDITIVE
            } else if settings.source_blend != 5 || settings.destination_blend != 6 || d[3] < 0.99 {
                BLEND
            } else if alpha && !part.diffuse_alpha_only {
                CUTOUT
            } else {
                0
            };
            let id = self
                .builder
                .surface(Surface::new(texture, flags, 0.5, reflect.min(0.35)));
            let base = self.geometry.vertices.len() as u32;
            self.geometry.vertices.extend(
                part.vertices
                    .iter()
                    .map(|v| Vertex::new(v.position, v.normal, v.uv).tint(tint, tint)),
            );
            for i in part.indices.chunks_exact(3) {
                self.geometry.tri(
                    [
                        base + u32::from(i[0]),
                        base + u32::from(i[1]),
                        base + u32::from(i[2]),
                    ],
                    id,
                );
            }
        }
        Ok(())
    }

    pub fn effects(
        &mut self,
        namespace: &'static str,
        quads: &[EffectQuad],
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        for q in quads {
            let flags = BLEND
                | if q.destination_blend == 2 {
                    ADDITIVE
                } else {
                    0
                }
                | if q.tile { 0 } else { CLAMP_UV };
            if let Some(skin) = &q.mesh {
                let texture = self
                    .textures
                    .archive(namespace, &skin.texture, decode)?
                    .map_or(0, |v| v.0);
                let flags = flags
                    | if skin.write_depth { WRITE_DEPTH } else { 0 }
                    | if skin.material.is_none() { EMISSIVE } else { 0 };
                let d = skin.material.map_or([1.; 4], |m| m.diffuse);
                let color = rgba(std::array::from_fn(|i| q.color[i] * d[i]));
                let id = self.builder.surface(Surface::new(texture, flags, 0., 0.));
                for tri in skin.vertices.chunks_exact(3) {
                    let base = self.geometry.vertices.len() as u32;
                    self.geometry.vertices.extend(
                        tri.iter()
                            .map(|v| Vertex::new(v.position, v.normal, v.uv).tint(color, color)),
                    );
                    self.geometry.tri([base, base + 1, base + 2], id);
                }
            } else {
                self.card(
                    namespace,
                    &q.texture,
                    flags,
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
            let flags = BLEND
                | CLAMP_UV
                | if q.destination_blend == 2 {
                    ADDITIVE
                } else {
                    0
                };
            self.card(
                namespace,
                &q.texture,
                flags,
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
        positions: [[f32; 3]; 4],
        uv: [[f32; 2]; 4],
        color: [[f32; 4]; 4],
        decode: &mut Decode<'_>,
    ) -> Result<(), String> {
        let tex = self
            .textures
            .archive(namespace, name, decode)?
            .map_or(0, |v| v.0);
        let base = self.geometry.vertices.len() as u32;
        let id = self
            .builder
            .surface(Surface::new(tex, flags | EMISSIVE, 0., 0.));
        for j in 0..4 {
            let color = rgba(color[j]);
            self.geometry
                .vertices
                .push(Vertex::new(positions[j], [0., 1., 0.], uv[j]).tint(color, color));
        }
        self.geometry.tri([base, base + 1, base + 2], id);
        self.geometry.tri([base + 2, base + 1, base + 3], id);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Packet, String> {
        self.builder.world_mesh(&self.geometry)?;
        Ok(self.builder.finish())
    }
}
