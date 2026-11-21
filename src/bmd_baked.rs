//! Baked BMD draw states for the inspected 1.4.2 client.
//!
//! World BMDs are *baked*: the client lights them into two per-vertex colours
//! (mesh members `+0x284`/`+0x288`) and blends those two arrays at `0x7b9bc0`
//! with the world day/night weight. They never enter the ordinary receiving
//! path (`0x7a714f..0x7a7158`, `0x7926d2..0x79275a`), which would add regional
//! ambient/diffuse/specular a second time.
//!
//! The submitter is `0x7a9730` for the ordinary world draw (`0x7a9120` is the
//! refraction variant, which differs only in the stage-0 alpha argument) and
//! its per-mesh loop is `0x7a9808..0x7a99bc`. Every row of [`DrawState`] is a
//! write in that loop:
//!
//! | state | write site | value for baked meshes |
//! |---|---|---|
//! | `LIGHTING` (`0x77a3c0`, id 0x89) | `0x7a9759` | 0, so material colours never contribute |
//! | `SPECULARENABLE` (`0x77b570`, id 0x1d) | `0x7a9762` | 0 |
//! | stage 0 `COLOROP` (`0x779fc0`) | `0x7a976d` | 5 `MODULATE2X`, args `TEXTURE`, `DIFFUSE` (`0x7a977a`) |
//! | stage 0 `ALPHAOP` (`0x77a020`) | `0x7a9785` | 4 `MODULATE`, args `TEXTURE`, `DIFFUSE` (`0x7a9792`) |
//! | stages 1..7 | `0x7a979e`/`0x7a97aa` | disabled (`COLOROP`/`ALPHAOP` = 1) |
//! | `CULLMODE` (`0x77a540`, id 0x16) | `0x7a98cf`, restored `0x7a9a26`/`0x7a9a49` | 1 `NONE` when `mesh+0x26c` is set, else the pass default (2 `CCW`) |
//! | `ALPHATEST` (`0x77a320`, id 0xf) | `0x7a992b` / `0x7a98f8` | 0, unless `mesh+0x2cc` differs from `0xff` |
//!
//! `mesh+0x2cc` is initialised to `0xff` by the mesh constructor (`0x7b9408`,
//! `movl $0xff,0x2cc(%esi)`) and no BMD reader writes it, so in 1.4.2 the alpha
//! test stays disabled for every file-authored mesh; the enabled branch
//! (`0x7a98f8..0x7a9924`) computes `80 * alpha / 255 + 1` as `ALPHAREF`.
//! Blending is *not* part of the mesh loop: no `ALPHABLENDENABLE` write exists
//! between `0x7a9808` and the draws it gates. Every pass that calls the
//! submitter turns it on immediately beforehand (`0x42b5a5`, `0x42bc17`,
//! `0x42c32e`, `0x42c650`), and none of them writes `SRCBLEND`/`DESTBLEND`, so
//! the pair left by the previous restore (`SRCALPHA`/`INVSRCALPHA`, e.g.
//! `0x410089`/`0x41023a`) applies. The blend is load bearing, because the
//! stage-0 alpha op is not a select: D3D8's `D3DTEXTUREOP` enum is
//! `DISABLE = 1, SELECTARG1 = 2, SELECTARG2 = 3, MODULATE = 4, MODULATE2X = 5`,
//! so `ALPHAOP(4)` at `0x7a9785` is `MODULATE` of `TEXTURE` and `DIFFUSE`
//! (`0x7a9792`). The surface alpha is therefore `texture.a * diffuse.a`, i.e.
//! the bound texture's own alpha for the per-mesh opacity the loop stamps into
//! the diffuse word (`mesh+0x2cc << 24`, `0x7b9d5e..0x7b9d74`, `0xff` unless a
//! mesh declares opacity). That is how the client draws leaf, canopy and banner
//! cards: no alpha test, and the texture alpha drives `SRCALPHA` coverage.
//! (An earlier revision of this file read opcode 4 as `SELECTARG2`, concluded
//! that the alpha was forced to 1.0, and rendered those cards opaque; the
//! opcode table above is the correction.)
//! One of the four passes is the water-quality variant: `0x42c5f3` compares the
//! quality byte `+0x95` against 2 before enabling blending, and that byte is
//! the `WaterEffect` key this pass object parses at `0x40d16c` and saves at
//! `0x40d765`. The gate is a graphics setting, never a material or mesh field.
//!
//! Depth: the world pass leaves `ZFUNC` at the D3D8 default (`LESSEQUAL`) and
//! never writes `ZBIAS` (`0x77a250` is called only from the effect path at
//! `0x417453`, value 2, restored at `0x417471`). Under the viewer's reversed
//! projection that stays `GREATER` for the terrain and becomes `GREATER_EQUAL`
//! for baked scenery, so a coincident duplicate submitted later wins exactly
//! as it does in the client's single depth buffer.
//!
//! Viewer deviation — alpha-test cutout instead of a depth-writing blend.
//! The recovered client state above is accurate reverse engineering, but the
//! viewer does *not* reproduce `ALPHABLENDENABLE(1)` with `ZWRITEENABLE` left
//! on. In the client an alpha-0 texel still occupies the depth buffer; its
//! scenery passes draw in one ordered depth buffer and its water is composited
//! differently. In the viewer the water pass is depth-tested against that same
//! buffer (`water_gpu.rs` reads `scene_depth` for the refraction guard), so a
//! transparent baked card that writes depth shows up as a solid square over
//! the water (lily pads) and hides scenery behind its transparent gaps
//! (stacked tree canopies). Blending that pass (PR #6, `f9adb72`) exposed
//! exactly those two regressions. The shared world shader therefore treats
//! textured baked meshes as a cutout: it discards fragments below the alpha
//! threshold — the same `((i.kind>1.5 && i.kind<2.5) || i.kind>3.5)` band used
//! for grass/trees — so transparent texels write neither colour nor depth, and
//! the survivors are written opaque with depth on. `scenery_pipeline` stays
//! unblended; `Blend`/`AlphaTest` below document the client, while `kind` is
//! the only field the production renderer consumes.
//!
//! One band is exempt: the `.sdr.` water/lava curtains (the downward-scroll
//! scripts `瀑布`, `瀑布细长`, `青云山小瀑布`, `岩浆`, kind 2.02/2.12) are
//! the client's translucent sheets and read as opaque white through a cutout —
//! the reported "waterfalls are a white plain texture with no transparency".
//! They draw in a second pass (`fs_sdr` + `sdr_blend_pipeline` in `gpu.rs`)
//! with the client's `SRCALPHA`/`INVSRCALPHA` and surface alpha
//! `texture.a * diffuse.a`, depth *write* off so the lily-pad/canopy failures
//! above cannot return; the depth test still occludes the sheet behind nearer
//! geometry.

/// Mesh versions the reader and this draw state were inspected against.
pub const INSPECTED_MESH_VERSIONS: [u32; 9] = [
    0x1000_0004,
    0x1000_0005,
    0x1000_0006,
    0x1000_0300,
    0x1000_0301,
    0x1000_0302,
    0x1000_0303,
    0x1000_0304,
    0x1000_0305,
];

/// `mesh+0x2cc` value the mesh constructor installs, i.e. no alpha test.
pub const CLIENT_MAX_MESH_ALPHA: u32 = 0xff;

/// Numerator of the client's alpha-test reference arithmetic (`0x7a990a`).
pub const CLIENT_ALPHA_REF_NUMERATOR: u32 = 80;

/// Texture stage combination for one baked mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// No texture: `MODULATE2X` against an unbound stage returns the doubled
    /// vertex colour, i.e. the baked day/night blend times two.
    ColorOnly,
    /// `COLOROP(5)` = `MODULATE2X` at `0x7a976d`: `texture.rgb * colour * 2`,
    /// with `ALPHAOP(4)` = `MODULATE` at `0x7a9785`, so the fragment alpha is
    /// `texture.a * diffuse.a` and crops the surface through the pass's
    /// `SRCALPHA`/`INVSRCALPHA` blend.
    Modulate2x,
}

/// Alpha test applied to the sampled texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaTest {
    /// `ALPHATEST(0)` at `0x7a992b`: every fragment survives.
    Disabled,
}

/// Back-face handling for one baked mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cull {
    /// Single-sided: the client keeps the pass cull mode (`D3DCULL_CCW`, the
    /// device default cached at `+0x168`) and the production shader drops the
    /// reversed winding.
    BackFace,
    /// `mesh+0x26c` set: `CULLMODE(1)` at `0x7a98cf`, restored afterwards.
    TwoSided,
}

/// Recovered client blend state of the baked mesh draw, i.e. what the client's
/// fragment alpha does. Documentation of the 1.4.2 passes only: the viewer
/// deliberately renders textured baked meshes as an alpha-test cutout instead
/// (see the module note), so these variants describe the client, not the
/// production pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// Textured baked mesh. Every pass enables `ALPHABLENDENABLE(1)` right
    /// before it calls the submitter (`0x42b5a5`, `0x42bc17`, `0x42c32e`,
    /// `0x42c650`) with `ALPHATEST(0)` (`0x42b595`, `0x42bc07`, `0x42c31e`,
    /// `0x42c660`), and stage 0 is `MODULATE` over `TEXTURE`/`DIFFUSE`
    /// (`0x7a9785`/`0x7a9792`). The fragment alpha is the sampled texture's
    /// alpha, so the pass blend (`SRCALPHA`/`INVSRCALPHA`, the pair the shared
    /// restores leave in place) cuts leaf and canopy cards instead of drawing
    /// their transparent texels.
    TextureAlphaSrcOver,
    /// Untextured baked mesh: the disabled/unbound stage returns white, so
    /// `MODULATE(TEXTURE, DIFFUSE)` yields alpha 1.0 and the same pass blend
    /// writes an opaque surface.
    Opaque,
}

/// Draw state for one baked mesh, including the flat `kind` float the shared
/// world pipeline carries into the shader. The float encodes `stage` and `cull`
/// for the shader; `kind` is the only row production consumes. The `alpha` and
/// `blend` rows record the recovered client states for traceability — the
/// viewer implements a cutout for textured meshes regardless (module note).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawState {
    pub stage: Stage,
    pub alpha: AlphaTest,
    pub cull: Cull,
    pub blend: Blend,
    pub kind: f32,
}

/// Native write sites for the states in this table (image base `0x400000`).
/// Kept for traceability; the runtime only needs the resulting [`DrawState`].
#[allow(dead_code)]
pub mod native {
    /// Ordinary world BMD submitter.
    pub const MESH_DRAW: u32 = 0x7a_9730;
    /// Refraction/reflection variant of the same submitter.
    pub const MESH_DRAW_REFRACTED: u32 = 0x7a_9120;
    /// Per-mesh loop head.
    pub const MESH_LOOP: u32 = 0x7a_9808;
    /// `LIGHTING(0)`.
    pub const LIGHTING_OFF: u32 = 0x7a_9759;
    /// `SPECULARENABLE(0)`.
    pub const SPECULAR_OFF: u32 = 0x7a_9762;
    /// `COLOROP(stage 0, MODULATE2X)`.
    pub const STAGE0_COLOROP: u32 = 0x7a_976d;
    /// `COLORARG1(stage 0, TEXTURE)` / `COLORARG2(stage 0, DIFFUSE)`.
    pub const STAGE0_COLORARG: u32 = 0x7a_977a;
    /// `ALPHAOP(stage 0, SELECTARG2)`.
    pub const STAGE0_ALPHAOP: u32 = 0x7a_9785;
    /// `ALPHAARG1/2(stage 0, TEXTURE, DIFFUSE)`.
    pub const STAGE0_ALPHAARG: u32 = 0x7a_9792;
    /// `COLOROP/ALPHAOP(stage 1, DISABLE)`.
    pub const STAGE1_DISABLE: u32 = 0x7a_979e;
    /// Two-sided test on `mesh+0x26c`.
    pub const TWO_SIDED_TEST: u32 = 0x7a_98c5;
    /// `CULLMODE(NONE)` for a two-sided mesh.
    pub const CULL_OVERRIDE: u32 = 0x7a_98cf;
    /// `CULLMODE(saved)` restore.
    pub const CULL_RESTORE: u32 = 0x7a_9a26;
    /// Per-mesh submit (`0x7b9ca0`): transform, day/night blend, index append.
    pub const MESH_SUBMIT: u32 = 0x7a_98e3;
    /// `mesh+0x2cc` compare against `0xff`.
    pub const ALPHA_COMPARE: u32 = 0x7a_98f1;
    /// `ALPHATEST(1)` on the unreachable branch.
    pub const ALPHA_TEST_ON: u32 = 0x7a_98f8;
    /// `80 * alpha / 255 + 1` reference arithmetic.
    pub const ALPHA_REF_MATH: u32 = 0x7a_990a;
    /// `ALPHATEST(0)` for every file-authored mesh.
    pub const ALPHA_TEST_OFF: u32 = 0x7a_992b;
    /// Mesh constructor default for `mesh+0x2cc`.
    pub const MESH_ALPHA_DEFAULT: u32 = 0x7b_9408;
    /// `ZBIAS(2)`/`ZBIAS(0)` in the effect path only.
    pub const EFFECT_ZBIAS: u32 = 0x41_7453;
    /// `ALPHABLENDENABLE(1)`, one per scenery pass that draws via [`MESH_DRAW`].
    pub const PASS_BLEND_ON: [u32; 4] = [0x42_b5a5, 0x42_bc17, 0x42_c32e, 0x42_c650];
    /// The matching `ALPHATEST(0)` writes.
    pub const PASS_ALPHA_TEST_OFF: [u32; 4] = [0x42_b595, 0x42_bc07, 0x42_c31e, 0x42_c660];
    /// Settings byte compared with 2 before the water-quality pass blends
    /// (`WaterEffect`, parsed at `0x40d16c`, saved at `0x40d765`).
    pub const WATER_EFFECT_GATE: u32 = 0x42_c5f3;
    /// `baked_rgb | mesh+0x2cc << 24`: the diffuse word `SELECTARG2` reads back.
    pub const DIFFUSE_ASSEMBLY: u32 = 0x7b_9d5e;
}

/// Classifies an inspected baked mesh. Unsupported versions are refused rather
/// than silently mapped onto a guessed draw state.
pub fn classify(mesh_version: u32, textured: bool, two_sided: bool) -> Result<DrawState, String> {
    if !INSPECTED_MESH_VERSIONS.contains(&mesh_version) {
        return Err(format!(
            "unsupported BMD mesh version {mesh_version:#x} for baked draw state"
        ));
    }
    let alpha = alpha_test(CLIENT_MAX_MESH_ALPHA)?;
    Ok(match (textured, two_sided) {
        // The no-cull flag only exists on v5 materials; a v4 mesh with a
        // two-sided flag is a caller bug, not something to paper over.
        (_, true) if mesh_version < 0x1000_0005 => {
            return Err("two-sided flag requires an inspected v5 material".into())
        }
        (false, false) => DrawState {
            stage: Stage::ColorOnly,
            alpha,
            cull: Cull::BackFace,
            blend: Blend::Opaque,
            kind: 1.1,
        },
        (false, true) => DrawState {
            stage: Stage::ColorOnly,
            alpha,
            cull: Cull::TwoSided,
            blend: Blend::Opaque,
            kind: 1.2,
        },
        (true, false) => DrawState {
            stage: Stage::Modulate2x,
            alpha,
            cull: Cull::BackFace,
            blend: Blend::TextureAlphaSrcOver,
            kind: 2.0,
        },
        (true, true) => DrawState {
            stage: Stage::Modulate2x,
            alpha,
            cull: Cull::TwoSided,
            blend: Blend::TextureAlphaSrcOver,
            kind: 2.1,
        },
    })
}

/// Maps `mesh+0x2cc` onto the alpha-test state. 1.4.2 only ever stores `0xff`
/// (see the module documentation), so any other value is refused: emitting a
/// cutout state the shader cannot sample correctly would be a silent guess.
pub fn alpha_test(mesh_alpha: u32) -> Result<AlphaTest, String> {
    match mesh_alpha {
        CLIENT_MAX_MESH_ALPHA => Ok(AlphaTest::Disabled),
        other => Err(format!(
            "mesh alpha {other:#04x} enables the alpha test with reference {:#04x}; \
             no 1.4.2 BMD reader writes mesh+0x2cc, so this state is unrepresentable",
            cutout_reference(other)
        )),
    }
}

/// `ALPHAREF` the client would compute on the unreachable branch
/// (`0x7a990a..0x7a9920`): `80 * alpha / 255 + 1`, saturated at `0xff`.
pub fn cutout_reference(mesh_alpha: u32) -> u8 {
    // Widened on purpose: the client's `imul` is 32x32 -> 64 bit, and a caller
    // may pass any u32 even though only `0..=0xff` is reachable from a mesh.
    let product = u64::from(mesh_alpha) * u64::from(CLIENT_ALPHA_REF_NUMERATOR);
    (product / 255 + 1).min(u64::from(u8::MAX)) as u8
}

/// How one surveyed coincidence between two baked triangles is reproduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlapOutcome {
    /// Opposite winding and at least one of the two meshes is single-sided: the
    /// cull state (`0x7a98cf` / the shader's winding drop) keeps exactly one
    /// surface for any camera side, so the coincidence never reaches the depth
    /// test twice.
    Culled,
    /// Both surfaces rasterise (same winding, or both meshes two-sided) and the
    /// later submission wins the equal-depth tie, which is the client's
    /// `LESSEQUAL` order.
    LaterSubmissionWins,
    /// Both surfaces rasterise but they do not reach the device in one
    /// submission, so no client order is proved for them.
    Unresolved,
}

/// Classifies one coincident pair. `same_submission` must be false when the two
/// triangles can land in different draw calls (for example a locally streamed
/// page and a fully preloaded world page); the result is then
/// [`OverlapOutcome::Unresolved`] rather than an assumed order.
pub fn overlap_outcome(
    same_winding: bool,
    first_two_sided: bool,
    second_two_sided: bool,
    same_submission: bool,
) -> OverlapOutcome {
    if !same_winding && !(first_two_sided && second_two_sided) {
        OverlapOutcome::Culled
    } else if same_submission {
        OverlapOutcome::LaterSubmissionWins
    } else {
        OverlapOutcome::Unresolved
    }
}

/// Integer day/night vertex blend, mirroring `bmd_baked_rgb` in `gpu.rs`:
/// `(day * (255 - w) + night * w) >> 8` per channel, `w` in `0..=255`.
pub fn blend(day: [u8; 3], night: [u8; 3], night_factor: u8) -> [u8; 3] {
    let w = u32::from(night_factor);
    let mix = |a: u8, b: u8| -> u8 { (((u32::from(a) * (255 - w)) + u32::from(b) * w) >> 8) as u8 };
    [
        mix(day[0], night[0]),
        mix(day[1], night[1]),
        mix(day[2], night[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspected_versions_classify_and_unknown_versions_are_refused() {
        assert_eq!(classify(0x1000_0005, true, false).unwrap().kind, 2.0);
        assert_eq!(
            classify(0x1000_0005, true, true).unwrap().cull,
            Cull::TwoSided
        );
        assert_eq!(classify(0x1000_0004, true, false).unwrap().kind, 2.0);
        assert_eq!(
            classify(0x1000_0004, false, false).unwrap().stage,
            Stage::ColorOnly
        );
        assert_eq!(classify(0x1000_0006, true, false).unwrap().kind, 2.0);
        assert_eq!(classify(0x1000_0302, true, false).unwrap().kind, 2.0);
        assert_eq!(classify(0x1000_0305, true, false).unwrap().kind, 2.0);
        for v in [0x1000_0003, 0x1000_0007, 0] {
            assert!(classify(v, true, false).is_err(), "{v:#x}");
        }
        // v4 has no material block, so a two-sided flag cannot be honoured.
        assert!(classify(0x1000_0004, true, true).is_err());
    }

    // Pins the recovered *client* `DrawState`, not the production renderer: the
    // viewer deviates to a cutout for textured meshes (module note). `kind` is
    // the only field production consumes, so these rows are traceability data.
    #[test]
    fn every_classified_mesh_has_no_alpha_test_and_texture_alpha_coverage() {
        for textured in [false, true] {
            for two_sided in [false, true] {
                let s = classify(0x1000_0005, textured, two_sided).unwrap();
                assert_eq!(s.alpha, AlphaTest::Disabled);
                // Coverage comes from the stage, never from the cull flag: a
                // textured mesh blends with its texture alpha (`ALPHAOP(4)` =
                // MODULATE), an untextured one is opaque (`ALPHAOP` against an
                // unbound white stage).
                let expected = if textured {
                    Blend::TextureAlphaSrcOver
                } else {
                    Blend::Opaque
                };
                assert_eq!(s.blend, expected);
                assert_eq!(s.cull == Cull::BackFace, !two_sided);
            }
        }
        assert_eq!(classify(0x1000_0005, false, false).unwrap().kind, 1.1);
        assert_eq!(classify(0x1000_0005, false, true).unwrap().kind, 1.2);
    }

    #[test]
    fn textured_scenery_must_keep_the_texture_alpha_for_coverage() {
        // `ALPHAOP(4)` is MODULATE (D3D8: DISABLE 1, SELECTARG1 2, SELECTARG2 3,
        // MODULATE 4, MODULATE2X 5), so the fragment alpha is
        // `texture.a * diffuse.a`. The per-mesh loop stamps the diffuse alpha
        // with `mesh+0x2cc << 24` (`0x7b9d5e..0x7b9d74`), which the constructor
        // leaves at 0xff (opaque multiplier, `0x7b9408`), so the sampled
        // texture alpha reaches the pass blend unchanged. Rendering this state
        // opaque *without* the cutout is what turns leaf and canopy cards into
        // solid rectangles; the viewer keeps the cutout (module note) so opaque
        // survivors are still correct. This test pins the recovered client
        // classification only.
        let diffuse = 0x0012_3456u32 | (CLIENT_MAX_MESH_ALPHA << 24);
        assert_eq!(diffuse >> 24, u32::from(u8::MAX));
        assert_eq!(
            classify(0x1000_0005, true, false).unwrap().blend,
            Blend::TextureAlphaSrcOver
        );
        assert_eq!(
            alpha_test(CLIENT_MAX_MESH_ALPHA).unwrap(),
            AlphaTest::Disabled
        );
        // A mesh-declared opacity would enter the alpha-test branch instead and
        // is refused, so the texture alpha is the only coverage input.
        assert!(alpha_test(u32::from(u8::MAX) - 1).is_err());
    }

    #[test]
    fn authored_alpha_is_refused_instead_of_becoming_a_cutout() {
        assert!(alpha_test(CLIENT_MAX_MESH_ALPHA).is_ok());
        for alpha in [0, 1, 0x80, 0xfe, 0x1_00] {
            assert!(alpha_test(alpha).is_err(), "{alpha:#x}");
        }
    }

    #[test]
    fn cutout_reference_matches_the_native_integer_arithmetic() {
        // 80 * alpha / 255 + 1, truncated, exactly as 0x7a990a..0x7a9920.
        assert_eq!(cutout_reference(0), 1);
        assert_eq!(cutout_reference(1), 1);
        assert_eq!(cutout_reference(128), 41);
        assert_eq!(cutout_reference(0xfe), 80);
        assert_eq!(cutout_reference(CLIENT_MAX_MESH_ALPHA), 81);
        assert_eq!(cutout_reference(u32::MAX), 255);
    }

    #[test]
    fn coincident_duplicates_are_won_by_the_later_draw() {
        // Reversed depth turns the client's LESSEQUAL into GREATER_EQUAL, so
        // equal depth must reach the later submission, not the earlier one.
        let passes = |incoming: f32, stored: f32| incoming >= stored;
        assert!(passes(0.5, 0.5));
        assert!(passes(0.75, 0.5));
        assert!(!passes(0.25, 0.5));
    }

    #[test]
    fn coincident_pairs_resolve_to_a_cull_or_a_proved_order() {
        // Opposite winding, single-sided: the two surfaces cannot both survive.
        assert_eq!(
            overlap_outcome(false, false, false, true),
            OverlapOutcome::Culled
        );
        // Same winding: both rasterise and the later submission wins the tie.
        assert_eq!(
            overlap_outcome(true, false, false, true),
            OverlapOutcome::LaterSubmissionWins
        );
        // Two-sided pairs are drawn twice whatever the winding.
        assert_eq!(
            overlap_outcome(false, true, true, true),
            OverlapOutcome::LaterSubmissionWins
        );
        // Split submissions are reported instead of being ordered by assumption.
        assert_eq!(
            overlap_outcome(true, false, false, false),
            OverlapOutcome::Unresolved
        );
        assert_eq!(
            overlap_outcome(false, true, true, false),
            OverlapOutcome::Unresolved
        );
    }

    #[test]
    fn integer_blend_matches_the_recovered_arithmetic() {
        // Endpoints are exact, intermediate values keep the client's >>8.
        // Note the >>8: even a full-day blend is one 255th dark, and a
        // full-night blend returns night * 255 >> 8.
        assert_eq!(blend([10, 20, 30], [200, 210, 220], 0), [9, 19, 29]);
        assert_eq!(blend([10, 20, 30], [200, 210, 220], 255), [199, 209, 219]);
        assert_eq!(blend([0, 0, 0], [255, 255, 255], 128), [127u8, 127, 127]);
        assert_eq!(blend([255, 255, 255], [0, 0, 0], 0), [254, 254, 254]);
    }

    /// The production shader must keep this module's classified kinds and the
    /// alpha-test cutout deviation. These strings are the exact WGSL/pipeline
    /// state in `gpu.rs`; a change here means the draw state above is stale.
    #[test]
    fn production_wgsl_implements_the_classified_states() {
        let gpu = include_str!("gpu.rs");
        assert!(gpu.contains("fn bmd_baked_rgb(day:u32, night:u32, night_factor:f32)->vec3<u32>"));
        assert!(
            gpu.contains("((a*(255u-w)+b*w))>>vec3<u32>(8u)") || gpu.contains("a*(255u-w)+b*w"),
            "integer day/night blend must stay integer"
        );
        assert!(gpu.contains("1.-camera.direction.w"), "night factor source");
        assert!(gpu.contains("raw_tex.rgb*i.diffuse*2."), "MODULATE2X");
        let winding = "((i.kind>1.05 && i.kind<1.15) || (i.kind>1.5 && i.kind<2.05))";
        assert!(
            gpu.contains(winding) && gpu.contains("&& camera.clip.w == 0."),
            "single-sided winding drop must cover both baked colour kinds"
        );
        // Textured baked meshes must take the alpha-test cutout together with
        // grass/trees. Transparent texels must be discarded (no colour, no
        // depth) or the depth-writing scenery pass breaks the water refraction
        // guard (square lily pads) and hides a canopy behind a nearer card's
        // transparent gaps. This is the viewer's deliberate deviation from the
        // client's ALPHATEST(0) + blend state documented above — with one
        // exception: the `.sdr.` water/lava band (kind 2.02/2.12) is dropped
        // from this pass and drawn by `fs_sdr` in the blended pass below.
        assert!(
            gpu.contains("if !sdr_blend_band(i.kind) && ((i.kind>1.5 && i.kind<2.5)"),
            "textured baked meshes must share the cutout discard outside the sdr band"
        );
        // Survivors are opaque, not the sampled texture alpha through a
        // SRCALPHA blend, and the scenery pipeline must stay unblended: a
        // surviving partial-alpha texel would otherwise still write depth
        // against the pre-water colour. The band's `select` returns the
        // client's `texture.a * diffuse.a` for its own pass alone.
        assert!(
            gpu.contains("select(1.,raw_tex.a*i.opacity,sdr_blend_band(i.kind))"),
            "textured baked meshes must render opaque after the cutout outside the band"
        );
        let scenery_start = gpu.find("scenery GPU pipeline").unwrap();
        let scenery_end = gpu.find("sdr blend GPU pipeline").unwrap();
        let scenery = &gpu[scenery_start..scenery_end];
        assert!(
            scenery.contains("blend: None") && !scenery.contains("BlendFactor"),
            "the scenery pipelines must stay unblended"
        );
        // The band's own pass blends with the client's SRCALPHA/INVSRCALPHA
        // while keeping depth write off — the lily-pad regression the cutout
        // exists to avoid cannot return through a depth-writing translucent
        // card drawn after the world.
        assert!(
            gpu.contains("entry_point: \"fs_sdr\"") && gpu.contains("depth_write_enabled: false"),
            "the sdr band must draw in its own blended, depth-write-off pass"
        );
        // The classified kinds must land on the intended side of the shader's
        // single-sided winding band.
        let single = classify(0x1000_0005, true, false).unwrap();
        let single_flat = classify(0x1000_0005, false, false).unwrap();
        let two = classify(0x1000_0005, true, true).unwrap();
        let two_flat = classify(0x1000_0005, false, true).unwrap();
        assert!(single.kind > 1.5 && single.kind < 2.05, "{single:?}");
        assert!(
            single_flat.kind > 1.05 && single_flat.kind < 1.15,
            "{single_flat:?}"
        );
        assert!(!(two.kind > 1.5 && two.kind < 2.05), "{two:?}");
        assert!(
            !(two_flat.kind > 1.05 && two_flat.kind < 1.15),
            "{two_flat:?}"
        );
        // Scenery must compare equal depth as a pass so duplicates keep the
        // later submission, as the client's LESSEQUAL test does.
        assert!(
            gpu.contains("depth_compare: wgpu::CompareFunction::GreaterEqual"),
            "scenery pipeline must keep the equal-depth tie"
        );
        // Baked meshes must not consume the ordinary receiving light.
        let baked = gpu.split("fn bmd_baked_rgb").nth(1).expect("baked helper");
        let baked = baked.split("fn ").next().unwrap();
        assert!(!baked.contains("camera.light"));
    }
}
