//! Exercise a crossing on the actual upload and trace paths, not just the
//! planner. Read back the old scene between asynchronous submission batches.
use super::*;

fn crossing(mode: Mode, cpu_query: bool) {
    let mut h = Harness::new(mode, cpu_query);
    let mut base = Textures::default();
    let green = base
        .tile([32, 32], 32, textures::Filter::Color, |_, _| 0xff00_ff00)
        .unwrap();
    base.seal();
    let prefix = base.byte_len();
    let mut initial = Builder::default();
    let wall = upright(&mut initial, -5., [1.; 4], 0, green);
    put(&mut initial, wall);
    h.ray.install(Ok((initial.finish(), base.fork_prefix())));
    let e = 9;
    let frame = test_frame(Vec3::ZERO, -Vec3::Z, e);
    let empty = Builder::default().finish();
    let retained = h.pixels(frame, &empty);
    let mid = (e / 2 * e + e / 2) as usize;
    assert_eq!(retained[mid].0, [0, 255, 0, 255]);
    let old_revision = h.ray.revision;
    let before_builds = h.builds();

    let mut next_bank = base.fork_prefix();
    let red = next_bank
        .tile([64, 64], 64, textures::Filter::Color, |_, _| 0xff00_00ff)
        .unwrap();
    next_bank.seal();
    let mut b = Builder::default();
    let wall = upright(&mut b, -5., [1.; 4], 0, green); // same MeshKey
    put(&mut b, wall);
    let subject = upright(&mut b, -3., [1.; 4], 0, red);
    put(&mut b, subject);
    for i in 0..17 {
        let id = upright(&mut b, -8. - i as f32, [1.; 4], 0, 0);
        b.place(id, Transform::translation([20., 0., 0.])).unwrap();
    }
    let packet = b.finish();
    let limit = 8 << 10; // small enough to force several copies for this fixture
    h.ray.residency_transfer_bytes = limit;
    let poll = |h: &mut Harness| {
        h.ray
            .stage_resident(&h.device, &h.queue, &packet, &next_bank)
    };

    // Abandon after the first submission. Opaque color/depth stay drawable,
    // and the next upload can proceed without mistaking an old fence/revision
    // for the new candidate. Explicit first-use install is the control here.
    assert!(!poll(&mut h).unwrap());
    h.ray.install(Ok((
        {
            let mut b = Builder::default();
            let wall = upright(&mut b, -5., [1.; 4], 0, green);
            put(&mut b, wall);
            b.finish()
        },
        base.fork_prefix(),
    )));
    assert_eq!(h.pixels(frame, &empty), retained);
    let submit_revision = h.ray.revision;
    assert_eq!(submit_revision, old_revision + 1);
    let expected_host = (packet.bytes().len() + next_bank.byte_len() - prefix) as u64;
    let expected_batches = (expected_host as usize + prefix).div_ceil(limit);
    let mut pending_frames = 0;
    while !poll(&mut h).unwrap() {
        pending_frames += 1;
        assert!(pending_frames < 32);
        assert_eq!(h.ray.revision, submit_revision);
        assert_eq!(
            h.pixels(frame, &empty),
            retained,
            "partly admitted replacement became visible"
        );
    }
    assert_eq!(h.ray.revision, submit_revision);
    let next_bank_len = next_bank.byte_len() as u64;
    h.ray.install(Ok((packet, next_bank)));
    let stats = h.ray.last_residency.expect("candidate was not installed");
    let [batches, host, reused, peak, blas_batches, builds] = h.ray.travel_pacing().unwrap();
    assert_eq!(
        [batches, host, reused, peak, blas_batches, builds],
        [
            stats.batches,
            stats.host_bytes,
            stats.resident_bytes,
            stats.max_batch_bytes as u64,
            stats.blas_batches,
            stats.blas_builds
        ]
    );
    assert_eq!(h.ray.travel_textures(), [next_bank_len, 0, 0, 0, 0]);
    assert_eq!(stats.batches as usize, expected_batches);
    assert_eq!(stats.max_batch_bytes, limit);
    assert_eq!(stats.resident_bytes, prefix as u64);
    assert_eq!(stats.host_bytes, expected_host);
    if mode == Mode::Hardware {
        assert_eq!(stats.blas_builds, 18);
        assert_eq!(stats.blas_batches, 3);
        assert_eq!(h.builds()[0] - before_builds[0], 18);
        assert_eq!(h.builds()[2], before_builds[2], "no early TLAS publication");
    }
    assert!(pending_frames >= (stats.batches + stats.blas_batches) as usize);
    let admitted = h.pixels(frame, &empty);
    assert_eq!(admitted[mid].0, [255, 0, 0, 255]);
    assert!((admitted[mid].1 - depth_at(frame, Vec3::new(0., 0., -3.))).abs() < 1e-6);
    assert!(admitted[mid].1 > retained[mid].1);
    if mode == Mode::Hardware {
        let built = h.builds();
        assert_eq!(
            built[0] - before_builds[0],
            18,
            "trace repeated staged BLAS builds"
        );
        assert_eq!(built[2] - before_builds[2], 1);
        assert_eq!(h.pixels(frame, &empty), admitted);
        assert_eq!(h.builds(), built);
    }
    eprintln!("Lighter paced {mode:?}: {pending_frames} old-scene redraws; {stats:?}");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan CPU query features; injects two first-use failures"]
fn lighter_failed_start_releases_owned_allocations_twice() {
    for attempt in 1..=2 {
        let mode = Mode::Hardware;
        let mut h = Harness::new(mode, true);
        let mut b = Builder::for_mode(mode);
        let mesh = upright(&mut b, -3., [1.; 4], 0, 0);
        put(&mut b, mesh);
        h.ray.install(Ok((b.finish(), Textures::default())));
        assert!(h.ray.prepare_frame(&h.device, 9, 9).unwrap());
        let stats = h.ray.hardware.as_ref().unwrap().live_probe();
        assert!(stats().0 > 0 && stats().1 > 0);
        h.ray.fail_start_after_prepare = true;
        let device = std::sync::Arc::new(h.device);
        let weak = std::sync::Arc::downgrade(&device);
        let error = h.ray.prepare_window_frame(&device, 9, 9, true).unwrap_err();
        assert!(error.contains("injected Lighter failure"));
        assert_eq!(
            stats(),
            (0, 0),
            "failed start still owns Vulkan allocations"
        );
        assert!(h.ray.hardware.is_none());
        drop(h.ray);
        drop(h.queue);
        drop(device);
        assert!(
            weak.upgrade().is_none(),
            "failed viewer retained the device owner"
        );
        eprintln!("S failed start {attempt}: charged VkDeviceMemory bytes and resource Context owners returned to zero");
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan CPU query features; opt-in first-frame pipeline and BLAS"]
fn lighter_paced_cold_startup() {
    let mode = Mode::Hardware;
    let mut h = Harness::new(mode, true);
    h.ray.paced_startup = true;
    let mut b = Builder::for_mode(mode);
    for i in 0..3 {
        let mesh = upright(&mut b, -3. - i as f32, [1.; 4], 0, 0);
        put(&mut b, mesh);
    }
    h.ray.install(Ok((b.finish(), Textures::default())));
    let device = std::sync::Arc::new(h.device);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    assert!(!h.ray.prepare_window_frame(&device, 9, 9, true).unwrap());
    let mut retries = 0;
    while !h.ray.prepare_window_frame(&device, 9, 9, true).unwrap() {
        assert!(
            std::time::Instant::now() < until,
            "staged Hardware startup stuck"
        );
        retries += 1;
        device.poll(wgpu::Maintain::Poll);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let stats = h.ray.last_residency.unwrap();
    assert!(retries >= 4);
    assert_eq!(stats.blas_batches, stats.blas_builds);
    assert!(stats.blas_builds > 0 && stats.max_batch_bytes <= 1 << 20);
    h.device = std::sync::Arc::try_unwrap(device)
        .ok()
        .expect("worker retained device");
    let pixel = h.pixel(
        test_frame(Vec3::ZERO, -Vec3::Z, 9),
        &Builder::for_mode(mode).finish(),
    );
    assert!(pixel.1 > 0., "staged resident chunk must draw");
}

#[test]
#[ignore = "requires Vulkan compute and texture readback"]
fn lighter_paced_residency_software() {
    crossing(Mode::Software, false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Vulkan KHR_ray_query on a CPU test adapter"]
fn lighter_paced_residency_interop() {
    crossing(Mode::Hardware, true);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires BC1 sampled arrays and Vulkan KHR_ray_query on a CPU ICD"]
fn lighter_bc_texture_pages_interop() {
    let mut h = Harness::new(Mode::Hardware, true);
    h.ray.bc_pages = true;
    h.ray.compact_terrain = true;
    let mut frame = test_frame(Vec3::new(-0.3, 3.5, 0.2), -Vec3::Y, 23);
    frame.terrain = [128., 128., 1., 128.];
    let scene = || {
        let mut b = Builder::for_mode(Mode::Hardware);
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
    let table = |bank: &mut Textures, addr: u32, scale: f32| {
        let at = bank.reserve(9).unwrap();
        bank.write_words(
            at,
            &[
                (at + 4) as u32,
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
        at as u32
    };
    let mut paged = Textures::default();
    let mut buffer = Textures::default();
    let mut rgba = Textures::default();
    let [w, hgt] = [129u32, 87u32];
    let levels = 8;
    let scales = [0.12, 0.7, 5.3];
    let mut offsets = Vec::new();
    for (tag, bytes) in [(b"DXT1", 8), (b"DXT3", 16), (b"DXT5", 16)] {
        let mut raw = vec![0; 128];
        raw[..4].copy_from_slice(b"DDS ");
        raw[84..88].copy_from_slice(tag);
        for (at, n) in [
            (4, 124u32),
            (12, hgt),
            (16, w),
            (28, levels),
            (76, 32),
            (80, 4),
        ] {
            raw[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        for mip in 0..levels {
            for i in 0..(w >> mip).max(1).div_ceil(4) * (hgt >> mip).max(1).div_ceil(4) {
                if bytes == 16 {
                    raw.extend([i as u8; 8]);
                }
                let c = 0x2545 + (i % 57) as u16 + 0x800 * (mip as u16);
                let d = 0xf8bf - 0x400 * (mip as u16);
                // Both endpoint orderings and the equal-endpoint case.
                let (a, b) = match i % 5 {
                    0 => (c, c),
                    1 | 2 => (c, d),
                    _ => (d, c),
                };
                raw.extend(a.to_le_bytes());
                raw.extend(b.to_le_bytes());
                raw.extend([0xe4, 0x93, 0x4e, 0x39]);
            }
        }
        let tile = textures::Tile::bc_terrain(&raw, w, true).unwrap().unwrap();
        let decoded =
            crate::dxt::decode_chain(u32::from_le_bytes(*tag), &raw[128..], w, hgt, levels)
                .unwrap();
        let a = paged.insert_paged_terrain(&tile).unwrap();
        let b = buffer.insert(&tile).unwrap();
        let c = rgba
            .insert(
                &textures::Tile::levels(
                    decoded.iter().map(|l| ([l.width, l.height], &*l.rgba)),
                    w,
                    textures::Filter::Color,
                )
                .unwrap(),
            )
            .unwrap();
        for scale in scales {
            offsets.push([
                table(&mut paged, a, scale),
                table(&mut buffer, b, scale),
                table(&mut rgba, c, scale),
            ]);
        }
    }
    paged.seal();
    let empty = Builder::for_mode(Mode::Hardware).finish();
    let mut draw = |h: &mut Harness, lane| -> Vec<_> {
        offsets
            .iter()
            .map(|at| {
                frame.options[3] = at[lane];
                h.pixels(frame, &empty)
            })
            .collect()
    };
    let expanded = rgba.byte_len();
    let compact = buffer.byte_len();
    let paged_payload = paged.byte_len();
    h.ray.install(Ok((scene(), paged.fork_prefix())));
    let samples = draw(&mut h, 0);
    let stats = h.ray.bc_page_stats();
    assert_eq!(
        stats[0], 3,
        "buffer fallback did not exercise the texture unit"
    );
    assert!(stats[1] > 0 && stats[2] > 0);
    // No re-upload on a differently sized resident suffix with the same
    // sealed terrain pages. Exercise the paced raw VkBuffer replacement.
    let mut replacement = paged.fork_prefix();
    replacement.reserve(500).unwrap();
    let packet = scene();
    crate::gpu::wait_for_capture(&h.device, || {
        h.ray
            .stage_resident(&h.device, &h.queue, &packet, &replacement)
            .map(|ready| ready.then_some(()))
    })
    .unwrap();
    h.ray.install(Ok((packet, replacement)));
    assert_eq!(draw(&mut h, 0), samples);
    assert_eq!(h.ray.bc_page_stats(), stats);
    h.ray.install(Ok((scene(), buffer)));
    let fallback = draw(&mut h, 1);
    // The mismatched bank cannot sample any pages, but the persistent
    // images are still allocated and must be charged to the experiment.
    assert_eq!(h.ray.bc_page_stats(), [0, 0, stats[2]]);
    h.ray.install(Ok((scene(), rgba)));
    let reference = draw(&mut h, 2);
    let mut max = 0;
    let mut sum = 0u64;
    let mut count = 0u64;
    for ((a, b), c) in samples
        .iter()
        .flatten()
        .zip(fallback.iter().flatten())
        .zip(reference.iter().flatten())
    {
        assert_eq!(a.1, b.1);
        assert_eq!(b.1, c.1);
        for channel in 0..3 {
            let delta = a.0[channel].abs_diff(c.0[channel]);
            max = max.max(delta);
            sum += u64::from(delta);
            count += 1;
            assert!(
                b.0[channel].abs_diff(c.0[channel]) <= 1,
                "normalized BC1 palette changed RGB"
            );
        }
    }
    eprintln!("Lighter BC sampled atlas: {stats:?}; RGB8 MAE {:.4}, max {max}; expanded {expanded}, compact {compact}, paged buffer {paged_payload} bytes", sum as f64/count as f64);
    assert!(max <= 4, "unexpected BC/sRGB discrepancy {max} RGB8");
}

pub(super) fn moving_occluder_history(h: &mut Harness) {
    let saved = h.ray.settings;
    h.ray.settings.samples = 1;
    h.ray.settings.resolution = 100;
    h.ray.settings.denoiser = false;
    h.ray.settings.bounces = 0;
    h.ray.settings.lighting = false;
    h.ray.settings.shadows = false;
    h.ray.settings.progressive = true;
    let mode = h.ray.settings.mode;
    let mut b = Builder::for_mode(mode);
    let far = quad(
        &mut b,
        [
            [-8., 8., -6.],
            [8., 8., -6.],
            [-8., -8., -6.],
            [8., -8., -6.],
        ],
        [0., 0., 1.],
        0,
        0,
        rgba([0., 1., 0., 1.]),
    );
    put(&mut b, far);
    h.ray.install(Ok((b.finish(), Textures::default())));
    let mut red = Mesh::default();
    for pos in [
        [-0.42, 0.9, -3.],
        [0.42, 0.9, -3.],
        [-0.42, -0.9, -3.],
        [0.42, -0.9, -3.],
    ] {
        red.vertices
            .push(Vertex::new(pos, [0., 0., 1.], [0.; 2]).tint(0xff00_00ff, 0xff00_00ff));
    }
    red.tri([0, 2, 1], 0);
    red.tri([1, 2, 3], 0);
    let poses: Vec<_> = (0..7)
        .map(|i| {
            let mut moving = Builder::for_mode(mode);
            let material = moving.surface(Surface::new(0, 0, 0., 0.));
            assert_eq!(material, 0);
            let mut posed = red.clone();
            for v in &mut posed.vertices {
                v.pos[0] += 0.37 * (i as f32 - 2.0);
            }
            moving
                .posed_world_mesh("motion-fixture", 2, &posed)
                .unwrap();
            let frame = test_frame(Vec3::X * (0.058 * i as f32), Vec3::NEG_Z, 21);
            (frame, moving.finish())
        })
        .collect();
    let progressive: Vec<_> = poses.iter().map(|(f, m)| h.pixels(*f, m)).collect();
    let retained = ((h.history_state(21 * 10 + 3).0 >> 24) & 63) as usize;
    assert!(
        retained >= 4,
        "the static background did not keep history during travel: {retained}"
    );
    h.ray.settings.progressive = false;
    let mut revealed = 0;
    for (i, ((frame, moving), got)) in poses.iter().zip(&progressive).enumerate() {
        let fresh = h.pixels(*frame, moving);
        assert_eq!(
            *got, fresh,
            "posed foreground left a history trail at travel step {i}"
        );
        if i > 0 {
            revealed += got
                .iter()
                .zip(&progressive[i - 1])
                .filter(|(a, b)| a.1 < b.1)
                .count();
        }
    }
    assert!(revealed > 100);
    eprintln!("Lighter {mode:?} camera + posed travel: {revealed} disoccluded samples match fresh RGB/depth; static history count {retained}");
    h.ray.settings = saved;
}
