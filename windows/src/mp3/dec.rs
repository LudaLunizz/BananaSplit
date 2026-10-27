//! MP3 decoder: MPEG-1, MPEG-2 and MPEG-2.5 Layer III, frame by frame.
//!
//! Every part of the format is handled (long, short and mixed blocks,
//! mid/side and intensity stereo, the bit reservoir, CRC-protected frames),
//! so the player plays MP3 files from anywhere, not only BananaSplit's own.
//! It is the textbook decoder of ISO/IEC 11172-3 and 13818-3; where the
//! standard leaves room (the regions of window-switched blocks) it does what
//! libmpg123 does, and the tests compare the two.
//!
//! Damaged input never panics: reads past the end give zeros, and a frame
//! whose bit reservoir is missing (the first frames after a seek) decodes as
//! silence of the right length.

use super::tables::{ALIAS_C, COUNT1, HUFF, INTWIN, KBPS, LINBITS, NR_OF_SFB, PRETAB, RATES, SFB_LONG, SFB_SHORT, SLEN};
use std::f64::consts::PI;
use std::sync::OnceLock;

/// A Layer III frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// MPEG-1 (else MPEG-2 or 2.5: one granule of 576 samples per frame).
    pub mpeg1: bool,
    /// Index into `tables::RATES`.
    pub rate_index: usize,
    pub sample_rate: u32,
    pub kbps: u32,
    pub padding: bool,
    pub crc: bool,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    pub mode: u8,
    /// Joint stereo: bit 1 = mid/side, bit 0 = intensity.
    pub mode_ext: u8,
    /// Bytes, header included.
    pub frame_len: usize,
}

impl Header {
    /// Parse a frame header. Only Layer III with a bitrate (not free format).
    pub fn parse(b: &[u8]) -> Option<Header> {
        if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = (b[1] >> 3) & 3; // 0 = MPEG-2.5, 1 = reserved, 2 = MPEG-2, 3 = MPEG-1
        let layer = (b[1] >> 1) & 3; // 1 = Layer III
        let bitrate_index = usize::from(b[2] >> 4);
        let sr = usize::from((b[2] >> 2) & 3);
        if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || sr == 3 {
            return None;
        }
        let mpeg1 = version == 3;
        let rate_index = match version {
            3 => sr,
            2 => 3 + sr,
            _ => 6 + sr,
        };
        let sample_rate = RATES[rate_index];
        let kbps = KBPS[usize::from(!mpeg1)][bitrate_index];
        let padding = b[2] & 2 != 0;
        let per = if mpeg1 { 144_000 } else { 72_000 };
        let frame_len = per * kbps as usize / sample_rate as usize + usize::from(padding);
        Some(Header {
            mpeg1,
            rate_index,
            sample_rate,
            kbps,
            padding,
            crc: b[1] & 1 == 0,
            mode: b[3] >> 6,
            mode_ext: (b[3] >> 4) & 3,
            frame_len,
        })
    }

    pub fn channels(&self) -> usize {
        if self.mode == 3 { 1 } else { 2 }
    }

    /// Samples per channel in the frame.
    pub fn samples(&self) -> usize {
        if self.mpeg1 { 1152 } else { 576 }
    }

    /// Bytes from the frame's start to its main data (header, CRC, side info).
    pub fn main_offset(&self) -> usize {
        let side = match (self.mpeg1, self.channels()) {
            (true, 1) => 17,
            (true, _) => 32,
            (false, 1) => 9,
            (false, _) => 17,
        };
        4 + if self.crc { 2 } else { 0 } + side
    }

    /// Two frames of one stream agree on these (the rest may change).
    pub fn same_stream(&self, o: &Header) -> bool {
        self.mpeg1 == o.mpeg1 && self.rate_index == o.rate_index && self.channels() == o.channels()
    }
}

// ---------------------------------------------------------------------------
// Bits and Huffman codes
// ---------------------------------------------------------------------------

/// MSB-first bit reader; reads past the end give zeros.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Bits<'a> {
        Bits { b, pos: 0 }
    }

    #[inline]
    fn bit(&mut self) -> u32 {
        let v = self.b.get(self.pos >> 3).map_or(0, |&x| u32::from(x >> (7 - (self.pos & 7)) & 1));
        self.pos += 1;
        v
    }

    #[inline]
    fn get(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }
}

/// A Huffman code as a binary tree: `nodes[i] = [child for 0, child for 1]`,
/// a child >= 0 is a node, `-1 - v` a leaf with value `v`.
struct Huffman {
    nodes: Vec<[i32; 2]>,
}

impl Huffman {
    /// From `(code, length)` per index; a table of `xlen` x `xlen` pairs
    /// gives `x << 4 | y`, a count1 table (`xlen` 0) its index.
    fn new(codes: &[(u32, u8)], xlen: usize) -> Huffman {
        let mut nodes = vec![[i32::MIN; 2]];
        for (k, &(code, len)) in codes.iter().enumerate() {
            let v = if xlen == 0 { k } else { (k / xlen) << 4 | (k % xlen) };
            let mut n = 0usize;
            for k in (0..len).rev() {
                let b = ((code >> k) & 1) as usize;
                if k == 0 {
                    nodes[n][b] = -1 - v as i32;
                } else {
                    if nodes[n][b] < 0 {
                        nodes.push([i32::MIN; 2]);
                        nodes[n][b] = (nodes.len() - 1) as i32;
                    }
                    n = nodes[n][b] as usize;
                }
            }
        }
        Huffman { nodes }
    }

    /// The next value, or `None` past `end` or on a code that does not exist.
    #[inline]
    fn decode(&self, bits: &mut Bits, end: usize) -> Option<u32> {
        let mut n = 0usize;
        loop {
            if bits.pos >= end {
                return None;
            }
            let c = self.nodes[n][bits.bit() as usize];
            if c == i32::MIN {
                return None;
            }
            if c < 0 {
                return Some((-1 - c) as u32);
            }
            n = c as usize;
        }
    }
}

// ---------------------------------------------------------------------------
// Constant tables, built once
// ---------------------------------------------------------------------------

struct Consts {
    /// |x|^(4/3) for every value a Huffman table can give (0..=8206).
    pow43: Vec<f32>,
    /// IMDCT cosines: long [36][18], short [12][6].
    cos36: [[f32; 18]; 36],
    cos12: [[f32; 6]; 12],
    /// 36-point windows of block types 0, 1, (2 unused), 3, and the 12-point one.
    win: [[f32; 36]; 4],
    win12: [f32; 12],
    cs: [f32; 8],
    ca: [f32; 8],
    /// Synthesis matrix N[i][k] = cos((16 + i)(2k + 1) pi / 64), and D.
    synth_n: Vec<[f32; 32]>,
    synth_d: Vec<f32>,
    /// Big-value codes of table 0-15, 16 (16-23) and 24 (24-31), by number.
    huff: Vec<Option<Huffman>>,
    count1: [Huffman; 2],
}

fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| {
        let mut cos36 = [[0f32; 18]; 36];
        for (i, row) in cos36.iter_mut().enumerate() {
            for (k, c) in row.iter_mut().enumerate() {
                *c = (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos() as f32;
            }
        }
        let mut cos12 = [[0f32; 6]; 12];
        for (i, row) in cos12.iter_mut().enumerate() {
            for (k, c) in row.iter_mut().enumerate() {
                *c = (PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos() as f32;
            }
        }
        Consts {
            pow43: (0..=8206).map(|i| f64::from(i).powf(4.0 / 3.0) as f32).collect(),
            cos36,
            cos12,
            win: std::array::from_fn(|bt| std::array::from_fn(|i| mdct_window(bt as u8, i) as f32)),
            win12: std::array::from_fn(|i| (PI / 12.0 * (i as f64 + 0.5)).sin() as f32),
            cs: std::array::from_fn(|i| (1.0 / (1.0 + ALIAS_C[i] * ALIAS_C[i]).sqrt()) as f32),
            ca: std::array::from_fn(|i| (ALIAS_C[i] / (1.0 + ALIAS_C[i] * ALIAS_C[i]).sqrt()) as f32),
            synth_n: (0..64)
                .map(|i| std::array::from_fn(|k| ((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos() as f32))
                .collect(),
            synth_d: (0..512).map(|i| window(i) as f32).collect(),
            huff: (0..32)
                .map(|t| (t < 16 && !HUFF[t].1.is_empty() || t == 16 || t == 24).then(|| Huffman::new(HUFF[t].1, HUFF[t].0)))
                .collect(),
            count1: [Huffman::new(&COUNT1[0], 0), Huffman::new(&COUNT1[1], 0)],
        }
    })
}

/// The synthesis window D[i] of ISO/IEC 11172-3 Table B.3 (the analysis
/// window C is D / 32): the prototype filter with every other block of 64
/// taps negated.
pub fn window(i: usize) -> f64 {
    let v = f64::from(INTWIN[if i <= 256 { i } else { 512 - i }]) / 65536.0;
    if (i / 64) % 2 == 1 { -v } else { v }
}

/// The 36-point (I)MDCT window of block type 0 (normal), 1 (start) or 3 (stop).
pub fn mdct_window(block_type: u8, i: usize) -> f64 {
    let s36 = |i: usize| (PI / 36.0 * (i as f64 + 0.5)).sin();
    let s12 = |i: usize| (PI / 12.0 * (i as f64 + 0.5)).sin();
    match block_type {
        1 => match i {
            0..18 => s36(i),
            18..24 => 1.0,
            24..30 => s12(i - 18),
            _ => 0.0,
        },
        3 => match i {
            0..6 => 0.0,
            6..12 => s12(i - 6),
            12..18 => 1.0,
            _ => s36(i),
        },
        _ => s36(i),
    }
}

// ---------------------------------------------------------------------------
// Side information
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Granule {
    part2_3_length: usize,
    big_values: usize,
    global_gain: i32,
    scalefac_compress: u32,
    /// 0 normal, 1 start, 2 short, 3 stop.
    block_type: u8,
    mixed: bool,
    table_select: [u8; 3],
    subblock_gain: [i32; 3],
    /// First line of region 1 and of region 2.
    region1: usize,
    region2: usize,
    preflag: bool,
    scalefac_scale: bool,
    count1_table: usize,
    /// MPEG-1 granule 1: bands 0-5, 6-10, 11-15, 16-20 reuse granule 0's
    /// scalefactors (bits 3..0).
    scfsi: u8,
}

fn read_side_info(bits: &mut Bits, h: &Header, gr: &mut [[Granule; 2]; 2]) -> Option<usize> {
    let nch = h.channels();
    let r = h.rate_index;
    let main_data_begin;
    let mut scfsi = [0u8; 2];
    if h.mpeg1 {
        main_data_begin = bits.get(9) as usize;
        bits.get(if nch == 1 { 5 } else { 3 });
        for s in scfsi.iter_mut().take(nch) {
            *s = bits.get(4) as u8;
        }
    } else {
        main_data_begin = bits.get(8) as usize;
        bits.get(if nch == 1 { 1 } else { 2 });
    }
    let ngr = if h.mpeg1 { 2 } else { 1 };
    for (g, granules) in gr.iter_mut().enumerate().take(ngr) {
        for (ch, out) in granules.iter_mut().enumerate().take(nch) {
            let mut x = Granule { part2_3_length: bits.get(12) as usize, ..Granule::default() };
            x.big_values = bits.get(9) as usize;
            if x.big_values > 288 {
                return None;
            }
            x.global_gain = bits.get(8) as i32;
            x.scalefac_compress = bits.get(if h.mpeg1 { 4 } else { 9 });
            if bits.bit() == 1 {
                x.block_type = bits.get(2) as u8;
                if x.block_type == 0 {
                    return None;
                }
                x.mixed = bits.bit() == 1;
                x.table_select[0] = bits.get(5) as u8;
                x.table_select[1] = bits.get(5) as u8;
                for s in &mut x.subblock_gain {
                    *s = bits.get(3) as i32;
                }
                // Implicit regions, as libmpg123 places them.
                let short = x.block_type == 2;
                x.region1 = if h.mpeg1 || (r < 6 && short) {
                    36
                } else if r < 6 {
                    54
                } else {
                    usize::from(SFB_LONG[r][if short && !x.mixed { 6 } else { 8 }])
                };
                x.region2 = 576;
            } else {
                for t in &mut x.table_select {
                    *t = bits.get(5) as u8;
                }
                let r0 = bits.get(4) as usize;
                let r1 = bits.get(3) as usize;
                x.region1 = usize::from(SFB_LONG[r][(r0 + 1).min(22)]);
                x.region2 = usize::from(SFB_LONG[r][(r0 + r1 + 2).min(22)]);
            }
            if h.mpeg1 {
                x.preflag = bits.bit() == 1;
            }
            x.scalefac_scale = bits.bit() == 1;
            x.count1_table = bits.bit() as usize;
            // scfsi applies to granule 1 of long blocks only
            x.scfsi = if g == 1 && x.block_type != 2 { scfsi[ch] } else { 0 };
            *out = x;
        }
    }
    Some(main_data_begin)
}

/// The scalefactor bands of a granule in bitstream order: the widths of its
/// long bands, then its short bands as (window 0, 1, 2) triplets.
struct Layout {
    widths: [u16; 40],
    n: usize,
    n_long: usize,
}

impl Layout {
    fn new(h: &Header, g: &Granule) -> Layout {
        let r = h.rate_index;
        let mut widths = [0u16; 40];
        let mut n = 0;
        let mut first_short = 13;
        let long_lines = match (g.block_type, g.mixed) {
            (2, false) => {
                first_short = 0;
                0
            }
            (2, true) => {
                first_short = 3;
                // the long part is two subbands (four at 8 kHz, where short
                // band 3 starts at line 72)
                usize::from(SFB_SHORT[r][3]) * 3
            }
            _ => 576,
        };
        for k in 0..22 {
            if usize::from(SFB_LONG[r][k + 1]) <= long_lines {
                widths[n] = SFB_LONG[r][k + 1] - SFB_LONG[r][k];
                n += 1;
            }
        }
        let n_long = n;
        for s in first_short..13 {
            for _ in 0..3 {
                widths[n] = SFB_SHORT[r][s + 1] - SFB_SHORT[r][s];
                n += 1;
            }
        }
        Layout { widths, n, n_long }
    }

    /// Line boundaries: `bounds[i]..bounds[i + 1]` is band entry `i`.
    fn bounds(&self) -> [usize; 41] {
        let mut b = [0usize; 41];
        for i in 0..self.n {
            b[i + 1] = b[i] + usize::from(self.widths[i]);
        }
        b
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// What one frame decoded to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub header: Header,
    /// False if the frame decoded as silence: its bit reservoir was missing
    /// (right after a seek) or its side information was damaged.
    pub complete: bool,
}

pub struct Decoder {
    /// Main data of the previous frames (the bit reservoir).
    reservoir: Vec<u8>,
    /// IMDCT overlap, [channel][subband * 18 + i].
    overlap: [[f32; 576]; 2],
    /// Synthesis filter state (the V vectors), newest at `vpos`.
    v: [[f32; 1024]; 2],
    vpos: usize,
    /// Granule 0's long-block scalefactors per channel (scfsi reuses them).
    scf: [[u8; 40]; 2],
    /// Intensity positions (the right channel's scalefactors; 255 = none).
    ist: [u8; 40],
    /// One granule: spectral lines, then subband samples.
    xr: [[f32; 576]; 2],
    data: Vec<u8>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        consts();
        Decoder {
            reservoir: Vec::with_capacity(1024),
            overlap: [[0.0; 576]; 2],
            v: [[0.0; 1024]; 2],
            vpos: 0,
            scf: [[0; 40]; 2],
            ist: [255; 40],
            xr: [[0.0; 576]; 2],
            data: Vec::with_capacity(4096),
        }
    }

    /// Forget everything (a new stream, or a seek).
    pub fn reset(&mut self) {
        self.reservoir.clear();
        self.overlap = [[0.0; 576]; 2];
        self.v = [[0.0; 1024]; 2];
        self.vpos = 0;
        self.scf = [[0; 40]; 2];
    }

    /// Decode one frame (`frame` starts with its header; bytes past its
    /// length are ignored). Appends `samples()` stereo frames to `out`
    /// (L R L R …; mono plays on both sides). `None` if it is no Layer III
    /// frame header.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<f32>) -> Option<Frame> {
        let h = Header::parse(frame)?;
        let frame = &frame[..frame.len().min(h.frame_len)];
        let side_start = 4 + if h.crc { 2 } else { 0 };
        let main = frame.get(h.main_offset()..).unwrap_or(&[]);
        let mut gr = [[Granule::default(); 2]; 2];
        let side = frame.get(side_start..h.main_offset()).unwrap_or(&[]);
        let main_data_begin = if side.len() == h.main_offset() - side_start {
            read_side_info(&mut Bits::new(side), &h, &mut gr)
        } else {
            None
        };
        let nch = h.channels();
        let ngr = if h.mpeg1 { 2 } else { 1 };
        let base = out.len();
        out.resize(base + 2 * h.samples(), 0.0);
        let mut complete = false;
        if let Some(mdb) = main_data_begin.filter(|&m| m <= self.reservoir.len()) {
            complete = true;
            let mut data = std::mem::take(&mut self.data);
            data.clear();
            data.extend_from_slice(&self.reservoir[self.reservoir.len() - mdb..]);
            data.extend_from_slice(main);
            let mut bits = Bits::new(&data);
            for (g, gi) in gr.iter().enumerate().take(ngr) {
                for ch in 0..nch {
                    let end = bits.pos + gi[ch].part2_3_length;
                    self.read_channel(&mut bits, &h, &gi[ch], ch, end);
                    bits.pos = end;
                }
                self.stereo(&h, gi);
                for ch in 0..nch {
                    self.to_subbands(&gi[ch], ch, h.rate_index);
                }
                self.synthesize(nch, &mut out[base + 1152 * g..base + 1152 * (g + 1)]);
            }
            self.data = data;
        } else {
            // Silence of the right length; the filters run down as usual.
            for g in 0..ngr {
                self.xr = [[0.0; 576]; 2];
                for ch in 0..nch {
                    self.to_subbands(&Granule::default(), ch, h.rate_index);
                }
                self.synthesize(nch, &mut out[base + 1152 * g..base + 1152 * (g + 1)]);
            }
        }
        self.reservoir.extend_from_slice(main);
        if self.reservoir.len() > 511 {
            let cut = self.reservoir.len() - 511;
            self.reservoir.drain(..cut);
        }
        Some(Frame { header: h, complete })
    }

    /// Scalefactors and Huffman data of one channel into `xr[ch]`.
    fn read_channel(&mut self, bits: &mut Bits, h: &Header, g: &Granule, ch: usize, end: usize) {
        let lay = Layout::new(h, g);
        let intensity_right = h.mode == 1 && h.mode_ext & 1 != 0 && ch == 1;
        let mut scf = [0u8; 40];
        let mut preflag = g.preflag;
        if h.mpeg1 {
            let (s1, s2) = SLEN[g.scalefac_compress as usize & 15];
            let (s1, s2) = (u32::from(s1), u32::from(s2));
            if g.block_type == 2 {
                // (mixed: 8 long bands, then short bands 3-11; else short 0-11)
                let mut i = 0;
                for _ in 0..lay.n_long {
                    scf[i] = bits.get(s1) as u8;
                    i += 1;
                }
                let first = if g.mixed { 3 } else { 0 };
                for sfb in first..12 {
                    let n = if sfb < 6 { s1 } else { s2 };
                    for _ in 0..3 {
                        scf[i] = bits.get(n) as u8;
                        i += 1;
                    }
                }
            } else {
                let prev = self.scf[ch];
                for (k, (a, b, n)) in [(0, 6, s1), (6, 11, s1), (11, 16, s2), (16, 21, s2)].into_iter().enumerate() {
                    let reuse = g.scfsi & (8 >> k) != 0;
                    for (i, s) in scf.iter_mut().enumerate().take(b).skip(a) {
                        *s = if reuse { prev[i] } else { bits.get(n) as u8 };
                    }
                }
                self.scf[ch] = scf;
            }
            if intensity_right {
                // 7 means "not intensity"; the top band is settled in `stereo`
                for (i, v) in self.ist.iter_mut().enumerate() {
                    *v = if i < lay.n && scf[i] != 7 { scf[i] } else { 255 };
                }
            }
        } else {
            preflag = self.lsf_scalefactors(bits, g, intensity_right, &mut scf);
        }

        // gain of every band entry
        let shift = if g.scalefac_scale { 2 } else { 1 };
        let mut gain = [0f32; 40];
        for (i, gv) in gain.iter_mut().enumerate().take(lay.n) {
            let mut e = g.global_gain - 210;
            if i < lay.n_long {
                let pre = if preflag { i32::from(PRETAB[i.min(21)]) } else { 0 };
                e -= (i32::from(scf[i]) + pre) << shift;
            } else {
                e -= 8 * g.subblock_gain[(i - lay.n_long) % 3] + (i32::from(scf[i]) << shift);
            }
            *gv = 2f64.powf(f64::from(e) / 4.0) as f32;
        }

        let c = consts();
        let xr = &mut self.xr[ch];
        *xr = [0.0; 576];
        let bounds = lay.bounds();
        let big = (g.big_values * 2).min(576);
        let mut line = 0;
        let mut band = 0;
        let put = |xr: &mut [f32; 576], l: usize, mag: f32, neg: bool, band: &mut usize| {
            while *band + 1 < lay.n && bounds[*band + 1] <= l {
                *band += 1;
            }
            let a = mag * gain[*band];
            xr[l] = if neg { -a } else { a };
        };
        for (reg, reg_end) in [g.region1.min(big), g.region2.min(big), big].into_iter().enumerate() {
            let t = usize::from(g.table_select[reg]) & 31;
            let lin = LINBITS[t];
            let table = c.huff[match t {
                16..24 => 16,
                24..32 => 24,
                _ => t,
            }]
            .as_ref();
            while line < reg_end && bits.pos < end {
                let xy = if t == 0 {
                    0
                } else {
                    match table.and_then(|hf| hf.decode(bits, end)) {
                        Some(v) => v,
                        None => break,
                    }
                };
                for (k, mut v) in [xy >> 4, xy & 15].into_iter().enumerate() {
                    if v == 15 && lin > 0 {
                        v += bits.get(lin);
                    }
                    if v != 0 {
                        let neg = bits.bit() == 1;
                        let mag = c.pow43.get(v as usize).copied().unwrap_or(0.0);
                        put(xr, line + k, mag, neg, &mut band);
                    }
                }
                line += 2;
            }
            line = line.max(reg_end);
        }
        // count1: quadruples of -1..1 until the granule's bits run out
        let quad = &c.count1[g.count1_table];
        line = big;
        'quads: while line + 4 <= 576 && bits.pos < end {
            let Some(q) = quad.decode(bits, end) else { break };
            for k in 0..4 {
                if q & (8 >> k) != 0 {
                    if bits.pos >= end {
                        break 'quads; // the code ran past the granule
                    }
                    let neg = bits.bit() == 1;
                    put(xr, line + k, 1.0, neg, &mut band);
                }
            }
            line += 4;
        }
    }

    /// MPEG-2/2.5 scalefactors (ISO/IEC 13818-3 2.4.3.2). Returns preflag.
    fn lsf_scalefactors(&mut self, bits: &mut Bits, g: &Granule, intensity_right: bool, scf: &mut [u8; 40]) -> bool {
        let block = match (g.block_type, g.mixed) {
            (2, false) => 1,
            (2, true) => 2,
            _ => 0,
        };
        let sfc = g.scalefac_compress;
        let (slen, table, preflag): ([u32; 4], usize, bool) = if !intensity_right {
            if sfc < 400 {
                ([(sfc >> 4) / 5, (sfc >> 4) % 5, (sfc & 15) >> 2, sfc & 3], 0, false)
            } else if sfc < 500 {
                let s = sfc - 400;
                ([(s >> 2) / 5, (s >> 2) % 5, s & 3, 0], 1, false)
            } else {
                let s = sfc - 500;
                ([s / 3, s % 3, 0, 0], 2, true)
            }
        } else {
            let s = sfc >> 1;
            if s < 180 {
                ([s / 36, (s % 36) / 6, s % 6, 0], 3, false)
            } else if s < 244 {
                let s = s - 180;
                ([s >> 4, (s >> 2) & 3, s & 3, 0], 4, false)
            } else {
                let s = s - 244;
                ([s / 3, s % 3, 0, 0], 5, false)
            }
        };
        let mut i = 0;
        for (k, &count) in NR_OF_SFB[block][table].iter().enumerate() {
            for _ in 0..count {
                let v = if slen[k] == 0 { 0 } else { bits.get(slen[k]) };
                if i < 40 {
                    scf[i] = v as u8;
                    if intensity_right {
                        // the largest value of the field means "not intensity"
                        self.ist[i] = if slen[k] > 0 && v == (1 << slen[k]) - 1 { 255 } else { v as u8 };
                    }
                }
                i += 1;
            }
        }
        if intensity_right {
            for v in self.ist.iter_mut().skip(i) {
                *v = 0;
            }
        }
        preflag
    }

    /// Mid/side and intensity stereo on `xr` (bitstream order).
    fn stereo(&mut self, h: &Header, gi: &[Granule; 2]) {
        if h.channels() != 2 || h.mode != 1 {
            return;
        }
        let ms = h.mode_ext & 2 != 0;
        if h.mode_ext & 1 == 0 {
            if ms {
                ms_lines(&mut self.xr, 0, 576);
            }
            return;
        }
        // Intensity stereo above the last band (per window) in which the
        // right channel has a line that is not zero.
        let g = &gi[1];
        let lay = Layout::new(h, g);
        let bounds = lay.bounds();
        let mut max_band = [-1i32; 3];
        for i in 0..lay.n {
            if self.xr[1][bounds[i]..bounds[i + 1]].iter().any(|&v| v != 0.0) {
                max_band[if i < lay.n_long { 0 } else { (i - lay.n_long) % 3 }] = i as i32;
            }
        }
        let blocks = if lay.n > lay.n_long { 3 } else { 1 };
        if lay.n_long > 0 {
            let m = max_band[0].max(max_band[1]).max(max_band[2]);
            max_band = [m; 3];
        }
        let mut ist = self.ist;
        // the top band (no scalefactor of its own) takes the one below's position
        for k in 0..blocks {
            let itop = lay.n - blocks + k;
            let prev = itop.saturating_sub(blocks);
            ist[itop] = if max_band[k] >= prev as i32 { if h.mpeg1 { 3 } else { 0 } } else { ist[prev] };
        }
        let sh = g.scalefac_compress & 1;
        for i in 0..lay.n {
            let (a, b) = (bounds[i], bounds[i + 1]);
            let win = if i < lay.n_long { 0 } else { (i - lay.n_long) % 3 };
            let ipos = u32::from(ist[i]);
            let valid = if h.mpeg1 { ipos < 7 } else { ipos < 64 };
            if i as i32 > max_band[win] && valid {
                // (no 1/sqrt 2 here: the left channel holds the sum as it is;
                // decoders that scale a mid/side frame's gains by it multiply
                // these by sqrt 2 to undo that)
                let (kl, kr) = if h.mpeg1 {
                    let t = (f64::from(ipos) * PI / 12.0).tan();
                    if ipos == 6 { (1.0, 0.0) } else { (t / (1.0 + t), 1.0 / (1.0 + t)) }
                } else {
                    let k = 2f64.powf(-f64::from(((ipos + 1) >> 1) << sh) / 4.0);
                    if ipos & 1 == 1 { (k, 1.0) } else { (1.0, k) }
                };
                let (kl, kr) = (kl as f32, kr as f32);
                for l in a..b {
                    let v = self.xr[0][l];
                    self.xr[0][l] = v * kl;
                    self.xr[1][l] = v * kr;
                }
            } else if ms {
                ms_lines(&mut self.xr, a, b);
            }
        }
    }

    /// Reorder (short blocks), anti-alias and inverse-MDCT one channel; its
    /// `xr` then holds 18 subband samples per subband.
    fn to_subbands(&mut self, g: &Granule, ch: usize, rate_index: usize) {
        let c = consts();
        let xr = &mut self.xr[ch];
        let long_sb = match (g.block_type, g.mixed) {
            (2, false) => 0,
            (2, true) => usize::from(SFB_SHORT[rate_index][3]) / 6,
            _ => 32,
        };
        if long_sb < 32 {
            // (band, window, line) -> per subband: line k of window w at 3k + w
            let mut tmp = [0f32; 576];
            let start = 18 * long_sb;
            tmp[..start].copy_from_slice(&xr[..start]);
            let mut src = start;
            for s in if long_sb > 0 { 3 } else { 0 }..13 {
                let (a, b) = (usize::from(SFB_SHORT[rate_index][s]), usize::from(SFB_SHORT[rate_index][s + 1]));
                for w in 0..3 {
                    for f in a..b {
                        if let (Some(d), Some(&v)) = (tmp.get_mut(3 * f + w), xr.get(src)) {
                            *d = v;
                        }
                        src += 1;
                    }
                }
            }
            *xr = tmp;
        }
        for sb in 1..long_sb {
            for i in 0..8 {
                let (u, d) = (18 * sb - 1 - i, 18 * sb + i);
                let (bu, bd) = (xr[u], xr[d]);
                xr[u] = bu * c.cs[i] - bd * c.ca[i];
                xr[d] = bd * c.cs[i] + bu * c.ca[i];
            }
        }
        let ov = &mut self.overlap[ch];
        for sb in 0..32 {
            let x = &mut xr[18 * sb..18 * sb + 18];
            let o = &mut ov[18 * sb..18 * sb + 18];
            let mut z = [0f32; 36];
            if sb < long_sb {
                // the long subbands of a mixed block use the normal window
                // (also when the flag comes with a start or stop block)
                let mixed_long = if rate_index == 8 { 4 } else { 2 };
                let w = &c.win[if g.mixed && sb < mixed_long { 0 } else { usize::from(g.block_type) }];
                for (i, zi) in z.iter_mut().enumerate() {
                    let mut s = 0f32;
                    for (k, &xk) in x.iter().enumerate() {
                        s += xk * c.cos36[i][k];
                    }
                    *zi = s * w[i];
                }
            } else {
                for w in 0..3 {
                    for i in 0..12 {
                        let mut s = 0f32;
                        for k in 0..6 {
                            s += x[3 * k + w] * c.cos12[i][k];
                        }
                        z[6 + 6 * w + i] += s * c.win12[i];
                    }
                }
            }
            for i in 0..18 {
                x[i] = z[i] + o[i];
                o[i] = z[18 + i];
            }
            if sb & 1 == 1 {
                for i in (1..18).step_by(2) {
                    x[i] = -x[i];
                }
            }
        }
    }

    /// Polyphase synthesis of one granule into `pcm` (576 stereo frames).
    fn synthesize(&mut self, nch: usize, pcm: &mut [f32]) {
        let c = consts();
        for t in 0..18 {
            self.vpos = (self.vpos + 1024 - 64) & 1023;
            for ch in 0..nch {
                let s: [f32; 32] = std::array::from_fn(|k| self.xr[ch][18 * k + t]);
                let v = &mut self.v[ch];
                for (i, n) in c.synth_n.iter().enumerate() {
                    let mut acc = 0f32;
                    for k in 0..32 {
                        acc += n[k] * s[k];
                    }
                    v[(self.vpos + i) & 1023] = acc;
                }
                for j in 0..32 {
                    let mut acc = 0f32;
                    for i in 0..8 {
                        acc += v[(self.vpos + 128 * i + j) & 1023] * c.synth_d[64 * i + j]
                            + v[(self.vpos + 128 * i + 96 + j) & 1023] * c.synth_d[64 * i + 32 + j];
                    }
                    let acc = if acc.is_finite() { acc } else { 0.0 };
                    let o = 2 * (32 * t + j);
                    pcm[o + ch] = acc;
                    if nch == 1 {
                        pcm[o + 1] = acc;
                    }
                }
            }
        }
    }
}

fn ms_lines(xr: &mut [[f32; 576]; 2], a: usize, b: usize) {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    for l in a..b {
        let (m, d) = (xr[0][l], xr[1][l]);
        xr[0][l] = (m + d) * s;
        xr[1][l] = (m - d) * s;
    }
}
