//! 1.4.2 skin queue classification, 0x7a5857–0x7a58c2. In this version
//! an alpha texture only triggers alpha-shader routing if it is ALSO a shader
//! texture. Related 1.5.2 source removed that condition; do not copy its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    Hidden,
    Opaque,
    Alpha,
}
pub fn classify(
    invisible: bool,
    transparency: f32,
    source_blend: u32,
    dest_blend: u32,
    shader_texture: bool,
    alpha_texture: bool,
) -> Queue {
    if invisible || transparency >= 1. {
        Queue::Hidden
    } else if transparency >= 0.
        || source_blend != 5
        || dest_blend != 6
        || (shader_texture && alpha_texture)
    {
        Queue::Alpha
    } else {
        Queue::Opaque
    }
}
/// Metadata used by the retail alpha-list insertion walk. This is deliberately
/// not a Rust sort comparator: the same-model AABB rule is not a total order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlphaNode {
    pub model: usize,
    pub mesh_weight: i32,
    pub skin_weight: i32,
    pub group: u32,
    pub group_weight: i32,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub distance: f32,
}
impl AlphaNode {
    fn valid(self) -> bool {
        self.distance.is_finite()
            && self.distance >= 0.
            && (0..3).all(|i| {
                self.min[i].is_finite() && self.max[i].is_finite() && self.min[i] <= self.max[i]
            })
    }
    fn weight(self) -> i32 {
        if self.mesh_weight == 0 {
            self.skin_weight
        } else {
            self.mesh_weight
        }
    }
}
/// Retail 0x7a5460: minimum distance to EIGHT transformed initial-AABB
/// corners, capped at 999999.875. Not distance to its surface, posed bounds,
/// or center: even an eye inside the box can have nonzero sort distance.
/// `world` is column-major and must be affine (as model placements are).
pub fn alpha_corner_distance(
    min: [f32; 3],
    max: [f32; 3],
    world: [[f32; 4]; 4],
    eye: [f32; 3],
) -> Result<f32, String> {
    let bounds = AlphaNode {
        model: 0,
        mesh_weight: 0,
        skin_weight: 0,
        group: 0,
        group_weight: 0,
        min,
        max,
        distance: 0.,
    };
    if !bounds.valid()
        || world
            .iter()
            .flatten()
            .chain(eye.iter())
            .any(|v| !v.is_finite())
        || [world[0][3], world[1][3], world[2][3], world[3][3]] != [0., 0., 0., 1.]
    {
        return Err("invalid alpha distance bounds/affine transform/eye".into());
    }
    let sentinel = f32::from_bits(0x497423fe);
    let mut nearest_sq = f32::INFINITY;
    for x in [min[0], max[0]] {
        for y in [min[1], max[1]] {
            for z in [min[2], max[2]] {
                let dx = world[0][0] * x + world[1][0] * y + world[2][0] * z + world[3][0] - eye[0];
                let dy = world[0][1] * x + world[1][1] * y + world[2][1] * z + world[3][1] - eye[1];
                let dz = world[0][2] * x + world[1][2] * y + world[2][2] * z + world[3][2] - eye[2];
                let dist_sq = dx * dx + dy * dy + dz * dz;
                if !dist_sq.is_finite() {
                    return Err("alpha distance overflow".into());
                }
                if dist_sq < nearest_sq {
                    nearest_sq = dist_sq;
                }
            }
        }
    }
    Ok(nearest_sq.sqrt().min(sentinel))
}
/// Source 0x7a5a1d–0x7a5aef. In 1.4.2, AABB comparisons ALSO run when
/// the new same-model weight is greater (not merely equal). Related 1.5.2
/// adds an equality guard; substituting that version changes render order.
pub fn alpha_insert_index(nodes: &[AlphaNode], new: AlphaNode) -> Result<usize, String> {
    if !new.valid() || nodes.iter().any(|n| !n.valid()) {
        return Err("invalid alpha queue bounds/distance".into());
    }
    Ok(nodes
        .iter()
        .position(|old| {
            if old.model == new.model {
                new.weight() < old.weight()
                    || old.max[1] > new.max[1]
                    || old.max[2] > new.max[2]
                    || old.min[2] < new.min[2]
            } else if old.group != 0 && old.group == new.group {
                new.group_weight < old.group_weight
            } else {
                old.distance <= new.distance
            }
        })
        .unwrap_or(nodes.len()))
}
/// D3D8 `D3DBLEND` codes. The 1.4.2 skin queue applies the ECM's authored
/// blend factors; codes that have no `wgpu` equivalent (11 `SRCALPHASAT`,
/// 12 `BOTHINVSRCALPHA`) stay explicit errors instead of being approximated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Factor {
    Zero,
    One,
    SrcColor,
    InvSrcColor,
    SrcAlpha,
    InvSrcAlpha,
    DestAlpha,
    InvDestAlpha,
    DestColor,
    InvDestColor,
}

pub fn blend_factor(code: u32) -> Result<Factor, String> {
    Ok(match code {
        1 => Factor::Zero,
        2 => Factor::One,
        3 => Factor::SrcColor,
        4 => Factor::InvSrcColor,
        5 => Factor::SrcAlpha,
        6 => Factor::InvSrcAlpha,
        7 => Factor::DestAlpha,
        8 => Factor::InvDestAlpha,
        9 => Factor::DestColor,
        10 => Factor::InvDestColor,
        _ => return Err(format!("unsupported D3D8 blend factor {code}")),
    })
}

/// Blend/depth state for one submitted mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshQueueState {
    pub queue: Queue,
    pub source: Factor,
    pub destination: Factor,
    pub depth_write: bool,
}

/// Depth write for the retail alpha list.
///
/// Recovered state bracket, not an assumption:
/// * `0x7844d0` — the per-node alpha pass, entered from `0x436984` inside the
///   alpha/overlay submit `0x436720` — sets SRCBLEND 5 (`0x77a2c0`), DESTBLEND
///   6 (`0x77a2f0`), `0x77a230(0)` = **ZWRITEENABLE off** and CULLMODE 1
///   (`0x77a540(1)`, D3DCULL_NONE) before it submits the list at `0x78452b`.
///   ZENABLE is *not* written, so the depth test stays on and only the write is
///   dropped. The same prologue disables fixed-function lighting
///   (`0x77a3c0` -> `0x77bd00(0x89, 0)`), which the shader receiving path does
///   not consult.
/// * Its tail `0x7845f0` restores, after the list is drawn, `0x77a230(1)`
///   (`0x784678`), CULLMODE 3 (`0x784684`) and the material state block
///   (`0x779fc0(0, 4)` at `0x78468e`).
/// * The per-mesh record path `0x7a76a0` only overrides SRC/DEST blend from the
///   record's `+0x2a4`/`+0x2a8` and toggles ALPHATESTENABLE, restoring 5/6 and
///   the previous alpha test; it never touches ZWRITEENABLE, so an authored
///   blend pair cannot re-enable depth writes for a blended mesh.
pub const ALPHA_DEPTH_WRITE: bool = false;

/// Complete queue state for one receiver mesh, or an explicit failure when the
/// authored blend factors cannot be reproduced.
pub fn mesh_state(
    invisible: bool,
    transparency: f32,
    source_blend: u32,
    destination_blend: u32,
    shader_texture: bool,
    alpha_texture: bool,
) -> Result<MeshQueueState, String> {
    let queue = classify(
        invisible,
        transparency,
        source_blend,
        destination_blend,
        shader_texture,
        alpha_texture,
    );
    Ok(MeshQueueState {
        queue,
        source: blend_factor(source_blend)?,
        destination: blend_factor(destination_blend)?,
        depth_write: match queue {
            Queue::Alpha => ALPHA_DEPTH_WRITE,
            Queue::Opaque | Queue::Hidden => true,
        },
    })
}

/// Per-mesh inputs of the 1.4.2 classify call `0x7a5830`, split by the object
/// they come from. The call reads three fields from the *model instance* and
/// two flags from the *mesh record*'s texture:
/// * model instance: `invisible` (`+0x9`), `transparency` (`+0xc`, compared
///   against 1.0 at `0x7a583d` and against 0.0 at `0x7a588c`) and the authored
///   blend pair (`+0x2a4`/`+0x2a8`, compared at `0x7a58a6`/`0x7a58b3`);
/// * mesh record: the texture vtable call `*0x1c(%eax)` at `0x7a5871` returns
///   a texture record whose `+0x36` (shader texture) and `+0x35` (alpha
///   texture) bytes set the local shader/alpha flag at `0x7a5878-0x7a5886`.
/// The SKI reader recovers only an alpha flag, so the shader flag is passed
/// through by the caller instead of being invented here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelState {
    pub invisible: bool,
    pub transparency: f32,
    pub source_blend: u32,
    pub destination_blend: u32,
}
impl ModelState {
    /// `transparency` must be finite. The client compares it with `fcomps`
    /// against 1.0 and 0.0; an unordered compare clears the flags it tests, so
    /// a NaN or -inf model would take the *alpha* branch there while Rust's
    /// `>=` comparisons would fall through to the blend-pair test. Rejecting
    /// the value keeps this reader from picking a queue the client never takes.
    pub fn new(
        invisible: bool,
        transparency: f32,
        source_blend: u32,
        destination_blend: u32,
    ) -> Result<Self, String> {
        if !transparency.is_finite() {
            return Err("non-finite model transparency".into());
        }
        Ok(Self {
            invisible,
            transparency,
            source_blend,
            destination_blend,
        })
    }
    /// Classify one mesh of this model from its two texture flags.
    pub fn mesh_state(
        self,
        shader_texture: bool,
        alpha_texture: bool,
    ) -> Result<MeshQueueState, String> {
        mesh_state(
            self.invisible,
            self.transparency,
            self.source_blend,
            self.destination_blend,
            shader_texture,
            alpha_texture,
        )
    }
}

/// One candidate mesh for the retail alpha insertion walk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlphaCandidate {
    pub slot: usize,
    pub mesh: usize,
    pub node: AlphaNode,
}

/// Source-ordered insertion walk of 0x7a5a1d. Retail inserts each culled mesh
/// into the existing list; replaying the same comparisons in the same order
/// reproduces the list because the AABB rule is not a total order.
pub fn alpha_order(candidates: &[AlphaCandidate]) -> Result<Vec<AlphaCandidate>, String> {
    let mut out: Vec<AlphaCandidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let new = candidate.node;
        if !new.valid() {
            return Err("invalid alpha queue bounds/distance".into());
        }
        let at = out
            .iter()
            .position(|existing| {
                let old = &existing.node;
                if old.model == new.model {
                    new.weight() < old.weight()
                        || old.max[1] > new.max[1]
                        || old.max[2] > new.max[2]
                        || old.min[2] < new.min[2]
                } else if old.group != 0 && old.group == new.group {
                    new.group_weight < old.group_weight
                } else {
                    old.distance <= new.distance
                }
            })
            .unwrap_or(out.len());
        out.insert(at, *candidate);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    const IDENTITY: [[f32; 4]; 4] = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    #[test]
    fn alpha_distance_uses_initial_corners_not_surface_or_center() {
        let d = alpha_corner_distance([-1.; 3], [1.; 3], IDENTITY, [0.; 3]).unwrap();
        assert!((d - 3f32.sqrt()).abs() < 1e-6);
        assert_eq!(
            alpha_corner_distance([-1.; 3], [1.; 3], IDENTITY, [1.; 3]).unwrap(),
            0.
        );
    }
    #[test]
    fn alpha_corner_distance_applies_rotation_scale_and_translation() {
        let world = [
            [2., 0., 0., 0.],
            [0., 3., 0., 0.],
            [0., 0., 4., 0.],
            [10., 20., 30., 1.],
        ];
        assert_eq!(
            alpha_corner_distance([-1.; 3], [1.; 3], world, [13., 23., 34.]).unwrap(),
            1.
        );
        let rotation = [
            [0., 1., 0., 0.],
            [-1., 0., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        assert_eq!(
            alpha_corner_distance([0.; 3], [1., 2., 0.], rotation, [-2., 1., 0.]).unwrap(),
            0.
        );
        assert_eq!(
            alpha_corner_distance([0.; 3], [0.; 3], IDENTITY, [2_000_000., 0., 0.]).unwrap(),
            999999.875
        );
    }
    #[test]
    fn alpha_corner_distance_rejects_nonaffine_nonfinite_and_overflow() {
        let mut bad = IDENTITY;
        bad[0][3] = 1.;
        assert!(alpha_corner_distance([0.; 3], [1.; 3], bad, [0.; 3]).is_err());
        assert!(alpha_corner_distance([0.; 3], [1.; 3], IDENTITY, [f32::NAN; 3]).is_err());
        assert!(alpha_corner_distance([0.; 3], [1.; 3], IDENTITY, [f32::MAX; 3]).is_err());
        assert!(alpha_corner_distance([2.; 3], [1.; 3], IDENTITY, [0.; 3]).is_err());
    }
    fn node(model: usize, distance: f32) -> AlphaNode {
        AlphaNode {
            model,
            mesh_weight: 0,
            skin_weight: 0,
            group: 0,
            group_weight: 0,
            min: [0.; 3],
            max: [1.; 3],
            distance,
        }
    }
    #[test]
    fn alpha_distance_is_far_first_with_new_equal_before_old() {
        let nodes = [node(1, 10.), node(2, 5.)];
        assert_eq!(alpha_insert_index(&nodes, node(3, 20.)).unwrap(), 0);
        assert_eq!(alpha_insert_index(&nodes, node(3, 10.)).unwrap(), 0);
        assert_eq!(alpha_insert_index(&nodes, node(3, 7.)).unwrap(), 1);
        assert_eq!(alpha_insert_index(&nodes, node(3, 1.)).unwrap(), 2);
    }
    #[test]
    fn same_model_142_aabb_rule_is_not_152_weight_sort() {
        let mut old = node(1, 10.);
        old.skin_weight = 4;
        let mut new = old;
        new.mesh_weight = 3;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
        new.mesh_weight = 5;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 1);
        new.max[1] = 0.5; // Greater weight STILL permits AABB insertion in 1.4.2.
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
        new = old;
        new.min[2] = 0.5;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
        new = old;
        new.max[2] = 0.5;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
    }
    #[test]
    fn group_weights_override_distance_but_zero_group_does_not() {
        let mut old = node(1, 1.);
        old.group = 7;
        old.group_weight = 4;
        let mut new = node(2, 100.);
        new.group = 7;
        new.group_weight = 4;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 1);
        new.group_weight = 3;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
        new.group = 0;
        assert_eq!(alpha_insert_index(&[old], new).unwrap(), 0);
    }
    #[test]
    fn alpha_insertion_rejects_invalid_input_before_mutation() {
        for bad in [f32::NAN, f32::INFINITY, -1.] {
            let mut n = node(1, 1.);
            n.distance = bad;
            assert!(alpha_insert_index(&[], n).is_err());
            assert!(alpha_insert_index(&[n], node(2, 1.)).is_err());
        }
        let mut n = node(1, 1.);
        n.min[1] = 2.;
        assert!(alpha_insert_index(&[], n).is_err());
    }
    #[test]
    fn ordinary_graded_alpha_dds_is_not_an_alpha_shader() {
        assert_eq!(classify(false, -1., 5, 6, false, true), Queue::Opaque);
        assert_eq!(classify(false, -1., 5, 6, true, false), Queue::Opaque);
        assert_eq!(classify(false, -1., 5, 6, true, true), Queue::Alpha);
    }
    #[test]
    fn zero_transparency_still_routes_alpha_and_hidden_wins() {
        assert_eq!(classify(false, 0., 5, 6, false, false), Queue::Alpha);
        assert_eq!(classify(false, 0.5, 5, 6, false, false), Queue::Alpha);
        assert_eq!(classify(false, 1., 1, 1, true, true), Queue::Hidden);
        assert_eq!(classify(true, -1., 5, 6, false, false), Queue::Hidden);
    }
    #[test]
    fn nondefault_blend_routes_alpha_but_does_not_change_texture_flags() {
        assert_eq!(classify(false, -1., 1, 6, false, false), Queue::Alpha);
        assert_eq!(classify(false, -1., 5, 1, false, false), Queue::Alpha);
    }

    #[test]
    fn authored_blend_codes_map_to_d3d8_factors() {
        for (code, factor) in [
            (1, Factor::Zero),
            (2, Factor::One),
            (3, Factor::SrcColor),
            (4, Factor::InvSrcColor),
            (5, Factor::SrcAlpha),
            (6, Factor::InvSrcAlpha),
            (7, Factor::DestAlpha),
            (8, Factor::InvDestAlpha),
            (9, Factor::DestColor),
            (10, Factor::InvDestColor),
        ] {
            assert_eq!(blend_factor(code).unwrap(), factor);
        }
        // No invented equivalent for the saturate/both-inverse modes.
        for code in [0, 11, 12, 13, u32::MAX] {
            assert!(blend_factor(code).is_err(), "{code}");
        }
    }

    #[test]
    fn alpha_list_depth_write_matches_the_native_pass_bracket() {
        // 0x7844d0 disables ZWRITEENABLE for the alpha list and 0x7845f0
        // restores it afterwards, so no blended mesh writes depth.
        assert!(!ALPHA_DEPTH_WRITE);
        for (transparency, source, destination) in
            [(0.5, 5, 6), (-1., 2, 6), (-1., 5, 2), (-1., 1, 1)]
        {
            let state = mesh_state(false, transparency, source, destination, false, false).unwrap();
            assert_eq!(state.queue, Queue::Alpha, "{source}/{destination}");
            assert!(!state.depth_write);
        }
        // The plain list (transparency < 0 with the default pair and no
        // shader/alpha texture) keeps depth writes.
        let opaque = mesh_state(false, -1., 5, 6, false, false).unwrap();
        assert_eq!(opaque.queue, Queue::Opaque);
        assert!(opaque.depth_write);
    }

    #[test]
    fn mesh_state_writes_depth_only_for_the_opaque_queue() {
        let opaque = mesh_state(false, -1., 5, 6, false, false).unwrap();
        assert_eq!(opaque.queue, Queue::Opaque);
        assert!(opaque.depth_write);
        assert_eq!(
            (opaque.source, opaque.destination),
            (Factor::SrcAlpha, Factor::InvSrcAlpha)
        );
        let alpha = mesh_state(false, -1., 2, 6, false, false).unwrap();
        assert_eq!(alpha.queue, Queue::Alpha);
        assert!(!alpha.depth_write);
        assert_eq!(
            (alpha.source, alpha.destination),
            (Factor::One, Factor::InvSrcAlpha)
        );
        let hidden = mesh_state(true, -1., 5, 6, false, false).unwrap();
        assert_eq!(hidden.queue, Queue::Hidden);
        // An unsupported authored factor fails the whole mesh, not just its queue.
        assert!(mesh_state(false, -1., 11, 6, false, false).is_err());
    }

    fn candidate(slot: usize, mesh: usize, distance: f32) -> AlphaCandidate {
        AlphaCandidate {
            slot,
            mesh,
            node: AlphaNode {
                model: slot,
                mesh_weight: 0,
                skin_weight: 0,
                group: 0,
                group_weight: 0,
                min: [0.; 3],
                max: [1.; 3],
                distance,
            },
        }
    }

    #[test]
    fn alpha_order_replays_the_insertion_walk_far_to_near() {
        let order = alpha_order(&[
            candidate(0, 0, 10.),
            candidate(1, 0, 30.),
            candidate(2, 1, 20.),
            candidate(3, 0, 10.),
        ])
        .unwrap();
        assert_eq!(
            order.iter().map(|c| (c.slot, c.mesh)).collect::<Vec<_>>(),
            [(1, 0), (2, 1), (3, 0), (0, 0)]
        );
        assert!(alpha_order(&[candidate(0, 0, f32::NAN)]).is_err());
    }

    #[test]
    fn alpha_order_keeps_group_and_same_model_rules() {
        let mut grouped = candidate(0, 0, 1.);
        grouped.node.group = 7;
        grouped.node.group_weight = 4;
        let mut later = candidate(1, 0, 100.);
        later.node.group = 7;
        later.node.group_weight = 4;
        let order = alpha_order(&[grouped, later]).unwrap();
        assert_eq!(
            order.iter().map(|c| c.slot).collect::<Vec<_>>(),
            [0, 1],
            "equal group weight keeps first insertion"
        );
        later.node.group_weight = 3;
        let order = alpha_order(&[grouped, later]).unwrap();
        assert_eq!(order.iter().map(|c| c.slot).collect::<Vec<_>>(), [1, 0]);
        let mut same_a = candidate(0, 0, 5.);
        same_a.node.skin_weight = 4;
        let mut same_b = candidate(0, 1, 50.);
        same_b.node.skin_weight = 9;
        let order = alpha_order(&[same_a, same_b]).unwrap();
        assert_eq!(
            order.iter().map(|c| c.mesh).collect::<Vec<_>>(),
            [0, 1],
            "equal same-model weights fall through to the distance comparison"
        );
        // 1.4.2 runs the AABB comparison even for a GREATER same-model weight.
        same_b.node.max[1] = 0.5;
        let order = alpha_order(&[same_a, same_b]).unwrap();
        assert_eq!(order.iter().map(|c| c.mesh).collect::<Vec<_>>(), [1, 0]);
    }

    #[test]
    fn model_level_fields_and_mesh_texture_flags_stay_separate() {
        let opaque = ModelState::new(false, -1., 5, 6).unwrap();
        assert_eq!(
            opaque.mesh_state(false, false).unwrap().queue,
            Queue::Opaque
        );
        // The texture flag pair only alpha-routes when BOTH bytes are set.
        assert_eq!(opaque.mesh_state(false, true).unwrap().queue, Queue::Opaque);
        assert_eq!(opaque.mesh_state(true, true).unwrap().queue, Queue::Alpha);
        // Model transparency 0 routes every mesh to alpha before the flags.
        let clear = ModelState::new(false, 0., 5, 6).unwrap();
        assert_eq!(clear.mesh_state(false, false).unwrap().queue, Queue::Alpha);
        // A fully transparent or invisible model hides every mesh.
        let gone = ModelState::new(false, 1., 5, 6).unwrap();
        assert_eq!(gone.mesh_state(true, true).unwrap().queue, Queue::Hidden);
        let hidden = ModelState::new(true, -1., 5, 6).unwrap();
        assert_eq!(
            hidden.mesh_state(false, false).unwrap().queue,
            Queue::Hidden
        );
        // The authored blend pair is a model field, so it moves all meshes.
        let authored = ModelState::new(false, -1., 2, 6).unwrap();
        assert_eq!(
            authored.mesh_state(false, false).unwrap().queue,
            Queue::Alpha
        );
    }
    #[test]
    fn nonfinite_model_transparency_is_rejected_not_silently_opaque() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(ModelState::new(false, bad, 5, 6).is_err(), "{bad}");
        }
        assert!(ModelState::new(false, 0., 5, 6).is_ok());
        // Unsupported authored blend codes still fail when state is requested.
        assert!(ModelState::new(false, -1., 11, 6)
            .unwrap()
            .mesh_state(false, false)
            .is_err());
    }
}
