//! The GPU itself, through wgpu: the host's best adapter (Vulkan, Metal or
//! DirectX 12), or a software one (lavapipe) when `WASM_ENV_GPU=software`
//! or there's no other. Buffers are wgpu storage buffers; a kernel launch
//! is one or more compute dispatches of the kernel's WGSL (wgsl.rs),
//! compiled once per work-group size and local-memory sizes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wgpu::util::DeviceExt;

use crate::ir::{Module, Res};
use crate::wgsl::{self, ArgKind, HEADER_WORDS, KernelInfo, Launch, Quirks};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
    pub quirks: Quirks,
    pipelines: Mutex<HashMap<(usize, String, Launch), Arc<Pipeline>>>,
    lost: std::sync::atomic::AtomicBool,
}

struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// The buffer arguments bound (binding k + 1 for argument k).
    buffers: Vec<usize>,
}

/// A buffer argument for a launch: the buffer (its offset, for a
/// sub-buffer, is in the argument's bytes).
pub type Bound<'a> = (usize, &'a wgpu::Buffer);

impl Gpu {
    pub fn open() -> Res<Gpu> {
        // lavapipe (llvmpipe) at its default 256-bit width miscompiles
        // large shaders (software doubles: wrong results, and builds four
        // times as long); at 128 bits it doesn't. It reads this when it
        // starts, so before the instance is made.
        if std::env::var_os("LP_NATIVE_VECTOR_WIDTH").is_none() {
            // SAFETY: while the GPU opens, before wgpu or its drivers read the
            // environment from other threads.
            unsafe { std::env::set_var("LP_NATIVE_VECTOR_WIDTH", "128") };
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        // WASM_ENV_GPU: "software", or part of an adapter's name ("intel") to
        // prefer it; else the most capable adapter.
        let wanted = std::env::var("WASM_ENV_GPU").unwrap_or_default().to_lowercase();
        let software = wanted == "software";
        let rank = |a: &wgpu::Adapter| {
            let t = a.get_info().device_type;
            let hardware = match t {
                wgpu::DeviceType::DiscreteGpu => 4,
                wgpu::DeviceType::IntegratedGpu => 3,
                wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
                wgpu::DeviceType::Cpu => 1,
            };
            let backend = if a.get_info().backend == wgpu::Backend::Gl { 0 } else { 1 };
            let chosen = if software { t == wgpu::DeviceType::Cpu } else { wanted.is_empty() || a.get_info().name.to_lowercase().contains(&wanted) };
            (chosen, backend, hardware)
        };
        let adapter = adapters.into_iter().filter(|a| a.get_info().backend != wgpu::Backend::Gl).max_by_key(rank).ok_or("no GPU adapter (not even a software one)")?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        // Doubles natively where the device has f64 and 64-bit integers (to
        // get at their bits), unless WASM_ENV_GPU_SOFT_F64 asks for the
        // software ones (f64.rs) everywhere.
        let f64_features = wgpu::Features::SHADER_F64 | wgpu::Features::SHADER_INT64;
        let native_f64 = adapter.features().contains(f64_features) && std::env::var_os("WASM_ENV_GPU_SOFT_F64").is_none();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wasm-env"),
            required_features: if native_f64 { f64_features } else { wgpu::Features::empty() },
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("can't open {}: {e}", info.name))?;
        // Errors are reported to the caller (error scopes), not panics.
        device.on_uncaptured_error(Arc::new(|e| eprintln!("wasm-env: GPU: {}", chain(&e))));
        let mut g = Gpu { device, queue, info, limits, quirks: Quirks::default(), pipelines: Mutex::new(HashMap::new()), lost: Default::default() };
        g.quirks = Quirks { native_f64: native_f64 && g.native_doubles_conform()?, ..g.probe_quirks()? };
        Ok(g)
    }

    /// What the device does that kernels must make up for (`Quirks`), by
    /// trying it: -2^-126 * -(1 - 2^-24), 2^-126 rounded to the denormals'
    /// precision, from a buffer (so no compiler folds it).
    fn probe_quirks(&self) -> Res<Quirks> {
        let code = "@group(0) @binding(0) var<storage, read_write> b: array<f32>;
@compute @workgroup_size(1) fn main() { b[2] = b[0] * b[1]; }";
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("probe"), source: wgpu::ShaderSource::Wgsl(code.into()) });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let b = self.buffer(12)?;
        let words = [(-f32::MIN_POSITIVE).to_bits(), (-(1.0f32 - f32::EPSILON / 2.0)).to_bits(), 1];
        self.write(&b, 0, &words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>())?;
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        self.queue.submit([enc.finish()]);
        let product = f32::from_le_bytes(self.read(&b, 8, 4)?.try_into().unwrap());
        Ok(Quirks { flushes_rounded_up_products: product == 0.0, ..Quirks::default() })
    }

    /// Whether the device's own doubles are OpenCL's: denormals kept
    /// (5e-324 + 0) and conversions not folded away ((double)(float)0.1 is
    /// not 0.1). lavapipe's are neither, so it gets f64.rs's.
    fn native_doubles_conform(&self) -> Res<bool> {
        let code = format!(
            "{}\n@group(0) @binding(0) var<storage, read_write> b: array<u32>;
@compute @workgroup_size(1) fn main() {{
    let x = vec2<u32>(b[0], b[1]);
    let y = vec2<u32>(b[2], b[3]);
    let s = f64_add(x, vec2<u32>(0u, 0u));
    let r = f64_from_f32(f64_to_f32(y));
    b[4] = s.x; b[5] = s.y; b[6] = r.x; b[7] = r.y;
}}",
            wgsl::library_code(&["f64_add", "f64_from_f32", "f64_to_f32"].into_iter().collect(), true)
        );
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("probe"), source: wgpu::ShaderSource::Wgsl(code.into()) });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let b = self.buffer(32)?;
        let (tiny, tenth) = (1u64, 0.1f64.to_bits());
        let words = [tiny as u32, (tiny >> 32) as u32, tenth as u32, (tenth >> 32) as u32, 0, 0, 0, 0];
        self.write(&b, 0, &words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>())?;
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        self.queue.submit([enc.finish()]);
        let out: Vec<u64> = self.read(&b, 16, 16)?.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
        Ok(out[0] == tiny && out[1] == (0.1f32 as f64).to_bits())
    }

    pub fn buffer(&self, size: u64) -> Res<wgpu::Buffer> {
        if size > self.limits.max_buffer_size {
            return Err(format!("a buffer of {size} bytes (at most {})", self.limits.max_buffer_size));
        }
        Ok(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: size.max(4).next_multiple_of(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }))
    }

    /// Bytes into a buffer (at any offset: partial words at the ends are
    /// read first).
    pub fn write(&self, b: &wgpu::Buffer, offset: u64, data: &[u8]) -> Res<()> {
        if data.is_empty() {
            return Ok(());
        }
        let start = offset & !3;
        let end = (offset + data.len() as u64).next_multiple_of(4);
        if start == offset && end == offset + data.len() as u64 {
            self.queue.write_buffer(b, offset, data);
            return Ok(());
        }
        let mut words = self.read(b, start, end - start)?;
        words[(offset - start) as usize..][..data.len()].copy_from_slice(data);
        self.queue.write_buffer(b, start, &words);
        Ok(())
    }

    /// Bytes from a buffer, once the work before has finished.
    pub fn read(&self, b: &wgpu::Buffer, offset: u64, len: u64) -> Res<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let start = offset & !3;
        let end = (offset + len).next_multiple_of(4).min(b.size());
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: end - start,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(b, start, &staging, 0, end - start);
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.wait()?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let out = data[(offset - start) as usize..][..len as usize].to_vec();
        drop(data);
        staging.unmap();
        Ok(out)
    }

    /// Copies bytes between buffers (through the host where they aren't
    /// word-aligned).
    pub fn copy(&self, src: &wgpu::Buffer, src_offset: u64, dst: &wgpu::Buffer, dst_offset: u64, len: u64) -> Res<()> {
        if len == 0 {
            return Ok(());
        }
        if (src_offset | dst_offset | len) % 4 == 0 {
            let mut enc = self.device.create_command_encoder(&Default::default());
            if src == dst {
                // Between sub-buffers of one buffer: wgpu copies only
                // between two, so through another.
                let temp = self.buffer(len)?;
                enc.copy_buffer_to_buffer(src, src_offset, &temp, 0, len);
                enc.copy_buffer_to_buffer(&temp, 0, dst, dst_offset, len);
            } else {
                enc.copy_buffer_to_buffer(src, src_offset, dst, dst_offset, len);
            }
            self.queue.submit([enc.finish()]);
            return Ok(());
        }
        let data = self.read(src, src_offset, len)?;
        self.write(dst, dst_offset, &data)
    }

    /// Waits for the GPU. A lost device (a hung kernel the driver reset)
    /// is an error from here on: wgpu panics if polled then.
    fn wait(&self) -> Res<()> {
        if self.lost.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("the GPU device was lost".into());
        }
        let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })));
        match polled {
            Ok(r) => r.map(|_| ()).map_err(|e| e.to_string()),
            Err(_) => {
                self.lost.store(true, std::sync::atomic::Ordering::Relaxed);
                Err("the GPU device was lost".into())
            }
        }
    }

    /// Waits for everything submitted.
    pub fn finish(&self) -> Res<()> {
        self.wait()
    }

    fn pipeline(&self, program: usize, m: &Module, info: &KernelInfo, launch: &Launch) -> Res<Arc<Pipeline>> {
        let key = (program, info.name.clone(), launch.clone());
        if let Some(p) = self.pipelines.lock().unwrap().get(&key) {
            return Ok(p.clone());
        }
        let start = std::time::Instant::now();
        let code = wgsl::compile(m, &info.name, launch, self.quirks)?;
        let translated = start.elapsed();
        if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
            eprintln!("wasm-env: kernel {} for {launch:?}:\n{code}", info.name);
        }
        let buffers: Vec<usize> = info.args.iter().enumerate().filter(|(_, a)| matches!(a.kind, ArgKind::Buffer { .. })).map(|(k, _)| k).collect();
        let entry = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let entries: Vec<_> = std::iter::once(entry(0, true)).chain(buffers.iter().map(|&k| entry(k as u32 + 1, false))).collect();
        let internal = self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &entries });
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(&info.name), source: wgpu::ShaderSource::Wgsl(code.clone().into()) });
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&info.name),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let (validation, internal) = (pollster::block_on(scope.pop()), pollster::block_on(internal.pop()));
        if let Some(e) = validation.or(internal) {
            return Err(format!("kernel {}: {}", info.name, chain(&e)));
        }
        if std::env::var_os("WASM_ENV_DEBUG_GPU_TIME").is_some() {
            eprintln!("wasm-env: GPU: kernel {}: WGSL {} bytes in {translated:?}, pipeline in {:?}", info.name, code.len(), start.elapsed() - translated);
        }
        let p = Arc::new(Pipeline { pipeline, layout, buffers });
        self.pipelines.lock().unwrap().insert(key, p.clone());
        Ok(p)
    }

    /// Runs a kernel over `global` work-items (with `offset`), in groups of
    /// `launch.local`. `block` is the argument block from the first
    /// argument on (its header is filled in here); `buffers` gives each
    /// buffer argument's buffer. `program` identifies the module (for the
    /// pipeline cache).
    #[allow(clippy::too_many_arguments)]
    pub fn run(&self, program: usize, m: &Module, info: &KernelInfo, launch: &Launch, dims: u32, global: [u32; 3], offset: [u32; 3], block: &[u8], buffers: &[Bound]) -> Res<()> {
        let p = self.pipeline(program, m, info, launch)?;
        let groups: [u32; 3] = std::array::from_fn(|d| global[d] / launch.local[d].max(1));
        if groups.contains(&0) {
            return Ok(());
        }
        let max = self.limits.max_compute_workgroups_per_dimension.max(1);
        let mut enc = self.device.create_command_encoder(&Default::default());
        let chunks = |n: u32| (0..n.div_ceil(max)).map(move |c| (c * max, (n - c * max).min(max)));
        for (gz, nz) in chunks(groups[2]) {
            for (gy, ny) in chunks(groups[1]) {
                for (gx, nx) in chunks(groups[0]) {
                    let mut data = vec![0u8; (HEADER_WORDS * 4) as usize];
                    let words = [offset[0], offset[1], offset[2], global[0], global[1], global[2], dims, gx, gy, gz];
                    for (k, w) in words.iter().enumerate() {
                        data[k * 4..k * 4 + 4].copy_from_slice(&w.to_le_bytes());
                    }
                    data.extend_from_slice(block);
                    data.resize(data.len().next_multiple_of(4).max(4), 0);
                    let args = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &data, usage: wgpu::BufferUsages::STORAGE });
                    let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: args.as_entire_binding() }];
                    for &k in &p.buffers {
                        let b = buffers.iter().find(|(a, _)| *a == k).map(|(_, b)| *b).ok_or_else(|| format!("kernel {}: argument {k} not set", info.name))?;
                        entries.push(wgpu::BindGroupEntry { binding: k as u32 + 1, resource: b.as_entire_binding() });
                    }
                    let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.layout, entries: &entries });
                    let mut pass = enc.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&p.pipeline);
                    pass.set_bind_group(0, &group, &[]);
                    pass.dispatch_workgroups(nx, ny, nz);
                }
            }
        }
        self.queue.submit([enc.finish()]);
        Ok(())
    }
}

/// A work-group size for a launch that doesn't give one: as many
/// work-items as divide the global size evenly, up to `max` in all.
pub fn choose_local(global: [u32; 3], dims: u32, max: u32) -> [u32; 3] {
    let mut local = [1u32; 3];
    let mut room = max.max(1);
    for d in 0..dims.min(3) as usize {
        let mut l = room.min(global[d].max(1));
        while l > 1 && global[d] % l != 0 {
            l -= 1;
        }
        local[d] = l;
        room /= l;
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencl::{build, test_clang};

    fn gpu() -> Option<Gpu> {
        match Gpu::open() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("no GPU to test with: {e}");
                None
            }
        }
    }

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
    }

    fn bytes<T: Copy>(v: &[T]) -> Vec<u8> {
        // SAFETY: plain numbers.
        unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }.to_vec()
    }

    #[test]
    fn kernels_run_and_agree_with_the_cpu() {
        let Some(g) = gpu() else { return };
        eprintln!("testing on {} ({:?}, {:?})", g.info.name, g.info.device_type, g.info.backend);
        let src = r#"
__kernel void add(__global const float *a, __global const float *b, __global float *c, int n) {
    int i = get_global_id(0);
    if (i < n) c[i] = a[i] + b[i];
}
__kernel void reduce(__global const int *in, __global int *out, __local int *tmp) {
    int l = get_local_id(0);
    tmp[l] = in[get_global_id(0)];
    barrier(CLK_LOCAL_MEM_FENCE);
    for (int s = get_local_size(0) / 2; s > 0; s >>= 1) {
        if (l < s) tmp[l] += tmp[l + s];
        barrier(CLK_LOCAL_MEM_FENCE);
    }
    if (l == 0) atomic_add(out, tmp[0]);
}
__kernel void bytes(__global uchar *p) {
    size_t i = get_global_id(0);
    p[i] = p[i] * 3 + 1;
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 100_000u32;
        let a: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();
        let b: Vec<f32> = (0..n).map(|i| 1.0 - i as f32).collect();
        let (ba, bb, bc) = (g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 4).unwrap());
        g.write(&ba, 0, &bytes(&a)).unwrap();
        g.write(&bb, 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "add").unwrap();
        let mut block = vec![0u8; 16];
        block[12..16].copy_from_slice(&(n as i32).to_le_bytes());
        let launch = Launch { local: choose_local([n, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &block, &[(0, &ba), (1, &bb), (2, &bc)]).unwrap();
        let c = words(&g.read(&bc, 0, n as u64 * 4).unwrap());
        for i in 0..n as usize {
            assert_eq!(f32::from_bits(c[i]), a[i] + b[i], "c[{i}]");
        }

        // A reduction in local memory, with barriers and an atomic.
        let input: Vec<i32> = (0..4096).map(|i| (i * 7 % 13) - 6).collect();
        let (bi, bo) = (g.buffer(4096 * 4).unwrap(), g.buffer(4).unwrap());
        g.write(&bi, 0, &bytes(&input)).unwrap();
        g.write(&bo, 0, &[0; 4]).unwrap();
        let info = wgsl::kernel_info(&m, "reduce").unwrap();
        let launch = Launch { local: [128, 1, 1], local_bytes: vec![(2, 128 * 4)] };
        g.run(1, &m, &info, &launch, 1, [4096, 1, 1], [0; 3], &[0u8; 8], &[(0, &bi), (1, &bo)]).unwrap();
        let sum = i32::from_le_bytes(g.read(&bo, 0, 4).unwrap().try_into().unwrap());
        assert_eq!(sum, input.iter().sum::<i32>());

        // Byte stores beside each other, by different work-items.
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let bp = g.buffer(1000).unwrap();
        g.write(&bp, 0, &data).unwrap();
        let info = wgsl::kernel_info(&m, "bytes").unwrap();
        let launch = Launch { local: choose_local([1000, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [1000, 1, 1], [0; 3], &[0u8; 4], &[(0, &bp)]).unwrap();
        let out = g.read(&bp, 0, 1000).unwrap();
        for i in 0..1000 {
            assert_eq!(out[i], data[i].wrapping_mul(3).wrapping_add(1), "p[{i}]");
        }
    }

    #[test]
    fn narrow_vectors_round_trip_through_memory() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void as4(__global char4 *src, __global uchar4 *dst) {
    int i = get_global_id(0);
    dst[i] = as_uchar4(src[i]);
}
__kernel void vst(__global short *src, __global short *dst) {
    int i = get_global_id(0);
    vstore2(vload2(i, src), i, dst + 1);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 256u32;
        let data: Vec<u8> = (0..n * 4).map(|i| (i * 37 + 11) as u8).collect();
        let (a, b) = (g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 4 + 8).unwrap());
        g.write(&a, 0, &data).unwrap();
        let launch = Launch { local: choose_local([n, 1, 1], 1, 64), local_bytes: vec![] };
        let info = wgsl::kernel_info(&m, "as4").unwrap();
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 8], &[(0, &a), (1, &b)]).unwrap();
        assert_eq!(g.read(&b, 0, n as u64 * 4).unwrap(), data, "as_uchar4");
        let n2 = n * 2 - 1;
        let launch = Launch { local: choose_local([n2 / 2, 1, 1], 1, 64), local_bytes: vec![] };
        let info = wgsl::kernel_info(&m, "vst").unwrap();
        g.write(&b, 0, &vec![0u8; n as usize * 4 + 8]).unwrap();
        g.run(1, &m, &info, &launch, 1, [n2 / 2, 1, 1], [0; 3], &[0u8; 8], &[(0, &a), (1, &b)]).unwrap();
        let out = g.read(&b, 2, (n2 / 2) as u64 * 4).unwrap();
        assert_eq!(out, data[..out.len()], "vstore2 at an odd element");
    }

    #[test]
    fn wide_vectors_work_lane_by_lane() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void wide(__global const float8 *a, __global const int16 *b, __global float8 *c, __global int16 *d, __global char8 *e) {
    int i = get_global_id(0);
    float8 x = a[i];
    c[i] = fmax(x * 2.0f, (float8)(3.0f)) + convert_float8(vload8(0, (__global const int *)&b[i]));
    int16 y = b[i];
    d[i] = select(y + 1, y * 3, y > 100) ^ (int16)(i);
    e[i] = convert_char8_sat(x);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64u32;
        let a: Vec<f32> = (0..n * 8).map(|k| (k as f32 - 200.0) * 0.75).collect();
        let b: Vec<i32> = (0..n * 16).map(|k| (k as i32 * 37) % 251 - 20).collect();
        let (ba, bb) = (g.buffer(n as u64 * 32).unwrap(), g.buffer(n as u64 * 64).unwrap());
        let (bc, bd, be) = (g.buffer(n as u64 * 32).unwrap(), g.buffer(n as u64 * 64).unwrap(), g.buffer(n as u64 * 8).unwrap());
        g.write(&ba, 0, &bytes(&a)).unwrap();
        g.write(&bb, 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "wide").unwrap();
        let launch = Launch { local: choose_local([n, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 20], &[(0, &ba), (1, &bb), (2, &bc), (3, &bd), (4, &be)]).unwrap();
        let c: Vec<f32> = words(&g.read(&bc, 0, n as u64 * 32).unwrap()).into_iter().map(f32::from_bits).collect();
        let d: Vec<i32> = words(&g.read(&bd, 0, n as u64 * 64).unwrap()).into_iter().map(|w| w as i32).collect();
        let e = g.read(&be, 0, n as u64 * 8).unwrap();
        for i in 0..n as usize {
            for k in 0..8 {
                let x = a[i * 8 + k];
                assert_eq!(c[i * 8 + k], (x * 2.0).max(3.0) + b[i * 16 + k] as f32, "c[{i}][{k}]");
                assert_eq!(e[i * 8 + k] as i8, x.clamp(-128.0, 127.0) as i8, "e[{i}][{k}]");
            }
            for k in 0..16 {
                let y = b[i * 16 + k];
                assert_eq!(d[i * 16 + k], (if y > 100 { y * 3 } else { y + 1 }) ^ i as i32, "d[{i}][{k}]");
            }
        }
    }

    #[test]
    fn long_integers_agree_with_the_cpu() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void longs(__global const long *a, __global const long *b, __global long *out, __global long4 *v) {
    int i = get_global_id(0);
    long x = a[i], y = b[i];
    ulong ux = x, uy = y;
    __global long *o = out + i * 16;
    o[0] = x + y;
    o[1] = x - y;
    o[2] = x * y;
    o[3] = y != 0 ? x / y : 0;
    o[4] = y != 0 ? x % y : 0;
    o[5] = uy != 0 ? (long)(ux / uy) : 0;
    o[6] = uy != 0 ? (long)(ux % uy) : 0;
    o[7] = x << (i & 63);
    o[8] = x >> (i & 63);
    o[9] = (long)(ux >> (i & 63));
    o[10] = (x < y) + 2 * (ux < uy) + 4 * (x == y);
    o[11] = min(x, y) ^ max(ux, uy);
    o[12] = clz(x);
    o[13] = convert_long((float)(i * 1000) - 5.5f);
    o[14] = (long)convert_int_sat(x);
    o[15] = (long)(convert_float(x) == (float)x);
    v[i] = (long4)(x, y, x & y, x | y) + (long4)(i);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 256usize;
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut a: Vec<i64> = (0..n).map(|_| next() as i64).collect();
        let mut b: Vec<i64> = (0..n).map(|_| next() as i64 >> (next() % 60)).collect();
        (a[0], b[0], a[1], b[1], a[2], b[2]) = (5, 0, -7, 2, i64::MIN, -1);
        let (ba, bb) = (g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 8).unwrap());
        let (bo, bv) = (g.buffer(n as u64 * 128).unwrap(), g.buffer(n as u64 * 32).unwrap());
        g.write(&ba, 0, &bytes(&a)).unwrap();
        g.write(&bb, 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "longs").unwrap();
        let launch = Launch { local: choose_local([n as u32, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 16], &[(0, &ba), (1, &bb), (2, &bo), (3, &bv)]).unwrap();
        let longs = |data: Vec<u8>| data.chunks(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect::<Vec<_>>();
        let (o, v) = (longs(g.read(&bo, 0, n as u64 * 128).unwrap()), longs(g.read(&bv, 0, n as u64 * 32).unwrap()));
        for i in 0..n {
            let (x, y) = (a[i], b[i]);
            let (ux, uy) = (x as u64, y as u64);
            let s = (i & 63) as u32;
            let want = [
                x.wrapping_add(y),
                x.wrapping_sub(y),
                x.wrapping_mul(y),
                if y != 0 { x.wrapping_div(y) } else { 0 },
                if y != 0 { x.wrapping_rem(y) } else { 0 },
                if uy != 0 { (ux / uy) as i64 } else { 0 },
                if uy != 0 { (ux % uy) as i64 } else { 0 },
                x << s,
                x >> s,
                (ux >> s) as i64,
                (x < y) as i64 + 2 * (ux < uy) as i64 + 4 * (x == y) as i64,
                x.min(y) ^ ux.max(uy) as i64,
                x.leading_zeros() as i64,
                ((i * 1000) as f32 - 5.5) as i64,
                x.clamp(i32::MIN as i64, i32::MAX as i64),
                1,
            ];
            for (k, w) in want.iter().enumerate() {
                assert_eq!(o[i * 16 + k], *w, "o[{i}][{k}] for {x}, {y}");
            }
            assert_eq!(v[i * 4..i * 4 + 4], [x + i as i64, y + i as i64, (x & y) + i as i64, (x | y) + i as i64].map(|z| z), "v[{i}]");
        }
    }

    #[test]
    fn async_copies_move_a_group_s_elements() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void copy(__global const float4 *src, __global float4 *dst, __local float4 *tmp, __global const char *cs, __global char *cd, __local char *ct) {
    int n = get_local_size(0) * 2, g = get_group_id(0);
    event_t e = async_work_group_copy(tmp, src + g * n, n, 0);
    event_t c = async_work_group_strided_copy(ct, cs + g * n * 3, n, 3, 0);
    wait_group_events(1, &e);
    wait_group_events(1, &c);
    tmp[get_local_id(0)] *= 2.0f;
    ct[get_local_id(0)] += 1;
    barrier(CLK_LOCAL_MEM_FENCE);
    e = async_work_group_strided_copy(dst + g * n * 2, tmp, n, 2, 0);
    c = async_work_group_copy(cd + g * n, ct, n, 0);
    wait_group_events(1, &e);
    wait_group_events(1, &c);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let (local, groups) = (16u32, 8u32);
        let n = (local * 2) as usize;
        let total = n * groups as usize;
        let a: Vec<f32> = (0..total * 4).map(|k| k as f32).collect();
        let cs: Vec<u8> = (0..total * 3).map(|k| (k * 7) as u8).collect();
        let (bs, bd) = (g.buffer(total as u64 * 16).unwrap(), g.buffer(total as u64 * 32).unwrap());
        let (bcs, bcd) = (g.buffer(total as u64 * 3).unwrap(), g.buffer(total as u64).unwrap());
        g.write(&bs, 0, &bytes(&a)).unwrap();
        g.write(&bcs, 0, &cs).unwrap();
        g.write(&bd, 0, &vec![0u8; total * 32]).unwrap();
        let info = wgsl::kernel_info(&m, "copy").unwrap();
        let launch = Launch { local: [local, 1, 1], local_bytes: vec![(2, n as u32 * 16), (5, n as u32)] };
        g.run(1, &m, &info, &launch, 1, [local * groups, 1, 1], [0; 3], &[0u8; 24], &[(0, &bs), (1, &bd), (3, &bcs), (4, &bcd)]).unwrap();
        let d: Vec<f32> = words(&g.read(&bd, 0, total as u64 * 32).unwrap()).into_iter().map(f32::from_bits).collect();
        let cd = g.read(&bcd, 0, total as u64).unwrap();
        for grp in 0..groups as usize {
            for e in 0..n {
                let doubled = e < local as usize;
                for l in 0..4 {
                    let x = a[(grp * n + e) * 4 + l];
                    assert_eq!(d[(grp * n * 2 + e * 2) * 4 + l], if doubled { x * 2.0 } else { x }, "dst group {grp} element {e}");
                }
                let c = cs[grp * n * 3 + e * 3];
                assert_eq!(cd[grp * n + e], if doubled { c.wrapping_add(1) } else { c }, "cd group {grp} element {e}");
            }
        }
    }

    #[test]
    fn compare_and_swap_loops_finish() {
        let Some(g) = gpu() else { return };
        // The CTS's add by compare-and-swap, in local and global memory:
        // every work-item spins until its swap lands.
        let src = r#"
__kernel void cas(__global int *dest, __global int *old, __local int *l) {
    int tid = get_global_id(0);
    if (get_local_id(0) == 0) l[0] = 0;
    barrier(CLK_LOCAL_MEM_FENCE);
    int o, orig;
    do {
        orig = l[0];
        o = atom_cmpxchg(&l[0], orig, orig + tid + 2);
    } while (o != orig);
    old[tid] = o;
    do {
        orig = dest[0];
        o = atom_cmpxchg(&dest[0], orig, orig + tid + 2);
    } while (o != orig);
    barrier(CLK_LOCAL_MEM_FENCE);
    if (get_local_id(0) == 0) dest[1] = l[0];
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 256u32;
        let (bd, bo) = (g.buffer(8).unwrap(), g.buffer(n as u64 * 4).unwrap());
        g.write(&bd, 0, &[0; 8]).unwrap();
        let info = wgsl::kernel_info(&m, "cas").unwrap();
        let launch = Launch { local: [n, 1, 1], local_bytes: vec![(2, 4)] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 12], &[(0, &bd), (1, &bo)]).unwrap();
        let d = words(&g.read(&bd, 0, 8).unwrap());
        let sum: u32 = (0..n).map(|t| t + 2).sum();
        assert_eq!(d, [sum, sum]);
    }

    #[test]
    fn saturation_and_nan_rules() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void k(__global const int *a, __global const int *b, __global int *o) {
    int i = get_global_id(0);
    int x = a[i], y = b[i];
    __global int *r = o + i * 12;
    r[0] = add_sat(x, y);
    r[1] = sub_sat(x, y);
    r[2] = add_sat((uint)x, (uint)y);
    r[3] = sub_sat((uint)x, (uint)y);
    r[4] = add_sat((char)x, (char)y);
    r[5] = sub_sat((uchar)x, (uchar)y);
    float fx = as_float(x), fy = as_float(y);
    r[6] = as_int(fmin(fx, fy));
    r[7] = as_int(fmax(fx, fy));
    float2 v = (float2)(fx, fy);
    int2 n = isnormal(v);
    r[8] = n.x;
    r[9] = n.y;
    r[10] = as_int(fmax(v, (float2)(fy, fx)).x);
    r[11] = mul_hi((int2)(x, y), (int2)(y, x)).y;
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let nan = 0x7fc0_0000u32 as i32;
        let pairs: [(i32, i32); 8] = [
            (i32::MAX, 1),
            (i32::MIN, 1),
            (-5, i32::MAX),
            (100, 100),
            (-100, -100),
            (1.5f32.to_bits() as i32, nan),
            (nan, (-2.0f32).to_bits() as i32),
            (1, 0x0080_0000),
        ];
        let n = pairs.len() as u32;
        let (a, b, o) = (g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 48).unwrap());
        g.write(&a, 0, &pairs.iter().flat_map(|p| p.0.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        g.write(&b, 0, &pairs.iter().flat_map(|p| p.1.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: [n, 1, 1], local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 12], &[(0, &a), (1, &b), (2, &o)]).unwrap();
        let out = words(&g.read(&o, 0, n as u64 * 48).unwrap());
        let fmin = |x: f32, y: f32| if x.is_nan() { y } else if y.is_nan() { x } else { x.min(y) };
        let fmax = |x: f32, y: f32| if x.is_nan() { y } else if y.is_nan() { x } else { x.max(y) };
        let normal = |x: f32| if x.is_normal() { -1 } else { 0 };
        for (k, &(x, y)) in pairs.iter().enumerate() {
            let (fx, fy) = (f32::from_bits(x as u32), f32::from_bits(y as u32));
            let want = [
                x.saturating_add(y),
                x.saturating_sub(y),
                (x as u32).saturating_add(y as u32) as i32,
                (x as u32).saturating_sub(y as u32) as i32,
                (x as i8).saturating_add(y as i8) as i32,
                (x as u8).saturating_sub(y as u8) as i32,
                fmin(fx, fy).to_bits() as i32,
                fmax(fx, fy).to_bits() as i32,
                normal(fx),
                normal(fy),
                fmax(fx, fy).to_bits() as i32,
                ((y as i64 * x as i64) >> 32) as i32,
            ];
            let got: Vec<i32> = out[k * 12..k * 12 + 12].iter().map(|&w| w as i32).collect();
            // fmin and fmax only of normal numbers and NaNs (a device
            // without denormals may flush them, and zeros' signs may go
            // either way).
            let plain = |f: f32| f.is_normal() || f.is_nan();
            let either_nan = |j: usize, w: i32| f32::from_bits(w as u32).is_nan() && f32::from_bits(got[j] as u32).is_nan();
            let want: Vec<i32> = want
                .iter()
                .enumerate()
                .map(|(j, &w)| if [6, 7, 10].contains(&j) && (!(plain(fx) && plain(fy)) || either_nan(j, w)) { got[j] } else { w })
                .collect();
            assert_eq!(got, want, "add_sat etc. of {x:#x}, {y:#x}");
        }
    }

    #[test]
    fn products_round_up_to_the_smallest_normal() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void k(__global const float *a, __global const float *b, __global float *o) {
    int i = get_global_id(0);
    o[i] = a[i] * b[i];
    float2 v = (float2)(a[i], 1.0f) * (float2)(b[i], 2.0f);
    o[i + 6] = v.x;
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let min = f32::MIN_POSITIVE;
        // 2^-126 - 2^-150 (a tie, to even: 2^-126); 2^-126 (1 - 2^-24 - 3 2^-47)
        // (just under the tie, a denormal, but scaled up it rounds onto it); 2^-126 * 0.75; 2 * 3; NaN and
        // infinity by something tiny.
        let a = [-min, f32::from_bits(0x0080_0001), min, 2.0f32, f32::NAN, f32::INFINITY];
        let b = [-(1.0 - f32::EPSILON / 2.0), 1.0 - 3.0 * f32::EPSILON / 2.0, 0.75, 3.0, -1e-30, 1e-30];
        let bytes = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>();
        let (ba, bb, bo) = (g.buffer(24).unwrap(), g.buffer(24).unwrap(), g.buffer(48).unwrap());
        g.write(&ba, 0, &bytes(&a)).unwrap();
        g.write(&bb, 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: [6, 1, 1], local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [6, 1, 1], [0; 3], &[0u8; 12], &[(0, &ba), (1, &bb), (2, &bo)]).unwrap();
        let out: Vec<f32> = words(&g.read(&bo, 0, 48).unwrap()).into_iter().map(f32::from_bits).collect();
        for k in [0, 6] {
            assert_eq!(out[k], min, "the tie");
            // (A denormal, or flushed.)
            assert!(out[k + 1] < min && out[k + 1] >= 0.0, "under the tie: {:e}", out[k + 1]);
            assert!(out[k + 2] < min, "three quarters");
            assert_eq!(out[k + 3], 6.0);
            assert!(out[k + 4].is_nan(), "NaN");
            assert_eq!(out[k + 5], f32::INFINITY);
        }
    }

    #[test]
    fn integer_builtins_agree_with_the_cpu() {
        let Some(g) = gpu() else { return };
        let src = r#"
__kernel void k(__global const long *a, __global const long *b, __global const long *c, __global long *o) {
    int i = get_global_id(0);
    long x = a[i], y = b[i], z = c[i];
    ulong ux = x, uy = y, uz = z;
    int xi = (int)x, yi = (int)y, zi = (int)z;
    __global long *r = o + i * 24;
    r[0] = mul_hi(x, y);
    r[1] = (long)mul_hi(ux, uy);
    r[2] = mad_hi(x, y, z);
    r[3] = add_sat(x, y);
    r[4] = (long)add_sat(ux, uy);
    r[5] = sub_sat(x, y);
    r[6] = (long)sub_sat(ux, uy);
    r[7] = mad_sat(x, y, z);
    r[8] = (long)mad_sat(ux, uy, uz);
    r[9] = (long)abs_diff(x, y);
    r[10] = (long)abs_diff(ux, uy);
    r[11] = hadd(x, y);
    r[12] = (long)rhadd(ux, uy);
    r[13] = (long)rotate(ux, uy);
    r[14] = (long)ctz(ux);
    r[15] = mad_sat(xi, yi, zi);
    r[16] = mad_sat((uint)xi, (uint)yi, (uint)zi);
    r[17] = abs_diff(xi, yi);
    r[18] = upsample(xi, (uint)yi);
    r[19] = upsample((short)xi, (ushort)yi);
    r[20] = mad_hi(xi, yi, zi);
    r[21] = mad_sat((short)xi, (short)yi, (short)zi);
    long2 v = mul_hi((long2)(x, y), (long2)(y, z));
    r[22] = v.y;
    r[23] = abs_diff((char)xi, (char)yi);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64usize;
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut v: Vec<[i64; 3]> = (0..n).map(|_| [next() as i64 >> (next() % 40), next() as i64 >> (next() % 40), next() as i64]).collect();
        v[0] = [i64::MAX, i64::MAX, 1];
        v[1] = [i64::MIN, -1, -1];
        v[2] = [i64::MIN, i64::MIN, i64::MAX];
        v[3] = [0x8c7f0aac, 0x1902f8c8, 0];
        let col = |k: usize| v.iter().flat_map(|t| t[k].to_le_bytes()).collect::<Vec<u8>>();
        let (ba, bb, bc, bo) = (g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 192).unwrap());
        g.write(&ba, 0, &col(0)).unwrap();
        g.write(&bb, 0, &col(1)).unwrap();
        g.write(&bc, 0, &col(2)).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: choose_local([n as u32, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 16], &[(0, &ba), (1, &bb), (2, &bc), (3, &bo)]).unwrap();
        let out: Vec<i64> = g.read(&bo, 0, n as u64 * 192).unwrap().chunks(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
        let sat = |r: i128| r.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        let usat = |r: u128| r.min(u64::MAX as u128) as u64 as i64;
        for (i, &[x, y, z]) in v.iter().enumerate() {
            let (ux, uy, uz) = (x as u64, y as u64, z as u64);
            let (xi, yi, zi) = (x as i32, y as i32, z as i32);
            let want = [
                ((x as i128 * y as i128) >> 64) as i64,
                ((ux as u128 * uy as u128) >> 64) as i64,
                (((x as i128 * y as i128) >> 64) as i64).wrapping_add(z),
                x.saturating_add(y),
                ux.saturating_add(uy) as i64,
                x.saturating_sub(y),
                ux.saturating_sub(uy) as i64,
                sat(x as i128 * y as i128 + z as i128),
                usat(ux as u128 * uy as u128 + uz as u128),
                (x as i128 - y as i128).unsigned_abs() as u64 as i64,
                ux.abs_diff(uy) as i64,
                ((x as i128 + y as i128) >> 1) as i64,
                ((ux as u128 + uy as u128 + 1) >> 1) as u64 as i64,
                ux.rotate_left((uy % 64) as u32) as i64,
                ux.trailing_zeros() as i64,
                (xi as i64 * yi as i64 + zi as i64).clamp(i32::MIN as i64, i32::MAX as i64),
                (xi as u32 as u64 * yi as u32 as u64 + zi as u32 as u64).min(u32::MAX as u64) as i64,
                xi.abs_diff(yi) as i64,
                ((xi as i64) << 32) | yi as u32 as i64,
                (((xi as i16 as i32) << 16) | yi as u16 as i32) as i64,
                (((xi as i64 * yi as i64) >> 32) as i32).wrapping_add(zi) as i64,
                (xi as i16 as i32 * yi as i16 as i32 + zi as i16 as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16 as i64,
                ((y as i128 * z as i128) >> 64) as i64,
                (xi as i8).abs_diff(yi as i8) as i64,
            ];
            assert_eq!(out[i * 24..i * 24 + 24], want, "builtins of {x:#x}, {y:#x}, {z:#x}");
        }
    }

    /// Runs WGSL with `input` at binding 0 and `out` words at binding 1, in
    /// `n` invocations of `main` (workgroups of 64).
    fn run_wgsl(g: &Gpu, code: &str, input: &[u32], out: usize, n: u32) -> Vec<u32> {
        let module = g.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(code.into()) });
        let pipeline = g.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let (bi, bo) = (g.buffer(input.len() as u64 * 4).unwrap(), g.buffer(out as u64 * 4).unwrap());
        g.write(&bi, 0, &input.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let group = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: bi.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: bo.as_entire_binding() }],
        });
        let mut enc = g.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(n.div_ceil(64), 1, 1);
        }
        g.queue.submit([enc.finish()]);
        words(&g.read(&bo, 0, out as u64 * 4).unwrap())
    }

    #[test]
    fn doubles_agree_with_the_cpu() {
        let Some(g) = gpu() else { return };
        check_doubles(&g, false);
        if g.quirks.native_f64 {
            check_doubles(&g, true);
        }
    }

    /// f64.rs's library, in software or native, against the CPU.
    fn check_doubles(g: &Gpu, native: bool) {
        let ops = [
            "f64_add(a, b)", "f64_sub(a, b)", "f64_mul(a, b)", "f64_div(a, b)", "f64_fma(a, b, c)", "f64_sqrt(a)", "f64_min(a, b)", "f64_max(a, b)",
            "f64_trunc(a)", "f64_floor(a)", "f64_ceil(a)", "f64_rint(a)", "f64_round_half_away(a)", "vec2<u32>(bitcast<u32>(f64_to_f32(a)), 0u)",
            "f64_from_f32(bitcast<f32>(a.y))", "vec2<u32>(select(0u, 1u, f64_lt(a, b)), select(0u, 1u, f64_eq(a, b)))", "f64_to_i64(a)", "f64_to_u64(a)",
            "f64_from_i64(a)", "f64_from_u64(a)",
        ];
        let names: std::collections::BTreeSet<&str> = ops.iter().flat_map(|o| o.split(|c: char| !c.is_alphanumeric() && c != '_')).filter(|w| w.starts_with("f64_")).collect();
        let mut code = wgsl::library_code(&names, native);
        code.push_str("@group(0) @binding(0) var<storage, read> inp: array<u32>;\n@group(0) @binding(1) var<storage, read_write> out: array<u32>;\n");
        code.push_str("fn put(k: u32, v: vec2<u32>) { out[k] = v.x; out[k + 1u] = v.y; }\n");
        code.push_str(&format!("@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n    let i = id.x;\n    if (i * 6u >= arrayLength(&inp)) {{ return; }}\n    let a = vec2<u32>(inp[i * 6u], inp[i * 6u + 1u]);\n    let b = vec2<u32>(inp[i * 6u + 2u], inp[i * 6u + 3u]);\n    let c = vec2<u32>(inp[i * 6u + 4u], inp[i * 6u + 5u]);\n    let o = i * {}u;\n", ops.len() * 2));
        for (k, op) in ops.iter().enumerate() {
            code.push_str(&format!("    put(o + {}u, {op});\n", k * 2));
        }
        code.push_str("}\n");
        let mut seed = 0x853c49e6748fea9bu64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let special = [0.0, -0.0, 1.0, -1.0, 0.5, 1.5, 2.5, -2.5, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, f64::MAX, f64::MIN_POSITIVE, 5e-324, -5e-324, 2.2250738585072009e-308, 4503599627370496.5, 9.2233720368547758e18, 1e300, 1e-300, 3.0, 0.1];
        let mut cases: Vec<[f64; 3]> = Vec::new();
        for &x in &special {
            for &y in &special {
                cases.push([x, y, special[cases.len() % special.len()]]);
            }
        }
        for k in 0..6000 {
            let r = |n: &mut dyn FnMut() -> u64| f64::from_bits(n());
            let c = match k % 6 {
                // Random bits.
                0 => [r(&mut next), r(&mut next), r(&mut next)],
                // Nearby exponents (cancellation), and c near a * b.
                1 => {
                    let x = r(&mut next);
                    let y = f64::from_bits(x.to_bits() ^ (next() & 0xf_ffff_ffff));
                    [x, y, -(x * y) * (1.0 + (next() % 7) as f64 * f64::EPSILON)]
                }
                // Moderate values.
                2 => [(next() as i64 >> 11) as f64 / 1e6, (next() as i64 >> 20) as f64 / 3.0, (next() as i64 >> 30) as f64],
                // Denormals.
                3 => [f64::from_bits(next() >> 12), f64::from_bits(next() >> 12 | (next() & 1) << 63), f64::from_bits(next() >> 13)],
                // Small and huge exponents.
                4 => [f64::from_bits((next() & 0x800f_ffff_ffff_ffff) | (next() % 60) << 52), f64::from_bits((next() & 0x800f_ffff_ffff_ffff) | (2000 + next() % 46) << 52), r(&mut next)],
                // Integers, halves.
                _ => [(next() % 2000) as f64 / 2.0 - 500.0, (next() as i64) as f64, f64::from_bits(next() & 0x433f_ffff_ffff_ffff)],
            };
            cases.push(c);
        }
        let input: Vec<u32> = cases.iter().flat_map(|c| c.iter().flat_map(|x| [x.to_bits() as u32, (x.to_bits() >> 32) as u32])).collect();
        let n = cases.len();
        let out = run_wgsl(g, &code, &input, n * ops.len() * 2, n as u32);
        let mut failures = Vec::new();
        for (i, &[a, b, c]) in cases.iter().enumerate() {
            let got = |k: usize| out[i * ops.len() * 2 + k * 2] as u64 | (out[i * ops.len() * 2 + k * 2 + 1] as u64) << 32;
            let af = f32::from_bits((a.to_bits() >> 32) as u32);
            let ai = a.to_bits() as i64;
            let want: [u64; 20] = [
                (a + b).to_bits(),
                (a - b).to_bits(),
                (a * b).to_bits(),
                (a / b).to_bits(),
                a.mul_add(b, c).to_bits(),
                a.sqrt().to_bits(),
                a.min(b).to_bits(),
                a.max(b).to_bits(),
                a.trunc().to_bits(),
                a.floor().to_bits(),
                a.ceil().to_bits(),
                a.round_ties_even().to_bits(),
                a.round().to_bits(),
                (a as f32).to_bits() as u64,
                (af as f64).to_bits(),
                (a < b) as u64 | ((a == b) as u64) << 32,
                a as i64 as u64,
                a as u64,
                ai as f64 as u64 as u64,
                (a.to_bits() as f64).to_bits(),
            ];
            let want = {
                let mut w = want;
                w[18] = (ai as f64).to_bits();
                w
            };
            for (k, &w) in want.iter().enumerate() {
                let r = got(k);
                let same = if k == 13 {
                    let (x, y) = (f32::from_bits(r as u32), f32::from_bits(w as u32));
                    r == w || (x.is_nan() && y.is_nan())
                } else if [15, 16, 17].contains(&k) {
                    r == w
                } else {
                    let (x, y) = (f64::from_bits(r), f64::from_bits(w));
                    // (Any NaN for a NaN; min and max of zeros, either zero.)
                    r == w || (x.is_nan() && y.is_nan()) || ((k == 6 || k == 7) && x == 0.0 && y == 0.0)
                };
                if !same && failures.len() < 20 {
                    failures.push(format!("{}: {a:e} ({:#x}), {b:e}, {c:e}: got {r:#x}, want {w:#x}", ops[k], a.to_bits()));
                }
            }
        }
        assert!(failures.is_empty(), "native {native}:\n{}", failures.join("\n"));
    }

    #[test]
    fn floats_round_to_halves() {
        let Some(g) = gpu() else { return };
        let mut code = wgsl::library_code(&["f32_to_f16"].into_iter().collect(), false);
        code.push_str("@group(0) @binding(0) var<storage, read> inp: array<u32>;\n@group(0) @binding(1) var<storage, read_write> out: array<u32>;\n");
        code.push_str("@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {\n    if (id.x < arrayLength(&inp)) { out[id.x] = f32_to_f16(bitcast<f32>(inp[id.x])); }\n}\n");
        // The reference: every half's value; the nearest, ties to the even.
        let decode = |h: u16| {
            let (s, e, m) = (if h & 0x8000 != 0 { -1.0 } else { 1.0 }, (h >> 10) & 0x1f, (h & 0x3ff) as f64);
            s * if e == 0 { m * 2f64.powi(-24) } else { (1024.0 + m) * 2f64.powi(e as i32 - 25) }
        };
        let mut finite: Vec<(f64, u16)> = (0..=0x7bffu16).map(|h| (decode(h), h)).collect();
        finite.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let reference = |f: f32| -> u16 {
            let sign = if f.is_sign_negative() { 0x8000 } else { 0 };
            if f.is_nan() {
                return 0x7e00;
            }
            let v = (f as f64).abs();
            // (Past the largest half by half its last bit: infinity.)
            if v >= 65520.0 {
                return sign | 0x7c00;
            }
            let k = finite.partition_point(|p| p.0 < v);
            let pick = if k == finite.len() {
                finite[k - 1].1
            } else if finite[k].0 == v || k == 0 {
                finite[k].1
            } else {
                let (lo, hi) = (finite[k - 1], finite[k]);
                let (dl, dh) = (v - lo.0, hi.0 - v);
                if dl < dh || (dl == dh && lo.1 & 1 == 0) { lo.1 } else { hi.1 }
            };
            sign | pick
        };
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut input: Vec<u32> = (0..40000).map(|_| next() as u32).collect();
        // Every half and its neighbours as floats, and the midpoints.
        for h in 0..=0x7c00u16 {
            let v = decode(h) as f32;
            input.extend([v.to_bits(), v.to_bits() + 1, v.to_bits().saturating_sub(1), (-v).to_bits()]);
            if h < 0x7c00 {
                input.push((((decode(h) + decode(h + 1)) / 2.0) as f32).to_bits());
            }
        }
        let out = run_wgsl(&g, &code, &input, input.len(), input.len() as u32);
        let mut bad = Vec::new();
        for (k, &u) in input.iter().enumerate() {
            let f = f32::from_bits(u);
            let want = reference(f);
            let got = out[k] as u16;
            let same = got == want || (f.is_nan() && got & 0x7c00 == 0x7c00 && got & 0x3ff != 0);
            if !same && bad.len() < 10 {
                bad.push(format!("{f:e} ({u:#x}): got {got:#x}, want {want:#x}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// A float rounded to the nearest half (ties to even), by search among
    /// every half's value: (its bits, its value).
    fn nearest_half(f: f32) -> (u16, f32) {
        let decode = |h: u16| {
            let (s, e, m) = (if h & 0x8000 != 0 { -1.0 } else { 1.0 }, (h >> 10) & 0x1f, (h & 0x3ff) as f64);
            s * if e == 0 { m * 2f64.powi(-24) } else { (1024.0 + m) * 2f64.powi(e as i32 - 25) }
        };
        let sign = if f.is_sign_negative() { 0x8000 } else { 0 };
        let v = (f as f64).abs();
        if f.is_nan() {
            return (0x7e00, f32::NAN);
        }
        if v >= 65520.0 {
            return (sign | 0x7c00, if sign != 0 { f32::NEG_INFINITY } else { f32::INFINITY });
        }
        let k = (0..=0x7bffu16).position(|h| decode(h) >= v).unwrap_or(0x7bff) as u16;
        let pick = if decode(k) == v || k == 0 {
            k
        } else {
            let (dl, dh) = (v - decode(k - 1), decode(k) - v);
            if dl < dh || (dl == dh && (k - 1) & 1 == 0) { k - 1 } else { k }
        };
        let h = sign | pick;
        (h, decode(h) as f32)
    }

    #[test]
    fn halves_in_kernels() {
        let Some(g) = gpu() else { return };
        let src = r#"
#pragma OPENCL EXTENSION cl_khr_fp16 : enable
__kernel void k(__global const float *a, __global const half *hs, __global float *o, __global half *ho) {
    int i = get_global_id(0);
    half x = (half)a[2 * i], y = (half)a[2 * i + 1];
    __global float *r = o + i * 8;
    r[0] = x + y;
    r[1] = x * y;
    r[2] = x / y;
    r[3] = sqrt(fabs(x));
    r[4] = fma(x, y, (half)1.0f);
    r[5] = vload_half(i, hs);
    r[6] = (float)(int)x;
    r[7] = sin(x);
    vstore_half_rtz(a[2 * i], i, ho);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64usize;
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let a: Vec<f32> = (0..2 * n).map(|_| (next() as i64 >> 40) as f32 / 1000.0).collect();
        let hs: Vec<u16> = (0..n).map(|_| next() as u16 & 0x7bff).collect();
        let (ba, bh, bo, bho) = (g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 2).unwrap(), g.buffer(n as u64 * 32).unwrap(), g.buffer(n as u64 * 2).unwrap());
        g.write(&ba, 0, &a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        g.write(&bh, 0, &hs.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: choose_local([n as u32, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 16], &[(0, &ba), (1, &bh), (2, &bo), (3, &bho)]).unwrap();
        let o: Vec<f32> = words(&g.read(&bo, 0, n as u64 * 32).unwrap()).into_iter().map(f32::from_bits).collect();
        let ho: Vec<u16> = g.read(&bho, 0, n as u64 * 2).unwrap().chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let h = |f: f32| nearest_half(f).1;
        let mut bad = Vec::new();
        for i in 0..n {
            let (x, y) = (h(a[2 * i]), h(a[2 * i + 1]));
            let want = [h(x + y), h(x * y), h(x / y), h(x.abs().sqrt()), h(x.mul_add(y, 1.0)), nearest_half_value(hs[i]), x as i32 as f32, h(x.sin())];
            for (k, &w) in want.iter().enumerate() {
                let got = o[i * 8 + k];
                // (sin from libclc: within 2 half ulps.)
                let ok = got.to_bits() == w.to_bits() || (got.is_nan() && w.is_nan()) || (k == 7 && (got - w).abs() <= 2.0 * 2f32.powi(-10) * w.abs().max(2f32.powi(-14)));
                if !ok {
                    bad.push(format!("o[{i}][{k}] for {x}, {y}: got {got:e}, want {w:e}"));
                }
            }
            // Toward zero: the nearest, or the one below it in magnitude.
            let (near, nv) = nearest_half(a[2 * i]);
            let rtz = if nv.abs() > a[2 * i].abs() { near - 1 } else { near };
            if ho[i] != rtz {
                bad.push(format!("vstore_half_rtz({}): got {:#x}, want {rtz:#x}", a[2 * i], ho[i]));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn pointers_chosen_between_buffers() {
        let Some(g) = gpu() else { return };
        // (pyopencl's if_positive: clang selects the pointer, then loads.)
        let src = r#"
__kernel void k(__global int *r, __global const int *c, __global int *a, __global int *b) {
    int i = get_global_id(0);
    r[i] = c[i] > 0 ? a[i] : b[i];
    *(c[i] > 0 ? &a[i] : &b[i]) = -i;
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64u32;
        let bytes = |v: &[i32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let c: Vec<i32> = (0..n as i32).map(|i| (i % 3) - 1).collect();
        let a: Vec<i32> = (0..n as i32).map(|i| 1000 + i).collect();
        let b: Vec<i32> = (0..n as i32).map(|i| 2000 + i).collect();
        let bufs: Vec<_> = (0..4).map(|_| g.buffer(n as u64 * 4).unwrap()).collect();
        g.write(&bufs[1], 0, &bytes(&c)).unwrap();
        g.write(&bufs[2], 0, &bytes(&a)).unwrap();
        g.write(&bufs[3], 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: [n, 1, 1], local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 16], &[(0, &bufs[0]), (1, &bufs[1]), (2, &bufs[2]), (3, &bufs[3])]).unwrap();
        let get = |k: usize| words(&g.read(&bufs[k], 0, n as u64 * 4).unwrap()).into_iter().map(|w| w as i32).collect::<Vec<i32>>();
        let (r, a2, b2) = (get(0), get(2), get(3));
        for i in 0..n as usize {
            let pos = c[i] > 0;
            assert_eq!(r[i], if pos { a[i] } else { b[i] }, "r[{i}]");
            assert_eq!((a2[i], b2[i]), if pos { (-(i as i32), b[i]) } else { (a[i], -(i as i32)) }, "stores {i}");
        }
    }

    #[test]
    fn double_vectors_from_bits() {
        let Some(g) = gpu() else { return };
        // (libclc's sincos ends this way once LLVM vectorizes it: doubles
        // built from words as <2 x i64>, subtracted, scaled by a splat.)
        let ir = r#"
define spir_kernel void @k(ptr addrspace(1) %o, ptr addrspace(1) %a) {
  %i = call i32 @_Z13get_global_idj(i32 0)
  %b = mul i32 %i, 6
  %p0 = getelementptr i32, ptr addrspace(1) %a, i32 %b
  %w0 = load i32, ptr addrspace(1) %p0, align 4
  %b1 = add i32 %b, 1
  %p1 = getelementptr i32, ptr addrspace(1) %a, i32 %b1
  %w1 = load i32, ptr addrspace(1) %p1, align 4
  %b2 = add i32 %b, 2
  %p2 = getelementptr i32, ptr addrspace(1) %a, i32 %b2
  %w2 = load i32, ptr addrspace(1) %p2, align 4
  %b3 = add i32 %b, 3
  %p3 = getelementptr i32, ptr addrspace(1) %a, i32 %b3
  %w3 = load i32, ptr addrspace(1) %p3, align 4
  %b4 = add i32 %b, 4
  %p4 = getelementptr i32, ptr addrspace(1) %a, i32 %b4
  %w4 = load i32, ptr addrspace(1) %p4, align 4
  %b5 = add i32 %b, 5
  %p5 = getelementptr i32, ptr addrspace(1) %a, i32 %b5
  %w5 = load i32, ptr addrspace(1) %p5, align 4
  %h0 = zext i32 %w0 to i64
  %h1 = zext i32 %w1 to i64
  %l0 = zext i32 %w2 to i64
  %l1 = zext i32 %w3 to i64
  %e0 = zext i32 %w4 to i64
  %e1 = zext i32 %w5 to i64
  %v0 = insertelement <2 x i64> poison, i64 %e0, i64 0
  %v1 = insertelement <2 x i64> %v0, i64 %e1, i64 1
  %v2 = shl nuw nsw <2 x i64> %v1, splat (i64 32)
  %v3 = bitcast <2 x i64> %v2 to <2 x double>
  %u0 = insertelement <2 x i64> poison, i64 %h0, i64 0
  %u1 = insertelement <2 x i64> %u0, i64 %h1, i64 1
  %u2 = shl nuw nsw <2 x i64> %u1, splat (i64 32)
  %t0 = insertelement <2 x i64> poison, i64 %l0, i64 0
  %t1 = insertelement <2 x i64> %t0, i64 %l1, i64 1
  %u3 = or disjoint <2 x i64> %u2, %t1
  %u4 = bitcast <2 x i64> %u3 to <2 x double>
  %d = fsub <2 x double> %u4, %v3
  %x = fmul double 2.0, 1.5
  %s0 = insertelement <2 x double> poison, double %x, i64 0
  %s1 = shufflevector <2 x double> %s0, <2 x double> poison, <2 x i32> zeroinitializer
  %m = fmul <2 x double> %s1, %d
  %m0 = extractelement <2 x double> %m, i64 0
  %m1 = extractelement <2 x double> %m, i64 1
  %b6 = mul i32 %i, 2
  %q0 = getelementptr double, ptr addrspace(1) %o, i32 %b6
  store double %m0, ptr addrspace(1) %q0, align 8
  %b7 = add i32 %b6, 1
  %q1 = getelementptr double, ptr addrspace(1) %o, i32 %b7
  store double %m1, ptr addrspace(1) %q1, align 8
  ret void
}
declare i32 @_Z13get_global_idj(i32)
"#;
        let m = crate::ir::parse(ir).unwrap();
        let n = 16usize;
        let mut seed = 0x51f1u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // Per item: high words, low words, the subtrahends' high words.
        let a: Vec<u32> = (0..n * 6).map(|k| if k % 6 < 2 || k % 6 >= 4 { 0x3fe0_0000 | (next() as u32 & 0xf_ffff) } else { next() as u32 }).collect();
        let (bo, ba) = (g.buffer(n as u64 * 16).unwrap(), g.buffer(n as u64 * 24).unwrap());
        g.write(&ba, 0, &a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: [n as u32, 1, 1], local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 8], &[(0, &bo), (1, &ba)]).unwrap();
        let o: Vec<f64> = g.read(&bo, 0, n as u64 * 16).unwrap().chunks(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect();
        for i in 0..n {
            let w = &a[i * 6..i * 6 + 6];
            for lane in 0..2 {
                let u = f64::from_bits((w[lane] as u64) << 32 | w[2 + lane] as u64);
                let v = f64::from_bits((w[4 + lane] as u64) << 32);
                assert_eq!(o[i * 2 + lane], 3.0 * (u - v), "item {i} lane {lane}");
            }
        }
    }

    #[test]
    fn odd_width_integers() {
        let Some(g) = gpu() else { return };
        // (LLVM narrows values to such widths; OpenCL C has none to write.)
        let ir = r#"
define spir_kernel void @k(ptr addrspace(1) %o, ptr addrspace(1) %a) {
  %i = call i32 @_Z13get_global_idj(i32 0)
  %p = getelementptr i32, ptr addrspace(1) %a, i32 %i
  %v = load i32, ptr addrspace(1) %p, align 4
  %t = trunc i32 %v to i2
  %s = add i2 %t, 1
  %u = sext i2 %s to i32
  %w = trunc i32 %v to i11
  %x = mul i11 %w, 3
  %y = zext i11 %x to i32
  %z = add i32 %u, %y
  %q = getelementptr i32, ptr addrspace(1) %o, i32 %i
  store i32 %z, ptr addrspace(1) %q, align 4
  ret void
}
declare i32 @_Z13get_global_idj(i32)
"#;
        let m = crate::ir::parse(ir).unwrap();
        let n = 64u32;
        let a: Vec<u32> = (0..n).map(|i| i * 2654435761).collect();
        let (bo, ba) = (g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 4).unwrap());
        g.write(&ba, 0, &a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: [n, 1, 1], local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n, 1, 1], [0; 3], &[0u8; 8], &[(0, &bo), (1, &ba)]).unwrap();
        let out = words(&g.read(&bo, 0, n as u64 * 4).unwrap());
        for (k, &v) in a.iter().enumerate() {
            // i2: (v + 1) mod 4, sign-extended; i11: v * 3 mod 2^11.
            let two = ((v + 1) & 3) as i32;
            let two = if two >= 2 { two - 4 } else { two };
            let want = (two + ((v.wrapping_mul(3)) & 0x7ff) as i32) as u32;
            assert_eq!(out[k], want, "for {v:#x}");
        }
    }

    #[test]
    fn halves_stored() {
        let Some(g) = gpu() else { return };
        let src = "__kernel void k(__global const float *a, __global half *ho, __global half *hz) { int i = get_global_id(0); vstore_half(a[i], i, ho); vstore_half_rtz(a[i], i, hz); }";
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64usize;
        let a: Vec<f32> = (0..n).map(|i| i as f32 * 1.37 - 20.0).collect();
        let (ba, bo, bz) = (g.buffer(n as u64 * 4).unwrap(), g.buffer(n as u64 * 2).unwrap(), g.buffer(n as u64 * 2).unwrap());
        g.write(&ba, 0, &a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: choose_local([n as u32, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 12], &[(0, &ba), (1, &bo), (2, &bz)]).unwrap();
        let read = |b| g.read(b, 0, n as u64 * 2).unwrap().chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<u16>>();
        let (ho, hz) = (read(&bo), read(&bz));
        for i in 0..n {
            let (near, nv) = nearest_half(a[i]);
            let rtz = if nv.abs() > a[i].abs() { near - 1 } else { near };
            assert_eq!((ho[i], hz[i]), (near, rtz), "vstore_half of {}", a[i]);
        }
    }

    /// A half's value.
    fn nearest_half_value(h: u16) -> f32 {
        let (s, e, m) = (if h & 0x8000 != 0 { -1.0 } else { 1.0 }, (h >> 10) & 0x1f, (h & 0x3ff) as f64);
        (s * if e == 0 { m * 2f64.powi(-24) } else { (1024.0 + m) * 2f64.powi(e as i32 - 25) }) as f32
    }

    #[test]
    fn doubles_in_kernels() {
        let Some(g) = gpu() else { return };
        let src = r#"
#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global const double *a, __global const double *b, __global double *o, __global long *l) {
    int i = get_global_id(0);
    double x = a[i], y = b[i];
    __global double *r = o + i * 16;
    r[0] = x + y;
    r[1] = x * y - x / y;
    r[2] = fma(x, y, -1.0);
    r[3] = sqrt(fabs(x));
    r[4] = exp(y / 1e3);
    r[5] = sin(x);
    r[6] = pow(fabs(x), 0.5);
    r[7] = floor(x) + ceil(y) * 2.0;
    double2 v = (double2)(x, y) * (double2)(3.0, -0.25) + (double2)(1.0);
    r[8] = v.x;
    r[9] = v.y;
    r[10] = (double)(float)x;
    r[11] = (double)(i * 7 - 300);
    r[12] = fmax(x, y);
    r[13] = dot((double2)(x, y), (double2)(y, x));
    r[14] = x < y ? 1.0 : 2.0;
    r[15] = (double)convert_int_rtn(y);
    __global long *s = l + i * 4;
    s[0] = (long)x;
    s[1] = convert_long_sat(y * 1e10);
    s[2] = as_long(x);
    s[3] = isnan(x) + 2 * isless(x, y);
}
"#;
        let m = build(&test_clang(), src, "", None).unwrap();
        let n = 64usize;
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut a: Vec<f64> = (0..n).map(|_| (next() as i64 >> 20) as f64 / 1e9).collect();
        let mut b: Vec<f64> = (0..n).map(|_| (next() as i64 >> 22) as f64 / 7e8).collect();
        a[0] = f64::NAN;
        b[1] = 0.0;
        a[2] = -0.0;
        let bytes = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let (ba, bb, bo, bl) = (g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 8).unwrap(), g.buffer(n as u64 * 128).unwrap(), g.buffer(n as u64 * 32).unwrap());
        g.write(&ba, 0, &bytes(&a)).unwrap();
        g.write(&bb, 0, &bytes(&b)).unwrap();
        let info = wgsl::kernel_info(&m, "k").unwrap();
        let launch = Launch { local: choose_local([n as u32, 1, 1], 1, 64), local_bytes: vec![] };
        g.run(1, &m, &info, &launch, 1, [n as u32, 1, 1], [0; 3], &[0u8; 16], &[(0, &ba), (1, &bb), (2, &bo), (3, &bl)]).unwrap();
        let o: Vec<f64> = g.read(&bo, 0, n as u64 * 128).unwrap().chunks(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect();
        let l: Vec<i64> = g.read(&bl, 0, n as u64 * 32).unwrap().chunks(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
        // Within ulps (libclc's transcendental functions are within 4).
        let close = |got: f64, want: f64, ulps: u64| {
            got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()) || (got - want).abs() <= ulps as f64 * f64::EPSILON * want.abs().max(f64::MIN_POSITIVE)
        };
        let mut bad = Vec::new();
        for i in 0..n {
            let (x, y) = (a[i], b[i]);
            let want = [
                (x + y, 0),
                // (Contracted: OpenCL allows it, clang does it.)
                (x.mul_add(y, -(x / y)), 0),
                (x.mul_add(y, -1.0), 0),
                (x.abs().sqrt(), 0),
                ((y / 1e3).exp(), 4),
                (x.sin(), 4),
                (x.abs().powf(0.5), 16),
                (x.floor() + y.ceil() * 2.0, 0),
                (x * 3.0 + 1.0, 0),
                (y * -0.25 + 1.0, 0),
                (x as f32 as f64, 0),
                ((i as i32 * 7 - 300) as f64, 0),
                (if x.is_nan() { y } else { x.max(y) }, 0),
                (x * y + y * x, 0),
                (if x < y { 1.0 } else { 2.0 }, 0),
                (y.floor() as i32 as f64, 0),
            ];
            for (k, &(w, u)) in want.iter().enumerate() {
                if !close(o[i * 16 + k], w, u) {
                    bad.push(format!("o[{i}][{k}] for {x:e}, {y:e}: got {:e}, want {w:e}", o[i * 16 + k]));
                }
            }
            let lwant = [x as i64, (y * 1e10) as i64, x.to_bits() as i64, x.is_nan() as i64 + 2 * (x < y) as i64];
            if l[i * 4..i * 4 + 4] != lwant {
                bad.push(format!("l[{i}] for {x:e}, {y:e}: got {:?}, want {lwant:?}", &l[i * 4..i * 4 + 4]));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn copies_within_a_buffer() {
        let Some(g) = gpu() else { return };
        // (Sub-buffers of one buffer copied to each other.)
        let b = g.buffer(64).unwrap();
        let data: Vec<u8> = (0..64).collect();
        g.write(&b, 0, &data).unwrap();
        g.copy(&b, 0, &b, 32, 16).unwrap();
        g.copy(&b, 49, &b, 1, 7).unwrap();
        let out = g.read(&b, 0, 64).unwrap();
        let mut want = data.clone();
        want.copy_within(0..16, 32);
        want.copy_within(49..56, 1);
        assert_eq!(out, want);
    }

    #[test]
    fn a_work_group_size_divides_the_global_size() {
        assert_eq!(choose_local([1000, 1, 1], 1, 64), [50, 1, 1]);
        assert_eq!(choose_local([64, 64, 1], 2, 256), [64, 4, 1]);
        assert_eq!(choose_local([7, 1, 1], 1, 64), [7, 1, 1]);
    }
}

/// An error and its causes, which wgpu's errors keep the detail in.
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        s.push_str(&format!(": {c}"));
        cause = c.source();
    }
    s
}
