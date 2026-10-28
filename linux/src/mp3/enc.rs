//! MP3 encoder: MPEG-1 Layer III (32, 44.1 and 48 kHz), constant bitrate,
//! joint stereo.
//!
//! ```text
//!  PCM ─► polyphase filterbank ─► MDCT (long, or 3 short after an attack)
//!      ─► mid/side? ─► masking thresholds ─► quantization loops ─► Huffman ─► frame
//! ```
//!
//! The analysis is the one of ISO/IEC 11172-3 (Annex C); the noise is
//! shaped with a simple model in the MDCT domain (the energy of each
//! scalefactor band spread over its neighbours, never below the threshold of
//! hearing), and the outer loop amplifies the bands where the noise is
//! furthest above it until the frame's bits run out, so the noise follows
//! the masking curve. Every frame is self-contained (no bit reservoir:
//! `main_data_begin` is always 0), which costs little at 320 kbit/s and lets
//! any frame be decoded on its own.
//!
//! The first frame is an Info frame with the LAME tag (frames, bytes, the
//! encoder delay and padding), so players that read it, and BananaSplit's
//! own, play exactly the samples that went in: the hourly parts of a take
//! follow each other without a gap.

use super::dec::{mdct_window, window};
use super::tables::{ALIAS_C, COUNT1, HUFF, KBPS, LINBITS, SFB_LONG, SFB_SHORT, SLEN};
use std::f64::consts::PI;
use std::sync::OnceLock;

/// Samples per channel in a frame.
pub const FRAME: usize = 1152;
const GRANULE: usize = 576;
/// The delay of the encoder and decoder together, in samples: a sample that
/// goes in at position `t` comes out of any decoder at `t + CODEC_DELAY`.
pub const CODEC_DELAY: u32 = 1057;
/// What a decoder adds to the LAME tag's encoder delay (its own filter delay).
pub const DECODER_DELAY: u32 = 529;
/// When a recording goes on in another file, the new file starts with this
/// much of the old one's end (`preroll`) and the old one ends with this much
/// of the new one's start (`encode_tail`): both encode the cut as if it were
/// in the middle of one file, and the LAME tag's delay and padding (12 bits
/// each) hide the overlap from every decoder that reads it.
pub const PREROLL: usize = 1152;
pub const TAIL: usize = 1728;
/// Largest value the Huffman tables can carry (15 + 13 linbits).
const IXMAX: i32 = 8206;
/// Exponents (in quarter steps) the quantizer can meet.
const QMIN: i32 = -400;
const QMAX: i32 = 64;

// ---------------------------------------------------------------------------
// Constants built once
// ---------------------------------------------------------------------------

struct Consts {
    /// Analysis window C = D / 32 and matrix cos((2k + 1)(i - 16) pi / 64).
    c: Vec<f32>,
    m: Vec<[f32; 64]>,
    /// Forward MDCT: long [18][36], short [6][12], already scaled for an
    /// ISO decoder's unscaled inverse.
    mdct36: Vec<[f32; 36]>,
    mdct12: [[f32; 12]; 6],
    win: [[f32; 36]; 4],
    win12: [f32; 12],
    cs: [f32; 8],
    ca: [f32; 8],
    /// x^(4/3), 2^(-3q/16) and 2^(q/4) for q in QMIN..=QMAX.
    pow43: Vec<f32>,
    ipow: Vec<f32>,
    pow2q: Vec<f32>,
    /// Code lengths with sign bits, per table 0..=31 (`xlen * xlen`).
    hlen: Vec<Vec<u8>>,
    /// Count1 lengths with sign bits, tables A and B.
    c1len: [[u8; 16]; 2],
}

fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| {
        let mut mdct12 = [[0f32; 12]; 6];
        for (i, row) in mdct12.iter_mut().enumerate() {
            for (k, v) in row.iter_mut().enumerate() {
                *v = (2.0 / 6.0 * (PI / 24.0 * (2 * k + 1 + 6) as f64 * (2 * i + 1) as f64).cos()) as f32;
            }
        }
        let hlen = (0..32)
            .map(|t| {
                let (xlen, codes) = HUFF[t];
                codes
                    .iter()
                    .enumerate()
                    .map(|(k, &(_, len))| len + u8::from(k / xlen.max(1) != 0) + u8::from(k % xlen.max(1) != 0))
                    .collect()
            })
            .collect();
        let c1len = std::array::from_fn(|t| std::array::from_fn(|p| COUNT1[t][p].1 + p.count_ones() as u8));
        Consts {
            c: (0..512).map(|i| (window(i) / 32.0) as f32).collect(),
            m: (0..32)
                .map(|k| std::array::from_fn(|i| ((2 * k + 1) as f64 * (i as f64 - 16.0) * PI / 64.0).cos() as f32))
                .collect(),
            mdct36: (0..18)
                .map(|i| {
                    std::array::from_fn(|k| {
                        (2.0 / 18.0 * (PI / 72.0 * (2 * k + 1 + 18) as f64 * (2 * i + 1) as f64).cos()) as f32
                    })
                })
                .collect(),
            mdct12,
            win: std::array::from_fn(|bt| std::array::from_fn(|i| mdct_window(bt as u8, i) as f32)),
            win12: std::array::from_fn(|i| (PI / 12.0 * (i as f64 + 0.5)).sin() as f32),
            cs: std::array::from_fn(|i| (1.0 / (1.0 + ALIAS_C[i] * ALIAS_C[i]).sqrt()) as f32),
            ca: std::array::from_fn(|i| (ALIAS_C[i] / (1.0 + ALIAS_C[i] * ALIAS_C[i]).sqrt()) as f32),
            pow43: (0..=IXMAX).map(|i| f64::from(i).powf(4.0 / 3.0) as f32).collect(),
            ipow: (QMIN..=QMAX).map(|q| 2f64.powf(-3.0 * f64::from(q) / 16.0) as f32).collect(),
            pow2q: (QMIN..=QMAX).map(|q| 2f64.powf(f64::from(q) / 4.0) as f32).collect(),
            hlen,
            c1len,
        }
    })
}

/// The MPEG-1 sample rates (`tables::RATES` index 0-2).
pub fn rate_index(rate: u32) -> Option<usize> {
    [44100, 48000, 32000].iter().position(|&r| r == rate)
}

/// The MPEG-1 bitrate index of `kbps`.
pub fn bitrate_index(kbps: u32) -> Option<usize> {
    (1..15).find(|&i| KBPS[0][i] == kbps)
}

// ---------------------------------------------------------------------------
// Bit writer
// ---------------------------------------------------------------------------

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
    bits: usize,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> BitWriter<'a> {
        BitWriter { out, acc: 0, n: 0, bits: 0 }
    }

    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        debug_assert!(n <= 32 && (n == 32 || v >> n == 0));
        self.acc = (self.acc << n) | u64::from(v);
        self.n += n;
        self.bits += n as usize;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }

    /// Pad the last byte with zeros.
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(0, pad);
        }
    }
}

// ---------------------------------------------------------------------------
// One granule of one channel
// ---------------------------------------------------------------------------

/// The scalefactor bands of a granule in bitstream order: 22 long bands
/// (the last without a scalefactor), or 13 short bands x 3 windows.
#[derive(Clone, Copy)]
struct Band {
    start: usize,
    end: usize,
    /// Allowed noise energy.
    thr: f32,
}

#[derive(Clone)]
struct Gr {
    block_type: u8,
    xr: [f32; 576],
    xr34: [f32; 576],
    bands: Vec<Band>,
    /// Bands with a scalefactor (21 long, 36 short).
    n_sf: usize,
    sf: [i32; 39],
    scalefac_scale: bool,
    global_gain: i32,
    ix: [i32; 576],
    // ---- results ----
    part2_3_length: u32,
    part2_length: u32,
    scalefac_compress: usize,
    big_values: usize,
    count1_end: usize,
    table_select: [usize; 3],
    region0_count: usize,
    region1_count: usize,
    count1_table: usize,
}

impl Gr {
    fn new() -> Gr {
        Gr {
            block_type: 0,
            xr: [0.0; 576],
            xr34: [0.0; 576],
            bands: Vec::with_capacity(39),
            n_sf: 21,
            sf: [0; 39],
            scalefac_scale: false,
            global_gain: 210,
            ix: [0; 576],
            part2_3_length: 0,
            part2_length: 0,
            scalefac_compress: 0,
            big_values: 0,
            count1_end: 0,
            table_select: [0; 3],
            region0_count: 0,
            region1_count: 0,
            count1_table: 0,
        }
    }

    /// The exponent (in quarter steps) of band `b` at gain `gg`.
    #[inline]
    fn q(&self, b: usize, gg: i32) -> i32 {
        let shift = if self.scalefac_scale { 2 } else { 1 };
        let sf = if b < self.n_sf { self.sf[b] } else { 0 };
        (gg - 210 - (sf << shift)).clamp(QMIN, QMAX)
    }

    /// Quantize at gain `gg`; false if a value would not fit the tables.
    fn quantize(&mut self, gg: i32) -> bool {
        let c = consts();
        for b in 0..self.bands.len() {
            let Band { start, end, .. } = self.bands[b];
            let f = c.ipow[(self.q(b, gg) - QMIN) as usize];
            for i in start..end {
                let v = self.xr34[i] * f + 0.4054;
                if v >= (IXMAX + 1) as f32 {
                    return false;
                }
                self.ix[i] = v as i32;
            }
        }
        true
    }

    /// Noise energy over the allowed noise, per band (after `quantize(gg)`).
    fn nmr(&self, gg: i32, out: &mut [f32; 39]) {
        let c = consts();
        for (b, band) in self.bands.iter().enumerate() {
            let step = c.pow2q[(self.q(b, gg) - QMIN) as usize];
            let mut noise = 0f32;
            for i in band.start..band.end {
                let d = self.xr[i].abs() - c.pow43[self.ix[i] as usize] * step;
                noise += d * d;
            }
            out[b] = noise / band.thr;
        }
    }

    /// Bits of the scalefactors (part 2), choosing `scalefac_compress`;
    /// `None` if they do not fit any.
    fn part2(&mut self) -> Option<u32> {
        let (n1, max1, n2, max2) = if self.block_type == 2 {
            (
                18,
                self.sf[..18].iter().copied().max().unwrap_or(0),
                18,
                self.sf[18..36].iter().copied().max().unwrap_or(0),
            )
        } else {
            (
                11,
                self.sf[..11].iter().copied().max().unwrap_or(0),
                10,
                self.sf[11..21].iter().copied().max().unwrap_or(0),
            )
        };
        let mut best: Option<(u32, usize)> = None;
        for (k, &(s1, s2)) in SLEN.iter().enumerate() {
            if max1 < 1 << s1 && max2 < 1 << s2 {
                let bits = n1 * u32::from(s1) + n2 * u32::from(s2);
                if best.is_none_or(|(b, _)| bits < b) {
                    best = Some((bits, k));
                }
            }
        }
        let (bits, k) = best?;
        self.scalefac_compress = k;
        self.part2_length = bits;
        Some(bits)
    }

    /// Huffman bits of the quantized values (part 3); sets the regions,
    /// tables and count1 table. `best` searches every region split.
    fn part3(&mut self, sfb_long: &[u16; 23], best: bool) -> u32 {
        let c = consts();
        let ix = &self.ix;
        let mut end = 576;
        while end >= 2 && ix[end - 1] == 0 && ix[end - 2] == 0 {
            end -= 2;
        }
        let count1_end = end;
        while end >= 4 && ix[end - 1] <= 1 && ix[end - 2] <= 1 && ix[end - 3] <= 1 && ix[end - 4] <= 1 {
            end -= 4;
        }
        // (a count1 region that starts at an odd line would split a pair)
        let big = end;
        self.big_values = big / 2;
        self.count1_end = count1_end;
        // count1 region: table A or B
        let (mut a, mut b) = (0u32, 0u32);
        for q in (big..count1_end).step_by(4) {
            let p = (ix[q] << 3 | ix[q + 1] << 2 | ix[q + 2] << 1 | ix[q + 3]) as usize;
            a += u32::from(c.c1len[0][p]);
            b += u32::from(c.c1len[1][p]);
        }
        self.count1_table = usize::from(b < a);
        let mut bits = a.min(b);
        // big values
        if self.block_type == 0 {
            if best {
                bits += self.best_regions(sfb_long, big);
            } else {
                let (r0, r1) = subdivide(sfb_long, big);
                bits += self.regions(sfb_long, big, r0, r1);
            }
        } else {
            // window-switched blocks: region 0 is the first 36 lines, region 1 the rest
            let r1 = 36.min(big);
            let (t0, b0) = choose_table(ix, 0, r1);
            let (t1, b1) = choose_table(ix, r1, big);
            self.table_select = [t0, t1, 0];
            bits += b0 + b1;
        }
        bits
    }

    fn regions(&mut self, sfb: &[u16; 23], big: usize, r0: usize, r1: usize) -> u32 {
        let e0 = usize::from(sfb[r0 + 1]).min(big);
        let e1 = usize::from(sfb[r0 + r1 + 2]).min(big);
        let (t0, b0) = choose_table(&self.ix, 0, e0);
        let (t1, b1) = choose_table(&self.ix, e0, e1);
        let (t2, b2) = choose_table(&self.ix, e1, big);
        self.table_select = [t0, t1, t2];
        self.region0_count = r0;
        self.region1_count = r1;
        b0 + b1 + b2
    }

    /// The split of the big values into three regions that costs the fewest bits.
    fn best_regions(&mut self, sfb: &[u16; 23], big: usize) -> u32 {
        // bands that hold big values
        let nb = (1..=22).find(|&k| usize::from(sfb[k]) >= big).unwrap_or(22);
        let mut best = (u32::MAX, 0, 0);
        for r0 in 0..16.min(nb) {
            let e0 = usize::from(sfb[r0 + 1]).min(big);
            let (_, b0) = choose_table(&self.ix, 0, e0);
            for r1 in 0..8 {
                if r0 + r1 + 2 > 22 || (r0 + r1 + 1 > nb && r1 > 0) {
                    break;
                }
                let e1 = usize::from(sfb[r0 + r1 + 2]).min(big);
                let (_, b1) = choose_table(&self.ix, e0, e1);
                let (_, b2) = choose_table(&self.ix, e1, big);
                if b0 + b1 + b2 < best.0 {
                    best = (b0 + b1 + b2, r0, r1);
                }
            }
        }
        let (_, r0, r1) = best;
        self.regions(sfb, big, r0, r1)
    }
}

/// Region 0 and 1 band counts for `big` lines of big values (LAME's table).
fn subdivide(sfb: &[u16; 23], big: usize) -> (usize, usize) {
    const SUBDV: [(usize, usize); 23] = [
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 1),
        (1, 1),
        (1, 1),
        (1, 2),
        (2, 2),
        (2, 3),
        (2, 3),
        (3, 4),
        (3, 4),
        (3, 4),
        (4, 5),
        (4, 5),
        (4, 6),
        (5, 6),
        (5, 6),
        (5, 7),
        (6, 7),
        (6, 7),
    ];
    if big == 0 {
        return (0, 0);
    }
    let mut n = 1;
    while n < 22 && usize::from(sfb[n]) < big {
        n += 1;
    }
    let (mut r0, mut r1) = SUBDV[n];
    while r0 > 0 && usize::from(sfb[r0 + 1]) > big {
        r0 -= 1;
    }
    while r1 > 0 && usize::from(sfb[r0 + r1 + 2]) > big {
        r1 -= 1;
    }
    (r0, r1)
}

/// The cheapest table for lines `a..b`, and its bits.
fn choose_table(ix: &[i32; 576], a: usize, b: usize) -> (usize, u32) {
    if a >= b {
        return (0, 0);
    }
    let max = ix[a..b].iter().copied().max().unwrap_or(0);
    if max == 0 {
        return (0, 0);
    }
    let c = consts();
    let count = |t: usize| -> u32 {
        let (xlen, _) = HUFF[t];
        let h = &c.hlen[t];
        let lin = LINBITS[t];
        let mut bits = 0u32;
        for p in (a..b).step_by(2) {
            let (x, y) = (ix[p], ix[p + 1]);
            if lin > 0 {
                bits += u32::from(h[x.min(15) as usize * xlen + y.min(15) as usize]);
                bits += lin * (u32::from(x >= 15) + u32::from(y >= 15));
            } else {
                bits += u32::from(h[x as usize * xlen + y as usize]);
            }
        }
        bits
    };
    let candidates: &[usize] = match max {
        1 => &[1],
        2 => &[2, 3],
        3 => &[5, 6],
        4..=5 => &[7, 8, 9],
        6..=7 => &[10, 11, 12],
        8..=15 => &[13, 15],
        _ => {
            let need = (max - 15) as u32;
            let t16 = (16..24).find(|&t| need < 1 << LINBITS[t]).unwrap_or(23);
            let t24 = (24..32).find(|&t| need < 1 << LINBITS[t]).unwrap_or(31);
            let (b16, b24) = (count(t16), count(t24));
            return if b16 <= b24 { (t16, b16) } else { (t24, b24) };
        }
    };
    candidates.iter().map(|&t| (t, count(t))).min_by_key(|&(_, b)| b).unwrap_or((0, 0))
}

/// The smallest global gain at which every value fits the Huffman tables.
fn min_gain(g: &Gr) -> i32 {
    let shift = if g.scalefac_scale { 2 } else { 1 };
    let mut gg = 0;
    for (b, band) in g.bands.iter().enumerate() {
        let m = g.xr34[band.start..band.end].iter().copied().fold(0f32, f32::max);
        if m > 0.0 {
            // m * 2^(-3q/16) + 0.4054 < IXMAX + 1, q = gg - 210 - (sf << shift)
            let q = (16.0 / 3.0 * (f64::from(m) / (f64::from(IXMAX) + 0.5)).log2()).floor() as i32;
            let sf = if b < g.n_sf { g.sf[b] } else { 0 };
            gg = gg.max(q + 210 + (sf << shift));
        }
    }
    gg.clamp(0, 255)
}

/// Find the smallest global gain (from `from`) whose bits fit `budget`;
/// leaves the granule quantized at it. `None` if none fits.
fn inner_loop(g: &mut Gr, sfb: &[u16; 23], budget: u32, from: i32) -> Option<i32> {
    let mut lo = from.max(min_gain(g)).clamp(0, 255);
    while lo <= 255 && !g.quantize(lo) {
        lo += 1;
    }
    if lo > 255 {
        return None;
    }
    if g.part3(sfb, false) <= budget {
        return Some(lo);
    }
    // bits fall as the gain rises: binary search lo (too many) .. hi (fits)
    let mut hi = lo + 1;
    let mut step = 1;
    loop {
        if hi > 255 {
            hi = 255;
            if !(g.quantize(hi) && g.part3(sfb, false) <= budget) {
                return None;
            }
            break;
        }
        if g.quantize(hi) && g.part3(sfb, false) <= budget {
            break;
        }
        lo = hi;
        step *= 2;
        hi += step;
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if g.quantize(mid) && g.part3(sfb, false) <= budget {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    g.quantize(hi);
    g.part3(sfb, false);
    Some(hi)
}

/// Shape the noise of one granule with its scalefactors and fit it into
/// `budget` bits (part 2 + part 3, at most 4095).
fn outer_loop(g: &mut Gr, sfb: &[u16; 23], budget: u32) {
    const MAX_ITER: usize = 24;
    let budget = budget.min(4095);
    if g.xr34.iter().all(|&v| v == 0.0) {
        g.sf = [0; 39];
        g.global_gain = 0;
        g.ix = [0; 576];
        g.scalefac_scale = false;
        g.part2().unwrap_or(0);
        g.part3(sfb, true);
        g.part2_3_length = g.part2_length;
        return;
    }
    g.sf = [0; 39];
    g.scalefac_scale = false;
    // (worst noise-to-mask ratio, scalefactors, scale, gain) of the best try
    let mut best: Option<(f32, [i32; 39], bool, i32)> = None;
    let mut gg = 0;
    let mut nmr = [0f32; 39];
    let split = if g.block_type == 2 { 18 } else { 11 };
    for _ in 0..MAX_ITER {
        let Some(p2) = g.part2() else { break };
        if p2 >= budget {
            break;
        }
        let Some(found) = inner_loop(g, sfb, budget - p2, gg) else { break };
        gg = found;
        g.nmr(gg, &mut nmr);
        let worst = nmr[..g.bands.len()].iter().copied().fold(0f32, f32::max);
        if best.is_none_or(|(w, ..)| worst < w) {
            best = Some((worst, g.sf, g.scalefac_scale, gg));
        }
        if worst <= 1.0 {
            break;
        }
        // amplify the bands with a scalefactor that are within 3 dB of the
        // worst of them (the gain then rises, and the others get noisier)
        let worst_sf = nmr[..g.n_sf].iter().copied().fold(0f32, f32::max);
        let limit = (worst_sf * 0.5).max(1.0);
        let mut any = false;
        let mut overflow = false;
        for b in 0..g.n_sf {
            if nmr[b] > limit {
                g.sf[b] += 1;
                any = true;
                overflow |= g.sf[b] > if b < split { 15 } else { 7 };
            }
        }
        if !any {
            break;
        }
        if overflow {
            if g.scalefac_scale {
                break;
            }
            g.scalefac_scale = true;
            for s in &mut g.sf[..g.n_sf] {
                *s = (*s + 1) / 2;
            }
        }
    }
    match best {
        Some((_, sf, scale, gain)) => {
            g.sf = sf;
            g.scalefac_scale = scale;
            g.global_gain = gain;
        }
        None => {
            // nothing fitted (cannot happen with samples within full scale): silence
            g.sf = [0; 39];
            g.scalefac_scale = false;
            g.global_gain = 255;
            g.xr34 = [0.0; 576];
        }
    }
    // final: the region split that costs the fewest bits
    g.quantize(g.global_gain);
    let p3 = g.part3(sfb, true);
    g.part2();
    g.part2_3_length = g.part2_length + p3;
}

// ---------------------------------------------------------------------------
// Psychoacoustics
// ---------------------------------------------------------------------------

/// Bark scale.
fn bark(f: f64) -> f64 {
    13.0 * (0.00076 * f).atan() + 3.5 * ((f / 7500.0) * (f / 7500.0)).atan()
}

/// Threshold in quiet (dB SPL, Terhardt), with a floor so that the
/// highest bands still get a finite value.
fn ath_db(f: f64) -> f64 {
    let k = (f.max(20.0)) / 1000.0;
    (3.64 * k.powf(-0.8) - 6.5 * (-0.6 * (k - 3.3) * (k - 3.3)).exp() + 1e-3 * k.powi(4)).min(96.0)
}

/// Spreading of masking (dB) from a masker to a band `dz` Bark above it.
fn spread_db(dz: f64) -> f64 {
    15.81 + 7.5 * (dz + 0.474) - 17.5 * (1.0 + (dz + 0.474) * (dz + 0.474)).sqrt()
}

/// Per-rate tables of the masking model for long (22 bands) and short
/// (13 bands of one window) blocks.
struct Psy {
    /// spread[b][k]: share of band k's energy that masks band b.
    long_spread: Vec<Vec<f32>>,
    short_spread: Vec<Vec<f32>>,
    long_ath: [f32; 22],
    short_ath: [f32; 13],
    long_offset: [f32; 22],
    short_offset: [f32; 13],
}

/// xr energy of a full-scale sine in one long block (measured once, so the
/// threshold in quiet sits at the right level whatever the scaling).
const FULL_SCALE_DB_SPL: f64 = 96.0;
/// Safety margin under the threshold in quiet (someone may turn it up).
const ATH_LOWER_DB: f64 = 6.0;

impl Psy {
    fn new(rate: u32, ri: usize, full_scale_energy: f64) -> Psy {
        let line_hz = |lines: f64, i: f64| (i + 0.5) * f64::from(rate) / 2.0 / lines;
        let make = |edges: &[u16], lines: f64, scale: f64| {
            let n = edges.len() - 1;
            let z: Vec<f64> = (0..n)
                .map(|b| bark(line_hz(lines, (f64::from(edges[b]) + f64::from(edges[b + 1]) - 1.0) / 2.0)))
                .collect();
            let spread: Vec<Vec<f32>> =
                (0..n).map(|b| (0..n).map(|k| 10f64.powf(spread_db(z[b] - z[k]) / 10.0) as f32).collect()).collect();
            let ath: Vec<f32> = (0..n)
                .map(|b| {
                    let db =
                        (edges[b]..edges[b + 1]).map(|i| ath_db(line_hz(lines, f64::from(i)))).fold(f64::MAX, f64::min);
                    (full_scale_energy * scale * 10f64.powf((db - FULL_SCALE_DB_SPL - ATH_LOWER_DB) / 10.0)) as f32
                })
                .collect();
            // masking offset: a conservative (tonal) masking index
            let offset: Vec<f32> = z.iter().map(|&z| 10f64.powf(-(12.0 + 0.4 * z) / 10.0) as f32).collect();
            (spread, ath, offset)
        };
        let (long_spread, la, lo) = make(&SFB_LONG[ri], 576.0, 1.0);
        let (short_spread, sa, so) = make(&SFB_SHORT[ri], 192.0, 1.0 / 3.0);
        Psy {
            long_spread,
            short_spread,
            long_ath: std::array::from_fn(|b| la[b]),
            short_ath: std::array::from_fn(|b| sa[b]),
            long_offset: std::array::from_fn(|b| lo[b]),
            short_offset: std::array::from_fn(|b| so[b]),
        }
    }

    /// Allowed noise per band from band energies.
    fn thresholds(e: &[f32], spread: &[Vec<f32>], ath: &[f32], offset: &[f32], out: &mut [f32]) {
        for b in 0..e.len() {
            let mut s = 0f32;
            for (k, &ek) in e.iter().enumerate() {
                s += ek * spread[b][k];
            }
            out[b] = (s * offset[b]).max(ath[b]).max(1e-30);
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// One granule after the filterbank: 18 subband samples per subband and
/// channel (odd subbands' odd samples already inverted), and whether an
/// attack calls for short blocks.
#[derive(Clone)]
struct Subbands {
    s: [[[f32; 18]; 32]; 2],
    attack: bool,
}

pub struct Encoder {
    rate: u32,
    ri: usize,
    kbps: u32,
    bitrate_index: usize,
    /// Frame `n` starts `n * num / den` bytes after the first audio frame.
    num: u64,
    den: u64,
    psy: Psy,
    lowpass: [f32; 576],
    lowpass_short: [f32; 192],
    // input
    input: [Vec<f32>; 2],
    /// Polyphase filter state: the newest 512 samples, newest first at `xpos`.
    x: [[f32; 1024]; 2],
    xpos: usize,
    /// Attack detection: filter state and the last segment energies.
    hp: [[f32; 2]; 2],
    seg_hist: [f32; 3],
    /// PCM of the attack window that reaches into the next granule.
    tail: [Vec<f32>; 2],
    // granules waiting for the next one (block-type look-ahead)
    prev: Option<Subbands>,
    pending: Option<Subbands>,
    prev_block: u8,
    /// Finished granules of the frame being built: (xr per channel, block type).
    frame: Vec<([[f32; 576]; 2], u8)>,
    work: [Gr; 2],
    // counters
    samples: u64,
    /// Samples given before the first one of the file (`preroll`) and after
    /// its last one (`encode_tail`).
    lead: u64,
    post: u64,
    frames: u64,
    bytes: u64,
    crc: u16,
    finished: bool,
}

/// Lowpass corner by bitrate (LAME's table for stereo).
fn lowpass_hz(kbps: u32) -> f64 {
    const MAP: [(u32, f64); 11] = [
        (64, 11000.0),
        (80, 13500.0),
        (96, 15100.0),
        (112, 15600.0),
        (128, 17000.0),
        (160, 17500.0),
        (192, 18600.0),
        (224, 19400.0),
        (256, 19700.0),
        (320, 20500.0),
        (u32::MAX, 20500.0),
    ];
    MAP.iter().find(|&&(k, _)| kbps <= k).map_or(20500.0, |&(_, f)| f)
}

impl Encoder {
    /// An encoder for stereo audio at `rate` (32000, 44100 or 48000 Hz)
    /// and `kbps` (an MPEG-1 bitrate, 32-320).
    pub fn new(rate: u32, kbps: u32) -> Result<Encoder, String> {
        let ri = rate_index(rate).ok_or_else(|| format!("MP3 does not support {rate} Hz (32, 44.1 or 48 kHz)"))?;
        let bitrate_index = bitrate_index(kbps).ok_or_else(|| format!("{kbps} kbit/s is not an MP3 bitrate"))?;
        consts();
        let fc = lowpass_hz(kbps).min(f64::from(rate) / 2.0 - 500.0);
        let taper = |f: f64| -> f32 {
            let (a, b) = (fc - 750.0, fc + 750.0);
            if f <= a {
                1.0
            } else if f >= b {
                0.0
            } else {
                (0.5 * (1.0 + (PI * (f - a) / (b - a)).cos())) as f32
            }
        };
        let nyq = f64::from(rate) / 2.0;
        Ok(Encoder {
            rate,
            ri,
            kbps,
            bitrate_index,
            num: 144_000 * u64::from(kbps),
            den: u64::from(rate),
            psy: Psy::new(rate, ri, full_scale_energy()),
            lowpass: std::array::from_fn(|i| taper((i as f64 + 0.5) * nyq / 576.0)),
            lowpass_short: std::array::from_fn(|i| taper((i as f64 + 0.5) * nyq / 192.0)),
            input: [Vec::with_capacity(4 * GRANULE), Vec::with_capacity(4 * GRANULE)],
            x: [[0.0; 1024]; 2],
            xpos: 0,
            hp: [[0.0; 2]; 2],
            seg_hist: [0.0; 3],
            tail: [vec![0.0; 224], vec![0.0; 224]],
            prev: None,
            pending: None,
            prev_block: 0,
            frame: Vec::with_capacity(2),
            work: [Gr::new(), Gr::new()],
            samples: 0,
            lead: 0,
            post: 0,
            frames: 0,
            bytes: 0,
            crc: 0,
            finished: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn kbps(&self) -> u32 {
        self.kbps
    }

    /// Samples per channel given so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Audio frames written (the Info frame not counted).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes of audio frames written (the Info frame not counted).
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Size of the Info frame (a frame without padding).
    pub fn info_len(&self) -> usize {
        (self.num / self.den) as usize
    }

    /// Bytes of audio frame `n` (0-based).
    fn frame_len(&self, n: u64) -> usize {
        ((n + 1) * self.num / self.den - n * self.num / self.den) as usize
    }

    /// Append interleaved stereo samples (L R L R …, full scale ±1);
    /// finished frames are appended to `out`.
    pub fn encode(&mut self, lr: &[f32], out: &mut Vec<u8>) {
        if self.finished || self.post > 0 {
            return;
        }
        self.push(lr);
        self.samples += (lr.len() / 2) as u64;
        self.run(out);
    }

    /// Before the first sample: the end of the previous file of the
    /// recording (at most `PREROLL` frames used), so the start of this one
    /// is encoded as in the middle of a stream. Decoders skip it (the LAME
    /// tag's encoder delay grows by its length).
    pub fn preroll(&mut self, lr: &[f32], out: &mut Vec<u8>) {
        if self.samples > 0 || self.lead > 0 || self.finished {
            return;
        }
        let lr = &lr[lr.len().saturating_sub(2 * PREROLL) & !1..];
        self.push(lr);
        self.lead = (lr.len() / 2) as u64;
        self.run(out);
    }

    /// After the last sample: the start of the next file of the recording
    /// (at most `TAIL` frames used), encoded but not counted (it is in the
    /// LAME tag's padding). Only `encode_tail` and `finish` may follow.
    pub fn encode_tail(&mut self, lr: &[f32], out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        let room = (TAIL as u64 - self.post) as usize;
        let lr = &lr[..lr.len().min(2 * room) & !1];
        self.push(lr);
        self.post += (lr.len() / 2) as u64;
        self.run(out);
    }

    fn push(&mut self, lr: &[f32]) {
        for f in lr.as_chunks::<2>().0 {
            for ch in 0..2 {
                // beyond full scale MP3 players clip anyway (and the
                // quantizer's range stays bounded)
                let v = f[ch];
                self.input[ch].push(if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 });
            }
        }
    }

    /// Encode what is left, padded with silence, so that every sample given
    /// is in the stream. Nothing can be added afterwards.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        // frames needed so that a decoder outputs every sample given
        let given = self.lead + self.samples + self.post;
        let need = (given + u64::from(CODEC_DELAY)).div_ceil(FRAME as u64).max(1);
        while self.frames < need {
            // silence up to the next whole granule (a frame is written once
            // the granule after it is known)
            let pad = GRANULE - self.input[0].len() % GRANULE;
            for ch in 0..2 {
                self.input[ch].extend(std::iter::repeat_n(0.0, pad));
            }
            self.run(out);
        }
        self.finished = true;
    }

    /// Encoder delay and padding for the LAME tag, for the stream as it is.
    fn delay_padding(&self) -> (u32, u32) {
        let delay = u64::from(CODEC_DELAY - DECODER_DELAY) + self.lead;
        let total = self.frames * FRAME as u64;
        let padding = if self.finished {
            total.saturating_sub(delay + self.samples)
        } else {
            // still recording: the samples a decoder can give so far
            u64::from(DECODER_DELAY).min(total.saturating_sub(delay))
        };
        (delay as u32, padding.min(4095) as u32)
    }

    /// The Info frame that starts the file: an MP3 frame of silence for
    /// players that do not read it, with the LAME tag (frame count, bytes,
    /// encoder delay and padding) for those that do.
    pub fn info_frame(&self) -> Vec<u8> {
        let len = self.info_len();
        let mut f = vec![0u8; len];
        f[..4].copy_from_slice(&self.header(false, false));
        let mut p = 4 + 32;
        let put = |f: &mut Vec<u8>, p: &mut usize, b: &[u8]| {
            f[*p..*p + b.len()].copy_from_slice(b);
            *p += b.len();
        };
        let audio_bytes = self.bytes + len as u64;
        put(&mut f, &mut p, b"Info");
        put(&mut f, &mut p, &0x0Fu32.to_be_bytes()); // frames, bytes, TOC, quality
        put(&mut f, &mut p, &(self.frames.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        put(&mut f, &mut p, &(audio_bytes.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        let toc: Vec<u8> = (0..100u32).map(|i| (i * 256 / 100) as u8).collect();
        put(&mut f, &mut p, &toc);
        put(&mut f, &mut p, &0u32.to_be_bytes()); // quality
        // LAME extension. Decoders take the gapless values from it only when
        // the version starts with "LAME" (LAME itself asks every encoder to
        // write it that way), so it does, and names BananaSplit after it.
        put(&mut f, &mut p, b"LAMEBnSpl");
        put(&mut f, &mut p, &[0x01]); // tag revision 0, CBR
        put(&mut f, &mut p, &[((lowpass_hz(self.kbps).min(f64::from(self.rate) / 2.0 - 500.0) / 100.0).round() as u8)]);
        put(&mut f, &mut p, &[0; 4]); // peak
        put(&mut f, &mut p, &[0; 4]); // replay gains
        put(&mut f, &mut p, &[0]); // flags, ATH type
        put(&mut f, &mut p, &[self.kbps.min(255) as u8]);
        let (delay, padding) = self.delay_padding();
        put(&mut f, &mut p, &[(delay >> 4) as u8, ((delay << 4) | (padding >> 8)) as u8, padding as u8]);
        let source = match self.rate {
            48000 => 2,
            44100 => 1,
            _ => 0,
        };
        put(&mut f, &mut p, &[(source << 6) | (3 << 2)]); // source rate, joint stereo
        put(&mut f, &mut p, &[0]); // MP3 gain
        put(&mut f, &mut p, &[0, 0]); // preset, surround
        put(&mut f, &mut p, &(audio_bytes.min(u64::from(u32::MAX)) as u32).to_be_bytes());
        put(&mut f, &mut p, &self.crc.to_be_bytes());
        let tag_crc = crc16(0, &f[..p]);
        put(&mut f, &mut p, &tag_crc.to_be_bytes());
        f
    }

    fn header(&self, padding: bool, ms: bool) -> [u8; 4] {
        [
            0xFF,
            0xFB, // MPEG-1 Layer III, no CRC
            ((self.bitrate_index as u8) << 4) | ((self.ri as u8) << 2) | (u8::from(padding) << 1),
            (1 << 6) | (u8::from(ms) << 5) | (1 << 2), // joint stereo, original
        ]
    }

    /// Analyze every whole granule of input and write every frame that is ready.
    fn run(&mut self, out: &mut Vec<u8>) {
        while self.input[0].len() >= GRANULE {
            let sb = self.analyze();
            for ch in 0..2 {
                self.input[ch].drain(..GRANULE);
            }
            // granule `pending` becomes final now that its successor is known
            if let Some(cur) = self.pending.take() {
                let next_short = sb.attack;
                self.finalize(cur, next_short, out);
            }
            self.pending = Some(sb);
        }
    }

    /// Filterbank and attack detection of the next 576 input samples.
    fn analyze(&mut self) -> Subbands {
        let c = consts();
        let mut s = [[[0f32; 18]; 32]; 2];
        for t in 0..18 {
            self.xpos = (self.xpos + 1024 - 32) & 1023;
            for ch in 0..2 {
                let x = &mut self.x[ch];
                // newest sample first: X[0] is input 32t + 31
                for i in 0..32 {
                    let v = self.input[ch][32 * t + 31 - i];
                    x[(self.xpos + i) & 1023] = v;
                }
                let mut y = [0f32; 64];
                for (i, yi) in y.iter_mut().enumerate() {
                    let mut acc = 0f32;
                    for j in 0..8 {
                        let k = i + 64 * j;
                        acc += c.c[k] * x[(self.xpos + k) & 1023];
                    }
                    *yi = acc;
                }
                for k in 0..32 {
                    let row = &c.m[k];
                    let mut acc = 0f32;
                    for i in 0..64 {
                        acc += row[i] * y[i];
                    }
                    // frequency inversion: odd subbands, odd samples
                    s[ch][k][t] = if k & 1 == 1 && t & 1 == 1 { -acc } else { acc };
                }
            }
        }
        let attack = self.detect_attack();
        Subbands { s, attack }
    }

    /// An attack in the part of the input this granule's subband samples
    /// stand for (shifted by the filterbank's delay)?
    fn detect_attack(&mut self) -> bool {
        // the window [g*576 - 224, g*576 + 352): the previous tail and the
        // first 352 samples of this granule
        let mut seg = [0f32; 6];
        for ch in 0..2 {
            let mut st = self.hp[ch];
            let src = self.tail[ch].iter().chain(self.input[ch][..352].iter());
            for (n, &v) in src.enumerate() {
                let y = v - 2.0 * st[0] + st[1];
                st[1] = st[0];
                st[0] = v;
                seg[n / 96] += y * y;
            }
            self.hp[ch] = st;
            self.tail[ch].clear();
            self.tail[ch].extend_from_slice(&self.input[ch][352..GRANULE]);
        }
        let mut attack = false;
        let floor = 96.0 * 1e-6;
        for &e in &seg {
            let before = self.seg_hist.iter().copied().fold(0f32, f32::max);
            if e > floor && e > 10.0 * before.max(floor / 10.0) {
                attack = true;
            }
            self.seg_hist = [self.seg_hist[1], self.seg_hist[2], e];
        }
        attack
    }

    /// Choose the block type of granule `cur` and turn it into MDCT lines.
    fn finalize(&mut self, cur: Subbands, next_short: bool, out: &mut Vec<u8>) {
        let prev = self.prev.take().unwrap_or(Subbands { s: [[[0.0; 18]; 32]; 2], attack: false });
        let bt = if cur.attack {
            2
        } else if matches!(self.prev_block, 1 | 2) {
            if next_short { 2 } else { 3 }
        } else if next_short {
            1
        } else {
            0
        };
        let c = consts();
        let mut xr = [[0f32; 576]; 2];
        for ch in 0..2 {
            for sb in 0..32 {
                let mut z = [0f32; 36];
                z[..18].copy_from_slice(&prev.s[ch][sb]);
                z[18..].copy_from_slice(&cur.s[ch][sb]);
                if bt == 2 {
                    // three short MDCTs; window w's line k goes to line 6 sb + k of that window
                    for w in 0..3 {
                        for (k, row) in c.mdct12.iter().enumerate() {
                            let mut acc = 0f32;
                            for i in 0..12 {
                                acc += row[i] * z[6 + 6 * w + i] * c.win12[i];
                            }
                            // stored window-major: w * 192 + frequency line
                            xr[ch][w * 192 + 6 * sb + k] = acc;
                        }
                    }
                } else {
                    let win = &c.win[usize::from(bt)];
                    for (k, row) in c.mdct36.iter().enumerate() {
                        let mut acc = 0f32;
                        for i in 0..36 {
                            acc += row[i] * z[i] * win[i];
                        }
                        xr[ch][18 * sb + k] = acc;
                    }
                }
            }
            if bt != 2 {
                // alias reduction (the inverse of the decoder's butterflies)
                for sb in 1..32 {
                    for i in 0..8 {
                        let (u, d) = (18 * sb - 1 - i, 18 * sb + i);
                        let (bu, bd) = (xr[ch][u], xr[ch][d]);
                        xr[ch][u] = bu * c.cs[i] + bd * c.ca[i];
                        xr[ch][d] = bd * c.cs[i] - bu * c.ca[i];
                    }
                }
                for (v, &l) in xr[ch].iter_mut().zip(&self.lowpass) {
                    *v *= l;
                }
            } else {
                for w in 0..3 {
                    for (v, &l) in xr[ch][w * 192..w * 192 + 192].iter_mut().zip(&self.lowpass_short) {
                        *v *= l;
                    }
                }
            }
        }
        self.prev_block = bt;
        self.prev = Some(cur);
        self.frame.push((xr, bt));
        if self.frame.len() == 2 {
            let gr = std::mem::take(&mut self.frame);
            self.write_frame(&gr, out);
        }
    }

    /// Thresholds, mid/side, bit allocation, quantization and the bitstream
    /// of one frame (two granules).
    fn write_frame(&mut self, gr: &[([[f32; 576]; 2], u8)], out: &mut Vec<u8>) {
        let sfb_long = SFB_LONG[self.ri];
        let sfb_short = SFB_SHORT[self.ri];
        // ---- band layout, energies and thresholds per granule and channel ----
        struct Prep {
            bands: Vec<(usize, usize, usize)>, // (start, end, window) in bitstream order
            thr: [Vec<f32>; 2],
            xr: [[f32; 576]; 2], // bitstream order
        }
        let mut preps: Vec<Prep> = Vec::with_capacity(2);
        let (mut el, mut er, mut em, mut es) = (0f64, 0f64, 0f64, 0f64);
        for (xr, bt) in gr {
            let mut p = Prep { bands: Vec::with_capacity(39), thr: [Vec::new(), Vec::new()], xr: [[0.0; 576]; 2] };
            if *bt == 2 {
                for s in 0..13 {
                    let (a, b) = (usize::from(sfb_short[s]), usize::from(sfb_short[s + 1]));
                    for w in 0..3 {
                        let start = 3 * a + w * (b - a);
                        p.bands.push((start, start + (b - a), w));
                    }
                }
                for ch in 0..2 {
                    let mut thr = vec![0f32; 39];
                    for w in 0..3 {
                        let mut e = [0f32; 13];
                        for (s, es) in e.iter_mut().enumerate() {
                            *es = xr[ch][w * 192 + usize::from(sfb_short[s])..w * 192 + usize::from(sfb_short[s + 1])]
                                .iter()
                                .map(|v| v * v)
                                .sum();
                        }
                        let mut t = [0f32; 13];
                        Psy::thresholds(
                            &e,
                            &self.psy.short_spread,
                            &self.psy.short_ath,
                            &self.psy.short_offset,
                            &mut t,
                        );
                        for s in 0..13 {
                            thr[3 * s + w] = t[s];
                        }
                    }
                    p.thr[ch] = thr;
                    // bitstream order: band, window, line
                    for (k, &(start, end, w)) in p.bands.iter().enumerate() {
                        let a = usize::from(sfb_short[k / 3]);
                        for (j, i) in (start..end).enumerate() {
                            p.xr[ch][i] = xr[ch][w * 192 + a + j];
                        }
                    }
                }
            } else {
                for s in 0..22 {
                    p.bands.push((usize::from(sfb_long[s]), usize::from(sfb_long[s + 1]), 0));
                }
                for ch in 0..2 {
                    let mut e = [0f32; 22];
                    for (s, es) in e.iter_mut().enumerate() {
                        *es =
                            xr[ch][usize::from(sfb_long[s])..usize::from(sfb_long[s + 1])].iter().map(|v| v * v).sum();
                    }
                    let mut t = vec![0f32; 22];
                    Psy::thresholds(&e, &self.psy.long_spread, &self.psy.long_ath, &self.psy.long_offset, &mut t);
                    p.thr[ch] = t;
                    p.xr[ch] = xr[ch];
                }
            }
            for i in 0..576 {
                let (l, r) = (f64::from(p.xr[0][i]), f64::from(p.xr[1][i]));
                el += l * l;
                er += r * r;
                em += (l + r) * (l + r) / 2.0;
                es += (l - r) * (l - r) / 2.0;
            }
            preps.push(p);
        }
        // ---- mid/side: when it packs the energy into one channel better ----
        let frac = |a: f64, b: f64| if a + b > 0.0 { a.min(b) / (a + b) } else { 0.5 };
        let ms = el + er > 0.0 && frac(em, es) < 0.8 * frac(el, er);
        if ms {
            let s = std::f32::consts::FRAC_1_SQRT_2;
            for p in &mut preps {
                for i in 0..576 {
                    let (l, r) = (p.xr[0][i], p.xr[1][i]);
                    p.xr[0][i] = (l + r) * s;
                    p.xr[1][i] = (l - r) * s;
                }
                for b in 0..p.thr[0].len() {
                    let t = p.thr[0][b].min(p.thr[1][b]);
                    p.thr[0][b] = t;
                    p.thr[1][b] = t;
                }
            }
        }
        // ---- bits: shared out by perceptual entropy ----
        let n = self.frames;
        let len = self.frame_len(n);
        let main_bits = (len - 4 - 32) as u32 * 8;
        let mut pe = [0f64; 4];
        for (g, p) in preps.iter().enumerate() {
            for ch in 0..2 {
                let mut v = 0f64;
                for (b, &(start, end, _)) in p.bands.iter().enumerate() {
                    let e: f64 = p.xr[ch][start..end].iter().map(|&x| f64::from(x) * f64::from(x)).sum();
                    let ratio = e / f64::from(p.thr[ch][b]);
                    if ratio > 1.0 {
                        v += (end - start) as f64 * ratio.log2();
                    }
                }
                pe[2 * g + ch] = v;
            }
        }
        let total_pe: f64 = pe.iter().sum();
        let base = f64::from(main_bits) / 4.0 * 0.25;
        let spare = f64::from(main_bits) - 4.0 * base;
        let mut want = [0f64; 4];
        for k in 0..4 {
            want[k] = base + if total_pe > 0.0 { spare * pe[k] / total_pe } else { spare / 4.0 };
        }
        // ---- quantize, in bitstream order; unused bits go to the next ----
        let mut used_total = 0u32;
        let mut results: Vec<Gr> = Vec::with_capacity(4);
        for k in 0..4 {
            let (g, ch) = (k / 2, k % 2);
            let p = &preps[g];
            let left = main_bits - used_total;
            let later: f64 = want[k + 1..].iter().sum();
            let budget =
                (want[k] + (f64::from(left) - want[k] - later).max(0.0)).min(f64::from(left)).min(4095.0) as u32;
            let w = &mut self.work[ch];
            w.block_type = gr[g].1;
            w.n_sf = if w.block_type == 2 { 36 } else { 21 };
            w.bands.clear();
            for (b, &(start, end, _)) in p.bands.iter().enumerate() {
                w.bands.push(Band { start, end, thr: p.thr[ch][b] });
            }
            w.xr = p.xr[ch];
            for i in 0..576 {
                w.xr34[i] = w.xr[i].abs().powf(0.75);
            }
            outer_loop(w, &sfb_long, budget);
            used_total += w.part2_3_length;
            results.push(w.clone());
        }
        // ---- the frame ----
        let start = out.len();
        let padding = len > self.info_len();
        out.extend_from_slice(&self.header(padding, ms));
        {
            let mut bw = BitWriter::new(out);
            bw.put(0, 9); // main_data_begin
            bw.put(0, 3); // private bits
            bw.put(0, 8); // scfsi
            for r in &results {
                bw.put(r.part2_3_length, 12);
                bw.put(r.big_values as u32, 9);
                bw.put(r.global_gain as u32, 8);
                bw.put(r.scalefac_compress as u32, 4);
                if r.block_type != 0 {
                    bw.put(1, 1);
                    bw.put(u32::from(r.block_type), 2);
                    bw.put(0, 1); // not mixed
                    bw.put(r.table_select[0] as u32, 5);
                    bw.put(r.table_select[1] as u32, 5);
                    bw.put(0, 9); // subblock gains
                } else {
                    bw.put(0, 1);
                    for t in 0..3 {
                        bw.put(r.table_select[t] as u32, 5);
                    }
                    bw.put(r.region0_count as u32, 4);
                    bw.put(r.region1_count as u32, 3);
                }
                bw.put(0, 1); // preflag
                bw.put(u32::from(r.scalefac_scale), 1);
                bw.put(r.count1_table as u32, 1);
            }
            debug_assert_eq!(bw.bits, 256);
            for r in &results {
                let before = bw.bits;
                write_main(&mut bw, r, &sfb_long);
                debug_assert_eq!((bw.bits - before) as u32, r.part2_3_length);
            }
            bw.flush();
        }
        out.resize(start + len, 0);
        self.crc = crc16(self.crc, &out[start..]);
        self.frames += 1;
        self.bytes += len as u64;
    }
}

/// Scalefactors and Huffman data of one granule.
fn write_main(bw: &mut BitWriter, r: &Gr, sfb: &[u16; 23]) {
    let (s1, s2) = SLEN[r.scalefac_compress];
    let (s1, s2) = (u32::from(s1), u32::from(s2));
    let split = if r.block_type == 2 { 18 } else { 11 };
    for b in 0..r.n_sf {
        bw.put(r.sf[b] as u32, if b < split { s1 } else { s2 });
    }
    let big = 2 * r.big_values;
    let (e0, e1) = if r.block_type == 0 {
        (
            usize::from(sfb[r.region0_count + 1]).min(big),
            usize::from(sfb[r.region0_count + r.region1_count + 2]).min(big),
        )
    } else {
        (36.min(big), big)
    };
    for (t, a, b) in [(r.table_select[0], 0, e0), (r.table_select[1], e0, e1), (r.table_select[2], e1, big)] {
        if t == 0 || a >= b {
            continue;
        }
        let (xlen, codes) = HUFF[t];
        let lin = LINBITS[t];
        for p in (a..b).step_by(2) {
            let (x, y) = (r.ix[p], r.ix[p + 1]);
            let (cx, cy) = if lin > 0 { (x.min(15), y.min(15)) } else { (x, y) };
            let (code, len) = codes[cx as usize * xlen + cy as usize];
            bw.put(code, u32::from(len));
            for (v, line) in [(x, p), (y, p + 1)] {
                if lin > 0 && v >= 15 {
                    bw.put((v - 15) as u32, lin);
                }
                if v != 0 {
                    bw.put(u32::from(r.xr[line] < 0.0), 1);
                }
            }
        }
    }
    for q in (big..r.count1_end).step_by(4) {
        let p = (r.ix[q] << 3 | r.ix[q + 1] << 2 | r.ix[q + 2] << 1 | r.ix[q + 3]) as usize;
        let (code, len) = COUNT1[r.count1_table][p];
        bw.put(code, u32::from(len));
        for k in 0..4 {
            if r.ix[q + k] != 0 {
                bw.put(u32::from(r.xr[q + k] < 0.0), 1);
            }
        }
    }
}

/// CRC-16 as the LAME tag uses it (polynomial 0x8005, reflected).
pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

/// MDCT energy of a full-scale sine in one long block (once).
fn full_scale_energy() -> f64 {
    static E: OnceLock<f64> = OnceLock::new();
    *E.get_or_init(|| {
        // a 1 kHz sine of amplitude 1 at 48 kHz through the filterbank: the
        // energy of its MDCT lines in the steady state
        let mut enc = Encoder::bare();
        let mut total = 0f64;
        let mut count = 0;
        for g in 0..8 {
            for ch in 0..2 {
                enc.input[ch] = (0..GRANULE)
                    .map(|i| (2.0 * PI * 1000.0 * (g * GRANULE + i) as f64 / 48000.0).sin() as f32)
                    .collect();
            }
            let sb = enc.analyze();
            if g >= 4 {
                let c = consts();
                let prev = enc.prev.as_ref().map_or([[0f32; 18]; 32], |p| p.s[0]);
                let mut e = 0f64;
                for s in 0..32 {
                    let mut z = [0f32; 36];
                    z[..18].copy_from_slice(&prev[s]);
                    z[18..].copy_from_slice(&sb.s[0][s]);
                    for row in &c.mdct36 {
                        let mut acc = 0f32;
                        for i in 0..36 {
                            acc += row[i] * z[i] * c.win[0][i];
                        }
                        e += f64::from(acc) * f64::from(acc);
                    }
                }
                total += e;
                count += 1;
            }
            enc.prev = Some(sb);
        }
        total / f64::from(count)
    })
}

impl Encoder {
    /// An encoder with nothing but its filterbank (for `full_scale_energy`).
    fn bare() -> Encoder {
        Encoder {
            rate: 48000,
            ri: 1,
            kbps: 320,
            bitrate_index: 14,
            num: 1,
            den: 1,
            psy: Psy {
                long_spread: vec![],
                short_spread: vec![],
                long_ath: [0.0; 22],
                short_ath: [0.0; 13],
                long_offset: [0.0; 22],
                short_offset: [0.0; 13],
            },
            lowpass: [1.0; 576],
            lowpass_short: [1.0; 192],
            input: [Vec::new(), Vec::new()],
            x: [[0.0; 1024]; 2],
            xpos: 0,
            hp: [[0.0; 2]; 2],
            seg_hist: [0.0; 3],
            tail: [vec![0.0; 224], vec![0.0; 224]],
            prev: None,
            pending: None,
            prev_block: 0,
            frame: Vec::new(),
            work: [Gr::new(), Gr::new()],
            samples: 0,
            lead: 0,
            post: 0,
            frames: 0,
            bytes: 0,
            crc: 0,
            finished: false,
        }
    }
}
