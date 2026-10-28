//! Real-time mixing engine.
//!
//! Signal flow (identical topology to Voicemeeter Banana, but 6 input strips):
//!
//! ```text
//!  strip input (L,R) + VBAN in ─► sanitize ─► [mono] ─► EQ ─► gain × fader × pan, mute/solo ─► meter ─┬─► route A1 ─┐
//!                                                                                                    ├─► route A2  │
//!                                                                                                    ├─► ...       ├─► bus sum ─►
//!                                                                                                    └─► route B2 ─┘
//!  (+ player, on the buses it is routed to)
//!  (+ other buses routed here) ─► [mono] ─► EQ ─► gain × fader × balance, mute ─► meter ─┬─► out
//!                                                                                      └─► other buses (sends)
//! ```
//!
//! A bus can feed other buses ("sends", e.g. B1 into A1 to hear the stream
//! mix): after its fader, like a strip. Buses are finished in an order where
//! every bus comes after the buses feeding it, so a send adds no latency
//! either; a send that would close a loop is never made.
//!
//! The input gain ("trim", the GAIN knob) and the pan / balance are applied
//! together with the fader as one ramped gain per side. The EQ is linear,
//! so this is the same as applying the input gain before it. At 0 dB and
//! centre they are exactly 1.0, so the signal is unchanged bit for bit.
//!
//! Recorder and VBAN out take strips after their fader and buses as they
//! leave the mixer (see [`crate::recorder`], [`crate::vban`]).
//!
//! Guarantees of [`Engine::process`]:
//! * **Sample-synchronous.** Every strip and every bus is computed in the same
//!   call for the same frame range, so a sample entering any strip at frame
//!   `k` leaves every bus at frame `k` of the same cycle. The engine adds
//!   **zero** samples of latency; total latency is only the audio server's
//!   period(s).
//! * **Real-time safe.** No allocation, no locks, no syscalls, no panics for
//!   any input. Parameters are read from atomics once per cycle.
//! * **Click-free.** Every gain change (fader, mute, solo, routing toggle) is
//!   ramped linearly across one cycle and lands exactly on its target on the
//!   last sample of that cycle.
//! * **Recorder tap.** With a recorder attached ([`Engine::attach_recorder`]),
//!   the post-fader strips and the finished buses of the same cycle are
//!   handed to it at the end of `process`, still with no allocation, locks
//!   or syscalls (see [`crate::recorder`]).
//! * **EQ.** Every strip and bus has a 6-band parametric EQ ([`crate::eq`]);
//!   off (the default) or flat, it leaves the signal bit for bit unchanged.
//! * **VBAN.** With VBAN attached ([`Engine::attach_vban`]), incoming network
//!   streams are added to their strips' inputs at the start of `process`
//!   and outgoing streams are fed at the end, through preallocated
//!   lock-free rings (see [`crate::vban`]).
//! * **Player.** With a player attached ([`Engine::attach_player`]), a
//!   recording being played back is added to the buses it is routed to,
//!   before their EQ and fader (see [`crate::player`]).
//! * **Sample rate.** The audio server's rate is [`Shared::sample_rate`];
//!   when it changes, the engine re-tunes its EQs and tells the recorder,
//!   VBAN and the player through their taps.
//! * **Meters.** Every strip and bus has two: after its fader
//!   ([`StripParams::peak`]) and at its input ([`StripParams::input_peak`],
//!   after mono and the input gain), which keeps showing what comes in while
//!   the channel is muted.

use crate::eq::{EqParams, EqRt};
use crate::player::{PlayRt, PlayTap};
use crate::recorder::{RecRt, RecTap};
use crate::vban::{VbanRt, VbanTap};
use std::array;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering::Relaxed};

pub const NUM_STRIPS: usize = 6;
pub const NUM_BUSES: usize = 5;
/// A1..A3 start as hardware outputs and B1..B2 as virtual microphones for
/// other applications, but any bus can be either, or both (its `connect`
/// lines and its virtual cable decide).
pub const BUS_IDS: [&str; NUM_BUSES] = ["A1", "A2", "A3", "B1", "B2"];
/// A1..A3: the buses that are hardware outputs by default.
pub const NUM_PHYSICAL_BUSES: usize = 3;
/// Bit mask with every bus set.
pub const ALL_BUSES: u8 = (1 << NUM_BUSES) - 1;
/// Largest period the engine will process. JACK2 and PipeWire both cap at
/// 8192 by default; we allow twice that. Larger periods produce silence and
/// increment [`Shared::oversize_cycles`] instead of allocating.
pub const MAX_FRAMES: usize = 16384;
pub const MIN_GAIN_DB: f32 = -60.0;
pub const MAX_GAIN_DB: f32 = 12.0;
/// Range of the input gain (the GAIN knob), applied with the fader.
pub const MIN_TRIM_DB: f32 = -24.0;
pub const MAX_TRIM_DB: f32 = 24.0;

/// f32 stored in an `AtomicU32`.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    #[inline]
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

/// Peak meter written by the audio thread, read-and-reset by the UI.
///
/// For non-negative finite f32 values the IEEE-754 bit pattern is monotonic
/// when read as u32, so `fetch_max` on the bits is a lock-free float max.
#[derive(Debug, Default)]
pub struct PeakMeter(AtomicU32);

impl PeakMeter {
    #[inline]
    fn push(&self, v: f32) {
        // Samples are sanitized before they are metered, but a huge finite
        // one times a gain can still overflow to infinity: that reads as
        // the loudest finite value.
        let v = if v.is_nan() { 0.0 } else { v.abs().min(f32::MAX) };
        self.0.fetch_max(v.to_bits(), Relaxed);
    }
    /// Returns the peak (linear) since the last call and resets it.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Relaxed))
    }

    #[doc(hidden)]
    pub fn push_for_test(&self, v: f32) {
        self.push(v);
    }
}

#[derive(Debug, Default)]
pub struct StripParams {
    /// Fader, dB.
    pub gain_db: AtomicF32,
    /// Input gain (GAIN knob), dB.
    pub trim_db: AtomicF32,
    /// Pan / balance, -1 (left) .. 0 (centre) .. +1 (right).
    pub pan: AtomicF32,
    pub mute: AtomicBool,
    pub solo: AtomicBool,
    pub mono: AtomicBool,
    /// Bit `b` set => strip feeds bus `b` (A1 = bit 0 ... B2 = bit 4).
    pub routes: AtomicU8,
    /// Level after the fader (what the strip sends).
    pub peak: [PeakMeter; 2],
    /// Level at the input (after mono and the input gain, before EQ, pan
    /// and fader), also while the strip is muted or soloed out.
    pub input_peak: [PeakMeter; 2],
    pub eq: EqParams,
}

#[derive(Debug, Default)]
pub struct BusParams {
    /// Fader, dB.
    pub gain_db: AtomicF32,
    /// Gain before the fader (GAIN knob), dB.
    pub trim_db: AtomicF32,
    /// Balance, -1 (left) .. 0 (centre) .. +1 (right).
    pub pan: AtomicF32,
    pub mute: AtomicBool,
    pub mono: AtomicBool,
    /// Bit `d` set => this bus feeds bus `d` after its fader (its own bit
    /// is ignored, and so is a send that would close a loop).
    pub routes: AtomicU8,
    /// Level after the fader (what leaves the bus).
    pub peak: [PeakMeter; 2],
    /// Level of the bus sum (after mono and the input gain, before EQ,
    /// balance and fader), also while the bus is muted.
    pub input_peak: [PeakMeter; 2],
    pub eq: EqParams,
}

/// State shared between the audio thread and the control/UI thread.
#[derive(Debug, Default)]
pub struct Shared {
    pub strips: [StripParams; NUM_STRIPS],
    pub buses: [BusParams; NUM_BUSES],
    /// Incremented by the backend's xrun callback.
    pub xruns: AtomicU32,
    /// Cycles larger than MAX_FRAMES (output was silenced).
    pub oversize_cycles: AtomicU32,
    /// Channels linked to the recorder: bit 0..5 = strips 1..6,
    /// bit 6..10 = buses A1..B2 (see [`crate::recorder::source_id`]).
    pub rec_links: AtomicU16,
    /// The audio server's sample rate, 0 = not known yet. The backend sets
    /// it once the server runs the mixer, and again whenever it changes.
    pub sample_rate: AtomicU32,
    /// Cycles processed so far (wrapping): the backend waits for the first.
    pub cycles: AtomicU32,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// EQ of channel `ch` (0..5 strips, 6..10 buses).
    pub fn eq(&self, ch: usize) -> &EqParams {
        if ch < NUM_STRIPS { &self.strips[ch].eq } else { &self.buses[ch - NUM_STRIPS].eq }
    }

    pub fn toggle_route(&self, strip: usize, bus: usize) {
        self.strips[strip].routes.fetch_xor(1 << bus, Relaxed);
    }

    /// The bus-to-bus sends, as set (own bits cleared).
    pub fn bus_routes(&self) -> [u8; NUM_BUSES] {
        array::from_fn(|b| self.buses[b].routes.load(Relaxed) & ALL_BUSES & !(1 << b))
    }

    /// Would bus `from` feeding bus `to` close a loop (or is it the same bus)?
    pub fn send_loops(&self, from: usize, to: usize) -> bool {
        from == to || reach(&self.bus_routes(), to) & (1 << from) != 0
    }

    /// Link/unlink recorder source `src` (0..5 strips, 6..10 buses).
    pub fn toggle_rec_link(&self, src: usize) {
        if src < NUM_STRIPS + NUM_BUSES {
            self.rec_links.fetch_xor(1 << src, Relaxed);
        }
    }
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-6 { -120.0 } else { 20.0 * g.log10() }
}

#[inline]
pub fn clamp_db(db: f32) -> f32 {
    if db.is_nan() { 0.0 } else { db.clamp(MIN_GAIN_DB, MAX_GAIN_DB) }
}

#[inline]
pub fn clamp_trim(db: f32) -> f32 {
    if db.is_nan() { 0.0 } else { db.clamp(MIN_TRIM_DB, MAX_TRIM_DB) }
}

#[inline]
pub fn clamp_pan(p: f32) -> f32 {
    if p.is_nan() { 0.0 } else { p.clamp(-1.0, 1.0) }
}

/// (left, right) gain of the pan / balance control: the side it moves away
/// from follows a cosine (-3 dB half way, silent at the end), the other side
/// stays at unity. Centre is exactly (1, 1).
#[inline]
pub fn pan_gains(p: f32) -> (f32, f32) {
    let p = clamp_pan(p);
    let fade = |x: f32| if x >= 1.0 { 0.0 } else { (x * std::f32::consts::FRAC_PI_2).cos().clamp(0.0, 1.0) };
    if p > 0.0 {
        (fade(p), 1.0)
    } else if p < 0.0 {
        (1.0, fade(-p))
    } else {
        (1.0, 1.0)
    }
}

/// Target gain of one side: fader × input gain × pan, or 0 when silenced.
#[inline]
fn side_gains(silenced: bool, fader_db: f32, trim_db: f32, pan: f32) -> [f32; 2] {
    if silenced {
        return [0.0, 0.0];
    }
    let g = db_to_gain(clamp_db(fader_db)) * db_to_gain(clamp_trim(trim_db));
    let (l, r) = pan_gains(pan);
    [g * l, g * r]
}

/// Replace NaN/inf (from a misbehaving client) with silence so that one bad
/// application can never poison every bus.
#[inline(always)]
fn sanitize(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// Linear ramp from `v0` to `v1` across `n` samples; sample `n-1` is exactly `v1`.
#[inline(always)]
fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

/// The buses reachable from bus `from` through `sends` (itself included).
pub fn reach(sends: &[u8; NUM_BUSES], from: usize) -> u8 {
    let mut seen = 1u8 << from;
    let mut frontier = seen;
    while frontier != 0 {
        let mut next = 0u8;
        for (b, &s) in sends.iter().enumerate() {
            if frontier & (1 << b) != 0 {
                next |= s;
            }
        }
        next &= ALL_BUSES & !seen;
        seen |= next;
        frontier = next;
    }
    seen
}

/// This cycle's bus-to-bus sends, without loops: the sends still sounding
/// from the last cycle (`active`, which never loop), plus the `requested`
/// ones in bus order as long as they close no loop (the controls never set
/// one; two changes racing could). Returns those sends and an order in which
/// every bus comes after the buses feeding it.
fn bus_graph(requested: &[u8; NUM_BUSES], active: &[u8; NUM_BUSES]) -> ([u8; NUM_BUSES], [usize; NUM_BUSES]) {
    let mut g = *active;
    for b in 0..NUM_BUSES {
        g[b] &= ALL_BUSES & !(1 << b);
        for d in 0..NUM_BUSES {
            if requested[b] & (1 << d) != 0 && d != b && g[b] & (1 << d) == 0 && reach(&g, d) & (1 << b) == 0 {
                g[b] |= 1 << d;
            }
        }
    }
    let mut order = [0usize; NUM_BUSES];
    let mut done = 0u8;
    for slot in order.iter_mut() {
        // A bus whose feeders are all done (always one: `g` has no loop).
        let ready = (0..NUM_BUSES)
            .find(|&c| done & (1 << c) == 0 && (0..NUM_BUSES).all(|s| done & (1 << s) != 0 || g[s] & (1 << c) == 0));
        let next = ready.or_else(|| (0..NUM_BUSES).find(|&c| done & (1 << c) == 0)).unwrap_or(0);
        *slot = next;
        done |= 1 << next;
    }
    (g, order)
}

/// Peak of the left and right input of a strip as its fader sees it: after
/// mono (sanitized like the mix).
#[inline]
fn input_peaks(l: &[f32], r: &[f32], mono: bool) -> (f32, f32) {
    let (mut pl, mut pr) = (0.0f32, 0.0f32);
    if mono {
        for (&a, &b) in l.iter().zip(r) {
            pl = pl.max((0.5 * (sanitize(a) + sanitize(b))).abs());
        }
        return (pl, pl);
    }
    for (&a, &b) in l.iter().zip(r) {
        pl = pl.max(sanitize(a).abs());
        pr = pr.max(sanitize(b).abs());
    }
    (pl, pr)
}

/// Bus `a` to read and bus `b` to add to (`a != b`).
#[inline]
fn two_buses<'a, 'b>(
    out: &'a mut [[&'b mut [f32]; 2]; NUM_BUSES],
    a: usize,
    b: usize,
) -> (&'a [&'b mut [f32]; 2], &'a mut [&'b mut [f32]; 2]) {
    if a < b {
        let (x, y) = out.split_at_mut(b);
        (&x[a], &mut y[0])
    } else {
        let (x, y) = out.split_at_mut(a);
        (&y[0], &mut x[b])
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    /// Private copy of every input channel: `[strip][L/R][MAX_FRAMES]`.
    /// Copying first means input buffers can never alias output buffers
    /// (JACK may hand an input port the very buffer of the output it is
    /// connected to, e.g. if a user patches a bus back into a strip).
    scratch: Box<[f32]>,
    /// Per strip / bus: [left, right] gain of the last cycle.
    strip_gain: [[f32; 2]; NUM_STRIPS],
    route_gain: [[f32; NUM_BUSES]; NUM_STRIPS],
    bus_gain: [[f32; 2]; NUM_BUSES],
    /// Bus-to-bus sends of the last cycle: [from][to].
    send_gain: [[f32; NUM_BUSES]; NUM_BUSES],
    /// Strips 0..5, then buses.
    eq: [EqRt; NUM_STRIPS + NUM_BUSES],
    /// The rate the EQs are tuned for.
    rate: u32,
    /// The rate last told to the taps (0 = none yet).
    tap_rate: u32,
    rec: Option<Arc<RecTap>>,
    rec_rt: RecRt,
    vban: Option<Arc<VbanTap>>,
    vban_rt: VbanRt,
    play: Option<Arc<PlayTap>>,
    play_rt: PlayRt,
}

impl Engine {
    /// All gains start at 0 so the first cycle fades in instead of popping.
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            scratch: vec![0.0; NUM_STRIPS * 2 * MAX_FRAMES].into_boxed_slice(),
            strip_gain: [[0.0; 2]; NUM_STRIPS],
            route_gain: [[0.0; NUM_BUSES]; NUM_STRIPS],
            bus_gain: [[0.0; 2]; NUM_BUSES],
            send_gain: [[0.0; NUM_BUSES]; NUM_BUSES],
            eq: array::from_fn(|_| EqRt::new(48_000)),
            rate: 48_000,
            tap_rate: 0,
            rec: None,
            rec_rt: RecRt::default(),
            vban: None,
            vban_rt: VbanRt::default(),
            play: None,
            play_rt: PlayRt::default(),
        }
    }

    /// The server's sample rate (for the EQ). Call it before the engine
    /// goes live; later, set [`Shared::sample_rate`] instead (the engine
    /// follows it at the start of its next cycle).
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.rate = sample_rate;
        for eq in &mut self.eq {
            eq.set_sample_rate(sample_rate);
        }
    }

    /// Follow [`Shared::sample_rate`]: re-tune the EQs and tell the taps.
    /// No allocation, locks or system calls.
    #[inline]
    fn follow_rate(&mut self) {
        let sr = self.shared.sample_rate.load(Relaxed);
        if sr == 0 || sr == self.tap_rate {
            return;
        }
        if sr != self.rate {
            self.set_sample_rate(sr);
        }
        self.tap_rate = sr;
        if let Some(t) = self.rec.as_deref() {
            t.rate.store(sr, Relaxed);
        }
        if let Some(t) = self.vban.as_deref() {
            t.rate.store(sr, Relaxed);
        }
        if let Some(t) = self.play.as_deref() {
            t.rate.store(sr, Relaxed);
        }
    }

    /// Feed every cycle to a recorder (call before the engine goes live).
    pub fn attach_recorder(&mut self, tap: Arc<RecTap>) {
        self.rec = Some(tap);
    }

    /// Exchange audio with the VBAN threads (call before the engine goes live).
    pub fn attach_vban(&mut self, tap: Arc<VbanTap>) {
        self.vban = Some(tap);
    }

    /// Play recordings into the buses (call before the engine goes live).
    pub fn attach_player(&mut self, tap: Arc<PlayTap>) {
        self.play = Some(tap);
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// A cycle that produced silence without mixing: the recorder (if any)
    /// records `n` frames of silence for it; VBAN streams wait.
    #[inline]
    fn rec_skip(&mut self, n: usize) {
        if let Some(tap) = self.rec.as_deref() {
            self.rec_rt.skip(tap, self.shared.rec_links.load(Relaxed), n);
        }
        if let Some(tap) = self.vban.as_deref() {
            self.vban_rt.idle(tap);
        }
        if let Some(tap) = self.play.as_deref() {
            self.play_rt.idle(tap);
        }
    }

    #[inline]
    fn chan_mut(&mut self, strip: usize, ch: usize) -> &mut [f32] {
        let off = (strip * 2 + ch) * MAX_FRAMES;
        &mut self.scratch[off..off + MAX_FRAMES]
    }

    /// Copy one input channel for this cycle. Extra frames beyond
    /// MAX_FRAMES are ignored (that cycle will be silenced by `process`).
    #[inline]
    pub fn load_input(&mut self, strip: usize, ch: usize, src: &[f32]) {
        let n = src.len().min(MAX_FRAMES);
        self.chan_mut(strip, ch)[..n].copy_from_slice(&src[..n]);
    }

    #[inline]
    pub fn clear_input(&mut self, strip: usize, ch: usize, n: usize) {
        let n = n.min(MAX_FRAMES);
        self.chan_mut(strip, ch)[..n].fill(0.0);
    }

    /// Mix one cycle. Call `load_input`/`clear_input` for all 12 input
    /// channels first. All 10 output slices must have the same length `n`,
    /// which must equal the length passed to the loaders.
    pub fn process(&mut self, out: &mut [[&mut [f32]; 2]; NUM_BUSES]) {
        self.shared.cycles.fetch_add(1, Relaxed);
        self.follow_rate();
        for bus in out.iter_mut() {
            for ch in bus.iter_mut() {
                ch.fill(0.0);
            }
        }
        let n = out[0][0].len();
        if n == 0 {
            self.rec_skip(0);
            return;
        }
        if n > MAX_FRAMES || out.iter().any(|b| b[0].len() != n || b[1].len() != n) {
            self.shared.oversize_cycles.fetch_add(1, Relaxed);
            self.rec_skip(n);
            return;
        }

        // ---- VBAN in: network streams join their strips' inputs ------------
        if let Some(tap) = self.vban.as_deref() {
            self.vban_rt.pull(tap, n, &mut self.scratch, std::time::Instant::now());
        }

        let shared = &*self.shared;
        let any_solo = shared.strips.iter().any(|s| s.solo.load(Relaxed));
        // Bit s set = strip s's scratch holds this cycle's post-fader signal
        // (a fully silent strip is skipped and its scratch still holds raw input).
        let mut live: u8 = 0;

        // ---- strips -> buses ---------------------------------------------
        for s in 0..NUM_STRIPS {
            let p = &shared.strips[s];
            let silenced = p.mute.load(Relaxed) || (any_solo && !p.solo.load(Relaxed));
            let trim_db = p.trim_db.load();
            let target = side_gains(silenced, p.gain_db.load(), trim_db, p.pan.load());
            let trim = db_to_gain(clamp_trim(trim_db));
            let routes = p.routes.load(Relaxed);
            let route_target: [f32; NUM_BUSES] = array::from_fn(|b| if routes & (1 << b) != 0 { 1.0 } else { 0.0 });
            let g0 = self.strip_gain[s];
            self.strip_gain[s] = target;
            let mono = p.mono.load(Relaxed);
            let base = s * 2 * MAX_FRAMES;

            if g0 == [0.0; 2] && target == [0.0; 2] {
                // Fully silent for the whole cycle: routing and EQ changes
                // are inaudible, so take them at once. The input is still
                // metered: a muted strip shows what comes in.
                let (l, r) = self.scratch[base..base + 2 * MAX_FRAMES].split_at(MAX_FRAMES);
                let (pl, pr) = input_peaks(&l[..n], &r[..n], mono);
                p.input_peak[0].push(pl * trim);
                p.input_peak[1].push(pr * trim);
                self.route_gain[s] = route_target;
                self.eq[s].reset(&p.eq);
                continue;
            }
            live |= 1 << s;

            let (l, r) = self.scratch[base..base + 2 * MAX_FRAMES].split_at_mut(MAX_FRAMES);
            let (l, r) = (&mut l[..n], &mut r[..n]);

            let (mut in_l, mut in_r) = (0.0f32, 0.0f32);
            for i in 0..n {
                let mut a = sanitize(l[i]);
                let mut b = sanitize(r[i]);
                if mono {
                    let m = 0.5 * (a + b);
                    a = m;
                    b = m;
                }
                l[i] = a;
                r[i] = b;
                in_l = in_l.max(a.abs());
                in_r = in_r.max(b.abs());
            }
            p.input_peak[0].push(in_l * trim);
            p.input_peak[1].push(in_r * trim);
            self.eq[s].process(&p.eq, l, r);
            let mut pk_l = 0.0f32;
            let mut pk_r = 0.0f32;
            for i in 0..n {
                let (a, b) = (l[i] * ramp(g0[0], target[0], i, n), r[i] * ramp(g0[1], target[1], i, n));
                l[i] = a;
                r[i] = b;
                pk_l = pk_l.max(a.abs());
                pk_r = pk_r.max(b.abs());
            }
            p.peak[0].push(pk_l);
            p.peak[1].push(pk_r);

            for (b, bus) in out.iter_mut().enumerate() {
                let r0 = self.route_gain[s][b];
                let r1 = route_target[b];
                self.route_gain[s][b] = r1;
                if r0 == 0.0 && r1 == 0.0 {
                    continue;
                }
                let [bl, br] = bus;
                if r0 == 1.0 && r1 == 1.0 {
                    for i in 0..n {
                        bl[i] += l[i];
                        br[i] += r[i];
                    }
                } else {
                    for i in 0..n {
                        let w = ramp(r0, r1, i, n);
                        bl[i] += l[i] * w;
                        br[i] += r[i] * w;
                    }
                }
            }
        }

        // ---- player: a recording played back joins the buses ---------------
        if let Some(tap) = self.play.as_deref() {
            self.play_rt.mix(tap, n, out);
        }

        // ---- bus masters, then their sends to other buses ---------------------
        // Every bus is finished before the buses it feeds, so a send adds no
        // latency; sends are ramped over one cycle like strip routes.
        let requested = shared.bus_routes();
        let active: [u8; NUM_BUSES] =
            array::from_fn(|b| (0..NUM_BUSES).filter(|&d| self.send_gain[b][d] != 0.0).fold(0u8, |m, d| m | 1 << d));
        let (sends, order) = bus_graph(&requested, &active);
        for &b in &order {
            let p = &shared.buses[b];
            let trim_db = p.trim_db.load();
            let target = side_gains(p.mute.load(Relaxed), p.gain_db.load(), trim_db, p.pan.load());
            let trim = db_to_gain(clamp_trim(trim_db));
            let g0 = self.bus_gain[b];
            self.bus_gain[b] = target;
            let mono = p.mono.load(Relaxed);
            {
                let [bl, br] = &mut out[b];
                let (mut in_l, mut in_r) = (0.0f32, 0.0f32);
                if mono {
                    for i in 0..n {
                        let m = 0.5 * (bl[i] + br[i]);
                        bl[i] = m;
                        br[i] = m;
                        in_l = in_l.max(m.abs());
                    }
                    in_r = in_l;
                } else {
                    for i in 0..n {
                        in_l = in_l.max(bl[i].abs());
                        in_r = in_r.max(br[i].abs());
                    }
                }
                p.input_peak[0].push(in_l * trim);
                p.input_peak[1].push(in_r * trim);
                self.eq[NUM_STRIPS + b].process(&p.eq, bl, br);
                let mut pk_l = 0.0f32;
                let mut pk_r = 0.0f32;
                for i in 0..n {
                    let (a, c) = (bl[i] * ramp(g0[0], target[0], i, n), br[i] * ramp(g0[1], target[1], i, n));
                    bl[i] = a;
                    br[i] = c;
                    pk_l = pk_l.max(a.abs());
                    pk_r = pk_r.max(c.abs());
                }
                p.peak[0].push(pk_l);
                p.peak[1].push(pk_r);
            }
            for d in 0..NUM_BUSES {
                let r0 = self.send_gain[b][d];
                let placed = sends[b] & (1 << d) != 0;
                let r1 = if placed && requested[b] & (1 << d) != 0 { 1.0 } else { 0.0 };
                if !placed {
                    // not this cycle (it would close a loop): stays off
                    self.send_gain[b][d] = 0.0;
                    continue;
                }
                self.send_gain[b][d] = r1;
                if r0 == 0.0 && r1 == 0.0 {
                    continue;
                }
                let (src, dst) = two_buses(out, b, d);
                let [sl, sr] = src;
                let [dl, dr] = dst;
                if r0 == 1.0 && r1 == 1.0 {
                    for i in 0..n {
                        dl[i] += sl[i];
                        dr[i] += sr[i];
                    }
                } else {
                    for i in 0..n {
                        let w = ramp(r0, r1, i, n);
                        dl[i] += sl[i] * w;
                        dr[i] += sr[i] * w;
                    }
                }
            }
        }

        // ---- recorder tap and VBAN out (same cycle, same frames) ----------
        let live: [bool; NUM_STRIPS] = array::from_fn(|s| live & (1 << s) != 0);
        if let Some(tap) = self.rec.as_deref() {
            let links = shared.rec_links.load(Relaxed);
            self.rec_rt.push(tap, links, n, self.tap_rate, &self.scratch, &live, out);
        }
        if let Some(tap) = self.vban.as_deref() {
            self.vban_rt.push(tap, n, &self.scratch, &live, out);
        }
    }
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 128;

    struct Rig {
        eng: Engine,
        out: Vec<Vec<f32>>, // 10 channels
    }

    impl Rig {
        fn new() -> (Rig, Arc<Shared>) {
            let sh = Shared::new();
            (Rig { eng: Engine::new(sh.clone()), out: vec![vec![0.0; N]; NUM_BUSES * 2] }, sh)
        }

        /// inputs: closure (strip, ch, frame) -> sample
        fn run(&mut self, n: usize, f: impl Fn(usize, usize, usize) -> f32) {
            for s in 0..NUM_STRIPS {
                for c in 0..2 {
                    let v: Vec<f32> = (0..n).map(|i| f(s, c, i)).collect();
                    self.eng.load_input(s, c, &v);
                }
            }
            for o in &mut self.out {
                o.clear();
                o.resize(n, 7.0); // garbage: engine must overwrite
            }
            let mut it = self.out.iter_mut();
            let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = array::from_fn(|_| {
                let l = it.next().unwrap().as_mut_slice();
                let r = it.next().unwrap().as_mut_slice();
                [l, r]
            });
            self.eng.process(&mut outs);
        }

        fn bus(&self, b: usize, ch: usize) -> &[f32] {
            &self.out[b * 2 + ch]
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn silent_with_no_routes() {
        let (mut rig, _sh) = Rig::new();
        rig.run(N, |_, _, _| 1.0);
        rig.run(N, |_, _, _| 1.0);
        for b in 0..NUM_BUSES {
            assert!(rig.bus(b, 0).iter().all(|&x| x == 0.0));
            assert!(rig.bus(b, 1).iter().all(|&x| x == 0.0));
        }
    }

    #[test]
    fn all_six_strips_are_sample_synchronous_on_every_bus() {
        let (mut rig, sh) = Rig::new();
        for s in 0..NUM_STRIPS {
            sh.strips[s].routes.store(0b11111, Relaxed);
        }
        rig.run(N, |_, _, _| 0.0); // settle ramps
        // Each strip gets a distinct-amplitude impulse at the SAME frame 37.
        // Amplitudes are powers of two so the sum identifies every strip.
        rig.run(N, |s, _, i| if i == 37 { (1u32 << s) as f32 / 64.0 } else { 0.0 });
        let expected: f32 = (0..NUM_STRIPS).map(|s| (1u32 << s) as f32 / 64.0).sum();
        for b in 0..NUM_BUSES {
            for ch in 0..2 {
                let o = rig.bus(b, ch);
                for (i, &x) in o.iter().enumerate() {
                    if i == 37 {
                        assert!(approx(x, expected), "bus {b} ch {ch}: {x}");
                    } else {
                        assert_eq!(x, 0.0, "bus {b} ch {ch} frame {i} leaked");
                    }
                }
            }
        }
    }

    #[test]
    fn zero_added_latency_impulse_at_frame_0_and_last() {
        let (mut rig, sh) = Rig::new();
        sh.strips[2].routes.store(1, Relaxed);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, i| if s == 2 && (i == 0 || i == N - 1) { 0.5 } else { 0.0 });
        assert!(approx(rig.bus(0, 0)[0], 0.5));
        assert!(approx(rig.bus(0, 0)[N - 1], 0.5));
    }

    #[test]
    fn gain_ramps_then_settles_exactly() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        sh.strips[0].gain_db.store(-6.0);
        rig.run(N, |_, _, _| 1.0); // ramp from 0
        let first = rig.bus(0, 0).to_vec();
        // monotonic ramp, no step bigger than one ramp increment
        for w in first.windows(2) {
            assert!(w[1] >= w[0]);
            assert!(w[1] - w[0] < 0.02);
        }
        let g = db_to_gain(-6.0);
        assert!(approx(*first.last().unwrap(), g));
        rig.run(N, |_, _, _| 1.0);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, g)));
    }

    #[test]
    fn mute_and_solo() {
        let (mut rig, sh) = Rig::new();
        for s in 0..NUM_STRIPS {
            sh.strips[s].routes.store(1, Relaxed);
        }
        rig.run(N, |_, _, _| 0.0);
        // only strip 3 soloed -> bus = strip 3's signal
        sh.strips[3].solo.store(true, Relaxed);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.04)));
        // mute the soloed strip -> silence
        sh.strips[3].mute.store(true, Relaxed);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        // unsolo -> everyone except muted strip 3
        sh.strips[3].solo.store(false, Relaxed);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        rig.run(N, |s, _, _| (s + 1) as f32 * 0.01);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.21 - 0.04)));
    }

    #[test]
    fn routing_is_per_bus() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b01000, Relaxed); // B1 only
        sh.strips[1].routes.store(0b00001, Relaxed); // A1 only
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, _| {
            if s == 0 {
                0.25
            } else if s == 1 {
                0.5
            } else {
                0.0
            }
        });
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)));
        assert!(rig.bus(3, 1).iter().all(|&x| approx(x, 0.25)));
        for b in [1, 2, 4] {
            assert!(rig.bus(b, 0).iter().all(|&x| x == 0.0));
        }
        // toggling a route ramps rather than stepping
        sh.toggle_route(0, 0);
        rig.run(N, |s, _, _| {
            if s == 0 {
                0.25
            } else if s == 1 {
                0.5
            } else {
                0.0
            }
        });
        let a1 = rig.bus(0, 0);
        assert!(a1[0] > 0.5 && a1[0] < 0.51);
        assert!(approx(a1[N - 1], 0.75));
    }

    #[test]
    fn strip_and_bus_mono() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b00011, Relaxed);
        sh.strips[0].mono.store(true, Relaxed);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, c, _| if s == 0 { if c == 0 { 1.0 } else { 0.0 } } else { 0.0 });
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)));
        assert!(rig.bus(0, 1).iter().all(|&x| approx(x, 0.5)));

        sh.strips[0].mono.store(false, Relaxed);
        sh.buses[1].mono.store(true, Relaxed);
        rig.run(N, |s, c, _| if s == 0 && c == 1 { 0.8 } else { 0.0 });
        assert!(rig.bus(1, 0).iter().all(|&x| approx(x, 0.4)));
        assert!(rig.bus(1, 1).iter().all(|&x| approx(x, 0.4)));
    }

    #[test]
    fn bus_gain_and_mute() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b10000, Relaxed);
        sh.buses[4].gain_db.store(6.0);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |_, _, _| 0.1);
        rig.run(N, |_, _, _| 0.1);
        assert!(rig.bus(4, 0).iter().all(|&x| approx(x, 0.1 * db_to_gain(6.0))));
        sh.buses[4].mute.store(true, Relaxed);
        rig.run(N, |_, _, _| 0.1);
        rig.run(N, |_, _, _| 0.1);
        assert!(rig.bus(4, 0).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn nan_and_inf_are_silenced() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        sh.strips[1].routes.store(1, Relaxed);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, i| match (s, i % 3) {
            (0, 0) => f32::NAN,
            (0, 1) => f32::INFINITY,
            (0, _) => f32::NEG_INFINITY,
            (1, _) => 0.3,
            _ => 0.0,
        });
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.3)));
    }

    #[test]
    fn gain_param_out_of_range_or_nan_is_clamped() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        sh.strips[0].gain_db.store(f32::NAN);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |_, _, _| 0.5);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)));
        sh.strips[0].gain_db.store(1000.0);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |_, _, _| 0.01);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.01 * db_to_gain(MAX_GAIN_DB))));
    }

    #[test]
    fn oversize_and_empty_cycles_do_not_panic() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        rig.run(0, |_, _, _| 1.0);
        rig.run(MAX_FRAMES + 1, |_, _, _| 1.0);
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        assert_eq!(sh.oversize_cycles.load(Relaxed), 1);
        rig.run(MAX_FRAMES, |_, _, _| 1.0);
        rig.run(1, |_, _, _| 1.0);
        assert!(approx(rig.bus(0, 0)[0], 1.0));
    }

    #[test]
    fn varying_period_sizes() {
        let (mut rig, sh) = Rig::new();
        sh.strips[5].routes.store(0b00100, Relaxed);
        for &n in &[64usize, 128, 256, 32, 1024, 17] {
            rig.run(n, |_, _, _| 0.2);
        }
        assert!(rig.bus(2, 0).iter().all(|&x| approx(x, 0.2)));
        assert_eq!(rig.bus(2, 0).len(), 17);
    }

    #[test]
    fn meters_report_post_fader_peak() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        sh.strips[0].gain_db.store(-20.0);
        rig.run(N, |_, _, _| 0.0);
        let _ = sh.strips[0].peak[0].take();
        rig.run(N, |_, c, i| if c == 0 && i == 5 { -1.0 } else { 0.0 });
        assert!(approx(sh.strips[0].peak[0].take(), 0.1));
        assert_eq!(sh.strips[0].peak[1].take(), 0.0);
        assert!(approx(sh.buses[0].peak[0].take(), 0.1));
        assert_eq!(sh.strips[0].peak[0].take(), 0.0, "take() resets");
    }

    #[test]
    fn input_gain_and_pan() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(1, Relaxed);
        sh.strips[0].trim_db.store(6.0);
        sh.strips[0].gain_db.store(-6.0);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        let g = db_to_gain(6.0) * db_to_gain(-6.0);
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.25 * g)));
        // half right: left at -3 dB, right untouched; ramped, not stepped
        sh.strips[0].pan.store(0.5);
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        let a = rig.bus(0, 0);
        assert!(a[0] > 0.25 * g * 0.99 && approx(a[N - 1], 0.25 * g * std::f32::consts::FRAC_1_SQRT_2));
        assert!(rig.bus(0, 1).iter().all(|&x| approx(x, 0.25 * g)));
        // hard left: right exactly silent
        sh.strips[0].pan.store(-1.0);
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        assert!(rig.bus(0, 1).iter().all(|&x| x == 0.0));
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.25 * g)));
        // bus balance and bus input gain
        sh.strips[0].pan.store(0.0);
        sh.buses[0].pan.store(1.0);
        sh.buses[0].trim_db.store(-6.0);
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        rig.run(N, |s, _, _| if s == 0 { 0.25 } else { 0.0 });
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        assert!(rig.bus(0, 1).iter().all(|&x| approx(x, 0.25 * g * db_to_gain(-6.0))));
        // out of range and NaN are clamped / ignored
        sh.buses[0].pan.store(f32::NAN);
        sh.buses[0].trim_db.store(f32::NAN);
        sh.strips[0].trim_db.store(1000.0);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, _| if s == 0 { 0.001 } else { 0.0 });
        let g = db_to_gain(MAX_TRIM_DB) * db_to_gain(-6.0);
        assert!(
            rig.bus(0, 0).iter().all(|&x| approx(x, 0.001 * g)) && rig.bus(0, 1).iter().all(|&x| approx(x, 0.001 * g))
        );
    }

    #[test]
    fn centre_and_zero_gain_are_bit_exact() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-0.0), (1.0, 1.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(f32::NAN), (1.0, 1.0));
        assert_eq!(db_to_gain(0.0), 1.0);
        for db in [-60.0f32, -12.5, -0.1, 0.0, 3.3, 12.0] {
            assert_eq!(side_gains(false, db, 0.0, 0.0), [db_to_gain(db); 2]);
        }
        assert_eq!(side_gains(true, 0.0, 12.0, 0.3), [0.0; 2]);
    }

    #[test]
    fn player_joins_the_buses_before_their_fader() {
        use crate::player::{PlayTap, START_FRAMES};
        let (mut rig, sh) = Rig::new();
        let tap = PlayTap::new();
        rig.eng.attach_player(tap.clone());
        tap.routes.store(0b01000, Relaxed); // B1
        sh.buses[3].gain_db.store(-6.0);
        tap.test_push(&vec![0.5f32; 2 * (START_FRAMES + 4 * N)]);
        tap.test_run(true, false);
        rig.run(N, |_, _, _| 0.0); // bus gains ramp in
        rig.run(N, |_, _, _| 0.0);
        assert!(rig.bus(3, 0).iter().all(|&x| approx(x, 0.5 * db_to_gain(-6.0))));
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        // a strip on the same bus adds to it
        sh.strips[1].routes.store(0b01000, Relaxed);
        rig.run(N, |s, _, _| if s == 1 { 0.25 } else { 0.0 });
        rig.run(N, |s, _, _| if s == 1 { 0.25 } else { 0.0 });
        assert!(rig.bus(3, 1).iter().all(|&x| approx(x, 0.75 * db_to_gain(-6.0))));
        // oversize cycles still honour a flush
        tap.test_flush();
        rig.run(MAX_FRAMES + 1, |_, _, _| 0.0);
        assert_eq!(tap.test_played(), 0);
    }

    #[test]
    fn input_meters_keep_showing_a_muted_channel() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b00001, Relaxed);
        sh.strips[0].trim_db.store(6.0);
        sh.strips[0].mute.store(true, Relaxed);
        sh.strips[1].routes.store(0b00010, Relaxed);
        sh.buses[1].mute.store(true, Relaxed);
        rig.run(N, |_, _, _| 0.0);
        for s in &sh.strips {
            let _ = (s.peak[0].take(), s.input_peak[0].take());
        }
        rig.run(N, |s, c, _| match (s, c) {
            (0, 0) => 0.25,
            (0, _) => -0.125,
            (1, _) => 0.5,
            _ => 0.0,
        });
        // strip 1 is muted: nothing after its fader, its input still shows (x input gain)
        assert_eq!(sh.strips[0].peak[0].take(), 0.0);
        assert!(approx(sh.strips[0].input_peak[0].take(), 0.25 * db_to_gain(6.0)));
        assert!(approx(sh.strips[0].input_peak[1].take(), 0.125 * db_to_gain(6.0)));
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        // a strip that is soloed out shows its input too
        sh.strips[0].mute.store(false, Relaxed);
        sh.strips[2].solo.store(true, Relaxed);
        rig.run(N, |s, _, _| if s == 1 { 0.5 } else { 0.0 }); // ramps out
        let _ = sh.strips[1].peak[0].take();
        rig.run(N, |s, _, _| if s == 1 { 0.5 } else { 0.0 });
        assert_eq!(sh.strips[1].peak[0].take(), 0.0);
        assert!(approx(sh.strips[1].input_peak[0].take(), 0.5));
        // mono is metered as the fader sees it
        sh.strips[2].solo.store(false, Relaxed);
        sh.strips[3].mono.store(true, Relaxed);
        sh.strips[3].mute.store(true, Relaxed);
        rig.run(N, |s, c, _| if s == 3 && c == 0 { 0.8 } else { 0.0 });
        assert!(approx(sh.strips[3].input_peak[0].take(), 0.4) && approx(sh.strips[3].input_peak[1].take(), 0.4));
        // a muted bus shows what reaches it (x its input gain)
        sh.buses[1].trim_db.store(-6.0);
        rig.run(N, |s, _, _| if s == 1 { 0.5 } else { 0.0 });
        let _ = (sh.buses[1].peak[0].take(), sh.buses[1].input_peak[0].take());
        rig.run(N, |s, _, _| if s == 1 { 0.5 } else { 0.0 });
        assert!(rig.bus(1, 0).iter().all(|&x| x == 0.0));
        assert_eq!(sh.buses[1].peak[0].take(), 0.0);
        assert!(approx(sh.buses[1].input_peak[0].take(), 0.5 * db_to_gain(-6.0)));
        // NaN and infinity never reach a meter as such
        rig.run(N, |s, _, i| if s == 3 { if i % 2 == 0 { f32::NAN } else { f32::INFINITY } } else { 0.0 });
        assert_eq!(sh.strips[3].input_peak[0].take(), 0.0);
    }

    #[test]
    fn a_bus_feeds_another_after_its_fader_in_the_same_cycle() {
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b01000, Relaxed); // strip 1 -> B1
        sh.strips[1].routes.store(0b00001, Relaxed); // strip 2 -> A1
        sh.buses[3].gain_db.store(-6.0); // B1 fader
        sh.buses[3].routes.store(0b00001, Relaxed); // B1 -> A1
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |_, _, _| 0.0);
        rig.run(N, |s, _, i| {
            if i == 37 {
                if s == 0 {
                    0.5
                } else if s == 1 {
                    0.25
                } else {
                    0.0
                }
            } else {
                0.0
            }
        });
        let g = db_to_gain(-6.0);
        for c in 0..2 {
            for (i, &x) in rig.bus(0, c).iter().enumerate() {
                let want = if i == 37 { 0.25 + 0.5 * g } else { 0.0 };
                assert!(approx(x, want), "A1 ch {c} frame {i}: {x}");
            }
            assert!(approx(rig.bus(3, c)[37], 0.5 * g));
        }
        // the receiving bus's own fader applies on top
        sh.buses[0].gain_db.store(-12.0);
        rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5 * g * db_to_gain(-12.0))));
        // muting B1 silences what it sends (ramped)
        sh.buses[3].mute.store(true, Relaxed);
        rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        let a1 = rig.bus(0, 0).to_vec();
        assert!(a1[0] > 0.0 && a1[N - 1] == 0.0);
        rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0));
        // a bus earlier in the list can be fed by a later one and feed another
        sh.buses[3].mute.store(false, Relaxed);
        sh.buses[0].gain_db.store(0.0);
        sh.buses[3].routes.store(0, Relaxed);
        sh.buses[4].routes.store(0b00100, Relaxed); // B2 -> A3
        sh.buses[2].routes.store(0b00010, Relaxed); // A3 -> A2
        sh.strips[2].routes.store(0b10000, Relaxed); // strip 3 -> B2
        rig.run(N, |s, _, _| if s == 2 { 0.1 } else { 0.0 });
        rig.run(N, |s, _, _| if s == 2 { 0.1 } else { 0.0 });
        assert!(rig.bus(1, 1).iter().all(|&x| approx(x, 0.1)), "B2 -> A3 -> A2");
        assert!(rig.bus(2, 0).iter().all(|&x| approx(x, 0.1)));
        assert!(rig.bus(0, 0).iter().all(|&x| x == 0.0), "the B1 send ramped out");
    }

    #[test]
    fn bus_sends_never_make_a_loop() {
        let sh = Shared::new();
        sh.buses[0].routes.store(0b01000, Relaxed); // A1 -> B1
        sh.buses[3].routes.store(0b10000, Relaxed); // B1 -> B2
        assert!(sh.send_loops(4, 0), "B2 -> A1 would close A1 -> B1 -> B2 -> A1");
        assert!(sh.send_loops(3, 3), "a bus into itself");
        assert!(!sh.send_loops(0, 4), "A1 -> B2 is a shortcut, not a loop");
        assert!(!sh.send_loops(1, 0));
        assert_eq!(reach(&sh.bus_routes(), 0), 0b11001);
        // even if a loop is asked for (two changes racing), the engine keeps
        // it open: no feedback, everything stays finite
        let (mut rig, sh) = Rig::new();
        sh.strips[0].routes.store(0b00001, Relaxed);
        sh.buses[0].routes.store(0b01000, Relaxed); // A1 -> B1
        sh.buses[3].routes.store(0b00001, Relaxed); // B1 -> A1 (loop)
        sh.buses[0].routes.fetch_or(1, Relaxed); // its own bit is ignored
        for _ in 0..50 {
            rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        }
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)), "A1 gets strip 1 once");
        assert!(rig.bus(3, 0).iter().all(|&x| approx(x, 0.5)), "B1 gets A1");
        // turning the first send off lets the other one in, a cycle later
        sh.buses[0].routes.store(0, Relaxed);
        for _ in 0..3 {
            rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        }
        assert!(rig.bus(3, 0).iter().all(|&x| x == 0.0));
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)));
        sh.strips[0].routes.store(0b01000, Relaxed); // strip 1 -> B1 -> A1
        for _ in 0..3 {
            rig.run(N, |s, _, _| if s == 0 { 0.5 } else { 0.0 });
        }
        assert!(rig.bus(0, 0).iter().all(|&x| approx(x, 0.5)) && rig.bus(3, 0).iter().all(|&x| approx(x, 0.5)));
    }

    #[test]
    fn bus_graph_orders_feeders_first_and_keeps_ramping_sends() {
        let none = [0u8; NUM_BUSES];
        let (g, order) = bus_graph(&[0, 0, 0, 0b00001, 0b01000], &none); // B1 -> A1, B2 -> B1
        assert_eq!(g, [0, 0, 0, 0b00001, 0b01000]);
        let pos = |b: usize| order.iter().position(|&x| x == b).unwrap();
        assert!(pos(4) < pos(3) && pos(3) < pos(0));
        let mut sorted = order;
        sorted.sort();
        assert_eq!(sorted, [0, 1, 2, 3, 4]);
        // a send still ramping out (active) wins over a new one that would loop
        let (g, _) = bus_graph(&[0, 0, 0, 0b00001, 0], &[0b01000, 0, 0, 0, 0]);
        assert_eq!(g, [0b01000, 0, 0, 0, 0]);
    }

    #[test]
    fn the_engine_follows_the_servers_sample_rate() {
        let (mut rig, sh) = Rig::new();
        let rec = crate::recorder::RecTap::new();
        let play = crate::player::PlayTap::new();
        let vban = crate::vban::VbanTap::new();
        rig.eng.attach_recorder(rec.clone());
        rig.eng.attach_player(play.clone());
        rig.eng.attach_vban(vban.clone());
        rig.run(N, |_, _, _| 0.0);
        assert_eq!(sh.cycles.load(Relaxed), 1);
        assert_eq!((rec.rate.load(Relaxed), play.rate.load(Relaxed), vban.rate.load(Relaxed)), (0, 0, 0));
        sh.sample_rate.store(96_000, Relaxed);
        rig.run(N, |_, _, _| 0.0);
        assert_eq!(
            (rec.rate.load(Relaxed), play.rate.load(Relaxed), vban.rate.load(Relaxed)),
            (96_000, 96_000, 96_000)
        );
        assert_eq!(rig.eng.rate, 96_000);
        rig.run(0, |_, _, _| 0.0); // empty and oversize cycles count too
        assert_eq!(sh.cycles.load(Relaxed), 3);
    }

    #[test]
    fn db_conversions() {
        assert!(approx(db_to_gain(0.0), 1.0));
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(gain_to_db(0.0), -120.0);
    }
}
