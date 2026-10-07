//! PulseAudio's tagstruct: the typed values a packet carries, each a tag
//! byte and its value, big-endian (pulsecore/tagstruct.c).

/// A sample format, its channels and rate (pa_sample_spec).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct SampleSpec {
    pub format: u8,
    pub channels: u8,
    pub rate: u32,
}

/// Properties: keys and values (strings with their NUL, or bytes).
pub type Proplist = Vec<(String, Vec<u8>)>;

/// The value of a string property.
pub fn prop<'a>(p: &'a Proplist, key: &str) -> Option<&'a str> {
    let (_, v) = p.iter().find(|(k, _)| k == key)?;
    std::str::from_utf8(v.strip_suffix(&[0]).unwrap_or(v)).ok()
}

/// A string property.
pub fn string(key: &str, value: &str) -> (String, Vec<u8>) {
    let mut v = value.as_bytes().to_vec();
    v.push(0);
    (key.to_string(), v)
}

/// A packet's values, written.
#[derive(Default)]
pub struct Writer {
    pub bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }
    fn tag(&mut self, t: u8) -> &mut Self {
        self.bytes.push(t);
        self
    }
    fn raw32(&mut self, v: u32) -> &mut Self {
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn string(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => {
                self.tag(b't');
                self.bytes.extend_from_slice(s.as_bytes());
                self.bytes.push(0);
                self
            }
            None => self.tag(b'N'),
        }
    }
    pub fn s(&mut self, s: &str) -> &mut Self {
        self.string(Some(s))
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.tag(b'L').raw32(v)
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.tag(b'B');
        self.bytes.push(v);
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.tag(b'R');
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn s64(&mut self, v: i64) -> &mut Self {
        self.tag(b'r');
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn usec(&mut self, v: u64) -> &mut Self {
        self.tag(b'U');
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bool(&mut self, b: bool) -> &mut Self {
        self.tag(if b { b'1' } else { b'0' })
    }
    pub fn sample_spec(&mut self, s: SampleSpec) -> &mut Self {
        self.tag(b'a');
        self.bytes.extend_from_slice(&[s.format, s.channels]);
        self.raw32(s.rate)
    }
    pub fn arbitrary(&mut self, b: &[u8]) -> &mut Self {
        self.tag(b'x').raw32(b.len() as u32);
        self.bytes.extend_from_slice(b);
        self
    }
    pub fn timeval(&mut self, sec: u32, usec: u32) -> &mut Self {
        self.tag(b'T').raw32(sec).raw32(usec)
    }
    pub fn channel_map(&mut self, map: &[u8]) -> &mut Self {
        self.tag(b'm');
        self.bytes.push(map.len() as u8);
        self.bytes.extend_from_slice(map);
        self
    }
    pub fn cvolume(&mut self, v: &[u32]) -> &mut Self {
        self.tag(b'v');
        self.bytes.push(v.len() as u8);
        for &x in v {
            self.raw32(x);
        }
        self
    }
    pub fn volume(&mut self, v: u32) -> &mut Self {
        self.tag(b'V').raw32(v)
    }
    pub fn proplist(&mut self, p: &Proplist) -> &mut Self {
        self.tag(b'P');
        for (k, v) in p {
            self.s(k).u32(v.len() as u32).arbitrary(v);
        }
        self.string(None)
    }
    /// A format_info: PCM (encoding 1) with properties.
    pub fn format_info(&mut self, encoding: u8, p: &Proplist) -> &mut Self {
        self.tag(b'f').u8(encoding).proplist(p)
    }
}

/// A packet's values, read in order: None where the next isn't of the
/// type asked for (a protocol error).
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, at: 0 }
    }
    pub fn eof(&self) -> bool {
        self.at >= self.b.len()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(s)
    }
    fn tag(&mut self, t: u8) -> Option<()> {
        (*self.take(1)?.first()? == t).then_some(())
    }
    fn raw32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    /// A string, or None for a null one (Some(None)).
    pub fn string(&mut self) -> Option<Option<String>> {
        match *self.b.get(self.at)? {
            b'N' => {
                self.at += 1;
                Some(None)
            }
            b't' => {
                self.at += 1;
                let len = self.b[self.at..].iter().position(|&c| c == 0)?;
                let s = String::from_utf8_lossy(self.take(len)?).into_owned();
                self.at += 1;
                Some(Some(s))
            }
            _ => None,
        }
    }
    pub fn u32(&mut self) -> Option<u32> {
        self.tag(b'L')?;
        self.raw32()
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.tag(b'B')?;
        Some(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Option<bool> {
        match *self.take(1)?.first()? {
            b'1' => Some(true),
            b'0' => Some(false),
            _ => None,
        }
    }
    pub fn sample_spec(&mut self) -> Option<SampleSpec> {
        self.tag(b'a')?;
        let fc = self.take(2)?;
        let (format, channels) = (fc[0], fc[1]);
        Some(SampleSpec { format, channels, rate: self.raw32()? })
    }
    pub fn arbitrary(&mut self) -> Option<&'a [u8]> {
        self.tag(b'x')?;
        let n = self.raw32()? as usize;
        self.take(n)
    }
    pub fn timeval(&mut self) -> Option<(u32, u32)> {
        self.tag(b'T')?;
        Some((self.raw32()?, self.raw32()?))
    }
    pub fn channel_map(&mut self) -> Option<Vec<u8>> {
        self.tag(b'm')?;
        let n = self.take(1)?[0] as usize;
        Some(self.take(n)?.to_vec())
    }
    pub fn cvolume(&mut self) -> Option<Vec<u32>> {
        self.tag(b'v')?;
        let n = self.take(1)?[0] as usize;
        (0..n).map(|_| self.raw32()).collect()
    }
    pub fn proplist(&mut self) -> Option<Proplist> {
        self.tag(b'P')?;
        let mut p = Vec::new();
        while let Some(k) = self.string()? {
            let n = self.u32()? as usize;
            let v = self.arbitrary()?;
            if v.len() != n {
                return None;
            }
            p.push((k, v.to_vec()));
        }
        Some(p)
    }
    /// A format_info: its encoding and properties.
    pub fn format_info(&mut self) -> Option<(u8, Proplist)> {
        self.tag(b'f')?;
        Some((self.u8()?, self.proplist()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        let p = vec![string("application.name", "paplay")];
        let mut w = Writer::new();
        w.u32(7).s("sink").string(None).sample_spec(SampleSpec { format: 3, channels: 2, rate: 44100 }).channel_map(&[1, 2]);
        w.cvolume(&[0x10000, 0x8000]).bool(true).proplist(&p).format_info(1, &p).arbitrary(b"ab");
        let mut r = Reader::new(&w.bytes);
        assert_eq!(r.u32(), Some(7));
        assert_eq!(r.string(), Some(Some("sink".into())));
        assert_eq!(r.string(), Some(None));
        assert_eq!(r.sample_spec(), Some(SampleSpec { format: 3, channels: 2, rate: 44100 }));
        assert_eq!(r.channel_map(), Some(vec![1, 2]));
        assert_eq!(r.cvolume(), Some(vec![0x10000, 0x8000]));
        assert_eq!(r.bool(), Some(true));
        let got = r.proplist().unwrap();
        assert_eq!(prop(&got, "application.name"), Some("paplay"));
        assert_eq!(r.format_info().map(|f| f.0), Some(1));
        assert_eq!(r.arbitrary(), Some(&b"ab"[..]));
        assert!(r.eof());
        // (The wrong type is an error.)
        assert_eq!(Reader::new(&w.bytes).u8(), None);
    }
}
