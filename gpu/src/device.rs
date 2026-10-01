//! The GPU itself, through wgpu: the host's best adapter (Vulkan, Metal or
//! DirectX 12), or a software one (lavapipe) when `WASM_ENV_GPU=software`
//! or there's no other. Buffers are wgpu storage buffers; a kernel launch
//! is one or more compute dispatches of the kernel's WGSL (wgsl.rs),
//! compiled once per work-group size and local-memory sizes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wgpu::util::DeviceExt;

use crate::ir::{Module, Res};
use crate::wgsl::{self, ArgKind, HEADER_WORDS, KernelInfo, Launch};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
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
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wasm-env"),
            required_features: wgpu::Features::empty(),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("can't open {}: {e}", info.name))?;
        // Errors are reported to the caller (error scopes), not panics.
        device.on_uncaptured_error(Arc::new(|e| eprintln!("wasm-env: GPU: {}", chain(&e))));
        Ok(Gpu { device, queue, info, limits, pipelines: Mutex::new(HashMap::new()), lost: Default::default() })
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
            enc.copy_buffer_to_buffer(src, src_offset, dst, dst_offset, len);
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
        let code = wgsl::compile(m, &info.name, launch)?;
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
