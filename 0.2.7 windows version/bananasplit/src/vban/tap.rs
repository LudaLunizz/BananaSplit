//! The audio thread's side of VBAN: one ring per stream between the
//! network threads and the engine.
//!
//! * **Incoming** (network thread -> engine): the receiver has already
//!   decoded and resampled the stream to the mixer's rate. Each cycle the
//!   engine adds `n` frames of it to its strip's input, before the strip is
//!   processed, so a VBAN stream goes through gain, EQ, mute and routing
//!   like anything else on that strip. When the ring runs dry the stream
//!   fades out and waits until the receiver's resume level is reached
//!   again, then fades back in. Moving a stream to another strip
//!   cross-fades it over one cycle.
//! * **Outgoing** (engine -> network thread): at the end of each cycle the
//!   engine copies the stream's source (a strip after its fader, or a bus)
//!   into the ring; the sender thread packs it into VBAN packets.
//!
//! Everything here is real-time safe: atomics and preallocated rings only.

use super::ring::Ring;
use crate::engine::{MAX_FRAMES, NUM_BUSES, NUM_STRIPS};
use crate::recorder::NUM_SOURCES;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, fence};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub const NUM_IN: usize = 8;
pub const NUM_OUT: usize = 8;
/// "No strip" / "no source".
pub const NONE: u8 = u8::MAX;
/// Ring size in samples (stereo frames x 2): 1.36 s at 48 kHz.
pub const RING_SAMPLES: usize = 1 << 17;

pub struct InSlot {
    ring: OnceLock<Ring>,
    /// Strip the stream is added to; NONE = stream off (fades out).
    pub(crate) strip: AtomicU8,
    /// Frames that must be buffered before playback (re)starts.
    pub(crate) resume: AtomicU32,
    /// Flush handshake: the receiver bumps `flush_req`, the engine
    /// empties the ring and copies the value to `flush_ack`.
    pub(crate) flush_req: AtomicU32,
    pub(crate) flush_ack: AtomicU32,
    /// The ring ran dry while playing.
    pub(crate) underruns: AtomicU32,
    pub(crate) playing: AtomicBool,
}

impl Default for InSlot {
    fn default() -> Self {
        InSlot {
            ring: OnceLock::new(),
            strip: AtomicU8::new(NONE),
            resume: AtomicU32::new(0),
            flush_req: AtomicU32::new(0),
            flush_ack: AtomicU32::new(0),
            underruns: AtomicU32::new(0),
            playing: AtomicBool::new(false),
        }
    }
}

impl InSlot {
    /// The ring, allocated on first use (by the network thread; the engine
    /// only ever calls `OnceLock::get`).
    pub(crate) fn ring(&self) -> &Ring {
        self.ring.get_or_init(|| Ring::new(RING_SAMPLES))
    }
}

pub struct OutSlot {
    ring: OnceLock<Ring>,
    /// Source 0..=10 (strips 1-6, then buses A1-B2); NONE = off.
    pub(crate) source: AtomicU8,
    /// Frames the engine could not hand over (the sender fell behind).
    pub(crate) dropped: AtomicU32,
}

impl Default for OutSlot {
    fn default() -> Self {
        OutSlot { ring: OnceLock::new(), source: AtomicU8::new(NONE), dropped: AtomicU32::new(0) }
    }
}

impl OutSlot {
    pub(crate) fn ring(&self) -> &Ring {
        self.ring.get_or_init(|| Ring::new(RING_SAMPLES))
    }
}

/// Shared between the engine and the VBAN threads.
pub struct VbanTap {
    pub(crate) ins: [InSlot; NUM_IN],
    pub(crate) outs: [OutSlot; NUM_OUT],
    /// Frames in the last engine cycle.
    pub(crate) period: AtomicU32,
    pub(crate) cycles: AtomicU32,
    /// When the last cycle took its frames from the incoming rings (ns
    /// since `base`), published with `period` under the seqlock `seq`, so
    /// the receiver can tell how far the engine has got into playing them.
    base: Instant,
    seq: AtomicU32,
    cycle_ns: AtomicU64,
    /// The engine's sample rate (0 = not known yet). Set by the engine; the
    /// network threads follow it.
    pub rate: AtomicU32,
}

impl Default for VbanTap {
    fn default() -> Self {
        VbanTap {
            ins: Default::default(),
            outs: Default::default(),
            period: AtomicU32::new(0),
            cycles: AtomicU32::new(0),
            base: Instant::now(),
            seq: AtomicU32::new(0),
            cycle_ns: AtomicU64::new(0),
            rate: AtomicU32::new(0),
        }
    }
}

impl VbanTap {
    pub fn new() -> Arc<VbanTap> {
        Arc::new(VbanTap::default())
    }

    // ---- for tests that drive the engine without the network threads ----

    #[doc(hidden)]
    /// Route incoming stream `k` to `strip` (allocates its ring).
    pub fn test_in(&self, k: usize, strip: usize, resume: u32) {
        let s = &self.ins[k];
        s.ring();
        s.resume.store(resume, Relaxed);
        s.strip.store(strip as u8, Relaxed);
    }

    #[doc(hidden)]
    /// Stereo frames for incoming stream `k` (false if they do not fit).
    pub fn test_push_in(&self, k: usize, frames: &[f32]) -> bool {
        self.ins[k].ring.get().is_some_and(|r| r.push(frames))
    }

    #[doc(hidden)]
    pub fn test_flush_in(&self, k: usize) {
        self.ins[k].flush_req.fetch_add(1, Release);
    }

    #[doc(hidden)]
    /// Feed outgoing stream `k` from `source` (allocates its ring).
    pub fn test_out(&self, k: usize, source: usize) {
        let s = &self.outs[k];
        s.ring();
        s.source.store(source as u8, Relaxed);
    }

    #[doc(hidden)]
    /// Take everything outgoing stream `k` has; returns the sample count.
    pub fn test_drain_out(&self, k: usize) -> usize {
        self.outs[k].ring.get().map_or(0, |r| {
            let n = r.available();
            r.consume(n);
            n
        })
    }

    fn ns(&self, t: Instant) -> u64 {
        t.saturating_duration_since(self.base).as_nanos() as u64
    }

    /// Frames of incoming stream `ring` that are still to be heard at
    /// `now`, as a continuous quantity: the engine takes a whole period at
    /// the start of each cycle but plays it out over the period, so this
    /// counts the part of the current period not yet played as well.
    pub(crate) fn fill(&self, ring: &Ring, now: Instant, sample_rate: u32) -> f64 {
        for _ in 0..1000 {
            let s1 = self.seq.load(Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let t = self.cycle_ns.load(Relaxed);
            let p = self.period.load(Relaxed) as f64;
            let held = (ring.len() / 2) as f64;
            fence(Acquire);
            if self.seq.load(Relaxed) == s1 {
                if s1 == 0 {
                    return held; // the engine has not run yet
                }
                let played = (self.ns(now).saturating_sub(t) as f64 * sample_rate as f64 / 1e9).min(p);
                return held + p - played;
            }
        }
        (ring.len() / 2) as f64
    }
}

#[inline(always)]
fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

#[derive(Debug, Default, Clone, Copy)]
struct InRt {
    playing: bool,
    strip: usize,
    flush_seen: u32,
}

/// The engine's private VBAN state.
#[derive(Debug, Default)]
pub struct VbanRt {
    ins: [InRt; NUM_IN],
}

impl VbanRt {
    /// Honour flush requests (also on cycles that mix nothing).
    #[inline]
    fn flushes(&mut self, tap: &VbanTap) {
        for (slot, st) in tap.ins.iter().zip(self.ins.iter_mut()) {
            let req = slot.flush_req.load(Acquire);
            if req != st.flush_seen {
                if let Some(ring) = slot.ring.get() {
                    ring.consume(ring.available());
                }
                st.flush_seen = req;
                st.playing = false;
                slot.playing.store(false, Relaxed);
                slot.flush_ack.store(req, Release);
            }
        }
    }

    /// A cycle that was silenced without mixing.
    pub fn idle(&mut self, tap: &VbanTap) {
        self.flushes(tap);
        tap.cycles.fetch_add(1, Relaxed);
    }

    /// Add the incoming streams to the strip inputs. `scratch` is the
    /// engine's input copy, `[(strip * 2 + ch) * MAX_FRAMES + frame]`;
    /// `now` is the time of this cycle.
    pub fn pull(&mut self, tap: &VbanTap, n: usize, scratch: &mut [f32], now: Instant) {
        let s = tap.seq.load(Relaxed);
        tap.seq.store(s.wrapping_add(1), Relaxed);
        fence(Release);
        tap.cycle_ns.store(tap.ns(now), Relaxed);
        tap.period.store(n as u32, Relaxed);
        self.take(tap, n, scratch);
        tap.seq.store(s.wrapping_add(2), Release);
    }

    fn take(&mut self, tap: &VbanTap, n: usize, scratch: &mut [f32]) {
        self.flushes(tap);
        if n == 0 || n > MAX_FRAMES || scratch.len() < NUM_STRIPS * 2 * MAX_FRAMES {
            return;
        }
        for (slot, st) in tap.ins.iter().zip(self.ins.iter_mut()) {
            let Some(ring) = slot.ring.get() else { continue };
            let target = slot.strip.load(Relaxed) as usize;
            let stopping = target >= NUM_STRIPS;
            let avail = ring.available() / 2;
            let fade_in = !st.playing;
            if !st.playing {
                let need = slot.resume.load(Relaxed) as usize;
                if stopping || need == 0 || avail < need.max(n) {
                    continue;
                }
                st.playing = true;
                st.strip = target;
                slot.playing.store(true, Relaxed);
            }
            let take = avail.min(n);
            let under = take < n;
            let (from, to) = (st.strip, if stopping { st.strip } else { target });
            for i in 0..take {
                let mut w = 1.0;
                if fade_in {
                    w *= ramp(0.0, 1.0, i, take);
                }
                if under || stopping {
                    w *= 1.0 - (i + 1) as f32 / take as f32;
                }
                let (l, r) = (ring.read(2 * i) * w, ring.read(2 * i + 1) * w);
                if from == to {
                    scratch[(from * 2) * MAX_FRAMES + i] += l;
                    scratch[(from * 2 + 1) * MAX_FRAMES + i] += r;
                } else {
                    let x = ramp(0.0, 1.0, i, take);
                    scratch[(from * 2) * MAX_FRAMES + i] += l * (1.0 - x);
                    scratch[(from * 2 + 1) * MAX_FRAMES + i] += r * (1.0 - x);
                    scratch[(to * 2) * MAX_FRAMES + i] += l * x;
                    scratch[(to * 2 + 1) * MAX_FRAMES + i] += r * x;
                }
            }
            ring.consume(2 * take);
            st.strip = to;
            if under || stopping {
                st.playing = false;
                slot.playing.store(false, Relaxed);
                if under && !stopping {
                    slot.underruns.fetch_add(1, Relaxed);
                }
            }
        }
    }

    /// Hand this cycle's sources to the outgoing streams. `strips` is the
    /// engine scratch with the post-fader strips (valid where `strip_live`),
    /// `buses` the finished bus outputs.
    pub fn push(&mut self, tap: &VbanTap, n: usize, strips: &[f32], strip_live: &[bool; NUM_STRIPS], buses: &[[&mut [f32]; 2]; NUM_BUSES]) {
        tap.cycles.fetch_add(1, Relaxed);
        if n == 0 || n > MAX_FRAMES || strips.len() < NUM_STRIPS * 2 * MAX_FRAMES || buses.iter().any(|b| b[0].len() < n || b[1].len() < n) {
            return;
        }
        for slot in &tap.outs {
            let src = slot.source.load(Relaxed) as usize;
            if src >= NUM_SOURCES {
                continue;
            }
            let Some(ring) = slot.ring.get() else { continue };
            if ring.free() < 2 * n {
                slot.dropped.fetch_add(n as u32, Relaxed);
                continue;
            }
            if src < NUM_STRIPS {
                if strip_live[src] {
                    let (l, r) = (&strips[src * 2 * MAX_FRAMES..][..n], &strips[(src * 2 + 1) * MAX_FRAMES..][..n]);
                    for i in 0..n {
                        ring.write(2 * i, l[i]);
                        ring.write(2 * i + 1, r[i]);
                    }
                } else {
                    for i in 0..2 * n {
                        ring.write(i, 0.0);
                    }
                }
            } else {
                let [l, r] = &buses[src - NUM_STRIPS];
                for i in 0..n {
                    ring.write(2 * i, l[i]);
                    ring.write(2 * i + 1, r[i]);
                }
            }
            ring.publish(2 * n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> Vec<f32> {
        vec![0.0; NUM_STRIPS * 2 * MAX_FRAMES]
    }

    fn feed(ring: &Ring, frames: usize, v: f32) {
        let s: Vec<f32> = (0..frames).flat_map(|_| [v, -v]).collect();
        assert!(ring.push(&s));
    }

    #[test]
    fn waits_for_the_resume_level_then_plays_and_fades() {
        let tap = VbanTap::new();
        let mut rt = VbanRt::default();
        let slot = &tap.ins[2];
        let ring = slot.ring();
        slot.strip.store(3, Relaxed);
        slot.resume.store(300, Relaxed);
        feed(ring, 200, 0.5);
        let mut sc = scratch();
        rt.pull(&tap, 128, &mut sc, Instant::now());
        assert!(sc.iter().all(|&v| v == 0.0), "below the resume level");
        feed(ring, 200, 0.5);
        rt.pull(&tap, 128, &mut sc, Instant::now());
        let l = &sc[6 * MAX_FRAMES..][..128];
        assert!(l[0] > 0.0 && l[0] < 0.01 && l[127] == 0.5, "fades in: {} {}", l[0], l[127]);
        assert!(l.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(sc[7 * MAX_FRAMES + 127], -0.5);
        assert!(slot.playing.load(Relaxed));
        let mut sc = scratch();
        rt.pull(&tap, 128, &mut sc, Instant::now());
        assert!(sc[6 * MAX_FRAMES..][..128].iter().all(|&v| v == 0.5), "steady");
        // 400 - 256 = 144 left: one more full cycle, then 16 frames that fade out
        let mut sc = scratch();
        rt.pull(&tap, 128, &mut sc, Instant::now());
        let mut sc = scratch();
        rt.pull(&tap, 128, &mut sc, Instant::now());
        let l = &sc[6 * MAX_FRAMES..][..128];
        assert!(l[0] < 0.5 && l[15] == 0.0 && l[16..].iter().all(|&v| v == 0.0), "underrun fades out");
        assert_eq!(slot.underruns.load(Relaxed), 1);
        assert!(!slot.playing.load(Relaxed));
        assert!(ring.is_empty());
    }

    #[test]
    fn adds_to_what_the_strip_already_has_and_crossfades_on_a_move() {
        let tap = VbanTap::new();
        let mut rt = VbanRt::default();
        let slot = &tap.ins[0];
        slot.strip.store(0, Relaxed);
        slot.resume.store(1, Relaxed);
        feed(slot.ring(), 1000, 0.25);
        let mut sc = scratch();
        rt.pull(&tap, 64, &mut sc, Instant::now()); // fade in
        let mut sc = scratch();
        sc[..64].fill(0.5);
        rt.pull(&tap, 64, &mut sc, Instant::now());
        assert!(sc[..64].iter().all(|&v| v == 0.75), "mixed with the strip's own input");
        slot.strip.store(5, Relaxed);
        let mut sc = scratch();
        rt.pull(&tap, 64, &mut sc, Instant::now());
        let (a, b) = (&sc[..64], &sc[10 * MAX_FRAMES..][..64]);
        for i in 0..64 {
            assert!((a[i] + b[i] - 0.25).abs() < 1e-6, "constant sum while moving");
        }
        assert_eq!((a[63], b[63]), (0.0, 0.25));
        let mut sc = scratch();
        rt.pull(&tap, 64, &mut sc, Instant::now());
        assert!(sc[..64].iter().all(|&v| v == 0.0) && sc[10 * MAX_FRAMES..][..64].iter().all(|&v| v == 0.25));
        // off: fades out on strip 6, no underrun counted
        slot.strip.store(NONE, Relaxed);
        let mut sc = scratch();
        rt.pull(&tap, 64, &mut sc, Instant::now());
        let s6 = &sc[10 * MAX_FRAMES..][..64];
        assert!(s6[0] > 0.24 && s6[63] == 0.0);
        assert_eq!(slot.underruns.load(Relaxed), 0);
        assert!(!slot.playing.load(Relaxed));
    }

    #[test]
    fn flush_handshake() {
        let tap = VbanTap::new();
        let mut rt = VbanRt::default();
        let slot = &tap.ins[7];
        feed(slot.ring(), 500, 0.1);
        slot.flush_req.store(5, Release);
        rt.idle(&tap);
        assert!(slot.ring().is_empty());
        assert_eq!(slot.flush_ack.load(Acquire), 5);
        feed(slot.ring(), 500, 0.1);
        rt.idle(&tap);
        assert_eq!(slot.ring().available(), 1000, "only once per request");
    }

    #[test]
    fn outgoing_takes_strips_post_fader_and_buses() {
        let tap = VbanTap::new();
        let mut rt = VbanRt::default();
        tap.outs[0].ring();
        tap.outs[1].ring();
        tap.outs[2].ring();
        tap.outs[0].source.store(1, Relaxed); // strip 2
        tap.outs[1].source.store(9, Relaxed); // B1
        tap.outs[2].source.store(4, Relaxed); // strip 5, silent this cycle
        let mut sc = scratch();
        sc[2 * MAX_FRAMES..][..32].fill(0.3);
        sc[3 * MAX_FRAMES..][..32].fill(-0.3);
        sc[8 * MAX_FRAMES..][..32].fill(9.0); // raw input of a skipped strip
        let mut store = vec![vec![0.0f32; 32]; NUM_BUSES * 2];
        store[6].fill(0.7);
        store[7].fill(-0.7);
        let mut it = store.iter_mut();
        let buses: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| [it.next().unwrap().as_mut_slice(), it.next().unwrap().as_mut_slice()]);
        let mut live = [true; NUM_STRIPS];
        live[4] = false;
        rt.push(&tap, 32, &sc, &live, &buses);
        let r0 = tap.outs[0].ring.get().unwrap();
        assert_eq!((r0.available(), r0.read(0), r0.read(1)), (64, 0.3, -0.3));
        let r1 = tap.outs[1].ring.get().unwrap();
        assert_eq!((r1.read(62), r1.read(63)), (0.7, -0.7));
        let r2 = tap.outs[2].ring.get().unwrap();
        assert!((0..64).all(|i| r2.read(i) == 0.0), "a silent strip sends silence");
        assert_eq!(tap.cycles.load(Relaxed), 1);
        // a full ring drops the cycle and counts it
        while r0.free() >= 64 {
            rt.push(&tap, 32, &sc, &live, &buses);
        }
        rt.push(&tap, 32, &sc, &live, &buses);
        assert_eq!(tap.outs[0].dropped.load(Relaxed), 32);
    }
}
