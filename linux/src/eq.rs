//! Parametric equalizer: six bands on every strip and on every bus.
//!
//! Each band is a state-variable filter in Andrew Simper's linear
//! trapezoidal form ("SvfLinearTrapOptimised2"). Its frequency responses
//! are those of the RBJ "Audio EQ Cookbook" biquads (bell, shelves, low and
//! high cut, notch), but unlike a direct-form biquad it stays well behaved
//! while its frequency, gain and Q change, and it keeps its precision in
//! `f32` down to 20 Hz.
//!
//! Real-time behaviour (see [`EqRt::process`]):
//! * **Click-free.** Frequency, gain and Q glide to a new setting (20 ms
//!   time constant, recomputed every 32 samples). Switching a band or the
//!   whole EQ on or off cross-fades between the dry and the filtered signal
//!   over 10 ms; changing a band's type fades the old type out and the new
//!   one in.
//! * **Transparent when flat.** A bell or shelf at 0 dB is skipped, and so
//!   is a band or EQ that is off: the signal passes bit for bit.
//! * **Real-time safe.** No allocation, locks or syscalls; parameters are
//!   atomics read once per cycle; filter state never becomes denormal or
//!   non-finite.

use crate::engine::AtomicF32;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};

pub const BANDS: usize = 6;
pub const MIN_FREQ: f32 = 20.0;
pub const MAX_FREQ: f32 = 20_000.0;
pub const MIN_GAIN_DB: f32 = -24.0;
pub const MAX_GAIN_DB: f32 = 24.0;
pub const MIN_Q: f32 = 0.1;
pub const MAX_Q: f32 = 30.0;

/// Parameters are smoothed and coefficients recomputed every SUB samples.
const SUB: usize = 32;
/// Time constant of each of the two cascaded one-pole smoothers that glide
/// frequency, gain and Q (an S-shaped move: no kink when it starts).
const GLIDE_S: f64 = 0.010;
/// Length of an on/off cross-fade.
const FADE_S: f64 = 0.010;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Bell = 0,
    LowShelf = 1,
    HighShelf = 2,
    /// High-pass, 12 dB/octave.
    LowCut = 3,
    /// Low-pass, 12 dB/octave.
    HighCut = 4,
    Notch = 5,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Bell, Kind::LowShelf, Kind::HighShelf, Kind::LowCut, Kind::HighCut, Kind::Notch];

    /// Name in the config file and the GUI protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bell => "bell",
            Kind::LowShelf => "lowshelf",
            Kind::HighShelf => "highshelf",
            Kind::LowCut => "lowcut",
            Kind::HighCut => "highcut",
            Kind::Notch => "notch",
        }
    }

    /// Name for people.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bell => "bell",
            Kind::LowShelf => "low shelf",
            Kind::HighShelf => "high shelf",
            Kind::LowCut => "low cut",
            Kind::HighCut => "high cut",
            Kind::Notch => "notch",
        }
    }

    pub fn from_u8(v: u8) -> Kind {
        Kind::ALL.get(v as usize).copied().unwrap_or(Kind::Bell)
    }

    pub fn parse(v: &str) -> Result<Kind, String> {
        match v.trim().to_ascii_lowercase().as_str() {
            "bell" | "peak" | "peaking" => Ok(Kind::Bell),
            "lowshelf" | "low-shelf" | "low_shelf" | "ls" => Ok(Kind::LowShelf),
            "highshelf" | "high-shelf" | "high_shelf" | "hs" => Ok(Kind::HighShelf),
            "lowcut" | "low-cut" | "low_cut" | "highpass" | "hp" | "hpf" => Ok(Kind::LowCut),
            "highcut" | "high-cut" | "high_cut" | "lowpass" | "lp" | "lpf" => Ok(Kind::HighCut),
            "notch" => Ok(Kind::Notch),
            _ => Err(format!(
                "unknown EQ band type {v:?} (expected bell, lowshelf, highshelf, lowcut, highcut or notch)"
            )),
        }
    }

    /// Bell and shelves have a gain; cuts and the notch do not.
    pub fn has_gain(self) -> bool {
        matches!(self, Kind::Bell | Kind::LowShelf | Kind::HighShelf)
    }
}

/// One band's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub on: bool,
    pub kind: Kind,
    /// Hz, MIN_FREQ..MAX_FREQ.
    pub freq: f32,
    /// dB, MIN_GAIN_DB..MAX_GAIN_DB (ignored by cuts and the notch).
    pub gain_db: f32,
    /// MIN_Q..MAX_Q.
    pub q: f32,
}

const fn band(kind: Kind, freq: f32, q: f32) -> Band {
    Band { on: true, kind, freq, gain_db: 0.0, q }
}

/// A flat EQ laid out like a tone control: shelves at the ends, four bells
/// between them. All at 0 dB, so switching the EQ on changes nothing until
/// a gain is moved.
pub const DEFAULT_BANDS: [Band; BANDS] = [
    band(Kind::LowShelf, 100.0, 0.71),
    band(Kind::Bell, 250.0, 1.0),
    band(Kind::Bell, 600.0, 1.0),
    band(Kind::Bell, 1500.0, 1.0),
    band(Kind::Bell, 4000.0, 1.0),
    band(Kind::HighShelf, 10_000.0, 0.71),
];

/// Settings are stored rounded: 1 Hz, 0.1 dB, 0.01 for Q.
pub fn round_freq(f: f32) -> f32 {
    f.round()
}
pub fn round_gain(g: f32) -> f32 {
    (g * 10.0).round() / 10.0
}
pub fn round_q(q: f32) -> f32 {
    (q * 100.0).round() / 100.0
}

fn clamp_or(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}

impl Band {
    /// Every value in range, rounded like the stored settings; a non-finite
    /// value becomes the `fallback` band's.
    pub fn sanitized(self, fallback: &Band) -> Band {
        Band {
            on: self.on,
            kind: self.kind,
            freq: round_freq(clamp_or(self.freq, MIN_FREQ, MAX_FREQ, fallback.freq)),
            gain_db: round_gain(clamp_or(self.gain_db, MIN_GAIN_DB, MAX_GAIN_DB, 0.0)),
            q: round_q(clamp_or(self.q, MIN_Q, MAX_Q, fallback.q)),
        }
    }

    /// Config file form: `bell 1000 +3 1.41`, with ` off` if the band is off.
    pub fn to_config(&self) -> String {
        let mut s = format!("{} {} {:+} {}", self.kind.as_str(), self.freq, self.gain_db, self.q);
        if !self.on {
            s.push_str(" off");
        }
        s
    }

    /// Parse `type freq_hz gain_db q [on|off]`. Values out of range are
    /// errors (a config typo must not silently become something else).
    pub fn parse(v: &str) -> Result<Band, String> {
        let t: Vec<&str> = v.split_whitespace().collect();
        if !(4..=5).contains(&t.len()) {
            return Err(format!("expected `type freq gain q [off]`, e.g. `bell 1000 +3 1.4`, got {v:?}"));
        }
        let kind = Kind::parse(t[0])?;
        let num = |s: &str, what: &str, lo: f32, hi: f32| -> Result<f32, String> {
            let x: f32 = s.parse().map_err(|_| format!("EQ {what} must be a number, got {s:?}"))?;
            if !x.is_finite() || x < lo || x > hi {
                return Err(format!("EQ {what} {s} is out of range ({lo} to {hi})"));
            }
            Ok(x)
        };
        let freq = round_freq(num(t[1], "frequency", MIN_FREQ, MAX_FREQ)?);
        let gain_db = round_gain(num(t[2], "gain", MIN_GAIN_DB, MAX_GAIN_DB)?);
        let q = round_q(num(t[3], "Q", MIN_Q, MAX_Q)?);
        let on = match t.get(4).map(|s| s.to_ascii_lowercase()) {
            None => true,
            Some(s) if s == "on" => true,
            Some(s) if s == "off" => false,
            Some(s) => return Err(format!("expected on or off after the Q, got {s:?}")),
        };
        Ok(Band { on, kind, freq, gain_db, q })
    }
}

// ---------------------------------------------------------------------------
// Shared (atomic) parameters
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct BandParams {
    pub on: AtomicBool,
    pub kind: AtomicU8,
    pub freq: AtomicF32,
    pub gain_db: AtomicF32,
    pub q: AtomicF32,
}

impl BandParams {
    fn new(b: &Band) -> BandParams {
        BandParams {
            on: AtomicBool::new(b.on),
            kind: AtomicU8::new(b.kind as u8),
            freq: AtomicF32::new(b.freq),
            gain_db: AtomicF32::new(b.gain_db),
            q: AtomicF32::new(b.q),
        }
    }

    pub fn load(&self) -> Band {
        Band {
            on: self.on.load(Relaxed),
            kind: Kind::from_u8(self.kind.load(Relaxed)),
            freq: self.freq.load(),
            gain_db: self.gain_db.load(),
            q: self.q.load(),
        }
    }

    pub fn store(&self, b: &Band) {
        self.on.store(b.on, Relaxed);
        self.kind.store(b.kind as u8, Relaxed);
        self.freq.store(b.freq);
        self.gain_db.store(b.gain_db);
        self.q.store(b.q);
    }
}

/// One channel's EQ, as the UIs and the config see it.
#[derive(Debug)]
pub struct EqParams {
    pub on: AtomicBool,
    pub bands: [BandParams; BANDS],
}

impl Default for EqParams {
    fn default() -> Self {
        EqParams { on: AtomicBool::new(false), bands: std::array::from_fn(|i| BandParams::new(&DEFAULT_BANDS[i])) }
    }
}

impl EqParams {
    pub fn bands(&self) -> [Band; BANDS] {
        std::array::from_fn(|i| self.bands[i].load())
    }

    pub fn store_bands(&self, bands: &[Band; BANDS]) {
        for (p, b) in self.bands.iter().zip(bands) {
            p.store(b);
        }
    }

    pub fn settings(&self) -> EqSettings {
        EqSettings { on: self.on.load(Relaxed), bands: self.bands() }
    }

    pub fn apply(&self, s: &EqSettings) {
        self.store_bands(&s.bands);
        self.on.store(s.on, Relaxed);
    }
}

/// One channel's EQ as stored in the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct EqSettings {
    pub on: bool,
    pub bands: [Band; BANDS],
}

impl Default for EqSettings {
    fn default() -> Self {
        EqSettings { on: false, bands: DEFAULT_BANDS }
    }
}

// ---------------------------------------------------------------------------
// Filter math
// ---------------------------------------------------------------------------

/// Coefficients of one SVF band: the prototype `g` (prewarped cutoff) and
/// `k` (damping), the output mix `m0..m2`, and `a1..a3` derived from g, k.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Coefs {
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    m0: f32,
    m1: f32,
    m2: f32,
}

impl Coefs {
    /// `a1..a3` for this `g`, `k`.
    #[inline(always)]
    fn derive(mut self) -> Coefs {
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
        self
    }

    /// The filter a fraction `t` of the way from `self` to `to`. Every
    /// in-between filter has g > 0 and k > 0, so it is stable.
    #[inline(always)]
    fn lerp(&self, to: &Coefs, t: f32) -> Coefs {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Coefs {
            g: l(self.g, to.g),
            k: l(self.k, to.k),
            m0: l(self.m0, to.m0),
            m1: l(self.m1, to.m1),
            m2: l(self.m2, to.m2),
            ..Coefs::default()
        }
        .derive()
    }
}

fn coefs(kind: Kind, freq: f32, gain_db: f32, q: f32, sr: f64) -> Coefs {
    // Stay below Nyquist (at 44.1 kHz 20 kHz is fine; at 32 kHz it is not).
    let f = (freq as f64).clamp(MIN_FREQ as f64, 0.49 * sr);
    let t = (PI * f / sr).tan();
    let q = (q as f64).clamp(MIN_Q as f64, MAX_Q as f64);
    let a = 10f64.powf(gain_db as f64 / 40.0);
    let (g, k, m0, m1, m2) = match kind {
        Kind::LowCut => (t, 1.0 / q, 1.0, -1.0 / q, -1.0),
        Kind::HighCut => (t, 1.0 / q, 0.0, 0.0, 1.0),
        Kind::Notch => (t, 1.0 / q, 1.0, -1.0 / q, 0.0),
        Kind::Bell => {
            let k = 1.0 / (q * a);
            (t, k, 1.0, k * (a * a - 1.0), 0.0)
        }
        Kind::LowShelf => {
            let k = 1.0 / q;
            (t / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0)
        }
        Kind::HighShelf => {
            let k = 1.0 / q;
            (t * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a)
        }
    };
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    Coefs {
        g: g as f32,
        k: k as f32,
        a1: a1 as f32,
        a2: a2 as f32,
        a3: a3 as f32,
        m0: m0 as f32,
        m1: m1 as f32,
        m2: m2 as f32,
    }
}

/// The RBJ cookbook biquad of a band: `(b0, b1, b2, a1, a2)`, normalized
/// so a0 = 1. This is the reference the SVF is tested against, and what
/// the GUI draws (its JavaScript uses the same formulas).
pub fn rbj_biquad(b: &Band, sr: f64) -> [f64; 5] {
    let f = (b.freq as f64).clamp(MIN_FREQ as f64, 0.49 * sr);
    let w0 = 2.0 * PI * f / sr;
    let (cw, sw) = (w0.cos(), w0.sin());
    let q = b.q as f64;
    let alpha = sw / (2.0 * q);
    let a = 10f64.powf(b.gain_db as f64 / 40.0);
    let sa = a.sqrt();
    let (b0, b1, b2, a0, a1, a2) = match b.kind {
        Kind::Bell => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
        Kind::LowShelf => (
            a * ((a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
            a * ((a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha),
            (a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cw),
            (a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha,
        ),
        Kind::HighShelf => (
            a * ((a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
            a * ((a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha),
            (a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cw),
            (a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha,
        ),
        Kind::LowCut => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        Kind::HighCut => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        Kind::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Magnitude in dB of a band at `f` Hz (from the RBJ biquad).
pub fn band_db(b: &Band, sr: f64, f: f64) -> f64 {
    let [b0, b1, b2, a1, a2] = rbj_biquad(b, sr);
    let w = 2.0 * PI * f / sr;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let num = (b0 + b1 * c1 + b2 * c2).powi(2) + (b1 * s1 + b2 * s2).powi(2);
    let den = (1.0 + a1 * c1 + a2 * c2).powi(2) + (a1 * s1 + a2 * s2).powi(2);
    10.0 * (num / den).log10()
}

// ---------------------------------------------------------------------------
// Real-time processing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct BandRt {
    /// What is running now (may lag the target during a fade).
    kind: Kind,
    freq: f32,
    gain_db: f32,
    q: f32,
    /// First stage of the two-stage glide (freq, gain, q).
    mid: [f32; 3],
    /// Dry/wet: 0 = band bypassed, 1 = fully filtered.
    wet: f32,
    c: Coefs,
    /// `[L/R][ic1eq, ic2eq]`
    s: [[f32; 2]; 2],
}

/// The audio thread's part of one channel's EQ.
#[derive(Debug, Clone)]
pub struct EqRt {
    b: [BandRt; BANDS],
    sr: f64,
    glide: f32,
    fade: f32,
}

#[inline(always)]
fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

fn move_toward(v: f32, target: f32, step: f32) -> f32 {
    if v < target { (v + step).min(target) } else { (v - step).max(target) }
}

/// One step of a two-stage glide (`mid` follows the target, `cur`
/// follows `mid`), in the log domain for frequency and Q. Returns the new
/// `(cur, mid)`; lands exactly on `target` once close.
fn glide(cur: f32, mid: f32, target: f32, a: f32, log: bool) -> (f32, f32) {
    if cur == target && mid == target {
        return (target, target);
    }
    let (c, m, t) = if log { (cur.ln(), mid.ln(), target.ln()) } else { (cur, mid, target) };
    let m = m + (t - m) * a;
    let c = c + (m - c) * a;
    let eps = if log { 1e-4 } else { 1e-3 };
    if (t - c).abs() < eps && (t - m).abs() < eps {
        (target, target)
    } else if log {
        (c.exp(), m.exp())
    } else {
        (c, m)
    }
}

#[inline(always)]
fn clean(v: f32) -> f32 {
    // denormal range or non-finite -> 0
    if v.is_finite() && v.abs() >= 1e-20 { v } else { 0.0 }
}

impl EqRt {
    pub fn new(sample_rate: u32) -> EqRt {
        let b = DEFAULT_BANDS.map(|d| BandRt {
            kind: d.kind,
            freq: d.freq,
            gain_db: d.gain_db,
            q: d.q,
            mid: [d.freq, d.gain_db, d.q],
            wet: 0.0,
            c: Coefs::default(),
            s: [[0.0; 2]; 2],
        });
        let mut eq = EqRt { b, sr: 48_000.0, glide: 0.0, fade: 0.0 };
        eq.set_sample_rate(sample_rate);
        eq
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.sr = sample_rate.max(8000) as f64;
        let block = SUB as f64;
        self.glide = (1.0 - (-block / (GLIDE_S * self.sr)).exp()) as f32;
        self.fade = (block / (FADE_S * self.sr)).min(1.0) as f32;
        for b in &mut self.b {
            b.c = coefs(b.kind, b.freq, b.gain_db, b.q, self.sr);
        }
    }

    /// Jump to the target settings with no fades and empty filters (the
    /// channel was silent, so nothing can be heard).
    pub fn reset(&mut self, p: &EqParams) {
        let on = p.on.load(Relaxed);
        for (k, b) in self.b.iter_mut().enumerate() {
            let t = p.bands[k].load().sanitized(&DEFAULT_BANDS[k]);
            if (b.kind, b.freq, b.gain_db, b.q) != (t.kind, t.freq, t.gain_db, t.q) || b.c == Coefs::default() {
                (b.kind, b.freq, b.gain_db, b.q) = (t.kind, t.freq, t.gain_db, t.q);
                b.c = coefs(b.kind, b.freq, b.gain_db, b.q, self.sr);
            }
            b.mid = [t.freq, t.gain_db, t.q];
            b.wet = if on && t.on { 1.0 } else { 0.0 };
            b.s = [[0.0; 2]; 2];
        }
    }

    /// Filter one cycle of a stereo channel in place.
    pub fn process(&mut self, p: &EqParams, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let on = p.on.load(Relaxed);
        let targets: [Band; BANDS] = std::array::from_fn(|k| p.bands[k].load().sanitized(&DEFAULT_BANDS[k]));
        // Fast path: every band bypassed and staying bypassed.
        if self.b.iter().zip(&targets).all(|(b, t)| b.wet == 0.0 && !(on && t.on)) {
            return;
        }
        let mut i0 = 0;
        while i0 < n {
            let len = SUB.min(n - i0);
            let (lb, rb) = (&mut l[i0..i0 + len], &mut r[i0..i0 + len]);
            for (b, t) in self.b.iter_mut().zip(&targets) {
                if b.wet == 0.0 {
                    // Inaudible: take the new settings at once.
                    if (b.kind, b.freq, b.gain_db, b.q) != (t.kind, t.freq, t.gain_db, t.q) {
                        (b.kind, b.freq, b.gain_db, b.q) = (t.kind, t.freq, t.gain_db, t.q);
                        b.c = coefs(b.kind, b.freq, b.gain_db, b.q, self.sr);
                    }
                    b.mid = [t.freq, t.gain_db, t.q];
                    b.s = [[0.0; 2]; 2];
                }
                // A new type fades the old one out first.
                let switching = b.kind != t.kind;
                let target_wet = if on && t.on && !switching { 1.0 } else { 0.0 };
                if b.wet == 0.0 && target_wet == 0.0 {
                    continue;
                }
                // Glide towards the target; the coefficients move from `c0`
                // to `c1` sample by sample across this sub-block.
                let c0 = b.c;
                if !switching {
                    let (f, mf) = glide(b.freq, b.mid[0], t.freq, self.glide, true);
                    let (g, mg) = glide(b.gain_db, b.mid[1], t.gain_db, self.glide, false);
                    let (q, mq) = glide(b.q, b.mid[2], t.q, self.glide, true);
                    b.mid = [mf, mg, mq];
                    if (f, g, q) != (b.freq, b.gain_db, b.q) {
                        (b.freq, b.gain_db, b.q) = (f, g, q);
                        b.c = coefs(b.kind, f, g, q, self.sr);
                    }
                }
                let w0 = b.wet;
                let w1 = move_toward(w0, target_wet, self.fade);
                b.wet = w1;
                if b.kind.has_gain() && b.gain_db == 0.0 && c0 == b.c {
                    // Exactly the identity: nothing to compute.
                    b.s = [[0.0; 2]; 2];
                    continue;
                }
                let c1 = b.c;
                run_band(&mut b.s, lb, rb, w0, w1, &c0, &c1);
            }
            i0 += len;
        }
    }

    /// True if any band is (or is fading) in the signal path.
    pub fn active(&self) -> bool {
        self.b.iter().any(|b| b.wet > 0.0)
    }
}

/// One SVF step: returns the output, updates the state.
#[inline(always)]
fn svf(c: &Coefs, ic1: &mut f32, ic2: &mut f32, v0: f32) -> f32 {
    let v3 = v0 - *ic2;
    let v1 = c.a1 * *ic1 + c.a2 * v3;
    let v2 = *ic2 + c.a2 * *ic1 + c.a3 * v3;
    *ic1 = 2.0 * v1 - *ic1;
    *ic2 = 2.0 * v2 - *ic2;
    c.m0 * v0 + c.m1 * v1 + c.m2 * v2
}

/// Run one band over a sub-block: wet `w0 -> w1`, coefficients `c0 -> c1`.
#[inline]
fn run_band(s: &mut [[f32; 2]; 2], l: &mut [f32], r: &mut [f32], w0: f32, w1: f32, c0: &Coefs, c1: &Coefs) {
    let len = l.len();
    let steady = c0 == c1;
    for (ch, x) in [l, r].into_iter().enumerate() {
        let [mut ic1, mut ic2] = s[ch];
        if steady && w0 == 1.0 && w1 == 1.0 {
            for v in x.iter_mut() {
                *v = svf(c1, &mut ic1, &mut ic2, *v);
            }
        } else if steady {
            for (i, v) in x.iter_mut().enumerate() {
                let v0 = *v;
                let y = svf(c1, &mut ic1, &mut ic2, v0);
                *v = v0 + ramp(w0, w1, i, len) * (y - v0);
            }
        } else {
            for (i, v) in x.iter_mut().enumerate() {
                let t = if i + 1 >= len { 1.0 } else { (i + 1) as f32 / len as f32 };
                let c = if t == 1.0 { *c1 } else { c0.lerp(c1, t) };
                let v0 = *v;
                let y = svf(&c, &mut ic1, &mut ic2, v0);
                *v = v0 + ramp(w0, w1, i, len) * (y - v0);
            }
        }
        s[ch] = [clean(ic1), clean(ic2)];
    }
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn params(on: bool, bands: &[Band; BANDS]) -> EqParams {
        let p = EqParams::default();
        p.on.store(on, Relaxed);
        p.store_bands(bands);
        p
    }

    fn only(b: Band) -> [Band; BANDS] {
        let mut bands = DEFAULT_BANDS;
        bands[2] = b;
        bands
    }

    /// Magnitude (dB) of a real impulse response at `f` Hz.
    fn dft_db(h: &[f32], sr: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / sr;
        let (cw, sw) = (w.cos(), -w.sin());
        // rotating phasor, renormalized now and then
        let (mut pr, mut pi) = (1.0f64, 0.0f64);
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &x) in h.iter().enumerate() {
            re += x as f64 * pr;
            im += x as f64 * pi;
            (pr, pi) = (pr * cw - pi * sw, pr * sw + pi * cw);
            if i % 4096 == 4095 {
                let m = (pr * pr + pi * pi).sqrt();
                (pr, pi) = (pr / m, pi / m);
            }
        }
        10.0 * (re * re + im * im).log10()
    }

    /// Long enough for the band's ringing to decay by 20 time constants.
    fn ir_len(b: &Band, sr: u32) -> usize {
        let a = 10f64.powf(b.gain_db as f64 / 40.0);
        let q_pole = b.q as f64 * if b.kind == Kind::Bell { a.max(1.0) } else { 1.0 };
        let tau = q_pole * sr as f64 / (PI * b.freq as f64);
        ((20.0 * tau) as usize).next_power_of_two().clamp(1 << 16, 1 << 22)
    }

    fn impulse_response(eq: &mut EqRt, p: &EqParams, len: usize) -> Vec<f32> {
        let mut l = vec![0.0f32; len];
        let mut r = vec![0.0f32; len];
        l[0] = 1.0;
        r[0] = 1.0;
        eq.process(p, &mut l, &mut r);
        assert_eq!(l, r, "both channels get the same filter");
        l
    }

    #[test]
    fn kinds_and_bands_parse_and_format() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()).unwrap(), k);
            assert_eq!(Kind::from_u8(k as u8), k);
        }
        assert_eq!(Kind::parse(" HPF ").unwrap(), Kind::LowCut);
        assert_eq!(Kind::parse("peak").unwrap(), Kind::Bell);
        assert!(Kind::parse("comb").is_err());
        assert_eq!(Kind::from_u8(200), Kind::Bell);
        for b in DEFAULT_BANDS {
            assert_eq!(Band::parse(&b.to_config()).unwrap(), b);
        }
        let b = Band { on: false, kind: Kind::LowCut, freq: 80.0, gain_db: -3.5, q: 0.71 };
        assert_eq!(b.to_config(), "lowcut 80 -3.5 0.71 off");
        assert_eq!(Band::parse("lowcut 80 -3.5 0.71 off").unwrap(), b);
        assert_eq!(
            Band::parse("bell 1000.4 +3.04 1.414 ON").unwrap(),
            Band { on: true, kind: Kind::Bell, freq: 1000.0, gain_db: 3.0, q: 1.41 }
        );
        for bad in [
            "bell 1000 3",
            "bell 10 0 1",
            "bell 1000 30 1",
            "bell 1000 0 0",
            "bell 1000 0 1 maybe",
            "bell x 0 1",
            "bell 1000 NaN 1",
            "x 1 2 3 4 5",
        ] {
            assert!(Band::parse(bad).is_err(), "{bad}");
        }
        let wild = Band { on: true, kind: Kind::Bell, freq: f32::NAN, gain_db: 99.0, q: 0.0 };
        let s = wild.sanitized(&DEFAULT_BANDS[3]);
        assert_eq!((s.freq, s.gain_db, s.q), (1500.0, MAX_GAIN_DB, MIN_Q));
    }

    #[test]
    fn svf_matches_the_rbj_cookbook() {
        let cases = [
            Band { on: true, kind: Kind::Bell, freq: 1000.0, gain_db: 6.0, q: 1.0 },
            Band { on: true, kind: Kind::Bell, freq: 60.0, gain_db: -12.0, q: 4.0 },
            Band { on: true, kind: Kind::Bell, freq: 12000.0, gain_db: 24.0, q: 0.5 },
            Band { on: true, kind: Kind::LowShelf, freq: 120.0, gain_db: 9.0, q: 0.71 },
            Band { on: true, kind: Kind::LowShelf, freq: 400.0, gain_db: -15.0, q: 1.2 },
            Band { on: true, kind: Kind::HighShelf, freq: 8000.0, gain_db: -6.0, q: 0.71 },
            Band { on: true, kind: Kind::HighShelf, freq: 3000.0, gain_db: 12.0, q: 2.0 },
            Band { on: true, kind: Kind::LowCut, freq: 80.0, gain_db: 0.0, q: 0.71 },
            Band { on: true, kind: Kind::HighCut, freq: 5000.0, gain_db: 0.0, q: 0.71 },
            Band { on: true, kind: Kind::Notch, freq: 50.0, gain_db: 0.0, q: 10.0 },
            Band { on: true, kind: Kind::Bell, freq: 20.0, gain_db: 18.0, q: 30.0 },
            Band { on: true, kind: Kind::Bell, freq: 20000.0, gain_db: -24.0, q: 0.1 },
        ];
        for sr in [44_100u32, 48_000, 96_000] {
            for b in cases {
                let p = params(true, &only(b));
                let mut eq = EqRt::new(sr);
                eq.reset(&p);
                let h = impulse_response(&mut eq, &p, ir_len(&b, sr));
                for f in [20.0, 31.5, 50.0, 63.0, 100.0, 250.0, 1000.0, 2500.0, 6300.0, 10000.0, 16000.0, 19999.0] {
                    let want = band_db(&b, sr as f64, f);
                    let got = dft_db(&h, sr as f64, f);
                    // deep notches/cuts: compare only where the level is measurable
                    if want > -60.0 {
                        assert!((want - got).abs() < 0.02, "{b:?} @{sr} {f} Hz: want {want:.3} got {got:.3}");
                    } else {
                        assert!(got < -50.0, "{b:?} @{sr} {f} Hz: want {want:.1} got {got:.1}");
                    }
                }
            }
        }
    }

    #[test]
    fn rbj_reference_values() {
        let sr = 48_000.0;
        let bell = Band { on: true, kind: Kind::Bell, freq: 1000.0, gain_db: 6.0, q: 1.0 };
        assert!((band_db(&bell, sr, 1000.0) - 6.0).abs() < 1e-9);
        assert!(band_db(&bell, sr, 20.0).abs() < 0.05);
        let ls = Band { on: true, kind: Kind::LowShelf, freq: 200.0, gain_db: 10.0, q: 0.71 };
        assert!((band_db(&ls, sr, 200.0) - 5.0).abs() < 1e-6, "shelf is at half gain at its frequency");
        assert!((band_db(&ls, sr, 20.0) - 10.0).abs() < 0.3);
        let hp = Band { on: true, kind: Kind::LowCut, freq: 100.0, gain_db: 0.0, q: std::f32::consts::FRAC_1_SQRT_2 };
        assert!((band_db(&hp, sr, 100.0) + 3.0103).abs() < 1e-3, "Butterworth -3 dB point");
        assert!((band_db(&hp, sr, 50.0) + 12.3).abs() < 0.1, "12 dB/octave");
    }

    #[test]
    fn off_and_flat_are_bit_exact() {
        let input: Vec<f32> = (0..1000).map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0).collect();
        for (on, bands) in [
            (false, DEFAULT_BANDS),
            (true, DEFAULT_BANDS),
            (false, only(Band { on: true, kind: Kind::LowCut, freq: 200.0, gain_db: 0.0, q: 0.71 })),
        ] {
            let p = params(on, &bands);
            let mut eq = EqRt::new(48_000);
            for _ in 0..20 {
                let (mut l, mut r) = (input.clone(), input.clone());
                eq.process(&p, &mut l, &mut r);
                assert_eq!(l, input);
                assert_eq!(r, input);
            }
        }
        // a band that is off does nothing either
        let mut b = only(Band { on: false, kind: Kind::Bell, freq: 200.0, gain_db: 12.0, q: 1.0 });
        b[0].gain_db = 0.0;
        let p = params(true, &b);
        let mut eq = EqRt::new(48_000);
        let (mut l, mut r) = (input.clone(), input.clone());
        eq.process(&p, &mut l, &mut r);
        assert_eq!(l, input);
    }

    /// Largest second difference of a signal: a smooth sine has a small
    /// one, a click or step a large one.
    fn roughness(x: &[f32]) -> f32 {
        x.windows(3).map(|w| (w[2] - 2.0 * w[1] + w[0]).abs()).fold(0.0, f32::max)
    }

    fn run_sine(eq: &mut EqRt, p: &EqParams, start: usize, n: usize, out: &mut Vec<f32>) {
        let mut l: Vec<f32> = (start..start + n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut r = l.clone();
        eq.process(p, &mut l, &mut r);
        out.extend_from_slice(&l);
    }

    #[test]
    fn switching_fading_and_gliding_are_click_free() {
        let boost = Band { on: true, kind: Kind::Bell, freq: 100.0, gain_db: 18.0, q: 2.0 };
        let p = params(false, &only(boost));
        let mut eq = EqRt::new(48_000);
        let mut out = Vec::new();
        let mut t = 0;
        let mut step = |eq: &mut EqRt, p: &EqParams, out: &mut Vec<f32>| {
            for n in [128usize, 128, 64, 256, 128, 1024, 17, 128] {
                run_sine(eq, p, t, n, out);
                t += n;
            }
        };
        step(&mut eq, &p, &mut out);
        p.on.store(true, Relaxed); // EQ on
        step(&mut eq, &p, &mut out);
        p.bands[2].kind.store(Kind::LowShelf as u8, Relaxed); // type change
        p.bands[2].q.store(0.71); // (a Q of 2 would give the shelf a +5 dB bump)
        step(&mut eq, &p, &mut out);
        p.bands[2].freq.store(1000.0); // big frequency jump
        step(&mut eq, &p, &mut out);
        p.bands[2].gain_db.store(-24.0); // big gain jump
        step(&mut eq, &p, &mut out);
        p.bands[2].on.store(false, Relaxed); // band off
        step(&mut eq, &p, &mut out);
        p.bands[2].on.store(true, Relaxed);
        step(&mut eq, &p, &mut out);
        p.on.store(false, Relaxed); // EQ off
        step(&mut eq, &p, &mut out);
        // A 100 Hz sine at up to 8x (+18 dB) has a first difference of at
        // most 4 * 2 pi 100 / 48000 = 0.052 and a second difference of
        // 7e-4. Gain moves add a little (a 10 ms cross-fade between dry and
        // +18 dB bends the slope by ~0.007); a click is a step of 0.1 or more.
        let slope = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(slope < 0.07, "slope {slope}");
        assert!(roughness(&out) < 0.012, "roughness {}", roughness(&out));
        assert!(!eq.active(), "fully bypassed again");
        // and it really boosted while on
        let peak_on = out[3200..3746].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak_on > 3.3, "+18 dB bell at 100 Hz: peak {peak_on}");
    }

    #[test]
    fn settles_exactly_on_the_target() {
        let b = Band { on: true, kind: Kind::HighShelf, freq: 5000.0, gain_db: -9.0, q: 0.71 };
        let p = params(true, &only(Band { kind: Kind::HighShelf, ..DEFAULT_BANDS[2] }));
        let mut eq = EqRt::new(48_000);
        let mut sink = Vec::new();
        run_sine(&mut eq, &p, 0, 4800, &mut sink);
        p.bands[2].store(&b);
        run_sine(&mut eq, &p, 0, 48_000, &mut sink); // one second: long settled
        assert_eq!((eq.b[2].freq, eq.b[2].gain_db, eq.b[2].q, eq.b[2].wet), (5000.0, -9.0, 0.71, 1.0));
        assert_eq!(eq.b[2].c, coefs(Kind::HighShelf, 5000.0, -9.0, 0.71, 48_000.0));
        eq.reset(&p); // empty filters (the settings are already the targets)
        assert_eq!(eq.b[2].c, coefs(Kind::HighShelf, 5000.0, -9.0, 0.71, 48_000.0));
        let h = impulse_response(&mut eq, &p, 1 << 15);
        assert!((dft_db(&h, 48_000.0, 15000.0) - band_db(&b, 48_000.0, 15000.0)).abs() < 0.05);
    }

    #[test]
    fn silence_never_leaves_denormals_or_nan() {
        let p = params(true, &only(Band { on: true, kind: Kind::Bell, freq: 30.0, gain_db: 12.0, q: 20.0 }));
        let mut eq = EqRt::new(48_000);
        let mut l = vec![1.0f32; 256];
        let mut r = vec![-1.0f32; 256];
        eq.process(&p, &mut l, &mut r);
        for _ in 0..20_000 {
            let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
            eq.process(&p, &mut l, &mut r);
            assert!(l.iter().chain(&r).all(|v| v.is_finite() && (*v == 0.0 || v.abs() >= f32::MIN_POSITIVE)));
        }
        for b in &eq.b {
            for s in b.s.iter().flatten() {
                assert!(*s == 0.0 || s.abs() >= 1e-20, "state {s}");
            }
        }
        // a NaN parameter (cannot come from the UIs, but anyway) is ignored
        p.bands[2].freq.store(f32::NAN);
        p.bands[2].q.store(f32::INFINITY);
        let (mut l, mut r) = (vec![0.3f32; 512], vec![0.3f32; 512]);
        eq.process(&p, &mut l, &mut r);
        assert!(l.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn reset_snaps_without_fades() {
        let p = params(true, &only(Band { on: true, kind: Kind::Notch, freq: 1000.0, gain_db: 0.0, q: 5.0 }));
        let mut eq = EqRt::new(48_000);
        eq.reset(&p);
        assert_eq!(eq.b[2].wet, 1.0);
        assert_eq!(eq.b[0].wet, 1.0, "flat bands are 'on' too, they just compute nothing");
        p.on.store(false, Relaxed);
        eq.reset(&p);
        assert!(!eq.active());
    }
}
