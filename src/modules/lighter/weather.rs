//! Shared native art and inputs. The view precipitation is a bounded card
//! approximation; the cloud shadow proxies live in world space.
use super::{FrameSnapshot, Terrain};
use crate::modules::lighter::{
    scene::*,
    textures::{Filter, Textures},
};
use glam::Vec3;

#[derive(Default)]
pub(super) struct Weather {
    weather: [u32; 2],
    cloud: Option<(Mesh, Vec<Surface>)>,
}

fn card(
    mesh: &mut Mesh,
    p: Vec3,
    x: Vec3,
    y: Vec3,
    rect: [f32; 4],
    tint: [u32; 2],
    mat: u32,
    layer: u32,
) {
    let base = mesh.vertices.len() as u32;
    for (i, pos) in [p - x + y, p + x + y, p - x - y, p + x - y]
        .into_iter()
        .enumerate()
    {
        let uv = [
            rect[0] + (i % 2) as f32 * rect[2],
            rect[1] + (i / 2) as f32 * rect[3],
        ];
        mesh.vertices
            .push(Vertex::new(pos.to_array(), [0., 1., 0.], uv).tint(tint[0], tint[1]));
    }
    mesh.tri_layer([base, base + 1, base + 3], mat, layer);
    mesh.tri_layer([base, base + 3, base + 2], mat, layer);
}

impl Weather {
    pub(super) fn load(t: &Terrain, tex: &mut Textures) -> Result<Self, String> {
        let mut out = Self::default();
        for (id, img) in out
            .weather
            .iter_mut()
            .zip(crate::weather_gpu::weather_images(&t.element))
        {
            *id = tex.images(&[img], 64, Filter::Alpha)?;
        }
        if let Some(surface) = &t.cloud_surface {
            let cards = crate::cloud_gpu::billboard_instances(surface);
            if !cards.is_empty() {
                match crate::cloud_gpu::cumulus_texture(&t.element) {
                    Ok(image) => {
                        let mut material = Vec::new();
                        let mut slots = [None; 16];
                        let mut mesh = Mesh::default();
                        for (i, c) in cards.iter().take(4096).enumerate() {
                            // Native samples a four-by-four atlas in level zero.
                            // Isolate each sprite before generating ray mips.
                            let x = (c.atlas_uv[0] * 4.).floor() as u32;
                            let y = (c.atlas_uv[1] * 4.).floor() as u32;
                            let slot = (y * 4 + x) as usize;
                            let mat = if let Some(mat) = slots[slot] {
                                mat
                            } else {
                                let w = image.width() / 4;
                                let h = image.height() / 4;
                                if w == 0 || h == 0 {
                                    break;
                                }
                                let part = image::imageops::crop_imm(&image, x * w, y * h, w, h)
                                    .to_image();
                                let addr = tex.images(&[part], 128, Filter::Alpha)?;
                                let mat = material.len() as u32;
                                material.push(Surface::new(
                                    addr,
                                    CLOUD | BLEND | CLAMP_UV | EMISSIVE,
                                    0.,
                                    0.,
                                ));
                                slots[slot] = Some(mat);
                                mat
                            };
                            let [size, r, g, b] = c.height_night;
                            let center = Vec3::from_slice(&c.center_width);
                            card(
                                &mut mesh,
                                center,
                                Vec3::X * c.center_width[3] * 0.5,
                                Vec3::Z * size * 0.5,
                                [0., 0., 1., 1.],
                                [rgba(c.day_color), rgba([r, g, b, 1.])],
                                mat,
                                i as u32 + 1,
                            );
                        }
                        if cards.len() > 4096 {
                            eprintln!("Lighter: kept 4096 of {} authored cloud cards", cards.len());
                        }
                        if !mesh.triangles.is_empty() {
                            out.cloud = Some((mesh, material));
                        }
                    }
                    Err(e) => eprintln!("Lighter: authored cloud detail unavailable: {e}"),
                }
            }
        }
        Ok(out)
    }

    pub(super) fn clouds(&self, b: &mut Builder) -> Result<(), String> {
        if let Some((cloud, materials)) = &self.cloud {
            let ids: Vec<_> = materials.iter().map(|&m| b.surface(m)).collect();
            b.chunk_mapped(ChunkKey::Clouds, cloud, &ids)?;
        }
        Ok(())
    }

    pub(super) fn precipitation(
        &self,
        eye: Vec3,
        yaw: f32,
        frame: &FrameSnapshot,
        b: &mut Builder,
        budget: &mut MeshBudget,
    ) -> Result<(), String> {
        use crate::client_weather::{RAIN, SNOW, SNOW_SPEED_SCALE};
        if !frame.weather.rain && !frame.weather.snow {
            return Ok(());
        }
        let (s, c) = yaw.sin_cos();
        let right = Vec3::new(c, 0., s);
        let up = right.cross(frame.direction).normalize_or_zero();
        let mut mesh = Mesh::default();
        for (kind, count, profile, enabled) in [
            (0, 320, RAIN, frame.weather.rain),
            (1, 160, SNOW, frame.weather.snow),
        ] {
            if !enabled {
                continue;
            }
            let snow = kind == 1;
            let slot = b.surface(Surface::new(
                self.weather[kind],
                BLEND | CLAMP_UV | PRECIPITATION,
                0.,
                0.,
            ));
            let tint = rgba(if snow {
                [0.91, 0.95, 1., 0.82]
            } else {
                [0.66, 0.78, 0.91, 0.52]
            });
            for i in 0..count {
                let seed = i as f32 + kind as f32 * 8192.;
                let z = 4. + hash(seed + 3.) * 72.;
                let x = (hash(seed + 1.) * 2. - 1.) * 28.;
                let speed = profile.speed
                    * (1. + (hash(seed + 11.) * 2. - 1.) * profile.speed_variation.unwrap_or(0.))
                    * if snow { SNOW_SPEED_SCALE } else { 1. };
                let y = fall(seed, frame.weather_ticks, speed);
                let half = 0.5
                    * profile.size
                    * (1. + (hash(seed + 5.) * 2. - 1.) * profile.size_variation.unwrap_or(0.));
                let p = eye + right * x + frame.direction * z + Vec3::Y * y;
                let v = if snow {
                    up * half
                } else {
                    Vec3::Y * (1.1 + hash(seed + 17.) * 1.3)
                };
                let layer = mesh.triangles.len() as u32 / 2 + 1;
                card(
                    &mut mesh,
                    p,
                    right * half,
                    v,
                    [0., 0., 1., 1.],
                    [tint; 2],
                    slot,
                    layer,
                );
            }
        }
        if budget.debit(&mesh, 0) {
            b.posed_world_mesh("weather", 0, &mesh)?;
        }
        Ok(())
    }
}
fn hash(seed: f32) -> f32 {
    ((seed * 127.1 + 311.7).sin() * 43758.5453).rem_euclid(1.)
}
fn fall(seed: f32, tick: u32, speed: f32) -> f32 {
    55. - (hash(seed + 7.) + tick as f32 * 0.033 * speed).rem_euclid(1.) * 110.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weather_uses_downward_motion_and_wrapped_native_clock() {
        let windows = (0..20).map(|i| fall(0., i, 0.5)).collect::<Vec<_>>();
        assert!(windows
            .windows(2)
            .all(|y| y[0] > y[1] || y[1] - y[0] > 100.));
        assert!(windows.iter().all(|v| (-55. ..=55.).contains(v)));
    }
}
