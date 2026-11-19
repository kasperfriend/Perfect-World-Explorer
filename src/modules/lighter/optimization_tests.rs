//! Matched fixtures for interactive hardware optimizations, run through both
//! real ray backends. Uses small scenes with exact color/depth expectations.
use super::*;

#[test]
fn hardware_packet_has_no_duplicate_geometry_tree() {
    let make = |mode| {
        let mut b = Builder::for_mode(mode);
        let id = upright(&mut b, -4., [1.; 4], 0, 0);
        for i in 0..64 {
            b.place(id, Transform::translation([i as f32, 0., 0.]))
                .unwrap();
            b.light(Light::point([i as f32, 0., 0., 2.], [1.; 4]));
        }
        let mut cache = AccelCache::default();
        let p = b.finish_cached(&mut cache);
        (p, cache.builds)
    };
    let (cpu, cpu_builds) = make(Mode::Software);
    let (hw, hw_builds) = make(Mode::Hardware);
    assert_eq!(cpu_builds, [1, 1]);
    assert_eq!(hw_builds, [0, 0]);
    assert_eq!(&hw.words[7..9], &[0, 0]);
    let light_nodes = |p: &Packet| {
        let offset = p.words[5] as usize;
        let start = offset + 9 * p.words[12] as usize;
        let end = offset + 9 * p.words[13] as usize;
        end - start
    };
    assert_eq!(light_nodes(&hw), light_nodes(&cpu));
    assert!(light_nodes(&hw) > 0);
    assert_eq!(hw.meshes[0].shape, cpu.meshes[0].shape);
    assert!(hw.bytes().len() < cpu.bytes().len());
}

pub(super) fn exercise(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings = Settings {
        mode: saved.mode,
        lighting: false,
        shadows: false,
        reflections: false,
        progressive: false,
        denoiser: false,
        bounces: 1,
        samples: 1,
        resolution: 100,
        single_path: false, // fixed-radiance optics probes
        ..Settings::default()
    };
    opaque_leaves_keep_cutout_holes(h);
    paint_layers(h);
    distance_only_budgets_diffuse(h);
    rigid_instances(h);
    matching_wet_tiles(h);
    h.ray.settings = saved;
}

fn opaque_leaves_keep_cutout_holes(h: &mut Harness) {
    let mut b = Builder::for_mode(h.ray.settings.mode);
    let mut bank = Textures::default();
    let leaf = bank
        .tile([2, 1], 2, textures::Filter::cutout(0.5, true), |x, _| {
            if x == 0 {
                0xff00ff00
            } else {
                0
            }
        })
        .unwrap();
    let sky = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xffff0000)
        .unwrap();
    let id = quad(
        &mut b,
        [
            [-1., 1., -3.],
            [1., 1., -3.],
            [-1., -1., -3.],
            [1., -1., -3.],
        ],
        [0., 0., 1.],
        leaf,
        FOLIAGE | CUTOUT | STRICT_ALPHA | PRIMARY_ONLY,
        u32::MAX,
    );
    put(&mut b, id);
    h.ray.install(Ok((b.finish(), bank)));
    let empty = Builder::for_mode(h.ray.settings.mode).finish();
    let probe = |h: &mut Harness, x: f32, bounces| {
        let mut f = test_frame(Vec3::X * x, Vec3::NEG_Z, 31);
        f.sky0 = [sky, sky, sky, 0];
        f.sky_tint = [1.; 4];
        h.ray.settings.bounces = bounces;
        h.pixel(f, &empty)
    };
    for n in [0, 1, 4] {
        let covered = probe(h, -0.55, n);
        assert_eq!(
            covered.0,
            [0, 255, 0, 255],
            "sky leaked through a covered leaf at {n} bounces"
        );
        assert!(covered.1 > 0.);
        assert_eq!(
            probe(h, 0.55, n),
            ([0, 0, 255, 255], 0.),
            "an authored hole stopped showing sky"
        );
    }
}

fn paint_layers(h: &mut Harness) {
    let mut b = Builder::for_mode(h.ray.settings.mode);
    let mut bank = Textures::default();
    let color = [0xff0000ff, 0xffff0000, 0xff00ff00, 0xffff00ff].map(|p| {
        bank.tile([2, 2], 2, textures::Filter::Color, |_, _| p)
            .unwrap()
    });
    let masks = [[0; 4], [128; 4], [0, 255, 0, 255]].map(|p| {
        bank.insert(&textures::Tile::mask(2, 2, &p).unwrap())
            .unwrap()
    });
    let at = bank.reserve(4 + 20).unwrap();
    bank.write_words(at, &[at as u32 + 4, 4, 0, 0]);
    for (i, address) in color.into_iter().enumerate() {
        bank.write_words(
            at + 4 + i * 5,
            &[
                address,
                if i == 0 { 0 } else { masks[i - 1] },
                1,
                1f32.to_bits(),
                1f32.to_bits(),
            ],
        );
    }
    let plane = quad(
        &mut b,
        [[-2., 0., 2.], [2., 0., 2.], [-2., 0., -2.], [2., 0., -2.]],
        [0., 1., 0.],
        0,
        TERRAIN,
        u32::MAX,
    );
    put(&mut b, plane);
    h.ray.install(Ok((b.finish(), bank)));
    let empty = Builder::for_mode(h.ray.settings.mode).finish();
    let probe = |h: &mut Harness, x| {
        let mut f = test_frame(Vec3::new(x, 4., 0.), Vec3::NEG_Y, 41);
        f.terrain = [8., 8., 0.5, 8.];
        f.options[3] = at as u32;
        h.pixel(f, &empty)
    };
    assert_radiance(
        "top zero, blue zero, red/green mix",
        probe(h, -1.5),
        [127. / 255., 128. / 255., 0.],
    );
    assert_radiance(
        "opaque paint hides every lower layer",
        probe(h, 1.4),
        [1., 0., 1.],
    );
}

fn distance_only_budgets_diffuse(h: &mut Harness) {
    h.ray.settings.lighting = true;
    h.ray.settings.bounces = 1;
    let m = h.ray.settings.mode;
    let mut bank = Textures::default();
    let black = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xff000000)
        .unwrap();
    let mut f = test_frame(Vec3::ZERO, Vec3::NEG_Z, 9);
    f.sky0 = [black, black, black, 0];
    f.sky_tint = [1.; 4];
    f.ambient = [0.; 4];
    let empty = Builder::for_mode(m).finish();
    let build = |reflectance| {
        let mut b = Builder::for_mode(m);
        let i = quad_material(
            &mut b,
            [
                [-4., 4., -4.],
                [4., 4., -4.],
                [-4., -4., -4.],
                [4., -4., -4.],
            ],
            [0., 0., 1.],
            Surface::new(0, 0, 0., reflectance),
            u32::MAX,
        );
        put(&mut b, i);
        let j = quad(
            &mut b,
            [
                [-100., 100., 20.],
                [100., 100., 20.],
                [-100., -100., 20.],
                [100., -100., 20.],
            ],
            [0., 0., -1.],
            0,
            EMISSIVE,
            0xff0000ff,
        );
        put(&mut b, j);
        b.finish()
    };
    h.ray.install(Ok((build(0.), bank.clone())));
    h.ray.settings.diffuse_distance = 3;
    let limited = h.pixels(f, &empty);
    assert!(limited.iter().all(|(c, z)| c[..3] == [0; 3] && *z > 0.));
    h.ray.settings.diffuse_distance = 0;
    let full = h.pixels(f, &empty);
    assert!(full.iter().filter(|p| p.0[0] > 250).count() > 40);
    for (a, b) in full.iter().zip(limited) {
        assert_eq!(a.1, b.1);
    }
    h.ray.install(Ok((build(1.), bank)));
    h.ray.settings.diffuse_distance = 3;
    h.ray.settings.reflections = true;
    assert_radiance(
        "mirror still traces 24 units",
        h.pixel(f, &empty),
        [1., 0., 0.],
    );
    h.ray.settings.lighting = false;
    h.ray.settings.reflections = false;
}

fn rigid_instances(h: &mut Harness) {
    let mut b = Builder::for_mode(h.ray.settings.mode);
    let red =
        b.surface(Surface::new(0, 0, 0., 0.).with_lighting([1., 0., 0., 1.], [1.; 4], [0.; 4]));
    let blue =
        b.surface(Surface::new(0, 0, 0., 0.).with_lighting([0., 0., 1., 1.], [1.; 4], [0.; 4]));
    let mut m = Mesh::default();
    for [x, y] in [[-1., 1.], [1., 1.], [-1., -1.], [1., -1.]] {
        m.vertices
            .push(Vertex::new([x, y, -4.], [0., 0., 1.], [0.; 2]));
    }
    m.tri([0, 2, 1], red);
    m.tri([1, 2, 3], red);
    for (x, s) in [(0., red), (4., red), (8., blue)] {
        m.surfaces.fill(s);
        b.rigid_part(
            &m,
            Attachment::new(Mat4::from_translation(Vec3::X * x)).unwrap(),
        )
        .unwrap();
    }
    let p = b.finish();
    assert_eq!((p.instances.len(), p.meshes.len()), (3, 2));
    assert_eq!(
        p.meshes[0].key, p.meshes[1].key,
        "material split must still share BLAS"
    );
    let make_empty = || Builder::for_mode(h.ray.settings.mode).finish();
    let empty = make_empty();
    let previous = h.builds();
    h.ray.install(Ok((empty, Textures::default())));
    for (x, color) in [(0., [1., 0., 0.]), (4., [1., 0., 0.]), (8., [0., 0., 1.])] {
        let f = test_frame(Vec3::X * x, Vec3::NEG_Z, 3);
        assert_radiance(
            "shared rigid geometry, independent material and transform",
            h.pixel(f, &p),
            color,
        );
    }
    if h.ray.settings.mode == Mode::Hardware {
        assert_eq!(h.builds()[1] - previous[1], 1);
    }
}

fn matching_wet_tiles(h: &mut Harness) {
    use crate::water::WaterArea;
    let area = |id, x, y, grid, columns, rows| WaterArea {
        export_id: id,
        color: u32::MAX,
        simple: false,
        height: y,
        grid_size: grid,
        columns,
        rows,
        center: [x, y, 0.],
        mask: vec![1; columns * rows],
        shore_waves: vec![],
    };
    let west = area(10, -1., 0., 1., 2, 2);
    let east = area(11, 1., 0., 2., 1, 1);
    let whole = area(12, 0., 0., 1., 4, 2);
    let eye = Vec3::new(-1.5, -1., 0.1);
    let exit = Vec3::new(1.5, -1., 0.1);
    let mode = h.ray.settings.mode;
    let scene = || {
        let mut b = Builder::for_mode(mode);
        for (x, n) in [(1.5, [-1., 0., 0.]), (-1.5, [1., 0., 0.])] {
            let wall = quad(
                &mut b,
                [[x, -0.2, -1.], [x, -0.2, 1.], [x, -3., -1.], [x, -3., 1.]],
                n,
                0,
                EMISSIVE,
                u32::MAX,
            );
            put(&mut b, wall);
        }
        // The bed has the same material on both sides of the geometric join.
        for (lo, hi) in [(-2., 0.), (0., 2.)] {
            let bed = quad(
                &mut b,
                [
                    [lo, -3.5, 1.],
                    [hi, -3.5, 1.],
                    [lo, -3.5, -1.],
                    [hi, -3.5, -1.],
                ],
                [0., 1., 0.],
                0,
                0,
                u32::MAX,
            );
            put(&mut b, bed);
        }
        b.finish()
    };
    let empty = Builder::for_mode(mode).finish();
    let probe = |h: &mut Harness, areas: &[WaterArea], pos, d| {
        let mut bank = Textures::default();
        let mut f = test_frame(pos, d, 3);
        f.water_effects = crate::gpu::ray_test_wet_grid(&mut bank, areas, pos);
        f.resident_bounds = [-100., 100., -100., 100.];
        f.water = [0., 12., 1., 0.];
        f.sun = Vec3::new(0.32, 0.95, 0.).normalize().extend(1.).to_array();
        f.sun_color = [1.; 4];
        f.ambient = [0.; 4];
        h.ray.install(Ok((scene(), bank)));
        h.pixel(f, &empty)
    };
    let mut partial_west = west.clone();
    partial_west.mask = vec![1, 1, 0, 0]; // same wet row, bitmap walk instead of a rectangle
    for (from, dir) in [
        (eye, Vec3::X),
        (exit, Vec3::NEG_X),
        (Vec3::new(0., eye.y, eye.z), Vec3::NEG_X),
    ] {
        let expected = probe(h, std::slice::from_ref(&whole), from, dir);
        for tile in [&west, &partial_west] {
            assert_eq!(
                probe(h, &[tile.clone(), east.clone()], from, dir),
                expected,
                "wet-to-wet export seam changed extinction or depth from {from:?}"
            );
        }
    }
    let wet = probe(h, &[west.clone(), east.clone()], eye, Vec3::X);
    for other in [area(11, 1.25, 0., 2., 1, 1), area(11, 1., 0.1, 2., 1, 1)] {
        let dry = probe(h, &[west.clone(), other], eye, Vec3::X);
        assert_eq!(dry.1, wet.1);
        assert!(
            dry.0[0] > wet.0[0] + 20,
            "a dry gap or a different surface level was erased"
        );
    }
    // The inverse-Snell sunlight connection crosses the seam while the
    // eye looks straight down at a constant material and depth.
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.soft_sun = false;
    h.ray.settings.bounces = 0;
    let split = [west, east];
    for x in [-0.02, 0., 0.02] {
        let p = Vec3::new(x, -1., 0.1);
        assert_eq!(
            probe(h, &split, p, Vec3::NEG_Y),
            probe(h, &[whole.clone()], p, Vec3::NEG_Y),
            "bed shading at x={x} disagrees with an unbroken pool"
        );
    }
    eprintln!("Lighter: covered leaf vs cutout, masked terrain paint, range separation, instancing, and wet tile joins passed");
}
