//! End-to-end proof that the Imaginator's composite changes the frame.
//!
//! The offline harness (`tools/imaginator-check`) replaces `wgpu` and D3D12 with
//! stubs, so it cannot catch a pass bound to the wrong uniform, a no-op blend or
//! a dispatch that writes nothing. This test runs the real shaders on a real
//! device — Mesa's lavapipe in CI — and reads the frame back.
//!
//! It exists because of a string of "the module is enabled and nothing happens"
//! reports that every unit test passed straight through.

use super::composite::{Composite, TraceSource};
use super::settings::ImaginatorSettings;
use crate::imaginator::gpu;

fn settings() -> ImaginatorSettings {
    ImaginatorSettings::default()
}

#[test]
fn the_composite_changes_a_frame_on_a_real_device() {
    if std::env::var_os("PW_IMAGINATOR_SYNTHETIC_GPU").is_none() {
        return;
    }
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .expect("a device to composite on");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("Imaginator composite regression"),
            required_features: wgpu::Features::empty(),
            // The frame's own device, not a downlevel one: the composite is
            // built from `Rgba16Float` and `Rgba32Float` targets, and a
            // downlevel limit set can reject exactly the storage formats the
            // viewer uses, which would test something the user never runs.
            required_limits: adapter.limits(),
        },
        None,
    ))
    .expect("a device to composite on");

    // wgpu panics on an uncaptured error, and its own panic message is one line
    // that a CI annotation truncates. Capture the reason instead, so a validation
    // mistake names the binding and the format it disliked.
    let captured: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    {
        let captured = captured.clone();
        device.on_uncaptured_error(Box::new(move |error| {
            if let Ok(mut slot) = captured.lock() {
                *slot = Some(format!("{error}"));
            }
        }));
    }

    const W: u32 = 64;
    const H: u32 = 64;
    // The frame the core drew: a mid grey wall, already linear because the
    // composite reads it through an sRGB view.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let frame_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frame"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // Exactly what DX12 reports for a swapchain surface, and nothing more:
        // `COLOR_TARGET | COPY_SRC | COPY_DST`, with no `TEXTURE_BINDING`. A test
        // that hands the composite a *sampleable* frame passes where the real one
        // fails, which is why this has to be the restricted set.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        // Declared so the readback can be done through a *linear* view: an
        // `Rgba8UnormSrgb` texture copied to a buffer yields the raw stored
        // bytes, but the values compared below are meant to be the same ones a
        // linear view would show, and declaring both keeps that explicit.
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    let frame_view = frame_texture.create_view(&Default::default());
    // 200/255 sRGB is ~0.57 linear: a lit wall the composite must be able to
    // visibly darken.
    let fill = vec![200u8; (W * H * 4) as usize];
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &frame_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &fill,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(W * 4),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );

    let settings = settings();
    let mut composite = Composite::new(&device, format);
    // `render` refuses to run without grown scene buffers, and it is right to:
    // a trace with no geometry is a sky-only image. Give it one triangle so the
    // buffers exist and the traced channels below are the only thing that varies.
    let scene = std::sync::Arc::new(super::bvh::Scene {
        bvh: Some(super::bvh::Bvh::build(vec![super::bvh::Triangle {
            a: super::math::v3(-10., 0., 2.),
            b: super::math::v3(10., 0., 2.),
            c: super::math::v3(0., 10., 2.),
            normal: super::math::v3(0., 0., -1.),
            material: 0,
        }])),
        materials: vec![super::bvh::Material::new(
            super::math::v3(0.6, 0.6, 0.6),
            0.1,
        )],
        stats: Default::default(),
    });
    composite.sync_scene(&device, &queue, &scene);

    // A trace that says every pixel is fully shadowed, with a little traced
    // colour so the additive term is exercised too.
    let traced_extent = gpu::traced_extent(W, H, settings.trace_scale);
    let mut packed = Vec::with_capacity((traced_extent.0 * traced_extent.1 * 4) as usize);
    for _ in 0..(traced_extent.0 * traced_extent.1) {
        packed.extend_from_slice(&[0.25, 0.04, 0.03, 0.0]);
    }
    composite.upload_software(&packed, traced_extent, &device, &queue);

    let params = gpu::params(
        super::math::v3(0., 1., 0.),
        super::math::v3(0., 0., 1.),
        super::math::v3(1., 0., 0.),
        super::math::v3(0., 1., 0.),
        1.0,
        1.0,
        H as f32,
        super::math::v3(0., -1., 0.),
        super::math::v3(1., 1., 1.),
        super::math::v3(0.4, 0.5, 0.8),
        super::math::v3(0.7, 0.7, 0.75),
        2,
        &settings,
        1000.,
        // The fog distance the composite fades traced light with.
        1000.,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    composite
        .render(
            &device,
            &queue,
            &mut encoder,
            // `Some`, not `None`: production always passes the swapchain image,
            // and that is the path that copies the frame into a sampleable scratch
            // texture first. Testing only `None` left the copy - and everything
            // that depends on the surface's usages - completely unexercised, which
            // is how a whole release can compose nothing while the test passes.
            Some(&frame_texture),
            &frame_view,
            (W, H),
            &settings,
            params,
            TraceSource::Software,
        )
        .expect("the composite must accept an sRGB frame and a packed trace");
    queue.submit(Some(encoder.finish()));
    if let Some(why) = captured.lock().ok().and_then(|slot| slot.clone()) {
        panic!("the composite submitted an invalid command: {why}");
    }

    // Read the result back through a linear (non-sRGB) view, so the values
    // compared are the 8-bit ones actually written.
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (W * H * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &frame_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let pixels = readback.slice(..).get_mapped_range().to_vec();

    // The wall was ~0.55 linear. A full shadow at the default strength must
    // leave it markedly darker; if the composite were a no-op this would be
    // 200 and the assertion below would fail with the value in the message.
    let centre = {
        let offset = (((H / 2) * W + W / 2) * 4) as usize;
        pixels[offset] as u32
    };
    assert!(
        centre < 190,
        "the composite left the frame unchanged ({centre}); the shadowed trace \
             never reached the pixels"
    );
    // The first upload occurs before render sizes the full-resolution targets.
    // A recreated/cleared traced texture still darkens the frame, so checking
    // only "not a no-op" misses it. Require the uploaded red term as well.
    assert!(
        centre > 150,
        "the uploaded trace was lost on target resize: {centre}"
    );
    assert!(
        centre > 0,
        "the composite wrote black ({centre}) instead of a darkened frame"
    );
    readback.unmap();

    // The full-image path must replace, not tint, a white raster frame. Use a
    // distinctly coloured synthetic traced image so a cleared texture or a
    // retained raster frame cannot accidentally pass this check.
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &frame_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &vec![255u8; (W * H * 4) as usize],
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(W * 4),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    let mut full = settings.clone();
    full.output = super::settings::Output::TraceOnly;
    let mut full_pixels = Vec::with_capacity(packed.len());
    for _ in 0..(traced_extent.0 * traced_extent.1) {
        full_pixels.extend_from_slice(&[0.10, 0.30, 0.02, 1.0]);
    }
    composite.upload_software(&full_pixels, traced_extent, &device, &queue);
    let mut encoder = device.create_command_encoder(&Default::default());
    composite
        .render(
            &device,
            &queue,
            &mut encoder,
            Some(&frame_texture),
            &frame_view,
            (W, H),
            &full,
            params,
            TraceSource::Software,
        )
        .expect("full trace composite");
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &frame_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let pixels = readback.slice(..).get_mapped_range();
    let offset = (((H / 2) * W + W / 2) * 4) as usize;
    let (r, g, b) = (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
    assert!(
        g > r && r > b && g < 200,
        "full trace did not replace white raster: {r}, {g}, {b}"
    );
    drop(pixels);
    readback.unmap();

    // A trace-only frame must not copy or sample the raster destination. This
    // target deliberately lacks COPY_SRC and TEXTURE_BINDING, like a restricted
    // surface; either operation would trigger a validation error.
    let restricted = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("trace-only target without copy source"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let restricted_view = restricted.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    composite
        .render(
            &device,
            &queue,
            &mut encoder,
            Some(&restricted),
            &restricted_view,
            (W, H),
            &full,
            params,
            TraceSource::Software,
        )
        .expect("trace-only restricted target");
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::Maintain::Wait);
    if let Some(why) = captured.lock().ok().and_then(|slot| slot.clone()) {
        panic!("trace-only touched the restricted raster target: {why}");
    }
}
