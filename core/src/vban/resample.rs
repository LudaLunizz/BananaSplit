//! Variable-ratio windowed-sinc resampler for incoming streams.
//!
//! A VBAN sender is its own clock master: its 48 kHz is never exactly the
//! mixer's 48 kHz, and it may not be 48 kHz at all (Voicemeeter often runs
//! at 44.1 kHz). Every incoming stream is therefore resampled to the
//! mixer's rate, at a ratio that the receiver nudges by a few ppm to keep
//! its buffer level (see `rx.rs`).
//!
//! Kernel: sinc with 48 zero crossings per side under a Kaiser window
//! (beta 8.96, about 90 dB stopband), cut off at 0.4725 of the lower of the
//! two rates, so the passband is flat to 21 kHz at 48 kHz and nothing above
//! the output's Nyquist frequency folds back. The kernel is tabulated at
//! 512 points per zero crossing and interpolated linearly; each output
//! sample is normalized by the sum of its weights, so DC passes exactly.
//! Latency: half the kernel, 51 input samples at equal rates (1.1 ms).
//!
//! This runs on the network thread, never in the audio callback.

const ZEROS: usize = 48;
const RES: usize = 512;
const BETA: f64 = 8.96;
const BW: f64 = 0.4725;
/// Highest input/output rate ratio supported (192 kHz into 48 kHz).
pub const MAX_DOWN: f64 = 4.0;

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

pub struct Resampler {
    table: Vec<f32>,
    /// Input samples -> zero crossings.
    scale: f64,
    /// Kernel half-width in input samples.
    half: f64,
    lead: usize,
    /// Stereo input history (`L R L R ...`).
    hist: Vec<f32>,
    /// Input position of the next output frame: `hist` frame `ipos`, plus
    /// `frac` (0 <= frac < 1). Kept apart so trimming the history (which
    /// subtracts from `ipos`) never changes the fractional position.
    ipos: usize,
    frac: f64,
    fs_in: u32,
    fs_out: u32,
}

impl Resampler {
    pub fn new(fs_in: u32, fs_out: u32) -> Resampler {
        let ratio = fs_out as f64 / fs_in.max(1) as f64;
        let fc = BW * ratio.min(1.0); // cycles per input sample
        let i0b = bessel_i0(BETA);
        let n = ZEROS * RES + 2;
        let table = (0..n)
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
        let lead = half.ceil() as usize + 1;
        Resampler {
            table,
            scale: 2.0 * fc,
            half,
            lead,
            hist: vec![0.0; 2 * lead],
            ipos: lead,
            frac: 0.0,
            fs_in,
            fs_out,
        }
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.fs_in, self.fs_out)
    }

    /// Input frames per output frame at the nominal rates.
    pub fn nominal_step(&self) -> f64 {
        self.fs_in as f64 / self.fs_out as f64
    }

    /// Latency in input frames (half the kernel).
    pub fn delay(&self) -> f64 {
        self.half
    }

    /// Forget the history (start again from silence).
    pub fn reset(&mut self) {
        self.hist.clear();
        self.hist.resize(2 * self.lead, 0.0);
        self.ipos = self.lead;
        self.frac = 0.0;
    }

    #[inline]
    fn kernel(&self, u: f64) -> f64 {
        let x = u * RES as f64;
        let i = x as usize;
        if i >= ZEROS * RES {
            return 0.0;
        }
        let f = x - i as f64;
        self.table[i] as f64 * (1.0 - f) + self.table[i + 1] as f64 * f
    }

    /// Append stereo `input` frames and append every output frame that can
    /// now be computed to `out`, advancing `step` input frames per output
    /// frame (`nominal_step()`, give or take the drift correction).
    pub fn process(&mut self, input: &[f32], step: f64, out: &mut Vec<f32>) {
        debug_assert!(input.len().is_multiple_of(2) && step > 0.0);
        self.hist.extend_from_slice(input);
        let frames = self.hist.len() / 2;
        // taps to each side of the position (offsets relative to `ipos`)
        let reach = self.half.floor() as usize + 1;
        loop {
            let last = self.ipos + (self.frac + self.half).floor() as usize;
            if last >= frames {
                break;
            }
            let first = self.ipos.saturating_sub(reach);
            let (mut l, mut r, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
            for j in first..=last {
                // distance from the position, computed from small numbers only
                let d = (j as f64 - self.ipos as f64 - self.frac).abs();
                let w = self.kernel(d * self.scale);
                l += self.hist[2 * j] as f64 * w;
                r += self.hist[2 * j + 1] as f64 * w;
                wsum += w;
            }
            out.push((l / wsum) as f32);
            out.push((r / wsum) as f32);
            self.frac += step;
            let whole = self.frac.floor();
            self.ipos += whole as usize;
            self.frac -= whole;
        }
        // Drop history that no output will need again.
        let dead = self.ipos.saturating_sub(reach + 1);
        if dead >= 4096 {
            self.hist.drain(..2 * dead);
            self.ipos -= dead;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(fs: u32, f: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = (amp * (2.0 * PI * f * i as f64 / fs as f64).sin()) as f32;
                [v, -v]
            })
            .collect()
    }

    /// Feed in uneven chunks; returns the output.
    fn run(rs: &mut Resampler, input: &[f32], step: f64) -> Vec<f32> {
        let mut out = Vec::new();
        let mut i = 0;
        let mut k = 0;
        while i < input.len() {
            let n = (2 * [37usize, 256, 1, 129, 480][k % 5]).min(input.len() - i);
            rs.process(&input[i..i + n], step, &mut out);
            i += n;
            k += 1;
        }
        out
    }

    /// Worst error against the ideal sine, after the start-up transient.
    fn worst(out: &[f32], fs_out: u32, f: f64, amp: f64, skip: usize) -> f64 {
        out.chunks(2)
            .enumerate()
            .skip(skip)
            .map(|(m, s)| {
                let want = amp * (2.0 * PI * f * m as f64 / fs_out as f64).sin();
                (s[0] as f64 - want).abs().max((s[1] as f64 + want).abs())
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn equal_rates_pass_the_audio_band_cleanly() {
        for f in [100.0, 1000.0, 10_000.0, 20_000.0] {
            let mut rs = Resampler::new(48_000, 48_000);
            let out = {
                let st = rs.nominal_step();
                run(&mut rs, &sine(48_000, f, 48_000, 0.5), st)
            };
            assert!(out.len() / 2 > 47_800, "{}", out.len() / 2);
            let e = worst(&out, 48_000, f, 0.5, 200);
            assert!(e < 0.5 * 2e-4, "{f} Hz: error {e:e} (-74 dB re signal)");
        }
    }

    #[test]
    fn converts_44100_to_48000_and_back() {
        for (a, b, f) in [
            (44_100, 48_000, 1000.0),
            (48_000, 44_100, 1000.0),
            (44_100, 48_000, 18_000.0),
            (8000, 48_000, 3000.0),
            (96_000, 48_000, 15_000.0),
            (192_000, 48_000, 440.0),
        ] {
            let mut rs = Resampler::new(a, b);
            let out = {
                let st = rs.nominal_step();
                run(&mut rs, &sine(a, f, a as usize, 0.5), st)
            };
            let expect = b as f64 * (a as f64 - rs.delay()) / a as f64;
            assert!((out.len() as f64 / 2.0 - expect).abs() < 3.0, "{a}->{b}: {} frames, want {expect}", out.len() / 2);
            let e = worst(&out, b, f, 0.5, 400);
            assert!(e < 0.5 * 3e-4, "{a}->{b} {f} Hz: error {e:e}");
        }
    }

    /// Level (dB re full scale) of the strongest component near `f`.
    fn level_db(out: &[f32], fs: u32, f: f64) -> f64 {
        let n = out.len() / 2;
        let (mut re, mut im) = (0.0, 0.0);
        let mut wsum = 0.0;
        for (m, s) in out.chunks(2).enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * m as f64 / n as f64).cos(); // Hann
            let ph = 2.0 * PI * f * m as f64 / fs as f64;
            re += s[0] as f64 * w * ph.cos();
            im += s[0] as f64 * w * ph.sin();
            wsum += w;
        }
        20.0 * ((re * re + im * im).sqrt() * 2.0 / wsum).log10()
    }

    #[test]
    fn no_aliasing_or_images() {
        // 96k -> 48k: a 30 kHz tone must not fold back to 18 kHz
        let mut rs = Resampler::new(96_000, 48_000);
        let out = {
            let st = rs.nominal_step();
            run(&mut rs, &sine(96_000, 30_000.0, 96_000, 1.0), st)
        };
        assert!(level_db(&out[800..], 48_000, 18_000.0) < -85.0, "{}", level_db(&out[800..], 48_000, 18_000.0));
        // 8k -> 48k: a 3 kHz tone must not leave an image at 5 kHz
        let mut rs = Resampler::new(8000, 48_000);
        let out = {
            let st = rs.nominal_step();
            run(&mut rs, &sine(8000, 3000.0, 8000, 1.0), st)
        };
        assert!(level_db(&out[2000..], 48_000, 5000.0) < -85.0, "{}", level_db(&out[2000..], 48_000, 5000.0));
        assert!(level_db(&out[2000..], 48_000, 3000.0) > -0.1);
    }

    #[test]
    fn drift_correction_changes_the_rate_smoothly() {
        let mut rs = Resampler::new(48_000, 48_000);
        let input = sine(48_000, 1000.0, 48_000, 0.5);
        let mut out = Vec::new();
        for (k, chunk) in input.chunks(256).enumerate() {
            // +-1000 ppm, switching every chunk: still a clean sine
            let step = 1.0 + if k % 2 == 0 { 1e-3 } else { -1e-3 };
            rs.process(chunk, step, &mut out);
        }
        let jump =
            out.chunks(2).collect::<Vec<_>>().windows(2).map(|w| (w[1][0] - w[0][0]).abs()).fold(0.0f32, f32::max);
        let max_slope = 0.5 * 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0;
        assert!(jump < max_slope * 1.01, "{jump} vs {max_slope}");
        assert!((out.len() as i64 / 2 - (48_000 - 51)).abs() < 5);
    }

    #[test]
    fn chunking_does_not_change_the_output() {
        let input = sine(44_100, 997.0, 20_000, 0.7);
        let mut a = Resampler::new(44_100, 48_000);
        let mut b = Resampler::new(44_100, 48_000);
        let mut oa = Vec::new();
        let st = a.nominal_step();
        a.process(&input, st, &mut oa);
        let ob = {
            let st = b.nominal_step();
            run(&mut b, &input, st)
        };
        assert_eq!(oa, ob);
        // and reset() starts again from silence
        a.reset();
        let mut oc = Vec::new();
        a.process(&input, st, &mut oc);
        assert_eq!(oa, oc);
    }

    #[test]
    fn dc_is_exact() {
        let mut rs = Resampler::new(44_100, 48_000);
        let st = rs.nominal_step();
        let out = run(&mut rs, &[0.25f32; 2 * 10_000], st);
        for s in &out[2 * 200..] {
            assert!((s - 0.25).abs() < 1e-6, "{s}");
        }
    }
}
