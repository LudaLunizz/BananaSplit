//! Recorder: capture any combination of the 11 mixer channels to MP3 or WAV.
//!
//! Every channel can be *linked* to the recorder, using the same numbering as
//! the rest of BananaSplit: strips `1`..`6` (tapped post-fader: after gain, mute,
//! solo and mono, i.e. exactly what the strip sends to the buses) and buses
//! `A1`..`B2` (tapped post-master: exactly what leaves the bus).
//!
//! Two modes:
//! * **mix**: all linked channels are summed (unity gain) into one stereo
//!   file. Links can be changed while recording; each change is ramped over
//!   one period, like a routing change, so it does not click.
//! * **multi**: one stereo file per linked channel, all started on the same
//!   sample and always the same length, so they line up in any DAW. The link
//!   set is fixed when the take starts.
//!
//! ```text
//!  audio thread (Engine::process)          writer thread           track threads (1 per file)
//!  RecRt::push ──► lock-free SPSC ring ──► Take::write_block ──► encode + write ──► 1..11 files
//!  (no alloc, no locks, no syscalls)       (split into tracks)     (MP3 or WAV)
//! ```
//!
//! The audio thread writes one block per cycle: a 5-word header (session,
//! frames, gap, channels, sample rate) followed by interleaved samples. If
//! the ring is ever full (disk stalled for seconds) the block is dropped and
//! its length is carried in the next header's `gap`; the writer fills the
//! gap with silence, so a take never loses sync with the timeline or between
//! tracks. Long takes continue in `_part2`, `_part3`, ... files at the same
//! frame for every track: every hour by default (`split`, in minutes; 0 =
//! never), and always before 4 GiB (larger files are not valid WAV, nor
//! countable in an MP3's Info frame). Each track has a thread of its own
//! that encodes and writes it, and finishes a part (its header, the disk
//! sync) while the next part is already being written, so neither a slow
//! disk at a split nor 11 tracks of MP3 encoding hold up the ring.
//!
//! MP3 files (the default, 320 kbit/s) start with an Info frame whose LAME
//! tag gives their exact length, so the parts of a take play back to back
//! without a gap in any player that reads it. MP3 stores 32, 44.1 and 48
//! kHz; a take at another rate is converted (96 kHz to 48 kHz, ...) in the
//! track threads, and its parts still meet at the same sample.
//!
//! Every file's header carries the sample rate the audio server really ran
//! at while it was recorded (the rate in each block, which the engine takes
//! from the server, or the MP3 rate it was converted to), so a take always
//! plays back at its own speed. If the server changes its rate during a
//! take, the take continues in new files (`<time>-2_…`) at the new rate.
//!
//! Takes stamped with a date go into a folder per day, `<folder>/YYYY-MM-DD/`
//! (the day each file starts on): a take that runs past midnight continues
//! in the next day's folder with its next part.

use crate::engine::{MAX_FRAMES, NUM_BUSES, NUM_STRIPS};
use crate::mp3;
use crate::vban::resample::Resampler;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicU32, AtomicUsize};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Strips 1..6 followed by buses A1..B2.
pub const NUM_SOURCES: usize = NUM_STRIPS + NUM_BUSES;
/// Bit mask with every source linked.
pub const ALL_LINKS: u16 = (1 << NUM_SOURCES) - 1;
/// Default ring size in 32-bit words (16 MiB, allocated on first take):
/// ~4 s of all 22 channels at 48 kHz, ~43 s of a stereo mix.
pub const DEFAULT_RING_WORDS: usize = 1 << 22;
/// Largest `data` chunk per file. RIFF sizes are 32-bit; stay clear of 4 GiB.
/// MP3 files stop there too (their Info frame counts bytes in 32 bits).
pub const MAX_WAV_DATA_BYTES: u64 = 0xFFF0_0000;
pub const DEFAULT_FOLDER: &str = "~/Music/BananaSplit";
/// A take continues in a new file after this many minutes.
pub const DEFAULT_SPLIT_MINUTES: u32 = 60;
/// Longest `split` accepted (one day).
pub const MAX_SPLIT_MINUTES: u32 = 24 * 60;

const HDR: usize = 5;
/// Words before the samples of every block in the ring (see the module docs).
pub const HEADER_WORDS: usize = HDR;
const MAGIC: u32 = 0xB5A1_0000;
const ACTIVE: u32 = 1 << 31;
const MULTI: u32 = 1 << 16;
const MASK_SHIFT: u32 = 17;
const HEADER_REFRESH: Duration = Duration::from_secs(2);
const CHUNK_FRAMES: usize = 4096;

// ---------------------------------------------------------------------------
// Source ids and settings
// ---------------------------------------------------------------------------

/// Short id of a recorder source: `"1"`..`"6"` for strips, `"A1"`..`"B2"` for buses.
pub fn source_id(i: usize) -> &'static str {
    const IDS: [&str; NUM_SOURCES] = ["1", "2", "3", "4", "5", "6", "A1", "A2", "A3", "B1", "B2"];
    IDS.get(i).copied().unwrap_or("?")
}

/// Parse `"1, 3, A1, B2"` (or `"none"` / empty) into a link mask.
pub fn parse_links(v: &str) -> Result<u16, String> {
    let mut mask = 0u16;
    for tok in v.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if tok.eq_ignore_ascii_case("none") {
            continue;
        }
        match (0..NUM_SOURCES).position(|i| source_id(i).eq_ignore_ascii_case(tok)) {
            Some(i) => mask |= 1 << i,
            None => return Err(format!("unknown recorder source {tok:?} (expected 1-6, A1, A2, A3, B1, B2)")),
        }
    }
    Ok(mask)
}

pub fn format_links(mask: u16) -> String {
    let v: Vec<&str> = (0..NUM_SOURCES).filter(|i| mask & (1 << i) != 0).map(source_id).collect();
    if v.is_empty() { "none".into() } else { v.join(",") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// MP3 at the bitrate in `Settings::kbps`.
    Mp3,
    Wav16,
    Wav24,
    Wav32f,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Mp3, Format::Wav16, Format::Wav24, Format::Wav32f];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Mp3 => "mp3",
            Format::Wav16 => "wav16",
            Format::Wav24 => "wav24",
            Format::Wav32f => "wav32f",
        }
    }

    pub fn parse(v: &str) -> Result<Format, String> {
        Format::ALL
            .into_iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(v.trim()))
            .ok_or_else(|| format!("unknown recorder format {v:?} (expected mp3, wav16, wav24, wav32f)"))
    }

    /// Bytes per sample of a WAV format (MP3: those of 32-bit float, unused).
    fn bytes_per_sample(self) -> u16 {
        match self {
            Format::Wav16 => 2,
            Format::Wav24 => 3,
            Format::Wav32f | Format::Mp3 => 4,
        }
    }

    pub fn ext(self) -> &'static str {
        if self == Format::Mp3 { "mp3" } else { "wav" }
    }

    /// Samples beyond full scale are cut off: in 16/24-bit WAV, and in MP3
    /// (the encoder takes full scale at most, as players play it).
    pub fn clips(self) -> bool {
        self != Format::Wav32f
    }
}

/// Parse `bitrate` (kbit/s of MP3 takes).
pub fn parse_kbps(v: &str) -> Result<u32, String> {
    match v.trim().parse::<u32>() {
        Ok(k) if mp3::BITRATES.contains(&k) => Ok(k),
        _ => Err(format!("recorder bitrate must be one of 128, 160, 192, 224, 256 or 320 (kbit/s), not {v:?}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// All linked channels summed into one stereo file.
    Mix,
    /// One stereo file per linked channel.
    Multi,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Mix => "mix",
            Mode::Multi => "multi",
        }
    }

    pub fn parse(v: &str) -> Result<Mode, String> {
        match v.trim().to_ascii_lowercase().as_str() {
            "mix" => Ok(Mode::Mix),
            "multi" | "multitrack" => Ok(Mode::Multi),
            _ => Err(format!("unknown recorder mode {v:?} (expected mix or multi)")),
        }
    }
}

/// Recorder settings that are not real-time parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Output folder as configured (`~/` is expanded when a take starts).
    pub folder: String,
    pub format: Format,
    /// Bitrate of MP3 takes, kbit/s.
    pub kbps: u32,
    pub mode: Mode,
    /// Minutes per file; a longer take continues in `_part2`, ...
    /// (0 = only before 4 GiB).
    pub split_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folder: DEFAULT_FOLDER.into(),
            format: Format::Mp3,
            kbps: mp3::DEFAULT_KBPS,
            mode: Mode::Mix,
            split_minutes: DEFAULT_SPLIT_MINUTES,
        }
    }
}

/// Parse `split` (minutes per file, `0` or `off` = no time limit).
pub fn parse_split(v: &str) -> Result<u32, String> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("off") {
        return Ok(0);
    }
    match v.parse::<u32>() {
        Ok(m) if m <= MAX_SPLIT_MINUTES => Ok(m),
        _ => Err(format!("recorder split must be 0-{MAX_SPLIT_MINUTES} minutes or off, not {v:?}")),
    }
}

pub fn validate_folder(v: &str) -> Result<(), String> {
    if v.trim().is_empty() {
        return Err("recorder folder must not be empty".into());
    }
    if v.len() > 4096 || v.chars().any(char::is_control) {
        return Err("recorder folder contains invalid characters".into());
    }
    Ok(())
}

/// The `YYYY-MM-DD` a time stamp starts with, if it does: the name of the
/// day's folder.
pub fn stamp_day(stamp: &str) -> Option<&str> {
    let d = stamp.get(..10)?;
    let ok = d.bytes().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == b'-' } else { c.is_ascii_digit() });
    ok.then_some(d)
}

/// Expand a leading `~` to `$HOME`.
pub fn expand_home(v: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    if v == "~" {
        if let Some(h) = home() {
            return h;
        }
    } else if let Some(rest) = v.strip_prefix("~/")
        && let Some(h) = home() {
            return h.join(rest);
        }
    PathBuf::from(v)
}

/// File-name label of a source: `"1-Mic"`, `"A1-Speakers"`.
pub fn source_label(i: usize, name: &str) -> String {
    let mut clean = String::new();
    for c in name.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' };
        if !(c == '_' && clean.ends_with('_')) {
            clean.push(c);
        }
    }
    let mut clean: String = clean.trim_matches('_').chars().take(32).collect();
    // `_part2` at the end of a file name is a take's second part (4 GiB):
    // a channel called "Guitar part2" is `Guitar-part2`.
    if let Some(p) = clean.rfind("_part") {
        let n = &clean[p + 5..];
        if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
            clean.replace_range(p..=p, "-");
        }
    }
    if clean.is_empty() { source_id(i).to_string() } else { format!("{}-{clean}", source_id(i)) }
}

// ---------------------------------------------------------------------------
// Lock-free single-producer / single-consumer ring of 32-bit words
// ---------------------------------------------------------------------------

/// Samples travel as `f32::to_bits` in `AtomicU32` slots. Relaxed atomic
/// loads/stores compile to plain moves, so this costs nothing over a plain
/// buffer and needs no `unsafe`. `head`/`tail` are monotonically increasing
/// (wrapping) word counters; Release/Acquire on them publishes the slots.
pub struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl Ring {
    pub fn new(words: usize) -> Ring {
        let words = words.max(HDR + 2 * NUM_SOURCES * MAX_FRAMES).next_power_of_two();
        Ring {
            buf: (0..words).map(|_| AtomicU32::new(0)).collect(),
            mask: words - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Producer: reserve `len` words, or `None` if the ring is too full.
    #[inline]
    fn reserve(&self, len: usize) -> Option<RingWriter<'_>> {
        let h = self.head.load(Relaxed);
        let t = self.tail.load(Acquire);
        let free = self.buf.len() - h.wrapping_sub(t);
        if len > free { None } else { Some(RingWriter { ring: self, pos: h, end: h.wrapping_add(len) }) }
    }

    /// Consumer: number of readable words.
    pub fn available(&self) -> usize {
        let t = self.tail.load(Relaxed);
        self.head.load(Acquire).wrapping_sub(t)
    }

    /// Consumer: word at offset `off` from the read position (`off < available()`).
    #[inline]
    pub fn peek(&self, off: usize) -> u32 {
        let t = self.tail.load(Relaxed);
        self.buf[t.wrapping_add(off) & self.mask].load(Relaxed)
    }

    /// Consumer: release `n` words (`n <= available()`).
    pub fn consume(&self, n: usize) {
        let t = self.tail.load(Relaxed);
        self.tail.store(t.wrapping_add(n), Release);
    }
}

struct RingWriter<'a> {
    ring: &'a Ring,
    pos: usize,
    end: usize,
}

impl RingWriter<'_> {
    #[inline(always)]
    fn push(&mut self, v: u32) {
        if self.pos != self.end {
            self.ring.buf[self.pos & self.ring.mask].store(v, Relaxed);
            self.pos = self.pos.wrapping_add(1);
        }
    }

    /// Publish the block. Only whole blocks are ever visible to the consumer.
    #[inline]
    fn commit(mut self) {
        while self.pos != self.end {
            self.push(0); // never happens with correct sizes; keeps the stream framed
        }
        self.ring.head.store(self.end, Release);
    }
}

// ---------------------------------------------------------------------------
// Shared tap state (audio thread <-> control/writer threads)
// ---------------------------------------------------------------------------

/// The part of the recorder the audio thread sees.
///
/// `state` packs everything a cycle needs into one atomic word so the audio
/// thread always sees a consistent session: bit 31 = recording, bits 17..27
/// = latched link mask (multi mode), bit 16 = multi mode, bits 0..15 =
/// session id.
pub struct RecTap {
    ring: OnceLock<Ring>,
    ring_words: usize,
    state: AtomicU32,
    /// Incremented by the audio thread at the end of every cycle (stop handshake).
    cycles: AtomicU32,
    /// Frames dropped because the ring was full (current take).
    dropped: AtomicU32,
    /// The sample rate the engine runs at (0 = not known yet). Set by the
    /// engine; a take starts at this rate.
    pub rate: AtomicU32,
}

impl RecTap {
    pub fn new() -> Arc<RecTap> {
        Self::with_ring_words(DEFAULT_RING_WORDS)
    }

    pub fn with_ring_words(words: usize) -> Arc<RecTap> {
        Arc::new(RecTap {
            ring: OnceLock::new(),
            ring_words: words,
            state: AtomicU32::new(0),
            cycles: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            rate: AtomicU32::new(0),
        })
    }

    /// The ring, allocated on first use (never from the audio thread: it
    /// only uses `OnceLock::get`, which is a single atomic load).
    pub fn ring(&self) -> &Ring {
        self.ring.get_or_init(|| Ring::new(self.ring_words))
    }

    pub fn state_word(session: u16, multi: bool, mask: u16) -> u32 {
        ACTIVE | ((mask & ALL_LINKS) as u32) << MASK_SHIFT | if multi { MULTI } else { 0 } | session as u32
    }

    /// Control side: start session `session`. The audio thread writes
    /// blocks for it from its next cycle on. `mask` is used in multi mode.
    pub fn activate(&self, session: u16, multi: bool, mask: u16) -> u32 {
        self.ring();
        self.dropped.store(0, SeqCst);
        let w = Self::state_word(session, multi, mask);
        self.state.store(w, SeqCst);
        w
    }

    /// Control side: stop. Returns the cycle counter read *after* the stop
    /// became visible; once `cycles()` has moved past it, no further block
    /// of the stopped session can be written.
    pub fn deactivate(&self) -> u32 {
        self.state.fetch_and(!ACTIVE, SeqCst);
        self.cycles.load(SeqCst)
    }

    /// Stop only if session `word` is still the active one (writer error path).
    fn deactivate_if(&self, word: u32) {
        let _ = self.state.compare_exchange(word, word & !ACTIVE, SeqCst, SeqCst);
    }

    pub fn is_active(&self) -> bool {
        self.state.load(SeqCst) & ACTIVE != 0
    }

    pub fn cycles(&self) -> u32 {
        self.cycles.load(SeqCst)
    }

    pub fn dropped_frames(&self) -> u32 {
        self.dropped.load(Relaxed)
    }
}

/// Linear ramp from `v0` to `v1` across `n` samples; sample `n-1` is exactly `v1`.
#[inline(always)]
fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

/// Recorder state private to the audio thread (lives inside the engine).
#[derive(Debug)]
pub struct RecRt {
    session: Option<u16>,
    /// Current mix weight of every source (ramped like routes).
    gain: [f32; NUM_SOURCES],
    /// Frames lost since the last block that made it into the ring.
    gap: u32,
}

impl Default for RecRt {
    fn default() -> Self {
        RecRt { session: None, gain: [0.0; NUM_SOURCES], gap: 0 }
    }
}

fn link_targets(links: u16) -> [f32; NUM_SOURCES] {
    std::array::from_fn(|s| if links & (1 << s) != 0 { 1.0 } else { 0.0 })
}

impl RecRt {
    /// Follow the control state; returns (session, multi, mask) if recording.
    #[inline]
    fn sync(&mut self, st: u32, links: u16) -> Option<(u16, bool, u16)> {
        if st & ACTIVE == 0 {
            self.session = None;
            return None;
        }
        let session = (st & 0xFFFF) as u16;
        if self.session != Some(session) {
            // New take: start exactly at the current link weights, no ramp.
            self.session = Some(session);
            self.gain = link_targets(links);
            self.gap = 0;
        }
        Some((session, st & MULTI != 0, ((st >> MASK_SHIFT) as u16) & ALL_LINKS))
    }

    #[inline]
    fn lose(&mut self, tap: &RecTap, n: usize) {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        self.gap = self.gap.saturating_add(n);
        tap.dropped.fetch_add(n, Relaxed);
    }

    /// A cycle whose outputs were silenced without mixing (e.g. oversize):
    /// the take records `n` frames of silence there.
    pub fn skip(&mut self, tap: &RecTap, links: u16, n: usize) {
        let st = tap.state.load(SeqCst);
        if self.sync(st, links).is_some() {
            self.gap = self.gap.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
        }
        tap.cycles.fetch_add(1, SeqCst);
    }

    /// Record one cycle. `rate` is the sample rate of this cycle (0 = not
    /// known). `strips` is the engine's scratch area holding the post-fader
    /// strip signals (`[(strip*2+ch)*MAX_FRAMES ..]`), valid only where
    /// `strip_live` is true (a fully silent strip is skipped by the engine
    /// and must be read as zeros). `buses` are the finished outputs.
    ///
    /// Real-time safe: no allocation, no locks, no syscalls, no panics.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        tap: &RecTap,
        links: u16,
        n: usize,
        rate: u32,
        strips: &[f32],
        strip_live: &[bool; NUM_STRIPS],
        buses: &[[&mut [f32]; 2]; NUM_BUSES],
    ) {
        let st = tap.state.load(SeqCst);
        if let Some((session, multi, mask)) = self.sync(st, links) {
            self.write_block(tap, session, multi, mask, links, n, rate, strips, strip_live, buses);
        }
        tap.cycles.fetch_add(1, SeqCst);
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn write_block(
        &mut self,
        tap: &RecTap,
        session: u16,
        multi: bool,
        mask: u16,
        links: u16,
        n: usize,
        rate: u32,
        strips: &[f32],
        strip_live: &[bool; NUM_STRIPS],
        buses: &[[&mut [f32]; 2]; NUM_BUSES],
    ) {
        let g0 = self.gain;
        let target = link_targets(links);
        self.gain = target;
        if n == 0 {
            return;
        }
        let chans = if multi { 2 * mask.count_ones() as usize } else { 2 };
        let shapes_ok = n <= MAX_FRAMES
            && strips.len() >= NUM_STRIPS * 2 * MAX_FRAMES
            && buses.iter().all(|b| b[0].len() >= n && b[1].len() >= n);
        if chans == 0 || !shapes_ok {
            self.lose(tap, n);
            return;
        }
        let Some(mut w) = tap.ring.get().and_then(|r| r.reserve(HDR + n * chans)) else {
            self.lose(tap, n);
            return;
        };
        w.push(MAGIC | session as u32);
        w.push(n as u32);
        w.push(self.gap);
        w.push(chans as u32);
        w.push(rate);
        self.gap = 0;

        let sample = |src: usize, i: usize| -> (f32, f32) {
            if src < NUM_STRIPS {
                if strip_live[src] {
                    (strips[(src * 2) * MAX_FRAMES + i], strips[(src * 2 + 1) * MAX_FRAMES + i])
                } else {
                    (0.0, 0.0)
                }
            } else {
                let b = &buses[src - NUM_STRIPS];
                (b[0][i], b[1][i])
            }
        };

        if multi {
            for i in 0..n {
                for src in 0..NUM_SOURCES {
                    if mask & (1 << src) != 0 {
                        let (l, r) = sample(src, i);
                        w.push(l.to_bits());
                        w.push(r.to_bits());
                    }
                }
            }
        } else {
            for i in 0..n {
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for src in 0..NUM_SOURCES {
                    let (a, b) = (g0[src], target[src]);
                    if a == 0.0 && b == 0.0 {
                        continue;
                    }
                    let g = if a == b { a } else { ramp(a, b, i, n) };
                    let (sl, sr) = sample(src, i);
                    l += sl * g;
                    r += sr * g;
                }
                w.push(l.to_bits());
                w.push(r.to_bits());
            }
        }
        w.commit();
    }
}

// ---------------------------------------------------------------------------
// WAV files
// ---------------------------------------------------------------------------

/// Canonical WAV header for 2-channel audio. PCM (format 1) for 16/24 bit;
/// IEEE float (format 3, 18-byte fmt + `fact` chunk) for 32-bit float.
pub fn wav_header(format: Format, sample_rate: u32, data_bytes: u32) -> Vec<u8> {
    const CH: u16 = 2;
    let bps = format.bytes_per_sample();
    let align = CH * bps;
    let float = format == Format::Wav32f;
    let fmt_len: u32 = if float { 18 } else { 16 };
    let fact_len: u32 = if float { 12 } else { 0 };
    let riff = 4 + (8 + fmt_len) + fact_len + 8 + data_bytes;
    let mut h = Vec::with_capacity(58);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&riff.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&fmt_len.to_le_bytes());
    h.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    h.extend_from_slice(&CH.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&(sample_rate.saturating_mul(align as u32)).to_le_bytes());
    h.extend_from_slice(&align.to_le_bytes());
    h.extend_from_slice(&(bps * 8).to_le_bytes());
    if float {
        h.extend_from_slice(&0u16.to_le_bytes()); // cbSize
        h.extend_from_slice(b"fact");
        h.extend_from_slice(&4u32.to_le_bytes());
        h.extend_from_slice(&(data_bytes / align as u32).to_le_bytes());
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_bytes.to_le_bytes());
    h
}

#[inline]
fn encode(format: Format, x: f32, out: &mut Vec<u8>) {
    let x = if x.is_finite() { x } else { 0.0 };
    match format {
        Format::Wav16 => out.extend_from_slice(&((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()),
        Format::Wav24 => out.extend_from_slice(&((x.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
        Format::Wav32f | Format::Mp3 => out.extend_from_slice(&x.to_le_bytes()),
    }
}

/// `path: error`, for errors that are about one file.
fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

struct WavWriter {
    out: BufWriter<File>,
    format: Format,
    sample_rate: u32,
    data_bytes: u64,
    buf: Vec<u8>,
}

impl WavWriter {
    fn new(file: File, format: Format, sample_rate: u32) -> io::Result<WavWriter> {
        let mut out = BufWriter::with_capacity(1 << 18, file);
        out.write_all(&wav_header(format, sample_rate, 0))?;
        Ok(WavWriter { out, format, sample_rate, data_bytes: 0, buf: Vec::with_capacity(CHUNK_FRAMES * 8) })
    }

    fn write(&mut self, lr: &[f32]) -> io::Result<()> {
        self.buf.clear();
        for &x in lr {
            encode(self.format, x, &mut self.buf);
        }
        self.out.write_all(&self.buf)?;
        self.data_bytes += self.buf.len() as u64;
        Ok(())
    }

    /// Rewrite the header with the current size (the file is a valid WAV
    /// up to this point even if BananaSplit is killed afterwards).
    fn refresh(&mut self) -> io::Result<()> {
        let size = u32::try_from(self.data_bytes).unwrap_or(u32::MAX);
        self.out.seek(SeekFrom::Start(0))?; // flushes the buffer first
        self.out.write_all(&wav_header(self.format, self.sample_rate, size))?;
        self.out.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.refresh()?;
        self.out.flush()?;
        // Best effort: some filesystems (FUSE, network) reject fsync.
        let _ = self.out.get_ref().sync_data();
        Ok(())
    }
}

/// An MP3 file being written: an Info frame, then the frames. The Info
/// frame is rewritten every 2 seconds (the file plays up to there if
/// BananaSplit is killed) and at the end, with the exact length.
struct Mp3Writer {
    out: BufWriter<File>,
    enc: mp3::enc::Encoder,
    buf: Vec<u8>,
}

impl Mp3Writer {
    fn new(file: File, rate: u32, kbps: u32) -> io::Result<Mp3Writer> {
        let enc = mp3::enc::Encoder::new(rate, kbps).map_err(io::Error::other)?;
        let mut out = BufWriter::with_capacity(1 << 16, file);
        out.write_all(&enc.info_frame())?;
        Ok(Mp3Writer { out, enc, buf: Vec::with_capacity(1 << 16) })
    }

    fn write(&mut self, lr: &[f32]) -> io::Result<()> {
        self.buf.clear();
        self.enc.encode(lr, &mut self.buf);
        self.out.write_all(&self.buf)
    }

    /// The end of the previous part, before this part's first sample.
    fn preroll(&mut self, lr: &[f32]) -> io::Result<()> {
        self.buf.clear();
        self.enc.preroll(lr, &mut self.buf);
        self.out.write_all(&self.buf)?;
        // (the Info frame's encoder delay now includes it)
        self.refresh()
    }

    /// The start of the next part, after this part's last sample.
    fn write_tail(&mut self, lr: &[f32]) -> io::Result<()> {
        self.buf.clear();
        self.enc.encode_tail(lr, &mut self.buf);
        self.out.write_all(&self.buf)
    }

    fn refresh(&mut self) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&self.enc.info_frame())?;
        self.out.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.buf.clear();
        self.enc.finish(&mut self.buf);
        self.out.write_all(&self.buf)?;
        self.refresh()?;
        self.out.flush()?;
        let _ = self.out.get_ref().sync_data();
        Ok(())
    }
}

/// One file of a track.
struct Sink {
    path: PathBuf,
    kind: SinkKind,
}

enum SinkKind {
    Wav(WavWriter),
    Mp3(Box<Mp3Writer>),
}

impl Sink {
    /// Write the header of a new file (`rate`: the file's sample rate).
    fn new(path: PathBuf, file: File, format: Format, kbps: u32, rate: u32) -> io::Result<Sink> {
        let kind = match format {
            Format::Mp3 => SinkKind::Mp3(Box::new(Mp3Writer::new(file, rate, kbps)?)),
            f => SinkKind::Wav(WavWriter::new(file, f, rate)?),
        };
        Ok(Sink { path, kind })
    }

    fn write(&mut self, lr: &[f32]) -> io::Result<()> {
        match &mut self.kind {
            SinkKind::Wav(w) => w.write(lr),
            SinkKind::Mp3(m) => m.write(lr),
        }
        .map_err(|e| at(&self.path, e))
    }

    fn refresh(&mut self) -> io::Result<()> {
        match &mut self.kind {
            SinkKind::Wav(w) => w.refresh(),
            SinkKind::Mp3(m) => m.refresh(),
        }
        .map_err(|e| at(&self.path, e))
    }

    /// MP3 encodes across a cut (see `mp3::enc::PREROLL`); WAV needs not.
    fn overlaps(&self) -> bool {
        matches!(self.kind, SinkKind::Mp3(_))
    }

    fn preroll(&mut self, lr: &[f32]) -> io::Result<()> {
        match &mut self.kind {
            SinkKind::Wav(_) => Ok(()),
            SinkKind::Mp3(m) => m.preroll(lr),
        }
        .map_err(|e| at(&self.path, e))
    }

    fn write_tail(&mut self, lr: &[f32]) -> io::Result<()> {
        match &mut self.kind {
            SinkKind::Wav(_) => Ok(()),
            SinkKind::Mp3(m) => m.write_tail(lr),
        }
        .map_err(|e| at(&self.path, e))
    }

    /// Nothing written yet, and the server now runs at another rate: the
    /// file starts over at `rate` (the file's rate).
    fn restart(&mut self, rate: u32) -> io::Result<()> {
        match &mut self.kind {
            SinkKind::Wav(w) => {
                w.sample_rate = rate;
                w.refresh()
            }
            SinkKind::Mp3(m) => {
                m.enc = mp3::enc::Encoder::new(rate, m.enc.kbps()).map_err(io::Error::other)?;
                m.out.seek(SeekFrom::Start(0))?;
                m.out.get_ref().set_len(0)?;
                m.out.write_all(&m.enc.info_frame())
            }
        }
        .map_err(|e| at(&self.path, e))
    }

    fn finish(self) -> io::Result<()> {
        match self.kind {
            SinkKind::Wav(w) => w.finish(),
            SinkKind::Mp3(m) => m.finish(),
        }
        .map_err(|e| at(&self.path, e))
    }
}

/// What a track's thread is asked to do, in this order.
enum Job {
    /// Samples (L R …) at the server's rate.
    Write(Vec<f32>),
    /// Rewrite the header.
    Refresh,
    /// Nothing is written yet and the server now runs at `.0`; the files
    /// are at `.1`.
    Rate(u32, u32),
    /// From frame `at` of the file's rate (counted from the take's start)
    /// on, continue in `sink`, and finish the file before it.
    Next { at: u64, sink: Sink },
    /// The take ends after `total` frames at the file's rate.
    Finish { total: u64 },
}

/// The first error of any of a take's track threads.
type Failure = Arc<Mutex<Option<String>>>;

/// One track's thread: converts the sample rate if the file needs it,
/// encodes and writes, and moves on to the next part at its first frame.
/// MP3 parts overlap where they meet: the new part starts with the end of
/// the old one, and the old one is finished once it has the start of the
/// new one (both hidden by their LAME tags), so the cut is encoded like
/// any other moment of the recording.
struct TrackWriter {
    sink: Sink,
    /// Frames written (at the file's rate) since the take started.
    done: u64,
    /// Parts to continue in, with the frame they start at.
    next: VecDeque<(u64, Sink)>,
    /// The part before the current one, and how many more frames it takes.
    closing: Option<(Sink, usize)>,
    /// The last frames written (L R …, at most `PREROLL`).
    recent: Vec<f32>,
    conv: Option<Resampler>,
    tmp: Vec<f32>,
    failure: Failure,
    failed: bool,
}

/// Keep the first error of a take.
fn record(failure: &Failure, e: io::Error) {
    let mut f = lock(failure);
    if f.is_none() {
        *f = Some(e.to_string());
    }
}

impl TrackWriter {
    fn fail(&mut self, e: io::Error) {
        self.failed = true;
        record(&self.failure, e);
    }

    /// Finish the part that still took the start of its successor.
    fn close(&mut self) {
        if let Some((old, _)) = self.closing.take()
            && let Err(e) = old.finish() {
                self.fail(e);
            }
    }

    /// Move on to the parts that start at or before `done`.
    fn advance(&mut self) {
        while self.next.front().is_some_and(|(at, _)| *at <= self.done) {
            let Some((_, sink)) = self.next.pop_front() else { break };
            self.close();
            let old = std::mem::replace(&mut self.sink, sink);
            if self.sink.overlaps() && !self.failed
                && let Err(e) = self.sink.preroll(&self.recent) {
                    self.fail(e);
                }
            if old.overlaps() && !self.failed {
                self.closing = Some((old, mp3::enc::TAIL));
            } else if let Err(e) = old.finish() {
                self.fail(e);
            }
        }
    }

    /// Write `lr` (at the file's rate), up to frame `limit`, across parts.
    fn put(&mut self, mut lr: &[f32], limit: u64) {
        let cap = limit.saturating_sub(self.done).saturating_mul(2).min(usize::MAX as u64) as usize;
        lr = &lr[..lr.len().min(cap)];
        loop {
            self.advance();
            if lr.is_empty() {
                return;
            }
            let room = self.next.front().map_or(usize::MAX, |(at, _)| (at - self.done) as usize);
            let k = (lr.len() / 2).min(room);
            let part = &lr[..2 * k];
            if !self.failed
                && let Err(e) = self.sink.write(part) {
                    self.fail(e);
                }
            // the start of this part ends the one before
            if let Some((old, want)) = &mut self.closing {
                let t = k.min(*want);
                let res = old.write_tail(&part[..2 * t]);
                *want -= t;
                let done = *want == 0;
                if let Err(e) = res {
                    self.fail(e);
                }
                if done {
                    self.close();
                }
            }
            self.recent.extend_from_slice(part);
            let keep = 2 * mp3::enc::PREROLL;
            if self.recent.len() > 2 * keep {
                self.recent.drain(..self.recent.len() - keep);
            }
            self.done += k as u64;
            lr = &lr[2 * k..];
        }
    }

    fn write(&mut self, lr: &[f32]) {
        match &mut self.conv {
            None => self.put(lr, u64::MAX),
            Some(rs) => {
                let mut tmp = std::mem::take(&mut self.tmp);
                tmp.clear();
                rs.process(lr, rs.nominal_step(), &mut tmp);
                self.put(&tmp, u64::MAX);
                self.tmp = tmp;
            }
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Job>) {
        for job in rx.iter() {
            match job {
                Job::Write(v) => self.write(&v),
                Job::Refresh => {
                    if !self.failed
                        && let Err(e) = self.sink.refresh() {
                            self.fail(e);
                        }
                }
                Job::Rate(server, file) => {
                    self.conv = (server != file).then(|| Resampler::new(server, file));
                    if let Err(e) = self.sink.restart(file) {
                        self.fail(e);
                    }
                }
                Job::Next { at, sink } => {
                    self.next.push_back((at, sink));
                    self.advance();
                }
                Job::Finish { total } => {
                    // the converter's last frames: push them out with silence
                    if let Some(rs) = &self.conv {
                        let tail = vec![0f32; 2 * (rs.delay().ceil() as usize + 2)];
                        while self.done < total {
                            let before = self.done;
                            let mut tmp = std::mem::take(&mut self.tmp);
                            tmp.clear();
                            if let Some(rs) = &mut self.conv {
                                rs.process(&tail, rs.nominal_step(), &mut tmp);
                            }
                            self.put(&tmp, total);
                            self.tmp = tmp;
                            if self.done == before {
                                break;
                            }
                        }
                    }
                    self.advance();
                    self.close();
                    for (_, s) in std::mem::take(&mut self.next) {
                        // (a part that never got a frame: none in practice)
                        let p = s.path.clone();
                        let _ = s.finish();
                        let _ = std::fs::remove_file(p);
                    }
                    if let Err(e) = self.sink.finish() {
                        record(&self.failure, e);
                    }
                    return;
                }
            }
        }
        // the take was dropped without finishing: still close the files
        self.close();
        let _ = self.sink.finish();
    }
}

/// The writer thread's end of a track thread.
struct Track {
    tx: mpsc::SyncSender<Job>,
    thread: Option<JoinHandle<()>>,
    /// Samples not sent yet.
    pending: Vec<f32>,
}

/// Jobs a track thread may have queued (about 5 s of audio): beyond that
/// the writer thread waits, and the ring takes up the slack.
const TRACK_QUEUE: usize = 64;

/// What a take's files are and how they are split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub format: Format,
    /// MP3 bitrate, kbit/s.
    pub kbps: u32,
    /// The rate the audio server runs at.
    pub rate: u32,
    /// Largest file (tests make it small).
    pub max_data_bytes: u64,
    /// A new part every this many seconds (0 = only before 4 GiB).
    pub split_secs: u64,
}

impl Spec {
    /// The files' sample rate (MP3 converts rates it cannot store).
    fn file_rate(&self) -> u32 {
        if self.format == Format::Mp3 { mp3::rate_for(self.rate) } else { self.rate }
    }

    /// Frames (at the server's rate) per part.
    fn part_frames(&self) -> u64 {
        let rate = u64::from(self.rate.max(1));
        let bytes = self.max_data_bytes.min(MAX_WAV_DATA_BYTES);
        let mut max = if self.format == Format::Mp3 {
            // whole seconds of CBR frames, room left for the Info frame and the last frames
            let per_sec = u64::from(self.kbps) * 1000 / 8;
            (bytes.saturating_sub(4 * 1441) / per_sec.max(1)).max(1) * rate
        } else {
            (bytes / (2 * u64::from(self.format.bytes_per_sample()))).max(1)
        };
        if self.split_secs > 0 {
            max = max.min(self.split_secs.saturating_mul(rate));
        }
        max
    }
}

/// One recording: 1 (mix) or N (multi) stereo tracks, split into parts.
/// The quietest sample that counts as sound (-80 dBFS); below it, a take
/// records silence.
pub const AUDIBLE: f32 = 1e-4;

pub struct Take {
    session: u16,
    word: u32,
    spec: Spec,
    channels: usize,
    /// Where and under which names (to continue at another sample rate):
    /// the recordings folder, and the day folder in it the files go to.
    root: PathBuf,
    day: Option<String>,
    /// Local time now, as a stamp (when a part starts on a new day, it goes
    /// into that day's folder).
    clock: Clock,
    stamp: String,
    labels: Vec<String>,
    bases: Vec<PathBuf>,
    tracks: Vec<Track>,
    failure: Failure,
    files: Vec<PathBuf>,
    part: u32,
    part_frames: u64,
    max_part_frames: u64,
    frames: u64,
    /// Frames written at the current sample rate (since the take started,
    /// or since the server changed its rate).
    rate_frames: u64,
    /// Seconds recorded before the server's last change of sample rate.
    prior_secs: f64,
    /// The server changed its sample rate during this take, or the take is
    /// converted to a rate MP3 stores.
    rate_note: Option<String>,
    /// Samples beyond full scale that the files had to clip.
    clipped: u64,
    /// `frames` at the last audible sample (see [`AUDIBLE`]).
    heard: u64,
    refreshed: Instant,
}

impl Take {
    /// Create the first part of every track: `<root>/<day>/<stamp>[-k]_<label>.mp3`
    /// (or `.wav`), `<day>` being today by `clock` (or the stamp's day; no
    /// day folder if neither has a date). `-k` is added if any of the names
    /// already exists.
    pub fn create(root: &Path, stamp: &str, labels: &[String], spec: Spec, clock: Clock) -> Result<Take, String> {
        if labels.is_empty() {
            return Err("nothing to record".into());
        }
        if spec.format == Format::Mp3 && mp3::enc::bitrate_index(spec.kbps).is_none() {
            return Err(format!("{} kbit/s is not an MP3 bitrate", spec.kbps));
        }
        let day = stamp_day(&clock()).or(stamp_day(stamp)).map(str::to_string);
        let folder = day.as_ref().map_or_else(|| root.to_path_buf(), |d| root.join(d));
        let folder = folder.as_path();
        std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        // A take's files share `<stamp>[-k]_`: that key must be new, not
        // only these file names (a mix take and a multi-track take started
        // in the same second were listed, and played, as one take). Files
        // of the key can be in the stamp's day folder too (a take that
        // changes its sample rate after midnight).
        let mut dirs = vec![folder.to_path_buf()];
        for d in [Some(root.to_path_buf()), stamp_day(stamp).map(|d| root.join(d))].into_iter().flatten() {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        let taken: Vec<String> = dirs
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flat_map(|d| d.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)))
            .collect();
        let mut chosen = None;
        for k in 1..1000u32 {
            let suffix = if k == 1 { String::new() } else { format!("-{k}") };
            let key = format!("{stamp}{suffix}_");
            let bases: Vec<PathBuf> = labels.iter().map(|l| folder.join(format!("{key}{l}"))).collect();
            if !taken.iter().any(|n| n.starts_with(&key)) && bases.iter().all(|b| !with_ext(b, 1, spec.format).exists()) {
                chosen = Some(bases);
                break;
            }
        }
        let bases = chosen.ok_or("could not find a free file name")?;
        let failure: Failure = Arc::new(Mutex::new(None));
        let sinks = Take::open(&bases, 1, &spec).map_err(|e| e.to_string())?;
        let file_rate = spec.file_rate();
        let mut tracks = Vec::with_capacity(sinks.len());
        let mut files = Vec::with_capacity(sinks.len());
        for sink in sinks {
            files.push(sink.path.clone());
            let (tx, rx) = mpsc::sync_channel(TRACK_QUEUE);
            let w = TrackWriter {
                sink,
                done: 0,
                next: VecDeque::new(),
                closing: None,
                recent: Vec::with_capacity(4 * mp3::enc::PREROLL),
                conv: (file_rate != spec.rate).then(|| Resampler::new(spec.rate.max(1), file_rate)),
                tmp: Vec::new(),
                failure: failure.clone(),
                failed: false,
            };
            let thread = std::thread::Builder::new().name("bs-rec-track".into()).spawn(move || w.run(rx)).ok();
            tracks.push(Track { tx, thread, pending: Vec::with_capacity(2 * CHUNK_FRAMES) });
        }
        let mut t = Take {
            session: 0,
            word: 0,
            spec,
            channels: 2 * labels.len(),
            root: root.to_path_buf(),
            day,
            clock,
            stamp: stamp.to_string(),
            labels: labels.to_vec(),
            bases,
            tracks,
            failure,
            files,
            part: 1,
            part_frames: 0,
            max_part_frames: spec.part_frames(),
            frames: 0,
            rate_frames: 0,
            prior_secs: 0.0,
            rate_note: None,
            clipped: 0,
            heard: 0,
            refreshed: Instant::now(),
        };
        if t.tracks.iter().any(|tr| tr.thread.is_none()) {
            let _ = t.end();
            for f in &t.files {
                let _ = std::fs::remove_file(f);
            }
            return Err("could not start the recorder's file threads".into());
        }
        if file_rate != spec.rate {
            t.rate_note = Some(convert_note(spec.rate, file_rate));
        }
        Ok(t)
    }

    /// Create part `part` of every track (all or none).
    fn open(bases: &[PathBuf], part: u32, spec: &Spec) -> io::Result<Vec<Sink>> {
        let mut sinks = Vec::with_capacity(bases.len());
        let mut made = Vec::new();
        let res = (|| {
            for b in bases {
                let p = with_ext(b, part, spec.format);
                let f = OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| at(&p, e))?;
                made.push(p.clone());
                sinks.push(Sink::new(p.clone(), f, spec.format, spec.kbps, spec.file_rate()).map_err(|e| at(&p, e))?);
            }
            Ok(())
        })();
        match res {
            Ok(()) => Ok(sinks),
            Err(e) => {
                drop(sinks);
                for p in made {
                    let _ = std::fs::remove_file(p);
                }
                Err(e)
            }
        }
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Length so far, in seconds (each part at its own sample rate).
    /// Seconds since the last audible sample (since the start if none).
    pub fn quiet_secs(&self) -> f64 {
        (self.frames - self.heard) as f64 / f64::from(self.sample_rate().max(1))
    }

    pub fn secs(&self) -> f64 {
        self.prior_secs + self.rate_frames as f64 / self.spec.rate.max(1) as f64
    }

    /// The rate of the take's (last) files.
    pub fn sample_rate(&self) -> u32 {
        self.spec.file_rate()
    }

    pub fn clipped(&self) -> u64 {
        self.clipped
    }

    /// What went wrong in a track thread, if anything.
    fn failure(&self) -> Option<io::Error> {
        lock(&self.failure).clone().map(io::Error::other)
    }

    /// Frames at the file's rate for `frames` at the server's.
    fn file_frames(&self, frames: u64) -> u64 {
        let (fr, sr) = (u128::from(self.spec.file_rate()), u128::from(self.spec.rate.max(1)));
        (u128::from(frames) * fr).div_ceil(sr) as u64
    }

    fn send(&self, t: usize, job: Job) -> io::Result<()> {
        self.tracks[t].tx.send(job).map_err(|_| io::Error::other(format!("{}: its file thread stopped", self.labels[t])))
    }

    /// Hand the samples gathered for every track to its thread.
    fn flush(&mut self) -> io::Result<()> {
        for t in 0..self.tracks.len() {
            if !self.tracks[t].pending.is_empty() {
                let v = std::mem::replace(&mut self.tracks[t].pending, Vec::with_capacity(2 * CHUNK_FRAMES));
                self.send(t, Job::Write(v))?;
            }
        }
        Ok(())
    }

    /// The server now runs at `rate`. Before anything was written, the files
    /// simply take the new rate; after, the take continues in new files
    /// (`<stamp>-2_<label>.mp3`, ...), so each file plays at its own speed.
    fn change_rate(&mut self, rate: u32) -> io::Result<()> {
        if rate == self.spec.rate || rate == 0 {
            return Ok(());
        }
        if self.frames == 0 {
            self.spec.rate = rate;
            self.max_part_frames = self.spec.part_frames();
            let file_rate = self.spec.file_rate();
            for t in 0..self.tracks.len() {
                self.send(t, Job::Rate(rate, file_rate))?;
            }
            self.rate_note = (file_rate != rate).then(|| convert_note(rate, file_rate));
            return Ok(());
        }
        let before = self.secs();
        let old_rate = self.spec.rate;
        self.end()?;
        let spec = Spec { rate, ..self.spec };
        let mut next = Take::create(&self.root, &self.stamp, &self.labels, spec, self.clock).map_err(io::Error::other)?;
        next.session = self.session;
        next.word = self.word;
        next.frames = self.frames;
        next.prior_secs = before;
        next.clipped = self.clipped;
        let new_file = next.files.first().cloned();
        let mut files = std::mem::take(&mut self.files);
        files.append(&mut next.files);
        next.files = files;
        let converted = next.rate_note.take().map(|_| format!(" (converted to {} kHz for MP3)", khz(next.sample_rate())));
        next.rate_note = Some(format!(
            "the audio server changed from {} to {} kHz; the recording continues in {}{}",
            khz(old_rate),
            khz(rate),
            new_file.as_deref().and_then(Path::file_name).map_or(String::new(), |f| f.to_string_lossy().into_owned()),
            converted.unwrap_or_default()
        ));
        *self = next;
        Ok(())
    }

    /// Continue every track in its next part, from the frame written last
    /// (in a new day's folder, if the day changed).
    fn next_part(&mut self) -> io::Result<()> {
        self.flush()?;
        let now = (self.clock)();
        if let (Some(_), Some(today)) = (&self.day, stamp_day(&now))
            && self.day.as_deref() != Some(today) {
                let dir = self.root.join(today);
                std::fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
                self.bases = self.bases.iter().map(|b| dir.join(b.file_name().unwrap_or_default())).collect();
                self.day = Some(today.to_string());
            }
        let sinks = Take::open(&self.bases, self.part + 1, &self.spec)?;
        self.part += 1;
        let at = self.file_frames(self.rate_frames);
        for (t, sink) in sinks.into_iter().enumerate() {
            self.files.push(sink.path.clone());
            self.send(t, Job::Next { at, sink })?;
        }
        self.part_frames = 0;
        Ok(())
    }

    /// Append `n` frames to every track; `sample(track, frame)` gives L/R.
    fn emit(&mut self, n: u64, mut sample: impl FnMut(usize, usize) -> (f32, f32)) -> io::Result<()> {
        let mut done = 0u64;
        let clips = self.spec.format.clips();
        while done < n {
            if self.part_frames >= self.max_part_frames {
                self.next_part()?;
            }
            let k = (n - done).min(self.max_part_frames - self.part_frames).min(CHUNK_FRAMES as u64);
            for t in 0..self.tracks.len() {
                let buf = &mut self.tracks[t].pending;
                for f in done..done + k {
                    let (l, r) = sample(t, f as usize);
                    if clips {
                        self.clipped += u64::from(l.abs() > 1.0) + u64::from(r.abs() > 1.0);
                    }
                    if l.abs() > AUDIBLE || r.abs() > AUDIBLE {
                        self.heard = self.frames + f + 1 - done;
                    }
                    buf.push(l);
                    buf.push(r);
                }
                if buf.len() >= 2 * CHUNK_FRAMES {
                    let v = std::mem::replace(buf, Vec::with_capacity(2 * CHUNK_FRAMES));
                    self.send(t, Job::Write(v))?;
                }
            }
            self.part_frames += k;
            self.frames += k;
            self.rate_frames += k;
            done += k;
        }
        Ok(())
    }

    /// Write one block from the ring (header at offset 0), recorded at
    /// `rate` (0 = not known: the take's rate).
    fn write_block(&mut self, ring: &Ring, n: usize, gap: u32, rate: u32) -> io::Result<()> {
        self.change_rate(rate)?;
        self.emit(gap as u64, |_, _| (0.0, 0.0))?;
        let ch = self.channels;
        self.emit(n as u64, |t, f| {
            let base = HDR + f * ch + 2 * t;
            (f32::from_bits(ring.peek(base)), f32::from_bits(ring.peek(base + 1)))
        })?;
        self.failure().map_or(Ok(()), Err)
    }

    fn refresh(&mut self) -> io::Result<()> {
        self.refreshed = Instant::now();
        self.flush()?;
        for t in 0..self.tracks.len() {
            self.send(t, Job::Refresh)?;
        }
        self.failure().map_or(Ok(()), Err)
    }

    /// Finish every file and wait for the track threads.
    fn end(&mut self) -> io::Result<()> {
        let flushed = self.flush();
        let total = self.file_frames(self.rate_frames);
        for t in 0..self.tracks.len() {
            let _ = self.tracks[t].tx.send(Job::Finish { total });
        }
        for tr in &mut self.tracks {
            if let Some(h) = tr.thread.take() {
                let _ = h.join();
            }
        }
        self.tracks.clear();
        flushed?;
        self.failure().map_or(Ok(()), Err)
    }

    fn finish(mut self) -> io::Result<()> {
        self.end()
    }
}

impl Drop for Take {
    fn drop(&mut self) {
        let _ = self.end();
    }
}

fn convert_note(server: u32, file: u32) -> String {
    format!("MP3 cannot hold the audio server's {} kHz: this take is recorded at {} kHz", khz(server), khz(file))
}

/// `48`, `44.1`.
fn khz(rate: u32) -> String {
    let k = rate as f64 / 1000.0;
    if rate.is_multiple_of(1000) { format!("{k:.0}") } else { format!("{k:.1}") }
}

fn with_ext(base: &Path, part: u32, format: Format) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    if part > 1 {
        s.push(format!("_part{part}"));
    }
    s.push(".");
    s.push(format.ext());
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Control + writer thread
// ---------------------------------------------------------------------------

/// What the UIs show.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub on: bool,
    /// Mode of the current (or last) take.
    pub mode: Mode,
    /// Files of the current (or last) take, in the order they were made:
    /// one per track, and again when the take continues in new files (every
    /// `split` minutes, at 4 GB, or when the server changes its sample rate).
    pub files: Vec<PathBuf>,
    /// Tracks of the current (or last) take (files at a time).
    pub tracks: usize,
    /// Frames written in the current (or last) take.
    pub frames: u64,
    /// Sample rate of the current (or last) take (of its last files, if the
    /// server changed its rate during the take).
    pub sample_rate: u32,
    /// Length of the current (or last) take, in seconds.
    pub secs: f64,
    /// Something worth knowing that is not an error (the server changed its
    /// sample rate during the take).
    pub note: Option<String>,
    /// Frames lost to a full ring in the current take (filled with silence).
    pub dropped: u32,
    /// Samples that were louder than full scale and clipped (16/24-bit WAV
    /// and MP3; 32-bit float keeps them).
    pub clipped: u64,
    /// How long the current take has recorded nothing audible (seconds):
    /// the armed channels get no sound.
    pub quiet_secs: f64,
    pub error: Option<String>,
}

impl Status {
    pub fn seconds(&self) -> f64 {
        self.secs
    }

    /// The files being written (the last one of each track).
    pub fn current_files(&self) -> &[PathBuf] {
        &self.files[self.files.len().saturating_sub(self.tracks.max(1))..]
    }
}

enum Cmd {
    Start(Box<Take>, mpsc::Sender<()>),
    Stop { cycles: u32 },
    Exit,
}

/// Local time now as a stamp, `YYYY-MM-DD_HH-MM-SS` (see [`Recorder::with_clock`]).
pub type Clock = fn() -> String;

/// No clock: a take stays in the folder of the day it started.
fn no_clock() -> String {
    String::new()
}

struct Inner {
    settings: Settings,
    session: u16,
    tx: Option<mpsc::Sender<Cmd>>,
}

/// Recorder control. Thread-safe; used by the TUI and GUI threads.
pub struct Recorder {
    tap: Arc<RecTap>,
    sample_rate: u32,
    max_data_bytes: u64,
    clock: Clock,
    inner: Mutex<Inner>,
    status: Arc<Mutex<Status>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Recorder {
    pub fn new(tap: Arc<RecTap>, sample_rate: u32, settings: Settings) -> Recorder {
        Self::with_max_data_bytes(tap, sample_rate, settings, MAX_WAV_DATA_BYTES)
    }

    /// Like `new`, with a custom file split size (tests).
    pub fn with_max_data_bytes(tap: Arc<RecTap>, sample_rate: u32, settings: Settings, max: u64) -> Recorder {
        let status = Arc::new(Mutex::new(Status {
            on: false,
            mode: settings.mode,
            files: Vec::new(),
            tracks: 0,
            frames: 0,
            sample_rate,
            secs: 0.0,
            note: None,
            dropped: 0,
            clipped: 0,
            quiet_secs: 0.0,
            error: None,
        }));
        let (tx, rx) = mpsc::channel();
        let (t2, s2) = (tap.clone(), status.clone());
        let thread = std::thread::Builder::new()
            .name("bs-recorder".into())
            .spawn(move || writer_thread(&t2, &s2, &rx))
            .ok();
        Recorder {
            tap,
            sample_rate,
            max_data_bytes: max,
            clock: no_clock,
            inner: Mutex::new(Inner { settings, session: 0, tx: thread.as_ref().map(|_| tx) }),
            status,
            thread: Mutex::new(thread),
        }
    }

    /// Tell the recorder the local time, so that a take that runs past
    /// midnight continues in the next day's folder.
    pub fn with_clock(mut self, clock: Clock) -> Recorder {
        self.clock = clock;
        self
    }

    pub fn tap(&self) -> &Arc<RecTap> {
        &self.tap
    }

    /// The sample rate a take started now is recorded at: the engine's, once
    /// it runs, else the one given to [`Recorder::new`].
    pub fn sample_rate(&self) -> u32 {
        match self.tap.rate.load(Relaxed) {
            0 => self.sample_rate,
            r => r,
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.inner).settings.clone()
    }

    /// Change settings; they apply to the next take.
    pub fn set_settings(&self, s: Settings) -> Result<(), String> {
        validate_folder(&s.folder)?;
        if s.split_minutes > MAX_SPLIT_MINUTES {
            return Err(format!("recorder split must be 0-{MAX_SPLIT_MINUTES} minutes"));
        }
        parse_kbps(&s.kbps.to_string())?;
        lock(&self.inner).settings = s;
        Ok(())
    }

    pub fn status(&self) -> Status {
        let mut s = lock(&self.status).clone();
        if s.on {
            s.dropped = self.tap.dropped_frames();
        }
        s
    }

    pub fn is_recording(&self) -> bool {
        lock(&self.status).on
    }

    /// Start a take. `links` = sources to record, `names` = the 11 channel
    /// names (for multi-mode file names), `stamp` = file-name prefix.
    pub fn start(&self, links: u16, names: &[String], stamp: &str) -> Result<Vec<PathBuf>, String> {
        let mut inner = lock(&self.inner);
        let tx = inner.tx.clone().ok_or("the recorder is not running")?;
        if lock(&self.status).on {
            return Err("already recording".into());
        }
        let links = links & ALL_LINKS;
        if links == 0 {
            return Err("link at least one channel to the recorder first".into());
        }
        let s = inner.settings.clone();
        validate_folder(&s.folder)?;
        let labels: Vec<String> = match s.mode {
            Mode::Mix => vec!["mix".into()],
            Mode::Multi => (0..NUM_SOURCES)
                .filter(|i| links & (1 << i) != 0)
                .map(|i| source_label(i, names.get(i).map_or("", String::as_str)))
                .collect(),
        };
        let stamp: String =
            stamp.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let rate = self.sample_rate();
        let spec = Spec {
            format: s.format,
            kbps: s.kbps,
            rate,
            max_data_bytes: self.max_data_bytes,
            split_secs: u64::from(s.split_minutes) * 60,
        };
        let mut take = Take::create(&expand_home(&s.folder), &stamp, &labels, spec, self.clock)?;
        inner.session = inner.session.wrapping_add(1);
        let session = inner.session;
        let multi = s.mode == Mode::Multi;
        let mask = if multi { links } else { 0 };
        take.session = session;
        take.word = RecTap::state_word(session, multi, mask);
        let files = take.files().to_vec();
        let (file_rate, note) = (take.sample_rate(), take.rate_note.clone());

        // The writer must own the take before the audio thread produces
        // blocks for it, otherwise the first blocks would be discarded.
        let (ack_tx, ack_rx) = mpsc::channel();
        tx.send(Cmd::Start(Box::new(take), ack_tx)).map_err(|_| "the recorder thread has stopped")?;
        ack_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "the recorder thread is not responding")?;
        {
            let mut st = lock(&self.status);
            *st = Status {
                on: true,
                mode: s.mode,
                files: files.clone(),
                tracks: files.len(),
                frames: 0,
                sample_rate: file_rate,
                secs: 0.0,
                note,
                dropped: 0,
                clipped: 0,
                quiet_secs: 0.0,
                error: None,
            };
        }
        self.tap.activate(session, multi, mask);
        Ok(files)
    }

    /// Stop the current take (non-blocking; files are finalized by the
    /// writer thread within a few periods).
    pub fn stop(&self) {
        let inner = lock(&self.inner);
        {
            let mut st = lock(&self.status);
            if !st.on {
                return;
            }
            st.on = false;
            st.dropped = self.tap.dropped_frames();
        }
        let cycles = self.tap.deactivate();
        if let Some(tx) = &inner.tx {
            let _ = tx.send(Cmd::Stop { cycles });
        }
    }

    /// Stop, finalize all files and end the writer thread. Idempotent.
    pub fn shutdown(&self) {
        self.stop();
        if let Some(tx) = lock(&self.inner).tx.take() {
            let _ = tx.send(Cmd::Exit);
        }
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn writer_thread(tap: &RecTap, status: &Mutex<Status>, rx: &mpsc::Receiver<Cmd>) {
    let mut cur: Option<Take> = None;
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Cmd::Exit),
        };
        let exit = matches!(cmd, Some(Cmd::Exit));
        match cmd {
            Some(Cmd::Start(take, ack)) => {
                drain(tap, &mut cur, status); // anything left belongs to an old session
                if let Some(old) = cur.take() {
                    finish(old, status);
                }
                cur = Some(*take);
                let _ = ack.send(());
            }
            Some(Cmd::Stop { cycles }) => {
                // Wait until the audio thread has certainly finished its last
                // block of this take (or give up if the server stalled).
                let t0 = Instant::now();
                while tap.cycles().wrapping_sub(cycles) < 2 && t0.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drain(tap, &mut cur, status);
                if let Some(t) = cur.take() {
                    finish(t, status);
                }
            }
            Some(Cmd::Exit) | None => {}
        }
        drain(tap, &mut cur, status);
        if let Some(t) = cur.as_mut()
            && t.refreshed.elapsed() >= HEADER_REFRESH
                && let Err(e) = t.refresh() {
                    fail(cur.take(), tap, status, &e);
                }
        if exit {
            if let Some(t) = cur.take() {
                tap.deactivate_if(t.word);
                finish(t, status);
            }
            return;
        }
    }
}

/// Consume every complete block in the ring, writing those of the current take.
fn drain(tap: &RecTap, cur: &mut Option<Take>, status: &Mutex<Status>) {
    let Some(ring) = tap.ring.get() else { return };
    let mut wrote = false;
    loop {
        let avail = ring.available();
        if avail < HDR {
            break;
        }
        let (w0, n, gap, ch) = (ring.peek(0), ring.peek(1) as usize, ring.peek(2), ring.peek(3) as usize);
        let rate = ring.peek(4);
        let len = HDR + n * ch;
        if w0 & 0xFFFF_0000 != MAGIC || n > MAX_FRAMES || ch == 0 || ch > 2 * NUM_SOURCES || len > avail || rate > 10_000_000 {
            // Cannot happen (blocks are published whole); resynchronise anyway.
            ring.consume(avail);
            let mut st = lock(status);
            st.error = Some("recorder stream lost sync; the take may contain a gap".into());
            break;
        }
        if let Some(t) = cur.as_mut()
            && t.session as u32 == w0 & 0xFFFF && t.channels == ch {
                if let Err(e) = t.write_block(ring, n, gap, rate) {
                    ring.consume(len);
                    fail(cur.take(), tap, status, &e);
                    continue;
                }
                wrote = true;
            }
        ring.consume(len);
    }
    if wrote
        && let Some(t) = cur.as_ref() {
            let mut st = lock(status);
            st.frames = t.frames;
            st.secs = t.secs();
            st.sample_rate = t.sample_rate();
            st.clipped = t.clipped;
            st.quiet_secs = t.quiet_secs();
            if t.rate_note.is_some() && st.note != t.rate_note {
                st.note = t.rate_note.clone();
            }
            if st.files.len() != t.files.len() {
                st.files = t.files.clone();
            }
        }
}

fn finish(t: Take, status: &Mutex<Status>) {
    let (frames, secs, rate, clipped, files) = (t.frames, t.secs(), t.sample_rate(), t.clipped, t.files.clone());
    let res = t.finish();
    let mut st = lock(status);
    st.frames = frames;
    st.secs = secs;
    st.sample_rate = rate;
    st.clipped = clipped;
    st.files = files;
    if let Err(e) = res {
        st.error = Some(format!("could not finish the recording: {e}"));
    }
}

fn fail(t: Option<Take>, tap: &RecTap, status: &Mutex<Status>, e: &io::Error) {
    let Some(t) = t else { return };
    tap.deactivate_if(t.word);
    let (frames, secs, clipped, files) = (t.frames, t.secs(), t.clipped, t.files.clone());
    let _ = t.finish();
    let mut st = lock(status);
    st.on = false;
    st.frames = frames;
    st.secs = secs;
    st.clipped = clipped;
    st.files = files;
    st.dropped = tap.dropped_frames();
    st.error = Some(format!("recording stopped: {e}"));
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BUS_IDS, Engine, Shared};

    /// Minimal independent WAV reader: (format tag, channels, rate, bits, samples as f32).
    pub(crate) fn read_wav(path: &Path) -> (u16, u16, u32, u16, Vec<f32>) {
        let b = std::fs::read(path).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize, b.len() - 8, "RIFF size");
        let mut pos = 12;
        let (mut tag, mut ch, mut rate, mut bits) = (0, 0, 0, 0);
        let mut fact = None;
        loop {
            let id = &b[pos..pos + 4];
            let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body = &b[pos + 8..pos + 8 + len];
            match id {
                b"fmt " => {
                    tag = u16::from_le_bytes([body[0], body[1]]);
                    ch = u16::from_le_bytes([body[2], body[3]]);
                    rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                    let byte_rate = u32::from_le_bytes(body[8..12].try_into().unwrap());
                    let align = u16::from_le_bytes([body[12], body[13]]);
                    bits = u16::from_le_bytes([body[14], body[15]]);
                    assert_eq!(align, ch * bits / 8);
                    assert_eq!(byte_rate, rate * align as u32);
                }
                b"fact" => fact = Some(u32::from_le_bytes(body[0..4].try_into().unwrap())),
                b"data" => {
                    assert_eq!(pos + 8 + len, b.len(), "data chunk must end the file");
                    let s: Vec<f32> = match (tag, bits) {
                        (1, 16) => body.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32767.0).collect(),
                        (1, 24) => body
                            .chunks(3)
                            .map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_607.0)
                            .collect(),
                        (3, 32) => body.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect(),
                        _ => panic!("unexpected format {tag}/{bits}"),
                    };
                    if tag == 3 {
                        assert_eq!(fact, Some((s.len() / ch as usize) as u32), "fact chunk");
                    }
                    return (tag, ch, rate, bits, s);
                }
                _ => panic!("unexpected chunk {:?}", String::from_utf8_lossy(id)),
            }
            pos += 8 + len;
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bananasplit-rec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn spec(format: Format, rate: u32, max_data_bytes: u64, split_secs: u64) -> Spec {
        Spec { format, kbps: 320, rate, max_data_bytes, split_secs }
    }

    #[test]
    fn source_ids_match_engine_numbering() {
        for s in 0..NUM_STRIPS {
            assert_eq!(source_id(s), (s + 1).to_string());
        }
        for (b, id) in BUS_IDS.iter().enumerate() {
            assert_eq!(source_id(NUM_STRIPS + b), *id);
        }
        assert_eq!(source_id(NUM_SOURCES), "?");
    }

    #[test]
    fn links_parse_and_format() {
        assert_eq!(parse_links("1, a1 ,B2").unwrap(), 0b100_0100_0001);
        assert_eq!(parse_links("none").unwrap(), 0);
        assert_eq!(parse_links("").unwrap(), 0);
        assert_eq!(format_links(0), "none");
        assert_eq!(format_links(ALL_LINKS), "1,2,3,4,5,6,A1,A2,A3,B1,B2");
        assert_eq!(parse_links(&format_links(0b101_0010_0110)).unwrap(), 0b101_0010_0110);
        assert!(parse_links("7").unwrap_err().contains("\"7\""));
        assert!(parse_links("C1").is_err());
        for i in 0..NUM_SOURCES {
            assert_eq!(parse_links(source_id(i)).unwrap(), 1 << i);
        }
    }

    #[test]
    fn labels_and_settings() {
        assert_eq!(source_label(0, "Mic"), "1-Mic");
        assert_eq!(source_label(6, "My Speakers / L+R"), "A1-My_Speakers_L_R");
        assert_eq!(source_label(10, "  "), "B2");
        // never `_part<n>` at the end: that is how a take's later parts end
        assert_eq!(source_label(0, "Guitar part2"), "1-Guitar-part2");
        assert_eq!(source_label(0, "Guitar_part12"), "1-Guitar-part12");
        assert_eq!(source_label(0, "Guitar part"), "1-Guitar_part");
        assert_eq!(source_label(0, "part2"), "1-part2");
        assert_eq!(Format::parse("WAV24").unwrap(), Format::Wav24);
        assert_eq!(Format::parse(" MP3").unwrap(), Format::Mp3);
        assert!(Format::parse("ogg").is_err());
        assert_eq!(Settings::default().format, Format::Mp3);
        assert_eq!(Settings::default().kbps, 320);
        assert_eq!(parse_kbps("192").unwrap(), 192);
        assert!(parse_kbps("320 ").is_ok() && parse_kbps("300").is_err() && parse_kbps("64").is_err());
        assert_eq!(Mode::parse("multitrack").unwrap(), Mode::Multi);
        assert!(validate_folder("").is_err());
        assert!(validate_folder("a\nb").is_err());
        assert!(validate_folder("~/x").is_ok());
        if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            assert_eq!(expand_home("~/rec"), PathBuf::from(h).join("rec"));
        }
        assert_eq!(expand_home("/abs/~/x"), PathBuf::from("/abs/~/x"));
    }

    #[test]
    fn ring_wraps_and_never_overfills() {
        let r = Ring::new(1);
        let cap = r.capacity();
        assert!(cap.is_power_of_two());
        // fill in uneven blocks across several wraps
        let mut next = 0u32;
        let mut expect = 0u32;
        for round in 0..20 {
            let len = 1000 + round * 777;
            while let Some(mut w) = r.reserve(len) {
                for _ in 0..len {
                    w.push(next);
                    next += 1;
                }
                w.commit();
            }
            assert!(r.available() <= cap);
            assert!(r.reserve(cap - r.available() + 1).is_none());
            let take = r.available() / 2;
            for i in 0..take {
                assert_eq!(r.peek(i), expect + i as u32);
            }
            r.consume(take);
            expect += take as u32;
        }
    }

    #[test]
    fn wav_headers_are_canonical() {
        let h = wav_header(Format::Wav16, 48000, 400);
        assert_eq!(h.len(), 44);
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 400);
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 400);
        let h = wav_header(Format::Wav24, 44100, 600);
        assert_eq!(h.len(), 44);
        assert_eq!(u16::from_le_bytes([h[32], h[33]]), 6);
        assert_eq!(u16::from_le_bytes([h[34], h[35]]), 24);
        let h = wav_header(Format::Wav32f, 96000, 800);
        assert_eq!(h.len(), 58);
        assert_eq!(u16::from_le_bytes([h[20], h[21]]), 3);
        assert_eq!(&h[38..42], b"fact");
        assert_eq!(u32::from_le_bytes(h[46..50].try_into().unwrap()), 100);
        assert_eq!(u32::from_le_bytes(h[54..58].try_into().unwrap()), 800);
    }

    #[test]
    fn encode_clips_and_rounds() {
        let mut v = Vec::new();
        for x in [0.0, 1.0, -1.0, 2.0, -2.0, f32::NAN, 0.5] {
            encode(Format::Wav16, x, &mut v);
        }
        let s: Vec<i16> = v.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(s, [0, 32767, -32767, 32767, -32767, 0, 16384]);
        v.clear();
        for x in [1.0, -1.0, -0.25] {
            encode(Format::Wav24, x, &mut v);
        }
        let s: Vec<i32> = v.chunks(3).map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8).collect();
        assert_eq!(s, [8_388_607, -8_388_607, -2_097_152]);
        v.clear();
        encode(Format::Wav32f, 1.5, &mut v);
        encode(Format::Wav32f, f32::INFINITY, &mut v);
        assert_eq!(v, [1.5f32.to_le_bytes(), 0f32.to_le_bytes()].concat());
    }

    /// Drive RecRt directly with synthetic strips/buses.
    struct Rig {
        rt: RecRt,
        strips: Vec<f32>,
        buses: Vec<Vec<f32>>,
        live: [bool; NUM_STRIPS],
    }

    impl Rig {
        fn new() -> Rig {
            Rig {
                rt: RecRt::default(),
                strips: vec![0.0; NUM_STRIPS * 2 * MAX_FRAMES],
                buses: vec![vec![0.0; MAX_FRAMES]; NUM_BUSES * 2],
                live: [true; NUM_STRIPS],
            }
        }
        /// Every source carries a constant: strip s ch c = (s*2+c+1)/100, bus b = (b*2+c+1)/10.
        fn cycle(&mut self, tap: &RecTap, links: u16, n: usize) {
            for s in 0..NUM_STRIPS {
                for c in 0..2 {
                    self.strips[(s * 2 + c) * MAX_FRAMES..][..n].fill((s * 2 + c + 1) as f32 / 100.0);
                }
            }
            for (k, b) in self.buses.iter_mut().enumerate() {
                b[..n].fill((k + 1) as f32 / 10.0);
            }
            let mut it = self.buses.iter_mut();
            let outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
                let l = &mut it.next().unwrap()[..n];
                let r = &mut it.next().unwrap()[..n];
                [l, r]
            });
            self.rt.push(tap, links, n, 48_000, &self.strips, &self.live, &outs);
        }
    }

    fn read_block(ring: &Ring) -> (u32, usize, u32, usize, Vec<f32>) {
        let (w0, n, gap, ch) = (ring.peek(0), ring.peek(1) as usize, ring.peek(2), ring.peek(3) as usize);
        assert_eq!(ring.peek(4), 48_000, "the block's sample rate");
        let v = (0..n * ch).map(|i| f32::from_bits(ring.peek(HDR + i))).collect();
        ring.consume(HDR + n * ch);
        (w0 & 0xFFFF, n, gap, ch, v)
    }

    #[test]
    fn rt_multi_interleaves_linked_sources_in_order() {
        let tap = RecTap::new();
        let mut rig = Rig::new();
        rig.cycle(&tap, 0, 64); // inactive: nothing written
        assert!(tap.ring.get().is_none());
        let mask = 0b000_0100_0010; // strip 2 and A1
        tap.activate(7, true, mask);
        rig.live[1] = true;
        rig.cycle(&tap, 0, 64); // live links are irrelevant in multi mode
        let ring = tap.ring();
        let (sess, n, gap, ch, v) = read_block(ring);
        assert_eq!((sess, n, gap, ch), (7, 64, 0, 4));
        for f in 0..n {
            assert_eq!(&v[f * 4..f * 4 + 4], &[0.03, 0.04, 0.1, 0.2]);
        }
        // a silent (not live) strip is recorded as zeros, not stale scratch
        rig.live[1] = false;
        rig.cycle(&tap, 0, 8);
        let (_, _, _, _, v) = read_block(ring);
        assert_eq!(&v[..4], &[0.0, 0.0, 0.1, 0.2]);
        assert_eq!(tap.cycles(), 3);
        tap.deactivate();
        rig.cycle(&tap, 0, 64);
        assert_eq!(ring.available(), 0);
    }

    #[test]
    fn rt_mix_sums_and_ramps_link_changes() {
        let tap = RecTap::new();
        let mut rig = Rig::new();
        tap.activate(1, false, 0);
        let n = 100;
        rig.cycle(&tap, 0b000_0100_0001, n); // strip 1 + A1, starts at full weight
        let (_, _, _, ch, v) = read_block(tap.ring());
        assert_eq!(ch, 2);
        assert!((v[0] - 0.11).abs() < 1e-6 && (v[1] - 0.22).abs() < 1e-6);
        // unlink A1: ramps down over the cycle, lands exactly on strip 1 only
        rig.cycle(&tap, 0b000_0000_0001, n);
        let (_, _, _, _, v) = read_block(tap.ring());
        assert!(v[0] > 0.01 && v[0] < 0.11);
        assert!((v[2 * (n - 1)] - 0.01).abs() < 1e-6);
        for f in 1..n {
            assert!(v[2 * f] <= v[2 * (f - 1)], "monotonic ramp");
        }
        // new take: starts at current links without a ramp
        tap.activate(2, false, 0);
        rig.cycle(&tap, 0b100_0000_0000, n); // B2 only
        let (sess, _, _, _, v) = read_block(tap.ring());
        assert_eq!(sess, 2);
        assert!((v[0] - 0.9).abs() < 1e-6 && (v[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rt_full_ring_drops_whole_blocks_and_reports_the_gap() {
        let tap = RecTap::with_ring_words(1); // rounded up to the minimum size
        let mut rig = Rig::new();
        tap.activate(3, true, ALL_LINKS);
        let ring = tap.ring();
        assert_eq!(ring.capacity(), 1 << 19); // 2^19 >= 5 + 22 * 16384
        let per = HDR + 22 * 1024; // 22533 words per 1024-frame block
        let fits = ring.capacity() / per; // 23 blocks, 6029 words left over
        assert_eq!(fits, 23);
        for _ in 0..fits {
            rig.cycle(&tap, 0, 1024);
        }
        assert_eq!(tap.dropped_frames(), 0);
        rig.cycle(&tap, 0, 1024); // needs 22533 > 6029: dropped
        assert_eq!(tap.dropped_frames(), 1024);
        rig.cycle(&tap, 0, 200); // needs 4405: fits, carries the gap
        for _ in 0..fits {
            let (_, n, gap, _, _) = read_block(ring);
            assert_eq!((n, gap), (1024, 0));
        }
        let (sess, n, gap, ch, _) = read_block(ring);
        assert_eq!((sess, n, gap, ch), (3, 200, 1024, 22));
        // an oversize/skipped cycle becomes silence in the next block
        rig.rt.skip(&tap, 0, 5);
        rig.cycle(&tap, 0, 32);
        let (_, n, gap, _, _) = read_block(ring);
        assert_eq!((n, gap), (32, 5));
        assert_eq!(ring.available(), 0);
    }

    #[test]
    fn rt_never_panics_on_bad_shapes() {
        let tap = RecTap::new();
        let mut rt = RecRt::default();
        tap.activate(1, false, 0);
        let mut store: Vec<Vec<f32>> = vec![vec![0.0; 4]; NUM_BUSES * 2];
        let mut si = store.iter_mut();
        let outs: [[&mut [f32]; 2]; NUM_BUSES] =
            std::array::from_fn(|_| [si.next().unwrap().as_mut_slice(), si.next().unwrap().as_mut_slice()]);
        rt.push(&tap, ALL_LINKS, 8, 48_000, &[0.0; 16], &[true; NUM_STRIPS], &outs); // too-short slices
        rt.push(&tap, ALL_LINKS, MAX_FRAMES + 1, 48_000, &[0.0; 16], &[true; NUM_STRIPS], &outs);
        assert_eq!(tap.ring().available(), 0);
        assert_eq!(tap.dropped_frames() as usize, 8 + MAX_FRAMES + 1);
    }

    #[test]
    fn the_files_being_written_are_the_last_of_each_track() {
        let p = |s: &str| PathBuf::from(s);
        let mut st = Status {
            on: true,
            mode: Mode::Multi,
            files: vec![p("t_1-Mic.wav"), p("t_A1-Speakers.wav")],
            tracks: 2,
            frames: 0,
            sample_rate: 48000,
            secs: 0.0,
            note: None,
            dropped: 0,
            clipped: 0,
            quiet_secs: 0.0,
            error: None,
        };
        assert_eq!(st.current_files(), [p("t_1-Mic.wav"), p("t_A1-Speakers.wav")]);
        // the server changed its rate: the take continues in new files
        st.files.extend([p("t-2_1-Mic.wav"), p("t-2_A1-Speakers.wav")]);
        assert_eq!(st.current_files(), [p("t-2_1-Mic.wav"), p("t-2_A1-Speakers.wav")]);
        st.tracks = 0;
        assert_eq!(st.current_files(), [p("t-2_A1-Speakers.wav")]);
        st.files.clear();
        assert!(st.current_files().is_empty());
    }

    #[test]
    fn take_splits_parts_at_the_same_frame_and_files_are_valid() {
        let dir = tmpdir("split");
        let labels = vec!["1-Mic".to_string(), "A1-Out".to_string()];
        // 6 bytes per frame (wav24) -> 100 frames per part
        let mut t = Take::create(&dir, "S", &labels, spec(Format::Wav24, 48000, 600, 0), no_clock).unwrap();
        t.emit(250, |tr, f| ((f as f32) / 1000.0, if tr == 0 { 0.5 } else { -0.5 })).unwrap();
        let files = t.files().to_vec();
        t.finish().unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(
            names,
            [
                "S_1-Mic.wav",
                "S_A1-Out.wav",
                "S_1-Mic_part2.wav",
                "S_A1-Out_part2.wav",
                "S_1-Mic_part3.wav",
                "S_A1-Out_part3.wav"
            ]
        );
        let mut frame = 0;
        for part in 0..3 {
            let (tag, ch, rate, bits, s) = read_wav(&files[part * 2]);
            assert_eq!((tag, ch, rate, bits), (1, 2, 48000, 24));
            let (_, _, _, _, s2) = read_wav(&files[part * 2 + 1]);
            assert_eq!(s.len(), s2.len());
            assert_eq!(s.len() / 2, if part < 2 { 100 } else { 50 });
            for f in 0..s.len() / 2 {
                assert!((s[2 * f] - frame as f32 / 1000.0).abs() < 1e-6);
                assert!((s2[2 * f + 1] + 0.5).abs() < 1e-6);
                frame += 1;
            }
        }
        // same stamp again -> "-2" instead of overwriting
        let t2 = Take::create(&dir, "S", &labels, spec(Format::Wav16, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        assert!(t2.files()[0].ends_with("S-2_1-Mic.wav"));
        t2.finish().unwrap();
        // another take in that second with other tracks (a mix take after a
        // multi-track one): a key of its own, not one take with S's files
        let t3 = Take::create(&dir, "S", &["mix".to_string()], spec(Format::Wav16, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        assert!(t3.files()[0].ends_with("S-3_mix.wav"), "{:?}", t3.files());
        t3.finish().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn take_follows_the_servers_sample_rate() {
        let dir = tmpdir("rate");
        let labels = vec!["1-Mic".to_string()];
        // Before any frame is written, the files just take the new rate.
        let mut t = Take::create(&dir, "R", &labels, spec(Format::Wav16, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        t.change_rate(44100).unwrap();
        t.emit(441, |_, _| (0.25, -0.25)).unwrap();
        assert!(t.rate_note.is_none());
        // After, the take continues in new files at the new rate.
        t.change_rate(96000).unwrap();
        t.emit(960, |_, _| (0.5, -0.5)).unwrap();
        t.change_rate(96000).unwrap(); // same rate: nothing happens
        t.change_rate(0).unwrap(); // not known: nothing happens
        assert!((t.secs() - 0.02).abs() < 1e-9, "{}", t.secs());
        assert_eq!(t.frames(), 441 + 960);
        let note = t.rate_note.clone().unwrap();
        assert!(note.contains("44.1 to 96 kHz") && note.ends_with("R-2_1-Mic.wav"), "{note}");
        let files = t.files().to_vec();
        t.finish().unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(names, ["R_1-Mic.wav", "R-2_1-Mic.wav"]);
        let (_, ch, rate, _, s) = read_wav(&files[0]);
        assert_eq!((ch, rate, s.len()), (2, 44100, 2 * 441));
        assert!((s[0] - 0.25).abs() < 1e-4);
        let (_, _, rate, _, s) = read_wav(&files[1]);
        assert_eq!((rate, s.len()), (96000, 2 * 960));
        assert!((s[1] + 0.5).abs() < 1e-4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The engine tells the recorder its rate, puts it in every block, and
    /// the files carry it: a take always plays at its own speed.
    #[test]
    fn recorded_files_carry_the_rate_the_server_runs_at() {
        const N: usize = 64;
        let dir = tmpdir("server-rate");
        let sh = Shared::new();
        let tap = RecTap::new();
        let mut eng = Engine::new(sh.clone());
        eng.set_sample_rate(48000); // what libjack said before the server ran us
        eng.attach_recorder(tap.clone());
        sh.rec_links.store(parse_links("A1").unwrap(), Relaxed);
        let mut store = vec![vec![0.0f32; N]; NUM_BUSES * 2];
        let mut run = |eng: &mut Engine| {
            let mut it = store.iter_mut();
            let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
                let l = it.next().unwrap().as_mut_slice();
                let r = it.next().unwrap().as_mut_slice();
                [l, r]
            });
            eng.process(&mut outs);
        };
        run(&mut eng);
        assert_eq!(tap.rate.load(Relaxed), 0, "not known yet");
        sh.sample_rate.store(44100, Relaxed); // what the graph really runs at
        run(&mut eng);
        assert_eq!(tap.rate.load(Relaxed), 44100);
        let settings = Settings { folder: dir.display().to_string(), format: Format::Wav24, ..Settings::default() };
        let rec = Recorder::new(tap.clone(), 48000, settings);
        assert_eq!(rec.sample_rate(), 44100);
        rec.start(sh.rec_links.load(Relaxed), &[], "T").unwrap();
        for _ in 0..10 {
            run(&mut eng);
        }
        sh.sample_rate.store(32000, Relaxed);
        for _ in 0..5 {
            run(&mut eng);
        }
        std::thread::sleep(Duration::from_millis(50));
        rec.stop();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) && rec.status().files.len() < 2 {
            run(&mut eng);
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..4 {
            run(&mut eng);
        }
        rec.shutdown();
        let st = rec.status();
        assert_eq!(st.files.len(), 2, "{st:?}");
        assert_eq!(read_wav(&st.files[0]).2, 44100);
        assert_eq!(read_wav(&st.files[0]).4.len(), 2 * 10 * N);
        assert_eq!(read_wav(&st.files[1]).2, 32000);
        assert_eq!(read_wav(&st.files[1]).4.len(), 2 * 5 * N);
        assert!((st.seconds() - (10.0 * N as f64 / 44100.0 + 5.0 * N as f64 / 32000.0)).abs() < 1e-9);
        assert!(st.note.as_deref().is_some_and(|n| n.contains("44.1 to 32 kHz")), "{st:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Real engine -> recorder -> WAV files, checked sample by sample.
    #[test]
    fn engine_to_files_multi_and_mix_are_sample_aligned() {
        const N: usize = 128;
        let dir = tmpdir("engine");
        let sh = Shared::new();
        let tap = RecTap::new();
        let mut eng = Engine::new(sh.clone());
        eng.attach_recorder(tap.clone());
        sh.strips[0].routes.store(0b00001, Relaxed); // strip 1 -> A1
        sh.strips[1].routes.store(0b00001, Relaxed); // strip 2 -> A1 at -6 dB
        sh.strips[1].gain_db.store(-6.0206);
        sh.rec_links.store(parse_links("1,A1").unwrap(), Relaxed);
        let names: Vec<String> = ["Mic", "Game", "c", "d", "e", "f", "Speakers", "x", "y", "z", "w"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut store = vec![vec![0.0f32; N]; NUM_BUSES * 2];
        // impulse on strips 1 and 2 at (cycle, frame 77) only
        let mut run = |eng: &mut Engine, impulse: bool| {
            for s in 0..NUM_STRIPS {
                for c in 0..2 {
                    let mut v = [0.0f32; N];
                    if impulse && s < 2 {
                        v[77] = [[0.5, 0.25], [0.125, 0.0625]][s][c];
                    }
                    eng.load_input(s, c, &v);
                }
            }
            let mut it = store.iter_mut();
            let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
                let l = it.next().unwrap().as_mut_slice();
                let r = it.next().unwrap().as_mut_slice();
                [l, r]
            });
            eng.process(&mut outs);
        };
        run(&mut eng, false);
        run(&mut eng, false); // ramps settled

        let rec = Recorder::new(
            tap.clone(),
            48000,
            Settings { folder: dir.display().to_string(), format: Format::Wav32f, kbps: 320, mode: Mode::Multi, split_minutes: 0 },
        );
        let files = rec.start(sh.rec_links.load(Relaxed), &names, "T").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("T_1-Mic.wav") && files[1].ends_with("T_A1-Speakers.wav"));
        for cycle in 0..10 {
            run(&mut eng, cycle == 4);
        }
        rec.stop();
        sh.rec_links.store(0, Relaxed); // must not affect the finished take
        for _ in 0..3 {
            run(&mut eng, false); // lets the stop handshake complete
        }
        // second take: mix mode, links changed live are irrelevant here
        rec.set_settings(Settings { mode: Mode::Mix, ..rec.settings() }).unwrap();
        sh.rec_links.store(parse_links("1,A1").unwrap(), Relaxed);
        let mix = rec.start(sh.rec_links.load(Relaxed), &names, "T").unwrap();
        assert_eq!(mix.len(), 1);
        // the same second as the multi-track take: a take of its own
        assert!(mix[0].ends_with("T-2_mix.wav"), "{mix:?}");
        for cycle in 0..6 {
            run(&mut eng, cycle == 2);
        }
        rec.stop();
        for _ in 0..3 {
            run(&mut eng, false);
        }
        rec.shutdown();
        assert_eq!(rec.status().frames, 6 * N as u64);
        assert_eq!(rec.status().clipped, 0, "1.0625 is kept in float, not clipped");
        assert_eq!(tap.dropped_frames(), 0);

        let at = 4 * N + 77;
        let (tag, ch, rate, bits, strip) = read_wav(&files[0]);
        assert_eq!((tag, ch, rate, bits), (3, 2, 48000, 32));
        let (_, _, _, _, bus) = read_wav(&files[1]);
        assert_eq!(strip.len(), 2 * 10 * N, "exactly the 10 cycles recorded");
        assert_eq!(bus.len(), strip.len());
        for f in 0..10 * N {
            let (sl, sr, bl, br) = (strip[2 * f], strip[2 * f + 1], bus[2 * f], bus[2 * f + 1]);
            if f == at {
                assert_eq!((sl, sr), (0.5, 0.25));
                assert!((bl - 0.5625).abs() < 1e-5 && (br - 0.28125).abs() < 1e-5, "{bl} {br}");
            } else {
                assert_eq!((sl, sr, bl, br), (0.0, 0.0, 0.0, 0.0), "frame {f} leaked");
            }
        }
        let (_, _, _, _, m) = read_wav(&mix[0]);
        assert_eq!(m.len(), 2 * 6 * N);
        let at = 2 * N + 77;
        assert!((m[2 * at] - 1.0625).abs() < 1e-5 && (m[2 * at + 1] - 0.53125).abs() < 1e-5);
        assert_eq!(m.iter().filter(|&&x| x != 0.0).count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn take_continues_in_a_new_file_every_split() {
        let dir = tmpdir("hourly");
        let labels = vec!["1-Mic".to_string(), "2-Discord".to_string()];
        // 2000 frames per part at 1 kHz
        let mut t = Take::create(&dir, "H", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 2), no_clock).unwrap();
        t.emit(1500, |_, _| (0.25, -0.25)).unwrap();
        t.emit(3000, |tr, _| (0.5 * tr as f32, 0.0)).unwrap();
        // The server changes its rate: still 2 s per part (4000 frames at 2 kHz).
        t.change_rate(2000).unwrap();
        t.emit(5000, |_, _| (0.0, 0.0)).unwrap();
        assert_eq!(t.frames(), 9500);
        let files = t.files().to_vec();
        t.finish().unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(
            names,
            [
                "H_1-Mic.wav", "H_2-Discord.wav", "H_1-Mic_part2.wav", "H_2-Discord_part2.wav",
                "H_1-Mic_part3.wav", "H_2-Discord_part3.wav", "H-2_1-Mic.wav", "H-2_2-Discord.wav",
                "H-2_1-Mic_part2.wav", "H-2_2-Discord_part2.wav",
            ]
        );
        let len = |i: usize| read_wav(&files[i]).4.len() / 2;
        assert_eq!([len(0), len(1), len(2), len(3), len(4), len(5)], [2000, 2000, 2000, 2000, 500, 500]);
        assert_eq!([len(6), len(7), len(8), len(9)], [4000, 4000, 1000, 1000]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The local time `test_clock` gives (only the day-folder tests use it).
    static TEST_NOW: Mutex<&str> = Mutex::new("");
    fn test_clock() -> String {
        lock(&TEST_NOW).to_string()
    }

    #[test]
    fn stamps_name_their_day() {
        assert_eq!(stamp_day("2026-10-28_23-59-58"), Some("2026-10-28"));
        assert_eq!(stamp_day("2026-10-28_23-59-58Z-2"), Some("2026-10-28"));
        assert_eq!(stamp_day("T"), None);
        assert_eq!(stamp_day("2026-1x-28_00"), None);
        assert_eq!(stamp_day(""), None);
    }

    #[test]
    fn takes_go_into_the_folder_of_each_files_day() {
        let dir = tmpdir("days");
        let labels = vec!["mix".to_string()];
        let rel = |p: &PathBuf| p.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
        // A take without a clock goes into the folder of its stamp's day;
        // one without a date in its stamp straight into the folder.
        let t = Take::create(&dir, "2026-10-27_10-00-00", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        assert_eq!(rel(&t.files()[0]), "2026-10-27/2026-10-27_10-00-00_mix.wav");
        t.finish().unwrap();
        let t = Take::create(&dir, "T", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        assert_eq!(rel(&t.files()[0]), "T_mix.wav");
        t.finish().unwrap();

        // 2000 frames per part at 1 kHz; midnight comes during part 1.
        *lock(&TEST_NOW) = "2026-10-28_23-59-58";
        let mut t = Take::create(&dir, "2026-10-28_23-59-58", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 2), test_clock).unwrap();
        t.emit(1000, |_, _| (0.25, 0.25)).unwrap();
        *lock(&TEST_NOW) = "2026-10-29_00-00-00";
        t.emit(4500, |_, _| (0.25, 0.25)).unwrap();
        let files = t.files().to_vec();
        t.finish().unwrap();
        assert_eq!(
            files.iter().map(rel).collect::<Vec<_>>(),
            [
                "2026-10-28/2026-10-28_23-59-58_mix.wav",
                "2026-10-29/2026-10-28_23-59-58_mix_part2.wav",
                "2026-10-29/2026-10-28_23-59-58_mix_part3.wav",
            ]
        );
        assert_eq!(files.iter().map(|f| read_wav(f).4.len() / 2).collect::<Vec<_>>(), [2000, 2000, 1500]);

        // The server changes its rate after midnight, while part 1 is still
        // in the day before: the take goes on as `-2` in today's folder, a
        // key that is new in both.
        *lock(&TEST_NOW) = "2026-10-30_23-59-59";
        let mut t = Take::create(&dir, "2026-10-30_23-59-59", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 0), test_clock).unwrap();
        t.emit(500, |_, _| (0.25, 0.25)).unwrap();
        *lock(&TEST_NOW) = "2026-10-31_00-00-01";
        t.change_rate(2000).unwrap();
        t.emit(500, |_, _| (0.25, 0.25)).unwrap();
        let files = t.files().to_vec();
        t.finish().unwrap();
        assert_eq!(
            files.iter().map(rel).collect::<Vec<_>>(),
            ["2026-10-30/2026-10-30_23-59-59_mix.wav", "2026-10-31/2026-10-30_23-59-59-2_mix.wav"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writer_error_stops_the_take_and_reports_it() {
        let dir = tmpdir("err");
        let tap = RecTap::new();
        let rec = Recorder::with_max_data_bytes(
            tap.clone(),
            48000,
            Settings { folder: dir.display().to_string(), format: Format::Wav16, kbps: 320, mode: Mode::Mix, split_minutes: 0 },
            4 * 50, // 50 frames per part
        );
        let names: Vec<String> = (0..NUM_SOURCES).map(|i| i.to_string()).collect();
        rec.start(1, &names, "E").unwrap();
        // make the next part impossible to create
        std::fs::write(dir.join("E_mix_part2.wav"), b"taken").unwrap();
        let mut rig = Rig::new();
        rig.cycle(&tap, 1, 64);
        let t0 = Instant::now();
        while rec.is_recording() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let st = rec.status();
        assert!(!st.on);
        assert!(st.error.as_deref().unwrap_or("").contains("part2"), "{:?}", st.error);
        assert!(!tap.is_active(), "audio thread told to stop");
        let (_, _, _, _, s) = read_wav(&dir.join("E_mix.wav"));
        assert_eq!(s.len(), 100, "first part finalized with its 50 frames");
        rec.shutdown();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clipping_is_counted_only_for_integer_formats() {
        let dir = tmpdir("clip");
        let labels = vec!["mix".to_string()];
        let mut t = Take::create(&dir, "C", &labels, spec(Format::Wav16, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        t.emit(10, |_, f| (if f < 4 { 1.5 } else { 0.5 }, -1.0)).unwrap(); // exactly 1.0 is not clipping
        assert_eq!(t.clipped(), 4);
        t.finish().unwrap();
        let mut t = Take::create(&dir, "F", &labels, spec(Format::Wav32f, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        t.emit(10, |_, _| (1.5, -3.0)).unwrap();
        assert_eq!(t.clipped(), 0, "float keeps values above full scale");
        let f = t.files()[0].clone();
        t.finish().unwrap();
        let (_, _, _, _, s) = read_wav(&f);
        assert_eq!(&s[..2], &[1.5, -3.0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A take whose armed channels get no sound says so (the 2.8.6 update
    /// left Discord on another strip, and a take recorded only silence).
    #[test]
    fn a_take_knows_how_long_it_has_been_silent() {
        let dir = tmpdir("quiet");
        let labels = vec!["1-Mic".to_string(), "B2-Stream".to_string()];
        let mut t = Take::create(&dir, "Q", &labels, spec(Format::Wav16, 1000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        t.emit(3000, |_, _| (0.0, 0.0)).unwrap();
        assert_eq!(t.quiet_secs(), 3.0, "silent from the start");
        t.emit(1000, |tr, f| if tr == 1 && f == 499 { (0.0, 0.01) } else { (5e-5, -5e-5) }).unwrap();
        assert_eq!(t.quiet_secs(), 0.5, "one sound on one track, half a second ago; -86 dB is no sound");
        t.emit(10, |_, _| (0.2, 0.2)).unwrap();
        assert_eq!(t.quiet_secs(), 0.0);
        t.finish().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An MP3 file decoded the way the player reads it: (rate, samples L R …).
    fn read_mp3(path: &Path) -> (u32, Vec<f32>) {
        let info = crate::mp3::probe(path).unwrap();
        assert!(info.exact, "{}: no Info frame", path.display());
        let mut r = crate::mp3::Reader::open(path, &info).unwrap();
        let mut out = Vec::new();
        while r.read(4096, &mut out).unwrap() > 0 {}
        assert_eq!(out.len() as u64, 2 * info.samples);
        (info.sample_rate, out)
    }

    /// Signal-to-noise ratio (dB) of `got` against `want` over `range` (frames, left channel).
    fn snr(want: &[f32], got: &[f32], range: std::ops::Range<usize>) -> f64 {
        let (mut s, mut e) = (0f64, 0f64);
        for i in range {
            s += f64::from(want[2 * i]).powi(2);
            e += f64::from(got[2 * i] - want[2 * i]).powi(2);
        }
        10.0 * (s / e.max(1e-30)).log10()
    }

    /// A take in MP3 continues in new parts that play back to back: every
    /// part holds exactly its frames, and nothing is lost or doubled where
    /// they meet.
    #[test]
    fn mp3_parts_play_back_to_back() {
        let dir = tmpdir("mp3-split");
        let labels = vec!["1-Mic".to_string(), "A1-Out".to_string()];
        let n = 120_000; // 2.5 s at 48 kHz, parts of 1 s
        let tone = |t: usize, f: f64| (0.5 * (2.0 * std::f64::consts::PI * f * t as f64 / 48000.0).sin()) as f32;
        let mut t = Take::create(&dir, "M", &labels, spec(Format::Mp3, 48000, MAX_WAV_DATA_BYTES, 1), no_clock).unwrap();
        // uneven blocks, as the audio thread delivers them
        let mut done = 0;
        for k in 0.. {
            let len = [128usize, 1, 333, 4096, 256][k % 5].min(n - done);
            if len == 0 {
                break;
            }
            t.emit(len as u64, |tr, f| {
                let i = done + f;
                if tr == 0 { (tone(i, 440.0), tone(i, 660.0)) } else { (tone(i, 1000.0), -tone(i, 1000.0)) }
            })
            .unwrap();
            done += len;
        }
        let files = t.files().to_vec();
        t.finish().unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
        assert_eq!(names, ["M_1-Mic.mp3", "M_A1-Out.mp3", "M_1-Mic_part2.mp3", "M_A1-Out_part2.mp3", "M_1-Mic_part3.mp3", "M_A1-Out_part3.mp3"]);
        for track in 0..2 {
            let mut all = Vec::new();
            for part in 0..3 {
                let (rate, pcm) = read_mp3(&files[2 * part + track]);
                assert_eq!(rate, 48000);
                assert_eq!(pcm.len() / 2, [48000, 48000, 24000][part], "track {track} part {part}");
                all.extend_from_slice(&pcm);
            }
            let want: Vec<f32> = (0..n)
                .flat_map(|i| if track == 0 { [tone(i, 440.0), tone(i, 660.0)] } else { [tone(i, 1000.0), -tone(i, 1000.0)] })
                .collect();
            assert!(snr(&want, &all, 0..n) > 45.0, "track {track}: {:.1} dB", snr(&want, &all, 0..n));
            // where the parts meet as clean as anywhere: no worse than 3 dB
            // over the noisiest stretch away from the cuts
            let worst = |r: std::ops::Range<usize>| r.map(|i| (all[2 * i] - want[2 * i]).abs()).fold(0f32, f32::max);
            let elsewhere = (2400..45_600).step_by(2400).map(|a| worst(a..a + 2400)).fold(0f32, f32::max);
            for at in [48000, 96000] {
                let here = worst(at - 1200..at + 1200);
                assert!(here < 1.41 * elsewhere, "track {track} at {at}: {here} against {elsewhere}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The audio server at 96 kHz: MP3 cannot hold that, so the take is
    /// converted to 48 kHz, plays at the same speed, and its parts still
    /// meet at the same sample.
    #[test]
    fn mp3_takes_at_96_khz_are_converted_to_48() {
        let dir = tmpdir("mp3-96k");
        let n = 240_000; // 2.5 s at 96 kHz
        let tone = |i: usize| (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 96000.0).sin()) as f32;
        let mut t = Take::create(&dir, "K", &["mix".to_string()], spec(Format::Mp3, 96000, MAX_WAV_DATA_BYTES, 1), no_clock).unwrap();
        assert_eq!(t.sample_rate(), 48000);
        assert!(t.rate_note.as_deref().is_some_and(|m| m.contains("96 kHz") && m.contains("48 kHz")), "{:?}", t.rate_note);
        for c in 0..n / 1000 {
            t.emit(1000, |_, f| (tone(c * 1000 + f), tone(c * 1000 + f))).unwrap();
        }
        assert!((t.secs() - 2.5).abs() < 1e-9);
        let files = t.files().to_vec();
        t.finish().unwrap();
        assert_eq!(files.len(), 3);
        let mut all = Vec::new();
        for (part, f) in files.iter().enumerate() {
            let (rate, pcm) = read_mp3(f);
            assert_eq!(rate, 48000);
            assert_eq!(pcm.len() / 2, [48000, 48000, 24000][part]);
            all.extend_from_slice(&pcm);
        }
        // the same tone at 48 kHz, from the same instant
        let want: Vec<f32> = (0..n / 2).flat_map(|i| [tone(2 * i), tone(2 * i)]).collect();
        assert!(snr(&want, &all, 200..n / 2 - 200) > 40.0, "{:.1} dB", snr(&want, &all, 200..n / 2 - 200));
        for at in [48000, 96000] {
            let s = snr(&want, &all, at - 1200..at + 1200);
            assert!(s > 40.0, "at {at}: {s:.1} dB");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mp3_take_follows_the_servers_sample_rate() {
        let dir = tmpdir("mp3-rate");
        let labels = vec!["1-Mic".to_string()];
        let mut t = Take::create(&dir, "R", &labels, spec(Format::Mp3, 48000, MAX_WAV_DATA_BYTES, 0), no_clock).unwrap();
        // before anything is written the file just takes the new rate
        t.change_rate(44100).unwrap();
        t.emit(4410, |_, f| ((f as f32 / 4410.0) - 0.5, 0.25)).unwrap();
        assert!(t.rate_note.is_none());
        // after, it continues in a new file; 96 kHz becomes 48 kHz
        t.change_rate(96000).unwrap();
        t.emit(9600, |_, _| (0.5, -0.5)).unwrap();
        assert!((t.secs() - 0.2).abs() < 1e-9, "{}", t.secs());
        let note = t.rate_note.clone().unwrap();
        assert!(note.contains("44.1 to 96 kHz") && note.contains("R-2_1-Mic.mp3") && note.contains("48 kHz for MP3"), "{note}");
        let files = t.files().to_vec();
        t.finish().unwrap();
        let (r1, a) = read_mp3(&files[0]);
        let (r2, b) = read_mp3(&files[1]);
        assert_eq!((r1, a.len() / 2, r2, b.len() / 2), (44100, 4410, 48000, 4800));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Real engine -> recorder -> MP3 files: every track starts on the same
    /// sample and has the same length, like the WAV ones.
    #[test]
    fn engine_to_mp3_files_are_sample_aligned() {
        const N: usize = 256;
        let dir = tmpdir("engine-mp3");
        let sh = Shared::new();
        let tap = RecTap::new();
        let mut eng = Engine::new(sh.clone());
        eng.attach_recorder(tap.clone());
        sh.strips[0].routes.store(0b00001, Relaxed); // strip 1 -> A1
        sh.rec_links.store(parse_links("1,A1").unwrap(), Relaxed);
        let names: Vec<String> = ["Mic", "b", "c", "d", "e", "f", "Speakers", "x", "y", "z", "w"].iter().map(|s| s.to_string()).collect();
        let mut store = vec![vec![0.0f32; N]; NUM_BUSES * 2];
        let mut run = |eng: &mut Engine, impulse: bool| {
            for s in 0..NUM_STRIPS {
                for c in 0..2 {
                    let mut v = [0.0f32; N];
                    if impulse && s == 0 {
                        v[100] = 0.8;
                    }
                    eng.load_input(s, c, &v);
                }
            }
            let mut it = store.iter_mut();
            let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
                let l = it.next().unwrap().as_mut_slice();
                let r = it.next().unwrap().as_mut_slice();
                [l, r]
            });
            eng.process(&mut outs);
        };
        run(&mut eng, false);
        run(&mut eng, false);
        let rec = Recorder::new(
            tap.clone(),
            48000,
            Settings { folder: dir.display().to_string(), mode: Mode::Multi, ..Settings::default() },
        );
        let files = rec.start(sh.rec_links.load(Relaxed), &names, "T").unwrap();
        assert!(files[0].ends_with("T_1-Mic.mp3") && files[1].ends_with("T_A1-Speakers.mp3"), "{files:?}");
        for cycle in 0..200 {
            run(&mut eng, cycle == 77);
        }
        rec.stop();
        for _ in 0..3 {
            run(&mut eng, false);
        }
        rec.shutdown();
        let st = rec.status();
        assert_eq!(st.frames, 200 * N as u64);
        assert_eq!(st.error, None);
        let (_, strip) = read_mp3(&files[0]);
        let (_, bus) = read_mp3(&files[1]);
        assert_eq!(strip.len(), 2 * 200 * N);
        assert_eq!(bus.len(), strip.len());
        let peak = |v: &[f32]| (0..v.len() / 2).max_by(|&a, &b| v[2 * a].abs().total_cmp(&v[2 * b].abs())).unwrap();
        assert_eq!(peak(&strip), 77 * N + 100, "the impulse where it was recorded");
        assert_eq!(peak(&bus), peak(&strip), "both tracks on the same sample");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recorder_refuses_bad_starts() {
        let dir = tmpdir("refuse");
        let rec = Recorder::new(
            RecTap::new(),
            48000,
            Settings { folder: dir.display().to_string(), ..Settings::default() },
        );
        let names: Vec<String> = (0..NUM_SOURCES).map(|i| format!("ch{i}")).collect();
        assert!(rec.start(0, &names, "x").unwrap_err().contains("link"));
        assert!(!rec.tap().is_active());
        assert!(rec.set_settings(Settings { folder: String::new(), ..Settings::default() }).is_err());
        rec.start(1, &names, "x").unwrap();
        assert!(rec.start(1, &names, "x").unwrap_err().contains("already"));
        rec.shutdown();
        assert!(!rec.tap().is_active());
        assert!(rec.start(1, &names, "y").is_err(), "no starts after shutdown");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
