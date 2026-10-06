//! Vulkan on the GPU service: SPIR-V shaders (through naga), compute
//! pipelines, and the command buffers a guest's Vulkan driver records,
//! replayed onto wgpu (packages/vulkan; docs/abi.md, "GPU").
//!
//! A descriptor set's buffers are taken when it's bound (Vulkan doesn't let
//! a bound set change before its commands run), and a pipeline's bind
//! groups are made at each dispatch from the bindings its entry point uses
//! (wgpu's layouts for a pipeline hold only those). Push constants, which
//! WebGPU has only as a native extension, become a read-only storage buffer
//! in a bind group of their own, after the shader's sets; and every buffer
//! is read-write storage (see `shader`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::device::Gpu;
use crate::ir::Res;

/// The bytes of push constants a pipeline may have (Vulkan's minimum).
pub const PUSH_BYTES: u32 = 128;

/// A shader module: SPIR-V made a wgpu shader, with what each entry point
/// uses.
pub struct Shader {
    module: wgpu::ShaderModule,
    /// Per entry point: its stage and the (set, binding)s it uses.
    entries: BTreeMap<String, (naga::ShaderStage, BTreeSet<(u32, u32)>)>,
    /// The bind group push constants are in, if the shader has them.
    push_group: Option<u32>,
}

/// A compute pipeline.
pub struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    /// The bindings its entry point uses, by set.
    uses: BTreeMap<u32, Vec<u32>>,
    push_group: Option<u32>,
}

/// SPIR-V words as a shader, or why it can't be one.
pub fn shader(gpu: &Gpu, spirv: &[u8]) -> Res<Shader> {
    let options = naga::front::spv::Options { adjust_coordinate_space: false, strict_capabilities: false, block_ctx_dump_prefix: None };
    // (Its combined image samplers split, its bindings renumbered: spirv.rs.)
    let spirv = crate::spirv::prepare(spirv)?;
    let mut module = naga::front::spv::parse_u8_slice(&spirv, &options).map_err(|e| format!("SPIR-V: {e}"))?;
    // Push constants: a storage buffer in the group after the sets'.
    let used = module.global_variables.iter().filter_map(|(_, g)| g.binding.as_ref().map(|b| b.group)).max();
    let push_group = used.map_or(0, |g| g + 1);
    let mut has_push = false;
    // Every buffer read-write storage: a Vulkan program's buffers are often
    // ranges of one memory (one wgpu buffer), and WebGPU won't have a buffer
    // in one dispatch as both read-only (or uniform) and read-write, even in
    // ranges apart. (Storage keeps the SPIR-V's explicit layout, so a
    // uniform's std140 offsets stay.)
    let read_write = naga::AddressSpace::Storage { access: naga::StorageAccess::LOAD | naga::StorageAccess::STORE };
    for (_, g) in module.global_variables.iter_mut() {
        match g.space {
            naga::AddressSpace::Immediate => {
                g.space = naga::AddressSpace::Storage { access: naga::StorageAccess::LOAD };
                g.binding = Some(naga::ResourceBinding { group: push_group, binding: 0 });
                has_push = true;
            }
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. } => g.space = read_write,
            _ => {}
        }
    }
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| format!("shader: {}", e.emit_to_string("")))?;
    let mut entries = BTreeMap::new();
    for (k, ep) in module.entry_points.iter().enumerate() {
        let uses = info.get_entry_point(k);
        let bindings = module
            .global_variables
            .iter()
            .filter(|(h, _)| !uses[*h].is_empty())
            .filter_map(|(_, g)| g.binding.as_ref().map(|b| (b.group, b.binding)))
            .collect();
        entries.insert(ep.name.clone(), (ep.stage, bindings));
    }
    let wgsl = naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty()).map_err(|e| format!("shader: {e}"))?;
    if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
        eprintln!("wasm-env: Vulkan shader:\n{wgsl}");
    }
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(wgsl.into()) });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("shader: {e}"));
    }
    Ok(Shader { module, entries, push_group: has_push.then_some(push_group) })
}

/// A compute pipeline of a shader's entry point.
pub fn compute_pipeline(gpu: &Gpu, shader: &Shader, entry: &str) -> Res<Pipeline> {
    let Some((stage, bindings)) = shader.entries.get(entry) else { return Err(format!("no entry point {entry}")) };
    if *stage != naga::ShaderStage::Compute {
        return Err(format!("{entry} isn't a compute shader"));
    }
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader.module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("pipeline: {e}"));
    }
    let mut uses: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(g, b) in bindings {
        if Some(g) != shader.push_group {
            uses.entry(g).or_default().push(b);
        }
    }
    Ok(Pipeline { pipeline, uses, push_group: shader.push_group.filter(|g| bindings.iter().any(|&(bg, _)| bg == *g)) })
}

/// A descriptor: what binding `binding` of its set is.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub binding: u32,
    pub resource: Resource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Resource {
    /// `size` bytes of a buffer from `offset`.
    Buffer { buffer: u64, offset: u64, size: u64 },
    /// An image view (sampled or storage).
    View(u64),
    Sampler(u64),
    /// A combined image sampler's view and sampler.
    Combined { view: u64, sampler: u64 },
}

/// What a copy between a buffer and an image covers: the buffer's bytes
/// (`row_length` texels a row, `image_height` rows an image; 0 for the
/// copy's own), and the image's region.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub buffer: u64,
    pub offset: u64,
    pub image: u64,
    pub row_length: u32,
    pub image_height: u32,
    pub sub: Sub,
}

/// An image's mip level, layers, aspect (Vulkan's), origin and extent.
#[derive(Clone, Debug, PartialEq)]
pub struct Sub {
    pub mip: u32,
    pub layer: u32,
    pub layers: u32,
    pub aspect: u32,
    pub origin: [i32; 3],
    pub extent: [u32; 3],
}

/// A recorded command (see `decode`).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    BindPipeline(u64),
    BindSet { set: u32, bindings: Vec<Binding> },
    Push { offset: u32, data: Vec<u8> },
    Dispatch([u32; 3]),
    DispatchIndirect { buffer: u64, offset: u64 },
    Copy { src: u64, src_offset: u64, dst: u64, dst_offset: u64, size: u64 },
    Fill { dst: u64, offset: u64, size: u64, word: u32 },
    Update { dst: u64, offset: u64, data: Vec<u8> },
    BufferToImage(Region),
    ImageToBuffer(Region),
    /// From `src`'s `from` to `dst`'s `to` (whose extent is the copy's).
    CopyImage { src: u64, from: Sub, dst: u64, to: Sub },
    /// `color`'s bits (floats, or integers for an integer format).
    ClearColor { image: u64, mip: u32, mips: u32, layer: u32, layers: u32, color: [u32; 4] },
}

/// A command buffer's commands from the stream the guest recorded: each a
/// u32 operation and its length in bytes (with these 8), then its operands,
/// little-endian:
///
///   1 BIND_PIPELINE  pipeline (u64)
///   2 BIND_SET       set, count (u32), then count × binding, kind (u32:
///                    0 a buffer, 1 a view, 2 a sampler, 3 a view and
///                    sampler), and three u64s: buffer, offset, size; or
///                    view; or sampler; or view, sampler
///   3 PUSH           offset, length (u32), the bytes
///   4 DISPATCH       x, y, z (u32)
///   5 DISPATCH_INDIRECT  buffer, offset (u64)
///   6 COPY           source, offset, destination, offset, size (u64)
///   7 FILL           buffer, offset, size (u64), word (u32)
///   8 UPDATE         buffer, offset (u64), length (u32), the bytes
///   9 BUFFER_TO_IMAGE  buffer, offset, image (u64), row length, image
///                    height (u32), then a SUB
///  10 IMAGE_TO_BUFFER  likewise
///  11 COPY_IMAGE     source (u64), a SUB, destination (u64), a SUB
///  12 CLEAR_COLOR    image (u64), mip, mips, layer, layers, color (u32 × 4)
///
/// A SUB: mip, layer, layers, aspect (u32), origin (i32 × 3), extent (u32 × 3).
pub fn decode(stream: &[u8]) -> Res<Vec<Command>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let u32_at = |k: usize| -> Res<u32> { stream.get(k..k + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "a command past the stream's end".to_string()) };
        let (op, len) = (u32_at(at)?, u32_at(at + 4)? as usize);
        if len < 8 || at + len > stream.len() {
            return Err(format!("a command of {len} bytes at {at}"));
        }
        let body = &stream[at + 8..at + len];
        let mut r = Reader { b: body, at: 0 };
        out.push(match op {
            1 => Command::BindPipeline(r.u64()?),
            2 => {
                let (set, n) = (r.u32()?, r.u32()?);
                let bindings = (0..n)
                    .map(|_| {
                        let (binding, kind) = (r.u32()?, r.u32()?);
                        let (a, b, c) = (r.u64()?, r.u64()?, r.u64()?);
                        let resource = match kind {
                            0 => Resource::Buffer { buffer: a, offset: b, size: c },
                            1 => Resource::View(a),
                            2 => Resource::Sampler(a),
                            3 => Resource::Combined { view: a, sampler: b },
                            k => return Err(format!("a descriptor of kind {k}")),
                        };
                        Ok(Binding { binding, resource })
                    })
                    .collect::<Res<_>>()?;
                Command::BindSet { set, bindings }
            }
            3 => {
                let (offset, n) = (r.u32()?, r.u32()? as usize);
                Command::Push { offset, data: r.bytes(n)? }
            }
            4 => Command::Dispatch([r.u32()?, r.u32()?, r.u32()?]),
            5 => Command::DispatchIndirect { buffer: r.u64()?, offset: r.u64()? },
            6 => Command::Copy { src: r.u64()?, src_offset: r.u64()?, dst: r.u64()?, dst_offset: r.u64()?, size: r.u64()? },
            7 => Command::Fill { dst: r.u64()?, offset: r.u64()?, size: r.u64()?, word: r.u32()? },
            8 => {
                let (dst, offset, n) = (r.u64()?, r.u64()?, r.u32()? as usize);
                Command::Update { dst, offset, data: r.bytes(n)? }
            }
            9 | 10 => {
                let region = Region { buffer: r.u64()?, offset: r.u64()?, image: r.u64()?, row_length: r.u32()?, image_height: r.u32()?, sub: r.sub()? };
                if op == 9 { Command::BufferToImage(region) } else { Command::ImageToBuffer(region) }
            }
            11 => Command::CopyImage { src: r.u64()?, from: r.sub()?, dst: r.u64()?, to: r.sub()? },
            12 => Command::ClearColor { image: r.u64()?, mip: r.u32()?, mips: r.u32()?, layer: r.u32()?, layers: r.u32()?, color: [r.u32()?, r.u32()?, r.u32()?, r.u32()?] },
            op => return Err(format!("command {op}")),
        });
        at += len;
    }
    Ok(out)
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Res<Vec<u8>> {
        let v = self.b.get(self.at..self.at + n).ok_or("a command shorter than its operands")?.to_vec();
        self.at += n;
        Ok(v)
    }
    fn u32(&mut self) -> Res<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Res<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn sub(&mut self) -> Res<Sub> {
        Ok(Sub {
            mip: self.u32()?,
            layer: self.u32()?,
            layers: self.u32()?,
            aspect: self.u32()?,
            origin: [self.u32()? as i32, self.u32()? as i32, self.u32()? as i32],
            extent: [self.u32()?, self.u32()?, self.u32()?],
        })
    }
}

// ---- Images ----

/// A Vulkan format as WebGPU's, where WebGPU has it.
pub fn format(vk: u32) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat as F;
    Some(match vk {
        9 => F::R8Unorm,
        10 => F::R8Snorm,
        13 => F::R8Uint,
        14 => F::R8Sint,
        16 => F::Rg8Unorm,
        17 => F::Rg8Snorm,
        20 => F::Rg8Uint,
        21 => F::Rg8Sint,
        37 => F::Rgba8Unorm,
        38 => F::Rgba8Snorm,
        41 => F::Rgba8Uint,
        42 => F::Rgba8Sint,
        43 => F::Rgba8UnormSrgb,
        44 => F::Bgra8Unorm,
        50 => F::Bgra8UnormSrgb,
        64 => F::Rgb10a2Unorm,
        68 => F::Rgb10a2Uint,
        70 => F::R16Unorm,
        71 => F::R16Snorm,
        74 => F::R16Uint,
        75 => F::R16Sint,
        76 => F::R16Float,
        77 => F::Rg16Unorm,
        78 => F::Rg16Snorm,
        81 => F::Rg16Uint,
        82 => F::Rg16Sint,
        83 => F::Rg16Float,
        91 => F::Rgba16Unorm,
        92 => F::Rgba16Snorm,
        95 => F::Rgba16Uint,
        96 => F::Rgba16Sint,
        97 => F::Rgba16Float,
        98 => F::R32Uint,
        99 => F::R32Sint,
        100 => F::R32Float,
        101 => F::Rg32Uint,
        102 => F::Rg32Sint,
        103 => F::Rg32Float,
        107 => F::Rgba32Uint,
        108 => F::Rgba32Sint,
        109 => F::Rgba32Float,
        122 => F::Rg11b10Ufloat,
        123 => F::Rgb9e5Ufloat,
        124 => F::Depth16Unorm,
        125 => F::Depth24Plus,
        126 => F::Depth32Float,
        127 => F::Stencil8,
        129 => F::Depth24PlusStencil8,
        130 => F::Depth32FloatStencil8,
        _ => return None,
    })
}

/// What the device does with a format, as Vulkan's VkFormatFeatureFlags
/// for optimal tiling: WebGPU's guarantees for it (the same on every
/// device), and the texel's size in bytes (0: not a format it has).
pub fn format_features(gpu: &Gpu, vk: u32) -> (u32, u32) {
    let Some(f) = format(vk) else { return (0, 0) };
    if !gpu.device.features().contains(f.required_features()) {
        return (0, 0);
    }
    let ff = f.guaranteed_format_features(gpu.device.features());
    let (u, flags) = (ff.allowed_usages, ff.flags);
    let depth = f.is_depth_stencil_format();
    let mut v = 0;
    if u.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
        v |= 0x1; // SAMPLED_IMAGE
        if flags.contains(wgpu::TextureFormatFeatureFlags::FILTERABLE) {
            v |= 0x1000; // SAMPLED_IMAGE_FILTER_LINEAR
        }
    }
    if u.contains(wgpu::TextureUsages::STORAGE_BINDING) {
        v |= 0x2; // STORAGE_IMAGE
    }
    if u.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        v |= if depth { 0x200 } else { 0x80 }; // DEPTH_STENCIL_ATTACHMENT, COLOR_ATTACHMENT
        if !depth && flags.contains(wgpu::TextureFormatFeatureFlags::BLENDABLE) {
            v |= 0x100; // COLOR_ATTACHMENT_BLEND
        }
    }
    if u.contains(wgpu::TextureUsages::COPY_SRC) {
        v |= 0x4000; // TRANSFER_SRC
    }
    if u.contains(wgpu::TextureUsages::COPY_DST) {
        v |= 0x8000; // TRANSFER_DST
    }
    (v, f.block_copy_size(None).or_else(|| f.block_copy_size(Some(wgpu::TextureAspect::DepthOnly))).unwrap_or(0))
}

/// An image: `dims` 1, 2 or 3, Vulkan's format and usage
/// (VkImageUsageFlags), its extent, mip levels, layers and samples.
pub fn image(gpu: &Gpu, dims: u32, vk_format: u32, extent: [u32; 3], mips: u32, layers: u32, samples: u32, usage: u32) -> Res<wgpu::Texture> {
    let f = format(vk_format).ok_or_else(|| format!("format {vk_format}"))?;
    let allowed = f.guaranteed_format_features(gpu.device.features()).allowed_usages;
    // (Copies always: Vulkan's transfers, and clears through them.)
    let mut u = wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
    if usage & (0x4 | 0x80) != 0 {
        u |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if usage & 0x8 != 0 {
        u |= wgpu::TextureUsages::STORAGE_BINDING;
    }
    // (Clears are render passes: attachments whenever the format can be.)
    u |= allowed & wgpu::TextureUsages::RENDER_ATTACHMENT;
    if !allowed.contains(u) {
        return Err(format!("{f:?} images can't be used so ({:?})", u - allowed));
    }
    let (dimension, depth_or_array_layers) = match dims {
        1 => (wgpu::TextureDimension::D1, layers),
        3 => (wgpu::TextureDimension::D3, extent[2]),
        _ => (wgpu::TextureDimension::D2, layers),
    };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: extent[0], height: extent[1], depth_or_array_layers },
        mip_level_count: mips.max(1),
        sample_count: samples.max(1),
        dimension,
        format: f,
        usage: u,
        view_formats: &[],
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("image: {e}"));
    }
    Ok(t)
}

/// Vulkan's aspect bits as WebGPU's aspect.
fn aspect(bits: u32) -> wgpu::TextureAspect {
    match bits {
        2 => wgpu::TextureAspect::DepthOnly,
        4 => wgpu::TextureAspect::StencilOnly,
        _ => wgpu::TextureAspect::All,
    }
}

/// A view of an image: Vulkan's view type (VkImageViewType), its format
/// (0: the image's), aspect, and mip levels and layers.
pub fn view(t: &wgpu::Texture, kind: u32, vk_format: u32, aspect_bits: u32, mip: u32, mips: u32, layer: u32, layers: u32) -> Res<wgpu::TextureView> {
    use wgpu::TextureViewDimension as D;
    let dimension = match kind {
        0 => D::D1,
        2 => D::D3,
        3 => D::Cube,
        5 => D::D2Array,
        6 => D::CubeArray,
        4 => return Err("1D array views".into()),
        _ => D::D2,
    };
    let format = if vk_format == 0 { None } else { Some(format(vk_format).ok_or_else(|| format!("format {vk_format}"))?) };
    Ok(t.create_view(&wgpu::TextureViewDescriptor {
        label: None,
        format,
        dimension: Some(dimension),
        usage: None,
        aspect: aspect(aspect_bits),
        base_mip_level: mip,
        mip_level_count: Some(mips),
        base_array_layer: layer,
        array_layer_count: if dimension == D::D3 { None } else { Some(layers) },
    }))
}

/// A sampler, from VkSamplerCreateInfo's fields: filters (magnification,
/// minification, mipmaps), address modes (u, v, w), maximum anisotropy (1:
/// none), the comparison (VkCompareOp + 1; 0 for none), and LOD bounds.
pub fn sampler(gpu: &Gpu, filters: [u32; 3], address: [u32; 3], anisotropy: u16, compare: u32, lod: [f32; 2]) -> wgpu::Sampler {
    let filter = |f| if f == 1 { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
    let mip = if filters[2] == 1 { wgpu::MipmapFilterMode::Linear } else { wgpu::MipmapFilterMode::Nearest };
    // (Clamping to a border color isn't WebGPU's: to the edge.)
    let mode = |m| match m {
        0 => wgpu::AddressMode::Repeat,
        1 => wgpu::AddressMode::MirrorRepeat,
        _ => wgpu::AddressMode::ClampToEdge,
    };
    use wgpu::CompareFunction as C;
    let compare = [C::Never, C::Less, C::Equal, C::LessEqual, C::Greater, C::NotEqual, C::GreaterEqual, C::Always].get((compare as usize).wrapping_sub(1)).copied();
    // (Anisotropy only with every filter linear.)
    let linear = filters.iter().all(|&f| f == 1);
    gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: None,
        address_mode_u: mode(address[0]),
        address_mode_v: mode(address[1]),
        address_mode_w: mode(address[2]),
        mag_filter: filter(filters[0]),
        min_filter: filter(filters[1]),
        mipmap_filter: mip,
        lod_min_clamp: lod[0],
        lod_max_clamp: lod[1],
        compare,
        anisotropy_clamp: if linear { anisotropy.clamp(1, 16) } else { 1 },
        border_color: None,
    })
}

/// What a command's handle names.
pub enum Object<'a> {
    Buffer(&'a wgpu::Buffer),
    Texture(&'a wgpu::Texture),
    View(&'a wgpu::TextureView),
    Sampler(&'a wgpu::Sampler),
    Pipeline(Arc<Pipeline>),
}

/// The copy's origin and extent on a texture (a 2D array's layers as its
/// third dimension).
fn on_texture<'t>(t: &'t wgpu::Texture, s: &Sub) -> (wgpu::TexelCopyTextureInfo<'t>, wgpu::Extent3d) {
    let d3 = t.dimension() == wgpu::TextureDimension::D3;
    let z = if d3 { s.origin[2] as u32 } else { s.layer };
    let depth = if d3 { s.extent[2] } else { s.layers };
    (
        wgpu::TexelCopyTextureInfo { texture: t, mip_level: s.mip, origin: wgpu::Origin3d { x: s.origin[0] as u32, y: s.origin[1] as u32, z }, aspect: aspect(s.aspect) },
        wgpu::Extent3d { width: s.extent[0], height: s.extent[1], depth_or_array_layers: depth },
    )
}

/// A copy between a buffer and an image, in the encoder. WebGPU wants a
/// buffer's rows 256 bytes apart; where Vulkan's aren't, a row at a time.
fn buffer_image(enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, t: &wgpu::Texture, r: &Region, to_image: bool) -> Res<()> {
    let (place, extent) = on_texture(t, &r.sub);
    let block = t.format().block_copy_size(Some(place.aspect)).ok_or("an image whose texels can't be copied")?;
    let row_length = if r.row_length == 0 { extent.width } else { r.row_length };
    let rows = if r.image_height == 0 { extent.height } else { r.image_height };
    let row_bytes = row_length * block;
    let mut copy = |offset: u64, place: wgpu::TexelCopyTextureInfo, extent: wgpu::Extent3d, bytes_per_row: Option<u32>| {
        let b = wgpu::TexelCopyBufferInfo { buffer, layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row, rows_per_image: bytes_per_row.map(|_| rows) } };
        if to_image { enc.copy_buffer_to_texture(b, place, extent) } else { enc.copy_texture_to_buffer(place, b, extent) }
    };
    if row_bytes.is_multiple_of(256) || (extent.height <= 1 && extent.depth_or_array_layers <= 1) {
        copy(r.offset, place, extent, Some(row_bytes));
    } else {
        for z in 0..extent.depth_or_array_layers {
            for y in 0..extent.height {
                let offset = r.offset + (z as u64 * rows as u64 + y as u64) * row_bytes as u64;
                let mut p = place;
                p.origin.y += y;
                p.origin.z += z;
                copy(offset, p, wgpu::Extent3d { width: extent.width, height: 1, depth_or_array_layers: 1 }, None);
            }
        }
    }
    Ok(())
}

/// Runs commands in order, as one submission: `object` finds what their
/// handles name.
pub fn submit<'a>(gpu: &Gpu, commands: &[Command], object: impl Fn(u64) -> Option<Object<'a>>) -> Res<()> {
    let buf = |h: u64| match object(h) {
        Some(Object::Buffer(b)) => Ok(b),
        _ => Err(format!("no buffer {h}")),
    };
    let tex = |h: u64| match object(h) {
        Some(Object::Texture(t)) => Ok(t),
        _ => Err(format!("no image {h}")),
    };
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    let mut bound: Option<Arc<Pipeline>> = None;
    let mut sets: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
    let mut push = vec![0u8; PUSH_BYTES as usize];
    // (Bytes a command writes from the host: a buffer of them, copied in
    // order with the rest.)
    let staged = |data: &[u8]| {
        wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: data, usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::STORAGE })
    };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    for c in commands {
        match c {
            Command::BindPipeline(h) => {
                bound = Some(match object(*h) {
                    Some(Object::Pipeline(p)) => p,
                    _ => return Err(format!("no pipeline {h}")),
                })
            }
            Command::BindSet { set, bindings } => {
                sets.insert(*set, bindings.clone());
            }
            Command::Push { offset, data } => {
                let end = *offset as usize + data.len();
                if end > push.len() {
                    return Err(format!("push constants to byte {end} (at most {PUSH_BYTES})"));
                }
                push[*offset as usize..end].copy_from_slice(data);
            }
            Command::Dispatch(_) | Command::DispatchIndirect { .. } => {
                let p = bound.as_ref().ok_or("a dispatch with no pipeline bound")?;
                let push_buffer = p.push_group.map(|_| staged(&push));
                let mut groups = Vec::new();
                for (&g, used) in &p.uses {
                    let set = sets.get(&g).ok_or_else(|| format!("set {g} isn't bound"))?;
                    let entries = used
                        .iter()
                        .map(|&w| {
                            let (b, sampler_part) = crate::spirv::vulkan_binding(w);
                            let d = set.iter().find(|d| d.binding == b).ok_or_else(|| format!("set {g} has no binding {b}"))?;
                            let view = |h: u64| match object(h) {
                                Some(Object::View(v)) => Ok(wgpu::BindingResource::TextureView(v)),
                                _ => Err(format!("no image view {h}")),
                            };
                            let sampler = |h: u64| match object(h) {
                                Some(Object::Sampler(s)) => Ok(wgpu::BindingResource::Sampler(s)),
                                _ => Err(format!("no sampler {h}")),
                            };
                            let resource = match (&d.resource, sampler_part) {
                                (Resource::Buffer { buffer, offset, size }, false) => {
                                    wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: buf(*buffer)?, offset: *offset, size: wgpu::BufferSize::new(*size) })
                                }
                                (Resource::View(v), false) | (Resource::Combined { view: v, .. }, false) => view(*v)?,
                                (Resource::Sampler(s), false) | (Resource::Combined { sampler: s, .. }, true) => sampler(*s)?,
                                (r, _) => return Err(format!("set {g} binding {b}: {r:?} isn't what the shader takes there")),
                            };
                            Ok(wgpu::BindGroupEntry { binding: w, resource })
                        })
                        .collect::<Res<Vec<_>>>()?;
                    groups.push((g, gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.pipeline.get_bind_group_layout(g), entries: &entries })));
                }
                if let (Some(g), Some(b)) = (p.push_group, &push_buffer) {
                    let entries = [wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }];
                    groups.push((g, gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.pipeline.get_bind_group_layout(g), entries: &entries })));
                }
                let mut pass = enc.begin_compute_pass(&Default::default());
                pass.set_pipeline(&p.pipeline);
                for (g, group) in &groups {
                    pass.set_bind_group(*g, group, &[]);
                }
                match c {
                    Command::Dispatch([x, y, z]) => pass.dispatch_workgroups(*x, *y, *z),
                    Command::DispatchIndirect { buffer, offset } => pass.dispatch_workgroups_indirect(buf(*buffer)?, *offset),
                    _ => unreachable!(),
                }
            }
            // (Within one buffer, two of Vulkan's in one memory: through
            // another, as WebGPU won't copy a buffer to itself.)
            Command::Copy { src, src_offset, dst, dst_offset, size } if src == dst => {
                let tmp = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: *size, usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
                enc.copy_buffer_to_buffer(buf(*src)?, *src_offset, &tmp, 0, *size);
                enc.copy_buffer_to_buffer(&tmp, 0, buf(*dst)?, *dst_offset, *size);
            }
            Command::Copy { src, src_offset, dst, dst_offset, size } => enc.copy_buffer_to_buffer(buf(*src)?, *src_offset, buf(*dst)?, *dst_offset, *size),
            Command::Fill { dst, offset, size, word } => {
                if *word == 0 {
                    enc.clear_buffer(buf(*dst)?, *offset, Some(*size));
                } else {
                    let pattern: Vec<u8> = word.to_le_bytes().iter().copied().cycle().take(*size as usize).collect();
                    enc.copy_buffer_to_buffer(&staged(&pattern), 0, buf(*dst)?, *offset, *size);
                }
            }
            Command::Update { dst, offset, data } => enc.copy_buffer_to_buffer(&staged(data), 0, buf(*dst)?, *offset, data.len() as u64),
            Command::BufferToImage(r) => buffer_image(&mut enc, buf(r.buffer)?, tex(r.image)?, r, true)?,
            Command::ImageToBuffer(r) => buffer_image(&mut enc, buf(r.buffer)?, tex(r.image)?, r, false)?,
            Command::CopyImage { src, from, dst, to } => {
                let (a, extent) = on_texture(tex(*src)?, from);
                let (b, _) = on_texture(tex(*dst)?, to);
                enc.copy_texture_to_texture(a, b, extent);
            }
            Command::ClearColor { image, mip, mips, layer, layers, color } => {
                let t = tex(*image)?;
                if !t.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
                    return Err(format!("clearing a {:?} image", t.format()));
                }
                // (A render pass per mip level and layer, cleared to the
                // color: its bits as the format's kind of number.)
                let value = |k: usize| match t.format().sample_type(None, None) {
                    Some(wgpu::TextureSampleType::Uint) => color[k] as f64,
                    Some(wgpu::TextureSampleType::Sint) => color[k] as i32 as f64,
                    _ => f32::from_bits(color[k]) as f64,
                };
                let clear = wgpu::Color { r: value(0), g: value(1), b: value(2), a: value(3) };
                let depth = if t.dimension() == wgpu::TextureDimension::D3 { 1 } else { *layers };
                for m in *mip..mip + mips {
                    for l in *layer..layer + depth {
                        let v = t.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(if t.dimension() == wgpu::TextureDimension::D3 { wgpu::TextureViewDimension::D3 } else { wgpu::TextureViewDimension::D2 }),
                            base_mip_level: m,
                            mip_level_count: Some(1),
                            base_array_layer: l,
                            array_layer_count: Some(1),
                            ..Default::default()
                        });
                        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: None,
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &v,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                            })],
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    gpu.queue.submit([enc.finish()]);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("{e}"));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// SPIR-V of a WGSL compute shader (naga's own writer, as a stand-in
    /// for glslang's).
    fn spirv(wgsl: &str) -> Vec<u8> {
        let module = naga::front::wgsl::parse_str(wgsl).unwrap();
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&module).unwrap();
        let words = naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None).unwrap();
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn a_compute_shader_runs_its_recorded_commands() {
        let Ok(g) = Gpu::open() else { return };
        let s = shader(
            &g,
            &spirv(
                "struct P { scale: u32, add: u32 }
var<immediate> p: P;
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(1) @binding(2) var<storage, read_write> dst: array<u32>;
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i < 16u) { dst[i] = src[i] * p.scale + p.add; }
}",
            ),
        )
        .unwrap();
        let p = Arc::new(compute_pipeline(&g, &s, "main").unwrap());
        let (src, mem, out) = (g.buffer(64).unwrap(), g.buffer(512).unwrap(), g.buffer(64).unwrap());
        let buffers: HashMap<u64, &wgpu::Buffer> = [(1, &src), (2, &mem), (3, &out)].into();
        let input: Vec<u8> = (0..16u32).flat_map(|i| (i * 10).to_le_bytes()).collect();
        let commands = [
            Command::Update { dst: 1, offset: 0, data: input },
            Command::Fill { dst: 2, offset: 0, size: 512, word: 0xdead_beef },
            Command::Push { offset: 0, data: 3u32.to_le_bytes().to_vec() },
            Command::Push { offset: 4, data: 5u32.to_le_bytes().to_vec() },
            Command::BindPipeline(9),
            Command::BindSet { set: 0, bindings: vec![Binding { binding: 0, resource: Resource::Buffer { buffer: 1, offset: 0, size: 64 } }] },
            // (dst at 256 bytes into its memory.)
            Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, resource: Resource::Buffer { buffer: 2, offset: 256, size: 64 } }] },
            Command::Dispatch([1, 1, 1]),
            Command::Copy { src: 2, src_offset: 256, dst: 3, dst_offset: 0, size: 64 },
        ];
        submit(&g, &commands, |h| if h == 9 { Some(Object::Pipeline(p.clone())) } else { buffers.get(&h).map(|b| Object::Buffer(b)) }).unwrap();
        let got: Vec<u32> = g.read(&out, 0, 64).unwrap().chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        let want: Vec<u32> = (0..16).map(|i| i * 10 * 3 + 5).collect();
        assert_eq!(got, want);
        // (The fill reached the rest of the memory.)
        assert_eq!(g.read(&mem, 0, 4).unwrap(), 0xdead_beefu32.to_le_bytes());
    }

    #[test]
    fn commands_decode_from_the_stream() {
        let mut s = Vec::new();
        let mut cmd = |op: u32, body: &[u8]| {
            s.extend(op.to_le_bytes());
            s.extend((body.len() as u32 + 8).to_le_bytes());
            s.extend(body);
        };
        cmd(1, &7u64.to_le_bytes());
        let mut set = vec![];
        for w in [1u32, 1, 2, 0] {
            set.extend(w.to_le_bytes());
        }
        for w in [5u64, 16, 32] {
            set.extend(w.to_le_bytes());
        }
        cmd(2, &set);
        cmd(4, &[2, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(
            decode(&s).unwrap(),
            [Command::BindPipeline(7), Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, resource: Resource::Buffer { buffer: 5, offset: 16, size: 32 } }] }, Command::Dispatch([2, 1, 1])]
        );
        assert!(decode(&s[..s.len() - 1]).is_err());
    }
}
