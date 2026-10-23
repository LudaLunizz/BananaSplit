//! Lock-free single-producer / single-consumer stereo ring buffer with
//! clock-drift correction.
//!
//! On Linux every port lives in one JACK/PipeWire graph driven by one clock,
//! so none of this is needed there. On Windows each WASAPI device (sound
//! card, VB-Audio cable, ...) runs on its **own** clock. The mixer is driven
//! by one of them (the "master"), and every other device is connected
//! through one of these rings. The consumer side reads through a
//! [`DriftReader`], which resamples by a tiny, continuously adjusted ratio
//! (a few hundred ppm at most) to keep the ring at its target fill level, so
//! the clocks never drift apart and the ring never runs dry or overflows.
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
unsafe impl Send for Ring {}

impl Ring {
    fn cap(&self) -> usize {
        self.buf.len()
    }
    fn len(&self) -> usize {
        self.head
            .load(Acquire)
            .wrapping_sub(self.tail.load(Acquire))
    }
}

/// Create a ring holding up to `capacity` stereo frames.
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let capacity = capacity.max(2);
    let buf: Box<[UnsafeCell<Frame>]> = (0..capacity).map(|_| UnsafeCell::new([0.0; 2])).collect();
    let r = Arc::new(Ring {
        buf,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
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
        for f in x.chunks_exact(2) {
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
    pub fn read_interleaved(
        &mut self,
        out: &mut [f32],
        scratch_l: &mut [f32],
        scratch_r: &mut [f32],
    ) -> bool {
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
    fn simulate(
        ppm: f64,
        prod_block: usize,
        cons_block: usize,
        seconds: f64,
    ) -> (DriftReader, f64) {
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
            assert!(
                (rd.ratio() - want).abs() < 60e-6,
                "ppm {ppm}: ratio {} want {want}",
                rd.ratio()
            );
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
}
