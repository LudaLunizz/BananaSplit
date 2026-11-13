//! Brickwall limiter on every bus (like Voicemeeter's LIMIT, but on the buses).
//!
//! It is the last stage of a bus: after its EQ, input gain, fader and
//! balance, before its meter, its sends to other buses, its device or cable,
//! the recorder and VBAN. Whatever the fader says, nothing leaves the bus
//! louder than the limit.
//!
//! * **A hard ceiling.** One gain for left and right (stereo linked), from a
//!   peak detector that jumps up at once (no attack time, no look-ahead), so
//!   every sample is held at or under the ceiling; a final clamp catches the
//!   last bit of float rounding. The ceiling is never exceeded, for any input.
//! * **Gentle release.** The detector holds a peak for [`HOLD_MS`], then
//!   lets go over [`RELEASE_MS`], so bass and steady tones are not chopped
//!   up wave by wave.
//! * **Moving the limit.** Lowering it acts at once (the new ceiling holds
//!   from the first sample); raising it glides over one cycle, so it never
//!   clicks and never lets anything over the new setting either.
//! * **Off** at the top of the range ([`LIMIT_OFF_DB`], +12 dB, the top of
//!   the fader): the signal passes bit for bit and nothing is computed.
//! * **No added latency**, **real-time safe**: no allocation, locks or
//!   syscalls; the setting is an atomic read once per cycle.

use crate::engine::{AtomicF32, db_to_gain};

/// Lowest limit, dBFS.
pub const LIMIT_MIN_DB: f32 = -40.0;
/// The top of the range: the limiter is off.
pub const LIMIT_OFF_DB: f32 = crate::engine::MAX_GAIN_DB;
/// How long a peak is held before the gain starts to come back.
pub const HOLD_MS: f32 = 5.0;
/// Time constant of the gain coming back after a peak.
pub const RELEASE_MS: f32 = 80.0;
/// How fast the shown gain reduction falls back (dB per second), so the
/// page (30 reads a second) sees short peaks too.
const SHOW_FALL_DB_PER_S: f32 = 20.0;

/// A limit in range (NaN = off), not rounded.
#[inline]
pub fn clamp_limit(db: f32) -> f32 {
    if db.is_nan() { LIMIT_OFF_DB } else { db.clamp(LIMIT_MIN_DB, LIMIT_OFF_DB) }
}

/// Is a limit (clamped) the "off" position?
#[inline]
pub fn is_off(db: f32) -> bool {
    clamp_limit(db) >= LIMIT_OFF_DB
}

/// A limit as read from a config, scene or command: dB, clamped to its
/// range, rounded to 0.1 dB.
pub fn parse_limit(v: &str) -> Result<f32, String> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("off") {
        return Ok(LIMIT_OFF_DB);
    }
    let g: f32 = v.parse().map_err(|_| format!("expected a number or off, got {v:?}"))?;
    if !g.is_finite() {
        return Err(format!("expected a finite number, got {v:?}"));
    }
    Ok((clamp_limit(g) * 10.0).round() / 10.0)
}

/// The limiter's setting and what it does, shared with the UI.
#[derive(Debug)]
pub struct LimitParams {
    /// The limit, dBFS; [`LIMIT_OFF_DB`] = off.
    db: AtomicF32,
    /// Gain reduction now, dB (≥ 0), falling back slowly after a peak.
    pub gr: AtomicF32,
}

impl Default for LimitParams {
    fn default() -> Self {
        Self { db: AtomicF32::new(LIMIT_OFF_DB), gr: AtomicF32::new(0.0) }
    }
}

impl LimitParams {
    /// The limit, dBFS (clamped).
    pub fn load(&self) -> f32 {
        clamp_limit(self.db.load())
    }
    pub fn store(&self, db: f32) {
        self.db.store(clamp_limit(db));
    }
    /// The ceiling as a sample value, `None` when off.
    #[inline]
    fn ceiling(&self) -> Option<f32> {
        let db = self.load();
        if db >= LIMIT_OFF_DB { None } else { Some(db_to_gain(db)) }
    }
}

/// The audio thread's half of one bus limiter.
#[derive(Debug)]
pub struct LimitRt {
    sr: f32,
    /// Peak detector, linear (what the gain is worked out from).
    env: f32,
    /// Samples the detector still holds its peak.
    hold_left: u32,
    /// The ceiling at the end of the last cycle, `None` = off.
    ceil: Option<f32>,
    /// Shown gain reduction, dB.
    show: f32,
}

impl LimitRt {
    pub fn new(sample_rate: u32) -> Self {
        let mut l = Self { sr: 48_000.0, env: 0.0, hold_left: 0, ceil: None, show: 0.0 };
        l.set_sample_rate(sample_rate);
        l
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.sr = sample_rate.max(8000) as f32;
    }

    /// Limit one cycle of a stereo bus in place. The samples must be finite
    /// (the engine sanitizes them first).
    pub fn process(&mut self, p: &LimitParams, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let target = p.ceiling();
        let fall = SHOW_FALL_DB_PER_S * n as f32 / self.sr;
        if self.ceil.is_none() && target.is_none() {
            // off and settled: untouched
            self.env = 0.0;
            self.hold_left = 0;
            self.show = (self.show - fall).max(0.0);
            p.gr.store(self.show);
            return;
        }
        let top = db_to_gain(LIMIT_OFF_DB);
        let to = target.unwrap_or(top);
        // Lower at once, raise over the cycle: never over the setting.
        let from = self.ceil.unwrap_or(top).min(to);
        let rel = (-1000.0 / (RELEASE_MS * self.sr)).exp();
        let hold_n = (HOLD_MS * 0.001 * self.sr) as u32;
        let (mut env, mut hold_left) = (self.env, self.hold_left);
        let mut g_min = 1.0f32;
        for i in 0..n {
            let c = if from == to || i + 1 >= n { to } else { from + (to - from) * ((i + 1) as f32 / n as f32) };
            let x = l[i].abs().max(r[i].abs());
            if x >= env {
                env = x;
                hold_left = hold_n;
            } else if hold_left > 0 {
                hold_left -= 1;
            } else {
                env = (env * rel).max(x);
            }
            if env > c {
                let g = c / env;
                l[i] = (l[i] * g).clamp(-c, c);
                r[i] = (r[i] * g).clamp(-c, c);
                g_min = g_min.min(g);
            }
        }
        self.ceil = target;
        if target.is_some() {
            self.env = env;
            self.hold_left = hold_left;
        } else {
            self.env = 0.0;
            self.hold_left = 0;
        }
        let gr = if g_min < 1.0 { -20.0 * g_min.log10() } else { 0.0 };
        self.show = gr.max(self.show - fall).max(0.0);
        p.gr.store(self.show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(db: f32) -> (LimitParams, LimitRt) {
        let p = LimitParams::default();
        p.store(db);
        (p, LimitRt::new(48_000))
    }

    /// A small deterministic noise source (xorshift), -1..1.
    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    #[test]
    fn off_by_default_and_bit_exact() {
        let p = LimitParams::default();
        assert_eq!(p.load(), LIMIT_OFF_DB);
        let mut rt = LimitRt::new(48_000);
        let mut l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.37).sin() * 3.0).collect();
        let mut r: Vec<f32> = l.iter().map(|x| -x * 0.5).collect();
        let (l0, r0) = (l.clone(), r.clone());
        rt.process(&p, &mut l, &mut r);
        assert_eq!(l, l0);
        assert_eq!(r, r0);
        assert_eq!(p.gr.load(), 0.0);
    }

    #[test]
    fn never_over_the_ceiling() {
        // every limit, loud noise up to +24 dB over full scale, odd cycle sizes
        for limit in [-40.0, -24.0, -12.5, -6.0, -0.1, 0.0, 6.0, 11.9] {
            let (p, mut rt) = rig(limit);
            let c = db_to_gain(limit);
            let mut noise = Noise(0x9E37_79B9);
            for (k, n) in [1usize, 7, 64, 128, 333, 1024, 4096].iter().cycle().take(60).enumerate() {
                let amp = 16.0 * (k % 5) as f32 / 4.0;
                let mut l: Vec<f32> = (0..*n).map(|_| noise.next() * amp).collect();
                let mut r: Vec<f32> = (0..*n).map(|_| noise.next() * amp * 0.7).collect();
                rt.process(&p, &mut l, &mut r);
                for (a, b) in l.iter().zip(&r) {
                    assert!(a.abs() <= c && b.abs() <= c, "limit {limit}: {a} {b} over {c}");
                }
            }
        }
    }

    #[test]
    fn extreme_samples_stay_under() {
        let (p, mut rt) = rig(-3.0);
        let c = db_to_gain(-3.0);
        let mut l = vec![f32::MAX, -f32::MAX, 1e30, f32::MIN_POSITIVE, 0.0, -0.0, 1.0];
        let mut r = vec![-f32::MAX, 3.0, -1e-30, 1e38, 0.0, 5.0, -1.0];
        rt.process(&p, &mut l, &mut r);
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() <= c), "{l:?} {r:?}");
    }

    #[test]
    fn quiet_signal_passes_unchanged() {
        let (p, mut rt) = rig(-6.0);
        let mut l: Vec<f32> = (0..512).map(|i| (i as f32 * 0.1).sin() * 0.25).collect();
        let mut r = l.clone();
        let l0 = l.clone();
        rt.process(&p, &mut l, &mut r);
        assert_eq!(l, l0);
        assert_eq!(p.gr.load(), 0.0);
    }

    #[test]
    fn lowering_holds_at_once_raising_glides() {
        let (p, mut rt) = rig(0.0);
        let loud = |n| vec![2.0f32; n];
        let (mut l, mut r) = (loud(128), loud(128));
        rt.process(&p, &mut l, &mut r);
        assert!(l.iter().all(|&x| x <= 1.0));
        // lower to -12: the very first sample is already under it
        p.store(-12.0);
        let c = db_to_gain(-12.0);
        let (mut l, mut r) = (loud(128), loud(128));
        rt.process(&p, &mut l, &mut r);
        assert!(l.iter().all(|&x| x <= c), "{}", l[0]);
        // raise to -6: glides up, never past -6, lands on it at the end
        p.store(-6.0);
        let c6 = db_to_gain(-6.0);
        let (mut l, mut r) = (loud(128), loud(128));
        rt.process(&p, &mut l, &mut r);
        assert!(l.iter().all(|&x| x <= c6));
        assert!(l[0] < c6 * 0.99, "jumped: {}", l[0]);
        assert!(l.windows(2).all(|w| w[1] >= w[0]), "not a glide");
        assert!((l[127] - c6).abs() < 1e-6);
        assert!(p.gr.load() > 5.0);
    }

    #[test]
    fn switching_off_glides_then_bit_exact() {
        let (p, mut rt) = rig(-6.0);
        let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
        rt.process(&p, &mut l, &mut r);
        p.store(LIMIT_OFF_DB);
        let (mut l, mut r) = (vec![1.0f32; 64], vec![1.0f32; 64]);
        rt.process(&p, &mut l, &mut r);
        assert!(l[0] < 0.6 && l[63] == 1.0, "{} {}", l[0], l[63]);
        let (mut l, mut r) = (vec![1.5f32; 64], vec![-7.0f32; 64]);
        rt.process(&p, &mut l, &mut r);
        assert!(l.iter().all(|&x| x == 1.5) && r.iter().all(|&x| x == -7.0));
    }

    #[test]
    fn stereo_linked_and_releases() {
        let (p, mut rt) = rig(-6.0);
        let c = db_to_gain(-6.0);
        // a peak on the left turns the right down by the same gain
        let (mut l, mut r) = (vec![1.0f32], vec![0.25f32]);
        rt.process(&p, &mut l, &mut r);
        assert!((l[0] - c).abs() < 1e-6);
        assert!((r[0] - 0.25 * c).abs() < 1e-6);
        // then quiet: the gain is back after hold + a few release times
        let n = (0.5 * 48_000.0) as usize;
        let (mut l, mut r) = (vec![0.1f32; n], vec![0.1f32; n]);
        rt.process(&p, &mut l, &mut r);
        assert!(l[0] < 0.1, "held");
        assert_eq!(l[n - 1], 0.1, "released");
    }

    #[test]
    fn parse_and_clamp() {
        assert_eq!(parse_limit("-6.04"), Ok(-6.0));
        assert_eq!(parse_limit("off"), Ok(LIMIT_OFF_DB));
        assert_eq!(parse_limit(" OFF "), Ok(LIMIT_OFF_DB));
        assert_eq!(parse_limit("-99"), Ok(LIMIT_MIN_DB));
        assert_eq!(parse_limit("40"), Ok(LIMIT_OFF_DB));
        assert!(parse_limit("nan").is_err());
        assert!(parse_limit("x").is_err());
        assert!(is_off(f32::NAN) && is_off(12.0) && !is_off(11.9));
    }
}
