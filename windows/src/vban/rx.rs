//! Receiving one VBAN stream (network thread): check the frame counter,
//! decode, resample to the mixer's rate and keep the buffer in the
//! engine's ring at the right level.
//!
//! **Buffer level.** The ring is filled in bursts (packets, often several
//! at once: Voicemeeter sends a whole audio buffer of 512-1024 samples in
//! 2-4 packets) and drained one engine period at a time. What matters is
//! its *low-water mark*: how much audio is left to play just before a
//! packet arrives, minimum over the last 2 s. It is measured continuously,
//! counting the part of the current engine period not yet played (from the
//! time stamp of the engine's last cycle), so it does not jump by a period
//! whenever the packet and engine timing slide past each other. The engine
//! needs a full period at the start of each cycle, so the receiver steers
//! the low-water mark to one period plus the configured reserve
//! (`buffer_ms`) plus whatever extra recent dropouts called for.
//!
//! **Clock drift.** Sender and mixer run on different clocks, so their
//! "48 kHz" differ by tens to hundreds of ppm. The resampling ratio is
//! corrected by two terms, so that neither ever drops or repeats samples:
//!
//! * the *drift* itself, measured as the trend of the buffer level over the
//!   last 8 s with the corrections already applied taken out (a
//!   frequency-locked loop: a least-squares slope, so packet timing jitter
//!   averages out), up to +-3000 ppm;
//! * a *level* term that moves the buffer towards its target, proportional
//!   to the error and never more than +-500 ppm (under a cent: inaudible).
//!
//! The total changes by at most 500 ppm per second, so the pitch glides.
//!
//! The level that is steered is the time-average of the buffer (smooth);
//! its target is the low-water target plus how far the buffer dips below
//! its average (that depth rises at once when the network gets worse and
//! relaxes by 0.1 ms per second when it gets better), so in the end it is
//! the low-water mark that sits at its target.
//!
//! **(Re)start.** Playback starts once the ring holds the reserve plus the
//! largest burst seen in the packet timing plus one engine period. If the
//! ring runs dry anyway (network hiccup), the engine fades out, the extra
//! reserve grows by 5 ms (up to 200 ms; it shrinks again by 1 ms every 30 s
//! without a dropout), and playback resumes at the new level.
//!
//! **Packet loss.** A gap of up to 64 packets in the frame counter is
//! filled with silence of the same length, so the timing stays right; a
//! late or duplicate packet is dropped; a larger jump means the sender
//! restarted.

use super::proto::{self, AudioHeader, SampleFormat};
use super::resample::{MAX_DOWN, Resampler};
use super::tap::{InSlot, VbanTap};
use std::net::SocketAddrV4;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::time::{Duration, Instant};

const BUCKET: Duration = Duration::from_millis(100);
const BUCKETS: usize = 20;
/// No packet for this long: the sender paused or stopped.
pub const IDLE: Duration = Duration::from_millis(300);
const MAX_GAP: i32 = 64;
/// Level term: ppm per second of level error, and its limit.
const KP: f64 = 0.19;
const LEVEL_MAX: f64 = 0.0005;
/// Drift + level, at most (0.3 %: 5 cents, only if the clocks are that far apart).
pub const MAX_CORRECTION: f64 = 0.003;
/// The average level follows the buffer with this time constant.
const AVG_S: f64 = 1.0;
/// Drift is measured over this much history (100 ms steps), from 3 s on.
const FLL_STEPS: usize = 80;
const FLL_MIN_STEPS: usize = 30;
/// The dip depth rises within ~0.5 s (0.2 of the way per 100 ms step) and
/// relaxes by at most 0.1 ms per second (so following it down takes no more
/// than a 100 ppm correction).
const SPREAD_RISE: f64 = 0.2;
const SPREAD_RELAX_S_PER_S: f64 = 0.000_1;
/// The correction changes by at most this much per 100 ms step (so the
/// pitch never jumps, it glides: 500 ppm per second at most).
const SLEW: f64 = 0.000_05;
const EXTRA_STEP_MS: f64 = 5.0;
const EXTRA_MAX_MS: f64 = 200.0;
const EXTRA_DECAY: Duration = Duration::from_secs(30);

/// Minimum and maximum of samples over the last 2 s, in 100 ms buckets.
#[derive(Debug, Clone)]
pub(crate) struct Window {
    lo: [f64; BUCKETS],
    hi: [f64; BUCKETS],
    filled: usize,
    idx: usize,
    cur_lo: f64,
    cur_hi: f64,
    start: Option<Instant>,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            lo: [f64::INFINITY; BUCKETS],
            hi: [f64::NEG_INFINITY; BUCKETS],
            filled: 0,
            idx: 0,
            cur_lo: f64::INFINITY,
            cur_hi: f64::NEG_INFINITY,
            start: None,
        }
    }
}

impl Window {
    pub fn clear(&mut self) {
        *self = Window::default();
    }

    /// Add a sample pair; returns how many buckets were completed.
    pub fn add(&mut self, now: Instant, lo: f64, hi: f64) -> u32 {
        let mut done = 0;
        match self.start {
            None => self.start = Some(now),
            Some(s) if now.saturating_duration_since(s) >= BUCKET * BUCKETS as u32 => {
                // a long gap: what we had is stale
                self.clear();
                self.start = Some(now);
            }
            Some(mut s) => {
                while now.saturating_duration_since(s) >= BUCKET {
                    self.lo[self.idx] = self.cur_lo;
                    self.hi[self.idx] = self.cur_hi;
                    self.idx = (self.idx + 1) % BUCKETS;
                    self.filled = (self.filled + 1).min(BUCKETS);
                    self.cur_lo = f64::INFINITY;
                    self.cur_hi = f64::NEG_INFINITY;
                    s += BUCKET;
                    done += 1;
                }
                self.start = Some(s);
            }
        }
        self.cur_lo = self.cur_lo.min(lo);
        self.cur_hi = self.cur_hi.max(hi);
        done
    }

    pub fn full(&self) -> bool {
        self.filled == BUCKETS
    }

    pub fn min(&self) -> Option<f64> {
        let m = self.lo[..self.filled].iter().copied().fold(self.cur_lo, f64::min);
        m.is_finite().then_some(m)
    }

    pub fn max(&self) -> Option<f64> {
        let m = self.hi[..self.filled].iter().copied().fold(self.cur_hi, f64::max);
        m.is_finite().then_some(m)
    }
}

/// Drift and level control (see the module notes).
#[derive(Debug, Clone, Default)]
struct Control {
    /// Time-average of the buffer level (frames).
    avg: Option<f64>,
    /// Time and level right after the previous push.
    prev: Option<(Instant, f64)>,
    /// Integral of the level since the last step (frames x s), and its length.
    acc: f64,
    acc_t: f64,
    /// How far the level dips below its average (frames).
    spread: Option<f64>,
    /// `(seconds, level + rate x integral of the correction)`, every 100 ms:
    /// the level as it would have been without any correction.
    hist: std::collections::VecDeque<(f64, f64)>,
    t0: Option<Instant>,
    /// Integral of the correction applied so far (seconds).
    applied: f64,
    /// Clock drift of the sender against the mixer (+ = sender faster).
    drift: f64,
    /// What the resampler applies: drift + level term.
    corr: f64,
}

impl Control {
    /// After a jump in the level (flush, dropout, pause): measure afresh,
    /// but keep the drift (same sender, same clock).
    fn relevel(&mut self) {
        self.avg = None;
        self.prev = None;
        self.acc = 0.0;
        self.acc_t = 0.0;
        self.hist.clear();
        self.t0 = None;
        self.applied = 0.0;
        self.corr = self.drift;
    }

    /// A push: the level was `before` just before it and `after` after it.
    fn sample(&mut self, now: Instant, before: f64, after: f64) {
        match (self.avg, self.prev) {
            (Some(avg), Some((t, prev_after))) => {
                // the level fell linearly from prev_after to before
                let dt = now.saturating_duration_since(t).as_secs_f64();
                let mean = (prev_after + before) / 2.0;
                let w = 1.0 - (-dt / AVG_S).exp();
                self.avg = Some(avg + (mean - avg) * w);
                self.acc += mean * dt;
                self.acc_t += dt;
            }
            _ => self.avg = Some((before + after) / 2.0),
        }
        self.prev = Some((now, after));
    }

    /// Every 100 ms while playing (`dt` = time since the last step): update
    /// the drift estimate and the correction. `low` = low-water mark of the
    /// last 2 s, `target` = where it should be (frames).
    fn step(&mut self, now: Instant, dt: f64, low: f64, target: f64, fs: f64) {
        let Some(avg) = self.avg else { return };
        // the exact mean level over this step (no smoothing lag), and the
        // correction applied up to the step's middle
        let mean = if self.acc_t > 0.0 { self.acc / self.acc_t } else { avg };
        let span = if self.acc_t > 0.0 { self.acc_t } else { dt };
        (self.acc, self.acc_t) = (0.0, 0.0);
        let applied_mid = self.applied + self.corr * span / 2.0;
        self.applied += self.corr * dt;
        let dip = (avg - low).max(0.0);
        let spread = match self.spread {
            Some(s) if dip > s => s + (dip - s) * SPREAD_RISE,
            Some(s) => s - (s - dip).min(SPREAD_RELAX_S_PER_S * fs * dt),
            None => dip,
        };
        self.spread = Some(spread);
        let t0 = *self.t0.get_or_insert(now);
        let t = now.saturating_duration_since(t0).as_secs_f64() - span / 2.0;
        self.hist.push_back((t, mean + fs * applied_mid));
        while self.hist.len() > FLL_STEPS {
            self.hist.pop_front();
        }
        if self.hist.len() >= FLL_MIN_STEPS {
            let n = self.hist.len() as f64;
            let (mt, mu) = self.hist.iter().fold((0.0, 0.0), |(a, b), (t, u)| (a + t / n, b + u / n));
            let (sxy, sxx) = self.hist.iter().fold((0.0, 0.0), |(a, b), (t, u)| (a + (t - mt) * (u - mu), b + (t - mt) * (t - mt)));
            if sxx > 0.0 {
                self.drift = (sxy / sxx / fs).clamp(-MAX_CORRECTION, MAX_CORRECTION);
            }
        }
        let e = (avg - (target + spread)) / fs;
        let level = (KP * e).clamp(-LEVEL_MAX, LEVEL_MAX);
        let want = (self.drift + level).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let slew = SLEW * (dt / 0.1).max(1.0);
        self.corr += (want - self.corr).clamp(-slew, slew);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct RxStats {
    pub packets: u64,
    /// Packets missing from the frame counter (filled with silence).
    pub lost: u64,
    /// Late or duplicate packets (dropped).
    pub late: u64,
    /// Frames that did not fit the ring (should never happen).
    pub overflow: u64,
    /// The sender's frame counter jumped: it restarted.
    pub restarts: u32,
    /// Far too much was buffered (e.g. after a network stall): emptied.
    pub resyncs: u32,
}

/// Receiver state for one incoming stream.
pub struct RxStream {
    pub sample_rate: u32,
    pub channels: usize,
    pub format: Option<SampleFormat>,
    pub from: Option<SocketAddrV4>,
    expect: Option<u32>,
    rs: Option<Resampler>,
    ctl: Control,
    fill: Window,
    burst: Window,
    out_total: f64,
    t0: Option<Instant>,
    pub extra_ms: f64,
    seen_underruns: u32,
    calm_since: Option<Instant>,
    pub last_packet: Option<Instant>,
    flush_pending: bool,
    playing: bool,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    pub stats: RxStats,
}

impl Default for RxStream {
    fn default() -> Self {
        RxStream {
            sample_rate: 0,
            channels: 0,
            format: None,
            from: None,
            expect: None,
            rs: None,
            ctl: Control::default(),
            fill: Window::default(),
            burst: Window::default(),
            out_total: 0.0,
            t0: None,
            extra_ms: 0.0,
            seen_underruns: 0,
            calm_since: None,
            last_packet: None,
            flush_pending: false,
            playing: false,
            in_buf: Vec::with_capacity(2 * 1024),
            out_buf: Vec::with_capacity(2 * 4096),
            stats: RxStats::default(),
        }
    }
}

impl RxStream {
    /// Ask the engine to empty the ring (nothing is pushed until it has).
    fn flush(&mut self, slot: &InSlot) {
        slot.flush_req.fetch_add(1, Release);
        self.flush_pending = true;
        self.fill.clear();
        self.burst.clear();
        self.t0 = None;
        self.out_total = 0.0;
        self.ctl.relevel();
    }

    /// The resampling correction in use (+ = the input is consumed faster).
    pub fn correction(&self) -> f64 {
        self.ctl.corr
    }

    /// The sender's measured clock drift against the mixer.
    pub fn drift(&self) -> f64 {
        self.ctl.drift
    }

    /// Forget everything about the stream (new settings, new sender).
    pub fn restart(&mut self, slot: &InSlot) {
        self.flush(slot);
        let stats = self.stats;
        *self = RxStream { stats, flush_pending: true, ..RxStream::default() };
        self.seen_underruns = slot.underruns.load(Relaxed);
    }

    /// The stream was switched off.
    pub fn stop(&mut self, slot: &InSlot) {
        self.restart(slot);
        self.stats = RxStats::default();
    }

    /// Is audio arriving?
    pub fn receiving(&self, now: Instant) -> bool {
        self.last_packet.is_some_and(|t| now.saturating_duration_since(t) < IDLE)
    }

    /// Average audio held in the ring (and the resampler), ms.
    pub fn latency_ms(&self, fs_out: u32) -> Option<f64> {
        let avg = self.ctl.avg?;
        let rs = self.rs.as_ref().map_or(0.0, |r| r.delay() / r.nominal_step());
        Some((avg + rs) * 1000.0 / fs_out.max(1) as f64)
    }

    /// Handle one audio packet of incoming stream `k`. `Err` if the stream
    /// cannot be played (status text).
    #[allow(clippy::too_many_arguments)]
    pub fn packet(
        &mut self,
        tap: &VbanTap,
        k: usize,
        h: &AudioHeader,
        data: &[u8],
        from: SocketAddrV4,
        now: Instant,
        fs_out: u32,
        reserve_ms: f64,
    ) -> Result<(), String> {
        // Another sender with the same stream name while this one plays (two
        // computers both sending "Stream1", say) is not mixed in: their frame
        // counters interleaved made nearly every packet a restart, and the
        // stream went silent. It is listed among the streams seen instead.
        if self.receiving(now) && self.from.is_some_and(|f| f != from) {
            return Ok(());
        }
        let slot = &tap.ins[k];
        let ring = slot.ring();
        let period = match tap.period.load(Relaxed) {
            0 => 1024,
            p => p as usize,
        };
        let sr = h.sample_rate();
        if sr as f64 > MAX_DOWN * fs_out as f64 {
            return Err(format!("the stream is {sr} Hz; BananaSplit takes at most {} Hz here", (MAX_DOWN * fs_out as f64) as u32));
        }
        if self.rs.as_ref().is_none_or(|r| r.rates() != (sr, fs_out)) {
            if self.rs.is_some() {
                self.stats.restarts += 1;
            }
            self.restart(slot);
            self.rs = Some(Resampler::new(sr, fs_out));
            self.sample_rate = sr;
        }
        self.channels = h.channels;
        self.format = Some(h.format);
        self.from = Some(from);
        self.stats.packets += 1;

        let idle = !self.receiving(now);
        self.last_packet = Some(now);
        let mut gap = 0usize;
        if let (Some(e), false) = (self.expect, idle) {
            let d = h.frame.wrapping_sub(e) as i32;
            if (1..=MAX_GAP).contains(&d) {
                self.stats.lost += d as u64;
                gap = d as usize * h.samples;
            } else if (-MAX_GAP..0).contains(&d) {
                self.stats.late += 1;
                return Ok(());
            } else if d != 0 {
                self.stats.restarts += 1;
                self.flush(slot);
                if let Some(r) = self.rs.as_mut() {
                    r.reset();
                }
            }
        }
        self.expect = Some(h.frame.wrapping_add(1));
        if idle {
            // After a pause: start again from silence, but keep the clock
            // estimate (same sender, same clock). A dropout caused by the
            // pause is not the network's fault.
            if let Some(r) = self.rs.as_mut() {
                r.reset();
            }
            self.fill.clear();
            self.burst.clear();
            self.t0 = None;
            self.out_total = 0.0;
            self.ctl.relevel();
            self.seen_underruns = slot.underruns.load(Relaxed);
        }

        self.in_buf.clear();
        self.in_buf.resize(2 * gap, 0.0);
        proto::decode_stereo(data, h.channels, h.format, &mut self.in_buf);
        self.out_buf.clear();
        let rs = self.rs.as_mut().expect("created above");
        let step = rs.nominal_step() * (1.0 + self.ctl.corr);
        rs.process(&self.in_buf, step, &mut self.out_buf);

        if self.flush_pending {
            if slot.flush_ack.load(Acquire) != slot.flush_req.load(Relaxed) {
                return Ok(()); // the engine has not emptied the ring yet
            }
            self.flush_pending = false;
        }
        let fs = fs_out as f64;
        let produced = (self.out_buf.len() / 2) as f64;
        let before = tap.fill(ring, now, fs_out);
        let t0 = *self.t0.get_or_insert(now);
        let d_before = self.out_total - fs * now.saturating_duration_since(t0).as_secs_f64();
        let after = if ring.push(&self.out_buf) {
            self.out_total += produced;
            before + produced
        } else {
            self.stats.overflow += produced as u64;
            before
        };
        // After lost packets the level is low because a packet never came,
        // not because one came late: that is not timing to plan for (the
        // silence that stands in for it is pushed now). Dropouts it causes
        // still raise the reserve.
        let rotated = if gap > 0 { self.fill.add(now, after, after) } else { self.fill.add(now, before, after) };
        self.burst.add(now, d_before, d_before + produced);
        self.ctl.sample(now, before, after);

        // Dropouts while packets keep coming: keep more in reserve.
        let u = slot.underruns.load(Relaxed);
        if u != self.seen_underruns {
            self.seen_underruns = u;
            self.extra_ms = (self.extra_ms + EXTRA_STEP_MS).min(EXTRA_MAX_MS);
            self.calm_since = Some(now);
            self.fill.clear();
            self.ctl.relevel();
        }
        let calm = *self.calm_since.get_or_insert(now);
        if self.extra_ms > 0.0 && now.saturating_duration_since(calm) >= EXTRA_DECAY {
            self.extra_ms = (self.extra_ms - 1.0).max(0.0);
            self.calm_since = Some(now);
        }

        // low-water target: a period for the engine to take, plus reserve
        let target = period as f64 + (reserve_ms + self.extra_ms) * fs / 1000.0;
        let ptp = match (self.burst.min(), self.burst.max()) {
            (Some(lo), Some(hi)) => (hi - lo).max(produced),
            _ => produced,
        };
        let cap = (ring.capacity() / 2) as f64 * 0.75;
        let resume = (target + ptp + fs / 1000.0).min(cap);
        slot.resume.store(resume as u32, Relaxed);

        let playing = slot.playing.load(Relaxed);
        if playing && !self.playing {
            // only count what was measured while playing
            self.fill.clear();
            self.ctl.relevel();
        }
        self.playing = playing;
        if rotated > 0 && playing && self.fill.full() {
            if let Some(low) = self.fill.min() {
                if low > target + (fs * 0.1).max(target) {
                    // Way too much buffered (a stall delivered a pile of
                    // packets at once): drop it rather than lag for a minute.
                    self.stats.resyncs += 1;
                    self.flush(slot);
                    return Ok(());
                }
                self.ctl.step(now, BUCKET.as_secs_f64() * rotated as f64, low, target, fs);
            }
        }
        Ok(())
    }
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{MAX_FRAMES, NUM_STRIPS};
    use crate::vban::proto::{SampleFormat, name_field};
    use crate::vban::tap::{VbanRt, VbanTap};

    /// Simulates a sender (its own clock, burst pattern, jitter, losses,
    /// stalls and pauses) and the engine (pulling one period at a time on
    /// the mixer's clock), as events in time order.
    struct Sim {
        tap: std::sync::Arc<VbanTap>,
        rt: VbanRt,
        rx: RxStream,
        base: Instant,
        fs_out: u32,
        period: usize,
        reserve_ms: f64,
        fs_in: u32,
        /// The sender's true clock error.
        drift: f64,
        nbs: usize,
        /// Packets sent at once.
        burst: usize,
        /// Random extra delay per burst, 0..jitter seconds.
        jitter: f64,
        drop_every: u64,
        /// Bursts due in [a, b) arrive at b (a network stall).
        stall: Option<(f64, f64)>,
        /// Bursts due in [a, b) are never sent (the sender paused).
        pause: Option<(f64, f64)>,
        frame: u32,
        sent: u64,
        next_arrival: Option<f64>,
        last_arrival: f64,
        rng: u64,
        cycles: u64,
        out: Vec<f32>,
        /// The correction in use, every engine cycle.
        corrs: Vec<f64>,
        /// Extra stalls: every `stall_every` s, a burst arrives `stall_s` late.
        stall_every: f64,
        stall_s: f64,
        sender: SocketAddrV4,
    }

    impl Sim {
        fn new(fs_in: u32, drift: f64, nbs: usize, burst: usize) -> Sim {
            let tap = VbanTap::new();
            tap.ins[0].strip.store(0, Relaxed);
            tap.ins[0].ring();
            Sim {
                tap,
                rt: VbanRt::default(),
                rx: RxStream::default(),
                base: Instant::now(),
                fs_out: 48_000,
                period: 128,
                reserve_ms: 5.0,
                fs_in,
                drift,
                nbs,
                burst,
                jitter: 0.0,
                drop_every: 0,
                stall: None,
                pause: None,
                frame: 0,
                sent: 0,
                next_arrival: None,
                last_arrival: 0.0,
                rng: 0x9E37_79B9_7F4A_7C15,
                cycles: 0,
                out: Vec::new(),
                corrs: Vec::new(),
                stall_every: 0.0,
                stall_s: 0.0,
                sender: "192.168.1.20:6980".parse().unwrap(),
            }
        }

        fn now(&self) -> f64 {
            self.cycles as f64 * self.period as f64 / self.fs_out as f64
        }

        fn rand(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            (self.rng >> 11) as f64 / (1u64 << 53) as f64
        }

        /// A 440 Hz sine, by the sender's sample index.
        fn sample(&self, i: u64) -> f32 {
            (0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / self.fs_in as f64).sin()) as f32
        }

        fn send_packet(&mut self, now: Instant) {
            let mut p = vec![0u8; proto::HEADER_LEN];
            let h = AudioHeader {
                sr_index: proto::sr_index(self.fs_in).unwrap(),
                samples: self.nbs,
                channels: 2,
                format: SampleFormat::I16,
                name: name_field("Stream1"),
                frame: self.frame,
            };
            proto::write_header(&mut p, &h);
            let start = self.frame as u64 * self.nbs as u64;
            let frames: Vec<f32> = (0..self.nbs as u64).flat_map(|k| { let v = self.sample(start + k); [v, v] }).collect();
            let mut data = vec![0u8; frames.len() * 2];
            proto::encode(&frames, 2, SampleFormat::I16, &mut data);
            p.extend_from_slice(&data);
            self.frame = self.frame.wrapping_add(1);
            self.sent += 1;
            if self.drop_every > 0 && self.sent % self.drop_every == 0 {
                return; // lost on the way
            }
            let Ok(proto::Packet::Audio(h, d)) = proto::parse(&p) else { panic!() };
            let from = self.sender;
            self.rx.packet(&self.tap, 0, &h, d, from, now, self.fs_out, self.reserve_ms).unwrap();
        }

        /// Run for `secs` of mixer time.
        fn run(&mut self, secs: f64) {
            let mut sc = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
            let end_cycle = self.cycles + (secs * self.fs_out as f64 / self.period as f64) as u64;
            let burst_dt = (self.burst * self.nbs) as f64 / (self.fs_in as f64 * (1.0 + self.drift));
            while self.cycles < end_cycle {
                let arrival = match self.next_arrival {
                    Some(a) => a,
                    None => {
                        let due = (self.sent / self.burst as u64) as f64 * burst_dt;
                        if self.pause.is_some_and(|(a, b)| due >= a && due < b) {
                            self.sent += self.burst as u64;
                            self.frame = self.frame.wrapping_add(self.burst as u32);
                            continue;
                        }
                        let mut t = due + self.jitter * self.rand();
                        if self.stall_every > 0.0 && (due / self.stall_every).fract() < burst_dt / self.stall_every {
                            t += self.stall_s;
                        }
                        if let Some((a, b)) = self.stall {
                            if due >= a && due < b {
                                t = t.max(b);
                            }
                        }
                        let t = t.max(self.last_arrival);
                        self.next_arrival = Some(t);
                        t
                    }
                };
                if arrival <= self.now() {
                    let at = self.base + Duration::from_secs_f64(arrival);
                    for _ in 0..self.burst {
                        self.send_packet(at);
                    }
                    self.last_arrival = arrival;
                    self.next_arrival = None;
                } else {
                    sc[..self.period].fill(0.0);
                    let at = self.base + Duration::from_secs_f64(self.now());
                    self.rt.pull(&self.tap, self.period, &mut sc, at);
                    self.out.extend_from_slice(&sc[..self.period]);
                    self.corrs.push(self.rx.correction());
                    self.cycles += 1;
                }
            }
        }

        fn underruns(&self) -> u32 {
            self.tap.ins[0].underruns.load(Relaxed)
        }
    }

    #[test]
    fn window_min_max() {
        let t = Instant::now();
        let mut w = Window::default();
        assert_eq!(w.min(), None);
        assert_eq!(w.add(t, 5.0, 9.0), 0);
        assert_eq!((w.min(), w.max()), (Some(5.0), Some(9.0)));
        assert_eq!(w.add(t + Duration::from_millis(150), 7.0, 8.0), 1);
        assert_eq!((w.min(), w.max()), (Some(5.0), Some(9.0)));
        for k in 2..=21 {
            w.add(t + Duration::from_millis(100 * k + 50), 7.0, 8.0);
        }
        assert!(w.full());
        assert_eq!((w.min(), w.max()), (Some(7.0), Some(8.0)), "old samples age out");
        w.add(t + Duration::from_secs(10), 1.0, 2.0);
        assert!(!w.full(), "a long gap starts over");
        assert_eq!(w.min(), Some(1.0));
    }

    #[test]
    fn locks_onto_a_drifting_clock_without_dropouts() {
        for drift in [0.0, 150e-6, -400e-6, 2000e-6] {
            let mut s = Sim::new(48_000, drift, 256, 2); // Voicemeeter-like: 512-sample bursts
            s.run(5.0);
            assert_eq!(s.underruns(), 0, "start-up, drift {drift}");
            s.run(300.0);
            assert_eq!(s.underruns(), 0, "drift {drift}");
            // the correction found the clock difference, and got there smoothly
            let err = (s.rx.correction() - drift).abs();
            assert!(err < 2e-6, "drift {drift}: correction {} ppm", s.rx.correction() * 1e6);
            assert_smooth(&s, drift, 48_000 * 10 / 128);
            // and the buffer's low-water mark sits at one period (2.67 ms)
            // plus the 5 ms reserve
            let low = s.rx.fill.min().unwrap() * 1000.0 / 48_000.0;
            assert!((7.4..8.0).contains(&low), "drift {drift}: low-water {low} ms");
            let lat = s.rx.latency_ms(48_000).unwrap();
            assert!(lat < 20.0, "latency {lat} ms");
            assert_eq!(s.rx.stats.lost + s.rx.stats.late + s.rx.stats.overflow, 0);
        }
    }

    /// The pitch never jumps (the correction glides at most 500 ppm/s) and
    /// stays within the level term's 500 ppm of the true drift.
    fn assert_smooth(s: &Sim, drift: f64, from_cycle: usize) {
        let per_cycle = SLEW * 10.0 * s.period as f64 / s.fs_out as f64 + 1e-9;
        for w in s.corrs[from_cycle..].windows(2) {
            assert!((w[1] - w[0]).abs() <= per_cycle.max(SLEW + 1e-9), "jump {} ppm", (w[1] - w[0]) * 1e6);
        }
        let worst = s.corrs[from_cycle..].iter().map(|c| (c - drift).abs()).fold(0.0, f64::max);
        assert!(worst <= LEVEL_MAX + 100e-6, "correction {} ppm away from the drift", worst * 1e6);
    }

    #[test]
    fn voicemeeter_with_stalls() {
        // Voicemeeter at 44.1 kHz: 512-sample buffers as 4 packets of 128,
        // 3 ms of jitter, and every 15 s a burst that is 25 ms late
        let mut s = Sim::new(44_100, -120e-6, 128, 4);
        s.reserve_ms = 10.0;
        s.jitter = 0.003;
        s.stall_every = 15.0;
        s.stall_s = 0.025;
        s.run(40.0);
        let u = s.underruns();
        assert!(u <= 1, "{u} dropouts while learning the stalls");
        let from = s.corrs.len();
        s.run(240.0);
        assert_eq!(s.underruns(), u, "none once the buffer has adapted");
        assert_smooth(&s, -120e-6, from);
        assert!((s.rx.drift() + 120e-6).abs() < 30e-6, "drift estimate {} ppm", s.rx.drift() * 1e6);
        let lat = s.rx.latency_ms(48_000).unwrap();
        assert!(lat < 60.0, "latency {lat} ms");
    }

    #[test]
    fn converts_44100_and_plays_a_clean_tone() {
        let mut s = Sim::new(44_100, 50e-6, 128, 1);
        s.run(60.0);
        assert_eq!(s.underruns(), 0);
        // the last second: a clean 440 Hz sine at 48 kHz (frequency and level)
        let tail = &s.out[s.out.len() - 48_000..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.002, "peak {peak}");
        let crossings = tail.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((439..=441).contains(&crossings), "{crossings} Hz");
        // smooth: no sample-to-sample jump bigger than the sine's own slope
        let slope = 0.5 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
        let jump = s.out[48_000..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(jump < slope * 1.05, "jump {jump} vs {slope}");
    }

    #[test]
    fn jitter_and_loss() {
        // up to 5 ms of random packet jitter, one packet in 500 lost
        let mut s = Sim::new(48_000, 80e-6, 256, 1);
        s.jitter = 0.005;
        s.drop_every = 500;
        s.run(60.0);
        let u = s.underruns();
        assert!(u <= 2, "{u} dropouts while learning the jitter");
        let from = s.corrs.len();
        s.run(120.0);
        assert_eq!(s.underruns(), u, "none once the reserve has adapted (extra {} ms)", s.rx.extra_ms);
        assert_smooth(&s, 80e-6, from);
        assert!(s.rx.stats.lost >= 60, "losses are counted: {}", s.rx.stats.lost);
        // a lost packet is replaced by silence of the same length, so the
        // stream stays in time and the correction still finds the drift
        assert!((s.rx.correction() - 80e-6).abs() < 60e-6, "{} ppm", s.rx.correction() * 1e6);
        let lat = s.rx.latency_ms(48_000).unwrap();
        assert!(lat < 30.0, "latency {lat} ms");
    }

    #[test]
    fn dropouts_raise_the_reserve_and_pauses_do_not() {
        let mut s = Sim::new(48_000, 0.0, 256, 1);
        s.run(5.0);
        assert_eq!((s.underruns(), s.rx.extra_ms), (0, 0.0));
        // a 40 ms stall: the packets of 40 ms arrive late, all at once
        let t = s.now();
        s.stall = Some((t + 0.5, t + 0.54));
        s.run(5.0);
        assert!(s.underruns() >= 1, "the stall caused a dropout");
        assert_eq!(s.rx.extra_ms, 5.0, "and more reserve");
        assert!(s.tap.ins[0].playing.load(Relaxed), "playing again");
        // the sender pauses for a second: not the network's fault
        let t = s.now();
        s.pause = Some((t + 0.5, t + 1.5));
        let before = s.underruns();
        s.run(5.0);
        assert!(s.underruns() > before, "ran dry during the pause");
        assert_eq!(s.rx.extra_ms, 5.0, "pauses do not add reserve");
        assert!(s.tap.ins[0].playing.load(Relaxed), "playing again");
        assert_eq!(s.rx.stats.restarts, 0);
        // the extra reserve wears off again
        s.run(200.0);
        assert_eq!(s.rx.extra_ms, 0.0);
    }

    #[test]
    fn a_late_duplicate_is_dropped_and_a_restart_detected() {
        let mut s = Sim::new(48_000, 0.0, 128, 1);
        s.run(2.0);
        let late = s.frame.wrapping_sub(3);
        let saved = s.frame;
        s.frame = late;
        let now = s.base + Duration::from_secs_f64(s.now());
        s.send_packet(now);
        assert_eq!(s.rx.stats.late, 1);
        s.frame = saved.wrapping_add(5000); // the sender started over
        s.send_packet(now + Duration::from_millis(1));
        assert_eq!(s.rx.stats.restarts, 1);
        s.run(3.0);
        assert!(s.tap.ins[0].playing.load(Relaxed));
    }

    #[test]
    fn a_second_sender_with_the_same_stream_name_is_not_mixed_in() {
        // Two computers send "Stream1" to an input with `from = any`: the
        // one that started first plays on (the other used to make every
        // packet a restart: silence), and takes over when it stops.
        let mut s = Sim::new(48_000, 0.0, 256, 1);
        let (a, b): (SocketAddrV4, SocketAddrV4) = ("192.168.1.20:6980".parse().unwrap(), "192.168.1.30:6980".parse().unwrap());
        let mut sc = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
        let base = s.base;
        let (mut fa, mut fb) = (1000u32, 5u32);
        let mut played = 0usize;
        let cycles = 48_000 * 4 / 128;
        for c in 0..cycles {
            let t = c as f64 * 128.0 / 48_000.0;
            if c % 2 == 0 {
                (s.sender, s.frame) = (a, fa);
                s.send_packet(base + Duration::from_secs_f64(t));
                fa = s.frame;
                (s.sender, s.frame) = (b, fb);
                s.send_packet(base + Duration::from_secs_f64(t + 1e-4));
                fb = s.frame;
            }
            sc[..128].fill(0.0);
            s.rt.pull(&s.tap, 128, &mut sc, base + Duration::from_secs_f64(t));
            played += sc[..128].iter().filter(|v| v.abs() > 1e-6).count();
        }
        assert_eq!(s.rx.stats.restarts, 0);
        assert_eq!(s.rx.from, Some(a));
        assert!(played > 48_000 * 3, "plays: {played} of {} frames", 48_000 * 4);
        // A stops: after a pause, B is heard
        for c in cycles..cycles + 48_000 / 128 {
            let t = c as f64 * 128.0 / 48_000.0;
            if c % 2 == 0 {
                (s.sender, s.frame) = (b, fb);
                s.send_packet(base + Duration::from_secs_f64(t));
                fb = s.frame;
            }
            s.rt.pull(&s.tap, 128, &mut sc, base + Duration::from_secs_f64(t));
        }
        assert_eq!(s.rx.from, Some(b));
        assert!(s.tap.ins[0].playing.load(Relaxed));
    }
}
