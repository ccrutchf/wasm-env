//! The runtime's own Wayland compositor (docs/abi.md, "Wayland"): on every
//! host, graphical programs are clients of the runtime itself. It speaks
//! the protocol over the runtime's Unix sockets (unix.rs), reads clients'
//! shared-memory buffers straight from the files they pass, and shows each
//! toplevel as a window of the host's (window.rs), or of nobody's
//! (headless, for tests). Nothing of the host's Wayland is needed.
//!
//! Each client connection has a thread that reads its requests
//! (client.rs); host input arrives from the display's thread; a clock
//! fires frame callbacks at 60 Hz.

pub mod client;
pub mod protocol;
pub mod window;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use client::Client;

/// Where the environment's clients find the compositor.
pub const SOCKET: &str = "/run/user/0/wayland-0";

/// A window's picture: logical size times `scale` pixels, 0xAARRGGBB.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub pixels: Arc<Vec<u32>>,
}

/// What the host tells the compositor about a window (coordinates are
/// logical, in the window).
#[derive(Debug, Clone)]
pub enum Input {
    Motion { x: f64, y: f64 },
    Leave,
    Button { button: u32, pressed: bool },
    Axis { dx: f64, dy: f64 },
    /// An evdev key code.
    Key { code: u32, pressed: bool },
    /// xkb modifier masks: shift 1, lock 2, control 4, alt 8, super 64.
    Modifiers { depressed: u32, locked: u32 },
    Focus(bool),
    Resize { width: u32, height: u32 },
    Close,
}

/// The host's display (in logical pixels) and its scale.
#[derive(Clone, Copy, Debug)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub refresh_mhz: u32,
}

/// Where windows appear.
pub trait Display: Send + Sync {
    fn open(&self, window: u64, title: &str, width: u32, height: u32);
    fn present(&self, window: u64, frame: Frame);
    fn set_title(&self, window: u64, title: &str);
    fn close(&self, window: u64);
    fn output(&self) -> Output;
    /// Begins moving the window with the pointer (a client's own title bar).
    fn start_move(&self, _window: u64) {}
}

/// No windows at all (tests): frames are counted and dropped.
#[derive(Default)]
pub struct Headless {
    pub frames: AtomicU64,
}

impl Display for Headless {
    fn open(&self, _: u64, _: &str, _: u32, _: u32) {}
    fn present(&self, window: u64, frame: Frame) {
        let n = self.frames.fetch_add(1, Ordering::Relaxed);
        // WASM_ENV_DEBUG_WAYLAND: what's drawn, now and then (tests check
        // that frames hold pictures, not only that there are frames).
        if n % 60 == 0 && std::env::var_os("WASM_ENV_DEBUG_WAYLAND").is_some() {
            let drawn = frame.pixels.iter().filter(|&&p| p & 0xff_ffff != 0).count();
            eprintln!("wasm-env: window {window} frame {n}: {}x{}, {drawn} pixels not black", frame.width * frame.scale, frame.height * frame.scale);
        }
    }
    fn set_title(&self, _: u64, _: &str) {}
    fn close(&self, _: u64) {}
    fn output(&self) -> Output {
        Output { width: 1920, height: 1080, scale: 1, refresh_mhz: 60_000 }
    }
}

pub struct Compositor {
    pub display: Arc<dyn Display>,
    serial: AtomicU32,
    next_client: AtomicU64,
    next_window: AtomicU64,
    started: Instant,
    /// Windows: which client, and which of its xdg_toplevels.
    windows: Mutex<HashMap<u64, (Weak<Client>, u32)>>,
    /// Frame callbacks due at the next tick: (client, callback id).
    due: Mutex<Vec<(Weak<Client>, u32)>>,
}

impl Compositor {
    /// Starts the compositor: clients connecting to SOCKET in `names` are
    /// served; windows go to `display`.
    pub fn start(names: &crate::unix::Names, display: Arc<dyn Display>) -> Arc<Compositor> {
        let c = Arc::new(Compositor {
            display,
            serial: AtomicU32::new(1),
            next_client: AtomicU64::new(1),
            next_window: AtomicU64::new(1),
            started: Instant::now(),
            windows: Mutex::new(HashMap::new()),
            due: Mutex::new(Vec::new()),
        });
        let serve = {
            let c = Arc::downgrade(&c);
            move |sock: Arc<crate::unix::UnixSocket>| {
                if let Some(c) = c.upgrade() {
                    let id = c.next_client.fetch_add(1, Ordering::Relaxed);
                    let _ = std::thread::Builder::new().name(format!("wayland client {id}")).spawn(move || client::serve(c, sock));
                }
            }
        };
        names.serve(crate::unix::Name::Path(SOCKET.to_string()), Arc::new(serve));
        // The frame clock.
        let w = Arc::downgrade(&c);
        let _ = std::thread::Builder::new().name("wayland frame clock".into()).spawn(move || {
            while let Some(c) = w.upgrade() {
                c.tick();
                drop(c);
                std::thread::sleep(Duration::from_micros(16_667));
            }
        });
        c
    }

    pub fn serial(&self) -> u32 {
        self.serial.fetch_add(1, Ordering::Relaxed)
    }

    /// Milliseconds since the compositor started (event timestamps).
    pub fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    pub fn new_window(&self, client: &Arc<Client>, toplevel: u32) -> u64 {
        let id = self.next_window.fetch_add(1, Ordering::Relaxed);
        self.windows.lock().unwrap().insert(id, (Arc::downgrade(client), toplevel));
        id
    }

    pub fn forget_window(&self, window: u64) {
        self.windows.lock().unwrap().remove(&window);
        self.display.close(window);
    }

    pub fn frame_due(&self, client: &Arc<Client>, callback: u32) {
        self.due.lock().unwrap().push((Arc::downgrade(client), callback));
    }

    fn tick(&self) {
        let due = std::mem::take(&mut *self.due.lock().unwrap());
        let time = self.time();
        for (client, callback) in due {
            if let Some(client) = client.upgrade() {
                client.frame_done(callback, time);
            }
        }
    }

    /// Host input for a window.
    pub fn input(&self, window: u64, input: Input) {
        let target = self.windows.lock().unwrap().get(&window).cloned();
        if let Some((client, toplevel)) = target
            && let Some(client) = client.upgrade()
        {
            client.input(self, toplevel, input);
        }
    }
}

#[cfg(test)]
mod tests;
