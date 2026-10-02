//! Runs one of the reproducer shaders with its inputs and prints every
//! output (value and bits), for comparing drivers or settings.
//!
//!   cargo run --release -- bug1 ../bug1.wgsl
//!   cargo run --release -- bug2 ../bug2-lets.wgsl
//!
//! Binding 0 is a read-only block of u32s (sixteen header words: global
//! offset x/y/z, global size x/y/z, dimensions, workgroup offset x/y/z,
//! six unused; then a word per kernel argument, zero here); bindings 1.. are the
//! kernel's buffers. Doubles are stored as IEEE binary64 bits.
use wgpu::util::DeviceExt;

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

static mut BUG1_INPUTS: Option<(Vec<f64>, Vec<f64>)> = None;

fn bytes(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (case, path) = (args[1].as_str(), &args[2]);
    // Inputs, output sizes (bytes), global size, workgroups to dispatch.
    let (inputs, outputs, global, groups): (Vec<Vec<u8>>, Vec<usize>, u32, u32) = match case {
        "bug1" => {
            let n = 64;
            let mut seed = 0x9e3779b97f4a7c15u64;
            let mut a: Vec<f64> = (0..n).map(|_| (xorshift(&mut seed) as i64 >> 20) as f64 / 1e9).collect();
            let mut b: Vec<f64> = (0..n).map(|_| (xorshift(&mut seed) as i64 >> 22) as f64 / 7e8).collect();
            a[0] = f64::NAN;
            b[1] = 0.0;
            a[2] = -0.0;
            // Kernel arguments: a, b, o (16 doubles per item), l (4 longs per item).
            unsafe { BUG1_INPUTS = Some((a.clone(), b.clone())) };
            (vec![bytes(&a), bytes(&b)], vec![n * 128, n * 32], n as u32, 1)
        }
        "bug2" => {
            let z: Vec<f64> = (0..32).map(|k| ((k * 7919 % 101) as f64 - 50.0) / 17.0).collect();
            // Kernel arguments: z (16 complex numbers), o (16 complex), b (-1.0).
            // (Groups of 4 or 16, as the shader was made for.)
            let groups = if std::fs::read_to_string(path).unwrap().contains("workgroup_size(16") { 1 } else { 4 };
            (vec![bytes(&z)], vec![32 * 8], 16, groups)
        }
        _ => panic!("bug1 or bug2"),
    };
    let nargs = if case == "bug1" { 4 } else { 3 };
    // (The header is sixteen words, six of them unused.)
    let mut header: Vec<u32> = vec![0, 0, 0, global, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    header.extend(std::iter::repeat_n(0, nargs));

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("no adapter");
    eprintln!("adapter: {:?}", adapter.get_info().name);
    // (As wasm-env does: the adapter's own limits.)
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_limits: adapter.limits(), ..Default::default() })).unwrap();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(std::fs::read_to_string(path).unwrap().into()) });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let header_bytes: Vec<u8> = header.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut buffers = vec![device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &header_bytes, usage })];
    for i in &inputs {
        buffers.push(device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: i, usage }));
    }
    let first_output = buffers.len();
    for &size in &outputs {
        buffers.push(device.create_buffer(&wgpu::BufferDescriptor { label: None, size: size as u64, usage, mapped_at_creation: false }));
    }
    if case == "bug2" {
        buffers.push(device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &(-1.0f64).to_le_bytes(), usage }));
    }
    let entries: Vec<_> = buffers.iter().enumerate().map(|(k, b)| wgpu::BindGroupEntry { binding: k as u32, resource: b.as_entire_binding() }).collect();
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipeline.get_bind_group_layout(0), entries: &entries });
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(groups, 1, 1);
    }
    let staging: Vec<_> = outputs.iter().map(|&s| device.create_buffer(&wgpu::BufferDescriptor { label: None, size: s as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })).collect();
    for (k, s) in staging.iter().enumerate() {
        enc.copy_buffer_to_buffer(&buffers[first_output + k], 0, s, 0, outputs[k] as u64);
    }
    queue.submit([enc.finish()]);
    for s in &staging {
        s.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    }
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).unwrap();
    let mut got: Vec<Vec<u64>> = Vec::new();
    for (k, s) in staging.iter().enumerate() {
        let data = s.slice(..).get_mapped_range().unwrap();
        got.push(data.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect());
        for (j, c) in data.chunks(8).enumerate() {
            let bits = u64::from_le_bytes(c.try_into().unwrap());
            println!("out{k}[{j}] = {:e} ({bits:#018x})", f64::from_bits(bits));
        }
    }
    if case == "bug1" {
        #[allow(static_mut_refs)]
        let (a, b) = unsafe { BUG1_INPUTS.clone().unwrap() };
        let mut wrong = 0;
        for i in 0..64 {
            let (o, l) = bug1_expected(i, a[i], b[i]);
            for (k, &(w, ulps)) in o.iter().enumerate() {
                let g = f64::from_bits(got[0][i * 16 + k]);
                let ok = g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()) || (g - w).abs() <= ulps as f64 * f64::EPSILON * w.abs().max(f64::MIN_POSITIVE);
                if !ok {
                    wrong += 1;
                    eprintln!("wrong: o[{i}][{k}] for {}, {}: got {g:e}, want {w:e}", a[i], b[i]);
                }
            }
            for (k, w) in l.iter().enumerate() {
                if got[1][i * 4 + k] as i64 != *w {
                    wrong += 1;
                    eprintln!("wrong: l[{i}][{k}] for {}, {}: got {}, want {w}", a[i], b[i], got[1][i * 4 + k] as i64);
                }
            }
        }
        eprintln!("{wrong} of {} outputs wrong", 64 * 20);
    }
    if case == "bug2" {
        // Expected: 1 / z, element by element (the kernel computes z^-1).
        let z: Vec<f64> = (0..32).map(|k| ((k * 7919 % 101) as f64 - 50.0) / 17.0).collect();
        for i in 0..16 {
            let (re, im) = (z[2 * i], z[2 * i + 1]);
            let d = re * re + im * im;
            println!("expected[{}] = {:e}  expected[{}] = {:e}", 2 * i, re / d, 2 * i + 1, -im / d);
        }
    }
}

/// bug1's expected outputs for item i (o: 16 doubles; l: 4 longs), and
/// the ulps each may be off (libclc's transcendental functions: a few).
#[allow(dead_code)]
fn bug1_expected(i: usize, x: f64, y: f64) -> ([(f64, u64); 16], [i64; 4]) {
    (
        [
            (x + y, 0),
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
        ],
        [x as i64, (y * 1e10) as i64, x.to_bits() as i64, x.is_nan() as i64 + 2 * (x < y) as i64],
    )
}
