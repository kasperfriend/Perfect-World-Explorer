//! Dedicated Blender-style 3D wgpu viewport renderer for Architect.
//!
//! Renders the editable terrain heightmap, real `.bmd` or stylized placed
//! objects, reference grid, selection bounding boxes, terrain brush ring,
//! placement ghost, and 3D transform gizmos into an offscreen `Rgba8Unorm`
//! texture registered with `egui_wgpu::Renderer`.

use super::world::{EditableTerrain, ObjectKind, PaintLayer, PlacedObject};
use crate::{bmd, pck};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::PathBuf,
};

const VIEWPORT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const SHADER_WGSL: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    sun_dir: vec4<f32>,
    flags: vec4<f32>, // x = shading_mode (0=solid, 1=wire/flat, 2=heightmap)
};

@group(0) @binding(0)
var<uniform> u: Uniforms;

struct MeshIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_mesh(v: MeshIn) -> MeshOut {
    var out: MeshOut;
    out.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    out.world_pos = v.pos;
    out.normal = v.normal;
    out.color = v.color;
    return out;
}

@fragment
fn fs_mesh(i: MeshOut) -> @location(0) vec4<f32> {
    let n = normalize(i.normal);
    let sun = normalize(u.sun_dir.xyz);
    let view_dir = normalize(u.camera_pos.xyz - i.world_pos);
    let ndl = max(dot(n, sun), 0.0);
    let fill = max(dot(n, normalize(vec3<f32>(-0.5, 0.35, -0.6))), 0.0) * 0.28;
    let hemi = mix(0.24, 0.42, clamp(n.y * 0.5 + 0.5, 0.0, 1.0));
    let rim = pow(1.0 - max(dot(n, view_dir), 0.0), 3.0) * 0.14;

    var base = i.color.rgb;
    if (u.flags.x > 1.5) {
        // Heightmap visualization mode
        let h = clamp((i.world_pos.y + 20.0) / 100.0, 0.0, 1.0);
        base = mix(vec3<f32>(0.16, 0.34, 0.62), vec3<f32>(0.92, 0.88, 0.72), h);
    }

    let lit = base * (hemi + ndl * 0.78 + fill) + vec3<f32>(rim);
    // Subtle studio distance fog so the horizon blends gently into the Blender slate sky.
    let dist = length(u.camera_pos.xyz - i.world_pos);
    let fog = clamp((dist - 220.0) / 680.0, 0.0, 0.65);
    let sky = vec3<f32>(0.15, 0.16, 0.19);
    let final_col = mix(lit, sky, fog);
    return vec4<f32>(final_col, i.color.a);
}

struct LineIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(v: LineIn) -> LineOut {
    var out: LineOut;
    out.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    out.color = v.color;
    return out;
}

@fragment
fn fs_line(i: LineOut) -> @location(0) vec4<f32> {
    return i.color;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    sun_dir: [f32; 4],
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshVertex {
    pos: [f32; 3],
    normal: [f32; 3],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 4],
}

/// Viewport shading mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadingMode {
    Solid,
    Wireframe,
    Heightmap,
}

impl ShadingMode {
    pub const ALL: [Self; 3] = [Self::Solid, Self::Wireframe, Self::Heightmap];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Wireframe => "Wireframe",
            Self::Heightmap => "Heightmap",
        }
    }
}

/// Active gizmo mode drawn at the selection pivot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoMode {
    None,
    Move,
    Rotate,
    Scale,
}

/// Axis handle on the 3D transform gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

/// Blender-style orbit/pan/zoom/fly camera.
#[derive(Clone, Debug)]
pub struct EditorCamera {
    pub pivot: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    pub fly_speed: f32,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            pivot: Vec3::ZERO,
            yaw: 0.72,
            pitch: 0.56,
            distance: 72.0,
            fov_y: 50.0_f32.to_radians(),
            fly_speed: 45.0,
        }
    }
}

impl EditorCamera {
    pub fn eye(&self) -> Vec3 {
        let cp = self.pitch.cos();
        let sp = self.pitch.sin();
        let cy = self.yaw.cos();
        let sy = self.yaw.sin();
        let offset = Vec3::new(sy * cp, sp, cy * cp) * self.distance;
        self.pivot + offset
    }

    pub fn forward(&self) -> Vec3 {
        (self.pivot - self.eye()).normalize_or_zero()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize_or_zero()
    }

    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward()).normalize_or_zero()
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.pivot, Vec3::Y)
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, aspect.max(0.1), 0.25, 4000.0)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    pub fn orbit(&mut self, delta: egui::Vec2) {
        self.yaw -= delta.x * 0.0075;
        self.pitch = (self.pitch + delta.y * 0.0075).clamp(-1.48, 1.48);
    }

    pub fn pan(&mut self, delta: egui::Vec2) {
        let scale = (self.distance * 0.0016).max(0.02);
        let right = self.right();
        let up = self.up();
        self.pivot += (-right * delta.x + up * delta.y) * scale;
    }

    pub fn zoom(&mut self, scroll_delta: f32) {
        let factor = (1.0 - scroll_delta * 0.0015).clamp(0.75, 1.35);
        self.distance = (self.distance * factor).clamp(2.5, 1200.0);
    }

    pub fn focus(&mut self, target: Vec3, radius: f32) {
        self.pivot = target;
        self.distance = (radius * 3.2).clamp(8.0, 400.0);
    }

    pub fn set_top_view(&mut self) {
        self.yaw = 0.0;
        self.pitch = 1.47;
    }

    pub fn set_front_view(&mut self) {
        self.yaw = 0.0;
        self.pitch = 0.05;
    }

    pub fn set_right_view(&mut self) {
        self.yaw = std::f32::consts::FRAC_PI_2;
        self.pitch = 0.05;
    }

    /// Convert a pointer position inside `viewport_rect` into a world-space ray `(origin, dir)`.
    pub fn screen_ray(&self, pos: egui::Pos2, rect: egui::Rect) -> (Vec3, Vec3) {
        let w = rect.width().max(1.0);
        let h = rect.height().max(1.0);
        let aspect = w / h;
        let ndc_x = ((pos.x - rect.min.x) / w) * 2.0 - 1.0;
        let ndc_y = 1.0 - ((pos.y - rect.min.y) / h) * 2.0;
        let inv_vp = self.view_projection(aspect).inverse();
        let near = inv_vp * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far = inv_vp * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near_p = near.truncate() / near.w.max(1e-6);
        let far_p = far.truncate() / far.w.max(1e-6);
        (near_p, (far_p - near_p).normalize_or_zero())
    }

    /// Project a world position to egui logical screen coordinates inside `rect`.
    pub fn project(&self, world: Vec3, rect: egui::Rect) -> Option<egui::Pos2> {
        let w = rect.width().max(1.0);
        let h = rect.height().max(1.0);
        let vp = self.view_projection(w / h);
        let clip = vp * Vec4::new(world.x, world.y, world.z, 1.0);
        if clip.w <= 0.001 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if ndc.z < 0.0 || ndc.z > 1.0 {
            return None;
        }
        let sx = rect.min.x + (ndc.x * 0.5 + 0.5) * w;
        let sy = rect.min.y + (0.5 - ndc.y * 0.5) * h;
        Some(egui::pos2(sx, sy))
    }
}

/// Cached CPU geometry for a loaded client `.bmd` asset.
struct CachedBmdMesh {
    vertices: Vec<(Vec3, Vec3)>,
    indices: Vec<u32>,
    bounds_half: [f32; 3],
}

/// On-demand BMD archive reader & mesh cache.
struct BmdCache {
    element: PathBuf,
    archives: Vec<(File, HashMap<String, pck::Entry>)>,
    archives_opened: bool,
    meshes: HashMap<String, Option<CachedBmdMesh>>,
}

impl BmdCache {
    fn new(element: PathBuf) -> Self {
        Self {
            element,
            archives: Vec::new(),
            archives_opened: false,
            meshes: HashMap::new(),
        }
    }

    fn ensure_open(&mut self) {
        if self.archives_opened {
            return;
        }
        self.archives_opened = true;
        if self.element.as_os_str().is_empty() {
            return;
        }
        for name in ["litmodels.pck", "building.pck", "models.pck"] {
            let path = self.element.join(name);
            if let Ok(mut file) = File::open(&path) {
                if let Ok(entries) = pck::directory(&mut file) {
                    let mut map = HashMap::with_capacity(entries.len());
                    for entry in entries {
                        let key = entry.name.replace('\\', "/").to_ascii_lowercase();
                        map.insert(key, entry);
                    }
                    self.archives.push((file, map));
                }
            }
        }
    }

    fn get(&mut self, asset_path: &str) -> Option<&CachedBmdMesh> {
        let key = asset_path.replace('\\', "/").to_ascii_lowercase();
        if !self.meshes.contains_key(&key) {
            self.ensure_open();
            let loaded = self.load_bmd(&key);
            self.meshes.insert(key.clone(), loaded);
        }
        self.meshes.get(&key).and_then(|o| o.as_ref())
    }

    fn load_bmd(&mut self, key: &str) -> Option<CachedBmdMesh> {
        let mut raw_bytes: Option<Vec<u8>> = None;
        for (file, map) in &mut self.archives {
            if let Some(entry) = map.get(key) {
                if let Ok(bytes) = pck::read_entry(file, entry) {
                    raw_bytes = Some(bytes);
                    break;
                }
            }
        }
        if raw_bytes.is_none() && !self.element.as_os_str().is_empty() {
            let loose = self.element.join(key);
            raw_bytes = std::fs::read(loose).ok();
        }
        let bytes = raw_bytes?;
        let model = bmd::Model::parse(&bytes).ok()?;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut min_p = Vec3::splat(f32::INFINITY);
        let mut max_p = Vec3::splat(f32::NEG_INFINITY);

        for mesh in &model.meshes {
            let base = vertices.len() as u32;
            // Cap ultra-dense meshes in the interactive editor to keep CPU upload snappy.
            if base > 12_000 {
                break;
            }
            for v in &mesh.vertices {
                let p = Vec3::from(v.position);
                let n = Vec3::from(v.normal).normalize_or_zero();
                min_p = min_p.min(p);
                max_p = max_p.max(p);
                vertices.push((p, if n.length_squared() < 0.1 { Vec3::Y } else { n }));
            }
            for &idx in &mesh.indices {
                indices.push(base + idx as u32);
            }
        }
        if vertices.is_empty() || indices.is_empty() {
            return None;
        }
        let half = ((max_p - min_p) * 0.5).max(Vec3::splat(0.5));
        Some(CachedBmdMesh {
            vertices,
            indices,
            bounds_half: half.to_array(),
        })
    }
}

/// Parameters passed to the viewport renderer each frame.
pub struct ViewportFrame<'a> {
    pub camera: &'a EditorCamera,
    pub terrain: &'a EditableTerrain,
    pub objects: &'a [PlacedObject],
    pub selected: &'a HashSet<u64>,
    pub primary_selected: Option<u64>,
    pub shading: ShadingMode,
    pub show_grid: bool,
    pub gizmo_mode: GizmoMode,
    pub gizmo_hover: Option<GizmoAxis>,
    pub brush_preview: Option<(Vec3, f32)>,
    pub placement_preview: Option<(Vec3, ObjectKind)>,
    pub sun_angle: f32,
}

struct GpuResources {
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    mesh_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    overlay_line_pipeline: wgpu::RenderPipeline,
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    egui_tex_id: egui::TextureId,
    width: u32,
    height: u32,
    terrain_vb: Option<wgpu::Buffer>,
    terrain_ib: Option<wgpu::Buffer>,
    terrain_index_count: u32,
    terrain_revision: u64,
    terrain_wire_vb: Option<wgpu::Buffer>,
    terrain_wire_count: u32,
}

/// Offscreen wgpu renderer for Architect's Blender-style 3D viewport.
pub struct ArchitectViewport {
    gpu: Option<GpuResources>,
    desired_size: [u32; 2],
    bmd_cache: BmdCache,
}

impl ArchitectViewport {
    pub fn new(element: PathBuf) -> Self {
        Self {
            gpu: None,
            desired_size: [960, 640],
            bmd_cache: BmdCache::new(element),
        }
    }

    pub fn set_desired_size(&mut self, width: u32, height: u32) {
        self.desired_size = [width.clamp(64, 4096), height.clamp(64, 4096)];
    }

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.gpu.as_ref().map(|g| g.egui_tex_id)
    }

    /// Ensure GPU resources exist, resize if needed, and render one frame into
    /// the offscreen viewport texture.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        egui_renderer: &mut egui_wgpu::Renderer,
        frame: &ViewportFrame<'_>,
    ) {
        let [w, h] = self.desired_size;
        if self.gpu.is_none() {
            self.gpu = Some(create_gpu_resources(device, egui_renderer, w, h));
        } else if let Some(gpu) = self.gpu.as_mut() {
            if gpu.width != w || gpu.height != h {
                resize_targets(device, egui_renderer, gpu, w, h);
            }
        }

        let gpu = self.gpu.as_mut().unwrap();

        // Rebuild terrain vertex/index buffers only when terrain revision changes.
        if gpu.terrain_revision != frame.terrain.revision || gpu.terrain_vb.is_none() {
            let (verts, indices, wire_verts) = build_terrain_mesh(frame.terrain);
            use wgpu::util::DeviceExt;
            gpu.terrain_vb = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("architect terrain vb"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            );
            gpu.terrain_ib = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("architect terrain ib"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            );
            gpu.terrain_index_count = indices.len() as u32;
            gpu.terrain_wire_vb = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("architect terrain wire vb"),
                    contents: bytemuck::cast_slice(&wire_verts),
                    usage: wgpu::BufferUsages::VERTEX,
                },
            ));
            gpu.terrain_wire_count = wire_verts.len() as u32;
            gpu.terrain_revision = frame.terrain.revision;
        }

        // Build dynamic object meshes.
        let mut obj_verts: Vec<MeshVertex> = Vec::new();
        let mut obj_indices: Vec<u32> = Vec::new();

        // Optional water plane.
        if let Some(wl) = frame.terrain.water_level {
            let hw = frame.terrain.half_width();
            let hl = frame.terrain.half_length();
            let base = obj_verts.len() as u32;
            let c = [0.16, 0.44, 0.72, 0.65];
            obj_verts.extend_from_slice(&[
                MeshVertex {
                    pos: [-hw, wl, -hl],
                    normal: [0., 1., 0.],
                    color: c,
                },
                MeshVertex {
                    pos: [hw, wl, -hl],
                    normal: [0., 1., 0.],
                    color: c,
                },
                MeshVertex {
                    pos: [hw, wl, hl],
                    normal: [0., 1., 0.],
                    color: c,
                },
                MeshVertex {
                    pos: [-hw, wl, hl],
                    normal: [0., 1., 0.],
                    color: c,
                },
            ]);
            obj_indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }

        for obj in frame.objects {
            if !obj.visible {
                continue;
            }
            let is_primary = frame.primary_selected == Some(obj.id);
            let is_selected = frame.selected.contains(&obj.id);
            let tint = if is_primary {
                [1.0, 0.62, 0.16, 1.0]
            } else if is_selected {
                [0.98, 0.78, 0.25, 1.0]
            } else {
                [obj.color_tint[0], obj.color_tint[1], obj.color_tint[2], 1.0]
            };

            let mat = obj.model_matrix();
            if obj.kind == ObjectKind::BuildingBmd
                && obj.asset_path.to_ascii_lowercase().ends_with(".bmd")
            {
                if let Some(mesh) = self.bmd_cache.get(&obj.asset_path) {
                    let base = obj_verts.len() as u32;
                    for &(p, n) in &mesh.vertices {
                        let wp = mat.transform_point3(p);
                        let wn = mat.transform_vector3(n).normalize_or_zero();
                        obj_verts.push(MeshVertex {
                            pos: wp.to_array(),
                            normal: wn.to_array(),
                            color: tint,
                        });
                    }
                    for &idx in &mesh.indices {
                        obj_indices.push(base + idx);
                    }
                    continue;
                }
            }
            append_stylized_object(&mut obj_verts, &mut obj_indices, obj, mat, tint);
        }

        // Build scene lines (grid, selection boxes, brush ring, placement ghost).
        let mut scene_lines: Vec<LineVertex> = Vec::new();
        if frame.show_grid {
            append_grid_lines(&mut scene_lines, frame.terrain);
        }
        for obj in frame.objects {
            if obj.visible && frame.selected.contains(&obj.id) {
                let is_primary = frame.primary_selected == Some(obj.id);
                let col = if is_primary {
                    [1.0, 0.60, 0.12, 1.0]
                } else {
                    [0.98, 0.82, 0.24, 1.0]
                };
                let (min, max) = obj.world_aabb();
                append_aabb_lines(&mut scene_lines, min, max, col);
            }
        }
        if let Some((center, radius)) = frame.brush_preview {
            append_brush_ring(&mut scene_lines, frame.terrain, center, radius);
        }
        if let Some((pos, kind)) = frame.placement_preview {
            let half = Vec3::from(kind.default_bounds_half());
            let min = Vec3::new(pos.x - half.x, pos.y, pos.z - half.z);
            let max = Vec3::new(pos.x + half.x, pos.y + half.y * 2.0, pos.z + half.z);
            append_aabb_lines(&mut scene_lines, min, max, [0.35, 0.92, 0.52, 1.0]);
        }

        // Build always-on-top overlay lines (3D Transform Gizmo).
        let mut overlay_lines: Vec<LineVertex> = Vec::new();
        if frame.gizmo_mode != GizmoMode::None && !frame.selected.is_empty() {
            if let Some(pivot) = selection_pivot(frame.objects, frame.selected) {
                let dist = (frame.camera.eye() - pivot).length().max(4.0);
                let gizmo_len = (dist * 0.14).clamp(1.8, 35.0);
                append_gizmo_lines(
                    &mut overlay_lines,
                    pivot,
                    gizmo_len,
                    frame.gizmo_mode,
                    frame.gizmo_hover,
                );
            }
        }

        // Upload uniforms.
        let aspect = w as f32 / h.max(1) as f32;
        let vp = frame.camera.view_projection(aspect);
        let eye = frame.camera.eye();
        let sun = Vec3::new(
            frame.sun_angle.cos() * 0.6,
            0.78,
            frame.sun_angle.sin() * 0.6,
        )
        .normalize_or_zero();
        let uniforms = Uniforms {
            view_proj: vp.to_cols_array_2d(),
            camera_pos: [eye.x, eye.y, eye.z, 1.0],
            sun_dir: [sun.x, sun.y, sun.z, 0.0],
            flags: [
                match frame.shading {
                    ShadingMode::Solid => 0.0,
                    ShadingMode::Wireframe => 1.0,
                    ShadingMode::Heightmap => 2.0,
                },
                0.0,
                0.0,
                0.0,
            ],
        };
        queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        use wgpu::util::DeviceExt;
        let obj_vb = (!obj_verts.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("architect obj vb"),
                contents: bytemuck::cast_slice(&obj_verts),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let obj_ib = (!obj_indices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("architect obj ib"),
                contents: bytemuck::cast_slice(&obj_indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });
        let line_vb = (!scene_lines.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("architect scene lines vb"),
                contents: bytemuck::cast_slice(&scene_lines),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let overlay_vb = (!overlay_lines.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("architect gizmo lines vb"),
                contents: bytemuck::cast_slice(&overlay_lines),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("architect viewport encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("architect viewport pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Blender 3D viewport slate background
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.145,
                            g: 0.155,
                            b: 0.180,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &gpu.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // 1. Terrain (solid or wireframe)
            if frame.shading != ShadingMode::Wireframe {
                if let (Some(vb), Some(ib)) = (&gpu.terrain_vb, &gpu.terrain_ib) {
                    pass.set_pipeline(&gpu.mesh_pipeline);
                    pass.set_bind_group(0, &gpu.uniform_bind_group, &[]);
                    pass.set_vertex_buffer(0, vb.slice(..));
                    pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..gpu.terrain_index_count, 0, 0..1);
                }
            } else if let Some(wvb) = &gpu.terrain_wire_vb {
                pass.set_pipeline(&gpu.line_pipeline);
                pass.set_bind_group(0, &gpu.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, wvb.slice(..));
                pass.draw(0..gpu.terrain_wire_count, 0..1);
            }

            // 2. Placed objects
            if let (Some(vb), Some(ib)) = (&obj_vb, &obj_ib) {
                pass.set_pipeline(&gpu.mesh_pipeline);
                pass.set_bind_group(0, &gpu.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..obj_indices.len() as u32, 0, 0..1);
            }

            // 3. Depth-tested scene lines (grid, AABBs, brush ring)
            if let Some(vb) = &line_vb {
                pass.set_pipeline(&gpu.line_pipeline);
                pass.set_bind_group(0, &gpu.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.draw(0..scene_lines.len() as u32, 0..1);
            }

            // 4. Always-on-top transform gizmos
            if let Some(vb) = &overlay_vb {
                pass.set_pipeline(&gpu.overlay_line_pipeline);
                pass.set_bind_group(0, &gpu.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.draw(0..overlay_lines.len() as u32, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

/// Compute average pivot of currently selected objects.
pub fn selection_pivot(objects: &[PlacedObject], selected: &HashSet<u64>) -> Option<Vec3> {
    let mut sum = Vec3::ZERO;
    let mut count = 0u32;
    for obj in objects {
        if selected.contains(&obj.id) {
            sum += Vec3::from(obj.position);
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f32)
}

/// Hit-test the 3D transform gizmo axes in screen space.
pub fn pick_gizmo_axis(
    camera: &EditorCamera,
    rect: egui::Rect,
    pointer: egui::Pos2,
    pivot: Vec3,
) -> Option<GizmoAxis> {
    let dist = (camera.eye() - pivot).length().max(4.0);
    let len = (dist * 0.14).clamp(1.8, 35.0);
    let origin_2d = camera.project(pivot, rect)?;
    let axes = [
        (GizmoAxis::X, pivot + Vec3::X * len),
        (GizmoAxis::Y, pivot + Vec3::Y * len),
        (GizmoAxis::Z, pivot + Vec3::Z * len),
    ];
    let mut best: Option<(GizmoAxis, f32)> = None;
    for (axis, tip) in axes {
        if let Some(tip_2d) = camera.project(tip, rect) {
            let d = point_segment_dist(pointer, origin_2d, tip_2d);
            if d < 14.0 && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((axis, d));
            }
        }
    }
    best.map(|(a, _)| a)
}

fn point_segment_dist(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-4 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn create_color_and_depth(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("architect viewport color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: VIEWPORT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let color_view = color.create_view(&Default::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("architect viewport depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&Default::default());
    (color, color_view, depth_view)
}

fn resize_targets(
    device: &wgpu::Device,
    egui_renderer: &mut egui_wgpu::Renderer,
    gpu: &mut GpuResources,
    width: u32,
    height: u32,
) {
    let (color, color_view, depth_view) = create_color_and_depth(device, width, height);
    egui_renderer.update_egui_texture_from_wgpu_texture(
        device,
        &color_view,
        wgpu::FilterMode::Linear,
        gpu.egui_tex_id,
    );
    gpu.color_texture = color;
    gpu.color_view = color_view;
    gpu.depth_view = depth_view;
    gpu.width = width;
    gpu.height = height;
}

fn create_gpu_resources(
    device: &wgpu::Device,
    egui_renderer: &mut egui_wgpu::Renderer,
    width: u32,
    height: u32,
) -> GpuResources {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("architect viewport shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
    });

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("architect uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("architect bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("architect bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("architect pipeline layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("architect mesh pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_mesh",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_mesh",
            targets: &[Some(wgpu::ColorTargetState {
                format: VIEWPORT_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
    });

    let line_layout = [wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<LineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
    }];

    let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("architect line pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_line",
            buffers: &line_layout,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_line",
            targets: &[Some(wgpu::ColorTargetState {
                format: VIEWPORT_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
    });

    let overlay_line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("architect overlay line pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_line",
            buffers: &line_layout,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_line",
            targets: &[Some(wgpu::ColorTargetState {
                format: VIEWPORT_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
    });

    let (color_texture, color_view, depth_view) = create_color_and_depth(device, width, height);
    let egui_tex_id =
        egui_renderer.register_native_texture(device, &color_view, wgpu::FilterMode::Linear);

    GpuResources {
        uniform_buffer,
        uniform_bind_group,
        mesh_pipeline,
        line_pipeline,
        overlay_line_pipeline,
        color_texture,
        color_view,
        depth_view,
        egui_tex_id,
        width,
        height,
        terrain_vb: None,
        terrain_ib: None,
        terrain_index_count: 0,
        terrain_revision: 0,
        terrain_wire_vb: None,
        terrain_wire_count: 0,
    }
}

fn build_terrain_mesh(terrain: &EditableTerrain) -> (Vec<MeshVertex>, Vec<u32>, Vec<LineVertex>) {
    let vx = terrain.verts_x();
    let vz = terrain.verts_z();
    let mut verts = Vec::with_capacity(vx * vz);

    for iz in 0..vz {
        for ix in 0..vx {
            let (wx, wz) = terrain.vertex_world_xz(ix, iz);
            let h = terrain.height_at_vertex(ix, iz);
            let n = terrain.vertex_normal(ix, iz);
            let layer = PaintLayer::from_u8(terrain.paint_layers[iz * vx + ix]);
            let base = Vec3::from(layer.color());
            // Subtle cliff/rock blend on steep slopes so sculpting reads crisply.
            let slope = (1.0 - n.y.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            let rock = Vec3::new(0.42, 0.43, 0.46);
            let col = base.lerp(rock, (slope * 1.4).clamp(0.0, 0.75));
            verts.push(MeshVertex {
                pos: [wx, h, wz],
                normal: n.to_array(),
                color: [col.x, col.y, col.z, 1.0],
            });
        }
    }

    let mut indices = Vec::with_capacity(terrain.cells_x * terrain.cells_z * 6);
    for iz in 0..terrain.cells_z {
        for ix in 0..terrain.cells_x {
            let i00 = (iz * vx + ix) as u32;
            let i10 = i00 + 1;
            let i01 = ((iz + 1) * vx + ix) as u32;
            let i11 = i01 + 1;
            indices.extend_from_slice(&[i00, i01, i10, i10, i01, i11]);
        }
    }

    // Subsampled wireframe lines for Wireframe mode.
    let step = (terrain.cells_x / 64).max(1);
    let mut wire = Vec::new();
    let wire_col = [0.46, 0.54, 0.62, 0.85];
    for iz in (0..vz).step_by(step) {
        for ix in (0..terrain.cells_x).step_by(step) {
            let nx = (ix + step).min(terrain.cells_x);
            wire.push(LineVertex {
                pos: verts[iz * vx + ix].pos,
                color: wire_col,
            });
            wire.push(LineVertex {
                pos: verts[iz * vx + nx].pos,
                color: wire_col,
            });
        }
    }
    for ix in (0..vx).step_by(step) {
        for iz in (0..terrain.cells_z).step_by(step) {
            let nz = (iz + step).min(terrain.cells_z);
            wire.push(LineVertex {
                pos: verts[iz * vx + ix].pos,
                color: wire_col,
            });
            wire.push(LineVertex {
                pos: verts[nz * vx + ix].pos,
                color: wire_col,
            });
        }
    }

    (verts, indices, wire)
}

fn append_box(
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    mat: Mat4,
    min: Vec3,
    max: Vec3,
    color: [f32; 4],
) {
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    let faces: [([usize; 4], Vec3); 6] = [
        ([0, 3, 2, 1], Vec3::NEG_Z),
        ([4, 5, 6, 7], Vec3::Z),
        ([0, 1, 5, 4], Vec3::NEG_Y),
        ([3, 7, 6, 2], Vec3::Y),
        ([0, 4, 7, 3], Vec3::NEG_X),
        ([1, 2, 6, 5], Vec3::X),
    ];
    for (quad, n) in faces {
        let base = verts.len() as u32;
        let wn = mat.transform_vector3(n).normalize_or_zero().to_array();
        for &ci in &quad {
            let wp = mat.transform_point3(corners[ci]).to_array();
            verts.push(MeshVertex {
                pos: wp,
                normal: wn,
                color,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

fn append_pyramid(
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    mat: Mat4,
    base_y: f32,
    top_y: f32,
    half_xz: f32,
    color: [f32; 4],
) {
    let p0 = Vec3::new(-half_xz, base_y, -half_xz);
    let p1 = Vec3::new(half_xz, base_y, -half_xz);
    let p2 = Vec3::new(half_xz, base_y, half_xz);
    let p3 = Vec3::new(-half_xz, base_y, half_xz);
    let apex = Vec3::new(0.0, top_y, 0.0);
    let tris = [
        (p0, p1, apex),
        (p1, p2, apex),
        (p2, p3, apex),
        (p3, p0, apex),
    ];
    for (a, b, c) in tris {
        let base = verts.len() as u32;
        let wa = mat.transform_point3(a);
        let wb = mat.transform_point3(b);
        let wc = mat.transform_point3(c);
        let wn = (wb - wa).cross(wc - wa).normalize_or_zero().to_array();
        verts.push(MeshVertex {
            pos: wa.to_array(),
            normal: wn,
            color,
        });
        verts.push(MeshVertex {
            pos: wb.to_array(),
            normal: wn,
            color,
        });
        verts.push(MeshVertex {
            pos: wc.to_array(),
            normal: wn,
            color,
        });
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
}

fn append_stylized_object(
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
    obj: &PlacedObject,
    mat: Mat4,
    tint: [f32; 4],
) {
    let lower = obj.asset_path.to_ascii_lowercase();
    if lower == "builtin://pillar" {
        append_box(
            verts,
            indices,
            mat,
            Vec3::new(-0.7, 0.0, -0.7),
            Vec3::new(0.7, 6.0, 0.7),
            tint,
        );
        return;
    }
    if lower == "builtin://wall" {
        append_box(
            verts,
            indices,
            mat,
            Vec3::new(-4.0, 0.0, -0.5),
            Vec3::new(4.0, 3.5, 0.5),
            tint,
        );
        return;
    }
    if lower == "builtin://platform" {
        append_box(
            verts,
            indices,
            mat,
            Vec3::new(-4.0, 0.0, -4.0),
            Vec3::new(4.0, 0.6, 4.0),
            tint,
        );
        return;
    }
    if lower == "builtin://ramp" {
        append_box(
            verts,
            indices,
            mat,
            Vec3::new(-2.0, 0.0, -3.5),
            Vec3::new(2.0, 0.5, 3.5),
            tint,
        );
        append_box(
            verts,
            indices,
            mat,
            Vec3::new(-2.0, 0.5, 0.0),
            Vec3::new(2.0, 1.4, 3.5),
            tint,
        );
        return;
    }

    match obj.kind {
        ObjectKind::Tree => {
            let trunk = [0.42, 0.28, 0.16, 1.0];
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-0.35, 0.0, -0.35),
                Vec3::new(0.35, 2.5, 0.35),
                trunk,
            );
            append_pyramid(verts, indices, mat, 2.0, 6.0, 2.1, tint);
            append_pyramid(verts, indices, mat, 4.2, 8.0, 1.5, tint);
        }
        ObjectKind::BuildingBmd => {
            let h = Vec3::from(obj.bounds_half);
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-h.x, 0.0, -h.z),
                Vec3::new(h.x, h.y * 1.6, h.z),
                tint,
            );
            let roof = [tint[0] * 0.72, tint[1] * 0.55, tint[2] * 0.45, 1.0];
            append_pyramid(verts, indices, mat, h.y * 1.6, h.y * 2.2, h.x * 1.08, roof);
        }
        ObjectKind::ModelEcm => {
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-0.55, 0.0, -0.55),
                Vec3::new(0.55, 2.0, 0.55),
                tint,
            );
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-0.35, 2.0, -0.35),
                Vec3::new(0.35, 2.7, 0.35),
                tint,
            );
        }
        ObjectKind::EffectGfx | ObjectKind::Light => {
            append_pyramid(verts, indices, mat, 0.8, 1.8, 0.65, tint);
            append_pyramid(verts, indices, mat, 0.8, 0.0, 0.65, tint);
        }
        ObjectKind::SoundSfx => {
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-0.6, 0.2, -0.6),
                Vec3::new(0.6, 1.4, 0.6),
                tint,
            );
        }
        ObjectKind::Water => {
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-8.0, 0.0, -8.0),
                Vec3::new(8.0, 0.25, 8.0),
                [tint[0], tint[1], tint[2], 0.72],
            );
        }
        _ => {
            let h = Vec3::from(obj.bounds_half);
            append_box(
                verts,
                indices,
                mat,
                Vec3::new(-h.x, 0.0, -h.z),
                Vec3::new(h.x, h.y * 2.0, h.z),
                tint,
            );
        }
    }
}

fn append_grid_lines(lines: &mut Vec<LineVertex>, terrain: &EditableTerrain) {
    let hw = terrain.half_width();
    let hl = terrain.half_length();
    let step = 8.0f32;
    let y = 0.06f32;
    let minor = [0.27, 0.29, 0.33, 0.45];
    let major = [0.38, 0.41, 0.47, 0.65];

    let nx = (hw / step).floor() as i32;
    let nz = (hl / step).floor() as i32;
    for i in -nx..=nx {
        let x = i as f32 * step;
        let col = if i == 0 {
            [0.24, 0.52, 0.96, 0.95] // Blue Z axis
        } else if i % 4 == 0 {
            major
        } else {
            minor
        };
        lines.push(LineVertex {
            pos: [x, y, -hl],
            color: col,
        });
        lines.push(LineVertex {
            pos: [x, y, hl],
            color: col,
        });
    }
    for j in -nz..=nz {
        let z = j as f32 * step;
        let col = if j == 0 {
            [0.92, 0.28, 0.28, 0.95] // Red X axis
        } else if j % 4 == 0 {
            major
        } else {
            minor
        };
        lines.push(LineVertex {
            pos: [-hw, y, z],
            color: col,
        });
        lines.push(LineVertex {
            pos: [hw, y, z],
            color: col,
        });
    }
    // Vertical origin Y-axis indicator
    lines.push(LineVertex {
        pos: [0.0, 0.0, 0.0],
        color: [0.30, 0.85, 0.35, 0.9],
    });
    lines.push(LineVertex {
        pos: [0.0, 14.0, 0.0],
        color: [0.30, 0.85, 0.35, 0.9],
    });
}

fn append_aabb_lines(lines: &mut Vec<LineVertex>, min: Vec3, max: Vec3, color: [f32; 4]) {
    let c = [
        [min.x, min.y, min.z],
        [max.x, min.y, min.z],
        [max.x, max.y, min.z],
        [min.x, max.y, min.z],
        [min.x, min.y, max.z],
        [max.x, min.y, max.z],
        [max.x, max.y, max.z],
        [min.x, max.y, max.z],
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (a, b) in edges {
        lines.push(LineVertex { pos: c[a], color });
        lines.push(LineVertex { pos: c[b], color });
    }
}

fn append_brush_ring(
    lines: &mut Vec<LineVertex>,
    terrain: &EditableTerrain,
    center: Vec3,
    radius: f32,
) {
    let segs = 40usize;
    let col = [0.28, 0.85, 1.0, 0.95];
    let mut prev = None;
    for i in 0..=segs {
        let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
        let x = center.x + a.cos() * radius;
        let z = center.z + a.sin() * radius;
        let y = terrain.sample_height(x, z) + 0.25;
        let p = [x, y, z];
        if let Some(p0) = prev {
            lines.push(LineVertex {
                pos: p0,
                color: col,
            });
            lines.push(LineVertex { pos: p, color: col });
        }
        prev = Some(p);
    }
}

fn append_gizmo_lines(
    lines: &mut Vec<LineVertex>,
    pivot: Vec3,
    len: f32,
    mode: GizmoMode,
    hover: Option<GizmoAxis>,
) {
    let x_col = if hover == Some(GizmoAxis::X) {
        [1.0, 0.85, 0.3, 1.0]
    } else {
        [0.95, 0.26, 0.26, 1.0]
    };
    let y_col = if hover == Some(GizmoAxis::Y) {
        [1.0, 0.85, 0.3, 1.0]
    } else {
        [0.30, 0.90, 0.34, 1.0]
    };
    let z_col = if hover == Some(GizmoAxis::Z) {
        [1.0, 0.85, 0.3, 1.0]
    } else {
        [0.28, 0.56, 0.98, 1.0]
    };

    match mode {
        GizmoMode::Move | GizmoMode::Scale => {
            let axes = [
                (Vec3::X, x_col, Vec3::Y),
                (Vec3::Y, y_col, Vec3::X),
                (Vec3::Z, z_col, Vec3::Y),
            ];
            for (dir, col, ortho) in axes {
                let tip = pivot + dir * len;
                lines.push(LineVertex {
                    pos: pivot.to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: tip.to_array(),
                    color: col,
                });
                let head = len * 0.16;
                let side = dir.cross(ortho).normalize_or_zero() * (head * 0.5);
                let up = ortho * (head * 0.5);
                let back = tip - dir * head;
                lines.push(LineVertex {
                    pos: tip.to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: (back + side).to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: tip.to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: (back - side).to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: tip.to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: (back + up).to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: tip.to_array(),
                    color: col,
                });
                lines.push(LineVertex {
                    pos: (back - up).to_array(),
                    color: col,
                });
            }
        }
        GizmoMode::Rotate => {
            let rings = [
                (Vec3::Y, Vec3::Z, x_col),
                (Vec3::X, Vec3::Z, y_col),
                (Vec3::X, Vec3::Y, z_col),
            ];
            let segs = 32usize;
            for (u, v, col) in rings {
                let mut prev = None;
                for i in 0..=segs {
                    let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
                    let p = (pivot + (u * a.cos() + v * a.sin()) * (len * 0.85)).to_array();
                    if let Some(p0) = prev {
                        lines.push(LineVertex {
                            pos: p0,
                            color: col,
                        });
                        lines.push(LineVertex { pos: p, color: col });
                    }
                    prev = Some(p);
                }
            }
        }
        GizmoMode::None => {}
    }
}
