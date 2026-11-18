use super::super::{adapter, build, read_target_pixels, Camera, DetailSettings, Renderer, TILE};
use crate::{
    modules::lighter::{Lighter, Mode, Settings},
    Terrain,
};
use glam::Vec3;

const SKY: [u8; 4] = [25, 75, 175, 255];

struct FixtureRoot(std::path::PathBuf);
impl FixtureRoot {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("pwe-lighter-test-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let root = Self(dir);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(1, 1, image::Rgba(SKY))
            .write_to(&mut bytes, image::ImageFormat::Bmp)
            .unwrap();
        let keys = crate::sky_gpu::SKY_DEFAULTS.map(crate::sky_gpu::sky_pack_key);
        let entries = keys
            .each_ref()
            .map(|key| (key.as_str(), bytes.get_ref().as_slice()));
        crate::pck::write_test_archive(&root.0.join("textures.pck"), &entries);
        root
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(element: &std::path::Path) -> Terrain {
    let element = element.to_path_buf();
    let side = TILE as f32;
    let mut elevation = crate::terrain_store::Builder::new_resident(TILE, TILE, TILE).unwrap();
    elevation
        .block(
            0,
            0,
            &vec![0.; TILE * TILE],
            &vec![[0, 127, 0]; TILE * TILE],
        )
        .unwrap();
    let (heights, normals) = elevation.finish(0).unwrap();
    let tree = crate::trees::TreeLod {
        branch: crate::trees::Part {
            vertices: [[-1., 0., 0.], [1., 0., 0.], [0., 0., 6.]]
                .map(|pos| crate::trees::Vert {
                    pos,
                    normal: [0., 1., 0.],
                    uv: [0.; 2],
                })
                .into(),
            indices: vec![0, 1, 2],
        },
        frond: Default::default(),
        branch_alpha_ref: None,
        frond_alpha_ref: None,
        leaves: vec![],
        rocking: vec![],
        rocking_clusters: 0,
    };
    let red = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
    Terrain {
        element: element.clone(),
        client_root: element.clone(),
        map_dir: element.clone(),
        map: crate::world_catalog::WorldMap {
            id: "lighter-fixture".into(),
            name: "Lighter fixture".into(),
            display_name: "Lighter fixture".into(),
            dir: element.clone(),
            element: element.clone(),
            instance_ids: vec![],
            rows: 1,
            cols: 1,
        },
        startup_notice: None,
        grid: TILE,
        width: TILE,
        height: TILE,
        heights,
        normals,
        grid_spacing: 1.,
        materials: crate::material::single_layer_fixture(TILE),
        subterrain_names: vec!["fixture".into()],
        objects: crate::objects::Objects::open(&element, Vec::new()).unwrap(),
        water: vec![crate::water::WaterArea {
            export_id: 1,
            color: 0xff40aaff,
            simple: false,
            height: 1.,
            grid_size: 5.,
            columns: 2,
            rows: 1,
            center: [0., 1., -5.],
            mask: vec![1, 0],
            shore_waves: vec![],
        }],
        environments: Default::default(),
        scene_lights: Default::default(),
        scene: std::sync::Arc::new(crate::scene::Scene {
            width: side,
            length: side,
            block_size: side,
            rows: 1,
            cols: 1,
            tree_types: vec![],
            grass_types: vec![],
            blocks: vec![],
        }),
        scenery_residency: Default::default(),
        terrain_material_window: None,
        models: Default::default(),
        water_normals: vec![
            vec![image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([128, 255, 128, 255])
            )];
            29
        ],
        water_simple_frames: vec![
            vec![image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([64, 170, 255, 255]),
            )];
            30
        ],
        trees: crate::trees::Trees {
            types: vec![Some(crate::trees::TreeMesh {
                lods: vec![tree],
                bark: red.clone(),
                composite: red,
                bark_mips: vec![],
                composite_mips: vec![],
            })],
            placements: vec![crate::scene::Tree {
                tree_type: 0,
                position: [8., 0., 0.],
            }],
        },
        grass: crate::grass::Grass {
            areas: vec![],
            types: vec![],
            models: vec![],
            textures: Default::default(),
            texture_mips: Default::default(),
        },
        cloud_surface: None,
        scenery_placements: None,
    }
}

#[test]
fn far_terrain_keeps_only_triangles_outside_the_resident_footprint() {
    let points = [[-4., 0., 0.], [0., 0., 0.], [0., 0., 4.], [4., 0., 4.]];
    let far = super::Mesh {
        vertices: points
            .map(|p| super::Vertex::new(p, [0., 1., 0.], [0., 0.]))
            .into(),
        triangles: vec![[0, 1, 2], [1, 2, 3]],
        surfaces: vec![],
    };
    let exterior = super::uncovered_far(&far, [-4., 0., 0., 4.], 7);
    assert_eq!(exterior.triangles, [[1, 2, 3]]);
    assert_eq!(exterior.surfaces, [7]);
    assert!(super::uncovered_far(&far, [-4., 4., 0., 4.], 7)
        .triangles
        .is_empty());
}

#[test]
#[ignore = "requires a Vulkan adapter (lavapipe is sufficient)"]
fn lighter_streamed_world_pixels() {
    // Use the real viewer upload/build and replacement dispatch, not a Packet
    // assembled in the test. A native dry cell, wet cell and SpeedTree instance
    // must reach the ray scene, and mesh streaming must invalidate it.
    let root = FixtureRoot::new();
    let mut terrain = fixture(&root.0);
    let mut camera = Camera::new(&terrain);
    camera.pos = Vec3::new(-2.5, 5., 0.);
    camera.yaw = 0.;
    camera.pitch = -std::f32::consts::FRAC_PI_4;
    camera.far = 40.;
    camera.time_of_day = 12.;
    camera.celestial_discs = false;
    camera.atmospheric_fog = false;
    camera.water_reflections = false;
    camera.terrain_lightmaps = false;
    camera.bloom = false;
    camera.sharpen = false;
    camera.client_gamma = false;
    let detail = DetailSettings::default();
    let mut residency = Default::default();
    let mut mesh = |terrain: &mut Terrain, atlas| {
        build(
            terrain,
            -2.5,
            0.,
            detail,
            &mut residency,
            false,
            atlas,
            false,
        )
        .unwrap()
    };
    let m = mesh(&mut terrain, 0);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let (a, device, queue) = pollster::block_on(adapter(&instance, None, Mode::Software)).unwrap();
    let cpu_adapter = a.get_info().device_type == wgpu::DeviceType::Cpu;
    let lighter = Lighter::new(
        Settings {
            mode: Mode::Software,
            lighting: false,
            shadows: false,
            bounces: 0,
            resolution: 100,
            ..Settings::default()
        },
        &a,
        &device,
    );
    const SIZE: u32 = 64;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut r = Renderer::new(
        device,
        queue,
        format,
        SIZE,
        SIZE,
        m,
        &mut terrain,
        false,
        lighter,
    )
    .unwrap();
    let target = r.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Lighter streamed world test"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    let view = target.create_view(&Default::default());
    let raw = target.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    let render_center = |r: &mut Renderer, c: &Camera| -> [u8; 4] {
        r.render(
            &view,
            &raw,
            c.matrix(SIZE, SIZE, c.far),
            c,
            SIZE,
            SIZE,
            true,
        );
        let bytes = read_target_pixels(r, &target, SIZE, SIZE).unwrap();
        let offset = (4 * (SIZE / 2 * SIZE + SIZE / 2)) as usize;
        bytes[offset..offset + 4].try_into().unwrap()
    };
    let at_center = |r: &mut Renderer, c: &Camera| {
        let pixel = render_center(r, c);
        assert_eq!(r.lighter.error(), None);
        pixel
    };
    let wet = at_center(&mut r, &camera);
    assert!(
        wet[2] > wet[1] && wet[1] > 0,
        "water did not cover wet cell: {wet:?}"
    );
    camera.pos.x = 2.5;
    assert_eq!(
        at_center(&mut r, &camera),
        [0, 255, 0, 255],
        "water covered an authored dry cell"
    );
    camera.pos = Vec3::new(8., 3., 7.);
    camera.pitch = 0.;
    assert_eq!(
        at_center(&mut r, &camera),
        [255, 0, 0, 255],
        "tree geometry missing from the ray scene"
    );

    camera.pos.x = 0.;
    camera.pitch = 0.6;
    let sky = at_center(&mut r, &camera);
    assert!(
        sky.into_iter().zip(SKY).all(|(a, b)| a.abs_diff(b) <= 1),
        "authored sky not sampled on misses: {sky:?}"
    );

    terrain.water.clear();
    r.update_mesh(mesh(&mut terrain, r.grass_atlas_key))
        .unwrap();
    camera.pos = Vec3::new(-2.5, 5., 0.);
    camera.pitch = -std::f32::consts::FRAC_PI_4;
    assert_eq!(
        at_center(&mut r, &camera),
        [0, 255, 0, 255],
        "stale water after stream replacement"
    );
    if cpu_adapter {
        r.lighter.settings.mode = Mode::Hardware;
        let refused = render_center(&mut r, &camera);
        assert!(r.lighter.error().is_some());
        assert_eq!(
            refused,
            [0, 0, 0, 255],
            "unsupported mode silently fell back"
        );
    }
}
