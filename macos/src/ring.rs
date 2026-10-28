//! Lock-free single-producer / single-consumer stereo ring buffer with
//! clock-drift correction.
//!
//! On Linux every port lives in one JACK/PipeWire graph driven by one clock,
//! so none of this is needed there. On macOS each Core Audio device (sound
//! card, headset, BlackHole, ...) runs on its **own** clock, as on Windows.
//! The mixer is driven by one of them (the "master"), and every other device
//! is connected through one of these rings. The consumer side reads through
//! a [`DriftReader`], which resamples by a tiny, continuously adjusted ratio
//! (a few hundred ppm at most) to keep the ring at its target fill level, so
//! the clocks never drift apart and the ring never runs dry or overflows.
//!
//! Unlike WASAPI shared mode, Core Audio hands every device's audio over at
//! the rate the device runs at (44.1 kHz, 96 kHz, ...), while the mixer runs
//! at 48 kHz. A ring between different rates is read through a
//! [`RateReader`]: the same drift control around the ratio of the two rates,
//! with a windowed-sinc kernel instead of linear interpolation. [`Reader`]
//! picks the right one.
//!
//! Real-time safe: after construction nothing allocates, locks or blocks.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::Acquire, Ordering::Release};

pub type Frame = [f32; 2];

struct Ring {
    buf: Box<[UnsafeCell<Frame>]>,
    /// Total frames ever written (producer-owned).
    head: AtomicUsize,
    /// Total frames ever read (consumer-owned).
    tail: AtomicUsize,
}

// SAFETY: the producer only writes slots in [tail+len, tail+cap), the
// consumer only reads slots in [tail, head); head/tail use Release/Acquire so
// a slot's contents are visible before its index is published.
unsafe impl Sync for Ring {}
// SAFETY: the ring owns its slots (plain f32 frames) and two atomics; nothing in it is tied to a thread.
unsafe impl Send for Ring {}

impl Ring {
    fn cap(&self) -> usize {
        self.buf.len()
    }
    fn len(&self) -> usize {
        self.head.load(Acquire).wrapping_sub(self.tail.load(Acquire))
    }
}

/// Create a ring holding up to `capacity` stereo frames.
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let capacity = capacity.max(2);
    let buf: Box<[UnsafeCell<Frame>]> = (0..capacity).map(|_| UnsafeCell::new([0.0; 2])).collect();
    let r = Arc::new(Ring { buf, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) });
    (Producer { r: r.clone() }, Consumer { r })
}

pub struct Producer {
    r: Arc<Ring>,
}

impl Producer {
    /// Push one frame; false (frame dropped) if the ring is full.
    #[inline]
    pub fn push(&mut self, f: Frame) -> bool {
        let head = self.r.head.load(Acquire);
        let tail = self.r.tail.load(Acquire);
        if head.wrapping_sub(tail) >= self.r.cap() {
            return false;
        }
        // SAFETY: slot `head` is not readable by the consumer until head is published.
        unsafe { *self.r.buf[head % self.r.cap()].get() = f };
        self.r.head.store(head.wrapping_add(1), Release);
        true
    }

    /// Push planar L/R. Returns how many frames were dropped (ring full).
    pub fn push_planar(&mut self, l: &[f32], r: &[f32]) -> usize {
        let mut dropped = 0;
        for (a, b) in l.iter().zip(r) {
            if !self.push([*a, *b]) {
                dropped += 1;
            }
        }
        dropped
    }

    /// Push interleaved stereo. Returns how many frames were dropped.
    pub fn push_interleaved(&mut self, x: &[f32]) -> usize {
        let mut dropped = 0;
        for f in x.as_chunks::<2>().0 {
            if !self.push([f[0], f[1]]) {
                dropped += 1;
            }
        }
        dropped
    }

    /// Push `n` frames of silence. Returns how many were dropped.
    pub fn push_silence(&mut self, n: usize) -> usize {
        (0..n).filter(|_| !self.push([0.0; 2])).count()
    }

    pub fn len(&self) -> usize {
        self.r.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct Consumer {
    r: Arc<Ring>,
}

impl Consumer {
    #[inline]
    pub fn pop(&mut self) -> Option<Frame> {
        let tail = self.r.tail.load(Acquire);
        let head = self.r.head.load(Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: slot `tail` was published by the producer and is not
        // overwritten until tail moves past it.
        let f = unsafe { *self.r.buf[tail % self.r.cap()].get() };
        self.r.tail.store(tail.wrapping_add(1), Release);
        Some(f)
    }

    /// Discard up to `n` frames.
    pub fn skip(&mut self, n: usize) {
        let n = n.min(self.len());
        let tail = self.r.tail.load(Acquire);
        self.r.tail.store(tail.wrapping_add(n), Release);
    }

    pub fn len(&self) -> usize {
        self.r.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.r.cap()
    }
}

/// Largest correction applied to the playback ratio (±0.5 %). Real clocks
/// differ by well under 0.05 %, so this only matters while settling.
const MAX_CORRECTION: f64 = 0.005;
const KP: f64 = 0.002;
const KI: f64 = 2.0e-6;
const MAX_INTEGRAL: f64 = 0.002;
/// Smoothing for the measured fill level (per read call).
const EMA: f64 = 0.05;

/// Reads from a [`Consumer`] at a slightly variable rate so that the ring
/// stays at `target` frames, regardless of clock drift between the producer
/// and the consumer. Linear interpolation; at ratios this close to 1 it is
/// effectively a tiny, slowly varying fractional delay.
pub struct DriftReader {
    cons: Consumer,
    target: f64,
    ema: f64,
    integral: f64,
    ratio: f64,
    frac: f64,
    prev: Frame,
    cur: Frame,
    primed: bool,
    /// Number of times the ring ran dry or had to be trimmed.
    pub glitches: u32,
}

impl DriftReader {
    /// `target`: desired ring fill in frames (latency added by the ring).
    pub fn new(cons: Consumer, target: usize) -> Self {
        let target = target.clamp(1, cons.capacity() / 2) as f64;
        DriftReader {
            cons,
            target,
            ema: target,
            integral: 0.0,
            ratio: 1.0,
            frac: 0.0,
            prev: [0.0; 2],
            cur: [0.0; 2],
            primed: false,
            glitches: 0,
        }
    }

    pub fn target(&self) -> usize {
        self.target as usize
    }

    /// Current resampling ratio (input frames consumed per output frame).
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    pub fn fill(&self) -> usize {
        self.cons.len()
    }

    #[inline]
    fn next_frame(&mut self) -> Option<Frame> {
        self.cons.pop()
    }

    /// Produce `out_l.len()` frames. With `add`, the result is added to the
    /// existing contents (used to sum several devices on one strip);
    /// otherwise it overwrites them. Returns false if the ring ran dry.
    pub fn read(&mut self, out_l: &mut [f32], out_r: &mut [f32], add: bool) -> bool {
        let n = out_l.len().min(out_r.len());
        if !self.primed {
            // Wait until there is enough buffered to absorb jitter.
            if (self.cons.len() as f64) < self.target {
                if !add {
                    out_l[..n].fill(0.0);
                    out_r[..n].fill(0.0);
                }
                return true;
            }
            self.primed = true;
            self.frac = 0.0;
            self.prev = self.next_frame().unwrap_or([0.0; 2]);
            self.cur = self.next_frame().unwrap_or(self.prev);
            self.ema = self.cons.len() as f64;
            self.integral = 0.0;
        }

        // Far too much buffered (producer burst, or we were stalled): trim
        // back to the target instead of carrying the extra latency forever.
        let fill = self.cons.len() as f64;
        if fill > self.target * 3.0 + n as f64 {
            self.cons.skip((fill - self.target) as usize);
            self.ema = self.target;
            self.glitches += 1;
        }

        // PI controller on the smoothed fill level.
        self.ema += EMA * (self.cons.len() as f64 - self.ema);
        let err = (self.ema - self.target) / self.target;
        self.integral = (self.integral + KI * err).clamp(-MAX_INTEGRAL, MAX_INTEGRAL);
        self.ratio = 1.0 + (KP * err + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);

        for i in 0..n {
            self.frac += self.ratio;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.prev = self.cur;
                match self.next_frame() {
                    Some(f) => self.cur = f,
                    None => {
                        // Ran dry: output silence and re-prime.
                        self.primed = false;
                        self.glitches += 1;
                        if !add {
                            out_l[i..n].fill(0.0);
                            out_r[i..n].fill(0.0);
                        }
                        return false;
                    }
                }
            }
            let t = self.frac as f32;
            let l = self.prev[0] + (self.cur[0] - self.prev[0]) * t;
            let r = self.prev[1] + (self.cur[1] - self.prev[1]) * t;
            if add {
                out_l[i] += l;
                out_r[i] += r;
            } else {
                out_l[i] = l;
                out_r[i] = r;
            }
        }
        true
    }

    /// Like [`read`](Self::read) but writes interleaved stereo into `out`
    /// (`out.len() / 2` frames), using `scratch` (>= 2 * frames) as space.
    pub fn read_interleaved(&mut self, out: &mut [f32], scratch_l: &mut [f32], scratch_r: &mut [f32]) -> bool {
        let n = (out.len() / 2).min(scratch_l.len()).min(scratch_r.len());
        let ok = self.read(&mut scratch_l[..n], &mut scratch_r[..n], false);
        for i in 0..n {
            out[2 * i] = scratch_l[i];
            out[2 * i + 1] = scratch_r[i];
        }
        out[2 * n..].fill(0.0);
        ok
    }
}

// ---------------------------------------------------------------------------
// Between two rates
// ---------------------------------------------------------------------------

/// The kernel of [`RateReader`]: sinc with 48 zero crossings per side under
/// a Kaiser window (beta 8.96, about 90 dB stopband), cut off at 0.4725 of
/// the lower of the two rates (flat to 21 kHz at 48 kHz), tabulated at 512
/// points per zero crossing and interpolated linearly. The kernel of
/// `vban::resample`, which converts network streams the same way.
const ZEROS: usize = 48;
const RES: usize = 512;
const BETA: f64 = 8.96;
const BW: f64 = 0.4725;
/// The largest ratio between the two rates a [`RateReader`] converts:
/// 384 kHz to 48 kHz, 48 kHz to 6 kHz.
pub const MAX_RATIO: f64 = 8.0;

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Reads a [`Consumer`] that holds audio at `fs_in` frames per second and
/// produces it at `fs_out`. Like a [`DriftReader`] it keeps the ring at its
/// target fill level, by nudging the ratio of the two rates by a few hundred
/// ppm at most. The frames the kernel needs are kept in a fixed buffer
/// filled from the ring, so nothing allocates after construction.
///
/// Latency: half the kernel (about 51 input frames at 44.1 kHz into 48 kHz,
/// 1.1 ms), on top of the ring's target.
pub struct RateReader {
    cons: Consumer,
    /// Hold the ring at `target` (a ring between two clocks); false: a fixed
    /// ratio, for a ring filled on the reader's own thread (see `needed`).
    controlled: bool,
    target: f64,
    ema: f64,
    integral: f64,
    /// Input frames per output frame at the nominal rates.
    nominal: f64,
    /// The drift correction of `nominal` (1 ± `MAX_CORRECTION`).
    correction: f64,
    table: Box<[f32]>,
    /// Input frames -> zero crossings.
    scale: f64,
    /// The kernel's half width in input frames.
    half: f64,
    /// Taps to the left of the position.
    reach: u64,
    /// Silent frames in front of the first one from the ring.
    lead: u64,
    /// The newest input frames: input frame `i` is `hist[i & mask]`.
    hist: Box<[Frame]>,
    mask: usize,
    /// Input frames taken so far (the next one taken is frame `have`).
    have: u64,
    /// Input position of the next output frame: frame `pos`, plus `frac`
    /// (0 <= frac < 1).
    pos: u64,
    frac: f64,
    primed: bool,
    /// Number of times the ring ran dry or had to be trimmed.
    pub glitches: u32,
}

impl RateReader {
    /// A ring between two clocks: `target` is the fill level to keep, in
    /// input frames.
    pub fn new(cons: Consumer, target: usize, fs_in: u32, fs_out: u32) -> RateReader {
        let mut r = RateReader::build(cons, fs_in, fs_out);
        r.controlled = true;
        r.target = target.clamp(1, r.cons.capacity() / 2) as f64;
        r.ema = r.target;
        r
    }

    /// A ring filled on the reader's own thread, just before each read with
    /// as many frames as [`needed`](Self::needed) says: the ratio stays
    /// exactly the ratio of the two rates.
    pub fn fixed(cons: Consumer, fs_in: u32, fs_out: u32) -> RateReader {
        let mut r = RateReader::build(cons, fs_in, fs_out);
        r.primed = true;
        r
    }

    fn build(cons: Consumer, fs_in: u32, fs_out: u32) -> RateReader {
        let (fs_in, fs_out) = (f64::from(fs_in.max(1)), f64::from(fs_out.max(1)));
        // cut off below the lower of the two Nyquist frequencies
        let fc = BW * (fs_out / fs_in).clamp(1.0 / MAX_RATIO, 1.0);
        let i0b = bessel_i0(BETA);
        let table: Box<[f32]> = (0..ZEROS * RES + 2)
            .map(|i| {
                let u = i as f64 / RES as f64;
                if u >= ZEROS as f64 {
                    return 0.0;
                }
                let sinc = if u == 0.0 { 1.0 } else { (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u) };
                let x = u / ZEROS as f64;
                (sinc * bessel_i0(BETA * (1.0 - x * x).sqrt()) / i0b) as f32
            })
            .collect();
        let half = ZEROS as f64 / (2.0 * fc);
        let reach = half.floor() as u64 + 1;
        let lead = half.ceil() as u64 + 1;
        // The kernel touches at most 2 * reach + 1 frames at once.
        let size = (2 * reach as usize + 4).next_power_of_two();
        RateReader {
            cons,
            controlled: false,
            target: 1.0,
            ema: 1.0,
            integral: 0.0,
            nominal: fs_in / fs_out,
            correction: 1.0,
            table,
            scale: 2.0 * fc,
            half,
            reach,
            lead,
            hist: vec![[0.0; 2]; size].into_boxed_slice(),
            mask: size - 1,
            have: lead,
            pos: lead,
            frac: 0.0,
            primed: false,
            glitches: 0,
        }
    }

    /// Start again from silence (the history is cleared).
    fn restart(&mut self) {
        self.hist.fill([0.0; 2]);
        self.have = self.lead;
        self.pos = self.lead;
        self.frac = 0.0;
    }

    pub fn target(&self) -> usize {
        self.target as usize
    }

    /// Current resampling ratio (input frames consumed per output frame).
    pub fn ratio(&self) -> f64 {
        self.nominal * self.correction
    }

    pub fn fill(&self) -> usize {
        self.cons.len()
    }

    #[inline]
    fn kernel(&self, u: f64) -> f64 {
        let x = u * RES as f64;
        let i = x as usize;
        if i >= ZEROS * RES {
            return 0.0;
        }
        let f = x - i as f64;
        f64::from(self.table[i]) * (1.0 - f) + f64::from(self.table[i + 1]) * f
    }

    /// How many input frames must be in the ring for the next `n` output
    /// frames (a [`fixed`](Self::fixed) reader): exactly the frames `read`
    /// will take, so a ring filled with this many is empty afterwards.
    pub fn needed(&self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let step = self.nominal * self.correction;
        let (mut pos, mut frac) = (self.pos, self.frac);
        // the same steps `read` takes, so the result is exact
        for _ in 1..n {
            frac += step;
            let whole = frac.floor();
            pos += whole as u64;
            frac -= whole;
        }
        let last = pos + (frac + self.half).floor() as u64;
        (last + 1).saturating_sub(self.have) as usize
    }

    /// Produce `out_l.len()` frames. With `add`, the result is added to the
    /// existing contents; otherwise it overwrites them. Returns false if
    /// the ring ran dry.
    pub fn read(&mut self, out_l: &mut [f32], out_r: &mut [f32], add: bool) -> bool {
        let n = out_l.len().min(out_r.len());
        if !self.primed {
            // Wait until there is enough buffered to absorb jitter.
            if (self.cons.len() as f64) < self.target {
                if !add {
                    out_l[..n].fill(0.0);
                    out_r[..n].fill(0.0);
                }
                return true;
            }
            self.primed = true;
            self.restart();
            self.ema = self.cons.len() as f64;
            self.integral = 0.0;
        }
        if self.controlled {
            // Far too much buffered: trim back to the target (as DriftReader).
            let fill = self.cons.len() as f64;
            if fill > self.target * 3.0 + n as f64 * self.nominal {
                self.cons.skip((fill - self.target) as usize);
                self.ema = self.target;
                self.glitches += 1;
            }
            // PI controller on the smoothed fill level.
            self.ema += EMA * (self.cons.len() as f64 - self.ema);
            let err = (self.ema - self.target) / self.target;
            self.integral = (self.integral + KI * err).clamp(-MAX_INTEGRAL, MAX_INTEGRAL);
            self.correction = 1.0 + (KP * err + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        }
        let step = self.nominal * self.correction;
        for i in 0..n {
            let last = self.pos + (self.frac + self.half).floor() as u64;
            while self.have <= last {
                match self.cons.pop() {
                    Some(f) => {
                        self.hist[self.have as usize & self.mask] = f;
                        self.have += 1;
                    }
                    None => {
                        // Ran dry: silence, and start again from silence.
                        self.glitches += 1;
                        if self.controlled {
                            self.primed = false;
                        } else {
                            self.restart();
                        }
                        if !add {
                            out_l[i..n].fill(0.0);
                            out_r[i..n].fill(0.0);
                        }
                        return false;
                    }
                }
            }
            let (mut l, mut r, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
            for j in self.pos - self.reach..=last {
                // distance from the position, from small numbers only
                let d = ((j as i64 - self.pos as i64) as f64 - self.frac).abs();
                let w = self.kernel(d * self.scale);
                let f = self.hist[j as usize & self.mask];
                l += f64::from(f[0]) * w;
                r += f64::from(f[1]) * w;
                wsum += w;
            }
            let (l, r) = ((l / wsum) as f32, (r / wsum) as f32);
            if add {
                out_l[i] += l;
                out_r[i] += r;
            } else {
                out_l[i] = l;
                out_r[i] = r;
            }
            self.frac += step;
            let whole = self.frac.floor();
            self.pos += whole as u64;
            self.frac -= whole;
        }
        true
    }
}

/// A [`DriftReader`] where both ends run at the same rate, a [`RateReader`]
/// where they don't.
pub enum Reader {
    Drift(DriftReader),
    Rate(RateReader),
}

impl Reader {
    /// `target`: the fill level to keep, in frames at `fs_in` (the ring's rate).
    pub fn new(cons: Consumer, target: usize, fs_in: u32, fs_out: u32) -> Reader {
        if fs_in == fs_out {
            Reader::Drift(DriftReader::new(cons, target))
        } else {
            Reader::Rate(RateReader::new(cons, target, fs_in, fs_out))
        }
    }

    #[inline]
    pub fn read(&mut self, out_l: &mut [f32], out_r: &mut [f32], add: bool) -> bool {
        match self {
            Reader::Drift(r) => r.read(out_l, out_r, add),
            Reader::Rate(r) => r.read(out_l, out_r, add),
        }
    }

    pub fn glitches(&self) -> u32 {
        match self {
            Reader::Drift(r) => r.glitches,
            Reader::Rate(r) => r.glitches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_order_and_full_empty() {
        let (mut p, mut c) = ring(4);
        assert!(c.pop().is_none());
        for i in 0..4 {
            assert!(p.push([i as f32, -(i as f32)]));
        }
        assert!(!p.push([9.0, 9.0]), "full ring must refuse");
        assert_eq!(c.len(), 4);
        for i in 0..4 {
            assert_eq!(c.pop(), Some([i as f32, -(i as f32)]));
        }
        assert!(c.pop().is_none());
        // wrap-around
        for round in 0..10 {
            assert_eq!(p.push_interleaved(&[round as f32, 0.5, 1.0, 1.5]), 0);
            assert_eq!(c.pop(), Some([round as f32, 0.5]));
            assert_eq!(c.pop(), Some([1.0, 1.5]));
        }
    }

    #[test]
    fn threads_keep_every_frame_in_order() {
        let (mut p, mut c) = ring(64);
        let n = 200_000usize;
        let t = std::thread::spawn(move || {
            let mut i = 0;
            while i < n {
                if p.push([i as f32, 0.0]) {
                    i += 1;
                }
            }
        });
        let mut expect = 0;
        while expect < n {
            if let Some(f) = c.pop() {
                assert_eq!(f[0], expect as f32);
                expect += 1;
            }
        }
        t.join().unwrap();
    }

    /// Simulates two devices whose clocks differ by `ppm`, exchanging blocks
    /// of different sizes, for `seconds` of audio at 48 kHz.
    fn simulate(ppm: f64, prod_block: usize, cons_block: usize, seconds: f64) -> (DriftReader, f64) {
        let (mut p, c) = ring(48_000);
        let target = prod_block + cons_block;
        let mut rd = DriftReader::new(c, target);
        let rate_p = 48_000.0 * (1.0 + ppm * 1e-6);
        let rate_c = 48_000.0;
        let (mut tp, mut tc) = (0.0f64, 0.0f64);
        let mut l = vec![0.0f32; cons_block];
        let mut r = vec![0.0f32; cons_block];
        let mut phase = 0.0f32;
        let end = seconds;
        let mut worst_late: f64 = 0.0;
        while tc < end {
            // whichever device's next block is due first runs next
            if tp <= tc {
                let blk: Vec<f32> = (0..prod_block)
                    .flat_map(|_| {
                        phase += 0.01;
                        [phase.sin(), phase.sin()]
                    })
                    .collect();
                p.push_interleaved(&blk);
                tp += prod_block as f64 / rate_p;
            } else {
                rd.read(&mut l, &mut r, false);
                tc += cons_block as f64 / rate_c;
                if tc > seconds * 0.5 {
                    worst_late = worst_late.max((rd.fill() as f64 - target as f64).abs());
                }
            }
        }
        (rd, worst_late)
    }

    #[test]
    fn drift_is_absorbed_without_glitches() {
        for ppm in [-300.0, -50.0, 0.0, 80.0, 300.0] {
            let (rd, worst) = simulate(ppm, 480, 441, 120.0);
            // after the start-up priming, never ran dry or overflowed
            assert!(rd.glitches <= 1, "ppm {ppm}: {} glitches", rd.glitches);
            // ratio converged to the clock ratio
            let want = 1.0 + ppm * 1e-6;
            assert!((rd.ratio() - want).abs() < 60e-6, "ppm {ppm}: ratio {} want {want}", rd.ratio());
            // fill stays within about one block of the target
            assert!(worst < 1000.0, "ppm {ppm}: fill strayed {worst} frames");
        }
    }

    #[test]
    fn underrun_outputs_silence_then_recovers() {
        let (mut p, c) = ring(4096);
        let mut rd = DriftReader::new(c, 256);
        let mut l = vec![1.0f32; 128];
        let mut r = vec![1.0f32; 128];
        // not primed yet -> silence
        assert!(rd.read(&mut l, &mut r, false));
        assert!(l.iter().all(|&x| x == 0.0));
        p.push_planar(&[0.5; 300], &[0.25; 300]);
        assert!(rd.read(&mut l, &mut r, false));
        assert!(l[10..].iter().all(|&x| (x - 0.5).abs() < 1e-6));
        assert!(r[10..].iter().all(|&x| (x - 0.25).abs() < 1e-6));
        // drain -> runs dry
        let mut big_l = vec![0.0f32; 1000];
        let mut big_r = vec![0.0f32; 1000];
        assert!(!rd.read(&mut big_l, &mut big_r, false));
        assert_eq!(rd.glitches, 1);
        assert_eq!(*big_l.last().unwrap(), 0.0);
    }

    #[test]
    fn add_mode_sums() {
        let (mut p, c) = ring(1024);
        let mut rd = DriftReader::new(c, 100);
        p.push_planar(&[0.25; 200], &[0.25; 200]);
        let mut l = vec![1.0f32; 64];
        let mut r = vec![1.0f32; 64];
        rd.read(&mut l, &mut r, true);
        assert!(l[4..].iter().all(|&x| (x - 1.25).abs() < 1e-6));
    }

    use std::f64::consts::PI;

    /// `n` frames of a sine at `f` Hz (left) and its negation (right).
    fn sine(fs: u32, f: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = (amp * (2.0 * PI * f * i as f64 / fs as f64).sin()) as f32;
                [v, -v]
            })
            .collect()
    }

    /// A fixed reader fed one whole signal, read in uneven blocks with
    /// exactly `needed` frames pushed before each read.
    fn convert(fs_in: u32, fs_out: u32, input: &[f32], outputs: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut p, c) = ring(input.len() / 2 + 16);
        let mut rd = RateReader::fixed(c, fs_in, fs_out);
        let (mut ol, mut or) = (Vec::new(), Vec::new());
        let mut fed = 0;
        let mut k = 0;
        while ol.len() < outputs {
            let n = [37usize, 256, 1, 129, 480, 512][k % 6].min(outputs - ol.len());
            k += 1;
            let need = rd.needed(n);
            assert!(fed + need * 2 <= input.len(), "ran out of input");
            assert_eq!(p.push_interleaved(&input[fed..fed + need * 2]), 0);
            fed += need * 2;
            let (mut l, mut r) = (vec![9.0f32; n], vec![9.0f32; n]);
            assert!(rd.read(&mut l, &mut r, false), "ran dry although `needed` frames were there");
            // `needed` is exact: the reader took every frame pushed
            assert_eq!(rd.fill(), 0, "block {k} of {n}");
            ol.extend(l);
            or.extend(r);
        }
        assert_eq!(rd.glitches, 0);
        (ol, or)
    }

    /// Worst error against the ideal sine after the start-up transient. The
    /// kernel is centred, so output frame m is input time m / fs_out.
    fn worst(l: &[f32], r: &[f32], fs_out: u32, f: f64, amp: f64, skip: usize) -> f64 {
        l.iter()
            .zip(r)
            .enumerate()
            .skip(skip)
            .map(|(m, (a, b))| {
                let want = amp * (2.0 * PI * f * m as f64 / fs_out as f64).sin();
                (f64::from(*a) - want).abs().max((f64::from(*b) + want).abs())
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn converts_between_rates_cleanly() {
        for (a, b, f) in [
            (44_100, 48_000, 1000.0),
            (48_000, 44_100, 1000.0),
            (44_100, 48_000, 18_000.0),
            (48_000, 44_100, 15_000.0),
            (96_000, 48_000, 15_000.0),
            (48_000, 96_000, 10_000.0),
            (192_000, 48_000, 440.0),
            (8_000, 48_000, 3000.0),
            (48_000, 16_000, 3000.0),
        ] {
            let input = sine(a, f, a as usize, 0.5);
            let outputs = (b as usize * 9 / 10).min(b as usize - 400);
            let (l, r) = convert(a, b, &input, outputs);
            let e = worst(&l, &r, b, f, 0.5, 400);
            assert!(e < 0.5 * 3e-4, "{a} -> {b}, {f} Hz: error {e:e}");
        }
    }

    #[test]
    fn dc_passes_exactly() {
        let input = vec![0.25f32; 2 * 20_000];
        let (l, r) = convert(44_100, 48_000, &input, 20_000);
        for (a, b) in l.iter().zip(&r).skip(200) {
            assert!((a - 0.25).abs() < 1e-6 && (b - 0.25).abs() < 1e-6, "{a} {b}");
        }
    }

    /// A 44.1 kHz device whose clock is off by `ppm`, delivering blocks of
    /// `pb` frames, read by the 48 kHz mixer in blocks of `cb`, for
    /// `seconds`. Returns the reader and how far the fill strayed from the
    /// target in the second half.
    fn simulate_rates(ppm: f64, pb: usize, cb: usize, seconds: f64) -> (RateReader, f64) {
        let (fs_in, fs_out) = (44_100.0f64, 48_000.0f64);
        let (mut p, c) = ring(48_000);
        let target = pb + cb * 441 / 480 + 96;
        let mut rd = RateReader::new(c, target, 44_100, 48_000);
        let rate_p = fs_in * (1.0 + ppm * 1e-6);
        let (mut tp, mut tc) = (0.0f64, 0.0f64);
        let (mut l, mut r) = (vec![0.0f32; cb], vec![0.0f32; cb]);
        let mut phase = 0.0f64;
        let mut worst_fill: f64 = 0.0;
        while tc < seconds {
            // whichever device's next block is due first runs next
            if tp <= tc {
                let blk: Vec<f32> = (0..pb)
                    .flat_map(|_| {
                        phase += 2.0 * PI * 997.0 / fs_in;
                        [phase.sin() as f32, phase.sin() as f32]
                    })
                    .collect();
                p.push_interleaved(&blk);
                tp += pb as f64 / rate_p;
            } else {
                rd.read(&mut l, &mut r, false);
                tc += cb as f64 / fs_out;
                if tc > seconds * 0.5 {
                    worst_fill = worst_fill.max((rd.fill() as f64 - target as f64).abs());
                }
            }
        }
        (rd, worst_fill)
    }

    #[test]
    fn rate_reader_absorbs_drift_between_clocks() {
        for ppm in [-300.0, 0.0, 120.0, 300.0] {
            // periods of different lengths, as two devices usually have
            let (rd, worst) = simulate_rates(ppm, 512, 441, 120.0);
            assert!(rd.glitches <= 1, "ppm {ppm}: {} glitches", rd.glitches);
            // the ratio converged to the ratio of the two clocks
            let want = 44_100.0 / 48_000.0 * (1.0 + ppm * 1e-6);
            assert!((rd.ratio() / want - 1.0).abs() < 60e-6, "ppm {ppm}: ratio {} want {want}", rd.ratio());
            assert!(worst < 1000.0, "ppm {ppm}: fill strayed {worst} frames");
            // periods of the same length (10 ms each): the fill seen at each
            // read sweeps slowly through a whole block as the clocks drift,
            // and the ratio follows it by a few hundred ppm, but nothing
            // runs dry or overflows
            let (rd, worst) = simulate_rates(ppm, 441, 480, 120.0);
            assert!(rd.glitches <= 1, "ppm {ppm}, 10 ms periods: {} glitches", rd.glitches);
            assert!(worst < 1000.0, "ppm {ppm}, 10 ms periods: fill strayed {worst} frames");
        }
    }

    #[test]
    fn rate_reader_underrun_is_silence_then_recovers() {
        let (mut p, c) = ring(8192);
        let mut rd = RateReader::new(c, 256, 44_100, 48_000);
        let (mut l, mut r) = (vec![1.0f32; 128], vec![1.0f32; 128]);
        // not primed yet: silence
        assert!(rd.read(&mut l, &mut r, false));
        assert!(l.iter().chain(&r).all(|&x| x == 0.0));
        // (less than three times the target: not trimmed)
        p.push_planar(&[0.5; 600], &[0.25; 600]);
        assert!(rd.read(&mut l, &mut r, false));
        let (mut big_l, mut big_r) = (vec![0.0f32; 2000], vec![0.0f32; 2000]);
        // the kernel has settled on the level after its half width
        assert!(rd.read(&mut big_l[..400], &mut big_r[..400], false));
        assert!(big_l[100..400].iter().all(|&x| (x - 0.5).abs() < 1e-4), "{:?}", &big_l[100..110]);
        assert!(big_r[100..400].iter().all(|&x| (x - 0.25).abs() < 1e-4));
        // drain: runs dry, rest is silence
        assert!(!rd.read(&mut big_l, &mut big_r, false));
        assert_eq!(rd.glitches, 1);
        assert_eq!(*big_l.last().unwrap(), 0.0);
        // primes again once the target is there
        p.push_planar(&[0.5; 600], &[0.5; 600]);
        assert!(rd.read(&mut l, &mut r, false));
        assert_eq!(rd.glitches, 1);
    }

    #[test]
    fn reader_picks_by_rate() {
        let (_, c) = ring(64);
        assert!(matches!(Reader::new(c, 16, 48_000, 48_000), Reader::Drift(_)));
        let (_, c) = ring(64);
        assert!(matches!(Reader::new(c, 16, 44_100, 48_000), Reader::Rate(_)));
    }
}
