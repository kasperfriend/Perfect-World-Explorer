use super::*;
use crate::modules::lighter::scene::{Attachment, Builder, Mesh, Surface, Transform, Vertex};
use crate::modules::lighter::{trace_source_with_pages, SOFTWARE};

fn camera(eye: Vec3) -> Camera {
    Camera {
        view: Mat4::look_to_rh(eye, -Vec3::Z, Vec3::Y),
        projection: Mat4::perspective_rh(0.7, 1., 64., 1.),
        lens: CameraLens {
            near: 1.,
            far: 64.,
            fov_y_radians: 0.7,
        },
        frame_ms: 16.,
    }
}

fn key(revision: u64, mode: Mode) -> Continuity {
    Continuity {
        revision,
        texture_bytes: 192,
        mode,
        rr: true,
    }
}

fn frame(e: u32, eye: Vec3) -> Frame {
    Frame {
        extent: [e, e, e * 2, e * 2],
        eye: eye.extend(64.).to_array(),
        options: [0, 0, 1, 0],
        ..Frame::zeroed()
    }
}

#[test]
fn temporal_history_only_commits_accepted_draws() {
    let mut h = History::default();
    let p = Builder::default().finish();
    let cam = camera(Vec3::ZERO);
    let k = key(4, Mode::Software);
    let a = h
        .prepare(&mut frame(23, Vec3::ZERO), cam, k, &p, MOTION_BYTES)
        .unwrap();
    let deferred = h
        .prepare(&mut frame(23, Vec3::ZERO), cam, k, &p, MOTION_BYTES)
        .unwrap();
    assert!(a.reset && deferred.reset);
    assert_eq!(
        a.jitter, deferred.jitter,
        "preflight must not use up a sample"
    );
    h.commit(a);
    let next = h
        .prepare(&mut frame(23, Vec3::ZERO), cam, k, &p, MOTION_BYTES)
        .unwrap();
    assert!(!next.reset);
    assert_ne!(next.jitter, deferred.jitter);
    assert_eq!(
        next.params.current, next.params.previous,
        "jitter is not motion"
    );
    assert_ne!(
        next.params.inverse_jittered,
        cam.projection.inverse().to_cols_array_2d()
    );
    for (cut, extent, continuity) in [
        (Vec3::new(500., 0., 0.), 23, k),
        (Vec3::ZERO, 31, k),
        (Vec3::ZERO, 23, key(5, Mode::Software)),
        (Vec3::ZERO, 23, key(4, Mode::Hardware)),
        (Vec3::ZERO, 23, Continuity { rr: false, ..k }),
        (
            Vec3::ZERO,
            23,
            Continuity {
                texture_bytes: 196,
                ..k
            },
        ),
    ] {
        let reset = h
            .prepare(
                &mut frame(extent, cut),
                camera(cut),
                continuity,
                &p,
                MOTION_BYTES,
            )
            .unwrap();
        assert!(reset.reset);
        assert_eq!(&reset.params.shape[2..], &[0, 0]);
    }
    h.reset(); // explicit renderer/device transition or failed SDK evaluation
    assert!(
        h.prepare(&mut frame(23, Vec3::ZERO), cam, k, &p, MOTION_BYTES)
            .unwrap()
            .reset
    );
}

#[test]
fn temporal_jitter_uses_ray_pixels_and_resets_on_lens_or_lighting_cuts() {
    let mut history = History::default();
    let scene = Builder::default().finish();
    let cam = camera(Vec3::ZERO);
    let stamp = key(1, Mode::Software);
    let f = frame(31, Vec3::ZERO);
    let a = history
        .prepare(&mut { f }, cam, stamp, &scene, MOTION_BYTES)
        .unwrap();
    history.commit(a);
    let mut f = f;
    let b = history
        .prepare(&mut f, cam, stamp, &scene, MOTION_BYTES)
        .unwrap();
    let world_point = Vec4::new(0.5, 0.25, -5., 1.);
    let screens = |vp: Mat4| {
        let clip = vp * world_point;
        (clip.truncate() / clip.w) * Vec3::new(15.5, -15.5, 1.)
    };
    let delta = screens(Mat4::from_cols_array_2d(&f.vp)) - screens(cam.projection * cam.view);
    assert!((delta.x - b.jitter[0]).abs() < 1e-5 && (delta.y - b.jitter[1]).abs() < 1e-5);
    assert!(!b.reset);
    let mut light_cut = frame(31, Vec3::ZERO);
    light_cut.ambient = [0.5, 0.2, 0., 0.];
    assert!(
        history
            .prepare(&mut light_cut, cam, stamp, &scene, MOTION_BYTES)
            .unwrap()
            .reset
    );
    let mut changed = cam;
    changed.lens.far = 72.;
    changed.projection = Mat4::perspective_rh(0.7, 1., 72., 1.);
    assert!(
        history
            .prepare(
                &mut frame(31, Vec3::ZERO),
                changed,
                stamp,
                &scene,
                MOTION_BYTES
            )
            .unwrap()
            .reset
    );
}

#[test]
fn temporal_rigid_instancing_remaps_by_draw_not_blas_or_slot() {
    let packet = |order: &[(u64, f32)], different: bool| {
        let mut b = Builder::default();
        let id = b.surface(Surface::new(0, 0, 0., 0.));
        let mut mesh = Mesh::default();
        for p in [[0., 0., 0.], [1., 1., 0.], [0., 1., 0.]] {
            mesh.vertices.push(Vertex::new(p, [0., 0., 1.], [0., 0.]));
        }
        mesh.tri(if different { [0, 2, 1] } else { [0, 1, 2] }, id);
        for &(owner, x) in order {
            let placement = Attachment::new(Mat4::from_translation(Vec3::new(x, 0., -3.))).unwrap();
            let i = b.placement_count();
            b.rigid_part(&mesh, placement).unwrap();
            b.tag_motion(i, MotionOwner("rigid", owner, [7; 32]));
        }
        b.finish()
    };
    let old = packet(&[(1, 2.), (7, 8.)], false);
    assert_eq!(old.meshes.len(), 1);
    let p = Poses::capture(&old, MOTION_BYTES);
    assert_eq!(p.positions.len(), 9);
    let (n, words) = p.remap(&packet(&[(7, 9.), (1, 3.), (2, 6.)], false), MOTION_BYTES);
    assert_eq!(n, 3);
    assert_eq!(words[7], 8f32.to_bits());
    assert_eq!(words[PLACEMENT_WORDS + 7], 2f32.to_bits());
    assert_eq!(
        words[PLACEMENT_WORDS * 2],
        0,
        "new draw aliases neither existing draw"
    );
    let (_, words) = p.remap(&packet(&[(7, 9.)], true), MOTION_BYTES);
    assert_eq!(
        words[0], 0,
        "a changed index order cannot reuse barycentrics"
    );
    let (_, words) = p.remap(&packet(&[(7, 9.), (7, 3.)], false), MOTION_BYTES);
    assert_eq!([words[0], words[PLACEMENT_WORDS]], [0; 2]);
    assert_eq!(
        p.remap(&old, 16).0,
        0,
        "motion cap falls back rather than overflowing"
    );
    // Ensure the ordinary placed world is never inadvertently tracked.
    let mut b = Builder::default();
    let i = b.surface(Surface::new(0, 0, 0., 0.));
    let mut m = Mesh::default();
    m.vertices = vec![Vertex::new([0.; 3], [0., 1., 0.], [0.; 2]); 3];
    m.tri([0, 1, 2], i);
    let id = b.mesh(&m).unwrap().unwrap();
    b.place(id, Transform::IDENTITY).unwrap();
    assert!(Poses::capture(&b.finish(), MOTION_BYTES)
        .positions
        .is_empty());
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn naga_validates_temporal_variants() {
    use crate::modules::lighter::{vulkan, HARDWARE};
    use naga_rt as naga;
    for source in [
        trace_source_with_pages(SOFTWARE, false, false, "", true),
        [PIXELS, PACK].concat(),
    ] {
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    }
    let code = vulkan::shader(&trace_source_with_pages(HARDWARE, true, false, "", true)).unwrap();
    assert_eq!(code[0], 0x0723_0203);
}
