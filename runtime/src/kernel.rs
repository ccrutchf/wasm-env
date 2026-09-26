//! State shared by all processes in one environment: the process table,
//! exit statuses, pending signals and compiled programs.
//!
//! Each guest process is a host thread running its own wasmtime instance.
//! The environment as a whole is one host OS process (PLAN.md, "Security
//! model"), so pipes, wait4 and signals are all in-process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use wasmtime::{Engine, Module};

use crate::abi::errno::*;
use crate::memory::SysResult;
use crate::process::{self, ExecImage, Inherit};

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Termination {
    Exit(i32),
    Signal(i32),
}

impl Termination {
    /// The status word wait4 reports.
    pub fn wait_status(self) -> i32 {
        match self {
            Termination::Exit(code) => (code & 0xff) << 8,
            Termination::Signal(sig) => sig & 0x7f,
        }
    }

    /// The status a shell reports ($?), used as wasm-env's own exit code.
    pub fn shell_status(self) -> i32 {
        match self {
            Termination::Exit(code) => code & 0xff,
            Termination::Signal(sig) => 128 + sig,
        }
    }
}

struct Entry {
    /// 0 once the parent has exited (nobody will wait for this process).
    ppid: i32,
    pgid: i32,
    sid: i32,
    /// Set when the process has exited but hasn't been waited for.
    status: Option<Termination>,
    /// A signal sent to the process, delivered at its next syscall.
    signal: Option<i32>,
}

struct Procs {
    next_pid: i32,
    table: HashMap<i32, Entry>,
}

pub struct Kernel {
    pub engine: Engine,
    pub root: PathBuf,
    pub trace: bool,
    procs: Mutex<Procs>,
    /// Signalled whenever a process exits or receives a signal.
    changed: Condvar,
    /// Compiled programs by host path, with the (mtime, size) they were
    /// compiled from. wasmtime's disk cache makes the first compile in a
    /// run cheap too; this avoids even reading the file again.
    modules: Mutex<HashMap<PathBuf, (SystemTime, u64, Module)>>,
}

/// How often a blocked wait4 checks for signals.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

impl Kernel {
    pub fn new(engine: Engine, root: PathBuf, trace: bool) -> Arc<Kernel> {
        Arc::new(Kernel {
            engine,
            root,
            trace,
            procs: Mutex::new(Procs { next_pid: 1, table: HashMap::new() }),
            changed: Condvar::new(),
            modules: Mutex::new(HashMap::new()),
        })
    }

    /// Allocates a pid for a new process (ppid 0 for the first one).
    pub fn create(&self, ppid: i32) -> i32 {
        let mut p = self.procs.lock().unwrap();
        let pid = p.next_pid;
        p.next_pid += 1;
        let (pgid, sid) = p.table.get(&ppid).map_or((pid, pid), |e| (e.pgid, e.sid));
        p.table.insert(pid, Entry { ppid, pgid, sid, status: None, signal: None });
        pid
    }

    /// Allocates a thread id: from the same numbers as pids, as on Linux,
    /// but not a process (the thread belongs to its creator's).
    pub fn create_thread(&self) -> i32 {
        let mut p = self.procs.lock().unwrap();
        let tid = p.next_pid;
        p.next_pid += 1;
        tid
    }

    /// Records that `pid` ended, and wakes anyone waiting for it.
    pub fn exit(&self, pid: i32, how: Termination) {
        let mut p = self.procs.lock().unwrap();
        // Orphans: forget exited children, and let running ones clean up
        // after themselves when they exit.
        p.table.retain(|_, e| !(e.ppid == pid && e.status.is_some()));
        for e in p.table.values_mut().filter(|e| e.ppid == pid) {
            e.ppid = 0;
        }
        match p.table.get_mut(&pid) {
            Some(e) if e.ppid != 0 => e.status = Some(how),
            _ => {
                p.table.remove(&pid);
            }
        }
        self.changed.notify_all();
    }

    pub fn ppid(&self, pid: i32) -> i32 {
        self.procs.lock().unwrap().table.get(&pid).map_or(0, |e| e.ppid)
    }

    pub fn pgid(&self, pid: i32) -> SysResult<i32> {
        self.procs.lock().unwrap().table.get(&pid).map(|e| e.pgid).ok_or(ESRCH)
    }

    pub fn sid(&self, pid: i32) -> SysResult<i32> {
        self.procs.lock().unwrap().table.get(&pid).map(|e| e.sid).ok_or(ESRCH)
    }

    pub fn setpgid(&self, pid: i32, pgid: i32) -> SysResult<()> {
        let mut p = self.procs.lock().unwrap();
        p.table.get_mut(&pid).ok_or(ESRCH)?.pgid = pgid;
        Ok(())
    }

    pub fn setsid(&self, pid: i32) -> SysResult<i32> {
        let mut p = self.procs.lock().unwrap();
        let e = p.table.get_mut(&pid).ok_or(ESRCH)?;
        e.sid = pid;
        e.pgid = pid;
        Ok(pid)
    }

    /// wait4: waits for a child of `pid` matching `target` (Linux rules:
    /// -1 any child, >0 that pid, 0 our process group, <-1 that group).
    /// Returns None with WNOHANG if no matching child has exited yet, and
    /// EINTR if a signal arrives for `pid` while waiting.
    pub fn wait(&self, pid: i32, target: i32, nohang: bool) -> SysResult<Option<(i32, Termination)>> {
        let mut p = self.procs.lock().unwrap();
        let own_pgid = p.table.get(&pid).map_or(pid, |e| e.pgid);
        loop {
            let matches = |child: i32, e: &Entry| {
                e.ppid == pid
                    && match target {
                        -1 => true,
                        0 => e.pgid == own_pgid,
                        t if t > 0 => child == t,
                        t => e.pgid == -t,
                    }
            };
            let mut any = false;
            let mut done = None;
            for (&child, e) in &p.table {
                if matches(child, e) {
                    any = true;
                    if let Some(status) = e.status {
                        done = Some((child, status));
                        break;
                    }
                }
            }
            if let Some((child, status)) = done {
                p.table.remove(&child);
                return Ok(Some((child, status)));
            }
            if !any {
                return Err(ECHILD);
            }
            if nohang {
                return Ok(None);
            }
            // Checked here, under the lock we already hold.
            if p.table.get(&pid).is_some_and(|e| e.signal.is_some()) {
                return Err(EINTR);
            }
            p = self.changed.wait_timeout(p, SIGNAL_POLL).unwrap().0;
        }
    }

    /// kill: queues `sig` for the target process(es). Signal 0 only checks
    /// that the target exists.
    pub fn kill(&self, sender: i32, target: i32, sig: i32) -> SysResult<()> {
        let mut p = self.procs.lock().unwrap();
        let sender_pgid = p.table.get(&sender).map_or(sender, |e| e.pgid);
        let mut found = false;
        for (&pid, e) in p.table.iter_mut() {
            let hit = match target {
                t if t > 0 => pid == t,
                0 => e.pgid == sender_pgid,
                -1 => pid != sender,
                t => e.pgid == -t,
            };
            if hit && e.status.is_none() {
                found = true;
                if sig != 0 {
                    e.signal = Some(sig);
                }
            }
        }
        self.changed.notify_all();
        if found { Ok(()) } else { Err(ESRCH) }
    }

    /// The pending signal for `pid`, if any, without consuming it.
    pub fn peek_signal(&self, pid: i32) -> Option<i32> {
        self.procs.lock().unwrap().table.get(&pid).and_then(|e| e.signal)
    }

    pub fn take_signal(&self, pid: i32) -> Option<i32> {
        self.procs.lock().unwrap().table.get_mut(&pid).and_then(|e| e.signal.take())
    }

    /// Loads (or reuses) a compiled library module (see modules.rs).
    pub fn module_any(&self, host: &Path) -> Result<Module, String> {
        self.load(host, false)
    }

    /// Loads (or reuses) the compiled program at a host path.
    pub fn module(&self, host: &Path) -> Result<Module, String> {
        self.load(host, true)
    }

    fn load(&self, host: &Path, program: bool) -> Result<Module, String> {
        let meta = std::fs::metadata(host).map_err(|e| e.to_string())?;
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        if let Some((mtime, len, module)) = self.modules.lock().unwrap().get(host)
            && (*mtime, *len) == stamp {
                return Ok(module.clone());
            }
        let module = process::load_module(&self.engine, host).map_err(|e| format!("{e:?}"))?;
        if program {
            process::check_shape(&module).map_err(|e| e.to_string())?;
        }
        self.modules.lock().unwrap().insert(host.to_path_buf(), (stamp.0, stamp.1, module.clone()));
        Ok(module)
    }

    /// Runs process `pid` on the current thread until it ends (following
    /// any execve), records its status, and returns it.
    pub fn run(self: &Arc<Self>, pid: i32, mut image: ExecImage, mut inherit: Inherit) -> Termination {
        let how = loop {
            match process::run_image(self, pid, image, inherit) {
                process::Outcome::Done(how) => break how,
                process::Outcome::Exec(next, state) => {
                    image = next;
                    inherit = state;
                }
            }
        };
        self.exit(pid, how);
        how
    }

    /// Starts process `pid` on a new thread.
    pub fn spawn(self: &Arc<Self>, pid: i32, image: ExecImage, inherit: Inherit) {
        let kernel = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("pid {pid}"))
            // wasmtime runs guest code on the host stack.
            .stack_size(8 << 20)
            .spawn(move || kernel.run(pid, image, inherit));
        if spawned.is_err() {
            self.exit(pid, Termination::Exit(127));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel() -> Arc<Kernel> {
        Kernel::new(Engine::default(), std::env::temp_dir(), false)
    }

    #[test]
    fn statuses() {
        assert_eq!(Termination::Exit(3).wait_status(), 3 << 8);
        assert_eq!(Termination::Exit(256 + 3).wait_status(), 3 << 8, "exit codes are 8 bits");
        assert_eq!(Termination::Signal(15).wait_status(), 15);
        assert_eq!(Termination::Exit(3).shell_status(), 3);
        assert_eq!(Termination::Signal(15).shell_status(), 143);
    }

    #[test]
    fn pids_are_sequential_and_inherit_groups() {
        let k = kernel();
        let parent = k.create(0);
        let child = k.create(parent);
        assert_eq!((parent, child), (1, 2));
        assert_eq!(k.ppid(child), parent);
        assert_eq!(k.pgid(child), Ok(parent), "a child joins its parent's process group");
        assert_eq!(k.sid(child), Ok(parent));
    }

    #[test]
    fn wait_reaps_an_exited_child_once() {
        let k = kernel();
        let parent = k.create(0);
        let child = k.create(parent);
        assert_eq!(k.wait(parent, -1, true), Ok(None), "WNOHANG while the child runs");
        k.exit(child, Termination::Exit(7));
        assert_eq!(k.wait(parent, -1, false), Ok(Some((child, Termination::Exit(7)))));
        assert_eq!(k.wait(parent, -1, true), Err(ECHILD), "reaped: no children left");
    }

    #[test]
    fn wait_without_children_is_echild() {
        let k = kernel();
        let pid = k.create(0);
        assert_eq!(k.wait(pid, -1, false), Err(ECHILD));
    }

    #[test]
    fn wait_matches_pid_and_group_targets() {
        let k = kernel();
        let parent = k.create(0);
        let a = k.create(parent);
        let b = k.create(parent);
        k.setpgid(b, b).unwrap();
        k.exit(a, Termination::Exit(1));
        k.exit(b, Termination::Exit(2));
        assert_eq!(k.wait(parent, b, false), Ok(Some((b, Termination::Exit(2)))), "specific pid");
        assert_eq!(k.wait(parent, -b, true), Err(ECHILD), "group of b is now empty");
        assert_eq!(k.wait(parent, 0, false), Ok(Some((a, Termination::Exit(1)))), "own group");
    }

    #[test]
    fn wait_blocks_until_a_child_exits() {
        let k = kernel();
        let parent = k.create(0);
        let child = k.create(parent);
        let k2 = k.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            k2.exit(child, Termination::Signal(9));
        });
        assert_eq!(k.wait(parent, child, false), Ok(Some((child, Termination::Signal(9)))));
        t.join().unwrap();
    }

    #[test]
    fn a_signal_interrupts_wait() {
        let k = kernel();
        let parent = k.create(0);
        let _child = k.create(parent);
        k.kill(parent, parent, 15).unwrap();
        assert_eq!(k.wait(parent, -1, false), Err(EINTR));
        assert_eq!(k.take_signal(parent), Some(15), "wait leaves the signal pending");
    }

    #[test]
    fn orphans_are_not_left_as_zombies() {
        let k = kernel();
        let parent = k.create(0);
        let exited = k.create(parent);
        let running = k.create(parent);
        k.exit(exited, Termination::Exit(0));
        k.exit(parent, Termination::Exit(0));
        assert_eq!(k.ppid(running), 0, "reparented to nobody");
        assert_eq!(k.pgid(exited), Err(ESRCH), "exited orphan forgotten");
        k.exit(running, Termination::Exit(0));
        assert_eq!(k.pgid(running), Err(ESRCH), "running orphan cleans up when it exits");
    }

    #[test]
    fn kill_targets() {
        let k = kernel();
        let a = k.create(0);
        let b = k.create(a);
        let c = k.create(a);
        k.setpgid(c, c).unwrap();
        assert_eq!(k.kill(a, 99, 15), Err(ESRCH), "no such pid");
        assert_eq!(k.kill(a, b, 0), Ok(()), "signal 0 checks existence only");
        assert_eq!(k.peek_signal(b), None);
        k.kill(a, 0, 1).unwrap(); // a's group: a and b
        assert_eq!((k.peek_signal(a), k.peek_signal(b), k.peek_signal(c)), (Some(1), Some(1), None));
        k.kill(a, -c, 2).unwrap(); // group c
        assert_eq!(k.take_signal(c), Some(2));
        assert_eq!(k.take_signal(c), None, "taking consumes it");
        k.exit(b, Termination::Exit(0));
        assert_eq!(k.kill(a, b, 0), Err(ESRCH), "exited processes can't be signalled");
    }

    #[test]
    fn setsid_makes_a_new_session_and_group() {
        let k = kernel();
        let a = k.create(0);
        let b = k.create(a);
        assert_eq!(k.setsid(b), Ok(b));
        assert_eq!((k.pgid(b), k.sid(b)), (Ok(b), Ok(b)));
    }
}
