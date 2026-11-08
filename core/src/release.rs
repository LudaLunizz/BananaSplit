//! Releases of BananaSplit, the platform-free half of each app's updater
//! (`update.rs`): where releases come from, the JSON of GitHub's release
//! list, SHA-256 and SHA-512, Ed25519 signature checks of
//! `SHA256SUMS.txt`, versions, the release file of each platform, picking
//! the newest release for this computer, and GitHub's times.
//!
//! Plain Rust with no dependencies: the apps download with the system's
//! `curl` or `wget` and install in their own way; everything they decide
//! with is here, the same on every platform.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

pub const REPO: &str = "LudaLunizz/BananaSplit";
pub const RELEASES_PAGE: &str = "https://github.com/LudaLunizz/BananaSplit/releases";
/// Tests only: `http://127.0.0.1:PORT`, a stand-in for GitHub (see
/// [`Source::from_env`]).
pub const SOURCE_ENV: &str = "BANANASPLIT_UPDATE_SOURCE";
/// Automatic updates leave a release alone this long after it is published:
/// a broken release can be withdrawn before it reaches everyone.
pub const SETTLE: Duration = Duration::from_secs(48 * 3600);
pub const MAX_SUMS: u64 = 64 << 10;
pub const MAX_SIG: u64 = 1 << 10;
pub const MAX_ASSET: u64 = 512 << 20;
pub const MAX_NOTES: usize = 4000;

// ---------------------------------------------------------------------------
// Where releases come from
// ---------------------------------------------------------------------------

/// The release list and the prefixes every link in it must have.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub api: String,
    /// Every download link starts with this.
    pub download: String,
    /// A release's page starts with this.
    pub pages: String,
    pub https: bool,
    /// The public key `SHA256SUMS.txt` must be signed with.
    pub key: [u8; 32],
}

impl Source {
    pub fn github() -> Source {
        Source {
            api: format!("https://api.github.com/repos/{REPO}/releases?per_page=30"),
            download: format!("https://github.com/{REPO}/releases/download/"),
            pages: format!("https://github.com/{REPO}/releases/"),
            https: true,
            key: release_key(),
        }
    }

    /// GitHub, unless a test points [`SOURCE_ENV`] at a server on this
    /// computer (`http://127.0.0.1:PORT` or `http://localhost:PORT`, nothing
    /// else is accepted), with its own key ([`KEY_ENV`]) if one is given.
    pub fn from_env() -> Source {
        std::env::var(SOURCE_ENV).ok().and_then(|b| Source::local(&b)).unwrap_or_else(Source::github)
    }

    pub fn local(base: &str) -> Option<Source> {
        let port = base.strip_prefix("http://127.0.0.1:").or_else(|| base.strip_prefix("http://localhost:"))?;
        if port.is_empty()
            || port.len() > 5
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u16>().ok()? == 0
        {
            return None;
        }
        Some(Source {
            api: format!("{base}/repos/{REPO}/releases?per_page=30"),
            download: format!("{base}/{REPO}/releases/download/"),
            pages: format!("{base}/{REPO}/releases/"),
            https: false,
            key: std::env::var(KEY_ENV).ok().and_then(|k| parse_hex32(k.trim())).unwrap_or_else(release_key),
        })
    }

    pub fn is_test(&self) -> bool {
        !self.https
    }
}

// ---------------------------------------------------------------------------
// JSON (the release list), no dependencies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = JsonParser { b: s.as_bytes(), i: 0, depth: 0 };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("unexpected text after the JSON value at byte {}", p.i));
        }
        Ok(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// A whole number in 0 ..= 2^53.
    pub fn u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_992.0 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }
}

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("bad JSON at byte {}: {what}", self.i))
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("unknown word")
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.b.get(self.i) {
            None => self.err("unexpected end"),
            Some(b'{') => self.nested(true),
            Some(b'[') => self.nested(false),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => self.err("unexpected character"),
        }
    }

    fn nested(&mut self, obj: bool) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > 64 {
            return self.err("nested too deeply");
        }
        self.i += 1;
        let close = if obj { b'}' } else { b']' };
        let (mut map, mut list) = (Vec::new(), Vec::new());
        self.ws();
        if self.b.get(self.i) == Some(&close) {
            self.i += 1;
        } else {
            loop {
                self.ws();
                if obj {
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected a key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    self.ws();
                    map.push((k, self.value()?));
                } else {
                    list.push(self.value()?);
                }
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(c) if *c == close => {
                        self.i += 1;
                        break;
                    }
                    _ => return self.err("expected ',' or the end of the list"),
                }
            }
        }
        self.depth -= 1;
        Ok(if obj { Json::Obj(map) } else { Json::Arr(list) })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .b
            .get(self.i..self.i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok());
        match h {
            Some(v) => {
                self.i += 4;
                Ok(v)
            }
            None => self.err("bad \\u escape"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // the opening quote
        let mut s = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && !matches!(self.b[self.i], b'"' | b'\\') && self.b[self.i] >= 0x20 {
                self.i += 1;
            }
            // Only ASCII bytes stop the run, so it ends on a character boundary.
            s.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad UTF-8 in JSON".to_string())?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(s);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else { return self.err("unexpected end") };
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xD800..0xDC00).contains(&c) && self.b[self.i..].starts_with(b"\\u") {
                                let at = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    c = 0x10000 + ((c - 0xD800) << 10) + (lo - 0xDC00);
                                } else {
                                    self.i = at; // not a pair: the first one alone
                                }
                            }
                            s.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                Some(_) => return self.err("control character in a string"),
                None => return self.err("unterminated string"),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.i < p.b.len() && p.b[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i - s
        };
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        let int_start = self.i;
        let n = digits(self);
        if n == 0 || (n > 1 && self.b[int_start] == b'0') {
            return self.err("bad number");
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return self.err("bad number");
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return self.err("bad number");
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("x");
        text.parse::<f64>().map(Json::Num).or_else(|_| self.err("bad number"))
    }
}

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4)
// ---------------------------------------------------------------------------

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    used: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            used: 0,
            total: 0,
        }
    }
}

impl Sha256 {
    fn block(h: &mut [u32; 8], b: &[u8]) {
        let mut w = [0u32; 64];
        for (t, c) in b.as_chunks::<4>().0.iter().enumerate() {
            w[t] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for t in 16..64 {
            let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
            let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
        }
        let [mut a, mut b2, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for t in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[t]).wrapping_add(w[t]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b2) ^ (a & c) ^ (b2 & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b2;
            b2 = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in h.iter_mut().zip([a, b2, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.used > 0 {
            let n = (64 - self.used).min(data.len());
            self.buf[self.used..self.used + n].copy_from_slice(&data[..n]);
            self.used += n;
            data = &data[n..];
            if self.used < 64 {
                return;
            }
            let buf = self.buf;
            Sha256::block(&mut self.h, &buf);
            self.used = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for c in blocks {
            Sha256::block(&mut self.h, c);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.used = rest.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (o, h) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.h) {
            o.copy_from_slice(&h.to_be_bytes());
        }
        out
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut s = Sha256::default();
    s.update(data);
    s.finish()
}

pub fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut s = Sha256::default();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            return Ok(s.finish());
        }
        s.update(&buf[..n]);
    }
}

pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// 64 hexadecimal digits (either case).
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.as_bytes();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (k, o) in out.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&s[2 * k..2 * k + 2]).ok()?;
        if !pair.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *o = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// SHA-512 (FIPS 180-4), for Ed25519
// ---------------------------------------------------------------------------

const K512: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

/// SHA-512 of the pieces, one after the other (all in memory: a checksum
/// file and its signature are small).
pub fn sha512(pieces: &[&[u8]]) -> [u8; 64] {
    let mut h: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    let mut data: Vec<u8> = pieces.concat();
    let bits = (data.len() as u128).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 128 != 112 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    for b in data.as_chunks::<128>().0 {
        let mut w = [0u64; 80];
        for (t, c) in b.as_chunks::<8>().0.iter().enumerate() {
            w[t] = u64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        }
        for t in 16..80 {
            let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
            let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
            w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
        }
        let [mut a, mut b2, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for t in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[t]).wrapping_add(w[t]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b2) ^ (a & c) ^ (b2 & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b2;
            b2 = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in h.iter_mut().zip([a, b2, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 64];
    for (o, x) in out.as_chunks_mut::<8>().0.iter_mut().zip(h) {
        o.copy_from_slice(&x.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Ed25519 signatures (RFC 8032), verifying only
// ---------------------------------------------------------------------------
//
// A release's SHA256SUMS.txt is signed with a key that is never on GitHub:
// whoever can publish a release there still cannot make one this updater
// installs. Verifying needs no secret and no constant time.

/// The public key of the releases (`packaging/sign-release.sh` signs with
/// its private half).
pub const SIGNING_KEY: &str = "eb6afcb3176c1f908744169710381148c6dde645a669a6f3f04c61e383056e8a";
/// Tests only, with [`SOURCE_ENV`]: the public key of the test releases.
pub const KEY_ENV: &str = "BANANASPLIT_UPDATE_KEY";

pub fn release_key() -> [u8; 32] {
    parse_hex32(SIGNING_KEY).expect("SIGNING_KEY is 64 hexadecimal digits")
}

/// A number modulo 2^255 - 19, in five 51-bit limbs.
#[derive(Clone, Copy, Debug)]
struct Fe([u64; 5]);

const MASK51: u64 = (1 << 51) - 1;

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// The low 255 bits (the top bit is the caller's).
    fn from_bytes(b: &[u8; 32]) -> Fe {
        let load =
            |i: usize| u64::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3], b[i + 4], b[i + 5], b[i + 6], b[i + 7]]);
        Fe([
            load(0) & MASK51,
            (load(6) >> 3) & MASK51,
            (load(12) >> 6) & MASK51,
            (load(19) >> 1) & MASK51,
            (load(24) >> 12) & MASK51,
        ])
    }

    /// Limbs back under 2^51 + 2^13.
    fn reduce(mut l: [u64; 5]) -> Fe {
        let c = [l[0] >> 51, l[1] >> 51, l[2] >> 51, l[3] >> 51, l[4] >> 51];
        for x in &mut l {
            *x &= MASK51;
        }
        l[0] += c[4] * 19;
        l[1] += c[0];
        l[2] += c[1];
        l[3] += c[2];
        l[4] += c[3];
        Fe(l)
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut l = Fe::reduce(self.0).0;
        // subtract p once if the number is p or more
        let mut q = (l[0] + 19) >> 51;
        for x in &l[1..] {
            q = (x + q) >> 51;
        }
        l[0] += 19 * q;
        for i in 0..4 {
            l[i + 1] += l[i] >> 51;
            l[i] &= MASK51;
        }
        l[4] &= MASK51;
        let mut out = [0u8; 32];
        let (mut acc, mut bits, mut k) = (0u128, 0, 0);
        for x in l {
            acc |= u128::from(x) << bits;
            bits += 51;
            while bits >= 8 {
                out[k] = acc as u8;
                acc >>= 8;
                bits -= 8;
                k += 1;
            }
        }
        out[31] = acc as u8;
        out
    }

    fn add(self, b: Fe) -> Fe {
        let (a, b) = (self.0, b.0);
        Fe::reduce([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]])
    }

    fn sub(self, b: Fe) -> Fe {
        // + 16p, so that no limb goes below zero
        let (a, b) = (self.0, b.0);
        Fe::reduce([
            (a[0] + 36028797018963664) - b[0],
            (a[1] + 36028797018963952) - b[1],
            (a[2] + 36028797018963952) - b[2],
            (a[3] + 36028797018963952) - b[3],
            (a[4] + 36028797018963952) - b[4],
        ])
    }

    fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    fn mul(self, b: Fe) -> Fe {
        let m = |x: u64, y: u64| u128::from(x) * u128::from(y);
        let [a0, a1, a2, a3, a4] = self.0;
        let [b0, b1, b2, b3, b4] = b.0;
        let (b1_19, b2_19, b3_19, b4_19) = (b1 * 19, b2 * 19, b3 * 19, b4 * 19);
        let mut c = [
            m(a0, b0) + m(a1, b4_19) + m(a2, b3_19) + m(a3, b2_19) + m(a4, b1_19),
            m(a0, b1) + m(a1, b0) + m(a2, b4_19) + m(a3, b3_19) + m(a4, b2_19),
            m(a0, b2) + m(a1, b1) + m(a2, b0) + m(a3, b4_19) + m(a4, b3_19),
            m(a0, b3) + m(a1, b2) + m(a2, b1) + m(a3, b0) + m(a4, b4_19),
            m(a0, b4) + m(a1, b3) + m(a2, b2) + m(a3, b1) + m(a4, b0),
        ];
        let mask = u128::from(MASK51);
        for i in 0..4 {
            c[i + 1] += c[i] >> 51;
            c[i] &= mask;
        }
        c[0] += (c[4] >> 51) * 19;
        c[4] &= mask;
        c[1] += c[0] >> 51;
        c[0] &= mask;
        Fe([c[0] as u64, c[1] as u64, c[2] as u64, c[3] as u64, c[4] as u64])
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    /// `self` to the power `e` (little-endian).
    fn pow(self, e: &[u8; 32]) -> Fe {
        let mut r = Fe::ONE;
        for i in (0..256).rev() {
            r = r.square();
            if (e[i / 8] >> (i % 8)) & 1 == 1 {
                r = r.mul(self);
            }
        }
        r
    }

    /// p - 2, (p - 5) / 8 and (p - 1) / 4, little-endian.
    fn exponent(low: u8, high: u8) -> [u8; 32] {
        let mut e = [0xff; 32];
        e[0] = low;
        e[31] = high;
        e
    }

    fn invert(self) -> Fe {
        self.pow(&Fe::exponent(0xeb, 0x7f))
    }

    fn eq(self, b: Fe) -> bool {
        self.to_bytes() == b.to_bytes()
    }

    fn is_odd(self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    fn small(n: u64) -> Fe {
        Fe([n, 0, 0, 0, 0])
    }
}

/// A point of the curve, in extended coordinates (X:Y:Z:T), x = X/Z,
/// y = Y/Z, xy = T/Z.
#[derive(Clone, Copy, Debug)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// The curve's d = -121665/121666.
fn curve_d() -> Fe {
    Fe::small(121665).neg().mul(Fe::small(121666).invert())
}

impl Point {
    const IDENTITY: Point = Point { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO };

    /// RFC 8032 5.1.3. `None` for anything that is not a point.
    fn decode(b: &[u8; 32], d: Fe) -> Option<Point> {
        let sign = b[31] >> 7;
        let mut yb = *b;
        yb[31] &= 0x7f;
        let y = Fe::from_bytes(&yb);
        if y.to_bytes() != yb {
            return None; // y is p or more
        }
        let yy = y.square();
        let u = yy.sub(Fe::ONE);
        let v = d.mul(yy).add(Fe::ONE);
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v7).pow(&Fe::exponent(0xfd, 0x0f)));
        let vxx = v.mul(x.square());
        if !vxx.eq(u) {
            if !vxx.eq(u.neg()) {
                return None;
            }
            x = x.mul(Fe::small(2).pow(&Fe::exponent(0xfb, 0x1f))); // the square root of -1
        }
        if x.eq(Fe::ZERO) && sign == 1 {
            return None;
        }
        if u8::from(x.is_odd()) != sign {
            x = x.neg();
        }
        Some(Point { x, y, z: Fe::ONE, t: x.mul(y) })
    }

    fn encode(self) -> [u8; 32] {
        let zi = self.z.invert();
        let mut b = self.y.mul(zi).to_bytes();
        b[31] |= u8::from(self.x.mul(zi).is_odd()) << 7;
        b
    }

    /// RFC 8032 5.1.4 (also right for doubling).
    fn add(self, q: Point, d2: Fe) -> Point {
        let a = self.y.sub(self.x).mul(q.y.sub(q.x));
        let b = self.y.add(self.x).mul(q.y.add(q.x));
        let c = self.t.mul(d2).mul(q.t);
        let d = self.z.add(self.z).mul(q.z);
        let (e, f, g, h) = (b.sub(a), d.sub(c), d.add(c), b.add(a));
        Point { x: e.mul(f), y: g.mul(h), z: f.mul(g), t: e.mul(h) }
    }

    fn neg(self) -> Point {
        Point { x: self.x.neg(), t: self.t.neg(), ..self }
    }

    /// `k` (little-endian, any length) times this point.
    fn times(self, k: &[u8], d2: Fe) -> Point {
        let mut r = Point::IDENTITY;
        for i in (0..k.len() * 8).rev() {
            r = r.add(r, d2);
            if (k[i / 8] >> (i % 8)) & 1 == 1 {
                r = r.add(self, d2);
            }
        }
        r
    }
}

/// The order of the base point, L = 2^252 + 27742317777372353535851937790883648493, little-endian.
const ORDER: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

/// Is `sig` `key`'s Ed25519 signature of `msg`? (RFC 8032 5.1.7, the
/// cofactorless check.)
pub fn ed25519_verify(key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let d = curve_d();
    let d2 = d.add(d);
    let Some(a) = Point::decode(key, d) else { return false };
    let (r, s) = (&sig[..32], &sig[32..]);
    // S < L, compared from the top byte down
    if s.iter().rev().cmp(ORDER.iter().rev()) != std::cmp::Ordering::Less {
        return false;
    }
    let mut base = [0x66u8; 32];
    base[0] = 0x58;
    let Some(b) = Point::decode(&base, d) else { return false };
    // k = SHA-512(R || A || M), used whole: [k]A is [k mod L]A for A in the
    // prime-order group, and the key is ours.
    let k = sha512(&[r, key, msg]);
    // [S]B - [k]A must be R
    b.times(s, d2).add(a.neg().times(&k, d2), d2).encode() == *r
}

/// 128 hexadecimal digits (a signature file, written by sign-release.sh).
pub fn parse_sig(text: &str) -> Option<[u8; 64]> {
    let t = text.trim();
    if t.len() != 128 {
        return None;
    }
    let (a, b) = (parse_hex32(&t[..64])?, parse_hex32(&t[64..])?);
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&a);
    out[32..].copy_from_slice(&b);
    Some(out)
}

/// Is `sums` (a release's SHA256SUMS.txt) signed by `key`?
pub fn check_signature(sums: &str, sig: &str, key: &[u8; 32]) -> Result<(), String> {
    let s = parse_sig(sig).ok_or("the release's SHA256SUMS.txt.sig is not a signature")?;
    if ed25519_verify(key, sums.as_bytes(), &s) {
        Ok(())
    } else {
        Err("the release's SHA256SUMS.txt is not signed with BananaSplit's release key (not an official release); nothing was changed".into())
    }
}

// ---------------------------------------------------------------------------
// Versions and release files
// ---------------------------------------------------------------------------

/// `2.7.3`: one to four whole numbers.
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    parts
        .iter()
        .map(
            |p| {
                if !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) {
                    p.parse().ok()
                } else {
                    None
                }
            },
        )
        .collect()
}

/// Is version `a` newer than `b`? (`2.10` is newer than `2.9`; `2.7` and
/// `2.7.0` are the same.)
pub fn newer(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (parse_version(a), parse_version(b)) else { return false };
    let n = x.len().max(y.len());
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    for i in 0..n {
        if at(&x, i) != at(&y, i) {
            return at(&x, i) > at(&y, i);
        }
    }
    false
}

/// The release file for this computer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Package {
    /// `bananasplit-V-linux-x86_64.tar.gz` (the portable package)
    Tarball,
    /// `bananasplit_V_amd64.deb`
    Deb,
    /// `bananasplit-V-windows-x64.exe`
    WindowsExe,
    /// `bananasplit-V-macos.zip` (the app, for Apple silicon and Intel)
    MacZip,
}

impl Package {
    fn affixes(self) -> (&'static str, &'static str) {
        match self {
            Package::Tarball => ("bananasplit-", "-linux-x86_64.tar.gz"),
            Package::Deb => ("bananasplit_", "_amd64.deb"),
            Package::WindowsExe => ("bananasplit-", "-windows-x64.exe"),
            Package::MacZip => ("bananasplit-", "-macos.zip"),
        }
    }

    pub fn file_name(self, version: &str) -> String {
        let (a, b) = self.affixes();
        format!("{a}{version}{b}")
    }

    /// The version in a file name of this kind.
    pub fn version_of(self, name: &str) -> Option<String> {
        let (a, b) = self.affixes();
        let v = name.strip_prefix(a)?.strip_suffix(b)?;
        parse_version(v).map(|_| v.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
    /// GitHub's SHA-256 of the upload.
    pub digest: Option<[u8; 32]>,
}

/// The newest release that has this computer's file.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub version: String,
    pub tag: String,
    /// The release's page (or the list of releases).
    pub page: String,
    /// Its notes, as written (shown as plain text).
    pub notes: String,
    pub asset: Asset,
    /// Its `SHA256SUMS.txt`, if it has one.
    pub sums: Option<Asset>,
    /// When it was published (seconds since 1970), if GitHub says.
    pub published: Option<u64>,
    /// Its `SHA256SUMS.txt.sig`, if it has one.
    pub sig: Option<Asset>,
}

pub fn clean_url(u: &str, prefix: &str) -> bool {
    let Some(rest) = u.strip_prefix(prefix) else { return false };
    u.len() < 2048
        && !u.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
        // no way out of the prefix: no dot segments, also not percent-encoded
        && !rest.split('/').any(|seg| seg == "." || seg == "..")
        && !rest.to_ascii_lowercase().contains("%2e")
}

pub fn asset_of(a: &Json, src: &Source, max: u64) -> Option<Asset> {
    if a.get("state").and_then(Json::str).is_some_and(|s| s != "uploaded") {
        return None;
    }
    let name = a.get("name")?.str()?.to_string();
    let url = a.get("browser_download_url")?.str()?.to_string();
    let size = a.get("size")?.u64()?;
    if !clean_url(&url, &src.download) || !url.ends_with(&format!("/{name}")) || size == 0 || size > max {
        return None;
    }
    let digest = a.get("digest").and_then(Json::str).and_then(|d| d.strip_prefix("sha256:")).and_then(parse_hex32);
    Some(Asset { name, size, url, digest })
}

/// Cut to at most `max` characters, on a character boundary.
pub fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", s[..i].trim_end()),
        None => s.to_string(),
    }
}

/// The newest published, non-pre-release release with a `pkg` file.
pub fn pick(list: &Json, pkg: Package, src: &Source) -> Result<Option<Offer>, String> {
    let Json::Arr(releases) = list else {
        return Err(match list.get("message").and_then(Json::str) {
            Some(m) => format!("GitHub answered: {}", cut(m, 200)),
            None => "GitHub's answer is not a list of releases".into(),
        });
    };
    let mut best: Option<Offer> = None;
    for r in releases {
        if r.get("draft").and_then(Json::bool) != Some(false) || r.get("prerelease").and_then(Json::bool) != Some(false)
        {
            continue;
        }
        let assets = r.get("assets").map(Json::arr).unwrap_or(&[]);
        let Some((version, asset)) = assets.iter().find_map(|a| {
            let asset = asset_of(a, src, MAX_ASSET)?;
            Some((pkg.version_of(&asset.name)?, asset))
        }) else {
            continue;
        };
        if best.as_ref().is_some_and(|b| !newer(&version, &b.version)) {
            continue;
        }
        let sums = assets.iter().filter_map(|a| asset_of(a, src, MAX_SUMS)).find(|a| a.name == "SHA256SUMS.txt");
        let sig = assets.iter().filter_map(|a| asset_of(a, src, MAX_SIG)).find(|a| a.name == "SHA256SUMS.txt.sig");
        let page = r
            .get("html_url")
            .and_then(Json::str)
            .filter(|u| clean_url(u, &src.pages))
            .unwrap_or(RELEASES_PAGE)
            .to_string();
        let notes = r
            .get("body")
            .and_then(Json::str)
            .map(|b| cut(b.replace("\r\n", "\n").trim(), MAX_NOTES))
            .unwrap_or_default();
        let tag = r.get("tag_name").and_then(Json::str).map(|t| cut(t, 64)).unwrap_or_default();
        let published = r.get("published_at").and_then(Json::str).and_then(parse_utc);
        best = Some(Offer { version, tag, page, notes, asset, sums, sig, published });
    }
    Ok(best)
}

/// Seconds since 1970 of a time as GitHub writes it (`2026-10-25T16:20:25Z`).
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        let t = &s[r];
        if t.bytes().all(|c| c.is_ascii_digit()) { t.parse().ok() } else { None }
    };
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if y < 1970 || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // days since 1970-01-01 (the proleptic Gregorian calendar)
    let (y, m) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

pub fn now_utc() -> Option<u64> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// How long automatic updates still leave `offer` alone ([`SETTLE`]); `None`
/// when it has been out long enough (or GitHub gives no date).
pub fn settling(offer: &Offer, now: Option<u64>) -> Option<Duration> {
    let wait = (offer.published? + SETTLE.as_secs()).saturating_sub(now?);
    (wait > 0).then(|| Duration::from_secs(wait))
}

/// The SHA-256 a `SHA256SUMS.txt` (sha256sum's format) gives for `name`.
pub fn sum_for(sums: &str, name: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|l| {
        let (h, rest) = l.split_once(char::is_whitespace)?;
        let n = rest.trim_start().strip_prefix('*').unwrap_or(rest.trim_start());
        (n.trim_end() == name).then(|| parse_hex32(h)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_parses_what_github_sends() {
        let j = Json::parse(r#" {"a": [1, -2.5e3, true, false, null], "s": "x\"\\\/\b\f\n\r\tA\u00e9\ud83c\udf4c\ud800!", "e": {}, "l": []} "#).unwrap();
        assert_eq!(j.get("a").unwrap().arr().len(), 5);
        assert_eq!(j.get("a").unwrap().arr()[0].u64(), Some(1));
        assert_eq!(j.get("a").unwrap().arr()[1], Json::Num(-2500.0));
        assert_eq!(j.get("a").unwrap().arr()[1].u64(), None);
        assert_eq!(j.get("s").unwrap().str(), Some("x\"\\/\u{8}\u{c}\n\r\tAé🍌\u{FFFD}!"));
        assert_eq!(j.get("e"), Some(&Json::Obj(vec![])));
        assert_eq!(j.get("l"), Some(&Json::Arr(vec![])));
        assert_eq!(Json::parse("\"日本語\"").unwrap().str(), Some("日本語"));
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "01",
            "1.",
            "-",
            "\"a",
            "\"\u{1}\"",
            "tru",
            "[1] 2",
            "{1:2}",
            "\"\\x\"",
            "\"\\u12\"",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?}");
        }
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert!(Json::parse(&deep).is_err(), "depth limit");
        let ok = "[".repeat(60) + &"]".repeat(60);
        assert!(Json::parse(&ok).is_ok());
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(hex(&sha256(&million)), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // fed in odd pieces: the same
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 3) as u8).collect();
        for step in [1, 3, 55, 56, 63, 64, 65, 127, 1000] {
            let mut s = Sha256::default();
            for c in data.chunks(step) {
                s.update(c);
            }
            assert_eq!(s.finish(), sha256(&data), "step {step}");
        }
        let h = sha256(b"abc");
        assert_eq!(parse_hex32(&hex(&h)), Some(h));
        assert_eq!(parse_hex32(&hex(&h).to_uppercase()), Some(h));
        assert_eq!(parse_hex32(&hex(&h)[1..]), None);
        assert_eq!(parse_hex32(&format!("{}g", &hex(&h)[1..])), None);
        assert_eq!(parse_hex32(&format!("+{}", &hex(&h)[1..])), None);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn sha512_matches_the_standard_vectors() {
        assert_eq!(
            hex(&sha512(&[b""])),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex(&sha512(&[b"a", b"bc"])),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        let long = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(
            hex(&sha512(&[long])),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
    }

    /// RFC 8032 7.1, tests 1 to 3, and one made with `openssl pkeyutl`.
    #[test]
    fn ed25519_signatures_are_verified() {
        let vectors = [
            (
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
                "",
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
            (
                "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
                "72",
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
            (
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "af82",
                "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            ),
            (
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "42616e616e6153706c69742072656c656173650a",
                "8d607a29ea4326a46154a5518336a1380271109f319a0bdf694af1f7b206fd21ebd634d1fd822e9451dc87b9a0042095eda14685df28c54782c264db81b77f08",
            ),
        ];
        for (k, m, s) in vectors {
            let key = parse_hex32(k).unwrap();
            let msg = unhex(m);
            let sig = parse_sig(s).unwrap();
            assert!(ed25519_verify(&key, &msg, &sig), "{k} {m}");
            // another message, a changed signature, another key: refused
            let mut other = msg.clone();
            other.push(0);
            assert!(!ed25519_verify(&key, &other, &sig));
            for at in [0, 31, 32, 63] {
                let mut bad = sig;
                bad[at] ^= 1;
                assert!(!ed25519_verify(&key, &msg, &bad), "byte {at}");
            }
            let mut wrong = key;
            wrong[0] ^= 1;
            assert!(!ed25519_verify(&wrong, &msg, &sig));
        }
        // S + L is the same point, but not a valid signature (malleability)
        let key = parse_hex32(vectors[1].0).unwrap();
        let mut sig = parse_sig(vectors[1].2).unwrap();
        let mut carry = 0u16;
        for i in 0..32 {
            let v = u16::from(sig[32 + i]) + u16::from(ORDER[i]) + carry;
            sig[32 + i] = v as u8;
            carry = v >> 8;
        }
        assert!(!ed25519_verify(&key, &[0x72], &sig), "S must be below L");
        // not a point: y = p
        let mut p = [0xffu8; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        assert!(!ed25519_verify(&p, b"", &parse_sig(vectors[0].2).unwrap()));
        assert!(
            parse_hex32(SIGNING_KEY).is_some_and(|k| Point::decode(&k, curve_d()).is_some()),
            "the release key is a point"
        );
    }

    #[test]
    fn only_signed_checksum_files_are_trusted() {
        let key = parse_hex32(vectors_key()).unwrap();
        let sums = "BananaSplit release\n";
        let sig = "8d607a29ea4326a46154a5518336a1380271109f319a0bdf694af1f7b206fd21ebd634d1fd822e9451dc87b9a0042095eda14685df28c54782c264db81b77f08\n";
        assert!(check_signature(sums, sig, &key).is_ok());
        assert!(check_signature("BananaSplit release!\n", sig, &key).unwrap_err().contains("not signed"));
        assert!(check_signature(sums, &sig[..100], &key).unwrap_err().contains("not a signature"));
        assert!(check_signature(sums, sig, &parse_hex32(SIGNING_KEY).unwrap()).is_err(), "another key");
    }

    fn vectors_key() -> &'static str {
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025"
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("2.8.0", "2.7.3"));
        assert!(newer("2.10.0", "2.9.9"));
        assert!(newer("3", "2.99.99"));
        assert!(newer("2.7.3.1", "2.7.3"));
        assert!(!newer("2.7.3", "2.7.3"));
        assert!(!newer("2.7", "2.7.0"));
        assert!(!newer("2.7.2", "2.7.10"));
        assert!(!newer("2.7.x", "1.0"));
        assert!(!newer("", "1.0"));
        assert!(!newer("1.0.0.0.0", "0.1"));
        assert_eq!(Package::Tarball.version_of("bananasplit-2.8.0-linux-x86_64.tar.gz").as_deref(), Some("2.8.0"));
        assert_eq!(Package::Deb.version_of("bananasplit_2.8.0_amd64.deb").as_deref(), Some("2.8.0"));
        assert_eq!(Package::WindowsExe.version_of("bananasplit-2.8.0-windows-x64.exe").as_deref(), Some("2.8.0"));
        assert_eq!(Package::WindowsExe.version_of("bananasplit-2.8.0-windows-x64.zip"), None);
        assert_eq!(Package::Tarball.version_of("bananasplit-2.7-beta-linux-x86_64.tar.gz"), None);
        assert_eq!(Package::Tarball.version_of("bananasplit_2.7.tar.gz"), None);
        assert_eq!(Package::MacZip.version_of("bananasplit-2.9.0-macos.zip").as_deref(), Some("2.9.0"));
        assert_eq!(Package::MacZip.version_of("bananasplit-2.9.0-windows-x64.zip"), None);
        for p in [Package::Tarball, Package::Deb, Package::WindowsExe, Package::MacZip] {
            assert_eq!(p.version_of(&p.file_name("10.0.1")).as_deref(), Some("10.0.1"));
        }
    }

    #[test]
    fn only_a_server_on_this_computer_stands_in_for_github() {
        assert_eq!(
            Source::local("http://127.0.0.1:8123").unwrap().download,
            "http://127.0.0.1:8123/LudaLunizz/BananaSplit/releases/download/"
        );
        assert!(Source::local("http://localhost:8123").is_some());
        for bad in [
            "http://127.0.0.1",
            "http://127.0.0.1:",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:80/x",
            "http://127.0.0.10:80",
            "https://evil.example:80",
            "http://127.0.0.1.evil:80",
            "http://127.0.0.1:80@evil",
        ] {
            assert!(Source::local(bad).is_none(), "{bad}");
        }
        let g = Source::github();
        assert!(g.https && g.api.starts_with("https://api.github.com/repos/LudaLunizz/BananaSplit/releases"));
    }

    fn release(tag: &str, draft: bool, pre: bool, files: &[(&str, u64, Option<&str>)]) -> String {
        let assets: Vec<String> = files
            .iter()
            .map(|(n, size, d)| {
                let digest = d.map_or("null".to_string(), |d| format!("\"sha256:{d}\""));
                format!(
                    r#"{{"name":"{n}","state":"uploaded","size":{size},"digest":{digest},"browser_download_url":"https://github.com/LudaLunizz/BananaSplit/releases/download/{tag}/{n}"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","name":"BananaSplit {tag}","draft":{draft},"prerelease":{pre},"html_url":"https://github.com/LudaLunizz/BananaSplit/releases/tag/{tag}","body":"Fixes.\r\nMore.","assets":[{}]}}"#,
            assets.join(",")
        )
    }

    #[test]
    fn the_newest_published_release_with_this_computers_file_is_picked() {
        let d = "a".repeat(64);
        let list = format!(
            "[{},{},{},{},{}]",
            release("v2.9.0", true, false, &[("bananasplit-2.9.0-windows-x64.exe", 10, Some(&d))]),
            release("v2.8.1", false, true, &[("bananasplit-2.8.1-windows-x64.exe", 10, Some(&d))]),
            release(
                "v2.7.10",
                false,
                false,
                &[
                    ("bananasplit-2.7.10-windows-x64.exe", 10, Some(&d)),
                    ("SHA256SUMS.txt", 300, None),
                    ("SHA256SUMS.txt.sig", 129, None)
                ]
            ),
            release("v2.7.9", false, false, &[("bananasplit-2.7.9-windows-x64.exe", 10, Some(&d))]),
            release("v2.8.0", false, false, &[("bananasplit-2.8.0-linux-x86_64.tar.gz", 10, None)]),
        );
        let j = Json::parse(&list).unwrap();
        let src = Source::github();
        let o = pick(&j, Package::WindowsExe, &src).unwrap().unwrap();
        assert_eq!(o.version, "2.7.10", "not the draft, not the pre-release, numbers compared as numbers");
        assert_eq!(o.tag, "v2.7.10");
        assert_eq!(o.asset.digest, parse_hex32(&d));
        assert_eq!(o.sums.as_ref().unwrap().name, "SHA256SUMS.txt");
        assert_eq!(o.sig.as_ref().unwrap().name, "SHA256SUMS.txt.sig");
        assert_eq!(o.notes, "Fixes.\nMore.");
        assert_eq!(o.page, "https://github.com/LudaLunizz/BananaSplit/releases/tag/v2.7.10");
        let t = pick(&j, Package::Tarball, &src).unwrap().unwrap();
        assert_eq!(
            (t.version.as_str(), t.asset.digest, t.sums.is_none(), t.sig.is_none()),
            ("2.8.0", None, true, true)
        );
        assert_eq!(pick(&j, Package::Deb, &src).unwrap(), None);
        assert!(
            pick(&Json::parse(r#"{"message":"API rate limit exceeded"}"#).unwrap(), Package::Deb, &src)
                .unwrap_err()
                .contains("rate limit")
        );
        assert_eq!(pick(&Json::parse("[]").unwrap(), Package::Deb, &src).unwrap(), None);
    }

    /// GitHub's real answer (the releases of 25 October 2026: 2.7.3, the
    /// 2.7.1 pre-release, 2.7.0).
    #[test]
    fn githubs_real_release_list_is_understood() {
        let j = Json::parse(include_str!("../tests/update/github-releases.json")).unwrap();
        let src = Source::github();
        let want = [
            (
                Package::Tarball,
                "bananasplit-2.7.3-linux-x86_64.tar.gz",
                "205c5e561e406b0b6a92c98f16ed5318aad0428f273d8d7ed68cb37a6e097f03",
            ),
            (
                Package::Deb,
                "bananasplit_2.7.3_amd64.deb",
                "b7032402e0195141f50d1bfa446dffab918c80630e01d03c31feb40961fc94ff",
            ),
            (
                Package::WindowsExe,
                "bananasplit-2.7.3-windows-x64.exe",
                "247e304c69470d6a65b4467a859263448b38a6cfdd59574f901830bb930dd9e0",
            ),
        ];
        for (pkg, name, digest) in want {
            let o = pick(&j, pkg, &src).unwrap().unwrap();
            assert_eq!((o.version.as_str(), o.tag.as_str(), o.asset.name.as_str()), ("2.7.3", "v2.7.3", name));
            assert_eq!(o.asset.digest, parse_hex32(digest));
            assert_eq!(
                o.asset.url,
                format!("https://github.com/LudaLunizz/BananaSplit/releases/download/v2.7.3/{name}")
            );
            assert_eq!(o.page, "https://github.com/LudaLunizz/BananaSplit/releases/tag/v2.7.3");
            assert_eq!(o.sums.as_ref().map(|s| s.name.as_str()), Some("SHA256SUMS.txt"));
            assert!(!o.notes.is_empty());
        }
    }

    #[test]
    fn links_outside_the_repositorys_releases_are_ignored() {
        let src = Source::github();
        let one = |url: &str, size: u64, page: &str| {
            let j = format!(
                r#"[{{"tag_name":"v9","draft":false,"prerelease":false,"html_url":"{page}","body":"","assets":[{{"name":"bananasplit_9.0.0_amd64.deb","size":{size},"browser_download_url":"{url}"}}]}}]"#
            );
            pick(&Json::parse(&j).unwrap(), Package::Deb, &src).unwrap()
        };
        let good = "https://github.com/LudaLunizz/BananaSplit/releases/download/v9/bananasplit_9.0.0_amd64.deb";
        let o = one(good, 5, "https://evil.example/x").unwrap();
        assert_eq!(o.page, RELEASES_PAGE, "a foreign page link is replaced by the releases list");
        assert!(
            one("https://evil.example/LudaLunizz/BananaSplit/releases/download/v9/bananasplit_9.0.0_amd64.deb", 5, "")
                .is_none()
        );
        assert!(
            one("http://github.com/LudaLunizz/BananaSplit/releases/download/v9/bananasplit_9.0.0_amd64.deb", 5, "")
                .is_none()
        );
        assert!(
            one("https://github.com/LudaLunizz/BananaSplit/releases/download/v9/other.deb", 5, "").is_none(),
            "the link names the file"
        );
        assert!(
            one(
                "https://github.com/LudaLunizz/BananaSplit/releases/download/v9/x y/bananasplit_9.0.0_amd64.deb",
                5,
                ""
            )
            .is_none()
        );
        assert!(one(good, 0, "").is_none(), "empty");
        assert!(one(good, MAX_ASSET + 1, "").is_none(), "too big");
        let j = format!(
            r#"[{{"tag_name":"v9","draft":false,"prerelease":false,"assets":[{{"name":"bananasplit_9.0.0_amd64.deb","state":"starter","size":5,"browser_download_url":"{good}"}}]}}]"#
        );
        assert_eq!(pick(&Json::parse(&j).unwrap(), Package::Deb, &src).unwrap(), None, "an upload that did not finish");
        let j = format!(
            r#"[{{"tag_name":"v9","assets":[{{"name":"bananasplit_9.0.0_amd64.deb","size":5,"browser_download_url":"{good}"}}]}}]"#
        );
        assert_eq!(
            pick(&Json::parse(&j).unwrap(), Package::Deb, &src).unwrap(),
            None,
            "draft/prerelease must say false"
        );
    }

    #[test]
    fn download_links_cannot_leave_the_releases() {
        let p = "https://github.com/LudaLunizz/BananaSplit/releases/download/";
        assert!(clean_url(&format!("{p}v9/bananasplit_9.0.0_amd64.deb"), p));
        assert!(clean_url(&format!("{p}v9/a.b..c.deb"), p), "dots inside a name are fine");
        for bad in [
            "v9/../../../other/x.deb",
            "v9/./x.deb",
            "v9/%2e%2e/x.deb",
            "v9/%2E%2E/x.deb",
            "..",
            "v9/x .deb",
            "v9/x\"y.deb",
        ] {
            assert!(!clean_url(&format!("{p}{bad}"), p), "{bad}");
        }
        assert!(!clean_url("https://github.com/LudaLunizz/BananaSplit/", p));
        // a character of several bytes where the prefix ends: refused, not a panic
        let multi = format!("{}é/x.deb", &p[..p.len() - 1]);
        assert!(!clean_url(&multi, p));
        assert!(!clean_url("é", p));
        let mut j = format!(
            r#"[{{"tag_name":"v9","draft":false,"prerelease":false,"html_url":"{multi}","body":"","assets":[]}}]"#
        );
        assert_eq!(pick(&Json::parse(&j).unwrap(), Package::Deb, &Source::github()).unwrap(), None);
        j = j.replace(
            r#""assets":[]"#,
            &format!(r#""assets":[{{"name":"x.deb","size":5,"browser_download_url":"{multi}"}}]"#),
        );
        assert_eq!(pick(&Json::parse(&j).unwrap(), Package::Deb, &Source::github()).unwrap(), None);
    }

    #[test]
    fn githubs_times_are_read() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-10-25T16:20:25Z"), Some(1_792_945_225));
        assert_eq!(parse_utc("2000-02-29T12:00:00Z"), Some(951_825_600), "a leap day");
        assert_eq!(parse_utc("2024-12-31T23:59:59Z"), Some(1_735_689_599));
        for bad in [
            "",
            "2026-10-25",
            "2026-10-25T16:20:25",
            "2026-10-25 16:20:25Z",
            "2026-13-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "2026-10-25T24:00:00Z",
            "2026-1a-25T16:20:25Z",
            "+026-10-25T16:20:25Z",
            "2026-10-25T16:20:25+01:00",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad}");
        }
    }

    #[test]
    fn automatic_updates_leave_a_new_release_alone_for_two_days() {
        let j = Json::parse(include_str!("../tests/update/github-releases.json")).unwrap();
        let mut o = pick(&j, Package::Tarball, &Source::github()).unwrap().unwrap();
        let at = o.published.expect("GitHub's list has published_at");
        assert_eq!(settling(&o, Some(at)), Some(SETTLE), "just published");
        assert_eq!(settling(&o, Some(at + 3600)), Some(SETTLE - Duration::from_secs(3600)));
        assert_eq!(settling(&o, Some(at + SETTLE.as_secs())), None, "out for two days");
        assert_eq!(settling(&o, Some(at + 10 * SETTLE.as_secs())), None);
        assert_eq!(settling(&o, None), None, "no clock: as before");
        o.published = None;
        assert_eq!(settling(&o, Some(at)), None, "no date from GitHub: as before");
    }

    #[test]
    fn notes_are_cut_on_a_character() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "abc…");
        assert_eq!(cut("ééééé", 2), "éé…");
        assert_eq!(cut("ab  cd", 3), "ab…");
    }

    #[test]
    fn checksum_files_are_read_like_sha256sum() {
        let a = "1".repeat(64);
        let b = "2".repeat(64);
        let text = format!("{a}  bananasplit-2.8.0-windows-x64.exe\n{b} *bananasplit_2.8.0_amd64.deb\r\n");
        assert_eq!(sum_for(&text, "bananasplit-2.8.0-windows-x64.exe"), parse_hex32(&a));
        assert_eq!(sum_for(&text, "bananasplit_2.8.0_amd64.deb"), parse_hex32(&b));
        assert_eq!(sum_for(&text, "bananasplit-2.8.0-windows-x64.zip"), None);
        assert_eq!(sum_for(&text, "windows-x64.exe"), None);
    }
}
