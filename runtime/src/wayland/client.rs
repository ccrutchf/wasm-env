//! One client of the compositor: the objects it has made and the requests
//! it sends, read from its connection by a thread of its own.
//!
//! Supported: wl_compositor (surfaces, regions), wl_subcompositor,
//! wl_shm (pools and buffers of ARGB8888 and XRGB8888), wl_seat (pointer,
//! keyboard), wl_output (and xdg_output), xdg_wm_base (toplevels and
//! popups), a clipboard that holds nothing (wl_data_device_manager), and
//! wp_viewporter (accepted; buffers aren't scaled yet).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use super::protocol::{Value, decode, encode, interface};
use super::{Compositor, Frame, Input};
use crate::fs::Open;
use crate::unix::{Names, Passed, UnixSocket};

/// Globals: (name, interface, version offered).
const GLOBALS: &[(u32, &str, u32)] = &[
    (1, "wl_compositor", 5),
    (2, "wl_subcompositor", 1),
    (3, "wl_shm", 1),
    (4, "wl_seat", 5),
    (5, "wl_output", 3),
    (6, "xdg_wm_base", 3),
    (7, "wl_data_device_manager", 3),
    (8, "wp_viewporter", 1),
    (9, "zxdg_output_manager_v1", 3),
];

/// The first id the compositor gives objects it makes.
const SERVER_IDS: u32 = 0xff00_0000;

const KEYMAP: &str = include_str!("../../protocols/keymap-us.xkb");

/// A connected stream needs no names to send.
static NO_NAMES: LazyLock<Names> = LazyLock::new(Names::default);

#[derive(Default)]
struct Surface {
    /// Attached since the last commit: Some(None) detaches the buffer.
    pending_buffer: Option<Option<u32>>,
    pending_frames: Vec<u32>,
    pending_scale: Option<u32>,
    pixels: Arc<Vec<u32>>,
    /// In buffer pixels.
    width: u32,
    height: u32,
    scale: u32,
    /// Subsurfaces, bottom to top, and (for a subsurface) its parent and
    /// position.
    children: Vec<u32>,
    parent: Option<u32>,
    position: (i32, i32),
    pending_position: Option<(i32, i32)>,
    /// Its xdg_surface, if it has one.
    xdg: Option<u32>,
}

impl Surface {
    fn logical(&self) -> (i32, i32) {
        let s = self.scale.max(1);
        ((self.width / s) as i32, (self.height / s) as i32)
    }
}

#[derive(Default)]
struct XdgSurface {
    surface: u32,
    geometry: Option<(i32, i32, i32, i32)>,
    pending_geometry: Option<(i32, i32, i32, i32)>,
    configured: bool,
    /// Its xdg_toplevel or xdg_popup.
    role: Option<u32>,
}

struct Toplevel {
    xdg: u32,
    window: Option<u64>,
    title: String,
    opened: bool,
}

struct Popup {
    xdg: u32,
    parent: u32, // its parent's xdg_surface
    x: i32,
    y: i32,
}

#[derive(Default, Clone, Copy)]
struct Positioner {
    size: (i32, i32),
    rect: (i32, i32, i32, i32),
    anchor: u32,
    gravity: u32,
    offset: (i32, i32),
}

struct Buffer {
    pool: Arc<Passed>,
    offset: i32,
    width: i32,
    height: i32,
    stride: i32,
    format: u32,
}

enum Kind {
    Plain,
    Surface(Surface),
    Pool(Arc<Passed>),
    Buffer(Buffer),
    Xdg(XdgSurface),
    Toplevel(Toplevel),
    Popup(Popup),
    Positioner(Positioner),
    Subsurface(u32),
}

struct Object {
    interface: &'static str,
    version: u32,
    kind: Kind,
}

#[derive(Default)]
struct State {
    objects: HashMap<u32, Object>,
    pointers: Vec<u32>,
    keyboards: Vec<u32>,
    outputs: Vec<u32>,
    /// Popups, oldest first.
    popups: Vec<u32>,
    /// The surface under the pointer (and where on it), and the one with
    /// the keyboard.
    pointer_on: Option<u32>,
    keyboard_on: Option<u32>,
}

pub struct Client {
    sock: Arc<UnixSocket>,
    state: Mutex<State>,
}

/// Serves one connection until the client goes.
pub fn serve(comp: Arc<Compositor>, sock: Arc<UnixSocket>) {
    let client = Arc::new(Client { sock: sock.clone(), state: Mutex::new(State::default()) });
    client.state.lock().unwrap().objects.insert(1, Object { interface: "wl_display", version: 1, kind: Kind::Plain });
    let mut buf: Vec<u8> = Vec::new();
    let mut fds: VecDeque<Passed> = VecDeque::new();
    loop {
        let Ok(got) = sock.recv(1 << 16, false, false, false, &|| false) else { break };
        if got.data.is_empty() {
            break; // gone
        }
        buf.extend_from_slice(&got.data);
        fds.extend(got.fds);
        let mut at = 0;
        while buf.len() - at >= 8 {
            let object = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
            let word = u32::from_le_bytes(buf[at + 4..at + 8].try_into().unwrap());
            let (size, opcode) = ((word >> 16) as usize, (word & 0xffff) as u16);
            if size < 8 {
                client.close();
                return;
            }
            if buf.len() - at < size {
                break;
            }
            let body = buf[at + 8..at + size].to_vec();
            at += size;
            client.request(&comp, object, opcode, &body, &mut fds);
        }
        buf.drain(..at);
    }
    client.close_windows(&comp);
}

impl Client {
    fn close(&self) {
        let _ = self.sock.shutdown(2);
    }

    /// Sends an event on `id` (an object of `iface`).
    fn send(&self, id: u32, iface: &str, event: &str, args: Vec<Value>) {
        let (bytes, fds) = encode(id, interface(iface).event(event), args);
        let _ = self.sock.send(&NO_NAMES, &bytes, fds, None, false, &|| false);
    }

    fn delete_id(&self, id: u32) {
        self.send(1, "wl_display", "delete_id", vec![Value::Uint(id)]);
    }

    fn close_windows(&self, comp: &Compositor) {
        let st = self.state.lock().unwrap();
        for o in st.objects.values() {
            if let Kind::Toplevel(t) = &o.kind
                && let Some(w) = t.window
            {
                comp.forget_window(w);
            }
        }
    }

    pub fn frame_done(&self, callback: u32, time: u32) {
        let mut st = self.state.lock().unwrap();
        if st.objects.remove(&callback).is_some() {
            self.send(callback, "wl_callback", "done", vec![Value::Uint(time)]);
            self.delete_id(callback);
        }
    }

    fn request(self: &Arc<Self>, comp: &Compositor, object: u32, opcode: u16, body: &[u8], fds: &mut VecDeque<Passed>) {
        let mut st = self.state.lock().unwrap();
        let Some(o) = st.objects.get(&object) else { return };
        let (iface, version) = (o.interface, o.version);
        let Some(m) = interface(iface).requests.get(opcode as usize) else { return };
        let Some(args) = decode(m, body, fds) else {
            self.close();
            return;
        };
        let name = m.name.as_str();
        // Objects a request makes take their interface from the message
        // (or, for bind, from the request itself), and the version of
        // the object making them.
        let made = |i: usize| m.args.get(i).and_then(|a| a.interface.as_deref()).map(|n| interface(n).name.as_str());
        match (iface, name) {
            ("wl_display", "sync") => {
                let cb = args[0].uint();
                self.send(cb, "wl_callback", "done", vec![Value::Uint(comp.serial())]);
                self.delete_id(cb);
            }
            ("wl_display", "get_registry") => {
                let id = args[0].uint();
                st.objects.insert(id, Object { interface: "wl_registry", version: 1, kind: Kind::Plain });
                for &(n, i, v) in GLOBALS {
                    self.send(id, "wl_registry", "global", vec![Value::Uint(n), Value::Str(Some(i.into())), Value::Uint(v)]);
                }
            }
            ("wl_registry", "bind") => {
                let Value::NewIdOf { interface: i, version: v, id } = &args[1] else { return };
                let Some(&(_, global, offered)) = GLOBALS.iter().find(|g| g.0 == args[0].uint()) else { return };
                if global != i.as_str() {
                    return;
                }
                // (At most what we offer, and what the protocol file describes.)
                let (id, v) = (*id, (*v).min(offered).min(interface(global).version));
                st.objects.insert(id, Object { interface: global, version: v, kind: Kind::Plain });
                self.bound(comp, &mut st, global, id, v);
            }
            ("wl_compositor", "create_surface") => {
                st.objects.insert(args[0].uint(), Object { interface: "wl_surface", version, kind: Kind::Surface(Surface { scale: 1, ..Default::default() }) });
            }
            ("wl_compositor", "create_region") | ("wl_data_device_manager", "create_data_source") => {
                st.objects.insert(args[0].uint(), Object { interface: made(0).unwrap(), version, kind: Kind::Plain });
            }
            ("wl_surface", "attach") => {
                let buffer = args[0].uint();
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    s.pending_buffer = Some((buffer != 0).then_some(buffer));
                }
            }
            ("wl_surface", "frame") => {
                let cb = args[0].uint();
                st.objects.insert(cb, Object { interface: "wl_callback", version: 1, kind: Kind::Plain });
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    s.pending_frames.push(cb);
                }
            }
            ("wl_surface", "set_buffer_scale") => {
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    s.pending_scale = Some(args[0].uint().max(1));
                }
            }
            ("wl_surface", "commit") => self.commit(comp, &mut st, object),
            ("wl_shm", "create_pool") => {
                let (id, mut a) = (args[0].uint(), args);
                let Value::Fd(fd) = a.swap_remove(1) else { return };
                st.objects.insert(id, Object { interface: "wl_shm_pool", version, kind: Kind::Pool(Arc::new(fd)) });
            }
            ("wl_shm_pool", "create_buffer") => {
                let Some(Kind::Pool(pool)) = st.objects.get(&object).map(|o| &o.kind) else { return };
                let b = Buffer {
                    pool: pool.clone(),
                    offset: args[1].int(),
                    width: args[2].int(),
                    height: args[3].int(),
                    stride: args[4].int(),
                    format: args[5].uint(),
                };
                st.objects.insert(args[0].uint(), Object { interface: "wl_buffer", version, kind: Kind::Buffer(b) });
            }
            ("wl_seat", "get_pointer") => {
                let id = args[0].uint();
                st.objects.insert(id, Object { interface: "wl_pointer", version, kind: Kind::Plain });
                st.pointers.push(id);
            }
            ("wl_seat", "get_keyboard") => {
                let id = args[0].uint();
                st.objects.insert(id, Object { interface: "wl_keyboard", version, kind: Kind::Plain });
                st.keyboards.push(id);
                if let Some(fd) = keymap() {
                    self.send(id, "wl_keyboard", "keymap", vec![Value::Uint(1), Value::Fd(fd), Value::Uint(KEYMAP.len() as u32 + 1)]);
                }
                if version >= 4 {
                    self.send(id, "wl_keyboard", "repeat_info", vec![Value::Int(25), Value::Int(600)]);
                }
                // A client whose window has the keyboard gets it at once.
                if let Some(s) = st.keyboard_on {
                    self.send(id, "wl_keyboard", "enter", vec![Value::Uint(comp.serial()), Value::Object(s), Value::Array(Vec::new())]);
                }
            }
            ("wl_seat", "get_touch") => {
                st.objects.insert(args[0].uint(), Object { interface: "wl_touch", version, kind: Kind::Plain });
            }
            ("xdg_wm_base", "create_positioner") => {
                st.objects.insert(args[0].uint(), Object { interface: "xdg_positioner", version, kind: Kind::Positioner(Positioner::default()) });
            }
            ("xdg_wm_base", "get_xdg_surface") => {
                let (id, surface) = (args[0].uint(), args[1].uint());
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&surface).map(|o| &mut o.kind) {
                    s.xdg = Some(id);
                }
                st.objects.insert(id, Object { interface: "xdg_surface", version, kind: Kind::Xdg(XdgSurface { surface, ..Default::default() }) });
            }
            ("xdg_positioner", _) => {
                if let Some(Kind::Positioner(p)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    match name {
                        "set_size" => p.size = (args[0].int(), args[1].int()),
                        "set_anchor_rect" => p.rect = (args[0].int(), args[1].int(), args[2].int(), args[3].int()),
                        "set_anchor" => p.anchor = args[0].uint(),
                        "set_gravity" => p.gravity = args[0].uint(),
                        "set_offset" => p.offset = (args[0].int(), args[1].int()),
                        _ => {}
                    }
                }
                if name == "destroy" {
                    self.destroy(comp, &mut st, object);
                }
            }
            ("xdg_surface", "get_toplevel") => {
                let id = args[0].uint();
                if let Some(Kind::Xdg(x)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    x.role = Some(id);
                }
                st.objects.insert(id, Object { interface: "xdg_toplevel", version, kind: Kind::Toplevel(Toplevel { xdg: object, window: None, title: String::new(), opened: false }) });
            }
            ("xdg_surface", "get_popup") => {
                let (id, parent, positioner) = (args[0].uint(), args[1].uint(), args[2].uint());
                let p = match st.objects.get(&positioner).map(|o| &o.kind) {
                    Some(Kind::Positioner(p)) => *p,
                    _ => Positioner::default(),
                };
                let (x, y) = place(&p);
                if let Some(Kind::Xdg(xs)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    xs.role = Some(id);
                }
                st.objects.insert(id, Object { interface: "xdg_popup", version, kind: Kind::Popup(Popup { xdg: object, parent, x, y }) });
                st.popups.push(id);
                self.send(id, "xdg_popup", "configure", vec![Value::Int(x), Value::Int(y), Value::Int(p.size.0), Value::Int(p.size.1)]);
                self.send(object, "xdg_surface", "configure", vec![Value::Uint(comp.serial())]);
            }
            ("xdg_surface", "set_window_geometry") => {
                if let Some(Kind::Xdg(x)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    x.pending_geometry = Some((args[0].int(), args[1].int(), args[2].int(), args[3].int()));
                }
            }
            ("xdg_toplevel", "set_title") => {
                let title = args[0].string();
                if let Some(Kind::Toplevel(t)) = st.objects.get_mut(&object).map(|o| &mut o.kind) {
                    t.title = title.clone();
                    if let Some(w) = t.window {
                        comp.display.set_title(w, &title);
                    }
                }
            }
            ("xdg_toplevel", "move") => {
                if let Some(Kind::Toplevel(t)) = st.objects.get(&object).map(|o| &o.kind)
                    && let Some(w) = t.window
                {
                    comp.display.start_move(w);
                }
            }
            ("wl_subcompositor", "get_subsurface") => {
                let (id, surface, parent) = (args[0].uint(), args[1].uint(), args[2].uint());
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&surface).map(|o| &mut o.kind) {
                    s.parent = Some(parent);
                }
                if let Some(Kind::Surface(p)) = st.objects.get_mut(&parent).map(|o| &mut o.kind) {
                    p.children.push(surface);
                }
                st.objects.insert(id, Object { interface: "wl_subsurface", version, kind: Kind::Subsurface(surface) });
            }
            ("wl_subsurface", "set_position") => {
                if let Some(Kind::Subsurface(s)) = st.objects.get(&object).map(|o| &o.kind) {
                    let s = *s;
                    if let Some(Kind::Surface(s)) = st.objects.get_mut(&s).map(|o| &mut o.kind) {
                        s.pending_position = Some((args[0].int(), args[1].int()));
                    }
                }
            }
            ("wl_data_device_manager", "get_data_device") | ("wp_viewporter", "get_viewport") | ("zxdg_output_manager_v1", "get_xdg_output") => {
                let id = args[0].uint();
                st.objects.insert(id, Object { interface: made(0).unwrap(), version, kind: Kind::Plain });
                if iface == "zxdg_output_manager_v1" {
                    let out = comp.display.output();
                    self.send(id, "zxdg_output_v1", "logical_position", vec![Value::Int(0), Value::Int(0)]);
                    self.send(id, "zxdg_output_v1", "logical_size", vec![Value::Int(out.width as i32), Value::Int(out.height as i32)]);
                    if version >= 2 {
                        self.send(id, "zxdg_output_v1", "name", vec![Value::Str(Some("wasm-env".into()))]);
                    }
                    if version < 3 {
                        self.send(id, "zxdg_output_v1", "done", vec![]);
                    } else if let Some(&o) = st.outputs.first() {
                        self.send(o, "wl_output", "done", vec![]);
                    }
                }
            }
            (_, "destroy") | (_, "release") => self.destroy(comp, &mut st, object),
            _ => {} // accepted and ignored
        }
    }

    /// Events a global sends when bound.
    fn bound(&self, comp: &Compositor, st: &mut State, global: &str, id: u32, version: u32) {
        match global {
            "wl_shm" => {
                for format in [0, 1] {
                    self.send(id, "wl_shm", "format", vec![Value::Uint(format)]);
                }
            }
            "wl_seat" => {
                self.send(id, "wl_seat", "capabilities", vec![Value::Uint(3)]);
                if version >= 2 {
                    self.send(id, "wl_seat", "name", vec![Value::Str(Some("seat0".into()))]);
                }
            }
            "wl_output" => {
                st.outputs.push(id);
                let out = comp.display.output();
                let (w, h) = (out.width * out.scale, out.height * out.scale);
                self.send(id, "wl_output", "geometry", vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int((w as f64 / 3.8) as i32), // mm, at about 96 dpi
                    Value::Int((h as f64 / 3.8) as i32),
                    Value::Int(0),
                    Value::Str(Some("wasm-env".into())),
                    Value::Str(Some("display".into())),
                    Value::Int(0),
                ]);
                self.send(id, "wl_output", "mode", vec![Value::Uint(3), Value::Int(w as i32), Value::Int(h as i32), Value::Int(out.refresh_mhz as i32)]);
                if version >= 2 {
                    self.send(id, "wl_output", "scale", vec![Value::Int(out.scale as i32)]);
                    self.send(id, "wl_output", "done", vec![]);
                }
            }
            _ => {}
        }
    }

    fn destroy(self: &Arc<Self>, comp: &Compositor, st: &mut State, id: u32) {
        let Some(o) = st.objects.remove(&id) else { return };
        match o.kind {
            Kind::Toplevel(t) => {
                if let Some(w) = t.window {
                    comp.forget_window(w);
                }
            }
            Kind::Popup(p) => {
                st.popups.retain(|&q| q != id);
                if let Some(t) = self.toplevel_of(st, p.parent) {
                    self.present(comp, st, t);
                }
            }
            Kind::Surface(s) => {
                if let Some(p) = s.parent
                    && let Some(Kind::Surface(p)) = st.objects.get_mut(&p).map(|o| &mut o.kind)
                {
                    p.children.retain(|&c| c != id);
                }
                if st.pointer_on == Some(id) {
                    st.pointer_on = None;
                }
                if st.keyboard_on == Some(id) {
                    st.keyboard_on = None;
                }
            }
            Kind::Subsurface(surface) => {
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&surface).map(|o| &mut o.kind) {
                    if let Some(p) = s.parent.take() {
                        if let Some(Kind::Surface(p)) = st.objects.get_mut(&p).map(|o| &mut o.kind) {
                            p.children.retain(|&c| c != surface);
                        }
                    }
                }
            }
            _ => {}
        }
        st.pointers.retain(|&p| p != id);
        st.keyboards.retain(|&k| k != id);
        st.outputs.retain(|&k| k != id);
        if id < SERVER_IDS {
            self.delete_id(id);
        }
    }

    /// wl_surface.commit: the pending state becomes current; its window
    /// shows it.
    fn commit(self: &Arc<Self>, comp: &Compositor, st: &mut State, id: u32) {
        let Some(Kind::Surface(s)) = st.objects.get_mut(&id).map(|o| &mut o.kind) else { return };
        let buffer = s.pending_buffer.take();
        let frames = std::mem::take(&mut s.pending_frames);
        if let Some(scale) = s.pending_scale.take() {
            s.scale = scale;
        }
        if let Some(p) = s.pending_position.take() {
            s.position = p;
        }
        let xdg = s.xdg;
        match buffer {
            Some(Some(b)) => {
                if let Some(Kind::Buffer(buf)) = st.objects.get(&b).map(|o| &o.kind) {
                    let (w, h, pixels) = read_buffer(buf);
                    if let Some(Kind::Surface(s)) = st.objects.get_mut(&id).map(|o| &mut o.kind) {
                        (s.width, s.height, s.pixels) = (w, h, Arc::new(pixels));
                    }
                    // Copied: the client may reuse it.
                    self.send(b, "wl_buffer", "release", vec![]);
                }
            }
            Some(None) => {
                if let Some(Kind::Surface(s)) = st.objects.get_mut(&id).map(|o| &mut o.kind) {
                    (s.width, s.height, s.pixels) = (0, 0, Arc::new(Vec::new()));
                }
            }
            None => {}
        }
        for cb in frames {
            comp.frame_due(self, cb);
        }
        if let Some(x) = xdg
            && let Some(Kind::Xdg(xs)) = st.objects.get_mut(&x).map(|o| &mut o.kind)
        {
            if let Some(g) = xs.pending_geometry.take() {
                xs.geometry = Some(g);
            }
            // A toplevel's first commit asks for its first configure.
            if !xs.configured
                && let Some(role) = xs.role
            {
                xs.configured = true;
                if matches!(st.objects.get(&role).map(|o| &o.kind), Some(Kind::Toplevel(_))) {
                    self.send(role, "xdg_toplevel", "configure", vec![Value::Int(0), Value::Int(0), Value::Array(states(true))]);
                    self.send(x, "xdg_surface", "configure", vec![Value::Uint(comp.serial())]);
                }
            }
        }
        if let Some(t) = self.toplevel_of_surface(st, id) {
            self.present(comp, st, t);
        }
    }

    /// The xdg_toplevel whose window shows the surface `id` (as its own
    /// surface, a subsurface of it, or in a popup of it).
    fn toplevel_of_surface(&self, st: &State, mut id: u32) -> Option<u32> {
        for _ in 0..64 {
            let Some(Kind::Surface(s)) = st.objects.get(&id).map(|o| &o.kind) else { return None };
            if let Some(p) = s.parent {
                id = p;
                continue;
            }
            return self.toplevel_of(st, s.xdg?);
        }
        None
    }

    /// The xdg_toplevel an xdg_surface belongs to (following popups' parents).
    fn toplevel_of(&self, st: &State, mut xdg: u32) -> Option<u32> {
        for _ in 0..64 {
            let Some(Kind::Xdg(x)) = st.objects.get(&xdg).map(|o| &o.kind) else { return None };
            match st.objects.get(&x.role?).map(|o| &o.kind) {
                Some(Kind::Toplevel(_)) => return x.role,
                Some(Kind::Popup(p)) => xdg = p.parent,
                _ => return None,
            }
        }
        None
    }

    /// Composes a toplevel's window (its surfaces, subsurfaces and popups)
    /// and shows it, opening the host window the first time.
    fn present(self: &Arc<Self>, comp: &Compositor, st: &mut State, toplevel: u32) {
        let Some(Kind::Toplevel(t)) = st.objects.get(&toplevel).map(|o| &o.kind) else { return };
        let xdg = t.xdg;
        let Some(Kind::Xdg(x)) = st.objects.get(&xdg).map(|o| &o.kind) else { return };
        let root = x.surface;
        let Some(Kind::Surface(rs)) = st.objects.get(&root).map(|o| &o.kind) else { return };
        if rs.pixels.is_empty() {
            return;
        }
        let (lw, lh) = rs.logical();
        let (gx, gy, gw, gh) = x.geometry.unwrap_or((0, 0, lw, lh));
        let scale = rs.scale.max(1);
        let (fw, fh) = (gw.max(1) as u32 * scale, gh.max(1) as u32 * scale);
        let mut canvas = vec![0xff00_0000u32; (fw * fh) as usize];
        // The toplevel's tree, then popups over it (oldest first).
        draw_tree(st, root, (-gx, -gy), scale, &mut canvas, fw, fh);
        for &p in &st.popups {
            if self.toplevel_of_popup(st, p) == Some(toplevel)
                && let Some((surface, origin)) = self.popup_origin(st, p)
            {
                draw_tree(st, surface, (origin.0 - gx, origin.1 - gy), scale, &mut canvas, fw, fh);
            }
        }
        let frame = Frame { width: gw.max(1) as u32, height: gh.max(1) as u32, scale, pixels: Arc::new(canvas) };
        let Some(Kind::Toplevel(t)) = st.objects.get_mut(&toplevel).map(|o| &mut o.kind) else { return };
        let window = match t.window {
            Some(w) => w,
            None => {
                let w = comp.new_window(self, toplevel);
                t.window = Some(w);
                w
            }
        };
        if !t.opened {
            t.opened = true;
            comp.display.open(window, &t.title, frame.width, frame.height);
        }
        comp.display.present(window, frame);
    }

    fn toplevel_of_popup(&self, st: &State, popup: u32) -> Option<u32> {
        match st.objects.get(&popup).map(|o| &o.kind) {
            Some(Kind::Popup(p)) => self.toplevel_of(st, p.parent),
            _ => None,
        }
    }

    /// A popup's surface and where its surface's origin is, in its
    /// toplevel's surface coordinates.
    fn popup_origin(&self, st: &State, popup: u32) -> Option<(u32, (i32, i32))> {
        let Some(Kind::Popup(p)) = st.objects.get(&popup).map(|o| &o.kind) else { return None };
        let Some(Kind::Xdg(own)) = st.objects.get(&p.xdg).map(|o| &o.kind) else { return None };
        let Some(Kind::Xdg(parent)) = st.objects.get(&p.parent).map(|o| &o.kind) else { return None };
        // The parent's origin: its own popup's, or a toplevel's (0, 0).
        let parent_origin = match st.objects.get(&parent.role?).map(|o| &o.kind) {
            Some(Kind::Popup(_)) => self.popup_origin(st, parent.role?)?.1,
            _ => (0, 0),
        };
        let (pgx, pgy) = parent.geometry.map_or((0, 0), |g| (g.0, g.1));
        let (ogx, ogy) = own.geometry.map_or((0, 0), |g| (g.0, g.1));
        Some((own.surface, (parent_origin.0 + pgx + p.x - ogx, parent_origin.1 + pgy + p.y - ogy)))
    }

    /// Host input for this client's toplevel.
    pub fn input(&self, comp: &Compositor, toplevel: u32, input: Input) {
        let mut st = self.state.lock().unwrap();
        let Some(Kind::Toplevel(t)) = st.objects.get(&toplevel).map(|o| &o.kind) else { return };
        let xdg = t.xdg;
        let Some(Kind::Xdg(x)) = st.objects.get(&xdg).map(|o| &o.kind) else { return };
        let (root, geometry) = (x.surface, x.geometry);
        let (gx, gy) = geometry.map_or((0, 0), |g| (g.0, g.1));
        let time = comp.time();
        match input {
            Input::Motion { x, y } => {
                // Topmost first: popups, newest first, then the tree.
                let (px, py) = (x + gx as f64, y + gy as f64);
                let mut hit = None;
                for &p in st.popups.iter().rev() {
                    if self.toplevel_of_popup(&st, p) == Some(toplevel)
                        && let Some((surface, o)) = self.popup_origin(&st, p)
                        && let Some(h) = hit_tree(&st, surface, (o.0, o.1), px, py)
                    {
                        hit = Some(h);
                        break;
                    }
                }
                let hit = hit.or_else(|| hit_tree(&st, root, (0, 0), px, py));
                let Some((surface, sx, sy)) = hit else { return };
                let fixed = |v: f64| Value::Fixed((v * 256.0) as i32);
                if st.pointer_on != Some(surface) {
                    if let Some(old) = st.pointer_on {
                        for &p in &st.pointers {
                            self.send(p, "wl_pointer", "leave", vec![Value::Uint(comp.serial()), Value::Object(old)]);
                        }
                    }
                    st.pointer_on = Some(surface);
                    for &p in &st.pointers {
                        self.send(p, "wl_pointer", "enter", vec![Value::Uint(comp.serial()), Value::Object(surface), fixed(sx), fixed(sy)]);
                    }
                }
                for &p in &st.pointers {
                    self.send(p, "wl_pointer", "motion", vec![Value::Uint(time), fixed(sx), fixed(sy)]);
                }
                self.pointer_frame(&st);
            }
            Input::Leave => {
                if let Some(old) = st.pointer_on.take() {
                    for &p in &st.pointers {
                        self.send(p, "wl_pointer", "leave", vec![Value::Uint(comp.serial()), Value::Object(old)]);
                    }
                    self.pointer_frame(&st);
                }
            }
            Input::Button { button, pressed } => {
                if st.pointer_on.is_none() {
                    return;
                }
                for &p in &st.pointers {
                    self.send(p, "wl_pointer", "button", vec![Value::Uint(comp.serial()), Value::Uint(time), Value::Uint(button), Value::Uint(pressed as u32)]);
                }
                self.pointer_frame(&st);
            }
            Input::Axis { dx, dy } => {
                for &p in &st.pointers {
                    for (axis, v) in [(0u32, dy), (1, dx)] {
                        if v != 0.0 {
                            self.send(p, "wl_pointer", "axis", vec![Value::Uint(time), Value::Uint(axis), Value::Fixed((v * 256.0) as i32)]);
                        }
                    }
                }
                self.pointer_frame(&st);
            }
            Input::Key { code, pressed } => {
                if let Some(s) = st.keyboard_on
                    && st.objects.contains_key(&s)
                {
                    for &k in &st.keyboards {
                        self.send(k, "wl_keyboard", "key", vec![Value::Uint(comp.serial()), Value::Uint(time), Value::Uint(code), Value::Uint(pressed as u32)]);
                    }
                }
            }
            Input::Modifiers { depressed, locked } => {
                for &k in &st.keyboards {
                    self.send(k, "wl_keyboard", "modifiers", vec![Value::Uint(comp.serial()), Value::Uint(depressed), Value::Uint(0), Value::Uint(locked), Value::Uint(0)]);
                }
            }
            Input::Focus(on) => {
                if on {
                    st.keyboard_on = Some(root);
                    for &k in &st.keyboards {
                        self.send(k, "wl_keyboard", "enter", vec![Value::Uint(comp.serial()), Value::Object(root), Value::Array(Vec::new())]);
                    }
                } else if let Some(s) = st.keyboard_on.take() {
                    for &k in &st.keyboards {
                        self.send(k, "wl_keyboard", "leave", vec![Value::Uint(comp.serial()), Value::Object(s)]);
                    }
                    // Popups close when their window loses focus.
                    for p in st.popups.clone() {
                        if self.toplevel_of_popup(&st, p) == Some(toplevel) {
                            self.send(p, "xdg_popup", "popup_done", vec![]);
                        }
                    }
                }
                self.send(toplevel, "xdg_toplevel", "configure", vec![Value::Int(0), Value::Int(0), Value::Array(states(on))]);
                self.send(xdg, "xdg_surface", "configure", vec![Value::Uint(comp.serial())]);
            }
            Input::Resize { width, height } => {
                let (w, h) = (width as i32, height as i32);
                self.send(toplevel, "xdg_toplevel", "configure", vec![Value::Int(w), Value::Int(h), Value::Array(states(st.keyboard_on.is_some()))]);
                self.send(xdg, "xdg_surface", "configure", vec![Value::Uint(comp.serial())]);
            }
            Input::Close => self.send(toplevel, "xdg_toplevel", "close", vec![]),
        }
    }

    fn pointer_frame(&self, st: &State) {
        for &p in &st.pointers {
            if st.objects.get(&p).is_some_and(|o| o.version >= 5) {
                self.send(p, "wl_pointer", "frame", vec![]);
            }
        }
    }
}

/// xdg_toplevel.configure's states: activated when it has the keyboard.
fn states(active: bool) -> Vec<u8> {
    if active { 4u32.to_le_bytes().to_vec() } else { Vec::new() }
}

/// A popup's position relative to its parent's window geometry: its
/// anchor point on the anchor rectangle, then placed that way by its
/// gravity, then offset. (Constraints aren't applied.)
fn place(p: &Positioner) -> (i32, i32) {
    let (rx, ry, rw, rh) = p.rect;
    let (ax, ay) = match p.anchor {
        1 => (rx + rw / 2, ry),
        2 => (rx + rw / 2, ry + rh),
        3 => (rx, ry + rh / 2),
        4 => (rx + rw, ry + rh / 2),
        5 => (rx, ry),
        6 => (rx, ry + rh),
        7 => (rx + rw, ry),
        8 => (rx + rw, ry + rh),
        _ => (rx + rw / 2, ry + rh / 2),
    };
    let (w, h) = p.size;
    let (x, y) = match p.gravity {
        1 => (ax - w / 2, ay - h),
        2 => (ax - w / 2, ay),
        3 => (ax - w, ay - h / 2),
        4 => (ax, ay - h / 2),
        5 => (ax - w, ay - h),
        6 => (ax - w, ay),
        7 => (ax, ay - h),
        8 => (ax, ay),
        _ => (ax - w / 2, ay - h / 2),
    };
    (x + p.offset.0, y + p.offset.1)
}

/// A keymap file to pass: the built-in keymap, NUL-terminated, in a memfd.
fn keymap() -> Option<Passed> {
    #[cfg(unix)]
    {
        use std::io::Write;
        let fd = crate::shared::memfd(c"wasm-env-keymap", 0).ok()?;
        let mut file = std::fs::File::from(fd);
        file.write_all(KEYMAP.as_bytes()).ok()?;
        file.write_all(&[0]).ok()?;
        let open = Open::File { file, path: "/memfd:wasm-env-keymap".into(), append: false };
        Some(Passed { open: Arc::new(Mutex::new(open)), flags: 0 })
    }
    #[cfg(not(unix))]
    None
}

/// A buffer's pixels, read from its pool's file: (width, height, pixels
/// as 0xAARRGGBB; XRGB8888 made opaque).
fn read_buffer(b: &Buffer) -> (u32, u32, Vec<u32>) {
    let (w, h) = (b.width.max(0) as usize, b.height.max(0) as usize);
    let stride = (b.stride.max(0) as usize).max(w * 4);
    let mut bytes = vec![0u8; stride * h];
    // Where shared mappings are copies (shared::host_mappings), the client
    // drew into its copy: read from there.
    if crate::shared::read_copy(&b.pool.open, b.offset as u64, &mut bytes) {
    } else if let Open::File { file, .. } = &*b.pool.open.lock().unwrap() {
        let mut done = 0;
        while done < bytes.len() {
            #[cfg(unix)]
            let r = std::os::unix::fs::FileExt::read_at(file, &mut bytes[done..], b.offset as u64 + done as u64);
            #[cfg(windows)]
            let r = std::os::windows::fs::FileExt::seek_read(file, &mut bytes[done..], b.offset as u64 + done as u64);
            match r {
                Ok(0) | Err(_) => break,
                Ok(n) => done += n,
            }
        }
    }
    let opaque = if b.format == 1 { 0xff00_0000 } else { 0 };
    let mut pixels = Vec::with_capacity(w * h);
    for row in bytes.chunks_exact(stride).take(h) {
        pixels.extend(row[..w * 4].chunks_exact(4).map(|p| u32::from_le_bytes(p.try_into().unwrap()) | opaque));
    }
    (w as u32, h as u32, pixels)
}

/// Draws a surface and its subsurfaces (above it, in order) onto the
/// canvas, the surface's origin at `origin` (logical, canvas coordinates).
fn draw_tree(st: &State, id: u32, origin: (i32, i32), scale: u32, canvas: &mut [u32], cw: u32, ch: u32) {
    let Some(Kind::Surface(s)) = st.objects.get(&id).map(|o| &o.kind) else { return };
    blit(s, origin, scale, canvas, cw, ch);
    for &c in &s.children {
        if let Some(Kind::Surface(child)) = st.objects.get(&c).map(|o| &o.kind) {
            draw_tree(st, c, (origin.0 + child.position.0, origin.1 + child.position.1), scale, canvas, cw, ch);
        }
    }
}

/// Blends a surface's pixels (premultiplied alpha) over the canvas,
/// scaled from its buffer scale to the canvas's.
fn blit(s: &Surface, origin: (i32, i32), scale: u32, canvas: &mut [u32], cw: u32, ch: u32) {
    if s.pixels.is_empty() {
        return;
    }
    let (lw, lh) = s.logical();
    let ss = s.scale.max(1);
    let (x0, y0) = (origin.0 * scale as i32, origin.1 * scale as i32);
    let (w, h) = (lw * scale as i32, lh * scale as i32);
    for y in 0..h {
        let cy = y0 + y;
        if cy < 0 || cy >= ch as i32 {
            continue;
        }
        let sy = (y as u32 * ss / scale) as usize;
        for x in 0..w {
            let cx = x0 + x;
            if cx < 0 || cx >= cw as i32 {
                continue;
            }
            let sx = (x as u32 * ss / scale) as usize;
            let Some(&src) = s.pixels.get(sy * s.width as usize + sx) else { continue };
            let dst = &mut canvas[(cy as u32 * cw + cx as u32) as usize];
            let a = src >> 24;
            *dst = if a == 0xff {
                src
            } else if a == 0 && src & 0xffffff == 0 {
                *dst
            } else {
                // Premultiplied: src + dst * (1 - a), per channel.
                let blend = |shift: u32| {
                    let (sc, dc) = ((src >> shift) & 0xff, (*dst >> shift) & 0xff);
                    (sc + dc * (255 - a) / 255).min(255) << shift
                };
                0xff00_0000 | blend(16) | blend(8) | blend(0)
            };
        }
    }
}

/// The topmost surface of a tree under a point (logical, in the tree's
/// coordinates, its root's origin at `origin`), and where on it.
fn hit_tree(st: &State, id: u32, origin: (i32, i32), x: f64, y: f64) -> Option<(u32, f64, f64)> {
    let Some(Kind::Surface(s)) = st.objects.get(&id).map(|o| &o.kind) else { return None };
    for &c in s.children.iter().rev() {
        if let Some(Kind::Surface(child)) = st.objects.get(&c).map(|o| &o.kind)
            && let Some(h) = hit_tree(st, c, (origin.0 + child.position.0, origin.1 + child.position.1), x, y)
        {
            return Some(h);
        }
    }
    let (w, h) = s.logical();
    let (sx, sy) = (x - origin.0 as f64, y - origin.1 as f64);
    (sx >= 0.0 && sy >= 0.0 && sx < w as f64 && sy < h as f64).then_some((id, sx, sy))
}
