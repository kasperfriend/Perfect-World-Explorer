//! Material/pass fog matrix and the fog values each production pass binds.
//!
//! Recovered behavior this table records (evidence in
//! `Findings/Phase-1-Agent-3-report.md`):
//!
//! * Main, reflection and above-water scene passes use the engine table fog
//!   (`D3DFOG_LINEAR`) over view-space z with the camera's active region fog:
//!   air above water, the interpolated regional underwater fog below it.
//! * The clipped refraction pass (camera above a water plane, geometry below it
//!   clipped at `0x7cc6ac`) replaces the table fog for grass, baked models and
//!   terrain with the recovered eye-ray exponential:
//!   `exp2(-density * depth * length(eye - original) / (eye.y - original.y))`
//!   with density `0.075` (`0x651319` grass, `0x7a91dd` BMD, `0x7c26e0`
//!   terrain). Grass measures submerged depth from the *displaced* vertex,
//!   baked models and terrain clamp above-plane depth to zero, and terrain also
//!   emits depth-only darkness `exp2(-2 * density * depth)`.
//! * Models, trees and shoreline geometry keep table fog in that pass – the
//!   shaders never receive the exponential inputs.
//! * The below-surface scene (`0x7cca50`) is rendered with the *air* fog while
//!   the camera is underwater; the underwater fog is restored afterwards.
//! * The water surface and the shoreline waves keep table fog too. The client
//!   refreshes the surface object's fog fields from the active region fog on
//!   every update (`0x6bfa34`/`0x6bfa47` through the setters at `0x780990` and
//!   `0x7809c0`) and rebinds them right before the draw
//!   (`0x7ca9d9`/`0x7ca9e8`/`0x7ca9f7`, plus `0x7cb444`/`0x7cb502`/`0x7cb94e`
//!   for the edge and refraction passes). Neither water shader carries fog
//!   math: `wavevs.txt` writes `oFog = 1.0` and `waveblend.txt` has no fog
//!   constants, so the pass state applies unchanged.
//!
//! Transparent/alpha-tested variants inherit their class row: the recovered
//! shaders return encoded material color and the client applied fog after the
//! pixel shader, so alpha rejection cannot change the fog quantity.

/// A fog parameter set as bound to a pass.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fog {
    pub start: f32,
    pub end: f32,
    pub color: [f32; 3],
    /// `false` uploads the disabled flag; shaders then return unfogged color.
    pub enabled: bool,
}

impl Fog {
    pub fn disabled() -> Self {
        Self {
            start: 0.,
            end: 1.,
            color: [0.; 3],
            enabled: false,
        }
    }

    /// `fog_range` uniform (start, end, enabled, reserved).
    pub fn range(&self) -> [f32; 4] {
        [self.start, self.end, if self.enabled { 1. } else { 0. }, 0.]
    }

    /// `fog_color` uniform (encoded client ARGB channels).
    pub fn color4(&self) -> [f32; 4] {
        [self.color[0], self.color[1], self.color[2], 1.]
    }
}

/// The camera's active fog: the regional underwater set when submerged,
/// otherwise the authored atmosphere. The enable flag is the client's
/// `atmospheric fog` gate and is always on while underwater.
pub fn camera_fog(camera_underwater: bool, atmospheric: bool, air: Fog, water: Fog) -> Fog {
    if camera_underwater {
        Fog {
            enabled: true,
            ..water
        }
    } else {
        Fog {
            enabled: atmospheric,
            ..air
        }
    }
}

/// Per-plane clipped refraction (`0x7cc6ac`): the water fog color with the
/// recovered plane-relative range. `start`/`end` come from the traced
/// `refraction_fog_range` (eye-to-plane height and 55% of the regional
/// underwater fog end).
pub fn refraction_fog(water: Fog, start: f32, end: f32) -> Fog {
    Fog {
        start,
        end,
        color: water.color,
        enabled: true,
    }
}

/// Below-surface scene (`0x7cca50`): air fog while the camera is underwater.
pub fn below_surface_fog(air: Fog) -> Fog {
    Fog {
        enabled: true,
        ..air
    }
}

/// The fog state the water surface binds immediately before drawing.
///
/// `A3DTerrainWater` seeds two triples in its constructor
/// (`0x7c9212..0x7c9248`): colour `0xffffffff`/100/400 at `0x6bc`/`0x6c0`/`0x6c4`
/// and colour `0xff0000ff`/10/60 at `0x6c8`/`0x6cc`/`0x6d0`. Both are only
/// start-up seeds: the scene fog update calls the two interface setters with the
/// active regional fog on every update (`0x6bfa34` → `0x780990` for the first
/// triple, `0x6bfa47` → `0x7809c0` for the second), passing the same colour and
/// the `0.0`/`400.0` range it pushes into `D3DRS_FOGCOLOR`/`FOGSTART`/`FOGEND`
/// at `0x6bf9d8..0x6bfa11`.
///
/// The drawing sites then rebind those object fields through the engine fog
/// setters: the surface renderer at `0x7ca9d9`/`0x7ca9e8`/`0x7ca9f7`, the edge
/// and refraction passes at `0x7cb444` and `0x7cb94e`, and the pass at
/// `0x7cb502`. The water path never writes `D3DRS_FOGENABLE`, so the surface
/// inherits the scene fog enable state and uses the same linear table fog over
/// view-space `z` as every other table-fog draw.
pub fn water_fog(camera_underwater: bool, atmospheric: bool, air: Fog, water: Fog) -> Fog {
    camera_fog(camera_underwater, atmospheric, air, water)
}

/// Materials that can be submitted in the water-related passes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Material {
    Terrain,
    Grass,
    BakedModel,
    Model,
    /// Alpha-tested/transparent model variant; shares the model row.
    TransparentModel,
    Tree,
    Shore,
    Sky,
    /// The water surface itself (see the module note: known gap).
    WaterSurface,
    /// Client rain/snow particles, drawn separately with view-specific fog.
    Precipitation,
    /// Indexed `world.cld` cumulus surfaces, drawn with per-view fog.
    CloudSurface,
}

/// Pass in which a material is submitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pass {
    Main,
    Reflection,
    /// Camera above the plane, geometry below it clipped (`0x7cc6ac`).
    Refraction,
    /// Camera below the plane, unmirrored scene above it (`0x7cca50`).
    BelowSurface,
}

/// How distance/depth becomes a fog factor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quantity {
    /// D3D table fog over view-space z (`D3DFOG_LINEAR` over `1/w`).
    TableFogViewZ,
    /// Recovered refraction vertex exponential (`exp2`, base two).
    EyeRayExponential,
    /// Material is not fogged in this pass.
    None,
}

/// Which range/color pair the pass binds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeColor {
    /// The camera's active regional fog (air or underwater).
    Camera,
    /// The air fog explicitly (below-surface scene while underwater).
    Air,
    /// The regional water fog color with the plane-relative refraction range.
    WaterRefraction,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub quantity: Quantity,
    pub range: RangeColor,
    /// Terrain additionally multiplies diffuse by `exp2(-2 * density * depth)`.
    pub darkness: bool,
    /// True when production applies the recovered behavior for this cell.
    pub implemented: bool,
    /// Native address or shader that establishes the row.
    pub evidence: &'static str,
}

const fn row_of(
    quantity: Quantity,
    range: RangeColor,
    darkness: bool,
    implemented: bool,
    evidence: &'static str,
) -> Row {
    Row {
        quantity,
        range,
        darkness,
        implemented,
        evidence,
    }
}

/// The complete applicable matrix. Cells that are `implemented == false` are
/// the explicit remaining gaps, not placeholders.
pub fn row(material: Material, pass: Pass) -> Row {
    // Fully qualified on purpose: `None` exists in both enums.
    use Material::{
        BakedModel, CloudSurface, Grass, Model, Precipitation, Shore, Sky, Terrain,
        TransparentModel, Tree, WaterSurface,
    };
    use Pass::{BelowSurface, Main, Reflection, Refraction};
    use Quantity::{EyeRayExponential, TableFogViewZ};
    use RangeColor::{Air, Camera, WaterRefraction};
    match (material, pass) {
        // Sky is authored without the world fog state in every pass.
        (Sky, _) => row_of(
            Quantity::None,
            RangeColor::None,
            false,
            true,
            "sky panorama pass has no fog constants",
        ),
        (Precipitation, Main) | (Precipitation, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "weather_gpu fogged() with the active camera/region table fog",
        ),
        // The clipped underwater refraction excludes above-water precipitation.
        (Precipitation, Refraction) => row_of(
            Quantity::None,
            RangeColor::None,
            false,
            true,
            "water clip and pass admission omit above-water precipitation",
        ),
        // From an underwater camera, the secondary view renders the scene above
        // the surface with air fog; precipitation follows that same state.
        (Precipitation, BelowSurface) => row_of(
            TableFogViewZ,
            Air,
            false,
            true,
            "weather_gpu receives the below-surface view's air-fog binding",
        ),
        (CloudSurface, Main) | (CloudSurface, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "cloud_gpu receives the active camera/region table fog",
        ),
        (CloudSurface, Refraction) => row_of(
            Quantity::None,
            RangeColor::None,
            false,
            true,
            "underwater refraction omits clouds above its water clip plane",
        ),
        (CloudSurface, BelowSurface) => row_of(
            TableFogViewZ,
            Air,
            false,
            true,
            "cloud_gpu receives the above-water secondary view's air fog",
        ),
        // The water surface gap is described in the module documentation.
        // The surface rebinds the object's fog fields right before its draw
        // (0x7ca9d9..0x7ca9f7) and those fields hold the region fog
        // (0x6bfa47); the shaders carry no fog math and water_vs writes no oFog,
        // so the quantity is the engine table fog.
        (WaterSurface, Main) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "0x6bfa47 refresh; 0x7cb502/0x7ca9f9 bind before the draw",
        ),
        (WaterSurface, _) => row_of(
            Quantity::None,
            RangeColor::None,
            false,
            true,
            "water surface not submitted in this pass",
        ),
        // Shoreline waves are drawn only into the main scene target.
        // wavevs.txt writes oFog = c0.x = 1.0 ("set no fog") and waveblend.txt
        // has no fog constants, so the waves add no fog of their own; they are
        // drawn in the water pass under the same bound fog state.
        (Shore, Main) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "wavevs.txt oFog=1.0; waveblend.txt has no fog; pass state shared",
        ),
        (Shore, _) => row_of(
            Quantity::None,
            RangeColor::None,
            false,
            true,
            "shoreline drawn only in the main pass",
        ),
        // Main and reflection passes: engine table fog for every world material.
        (Terrain, Main) | (Terrain, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "terrain_gpu fogged() (0x7c4edb)",
        ),
        (Grass, Main) | (Grass, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "grass fogged() table fog",
        ),
        (BakedModel, Main) | (BakedModel, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "bmd fogged() (0x7a91dd main path)",
        ),
        (Model, Main) | (Model, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "model_receiver table fog (fog_range.z)",
        ),
        (TransparentModel, Main) | (TransparentModel, Reflection) => row_of(
            TableFogViewZ,
            Camera,
            false,
            true,
            "same shader/fog as the model row",
        ),
        (Tree, Main) | (Tree, Reflection) => {
            row_of(TableFogViewZ, Camera, false, true, "far_trees_gpu fogged()")
        }
        // Clipped refraction: recovered exponential for grass/BMD/terrain.
        (Grass, Refraction) => row_of(
            EyeRayExponential,
            WaterRefraction,
            false,
            true,
            "grass_wave_refract 0x651319 (c91=(1,water,0.075,0))",
        ),
        (BakedModel, Refraction) => row_of(
            EyeRayExponential,
            WaterRefraction,
            false,
            true,
            "lm_refract_vs 0x7a91dd (above-plane depth clamped)",
        ),
        (Terrain, Refraction) => row_of(
            EyeRayExponential,
            WaterRefraction,
            true,
            true,
            "terrain_refract_vs 0x7c26e0 (+depth darkness)",
        ),
        (Model, Refraction) | (TransparentModel, Refraction) => row_of(
            TableFogViewZ,
            WaterRefraction,
            false,
            true,
            "model passes receive the refraction range/color only",
        ),
        (Tree, Refraction) => row_of(
            TableFogViewZ,
            WaterRefraction,
            false,
            true,
            "tree shader has no refraction exponential inputs",
        ),
        // Below-surface scene while the camera is underwater.
        (Terrain, BelowSurface)
        | (Grass, BelowSurface)
        | (BakedModel, BelowSurface)
        | (Model, BelowSurface)
        | (TransparentModel, BelowSurface)
        | (Tree, BelowSurface) => row_of(
            TableFogViewZ,
            Air,
            false,
            true,
            "0x7cca50 restores air fog for the unmirrored scene",
        ),
    }
}

/// Model vertex fog fallback. Every model pass enables table fog, so the
/// interpolated vertex factor is never the active quantity.
pub const MODEL_VERTEX_FOG: f32 = 1.;

#[cfg(test)]
mod tests {
    use super::*;

    const MATERIALS: [Material; 11] = [
        Material::Terrain,
        Material::Grass,
        Material::BakedModel,
        Material::Model,
        Material::TransparentModel,
        Material::Tree,
        Material::Shore,
        Material::Sky,
        Material::WaterSurface,
        Material::Precipitation,
        Material::CloudSurface,
    ];
    const PASSES: [Pass; 4] = [
        Pass::Main,
        Pass::Reflection,
        Pass::Refraction,
        Pass::BelowSurface,
    ];

    fn air() -> Fog {
        Fog {
            start: 10.,
            end: 600.,
            color: [0.1, 0.2, 0.3],
            enabled: true,
        }
    }

    fn water() -> Fog {
        Fog {
            start: 0.,
            end: 120.,
            color: [0.02, 0.1, 0.12],
            enabled: true,
        }
    }

    #[test]
    fn every_material_pass_cell_states_its_evidence() {
        for material in MATERIALS {
            for pass in PASSES {
                let cell = row(material, pass);
                assert!(!cell.evidence.is_empty(), "{material:?} {pass:?}");
                if cell.quantity == Quantity::None {
                    assert_eq!(cell.range, RangeColor::None, "{material:?} {pass:?}");
                } else {
                    assert_ne!(cell.range, RangeColor::None, "{material:?} {pass:?}");
                }
            }
        }
    }

    #[test]
    fn precipitation_inherits_each_render_views_fog_and_water_admission() {
        for pass in [Pass::Main, Pass::Reflection] {
            assert_eq!(row(Material::Precipitation, pass).range, RangeColor::Camera);
            assert_eq!(
                row(Material::Precipitation, pass).quantity,
                Quantity::TableFogViewZ
            );
        }
        assert_eq!(
            row(Material::Precipitation, Pass::Refraction).quantity,
            Quantity::None
        );
        assert_eq!(
            row(Material::Precipitation, Pass::BelowSurface).range,
            RangeColor::Air
        );
        assert!(row(Material::Precipitation, Pass::BelowSurface).implemented);
    }

    #[test]
    fn cloud_surfaces_receive_main_reflection_and_secondary_air_fog() {
        for pass in [Pass::Main, Pass::Reflection] {
            assert_eq!(row(Material::CloudSurface, pass).range, RangeColor::Camera);
            assert_eq!(
                row(Material::CloudSurface, pass).quantity,
                Quantity::TableFogViewZ
            );
        }
        assert_eq!(
            row(Material::CloudSurface, Pass::Refraction).quantity,
            Quantity::None
        );
        assert_eq!(
            row(Material::CloudSurface, Pass::BelowSurface).range,
            RangeColor::Air
        );
        assert!(row(Material::CloudSurface, Pass::BelowSurface).implemented);
    }

    #[test]
    fn only_terrain_refraction_carries_depth_darkness() {
        for material in MATERIALS {
            for pass in PASSES {
                let cell = row(material, pass);
                let expected = material == Material::Terrain && pass == Pass::Refraction;
                assert_eq!(cell.darkness, expected, "{material:?} {pass:?}");
            }
        }
    }

    #[test]
    fn refraction_exponentials_are_the_three_recovered_materials_only() {
        let exponential = MATERIALS
            .iter()
            .filter(|m| row(**m, Pass::Refraction).quantity == Quantity::EyeRayExponential)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            exponential,
            vec![Material::Terrain, Material::Grass, Material::BakedModel]
        );
        // The same materials use table fog in main/reflection: no universal
        // screen-space approximation replaced them.
        for material in exponential {
            assert_eq!(row(material, Pass::Main).quantity, Quantity::TableFogViewZ);
            assert_eq!(
                row(material, Pass::Reflection).quantity,
                Quantity::TableFogViewZ
            );
        }
    }

    #[test]
    fn transparent_model_variants_inherit_the_model_row() {
        for pass in PASSES {
            let model = row(Material::Model, pass);
            let transparent = row(Material::TransparentModel, pass);
            assert_eq!(model.quantity, transparent.quantity);
            assert_eq!(model.range, transparent.range);
            assert_eq!(model.implemented, transparent.implemented);
        }
    }

    #[test]
    fn remaining_gaps_are_named_cells_not_blanket_blockers() {
        let gaps = MATERIALS
            .iter()
            .flat_map(|m| PASSES.map(|p| (*m, p)))
            .filter(|(m, p)| !row(*m, *p).implemented)
            .collect::<Vec<_>>();
        // Both former gaps are recovered: the water surface binds its own
        // fog triple and the waves add none of their own.
        assert!(gaps.is_empty(), "unexpected gaps: {gaps:?}");
    }

    #[test]
    fn camera_fog_switches_range_color_with_the_submerged_state() {
        let above = camera_fog(false, true, air(), water());
        assert_eq!(above.color, air().color);
        assert_eq!((above.start, above.end), (air().start, air().end));
        assert!(above.enabled);
        let no_atmosphere = camera_fog(false, false, air(), water());
        assert!(!no_atmosphere.enabled);
        assert_eq!(no_atmosphere.range()[2], 0.);
        // Underwater the regional water fog wins and is always enabled.
        let below = camera_fog(true, false, air(), water());
        assert_eq!(below.color, water().color);
        assert!(below.enabled);
        assert_eq!(below.range(), [water().start, water().end, 1., 0.]);
    }

    #[test]
    fn normal_reflected_underwater_and_clipped_views_keep_distinct_regional_fog() {
        let above = camera_fog(false, true, air(), water());
        let reflected = camera_fog(false, true, air(), water());
        assert_eq!(reflected, above); // mirrored camera uses the same active air fog
        assert_eq!(
            row(Material::Terrain, Pass::Reflection).range,
            RangeColor::Camera
        );
        assert_eq!(
            row(Material::Model, Pass::Reflection).range,
            RangeColor::Camera
        );

        let submerged = camera_fog(true, false, air(), water());
        assert_eq!(submerged.color, water().color);
        assert!(submerged.enabled); // water fog stays on independent of air toggle
        assert_eq!(row(Material::Terrain, Pass::Main).range, RangeColor::Camera);

        let clipped_refraction = refraction_fog(water(), 7., 66.);
        assert_eq!(clipped_refraction.color, water().color);
        assert_eq!(clipped_refraction.range(), [7., 66., 1., 0.]);
        assert_eq!(
            row(Material::Terrain, Pass::Refraction).range,
            RangeColor::WaterRefraction
        );
        assert_eq!(
            row(Material::Grass, Pass::Refraction).quantity,
            Quantity::EyeRayExponential
        );
        assert_eq!(
            row(Material::Model, Pass::Refraction).range,
            RangeColor::WaterRefraction
        );

        let below_surface = below_surface_fog(air());
        assert_eq!(below_surface.color, air().color); // native pass restores air fog
        assert_eq!(
            row(Material::Terrain, Pass::BelowSurface).range,
            RangeColor::Air
        );

        // View distance is the camera far clip, not an inferred fog distance.
        // Disabling atmospheric fog leaves the authored range/color intact.
        let no_fog = camera_fog(false, false, air(), water());
        assert!(!no_fog.enabled);
        assert_eq!((no_fog.start, no_fog.end), (air().start, air().end));
        assert_eq!(
            row(Material::Terrain, Pass::Main).quantity,
            Quantity::TableFogViewZ
        );
    }

    #[test]
    fn refraction_and_below_surface_bind_their_own_fog() {
        let clipped = refraction_fog(water(), 3., 77.);
        assert_eq!(clipped.color, water().color);
        assert_eq!(clipped.range(), [3., 77., 1., 0.]);
        assert_eq!(
            clipped.color4(),
            [water().color[0], water().color[1], water().color[2], 1.]
        );
        // The below-surface scene uses air fog even while the camera is under
        // water, matching 0x7cca50's explicit air-fog set before the draw.
        let restored = below_surface_fog(air());
        assert_eq!(restored.color, air().color);
        assert!(restored.enabled);
        assert_eq!(Fog::disabled().range(), [0., 1., 0., 0.]);
    }

    #[test]
    fn model_vertex_fog_is_never_the_active_quantity() {
        assert_eq!(MODEL_VERTEX_FOG, 1.);
        for pass in PASSES {
            assert_ne!(row(Material::Model, pass).quantity, Quantity::None);
        }
    }

    #[test]
    fn water_surface_uses_the_active_region_fog() {
        let air = Fog {
            start: 0.,
            end: 400.,
            color: [0.5, 0.6, 0.7],
            enabled: true,
        };
        let water = Fog {
            start: 12.,
            end: 64.,
            color: [0.1, 0.2, 0.4],
            enabled: true,
        };
        assert_eq!(
            water_fog(false, true, air, water),
            camera_fog(false, true, air, water)
        );
        assert_eq!(
            water_fog(true, false, air, water),
            camera_fog(true, false, air, water)
        );
        // The surface never invents a fixed triple: with the atmosphere off and
        // the camera above water it stays disabled, exactly like the scene.
        let off = water_fog(false, false, air, water);
        assert!(!off.enabled);
        // Both water rows use the camera's active regional fog.
        for material in [Material::WaterSurface, Material::Shore] {
            let row = row(material, Pass::Main);
            assert_eq!(row.range, RangeColor::Camera, "{material:?}");
            assert_eq!(row.quantity, Quantity::TableFogViewZ);
            assert!(row.implemented);
        }
    }
}
