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
//!
//! INFO's description (256 bytes): name, vendor and driver (64 bytes
//! each), device type (u32: OpenCL's), maximum work-group size, maximum
//! work-item sizes (3), compute units (u32s), local memory, largest
//! buffer, global memory (u64s), buffer arguments at most, vendor id (u32s).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use wasm_env_gpu::device::{Gpu, choose_local};
use wasm_env_gpu::ir::Module;
use wasm_env_gpu::wgpu;
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

/// The host's GPU, opened when the environment first asks for it.
static GPU: OnceLock<Result<Arc<Gpu>, String>> = OnceLock::new();

pub fn gpu() -> Result<Arc<Gpu>, String> {
    GPU.get_or_init(|| Gpu::open().map(Arc::new)).clone()
}

/// Programs' identities, for the service's pipeline cache.
static PROGRAMS: AtomicUsize = AtomicUsize::new(1);

pub struct Session {
    gpu: Arc<Gpu>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: u64,
    buffers: HashMap<u64, wgpu::Buffer>,
    programs: HashMap<u64, Program>,
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
        write!(f, "Session({})", self.gpu.info.name)
    }
}

impl Session {
    pub fn open() -> SysResult<Session> {
        let gpu = gpu().map_err(|e| {
            if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                eprintln!("wasm-env: no GPU: {e}");
            }
            ENODEV
        })?;
        Ok(Session { gpu, state: Mutex::new(State { next: 1, ..Default::default() }) })
    }

    /// One request: the block at `arg` in the caller's memory.
    /// `guest`: where the calling program is (its paths, for clang).
    pub fn call(&self, data: &mut [u8], arg: u64, guest: &wasm_env_gpu::opencl::Guest) -> SysResult<i64> {
        let block: [u8; 128] = mem::slice(data, arg, 128)?.try_into().unwrap();
        let w = |k: usize| u64::from_le_bytes(block[k * 8..k * 8 + 8].try_into().unwrap());
        let op = w(0);
        let a: [u64; 9] = std::array::from_fn(|k| w(k + 1));
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
        let g = &self.gpu;
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
                if st.buffers.remove(&a[0]).is_none() && st.programs.remove(&a[0]).is_none() {
                    return Err(Failure::Cl(CL_INVALID_MEM_OBJECT));
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
                let (module, log) = build(&source, &options, guest);
                let failed = module.is_none();
                let mut st = self.state.lock().unwrap();
                let h = st.next;
                st.next += 1;
                st.programs.insert(h, Program { id: PROGRAMS.fetch_add(1, Ordering::Relaxed), module: module.map(Arc::new), log, object: None, library: false });
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
            12 => self.run(data, a)?,
            13 => g.finish().map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?,
            _ => return Err(Failure::Errno(EINVAL)),
        }
        Ok(())
    }

    fn run(&self, data: &mut [u8], a: &[u64; 9]) -> Result<(), Failure> {
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
        let limits = &self.gpu.limits;
        let max = limits.max_compute_invocations_per_workgroup;
        if local[..dims as usize].contains(&0) {
            local = info.required_local.unwrap_or_else(|| choose_local(global, dims, max.min(256)));
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
        let placeholder = self.gpu.buffer(4).map_err(|_| Failure::Cl(CL_OUT_OF_RESOURCES))?;
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
        let launch = Launch { local, local_bytes };
        self.gpu.run(id, &m, &info, &launch, dims, global, offset, &block, &bound).map_err(|e| {
            if std::env::var_os("WASM_ENV_DEBUG_GPU").is_some() {
                eprintln!("wasm-env: GPU: {e}");
            }
            Failure::Cl(CL_OUT_OF_RESOURCES)
        })
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

    fn description(&self) -> [u8; 256] {
        let g = &self.gpu;
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
        let max_alloc = l.max_storage_buffer_binding_size.min(l.max_buffer_size);
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
fn build(source: &str, options: &str, guest: &wasm_env_gpu::opencl::Guest) -> (Option<Module>, String) {
    let cc = match toolchain() {
        Ok(cc) => cc,
        Err(log) => return (None, log),
    };
    let timing = std::env::var_os("WASM_ENV_DEBUG_GPU_TIME").is_some();
    let start = std::time::Instant::now();
    let m = match wasm_env_gpu::opencl::build(&cc, source, options, Some(guest)) {
        Ok(m) => m,
        Err(log) => return (None, log),
    };
    let built = start.elapsed();
    let log = check_kernels(&m);
    if timing {
        eprintln!("wasm-env: GPU: built a program of {} bytes: clang and parsing {built:?}, checking its kernels {:?}", source.len(), start.elapsed() - built);
    }
    if log.is_empty() { (Some(m), log) } else { (None, log) }
}

/// The clang that compiles OpenCL C: the translation toolchain's.
fn toolchain() -> Result<std::path::PathBuf, String> {
    wasm_env_translate::link::Toolchain::from_env().map(|t| t.cc).ok_or_else(|| "wasm-env: no toolchain (clang) to compile OpenCL C with".into())
}

/// What a module's kernels use that the device can't do: the build log.
fn check_kernels(m: &Module) -> String {
    let mut log = String::new();
    for f in m.functions.iter().filter(|f| f.kernel) {
        // Checked with a trivial launch: what the kernel's code needs is the
        // same for every launch.
        let locals = f.params.iter().enumerate().filter(|(_, t)| **t == wasm_env_gpu::ir::Type::Ptr(3)).map(|(k, _)| (k, 4)).collect();
        if let Err(e) = wgsl::compile(m, &f.name, &Launch { local: [1, 1, 1], local_bytes: locals }) {
            log.push_str(&format!("kernel {}: not supported here: {e}\n", f.name));
        }
    }
    log
}
