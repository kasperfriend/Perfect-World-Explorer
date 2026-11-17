//! Offscreen item-model preview. The target is independent of the world depth
//! buffer and follows the viewer's mirrored, reversed-depth camera convention.
use super::archive::LibrarianAssets;
use crate::{
    model_asset::ModelAsset,
    model_draw_gpu::{Draw, Frame},
    model_material::Lighting,
};
use glam::{Mat4, Vec3};

// The model shader blends encoded colors; the window surface may be sRGB.
const PREVIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// GFX can contain many independently animated elements. This intentionally
// extracts only the first embedded skin model; it is not a GFX renderer.
fn effect_model_paths(text: &str) -> Vec<String> {
    let mut kind = "";
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("GFXELEMENTID:") {
            kind = value.trim();
        } else if kind == "160" {
            if let Some(value) = line.strip_prefix("ModelPath:") {
                let path = crate::ecmodel::normalize(value);
                if !path.is_empty()
                    && path.ends_with(".smd")
                    && !path
                        .split('/')
                        .any(|part| part == "." || part == ".." || part.contains(':'))
                {
                    paths.push(format!("gfx/models/{path}"));
                }
            }
        }
    }
    paths
}

fn load_effect_model(
    assets: &mut LibrarianAssets,
    asset: &ModelAsset,
) -> Result<ModelAsset, String> {
    let mut errors = Vec::new();
    for gfx in asset
        .descriptor
        .events
        .iter()
        .filter_map(|event| event.gfx_path())
    {
        let gfx = crate::ecmodel::normalize(&gfx);
        if gfx
            .split('/')
            .any(|part| part == "." || part == ".." || part.contains(':'))
        {
            continue;
        }
        let path = if gfx.starts_with("gfx/") {
            gfx
        } else {
            format!("gfx/{gfx}")
        };
        let data = match assets.read_model_resource(&path) {
            Ok(data) => data,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let (text, _, bad) = encoding_rs::GBK.decode(&data);
        if bad {
            errors.push(format!("{path}: invalid GBK GFX"));
            continue;
        }
        for model in effect_model_paths(&text) {
            let result = (|| {
                let smd = crate::ecmodel::parse_smd(&assets.read_model_resource(&model)?)?;
                let parent = model.rsplit_once('/').map_or("", |v| v.0);
                let relative = |value: &str| {
                    let path = crate::ecmodel::normalize(value);
                    if path.starts_with("gfx/") || path.starts_with("models/") {
                        path
                    } else {
                        format!("{parent}/{path}")
                    }
                };
                let skeleton = relative(&smd.skeleton);
                let skins: Vec<_> = smd.skins.iter().map(|p| relative(p)).collect();
                ModelAsset::load_static_skins_with(&skeleton, &skins, |p| {
                    assets.read_model_resource(p)
                })
            })();
            match result {
                Ok(model) => return Ok(model),
                Err(error) => errors.push(error),
            }
        }
    }
    Err(format!(
        "Effect-only model has no supported embedded GFX model{}",
        if errors.is_empty() {
            String::new()
        } else {
            format!(": {}", errors.join("; "))
        }
    ))
}

// The client reflection replacement uses R_ texture alpha as a specular
// mask, not opacity. Other texture names retain their authored opacity.
fn apply_reflect_alpha(asset: &mut ModelAsset) {
    for (skin_index, skin) in asset.skins.iter().enumerate() {
        for (mesh_index, mesh) in skin.data.meshes.iter().enumerate() {
            let reflect = usize::try_from(mesh.texture)
                .ok()
                .and_then(|index| skin.data.textures.get(index))
                .and_then(|path| path.rsplit(['/', '\\']).next())
                .is_some_and(|name| name.to_ascii_lowercase().starts_with("r_"));
            if reflect {
                asset.diffuse_alpha_meshes.insert((skin_index, mesh_index));
            }
        }
    }
}

fn apply_preview_dye(asset: &mut ModelAsset, color: [u8; 3]) {
    let argb = 0xff00_0000
        | (u32::from(color[0]) << 16)
        | (u32::from(color[1]) << 8)
        | u32::from(color[2]);
    for (skin, data) in asset.skins.iter().enumerate() {
        for mesh in 0..data.data.meshes.len() {
            asset.mesh_tints.insert((skin, mesh), argb);
        }
    }
}

fn pan_offset(yaw: f32, radius: f32, zoom: f32, delta: egui::Vec2) -> Vec3 {
    let right = Vec3::new(yaw.cos(), 0., -yaw.sin());
    (right * delta.x + Vec3::Y * delta.y) * radius * zoom * 0.004
}

pub fn orbit_pitch(pitch: f32, vertical_drag: f32) -> f32 {
    (pitch + vertical_drag * 0.01).clamp(-1.45, 1.45)
}

fn animation_index(allowed: bool, action: usize) -> Option<usize> {
    allowed.then_some(action)
}

fn action_time(start: f32, end: f32, fps: i32, elapsed: f32) -> f32 {
    let fps = fps.max(1) as f32;
    let duration = (end - start) / fps;
    if duration <= 0.0 {
        start / fps
    } else {
        start / fps + elapsed % duration
    }
}

// SMD action identifiers are internal Chinese names, not translated item data.
// Keep unrecognized names identifiable without pretending to translate them.
pub fn action_label(name: &str, index: usize) -> String {
    let mut parts = Vec::new();
    let mut unknown = false;
    for word in name.split('_') {
        let translated = match word {
            "ok" => "Idle pose",
            "通用" => "General",
            "站立" => "Stand",
            "战斗站立" => "Combat stance",
            "行走" => "Walk",
            "奔跑" => "Run",
            "双手长" => "Two-handed long",
            "双手短" => "Two-handed short",
            "单手短" => "One-handed short",
            "空拳" => "Unarmed",
            "绑定" => "Bound",
            "骑乘" => "Mounted",
            "吟唱" => "Cast preparation",
            "吟唱循环" => "Casting loop",
            "施放起" => "Cast start",
            "施放落" => "Cast finish",
            "施放" => "Cast",
            "打坐" => "Meditate",
            "打坐循环" => "Meditation loop",
            "打坐站起" => "Stand up",
            "被击倒" => "Knockdown",
            "被击退" => "Knockback",
            "地面死亡" => "Death",
            "地面死亡循环" => "Death loop",
            "倒地循环" => "Prone loop",
            "防御" => "Defend",
            "防御循环" => "Defense loop",
            "复活" => "Revive",
            "起跳" => "Jump",
            "跳跃空中循环" => "Jump loop",
            "跳跃落地" => "Land",
            "回城" => "Return to town",
            "回城循环" => "Return to town loop",
            "捡东西" => "Pick up",
            "伸懒腰" => "Stretch",
            "四处张望" => "Look around",
            "普通攻击1" => "Attack 1",
            "普通攻击2" => "Attack 2",
            "普通攻击3" => "Attack 3",
            "普通攻击4" => "Attack 4",
            _ if word.is_ascii() => word,
            _ => {
                unknown = true;
                continue;
            }
        };
        if !translated.is_empty() {
            parts.push(translated);
        }
    }
    if unknown {
        parts.insert(0, "Untranslated action");
    }
    if parts.is_empty() {
        return format!("Action {}", index + 1);
    }
    if unknown {
        format!("{} ({})", parts.join(" · "), index + 1)
    } else {
        parts.join(" · ")
    }
}

pub struct Preview {
    color: wgpu::Texture,
    depth: wgpu::Texture,
    pub texture_id: egui::TextureId,
    asset: Option<ModelAsset>,
    draw: Option<Draw>,
    loaded_path: String,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub animation_error: Option<String>,
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub turntable: bool,
    pub flip_vertical: bool,
    pub playing: bool,
    center: Vec3,
    pub pan: Vec3,
    radius: f32,
    updated: std::time::Instant,
    elapsed: f32,
    pub action: usize,
    /// Combined scene action to start on once a model loads. The client asks a
    /// scene NPC for `站立` and falls back to `休闲`; leaving this empty keeps
    /// the first clip, which is what an item preview wants.
    pub idle_action: Option<String>,
}
impl Preview {
    pub fn new(
        device: &wgpu::Device,
        renderer: &mut egui_wgpu::Renderer,
        _format: wgpu::TextureFormat,
    ) -> Self {
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: 512,
                    height: 512,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: if format == PREVIEW_FORMAT {
                    &[wgpu::TextureFormat::Rgba8UnormSrgb]
                } else {
                    &[]
                },
            })
        };
        let color = texture(
            "Librarian preview",
            PREVIEW_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = texture(
            "Librarian preview depth",
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let texture_id = renderer.register_native_texture(
            device,
            // Model blending writes encoded colors to UNORM; egui expects an
            // sRGB-sampling view and performs its own display conversion.
            &color.create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            }),
            wgpu::FilterMode::Linear,
        );
        Self {
            color,
            depth,
            texture_id,
            asset: None,
            draw: None,
            loaded_path: String::new(),
            error: Some("Select an item with a supported model.".into()),
            warnings: Vec::new(),
            animation_error: None,
            // Player meshes face toward negative Z; start on their front.
            yaw: std::f32::consts::PI + 0.6,
            pitch: 0.2,
            zoom: 1.,
            turntable: false,
            flip_vertical: false,
            playing: true,
            center: Vec3::ZERO,
            pan: Vec3::ZERO,
            radius: 1.,
            updated: std::time::Instant::now(),
            elapsed: 0.,
            action: 0,
            idle_action: None,
        }
    }
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _format: wgpu::TextureFormat,
        assets: &mut LibrarianAssets,
        path: &str,
        dye: Option<[u8; 3]>,
        allow_animation: bool,
    ) {
        if self.loaded_path != path {
            self.loaded_path = path.to_owned();
            self.asset = None;
            self.draw = None;
            self.error = None;
            self.warnings.clear();
            self.animation_error = None;
            if path.is_empty() || path == "0" {
                self.error = Some("No model is referenced by this item.".into());
            } else {
                let path = if path.starts_with("worn:")
                    || path.starts_with("hair:")
                    || path.starts_with("models/")
                {
                    path.to_owned()
                } else {
                    format!("models/{path}")
                };
                let result = if let Some(hair) = path.strip_prefix("hair:") {
                    load_hair(assets, hair).map(|asset| (asset, Vec::new()))
                } else if let Some(worn) = path.strip_prefix("worn:") {
                    load_worn(assets, worn).map(|asset| (asset, Vec::new()))
                } else if path.ends_with(".ecm") {
                    ModelAsset::load_unique_with(&path, |p| assets.read_model_resource(p))
                } else if path.ends_with(".smd") {
                    assets
                        .read_model_resource(&path)
                        .and_then(|bytes| crate::ecmodel::parse_smd(&bytes))
                        .and_then(|smd| {
                            let relative = |p: &str| {
                                let p = crate::ecmodel::normalize(p);
                                if p.starts_with("models/") {
                                    p
                                } else {
                                    format!("{}/{}", path.rsplit_once('/').map_or("", |v| v.0), p)
                                }
                            };
                            let skeleton = relative(&smd.skeleton);
                            let skins: Vec<_> = smd.skins.iter().map(|p| relative(p)).collect();
                            ModelAsset::load_static_skins_with(&skeleton, &skins, |p| {
                                assets.read_model_resource(p)
                            })
                            .map(|asset| (asset, Vec::new()))
                        })
                } else {
                    Err("This item does not reference a supported ECM or SMD model.".into())
                };
                match result.and_then(|(mut asset, mut warnings)| {
                    apply_reflect_alpha(&mut asset);
                    let (mut poses, mut approximate) = preview_pose(&asset, None)?;
                    if poses.iter().flat_map(|mesh| &mesh.vertices).next().is_none()
                        && asset.descriptor.events.iter().any(|event| event.gfx_path().is_some())
                    {
                        asset = load_effect_model(assets, &asset)?;
                        apply_reflect_alpha(&mut asset);
                        (poses, approximate) = preview_pose(&asset, None)?;
                        warnings.push("Partial GFX preview: first embedded model only, in bind pose; effects, animation and GFX transforms are omitted.".into());
                    }
                    if approximate {
                        warnings.push("Approximate preview: skeleton reflection or constraints are not applied.".into());
                    }
                    let mut min = Vec3::splat(f32::INFINITY);
                    let mut max = Vec3::splat(f32::NEG_INFINITY);
                    for vertex in poses.iter().flat_map(|mesh| &mesh.vertices) {
                        let p = Vec3::from_array(vertex.position);
                        if p.is_finite() {
                            min = min.min(p);
                            max = max.max(p);
                        }
                    }
                    if !min.is_finite() || !max.is_finite() {
                        if asset.descriptor.events.iter().any(|event| event.gfx_path().is_some()) {
                            return Err("This effect-only model has no finite preview geometry.".into());
                        }
                        return Err("Model contains no finite geometry".into());
                    }
                    self.center = (min + max) * 0.5;
                    self.pan = Vec3::ZERO;
                    self.radius = ((max - min).length() * 0.5).max(0.1);
                    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                        label: Some("Librarian model sampler"),
                        address_mode_u: wgpu::AddressMode::Repeat,
                        address_mode_v: wgpu::AddressMode::Repeat,
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::FilterMode::Linear,
                        ..Default::default()
                    });
                    let draw = Draw::new(
                        device,
                        queue,
                        PREVIEW_FORMAT,
                        Some((
                            wgpu::TextureFormat::Depth32Float,
                            wgpu::CompareFunction::Greater,
                        )),
                        true,
                        &sampler,
                        &asset,
                        &poses,
                        self.frame(),
                        |p| assets.read_model_resource(p),
                    )?;
                    Ok((asset, draw, warnings))
                }) {
                    Ok((asset, draw, warnings)) => {
                        // A combined scene action names the base clip that
                        // should play; without one the first clip stays.
                        let idle = self.idle_action.as_deref().and_then(|requested| {
                            let combined = asset.descriptor.scene_action(requested)?;
                            let base = asset
                                .descriptor
                                .actions
                                .get(combined)?
                                .bases
                                .first()?
                                .name
                                .clone();
                            asset
                                .model
                                .actions
                                .iter()
                                .position(|action| action.name == base)
                        });
                        self.warnings = warnings;
                        self.asset = Some(asset);
                        self.draw = Some(draw);
                        self.elapsed = 0.;
                        self.action = idle.unwrap_or(0);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.updated).as_secs_f32().min(0.1);
        self.updated = now;
        if self.turntable {
            self.yaw += 0.6 * dt;
        }
        if self.playing && allow_animation {
            self.elapsed += dt;
        }
        if let Some(color) = dye {
            if let Some(asset) = &mut self.asset {
                apply_preview_dye(asset, color);
            }
        }
        if let (Some(asset), Some(draw)) = (&self.asset, &self.draw) {
            let poses = if let Some(clip) = animation_index(allow_animation, self.action)
                .and_then(|index| asset.model.actions.get(index))
            {
                let fps = clip
                    .fps
                    .filter(|fps| *fps > 0)
                    .unwrap_or(asset.skeleton.fps)
                    .max(1) as f32;
                let time = action_time(clip.start, clip.end, fps as i32, self.elapsed);
                match preview_pose(asset, Some((self.action, time))) {
                    Ok((poses, _)) => {
                        self.animation_error = None;
                        Ok(poses)
                    }
                    Err(error) => {
                        self.animation_error = Some(format!(
                            "Animation unavailable: {error}; showing bind pose."
                        ));
                        preview_pose(asset, None).map(|(poses, _)| poses)
                    }
                }
            } else {
                self.animation_error = None;
                preview_pose(asset, None).map(|(poses, _)| poses)
            };
            match poses {
                Ok(poses) => {
                    if let Err(error) = draw.update(queue, asset, &poses, self.frame()) {
                        self.animation_error = Some(format!("Preview update failed: {error}"));
                    }
                }
                Err(error) => self.animation_error = Some(format!("Preview pose failed: {error}")),
            }
        }
        let view = self.color.create_view(&Default::default());
        let depth = self.depth.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Librarian model preview"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.18,
                            g: 0.21,
                            b: 0.25,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(draw) = &self.draw {
                draw.draw(&mut pass);
            }
        }
        queue.submit(Some(encoder.finish()));
    }
    pub fn select_action(&mut self, action: usize) {
        if self.action != action {
            self.action = action;
            self.elapsed = 0.;
        }
    }
    pub fn action_names(&self) -> Vec<String> {
        self.asset
            .as_ref()
            .map(|asset| {
                asset
                    .model
                    .actions
                    .iter()
                    .map(|action| action.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn pan_by(&mut self, delta: egui::Vec2) {
        self.pan += pan_offset(self.yaw, self.radius, self.zoom, delta);
    }
    fn frame(&self) -> Frame {
        let matrix = camera_matrix(
            self.center + self.pan,
            self.radius,
            self.zoom,
            self.yaw,
            self.pitch,
        );
        Frame {
            blend_enabled: false,
            matrix: matrix.to_cols_array_2d(),
            lighting: Lighting {
                environment_ambient: [0.6, 0.6, 0.6, 1.],
                additional_ambient: [0.; 4],
                directional_diffuse: [0.7, 0.7, 0.7, 1.],
                point_ambient: [0.; 4],
                point_diffuse: [0.; 4],
                transparency: -1.,
                point_enabled: false,
            },
            direction: [0.3, -0.8, 0.5],
            point_position: [0.; 3],
            attenuation: [0., 0.4, 0.],
            eye_direction: [0., 0., 1.],
            directional_specular: [0.; 4],
            specular_variant: false,
            vertex_fog: 1.,
            fog_color: [0.; 4],
            fog_range: [0.; 4],
            clip_plane: [0.; 4],
            reverse_front: true,
            alpha_compare: 5,
            alpha_reference: 84. / 255.,
            // The librarian preview draws a single model into its own target:
            // there is no world atlas and no upscaler, so no LOD bias.
            lod_bias: 0.,
        }
    }
}

/// The main renderer deliberately rejects controllers it cannot reproduce.
/// A preview may still show the authored local transforms as an explicitly
/// approximate pose, without relaxing the production skeleton evaluator.
fn preview_pose(
    asset: &ModelAsset,
    action: Option<(usize, f32)>,
) -> Result<(Vec<crate::model_asset::PosedMesh>, bool), String> {
    let skeleton = &asset.skeleton;
    let approximate = skeleton.bones.iter().any(|bone| bone.flags != 0)
        || skeleton.joints.iter().any(|joint| joint.class != 15);
    let mut fallback;
    let skeleton = if approximate {
        fallback = skeleton.clone();
        for bone in &mut fallback.bones {
            bone.flags = 0;
        }
        for joint in &mut fallback.joints {
            joint.class = 15;
        }
        &fallback
    } else {
        skeleton
    };
    let tracks = match action {
        None => &[][..],
        Some((index, _)) => match asset
            .tracks
            .get(index)
            .ok_or("model action index out of range")?
        {
            Some(set) => &set.tracks,
            None if !skeleton.embedded_tracks.is_empty() => &skeleton.embedded_tracks,
            None => return Err("action has neither external nor embedded bone tracks".into()),
        },
    };
    let seconds = action.map_or(0., |(_, seconds)| seconds);
    let pose = crate::skeleton_pose::evaluate(skeleton, tracks, seconds, Mat4::IDENTITY)?;
    Ok((asset.meshes_from_pose(&pose)?, approximate))
}

fn camera_matrix(target: Vec3, radius: f32, zoom: f32, yaw: f32, pitch: f32) -> Mat4 {
    let distance = radius * 3.5 * zoom;
    let eye = target
        + Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ) * distance;
    let near = (distance * 0.01).max(0.001);
    let far = distance + radius * 10.;
    Mat4::from_scale(Vec3::new(-1., 1., 1.))
        * Mat4::perspective_rh(45f32.to_radians(), 1., far, near)
        * Mat4::look_at_rh(eye, target, Vec3::Y)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentic_client_models_when_available() {
        let Ok(root) = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT") else {
            return;
        };
        let mut assets = LibrarianAssets::open(std::path::Path::new(&root));
        let db = super::super::database::Database::parse_v27(&assets.read_elements_data().unwrap())
            .unwrap();
        let weapon = &db.items[*db.by_id.get(&6).expect("weapon 6")];
        let (asset, _) =
            ModelAsset::load_unique_with(&weapon.model, |path| assets.read_model_resource(path))
                .expect("load real weapon model");
        assert!(!preview_pose(&asset, None)
            .expect("weapon bind pose")
            .0
            .is_empty());

        let name =
            db.items
                .iter()
                .filter(|item| item.section == 6)
                .filter_map(|item| item.equipment_name())
                .find(|name| {
                    let path = crate::modules::designer::catalog::Class::Blademaster
                        .equipment_skin(crate::modules::designer::catalog::Gender::Male, name, 0);
                    assets.read_model_resource(&path).is_ok()
                })
                .expect("an armor skin for the male blademaster");
        let worn = load_worn(&mut assets, &format!("0:m:{name}")).expect("load worn armor");
        assert!(!preview_pose(&worn, None)
            .expect("worn bind pose")
            .0
            .is_empty());
        if let Some(action) = worn.model.actions.first() {
            let fps = action
                .fps
                .filter(|v| *v > 0)
                .unwrap_or(worn.skeleton.fps)
                .max(1) as f32;
            preview_pose(&worn, Some((0, action.start / fps))).expect("worn animation pose");
        }
    }

    #[test]
    fn authentic_reflective_weapons_when_available() {
        let Ok(root) = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT") else {
            return;
        };
        let mut assets = LibrarianAssets::open(std::path::Path::new(&root));
        let db = super::super::database::Database::parse_v27(&assets.read_elements_data().unwrap())
            .unwrap();
        for id in [89, 119] {
            let item = &db.items[*db.by_id.get(&id).expect("weapon ID")];
            let (mut asset, _) =
                ModelAsset::load_unique_with(&item.model, |path| assets.read_model_resource(path))
                    .expect("load weapon");
            apply_reflect_alpha(&mut asset);
            assert!(
                !asset.diffuse_alpha_meshes.is_empty(),
                "weapon {id} needs reflective texture alpha"
            );
        }
    }

    #[test]
    fn authentic_client_equipment_variants_when_available() {
        let Ok(root) = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT") else {
            return;
        };
        use crate::modules::designer::catalog::{Class, Gender};
        let mut assets = LibrarianAssets::open(std::path::Path::new(&root));
        let db = super::super::database::Database::parse_v27(&assets.read_elements_data().unwrap())
            .unwrap();
        for class in Class::ALL {
            for gender in Gender::ALL {
                if !class.allows(gender) {
                    continue;
                }
                let name = db
                    .items
                    .iter()
                    .filter(|item| item.section == 6)
                    .filter_map(|item| item.equipment_name())
                    .find(|name| assets.model_exists(&class.equipment_skin(gender, name, 0)))
                    .unwrap_or_else(|| {
                        panic!("no armor skin for {} {}", class.name(), gender.name())
                    });
                let request = format!(
                    "{}:{}:{name}",
                    class.index(),
                    if gender == Gender::Female { "f" } else { "m" }
                );
                let asset = load_worn(&mut assets, &request).unwrap_or_else(|error| {
                    panic!("{} {} {name}: {error}", class.name(), gender.name())
                });
                let poses = preview_pose(&asset, None).expect("equipment bind pose").0;
                assert_eq!(
                    asset.skins.len(),
                    1,
                    "only the equipment skin should be loaded"
                );
                assert!(poses
                    .iter()
                    .any(|pose| pose.skin == 0 && !pose.vertices.is_empty()));
            }
        }
    }

    #[test]
    fn reflect_alpha_and_dye_apply_only_to_expected_meshes() {
        let (mut asset, _) = crate::model_draw_fixture::fixture();
        asset.skins[0].data.textures[0] = "R_club.dds".into();
        apply_reflect_alpha(&mut asset);
        assert!(asset.diffuse_alpha_meshes.contains(&(0, 0)));
        asset.diffuse_alpha_meshes.clear();
        asset.skins[0].data.textures[0] = "rw_club.dds".into();
        apply_reflect_alpha(&mut asset);
        assert!(asset.diffuse_alpha_meshes.is_empty());
        apply_preview_dye(&mut asset, [10, 20, 30]);
        assert_eq!(asset.mesh_tints[&(0, 0)], 0xff0a141e);
    }

    #[test]
    fn drag_directions_follow_pointer_and_weapon_animation_is_disabled() {
        let delta = pan_offset(0., 1., 1., egui::vec2(10., 0.));
        let before = camera_matrix(Vec3::ZERO, 1., 1., 0., 0.) * Vec3::ZERO.extend(1.);
        let after = camera_matrix(delta, 1., 1., 0., 0.) * Vec3::ZERO.extend(1.);
        assert!(after.x / after.w > before.x / before.w);
        assert!(orbit_pitch(0., 10.) > 0.);
        assert_eq!(animation_index(false, 2), None);
        assert_eq!(animation_index(true, 2), Some(2));
    }

    #[test]
    fn static_action_does_not_jitter_and_labels_are_localized() {
        for elapsed in [0.0, 0.0005, 0.01, 2.3] {
            assert_eq!(action_time(0., 0., 30, elapsed), 0.);
            assert_eq!(action_time(30., 30., 30, elapsed), 1.);
        }
        assert_eq!(action_label("ok", 0), "Idle pose");
        assert_eq!(action_label("奔跑_通用", 1), "Run · General");
        assert!(!action_label("未知_通用", 2).contains("未知"));
    }

    #[test]
    fn effect_model_paths_accept_only_safe_embedded_smds() {
        let text = "GFXELEMENTID: 123\nModelPath: wrong.smd\nGFXELEMENTID: 160\nModelPath: 飞剑\\飞剑.SMD\nModelPath: ../bad.smd\nModelPath: C:/bad.smd\nGFXELEMENTID: 100\nModelPath: wrong.smd";
        assert_eq!(
            super::effect_model_paths(text),
            vec!["gfx/models/飞剑/飞剑.smd"]
        );
    }

    #[test]
    fn srgb_window_uses_encoded_unorm_preview_attachment() {
        if std::env::var_os("PW_LIBRARIAN_SYNTHETIC_GPU").is_none() {
            return;
        }
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Librarian sRGB preview regression"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .unwrap();
        let mut renderer =
            egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, None, 1);
        let preview = Preview::new(&device, &mut renderer, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(preview.color.format(), PREVIEW_FORMAT);
        assert!(
            preview.frame().reverse_front,
            "mirrored preview must keep the native front faces"
        );
        let (asset, poses) = crate::model_draw_fixture::fixture();
        let sampler = device.create_sampler(&Default::default());
        let texture = std::sync::Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        }));
        let draw = Draw::with_textures(
            &device,
            &queue,
            PREVIEW_FORMAT,
            Some((
                wgpu::TextureFormat::Depth32Float,
                wgpu::CompareFunction::Greater,
            )),
            true,
            &sampler,
            &asset,
            &poses,
            preview.frame(),
            &mut crate::model_draw_gpu::ReceiverCache::default(),
            |_| Ok((texture.clone(), false)),
        )
        .expect("sRGB window must not be used as the model attachment format");
        let color = preview.color.create_view(&Default::default());
        let depth = preview.depth.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            draw.draw(&mut pass);
        }
        queue.submit(Some(encoder.finish()));
        device.poll(wgpu::Maintain::Wait);
    }

    #[test]
    fn authentic_client_gpu_preview_when_available() {
        if std::env::var_os("PW_LIBRARIAN_GPU").is_none() {
            return;
        }
        let root = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT").expect("client root");
        let mut assets = LibrarianAssets::open(std::path::Path::new(&root));
        let db = super::super::database::Database::parse_v27(&assets.read_elements_data().unwrap())
            .unwrap();
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("headless GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Librarian client preview test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .expect("device");
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let mut renderer = egui_wgpu::Renderer::new(&device, format, None, 1);
        let mut preview = Preview::new(&device, &mut renderer, format);
        let weapon = &db.items[*db.by_id.get(&6).unwrap()];
        let club = &db.items[*db.by_id.get(&89).unwrap()].model;
        let hammer = &db.items[*db.by_id.get(&119).unwrap()].model;
        let armor = &db.items[*db.by_id.get(&128).unwrap()];
        let worn = format!("worn:0:m:{}", armor.equipment_name().unwrap());
        let fashion = &db.items[*db.by_id.get(&4276).unwrap()];
        let fashion = format!("worn:0:m:{}", fashion.equipment_name().unwrap());
        let hair_item = &db.items[*db.by_id.get(&26175).unwrap()];
        let (hair_id, texture_id) = hair_item.head_hair().expect("fashion hair reference");
        let hair = format!(
            "hair:0:m:{}:{}",
            db.hair_skins.get(&hair_id).unwrap(),
            db.face_textures.get(&texture_id).unwrap()
        );
        let flight = &db.items[*db.by_id.get(&2092).unwrap()].model;
        let flight_sword = &db.items[*db.by_id.get(&2093).unwrap()].model;
        let pet = &db.items[*db.by_id.get(&9069).unwrap()].model;
        let genie = &db.items[*db.by_id.get(&22181).unwrap()].model;
        for (label, path) in [
            ("weapon", &weapon.model),
            ("club", club),
            ("hammer", hammer),
            ("worn", &worn),
            ("fashion", &fashion),
            ("hair", &hair),
            ("flight", flight),
            ("flight_sword", flight_sword),
            ("pet", pet),
            ("genie", genie),
        ] {
            preview.update(
                &device,
                &queue,
                format,
                &mut assets,
                path,
                None,
                !matches!(label, "weapon" | "club" | "hammer"),
            );
            assert!(preview.error.is_none(), "{path}: {:?}", preview.error);
            if path == flight_sword {
                assert!(preview
                    .warnings
                    .iter()
                    .any(|w| w.contains("Partial GFX preview")));
            }
            if path == &worn {
                assert!(
                    !preview.action_names().is_empty(),
                    "equipment rig has no actions"
                );
                assert!(
                    preview.animation_error.is_none(),
                    "worn animation: {:?}",
                    preview.animation_error
                );
            }
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 512 * 512 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &preview.color,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(512 * 4),
                        rows_per_image: Some(512),
                    },
                },
                wgpu::Extent3d {
                    width: 512,
                    height: 512,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(encoder.finish()));
            let slice = buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().expect("map preview");
            let data = slice.get_mapped_range();
            if let Some(dir) = std::env::var_os("PW_LIBRARIAN_SCREENSHOTS") {
                image::save_buffer(
                    std::path::Path::new(&dir).join(format!("{label}.png")),
                    &data,
                    512,
                    512,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            let background = &data[..4];
            let changed = data
                .chunks_exact(4)
                .filter(|pixel| *pixel != background)
                .count();
            assert!(
                changed > 100,
                "{path} rendered only {changed} non-background pixels"
            );
            drop(data);
            buffer.unmap();
        }
    }

    #[test]
    fn preview_camera_mirrors_x_and_uses_reversed_depth() {
        let matrix = camera_matrix(Vec3::ZERO, 1., 1., 0., 0.);
        let projected = |p: Vec3| {
            let p = matrix * p.extend(1.);
            p.truncate() / p.w
        };
        assert!(projected(Vec3::X).x < 0.);
        assert!(projected(Vec3::new(0., 0., 2.)).z > projected(Vec3::new(0., 0., -2.)).z);
    }
}

fn load_hair(assets: &mut LibrarianAssets, request: &str) -> Result<ModelAsset, String> {
    use crate::modules::designer::catalog::{Class, Gender};
    let mut parts = request.splitn(4, ':');
    let class = parts
        .next()
        .and_then(|x| x.parse::<usize>().ok())
        .and_then(|x| Class::ALL.get(x))
        .copied()
        .ok_or("invalid hair class")?;
    let gender = match parts.next() {
        Some("f") => Gender::Female,
        Some("m") => Gender::Male,
        _ => return Err("invalid hair gender".into()),
    };
    if !class.allows(gender) {
        return Err("class and gender are incompatible".into());
    }
    let skin = parts
        .next()
        .filter(|x| !x.is_empty())
        .ok_or("missing hair skin")?;
    let texture = parts.next().unwrap_or("");
    let skeleton = class.face_asset(gender, 0, "bon");
    let mut asset = ModelAsset::load_static_skins_with(&skeleton, &[skin.to_owned()], |path| {
        assets.read_model_resource(path)
    })?;
    if !texture.is_empty() {
        asset.texture_overrides.insert((0, 0), texture.to_owned());
    }
    Ok(asset)
}

fn load_worn(assets: &mut LibrarianAssets, request: &str) -> Result<ModelAsset, String> {
    use crate::modules::designer::catalog::{Class, Gender};
    let (class, remainder) = request.split_once(':').ok_or("invalid equipment preview")?;
    let (gender, realname) = remainder
        .split_once(':')
        .ok_or("invalid equipment preview")?;
    let class = class
        .parse::<usize>()
        .ok()
        .and_then(|index| Class::ALL.get(index))
        .copied()
        .ok_or("invalid equipment class")?;
    let gender = if gender == "f" {
        Gender::Female
    } else {
        Gender::Male
    };
    if !class.allows(gender) {
        return Err("class and gender are incompatible".into());
    }
    let mut asset = ModelAsset::load_without_skins_with(&class.body_model(gender), |path| {
        assets.read_model_resource(path)
    })?;
    let path = class.equipment_skin(gender, realname, 0);
    let data = crate::skin::Skin::parse(&assets.read_model_resource(&path)?)
        .map_err(|error| format!("{path}: {error}"))?;
    asset
        .diffuse_alpha_meshes
        .extend((0..data.meshes.len()).map(|mesh| (0, mesh)));
    asset.model.skins.push(path.clone());
    asset
        .skins
        .push(crate::model_asset::SkinAsset { path, data });
    Ok(asset)
}
