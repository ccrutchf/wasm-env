//! The Wayland protocols the compositor speaks, read from their XML
//! descriptions (runtime/protocols, embedded), and the wire format: each
//! message a header (object id; size << 16 | opcode) and arguments in
//! 32-bit words, strings and arrays padded to 4 bytes, descriptors passed
//! alongside (SCM_RIGHTS).

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::unix::Passed;

const XML: &[&str] = &[
    include_str!("../../protocols/wayland.xml"),
    include_str!("../../protocols/xdg-shell.xml"),
    include_str!("../../protocols/viewporter.xml"),
    include_str!("../../protocols/xdg-decoration-unstable-v1.xml"),
    include_str!("../../protocols/xdg-output-unstable-v1.xml"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int,
    Uint,
    Fixed,
    Str,
    Object,
    NewId,
    Array,
    Fd,
}

#[derive(Debug)]
pub struct Arg {
    #[allow(dead_code)] // (for Debug output)
    pub name: String,
    pub kind: Kind,
    /// For objects and new ids: their interface (a new id without one, as
    /// in wl_registry.bind, carries the interface and version itself).
    pub interface: Option<String>,
}

#[derive(Debug)]
pub struct Message {
    pub name: String,
    pub args: Vec<Arg>,
}

#[derive(Debug)]
pub struct Interface {
    pub name: String,
    pub version: u32,
    pub requests: Vec<Message>,
    pub events: Vec<Message>,
}

impl Interface {
    /// An event's opcode by name.
    pub fn event(&self, name: &str) -> u16 {
        self.events.iter().position(|m| m.name == name).unwrap_or_else(|| panic!("no event {}.{name}", self.name)) as u16
    }
}

/// Every interface, by name.
pub static INTERFACES: LazyLock<HashMap<String, Interface>> = LazyLock::new(|| {
    let mut all = HashMap::new();
    for xml in XML {
        for i in parse(xml) {
            all.insert(i.name.clone(), i);
        }
    }
    all
});

pub fn interface(name: &str) -> &'static Interface {
    INTERFACES.get(name).unwrap_or_else(|| panic!("no interface {name}"))
}

fn parse(xml: &str) -> Vec<Interface> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out: Vec<Interface> = Vec::new();
    // Whether the message being read is a request (or an event).
    let mut in_request = false;
    let attr = |e: &quick_xml::events::BytesStart, key: &str| -> Option<String> {
        e.attributes().flatten().find(|a| a.key.as_ref() == key.as_bytes()).map(|a| String::from_utf8_lossy(&a.value).into_owned())
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"interface" => out.push(Interface {
                    name: attr(&e, "name").unwrap_or_default(),
                    version: attr(&e, "version").and_then(|v| v.parse().ok()).unwrap_or(1),
                    requests: Vec::new(),
                    events: Vec::new(),
                }),
                b"request" | b"event" => {
                    in_request = e.name().as_ref() == b"request";
                    let m = Message { name: attr(&e, "name").unwrap_or_default(), args: Vec::new() };
                    let i = out.last_mut().expect("a message inside an interface");
                    if in_request { i.requests.push(m) } else { i.events.push(m) }
                }
                b"arg" => {
                    let kind = match attr(&e, "type").as_deref() {
                        Some("int") => Kind::Int,
                        Some("uint") => Kind::Uint,
                        Some("fixed") => Kind::Fixed,
                        Some("string") => Kind::Str,
                        Some("object") => Kind::Object,
                        Some("new_id") => Kind::NewId,
                        Some("array") => Kind::Array,
                        Some("fd") => Kind::Fd,
                        other => panic!("argument type {other:?}"),
                    };
                    let arg = Arg { name: attr(&e, "name").unwrap_or_default(), kind, interface: attr(&e, "interface") };
                    let i = out.last_mut().expect("an argument inside an interface");
                    let m = if in_request { i.requests.last_mut() } else { i.events.last_mut() };
                    m.expect("an argument inside a message").args.push(arg);
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => panic!("protocol XML: {e}"),
            _ => {}
        }
    }
    out
}

/// An argument's value.
#[derive(Debug)]
pub enum Value {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    Str(Option<String>),
    Object(u32),
    NewId(u32),
    /// A new id of an interface the message doesn't name (wl_registry.bind).
    NewIdOf { interface: String, version: u32, id: u32 },
    Array(Vec<u8>),
    Fd(Passed),
}

impl Value {
    pub fn uint(&self) -> u32 {
        match self {
            Value::Uint(v) | Value::Object(v) | Value::NewId(v) => *v,
            Value::Int(v) | Value::Fixed(v) => *v as u32,
            _ => 0,
        }
    }

    pub fn int(&self) -> i32 {
        self.uint() as i32
    }

    pub fn string(&self) -> String {
        match self {
            Value::Str(Some(s)) => s.clone(),
            _ => String::new(),
        }
    }
}

/// Decodes a message's arguments (the bytes after its header), taking
/// descriptors from `fds`. None if it is malformed.
pub fn decode(m: &Message, mut b: &[u8], fds: &mut std::collections::VecDeque<Passed>) -> Option<Vec<Value>> {
    fn word(b: &mut &[u8]) -> Option<u32> {
        let (w, rest) = b.split_first_chunk::<4>()?;
        *b = rest;
        Some(u32::from_le_bytes(*w))
    }
    fn bytes(b: &mut &[u8]) -> Option<Vec<u8>> {
        let len = word(b)? as usize;
        let padded = len.next_multiple_of(4);
        if b.len() < padded {
            return None;
        }
        let v = b[..len].to_vec();
        *b = &b[padded..];
        Some(v)
    }
    fn string(b: &mut &[u8]) -> Option<Option<String>> {
        let raw = bytes(b)?;
        if raw.is_empty() {
            return Some(None);
        }
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        Some(Some(String::from_utf8_lossy(&raw[..end]).into_owned()))
    }
    let mut out = Vec::with_capacity(m.args.len());
    for a in &m.args {
        out.push(match a.kind {
            Kind::Int => Value::Int(word(&mut b)? as i32),
            Kind::Uint => Value::Uint(word(&mut b)?),
            Kind::Fixed => Value::Fixed(word(&mut b)? as i32),
            Kind::Str => Value::Str(string(&mut b)?),
            Kind::Object => Value::Object(word(&mut b)?),
            Kind::NewId if a.interface.is_none() => {
                let interface = string(&mut b)?.unwrap_or_default();
                let version = word(&mut b)?;
                Value::NewIdOf { interface, version, id: word(&mut b)? }
            }
            Kind::NewId => Value::NewId(word(&mut b)?),
            Kind::Array => Value::Array(bytes(&mut b)?),
            Kind::Fd => Value::Fd(fds.pop_front()?),
        });
    }
    Some(out)
}

/// Encodes a message: its bytes, and the descriptors to pass with them.
pub fn encode(id: u32, opcode: u16, args: Vec<Value>) -> (Vec<u8>, Vec<Passed>) {
    let mut b = vec![0u8; 8];
    let mut fds = Vec::new();
    let put_bytes = |b: &mut Vec<u8>, data: &[u8]| {
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        b.resize(b.len().next_multiple_of(4), 0);
    };
    for a in args {
        match a {
            Value::Int(v) | Value::Fixed(v) => b.extend_from_slice(&v.to_le_bytes()),
            Value::Uint(v) | Value::Object(v) | Value::NewId(v) => b.extend_from_slice(&v.to_le_bytes()),
            Value::Str(None) => b.extend_from_slice(&0u32.to_le_bytes()),
            Value::Str(Some(s)) => {
                let mut z = s.into_bytes();
                z.push(0);
                put_bytes(&mut b, &z);
            }
            Value::NewIdOf { interface, version, id } => {
                let mut z = interface.into_bytes();
                z.push(0);
                put_bytes(&mut b, &z);
                b.extend_from_slice(&version.to_le_bytes());
                b.extend_from_slice(&id.to_le_bytes());
            }
            Value::Array(v) => put_bytes(&mut b, &v),
            Value::Fd(p) => fds.push(p),
        }
    }
    let size = b.len() as u32;
    b[..4].copy_from_slice(&id.to_le_bytes());
    b[4..8].copy_from_slice(&((size << 16) | opcode as u32).to_le_bytes());
    (b, fds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocols_load() {
        let registry = interface("wl_registry");
        let bind = &registry.requests[0];
        assert_eq!(bind.name, "bind");
        assert_eq!(bind.args[1].kind, Kind::NewId);
        assert!(bind.args[1].interface.is_none(), "bind names its interface on the wire");
        assert!(interface("xdg_wm_base").version >= 5);
        assert_eq!(interface("wl_surface").event("enter"), 0);
    }

    #[test]
    fn messages_round_trip() {
        let (b, fds) = encode(3, 0, vec![Value::Uint(7), Value::NewIdOf { interface: "wl_compositor".into(), version: 4, id: 9 }]);
        assert!(fds.is_empty());
        assert_eq!(u32::from_le_bytes(b[..4].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()) >> 16, b.len() as u32);
        let bind = &interface("wl_registry").requests[0];
        let args = decode(bind, &b[8..], &mut Default::default()).unwrap();
        assert_eq!(args[0].uint(), 7);
        match &args[1] {
            Value::NewIdOf { interface, version, id } => assert_eq!((interface.as_str(), *version, *id), ("wl_compositor", 4, 9)),
            other => panic!("{other:?}"),
        }
    }
}
