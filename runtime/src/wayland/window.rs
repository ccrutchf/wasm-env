//! Windows on the host's own window system (winit), drawn in software
//! (softbuffer): the compositor's display on every host. The event loop
//! runs on the main thread (macOS requires it); the compositor sends it
//! commands, and it sends host input back.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use super::{Compositor, Display, Frame, Input, Output};

pub enum Command {
    Open { window: u64, title: String, width: u32, height: u32 },
    Present { window: u64, frame: Frame },
    Title { window: u64, title: String },
    Close { window: u64 },
    Move { window: u64 },
    /// The program ended: leave the event loop, exiting with this status.
    Exit(i32),
}

/// The compositor's side: commands to the event loop.
pub struct Windowed {
    proxy: Mutex<EventLoopProxy<Command>>,
    output: Mutex<Output>,
}

impl Display for Windowed {
    fn open(&self, window: u64, title: &str, width: u32, height: u32) {
        self.send(Command::Open { window, title: title.into(), width, height });
    }
    fn present(&self, window: u64, frame: Frame) {
        self.send(Command::Present { window, frame });
    }
    fn set_title(&self, window: u64, title: &str) {
        self.send(Command::Title { window, title: title.into() });
    }
    fn close(&self, window: u64) {
        self.send(Command::Close { window });
    }
    fn output(&self) -> Output {
        *self.output.lock().unwrap()
    }
    fn start_move(&self, window: u64) {
        self.send(Command::Move { window });
    }
}

impl Windowed {
    fn send(&self, c: Command) {
        let _ = self.proxy.lock().unwrap().send_event(c);
    }

    /// Ends the event loop (the program finished).
    pub fn exit(&self, status: i32) {
        self.send(Command::Exit(status));
    }
}

/// An event loop on this (the main) thread, if the host has a window
/// system: returns the display to give the compositor, and a function
/// that runs the loop until the program ends, returning its status.
pub fn event_loop() -> Option<(Arc<Windowed>, impl FnOnce(Arc<Compositor>) -> i32)> {
    let event_loop = EventLoop::<Command>::with_user_event().build().ok()?;
    let windowed = Arc::new(Windowed {
        proxy: Mutex::new(event_loop.create_proxy()),
        output: Mutex::new(Output { width: 1920, height: 1080, scale: 1, refresh_mhz: 60_000 }),
    });
    let w = windowed.clone();
    let run = move |comp: Arc<Compositor>| {
        let mut app = App { comp, windowed: w, windows: HashMap::new(), ids: HashMap::new(), status: 0, modifiers: ModifiersState::empty() };
        let _ = event_loop.run_app(&mut app);
        app.status
    };
    Some((windowed, run))
}

struct Win {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    frame: Option<Frame>,
}

struct App {
    comp: Arc<Compositor>,
    windowed: Arc<Windowed>,
    windows: HashMap<u64, Win>,
    ids: HashMap<WindowId, u64>,
    status: i32,
    modifiers: ModifiersState,
}

impl App {
    fn input(&self, window: u64, input: Input) {
        self.comp.input(window, input);
    }

    fn redraw(&mut self, window: u64) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        let Some(frame) = &w.frame else { return };
        let size = w.window.inner_size();
        let (Some(pw), Some(ph)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if w.surface.resize(pw, ph).is_err() {
            return;
        }
        let Ok(mut buf) = w.surface.buffer_mut() else { return };
        // The frame, scaled (nearest) to the window's physical pixels.
        let (fw, fh) = (frame.width * frame.scale, frame.height * frame.scale);
        let (sw, sh) = (size.width, size.height);
        for y in 0..sh {
            let fy = (y as u64 * fh as u64 / sh as u64) as u32;
            for x in 0..sw {
                let fx = (x as u64 * fw as u64 / sw as u64) as u32;
                buf[(y * sw + x) as usize] = frame.pixels.get((fy * fw + fx) as usize).copied().unwrap_or(0) & 0xff_ffff;
            }
        }
        let _ = buf.present();
    }
}

impl ApplicationHandler<Command> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(m) = event_loop.primary_monitor() {
            let scale = m.scale_factor();
            let size = m.size().to_logical::<f64>(scale);
            *self.windowed.output.lock().unwrap() = Output {
                width: size.width as u32,
                height: size.height as u32,
                scale: scale.round().max(1.0) as u32,
                refresh_mhz: m.refresh_rate_millihertz().unwrap_or(60_000),
            };
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, c: Command) {
        match c {
            Command::Open { window, title, width, height } => {
                let attrs = Window::default_attributes().with_title(if title.is_empty() { "wasm-env".into() } else { title }).with_inner_size(LogicalSize::new(width, height));
                let w = match event_loop.create_window(attrs) {
                    Ok(w) => Arc::new(w),
                    Err(e) => return eprintln!("wasm-env: can't open a window: {e}"),
                };
                let surface = softbuffer::Context::new(w.clone()).and_then(|c| softbuffer::Surface::new(&c, w.clone()));
                let surface = match surface {
                    Ok(s) => s,
                    Err(e) => return eprintln!("wasm-env: can't draw in a window: {e}"),
                };
                self.ids.insert(w.id(), window);
                self.windows.insert(window, Win { window: w, surface, frame: None });
            }
            Command::Present { window, frame } => {
                if let Some(w) = self.windows.get_mut(&window) {
                    // The client chose a size: the window follows.
                    let logical: LogicalSize<u32> = w.window.inner_size().to_logical(w.window.scale_factor());
                    if (logical.width, logical.height) != (frame.width, frame.height) {
                        let _ = w.window.request_inner_size(LogicalSize::new(frame.width, frame.height));
                    }
                    w.frame = Some(frame);
                    w.window.request_redraw();
                }
            }
            Command::Title { window, title } => {
                if let Some(w) = self.windows.get(&window) {
                    w.window.set_title(&title);
                }
            }
            Command::Close { window } => {
                if let Some(w) = self.windows.remove(&window) {
                    self.ids.remove(&w.window.id());
                }
            }
            Command::Move { window } => {
                if let Some(w) = self.windows.get(&window) {
                    let _ = w.window.drag_window();
                }
            }
            Command::Exit(status) => {
                self.status = status;
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(&window) = self.ids.get(&id) else { return };
        let scale = self.windows.get(&window).map_or(1.0, |w| w.window.scale_factor());
        match event {
            WindowEvent::RedrawRequested => self.redraw(window),
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.input(window, Input::Motion { x: x / scale, y: y / scale });
            }
            WindowEvent::CursorLeft { .. } => self.input(window, Input::Leave),
            WindowEvent::MouseInput { state, button, .. } => {
                // Linux's input event codes.
                let button = match button {
                    MouseButton::Left => 0x110,
                    MouseButton::Right => 0x111,
                    MouseButton::Middle => 0x112,
                    MouseButton::Back => 0x116,
                    MouseButton::Forward => 0x115,
                    MouseButton::Other(n) => 0x110 + n as u32,
                };
                self.input(window, Input::Button { button, pressed: state == ElementState::Pressed });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Wayland: positive is down or right, in pixels (10 a notch).
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x as f64 * 10.0, -y as f64 * 10.0),
                    MouseScrollDelta::PixelDelta(p) => (-p.x / scale, -p.y / scale),
                };
                self.input(window, Input::Axis { dx, dy });
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // The client repeats keys itself.
                if event.repeat {
                    return;
                }
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(code) = evdev(code)
                {
                    self.input(window, Input::Key { code, pressed: event.state == ElementState::Pressed });
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                let s = self.modifiers;
                let depressed = (s.shift_key() as u32) | (s.control_key() as u32) << 2 | (s.alt_key() as u32) << 3 | (s.super_key() as u32) << 6;
                self.input(window, Input::Modifiers { depressed, locked: 0 });
            }
            WindowEvent::Focused(on) => self.input(window, Input::Focus(on)),
            WindowEvent::Resized(size) => {
                let logical: LogicalSize<u32> = size.to_logical(scale);
                // Only a size the client didn't choose itself.
                let frame = self.windows.get(&window).and_then(|w| w.frame.as_ref().map(|f| (f.width, f.height)));
                if frame != Some((logical.width, logical.height)) {
                    self.input(window, Input::Resize { width: logical.width, height: logical.height });
                }
                if let Some(w) = self.windows.get(&window) {
                    w.window.request_redraw();
                }
            }
            WindowEvent::CloseRequested => self.input(window, Input::Close),
            _ => {}
        }
    }
}

/// A key's Linux input event code (what the xkb keymap's keycodes are,
/// less 8).
fn evdev(k: KeyCode) -> Option<u32> {
    use KeyCode::*;
    Some(match k {
        Escape => 1,
        Digit1 => 2,
        Digit2 => 3,
        Digit3 => 4,
        Digit4 => 5,
        Digit5 => 6,
        Digit6 => 7,
        Digit7 => 8,
        Digit8 => 9,
        Digit9 => 10,
        Digit0 => 11,
        Minus => 12,
        Equal => 13,
        Backspace => 14,
        Tab => 15,
        KeyQ => 16,
        KeyW => 17,
        KeyE => 18,
        KeyR => 19,
        KeyT => 20,
        KeyY => 21,
        KeyU => 22,
        KeyI => 23,
        KeyO => 24,
        KeyP => 25,
        BracketLeft => 26,
        BracketRight => 27,
        Enter => 28,
        ControlLeft => 29,
        KeyA => 30,
        KeyS => 31,
        KeyD => 32,
        KeyF => 33,
        KeyG => 34,
        KeyH => 35,
        KeyJ => 36,
        KeyK => 37,
        KeyL => 38,
        Semicolon => 39,
        Quote => 40,
        Backquote => 41,
        ShiftLeft => 42,
        Backslash => 43,
        KeyZ => 44,
        KeyX => 45,
        KeyC => 46,
        KeyV => 47,
        KeyB => 48,
        KeyN => 49,
        KeyM => 50,
        Comma => 51,
        Period => 52,
        Slash => 53,
        ShiftRight => 54,
        NumpadMultiply => 55,
        AltLeft => 56,
        Space => 57,
        CapsLock => 58,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        NumLock => 69,
        ScrollLock => 70,
        Numpad7 => 71,
        Numpad8 => 72,
        Numpad9 => 73,
        NumpadSubtract => 74,
        Numpad4 => 75,
        Numpad5 => 76,
        Numpad6 => 77,
        NumpadAdd => 78,
        Numpad1 => 79,
        Numpad2 => 80,
        Numpad3 => 81,
        Numpad0 => 82,
        NumpadDecimal => 83,
        IntlBackslash => 86,
        F11 => 87,
        F12 => 88,
        NumpadEnter => 96,
        ControlRight => 97,
        NumpadDivide => 98,
        PrintScreen => 99,
        AltRight => 100,
        Home => 102,
        ArrowUp => 103,
        PageUp => 104,
        ArrowLeft => 105,
        ArrowRight => 106,
        End => 107,
        ArrowDown => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        Pause => 119,
        SuperLeft => 125,
        SuperRight => 126,
        ContextMenu => 127,
        _ => return None,
    })
}
