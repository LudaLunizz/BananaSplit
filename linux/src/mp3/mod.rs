//! MP3 (MPEG audio Layer III): the recorder's encoder, the player's decoder,
//! and what both need to agree on about files.
//!
//! * [`enc::Encoder`] writes MPEG-1 Layer III at 32, 44.1 or 48 kHz, CBR,
//!   joint stereo, starting with an Info frame whose LAME tag says exactly
//!   how many samples the file holds (encoder delay and padding), so a take
//!   split into hourly parts plays back without a gap or an extra sample in
//!   any player that reads the tag (and in BananaSplit's).
//! * [`dec::Decoder`] decodes MPEG-1, 2 and 2.5 Layer III frames.
//! * [`probe`] and [`Reader`] find the frames of a file (after an ID3v2 tag,
//!   before ID3v1 or APE tags), read its Info/Xing/VBRI header, and decode
//!   it from any sample on.
//!
//! No dependencies, no `unsafe`, like the rest of BananaSplit.

pub mod dec;
pub mod enc;
pub mod tables;

use dec::{Decoder, Header};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// MP3 can store these sample rates directly (MPEG-1).
pub fn supported_rate(rate: u32) -> bool {
    enc::rate_index(rate).is_some()
}

/// The MPEG-1 rate a recording at `rate` is stored at: the same if MP3 has
/// it, else the one it converts to most simply (96 and 192 kHz -> 48 kHz,
/// 88.2 kHz -> 44.1 kHz, 16 kHz -> 32 kHz).
pub fn rate_for(rate: u32) -> u32 {
    if supported_rate(rate) {
        return rate;
    }
    // a whole multiple converts down most simply, a whole fraction up
    if let Some(r) = [48000, 44100, 32000].into_iter().find(|r| rate.is_multiple_of(*r)) {
        return r;
    }
    if let Some(r) = [32000, 44100, 48000].into_iter().find(|r| r % rate.max(1) == 0) {
        return r;
    }
    if rate > 44100 {
        48000
    } else if rate > 32000 {
        44100
    } else {
        32000
    }
}

/// The MPEG-1 bitrates BananaSplit records at (kbit/s, stereo).
pub const BITRATES: [u32; 6] = [128, 160, 192, 224, 256, 320];
pub const DEFAULT_KBPS: u32 = 320;

// ---------------------------------------------------------------------------
// File information
// ---------------------------------------------------------------------------

/// What an MP3 file's headers say.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bitrate of the first audio frame.
    pub kbps: u32,
    /// The bitrate changes from frame to frame (a "Xing" header, or frames
    /// that differ).
    pub vbr: bool,
    /// Samples per channel in each frame (1152, or 576 for MPEG-2/2.5).
    pub spf: u32,
    /// File offset of the first audio frame (after any ID3v2 tag and
    /// Info/Xing frame), and of the end of the audio (before ID3v1/APE tags).
    pub audio_start: u64,
    pub audio_end: u64,
    /// Audio frames (counted by the Info/Xing/VBRI header, else estimated).
    pub frames: u64,
    /// Samples a decoder gives: with a LAME tag, exactly what the encoder
    /// was given (its delay and padding removed).
    pub samples: u64,
    /// Decoded samples before the first real one (encoder delay + the
    /// decoder's 529), when the LAME tag says so; else 0.
    pub skip: u64,
    /// `frames` and `samples` come from the file's header, not an estimate.
    pub exact: bool,
    /// Xing table of contents (byte position per percent of the duration).
    toc: Option<Vec<u8>>,
    /// All audio frames have the length `spf * kbps / 8 / rate` (with padding).
    cbr: bool,
}

impl Info {
    pub fn secs(&self) -> f64 {
        self.samples as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Byte length of the first `n` frames of a CBR stream (padding as
    /// encoders insert it).
    fn cbr_offset(&self, n: u64) -> u64 {
        let per: u64 = if self.spf == 1152 { 144_000 } else { 72_000 };
        n * per * u64::from(self.kbps) / u64::from(self.sample_rate.max(1))
    }
}

/// Syncsafe 28-bit integer of an ID3v2 header.
fn syncsafe(b: &[u8]) -> u64 {
    b.iter().take(4).fold(0, |a, &x| (a << 7) | u64::from(x & 0x7F))
}

/// A header at `b[0..]` that is followed by another frame of the same stream
/// (or by the end of the data, if `last_ok`).
fn frame_at(b: &[u8], last_ok: bool) -> Option<Header> {
    let h = Header::parse(b)?;
    match b.get(h.frame_len..h.frame_len + 4) {
        Some(next) => Header::parse(next).filter(|n| n.same_stream(&h)).map(|_| h),
        None => (last_ok && b.len() >= h.frame_len.min(b.len())).then_some(h),
    }
}

/// Read the headers of `path` (fast: the first frames and the tags only).
pub fn probe(path: &Path) -> Result<Info, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    // ---- tags at the end: ID3v1 (128 bytes) and APEv2 ----
    let mut audio_end = len;
    let mut tail = [0u8; 160];
    if len >= 160 {
        f.seek(SeekFrom::Start(len - 160)).map_err(|e| e.to_string())?;
        f.read_exact(&mut tail).map_err(|e| e.to_string())?;
        if &tail[32..35] == b"TAG" {
            audio_end -= 128;
        }
        // an APE footer ends right before ID3v1, or at the end
        let foot = if audio_end < len { &tail[0..32] } else { &tail[128..160] };
        if &foot[..8] == b"APETAGEX" {
            let size = u64::from(u32::from_le_bytes([foot[12], foot[13], foot[14], foot[15]]));
            let header = if foot[23] & 0x80 != 0 { 32 } else { 0 };
            audio_end = audio_end.saturating_sub(size + header);
        }
    }
    // ---- ID3v2 at the start ----
    let mut start = 0u64;
    let mut head = [0u8; 10];
    for _ in 0..8 {
        f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        if f.read_exact(&mut head).is_err() || &head[..3] != b"ID3" {
            break;
        }
        start += 10 + syncsafe(&head[6..10]) + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    // ---- the first frame (within 128 KiB) ----
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.by_ref().take(128 * 1024).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let to_end = start + buf.len() as u64 >= audio_end;
    let (pos, h) = (0..buf.len().saturating_sub(4))
        .find_map(|i| frame_at(&buf[i..], to_end).map(|h| (i, h)))
        .ok_or("not an MP3 file (no MPEG Layer III frames)")?;
    let first = start + pos as u64;
    let spf = h.samples() as u32;
    let mut info = Info {
        sample_rate: h.sample_rate,
        channels: h.channels() as u16,
        kbps: h.kbps,
        vbr: false,
        spf,
        audio_start: first,
        audio_end: audio_end.max(first),
        frames: 0,
        samples: 0,
        skip: 0,
        exact: false,
        toc: None,
        cbr: true,
    };
    // ---- an Info/Xing header in the first frame, or a VBRI header ----
    let fr = &buf[pos..(pos + h.frame_len).min(buf.len())];
    let x = h.main_offset();
    let mut tag_frames = None;
    let mut gapless = None;
    if fr.len() >= x + 8 && (&fr[x..x + 4] == b"Xing" || &fr[x..x + 4] == b"Info") {
        info.vbr = &fr[x..x + 4] == b"Xing";
        let flags = u32::from_be_bytes([fr[x + 4], fr[x + 5], fr[x + 6], fr[x + 7]]);
        let mut p = x + 8;
        let be32 = |p: usize| fr.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        if flags & 1 != 0 {
            tag_frames = be32(p).map(u64::from);
            p += 4;
        }
        if flags & 2 != 0 {
            p += 4;
        }
        if flags & 4 != 0 {
            info.toc = fr.get(p..p + 100).map(<[u8]>::to_vec);
            p += 100;
        }
        if flags & 8 != 0 {
            p += 4;
        }
        // LAME tag: delay and padding, read when the encoder says it is one
        // (as ffmpeg does)
        if let Some(v) = fr.get(p..p + 24)
            && [&b"LAME"[..], b"Lavf", b"Lavc"].contains(&&v[..4])
        {
            let delay = (u64::from(v[21]) << 4) | u64::from(v[22] >> 4);
            let padding = (u64::from(v[22] & 15) << 8) | u64::from(v[23]);
            gapless = Some((delay, padding));
        }
        info.audio_start = first + h.frame_len as u64;
    } else if fr.len() >= 36 + 18 && &fr[36..40] == b"VBRI" {
        info.vbr = true;
        tag_frames = Some(u64::from(u32::from_be_bytes([fr[50], fr[51], fr[52], fr[53]])));
        info.audio_start = first + h.frame_len as u64;
    }
    // ---- are the frames constant? compare a few headers through the file ----
    let bytes = info.audio_end.saturating_sub(info.audio_start);
    if !info.vbr {
        // the first frame after any Info frame, and frames at 1/3 and 2/3:
        // where constant frames would start (within a byte: encoders pad
        // a frame earlier or later)
        let per: u64 = if spf == 1152 { 144_000 } else { 72_000 };
        let est = |third: u64| bytes * third / 3 * u64::from(h.sample_rate) / (per * u64::from(h.kbps)).max(1);
        let mut hb = [0u8; 6];
        for n in [0, est(1), est(2)] {
            let off = info.audio_start + info.cbr_offset(n);
            if off + 5 > info.audio_end {
                continue;
            }
            f.seek(SeekFrom::Start(off - u64::from(off > 0))).map_err(|e| e.to_string())?;
            let ok = f.read_exact(&mut hb).is_ok()
                && (0..3).any(|d| Header::parse(&hb[d..]).is_some_and(|o| o.same_stream(&h) && o.kbps == h.kbps));
            if !ok {
                info.cbr = false;
                info.vbr = true;
            }
        }
    } else {
        info.cbr = false;
    }
    // ---- frame and sample counts ----
    info.frames = match tag_frames {
        Some(n) if n > 0 => {
            info.exact = true;
            n
        }
        _ => {
            if info.cbr {
                // exact for constant frames: the largest n with offset(n) <= bytes
                let per: u64 = if spf == 1152 { 144_000 } else { 72_000 };
                let mut n = bytes * u64::from(h.sample_rate) / (per * u64::from(h.kbps)).max(1);
                while n > 0 && info.cbr_offset(n) > bytes {
                    n -= 1;
                }
                while info.cbr_offset(n + 1) <= bytes {
                    n += 1;
                }
                n
            } else {
                // VBR without a count: from the first frame's bitrate
                bytes / (h.frame_len as u64).max(1)
            }
        }
    };
    let total = info.frames * u64::from(spf);
    match gapless {
        Some((delay, padding)) if delay + padding <= total => {
            info.skip = delay + u64::from(enc::DECODER_DELAY);
            info.samples = total - delay - padding;
        }
        _ => info.samples = total,
    }
    Ok(info)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Decodes a file as stereo `f32` frames from any sample on.
pub struct Reader {
    file: File,
    info: Info,
    dec: Decoder,
    /// Decoded samples (L R …) not handed out yet, from `pcm_pos`.
    pcm: Vec<f32>,
    pcm_pos: usize,
    /// Index of the next frame to decode and where it starts in the file.
    next_frame: u64,
    file_pos: u64,
    /// Stream sample (counted from the stream's start, encoder delay
    /// included) of `pcm[0]`.
    pcm_start: u64,
    /// The next sample handed out (after the encoder delay is skipped).
    out: u64,
    buf: Vec<u8>,
    frame: Vec<u8>,
    /// Bytes of `buf` that hold file data from `buf_at`.
    buf_at: u64,
    buf_len: usize,
    eof: bool,
}

const READ_CHUNK: usize = 64 * 1024;

impl Reader {
    pub fn open(path: &Path, info: &Info) -> Result<Reader, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut r = Reader {
            file,
            info: info.clone(),
            dec: Decoder::new(),
            pcm: Vec::with_capacity(2 * 1152 * 4),
            pcm_pos: 0,
            next_frame: 0,
            file_pos: info.audio_start,
            pcm_start: 0,
            out: 0,
            buf: vec![0; READ_CHUNK],
            frame: Vec::with_capacity(2048),
            buf_at: 0,
            buf_len: 0,
            eof: false,
        };
        r.seek(0)?;
        Ok(r)
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// `n` bytes of the file at `at` (fewer at its end), buffered.
    fn bytes_at(&mut self, at: u64, n: usize) -> Result<&[u8], String> {
        let have = at >= self.buf_at && at + n as u64 <= self.buf_at + self.buf_len as u64;
        if !have {
            self.file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            let mut got = 0;
            while got < READ_CHUNK {
                match self.file.read(&mut self.buf[got..]) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            self.buf_at = at;
            self.buf_len = got;
        }
        let o = (at - self.buf_at) as usize;
        Ok(&self.buf[o..(o + n).min(self.buf_len)])
    }

    /// The frame header at `at`, if one is there (and inside the audio).
    fn header_at(&mut self, at: u64) -> Result<Option<Header>, String> {
        if at + 4 > self.info.audio_end {
            return Ok(None);
        }
        let b = self.bytes_at(at, 4)?;
        Ok(Header::parse(b)
            .filter(|h| h.sample_rate == self.info.sample_rate && h.channels() == usize::from(self.info.channels)))
    }

    /// Look for a frame at or after `at` (within 64 KiB), confirmed by the one after it.
    fn sync_from(&mut self, at: u64) -> Result<Option<u64>, String> {
        let end = (at + READ_CHUNK as u64).min(self.info.audio_end);
        let mut p = at;
        while p + 4 <= end {
            if let Some(h) = self.header_at(p)? {
                let next = p + h.frame_len as u64;
                if next + 4 > self.info.audio_end || self.header_at(next)?.is_some() {
                    return Ok(Some(p));
                }
            }
            p += 1;
        }
        Ok(None)
    }

    /// Continue at sample `sample` (0 = the first real one).
    pub fn seek(&mut self, sample: u64) -> Result<(), String> {
        let spf = u64::from(self.info.spf);
        let target = sample.min(self.info.samples) + self.info.skip;
        let k = target / spf;
        // frames decoded first and thrown away: the bit reservoir (up to 511
        // bytes of earlier frames) and the filters' memory must be there
        let main = (self.info.cbr_offset(1).max(1)).saturating_sub(36).max(16);
        let preroll = 2 + 511u64.div_ceil(main);
        let start = k.saturating_sub(preroll);
        let pos = if self.info.cbr {
            let est = self.info.audio_start + self.info.cbr_offset(start);
            // exact for encoders that pad like the standard; else the frame
            // that starts within a byte of it
            let mut found = None;
            for d in [0i64, 1, -1] {
                let p = est as i64 + d;
                if p >= self.info.audio_start as i64 && self.header_at(p as u64)?.is_some() {
                    found = Some(p as u64);
                    break;
                }
            }
            match found {
                Some(p) => p,
                None => self.sync_from(est)?.unwrap_or(self.info.audio_end),
            }
        } else {
            // VBR: from the table of contents, else in proportion
            let frames = self.info.frames.max(1);
            let bytes = self.info.audio_end.saturating_sub(self.info.audio_start);
            let est = match &self.info.toc {
                Some(toc) if start > 0 => {
                    let pct = ((start * 100) / frames).min(99) as usize;
                    bytes * u64::from(toc[pct]) / 256
                }
                _ => bytes * start / frames,
            };
            let at = self.info.audio_start + est;
            if start == 0 { self.info.audio_start } else { self.sync_from(at)?.unwrap_or(self.info.audio_end) }
        };
        self.dec.reset();
        self.pcm.clear();
        self.pcm_pos = 0;
        self.next_frame = start;
        self.file_pos = pos;
        self.pcm_start = start * spf;
        self.eof = pos >= self.info.audio_end;
        self.out = target - self.info.skip;
        // decode the frames before the target's and throw them away, then
        // the target's, from the target on
        loop {
            self.pcm.clear();
            self.pcm_pos = 0;
            if !self.decode_next()? {
                break;
            }
            let n = (self.pcm.len() / 2) as u64;
            if self.pcm_start + n > target {
                self.pcm_pos = (target.saturating_sub(self.pcm_start)) as usize;
                break;
            }
            self.pcm_start += n;
        }
        Ok(())
    }

    /// Decode the next frame into `pcm` (appended); false at the end.
    fn decode_next(&mut self) -> Result<bool, String> {
        loop {
            if self.file_pos + 4 > self.info.audio_end {
                self.eof = true;
                return Ok(false);
            }
            let Some(h) = self.header_at(self.file_pos)? else {
                // junk between frames: find the next one
                match self.sync_from(self.file_pos + 1)? {
                    Some(p) => {
                        self.file_pos = p;
                        continue;
                    }
                    None => {
                        self.eof = true;
                        return Ok(false);
                    }
                }
            };
            let at = self.file_pos;
            let n = h.frame_len;
            let mut frame = std::mem::take(&mut self.frame);
            frame.clear();
            frame.extend_from_slice(self.bytes_at(at, n)?);
            self.file_pos += n as u64;
            self.next_frame += 1;
            let ok = self.dec.decode(&frame, &mut self.pcm).is_some();
            self.frame = frame;
            if ok {
                return Ok(true);
            }
        }
    }

    /// Append up to `max` stereo frames (L R …) to `out`; returns how many
    /// (fewer only at the end).
    pub fn read(&mut self, max: usize, out: &mut Vec<f32>) -> Result<usize, String> {
        let mut done = 0;
        while done < max {
            let avail = self.pcm.len() / 2 - self.pcm_pos;
            if avail == 0 {
                if self.eof {
                    break;
                }
                self.pcm_start += (self.pcm.len() / 2) as u64;
                self.pcm.clear();
                self.pcm_pos = 0;
                if !self.decode_next()? {
                    break;
                }
                // (still inside the encoder's delay: only after a seek
                // into a stream shorter than it)
                let n = (self.pcm.len() / 2) as u64;
                if self.pcm_start < self.info.skip {
                    self.pcm_pos = (self.info.skip - self.pcm_start).min(n) as usize;
                }
                continue;
            }
            // never past the end the header gives (the encoder's padding)
            let left = if self.info.exact { self.info.samples.saturating_sub(self.out) } else { u64::MAX };
            if left == 0 {
                break;
            }
            let k = avail.min(max - done).min(left.min(usize::MAX as u64) as usize);
            out.extend_from_slice(&self.pcm[2 * self.pcm_pos..2 * (self.pcm_pos + k)]);
            self.pcm_pos += k;
            self.out += k as u64;
            done += k;
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::enc::{CODEC_DELAY, DECODER_DELAY, Encoder, FRAME, crc16};
    use super::*;
    use std::f64::consts::PI;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bs-mp3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A whole file as the recorder writes it: Info frame, frames, Info again.
    fn encode(rate: u32, kbps: u32, lr: &[f32]) -> Vec<u8> {
        let mut e = Encoder::new(rate, kbps).unwrap();
        let first = e.info_frame();
        let mut body = Vec::new();
        for c in lr.chunks(2 * 1000) {
            e.encode(c, &mut body);
        }
        e.finish(&mut body);
        let last = e.info_frame();
        assert_eq!(first.len(), last.len());
        let mut f = last;
        f.extend_from_slice(&body);
        f
    }

    fn signal(rate: u32, n: usize) -> Vec<f32> {
        // two tones and a little noise, different on each side
        let mut seed = 7u32;
        (0..n)
            .flat_map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 9) as f64 / (1u64 << 23) as f64 - 0.5;
                let t = i as f64 / f64::from(rate);
                let a = 0.4 * (2.0 * PI * 1000.0 * t).sin() + 0.01 * noise;
                let b = 0.3 * (2.0 * PI * 3100.0 * t).sin() + 0.2 * a;
                [a as f32, b as f32]
            })
            .collect()
    }

    fn read_all(path: &Path) -> (Info, Vec<f32>) {
        let info = probe(path).unwrap();
        let mut r = Reader::open(path, &info).unwrap();
        let mut out = Vec::new();
        while r.read(777, &mut out).unwrap() > 0 {}
        (info, out)
    }

    #[test]
    fn mp3_stores_these_rates_and_converts_the_rest_simply() {
        for r in [32000, 44100, 48000] {
            assert!(supported_rate(r));
            assert_eq!(rate_for(r), r);
        }
        for (r, want) in [
            (96000, 48000),
            (192000, 48000),
            (384000, 48000),
            (88200, 44100),
            (176400, 44100),
            (64000, 32000),
            (16000, 32000),
            (8000, 32000),
            (22050, 44100),
            (11025, 44100),
            (24000, 48000),
            (12000, 48000),
            (50000, 48000),
            (46000, 48000),
            (40000, 44100),
            (30000, 32000),
        ] {
            assert!(!supported_rate(r));
            assert_eq!(rate_for(r), want, "{r}");
        }
        assert!(Encoder::new(96000, 320).is_err() && Encoder::new(48000, 300).is_err());
    }

    /// What goes in comes out: the same number of samples (the LAME tag's
    /// delay and padding removed), the same level, little noise.
    #[test]
    fn round_trip_keeps_length_level_and_timing() {
        let d = tmpdir("roundtrip");
        for rate in [32000, 44100, 48000] {
            for kbps in [128, 320] {
                let n = 61_237;
                let src = signal(rate, n);
                let p = d.join(format!("{rate}-{kbps}.mp3"));
                std::fs::write(&p, encode(rate, kbps, &src)).unwrap();
                let (info, out) = read_all(&p);
                assert_eq!((info.sample_rate, info.channels, info.kbps), (rate, 2, kbps));
                assert!(info.exact && !info.vbr);
                assert_eq!(info.samples, n as u64);
                assert_eq!(info.skip, u64::from(CODEC_DELAY));
                assert_eq!(out.len(), 2 * n, "{rate} {kbps}");
                // least-squares gain of the left channel, and what is left over
                let (mut xy, mut xx, mut res) = (0f64, 0f64, 0f64);
                for i in (2000..n - 2000).map(|i| 2 * i) {
                    xy += f64::from(src[i]) * f64::from(out[i]);
                    xx += f64::from(src[i]) * f64::from(src[i]);
                }
                let g = xy / xx;
                for i in (2000..n - 2000).map(|i| 2 * i) {
                    res += (f64::from(out[i]) - g * f64::from(src[i])).powi(2);
                }
                let snr = 10.0 * (xx / res).log10();
                assert!((g - 1.0).abs() < 2e-3, "{rate} {kbps}: gain {g}");
                assert!(snr > if kbps == 320 { 40.0 } else { 30.0 }, "{rate} {kbps}: SNR {snr:.1} dB");
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn short_and_empty_streams_keep_their_length() {
        let d = tmpdir("short");
        for n in [0usize, 1, 575, 576, 1151, 1152, 1153, 2304, 4000] {
            let p = d.join(format!("{n}.mp3"));
            std::fs::write(&p, encode(48000, 320, &signal(48000, n))).unwrap();
            let (info, out) = read_all(&p);
            assert_eq!((info.samples, out.len()), (n as u64, 2 * n), "{n}");
            assert!(out.iter().all(|v| v.is_finite()));
            // padding leaves room for the decoder's delay, and fits its field
            let frames = info.frames;
            let padding = frames * FRAME as u64 - (u64::from(CODEC_DELAY - DECODER_DELAY) + n as u64);
            assert!(
                (u64::from(DECODER_DELAY)..u64::from(DECODER_DELAY) + FRAME as u64).contains(&padding),
                "{n}: {padding}"
            );
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// A seek lands on exactly the sample a straight read gives there.
    #[test]
    fn seeking_is_sample_exact() {
        let d = tmpdir("seek");
        for rate in [44100, 48000] {
            let n = 3 * rate as usize + 123;
            let p = d.join(format!("{rate}.mp3"));
            std::fs::write(&p, encode(rate, 256, &signal(rate, n))).unwrap();
            let (info, all) = read_all(&p);
            assert_eq!(all.len(), 2 * n);
            let mut r = Reader::open(&p, &info).unwrap();
            for pos in [0usize, 1, 529, 1151, 1152, 1153, 4607, 47999, 100_000, n - 3000, n - 1, n] {
                r.seek(pos as u64).unwrap();
                let mut got = Vec::new();
                while got.len() < 2 * 3000 && r.read(3000 - got.len() / 2, &mut got).unwrap() > 0 {}
                let want = &all[2 * pos..(2 * (pos + 3000)).min(all.len())];
                assert_eq!(got.len(), want.len(), "{rate}: at {pos}");
                assert!(got == want, "{rate}: samples differ after a seek to {pos}");
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn info_frame_carries_the_lame_tag() {
        let n = 10_000;
        let file = encode(48000, 320, &signal(48000, n));
        let f = &file[..960];
        let h = dec::Header::parse(f).unwrap();
        assert_eq!((h.sample_rate, h.kbps, h.frame_len, h.padding), (48000, 320, 960, false));
        assert!(f[4..36].iter().all(|&b| b == 0), "the side info of silence");
        assert_eq!(&f[36..40], b"Info");
        let be = |o: usize| u32::from_be_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]);
        assert_eq!(be(40), 0x0F);
        let frames = be(44) as usize;
        assert_eq!(frames * 960 + 960, file.len());
        assert_eq!(be(48) as usize, file.len());
        assert_eq!(&f[156..160], b"LAME");
        let delay = (u32::from(f[177]) << 4) | u32::from(f[178] >> 4);
        let padding = (u32::from(f[178] & 15) << 8) | u32::from(f[179]);
        assert_eq!(delay, CODEC_DELAY - DECODER_DELAY);
        assert_eq!(frames * 1152, delay as usize + padding as usize + n);
        assert_eq!(be(184) as usize, file.len());
        assert_eq!(u16::from_be_bytes([f[188], f[189]]), crc16(0, &file[960..]), "music CRC");
        assert_eq!(u16::from_be_bytes([f[190], f[191]]), crc16(0, &f[..190]), "tag CRC");
    }

    /// 44.1 kHz frames are 1044 or 1045 bytes, padded the standard way.
    #[test]
    fn frames_have_the_standard_lengths() {
        let file = encode(44100, 320, &signal(44100, 44100));
        let mut p = 1044; // after the Info frame (never padded)
        let mut n = 0u64;
        while p < file.len() {
            let h = dec::Header::parse(&file[p..]).unwrap();
            let want = ((n + 1) * 46_080_000 / 44100 - n * 46_080_000 / 44100) as usize;
            assert_eq!(h.frame_len, want, "frame {n}");
            assert_eq!(h.padding, want == 1045);
            p += h.frame_len;
            n += 1;
        }
        assert_eq!(p, file.len());
    }

    #[test]
    fn tags_around_the_frames_are_skipped() {
        let d = tmpdir("tags");
        let n = 20_000;
        let body = encode(48000, 192, &signal(48000, n));
        let plain = d.join("plain.mp3");
        std::fs::write(&plain, &body).unwrap();
        let (_, want) = read_all(&plain);
        // an ID3v2 tag (with a fake frame sync inside), ID3v1, APEv2
        let mut id3 = b"ID3\x04\x00\x00\x00\x00\x08\x00".to_vec();
        id3.extend((0..1024).map(|i| if i % 97 == 0 { 0xFF } else { (i * 7) as u8 }));
        let mut ape = b"APETAGEX\xd0\x07\x00\x00".to_vec();
        ape.extend_from_slice(&32u32.to_le_bytes());
        ape.extend_from_slice(&[0; 12]);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, b'x');
        for (name, pre, post) in [
            ("id3v2", id3.clone(), vec![]),
            ("id3v1", vec![], v1.clone()),
            ("ape", vec![], ape.clone()),
            ("all", id3.clone(), [ape, v1].concat()),
        ] {
            let p = d.join(format!("{name}.mp3"));
            std::fs::write(&p, [pre, body.clone(), post].concat()).unwrap();
            let (info, out) = read_all(&p);
            assert!(info.exact, "{name}");
            assert_eq!(out, want, "{name}");
        }
        // no Info frame: counted from the file's length, nothing trimmed
        let raw = d.join("raw.mp3");
        let info_len = 144_000 * 192 / 48000;
        std::fs::write(&raw, &body[info_len..]).unwrap();
        let (info, out) = read_all(&raw);
        let frames = (body.len() - info_len) / info_len;
        assert!(!info.exact && !info.vbr);
        assert_eq!((info.frames, info.samples), (frames as u64, frames as u64 * 1152));
        assert_eq!(out.len(), 2 * frames * 1152);
        // the same audio, CODEC_DELAY later
        let skip = 2 * CODEC_DELAY as usize;
        assert_eq!(&out[skip..skip + 2 * n], &want[..]);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Damaged files never panic, and decode to finite samples.
    #[test]
    fn damaged_input_never_panics() {
        let d = tmpdir("damaged");
        let good = encode(48000, 320, &signal(48000, 30_000));
        let mut seed = 99u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        let p = d.join("x.mp3");
        for round in 0..60 {
            let mut f = good.clone();
            match round % 4 {
                0 => f.truncate(rnd() as usize % good.len()),
                1 => {
                    for _ in 0..1 + rnd() % 40 {
                        let i = rnd() as usize % f.len();
                        f[i] = rnd() as u8;
                    }
                }
                2 => f = (0..rnd() % 5000).map(|_| rnd() as u8).collect(),
                _ => {
                    // valid headers in front of random payloads
                    f.clear();
                    for _ in 0..20 {
                        f.extend_from_slice(&[0xFF, 0xFB, 0x94, 0x64]);
                        f.extend((0..rnd() % 1200).map(|_| rnd() as u8));
                    }
                }
            }
            std::fs::write(&p, &f).unwrap();
            let mut dec = dec::Decoder::new();
            let mut pcm = Vec::new();
            for i in 0..f.len() {
                if dec::Header::parse(&f[i..]).is_some() {
                    dec.decode(&f[i..], &mut pcm);
                }
            }
            assert!(pcm.iter().all(|v| v.is_finite()));
            if let Ok(info) = probe(&p)
                && let Ok(mut r) = Reader::open(&p, &info)
            {
                let mut out = Vec::new();
                let _ = r.read(100_000, &mut out);
                let _ = r.seek(u64::from(rnd() % 50_000));
                let _ = r.read(5000, &mut out);
                assert!(out.iter().all(|v| v.is_finite()));
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// Mid/side for the same sound on both sides, left/right for a sound on one.
    #[test]
    fn mid_side_only_where_it_helps() {
        let n = 20_000;
        let mono: Vec<f32> = signal(48000, n).chunks(2).flat_map(|f| [f[0], f[0]]).collect();
        let left: Vec<f32> = signal(48000, n).chunks(2).flat_map(|f| [f[0], 0.0]).collect();
        for (lr, want_ms) in [(mono, true), (left, false)] {
            let file = encode(48000, 320, &lr);
            let mut p = 960;
            let (mut ms, mut all) = (0, 0);
            while p < file.len() {
                let h = dec::Header::parse(&file[p..]).unwrap();
                ms += usize::from(h.mode_ext & 2 != 0);
                all += 1;
                p += h.frame_len;
            }
            assert_eq!(ms == all, want_ms, "{ms} of {all} frames mid/side");
        }
    }

    /// Clicks after silence: short blocks around them, and little noise
    /// before them (pre-echo).
    #[test]
    fn attacks_get_short_blocks() {
        let d = tmpdir("attack");
        let rate = 48000;
        let n = rate as usize;
        let src: Vec<f32> = (0..n)
            .flat_map(|i| {
                let k = i % 12_000;
                let v = if k < 1500 { 0.7 * (-(k as f64) / 150.0).exp() * (k as f64 * 0.45).sin() } else { 0.0 };
                [v as f32, v as f32]
            })
            .collect();
        let file = encode(rate, 320, &src);
        // block types from the side info
        let mut p = 960;
        let mut short = 0;
        while p < file.len() {
            let h = dec::Header::parse(&file[p..]).unwrap();
            let s = &file[p + 4..p + 36];
            let bit = |k: usize| (s[k / 8] >> (7 - k % 8)) & 1;
            let mut k = 20;
            for _ in 0..4 {
                k += 12 + 9 + 8 + 4;
                if bit(k) == 1 && bit(k + 1) == 1 && bit(k + 2) == 0 {
                    short += 1;
                }
                k += 1 + 22 + 3;
            }
            p += h.frame_len;
        }
        assert!(short >= 4 * 2, "{short} short granules for 4 clicks");
        let path = d.join("clicks.mp3");
        std::fs::write(&path, &file).unwrap();
        let (_, out) = read_all(&path);
        // from 20 to 4 ms before each click: below -55 dB of its peak (LAME
        // 3.100 measures the same here; the last few ms hold the click's own
        // onset, smeared by the filterbank, in any MP3)
        for c in 1..4 {
            let at = c * 12_000;
            let pre = (at - 960..at - 192).map(|i| out[2 * i].abs()).fold(0f32, f32::max);
            assert!(pre < 0.7 * 0.0018, "click {c}: {pre} before it");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn samples_beyond_full_scale_are_clipped_and_nan_is_silence() {
        let d = tmpdir("clip");
        let lr: Vec<f32> =
            (0..10_000).flat_map(|i| if i % 100 == 0 { [f32::NAN, f32::INFINITY] } else { [3.0, -3.0] }).collect();
        let p = d.join("x.mp3");
        std::fs::write(&p, encode(48000, 320, &lr)).unwrap();
        let (_, out) = read_all(&p);
        assert_eq!(out.len(), 20_000);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() < 1.3));
        let _ = std::fs::remove_dir_all(d);
    }
}
