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
    let mut module = naga::front::spv::parse_u8_slice(spirv, &options).map_err(|e| format!("SPIR-V: {e}"))?;
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

/// A buffer descriptor: binding `binding` of its set is `size` bytes of
/// `buffer` from `offset`.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub binding: u32,
    pub buffer: u64,
    pub offset: u64,
    pub size: u64,
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
}

/// A command buffer's commands from the stream the guest recorded: each a
/// u32 operation and its length in bytes (with these 8), then its operands,
/// little-endian:
///
///   1 BIND_PIPELINE  pipeline (u64)
///   2 BIND_SET       set, count (u32), then count × binding, 0 (u32),
///                    buffer, offset, size (u64)
///   3 PUSH           offset, length (u32), the bytes
///   4 DISPATCH       x, y, z (u32)
///   5 DISPATCH_INDIRECT  buffer, offset (u64)
///   6 COPY           source, offset, destination, offset, size (u64)
///   7 FILL           buffer, offset, size (u64), word (u32)
///   8 UPDATE         buffer, offset (u64), length (u32), the bytes
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
                        let binding = r.u32()?;
                        r.u32()?;
                        Ok(Binding { binding, buffer: r.u64()?, offset: r.u64()?, size: r.u64()? })
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
}

/// Runs commands in order, as one submission: `buffer` and `pipeline` find
/// what their handles name.
pub fn submit<'a>(gpu: &Gpu, commands: &[Command], buffer: impl Fn(u64) -> Option<&'a wgpu::Buffer>, pipeline: impl Fn(u64) -> Option<Arc<Pipeline>>) -> Res<()> {
    let buf = |h: u64| buffer(h).ok_or_else(|| format!("no buffer {h}"));
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
            Command::BindPipeline(h) => bound = Some(pipeline(*h).ok_or_else(|| format!("no pipeline {h}"))?),
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
                        .map(|&b| {
                            let d = set.iter().find(|d| d.binding == b).ok_or_else(|| format!("set {g} has no binding {b}"))?;
                            Ok(wgpu::BindGroupEntry {
                                binding: b,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: buf(d.buffer)?, offset: d.offset, size: wgpu::BufferSize::new(d.size) }),
                            })
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
            Command::BindSet { set: 0, bindings: vec![Binding { binding: 0, buffer: 1, offset: 0, size: 64 }] },
            // (dst at 256 bytes into its memory.)
            Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, buffer: 2, offset: 256, size: 64 }] },
            Command::Dispatch([1, 1, 1]),
            Command::Copy { src: 2, src_offset: 256, dst: 3, dst_offset: 0, size: 64 },
        ];
        submit(&g, &commands, |h| buffers.get(&h).copied(), |h| (h == 9).then(|| p.clone())).unwrap();
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
            [Command::BindPipeline(7), Command::BindSet { set: 1, bindings: vec![Binding { binding: 2, buffer: 5, offset: 16, size: 32 }] }, Command::Dispatch([2, 1, 1])]
        );
        assert!(decode(&s[..s.len() - 1]).is_err());
    }
}
