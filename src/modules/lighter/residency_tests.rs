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
    h.ray.install(Ok((packet, next_bank)));
    let stats = h.ray.last_residency.expect("candidate was not installed");
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
