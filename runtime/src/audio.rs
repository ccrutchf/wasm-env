//! `/dev/snd`: the runtime's sound card (docs/abi.md, "Audio"), so the
//! environment's own `libasound` (and SDL, GStreamer, everything over ALSA)
//! plays sound on every host. It speaks ALSA's kernel interface: a control
//! device (`controlC0`: the card, "wasm-env", and its master volume and
//! switch), a playback PCM (`pcmC0D0p`) and a capture one (`pcmC0D0c`).
//! The card's driver matches no card configuration of
//! `libasound`'s, so its `default` is `plug` over `hw`: conversions the card
//! lacks happen in the guest.
//!
//! A PCM takes U8, S16_LE, S32_LE and FLOAT_LE, one or two channels, at 8 to
//! 192 kHz, through `WRITEI_FRAMES` and `WRITEN_FRAMES` (or `READ`s), or
//! memory of the program's own (the status and control pages refused,
//! `libasound` uses `SYNC_PTR`). Its frames wait in a ring buffer of the
//! size its parameters chose, which a mixer drains: every running playback
//! stream, converted to float stereo at the sink's rate (resampled
//! linearly), summed, at the master volume. The sink is the host's audio
//! output (`cpal`) or none (`--audio=null`: the mixer on a clock of its own),
//! which can record what each stream played (`WASM_ENV_AUDIO_RECORD`: the
//! bytes as written, for tests). Capture streams fill from the host's input
//! the same way round, or from silence on a clock (or `WASM_ENV_AUDIO_SOURCE`:
//! bytes as read, for tests).

use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use crate::abi::errno::*;
use crate::memory::{self as mem, SysResult};



// ---- The kernel's interface (sound/asound.h, for arm64) ----

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32
}
const NONE: u32 = 0;
const W: u32 = 1;
const R: u32 = 2;
const RW: u32 = 3;

const PCM_INFO_SIZE: usize = 288;
const HW_PARAMS_SIZE: usize = 608;
const SW_PARAMS_SIZE: usize = 136;
const STATUS_SIZE: usize = 152;
const SYNC_PTR_SIZE: usize = 136;
const XFER_SIZE: usize = 24;
const CHANNEL_INFO_SIZE: usize = 24;
const CARD_INFO_SIZE: usize = 376;
const ELEM_LIST_SIZE: usize = 80;
const ELEM_ID_SIZE: usize = 64;
const ELEM_INFO_SIZE: usize = 272;
const ELEM_VALUE_SIZE: usize = 1224;

const PCM_PVERSION: u32 = ioc(R, b'A', 0x00, 4);
const PCM_INFO: u32 = ioc(R, b'A', 0x01, PCM_INFO_SIZE);
const PCM_TSTAMP: u32 = ioc(W, b'A', 0x02, 4);
const PCM_TTSTAMP: u32 = ioc(W, b'A', 0x03, 4);
const PCM_USER_PVERSION: u32 = ioc(W, b'A', 0x04, 4);
const PCM_HW_REFINE: u32 = ioc(RW, b'A', 0x10, HW_PARAMS_SIZE);
const PCM_HW_PARAMS: u32 = ioc(RW, b'A', 0x11, HW_PARAMS_SIZE);
const PCM_HW_FREE: u32 = ioc(NONE, b'A', 0x12, 0);
const PCM_SW_PARAMS: u32 = ioc(RW, b'A', 0x13, SW_PARAMS_SIZE);
const PCM_STATUS: u32 = ioc(R, b'A', 0x20, STATUS_SIZE);
const PCM_DELAY: u32 = ioc(R, b'A', 0x21, 8);
const PCM_HWSYNC: u32 = ioc(NONE, b'A', 0x22, 0);
const PCM_SYNC_PTR: u32 = ioc(RW, b'A', 0x23, SYNC_PTR_SIZE);
const PCM_STATUS_EXT: u32 = ioc(RW, b'A', 0x24, STATUS_SIZE);
const PCM_CHANNEL_INFO: u32 = ioc(R, b'A', 0x32, CHANNEL_INFO_SIZE);
const PCM_PREPARE: u32 = ioc(NONE, b'A', 0x40, 0);
const PCM_RESET: u32 = ioc(NONE, b'A', 0x41, 0);
const PCM_START: u32 = ioc(NONE, b'A', 0x42, 0);
const PCM_DROP: u32 = ioc(NONE, b'A', 0x43, 0);
const PCM_DRAIN: u32 = ioc(NONE, b'A', 0x44, 0);
const PCM_PAUSE: u32 = ioc(W, b'A', 0x45, 4);
const PCM_REWIND: u32 = ioc(W, b'A', 0x46, 8);
const PCM_RESUME: u32 = ioc(NONE, b'A', 0x47, 0);
const PCM_XRUN: u32 = ioc(NONE, b'A', 0x48, 0);
const PCM_FORWARD: u32 = ioc(W, b'A', 0x49, 8);
const PCM_WRITEI: u32 = ioc(W, b'A', 0x50, XFER_SIZE);
const PCM_WRITEN: u32 = ioc(W, b'A', 0x52, XFER_SIZE);
const PCM_READI: u32 = ioc(R, b'A', 0x51, XFER_SIZE);
const PCM_READN: u32 = ioc(R, b'A', 0x53, XFER_SIZE);
const PCM_LINK: u32 = ioc(W, b'A', 0x60, 4);
const PCM_UNLINK: u32 = ioc(NONE, b'A', 0x61, 0);

const CTL_PVERSION: u32 = ioc(R, b'U', 0x00, 4);
const CTL_CARD_INFO: u32 = ioc(R, b'U', 0x01, CARD_INFO_SIZE);
const CTL_ELEM_LIST: u32 = ioc(RW, b'U', 0x10, ELEM_LIST_SIZE);
const CTL_ELEM_INFO: u32 = ioc(RW, b'U', 0x11, ELEM_INFO_SIZE);
const CTL_ELEM_READ: u32 = ioc(RW, b'U', 0x12, ELEM_VALUE_SIZE);
const CTL_ELEM_WRITE: u32 = ioc(RW, b'U', 0x13, ELEM_VALUE_SIZE);
const CTL_TLV_READ: u32 = ioc(RW, b'U', 0x1a, 8);
const CTL_SUBSCRIBE_EVENTS: u32 = ioc(RW, b'U', 0x16, 4);
const CTL_HWDEP_NEXT_DEVICE: u32 = ioc(RW, b'U', 0x20, 4);
const CTL_PCM_NEXT_DEVICE: u32 = ioc(R, b'U', 0x30, 4);
const CTL_PCM_INFO: u32 = ioc(RW, b'U', 0x31, PCM_INFO_SIZE);
const CTL_PCM_PREFER_SUBDEVICE: u32 = ioc(W, b'U', 0x32, 4);
const CTL_RAWMIDI_NEXT_DEVICE: u32 = ioc(RW, b'U', 0x40, 4);
const CTL_UMP_NEXT_DEVICE: u32 = ioc(RW, b'U', 0x43, 4);
const CTL_POWER_STATE: u32 = ioc(R, b'U', 0xd1, 4);

const fn version(major: u32, minor: u32, sub: u32) -> u32 {
    (major << 16) | (minor << 8) | sub
}
const PCM_VERSION: u32 = version(2, 0, 17);
const CTL_VERSION: u32 = version(2, 0, 9);

// Parameters: masks (access, format, subformat), then intervals.
const ACCESS: usize = 0;
const FORMAT: usize = 1;
const SUBFORMAT: usize = 2;
const SAMPLE_BITS: usize = 8;
const FRAME_BITS: usize = 9;
const CHANNELS: usize = 10;
const RATE: usize = 11;
const PERIOD_TIME: usize = 12;
const PERIOD_SIZE: usize = 13;
const PERIOD_BYTES: usize = 14;
const PERIODS: usize = 15;
const BUFFER_TIME: usize = 16;
const BUFFER_SIZE: usize = 17;
const BUFFER_BYTES: usize = 18;
const TICK_TIME: usize = 19;

const ACCESS_MMAP_INTERLEAVED: u32 = 0;
const ACCESS_MMAP_NONINTERLEAVED: u32 = 1;
const ACCESS_RW_INTERLEAVED: u32 = 3;
const ACCESS_RW_NONINTERLEAVED: u32 = 4;
const SUBFORMAT_STD: u32 = 0;

/// The formats the card takes: ALSA's number, and its width in bits.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    U8 = 1,
    S16 = 2,
    S32 = 10,
    F32 = 14,
}
const FORMATS: [Format; 4] = [Format::U8, Format::S16, Format::S32, Format::F32];

impl Format {
    fn bits(self) -> u32 {
        match self {
            Format::U8 => 8,
            Format::S16 => 16,
            Format::S32 | Format::F32 => 32,
        }
    }
    fn of(n: u32) -> Option<Format> {
        FORMATS.into_iter().find(|f| *f as u32 == n)
    }
    /// A sample as a float in [-1, 1].
    fn sample(self, b: &[u8]) -> f32 {
        match self {
            Format::U8 => (b[0] as f32 - 128.0) / 128.0,
            Format::S16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            Format::S32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0,
            Format::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
    /// A float in [-1, 1] as a sample, into `b`.
    fn put(self, v: f32, b: &mut [u8]) {
        let v = v.clamp(-1.0, 1.0);
        match self {
            Format::U8 => b[0] = (v * 127.0 + 128.0).round() as u8,
            Format::S16 => b.copy_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
            Format::S32 => b.copy_from_slice(&((v as f64 * 2147483647.0).round() as i32).to_le_bytes()),
            Format::F32 => b.copy_from_slice(&v.to_le_bytes()),
        }
    }
}

// The card's ranges.
const MAX_CHANNELS: u32 = 2;
const RATES: (u32, u32) = (8000, 192000);
const PERIOD_BYTES_RANGE: (u32, u32) = (64, 1 << 20);
const PERIODS_RANGE: (u32, u32) = (2, 1024);
const BUFFER_BYTES_MAX: u32 = 4 << 20;
// (At least 80 ms buffered: a translated program needs the slack more than
// a native one, and a host's output may take 20 ms at a time.)
const BUFFER_TIME_MIN: u32 = 80_000;

const STATE_OPEN: u32 = 0;
const STATE_SETUP: u32 = 1;
const STATE_PREPARED: u32 = 2;
const STATE_RUNNING: u32 = 3;
const STATE_XRUN: u32 = 4;
const STATE_DRAINING: u32 = 5;
const STATE_PAUSED: u32 = 6;

// MMAP, MMAP_VALID, INTERLEAVED, NONINTERLEAVED, PAUSE, PERFECT_DRAIN.
const INFO: u32 = 0x1 | 0x2 | 0x100 | 0x200 | 0x80000 | 0x40;

// ---- Hardware parameters and their refinement (the kernel's rules) ----

/// An interval of a parameter's values (struct snd_interval).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Interval {
    min: u32,
    max: u32,
    openmin: bool,
    openmax: bool,
    integer: bool,
    empty: bool,
}

impl Interval {
    fn any() -> Interval {
        Interval { min: 0, max: u32::MAX, openmin: false, openmax: false, integer: false, empty: false }
    }
    fn range(min: u32, max: u32) -> Interval {
        Interval { min, max, ..Interval::any() }
    }
    fn integer(mut self) -> Interval {
        self.integer = true;
        self
    }
    fn read(b: &[u8]) -> Interval {
        let w = |k: usize| u32::from_le_bytes(b[4 * k..4 * k + 4].try_into().unwrap());
        let f = w(2);
        Interval { min: w(0), max: w(1), openmin: f & 1 != 0, openmax: f & 2 != 0, integer: f & 4 != 0, empty: f & 8 != 0 }
    }
    fn write(&self, b: &mut [u8]) {
        b[0..4].copy_from_slice(&self.min.to_le_bytes());
        b[4..8].copy_from_slice(&self.max.to_le_bytes());
        let f = self.openmin as u32 | (self.openmax as u32) << 1 | (self.integer as u32) << 2 | (self.empty as u32) << 3;
        b[8..12].copy_from_slice(&f.to_le_bytes());
    }
    fn is_empty(&self) -> bool {
        self.empty || self.min > self.max || (self.min == self.max && (self.openmin || self.openmax))
    }
    fn single(&self) -> Option<u32> {
        (!self.is_empty() && (self.min == self.max || (self.min + 1 == self.max && (self.openmin || self.openmax)))).then(|| if self.openmin { self.max } else { self.min })
    }
    /// Narrowed to what `v` allows (snd_interval_refine): whether it
    /// changed; Err if nothing's left.
    fn refine(&mut self, v: &Interval) -> SysResult<bool> {
        if self.is_empty() || v.is_empty() {
            return Err(EINVAL);
        }
        let before = *self;
        if self.min < v.min {
            self.min = v.min;
            self.openmin = v.openmin;
        } else if self.min == v.min && !self.openmin && v.openmin {
            self.openmin = true;
        }
        if self.max > v.max {
            self.max = v.max;
            self.openmax = v.openmax;
        } else if self.max == v.max && !self.openmax && v.openmax {
            self.openmax = true;
        }
        if !self.integer && v.integer {
            self.integer = true;
        }
        if self.integer {
            if self.openmin {
                self.min = self.min.saturating_add(1);
                self.openmin = false;
            }
            if self.openmax {
                self.max = self.max.saturating_sub(1);
                self.openmax = false;
            }
        } else if !self.openmin && !self.openmax && self.min == self.max {
            self.integer = true;
        }
        if self.is_empty() {
            self.empty = true;
            return Err(EINVAL);
        }
        Ok(*self != before)
    }
}

/// a * b, saturating.
fn mul(a: &Interval, b: &Interval) -> Interval {
    Interval {
        min: a.min.saturating_mul(b.min),
        openmin: a.openmin || b.openmin,
        max: a.max.saturating_mul(b.max),
        openmax: a.openmax || b.openmax,
        integer: a.integer && b.integer,
        empty: false,
    }
}

fn div32(a: u32, b: u32) -> (u32, bool) {
    if b == 0 { (u32::MAX, false) } else { (a / b, a % b != 0) }
}

fn muldiv32(a: u32, b: u32, c: u32) -> (u32, bool) {
    if c == 0 {
        return (u32::MAX, false);
    }
    let n = a as u64 * b as u64;
    let (q, r) = (n / c as u64, n % c as u64);
    if q > u32::MAX as u64 { (u32::MAX, false) } else { (q as u32, r != 0) }
}

/// a / b.
fn div(a: &Interval, b: &Interval) -> Interval {
    let (min, r) = div32(a.min, b.max);
    let mut c = Interval { min, openmin: r || a.openmin || b.openmax, ..Interval::any() };
    if b.min > 0 {
        let (max, r) = div32(a.max, b.min);
        c.max = max;
        if r {
            c.max = c.max.saturating_add(1);
            c.openmax = true;
        } else {
            c.openmax = a.openmax || b.openmin;
        }
    }
    c
}

/// a * b / k.
fn muldivk(a: &Interval, b: &Interval, k: u32) -> Interval {
    let (min, r) = muldiv32(a.min, b.min, k);
    let (max, r2) = muldiv32(a.max, b.max, k);
    Interval { min, openmin: r || a.openmin || b.openmin, max: if r2 { max.saturating_add(1) } else { max }, openmax: r2 || a.openmax || b.openmax, integer: false, empty: false }
}

/// a * k / b.
fn mulkdiv(a: &Interval, k: u32, b: &Interval) -> Interval {
    let (min, r) = muldiv32(a.min, k, b.max);
    let mut c = Interval { min, openmin: r || a.openmin || b.openmax, ..Interval::any() };
    if b.min > 0 {
        let (max, r) = muldiv32(a.max, k, b.min);
        c.max = if r { max.saturating_add(1) } else { max };
        c.openmax = r || a.openmax || b.openmin;
    }
    c
}

/// struct snd_pcm_hw_params.
#[derive(Clone, Debug, PartialEq)]
struct HwParams {
    flags: u32,
    masks: [[u32; 8]; 3],
    intervals: [Interval; 12],
    rmask: u32,
    cmask: u32,
    info: u32,
    msbits: u32,
    rate_num: u32,
    rate_den: u32,
    fifo_size: u64,
}

impl HwParams {
    fn read(b: &[u8]) -> HwParams {
        let w = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let mut masks = [[0u32; 8]; 3];
        for (m, mask) in masks.iter_mut().enumerate() {
            for (k, word) in mask.iter_mut().enumerate() {
                *word = w(4 + 32 * m + 4 * k);
            }
        }
        // (Masks: 3 + 5 reserved, 32 bytes each, from byte 4; intervals:
        // 12 + 9 reserved, 12 bytes each.)
        let intervals = std::array::from_fn(|k| Interval::read(&b[260 + 12 * k..]));
        let at = 260 + 21 * 12;
        HwParams {
            flags: w(0),
            masks,
            intervals,
            rmask: w(at),
            cmask: w(at + 4),
            info: w(at + 8),
            msbits: w(at + 12),
            rate_num: w(at + 16),
            rate_den: w(at + 20),
            fifo_size: u64::from_le_bytes(b[at + 24..at + 32].try_into().unwrap()),
        }
    }
    fn write(&self, b: &mut [u8]) {
        let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0, self.flags);
        for (m, mask) in self.masks.iter().enumerate() {
            for (k, word) in mask.iter().enumerate() {
                put(4 + 32 * m + 4 * k, *word);
            }
        }
        let at = 260 + 21 * 12;
        for (v, at) in [self.rmask, self.cmask, self.info, self.msbits, self.rate_num, self.rate_den].into_iter().zip((at..).step_by(4)) {
            put(at, v);
        }
        for (k, i) in self.intervals.iter().enumerate() {
            i.write(&mut b[260 + 12 * k..]);
        }
        b[at + 24..at + 32].copy_from_slice(&self.fifo_size.to_le_bytes());
    }
    fn i(&self, p: usize) -> Interval {
        self.intervals[p - SAMPLE_BITS]
    }
    fn has(&self, m: usize, bit: u32) -> bool {
        bit < 256 && self.masks[m][(bit / 32) as usize] & (1 << (bit % 32)) != 0
    }
    /// Narrows a parameter's interval, noting it changed.
    fn refine(&mut self, p: usize, v: &Interval, changed: &mut u32) -> SysResult<()> {
        if self.intervals[p - SAMPLE_BITS].refine(v)? {
            *changed |= 1 << p;
        }
        Ok(())
    }
    fn refine_mask(&mut self, m: usize, allowed: &[u32; 8], changed: &mut u32) -> SysResult<()> {
        let mut any = false;
        for k in 0..8 {
            let v = self.masks[m][k] & allowed[k];
            if v != self.masks[m][k] {
                *changed |= 1 << m;
            }
            self.masks[m][k] = v;
            any |= v != 0;
        }
        if any { Ok(()) } else { Err(EINVAL) }
    }

    /// The card's constraints and the kernel's rules between parameters,
    /// applied until nothing changes (snd_pcm_hw_refine).
    fn refine_all(&mut self) -> SysResult<()> {
        let mut changed = 0u32;
        let mask_of = |bits: &[u32]| {
            let mut m = [0u32; 8];
            for &b in bits {
                m[(b / 32) as usize] |= 1 << (b % 32);
            }
            m
        };
        let access = [ACCESS_MMAP_INTERLEAVED, ACCESS_MMAP_NONINTERLEAVED, ACCESS_RW_INTERLEAVED, ACCESS_RW_NONINTERLEAVED];
        self.refine_mask(ACCESS, &mask_of(&access), &mut changed)?;
        self.refine_mask(FORMAT, &mask_of(&FORMATS.map(|f| f as u32)), &mut changed)?;
        self.refine_mask(SUBFORMAT, &mask_of(&[SUBFORMAT_STD]), &mut changed)?;
        self.refine(CHANNELS, &Interval::range(1, MAX_CHANNELS).integer(), &mut changed)?;
        self.refine(RATE, &Interval::range(RATES.0, RATES.1), &mut changed)?;
        self.refine(PERIOD_BYTES, &Interval::range(PERIOD_BYTES_RANGE.0, PERIOD_BYTES_RANGE.1), &mut changed)?;
        self.refine(PERIODS, &Interval::range(PERIODS_RANGE.0, PERIODS_RANGE.1).integer(), &mut changed)?;
        self.refine(BUFFER_BYTES, &Interval::range(1, BUFFER_BYTES_MAX).integer(), &mut changed)?;
        self.refine(BUFFER_TIME, &Interval::range(BUFFER_TIME_MIN, u32::MAX), &mut changed)?;
        for p in [SAMPLE_BITS, FRAME_BITS, BUFFER_SIZE, PERIOD_SIZE] {
            self.refine(p, &Interval::any().integer(), &mut changed)?;
        }
        self.refine(TICK_TIME, &Interval::any(), &mut changed)?;
        for _ in 0..64 {
            let mut now = 0u32;
            // Formats and their widths.
            let formats: Vec<Format> = FORMATS.into_iter().filter(|f| self.has(FORMAT, *f as u32)).collect();
            let bits = Interval::range(formats.iter().map(|f| f.bits()).min().ok_or(EINVAL)?, formats.iter().map(|f| f.bits()).max().unwrap()).integer();
            self.refine(SAMPLE_BITS, &bits, &mut now)?;
            let sb = self.i(SAMPLE_BITS);
            let fits = mask_of(&formats.iter().filter(|f| f.bits() >= sb.min && f.bits() <= sb.max).map(|f| *f as u32).collect::<Vec<_>>());
            self.refine_mask(FORMAT, &fits, &mut now)?;
            // Frames, bytes, sizes, times.
            let rules: [(usize, fn(&HwParams) -> Interval); 18] = [
                (SAMPLE_BITS, |s| div(&s.i(FRAME_BITS), &s.i(CHANNELS))),
                (FRAME_BITS, |s| mul(&s.i(SAMPLE_BITS), &s.i(CHANNELS))),
                (FRAME_BITS, |s| mulkdiv(&s.i(PERIOD_BYTES), 8, &s.i(PERIOD_SIZE))),
                (FRAME_BITS, |s| mulkdiv(&s.i(BUFFER_BYTES), 8, &s.i(BUFFER_SIZE))),
                (CHANNELS, |s| div(&s.i(FRAME_BITS), &s.i(SAMPLE_BITS))),
                (RATE, |s| mulkdiv(&s.i(PERIOD_SIZE), 1_000_000, &s.i(PERIOD_TIME))),
                (RATE, |s| mulkdiv(&s.i(BUFFER_SIZE), 1_000_000, &s.i(BUFFER_TIME))),
                (PERIODS, |s| div(&s.i(BUFFER_SIZE), &s.i(PERIOD_SIZE))),
                (PERIOD_SIZE, |s| div(&s.i(BUFFER_SIZE), &s.i(PERIODS))),
                (PERIOD_SIZE, |s| mulkdiv(&s.i(PERIOD_BYTES), 8, &s.i(FRAME_BITS))),
                (PERIOD_SIZE, |s| muldivk(&s.i(PERIOD_TIME), &s.i(RATE), 1_000_000)),
                (BUFFER_SIZE, |s| mul(&s.i(PERIOD_SIZE), &s.i(PERIODS))),
                (BUFFER_SIZE, |s| mulkdiv(&s.i(BUFFER_BYTES), 8, &s.i(FRAME_BITS))),
                (BUFFER_SIZE, |s| muldivk(&s.i(BUFFER_TIME), &s.i(RATE), 1_000_000)),
                (PERIOD_BYTES, |s| muldivk(&s.i(PERIOD_SIZE), &s.i(FRAME_BITS), 8)),
                (BUFFER_BYTES, |s| muldivk(&s.i(BUFFER_SIZE), &s.i(FRAME_BITS), 8)),
                (PERIOD_TIME, |s| mulkdiv(&s.i(PERIOD_SIZE), 1_000_000, &s.i(RATE))),
                (BUFFER_TIME, |s| mulkdiv(&s.i(BUFFER_SIZE), 1_000_000, &s.i(RATE))),
            ];
            for (p, rule) in rules {
                let v = rule(self);
                self.refine(p, &v, &mut now)?;
            }
            changed |= now;
            if now == 0 {
                break;
            }
        }
        self.cmask |= changed;
        self.info = INFO;
        if self.msbits == 0
            && let Some(b) = self.i(SAMPLE_BITS).single()
        {
            self.msbits = b;
        }
        if self.rate_den == 0
            && let Some(r) = self.i(RATE).single()
        {
            (self.rate_num, self.rate_den) = (r, 1);
        }
        self.fifo_size = 0;
        Ok(())
    }

    /// A value chosen for each parameter left open, as the kernel does on
    /// HW_PARAMS: the first for most, the largest buffer.
    fn choose(&mut self) -> SysResult<()> {
        self.refine_all()?;
        for m in [ACCESS, FORMAT, SUBFORMAT] {
            let first = (0..256).find(|&b| self.has(m, b)).ok_or(EINVAL)?;
            self.masks[m] = [0; 8];
            self.masks[m][(first / 32) as usize] = 1 << (first % 32);
            self.refine_all()?;
        }
        for (p, last) in [(CHANNELS, false), (RATE, false), (PERIOD_TIME, false), (PERIOD_SIZE, false), (PERIODS, false), (BUFFER_TIME, true), (BUFFER_SIZE, true)] {
            // (snd_interval_refine_first and _last: a single value stays;
            // an open end is kept where it still excludes.)
            let i = &mut self.intervals[p - SAMPLE_BITS];
            if i.is_empty() {
                return Err(EINVAL);
            }
            if i.single().is_some() {
                continue;
            }
            if last {
                let first = i.min;
                i.min = i.max - i.openmax as u32;
                i.openmin = i.openmin && i.min <= first;
            } else {
                let last = i.max;
                i.max = i.min + i.openmin as u32;
                i.openmax = i.openmax && i.max >= last;
            }
            self.refine_all()?;
        }
        Ok(())
    }
}

// ---- The card's streams and its mixer ----

/// A stream, playback or capture: its setup, its ring buffer, and where the
/// program and the hardware are in it (frames, wrapping at `boundary`).
struct Stream {
    capture: bool,
    state: u32,
    format: Format,
    channels: usize,
    rate: u32,
    interleaved: bool,
    buffer_size: u64,
    period_size: u64,
    boundary: u64,
    avail_min: u64,
    start_threshold: u64,
    stop_threshold: u64,
    ring: Vec<u8>,
    appl: u64,
    hw: u64,
    trigger: Instant,
    /// The resampler: the position between `prev` and `next` (frames as
    /// float stereo).
    phase: f64,
    prev: [f32; 2],
    next: [f32; 2],
    record: Option<std::fs::File>,
    /// What a capture stream captures instead (`WASM_ENV_AUDIO_SOURCE`:
    /// frames as the stream takes them, then silence).
    source: Option<std::io::BufReader<std::fs::File>>,
    /// The program's mapping of the data (memory-mapped access): its
    /// address and length, which committed frames are copied from.
    area: Option<(u64, u64)>,
    /// A stream of the runtime's own PulseAudio server's (pulse.rs) rather
    /// than a program's PCM: full, it drops what it captures; it has a
    /// volume of its own (left, right gains); and it counts its underruns.
    /// Run dry, it waits to be filled to its start threshold again if it
    /// `refills` (PulseAudio's prebuffering), else runs on through silence
    /// (frames written for the time it passed are dropped), as PulseAudio's
    /// streams without prebuffering do.
    held: bool,
    refills: bool,
    dry: bool,
    gain: [f32; 2],
    underruns: u64,
    overruns: u64,
}

impl Stream {
    fn frame_bytes(&self) -> usize {
        (self.format.bits() / 8) as usize * self.channels
    }
    /// Frames in the ring: written and not yet played (negative once the
    /// hardware has run past the program's pointer, an underrun playing on),
    /// or captured and not yet read.
    fn filled(&self) -> i64 {
        let (ahead, behind) = if self.capture { (self.hw, self.appl) } else { (self.appl, self.hw) };
        let d = (ahead + self.boundary - behind) % self.boundary;
        if d > self.boundary / 2 { d as i64 - self.boundary as i64 } else { d as i64 }
    }
    /// What the program can do: room to write, or frames to read.
    fn avail(&self) -> i64 {
        if self.capture { self.filled() } else { self.buffer_size as i64 - self.filled() }
    }
    fn running(&self) -> bool {
        self.state == STATE_RUNNING || self.state == STATE_DRAINING
    }
    /// Set up (`period` and `buffer` in frames): an empty ring, the
    /// kernel's defaults for the rest.
    fn configure(&mut self, format: Format, interleaved: bool, channels: u32, rate: u32, period: u64, buffer: u64) {
        self.format = format;
        self.interleaved = interleaved;
        self.channels = channels as usize;
        self.rate = rate;
        self.period_size = period;
        self.buffer_size = buffer;
        // (The kernel's boundary: the buffer size doubled while it fits.)
        let mut boundary = self.buffer_size;
        while boundary * 2 <= (i64::MAX as u64) / 2 - self.buffer_size {
            boundary *= 2;
        }
        self.boundary = boundary;
        self.avail_min = self.period_size;
        self.start_threshold = 1;
        self.stop_threshold = self.buffer_size;
        self.ring = vec![0; self.buffer_size as usize * self.frame_bytes()];
        self.appl = 0;
        self.hw = 0;
        self.state = STATE_SETUP;
        if self.capture {
            self.source = std::env::var_os("WASM_ENV_AUDIO_SOURCE").and_then(|p| std::fs::File::open(p).ok()).map(std::io::BufReader::new);
        } else {
            self.record = std::env::var_os("WASM_ENV_AUDIO_RECORD").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        }
        if std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some() {
            let way = if self.capture { "capture" } else { "playback" };
            eprintln!("wasm-env: sound: {way}, {:?}, {} channels, {} Hz, period {}, buffer {}", self.format, self.channels, self.rate, self.period_size, self.buffer_size);
        }
    }
    /// The `n` frames after the program's pointer, from its mapping of the
    /// data (laid out as the ring is) into the ring.
    fn copy_in(&mut self, data: &[u8], n: u64) -> SysResult<()> {
        let Some((addr, len)) = self.area else { return Ok(()) };
        let sb = (self.format.bits() / 8) as u64;
        let fb = self.frame_bytes() as u64;
        if len < self.buffer_size * fb {
            return Err(EINVAL);
        }
        for k in 0..n {
            let at = (self.appl + k) % self.buffer_size;
            if self.interleaved {
                let src = mem::slice(data, addr + at * fb, fb)?;
                self.ring[(at * fb) as usize..((at + 1) * fb) as usize].copy_from_slice(src);
            } else {
                for c in 0..self.channels as u64 {
                    let off = c * self.buffer_size * sb + at * sb;
                    let src = mem::slice(data, addr + off, sb)?;
                    self.ring[off as usize..(off + sb) as usize].copy_from_slice(src);
                }
            }
        }
        Ok(())
    }
    /// Captured frames, the `n` after the program's pointer, from the ring
    /// out to its mapping of the data.
    fn copy_out(&self, data: &mut [u8], n: u64) -> SysResult<()> {
        let Some((addr, len)) = self.area else { return Ok(()) };
        let sb = (self.format.bits() / 8) as u64;
        let fb = self.frame_bytes() as u64;
        if len < self.buffer_size * fb {
            return Err(EINVAL);
        }
        for k in 0..n {
            let at = (self.appl + k) % self.buffer_size;
            if self.interleaved {
                mem::write(data, addr + at * fb, &self.ring[(at * fb) as usize..((at + 1) * fb) as usize])?;
            } else {
                for c in 0..self.channels as u64 {
                    let off = c * self.buffer_size * sb + at * sb;
                    mem::write(data, addr + off, &self.ring[off as usize..(off + sb) as usize])?;
                }
            }
        }
        Ok(())
    }
    /// The next frame played, as float stereo (None: none written).
    fn take(&mut self) -> Option<[f32; 2]> {
        if self.filled() <= 0 {
            return None;
        }
        let fb = self.frame_bytes();
        let at = (self.hw % self.buffer_size) as usize;
        let sb = (self.format.bits() / 8) as usize;
        let sample = |c: usize| {
            let off = if self.interleaved { at * fb + c * sb } else { c * self.buffer_size as usize * sb + at * sb };
            self.format.sample(&self.ring[off..off + sb])
        };
        let l = sample(0);
        let r = if self.channels > 1 { sample(1) } else { l };
        if let Some(f) = &mut self.record {
            use std::io::Write;
            let bytes: Vec<u8> = if self.interleaved {
                self.ring[at * fb..at * fb + fb].to_vec()
            } else {
                (0..self.channels).flat_map(|c| self.ring[c * self.buffer_size as usize * sb + at * sb..][..sb].to_vec()).collect()
            };
            let _ = f.write_all(&bytes);
        }
        self.hw = (self.hw + 1) % self.boundary;
        Some([l * self.gain[0], r * self.gain[1]])
    }
    /// Adds `out.len() / 2` frames of float stereo at `rate` into `out`.
    fn mix(&mut self, out: &mut [f32], rate: u32) {
        let step = self.rate as f64 / rate as f64;
        for frame in out.chunks_exact_mut(2) {
            while self.phase >= 1.0 {
                self.prev = self.next;
                match self.take() {
                    Some(f) => {
                        self.next = f;
                        self.dry = false;
                    }
                    None => {
                        // Nothing left: drained, or an underrun (the
                        // hardware runs on, through silence, until the
                        // stop threshold).
                        if self.state == STATE_DRAINING {
                            self.state = STATE_SETUP;
                            return;
                        }
                        // (A server's stream: an underrun counted once; it
                        // waits to be filled again, or runs on.)
                        if self.held {
                            if !self.dry {
                                self.dry = true;
                                self.underruns += 1;
                            }
                            if self.refills {
                                self.state = STATE_PREPARED;
                                self.phase = 1.0;
                                self.next = [0.0; 2];
                                return;
                            }
                        }
                        self.next = [0.0; 2];
                        self.hw = (self.hw + 1) % self.boundary;
                        if self.avail() >= self.stop_threshold as i64 {
                            self.state = STATE_XRUN;
                            return;
                        }
                    }
                }
                self.phase -= 1.0;
            }
            let t = self.phase as f32;
            frame[0] += self.prev[0] + (self.next[0] - self.prev[0]) * t;
            frame[1] += self.prev[1] + (self.next[1] - self.prev[1]) * t;
            self.phase += step;
        }
    }
    /// Captures `input` (float stereo frames at `rate`), resampled to the
    /// stream's rate, into the ring, until it overruns.
    fn capture(&mut self, input: &[f32], rate: u32) {
        let step = rate as f64 / self.rate as f64;
        for f in input.chunks_exact(2) {
            self.prev = self.next;
            self.next = [f[0], f[1]];
            while self.phase < 1.0 {
                let t = self.phase as f32;
                let frame = [self.prev[0] + (self.next[0] - self.prev[0]) * t, self.prev[1] + (self.next[1] - self.prev[1]) * t];
                self.put(frame);
                if self.state != STATE_RUNNING {
                    return;
                }
                self.phase += step;
            }
            self.phase -= 1.0;
        }
    }
    /// One captured frame into the ring (or the source's, if there is
    /// one): an overrun once as many as the stop threshold wait.
    fn put(&mut self, [l, r]: [f32; 2]) {
        // (A server's stream, full: what it captures is dropped.)
        if self.held && self.avail() >= self.buffer_size as i64 {
            self.overruns += 1;
            return;
        }
        let (l, r) = (l * self.gain[0], r * self.gain[1]);
        let fb = self.frame_bytes();
        let sb = (self.format.bits() / 8) as usize;
        let mut frame = vec![0u8; fb];
        if let Some(src) = &mut self.source {
            use std::io::Read;
            // (Past its end, silence.)
            let mut got = 0;
            while got < fb {
                match src.read(&mut frame[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
            if got < fb {
                frame.fill(0);
                if self.format == Format::U8 {
                    frame.fill(0x80);
                }
            }
        } else {
            let values = if self.channels == 1 { vec![0.5 * (l + r)] } else { vec![l, r] };
            for (c, v) in values.into_iter().enumerate() {
                self.format.put(v, &mut frame[c * sb..(c + 1) * sb]);
            }
        }
        let at = (self.hw % self.buffer_size) as usize;
        if self.interleaved {
            self.ring[at * fb..(at + 1) * fb].copy_from_slice(&frame);
        } else {
            for c in 0..self.channels {
                let off = c * self.buffer_size as usize * sb + at * sb;
                self.ring[off..off + sb].copy_from_slice(&frame[c * sb..(c + 1) * sb]);
            }
        }
        self.hw = (self.hw + 1) % self.boundary;
        if self.avail() >= self.stop_threshold as i64 {
            self.state = STATE_XRUN;
        }
    }
}

/// The card: its streams (for the mixer and the source), its sink and
/// source started, and its mixer controls.
struct Card {
    streams: Mutex<Vec<Weak<Pcm>>>,
    started: OnceLock<()>,
    listening: OnceLock<()>,
    /// Its mixer controls: the master volume (left, right: 0 to
    /// VOLUME_MAX) and switch.
    volume: Mutex<([i64; 2], bool)>,
}

static CARD: Card = Card { streams: Mutex::new(Vec::new()), started: OnceLock::new(), listening: OnceLock::new(), volume: Mutex::new(([VOLUME_MAX; 2], true)) };

/// The master volume's steps: half a decibel each, from -50 dB (0 is
/// silence) to 0 dB.
const VOLUME_MAX: i64 = 100;
const VOLUME_DB_MIN: i64 = -5000; // in 0.01 dB
const VOLUME_DB_STEP: i64 = 50;

/// A volume step's gain.
fn gain(step: i64) -> f32 {
    if step <= 0 {
        return 0.0;
    }
    let db = (VOLUME_DB_MIN + step.min(VOLUME_MAX) * VOLUME_DB_STEP) as f32 / 100.0;
    10f32.powf(db / 20.0)
}

/// Where the card's sound goes (`--audio`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sink {
    /// The host's default output, else none.
    Host,
    /// None: the mixer on a clock of its own.
    Null,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Chooses the sink (once, before the first stream starts).
pub fn set_sink(s: Sink) {
    let _ = SINK.set(s);
}

/// The rate the mixer runs at (the null sink's; the host's own).
const NULL_RATE: u32 = 48000;

/// Mixes every running stream into `out` (float stereo frames at `rate`),
/// waking writers and drains.
pub fn mix(out: &mut [f32], rate: u32) {
    out.fill(0.0);
    for p in streams() {
        let mut s = p.stream.lock().unwrap();
        if !s.capture && s.running() {
            s.mix(out, rate);
            drop(s);
            p.wake.notify_all();
        }
    }
    turn_down(out, *CARD.volume.lock().unwrap());
    for v in out.iter_mut() {
        *v = v.clamp(-1.0, 1.0);
    }
}

/// Applies the master volume (left, right) and switch to mixed frames.
fn turn_down(out: &mut [f32], (volume, on): ([i64; 2], bool)) {
    let gains = if on { [gain(volume[0]), gain(volume[1])] } else { [0.0; 2] };
    for frame in out.chunks_exact_mut(2) {
        frame[0] *= gains[0];
        frame[1] *= gains[1];
    }
}

/// Captures `input` (float stereo frames at `rate`, from the host's input
/// or silence) into every running capture stream, waking readers.
pub fn capture(input: &[f32], rate: u32) {
    for p in streams() {
        let mut s = p.stream.lock().unwrap();
        if s.capture && s.state == STATE_RUNNING {
            s.capture(input, rate);
            drop(s);
            p.wake.notify_all();
        }
    }
}

/// The card's open streams.
fn streams() -> Vec<Arc<Pcm>> {
    let mut s = CARD.streams.lock().unwrap();
    s.retain(|w| w.strong_count() > 0);
    s.iter().filter_map(Weak::upgrade).collect()
}

/// Starts the sink, when the first stream needs it.
fn start_sink() {
    CARD.started.get_or_init(|| {
        let sink = *SINK.get_or_init(|| Sink::Host);
        if sink == Sink::Host && crate::audio_host::start() {
            return;
        }
        on_a_clock("wasm-env audio", |buf| mix(buf, NULL_RATE));
    });
}

/// Starts the source, when the first capture stream needs it: the host's
/// default input, else silence on a clock of its own.
fn start_source() {
    CARD.listening.get_or_init(|| {
        let sink = *SINK.get_or_init(|| Sink::Host);
        if sink == Sink::Host && crate::audio_host::start_input() {
            return;
        }
        on_a_clock("wasm-env audio in", |buf| {
            buf.fill(0.0);
            capture(buf, NULL_RATE);
        });
    });
}

/// Calls `f` on a thread of its own with float stereo frames at NULL_RATE,
/// as many as have come due by the clock (a few milliseconds' at a time).
fn on_a_clock(name: &str, f: fn(&mut [f32])) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let begun = Instant::now();
            let mut done = 0u64;
            let mut buf = Vec::new();
            loop {
                std::thread::sleep(Duration::from_millis(5));
                let due = (begun.elapsed().as_secs_f64() * NULL_RATE as f64) as u64;
                if due > done {
                    buf.resize(2 * (due - done) as usize, 0.0);
                    f(&mut buf);
                    done = due;
                }
            }
        })
        .expect("audio thread");
}

// ---- The devices ----

/// A PCM device (`/dev/snd/pcmC0D0p`, playback, or `pcmC0D0c`, capture), one
/// per open.
pub struct Pcm {
    stream: Mutex<Stream>,
    wake: Condvar,
}

impl Pcm {
    pub fn open(capture: bool) -> Arc<Pcm> {
        let p = Arc::new(Pcm {
            stream: Mutex::new(Stream {
                capture,
                state: STATE_OPEN,
                format: Format::S16,
                channels: 2,
                rate: 48000,
                interleaved: true,
                buffer_size: 0,
                period_size: 0,
                boundary: 1,
                avail_min: 1,
                start_threshold: 1,
                stop_threshold: 0,
                ring: Vec::new(),
                appl: 0,
                hw: 0,
                trigger: Instant::now(),
                phase: 1.0,
                prev: [0.0; 2],
                next: [0.0; 2],
                record: None,
                source: None,
                area: None,
                held: false,
                refills: true,
                dry: false,
                gain: [1.0; 2],
                underruns: 0,
                overruns: 0,
            }),
            wake: Condvar::new(),
        });
        CARD.streams.lock().unwrap().push(Arc::downgrade(&p));
        p
    }

    /// The program mapped the data (mmap of the PCM): where.
    pub fn mapped(&self, addr: u64, len: u64) {
        self.stream.lock().unwrap().area = Some((addr, len));
    }

    /// Which device it is.
    pub fn device(&self) -> SoundDevice {
        if self.stream.lock().unwrap().capture { SoundDevice::Capture } else { SoundDevice::Playback }
    }

    /// For poll: frames to read, room to write (as many as avail_min), or
    /// an error state.
    pub fn readiness(&self) -> (bool, bool, bool) {
        let s = self.stream.lock().unwrap();
        let err = s.state == STATE_XRUN || s.state == STATE_OPEN;
        let ready = s.state != STATE_OPEN && s.avail() >= s.avail_min as i64;
        (ready && s.capture, ready && !s.capture, err)
    }

    /// An ioctl: `interrupted` says when a wait should end (a signal).
    pub fn ioctl(self: &Arc<Self>, data: &mut [u8], req: u32, arg: u64, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<i64> {
        let word = |data: &mut [u8], v: u32| mem::write(data, arg, &v.to_le_bytes()).map(|_| 0);
        match req {
            PCM_PVERSION => word(data, PCM_VERSION),
            PCM_INFO => {
                let mut b = vec![0u8; PCM_INFO_SIZE];
                pcm_info(&mut b, self.stream.lock().unwrap().capture);
                mem::write(data, arg, &b).map(|_| 0)
            }
            PCM_TSTAMP | PCM_TTSTAMP | PCM_USER_PVERSION | PCM_LINK | PCM_UNLINK | PCM_HWSYNC => Ok(0),
            PCM_HW_REFINE | PCM_HW_PARAMS => {
                let mut p = HwParams::read(mem::slice(data, arg, HW_PARAMS_SIZE as u64)?);
                if req == PCM_HW_REFINE {
                    p.refine_all()?;
                } else {
                    p.choose()?;
                    self.setup(&p)?;
                }
                let mut b = mem::slice(data, arg, HW_PARAMS_SIZE as u64)?.to_vec();
                p.write(&mut b);
                mem::write(data, arg, &b).map(|_| 0)
            }
            PCM_HW_FREE => {
                let mut s = self.stream.lock().unwrap();
                if s.running() {
                    return Err(EBADFD);
                }
                s.state = STATE_OPEN;
                s.ring = Vec::new();
                Ok(0)
            }
            PCM_SW_PARAMS => {
                let b = mem::slice(data, arg, SW_PARAMS_SIZE as u64)?.to_vec();
                let q = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
                let mut s = self.stream.lock().unwrap();
                if s.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                s.avail_min = q(16).max(1);
                s.start_threshold = q(32);
                s.stop_threshold = q(40);
                let mut out = b.clone();
                out[64..72].copy_from_slice(&s.boundary.to_le_bytes());
                mem::write(data, arg, &out).map(|_| 0)
            }
            PCM_STATUS | PCM_STATUS_EXT => {
                let s = self.stream.lock().unwrap();
                let mut b = vec![0u8; STATUS_SIZE];
                b[0..4].copy_from_slice(&s.state.to_le_bytes());
                let now = timespec(Instant::now());
                b[8..24].copy_from_slice(&timespec(s.trigger));
                b[24..40].copy_from_slice(&now);
                b[40..48].copy_from_slice(&s.appl.to_le_bytes());
                b[48..56].copy_from_slice(&s.hw.to_le_bytes());
                b[56..64].copy_from_slice(&s.filled().max(0).to_le_bytes());
                b[64..72].copy_from_slice(&s.avail().max(0).to_le_bytes());
                b[72..80].copy_from_slice(&s.avail().max(0).to_le_bytes());
                b[112..128].copy_from_slice(&now);
                mem::write(data, arg, &b).map(|_| 0)
            }
            PCM_DELAY => {
                let s = self.stream.lock().unwrap();
                if s.state == STATE_XRUN {
                    return Err(EPIPE);
                }
                mem::write(data, arg, &s.filled().to_le_bytes()).map(|_| 0)
            }
            PCM_SYNC_PTR => {
                let mut b = mem::slice(data, arg, SYNC_PTR_SIZE as u64)?.to_vec();
                let flags = u32::from_le_bytes(b[0..4].try_into().unwrap());
                let mut s = self.stream.lock().unwrap();
                // (The program's pointer and avail_min, unless it asks for
                // ours.)
                if flags & 2 == 0 {
                    let appl = u64::from_le_bytes(b[72..80].try_into().unwrap());
                    if s.state != STATE_OPEN && s.capture {
                        // (Frames the program read from its mapping: done
                        // with.)
                        s.appl = appl % s.boundary;
                    } else if s.state != STATE_OPEN {
                        // (Frames the program committed in its mapping of
                        // the data: copied in.)
                        let appl = appl % s.boundary;
                        let n = (appl + s.boundary - s.appl) % s.boundary;
                        if n > 0 && n <= s.buffer_size {
                            s.copy_in(data, n)?;
                        }
                        s.appl = appl;
                    }
                }
                if flags & 4 == 0 {
                    s.avail_min = u64::from_le_bytes(b[80..88].try_into().unwrap()).max(1);
                }
                // (Frames captured since: out to the program's mapping.)
                if s.capture && s.state != STATE_OPEN {
                    let n = s.filled().clamp(0, s.buffer_size as i64) as u64;
                    s.copy_out(data, n)?;
                }
                b[8..12].copy_from_slice(&s.state.to_le_bytes());
                b[16..24].copy_from_slice(&s.hw.to_le_bytes());
                b[24..40].copy_from_slice(&timespec(Instant::now()));
                b[72..80].copy_from_slice(&s.appl.to_le_bytes());
                b[80..88].copy_from_slice(&s.avail_min.to_le_bytes());
                mem::write(data, arg, &b).map(|_| 0)
            }
            // Where a channel's samples are in the mapping: one area, the
            // frames interleaved or the channels one after another.
            PCM_CHANNEL_INFO => {
                let mut b = mem::slice(data, arg, CHANNEL_INFO_SIZE as u64)?.to_vec();
                let c = u32::from_le_bytes(b[0..4].try_into().unwrap()) as u64;
                let s = self.stream.lock().unwrap();
                if s.state == STATE_OPEN || c >= s.channels as u64 {
                    return Err(EINVAL);
                }
                let bits = s.format.bits() as u64;
                let (first, step) = if s.interleaved { (c * bits, bits * s.channels as u64) } else { (c * bits * s.buffer_size, bits) };
                b[8..16].copy_from_slice(&0u64.to_le_bytes());
                b[16..20].copy_from_slice(&(first as u32).to_le_bytes());
                b[20..24].copy_from_slice(&(step as u32).to_le_bytes());
                mem::write(data, arg, &b).map(|_| 0)
            }
            PCM_PREPARE | PCM_RESET => {
                let mut s = self.stream.lock().unwrap();
                if s.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                if req == PCM_PREPARE || !s.running() {
                    s.state = STATE_PREPARED;
                }
                s.appl = 0;
                s.hw = 0;
                s.phase = 1.0;
                s.prev = [0.0; 2];
                s.next = [0.0; 2];
                Ok(0)
            }
            PCM_START => {
                let mut s = self.stream.lock().unwrap();
                if s.state != STATE_PREPARED {
                    return Err(EBADFD);
                }
                // (Nothing to play: an underrun at once, so not started, as
                // the kernel has it; the first write starts it.)
                if !s.capture && s.filled() <= 0 && s.stop_threshold < s.boundary {
                    return Err(EPIPE);
                }
                self.start(&mut s);
                Ok(0)
            }
            PCM_DROP => {
                let mut s = self.stream.lock().unwrap();
                if s.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                s.state = STATE_SETUP;
                drop(s);
                self.wake.notify_all();
                Ok(0)
            }
            PCM_DRAIN => {
                let mut s = self.stream.lock().unwrap();
                // (Capture stops at once; what it captured stays to read.)
                if s.capture {
                    if s.state == STATE_OPEN {
                        return Err(EBADFD);
                    }
                    s.state = STATE_SETUP;
                    drop(s);
                    self.wake.notify_all();
                    return Ok(0);
                }
                match s.state {
                    STATE_OPEN => return Err(EBADFD),
                    STATE_PREPARED if s.filled() > 0 => self.start(&mut s),
                    STATE_PREPARED | STATE_SETUP | STATE_XRUN => {
                        s.state = STATE_SETUP;
                        return Ok(0);
                    }
                    _ => {}
                }
                s.state = STATE_DRAINING;
                if nonblock {
                    return Err(EAGAIN);
                }
                while s.state == STATE_DRAINING {
                    if interrupted() {
                        return Err(EINTR);
                    }
                    s = self.wake.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
                }
                Ok(0)
            }
            PCM_PAUSE => {
                let on = u32::from_le_bytes(mem::slice(data, arg, 4)?.try_into().unwrap()) != 0;
                let mut s = self.stream.lock().unwrap();
                match (on, s.state) {
                    (true, STATE_RUNNING) => s.state = STATE_PAUSED,
                    (false, STATE_PAUSED) => s.state = STATE_RUNNING,
                    _ => return Err(EBADFD),
                }
                Ok(0)
            }
            PCM_REWIND | PCM_FORWARD => mem::write(data, arg, &0u64.to_le_bytes()).map(|_| 0),
            // (Never suspended: nothing to resume.)
            PCM_RESUME => Err(ENOSYS),
            PCM_XRUN => {
                let mut s = self.stream.lock().unwrap();
                s.state = STATE_XRUN;
                Ok(0)
            }
            PCM_WRITEI | PCM_WRITEN | PCM_READI | PCM_READN => {
                let x = mem::slice(data, arg, XFER_SIZE as u64)?.to_vec();
                let buf = u64::from_le_bytes(x[8..16].try_into().unwrap());
                let frames = u64::from_le_bytes(x[16..24].try_into().unwrap());
                let reading = req == PCM_READI || req == PCM_READN;
                if reading != self.stream.lock().unwrap().capture {
                    return Err(EINVAL);
                }
                let separate = req == PCM_WRITEN || req == PCM_READN;
                let n = self.transfer(data, buf, frames, separate, nonblock, interrupted)?;
                let mut out = x;
                out[0..8].copy_from_slice(&(n as i64).to_le_bytes());
                mem::write(data, arg, &out).map(|_| 0)
            }
            _ => Err(ENOTTY),
        }
    }

    /// The stream set up as the parameters chose.
    fn setup(&self, p: &HwParams) -> SysResult<()> {
        let one = |k| p.i(k).single().ok_or(EINVAL);
        let format = Format::of((0..256).find(|&b| p.has(FORMAT, b)).ok_or(EINVAL)?).ok_or(EINVAL)?;
        let interleaved = p.has(ACCESS, ACCESS_RW_INTERLEAVED) || p.has(ACCESS, ACCESS_MMAP_INTERLEAVED);
        let (channels, rate, period, buffer) = (one(CHANNELS)?, one(RATE)?, one(PERIOD_SIZE)?, one(BUFFER_SIZE)?);
        let mut s = self.stream.lock().unwrap();
        if s.running() {
            return Err(EBADFD);
        }
        s.configure(format, interleaved, channels, rate, period as u64, buffer as u64);
        Ok(())
    }

    fn start(&self, s: &mut Stream) {
        s.state = STATE_RUNNING;
        s.trigger = Instant::now();
        if s.capture {
            // (From silence: the first frame captured is before the input's.)
            s.phase = 0.0;
            s.next = [0.0; 2];
            start_source();
        } else {
            s.phase = 1.0;
            start_sink();
        }
    }

    /// `frames` frames between the program's memory at `buf` (interleaved,
    /// or an array of per-channel pointers) and the ring, written or read,
    /// waiting for room or frames unless `nonblock`; how many.
    fn transfer(&self, data: &mut [u8], buf: u64, frames: u64, separate: bool, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<u64> {
        let mut s = self.stream.lock().unwrap();
        if separate == s.interleaved {
            return Err(EINVAL);
        }
        let channels = s.channels;
        let pointers: Vec<u64> = if separate {
            mem::slice(data, buf, 8 * channels as u64)?.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
        } else {
            Vec::new()
        };
        let mut done = 0u64;
        while done < frames {
            match s.state {
                STATE_XRUN => return if done > 0 { Ok(done) } else { Err(EPIPE) },
                STATE_PREPARED | STATE_RUNNING | STATE_PAUSED => {}
                STATE_SETUP | STATE_OPEN | STATE_DRAINING => return if done > 0 { Ok(done) } else { Err(EBADFD) },
                _ => return Err(EBADFD),
            }
            // (Reading starts capture.)
            if s.capture && s.state == STATE_PREPARED {
                self.start(&mut s);
            }
            let avail = s.avail().max(0) as u64;
            if avail == 0 {
                if nonblock {
                    return if done > 0 { Ok(done) } else { Err(EAGAIN) };
                }
                if interrupted() {
                    return if done > 0 { Ok(done) } else { Err(EINTR) };
                }
                // (A stream that can't start would wait forever.)
                if s.state == STATE_PREPARED {
                    self.start(&mut s);
                }
                s = self.wake.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
                continue;
            }
            let n = avail.min(frames - done);
            let fb = s.frame_bytes();
            let sb = (s.format.bits() / 8) as usize;
            for k in 0..n {
                let at = (s.appl % s.buffer_size) as usize;
                if separate {
                    for (c, &p) in pointers.iter().enumerate() {
                        let addr = p + (done + k) * sb as u64;
                        let off = c * s.buffer_size as usize * sb + at * sb;
                        if s.capture {
                            mem::write(data, addr, &s.ring[off..off + sb])?;
                        } else {
                            let src = mem::slice(data, addr, sb as u64)?;
                            s.ring[off..off + sb].copy_from_slice(src);
                        }
                    }
                } else {
                    let addr = buf + (done + k) * fb as u64;
                    if s.capture {
                        mem::write(data, addr, &s.ring[at * fb..at * fb + fb])?;
                    } else {
                        let src = mem::slice(data, addr, fb as u64)?;
                        s.ring[at * fb..at * fb + fb].copy_from_slice(src);
                    }
                }
                s.appl = (s.appl + 1) % s.boundary;
            }
            done += n;
            if !s.capture && s.state == STATE_PREPARED && s.filled() as u64 >= s.start_threshold.max(1) {
                self.start(&mut s);
            }
        }
        Ok(done)
    }
}

// ---- For the runtime's PulseAudio server (pulse.rs) ----

/// The sample formats a server's stream takes (the card's own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sample {
    U8,
    S16,
    S32,
    F32,
}

/// Where a server's stream is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Position {
    /// Frames in the ring: written and not yet played (negative: run on
    /// past what was written), or captured and not yet read.
    pub filled: i64,
    /// Playing (or capturing), and not waiting to be filled.
    pub running: bool,
    /// Draining, and not yet drained.
    pub draining: bool,
    pub underruns: u64,
}

impl Pcm {
    /// A stream of the server's: `channels` (one or two) interleaved
    /// samples at `rate`, a ring of `buffer` frames, playback starting once
    /// `start` frames are written (capture at once).
    pub fn server(capture: bool, sample: Sample, channels: u32, rate: u32, buffer: u64, start: u64) -> Arc<Pcm> {
        let p = Pcm::open(capture);
        {
            let mut s = p.stream.lock().unwrap();
            let format = match sample {
                Sample::U8 => Format::U8,
                Sample::S16 => Format::S16,
                Sample::S32 => Format::S32,
                Sample::F32 => Format::F32,
            };
            s.configure(format, true, channels, rate.clamp(RATES.0, RATES.1), (buffer / 4).max(1), buffer.max(2));
            s.held = true;
            s.start_threshold = start.clamp(1, s.buffer_size);
            s.refills = start > 0;
            s.stop_threshold = s.boundary;
            s.state = STATE_PREPARED;
            if capture {
                p.start(&mut s);
            }
        }
        p
    }

    /// Frames at the end of the ring, from `bytes` (whole frames, as many
    /// as there's room for): how many bytes it took. Playback starts at its
    /// start threshold (unless paused).
    pub fn put(&self, bytes: &[u8]) -> usize {
        let mut s = self.stream.lock().unwrap();
        if s.state == STATE_SETUP {
            s.state = STATE_PREPARED;
        }
        let fb = s.frame_bytes();
        let frames = bytes.len() / fb;
        // (Frames for the time a stream ran on through: passed, dropped.)
        let late = ((-s.filled()).max(0) as usize).min(frames);
        s.appl = (s.appl + late as u64) % s.boundary;
        let n = (frames - late).min(s.avail().max(0) as usize);
        for k in late..late + n {
            let at = (s.appl % s.buffer_size) as usize;
            s.ring[at * fb..(at + 1) * fb].copy_from_slice(&bytes[k * fb..(k + 1) * fb]);
            s.appl = (s.appl + 1) % s.boundary;
        }
        if s.state == STATE_PREPARED && s.filled() > 0 && s.filled() as u64 >= s.start_threshold {
            self.start(&mut s);
        }
        (late + n) * fb
    }

    /// Captured frames, up to `max` bytes (whole frames), out of the ring.
    pub fn get(&self, max: usize) -> Vec<u8> {
        let mut s = self.stream.lock().unwrap();
        let fb = s.frame_bytes();
        let n = (max / fb).min(s.avail().max(0) as usize);
        let mut out = Vec::with_capacity(n * fb);
        for _ in 0..n {
            let at = (s.appl % s.buffer_size) as usize;
            out.extend_from_slice(&s.ring[at * fb..(at + 1) * fb]);
            s.appl = (s.appl + 1) % s.boundary;
        }
        out
    }

    pub fn position(&self) -> Position {
        let s = self.stream.lock().unwrap();
        Position {
            filled: s.filled(),
            running: s.running(),
            draining: s.state == STATE_DRAINING,
            underruns: s.underruns,
        }
    }

    /// Its own volume: left and right gains.
    pub fn set_gain(&self, gain: [f32; 2]) {
        self.stream.lock().unwrap().gain = gain;
    }

    /// How many frames playback waits for before it starts.
    pub fn set_start(&self, frames: u64) {
        let mut s = self.stream.lock().unwrap();
        s.start_threshold = frames.clamp(1, s.buffer_size);
        s.refills = frames > 0;
    }

    /// Paused (corked) or not. Unpaused, playback runs if it's filled to
    /// its start threshold, else waits to be.
    pub fn pause(&self, on: bool) {
        let mut s = self.stream.lock().unwrap();
        if on {
            if s.state != STATE_SETUP {
                s.state = STATE_PAUSED;
            }
        } else if s.state == STATE_PAUSED {
            s.state = STATE_PREPARED;
            if s.capture || (s.filled() > 0 && s.filled() as u64 >= s.start_threshold) {
                self.start(&mut s);
            }
        }
    }

    /// Everything in the ring dropped.
    pub fn flush(&self) {
        let mut s = self.stream.lock().unwrap();
        s.appl = s.hw;
        if !s.capture && s.state == STATE_RUNNING {
            s.state = STATE_PREPARED;
        }
    }

    /// Playback started now, whatever is in it (if it isn't paused).
    pub fn trigger(&self) {
        let mut s = self.stream.lock().unwrap();
        if s.state == STATE_PREPARED && s.filled() > 0 {
            self.start(&mut s);
        }
    }

    /// Playback plays what it has and stops (position().draining until it
    /// has).
    pub fn drain(&self) {
        let mut s = self.stream.lock().unwrap();
        if s.filled() <= 0 {
            s.state = STATE_SETUP;
            return;
        }
        if s.state == STATE_PREPARED {
            self.start(&mut s);
        }
        if s.state == STATE_RUNNING {
            s.state = STATE_DRAINING;
        }
    }
}

/// The master volume (left, right: 0 to 100, half-decibel steps from
/// -50 dB) and switch.
pub fn master() -> ([i64; 2], bool) {
    *CARD.volume.lock().unwrap()
}

pub fn set_master(volume: Option<[i64; 2]>, on: Option<bool>) {
    let mut v = CARD.volume.lock().unwrap();
    if let Some(volume) = volume {
        v.0 = volume.map(|x| x.clamp(0, VOLUME_MAX));
    }
    if let Some(on) = on {
        v.1 = on;
    }
}

/// A timespec of an instant (since the runtime's start: a monotonic clock).
fn timespec(t: Instant) -> [u8; 16] {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let d = t.saturating_duration_since(*EPOCH.get_or_init(Instant::now));
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&(d.as_secs() as i64).to_le_bytes());
    b[8..16].copy_from_slice(&(d.subsec_nanos() as i64).to_le_bytes());
    b
}

/// struct snd_pcm_info for the card's one PCM, playback or capture.
fn pcm_info(b: &mut [u8], capture: bool) {
    b[8..12].copy_from_slice(&(capture as u32).to_le_bytes()); // stream
    b[12..16].copy_from_slice(&0i32.to_le_bytes()); // card
    let name = b"wasm-env";
    b[16..16 + name.len()].copy_from_slice(name); // id
    b[80..80 + name.len()].copy_from_slice(name); // name
    let sub = b"subdevice #0";
    b[160..160 + sub.len()].copy_from_slice(sub); // subname
    b[200..204].copy_from_slice(&1u32.to_le_bytes()); // subdevices_count
    b[204..208].copy_from_slice(&1u32.to_le_bytes()); // subdevices_avail
}

/// A mixer control of the card's.
struct Control {
    numid: u32,
    name: &'static str,
    /// SNDRV_CTL_ELEM_TYPE_: 1 boolean, 2 integer.
    kind: u32,
    access: u32,
    count: u32,
    max: i64,
}

impl Control {
    /// Its snd_ctl_elem_id.
    fn id(&self) -> [u8; ELEM_ID_SIZE] {
        let mut b = [0u8; ELEM_ID_SIZE];
        b[0..4].copy_from_slice(&self.numid.to_le_bytes());
        b[4..8].copy_from_slice(&2u32.to_le_bytes()); // SNDRV_CTL_ELEM_IFACE_MIXER
        b[16..16 + self.name.len()].copy_from_slice(self.name.as_bytes());
        b
    }
}

/// The card's controls: its master volume (with a dB scale) and switch.
static CONTROLS: [Control; 2] = [
    Control { numid: 1, name: "Master Playback Volume", kind: 2, access: 0x3 | 0x10, count: 2, max: VOLUME_MAX },
    Control { numid: 2, name: "Master Playback Switch", kind: 1, access: 0x3, count: 1, max: 1 },
];

/// The control an snd_ctl_elem_id names: by its numid, or (numid 0) by its
/// interface, name and index.
fn control(id: &[u8]) -> SysResult<&'static Control> {
    let numid = u32::from_le_bytes(id[0..4].try_into().unwrap());
    let found = if numid != 0 {
        CONTROLS.iter().find(|c| c.numid == numid)
    } else {
        let name = &id[16..60];
        let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
        let iface = u32::from_le_bytes(id[4..8].try_into().unwrap());
        let index = u32::from_le_bytes(id[60..64].try_into().unwrap());
        CONTROLS.iter().find(|c| iface == 2 && index == 0 && c.name.as_bytes() == name)
    };
    found.ok_or(ENOENT)
}

/// The control device (`/dev/snd/controlC0`).
pub fn control_ioctl(data: &mut [u8], req: u32, arg: u64) -> SysResult<i64> {
    let int = |data: &mut [u8], v: i32| mem::write(data, arg, &v.to_le_bytes()).map(|_| 0);
    match req {
        CTL_PVERSION => int(data, CTL_VERSION as i32),
        CTL_CARD_INFO => {
            let mut b = vec![0u8; CARD_INFO_SIZE];
            let put = |b: &mut [u8], at: usize, s: &[u8]| b[at..at + s.len()].copy_from_slice(s);
            put(&mut b, 8, b"wasmenv"); // id
            put(&mut b, 24, b"wasm-env"); // driver
            put(&mut b, 40, b"wasm-env"); // name
            put(&mut b, 72, b"wasm-env's sound card"); // longname
            put(&mut b, 168, b"wasm-env"); // mixername
            mem::write(data, arg, &b).map(|_| 0)
        }
        CTL_ELEM_LIST => {
            let mut b = mem::slice(data, arg, ELEM_LIST_SIZE as u64)?.to_vec();
            let w = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            let (offset, space) = (w(&b, 0) as usize, w(&b, 4) as usize);
            let pids = u64::from_le_bytes(b[16..24].try_into().unwrap());
            let listed: Vec<&Control> = CONTROLS.iter().skip(offset).take(space).collect();
            for (k, c) in listed.iter().enumerate() {
                mem::write(data, pids + (k * ELEM_ID_SIZE) as u64, &c.id())?;
            }
            b[8..12].copy_from_slice(&(listed.len() as u32).to_le_bytes());
            b[12..16].copy_from_slice(&(CONTROLS.len() as u32).to_le_bytes());
            mem::write(data, arg, &b).map(|_| 0)
        }
        CTL_ELEM_INFO => {
            let mut b = mem::slice(data, arg, ELEM_INFO_SIZE as u64)?.to_vec();
            let c = control(&b)?;
            b[..ELEM_ID_SIZE].copy_from_slice(&c.id());
            b[64..68].copy_from_slice(&c.kind.to_le_bytes());
            b[68..72].copy_from_slice(&c.access.to_le_bytes());
            b[72..76].copy_from_slice(&c.count.to_le_bytes());
            b[76..80].copy_from_slice(&(-1i32).to_le_bytes()); // no owner
            b[80..88].copy_from_slice(&0i64.to_le_bytes());
            b[88..96].copy_from_slice(&c.max.to_le_bytes());
            b[96..104].copy_from_slice(&1i64.to_le_bytes());
            mem::write(data, arg, &b).map(|_| 0)
        }
        CTL_ELEM_READ | CTL_ELEM_WRITE => {
            let mut b = mem::slice(data, arg, ELEM_VALUE_SIZE as u64)?.to_vec();
            let c = control(&b)?;
            b[..ELEM_ID_SIZE].copy_from_slice(&c.id());
            let mut v = CARD.volume.lock().unwrap();
            let value = |b: &[u8], k: usize| i64::from_le_bytes(b[72 + 8 * k..80 + 8 * k].try_into().unwrap());
            if req == CTL_ELEM_WRITE {
                let before = *v;
                if c.numid == 1 {
                    v.0 = [value(&b, 0).clamp(0, VOLUME_MAX), value(&b, 1).clamp(0, VOLUME_MAX)];
                } else {
                    v.1 = value(&b, 0) != 0;
                }
                // (1: changed; 0: as it was.)
                return Ok((*v != before) as i64);
            }
            let values: Vec<i64> = if c.numid == 1 { v.0.to_vec() } else { vec![v.1 as i64] };
            for (k, x) in values.iter().enumerate() {
                b[72 + 8 * k..80 + 8 * k].copy_from_slice(&x.to_le_bytes());
            }
            mem::write(data, arg, &b).map(|_| 0)
        }
        CTL_TLV_READ => {
            // The volume's dB scale (SNDRV_CTL_TLVT_DB_SCALE: its minimum,
            // its step, and that the minimum is silence).
            let head = mem::slice(data, arg, 8)?.to_vec();
            let numid = u32::from_le_bytes(head[0..4].try_into().unwrap());
            let room = u32::from_le_bytes(head[4..8].try_into().unwrap());
            if numid != 1 {
                return Err(ENXIO);
            }
            if room < 16 {
                return Err(ENOMEM);
            }
            let tlv: [u32; 4] = [1, 8, VOLUME_DB_MIN as i32 as u32, VOLUME_DB_STEP as u32 | 0x10000];
            let bytes: Vec<u8> = tlv.iter().flat_map(|w| w.to_le_bytes()).collect();
            mem::write(data, arg + 8, &bytes).map(|_| 0)
        }
        CTL_SUBSCRIBE_EVENTS | CTL_PCM_PREFER_SUBDEVICE => Ok(0),
        CTL_PCM_NEXT_DEVICE => {
            let d = i32::from_le_bytes(mem::slice(data, arg, 4)?.try_into().unwrap());
            int(data, if d < 0 { 0 } else { -1 })
        }
        CTL_HWDEP_NEXT_DEVICE | CTL_RAWMIDI_NEXT_DEVICE | CTL_UMP_NEXT_DEVICE => int(data, -1),
        CTL_PCM_INFO => {
            let mut b = mem::slice(data, arg, PCM_INFO_SIZE as u64)?.to_vec();
            let w = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            // (Device 0, subdevice 0, playback or capture: the only one.)
            if w(0) != 0 || w(4) != 0 {
                return Err(ENXIO);
            }
            if w(8) > 1 {
                return Err(ENOENT);
            }
            let capture = w(8) == 1;
            pcm_info(&mut b, capture);
            mem::write(data, arg, &b).map(|_| 0)
        }
        CTL_POWER_STATE => int(data, 0),
        _ => Err(ENOTTY),
    }
}

/// The devices under /dev/snd, by name.
pub fn device(name: &str) -> Option<SoundDevice> {
    match name {
        "controlC0" => Some(SoundDevice::Control),
        "pcmC0D0p" => Some(SoundDevice::Playback),
        "pcmC0D0c" => Some(SoundDevice::Capture),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SoundDevice {
    Control,
    Playback,
    Capture,
}

impl SoundDevice {
    /// Its device number (major 116, as Linux's ALSA).
    pub fn rdev(self) -> u64 {
        let minor = match self {
            SoundDevice::Control => 0,
            SoundDevice::Playback => 16,
            SoundDevice::Capture => 24,
        };
        (116 << 8) | minor
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn any() -> HwParams {
        HwParams {
            flags: 0,
            masks: [[!0; 8]; 3],
            intervals: [Interval::any(); 12],
            rmask: !0,
            cmask: 0,
            info: 0,
            msbits: 0,
            rate_num: 0,
            rate_den: 0,
            fifo_size: 0,
        }
    }

    fn set(p: &mut HwParams, k: usize, v: u32) {
        p.intervals[k - SAMPLE_BITS] = Interval::range(v, v);
    }

    #[test]
    fn the_space_is_the_cards() {
        let mut p = any();
        p.refine_all().unwrap();
        assert!(p.has(ACCESS, ACCESS_RW_INTERLEAVED) && p.has(ACCESS, ACCESS_MMAP_INTERLEAVED) && !p.has(ACCESS, 2));
        assert!(p.has(FORMAT, 2) && p.has(FORMAT, 14) && !p.has(FORMAT, 3));
        assert_eq!((p.i(CHANNELS).min, p.i(CHANNELS).max), (1, 2));
        assert_eq!((p.i(RATE).min, p.i(RATE).max), RATES);
        assert_eq!((p.i(SAMPLE_BITS).min, p.i(SAMPLE_BITS).max), (8, 32));
        assert_eq!((p.i(FRAME_BITS).min, p.i(FRAME_BITS).max), (8, 64));
    }

    #[test]
    fn a_setup_is_chosen_and_consistent() {
        // (What aplay asks of a WAV: S16_LE, stereo, 44.1 kHz.)
        let mut p = any();
        p.masks[FORMAT] = [0; 8];
        p.masks[FORMAT][0] = 1 << 2;
        set(&mut p, CHANNELS, 2);
        set(&mut p, RATE, 44100);
        p.refine_all().unwrap();
        assert_eq!(p.i(FRAME_BITS).single(), Some(32));
        assert_eq!((p.msbits, p.rate_num, p.rate_den), (16, 44100, 1));
        // A buffer of half a second, in periods of about 1/20th.
        let mut x = 0;
        p.refine(BUFFER_TIME, &Interval::range(0, 500_000), &mut x).unwrap();
        p.refine(PERIOD_TIME, &Interval::range(50_000, 50_000), &mut x).unwrap();
        p.refine_all().unwrap();
        p.choose().unwrap();
        let (period, buffer) = (p.i(PERIOD_SIZE).single().unwrap(), p.i(BUFFER_SIZE).single().unwrap());
        assert_eq!(period, 2205);
        assert_eq!(buffer % period, 0);
        assert_eq!(p.i(PERIODS).single(), Some(buffer / period));
        assert_eq!(p.i(BUFFER_BYTES).single(), Some(buffer * 4));
    }

    #[test]
    fn what_cant_be_is_refused() {
        let mut p = any();
        set(&mut p, CHANNELS, 6);
        assert_eq!(p.refine_all(), Err(EINVAL));
        let mut p = any();
        p.masks[FORMAT] = [0; 8];
        p.masks[FORMAT][0] = 1 << 3; // S16_BE
        assert_eq!(p.refine_all(), Err(EINVAL));
    }

    #[test]
    fn params_round_trip_through_their_bytes() {
        let mut p = any();
        p.refine_all().unwrap();
        let mut b = vec![0u8; HW_PARAMS_SIZE];
        p.write(&mut b);
        assert_eq!(HwParams::read(&b), p);
    }

    #[test]
    fn a_stream_is_mixed_resampled() {
        let pcm = Pcm::open(false);
        let mut p = any();
        p.masks[FORMAT] = [0; 8];
        p.masks[FORMAT][0] = 1 << 2;
        set(&mut p, CHANNELS, 1);
        set(&mut p, RATE, 24000);
        p.choose().unwrap();
        pcm.setup(&p).unwrap();
        let mut s = pcm.stream.lock().unwrap();
        s.state = STATE_RUNNING;
        // (Half scale, written as the program would.)
        assert!(s.buffer_size >= 64);
        for k in 0..s.buffer_size as usize {
            let at = k * 2;
            s.ring[at..at + 2].copy_from_slice(&16384i16.to_le_bytes());
            s.appl += 1;
        }
        let mut out = vec![0.0f32; 2 * 100];
        s.mix(&mut out, 48000);
        // (At twice the rate: 100 frames out take 50 in, the first
        // between silence and the first frame.)
        assert_eq!(s.hw, 50);
        assert!((out[198] - 0.5).abs() < 1e-6 && out[198] == out[199]);
    }

    #[test]
    fn a_capture_stream_fills_resampled_and_overruns() {
        let pcm = Pcm::open(true);
        let mut p = any();
        p.masks[FORMAT] = [0; 8];
        p.masks[FORMAT][0] = 1 << 2;
        set(&mut p, CHANNELS, 1);
        set(&mut p, RATE, 24000);
        p.choose().unwrap();
        pcm.setup(&p).unwrap();
        let mut s = pcm.stream.lock().unwrap();
        // (Started as start starts it, without the host's input.)
        s.state = STATE_RUNNING;
        s.phase = 0.0;
        // (Half scale in, at twice the rate: 100 frames in make 50, the
        // first silence.)
        s.capture(&[0.5f32; 2 * 100], 48000);
        assert_eq!((s.hw, s.avail()), (50, 50));
        assert_eq!(&s.ring[0..2], &0i16.to_le_bytes());
        assert_eq!(&s.ring[2..4], &16384i16.to_le_bytes());
        // Read half; then more than the ring holds: an overrun.
        s.appl = 25;
        assert_eq!(s.avail(), 25);
        let more = vec![0.5f32; 4 * s.buffer_size as usize];
        s.capture(&more, 48000);
        assert_eq!((s.state, s.avail()), (STATE_XRUN, s.buffer_size as i64));
    }

    #[test]
    fn the_volume_follows_its_db_scale() {
        // Full volume leaves samples as they are; each step is 0.5 dB; the
        // bottom is silence.
        assert_eq!(gain(VOLUME_MAX), 1.0);
        assert!((gain(VOLUME_MAX - 12) - 0.5).abs() < 0.01); // -6 dB
        assert_eq!(gain(0), 0.0);
        let mut out = [0.5f32; 4];
        turn_down(&mut out, ([VOLUME_MAX, 0], true));
        assert_eq!(out, [0.5, 0.0, 0.5, 0.0]);
        turn_down(&mut out, ([VOLUME_MAX; 2], false));
        assert_eq!(out, [0.0; 4]);
        let mut id = [0u8; ELEM_ID_SIZE];
        id[4] = 2;
        id[16..38].copy_from_slice(b"Master Playback Switch");
        assert_eq!(control(&id).unwrap().numid, 2);
        id[0] = 1;
        assert_eq!(control(&id).unwrap().numid, 1);
        id[0] = 3;
        assert_eq!(control(&id).err(), Some(ENOENT));
    }
}
