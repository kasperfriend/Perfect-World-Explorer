//! In-place check for the client's loading-screen art path.
//!
//! This drives the **production** `loading_screen` module (declared by path, not
//! copied) all the way to a GPU surface: archive lookup, random choice, decode,
//! cover fit, the darkened status strip, the BGRA conversion and an upload to a
//! wgpu texture of the size the loading window would use. The texture is read
//! back and compared with the CPU bytes, so a channel-order or row-stride
//! mistake in the path the Windows `StretchDIBits` call consumes fails here
//! instead of showing wrong colours on a real load.
//!
//! It needs no game client: a synthetic `surfaces.pck` is built in memory with a
//! valid v2 directory, a JPEG (`background.jpg`), a BMP and a nested
//! `surfaces/maps/000.dds` that must *not* be chosen, plus a corrupt root JPEG
//! that must fail as a decode rather than a blank picture. The CI GPU job runs
//! it against software Vulkan; `loading-screen-check` covers a staged retail
//! client separately.
use wgpu::util::DeviceExt;

// The production module declares the full artwork API; a check binary uses
// part of it, and an unused helper here is not a defect in the viewer.
#[allow(dead_code)]
#[path = "../loading_screen.rs"]
mod loading_screen;
#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;

const MASK: u32 = 0xa8937462;
const CHECK: u32 = 0x59374231;

/// Build one entry-table record in the shape `pck::directory` parses: length
/// XORed with the mask, a check word, then the 260-byte name and bounds.
fn record(name: &str, offset: u32, size: u32, compressed: u32) -> Vec<u8> {
    let mut data = vec![0u8; 276];
    let bytes = name.as_bytes();
    assert!(bytes.len() < 260, "fixture name must fit the record");
    data[..bytes.len()].copy_from_slice(bytes);
    data[260..264].copy_from_slice(&offset.to_le_bytes());
    data[264..268].copy_from_slice(&size.to_le_bytes());
    data[268..272].copy_from_slice(&compressed.to_le_bytes());
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&((data.len() as u32) ^ MASK).to_le_bytes());
    out.extend_from_slice(&((data.len() as u32) ^ MASK ^ CHECK).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// Assemble a v2 PCK from `(name, payload)` pairs, all stored uncompressed.
///
/// Layout: the 12-byte safe header, then the payloads, then the entry table,
/// then the 280-byte footer. Every offset the reader consumes is absolute from
/// the start of the file, and the `end` field **is** the file length (the reader
/// reads the footer from `end - 280`), so offsets are measured after the safe
/// header and `end` is only known once the footer is appended.
fn build_pck(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    const SAFE_HEADER: usize = 12;
    let mut data = Vec::new();
    let mut table: Vec<u8> = Vec::new();
    for (name, payload) in files {
        let offset = (SAFE_HEADER + data.len()) as u32;
        data.extend_from_slice(payload);
        table.extend_from_slice(&record(
            name,
            offset,
            payload.len() as u32,
            payload.len() as u32,
        ));
    }
    // An entry is only valid while its data ends before `end`, and the entry
    // table begins right here.
    let table_pos = (SAFE_HEADER + data.len()) as u32;
    data.extend_from_slice(&table);
    // The footer sits directly after the table; `end` counts it.
    let end = (SAFE_HEADER + data.len() + 280) as u32;
    let mut footer = vec![0u8; 280];
    footer[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
    footer[8..12].copy_from_slice(&(table_pos ^ MASK).to_le_bytes());
    footer[268..272].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
    footer[272..276].copy_from_slice(&(files.len() as u32).to_le_bytes());
    footer[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
    let mut pck = Vec::with_capacity(end as usize);
    pck.extend_from_slice(&0x4dca23efu32.to_le_bytes());
    pck.extend_from_slice(&end.to_le_bytes());
    pck.extend_from_slice(&0x56a089b7u32.to_le_bytes());
    pck.extend_from_slice(&data);
    pck.extend_from_slice(&footer);
    debug_assert_eq!(pck.len(), end as usize);
    pck
}

fn jpeg() -> Vec<u8> {
    let mut source = image::RgbImage::new(64, 40);
    for (x, y, pixel) in source.enumerate_pixels_mut() {
        // A gradient plus a bright band, so a column or row mistake is visible.
        // `y * 6` reaches 354 on the last row, so the sum is clamped before the
        // `u8` cast instead of wrapping to a dark value that hides the band.
        let band = if (24..40).contains(&y) { 120 } else { 0 };
        *pixel = image::Rgb([
            (x * 4) as u8,
            (y * 6 + band).min(255) as u8,
            200 - (x * 3) as u8,
        ]);
    }
    let mut out = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut out);
        let mut writer = image::codecs::jpeg::JpegEncoder::new_with_quality(cursor, 90);
        writer.encode_image(&source).unwrap();
    }
    out
}

fn bmp() -> Vec<u8> {
    let mut source = image::RgbaImage::new(48, 64);
    for (x, y, pixel) in source.enumerate_pixels_mut() {
        *pixel = image::Rgba([(255 - x * 3) as u8, (y * 4) as u8, 90, 255]);
    }
    let mut out = Vec::new();
    {
        // `BmpEncoder::new` borrows its writer, unlike `JpegEncoder::new`.
        let mut cursor = std::io::Cursor::new(&mut out);
        let mut writer = image::codecs::bmp::BmpEncoder::new(&mut cursor);
        writer
            .encode(source.as_raw(), 48, 64, image::ExtendedColorType::Rgba8)
            .unwrap();
    }
    out
}

/// A DXT1 DDS, the layout `image`'s own DDS decoder accepts.
///
/// The crate decodes DDS through its DXT1/DXT3/DXT5 path only (a FourCC in the
/// pixel format with `DDPF_FOURCC` set) and rejects uncompressed formats, so the
/// fixture is a 16x16 image in four 8x8 blocks, each one solid colour: two
/// RGB565 endpoints and a 32-bit index word whose 16 two-bit selectors are all
/// `0`, which is always the first endpoint.
fn dds() -> Vec<u8> {
    let (width, height) = (16u32, 16u32);
    let mut out = vec![0u8; 128];
    out[0..4].copy_from_slice(b"DDS ");
    // dwSize 124, dwFlags = CAPS|HEIGHT|WIDTH|PIXELFORMAT|LINEARSIZE.
    out[4..8].copy_from_slice(&124u32.to_le_bytes());
    out[8..12].copy_from_slice(&0x0008_100fu32.to_le_bytes());
    out[12..16].copy_from_slice(&height.to_le_bytes());
    out[16..20].copy_from_slice(&width.to_le_bytes());
    // dwPitchOrLinearSize is the top-level compressed size: 8 bytes per block.
    let blocks = (width / 4) * (height / 4);
    out[20..24].copy_from_slice(&(blocks * 8).to_le_bytes());
    // ddspf.dwSize = 32, ddspf.dwFlags = DDPF_FOURCC, ddspf.dwFourCC = "DXT1".
    out[76..80].copy_from_slice(&32u32.to_le_bytes());
    out[80..84].copy_from_slice(&0x4u32.to_le_bytes());
    out[84..88].copy_from_slice(b"DXT1");
    // dwCaps = DDSCAPS_TEXTURE.
    out[108..112].copy_from_slice(&0x1000u32.to_le_bytes());
    // 4x4 blocks of 4x4 texels. A DXT1 block is two RGB565 endpoints then 16
    // two-bit selectors; all selectors are 0, which always picks the first
    // endpoint, so every block is one flat colour. The corner blocks are the
    // four primaries, which the caller asserts after decoding, so a channel
    // swap, a block-order mistake or a short data section cannot pass.
    for index in 0..blocks {
        let (r, g, b) = match index {
            0 => (31u16, 0u16, 0u16),                   // red, top left
            i if i == width / 4 - 1 => (0, 63, 0),      // green, top right
            i if i == blocks - width / 4 => (0, 0, 31), // blue, bottom left
            i if i == blocks - 1 => (31, 63, 31),       // white, bottom right
            _ => (16, 32, 16),                          // mid grey everywhere else
        };
        let endpoint = (r << 11) | (g << 5) | b;
        out.extend_from_slice(&endpoint.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
    }
    out
}

/// Every picture-shaped entry in the fixture archive, root and nested, so the
/// nested DDS can be fed to `loading_screen::decode` as well. Production only
/// ever offers root entries through `loading_screen::list`.
fn pck_entries_for(element: &std::path::Path) -> Vec<loading_screen::Picture> {
    let mut file = std::fs::File::open(element.join(loading_screen::ARCHIVE_NAME)).unwrap();
    pck::directory(&mut file)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.name.contains('.'))
        .map(|entry| loading_screen::Picture {
            path: entry.name.to_ascii_lowercase().replace('\\', "/"),
            size: entry.size,
        })
        .collect()
}

fn main() {
    let mut failures: Vec<String> = Vec::new();
    let root =
        std::env::temp_dir().join(format!("pwe-loading-screen-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let element = root.join("element");
    std::fs::create_dir_all(element.join("maps/world")).unwrap();
    let files: Vec<(String, Vec<u8>)> = vec![
        ("surfaces/background.jpg".to_string(), jpeg()),
        ("surfaces/loading03.bmp".to_string(), bmp()),
        ("surfaces/broken.jpg".to_string(), b"not a jpeg".to_vec()),
        ("surfaces/maps/000.dds".to_string(), dds()),
        ("surfaces/iconset/iconlist_ivtrm.dds".to_string(), dds()),
    ];
    std::fs::write(element.join("surfaces.pck"), build_pck(&files)).unwrap();

    // 1. Lookup and selection.
    let listed = loading_screen::list(&element).expect("fixture archive lists");
    let names: Vec<&str> = listed.iter().map(|picture| picture.path.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "surfaces/background.jpg",
            "surfaces/broken.jpg",
            "surfaces/loading03.bmp"
        ]
    );
    if loading_screen::pick(&listed, 0).unwrap().path != "surfaces/background.jpg" {
        failures.push("pick(word 0) did not select the first picture".into());
    }
    if loading_screen::pick(&listed, 2).unwrap().path != "surfaces/loading03.bmp" {
        failures.push("pick(word 2) did not wrap to the third picture".into());
    }

    // 2. Decode every picture that must work, and pin the corrupt one. The DDS
    //    lives in a subfolder and so is not a loading screen, but it is decoded
    //    here through the same path a modified client's root DDS would use.
    let nested = pck_entries_for(&element)
        .into_iter()
        .find(|picture| picture.path == "surfaces/maps/000.dds")
        .expect("the nested DDS is in the archive index");
    match loading_screen::decode(&element, &nested) {
        // Both a size and a colour check: a block-order or channel swap in the
        // DDS path would still produce a 16x16 image, so the corners pin the
        // four solid colours the fixture encodes.
        Ok(image)
            if image.dimensions() == (16, 16)
                && image.get_pixel(0, 0).0 == [255, 0, 0, 255]
                && image.get_pixel(15, 0).0 == [0, 255, 0, 255]
                && image.get_pixel(0, 15).0 == [0, 0, 255, 255]
                && image.get_pixel(15, 15).0 == [255, 255, 255, 255] => {}
        Ok(image) => failures.push(format!(
            "surfaces/maps/000.dds decoded as {:?} with corner {:?}, expected a 16x16 red/green/blue/white block layout",
            image.dimensions(),
            image.get_pixel(0, 0).0
        )),
        Err(error) => failures.push(format!("surfaces/maps/000.dds did not decode: {error}")),
    }
    for (name, expected) in [
        ("surfaces/background.jpg", (64u32, 40u32)),
        ("surfaces/loading03.bmp", (48, 64)),
    ] {
        let picture = listed.iter().find(|picture| picture.path == name).unwrap();
        match loading_screen::decode(&element, picture) {
            Ok(image) if image.dimensions() == expected => {}
            Ok(image) => failures.push(format!(
                "{name} decoded as {:?}, expected {expected:?}",
                image.dimensions()
            )),
            Err(error) => failures.push(format!("{name} did not decode: {error}")),
        }
    }
    let broken = listed
        .iter()
        .find(|picture| picture.path == "surfaces/broken.jpg")
        .unwrap();
    if loading_screen::decode(&element, broken).is_ok() {
        failures.push("the corrupt JPEG decoded instead of failing".into());
    }

    // 3. Cover fit and the overlay strip at a laptop, a 4K and a portrait size.
    let picture = listed
        .iter()
        .find(|picture| picture.path == "surfaces/background.jpg")
        .unwrap();
    let source = loading_screen::decode(&element, picture).unwrap();
    let mut surfaces = Vec::new();
    for (width, height) in [(1280u32, 720u32), (3840, 2160), (1200, 1920)] {
        let fitted = loading_screen::fill(&source, width, height).expect("cover fit");
        let blended = loading_screen::overlay_strip(fitted.as_raw(), width, height, 178, 72, 0.78)
            .expect("strip");
        let bgra = loading_screen::to_bgra(&blended);
        if bgra.len() != width as usize * height as usize * 4 {
            failures.push(format!("{width}x{height}: BGRA length mismatch"));
            continue;
        }
        let bottom = (height as usize - 1) * width as usize * 4;
        if blended[bottom] > 40 && blended[bottom + 1] > 40 {
            failures.push(format!("{width}x{height}: bottom row was not darkened"));
        }
        surfaces.push((width, height, bgra));
    }

    // 4. Upload the fitted BGRA bytes to a GPU texture and read them back.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("a GPU adapter for the readback check");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("loading screen check"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .expect("a device for the readback check");
    for (width, height, bgra) in &surfaces {
        let texture = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("loading screen surface"),
                size: wgpu::Extent3d {
                    width: *width,
                    height: *height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bgra,
        );
        // 256-byte row alignment for the copy.
        let padded = (*width * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loading screen readback"),
            size: (padded * *height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(*height),
                },
            },
            wgpu::Extent3d {
                width: *width,
                height: *height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        // `get_mapped_range` returns the view itself in wgpu 0.19; the map
        // callback is what says the copy has landed on the host side.
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().expect("the mapping callback runs").unwrap();
        let view = slice.get_mapped_range();
        let row = (*width * 4) as usize;
        let mut mismatches = 0usize;
        for y in 0..*height as usize {
            let expected = &bgra[y * row..(y + 1) * row];
            let actual = &view[y * padded as usize..y * padded as usize + row];
            if expected != actual {
                mismatches += 1;
            }
        }
        if mismatches > 0 {
            failures.push(format!(
                "{width}x{height}: {mismatches} rows of the BGRA upload differ from the DIB bytes"
            ));
        }
        drop(view);
        buffer.unmap();
    }

    std::fs::remove_dir_all(&root).ok();
    if failures.is_empty() {
        println!(
            "loading screen: 2 decodes, 1 pinned corrupt payload, {} fitted sizes (1280x720, 3840x2160, 1200x1920) and their BGRA uploads passed",
            surfaces.len()
        );
        return;
    }
    for failure in &failures {
        eprintln!("::error::loading screen: {failure}");
    }
    std::process::exit(1);
}
