use super::super::{
    adapter, build, read_target_pixels, readback, Camera, DetailSettings, Renderer, TILE,
};
use crate::{
    modules::lighter::{Lighter, Mode, Settings},
    Terrain,
};
use glam::Vec3;

const SKY: [u8; 4] = [25, 75, 175, 255];

pub(crate) fn ray_test_wet_grid(
    bank: &mut super::Textures,
    areas: &[crate::water::WaterArea],
    eye: Vec3,
) -> [u32; 4] {
    let water = super::water_feed::Water::pack(areas, bank).unwrap();
    let [table, count] = water.table();
    [0, water.at(eye, None), table, count]
}

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
        let normals = encode(
            &image::RgbaImage::from_pixel(1, 1, image::Rgba([128, 128, 255, 255])),
            image::ImageFormat::Tga,
        );
        crate::pck::write_test_archive(
            &root.0.join("shaders.pck"),
            &[("shaders/textures/nrf/lighter_wall.tga", &normals)],
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

fn install_streamed_fixture(r: &mut super::super::Renderer, native: super::MeshData) {
    let mut packer = r.lighter_feed.as_mut().and_then(super::Feed::take_packer);
    let storage = super::lighter::storage_limit(&r.device);
    let mode = r.lighter.settings.mode;
    let mesh = std::thread::scope(|scope| {
        scope
            .spawn(|| super::super::StreamedMesh::prepare(native, packer.as_mut(), storage, mode))
            .join()
            .unwrap()
    });
    if let Some(feed) = &mut r.lighter_feed {
        feed.packer = packer;
    }
    let mut pending = Some(mesh);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !r.update_mesh(&mut pending).unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "staged scene did not complete"
        );
        r.device.poll(wgpu::Maintain::Poll);
        std::thread::yield_now();
    }
    assert!(pending.is_none());
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
fn terrain_masks_fit_the_budget_and_never_reuse_a_failed_upload() {
    use super::{FeedSource, StreamPacker, Textures};
    let mut material = crate::material::cutoff_mask_fixture();
    material.areas.truncate(8);
    let mut source = FeedSource {
        base: Textures::with_limit(4 << 10),
        terrain_ids: vec![0],
        ..FeedSource::default()
    };
    source.base.seal();
    let mut packer = StreamPacker::new(std::sync::Arc::new(source));
    packer.update_masks(&material);
    let masks = packer.masks.as_ref().unwrap().as_ref().unwrap();
    let bank = &masks.bank;
    assert!(bank.remaining_bytes() >= 1024);
    let words: Vec<u32> = bank
        .byte_segments()
        .flat_map(bytemuck::cast_slice::<u8, u32>)
        .copied()
        .collect();
    for area in 0..8 {
        let row = masks.area_table as usize + area * 4;
        assert_eq!(words[row + 1], 2, "do not drop any layers");
        let layer = words[row] as usize;
        let mask = words[layer + 6] as usize;
        assert_eq!(words[mask], 8, "lower the resolution, not the coverage");
        let top = mask + words[mask + 4] as usize;
        assert_eq!(words[top].to_le_bytes(), [area as u8; 4]);
    }
    let source = std::sync::Arc::get_mut(&mut packer.source).unwrap();
    source.base = Textures::with_limit(256);
    source.base.seal();
    packer.update_masks(&material);
    assert!(
        packer.masks.unwrap().is_err(),
        "never use obsolete streamed masks"
    );
}

#[test]
fn ray_lightmaps_keep_day_only_and_distinct_night_bindings() {
    use crate::modules::lighter::textures::Textures;
    let root = FixtureRoot::new();
    let t = fixture(&root.0);
    let material = crate::material::single_layer_fixture(TILE);
    let write = |ext: &str, color: u16| {
        let mut dds = vec![0; 128];
        dds[..4].copy_from_slice(b"DDS ");
        for (o, v) in [
            (4, 124u32),
            (8, 0x81007),
            (12, 256),
            (16, 256),
            (20, 32768),
            (76, 32),
            (80, 4),
            (108, 0x1000),
        ] {
            dds[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        dds[84..88].copy_from_slice(b"DXT1");
        let [lo, hi] = color.to_le_bytes();
        dds.extend([lo, hi, 0, 0, 0, 0, 0, 0].repeat(4096));
        let mut lm = vec![0; 88];
        for (o, v) in [0x54324c4d, 2u32, 16, 256, 1, dds.len() as u32]
            .into_iter()
            .enumerate()
        {
            lm[o * 4..o * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        for i in 0..16 {
            let o = 24 + 4 * i;
            lm[o..o + 4].copy_from_slice(&((88 + (15 - i) * dds.len()) as u32).to_le_bytes());
            lm.extend_from_slice(&dds);
        }
        std::fs::write(root.0.join(format!("fixture.{ext}")), lm).unwrap();
    };
    let mut bank = Textures::default();
    assert!(super::lightmaps::load(&t, &material, &mut bank)
        .unwrap()
        .is_empty());
    std::fs::write(root.0.join("fixture.t2lm"), [0u8; 128]).unwrap();
    let size = bank.byte_len();
    assert_eq!(
        super::lightmaps::load(&t, &material, &mut bank).unwrap(),
        vec![[0; 2]; 16]
    );
    assert_eq!(
        bank.byte_len(),
        size,
        "an unsupported optional record must not poison the bank"
    );
    write("t2lm", 0xf800);
    let offsets = super::lightmaps::load(&t, &material, &mut bank).unwrap();
    assert_eq!(offsets.len(), 16);
    assert!(offsets.iter().all(|&[d, n]| d != 0 && d == n));
    write("t2lm1", 0x001f);
    let mut bank = Textures::default();
    let offsets = super::lightmaps::load(&t, &material, &mut bank).unwrap();
    assert!(offsets.iter().all(|&[d, n]| d != 0 && n > d));
    let words: Vec<u32> = bank
        .byte_segments()
        .flat_map(bytemuck::cast_slice::<u8, u32>)
        .copied()
        .collect();
    for (addr, rgba) in [
        (offsets[0][0], [255, 0, 0, 255]),
        (offsets[0][1], [0, 0, 255, 255]),
    ] {
        let i = addr as usize;
        assert_eq!(&words[i..i + 2], &[64, 64]);
        assert_eq!(words[i + words[i + 4] as usize].to_le_bytes(), rgba);
    }
}

#[test]
#[ignore = "requires Vulkan and the two checksum-pinned a07 lightmap records; synthetic geometry"]
fn lighter_retail_t2lm_record_fixture() {
    use sha2::{Digest, Sha256};
    let root = FixtureRoot::new();
    let source = std::path::PathBuf::from(
        std::env::var_os("PW_TEST_A07_LIGHTMAP_DIR").expect("set PW_TEST_A07_LIGHTMAP_DIR"),
    );
    for (ext, expected) in [
        (
            "t2lm",
            "e7b1830026d15fc58775c510ec23cb6d82113de99423bc26a7b395d7ba2ad009",
        ),
        (
            "t2lm1",
            "6f92f831a0c7830aea97240ecd1a4f019b8fbecd34051f3306894b225c8e23de",
        ),
    ] {
        let bytes = std::fs::read(source.join(format!("a07_1.{ext}"))).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
        std::fs::write(root.0.join(format!("fixture.{ext}")), bytes).unwrap();
    }
    let out = std::env::var_os("PW_SAVE_LIGHTER_T2LM")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.0.clone());
    std::fs::create_dir_all(&out).unwrap();
    for (mode, label) in [(Mode::Off, "native"), (Mode::Software, "software")] {
        let path = out.join(format!("a07-record-fixture-{label}.png"));
        super::super::preview_frame(
            fixture(&root.0),
            &path,
            Some([-2.5, 5., 0., 0., -std::f32::consts::FRAC_PI_4]),
            mode,
            Some(2),
        )
        .unwrap();
        assert_world_png(&path, (960, 720));
        let sidecar = std::fs::read_to_string(path.with_extension("coverage.txt")).unwrap();
        assert!(sidecar.contains("lightmaps=true\n"));
        assert!(sidecar.contains("native_lightmap_areas_day_night=16\n"));
        assert!(sidecar.contains("native_lightmap_areas_unbound=0\n"));
    }
}

#[test]
fn ray_grass_tiles_reuse_authored_rectangles_without_native_atlas_pixels() {
    use crate::modules::lighter::textures::Textures;
    use std::sync::Arc;
    let path = "grass.dds";
    let mut grass = crate::grass::Grass {
        areas: vec![],
        types: vec![],
        models: vec![],
        textures: Default::default(),
        texture_mips: Default::default(),
    };
    grass.textures.insert(
        path.into(),
        image::RgbaImage::from_pixel(8, 2, image::Rgba([9, 77, 133, 255])),
    );
    grass.texture_mips.insert(
        path.into(),
        vec![image::RgbaImage::from_pixel(
            4,
            1,
            image::Rgba([5, 30, 60, 128]),
        )],
    );
    let mut stream = super::SceneryStream::default();
    // A change of renderer slot never replaces the source mip chain; without
    // a slot change the native build may omit its atlas pixels altogether.
    let slot = |x| crate::texture_atlas::Slot {
        x,
        y: 0,
        w: 128,
        h: 128,
    };
    let first = super::grass_tiles(&mut stream, &grass, &[(slot(0), path, false)]).unwrap();
    let next = super::grass_tiles(&mut stream, &grass, &[(slot(128), path, false)]).unwrap();
    assert!(Arc::ptr_eq(&first[&[0, 0]], &next[&[128, 0]]));
    let mut bank = Textures::default();
    bank.insert(&next[&[128, 0]]).unwrap();
    assert_eq!(bank.mips.authored_levels, 2);
    assert_eq!(bank.mips.fallback, 0);
    assert_eq!(bank.byte_len(), (6 + 6 + 16 + 4) * 4);
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
    assert_eq!(exterior.triangles, [[0, 1, 2]]);
    assert_eq!(
        exterior.vertices.iter().map(|v| v.pos).collect::<Vec<_>>(),
        points[1..]
    );
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

#[test]
fn fine_far_grid_snaps_to_a_coarser_native_edge() {
    let mesh = |points: &[[f32; 3]], triangles: Vec<[u32; 3]>| super::Mesh {
        vertices: points
            .iter()
            .map(|&p| super::Vertex::new(p, [0., 1., 0.], [0.; 2]))
            .collect(),
        triangles,
        ..Default::default()
    };
    let near = mesh(
        &[[0., 1., 0.], [0., 5., 4.], [-4., 0., 2.]],
        vec![[0, 1, 2]],
    );
    let far = mesh(
        &[[0., 1., 0.], [0., 99., 2.], [0., 5., 4.], [4., 0., 2.]],
        vec![[0, 1, 3], [1, 2, 3]],
    );
    let stitched = super::uncovered_far(&far, &near, [-4., 0., 0., 4.], 0);
    assert_eq!(stitched.triangles.len(), 2);
    assert_eq!(
        stitched
            .vertices
            .iter()
            .find(|v| v.pos[0] == 0. && v.pos[2] == 2.)
            .unwrap()
            .pos,
        [0., 3., 2.]
    );
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

pub(crate) fn fine_far_terrain_scene() -> super::Packet {
    let make = |points: &[[f32; 3]], triangles: &[[u32; 3]], tint| super::Mesh {
        vertices: points
            .iter()
            .map(|&p| super::Vertex::new(p, [0., 1., 0.], [0.; 2]).tint(tint, tint))
            .collect(),
        triangles: triangles.to_vec(),
        surfaces: vec![0; triangles.len()],
        layers: Vec::new(),
    };
    let near = make(
        &[[-4., 1., 0.], [0., 1., 0.], [-4., 1., 4.], [0., 1., 4.]],
        &[[0, 2, 1], [1, 2, 3]],
        0xff0000ff,
    );
    let fine = make(
        &[
            [0., 1., 0.],
            [4., 1., 0.],
            [0., 99., 2.],
            [4., 1., 2.],
            [0., 1., 4.],
            [4., 1., 4.],
        ],
        &[[0, 2, 1], [1, 2, 3], [2, 4, 3], [3, 4, 5]],
        0xffff0000,
    );
    let mut far = super::uncovered_far(&fine, &near, [-4., 0., 0., 4.], 0);
    // Same edge positions; keep the two halves visibly identifiable.
    for v in &mut far.vertices {
        *v = v.tint(0xffff0000, 0xffff0000);
    }
    let mut builder = super::Builder::default();
    builder.surface(super::Surface::new(0, 0, 0., 0.));
    builder
        .chunk(super::ChunkKey::NearTerrain(0, 0), &near)
        .unwrap();
    builder
        .chunk(super::ChunkKey::FarTerrain(1, 0), &far)
        .unwrap();
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
        let rays = r.renders_rays();
        let native_before = r.native_opaque_draws.get();
        assert!(r.render(
            &view,
            &raw,
            c.matrix(SIZE, SIZE, c.far),
            c,
            SIZE,
            SIZE,
            true,
        ));
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
                "Native route did not submit the raster world"
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
    // Fixed clock, moving eye and a much darker region record. The
    // camera can change fog/weather without changing world radiance.
    let mut sky_camera = Camera::new(&terrain);
    sky_camera.pos = Vec3::new(0., 20., 0.);
    sky_camera.yaw = 0.;
    sky_camera.pitch = 0.65;
    sky_camera.far = 40.;
    sky_camera.time_of_day = 12.;
    sky_camera.celestial_discs = false;
    sky_camera.world_effects = false;
    sky_camera.bloom = false;
    sky_camera.logic_delta_ms = 0;
    let region = |v, toward_sun| crate::environment::RegionalLight {
        ambient: [v; 3],
        diffuse: [v; 3],
        specular: [v; 3],
        toward_sun,
        celestial_flags: 0x18,
    };
    sky_camera.regional_light = Some(region(0.9, [0., 1., 0.]));
    let same_hour = |r: &mut Renderer, c: &Camera| {
        let snap = r.begin_frame(c, SIZE, SIZE);
        r.lighter_feed.as_ref().unwrap().frame(
            c.matrix(SIZE, SIZE, c.far),
            c,
            &snap,
            SIZE,
            SIZE,
            false,
        )
    };
    let before = same_hour(&mut r, &sky_camera);
    assert!(
        (Vec3::from_slice(&before.sun) - Vec3::new(0., 1., 0.46).normalize()).length() < 1e-6,
        "noon light used disc fade/elevation instead of a direction"
    );
    let bright_sky = at_center(&mut r, &sky_camera);
    assert_eq!(bright_sky.1, 0.);
    sky_camera.pos.x += 3.;
    sky_camera.regional_light = Some(region(0.07, [1., 0., 0.]));
    let after = same_hour(&mut r, &sky_camera);
    assert_eq!(
        (
            before.sun,
            before.sun_color,
            before.ambient,
            before.sky_tint
        ),
        (after.sun, after.sun_color, after.ambient, after.sky_tint)
    );
    assert_eq!(at_center(&mut r, &sky_camera), bright_sky);
    // The override explicitly retains the legacy local palette, not the bug.
    r.lighter.settings.regional_lighting = true;
    let local_sky = at_center(&mut r, &sky_camera);
    assert!(local_sky.0[2] + 70 < bright_sky.0[2]);
    r.lighter.settings.regional_lighting = false;
    sky_camera.time_of_day = 0.;
    let night = same_hour(&mut r, &sky_camera);
    assert_eq!(night.sun[3], 0.);
    assert!((night.sky_tint[0] + 0.5) < before.sky_tint[0]);
    let night_sky = at_center(&mut r, &sky_camera);
    assert!(night_sky.0[2] + 60 < bright_sky.0[2]);
    // Return sky state to the day before the material-specific probes.
    for _ in 0..2 {
        let _ = same_hour(&mut r, &camera);
    }

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
    install_streamed_fixture(&mut r, streamed);
    let packet = &r.lighter.world.as_ref().unwrap().packet.words;
    assert!(
        packet[packet[3] as usize..packet[4] as usize]
            .chunks_exact(super::SURFACE_WORDS)
            .any(|m| m[1] & super::BAKED != 0
                && m[6] != 0 // optional NRF came through the archive-mounted material feed
                && f32::from_bits(m[4]) == 96.
                && (f32::from_bits(m[3]) - 0.20).abs() < 1.0e-6
                && m[5].to_le_bytes() == [255, 191, 128, 255]),
        "BMD specular color/power or NRF binding lost in streaming"
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
    install_streamed_fixture(&mut r, evicted);
    for (x, lit) in points.into_iter().zip(bright) {
        let unoccluded = probe(&mut r, &mut camera, x);
        // The no-shadow baseline samples the emitter center. Once the caster
        // is gone, a finite sample can still differ by a few encoded levels
        // due to its incidence angle; it cannot retain the previous umbra.
        assert_eq!(unoccluded.1, lit.1);
        assert!(
            unoccluded
                .0
                .into_iter()
                .zip(lit.0)
                .all(|(a, b)| a.abs_diff(b) <= 3),
            "stale shadow after BMD eviction: {unoccluded:?} vs {lit:?}"
        );
    }
    // Freeze the clock for a pixel/depth comparison with the exact Native
    // frame. A failed scene should use it even with Software still selected.
    camera.logic_delta_ms = 0;
    camera.pos = Vec3::new(-2.5, 5., 0.);
    camera.time_of_day = 12.;
    let selection = r.lighter.settings;
    r.lighter.settings.mode = Mode::Off;
    let native = at_center(&mut r, &camera);
    assert!(native.0[1] > 0 && native.1 > 0.);
    r.lighter.settings = selection;
    // Inject an actual bank admission error, without requiring a giant map.
    let error = super::Textures::with_limit(24).reserve(1).unwrap_err();
    r.lighter.install(Err(error.clone()));
    assert_eq!(r.lighter.error(), Some(error.as_str()));
    assert_eq!(render_center(&mut r, &camera), native);
    assert_eq!(r.lighter.settings, selection, "mode must not be rewritten");
    assert!(!r.renders_rays());
    assert!(r.lighter.world.is_none());

    let rebuilt = mesh(&mut terrain, r.grass_atlas_key);
    install_streamed_fixture(&mut r, rebuilt);
    assert!(r.renders_rays(), "a healthy stream must restore Software");
    assert!(render_center(&mut r, &camera).1 > 0.);
    assert!(r.lighter.error().is_none());

    // An error discovered inside the draw is different from one found during
    // stream installation: this attempt publishes nothing, and the *next*
    // redraw uses Native. Do not submit a clear or tick the raster world here.
    let feed = r.lighter_feed.take().unwrap();
    let calls = r.native_opaque_draws.get();
    let mut verify = readback::Ring::default();
    let last = read_target_pixels(&r, &mut verify, &target).unwrap();
    assert!(!r.render(
        &view,
        &raw,
        camera.matrix(SIZE, SIZE, camera.far),
        &camera,
        SIZE,
        SIZE,
        true,
    ));
    assert_eq!(r.native_opaque_draws.get(), calls);
    assert_eq!(read_target_pixels(&r, &mut verify, &target).unwrap(), last);
    assert_eq!(r.lighter.error(), Some("Lighter scene feed is unavailable"));
    r.lighter_feed = Some(feed);
    assert_eq!(render_center(&mut r, &camera), native);

    let rebuilt = mesh(&mut terrain, r.grass_atlas_key);
    install_streamed_fixture(&mut r, rebuilt);
    assert!(r.renders_rays());
    if cpu_adapter {
        r.lighter.settings.mode = Mode::Hardware;
        assert!(r.lighter.error().unwrap().contains("ray-query support"));
        assert_eq!(render_center(&mut r, &camera), native);
        assert_eq!(r.lighter.settings.mode, Mode::Hardware);
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

#[test]
#[ignore = "requires Vulkan for the coverage capture"]
fn lighter_coverage_fixture_capture() {
    let root = FixtureRoot::new();
    // One available façade with a missing texture, one absent member and an
    // out-of-range placement. These are mutually exclusive placement results.
    std::fs::remove_file(root.0.join("building.pck")).unwrap();
    let mut terrain = fixture(&root.0);
    terrain.objects = crate::objects::Objects::open(
        &root.0,
        vec![
            ornament_record(),
            crate::scene::Ornament {
                export_id: 124,
                path: "not-installed.bmd".into(),
                ..ornament_record()
            },
            crate::scene::Ornament {
                export_id: 125,
                position: [3000., 0., 0.],
                ..ornament_record()
            },
        ],
    )
    .unwrap();
    terrain.trees.types.push(None);
    terrain.trees.placements.push(crate::scene::Tree {
        tree_type: 1,
        position: [-8., 0., 0.],
    });
    let dry = crate::water::WaterArea {
        export_id: 2,
        mask: vec![0, 0],
        shore_waves: Vec::new(),
        ..terrain.water[0].clone()
    };
    terrain.water.push(dry);
    let effects = &mut std::sync::Arc::get_mut(&mut terrain.scene).unwrap().blocks[0].effects;
    effects.truncate(1);
    effects.push(placement(1, "scene/binary.gfx", [-6., 2., 0.]));
    let sprite = gfx_header() + SPRITE;
    let glow = encode(
        &image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 32, 0, 128])),
        image::ImageFormat::Png,
    );
    crate::pck::write_test_archive(
        &root.0.join("gfx.pck"),
        &[
            ("gfx/scene/lighter-sprite.gfx", sprite.as_bytes()),
            ("gfx/scene/binary.gfx", b"MOXB synthetic unsupported root"),
            ("gfx/textures/lighter-glow.png", &glow),
        ],
    );
    let actor = crate::scene::SceneModel {
        export_id: 1,
        position: [0., 2., -4.],
        direction: [0., 0., 1.],
        up: [0., 1., 0.],
        path: "models/a/a.ecm".into(),
        action: String::new(),
    };
    let (mut asset, _) = crate::model_draw_fixture::fixture();
    asset.skins[0].data.meshes[0].vertices[1].position[1] = 1.;
    let playback = crate::model_playback::Playback::new(&asset, None);
    terrain.models = crate::world_models::Assets::load_with(
        vec![crate::scene::SceneModel {
            export_id: 2,
            path: "models/missing.ecm".into(),
            ..actor.clone()
        }],
        |_| Err("synthetic absent archive member".into()),
    );
    terrain
        .models
        .instances
        .push(crate::world_models::Instance {
            placement: actor,
            asset: 0,
            action: None,
            playback,
        });
    terrain.models.assets.push(asset);
    crate::pck::write_test_archive(
        &root.0.join("models.pck"),
        &[("fixture-placeholder", b"no textures")],
    );

    let path = root.0.join("coverage.png");
    super::super::preview_frame(
        terrain,
        &path,
        Some([-2.5, 5., 0., 0., -std::f32::consts::FRAC_PI_4]),
        Mode::Software,
        Some(2),
    )
    .unwrap();
    assert_world_png(&path, (960, 720));
    let csv = std::fs::read_to_string(path.with_extension("coverage.csv")).unwrap();
    assert_eq!(
        csv,
        include_str!("../../../tools/fixtures/lighter-coverage.csv")
    );
    let meta = std::fs::read_to_string(path.with_extension("coverage.txt")).unwrap();
    assert!(meta.contains("map=\"lighter-fixture\"\n") && meta.contains("warmup_frames=2\n"));
    assert!(meta.contains("animation_elapsed_ms=32\n"));
    for field in [
        "mip_fallback",
        "mip_generated",
        "mip_reduced",
        "mip_authored_levels",
    ] {
        let value = meta
            .lines()
            .find_map(|line| line.strip_prefix(field).and_then(|s| s.strip_prefix('=')))
            .and_then(|s| s.parse::<usize>().ok());
        assert!(value.is_some(), "invalid mip audit field: {field}");
    }
    use sha2::{Digest, Sha256};
    let rgba = image::open(&path).unwrap().into_rgba8();
    let digest = Sha256::digest(rgba.as_raw());
    assert!(meta.contains(&format!("rgba8_sha256={digest:x}\n")));
    if let Some(dir) = std::env::var_os("PW_SAVE_LIGHTER_COVERAGE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for ext in ["png", "coverage.csv", "coverage.txt"] {
            let src = path.with_extension(ext);
            std::fs::copy(&src, dir.join(src.file_name().unwrap())).unwrap();
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "requires a real Win32 display and SDK, or X11 and Vulkan for the portable control"]
fn lighter_live_viewer_frames() {
    let root = FixtureRoot::new();
    run_live_fixture(&root.0, &root.0).unwrap();
    let sdk = window_sdk_fixture();
    let (base, resized) = if sdk {
        ((512, 512), (640, 640))
    } else {
        ((160, 96), (192, 112))
    };
    assert_world_png(&root.0.join("0.png"), base);
    for i in 1..4 {
        assert_world_png(&root.0.join(format!("{i}.png")), resized);
    }
    if sdk {
        assert_world_png(&root.0.join("recovered-sdk.png"), base);
        save_window_sdk_fixture(&root.0, "recovery");
    }
    let child_pid: u32 = std::fs::read_to_string(root.0.join("restored-present"))
        .expect("replacement did not present")
        .parse()
        .unwrap();
    assert_ne!(child_pid, std::process::id());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an X11 display (e.g. xvfb-run) and Vulkan"]
fn lighter_live_travel_csv() {
    let root = FixtureRoot::new();
    let motion = std::env::var_os("PW_TEST_LIGHTER_TRAVEL_MOTION").is_some();
    let sync = std::env::var_os("PW_TEST_LIGHTER_TRAVEL_SYNC").is_some();
    let count = if sync { 500 } else { 30 };
    std::fs::write(
        root.0.join("travel.plan"),
        format!(
            "lighter-fixture 12 3\n0 -2.5 5 0 0 -0.7853982\n{} 2.5 5 -4 0.2 -0.5\n",
            count - 1
        ),
    )
    .unwrap();
    if motion {
        std::fs::write(root.0.join("motion-test"), []).unwrap();
    }
    if sync {
        std::fs::write(root.0.join("sync-test"), []).unwrap();
    }
    super::super::run_viewer(fixture(&root.0), Some(&root.0)).unwrap();
    let csv = std::fs::read_to_string(root.0.join("travel.csv")).unwrap();
    for header in [
        "# display=160x96\n",
        "# trace=0x0\n",
        "# ray=Settings { mode: Off,",
    ] {
        assert!(csv.contains(header), "missing {header:?} from {csv}");
    }
    let rows = csv
        .lines()
        .filter(|s| !s.starts_with("# "))
        .skip(1)
        .collect::<Vec<_>>();
    assert!(csv.contains(&format!("# count={count}\n")));
    assert_eq!(rows.len(), count);
    for (i, row) in rows.iter().enumerate() {
        let columns = row.split(',').collect::<Vec<_>>();
        assert_eq!(columns.len(), if sync { 23 } else { 15 });
        if sync {
            assert!(columns[15]
                .parse::<f64>()
                .is_ok_and(|v| v.is_finite() && v >= 0.));
            assert!(columns[16..].iter().all(|column| column.is_empty()));
        }
        assert_eq!(columns[0].parse::<usize>().unwrap(), i);
        for field in &columns[1..5] {
            let ms: f64 = field.parse().unwrap();
            assert!(ms.is_finite() && ms >= 0.);
        }
    }
    assert!(rows[0].contains(",-2.5,5,0,0,-0.7853982,"));
    assert!(rows[count - 1].contains(",2.5,5,-4,0.2,-0.5,"));
    assert!(
        !root.0.join("0.png").exists(),
        "travel must not capture a screenshot"
    );
    if motion {
        assert!(csv.contains("# capture_policy=motion_readback\n"));
        for i in 0..30 {
            assert_world_png(
                &root.0.join("motion").join(format!("{i:06}.png")),
                (160, 96),
            );
        }
    } else {
        let policy = if sync {
            "synchronized"
        } else {
            "normal_present"
        };
        assert!(csv.contains(&format!("# capture_policy={policy}\n")));
        assert!(!root.0.join("motion").exists());
    }
    if let Some(path) = std::env::var_os("PW_SAVE_LIGHTER_TRAVEL") {
        std::fs::copy(root.0.join("travel.csv"), path).unwrap();
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "requires a real Win32 display and SDK, or X11 and a CPU Vulkan adapter for the portable control"]
fn lighter_live_world_reload() {
    let root = FixtureRoot::new();
    std::fs::write(
        root.0.join("reload-test"),
        b"saved software => native => unsupported hardware => stalled software => software",
    )
    .unwrap();
    run_live_fixture(&root.0, &root.0).unwrap();
    if window_sdk_fixture() {
        for file in ["0.png", "reload-software.png"] {
            assert_world_png(&root.0.join(file), (512, 512));
        }
        save_window_sdk_fixture(&root.0, "reload");
    } else {
        for file in [
            "0.png",
            "after-failed-reload.png",
            "after-stalled-reload.png",
            "reload-software.png",
        ] {
            assert_world_png(&root.0.join(file), (160, 96));
        }
    }
    let child_pid: u32 = std::fs::read_to_string(root.0.join("restored-present"))
        .expect("same-world reload did not present")
        .parse()
        .unwrap();
    assert_ne!(child_pid, std::process::id());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
#[ignore = "entrypoint for a window fixture handoff subprocess"]
fn lighter_live_handoff_child() {
    let Some(dir) = std::env::var_os("PW_TEST_LIGHTER_VIEWER_DIR") else {
        return;
    };
    assert!(super::super::same_world_handoff().is_some());
    let root = FixtureRoot::new();
    if let Err(error) = run_live_fixture(&root.0, std::path::Path::new(&dir)) {
        crate::world_handoff::publish_failure(&error);
        panic!("replacement viewer: {error}");
    }
}

#[cfg(any(target_os = "linux", windows))]
fn window_sdk_fixture() -> bool {
    cfg!(windows) || std::env::var_os("PW_TEST_LIGHTER_WINDOW_SDK").is_some()
}

#[cfg(any(target_os = "linux", windows))]
fn run_live_fixture(root: &std::path::Path, smoke: &std::path::Path) -> Result<(), String> {
    super::super::run_viewer(
        fixture(root),
        #[cfg(windows)]
        None,
        Some(smoke),
    )
}

#[cfg(any(target_os = "linux", windows))]
fn save_window_sdk_fixture(root: &std::path::Path, report: &str) {
    assert!(std::fs::read_to_string(root.join("restored-provider"))
        .unwrap()
        .starts_with("Vulkan "));
    if let Some(dir) = std::env::var_os("PW_SAVE_LIGHTER_WINDOW") {
        let dir = std::path::PathBuf::from(dir).join(report);
        std::fs::create_dir_all(&dir).unwrap();
        for entry in std::fs::read_dir(root).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "png")
                || entry.file_name() == "restored-provider"
                || entry.file_name() == "restored-present"
            {
                std::fs::copy(&path, dir.join(entry.file_name())).unwrap();
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan for a destroyed/fresh device with streamed assets"]
fn lighter_device_recovery_fixture() {
    // Inject wgpu's explicit destroy/lost callback, not a physical driver
    // reset. Rebuild the same streamed window on a completely fresh device.
    let root = FixtureRoot::new();
    let position = [-2.5, 5., 0., 0., -std::f32::consts::FRAC_PI_4];
    let instance = super::super::make_instance(Mode::Software);
    let (_, device, queue) = pollster::block_on(adapter(&instance, None, Mode::Software)).unwrap();
    let loss = super::super::recovery::Loss::default();
    loss.register(&device, || {});
    queue.submit([]);
    device.destroy();
    super::super::wait_for_capture(&device, || Ok(loss.take())).unwrap();
    drop((queue, device, instance));
    let path = root.0.join("rebuilt.png");
    let mut terrain = fixture(&root.0);
    terrain.objects = crate::objects::Objects::open(&root.0, vec![ornament_record()]).unwrap();
    super::super::preview_frame(terrain, &path, Some(position), Mode::Software, Some(0)).unwrap();
    assert_world_png(&path, (960, 720));
    let meta = std::fs::read_to_string(path.with_extension("coverage.txt")).unwrap();
    assert!(meta.contains("map=\"lighter-fixture\""));
    let inventory = std::fs::read_to_string(path.with_extension("coverage.csv")).unwrap();
    assert!(inventory.lines().any(|l| l.starts_with("buildings,1,")));
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
    let mut feed = super::Feed::new(&mut t, &first, false, storage, false).unwrap();
    let install = |feed: &mut super::Feed,
                   mesh: &super::MeshData,
                   (local, extra, outside): (usize, usize, usize)| {
        let (packet, textures, inventory, coverage) = feed.snapshot(mesh, storage).unwrap();
        assert_eq!(
            (
                coverage.local,
                coverage.extra,
                coverage.counts.range + coverage.counts.cutoff
            ),
            (local, extra, outside),
            "{coverage}"
        );
        assert_eq!(
            (
                coverage.resident_chunks,
                coverage.catalog_chunks,
                coverage.estimated_bounds
            ),
            (local + extra, 1, 0)
        );
        assert_eq!(
            (
                coverage.counts.texture_fallback,
                coverage.counts.budget,
                coverage.counts.failed()
            ),
            (0, 0, 0),
            "{coverage}"
        );
        assert_eq!(inventory[super::Class::Buildings], coverage.counts);
        (packet, textures)
    };
    let extended = install(&mut feed, &first, (0, 1, 0));
    // Required terrain and vegetation can already occupy the binding. The
    // optional asset must be deferred, not leave a partially packed mesh.
    let (limited, _, inventory, stats) =
        feed.snapshot(&first, extended.0.bytes().len() - 1).unwrap();
    assert_eq!((stats.extra, stats.counts.budget), (0, 1));
    assert_eq!(inventory[super::Class::Buildings].budget, 1);
    assert_eq!(extended.0.triangles(), limited.triangles() + 2);
    let local_mesh = make(&mut t, 160.);
    assert!(!local_mesh.local_pages.is_empty());
    let mut packer = feed.take_packer().unwrap();
    let local_scene = std::thread::scope(|scope| {
        scope
            .spawn(|| packer.prepare(&local_mesh, storage, super::lighter::Mode::Software))
            .join()
            .unwrap()
            .unwrap()
    });
    assert_eq!(
        (local_scene.coverage.local, local_scene.coverage.extra),
        (1, 0)
    );
    assert!(feed.packer.is_none());
    // Keep both pictures immutable until the native upload is ready. The
    // fixture then reuses the same worker cache for eviction/re-entry tests.
    let local = (local_scene.packet, local_scene.bank);
    feed.packer = Some(packer);
    let evicted = install(&mut feed, &make(&mut t, -1200.), (0, 0, 1));
    let restored = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    t.objects = source(0x0100);
    let transmit_only = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    t.objects = source(0x0001);
    let reflect_only = install(&mut feed, &make(&mut t, 0.), (0, 1, 0));
    StreamedBmdScenes {
        terrain: feed.source.terrain,
        area_table: feed.resident.area_table,
        extended,
        local,
        evicted,
        restored,
        transmit_only,
        reflect_only,
    }
}

pub(crate) fn joined_bmd_packets(storage: usize) -> [Scene; 4] {
    let root = FixtureRoot::new();
    let facade = |x0, x1| {
        let mut b = bmd_header([128., 0., -5.], 1);
        bmd_quad(
            &mut b,
            [[x0, 1., 0.], [x1, 1., 0.], [x0, 5., 0.], [x1, 5., 0.]],
            [0., 0., 1.],
            "",
        );
        b
    };
    crate::pck::write_test_archive(
        &root.0.join("litmodels.pck"),
        &[
            ("litmodels/a01/left.bmd", &facade(-4., 0.)),
            ("litmodels/a01/right.bmd", &facade(0., 4.)),
        ],
    );
    let ornaments = [(-1., "left.bmd"), (1., "right.bmd")]
        .into_iter()
        .enumerate()
        .map(|(i, (dx, path))| crate::scene::Ornament {
            export_id: i as u32,
            path: path.into(),
            position: [128. + dx, 2., 0.],
            ..ornament_record()
        })
        .collect::<Vec<_>>();
    let mut objects = crate::objects::Objects::open(&root.0, ornaments).unwrap();
    let mut stream = super::SceneryStream::default();
    let mut cache = super::AccelCache::default();
    let mut install = |local: &[usize], x, counts: (usize, usize, usize)| {
        objects.load_ids(local).unwrap();
        let snapshot = stream
            .prepare(&mut objects, local, [x, 0.], 32., None, &[])
            .unwrap();
        let mut builder = super::Builder::default();
        let mut bank = super::Textures::default();
        let stats = snapshot.append(&mut builder, &mut bank, storage).unwrap();
        assert_eq!(
            (
                stats.local,
                stats.extra,
                stats.counts.range + stats.counts.cutoff
            ),
            counts
        );
        assert_eq!((stats.catalog_chunks, stats.estimated_bounds), (2, 0));
        let packet = builder.finish_cached(&mut cache);
        assert_eq!(packet.triangles(), (counts.0 + counts.1) * 2);
        assert_eq!(packet.instances.len(), stats.resident_chunks);
        if stats.resident_chunks != 0 {
            assert_eq!(
                packet
                    .meshes
                    .iter()
                    .map(|m| m.key)
                    .map(|key| match key {
                        super::MeshKey::Static(Some(chunk), _) => chunk,
                        _ => panic!("missing chunk owner"),
                    })
                    .collect::<Vec<_>>(),
                [
                    super::ChunkKey::Scenery(0, 0),
                    super::ChunkKey::Scenery(1, 0)
                ]
            );
        }
        (packet, bank)
    };
    [
        install(&[0, 0], 127.5, (1, 1, 0)),
        install(&[1], 128.5, (1, 1, 0)),
        install(&[], -1600., (0, 0, 2)),
        install(&[], 128., (0, 2, 0)),
    ]
}

#[test]
fn ray_stream_anchor_crosses_blocks_even_when_terrain_lods_are_clamped() {
    let terrain = |x| super::super::lod_levels(TILE, TILE, 1., [x, 0.], 7);
    let anchor = |x| super::super::camera_block(TILE, TILE, 1., [x, 0.]);
    assert_eq!(terrain(-900.), terrain(-1000.));
    assert_ne!(anchor(-900.), anchor(-1000.));
    assert_eq!(anchor(-900.), anchor(-901.));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "benchmark: requires lavapipe with KHR_ray_query and saves matched headless paths"]
fn lighter_matched_fixture_capture() {
    let root = FixtureRoot::new();
    let save = std::env::var_os("PW_SAVE_LIGHTER_MATCHED")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.0.clone());
    std::fs::create_dir_all(&save).unwrap();
    let only = std::env::var("PW_LIGHTER_FIXTURE_ONLY").ok();
    let authored = std::env::var("PW_LIGHTER_FIXTURE_AUTHORED_BC").as_deref() == Ok("1");
    let warmup = std::env::var("PW_LIGHTER_FIXTURE_WARMUP").map_or(4, |n| {
        n.parse::<u32>()
            .ok()
            .filter(|n| (1..=600).contains(n))
            .expect("invalid fixture warm-up")
    });
    let mut captures = 0;
    let mut adapter_name = None;
    for (name, mode, raster) in [
        ("native", Mode::Off, false),
        ("software", Mode::Software, false),
        ("full-ray", Mode::Hardware, false),
        ("hybrid", Mode::Hardware, true),
    ] {
        if only.as_ref().is_some_and(|mode| mode != name) {
            continue;
        }
        let mut terrain = fixture(&root.0);
        if authored {
            // Real terrain upload and feed, with seven authored mips. Give
            // this input a distinct ID so it is never diffed with the 4²
            // single-mip water/Hybrid fixture by accident.
            terrain.map.id = "lighter-fixture-authored-bc".into();
            let mut dds = vec![0; 128];
            dds[..4].copy_from_slice(b"DDS ");
            dds[84..88].copy_from_slice(b"DXT1");
            for (at, value) in [(4, 124u32), (12, 64), (16, 64), (28, 7), (76, 32), (80, 4)] {
                dds[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            for lod in 0..7 {
                let blocks = (64u32 >> lod).div_ceil(4);
                for y in 0..blocks {
                    for x in 0..blocks {
                        let a = if (x ^ y) & 1 != 0 {
                            0x8780u16
                        } else {
                            0x57a0u16
                        };
                        dds.extend(a.to_le_bytes());
                        dds.extend(0x2900u16.to_le_bytes());
                        dds.extend([0xe4, 0x4e, 0x4b, 0x93]);
                    }
                }
            }
            terrain.materials.textures[0] = crate::material::Texture {
                w: 64,
                h: 64,
                source: dds,
                levels: Default::default(),
            };
        }
        let path = save.join(format!("{name}.png"));
        super::super::preview_frame_using(
            terrain,
            &path,
            Some([-2.5, 5., 0., 0., -std::f32::consts::FRAC_PI_4]),
            mode,
            Some(warmup),
            |mut settings| {
                settings.hybrid_primary = raster;
                let context = if mode != Mode::Hardware {
                    super::super::preview_device(settings)?
                } else {
                    // Test only. Never offer Hardware mode to a CPU adapter
                    // in the viewer or the production preview CLI.
                    let instance = super::super::make_instance(mode);
                    let adapter = pollster::block_on(
                        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
                    )
                    .ok_or("missing CPU Vulkan adapter")?;
                    let info = adapter.get_info();
                    assert_eq!(info.device_type, wgpu::DeviceType::Cpu);
                    let available = adapter.features();
                    let mut limits =
                        wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
                    limits.max_buffer_size = adapter.limits().max_buffer_size.min(512 << 20);
                    limits.max_storage_buffer_binding_size = adapter
                        .limits()
                        .max_storage_buffer_binding_size
                        .min(256 << 20);
                    let features = Lighter::CPU_QUERY_TEST_FEATURES
                        | (available
                            & (wgpu::Features::TIMESTAMP_QUERY
                                | wgpu::Features::TEXTURE_COMPRESSION_BC
                                | wgpu::Features::MULTI_DRAW_INDIRECT
                                | wgpu::Features::INDIRECT_FIRST_INSTANCE));
                    let (device, queue) = pollster::block_on(adapter.request_device(
                        &wgpu::DeviceDescriptor {
                            label: None,
                            required_features: features,
                            required_limits: limits,
                        },
                        None,
                    ))
                    .map_err(|e| e.to_string())?;
                    let lighter = Lighter::new(settings, &adapter, &device)
                        .for_cpu_query_test(&adapter, &device);
                    (info, device, queue, lighter)
                };
                assert_eq!(context.0.device_type, wgpu::DeviceType::Cpu);
                if let Some(info) = &adapter_name {
                    assert_eq!(info, &context.0);
                } else {
                    adapter_name = Some(context.0.clone());
                }
                eprintln!("Lighter matched capture: {name}, {:?}", context.0);
                Ok(context)
            },
        )
        .unwrap();
        assert_world_png(&path, (960, 720));
        captures += 1;
    }
    assert!(captures != 0, "unknown PW_LIGHTER_FIXTURE_ONLY draw route");
}
