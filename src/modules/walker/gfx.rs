//! Persistent character ECM EventType-100 effects.
//!
//! Only actionless records belong here: attack/drop events are owned by action
//! scheduling and must never play merely because a character is standing.
//! A root is first offered to `gfx_decal`, the verified static subset: those
//! roots need no per-frame state and keep rendering exactly as before. Roots it
//! rejects go to `gfx_file` + `gfx_player`, which execute the animated grammar —
//! keypoint clocks, controllers, particle emitters, ribbons, lightning and
//! nested children — against the same hook placement. A root neither reader
//! accepts is still a named warning, never an approximated effect.

use crate::{
    ecmodel,
    gfx_decal::{Quad, Root},
    gfx_file, gfx_player,
    model_asset::ModelAsset,
    model_attachment,
    modules::{
        designer::{
            assembly::{Character, CharacterPose, TravelState},
            assets::Assets,
        },
        walker::physics,
    },
    navigation::MapSpace,
};
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Body,
    Face,
    Weapon(usize),
    Flight,
    Mount,
}

struct HookBinding {
    hook: String,
    local: Mat4,
}

/// How a persistent root is executed. `Static` is the verified stateless decal
/// subset; `Dynamic` carries live clock, particle and strip state.
enum Kind {
    Static(Root),
    Dynamic(gfx_player::Player),
}

struct Effect {
    owner: Owner,
    path: String,
    kind: Kind,
    hook: Option<HookBinding>,
    scale: f32,
    alpha: f32,
    /// The event's `GfxSpeed`. A static root has no clock for it to act on, so
    /// only the dynamic kind reads it: the player is advanced by `dt * speed`,
    /// which is the same slot the client scales its millisecond delta by
    /// (`0x71a217` reads the root's live `PlaySpeed` copy at root+0x1ec).
    speed: f32,
}

pub struct CharacterGfx {
    effects: Vec<Effect>,
    pub warnings: Vec<String>,
}

fn field<'a>(event: &'a ecmodel::EcmEvent, key: &str) -> Option<&'a str> {
    event
        .fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn number(event: &ecmodel::EcmEvent, key: &str, default: f32) -> Result<f32, String> {
    let value = match field(event, key) {
        Some(value) => value
            .parse::<f32>()
            .map_err(|_| format!("invalid event {key}: {value}"))?,
        None => default,
    };
    if !value.is_finite() {
        return Err(format!("non-finite event {key}"));
    }
    Ok(value)
}

/// A stable per-path seed, so a character effect replays identically instead of
/// depending on frame order. The client seeds from the CRT `rand()` state.
fn seed_for(path: &str) -> u32 {
    path.bytes().fold(0x9E37_79B9u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u32)
    }) | 1
}

fn root_path(path: &str) -> String {
    let path = ecmodel::normalize(path);
    if path.starts_with("gfx/") {
        path
    } else {
        format!("gfx/{path}")
    }
}

impl CharacterGfx {
    pub fn load(assembly: &Character, assets: &mut Assets) -> Self {
        let mut result = Self {
            effects: Vec::new(),
            warnings: Vec::new(),
        };
        let mut models = vec![
            (Owner::Body, &assembly.body, "body"),
            (Owner::Face, &assembly.face, "face"),
        ];
        models.extend(
            assembly
                .weapons
                .iter()
                .enumerate()
                .map(|(index, weapon)| (Owner::Weapon(index), &weapon.asset, "weapon")),
        );
        if let Some(flight) = &assembly.flight {
            models.push((Owner::Flight, &flight.asset, "flight"));
        }
        if let Some(mount) = &assembly.mount {
            models.push((Owner::Mount, &mount.asset, "mount"));
        }
        for (owner, asset, label) in models {
            for (event_index, event) in asset.descriptor.events.iter().enumerate() {
                if event.action.is_some() || event.kind != 100 {
                    continue;
                }
                let Some(path) = event.gfx_path() else {
                    continue;
                };
                let path = root_path(&path);
                let origin = format!("{label} persistent event {event_index} ({path})");
                match Effect::load(owner, asset, event, &path, assets, &mut result.warnings) {
                    Ok(effect) => result.effects.push(effect),
                    Err(error) => result.warnings.push(format!("{origin}: {error}")),
                }
            }
        }
        result
    }

    pub fn effect_count(&self) -> usize {
        self.effects.len()
    }

    /// Roots executed by the runtime player rather than the static decal subset.
    /// The camera offset the roots' `ShakeCam` asks for, summed over the effects
    /// that are shaking right now. The client applies each root's offset to the
    /// camera in turn (`0x41a4e0`), which is additive, and puts the camera back
    /// once a root leaves its window (`0x6e5150`), so the sum is zero whenever no
    /// root is inside its shake window. The player owns no camera — see
    /// [`gfx_player::Player::camera_shake`] — so the host adds this to the eye it
    /// builds its view-projection from.
    pub fn camera_shake(&self) -> Vec3 {
        self.effects
            .iter()
            .filter_map(|effect| match &effect.kind {
                Kind::Dynamic(player) => Some(player.camera_shake()),
                Kind::Static(_) => None,
            })
            .fold(Vec3::ZERO, |sum, offset| sum + offset)
    }

    pub fn dynamic_count(&self) -> usize {
        self.effects
            .iter()
            .filter(|effect| matches!(effect.kind, Kind::Dynamic(_)))
            .count()
    }

    /// Resolve against the exact pose used by the character meshes. Hidden
    /// travel models contribute no effects, just as they contribute no meshes.
    pub fn quads(&self, assembly: &Character, pose: &CharacterPose) -> Result<Vec<Quad>, String> {
        let mut quads = Vec::new();
        for effect in &self.effects {
            let Kind::Static(root) = &effect.kind else {
                continue;
            };
            let Some(parent) = parent_of(effect, assembly, pose)? else {
                continue;
            };
            quads.extend(
                root.quads(parent, effect.scale, effect.alpha)
                    .map_err(|error| format!("{}: {error}", effect.path))?,
            );
        }
        Ok(quads)
    }

    /// Advance every dynamic root by `dt_ms` and return this frame's effect
    /// quads, resolved against the same hook placements `quads` uses. The
    /// camera basis is needed because the animated grammar billboards.
    pub fn advance(
        &mut self,
        assembly: &Character,
        pose: &CharacterPose,
        view: &gfx_player::View,
        dt_ms: f32,
        map: Option<&MapSpace>,
    ) -> Result<Vec<gfx_player::EffectQuad>, String> {
        let mut out = Vec::new();
        for effect in &mut self.effects {
            // Resolved first: `parent_of` reads the whole effect, which cannot
            // overlap the mutable borrow of its runtime below.
            let Some(parent) = parent_of(effect, assembly, pose)? else {
                continue;
            };
            let Kind::Dynamic(player) = &mut effect.kind else {
                continue;
            };
            // One ground query per effect per frame, at the placement the effect
            // is parented to, which is the owner position `0x6e53f0` raycasts
            // from. `None` when the host has no terrain, and then `MatchGrnd` and
            // `GroundHeight` stay inert exactly as they do for an owner whose
            // +0x1f4 byte is clear.
            let ground = map.map(|map| physics::effect_ground(map, parent.w_axis.truncate()));
            // The same grid answers the decal drape, which asks a height per
            // vertex rather than the one sample the frames are built from.
            let height = |x: f32, z: f32| map.map(|map| physics::ground(map, x, z).height);
            let terrain: Option<gfx_player::GroundHeightQuery<'_>> = Some(&height);
            player
                .advance_with_terrain(
                    dt_ms * effect.speed,
                    view,
                    parent,
                    effect.scale,
                    effect.alpha,
                    ground,
                    terrain,
                )
                .map_err(|error| format!("{}: {error}", effect.path))?;
            out.extend(player.quads().iter().cloned());
        }
        Ok(out)
    }
}

/// The world placement an effect is parented to, or `None` when its owner model
/// is hidden this frame (mounted weapons, grounded flight gear, and so on).
fn parent_of(
    effect: &Effect,
    assembly: &Character,
    pose: &CharacterPose,
) -> Result<Option<Mat4>, String> {
    {
        let (asset, owner_pose) = match effect.owner {
            Owner::Body => (&assembly.body, Some(&pose.body)),
            Owner::Face => (&assembly.face, Some(&pose.face)),
            Owner::Weapon(index) if pose.travel != TravelState::Mounted => {
                match (assembly.weapons.get(index), pose.weapons.get(index)) {
                    (Some(weapon), Some(weapon_pose)) => (&weapon.asset, Some(weapon_pose)),
                    _ => return Err(format!("{} lost its weapon pose", effect.path)),
                }
            }
            Owner::Weapon(_) => (&assembly.body, None),
            Owner::Flight => match (&assembly.flight, &pose.flight) {
                (Some(flight), Some(flight_pose)) => (&flight.asset, Some(flight_pose)),
                (Some(_), None) => (&assembly.body, None),
                _ => return Err(format!("{} lost its flight asset/pose", effect.path)),
            },
            Owner::Mount => match (&assembly.mount, &pose.mount) {
                (Some(mount), Some(mount_pose)) => (&mount.asset, Some(mount_pose)),
                (Some(_), None) => (&assembly.body, None),
                _ => return Err(format!("{} lost its mount asset/pose", effect.path)),
            },
        };
        let Some(owner_pose) = owner_pose else {
            return Ok(None);
        };
        let parent = if let Some(binding) = &effect.hook {
            let hook = asset
                .hook_matrix(owner_pose, &binding.hook)
                .map_err(|error| format!("{}: {error}", effect.path))?;
            model_attachment::world(hook, binding.local)?
        } else {
            owner_pose.model
        };
        Ok(Some(parent))
    }
}

impl Effect {
    fn load(
        owner: Owner,
        asset: &ModelAsset,
        event: &ecmodel::EcmEvent,
        path: &str,
        assets: &mut Assets,
        warnings: &mut Vec<String>,
    ) -> Result<Self, String> {
        for (key, expected) in [
            ("BindParent", "1"),
            ("Once", "0"),
            ("StartTime", "0"),
            ("TimeSpan", "-1"),
        ] {
            if field(event, key) != Some(expected) {
                return Err(format!("unsupported persistent event {key}"));
            }
        }
        for key in ["GfxOuterPath", "GfxRelToECM"] {
            if field(event, key).is_some_and(|value| value != "0") {
                return Err(format!("unsupported persistent event {key}"));
            }
        }
        let scale = number(event, "GfxScale", 1.)?;
        let alpha = number(event, "GfxAlpha", 1.)?;
        let speed = number(event, "GfxSpeed", 1.)?;
        if alpha < 0. || speed <= 0. {
            return Err("invalid persistent GFX alpha/speed".into());
        }
        if field(event, "UseModelAlpha") == Some("1")
            && asset.descriptor.render_settings().original_color >> 24 != 255
        {
            return Err("model-alpha persistent GFX needs a non-opaque model clock".into());
        }
        let hook = model_attachment::from_fields(&event.fields)?
            .map(|record| {
                let local = record.local()?;
                Ok::<_, String>(HookBinding {
                    hook: record.hook.clone(),
                    local,
                })
            })
            .transpose()?;
        let bytes = assets.read(path)?;
        let (text, bad) = gfx_file::decode(&bytes);
        if bad {
            return Err("invalid GBK GFX root".into());
        }
        // The verified static subset first: those roots are stateless and stay on
        // the path this viewer has always used. Whatever it rejects goes to the
        // full grammar, and only a root neither reader accepts is an error.
        let kind = match Root::parse(&text) {
            Ok(root) => Kind::Static(root),
            Err(static_error) => {
                let root = gfx_file::parse(&text)
                    .map_err(|error| format!("{error} (static subset: {static_error})"))?;
                let player = gfx_player::Player::new(root, seed_for(path), &mut |child| {
                    // A nested `GfxPath` is another member of the same archive.
                    let bytes = assets.read(child)?;
                    let (text, bad) = gfx_file::decode(&bytes);
                    if bad {
                        return Err(format!("{child}: invalid GBK"));
                    }
                    gfx_file::parse(&text)
                })
                .map_err(|error| format!("{error} (static subset: {static_error})"))?;
                // 82 of the corpus's 1,530 child references name files that do
                // not ship; those are named here rather than dropping the root.
                for warning in player.warnings() {
                    warnings.push(format!("{path}: {warning}"));
                }
                Kind::Dynamic(player)
            }
        };
        Ok(Self {
            owner,
            path: path.into(),
            kind,
            hook,
            scale,
            alpha,
            speed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::designer::catalog::{Class, Gender};
    use crate::modules::walker::character::Character as WalkerCharacter;

    #[test]
    fn authentic_static_weapon_event_loads_and_action_events_stay_excluded() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let wanted = "models/weapons/人物/拳套/短刃/噬魂刃/噬魂刃_右极品.ecm";
        let weapon = assets
            .catalog
            .weapons
            .iter()
            .find(|weapon| weapon.right_model == wanted)
            .unwrap_or_else(|| panic!("no catalog weapon uses {wanted}"));
        let id = weapon.id;
        let mut design = assets
            .default_design(Class::Blademaster, Gender::Male)
            .unwrap();
        design.equipment.weapon = id;
        let character = WalkerCharacter::load(&mut assets, design, Mat4::IDENTITY).unwrap();
        assert!(character.gfx.effect_count() > 0);
        let quads = character
            .gfx
            .quads(&character.assembly, &character.posed.pose)
            .unwrap();
        assert!(!quads.is_empty());
        assert!(quads
            .iter()
            .any(|quad| quad.texture == "gfx/textures/粒子/jianguang.dds"));
        let weapon = character
            .assembly
            .weapons
            .iter()
            .find(|weapon| !weapon.asset.descriptor.events.is_empty())
            .unwrap();
        assert!(weapon
            .asset
            .descriptor
            .events
            .iter()
            .any(|event| event.action.is_some()));
        let persistent = character
            .assembly
            .weapons
            .iter()
            .flat_map(|weapon| &weapon.asset.descriptor.events)
            .filter(|event| event.action.is_none() && event.kind == 100)
            .count();
        assert_eq!(character.gfx.effect_count(), persistent);
    }
}
