//! Player: plays a recording back into the buses.
//!
//! A *take* is what one press of record produced: one stereo WAV file in
//! `mix` mode, one per linked channel in `multi` mode, each possibly split
//! into `_part2`, `_part3` … files. [`scan`] groups the WAV files of the
//! recorder folder into takes; files that were not made by BananaSplit are
//! takes of their own. Playing a multi-track take plays the sum of its
//! tracks, like the `mix` recording would have been.
//!
//! ```text
//!  player thread                                  audio thread (Engine::process)
//!  read WAV ─► decode ─► sum tracks ─► resample ─► SPSC ring ─► PlayRt::mix ─► buses (routes)
//! ```
//!
//! The audio thread only reads the preallocated ring and atomics: no
//! allocation, locks or system calls. Playback fades in and out over one
//! period (start, pause, stop, seek, and if the disk cannot keep up), and a
//! routing change is ramped like a strip's, so nothing clicks. Played
//! audio joins the buses before their EQ, gain and fader, so a bus that is
//! recorded or sent over VBAN carries it too.
//!
//! WAV support: PCM 8/16/24/32-bit and IEEE float 32/64-bit, plain or
//! `WAVE_FORMAT_EXTENSIBLE`, any number of channels (1 and 2 are used; mono
//! plays on both sides), any sample rate up to four times the mixer's
//! (resampled with the VBAN receiver's windowed-sinc resampler; at the
//! mixer's own rate the samples are played unchanged).

use crate::engine::{MAX_FRAMES, NUM_BUSES};
use crate::recorder::{NUM_SOURCES, source_id};
use crate::vban::resample::{MAX_DOWN, Resampler};
use crate::vban::ring::Ring;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Ring size in samples (stereo): 1.36 s at 48 kHz.
pub const RING_SAMPLES: usize = 1 << 17;
/// Frames buffered before playback starts or resumes (85 ms at 48 kHz).
pub const START_FRAMES: usize = 4096;
/// Frames read from the files at a time.
const CHUNK: usize = 4096;
/// The lowest sample rate played (the lowest VBAN rate, telephone audio).
pub const MIN_RATE: u32 = 8000;

// ===========================================================================
// Real-time side
// ===========================================================================

/// Shared between the player thread and the engine.
pub struct PlayTap {
    ring: Ring,
    /// Buses the player feeds (bit b = bus b, A1 = bit 0).
    pub routes: AtomicU8,
    /// The engine plays while this is set (pause = clear it).
    run: AtomicBool,
    /// The player has pushed the last sample of the take.
    eof: AtomicBool,
    /// Flush handshake: the player bumps `flush_req`; the engine empties
    /// the ring, zeroes `played` and copies the value to `flush_ack`.
    flush_req: AtomicU32,
    flush_ack: AtomicU32,
    /// Frames handed to the buses since the last flush.
    played: AtomicU64,
    /// The ring ran dry while playing (the disk could not keep up).
    underruns: AtomicU32,
    /// The engine is currently playing (not waiting for data, not paused).
    playing: AtomicBool,
    /// The engine's sample rate (0 = not known yet). Set by the engine;
    /// when it changes, the player converts the take to the new rate.
    pub rate: AtomicU32,
}

impl PlayTap {
    pub fn new() -> Arc<PlayTap> {
        Arc::new(PlayTap {
            ring: Ring::new(RING_SAMPLES),
            routes: AtomicU8::new(1),
            run: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            flush_req: AtomicU32::new(0),
            flush_ack: AtomicU32::new(0),
            played: AtomicU64::new(0),
            underruns: AtomicU32::new(0),
            playing: AtomicBool::new(false),
            rate: AtomicU32::new(0),
        })
    }

    pub fn underruns(&self) -> u32 {
        self.underruns.load(Relaxed)
    }

    // ---- for tests that drive the engine without the player thread ----

    #[doc(hidden)]
    pub fn test_push(&self, frames: &[f32]) -> bool {
        self.ring.push(frames)
    }

    #[doc(hidden)]
    pub fn test_run(&self, run: bool, eof: bool) {
        self.eof.store(eof, Release);
        self.run.store(run, Release);
    }

    #[doc(hidden)]
    pub fn test_flush(&self) {
        self.flush_req.fetch_add(1, Release);
    }

    #[doc(hidden)]
    pub fn test_played(&self) -> u64 {
        self.played.load(Relaxed)
    }
}

#[inline(always)]
fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

/// The engine's private player state.
#[derive(Debug, Default)]
pub struct PlayRt {
    playing: bool,
    flush_seen: u32,
    route_gain: [f32; NUM_BUSES],
}

impl PlayRt {
    #[inline]
    fn flushes(&mut self, tap: &PlayTap) {
        let req = tap.flush_req.load(Acquire);
        if req != self.flush_seen {
            tap.ring.consume(tap.ring.available());
            self.flush_seen = req;
            self.playing = false;
            tap.playing.store(false, Relaxed);
            tap.played.store(0, Relaxed);
            tap.flush_ack.store(req, Release);
        }
    }

    /// A cycle that was silenced without mixing.
    pub fn idle(&mut self, tap: &PlayTap) {
        self.flushes(tap);
    }

    /// Add this cycle's `n` frames of playback to the buses it is routed to.
    pub fn mix(&mut self, tap: &PlayTap, n: usize, out: &mut [[&mut [f32]; 2]; NUM_BUSES]) {
        let valid = n > 0 && n <= MAX_FRAMES && out.iter().all(|b| b[0].len() >= n && b[1].len() >= n);
        // A stop, seek or new take while playing: this cycle still plays what
        // the ring holds, fading out, and empties the ring after it. The
        // player writes nothing new until the flush is acknowledged.
        let fade_flush = valid && self.playing && tap.flush_req.load(Acquire) != self.flush_seen;
        if !fade_flush {
            self.flushes(tap);
        }
        if !valid {
            return;
        }
        let routes = tap.routes.load(Relaxed);
        let target: [f32; NUM_BUSES] = std::array::from_fn(|b| if routes & (1 << b) != 0 { 1.0 } else { 0.0 });
        // `eof` before `available`: once eof is seen, every sample of the
        // take is already visible, so a short ring really is the end.
        let eof = tap.eof.load(Acquire) && !fade_flush;
        let run = tap.run.load(Acquire) && !fade_flush;
        let avail = tap.ring.available() / 2;
        let fade_in = !self.playing;
        if !self.playing {
            if !run || avail == 0 || (avail < START_FRAMES.max(2 * n) && !eof) {
                self.route_gain = target;
                return;
            }
            self.playing = true;
            tap.playing.store(true, Relaxed);
        }
        let take = avail.min(n);
        let stopping = !run;
        // Less than two cycles left and more to come (the disk is not
        // keeping up): fade out now rather than stop dead next cycle.
        let under = !eof && avail < 2 * n;
        let fade_out = stopping || under;
        let ring = &tap.ring;
        for (b, bus) in out.iter_mut().enumerate() {
            let (r0, r1) = (self.route_gain[b], target[b]);
            if r0 == 0.0 && r1 == 0.0 {
                continue;
            }
            let [bl, br] = bus;
            for i in 0..take {
                let mut w = ramp(r0, r1, i, take);
                if fade_in {
                    w *= ramp(0.0, 1.0, i, take);
                }
                if fade_out {
                    w *= 1.0 - (i + 1) as f32 / take as f32;
                }
                bl[i] += ring.read(2 * i) * w;
                br[i] += ring.read(2 * i + 1) * w;
            }
        }
        self.route_gain = target;
        ring.consume(2 * take);
        tap.played.fetch_add(take as u64, Relaxed);
        if fade_out || take < n {
            self.playing = false;
            tap.playing.store(false, Relaxed);
            if under && !stopping {
                tap.underruns.fetch_add(1, Relaxed);
            }
        }
        if fade_flush {
            self.flushes(tap);
        }
    }
}

// ===========================================================================
// WAV files
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    U8,
    I16,
    I24,
    I32,
    F32,
    F64,
}

impl SampleKind {
    fn bytes(self) -> usize {
        match self {
            SampleKind::U8 => 1,
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::I32 | SampleKind::F32 => 4,
            SampleKind::F64 => 8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SampleKind::U8 => "8-bit",
            SampleKind::I16 => "16-bit",
            SampleKind::I24 => "24-bit",
            SampleKind::I32 => "32-bit",
            SampleKind::F32 => "32-bit float",
            SampleKind::F64 => "64-bit float",
        }
    }

    #[inline]
    fn decode(self, b: &[u8]) -> f32 {
        let v = match self {
            SampleKind::U8 => (b[0] as f32 - 128.0) / 128.0,
            SampleKind::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
            SampleKind::I24 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            SampleKind::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            SampleKind::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SampleKind::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        };
        if v.is_finite() { v } else { 0.0 }
    }
}

/// What a WAV header says.
#[derive(Debug, Clone, PartialEq)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub kind: SampleKind,
    pub data_offset: u64,
    pub data_bytes: u64,
}

impl WavInfo {
    pub fn block_align(&self) -> usize {
        self.channels as usize * self.kind.bytes()
    }

    pub fn frames(&self) -> u64 {
        self.data_bytes / self.block_align() as u64
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Parse the header of a RIFF/WAVE file.
pub fn read_wav_info(path: &Path) -> Result<WavInfo, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut head = [0u8; 12];
    f.read_exact(&mut head).map_err(|_| "not a WAV file (too short)".to_string())?;
    if &head[0..4] == b"RF64" || &head[0..4] == b"BW64" {
        return Err("64-bit WAV (RF64) is not supported".into());
    }
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let mut pos = 12u64;
    let mut fmt: Option<(u16, SampleKind, u32)> = None;
    for _ in 0..64 {
        if pos + 8 > len {
            break;
        }
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch).map_err(|e| e.to_string())?;
        let size = u32le(&ch, 4) as u64;
        let body = pos + 8;
        if &ch[0..4] == b"fmt " {
            if size < 16 {
                return Err("bad WAV format chunk".into());
            }
            let mut b = vec![0u8; size.min(64) as usize];
            f.read_exact(&mut b).map_err(|_| "bad WAV format chunk".to_string())?;
            let mut tag = u16le(&b, 0);
            let channels = u16le(&b, 2);
            let rate = u32le(&b, 4);
            let align = u16le(&b, 12);
            let bits = u16le(&b, 14);
            if tag == 0xFFFE {
                if b.len() < 26 {
                    return Err("bad WAV format chunk".into());
                }
                tag = u16le(&b, 24); // first two bytes of the sub-format GUID
            }
            let kind = match (tag, bits) {
                (1, 8) => SampleKind::U8,
                (1, 16) => SampleKind::I16,
                (1, 24) => SampleKind::I24,
                (1, 32) => SampleKind::I32,
                (3, 32) => SampleKind::F32,
                (3, 64) => SampleKind::F64,
                (1 | 3, _) => return Err(format!("{bits}-bit samples are not supported")),
                _ => return Err("compressed WAV files are not supported (only PCM and float)".into()),
            };
            if channels == 0 || channels > 64 {
                return Err(format!("{channels} channels is not supported"));
            }
            if rate == 0 {
                return Err("sample rate 0".into());
            }
            if align as usize != channels as usize * kind.bytes() {
                return Err("bad WAV block alignment".into());
            }
            fmt = Some((channels, kind, rate));
        } else if &ch[0..4] == b"data" {
            let (channels, kind, sample_rate) = fmt.ok_or("WAV data before its format chunk")?;
            let avail = len.saturating_sub(body);
            // 0 and 0xFFFFFFFF are what streaming writers leave behind.
            let bytes = if size == 0 || size == 0xFFFF_FFFF || size > avail { avail } else { size };
            let align = channels as u64 * kind.bytes() as u64;
            return Ok(WavInfo { channels, sample_rate, kind, data_offset: body, data_bytes: bytes - bytes % align });
        }
        pos = body + size + (size & 1);
    }
    Err(if fmt.is_some() { "WAV file has no audio data".into() } else { "WAV file has no format chunk".into() })
}

/// Reads one track (its parts one after another) as stereo f32 frames.
struct TrackReader {
    parts: Vec<(PathBuf, WavInfo)>,
    part: usize,
    file: Option<File>,
    /// Frames left in the current part.
    left: u64,
    buf: Vec<u8>,
}

impl TrackReader {
    fn open(parts: &[(PathBuf, WavInfo)]) -> TrackReader {
        TrackReader { parts: parts.to_vec(), part: 0, file: None, left: 0, buf: Vec::new() }
    }

    /// Position at `frame` from the start of the track.
    fn seek(&mut self, mut frame: u64) -> Result<(), String> {
        self.file = None;
        self.left = 0;
        for (k, (path, info)) in self.parts.iter().enumerate() {
            let n = info.frames();
            if frame < n || k + 1 == self.parts.len() {
                let frame = frame.min(n);
                let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                f.seek(SeekFrom::Start(info.data_offset + frame * info.block_align() as u64))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                self.file = Some(f);
                self.part = k;
                self.left = n - frame;
                return Ok(());
            }
            frame -= n;
        }
        Ok(())
    }

    /// Append up to `max` stereo frames to `out` (L R L R …); returns how
    /// many were read (fewer only at the end of the track).
    fn read(&mut self, max: usize, out: &mut Vec<f32>) -> Result<usize, String> {
        let mut done = 0;
        while done < max {
            if self.left == 0 {
                if self.part + 1 >= self.parts.len() || self.file.is_none() {
                    break;
                }
                let next = self.part + 1;
                let (path, info) = &self.parts[next];
                let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                f.seek(SeekFrom::Start(info.data_offset)).map_err(|e| format!("{}: {e}", path.display()))?;
                self.file = Some(f);
                self.part = next;
                self.left = info.frames();
                continue;
            }
            let info = &self.parts[self.part].1;
            let align = info.block_align();
            let n = (max - done).min(self.left as usize).min(CHUNK);
            self.buf.resize(n * align, 0);
            let f = self.file.as_mut().ok_or("file not open")?;
            let mut got = 0;
            while got < self.buf.len() {
                match f.read(&mut self.buf[got..]) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            let frames = got / align;
            let (kind, bs, stereo) = (info.kind, info.kind.bytes(), info.channels >= 2);
            for fr in 0..frames {
                let b = &self.buf[fr * align..];
                let l = kind.decode(&b[..bs]);
                let r = if stereo { kind.decode(&b[bs..2 * bs]) } else { l };
                out.push(l);
                out.push(r);
            }
            done += frames;
            if frames < n {
                // the file is shorter than its header said (still being written)
                self.left = 0;
                self.part = self.parts.len();
            } else {
                self.left -= frames as u64;
            }
        }
        Ok(done)
    }
}

// ===========================================================================
// Takes
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `mix`, `1-Mic`, `A1-Speakers`, or the file name for other files.
    pub label: String,
    pub parts: Vec<(PathBuf, WavInfo)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TakeInfo {
    /// Unique within the folder: the time stamp (with `-k`) of a
    /// BananaSplit recording, else the file name without `.wav`.
    pub key: String,
    /// `YYYY-MM-DD HH:MM:SS` of a BananaSplit recording.
    pub when: Option<String>,
    pub tracks: Vec<Track>,
    pub sample_rate: u32,
    pub kind: SampleKind,
    pub frames: u64,
    pub bytes: u64,
    /// Modification time (seconds since 1970) of the newest file.
    pub mtime: u64,
}

impl TakeInfo {
    pub fn secs(&self) -> f64 {
        if self.sample_rate == 0 { 0.0 } else { self.frames as f64 / self.sample_rate as f64 }
    }

    /// `mix`, `3 tracks`, `1-Mic`, or the file name.
    pub fn label(&self) -> String {
        match self.tracks.as_slice() {
            [t] => t.label.clone(),
            ts => format!("{} tracks", ts.len()),
        }
    }
}

/// `YYYY-MM-DD_HH-MM-SS` (+ `Z`) + optional `-k`, then `_`: the time stamp of
/// a BananaSplit recording (see `Recorder::start`). Returns (key, rest).
fn split_stamp(base: &str) -> Option<(&str, &str)> {
    let b = base.as_bytes();
    if b.len() < 21 {
        return None;
    }
    let digit = |i: usize| b[i].is_ascii_digit();
    let shape = (0..4).all(digit)
        && b[4] == b'-'
        && (5..7).all(digit)
        && b[7] == b'-'
        && (8..10).all(digit)
        && b[10] == b'_'
        && (11..13).all(digit)
        && b[13] == b'-'
        && (14..16).all(digit)
        && b[16] == b'-'
        && (17..19).all(digit);
    if !shape {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'Z') {
        i += 1;
    }
    if b.get(i) == Some(&b'-') {
        let j = i + 1;
        let mut k = j;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k == j {
            return None;
        }
        i = k;
    }
    if b.get(i) != Some(&b'_') || i + 1 >= b.len() {
        return None;
    }
    Some((&base[..i], &base[i + 1..]))
}

/// `name_part3` -> (`name`, 3); anything else is part 1.
fn split_part(stem: &str) -> (&str, u32) {
    if let Some(p) = stem.rfind("_part") {
        if let Ok(n) = stem[p + 5..].parse::<u32>() {
            if n >= 2 && !stem[p + 5..].starts_with('+') {
                return (&stem[..p], n);
            }
        }
    }
    (stem, 1)
}

/// Order of a track label: the mix, then strips 1-6, then buses, then others.
fn label_rank(label: &str) -> (usize, String) {
    if label == "mix" {
        return (0, String::new());
    }
    let id = label.split('-').next().unwrap_or("");
    match (0..NUM_SOURCES).position(|i| source_id(i) == id) {
        Some(i) => (1 + i, label.to_string()),
        None => (1 + NUM_SOURCES, label.to_string()),
    }
}

fn stamp_text(key: &str) -> Option<String> {
    let b = key.as_bytes();
    if b.len() < 19 {
        return None;
    }
    Some(format!("{} {}:{}:{}", &key[..10], &key[11..13], &key[14..16], &key[17..19]))
}

/// Header cache for [`scan`]: (path, size, mtime) -> header.
#[derive(Default)]
pub struct ScanCache {
    headers: HashMap<PathBuf, (u64, u64, Result<WavInfo, String>)>,
}

/// One take while scanning: its time, label -> parts (part number, file,
/// header), total bytes, newest modification time.
type Group = (Option<String>, HashMap<String, Vec<(u32, PathBuf, WavInfo)>>, u64, u64);

/// The takes in `folder`, oldest first (TAKE 1 is the first): other WAV
/// files by date, then BananaSplit's recordings by their time stamp. Files
/// whose header cannot be read are skipped. `cache` avoids re-reading
/// headers of files that did not change.
pub fn scan(folder: &Path, cache: &mut ScanCache) -> Vec<TakeInfo> {
    let Ok(dir) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut seen = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        // (bytes: the last four of a name in another script can start
        // inside a character, and slicing the text there panics)
        let b = name.as_bytes();
        if b.len() < 5 || !b[b.len() - 4..].eq_ignore_ascii_case(b".wav") || name.starts_with('.') {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        let size = meta.len();
        seen.push(path.clone());
        let fresh = cache.headers.get(&path).is_some_and(|(s, m, _)| *s == size && *m == mtime);
        if !fresh {
            cache.headers.insert(path.clone(), (size, mtime, read_wav_info(&path)));
        }
        let Some((_, _, Ok(info))) = cache.headers.get(&path) else { continue };
        let stem = &name[..name.len() - 4];
        let (base, part) = split_part(stem);
        let (key, label) = match split_stamp(base) {
            Some((k, l)) => (k.to_string(), l.to_string()),
            None => (base.to_string(), base.to_string()),
        };
        let when = split_stamp(base).and_then(|(k, _)| stamp_text(k));
        let g = groups.entry(key).or_insert_with(|| (when, HashMap::new(), 0, 0));
        g.1.entry(label).or_default().push((part, path, info.clone()));
        g.2 += size;
        g.3 = g.3.max(mtime);
    }
    cache.headers.retain(|p, _| seen.contains(p));
    let mut takes: Vec<TakeInfo> = groups
        .into_iter()
        .filter_map(|(key, (when, labels, bytes, mtime))| {
            let mut tracks: Vec<Track> = labels
                .into_iter()
                .map(|(label, mut parts)| {
                    parts.sort_by_key(|(n, _, _)| *n);
                    Track { label, parts: parts.into_iter().map(|(_, p, i)| (p, i)).collect() }
                })
                .collect();
            tracks.sort_by_key(|t| label_rank(&t.label));
            let first = tracks.first()?.parts.first()?.1.clone();
            let frames = tracks.iter().map(|t| t.parts.iter().map(|(_, i)| i.frames()).sum::<u64>()).max().unwrap_or(0);
            Some(TakeInfo { key, when, tracks, sample_rate: first.sample_rate, kind: first.kind, frames, bytes, mtime })
        })
        .collect();
    // A total order (the sort may panic on anything less): other files
    // first, by date; then recordings by their time stamp, which sorts as
    // text; the key breaks ties.
    takes.sort_by_cached_key(|t| match &t.when {
        Some(w) => (1, w.clone(), t.key.clone()),
        None => (0, format!("{:020}", t.mtime), t.key.clone()),
    });
    takes
}

/// The sum of a take's tracks, `buckets` peak values in percent of full
/// scale (0..=100; anything audible is at least 1). Short takes are read
/// completely; long ones are sampled (`4096` frames at the middle of every
/// bucket), which is plenty for a picture.
pub fn overview(take: &TakeInfo, buckets: usize) -> Result<Vec<u8>, String> {
    let buckets = buckets.clamp(1, 4096);
    let total = take.frames;
    let mut peaks = vec![0.0f32; buckets];
    if total == 0 {
        return Ok(vec![0; buckets]);
    }
    let mut readers: Vec<TrackReader> = take.tracks.iter().map(|t| TrackReader::open(&t.parts)).collect();
    let full = total.saturating_mul(take.tracks.len() as u64) <= 4_000_000;
    let mut buf = Vec::with_capacity(2 * CHUNK);
    let mut sum = vec![0.0f32; 2 * CHUNK];
    let bucket_of = |frame: u64| ((frame as u128 * buckets as u128 / total as u128) as usize).min(buckets - 1);
    let window = |b: usize| -> (u64, usize) {
        if full {
            (0, 0)
        } else {
            let mid = (b as u64 * total + total / 2) / buckets as u64;
            let len = 4096u64.min(total);
            (mid.saturating_sub(len / 2).min(total - len), len as usize)
        }
    };
    let ranges: Vec<(u64, usize)> = if full { vec![(0, total as usize)] } else { (0..buckets).map(window).collect() };
    for (start, len) in ranges {
        for r in &mut readers {
            r.seek(start)?;
        }
        let mut pos = 0usize;
        while pos < len {
            let want = (len - pos).min(CHUNK);
            sum[..2 * want].fill(0.0);
            let mut got_max = 0;
            for r in &mut readers {
                buf.clear();
                let got = r.read(want, &mut buf)?;
                got_max = got_max.max(got);
                for (s, v) in sum.iter_mut().zip(&buf) {
                    *s += v;
                }
            }
            if got_max == 0 {
                break;
            }
            for i in 0..got_max {
                let b = if full { bucket_of(start + (pos + i) as u64) } else { bucket_of(start + len as u64 / 2) };
                let v = sum[2 * i].abs().max(sum[2 * i + 1].abs());
                if v > peaks[b] {
                    peaks[b] = v;
                }
            }
            pos += got_max;
        }
    }
    Ok(peaks.iter().map(|&p| if p < 1e-4 { 0 } else { (p.min(1.0) * 100.0).round().max(1.0) as u8 }).collect())
}

// ===========================================================================
// Player thread
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

impl PlayState {
    pub fn as_str(self) -> &'static str {
        match self {
            PlayState::Stopped => "stopped",
            PlayState::Playing => "playing",
            PlayState::Paused => "paused",
        }
    }
}

/// What the UIs show.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayStatus {
    pub state: PlayState,
    /// Key of the loaded take.
    pub take: Option<String>,
    pub pos: f64,
    pub dur: f64,
    pub underruns: u32,
    pub error: Option<String>,
}

enum Cmd {
    Load(Box<TakeInfo>, f64, bool),
    Pause,
    Resume,
    Stop,
    Seek(f64),
    Exit,
}

struct Shared {
    status: PlayStatus,
    /// Output frame at which the ring's flush happened (position base).
    base: u64,
}

/// Player control. Thread-safe; used by the GUI thread.
pub struct Player {
    tap: Arc<PlayTap>,
    rate: u32,
    shared: Arc<Mutex<Shared>>,
    tx: Mutex<Option<mpsc::Sender<Cmd>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Player {
    /// `rate`: the mixer's sample rate.
    pub fn new(tap: Arc<PlayTap>, rate: u32) -> Player {
        let shared = Arc::new(Mutex::new(Shared {
            status: PlayStatus { state: PlayState::Stopped, take: None, pos: 0.0, dur: 0.0, underruns: 0, error: None },
            base: 0,
        }));
        let (tx, rx) = mpsc::channel();
        let (t2, s2) = (tap.clone(), shared.clone());
        let thread =
            std::thread::Builder::new().name("bs-player".into()).spawn(move || player_thread(&t2, rate, &s2, &rx)).ok();
        Player { tap, rate: rate.max(1), shared, tx: Mutex::new(thread.as_ref().map(|_| tx)), thread: Mutex::new(thread) }
    }

    pub fn tap(&self) -> &Arc<PlayTap> {
        &self.tap
    }

    /// The mixer's sample rate: the engine's once it runs, else the one
    /// given to [`Player::new`].
    pub fn rate(&self) -> u32 {
        match self.tap.rate.load(Relaxed) {
            0 => self.rate,
            r => r,
        }
    }

    fn send(&self, c: Cmd) -> Result<(), String> {
        lock(&self.tx).as_ref().ok_or("the player is not running")?.send(c).map_err(|_| "the player has stopped".into())
    }

    /// Load `take` and play it from `from` seconds (or only load it, stopped
    /// there: `resume` then plays it).
    pub fn play(&self, take: &TakeInfo, from: f64, start: bool) -> Result<(), String> {
        let (fs, rate) = (take.sample_rate, self.rate());
        if fs as f64 > rate as f64 * MAX_DOWN {
            return Err(format!("{} Hz is more than four times the mixer's {rate} Hz", fs));
        }
        // Below that, a header that is wrong (or a file that is not audio)
        // would make every block of the file millions of frames long.
        if fs < MIN_RATE {
            return Err(format!("{fs} Hz is below {MIN_RATE} Hz, the lowest sample rate the player plays"));
        }
        if take.tracks.iter().flat_map(|t| &t.parts).any(|(_, i)| i.sample_rate != fs) {
            return Err("the tracks of this take have different sample rates".into());
        }
        self.send(Cmd::Load(Box::new(take.clone()), from.max(0.0), start))
    }

    pub fn pause(&self) -> Result<(), String> {
        self.send(Cmd::Pause)
    }

    /// Play the loaded take from where it is (paused or stopped).
    pub fn resume(&self) -> Result<(), String> {
        self.send(Cmd::Resume)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.send(Cmd::Stop)
    }

    pub fn seek(&self, secs: f64) -> Result<(), String> {
        self.send(Cmd::Seek(secs.max(0.0)))
    }

    /// Buses playback goes to (bit b = bus b).
    pub fn set_routes(&self, mask: u8) {
        self.tap.routes.store(mask & ((1 << NUM_BUSES) - 1) as u8, Relaxed);
    }

    pub fn routes(&self) -> u8 {
        self.tap.routes.load(Relaxed)
    }

    pub fn status(&self) -> PlayStatus {
        let g = lock(&self.shared);
        let mut s = g.status.clone();
        if s.take.is_some() {
            // Until the engine has emptied the ring after a seek or stop,
            // `played` still counts the old position.
            let flushing = self.tap.flush_req.load(Acquire) != self.tap.flush_ack.load(Acquire);
            let played = if flushing { 0 } else { self.tap.played.load(Relaxed) };
            s.pos = ((g.base + played) as f64 / self.rate() as f64).min(s.dur);
        }
        s.underruns = self.tap.underruns();
        s
    }

    pub fn shutdown(&self) {
        if let Some(tx) = lock(&self.tx).take() {
            let _ = tx.send(Cmd::Exit);
        }
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The take being fed into the ring.
struct Feed {
    readers: Vec<TrackReader>,
    resampler: Option<Resampler>,
    step: f64,
    tracks_buf: Vec<f32>,
    sum: Vec<f32>,
    out: Vec<f32>,
    /// Output samples waiting for room in the ring.
    pending: Vec<f32>,
    done: bool,
}

impl Feed {
    fn new(take: &TakeInfo, rate: u32) -> Feed {
        let resampler = (take.sample_rate != rate).then(|| Resampler::new(take.sample_rate, rate));
        let step = resampler.as_ref().map_or(1.0, Resampler::nominal_step);
        Feed {
            readers: take.tracks.iter().map(|t| TrackReader::open(&t.parts)).collect(),
            resampler,
            step,
            tracks_buf: Vec::with_capacity(2 * CHUNK),
            sum: Vec::with_capacity(2 * CHUNK),
            out: Vec::with_capacity(4 * CHUNK),
            pending: Vec::new(),
            done: false,
        }
    }

    /// Position every track at input frame `frame`.
    fn seek(&mut self, frame: u64) -> Result<(), String> {
        for r in &mut self.readers {
            r.seek(frame)?;
        }
        if let Some(rs) = &mut self.resampler {
            rs.reset();
        }
        self.pending.clear();
        self.done = false;
        Ok(())
    }

    /// Move as much as fits into the ring; returns false once the take has
    /// been completely written.
    fn pump(&mut self, ring: &Ring) -> Result<bool, String> {
        loop {
            if !self.pending.is_empty() {
                let n = self.pending.len().min(ring.free()) & !1;
                for (i, &v) in self.pending[..n].iter().enumerate() {
                    ring.write(i, v);
                }
                ring.publish(n);
                self.pending.drain(..n);
                if !self.pending.is_empty() {
                    return Ok(true);
                }
            }
            if self.done {
                return Ok(false);
            }
            if ring.free() < 2 * CHUNK {
                return Ok(true);
            }
            self.sum.clear();
            self.sum.resize(2 * CHUNK, 0.0);
            let mut got = 0;
            for r in &mut self.readers {
                self.tracks_buf.clear();
                let n = r.read(CHUNK, &mut self.tracks_buf)?;
                got = got.max(n);
                for (s, v) in self.sum.iter_mut().zip(&self.tracks_buf) {
                    *s += v;
                }
            }
            self.sum.truncate(2 * got);
            let end = got < CHUNK;
            match &mut self.resampler {
                None => self.pending.extend_from_slice(&self.sum),
                Some(rs) => {
                    self.out.clear();
                    rs.process(&self.sum, self.step, &mut self.out);
                    if end {
                        // push the kernel's tail out with silence
                        let tail = vec![0.0f32; 2 * (rs.delay().ceil() as usize + 2)];
                        rs.process(&tail, self.step, &mut self.out);
                    }
                    self.pending.extend_from_slice(&self.out);
                }
            }
            if end {
                self.done = true;
            }
        }
    }
}

/// Seconds -> frames at the mixer's rate.
fn secs_to_out(s: f64, rate: u32) -> u64 {
    (s * rate as f64).round().max(0.0) as u64
}

fn player_thread(tap: &PlayTap, rate: u32, shared: &Mutex<Shared>, rx: &mpsc::Receiver<Cmd>) {
    let mut rate = rate.max(1);
    let mut feed: Option<Feed> = None;
    let mut take: Option<TakeInfo> = None;
    // Flush in progress: wait for the engine before writing to the ring.
    let mut flush_wait: Option<(u32, Instant)> = None;
    let mut eof_sent = false;
    let set = |f: &dyn Fn(&mut Shared)| f(&mut lock(shared));

    // Empty the ring through the engine; positions restart at `base`.
    let flush = |base: u64, flush_wait: &mut Option<(u32, Instant)>| {
        tap.run.store(false, Release);
        tap.eof.store(false, Release);
        let req = tap.flush_req.load(Relaxed).wrapping_add(1);
        tap.flush_req.store(req, Release);
        *flush_wait = Some((req, Instant::now()));
        lock(shared).base = base;
    };

    loop {
        // The audio server changed its sample rate: convert the take to the
        // new rate from where it is, so it keeps playing at its own speed.
        let r = tap.rate.load(Relaxed);
        if r != 0 && r != rate {
            let played = if flush_wait.is_some() { 0 } else { tap.played.load(Relaxed) };
            let pos = (lock(shared).base + played) as f64 / rate as f64;
            rate = r;
            if let Some(t) = &take {
                let pos = pos.min(t.secs());
                let mut f = Feed::new(t, rate);
                let res = f.seek((pos * t.sample_rate as f64) as u64);
                flush(secs_to_out(pos, rate), &mut flush_wait);
                eof_sent = false;
                match res {
                    Ok(()) => feed = Some(f),
                    Err(e) => {
                        feed = None;
                        take = None;
                        set(&|s| {
                            s.status.state = PlayState::Stopped;
                            s.status.take = None;
                            s.status.error = Some(e.clone());
                        });
                    }
                }
            }
        }

        let idle = feed.is_none() || flush_wait.is_some();
        let cmd = match rx.recv_timeout(Duration::from_millis(if idle { 20 } else { 5 })) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Cmd::Exit),
        };
        match cmd {
            Some(Cmd::Exit) => {
                tap.run.store(false, Release);
                return;
            }
            Some(Cmd::Load(t, from, start)) => {
                let from = from.min(t.secs());
                let mut f = Feed::new(&t, rate);
                let res = f.seek((from * t.sample_rate as f64) as u64);
                flush(secs_to_out(from, rate), &mut flush_wait);
                eof_sent = false;
                set(&|s| {
                    s.status.take = Some(t.key.clone());
                    s.status.dur = t.secs();
                    s.status.pos = from;
                    s.status.error = None;
                    s.status.state = if start { PlayState::Playing } else { PlayState::Stopped };
                });
                match res {
                    Ok(()) => {
                        feed = Some(f);
                        take = Some(*t);
                    }
                    Err(e) => {
                        feed = None;
                        take = None;
                        set(&|s| {
                            s.status.state = PlayState::Stopped;
                            s.status.take = None;
                            s.status.error = Some(e.clone());
                        });
                    }
                }
            }
            Some(Cmd::Pause) => {
                tap.run.store(false, Release);
                set(&|s| {
                    if s.status.state == PlayState::Playing {
                        s.status.state = PlayState::Paused;
                    }
                });
            }
            Some(Cmd::Resume) => {
                let st = lock(shared).status.state;
                if st != PlayState::Playing && feed.is_some() {
                    lock(shared).status.state = PlayState::Playing;
                    if flush_wait.is_none() {
                        tap.run.store(true, Release);
                    }
                }
            }
            Some(Cmd::Stop) => {
                if let (Some(f), Some(t)) = (&mut feed, &take) {
                    let _ = f.seek(0);
                    flush(0, &mut flush_wait);
                    eof_sent = false;
                    let dur = t.secs();
                    set(&|s| {
                        s.status.state = PlayState::Stopped;
                        s.status.pos = 0.0;
                        s.status.dur = dur;
                    });
                }
            }
            Some(Cmd::Seek(secs)) => {
                if let (Some(f), Some(t)) = (&mut feed, &take) {
                    let secs = secs.min(t.secs());
                    let res = f.seek((secs * t.sample_rate as f64) as u64);
                    flush(secs_to_out(secs, rate), &mut flush_wait);
                    eof_sent = false;
                    if let Err(e) = res {
                        set(&|s| s.status.error = Some(e.clone()));
                    }
                }
            }
            None => {}
        }

        // The engine acknowledged the flush: the ring is ours again.
        if let Some((req, at)) = flush_wait {
            if tap.flush_ack.load(Acquire) == req {
                flush_wait = None;
                if lock(shared).status.state == PlayState::Playing {
                    tap.run.store(true, Release);
                }
            } else if at.elapsed() > Duration::from_secs(2) {
                // The engine is not running cycles; keep waiting, quietly.
                flush_wait = Some((req, Instant::now()));
            }
            continue;
        }

        let Some(f) = &mut feed else { continue };
        let state = lock(shared).status.state;
        if state == PlayState::Stopped {
            continue;
        }
        match f.pump(&tap.ring) {
            Ok(true) => {}
            Ok(false) => {
                if !eof_sent {
                    tap.eof.store(true, Release);
                    eof_sent = true;
                }
                // Played to the end: back to the start, stopped.
                if tap.ring.available() == 0 && !tap.playing.load(Relaxed) && state == PlayState::Playing {
                    let _ = f.seek(0);
                    flush(0, &mut flush_wait);
                    eof_sent = false;
                    set(&|s| {
                        s.status.state = PlayState::Stopped;
                        s.status.pos = 0.0;
                    });
                }
            }
            Err(e) => {
                tap.run.store(false, Release);
                set(&|s| {
                    s.status.state = PlayState::Stopped;
                    s.status.error = Some(e.clone());
                });
                feed = None;
                take = None;
                flush(0, &mut flush_wait);
                set(&|s| s.status.take = None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::{Format, wav_header};
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bs-player-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A WAV file with `frames` stereo frames from `f(frame) -> (l, r)`.
    fn write_wav(path: &Path, format: Format, rate: u32, frames: usize, f: impl Fn(usize) -> (f32, f32)) {
        let bps = match format {
            Format::Wav16 => 2,
            Format::Wav24 => 3,
            Format::Wav32f => 4,
        };
        let mut data = Vec::new();
        for i in 0..frames {
            let (l, r) = f(i);
            for x in [l, r] {
                match format {
                    Format::Wav16 => data.extend_from_slice(&((x * 32767.0).round() as i16).to_le_bytes()),
                    Format::Wav24 => data.extend_from_slice(&((x * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
                    Format::Wav32f => data.extend_from_slice(&x.to_le_bytes()),
                }
            }
        }
        assert_eq!(data.len(), frames * 2 * bps);
        let mut file = File::create(path).unwrap();
        file.write_all(&wav_header(format, rate, data.len() as u32)).unwrap();
        file.write_all(&data).unwrap();
    }

    /// A hand-made WAV: `fmt` body + data.
    fn raw_wav(path: &Path, fmt: &[u8], data: &[u8], extra_before_data: &[u8]) {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        v.extend_from_slice(fmt);
        v.extend_from_slice(extra_before_data);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        std::fs::write(path, v).unwrap();
    }

    fn fmt(tag: u16, ch: u16, rate: u32, bits: u16) -> Vec<u8> {
        let align = ch * bits / 8;
        let mut v = Vec::new();
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&ch.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * align as u32).to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v
    }

    fn read_all(info_path: &Path) -> Vec<f32> {
        let info = read_wav_info(info_path).unwrap();
        let mut r = TrackReader::open(&[(info_path.to_path_buf(), info)]);
        r.seek(0).unwrap();
        let mut out = Vec::new();
        while r.read(1000, &mut out).unwrap() > 0 {}
        out
    }

    #[test]
    fn reads_our_own_formats_exactly() {
        let d = tmpdir("own");
        for (fmtk, tol) in [(Format::Wav16, 1.0 / 32767.0), (Format::Wav24, 1.0 / 8_388_607.0), (Format::Wav32f, 0.0)] {
            let p = d.join(format!("{}.wav", fmtk.as_str()));
            write_wav(&p, fmtk, 44_100, 1000, |i| ((i as f32 / 1000.0) - 0.5, -0.25));
            let info = read_wav_info(&p).unwrap();
            assert_eq!((info.channels, info.sample_rate, info.frames()), (2, 44_100, 1000));
            let v = read_all(&p);
            assert_eq!(v.len(), 2000);
            for i in 0..1000 {
                assert!((v[2 * i] - ((i as f32 / 1000.0) - 0.5)).abs() <= tol * 1.01, "{fmtk:?} {i}");
                assert!((v[2 * i + 1] + 0.25).abs() <= tol * 1.01);
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn reads_other_wav_flavours() {
        let d = tmpdir("flavours");
        // mono 8-bit, an odd-sized LIST chunk before the data
        let p = d.join("u8.wav");
        raw_wav(&p, &fmt(1, 1, 8000, 8), &[0, 128, 255], b"LIST\x03\x00\x00\x00abc\x00");
        let v = read_all(&p);
        assert_eq!(v, vec![-1.0, -1.0, 0.0, 0.0, 127.0 / 128.0, 127.0 / 128.0]);
        // 4-channel 32-bit int: first two used
        let p = d.join("i32.wav");
        let mut data = Vec::new();
        for x in [i32::MIN, i32::MAX, 0, 5] {
            data.extend_from_slice(&x.to_le_bytes());
        }
        raw_wav(&p, &fmt(1, 4, 48000, 32), &data, b"");
        assert_eq!(read_all(&p), vec![-1.0, 1.0]);
        // float64, extensible
        let p = d.join("f64.wav");
        let mut f = fmt(0xFFFE, 2, 96000, 64);
        f.extend_from_slice(&22u16.to_le_bytes());
        f.extend_from_slice(&64u16.to_le_bytes());
        f.extend_from_slice(&3u32.to_le_bytes());
        f.extend_from_slice(&3u16.to_le_bytes()); // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        f.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        let mut data = Vec::new();
        for x in [0.5f64, -0.125, f64::NAN, 1.0] {
            data.extend_from_slice(&x.to_le_bytes());
        }
        raw_wav(&p, &f, &data, b"");
        assert_eq!(read_wav_info(&p).unwrap().kind, SampleKind::F64);
        assert_eq!(read_all(&p), vec![0.5, -0.125, 0.0, 1.0]);
        // a data size the writer never filled in (0): the rest of the file
        let p = d.join("streamed.wav");
        let mut v = Vec::new();
        for x in [1000i16, -1000, 2000, -2000] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        raw_wav(&p, &fmt(1, 2, 48000, 16), &v, b"");
        let mut bytes = std::fs::read(&p).unwrap();
        let n = bytes.len();
        bytes[n - 12..n - 8].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        assert_eq!(read_wav_info(&p).unwrap().frames(), 2);
        // rejections
        let p = d.join("bad.wav");
        raw_wav(&p, &fmt(85, 2, 48000, 16), &[0; 8], b"");
        assert!(read_wav_info(&p).unwrap_err().contains("compressed"));
        raw_wav(&p, &fmt(1, 2, 48000, 12), &[0; 8], b"");
        assert!(read_wav_info(&p).is_err());
        std::fs::write(&p, b"RIFF\0\0\0\0AVI LIST").unwrap();
        assert!(read_wav_info(&p).unwrap_err().contains("not a WAV"));
        std::fs::write(&p, b"RF64\0\0\0\0WAVE").unwrap();
        assert!(read_wav_info(&p).unwrap_err().contains("RF64"));
        std::fs::write(&p, b"RIFF").unwrap();
        assert!(read_wav_info(&p).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn names_split_into_takes_tracks_and_parts() {
        assert_eq!(split_stamp("2026-10-21_14-03-05_mix"), Some(("2026-10-21_14-03-05", "mix")));
        assert_eq!(split_stamp("2026-10-21_14-03-05-2_1-Mic"), Some(("2026-10-21_14-03-05-2", "1-Mic")));
        assert_eq!(split_stamp("2026-10-21_14-03-05Z_A1-Speakers"), Some(("2026-10-21_14-03-05Z", "A1-Speakers")));
        assert_eq!(split_stamp("2026-10-21_14-03-05"), None);
        assert_eq!(split_stamp("2026-10-21_14-03-05_"), None);
        assert_eq!(split_stamp("2026-10-21_14-03-05-_x"), None);
        assert_eq!(split_stamp("my song"), None);
        assert_eq!(split_part("a_mix_part3"), ("a_mix", 3));
        assert_eq!(split_part("a_mix_part1"), ("a_mix_part1", 1));
        assert_eq!(split_part("a_mix_partx"), ("a_mix_partx", 1));
        assert_eq!(split_part("a_mix"), ("a_mix", 1));
        assert_eq!(stamp_text("2026-10-21_14-03-05-2").unwrap(), "2026-10-21 14:03:05");
    }

    #[test]
    fn scan_groups_files() {
        let d = tmpdir("scan");
        let w = |name: &str, frames: usize| write_wav(&d.join(name), Format::Wav16, 48000, frames, |_| (0.1, 0.1));
        w("2026-10-21_14-03-05_mix.wav", 100);
        w("2026-10-21_14-03-05_mix_part2.wav", 50);
        w("2026-10-21_09-00-00_A1-Speakers.wav", 10);
        w("2026-10-21_09-00-00_1-Mic.wav", 10);
        w("2026-10-21_09-00-00_mix.wav", 10); // not ours, but fine
        w("Song.WAV", 7);
        std::fs::write(d.join("broken.wav"), b"nope").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        let mut cache = ScanCache::default();
        let t = scan(&d, &mut cache);
        let keys: Vec<&str> = t.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["Song", "2026-10-21_09-00-00", "2026-10-21_14-03-05"], "{t:?}");
        assert_eq!(t[1].tracks.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), ["mix", "1-Mic", "A1-Speakers"]);
        assert_eq!(t[1].label(), "3 tracks");
        assert_eq!(t[2].label(), "mix");
        assert_eq!(t[2].tracks[0].parts.len(), 2);
        assert_eq!(t[2].frames, 150);
        assert_eq!(t[2].when.as_deref(), Some("2026-10-21 14:03:05"));
        assert_eq!(t[0].when, None);
        assert_eq!(t[0].label(), "Song");
        // cached: same result, and a removed file disappears
        assert_eq!(scan(&d, &mut cache), t);
        std::fs::remove_file(d.join("Song.WAV")).unwrap();
        assert_eq!(scan(&d, &mut cache).len(), 2);
        assert!(scan(&d.join("missing"), &mut cache).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn scan_takes_names_in_any_script() {
        // A name whose last four bytes start inside a character (a folder
        // `録音`, a file `Größe`) used to panic the scan, and with it the
        // thread that keeps the take list up to date, for good.
        let d = tmpdir("scan-utf8");
        std::fs::create_dir_all(d.join("録音")).unwrap();
        std::fs::write(d.join("Größe"), b"x").unwrap();
        std::fs::write(d.join("新建文件夹.txt"), b"x").unwrap();
        write_wav(&d.join("Bässe.wav"), Format::Wav16, 48000, 10, |_| (0.1, 0.1));
        write_wav(&d.join("2026-10-21_14-03-05_1-Gitarre ä.wav"), Format::Wav16, 48000, 10, |_| (0.1, 0.1));
        let t = scan(&d, &mut ScanCache::default());
        let keys: Vec<&str> = t.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["Bässe", "2026-10-21_14-03-05"], "{t:?}");
        assert_eq!(t[1].tracks[0].label, "1-Gitarre ä");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn overview_shows_where_it_is_loud() {
        let d = tmpdir("overview");
        // first half silent, second half at -6 dBFS
        write_wav(&d.join("x.wav"), Format::Wav32f, 48000, 48000, |i| if i < 24000 { (0.0, 0.0) } else { (0.5, -0.5) });
        let t = scan(&d, &mut ScanCache::default());
        let o = overview(&t[0], 10).unwrap();
        assert_eq!(o, [0, 0, 0, 0, 0, 50, 50, 50, 50, 50]);
        let _ = std::fs::remove_dir_all(d);
    }

    // ---- the real-time side ----

    struct Bus {
        out: Vec<Vec<f32>>,
    }

    impl Bus {
        fn new() -> Bus {
            Bus { out: vec![vec![0.0; 256]; NUM_BUSES * 2] }
        }
        fn run(&mut self, rt: &mut PlayRt, tap: &PlayTap, n: usize) {
            for o in &mut self.out {
                o.clear();
                o.resize(n, 0.0);
            }
            let mut it = self.out.iter_mut();
            let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
                let l = it.next().unwrap().as_mut_slice();
                let r = it.next().unwrap().as_mut_slice();
                [l, r]
            });
            rt.mix(tap, n, &mut outs);
        }
    }

    #[test]
    fn rt_waits_fades_plays_routes_and_stops() {
        let tap = PlayTap::new();
        let mut rt = PlayRt::default();
        let mut b = Bus::new();
        let frames: Vec<f32> = (0..2 * 20_000).map(|i| if i % 2 == 0 { 0.5 } else { -0.25 }).collect();
        // nothing until running and START_FRAMES are buffered
        tap.test_push(&frames[..2 * 1000]);
        tap.test_run(true, false);
        b.run(&mut rt, &tap, 128);
        assert!(b.out.iter().all(|o| o.iter().all(|&x| x == 0.0)));
        assert_eq!(tap.test_played(), 0);
        tap.test_push(&frames[2 * 1000..2 * 8000]);
        // first cycle fades in on A1 (the default route)
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][0] > 0.0 && b.out[0][0] < 0.01);
        assert_eq!(b.out[0][127], 0.5);
        assert_eq!(b.out[1][127], -0.25);
        assert!(b.out[2..].iter().all(|o| o.iter().all(|&x| x == 0.0)));
        // steady
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.5));
        // route to B1 as well: ramps in over the cycle
        tap.routes.store(0b01001, Relaxed);
        b.run(&mut rt, &tap, 128);
        assert!(b.out[6][0] > 0.0 && b.out[6][0] < 0.01 && b.out[6][127] == 0.5);
        assert_eq!(tap.test_played(), 3 * 128);
        // pause: one faded cycle, then silence and no consumption
        tap.test_run(false, false);
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][0] > 0.49 && b.out[0][127] == 0.0);
        let played = tap.test_played();
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.0));
        assert_eq!(tap.test_played(), played);
        // resume
        tap.test_run(true, false);
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][0] < 0.01 && b.out[0][127] == 0.5);
        // flush while playing (stop, seek, another take): one faded cycle,
        // then the ring is empty and the count zero
        tap.test_flush();
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][0] > 0.49 && b.out[0][126] > 0.0 && b.out[0][127] == 0.0);
        assert!((1..128).all(|i| b.out[0][i] <= b.out[0][i - 1]), "a monotonic fade");
        assert_eq!(tap.test_played(), 0);
        assert_eq!(tap.ring.available(), 0);
        assert_eq!(tap.flush_ack.load(Relaxed), tap.flush_req.load(Relaxed));
        assert!(!tap.playing.load(Relaxed));
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.0));
        // flush while not playing: nothing to fade, acknowledged at once
        tap.test_push(&frames[..2 * 500]);
        tap.test_run(false, false);
        tap.test_flush();
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.0));
        assert_eq!(tap.ring.available(), 0);
        assert_eq!(tap.flush_ack.load(Relaxed), tap.flush_req.load(Relaxed));
    }

    #[test]
    fn rt_underrun_and_end() {
        let tap = PlayTap::new();
        let mut rt = PlayRt::default();
        let mut b = Bus::new();
        // START_FRAMES + 100 frames, more promised
        let frames = vec![0.5f32; 2 * (START_FRAMES + 100)];
        tap.test_push(&frames);
        tap.test_run(true, false);
        for _ in 0..START_FRAMES / 128 - 1 {
            b.run(&mut rt, &tap, 128);
            assert!(b.out[0][127] == 0.5);
        }
        assert_eq!(tap.underruns(), 0);
        // 228 frames left, less than two cycles: this cycle fades out (an
        // underrun), so the next one does not stop dead
        b.run(&mut rt, &tap, 128);
        assert_eq!(tap.underruns(), 1);
        assert!(b.out[0][0] > 0.49 && b.out[0][127] == 0.0);
        assert!(!tap.playing.load(Relaxed));
        // 100 frames left: not enough to start again
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.0));
        assert_eq!(tap.ring.available(), 2 * 100);
        // the end of the take: plays the rest at full level after the
        // fade-in, then stops; no underrun
        tap.test_run(true, true);
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][0] > 0.0 && b.out[0][0] < 0.01);
        assert_eq!(b.out[0][99], 0.5);
        assert_eq!(b.out[0][100], 0.0);
        assert_eq!(tap.underruns(), 1);
        assert!(!tap.playing.load(Relaxed));
        assert_eq!(tap.ring.available(), 0);
        // playing on into the end of a take: the last samples are not faded
        let mut rt = PlayRt::default();
        tap.test_push(&frames[..2 * (START_FRAMES + 50)]);
        tap.test_run(true, false);
        for _ in 0..START_FRAMES / 128 - 1 {
            b.run(&mut rt, &tap, 128);
        }
        tap.test_run(true, true);
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0].iter().all(|&x| x == 0.5));
        b.run(&mut rt, &tap, 128);
        assert!(b.out[0][..50].iter().all(|&x| x == 0.5) && b.out[0][50..].iter().all(|&x| x == 0.0));
        assert_eq!(tap.underruns(), 1);
    }

    #[test]
    fn refuses_sample_rates_it_cannot_play() {
        let d = tmpdir("rates");
        write_wav(&d.join("low.wav"), Format::Wav16, 1, 100, |_| (0.1, 0.1));
        write_wav(&d.join("high.wav"), Format::Wav16, 384_000, 100, |_| (0.1, 0.1));
        write_wav(&d.join("ok.wav"), Format::Wav16, 8000, 100, |_| (0.1, 0.1));
        let takes = scan(&d, &mut ScanCache::default());
        let p = Player::new(PlayTap::new(), 48000);
        let find = |k: &str| takes.iter().find(|t| t.key.contains(k)).unwrap();
        assert!(p.play(find("low"), 0.0, true).unwrap_err().contains("below 8000 Hz"));
        assert!(p.play(find("high"), 0.0, true).unwrap_err().contains("more than four times"));
        assert!(p.play(find("ok"), 0.0, true).is_ok());
        p.shutdown();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn plays_a_take_from_disk_bit_exact_and_resampled() {
        let d = tmpdir("play");
        // 32-bit float at the mixer's rate: what comes out is what is in the file
        write_wav(&d.join("2026-01-01_00-00-00_mix.wav"), Format::Wav32f, 48000, 30_000, |i| {
            (((i % 100) as f32 - 50.0) / 100.0, 0.25)
        });
        let takes = scan(&d, &mut ScanCache::default());
        let tap = PlayTap::new();
        let p = Player::new(tap.clone(), 48000);
        p.play(&takes[0], 0.0, true).unwrap();
        let mut rt = PlayRt::default();
        let mut b = Bus::new();
        let file = |i: usize| ((i % 100) as f32 - 50.0) / 100.0;
        let mut got: Vec<f32> = Vec::new();
        let t0 = Instant::now();
        loop {
            b.run(&mut rt, &tap, 256);
            got.extend_from_slice(&b.out[0]);
            let first = got.iter().position(|&x| x != 0.0);
            if first.is_some_and(|f| got.len() >= f + 30_000 + 512) || t0.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_micros(300));
        }
        // Playback starts on a cycle boundary with file frame 0 (-0.5, faded
        // in); after that first cycle every sample is exactly the file's.
        let first = got.iter().position(|&x| x != 0.0).expect("nothing played");
        assert_eq!(first % 256, 0);
        assert_eq!(got[first], file(0) / 256.0);
        for (k, &x) in got[first + 256..first + 30_000].iter().enumerate() {
            assert_eq!(x, file(k + 256), "file frame {}", k + 256);
        }
        assert!(got[first + 30_000..].iter().all(|&x| x == 0.0), "played past the end");
        // played to the end -> stopped, back at 0
        let t0 = Instant::now();
        while p.status().state != PlayState::Stopped && t0.elapsed() < Duration::from_secs(5) {
            b.run(&mut rt, &tap, 256);
            std::thread::sleep(Duration::from_millis(2));
        }
        let st = p.status();
        assert_eq!((st.state, st.pos, st.underruns), (PlayState::Stopped, 0.0, 0), "{st:?}");
        assert!((st.dur - 30_000.0 / 48_000.0).abs() < 1e-9);

        // 44.1 kHz file at 48 kHz: a 1 kHz sine stays a 1 kHz sine of the same level
        write_wav(&d.join("2026-01-01_00-00-01_mix.wav"), Format::Wav24, 44100, 44100, |i| {
            let v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44100.0).sin();
            (v, v)
        });
        let takes = scan(&d, &mut ScanCache::default());
        p.play(&takes[1], 0.25, true).unwrap();
        let mut got = Vec::new();
        let t0 = Instant::now();
        while got.len() < 24_000 && t0.elapsed() < Duration::from_secs(10) {
            b.run(&mut rt, &tap, 256);
            if b.out[0].iter().any(|&x| x != 0.0) || !got.is_empty() {
                got.extend_from_slice(&b.out[0]);
            }
            std::thread::sleep(Duration::from_micros(300));
        }
        let mid = &got[4800..4800 + 4800];
        let peak = mid.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((99..=101).contains(&crossings), "{crossings} cycles in 0.1 s");
        // position counts from the seek point
        let pos = p.status().pos;
        assert!(pos > 0.25 && pos < 1.0, "{pos}");
        // seek, pause, stop
        p.seek(0.9).unwrap();
        let t0 = Instant::now();
        while p.status().pos < 0.9 && t0.elapsed() < Duration::from_secs(5) {
            b.run(&mut rt, &tap, 256);
            std::thread::sleep(Duration::from_micros(300));
        }
        assert!(p.status().pos >= 0.9);
        p.pause().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        b.run(&mut rt, &tap, 256);
        b.run(&mut rt, &tap, 256);
        assert!(b.out[0].iter().all(|&x| x == 0.0));
        assert_eq!(p.status().state, PlayState::Paused);
        p.stop().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        b.run(&mut rt, &tap, 256);
        let st = p.status();
        assert_eq!((st.state, st.pos), (PlayState::Stopped, 0.0));
        // a file that disappeared is an error, not a hang
        p.play(&takes[0], 0.0, true).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
        // (the reader already has the file open; the take still plays)
        let too_fast = TakeInfo { sample_rate: 400_000, ..takes[0].clone() };
        assert!(p.play(&too_fast, 0.0, true).unwrap_err().contains("four times"));
        p.shutdown();
    }
}
