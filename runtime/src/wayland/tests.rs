//! The compositor with a client written by hand, over the runtime's Unix
//! sockets: no host display, no host Wayland.

use std::sync::Arc;

use super::protocol::{Value, encode};
use super::*;
use crate::unix::{Name, Names, UnixSocket};

fn never() -> bool {
    false
}

struct TestClient {
    sock: Arc<UnixSocket>,
    names: Arc<Names>,
    buf: Vec<u8>,
}

impl TestClient {
    fn send(&self, id: u32, opcode: u16, args: Vec<Value>) {
        let (b, fds) = encode(id, opcode, args);
        self.sock.send(&self.names, &b, fds, None, false, &never).unwrap();
    }

    /// Reads events until one on `id` with `opcode`: all read, as (object,
    /// opcode, body).
    fn until(&mut self, id: u32, opcode: u16) -> Vec<(u32, u16, Vec<u8>)> {
        let mut out = Vec::new();
        loop {
            while self.buf.len() >= 8 {
                let object = u32::from_le_bytes(self.buf[..4].try_into().unwrap());
                let word = u32::from_le_bytes(self.buf[4..8].try_into().unwrap());
                let size = (word >> 16) as usize;
                if self.buf.len() < size {
                    break;
                }
                let body = self.buf[8..size].to_vec();
                self.buf.drain(..size);
                let done = object == id && (word & 0xffff) as u16 == opcode;
                out.push((object, (word & 0xffff) as u16, body));
                if done {
                    return out;
                }
            }
            let got = self.sock.recv(4096, false, false, false, &never).unwrap();
            assert!(!got.data.is_empty(), "the compositor hung up");
            self.buf.extend_from_slice(&got.data);
        }
    }
}

fn connect() -> (Arc<Headless>, Arc<Compositor>, TestClient) {
    let names = Arc::new(Names::default());
    let headless = Arc::new(Headless::default());
    let comp = Compositor::start(&names, headless.clone());
    let sock = UnixSocket::new(crate::unix::SOCK_STREAM);
    sock.connect(&names, &Name::Path(SOCKET.into()), 1, false, &never).unwrap();
    (headless, comp, TestClient { sock, names, buf: Vec::new() })
}

#[test]
fn a_client_sees_the_globals() {
    let (_h, _comp, mut c) = connect();
    c.send(1, 1, vec![Value::NewId(2)]); // wl_display.get_registry
    c.send(1, 0, vec![Value::NewId(3)]); // wl_display.sync
    let events = c.until(3, 0);
    let globals = events.iter().filter(|e| e.0 == 2).count();
    assert_eq!(globals, 9);
}

#[cfg(unix)] // (a pool in a temporary file: shared::anonymous_file)
#[test]
fn a_toplevel_shows_its_buffer() {
    use crate::unix::Passed;
    use std::sync::atomic::Ordering;
    let (headless, _comp, mut c) = connect();
    c.send(1, 1, vec![Value::NewId(2)]);
    // Bind wl_compositor (1), wl_shm (3), xdg_wm_base (6).
    for (name, iface, version, id) in [(1u32, "wl_compositor", 4u32, 10u32), (3, "wl_shm", 1, 11), (6, "xdg_wm_base", 1, 12)] {
        c.send(2, 0, vec![Value::Uint(name), Value::NewIdOf { interface: iface.into(), version, id }]);
    }
    c.send(10, 0, vec![Value::NewId(20)]); // create_surface
    c.send(12, 2, vec![Value::NewId(21), Value::Object(20)]); // get_xdg_surface
    c.send(21, 1, vec![Value::NewId(22)]); // get_toplevel
    c.send(20, 6, vec![]); // commit
    let events = c.until(21, 0); // xdg_surface.configure
    let serial = u32::from_le_bytes(events.last().unwrap().2[..4].try_into().unwrap());
    c.send(21, 4, vec![Value::Uint(serial)]); // ack_configure
    // A 4x4 buffer, in a file passed to the compositor.
    let file = crate::shared::anonymous_file().unwrap();
    let mut file = std::fs::File::from(file);
    std::io::Write::write_all(&mut file, &[0x80u8; 64]).unwrap();
    let open = Arc::new(std::sync::Mutex::new(crate::fs::Open::File { file, path: "pool".into(), append: false }));
    c.send(11, 0, vec![Value::NewId(30), Value::Fd(Passed { open, flags: 0 }), Value::Int(64)]); // create_pool
    c.send(30, 0, vec![Value::NewId(31), Value::Int(0), Value::Int(4), Value::Int(4), Value::Int(16), Value::Uint(1)]); // create_buffer
    c.send(20, 1, vec![Value::Object(31), Value::Int(0), Value::Int(0)]); // attach
    c.send(20, 3, vec![Value::NewId(40)]); // frame
    c.send(20, 6, vec![]); // commit
    c.until(31, 0); // wl_buffer.release
    c.until(40, 0); // wl_callback.done: the frame was shown
    assert!(headless.frames.load(Ordering::Relaxed) >= 1);
}
