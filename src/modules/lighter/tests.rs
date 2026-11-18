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
    let mut at = 5;
    while at < shader.len() {
        opcodes.push(shader[at] & 0xffff);
        at += (shader[at] >> 16) as usize;
    }
    // SPV_KHR_ray_query: OpRayQueryInitializeKHR and OpRayQueryProceedKHR.
    assert!(opcodes.contains(&4473) && opcodes.contains(&4477));
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
    mesh.tri([0, 2, 1], id);
    mesh.tri([1, 2, 3], id);
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
        let row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
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
        self.device.poll(wgpu::Maintain::Wait);
        recv.recv().unwrap().unwrap();
        let data = read.get_mapped_range();
        let index = ((e / 2) * row + (e / 2) * 4) as usize;
        let z = index + stride as usize;
        (
            data[index..index + 4].try_into().unwrap(),
            f32::from_le_bytes(data[z..z + 4].try_into().unwrap()),
        )
    }
}

fn exercise(mode: Mode, cpu_query: bool) {
    let mut h = Harness::new(mode, cpu_query);
    let mut bank = Textures::default();
    let hole = bank.tile([1, 1], 1, |_, _| 0x00ff_ffff).unwrap();
    let frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    let empty = Builder::default().finish();
    let mut b = Builder::default();
    let green = upright(&mut b, -5., [0., 1., 0., 1.], 0, 0);
    put(&mut b, green);
    let reject = upright(&mut b, -2., [0., 0., 1., 1.], CUTOUT, hole);
    put(&mut b, reject);
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
