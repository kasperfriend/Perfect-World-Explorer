use super::super::{
    adapter, build, read_target_pixels, readback, Camera, DetailSettings, Renderer, TILE,
};
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
        let sky = encode(
            &image::RgbaImage::from_pixel(1, 1, image::Rgba(SKY)),
            image::ImageFormat::Bmp,
        );
        let wall = encode(
            &image::RgbaImage::from_fn(4, 1, |x, _| {
                image::Rgba(if x < 2 {
                    [255, 0, 255, 0]
                } else {
                    [0, 0, 255, 255]
                })
            }),
            image::ImageFormat::Tga,
        );
        let keys = crate::sky_gpu::SKY_DEFAULTS.map(crate::sky_gpu::sky_pack_key);
        let entries: Vec<_> = keys
            .iter()
            .map(|key| (key.as_str(), sky.as_slice()))
            .collect();
        crate::pck::write_test_archive(&root.0.join("textures.pck"), &entries);
        crate::pck::write_test_archive(
            &root.0.join("building.pck"),
            &[("building/lighter_wall.tga", &wall)],
        );
        let bmd = bmd_fixture();
        let mut distant = bmd_header([160., 0., 0.], 1);
        bmd_quad(
            &mut distant,
            [
                [0., 0.25, -10.],
                [0., 0.25, 10.],
                [0., 20., -10.],
                [0., 20., 10.],
            ],
            [-1., 0., 0.],
            "building/lighter_wall.tga",
        );
        crate::pck::write_test_archive(
            &root.0.join("litmodels.pck"),
            &[
                ("litmodels/a01/lighter.bmd", &bmd),
                ("litmodels/a01/distant.bmd", &distant),
            ],
        );
        let sprite = gfx_header() + SPRITE;
        let light = gfx_header() + LIGHT;
        let glow = encode(
            &image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 32, 0, 128])),
            image::ImageFormat::Png,
        );
        crate::pck::write_test_archive(
            &root.0.join("gfx.pck"),
            &[
                ("gfx/scene/lighter-sprite.gfx", sprite.as_bytes()),
                ("gfx/scene/lighter-light.gfx", light.as_bytes()),
                ("gfx/textures/lighter-glow.png", &glow),
            ],
        );
        root
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn encode(image: &image::RgbaImage, format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

fn bmd_header(position: [f32; 3], meshes: u32) -> Vec<u8> {
    let mut b = b"MOXB".to_vec();
    b.extend_from_slice(&0x1000_0002u32.to_le_bytes());
    for v in [1f32, 1., 1., 0., 0., 1., 0., 1., 0.]
        .into_iter()
        .chain(position)
    {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&meshes.to_le_bytes());
    b
}

fn bmd_quad(b: &mut Vec<u8>, positions: [[f32; 3]; 4], n: [f32; 3], texture: &str) {
    b.extend_from_slice(&0x1000_0005u32.to_le_bytes());
    b.extend_from_slice(&[0; 64]); // mesh name
    b.extend_from_slice(texture.as_bytes());
    b.resize(b.len() + 256 - texture.len(), 0);
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    for (p, uv) in positions
        .into_iter()
        .zip([[0f32, 1.], [1., 1.], [0., 0.], [1., 0.]])
    {
        for v in p {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        for v in uv {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    for i in [0u16, 1, 2, 2, 1, 3] {
        b.extend_from_slice(&i.to_le_bytes());
    }
    for _ in 0..4 {
        for v in n {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b.extend_from_slice(&[255; 32]); // day/night vertex colors
    b.extend_from_slice(&[0; 48]);
    b.extend_from_slice(b"fixture\0");
    b.extend_from_slice(&[0; 48]);
    for v in if texture.is_empty() {
        [0f32; 5]
    } else {
        [0.8f32, 0.6, 0.4, 1., 96.]
    } {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.push(1); // material is two-sided
}

// A v5 BMD: one perforated blue facade and two off-axis opaque shadow casters.
// It goes through Objects::load_ids and both the native page and ray feeds.
fn bmd_fixture() -> Vec<u8> {
    let mut b = bmd_header([0.; 3], 3);
    bmd_quad(
        &mut b,
        [[10., 1., 0.], [14., 1., 0.], [10., 5., 0.], [14., 5., 0.]],
        [0., 0., 1.],
        "building/lighter_wall.tga",
    );
    for x in [2., -11.] {
        bmd_quad(
            &mut b,
            [
                [x, 0.5, -12.],
                [x, 0.5, -10.],
                [x, 3.5, -12.],
                [x, 3.5, -10.],
            ],
            [-1., 0., 0.],
            "",
        );
    }
    b
}

fn gfx_header() -> String {
    "MOXTVersion: 37\r\nDedaultScale: 1\r\nPlaySpeed: 1\r\nDefaultAlpha: 1\r\n\
             Raytrace: 0\r\nFadeByDist: 0\r\nFadeStart: 0.000000\r\nFadeEnd: 0.000000\r\n\
             Vec: 0.000000, 0.000000, 0.000000\r\nVec: 1.000000, 1.000000, 1.000000\r\n\
             ShakeCam: 0\r\nGFXELEMENTCOUNT: 1\r\n"
        .into()
}

const SPRITE: &str = "GFXELEMENTID: 100\r\nName: flame\r\nSrcBlend: 5\r\n\
        DestBlend: 2\r\nRepeatCount: 1\r\nRepeatDelay: 0\r\nTexFile: lighter-glow.png\r\n\
        BindEle: \r\nZEnable: 1\r\nMatchGrnd: 0\r\nTexRow: 1\r\nTexCol: 1\r\n\
        TexInterval: 50\r\nPriority: 2\r\nIsDummy: 0\r\nDummyEle: \r\nWarp: 0\r\n\
        Width: 2\r\nHeight: 2\r\nRotFromView: 0\r\nGrndNormOnly: 0\r\nStartTime: 0\r\n\
        KEYPOINTCOUNT: 1\r\nInterpolateMode: 0\r\nTimeSpan: -1\r\nPosition: 0, 0, 0\r\n\
        Color: -1\r\nScale: 1\r\nDirection: 0, 0, 0, 1\r\nRad_2D: 0\r\n\
        CtrlMethodCount: 0\r\n";

const LIGHT: &str = "GFXELEMENTID: 130\r\nName: torch\r\nSrcBlend: 5\r\n\
        DestBlend: 2\r\nRepeatCount: 1\r\nRepeatDelay: 0\r\nBindEle: \r\nDummyEle: \r\n\
        IsDummy: 0\r\nMatchGrnd: 0\r\nLightType: 1\r\nPriority: 4\r\nDiffuse: -1\r\n\
        Specular: 0\r\nAmbient: 0\r\nPosition: 0.000000, 0.000000, 0.000000\r\n\
        Direction: 0.000000, 0.000000, 0.000000\r\nRange: 9.000000\r\nFallOff: 0.000000\r\n\
        Attenuation0: 0.000000\r\nAttenuation0: 0.100000\r\nAttenuation0: 0.000000\r\n\
        Theta: 0.000000\r\nPhi: 0.000000\r\nStartTime: 0\r\nKEYPOINTCOUNT: 1\r\n\
        InterpolateMode: 0\r\nTimeSpan: -1\r\nPosition: 0, 0, 0\r\nColor: -1\r\nScale: 1\r\n\
        Direction: 0, 0, 0, 1\r\nRad_2D: 0\r\nCtrlMethodCount: 0\r\n";

fn placement(index: u32, path: &str, position: [f32; 3]) -> crate::scene::Effect {
    crate::scene::Effect {
        index,
        export_id: index + 1,
        scale: 1.,
        position,
        direction: [0., 0., 1.],
        up: [0., 1., 0.],
        speed: 1.,
        valid_time: 2,
        alpha: 1.,
        attenuation: false,
        path: path.into(),
    }
}

fn ornament_record() -> crate::scene::Ornament {
    crate::scene::Ornament {
        export_id: 123,
        offset: 0,
        position: [0.; 3],
        direction: [0., 0., 1.],
        up: [0., 1., 0.],
        extra: 0,
        path: "lighter.bmd".into(),
        map_id: "a01".into(),
    }
}

fn ornament(element: &std::path::Path) -> crate::objects::Objects {
    crate::objects::Objects::open(element, vec![ornament_record()]).unwrap()
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
    let tree = || crate::trees::TreeLod {
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
        scene_lights: std::sync::Arc::new(crate::scene_lights::SceneLights {
            lights: vec![crate::scene_lights::SceneLight {
                position: [4., 4., -11.],
                range: 10.,
                stored_attenuation: [1., 0., 0.],
                diffuse: [1.; 4],
                ambient: [0.; 4],
                bounds_center: [4., 4., -11.],
                bounds_half_extent: [10.; 3],
                bounds_min: [-6., -6., -21.],
                bounds_max: [14., 14., -1.],
                flags: 0,
            }],
        }),
        scene: std::sync::Arc::new(crate::scene::Scene {
            width: side,
            length: side,
            block_size: side,
            rows: 1,
            cols: 1,
            tree_types: vec![],
            grass_types: vec![],
            blocks: vec![crate::scene::Block {
                row: 0,
                col: 0,
                trees: vec![],
                water: vec![],
                grass: vec![],
                ornaments: vec![crate::scene::Reference {
                    export_id: 123,
                    offset: 0,
                }],
                boxes: vec![],
                effects: vec![
                    placement(0, "scene/lighter-sprite.gfx", [-8., 3., 1.]),
                    placement(1, "scene/lighter-light.gfx", [-9., 4., -11.]),
                ],
                models: vec![],
                critters: vec![],
                beziers: vec![],
                sounds: vec![],
            }],
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
                lods: (0..3).map(|_| tree()).collect(),
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
        layers: vec![],
    };
    let exterior = super::uncovered_far(&far, &super::Mesh::default(), [-4., 0., 0., 4.], 7);
    assert_eq!(exterior.triangles, [[1, 2, 3]]);
    assert_eq!(exterior.surfaces, [7]);
    assert!(
        super::uncovered_far(&far, &super::Mesh::default(), [-4., 4., 0., 4.], 7)
            .triangles
            .is_empty()
    );
}

fn seam_fixture(turns: usize) -> (super::Mesh, super::Mesh, [f32; 4]) {
    // A four-cell perimeter with varying elevations between the stride-16
    // lattice points. The vertex at z=6 is not used by the near mesh.
    let vertex = |[mut x, y, mut z]: [f32; 3]| {
        for _ in 0..turns {
            (x, z) = (-z, x);
        }
        super::Vertex::new([x, y, z], [0., 1., 0.], [0.; 2])
    };
    let near = super::Mesh {
        vertices: [
            [0., 0., 0.],
            [0., 5., 4.],
            [0., -3., 8.],
            [0., 7., 12.],
            [0., 0., 16.],
            [-16., 0., 8.],
            [0., 199., 6.],
        ]
        .into_iter()
        .map(vertex)
        .collect(),
        triangles: (0..4).map(|n| [n + 1, n, 5]).collect(),
        surfaces: vec![0; 4],
        layers: vec![],
    };
    let far = super::Mesh {
        vertices: [[0., 0., 0.], [0., 0., 16.], [16., 0., 0.], [16., 0., 16.]]
            .into_iter()
            .map(vertex)
            .collect(),
        triangles: vec![[1, 3, 0], [3, 2, 0]],
        surfaces: vec![0; 2],
        layers: vec![],
    };
    let bounds = near.vertices.iter().fold(
        [
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ],
        |[xmin, xmax, zmin, zmax], v| {
            [
                xmin.min(v.pos[0]),
                xmax.max(v.pos[0]),
                zmin.min(v.pos[2]),
                zmax.max(v.pos[2]),
            ]
        },
    );
    (near, far, bounds)
}

#[test]
fn coarse_far_terrain_matches_the_nonflat_perimeter_in_all_directions() {
    for turns in 0..4 {
        let (near, far, bounds) = seam_fixture(turns);
        let patched = super::uncovered_far(&far, &near, bounds, 7);
        assert_eq!(patched.triangles.len(), 5);
        assert_eq!(patched.vertices.len(), 7); // no unreferenced vertex copied
        assert!(patched.surfaces.iter().all(|&id| id == 7));
        for pair in near.vertices[..5].windows(2) {
            let [a, b] = [pair[0].pos, pair[1].pos];
            let mut uses = 0;
            for tri in &patched.triangles {
                let p = tri.map(|i| patched.vertices[i as usize].pos);
                uses += (0..3).filter(|&i| p[i] == a && p[(i + 1) % 3] == b).count();
            }
            assert_eq!(
                uses, 1,
                "far seam does not share the exact near edge {a:?} → {b:?}"
            );
        }
        // The fan changes height only, never the coverage of the coarse cell
        // on the ground plane or the winding of an exterior triangle.
        let twice_area = patched
            .triangles
            .iter()
            .map(|tri| {
                let [a, b, c] = tri.map(|i| Vec3::from_array(patched.vertices[i as usize].pos));
                let y = (b - a).cross(c - a).y;
                assert!(y > 0.);
                y
            })
            .sum::<f32>();
        assert_eq!(twice_area, 2. * 16. * 16.);
    }
}

pub(crate) fn stitched_terrain_scene() -> super::Packet {
    let (mut near, coarse, bounds) = seam_fixture(0);
    let mut builder = super::Builder::default();
    let material = builder.surface(super::Surface::new(0, 0, 0., 0.));
    let mut far = super::uncovered_far(&coarse, &near, bounds, material);
    near.surfaces.fill(material);
    // Different colors let a grazing ray prove which side it hit first.
    near.vertices
        .iter_mut()
        .for_each(|v| *v = v.tint(0xff0000ff, 0xff0000ff));
    far.vertices
        .iter_mut()
        .for_each(|v| *v = v.tint(0xff00ff00, 0xff00ff00));
    builder.world_mesh(&near).unwrap();
    builder.world_mesh(&far).unwrap();
    builder.finish()
}

#[test]
#[ignore = "requires a Vulkan adapter (lavapipe is sufficient)"]
fn lighter_streamed_world_pixels() {
    // Use the real viewer upload/build and replacement dispatch, not a Packet
    // assembled in the test. Both main-world routes run with the same camera
    // and sources, with assertions on actual color, reverse-Z and dispatch.
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
    camera.logic_delta_ms = 16;
    let detail = DetailSettings::default();
    let mut residency = Default::default();
    let mut ray_scenery = super::SceneryStream::default();
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
            Some(&mut ray_scenery),
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
    let mut capture = readback::Ring::default();
    let mut render_center = |r: &mut Renderer, c: &Camera| -> ([u8; 4], f32) {
        let rays = r.lighter.replaces_world();
        let native_before = r.native_opaque_draws.get();
        r.render(
            &view,
            &raw,
            c.matrix(SIZE, SIZE, c.far),
            c,
            SIZE,
            SIZE,
            true,
        );
        if rays {
            assert_eq!(
                r.native_opaque_draws.get(),
                native_before,
                "raster world submitted under Lighter"
            );
            assert_eq!(r.scene_passes, 0);
        } else {
            assert!(
                r.native_opaque_draws.get() > native_before,
                "Off didn't submit the native world"
            );
        }
        let bytes = read_target_pixels(r, &mut capture, &target).unwrap();
        let offset = (4 * (SIZE / 2 * SIZE + SIZE / 2)) as usize;
        let depth = r.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter host depth readback"),
            size: u64::from(SIZE * SIZE * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = r.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &r.depth,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::ImageCopyBuffer {
                buffer: &depth,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
        r.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        depth
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        super::super::wait_for_capture_callback(&r.device, rx)
            .unwrap()
            .unwrap();
        let z = f32::from_le_bytes(
            depth.slice(..).get_mapped_range()[offset..offset + 4]
                .try_into()
                .unwrap(),
        );
        (bytes[offset..offset + 4].try_into().unwrap(), z)
    };
    let mut at_center = |r: &mut Renderer, c: &Camera| {
        let pixel = render_center(r, c);
        assert_eq!(r.lighter.error(), None);
        pixel
    };
    let wet = at_center(&mut r, &camera);
    assert!(
        wet.0[2] > wet.0[1] && wet.0[1] > 0,
        "water did not cover wet cell: {wet:?}"
    );
    camera.pos.x = 2.5;
    let dry = at_center(&mut r, &camera);
    assert_eq!(
        dry.0,
        [0, 255, 0, 255],
        "water covered an authored dry cell"
    );
    assert!(
        wet.1 > dry.1 && dry.1 > 0.,
        "water didn't write closer reverse-Z: {wet:?} {dry:?}"
    );
    camera.pos = Vec3::new(8., 3., 7.);
    camera.pitch = 0.;
    let tree = at_center(&mut r, &camera);
    assert_eq!(
        tree.0,
        [255, 0, 0, 255],
        "tree geometry missing from the ray scene"
    );
    assert!(tree.1 > 0.);

    camera.pos.x = 0.;
    camera.pitch = 0.6;
    let sky = at_center(&mut r, &camera);
    assert!(
        sky.0.into_iter().zip(SKY).all(|(a, b)| a.abs_diff(b) <= 1),
        "authored sky not sampled: {sky:?}"
    );
    assert_eq!(sky.1, 0.);

    // The first local window had no ornaments. Load the authored BMD now,
    // using the actual stream build, and evict the obsolete water cells.
    terrain.water.clear();
    terrain.objects = ornament(&root.0);
    terrain.scenery_placements = None;
    let streamed = mesh(&mut terrain, r.grass_atlas_key);
    assert!(
        !streamed.local_pages.is_empty(),
        "fixture BMD did not reach the local page builder"
    );
    assert!(r.update_mesh(&mut Some(streamed)).unwrap());
    let packet = &r.lighter.world.as_ref().unwrap().packet.words;
    assert!(
        packet[packet[3] as usize..packet[4] as usize]
            .chunks_exact(5)
            .any(|m| m[1] & super::BAKED != 0
                && f32::from_bits(m[4]) == 96.
                && (f32::from_bits(m[3]) - 0.15).abs() < 1.0e-6),
        "BMD specular material lost in streaming"
    );
    camera.pos = Vec3::new(-2.5, 5., 0.);
    camera.pitch = -std::f32::consts::FRAC_PI_4;
    assert_eq!(
        at_center(&mut r, &camera).0,
        dry.0,
        "stale water after stream replacement"
    );

    camera.pitch = 0.;
    let mut solid: Option<f32> = None;
    for mode in [Mode::Off, Mode::Software] {
        r.lighter.settings.mode = mode;
        camera.pos = Vec3::new(13., 3., 7.);
        let wall = at_center(&mut r, &camera);
        assert!(
            wall.0[2] > 220 && wall.0[0] < 5 && wall.0[1] < 5 && wall.1 > 0.,
            "{mode:?}: missing textured BMD facade {wall:?}"
        );
        if let Some(z) = solid {
            assert!((wall.1 - z).abs() < 1e-5);
        }
        solid = Some(wall.1);
        camera.pos.x = 11.;
        let hole = at_center(&mut r, &camera);
        assert_eq!(
            hole.1, 0.,
            "{mode:?}: alpha hole in BMD wrote depth: {hole:?}"
        );

        camera.pos = Vec3::new(-8., 3., 6.);
        camera.world_effects = false;
        let hidden = at_center(&mut r, &camera);
        camera.world_effects = true;
        let _ = at_center(&mut r, &camera); // one owner can restart per tick
        let glow = at_center(&mut r, &camera);
        assert!(
            glow.0[0] > hidden.0[0] + 70 && glow.0[2].abs_diff(hidden.0[2]) < 4,
            "{mode:?}: authored texture/opacity was not composited: {glow:?} {hidden:?}"
        );
        assert_eq!(glow.1, hidden.1, "{mode:?}: read-only sprite wrote depth");
        assert_eq!(
            at_center(&mut r, &camera),
            glow,
            "{mode:?}: static sprite flickered"
        );
        camera.pos.x += 0.004;
        let nudge = at_center(&mut r, &camera);
        assert!(
            nudge
                .0
                .into_iter()
                .zip(glow.0)
                .all(|(a, b)| a.abs_diff(b) < 3),
            "{mode:?}: sprite popped on small motion"
        );
    }

    // Sample terrain at night: no sun light can mask a point-light shadow.
    // The occluders are lateral to the view, on the rays to the two lamps.
    camera.time_of_day = 0.;
    camera.world_effects = false; // light production remains separate from sheets
    camera.pitch = -std::f32::consts::FRAC_PI_4;
    r.lighter.settings.lighting = true;
    let mut probe = |r: &mut Renderer, c: &mut Camera, x: f32| {
        c.pos = Vec3::new(x, 5., -6.);
        at_center(r, c)
    };
    let points = [0., -13.];
    r.lighter.settings.shadows = false;
    let bright = points.map(|x| probe(&mut r, &mut camera, x));
    r.lighter.settings.shadows = true;
    let dark = points.map(|x| probe(&mut r, &mut camera, x));
    for ((lit, shadowed), name) in bright.into_iter().zip(dark).zip(["static", "GFX runtime"]) {
        assert!(
            lit.0[1] > shadowed.0[1] + 20 && lit.1 == shadowed.1,
            "{name} point light or its ray shadow missing: {lit:?} {shadowed:?}"
        );
    }
    r.models
        .as_mut()
        .expect("world effect producer")
        .set_light_priority_limit(0);
    r.lighter.settings.shadows = false;
    let disabled = probe(&mut r, &mut camera, points[1]);
    assert_eq!(disabled, dark[1], "GFX light publication was not withdrawn");
    r.models
        .as_mut()
        .unwrap()
        .set_light_priority_limit(i32::MAX);
    r.lighter.settings.shadows = true;

    terrain.objects = crate::objects::Objects::open(&root.0, Vec::new()).unwrap();
    terrain.scenery_placements = None;
    let evicted = mesh(&mut terrain, r.grass_atlas_key);
    assert!(r.update_mesh(&mut Some(evicted)).unwrap());
    for (x, lit) in points.into_iter().zip(bright) {
        assert_eq!(
            probe(&mut r, &mut camera, x),
            lit,
            "stale shadow after BMD eviction"
        );
    }
    if cpu_adapter {
        r.lighter.settings.mode = Mode::Hardware;
        let refused = render_center(&mut r, &camera);
        assert!(r.lighter.error().is_some());
        assert_eq!(
            refused,
            ([0, 0, 0, 255], 0.),
            "unsupported mode silently fell back"
        );
    }
}

#[test]
#[ignore = "requires Vulkan for an actual headless world PNG"]
fn lighter_headless_preview_png() {
    let root = FixtureRoot::new();
    let path = root.0.join("headless.png");
    super::super::preview(
        fixture(&root.0),
        &path,
        Some([-2.5, 5., 0., 0., -std::f32::consts::FRAC_PI_4]),
    )
    .unwrap();
    assert_world_png(&path, (960, 720));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an X11 display (e.g. xvfb-run) and Vulkan"]
fn lighter_live_viewer_frames() {
    let root = FixtureRoot::new();
    super::super::run_viewer(fixture(&root.0), Some(&root.0)).unwrap();
    assert_world_png(&root.0.join("0.png"), (160, 96));
    assert_world_png(&root.0.join("1.png"), (192, 112));
}

fn assert_world_png(path: &std::path::Path, extent: (u32, u32)) {
    let image = image::open(path).unwrap().into_rgba8();
    assert_eq!(image.dimensions(), extent);
    let first = image.get_pixel(0, 0);
    assert!(
        image.pixels().any(|p| p != first && p.0[3] == 255),
        "{} did not contain a rendered world",
        path.display()
    );
}

type Scene = (super::Packet, super::Textures);

pub(crate) struct StreamedBmdScenes {
    pub terrain: [f32; 4],
    pub area_table: u32,
    pub extended: Scene,
    pub local: Scene,
    pub evicted: Scene,
    pub restored: Scene,
    pub transmit_only: Scene,
    pub reflect_only: Scene,
}

/// Real asset resolver, world loader and ray feed; shared by the software,
/// Vulkan CPU interop and opt-in physical hardware pixel suites.
pub(crate) fn streamed_bmd_scenes(storage: usize) -> StreamedBmdScenes {
    let root = FixtureRoot::new();
    let mut t = fixture(&root.0);
    t.grid_spacing = 16.; // one 512-unit block, flat ground under both paths
    let scene = std::sync::Arc::get_mut(&mut t.scene).unwrap();
    scene.width = TILE as f32 * t.grid_spacing;
    scene.length = scene.width;
    scene.block_size = scene.width;
    t.water[0].center = [152., 10., 5.];
    t.water[0].height = 10.;
    t.water[0].columns = 1;
    t.water[0].mask.truncate(1);
    let source = |extra| {
        crate::objects::Objects::open(
            &root.0,
            vec![crate::scene::Ornament {
                position: [160., 0., 0.],
                path: "distant.bmd".into(),
                extra,
                ..ornament_record()
            }],
        )
        .unwrap()
    };
    t.objects = source(0x0101);
    let detail = DetailSettings {
        scenery_quality: 2,
        ..Default::default()
    };
    assert!(crate::scenery_visibility::ornament_distance(detail.scenery_quality) < 160.);
    let mut stream = super::SceneryStream::default();
    let mut residency = Default::default();
    let mut atlas = 0;
    let mut make = |t: &mut Terrain, x: f32| {
        let mesh = build(
            t,
            x,
            0.,
            detail,
            &mut residency,
            false,
            atlas,
            false,
            Some(&mut stream),
        )
        .unwrap();
        atlas = mesh.grass_atlas_key;
        mesh
    };
    let first = make(&mut t, 0.);
    assert!(
        first.local_pages.is_empty(),
        "native renderer must not load this distant BMD"
    );
    let mut feed = super::Feed::new(&t, &first, false).unwrap();
    let install = |feed: &mut super::Feed,
                   mesh: &super::MeshData,
                   (local, extra, outside): (usize, usize, usize)| {
        let (packet, textures, coverage) = feed.snapshot(mesh, storage).unwrap();
        assert_eq!(
            (coverage.local, coverage.extra, coverage.outside),
            (local, extra, outside),
            "{coverage}"
        );
        assert_eq!(
            (
                coverage.missing_textures,
                coverage.deferred,
                coverage.unavailable
            ),
            (0, 0, 0),
            "{coverage}"
        );
        (packet, textures)
    };
    let extended = install(&mut feed, &first, (0, 1, 0));
    // Required terrain and vegetation can already occupy the binding. The
    // optional asset must be deferred, not leave a partially packed mesh.
    let (limited, _, stats) = feed.snapshot(&first, extended.0.bytes().len() - 1).unwrap();
    assert_eq!((stats.extra, stats.deferred), (0, 1));
    assert_eq!(extended.0.triangles(), limited.triangles() + 2);
    let local_mesh = make(&mut t, 160.);
    assert!(!local_mesh.local_pages.is_empty());
    let local = install(&mut feed, &local_mesh, (1, 0, 0));
    let evicted = install(&mut feed, &make(&mut t, -1200.), (0, 0, 1));
    let restored = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    t.objects = source(0x0100);
    let transmit_only = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    t.objects = source(0x0001);
    let reflect_only = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    StreamedBmdScenes {
        terrain: feed.terrain,
        area_table: feed.area_table,
        extended,
        local,
        evicted,
        restored,
        transmit_only,
        reflect_only,
    }
}

#[test]
fn ray_stream_anchor_crosses_blocks_even_when_terrain_lods_are_clamped() {
    let terrain = |x| super::super::lod_levels(TILE, TILE, 1., [x, 0.], 7);
    let anchor = |x| super::super::camera_block(TILE, TILE, 1., [x, 0.]);
    assert_eq!(terrain(-900.), terrain(-1000.));
    assert_ne!(anchor(-900.), anchor(-1000.));
    assert_eq!(anchor(-900.), anchor(-901.));
}
