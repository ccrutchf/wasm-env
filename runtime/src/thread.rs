//! Threads (docs/abi.md, "Threads"): what a process's threads share, futex,
//! and starting a thread.
//!
//! A translated program's memory is a shared WebAssembly memory. Each guest
//! thread runs its own instance of the program's module on that memory, in
//! its own wasmtime store, on its own host thread; the instance's
//! thread-local block holds that thread's guest registers. Everything else a
//! process has (descriptors, address space, futex queues, exit) is shared
//! through a `Group`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use wasmtime::{Module, SharedMemory, Store, Val};

use crate::abi::errno::*;
use crate::kernel::Termination;
use crate::memory::{self as mem, AddressSpace, SysResult};
use crate::process::{ExecImage, Inherit, Pending, Process};

/// The address space a process manages inside its memory (docs/abi.md,
/// "Memory").
pub struct Mm {
    pub space: AddressSpace,
    /// The program break, and where it started.
    pub brk: u64,
    pub brk_start: u64,
    /// Mappings placed in the break's reserve, above the break, because the
    /// program asked for that address (LuaJIT wants its code near its VM, to
    /// reach it with direct branches). The break can't grow into them.
    /// Start → end.
    pub low: Ranges,
    /// Memory the program made executable (an anonymous mapping, or
    /// `mprotect` with PROT_EXEC): where a JIT puts the code it generates,
    /// translated when it's first jumped to (see translate::ensure_code).
    pub code: Ranges,
    /// Shared mappings: host mappings of files (see shared.rs).
    pub shared: crate::shared::SharedMaps,
}

/// Disjoint address ranges, start → end.
pub type Ranges = std::collections::BTreeMap<u64, u64>;

/// The range of `ranges` holding `addr`, if any.
fn range_at(ranges: &Ranges, addr: u64) -> Option<(u64, u64)> {
    ranges.range(..=addr).next_back().filter(|&(_, &end)| addr < end).map(|(&s, &e)| (s, e))
}

/// Whether any of `ranges` overlaps [start, end).
fn overlaps(ranges: &Ranges, start: u64, end: u64) -> bool {
    ranges.range(..end).next_back().is_some_and(|(_, &e)| e > start)
}

/// Adds [start, end) to `ranges` (merging with those it overlaps), or with
/// `on` false removes it (keeping the parts of ranges outside it).
fn set_range(ranges: &mut Ranges, start: u64, end: u64, on: bool) {
    let overlapping: Vec<(u64, u64)> = ranges.range(..end).filter(|&(_, &e)| e > start).map(|(&s, &e)| (s, e)).collect();
    let (mut lo, mut hi) = (start, end);
    for (s, e) in overlapping {
        ranges.remove(&s);
        if on {
            (lo, hi) = (lo.min(s), hi.max(e));
        } else {
            if s < start {
                ranges.insert(s, start);
            }
            if e > end {
                ranges.insert(end, e);
            }
        }
    }
    if on {
        ranges.insert(lo, hi);
    }
}

impl Mm {
    /// The executable range holding `addr`, if any.
    pub fn code_at(&self, addr: u64) -> Option<(u64, u64)> {
        range_at(&self.code, addr)
    }

    /// Records [start, end) as executable (`exec`) or not.
    pub fn set_code(&mut self, start: u64, end: u64, exec: bool) {
        set_range(&mut self.code, start, end, exec);
    }

    /// Places a mapping of `len` bytes at `hint`, as Linux does when that
    /// range is free: in the mmap region, or in the break's reserve above
    /// the break. Returns whether it did (else the caller maps it elsewhere,
    /// as Linux does too).
    pub fn map_at_hint(&mut self, hint: u64, len: u64) -> bool {
        if hint == 0 || !hint.is_multiple_of(crate::abi::PAGE_SIZE) {
            return false;
        }
        let Some(end) = hint.checked_add(len) else { return false };
        // In the mmap region only below its top: a hint elsewhere (LuaJIT's
        // random ones may be anywhere) must not move the top.
        if end <= self.space.top && self.space.is_free(hint, len) {
            self.space.alloc_at(hint, len);
            return true;
        }
        let reserve_end = self.brk_start + mem::BRK_MAX;
        if hint >= mem::page_align(self.brk) && end <= reserve_end && !overlaps(&self.low, hint, end) {
            set_range(&mut self.low, hint, end, true);
            return true;
        }
        false
    }

    /// Unmaps [start, end) where it's in the break's reserve.
    pub fn unmap_low(&mut self, start: u64, end: u64) {
        set_range(&mut self.low, start, end, false);
    }

    /// Whether the break can move to `want` (not over a mapping).
    pub fn brk_fits(&self, want: u64) -> bool {
        !overlaps(&self.low, self.brk.min(want), self.brk.max(want))
    }
}

/// How a process with several threads ends, when a thread other than the
/// one running its main loop ends it.
pub enum GroupEnd {
    Terminate(Termination),
    Exec(ExecImage, Inherit),
}

struct GroupState {
    /// Threads running (including the first).
    live: usize,
    /// Set once the process is ending: every thread stops at its next
    /// syscall, and blocking calls return EINTR.
    end: Option<GroupEnd>,
    ended: bool,
}

/// What a module needs to start more threads: set by the first thread once
/// its memory is shared.
pub struct Threading {
    pub module: Module,
    pub memory: SharedMemory,
}

/// State shared by all threads of a process.
pub struct Group {
    pub mm: Mutex<Mm>,
    pub futex: Futexes,
    pub threading: OnceLock<Threading>,
    /// Libraries loaded as modules of their own (see modules.rs).
    pub libraries: crate::modules::Libraries,
    state: Mutex<GroupState>,
    changed: Condvar,
}

impl Group {
    pub fn new() -> Arc<Group> {
        Arc::new(Group {
            mm: Mutex::new(Mm { space: AddressSpace::new(0), brk: 0, brk_start: 0, low: Default::default(), code: Default::default(), shared: Default::default() }),
            futex: Futexes::default(),
            threading: OnceLock::new(),
            libraries: Default::default(),
            state: Mutex::new(GroupState { live: 1, end: None, ended: false }),
            changed: Condvar::new(),
        })
    }

    /// A forked child's group: a copy of this one's address space and
    /// libraries, running the same module on `memory`, with one thread.
    pub fn fork(&self, memory: SharedMemory) -> Arc<Group> {
        let mm = self.mm.lock().unwrap();
        let child = Arc::new(Group {
            mm: Mutex::new(Mm { space: mm.space.clone(), brk: mm.brk, brk_start: mm.brk_start, low: mm.low.clone(), code: mm.code.clone(), shared: mm.shared.clone() }),
            futex: Futexes::default(),
            threading: OnceLock::new(),
            libraries: self.libraries.fork(),
            state: Mutex::new(GroupState { live: 1, end: None, ended: false }),
            changed: Condvar::new(),
        });
        let module = self.threading.get().expect("a translated program").module.clone();
        let _ = child.threading.set(Threading { module, memory });
        child
    }

    /// Whether the process is ending (another thread exited it or exec'd).
    pub fn ended(&self) -> bool {
        self.state.lock().unwrap().ended
    }

    /// Ends the process (the first call wins): other threads stop at their
    /// next syscall.
    pub fn end(&self, how: GroupEnd) {
        let mut s = self.state.lock().unwrap();
        if !s.ended {
            s.ended = true;
            s.end = Some(how);
            self.changed.notify_all();
        }
    }

    /// For the thread that runs the process's main loop: how another thread
    /// ended the process, if one did.
    pub fn take_end(&self) -> Option<GroupEnd> {
        self.state.lock().unwrap().end.take()
    }

    pub fn thread_started(&self) {
        self.state.lock().unwrap().live += 1;
    }

    /// A thread finished. When the last one exits with exit(), the process
    /// ends with its status, as on Linux.
    pub fn thread_exited(&self, code: Option<i32>) {
        let mut s = self.state.lock().unwrap();
        s.live -= 1;
        if s.live == 0 && !s.ended {
            s.ended = true;
            s.end = Some(GroupEnd::Terminate(Termination::Exit(code.unwrap_or(0))));
        }
        self.changed.notify_all();
    }

    pub fn threads(&self) -> usize {
        self.state.lock().unwrap().live
    }

    /// Waits until the process ends (for a main thread that called exit()
    /// while others run).
    pub fn wait_end(&self) -> Option<GroupEnd> {
        let mut s = self.state.lock().unwrap();
        while !s.ended {
            s = self.changed.wait(s).unwrap();
        }
        s.end.take()
    }
}

/// Futex wait queues, by address.
#[derive(Default)]
pub struct Futexes {
    queues: Mutex<HashMap<u64, Vec<Arc<Waiter>>>>,
}

struct Waiter {
    bitset: u32,
    woken: Mutex<bool>,
    cv: Condvar,
}

/// How often a futex wait checks whether it was interrupted.
const POLL: Duration = Duration::from_millis(50);

/// The futex word at `addr`, read atomically.
fn word(data: &[u8], addr: u64) -> SysResult<&AtomicU32> {
    if !addr.is_multiple_of(4) {
        return Err(EINVAL);
    }
    let bytes = mem::slice(data, addr, 4)?;
    // SAFETY: in bounds and aligned (checked above); other threads access
    // the word only atomically or through their own shared-memory view.
    Ok(unsafe { AtomicU32::from_ptr(bytes.as_ptr() as *mut u32) })
}

impl Futexes {
    /// FUTEX_WAIT: sleeps until woken, if the word at `addr` still holds
    /// `val` (else EAGAIN), until `deadline` (ETIMEDOUT) or until
    /// `interrupted` (EINTR).
    pub fn wait(
        &self,
        data: &[u8],
        addr: u64,
        val: u32,
        bitset: u32,
        deadline: Option<Instant>,
        interrupted: &dyn Fn() -> bool,
    ) -> SysResult<i64> {
        if bitset == 0 {
            return Err(EINVAL);
        }
        let waiter = {
            let mut queues = self.queues.lock().unwrap();
            // Checked under the queue lock: a waker changes the word, then
            // takes this lock to wake, so it can't slip in between.
            if word(data, addr)?.load(Ordering::SeqCst) != val {
                return Err(EAGAIN);
            }
            let w = Arc::new(Waiter { bitset, woken: Mutex::new(false), cv: Condvar::new() });
            queues.entry(addr).or_default().push(w.clone());
            w
        };
        let mut woken = waiter.woken.lock().unwrap();
        loop {
            if *woken {
                return Ok(0);
            }
            let now = Instant::now();
            let error = match deadline {
                Some(d) if now >= d => Some(ETIMEDOUT),
                _ if interrupted() => Some(EINTR),
                _ => None,
            };
            if let Some(e) = error {
                drop(woken);
                // Leave the queue, unless a waker took us off it meanwhile.
                let mut queues = self.queues.lock().unwrap();
                if *waiter.woken.lock().unwrap() {
                    return Ok(0);
                }
                remove(&mut queues, addr, &waiter);
                return Err(e);
            }
            let slice = deadline.map_or(POLL, |d| (d - now).min(POLL));
            woken = waiter.cv.wait_timeout(woken, slice).unwrap().0;
        }
    }

    /// FUTEX_WAKE: wakes up to `n` waiters on `addr` whose bitset matches.
    pub fn wake(&self, addr: u64, n: u32, bitset: u32) -> i64 {
        let mut queues = self.queues.lock().unwrap();
        let Some(q) = queues.get_mut(&addr) else { return 0 };
        let mut woken = 0;
        q.retain(|w| {
            if woken < n && w.bitset & bitset != 0 {
                woken += 1;
                *w.woken.lock().unwrap() = true;
                w.cv.notify_one();
                false
            } else {
                true
            }
        });
        if q.is_empty() {
            queues.remove(&addr);
        }
        woken as i64
    }

    /// FUTEX_REQUEUE and FUTEX_CMP_REQUEUE: wakes up to `n` waiters on
    /// `addr` and moves up to `requeue` more to `to`; with `expect`, only if
    /// the word at `addr` holds it (else EAGAIN).
    pub fn requeue(&self, data: &[u8], addr: u64, n: u32, requeue: u32, to: u64, expect: Option<u32>) -> SysResult<i64> {
        let mut queues = self.queues.lock().unwrap();
        if let Some(v) = expect
            && word(data, addr)?.load(Ordering::SeqCst) != v
        {
            return Err(EAGAIN);
        }
        let waiters = queues.remove(&addr).unwrap_or_default();
        let (mut woken, mut moved) = (0, 0);
        let mut kept = Vec::new();
        for w in waiters {
            if woken < n {
                woken += 1;
                *w.woken.lock().unwrap() = true;
                w.cv.notify_one();
            } else if moved < requeue && to != addr {
                moved += 1;
                queues.entry(to).or_default().push(w);
            } else {
                kept.push(w);
            }
        }
        if !kept.is_empty() {
            queues.insert(addr, kept);
        }
        Ok((woken + moved) as i64)
    }
}

fn remove(queues: &mut HashMap<u64, Vec<Arc<Waiter>>>, addr: u64, waiter: &Arc<Waiter>) {
    // A requeue may have moved the waiter to another address.
    let at = if queues.get(&addr).is_some_and(|q| q.iter().any(|w| Arc::ptr_eq(w, waiter))) {
        Some(addr)
    } else {
        queues.iter().find(|(_, q)| q.iter().any(|w| Arc::ptr_eq(w, waiter))).map(|(&a, _)| a)
    };
    if let Some(a) = at {
        let q = queues.get_mut(&a).unwrap();
        q.retain(|w| !Arc::ptr_eq(w, waiter));
        if q.is_empty() {
            queues.remove(&a);
        }
    }
}

/// The C stack each thread's instance of the support code gets.
const THREAD_C_STACK: u64 = 64 << 10;

/// Starts guest thread `process` (its tid, descriptors and so on already
/// set) on a new host thread: a new instance of the program's module on its
/// memory, with its own thread-local block, entered at
/// `__wasm_env_thread_start(frame)`. Returns once the thread has copied
/// `frame` (the support code's clone frame, in the parent's thread-local
/// block), or EAGAIN if it couldn't start.
pub fn spawn(mut process: Process, frame: u64) -> SysResult<()> {
    let group = process.group.clone();
    if group.threading.get().is_none() {
        return Err(ENOSYS); // not a translated program
    }
    let (started, copied) = std::sync::mpsc::channel();
    process.started = Some(started);
    group.thread_started();
    let name = format!("pid {} tid {}", process.pid, process.tid);
    let spawned = std::thread::Builder::new()
        .name(name)
        // wasmtime runs guest code on the host stack.
        .stack_size(8 << 20)
        .spawn(move || run(process, frame));
    if spawned.is_err() {
        group.thread_exited(None);
        return Err(EAGAIN);
    }
    // The thread's end of the channel goes away if it fails to start.
    copied.recv().map_err(|_| EAGAIN)
}

/// Runs a started thread until it exits, then cleans up after it.
fn run(process: Process, frame: u64) {
    let kernel = process.kernel.clone();
    let group = process.group.clone();
    let threading = group.threading.get().unwrap();
    let mut store = Store::new(&kernel.engine, process);
    crate::process::set_deadline(&mut store);
    let mut block = None;
    let result = (|| -> anyhow::Result<()> {
        let instance = crate::process::instantiate(&mut store, &threading.module, Some(&threading.memory))?;
        store.data_mut().main = crate::modules::Main::new(&mut store, instance);
        let global = |store: &mut Store<Process>, name: &str| match instance.get_global(&mut *store, name).map(|g| g.get(&mut *store)) {
            Some(Val::I64(v)) => Ok(v as u64),
            Some(Val::I32(v)) => Ok(v as u32 as u64),
            _ => anyhow::bail!("the module doesn't export {name}"),
        };
        // This thread's thread-local block, with its C stack above it.
        let (size, align) = (global(&mut store, "__tls_size")?, global(&mut store, "__tls_align")?.max(16));
        let len = mem::page_align(size + align + THREAD_C_STACK);
        let base = crate::syscall::alloc_shared(&group, &threading.memory, len).map_err(|e| anyhow::anyhow!("errno {e}"))?;
        block = Some((base, len));
        let tls = base.next_multiple_of(align);
        let init_tls = instance.get_typed_func::<i64, ()>(&mut store, "__wasm_init_tls")?;
        init_tls.call(&mut store, tls as i64)?;
        let sp = instance.get_global(&mut store, "__stack_pointer").unwrap();
        sp.set(&mut store, Val::I64((base + len) as i64))?;
        // The libraries the process has loaded so far.
        crate::modules::sync(&mut store)?;
        let start = instance.get_typed_func::<i64, ()>(&mut store, "__wasm_env_thread_start")?;
        Ok(start.call(&mut store, frame as i64)?)
    })();

    let p = store.data_mut();
    let code = match (p.pending.take(), result) {
        (Some(Pending::ThreadExit(code)), _) => Some(code),
        (Some(Pending::Terminate(how)), _) => {
            group.end(GroupEnd::Terminate(how));
            None
        }
        (Some(Pending::Exec(image)), _) => {
            let inherit = p.inherit();
            group.end(GroupEnd::Exec(image, inherit));
            None
        }
        (Some(Pending::GroupEnded), _) | (None, Ok(())) => None,
        (None, Err(e)) => {
            // A fault in any thread ends the process, like the signal would.
            let sig = crate::process::trap_signal(&e);
            eprintln!("wasm-env: pid {} (thread {}): {e}", p.pid, p.tid);
            group.end(GroupEnd::Terminate(Termination::Signal(sig)));
            None
        }
    };
    exit_thread(p);
    if let Some((base, len)) = block {
        group.mm.lock().unwrap().space.release(base, len);
    }
    group.thread_exited(code);
}

/// A thread is gone: CLONE_CHILD_CLEARTID (or set_tid_address) asks for its
/// tid word to be zeroed and a waiter woken, which is how pthread_join
/// learns it exited.
pub fn exit_thread(p: &mut Process) {
    if p.clear_tid == 0 {
        return;
    }
    let Some(t) = p.group.threading.get() else { return };
    let data = crate::syscall::shared_bytes(&t.memory);
    if let Ok(w) = word(data, p.clear_tid) {
        w.store(0, Ordering::SeqCst);
        p.group.futex.wake(p.clear_tid, 1, u32::MAX);
    }
    p.clear_tid = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Vec<u8> {
        vec![0u8; 64]
    }

    #[test]
    fn wait_returns_eagain_if_the_word_changed() {
        let f = Futexes::default();
        let data = memory();
        assert_eq!(f.wait(&data, 8, 1, u32::MAX, None, &|| false), Err(EAGAIN));
        assert_eq!(f.wait(&data, 6, 0, u32::MAX, None, &|| false), Err(EINVAL), "misaligned");
    }

    #[test]
    fn wait_times_out_and_is_interrupted() {
        let f = Futexes::default();
        let data = memory();
        let soon = Some(Instant::now() + Duration::from_millis(20));
        assert_eq!(f.wait(&data, 8, 0, u32::MAX, soon, &|| false), Err(ETIMEDOUT));
        assert_eq!(f.wait(&data, 8, 0, u32::MAX, None, &|| true), Err(EINTR));
        assert_eq!(f.wake(8, 1, u32::MAX), 0, "a waiter that gave up is no longer queued");
    }

    #[test]
    fn wake_wakes_a_waiter() {
        let f = Arc::new(Futexes::default());
        let data = Arc::new(memory());
        let (f2, d2) = (f.clone(), data.clone());
        let t = std::thread::spawn(move || f2.wait(&d2, 16, 0, u32::MAX, None, &|| false));
        // Wake until the waiter has queued and been woken.
        let mut woken = 0;
        while woken == 0 {
            std::thread::sleep(Duration::from_millis(5));
            woken = f.wake(16, 1, u32::MAX);
        }
        assert_eq!(t.join().unwrap(), Ok(0));
        assert_eq!(woken, 1);
    }

    #[test]
    fn wake_respects_bitsets_and_counts() {
        let f = Arc::new(Futexes::default());
        let data = Arc::new(memory());
        let spawn = |bits: u32| {
            let (f, d) = (f.clone(), data.clone());
            std::thread::spawn(move || f.wait(&d, 0, 0, bits, None, &|| false))
        };
        let (a, b, c) = (spawn(1), spawn(2), spawn(2));
        while f.queues.lock().unwrap().get(&0).map_or(0, Vec::len) < 3 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(f.wake(0, 5, 2), 2, "only waiters whose bitset matches");
        assert_eq!((b.join().unwrap(), c.join().unwrap()), (Ok(0), Ok(0)));
        assert_eq!(f.wake(0, 5, u32::MAX), 1);
        assert_eq!(a.join().unwrap(), Ok(0));
    }

    #[test]
    fn requeue_moves_waiters() {
        let f = Arc::new(Futexes::default());
        let data = Arc::new(memory());
        let spawn = || {
            let (f, d) = (f.clone(), data.clone());
            std::thread::spawn(move || f.wait(&d, 0, 0, u32::MAX, None, &|| false))
        };
        let (a, b) = (spawn(), spawn());
        while f.queues.lock().unwrap().get(&0).map_or(0, Vec::len) < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(f.requeue(&data, 0, 0, 1, 8, Some(1)), Err(EAGAIN), "CMP_REQUEUE checks the word");
        assert_eq!(f.requeue(&data, 0, 1, 1, 8, Some(0)), Ok(2), "one woken, one moved");
        assert_eq!(f.wake(8, 1, u32::MAX), 1, "the moved one waits on the new address");
        assert_eq!((a.join().unwrap(), b.join().unwrap()), (Ok(0), Ok(0)));
    }

    #[test]
    fn the_last_thread_to_exit_ends_the_group() {
        let g = Group::new();
        g.thread_started();
        g.thread_exited(Some(3));
        assert!(!g.ended());
        g.thread_exited(Some(4));
        assert!(matches!(g.wait_end(), Some(GroupEnd::Terminate(Termination::Exit(4)))));
    }

    #[test]
    fn the_first_end_wins() {
        let g = Group::new();
        g.end(GroupEnd::Terminate(Termination::Exit(1)));
        g.end(GroupEnd::Terminate(Termination::Exit(2)));
        assert!(g.ended());
        assert!(matches!(g.take_end(), Some(GroupEnd::Terminate(Termination::Exit(1)))));
    }
}
