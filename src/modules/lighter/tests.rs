#[path = "optimization_tests.rs"]
mod optimizations;
#[path = "residency_tests.rs"]
mod residency_tests;

use super::*;
use glam::{Mat4, Vec3};
use scene::*;

#[test]
fn reload_world_button_applies_pending_mode_without_changing_the_live_renderer() {
    let ctx = egui::Context::default();
    let mut lighter = Lighter::default();
    lighter.settings.resolution = 61;
    lighter.stage_saved_mode(Mode::Hardware);
    lighter.pending_primary = Some(true);
    assert_eq!(lighter.settings_for_disk().mode, Mode::Hardware);
    assert_eq!(lighter.settings_for_disk().resolution, 61);
    let mut draw = |events| {
        let mut result = None;
        let output = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| result = lighter.ui(ui));
            },
        );
        (result, output)
    };
    let (result, output) = draw(Vec::new());
    assert!(result.is_none());
    let button = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Reload World" => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .expect("Reload World button");
    let click = |pressed| egui::Event::PointerButton {
        pos: button,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    assert!(draw(vec![egui::Event::PointerMoved(button), click(true)])
        .0
        .is_none());
    let next = draw(vec![click(false)])
        .0
        .expect("reload request from click");
    assert_eq!(next.mode, Mode::Hardware);
    assert_eq!(next.resolution, 61);
    assert!(next.hybrid_primary);
    assert!(!lighter.settings.hybrid_primary);
    assert_eq!(lighter.settings.mode, Mode::Off);
    assert_eq!(lighter.settings_for_disk().mode, Mode::Hardware);
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
fn naga_validates_both_paths_and_emits_native_ray_queries() {
    use naga_rt as naga;
    for src in [
        trace_source(SOFTWARE, false, false),
        trace_source(SOFTWARE, true, true),
        [PIXELS, post::SHADER].concat(),
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
    let shader = vulkan::shader(&trace_source(HARDWARE, true, true))
        .expect("Vulkan ray-query WGSL to SPIR-V");
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

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
fn compiles_raster_primary_sibling_without_ray_query_capability() {
    use naga_rt::ShaderStage;
    let raster = raster_source();
    for (stage, name) in [
        (ShaderStage::Vertex, "visibility_vertex"),
        (ShaderStage::Fragment, "visibility_fragment"),
    ] {
        let words = vulkan::shader_entry(&raster, stage, name).unwrap();
        let mut next = 5;
        while next < words.len() {
            let op = words[next] & 0xffff;
            if op == 17 {
                assert_ne!(words[next + 1], 4472, "SPIR-V RayQueryKHR capability");
            }
            next += (words[next] >> 16) as usize;
        }
    }
    let shader = trace_source(HARDWARE, true, true);
    let words = vulkan::shader_entry(&shader, ShaderStage::Compute, "render_hybrid").unwrap();
    assert_eq!(words[0], 0x0723_0203);
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
        b.light(Light::point([i as f32 * 8., 0., 0., 2.], [1.; 4]));
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
    assert!(
        visited <= 19,
        "wide tree checked {visited} nodes for one localized light"
    );
}

fn test_frame(eye: Vec3, dir: Vec3, e: u32) -> Frame {
    let up = if dir.normalize().y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let vp = Mat4::perspective_rh(0.7, 1., 64., 1.) * Mat4::look_to_rh(eye, dir, up);
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
    quad_material(b, positions, n, Surface::new(tex, flags, 0.5, 0.), tint)
}
fn quad_material(
    b: &mut Builder,
    positions: [[f32; 3]; 4],
    n: [f32; 3],
    material: Surface,
    tint: u32,
) -> u32 {
    let id = b.surface(material);
    let mut mesh = Mesh::default();
    for (i, p) in positions.into_iter().enumerate() {
        mesh.vertices
            .push(Vertex::new(p, n, [(i % 2) as f32, (i / 2) as f32]).tint(tint, tint));
    }
    if material.flags & BLEND != 0 {
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

pub(super) struct Harness {
    pub ray: Lighter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}
impl Harness {
    pub(super) fn new(mode: Mode, cpu_query: bool) -> Self {
        Self::with_device(mode, cpu_query, |adapter, desc| {
            pollster::block_on(adapter.request_device(desc, None)).unwrap()
        })
    }

    pub(super) fn with_device(
        mode: Mode,
        cpu_query: bool,
        open: impl FnOnce(&wgpu::Adapter, &wgpu::DeviceDescriptor<'_>) -> (wgpu::Device, wgpu::Queue),
    ) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        Self::on_instance(&instance, mode, cpu_query, open)
    }

    pub(super) fn on_instance(
        instance: &wgpu::Instance,
        mode: Mode,
        cpu_query: bool,
        open: impl FnOnce(&wgpu::Adapter, &wgpu::DeviceDescriptor<'_>) -> (wgpu::Device, wgpu::Queue),
    ) -> Self {
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
        let (device, queue) = open(
            &adapter,
            &wgpu::DeviceDescriptor {
                label: Some("Lighter test device"),
                required_features: features
                    | (adapter.features()
                        & (wgpu::Features::TIMESTAMP_QUERY
                            | wgpu::Features::TEXTURE_COMPRESSION_BC)),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
        );
        let mut ray = Lighter::new(
            Settings {
                mode,
                lighting: false,
                shadows: false,
                bounces: 0,
                resolution: 100,
                denoiser: Denoiser::None,
                soft_sun: false,
                single_path: false, // deterministic split for the legacy optical probes
                progressive: false,
                ..Settings::default()
            },
            &adapter,
            &device,
        )
        .for_capture();
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
        self.pixels_with_camera(frame, moving, None)
    }

    pub(super) fn pixels_with_camera(
        &mut self,
        frame: Frame,
        moving: &Packet,
        camera: Option<TemporalCamera>,
    ) -> Vec<([u8; 4], f32)> {
        let e = frame.extent[2];
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let extent = wgpu::Extent3d {
            width: e,
            height: e,
            depth_or_array_layers: 1,
        };
        let color = self.target(format, extent);
        let depth = self.target(wgpu::TextureFormat::Depth32Float, extent);
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
        let mut render = || {
            self.ray.render(
                &self.device,
                &self.queue,
                &mut encoder,
                &color.create_view(&Default::default()),
                &depth.create_view(&Default::default()),
                format,
                frame,
                moving,
                camera,
            )
        };
        if !render().unwrap() {
            // Tests on the live path emulate later redraws here, using the
            // harness's wait rather than changing the renderer into capture mode.
            crate::gpu::wait_for_capture(&self.device, || {
                render().map(|ready| ready.then_some(()))
            })
            .unwrap();
        }
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
        self.ray.profile_submitted();
        let read = copy.slice(..);
        let (send, recv) = std::sync::mpsc::channel();
        read.map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        crate::gpu::wait_for_capture_callback(&self.device, recv)
            .unwrap()
            .unwrap();
        self.ray.profile_collect(&self.queue);
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

    fn history_state(&self, index: usize) -> (u32, u32) {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter history probe"),
            size: PIXEL_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let history = self.ray.post.as_ref().unwrap().last_history();
        encoder.copy_buffer_to_buffer(history, index as u64 * PIXEL_BYTES, &buffer, 0, PIXEL_BYTES);
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            let _ = tx.send(v);
        });
        crate::gpu::wait_for_capture_callback(&self.device, rx)
            .unwrap()
            .unwrap();
        let data = slice.get_mapped_range();
        let pixel = bytemuck::from_bytes::<Pixel>(&data);
        (pixel.albedo_history, pixel.owner)
    }

    fn target(&self, format: wgpu::TextureFormat, size: wgpu::Extent3d) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
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
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    if mode == Mode::Hardware {
        h.ray.capture = false;
        assert!(
            !h.ray.prepare_frame(&h.device, 3, 3).unwrap(),
            "initial image must defer"
        );
    }
    let (green, z) = h.pixel(frame, &empty);
    assert_eq!(green, [0, 255, 0, 255]);
    assert!(
        (z - depth_at(frame, Vec3::new(0., 0., -5.))).abs() < 1e-5,
        "{z}"
    );
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    if mode == Mode::Hardware {
        assert!(
            !h.ray.prepare_frame(&h.device, 5, 5).unwrap(),
            "replacement image must defer"
        );
        let resized = test_frame(Vec3::ZERO, -Vec3::Z, 5);
        let (pixel, depth) = h.pixel(resized, &empty);
        assert_eq!(pixel, green);
        assert!((depth - z).abs() < 1e-6);
        h.ray.capture = true;
    }
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
        b.light(Light::point([6., 0., -2., 12.], color));
        // The tested light must not be pruned just because decorative lights
        // elsewhere in world space also exist in the same packet.
        for i in 0..70 {
            b.light(Light::point([100. + i as f32 * 16., 0., 0., 3.], [1.; 4]));
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
    exercise_transport(&mut h);
    exercise_denoiser_edges(&mut h);
    exercise_spatial_upscale(&mut h);
    exercise_stable_primary_water(&mut h);
    exercise_sun_and_history(&mut h);
    exercise_temporal_animation(&mut h);
    residency_tests::moving_occluder_history(&mut h);
    exercise_gloss(&mut h);
    exercise_streamed_bmds(&mut h);
    exercise_bmd_chunk_join(&mut h);
    exercise_terrain_seam(&mut h);
    exercise_accel_reuse(&mut h);
    exercise_off_camera_pose(&mut h);
    exercise_rigid_hanger(&mut h);
    exercise_resource_reuse(&mut h);
    exercise_texture_prefix_replacement(&mut h);
    exercise_ray_texture_filter(&mut h);
    exercise_material_channels(&mut h);
    exercise_vegetation(&mut h);
    exercise_water_art(&mut h);
    exercise_effect_placement(&mut h);
    exercise_finite_emitters(&mut h);
    exercise_dielectric_and_metal(&mut h);
    exercise_single_path_estimators(&mut h);
    exercise_stationary_sun(&mut h);
    exercise_media_segments(&mut h);
    exercise_reference_lighting(&mut h);
    optimizations::exercise(&mut h);
    if let Some(t) = &h.ray.timings {
        assert!(t.collected > 0);
    }
    if mode == Mode::Software {
        exercise_bsdf_energy(&h);
    }
    if mode == Mode::Hardware {
        exercise_backend_switch(&mut h);
    }
    exercise_chunk_scaling(&mut h);
    if mode == Mode::Software {
        exercise_reprojection_shader(&h);
    }
}

fn exercise_backend_switch(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings.lighting = false;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.samples = 1;
    h.ray.settings.resolution = 100;
    let mut b = Builder::default();
    let red = upright(&mut b, -4., [1., 0., 0., 1.], EMISSIVE, 0);
    put(&mut b, red);
    h.ray.install(Ok((b.finish(), Textures::default())));
    let moving = Builder::default().finish();
    let frame = test_frame(Vec3::ZERO, Vec3::NEG_Z, 3);
    let original = h.pixel(frame, &moving);
    assert_eq!(original.0, [255, 0, 0, 255]);
    h.ray.settings.mode = Mode::Software;
    h.ray.reset_history();
    // Resize while the Software target owns presentation. Switch back to
    // the retained RT backend at the enlarged extent, with its own image.
    let larger = test_frame(Vec3::ZERO, Vec3::NEG_Z, 21);
    let software = h.pixel(larger, &moving);
    h.ray.settings.mode = Mode::Hardware;
    h.ray.reset_history();
    let hardware = h.pixel(larger, &moving);
    assert_eq!(software.0, original.0);
    assert_eq!(hardware.0, original.0);
    assert!((hardware.1 - software.1).abs() < 1.0e-6);
    h.ray.settings = saved;
    eprintln!("Lighter: Hardware / Software / Hardware resize kept color and reverse-Z");
}

fn exercise_ray_texture_filter(h: &mut Harness) {
    use textures::Filter;
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = false;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.samples = 1;
    h.ray.settings.resolution = 100;
    let empty = Builder::default().finish();
    const SIDE: u32 = 33;

    // Half black / half white at level zero must interpolate to 0.5 linear,
    // not decode a prefiltered encoded 0.5 to 0.214.
    let mut bank = Textures::default();
    let ramp = bank
        .tile([2, 1], 2, Filter::Color, |x, _| {
            if x == 0 {
                0xff00_0000
            } else {
                u32::MAX
            }
        })
        .unwrap();
    let mut b = Builder::default();
    let quad = upright(&mut b, -3., [1.; 4], 0, ramp);
    put(&mut b, quad);
    h.ray.install(Ok((b.finish(), bank)));
    assert_radiance(
        "linear-light bilinear",
        h.pixel(test_frame(Vec3::ZERO, -Vec3::Z, SIDE), &empty),
        [0.5; 3],
    );

    // Leave the deliberately green authored lower mip distinct from the red
    // top when a real ray minifies the wall. There is no generated third mip.
    let mut bank = Textures::default();
    let tile = textures::Tile::levels(
        [
            ([64, 32], &[255, 0, 0, 255].repeat(64 * 32)[..]),
            ([32, 16], &[0, 255, 0, 255].repeat(32 * 16)[..]),
        ],
        64,
        Filter::Color,
    )
    .unwrap();
    let addr = bank.insert(&tile).unwrap();
    let mut b = Builder::default();
    let quad = upright(&mut b, -10., [1.; 4], 0, addr);
    put(&mut b, quad);
    h.ray.install(Ok((b.finish(), bank)));
    assert_radiance(
        "authored mip",
        h.pixel(test_frame(Vec3::ZERO, -Vec3::Z, SIDE), &empty),
        [0., 1., 0.],
    );

    // A repeating brick wall at 3.6 degrees from tangent. Measure jitter
    // along the wide footprint axis and horizontal mortar detail across it.
    // All probes hit the wall (depth check), so sky cannot inflate contrast.
    let mut bank = Textures::default();
    let brick = bank
        .tile([128, 128], 128, Filter::Color, |x, y| {
            let mortar = y % 32 < 8 || (x + ((y / 32) & 1) * 8) % 16 < 2;
            u32::from_le_bytes(if mortar {
                [40, 44, 48, 255]
            } else {
                [220, 72, 32, 255]
            })
        })
        .unwrap();
    let mut b = Builder::default();
    let material = b.surface(Surface::new(brick, 0, 0., 0.));
    let mut mesh = Mesh::default();
    for [x, y] in [[-20., 4.], [20., 4.], [-20., -4.], [20., -4.]] {
        mesh.vertices.push(Vertex::new(
            [x, y, -4.],
            [0., 0., 1.],
            [x * 0.125, y * 0.0625],
        ));
    }
    mesh.tri([0, 2, 1], material);
    mesh.tri([1, 2, 3], material);
    b.world_mesh(&mesh).unwrap();
    h.ray.install(Ok((b.finish(), bank)));
    let mut measures = Vec::new();
    for anisotropy in [1, 8, 16] {
        h.ray.settings.anisotropy = anisotropy;
        let sweep = |h: &mut Harness, y| -> [u8; 8] {
            std::array::from_fn(|i| {
                let x = i as f32 / 16.;
                let eye = Vec3::new(x - 16., y, -3.);
                let frame = test_frame(eye, Vec3::new(16., 0., -1.), SIDE);
                let (c, depth) = h.pixel(frame, &empty);
                assert!(
                    (depth - depth_at(frame, Vec3::new(x, y, -4.))).abs() < 2e-6,
                    "missed the brick wall"
                );
                c[0]
            })
        };
        let fill = sweep(h, 2.5);
        let mortar = sweep(h, 0.5);
        let contrast = fill.iter().map(|&x| f32::from(x)).sum::<f32>() / 8.
            - mortar.iter().map(|&x| f32::from(x)).sum::<f32>() / 8.;
        let shimmer = fill.iter().max().unwrap() - fill.iter().min().unwrap();
        measures.push((contrast, shimmer));
    }
    assert!(
        measures[0].0.abs() < 5.,
        "isotropic reference was not minified: {measures:?}"
    );
    for (contrast, shimmer) in &measures[1..] {
        assert!(
            *contrast > 35. && *shimmer < 14,
            "grazing brick wall lost detail or shimmered: {measures:?}"
        );
    }
    assert!(
        measures[2].0 >= measures[1].0,
        "16× removed cross-axis detail: {measures:?}"
    );
    h.ray.settings.anisotropy = 8;
    eprintln!("Lighter: brick contrast and red-channel pan range at 1×/8×/16×: {measures:?}");
}

fn exercise_material_channels(h: &mut Harness) {
    use textures::Filter;
    let empty = Builder::default().finish();
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = false;
    h.ray.settings.soft_sun = false;
    h.ray.settings.reflections = false;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = false;
    h.ray.settings.relight_baked = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.samples = 1;
    h.ray.settings.resolution = 100;
    let mut day = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    day.sun[3] = 1.;
    day.sun_color = [0.; 4];
    day.ambient = [0.4; 4];
    let mut night = day;
    night.sun[3] = 0.;
    night.material_time[0] = 1.;
    let srgb = |v: f32| ((v + 0.055) / 1.055).powf(2.4);
    let mut b = Builder::default();
    let mat = b.surface(
        Surface::new(0, BAKED | BAKED_COLOR_ONLY, 0., 0.).with_lighting(
            [0.5, 0., 0., 1.],
            [0.5; 4],
            [0.2, 0., 0., 1.],
        ),
    );
    let mut mesh = Mesh::default();
    for p in [
        [-2., 2., -2.],
        [2., 2., -2.],
        [-2., -2., -2.],
        [2., -2., -2.],
    ] {
        mesh.vertices.push(
            Vertex::new(p, [0., 0., 1.], [0.; 2])
                .tint(rgba([0.65, 0., 0., 1.]), rgba([0., 0., 0.28, 1.])),
        );
    }
    mesh.tri([0, 2, 1], mat);
    mesh.tri([1, 2, 3], mat);
    b.world_mesh(&mesh).unwrap();
    h.ray.install(Ok((b.finish(), Textures::default())));
    assert_radiance(
        "day BMD vertex bake",
        h.pixel(day, &empty),
        [srgb(0.65), 0., 0.],
    );
    assert_radiance(
        "night BMD vertex bake",
        h.pixel(night, &empty),
        [0., 0., srgb(0.28)],
    );
    h.ray.settings.relight_baked = true;
    let relit = h.pixel(day, &empty);
    assert_radiance(
        "unbaked BMD, authored ambient and emission",
        relit,
        [0.40, 0.20, 0.20],
    );
    assert_eq!(
        h.pixel(night, &empty),
        relit,
        "baked vertex tint leaked into relighting"
    );
    let sunlit = Frame {
        sun: [0., 0.6, 0.8, 1.],
        sun_color: [0.5; 4],
        ..day
    };
    assert_radiance(
        "BMD reflectance is independent of ambient and emission",
        h.pixel(sunlit, &empty),
        [0.60, 0.20, 0.20],
    );
    h.ray.settings.relight_baked = false;

    // Neighboring terrain areas. The grass straddles the join; its baked
    // light uses the area center and clamps the world UV to that tile.
    let mut bank = Textures::default();
    let red = bank
        .tile([1, 1], 1, Filter::Data, |_, _| rgba([0.25, 0., 0., 1.]))
        .unwrap();
    let blue = bank
        .tile([1, 1], 1, Filter::Data, |_, _| rgba([0., 0., 0.35, 1.]))
        .unwrap();
    let green = bank
        .tile([1, 1], 1, Filter::Data, |_, _| rgba([0., 0.5, 0., 1.]))
        .unwrap();
    let area = bank.reserve(8).unwrap();
    let layer = bank.reserve(5).unwrap();
    bank.write_words(
        area,
        &[layer as u32, 1, red, blue, layer as u32, 1, green, green],
    );
    bank.write_words(layer, &[0, 0, 1, 1.0f32.to_bits(), 1.0f32.to_bits()]);
    for frame in [&mut day, &mut night] {
        frame.options[0] = BAKED_LIGHTMAPS | GRASS_AREA_LIGHTMAPS;
        frame.options[3] = area as u32;
        frame.terrain = [8., 4., 1., 4.];
    }
    let ground = |h: &mut Harness, flags, x: f32, area: f32| {
        let mut b = Builder::default();
        let mesh = upright(&mut b, -1.5, [1.; 4], flags, 0);
        b.place_grass(mesh, Transform::translation([x, 0., 0.]), [area, 0.])
            .unwrap();
        h.ray.install(Ok((b.finish(), bank.clone())));
    };
    ground(h, TERRAIN, -1., -1.);
    // Rebuild the camera matrices as well as the origin.
    let at = |frame: Frame, eye| {
        let view = test_frame(Vec3::new(eye, 0., 0.), -Vec3::Z, 3);
        Frame {
            vp: view.vp,
            inv_vp: view.inv_vp,
            eye: view.eye,
            ..frame
        }
    };
    assert_radiance(
        "day terrain pair",
        h.pixel(at(day, -0.5), &empty),
        [srgb(0.5), 0., 0.],
    );
    assert_radiance(
        "night terrain pair",
        h.pixel(at(night, -0.5), &empty),
        [0., 0., srgb(0.7)],
    );
    let mut dusk = day;
    dusk.material_time[0] = 0.5;
    assert_radiance(
        "terrain twilight uses terrain night factor, not the sun strength",
        h.pixel(at(dusk, -0.5), &empty),
        [srgb(0.25), 0., srgb(0.35)],
    );
    h.ray.settings.relight_baked = true;
    assert_radiance(
        "relit terrain ignores T2LM",
        h.pixel(at(night, -0.5), &empty),
        [0.4; 3],
    );
    h.ray.settings.relight_baked = false;
    let mut no_maps = night;
    no_maps.options[0] &= !BAKED_LIGHTMAPS;
    assert_radiance(
        "no lightmaps uses local lighting",
        h.pixel(at(no_maps, -0.5), &empty),
        [0.4; 3],
    );
    ground(h, TERRAIN, 1., 1.);
    assert_radiance(
        "day-only terrain night fallback",
        h.pixel(at(night, 0.5), &empty),
        [0., 1., 0.],
    );
    ground(h, GRASS_MATERIAL, 1., -1.);
    assert_radiance(
        "grass area-center day",
        h.pixel(at(day, 0.5), &empty),
        [srgb(0.5), 0., 0.],
    );
    assert_radiance(
        "grass area-center night",
        h.pixel(at(night, 0.5), &empty),
        [0., 0., srgb(0.7)],
    );
    ground(h, GRASS_MATERIAL, 1., 1.);
    assert_radiance(
        "grass never treats copied day as a night map",
        h.pixel(at(night, 0.5), &empty),
        [1.; 3],
    );
    h.ray.settings.relight_baked = true;
    assert_radiance(
        "relit grass omits baked light",
        h.pixel(at(night, 0.5), &empty),
        [0.4; 3],
    );
    h.ray.settings.relight_baked = false;

    // Compare the same asymmetric mask in the old RGBA and new packed R8
    // layouts. Use grazing and distant footprints as well as a close sample.
    h.ray.settings.lighting = false;
    let mask: Vec<_> = (0..32 * 32)
        .map(|i| ((i / 32 * 7 + i % 32 * 5) % 247 + 4) as u8)
        .collect();
    let mut probes = Vec::new();
    for packed in [false, true] {
        let mut bank = Textures::default();
        let red = bank
            .tile([1; 2], 1, Filter::Color, |_, _| 0xff0000ff)
            .unwrap();
        let green = bank
            .tile([1; 2], 1, Filter::Color, |_, _| 0xff00ff00)
            .unwrap();
        let addr = if packed {
            bank.insert(&textures::Tile::mask(32, 32, &mask).unwrap())
                .unwrap()
        } else {
            bank.tile([32; 2], 32, Filter::Data, |x, y| {
                u32::from(mask[(y * 32 + x) as usize]) * 0x01010101
            })
            .unwrap()
        };
        let area = bank.reserve(4).unwrap();
        let layer = bank.reserve(10).unwrap();
        bank.write_words(area, &[layer as u32, 2, 0, 0]);
        let one = 1.0f32.to_bits();
        bank.write_words(layer, &[red, 0, 1, one, one, green, addr, 1, one, one]);
        let mut b = Builder::default();
        let plane = quad(
            &mut b,
            [[-4., 0., 4.], [4., 0., 4.], [-4., 0., -4.], [4., 0., -4.]],
            [0., 1., 0.],
            0,
            TERRAIN,
            u32::MAX,
        );
        put(&mut b, plane);
        h.ray.install(Ok((b.finish(), bank)));
        let mut trace = |eye, dir| {
            let mut frame = test_frame(eye, dir, 3);
            frame.terrain = [4., 4., 1., 4.];
            frame.options[3] = area as u32;
            h.pixel(frame, &empty)
        };
        probes.push([
            // Keep probes strictly beyond the near plane and off the
            // shared triangle edge; this test is about blend sampling.
            trace(Vec3::new(-0.73, 2., 0.33), -Vec3::Y),
            trace(Vec3::new(0.32, 25., -0.14), -Vec3::Y),
            trace(
                Vec3::new(-12., 1., -0.63),
                Vec3::new(12., -1., 0.).normalize(),
            ),
        ]);
    }
    for (probe, ((old, depth), (packed, packed_depth))) in
        probes[0].into_iter().zip(probes[1]).enumerate()
    {
        assert!(
            old[0] > 0 && old[1] > 0 && old[2] == 0 && depth > 0.,
            "expected blended terrain at probe {probe}: {old:?}, {depth}"
        );
        assert_eq!(depth, packed_depth);
        assert!(
            old.into_iter().zip(packed).all(|(a, b)| a.abs_diff(b) <= 1),
            "packed mask changed the terrain blend: {old:?} vs {packed:?}"
        );
    }
    h.ray.settings.lighting = true;

    // Normal map steers the shading normal, not the ray-plane side/depth.
    let mut bank = Textures::default();
    let map = bank
        .tile([1, 1], 1, Filter::Data, |_, _| rgba([0.8, 0.5, 0.9, 1.]))
        .unwrap();
    // The same tangent normal in ATI2, with no stored blue channel. Keep
    // an actual decoded mip in this test rather than manufacturing RGB.
    let mut bc5 = vec![0; 128];
    bc5[..4].copy_from_slice(b"DDS ");
    bc5[84..88].copy_from_slice(b"ATI2");
    for (at, v) in [(4, 124u32), (12, 1), (16, 1), (76, 32), (80, 4)] {
        bc5[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    bc5.extend([204, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0]);
    let (levels, _) = crate::model_texture_gpu::decode_for_rays(&bc5).unwrap();
    let rg = bank.images(&levels, 1, Filter::NormalRg).unwrap();
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    frame.ambient = [0.; 4];
    frame.sun = [0.6, 0.2, (0.6f32).sqrt(), 1.];
    frame.sun_color = [1.; 4];
    let mut probes = Vec::new();
    for normal_map in [0, map, rg] {
        let mut b = Builder::default();
        let mut mat = Surface::new(0, SINGLE_SIDED, 0., 0.);
        mat.normal_map = normal_map;
        let mesh = quad_material(
            &mut b,
            [
                [-1., 1., -2.],
                [1., 1., -2.],
                [-1., -1., -2.],
                [1., -1., -2.],
            ],
            [0., 0., 1.],
            mat,
            u32::MAX,
        );
        put(&mut b, mesh);
        h.ray.install(Ok((b.finish(), bank.clone())));
        probes.push(h.pixel(frame, &empty));
        assert_eq!(
            h.pixel(test_frame(-4. * Vec3::Z, Vec3::Z, 3), &empty).1,
            0.,
            "single-sided material kept a back-facing hit"
        );
    }
    assert!(
        probes[1].0[0] > probes[0].0[0] + 30 && probes[1].1 == probes[0].1,
        "NRF basis or side incorrect: {probes:?}"
    );
    assert_eq!(probes[2].1, probes[1].1);
    assert!(
        probes[2]
            .0
            .into_iter()
            .zip(probes[1].0)
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "reconstructed BC5 tangent normal: {probes:?}"
    );

    // Real posed-skin producer, including its part-specific face overrides.
    // The body alpha is color data; a transparent base is still opaque.
    use crate::model_asset::FaceShader;
    let (mut asset, _) = crate::model_draw_fixture::fixture();
    asset.skins[0].data.materials[0].diffuse = [1.; 4];
    let skin = &mut asset.skins[0].data.meshes[0];
    skin.vertices = [[-2., 2., -3.], [2., 2., -3.], [0., -2., -3.]]
        .into_iter()
        .map(|position| crate::skin::Vertex {
            position,
            normal: [0., 0., 1.],
            uv: [0.5; 2],
            weights: [1., 0., 0., 0.],
            joints: [0; 4],
        })
        .collect();
    skin.indices = vec![0, 2, 1];
    h.ray.settings.lighting = false;
    for (stage, base_alpha, expected) in [
        (FaceShader::Body { color: 0xffff0000 }, 0, [1.; 3]),
        (FaceShader::Body { color: 0xffff0000 }, 255, [1., 0., 0.]),
        (
            FaceShader::Eye {
                color: 0xff00ff00,
                skin_color: 0xffff0000,
                highlight: "iris".into(),
            },
            255,
            [srgb(127. / 255.), srgb(128. / 255.), 0.],
        ),
        (
            FaceShader::EyeBall {
                color: 0xffff0000,
                highlight: "iris".into(),
            },
            255,
            [0., 0., 1.],
        ),
    ] {
        asset.face_shaders.insert((0, 0), stage);
        let pose = asset.bind_pose(Mat4::IDENTITY).unwrap();
        let meshes = asset.meshes_from_pose(&pose).unwrap();
        let mut bank = Textures::default();
        let mut moving = super::model_feed::Moving::new(&mut bank);
        moving
            .model(
                "walker",
                1,
                super::model_feed::Posed {
                    asset: &asset,
                    pose: &pose,
                    meshes: &meshes,
                    attached: false,
                },
                |_| true,
                &mut |path| {
                    let rgba = match path {
                        "iris" => [255, 255, 255, 128],
                        "textures/face/eyereflect.tga" => [0, 0, 255, 255],
                        _ => [255, 255, 255, base_alpha],
                    };
                    Ok((
                        vec![image::RgbaImage::from_pixel(1, 1, image::Rgba(rgba))],
                        true,
                    ))
                },
            )
            .unwrap();
        let packet = moving.finish_cached(&mut AccelCache::default()).unwrap();
        h.ray.install(Ok((Builder::default().finish(), bank)));
        assert_radiance(
            "posed skin/eye/body stages",
            h.pixel(frame, &packet),
            expected,
        );
    }
    asset.face_shaders.clear();
    asset.skins[0].data.materials[0].diffuse = [1., 0., 0., 0.5];
    let pose = asset.bind_pose(Mat4::IDENTITY).unwrap();
    let meshes = asset.meshes_from_pose(&pose).unwrap();
    let mut bank = Textures::default();
    let mut moving = super::model_feed::Moving::new(&mut bank);
    moving
        .model(
            "walker",
            1,
            super::model_feed::Posed {
                asset: &asset,
                pose: &pose,
                meshes: &meshes,
                attached: false,
            },
            |_| true,
            &mut |_| {
                Ok((
                    vec![image::RgbaImage::from_pixel(
                        1,
                        1,
                        image::Rgba([255, 255, 255, 128]),
                    )],
                    true,
                ))
            },
        )
        .unwrap();
    let packet = moving.finish_cached(&mut AccelCache::default()).unwrap();
    let mut b = Builder::default();
    let background = upright(&mut b, -5., [0., 0., 1., 1.], EMISSIVE, 0);
    put(&mut b, background);
    h.ray.install(Ok((b.finish(), bank)));
    let pixel = h.pixel(frame, &packet);
    assert_radiance(
        "material and texture alpha transmit the background",
        pixel,
        [0.25, 0., 0.75],
    );
    assert!((pixel.1 - depth_at(frame, -5. * Vec3::Z)).abs() < 1e-6);
    eprintln!("Lighter: both authored bake endpoints, relighting, face stages, sidedness and normal map passed");
}

fn exercise_reference_lighting(h: &mut Harness) {
    let old = h.ray.settings;
    h.ray.settings.reference = true;
    let mut bank = Textures::default();
    let tile = |bank: &mut Textures, rgb| {
        bank.tile([1, 1], 1, textures::Filter::Color, |_, _| rgba(rgb))
            .unwrap()
    };
    let bright = tile(&mut bank, [0.6, 0.45, 0.27, 1.]);
    let dark = tile(&mut bank, [0.08, 0.09, 0.10, 1.]);
    let black = tile(&mut bank, [0., 0., 0., 1.]);
    let sky = sky::pack_cdf(&mut bank, [dark, dark, bright]).unwrap();
    let black_cdf = sky::pack_cdf(&mut bank, [black; 3]).unwrap();
    let alb = 0.6;
    let material = Surface::new(0, 0, 0., 0.).with_lighting([alb, alb, alb, 1.], [1.; 4], [0.; 4]);
    let mut b = Builder::default();
    let floor = quad_material(
        &mut b,
        [
            [-90., -4., 90.],
            [90., -4., 90.],
            [-90., -4., -90.],
            [90., -4., -90.],
        ],
        [0., 1., 0.],
        material,
        u32::MAX,
    );
    put(&mut b, floor);
    let empty = Builder::default().finish();
    h.ray.install(Ok((b.finish(), bank.clone())));
    let mut f = test_frame(Vec3::ZERO, -Vec3::Y, 24);
    f.sky0 = [dark, dark, bright, 0];
    f.sky_cdf = [sky, 0, 0, 0];
    f.sky_tint = [1.; 4];
    f.ambient = [0.8; 4];
    let mean = |pixels: Vec<([u8; 4], f32)>| {
        let n = pixels.len() as f32;
        let sum: Vec3 = pixels
            .into_iter()
            .map(|(c, d)| {
                assert!(d > 0.);
                Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)
            })
            .sum();
        sum / (255. * n)
    };
    let sky_mean = mean(h.pixels(f, &empty));
    let decode = |v: f32| textures::decode_srgb((v * 255.).round() / 255.);
    let sky_expected = Vec3::from_array([0.6, 0.45, 0.27].map(decode)) * alb;
    assert!(
        sky_mean.abs_diff_eq(sky_expected, 0.008),
        "sky MIS: {sky_mean:?} vs {sky_expected:?}"
    );

    // Every face emits a constant, textured Le and reflects the same diffuse
    // albedo. Uniform radiance solves the closed-cube rendering equation:
    // Lo = Le + rho * Lo. One wall belongs to the moving packet.
    let material =
        Surface::new(bright, 0, 0., 0.).with_lighting([alb; 4], [1.; 4], [0.45, 0.5, 0.65, 0.]);
    let mut resident = Builder::default();
    let mut moving = Builder::default();
    for (i, n) in [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ]
    .into_iter()
    .enumerate()
    {
        let builder = if i == 0 { &mut moving } else { &mut resident };
        let u = if n.x.abs() > 0. { Vec3::Z } else { Vec3::X };
        let v = n.cross(u);
        let points = [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)]
            .map(|(x, y)| (-n * 4. + u * (4. * x) + v * (4. * y)).to_array());
        let wall = quad_material(builder, points, n.to_array(), material, u32::MAX);
        put(builder, wall);
    }
    let pack = |b: Builder| b.finish_with_emitters(&mut AccelCache::default(), Some(&bank));
    let resident = pack(resident);
    let moving = pack(moving);
    assert_eq!((resident.words[17], moving.words[17]), (10, 2));
    assert!(
        (f32::from_bits(resident.words[18]) / f32::from_bits(moving.words[18]) - 5.).abs() < 1e-5
    );
    h.ray.install(Ok((resident, bank)));
    let mut f = test_frame(Vec3::ZERO, -Vec3::X, 24);
    f.sky0 = [black, black, black, 0];
    f.sky_tint = [1.; 4];
    f.sky_cdf = [black_cdf, 0, 0, 0];
    f.ambient = [0.8; 4];
    let box_mean = mean(h.pixels(f, &moving));
    let texel = Vec3::from_array([0.6, 0.45, 0.27].map(decode));
    let emission = texel * Vec3::new(0.45, 0.5, 0.65);
    let reflected = texel * alb;
    let sum = Vec3::from_array(
        reflected
            .to_array()
            .map(|rho| (1. - rho.powi(10)) / (1. - rho)),
    );
    let box_expected = emission * sum;
    assert!(
        box_mean.abs_diff_eq(box_expected, 0.015),
        "diffuse box MIS: {box_mean:?} vs {box_expected:?}"
    );
    eprintln!("Lighter: sky MIS {sky_mean:?} vs {sky_expected:?}, diffuse-box MIS {box_mean:?} vs {box_expected:?}");
    h.ray.settings = old;
}

fn exercise_media_segments(h: &mut Harness) {
    let mut bank = Textures::default();
    let sky = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| {
            rgba([0., 0., 0.5, 1.])
        })
        .unwrap();
    let body = bank.reserve(8).unwrap();
    let mask = bank.reserve(1).unwrap();
    bank.write_words(mask, &[1]); // west cell wet, east cell dry
    bank.write_words(
        body,
        &[
            (-2.0f32).to_bits(),
            1.0f32.to_bits(),
            2.0f32.to_bits(),
            0,
            2,
            1,
            mask as u32,
            0,
        ],
    );
    let mut b = Builder::default();
    let top = quad_material(
        &mut b,
        [[-2., 0., 1.], [0., 0., 1.], [-2., 0., -1.], [0., 0., -1.]],
        [0., 1., 0.],
        Surface::new(0, WATER, 0., 0.).with_water_body(1),
        u32::MAX,
    );
    put(&mut b, top);
    let wall = quad(
        &mut b,
        [[1., 0., -1.], [1., 0., 1.], [1., -3., -1.], [1., -3., 1.]],
        [-1., 0., 0.],
        0,
        EMISSIVE,
        u32::MAX,
    );
    put(&mut b, wall);
    let bottom = quad(
        &mut b,
        [
            [-2., -4., 1.],
            [0., -4., 1.],
            [-2., -4., -1.],
            [0., -4., -1.],
        ],
        [0., 1., 0.],
        0,
        0,
        u32::MAX,
    );
    put(&mut b, bottom);
    h.ray.install(Ok((b.finish(), bank)));
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = false;
    h.ray.settings.bounces = 1;
    let empty = Builder::default().finish();
    let eye = Vec3::new(-1.5, -2., 0.);
    let mut f = test_frame(eye, Vec3::X, 3);
    f.water_effects = [0, 1, body as u32, 1];
    f.resident_bounds = [-200., 200., -200., 200.];
    f.water = [0., 12., 1., 0.];
    let hue = Vec3::from_slice(&f.water_color);
    let tr = |dist: f32| {
        let density = Vec3::ONE + (Vec3::ONE - hue / hue.max_element()) * 0.5;
        (density * (-4.321928 * dist / 12.))
            .to_array()
            .map(f32::exp2)
    };
    let wet = Vec3::from_array(tr(1.5));
    f.air = [0., 4., 1., 0.];
    f.air_color = [0.12, 0.20, 0.24, 0.];
    let air = Vec3::splat(0.75) + Vec3::from_slice(&f.air_color) * 0.25;
    let expected = hue * (Vec3::ONE - wet) + wet * air;
    let p = h.pixel(f, &empty);
    assert_radiance(
        "water footprint exits into a dry cell, then air fog",
        p,
        expected.to_array(),
    );
    assert!((p.1 - depth_at(f, Vec3::new(1., -2., 0.))).abs() < 1e-5);

    let view = |dir| {
        let mut view = test_frame(eye, dir, 3);
        view.water_effects = f.water_effects;
        view.resident_bounds = f.resident_bounds;
        view.water = f.water;
        view
    };
    let mut up = view(Vec3::Y);
    up.sky0 = [sky, sky, sky, 0];
    up.sky_tint = [1.; 4];
    let t = Vec3::from_array(tr(2.));
    let expected = hue * (Vec3::ONE - t) + t * Vec3::Z * (0.2158605 * 1.333f32.powi(2));
    let p = h.pixel(up, &empty);
    assert_radiance(
        "submerged sky through typed water exit",
        p,
        expected.to_array(),
    );
    assert!((p.1 - depth_at(up, Vec3::new(eye.x, 0., 0.))).abs() < 1e-5);

    let mut down = view(-Vec3::Y);
    down.sun = [0., 1., 0., 1.];
    down.ambient = [0.; 4];
    down.sun_color = [0.75; 4];
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.soft_sun = false;
    h.ray.settings.bounces = 0;
    let fresnel = ((1.333f32 - 1.) / (1.333 + 1.)).powi(2);
    let fog = hue * (Vec3::ONE - t);
    let expected = fog + t * Vec3::from_array(tr(4.)) * (0.75 * (1. - fresnel));
    let lit = h.pixel(down, &empty);
    assert_radiance(
        "flat-interface caustic link attenuates the sun leg separately",
        lit,
        expected.to_array(),
    );
    let mut blocker = Builder::default();
    let slab = quad(
        &mut blocker,
        [[-2., 1., 1.], [0., 1., 1.], [-2., 1., -1.], [0., 1., -1.]],
        [0., 1., 0.],
        0,
        0,
        u32::MAX,
    );
    put(&mut blocker, slab);
    let occluded = h.pixel(down, &blocker.finish());
    assert_radiance(
        "an above-water blocker still occludes a submerged receiver",
        occluded,
        fog.to_array(),
    );
    assert_eq!(lit.1, occluded.1);
    eprintln!("Lighter: wet-to-dry extinction, submerged sky and two-leg sun connection passed");
}

fn exercise_dielectric_and_metal(h: &mut Harness) {
    h.ray.settings.bounces = 2;
    h.ray.settings.samples = 1;
    h.ray.settings.physical_bsdf = true;
    h.ray.settings.shadows = false;
    h.ray.settings.lighting = true;
    h.ray.settings.reflections = true;
    let empty = Builder::default().finish();
    let mut f = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    f.ambient = [0.; 4];
    let scene = |solid: bool| {
        let mut b = Builder::default();
        let art = Surface::new(0, 0, 0., 0.);
        let glass = if solid {
            art.with_solid_glass([1.; 3], 1.5)
        } else {
            art.with_glass([1., 0.5, 0.25], 1.5)
        };
        let mut pane = |z, reversed| {
            let mut corners = [[-1., 1., z], [1., 1., z], [-1., -1., z], [1., -1., z]];
            if reversed {
                corners.swap(0, 1);
                corners.swap(2, 3);
            }
            let mesh = quad_material(&mut b, corners, [0., 0., 1.], glass, u32::MAX);
            put(&mut b, mesh);
        };
        pane(-3., false);
        if solid {
            pane(-4., true);
        }
        let far = upright(&mut b, -5., [0., 0., 1., 1.], EMISSIVE, 0);
        put(&mut b, far);
        let near = upright(&mut b, 2., [0., 1., 0., 1.], EMISSIVE, 0);
        put(&mut b, near);
        (b.finish(), Textures::default())
    };
    h.ray.install(Ok(scene(false)));
    let f_thin = 2. * 0.04 / 1.04;
    let thin = h.pixel(f, &empty);
    assert_radiance(
        "thin glass split, reverse-depth and linear tint",
        thin,
        [0., f_thin, (1. - f_thin) * 0.25],
    );
    assert!((thin.1 - depth_at(f, -3. * Vec3::Z)).abs() < 1e-5);
    h.ray.install(Ok(scene(true)));
    h.ray.settings.reflections = false;
    assert_radiance(
        "glass slab exiting cancels the eta-squared factors",
        h.pixel(f, &empty),
        [0., 0., 0.96 * 0.96],
    );

    let mut b = Builder::default();
    let conductor = quad_material(
        &mut b,
        [
            [-1., 1., -5.],
            [1., 1., -5.],
            [-1., -1., -5.],
            [1., -1., -5.],
        ],
        [0., 0., 1.],
        Surface::new(0, 0, 0., 0.).with_pbr(1., 0.65),
        rgba([0.8, 0.1, 0.05, 1.]),
    );
    put(&mut b, conductor);
    b.light(Light::point([0., 0., -2., 10.], [1.; 4]));
    h.ray.install(Ok((b.finish(), Textures::default())));
    h.ray.settings.bounces = 0;
    h.ray.settings.reflections = true;
    let lit = h.pixel(f, &empty);
    assert!(
        lit.0[0] > 70 && u32::from(lit.0[0]) > 16 * u32::from(lit.0[1].max(1)),
        "conductor lost colored Fresnel: {lit:?}"
    );
    h.ray.settings.physical_bsdf = false;
    eprintln!("Lighter: thin and solid glass interfaces and colored GGX conductor passed");
}

// Compare linear encoded readback means without a filter, history, exposure,
// or sRGB curve hiding errors in the probability compensation.
fn exercise_single_path_estimators(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings = Settings {
        bounces: 1,
        samples: 1,
        resolution: 100,
        denoiser: Denoiser::None,
        progressive: false,
        soft_sun: false,
        physical_bsdf: false,
        reference: false,
        lighting: true,
        shadows: false,
        reflections: true,
        ..saved
    };
    let mode = saved.mode;
    let empty = Builder::for_mode(mode).finish();
    let f = Frame {
        ambient: [0.; 4],
        ..test_frame(Vec3::ZERO, Vec3::NEG_Z, 65)
    };
    let pane = |builder: &mut Builder, z: f32, material: Surface, tint| {
        let r = 5.;
        let mesh = quad_material(
            builder,
            [[-r, r, z], [r, r, z], [-r, -r, z], [r, -r, z]],
            [0., 0., 1.],
            material,
            tint,
        );
        put(builder, mesh);
    };
    let mean = |pixels: &[([u8; 4], f32)]| {
        let sum = pixels.iter().fold([0u32; 3], |mut sum, p| {
            for i in 0..3 {
                sum[i] += u32::from(p.0[i]);
            }
            sum
        });
        sum.map(|x| x as f32 / (255. * pixels.len() as f32))
    };
    let compare = |h: &mut Harness, name, tol: f32| {
        h.ray.settings.single_path = false;
        let split = h.pixels(f, &empty);
        h.ray.settings.single_path = true;
        let sampled = h.pixels(f, &empty);
        let (a, b) = (mean(&split), mean(&sampled));
        assert!(split.iter().zip(&sampled).all(|(a, b)| a.1 == b.1));
        assert!(
            a.into_iter().zip(b).all(|(x, y)| (x - y).abs() < tol),
            "{name}: {a:?} != {b:?}"
        );
        eprintln!("Lighter {name}: split/full mean {a:?}, sampled mean {b:?}");
        (a, sampled)
    };
    let flat = Surface::new(0, 0, 0., 0.);
    let luminous = Surface::new(0, EMISSIVE, 0., 0.);
    let mut b = Builder::for_mode(mode);
    // Solid entering dielectric: the selected transmission must still use
    // its eta-squared radiance factor and RGB tint, unlike a thin pane.
    pane(
        &mut b,
        -3.,
        flat.with_solid_glass([0.9, 0.65, 0.5], 1.5),
        u32::MAX,
    );
    pane(&mut b, -5., luminous, rgba([0., 0., 0.9, 1.]));
    pane(&mut b, 2., luminous, rgba([0.8, 0., 0., 1.]));
    h.ray.install(Ok((b.finish(), Textures::default())));
    let (a, sampled) = compare(h, "Fresnel", 0.015);
    assert!(a[0] > 0.02 && a[2] > 0.16);
    assert!(sampled.iter().any(|p| p.0[0] > 180) && sampled.iter().any(|p| p.0[2] > 40));

    h.ray.settings.reflections = false;
    h.ray.settings.bounces = 2;
    let mut b = Builder::for_mode(mode);
    for z in [-2., -3.] {
        pane(&mut b, z, flat.with_glass([0.5; 3], 1.5), u32::MAX);
    }
    pane(&mut b, -4., luminous, rgba([0.8, 0., 0., 1.]));
    h.ray.install(Ok((b.finish(), Textures::default())));
    let (a, sampled) = compare(h, "roulette", 0.016);
    assert!(a[0] > 0.15);
    assert!(sampled.iter().any(|p| p.0[0] > 185) && sampled.iter().any(|p| p.0[0] == 0));

    h.ray.settings.shadows = true;
    h.ray.settings.bounces = 0;
    let mut b = Builder::for_mode(mode);
    pane(&mut b, -4., flat, u32::MAX);
    for i in 0..6 {
        let mut color = [0.; 4];
        color[i % 3] = 0.065 + i as f32 * 0.012;
        color[3] = 1.;
        b.light(Light::point(
            [(i as f32 - 2.5) * 0.48, 1.7, -1.5, 8.],
            color,
        ));
    }
    // Small off-axis blocker covers some light legs but not the whole receiver.
    let obstacle = upright(&mut b, -2.5, [0.1, 0.1, 0.1, 1.], 0, 0);
    b.place(
        obstacle,
        Transform {
            scale: 0.43,
            ..Transform::translation([0.4, 1.1, 0.])
        },
    )
    .unwrap();
    h.ray.install(Ok((b.finish(), Textures::default())));
    let (a, sampled) = compare(h, "local light PMF", 0.014);
    assert!(a.iter().all(|&x| x > 0.025));
    assert!(
        sampled.iter().any(|p| p.0[0] == 0 && p.0[1] > 0),
        "all lights were evaluated, not sampled"
    );
    h.ray.settings = saved;
}

fn exercise_stationary_sun(h: &mut Harness) {
    const VALID: u32 = 1 << 30;
    const BLOCKED: u32 = 1 << 31;
    const HISTORY: u32 = 63 << 24;
    const MOVING: u32 = 1 << 31;
    let saved = h.ray.settings;
    h.ray.settings = Settings {
        lighting: true,
        bounces: 0,
        samples: 1,
        resolution: 100,
        shadows: true,
        soft_sun: false,
        progressive: true,
        denoiser: Denoiser::Svgf,
        ..saved
    };
    let mode = saved.mode;
    const E: u32 = 19;
    let f = Frame {
        ambient: [0.; 4],
        sun: [0., 0.8, 0.6, 1.],
        ..test_frame(Vec3::ZERO, Vec3::NEG_Z, E)
    };
    let mut b = Builder::for_mode(mode);
    let mesh = quad(
        &mut b,
        [
            [-4., 4., -5.],
            [4., 4., -5.],
            [-4., -4., -5.],
            [4., -4., -5.],
        ],
        [0., 0., 1.],
        0,
        0,
        u32::MAX,
    );
    put(&mut b, mesh);
    let world = b.finish();
    h.ray.install(Ok((world, Textures::default())));
    let blocker = |x, glass: bool| {
        let mut b = Builder::for_mode(mode);
        let mut material = Surface::new(0, 0, 0., 0.);
        if glass {
            material = material.with_glass([0.5, 0.7, 0.9], 1.5);
        }
        let mat = quad_material(
            &mut b,
            [
                [-0.9, 4., -3.],
                [0.9, 4., -3.],
                [-0.9, 1.4, -3.],
                [0.9, 1.4, -3.],
            ],
            [0., 0., 1.],
            material,
            u32::MAX,
        );
        b.place(mat, Transform::translation([x, 0., 0.])).unwrap();
        b.finish()
    };
    let center = (E * E / 2) as usize;
    let unaffected = center - 7;
    let cast = blocker(0., false);
    for n in 1..=4 {
        let shadow = h.pixels(f, &cast);
        assert!(
            shadow[center].0[0] < 6,
            "shadow leaked with filter/cache: {:?}",
            shadow[center]
        );
        let steps = h.history_state(center);
        assert_eq!(
            steps.0 & (VALID | BLOCKED),
            VALID | BLOCKED,
            "opaque sun result not retained"
        );
        assert_eq!((steps.0 & HISTORY) >> 24, n);
        assert_ne!(steps.1 & MOVING, 0);
    }
    let moved = h.pixels(f, &blocker(16., false));
    assert!(
        moved[center].0[0] > 100,
        "stationary cache retained a departed blocker"
    );
    let steps = h.history_state(center);
    assert_eq!(steps.0 & (VALID | BLOCKED), VALID);
    assert_eq!(
        (steps.0 & HISTORY) >> 24,
        1,
        "moving-shadow history was reused"
    );
    assert_eq!(
        (h.history_state(unaffected).0 & HISTORY) >> 24,
        5,
        "unaffected history was dropped"
    );
    let tinted = h.pixels(f, &blocker(0., true));
    assert!(tinted[center].0[0] < tinted[center].0[2]);
    assert_eq!(
        h.history_state(center).0 & VALID,
        0,
        "colored transmittance was quantized to a bit"
    );
    h.ray.settings = saved;
}

fn exercise_bsdf_energy(h: &Harness) {
    const N: u64 = 25;
    let output = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("BSDF furnace probe"),
        size: N * 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let map = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: N * 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let shader = h.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("BSDF furnace across angles and materials"),
        source: wgpu::ShaderSource::Wgsl(
            [
                BSDF,
                r#"
@group(0) @binding(0) var<storage,read_write> values: array<vec4<f32>>;
fn rng(seed: u32) -> f32 {
    var x = (seed ^ (seed >> 16u))*0x7feb352du;
    x = (x ^ (x >> 15u))*0x846ca68bu;
    return f32((x ^ (x >> 16u)) & 0xffffffu)/16777216.;
}
@compute @workgroup_size(1) fn integrate(@builtin(global_invocation_id) id: vec3<u32>) {
    let i=id.x;
    let n=vec3<f32>(0.,0.,1.);
    if i==24u {
        values[i]=vec4<f32>(dielectric(vec3<f32>(0.8660254,0.,-0.5),n,1.5).fresnel,
                           dielectric(-n,n,1./1.5).fresnel,0.,0.);
        return;
    }
    var cosines=array<f32,4>(1.,0.7,0.25,0.07);
    var alphas=array<f32,3>(0.04,0.25,0.8);
    let nv=cosines[i%4u];
    let alpha=alphas[(i/4u)%3u];
    let metal=i>=12u;
    let b=Bsdf(select(vec3<f32>(1.),vec3<f32>(0.),metal),
               select(vec3<f32>(0.04),vec3<f32>(0.95,0.6,0.2),metal),alpha);
    let wo=vec3<f32>(sqrt(1.-nv*nv),0.,nv);
    var accum=vec3<f32>(0.);
    for (var j=0u;j<8192u;j++) {
        let u = vec3<f32>((f32(j)+0.5)/8192.,rng(j*3u+1u),rng(j*3u+2u));
        accum += bsdf_sample(b,n,wo,u).weight;
    }
    values[i]=vec4<f32>(accum/8192.,0.);
}
"#,
            ]
            .concat()
            .into(),
        ),
    });
    let pipeline = h
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &shader,
            entry_point: "integrate",
        });
    let bind = h.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    let mut e = h.device.create_command_encoder(&Default::default());
    {
        let mut pass = e.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(N as u32, 1, 1);
    }
    e.copy_buffer_to_buffer(&output, 0, &map, 0, N * 16);
    h.queue.submit(Some(e.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    let view = map.slice(..);
    view.map_async(wgpu::MapMode::Read, move |v| {
        let _ = tx.send(v);
    });
    crate::gpu::wait_for_capture_callback(&h.device, rx)
        .unwrap()
        .unwrap();
    let bytes = view.get_mapped_range();
    let values: &[[f32; 4]] = bytemuck::cast_slice(&bytes);
    assert!(
        (values[24][0] - 1.).abs() < 1e-7 && (values[24][1] - 0.04).abs() < 1e-6,
        "Snell/TIR {:?}",
        values[24]
    );
    let mut max = 0.0f32;
    for (i, value) in values[..24].iter().enumerate() {
        for &v in &value[..3] {
            assert!(
                (0. ..=1.015).contains(&v),
                "BSDF energy exceeded incident at case {i}: {value:?}"
            );
            max = max.max(v);
        }
    }
    assert!(
        values[12][0] > 0.9 && values[12][0] > values[12][2] * 3.,
        "gloss was extinguished instead of conserving energy"
    );
    eprintln!("Lighter: GGX furnace at 24 angle/roughness/material points, peak reflected fraction {max:.5}");
}

fn exercise_finite_emitters(h: &mut Harness) {
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.bounces = 0;
    h.ray.settings.progressive = false;
    let mut world = Builder::default();
    let receiver = upright(&mut world, -6., [1.; 4], 0, 0);
    put(&mut world, receiver);
    h.ray.install(Ok((world.finish(), Textures::default())));
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    frame.ambient = [0.; 4];
    let impact = [4., 0., -2., 10.];
    let point = Light::point(impact, [1.; 4]);
    let pane = |x: f32, z, tint| {
        (
            [
                [x - 0.8, 1., z],
                [x + 0.8, 1., z],
                [x - 0.8, -1., z],
                [x + 0.8, -1., z],
            ],
            tint,
        )
    };
    let fixture = |lamp, panes: &[([[f32; 3]; 4], [f32; 3])]| {
        let mut b = Builder::default();
        b.light(lamp);
        for &(pos, tint) in panes {
            let obj = quad_material(
                &mut b,
                pos,
                [0., 0., 1.],
                Surface::new(0, 0, 0., 0.).with_glass(tint, 1.5),
                u32::MAX,
            );
            put(&mut b, obj);
        }
        b.finish()
    };
    let cosine = std::f32::consts::FRAC_1_SQRT_2;
    let energy = cosine * (1. - (32.0f32).sqrt() / 10.).powi(2);
    let open = h.pixel(frame, &fixture(point, &[]));
    assert_radiance("point baseline", open, [energy; 3]);
    let ri = 1.0 / 1.5;
    let ct = (1. - ri * ri * (1. - cosine * cosine)).sqrt();
    let fr = 0.5
        * (((ri * cosine - ct) / (ri * cosine + ct)).powi(2)
            + ((cosine - ri * ct) / (cosine + ri * ct)).powi(2));
    let tx = (1. - fr) / (1. + fr);
    let p = h.pixel(
        frame,
        &fixture(
            point,
            &[
                pane(2., -4., [1., 0.5, 0.25]),
                pane(3., -3., [0.25, 1., 0.5]),
                pane(5., -1., [0.; 3]),
            ],
        ),
    );
    assert_radiance(
        "two tinted pane shadows, with blocker beyond emitter",
        p,
        [0.25, 0.5, 0.125].map(|x| x * energy * tx * tx),
    );
    assert_eq!(open.1, p.1);
    let conditional = fixture(point.night_rgb([1.; 4]), &[]);
    assert_radiance(
        "night-flag light by day",
        h.pixel(frame, &conditional),
        [0.; 3],
    );
    frame.material_time[2] = 1.;
    assert_eq!(h.pixel(frame, &conditional), open);
    frame.material_time[2] = 0.5;
    assert_radiance(
        "night weight applied in encoded RGB",
        h.pixel(frame, &conditional),
        [0.21404114 * energy; 3],
    );

    // Boundary of an off-axis blocker. For the two finite shapes at least
    // some samples go around the edge that fully covers the point emitter.
    let edge = |light| {
        let mut b = Builder::default();
        b.light(light);
        let block = quad(
            &mut b,
            [
                [1.67, 5., -4.],
                [3., 5., -4.],
                [1.67, -5., -4.],
                [3., -5., -4.],
            ],
            [0., 0., 1.],
            0,
            0,
            u32::MAX,
        );
        put(&mut b, block);
        b.finish()
    };
    frame.extent = [0, 0, 31, 31];
    let open = h.pixels(frame, &fixture(point, &[]));
    let hard = h.pixels(frame, &edge(point));
    for lamp in [point.sphere(1.3), point.disk(1.3)] {
        let soft = h.pixels(frame, &edge(lamp));
        assert!(
            soft.iter()
                .zip(&hard)
                .zip(&open)
                .filter(|((s, h), o)| h.0[0] == 0 && o.0[0] > 40 && s.0[0] > 15)
                .count()
                > 4,
            "finite emitter did not illuminate part of the point umbra"
        );
    }
    eprintln!("Lighter: finite sphere/disk penumbra, static night flag and two-pane colored shadow passed");
}

fn exercise_effect_placement(h: &mut Harness) {
    let empty = Builder::default().finish();
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.bounces = 0;
    let mut b = Builder::default();
    let floor = upright(&mut b, -3., [0., 0., 1., 1.], EMISSIVE, 0);
    put(&mut b, floor);
    for (z, tint, blend) in [
        (-5., [1., 0., 0., 0.5], 2 | 6 << 4),
        (-7., [0., 1., 0., 0.25], 5 | 6 << 4),
    ] {
        let positions = [[-1., 1., z], [1., 1., z], [-1., -1., z], [1., -1., z]];
        let m = Surface::new(
            0,
            EMISSIVE | BLEND | ORDERED_GFX | OVERLAY | PRIMARY_ONLY,
            0.,
            0.,
        )
        .with_blend(blend);
        let layer = quad_material(&mut b, positions, [0., 0., 1.], m, rgba(tint));
        put(&mut b, layer);
    }
    h.ray.install(Ok((b.finish(), Textures::default())));
    let f = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    let p = h.pixel(f, &empty);
    assert_radiance(
        "no-Z GFX layers follow draw order, not distance or opaque depth",
        p,
        [0.75, 0.25, 0.375],
    );
    assert!((p.1 - depth_at(f, -3. * Vec3::Z)).abs() < 1e-5);

    let mut b = Builder::default();
    let floor = upright(&mut b, -8., [0., 0., 1., 1.], EMISSIVE, 0);
    put(&mut b, floor);
    let mut effects = Builder::default();
    let mut batch = Mesh::default();
    for (i, (z, tint, depth)) in [
        (-3., [1., 0., 0., 0.5], false),
        (-6., [0., 1., 0., 0.5], false),
        (-4., [1., 1., 1., 0.5], true),
        (-5., [0., 0., 0., 1.], false),
    ]
    .into_iter()
    .enumerate()
    {
        let flags = EMISSIVE | BLEND | ORDERED_GFX | if depth { WRITE_DEPTH } else { 0 };
        let mat = effects.surface(Surface::new(0, flags, 0., 0.).with_blend(5 | 6 << 4));
        let start = batch.vertices.len() as u32;
        batch.vertices.extend(
            [[-1., 1., z], [1., 1., z], [-1., -1., z], [1., -1., z]]
                .map(|p| Vertex::new(p, [0., 0., 1.], [0.; 2]).tint(rgba(tint), rgba(tint))),
        );
        batch.tri_layer([start, start + 1, start + 3], mat, i as u32 + 1);
        batch.tri_layer([start, start + 3, start + 2], mat, i as u32 + 1);
    }
    effects.world_mesh(&batch).unwrap();
    h.ray.install(Ok((b.finish(), Textures::default())));
    let p = h.pixel(f, &effects.finish());
    assert_radiance(
        "depth-tested GFX keeps layer order and a later sheet fails write-Z",
        p,
        [0.625, 0.75, 0.625],
    );
    assert!((p.1 - depth_at(f, -4. * Vec3::Z)).abs() < 1e-5);

    let ground = |y| [[-2., y, 2.], [2., y, 2.], [-2., y, -2.], [2., y, -2.]];
    let mut b = Builder::default();
    let floor = quad(&mut b, ground(0.), [0., 1., 0.], 0, TERRAIN, u32::MAX);
    put(&mut b, floor);
    h.ray.install(Ok((b.finish(), Textures::default())));
    let mut decals = Builder::default();
    let projector = |y| [[-1., y, 1.], [1., y, 1.], [-1., y, -1.], [1., y, -1.]];
    decals
        .project_decal(
            projector(0.2),
            [[0.5; 2]; 4],
            [rgba([1., 0., 0., 1.]); 4],
            0,
            5 | 6 << 4 | CLAMP_UV,
        )
        .unwrap();
    decals
        .project_decal(
            projector(4.2),
            [[0.5; 2]; 4],
            [rgba([0., 1., 0., 1.]); 4],
            0,
            5 | 6 << 4,
        )
        .unwrap();
    let projected = decals.finish();
    let mut f = test_frame(3. * Vec3::Y, -Vec3::Y, 3);
    f.terrain = [16., 16., 1., 16.];
    let p = h.pixel(f, &projected);
    assert_radiance(
        "projected only onto the hit inside the decal box",
        p,
        [1., 0., 0.],
    );
    assert!((p.1 - depth_at(f, Vec3::ZERO)).abs() < 1e-5);

    let mut b = Builder::default();
    let floor = quad(&mut b, ground(0.), [0., 1., 0.], 0, 0, u32::MAX);
    put(&mut b, floor);
    let cloud = quad(
        &mut b,
        ground(4.),
        [0., 1., 0.],
        0,
        CLOUD | BLEND | EMISSIVE,
        rgba([1., 1., 1., 0.5]),
    );
    put(&mut b, cloud);
    h.ray.install(Ok((b.finish(), Textures::default())));
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.soft_sun = false;
    f.sun = [0., 1., 0., 1.];
    f.ambient = [0.; 4];
    f.sun_color = [1.; 4];
    f.options[0] = CLOUD_DETAIL;
    assert_radiance(
        "authored cloud opacity transmits only part of noon sun",
        h.pixel(f, &empty),
        [1. - 0.72 * (128. / 255.); 3],
    );
    f.options[0] = 0;
    assert_radiance(
        "cloud detail gate includes its shadow",
        h.pixel(f, &empty),
        [1.; 3],
    );
    eprintln!("Lighter: no-depth GFX order, bounded decal projection and cloud shadow passed");
}

fn exercise_spatial_upscale(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings = Settings {
        bounces: 0,
        samples: 1,
        resolution: 50,
        progressive: false,
        denoiser: Denoiser::None,
        lighting: false,
        ..saved
    };
    let mut b = Builder::for_mode(saved.mode);
    let near = upright(&mut b, -4., [1., 0., 0., 1.], EMISSIVE, 0);
    put(&mut b, near);
    h.ray.install(Ok((b.finish(), Textures::default())));
    let f = test_frame(Vec3::ZERO, Vec3::NEG_Z, 47);
    let empty = Builder::for_mode(saved.mode).finish();
    let linear = h.pixels(f, &empty);
    let spatial = h.pixels(f, &empty);
    assert!(linear.iter().zip(&spatial).all(|(a, b)| a.1 == b.1));
    assert!(linear.iter().zip(&spatial).any(|(a, b)| a.0 != b.0));
    assert_eq!(spatial[47 * 23 + 23].0, [255, 0, 0, 255]);
    h.ray.settings.resolution = 100;
    let native = h.pixels(f, &empty);
    assert_eq!(
        native,
        h.pixels(f, &empty),
        "1:1 EASU must bypass the filter"
    );
    h.ray.settings = saved;
}

fn exercise_stable_primary_water(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings = Settings {
        resolution: 100,
        samples: 1,
        bounces: 1,
        lighting: false,
        shadows: false,
        reflections: true,
        progressive: false,
        denoiser: Denoiser::None,
        single_path: false,
        ..saved
    };
    let mut bank = Textures::default();
    let mirror = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xffff_0000)
        .unwrap();
    let mut b = Builder::for_mode(saved.mode);
    let ground = |y| {
        [
            [-10., y, 10.],
            [10., y, 10.],
            [-10., y, -10.],
            [10., y, -10.],
        ]
    };
    let water = quad(&mut b, ground(0.), [0., 1., 0.], 0, WATER, u32::MAX);
    put(&mut b, water);
    let bed = quad(
        &mut b,
        ground(-1.),
        [0., 1., 0.],
        0,
        EMISSIVE,
        rgba([0., 1., 0., 1.]),
    );
    put(&mut b, bed);
    h.ray.install(Ok((b.finish(), bank)));
    let f = Frame {
        options: [WATER_MIRROR, 0, 0, 0],
        sky0: [mirror, mirror, mirror, 0],
        sky1: [mirror, mirror, mirror, 0],
        ..test_frame(Vec3::new(0., 1., 0.), Vec3::new(0., -1., -1.), 27)
    };
    let empty = Builder::for_mode(saved.mode).finish();
    let control = h.pixels(f, &empty);
    h.ray.settings.single_path = true;
    let sampled = h.pixels(f, &empty);
    let center = 13 * 27 + 13;
    assert!(
        control[center].0[1] > 40 && control[center].0[2] > 0,
        "both water legs must contribute: {:?}",
        control[center]
    );
    assert_eq!(sampled, control, "primary water remains stable at 1 spp");
    h.ray.settings = saved;
}

fn exercise_water_art(h: &mut Harness) {
    let empty = Builder::default().finish();
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = true;
    h.ray.settings.bounces = 0;
    let ground = |y| [[-2., y, 2.], [2., y, 2.], [-2., y, -2.], [2., y, -2.]];
    let mut bank = Textures::default();
    let wave = bank
        .tile([1, 1], 1, textures::Filter::ColorMask, |_, _| {
            rgba([0., 0., 1., 0.25])
        })
        .unwrap();
    let mut b = Builder::default();
    let floor = quad(
        &mut b,
        ground(-2.),
        [0., 1., 0.],
        0,
        EMISSIVE,
        rgba([1., 0., 0., 1.]),
    );
    put(&mut b, floor);
    let water = quad(
        &mut b,
        ground(0.),
        [0., 1., 0.],
        0,
        WATER | SIMPLE_WATER,
        u32::MAX,
    );
    b.place(water, Transform::translation([-3., 0., 0.]))
        .unwrap();
    let shore = quad(
        &mut b,
        ground(0.2),
        [0., 1., 0.],
        wave,
        SHORE | BLEND | EMISSIVE | PRIMARY_ONLY | IGNORE_TEXTURE_ALPHA,
        u32::MAX,
    );
    put(&mut b, shore);
    // This equal-height body is a separate instance and is not water in the dry gap.
    b.place(water, Transform::translation([7., 0., 0.]))
        .unwrap();
    h.ray.install(Ok((b.finish(), bank)));
    let at = |x| {
        let mut f = test_frame(Vec3::new(x, 3., 0.), -Vec3::Y, 3);
        f.resident_bounds = [-20., 20., -20., 20.];
        f.options[0] = WATER_MIRROR;
        f.water_effects[0] = wave;
        f
    };
    let linear = |x: f32| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let alpha = 64. / 255.;
    let expected = [37. / 255., 79. / 255., 85. / 255.]
        .into_iter()
        .zip([0., 0., 1.])
        .map(|(a, b)| linear(a * (1. - alpha) + b * alpha))
        .collect::<Vec<_>>();
    for x in [-3., 7.] {
        let f = at(x);
        let p = h.pixel(f, &empty);
        assert_radiance(
            "native simple-water alpha/color stage",
            p,
            expected.as_slice().try_into().unwrap(),
        );
        assert!((p.1 - depth_at(f, Vec3::new(x, 0., 0.))).abs() < 1e-5);
    }
    let mut f = at(0.);
    let expected_alpha =
        ((0.5 * alpha * (1. - alpha) + alpha * alpha) * (1. - alpha) + alpha * alpha) * 0.6;
    let p = h.pixel(f, &empty);
    assert_radiance(
        "native shoreline three-stage wave over dry bed",
        p,
        [1. - expected_alpha, 0., expected_alpha],
    );
    let depth = depth_at(f, -2. * Vec3::Y);
    assert!(
        (p.1 - depth).abs() < 1e-5,
        "a shoreline overlay wrote water depth in the dry gap"
    );
    f.resident_bounds = [0.5, 20., -20., 20.];
    assert_radiance(
        "shoreline clipped without clipping the bed",
        h.pixel(f, &empty),
        [1., 0., 0.],
    );
    eprintln!("Lighter: distinct water bodies, dry gap, alternate-water stages and shoreline overlay passed");
}

fn exercise_vegetation(h: &mut Harness) {
    let empty = Builder::default().finish();
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.soft_sun = false;
    h.ray.settings.reflections = false;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.samples = 1;
    h.ray.settings.resolution = 100;
    let mut bank = Textures::default();
    let tile = bank
        .tile([2, 1], 2, textures::Filter::cutout(0.5, false), |x, _| {
            if x == 0 {
                rgba([0., 1., 0., 1.])
            } else {
                0
            }
        })
        .unwrap();
    let ground = |y, extent: f32| {
        [
            [-extent, y, -extent],
            [extent, y, -extent],
            [-extent, y, extent],
            [extent, y, extent],
        ]
    };
    let mut b = Builder::default();
    let floor = quad(&mut b, ground(0., 4.), [0., 1., 0.], 0, 0, u32::MAX);
    put(&mut b, floor);
    // The noon rays see a leaf's secondary shape from below. Its view card,
    // even though opaque and closer to the receiver, must never cast a shadow.
    let view_card = quad(
        &mut b,
        ground(1.7, 4.),
        [0., 1., 0.],
        0,
        PRIMARY_ONLY,
        u32::MAX,
    );
    put(&mut b, view_card);
    let leaf = quad(
        &mut b,
        ground(2., 1.),
        [0., 1., 0.],
        tile,
        FOLIAGE | CUTOUT | HIDE_PRIMARY,
        u32::MAX,
    );
    put(&mut b, leaf);
    h.ray.install(Ok((b.finish(), bank)));
    for offset in [0., 0.16] {
        for (x, expected) in [(-0.55, [0.12, 0.62, 0.12]), (0.55, [1.; 3])] {
            let eye = Vec3::new(x, 1.5, offset);
            let target = Vec3::new(x, 0., 0.);
            let mut frame = test_frame(eye, target - eye, 3);
            frame.sun = [0., 1., 0., 1.];
            frame.sun_color = [1.; 4];
            frame.ambient = [0.; 4];
            assert_radiance(
                "dappled noon with a two-sided leaf",
                h.pixel(frame, &empty),
                expected,
            );
        }
    }

    // A native LOD dissolve is complementary even at exactly coincident
    // depth. Eight pixels use each side of the 4x4 matrix at half transition.
    let mut b = Builder::default();
    for (side, color) in [(1, [1., 0., 0., 1.]), (2, [0., 1., 0., 1.])] {
        let mesh = upright(&mut b, -3., color, TREE_MATERIAL | EMISSIVE, 0);
        b.place_tree(mesh, [0.; 3], 0.5, side).unwrap();
    }
    h.ray.install(Ok((b.finish(), Textures::default())));
    let frame = test_frame(Vec3::ZERO, -Vec3::Z, 4);
    let p = h.pixels(frame, &empty);
    let red = p.iter().filter(|p| p.0 == [255, 0, 0, 255]).count();
    let green = p.iter().filter(|p| p.0 == [0, 255, 0, 255]).count();
    assert_eq!((red, green), (8, 8));
    assert!(p
        .iter()
        .all(|p| (p.1 - depth_at(frame, -3. * Vec3::Z)).abs() < 1e-5));

    // Fade is evaluated at the original vertices, not the hit or the bent
    // tip. Lower the authored texture alpha so the fade changes coverage.
    let mut bank = Textures::default();
    let tile = bank
        .tile([1, 1], 1, textures::Filter::cutout(0.3, true), |_, _| {
            rgba([1., 0., 0., 0.5])
        })
        .unwrap();
    let mut b = Builder::default();
    let material = b.surface(Surface::new(
        tile,
        GRASS_MATERIAL | CUTOUT | STRICT_ALPHA,
        0.3,
        0.,
    ));
    let mut grass = Mesh::default();
    for p in [
        [-1., 1., -2.],
        [1., 1., -2.],
        [-1., -1., -2.],
        [1., -1., -2.],
    ] {
        grass
            .vertices
            .push(Vertex::new(p, [0., 0., 1.], [0.5; 2]).grass(3.0f32.to_bits(), 0));
    }
    grass.tri([0, 2, 1], material);
    grass.tri([1, 2, 3], material);
    b.world_mesh(&grass).unwrap();
    h.ray.install(Ok((b.finish(), bank)));
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    assert_radiance("grass without fade", h.pixel(frame, &empty), [1., 0., 0.]);
    frame.material_time[1] = 1.;
    assert_eq!(
        h.pixel(frame, &empty).1,
        0.,
        "authored range and vertex fade did not reject grass"
    );
    // Move only 0.95 units: the 1-unit near clip must not clip this blade.
    let mut near = test_frame(-0.95 * Vec3::Z, -Vec3::Z, 3);
    near.material_time[1] = 1.;
    assert_radiance(
        "grass inside the authored fade",
        h.pixel(near, &empty),
        [1., 0., 0.],
    );
    eprintln!("Lighter: tinted leaf shadow, matched LOD dissolve, and grass distance fade passed");
}

fn assert_radiance(label: &str, pixel: ([u8; 4], f32), linear: [f32; 3]) {
    let expected = linear.map(|v| (v.clamp(0., 1.) * 255.).round() as u8);
    assert!(
        pixel.0[..3]
            .iter()
            .zip(expected)
            .all(|(&a, b)| a.abs_diff(b) <= 2),
        "{label}: expected {expected:?}, got {pixel:?}"
    );
    assert!(pixel.1 > 0., "{label}: lost primary depth");
}

// Analytic cases use known radiance in a linear attachment, not an image
// comparison or the native renderer as a lighting oracle.
fn exercise_transport(h: &mut Harness) {
    let mut bank = Textures::default();
    let empty = Builder::default().finish();
    let mut f = test_frame(Vec3::ZERO, -Vec3::Z, 1);
    let sky = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xffc0_a080)
        .unwrap();
    f.sky0 = [sky, sky, sky, 0];
    f.sky_tint = [1.; 4];
    f.ambient = [0.35; 4];
    h.ray.settings.samples = 1;
    h.ray.settings.shadows = false;
    h.ray.settings.lighting = true;
    h.ray.settings.reflections = true;
    let linear = |byte: u8| ((f32::from(byte) / 255. + 0.055) / 1.055).powf(2.4);
    let install_wall = |h: &mut Harness, flags, tint| {
        let mut b = Builder::default();
        let wall = upright(&mut b, -3., tint, flags, 0);
        put(&mut b, wall);
        h.ray.install(Ok((b.finish(), bank.clone())));
    };
    install_wall(h, 0, [1.; 4]);
    h.ray.settings.bounces = 0;
    let local = h.pixel(f, &empty);
    assert_radiance("zero-bounce ambient", local, [0.35; 3]);
    h.ray.settings.bounces = 1;
    let sky_lit = h.pixel(f, &empty);
    // A Lambertian white patch under a constant dome returns the dome's
    // radiance; adding ambient or a fixed 0.28 bounce factor is not correct.
    assert_radiance("Lambertian sky", sky_lit, [128, 160, 192].map(linear));
    assert_eq!(local.1, sky_lit.1);

    install_wall(h, BAKED, [0.25, 0.25, 0.25, 1.]);
    let bounced = h.pixel(f, &empty);
    h.ray.settings.bounces = 0;
    let baked = h.pixel(f, &empty);
    assert_radiance("baked color", baked, [linear(128); 3]);
    assert_eq!(bounced, baked, "baked light received a second ambient term");

    let mut b = Builder::default();
    let receiver = quad_material(
        &mut b,
        [
            [-1., 1., -3.],
            [1., 1., -3.],
            [-1., -1., -3.],
            [1., -1., -3.],
        ],
        [0., 0., 1.],
        Surface::new(0, 0, 0., 0.5),
        rgba([0., 1., 0., 1.]),
    );
    put(&mut b, receiver);
    // This plate is behind the camera. Its placement excludes it from the
    // client's water pass, not reflections on other materials.
    let emitter = upright(&mut b, 2., [1., 0., 0., 1.], EMISSIVE | NO_REFLECT, 0);
    put(&mut b, emitter);
    h.ray.install(Ok((b.finish(), bank.clone())));
    h.ray.settings.lighting = false;
    h.ray.settings.bounces = 1;
    let mirror = h.pixel(f, &empty);
    assert_radiance("half mirror, half matte", mirror, [0.5, 0.5, 0.]);
    h.ray.settings.reflections = false;
    assert_radiance("mirror disabled", h.pixel(f, &empty), [0., 1., 0.]);

    let mut b = Builder::default();
    let wall = quad(
        &mut b,
        [
            [-1., 1., -3.],
            [1., 1., -3.],
            [-1., -1., -3.],
            [1., -1., -3.],
        ],
        [1., 0., 0.1],
        0,
        0,
        u32::MAX,
    );
    put(&mut b, wall);
    h.ray.install(Ok((b.finish(), bank.clone())));
    h.ray.settings.lighting = true;
    h.ray.settings.bounces = 0;
    f.ambient = [0.; 4];
    let lamp = |z| {
        let mut b = Builder::default();
        b.light(Light::point([2., 0., z, 10.], [1.; 4]));
        b.finish()
    };
    // With shadows OFF, an interpolated normal still cannot see a light on
    // the far side of the actual triangle.
    assert_radiance("rear light", h.pixel(f, &lamp(-4.)), [0.; 3]);
    assert!(h.pixel(f, &lamp(-2.)).0[0] > 120, "front light was lost");

    let mut b = Builder::default();
    for (y, flags, tint) in [
        (0., WATER, [1.; 4]),
        (2.5, EMISSIVE, [0.5, 0., 0., 1.]),
        (-2.5, EMISSIVE, [0., 1., 0., 1.]),
    ] {
        let plane = quad(
            &mut b,
            [
                [-32., y, 32.],
                [32., y, 32.],
                [-32., y, -32.],
                [32., y, -32.],
            ],
            [0., 1., 0.],
            0,
            flags,
            rgba(tint),
        );
        put(&mut b, plane);
    }
    h.ray.install(Ok((b.finish(), bank.clone())));
    h.ray.settings.lighting = false;
    h.ray.settings.reflections = true;
    h.ray.settings.bounces = 1;
    let interface = |eye_y, dir, underwater| {
        let mut f = test_frame(Vec3::Y * eye_y, dir, 1);
        f.options[0] = WATER_MIRROR | if underwater { UNDERWATER } else { 0 };
        f.water[2] = 0.; // Isolate the interface from volume extinction.
        f
    };
    let ior = 1.333_f32;
    let normal_fresnel = ((ior - 1.) / (ior + 1.)).powi(2);
    assert_radiance(
        "normally incident air-to-water",
        h.pixel(interface(1.5, -Vec3::Y, false), &empty),
        [
            normal_fresnel * 128. / 255.,
            (1. - normal_fresnel) / ior.powi(2),
            0.,
        ],
    );
    let incident_angle = 0.68_f32.acos();
    let outgoing_angle = (ior * incident_angle.sin()).asin();
    let minus = incident_angle - outgoing_angle;
    let plus = incident_angle + outgoing_angle;
    let f = 0.5 * ((minus.sin() / plus.sin()).powi(2) + (minus.tan() / plus.tan()).powi(2));
    assert_radiance(
        "water-to-air just below the critical angle",
        h.pixel(
            interface(
                -1.5,
                Vec3::new(0., incident_angle.cos(), -incident_angle.sin()),
                true,
            ),
            &empty,
        ),
        [(1. - f) * ior.powi(2) * 128. / 255., f, 0.],
    );
    assert_radiance(
        "total internal reflection",
        h.pixel(interface(-1.5, Vec3::new(0., 0.48, -1.), true), &empty),
        [0., 1., 0.],
    );

    h.ray.settings.bounces = 0;
    let mut water = test_frame(Vec3::ZERO, -Vec3::Z, 1);
    water.options[0] = UNDERWATER;
    water.water = [0., 8., 1., 0.];
    water.water_color = [0.2; 4];
    for d in [2_f32, 4.] {
        let mut b = Builder::default();
        let white = upright(&mut b, -d, [1.; 4], EMISSIVE, 0);
        put(&mut b, white);
        h.ray.install(Ok((b.finish(), bank.clone())));
        let value = 0.2 + 0.8 * 0.05_f32.powf(d / 8.);
        assert_radiance(
            "underwater path extinction",
            h.pixel(water, &empty),
            [value; 3],
        );
    }
}

// The same test suite runs through the compute BVH, CPU Vulkan queries, and
// opt-in physical Vulkan hardware. Measurements are from the linear target.
fn exercise_denoiser_edges(h: &mut Harness) {
    let empty = Builder::default().finish();
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = false;
    h.ray.settings.bounces = 0;
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 7);
    frame.sun = [0., 0.8, 0.6, 1.];
    frame.ambient = [0.; 4];
    let small = |p: &[([u8; 4], f32)], x: usize| p[3 * 7 + x];
    for (label, z, n) in [
        ("depth step", -8., [0., 0., 1.]),
        ("coplanar normal crease", -5., [0., -0.8, 0.6]),
    ] {
        let mut b = Builder::default();
        for (x0, x1, z, n) in [(-4., 0.1, -5., [0., 0., 1.]), (0.1, 4., z, n)] {
            let mesh = quad(
                &mut b,
                [[x0, 4., z], [x1, 4., z], [x0, -4., z], [x1, -4., z]],
                n,
                0,
                0,
                u32::MAX,
            );
            put(&mut b, mesh);
        }
        h.ray.install(Ok((b.finish(), Textures::default())));
        // Same white material on both sides. Only depth (distance fog) or
        // normal (direct sun) can explain the discontinuity.
        frame.air = [6., 7., 1., 0.];
        frame.extent = [0, 0, 7, 7];
        h.ray.settings.denoiser = Denoiser::None;
        h.ray.settings.resolution = 100;
        let raw = h.pixels(frame, &empty);
        let left = small(&raw, 3);
        let right = small(&raw, 4);
        assert!(left.0[0] > right.0[0] + 80, "{label}: {left:?} {right:?}");
        h.ray.settings.denoiser = Denoiser::Svgf;
        let filtered = h.pixels(frame, &empty);
        assert_eq!(small(&filtered, 3), left, "{label}: left edge filtered");
        assert_eq!(small(&filtered, 4), right, "{label}: right edge filtered");
        assert!(filtered.iter().zip(&raw).all(|(a, b)| a.1 == b.1));

        frame.extent = [0, 0, 14, 14];
        h.ray.settings.resolution = 50;
        let guided = h.pixels(frame, &empty);
        h.ray.settings.denoiser = Denoiser::None;
        let bilinear = h.pixels(frame, &empty);
        let at = 7 * 14 + 7; // exactly between source texels 3 and 4
        assert_eq!(guided[at], left, "{label}: upscale bled into left");
        assert_eq!(guided[at + 1], right, "{label}: upscale bled into right");
        assert!(bilinear[at].0[0] < left.0[0] - 10);
        assert!(guided.iter().zip(&bilinear).all(|(a, b)| a.1 == b.1));
    }
    h.ray.settings.resolution = 100;
}

fn exercise_sun_and_history(h: &mut Harness) {
    const SIDE: usize = 33;
    let saved_single_path = h.ray.settings.single_path;
    h.ray.settings.single_path = true; // the primary disc uses shifted Sobol
    let empty = Builder::default().finish();
    let scene = |block| {
        let mut b = Builder::default();
        let wall = quad(
            &mut b,
            [
                [-4., 4., -5.],
                [4., 4., -5.],
                [-4., -4., -5.],
                [4., -4., -5.],
            ],
            [0., 0., 1.],
            0,
            0,
            u32::MAX,
        );
        put(&mut b, wall);
        if block {
            // A straight edge outside the primary view, 30 units toward the
            // sun. The disc has radius ~0.141 in this plane; x=0 sees only
            // the slice on the right of a 0.025-unit obstruction.
            let shader_edge = quad(
                &mut b,
                [
                    [-8., 35., 13.],
                    [0.025, 35., 13.],
                    [-8., 13., 13.],
                    [0.025, 13., 13.],
                ],
                [0., 0., -1.],
                0,
                0,
                u32::MAX,
            );
            put(&mut b, shader_edge);
        }
        (b.finish(), Textures::default())
    };
    h.ray.install(Ok(scene(true)));
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, SIDE as u32);
    frame.ambient = [0.; 4];
    frame.sun = [0., 0.8, 0.6, 1.];
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    let column = |y| y * SIDE + SIDE / 2;
    let center = column(SIDE / 2);
    let hard = h.pixels(frame, &empty);
    let full = hard[center + 5].0[0];
    assert!(full > 100);
    assert_eq!(hard[center].0[0], 0, "point sun did not cast a hard shadow");
    h.ray.settings.soft_sun = true;
    let grain = h.pixels(frame, &empty);
    h.ray.settings.denoiser = Denoiser::Svgf;
    let smooth = h.pixels(frame, &empty);
    let stats = |pixels: &[([u8; 4], f32)]| {
        let row = (2..SIDE - 2).map(|y| f32::from(pixels[column(y)].0[0]) / f32::from(full));
        let mean = row.clone().sum::<f32>() / (SIDE - 4) as f32;
        let variance = row.map(|v| (v - mean).powi(2)).sum::<f32>() / (SIDE - 4) as f32;
        (mean, variance)
    };
    let (_, noisy_variance) = stats(&grain);
    let (_, smooth_variance) = stats(&smooth);
    assert!(
        noisy_variance > 0.1,
        "finite sun did not sample both sides of the edge"
    );
    assert!(
        smooth_variance < 0.55 * noisy_variance,
        "filter did not reduce flat-surface noise"
    );
    assert!(smooth.iter().zip(&grain).all(|(a, b)| a.1 == b.1));
    assert_eq!(smooth[center - 5].0[0], 0);
    assert!(smooth[center + 5].0[0].abs_diff(full) <= 1);

    // Compare the history to independent draws of the same transport seeds.
    // Build the expected EMA on the CPU (linear target, with 8-bit rounding).
    const FRAMES: u32 = 12;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = true;
    let mut expected = vec![(0., 0.); SIDE * SIDE];
    for i in 1..=FRAMES {
        h.ray.reset_history();
        for ((mean, variance), pixel) in expected.iter_mut().zip(h.pixels(frame, &empty)) {
            let noise = f32::sqrt(*variance) / f32::max(0.04, *mean);
            let t = ((noise - 0.05) / 0.35).clamp(0., 1.);
            let alpha = f32::max(1. / i as f32, 0.10 - 0.065 * t * t * (3. - 2. * t));
            let delta = f32::from(pixel.0[0]) / 255. - *mean;
            *variance = (1. - alpha) * (*variance + alpha * delta * delta);
            *mean += alpha * delta;
        }
    }
    h.ray.history = post::History::default();
    let mut converged = Vec::new();
    for _ in 0..FRAMES {
        converged = h.pixels(frame, &empty);
    }
    for ((actual, expected), reference) in converged.iter().zip(expected).zip(&grain) {
        assert!((f32::from(actual.0[0]) - expected.0 * 255.).abs() <= 1.5);
        assert_eq!(actual.1, reference.1, "history changed primary depth");
    }
    let (lit_fraction, _) = stats(&converged);
    let x = 0.025 / (30. * 0.27_f32.to_radians());
    let analytic = (x.acos() - x * (1. - x * x).sqrt()) / std::f32::consts::PI;
    assert!(
        (lit_fraction - analytic).abs() < 0.09,
        "disc slice {lit_fraction}, expected {analytic}"
    );
    eprintln!("Lighter: penumbra visibility {lit_fraction:.3} (analytic {analytic:.3}); spatial variance {noisy_variance:.4} -> {smooth_variance:.4}");

    // Identical receiver depth/normal/albedo, but the camera crosses from
    // penumbra to full sun. Per-pixel G-buffer comparison alone wouldn't know.
    let mut shifted = test_frame(Vec3::new(0.4, 0., 0.), -Vec3::Z, SIDE as u32);
    shifted.sun = frame.sun;
    shifted.ambient = frame.ambient;
    assert!(
        h.pixel(shifted, &empty).0[0].abs_diff(full) <= 1,
        "camera left a lighting trail"
    );
    let _ = h.pixels(frame, &empty);
    let _ = h.pixels(frame, &empty);
    h.ray.install(Ok(scene(false)));
    assert!(
        h.pixel(frame, &empty).0[0].abs_diff(full) <= 1,
        "evicted caster left a shadow"
    );

    frame.sun = [0.; 4];
    let lamp = |color| {
        let mut b = Builder::default();
        b.light(Light::point([0., 0., -2., 10.], color));
        b.finish()
    };
    let red = lamp([1., 0., 0., 1.]);
    let green = lamp([0., 1., 0., 1.]);
    for _ in 0..2 {
        assert_radiance("red runtime light", h.pixel(frame, &red), [0.49, 0., 0.]);
    }
    let builds = h.builds();
    assert_radiance(
        "light-only history reset",
        h.pixel(frame, &green),
        [0., 0.49, 0.],
    );
    assert_eq!(h.builds(), builds, "light change rebuilt Vulkan ASes");
    h.ray.settings.single_path = saved_single_path;
}

fn exercise_temporal_animation(h: &mut Harness) {
    let empty = Builder::default().finish();
    let mut bank = Textures::default();
    let tile = |bank: &mut Textures, dims| {
        bank.tile(dims, 2, textures::Filter::Color, |x, y| {
            if x + y == 0 {
                0xff00_00ff
            } else {
                0xff00_ff00
            }
        })
        .unwrap()
    };
    let roof = tile(&mut bank, [2, 1]);
    let scroll = tile(&mut bank, [1, 2]);
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.bounces = 0;
    let mut frame = test_frame(Vec3::ZERO, Vec3::Y, 1);
    frame.sky0 = [roof, roof, roof, 0];
    frame.sky_tint = [1.; 4];
    frame.sky_uv = [0., 0., 0.5, 0.];
    h.ray
        .install(Ok((Builder::default().finish(), bank.clone())));
    for _ in 0..2 {
        assert_eq!(h.pixel(frame, &empty), ([255, 0, 0, 255], 0.));
    }
    frame.sky_uv[0] = 0.5;
    assert_eq!(
        h.pixel(frame, &empty),
        ([0, 255, 0, 255], 0.),
        "animated roof ghosted"
    );

    let mut b = Builder::default();
    let sheet = quad_material(
        &mut b,
        [
            [-1., 1., -2.],
            [1., 1., -2.],
            [-1., -1., -2.],
            [1., -1., -2.],
        ],
        [0., 0., 1.],
        Surface::new(scroll, BAKED | BLEND, 0., 0.).uv_transform([1.; 2], [0., -0.5]),
        u32::MAX,
    );
    put(&mut b, sheet);
    h.ray.install(Ok((b.finish(), bank)));
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 9);
    frame.air[3] = 0.5;
    for _ in 0..2 {
        assert_eq!(h.pixel(frame, &empty), ([255, 0, 0, 255], 0.));
    }
    frame.air[3] = 1.5;
    assert_eq!(
        h.pixel(frame, &empty),
        ([0, 255, 0, 255], 0.),
        "scrolling sheet ghosted"
    );
}

fn exercise_gloss(h: &mut Harness) {
    const SIDE: usize = 15;
    let empty = Builder::default().finish();
    let mut bank = Textures::default();
    let sky = bank
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xff00_0000)
        .unwrap();
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, SIDE as u32);
    frame.sky0 = [sky, sky, sky, 0];
    frame.sky_tint = [1.; 4];
    h.ray.settings.reflections = true;
    h.ray.settings.bounces = 1;
    let mut means = Vec::new();
    for power in [2., 128.] {
        let mut b = Builder::default();
        let material = Surface::new(0, BAKED, 0., 0.).with_gloss(Gloss::authored([1.; 4], power));
        let mirror = quad_material(
            &mut b,
            [
                [-3., 3., -4.],
                [3., 3., -4.],
                [-3., -3., -4.],
                [3., -3., -4.],
            ],
            [0., 0., 1.],
            material,
            rgba([0., 0., 0., 1.]),
        );
        put(&mut b, mirror);
        let red = upright(&mut b, 1., [1., 0., 0., 1.], EMISSIVE, 0);
        put(&mut b, red);
        let green = quad(
            &mut b,
            [
                [-100., 100., 2.],
                [100., 100., 2.],
                [-100., -100., 2.],
                [100., -100., 2.],
            ],
            [0., 0., 1.],
            0,
            EMISSIVE,
            rgba([0., 1., 0., 1.]),
        );
        put(&mut b, green);
        h.ray.install(Ok((b.finish(), bank.clone())));
        let mut pixels = Vec::new();
        for _ in 0..16 {
            pixels = h.pixels(frame, &empty);
        }
        let region = pixels
            .chunks_exact(SIDE)
            .skip(5)
            .take(5)
            .flat_map(|row| &row[5..10]);
        let mut mean = [0.; 2];
        for (rgb, depth) in region {
            for i in 0..2 {
                mean[i] += f32::from(rgb[i]) / (25. * 255.);
            }
            assert!((*depth - depth_at(frame, Vec3::new(0., 0., -4.))).abs() < 1e-5);
        }
        assert!(
            (mean[0] + mean[1] - 0.25).abs() < 0.016,
            "reflection energy {mean:?}"
        );
        means.push(mean);
    }
    assert!(
        means[1][0] > means[0][0] + 0.07 && means[0][1] > means[1][1] + 0.07,
        "increasing authored power did not concentrate the reflection: {means:?}"
    );
    let mut colored = Builder::default();
    let mirror = quad_material(
        &mut colored,
        [
            [-3., 3., -4.],
            [3., 3., -4.],
            [-3., -3., -4.],
            [3., -3., -4.],
        ],
        [0., 0., 1.],
        Surface::new(0, BAKED_COLOR_ONLY | BAKED, 0., 0.)
            .with_gloss(Gloss::authored([1., 0., 0., 1.], 128.)),
        rgba([0., 0., 0., 1.]),
    );
    put(&mut colored, mirror);
    let white = quad(
        &mut colored,
        [
            [-100., 100., 2.],
            [100., 100., 2.],
            [-100., -100., 2.],
            [100., -100., 2.],
        ],
        [0., 0., 1.],
        0,
        EMISSIVE,
        u32::MAX,
    );
    put(&mut colored, white);
    h.ray.install(Ok((colored.finish(), bank)));
    assert_radiance(
        "colored specular on a neutral reflector",
        h.pixel(frame, &empty),
        [0.25, 0., 0.],
    );
    h.ray.settings.reflections = false;
    assert_radiance("reflections disabled", h.pixel(frame, &empty), [0.; 3]);
    eprintln!(
        "Lighter: reflected red/green, power 2 {:?}, power 128 {:?}",
        means[0], means[1]
    );
}

fn exercise_streamed_bmds(h: &mut Harness) {
    let scenes = crate::gpu::streamed_bmd_scenes(storage_limit(&h.device));
    let frame = |pos, dir| {
        let mut f = test_frame(pos, dir, 9);
        f.terrain = scenes.terrain;
        f.options[3] = scenes.area_table;
        f.water = [0.; 4];
        f.water_color = [0.; 4];
        f
    };
    let primary = frame(Vec3::new(112., 9., 5.), Vec3::X);
    let hole = frame(Vec3::new(112., 9., -5.), Vec3::X);
    let mut shadow = frame(Vec3::new(3., 5., 5.), -Vec3::Y);
    shadow.eye[3] = 256.; // sunlight can travel past the raster load radius
    shadow.sun = Vec3::new(1., 0.06, 0.).normalize().extend(1.).to_array();
    let mut water = frame(Vec3::new(146., 13., 5.), Vec3::new(1., -0.5, 0.));
    water.options[0] = WATER_MIRROR;
    let empty = Builder::default().finish();
    h.ray.settings = Settings {
        mode: h.ray.settings.mode,
        lighting: false,
        shadows: false,
        reflections: false,
        bounces: 0,
        samples: 1,
        resolution: 100,
        denoiser: Denoiser::None,
        progressive: false,
        soft_sun: false,
        anisotropy: 8,
        relight_baked: false,
        single_path: false, // deterministic reflection + transmission control
        physical_bsdf: false,
        ..Settings::default()
    };
    let sample = |h: &mut Harness| {
        h.ray.settings.bounces = 0;
        h.ray.settings.reflections = false;
        h.ray.settings.shadows = false;
        let front = h.pixel(primary, &empty);
        let perforation = h.pixel(hole, &empty);
        h.ray.settings.shadows = true;
        let shaded = h.pixel(shadow, &empty);
        h.ray.settings.shadows = false;
        h.ray.settings.bounces = 1;
        let transmitted = h.pixel(water, &empty);
        h.ray.settings.reflections = true;
        let both = h.pixel(water, &empty);
        (front, perforation, shaded, transmitted, both)
    };
    h.ray.install(Ok(scenes.extended));
    let reference = sample(h);
    let (front, perforation, shaded, transmitted, both) = reference;
    assert_eq!(
        front.0,
        [0, 0, 255, 255],
        "no baked facade past the native range"
    );
    assert!((front.1 - depth_at(primary, Vec3::new(160., 9., 5.))).abs() < 1e-6);
    assert_eq!(perforation.1, 0., "distant BMD lost its alpha hole");
    assert!(
        shaded.0[1] < 150 && shaded.1 > 0.,
        "off-axis sun shadow missing: {shaded:?}"
    );
    assert!(
        transmitted.0[2] > 130 && transmitted.1 > 0.,
        "refraction missed the extended asset: {transmitted:?}"
    );
    assert!(
        both.0[2] > transmitted.0[2] && both.1 == transmitted.1,
        "mirror branch missing: {both:?}, {transmitted:?}"
    );

    h.ray.install(Ok(scenes.local));
    assert_eq!(
        sample(h),
        reference,
        "a BMD changed when the native region caught up"
    );
    h.ray.install(Ok(scenes.evicted));
    let (gone, _, lit, bottom, _) = sample(h);
    assert_eq!(gone.1, 0., "evicted BMD still wrote depth");
    assert!(
        lit.0[1] > shaded.0[1] + 100 && lit.1 == shaded.1,
        "evicted BMD still cast a shadow: {lit:?}"
    );
    assert!(
        bottom.0[2] < 5 && bottom.1 == transmitted.1,
        "evicted BMD still visible through water: {bottom:?}"
    );
    h.ray.install(Ok(scenes.restored));
    assert_eq!(
        sample(h),
        reference,
        "returning to the stream region failed to reload the BMD"
    );

    h.ray.install(Ok(scenes.transmit_only));
    let (view, _, shade, refract, no_mirror) = sample(h);
    assert_eq!((view, shade, refract), (front, shaded, transmitted));
    assert!(
        no_mirror.0[2] + 6 < both.0[2] && no_mirror.1 == both.1,
        "reflect flag ignored: {no_mirror:?}, {both:?}"
    );
    h.ray.install(Ok(scenes.reflect_only));
    let (view, _, shade, no_refract, mirror) = sample(h);
    assert_eq!((view, shade), (front, shaded));
    assert_eq!(no_refract, bottom, "refract flag ignored");
    assert!(
        mirror.0[2] > 8 && mirror.0[2] < 32 && mirror.1 == both.1,
        "reflect flag lost: {mirror:?}"
    );
}

/// Change one source at a time and require both correct visibility and the
/// appropriate acceleration action. The same fixture exercises the live
/// renderer code on Software, the CI CPU ray-query ICD and an opt-in GPU.
fn exercise_accel_reuse(h: &mut Harness) {
    fn card(b: &mut Builder, z: f32, x: f32, tint: [f32; 4]) -> Mesh {
        let mut m = Mesh::default();
        let mat = b.surface(Surface::new(0, 0, 0., 0.));
        let tint = rgba(tint);
        for (dx, y) in [(-1., 1.), (1., 1.), (-1., -1.), (1., -1.)] {
            m.vertices
                .push(Vertex::new([x + dx, y, z], [0., 0., 1.], [0.; 2]).tint(tint, tint));
        }
        m.tri([0, 2, 1], mat);
        m.tri([1, 2, 3], mat);
        m
    }
    let mut cache = AccelCache::default();
    let mut world = |order: [usize; 2], tint: [f32; 4]| {
        let mut b = Builder::default();
        for i in order {
            let m = if i == 0 {
                card(&mut b, -8., 0., tint)
            } else {
                card(&mut b, -6., i as f32 * 10., [1.; 4])
            };
            b.chunk(ChunkKey::NearTerrain(i, 0), &m).unwrap();
        }
        b.finish_cached(&mut cache)
    };
    let red = [1., 0., 0., 1.];
    let blue = [0., 0., 1., 1.];
    let install = |h: &mut Harness, packet| h.ray.install(Ok((packet, Textures::default())));
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = false;
    h.ray.settings.progressive = false;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.resolution = 100;
    h.ray.settings.samples = 1;
    let empty = Builder::default().finish();
    let view = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    let panning = test_frame(Vec3::X * 0.25, -Vec3::Z, 3);
    install(h, world([0, 1], red));
    let before = h.pixel(view, &empty);
    assert!(before.0[0] > 245 && before.1 > 0.);
    let builds = h.builds();
    assert_eq!(h.pixel(panning, &empty), before);
    assert_eq!(
        h.builds(),
        builds,
        "panning rebuilt a static acceleration structure"
    );

    install(h, world([1, 0], blue));
    let after = h.pixel(view, &empty);
    assert!(after.0[2] > 245 && (after.1 - before.1).abs() < 1e-6);
    assert_eq!(
        h.builds(),
        builds,
        "reordering, offsets or tint rebuilt an AS"
    );

    install(h, world([2, 0], blue));
    assert_eq!(h.pixel(view, &empty), after);
    if h.ray.settings.mode == Mode::Hardware {
        assert_eq!(h.builds(), [builds[0] + 1, builds[1], builds[2] + 1]);
    }
    install(h, world([1, 0], blue));
    assert_eq!(h.pixel(view, &empty), after);
    if h.ray.settings.mode == Mode::Hardware {
        assert_eq!(
            h.builds(),
            [builds[0] + 2, builds[1], builds[2] + 2],
            "evicted geometry wasn't readmitted"
        );
    }
    assert_eq!(cache.builds, [4, 3]);
    assert_eq!(cache.refits, [0, 0]);

    let mut cache = AccelCache::default();
    let mut pose = |x, blue: bool| {
        let mut b = Builder::default();
        let c = if blue {
            [0., 1., 1., 1.]
        } else {
            [0., 1., 0., 1.]
        };
        let m = card(&mut b, -4., x, c);
        b.posed_world_mesh("fixture", 16, &m).unwrap();
        b.finish_cached(&mut cache)
    };
    let first = h.pixel(view, &pose(0., false));
    assert!(
        first.0[1] > 245
            && first.0[2] < 5
            && (first.1 - depth_at(view, -4. * Vec3::Z)).abs() < 1e-6
    );
    let builds = h.builds();
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let refits = h.ray.hardware.as_ref().map(|rt| rt.as_refits);
    let tinted = h.pixel(view, &pose(0., true));
    assert!(tinted.0[1] > 245 && tinted.0[2] > 245 && (first.1 - tinted.1).abs() < 1e-6);
    assert_eq!(h.builds(), builds, "shading attributes rebuilt an AS");
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    assert_eq!(h.ray.hardware.as_ref().map(|rt| rt.as_refits), refits);
    assert_eq!(
        h.pixel(view, &pose(3., true)),
        after,
        "moved pose still occupies the old bounds"
    );
    assert_eq!(
        h.builds(),
        builds,
        "position-only change rebuilt AS topology"
    );
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    if let Some(rt) = &h.ray.hardware {
        let [a, b, c] = refits.unwrap();
        assert_eq!(rt.as_refits, [a, b + 1, c + 1]);
    }
    let moved_view = test_frame(Vec3::X * 3., -Vec3::Z, 3);
    assert_eq!(
        h.pixel(moved_view, &pose(3., true)),
        tinted,
        "refit did not cover the new pose bounds"
    );
    assert_eq!(cache.builds, [1, 1]);
    assert_eq!(cache.refits, [1, 1]);
}

fn exercise_off_camera_pose(h: &mut Harness) {
    let mut b = Builder::default();
    let receive = upright(&mut b, -8., [1.; 4], 0, 0);
    put(&mut b, receive);
    h.ray.install(Ok((b.finish(), Textures::default())));
    let mut frame = test_frame(Vec3::ZERO, -Vec3::Z, 3);
    frame.ambient = [0.; 4];
    // The production sun must be above the horizon. At z=-5 the ray
    // from the receiver crosses (4, 35/24, -5), inside the first pose.
    frame.sun = [0.768, 0.28, 0.576, 1.];
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.soft_sun = false;
    h.ray.settings.bounces = 0;
    let [before, after] =
        crate::modules::off_camera_npc_packets(Mat4::from_cols_array_2d(&frame.vp));
    let shadow = h.pixel(frame, &before);
    assert_eq!(
        shadow.0,
        [0, 0, 0, 255],
        "off-screen animated NPC did not cast a shadow"
    );
    let builds = h.builds();
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let refs = h.ray.hardware.as_ref().map(|rt| rt.as_refits);
    let lit = h.pixel(frame, &after);
    assert!(
        lit.0[0] > 90,
        "animated caster retained stale shadow: {lit:?}"
    );
    assert_eq!(lit.1, shadow.1);
    assert!((lit.1 - depth_at(frame, -8. * Vec3::Z)).abs() < 1e-6);
    assert_eq!(h.builds(), builds);
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if let Some(rt) = &h.ray.hardware {
        let [a, b, c] = refs.unwrap();
        assert_eq!(rt.as_refits, [a, b + 1, c + 1]);
    }
}

fn exercise_rigid_hanger(h: &mut Harness) {
    let mut builder = Builder::default();
    let mirror_surface = builder.surface(Surface::new(0, 0, 0., 1.));
    let mut mirror = Mesh::default();
    mirror.vertices = [
        [-5., 5., -8.],
        [5., 5., -8.],
        [-5., -5., -8.],
        [5., -5., -8.],
    ]
    .map(|p| Vertex::new(p, [0., 0., 1.], [0.; 2]))
    .to_vec();
    mirror.tri([0, 2, 1], mirror_surface);
    mirror.tri([1, 2, 3], mirror_surface);
    builder.world_mesh(&mirror).unwrap();
    h.ray.install(Ok((builder.finish(), Textures::default())));
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.reflections = true;
    h.ray.settings.bounces = 0;

    let (mut child, _) = crate::model_draw_fixture::fixture();
    let skin = &mut child.skins[0].data;
    skin.materials[0].diffuse = [1., 0., 0., 1.];
    let mesh = &mut skin.meshes[0];
    mesh.rigid_bone = Some(0); // direct virtual/model slot
    mesh.vertices = [[-1., 1., 0.], [1., 1., 0.], [-1., -1., 0.], [1., -1., 0.]]
        .into_iter()
        .map(|position| crate::skin::Vertex {
            position,
            normal: [0., 0., 1.],
            uv: [0.; 2],
            weights: [1., 0., 0., 0.],
            joints: [0; 4],
        })
        .collect();
    mesh.indices = vec![0, 2, 1, 1, 2, 3];
    let (mut parent, _) = crate::model_draw_fixture::fixture();
    let local = Mat4::from_translation(Vec3::NEG_Z * 4.)
        * Mat4::from_rotation_x(0.55)
        * Mat4::from_scale(Vec3::new(1.3, 0.8, 1.));
    parent.skeleton.hooks.push(crate::skeleton::Hook {
        name: "head".into(),
        kind: 0,
        bone: -1,
        matrix: local.to_cols_array(),
    });
    let mut textures = Textures::default();
    let mut cache = AccelCache::default();
    let mut make = |x: f32, hide: bool| {
        let pose = parent
            .bind_pose(Mat4::from_translation(Vec3::X * x))
            .unwrap();
        let child_pose = child
            .bind_pose(parent.hook_matrix(&pose, "head").unwrap())
            .unwrap();
        let meshes = child.meshes_from_pose(&child_pose).unwrap();
        let mut out = super::model_feed::Moving::new(&mut textures);
        out.model(
            "walker",
            1,
            super::model_feed::Posed {
                asset: &child,
                pose: &child_pose,
                meshes: &meshes,
                attached: true,
            },
            |_| !hide,
            &mut |_| Err("fixture white fallback".into()),
        )
        .unwrap();
        let packet = out.finish_cached(&mut cache).unwrap();
        assert!(matches!(packet.meshes[0].key, MeshKey::RigidPart(_)));
        packet
    };
    let frame = test_frame(Vec3::ZERO, Vec3::NEG_Z, 3);
    let red = [255, 0, 0, 255];
    let visible = make(0., false);
    let (front, depth) = h.pixel(frame, &visible);
    assert_eq!(front, red);
    assert!((depth - depth_at(frame, -4. * Vec3::Z)).abs() < 1e-6);
    let builds = h.builds();
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let refs = h.ray.hardware.as_ref().map(|rt| rt.as_refits);
    let hidden = make(0., true);
    assert_eq!(visible.meshes[0].shape, hidden.meshes[0].shape);
    let (behind, depth) = h.pixel(frame, &hidden);
    assert_eq!(behind, [255; 4]);
    assert!((depth - depth_at(frame, -8. * Vec3::Z)).abs() < 1e-6);
    assert_eq!(h.builds(), builds);
    h.ray.settings.bounces = 1;
    let reflection = h.pixel(frame, &hidden);
    assert_eq!(
        reflection,
        (red, depth),
        "first-person head missing from reflection"
    );
    let moved = make(3., true);
    assert_eq!(moved.meshes[0].shape, hidden.meshes[0].shape);
    let miss = h.pixel(frame, &moved);
    assert_ne!(miss.0, red, "rigid child did not follow the hook");
    assert_eq!(miss.1, depth);
    assert_eq!(h.builds(), builds);
    assert_eq!(cache.builds, [1, 1]);
    assert_eq!(cache.refits, [0, 1]);
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if let Some(rt) = &h.ray.hardware {
        let [a, b, c] = refs.unwrap();
        assert_eq!(rt.as_refits, [a, b, c + 2]);
    }
}

#[test]
fn resident_capacity_doubles_up_to_the_device_limit() {
    assert_eq!(grown_capacity(24, 16, 500), 32);
    assert_eq!(grown_capacity(42, 16, 500), 42);
    assert_eq!(grown_capacity(280, 256, 300), 300);
}

fn exercise_resource_reuse(h: &mut Harness) {
    let mut bank = Textures::default();
    let tile = bank
        .tile([2, 2], 2, textures::Filter::Color, |_, _| 0xff0099ff)
        .unwrap();
    let mut b = Builder::default();
    let id = upright(&mut b, -4., [1.; 4], 0, tile);
    put(&mut b, id);
    h.ray.install(Ok((b.finish(), bank)));
    h.ray.settings.progressive = true;
    h.ray.settings.denoiser = Denoiser::Svgf;
    let empty = Builder::default().finish();
    let before = h.pixel(test_frame(Vec3::ZERO, -Vec3::Z, 25), &empty);
    assert!(before.0[0] > 245 && before.0[1] > 70 && before.0[2] < 5 && before.1 > 0.);
    // Never lose earlier texture addresses/pixels when the bank appends.
    h.ray
        .world
        .as_mut()
        .unwrap()
        .textures
        .tile([256, 256], 256, textures::Filter::Color, |x, y| {
            0xff000000 | (x + y)
        })
        .unwrap();
    let resized = test_frame(Vec3::ZERO, -Vec3::Z, 27);
    // Both directions of the retained history/filter bindings must be warm.
    for _ in 0..2 {
        assert_eq!(before, h.pixel(resized, &empty));
    }
    let allocations = h.ray.allocation_counts();
    for e in [27, 21, 25, 27] {
        let frame = test_frame(Vec3::X * 0.1, -Vec3::Z, e);
        let p = h.pixel(frame, &empty);
        assert_eq!(p.0, before.0);
        assert!((p.1 - before.1).abs() < 1.0e-6);
        assert_eq!(
            h.ray.allocation_counts(),
            allocations,
            "steady frame at {e}x{e} allocated resources"
        );
    }
    eprintln!("Lighter: zero steady-frame buffer, texture, binding, and AS creations after image/texture growth");
}

fn exercise_texture_prefix_replacement(h: &mut Harness) {
    let mut base = Textures::default();
    let red = base
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xff0000ff)
        .unwrap();
    base.seal();
    let mut previous = base.fork_prefix();
    let green = previous
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xff00ff00)
        .unwrap();
    previous.seal();
    let empty = Builder::default().finish();
    h.ray.install(Ok((Builder::default().finish(), previous)));
    let mut sky = test_frame(Vec3::ZERO, Vec3::Y, 9);
    sky.sky_tint = [1.; 4];
    let probe = |h: &mut Harness, addr| {
        let mut frame = sky;
        frame.sky0 = [addr, addr, addr, 0];
        h.pixel(frame, &empty)
    };
    assert_eq!(probe(h, red), ([255, 0, 0, 255], 0.));
    assert_eq!(probe(h, green), ([0, 255, 0, 255], 0.));
    let mut replacement = base.fork_prefix();
    let blue = replacement
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xffff0000)
        .unwrap();
    assert_eq!(
        green, blue,
        "exercise a same-size replacement at the old offset"
    );
    replacement.seal();
    h.ray
        .install(Ok((Builder::default().finish(), replacement)));
    assert_eq!(probe(h, red), ([255, 0, 0, 255], 0.));
    assert_eq!(probe(h, blue), ([0, 0, 255, 255], 0.));
    let allocations = h.ray.allocation_counts();
    let yellow = h
        .ray
        .world
        .as_mut()
        .unwrap()
        .textures
        .tile([1, 1], 1, textures::Filter::Color, |_, _| 0xff00ffff)
        .unwrap();
    assert_eq!(
        probe(h, yellow),
        ([255, 255, 0, 255], 0.),
        "appended texels must reach the GPU"
    );
    assert_eq!(probe(h, red), ([255, 0, 0, 255], 0.));
    assert_eq!(
        h.ray.allocation_counts(),
        allocations,
        "the append fit in retained capacity"
    );
}

/// Synthetic chunk-count sweep, not a physical GPU performance threshold.
/// Use the same target/viewport and no per-frame pixel readback in the timings.
fn exercise_chunk_scaling(h: &mut Harness) {
    const SIDE: u32 = 96;
    let extent = wgpu::Extent3d {
        width: SIDE,
        height: SIDE,
        depth_or_array_layers: 1,
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let color = h.target(format, extent).create_view(&Default::default());
    let depth = h
        .target(wgpu::TextureFormat::Depth32Float, extent)
        .create_view(&Default::default());
    h.ray.settings.samples = 1;
    h.ray.settings.lighting = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.denoiser = Denoiser::None;
    h.ray.settings.progressive = false;
    let empty = Builder::default().finish();
    let mut accel = AccelCache::default();
    let mut base = Textures::default();
    let tile = base
        .tile([128, 128], 128, textures::Filter::Color, |_, _| 0xff00ff00)
        .unwrap();
    base.seal();
    let frame = test_frame(Vec3::ZERO, -Vec3::Z, SIDE);
    let meshes: Vec<_> = (0..256)
        .map(|i| {
            let mut mesh = Mesh::default();
            for x in 0..4 {
                for y in 0..4 {
                    let x = (i % 16) as f32 * 2. + x as f32 * 0.5 - 0.75;
                    let y = (i / 16) as f32 * 2. + y as f32 * 0.5 - 0.75;
                    let n = mesh.vertices.len() as u32;
                    mesh.vertices.extend(
                        [
                            [x, y, -4.],
                            [x + 0.5, y, -4.],
                            [x, y + 0.5, -4.],
                            [x + 0.5, y + 0.5, -4.],
                        ]
                        .map(|p| Vertex::new(p, [0., 0., 1.], [0.; 2])),
                    );
                    mesh.tri([n, n + 1, n + 2], 0);
                    mesh.tri([n + 1, n + 3, n + 2], 0);
                }
            }
            mesh
        })
        .collect();
    for count in [16, 64, 256] {
        let mut pack_ms = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let mut builder = Builder::for_mode(h.ray.settings.mode);
            builder.surface(Surface::new(tile, 0, 0., 0.));
            for (i, mesh) in meshes[..count].iter().enumerate() {
                builder.chunk(ChunkKey::Scenery(i as i32, 0), mesh).unwrap();
            }
            let packet = builder.finish_cached(&mut accel);
            let bank = base.fork_prefix();
            pack_ms.push(start.elapsed().as_secs_f64() * 1000.);
            h.ray.install(Ok((packet, bank)));
        }
        let pixel = h.pixel(frame, &empty);
        assert_eq!(pixel.0, [0, 255, 0, 255]);
        assert!((pixel.1 - depth_at(frame, Vec3::NEG_Z * 4.)).abs() < 1e-5);
        let packet_bytes = h.ray.world.as_ref().unwrap().packet.bytes().len();
        let before = h.ray.allocation_counts();
        let mut frame_ms = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            let mut encoder = h.device.create_command_encoder(&Default::default());
            assert!(h
                .ray
                .render(
                    &h.device,
                    &h.queue,
                    &mut encoder,
                    &color,
                    &depth,
                    format,
                    frame,
                    &empty,
                    None,
                )
                .unwrap());
            let submitted = h.queue.submit(Some(encoder.finish()));
            h.device
                .poll(wgpu::Maintain::WaitForSubmissionIndex(submitted));
            frame_ms.push(start.elapsed().as_secs_f64() * 1000.);
        }
        assert_eq!(before, h.ray.allocation_counts());
        pack_ms.sort_unstable_by(f64::total_cmp);
        frame_ms.sort_unstable_by(f64::total_cmp);
        let notice = if std::env::var_os("GITHUB_ACTIONS").is_some() {
            "::notice::"
        } else {
            ""
        };
        eprintln!("{notice}Lighter {:?}, {count} chunks ({} triangles), {SIDE}x{SIDE}: {packet_bytes} packet bytes, median CPU pack {:.3} ms, synchronized frame {:.3} ms", h.ray.settings.mode, count * 32, pack_ms[2], frame_ms[4]);
    }
}

fn exercise_bmd_chunk_join(h: &mut Harness) {
    h.ray.settings.bounces = 0;
    h.ray.settings.reflections = false;
    h.ray.settings.shadows = false;
    let empty = Builder::default().finish();
    let scenes = crate::gpu::joined_bmd_packets(storage_limit(&h.device));
    let mut first = None;
    let mut previous_builds = h.builds();
    for (i, scene) in scenes.into_iter().enumerate() {
        h.ray.install(Ok(scene));
        let row = [127.8, 128., 128.2].map(|x| {
            let frame = test_frame(Vec3::new(x, 3., 0.), Vec3::NEG_Z, 9);
            let p = h.pixel(frame, &empty);
            if i == 2 {
                assert_eq!(p.1, 0., "teleport left a BMD resident");
            } else {
                assert_eq!(p.0, [255; 4], "gap at BMD chunk join x={x}");
                assert!((p.1 - depth_at(frame, Vec3::new(x, 3., -5.))).abs() < 1e-5);
            }
            p
        });
        match i {
            0 => first = Some(row),
            1 => assert_eq!(
                h.builds(),
                previous_builds,
                "promotion rebuilt the same BMD chunks"
            ),
            2 => {}
            _ => assert_eq!(Some(row), first, "return teleport changed the join"),
        }
        previous_builds = h.builds();
    }
}

fn exercise_reprojection_shader(h: &Harness) {
    use wgpu::util::DeviceExt;

    const EXTENT: [u32; 4] = [4, 1, 4, 1];
    // Positive normal half floats in [0,1] for these synthetic values.
    let fp16 = |x: f32| {
        if x == 0. {
            return 0;
        }
        let bits = x.to_bits();
        ((bits.wrapping_sub(112 << 23) + 0x1000) >> 13) & 0xffff
    };
    let from_half = |x: u32| {
        if x == 0 {
            0.
        } else {
            f32::from_bits((x << 13) + (112 << 23))
        }
    };
    let old: [Pixel; 4] = std::array::from_fn(|i| Pixel {
        rg: fp16(0.2 * (i + 1) as f32),
        depth: 0.25,
        view_depth: if i == 2 { 8. } else { 1. },
        normal_flags: 127 << 16,
        owner: 11,
        ..Pixel::zeroed()
    });
    let current: [Pixel; 4] = std::array::from_fn(|i| Pixel {
        rg: fp16(1.) << 16,
        depth: 0.4,
        view_depth: 1.,
        normal_flags: 127 << 16,
        // Pixel 2 reprojects to an identical geometric guide but a new owner.
        owner: if i == 2 { 22 } else { 11 },
        ..Pixel::zeroed()
    });
    let upload = |pixels: &[Pixel]| {
        h.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Reprojection test pixels"),
                contents: bytemuck::cast_slice(pixels),
                usage: wgpu::BufferUsages::STORAGE,
            })
    };
    let old = upload(&old);
    let current = upload(&current);
    let mut post = post::Post::new(&h.device);
    for i in 0..12 {
        let mut encoder = h.device.create_command_encoder(&Default::default());
        post.process(
            &h.device,
            &h.queue,
            &mut encoder,
            &old,
            EXTENT,
            Some(post::HistoryUse {
                reuse: i > 0,
                changed_animations: 0,
                previous_from_current: None,
            }),
            false,
            None,
        );
        h.queue.submit(Some(encoder.finish()));
    }
    let copy = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Reprojection test readback"),
        size: PIXEL_BYTES * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let allocation_counts = post.allocation_counts();
    let mut encoder = h.device.create_command_encoder(&Default::default());
    let resolved = post.process(
        &h.device,
        &h.queue,
        &mut encoder,
        &current,
        EXTENT,
        Some(post::HistoryUse {
            reuse: true,
            changed_animations: 0,
            previous_from_current: Some(Mat4::from_translation(Vec3::new(0.5, 0., -0.15))),
        }),
        false,
        None,
    );
    encoder.copy_buffer_to_buffer(resolved, 0, &copy, 0, PIXEL_BYTES * 4);
    h.queue.submit(Some(encoder.finish()));
    let slice = copy.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    crate::gpu::wait_for_capture_callback(&h.device, rx)
        .unwrap()
        .unwrap();
    let data = slice.get_mapped_range();
    let pixels: &[Pixel] = bytemuck::cast_slice(&data);
    for (i, pixel) in pixels.iter().enumerate() {
        let rgb = [
            from_half(pixel.rg & 0xffff),
            from_half(pixel.rg >> 16),
            from_half(pixel.bv & 0xffff),
        ];
        let expected = if i == 0 {
            [0.352, 0.12, 0.]
        } else {
            [0., 1., 0.]
        };
        assert!(
            rgb.into_iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.001),
            "reprojected RGB {rgb:?} != {expected:?}"
        );
        assert_eq!(pixel.depth, 0.4, "reverse-Z is never filtered");
        assert_eq!(
            pixel.bv >> 16 > 0,
            i == 0,
            "only valid history contributes variance"
        );
    }
    // New input binding can be created; the two history buffers stay resident.
    assert_eq!(allocation_counts.0, post.allocation_counts().0);
}

#[test]
#[ignore = "requires pinned PW142 a07 ECWLD/EC-BSD/water records and Vulkan compute"]
fn lighter_a07_authored_medium() {
    let dir = std::path::PathBuf::from(std::env::var_os("PW_TEST_RETAIL_A07").unwrap());
    let scene = crate::scene::Scene::load(&dir.join("a07.ecwld")).unwrap();
    let areas = crate::water::load(&scene, &dir).unwrap();
    let [area] = areas.as_slice() else {
        panic!("expected one a07 water area")
    };
    assert_eq!(area.export_id, 381);
    let env = crate::environment::Environments::load(&scene, &dir).unwrap();
    let mut h = Harness::new(Mode::Software, false);
    let mut bank = Textures::default();
    let eye = Vec3::new(80., 329., -144.);
    let dry = Vec3::new(80., 332.66, -144.);
    let wet = crate::gpu::ray_test_wet_grid(&mut bank, &areas, eye);
    assert_eq!(wet[1], 1);
    // At z=-144 the authored wet cell centered at -80 ends at x=-64.
    // The adjacent cell centered at -48 is dry, inside the same grid.
    assert!(area.contains_xz(-66., -144.));
    assert!(!area.contains_xz(-60., -144.));
    // Isolate extinction from Fresnel, surface refraction, art, and other
    // lighting. The white receiver is diagnostic geometry at the independently
    // sampled a07 source-height Y at this wet cell center.
    let bed_y = 326.96274;
    let receiver = |dx, dz| [eye.x + dx, bed_y, eye.z + dz];
    let mut b = Builder::default();
    let patch = quad(
        &mut b,
        [
            receiver(-8., 8.),
            receiver(8., 8.),
            receiver(-8., -8.),
            receiver(8., -8.),
        ],
        [0., 1., 0.],
        0,
        EMISSIVE,
        u32::MAX,
    );
    put(&mut b, patch);
    let shore_receiver = |dy, dz| [-58., eye.y + dy, eye.z + dz];
    let patch = quad(
        &mut b,
        [
            shore_receiver(-8., 8.),
            shore_receiver(8., 8.),
            shore_receiver(-8., -8.),
            shore_receiver(8., -8.),
        ],
        [-1., 0., 0.],
        0,
        EMISSIVE,
        u32::MAX,
    );
    put(&mut b, patch);
    h.ray.install(Ok((b.finish(), bank)));
    let empty = Builder::default().finish();
    let from_srgb = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let probes = [
        ("above", dry, -Vec3::Y, 0., 0),
        (
            "surface",
            Vec3::new(eye.x, area.height, eye.z),
            -Vec3::Y,
            0.,
            0,
        ),
        (
            "just-below",
            Vec3::new(eye.x, area.height - 0.125, eye.z),
            -Vec3::Y,
            area.height - 0.125 - bed_y,
            1,
        ),
        ("submerged", eye, -Vec3::Y, eye.y - bed_y, 1),
        ("resurface", dry, -Vec3::Y, 0., 0),
        ("wet-to-dry", Vec3::new(-66., eye.y, eye.z), Vec3::X, 2., 1),
        ("dry-gap", Vec3::new(-60., eye.y, eye.z), Vec3::X, 0., 0),
    ];
    for hour in [12., 0.] {
        for (label, position, direction, dist, body) in probes {
            let fog = env.underwater(position.to_array(), hour);
            let hue = Vec3::from_array(fog.color.map(from_srgb));
            let bias = Vec3::ONE + 0.5 * (Vec3::ONE - hue / hue.max_element().max(1e-5));
            let tr = Vec3::from_array(
                (bias * (-4.321928 * dist / (fog.end - fog.start).max(0.01)))
                    .to_array()
                    .map(f32::exp2),
            );
            let mut frame = test_frame(position, direction, 3);
            frame.resident_bounds = [-512., 512., -512., 512.];
            frame.water_effects =
                crate::gpu::ray_test_wet_grid(&mut Textures::default(), &areas, position);
            assert_eq!(frame.water_effects, [0, body, wet[2], wet[3]], "{label}");
            frame.water = [fog.start, fog.end, 1., 0.];
            frame.water_color = hue.extend(0.).to_array();
            let actual = h.pixel(frame, &empty);
            let expected = hue * (Vec3::ONE - tr) + tr;
            assert_radiance(label, actual, expected.to_array());
            frame.water[2] = 0.;
            let unattenuated = h.pixel(frame, &empty);
            assert_radiance(
                "a07 identical ray with extinction disabled",
                unattenuated,
                [1.; 3],
            );
            assert_eq!(actual.1, unattenuated.1);
            assert_eq!(actual.0 == unattenuated.0, dist == 0.);
            eprintln!("a07 water {} hour {hour} {label}: eye={position} body={body} wet_path={dist:.5} fog=[{},{}] tr={tr} rgba8={:?} control={:?} reverse_z={}",
                area.export_id, fog.start, fog.end, actual.0, unattenuated.0, actual.1);
        }
    }
}

fn exercise_terrain_seam(h: &mut Harness) {
    // Both paths use the viewer's seam builder. Without the fine points in
    // the far triangle the first probe goes straight through the raised rim
    // and hits the red surface on the wrong side.
    h.ray.install(Ok((
        crate::gpu::stitched_terrain_scene(),
        Textures::default(),
    )));
    h.ray.settings.bounces = 0;
    h.ray.settings.shadows = false;
    let empty = Builder::default().finish();
    for (x, dir, hit_x, color) in [
        (20., -Vec3::X, 2.4, [0, 255, 0, 255]),
        (-20., Vec3::X, -4.8, [255, 0, 0, 255]),
    ] {
        let frame = test_frame(Vec3::new(x, 2., 4.), dir, 3);
        let (pixel, depth) = h.pixel(frame, &empty);
        assert_eq!(pixel, color, "grazing ray crossed a terrain seam at x={x}");
        assert!((depth - depth_at(frame, Vec3::new(hit_x, 2., 4.))).abs() < 1e-5);
    }
    let mut bank = Textures::default();
    // One wet owner spans the actual near/far geometry join. It is not a
    // medium boundary: each bed hit should receive the same 4-unit extinction.
    let wet = crate::gpu::ray_test_wet_grid(
        &mut bank,
        &[crate::water::WaterArea {
            export_id: 1,
            color: u32::MAX,
            simple: false,
            height: 8.,
            grid_size: 16.,
            columns: 1,
            rows: 1,
            center: [0., 8., 2.],
            mask: vec![1],
            shore_waves: vec![],
        }],
        Vec3::new(0., 5., 2.),
    );
    h.ray
        .install(Ok((crate::gpu::fine_far_terrain_scene(), bank)));
    for x in [-0.125, 0., 0.125] {
        let mut frame = test_frame(Vec3::new(x, 5., 2.), -Vec3::Y, 3);
        let (color, depth) = h.pixel(frame, &empty);
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        assert!(
            if x < 0. {
                color == red
            } else if x > 0. {
                color == blue
            } else {
                color == red || color == blue
            },
            "gap at the finer far grid join x={x}: {color:?}"
        );
        assert!((depth - depth_at(frame, Vec3::new(x, 1., 2.))).abs() < 1e-5);

        frame.water_effects = wet;
        frame.resident_bounds = [-50., 50., -50., 50.];
        frame.water = [0., 12., 1., 0.];
        let hue = Vec3::from_slice(&frame.water_color);
        let strength = Vec3::ONE + 0.5 * (Vec3::ONE - hue / hue.max_element());
        let tr = Vec3::from_array(
            (strength * (-4.321928 * 4. / 12.))
                .to_array()
                .map(f32::exp2),
        );
        let bed = Vec3::new(color[0] as f32, color[1] as f32, color[2] as f32) / 255.;
        let expected = hue * (Vec3::ONE - tr) + tr * bed;
        let hit = h.pixel(frame, &empty);
        assert_radiance("submerged near/far terrain join", hit, expected.to_array());
        assert!((hit.1 - depth).abs() < 1e-5, "wet join at x={x}: {hit:?}");
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

#[cfg(target_os = "linux")]
fn screen_markers(h: &Harness, side: u32, page: u32) -> [usize; 3] {
    let n = side * side;
    let offset = n * (1 + 3 * page);
    let shader = h.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Lighter screen reuse probe"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"
@group(0) @binding(0) var<storage,read> image: array<u32>;
@group(0) @binding(1) var<storage,read_write> marks: array<u32>;
@compute @workgroup_size(64) fn probe(@builtin(global_invocation_id) id: vec3<u32>) {{
    if id.x>={n}u {{ return; }}
    let at = ({offset}u+id.x*3u)*7u;
    let anchor = image[at+13u]!=0u;
    let source = image[at+12u];
    let gi = (image[at+18u] & 0x80000000u)!=0u;
    marks[id.x] = select(0u,source,anchor) | select(0u,4u,gi);
}}
"#
            )
            .into(),
        ),
    });
    let pipeline = h
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &shader,
            entry_point: "probe",
        });
    let size = u64::from(n) * 4;
    let marks = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let image = match h.ray.settings.mode {
        Mode::Software => &h.ray.software.as_ref().unwrap().output.0,
        Mode::Hardware => h.ray.hardware.as_ref().unwrap().image_for_test(),
        Mode::Off => unreachable!(),
    };
    let bind = h.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[resource(0, image), resource(1, &marks)],
    });
    let mut encoder = h.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(n.div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&marks, 0, &read, 0, size);
    h.queue.submit(Some(encoder.finish()));
    let bytes = read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    bytes.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    crate::gpu::wait_for_capture_callback(&h.device, rx)
        .unwrap()
        .unwrap();
    let mut counts = [0; 3];
    for &bits in bytemuck::cast_slice::<_, u32>(&bytes.get_mapped_range()) {
        counts[0] += usize::from(bits & 3 == 1);
        counts[1] += usize::from(bits & 3 == 2);
        counts[2] += usize::from(bits & 4 != 0);
    }
    counts
}

#[cfg(target_os = "linux")]
fn exercise_screen_reuse(mode: Mode) {
    const SIDE: u32 = 43;
    let mut h = Harness::new(mode, mode == Mode::Hardware);
    h.ray.screen_reuse = SCREEN_DI | SCREEN_GI;
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = true;
    h.ray.settings.progressive = true;
    h.ray.settings.bounces = 0;
    let empty = Builder::for_mode(mode).finish();
    let mut b = Builder::for_mode(mode);
    for (plane, normal) in [
        (
            [
                [-10., 0., -4.],
                [10., 0., -4.],
                [-10., 0., 6.],
                [10., 0., 6.],
            ],
            [0., 1., 0.],
        ),
        (
            [
                [-10., 8., -4.],
                [10., 8., -4.],
                [-10., 0., -4.],
                [10., 0., -4.],
            ],
            [0., 0., 1.],
        ),
    ] {
        let mesh = quad(&mut b, plane, normal, 0, 0, rgba([0.8, 0.8, 0.8, 1.]));
        put(&mut b, mesh);
    }
    // Colored lights with different influence boundaries. The previous
    // neighbor need not have support for all of the current pixel's lights.
    for (i, x) in [-5., -2.5, -1.5, 1.5, 2.5, 5.].into_iter().enumerate() {
        let mut rgb = [0.08; 4];
        rgb[i % 3] = 0.5;
        b.light(Light::point([x, 1.8, -1.7, 4.9], rgb).disk(0.22));
    }
    h.ray.install(Ok((b.finish(), Textures::default())));
    let mut f = test_frame(Vec3::new(0., 1.8, 2.), Vec3::new(0., -0.24, -1.), SIDE);
    f.sun = [0., 0.7, 0.7141428, 1.];
    f.sun_color = [0.16, 0.16, 0.16, 0.];
    f.ambient = [0.; 4];
    // Compile one reuse-capable pipeline, initially with no admitted caches.
    // Then compare identical progressive histories (sequences 0..12) with
    // caches off and on. Keep the full light sum as a separate DI reference.
    let full = h.pixels(f, &empty);
    assert_eq!(h.ray.screen_reuse_flags(), 0);
    h.ray.settings.single_path = true;
    let mean = |p: &[([u8; 4], f32)]| {
        p.iter()
            .map(|(p, _)| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32))
            .sum::<Vec3>()
            / p.len() as f32
    };
    for bounces in 0..=1 {
        h.ray.settings.bounces = bounces;
        h.ray.screen_reuse = 0;
        h.ray.history = post::History::default();
        h.ray.reset_history();
        let first = h.pixels(f, &empty);
        let control = (0..12).map(|_| h.pixels(f, &empty)).last().unwrap();
        assert_eq!(h.ray.screen_reuse_flags(), 0);

        h.ray.screen_reuse = SCREEN_DI | SCREEN_GI;
        h.ray.history = post::History::default();
        h.ray.reset_history();
        assert_eq!(first, h.pixels(f, &empty), "empty-page fallback");
        let requested = SCREEN_DI | u32::from(bounces == 1) * SCREEN_GI;
        assert_eq!(h.ray.screen_reuse_flags(), requested);
        let estimate = (0..12).map(|_| h.pixels(f, &empty)).last().unwrap();
        let previous = SCREEN_PREVIOUS | u32::from(bounces == 1) * SCREEN_GI_PREVIOUS;
        assert_eq!(h.ray.screen_reuse_flags(), requested | previous);
        assert!(control.iter().zip(&estimate).all(|(a, b)| a.1 == b.1));
        assert!(control.iter().zip(&estimate).any(|(a, b)| a.0 != b.0));
        let (expected, actual) = (mean(&control), mean(&estimate));
        assert!(
            (expected - actual).abs().max_element() < 2.0,
            "{mode:?}, {bounces} bounces: fresh {expected:?}, reuse {actual:?}"
        );
        if bounces == 0 {
            assert!((mean(&full) - actual).abs().max_element() < 2.0);
        }
        let abs_sum: u32 = control
            .iter()
            .zip(&estimate)
            .map(|(a, b)| {
                (0..3)
                    .map(|i| u32::from(a.0[i].abs_diff(b.0[i])))
                    .sum::<u32>()
            })
            .sum();
        eprintln!("Lighter {mode:?} {bounces} bounces, 13 progressive frames: fresh vs reused mean RGB8 {expected:?} / {actual:?}, pixel MAE {:.5}", abs_sum as f64 / (3 * control.len()) as f64);
    }
    // Last dispatch used sequence 12 (page zero).
    let marks = screen_markers(&h, SIDE, 0);
    assert!(
        marks.into_iter().all(|x| x > 0),
        "{mode:?}: temporal/spatial/GI hit counts {marks:?}"
    );
    // Pan by multiple cumulative pixels, without a cut. Replay the same
    // poses and seed indices with caches off, using the same GPU pipeline.
    let travel = |mut frame: Frame, i| {
        let shifted = test_frame(
            Vec3::new(0.065 * (i + 1) as f32, 1.8, 2.),
            Vec3::new(0., -0.24, -1.),
            SIDE,
        );
        frame.vp = shifted.vp;
        frame.inv_vp = shifted.inv_vp;
        frame.eye = shifted.eye;
        frame
    };
    // Four separated RNG subsequences at the same eight poses. Compare
    // per-pixel repeat variance (not variance over different scene content),
    // against an otherwise identical progressive, no-cache pipeline.
    const STEPS: usize = 8;
    const SEEDS: [u32; 4] = [0, 96, 208, 512];
    let median = |mut ms: Vec<f64>| {
        ms.sort_by(f64::total_cmp);
        0.5 * (ms[(ms.len() - 1) / 2] + ms[ms.len() / 2])
    };
    for bounces in 0..=1 {
        h.ray.settings.bounces = bounces;
        let requested = SCREEN_DI | u32::from(bounces == 1) * SCREEN_GI;
        let mut durations = [Vec::new(), Vec::new()];
        let mut trials = [Vec::new(), Vec::new()];
        let mut counts = [0; 3];
        for (trial, start) in SEEDS.into_iter().enumerate() {
            // Alternate A/B order. Keep marker readback outside the draw timer.
            for k in 0..2 {
                let route = (k + usize::from(trial == 1 || trial == 2)) % 2;
                let reuse = route == 1;
                h.ray.screen_reuse = if reuse { requested } else { 0 };
                h.ray.history = post::History::from_sequence(start);
                h.ray.reset_history();
                let mut sequence = Vec::new();
                for i in 0..STEPS {
                    let t = Instant::now();
                    let pixels = h.pixels(travel(f, i), &empty);
                    if i != 0 {
                        durations[route].push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    let previous = SCREEN_PREVIOUS
                        | SCREEN_REPROJECTED
                        | u32::from(bounces == 1) * SCREEN_GI_PREVIOUS;
                    assert_eq!(
                        h.ray.screen_reuse_flags(),
                        if reuse {
                            requested | if i == 0 { 0 } else { previous }
                        } else {
                            0
                        }
                    );
                    if reuse && trial == 0 && i > 0 {
                        for (total, n) in
                            counts
                                .iter_mut()
                                .zip(screen_markers(&h, SIDE, (start + i as u32) & 1))
                        {
                            *total += n;
                        }
                    }
                    sequence.extend(pixels);
                }
                trials[route].push(sequence);
            }
        }
        assert!(
            counts[..(2 + bounces as usize)].iter().all(|&n| n > 0),
            "missing moving cache reuse: {counts:?}"
        );
        let mut signed = [0i64; 3];
        let mut squared = 0f64;
        let mut variance = [0.; 2];
        let n = SEEDS.len() as f64;
        let count = trials[0][0].len() as f64;
        for px in 0..trials[0][0].len() {
            for trial in 0..SEEDS.len() {
                assert_eq!(
                    trials[0][trial][px].1, trials[1][trial][px].1,
                    "pan changed current reverse-Z"
                );
            }
            for c in 0..3 {
                let samples = trials
                    .each_ref()
                    .map(|seqs| seqs.iter().map(|seq| f64::from(seq[px].0[c])));
                let sum = samples.clone().map(|values| values.sum::<f64>());
                for (v, (samples, sum)) in variance.iter_mut().zip(samples.into_iter().zip(sum)) {
                    *v += samples.map(|x| (x - sum / n).powi(2)).sum::<f64>() / (n - 1.);
                }
                signed[c] += (sum[1] - sum[0]) as i64;
                squared += (sum[1] - sum[0]).powi(2) / (n * n);
            }
        }
        let bias = signed.map(|x| x as f64 / (count * n));
        let variance = variance.map(|v| v / (3. * count));
        let [no_cache_ms, cache_ms] = durations.map(median);
        eprintln!("Lighter {mode:?} moving, {bounces} bounces, 4×8 frames: mean delta {bias:?} RGB8, RMS of local means {:.4}, repeat variance {variance:?} RGB8², picks {counts:?}, sync draw/readback p50 {no_cache_ms:.2}/{cache_ms:.2} ms", (squared/(3.*count)).sqrt());
        assert!(
            bias.iter().all(|v| v.abs() < 2.),
            "moving cache mean delta: {bias:?}"
        );
    }
    // A teleport invalidates the pages, not the requested feature.
    let shifted = test_frame(Vec3::new(64., 1.8, 2.), Vec3::new(0., -0.24, -1.), SIDE);
    f.vp = shifted.vp;
    f.inv_vp = shifted.inv_vp;
    f.eye = shifted.eye;
    let _ = h.pixels(f, &empty);
    assert_eq!(h.ray.screen_reuse_flags(), SCREEN_DI | SCREEN_GI);
    assert_eq!(
        screen_markers(&h, SIDE, (SEEDS[SEEDS.len() - 1] + STEPS as u32) & 1),
        [0; 3],
        "camera cut must reject both old pages"
    );
    h.ray.settings.progressive = false;
    let _ = h.pixels(f, &empty);
    assert_eq!(h.ray.screen_reuse_flags(), 0);
    eprintln!("Lighter {mode:?}: temporal/spatial/GI picks {marks:?}");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Vulkan compute adapter"]
fn lighter_screen_reuse_pixels() {
    exercise_screen_reuse(Mode::Software);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Vulkan CPU ICD exposing KHR_ray_query"]
fn lighter_screen_reuse_interop() {
    exercise_screen_reuse(Mode::Hardware);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan and a CPU ICD exposing KHR_ray_query"]
fn lighter_compact_terrain_pixels() {
    exercise_compact_terrain(Mode::Software);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Vulkan CPU ICD exposing KHR_ray_query"]
fn lighter_compact_terrain_interop() {
    exercise_compact_terrain(Mode::Hardware);
}

#[cfg(target_os = "linux")]
fn exercise_compact_terrain(mode: Mode) {
    let [w, h] = [33u32, 19u32];
    let mut harn = Harness::new(mode, mode == Mode::Hardware);
    harn.ray.compact_terrain = true;
    let mut camera = test_frame(Vec3::new(0., 3.5, 0.), -Vec3::Y, 37);
    camera.terrain = [128., 128., 1., 128.];
    let empty = Builder::for_mode(mode).finish();
    let geometry = || {
        let mut b = Builder::for_mode(mode);
        let patch = quad(
            &mut b,
            [
                [-20., 0., -20.],
                [20., 0., -20.],
                [-20., 0., 20.],
                [20., 0., 20.],
            ],
            [0., 1., 0.],
            0,
            TERRAIN,
            u32::MAX,
        );
        put(&mut b, patch);
        b.finish()
    };
    for (tag, bytes) in [(b"DXT1", 8), (b"DXT3", 16), (b"DXT5", 16)] {
        let mut raw = vec![0; 128];
        raw[..4].copy_from_slice(b"DDS ");
        raw[84..88].copy_from_slice(tag);
        for (at, n) in [(4, 124u32), (12, h), (16, w), (28, 6), (76, 32), (80, 4)] {
            raw[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        for mip in 0..6 {
            for i in 0..(w >> mip).max(1).div_ceil(4) * (h >> mip).max(1).div_ceil(4) {
                if bytes == 16 {
                    raw.extend_from_slice(&[0; 8]);
                } // ignored alpha
                let a = 0x3595 + i as u16 + (mip as u16) * 0x0800;
                let b = 0xfa2a - (mip as u16) * 0x0400;
                let (a, b) = if i % 2 == 0 { (a, b) } else { (b, a) };
                raw.extend(a.to_le_bytes());
                raw.extend(b.to_le_bytes());
                raw.extend([0xe4, 0x4e, 0x4b, 0x93]); // all four selectors
            }
        }
        let decoded =
            crate::dxt::decode_chain(u32::from_le_bytes(*tag), &raw[128..], w, h, 6).unwrap();
        let reference = textures::Tile::levels(
            decoded.iter().map(|l| ([l.width, l.height], &*l.rgba)),
            16,
            textures::Filter::Color,
        )
        .unwrap();
        let compact = textures::Tile::compact_terrain(&raw, 16).unwrap().unwrap();
        assert!(compact.bytes() * 2 < reference.bytes());
        let mut pack = |tile: &textures::Tile, scale: f32| {
            let mut bank = Textures::default();
            let addr = bank.insert(tile).unwrap();
            let area = bank.reserve(9).unwrap();
            bank.write_words(
                area,
                &[
                    (area + 4) as u32,
                    1,
                    0,
                    0,
                    addr,
                    0,
                    1,
                    scale.to_bits(),
                    scale.to_bits(),
                ],
            );
            camera.options[3] = area as u32;
            let payload = bank.byte_len();
            let stats = bank.mips;
            harn.ray.install(Ok((geometry(), bank)));
            (harn.pixels(camera, &empty), payload, stats)
        };
        for scale in [0.4, 2.1, 9.0] {
            let (a, expanded, _) = pack(&reference, scale);
            let (b, compressed, stats) = pack(&compact, scale);
            assert_eq!(expanded - compressed, stats.bc_terrain_saved_bytes);
            assert_eq!(stats.bc_terrain_tiles, 1);
            assert!(a.iter().any(|p| p.1 > 0. && p.0[0] > 20));
            assert!(
                a.iter().zip(b).all(|(a, b)| a.1 == b.1
                    && a.0.into_iter().zip(b.0).all(|(a, b)| a.abs_diff(b) <= 1)),
                "{mode:?} {:?}, scale {scale}",
                std::str::from_utf8(tag).unwrap()
            );
        }
        eprintln!(
            "Lighter {mode:?} {:?}: terrain tile {} -> {} bytes, RGB8 <= 1, depth exact",
            std::str::from_utf8(tag).unwrap(),
            reference.bytes(),
            compact.bytes()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Vulkan ray-query ICD (lavapipe is sufficient)"]
fn lighter_raster_primary_parity() {
    let mut h = Harness::new(Mode::Hardware, true);
    let mut bank = Textures::default();
    let hole = bank
        .tile([1, 1], 1, textures::Filter::cutout(0.5, false), |_, _| {
            0x00ff_ffff
        })
        .unwrap();
    let mut world = Builder::for_mode(Mode::Hardware);
    let wall = upright(&mut world, -4., [0., 1., 0., 1.], EMISSIVE, 0);
    put(&mut world, wall);
    let reject = upright(&mut world, -2., [0., 0., 1., 1.], CUTOUT, hole);
    put(&mut world, reject);
    let sheet = upright(&mut world, -3.5, [0.25, 0.2, 0.1, 0.25], BLEND, 0);
    world
        .place(
            sheet,
            Transform {
                scale: 0.5,
                ..Transform::translation([0.5, 0.3, 0.])
            },
        )
        .unwrap();
    h.ray.install(Ok((world.finish(), bank)));
    for (i, size) in [19, 25, 43].into_iter().enumerate() {
        let mut moving = Builder::for_mode(Mode::Hardware);
        let small = upright(&mut moving, -2.5, [1., 0., 0., 1.], EMISSIVE, 0);
        moving
            .place(
                small,
                Transform {
                    scale: 0.63,
                    cos: 0.95_f32.sqrt(),
                    sin: 0.05_f32.sqrt(),
                    ..Transform::translation([-0.408 + 0.18 * i as f32, -0.37, 0.])
                },
            )
            .unwrap();
        let moving = moving.finish();
        let frame = test_frame(
            // Keep these parity probes away from subpixel raster edge
            // rounding; exact silhouette/tie rules need not match BVH hits.
            Vec3::new(0.03 * i as f32, 0.113, 0.),
            Vec3::new(0.04, -0.02, -1.),
            size,
        );
        h.ray.settings.hybrid_primary = false;
        let full = h.pixels(frame, &moving);
        assert_eq!(h.ray.travel_primary(), [u64::from(size * size), 0]);
        h.ray.settings.hybrid_primary = true;
        let started = Instant::now();
        let hybrid = h.pixels(frame, &moving);
        assert_eq!(h.ray.travel_primary(), [0, u64::from(size * size)]);
        assert!(
            h.ray.hardware.as_ref().unwrap().raster_active(),
            "render only passed via fallback"
        );
        let error = full
            .iter()
            .zip(&hybrid)
            .map(|(a, b)| (a.1 - b.1).abs())
            .fold(0.0f32, f32::max);
        for (i, ((a, az), (b, bz))) in full.iter().zip(&hybrid).enumerate() {
            assert!(
                (az - bz).abs() < 1e-5,
                "{size}² pixel {i}: reverse-Z {az} != {bz}"
            );
            assert!(
                a.iter().zip(b).all(|(&a, &b)| a.abs_diff(b) <= 1),
                "pixel {i}: {a:?} != {b:?}"
            );
        }
        eprintln!("Lighter raster primary parity: {size}², max reverse-Z error {error:.8}, harness {:.2}ms", started.elapsed().as_secs_f32()*1000.);
    }
    // Exercise real material ownership/ordered ties and a posed occluder
    // outside the raster frustum: secondary/shadow rays still see it.
    exercise_water_art(&mut h);
    exercise_terrain_seam(&mut h);
    exercise_effect_placement(&mut h);
    exercise_off_camera_pose(&mut h);
    assert!(h.ray.raster_active());

    let empty = Builder::for_mode(Mode::Hardware).finish();
    let f = test_frame(Vec3::ZERO, Vec3::NEG_Z, 19);
    h.ray.settings.hybrid_primary = false;
    let full = h.pixels(f, &empty);
    h.ray.settings.hybrid_primary = true;
    h.ray
        .hardware
        .as_mut()
        .unwrap()
        .simulate_primary_allocation_failure([19; 4]);
    assert_eq!(
        h.pixels(f, &empty),
        full,
        "failed raster target did not fall back to full rays"
    );
    assert!(!h.ray.raster_active());
    let next = test_frame(Vec3::ZERO, Vec3::NEG_Z, 21);
    h.pixels(next, &empty);
    assert!(
        h.ray.raster_active(),
        "resize did not retry the raster target"
    );
    h.ray.settings.samples = 2;
    h.pixels(next, &empty);
    assert!(
        !h.ray.raster_active(),
        "multisample fell back to a single raster sample"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan ray query (lavapipe is sufficient)"]
fn lighter_raster_ambient_fallback() {
    let mut h = Harness::new(Mode::Hardware, true);
    let mut world = Builder::for_mode(Mode::Hardware);
    let receiver = upright(&mut world, -3., [1.; 4], 0, 0);
    put(&mut world, receiver);
    h.ray.install(Ok((world.finish(), Textures::default())));
    h.ray.settings.bounces = 0;
    h.ray.settings.lighting = true;
    h.ray.settings.shadows = false;
    let mut frame = test_frame(Vec3::ZERO, Vec3::NEG_Z, 19);
    frame.sun_color = [0.; 4];
    frame.ambient = [0.35, 0.2, 0.1, 0.];
    let moving = Builder::for_mode(Mode::Hardware).finish();
    let full = h.pixel(frame, &moving);
    assert_radiance("full-primary ambient-only receiver", full, [0.35, 0.2, 0.1]);
    assert!((full.1 - depth_at(frame, -3. * Vec3::Z)).abs() < 1e-5);
    h.ray.settings.hybrid_primary = true;
    h.ray
        .hardware
        .as_mut()
        .unwrap()
        .simulate_primary_allocation_failure([19; 4]);
    let fallback = h.pixel(frame, &moving);
    assert_eq!(
        full, fallback,
        "unavailable raster must retain ambient color and depth"
    );
    assert!(!h.ray.raster_active());
    eprintln!("Hybrid unavailable: full-ray ambient RGBA/depth {fallback:?}");
}
