//! Experimental first-person software terrain viewer; not the Angelica rendering pipeline.
//! It still shares the recovered regional scenery residency, but it has no
//! secondary views, so EC-BSD reflection/refraction flags do not apply here.
use crate::scenery_visibility::Class;
use crate::Terrain;
use minifb::{Key, Window, WindowOptions};
use std::time::Instant;
const W: usize = 960;
const H: usize = 720;
const NEAR: f32 = 2.0;
const FAR: f32 = 1200.0;
#[derive(Clone, Copy, Debug)]
struct Vertex {
    x: f32,
    y: f32,
    z: f32,
    wx: f32,
    wy: f32,
    wh: f32,
    light: f32,
    tu: f32,
    tv: f32,
}
struct Camera {
    x: f32,
    y: f32,
    altitude: f32,
    yaw: f32,
    pitch: f32,
}
struct View<'a> {
    camera: &'a Camera,
    yaw: (f32, f32),
    pitch: (f32, f32),
}
impl<'a> View<'a> {
    fn new(c: &'a Camera) -> Self {
        Self {
            camera: c,
            yaw: c.yaw.sin_cos(),
            pitch: c.pitch.sin_cos(),
        }
    }
}
fn camera_vertex(
    t: &Terrain,
    view: &View,
    reader: &mut crate::terrain_store::Reader,
    x: usize,
    y: usize,
) -> Vertex {
    let c = view.camera;
    let dx = x as f32 - c.x;
    let dy = y as f32 - c.y;
    let (h, n) = reader
        .cell(y * t.width + x)
        .expect("lossless terrain backing store read failed");
    let dz = h / t.grid_spacing - c.altitude;
    let (s, co) = view.yaw;
    let right = dx * co - dy * s;
    let forward = dx * s + dy * co;
    let (sp, cp) = view.pitch;
    let lighting = (0.55
        + (n[0] as f32 * -0.32 + n[1] as f32 * 0.82 + n[2] as f32 * 0.46).max(0.) / 127. * 0.45)
        .clamp(0.35, 1.0);
    Vertex {
        x: right,
        y: dz * cp - forward * sp,
        z: forward * cp + dz * sp,
        wx: x as f32,
        wy: y as f32,
        wh: h,
        light: lighting,
        tu: 0.,
        tv: 0.,
    }
}
fn lerp(a: Vertex, b: Vertex, t: f32) -> Vertex {
    Vertex {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        z: a.z + (b.z - a.z) * t,
        wx: a.wx + (b.wx - a.wx) * t,
        wy: a.wy + (b.wy - a.wy) * t,
        wh: a.wh + (b.wh - a.wh) * t,
        light: a.light + (b.light - a.light) * t,
        tu: a.tu + (b.tu - a.tu) * t,
        tv: a.tv + (b.tv - a.tv) * t,
    }
}
fn clip_near_stack(v: [Vertex; 3]) -> ([Vertex; 4], usize) {
    if v.iter().all(|p| p.z >= NEAR) {
        return ([v[0], v[1], v[2], v[2]], 3);
    }
    let mut out = [v[0]; 4];
    let mut n = 0;
    let mut a = v[2];
    for b in v {
        let ai = a.z >= NEAR;
        let bi = b.z >= NEAR;
        if ai != bi {
            out[n] = lerp(a, b, (NEAR - a.z) / (b.z - a.z));
            n += 1;
        }
        if bi {
            out[n] = b;
            n += 1;
        }
        a = b;
    }
    (out, n)
}

fn project(v: Vertex) -> Vertex {
    let f = 600.;
    Vertex {
        x: W as f32 * 0.5 + f * v.x / v.z,
        y: H as f32 * 0.5 - f * v.y / v.z,
        z: 1. / v.z,
        wx: v.wx / v.z,
        wy: v.wy / v.z,
        wh: v.wh / v.z,
        light: v.light / v.z,
        tu: v.tu / v.z,
        tv: v.tv / v.z,
    }
}
fn edge(a: Vertex, b: Vertex, x: f32, y: f32) -> f32 {
    (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x)
}
fn triangle(
    frame: &mut [u32],
    depth: &mut [f32],
    a: Vertex,
    b: Vertex,
    c: Vertex,
    color: u32,
    materials: Option<&crate::material::Materials>,
    model_texture: Option<&crate::objects::Texture>,
) {
    let area = edge(a, b, c.x, c.y);
    if area.abs() < 0.001 {
        return;
    }
    if a.x.max(b.x).max(c.x) < 0.0
        || a.x.min(b.x).min(c.x) >= W as f32
        || a.y.max(b.y).max(c.y) < 0.0
        || a.y.min(b.y).min(c.y) >= H as f32
    {
        return;
    }
    let x0 = (a.x.min(b.x).min(c.x).floor() as i32).clamp(0, W as i32 - 1);
    let x1 = (a.x.max(b.x).max(c.x).ceil() as i32).clamp(0, W as i32 - 1);
    let y0 = (a.y.min(b.y).min(c.y).floor() as i32).clamp(0, H as i32 - 1);
    let y1 = (a.y.max(b.y).max(c.y).ceil() as i32).clamp(0, H as i32 - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let wa = edge(b, c, px, py) / area;
            let wb = edge(c, a, px, py) / area;
            let wc = 1.0 - wa - wb;
            if wa >= 0.0 && wb >= 0.0 && wc >= 0.0 {
                let z = wa * a.z + wb * b.z + wc * c.z;
                let i = y as usize * W + x as usize;
                if z > depth[i] {
                    depth[i] = z;
                    frame[i] = if let Some(mat) = materials {
                        let wx = (wa * a.wx + wb * b.wx + wc * c.wx) / z;
                        let wy = (wa * a.wy + wb * b.wy + wc * c.wy) / z;
                        let h = (wa * a.wh + wb * b.wh + wc * c.wh) / z;
                        let rgb = mat.sample(wx, wy, h);
                        let shade =
                            ((wa * a.light + wb * b.light + wc * c.light) / z).clamp(0., 1.);
                        let channels = rgb.map(|v| (v * shade).clamp(0., 255.) as u32);
                        (channels[0] << 16) | (channels[1] << 8) | channels[2]
                    } else if let Some(tex) = model_texture {
                        let u = (wa * a.tu + wb * b.tu + wc * c.tu) / z;
                        let v = (wa * a.tv + wb * b.tv + wc * c.tv) / z;
                        let rgb = tex.sample(u, v);
                        let shade =
                            ((wa * a.light + wb * b.light + wc * c.light) / z).clamp(0., 1.);
                        let ch = rgb.map(|v| (v * shade).clamp(0., 255.) as u32);
                        (ch[0] << 16) | (ch[1] << 8) | ch[2]
                    } else {
                        color
                    };
                }
            }
        }
    }
}
fn clipped_triangle(
    frame: &mut [u32],
    depth: &mut [f32],
    face: [Vertex; 3],
    color: u32,
    materials: Option<&crate::material::Materials>,
    texture: Option<&crate::objects::Texture>,
) {
    let (mut p, n) = clip_near_stack(face);
    if n < 3 {
        return;
    }
    for v in &mut p[..n] {
        *v = project(*v);
    }
    for i in 1..n - 1 {
        triangle(
            frame,
            depth,
            p[0],
            p[i],
            p[i + 1],
            color,
            materials,
            texture,
        );
    }
}
fn default_camera(t: &Terrain) -> Camera {
    let x = t.width / 2;
    let y = t.height / 2;
    Camera {
        x: x as f32,
        y: y as f32,
        altitude: t.heights.at(y * t.width + x) / t.grid_spacing + 80.,
        yaw: 0.,
        pitch: -0.15,
    }
}
fn draw(t: &Terrain, view: &View, frame: &mut [u32], depth: &mut [f32]) {
    for y in 0..H {
        let f = y as f32 / H as f32;
        let r = (85. + 45. * f) as u32;
        let g = (137. + 35. * f) as u32;
        let b = (184. + 20. * f) as u32;
        frame[y * W..(y + 1) * W].fill((r << 16) | (g << 8) | b);
    }
    depth.fill(0.);
    let cam = view.camera;
    let startx = (cam.x - FAR).max(0.) as usize;
    let starty = (cam.y - FAR).max(0.) as usize;
    let endx = ((cam.x + FAR) as usize).min(t.width - 1);
    let endy = ((cam.y + FAR) as usize).min(t.height - 1);
    if startx >= endx || starty >= endy {
        return;
    }
    // Overwrite the previous scanline in place. A grid corner is sampled and
    // transformed once even when four terrain quads share it.
    let mut reader = t.heights.reader();
    let mut row: Vec<_> = (startx..=endx)
        .map(|x| camera_vertex(t, view, &mut reader, x, starty))
        .collect();
    for y in starty..endy {
        let mut a = row[0];
        let mut c = camera_vertex(t, view, &mut reader, startx, y + 1);
        row[0] = c;
        for (i, next) in row[1..].iter_mut().enumerate() {
            let b = *next;
            let d = camera_vertex(t, view, &mut reader, startx + i + 1, y + 1);
            *next = d;
            if !(a.z < NEAR && b.z < NEAR && c.z < NEAR && d.z < NEAR
                || a.z > FAR && b.z > FAR && c.z > FAR && d.z > FAR)
            {
                clipped_triangle(frame, depth, [a, b, c], 0, Some(&t.materials), None);
                clipped_triangle(frame, depth, [b, d, c], 0, Some(&t.materials), None);
            }
            a = b;
            c = d;
        }
    }
}
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-8 {
        [0., 1., 0.]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}
fn model_basis(model: &crate::bmd::Model) -> [[f32; 3]; 3] {
    let z = normalize(model.direction);
    let y = normalize(model.up);
    let x = normalize([
        y[1] * z[2] - y[2] * z[1],
        y[2] * z[0] - y[0] * z[2],
        y[0] * z[1] - y[1] * z[0],
    ]);
    [x, y, z]
}
fn model_vertex(
    t: &Terrain,
    view: &View,
    [x, y, z]: [[f32; 3]; 3],
    model: &crate::bmd::Model,
    v: &crate::bmd::Vertex,
) -> Vertex {
    let mut w = model.position;
    let mut n = [0.; 3];
    for i in 0..3 {
        w[i] += x[i] * v.position[0] * model.scale[0]
            + y[i] * v.position[1] * model.scale[1]
            + z[i] * v.position[2] * model.scale[2];
        n[i] = x[i] * v.normal[0] + y[i] * v.normal[1] + z[i] * v.normal[2];
    }
    let gx = w[0] / t.grid_spacing + t.width as f32 * 0.5;
    let gy = t.height as f32 * 0.5 - w[2] / t.grid_spacing;
    let cam = view.camera;
    let dx = gx - cam.x;
    let dy = gy - cam.y;
    let dz = w[1] / t.grid_spacing - cam.altitude;
    let (s, co) = view.yaw;
    let right = dx * co - dy * s;
    let forward = dx * s + dy * co;
    let (sp, cp) = view.pitch;
    let n = normalize(n);
    let light = (0.55 + (-n[0] * 0.32 + n[1] * 0.82 + n[2] * 0.46).max(0.) * 0.45).clamp(0.35, 1.);
    Vertex {
        x: right,
        y: dz * cp - forward * sp,
        z: forward * cp + dz * sp,
        wx: gx,
        wy: gy,
        wh: w[1],
        light,
        tu: v.uv[0],
        tv: v.uv[1],
    }
}
fn point_in_world(t: &Terrain, view: &View, x: f32, y: f32, z: f32) -> Vertex {
    let gx = x / t.grid_spacing + t.width as f32 * 0.5;
    let gy = t.height as f32 * 0.5 - z / t.grid_spacing;
    let cam = view.camera;
    let dx = gx - cam.x;
    let dy = gy - cam.y;
    let dz = y / t.grid_spacing - cam.altitude;
    let (s, co) = view.yaw;
    let right = dx * co - dy * s;
    let forward = dx * s + dy * co;
    let (sp, cp) = view.pitch;
    Vertex {
        x: right,
        y: dz * cp - forward * sp,
        z: forward * cp + dz * sp,
        wx: gx,
        wy: gy,
        wh: y,
        light: 1.,
        tu: 0.,
        tv: 0.,
    }
}
fn draw_water(t: &Terrain, view: &View, frame: &mut [u32], depth: &mut [f32]) {
    for area in &t.water {
        let halfx = area.columns as f32 * area.grid_size * 0.5;
        let halfz = area.rows as f32 * area.grid_size * 0.5;
        let x0 = area.center[0] - halfx;
        let z0 = area.center[2] + halfz;
        for row in 0..area.rows {
            for col in 0..area.columns {
                if area.mask[row * area.columns + col] & 1 == 0 {
                    continue;
                }
                let x = x0 + col as f32 * area.grid_size;
                let z = z0 - row as f32 * area.grid_size;
                let size = area.grid_size;
                let a = point_in_world(t, view, x, area.height, z);
                let b = point_in_world(t, view, x + size, area.height, z);
                let c = point_in_world(t, view, x, area.height, z - size);
                let d = point_in_world(t, view, x + size, area.height, z - size);
                if a.z < NEAR && b.z < NEAR && c.z < NEAR && d.z < NEAR {
                    continue;
                }
                if a.z > FAR && b.z > FAR && c.z > FAR && d.z > FAR {
                    continue;
                }
                for face in [[a, b, c], [b, d, c]] {
                    clipped_triangle(frame, depth, face, area.color & 0x00ffffff, None, None);
                }
            }
        }
    }
}
fn render(
    t: &mut Terrain,
    cam: &Camera,
    frame: &mut [u32],
    depth: &mut [f32],
) -> Result<(), String> {
    let wx = (cam.x - t.width as f32 * 0.5) * t.grid_spacing;
    let wz = (t.height as f32 * 0.5 - cam.y) * t.grid_spacing;
    // Same recovered `CECSceneBlock` ten-class selection as the GPU renderer
    // (0x424020). Preset 2 is the shipped `detail_scene = 1` default.
    let placements = crate::gpu::ten_class_placements(t);
    let margin = 0.5 * t.scene.block_size.max(0.);
    let nearby = t.scenery_residency.select(&placements, [wx, wz], 2, margin);
    let ornament_count = t.objects.ornaments.len();
    let ornament_ids: Vec<usize> = nearby
        .iter()
        .copied()
        .filter(|&i| i < ornament_count)
        .collect();
    t.objects.load_ids(&ornament_ids)?;
    eprintln!(
        "Software viewer scenery residency: {} native classes, {} resident, {} ornaments retained, {} loads, {} evictions, {} distance culls",
        Class::ALL.len(),
        nearby.len(),
        ornament_ids.len(),
        t.scenery_residency.loads,
        t.scenery_residency.evictions,
        t.scenery_residency.distance_culls
    );
    let view = View::new(cam);
    draw(t, &view, frame, depth);
    let mut vertices = Vec::new();
    for id in ornament_ids {
        let Some(model) = t.objects.get(id) else {
            return Err(format!("missing resident scenery model {id}"));
        };
        let basis = model_basis(model);
        for mesh in &model.meshes {
            let Some(texture) = t.objects.texture(&mesh.texture) else {
                return Err(format!(
                    "missing scenery texture {} for ornament {}",
                    mesh.texture, id
                ));
            };
            vertices.clear();
            vertices.extend(
                mesh.vertices
                    .iter()
                    .map(|v| model_vertex(t, &view, basis, model, v)),
            );
            for face in mesh.indices.chunks_exact(3) {
                let a = vertices[face[0] as usize];
                let b = vertices[face[1] as usize];
                let c = vertices[face[2] as usize];
                if a.z < NEAR && b.z < NEAR && c.z < NEAR {
                    continue;
                }
                clipped_triangle(frame, depth, [a, b, c], 0, None, Some(texture));
            }
        }
    }
    draw_water(t, &view, frame, depth);
    Ok(())
}
pub fn preview(t: &mut Terrain, output: &std::path::Path) -> Result<(), String> {
    let cam = default_camera(t);
    preview_camera(t, output, cam)
}
pub fn preview_at(
    t: &mut Terrain,
    output: &std::path::Path,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    pitch: f32,
) -> Result<(), String> {
    if ![x, y, z, yaw, pitch].iter().all(|x| x.is_finite()) {
        return Err("invalid camera coordinates".into());
    }
    let cam = Camera {
        x: x / t.grid_spacing + t.width as f32 * 0.5,
        y: t.height as f32 * 0.5 - z / t.grid_spacing,
        altitude: y / t.grid_spacing,
        yaw,
        pitch,
    };
    preview_camera(t, output, cam)
}
fn preview_camera(t: &mut Terrain, output: &std::path::Path, cam: Camera) -> Result<(), String> {
    let mut frame = vec![0u32; W * H];
    let mut depth = vec![0.; W * H];
    render(t, &cam, &mut frame, &mut depth)?;
    let mut rgb = Vec::with_capacity(W * H * 3);
    for pixel in frame {
        rgb.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
    }
    let f = std::fs::File::create(output).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(f), W as u32, H as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(())
}
pub fn run(t: &mut Terrain) -> Result<(), String> {
    let mut win=Window::new("PW terrain flight prototype | WASD fly, E/Q ascend/descend, arrows look, Shift speed, Esc exit",W,H,WindowOptions::default()).map_err(|e|e.to_string())?;
    win.set_target_fps(30);
    let mut frame = vec![0u32; W * H];
    let mut depth = vec![0.; W * H];
    let mut cam = default_camera(t);
    let mut last = Instant::now();
    while win.is_open() && !win.is_key_down(Key::Escape) {
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        let speed = if win.is_key_down(Key::LeftShift) {
            500.
        } else {
            120.
        };
        let movement = speed * dt;
        let (s, co) = cam.yaw.sin_cos();
        let mut right = 0.;
        let mut forward = 0.;
        if win.is_key_down(Key::W) {
            forward += movement
        }
        if win.is_key_down(Key::S) {
            forward -= movement
        }
        if win.is_key_down(Key::A) {
            right -= movement
        }
        if win.is_key_down(Key::D) {
            right += movement
        }
        cam.x = (cam.x + right * co + forward * s).clamp(0., (t.width - 1) as f32);
        cam.y = (cam.y - right * s + forward * co).clamp(0., (t.height - 1) as f32);
        if win.is_key_down(Key::E) {
            cam.altitude += movement
        }
        if win.is_key_down(Key::Q) {
            cam.altitude -= movement
        }
        if win.is_key_down(Key::Left) {
            cam.yaw -= dt
        }
        if win.is_key_down(Key::Right) {
            cam.yaw += dt
        }
        if win.is_key_down(Key::Up) {
            cam.pitch = (cam.pitch + dt).min(1.5)
        }
        if win.is_key_down(Key::Down) {
            cam.pitch = (cam.pitch - dt).max(-1.5)
        }
        render(t, &cam, &mut frame, &mut depth)?;
        win.update_with_buffer(&frame, W, H)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_triangle() {
        let mut f = vec![0; W * H];
        let mut z = vec![0.; W * H];
        triangle(
            &mut f,
            &mut z,
            Vertex {
                x: 10.,
                y: 10.,
                z: 1.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 30.,
                y: 10.,
                z: 1.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 10.,
                y: 30.,
                z: 1.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            0xabcdef,
            None,
            None,
        );
        assert_eq!(f[15 * W + 15], 0xabcdef);
        assert_eq!(f[40 * W + 40], 0);
    }
    #[test]
    fn clips_near_plane() {
        let (p, n) = clip_near_stack([
            Vertex {
                x: 0.,
                y: 0.,
                z: 1.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 1.,
                y: 0.,
                z: 4.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 0.,
                y: 1.,
                z: 4.,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
        ]);
        assert_eq!(n, 4);
        assert!(p.iter().all(|x| x.z >= NEAR - 0.0001));
    }
    #[test]
    fn nearest_wins() {
        let mut f = vec![0; W * H];
        let mut z = vec![0.; W * H];
        let v = [
            Vertex {
                x: 10.,
                y: 10.,
                z: 0.1,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 30.,
                y: 10.,
                z: 0.1,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
            Vertex {
                x: 10.,
                y: 30.,
                z: 0.1,
                wx: 0.,
                wy: 0.,
                wh: 0.,
                light: 0.,
                tu: 0.,
                tv: 0.,
            },
        ];
        triangle(&mut f, &mut z, v[0], v[1], v[2], 1, None, None);
        let a = v.map(|v| Vertex { z: 0.2, ..v });
        triangle(&mut f, &mut z, a[0], a[1], a[2], 2, None, None);
        triangle(&mut f, &mut z, v[0], v[1], v[2], 1, None, None);
        assert_eq!(f[15 * W + 15], 2);
    }
}
