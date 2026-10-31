//! Noise gate and compressor on every input strip.
//!
//! Both work on one gain per strip, the same for left and right (stereo
//! linked), worked out from the louder side after mono and the input gain.
//! They sit before the EQ, so their thresholds read the level the GAIN knob
//! sets, and the EQ shapes what is left.
//!
//! * **Gate.** Opens when the level goes over its threshold (fading in over
//!   the attack time), stays open while it stays above a point 4 dB lower
//!   (so it does not chatter on a level near the threshold), then waits the
//!   hold time and fades down to its range over the release time. A range of
//!   -80 dB closes it fully; -10 dB only turns the background down.
//! * **Compressor.** Feed-forward, peak detecting, with a soft knee: above
//!   the threshold the level rises only 1/ratio as fast. The gain reduction
//!   follows the level with the attack time and lets go with the release time
//!   (smoothed in dB, so it never pumps unevenly); the make-up gain is added
//!   after it.
//!
//! Real-time behaviour (see [`DynRt::process`]):
//! * **Transparent when off.** With both off and settled, the signal passes
//!   bit for bit and nothing is computed.
//! * **Click-free.** Switching either one on or off, or moving the make-up
//!   gain, glides over 10 ms; every other setting acts through the attack and
//!   release smoothing.
//! * **No added latency** (no look-ahead) and **real-time safe**: no
//!   allocation, locks or syscalls; parameters are atomics read once per
//!   cycle; the state never becomes non-finite.

use crate::engine::AtomicF32;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One setting: config key, name for people, unit, range, default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub def: f32,
}

const fn spec(key: &'static str, label: &'static str, unit: &'static str, min: f32, max: f32, def: f32) -> Spec {
    Spec { key, label, unit, min, max, def }
}

pub const GATE_N: usize = 5;
pub const G_THRESHOLD: usize = 0;
pub const G_RANGE: usize = 1;
pub const G_ATTACK: usize = 2;
pub const G_HOLD: usize = 3;
pub const G_RELEASE: usize = 4;

/// The gate's settings, in this order everywhere (config, GUI protocol).
pub const GATE: [Spec; GATE_N] = [
    spec("gate_threshold", "Threshold", "dB", -80.0, 0.0, -50.0),
    spec("gate_range", "Range", "dB", -80.0, 0.0, -80.0),
    spec("gate_attack", "Attack", "ms", 0.1, 50.0, 1.0),
    spec("gate_hold", "Hold", "ms", 0.0, 2000.0, 100.0),
    spec("gate_release", "Release", "ms", 5.0, 4000.0, 200.0),
];

pub const COMP_N: usize = 6;
pub const C_THRESHOLD: usize = 0;
pub const C_RATIO: usize = 1;
pub const C_KNEE: usize = 2;
pub const C_ATTACK: usize = 3;
pub const C_RELEASE: usize = 4;
pub const C_MAKEUP: usize = 5;

/// The compressor's settings, in this order everywhere.
pub const COMP: [Spec; COMP_N] = [
    spec("comp_threshold", "Threshold", "dB", -60.0, 0.0, -18.0),
    spec("comp_ratio", "Ratio", ":1", 1.0, 20.0, 3.0),
    spec("comp_knee", "Knee", "dB", 0.0, 24.0, 6.0),
    spec("comp_attack", "Attack", "ms", 0.1, 200.0, 10.0),
    spec("comp_release", "Release", "ms", 10.0, 4000.0, 150.0),
    spec("comp_makeup", "Make-up", "dB", 0.0, 24.0, 0.0),
];

/// How far below its threshold the gate closes again.
pub const GATE_HYSTERESIS_DB: f32 = 4.0;

/// Settings are stored rounded to 0.1.
pub fn round(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

impl Spec {
    /// In range and rounded; a non-finite value becomes the default.
    pub fn clamp(&self, v: f32) -> f32 {
        if v.is_finite() { round(v.clamp(self.min, self.max)) } else { self.def }
    }

    /// A config value: a number in range (a typo must not silently become
    /// something else).
    pub fn parse(&self, v: &str) -> Result<f32, String> {
        let s = v.trim();
        let x: f32 = s.parse().map_err(|_| format!("{} must be a number, got {s:?}", self.key))?;
        if !x.is_finite() || x < self.min || x > self.max {
            return Err(format!("{} {s} is out of range ({} to {} {})", self.key, self.min, self.max, self.unit));
        }
        Ok(round(x))
    }
}

/// One strip's gate and compressor as stored in the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct DynSettings {
    pub gate_on: bool,
    pub gate: [f32; GATE_N],
    pub comp_on: bool,
    pub comp: [f32; COMP_N],
}

impl Default for DynSettings {
    fn default() -> Self {
        DynSettings { gate_on: false, gate: GATE.map(|s| s.def), comp_on: false, comp: COMP.map(|s| s.def) }
    }
}

/// One strip's gate and compressor, shared with the audio thread.
#[derive(Debug)]
pub struct DynParams {
    pub gate_on: AtomicBool,
    pub gate: [AtomicF32; GATE_N],
    pub comp_on: AtomicBool,
    pub comp: [AtomicF32; COMP_N],
    /// The gate's gain in the last cycle at its lowest, dB (0 = open).
    pub gate_gr: AtomicF32,
    /// The compressor's gain reduction in the last cycle at its deepest,
    /// dB (0 = none; the make-up gain is not counted).
    pub comp_gr: AtomicF32,
}

impl Default for DynParams {
    fn default() -> Self {
        DynParams {
            gate_on: AtomicBool::new(false),
            gate: GATE.map(|s| AtomicF32::new(s.def)),
            comp_on: AtomicBool::new(false),
            comp: COMP.map(|s| AtomicF32::new(s.def)),
            gate_gr: AtomicF32::new(0.0),
            comp_gr: AtomicF32::new(0.0),
        }
    }
}

impl DynParams {
    pub fn settings(&self) -> DynSettings {
        DynSettings {
            gate_on: self.gate_on.load(Relaxed),
            gate: std::array::from_fn(|i| self.gate[i].load()),
            comp_on: self.comp_on.load(Relaxed),
            comp: std::array::from_fn(|i| self.comp[i].load()),
        }
    }

    pub fn apply(&self, s: &DynSettings) {
        for (i, a) in self.gate.iter().enumerate() {
            a.store(GATE[i].clamp(s.gate[i]));
        }
        for (i, a) in self.comp.iter().enumerate() {
            a.store(COMP[i].clamp(s.comp[i]));
        }
        self.gate_on.store(s.gate_on, Relaxed);
        self.comp_on.store(s.comp_on, Relaxed);
    }

    /// Every setting back to its default; the on/off switches stay.
    pub fn reset_settings(&self) {
        for (a, s) in self.gate.iter().zip(&GATE) {
            a.store(s.def);
        }
        for (a, s) in self.comp.iter().zip(&COMP) {
            a.store(s.def);
        }
    }
}

// ---------------------------------------------------------------------------
// Signal processing
// ---------------------------------------------------------------------------

/// Length of the on/off and make-up glides.
const FADE_S: f32 = 0.010;
/// Release of the gate's level detector (between two peaks of a low note).
const GATE_DETECT_S: f32 = 0.020;
/// Level the detectors treat as silence.
const FLOOR: f32 = 1e-9;

#[inline(always)]
fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline(always)]
fn to_db(x: f32) -> f32 {
    20.0 * x.max(FLOOR).log10()
}

/// One-pole coefficient for time constant `secs`.
#[inline]
fn coef(secs: f32, sr: f32) -> f32 {
    if secs <= 0.0 { 0.0 } else { (-1.0 / (secs * sr)).exp() }
}

/// Static curve of the compressor: gain change in dB (≤ 0) for a level `x`
/// in dB, with a soft knee of width `knee` around the threshold `t`.
#[inline]
pub fn comp_curve(x: f32, t: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - t;
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    if 2.0 * over <= -knee {
        0.0
    } else if knee > 0.0 && 2.0 * over < knee {
        let k = over + knee / 2.0;
        slope * k * k / (2.0 * knee)
    } else {
        slope * over
    }
}

#[inline(always)]
fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

/// Per strip state of the audio thread.
#[derive(Debug, Clone)]
pub struct DynRt {
    sr: f32,
    /// Gate gain, linear (1 = open).
    gate_g: f32,
    /// Gate level detector, linear.
    gate_env: f32,
    gate_open: bool,
    /// Samples of hold left.
    hold_left: u32,
    /// Compressor gain change, dB (≤ 0).
    comp_db: f32,
    /// Make-up gain now, dB (glides to the setting, or to 0 when off).
    makeup: f32,
}

impl DynRt {
    pub fn new(sample_rate: u32) -> DynRt {
        let mut d = DynRt {
            sr: 48_000.0,
            gate_g: 1.0,
            gate_env: 0.0,
            gate_open: false,
            hold_left: 0,
            comp_db: 0.0,
            makeup: 0.0,
        };
        d.set_sample_rate(sample_rate);
        d
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.sr = sample_rate.max(8000) as f32;
    }

    /// Settled with both off: the signal passes untouched.
    #[inline]
    fn idle(&self) -> bool {
        self.gate_g == 1.0 && self.comp_db == 0.0 && self.makeup == 0.0
    }

    /// Jump to the settings with no glides (the strip was silent, so
    /// nothing can be heard): a gate that is on starts closed.
    pub fn reset(&mut self, p: &DynParams) {
        let gate_on = p.gate_on.load(Relaxed);
        let comp_on = p.comp_on.load(Relaxed);
        self.gate_g = if gate_on { db_to_gain(GATE[G_RANGE].clamp(p.gate[G_RANGE].load())) } else { 1.0 };
        self.gate_env = 0.0;
        self.gate_open = false;
        self.hold_left = 0;
        self.comp_db = 0.0;
        self.makeup = if comp_on { COMP[C_MAKEUP].clamp(p.comp[C_MAKEUP].load()) } else { 0.0 };
        p.gate_gr.store(if gate_on { to_db(self.gate_g) } else { 0.0 });
        p.comp_gr.store(0.0);
    }

    /// Gate and compress one cycle of a stereo strip in place. `trim` is
    /// the strip's input gain (linear), applied later with the fader: the
    /// detectors read the level as if it were already applied.
    pub fn process(&mut self, p: &DynParams, l: &mut [f32], r: &mut [f32], trim: f32) {
        let n = l.len().min(r.len());
        let gate_on = p.gate_on.load(Relaxed);
        let comp_on = p.comp_on.load(Relaxed);
        if !gate_on && !comp_on && self.idle() {
            p.gate_gr.store(0.0);
            p.comp_gr.store(0.0);
            return;
        }
        let sr = self.sr;
        let gs: [f32; GATE_N] = std::array::from_fn(|i| GATE[i].clamp(p.gate[i].load()));
        let cs: [f32; COMP_N] = std::array::from_fn(|i| COMP[i].clamp(p.comp[i].load()));

        // gate
        let open_at = db_to_gain(gs[G_THRESHOLD]);
        let close_at = db_to_gain(gs[G_THRESHOLD] - GATE_HYSTERESIS_DB);
        let range = db_to_gain(gs[G_RANGE]);
        let g_att = coef(gs[G_ATTACK] / 1000.0, sr);
        let g_rel = coef(gs[G_RELEASE] / 1000.0, sr);
        let hold = (gs[G_HOLD] / 1000.0 * sr) as u32;
        let detect = coef(GATE_DETECT_S, sr);
        let fade = coef(FADE_S, sr);
        // compressor
        let (t, ratio, knee) = (cs[C_THRESHOLD], cs[C_RATIO], cs[C_KNEE]);
        let c_att = coef(cs[C_ATTACK] / 1000.0, sr);
        let c_rel = coef(cs[C_RELEASE] / 1000.0, sr);
        let makeup_target = if comp_on { cs[C_MAKEUP] } else { 0.0 };
        let trim = finite_or(trim, 1.0).abs();

        let (mut gate_min, mut comp_min) = (1.0f32, 0.0f32);
        for i in 0..n {
            let x = l[i].abs().max(r[i].abs()) * trim;

            // ---- gate: target gain, then a glide towards it
            let target = if gate_on {
                self.gate_env = x.max(self.gate_env * detect);
                if self.gate_env >= open_at {
                    self.gate_open = true;
                    self.hold_left = hold;
                } else if self.gate_open && self.gate_env < close_at {
                    if self.hold_left > 0 {
                        self.hold_left -= 1;
                    } else {
                        self.gate_open = false;
                    }
                } else if self.gate_open {
                    self.hold_left = hold;
                }
                if self.gate_open { 1.0 } else { range }
            } else {
                1.0
            };
            if self.gate_g != target {
                let a = if !gate_on {
                    fade
                } else if target > self.gate_g {
                    g_att
                } else {
                    g_rel
                };
                self.gate_g = target + (self.gate_g - target) * a;
                // (1e-5 is -0.0001 dB; closer, f32 steps near 1.0 can stall the glide)
                if (self.gate_g - target).abs() < 1e-5 {
                    self.gate_g = target;
                }
            }
            gate_min = gate_min.min(self.gate_g);

            // ---- compressor: gain change in dB, smoothed
            let want = if comp_on { comp_curve(to_db(x), t, ratio, knee) } else { 0.0 };
            if self.comp_db != want {
                let a = if !comp_on {
                    fade
                } else if want < self.comp_db {
                    c_att
                } else {
                    c_rel
                };
                self.comp_db = want + (self.comp_db - want) * a;
                if (self.comp_db - want).abs() < 1e-4 {
                    self.comp_db = want;
                }
            }
            comp_min = comp_min.min(self.comp_db);
            if self.makeup != makeup_target {
                self.makeup = makeup_target + (self.makeup - makeup_target) * fade;
                if (self.makeup - makeup_target).abs() < 1e-4 {
                    self.makeup = makeup_target;
                }
            }

            let db = self.comp_db + self.makeup;
            let g = if db == 0.0 { self.gate_g } else { self.gate_g * db_to_gain(db) };
            l[i] *= g;
            r[i] *= g;
        }
        // never let a bad value stick
        self.gate_g = finite_or(self.gate_g, 1.0).clamp(0.0, 1.0);
        self.gate_env = finite_or(self.gate_env, 0.0).min(f32::MAX);
        self.comp_db = finite_or(self.comp_db, 0.0).min(0.0);
        self.makeup = finite_or(self.makeup, 0.0);
        p.gate_gr.store(to_db(gate_min).max(-120.0));
        p.comp_gr.store(comp_min.max(-120.0));
    }

    /// True if the gate or the compressor is (or is gliding) in the path.
    pub fn active(&self) -> bool {
        !self.idle()
    }
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn run(d: &mut DynRt, p: &DynParams, level: f32, frames: usize) -> Vec<f32> {
        let mut l = vec![level; frames];
        let mut r = vec![-level; frames];
        let mut i0 = 0;
        while i0 < frames {
            let len = 128.min(frames - i0);
            d.process(p, &mut l[i0..i0 + len], &mut r[i0..i0 + len], 1.0);
            i0 += len;
        }
        assert_eq!(l.iter().map(|x| -x).collect::<Vec<_>>(), r, "left and right get the same gain");
        l
    }

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn off_is_bit_exact() {
        let p = DynParams::default();
        let mut d = DynRt::new(SR);
        let mut l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.37).sin() * 0.9).collect();
        let mut r: Vec<f32> = (0..256).map(|i| (i as f32 * 0.11).cos() * 0.2).collect();
        let (l0, r0) = (l.clone(), r.clone());
        d.process(&p, &mut l, &mut r, 3.0);
        assert_eq!((l, r), (l0, r0));
        assert!(!d.active());
    }

    #[test]
    fn compressor_follows_its_curve_and_adds_makeup() {
        let p = DynParams::default();
        p.comp_on.store(true, Relaxed);
        p.comp[C_THRESHOLD].store(-20.0);
        p.comp[C_RATIO].store(4.0);
        p.comp[C_KNEE].store(0.0);
        let mut d = DynRt::new(SR);
        // a steady -8 dBFS level: 12 dB over, 4:1 -> 3 dB over, so -9 dB
        let x = db_to_gain(-8.0);
        let out = run(&mut d, &p, x, SR as usize / 2);
        let got = to_db(*out.last().unwrap());
        assert!(approx(got, -17.0, 0.05), "{got}");
        assert!(approx(p.comp_gr.load(), -9.0, 0.05), "{}", p.comp_gr.load());
        // below the threshold nothing changes (after the release)
        let out = run(&mut d, &p, db_to_gain(-30.0), 3 * SR as usize);
        assert!(approx(to_db(*out.last().unwrap()), -30.0, 0.01));
        // make-up gain glides in
        p.comp[C_MAKEUP].store(6.0);
        let out = run(&mut d, &p, db_to_gain(-30.0), SR as usize / 10);
        assert!(approx(to_db(*out.last().unwrap()), -24.0, 0.01));
        assert!(to_db(out[0]) < -29.0, "no step: {}", to_db(out[0]));
    }

    #[test]
    fn the_input_gain_counts_for_the_threshold() {
        let p = DynParams::default();
        p.comp_on.store(true, Relaxed);
        p.comp[C_KNEE].store(0.0);
        let mut d = DynRt::new(SR);
        let x = db_to_gain(-25.0); // -19 dB with +6 dB of input gain: under the threshold
        let mut l = vec![x; 4800];
        let mut r = l.clone();
        d.process(&p, &mut l, &mut r, db_to_gain(6.0));
        assert_eq!(*l.last().unwrap(), x);
        let mut l = vec![x; 48_000];
        let mut r = l.clone();
        d.process(&p, &mut l, &mut r, db_to_gain(19.0)); // 12 over at 3:1 -> -8 dB
        assert!(approx(to_db(*l.last().unwrap()) - to_db(x), -8.0, 0.05));
    }

    #[test]
    fn soft_knee_is_continuous() {
        let (t, ratio, knee) = (-20.0, 4.0, 6.0);
        assert_eq!(comp_curve(-30.0, t, ratio, knee), 0.0);
        assert_eq!(comp_curve(-23.0, t, ratio, knee), 0.0);
        assert!(approx(comp_curve(-17.0, t, ratio, knee), -0.75 * 3.0, 1e-5));
        let mut prev = 0.0;
        for i in 0..=400 {
            let x = -40.0 + i as f32 * 0.1;
            let y = comp_curve(x, t, ratio, knee);
            assert!(y <= 0.0 && y <= prev + 1e-6 && (prev - y) < 0.1, "x {x}: {y}");
            prev = y;
        }
        assert_eq!(comp_curve(0.0, -10.0, 1.0, 0.0), 0.0, "1:1 does nothing");
    }

    #[test]
    fn gate_closes_after_hold_and_opens_on_sound() {
        let p = DynParams::default();
        p.gate_on.store(true, Relaxed);
        p.gate[G_THRESHOLD].store(-40.0);
        p.gate[G_RANGE].store(-60.0);
        p.gate[G_HOLD].store(50.0);
        p.gate[G_RELEASE].store(20.0);
        let mut d = DynRt::new(SR);
        d.reset(&p);
        // background at -50 dB: stays closed at -60 dB
        let out = run(&mut d, &p, db_to_gain(-50.0), 4800);
        assert!(approx(to_db(out[0]), -110.0, 0.1), "starts closed: {}", to_db(out[0]));
        assert!(approx(p.gate_gr.load(), -60.0, 0.1));
        // speech at -20 dB: open within a few ms (1 ms attack)
        let out = run(&mut d, &p, db_to_gain(-20.0), 960);
        assert!(approx(to_db(out[959]), -20.0, 0.01));
        assert!(to_db(out[0]) < -40.0, "it was closed when the sound started: {}", to_db(out[0]));
        // back to background: open through the hold (50 ms) ...
        let out = run(&mut d, &p, db_to_gain(-50.0), 2000);
        assert!(approx(to_db(out[1999]), -50.0, 0.01), "held open");
        // ... then down to the range
        let out = run(&mut d, &p, db_to_gain(-50.0), 24_000);
        assert!(approx(to_db(*out.last().unwrap()), -110.0, 0.1));
        // a level just under the threshold, but above the hysteresis, keeps it open
        run(&mut d, &p, db_to_gain(-20.0), 960);
        let out = run(&mut d, &p, db_to_gain(-42.0), 24_000);
        assert!(approx(to_db(*out.last().unwrap()), -42.0, 0.01));
    }

    #[test]
    fn switching_off_glides_back_to_bit_exact() {
        let p = DynParams::default();
        p.gate_on.store(true, Relaxed);
        p.comp_on.store(true, Relaxed);
        p.comp[C_MAKEUP].store(6.0);
        let mut d = DynRt::new(SR);
        d.reset(&p);
        run(&mut d, &p, 0.5, 4800);
        p.gate_on.store(false, Relaxed);
        p.comp_on.store(false, Relaxed);
        let out = run(&mut d, &p, 0.5, 128);
        for w in out.windows(2) {
            assert!((w[1] - w[0]).abs() < 0.02, "no step: {} {}", w[0], w[1]);
        }
        run(&mut d, &p, 0.5, 48_000);
        assert!(!d.active(), "{d:?}");
        let mut l = vec![0.3f32; 64];
        let mut r = vec![0.1f32; 64];
        d.process(&p, &mut l, &mut r, 1.0);
        assert!(l.iter().all(|&x| x == 0.3) && r.iter().all(|&x| x == 0.1));
    }

    #[test]
    fn bad_input_and_settings_stay_finite() {
        let p = DynParams::default();
        p.gate_on.store(true, Relaxed);
        p.comp_on.store(true, Relaxed);
        p.comp[C_RATIO].store(f32::NAN);
        p.gate[G_ATTACK].store(f32::INFINITY);
        p.comp[C_MAKEUP].store(1e9);
        let mut d = DynRt::new(SR);
        let mut l = vec![f32::MAX, 0.0, 1e-30, 2.0];
        let mut r = vec![0.0, f32::MAX, 0.0, -2.0];
        d.process(&p, &mut l, &mut r, f32::NAN);
        assert!(l.iter().chain(&r).all(|x| x.is_finite()), "{l:?} {r:?}");
        assert!(p.gate_gr.load().is_finite() && p.comp_gr.load().is_finite());
    }

    #[test]
    fn settings_round_trip_and_clamp() {
        let p = DynParams::default();
        let mut s = DynSettings { gate_on: true, ..DynSettings::default() };
        s.gate[G_THRESHOLD] = -42.04;
        s.comp[C_RATIO] = 100.0;
        p.apply(&s);
        let back = p.settings();
        assert!(back.gate_on && !back.comp_on);
        assert_eq!(back.gate[G_THRESHOLD], -42.0);
        assert_eq!(back.comp[C_RATIO], 20.0);
        p.reset_settings();
        assert_eq!(p.settings(), DynSettings { gate_on: true, ..DynSettings::default() });
        assert_eq!(GATE[G_HOLD].parse(" 250 "), Ok(250.0));
        assert!(GATE[G_HOLD].parse("-1").unwrap_err().contains("out of range"));
        assert!(COMP[C_RATIO].parse("x").unwrap_err().contains("number"));
    }
}
