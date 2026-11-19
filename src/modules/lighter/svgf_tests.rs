use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const W: u32 = 32;
const H: u32 = 16;
const TEXT: &str = include_str!("svgf.wgsl");

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Guide {
    normal: [f32; 3],
    depth: f32,
    motion: [f32; 2],
    material: u32,
    valid: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Record {
    signal: [f32; 4],
    moments: [f32; 4],
    count: [f32; 4],
}

#[test]
fn svgf_isolated_effect_kernels_validate() {
    let module = naga_rt::front::wgsl::parse_str(TEXT)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(TEXT)));
    naga_rt::valid::Validator::new(
        naga_rt::valid::ValidationFlags::all(),
        naga_rt::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{}", e.emit_to_string(TEXT)));
}

#[test]
#[ignore = "requires Vulkan; separate reflection/shadow variance and motion fixture"]
fn lighter_svgf_isolated_signal_rmse_and_motion() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_storage_buffers_per_shader_stage: 5,
                ..wgpu::Limits::downlevel_defaults()
            },
        },
        None,
    ))
    .unwrap();
    eprintln!("SVGF fixture adapter: {:?}", adapter.get_info());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Isolated effect fixture"),
        source: wgpu::ShaderSource::Wgsl(TEXT.into()),
    });
    let pipe = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: None,
            module: &shader,
            entry_point: entry,
        })
    };
    let [moments, atrous] = [pipe("moments"), pipe("atrous")];
    let make = |usage, bytes: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytes,
            usage,
        })
    };
    let storage = wgpu::BufferUsages::STORAGE;
    let mut now = Vec::new();
    let mut prior = Vec::new();
    let mut noisy = Vec::new();
    let mut old = Vec::new();
    let mut ref_signal = Vec::new();
    let truth = |right| {
        if right {
            [0.75, 0.75, 0.75, 0.8]
        } else {
            [0.22, 0.22, 0.22, 0.28]
        }
    };
    for i in 0..W * H {
        let (x, y) = (i % W, i / W);
        let right = x >= 16;
        let g = Guide {
            normal: [0., 1., 0.],
            depth: if right { 8. } else { 4. },
            motion: [-1., 0.],
            material: u32::from(right) + 1,
            valid: 1,
        };
        now.push(g);
        let old_right = x >= 15;
        let p = truth(old_right);
        let mut pg = Guide {
            depth: if old_right { 8. } else { 4. },
            material: u32::from(old_right) + 1,
            ..g
        };
        let mut h = Record {
            signal: p,
            moments: [p[0], p[0] * p[0] + 0.02, p[3], p[3] * p[3] + 0.02],
            count: [7.; 4],
        };
        // Unrelated previous surface at one reprojected position: ensure
        // both luminous highlights and dark shadows cannot ghost through ID.
        if x == 2 {
            pg.material += 17;
            h.signal = [1., 1., 1., 0.];
        }
        prior.push(pg);
        old.push(h);
        let r = truth(right);
        ref_signal.push(r);
        let d = if (x + y) % 2 == 0 { 0.18 } else { -0.18 };
        noisy.push(Record {
            signal: r.map(|v| v + d),
            ..Record::zeroed()
        });
    }
    let current = make(storage, bytemuck::cast_slice(&now));
    let previous = make(storage, bytemuck::cast_slice(&prior));
    let raw = make(storage, bytemuck::cast_slice(&noisy));
    let history = make(storage, bytemuck::cast_slice(&old));
    let bytes = (noisy.len() * std::mem::size_of::<Record>()) as u64;
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage,
            mapped_at_creation: false,
        })
    };
    let initial = buffer(storage | wgpu::BufferUsages::COPY_SRC);
    let ping = buffer(storage | wgpu::BufferUsages::COPY_SRC);
    let pong = buffer(storage);
    let temporal_copy = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
    let spatial_copy = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
    let mut enc = device.create_command_encoder(&Default::default());
    let mut run =
        |pipeline: &wgpu::ComputePipeline, step: u32, entries: &[(u32, &wgpu::Buffer)]| {
            let params = make(
                wgpu::BufferUsages::UNIFORM,
                bytemuck::cast_slice(&[W, H, step, 0]),
            );
            let binds: Vec<_> = std::iter::once((0, &params))
                .chain(entries.iter().copied())
                .map(|(binding, b)| wgpu::BindGroupEntry {
                    binding,
                    resource: b.as_entire_binding(),
                })
                .collect();
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &binds,
            });
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(W.div_ceil(8), H.div_ceil(8), 1);
        };
    run(
        &moments,
        0,
        &[
            (1, &current),
            (2, &previous),
            (3, &raw),
            (4, &history),
            (5, &initial),
        ],
    );
    for (i, step) in [1, 2, 4, 8, 16].into_iter().enumerate() {
        let src = if i == 0 {
            &initial
        } else if i % 2 == 1 {
            &ping
        } else {
            &pong
        };
        let dst = if i % 2 == 0 { &ping } else { &pong };
        run(&atrous, step, &[(1, &current), (3, src), (5, dst)]);
    }
    enc.copy_buffer_to_buffer(&initial, 0, &temporal_copy, 0, bytes);
    enc.copy_buffer_to_buffer(&ping, 0, &spatial_copy, 0, bytes);
    queue.submit(Some(enc.finish()));
    let read = |b: &wgpu::Buffer| {
        let view = b.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        view.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        crate::gpu::wait_for_capture(&device, || match rx.try_recv() {
            Ok(r) => r.map(|()| Some(())).map_err(|e| e.to_string()),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(e) => Err(e.to_string()),
        })
        .unwrap();
        bytemuck::cast_slice::<_, Record>(&view.get_mapped_range()).to_vec()
    };
    let temporal = read(&temporal_copy);
    let filtered = read(&spatial_copy);
    assert_eq!(
        temporal[0].signal, noisy[0].signal,
        "out-of-bounds history must reset"
    );
    assert_eq!(
        temporal[3].signal, noisy[3].signal,
        "different material must reset"
    );
    assert_eq!(
        temporal[1].count[0], 8.,
        "one-pixel raster motion should find matching history"
    );
    let rmse = |signal: &[Record], k: usize| {
        (signal
            .iter()
            .zip(&ref_signal)
            .map(|(r, gt)| (r.signal[k] - gt[k]).powi(2))
            .sum::<f32>()
            / signal.len() as f32)
            .sqrt()
    };
    for (k, name) in [(0, "reflection linear RGB"), (3, "sun visibility")] {
        let (before, after) = (rmse(&noisy, k), rmse(&filtered, k));
        assert!(
            after.is_finite() && after < before * 0.5,
            "{name}: {after} >= {before}/2"
        );
        eprintln!("S isolated {name} RMSE vs known expectation: {before:.6} -> {after:.6}");
    }
}
