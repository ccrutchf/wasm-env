//! The runtime's own PulseAudio server (docs/abi.md, "Audio"): programs that
//! speak PulseAudio's native protocol (through the environment's own
//! `libpulse`) are clients of the runtime, as Wayland programs are of its
//! compositor. Its one sink and one source are the sound card's (audio.rs):
//! each stream a client makes is a stream of the card's, mixed with ALSA
//! programs' and played (or captured) on the host through `cpal`, the
//! card's master volume its sink's. Nothing of the host's sound server is
//! needed.
//!
//! It speaks protocol version 35 (PulseAudio 15 and later) without shared
//! memory: audio comes and goes inline, in the connection's frames. Each
//! connection has a thread that reads its requests, and a clock that moves
//! its streams on (requests for more, data captured, drains finished)
//! every 10 ms.

pub mod convert;
pub mod tagstruct;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use crate::audio::Pcm;
use crate::unix::{Name, Names, UnixSocket};
use convert::Conversion;
use tagstruct::{Proplist, Reader, SampleSpec, Writer, prop, string};

/// Where clients find the server: libpulse tries the user's runtime
/// directory, then the system-wide socket.
pub const SOCKETS: [&str; 2] = ["/run/user/0/pulse/native", "/run/pulse/native"];

const VERSION: u32 = 35;
const INVALID: u32 = 0xffff_ffff;
const PACKET: u32 = 0xffff_ffff;
const NORM: u32 = 0x10000;

// Commands (pulsecore/native-common.h).
const ERROR: u32 = 0;
const REPLY: u32 = 2;
const CREATE_PLAYBACK_STREAM: u32 = 3;
const DELETE_PLAYBACK_STREAM: u32 = 4;
const CREATE_RECORD_STREAM: u32 = 5;
const DELETE_RECORD_STREAM: u32 = 6;
const AUTH: u32 = 8;
const SET_CLIENT_NAME: u32 = 9;
const LOOKUP_SINK: u32 = 10;
const LOOKUP_SOURCE: u32 = 11;
const DRAIN_PLAYBACK_STREAM: u32 = 12;
const STAT: u32 = 13;
const GET_PLAYBACK_LATENCY: u32 = 14;
const GET_SERVER_INFO: u32 = 20;
const GET_SINK_INFO: u32 = 21;
const GET_SINK_INFO_LIST: u32 = 22;
const GET_SOURCE_INFO: u32 = 23;
const GET_SOURCE_INFO_LIST: u32 = 24;
const GET_MODULE_INFO_LIST: u32 = 26;
const GET_CLIENT_INFO: u32 = 27;
const GET_CLIENT_INFO_LIST: u32 = 28;
const GET_SINK_INPUT_INFO: u32 = 29;
const GET_SINK_INPUT_INFO_LIST: u32 = 30;
const GET_SOURCE_OUTPUT_INFO: u32 = 31;
const GET_SOURCE_OUTPUT_INFO_LIST: u32 = 32;
const GET_SAMPLE_INFO_LIST: u32 = 34;
const SUBSCRIBE: u32 = 35;
const SET_SINK_VOLUME: u32 = 36;
const SET_SINK_INPUT_VOLUME: u32 = 37;
const SET_SOURCE_VOLUME: u32 = 38;
const SET_SINK_MUTE: u32 = 39;
const SET_SOURCE_MUTE: u32 = 40;
const CORK_PLAYBACK_STREAM: u32 = 41;
const FLUSH_PLAYBACK_STREAM: u32 = 42;
const TRIGGER_PLAYBACK_STREAM: u32 = 43;
const SET_DEFAULT_SINK: u32 = 44;
const SET_DEFAULT_SOURCE: u32 = 45;
const SET_PLAYBACK_STREAM_NAME: u32 = 46;
const SET_RECORD_STREAM_NAME: u32 = 47;
const GET_RECORD_LATENCY: u32 = 57;
const CORK_RECORD_STREAM: u32 = 58;
const FLUSH_RECORD_STREAM: u32 = 59;
const PREBUF_PLAYBACK_STREAM: u32 = 60;
const REQUEST: u32 = 61;
const OVERFLOW: u32 = 62;
const UNDERFLOW: u32 = 63;
const SUBSCRIBE_EVENT: u32 = 66;
const MOVE_SINK_INPUT: u32 = 67;
const MOVE_SOURCE_OUTPUT: u32 = 68;
const SET_SINK_INPUT_MUTE: u32 = 69;
const SUSPEND_SINK: u32 = 70;
const SUSPEND_SOURCE: u32 = 71;
const SET_PLAYBACK_STREAM_BUFFER_ATTR: u32 = 72;
const SET_RECORD_STREAM_BUFFER_ATTR: u32 = 73;
const UPDATE_PLAYBACK_STREAM_SAMPLE_RATE: u32 = 74;
const UPDATE_RECORD_STREAM_SAMPLE_RATE: u32 = 75;
const UPDATE_RECORD_STREAM_PROPLIST: u32 = 80;
const UPDATE_PLAYBACK_STREAM_PROPLIST: u32 = 81;
const UPDATE_CLIENT_PROPLIST: u32 = 82;
const REMOVE_RECORD_STREAM_PROPLIST: u32 = 83;
const REMOVE_PLAYBACK_STREAM_PROPLIST: u32 = 84;
const REMOVE_CLIENT_PROPLIST: u32 = 85;
const STARTED: u32 = 86;
const EXTENSION: u32 = 87;
const GET_CARD_INFO: u32 = 88;
const GET_CARD_INFO_LIST: u32 = 89;
const ENABLE_SRBCHANNEL: u32 = 101;
const REGISTER_MEMFD_SHMID: u32 = 103;

// Errors (pa_error_code_t).
const ERR_ACCESS: u32 = 1;
const ERR_INVALID: u32 = 3;
const ERR_NOENTITY: u32 = 5;
const ERR_NOTSUPPORTED: u32 = 19;
const ERR_NOEXTENSION: u32 = 21;

// Subscription facilities and event types.
const FAC_SINK: u32 = 0;
const FAC_SOURCE: u32 = 1;
const FAC_SINK_INPUT: u32 = 2;
const FAC_SOURCE_OUTPUT: u32 = 3;
const FAC_CLIENT: u32 = 5;
const EV_NEW: u32 = 0x00;
const EV_CHANGE: u32 = 0x10;
const EV_REMOVE: u32 = 0x20;

/// The sink and the source: the card's output and input.
const SINK: &str = "wasm-env-output";
const SOURCE: &str = "wasm-env-input";
/// What the card is taken to be: float stereo at 48 kHz.
const DEVICE_SPEC: SampleSpec = SampleSpec { format: convert::FLOAT32LE, channels: 2, rate: 48000 };
/// The latency the device is said to add (the card's mix, the host's
/// output), in microseconds.
const DEVICE_LATENCY: u64 = 20_000;

/// A connection that needs no names to send.
static NO_NAMES: LazyLock<Names> = LazyLock::new(Names::default);

/// The server: its clients and their streams (by their global indices), and
/// the source's volume (the sink's is the card's master volume).
struct Server {
    next_client: AtomicU32,
    next_input: AtomicU32,
    next_output: AtomicU32,
    conns: Mutex<Vec<Weak<Conn>>>,
    inputs: Mutex<BTreeMap<u32, Arc<Play>>>,
    outputs: Mutex<BTreeMap<u32, Arc<Record>>>,
    source_volume: Mutex<([u32; 2], bool)>,
}

static SERVER: LazyLock<Server> = LazyLock::new(|| Server {
    next_client: AtomicU32::new(0),
    next_input: AtomicU32::new(0),
    next_output: AtomicU32::new(0),
    conns: Mutex::new(Vec::new()),
    inputs: Mutex::new(BTreeMap::new()),
    outputs: Mutex::new(BTreeMap::new()),
    source_volume: Mutex::new(([NORM; 2], false)),
});

/// Starts the server: clients connecting to SOCKETS in `names` are served.
/// `root` is the environment's, where the sockets' files go.
pub fn start(names: &Names, root: &std::path::Path) {
    for path in SOCKETS {
        names.serve(Name::Path(path.to_string()), Arc::new(|sock: Arc<UnixSocket>| {
            let _ = std::thread::Builder::new().name("pulse client".into()).spawn(move || serve(sock));
        }));
        // Its file, as a server's socket would have, where the environment
        // has the directory it goes in (/run, /run/user/0): nothing made
        // in a root without them.
        let host = root.join(path.trim_start_matches('/'));
        if let Some(dir) = host.parent()
            && dir.parent().is_some_and(|d| d.is_dir())
        {
            // (Private, as libpulse wants its runtime directory.)
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            let _ = builder.create(dir);
        }
        let _ = std::fs::File::create(host);
    }
}

// ---- Volumes ----

/// A PulseAudio volume as a gain (its volumes are cubic).
fn linear(v: u32) -> f32 {
    (v as f32 / NORM as f32).powi(3)
}

/// The card's master volume step (0 to 100: half decibels from -50 dB) as
/// a PulseAudio volume, and back.
fn volume_of_step(step: i64) -> u32 {
    if step <= 0 {
        return 0;
    }
    let db = (step - 100) as f64 / 2.0;
    (NORM as f64 * 10f64.powf(db / 60.0)).round() as u32
}

fn step_of_volume(v: u32) -> i64 {
    if v == 0 {
        return 0;
    }
    let db = 60.0 * (v as f64 / NORM as f64).log10();
    (100.0 + 2.0 * db).round().clamp(0.0, 100.0) as i64
}

/// A stream's gains (left, right) for its volume (per channel of its map).
fn gains(volume: &[u32], map: &[u8], muted: bool) -> [f32; 2] {
    if muted || volume.is_empty() {
        return [0.0; 2];
    }
    if volume.len() == 2 && map == [convert::FRONT_LEFT, convert::FRONT_RIGHT] {
        return [linear(volume[0]), linear(volume[1])];
    }
    let avg = volume.iter().map(|&v| v as u64).sum::<u64>() / volume.len() as u64;
    [linear(avg as u32); 2]
}

// ---- Connections ----

struct Conn {
    sock: Arc<UnixSocket>,
    index: u32,
    props: Mutex<Proplist>,
    /// Frames go out whole, one at a time.
    sending: Mutex<()>,
    subscribed: AtomicU32,
    closed: AtomicBool,
    streams: Mutex<Streams>,
}

#[derive(Default)]
struct Streams {
    plays: HashMap<u32, Arc<Play>>,
    records: HashMap<u32, Arc<Record>>,
    next_play: u32,
    next_record: u32,
}

/// A playback stream (a sink input).
struct Play {
    channel: u32,
    index: u32,
    client: u32,
    spec: SampleSpec,
    map: Vec<u8>,
    conv: Conversion,
    pcm: Arc<Pcm>,
    attr: Mutex<PlayAttr>,
    state: Mutex<PlayState>,
    volume: Mutex<(Vec<u32>, bool)>,
    props: Mutex<Proplist>,
}

#[derive(Clone, Copy)]
struct PlayAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
}

/// Where the client and the card are (bytes of the client's), and what
/// the client has been asked for (PulseAudio's accounting).
#[derive(Default)]
struct PlayState {
    written: i64,
    read: i64,
    missing: i64,
    requested: i64,
    corked: bool,
    playing: bool,
    underruns: u64,
    drain: Option<u32>,
    played_since: i64,
}

/// A record stream (a source output).
struct Record {
    channel: u32,
    index: u32,
    client: u32,
    spec: SampleSpec,
    map: Vec<u8>,
    conv: Conversion,
    pcm: Arc<Pcm>,
    attr: Mutex<(u32, u32)>,
    state: Mutex<RecordState>,
    volume: Mutex<(Vec<u32>, bool)>,
    props: Mutex<Proplist>,
}

#[derive(Default)]
struct RecordState {
    sent: i64,
    corked: bool,
}

/// Serves one client until it goes.
fn serve(sock: Arc<UnixSocket>) {
    let conn = Arc::new(Conn {
        sock: sock.clone(),
        index: SERVER.next_client.fetch_add(1, Ordering::Relaxed),
        props: Mutex::new(Vec::new()),
        sending: Mutex::new(()),
        subscribed: AtomicU32::new(0),
        closed: AtomicBool::new(false),
        streams: Mutex::new(Streams::default()),
    });
    SERVER.conns.lock().unwrap().push(Arc::downgrade(&conn));
    let clock = Arc::downgrade(&conn);
    let _ = std::thread::Builder::new().name("pulse clock".into()).spawn(move || {
        while let Some(c) = clock.upgrade() {
            if c.closed.load(Ordering::Relaxed) {
                break;
            }
            c.tick();
            drop(c);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let mut buf: Vec<u8> = Vec::new();
    'conn: loop {
        let Ok(got) = sock.recv(1 << 16, false, false, false, &|| false) else { break };
        if got.data.is_empty() {
            break;
        }
        buf.extend_from_slice(&got.data);
        let mut at = 0;
        while buf.len() - at >= 20 {
            let d: Vec<u32> = buf[at..at + 20].chunks(4).map(|w| u32::from_be_bytes(w.try_into().unwrap())).collect();
            let (len, channel, offset, flags) = (d[0] as usize, d[1], ((d[2] as u64) << 32 | d[3] as u64) as i64, d[4]);
            if len == 0 || len > 16 << 20 {
                break 'conn;
            }
            if buf.len() - at < 20 + len {
                break;
            }
            let payload = buf[at + 20..at + 20 + len].to_vec();
            at += 20 + len;
            let ok = if channel == PACKET { flags == 0 && conn.packet(&payload) } else { flags & 0xffff_ff00 == 0 && conn.data(channel, offset, &payload) };
            if !ok {
                break 'conn;
            }
        }
        buf.drain(..at);
    }
    conn.close();
}

impl Conn {
    /// Sends a frame (a packet's, or a stream's data on `channel`).
    fn frame(&self, channel: u32, payload: &[u8]) {
        let mut b = Vec::with_capacity(20 + payload.len());
        for w in [payload.len() as u32, channel, 0, 0, 0] {
            b.extend_from_slice(&w.to_be_bytes());
        }
        b.extend_from_slice(payload);
        let _g = self.sending.lock().unwrap();
        let mut at = 0;
        while at < b.len() {
            match self.sock.send(&NO_NAMES, &b[at..], Vec::new(), None, false, &|| false) {
                Ok(n) if n > 0 => at += n,
                _ => return,
            }
        }
    }

    fn send(&self, command: u32, tag: u32, body: &Writer) {
        let mut w = Writer::new();
        w.u32(command).u32(tag);
        w.bytes.extend_from_slice(&body.bytes);
        self.frame(PACKET, &w.bytes);
    }

    fn reply(&self, tag: u32, body: &Writer) {
        self.send(REPLY, tag, body);
    }

    fn ack(&self, tag: u32) {
        self.reply(tag, &Writer::new());
    }

    fn error(&self, tag: u32, code: u32) {
        let mut w = Writer::new();
        w.u32(code);
        self.send(ERROR, tag, &w);
    }

    /// A packet from the client: false if it doesn't make sense (the
    /// connection ends, as PulseAudio's does).
    fn packet(self: &Arc<Self>, b: &[u8]) -> bool {
        let mut r = Reader::new(b);
        let (Some(command), Some(tag)) = (r.u32(), r.u32()) else { return false };
        if debug() {
            eprintln!("wasm-env: pulse: client {} command {command} tag {tag}", self.index);
        }
        match self.command(command, tag, &mut r) {
            Some(()) => true,
            None => {
                if std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some() {
                    eprintln!("wasm-env: pulse: command {command} malformed");
                }
                false
            }
        }
    }

    /// A command: None if its fields aren't what it takes.
    fn command(self: &Arc<Self>, command: u32, tag: u32, r: &mut Reader) -> Option<()> {
        let end = |r: &Reader| r.eof().then_some(());
        match command {
            AUTH => {
                let version = r.u32()? & 0xffff;
                r.arbitrary()?;
                end(r)?;
                if version < 8 {
                    self.error(tag, 17);
                } else {
                    // (No shared memory, no memfd: both flags clear.)
                    let mut w = Writer::new();
                    w.u32(VERSION);
                    self.reply(tag, &w);
                }
            }
            SET_CLIENT_NAME => {
                let p = r.proplist()?;
                end(r)?;
                *self.props.lock().unwrap() = p;
                let mut w = Writer::new();
                w.u32(self.index);
                self.reply(tag, &w);
                broadcast(FAC_CLIENT, EV_NEW, self.index);
            }
            UPDATE_CLIENT_PROPLIST => {
                let mode = r.u32()?;
                let p = r.proplist()?;
                end(r)?;
                update_props(&mut self.props.lock().unwrap(), mode, p);
                self.ack(tag);
                broadcast(FAC_CLIENT, EV_CHANGE, self.index);
            }
            REMOVE_CLIENT_PROPLIST => {
                let keys = keys(r)?;
                self.props.lock().unwrap().retain(|(k, _)| !keys.contains(k));
                self.ack(tag);
            }
            GET_SERVER_INFO => {
                end(r)?;
                let mut w = Writer::new();
                w.s("pulseaudio").s("16.1").s("root").s("wasm-env").sample_spec(DEVICE_SPEC).s(SINK).s(SOURCE).u32(0x7761_736d);
                w.channel_map(&[convert::FRONT_LEFT, convert::FRONT_RIGHT]);
                self.reply(tag, &w);
            }
            GET_SINK_INFO | GET_SOURCE_INFO => {
                let (idx, name) = (r.u32()?, r.string()?);
                end(r)?;
                let sink = command == GET_SINK_INFO;
                if !device_named(sink, idx, name.as_deref()) {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    let mut w = Writer::new();
                    device_record(&mut w, sink);
                    self.reply(tag, &w);
                }
            }
            GET_SINK_INFO_LIST | GET_SOURCE_INFO_LIST => {
                end(r)?;
                let mut w = Writer::new();
                device_record(&mut w, command == GET_SINK_INFO_LIST);
                self.reply(tag, &w);
            }
            LOOKUP_SINK | LOOKUP_SOURCE => {
                let name = r.string()?;
                end(r)?;
                if device_named(command == LOOKUP_SINK, INVALID, name.as_deref()) {
                    let mut w = Writer::new();
                    w.u32(0);
                    self.reply(tag, &w);
                } else {
                    self.error(tag, ERR_NOENTITY);
                }
            }
            GET_CLIENT_INFO | GET_CLIENT_INFO_LIST => {
                let one = if command == GET_CLIENT_INFO { Some(r.u32()?) } else { None };
                end(r)?;
                let mut w = Writer::new();
                let mut found = false;
                for c in conns() {
                    if one.is_some_and(|i| i != c.index) {
                        continue;
                    }
                    found = true;
                    let p = c.props.lock().unwrap().clone();
                    w.u32(c.index).string(prop(&p, "application.name")).u32(INVALID).s("protocol-native.c").proplist(&p);
                }
                if one.is_some() && !found {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    self.reply(tag, &w);
                }
            }
            GET_SINK_INPUT_INFO | GET_SINK_INPUT_INFO_LIST => {
                let one = if command == GET_SINK_INPUT_INFO { Some(r.u32()?) } else { None };
                end(r)?;
                let inputs: Vec<Arc<Play>> = SERVER.inputs.lock().unwrap().values().filter(|p| one.is_none_or(|i| i == p.index)).cloned().collect();
                if one.is_some() && inputs.is_empty() {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    let mut w = Writer::new();
                    for p in inputs {
                        p.record(&mut w);
                    }
                    self.reply(tag, &w);
                }
            }
            GET_SOURCE_OUTPUT_INFO | GET_SOURCE_OUTPUT_INFO_LIST => {
                let one = if command == GET_SOURCE_OUTPUT_INFO { Some(r.u32()?) } else { None };
                end(r)?;
                let outputs: Vec<Arc<Record>> = SERVER.outputs.lock().unwrap().values().filter(|p| one.is_none_or(|i| i == p.index)).cloned().collect();
                if one.is_some() && outputs.is_empty() {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    let mut w = Writer::new();
                    for p in outputs {
                        p.record(&mut w);
                    }
                    self.reply(tag, &w);
                }
            }
            // (No modules, cards or sample cache: empty lists.)
            GET_MODULE_INFO_LIST | GET_CARD_INFO_LIST | GET_SAMPLE_INFO_LIST => {
                end(r)?;
                self.ack(tag);
            }
            GET_CARD_INFO => self.error(tag, ERR_NOENTITY),
            STAT => {
                end(r)?;
                let mut w = Writer::new();
                w.u32(0).u32(0).u32(0).u32(0).u32(0);
                self.reply(tag, &w);
            }
            SUBSCRIBE => {
                let mask = r.u32()?;
                end(r)?;
                if mask & !0x02ff != 0 {
                    self.error(tag, ERR_INVALID);
                } else {
                    self.subscribed.store(mask, Ordering::Relaxed);
                    self.ack(tag);
                }
            }
            SET_SINK_VOLUME | SET_SOURCE_VOLUME => {
                let (idx, name, v) = (r.u32()?, r.string()?, r.cvolume()?);
                end(r)?;
                let sink = command == SET_SINK_VOLUME;
                if !(v.len() == 1 || v.len() == 2) || v.iter().any(|&x| x > 0x7fff_ffff) {
                    self.error(tag, ERR_INVALID);
                } else if !device_named(sink, idx, name.as_deref()) {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    let v = if v.len() == 1 { [v[0]; 2] } else { [v[0], v[1]] };
                    if sink {
                        crate::audio::set_master(Some(v.map(step_of_volume)), None);
                    } else {
                        SERVER.source_volume.lock().unwrap().0 = v;
                        retune_records();
                    }
                    self.ack(tag);
                    broadcast(if sink { FAC_SINK } else { FAC_SOURCE }, EV_CHANGE, 0);
                }
            }
            SET_SINK_MUTE | SET_SOURCE_MUTE => {
                let (idx, name, mute) = (r.u32()?, r.string()?, r.bool()?);
                end(r)?;
                let sink = command == SET_SINK_MUTE;
                if !device_named(sink, idx, name.as_deref()) {
                    self.error(tag, ERR_NOENTITY);
                } else {
                    if sink {
                        crate::audio::set_master(None, Some(!mute));
                    } else {
                        SERVER.source_volume.lock().unwrap().1 = mute;
                        retune_records();
                    }
                    self.ack(tag);
                    broadcast(if sink { FAC_SINK } else { FAC_SOURCE }, EV_CHANGE, 0);
                }
            }
            SET_SINK_INPUT_VOLUME | SET_SINK_INPUT_MUTE => {
                let idx = r.u32()?;
                let (v, mute) = if command == SET_SINK_INPUT_VOLUME { (Some(r.cvolume()?), None) } else { (None, Some(r.bool()?)) };
                end(r)?;
                let Some(p) = SERVER.inputs.lock().unwrap().get(&idx).cloned() else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                if v.as_ref().is_some_and(|v| v.len() != 1 && v.len() != p.map.len()) {
                    self.error(tag, ERR_INVALID);
                    return Some(());
                }
                {
                    let mut vol = p.volume.lock().unwrap();
                    if let Some(v) = v {
                        vol.0 = if v.len() == 1 { vec![v[0]; p.map.len()] } else { v };
                    }
                    if let Some(m) = mute {
                        vol.1 = m;
                    }
                    p.pcm.set_gain(gains(&vol.0, &p.map, vol.1));
                }
                self.ack(tag);
                broadcast(FAC_SINK_INPUT, EV_CHANGE, idx);
            }
            SET_DEFAULT_SINK | SET_DEFAULT_SOURCE => {
                let name = r.string()?;
                end(r)?;
                if device_named(command == SET_DEFAULT_SINK, INVALID, name.as_deref()) {
                    self.ack(tag);
                } else {
                    self.error(tag, ERR_NOENTITY);
                }
            }
            // (One sink and one source: nowhere else to move to.)
            MOVE_SINK_INPUT | MOVE_SOURCE_OUTPUT => {
                let (_, idx, name) = (r.u32()?, r.u32()?, r.string()?);
                end(r)?;
                if device_named(command == MOVE_SINK_INPUT, idx, name.as_deref()) {
                    self.ack(tag);
                } else {
                    self.error(tag, ERR_NOENTITY);
                }
            }
            SUSPEND_SINK | SUSPEND_SOURCE => {
                let (_, _, _) = (r.u32()?, r.string()?, r.bool()?);
                end(r)?;
                self.ack(tag);
            }
            EXTENSION => self.error(tag, ERR_NOEXTENSION),
            // (Never offered: no reply, as PulseAudio gives none.)
            REGISTER_MEMFD_SHMID | ENABLE_SRBCHANNEL => {}
            CREATE_PLAYBACK_STREAM => self.create_play(tag, r)?,
            CREATE_RECORD_STREAM => self.create_record(tag, r)?,
            DELETE_PLAYBACK_STREAM | DELETE_RECORD_STREAM => {
                let ch = r.u32()?;
                end(r)?;
                let gone = if command == DELETE_PLAYBACK_STREAM { self.drop_play(ch) } else { self.drop_record(ch) };
                if gone { self.ack(tag) } else { self.error(tag, ERR_NOENTITY) }
            }
            CORK_PLAYBACK_STREAM | FLUSH_PLAYBACK_STREAM | TRIGGER_PLAYBACK_STREAM | PREBUF_PLAYBACK_STREAM | DRAIN_PLAYBACK_STREAM => {
                let ch = r.u32()?;
                let cork = if command == CORK_PLAYBACK_STREAM { Some(r.bool()?) } else { None };
                end(r)?;
                let Some(p) = self.play(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                match command {
                    CORK_PLAYBACK_STREAM => {
                        let on = cork.unwrap();
                        p.state.lock().unwrap().corked = on;
                        p.pcm.pause(on);
                        self.ack(tag);
                        broadcast(FAC_SINK_INPUT, EV_CHANGE, p.index);
                    }
                    FLUSH_PLAYBACK_STREAM => {
                        let filled = p.pcm.position().filled.max(0) * p.conv.client_frame() as i64;
                        p.pcm.flush();
                        {
                            let mut s = p.state.lock().unwrap();
                            s.written -= filled;
                            s.missing += filled;
                        }
                        self.ack(tag);
                        p.update(self);
                    }
                    TRIGGER_PLAYBACK_STREAM => {
                        p.pcm.trigger();
                        self.ack(tag);
                    }
                    PREBUF_PLAYBACK_STREAM => self.ack(tag),
                    _ => {
                        // (Answered once it's played out: the clock sees.)
                        p.pcm.drain();
                        let mut s = p.state.lock().unwrap();
                        if p.pcm.position().filled <= 0 {
                            drop(s);
                            self.ack(tag);
                        } else {
                            if let Some(old) = s.drain.replace(tag) {
                                self.error(old, ERR_NOENTITY);
                            }
                        }
                    }
                }
            }
            CORK_RECORD_STREAM | FLUSH_RECORD_STREAM => {
                let ch = r.u32()?;
                let cork = if command == CORK_RECORD_STREAM { Some(r.bool()?) } else { None };
                end(r)?;
                let Some(rec) = self.record(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                if let Some(on) = cork {
                    rec.state.lock().unwrap().corked = on;
                    rec.pcm.pause(on);
                    broadcast(FAC_SOURCE_OUTPUT, EV_CHANGE, rec.index);
                } else {
                    let n = rec.pcm.position().filled.max(0) as usize;
                    rec.pcm.get(n * rec.conv.card_frame());
                }
                self.ack(tag);
            }
            GET_PLAYBACK_LATENCY => {
                let ch = r.u32()?;
                let (sec, usec) = r.timeval()?;
                end(r)?;
                let Some(p) = self.play(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                let pos = p.pcm.position();
                let s = p.state.lock().unwrap();
                let read = s.written - pos.filled as i64 * p.conv.client_frame() as i64;
                let (now_s, now_us) = now();
                let mut w = Writer::new();
                w.usec(DEVICE_LATENCY).usec(0).bool(pos.running);
                w.timeval(sec, usec).timeval(now_s, now_us).s64(s.written).s64(read);
                let since = (read - s.played_since).max(0) as u64;
                if pos.running { w.u64(0).u64(since) } else { w.u64(if s.played_since == 0 && read == 0 { u64::MAX } else { 0 }).u64(0) };
                drop(s);
                self.reply(tag, &w);
            }
            GET_RECORD_LATENCY => {
                let ch = r.u32()?;
                let (sec, usec) = r.timeval()?;
                end(r)?;
                let Some(rec) = self.record(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                let pos = rec.pcm.position();
                let sent = rec.state.lock().unwrap().sent;
                let queued = pos.filled as i64 * rec.conv.client_frame() as i64;
                let (now_s, now_us) = now();
                let mut w = Writer::new();
                w.usec(0).usec(DEVICE_LATENCY).bool(pos.running);
                w.timeval(sec, usec).timeval(now_s, now_us).s64(sent + queued).s64(sent);
                self.reply(tag, &w);
            }
            SET_PLAYBACK_STREAM_BUFFER_ATTR => {
                let ch = r.u32()?;
                let req = PlayAttr { maxlength: r.u32()?, tlength: r.u32()?, prebuf: r.u32()?, minreq: r.u32()? };
                let (adjust, early) = (r.bool()?, r.bool()?);
                end(r)?;
                let Some(p) = self.play(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                let (a, latency) = fix_play_attr(req, p.conv.client_frame() as u32, p.spec.rate, adjust, early);
                *p.attr.lock().unwrap() = a;
                p.pcm.set_start((a.prebuf / p.conv.client_frame() as u32) as u64);
                let mut w = Writer::new();
                w.u32(a.maxlength).u32(a.tlength).u32(a.prebuf).u32(a.minreq).usec(latency);
                self.reply(tag, &w);
                p.update(self);
            }
            SET_RECORD_STREAM_BUFFER_ATTR => {
                let ch = r.u32()?;
                let (maxlength, fragsize) = (r.u32()?, r.u32()?);
                let (adjust, early) = (r.bool()?, r.bool()?);
                end(r)?;
                let Some(rec) = self.record(ch) else {
                    self.error(tag, ERR_NOENTITY);
                    return Some(());
                };
                let (a, latency) = fix_record_attr(maxlength, fragsize, rec.conv.client_frame() as u32, rec.spec.rate, adjust || early);
                *rec.attr.lock().unwrap() = a;
                let mut w = Writer::new();
                w.u32(a.0).u32(a.1).usec(latency);
                self.reply(tag, &w);
            }
            // (The card's streams keep the rate they were made with.)
            UPDATE_PLAYBACK_STREAM_SAMPLE_RATE | UPDATE_RECORD_STREAM_SAMPLE_RATE => {
                let (_, _) = (r.u32()?, r.u32()?);
                end(r)?;
                self.error(tag, ERR_NOTSUPPORTED);
            }
            UPDATE_PLAYBACK_STREAM_PROPLIST | UPDATE_RECORD_STREAM_PROPLIST => {
                let (ch, mode, p) = (r.u32()?, r.u32()?, r.proplist()?);
                end(r)?;
                let props = if command == UPDATE_PLAYBACK_STREAM_PROPLIST { self.play(ch).map(|s| (s.index, FAC_SINK_INPUT, s)).map(|(i, f, s)| (i, f, Props::Play(s))) } else { self.record(ch).map(|s| (s.index, FAC_SOURCE_OUTPUT, Props::Record(s))) };
                match props {
                    Some((index, facility, s)) => {
                        update_props(&mut s.props().lock().unwrap(), mode, p);
                        self.ack(tag);
                        broadcast(facility, EV_CHANGE, index);
                    }
                    None => self.error(tag, ERR_NOENTITY),
                }
            }
            REMOVE_PLAYBACK_STREAM_PROPLIST | REMOVE_RECORD_STREAM_PROPLIST => {
                let ch = r.u32()?;
                let keys = keys(r)?;
                let props = if command == REMOVE_PLAYBACK_STREAM_PROPLIST { self.play(ch).map(Props::Play) } else { self.record(ch).map(Props::Record) };
                match props {
                    Some(s) => {
                        s.props().lock().unwrap().retain(|(k, _)| !keys.contains(k));
                        self.ack(tag);
                    }
                    None => self.error(tag, ERR_NOENTITY),
                }
            }
            SET_PLAYBACK_STREAM_NAME | SET_RECORD_STREAM_NAME => {
                let (ch, name) = (r.u32()?, r.string()?);
                end(r)?;
                let props = if command == SET_PLAYBACK_STREAM_NAME { self.play(ch).map(Props::Play) } else { self.record(ch).map(Props::Record) };
                match (props, name) {
                    (Some(s), Some(name)) => {
                        update_props(&mut s.props().lock().unwrap(), 2, vec![string("media.name", &name)]);
                        self.ack(tag);
                    }
                    (None, _) => self.error(tag, ERR_NOENTITY),
                    (_, None) => self.error(tag, ERR_INVALID),
                }
            }
            // Everything else (modules, samples, uploads, killing, ports,
            // cards, exiting): not here.
            7 | 15..=19 | 25 | 33 | 48..=56 | 90 | 96..=100 | 104 => self.error(tag, if command == 7 { ERR_ACCESS } else { ERR_NOTSUPPORTED }),
            _ => return None,
        }
        Some(())
    }

    fn play(&self, ch: u32) -> Option<Arc<Play>> {
        self.streams.lock().unwrap().plays.get(&ch).cloned()
    }

    fn record(&self, ch: u32) -> Option<Arc<Record>> {
        self.streams.lock().unwrap().records.get(&ch).cloned()
    }

    fn create_play(self: &Arc<Self>, tag: u32, r: &mut Reader) -> Option<()> {
        let (spec, map, sink_idx, sink_name) = (r.sample_spec()?, r.channel_map()?, r.u32()?, r.string()?);
        let maxlength = r.u32()?;
        let corked = r.bool()?;
        let (tlength, prebuf, minreq, _sync) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        let volume = r.cvolume()?;
        let (_no_remap, _no_remix, _fix_format, _fix_rate, _fix_channels, _dont_move, _variable_rate) = (r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?);
        let (start_muted, adjust) = (r.bool()?, r.bool()?);
        let props = r.proplist()?;
        let (volume_set, early) = (r.bool()?, r.bool()?);
        let (muted_set, _dont_inhibit, _fail_on_suspend) = (r.bool()?, r.bool()?, r.bool()?);
        let _relative = r.bool()?;
        let _passthrough = r.bool()?;
        let formats = formats(r)?;
        r.eof().then_some(())?;
        if !device_named(true, sink_idx, sink_name.as_deref()) {
            self.error(tag, ERR_NOENTITY);
            return Some(());
        }
        let Some((spec, map)) = stream_format(spec, map, &formats) else {
            self.error(tag, if formats.is_empty() { ERR_INVALID } else { ERR_NOTSUPPORTED });
            return Some(());
        };
        if formats.is_empty() && volume.len() != spec.channels as usize {
            self.error(tag, ERR_INVALID);
            return Some(());
        }
        let conv = Conversion::new(spec.format, &map);
        let fs = conv.client_frame() as u32;
        let (attr, latency) = fix_play_attr(PlayAttr { maxlength, tlength, prebuf, minreq }, fs, spec.rate, adjust, early);
        // (The card's ring: room for what the client may have in flight,
        // up to ten seconds.)
        let frames = ((attr.maxlength / fs) as u64).min(spec.rate as u64 * 10).max(((attr.tlength + attr.minreq) / fs) as u64);
        let pcm = Pcm::server(false, conv.card, conv.card_channels, spec.rate, frames, (attr.prebuf / fs) as u64);
        let volume = if volume_set && volume.len() == map.len() { volume } else { vec![NORM; map.len()] };
        let muted = muted_set && start_muted;
        pcm.set_gain(gains(&volume, &map, muted));
        if corked {
            pcm.pause(true);
        }
        let channel = {
            let mut st = self.streams.lock().unwrap();
            st.next_play += 1;
            st.next_play - 1
        };
        let play = Arc::new(Play {
            channel,
            index: SERVER.next_input.fetch_add(1, Ordering::Relaxed),
            client: self.index,
            spec,
            map: map.clone(),
            conv,
            pcm,
            attr: Mutex::new(attr),
            state: Mutex::new(PlayState { missing: attr.tlength as i64, corked, ..Default::default() }),
            volume: Mutex::new((volume, muted)),
            props: Mutex::new(props),
        });
        let requested = play.pop_missing(&mut play.state.lock().unwrap());
        self.streams.lock().unwrap().plays.insert(channel, play.clone());
        SERVER.inputs.lock().unwrap().insert(play.index, play.clone());
        let mut w = Writer::new();
        w.u32(channel).u32(play.index).u32(requested as u32);
        w.u32(attr.maxlength).u32(attr.tlength).u32(attr.prebuf).u32(attr.minreq);
        w.sample_spec(spec).channel_map(&map).u32(0).s(SINK).bool(false).usec(latency);
        format_info(&mut w, spec, &map);
        self.reply(tag, &w);
        broadcast(FAC_SINK_INPUT, EV_NEW, play.index);
        Some(())
    }

    fn create_record(self: &Arc<Self>, tag: u32, r: &mut Reader) -> Option<()> {
        let (spec, map, source_idx, source_name) = (r.sample_spec()?, r.channel_map()?, r.u32()?, r.string()?);
        let maxlength = r.u32()?;
        let corked = r.bool()?;
        let fragsize = r.u32()?;
        let (_no_remap, _no_remix, _fix_format, _fix_rate, _fix_channels, _dont_move, _variable_rate) = (r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?);
        let (_peak, adjust) = (r.bool()?, r.bool()?);
        let props = r.proplist()?;
        let direct = r.u32()?;
        let early = r.bool()?;
        let (_dont_inhibit, _fail_on_suspend) = (r.bool()?, r.bool()?);
        let formats = formats(r)?;
        let volume = r.cvolume()?;
        let (start_muted, volume_set, muted_set, _relative, _passthrough) = (r.bool()?, r.bool()?, r.bool()?, r.bool()?, r.bool()?);
        r.eof().then_some(())?;
        if !device_named(false, source_idx, source_name.as_deref()) || direct != INVALID {
            self.error(tag, ERR_NOENTITY);
            return Some(());
        }
        let Some((spec, map)) = stream_format(spec, map, &formats) else {
            self.error(tag, if formats.is_empty() { ERR_INVALID } else { ERR_NOTSUPPORTED });
            return Some(());
        };
        let conv = Conversion::new(spec.format, &map);
        let fs = conv.client_frame() as u32;
        let (attr, latency) = fix_record_attr(maxlength, fragsize, fs, spec.rate, adjust || early);
        let frames = ((2 * attr.1 / fs) as u64).max(spec.rate as u64 / 2).min(spec.rate as u64 * 10);
        let pcm = Pcm::server(true, conv.card, conv.card_channels, spec.rate, frames, 1);
        let volume = if volume_set && volume.len() == map.len() { volume } else { vec![NORM; map.len()] };
        let muted = muted_set && start_muted;
        if corked {
            pcm.pause(true);
        }
        let channel = {
            let mut st = self.streams.lock().unwrap();
            st.next_record += 1;
            st.next_record - 1
        };
        let rec = Arc::new(Record {
            channel,
            index: SERVER.next_output.fetch_add(1, Ordering::Relaxed),
            client: self.index,
            spec,
            map: map.clone(),
            conv,
            pcm,
            attr: Mutex::new(attr),
            state: Mutex::new(RecordState { sent: 0, corked }),
            volume: Mutex::new((volume, muted)),
            props: Mutex::new(props),
        });
        rec.retune();
        self.streams.lock().unwrap().records.insert(channel, rec.clone());
        SERVER.outputs.lock().unwrap().insert(rec.index, rec.clone());
        let mut w = Writer::new();
        w.u32(channel).u32(rec.index).u32(attr.0).u32(attr.1);
        w.sample_spec(spec).channel_map(&map).u32(0).s(SOURCE).bool(false).usec(latency);
        format_info(&mut w, spec, &map);
        self.reply(tag, &w);
        broadcast(FAC_SOURCE_OUTPUT, EV_NEW, rec.index);
        Some(())
    }

    fn drop_play(&self, ch: u32) -> bool {
        let Some(p) = self.streams.lock().unwrap().plays.remove(&ch) else { return false };
        SERVER.inputs.lock().unwrap().remove(&p.index);
        if let Some(t) = p.state.lock().unwrap().drain.take() {
            self.error(t, ERR_NOENTITY);
        }
        broadcast(FAC_SINK_INPUT, EV_REMOVE, p.index);
        true
    }

    fn drop_record(&self, ch: u32) -> bool {
        let Some(rec) = self.streams.lock().unwrap().records.remove(&ch) else { return false };
        SERVER.outputs.lock().unwrap().remove(&rec.index);
        broadcast(FAC_SOURCE_OUTPUT, EV_REMOVE, rec.index);
        true
    }

    /// A stream's data from the client.
    fn data(self: &Arc<Self>, ch: u32, _offset: i64, b: &[u8]) -> bool {
        // (Unknown streams' data, or part frames, are dropped, as
        // PulseAudio drops them.)
        let Some(p) = self.play(ch) else { return true };
        let fs = p.conv.client_frame();
        if b.len() % fs != 0 {
            return true;
        }
        let card = p.conv.to_card(b);
        let took = p.pcm.put(&card);
        let overflow = took < card.len();
        {
            let mut s = p.state.lock().unwrap();
            s.written += b.len() as i64;
            s.requested -= b.len() as i64;
        }
        if overflow {
            let mut w = Writer::new();
            w.u32(p.channel);
            self.send(OVERFLOW, INVALID, &w);
        }
        p.update(self);
        true
    }

    /// The clock: playback streams' accounting, record streams' data.
    fn tick(&self) {
        let (plays, records): (Vec<Arc<Play>>, Vec<Arc<Record>>) = {
            let st = self.streams.lock().unwrap();
            (st.plays.values().cloned().collect(), st.records.values().cloned().collect())
        };
        for p in plays {
            p.update(self);
        }
        for rec in records {
            rec.send(self);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let st = std::mem::take(&mut *self.streams.lock().unwrap());
        for p in st.plays.values() {
            SERVER.inputs.lock().unwrap().remove(&p.index);
            broadcast(FAC_SINK_INPUT, EV_REMOVE, p.index);
        }
        for rec in st.records.values() {
            SERVER.outputs.lock().unwrap().remove(&rec.index);
            broadcast(FAC_SOURCE_OUTPUT, EV_REMOVE, rec.index);
        }
        let _ = self.sock.shutdown(2);
        SERVER.conns.lock().unwrap().retain(|c| c.strong_count() > 0 && !c.upgrade().is_some_and(|c| c.index == self.index));
        broadcast(FAC_CLIENT, EV_REMOVE, self.index);
    }
}

enum Props {
    Play(Arc<Play>),
    Record(Arc<Record>),
}

impl Props {
    fn props(&self) -> &Mutex<Proplist> {
        match self {
            Props::Play(p) => &p.props,
            Props::Record(r) => &r.props,
        }
    }
}

impl Play {
    /// What the client is asked for next (pop_missing): what's been played
    /// since, in batches of at least minreq (smaller while prebuffering).
    fn pop_missing(&self, s: &mut PlayState) -> i64 {
        let attr = *self.attr.lock().unwrap();
        if s.missing <= 0 {
            return 0;
        }
        let pos = self.pcm.position();
        let prebuffering = !pos.running && (pos.filled.max(0) as u64 * self.conv.client_frame() as u64) < attr.prebuf as u64;
        if s.missing < attr.minreq as i64 && !prebuffering {
            return 0;
        }
        let n = s.missing;
        s.requested += n;
        s.missing = 0;
        n
    }

    /// Catches up with the card: what's been played is asked for again;
    /// starts, underruns and drains are told.
    fn update(&self, conn: &Conn) {
        let pos = self.pcm.position();
        let fs = self.conv.client_frame() as i64;
        let mut events: Vec<(u32, Writer)> = Vec::new();
        let mut done_drain = None;
        let request;
        {
            let mut s = self.state.lock().unwrap();
            let read = s.written - pos.filled as i64 * fs;
            if read > s.read {
                s.missing += read - s.read;
                s.read = read;
            }
            if pos.running && !s.playing {
                s.playing = true;
                s.played_since = read;
                let mut w = Writer::new();
                w.u32(self.channel);
                events.push((STARTED, w));
            }
            if pos.underruns > s.underruns {
                s.underruns = pos.underruns;
                s.playing = false;
                if s.drain.is_none() {
                    let mut w = Writer::new();
                    w.u32(self.channel).s64(read);
                    events.push((UNDERFLOW, w));
                }
            }
            if !pos.running && pos.filled <= 0 {
                s.playing = false;
            }
            if s.drain.is_some() && !pos.draining && pos.filled <= 0 {
                done_drain = s.drain.take();
            }
            request = self.pop_missing(&mut s);
        }
        for (cmd, w) in events {
            conn.send(cmd, INVALID, &w);
        }
        if let Some(t) = done_drain {
            conn.ack(t);
        }
        if request > 0 {
            let mut w = Writer::new();
            w.u32(self.channel).u32(request as u32);
            conn.send(REQUEST, INVALID, &w);
        }
    }

    /// Its sink-input record.
    fn record(&self, w: &mut Writer) {
        let props = self.props.lock().unwrap().clone();
        let (volume, muted) = self.volume.lock().unwrap().clone();
        let pos = self.pcm.position();
        let buffered = pos.filled.max(0) as u64 * 1_000_000 / self.spec.rate as u64;
        let corked = self.state.lock().unwrap().corked;
        w.u32(self.index).string(Some(prop(&props, "media.name").unwrap_or("Playback"))).u32(INVALID).u32(self.client).u32(0);
        w.sample_spec(self.spec).channel_map(&self.map).cvolume(&volume).usec(buffered).usec(DEVICE_LATENCY);
        w.string(None).s("protocol-native.c").bool(muted).proplist(&props).bool(corked).bool(true).bool(true);
        format_info(w, self.spec, &self.map);
    }
}

impl Record {
    /// Its gains: its own volume and the source's.
    fn retune(&self) {
        let (v, m) = self.volume.lock().unwrap().clone();
        let (sv, sm) = *SERVER.source_volume.lock().unwrap();
        let own = gains(&v, &self.map, m);
        let source = gains(&sv, &[convert::FRONT_LEFT, convert::FRONT_RIGHT], sm);
        self.pcm.set_gain([own[0] * source[0], own[1] * source[1]]);
    }

    /// What's been captured, to the client, at most fragsize at a time.
    fn send(&self, conn: &Conn) {
        if self.state.lock().unwrap().corked {
            return;
        }
        let card = self.pcm.get(usize::MAX);
        if card.is_empty() {
            return;
        }
        let data = self.conv.from_card(&card);
        let fs = self.conv.client_frame();
        let frag = ((self.attr.lock().unwrap().1 as usize / fs).max(1)) * fs;
        for chunk in data.chunks(frag) {
            conn.frame(self.channel, chunk);
        }
        self.state.lock().unwrap().sent += data.len() as i64;
    }

    /// Its source-output record.
    fn record(&self, w: &mut Writer) {
        let props = self.props.lock().unwrap().clone();
        let (volume, muted) = self.volume.lock().unwrap().clone();
        let pos = self.pcm.position();
        let buffered = pos.filled.max(0) as u64 * 1_000_000 / self.spec.rate as u64;
        let corked = self.state.lock().unwrap().corked;
        w.u32(self.index).string(Some(prop(&props, "media.name").unwrap_or("Record"))).u32(INVALID).u32(self.client).u32(0);
        w.sample_spec(self.spec).channel_map(&self.map).usec(buffered).usec(DEVICE_LATENCY).string(None).s("protocol-native.c");
        w.proplist(&props).bool(corked).cvolume(&volume).bool(muted).bool(true).bool(true);
        format_info(w, self.spec, &self.map);
    }
}

/// Record streams' gains again (the source's volume changed).
fn retune_records() {
    let records: Vec<Arc<Record>> = SERVER.outputs.lock().unwrap().values().cloned().collect();
    for r in records {
        r.retune();
    }
}

fn conns() -> Vec<Arc<Conn>> {
    let mut c = SERVER.conns.lock().unwrap();
    c.retain(|w| w.strong_count() > 0);
    c.iter().filter_map(Weak::upgrade).filter(|c| !c.closed.load(Ordering::Relaxed)).collect()
}

/// Tells subscribers of `facility` about a change.
fn broadcast(facility: u32, kind: u32, index: u32) {
    for c in conns() {
        if c.subscribed.load(Ordering::Relaxed) & (1 << facility) != 0 {
            let mut w = Writer::new();
            w.u32(facility | kind).u32(index);
            c.send(SUBSCRIBE_EVENT, INVALID, &w);
        }
    }
}

/// Whether an index and name (namereg's rules) name the sink (or source).
fn device_named(sink: bool, idx: u32, name: Option<&str>) -> bool {
    let (own, default) = if sink { (SINK, "@DEFAULT_SINK@") } else { (SOURCE, "@DEFAULT_SOURCE@") };
    match (idx, name) {
        (INVALID, None) => true,
        (INVALID, Some(n)) => n == own || n == default || n == "0",
        (0, None) => true,
        _ => false,
    }
}

/// The sink's (or source's) record.
fn device_record(w: &mut Writer, sink: bool) {
    let (name, description, volume, muted) = if sink {
        let (steps, on) = crate::audio::master();
        (SINK, "wasm-env sound card", steps.map(volume_of_step), !on)
    } else {
        let (v, m) = *SERVER.source_volume.lock().unwrap();
        (SOURCE, "wasm-env sound card (input)", v, m)
    };
    let busy = if sink { SERVER.inputs.lock().unwrap().values().any(|p| p.pcm.position().running) } else { !SERVER.outputs.lock().unwrap().is_empty() };
    let props = vec![
        string("device.description", description),
        string("device.class", "sound"),
        string("device.api", "wasm-env"),
        string("device.icon_name", "audio-card"),
    ];
    // HW_VOLUME_CTRL, LATENCY, HARDWARE, HW_MUTE_CTRL, DECIBEL_VOLUME.
    let flags = 0x1 | 0x2 | 0x4 | 0x10 | 0x20;
    w.u32(0).s(name).s(description).sample_spec(DEVICE_SPEC).channel_map(&[convert::FRONT_LEFT, convert::FRONT_RIGHT]).u32(INVALID);
    w.cvolume(&volume).bool(muted).u32(INVALID).string(None);
    w.usec(DEVICE_LATENCY).s("wasm-env").u32(flags).proplist(&props).usec(DEVICE_LATENCY);
    w.volume(NORM).u32(if busy { 0 } else { 1 }).u32(NORM + 1).u32(INVALID);
    w.u32(0).string(None);
    w.u8(1).format_info(1, &Vec::new());
}

/// A stream's format_info: PCM, its sample format, rate and channels.
fn format_info(w: &mut Writer, spec: SampleSpec, map: &[u8]) {
    let names: Vec<&str> = map.iter().map(|&p| position_name(p)).collect();
    let p = vec![
        string("format.sample_format", &format!("\"{}\"", format_name(spec.format))),
        string("format.rate", &spec.rate.to_string()),
        string("format.channels", &spec.channels.to_string()),
        string("format.channel_map", &format!("\"{}\"", names.join(","))),
    ];
    w.format_info(1, &p);
}

const FORMAT_NAMES: [&str; 13] = ["u8", "aLaw", "uLaw", "s16le", "s16be", "float32le", "float32be", "s32le", "s32be", "s24le", "s24be", "s24-32le", "s24-32be"];

fn format_name(f: u8) -> &'static str {
    FORMAT_NAMES.get(f as usize).copied().unwrap_or("invalid")
}

const POSITION_NAMES: [&str; 12] = ["mono", "front-left", "front-right", "front-center", "rear-center", "rear-left", "rear-right", "lfe", "front-left-of-center", "front-right-of-center", "side-left", "side-right"];
const TOP_NAMES: [&str; 7] = ["top-center", "top-front-left", "top-front-right", "top-front-center", "top-rear-left", "top-rear-right", "top-rear-center"];

fn position_name(p: u8) -> &'static str {
    const AUX: [&str; 32] = [
        "aux0", "aux1", "aux2", "aux3", "aux4", "aux5", "aux6", "aux7", "aux8", "aux9", "aux10", "aux11", "aux12", "aux13", "aux14", "aux15", "aux16", "aux17", "aux18", "aux19", "aux20", "aux21", "aux22", "aux23", "aux24", "aux25", "aux26", "aux27", "aux28",
        "aux29", "aux30", "aux31",
    ];
    match p {
        0..=11 => POSITION_NAMES[p as usize],
        12..=43 => AUX[p as usize - 12],
        44..=50 => TOP_NAMES[p as usize - 44],
        _ => "invalid",
    }
}

fn position_of(name: &str) -> Option<u8> {
    let name = match name {
        "left" => "front-left",
        "right" => "front-right",
        "center" => "front-center",
        "subwoofer" => "lfe",
        n => n,
    };
    (0..51u8).find(|&p| position_name(p) == name)
}

/// The formats a stream offers (pa_stream_new_extended): (encoding,
/// properties).
fn formats(r: &mut Reader) -> Option<Vec<(u8, Proplist)>> {
    let n = r.u8()?;
    (0..n).map(|_| r.format_info()).collect()
}

/// A stream's sample spec and channel map: as given, or from the first PCM
/// format offered. None if they aren't valid (or none is PCM).
fn stream_format(spec: SampleSpec, map: Vec<u8>, formats: &[(u8, Proplist)]) -> Option<(SampleSpec, Vec<u8>)> {
    let (spec, map) = if formats.is_empty() {
        (spec, map)
    } else {
        let (_, p) = formats.iter().find(|(enc, _)| *enc == 1)?;
        let unquote = |k: &str| prop(p, k).map(|v| v.trim_matches('"').to_string());
        let format = FORMAT_NAMES.iter().position(|&n| Some(n) == unquote("format.sample_format").as_deref())? as u8;
        let rate = unquote("format.rate")?.parse().ok()?;
        let channels: u8 = unquote("format.channels")?.parse().ok()?;
        let map = match unquote("format.channel_map") {
            Some(m) => m.split(',').map(position_of).collect::<Option<Vec<u8>>>()?,
            None if channels == 1 => vec![convert::MONO],
            None => (0..channels).map(|c| if c < 2 { c + 1 } else { 10 + c }).collect(),
        };
        (SampleSpec { format, channels, rate }, map)
    };
    let valid = convert::sample_bytes(spec.format).is_some()
        && (1..=775_680).contains(&spec.rate)
        && (1..=32).contains(&spec.channels)
        && map.len() == spec.channels as usize
        && map.iter().all(|&p| p < 51);
    valid.then_some((spec, map))
}

/// A playback stream's buffer attributes as PulseAudio fixes them
/// (fix_playback_buffer_attr), and the sink latency they come to.
fn fix_play_attr(a: PlayAttr, fs: u32, rate: u32, adjust: bool, early: bool) -> (PlayAttr, u64) {
    let fs = fs.max(1) as u64;
    let bytes = |us: u64| (us * rate as u64).div_ceil(1_000_000) * fs;
    let usec = |b: u64| b / fs * 1_000_000 / rate as u64;
    let round_up = |b: u64| b.div_ceil(fs) * fs;
    let mut maxlength = if a.maxlength == INVALID || a.maxlength > 4 << 20 { 4 << 20 } else { a.maxlength as u64 };
    if maxlength == 0 {
        maxlength = fs;
    }
    maxlength = round_up(maxlength);
    let mut tlength = match a.tlength {
        INVALID => bytes(2_000_000),
        0 => fs,
        t => t as u64,
    }
    .min(maxlength);
    let mut minreq = match a.minreq {
        INVALID => bytes(20_000).min(tlength / 4 / fs * fs),
        0 => fs,
        m => m as u64,
    };
    minreq = (minreq / fs * fs).max(fs);
    if tlength < minreq + fs {
        tlength = minreq + fs;
    }
    let wanted = if early {
        minreq
    } else if adjust {
        tlength.saturating_sub(2 * minreq) / 2
    } else {
        tlength.saturating_sub(2 * minreq)
    };
    let latency = usec(wanted).max(DEVICE_LATENCY);
    if adjust {
        tlength = tlength.saturating_sub(bytes(latency));
    }
    tlength = tlength.max(bytes(latency) + 2 * minreq);
    if tlength <= minreq {
        tlength = 2 * minreq + fs;
    }
    tlength = round_up(tlength);
    let most = tlength + fs - minreq;
    let prebuf = if a.prebuf == INVALID || a.prebuf as u64 > most { most } else { round_up(a.prebuf as u64) };
    (PlayAttr { maxlength: maxlength as u32, tlength: tlength as u32, prebuf: prebuf as u32, minreq: minreq as u32 }, latency)
}

/// A record stream's (maxlength, fragsize) as PulseAudio fixes them, and
/// the source latency.
fn fix_record_attr(maxlength: u32, fragsize: u32, fs: u32, rate: u32, latency_asked: bool) -> ((u32, u32), u64) {
    let fs = fs.max(1) as u64;
    let bytes = |us: u64| (us * rate as u64).div_ceil(1_000_000) * fs;
    let mut max = if maxlength == INVALID || maxlength > 4 << 20 { 4 << 20 } else { maxlength as u64 };
    if max == 0 {
        max = fs;
    }
    max = max.div_ceil(fs) * fs;
    let mut frag = match fragsize {
        INVALID => bytes(2_000_000),
        0 => fs,
        f => f as u64,
    };
    frag = (frag / fs * fs).max(fs).min(max);
    let latency = if latency_asked { (frag / fs * 1_000_000 / rate as u64).max(DEVICE_LATENCY) } else { DEVICE_LATENCY };
    ((max as u32, frag as u32), latency)
}

/// A proplist update (SET 0, MERGE 1, REPLACE 2).
fn update_props(p: &mut Proplist, mode: u32, new: Proplist) {
    match mode {
        0 => *p = new,
        1 => {
            for (k, v) in new {
                if !p.iter().any(|(key, _)| *key == k) {
                    p.push((k, v));
                }
            }
        }
        _ => {
            for (k, v) in new {
                p.retain(|(key, _)| *key != k);
                p.push((k, v));
            }
        }
    }
}

/// Property keys, ended by a null string.
fn keys(r: &mut Reader) -> Option<Vec<String>> {
    let mut k = Vec::new();
    while let Some(s) = r.string()? {
        k.push(s);
    }
    r.eof().then_some(k)
}

/// WASM_ENV_DEBUG_AUDIO: what clients ask.
fn debug() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some());
    *ON
}

fn now() -> (u32, u32) {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as u32, d.subsec_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_attributes_are_fixed_as_pulseaudios() {
        // paplay's defaults (all -1), S16LE stereo at 44.1 kHz: two
        // seconds, minreq 20 ms, prebuf the rest.
        let a = PlayAttr { maxlength: INVALID, tlength: INVALID, prebuf: INVALID, minreq: INVALID };
        let (a, _) = fix_play_attr(a, 4, 44100, false, false);
        assert_eq!(a.maxlength, 4 << 20);
        assert_eq!(a.tlength, 352800);
        assert_eq!(a.minreq, 3528);
        assert_eq!(a.prebuf, a.tlength + 4 - a.minreq);
        assert_eq!(a.tlength % 4, 0);
    }

    #[test]
    fn volumes_meet_the_master_volumes_steps() {
        assert_eq!(volume_of_step(100), NORM);
        assert_eq!(step_of_volume(NORM), 100);
        assert_eq!(step_of_volume(volume_of_step(50)), 50);
        assert_eq!(step_of_volume(0), 0);
        assert!((linear(NORM / 2) - 0.125).abs() < 1e-6);
    }

    #[test]
    fn extended_streams_take_their_pcm_format() {
        let p = vec![string("format.sample_format", "\"s16le\""), string("format.rate", "44100"), string("format.channels", "2"), string("format.channel_map", "\"front-left,front-right\"")];
        let got = stream_format(SampleSpec { format: 0xff, channels: 0, rate: 0 }, vec![], &[(1, p)]);
        assert_eq!(got, Some((SampleSpec { format: convert::S16LE, channels: 2, rate: 44100 }, vec![1, 2])));
        assert!(device_named(true, INVALID, Some("@DEFAULT_SINK@")) && !device_named(true, INVALID, Some("other")));
    }
}
