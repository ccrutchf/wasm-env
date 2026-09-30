//! The host terminal, when wasm-env runs in one: programs change its
//! settings (a shell's line editor puts it in raw mode), so they're
//! restored when wasm-env exits; and Ctrl-C and Ctrl-\ go to the guest's
//! foreground process group (see Kernel::foreground), not to wasm-env.

use std::sync::Arc;
#[cfg_attr(not(unix), allow(unused_imports))]
use std::sync::atomic::{AtomicI32, Ordering};

use crate::kernel::Kernel;

/// The last terminal signal the host received (0: none).
#[cfg_attr(not(unix), allow(dead_code))]
static RECEIVED: AtomicI32 = AtomicI32::new(0);

pub struct Terminal {
    #[cfg(unix)]
    saved: Option<libc::termios>,
}

#[cfg(unix)]
extern "C" fn received(sig: libc::c_int) {
    RECEIVED.store(sig, Ordering::Relaxed);
}

impl Terminal {
    /// Takes over the terminal if stdin is one.
    pub fn take(kernel: Arc<Kernel>) -> Terminal {
        #[cfg(unix)]
        {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                return Terminal { saved: None };
            }
            // SAFETY: tcgetattr writes a termios; `received` only stores to an
            // atomic, which is async-signal-safe.
            let saved = unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                let saved = (libc::tcgetattr(0, &mut t) == 0).then_some(t);
                for sig in [libc::SIGINT, libc::SIGQUIT] {
                    libc::signal(sig, received as *const () as libc::sighandler_t);
                }
                // Stopping isn't supported: Ctrl-Z would suspend wasm-env
                // itself, leaving the terminal as the guest set it.
                libc::signal(libc::SIGTSTP, libc::SIG_IGN);
                saved
            };
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    let sig = RECEIVED.swap(0, Ordering::Relaxed);
                    if sig != 0 {
                        let group = kernel.foreground.load(Ordering::Relaxed);
                        let _ = kernel.kill(0, -group, sig);
                    }
                }
            });
            Terminal { saved }
        }
        #[cfg(not(unix))]
        {
            let _ = kernel;
            Terminal {}
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(t) = &self.saved {
            // SAFETY: restores settings tcgetattr returned.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
        }
    }
}
