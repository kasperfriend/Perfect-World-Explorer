use super::*;
use glam::{Mat4, Vec3};
use scene::*;

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
fn naga_validates_both_paths_and_emits_native_ray_queries() {
    use naga_rt as naga;
    for src in [
        [BASE, SOFTWARE].concat(),
        include_str!("present.wgsl").into(),
    ] {
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
    }
    let shader =
        vulkan::shader(&[BASE, HARDWARE].concat()).expect("Vulkan ray-query WGSL to SPIR-V");
    assert_eq!(shader[0], 0x0723_0203);
    let mut opcodes = Vec::new();
    let mut intersection_kinds = std::collections::BTreeSet::new();
    let mut at = 5;
    while at < shader.len() {
        let op = shader[at] & 0xffff;
        opcodes.push(op);
        if op == 4479 {
            // OpRayQueryGetIntersectionTypeKHR takes the intersection selector
            // last. Naga 25 reused the first helper for both kinds of getter.
            intersection_kinds.insert(shader[at + 4]);
        }
        at += (shader[at] >> 16) as usize;
    }
    assert_eq!(intersection_kinds.len(), 2, "missing a ray hit getter");
    // SPV_KHR_ray_query: initialize, proceed and explicit alpha-tested confirm.
    assert!([4473, 4476, 4477]
        .into_iter()
        .all(|op| opcodes.contains(&op)));
}

#[test]
fn packet_rejects_bad_indices_and_materials() {
    let mut builder = Builder::default();
    let mut mesh = Mesh::default();
    mesh.tri([0, 1, 2], 0);
    assert!(builder.world_mesh(&mesh).is_err());
    builder.surface(Surface::new(0, 0, 0., 0.));
    assert!(builder.world_mesh(&mesh).is_err());
    assert!(builder.place(0, Transform::IDENTITY).is_err());
}

#[test]
fn light_influence_tree_limits_per_hit_work() {
    let mut b = Builder::default();
    for i in 0..1024 {
        b.light(Light {
            position_range: [i as f32 * 8., 0., 0., 2.],
            color: [1.; 4],
        });
    }
    let packet = b.finish();
    let w = &packet.words;
    assert_eq!(w[10], 1024);
    let at = w[5] as usize;
    let mut node = w[12];
    let mut candidates = 0;
    let mut visited = 0;
    let p = 2048.;
    // Traverse the packed nodes the GPU sees without the subsequent sphere
    // test. Counting leaf entries measures work saved even in a no-hit leaf.
    while node < w[13] {
        visited += 1;
        let n = at + 9 * node as usize;
        if p < f32::from_bits(w[n]) || p > f32::from_bits(w[n + 3]) {
            node = w[n + 6];
        } else {
            candidates += w[n + 8];
            node += 1;
        }
    }
    assert_eq!(candidates, 4);
    assert_eq!(visited, 17);
}

fn test_frame(eye: Vec3, dir: Vec3, e: u32) -> Frame {
    let vp = Mat4::perspective_rh(0.7, 1., 64., 1.) * Mat4::look_to_rh(eye, dir, Vec3::Y);
    Frame {
        vp: vp.to_cols_array_2d(),
        inv_vp: vp.inverse().to_cols_array_2d(),
        eye: eye.extend(64.).to_array(),
        extent: [0, 0, e, e],
        ambient: [0.1, 0.1, 0.1, 0.],
        sun_color: [0.7, 0.7, 0.7, 0.],
        water: [2., 20., 1., 0.],
        water_color: [0.015, 0.045, 0.19, 1.],
        ..Frame::zeroed()
    }
}
fn depth_at(f: Frame, p: Vec3) -> f32 {
    let pos = Mat4::from_cols_array_2d(&f.vp) * p.extend(1.);
    pos.z / pos.w
}
fn quad(
    b: &mut Builder,
    positions: [[f32; 3]; 4],
    n: [f32; 3],
    tex: u32,
    flags: u32,
    tint: u32,
) -> u32 {
    let id = b.surface(Surface::new(tex, flags, 0.5, 0.));
    let mut mesh = Mesh::default();
    for (i, p) in positions.into_iter().enumerate() {
        mesh.vertices
            .push(Vertex::new(p, n, [(i % 2) as f32, (i / 2) as f32]).tint(tint, tint));
    }
    if flags & BLEND != 0 {
        mesh.tri_layer([0, 2, 1], id, 1);
        mesh.tri_layer([1, 2, 3], id, 1);
    } else {
        mesh.tri([0, 2, 1], id);
        mesh.tri([1, 2, 3], id);
    }
    b.mesh(&mesh).unwrap().unwrap()
}
fn upright(b: &mut Builder, z: f32, rgb: [f32; 4], flags: u32, texture: u32) -> u32 {
    quad(
        b,
        [[-1., 1., z], [1., 1., z], [-1., -1., z], [1., -1., z]],
        [0., 0., 1.],
        texture,
        flags,
        rgba(rgb),
    )
}
fn put(b: &mut Builder, mesh: u32) {
    b.place(mesh, Transform::IDENTITY).unwrap();
}

struct Harness {
    ray: Lighter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}
impl Harness {
    fn new(mode: Mode, cpu_query: bool) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = if mode == Mode::Hardware && !cpu_query {
            instance
                .enumerate_adapters(wgpu::Backends::VULKAN)
                .into_iter()
                .find(supports_hardware)
                .expect("no physical Vulkan ray-query GPU; this is an opt-in hardware test")
        } else {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("Vulkan test device")
        };
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let features = if cpu_query {
            assert_eq!(adapter.get_info().device_type, wgpu::DeviceType::Cpu);
            assert!(
                !supports_hardware(&adapter),
                "software ICD must never be offered as Hardware RT"
            );
            assert!(
                adapter.features().contains(vulkan::FEATURES),
                "this test requires a software ICD exposing KHR_ray_query"
            );
            vulkan::FEATURES
        } else {
            device_features(&adapter, mode)
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let features = device_features(&adapter, mode);
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Lighter test device"),
                required_features: features,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .unwrap();
        let mut ray = Lighter::new(
            Settings {
                mode,
                lighting: false,
                shadows: false,
                bounces: 0,
                resolution: 100,
                ..Settings::default()
            },
            &adapter,
            &device,
        );
        // Test-only: assert production rejects CPU Vulkan, then exercise the
        // *same* Vulkan descriptors, AS builds, submission and composition.
        if cpu_query {
            ray.has_hardware_device = true;
        }
        Self { ray, device, queue }
    }

    fn pixel(&mut self, frame: Frame, moving: &Packet) -> ([u8; 4], f32) {
        let e = frame.extent[2] as usize;
        self.pixels(frame, moving)[e / 2 * e + e / 2]
    }

    fn pixels(&mut self, frame: Frame, moving: &Packet) -> Vec<([u8; 4], f32)> {
        let e = frame.extent[2];
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let extent = wgpu::Extent3d {
            width: e,
            height: e,
            depth_or_array_layers: 1,
        };
        let new_target = |format| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let color = new_target(format);
        let depth = new_target(wgpu::TextureFormat::Depth32Float);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let row = (4 * e).div_ceil(align) * align;
        let stride = row * e;
        let copy = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride) * 2,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.ray
            .render(
                &self.device,
                &self.queue,
                &mut encoder,
                &color.create_view(&Default::default()),
                &depth.create_view(&Default::default()),
                format,
                frame,
                moving,
            )
            .unwrap();
        for (i, (texture, aspect)) in [
            (&color, wgpu::TextureAspect::All),
            (&depth, wgpu::TextureAspect::DepthOnly),
        ]
        .into_iter()
        .enumerate()
        {
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &copy,
                    layout: wgpu::ImageDataLayout {
                        offset: i as u64 * u64::from(stride),
                        bytes_per_row: Some(row),
                        rows_per_image: Some(e),
                    },
                },
                extent,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        let read = copy.slice(..);
        let (send, recv) = std::sync::mpsc::channel();
        read.map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        crate::gpu::wait_for_gpu(&self.device, recv)
            .unwrap()
            .unwrap();
        let data = read.get_mapped_range();
        (0..e)
            .flat_map(|y| (0..e).map(move |x| (y * row + x * 4) as usize))
            .map(|i| {
                let z = i + stride as usize;
                (
                    data[i..i + 4].try_into().unwrap(),
                    f32::from_le_bytes(data[z..z + 4].try_into().unwrap()),
                )
            })
            .collect()
    }

    fn builds(&self) -> [u64; 3] {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(h) = &self.ray.hardware {
            return h.as_builds;
        }
        [0; 3]
    }
}

fn exercise(mode: Mode, cpu_query: bool) {
    let mut h = Harness::new(mode, cpu_query);
    let mut bank = Textures::default();
    let hole = bank
        .tile([1, 1], 1, textures::Filter::cutout(0.5, false), |_, _| {
            0x00ff_ffff
        })
        .unwrap();
    let frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    let empty = Builder::default().finish();
    let mut b = Builder::default();
    let green = upright(&mut b, -5., [0., 1., 0., 1.], 0, 0);
    put(&mut b, green);
    // More masks than the old retry budget, plus one exactly coincident and
    // one less than a millimetre ahead of the real wall. A rejected primitive
    // does not consume an iteration or hide its eligible neighbor.
    for z in (0..80)
        .map(|i| -1.1 - i as f32 * 0.045)
        .chain([-4.9995, -5.])
    {
        let reject = upright(&mut b, z, [0., 0., 1., 1.], CUTOUT, hole);
        put(&mut b, reject);
    }
    h.ray.install(Ok((b.finish(), bank.clone())));
    let (green, z) = h.pixel(frame, &empty);
    assert_eq!(green, [0, 255, 0, 255]);
    assert!(
        (z - depth_at(frame, Vec3::new(0., 0., -5.))).abs() < 1e-5,
        "{z}"
    );
    let mut animated = Builder::default();
    let red = upright(&mut animated, 0., [1., 0., 0., 1.], 0, 0);
    animated
        .place(
            red,
            Transform {
                position: [0., 0., -3.],
                scale: 1.7,
                cos: 0.98,
                sin: (1.0f32 - 0.98 * 0.98).sqrt(),
            },
        )
        .unwrap();
    let red = animated.finish();
    let (c, z) = h.pixel(frame, &red);
    assert_eq!(c, [255, 0, 0, 255]);
    assert!(
        (z - depth_at(frame, Vec3::new(0., 0., -3.))).abs() < 1e-5,
        "{z}"
    );
    let mut effects = Builder::default();
    let sheet = upright(&mut effects, -3., [1., 0., 0., 0.25], BLEND | EMISSIVE, 0);
    put(&mut effects, sheet);
    let (c, z) = h.pixel(frame, &effects.finish());
    assert!(
        (63..=65).contains(&c[0]) && (190..=192).contains(&c[1]),
        "{c:?}"
    );
    assert!(
        (z - depth_at(frame, Vec3::new(0., 0., -5.))).abs() < 1e-5,
        "a read-only effect overwrote the depth: {z}"
    );

    let mut effects = Builder::default();
    let red = upright(&mut effects, -3., [1., 0., 0., 0.25], BLEND | EMISSIVE, 0);
    let blue = upright(&mut effects, -3., [0., 0., 1., 0.25], BLEND | EMISSIVE, 0);
    put(&mut effects, red);
    put(&mut effects, blue);
    let coplanar = effects.finish();
    let center = h.pixel(frame, &coplanar);
    assert_eq!(
        center.0,
        [48, 143, 64, 255],
        "distinct coincident cards (two triangles each)"
    );
    let builds = h.builds();
    if mode == Mode::Hardware {
        assert!(builds.into_iter().all(|n| n > 0));
    }
    let uploads = h.ray.software.as_ref().map(|s| s.moving_uploads);
    for x in [0., 0.001, -0.001, 0.] {
        let moved = test_frame(Vec3::new(x, 0., 0.), -Vec3::Z, 3);
        let actual = h.pixel(moved, &coplanar);
        assert_eq!(actual.0, center.0, "coverage/order at x={x}");
        assert!((actual.1 - center.1).abs() < 1e-6);
    }
    assert_eq!(h.builds(), builds, "still geometry rebuilt ASes");
    assert_eq!(
        h.ray.software.as_ref().map(|s| s.moving_uploads),
        uploads,
        "still geometry re-uploaded"
    );

    let local_light = |color: [f32; 4], masked| {
        let mut b = Builder::default();
        b.light(Light {
            position_range: [6., 0., -2., 12.],
            color,
        });
        // The tested light must not be pruned just because decorative lights
        // elsewhere in world space also exist in the same packet.
        for i in 0..70 {
            b.light(Light {
                position_range: [100. + i as f32 * 16., 0., 0., 3.],
                color: [1.; 4],
            });
        }
        let block = upright(
            &mut b,
            0.,
            [1.; 4],
            if masked { CUTOUT } else { 0 },
            if masked { hole } else { 0 },
        );
        b.place(block, Transform::translation([3., 0., -3.5]))
            .unwrap();
        b.finish()
    };
    h.ray.settings.lighting = true;
    let bright = h.pixel(frame, &local_light([3.; 4], false));
    let builds = h.builds();
    let dim = h.pixel(frame, &local_light([0.5; 4], false));
    assert!(
        bright.0[1] > dim.0[1] + 30 && bright.1 == dim.1,
        "{bright:?} {dim:?}"
    );
    assert_eq!(h.builds(), builds, "changing light values rebuilt ASes");
    h.ray.settings.shadows = true;
    let dark = h.pixel(frame, &local_light([3.; 4], false));
    let through = h.pixel(frame, &local_light([3.; 4], true));
    assert!(
        dark.0[1] + 30 < bright.0[1] && dark.1 == bright.1,
        "{dark:?} {bright:?}"
    );
    assert_eq!(through, bright, "alpha hole cast a solid point shadow");
    h.ray.settings.shadows = false;
    h.ray.settings.lighting = false;

    // Off-camera blocker in the sun ray (but not in the camera ray). The
    // caster's presence cannot be inferred from primary colour or depth.
    let mut sun_frame = test_frame(Vec3::ZERO, -Vec3::Z, 1);
    sun_frame.sun = Vec3::new(0.8, 0.1, 0.6).normalize().extend(1.).to_array();
    let mut shadow = Builder::default();
    let block = upright(&mut shadow, 0., [1.; 4], 0, 0);
    shadow
        .place(block, Transform::translation([2.4, 0., -3.2]))
        .unwrap();
    let shadow = shadow.finish();
    let bright = h.pixel(sun_frame, &shadow);
    h.ray.settings.shadows = true;
    let dark = h.pixel(sun_frame, &shadow);
    assert!(
        dark.0[1] < bright.0[1] && dark.1 == bright.1,
        "{dark:?} {bright:?}"
    );
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = false;
    let lit = h.pixel(sun_frame, &shadow);
    assert_ne!(lit.0, bright.0);
    assert_eq!(lit.1, bright.1);

    // Neither secondary destination lies on the primary ray. Verify water
    // transmission without reflections, then turn on reflected visibility.
    let mut f = test_frame(Vec3::Y, Vec3::new(0., -0.25, -1.), 3);
    f.options[0] = WATER_MIRROR;
    let water_fixture = |local_visibility| {
        let mut b = Builder::default();
        let water = quad(
            &mut b,
            [
                [-3., 0., -1.],
                [3., 0., -1.],
                [-3., 0., -14.],
                [3., 0., -14.],
            ],
            [0., 1., 0.],
            0,
            WATER,
            rgba([0.2, 0.7, 0.8, 1.]),
        );
        put(&mut b, water);
        let below = quad(
            &mut b,
            [
                [-2., -1., -4.],
                [2., -1., -4.],
                [-2., -1., -8.],
                [2., -1., -8.],
            ],
            [0., 1., 0.],
            0,
            EMISSIVE,
            rgba([0., 1., 0., 1.]),
        );
        put(&mut b, below);
        let above = quad(
            &mut b,
            [
                [-1., 0.9, -6.],
                [1., 0.9, -6.],
                [-1., 0.1, -6.],
                [1., 0.1, -6.],
            ],
            [0., 0., 1.],
            0,
            EMISSIVE | local_visibility,
            rgba([1., 0., 0., 1.]),
        );
        put(&mut b, above);
        b.finish()
    };
    h.ray.install(Ok((water_fixture(0), bank.clone())));
    h.ray.settings.lighting = false;
    let no_bounces = h.pixel(f, &empty);
    h.ray.settings.bounces = 1;
    h.ray.settings.reflections = false;
    let refracted = h.pixel(f, &empty);
    assert!(
        refracted.0[1] > no_bounces.0[1] + 50 && refracted.0[0] < 5,
        "{no_bounces:?} {refracted:?}"
    );
    h.ray.settings.reflections = true;
    let reflected = h.pixel(f, &empty);
    assert!(
        reflected.0[0] > refracted.0[0] + 40 && reflected.0[1] < refracted.0[1],
        "{reflected:?} {refracted:?}"
    );
    assert!(
        (reflected.1 - depth_at(f, Vec3::new(0., 0., -4.))).abs() < 1e-5,
        "{reflected:?}"
    );
    h.ray.install(Ok((water_fixture(NO_REFLECT), bank.clone())));
    let hidden = h.pixel(f, &empty);
    assert!(
        hidden.0[0] + 40 < reflected.0[0],
        "{hidden:?} {reflected:?}"
    );

    let mut foliage = Builder::default();
    let texture = bank
        .tile([8, 8], 1, textures::Filter::cutout(0.5, false), |x, y| {
            if (x + y) % 2 == 0 {
                0xff00_00ff
            } else {
                0x00ff_0000
            }
        })
        .unwrap();
    let leaf = upright(&mut foliage, -2., [1.; 4], CUTOUT, texture);
    let background = upright(&mut foliage, -2.001, [0., 1., 0., 1.], 0, 0);
    put(&mut foliage, leaf);
    put(&mut foliage, background);
    h.ray.settings.bounces = 0;
    h.ray.install(Ok((foliage.finish(), bank.clone())));
    let sample = |x| test_frame(Vec3::new(x, 0., 0.), -Vec3::Z, 31);
    let far = h.pixels(sample(0.), &empty);
    let red = far.iter().filter(|p| p.0 == [255, 0, 0, 255]).count();
    let green = far.iter().filter(|p| p.0 == [0, 255, 0, 255]).count();
    assert_eq!(
        red + green,
        far.len(),
        "transparent RGB fringe or missing depth"
    );
    assert!(
        (420..=540).contains(&red),
        "checkerboard 50% coverage became {red}/{}",
        far.len()
    );
    assert_eq!(
        h.pixels(sample(0.), &empty),
        far,
        "cutout pattern changed on a still frame"
    );
    let moved = h.pixels(sample(0.004), &empty);
    assert!(
        moved
            .iter()
            .zip(&far)
            .all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < 1e-6),
        "minified foliage changed under subpixel motion"
    );

    // Two 50% sheets should leave about 25% of the ground visible, not 50%
    // (correlated masks) or 0% (complementary masks at low sample counts).
    let blue_texture = bank
        .tile([2, 1], 1, textures::Filter::cutout(0.5, false), |x, _| {
            if x == 0 {
                0xffff_0000
            } else {
                0
            }
        })
        .unwrap();
    let mut stacked = Builder::default();
    for (z, tint, flags, tex) in [
        (-2., [1.; 4], CUTOUT, texture),
        (-2.2, [1.; 4], CUTOUT, blue_texture),
        (-2.4, [0., 1., 0., 1.], 0, 0),
    ] {
        let mesh = upright(&mut stacked, z, tint, flags, tex);
        put(&mut stacked, mesh);
    }
    h.ray.install(Ok((stacked.finish(), bank)));
    h.ray.settings.samples = 4;
    let layers = h.pixels(sample(0.), &empty);
    for (i, expected) in [0.50, 0.25, 0.25].into_iter().enumerate() {
        // The harness uses Rgba8Unorm, so this readback is already linear.
        let mean =
            layers.iter().map(|p| f32::from(p.0[i])).sum::<f32>() / (255. * layers.len() as f32);
        assert!((mean - expected).abs() < 0.055, "channel {i}: {mean}");
    }
}

#[test]
#[ignore = "requires a Vulkan adapter (lavapipe is sufficient)"]
fn lighter_software_pixels() {
    exercise(Mode::Software, false);
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
#[ignore = "requires a physical GPU with Vulkan KHR_ray_query"]
fn lighter_hardware_pixels() {
    exercise(Mode::Hardware, false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a recent lavapipe exposing KHR_ray_query; this is not GPU RT-core validation"]
fn lighter_vulkan_interop_on_cpu() {
    exercise(Mode::Hardware, true);
}
