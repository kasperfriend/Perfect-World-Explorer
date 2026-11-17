//! Offline stand-in for the `wgpu` 0.19 surface the Imaginator's composite
//! uses. Signatures and shapes mirror the real crate; nothing here touches a
//! GPU. Harness for `tools/imaginator-check.sh`, not shipped.
#![allow(dead_code, unused_variables, non_snake_case)]

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextureFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgba16Float,
    Rgba32Float,
    Depth32Float,
}
impl TextureFormat {
    pub fn is_srgb(self) -> bool {
        matches!(self, TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8UnormSrgb)
    }
    pub fn remove_srgb_suffix(self) -> Self {
        match self {
            TextureFormat::Rgba8UnormSrgb => TextureFormat::Rgba8Unorm,
            TextureFormat::Bgra8UnormSrgb => TextureFormat::Bgra8Unorm,
            other => other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureDimension {
    D1,
    D2,
    D3,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureViewDimension {
    D1,
    D2,
    D2Array,
    Cube,
    CubeArray,
    D3,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureAspect {
    All,
    StencilOnly,
    DepthOnly,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StorageTextureAccess {
    WriteOnly,
    ReadOnly,
    ReadWrite,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureSampleType {
    Float { filterable: bool },
    Depth,
    Sint,
    Uint,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferBindingType {
    Uniform,
    Storage { read_only: bool },
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SamplerBindingType {
    Filtering,
    NonFiltering,
    Comparison,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterMode {
    Nearest,
    Linear,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddressMode {
    ClampToEdge,
    Repeat,
    MirrorRepeat,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapMode {
    Read,
    Write,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadOp<T> {
    Load,
    Clear(T),
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreOp {
    Store,
    Discard,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreOpDiscard {
    Discard,
}

macro_rules! flags {
    ($name:ident { $($flag:ident),* $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
        pub struct $name(u32);
        impl $name {
            $(pub const $flag: $name = $name(1 << flags!(@idx $name $flag));)*
        }
        impl std::ops::BitOr for $name {
            type Output = $name;
            fn bitor(self, other: $name) -> $name {
                $name(self.0 | other.0)
            }
        }
        impl std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, other: $name) {
                self.0 |= other.0;
            }
        }
    };
    (@idx $name:ident $flag:ident) => { 0 };
}

flags!(TextureUsages {
    COPY_SRC, COPY_DST, TEXTURE_BINDING, STORAGE_BINDING, RENDER_ATTACHMENT
});
flags!(BufferUsages {
    COPY_SRC, COPY_DST, UNIFORM, STORAGE, MAP_READ, MAP_WRITE, INDEX, VERTEX
});
flags!(ShaderStages { VERTEX, FRAGMENT, COMPUTE, VERTEX_FRAGMENT });
flags!(ColorWrites { RED, GREEN, BLUE, ALPHA, ALL });

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}
impl Color {
    pub const BLACK: Color = Color {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 1.,
    };
    pub const TRANSPARENT: Color = Color {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 0.,
    };
}

#[derive(Clone, Copy, Debug)]
pub struct Extent3d {
    pub width: u32,
    pub height: u32,
    pub depth_or_array_layers: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Origin3d {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}
impl Origin3d {
    pub const ZERO: Origin3d = Origin3d { x: 0, y: 0, z: 0 };
}

pub struct Device;
pub struct Queue;
pub struct Texture;
impl Texture {
    pub fn width(&self) -> u32 {
        0
    }
    pub fn height(&self) -> u32 {
        0
    }
    pub fn create_view(&self, _desc: &TextureViewDescriptor) -> TextureView {
        TextureView
    }
}
pub struct TextureView;
pub struct Sampler;
pub struct Buffer;
pub struct ShaderModule;
pub struct BindGroup;
pub struct BindGroupLayout;
pub struct PipelineLayout;
pub struct ComputePipeline;
pub struct RenderPipeline;

pub struct TextureViewDescriptor<'a> {
    pub label: Option<&'a str>,
    pub format: Option<TextureFormat>,
    pub dimension: Option<TextureViewDimension>,
    // No `usage`: 0.19.4 has no such field on a view descriptor.
    pub aspect: TextureAspect,
    pub base_mip_level: u32,
    pub mip_level_count: Option<u32>,
    pub base_array_layer: u32,
    pub array_layer_count: Option<u32>,
}
impl Default for TextureViewDescriptor<'_> {
    fn default() -> Self {
        TextureViewDescriptor {
            label: None,
            format: None,
            dimension: None,
            aspect: TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: None,
        }
    }
}

pub struct TextureDescriptor<'a> {
    pub label: Option<&'a str>,
    pub size: Extent3d,
    pub mip_level_count: u32,
    pub sample_count: u32,
    pub dimension: TextureDimension,
    pub format: TextureFormat,
    pub usage: TextureUsages,
    pub view_formats: &'a [TextureFormat],
}

pub struct SamplerDescriptor<'a> {
    pub label: Option<&'a str>,
    pub address_mode_u: AddressMode,
    pub address_mode_v: AddressMode,
    pub address_mode_w: AddressMode,
    pub mag_filter: FilterMode,
    pub min_filter: FilterMode,
    pub mipmap_filter: FilterMode,
    pub lod_min_clamp: f32,
    pub lod_max_clamp: f32,
    pub compare: Option<()>,
    pub anisotropy_clamp: u16,
    pub border_color: Option<()>,
}
impl Default for SamplerDescriptor<'_> {
    fn default() -> Self {
        SamplerDescriptor {
            label: None,
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            mipmap_filter: FilterMode::Nearest,
            lod_min_clamp: 0.,
            lod_max_clamp: 32.,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
        }
    }
}

pub struct BufferDescriptor<'a> {
    pub label: Option<&'a str>,
    pub size: u64,
    pub usage: BufferUsages,
    pub mapped_at_creation: bool,
}

pub struct BindGroupLayoutEntry {
    pub binding: u32,
    pub visibility: ShaderStages,
    pub ty: BindingType,
    pub count: Option<u32>,
}

pub enum BindingType {
    Buffer {
        ty: BufferBindingType,
        has_dynamic_offset: bool,
        min_binding_size: Option<()>,
    },
    Sampler(SamplerBindingType),
    Texture {
        sample_type: TextureSampleType,
        view_dimension: TextureViewDimension,
        multisampled: bool,
    },
    StorageTexture {
        access: StorageTextureAccess,
        format: TextureFormat,
        view_dimension: TextureViewDimension,
    },
}

pub struct BindGroupLayoutDescriptor<'a> {
    pub label: Option<&'a str>,
    pub entries: &'a [BindGroupLayoutEntry],
}

pub enum BindingResource<'a> {
    BufferBinding(BufferBinding<'a>),
    Sampler(&'a Sampler),
    TextureView(&'a TextureView),
}

pub struct BufferBinding<'a> {
    pub buffer: &'a Buffer,
    pub offset: u64,
    pub size: Option<u64>,
}
impl Buffer {
    pub fn as_entire_binding(&self) -> BindingResource<'_> {
        BindingResource::BufferBinding(BufferBinding {
            buffer: self,
            offset: 0,
            size: None,
        })
    }
    pub fn slice(&self, _range: impl std::ops::RangeBounds<u64>) -> BufferSlice<'_> {
        BufferSlice(std::marker::PhantomData)
    }
}
pub struct BufferSlice<'a>(std::marker::PhantomData<&'a ()>);
impl BufferSlice<'_> {
    pub fn map_async(&self, _mode: MapMode, _callback: impl FnOnce(Result<(), ()>) + 'static) {}
    pub fn get_mapped_range(&self) -> Vec<u8> {
        Vec::new()
    }
}
impl Buffer {
    pub fn unmap(&self) {}
}

pub struct BindGroupEntry<'a> {
    pub binding: u32,
    pub resource: BindingResource<'a>,
}
pub struct BindGroupDescriptor<'a> {
    pub label: Option<&'a str>,
    pub layout: &'a BindGroupLayout,
    pub entries: &'a [BindGroupEntry<'a>],
}

pub struct ShaderModuleDescriptor<'a> {
    pub label: Option<&'a str>,
    pub source: ShaderSource<'a>,
}
pub enum ShaderSource<'a> {
    Wgsl(std::borrow::Cow<'a, str>),
}

pub struct PipelineLayoutDescriptor<'a> {
    pub label: Option<&'a str>,
    pub bind_group_layouts: &'a [&'a BindGroupLayout],
    pub push_constant_ranges: &'a [()],
}

pub struct ComputePipelineDescriptor<'a> {
    pub label: Option<&'a str>,
    pub layout: Option<&'a PipelineLayout>,
    pub module: &'a ShaderModule,
    pub entry_point: &'a str,
}

// wgpu 0.19.4's shape: no `compilation_options` (added in 22). The shim had it,
// which is why `composite.rs` compiled offline and failed `cargo check --bins`.
pub struct VertexState<'a> {
    pub module: &'a ShaderModule,
    pub entry_point: &'a str,
    pub buffers: &'a [()],
}
pub struct FragmentState<'a> {
    pub module: &'a ShaderModule,
    pub entry_point: &'a str,
    pub targets: &'a [Option<ColorTargetState>],
}
pub struct ColorTargetState {
    pub format: TextureFormat,
    pub blend: Option<()>,
    pub write_mask: ColorWrites,
}
pub struct PrimitiveState;
impl Default for PrimitiveState {
    fn default() -> Self {
        PrimitiveState
    }
}
pub struct MultisampleState;
impl Default for MultisampleState {
    fn default() -> Self {
        MultisampleState
    }
}
pub struct RenderPipelineDescriptor<'a> {
    pub label: Option<&'a str>,
    pub layout: Option<&'a PipelineLayout>,
    pub vertex: VertexState<'a>,
    pub primitive: PrimitiveState,
    pub depth_stencil: Option<()>,
    pub multisample: MultisampleState,
    pub fragment: Option<FragmentState<'a>>,
    pub multiview: Option<&'a [u32]>,
}

pub struct Operations<T> {
    pub load: LoadOp<T>,
    pub store: StoreOp,
}
pub struct RenderPassColorAttachment<'a> {
    pub view: &'a TextureView,
    pub resolve_target: Option<&'a TextureView>,
    pub ops: Operations<Color>,
}
pub struct RenderPassDescriptor<'a> {
    pub label: Option<&'a str>,
    pub color_attachments: &'a [Option<RenderPassColorAttachment<'a>>],
    pub depth_stencil_attachment: Option<()>,
    pub timestamp_writes: Option<()>,
    pub occlusion_query_set: Option<&'a ()>,
}

pub struct CommandEncoder;
pub struct CommandEncoderDescriptor<'a> {
    pub label: Option<&'a str>,
}
impl Default for CommandEncoderDescriptor<'_> {
    fn default() -> Self {
        CommandEncoderDescriptor { label: None }
    }
}
impl CommandEncoder {
    pub fn begin_compute_pass(&mut self, _desc: &ComputePassDescriptor<'_>) -> ComputePass<'_> {
        ComputePass(std::marker::PhantomData)
    }
    pub fn begin_render_pass(&mut self, _desc: &RenderPassDescriptor<'_>) -> RenderPass<'_> {
        RenderPass(std::marker::PhantomData)
    }
    pub fn copy_texture_to_buffer(
        &mut self,
        _src: ImageCopyTexture<'_>,
        _dst: ImageCopyBuffer<'_>,
        _size: Extent3d,
    ) {
    }
    pub fn finish(self) -> CommandBuffer {
        CommandBuffer
    }
}
pub struct CommandBuffer;
pub struct ComputePass<'a>(std::marker::PhantomData<&'a ()>);
impl ComputePass<'_> {
    pub fn set_pipeline(&mut self, _p: &ComputePipeline) {}
    pub fn set_bind_group(&mut self, _index: u32, _group: &BindGroup, _offsets: &[u32]) {}
    pub fn dispatch_workgroups(&mut self, _x: u32, _y: u32, _z: u32) {}
}
pub struct RenderPass<'a>(std::marker::PhantomData<&'a ()>);
impl RenderPass<'_> {
    pub fn set_pipeline(&mut self, _p: &RenderPipeline) {}
    pub fn set_bind_group(&mut self, _index: u32, _group: &BindGroup, _offsets: &[u32]) {}
    pub fn draw(&mut self, _vertices: Range<u32>, _instances: Range<u32>) {}
}

pub struct ComputePassDescriptor<'a> {
    pub label: Option<&'a str>,
    pub timestamp_writes: Option<()>,
}

pub struct ImageCopyTexture<'a> {
    pub texture: &'a Texture,
    pub mip_level: u32,
    pub origin: Origin3d,
    pub aspect: TextureAspect,
}
pub struct ImageCopyBuffer<'a> {
    pub buffer: &'a Buffer,
    pub layout: ImageDataLayout,
}
pub struct ImageDataLayout {
    pub offset: u64,
    pub bytes_per_row: Option<u32>,
    pub rows_per_image: Option<u32>,
}

pub enum Maintain {
    Poll,
    Wait,
}

impl Device {
    pub fn create_bind_group_layout(&self, _d: &BindGroupLayoutDescriptor<'_>) -> BindGroupLayout {
        BindGroupLayout
    }
    pub fn create_bind_group(&self, _d: &BindGroupDescriptor<'_>) -> BindGroup {
        BindGroup
    }
    pub fn create_shader_module(&self, _d: ShaderModuleDescriptor<'_>) -> ShaderModule {
        ShaderModule
    }
    pub fn create_pipeline_layout(&self, _d: &PipelineLayoutDescriptor<'_>) -> PipelineLayout {
        PipelineLayout
    }
    pub fn create_compute_pipeline(&self, _d: &ComputePipelineDescriptor<'_>) -> ComputePipeline {
        ComputePipeline
    }
    pub fn create_render_pipeline(&self, _d: &RenderPipelineDescriptor<'_>) -> RenderPipeline {
        RenderPipeline
    }
    pub fn create_sampler(&self, _d: &SamplerDescriptor<'_>) -> Sampler {
        Sampler
    }
    pub fn create_buffer(&self, _d: &BufferDescriptor<'_>) -> Buffer {
        Buffer
    }
    pub fn create_texture(&self, _d: &TextureDescriptor<'_>) -> Texture {
        Texture
    }
    pub fn create_command_encoder(&self, _d: &CommandEncoderDescriptor<'_>) -> CommandEncoder {
        CommandEncoder
    }
    pub fn poll(&self, _maintain: Maintain) {}
}

impl Queue {
    pub fn write_buffer(&self, _buffer: &Buffer, _offset: u64, _data: &[u8]) {}
    pub fn write_texture(
        &self,
        _dst: ImageCopyTexture<'_>,
        _data: &[u8],
        _layout: ImageDataLayout,
        _size: Extent3d,
    ) {
    }
    pub fn submit<I: IntoIterator<Item = CommandBuffer>>(&self, _buffers: I) {}
}
