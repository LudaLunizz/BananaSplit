//! VBAN packet format, as specified by VB-Audio ("VBAN Protocol
//! Specifications", revision 13) and implemented by Voicemeeter.
//!
//! ```text
//! offset  size  field
//!      0     4  "VBAN"
//!      4     1  format_SR:  bits 0-4 sample-rate index, bits 5-7 sub-protocol (0 = audio)
//!      5     1  format_nbs: samples per channel in this packet, minus 1 (1..256)
//!      6     1  format_nbc: channels, minus 1
//!      7     1  format_bit: bits 0-2 data type, bit 3 reserved (0), bits 4-7 codec (0 = PCM)
//!      8    16  stream name, ASCII, zero-padded
//!     24     4  frame counter, +1 per packet of the stream
//!     28   ...  interleaved samples, little-endian
//! ```
//!
//! A packet carries at most 1436 bytes of samples (1464 bytes in all), so it
//! fits an Ethernet frame without fragmentation; BananaSplit never sends more.
//! Receivers must drop packets whose reserved bit is set, whose codec is not
//! PCM, or whose size is not exactly `samples x channels x sample size`.
//! A larger packet whose size does match its header is accepted (some
//! senders other than Voicemeeter put 256 24-bit stereo samples in one).

pub const MAGIC: &[u8; 4] = b"VBAN";
pub const HEADER_LEN: usize = 28;
pub const MAX_DATA: usize = 1436;
pub const MAX_PACKET: usize = HEADER_LEN + MAX_DATA;
/// Samples per channel in one packet.
pub const MAX_SAMPLES: usize = 256;
pub const NAME_LEN: usize = 16;
pub const DEFAULT_PORT: u16 = 6980;

const SR_MASK: u8 = 0x1F;
const SUB_MASK: u8 = 0xE0;
pub const SUB_AUDIO: u8 = 0x00;
pub const SUB_SERIAL: u8 = 0x20;
pub const SUB_TEXT: u8 = 0x40;
pub const SUB_SERVICE: u8 = 0x60;
const TYPE_MASK: u8 = 0x07;
const RESERVED: u8 = 0x08;
const CODEC_MASK: u8 = 0xF0;
const CODEC_PCM: u8 = 0x00;

/// The sample rates a VBAN stream can have, by index.
pub const SAMPLE_RATES: [u32; 21] = [
    6000, 12000, 24000, 48000, 96000, 192000, 384000, 8000, 16000, 32000, 64000, 128000, 256000, 512000, 11025,
    22050, 44100, 88200, 176400, 352800, 705600,
];

/// Index of `rate` in [`SAMPLE_RATES`].
pub fn sr_index(rate: u32) -> Option<u8> {
    SAMPLE_RATES.iter().position(|&r| r == rate).map(|i| i as u8)
}

/// PCM sample formats (the packed 10- and 12-bit types are not supported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SampleFormat {
    /// 8-bit unsigned, 128 = zero.
    U8 = 0,
    I16 = 1,
    I24 = 2,
    I32 = 3,
    F32 = 4,
    F64 = 5,
}

impl SampleFormat {
    /// Formats BananaSplit can send.
    pub const SEND: [SampleFormat; 4] = [SampleFormat::I16, SampleFormat::I24, SampleFormat::I32, SampleFormat::F32];

    pub fn from_bits(v: u8) -> Option<SampleFormat> {
        Some(match v {
            0 => SampleFormat::U8,
            1 => SampleFormat::I16,
            2 => SampleFormat::I24,
            3 => SampleFormat::I32,
            4 => SampleFormat::F32,
            5 => SampleFormat::F64,
            _ => return None,
        })
    }

    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::I32 | SampleFormat::F32 => 4,
            SampleFormat::F64 => 8,
        }
    }

    /// Config file name.
    pub fn as_str(self) -> &'static str {
        match self {
            SampleFormat::U8 => "uint8",
            SampleFormat::I16 => "int16",
            SampleFormat::I24 => "int24",
            SampleFormat::I32 => "int32",
            SampleFormat::F32 => "float32",
            SampleFormat::F64 => "float64",
        }
    }

    /// For people: "16-bit", "32-bit float".
    pub fn label(self) -> &'static str {
        match self {
            SampleFormat::U8 => "8-bit",
            SampleFormat::I16 => "16-bit",
            SampleFormat::I24 => "24-bit",
            SampleFormat::I32 => "32-bit",
            SampleFormat::F32 => "32-bit float",
            SampleFormat::F64 => "64-bit float",
        }
    }

    /// A format to send in (config / GUI).
    pub fn parse_send(v: &str) -> Result<SampleFormat, String> {
        match v.trim().to_ascii_lowercase().as_str() {
            "int16" | "16" | "pcm16" | "s16" => Ok(SampleFormat::I16),
            "int24" | "24" | "pcm24" | "s24" => Ok(SampleFormat::I24),
            "int32" | "32" | "pcm32" | "s32" => Ok(SampleFormat::I32),
            "float32" | "float" | "f32" | "32f" => Ok(SampleFormat::F32),
            _ => Err(format!("unknown VBAN format {v:?} (expected int16, int24, int32 or float32)")),
        }
    }
}

/// The fields of an audio packet's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    pub sr_index: u8,
    /// Samples per channel, 1..=256.
    pub samples: usize,
    /// 1..=256.
    pub channels: usize,
    pub format: SampleFormat,
    pub name: [u8; NAME_LEN],
    pub frame: u32,
}

impl AudioHeader {
    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATES[self.sr_index as usize]
    }

    /// The stream name without its zero padding.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        &self.name[..end]
    }

    /// The stream name for display (anything that is not printable ASCII
    /// becomes `?`).
    pub fn name_lossy(&self) -> String {
        self.name_bytes().iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '?' }).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// An audio packet and its samples (exactly the size the header says).
    Audio(AudioHeader, &'a [u8]),
    /// Another VBAN sub-protocol (serial, text, service...): not audio.
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Shorter than a header.
    Length(usize),
    NotVban,
    SampleRate(u8),
    Reserved,
    /// Not PCM (a VB-Audio codec or a user codec).
    Codec(u8),
    /// 10- or 12-bit packed samples.
    DataType(u8),
    /// Payload size differs from samples x channels x sample size.
    Size { expected: usize, got: usize },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Length(n) => write!(f, "{n}-byte packet is too short for VBAN"),
            ParseError::NotVban => write!(f, "not a VBAN packet"),
            ParseError::SampleRate(i) => write!(f, "unknown VBAN sample-rate index {i}"),
            ParseError::Reserved => write!(f, "reserved bit set"),
            ParseError::Codec(c) => write!(f, "compressed VBAN stream (codec 0x{c:02x}); BananaSplit needs PCM"),
            ParseError::DataType(t) => write!(f, "unsupported VBAN sample type {t} (10/12-bit)"),
            ParseError::Size { expected, got } => write!(f, "VBAN packet holds {got} bytes of samples, header says {expected}"),
        }
    }
}

/// Parse one datagram.
pub fn parse(buf: &[u8]) -> Result<Packet<'_>, ParseError> {
    if buf.len() < HEADER_LEN {
        return Err(ParseError::Length(buf.len()));
    }
    if &buf[0..4] != MAGIC {
        return Err(ParseError::NotVban);
    }
    let sub = buf[4] & SUB_MASK;
    if sub != SUB_AUDIO {
        return Ok(Packet::Other(sub));
    }
    let sri = buf[4] & SR_MASK;
    if sri as usize >= SAMPLE_RATES.len() {
        return Err(ParseError::SampleRate(sri));
    }
    let fb = buf[7];
    if fb & RESERVED != 0 {
        return Err(ParseError::Reserved);
    }
    if fb & CODEC_MASK != CODEC_PCM {
        return Err(ParseError::Codec(fb & CODEC_MASK));
    }
    let format = SampleFormat::from_bits(fb & TYPE_MASK).ok_or(ParseError::DataType(fb & TYPE_MASK))?;
    let samples = buf[5] as usize + 1;
    let channels = buf[6] as usize + 1;
    let data = &buf[HEADER_LEN..];
    let expected = samples * channels * format.bytes();
    if data.len() != expected {
        return Err(ParseError::Size { expected, got: data.len() });
    }
    let mut name = [0u8; NAME_LEN];
    name.copy_from_slice(&buf[8..24]);
    let frame = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    Ok(Packet::Audio(AudioHeader { sr_index: sri, samples, channels, format, name, frame }, data))
}

/// Write an audio header into `out[..HEADER_LEN]`.
pub fn write_header(out: &mut [u8], h: &AudioHeader) {
    debug_assert!((1..=MAX_SAMPLES).contains(&h.samples) && (1..=256).contains(&h.channels));
    out[0..4].copy_from_slice(MAGIC);
    out[4] = (h.sr_index & SR_MASK) | SUB_AUDIO;
    out[5] = (h.samples - 1) as u8;
    out[6] = (h.channels - 1) as u8;
    out[7] = h.format as u8 | CODEC_PCM;
    out[8..24].copy_from_slice(&h.name);
    out[24..28].copy_from_slice(&h.frame.to_le_bytes());
}

/// Most samples per channel that fit one packet.
pub fn max_samples(channels: usize, format: SampleFormat) -> usize {
    (MAX_DATA / (channels.max(1) * format.bytes())).clamp(1, MAX_SAMPLES)
}

/// A stream name as the 16 header bytes (the name is validated ASCII).
pub fn name_field(name: &str) -> [u8; NAME_LEN] {
    let mut f = [0u8; NAME_LEN];
    for (d, s) in f.iter_mut().zip(name.bytes()) {
        *d = s;
    }
    f
}

/// Does a received stream name match a configured one? Compared without
/// regard to ASCII case, so `stream1` accepts Voicemeeter's `Stream1`.
pub fn name_matches(received: &[u8], configured: &str) -> bool {
    received.eq_ignore_ascii_case(configured.as_bytes())
}

/// A valid stream name: 1 to 16 printable ASCII characters.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > NAME_LEN || !name.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        return Err(format!("VBAN stream names are 1-16 ASCII characters, got {name:?}"));
    }
    if name.trim() != name {
        return Err(format!("VBAN stream name {name:?} starts or ends with a space"));
    }
    Ok(())
}

/// Decode the samples of one packet into stereo frames (`L R L R ...`)
/// appended to `out`: channel 1 is L and channel 2 is R; a mono stream goes
/// to both; further channels are ignored.
pub fn decode_stereo(data: &[u8], channels: usize, format: SampleFormat, out: &mut Vec<f32>) {
    let bps = format.bytes();
    let frame = channels * bps;
    if frame == 0 {
        return;
    }
    let get = |s: &[u8]| -> f32 {
        match format {
            SampleFormat::U8 => (s[0] as f32 - 128.0) / 128.0,
            SampleFormat::I16 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0,
            SampleFormat::I24 => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            SampleFormat::I32 => (i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64 / 2_147_483_648.0) as f32,
            SampleFormat::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            SampleFormat::F64 => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
        }
    };
    out.reserve(2 * (data.len() / frame));
    for f in data.chunks_exact(frame) {
        let l = get(&f[..bps]);
        let r = if channels > 1 { get(&f[bps..2 * bps]) } else { l };
        out.push(if l.is_finite() { l } else { 0.0 });
        out.push(if r.is_finite() { r } else { 0.0 });
    }
}

/// Encode stereo frames (`L R L R ...`) as `channels` (1 = (L+R)/2, or 2)
/// of `format` into `out`; returns the number of bytes written.
pub fn encode(frames: &[f32], channels: usize, format: SampleFormat, out: &mut [u8]) -> usize {
    let mut o = 0;
    let mut put = |x: f32| {
        let x = if x.is_finite() { x } else { 0.0 };
        let c = x.clamp(-1.0, 1.0);
        match format {
            SampleFormat::U8 => {
                out[o] = ((c * 127.0).round() as i32 + 128) as u8;
            }
            SampleFormat::I16 => out[o..o + 2].copy_from_slice(&((c * 32_767.0).round() as i16).to_le_bytes()),
            SampleFormat::I24 => out[o..o + 3].copy_from_slice(&((c * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
            SampleFormat::I32 => {
                out[o..o + 4].copy_from_slice(&((c as f64 * 2_147_483_647.0).round() as i32).to_le_bytes())
            }
            SampleFormat::F32 => out[o..o + 4].copy_from_slice(&x.to_le_bytes()),
            SampleFormat::F64 => out[o..o + 8].copy_from_slice(&(x as f64).to_le_bytes()),
        }
        o += format.bytes();
    };
    for f in frames.chunks_exact(2) {
        if channels == 1 {
            put(0.5 * (f[0] + f[1]));
        } else {
            put(f[0]);
            put(f[1]);
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet built by hand, byte by byte, from the specification.
    fn spec_packet() -> Vec<u8> {
        let mut p = b"VBAN".to_vec();
        p.push(3); // 48000 Hz, audio
        p.push(1); // 2 samples
        p.push(1); // 2 channels
        p.push(1); // int16, PCM
        p.extend_from_slice(b"Stream1\0\0\0\0\0\0\0\0\0");
        p.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        for v in [16384i16, -16384, 32767, -32768] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        p
    }

    #[test]
    fn parses_a_packet_built_from_the_spec() {
        let p = spec_packet();
        let Ok(Packet::Audio(h, data)) = parse(&p) else { panic!("{:?}", parse(&p)) };
        assert_eq!((h.sample_rate(), h.samples, h.channels, h.format, h.frame), (48000, 2, 2, SampleFormat::I16, 0x0102_0304));
        assert_eq!(h.name_bytes(), b"Stream1");
        assert!(name_matches(h.name_bytes(), "stream1") && !name_matches(h.name_bytes(), "Stream10"));
        let mut out = Vec::new();
        decode_stereo(data, h.channels, h.format, &mut out);
        assert_eq!(out, [0.5, -0.5, 32767.0 / 32768.0, -1.0]);
        // writing the same header gives the same bytes
        let mut w = vec![0u8; HEADER_LEN];
        write_header(&mut w, &h);
        assert_eq!(w, p[..HEADER_LEN]);
    }

    #[test]
    fn rejects_what_the_spec_says_to_reject() {
        let p = spec_packet();
        let mut bad = p.clone();
        bad[7] |= 0x08;
        assert_eq!(parse(&bad), Err(ParseError::Reserved));
        let mut bad = p.clone();
        bad[7] = 0x10 | 1; // VBCA codec
        assert_eq!(parse(&bad), Err(ParseError::Codec(0x10)));
        let mut bad = p.clone();
        bad[7] = 6; // 12-bit
        assert_eq!(parse(&bad), Err(ParseError::DataType(6)));
        let mut bad = p.clone();
        bad[4] = 21;
        assert_eq!(parse(&bad), Err(ParseError::SampleRate(21)));
        assert_eq!(parse(&p[..p.len() - 1]), Err(ParseError::Size { expected: 8, got: 7 }));
        let mut long = p.clone();
        long.push(0);
        assert_eq!(parse(&long), Err(ParseError::Size { expected: 8, got: 9 }));
        assert_eq!(parse(&p[..27]), Err(ParseError::Length(27)));
        // over the 1436-byte limit but consistent: accepted
        let mut big = spec_packet();
        big[5] = 255; // 256 samples
        big[7] = 2; // 24-bit
        big.truncate(HEADER_LEN);
        big.extend(std::iter::repeat_n(0u8, 256 * 2 * 3));
        assert!(matches!(parse(&big), Ok(Packet::Audio(h, d)) if h.samples == 256 && d.len() == 1536));
        let mut bad = p.clone();
        bad[0] = b'X';
        assert_eq!(parse(&bad), Err(ParseError::NotVban));
        let mut text = p.clone();
        text[4] = SUB_TEXT;
        assert_eq!(parse(&text), Ok(Packet::Other(SUB_TEXT)));
        let mut svc = p;
        svc[4] = SUB_SERVICE;
        assert_eq!(parse(&svc), Ok(Packet::Other(SUB_SERVICE)));
    }

    #[test]
    fn never_panics_on_garbage() {
        let mut x = 0x1234_5678u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for _ in 0..50_000 {
            let len = (next() % 1500) as usize;
            let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if len >= 4 && next() % 2 == 0 {
                b[..4].copy_from_slice(MAGIC);
            }
            if let Ok(Packet::Audio(h, d)) = parse(&b) {
                let mut out = Vec::new();
                decode_stereo(d, h.channels, h.format, &mut out);
                assert_eq!(out.len(), 2 * h.samples);
                assert!(out.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn sample_rates_and_sizes() {
        assert_eq!(sr_index(48000), Some(3));
        assert_eq!(sr_index(44100), Some(16));
        assert_eq!(sr_index(705_600), Some(20));
        assert_eq!(sr_index(50000), None);
        for (i, &r) in SAMPLE_RATES.iter().enumerate() {
            assert_eq!(sr_index(r), Some(i as u8));
        }
        // 1436 bytes and 256 samples at most
        assert_eq!(max_samples(2, SampleFormat::I16), 256);
        assert_eq!(max_samples(2, SampleFormat::I24), 239);
        assert_eq!(max_samples(2, SampleFormat::F32), 179);
        assert_eq!(max_samples(1, SampleFormat::F32), 256);
        assert_eq!(max_samples(8, SampleFormat::I16), 89, "the spec's own example");
        for f in SampleFormat::SEND {
            for ch in [1, 2] {
                assert!(max_samples(ch, f) * ch * f.bytes() <= MAX_DATA);
            }
        }
    }

    #[test]
    fn every_format_round_trips() {
        let frames = [0.0f32, 0.5, -0.5, 0.25, 1.0, -1.0, 0.123_456_7, -0.999];
        for f in [SampleFormat::U8, SampleFormat::I16, SampleFormat::I24, SampleFormat::I32, SampleFormat::F32, SampleFormat::F64] {
            let mut buf = vec![0u8; frames.len() * 8];
            let n = encode(&frames, 2, f, &mut buf);
            assert_eq!(n, frames.len() * f.bytes());
            let mut back = Vec::new();
            decode_stereo(&buf[..n], 2, f, &mut back);
            // encoding scales by 2^(n-1) - 1 and decoding by 2^(n-1) (so
            // full scale encodes to the largest code and -1.0 decodes from
            // the smallest), plus half a step of rounding
            let tol = match f {
                SampleFormat::U8 => 1.0 / 128.0 + 0.5 / 127.0,
                SampleFormat::I16 => 1.0 / 32_768.0 + 0.5 / 32_767.0,
                SampleFormat::I24 => 1.0 / 8_388_608.0 + 0.5 / 8_388_607.0,
                _ => 1e-7,
            };
            for (a, b) in frames.iter().zip(&back) {
                assert!((a - b).abs() <= tol, "{f:?}: {a} -> {b}");
            }
        }
        // clipping, NaN, mono
        let mut buf = [0u8; 12];
        let n = encode(&[2.0, -3.0, f32::NAN, 0.0, 0.5, 0.25], 2, SampleFormat::I16, &mut buf);
        let s: Vec<i16> = buf[..n].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(s, [32767, -32767, 0, 0, 16384, 8192]);
        let n = encode(&[0.5, 0.25, -1.0, 1.0], 1, SampleFormat::I24, &mut buf);
        assert_eq!(n, 6);
        let mut back = Vec::new();
        decode_stereo(&buf[..n], 1, SampleFormat::I24, &mut back);
        assert!((back[0] - 0.375).abs() < 1e-6 && back[0] == back[1] && back[2].abs() < 1e-6);
        // 24-bit sign extension and an 8-channel stream (only 1 and 2 are used)
        let mut eight = Vec::new();
        for ch in 0..8i32 {
            eight.extend_from_slice(&(-(ch + 1) * 1000).to_le_bytes()[..3]);
        }
        let mut back = Vec::new();
        decode_stereo(&eight, 8, SampleFormat::I24, &mut back);
        assert_eq!(back, [-1000.0 / 8_388_608.0, -2000.0 / 8_388_608.0]);
    }

    #[test]
    fn names() {
        assert!(validate_name("Stream1").is_ok());
        assert!(validate_name("Mic from PC 2").is_ok());
        assert!(validate_name("0123456789abcdef").is_ok());
        assert!(validate_name("0123456789abcdefg").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(" x").is_err());
        assert!(validate_name("Stréam").is_err());
        assert_eq!(&name_field("Stream1")[..8], b"Stream1\0");
        assert_eq!(name_field("0123456789abcdef"), *b"0123456789abcdef");
    }
}
