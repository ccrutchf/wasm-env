//! A stream's samples as PulseAudio's clients have them (any of its
//! formats, any channel map) and as the card takes them: as they are where
//! the card takes them (U8, S16LE, S32LE and FLOAT32LE, mono or stereo),
//! else as float stereo, mixed down by the channels' positions (capture:
//! spread out the same way).

use crate::audio::Sample;

// pa_sample_format_t.
pub const U8: u8 = 0;
pub const ALAW: u8 = 1;
pub const ULAW: u8 = 2;
pub const S16LE: u8 = 3;
pub const S16BE: u8 = 4;
pub const FLOAT32LE: u8 = 5;
pub const FLOAT32BE: u8 = 6;
pub const S32LE: u8 = 7;
pub const S32BE: u8 = 8;
pub const S24LE: u8 = 9;
pub const S24BE: u8 = 10;
pub const S24_32LE: u8 = 11;
pub const S24_32BE: u8 = 12;

// pa_channel_position_t (those that aren't simply left or right).
pub const MONO: u8 = 0;
pub const FRONT_LEFT: u8 = 1;
pub const FRONT_RIGHT: u8 = 2;

/// Bytes per sample of a format; None if it isn't one.
pub fn sample_bytes(format: u8) -> Option<usize> {
    Some(match format {
        U8 | ALAW | ULAW => 1,
        S16LE | S16BE => 2,
        S24LE | S24BE => 3,
        FLOAT32LE | FLOAT32BE | S32LE | S32BE | S24_32LE | S24_32BE => 4,
        _ => return None,
    })
}

/// Where a channel position is heard: (left, right) weights.
fn weights(position: u8) -> [f32; 2] {
    // Positions with left or right in them (pa_channel_position_t):
    // front, rear, front-of-center, side, top front and top rear.
    const LEFT: &[u8] = &[1, 5, 8, 10, 45, 48];
    const RIGHT: &[u8] = &[2, 6, 9, 11, 46, 49];
    if LEFT.contains(&position) {
        [1.0, 0.0]
    } else if RIGHT.contains(&position) {
        [0.0, 1.0]
    } else {
        // (Mono, center, LFE, auxiliary: both, half each.)
        [0.5, 0.5]
    }
}

/// How a stream's samples meet the card's.
#[derive(Clone, Debug)]
pub struct Conversion {
    pub format: u8,
    pub map: Vec<u8>,
    /// What the card's stream takes: the client's own samples, or float
    /// stereo.
    pub card: Sample,
    pub card_channels: u32,
    pub direct: bool,
}

impl Conversion {
    pub fn new(format: u8, map: &[u8]) -> Conversion {
        let direct_sample = match format {
            U8 => Some(Sample::U8),
            S16LE => Some(Sample::S16),
            S32LE => Some(Sample::S32),
            FLOAT32LE => Some(Sample::F32),
            _ => None,
        };
        let plain = matches!(map, [MONO] | [FRONT_LEFT, FRONT_RIGHT]);
        match direct_sample {
            Some(s) if plain => Conversion { format, map: map.to_vec(), card: s, card_channels: map.len() as u32, direct: true },
            _ => Conversion { format, map: map.to_vec(), card: Sample::F32, card_channels: 2, direct: false },
        }
    }

    /// Bytes per frame, the client's and the card's.
    pub fn client_frame(&self) -> usize {
        sample_bytes(self.format).unwrap_or(1) * self.map.len()
    }
    pub fn card_frame(&self) -> usize {
        if self.direct { self.client_frame() } else { 8 }
    }

    /// The client's frames (whole ones) as the card's.
    pub fn to_card(&self, b: &[u8]) -> Vec<u8> {
        if self.direct {
            return b.to_vec();
        }
        let sb = sample_bytes(self.format).unwrap_or(1);
        let mut out = Vec::with_capacity(b.len() / self.client_frame() * 8);
        for frame in b.chunks_exact(self.client_frame()) {
            let mut lr = [0.0f32; 2];
            let mut total = [0.0f32; 2];
            for (c, &pos) in self.map.iter().enumerate() {
                let v = decode(self.format, &frame[c * sb..(c + 1) * sb]);
                let w = weights(pos);
                lr[0] += v * w[0];
                lr[1] += v * w[1];
                total[0] += w[0];
                total[1] += w[1];
            }
            // (Each side the average of what's heard there, so a mono
            // stream or a center channel isn't louder or softer.)
            for k in 0..2 {
                let v = if total[k] > 0.0 { lr[k] / total[k].max(1.0) } else { 0.0 };
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// The card's frames (whole ones) as the client's.
    pub fn from_card(&self, b: &[u8]) -> Vec<u8> {
        if self.direct {
            return b.to_vec();
        }
        let mut out = Vec::with_capacity(b.len() / 8 * self.client_frame());
        let mut sample = vec![0u8; sample_bytes(self.format).unwrap_or(1)];
        for frame in b.chunks_exact(8) {
            let l = f32::from_le_bytes(frame[0..4].try_into().unwrap());
            let r = f32::from_le_bytes(frame[4..8].try_into().unwrap());
            for &pos in &self.map {
                let w = weights(pos);
                encode(self.format, l * w[0] + r * w[1], &mut sample);
                out.extend_from_slice(&sample);
            }
        }
        out
    }
}

/// A sample as a float in [-1, 1].
pub fn decode(format: u8, b: &[u8]) -> f32 {
    let i24 = |x: u32| ((x << 8) as i32 >> 8) as f32 / 8388608.0;
    match format {
        U8 => (b[0] as f32 - 128.0) / 128.0,
        ALAW => alaw(b[0]) as f32 / 32768.0,
        ULAW => ulaw(b[0]) as f32 / 32768.0,
        S16LE => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        S16BE => i16::from_be_bytes([b[0], b[1]]) as f32 / 32768.0,
        FLOAT32LE => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        FLOAT32BE => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        S32LE => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0,
        S32BE => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0,
        S24LE => i24(u32::from_le_bytes([b[0], b[1], b[2], 0])),
        S24BE => i24(u32::from_be_bytes([0, b[0], b[1], b[2]])),
        S24_32LE => i24(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff),
        S24_32BE => i24(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff),
        _ => 0.0,
    }
}

/// A float in [-1, 1] as a sample, into `b`.
pub fn encode(format: u8, v: f32, b: &mut [u8]) {
    let v = v.clamp(-1.0, 1.0);
    let s16 = (v * 32767.0).round() as i16;
    let s24 = ((v as f64 * 8388607.0).round() as i32) as u32 & 0xff_ffff;
    let s32 = (v as f64 * 2147483647.0).round() as i32;
    match format {
        U8 => b[0] = (v * 127.0 + 128.0).round() as u8,
        ALAW => b[0] = to_alaw(s16),
        ULAW => b[0] = to_ulaw(s16),
        S16LE => b.copy_from_slice(&s16.to_le_bytes()),
        S16BE => b.copy_from_slice(&s16.to_be_bytes()),
        FLOAT32LE => b.copy_from_slice(&v.to_le_bytes()),
        FLOAT32BE => b.copy_from_slice(&v.to_be_bytes()),
        S32LE => b.copy_from_slice(&s32.to_le_bytes()),
        S32BE => b.copy_from_slice(&s32.to_be_bytes()),
        S24LE => b.copy_from_slice(&s24.to_le_bytes()[..3]),
        S24BE => b.copy_from_slice(&s24.to_be_bytes()[1..]),
        S24_32LE => b.copy_from_slice(&(((s24 << 8) as i32 >> 8) as u32).to_le_bytes()),
        S24_32BE => b.copy_from_slice(&(((s24 << 8) as i32 >> 8) as u32).to_be_bytes()),
        _ => {}
    }
}

/// G.711 A-law and µ-law, to and from 16-bit linear.
fn alaw(a: u8) -> i16 {
    let a = a ^ 0x55;
    let exp = ((a >> 4) & 7) as i32;
    let mant = (a & 0x0f) as i32;
    let mag = if exp == 0 { (mant << 4) + 8 } else { ((mant << 4) + 0x108) << (exp - 1) };
    (if a & 0x80 != 0 { mag } else { -mag }) as i16
}

fn ulaw(u: u8) -> i16 {
    let u = !u;
    let exp = ((u >> 4) & 7) as i32;
    let mant = (u & 0x0f) as i32;
    let mag = (((mant << 3) + 0x84) << exp) - 0x84;
    (if u & 0x80 != 0 { -mag } else { mag }) as i16
}

fn to_alaw(s: i16) -> u8 {
    let (sign, mut mag) = if s >= 0 { (0x80u8, s as i32) } else { (0, -(s as i32) - 1) };
    mag = mag.min(0x7fff);
    let exp = (0..8).rev().find(|&e| mag >= (if e == 0 { 0 } else { 0x100 << (e - 1) })).unwrap_or(0);
    let mant = if exp == 0 { (mag >> 4) & 0x0f } else { (mag >> (exp + 3)) & 0x0f };
    (sign | ((exp as u8) << 4) | mant as u8) ^ 0x55
}

fn to_ulaw(s: i16) -> u8 {
    let (sign, mag) = if s < 0 { (0x80u8, (-(s as i32)).min(32635)) } else { (0, (s as i32).min(32635)) };
    let mag = mag + 0x84;
    // (The exponent: where the top bit is, above bit 7.)
    let exp = (31 - (mag as u32).leading_zeros() as i32 - 7).clamp(0, 7);
    let mant = (mag >> (exp + 3)) & 0x0f;
    !(sign | ((exp as u8) << 4) | mant as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_trip() {
        let mut b = [0u8; 4];
        for f in [U8, S16LE, S16BE, FLOAT32LE, FLOAT32BE, S32LE, S32BE, S24LE, S24BE, S24_32LE, S24_32BE, ALAW, ULAW] {
            let n = sample_bytes(f).unwrap();
            for v in [0.0f32, 0.5, -0.5, 0.25] {
                encode(f, v, &mut b[..n]);
                let tol = if n == 1 { 0.04 } else { 1e-3 };
                assert!((decode(f, &b[..n]) - v).abs() < tol, "format {f}: {v} came back as {}", decode(f, &b[..n]));
            }
        }
    }

    #[test]
    fn surround_is_mixed_down_and_mono_spread() {
        // 5.1 (FL FR FC LFE RL RR): left only in the left.
        let c = Conversion::new(FLOAT32LE, &[1, 2, 3, 7, 5, 6]);
        assert!(!c.direct);
        let frame: Vec<u8> = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let out = c.to_card(&frame);
        let l = f32::from_le_bytes(out[0..4].try_into().unwrap());
        let r = f32::from_le_bytes(out[4..8].try_into().unwrap());
        assert!(l > 0.5 && r == 0.0);
        // Stereo S16LE goes as it is.
        assert!(Conversion::new(S16LE, &[1, 2]).direct);
        // S16BE stereo, captured: the card's float stereo, swapped.
        let c = Conversion::new(S16BE, &[1, 2]);
        let card: Vec<u8> = [0.5f32, -0.5].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(c.from_card(&card), [0x40, 0x00, 0xc0, 0x00]);
    }
}
