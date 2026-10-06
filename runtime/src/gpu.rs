//! `/dev/wasm-env-gpu`: the GPU service (gpu/, `wasm-env-gpu`) as a device
//! the environment's GPU libraries talk to, on every host (docs/abi.md,
//! "GPU"). Each open of the device is a session owning the buffers and
//! programs made through it (freed when it closes); each request is an
//! ioctl with a 128-byte block:
//!
//!   op (u64), a[9] (u64: arguments), r[6] (u64: results)
//!
//! The ioctl returns 0 or -errno (a bad request); r[0] is the result and
//! r[1] an OpenCL status (0, or CL_BUILD_PROGRAM_FAILURE and the like).
//! Guest addresses in arguments are read and written in the caller's
//! memory. The operations (`op`):
//!
//!   1 INFO     a0: address, a1: size   the device's description (below)
//!   2 CREATE   a0: size                r0: buffer handle
//!   3 FREE     a0: handle (a buffer or a program)
//!   4 WRITE    a0: buffer, a1: offset, a2: address, a3: length
//!   5 READ     a0: buffer, a1: offset, a2: address, a3: length
//!   6 COPY     a0: source, a1: offset, a2: destination, a3: offset, a4: length
//!   7 FILL     a0: buffer, a1: offset, a2: pattern address, a3: pattern size, a4: length
//!   8 BUILD    a0: source, a1: length, a2: options, a3: length   r0: program handle
//!   9 LOG      a0: program, a1: address, a2: capacity   r0: the build log's length
//!  10 KERNELS  a0: program, a1: address, a2: capacity   r0: length of the names (;-separated)
//!  11 ARGS     a0: program, a1: name, a2: length, a3: address, a4: capacity
//!              r0: number of arguments, r2: argument block size, r3..r5:
//!              reqd_work_group_size (0 if none); the arguments as 128-byte
//!              records: kind (u32: 0 buffer, 1 constant buffer, 2 local,
//!              3 value), size (u32), offset in the block (u32), qualifiers
//!              (u32: the type's, CL_KERNEL_ARG_TYPE_*, and above them the
//!              access, CL_KERNEL_ARG_ACCESS_* - 0x1100 or 0),
//!              type name (56 bytes), name (56 bytes)
//!  12 RUN      a0: program, a1: name, a2: length, a3: dimensions,
//!              a4: sizes (u32 × 9: global, offset, local; local 0 to let
//!              the device choose), a5: argument block, a6: its length,
//!              a7: per argument, a buffer handle (u64; 0 if none),
//!              a8: per argument, local memory bytes (u32)
//!              r0: bytes of printf output the launch left (see PRINTF)
//!  13 FINISH
//!  14 ATTRIBUTES a0: program, a1: name, a2: length, a3: address, a4: capacity
//!              r0: the length of the kernel's attributes ("reqd_work_group_size(…)…"),
//!              r2: bytes of its own __local variables
//!  15 COMPILE  a0: source, a1: length, a2: options, a3: length, a4: headers
//!              (u64 × 4 each: source, length, name, length), a5: their number
//!              r0: program handle (an object: clCompileProgram)
//!  16 LINK     a0: program handles (u64), a1: their number, a2: options,
//!              a3: length   r0: program handle (executable, or a library
//!              with -create-library)
//!  17 BINARY   a0: program, a1: address, a2: capacity   r0: the length of an
//!              object's, library's or linked executable's bitcode, r2: which
//!              (0, 1, 2)
//!  18 LOAD     a0: bitcode, a1: length, a2: its kind (as BINARY's r2)
//!              r0: program handle
//!  19 PRINTF   a0: address, a1: capacity   r0: the length of the launches'
//!              printf output not yet taken (taken if it fits): each
//!              printf's format (u32 length, bytes, padded to 4), its
//!              arguments' number (u32), then each argument: its kind (u32:
//!              0 integer, 1 float, 2 string, 3 pointer), bytes and lanes
//!              (u32s), then a u64 for each lane (a string: u32 length,
//!              bytes, padded to 4)
//!
//! Vulkan's (packages/vulkan; gpu/src/vulkan.rs), whose failures put a
//! VkResult in r[1]:
//!
//!  20 VK_SHADER   a0: SPIR-V, a1: its length, a3: address, a4: capacity
//!              r0: shader handle; or, failing, why (text at a3) and its length
//!  21 VK_PIPELINE a0: shader, a1: entry point's name, a2: its length,
//!              a3: address, a4: capacity, a5: specialization constants (each
//!              id, size, low and high words: u32s), a6: their number
//!              r0: compute pipeline handle (or
//!              as VK_SHADER's)
//!  22 VK_SUBMIT   a0: commands, a1: their length (gpu/src/vulkan.rs, `decode`)
//!  23 VK_LIMITS   a0: address, a1: size   u64s: storage and uniform
//!              buffer offset alignments, their largest bindings, work
//!              groups per dimension, work-group size (3) and invocations,
//!              shared memory, descriptor sets, storage and uniform buffers
//!              per stage, largest allocation, memory, push constant bytes,
//!              vendor and device ids, device type (Vulkan's)
//!  24 VK_IMAGE    a0: u32s: dimensions (1, 2, 3), format (VkFormat), width,
//!              height, depth, mip levels, layers, samples, usage
//!              (VkImageUsageFlags)   r0: image handle
//!  25 VK_VIEW     a0: image, a1: u32s: view type (VkImageViewType), format (0:
//!              the image's), aspect, mip level, levels, layer, layers   r0: handle
//!  26 VK_SAMPLER  a0: u32s: filters (magnification, minification, mipmap),
//!              address modes (u, v, w), maximum anisotropy, comparison
//!              (VkCompareOp + 1, or 0), LOD minimum and maximum (f32)   r0: handle
//!  27 VK_FORMAT   a0: VkFormat   r0: its VkFormatFeatureFlags (optimal
//!              tiling), r2: a texel's bytes (0: not supported)
//!  28 VK_GRAPHICS a0: state, a1: its length (gpu/src/vulkan.rs,
//!              `GraphicsState::decode`), a3, a4: as VK_SHADER's   r0:
//!              graphics pipeline handle
//!
//! Vulkan's device memory is a buffer (CREATE), read and written as one.
//!
//! INFO's description (256 bytes): name, vendor and driver (64 bytes
//! each), device type (u32: OpenCL's), maximum work-group size, maximum
//! work-item sizes (3), compute units (u32s), local memory, largest
//! buffer, global memory (u64s), buffer arguments at most, vendor id (u32s).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use wasm_env_gpu::device::{Api, Gpu, choose_local};
use wasm_env_gpu::ir::Module;
use wasm_env_gpu::wgpu;
use wasm_env_gpu::vulkan;
use wasm_env_gpu::wgsl::{self, ArgKind, Launch};

use crate::abi::errno::*;
use crate::memory::{self as mem, SysResult};

/// The ioctl request: _IOWR('W', 0x40, 128 bytes).
pub const GPU_CALL: i64 = 0xc080_5740;

const CL_SUCCESS: i64 = 0;
const CL_MEM_OBJECT_ALLOCATION_FAILURE: i64 = -4;
const CL_OUT_OF_RESOURCES: i64 = -5;
const CL_BUILD_PROGRAM_FAILURE: i64 = -11;
const CL_COMPILE_PROGRAM_FAILURE: i64 = -15;
const CL_LINK_PROGRAM_FAILURE: i64 = -17;
const CL_INVALID_PROGRAM: i64 = -44;
const CL_INVALID_BINARY: i64 = -42;
const CL_INVALID_VALUE: i64 = -30;
const CL_INVALID_MEM_OBJECT: i64 = -38;
const CL_INVALID_PROGRAM_EXECUTABLE: i64 = -45;
const CL_INVALID_KERNEL_NAME: i64 = -46;
const CL_INVALID_KERNEL_ARGS: i64 = -52;
const CL_INVALID_WORK_GROUP_SIZE: i64 = -54;
// (Vulkan's, for its operations.)
const VK_ERROR_INITIALIZATION_FAILED: i64 = -3;
const VK_ERROR_DEVICE_LOST: i64 = -4;
const VK_ERROR_FORMAT_NOT_SUPPORTED: i64 = -11;

/// The host's GPU, opened when the environment first asks for it, for the
/// API that asks (device.rs, `open_for`: lavapipe takes OpenCL's and
/// Vulkan's shaders best at different widths, fixed once a process).
static GPU: OnceLock<Result<Arc<Gpu>, String>> = OnceLock::new();

fn gpu(api: Api) -> Result<Arc<Gpu>, String> {
    GPU.get_or_init(|| Gpu::open_for(api).map(Arc::new)).clone()
}

/// Programs' identities, for the service's pipeline cache.
static PROGRAMS: AtomicUsize = AtomicUsize::new(1);

pub struct Session {
    /// The GPU, opened at the session's first request (whose operation says
    /// which API it's for).
    gpu: OnceLock<Arc<Gpu>>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: u64,
    buffers: HashMap<u64, wgpu::Buffer>,
    programs: HashMap<u64, Program>,
    /// Launches' printf output, for PRINTF.
    printf: Vec<u8>,
    /// Vulkan's shaders and pipelines.
    vk: HashMap<u64, Vk>,
}

enum Vk {
    Shader(Arc<vulkan::Shader>),
    Pipeline(Arc<vulkan::Pipeline>),
    Image(wgpu::Texture),
    View(vulkan::View),
    Sampler(wgpu::Sampler),
}

struct Program {
    id: usize,
    module: Option<Arc<Module>>,
    log: String,
    /// A compiled object or a library (bitcode), not yet linked.
    object: Option<Vec<u8>>,
    library: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Session({})", self.gpu.get().map_or("not open yet", |g| g.info.name.as_str()))
    }
}

impl Session {
    pub fn open() -> SysResult<Session> {
        Ok(Session { gpu: OnceLock::new(), state: Mutex::new(State { next: 1, ..Default::default() }) })
    }

    fn gpu(&self) -> &Arc<Gpu> {
        self.gpu.get().expect("opened by the session's first request")
    }

    /// One request: the block at `arg` in the caller's memory.
    /// `guest`: where the calling program is (its paths, for clang).
    pub fn call(&self, data: &mut [u8], arg: u64, guest: &wasm_env_gpu::opencl::Guest) -> SysResult<i64> {
        let block: [u8; 128] = mem::slice(data, arg, 128)?.try_into().unwrap();
        let w = |k: usize| u64::from_le_bytes(block[k * 8..k * 8 + 8].try_into().unwrap());
        let op = w(0);
        let a: [u64; 9] = std::array::from_fn(|k| w(k + 1));
        // (ENODEV where there's no GPU at all.)
        if self.gpu.get().is_none() {
            let api = if op >= 20 { Api::Vulkan } else { Api::OpenCl };
            let g = gpu(api).map_err(|e| {
                if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                    eprintln!("wasm-env: no GPU: {e}");
                }
                ENODEV
            })?;
            let _ = self.gpu.set(g);
        }
        let mut r = [0u64; 6];
        let status = match self.op(data, op, &a, &mut r, guest) {
            Ok(()) => CL_SUCCESS,
            Err(Failure::Errno(e)) => return Err(e),
            Err(Failure::Cl(s)) => s,
        };
        r[1] = status as u64;
        let out = mem::slice_mut(data, arg + 80, 48)?;
        for (k, v) in r.iter().enumerate() {
            out[k * 8..k * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        Ok(0)
    }

    fn op(&self, data: &mut [u8], op: u64, a: &[u64; 9], r: &mut [u64; 6], guest: &wasm_env_gpu::opencl::Guest) -> Result<(), Failure> {
        let g = self.gpu();
        match op {
            1 => {
                let d = self.description();
                let n = (a[1] as usize).min(d.len());
                mem::slice_mut(data, a[0], n as u64)?.copy_from_slice(&d[..n]);
            }
            2 => {
                let b = g.buffer(a[0]).map_err(|_| Failure::Cl(CL_MEM_OBJECT_ALLOCATION_FAILURE))?;
                let mut st = self.state.lock().unwrap();
                let h = st.next;
                st.next += 1;
                st.buffers.insert(h, b);
                r[0] = h;
            }
            3 => {
                let mut st = self.state.lock().unwrap();
                if st.buffers.remove(&a[0]).is_none() && st.vk.remove(&a[0]).is_none() {
                    match st.programs.remove(&a[0]) {
                        Some(p) => self.gpu().forget(p.id),
                        None => return Err(Failure::Cl(CL_INVALID_MEM_OBJECT)),
                    }
                }
            }
            4 => {
                let b = self.buffer(a[0])?;
                self.check(&b, a[1], a[3])?;
                let bytes = mem::slice(data, a[2], a[3])?.to_vec();
                g.write(&b, a[1], &bytes).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
            }
            5 => {
                let b = self.buffer(a[0])?;
                self.check(&b, a[1], a[3])?;
                let bytes = g.read(&b, a[1], a[3]).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
                mem::slice_mut(data, a[2], a[3])?.copy_from_slice(&bytes);
            }
            6 => {
                let (s, d) = (self.buffer(a[0])?, self.buffer(a[2])?);
                self.check(&s, a[1], a[4])?;
                self.check(&d, a[3], a[4])?;
                g.copy(&s, a[1], &d, a[3], a[4]).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
            }
            7 => {
                let b = self.buffer(a[0])?;
                self.check(&b, a[1], a[4])?;
                let pattern = mem::slice(data, a[2], a[3])?.to_vec();
                if pattern.is_empty() || a[4] % pattern.len() as u64 != 0 {
                    return Err(Failure::Cl(CL_INVALID_VALUE));
                }
                let bytes: Vec<u8> = pattern.iter().copied().cycle().take(a[4] as usize).collect();
                g.write(&b, a[1], &bytes).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
            }
            8 => {
                let source = String::from_utf8_lossy(mem::slice(data, a[0], a[1])?).into_owned();
                let options = String::from_utf8_lossy(mem::slice(data, a[2], a[3])?).into_owned();
                let (built, log) = build(&source, &options, guest);
                let failed = built.is_none();
                // (Its IR is its binary: an executable's, built again by LOAD.)
                let (module, object) = built.map_or((None, None), |(m, ir)| (Some(Arc::new(m)), Some(ir.into_bytes())));
                let mut st = self.state.lock().unwrap();
                let h = st.next;
                st.next += 1;
                st.programs.insert(h, Program { id: PROGRAMS.fetch_add(1, Ordering::Relaxed), module, log, object, library: false });
                r[0] = h;
                if failed {
                    return Err(Failure::Cl(CL_BUILD_PROGRAM_FAILURE));
                }
            }
            9 | 10 => {
                let st = self.state.lock().unwrap();
                let p = st.programs.get(&a[0]).ok_or(Failure::Cl(CL_INVALID_VALUE))?;
                let text = if op == 9 {
                    p.log.clone()
                } else {
                    let m = p.module.as_ref().ok_or(Failure::Cl(CL_INVALID_PROGRAM_EXECUTABLE))?;
                    m.functions.iter().filter(|f| f.kernel).map(|f| f.name.as_str()).collect::<Vec<_>>().join(";")
                };
                r[0] = text.len() as u64;
                let n = text.len().min(a[2] as usize);
                mem::slice_mut(data, a[1], n as u64)?.copy_from_slice(&text.as_bytes()[..n]);
            }
            11 => {
                let (_, m) = self.program(a[0])?;
                let name = String::from_utf8_lossy(mem::slice(data, a[1], a[2])?).into_owned();
                let info = wgsl::kernel_info(&m, &name).map_err(|_| Failure::Cl(CL_INVALID_KERNEL_NAME))?;
                r[0] = info.args.len() as u64;
                r[2] = info.block_size as u64 - wgsl::HEADER_WORDS as u64 * 4;
                if let Some(l) = info.required_local {
                    r[3..6].copy_from_slice(&l.map(u64::from));
                }
                let n = info.args.len().min(a[4] as usize / 128);
                let out = mem::slice_mut(data, a[3], n as u64 * 128)?;
                for (k, arg) in info.args.iter().take(n).enumerate() {
                    let rec = &mut out[k * 128..k * 128 + 128];
                    rec.fill(0);
                    let (kind, size) = match arg.kind {
                        ArgKind::Buffer { constant: false } => (0u32, 4u32),
                        ArgKind::Buffer { constant: true } => (1, 4),
                        ArgKind::Local => (2, 0),
                        ArgKind::Value { size } => (3, size),
                    };
                    rec[0..4].copy_from_slice(&kind.to_le_bytes());
                    rec[4..8].copy_from_slice(&size.to_le_bytes());
                    rec[8..12].copy_from_slice(&(arg.offset - wgsl::HEADER_WORDS * 4).to_le_bytes());
                    rec[12..16].copy_from_slice(&(arg.type_qualifiers | arg.access << 16).to_le_bytes());
                    let t = arg.type_name.as_bytes();
                    rec[16..16 + t.len().min(55)].copy_from_slice(&t[..t.len().min(55)]);
                    let nm = arg.name.as_bytes();
                    rec[72..72 + nm.len().min(55)].copy_from_slice(&nm[..nm.len().min(55)]);
                }
            }
            14 => {
                let (_, m) = self.program(a[0])?;
                let name = String::from_utf8_lossy(mem::slice(data, a[1], a[2])?).into_owned();
                let info = wgsl::kernel_info(&m, &name).map_err(|_| Failure::Cl(CL_INVALID_KERNEL_NAME))?;
                r[0] = info.attributes.len() as u64;
                r[2] = info.static_local as u64;
                let n = info.attributes.len().min(a[4] as usize);
                mem::slice_mut(data, a[3], n as u64)?.copy_from_slice(&info.attributes.as_bytes()[..n]);
            }
            15 => {
                let source = String::from_utf8_lossy(mem::slice(data, a[0], a[1])?).into_owned();
                let options = String::from_utf8_lossy(mem::slice(data, a[2], a[3])?).into_owned();
                let records = mem::slice(data, a[4], a[5] * 32)?.to_vec();
                let w = |k: usize| u64::from_le_bytes(records[k * 8..k * 8 + 8].try_into().unwrap());
                let mut headers = Vec::new();
                for h in 0..a[5] as usize {
                    let text = mem::slice(data, w(h * 4), w(h * 4 + 1))?.to_vec();
                    let name = String::from_utf8_lossy(mem::slice(data, w(h * 4 + 2), w(h * 4 + 3))?).into_owned();
                    headers.push((name, text));
                }
                let (object, log) = match toolchain() {
                    Ok(cc) => match wasm_env_gpu::opencl::compile(&cc, &source, &options, &headers, Some(guest)) {
                        Ok(o) => (Some(o), String::new()),
                        Err(log) => (None, log),
                    },
                    Err(log) => (None, log),
                };
                let failed = object.is_none();
                r[0] = self.add_program(Program { id: PROGRAMS.fetch_add(1, Ordering::Relaxed), module: None, log, object, library: false });
                if failed {
                    return Err(Failure::Cl(CL_COMPILE_PROGRAM_FAILURE));
                }
            }
            16 => {
                let handles = mem::slice(data, a[0], a[1] * 8)?.to_vec();
                let options = String::from_utf8_lossy(mem::slice(data, a[2], a[3])?).into_owned();
                let library = options.split_whitespace().any(|o| o == "-create-library");
                let objects: Vec<Vec<u8>> = {
                    let st = self.state.lock().unwrap();
                    handles
                        .chunks(8)
                        .map(|c| st.programs.get(&u64::from_le_bytes(c.try_into().unwrap())).and_then(|p| p.object.clone()).ok_or(Failure::Cl(CL_INVALID_PROGRAM)))
                        .collect::<Result<_, _>>()?
                };
                let linked = toolchain().and_then(|cc| wasm_env_gpu::opencl::link(&cc, &objects, library));
                let (program, failed) = match linked {
                    Ok(wasm_env_gpu::opencl::Linked::Library(o)) => (Program { id: 0, module: None, log: String::new(), object: Some(o), library: true }, false),
                    Ok(wasm_env_gpu::opencl::Linked::Executable(m, bitcode)) => {
                        let log = check_kernels(&m);
                        let ok = log.is_empty();
                        // (Its bitcode kept: its binary.)
                        (Program { id: 0, module: ok.then(|| Arc::new(m)), log, object: Some(bitcode), library: false }, !ok)
                    }
                    Err(log) => (Program { id: 0, module: None, log, object: None, library: false }, true),
                };
                r[0] = self.add_program(Program { id: PROGRAMS.fetch_add(1, Ordering::Relaxed), ..program });
                if failed {
                    return Err(Failure::Cl(CL_LINK_PROGRAM_FAILURE));
                }
            }
            17 => {
                let st = self.state.lock().unwrap();
                let p = st.programs.get(&a[0]).ok_or(Failure::Cl(CL_INVALID_PROGRAM))?;
                let o = p.object.as_deref().unwrap_or_default();
                r[0] = o.len() as u64;
                // 0: an object, 1: a library, 2: an executable (linked).
                r[2] = if p.library { 1 } else if p.module.is_some() { 2 } else { 0 };
                let n = o.len().min(a[2] as usize);
                mem::slice_mut(data, a[1], n as u64)?.copy_from_slice(&o[..n]);
            }
            18 => {
                let o = mem::slice(data, a[0], a[1])?.to_vec();
                // An executable's bitcode is built again into its module.
                let module = if a[2] == 2 {
                    let m = toolchain().and_then(|cc| wasm_env_gpu::opencl::from_bitcode(&cc, &o)).map_err(|_| Failure::Cl(CL_INVALID_BINARY))?;
                    Some(Arc::new(m))
                } else {
                    None
                };
                r[0] = self.add_program(Program { id: PROGRAMS.fetch_add(1, Ordering::Relaxed), module, log: String::new(), object: Some(o), library: a[2] == 1 });
            }
            12 => r[0] = self.run(data, a)? as u64,
            19 => {
                let mut st = self.state.lock().unwrap();
                r[0] = st.printf.len() as u64;
                if st.printf.len() as u64 <= a[1] {
                    mem::slice_mut(data, a[0], st.printf.len() as u64)?.copy_from_slice(&st.printf);
                    st.printf.clear();
                }
            }
            13 => g.finish().map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?,
            // Vulkan (packages/vulkan): shaders, pipelines, submissions.
            20 | 21 | 28 => {
                let made = if op == 20 {
                    let spirv = mem::slice(data, a[0], a[1])?.to_vec();
                    vulkan::shader(g, &spirv).map(|s| Vk::Shader(Arc::new(s)))
                } else if op == 28 {
                    let state = vulkan::GraphicsState::decode(mem::slice(data, a[0], a[1])?).map_err(|_| Failure::Errno(EINVAL))?;
                    let st = self.state.lock().unwrap();
                    let shader = |h: u64| match st.vk.get(&h) {
                        Some(Vk::Shader(s)) => Some(&**s),
                        _ => None,
                    };
                    vulkan::graphics_pipeline(g, &state, shader).map(|p| Vk::Pipeline(Arc::new(p)))
                } else {
                    let shader = match self.state.lock().unwrap().vk.get(&a[0]) {
                        Some(Vk::Shader(s)) => s.clone(),
                        _ => return Err(Failure::Cl(VK_ERROR_INITIALIZATION_FAILED)),
                    };
                    let entry = String::from_utf8_lossy(mem::slice(data, a[1], a[2])?).into_owned();
                    let spec: Vec<vulkan::Spec> = if a[6] == 0 { &[][..] } else { mem::slice(data, a[5], a[6] * 16)? }
                        .chunks(16)
                        .map(|c| std::array::from_fn(|k| u32::from_le_bytes(c[4 * k..4 * k + 4].try_into().unwrap())))
                        .collect();
                    vulkan::compute_pipeline(g, &shader, &entry, &spec).map(|p| Vk::Pipeline(Arc::new(p)))
                };
                match made {
                    Ok(v) => {
                        let mut st = self.state.lock().unwrap();
                        let h = st.next;
                        st.next += 1;
                        st.vk.insert(h, v);
                        r[0] = h;
                    }
                    Err(e) => {
                        if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                            eprintln!("wasm-env: Vulkan: {e}");
                        }
                        // (Why, as text at a3 (capacity a4), for the driver's
                        // messages.)
                        let n = (a[4] as usize).min(e.len());
                        mem::slice_mut(data, a[3], n as u64)?.copy_from_slice(&e.as_bytes()[..n]);
                        r[0] = e.len() as u64;
                        return Err(Failure::Cl(VK_ERROR_INITIALIZATION_FAILED));
                    }
                }
            }
            22 => {
                let commands = vulkan::decode(mem::slice(data, a[0], a[1])?).map_err(|_| Failure::Errno(EINVAL))?;
                let st = self.state.lock().unwrap();
                let object = |h: u64| match (st.buffers.get(&h), st.vk.get(&h)) {
                    (Some(b), _) => Some(vulkan::Object::Buffer(b)),
                    (_, Some(Vk::Pipeline(p))) => Some(vulkan::Object::Pipeline(p.clone())),
                    (_, Some(Vk::Image(t))) => Some(vulkan::Object::Texture(t)),
                    (_, Some(Vk::View(v))) => Some(vulkan::Object::View(v)),
                    (_, Some(Vk::Sampler(s))) => Some(vulkan::Object::Sampler(s)),
                    _ => None,
                };
                if let Err(e) = vulkan::submit(g, &commands, object) {
                    if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                        eprintln!("wasm-env: Vulkan submission: {e}");
                    }
                    return Err(Failure::Cl(VK_ERROR_DEVICE_LOST));
                }
            }
            23 => {
                let d = self.vulkan_limits();
                let n = (a[1] as usize).min(d.len());
                mem::slice_mut(data, a[0], n as u64)?.copy_from_slice(&d[..n]);
            }
            24..=26 => {
                let words = |at: u64, n: u64| -> Result<Vec<u32>, Failure> { Ok(mem::slice(data, at, n * 4)?.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()) };
                let made = match op {
                    24 => {
                        let w = words(a[0], 9)?;
                        vulkan::image(g, w[0], w[1], [w[2], w[3], w[4]], w[5], w[6], w[7], w[8]).map(Vk::Image)
                    }
                    25 => {
                        let w = words(a[1], 7)?;
                        match self.state.lock().unwrap().vk.get(&a[0]) {
                            Some(Vk::Image(t)) => vulkan::view(t, w[0], w[1], w[2], w[3], w[4], w[5], w[6]).map(Vk::View),
                            _ => return Err(Failure::Cl(VK_ERROR_INITIALIZATION_FAILED)),
                        }
                    }
                    _ => {
                        let w = words(a[0], 10)?;
                        Ok(Vk::Sampler(vulkan::sampler(g, [w[0], w[1], w[2]], [w[3], w[4], w[5]], w[6] as u16, w[7], [f32::from_bits(w[8]), f32::from_bits(w[9])])))
                    }
                };
                match made {
                    Ok(v) => {
                        let mut st = self.state.lock().unwrap();
                        let h = st.next;
                        st.next += 1;
                        st.vk.insert(h, v);
                        r[0] = h;
                    }
                    Err(e) => {
                        if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                            eprintln!("wasm-env: Vulkan: {e}");
                        }
                        return Err(Failure::Cl(VK_ERROR_FORMAT_NOT_SUPPORTED));
                    }
                }
            }
            27 => {
                let (features, bytes) = vulkan::format_features(g, a[0] as u32);
                (r[0], r[2]) = (features as u64, bytes as u64);
            }
            _ => return Err(Failure::Errno(EINVAL)),
        }
        Ok(())
    }

    /// A launch; the result: the printf output waiting (PRINTF).
    fn run(&self, data: &mut [u8], a: &[u64; 9]) -> Result<usize, Failure> {
        let (id, m) = self.program(a[0])?;
        let name = String::from_utf8_lossy(mem::slice(data, a[1], a[2])?).into_owned();
        let info = wgsl::kernel_info(&m, &name).map_err(|_| Failure::Cl(CL_INVALID_KERNEL_NAME))?;
        let dims = a[3] as u32;
        if !(1..=3).contains(&dims) {
            return Err(Failure::Cl(CL_INVALID_VALUE));
        }
        let sizes = mem::slice(data, a[4], 36)?;
        let s = |k: usize| u32::from_le_bytes(sizes[k * 4..k * 4 + 4].try_into().unwrap());
        let mut global = [1u32; 3];
        let mut offset = [0u32; 3];
        let mut local = [1u32; 3];
        for d in 0..dims as usize {
            global[d] = s(d);
            offset[d] = s(3 + d);
            local[d] = s(6 + d);
        }
        let limits = &self.gpu().limits;
        let max = limits.max_compute_invocations_per_workgroup;
        if local[..dims as usize].contains(&0) {
            // WASM_ENV_GPU_GROUPS_OF_ONE: work-groups of one work-item where the
            // program leaves them to the driver. A diagnostic: a kernel's results
            // don't depend on the work-group size it didn't choose, so results
            // that change with it are the driver's doing (lavapipe's, with some
            // large shaders: docs/upstream/lavapipe). Not a workaround: lavapipe
            // gets other shaders wrong with work-groups of one.
            static ONE: OnceLock<bool> = OnceLock::new();
            let one = *ONE.get_or_init(|| std::env::var_os("WASM_ENV_GPU_GROUPS_OF_ONE").is_some_and(|v| v != "0"));
            local = info.required_local.unwrap_or_else(|| if one { [1, 1, 1] } else { choose_local(global, dims, max.min(256)) });
        }
        let per_dim = [limits.max_compute_workgroup_size_x, limits.max_compute_workgroup_size_y, limits.max_compute_workgroup_size_z];
        if (0..3).any(|d| local[d] == 0 || global[d] % local[d] != 0 || local[d] > per_dim[d])
            || local.iter().product::<u32>() > max
            || info.required_local.is_some_and(|r| r != local)
        {
            return Err(Failure::Cl(CL_INVALID_WORK_GROUP_SIZE));
        }
        let block = mem::slice(data, a[5], a[6])?.to_vec();
        let n = info.args.len() as u64;
        let handles = mem::slice(data, a[7], n * 8)?.to_vec();
        let locals = mem::slice(data, a[8], n * 4)?.to_vec();
        // What a null buffer argument binds (a kernel mustn't use it).
        let placeholder = self.gpu().buffer(4).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
        let mut local_bytes = Vec::new();
        let st = self.state.lock().unwrap();
        let mut bound = Vec::new();
        let mut total_local = 0;
        for (k, arg) in info.args.iter().enumerate() {
            match arg.kind {
                ArgKind::Buffer { .. } => {
                    let h = u64::from_le_bytes(handles[k * 8..k * 8 + 8].try_into().unwrap());
                    let b = if h == 0 { &placeholder } else { st.buffers.get(&h).ok_or(Failure::Cl(CL_INVALID_KERNEL_ARGS))? };
                    bound.push((k, b));
                }
                ArgKind::Local => {
                    let bytes = u32::from_le_bytes(locals[k * 4..k * 4 + 4].try_into().unwrap());
                    total_local += bytes;
                    local_bytes.push((k, bytes));
                }
                ArgKind::Value { .. } => {}
            }
        }
        if total_local > limits.max_compute_workgroup_storage_size {
            return Err(Failure::Cl(CL_OUT_OF_RESOURCES));
        }
        let launch = Launch { local, local_bytes, ..Default::default() };
        let words = self.gpu().run(id, &m, &info, &launch, dims, global, offset, &block, &bound).map_err(|e| {
            if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                eprintln!("wasm-env: GPU: {e}");
            }
            Failure::Cl(CL_OUT_OF_RESOURCES)
        })?;
        drop(st);
        let mut st = self.state.lock().unwrap();
        printf_output(&info.printf, &words, &mut st.printf);
        Ok(st.printf.len())
    }

    fn add_program(&self, p: Program) -> u64 {
        let mut st = self.state.lock().unwrap();
        let h = st.next;
        st.next += 1;
        st.programs.insert(h, p);
        h
    }

    fn buffer(&self, h: u64) -> Result<wgpu::Buffer, Failure> {
        self.state.lock().unwrap().buffers.get(&h).cloned().ok_or(Failure::Cl(CL_INVALID_MEM_OBJECT))
    }

    fn check(&self, b: &wgpu::Buffer, offset: u64, len: u64) -> Result<(), Failure> {
        if offset.checked_add(len).is_none_or(|end| end > b.size()) {
            return Err(Failure::Cl(CL_INVALID_VALUE));
        }
        Ok(())
    }

    fn program(&self, h: u64) -> Result<(usize, Arc<Module>), Failure> {
        let st = self.state.lock().unwrap();
        let p = st.programs.get(&h).ok_or(Failure::Cl(CL_INVALID_VALUE))?;
        Ok((p.id, p.module.clone().ok_or(Failure::Cl(CL_INVALID_PROGRAM_EXECUTABLE))?))
    }

    /// VK_LIMITS's description (u64s): see the operations, above.
    fn vulkan_limits(&self) -> Vec<u8> {
        let l = &self.gpu().limits;
        let info = &self.gpu().info;
        // (wgpu's device types are in Vulkan's order: other, integrated,
        // discrete, virtual, CPU.)
        let kind = match info.device_type {
            wgpu::DeviceType::Other => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::DiscreteGpu => 2,
            wgpu::DeviceType::VirtualGpu => 3,
            wgpu::DeviceType::Cpu => 4,
        };
        let max_alloc = l.max_buffer_size.min((1 << 32) - 4096);
        let words: [u64; 19] = [
            l.min_storage_buffer_offset_alignment as u64,
            l.min_uniform_buffer_offset_alignment as u64,
            l.max_storage_buffer_binding_size,
            l.max_uniform_buffer_binding_size,
            l.max_compute_workgroups_per_dimension as u64,
            l.max_compute_workgroup_size_x as u64,
            l.max_compute_workgroup_size_y as u64,
            l.max_compute_workgroup_size_z as u64,
            l.max_compute_invocations_per_workgroup as u64,
            l.max_compute_workgroup_storage_size as u64,
            // (One bind group goes to push constants.)
            l.max_bind_groups.saturating_sub(1) as u64,
            l.max_storage_buffers_per_shader_stage as u64,
            l.max_uniform_buffers_per_shader_stage as u64,
            max_alloc,
            max_alloc.saturating_mul(4),
            vulkan::PUSH_BYTES as u64,
            info.vendor as u64,
            info.device as u64,
            kind,
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn description(&self) -> [u8; 256] {
        let g = self.gpu();
        let l = &g.limits;
        let mut d = [0u8; 256];
        let mut text = |at: usize, s: &str| {
            let b = s.as_bytes();
            let n = b.len().min(63);
            d[at..at + n].copy_from_slice(&b[..n]);
        };
        text(0, &g.info.name);
        text(64, &vendor_name(g.info.vendor));
        text(128, &format!("wasm-env ({:?}{}{})", g.info.backend, if g.info.driver.is_empty() { "" } else { ", " }, g.info.driver));
        const CL_DEVICE_TYPE_CPU: u32 = 2;
        const CL_DEVICE_TYPE_GPU: u32 = 4;
        let ty = if g.info.device_type == wgpu::DeviceType::Cpu { CL_DEVICE_TYPE_CPU } else { CL_DEVICE_TYPE_GPU };
        let words32 = [
            ty,
            l.max_compute_invocations_per_workgroup.min(256),
            l.max_compute_workgroup_size_x,
            l.max_compute_workgroup_size_y,
            l.max_compute_workgroup_size_z,
            // (wgpu doesn't say; OpenCL's programs use this to size launches.)
            16,
        ];
        for (k, v) in words32.iter().enumerate() {
            d[192 + k * 4..196 + k * 4].copy_from_slice(&v.to_le_bytes());
        }
        // (Buffers larger than a binding are bound in parts: Launch::parts.
        // A kernel's pointers are 32-bit byte offsets from 4096.)
        let max_alloc = l.max_buffer_size.min((1 << 32) - 4096);
        for (k, v) in [l.max_compute_workgroup_storage_size as u64, max_alloc, max_alloc.saturating_mul(4)].iter().enumerate() {
            d[216 + k * 8..224 + k * 8].copy_from_slice(&v.to_le_bytes());
        }
        d[240..244].copy_from_slice(&l.max_storage_buffers_per_shader_stage.saturating_sub(1).to_le_bytes());
        d[244..248].copy_from_slice(&g.info.vendor.to_le_bytes());
        d
    }
}

enum Failure {
    Errno(i64),
    Cl(i64),
}

impl From<i64> for Failure {
    fn from(e: i64) -> Self {
        Failure::Errno(e)
    }
}

fn vendor_name(id: u32) -> String {
    match id {
        0x10de => "NVIDIA Corporation".into(),
        0x1002 => "Advanced Micro Devices, Inc.".into(),
        0x8086 => "Intel(R) Corporation".into(),
        0x106b => "Apple".into(),
        0x5143 => "Qualcomm".into(),
        0x13b5 => "ARM".into(),
        0x10005 => "Mesa".into(),
        _ => "wasm-env".into(),
    }
}

/// A program's source built: its module (with every kernel checked to
/// translate), or none; and the build log.
fn build(source: &str, options: &str, guest: &wasm_env_gpu::opencl::Guest) -> (Option<(Module, String)>, String) {
    let cc = match toolchain() {
        Ok(cc) => cc,
        Err(log) => return (None, log),
    };
    let timing = std::env::var_os("WASM_ENV_DEBUG_GPU_TIME").is_some();
    let start = std::time::Instant::now();
    let (m, ir) = match wasm_env_gpu::opencl::build_with_ir(&cc, source, options, Some(guest)) {
        Ok(built) => built,
        Err(log) => return (None, log),
    };
    let built = start.elapsed();
    let log = check_kernels(&m);
    if timing {
        eprintln!("wasm-env: GPU: built a program of {} bytes: clang and parsing {built:?}, checking its kernels {:?}", source.len(), start.elapsed() - built);
    }
    if log.is_empty() { (Some((m, ir)), log) } else { (None, log) }
}

/// What compiles OpenCL C: the translation toolchain's clang, and OpenCL's
/// builtins (libclc, packages/libclc) from `$WASM_ENV_LIBCLC`, beside the
/// program (`../lib/wasm-env/clc`, as flake.nix installs them), or the
/// source tree's build; without them, math is the GPU's own.
fn toolchain() -> Result<wasm_env_gpu::opencl::Compiler, String> {
    let clang = wasm_env_translate::link::Toolchain::from_env().map(|t| t.cc).ok_or("wasm-env: no toolchain (clang) to compile OpenCL C with")?;
    let installed = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("../lib/wasm-env/clc/clspv--.bc")));
    let source_tree = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/libclc/clspv--.bc");
    let builtins = std::env::var_os("WASM_ENV_LIBCLC").map(Into::into).into_iter().chain(installed).chain([source_tree]).find(|p: &std::path::PathBuf| p.exists());
    Ok(wasm_env_gpu::opencl::Compiler { clang, builtins })
}

/// What a module's kernels use that the device can't do: the build log.
fn check_kernels(m: &Module) -> String {
    let mut log = String::new();
    for f in m.functions.iter().filter(|f| f.kernel) {
        // Checked with a trivial launch: what the kernel's code needs is the
        // same for every launch.
        let locals = f.params.iter().enumerate().filter(|(_, t)| **t == wasm_env_gpu::ir::Type::Ptr(3)).map(|(k, _)| (k, 4)).collect();
        if let Err(e) = wgsl::compile(m, &f.name, &Launch { local: [1, 1, 1], local_bytes: locals, ..Default::default() }, Default::default()) {
            log.push_str(&format!("kernel {}: not supported here: {e}\n", f.name));
        }
    }
    log
}

/// A launch's printf records (wgsl.rs, "printf") as PRINTF gives them.
fn printf_output(formats: &[wgsl::PrintfFormat], words: &[u32], out: &mut Vec<u8>) {
    let mut put = |out: &mut Vec<u8>, w: u32| out.extend_from_slice(&w.to_le_bytes());
    let bytes = |out: &mut Vec<u8>, b: &[u8], put: &mut dyn FnMut(&mut Vec<u8>, u32)| {
        put(out, b.len() as u32);
        out.extend_from_slice(b);
        out.resize(out.len().next_multiple_of(4), 0);
    };
    let mut at = 0;
    while let Some(f) = words.get(at).and_then(|&id| formats.get(id as usize)) {
        let len = 1 + f.args.iter().map(|a| a.words() as usize).sum::<usize>();
        let Some(w) = words.get(at + 1..at + len) else { break };
        at += len;
        bytes(out, f.format.as_bytes(), &mut put);
        put(out, f.args.len() as u32);
        let mut k = 0;
        for a in &f.args {
            match a {
                wgsl::PrintfArg::Int { bytes: b, lanes } | wgsl::PrintfArg::Float { bytes: b, lanes } => {
                    put(out, if matches!(a, wgsl::PrintfArg::Int { .. }) { 0 } else { 1 });
                    put(out, *b);
                    put(out, *lanes);
                    for _ in 0..*lanes {
                        let (lo, hi) = if *b == 8 { (w[k], w[k + 1]) } else { (w[k], 0) };
                        k += if *b == 8 { 2 } else { 1 };
                        put(out, lo);
                        put(out, hi);
                    }
                }
                wgsl::PrintfArg::Str(text) => {
                    put(out, 2);
                    put(out, 0);
                    put(out, 1);
                    bytes(out, text.as_bytes(), &mut put);
                }
                wgsl::PrintfArg::Ptr => {
                    put(out, 3);
                    put(out, 4);
                    put(out, 1);
                    put(out, w[k]);
                    put(out, 0);
                    k += 1;
                }
            }
        }
    }
}
