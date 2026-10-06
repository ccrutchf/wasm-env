//! `/dev/snd`: the runtime's sound card (docs/abi.md, "Audio"), so the
//! environment's own `libasound` (and SDL, GStreamer, everything over ALSA)
//! plays sound on every host. It speaks ALSA's kernel interface: a control
//! device (`controlC0`: the card, "wasm-env") and a playback PCM
//! (`pcmC0D0p`). The card's driver matches no card configuration of
//! `libasound`'s, so its `default` is `plug` over `hw`: conversions the card
//! lacks happen in the guest.
//!
//! A PCM takes U8, S16_LE, S32_LE and FLOAT_LE, one or two channels, at 8 to
//! 192 kHz, through `WRITEI_FRAMES` and `WRITEN_FRAMES` (no mmap: the
//! status and control pages refused, `libasound` uses `SYNC_PTR`). Its
//! frames wait in a ring buffer of the size its parameters chose, which a
//! mixer drains: every running stream, converted to float stereo at the
//! sink's rate (resampled linearly), summed. The sink is the host's audio
//! output (`cpal`) or none (`--audio=null`: the mixer on a clock of its own),
//! which can record what each stream played (`WASM_ENV_AUDIO_RECORD`: the
//! bytes as written, for tests).

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
const PCM_LINK: u32 = ioc(W, b'A', 0x60, 4);
const PCM_UNLINK: u32 = ioc(NONE, b'A', 0x61, 0);

const CTL_PVERSION: u32 = ioc(R, b'U', 0x00, 4);
const CTL_CARD_INFO: u32 = ioc(R, b'U', 0x01, CARD_INFO_SIZE);
const CTL_ELEM_LIST: u32 = ioc(RW, b'U', 0x10, ELEM_LIST_SIZE);
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

/// A playback stream: its setup, its ring buffer, and where the program
/// and the hardware are in it (frames, wrapping at `boundary`).
struct Stream {
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
    /// The program's mapping of the data (memory-mapped access): its
    /// address and length, which committed frames are copied from.
    area: Option<(u64, u64)>,
}

impl Stream {
    fn frame_bytes(&self) -> usize {
        (self.format.bits() / 8) as usize * self.channels
    }
    /// Frames written and not yet played: negative once the hardware has
    /// run past the program's pointer (an underrun, playing on).
    fn hw_avail(&self) -> i64 {
        let d = (self.appl + self.boundary - self.hw) % self.boundary;
        if d > self.boundary / 2 { d as i64 - self.boundary as i64 } else { d as i64 }
    }
    /// Room for the program.
    fn avail(&self) -> i64 {
        self.buffer_size as i64 - self.hw_avail()
    }
    fn running(&self) -> bool {
        self.state == STATE_RUNNING || self.state == STATE_DRAINING
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
    /// The next frame played, as float stereo (None: none written).
    fn take(&mut self) -> Option<[f32; 2]> {
        if self.hw_avail() <= 0 {
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
        Some([l, r])
    }
    /// Adds `out.len() / 2` frames of float stereo at `rate` into `out`.
    fn mix(&mut self, out: &mut [f32], rate: u32) {
        let step = self.rate as f64 / rate as f64;
        for frame in out.chunks_exact_mut(2) {
            while self.phase >= 1.0 {
                self.prev = self.next;
                match self.take() {
                    Some(f) => self.next = f,
                    None => {
                        // Nothing left: drained, or an underrun (the
                        // hardware runs on, through silence, until the
                        // stop threshold).
                        if self.state == STATE_DRAINING {
                            self.state = STATE_SETUP;
                            return;
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
}

/// The card: its streams (for the mixer), woken writers, and the sink.
struct Card {
    streams: Mutex<Vec<Weak<Pcm>>>,
    started: OnceLock<()>,
}

static CARD: Card = Card { streams: Mutex::new(Vec::new()), started: OnceLock::new() };

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
    let streams: Vec<Arc<Pcm>> = {
        let mut s = CARD.streams.lock().unwrap();
        s.retain(|w| w.strong_count() > 0);
        s.iter().filter_map(Weak::upgrade).collect()
    };
    for p in streams {
        let mut s = p.stream.lock().unwrap();
        if s.running() {
            s.mix(out, rate);
            drop(s);
            p.wake.notify_all();
        }
    }
    for v in out.iter_mut() {
        *v = v.clamp(-1.0, 1.0);
    }
}

/// Starts the sink, when the first stream needs it.
fn start_sink() {
    CARD.started.get_or_init(|| {
        let sink = *SINK.get_or_init(|| Sink::Host);
        if sink == Sink::Host && crate::audio_host::start() {
            return;
        }
        std::thread::Builder::new()
            .name("wasm-env audio".into())
            .spawn(|| {
                // (Ten milliseconds at a time, by the clock.)
                let begun = Instant::now();
                let mut done = 0u64;
                let mut buf = Vec::new();
                loop {
                    std::thread::sleep(Duration::from_millis(5));
                    let due = (begun.elapsed().as_secs_f64() * NULL_RATE as f64) as u64;
                    if due > done {
                        buf.resize(2 * (due - done) as usize, 0.0);
                        mix(&mut buf, NULL_RATE);
                        done = due;
                    }
                }
            })
            .expect("audio thread");
    });
}

// ---- The devices ----

/// The PCM device (`/dev/snd/pcmC0D0p`), one per open.
pub struct Pcm {
    stream: Mutex<Stream>,
    wake: Condvar,
}

impl Pcm {
    pub fn open() -> Arc<Pcm> {
        let p = Arc::new(Pcm {
            stream: Mutex::new(Stream {
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
                area: None,
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

    /// For poll: room to write (as many frames as avail_min), or an error
    /// state.
    pub fn readiness(&self) -> (bool, bool) {
        let s = self.stream.lock().unwrap();
        let err = s.state == STATE_XRUN || s.state == STATE_OPEN;
        (s.state != STATE_OPEN && s.avail() >= s.avail_min as i64, err)
    }

    /// An ioctl: `interrupted` says when a wait should end (a signal).
    pub fn ioctl(self: &Arc<Self>, data: &mut [u8], req: u32, arg: u64, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<i64> {
        let word = |data: &mut [u8], v: u32| mem::write(data, arg, &v.to_le_bytes()).map(|_| 0);
        match req {
            PCM_PVERSION => word(data, PCM_VERSION),
            PCM_INFO => {
                let mut b = vec![0u8; PCM_INFO_SIZE];
                pcm_info(&mut b);
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
                b[56..64].copy_from_slice(&s.hw_avail().max(0).to_le_bytes());
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
                mem::write(data, arg, &s.hw_avail().to_le_bytes()).map(|_| 0)
            }
            PCM_SYNC_PTR => {
                let mut b = mem::slice(data, arg, SYNC_PTR_SIZE as u64)?.to_vec();
                let flags = u32::from_le_bytes(b[0..4].try_into().unwrap());
                let mut s = self.stream.lock().unwrap();
                // (The program's pointer and avail_min, unless it asks for
                // ours.)
                if flags & 2 == 0 {
                    let appl = u64::from_le_bytes(b[72..80].try_into().unwrap());
                    if s.state != STATE_OPEN {
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
                b[8..12].copy_from_slice(&s.state.to_le_bytes());
                b[16..24].copy_from_slice(&s.hw.to_le_bytes());
                b[24..40].copy_from_slice(&timespec(Instant::now()));
                b[72..80].copy_from_slice(&s.appl.to_le_bytes());
                b[80..88].copy_from_slice(&s.avail_min.to_le_bytes());
                mem::write(data, arg, &b).map(|_| 0)
            }
            // (No mmap: libasound reads and writes through ioctls.)
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
                if s.hw_avail() <= 0 && s.stop_threshold < s.boundary {
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
                match s.state {
                    STATE_OPEN => return Err(EBADFD),
                    STATE_PREPARED if s.hw_avail() > 0 => self.start(&mut s),
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
            PCM_WRITEI | PCM_WRITEN => {
                let x = mem::slice(data, arg, XFER_SIZE as u64)?.to_vec();
                let buf = u64::from_le_bytes(x[8..16].try_into().unwrap());
                let frames = u64::from_le_bytes(x[16..24].try_into().unwrap());
                let n = self.write(data, buf, frames, req == PCM_WRITEN, nonblock, interrupted)?;
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
        s.format = format;
        s.interleaved = interleaved;
        s.channels = channels as usize;
        s.rate = rate;
        s.period_size = period as u64;
        s.buffer_size = buffer as u64;
        // (The kernel's boundary: the buffer size doubled while it fits.)
        let mut boundary = s.buffer_size;
        while boundary * 2 <= (i64::MAX as u64) / 2 - s.buffer_size {
            boundary *= 2;
        }
        s.boundary = boundary;
        s.avail_min = s.period_size;
        s.start_threshold = 1;
        s.stop_threshold = s.buffer_size;
        s.ring = vec![0; s.buffer_size as usize * s.frame_bytes()];
        s.appl = 0;
        s.hw = 0;
        s.state = STATE_SETUP;
        s.record = std::env::var_os("WASM_ENV_AUDIO_RECORD").and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        if std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some() {
            let access = (0..5).find(|&b| p.has(ACCESS, b)).unwrap_or(9);
            eprintln!("wasm-env: sound: access {access}, {:?}, {} channels, {} Hz, period {}, buffer {}", s.format, s.channels, s.rate, s.period_size, s.buffer_size);
        }
        Ok(())
    }

    fn start(&self, s: &mut Stream) {
        s.state = STATE_RUNNING;
        s.trigger = Instant::now();
        s.phase = 1.0;
        start_sink();
    }

    /// `frames` frames from the program's memory at `buf` (interleaved, or
    /// an array of per-channel pointers), waiting for room unless
    /// `nonblock`; the frames written.
    fn write(&self, data: &mut [u8], buf: u64, frames: u64, separate: bool, nonblock: bool, interrupted: &dyn Fn() -> bool) -> SysResult<u64> {
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
            let room = s.avail().max(0) as u64;
            if room == 0 {
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
            let n = room.min(frames - done);
            let fb = s.frame_bytes();
            let sb = (s.format.bits() / 8) as usize;
            for k in 0..n {
                let at = (s.appl % s.buffer_size) as usize;
                if separate {
                    for (c, &p) in pointers.iter().enumerate() {
                        let src = mem::slice(data, p + (done + k) * sb as u64, sb as u64)?;
                        let off = c * s.buffer_size as usize * sb + at * sb;
                        s.ring[off..off + sb].copy_from_slice(src);
                    }
                } else {
                    let src = mem::slice(data, buf + (done + k) * fb as u64, fb as u64)?;
                    s.ring[at * fb..at * fb + fb].copy_from_slice(src);
                }
                s.appl = (s.appl + 1) % s.boundary;
            }
            done += n;
            if s.state == STATE_PREPARED && s.hw_avail() as u64 >= s.start_threshold.max(1) {
                self.start(&mut s);
            }
        }
        Ok(done)
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

/// struct snd_pcm_info for the card's one PCM (playback).
fn pcm_info(b: &mut [u8]) {
    b[12..16].copy_from_slice(&0i32.to_le_bytes()); // card
    let name = b"wasm-env";
    b[16..16 + name.len()].copy_from_slice(name); // id
    b[80..80 + name.len()].copy_from_slice(name); // name
    let sub = b"subdevice #0";
    b[160..160 + sub.len()].copy_from_slice(sub); // subname
    b[200..204].copy_from_slice(&1u32.to_le_bytes()); // subdevices_count
    b[204..208].copy_from_slice(&1u32.to_le_bytes()); // subdevices_avail
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
            // (No controls yet: none listed.)
            let mut b = mem::slice(data, arg, ELEM_LIST_SIZE as u64)?.to_vec();
            b[8..16].fill(0); // used, count
            mem::write(data, arg, &b).map(|_| 0)
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
            // (Device 0, subdevice 0, playback: the only one.)
            if w(0) != 0 || w(4) != 0 {
                return Err(ENXIO);
            }
            if w(8) != 0 {
                return Err(ENOENT);
            }
            pcm_info(&mut b);
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
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SoundDevice {
    Control,
    Playback,
}

impl SoundDevice {
    /// Its device number (major 116, as Linux's ALSA).
    pub fn rdev(self) -> u64 {
        let minor = match self {
            SoundDevice::Control => 0,
            SoundDevice::Playback => 16,
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
        let pcm = Pcm::open();
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
}
