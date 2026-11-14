//! Walker add-on: real player assets, source-backed ground mechanics, and an
//! optional first-person camera. All state, archive handles and GPU resources
//! belong to this module; disabling it restores the host's free-flight path.

pub(crate) mod archive;
mod audio;
mod character;
mod collision;
mod designer;
mod gallery;
mod gfx;
pub(crate) mod physics;
pub(crate) mod pose;

use self::{
    character::{character_transform, Character},
    designer::Designer,
    physics::{Body, FlightInput, Input, Mechanics},
};
use crate::{
    gfx_decal, gfx_decal_gpu, gfx_effect_gpu, gfx_player,
    model_asset::{ModelAsset, PosedMesh},
    model_draw_gpu,
    modules::designer::{
        assembly::{CharacterMeshes, TravelState},
        assets::Assets,
    },
    navigation::MapSpace,
    water::WaterArea,
};
use glam::{Mat4, Vec3};
use std::{collections::HashSet, path::PathBuf};
use winit::keyboard::KeyCode;

const THIRD_PERSON_DISTANCE: f32 = 4.5;

#[derive(Clone, Copy, Debug)]
struct Settings {
    mechanics: Mechanics,
    first_person: bool,
    /// First-person-only camera attached to the animated head bone.
    real_camera: bool,
    object_collision: bool,
    camera_distance: f32,
    camera_distance_target: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mechanics: Mechanics::default(),
            first_person: false,
            real_camera: false,
            object_collision: true,
            camera_distance: THIRD_PERSON_DISTANCE,
            camera_distance_target: THIRD_PERSON_DISTANCE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotkeyAction {
    Flight,
    Mount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Hotkey {
    key: KeyCode,
    control: bool,
}

impl Hotkey {
    fn is_down(self, keys: &HashSet<KeyCode>) -> bool {
        let control = keys.contains(&KeyCode::ControlLeft) || keys.contains(&KeyCode::ControlRight);
        control == self.control && keys.contains(&self.key)
    }

    fn label(self) -> String {
        format!("{}{:?}", if self.control { "Ctrl+" } else { "" }, self.key).replace("Key", "")
    }
}

fn captured_hotkey(keys: &HashSet<KeyCode>) -> Option<Hotkey> {
    let key = keys.iter().copied().find(|key| {
        !matches!(
            key,
            KeyCode::ControlLeft | KeyCode::ControlRight | KeyCode::ShiftLeft | KeyCode::ShiftRight
        )
    })?;
    Some(Hotkey {
        key,
        control: keys.contains(&KeyCode::ControlLeft) || keys.contains(&KeyCode::ControlRight),
    })
}

fn edge_triggered(down: bool, was_down: &mut bool) -> bool {
    let triggered = down && !*was_down;
    *was_down = down;
    triggered
}

fn jump_requested(space: bool, was_down: bool, travel: TravelState) -> bool {
    space && !was_down && travel != TravelState::Flying
}

fn zoomed_distance(current: f32, steps: f32) -> f32 {
    let change = if steps > 0.0 {
        1.0
    } else if steps < 0.0 {
        -1.0
    } else {
        0.0
    };
    (current - change).clamp(0.0, 20.0)
}

/// The Real Camera is a first-person-only option: third person always uses the
/// orbiting camera, and a rig that cannot supply an animated head keeps the
/// stable bind-head anchor rather than jumping to the world origin.
fn first_person_eye(real_camera: bool, animated: Option<Vec3>, stable: Vec3) -> Vec3 {
    if real_camera {
        animated.unwrap_or(stable)
    } else {
        stable
    }
}

fn eased_camera_distance(current: f32, target: f32, delta: f32) -> f32 {
    if !delta.is_finite() || delta <= 0.0 {
        return current;
    }
    let milliseconds = delta * 1000.0;
    let difference = target - current;
    if difference.abs() < delta {
        // fCamZoomSpeed is 1 m/s in the native default settings.
        target
    } else {
        current + difference * milliseconds / 200.0
    }
}

#[derive(Clone, Copy)]
enum ModelPart {
    Body,
    Face,
    Weapon { right: bool },
    Flight,
    Mount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartVisibility {
    All,
    /// First person draws the body except the native head skin.
    BodyWithoutHead,
    Hidden,
}

/// Which part geometry each camera mode submits. In first person only the
/// native head and face are hidden: wings/fly-swords and the right-hand sword
/// are attached to the body and must remain in front of the head camera.
fn part_visibility(
    part: &ModelPart,
    first_person: bool,
    soulsphere_hidden: bool,
) -> PartVisibility {
    if soulsphere_hidden && matches!(part, ModelPart::Weapon { right: true }) {
        return PartVisibility::Hidden;
    }
    if !first_person {
        return PartVisibility::All;
    }
    match part {
        ModelPart::Body => PartVisibility::BodyWithoutHead,
        // The head/face geometry would fill the near plane from inside.
        ModelPart::Face => PartVisibility::Hidden,
        ModelPart::Flight | ModelPart::Weapon { .. } | ModelPart::Mount => PartVisibility::All,
    }
}

struct RenderModel<'a> {
    part: ModelPart,
    asset: &'a ModelAsset,
    poses: &'a [PosedMesh],
}

struct Gpu {
    draws: Vec<model_draw_gpu::Draw>,
    parts: Vec<ModelPart>,
    gfx: gfx_decal_gpu::Draw,
    /// Quads for the pose that is already uploaded. Reflection/refraction
    /// passes reuse them instead of recomputing every hook matrix again.
    quads: Vec<gfx_decal::Quad>,
    pose_generation: u64,
    /// The animated roots: keypoint clocks, controllers, particles, ribbons,
    /// lightning and nested children, submitted through their own pass so the
    /// per-bucket blend factors stay exact.
    effects: gfx_effect_gpu::Draw,
    // Keep the sampler alive alongside bind groups that reference it.
    _sampler: wgpu::Sampler,
}

fn render_models<'a>(
    character: &'a Character,
    meshes: &'a CharacterMeshes,
) -> Result<Vec<RenderModel<'a>>, String> {
    let assembly = &character.assembly;
    let mut models = Vec::with_capacity(4 + assembly.weapons.len());
    models.push(RenderModel {
        part: ModelPart::Body,
        asset: &assembly.body,
        poses: &meshes.body,
    });
    models.push(RenderModel {
        part: ModelPart::Face,
        asset: &assembly.face,
        poses: &meshes.face,
    });
    models.extend(
        assembly
            .weapons
            .iter()
            .zip(&meshes.weapons)
            .filter(|(_, posed)| !posed.is_empty())
            .map(|(weapon, posed)| RenderModel {
                part: ModelPart::Weapon {
                    // HH_qiu is also a native right-hand hanger (Psychic
                    // soulspheres); only the explicit left hook is left-hand.
                    right: weapon.hook != "HH_lefthandweapon",
                },
                asset: &weapon.asset,
                poses: posed,
            }),
    );
    if !meshes.flight.is_empty() {
        let flight = assembly
            .flight
            .as_ref()
            .ok_or("flight meshes have no asset")?;
        models.push(RenderModel {
            part: ModelPart::Flight,
            asset: &flight.asset,
            poses: &meshes.flight,
        });
    }
    if !meshes.mount.is_empty() {
        let mount = assembly
            .mount
            .as_ref()
            .ok_or("mount meshes have no asset")?;
        models.push(RenderModel {
            part: ModelPart::Mount,
            asset: &mount.asset,
            poses: &meshes.mount,
        });
    }
    Ok(models)
}

pub struct Walker {
    element: Option<PathBuf>,
    map: Option<MapSpace>,
    water: Vec<WaterArea>,
    assets: Option<Assets>,
    designer: Designer,
    settings: Settings,
    body: Option<Body>,
    character: Option<Character>,
    facing: Vec3,
    camera_position: Option<Vec3>,
    /// Incremented once per simulation frame, so the second and later render
    /// passes of that frame can refresh only their per-pass constants.
    pose_generation: u64,
    travel: TravelState,
    flight_hotkey: Hotkey,
    mount_hotkey: Hotkey,
    capture: Option<HotkeyAction>,
    capture_wait_release: bool,
    flight_was_down: bool,
    mount_was_down: bool,
    jump_was_down: bool,
    gpu: Option<Gpu>,
    gpu_failed: bool,
    /// Wall clock of the previous `render`, because the animated GFX roots need
    /// a millisecond delta and the host's frame carries none.
    last_render: Option<std::time::Instant>,
    last_error: Option<String>,
    audio: audio::CharacterAudio,
    collision: collision::Collision,
    /// Free-standing roots from the effect catalogue, placed in the world and
    /// submitted through the same pass as the character's own effects.
    gallery: gallery::Gallery,
}

impl Default for Walker {
    fn default() -> Self {
        Self {
            element: None,
            map: None,
            water: Vec::new(),
            assets: None,
            designer: Designer::default(),
            settings: Settings::default(),
            body: None,
            character: None,
            facing: Vec3::NEG_Z,
            camera_position: None,
            pose_generation: 0,
            travel: TravelState::Ground,
            flight_hotkey: Hotkey {
                key: KeyCode::KeyF,
                control: true,
            },
            mount_hotkey: Hotkey {
                key: KeyCode::KeyH,
                control: true,
            },
            capture: None,
            capture_wait_release: false,
            flight_was_down: false,
            mount_was_down: false,
            jump_was_down: false,
            gpu: None,
            gpu_failed: false,
            last_render: None,
            last_error: None,
            audio: audio::CharacterAudio::default(),
            gallery: gallery::Gallery::default(),
            collision: collision::Collision::default(),
        }
    }
}

struct PopulationFloors<'a> {
    collision: &'a collision::Collision,
    water: &'a [WaterArea],
}

impl crate::modules::populator::FloorSource for PopulationFloors<'_> {
    fn lowest_floor(&self, feet: Vec3, extents: Vec3, max_up: f32) -> Option<f32> {
        crate::modules::populator::FloorSource::lowest_floor(self.collision, feet, extents, max_up)
    }

    fn floor_stamp(&self) -> u64 {
        crate::modules::populator::FloorSource::floor_stamp(self.collision)
    }

    fn sight_blocked(&self, from: Vec3, to: Vec3) -> bool {
        crate::modules::populator::FloorSource::sight_blocked(self.collision, from, to)
    }

    fn solid_at(&self, feet: Vec3, extents: Vec3) -> bool {
        crate::modules::populator::FloorSource::solid_at(self.collision, feet, extents)
    }

    fn water_height(&self, x: f32, z: f32) -> Option<f32> {
        physics::water_height(self.water, x, z)
    }
}

impl Walker {
    pub fn with_host(
        element: PathBuf,
        map: MapSpace,
        water: Vec<WaterArea>,
        scene: &crate::scene::Scene,
        ornaments: Vec<crate::scene::Ornament>,
    ) -> Self {
        let audio = audio::CharacterAudio::new(&element);
        let collision = collision::Collision::new(element.clone(), scene, &ornaments);
        Self {
            gallery: gallery::Gallery::new(element.clone()),
            element: Some(element),
            map: Some(map),
            water,
            audio,
            collision,
            ..Self::default()
        }
    }

    pub fn is_active(&self) -> bool {
        self.body.is_some() && self.character.is_some()
    }

    /// Starts and advances the building-hull loader on behalf of another
    /// add-on. The Populator stands authored placements on real BMD floors, so
    /// it needs the same hulls Walker collides with even when Walker itself is
    /// inactive; the load is only requested, never forced.
    pub fn prepare_object_collision(&mut self) {
        self.collision.ensure_loading();
        if self.collision.is_loading() {
            self.collision.poll();
        }
    }

    /// Building hull and client-water queries for placement and ambient routes.
    pub fn floors(&self) -> impl crate::modules::populator::FloorSource + '_ {
        PopulationFloors {
            collision: &self.collision,
            water: &self.water,
        }
    }

    /// CECCamera::Action changes distance by the configured native 1 m zoom
    /// step per wheel notch and clamps to 0..20 m. Positive wheel steps zoom in.
    pub fn zoom(&mut self, steps: f32) {
        if !self.is_active() || self.settings.first_person || !steps.is_finite() {
            return;
        }
        self.settings.camera_distance_target =
            zoomed_distance(self.settings.camera_distance_target, steps);
    }

    pub fn visible_in_water_pass(&self, plane: f32, reflection: bool) -> bool {
        let (Some(body), Some(character)) = (&self.body, &self.character) else {
            return false;
        };
        let design = &character.assembly.design;
        let constants = physics::MovementConstants::new(design.class, design.gender);
        let (min_y, max_y) = body.aabb_y(constants);
        let (reflect, refract) = physics::water_pass_visibility(min_y, max_y, plane);
        if reflection {
            reflect
        } else {
            refract
        }
    }

    pub fn status(&self) -> String {
        if let Some(error) = &self.last_error {
            return error.clone();
        }
        if let Some(character) = &self.character {
            format!(
                "Active — {} {} — {} persistent GFX root(s), {} animated",
                character.assembly.design.class.name(),
                character.assembly.design.gender.name(),
                character.gfx.effect_count(),
                character.gfx.dynamic_count()
            )
        } else {
            "Disabled".into()
        }
    }

    fn movement_sound(&self) -> Option<audio::MovementSound> {
        let body = self.body.as_ref()?;
        let character = self.character.as_ref()?;
        if self.travel == TravelState::Flying {
            return None;
        }
        if body.swimming && !body.water_jumping {
            let level = body
                .water_height
                .or_else(|| physics::water_height(&self.water, body.position.x, body.position.z))?;
            let camera_y = self.camera_position.unwrap_or(character.eye).y;
            return Some(audio::MovementSound::water(camera_y > level));
        }
        if !body.grounded || !body.moving {
            return None;
        }
        let mount_sound_type = if self.travel == TravelState::Mounted {
            Some(character.assembly.mount.as_ref()?.sound_type)
        } else {
            None
        };
        audio::MovementSound::land(
            character.assembly.design.class,
            character.assembly.design.gender,
            true,
            mount_sound_type,
        )
    }

    pub fn update_audio(&mut self, enabled: bool, volume: f32) {
        let active = self.is_active();
        // Always drain the animation clock: leaving edges queued would replay
        // them on the next enable. The sound selection itself is only worth
        // computing when the channel is actually on.
        let commands = self
            .character
            .as_mut()
            .map(|character| character.take_sound_commands())
            .unwrap_or_default();
        let movement = if active && enabled {
            self.movement_sound()
        } else {
            None
        };
        self.audio
            .update(enabled && active, volume, commands, movement);
    }

    pub fn audio_status(&self) -> &str {
        self.audio.status()
    }

    pub fn audio_diagnostic(&self) -> Option<String> {
        self.audio.diagnostic()
    }

    pub fn set_active(&mut self, active: bool, spawn: Vec3, facing: Vec3) {
        if !active {
            if let Some(character) = &self.character {
                self.designer
                    .restore_applied(character.assembly.design.clone());
            }
            self.body = None;
            self.character = None;
            self.camera_position = None;
            self.gpu = None;
            self.gpu_failed = false;
            self.last_error = None;
            self.jump_was_down = false;
            self.travel = TravelState::Ground;
            self.capture = None;
            self.flight_was_down = false;
            self.mount_was_down = false;
            self.audio.stop();
            return;
        }
        if self.is_active() {
            return;
        }
        let result = (|| -> Result<(), String> {
            let element = self
                .element
                .as_ref()
                .ok_or("Walker has no client asset host in this mode")?;
            let map = self.map.as_ref().ok_or("Walker has no terrain host")?;
            if self.assets.is_none() {
                self.assets = Some(Assets::open(element)?);
            }
            let ground = physics::ground(map, spawn.x, spawn.z);
            let position = Vec3::new(spawn.x, ground.height, spawn.z);
            let facing = Vec3::new(facing.x, 0., facing.z).normalize_or_zero();
            let facing = if facing == Vec3::ZERO {
                Vec3::NEG_Z
            } else {
                facing
            };
            let assets = self.assets.as_mut().unwrap();
            let design = self.designer.ensure_initialized(assets)?;
            let character = Character::load(assets, design, character_transform(position, facing))?;
            self.settings
                .mechanics
                .set_move_speed(character.assembly.class_speeds.run);
            self.body = Some(Body::new(position));
            self.character = Some(character);
            self.facing = facing;
            self.gpu = None;
            self.gpu_failed = false;
            self.last_error = None;
            Ok(())
        })();
        if result.is_ok() && self.settings.object_collision {
            self.collision.ensure_loading();
        }
        if let Err(error) = result {
            self.last_error = Some(format!("Walker could not spawn: {error}"));
            self.body = None;
            self.character = None;
            self.gpu = None;
        }
    }

    fn change_design(&mut self, design: crate::modules::designer::customize::CharacterDesign) {
        if !self.is_active() {
            return;
        }
        let result = (|| -> Result<Character, String> {
            let body = self.body.as_ref().ok_or("Walker body is not active")?;
            Character::load(
                self.assets.as_mut().ok_or("Walker assets are not open")?,
                design,
                character_transform(body.position, self.facing),
            )
        })();
        match result {
            Ok(character) => {
                self.designer.rebuild_succeeded(
                    character.assembly.equipment.len(),
                    character.assembly.warnings.len(),
                );
                if (self.travel == TravelState::Flying && character.assembly.flight.is_none())
                    || (self.travel == TravelState::Mounted && character.assembly.mount.is_none())
                {
                    self.travel = TravelState::Ground;
                }
                self.audio.stop();
                self.character = Some(character);
                self.gpu = None;
                self.gpu_failed = false;
                self.last_error = None;
            }
            Err(error) => {
                if let Some(character) = &self.character {
                    self.designer
                        .restore_applied(character.assembly.design.clone());
                }
                self.designer.rebuild_failed(&error);
                self.last_error = Some(format!("Could not customize Walker: {error}"));
            }
        }
    }

    fn activate_travel(&mut self, requested: TravelState) {
        let next = if self.travel == requested {
            TravelState::Ground
        } else {
            requested
        };
        match next {
            TravelState::Ground => {}
            TravelState::Mounted => {
                if self
                    .character
                    .as_ref()
                    .and_then(|character| character.assembly.mount.as_ref())
                    .is_none()
                {
                    self.last_error = Some("Select a Ride pet before mounting.".into());
                    return;
                }
            }
            TravelState::Flying => {
                if self
                    .character
                    .as_ref()
                    .and_then(|character| character.assembly.flight.as_ref())
                    .is_none()
                {
                    self.last_error = Some("Select a wing or fly-sword before flying.".into());
                    return;
                }
                if let Some(body) = &mut self.body {
                    body.begin_flight();
                }
            }
        }
        self.travel = next;
        self.gpu = None;
        self.gpu_failed = false;
        self.last_error = None;
    }

    /// Returns the add-on camera position only while Walker owns movement.
    /// `None` is the narrow host signal to retain ordinary free flight.
    pub fn update(
        &mut self,
        delta: f32,
        input_enabled: bool,
        keys: &HashSet<KeyCode>,
        yaw: f32,
        pitch: f32,
    ) -> Option<Vec3> {
        if !self.is_active() {
            return None;
        }
        self.settings.camera_distance = eased_camera_distance(
            self.settings.camera_distance,
            self.settings.camera_distance_target,
            delta,
        );
        if self.collision.is_loading() {
            self.collision.poll();
        }
        let mut captured = false;
        if let Some(action) = self.capture {
            if self.capture_wait_release {
                if captured_hotkey(keys).is_none() {
                    self.capture_wait_release = false;
                }
            } else if let Some(hotkey) = captured_hotkey(keys) {
                match action {
                    HotkeyAction::Flight => self.flight_hotkey = hotkey,
                    HotkeyAction::Mount => self.mount_hotkey = hotkey,
                }
                self.capture = None;
                captured = true;
            }
        }
        let flight_down = self.flight_hotkey.is_down(keys);
        let mount_down = self.mount_hotkey.is_down(keys);
        if captured {
            self.flight_was_down = flight_down;
            self.mount_was_down = mount_down;
        } else if input_enabled {
            if edge_triggered(flight_down, &mut self.flight_was_down) {
                self.activate_travel(TravelState::Flying);
            }
            if edge_triggered(mount_down, &mut self.mount_was_down) {
                self.activate_travel(TravelState::Mounted);
            }
        } else {
            self.flight_was_down = flight_down;
            self.mount_was_down = mount_down;
        }
        let (s, c) = yaw.sin_cos();
        let forward = Vec3::new(s, 0., -c);
        let right = Vec3::new(-c, 0., -s);
        let mut direction = Vec3::ZERO;
        if input_enabled {
            if keys.contains(&KeyCode::KeyW) {
                direction += forward;
            }
            if keys.contains(&KeyCode::KeyS) {
                direction -= forward;
            }
            if keys.contains(&KeyCode::KeyD) {
                direction += right;
            }
            if keys.contains(&KeyCode::KeyA) {
                direction -= right;
            }
        }
        direction = direction.normalize_or_zero();
        if direction != Vec3::ZERO {
            self.facing = direction;
        }
        let space = input_enabled && keys.contains(&KeyCode::Space);
        let jump = jump_requested(space, self.jump_was_down, self.travel);
        self.jump_was_down = space;
        let rushing = input_enabled
            && self.travel == TravelState::Flying
            && (keys.contains(&KeyCode::ShiftLeft) || keys.contains(&KeyCode::ShiftRight))
            && self
                .character
                .as_ref()
                .and_then(|character| character.assembly.flight.as_ref())
                .and_then(|flight| flight.rush_speed)
                .is_some();
        let vertical = if input_enabled && self.travel == TravelState::Flying {
            (if space { 1.0 } else { 0.0 })
                - if keys.contains(&KeyCode::KeyC) {
                    1.0
                } else {
                    0.0
                }
        } else {
            0.0
        };
        let flight_speed = self.character.as_ref().and_then(|character| {
            character.assembly.flight.as_ref().map(|flight| {
                let range = if rushing {
                    flight.rush_speed.unwrap_or(flight.normal_speed)
                } else {
                    flight.normal_speed
                };
                character.assembly.class_speeds.fly + range.0
            })
        });
        let mount_speed = self
            .character
            .as_ref()
            .and_then(|character| character.assembly.mount.as_ref().map(|mount| mount.speed));
        let (movement_constants, swim_speed) = self.character.as_ref().map(|character| {
            let design = &character.assembly.design;
            (
                physics::MovementConstants::new(design.class, design.gender),
                character.assembly.class_speeds.swim,
            )
        })?;
        let water_vertical = if input_enabled && self.travel == TravelState::Ground {
            let ascend = keys.contains(&KeyCode::ArrowUp);
            let descend = keys.contains(&KeyCode::ArrowDown) || keys.contains(&KeyCode::KeyZ);
            let deep_space = space
                && self.body.as_ref().is_some_and(|body| {
                    let level =
                        physics::water_height(&self.water, body.position.x, body.position.z);
                    level.is_some()
                        && !physics::can_take_off_water(body.position, level, movement_constants)
                });
            (if ascend || deep_space { 1.0 } else { 0.0 }) - if descend { 1.0 } else { 0.0 }
        } else {
            0.0
        };

        let map = self.map.as_ref()?;
        let extents = movement_constants.collision_extents();
        // The fixed-step integrator must see object hull tops while it decides
        // whether the body is grounded. Patching `grounded` after the step (the
        // old `ground_on_object` path) made every frame on a roof transiently
        // airborne: the terrain-only step projected movement onto the terrain
        // normal, produced a bogus vertical velocity and restarted the landing
        // animation, so a character jumping onto an object jittered in mid-air.
        let objects: Option<&dyn physics::ObjectSupport> = if self.settings.object_collision {
            Some(&self.collision)
        } else {
            None
        };
        let floor = physics::Floor {
            map,
            extents,
            objects,
        };
        let body = self.body.as_mut()?;
        let previous_position = body.position;
        if input_enabled {
            match self.travel {
                TravelState::Ground => body.advance_with_water(
                    delta,
                    &self.settings.mechanics,
                    Input { direction, jump },
                    water_vertical,
                    swim_speed,
                    floor,
                    &self.water,
                    movement_constants,
                ),
                TravelState::Mounted => {
                    let mut mechanics = self.settings.mechanics;
                    mechanics.move_speed = mount_speed?;
                    body.advance_mounted(delta, &mechanics, Input { direction, jump }, floor);
                }
                TravelState::Flying => body.advance_flight(
                    delta,
                    FlightInput {
                        direction,
                        vertical,
                        speed: flight_speed?,
                    },
                    map,
                    &self.water,
                    movement_constants,
                ),
            }
        }
        if self.settings.object_collision {
            // Swept AABB blocking and wall sliding remain a separate layer.
            // Support/landing is owned by the fixed step above, so this only
            // stops horizontal penetration and ceilings.
            let resolved = self
                .collision
                .resolve(previous_position, body.position, extents);
            body.position = resolved.feet;
            if resolved.blocked {
                if resolved.normal.y >= 0.5 && body.vertical_velocity < 0.0 {
                    body.vertical_velocity = 0.0;
                } else if resolved.normal.y < -0.5 && body.vertical_velocity > 0.0 {
                    body.vertical_velocity = 0.0;
                }
            }
        }
        // Wall sliding can move x/z onto a higher terrain triangle. Object
        // collision is an additional layer, never a terrain toggle.
        body.apply_terrain_floor(map);
        if let Some(character) = &mut self.character {
            if let Err(error) =
                character.update(delta, body, self.facing, self.travel, rushing, vertical)
            {
                self.last_error = Some(format!("Walker animation: {error}"));
            }
            self.pose_generation = self.pose_generation.wrapping_add(1);
            let eye = character.eye;
            if self.settings.first_person {
                // Real Camera samples the current animated head bone, so the
                // view bobs and sways with the locomotion/flying/riding clips.
                // The ordinary first-person mode keeps the stable bind anchor.
                let animated = if self.settings.real_camera {
                    character.real_camera_eye()
                } else {
                    None
                };
                let eye = first_person_eye(self.settings.real_camera, animated, eye);
                self.camera_position = Some(eye);
                return Some(eye);
            }
            let cp = pitch.cos();
            let look = Vec3::new(s * cp, pitch.sin(), -c * cp);
            let mut camera = eye - look * self.settings.camera_distance;
            let clearance = physics::ground(map, camera.x, camera.z).height + 0.25;
            camera.y = camera.y.max(clearance);
            self.camera_position = Some(camera);
            return Some(camera);
        }
        None
    }

    /// The camera offset the loaded roots' `ShakeCam` asks for, for the host to
    /// add to the eye it builds the frame's view-projection from. The client owns
    /// its camera and applies the offset itself (`0x41a4e0`), restoring it once a
    /// root leaves its shake window (`0x6e5150`); the walker owns none, so it
    /// reports what the roots asked for and leaves the applying to whoever holds
    /// the camera. `Vec3::ZERO` while nothing shakes.
    pub fn camera_shake(&self) -> Vec3 {
        self.character
            .as_ref()
            .map_or(Vec3::ZERO, |character| character.gfx.camera_shake())
    }

    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let rebuild = if let Some(assets) = self.assets.as_mut() {
            self.designer.ui(ui, assets)
        } else {
            None
        };
        if let Some(design) = rebuild {
            self.change_design(design);
        }

        ui.separator();
        ui.heading("Effect gallery");
        {
            // Copied out first: the gallery only needs the eye and the look
            // direction, and taking them keeps its borrow of `self` disjoint.
            let at = self.camera_position.unwrap_or(Vec3::ZERO);
            let facing = self.facing;
            self.gallery.ui(ui, at, facing);
        }

        ui.separator();
        ui.heading("Mounts and flight");
        ui.horizontal(|ui| {
            let flight = if self.travel == TravelState::Flying {
                "Land"
            } else {
                "Fly"
            };
            if ui.button(flight).clicked() {
                self.activate_travel(TravelState::Flying);
            }
            let mount = if self.travel == TravelState::Mounted {
                "Dismount"
            } else {
                "Mount"
            };
            if ui.button(mount).clicked() {
                self.activate_travel(TravelState::Mounted);
            }
            if ui.button("Ground").clicked() {
                self.activate_travel(self.travel);
            }
        });
        ui.horizontal(|ui| {
            let flight_label = if self.capture == Some(HotkeyAction::Flight) {
                "Flight: press a key…".to_owned()
            } else {
                format!("Flight: {}", self.flight_hotkey.label())
            };
            if ui.button(flight_label).clicked() {
                self.capture = Some(HotkeyAction::Flight);
                self.capture_wait_release = true;
            }
            let mount_label = if self.capture == Some(HotkeyAction::Mount) {
                "Mount: press a key…".to_owned()
            } else {
                format!("Mount: {}", self.mount_hotkey.label())
            };
            if ui.button(mount_label).clicked() {
                self.capture = Some(HotkeyAction::Mount);
                self.capture_wait_release = true;
            }
        });
        if let Some(character) = &self.character {
            if let Some(flight) = &character.assembly.flight {
                let base = character.assembly.class_speeds.fly;
                let normal = flight.normal_speed;
                let rush = flight.rush_speed;
                ui.label(
                    egui::RichText::new(format!(
                        "{} • base {base:.1} m/s • normal bonus +{:.1}–{:.1}{}",
                        flight.mode.name(),
                        normal.0,
                        normal.1,
                        rush.map_or_else(
                            || " • no rush-flight range".to_owned(),
                            |range| format!(" • rush bonus +{:.1}–{:.1}", range.0, range.1)
                        )
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text("Flight movement uses the selected range's lower bound. Hold Shift for rush flight when the authentic record provides it.");
            }
            if let Some(mount) = &character.assembly.mount {
                ui.label(
                    egui::RichText::new(format!(
                        "Ride level 1: {:.1} m/s • native per-level coefficient {:.3}",
                        mount.speed, mount.speed_per_level
                    ))
                    .small()
                    .weak(),
                );
            }
        }
        ui.label(
            egui::RichText::new(
                "Fly: WASD, Space up, C down; hold Shift for rush. Mount: WASD + Space jump. Hotkeys trigger once per press.",
            )
            .small()
            .weak(),
        );

        ui.separator();
        ui.heading("Movement and camera");
        let mut speed = self.settings.mechanics.move_speed;
        if ui
            .add(egui::Slider::new(&mut speed, 0.5..=20.0).suffix(" m/s"))
            .changed()
        {
            self.settings.mechanics.set_move_speed(speed);
        }
        let native_run = self
            .character
            .as_ref()
            .map_or(physics::NATIVE_RUN_SPEED, |character| {
                character.assembly.class_speeds.run
            });
        ui.label(format!("Ground move speed (class native {native_run:.1} m/s)"))
            .on_hover_text("CECConfigs' profession run speed. Server-side equipment and buff modifiers are not present in this offline explorer.");

        let mut height = self.settings.mechanics.jump_height;
        if ui
            .add(egui::Slider::new(&mut height, 0.25..=12.0).suffix(" m"))
            .changed()
        {
            self.settings.mechanics.set_jump_height(height);
        }
        ui.label("Jump height at the fixed 60 Hz physics step")
            .on_hover_text(format!(
                "Recovered ordinary jump: 10.0 m/s impulse and 9.8 m/s² gravity. The native default is {:.3} m at 60 Hz; the continuous v²/(2g) reference is {:.3} m. A custom height is converted to an equivalent launch impulse.",
                physics::native_jump_height(),
                physics::CONTINUOUS_JUMP_HEIGHT,
            ));
        ui.checkbox(&mut self.settings.first_person, "First Person Mode")
            .on_hover_text("Attaches the camera to the character's head. Only the native head and face are hidden; wings, fly-swords, weapons and the mount stay visible.");
        ui.add_enabled(
            self.settings.first_person,
            egui::Checkbox::new(&mut self.settings.real_camera, "Real Camera"),
        )
        .on_hover_text("First Person only: follow the animated head bone instead of the stable bind-head anchor, so the view gets real head bob, sway, nod and travel movement. Mouse look still controls orientation.");
        if ui
            .checkbox(&mut self.settings.object_collision, "Collision check")
            .on_hover_text("Collide with native BMD building/object hulls and TBH tree hulls. Terrain collision remains enabled permanently, even when this option is off.")
            .changed()
            && self.settings.object_collision
        {
            self.collision.ensure_loading();
        }
        ui.label(egui::RichText::new(self.collision.status()).small().weak());
        if let Some(diagnostic) = self.collision.diagnostic() {
            ui.label(egui::RichText::new(diagnostic).small().weak());
        }
        if ui.button("Restore native movement defaults").clicked() {
            self.settings.mechanics = Mechanics::default();
            self.settings.mechanics.set_move_speed(native_run);
        }
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Ground: WASD + Space jump/double jump • Water: WASD + Arrow Up/Down or Z, Space takeoff/ascend • Travel controls above • Mouse look + wheel zoom",
            )
            .small(),
        );
        if let Some(body) = &self.body {
            ui.label(
                egui::RichText::new(format!(
                    "{}  •  jumps {}/{}  •  xyz {:.1}, {:.1}, {:.1}",
                    match self.travel {
                        TravelState::Flying => "Flying",
                        TravelState::Mounted => "Riding",
                        TravelState::Ground if body.swimming && !body.water_jumping => "Swimming",
                        TravelState::Ground if body.grounded => "Grounded",
                        TravelState::Ground => "Airborne",
                    },
                    body.jump_count,
                    if self.travel == TravelState::Mounted {
                        physics::MOUNTED_MAX_JUMPS
                    } else {
                        physics::MAX_JUMPS
                    },
                    body.position.x,
                    body.position.y,
                    body.position.z,
                ))
                .small()
                .weak(),
            );
        }
        let status = self.status();
        if self.last_error.is_some() {
            ui.colored_label(egui::Color32::LIGHT_RED, status);
        } else {
            ui.label(egui::RichText::new(status).small().weak());
        }
        if let Some(character) = &self.character {
            for warning in character.warnings.iter().take(3) {
                ui.label(egui::RichText::new(warning).small().weak());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: model_draw_gpu::Frame,
    ) {
        if !self.is_active() || self.gpu_failed {
            return;
        }
        if self.gpu.is_none() {
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Walker character sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let result = (|| -> Result<Gpu, String> {
                let character = self.character.as_ref().ok_or("Walker character missing")?;
                let assets = self.assets.as_mut().ok_or("Walker assets missing")?;
                let models = render_models(character, &character.posed.meshes)?;
                let mut draws = Vec::with_capacity(models.len());
                let mut parts = Vec::with_capacity(models.len());
                for model in models {
                    parts.push(model.part);
                    draws.push(model_draw_gpu::Draw::new(
                        device,
                        queue,
                        format,
                        Some((
                            wgpu::TextureFormat::Depth32Float,
                            wgpu::CompareFunction::Greater,
                        )),
                        true,
                        &sampler,
                        model.asset,
                        model.poses,
                        frame,
                        |path| assets.read(path),
                    )?);
                }
                let decal_quads = character
                    .gfx
                    .quads(&character.assembly, &character.posed.pose)?;
                let gfx = gfx_decal_gpu::Draw::new(
                    device,
                    queue,
                    format,
                    Some((
                        wgpu::TextureFormat::Depth32Float,
                        wgpu::CompareFunction::Greater,
                    )),
                    &decal_quads,
                    |path| assets.read(path),
                )?;
                // The effects pass renders into the same reverse-Z depth the
                // models and the static decals use, so occlusion stays consistent.
                let effects = gfx_effect_gpu::Draw::new(
                    device,
                    queue,
                    format,
                    Some((
                        wgpu::TextureFormat::Depth32Float,
                        wgpu::CompareFunction::Greater,
                    )),
                )?;
                Ok(Gpu {
                    draws,
                    parts,
                    gfx,
                    quads: decal_quads.clone(),
                    pose_generation: self.pose_generation,
                    effects,
                    _sampler: sampler,
                })
            })();
            match result {
                Ok(gpu) => self.gpu = Some(gpu),
                Err(error) => {
                    self.last_error = Some(format!("Walker rendering: {error}"));
                    self.gpu_failed = true;
                    return;
                }
            }
        }
        // The host draws the character once per pass (main, reflection,
        // refraction). Posed vertices are shared by every pass; only the
        // view/projection and clip constants change, so uploading the whole
        // skeleton again for each pass is pure main-thread/PCIe churn.
        let pose_uploaded = self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.pose_generation == self.pose_generation);
        let update = (|| -> Result<Option<Vec<gfx_decal::Quad>>, String> {
            let gpu = self.gpu.as_ref().ok_or("Walker GPU draws missing")?;
            let character = self.character.as_ref().ok_or("Walker character missing")?;
            let models = render_models(character, &character.posed.meshes)?;
            if models.len() != gpu.draws.len() {
                return Err("travel draw count changed without rebuilding GPU resources".into());
            }
            let mut quads = None;
            if pose_uploaded {
                for (index, model) in models.iter().enumerate() {
                    gpu.draws[index].update_constants(queue, model.asset, frame)?;
                }
            } else {
                for (index, model) in models.into_iter().enumerate() {
                    gpu.draws[index].update_posed(queue, model.asset, model.poses, frame)?;
                }
                quads = Some(
                    character
                        .gfx
                        .quads(&character.assembly, &character.posed.pose)?,
                );
            }
            let decal_quads = quads.as_deref().unwrap_or(gpu.quads.as_slice());
            gpu.gfx
                .update(queue, frame.matrix, frame.clip_plane, decal_quads)?;
            Ok(quads)
        })();
        match update {
            Ok(Some(quads)) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.quads = quads;
                    gpu.pose_generation = self.pose_generation;
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.last_error = Some(format!("Walker rendering: {error}"));
                self.gpu_failed = true;
                self.gpu = None;
                return;
            }
        }
        // The animated roots advance once per frame, keyed to the pose
        // generation, because the host calls `render` once per pass (main,
        // reflection, refraction). Advancing per call would run every effect at
        // triple speed and let an emitter discharge three times the particles.
        if !pose_uploaded {
            let effects = (|| -> Result<Vec<gfx_player::EffectQuad>, String> {
                let character = self.character.as_mut().ok_or("Walker character missing")?;
                let assets = self.assets.as_mut().ok_or("Walker assets missing")?;
                // Terrain for the effects' own ground query: `MatchGrnd` and
                // `GroundHeight` are gated on an owner byte no `.gfx` file can
                // set, so the host supplies the sample and those keys stay inert
                // only while no map is loaded.
                let map = self.map.as_ref();
                // `render` carries no delta, so the wall clock supplies one,
                // clamped: a stall (shader compile, window drag, a breakpoint)
                // must not fast-forward an effect or let an emitter discharge a
                // thousand particles in a single frame.
                let now = std::time::Instant::now();
                let dt_ms = match self.last_render {
                    Some(last) => (now.duration_since(last).as_secs_f32() * 1000.).clamp(0., 100.),
                    None => 0.,
                };
                self.last_render = Some(now);
                // The billboards need a camera basis, and `Frame` only carries
                // the view-projection, so the eye is recovered from its inverse.
                let view = gfx_player::View::from_inverse_view_projection(
                    Mat4::from_cols_array_2d(&frame.matrix).inverse(),
                );
                let mut quads = character.gfx.advance(
                    &character.assembly,
                    &character.posed.pose,
                    &view,
                    dt_ms,
                    map,
                )?;
                // The gallery shares this pass: the same draw object, the same
                // blend buckets and the same texture resolution, so a
                // free-standing root renders exactly like an attached one.
                quads.extend(self.gallery.advance(&view, dt_ms, map)?);
                if let Some(gpu) = self.gpu.as_mut() {
                    // Element 160 is a skin model like any other, so it takes this
                    // frame's `LIGHTINFO` — the same ambient and directional colour
                    // the character's own meshes are drawn with.
                    let light = gfx_effect_gpu::EffectLight {
                        lighting: frame.lighting,
                        direction: frame.direction,
                        fog_range: frame.fog_range,
                        fog_color: frame.fog_color,
                        directional_specular: frame.directional_specular,
                        eye_direction: frame.eye_direction,
                    };
                    gpu.effects.update(
                        device,
                        queue,
                        &quads,
                        frame.matrix,
                        frame.clip_plane,
                        [view.position.x, view.position.y, view.position.z, 0.],
                        Some(light),
                        &mut |path| assets.read(path),
                    )?;
                }
                Ok(quads)
            })();
            match effects {
                Ok(_) => {}
                Err(error) => {
                    self.last_error = Some(format!("Walker effects: {error}"));
                }
            }
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("add-on Walker character pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if let Some(gpu) = &self.gpu {
            let soulsphere_hidden = self
                .character
                .as_ref()
                .is_some_and(|character| character.soulsphere_hidden());
            for (draw, part) in gpu.draws.iter().zip(&gpu.parts) {
                match part_visibility(part, self.settings.first_person, soulsphere_hidden) {
                    PartVisibility::Hidden => {}
                    // Skin 1 is the separate native head installed by
                    // CECPlayer. Keep every pose uploaded; only submission
                    // changes, so the full-mesh validation remains intact.
                    PartVisibility::BodyWithoutHead => {
                        draw.draw_filtered(&mut pass, |(skin, _)| skin != 1);
                    }
                    PartVisibility::All => draw.draw(&mut pass),
                }
            }
            gpu.gfx.draw_quads(&mut pass);
            gpu.effects.draw(&mut pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_require_their_modifier_and_trigger_only_on_press_edges() {
        let hotkey = Hotkey {
            key: KeyCode::KeyF,
            control: true,
        };
        let mut keys = HashSet::from([KeyCode::KeyF]);
        assert!(!hotkey.is_down(&keys));
        keys.insert(KeyCode::ControlLeft);
        assert!(hotkey.is_down(&keys));

        let mut was_down = false;
        assert!(edge_triggered(true, &mut was_down));
        assert!(!edge_triggered(true, &mut was_down));
        assert!(!edge_triggered(false, &mut was_down));
        assert!(edge_triggered(true, &mut was_down));
    }

    #[test]
    fn capture_ignores_modifier_only_keys_and_retains_control_state() {
        assert_eq!(
            captured_hotkey(&HashSet::from([KeyCode::ControlLeft])),
            None
        );
        let captured =
            captured_hotkey(&HashSet::from([KeyCode::ControlRight, KeyCode::KeyH])).unwrap();
        assert_eq!(captured.key, KeyCode::KeyH);
        assert!(captured.control);
    }

    #[test]
    fn wheel_zoom_uses_native_steps_and_clamps_without_touching_other_modes() {
        assert_eq!(zoomed_distance(4.5, 1.0), 3.5);
        assert_eq!(zoomed_distance(4.5, -1.0), 5.5);
        assert_eq!(zoomed_distance(4.5, 3.0), 3.5);
        assert_eq!(zoomed_distance(4.5, -3.0), 5.5);
        assert_eq!(zoomed_distance(4.5, 0.0), 4.5);
        assert_eq!(zoomed_distance(0.5, 1.0), 0.0);
        assert_eq!(zoomed_distance(19.5, -1.0), 20.0);
        let eased = eased_camera_distance(4.5, 3.5, 1.0 / 60.0);
        assert!((eased - 53.0 / 12.0).abs() < 0.000_01);
        assert_eq!(eased_camera_distance(4.5, 3.5, 2.0), 3.5);
        assert_eq!(eased_camera_distance(4.5, 3.5, 0.0), 4.5);
    }

    #[test]
    fn jump_press_edge_is_shared_by_ground_and_riding_but_not_flight() {
        assert!(jump_requested(true, false, TravelState::Ground));
        assert!(jump_requested(true, false, TravelState::Mounted));
        assert!(!jump_requested(true, false, TravelState::Flying));
        assert!(!jump_requested(true, true, TravelState::Ground));
        assert!(!jump_requested(true, true, TravelState::Mounted));
        assert!(!jump_requested(false, false, TravelState::Mounted));
    }

    #[test]
    fn first_person_keeps_wings_swords_and_mount_visible() {
        assert_eq!(
            part_visibility(&ModelPart::Flight, true, false),
            PartVisibility::All,
            "flying wings/fly-sword must be drawn in first person"
        );
        assert_eq!(
            part_visibility(&ModelPart::Weapon { right: true }, true, false),
            PartVisibility::All,
            "the right-hand sword must be drawn in first person"
        );
        assert_eq!(
            part_visibility(&ModelPart::Weapon { right: false }, true, false),
            PartVisibility::All
        );
        assert_eq!(
            part_visibility(&ModelPart::Mount, true, false),
            PartVisibility::All
        );
        assert_eq!(
            part_visibility(&ModelPart::Face, true, false),
            PartVisibility::Hidden
        );
        assert_eq!(
            part_visibility(&ModelPart::Body, true, false),
            PartVisibility::BodyWithoutHead
        );
        // The authored ShowMagicWeapon rule still wins in either camera mode.
        assert_eq!(
            part_visibility(&ModelPart::Weapon { right: true }, true, true),
            PartVisibility::Hidden
        );
        assert_eq!(
            part_visibility(&ModelPart::Flight, false, false),
            PartVisibility::All
        );
        assert_eq!(
            part_visibility(&ModelPart::Body, false, false),
            PartVisibility::All
        );
    }

    #[test]
    fn real_camera_only_replaces_the_first_person_eye() {
        let stable = Vec3::new(0., 1.6, 0.);
        let animated = Vec3::new(0.08, 1.63, -0.02);
        assert_eq!(first_person_eye(false, Some(animated), stable), stable);
        assert_eq!(first_person_eye(true, Some(animated), stable), animated);
        assert_eq!(first_person_eye(true, None, stable), stable);
    }
}
