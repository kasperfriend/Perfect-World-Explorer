//! CPU Vulkan tests for the *real* trace/pack/submit/composite protocol. Probe
//! writes a known VkImage color: this is not a test of either SDK's algorithms.
use super::*;
use crate::modules::lighter::{scene::*, tests::Harness, Denoiser, Frame};
use bytemuck::Zeroable;
use glam::{Mat4, Vec3};

const SDK_COLOR: [u8; 4] = [64, 128, 191, 255];
const FOV: f32 = 0.7;
const RIGID_SHIFT: [f32; 2] = [0.30, 0.42];

fn card(b: &mut Builder, rect: [f32; 4], z: f32, rgb: [f32; 4], flags: u32) -> Mesh {
    let [left, bottom, right, top] = rect;
    let id = b.surface(Surface::new(0, flags, 0., 0.));
    let mut mesh = Mesh::default();
    for (x, y) in [(left, top), (right, top), (left, bottom), (right, bottom)] {
        mesh.vertices
            .push(Vertex::new([x, y, z], [0., 0., 1.], [0.; 2]).tint(rgba(rgb), rgba(rgb)));
    }
    mesh.tri([0, 2, 1], id);
    mesh.tri([1, 2, 3], id);
    mesh
}

fn placed(b: &mut Builder, mesh: &Mesh) {
    let id = b.mesh(mesh).unwrap().unwrap();
    b.place(id, Transform::IDENTITY).unwrap();
}

fn world(mode: Mode) -> Packet {
    let mut b = Builder::for_mode(mode);
    let ground = card(&mut b, [-5., -5., 5., 2.4], -11., [0.12, 0.2, 0.05, 1.], 0);
    placed(&mut b, &ground);
    // A translucent card overlapping the backdrop but not the two movers.
    let sheet = card(
        &mut b,
        [-0.25, -1.3, 0.25, -0.45],
        -4.,
        [0.2, 0.9, 0.2, 0.7],
        BLEND,
    );
    placed(&mut b, &sheet);
    b.finish()
}

fn moving(mode: Mode, shifted: bool) -> Packet {
    let mut b = Builder::for_mode(mode);
    let rigid = card(
        &mut b,
        [-1.6, -0.85, -0.45, 0.85],
        0.,
        [0.9, 0.1, 0.03, 1.],
        0,
    );
    let mut skinned = card(
        &mut b,
        [0.65, -0.85, 1.6, 0.85],
        -6.,
        [0.05, 0.1, 0.9, 1.],
        0,
    );
    if shifted {
        // A two-bone translation with respective top/bottom weights 3/4 and
        // 1/4. The changing silhouette and interior motion require previous
        // vertex barycentrics, not an actor-wide constant translation.
        for (i, v) in skinned.vertices.iter_mut().enumerate() {
            v.pos[1] += if i < 2 { 0.6 } else { 0.2 };
        }
        b.posed_world_mesh("fixture weighted", 21, &skinned)
            .unwrap();
    }
    let [x, y] = if shifted { RIGID_SHIFT } else { [0.; 2] };
    let before = b.placement_count();
    b.rigid_part(
        &rigid,
        Attachment::new(Mat4::from_translation(Vec3::new(x, y, -7.))).unwrap(),
    )
    .unwrap();
    b.tag_motion(before, MotionOwner("fixture rigid", 2, [0; 32]));
    if !shifted {
        b.posed_world_mesh("fixture weighted", 21, &skinned)
            .unwrap();
    }
    b.finish()
}

fn camera(eye: Vec3, side: u32) -> (Frame, temporal::Camera) {
    let view = Mat4::look_to_rh(eye, -Vec3::Z, Vec3::Y);
    let projection = Mat4::perspective_rh(FOV, 1., 64., 1.);
    let vp = projection * view;
    let camera = temporal::Camera {
        view,
        projection,
        lens: crate::fsr::CameraLens {
            near: 1.,
            far: 64.,
            fov_y_radians: FOV,
        },
        frame_ms: 16.,
    };
    (
        Frame {
            vp: vp.to_cols_array_2d(),
            inv_vp: vp.inverse().to_cols_array_2d(),
            eye: eye.extend(64.).to_array(),
            extent: [0, 0, side, side],
            ..Frame::zeroed()
        },
        camera,
    )
}

fn read(h: &Harness, plane: &Plane, bytes: u32) -> Vec<u32> {
    let [w, hgt] = [plane.texture.width(), plane.texture.height()];
    let row = (w * bytes).div_ceil(256) * 256;
    let buffer = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Lighter temporal input probe"),
        size: u64::from(row * hgt),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = h.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        plane.texture.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(hgt),
            },
        },
        plane.texture.size(),
    );
    h.queue.submit(Some(encoder.finish()));
    let (send, recv) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    crate::gpu::wait_for_capture_callback(&h.device, recv)
        .unwrap()
        .unwrap();
    let range = buffer.slice(..).get_mapped_range();
    range
        .chunks_exact(row as usize)
        .flat_map(|r| {
            r[..(w * bytes) as usize]
                .chunks_exact(4)
                .map(|s| u32::from_ne_bytes(s.try_into().unwrap()))
        })
        .collect()
}

fn epoch(h: &Harness) -> &Epoch {
    h.ray
        .reconstruction
        .as_ref()
        .unwrap()
        .epoch
        .as_ref()
        .unwrap()
}

fn make(mode: Mode, cpu_query: bool, rr: bool) -> Harness {
    let request = Request::Probe { rr };
    let instance = bootstrap::instance(&[], wgpu::InstanceFlags::default()).unwrap();
    let mut h = Harness::on_instance(&instance, mode, cpu_query, |adapter, desc| {
        bootstrap::device(adapter, desc, &request).unwrap()
    });
    Startup(Some(request), Bisect::None, Fallback::default()).install(&mut h.ray, &h.device);
    h.ray.install(Ok((
        world(mode),
        super::super::textures::Textures::default(),
    )));
    h.ray.settings.resolution = 50;
    h.ray.settings.progressive = true;
    h.ray.settings.denoiser = Denoiser::Svgf;
    h
}

fn z_for(at: f32) -> f32 {
    64. / 63. / at - 1. / 63.
}

fn exercise(mode: Mode, cpu: bool) {
    let mut h = make(mode, cpu, true);
    let before = moving(mode, false);
    let moved = moving(mode, true);
    let eye = Vec3::new(0.13, -0.11, 0.);
    for display in [64, 48] {
        let (f, c) = camera(Vec3::ZERO, display);
        let first = h.pixels_with_camera(f, &before, Some(c));
        let e = epoch(&h);
        assert_eq!(e.extent, [display / 2, display / 2, display, display]);
        assert!(e.last_reset && e.input.page() == 1);
        assert!(h.ray.reconstruction_status().is_some());
        assert!(h.ray.post.is_none(), "do not double-filter vendor input");
        let history_jitter = e.last_jitter;
        let d0 = read(&h, &e.input.depth, 4);
        let input_color = read(&h, &e.input.color, 8);
        let valid = read(&h, &e.input.mask, 4);
        let m0 = read(&h, &e.input.velocity, 8);
        assert!(m0.iter().all(|&p| f32::from_bits(p) == 0.));
        let side = display / 2;
        let mut fallback = 0;
        let mut sdk = 0;
        for y in 0..display {
            for x in 0..display {
                let ray = (y / 2 * side + x / 2) as usize;
                let (color, depth) = first[(y * display + x) as usize];
                assert_eq!(depth.to_bits(), d0[ray]);
                if f32::from_bits(valid[ray]) == 1. {
                    assert_ne!(color, SDK_COLOR, "layered pixels must use portable color");
                    fallback += 1;
                } else {
                    assert_eq!(
                        color, SDK_COLOR,
                        "native result must survive wgpu's first image use"
                    );
                    sdk += 1;
                }
            }
        }
        assert!(fallback > 0 && sdk > 0);
        assert!(
            input_color.chunks_exact(2).any(|p| p[0] != 0),
            "pack actual traced linear color"
        );
        let g = e.input.guides.as_ref().unwrap();
        let nr = read(&h, &g[2], 8);
        let sd = read(&h, &g[3], 4);
        assert!(sd.iter().all(|&v| f32::from_bits(v) == 65504.));
        for (i, &z) in d0.iter().enumerate() {
            let z = f32::from_bits(z);
            if z > 0. && valid[i] == 0 {
                // Packed vertex normals round the two zero components up by
                // one octahedral quantization step. Z and roughness are unit.
                let xy = nr[2 * i];
                assert!((xy & 0x7fff) <= 0x211f && ((xy >> 16) & 0x7fff) <= 0x211f);
                assert_eq!(nr[2 * i + 1], 0x3c003c00);
            }
        }

        let (f, c) = camera(eye, display);
        let _ = h.pixels_with_camera(f, &moved, Some(c));
        let e = epoch(&h);
        assert!(!e.last_reset && e.input.page() == 0);
        let mv = read(&h, &e.input.velocity, 8);
        let depth = read(&h, &e.input.depth, 4);
        let mask = read(&h, &e.input.mask, 4);
        let mut counts = [0; 6]; // sky, backdrop, rigid, weighted, depth disocclusion, good history
        for (i, (&d, &m)) in depth.iter().zip(&mask).enumerate() {
            let z = f32::from_bits(d);
            let reject = f32::from_bits(m);
            if reject >= 1. {
                continue;
            }
            let got = [f32::from_bits(mv[2 * i]), f32::from_bits(mv[2 * i + 1])];
            let (class, dist, dx, dy) = if z == 0. {
                (0, 1., eye.x, eye.y) // direction ignores camera translation
            } else if (z - z_for(11.)).abs() < 0.0001 {
                (1, 11., 0., 0.)
            } else if (z - z_for(7.)).abs() < 0.0001 {
                (2, 7., RIGID_SHIFT[0], RIGID_SHIFT[1])
            } else {
                assert!(
                    (z - z_for(6.)).abs() < 0.0001,
                    "unexpected primary depth {z}"
                );
                let y = (i as u32 / side) as f32 + 0.5 - e.last_jitter[1];
                let y_world = eye.y + (1. - 2. * y / side as f32) * 6. * (FOV * 0.5).tan();
                let y_old = (y_world - 0.4) / (1. + 0.4 / 1.7);
                (3, 6., 0., y_world - y_old)
            };
            let k = 0.5 * side as f32 / ((FOV * 0.5).tan() * dist);
            let expected = [(eye.x - dx) * k, (dy - eye.y) * k];
            for [a, b] in [[got[0], expected[0]], [got[1], expected[1]]] {
                assert!((a - b).abs() < 0.008, "{mode:?} extent {side}, class {class} pixel {i}: motion {got:?} vs {expected:?}");
            }
            counts[class] += 1;
            // Independently sample frame one's depth at the previous *jittered*
            // coordinate. This rejects a near occluder but keeps matching depth.
            let previous_xy = [i as u32 % side, i as u32 / side].map(|v| v as f32 + 0.5);
            let previous_xy: [f32; 2] = std::array::from_fn(|j| {
                previous_xy[j] + expected[j] - e.last_jitter[j] + history_jitter[j]
            });
            if previous_xy
                .iter()
                .all(|&v| v > 0. && v < side as f32 && (v - v.round()).abs() > 0.003)
            {
                let at = previous_xy[1] as usize * side as usize + previous_xy[0] as usize;
                let old_z = f32::from_bits(d0[at]);
                let unsuitable = (z - old_z).abs() > 0.0001 || f32::from_bits(valid[at]) == 1.;
                assert_eq!(
                    reject > 0.8,
                    unsuitable,
                    "prior depth mismatch at {i}: {z} vs {old_z}, previous sample {at}"
                );
                if class == 1 && old_z > z + 0.0001 {
                    counts[4] += 1;
                }
                if !unsuitable {
                    counts[5] += 1;
                }
            }
        }
        eprintln!(
            "{mode:?} {side}² temporal probes (sky, static, rigid, weighted, revealed, matched): {counts:?}"
        );
        assert!(counts.into_iter().all(|n| n > 0));
    }
    let (cut, c) = camera(eye + Vec3::X * 100., 48);
    h.pixels_with_camera(cut, &moved, Some(c));
    assert!(epoch(&h).last_reset);
    h.pixels_with_camera(cut, &moved, Some(c));
    assert!(!epoch(&h).last_reset);
    h.ray.install(Ok((
        world(mode),
        super::super::textures::Textures::default(),
    )));
    h.pixels_with_camera(cut, &moved, Some(c));
    assert!(
        epoch(&h).last_reset,
        "published revision rejects prior history"
    );
    check_specular_leg(&mut h, mode);
}

fn check_specular_leg(h: &mut Harness, mode: Mode) {
    let (f, c) = camera(Vec3::ZERO, 48);
    let moving = Builder::for_mode(mode).finish();
    let mut fixture = |layered| {
        let mut b = Builder::for_mode(mode);
        let mut mirror = card(&mut b, [-2., -2., 2., 2.], -5., [0.6, 0.6, 0.6, 1.], 0);
        mirror.surfaces.fill(b.surface(Surface::new(0, 0, 0., 1.)));
        placed(&mut b, &mirror);
        let behind = card(&mut b, [-4., -4., 4., 4.], 5., [0.3, 0.2, 0.1, 1.], 0);
        placed(&mut b, &behind);
        if layered {
            let sheet = card(&mut b, [-4., -4., 4., 4.], 4., [0.8, 0.4, 0.1, 0.5], BLEND);
            placed(&mut b, &sheet);
        }
        h.ray.install(Ok((
            b.finish(),
            super::super::textures::Textures::default(),
        )));
        h.ray.settings.bounces = 1;
        h.ray.settings.reflections = true;
        let pixels = h.pixels_with_camera(f, &moving, Some(c));
        let epoch = epoch(h);
        let guides = epoch.input.guides.as_ref().unwrap();
        let distance = read(h, &guides[3], 4);
        let mask = read(h, &epoch.input.mask, 4);
        let at = 12 * 24 + 12;
        let dist = f32::from_bits(distance[at]);
        let (pixel, depth) = pixels[24 * 48 + 24];
        assert!(
            (z_for(5.) - depth).abs() < 1e-5,
            "specular path length is not primary depth"
        );
        if layered {
            assert_eq!(dist, 65504.);
            assert_eq!(f32::from_bits(mask[at]), 1.);
            assert_ne!(
                pixel, SDK_COLOR,
                "layered reflected path isn't a valid RR hit-distance guide"
            );
        } else {
            assert!(
                (dist - 10.).abs() < 0.04,
                "expected an actual 10-unit first specular segment, got {dist}"
            );
            assert_eq!(mask[at], 0);
            assert_eq!(pixel, SDK_COLOR);
        }
    };
    fixture(false);
    fixture(true);
}

#[test]
#[ignore = "requires a Vulkan ICD; no vendor runtime"]
fn lighter_temporal_software_vulkan() {
    exercise(Mode::Software, false);
}

#[test]
#[ignore = "requires a CPU Vulkan ICD with VK_KHR_ray_query; no vendor runtime"]
fn lighter_temporal_ray_query_vulkan() {
    exercise(Mode::Hardware, true);
}

#[test]
#[ignore = "requires a Vulkan ICD; no vendor runtime"]
fn lighter_temporal_fsr_masks_and_fallback() {
    let mut h = make(Mode::Software, false, false);
    let mode = Mode::Software;
    let (f, c) = camera(Vec3::ZERO, 56);
    let before = moving(mode, false);
    let moved = moving(mode, true);
    h.pixels_with_camera(f, &before, Some(c));
    h.pixels_with_camera(f, &moved, Some(c));
    let e = epoch(&h);
    assert!(!e.last_reset);
    let p = e.input.fsr_masks.as_ref().unwrap();
    let reactive = read(&h, &p[0], 4);
    let comp = read(&h, &p[1], 4);
    assert!(
        reactive
            .iter()
            .zip(&comp)
            .any(|(&r, &m)| f32::from_bits(r) == 0.9 && m == 0),
        "disocclusion is not a composition layer"
    );
    assert!(
        reactive
            .iter()
            .zip(&comp)
            .any(|(&r, &m)| f32::from_bits(r) == 0.9 && f32::from_bits(m) == 1.),
        "authored layer must contribute to both FSR masks"
    );
    // Even a non-black native write from a stand-in is not vendor evidence.
    let label = "Vulkan temporal FSR";
    assert!(require_dispatch(h.ray.reconstruction_status(), label).is_err());
    h.ray
        .reconstruction
        .as_mut()
        .unwrap()
        .epoch
        .as_mut()
        .unwrap()
        .commands
        .fail_submit = true;
    h.pixels_with_camera(f, &moved, Some(c));
    assert!(h.ray.reconstruction_status().is_none());
    assert!(require_dispatch(h.ray.reconstruction_status(), label).is_err());
    assert!(h.ray.reconstruction.as_ref().unwrap().epoch.is_none());
    let failed = h.pixels_with_camera(f, &moved, Some(c));
    let mut control = Harness::new(mode, false);
    control.ray.settings = h.ray.settings;
    control.ray.install(Ok((
        world(mode),
        super::super::textures::Textures::default(),
    )));
    let portable = control.pixels_with_camera(f, &moved, Some(c));
    assert_eq!(
        failed, portable,
        "after SDK rejection portable color and depth return bit-exactly"
    );
    h.ray.settings.reference = true;
    h.pixels_with_camera(f, &before, Some(c));
    assert!(h.ray.reconstruction_status().is_none());
}

fn require_dispatch(active: Option<&str>, requested: &str) -> R<()> {
    if active == Some(requested) {
        Ok(())
    } else {
        Err(format!(
            "SDK must not silently fall back: requested {requested}, active {active:?}"
        ))
    }
}

fn check_sdk_radiance(
    extent: [u32; 4],
    pixels: &[u32],
    mask: &[u32],
    depth: &[u32],
) -> R<(usize, usize)> {
    let [rw, rh, w, h] = extent.map(|n| n as usize);
    let mut checked = 0;
    let mut nonzero = 0;
    if w == 0
        || h == 0
        || rw == 0
        || rh == 0
        || pixels.len() != w * h * 2
        || mask.len() != rw * rh
        || depth.len() != rw * rh
    {
        return Err("SDK readback extent mismatch".into());
    }
    for (i, p) in pixels.chunks_exact(2).enumerate() {
        let at = (i / w * rh / h) * rw + i % w * rw / w;
        if f32::from_bits(mask[at]) >= 1. || f32::from_bits(depth[at]) <= 0. {
            continue;
        }
        let rgb = [p[0] & 0xffff, p[0] >> 16, p[1] & 0xffff];
        if rgb.iter().any(|v| v & 0x7c00 == 0x7c00) {
            return Err("nonfinite SDK radiance".into());
        }
        checked += 1;
        nonzero += usize::from(rgb.iter().any(|v| (1..0x7c00).contains(v)));
    }
    if checked <= 100 || nonzero <= 100 {
        return Err("missing SDK output on supported opaque pixels".into());
    }
    Ok((nonzero, checked))
}

#[test]
fn reconstruction_bisect_gates_opt_in_loads_before_path_lookup() {
    assert!(Bisect::parse("ffx-loader,ngx-loader").is_err());
    for (mode, skip) in [
        ("fsr", Bisect::Bootstrap),
        ("rr", Bisect::Bootstrap),
        ("fsr", Bisect::FfxLoader),
        ("rr", Bisect::NgxLoader),
    ] {
        let result = Request::load_with(skip, Ok(mode), |_| panic!("loader was reached"));
        assert!(matches!(result, Err(reason) if reason.contains("diagnostic skipped")));
    }
    assert!(matches!(
        Request::load_with(Bisect::None, Ok("off"), |_| panic!("off must not load")),
        Ok(None)
    ));
    assert!(
        matches!(Request::load_with(Bisect::NgxLoader, Ok("fsr"), |key| Err(key.into())), Err(key) if key=="PW_RAY_FSR_VK_DLL")
    );
}

#[test]
#[ignore = "requires Vulkan CPU; no SDK DLL is loaded"]
fn lighter_reconstruction_bisect_uses_valid_portable_draws() {
    let mut startup = Startup(
        Some(Request::Probe { rr: true }),
        Bisect::RawDevice,
        Fallback::default(),
    );
    let _instance = startup.instance(wgpu::Backends::VULKAN);
    assert!(startup.0.is_none());
    let mode = Mode::Software;
    let mut probe = make(mode, false, true);
    probe.ray.reconstruction.as_mut().unwrap().skip = Bisect::Interop;
    let (frame, lens) = camera(Vec3::ZERO, 32);
    let actors = moving(mode, false);
    let blocked = probe.pixels_with_camera(frame, &actors, Some(lens));
    let session = probe.ray.reconstruction.as_ref().unwrap();
    assert!(session.epoch.is_none() && session.rejected == Some([16, 16, 32, 32]));
    assert!(probe.ray.reconstruction_status().is_none());
    let mut control = Harness::new(mode, false);
    control.ray.settings = probe.ray.settings;
    control.ray.install(Ok((world(mode), Default::default())));
    assert_eq!(
        blocked,
        control.pixels_with_camera(frame, &actors, Some(lens))
    );
}

#[test]
fn sdk_missing_dll_and_same_length_bad_hash_fail_before_loading() {
    let root = std::env::temp_dir().join(format!("pwe-ffx-cbk-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("amd_fidelityfx_upscaler_vk.dll");
    let _ = std::fs::remove_file(&path);
    let missing = ffx::verified_dll(&path).unwrap_err();
    assert!(missing.contains("amd_fidelityfx_upscaler_vk.dll"));
    std::fs::File::create(&path)
        .unwrap()
        .set_len(36_135_696)
        .unwrap();
    let invalid = ffx::verified_dll(&path).unwrap_err();
    assert!(invalid.contains("pinned SHA-256"), "{invalid}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sdk_fallback_is_one_shot_and_missing_fsr_stays_portable() {
    let path = PathBuf::from("pwe-missing-vulkan-upscaler-for-fallback-test.dll");
    assert!(!path.exists());
    let mut f = Fallback(Some(path));
    assert!(f.try_fsr("injected RR error").is_none());
    assert!(
        f.0.is_none(),
        "a missing FSR must not reload on every frame"
    );
    assert!(f.try_fsr("previously failed").is_none());
}

#[test]
fn sdk_unsupported_adapter_and_fallback_are_not_dispatch() {
    let cpu = vk::PhysicalDeviceProperties {
        device_type: vk::PhysicalDeviceType::CPU,
        ..Default::default()
    };
    assert!(check_adapter(&cpu, false)
        .unwrap_err()
        .contains("physical Vulkan GPU"));
    assert!(check_adapter(&cpu, true).is_err());
    let amd = vk::PhysicalDeviceProperties {
        device_type: vk::PhysicalDeviceType::DISCRETE_GPU,
        vendor_id: 0x1002,
        ..Default::default()
    };
    assert!(check_adapter(&amd, true).unwrap_err().contains("NVIDIA"));
    assert!(require_dispatch(None, "Vulkan temporal FSR").is_err());
    assert!(require_dispatch(
        Some("Vulkan reconstruction interop fixture"),
        "Vulkan temporal FSR"
    )
    .is_err());
}

#[test]
fn sdk_smoke_rejects_black_and_nonfinite_raw_output() {
    let mask = [0; 256];
    let depth = [0.5f32.to_bits(); 256];
    let extent = [16; 4];
    let mut img = vec![0; 512];
    assert!(check_sdk_radiance(extent, &img, &mask, &depth)
        .unwrap_err()
        .contains("missing SDK output"));
    img[0] = 0x7c00;
    assert!(check_sdk_radiance(extent, &img, &mask, &depth)
        .unwrap_err()
        .contains("nonfinite"));
}

#[test]
#[ignore = "requires a Vulkan ICD; deliberately rejects before vendor or shader work"]
fn lighter_temporal_rejected_extent_is_not_dispatch() {
    let mut h = make(Mode::Software, false, false);
    let w = h.device.limits().max_texture_dimension_2d + 1;
    let s = h.ray.reconstruction.as_mut().unwrap();
    let extent = [1, 1, w, 1];
    assert!(s.prepare(&h.device, extent, h.ray.settings).unwrap());
    assert_eq!(s.rejected, Some(extent));
    assert!(s.epoch.is_none());
    assert!(require_dispatch(h.ray.reconstruction_status(), "Vulkan temporal FSR").is_err());
}

// Opt-in acceptance, never the CPU mock. A missing SDK, device, incompatible
// extent, or silent portable fallback must fail this test.
#[test]
#[ignore = "requires a physical Vulkan GPU and explicit FSR or RR SDK setup"]
fn lighter_temporal_vendor_dispatch() {
    let mode = if std::env::var_os("PW_TEST_RAY_QUERY").is_some() {
        Mode::Hardware
    } else {
        Mode::Software
    };
    let mut startup = Startup::new(mode);
    let label = match std::env::var("PW_RAY_RECONSTRUCTION").as_deref() {
        Ok("rr") => "Vulkan DLSS Ray Reconstruction",
        Ok("fsr") => "Vulkan temporal FSR",
        _ => panic!("set PW_RAY_RECONSTRUCTION=rr or fsr and its SDK paths"),
    };
    assert_eq!(
        startup.0.as_ref().map(Request::label),
        Some(label),
        "requested SDK did not load; fallback is not a pass"
    );
    let instance = startup.instance(wgpu::Backends::VULKAN);
    let mut h = Harness::on_instance(&instance, mode, false, |adapter, desc| {
        eprintln!("Vendor reconstruction adapter: {:?}", adapter.get_info());
        startup
            .device(adapter, desc)
            .expect("vendor Vulkan device must be admitted")
    });
    startup.install(&mut h.ray, &h.device);
    h.ray.install(Ok((
        world(mode),
        super::super::textures::Textures::default(),
    )));
    h.ray.settings.resolution = match std::env::var("PW_TEST_RAY_RESOLUTION").as_deref() {
        Ok("100") => 100,
        Ok("50") | Err(std::env::VarError::NotPresent) => 50,
        _ => panic!("PW_TEST_RAY_RESOLUTION must be 50 or 100"),
    };
    let p0 = moving(mode, false);
    let p1 = moving(mode, true);
    for side in [512, 640] {
        let (cut, changed) = camera(-Vec3::Z * 20., side);
        h.pixels_with_camera(cut, &p0, Some(changed));
        require_dispatch(h.ray.reconstruction_status(), label).unwrap();
        let first = epoch(&h);
        let render = side * h.ray.settings.resolution / 100;
        assert_eq!(first.extent, [render, render, side, side]);
        assert!(first.last_reset, "first draw at each extent must reset");
        for i in 0..12 {
            let (frame, lens) = camera(Vec3::X * (i as f32 * 0.013), side);
            let packet = if i < 6 { &p0 } else { &p1 };
            h.pixels_with_camera(frame, packet, Some(lens));
            require_dispatch(h.ray.reconstruction_status(), label).unwrap();
            assert_eq!(epoch(&h).last_reset, i == 0, "cut vs continuous pan");
        }
        let e = epoch(&h);
        // Inspect the SDK image itself, not a composited pixel that could have
        // used portable color. Do this at the end of the epoch: copy-source
        // readback changes the layout normally owned by the compositor.
        let pixels = read(&h, &e.output, 8);
        let mask = read(&h, &e.input.mask, 4);
        let depth = read(&h, &e.input.depth, 4);
        let (nonzero, checked) = check_sdk_radiance(e.extent, &pixels, &mask, &depth).unwrap();
        eprintln!("{label} vendor smoke {side}²: {nonzero}/{checked} opaque pixels have positive SDK radiance");
    }
}

/// P9-a: Fail-closed smoke tests per provider. Each failure mode must fail
/// the smoke test — no silent fallback, no portable substitution.

#[test]
fn smoke_bad_hash_fails() {
    // verified_binary rejects a file whose SHA-256 does not match the pin.
    // A temp file with wrong content must fail verification.
    use std::io::Write;
    let root = std::env::temp_dir().join(format!("pwe-hash-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&root);
    let bad = root.join("bad-sdk.dll");
    std::fs::File::create(&bad)
        .unwrap()
        .write_all(b"wrong content that does not match any pinned SHA-256")
        .unwrap();
    let result = super::verified_binary(&bad, 1024, "deadbeef");
    assert!(result.is_err(), "bad hash must fail verification");
    let _ = std::fs::remove_dir_all(&root);
}
