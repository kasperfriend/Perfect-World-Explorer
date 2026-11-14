//! Batched receiver for the runtime effect quads.
//!
//! The static decal pass in `gfx_decal_gpu` builds one vertex buffer and one
//! bind group per quad, which is right for a handful of character decals and
//! wrong for a particle system: a single retail torch root can hold three
//! emitters at a quota of a hundred particles each, and a scene block can hold
//! dozens of those roots. This pass therefore groups the frame's quads by
//! `(RenderLayer, depth test, depth write, blend pair, texture)` and draws one
//! buffer per group, growing the buffers instead of reallocating them.
//!
//! Render state follows `A3DGFXRenderSlotMan`, the same manager the verified
//! static path reproduces: colour and alpha modulate, lighting off, culling off,
//! the authored D3D8 blend pair selected per element, and depth writes off —
//! except on a skin draw, where element 160's own `WriteZ` decides, because the
//! slot manager hands a model to `A3DSkinModel::RenderAtOnce` rather than to a
//! quad slot. The
//! element's `RenderLayer` decides the draw order, which is what the field is
//! for; within a layer, depth-tested quads go first so the additive ones blend
//! against settled geometry.

use crate::{gfx_player::EffectQuad, model_queue, model_texture_gpu};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
    /// The shaded colour. For a quad this is the element colour; for a skin mesh
    /// it is the lighting constant `c7` — `material ambient * environment
    /// ambient + emissive + extra ambient` — because `A3DGFXModel` spends the
    /// element diffuse on the *material*, not on a vertex tint.
    lit: [f32; 4],
    /// `c8`, the directional diffuse constant: `material diffuse * light
    /// diffuse`, scaled by the Lambert term in the shader. Zero for a quad.
    directional: [f32; 4],
    /// World-space normal for a skin mesh, zero for a quad.
    normal: [f32; 3],
    /// `c9` and the Blinn exponent: `xyz` is the material specular colour bound
    /// against the frame's directional specular light, `w` the material power.
    /// Zero for a quad, which the client never shades specularly.
    specular: [f32; 4],
    /// The slot's `m_enumFogFlag`, not a yes/no switch: `0` would mean unfogged,
    /// `1` fades towards the frame's `fog_color` (`GFX_FOG_CURRENT`) and `2`
    /// fades towards black (`GFX_FOG_ZERO`). `A3DGFXRenderSlot.cpp:341` starts
    /// every slot at `CURRENT` and `:349-352` downgrades only an additive slot
    /// (`DestBlend == A3DBLEND_ONE`) to `ZERO`, so an added glow dims into black
    /// with distance while every other sheet dims into the region's fog.
    fog: f32,
}

/// The light an element-160 mesh is shaded with, in the client's own terms.
///
/// `A3DGFXModel::UpdateModel` builds an `A3DSkinModel::LIGHTINFO` from
/// `AfxGetLightparam()`: device ambient, the directional light's direction and
/// its diffuse and specular colours, with `bPtLight = false` — a model never
/// takes the dynamic point light. The same `LIGHTINFO` reaches both a scenery
/// skin model and an effect model, because they are the same class.
#[derive(Clone, Copy, Debug)]
pub struct EffectLight {
    pub lighting: crate::model_material::Lighting,
    pub direction: [f32; 3],
    /// The frame's `fog_range`: `x` starts, `y` ends, `z` enables.
    pub fog_range: [f32; 4],
    /// Linear fog colour, `rgb` only.
    pub fog_color: [f32; 4],
    /// The frame's `directional_specular`: the light's specular colour, which
    /// `bind_specular` binds against each material's own specular colour.
    pub directional_specular: [f32; 4],
    /// The camera's forward direction, which is what the client's Blinn term
    /// takes as its eye input (not a per-vertex vector to the eye).
    pub eye_direction: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    clip_plane: [f32; 4],
    /// `xyz` is the camera position. The root's `FadeByDist` ramp is already
    /// folded into the vertex colours on the CPU (`gfx_player::distance_fade`),
    /// so the shader does not read this field; it is uploaded because the
    /// client's GFX constants carry the eye position alongside the matrix.
    camera: [f32; 4],
    /// `xyz` is the directional light's direction, the `vLightDir` of the
    /// model's `LIGHTINFO`. Quads ignore it; a skin mesh needs it for the
    /// Lambert term its material constants are multiplied by.
    light: [f32; 4],
    /// `x` starts the fog, `y` ends it and `z` enables it, the same
    /// `fog_range` the scenery model shader reads.
    fog_range: [f32; 4],
    fog_color: [f32; 4],
    /// `xyz` is the camera's forward direction for the model Blinn term.
    eye: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    source: u32,
    destination: u32,
    depth_test: bool,
    /// Element 160's `WriteZ`. The slot manager never writes depth for a
    /// *quad*, but a model slot is a skin draw: `A3DGFXModel::PrepareRenderSkinModel`
    /// ends with `SetZWriteEnable(m_bZWriteEnable)`, a member whose constructor
    /// value is false and which only the version-49 `WriteZ` key turns on — 115
    /// of the shipped 447 models author `1`, 131 author `0`, 201 predate the key.
    /// Without it a crystal cannot occlude the sheet drawn after it.
    write_depth: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BucketKey {
    layer: i32,
    /// `Priority` orders elements *inside* a layer. It is a sort key only, so it
    /// lives here rather than in `PipelineKey` — a distinct pipeline per
    /// priority would reload the shader for no gain.
    priority: i32,
    /// `TileMode`: the sampler state is per slot (`A3DGFXRenderSlot.cpp:547-549`),
    /// so a tile group binds the WRAP sampler and every other group CLAMPs.
    tile: bool,
    pipeline: PipelineKey,
    texture: String,
}

struct TextureEntry {
    // The view keeps the texture alive for the bind groups that reference it.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Bucket {
    pipeline: PipelineKey,
    binding: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    /// Vertices the buffer holds; the CPU side is refilled every frame.
    capacity: usize,
    /// Whether `binding` points at the placeholder rather than the real
    /// texture. A bucket built while the texture was missing must rebind
    /// once it loads — the cache key matches, so nothing else would.
    bound_fallback: bool,
}

/// How many distinct (layer, state, texture) groups are cached before the pass
/// drops them all. Retail roots reuse a few hundred at most.
const BUCKET_CACHE_LIMIT: usize = 1024;

pub struct Draw {
    device_format: wgpu::TextureFormat,
    depth: Option<(wgpu::TextureFormat, wgpu::CompareFunction)>,
    binding_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    textures: HashMap<String, TextureEntry>,
    buckets: HashMap<BucketKey, Bucket>,
    uniform: wgpu::Buffer,
    sampler_repeat: wgpu::Sampler,
    sampler_clamp: wgpu::Sampler,
    /// Stand-in used when an element's texture will not load.
    ///
    /// Dropping the bucket instead — which this used to do — means a single
    /// unreadable file hides every quad that shares it, and on the shipped
    /// corpus there is no guarantee that every named texture is present. The
    /// client keeps drawing such an element with its vertex colour, so a white
    /// opaque pixel reproduces that: the geometry is still there, the colour is
    /// still the authored one, only the detail is missing.
    fallback: TextureEntry,
    /// This frame's groups in draw order, with their vertex counts.
    order: Vec<(BucketKey, u32)>,
    staging: Vec<Vertex>,
    failed: Vec<String>,
}

fn blend_factor(code: u32) -> Result<wgpu::BlendFactor, String> {
    Ok(match model_queue::blend_factor(code)? {
        model_queue::Factor::Zero => wgpu::BlendFactor::Zero,
        model_queue::Factor::One => wgpu::BlendFactor::One,
        model_queue::Factor::SrcColor => wgpu::BlendFactor::Src,
        model_queue::Factor::InvSrcColor => wgpu::BlendFactor::OneMinusSrc,
        model_queue::Factor::SrcAlpha => wgpu::BlendFactor::SrcAlpha,
        model_queue::Factor::InvSrcAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
        model_queue::Factor::DestAlpha => wgpu::BlendFactor::DstAlpha,
        model_queue::Factor::InvDestAlpha => wgpu::BlendFactor::OneMinusDstAlpha,
        model_queue::Factor::DestColor => wgpu::BlendFactor::Dst,
        model_queue::Factor::InvDestColor => wgpu::BlendFactor::OneMinusDst,
    })
}

/// The material constants one element-160 mesh draws with, in `A3DSkinModel`'s
/// own arithmetic.
///
/// `A3DGFXModel::UpdateModel` spends the element diffuse — keypoint colour times
/// the root's actual alpha — on the *material*: when its RGB is not white it
/// switches the skin model to `MTL_SCALED` and multiplies each material colour by
/// `diffuse.rgb / 255`, clamped; when its alpha is not 255 it passes
/// `m_fTransparent = 1 - a/255` instead. `model_material::bind` then produces
/// `c7` (ambient) and `c8` (directional diffuse) from the scaled material and the
/// frame's light. A mesh with no material keeps the element colour and takes no
/// light, which is the flat look the pass had before this.
fn shade(
    mesh: &crate::gfx_player::EffectMesh,
    color: [f32; 4],
    light: Option<EffectLight>,
) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let (Some(material), Some(light)) = (mesh.material, light) else {
        return (color, [0.; 4], [0.; 4]);
    };
    let scale = [color[0], color[1], color[2], 1.];
    let mut lighting = light.lighting;
    // `A3DGFXModel` sets `bPtLight = false` in every `LIGHTINFO` it builds, so a
    // model element never takes the dynamic point light.
    lighting.point_enabled = false;
    // The client's `_white_alpha` test: an alpha of 255 leaves the material
    // alpha alone (`m_fTransparent` stays -1), anything else overrides `c8.a`.
    lighting.transparency = if color[3] < 1. { 1. - color[3] } else { -1. };
    let scaled = crate::model_material::Material {
        ambient: crate::model_material::scale_rgba(material.ambient, scale),
        diffuse: crate::model_material::scale_rgba(material.diffuse, scale),
        emissive: crate::model_material::scale_rgba(material.emissive, scale),
    };
    let constants = crate::model_material::bind(scaled, lighting);
    // `c9`: the material's specular colour bound against the directional light's
    // specular colour, with the same `MTL_SCALED` factor the other coefficients
    // take. The exponent rides in `w`.
    let specular = crate::model_material::bind_specular(
        crate::model_material::scale_rgba(material.specular, scale),
        light.directional_specular,
    );
    (
        constants.ambient,
        constants.directional_diffuse,
        [specular[0], specular[1], specular[2], material.power],
    )
}

impl Draw {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        depth: Option<(wgpu::TextureFormat, wgpu::CompareFunction)>,
    ) -> Result<Self, String> {
        if !matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return Err("GFX effects require an encoded UNORM target".into());
        }
        // The sampler state is **per slot** in the client:
        // `A3DGFXRenderSlotMan::PreRender` gives a `m_bTileMode` slot
        // `A3DTADDR_WRAP` and every other slot `A3DTADDR_CLAMP`
        // (A3DGFXRenderSlot.cpp:547-549). Both halves matter:
        //
        // * a tile must wrap — a waterfall has no flipbook (its sheet is one
        //   image, `TexRow`/`TexCol` 1) and its whole animation is the
        //   `TexSpeed` UV scroll, which walks past [0,1] for as long as the
        //   effect plays. Clamping that scroll freezes the sheet on its edge
        //   texel — the waterfall stops dead — and stretches those edge texels
        //   across the quad as vertical streaks. Clamping was tried and produced
        //   exactly that, so a tile keeps the wrap.
        // * a non-tile card must clamp. The fractal strips put
        //   `m_fTexWid * ratio + m_fTexU` along their length
        //   (A3DGFXLightning.cpp:836) and the pair's two vertices share one u
        //   (`:953-958`), so a widened strip corner crosses u = 1 and the wrap
        //   sampler drew the opposite edge of the lightning sheet there — the
        //   `u = 1.033` sheared corners logged on 血鲷封印台罩子闪电 and
        //   闪电子.
        //
        // One Repeat sampler for every slot was both bugs at once: it was the
        // only reason the tile scroll ran, and the only reason the lightning
        // wrapped. The bucket key carries `tile` so each group binds its own.
        let sampler_repeat = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native GFX effect sampler (tile slot, WRAP)"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sampler_clamp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native GFX effect sampler (non-tile slot, CLAMP)"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("GFX effect frame constants"),
            contents: bytemuck::bytes_of(&Params {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                clip_plane: [0.; 4],
                camera: [0.; 4],
                light: [0.; 4],
                fog_range: [0.; 4],
                fog_color: [0.; 4],
                eye: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let binding_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("GFX effect binding layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("GFX effect pipeline layout"),
            bind_group_layouts: &[&binding_layout],
            push_constant_ranges: &[],
        });
        // `model_light::WGSL` is the ported skin-model vertex lighting
        // (`model_specular_variant_diffuse`, `model_directional_specular`) with
        // no bindings of its own, so the effect shader composes it in rather
        // than carrying a second copy of the maths.
        let source = format!("{}\n{}", crate::model_light::WGSL, WGSL);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native GFX effect shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let fallback = {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("native GFX effect placeholder texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            // Fully transparent is the wrong stand-in: the client never draws
            // a zero texel for a missing map. `A3DGFXRenderSlotMan` keeps the
            // slot untextured and runs its NOTEX pixel shader
            // (`m_pDefaultPS_NoTex`, "gfx.hlslp#NOTEX$1"), i.e. the quad comes
            // out as its **vertex colour** — the recorded fallback law at
            // A3DGFXRenderSlot.cpp:78/227. A white texel through the WGSL
            // multiply path reproduces that exactly: colour = texel × vertex
            // = vertex. The zero texel made every unresolved map cost the
            // element its sprite; the client still draws the quad (a
            // `RingGlowWhite9` ring becomes a white slab in retail too, which
            // is why the earlier white run looked wrong but matched).
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &[255, 255, 255, 255],
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&Default::default());
            TextureEntry { texture, view }
        };
        Ok(Self {
            device_format: format,
            depth,
            binding_layout,
            pipeline_layout,
            shader,
            pipelines: HashMap::new(),
            textures: HashMap::new(),
            buckets: HashMap::new(),
            uniform,
            sampler_repeat,
            sampler_clamp,
            fallback,
            order: Vec::new(),
            staging: Vec::new(),
            failed: Vec::new(),
        })
    }

    /// Textures that failed to load, for the status line.
    pub fn failures(&self) -> &[String] {
        &self.failed
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    pub fn vertex_count(&self) -> usize {
        self.order.iter().map(|(_, count)| *count as usize).sum()
    }

    /// Refill every group from this frame's quads. `read` resolves an archive
    /// path to bytes and is only called for textures the pass has not seen.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        quads: &[EffectQuad],
        matrix: [[f32; 4]; 4],
        clip_plane: [f32; 4],
        camera: [f32; 4],
        light: Option<EffectLight>,
        read: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        if matrix
            .iter()
            .flatten()
            .chain(clip_plane.iter())
            .chain(camera.iter())
            .any(|value| !value.is_finite())
        {
            return Err("non-finite GFX effect frame constants".into());
        }
        let direction = light.map_or([0.; 3], |light| light.direction);
        if direction.iter().any(|value| !value.is_finite()) {
            return Err("non-finite GFX effect light direction".into());
        }
        // No light means no fog either: both come from the same frame.
        let (fog_range, fog_color) = light.map_or(([0.; 4], [0.; 4]), |light| {
            (light.fog_range, light.fog_color)
        });
        let eye = light.map_or([0.; 3], |light| light.eye_direction);
        if eye.iter().any(|value| !value.is_finite()) {
            return Err("non-finite GFX effect eye direction".into());
        }
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Params {
                matrix,
                clip_plane,
                camera,
                light: [direction[0], direction[1], direction[2], 0.],
                fog_range,
                fog_color,
                eye: [eye[0], eye[1], eye[2], 0.],
            }),
        );

        // Group and order: `RenderLayer` first, then depth-tested quads, then
        // the blend pair and the texture so a group reuses one pipeline.
        let mut groups: Vec<(BucketKey, Vec<Vertex>)> = Vec::new();
        let mut index: HashMap<BucketKey, usize> = HashMap::new();
        for quad in quads {
            let key = BucketKey {
                layer: quad.layer,
                pipeline: PipelineKey {
                    source: quad.source_blend,
                    destination: quad.destination_blend,
                    depth_test: quad.depth_test,
                    write_depth: quad.mesh.as_ref().is_some_and(|mesh| mesh.write_depth),
                },
                priority: quad.priority,
                tile: quad.tile,
                texture: quad.texture.clone(),
            };
            let position = match index.get(&key) {
                Some(position) => *position,
                None => {
                    index.insert(key.clone(), groups.len());
                    groups.push((key.clone(), Vec::new()));
                    groups.len() - 1
                }
            };
            let vertices = &mut groups[position].1;
            if let Some(mesh) = &quad.mesh {
                // A model element arrives as a triangle soup already in world
                // space, so it needs no corner expansion and no winding order.
                let (lit, directional, specular) = shade(mesh, quad.color, light);
                for vertex in &mesh.vertices {
                    vertices.push(Vertex {
                        position: vertex.position,
                        uv: vertex.uv,
                        lit,
                        directional,
                        normal: vertex.normal,
                        specular,
                        fog: 1.,
                    });
                }
                continue;
            }
            // Two triangles per quad, in the client's corner order. A quad is
            // not lit: `A3DGFXRenderSlot` draws it with lighting off, exactly as
            // the element's colour says.
            //
            // Its fog colour is the slot's, not a choice of whether to fog:
            // `m_enumFogFlag` is `GFX_FOG_ZERO` only for `DestBlend ==
            // A3DBLEND_ONE` (code 2 in `model_queue::blend_factor`), which is the
            // client's additive slot.
            let fog = if quad.destination_blend == 2 { 2. } else { 1. };
            for slot in [0usize, 1, 3, 0, 3, 2] {
                vertices.push(Vertex {
                    position: quad.positions[slot],
                    uv: quad.uv[slot],
                    // A grid carries one authored colour per corner and the
                    // pipeline interpolates it across the cell; every other
                    // class paints the whole quad with `color`.
                    lit: quad.vertex_colors.map_or(quad.color, |colors| colors[slot]),
                    directional: [0.; 4],
                    normal: [0.; 3],
                    specular: [0.; 4],
                    fog,
                });
            }
        }
        groups.sort_by(|(a, _), (b, _)| {
            a.layer
                .cmp(&b.layer)
                // Depth writers establish the buffer the blended sheets are
                // tested against, so within a layer they go first whatever the
                // authored `Priority` says: a model drawn after the sheet it
                // stands in front of would otherwise be painted over.
                .then_with(|| b.pipeline.write_depth.cmp(&a.pipeline.write_depth))
                // `Priority` orders the elements inside one `RenderLayer`: the
                // client authors it 1..5 on every element (7,820 are a 4 and
                // 5,586 a 2) and nothing read it, so a background dust sheet
                // and the flash over it landed in whatever order the texture
                // bucket happened to produce.
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.pipeline.depth_test.cmp(&b.pipeline.depth_test).reverse())
                .then_with(|| a.pipeline.source.cmp(&b.pipeline.source))
                .then_with(|| a.pipeline.destination.cmp(&b.pipeline.destination))
                .then_with(|| a.texture.cmp(&b.texture))
                .then_with(|| a.tile.cmp(&b.tile))
        });

        if self.buckets.len() + groups.len() > BUCKET_CACHE_LIMIT {
            self.buckets.clear();
        }
        self.order.clear();
        self.order.reserve(groups.len());
        for (key, vertices) in &groups {
            if vertices.is_empty() {
                continue;
            }
            if !self.textures.contains_key(&key.texture) {
                match read(&key.texture) {
                    Ok(bytes) => {
                        match model_texture_gpu::upload(
                            device,
                            queue,
                            &bytes,
                            model_texture_gpu::UploadMode::Auto,
                        ) {
                            Ok(texture) => {
                                let view = texture.create_view(&Default::default());
                                self.textures
                                    .insert(key.texture.clone(), TextureEntry { texture, view });
                            }
                            Err(error) => self.note_failure(&key.texture, &error),
                        }
                    }
                    // The element still draws, with the white stand-in: the
                    // quads and their authored colour survive (the NOTEX
                    // vertex-colour law), only the map is lost. Returning here
                    // instead would delete the bucket and the element with it.
                    Err(error) => self.note_failure(&key.texture, &error),
                }
            }
            let entry = self.textures.get(&key.texture).unwrap_or(&self.fallback);
            if !self.pipelines.contains_key(&key.pipeline) {
                let pipeline = self.pipeline(device, key.pipeline)?;
                self.pipelines.insert(key.pipeline, pipeline);
            }
            let loaded = self.textures.contains_key(&key.texture);
            let was_fallback = self.buckets.get(key).is_some_and(|b| b.bound_fallback);
            if !self.buckets.contains_key(key) {
                let binding = self.binding_for(device, entry, key.tile);
                let capacity = vertices.len().next_power_of_two();
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("GFX effect quad batch"),
                    size: (capacity * std::mem::size_of::<Vertex>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.buckets.insert(
                    key.clone(),
                    Bucket {
                        pipeline: key.pipeline,
                        binding,
                        buffer,
                        capacity,
                        bound_fallback: !loaded,
                    },
                );
            } else if loaded && was_fallback {
                // Fail-then-succeed: the first frames bound the placeholder
                // because the texture would not load, and now it has. The
                // bucket survives (same key), so its binding must be rebuilt
                // against the real view or the element draws the placeholder
                // for the rest of the session.
                let binding = self.binding_for(device, entry, key.tile);
                if let Some(bucket) = self.buckets.get_mut(key) {
                    bucket.binding = binding;
                    bucket.bound_fallback = false;
                }
            }
            let bucket = match self.buckets.get_mut(key) {
                Some(bucket) => bucket,
                None => continue,
            };
            if vertices.len() > bucket.capacity {
                bucket.capacity = vertices.len().next_power_of_two();
                bucket.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("GFX effect quad batch"),
                    size: (bucket.capacity * std::mem::size_of::<Vertex>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.staging.clear();
            self.staging.extend_from_slice(vertices);
            queue.write_buffer(&bucket.buffer, 0, bytemuck::cast_slice(&self.staging));
            self.order.push((key.clone(), vertices.len() as u32));
        }
        Ok(())
    }

    fn note_failure(&mut self, texture: &str, error: &str) {
        let message = format!("{texture}: {error}");
        if !self.failed.contains(&message) {
            self.failed.push(message);
        }
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for (key, count) in &self.order {
            let (Some(bucket), Some(pipeline)) =
                (self.buckets.get(key), self.pipelines.get(&key.pipeline))
            else {
                continue;
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bucket.binding, &[]);
            pass.set_vertex_buffer(0, bucket.buffer.slice(..));
            pass.draw(0..*count, 0..1);
        }
    }

    /// The texture binding one bucket draws with: the uniform frame block, the
    /// entry's texture view (the real map or the placeholder), and the
    /// per-slot sampler (`A3DGFXRenderSlot` WRAP for a tile, CLAMP otherwise).
    fn binding_for(
        &self,
        device: &wgpu::Device,
        entry: &TextureEntry,
        tile: bool,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("GFX effect texture binding"),
            layout: &self.binding_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&entry.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(if tile {
                        &self.sampler_repeat
                    } else {
                        &self.sampler_clamp
                    }),
                },
            ],
        })
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        key: PipelineKey,
    ) -> Result<wgpu::RenderPipeline, String> {
        let component = wgpu::BlendComponent {
            src_factor: blend_factor(key.source)?,
            dst_factor: blend_factor(key.destination)?,
            operation: wgpu::BlendOperation::Add,
        };
        // position, uv, lit, directional, normal: a quad leaves the last three
        // at zero, a skin mesh fills them from its material and its pose.
        let attributes = wgpu::vertex_attr_array![
            0=>Float32x3,1=>Float32x2,2=>Float32x4,
            3=>Float32x4,4=>Float32x3,5=>Float32,
            6=>Float32x4
        ];
        let buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        }];
        Ok(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("native GFX effect receiver"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: "vs",
                    buffers: &buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.device_format,
                        blend: Some(wgpu::BlendState {
                            color: component,
                            alpha: component,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: self.depth.map(|(format, compare)| wgpu::DepthStencilState {
                    format,
                    // Only a mesh whose element asks for `WriteZ` writes; a
                    // quad slot never does (A3DGFXRenderSlot has no Z-write
                    // field) and a missing depth attachment suppresses it.
                    depth_write_enabled: key.write_depth,
                    depth_compare: if key.depth_test {
                        compare
                    } else {
                        wgpu::CompareFunction::Always
                    },
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
            }),
        )
    }
}

const WGSL: &str = r#"
struct Params { matrix:mat4x4<f32>, clip_plane:vec4<f32>, camera:vec4<f32>, light:vec4<f32>, fog_range:vec4<f32>, fog_color:vec4<f32>, eye:vec4<f32> };
@group(0) @binding(0) var<uniform> p:Params;
@group(0) @binding(1) var image:texture_2d<f32>;
@group(0) @binding(2) var image_sampler:sampler;
struct Input {
 @location(0) position:vec3<f32>,
 @location(1) uv:vec2<f32>,
 @location(2) lit:vec4<f32>,
 @location(3) directional:vec4<f32>,
 @location(4) normal:vec3<f32>,
 @location(5) fog:f32,
 @location(6) specular:vec4<f32>,
};
struct Output {
 @builtin(position) position:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) color:vec4<f32>,
 @location(2) world:vec3<f32>,
 @location(3) fog:f32,
 @location(4) specular:vec3<f32>,
};
@vertex fn vs(v:Input)->Output {
 var o:Output;
 o.position=p.matrix*vec4<f32>(v.position,1.);
 o.world=v.position;
 o.uv=v.uv;
 o.fog=v.fog;
 // The shading sum is the ported skin-model function itself, called with the
 // point light off: `ambient + directional * lambert` clamped to one per channel,
 // and an alpha of `ambient.a + directional.a` that is NOT scaled by the Lambert
 // term (only the point-light variant multiplies alpha by it). A quad carries a
 // zero normal and a zero directional constant, so this reduces to its element
 // colour and the pass keeps drawing it unlit.
 o.color=model_specular_variant_diffuse(v.position,v.normal,p.light.xyz,vec3<f32>(0.),vec3<f32>(1.,0.,0.),v.lit,v.directional,vec4<f32>(0.),vec4<f32>(0.),0.);
 // `model_directional_specular`: `A3DGFXModel::ChangeModel` calls
 // `EnableSpecular(true)` on every effect model, so the Blinn term is always
 // evaluated; a quad's `specular` is zero and its normal is zero, which the
 // function rejects with a zero result.
 o.specular=model_directional_specular(v.normal,p.light.xyz,p.eye.xyz,v.specular.w,v.specular.rgb);
 return o;
}
@fragment fn fs(v:Output)->@location(0) vec4<f32> {
 if dot(vec4<f32>(v.world,1.),p.clip_plane)<0. {discard;}
 let tex=textureSample(image,image_sampler,v.uv);
 // The client's fragment stage is `clamp(texel.rgb * diffuse.rgb + specular)`,
 // and its opacity is the vertex alpha times the texture alpha.
 var rgb=clamp(tex.rgb*v.color.rgb+v.specular,vec3<f32>(0.),vec3<f32>(1.));
 let opacity=clamp(tex.a*v.color.a,0.,1.);
 // Every slot is fogged while the device's fog is on; the retained fraction is
 // `(end - depth) / span` on the reciprocal of the clip w, and zero below the
 // near plane, the same formula a scenery skin model uses. `m_enumFogFlag`
 // selects the colour rather than the yes/no: `GFX_FOG_CURRENT` (1) fades to the
 // frame's fog colour and `GFX_FOG_ZERO` (2, additive slots) fades to black.
 if p.fog_range.z>0.5 && v.fog>0.5 {
  let depth=1./max(v.position.w,0.0000001);
  let span=p.fog_range.y-p.fog_range.x;
  var keep:f32;
  if abs(span)<0.001 {keep=select(1.,0.,depth>=p.fog_range.y);}
  else {keep=clamp((p.fog_range.y-depth)/span,0.,1.);}
  let fog_rgb=select(p.fog_color.rgb,vec3<f32>(0.),v.fog>1.5);
  rgb=mix(fog_rgb,rgb,keep);
 }
 return vec4<f32>(rgb,opacity);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(
        texture: &str,
        source: u32,
        destination: u32,
        layer: i32,
        depth_test: bool,
        color: [f32; 4],
    ) -> EffectQuad {
        EffectQuad {
            texture: texture.into(),
            source_blend: source,
            destination_blend: destination,
            depth_test,
            layer,
            priority: 0,
            tile: false,
            positions: [[-1., -1., 0.], [1., -1., 0.], [-1., 1., 0.], [1., 1., 0.]],
            uv: [[0., 0.], [1., 0.], [0., 1.], [1., 1.]],
            color,
            vertex_colors: None,
            mesh: None,
        }
    }

    /// A single screen-covering triangle of an element-160 mesh, shaded with
    /// `material` and the given world normal.
    fn mesh_quad(material: crate::gfx_player::MeshMaterial, normal: [f32; 3]) -> EffectQuad {
        let vertex = |position: [f32; 3]| crate::gfx_player::EffectVertex {
            position,
            uv: [0., 0.],
            normal,
        };
        EffectQuad {
            texture: "a.png".into(),
            source_blend: 5,
            destination_blend: 2,
            depth_test: false,
            layer: 0,
            priority: 0,
            tile: true,
            positions: [[0.; 3]; 4],
            uv: [[0.; 2]; 4],
            color: [1.; 4],
            vertex_colors: None,
            mesh: Some(crate::gfx_player::EffectMesh {
                texture: "a.png".into(),
                vertices: vec![
                    vertex([-1., -1., 0.]),
                    vertex([3., -1., 0.]),
                    vertex([-1., 3., 0.]),
                ],
                write_depth: false,
                material: Some(material),
            }),
        }
    }

    /// The source's own lighting, plus point-light colours that must be ignored:
    /// `A3DGFXModel::UpdateModel` fills `LIGHTINFO` with `bPtLight = false` for
    /// every model it builds, so no model element takes the dynamic point light.
    fn effect_light() -> EffectLight {
        EffectLight {
            lighting: crate::model_material::Lighting {
                environment_ambient: [0.; 4],
                additional_ambient: [0.; 4],
                directional_diffuse: [1.; 4],
                point_ambient: [1.; 4],
                point_diffuse: [1.; 4],
                transparency: -1.,
                point_enabled: true,
            },
            direction: [0., 0., -1.],
            // Fog off, so the shading assertions read the light alone.
            fog_range: [0.; 4],
            fog_color: [0.; 4],
            // No specular light, so the diffuse assertions stay exact; the
            // specular test raises this itself.
            directional_specular: [0.; 4],
            eye_direction: [0., 0., -1.],
        }
    }

    /// A solid 64-grey 4x4 texture: the blend arithmetic is the only variable.
    fn grey_png() -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([64, 64, 64, 255]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn white_png() -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 255, 255, 255]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    struct Harness {
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: wgpu::Texture,
        readback: wgpu::Buffer,
    }

    fn harness() -> Option<Harness> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            },
            None,
        ))
        .ok()?;
        let size = wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("effect target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Harness {
            device,
            queue,
            target,
            readback,
        })
    }

    /// Render `quads` over `background` and return the first pixel's channels.
    fn render(
        draw: &mut Draw,
        harness: &Harness,
        background: f64,
        quads: &[EffectQuad],
        light: Option<EffectLight>,
    ) -> [f32; 4] {
        let png = grey_png();
        let mut read = |_: &str| Ok(png.clone());
        render_with(draw, harness, background, quads, light, &mut read)
    }

    fn render_with(
        draw: &mut Draw,
        harness: &Harness,
        background: f64,
        quads: &[EffectQuad],
        light: Option<EffectLight>,
        read: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> [f32; 4] {
        draw.update(
            &harness.device,
            &harness.queue,
            quads,
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            [0.; 4],
            [0., 0., 0., 100.],
            light,
            read,
        )
        .unwrap();
        let view = harness.target.create_view(&Default::default());
        let mut encoder = harness.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effect test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: background,
                            g: background,
                            b: background,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            draw.draw(&mut pass);
        }
        let size = wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &harness.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &harness.readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            size,
        );
        harness.queue.submit(Some(encoder.finish()));
        let slice = harness.readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
        harness.device.poll(wgpu::Maintain::Wait);
        receiver.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range();
        let pixel = [
            bytes[0] as f32 / 255.,
            bytes[1] as f32 / 255.,
            bytes[2] as f32 / 255.,
            bytes[3] as f32 / 255.,
        ];
        drop(bytes);
        harness.readback.unmap();
        pixel
    }

    #[test]
    fn gpu_effect_batches_honour_blend_mode_layer_and_depth_test() {
        let Some(harness) = harness() else {
            eprintln!("effect raster test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();

        // Additive over a 0.25 background: texture grey 64/255 times colour 1.
        // is 0.25, added to 0.25 gives 0.5. An alpha-blended pipeline would
        // have replaced the background instead, so this pins the blend pair.
        let additive = render(
            &mut draw,
            &harness,
            0.25,
            &[quad("a.png", 5, 2, 0, false, [1., 1., 1., 1.])],
            None,
        );
        assert!(
            (additive[0] - 0.5).abs() <= 0.02,
            "additive blend mismatch: {additive:?}"
        );

        // SrcAlpha/InvSrcAlpha with alpha 0.5 over the same background. The
        // fragment stage samples the texel and multiplies it by the quad
        // colour, so the source is the 64-grey map (0.25098), not 1:
        // 0.25098*0.5 + 0.25*0.5 = 0.25049.
        let alpha = render(
            &mut draw,
            &harness,
            0.25,
            &[quad("a.png", 5, 6, 0, false, [1., 1., 1., 0.5])],
            None,
        );
        assert!(
            (alpha[0] - 0.2505).abs() <= 0.02,
            "alpha blend mismatch: {alpha:?}"
        );

        // Two quads in different layers must both draw, and the pass must group
        // four quads over two textures and three states into three batches.
        let png = grey_png();
        let mut read = |_: &str| Ok(png.clone());
        draw.update(
            &harness.device,
            &harness.queue,
            &[
                quad("a.png", 5, 6, 1, false, [0.5, 0.5, 0.5, 1.]),
                quad("a.png", 5, 6, 1, false, [0.5, 0.5, 0.5, 1.]),
                quad("b.png", 5, 2, 0, false, [0.25, 0.25, 0.25, 1.]),
                quad("a.png", 5, 6, 0, true, [0.25, 0.25, 0.25, 1.]),
            ],
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            [0.; 4],
            [0., 0., 0., 100.],
            None,
            &mut read,
        )
        .unwrap();
        assert_eq!(draw.texture_count(), 2);
        assert_eq!(draw.order.len(), 3, "four quads collapse into three groups");
        assert_eq!(draw.vertex_count(), 24);
        assert_eq!(draw.order[0].0.layer, 0);
        assert!(draw.order[0].0.pipeline.depth_test, "depth-tested first");
        assert_eq!(draw.order[2].0.layer, 1);
        assert_eq!(draw.order[2].1, 12);
        // The second frame reused the cached buffers: every group's buffer can
        // still hold its own draw (a 6-vertex group only needs 8), and the pair
        // that grew to twelve vertices kept a buffer at least that large.
        assert!(draw.order.iter().all(|(key, count)| draw
            .buckets
            .get(key)
            .is_some_and(|bucket| bucket.capacity >= *count as usize)));
    }

    /// Element 160 draws through `A3DSkinModel`, so its vertex colour is the
    /// lighting sum `A3DGFXModel` feeds it — `c7 + c8 * lambert`, clamped — and
    /// the element diffuse scales the *material*, not a tint. With a material
    /// ambient of zero and a directional diffuse of one, a facet turned towards
    /// the light renders at the texture's brightness and the same facet turned
    /// away renders black; the pass used to paint both with the flat element
    /// colour. The point-light colours in the fixture prove `bPtLight = false`
    /// is honoured: were they used, the away-facing facet could not be black.
    #[test]
    fn gpu_effect_shades_a_model_mesh_against_the_directional_light() {
        let Some(harness) = harness() else {
            eprintln!("effect shading test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let material = crate::gfx_player::MeshMaterial {
            ambient: [0.; 4],
            diffuse: [1.; 4],
            emissive: [0.; 4],
            specular: [0.; 4],
            power: 1.,
        };
        // Grey 64/255 = 0.25 of texture times a full Lambert, over black.
        let facing = render(
            &mut draw,
            &harness,
            0.,
            &[mesh_quad(material, [0., 0., 1.])],
            Some(effect_light()),
        );
        assert!(
            (facing[0] - 0.25).abs() <= 0.02,
            "lit facet mismatch: {facing:?}"
        );
        let away = render(
            &mut draw,
            &harness,
            0.,
            &[mesh_quad(material, [0., 0., -1.])],
            Some(effect_light()),
        );
        assert!(away[0] <= 0.02, "unlit facet must be black: {away:?}");
        // A mesh with no material keeps the flat element colour, and a quad is
        // never shaded: both still render the texture times that colour.
        let mut flat = mesh_quad(material, [0., 0., -1.]);
        if let Some(mesh) = &mut flat.mesh {
            mesh.material = None;
        }
        let flat = render(&mut draw, &harness, 0., &[flat], Some(effect_light()));
        assert!(
            (flat[0] - 0.25).abs() <= 0.02,
            "a material-less mesh keeps its flat colour: {flat:?}"
        );
    }

    /// Every slot is fogged the way a scenery skin model is: the same
    /// `fog_range` and `fog_color`, applied in the fragment stage to the shaded
    /// colour. At depth 1 with the range `[0, 2]` the retained fraction is a
    /// half, so a grey 0.25 surface picks up half the red fog and lands at red
    /// 0.625, green 0.125. A quad is fogged too, but towards the colour its
    /// slot's `m_enumFogFlag` names: black for an additive slot, the frame's fog
    /// colour for every other one (`A3DGFXRenderSlot.cpp:341`, `:349-352`,
    /// `:565-573`).
    #[test]
    fn gpu_effect_fogs_every_slot_towards_its_own_fog_colour() {
        let Some(harness) = harness() else {
            eprintln!("effect fog test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let mut light = effect_light();
        light.fog_range = [0., 2., 1., 0.];
        light.fog_color = [1., 0., 0., 1.];
        let material = crate::gfx_player::MeshMaterial {
            ambient: [0.; 4],
            diffuse: [1.; 4],
            emissive: [0.; 4],
            specular: [0.; 4],
            power: 1.,
        };
        let fogged = render(
            &mut draw,
            &harness,
            0.,
            &[mesh_quad(material, [0., 0., 1.])],
            Some(light),
        );
        assert!(
            (fogged[0] - 0.625).abs() <= 0.02 && (fogged[1] - 0.125).abs() <= 0.02,
            "fogged mesh mismatch: {fogged:?}"
        );
        // `DestBlend == A3DBLEND_ONE` (code 2) is the client's additive slot, so
        // its flag is `GFX_FOG_ZERO`: the 64-grey sheet fades halfway into black,
        // not into the red fog, leaving 0.25098 * 0.5.
        let additive = render(
            &mut draw,
            &harness,
            0.,
            &[quad("a.png", 5, 2, 0, false, [1., 1., 1., 1.])],
            Some(light),
        );
        assert!(
            (additive[0] - 0.1255).abs() <= 0.02 && (additive[2] - 0.1255).abs() <= 0.02,
            "an additive quad must fade to black: {additive:?}"
        );
        // Every other slot keeps `GFX_FOG_CURRENT`, so the same sheet under
        // SrcAlpha/InvSrcAlpha fades into the red fog instead.
        let current = render(
            &mut draw,
            &harness,
            0.,
            &[quad("a.png", 5, 6, 0, false, [1., 1., 1., 1.])],
            Some(light),
        );
        assert!(
            (current[0] - 0.625).abs() <= 0.02 && (current[1] - 0.125).abs() <= 0.02,
            "a non-additive quad must fade to the current fog colour: {current:?}"
        );
    }

    /// The element's alpha reaches the material as `m_fTransparent = 1 - a/255`
    /// and therefore as `c8.a`, so a model authored at 0.5 opacity must compose as
    /// half coverage. This pins a regression that the compiler could not: the
    /// assignment below the `_white_alpha` comment had been swallowed by the
    /// comment itself, leaving `transparency` at the frame's value and every
    /// element-160 model fully opaque.
    #[test]
    fn gpu_effect_applies_the_element_alpha_as_material_transparency() {
        let Some(harness) = harness() else {
            eprintln!("effect transparency test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let material = crate::gfx_player::MeshMaterial {
            ambient: [0.; 4],
            diffuse: [1.; 4],
            emissive: [0.; 4],
            specular: [0.; 4],
            power: 1.,
        };
        let mut half = mesh_quad(material, [0., 0., 1.]);
        half.color = [1., 1., 1., 0.5];
        half.source_blend = 5;
        half.destination_blend = 6;
        let half = render(&mut draw, &harness, 0., &[half], Some(effect_light()));
        // 0.25098 of texture times material diffuse 1, then SrcAlpha over black
        // with the element's 0.5 opacity.
        assert!(
            (half[0] - 0.1255).abs() <= 0.01,
            "half-opacity model mismatch: {half:?}"
        );
    }

    /// `A3DGFXModel::ChangeModel` calls `A3DSkinModel::EnableSpecular(true)`, so
    /// an element-160 mesh carries the Blinn term: `c9` is the material specular
    /// colour bound against the direction light's specular colour, and the
    /// fragment stage adds it after the texture multiply. With the light and the
    /// material both white, a facet facing the half-vector saturates.
    #[test]
    fn gpu_effect_adds_the_model_blinn_term() {
        let Some(harness) = harness() else {
            eprintln!("effect specular test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let material = crate::gfx_player::MeshMaterial {
            ambient: [0.; 4],
            diffuse: [1.; 4],
            emissive: [0.; 4],
            specular: [1.; 4],
            power: 8.,
        };
        let mut light = effect_light();
        light.directional_specular = [1.; 4];
        // Direction and eye both along -Z: the half-vector is +Z, the facet's
        // normal, so the Blinn exponent is 1 and the coefficient is c9 itself.
        let lit = render(
            &mut draw,
            &harness,
            0.,
            &[mesh_quad(material, [0., 0., 1.])],
            Some(light),
        );
        assert!(
            lit[0] >= 0.99,
            "a white specular term must saturate: {lit:?}"
        );
        // The same material with no specular light keeps the diffuse-only value,
        // which is what the pass drew before this.
        let plain = render(
            &mut draw,
            &harness,
            0.,
            &[mesh_quad(material, [0., 0., 1.])],
            Some(effect_light()),
        );
        assert!(
            (plain[0] - 0.25).abs() <= 0.02,
            "diffuse-only mismatch: {plain:?}"
        );
    }

    #[test]
    fn gpu_effect_rejects_non_unorm_targets_and_reports_missing_textures() {
        let Some(harness) = harness() else {
            eprintln!("effect target test skipped: no WGPU adapter");
            return;
        };
        // `Draw` holds GPU resources and is not `Debug`, so the rejection is
        // matched rather than unwrapped.
        let error = match Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba16Float,
            None,
        ) {
            Ok(_) => panic!("a non-UNORM target must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("encoded UNORM"), "{error}");

        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let mut read = |_: &str| -> Result<Vec<u8>, String> { Err("absent".into()) };
        draw.update(
            &harness.device,
            &harness.queue,
            &[quad("missing.png", 5, 2, 0, false, [1.; 4])],
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            [0., 0., 0., 1.],
            [0., 0., 0., 100.],
            None,
            &mut read,
        )
        .unwrap();
        // A missing texture is reported, never fatal: the group survives on
        // the white stand-in (`self.fallback`, the NOTEX vertex-colour law),
        // so the quad keeps its place in the order and only its map is lost.
        // Deleting the bucket instead would drop the element from the frame.
        assert_eq!(draw.order.len(), 1);
        assert_eq!(draw.failures(), ["missing.png: absent"]);
        assert_eq!(draw.vertex_count(), 6);

        let error = draw
            .update(
                &harness.device,
                &harness.queue,
                &[],
                [[f32::NAN; 4]; 4],
                [0., 0., 0., 1.],
                [0., 0., 0., 100.],
                None,
                &mut read,
            )
            .unwrap_err();
        assert!(error.contains("non-finite"), "{error}");
    }

    /// A quad that carries per-corner colours must interpolate them across the
    /// triangle: the effect vertex attribute is the grid's authored `dwColor`
    /// lattice colour, not one flat tint for the whole cell.
    #[test]
    fn quad_vertex_colours_interpolate_across_the_cell() {
        let Some(harness) = harness() else {
            eprintln!("effect colour test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let mut quad = quad("grey.png", 5, 2, 0, false, [1.; 4]);
        // Left edge black, right edge white: the left-edge readback pixel is
        // dark only if the shader interpolates the four tints.
        quad.vertex_colors = Some([
            [0., 0., 0., 1.],
            [1., 1., 1., 1.],
            [0., 0., 0., 1.],
            [1., 1., 1., 1.],
        ]);
        let pixel = render(&mut draw, &harness, 0., &[quad], None);
        assert!(
            pixel[0] < 0.1,
            "the left-edge pixel must be near black through interpolation: {pixel:?}"
        );
    }

    /// The white fallback must render the same pixel as an explicit 1x1 white
    /// texture: the client's NOTEX placeholder IS a white sheet
    /// (`A3DGFXTexMan::LoadTextureFromFile` failure path binds one), so a
    /// bucket whose texture failed and a bucket bound to white cannot be told
    /// apart on screen. Only a rendered-pixel comparison proves that.
    #[test]
    fn a_failed_texture_renders_the_same_pixel_as_white() {
        let Some(harness) = harness() else {
            eprintln!("effect fallback test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let tint = [1., 0.5, 0.25, 1.];
        let missing = quad("missing.png", 5, 2, 0, false, tint);
        let mut fail = |_: &str| Err("no such texture".to_string());
        let fallback = render_with(
            &mut draw,
            &harness,
            0.,
            std::slice::from_ref(&missing),
            None,
            &mut fail,
        );
        let explicit = quad("white.png", 5, 2, 0, false, tint);
        let white = white_png();
        let mut found = move |_: &str| Ok(white.clone());
        let bound = render_with(
            &mut draw,
            &harness,
            0.,
            std::slice::from_ref(&explicit),
            None,
            &mut found,
        );
        for channel in 0..4 {
            assert!(
                (fallback[channel] - bound[channel]).abs() < 2. / 255.,
                "channel {channel}: failed {fallback:?} vs white {bound:?}"
            );
        }
        assert!(
            fallback[0] > 0.95 && fallback[1] > 0.45 && fallback[2] > 0.2,
            "the fallback renders the tint through white: {fallback:?}"
        );
    }

    /// A bucket that bound the placeholder while its texture was missing must
    /// rebind once the texture loads. The bucket cache is keyed by the frame
    /// group, which does not change, so without the explicit check the element
    /// draws the placeholder for the rest of the session (fail-then-succeed).
    #[test]
    fn a_late_texture_replaces_the_placeholder_binding() {
        let Some(harness) = harness() else {
            eprintln!("effect binding test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let frame = [quad("late.png", 5, 2, 0, false, [1.; 4])];
        let matrix = glam::Mat4::IDENTITY.to_cols_array_2d();
        let mut absent = |_: &str| -> Result<Vec<u8>, String> { Err("absent".into()) };
        draw.update(
            &harness.device,
            &harness.queue,
            &frame,
            matrix,
            [0., 0., 0., 1.],
            [0., 0., 0., 100.],
            None,
            &mut absent,
        )
        .unwrap();
        assert!(
            draw.buckets.values().next().unwrap().bound_fallback,
            "the first frame must bind the placeholder"
        );
        // The archive answers on the next frame: a 1x1 uncompressed TGA.
        let mut present = |_: &str| -> Result<Vec<u8>, String> {
            let mut bytes = vec![0; 18];
            bytes[2] = 2;
            bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
            bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
            bytes[16] = 32;
            bytes[17] = 8;
            bytes.extend_from_slice(&[3, 2, 1, 4]);
            Ok(bytes)
        };
        draw.update(
            &harness.device,
            &harness.queue,
            &frame,
            matrix,
            [0., 0., 0., 1.],
            [0., 0., 0., 100.],
            None,
            &mut present,
        )
        .unwrap();
        assert!(
            !draw.buckets.values().next().unwrap().bound_fallback,
            "a texture that loads after failing must replace the placeholder binding"
        );
    }

    #[test]
    fn gpu_effect_grows_buffers_when_a_burst_arrives() {
        let Some(harness) = harness() else {
            eprintln!("effect growth test skipped: no WGPU adapter");
            return;
        };
        let mut draw = Draw::new(
            &harness.device,
            &harness.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
        )
        .unwrap();
        let png = grey_png();
        let mut read = |_: &str| Ok(png.clone());
        let projection = glam::Mat4::IDENTITY.to_cols_array_2d();
        draw.update(
            &harness.device,
            &harness.queue,
            &[quad("a.png", 5, 2, 0, false, [1.; 4])],
            projection,
            [0.; 4],
            [0.; 4],
            None,
            &mut read,
        )
        .unwrap();
        let small = draw.buckets.values().next().unwrap().capacity;
        let burst: Vec<EffectQuad> = (0..200)
            .map(|_| quad("a.png", 5, 2, 0, false, [1.; 4]))
            .collect();
        draw.update(
            &harness.device,
            &harness.queue,
            &burst,
            projection,
            [0.; 4],
            [0.; 4],
            None,
            &mut read,
        )
        .unwrap();
        let large = draw.buckets.values().next().unwrap().capacity;
        assert!(large >= 200 * 6, "{small} -> {large}");
        assert_eq!(draw.vertex_count(), 200 * 6);
        // Emptying the frame keeps the group but draws nothing.
        draw.update(
            &harness.device,
            &harness.queue,
            &[],
            projection,
            [0.; 4],
            [0.; 4],
            None,
            &mut read,
        )
        .unwrap();
        assert_eq!(draw.order.len(), 0);
        assert_eq!(draw.vertex_count(), 0);
    }

    #[test]
    fn grouping_order_is_deterministic_without_a_device() {
        // The sort key is pure, so the draw order can be checked headless.
        let pipeline = |source, destination, depth_test| PipelineKey {
            source,
            destination,
            depth_test,
            write_depth: false,
        };
        let mut sorted = vec![
            BucketKey {
                layer: 1,
                priority: 0,
                pipeline: pipeline(5, 2, false),
                texture: "b".into(),
                tile: false,
            },
            BucketKey {
                layer: 0,
                priority: 0,
                pipeline: pipeline(5, 6, true),
                texture: "a".into(),
                tile: false,
            },
            BucketKey {
                layer: 0,
                priority: 0,
                pipeline: pipeline(5, 2, false),
                texture: "a".into(),
                tile: false,
            },
        ];
        sorted.sort_by(|a, b| {
            a.layer
                .cmp(&b.layer)
                // Depth writers establish the buffer the blended sheets are
                // tested against, so within a layer they go first whatever the
                // authored `Priority` says: a model drawn after the sheet it
                // stands in front of would otherwise be painted over.
                .then_with(|| b.pipeline.write_depth.cmp(&a.pipeline.write_depth))
                // `Priority` orders the elements inside one `RenderLayer`: the
                // client authors it 1..5 on every element (7,820 are a 4 and
                // 5,586 a 2) and nothing read it, so a background dust sheet
                // and the flash over it landed in whatever order the texture
                // bucket happened to produce.
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.pipeline.depth_test.cmp(&b.pipeline.depth_test).reverse())
                .then_with(|| a.pipeline.source.cmp(&b.pipeline.source))
                .then_with(|| a.pipeline.destination.cmp(&b.pipeline.destination))
                .then_with(|| a.texture.cmp(&b.texture))
                .then_with(|| a.tile.cmp(&b.tile))
        });
        assert!(sorted[0].pipeline.depth_test);
        assert_eq!(sorted[1].pipeline.destination, 2);
        assert_eq!(sorted[2].layer, 1);
        assert_eq!(blend_factor(5).unwrap(), wgpu::BlendFactor::SrcAlpha);
        assert_eq!(blend_factor(2).unwrap(), wgpu::BlendFactor::One);
        assert!(blend_factor(99).is_err());
    }

    /// `Priority` is authored 1..5 on every corpus element and nothing read it,
    /// so two elements in the same layer and blend state could draw in either
    /// order. It now orders them inside the layer, and only inside it.
    #[test]
    fn priority_orders_elements_within_a_layer() {
        let pipeline = |source, destination, depth_test| PipelineKey {
            source,
            destination,
            depth_test,
            write_depth: false,
        };
        let mut sorted = vec![
            BucketKey {
                layer: 0,
                priority: 4,
                pipeline: pipeline(5, 2, true),
                texture: "flash".into(),
                tile: false,
            },
            BucketKey {
                layer: 0,
                priority: 1,
                pipeline: pipeline(5, 2, true),
                texture: "dust".into(),
                tile: false,
            },
            BucketKey {
                layer: 1,
                priority: 1,
                pipeline: pipeline(5, 2, true),
                texture: "later-layer".into(),
                tile: false,
            },
        ];
        sorted.sort_by(|a, b| {
            a.layer
                .cmp(&b.layer)
                // Depth writers establish the buffer the blended sheets are
                // tested against, so within a layer they go first whatever the
                // authored `Priority` says: a model drawn after the sheet it
                // stands in front of would otherwise be painted over.
                .then_with(|| b.pipeline.write_depth.cmp(&a.pipeline.write_depth))
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.pipeline.depth_test.cmp(&b.pipeline.depth_test).reverse())
                .then_with(|| a.pipeline.source.cmp(&b.pipeline.source))
                .then_with(|| a.pipeline.destination.cmp(&b.pipeline.destination))
                .then_with(|| a.texture.cmp(&b.texture))
                .then_with(|| a.tile.cmp(&b.tile))
        });
        assert_eq!(
            sorted[0].texture, "dust",
            "priority 1 inside layer 0 draws before priority 4"
        );
        assert_eq!(sorted[1].texture, "flash");
        // Layer still dominates: a priority-1 element in layer 1 comes last.
        assert_eq!(sorted[2].texture, "later-layer");
    }

    /// A model slot is a skin draw, not a quad slot, so element 160's `WriteZ`
    /// decides whether it writes depth — and a writer has to draw before the
    /// blended sheets it should occlude, whatever `Priority` it carries. 115 of
    /// the shipped `gfx.pck` models author `WriteZ: 1`; the other 332 (131
    /// authored `0`, 201 older than the key) keep `A3DGFXModel`'s constructor
    /// value of false and write nothing.
    #[test]
    fn a_model_that_writes_depth_draws_before_the_sheets_in_its_layer() {
        let model = PipelineKey {
            source: 5,
            destination: 2,
            depth_test: true,
            write_depth: true,
        };
        let sheet = PipelineKey {
            source: 5,
            destination: 2,
            depth_test: true,
            write_depth: false,
        };
        let mut sorted = vec![
            BucketKey {
                layer: 0,
                priority: 1,
                pipeline: sheet,
                texture: "sheet".into(),
                tile: false,
            },
            BucketKey {
                layer: 0,
                priority: 4,
                pipeline: model,
                texture: "model".into(),
                tile: false,
            },
            BucketKey {
                layer: 1,
                priority: 1,
                pipeline: model,
                texture: "later-layer-model".into(),
                tile: false,
            },
        ];
        sorted.sort_by(|a, b| {
            a.layer
                .cmp(&b.layer)
                .then_with(|| b.pipeline.write_depth.cmp(&a.pipeline.write_depth))
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| a.pipeline.depth_test.cmp(&b.pipeline.depth_test).reverse())
                .then_with(|| a.pipeline.source.cmp(&b.pipeline.source))
                .then_with(|| a.pipeline.destination.cmp(&b.pipeline.destination))
                .then_with(|| a.texture.cmp(&b.texture))
                .then_with(|| a.tile.cmp(&b.tile))
        });
        assert_eq!(
            sorted[0].texture, "model",
            "the depth writer goes first even at priority 4"
        );
        assert_eq!(sorted[1].texture, "sheet");
        assert_eq!(sorted[2].texture, "later-layer-model");
        // A writer and a non-writer must not share a pipeline: the state differs.
        assert_ne!(sorted[0].pipeline, sorted[1].pipeline);
    }
}
