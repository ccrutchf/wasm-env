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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

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
    /// Its specialization constants (WGSL's overrides): id, and type.
    overrides: BTreeMap<u16, naga::Scalar>,
    /// Its binding arrays: set and binding (WebGPU's), and how many.
    arrays: BTreeMap<(u32, u32), u32>,
    /// Its buffers' least sizes: set and binding, and that size.
    sizes: BTreeMap<(u32, u32), u64>,
    /// Each binding's type, for a pipeline that can't have its layout made
    /// for it (wgpu's are of single items: binding arrays need their own).
    types: BTreeMap<(u32, u32), Res<(wgpu::BindingType, Option<std::num::NonZeroU32>)>>,
    /// The bind group push constants are in (binding 0), and the view
    /// index (multiview's `gl_ViewIndex`, binding 1), if the shader has
    /// them.
    push_group: Option<u32>,
    /// Its SPIR-V, if it computes with specialization constants, and its
    /// variants: the shader made again for each set of their values.
    spirv: Option<Vec<u8>>,
    variants: std::sync::Mutex<BTreeMap<Vec<Spec>, Arc<Shader>>>,
    /// The SPIR-V it was made of (specialized): for a vertex shader made
    /// again without its y flipped (`unflipped`).
    source: Arc<[u8]>,
}

/// A compute or graphics pipeline.
pub struct Pipeline {
    kind: Kind,
    /// The bindings its entry points use, by set.
    uses: BTreeMap<u32, Vec<u32>>,
    push_group: Option<u32>,
    /// The bindings in that group its shaders use: 0, push constants; 1,
    /// the view index.
    push_bindings: Vec<u32>,
    /// A graphics pipeline's vertex buffers: the Vulkan binding of each of
    /// WebGPU's slots, and how far into the buffer bound there the slot
    /// starts (attributes past their binding's stride have slots of their
    /// own, where they're within it).
    vertex_bindings: Vec<(u32, u64)>,
    /// Its binding arrays (Vulkan's descriptor arrays): WebGPU's set and
    /// binding, and how many.
    arrays: BTreeMap<(u32, u32), u32>,
    /// Its buffers' least sizes: the least a range bound there is
    /// (WebGPU's sizes are rounded up to their alignment, a `bvec3` block's
    /// 12 bytes to 16, and a runtime array has an element; Vulkan's
    /// aren't, and haven't).
    sizes: BTreeMap<(u32, u32), u64>,
    /// A graphics pipeline's making, and the variants made of it: for
    /// render passes whose color formats aren't its own (Vulkan's drivers
    /// draw with a pipeline made for RGBA into BGRA; WebGPU's won't), and
    /// for the state it leaves to commands (`GraphicsState::dynamic`) as
    /// they set it.
    recipe: Option<Recipe>,
    dynamic: u32,
    /// The Vulkan bindings it writes (storage buffers and images), as bits:
    /// set × 16 + binding (all of them: a set past 3 or a binding past 15).
    pub written: u64,
    /// A triangle fan's (VK_PRIMITIVE_TOPOLOGY_TRIANGLE_FAN): a list's,
    /// its draws' vertices made a list's indices (WebGPU has no fans).
    fan: bool,
    /// One culling both faces of triangles (which WebGPU can't): its draws
    /// draw nothing.
    culls_all: bool,
    variants: Mutex<HashMap<(Vec<wgpu::TextureFormat>, Dynamic), wgpu::RenderPipeline>>,
}

/// State set by commands that WebGPU makes part of a pipeline: depth bias
/// (constant, slope, clamp: f32s' bits) and stencil compare and write
/// masks, as last set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dynamic {
    pub bias: Option<[u32; 3]>,
    pub compare_mask: Option<u32>,
    pub write_mask: Option<u32>,
    /// And, for an indexed draw of strips, its indices' format (WebGPU's
    /// pipeline names it).
    pub strip_index: Option<wgpu::IndexFormat>,
    /// And whether the viewport's height is negative (maintenance1): the
    /// vertex shader's y not flipped, the viewport made positive.
    pub flip: bool,
}

/// What a graphics pipeline is made of (owned, to make it again).
struct Recipe {
    layout: wgpu::PipelineLayout,
    vs: (wgpu::ShaderModule, String, Vec<(String, f64)>),
    /// The vertex shader's SPIR-V, and the shader made of it without its y
    /// flipped, once a negative viewport height asks for it.
    vs_source: Arc<[u8]>,
    vs_unflipped: std::sync::OnceLock<Res<wgpu::ShaderModule>>,
    fs: Option<(wgpu::ShaderModule, String, Vec<(String, f64)>)>,
    buffers: Vec<(u64, wgpu::VertexStepMode, Vec<wgpu::VertexAttribute>)>,
    primitive: wgpu::PrimitiveState,
    depth_stencil: Option<wgpu::DepthStencilState>,
    multisample: wgpu::MultisampleState,
    targets: Vec<Option<wgpu::ColorTargetState>>,
}

impl Recipe {
    /// The pipeline, its color targets of `formats` where given, and its
    /// dynamic state as `d` has it.
    fn make(&self, gpu: &Gpu, formats: Option<&[wgpu::TextureFormat]>, d: Dynamic) -> Res<wgpu::RenderPipeline> {
        let targets: Vec<Option<wgpu::ColorTargetState>> = match formats {
            Some(f) => self.targets.iter().zip(f).map(|(t, &format)| t.clone().map(|t| wgpu::ColorTargetState { format, ..t })).collect(),
            None => self.targets.clone(),
        };
        let depth_stencil = self.depth_stencil.clone().map(|mut s| {
            if let Some([constant, slope, clamp]) = d.bias {
                s.bias = wgpu::DepthBiasState { constant: f32::from_bits(constant) as i32, slope_scale: f32::from_bits(slope), clamp: f32::from_bits(clamp) };
            }
            if let Some(m) = d.compare_mask {
                s.stencil.read_mask = m;
            }
            if let Some(m) = d.write_mask {
                s.stencil.write_mask = m;
            }
            // (A format without stencil: no stencil test, which Vulkan
            // ignores there and WebGPU refuses.)
            if !s.format.has_stencil_aspect() {
                s.stencil = Default::default();
            }
            s
        });
        let buffers: Vec<Option<wgpu::VertexBufferLayout>> = self.buffers.iter().map(|(array_stride, step_mode, attributes)| Some(wgpu::VertexBufferLayout { array_stride: *array_stride, step_mode: *step_mode, attributes })).collect();
        fn constants(c: &[(String, f64)]) -> Vec<(&str, f64)> {
            c.iter().map(|(k, v)| (k.as_str(), *v)).collect()
        }
        let (vc, fc) = (constants(&self.vs.2), self.fs.as_ref().map(|f| constants(&f.2)).unwrap_or_default());
        // (Under a negative viewport height, its vertex shader unflipped.)
        let vs = if d.flip { self.vs_unflipped.get_or_init(|| unpanicking(|| build_flipped(gpu, &self.vs_source, false)).map(|s| s.module)).clone()? } else { self.vs.0.clone() };
        caught(gpu, "pipeline", || {
            gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&self.layout),
            vertex: wgpu::VertexState { module: &vs, entry_point: Some(&self.vs.1), compilation_options: options(&vc), buffers: &buffers },
                primitive: wgpu::PrimitiveState { strip_index_format: d.strip_index, ..self.primitive },
            depth_stencil,
            multisample: self.multisample,
            fragment: self.fs.as_ref().map(|fs| wgpu::FragmentState { module: &fs.0, entry_point: Some(&fs.1), compilation_options: options(&fc), targets: &targets }),
            multiview_mask: None,
            cache: None,
            })
        })
    }
}

enum Kind {
    Compute(wgpu::ComputePipeline),
    Render(wgpu::RenderPipeline),
}

impl Pipeline {
    fn layout(&self, group: u32) -> wgpu::BindGroupLayout {
        match &self.kind {
            Kind::Compute(p) => p.get_bind_group_layout(group),
            Kind::Render(p) => p.get_bind_group_layout(group),
        }
    }

    /// A graphics pipeline for a render pass of these color formats, with
    /// the dynamic state commands set: itself, or a variant made for them.
    fn render_for(&self, gpu: &Gpu, formats: &[wgpu::TextureFormat], d: Dynamic) -> Res<wgpu::RenderPipeline> {
        let Kind::Render(rp) = &self.kind else { return Err("a compute pipeline in a render pass".into()) };
        let Some(recipe) = &self.recipe else { return Ok(rp.clone()) };
        let own: Vec<wgpu::TextureFormat> = recipe.targets.iter().flatten().map(|t| t.format).collect();
        let formats = if own.len() != formats.len() || recipe.targets.iter().any(Option::is_none) { own.clone() } else { formats.to_vec() };
        // (Only what it leaves to commands counts.)
        let d = Dynamic {
            bias: d.bias.filter(|_| self.dynamic & 1 != 0),
            compare_mask: d.compare_mask.filter(|_| self.dynamic & 2 != 0),
            write_mask: d.write_mask.filter(|_| self.dynamic & 4 != 0),
            strip_index: d.strip_index.filter(|_| recipe.primitive.topology.is_strip()),
            flip: d.flip,
        };
        if formats == own && d == Dynamic::default() {
            return Ok(rp.clone());
        }
        let mut variants = self.variants.lock().unwrap();
        let key = (formats, d);
        if let Some(v) = variants.get(&key) {
            return Ok(v.clone());
        }
        let v = recipe.make(gpu, Some(&key.0), d)?;
        variants.insert(key, v.clone());
        Ok(v)
    }
}

/// The Vulkan bindings a pipeline's entry points write, as bits (see
/// `Pipeline::written`).
fn written_of(shaders: &[(&Shader, &BTreeSet<(u32, u32)>)]) -> u64 {
    let mut bits = 0u64;
    for (s, used) in shaders {
        for &(g, w) in used.iter() {
            if Some(g) == s.push_group {
                continue;
            }
            let writes = match s.types.get(&(g, w)) {
                Some(Ok((wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, .. }, _))) => !read_only,
                Some(Ok((wgpu::BindingType::StorageTexture { access, .. }, _))) => *access != wgpu::StorageTextureAccess::ReadOnly,
                Some(Ok(_)) => false,
                _ => true,
            };
            if writes {
                let (b, _) = crate::spirv::vulkan_binding(w);
                if g >= 4 || b >= 16 {
                    return u64::MAX;
                }
                bits |= 1 << (g * 16 + b);
            }
        }
    }
    bits
}

/// The bindings a pipeline's entry points use, by set (the push
/// constants' group apart), and whether they use push constants.
fn uses_of(shaders: &[(&Shader, &BTreeSet<(u32, u32)>)]) -> (BTreeMap<u32, Vec<u32>>, Option<u32>, Vec<u32>) {
    let mut uses: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut push = None;
    let mut push_bindings = BTreeSet::new();
    for (shader, bindings) in shaders {
        for &(g, b) in bindings.iter() {
            if Some(g) == shader.push_group {
                push = Some(g);
                push_bindings.insert(b);
            } else {
                uses.entry(g).or_default().insert(b);
            }
        }
    }
    (uses.into_iter().map(|(g, b)| (g, b.into_iter().collect())).collect(), push, push_bindings.into_iter().collect())
}

/// SPIR-V words as a shader, or why it can't be one. One that computes with
/// specialization constants is made with their defaults, and again for
/// each pipeline's values (`Shader::variant`).
pub fn shader(gpu: &Gpu, spirv: &[u8]) -> Res<Shader> {
    let made = unpanicking(|| shader_of(gpu, spirv));
    // (One that can't be, kept for a look: WASM_ENV_DEBUG_SPIRV=FILE.)
    if let (Err(_), Some(path)) = (&made, std::env::var_os("WASM_ENV_DEBUG_SPIRV")) {
        let _ = std::fs::write(path, spirv);
    }
    made
}

/// What `make` makes, naga panicking at SPIR-V it can't read made an
/// error: a shader that can't be, not the device's end.
fn unpanicking<T>(make: impl FnOnce() -> Res<T>) -> Res<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)).unwrap_or_else(|p| {
        let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        Err(format!("SPIR-V: naga: {why}"))
    })
}

fn shader_of(gpu: &Gpu, spirv: &[u8]) -> Res<Shader> {
    if crate::spirv::computes_with_spec_constants(spirv) {
        let mut s = build(gpu, &crate::spirv::specialize(spirv, &[])?)?;
        s.spirv = Some(spirv.to_vec());
        return Ok(s);
    }
    build(gpu, spirv)
}

fn build(gpu: &Gpu, spirv: &[u8]) -> Res<Shader> {
    build_flipped(gpu, spirv, true)
}

fn build_flipped(gpu: &Gpu, spirv: &[u8], flip: bool) -> Res<Shader> {
    let source: Arc<[u8]> = spirv.into();
    // (Vulkan's clip space has y down, WebGPU's up: a vertex shader's
    // position is flipped, unless a negative viewport height flips it.)
    let options = naga::front::spv::Options { adjust_coordinate_space: flip, strict_capabilities: false, block_ctx_dump_prefix: None };
    // (Its combined image samplers split, its bindings renumbered: spirv.rs.)
    let spirv = crate::spirv::prepare(spirv)?;
    let mut module = naga::front::spv::parse_u8_slice(&spirv, &options).map_err(|e| format!("SPIR-V: {e}"))?;
    let validate = |m: &naga::Module| {
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(m).map_err(|e| format!("shader: {}", e.emit_to_string("")))
    };
    // Push constants: a storage buffer in the last group, after every set
    // the device has (the same group in each of a pipeline's shaders).
    let push_group = gpu.device.limits().max_bind_groups - 1;
    let mut has_push = false;
    // Buffers are storage (which keeps the SPIR-V's explicit layout, so a
    // uniform's std140 offsets stay). In a compute shader every one is
    // read-write: a Vulkan program's buffers are often ranges of one memory
    // (one wgpu buffer), and WebGPU won't have a buffer in one dispatch as
    // both read-only (or uniform) and read-write, even in ranges apart. In
    // a graphics shader, where a render pass's vertex and index buffers
    // (read-only uses) are often in that memory too, a buffer is read-only
    // unless the shader writes it.
    // An array of blocks is a descriptor array of buffers, which naga reads
    // as one buffer of them: a binding array. (The array's type goes: one
    // of blocks of no fixed size isn't valid.)
    let mut arrays_of_blocks = false;
    for (_, g) in module.global_variables.iter_mut() {
        if let (Some(_), naga::AddressSpace::Storage { .. } | naga::AddressSpace::Uniform) = (&g.binding, g.space) {
            if let naga::TypeInner::Array { base, size: naga::ArraySize::Constant(n), .. } = module.types[g.ty].inner {
                if matches!(module.types[base].inner, naga::TypeInner::Struct { .. }) {
                    // (Its block a type of its own: naga makes one of
                    // blocks alike, and a block both a binding array's and
                    // a buffer's makes SPIR-V drivers refuse.)
                    let name = Some(format!("block_array_{}", g.binding.as_ref().map_or(0, |b| b.group * 1000 + b.binding)));
                    let base = module.types.insert(naga::Type { name, inner: module.types[base].inner.clone() }, naga::Span::UNDEFINED);
                    let inner = naga::TypeInner::BindingArray { base, size: naga::ArraySize::Constant(n) };
                    g.ty = module.types.insert(naga::Type { name: None, inner }, naga::Span::UNDEFINED);
                    arrays_of_blocks = true;
                }
            }
        }
    }
    if arrays_of_blocks {
        naga::compact::compact(&mut module, naga::compact::KeepUnused::Yes);
    }
    let graphics = module.entry_points.iter().any(|e| e.stage != naga::ShaderStage::Compute);
    let written: BTreeSet<naga::Handle<naga::GlobalVariable>> = if graphics {
        let info = validate(&module)?;
        module
            .global_variables
            .iter()
            .filter(|(h, _)| (0..module.entry_points.len()).any(|k| info.get_entry_point(k)[*h].contains(naga::valid::GlobalUse::WRITE)))
            .map(|(h, _)| h)
            .collect()
    } else {
        BTreeSet::new()
    };
    let read_write = naga::AddressSpace::Storage { access: naga::StorageAccess::LOAD | naga::StorageAccess::STORE };
    let read_only = naga::AddressSpace::Storage { access: naga::StorageAccess::LOAD };
    for (h, g) in module.global_variables.iter_mut() {
        match g.space {
            naga::AddressSpace::Immediate => {
                g.space = read_only;
                g.binding = Some(naga::ResourceBinding { group: push_group, binding: 0 });
                has_push = true;
            }
            // (The view index's block, spirv.rs: beside them.)
            _ if g.binding.as_ref().is_some_and(|b| b.group == crate::spirv::VIEW_SET) => {
                g.space = read_only;
                g.binding = Some(naga::ResourceBinding { group: push_group, binding: 1 });
                has_push = true;
            }
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. } => g.space = if !graphics || written.contains(&h) { read_write } else { read_only },
            _ => {}
        }
    }
    let info = validate(&module)?;
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
    let arrays = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| match (&g.binding, &module.types[g.ty].inner) {
            (Some(b), naga::TypeInner::BindingArray { size: naga::ArraySize::Constant(n), .. }) => Some(((b.group, b.binding), n.get())),
            _ => None,
        })
        .collect();
    let sizes = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| {
            let b = g.binding.as_ref()?;
            let ty = match module.types[g.ty].inner {
                naga::TypeInner::BindingArray { base, .. } => base,
                _ => g.ty,
            };
            // (A runtime array's size counts one element, as WebGPU's
            // least binding does.)
            matches!(g.space, naga::AddressSpace::Storage { .. } | naga::AddressSpace::Uniform).then(|| ((b.group, b.binding), module.types[ty].inner.size(module.to_ctx()) as u64))
        })
        .collect();
    let overrides = module
        .overrides
        .iter()
        .filter_map(|(_, o)| match (o.id, &module.types[o.ty].inner) {
            (Some(id), naga::TypeInner::Scalar(s)) => Some((id, *s)),
            _ => None,
        })
        .collect();
    let sampled = sampled_images(&module);
    let types = module
        .global_variables
        .iter()
        .filter_map(|(h, g)| Some(((g.binding.as_ref()?.group, g.binding.as_ref()?.binding), binding_type(&module, g, sampled.contains(&h)))))
        .collect();
    // (Given to wgpu as naga's IR, not WGSL: WGSL can't say the SPIR-V's
    // explicit layout, a std140 array's stride of 16 or a member's offset.)
    // (As WGSL, to look at: WASM_ENV_DEBUG_WGSL.)
    if std::env::var_os("WASM_ENV_DEBUG_WGSL").is_some() {
        let wgsl = naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty()).unwrap_or_else(|e| e.to_string());
        eprintln!("wasm-env: Vulkan shader:\n{wgsl}");
    }
    let module =
        caught(gpu, "shader", || gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)) }))?;
    Ok(Shader { module, entries, overrides, arrays, sizes, types, push_group: has_push.then_some(push_group), spirv: None, variants: Default::default(), source })
    }

/// What `make` makes, or the error wgpu found making it: a validation
/// error, or an internal one (a backend's shader that didn't compile).
fn caught<T>(gpu: &Gpu, what: &str, make: impl FnOnce() -> T) -> Res<T> {
    let memory = gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = gpu.device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let made = make();
    let errors = [pollster::block_on(validation.pop()), pollster::block_on(internal.pop()), pollster::block_on(memory.pop())];
    match errors.into_iter().flatten().next() {
        Some(e) => Err(format!("{what}: {}", describe(&e))),
        None => Ok(made),
    }
}

/// The images a module samples (rather than only fetching from).
fn sampled_images(module: &naga::Module) -> BTreeSet<naga::Handle<naga::GlobalVariable>> {
    let mut sampled = BTreeSet::new();
    for f in module.functions.iter().map(|(_, f)| f).chain(module.entry_points.iter().map(|e| &e.function)) {
        for (_, e) in f.expressions.iter() {
            let naga::Expression::ImageSample { mut image, .. } = *e else {
                continue;
            };
            loop {
                match f.expressions[image] {
                    naga::Expression::GlobalVariable(g) => {
                        sampled.insert(g);
                        break;
                    }
                    naga::Expression::Access { base, .. } | naga::Expression::AccessIndex { base, .. } => image = base,
                    _ => break,
                }
            }
        }
    }
    sampled
}

/// A global's binding type as a layout declares it (and, for a binding
/// array, how many): what wgpu derives for a layout of its own, reflected
/// from the shader.
fn binding_type(module: &naga::Module, g: &naga::GlobalVariable, sampled: bool) -> Res<(wgpu::BindingType, Option<std::num::NonZeroU32>)> {
    use naga::{ImageClass, ImageDimension as Dim, ScalarKind as K, TypeInner as T};
    let (ty, count) = match module.types[g.ty].inner {
        T::BindingArray { base, size: naga::ArraySize::Constant(n) } => (base, Some(n)),
        T::BindingArray { .. } => return Err("a binding array of no fixed size".into()),
        _ => (g.ty, None),
    };
    let view_dimension = |dim, arrayed| match (dim, arrayed) {
        (Dim::D1, _) => wgpu::TextureViewDimension::D1,
        (Dim::D2, false) => wgpu::TextureViewDimension::D2,
        (Dim::D2, true) => wgpu::TextureViewDimension::D2Array,
        (Dim::D3, _) => wgpu::TextureViewDimension::D3,
        (Dim::Cube, false) => wgpu::TextureViewDimension::Cube,
        (Dim::Cube, true) => wgpu::TextureViewDimension::CubeArray,
    };
    let b = match module.types[ty].inner {
        T::Image { dim, arrayed, class } => match class {
            ImageClass::Sampled { kind, multi } => wgpu::BindingType::Texture {
                sample_type: match kind {
                    K::Sint => wgpu::TextureSampleType::Sint,
                    K::Uint => wgpu::TextureSampleType::Uint,
                    // (Filterable if it's sampled: one only fetched from
                    // may be of floats that aren't, a texel buffer's R32F.)
                    _ => wgpu::TextureSampleType::Float { filterable: sampled },
                },
                view_dimension: view_dimension(dim, arrayed),
                multisampled: multi,
            },
            ImageClass::Depth { multi } => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: view_dimension(dim, arrayed), multisampled: multi },
            ImageClass::Storage { format, access } => wgpu::BindingType::StorageTexture {
                access: if access.contains(naga::StorageAccess::LOAD | naga::StorageAccess::STORE) {
                    wgpu::StorageTextureAccess::ReadWrite
                } else if access.contains(naga::StorageAccess::STORE) {
                    wgpu::StorageTextureAccess::WriteOnly
                } else {
                    wgpu::StorageTextureAccess::ReadOnly
                },
                format: storage_format(format).ok_or_else(|| format!("storage images of {format:?}"))?,
                view_dimension: view_dimension(dim, arrayed),
            },
            ImageClass::External => return Err("external textures".into()),
        },
        T::Sampler { comparison } => wgpu::BindingType::Sampler(if comparison { wgpu::SamplerBindingType::Comparison } else { wgpu::SamplerBindingType::Filtering }),
        _ => wgpu::BindingType::Buffer {
            ty: match g.space {
                naga::AddressSpace::Uniform => wgpu::BufferBindingType::Uniform,
                naga::AddressSpace::Storage { access } => wgpu::BufferBindingType::Storage { read_only: !access.contains(naga::StorageAccess::STORE) },
                s => return Err(format!("a binding in {s:?}")),
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    };
    Ok((b, count))
}

/// naga's storage image formats as WebGPU's.
fn storage_format(f: naga::StorageFormat) -> Option<wgpu::TextureFormat> {
    use naga::StorageFormat as S;
    use wgpu::TextureFormat as F;
    Some(match f {
        S::R8Unorm => F::R8Unorm,
        S::R8Snorm => F::R8Snorm,
        S::R8Uint => F::R8Uint,
        S::R8Sint => F::R8Sint,
        S::R16Uint => F::R16Uint,
        S::R16Sint => F::R16Sint,
        S::R16Float => F::R16Float,
        S::Rg8Unorm => F::Rg8Unorm,
        S::Rg8Snorm => F::Rg8Snorm,
        S::Rg8Uint => F::Rg8Uint,
        S::Rg8Sint => F::Rg8Sint,
        S::R32Uint => F::R32Uint,
        S::R32Sint => F::R32Sint,
        S::R32Float => F::R32Float,
        S::Rg16Uint => F::Rg16Uint,
        S::Rg16Sint => F::Rg16Sint,
        S::Rg16Float => F::Rg16Float,
        S::Rgba8Unorm => F::Rgba8Unorm,
        S::Rgba8Snorm => F::Rgba8Snorm,
        S::Rgba8Uint => F::Rgba8Uint,
        S::Rgba8Sint => F::Rgba8Sint,
        S::Bgra8Unorm => F::Bgra8Unorm,
        S::Rgb10a2Uint => F::Rgb10a2Uint,
        S::Rgb10a2Unorm => F::Rgb10a2Unorm,
        S::Rg11b10Ufloat => F::Rg11b10Ufloat,
        S::Rg32Uint => F::Rg32Uint,
        S::Rg32Sint => F::Rg32Sint,
        S::Rg32Float => F::Rg32Float,
        S::Rgba16Uint => F::Rgba16Uint,
        S::Rgba16Sint => F::Rgba16Sint,
        S::Rgba16Float => F::Rgba16Float,
        S::Rgba32Uint => F::Rgba32Uint,
        S::Rgba32Sint => F::Rgba32Sint,
        S::Rgba32Float => F::Rgba32Float,
        S::R16Unorm => F::R16Unorm,
        S::R16Snorm => F::R16Snorm,
        S::Rg16Unorm => F::Rg16Unorm,
        S::Rg16Snorm => F::Rg16Snorm,
        S::Rgba16Unorm => F::Rgba16Unorm,
        S::Rgba16Snorm => F::Rgba16Snorm,
        _ => return None,
    })
}

/// A layout of a pipeline's own (for a graphics pipeline, or one with
/// binding arrays: wgpu's derived ones are of single items, and refuse a
/// binding two stages declare differently): each group up to the last used,
/// each binding its shaders use, visible to the stages that do, with no
/// minimum sizes.
fn explicit_layout(gpu: &Gpu, stages: &[(&Shader, &BTreeSet<(u32, u32)>, wgpu::ShaderStages)]) -> Res<wgpu::PipelineLayout> {
    let mut groups: BTreeMap<u32, BTreeMap<u32, wgpu::BindGroupLayoutEntry>> = BTreeMap::new();
    for (shader, used, stage) in stages {
        for &(g, b) in used.iter() {
            let (ty, count) = shader.types.get(&(g, b)).ok_or_else(|| format!("binding {g}.{b} not in its shader"))?.clone()?;
            let e = groups.entry(g).or_default().entry(b).or_insert(wgpu::BindGroupLayoutEntry { binding: b, visibility: wgpu::ShaderStages::NONE, ty, count });
            e.visibility |= *stage;
        }
    }
    let last = groups.keys().max().copied();
    caught(gpu, "layout", || {
    let layouts: Vec<wgpu::BindGroupLayout> = (0..last.map_or(0, |l| l + 1))
        .map(|g| {
            let entries: Vec<wgpu::BindGroupLayoutEntry> = groups.get(&g).map(|m| m.values().cloned().collect()).unwrap_or_default();
            gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &entries })
        })
        .collect();
    let refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &refs, immediate_size: 0 })
    })
}

/// A specialization constant's value from Vulkan: its id, its size in
/// bytes, and its bytes (the low and high words).
pub type Spec = [u32; 4];

impl Shader {
    /// The shader made with these specialization constants, if it computes
    /// with them (else None: its overrides take them). Made once per set of
    /// values.
    fn variant(&self, gpu: &Gpu, spec: &[Spec]) -> Res<Option<Arc<Shader>>> {
        let Some(spirv) = &self.spirv else { return Ok(None) };
        let mut variants = self.variants.lock().unwrap();
        if let Some(s) = variants.get(spec) {
            return Ok(Some(s.clone()));
        }
        let values: Vec<(u32, u64)> = spec.iter().map(|&[id, size, lo, hi]| (id, if size == 8 { (hi as u64) << 32 | lo as u64 } else { lo as u64 })).collect();
        let s = Arc::new(unpanicking(|| build(gpu, &crate::spirv::specialize(spirv, &values)?))?);
        variants.insert(spec.to_vec(), s.clone());
        Ok(Some(s))
    }

    /// Specialization constants as wgpu's override values, each read as its
    /// type in the shader (those it doesn't have are left out).
    fn constants(&self, spec: &[Spec]) -> Vec<(String, f64)> {
        use naga::ScalarKind as K;
        spec.iter()
            .filter_map(|&[id, size, lo, hi]| {
                let s = self.overrides.get(&u16::try_from(id).ok()?)?;
                let wide = size == 8 && s.width == 8;
                let v = match s.kind {
                    K::Bool => (lo != 0) as u32 as f64,
                    K::Sint if wide => ((hi as u64) << 32 | lo as u64) as i64 as f64,
                    K::Sint => lo as i32 as f64,
                    K::Uint if wide => ((hi as u64) << 32 | lo as u64) as f64,
                    K::Uint => lo as f64,
                    K::Float if wide => f64::from_bits((hi as u64) << 32 | lo as u64),
                    K::Float => f32::from_bits(lo) as f64,
                    _ => return None,
                };
                Some((id.to_string(), v))
            })
            .collect()
    }
}

fn options<'a>(constants: &'a [(&'a str, f64)]) -> wgpu::PipelineCompilationOptions<'a> {
    wgpu::PipelineCompilationOptions { constants, ..Default::default() }
}

/// A compute pipeline of a shader's entry point, specialized.
pub fn compute_pipeline(gpu: &Gpu, shader: &Shader, entry: &str, spec: &[Spec]) -> Res<Pipeline> {
    // (One computing with specialization constants: made for these.)
    let variant = shader.variant(gpu, spec)?;
    let shader = variant.as_deref().unwrap_or(shader);
    let Some((stage, bindings)) = shader.entries.get(entry) else { return Err(format!("no entry point {entry}")) };
    if *stage != naga::ShaderStage::Compute {
        return Err(format!("{entry} isn't a compute shader"));
    }
    let constants = shader.constants(spec);
    let constants: Vec<(&str, f64)> = constants.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    // (Binding arrays: a layout of its own.)
    let explicit = if shader.arrays.is_empty() { None } else { Some(explicit_layout(gpu, &[(shader, bindings, wgpu::ShaderStages::COMPUTE)])?) };
    let pipeline = caught(gpu, "pipeline", || {
        gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: explicit.as_ref(),
        module: &shader.module,
        entry_point: Some(entry),
        compilation_options: options(&constants),
        cache: None,
        })
    })?;
    let (uses, push_group, push_bindings) = uses_of(&[(shader, bindings)]);
    let written = written_of(&[(shader, bindings)]);
    Ok(Pipeline {
        kind: Kind::Compute(pipeline),
        uses,
        push_group,
        push_bindings,
        vertex_bindings: Vec::new(),
        arrays: shader.arrays.clone(),
        sizes: shader.sizes.clone(),
        recipe: None,
        dynamic: 0,
        fan: false,
        culls_all: false,
        written,
        variants: Default::default(),
    })
}

/// A graphics pipeline's state, from the words the driver sends (u32s,
/// little-endian; `GraphicsState::decode`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphicsState {
    /// The vertex and fragment shaders (0: none) and their entry points.
    pub vertex: (u64, String),
    pub fragment: (u64, String),
    /// VkPrimitiveTopology, VkCullModeFlags, VkFrontFace.
    pub topology: u32,
    pub cull: u32,
    pub front_face: u32,
    /// Depth bias (enabled, constant, slope, clamp).
    pub bias: (bool, f32, f32, f32),
    /// Depth test and write, VkCompareOp; stencil test, and its front and
    /// back: VkStencilOps (fail, pass, depth fail), VkCompareOp, compare
    /// and write masks.
    pub depth: (bool, bool, u32),
    pub stencil: (bool, [u32; 6], [u32; 6]),
    /// The depth attachment's format (0: none), and samples.
    pub depth_format: u32,
    pub samples: u32,
    /// Per color attachment: format, blend enabled, color's source and
    /// destination factors and op, alpha's likewise, write mask.
    pub colors: Vec<[u32; 9]>,
    /// Vertex buffers: binding, stride, input rate; attributes: location,
    /// binding, format, offset.
    pub buffers: Vec<[u32; 3]>,
    pub attributes: Vec<[u32; 4]>,
    /// The vertex and fragment shaders' specialization constants.
    pub vertex_spec: Vec<Spec>,
    pub fragment_spec: Vec<Spec>,
    /// What it leaves to commands (VkDynamicState's that WebGPU makes
    /// part of a pipeline): 1 depth bias, 2 stencil compare mask, 4
    /// stencil write mask.
    pub dynamic: u32,
}

impl GraphicsState {
    /// The words: the vertex shader (u64, as two u32s), the fragment
    /// shader (likewise), their entry points' lengths; topology, cull mode,
    /// front face; depth bias (enabled, then f32s: constant, slope, clamp);
    /// depth test, write, compare op; stencil test, front (6), back (6);
    /// depth format, samples; then the numbers of color attachments,
    /// vertex buffers and attributes, and each (9, 3 and 4 words); then
    /// the entry points' names, each padded to 4 bytes; then each shader's
    /// specialization constants: their number, and each (`Spec`'s 4 words).
    /// Last, what it leaves to commands (`dynamic`).
    pub fn decode(b: &[u8]) -> Res<GraphicsState> {
        let mut r = Reader { b, at: 0 };
        let mut s = GraphicsState::default();
        let (vs, fs) = (r.u64()?, r.u64()?);
        let (nv, nf) = (r.u32()? as usize, r.u32()? as usize);
        s.topology = r.u32()?;
        s.cull = r.u32()?;
        s.front_face = r.u32()?;
        s.bias = (r.u32()? != 0, f32::from_bits(r.u32()?), f32::from_bits(r.u32()?), f32::from_bits(r.u32()?));
        s.depth = (r.u32()? != 0, r.u32()? != 0, r.u32()?);
        let test = r.u32()? != 0;
        let mut six = || -> Res<[u32; 6]> { Ok([r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?]) };
        let (front, back) = (six()?, six()?);
        s.stencil = (test, front, back);
        s.depth_format = r.u32()?;
        s.samples = r.u32()?;
        let (nc, nb, na) = (r.u32()?, r.u32()?, r.u32()?);
        for _ in 0..nc {
            let mut c = [0; 9];
            for w in &mut c {
                *w = r.u32()?;
            }
            s.colors.push(c);
        }
        for _ in 0..nb {
            s.buffers.push([r.u32()?, r.u32()?, r.u32()?]);
        }
        for _ in 0..na {
            s.attributes.push([r.u32()?, r.u32()?, r.u32()?, r.u32()?]);
        }
        let mut name = |n: usize| -> Res<String> {
            let bytes = r.bytes(n.next_multiple_of(4))?;
            Ok(String::from_utf8_lossy(&bytes[..n]).into_owned())
        };
        s.vertex = (vs, name(nv)?);
        s.fragment = (fs, name(nf)?);
        for spec in [&mut s.vertex_spec, &mut s.fragment_spec] {
            let n = r.u32()?;
            for _ in 0..n {
                spec.push([r.u32()?, r.u32()?, r.u32()?, r.u32()?]);
            }
        }
        s.dynamic = r.u32()?;
        Ok(s)
    }
}

/// A Vulkan vertex attribute format as WebGPU's.
fn vertex_format(vk: u32) -> Option<wgpu::VertexFormat> {
    use wgpu::VertexFormat as V;
    Some(match vk {
        9 => V::Unorm8,
        10 => V::Snorm8,
        13 => V::Uint8,
        14 => V::Sint8,
        70 => V::Unorm16,
        71 => V::Snorm16,
        74 => V::Uint16,
        75 => V::Sint16,
        76 => V::Float16,
        16 => V::Unorm8x2,
        17 => V::Snorm8x2,
        20 => V::Uint8x2,
        21 => V::Sint8x2,
        37 | 51 => V::Unorm8x4,
        38 | 52 => V::Snorm8x4,
        41 | 55 => V::Uint8x4,
        42 | 56 => V::Sint8x4,
        44 => V::Unorm8x4Bgra,
        64 => V::Unorm10_10_10_2,
        77 => V::Unorm16x2,
        78 => V::Snorm16x2,
        81 => V::Uint16x2,
        82 => V::Sint16x2,
        83 => V::Float16x2,
        91 => V::Unorm16x4,
        92 => V::Snorm16x4,
        95 => V::Uint16x4,
        96 => V::Sint16x4,
        97 => V::Float16x4,
        98 => V::Uint32,
        99 => V::Sint32,
        100 => V::Float32,
        101 => V::Uint32x2,
        102 => V::Sint32x2,
        103 => V::Float32x2,
        104 => V::Uint32x3,
        105 => V::Sint32x3,
        106 => V::Float32x3,
        107 => V::Uint32x4,
        108 => V::Sint32x4,
        109 => V::Float32x4,
        _ => return None,
    })
}

fn compare(op: u32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    [C::Never, C::Less, C::Equal, C::LessEqual, C::Greater, C::NotEqual, C::GreaterEqual, C::Always].get(op as usize).copied().unwrap_or(C::Always)
}

fn blend_factor(f: u32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match f {
        0 => B::Zero,
        2 => B::Src,
        3 => B::OneMinusSrc,
        4 => B::Dst,
        5 => B::OneMinusDst,
        6 => B::SrcAlpha,
        7 => B::OneMinusSrcAlpha,
        8 => B::DstAlpha,
        9 => B::OneMinusDstAlpha,
        // (WebGPU's constant is one color: Vulkan's constant alpha is its.)
        10 | 12 => B::Constant,
        11 | 13 => B::OneMinusConstant,
        14 => B::SrcAlphaSaturated,
        15 => B::Src1,
        16 => B::OneMinusSrc1,
        17 => B::Src1Alpha,
        18 => B::OneMinusSrc1Alpha,
        _ => B::One,
    }
}

fn blend_op(op: u32) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as O;
    [O::Add, O::Subtract, O::ReverseSubtract, O::Min, O::Max].get(op as usize).copied().unwrap_or(O::Add)
}

fn stencil_face(s: &[u32; 6]) -> wgpu::StencilFaceState {
    use wgpu::StencilOperation as S;
    // (Vulkan's order: keep, zero, replace, increment and clamp, decrement
    // and clamp, invert, increment and wrap, decrement and wrap.)
    let op = |o: u32| [S::Keep, S::Zero, S::Replace, S::IncrementClamp, S::DecrementClamp, S::Invert, S::IncrementWrap, S::DecrementWrap].get(o as usize).copied().unwrap_or(S::Keep);
    wgpu::StencilFaceState { compare: compare(s[3]), fail_op: op(s[0]), depth_fail_op: op(s[2]), pass_op: op(s[1]) }
}

/// A graphics pipeline: `shader` finds a handle's shader.
pub fn graphics_pipeline<'a>(gpu: &Gpu, s: &GraphicsState, shader: impl Fn(u64) -> Option<&'a Shader>) -> Res<Pipeline> {
    let vs = shader(s.vertex.0).ok_or("no vertex shader")?;
    let vs_variant = vs.variant(gpu, &s.vertex_spec)?;
    let vs = vs_variant.as_deref().unwrap_or(vs);
    let (stage, vs_bindings) = vs.entries.get(&s.vertex.1).ok_or_else(|| format!("no entry point {}", s.vertex.1))?;
    if *stage != naga::ShaderStage::Vertex {
        return Err(format!("{} isn't a vertex shader", s.vertex.1));
    }
    let fs = if s.fragment.0 == 0 { None } else { Some(shader(s.fragment.0).ok_or("no fragment shader")?) };
    let fs_variant = match fs {
        Some(fs) => fs.variant(gpu, &s.fragment_spec)?,
        None => None,
    };
    let fs = fs_variant.as_deref().or(fs);
    let fs_bindings = match fs {
        Some(fs) => {
            let (stage, b) = fs.entries.get(&s.fragment.1).ok_or_else(|| format!("no entry point {}", s.fragment.1))?;
            if *stage != naga::ShaderStage::Fragment {
                return Err(format!("{} isn't a fragment shader", s.fragment.1));
            }
            Some(b)
        }
        None => None,
    };
    // Vertex buffers, in WebGPU's slots: one a binding, and one more for
    // each place past its stride an attribute is at (WebGPU's attributes
    // are within it; Vulkan's needn't be): the binding's buffer from there.
    let mut slots: Vec<(u32, u64, [u32; 3])> = s.buffers.iter().map(|b| (b[0], 0, *b)).collect();
    let mut attributes: Vec<Vec<wgpu::VertexAttribute>> = vec![Vec::new(); s.buffers.len()];
    for a in &s.attributes {
        let b = *s.buffers.iter().find(|b| b[0] == a[1]).ok_or_else(|| format!("an attribute of vertex binding {}, which there isn't", a[1]))?;
        let format = vertex_format(a[2]).ok_or_else(|| format!("vertex format {}", a[2]))?;
        let (offset, stride) = (a[3] as u64, b[1] as u64);
        let from = if stride != 0 && offset + format.size() > stride { offset & !3 } else { 0 };
        let slot = match slots.iter().position(|s| s.0 == a[1] && s.1 == from) {
            Some(k) => k,
            None => {
                slots.push((a[1], from, b));
                attributes.push(Vec::new());
                slots.len() - 1
    }
        };
        attributes[slot].push(wgpu::VertexAttribute { format, offset: offset - from, shader_location: a[0] });
    }
    let buffers: Vec<Option<wgpu::VertexBufferLayout>> = slots
        .iter()
        .zip(&attributes)
        .map(|((_, _, b), a)| {
            Some(wgpu::VertexBufferLayout {
                array_stride: b[1] as u64,
                step_mode: if b[2] == 1 { wgpu::VertexStepMode::Instance } else { wgpu::VertexStepMode::Vertex },
                attributes: a,
            })
        })
        .collect();
    use wgpu::PrimitiveTopology as T;
    let topology = match s.topology {
        0 => T::PointList,
        1 => T::LineList,
        2 => T::LineStrip,
        3 => T::TriangleList,
        4 => T::TriangleStrip,
        // (A fan's draws are a list's: see `Pipeline::fan`.)
        5 => T::TriangleList,
        t => return Err(format!("topology {t} (WebGPU has no adjacency or patches)")),
    };
    let cull_mode = match s.cull {
        1 => Some(wgpu::Face::Front),
        2 => Some(wgpu::Face::Back),
        3 => None,
        _ => None,
    };
    let depth_stencil = if s.depth_format == 0 {
        None
    } else {
        let f = format(s.depth_format).ok_or_else(|| format!("format {}", s.depth_format))?;
        let (test, write, op) = s.depth;
        let (stencil_test, front, back) = &s.stencil;
        Some(wgpu::DepthStencilState {
            format: f,
            depth_write_enabled: Some(test && write),
            depth_compare: Some(if test { compare(op) } else { wgpu::CompareFunction::Always }),
            stencil: if *stencil_test {
                wgpu::StencilState { front: stencil_face(front), back: stencil_face(back), read_mask: front[4], write_mask: front[5] }
            } else {
                wgpu::StencilState::default()
            },
            bias: if s.bias.0 { wgpu::DepthBiasState { constant: s.bias.1 as i32, slope_scale: s.bias.2, clamp: s.bias.3 } } else { Default::default() },
        })
    };
    let targets: Vec<Option<wgpu::ColorTargetState>> = s
        .colors
        .iter()
        .map(|c| {
            let f = format(c[0]).ok_or_else(|| format!("format {}", c[0]))?;
            let blend = (c[1] != 0).then(|| wgpu::BlendState {
                color: wgpu::BlendComponent { src_factor: blend_factor(c[2]), dst_factor: blend_factor(c[3]), operation: blend_op(c[4]) },
                alpha: wgpu::BlendComponent { src_factor: blend_factor(c[5]), dst_factor: blend_factor(c[6]), operation: blend_op(c[7]) },
            });
            Ok(Some(wgpu::ColorTargetState { format: f, blend, write_mask: wgpu::ColorWrites::from_bits_truncate(c[8]) }))
        })
        .collect::<Res<_>>()?;
    // A layout of its own, made from both stages: wgpu's derived layouts
    // refuse a binding the stages declare differently (a block of which each
    // declares the members it uses: their minimum sizes differ).
    let layout = {
        let mut stages = vec![(vs, vs_bindings, wgpu::ShaderStages::VERTEX)];
        if let (Some(fs), Some(b)) = (fs, fs_bindings) {
            stages.push((fs, b, wgpu::ShaderStages::FRAGMENT));
        }
        explicit_layout(gpu, &stages)?
    };
    let recipe = Recipe {
        layout,
        vs: (vs.module.clone(), s.vertex.1.clone(), vs.constants(&s.vertex_spec).into_iter().collect()),
        vs_source: vs.source.clone(),
        vs_unflipped: Default::default(),
        fs: fs.map(|fs| (fs.module.clone(), s.fragment.1.clone(), fs.constants(&s.fragment_spec).into_iter().collect())),
        buffers: buffers.iter().flatten().map(|b| (b.array_stride, b.step_mode, b.attributes.to_vec())).collect(),
        primitive: wgpu::PrimitiveState {
            topology,
            strip_index_format: None,
            front_face: if s.front_face == 1 { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
            cull_mode,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil,
        multisample: wgpu::MultisampleState { count: s.samples.max(1), mask: !0, alpha_to_coverage_enabled: false },
        targets,
    };
    let pipeline = recipe.make(gpu, None, Dynamic::default())?;
    let mut stages = vec![(vs, vs_bindings)];
    if let (Some(fs), Some(b)) = (fs, fs_bindings) {
        stages.push((fs, b));
    }
    let (uses, push_group, push_bindings) = uses_of(&stages);
    let written = written_of(&stages);
    let arrays = vs.arrays.iter().chain(fs.iter().flat_map(|f| f.arrays.iter())).map(|(k, v)| (*k, *v)).collect();
    let mut sizes = vs.sizes.clone();
    for (&k, &v) in fs.iter().flat_map(|f| f.sizes.iter()) {
        let size = sizes.entry(k).or_insert(v);
        *size = (*size).max(v);
    }
    Ok(Pipeline {
        kind: Kind::Render(pipeline),
        uses,
        push_group,
        push_bindings,
        vertex_bindings: slots.iter().map(|s| (s.0, s.1)).collect(),
        arrays,
        sizes,
        recipe: Some(recipe),
        dynamic: s.dynamic,
        fan: s.topology == 5,
        culls_all: s.cull == 3 && (3..=5).contains(&s.topology),
        written,
        variants: Default::default(),
    })
}

/// A descriptor: what binding `binding` of its set is.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub binding: u32,
    /// Which element of a descriptor array (0 for a single descriptor).
    pub element: u32,
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
    /// A texel buffer's view (uniform or storage texel buffer).
    Texel(u64),
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
    /// A render pass: its color attachments and depth/stencil attachment;
    /// the commands up to EndPass are its.
    BeginPass { colors: Vec<Attachment>, depth: Option<DepthAttachment>, views: u32 },
    EndPass,
    /// x, y, width, height, minimum and maximum depth.
    SetViewport([f32; 6]),
    /// x, y, width, height.
    SetScissor([u32; 4]),
    /// Vertex buffers from Vulkan binding `first` on: buffer, offset.
    BindVertex { first: u32, buffers: Vec<(u64, u64)> },
    /// `wide`: 32-bit indices, else 16.
    BindIndex { buffer: u64, offset: u64, wide: bool },
    /// Vertices, instances, first vertex, first instance.
    Draw([u32; 4]),
    DrawIndexed { indices: u32, instances: u32, first: u32, vertex_offset: i32, first_instance: u32 },
    DrawIndirect { buffer: u64, offset: u64, count: u32, stride: u32, indexed: bool },
    SetBlendConstants([f32; 4]),
    SetStencilReference(u32),
    /// A descriptor set bound for graphics (BindSet's are compute's).
    BindGraphicsSet { set: u32, bindings: Vec<Binding> },
    /// A region of one image's mip level and layer scaled into another's
    /// (corners: x0, y0, x1, y1), linearly filtered or not.
    Blit { src: u64, src_level: [u32; 2], from: [i32; 4], dst: u64, dst_level: [u32; 2], to: [i32; 4], linear: bool },
    /// An image's depth and stencil (`aspect`: VkImageAspectFlags) cleared.
    ClearDepth { image: u64, mip: u32, mips: u32, layer: u32, layers: u32, aspect: u32, depth: f32, stencil: u32 },
    /// A multisampled image's mip level and layers resolved into another's.
    Resolve { src: u64, src_level: [u32; 2], dst: u64, dst_level: [u32; 2], layers: u32 },
    /// Inside a render pass: attachments cleared within rectangles.
    ClearAttachments { attachments: Vec<Clear>, rects: Vec<[u32; 4]> },
    /// An occlusion query begun and ended in a render pass.
    BeginQuery { pool: u64, index: u32 },
    EndQuery { pool: u64, index: u32 },
    ResetQueries { pool: u64, first: u32, count: u32 },
    Timestamp { pool: u64, index: u32 },
    /// Queries' results into a buffer, `stride` apart (`flags`:
    /// VkQueryResultFlags: 1 64-bit, 4 with each its availability).
    CopyQueries { pool: u64, first: u32, count: u32, buffer: u64, offset: u64, stride: u64, flags: u32 },
    /// Dynamic state WebGPU makes part of a pipeline: depth bias (constant,
    /// clamp, slope: f32s' bits), stencil compare and write masks.
    SetDepthBias([u32; 3]),
    SetStencilCompareMask(u32),
    SetStencilWriteMask(u32),
}

/// An attachment cleared inside a render pass: a color attachment (its
/// index in the subpass) to a color's bits, or depth and stencil (`aspect`,
/// VkImageAspectFlags) to a depth and a stencil value.
#[derive(Clone, Debug, PartialEq)]
pub struct Clear {
    pub aspect: u32,
    pub index: u32,
    pub value: [u32; 4],
}

/// A render pass's color attachment: a view, VkAttachmentLoadOp and
/// VkAttachmentStoreOp, and the clear color's bits.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub view: u64,
    pub load: u32,
    pub store: u32,
    pub clear: [u32; 4],
    /// The view it's resolved into (0: none).
    pub resolve: u64,
    /// Multiview's views its load op is for (those that use it first).
    pub firsts: u32,
}

/// A depth/stencil attachment: its view, depth's load and store ops,
/// stencil's, and the clear values.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthAttachment {
    pub view: u64,
    pub depth: (u32, u32),
    pub stencil: (u32, u32),
    pub clear_depth: f32,
    pub clear_stencil: u32,
    /// Multiview's views its load ops are for.
    pub firsts: u32,
}

/// A command buffer's commands from the stream the guest recorded: each a
/// u32 operation and its length in bytes (with these 8), then its operands,
/// little-endian:
///
///   1 BIND_PIPELINE  pipeline (u64)
///   2 BIND_SET       set, count (u32), then count × binding, kind (u32:
///                    0 a buffer, 1 a view, 2 a sampler, 3 a view and
///                    sampler, 4 a texel buffer view), and three u64s:
///                    buffer, offset, size; or view; or sampler; or view,
///                    sampler; or texel buffer view
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
///  13 BEGIN_PASS     colors, depth (u32: 1 if there is), each color's view
///                    (u64), load and store ops (u32), clear color (u32 × 4),
///                    the view it's resolved into (u64, 0 if none), the views
///                    its load op is for (u32: multiview's, those that use it
///                    first), and the depth's view (u64), depth's and
///                    stencil's load and store ops, clear depth (f32) and
///                    stencil (u32), and the views its load ops are for;
///                    then the views (multiview's view mask, 0: none)
///  14 END_PASS
///  15 SET_VIEWPORT   x, y, width, height, minimum and maximum depth (f32)
///  16 SET_SCISSOR    x, y, width, height (u32)
///  17 BIND_VERTEX    first binding, count (u32), count × buffer, offset (u64)
///  18 BIND_INDEX     buffer, offset (u64), VkIndexType (u32)
///  19 DRAW           vertices, instances, first vertex, first instance (u32)
///  20 DRAW_INDEXED   indices, instances, first index, vertex offset (i32),
///                    first instance
///  21 DRAW_INDIRECT  buffer, offset (u64), count, stride (u32)
///  22 DRAW_INDEXED_INDIRECT  likewise
///  23 SET_BLEND_CONSTANTS  (f32 × 4)
///  24 SET_STENCIL_REFERENCE  (u32)
///  25 BIND_GRAPHICS_SET  as BIND_SET, for graphics
///  26 BLIT           source, destination (u64), then u32s: the source's mip
///                    level, layer, corners (i32 × 4: x0, y0, x1, y1); the
///                    destination's likewise; VkFilter
///  27 CLEAR_DEPTH    image (u64), mip, mips, layer, layers, aspect (u32),
///                    depth (f32), stencil (u32)
///  28 RESOLVE        source, destination (u64), the source's mip level and
///                    layer, the destination's, layers (u32)
///  29 CLEAR_ATTACHMENTS  attachments, rectangles (u32), each attachment's
///                    aspect, index and value (u32 × 4: a color's bits, or
///                    depth and stencil), each rectangle's x, y, width,
///                    height, layer, layers (u32)
///  30 BEGIN_QUERY    pool (u64), index, VkQueryControlFlags (u32)
///  31 END_QUERY      pool (u64), index (u32)
///  32 RESET_QUERIES  pool (u64), first, count (u32)
///  33 TIMESTAMP      pool (u64), index (u32)
///  34 COPY_QUERIES   pool (u64), first, count (u32), buffer, offset, stride
///                    (u64), VkQueryResultFlags (u32)
///  35 SET_DEPTH_BIAS constant, clamp, slope (f32)
///  36 SET_STENCIL_COMPARE_MASK  mask (u32)
///  37 SET_STENCIL_WRITE_MASK  mask (u32)
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
            2 | 25 => {
                let (set, n) = (r.u32()?, r.u32()?);
                let bindings = (0..n)
                    .map(|_| {
                        // (The kind's low byte; above it, an array's element.)
                        let (binding, kind) = (r.u32()?, r.u32()?);
                        let (kind, element) = (kind & 0xff, kind >> 8);
                        let (a, b, c) = (r.u64()?, r.u64()?, r.u64()?);
                        let resource = match kind {
                            0 => Resource::Buffer { buffer: a, offset: b, size: c },
                            1 => Resource::View(a),
                            2 => Resource::Sampler(a),
                            3 => Resource::Combined { view: a, sampler: b },
                            4 => Resource::Texel(a),
                            k => return Err(format!("a descriptor of kind {k}")),
                        };
                        Ok(Binding { binding, element, resource })
                    })
                    .collect::<Res<_>>()?;
                if op == 2 { Command::BindSet { set, bindings } } else { Command::BindGraphicsSet { set, bindings } }
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
            13 => {
                let (n, has_depth) = (r.u32()?, r.u32()? != 0);
                let colors = (0..n)
                    .map(|_| Ok(Attachment { view: r.u64()?, load: r.u32()?, store: r.u32()?, clear: [r.u32()?, r.u32()?, r.u32()?, r.u32()?], resolve: r.u64()?, firsts: r.u32()? }))
                    .collect::<Res<_>>()?;
                let depth = if has_depth {
                    Some(DepthAttachment { view: r.u64()?, depth: (r.u32()?, r.u32()?), stencil: (r.u32()?, r.u32()?), clear_depth: f32::from_bits(r.u32()?), clear_stencil: r.u32()?, firsts: r.u32()? })
                } else {
                    None
                };
                Command::BeginPass { colors, depth, views: r.u32()? }
            }
            14 => Command::EndPass,
            15 => Command::SetViewport([r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
            16 => Command::SetScissor([r.u32()?, r.u32()?, r.u32()?, r.u32()?]),
            17 => {
                let (first, n) = (r.u32()?, r.u32()?);
                Command::BindVertex { first, buffers: (0..n).map(|_| Ok((r.u64()?, r.u64()?))).collect::<Res<_>>()? }
            }
            18 => Command::BindIndex { buffer: r.u64()?, offset: r.u64()?, wide: r.u32()? == 1 },
            19 => Command::Draw([r.u32()?, r.u32()?, r.u32()?, r.u32()?]),
            20 => Command::DrawIndexed { indices: r.u32()?, instances: r.u32()?, first: r.u32()?, vertex_offset: r.u32()? as i32, first_instance: r.u32()? },
            21 | 22 => Command::DrawIndirect { buffer: r.u64()?, offset: r.u64()?, count: r.u32()?, stride: r.u32()?, indexed: op == 22 },
            23 => Command::SetBlendConstants([r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
            24 => Command::SetStencilReference(r.u32()?),
            26 => {
                let (src, dst) = (r.u64()?, r.u64()?);
                let mut w = [0u32; 13];
                for v in &mut w {
                    *v = r.u32()?;
                }
                let corners = |k: usize| [w[k] as i32, w[k + 1] as i32, w[k + 2] as i32, w[k + 3] as i32];
                Command::Blit { src, src_level: [w[0], w[1]], from: corners(2), dst, dst_level: [w[6], w[7]], to: corners(8), linear: w[12] == 1 }
            }
            27 => Command::ClearDepth { image: r.u64()?, mip: r.u32()?, mips: r.u32()?, layer: r.u32()?, layers: r.u32()?, aspect: r.u32()?, depth: r.f32()?, stencil: r.u32()? },
            28 => {
                let (src, dst) = (r.u64()?, r.u64()?);
                Command::Resolve { src, src_level: [r.u32()?, r.u32()?], dst, dst_level: [r.u32()?, r.u32()?], layers: r.u32()? }
            }
            29 => {
                let (n, rects) = (r.u32()?, r.u32()?);
                let attachments = (0..n).map(|_| Ok(Clear { aspect: r.u32()?, index: r.u32()?, value: [r.u32()?, r.u32()?, r.u32()?, r.u32()?] })).collect::<Res<_>>()?;
                // (Layers aside: a pass's attachments are views of one.)
                let rects = (0..rects)
                    .map(|_| {
                        let rect = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
                        let (_, _) = (r.u32()?, r.u32()?);
                        Ok(rect)
                    })
                    .collect::<Res<_>>()?;
                Command::ClearAttachments { attachments, rects }
            }
            30 => {
                let (pool, index, _) = (r.u64()?, r.u32()?, r.u32()?);
                Command::BeginQuery { pool, index }
            }
            31 => Command::EndQuery { pool: r.u64()?, index: r.u32()? },
            32 => Command::ResetQueries { pool: r.u64()?, first: r.u32()?, count: r.u32()? },
            33 => Command::Timestamp { pool: r.u64()?, index: r.u32()? },
            34 => Command::CopyQueries { pool: r.u64()?, first: r.u32()?, count: r.u32()?, buffer: r.u64()?, offset: r.u64()?, stride: r.u64()?, flags: r.u32()? },
            35 => Command::SetDepthBias([r.u32()?, r.u32()?, r.u32()?]),
            36 => Command::SetStencilCompareMask(r.u32()?),
            37 => Command::SetStencilWriteMask(r.u32()?),
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
    fn f32(&mut self) -> Res<f32> {
        Ok(f32::from_bits(self.u32()?))
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
        // (A8B8G8R8_*_PACK32: a little-endian word's bytes are RGBA8's.)
        51 => F::Rgba8Unorm,
        52 => F::Rgba8Snorm,
        55 => F::Rgba8Uint,
        56 => F::Rgba8Sint,
        57 => F::Rgba8UnormSrgb,
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
        // Block-compressed (BC1 to BC7), where the device has them; BC1's RGB
        // forms as its RGBA ones.
        131 | 133 => F::Bc1RgbaUnorm,
        132 | 134 => F::Bc1RgbaUnormSrgb,
        135 => F::Bc2RgbaUnorm,
        136 => F::Bc2RgbaUnormSrgb,
        137 => F::Bc3RgbaUnorm,
        138 => F::Bc3RgbaUnormSrgb,
        139 => F::Bc4RUnorm,
        140 => F::Bc4RSnorm,
        141 => F::Bc5RgUnorm,
        142 => F::Bc5RgSnorm,
        143 => F::Bc6hRgbUfloat,
        144 => F::Bc6hRgbFloat,
        145 => F::Bc7RgbaUnorm,
        146 => F::Bc7RgbaUnormSrgb,
        _ => return None,
    })
}

/// What the device does with a format, as Vulkan's VkFormatFeatureFlags
/// for optimal tiling: WebGPU's guarantees for it (the same on every
/// device), and the texel's size in bytes (0: not a format it has).
pub fn format_features(gpu: &Gpu, vk: u32) -> (u32, u32, u32) {
    // (Vertex attributes' formats: VERTEX_BUFFER; some are only that.)
    let vertex = if vertex_format(vk).is_some() { 0x40 } else { 0 };
    let Some(f) = format(vk) else {
        return (vertex, 0, 0);
    };
    if !gpu.device.features().contains(f.required_features()) {
        return (vertex, 0, 0);
    }
    let ff = gpu.format_features(f);
    let (u, flags) = (ff.allowed_usages, ff.flags);
    let depth = f.is_depth_stencil_format();
    let mut v = vertex;
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
        if !depth {
            v |= 0x800; // BLIT_DST (drawn into)
        }
    }
    if u.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
        v |= 0x400; // BLIT_SRC (sampled, or fetched from: integers' nearest)
    }
    if u.contains(wgpu::TextureUsages::COPY_SRC) {
        v |= 0x4000; // TRANSFER_SRC
    }
    if u.contains(wgpu::TextureUsages::COPY_DST) {
        v |= 0x8000; // TRANSFER_DST
    }
    // (Its sample counts, as VkSampleCountFlags: four where it multisamples.)
    let samples = if flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4) { 0x1 | 0x4 } else { 0x1 };
    (v, f.block_copy_size(None).or_else(|| f.block_copy_size(Some(wgpu::TextureAspect::DepthOnly))).unwrap_or(0), samples)
}

/// An image: `dims` 1, 2 or 3, Vulkan's format and usage
/// (VkImageUsageFlags), its extent, mip levels, layers and samples.
pub fn image(gpu: &Gpu, dims: u32, vk_format: u32, extent: [u32; 3], mips: u32, layers: u32, samples: u32, usage: u32) -> Res<wgpu::Texture> {
    let f = format(vk_format).ok_or_else(|| format!("format {vk_format}"))?;
    let allowed = gpu.format_features(f).allowed_usages;
    // (Copies whenever the format allows: Vulkan's transfers.)
    let mut u = (wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST) & allowed;
    if usage & (0x4 | 0x80) != 0 {
        u |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    // (A transfer's source, sampled too where it can be: blits draw from it.)
    if usage & 0x1 != 0 {
        u |= wgpu::TextureUsages::TEXTURE_BINDING & allowed;
    }
    if usage & 0x8 != 0 && samples <= 1 {
        u |= wgpu::TextureUsages::STORAGE_BINDING;
    }
    // (Clears are render passes: attachments whenever the format can be.)
    u |= allowed & wgpu::TextureUsages::RENDER_ATTACHMENT;
    if !allowed.contains(u) {
        return Err(format!("{f:?} images can't be used so ({:?})", u - allowed));
    }
    // (A 1D image is 2D, a row high: WebGPU's 1D textures have no levels,
    // layers or attachments; the shaders' 1D images are made 2D too,
    // spirv/images1d.rs.)
    let (dimension, depth_or_array_layers) = match dims {
        3 => (wgpu::TextureDimension::D3, extent[2]),
        _ => (wgpu::TextureDimension::D2, layers),
    };
    // (Viewable as its sRGB or plain counterpart: what WebGPU lets a
    // texture's views be of Vulkan's mutable formats.)
    let other = if f.is_srgb() { f.remove_srgb_suffix() } else { f.add_srgb_suffix() };
    let views = if other != f { vec![other] } else { Vec::new() };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: extent[0], height: extent[1], depth_or_array_layers },
        mip_level_count: mips.max(1),
        sample_count: samples.max(1),
        dimension,
        format: f,
        usage: u,
        view_formats: &views,
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("image: {}", describe(&e)));
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

/// An image view, and its format (an attachment's clear color is in it).
pub struct View {
    pub view: wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    /// Its first mip level's width and height.
    pub size: [u32; 2],
    /// A view of every aspect, where this one shows some (a depth-only view
    /// of a depth and stencil image): WebGPU's attachments show them all.
    pub attachment: Option<wgpu::TextureView>,
    /// Its image's samples.
    pub samples: u32,
    /// Its first mip level and layer.
    pub base: [u32; 2],
    /// Whether it's of a 2D array's layers.
    pub array: bool,
}

/// A view of an image: Vulkan's view type (VkImageViewType), its format
/// (0: the image's), aspect, and mip levels and layers.
pub fn view(t: &wgpu::Texture, kind: u32, vk_format: u32, aspect_bits: u32, mip: u32, mips: u32, layer: u32, layers: u32) -> Res<View> {
    use wgpu::TextureViewDimension as D;
    // (1D views 2D, as their images are.)
    let dimension = match kind {
        2 => D::D3,
        3 => D::Cube,
        4 | 5 => D::D2Array,
        6 => D::CubeArray,
        _ => D::D2,
    };
    let format = if vk_format == 0 { None } else { Some(format(vk_format).ok_or_else(|| format!("format {vk_format}"))?) };
    let view = t.create_view(&wgpu::TextureViewDescriptor {
        label: None,
        format,
        dimension: Some(dimension),
        usage: None,
        aspect: aspect(aspect_bits),
        base_mip_level: mip,
        mip_level_count: Some(mips),
        base_array_layer: layer,
        array_layer_count: if dimension == D::D3 { None } else { Some(layers) },
    });
    let size = [(t.width() >> mip).max(1), (t.height() >> mip).max(1)];
    let attachment = (aspect(aspect_bits) != wgpu::TextureAspect::All && t.format().is_combined_depth_stencil_format()).then(|| {
        t.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(dimension),
            base_mip_level: mip,
            mip_level_count: Some(mips),
            base_array_layer: layer,
            array_layer_count: if dimension == D::D3 { None } else { Some(layers) },
            ..Default::default()
        })
    });
    Ok(View { view, format: format.unwrap_or(t.format()), size, attachment, samples: t.sample_count(), base: [mip, layer], array: dimension == D::D2Array })
}

/// A query pool (VkQueryPool): WebGPU's query set, occlusion or timestamps,
/// the buffer its queries are resolved into, and their values as last read
/// back (None: not available, never ended or reset since). A submission
/// reads back the queries it ends once it's done.
pub struct QueryPool {
    pub set: wgpu::QuerySet,
    pub count: u32,
    resolved: wgpu::Buffer,
    /// For occlusion queries begun outside a render pass (WebGPU's are of
    /// one pass): a set each pass while one is on counts into, summed as
    /// it ends, and the buffer it's resolved into.
    spill: Option<(wgpu::QuerySet, wgpu::Buffer)>,
    pub values: Mutex<Vec<Option<u64>>>,
}

/// Render passes an occlusion query begun outside one may cover, in a
/// submission.
const SPILL: u32 = 256;

/// Whether the device takes timestamps anywhere in a submission.
pub fn timestamps(gpu: &Gpu) -> bool {
    gpu.device.features().contains(wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES)
}

/// A query pool of `count` queries: VkQueryType 0 occlusion, 2 timestamps.
pub fn query_pool(gpu: &Gpu, kind: u32, count: u32) -> Res<QueryPool> {
    let ty = match kind {
        0 => wgpu::QueryType::Occlusion,
        2 if timestamps(gpu) => wgpu::QueryType::Timestamp,
        k => return Err(format!("queries of type {k}")),
    };
    if count == 0 || count > wgpu::QUERY_SET_MAX_QUERIES {
        return Err(format!("a pool of {count} queries"));
    }
    let set = gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty, count });
    let resolved = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: count as u64 * 8,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let spill = matches!(ty, wgpu::QueryType::Occlusion).then(|| {
        let set = gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty, count: SPILL });
        let resolved = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: SPILL as u64 * 8, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        (set, resolved)
    });
    Ok(QueryPool { set, count, resolved, spill, values: Mutex::new(vec![None; count as usize]) })
}

/// A texel buffer's view (VkBufferView): a 2D texture of its format, its
/// texels `TEXEL_ROW` a row (WGSL has no texel buffers: the shader's buffer
/// images are made 2D, spirv.rs), copied from its range of the buffer
/// before each pass or dispatch that uses it (`refresh`).
pub struct TexelView {
    pub buffer: u64,
    pub offset: u64,
    pub texels: u32,
    pub texture: wgpu::Texture,
    pub view: View,
}

/// A view of `range` bytes of a buffer (`buffer`, a handle the submission
/// will find) from `offset`, as Vulkan's format `vk_format`.
pub fn texel_view(gpu: &Gpu, buffer: u64, offset: u64, range: u64, vk_format: u32) -> Res<TexelView> {
    let f = format(vk_format).ok_or_else(|| format!("format {vk_format}"))?;
    let size = f.block_copy_size(None).ok_or_else(|| format!("{f:?} texel buffers"))? as u64;
    let texels = (range / size).max(1);
    let row = crate::spirv::TEXEL_ROW as u64;
    let max = row * gpu.device.limits().max_texture_dimension_2d as u64;
    if texels > max {
        return Err(format!("a texel buffer view of {texels} texels (at most {max})"));
    }
    let (width, height) = (texels.min(row) as u32, texels.div_ceil(row) as u32);
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: f,
        // (A storage texel buffer too, where its format can be one: written,
        // it's copied back, `write_back`.)
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | (wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC) & gpu.format_features(f).allowed_usages,
        view_formats: &[],
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("texel buffer view: {}", describe(&e)));
    }
    let view = View { view: texture.create_view(&Default::default()), format: f, size: [width, height], attachment: None, samples: 1, base: [0, 0], array: false };
    Ok(TexelView { buffer, offset, texels: texels as u32, texture, view })
}

/// A texel view's rows, as copies: where in the buffer each starts, its
/// first row, how many rows, and how wide (the whole rows, then what's
/// left over).
fn texel_rows(t: &TexelView) -> Vec<(u64, u32, u32, u32)> {
    let row = crate::spirv::TEXEL_ROW;
    let size = t.texture.format().block_copy_size(None).unwrap_or(1) as u64;
    let (whole, rest) = (t.texels / row, t.texels % row);
    let mut copies = Vec::new();
    if whole > 0 || t.texels < row {
        copies.push((t.offset, 0, whole.max(1), t.texels.min(row)));
    }
    if rest > 0 && whole > 0 {
        copies.push((t.offset + (whole * row) as u64 * size, whole, 1, rest));
    }
    copies
}

fn texel_copy(t: &TexelView, row: u32, width: u32) -> (wgpu::TexelCopyBufferLayout, wgpu::TexelCopyTextureInfo<'_>, wgpu::Extent3d) {
    let size = t.texture.format().block_copy_size(None).unwrap_or(1);
    (
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(crate::spirv::TEXEL_ROW * size), rows_per_image: None },
        wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: row, z: 0 }, aspect: wgpu::TextureAspect::All },
        wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
    )
}

/// A texel view's texture brought up to date with its buffer.
fn refresh(enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, t: &TexelView) {
    for (offset, first, rows, width) in texel_rows(t) {
        let (mut layout, texture, mut extent) = texel_copy(t, first, width);
        layout.offset = offset;
        extent.height = rows;
        enc.copy_buffer_to_texture(wgpu::TexelCopyBufferInfo { buffer, layout }, texture, extent);
    }
}

/// A storage texel view's texture written back to its buffer (after a
/// dispatch or a pass that may have written it).
fn write_back(enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, t: &TexelView) {
    if t.texture.usage().contains(wgpu::TextureUsages::STORAGE_BINDING) {
        for (offset, first, rows, width) in texel_rows(t) {
            let (mut layout, texture, mut extent) = texel_copy(t, first, width);
            layout.offset = offset;
            extent.height = rows;
            enc.copy_texture_to_buffer(texture, wgpu::TexelCopyBufferInfo { buffer, layout }, extent);
        }
    }
}

/// The texel views among descriptors.
fn texels_of<'b>(bindings: impl Iterator<Item = &'b Binding>) -> Vec<u64> {
    bindings.filter_map(|b| if let Resource::Texel(t) = b.resource { Some(t) } else { None }).collect()
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
    Queries(&'a QueryPool),
    Texture(&'a wgpu::Texture),
    View(&'a View),
    Sampler(&'a wgpu::Sampler),
    Pipeline(Arc<Pipeline>),
    Texel(&'a TexelView),
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
fn buffer_image(gpu: &Gpu, enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, t: &wgpu::Texture, r: &Region, to_image: bool) -> Res<()> {
    let (place, extent) = on_texture(t, &r.sub);
    // (24-bit depth, which WebGPU won't copy, read by a compute shader.)
    if !to_image && t.format().block_copy_size(Some(place.aspect)).is_none() && matches!(t.format(), wgpu::TextureFormat::Depth24Plus | wgpu::TextureFormat::Depth24PlusStencil8) {
        return depth24_to_buffer(gpu, enc, buffer, t, r);
    }
    let block = t.format().block_copy_size(Some(place.aspect)).ok_or("an image whose texels can't be copied")?;
    // (Rows and their lengths in blocks: a compressed format's are of
    // several texels.)
    let (bw, bh) = t.format().block_dimensions();
    let row_length = if r.row_length == 0 { extent.width } else { r.row_length };
    let rows = (if r.image_height == 0 { extent.height } else { r.image_height }).div_ceil(bh);
    let row_bytes = row_length.div_ceil(bw) * block;
    let mut copy = |offset: u64, place: wgpu::TexelCopyTextureInfo, extent: wgpu::Extent3d, bytes_per_row: Option<u32>| {
        let b = wgpu::TexelCopyBufferInfo { buffer, layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row, rows_per_image: bytes_per_row.map(|_| rows) } };
        if to_image { enc.copy_buffer_to_texture(b, place, extent) } else { enc.copy_texture_to_buffer(place, b, extent) }
    };
    if extent.height <= bh && extent.depth_or_array_layers <= 1 {
        // (One row: no pitch, which needn't then be WebGPU's 256 bytes.)
        copy(r.offset, place, extent, None);
    } else if row_bytes.is_multiple_of(256) {
        copy(r.offset, place, extent, Some(row_bytes));
    } else if t.format().is_depth_stencil_format() {
        // (Depth and stencil go whole, not a row at a time: through a
        // buffer of WebGPU's pitch, the rows moved to and from Vulkan's.)
        let tight = extent.width * block;
        // (Rows moved a word at a time, or, where they aren't whole words,
        // a byte at a time by a compute shader: stencil's.)
        let aligned = tight.is_multiple_of(4) && row_bytes.is_multiple_of(4) && r.offset.is_multiple_of(4);
        let pitch = tight.next_multiple_of(256);
        let planes = extent.height as u64 * extent.depth_or_array_layers as u64;
        let tmp = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: pitch as u64 * planes,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let staged = wgpu::TexelCopyBufferInfo { buffer: &tmp, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(extent.height) } };
        let row_at = |z: u32, y: u32| r.offset + (z as u64 * rows as u64 + y as u64) * row_bytes as u64;
        let rows_of = || (0..extent.depth_or_array_layers).flat_map(|z| (0..extent.height).map(move |y| (z, y)));
        let row = |enc: &mut wgpu::CommandEncoder, src: &wgpu::Buffer, from: u64, dst: &wgpu::Buffer, to: u64| {
            if aligned {
                enc.copy_buffer_to_buffer(src, from, dst, to, tight as u64);
            } else {
                copy_bytes(gpu, enc, src, from, dst, to, tight as u64);
            }
        };
        if to_image {
            for (z, y) in rows_of() {
                row(enc, buffer, row_at(z, y), &tmp, (z as u64 * extent.height as u64 + y as u64) * pitch as u64);
            }
            enc.copy_buffer_to_texture(staged, place, extent);
        } else {
            enc.copy_texture_to_buffer(place, staged, extent);
            for (z, y) in rows_of() {
                row(enc, &tmp, (z as u64 * extent.height as u64 + y as u64) * pitch as u64, buffer, row_at(z, y));
            }
        }
    } else {
        for z in 0..extent.depth_or_array_layers {
            for y in 0..extent.height.div_ceil(bh) {
                let offset = r.offset + (z as u64 * rows as u64 + y as u64) * row_bytes as u64;
                let mut p = place;
                p.origin.y += y * bh;
                p.origin.z += z;
                let height = (extent.height - y * bh).min(bh);
                copy(offset, p, wgpu::Extent3d { width: extent.width, height, depth_or_array_layers: 1 }, None);
            }
        }
    }
    Ok(())
}

/// A 24-bit depth image's region read into a buffer, as Vulkan lays it
/// out (X8_D24_UNORM_PACK32: the depth in a word's low 24 bits): each
/// texel loaded by a compute shader into a buffer of the region's rows,
/// which go to Vulkan's.
fn depth24_to_buffer(gpu: &Gpu, enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, t: &wgpu::Texture, r: &Region) -> Res<()> {
    const DEPTH24: &str = "
struct P { x: u32, y: u32, layer: u32, mip: u32, width: u32, height: u32 }
@group(0) @binding(0) var depth: texture_depth_2d_array;
@group(0) @binding(1) var<storage, read_write> out: array<u32>;
@group(0) @binding(2) var<uniform> p: P;
@compute @workgroup_size(8, 8, 1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.width || id.y >= p.height { return; }
    let d = textureLoad(depth, vec2(p.x + id.x, p.y + id.y), p.layer + id.z, p.mip);
    out[(id.z * p.height + id.y) * p.width + id.x] = u32(round(clamp(d, 0.0, 1.0) * 16777215.0));
}
";
    let s = &r.sub;
    if t.dimension() != wgpu::TextureDimension::D2 || t.sample_count() > 1 || !t.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING) {
        return Err(format!("reading a {:?} image's depth", t.format()));
    }
    let pipeline = gpu
        .depth24
        .lock()
        .unwrap()
        .get_or_insert_with(|| {
            let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(DEPTH24.into()) });
            gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        })
        .clone();
    let (w, h, layers) = (s.extent[0], s.extent[1], s.layers.max(1));
    let words = w as u64 * h as u64 * layers as u64;
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: words * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let p: Vec<u8> = [s.origin[0] as u32, s.origin[1] as u32, s.layer, s.mip, w, h, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let uniform = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: &p, usage: wgpu::BufferUsages::UNIFORM });
    let view = t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), aspect: wgpu::TextureAspect::DepthOnly, ..Default::default() });
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
        ],
    });
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), layers);
    }
    // (Its rows, to Vulkan's places for them.)
    let row_length = if r.row_length == 0 { w } else { r.row_length } as u64;
    let rows = if r.image_height == 0 { h } else { r.image_height } as u64;
    for z in 0..layers as u64 {
        for y in 0..h as u64 {
            enc.copy_buffer_to_buffer(&out, (z * h as u64 + y) * w as u64 * 4, buffer, r.offset + (z * rows + y) * row_length * 4, w as u64 * 4);
        }
    }
    Ok(())
}

/// The bind groups for a pipeline's dispatch or draw: the bindings its
/// entry points use, from the sets bound, and its push constants (a
/// buffer `staged` makes of their bytes).
fn bind_groups<'a>(
    gpu: &Gpu,
    p: &Pipeline,
    sets: &BTreeMap<u32, Vec<Binding>>,
    push: &[u8],
    view: u32,
    object: &impl Fn(u64) -> Option<Object<'a>>,
    staged: &impl Fn(&[u8]) -> wgpu::Buffer,
) -> Res<Vec<(u32, wgpu::BindGroup)>> {
    // A descriptor's resource (a combined image sampler's image or sampler).
    enum One<'r> {
        Buffer(wgpu::BufferBinding<'r>),
        View(&'r wgpu::TextureView),
        Sampler(&'r wgpu::Sampler),
    }
    let one = |g: u32, w: u32, d: &Binding, sampler_part: bool| -> Res<One<'a>> {
        Ok(match (&d.resource, sampler_part) {
            // (A range as big as the shader's buffer is, where the memory
            // has room.)
            (Resource::Buffer { buffer: h, offset, size }, false) => match object(*h) {
                Some(Object::Buffer(b)) => {
                    let size = p.sizes.get(&(g, w)).map_or(*size, |&least| (*size).max(least.min(b.size().saturating_sub(*offset))));
                    One::Buffer(wgpu::BufferBinding { buffer: b, offset: *offset, size: wgpu::BufferSize::new(size) })
                }
                _ => return Err(format!("no buffer {h}")),
            },
            (Resource::View(v), false) | (Resource::Combined { view: v, .. }, false) => match object(*v) {
                Some(Object::View(v)) => One::View(&v.view),
                _ => return Err(format!("no image view {v}")),
            },
            (Resource::Sampler(s), false) | (Resource::Combined { sampler: s, .. }, true) => match object(*s) {
                Some(Object::Sampler(s)) => One::Sampler(s),
                _ => return Err(format!("no sampler {s}")),
            },
            (Resource::Texel(t), false) => match object(*t) {
                Some(Object::Texel(t)) => One::View(&t.view.view),
                _ => return Err(format!("no texel buffer view {t}")),
            },
            (r, _) => return Err(format!("set {g} binding {}: {r:?} isn't what the shader takes there", d.binding)),
        })
    };
    // A binding's resources: one, or an array's (Vulkan's descriptor
    // array, in element order).
    enum Gathered<'r> {
        One(One<'r>),
        Views(Vec<&'r wgpu::TextureView>),
        Samplers(Vec<&'r wgpu::Sampler>),
        Buffers(Vec<wgpu::BufferBinding<'r>>),
    }
    let mut groups = Vec::new();
    for (&g, used) in &p.uses {
        let set = sets.get(&g).ok_or_else(|| format!("set {g} isn't bound"))?;
        let gathered = used
            .iter()
            .map(|&w| {
                let (b, sampler_part) = crate::spirv::vulkan_binding(w);
                let element = |e: u32| {
                    let d = set.iter().find(|d| d.binding == b && d.element == e).ok_or_else(|| format!("set {g} has no binding {b}[{e}]"))?;
                    one(g, w, d, sampler_part)
                };
                let Some(&n) = p.arrays.get(&(g, w)) else { return Ok((w, Gathered::One(element(0)?))) };
                let all = (0..n).map(element).collect::<Res<Vec<_>>>()?;
                Ok((w, match all.first() {
                    Some(One::View(_)) => Gathered::Views(all.into_iter().filter_map(|o| if let One::View(v) = o { Some(v) } else { None }).collect()),
                    Some(One::Sampler(_)) => Gathered::Samplers(all.into_iter().filter_map(|o| if let One::Sampler(s) = o { Some(s) } else { None }).collect()),
                    _ => Gathered::Buffers(all.into_iter().filter_map(|o| if let One::Buffer(b) = o { Some(b) } else { None }).collect()),
                }))
            })
            .collect::<Res<Vec<_>>>()?;
        let entries: Vec<wgpu::BindGroupEntry> = gathered
            .iter()
            .map(|(w, r)| wgpu::BindGroupEntry {
                binding: *w,
                resource: match r {
                    Gathered::One(One::Buffer(b)) => wgpu::BindingResource::Buffer(b.clone()),
                    Gathered::One(One::View(v)) => wgpu::BindingResource::TextureView(v),
                    Gathered::One(One::Sampler(s)) => wgpu::BindingResource::Sampler(s),
                    Gathered::Views(v) => wgpu::BindingResource::TextureViewArray(v),
                    Gathered::Samplers(s) => wgpu::BindingResource::SamplerArray(s),
                    Gathered::Buffers(b) => wgpu::BindingResource::BufferArray(b),
                },
            })
            .collect();
        groups.push((g, gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.layout(g), entries: &entries })));
    }
    // (Push constants, and the view index, where the shaders have them.)
    if let Some(g) = p.push_group {
        let buffers: Vec<(u32, wgpu::Buffer)> = p.push_bindings.iter().map(|&b| (b, if b == 0 { staged(push) } else { staged(&view.to_le_bytes()) })).collect();
        let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().map(|(b, buffer)| wgpu::BindGroupEntry { binding: *b, resource: buffer.as_entire_binding() }).collect();
        groups.push((g, gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.layout(g), entries: &entries })));
    }
    // (Groups between that the pipeline doesn't use: empty, but bound.)
    let last = groups.iter().map(|(g, _)| *g).max();
    for g in 0..last.unwrap_or(0) {
        if !groups.iter().any(|(h, _)| *h == g) {
            groups.push((g, gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.layout(g), entries: &[] })));
        }
    }
    Ok(groups)
}

/// A triangle fan's `count` indices from `first` of an index buffer (at
/// `offset`, 16 or 32 bits each) as a list's: each triangle's first, k-th
/// and (k + 1)-th, 32 bits each, in a new buffer, by a compute shader in
/// the encoder (the indices are the device's).
fn fan_list(gpu: &Gpu, enc: &mut wgpu::CommandEncoder, buffer: &wgpu::Buffer, offset: u64, format: wgpu::IndexFormat, first: u32, count: u32) -> Res<wgpu::Buffer> {
    const FAN: &str = "
struct P { first: u32, count: u32, wide: u32 }
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<uniform> p: P;
fn index(k: u32) -> u32 {
    if p.wide == 1u { return src[k]; }
    let w = src[k / 2u];
    return select(w & 0xffffu, w >> 16u, (k & 1u) == 1u);
}
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = g.x;
    if i + 2u >= p.count { return; }
    dst[3u * i] = index(p.first);
    dst[3u * i + 1u] = index(p.first + i + 1u);
    dst[3u * i + 2u] = index(p.first + i + 2u);
}
";
    let pipeline = gpu
        .fans
        .lock()
        .unwrap()
        .get_or_insert_with(|| {
            let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(FAN.into()) });
            gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: None, layout: None, module: &module, entry_point: Some("main"), compilation_options: Default::default(), cache: None })
        })
        .clone();
    // (The source bound from an aligned offset, the indices counted from it.)
    let size = if format == wgpu::IndexFormat::Uint32 { 4 } else { 2 };
    let align = gpu.device.limits().min_storage_buffer_offset_alignment as u64;
    let at = offset / align * align;
    let first = first + ((offset - at) / size) as u32;
    let bound = ((first as u64 + count as u64) * size).next_multiple_of(4).min(buffer.size() - at);
    let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 3 * (count as u64 - 2) * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDEX,
        mapped_at_creation: false,
    });
    let params: Vec<u8> = [first, count, (size == 4) as u32, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    let uniform = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: &params, usage: wgpu::BufferUsages::UNIFORM });
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer, offset: at, size: wgpu::BufferSize::new(bound) }) },
            wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
        ],
    });
    let mut pass = enc.begin_compute_pass(&Default::default());
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.dispatch_workgroups((count - 2).div_ceil(64), 1, 1);
    drop(pass);
    Ok(out)
}

/// A render pass's targets: its color attachments' formats, its depth and
/// stencil attachment's, and their samples (what's drawn inside it matches).
pub struct Targets {
    pub colors: Vec<wgpu::TextureFormat>,
    pub depth: Option<wgpu::TextureFormat>,
    pub samples: u32,
}

/// Vulkan's vkCmdClearAttachments, inside a render pass (WebGPU's passes
/// clear only as they begin): a quad over each rectangle, at the clear
/// depth, writing the clear values to the attachments named and nothing to
/// the others. The pass's viewport, scissor and stencil reference are left
/// as a pass starts them (the driver sets the program's again).
fn clear_attachments(gpu: &Gpu, pass: &mut wgpu::RenderPass<'_>, t: &Targets, size: [u32; 2], attachments: &[Clear], rects: &[[u32; 4]]) -> Res<()> {
    let mut colors: Vec<Option<[u32; 4]>> = vec![None; t.colors.len()];
    let (mut depth, mut stencil) = (None, None);
    for a in attachments {
        if a.aspect & 1 != 0
            && let Some(c) = colors.get_mut(a.index as usize)
        {
            *c = Some(a.value);
        }
        if a.aspect & 2 != 0 && t.depth.is_some_and(|f| f.has_depth_aspect()) {
            depth = Some(f32::from_bits(a.value[0]));
        }
        if a.aspect & 4 != 0 && t.depth.is_some_and(|f| f.has_stencil_aspect()) {
            stencil = Some(a.value[1]);
        }
    }
    if colors.iter().all(Option::is_none) && depth.is_none() && stencil.is_none() {
        return Ok(());
    }
    let written: Vec<bool> = colors.iter().map(Option::is_some).collect();
    let key = format!("{:?} {:?} {} {written:?} {} {}", t.colors, t.depth, t.samples, depth.is_some(), stencil.is_some());
    let pipeline = {
        let mut clears = gpu.clears.lock().unwrap();
        clears
            .entry(key)
            .or_insert_with(|| {
                // (Each target's values as its sample type: the bits.)
                let mut outputs = String::new();
                let mut values = String::new();
                for (k, f) in t.colors.iter().enumerate() {
                    let ty = match f.sample_type(None, None) {
                        Some(wgpu::TextureSampleType::Uint) => "u32",
                        Some(wgpu::TextureSampleType::Sint) => "i32",
                        _ => "f32",
                    };
                    outputs += &format!("@location({k}) c{k}: vec4<{ty}>, ");
                    values += &format!("bitcast<vec4<{ty}>>(u.colors[{k}]), ");
                }
                let fragment = if t.colors.is_empty() {
                    "@fragment fn fs() {}".to_string()
                } else {
                    format!("struct Out {{ {outputs} }}\n@fragment fn fs() -> Out {{ return Out({values}); }}")
                };
                let wgsl = format!(
                    "struct U {{ colors: array<vec4<u32>, 8>, depth: f32 }}\n@group(0) @binding(0) var<uniform> u: U;\n\
                     @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {{\n\
                     let p = vec2<f32>(f32(i & 1u), f32(i >> 1u)) * 2.0 - 1.0;\n return vec4<f32>(p, u.depth, 1.0);\n}}\n{fragment}\n"
                );
                let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(wgsl.into()) });
                let targets: Vec<Option<wgpu::ColorTargetState>> = t
                    .colors
                    .iter()
                    .zip(&written)
                    .map(|(&format, &w)| Some(wgpu::ColorTargetState { format, blend: None, write_mask: if w { wgpu::ColorWrites::ALL } else { wgpu::ColorWrites::empty() } }))
                    .collect();
                let face = wgpu::StencilFaceState { compare: wgpu::CompareFunction::Always, fail_op: wgpu::StencilOperation::Keep, depth_fail_op: wgpu::StencilOperation::Keep, pass_op: wgpu::StencilOperation::Replace };
                let depth_stencil = t.depth.map(|format| wgpu::DepthStencilState {
                    format,
                    depth_write_enabled: Some(depth.is_some()),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: if format.has_stencil_aspect() {
                        wgpu::StencilState { front: face, back: face, read_mask: 0xff, write_mask: if stencil.is_some() { 0xff } else { 0 } }
                    } else {
                        Default::default()
                    },
                    bias: Default::default(),
                });
                gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: None,
                    layout: None,
                    vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                    primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
                    depth_stencil,
                    multisample: wgpu::MultisampleState { count: t.samples.max(1), ..Default::default() },
                    fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs"), compilation_options: Default::default(), targets: &targets }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .clone()
    };
    let mut uniform = Vec::with_capacity(144);
    for k in 0..8 {
        for w in colors.get(k).copied().flatten().unwrap_or([0; 4]) {
            uniform.extend_from_slice(&w.to_le_bytes());
        }
    }
    uniform.extend_from_slice(&depth.unwrap_or(0.0).to_le_bytes());
    uniform.resize(144, 0);
    let buffer = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: &uniform, usage: wgpu::BufferUsages::UNIFORM });
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
    });
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.set_viewport(0.0, 0.0, size[0] as f32, size[1] as f32, 0.0, 1.0);
    pass.set_stencil_reference(stencil.unwrap_or(0));
    for &[x, y, w, h] in rects {
        let (x0, y0) = (x.min(size[0]), y.min(size[1]));
        let (x1, y1) = ((x as u64 + w as u64).min(size[0] as u64) as u32, (y as u64 + h as u64).min(size[1] as u64) as u32);
        if x1 > x0 && y1 > y0 {
            pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
            pass.draw(0..4, 0..1);
        }
    }
    pass.set_scissor_rect(0, 0, size[0], size[1]);
    pass.set_stencil_reference(0);
    Ok(())
}

const BLIT: &str = "
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
// The source's coordinates at the destination's corners: u0, v0, u1, v1.
@group(0) @binding(2) var<uniform> r: vec4<f32>;
struct V { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    return V(vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0), mix(r.xy, r.zw, c));
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> { return textureSampleLevel(t, s, v.uv, 0.0); }
";

/// A blit from a format that isn't filtered (integers', or floats' the
/// device won't filter): the nearest texel fetched, T the source's (and
/// destination's) sample type.
const BLIT_NEAREST: &str = "
@group(0) @binding(0) var t: texture_2d<T>;
@group(0) @binding(2) var<uniform> r: vec4<f32>;
struct V { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    return V(vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0), mix(r.xy, r.zw, c));
}
@fragment fn fs(v: V) -> @location(0) vec4<T> {
    let size = vec2<f32>(textureDimensions(t, 0));
    return textureLoad(t, vec2<i32>(clamp(floor(v.uv * size), vec2<f32>(0.0), size - 1.0)), 0);
}
";

/// A blit: one mip level and layer's region (corners `from`) scaled into
/// another's (`to`), mirrored where the corners are; a copy where nothing
/// is scaled, else drawn (a quad sampling the source, or fetching its
/// nearest texels where it isn't filtered).
#[allow(clippy::too_many_arguments)]
fn blit(gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, [sm, sl]: [u32; 2], from: [i32; 4], dst: &wgpu::Texture, [dm, dl]: [u32; 2], to: [i32; 4], linear: bool) -> Res<()> {
    let (mut from, mut to) = (from, to);
    // (The destination's corners in order; the source's swapped with them.)
    for k in 0..2 {
        if to[k] > to[k + 2] {
            to.swap(k, k + 2);
            from.swap(k, k + 2);
        }
    }
    let (w, h) = ((to[2] - to[0]) as u32, (to[3] - to[1]) as u32);
    if w == 0 || h == 0 {
        return Ok(());
    }
    if from[2] - from[0] == w as i32 && from[3] - from[1] == h as i32 && src.format() == dst.format() {
        let at = |t, mip, layer, x: i32, y: i32| wgpu::TexelCopyTextureInfo { texture: t, mip_level: mip, origin: wgpu::Origin3d { x: x as u32, y: y as u32, z: layer }, aspect: wgpu::TextureAspect::All };
        enc.copy_texture_to_texture(at(src, sm, sl, from[0], from[1]), at(dst, dm, dl, to[0], to[1]), wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
        return Ok(());
    }
    let format = dst.format();
    let filterable = |f: wgpu::TextureFormat| gpu.format_features(f).flags.contains(wgpu::TextureFormatFeatureFlags::FILTERABLE);
    if !src.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING) || !dst.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) || src.format().is_depth_stencil_format() {
        return Err(format!("a scaled blit from {:?} to {format:?}", src.format()));
    }
    // (Sampled, or the nearest texel fetched: integers', floats' unfiltered.)
    let kind = match src.format().sample_type(None, None) {
        Some(wgpu::TextureSampleType::Uint) => "u32",
        Some(wgpu::TextureSampleType::Sint) => "i32",
        _ if filterable(src.format()) => "",
        _ => "f32",
    };
    let mut blits = gpu.blits.lock().unwrap();
    let pipeline = blits.entry((format, kind)).or_insert_with(|| {
        let source = if kind.is_empty() { BLIT.to_string() } else { BLIT_NEAREST.replace("<T>", &format!("<{kind}>")) };
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(source.into()) });
        gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(format.into())] }),
            multiview_mask: None,
            cache: None,
        })
    });
    let level = |t: &wgpu::Texture, mip, layer| {
        t.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_mip_level: mip,
            mip_level_count: Some(1),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    };
    let (sw, sh) = ((src.width() >> sm).max(1) as f32, (src.height() >> sm).max(1) as f32);
    let r = [from[0] as f32 / sw, from[1] as f32 / sh, from[2] as f32 / sw, from[3] as f32 / sh];
    let uniform = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor {
        label: None,
        contents: &r.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>(),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let filter = if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: filter, min_filter: filter, ..Default::default() });
    let source = level(src, sm, sl);
    let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&source) }, wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() }];
    if kind.is_empty() {
        entries.push(wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) });
    }
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipeline.get_bind_group_layout(0), entries: &entries });
    let target = level(dst, dm, dl);
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.set_viewport(to[0] as f32, to[1] as f32, w as f32, h as f32, 0.0, 1.0);
    pass.draw(0..4, 0..1);
    Ok(())
}

const BYTE_COPY: &str = "
// The aligned words around the source, the destination, and where: the
// source's first byte among those words, the destination's first byte, and
// how many.
@group(0) @binding(0) var<storage, read> src_words: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst_words: array<u32>;
@group(0) @binding(2) var<uniform> at: vec4<u32>;
fn byte(i: u32) -> u32 { return (src_words[i / 4u] >> (8u * (i % 4u))) & 255u; }
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    // (A word each: the destination's own bytes kept outside the range.)
    let w = at.y / 4u + id.x;
    if (w * 4u >= at.y + at.z) { return; }
    var v = dst_words[w];
    for (var b = 0u; b < 4u; b++) {
        let g = w * 4u + b;
        if (g >= at.y && g < at.y + at.z) {
            v = (v & ~(255u << (8u * b))) | (byte(at.x + g - at.y) << (8u * b));
        }
    }
    dst_words[w] = v;
}
";

/// A copy of buffer bytes at any offset and size (WebGPU copies whole
/// words): the source's aligned words copied aside, then a word of the
/// destination per invocation made from them.
#[allow(clippy::too_many_arguments)]
fn copy_bytes(gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Buffer, src_offset: u64, dst: &wgpu::Buffer, dst_offset: u64, size: u64) {
    if size == 0 {
        return;
    }
    let (start, end) = (src_offset & !3, (src_offset + size).next_multiple_of(4).min(src.size()));
    let aside = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: end - start, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
    enc.copy_buffer_to_buffer(src, start, &aside, 0, end - start);
    let mut cache = gpu.byte_copy.lock().unwrap();
    let pipeline = cache.get_or_insert_with(|| {
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(BYTE_COPY.into()) });
        gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: None, layout: None, module: &module, entry_point: Some("main"), compilation_options: Default::default(), cache: None })
    });
    let at: Vec<u8> = [(src_offset - start) as u32, dst_offset as u32, size as u32, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    let uniform = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: &at, usage: wgpu::BufferUsages::UNIFORM });
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: aside.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
        ],
    });
    let words = (dst_offset + size).div_ceil(4) - dst_offset / 4;
    let mut pass = enc.begin_compute_pass(&Default::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &group, &[]);
    pass.dispatch_workgroups(words.div_ceil(64) as u32, 1, 1);
}

/// A wgpu error, described (its description, where the error's own text
/// says only "Validation Error").
fn describe(e: &wgpu::Error) -> String {
    match e {
        wgpu::Error::Validation { description, .. } | wgpu::Error::Internal { description, .. } => description.trim().to_string(),
        e => e.to_string(),
    }
}

/// A color's bits as the format takes it: floats, or integers for an
/// integer format.
fn color(format: wgpu::TextureFormat, bits: &[u32; 4]) -> wgpu::Color {
    let value = |k: usize| match format.sample_type(None, None) {
        Some(wgpu::TextureSampleType::Uint) => bits[k] as f64,
        Some(wgpu::TextureSampleType::Sint) => bits[k] as i32 as f64,
        _ => f32::from_bits(bits[k]) as f64,
    };
    wgpu::Color { r: value(0), g: value(1), b: value(2), a: value(3) }
}

/// A clear color (Vulkan's clear value's bits) as a texel of `format`'s
/// bytes, for the formats images that aren't attachments have.
fn packed(format: wgpu::TextureFormat, bits: &[u32; 4]) -> Option<Vec<u8>> {
    use wgpu::TextureFormat as F;
    let f = |k: usize| f32::from_bits(bits[k]);
    let unorm8 = |k: usize| (f(k).clamp(0.0, 1.0) * 255.0).round() as u8;
    let snorm8 = |k: usize| (f(k).clamp(-1.0, 1.0) * 127.0).round() as i8 as u8;
    let unorm16 = |k: usize| ((f(k).clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes();
    let snorm16 = |k: usize| ((f(k).clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes();
    let half = |k: usize| half_bits(f(k)).to_le_bytes();
    let srgb = |k: usize| {
        let c = f(k).clamp(0.0, 1.0);
        let s = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    let lanes = |n: usize, one: &dyn Fn(usize) -> Vec<u8>| (0..n).flat_map(one).collect::<Vec<u8>>();
    Some(match format {
        F::R8Unorm => lanes(1, &|k| vec![unorm8(k)]),
        F::Rg8Unorm => lanes(2, &|k| vec![unorm8(k)]),
        F::Rgba8Unorm => lanes(4, &|k| vec![unorm8(k)]),
        F::Rgba8UnormSrgb => vec![srgb(0), srgb(1), srgb(2), unorm8(3)],
        F::Bgra8Unorm => vec![unorm8(2), unorm8(1), unorm8(0), unorm8(3)],
        F::Bgra8UnormSrgb => vec![srgb(2), srgb(1), srgb(0), unorm8(3)],
        F::R8Snorm => lanes(1, &|k| vec![snorm8(k)]),
        F::Rg8Snorm => lanes(2, &|k| vec![snorm8(k)]),
        F::Rgba8Snorm => lanes(4, &|k| vec![snorm8(k)]),
        F::R8Uint | F::R8Sint => lanes(1, &|k| vec![bits[k] as u8]),
        F::Rg8Uint | F::Rg8Sint => lanes(2, &|k| vec![bits[k] as u8]),
        F::Rgba8Uint | F::Rgba8Sint => lanes(4, &|k| vec![bits[k] as u8]),
        F::R16Unorm => lanes(1, &|k| unorm16(k).to_vec()),
        F::Rg16Unorm => lanes(2, &|k| unorm16(k).to_vec()),
        F::Rgba16Unorm => lanes(4, &|k| unorm16(k).to_vec()),
        F::R16Snorm => lanes(1, &|k| snorm16(k).to_vec()),
        F::Rg16Snorm => lanes(2, &|k| snorm16(k).to_vec()),
        F::Rgba16Snorm => lanes(4, &|k| snorm16(k).to_vec()),
        F::R16Uint | F::R16Sint => lanes(1, &|k| (bits[k] as u16).to_le_bytes().to_vec()),
        F::Rg16Uint | F::Rg16Sint => lanes(2, &|k| (bits[k] as u16).to_le_bytes().to_vec()),
        F::Rgba16Uint | F::Rgba16Sint => lanes(4, &|k| (bits[k] as u16).to_le_bytes().to_vec()),
        F::R16Float => lanes(1, &|k| half(k).to_vec()),
        F::Rg16Float => lanes(2, &|k| half(k).to_vec()),
        F::Rgba16Float => lanes(4, &|k| half(k).to_vec()),
        F::R32Uint | F::R32Sint | F::R32Float => lanes(1, &|k| bits[k].to_le_bytes().to_vec()),
        F::Rg32Uint | F::Rg32Sint | F::Rg32Float => lanes(2, &|k| bits[k].to_le_bytes().to_vec()),
        F::Rgba32Uint | F::Rgba32Sint | F::Rgba32Float => lanes(4, &|k| bits[k].to_le_bytes().to_vec()),
        _ => return None,
    })
}

/// A float's half-precision bits, rounded to nearest even.
fn half_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rest = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let round = (rest > mid || (rest == mid && half & 1 == 1)) as u32;
        return sign | (half + round) as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    let round = (rest > 0x1000 || (rest == 0x1000 && half & 1 == 1)) as u32;
    sign | (half + round) as u16
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
    let view = |h: u64| match object(h) {
        Some(Object::View(v)) => Ok(v),
        _ => Err(format!("no image view {h}")),
    };
    let texel = |h: u64| match object(h) {
        Some(Object::Texel(t)) => Ok(t),
        _ => Err(format!("no texel buffer view {h}")),
    };
    let pipeline = |h: u64| match object(h) {
        Some(Object::Pipeline(p)) => Ok(p),
        _ => Err(format!("no pipeline {h}")),
    };
    let queries = |h: u64| match object(h) {
        Some(Object::Queries(q)) => Ok(q),
        _ => Err(format!("no query pool {h}")),
    };
    // (Depth bias and stencil masks as commands set them, for the draws.)
    let mut dynamic = Dynamic::default();
    // (An occlusion query begun outside render passes, its pool and index and
    // the overflow slots the passes since counted into; those ended; and the
    // slots used, by pool.)
    let mut spanning: Option<(u64, u32, Vec<u32>)> = None;
    let mut spans: Vec<(u64, u32, Vec<u32>)> = Vec::new();
    let mut spill_next: BTreeMap<u64, u32> = BTreeMap::new();
    // (Queries ended in it, or reset since, by pool: read back after.)
    let mut ended: BTreeMap<u64, BTreeMap<u32, bool>> = BTreeMap::new();
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    // Compute's state, and graphics' (Vulkan's two bind points).
    let mut bound: Option<Arc<Pipeline>> = None;
    let mut sets: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
    let mut graphics: Option<Arc<Pipeline>> = None;
    let mut graphics_sets: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
    let mut vertex: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut index: Option<(u64, u64, wgpu::IndexFormat)> = None;
    let mut push = vec![0u8; PUSH_BYTES as usize];
    // (Bytes a command writes from the host: a buffer of them, copied in
    // order with the rest.)
    let staged = |data: &[u8]| {
        wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: data, usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::STORAGE })
    };
    let set_push = |push: &mut Vec<u8>, offset: u32, data: &[u8]| -> Res<()> {
        let end = offset as usize + data.len();
        if end > push.len() {
            return Err(format!("push constants to byte {end} (at most {PUSH_BYTES})"));
        }
        push[offset as usize..end].copy_from_slice(data);
        Ok(())
    };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut i = 0;
    while i < commands.len() {
        match &commands[i] {
            Command::BindPipeline(h) => {
                let p = pipeline(*h)?;
                if matches!(p.kind, Kind::Render(_)) { graphics = Some(p) } else { bound = Some(p) }
            }
            Command::BindSet { set, bindings } => {
                sets.insert(*set, bindings.clone());
            }
            Command::BindGraphicsSet { set, bindings } => {
                graphics_sets.insert(*set, bindings.clone());
            }
            Command::Push { offset, data } => set_push(&mut push, *offset, data)?,
            Command::BindVertex { first, buffers } => {
                for (k, &b) in buffers.iter().enumerate() {
                    vertex.insert(first + k as u32, b);
                }
            }
            Command::BindIndex { buffer, offset, wide } => index = Some((*buffer, *offset, if *wide { wgpu::IndexFormat::Uint32 } else { wgpu::IndexFormat::Uint16 })),
            c @ (Command::Dispatch(_) | Command::DispatchIndirect { .. }) => {
                let p = bound.as_ref().ok_or("a dispatch with no pipeline bound")?;
                let Kind::Compute(cp) = &p.kind else { unreachable!() };
                for t in texels_of(sets.values().flatten()) {
                    let t = texel(t)?;
                    refresh(&mut enc, buf(t.buffer)?, t);
                }
                let groups = bind_groups(gpu, p, &sets, &push, 0, &object, &staged)?;
                let mut pass = enc.begin_compute_pass(&Default::default());
                pass.set_pipeline(cp);
                for (g, group) in &groups {
                    pass.set_bind_group(*g, group, &[]);
                }
                match c {
                    Command::Dispatch([x, y, z]) => pass.dispatch_workgroups(*x, *y, *z),
                    Command::DispatchIndirect { buffer, offset } => pass.dispatch_workgroups_indirect(buf(*buffer)?, *offset),
                    _ => unreachable!(),
                }
                drop(pass);
                for t in texels_of(sets.values().flatten()) {
                    let t = texel(t)?;
                    write_back(&mut enc, buf(t.buffer)?, t);
                }
            }
            // (Within one buffer, two of Vulkan's in one memory: through
            // another, as WebGPU won't copy a buffer to itself.)
            Command::Copy { src, src_offset, dst, dst_offset, size } if (src_offset | dst_offset | size) % 4 != 0 => {
                copy_bytes(gpu, &mut enc, buf(*src)?, *src_offset, buf(*dst)?, *dst_offset, *size);
            }
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
            Command::BufferToImage(r) => buffer_image(gpu, &mut enc, buf(r.buffer)?, tex(r.image)?, r, true)?,
            Command::ImageToBuffer(r) => buffer_image(gpu, &mut enc, buf(r.buffer)?, tex(r.image)?, r, false)?,
            Command::CopyImage { src, from, dst, to } => {
                let (a, extent) = on_texture(tex(*src)?, from);
                let (b, _) = on_texture(tex(*dst)?, to);
                let (sf, df) = (a.texture.format(), b.texture.format());
                if sf.remove_srgb_suffix() == df.remove_srgb_suffix() {
                    enc.copy_texture_to_texture(a, b, extent);
                } else {
                    // (Formats of texels the same size, which Vulkan copies
                    // between and WebGPU won't: through a buffer, as blocks
                    // where one is compressed.)
                    let ((sw, sh), (dw, dh)) = (sf.block_dimensions(), df.block_dimensions());
                    let bytes = sf.block_copy_size(Some(a.aspect)).ok_or_else(|| format!("copying a {sf:?} image"))?;
                    let (w, h) = (extent.width.div_ceil(sw), extent.height.div_ceil(sh));
                    let pitch = (w * bytes).next_multiple_of(256);
                    let staged = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: None,
                        size: pitch as u64 * h as u64 * extent.depth_or_array_layers as u64,
                        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let layout = wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(h) };
                    enc.copy_texture_to_buffer(a, wgpu::TexelCopyBufferInfo { buffer: &staged, layout }, wgpu::Extent3d { width: w * sw, height: h * sh, ..extent });
                    enc.copy_buffer_to_texture(wgpu::TexelCopyBufferInfo { buffer: &staged, layout }, b, wgpu::Extent3d { width: w * dw, height: h * dh, ..extent });
                }
            }
            Command::ClearColor { image, mip, mips, layer, layers, color: bits } => {
                let t = tex(*image)?;
                // (An image that can't be an attachment, a 1D one: its
                // texels copied in, each the color packed.)
                if !t.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
                    let texel = packed(t.format(), bits).ok_or_else(|| format!("clearing a {:?} image", t.format()))?;
                    let d3 = t.dimension() == wgpu::TextureDimension::D3;
                    for m in *mip..mip + mips {
                        let size = t.size().mip_level_size(m, t.dimension());
                        let pitch = (size.width as usize * texel.len()).next_multiple_of(256);
                        let rows = size.height as usize * if d3 { size.depth_or_array_layers as usize } else { 1 };
                        let mut bytes = vec![0u8; pitch * rows];
                        for row in bytes.chunks_mut(pitch) {
                            for x in 0..size.width as usize {
                                row[x * texel.len()..(x + 1) * texel.len()].copy_from_slice(&texel);
                            }
                        }
                        let b = wgpu::util::DeviceExt::create_buffer_init(
                            &gpu.device,
                            &wgpu::util::BufferInitDescriptor { label: None, contents: &bytes, usage: wgpu::BufferUsages::COPY_SRC },
                        );
                        for l in *layer..layer + if d3 { 1 } else { *layers } {
                            let depth = if d3 { size.depth_or_array_layers } else { 1 };
                            enc.copy_buffer_to_texture(
                                wgpu::TexelCopyBufferInfo {
                                    buffer: &b,
                                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch as u32), rows_per_image: Some(size.height) },
                                },
                                wgpu::TexelCopyTextureInfo {
                                    texture: t,
                                    mip_level: m,
                                    origin: wgpu::Origin3d { x: 0, y: 0, z: if d3 { 0 } else { l } },
                                    aspect: wgpu::TextureAspect::All,
                                },
                                wgpu::Extent3d { width: size.width, height: size.height, depth_or_array_layers: depth },
                            );
                        }
                    }
                    i += 1;
                    continue;
                }
                // (A render pass per mip level and layer, cleared.)
                let clear = color(t.format(), bits);
                let d3 = t.dimension() == wgpu::TextureDimension::D3;
                for m in *mip..mip + mips {
                    for l in *layer..layer + if d3 { 1 } else { *layers } {
                        let v = t.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(if d3 { wgpu::TextureViewDimension::D3 } else { wgpu::TextureViewDimension::D2 }),
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
            Command::BeginPass { colors, depth, views } => {
                // (Multiview: the subpass drawn again for each view, into
                // its layer of each attachment, its index the shaders'
                // `gl_ViewIndex`.)
                let start = i;
                let each: Vec<Option<u32>> = if *views == 0 { vec![None] } else { (0..32).filter(|v| views >> v & 1 == 1).map(Some).collect() };
                for (ordinal, one) in each.into_iter().enumerate() {
                    i = start;
                    let view_index = one.unwrap_or(0);
                    // (A query's views count in the queries after it, one
                    // each.)
                    let ordinal = ordinal as u32;
                    // (Whether an attachment's load op is this view's: it
                    // uses the attachment first here, or there are no views.)
                    let first = |firsts: u32| one.is_none_or(|k| firsts >> k & 1 == 1);
                    let mut layered: HashMap<u64, wgpu::TextureView> = HashMap::new();
                    if let Some(k) = one {
                        let handles = colors.iter().flat_map(|a| [a.view, a.resolve]).chain(depth.iter().map(|d| d.view)).filter(|&h| h != 0);
                        for h in handles {
                            let v = view(h)?;
                            let all = v.attachment.is_some() || v.format.is_depth_stencil_format();
                            let layer = v.view.texture().create_view(&wgpu::TextureViewDescriptor {
                                format: if all { None } else { Some(v.format) },
                                dimension: Some(wgpu::TextureViewDimension::D2),
                                base_mip_level: v.base[0],
                                mip_level_count: Some(1),
                                base_array_layer: v.base[1] + k,
                                array_layer_count: Some(1),
                                ..Default::default()
                            });
                            layered.insert(h, layer);
                        }
                    }
                    // (Texel buffers the pass may read, brought up to date: those
                    // bound, and those it binds.)
                    let ahead = commands[i + 1..].iter().take_while(|c| **c != Command::EndPass).filter_map(|c| match c {
                        Command::BindGraphicsSet { bindings, .. } => Some(bindings),
                        _ => None,
                    });
                    for t in texels_of(graphics_sets.values().flatten().chain(ahead.flatten())) {
                        let t = texel(t)?;
                        refresh(&mut enc, buf(t.buffer)?, t);
                    }
                    // (Vulkan's load ops: load, clear, don't care; store ops:
                    // store, don't care.)
                    let store = |s: u32| if s == 1 { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store };
                    let color_attachments = colors
                        .iter()
                        .map(|a| {
                            let v = view(a.view)?;
                            let load = if a.load == 1 && first(a.firsts) { wgpu::LoadOp::Clear(color(v.format, &a.clear)) } else { wgpu::LoadOp::Load };
                            let resolve_target = if a.resolve != 0 { Some(layered.get(&a.resolve).unwrap_or(&view(a.resolve)?.view)) } else { None };
                            Ok(Some(wgpu::RenderPassColorAttachment { view: layered.get(&a.view).unwrap_or(&v.view), depth_slice: None, resolve_target, ops: wgpu::Operations { load, store: store(a.store) } }))
                        })
                        .collect::<Res<Vec<_>>>()?;
                    let depth_stencil_attachment = match depth {
                        Some(d) => {
                            let v = view(d.view)?;
                            fn ops<T>((load, store): (u32, u32), clear: T) -> wgpu::Operations<T> {
                                let load = if load == 1 { wgpu::LoadOp::Clear(clear) } else { wgpu::LoadOp::Load };
                                wgpu::Operations { load, store: if store == 1 { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store } }
                            }
                            Some(wgpu::RenderPassDepthStencilAttachment {
                                view: layered.get(&d.view).unwrap_or(v.attachment.as_ref().unwrap_or(&v.view)),
                                depth_ops: v.format.has_depth_aspect().then(|| ops(if first(d.firsts) { d.depth } else { (0, d.depth.1) }, d.clear_depth)),
                                stencil_ops: v.format.has_stencil_aspect().then(|| ops(if first(d.firsts) { d.stencil } else { (0, d.stencil.1) }, d.clear_stencil)),
                            })
                        }
                        None => None,
                    };
                    // (Its size: its attachments', for scissors within it.)
                    let first = colors.first().map(|a| a.view).or(depth.as_ref().map(|d| d.view));
                    let size = match first {
                        Some(h) => view(h)?.size,
                        None => [0, 0],
                    };
                    // (Its targets, for clears drawn inside it.)
                    let targets = Targets {
                        colors: colors.iter().map(|a| Ok(view(a.view)?.format)).collect::<Res<_>>()?,
                        depth: match depth {
                            Some(d) => Some(view(d.view)?.format),
                            None => None,
                        },
                        samples: match first {
                            Some(h) => view(h)?.samples,
                            None => 1,
                        },
                    };
                    // (Its indexed draws of triangle fans: their indices made
                    // lists', before it begins, as compute can't run inside it.)
                    let mut fans: HashMap<usize, wgpu::Buffer> = HashMap::new();
                    {
                        let (mut p, mut ix) = (graphics.clone(), index);
                        for (j, c) in commands.iter().enumerate().skip(i + 1) {
                            match c {
                                Command::EndPass => break,
                                Command::BindPipeline(h) => p = Some(pipeline(*h)?),
                                Command::BindIndex { buffer, offset, wide } => ix = Some((*buffer, *offset, if *wide { wgpu::IndexFormat::Uint32 } else { wgpu::IndexFormat::Uint16 })),
                                Command::DrawIndexed { indices, first, .. } if *indices >= 3 && p.as_ref().is_some_and(|p| p.fan) => {
                                    let (h, offset, f) = ix.ok_or("an indexed draw with no index buffer")?;
                                    fans.insert(j, fan_list(gpu, &mut enc, buf(h)?, offset, f, *first, *indices)?);
                                }
                                _ => {}
                            }
                        }
                    }
                    // (The pool of the occlusion queries it has, named as it begins.)
                    let occlusion = commands[i + 1..].iter().take_while(|c| **c != Command::EndPass).find_map(|c| match c {
                        Command::BeginQuery { pool, .. } => Some(*pool),
                        _ => None,
                    });
                    // (Or, while a query begun outside passes is on, the pool's
                    // overflow set: the pass counts into a slot of it.)
                    if occlusion.is_some() && spanning.is_some() {
                        return Err("occlusion queries begun inside a render pass and around it".into());
                    }
                    let occlusion_query_set = match (occlusion, &spanning) {
                        (Some(h), _) => Some(&queries(h)?.set),
                        (None, Some((h, _, _))) => Some(&queries(*h)?.spill.as_ref().ok_or("an occlusion query of a pool of timestamps")?.0),
                        (None, None) => None,
                    };
                    // (Views of its own attachments its draws read, as input
                    // attachments of a subpass that draws into them, a
                    // self-dependency: copies made as it begins, which WebGPU
                    // reads, as it won't read an attachment.)
                    let attached: Vec<&wgpu::Texture> = colors.iter().map(|a| a.view).chain(depth.iter().map(|d| d.view)).map(|h| Ok(view(h)?.view.texture())).collect::<Res<_>>()?;
                    let mut snapshots: HashMap<u64, View> = HashMap::new();
                    let ahead = commands[i + 1..].iter().take_while(|c| **c != Command::EndPass).filter_map(|c| match c {
                        Command::BindGraphicsSet { bindings, .. } => Some(bindings),
                        _ => None,
                    });
                    for b in graphics_sets.values().flatten().chain(ahead.flatten()) {
                        let (Resource::View(h) | Resource::Combined { view: h, .. }) = b.resource else {
                            continue;
                        };
                        let v = view(h)?;
                        let t = v.view.texture();
                        if snapshots.contains_key(&h) {
                            continue;
                        }
                        // (Multiview: an attachment's layers, bound as an input
                        // attachment, read at this view's layer.)
                        let layer = v.base[1] + one.filter(|_| v.array).unwrap_or(0);
                        if one.is_some() && v.array && t.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) && !attached.contains(&t) {
                            let layer_view = t.create_view(&wgpu::TextureViewDescriptor {
                                format: Some(v.format),
                                dimension: Some(wgpu::TextureViewDimension::D2),
                                base_mip_level: v.base[0],
                                mip_level_count: Some(1),
                                base_array_layer: layer,
                                array_layer_count: Some(1),
                                ..Default::default()
                            });
                            snapshots.insert(h, View { view: layer_view, format: v.format, size: v.size, attachment: None, samples: v.samples, base: [v.base[0], layer], array: false });
                            continue;
                        }
                        if !attached.contains(&t) || v.samples > 1 {
                            continue;
                        }
                        let size = wgpu::Extent3d { width: v.size[0], height: v.size[1], depth_or_array_layers: 1 };
                        let copy = gpu.device.create_texture(&wgpu::TextureDescriptor {
                            label: None,
                            size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: t.format(),
                            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        let from =
                            wgpu::TexelCopyTextureInfo { texture: t, mip_level: v.base[0], origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All };
                        enc.copy_texture_to_texture(from, copy.as_image_copy(), size);
                        let aspect =
                            if v.format.has_depth_aspect() && t.format().is_combined_depth_stencil_format() { wgpu::TextureAspect::DepthOnly } else { wgpu::TextureAspect::All };
                        let snapshot = copy.create_view(&wgpu::TextureViewDescriptor { format: Some(v.format), aspect, ..Default::default() });
                        snapshots.insert(h, View { view: snapshot, format: v.format, size: v.size, attachment: None, samples: 1, base: [0, 0], array: false });
                    }
                    let object = |h: u64| match snapshots.get(&h) {
                        Some(v) => Some(Object::View(v)),
                        None => object(h),
                    };
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &color_attachments, depth_stencil_attachment, occlusion_query_set, ..Default::default() });
                    if let Some((h, _, slots)) = &mut spanning {
                        let next = spill_next.entry(*h).or_insert(0);
                        if *next >= SPILL {
                            return Err(format!("an occlusion query over more than {SPILL} render passes"));
                        }
                        pass.begin_occlusion_query(*next);
                        slots.push(*next);
                        *next += 1;
                    }
                    // The pass's commands, to its end.
                    i += 1;
                    while i < commands.len() && commands[i] != Command::EndPass {
                        match &commands[i] {
                            Command::BindPipeline(h) => {
                                let p = pipeline(*h)?;
                                pass.set_pipeline(&p.render_for(gpu, &targets.colors, dynamic)?);
                                graphics = Some(p);
                            }
                            Command::BindGraphicsSet { set, bindings } => {
                                graphics_sets.insert(*set, bindings.clone());
                            }
                            Command::BindSet { set, bindings } => {
                                sets.insert(*set, bindings.clone());
                            }
                            Command::Push { offset, data } => set_push(&mut push, *offset, data)?,
                            // (A negative height flips: drawn by pipelines whose
                            // vertex shaders don't, in the viewport made positive.)
                            Command::SetViewport([x, y, w, h, near, far]) => {
                                dynamic.flip = *h < 0.0;
                                if dynamic.flip {
                                    pass.set_viewport(*x, y + h, *w, -h, *near, *far);
                                } else {
                                    pass.set_viewport(*x, *y, *w, *h, *near, *far);
                                }
                            }
                            Command::SetScissor([x, y, w, h]) => {
                                // (Within the attachments, as WebGPU's must be:
                                // Vulkan's offsets are signed.)
                                let clip = |o: u32, n: u32, max: u32| {
                                    let (o, end) = (o as i32 as i64, o as i32 as i64 + n as i64);
                                    let (o, end) = (o.clamp(0, max as i64), end.clamp(0, max as i64));
                                    (o as u32, (end - o).max(0) as u32)
                                };
                                let ((x, w), (y, h)) = (clip(*x, *w, size[0]), clip(*y, *h, size[1]));
                                pass.set_scissor_rect(x, y, w, h)
                            }
                            Command::SetBlendConstants([r, g, b, a]) => pass.set_blend_constant(wgpu::Color { r: *r as f64, g: *g as f64, b: *b as f64, a: *a as f64 }),
                            Command::SetStencilReference(r) => pass.set_stencil_reference(*r),
                            // (Kept for the draws: a pipeline variant's.)
                            Command::SetDepthBias([constant, clamp, slope]) => dynamic.bias = Some([*constant, *slope, *clamp]),
                            Command::SetStencilCompareMask(m) => dynamic.compare_mask = Some(*m),
                            Command::SetStencilWriteMask(m) => dynamic.write_mask = Some(*m),
                            Command::ClearAttachments { attachments, rects } => clear_attachments(gpu, &mut pass, &targets, size, attachments, rects)?,
                            Command::BeginQuery { pool, index } => {
                                if Some(*pool) != occlusion {
                                    return Err("occlusion queries of two pools in one render pass".into());
                                }
                                pass.begin_occlusion_query(index + ordinal);
                            }
                            Command::EndQuery { pool, index } => {
                                pass.end_occlusion_query();
                                ended.entry(*pool).or_default().insert(index + ordinal, true);
                            }
                            Command::Timestamp { pool, index } => {
                                pass.write_timestamp(&queries(*pool)?.set, index + ordinal);
                                ended.entry(*pool).or_default().insert(index + ordinal, true);
                            }
                            Command::BindVertex { first, buffers } => {
                                for (k, &b) in buffers.iter().enumerate() {
                                    vertex.insert(first + k as u32, b);
                                }
                            }
                            Command::BindIndex { buffer, offset, wide } => index = Some((*buffer, *offset, if *wide { wgpu::IndexFormat::Uint32 } else { wgpu::IndexFormat::Uint16 })),
                            c @ (Command::Draw(_) | Command::DrawIndexed { .. } | Command::DrawIndirect { .. }) => {
                                let p = graphics.as_ref().ok_or("a draw with no pipeline bound")?;
                                if p.culls_all {
                                    i += 1;
                                    continue;
                                }
                                // (Its pipeline again: one bound before the pass,
                                // and an indexed one's for its indices' format.)
                                let indexed = matches!(c, Command::DrawIndexed { .. } | Command::DrawIndirect { indexed: true, .. });
                                let d = Dynamic { strip_index: index.filter(|_| indexed).map(|(_, _, f)| f), ..dynamic };
                                pass.set_pipeline(&p.render_for(gpu, &targets.colors, d)?);
                                for (g, group) in bind_groups(gpu, p, &graphics_sets, &push, view_index, &object, &staged)? {
                                    pass.set_bind_group(g, &group, &[]);
                                }
                                for (slot, (b, from)) in p.vertex_bindings.iter().enumerate() {
                                    let &(h, offset) = vertex.get(b).ok_or_else(|| format!("no vertex buffer bound to binding {b}"))?;
                                    let b = buf(h)?;
                                    pass.set_vertex_buffer(slot as u32, b.slice((offset + from).min(b.size())..));
                                }
                                // (A triangle fan's: a list of its triangles' indices.)
                                if p.fan {
                                    match c {
                                        Command::Draw([n, instances, first, first_instance]) => {
                                            let list: Vec<u8> = (1..n.saturating_sub(1)).flat_map(|k| [*first, first + k, first + k + 1]).flat_map(u32::to_le_bytes).collect();
                                            if !list.is_empty() {
                                                let b = wgpu::util::DeviceExt::create_buffer_init(&gpu.device, &wgpu::util::BufferInitDescriptor { label: None, contents: &list, usage: wgpu::BufferUsages::INDEX });
                                                pass.set_index_buffer(b.slice(..), wgpu::IndexFormat::Uint32);
                                                pass.draw_indexed(0..(list.len() / 4) as u32, 0, *first_instance..first_instance + instances);
                                            }
                                        }
                                        Command::DrawIndexed { indices, instances, vertex_offset, first_instance, .. } => {
                                            if let Some(b) = fans.get(&i) {
                                                pass.set_index_buffer(b.slice(..), wgpu::IndexFormat::Uint32);
                                                pass.draw_indexed(0..3 * (indices - 2), *vertex_offset, *first_instance..first_instance + instances);
                                            }
                                        }
                                        _ => return Err("an indirect draw of a triangle fan".into()),
                                    }
                                    i += 1;
                                    continue;
                                }
                                let indexed = matches!(c, Command::DrawIndexed { .. } | Command::DrawIndirect { indexed: true, .. });
                                if indexed {
                                    let (h, offset, f) = index.ok_or("an indexed draw with no index buffer")?;
                                    pass.set_index_buffer(buf(h)?.slice(offset..), f);
                                }
                                match c {
                                    Command::Draw([n, instances, first, first_instance]) => pass.draw(*first..first + n, *first_instance..first_instance + instances),
                                    Command::DrawIndexed { indices, instances, first, vertex_offset, first_instance } => {
                                        pass.draw_indexed(*first..first + indices, *vertex_offset, *first_instance..first_instance + instances)
                                    }
                                    Command::DrawIndirect { buffer, offset, count, stride, indexed } => {
                                        for k in 0..*count as u64 {
                                            let at = offset + k * *stride as u64;
                                            if *indexed { pass.draw_indexed_indirect(buf(*buffer)?, at) } else { pass.draw_indirect(buf(*buffer)?, at) }
                                        }
                                    }
                                    _ => unreachable!(),
                                }
                            }
                            c => return Err(format!("{c:?} in a render pass")),
                        }
                        i += 1;
                    }
                    if spanning.is_some() {
                        pass.end_occlusion_query();
                    }
                    drop(pass);
                    // (Storage texel buffers it may have written, back to their
                    // buffers.)
                    for t in texels_of(graphics_sets.values().flatten()) {
                        let t = texel(t)?;
                        write_back(&mut enc, buf(t.buffer)?, t);
                    }
                }
            }
            c @ (Command::EndPass
            | Command::SetViewport(_)
            | Command::SetScissor(_)
            | Command::SetBlendConstants(_)
            | Command::SetStencilReference(_)
            | Command::SetDepthBias(_)
            | Command::SetStencilCompareMask(_)
            | Command::SetStencilWriteMask(_)) => {
                // (Dynamic state set outside a pass: Vulkan keeps it for the
                // next; the driver sets it again in each pass.)
                if *c == Command::EndPass {
                    return Err("a render pass's end with none begun".into());
                }
            }
            Command::Blit { src, src_level, from, dst, dst_level, to, linear } => blit(gpu, &mut enc, tex(*src)?, *src_level, *from, tex(*dst)?, *dst_level, *to, *linear)?,
            Command::ClearDepth { image, mip, mips, layer, layers, aspect: bits, depth, stencil } => {
                let t = tex(*image)?;
                let f = t.format();
                if !t.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT) || !f.is_depth_stencil_format() {
                    return Err(format!("clearing the depth of a {f:?} image"));
                }
                // (A render pass per mip level and layer, clearing the
                // aspects asked for and keeping the others.)
                let ops = |has: bool, asked: bool| has.then_some(asked);
                let (depth_ops, stencil_ops) = (ops(f.has_depth_aspect(), bits & 2 != 0), ops(f.has_stencil_aspect(), bits & 4 != 0));
                for m in *mip..mip + mips {
                    for l in *layer..layer + layers {
                        let v = t.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::D2),
                            base_mip_level: m,
                            mip_level_count: Some(1),
                            base_array_layer: l,
                            array_layer_count: Some(1),
                            ..Default::default()
                        });
                        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: None,
                            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                                view: &v,
                                depth_ops: depth_ops.map(|c| wgpu::Operations { load: if c { wgpu::LoadOp::Clear(*depth) } else { wgpu::LoadOp::Load }, store: wgpu::StoreOp::Store }),
                                stencil_ops: stencil_ops.map(|c| wgpu::Operations { load: if c { wgpu::LoadOp::Clear(*stencil) } else { wgpu::LoadOp::Load }, store: wgpu::StoreOp::Store }),
                            }),
                            ..Default::default()
                        });
                    }
                }
            }
            Command::Resolve { src, src_level: [sm, sl], dst, dst_level: [dm, dl], layers } => {
                let (s, d) = (tex(*src)?, tex(*dst)?);
                if s.sample_count() <= 1 || d.sample_count() != 1 || s.format() != d.format() {
                    return Err(format!("resolving a {:?} image of {} samples into a {:?} one of {}", s.format(), s.sample_count(), d.format(), d.sample_count()));
                }
                // (A render pass that keeps the source and resolves it.)
                let level = |t: &wgpu::Texture, mip, layer| {
                    t.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_mip_level: mip,
                        mip_level_count: Some(1),
                        base_array_layer: layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                };
                for k in 0..*layers {
                    let (a, b) = (level(s, *sm, sl + k), level(d, *dm, dl + k));
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &a,
                            depth_slice: None,
                            resolve_target: Some(&b),
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        ..Default::default()
                    });
                }
            }
            Command::ClearAttachments { .. } => return Err("attachments cleared outside a render pass".into()),
            // (Occlusion queries over more than one render pass: not yet.)
            // (An occlusion query around render passes: each counts into a
            // slot of the pool's overflow set; summed as it's read back.)
            Command::BeginQuery { pool, index } => spanning = Some((*pool, *index, Vec::new())),
            Command::EndQuery { pool, index } => {
                if let Some((p, k, slots)) = spanning.take()
                    && (p, k) == (*pool, *index)
                {
                    ended.entry(p).or_default().insert(k, true);
                    spans.push((p, k, slots));
                }
            }
            Command::Timestamp { pool, index } => {
                enc.write_timestamp(&queries(*pool)?.set, *index);
                ended.entry(*pool).or_default().insert(*index, true);
            }
            Command::ResetQueries { pool, first, count } => {
                let e = ended.entry(*pool).or_default();
                for k in *first..first + count {
                    e.insert(k, false);
                }
            }
            Command::CopyQueries { pool, first, count, buffer, offset, stride, flags } => {
                let q = queries(*pool)?;
                if first + count > q.count {
                    return Err(format!("queries {first} to {} of {}", first + count, q.count));
                }
                // (Resolved as they are now, then each moved to its place:
                // 64 or 32 bits, with its availability after it if asked.)
                enc.resolve_query_set(&q.set, 0..first + count, &q.resolved, 0);
                let wide = flags & 1 != 0;
                let size = if wide { 8 } else { 4 };
                let earlier = q.values.lock().unwrap().clone();
                let mut available = Vec::new();
                for k in *first..first + count {
                    let now = ended.get(pool).and_then(|e| e.get(&k).copied());
                    let a = now.unwrap_or(earlier[k as usize].is_some()) as u64;
                    let bytes = a.to_le_bytes();
                    available.extend_from_slice(&bytes[..size as usize]);
                }
                let words = staged(&available);
                let dst = buf(*buffer)?;
                for (j, k) in (*first..first + count).enumerate() {
                    let at = offset + j as u64 * stride;
                    enc.copy_buffer_to_buffer(&q.resolved, k as u64 * 8, dst, at, size);
                    if flags & 4 != 0 {
                        enc.copy_buffer_to_buffer(&words, j as u64 * size, dst, at + size, size);
                    }
                }
            }
            c @ (Command::Draw(_) | Command::DrawIndexed { .. } | Command::DrawIndirect { .. }) => return Err(format!("{c:?} outside a render pass")),
        }
        i += 1;
    }
    // (The queries it ended resolved, to be read back once it's done.)
    let mut read_back = Vec::new();
    for (&h, e) in &ended {
        let q = queries(h)?;
        if let Some(&last) = e.iter().filter(|(_, on)| **on).map(|(k, _)| k).last() {
            enc.resolve_query_set(&q.set, 0..last + 1, &q.resolved, 0);
            read_back.push((q, e, last + 1));
        } else {
            read_back.push((q, e, 0));
        }
    }
    for (&h, &n) in &spill_next {
        if let Some((set, resolved)) = &queries(h)?.spill {
            enc.resolve_query_set(set, 0..n, resolved, 0);
        }
    }
    gpu.queue.submit([enc.finish()]);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(describe(&e));
    }
    // (Queries around render passes: their passes' counts summed.)
    let mut sums: Vec<(&QueryPool, u32, u64)> = Vec::new();
    for (h, k, slots) in &spans {
        let q = queries(*h)?;
        let Some((_, resolved)) = &q.spill else { continue };
        let bytes = gpu.read(resolved, 0, spill_next[h] as u64 * 8)?;
        let sum = slots.iter().map(|&s| u64::from_le_bytes(bytes[s as usize * 8..][..8].try_into().unwrap())).sum();
        sums.push((q, *k, sum));
    }
    for (q, e, n) in read_back {
        let bytes = gpu.read(&q.resolved, 0, n as u64 * 8)?;
        let mut values = q.values.lock().unwrap();
        for (&k, &on) in e {
            values[k as usize] = on.then(|| u64::from_le_bytes(bytes[k as usize * 8..][..8].try_into().unwrap()));
        }
    }
    for (q, k, sum) in sums {
        q.values.lock().unwrap()[k as usize] = Some(sum);
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
        let p = Arc::new(compute_pipeline(&g, &s, "main", &[]).unwrap());
        let (src, mem, out) = (g.buffer(64).unwrap(), g.buffer(512).unwrap(), g.buffer(64).unwrap());
        let buffers: HashMap<u64, &wgpu::Buffer> = [(1, &src), (2, &mem), (3, &out)].into();
        let input: Vec<u8> = (0..16u32).flat_map(|i| (i * 10).to_le_bytes()).collect();
        let commands = [
            Command::Update { dst: 1, offset: 0, data: input },
            Command::Fill { dst: 2, offset: 0, size: 512, word: 0xdead_beef },
            Command::Push { offset: 0, data: 3u32.to_le_bytes().to_vec() },
            Command::Push { offset: 4, data: 5u32.to_le_bytes().to_vec() },
            Command::BindPipeline(9),
            Command::BindSet { set: 0, bindings: vec![Binding { binding: 0, element: 0, resource: Resource::Buffer { buffer: 1, offset: 0, size: 64 } }] },
            // (dst at 256 bytes into its memory.)
            Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, element: 0, resource: Resource::Buffer { buffer: 2, offset: 256, size: 64 } }] },
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
            [Command::BindPipeline(7), Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, element: 0, resource: Resource::Buffer { buffer: 5, offset: 16, size: 32 } }] }, Command::Dispatch([2, 1, 1])]
        );
        assert!(decode(&s[..s.len() - 1]).is_err());
    }

    #[test]
    fn a_render_pass_clears_and_draws_within_the_scissor() {
        let Ok(g) = Gpu::open_for(crate::device::Api::Vulkan) else { return };
        let s = shader(
            &g,
            &spirv(
                "struct C { color: vec4<f32> }
@group(0) @binding(0) var<uniform> c: C;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32(i32(i & 1u) * 4 - 1), f32(i32(i >> 1u) * 4 - 1), 0.5, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return c.color; }",
            ),
        )
        .unwrap();
        // (R8G8B8A8_UNORM, written in full, not blended.)
        let state = GraphicsState { vertex: (1, "vs".into()), fragment: (1, "fs".into()), topology: 3, colors: vec![[37, 0, 0, 0, 0, 0, 0, 0, 0xf]], ..Default::default() };
        let p = Arc::new(graphics_pipeline(&g, &state, |_| Some(&s)).unwrap());
        let t = image(&g, 2, 37, [8, 8, 1], 1, 1, 1, 0x10 | 0x1).unwrap();
        let v = view(&t, 1, 0, 1, 0, 1, 0, 1).unwrap();
        let (uniform, out) = (g.buffer(16).unwrap(), g.buffer(8 * 8 * 4).unwrap());
        let green: Vec<u8> = [0f32, 1., 0., 1.].iter().flat_map(|f| f.to_le_bytes()).collect();
        let one = 1f32.to_bits();
        let commands = [
            Command::Update { dst: 1, offset: 0, data: green },
            Command::BindPipeline(9),
            Command::BeginPass { colors: vec![Attachment { view: 4, load: 1, store: 0, clear: [one, 0, 0, one], resolve: 0, firsts: 0 }], depth: None, views: 0 },
            Command::BindGraphicsSet { set: 0, bindings: vec![Binding { binding: 0, element: 0, resource: Resource::Buffer { buffer: 1, offset: 0, size: 16 } }] },
            Command::SetScissor([0, 0, 4, 8]),
            Command::Draw([3, 1, 0, 0]),
            Command::EndPass,
            Command::ImageToBuffer(Region { buffer: 2, offset: 0, image: 3, row_length: 0, image_height: 0, sub: Sub { mip: 0, layer: 0, layers: 1, aspect: 1, origin: [0; 3], extent: [8, 8, 1] } }),
        ];
        submit(&g, &commands, |h| match h {
            1 => Some(Object::Buffer(&uniform)),
            2 => Some(Object::Buffer(&out)),
            3 => Some(Object::Texture(&t)),
            4 => Some(Object::View(&v)),
            9 => Some(Object::Pipeline(p.clone())),
            _ => None,
        })
        .unwrap();
        let px = g.read(&out, 0, 8 * 8 * 4).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                let want = if x < 4 { [0, 255, 0, 255] } else { [255, 0, 0, 255] };
                assert_eq!(px[(y * 8 + x) * 4..][..4], want, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn graphics_state_decodes_from_its_words() {
        let mut w: Vec<u32> = vec![5, 0, 6, 0, 4, 2];
        w.extend([3, 2, 1]); // topology, cull, front face
        w.extend([1, 2f32.to_bits(), 0.5f32.to_bits(), 0]); // bias
        w.extend([1, 1, 1]); // depth
        w.push(0);
        w.extend([0; 12]); // stencil
        w.extend([126, 1, 1, 1, 2]); // depth format, samples; counts
        w.extend([37, 0, 0, 0, 0, 0, 0, 0, 0xf]);
        w.extend([0, 24, 0]);
        w.extend([0, 0, 106, 0, 1, 0, 106, 12]);
        w.extend([u32::from_le_bytes(*b"main"), u32::from_le_bytes(*b"fs\0\0")]);
        w.extend([1, 7, 4, 1f32.to_bits(), 0, 0]); // specialization: the vertex shader's one, the fragment's none
        w.push(1 | 4); // dynamic: depth bias, stencil write mask
        let bytes: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
        let s = GraphicsState::decode(&bytes).unwrap();
        assert_eq!((s.vertex, s.fragment), ((5, "main".into()), (6, "fs".into())));
        assert_eq!((s.topology, s.cull, s.front_face, s.bias), (3, 2, 1, (true, 2., 0.5, 0.)));
        assert_eq!((s.depth, s.depth_format, s.samples), ((true, true, 1), 126, 1));
        assert_eq!(s.dynamic, 5);
        assert_eq!((s.colors.len(), s.buffers, s.attributes), (1, vec![[0, 24, 0]], vec![[0, 0, 106, 0], [1, 0, 106, 12]]));
        assert_eq!((s.vertex_spec, s.fragment_spec), (vec![[7, 4, 1f32.to_bits(), 0]], vec![]));
        assert!(GraphicsState::decode(&bytes[..bytes.len() - 4]).is_err());
    }
}
