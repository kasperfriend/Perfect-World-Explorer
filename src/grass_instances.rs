//! GPU-instanced local MOXB grass. Placement sampling remains the viewer's
//! client-derived patch randomization; meshes are stored once per type/material.
#[path = "refraction_fog.rs"]
mod refraction_fog;
use crate::gpu::Vertex;
use crate::indirect_args::{record, DrawIndexedArgs};
use std::{collections::BTreeMap, ops::Range};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GrassVertex {
    pub vertex: Vertex,
    /// Per-material Y atlas extent; kept separate from `Vertex::color[3]`,
    /// which remains the grass alpha reference for diagnostics and the shader.
    pub scale_y: f32,
}
#[derive(Default)]
struct Group {
    vertices: Vec<GrassVertex>,
    indices: Vec<u32>,
    instances: Vec<[f32; 12]>,
}
#[derive(Default)]
pub struct Builder {
    groups: BTreeMap<(usize, usize), Group>,
}
pub struct Batch {
    indices: Range<u32>,
    instances: Range<u32>,
}
#[derive(Default)]
pub struct Data {
    vertices: Vec<GrassVertex>,
    indices: Vec<u32>,
    instances: Vec<[f32; 12]>,
    batches: Vec<Batch>,
}
impl Builder {
    pub fn contains(&self, key: (usize, usize)) -> bool {
        self.groups.contains_key(&key)
    }
    pub fn mesh(&mut self, key: (usize, usize), vertices: Vec<Vertex>, indices: Vec<u32>) {
        let scale_y = vertices.first().map_or(0., |v| v.color[2]);
        self.mesh_with_scale_y(key, vertices, indices, scale_y);
    }
    pub fn mesh_with_scale_y(
        &mut self,
        key: (usize, usize),
        vertices: Vec<Vertex>,
        indices: Vec<u32>,
        scale_y: f32,
    ) {
        let vertices = vertices
            .into_iter()
            .map(|vertex| GrassVertex { vertex, scale_y })
            .collect();
        self.groups.entry(key).or_insert(Group {
            vertices,
            indices,
            instances: Vec::new(),
        });
    }
    pub fn instance(&mut self, key: (usize, usize), position: [f32; 3], scale: f32) {
        self.instance_oriented(key, position, scale, 0., 0.);
    }
    pub fn instance_oriented(
        &mut self,
        key: (usize, usize),
        position: [f32; 3],
        scale: f32,
        yaw: f32,
        wind_index: f32,
    ) {
        self.instance_for_area(key, position, scale, yaw, wind_index, position);
    }
    pub fn instance_for_area(
        &mut self,
        key: (usize, usize),
        position: [f32; 3],
        scale: f32,
        yaw: f32,
        wind_index: f32,
        area_center: [f32; 3],
    ) {
        // `wind_index` is the placement's history slot (`0x653e93`), which the
        // retail vertex shader consumes as `v1.x`.
        if let Some(g) = self.groups.get_mut(&key) {
            let (sn, cs) = yaw.sin_cos();
            g.instances.push([
                position[0],
                position[1],
                position[2],
                scale,
                cs,
                sn,
                wind_index,
                0.,
                area_center[0],
                area_center[2],
                0.,
                0.,
            ]);
        }
    }
    pub fn finish(self) -> Data {
        let mut data = Data::default();
        data.batches.reserve(self.groups.len());
        data.instances
            .reserve(self.groups.values().map(|g| g.instances.len()).sum());
        for (_, g) in self.groups {
            if g.instances.is_empty() {
                continue;
            }
            let base = data.vertices.len() as u32;
            let i = data.indices.len() as u32;
            let n = data.instances.len() as u32;
            data.vertices.extend(g.vertices);
            data.indices.extend(g.indices.into_iter().map(|v| v + base));
            data.instances.extend(g.instances);
            data.batches.push(Batch {
                indices: i..data.indices.len() as u32,
                instances: n..data.instances.len() as u32,
            });
        }
        data
    }
}
impl Data {
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.vertices.len(),
            self.instances.len(),
            self.batches.len(),
        )
    }
}
#[cfg(test)]
mod scale_y_tests {
    use super::*;

    #[test]
    fn rectangular_extent_does_not_repurpose_alpha_reference() {
        let vertex = Vertex {
            pos: [0.; 3],
            normal: [0., 1., 0.],
            uv: [0., 0.],
            color: [0., 0., 0.25, 84. / 255.],
            kind: 3.75,
            diffuse: 0,
            night_diffuse: 0,
        };
        let mut builder = Builder::default();
        builder.mesh_with_scale_y((0, 0), vec![vertex], vec![], 0.125);
        builder.instance((0, 0), [0.; 3], 1.);
        let data = builder.finish();
        assert_eq!(data.vertices[0].scale_y, 0.125);
        assert_eq!(data.vertices[0].vertex.color[2], 0.25);
        assert_eq!(data.vertices[0].vertex.color[3], 84. / 255.);
    }
}
pub struct Gpu {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    vertex_capacity: u64,
    indices: wgpu::Buffer,
    index_capacity: u64,
    instances: wgpu::Buffer,
    instance_capacity: u64,
    batches: Vec<Batch>,
    /// One indirect record per batch, so the whole grass field is one call.
    args: wgpu::Buffer,
    args_capacity: u64,
    /// Set when the adapter has `MULTI_DRAW_INDIRECT` + `INDIRECT_FIRST_INSTANCE`.
    multi_draw: bool,
}
impl Gpu {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        layout: &wgpu::BindGroupLayout,
        shader: &str,
        data: Data,
        light_layout: Option<&wgpu::BindGroupLayout>,
    ) -> Self {
        let marker = "@vertex fn vs(i:VIn)->VOut { var o:VOut; var pos=i.pos;";
        assert!(
            shader.contains(marker),
            "world shader changed; grass transform must be updated"
        );
        let mut source=shader.replacen(marker,"@vertex fn vs(i:VIn, @location(6) instance:vec4<f32>, @location(8) extra:vec4<f32>, @location(9) area_center:vec2<f32>)->VOut { var o:VOut; var local=vec3<f32>(i.pos.x*extra.x+i.pos.z*extra.y,i.pos.y,-i.pos.x*extra.y+i.pos.z*extra.x); var pos=local*instance.w+instance.xyz;",1);
        let marker = "@location(7) night_diffuse:u32 };";
        assert!(source.contains(marker));
        source = source.replacen(
            marker,
            "@location(7) night_diffuse:u32, @location(10) grass_scale_y:f32 };",
            1,
        );
        let marker = "var norm=i.normal;";
        assert!(source.contains(marker));
        source = source.replacen(marker, "var norm=vec3<f32>(i.normal.x*extra.x+i.normal.z*extra.y,i.normal.y,-i.normal.x*extra.y+i.normal.z*extra.x);", 1);
        // The retail grass VS computes alpha per original world-space vertex,
        // and its PS multiplies this into the texture alpha before D3D alpha test.
        // i.diffuse stores the authored per-type range as f32 bits for this pass.
        let marker = "@location(5) diffuse:vec3<f32>, @location(6) opacity:f32 };";
        assert!(source.contains(marker));
        source = source.replacen(
            marker,
            "@location(5) diffuse:vec3<f32>, @location(6) opacity:f32, @location(7) grass_fade:f32, @location(8) @interpolate(flat) grass_area:vec2<f32>, @location(10) grass_refraction:f32, @location(11) @interpolate(flat) grass_scale_y:f32 };",
            1,
        );
        let marker = "o.pos=camera.matrix*vec4<f32>(pos,1.);";
        assert!(source.contains(marker));
        // The retail `grass_wave` vertex shader, verbatim from
        // `shaders/vs/grass_wave.txt` in the client's shaders.pck:
        //
        //     mov a0.x, v1.x
        //     mul r1, v1.y, c[10+a0.x]
        //     add r0, v0, r1
        //
        // so the slot is a per-vertex attribute (`v1.x`) and the applied vector
        // is scaled by `v1.y`. The emitter computes `ftol(2*|y-10000| +
        // rand(0,10)) % 80` once per placement (`0x653e93`-`0x653ec1`) from the
        // placement's own height, so the instance carries that slot and the
        // mesh vertex keeps its weight.
        source = source.replacen(marker,"o.grass_area=area_center; o.grass_scale_y=i.grass_scale_y; o.grass_fade=1.; let original=pos; let sight=bitcast<f32>(i.diffuse); if camera.grass_eye.w>0.5 && sight>0. && sight<10000. {let delta=pos-camera.grass_eye.xyz; o.grass_fade=max(1.-dot(delta,delta)/(sight*sight),0.);} let weight=bitcast<f32>(i.night_diffuse); if camera.grass_wind.y>0.5 && weight>=0. && weight<100. {let slot=i32(round(extra.z))%80; let wrapped=select(slot,slot+80,slot<0); let history_index=u32(wrapped); pos+=camera.grass_history[history_index].xyz*weight;} o.grass_refraction=1.; if camera.clip.y<0. {o.grass_refraction=grass_refraction_transmittance(original,pos,camera.grass_eye.xyz,camera.clip.x);} o.pos=camera.matrix*vec4<f32>(pos,1.);",1);
        let marker = "tex.a<select(0.5,i.color.w,i.kind>3.5 && i.kind<3.9)";
        assert!(source.contains(marker));
        source = source.replacen(
            marker,
            "tex.a*i.grass_fade<=select(0.5,i.color.w,i.kind>3.5 && i.kind<3.9)",
            1,
        );
        let marker = "let scale=vec2<f32>(i.color.z,select(i.color.z,i.color.w,i.kind<2.5));";
        assert!(source.contains(marker));
        source = source.replacen(
            marker,
            "let scale=vec2<f32>(i.color.z,select(select(i.color.z,i.color.w,i.kind<2.5),i.grass_scale_y,i.kind>3.5 && i.kind<3.9));",
            1,
        );
        // Retail 0x652e84: without a complete day/night pair, stage 0
        // SELECTARG1 keeps the texture RGB and stage 1 is disabled. It is not
        // an invitation to add a provisional directional-light multiplier.
        let fallback="let light=camera.light.x+camera.light.y*max(dot(normalize(i.normal),normalize(camera.sun.xyz)),0.);return vec4<f32>(fogged(tex.rgb*light*camera.light.zww,i.pos.w),1.);";
        assert!(source.contains(fallback));
        source = source.replacen(fallback, "return vec4<f32>(fogged(tex.rgb,i.pos.w),1.);", 1);
        if light_layout.is_some() {
            // Patch the scene fragment's final return so the lightmapped branch
            // samples the authored day/night pair; without this the grass keeps
            // the unlit texture and GRASS_LIGHT_WGSL is dead code.
            let marker = "return vec4<f32>(fogged(tex.rgb,i.pos.w),1.);";
            assert!(source.contains(marker));
            source = source.replacen(
                marker,
                "if camera.sun.w>=0. {return vec4<f32>(grass_pass_fog(grass_lighting(i.world,i.grass_area,raw_tex.rgb),i.pos.w,i.grass_refraction),1.);} return vec4<f32>(fogged(tex.rgb,i.pos.w),1.);",
                1,
            );
            source.push_str(GRASS_LIGHT_WGSL);
        }
        // Ordinary grass retains table fog. Only the water-plane-clipped
        // refraction pass interpolates the recovered exponential vertex fog.
        source = source.replace(
            "fogged(tex.rgb,i.pos.w)",
            "grass_pass_fog(tex.rgb,i.pos.w,i.grass_refraction)",
        );
        source.push_str(refraction_fog::WGSL);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("instanced client grass shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let mut layouts = vec![layout];
        if let Some(l) = light_layout {
            layouts.push(l);
        }
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("instanced grass layout"),
            bind_group_layouts: &layouts,
            push_constant_ranges: &[],
        });
        const ATTR: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32,5=>Uint32,7=>Uint32,10=>Float32];
        const INSTANCE: [wgpu::VertexAttribute; 3] =
            wgpu::vertex_attr_array![6=>Float32x4,8=>Float32x4,9=>Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("instanced grass pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs",
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GrassVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &ATTR,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: 48,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE,
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        let (vertices, indices, instances, args, batches) = Self::buffers(device, data);
        Self {
            pipeline,
            vertex_capacity: vertices.size(),
            vertices,
            index_capacity: indices.size(),
            indices,
            instance_capacity: instances.size(),
            instances,
            batches,
            args_capacity: args.size(),
            args,
            multi_draw: crate::indirect_args::multi_draw(device)
                && crate::indirect_args::base_instance(device),
        }
    }
    /// Choose the single-call path explicitly, so a check can render both.
    ///
    /// It only flips which of the two draw paths runs: the records already
    /// carry each batch's `first_instance`, and the fallback draws the same
    /// ranges directly, so the picture cannot depend on the choice.
    pub fn set_multi_draw(&mut self, multi_draw: bool) {
        self.multi_draw = multi_draw;
    }
    fn buffers(
        device: &wgpu::Device,
        data: Data,
    ) -> (
        wgpu::Buffer,
        wgpu::Buffer,
        wgpu::Buffer,
        wgpu::Buffer,
        Vec<Batch>,
    ) {
        let data = Self::padded(data);
        let buffer = |label, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        // `COPY_DST` from the start: `update` rewrites these buffers in place
        // whenever the field fits, which is what stops a block crossing from
        // destroying and recreating them.
        (
            buffer(
                "grass mesh vertices",
                bytemuck::cast_slice(&data.vertices),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            ),
            buffer(
                "grass mesh indices",
                bytemuck::cast_slice(&data.indices),
                wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            ),
            buffer(
                "grass placement instances",
                bytemuck::cast_slice(&data.instances),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            ),
            Self::args_buffer(device, &data.batches),
            data.batches,
        )
    }
    /// One indirect record per batch, in the order `draw` submits them.
    fn args_data(batches: &[Batch]) -> Vec<DrawIndexedArgs> {
        let mut args: Vec<DrawIndexedArgs> = batches
            .iter()
            .map(|batch| record(batch.indices.clone(), batch.instances.clone()))
            .collect();
        if args.is_empty() {
            args.push(record(0..0, 0..0));
        }
        args
    }
    /// One record per batch; the ranges never change until the next update, so
    /// this is rebuilt with the rest of the grass buffers and never per frame.
    fn args_buffer(device: &wgpu::Device, batches: &[Batch]) -> wgpu::Buffer {
        let args = Self::args_data(batches);
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grass draw args"),
            contents: bytemuck::cast_slice(&args),
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
        })
    }
    /// Replace the grass field with a rebuilt one.
    ///
    /// Every block-window update rebuilds the whole field, but its four buffers
    /// barely change size, so they are written through `queue.write_buffer` and
    /// only reallocated when the field grows past its capacity. Recreating them
    /// meant a fresh allocation and a full copy of every placement — the largest
    /// buffer in the streaming path — for a crossing of one block.
    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: Data) {
        let data = Self::padded(data);
        self.batches = data.batches;
        Self::upload(
            device,
            queue,
            &mut self.vertices,
            &mut self.vertex_capacity,
            "grass mesh vertices",
            bytemuck::cast_slice(&data.vertices),
            wgpu::BufferUsages::VERTEX,
        );
        Self::upload(
            device,
            queue,
            &mut self.indices,
            &mut self.index_capacity,
            "grass mesh indices",
            bytemuck::cast_slice(&data.indices),
            wgpu::BufferUsages::INDEX,
        );
        Self::upload(
            device,
            queue,
            &mut self.instances,
            &mut self.instance_capacity,
            "grass placement instances",
            bytemuck::cast_slice(&data.instances),
            wgpu::BufferUsages::VERTEX,
        );
        let args = Self::args_data(&self.batches);
        Self::upload(
            device,
            queue,
            &mut self.args,
            &mut self.args_capacity,
            "grass draw args",
            bytemuck::cast_slice(&args),
            wgpu::BufferUsages::INDIRECT,
        );
    }
    /// Write one buffer, growing it to the next power of two only if it no
    /// longer holds the data.
    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        buffer: &mut wgpu::Buffer,
        capacity: &mut u64,
        label: &str,
        contents: &[u8],
        usage: wgpu::BufferUsages,
    ) {
        let needed = (contents.len() as u64).next_power_of_two().max(4);
        if *capacity < needed {
            *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: needed,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            *capacity = needed;
        }
        if !contents.is_empty() {
            queue.write_buffer(buffer, 0, contents);
        }
    }
    /// Pad an empty field the way the draw path expects: one degenerate mesh
    /// vertex, one index and one instance, so a map without grass still binds
    /// valid buffers.
    fn padded(mut data: Data) -> Data {
        if data.vertices.is_empty() {
            data.vertices.push(bytemuck::Zeroable::zeroed());
        }
        if data.indices.is_empty() {
            data.indices.push(0);
        }
        if data.instances.is_empty() {
            data.instances.push([0.; 12]);
        }
        data
    }
    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        bind: &'a wgpu::BindGroup,
        light: Option<&'a wgpu::BindGroup>,
    ) {
        if self.batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        if let Some(l) = light {
            pass.set_bind_group(1, l, &[]);
        }
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        // Every batch shares these three buffers, so the whole field is one
        // call when the adapter can read `first_instance` from the records.
        if self.multi_draw && !self.batches.is_empty() {
            pass.multi_draw_indexed_indirect(&self.args, 0, self.batches.len() as u32);
        } else {
            for b in &self.batches {
                pass.draw_indexed(b.indices.clone(), 0, b.instances.clone());
            }
        }
    }
}
/// Day/night lightmap sampling for the grass shader (bind group 1).
/// Hoisted so the availability gate can be asserted without a device: an
/// area without both authored maps must use the unlit texture, never the
/// terrain fallback copy or a directional-light product.
const GRASS_LIGHT_WGSL: &str = r#"
struct GrassLightSettings { params:vec4<f32>, areas:vec4<u32>, light:vec4<u32> };
@group(1) @binding(0) var<uniform> grass_settings:GrassLightSettings;
@group(1) @binding(1) var grass_lightmaps:texture_2d_array<f32>;
@group(1) @binding(2) var grass_light_sampler:sampler;
@group(1) @binding(3) var<storage,read> grass_light_availability:array<u32>;
fn grass_linear(c:vec3<f32>)->vec3<f32>{return select(c/12.92,pow((max(c,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),c>vec3<f32>(0.04045));}
fn grass_lighting(world:vec3<f32>, area:vec2<f32>, base:vec3<f32>)->vec3<f32>{
 let s=grass_settings;
 let gx=clamp(world.x/s.params.z+s.params.x*0.5,0.,s.params.x-1.);
 let gy=clamp(s.params.y*0.5-world.z/s.params.z,0.,s.params.y-1.);
 let coord=vec2<f32>(gx,gy)/s.params.w;
 let area_gx=clamp(area.x/s.params.z+s.params.x*0.5,0.,s.params.x-1.);
 let area_gy=clamp(s.params.y*0.5-area.y/s.params.z,0.,s.params.y-1.);
 let selected=select(floor(coord),floor(vec2<f32>(area_gx,area_gy)/s.params.w),camera.grass_wind.z>0.5);
 let ax=u32(selected.x);let ay=u32(selected.y);
 let id=ay*s.areas.x+ax;
 // Terrain may copy day into a missing night slot. Grass instead uses its
 // unlit texture whenever EITHER authored map is unavailable.
 if (grass_light_availability[id]&3u)!=3u {return grass_linear(clamp(base,vec3<f32>(0.),vec3<f32>(1.)));}
 let slot=id%s.light.z;let page=id/s.light.z;
 let side=f32(s.light.x);let xy=vec2<f32>(f32(slot%s.light.y),f32(slot/s.light.y))*side;
 let loc=clamp((coord-selected)*side,vec2<f32>(0.5),vec2<f32>(side-0.5));
 let lmuv=(xy+loc)/vec2<f32>(textureDimensions(grass_lightmaps));
 let day=textureSampleLevel(grass_lightmaps,grass_light_sampler,lmuv,i32(page),0.).rgb;
 let night=textureSampleLevel(grass_lightmaps,grass_light_sampler,lmuv,i32(page+s.light.w),0.).rgb;
 return grass_linear(clamp(base*mix(night,day,clamp(camera.sun.w,0.,1.))*2.,vec3<f32>(0.),vec3<f32>(1.)));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_pairs_use_the_unlit_texture_and_never_the_region_light() {
        let w = super::GRASS_LIGHT_WGSL;
        assert!(w.contains(
            "if (grass_light_availability[id]&3u)!=3u {return grass_linear(clamp(base,vec3<f32>(0.),vec3<f32>(1.)));}"
        ));
        let fallback = w.split("!=3u {").nth(1).unwrap().split('}').next().unwrap();
        assert!(fallback.contains("base"));
        assert!(!fallback.contains("camera.light"));
        assert!(
            !w.contains("camera.region_"),
            "grass keeps the lightmap path"
        );
        assert!(w.contains("mix(night,day,clamp(camera.sun.w,0.,1.))*2."));
        // The declarations are inert unless the assembled shader actually calls
        // `grass_lighting`; assert the source keeps that injection.
        let source = include_str!("grass_instances.rs");
        assert!(source.contains(
            "if camera.sun.w>=0. {return vec4<f32>(grass_pass_fog(grass_lighting(i.world,i.grass_area,raw_tex.rgb),i.pos.w,i.grass_refraction),1.);}"
        ));
    }

    /// `shaders/vs/grass_wave.txt` (client shaders.pck) is
    /// `mov a0.x, v1.x` / `mul r1, v1.y, c[10+a0.x]` / `add r0, v0, r1`: the
    /// slot is per placement and only the scale is per vertex, so the injected
    /// vertex stage must read the instance slot once and multiply by the vertex
    /// weight, not hash the vertex height again.
    #[test]
    fn wind_stage_matches_the_retail_grass_wave_shader() {
        // `include_str!` also captures the tests, so cut the file at the first
        // test module before looking for strings the tests have to spell out.
        // Main added a second test module (`scale_y_tests`) *above* the shader,
        // so cutting at the first `#[cfg(test)]` threw the shader away with it.
        // Only this module has to go, and it is the last thing in the file.
        let source = include_str!("grass_instances.rs");
        let injected = &source[..source.find("mod tests").expect("this test module")];
        assert!(injected.contains("let slot=i32(round(extra.z))%80;"));
        assert!(injected.contains("let weight=bitcast<f32>(i.night_diffuse);"));
        assert!(injected.contains("pos+=camera.grass_history[history_index].xyz*weight;"));
        // The frozen per-vertex hash must be gone: the native emitter computes
        // the slot on the CPU and passes it as `v1.x`.
        assert!(
            !injected.contains("pos.y-10000."),
            "the vertex stage must not re-hash the vertex height"
        );
    }
    #[test]
    fn injected_vertex_stage_is_valid_wgsl() {
        // WGSL has no ternary operator. A `cond ? a : b` slipped into this
        // injection once and only failed at runtime, when wgpu parsed the
        // shader on a real adapter ("expected ')', found '?'"), because the
        // GPU checks were not actually gating. Keep the whole injected line
        // free of `?`, and keep the slot wrap in `select`, which is the WGSL
        // spelling of the same arithmetic.
        let source = include_str!("grass_instances.rs");
        let injected = &source[..source.find("mod tests").expect("this test module")];
        let line = injected
            .lines()
            .find(|l| l.contains("o.grass_area=area_center;"))
            .expect("the injected vertex stage");
        assert!(
            !line.contains('?'),
            "WGSL has no ternary operator; use select(): {line}"
        );
        assert!(line.contains("select(slot,slot+80,slot<0)"));
    }
    #[test]
    fn area_center_is_preserved_per_instance() {
        let mut builder = Builder::default();
        builder.mesh((0, 0), Vec::new(), Vec::new());
        builder.instance_for_area((0, 0), [12., 5., 22.], 1., 0., 0., [100., 10., -50.]);
        builder.instance_oriented((0, 0), [3., 4., 5.], 1., 0., 0.);
        let d = builder.finish();
        assert_eq!(&d.instances[0][8..10], &[100., -50.]);
        assert_eq!(&d.instances[1][8..10], &[3., 5.]);
    }
    #[test]
    fn stores_mesh_once_for_many_instances() {
        let mut b = Builder::default();
        let v = Vertex {
            pos: [1., 2., 3.],
            normal: [0., 1., 0.],
            uv: [0., 0.],
            color: [1.; 4],
            kind: 3.75,
            diffuse: 0,
            night_diffuse: 0xffff_ffff,
        };
        b.mesh((2, 0), vec![v, v, v], vec![0, 1, 2]);
        for i in 0..1000 {
            b.instance((2, 0), [i as f32, 0., 0.], 2.);
        }
        let d = b.finish();
        assert_eq!(d.counts(), (3, 1000, 1));
        assert_eq!(d.batches[0].instances, 0..1000);
    }
}
