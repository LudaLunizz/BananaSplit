//! The updater: new versions of BananaSplit from its GitHub releases.
//!
//! * **Off by default.** With "Install updates automatically" off (the
//!   default, `[gui] auto_update = false`), BananaSplit never contacts
//!   anything on its own: it looks for a new version only when the user
//!   clicks "Check now" (Menu ▸ Updates) or runs `bananasplit --update`.
//! * **On:** the GUI checks a minute after it starts and then once a day,
//!   downloads and installs a new version by itself, and says that a
//!   restart finishes the update. It never restarts on its own: that would
//!   cut the audio. Where installing needs an administrator (the .deb), it
//!   downloads and checks the package, then waits for a click (and the
//!   password).
//!
//! What counts as a release: a GitHub release of `LudaLunizz/BananaSplit`
//! that is published (not a draft) and not marked pre-release, with this
//! platform's file. The file's version is in its name
//! (`bananasplit-2.7.4-linux-x86_64.tar.gz`, `bananasplit_2.7.4_amd64.deb`,
//! `bananasplit-2.7.4-windows-x64.exe`), the same names the release
//! workflow gives them.
//!
//! What is checked before anything is replaced:
//! * the file comes from this repository's release downloads, over HTTPS
//!   (curl with `--proto =https`), and has the size GitHub lists;
//! * its SHA-256 matches GitHub's digest of the upload and the release's
//!   `SHA256SUMS.txt` (each one that exists; a release with neither is
//!   refused);
//! * the new program runs here and says it is that version
//!   (`--version`), before it takes the old one's place.
//!
//! How it is installed:
//! * Linux, a program in a folder of the user's (the portable package's
//!   `~/.local/bin`, an unpacked folder): the new binary is written next
//!   to it and renamed over it, which is atomic; the running mixer keeps
//!   the old one until it ends. BananaSplit's own app-menu copy is updated
//!   too.
//! * Linux, the .deb: `pkexec apt-get install` of the downloaded package
//!   (the desktop asks for the password).
//! * Windows: the running `bananasplit.exe` is renamed to
//!   `bananasplit.exe.old` (Windows allows that, not overwriting it), the
//!   new one takes its name, and the next start removes the old one.
//! * Anything else (another package manager, an AppImage, a folder it
//!   cannot write to): it says so and links the release page.
//!
//! Downloads use the system's `curl` (on Windows the one in System32, part
//! of Windows 10 and 11) or `wget`, so BananaSplit itself still has no
//! dependencies and no TLS code of its own.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO: &str = "LudaLunizz/BananaSplit";
pub const RELEASES_PAGE: &str = "https://github.com/LudaLunizz/BananaSplit/releases";

/// Tests only: `http://127.0.0.1:PORT`, a stand-in for GitHub (see
/// [`Source::from_env`]).
pub const SOURCE_ENV: &str = "BANANASPLIT_UPDATE_SOURCE";
/// Tests only, with [`SOURCE_ENV`]: seconds before the first automatic check.
pub const DELAY_ENV: &str = "BANANASPLIT_UPDATE_DELAY";
/// Set for a restarted BananaSplit: the process id of the one it replaces,
/// which it waits for (that one still holds the GUI port and the audio).
pub const WAIT_ENV: &str = "BANANASPLIT_WAIT_PID";

/// The first automatic check, after the start: the desktop's network and
/// the audio have settled by then.
const FIRST_CHECK: Duration = Duration::from_secs(60);
const RECHECK: Duration = Duration::from_secs(24 * 3600);
const RETRY: Duration = Duration::from_secs(3600);
const MAX_LIST: u64 = 4 << 20;
const MAX_SUMS: u64 = 64 << 10;
const MAX_ASSET: u64 = 512 << 20;
const MAX_NOTES: usize = 4000;

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
}

impl Source {
    pub fn github() -> Source {
        Source {
            api: format!("https://api.github.com/repos/{REPO}/releases?per_page=30"),
            download: format!("https://github.com/{REPO}/releases/download/"),
            pages: format!("https://github.com/{REPO}/releases/"),
            https: true,
        }
    }

    /// GitHub, unless a test points [`SOURCE_ENV`] at a server on this
    /// computer (`http://127.0.0.1:PORT` or `http://localhost:PORT`, nothing
    /// else is accepted).
    pub fn from_env() -> Source {
        std::env::var(SOURCE_ENV).ok().and_then(|b| Source::local(&b)).unwrap_or_else(Source::github)
    }

    pub fn local(base: &str) -> Option<Source> {
        let port = base.strip_prefix("http://127.0.0.1:").or_else(|| base.strip_prefix("http://localhost:"))?;
        if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u16>().ok()? == 0 {
            return None;
        }
        Some(Source {
            api: format!("{base}/repos/{REPO}/releases?per_page=30"),
            download: format!("{base}/{REPO}/releases/download/"),
            pages: format!("{base}/{REPO}/releases/"),
            https: false,
        })
    }

    fn is_test(&self) -> bool {
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
        let h = self.b.get(self.i..self.i + 4).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u32::from_str_radix(h, 16).ok());
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
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
    0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
    0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
    0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
    0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
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
        for (t, c) in b.chunks_exact(4).enumerate() {
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
        let mut chunks = data.chunks_exact(64);
        for c in &mut chunks {
            Sha256::block(&mut self.h, c);
        }
        let rest = chunks.remainder();
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
        for (o, h) in out.chunks_exact_mut(4).zip(self.h) {
            o.copy_from_slice(&h.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
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

#[cfg(test)]
pub fn hex(b: &[u8]) -> String {
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
// Versions and release files
// ---------------------------------------------------------------------------

/// `2.7.3`: one to four whole numbers.
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    parts.iter().map(|p| if !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None }).collect()
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
}

impl Package {
    fn affixes(self) -> (&'static str, &'static str) {
        match self {
            Package::Tarball => ("bananasplit-", "-linux-x86_64.tar.gz"),
            Package::Deb => ("bananasplit_", "_amd64.deb"),
            Package::WindowsExe => ("bananasplit-", "-windows-x64.exe"),
        }
    }

    #[cfg(test)]
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
}

fn clean_url(u: &str, prefix: &str) -> bool {
    let rest = &u[prefix.len().min(u.len())..];
    u.starts_with(prefix)
        && u.len() < 2048
        && !u.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\')
        // no way out of the prefix: no dot segments, also not percent-encoded
        && !rest.split('/').any(|seg| seg == "." || seg == "..")
        && !rest.to_ascii_lowercase().contains("%2e")
}

fn asset_of(a: &Json, src: &Source, max: u64) -> Option<Asset> {
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
fn cut(s: &str, max: usize) -> String {
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
        if r.get("draft").and_then(Json::bool) != Some(false) || r.get("prerelease").and_then(Json::bool) != Some(false) {
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
        let page = r.get("html_url").and_then(Json::str).filter(|u| clean_url(u, &src.pages)).unwrap_or(RELEASES_PAGE).to_string();
        let notes = r.get("body").and_then(Json::str).map(|b| cut(b.replace("\r\n", "\n").trim(), MAX_NOTES)).unwrap_or_default();
        let tag = r.get("tag_name").and_then(Json::str).map(|t| cut(t, 64)).unwrap_or_default();
        best = Some(Offer { version, tag, page, notes, asset, sums });
    }
    Ok(best)
}

/// The SHA-256 a `SHA256SUMS.txt` (sha256sum's format) gives for `name`.
pub fn sum_for(sums: &str, name: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|l| {
        let (h, rest) = l.split_once(char::is_whitespace)?;
        let n = rest.trim_start().strip_prefix('*').unwrap_or(rest.trim_start());
        (n.trim_end() == name).then(|| parse_hex32(h)).flatten()
    })
}

// ---------------------------------------------------------------------------
// Downloads (curl or wget)
// ---------------------------------------------------------------------------

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command that never opens a console window (Windows) and gets none of
/// libjack's descriptors (Linux).
fn command(program: &Path) -> Command {
    crate::sys::cloexec_all();
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0); // not stopped by a terminal's Ctrl-C
    }
    c
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    Curl(PathBuf),
    #[cfg_attr(windows, allow(dead_code))]
    Wget(PathBuf),
}

/// curl, or else wget. On Windows: the curl in System32 (part of Windows
/// 10 1803 and later), not one a PATH entry may put first.
pub fn find_tool() -> Option<Tool> {
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("SystemRoot").map(PathBuf::from) {
            let c = root.join("System32").join("curl.exe");
            if c.is_file() {
                return Some(Tool::Curl(c));
            }
        }
        which("curl.exe").map(Tool::Curl)
    }
    #[cfg(unix)]
    {
        which("curl").map(Tool::Curl).or_else(|| which("wget").map(Tool::Wget))
    }
}

/// `curl -V`'s version (major, minor), asked once.
#[cfg(windows)]
fn curl_version(curl: &Path) -> Option<(u32, u32)> {
    static V: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let mut c = command(curl);
        c.arg("-V");
        let never = AtomicBool::new(false);
        let (_, out, _) = run(c, Duration::from_secs(10), &never, &mut || Ok(())).ok()?;
        parse_curl_version(&out)
    })
}

/// "curl 8.4.0 (Windows) libcurl/8.4.0 …" -> (8, 4)
#[cfg_attr(unix, allow(dead_code))]
fn parse_curl_version(out: &str) -> Option<(u32, u32)> {
    let v = first_line(out).strip_prefix("curl ")?.split_whitespace().next()?;
    let mut it = v.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

const NO_TOOL: &str = if cfg!(windows) {
    "Windows' curl.exe was not found (it is part of Windows 10 and 11)"
} else {
    "curl or wget is needed to download updates; install curl"
};

/// Run a child process to its end: `stop` ends it early, `limit` too;
/// `tick` runs every 100 ms (progress). Returns (exit code, stdout, stderr).
fn run(mut cmd: Command, limit: Duration, stop: &AtomicBool, tick: &mut dyn FnMut() -> Result<(), String>) -> Result<(Option<i32>, String, String), String> {
    let mut child: Child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.by_ref().take(1 << 20).read_to_end(&mut s);
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = pipe(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let t0 = Instant::now();
    let why = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (o, e) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
                return Ok((status.code(), o, e));
            }
            Ok(None) => {}
            Err(e) => break format!("waiting for the download: {e}"),
        }
        if stop.load(SeqCst) {
            break "stopped".to_string();
        }
        if t0.elapsed() > limit {
            break "timed out".to_string();
        }
        if let Err(e) = tick() {
            break e;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = (out.join(), err.join());
    Err(why)
}

fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

fn curl_error(code: i32, stderr: &str) -> String {
    let what = match code {
        5 | 6 => "could not find the server (is this computer online?)",
        7 => "could not connect to the server",
        28 => "the connection timed out",
        35 | 51 | 53 | 54 | 58 | 59 | 60 | 66 | 77 | 80 | 83 | 90 | 91 => "the secure (TLS) connection failed",
        47 => "too many redirects",
        63 => "the file is larger than expected",
        _ => "",
    };
    let detail = first_line(stderr).trim_start_matches("curl: ");
    match (what.is_empty(), detail.is_empty()) {
        (false, _) => what.to_string(),
        (true, false) => format!("curl failed (exit {code}): {}", cut(detail, 200)),
        (true, true) => format!("curl failed (exit {code})"),
    }
}

/// Download `url` to `dest` (through `dest.part`, renamed when complete).
/// `max`: the most it may be. `json`: GitHub's API (its headers).
pub fn fetch(src: &Source, url: &str, dest: &Path, max: u64, json: bool, stop: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let tool = find_tool().ok_or(NO_TOOL)?;
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(dest);
    let limit = if json { Duration::from_secs(60) } else { Duration::from_secs(30 * 60) };
    let agent = format!("BananaSplit/{VERSION} (updater; https://github.com/{REPO})");
    let proto = if src.https { "=https" } else { "=http" };
    let cmd = match &tool {
        Tool::Curl(p) => {
            let mut c = command(p);
            // -q first: the user's ~/.curlrc cannot change what this does
            c.args(["-q", "-sS", "-L", "--proto", proto, "--proto-redir", proto, "--max-redirs", "8", "--connect-timeout", "20"]);
            c.args(["--max-time", &limit.as_secs().to_string(), "--max-filesize", &max.to_string(), "--retry", "2"]);
            c.arg("-H").arg(format!("User-Agent: {agent}"));
            if json {
                c.args(["-H", "Accept: application/vnd.github+json", "-H", "X-GitHub-Api-Version: 2022-11-28"]);
            }
            // Schannel: a network that blocks revocation lists (curl 7.70 and later)
            #[cfg(windows)]
            if curl_version(p).is_some_and(|v| v >= (7, 70)) {
                c.arg("--ssl-revoke-best-effort");
            }
            c.args(["-w", "%{http_code}", "-o"]).arg(&part).arg(url);
            c
        }
        Tool::Wget(p) => {
            let mut c = command(p);
            c.args(["--no-config", "-q", "--max-redirect=8", "--timeout=20", "--tries=3"]);
            c.arg(format!("--user-agent={agent}"));
            if json {
                c.args(["--header=Accept: application/vnd.github+json", "--header=X-GitHub-Api-Version: 2022-11-28"]);
            }
            c.arg("-O").arg(&part).arg(url);
            c
        }
    };
    let mut tick = || {
        let n = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        progress(n);
        if n > max { Err("the file is larger than expected".to_string()) } else { Ok(()) }
    };
    let result = run(cmd, limit, stop, &mut tick);
    let fail = |e: String| {
        let _ = std::fs::remove_file(&part);
        Err(e)
    };
    let (code, out, err) = match result {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    match (&tool, code) {
        (Tool::Curl(_), Some(0)) => {
            let http = out.trim();
            if http != "200" {
                let why = match http {
                    "403" | "429" if json => "GitHub refused the request (too many in a short time); try again in an hour".to_string(),
                    "404" => "not found (404)".to_string(),
                    "000" | "" => "no answer from the server".to_string(),
                    h => format!("the server answered HTTP {h}"),
                };
                return fail(why);
            }
        }
        (Tool::Curl(_), Some(c)) => return fail(curl_error(c, &err)),
        (Tool::Wget(_), Some(0)) => {}
        (Tool::Wget(_), Some(8)) => return fail("the server answered with an error".into()),
        (Tool::Wget(_), Some(c)) => {
            let d = first_line(&err);
            return fail(if d.is_empty() { format!("wget failed (exit {c})") } else { format!("wget failed (exit {c}): {}", cut(d, 200)) });
        }
        (_, None) => return fail("the download was stopped".into()),
    }
    let n = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if n > max {
        return fail("the file is larger than expected".into());
    }
    progress(n);
    std::fs::rename(&part, dest).or_else(|e| fail(format!("{}: {e}", dest.display())))
}

// ---------------------------------------------------------------------------
// This installation
// ---------------------------------------------------------------------------

/// Where downloads are kept until they are installed.
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache")));
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|p| p.is_absolute());
    base.map(|b| b.join("bananasplit").join("update"))
}

fn make_cache() -> Result<PathBuf, String> {
    let d = cache_dir().ok_or("no folder for downloads (no home folder)")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    #[cfg(windows)]
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// How this copy of BananaSplit can be updated.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// Write the new program over these files (the running one first).
    Replace(Vec<PathBuf>),
    /// `pkexec apt-get install` of the new .deb.
    Deb,
    /// Not by itself: why (the user downloads it).
    Manual(String),
}

impl Method {
    pub fn package(&self) -> Package {
        if cfg!(windows) {
            return Package::WindowsExe;
        }
        let deb = match self {
            Method::Deb => true,
            Method::Replace(_) => false,
            // e.g. the .deb without apt-get: its version is the .deb's
            Method::Manual(_) => self_path().is_some_and(|p| deb_owned(&p)),
        };
        if deb { Package::Deb } else { Package::Tarball }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Method::Replace(_) => "replace",
            Method::Deb => "package",
            Method::Manual(_) => "manual",
        }
    }
}

const DEB_PROGRAM: &str = "/usr/bin/bananasplit";
#[cfg(unix)]
const APT_GET: &str = "/usr/bin/apt-get";

/// `/usr/bin/bananasplit`, installed by the bananasplit .deb.
fn deb_owned(exe: &Path) -> bool {
    exe == Path::new(DEB_PROGRAM)
        && ["/var/lib/dpkg/info/bananasplit.list", "/var/lib/dpkg/info/bananasplit:amd64.list"]
            .iter()
            .any(|l| std::fs::read_to_string(l).is_ok_and(|t| t.lines().any(|x| x == DEB_PROGRAM)))
}

/// The program file that runs (on Linux after an update replaced it: the
/// file now at its place).
fn self_path() -> Option<PathBuf> {
    crate::desktop::program_path()
}

/// Can this process create files in `dir`?
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".bananasplit-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

pub fn method() -> Method {
    if !cfg!(target_arch = "x86_64") {
        return Method::Manual("the release page has BananaSplit for x86-64 (64-bit Intel and AMD) only".into());
    }
    #[cfg(unix)]
    if std::env::var_os("APPIMAGE").is_some_and(|a| !a.is_empty()) {
        return Method::Manual("this BananaSplit runs from an AppImage: download the new one from the release page".into());
    }
    let Some(exe) = self_path() else { return Method::Manual("cannot find this program's file".into()) };
    #[cfg(unix)]
    {
        if deb_owned(&exe) {
            if Path::new(APT_GET).is_file() {
                return Method::Deb;
            }
            return Method::Manual("BananaSplit was installed from its .deb, and apt-get is not here: install the new .deb from the release page".into());
        }
        if crate::desktop::is_system_path(&exe) {
            return Method::Manual(format!("{} was installed by a package manager: update BananaSplit there", exe.display()));
        }
    }
    #[cfg_attr(windows, allow(unused_mut))]
    let mut targets = vec![exe];
    // The copy BananaSplit's own app-menu entry starts (`--install`), when
    // this one is another (run from the unpacked package).
    #[cfg(unix)]
    if let Some(menu) = crate::desktop::installed_program() {
        if !same_file(&menu, &targets[0]) {
            targets.push(menu);
        }
    }
    for t in &targets {
        let dir = t.parent().unwrap_or(Path::new("."));
        if !writable(dir) {
            return Method::Manual(format!("BananaSplit cannot write to {}: download the new version from the release page", dir.display()));
        }
    }
    Method::Replace(targets)
}

// ---------------------------------------------------------------------------
// Download, check, install
// ---------------------------------------------------------------------------

/// The newest release for this computer, if any.
pub fn find(src: &Source, pkg: Package, stop: &AtomicBool) -> Result<Option<Offer>, String> {
    let dir = make_cache()?;
    let file = dir.join("releases.json");
    fetch(src, &src.api, &file, MAX_LIST, true, stop, &mut |_| {})?;
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()));
    let _ = std::fs::remove_file(&file);
    let json = Json::parse(&text?).map_err(|e| format!("GitHub's answer could not be read ({e})"))?;
    pick(&json, pkg, src)
}

/// Check a downloaded file against everything the release says about it.
pub fn verify(file: &Path, offer: &Offer, sums: Option<&str>) -> Result<[u8; 32], String> {
    let name = &offer.asset.name;
    let len = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if len != offer.asset.size {
        return Err(format!("{name} has {len} bytes, the release says {}", offer.asset.size));
    }
    let listed = match sums {
        Some(text) => Some(sum_for(text, name).ok_or_else(|| format!("the release's SHA256SUMS.txt does not list {name}"))?),
        None => None,
    };
    if offer.asset.digest.is_none() && listed.is_none() {
        return Err(format!("the release has no checksum for {name}, so it is not installed"));
    }
    let got = sha256_file(file)?;
    for (want, from) in [(offer.asset.digest, "GitHub's checksum"), (listed, "SHA256SUMS.txt")] {
        if want.is_some_and(|w| w != got) {
            return Err(format!("{name} does not match {from} (damaged or not the released file); nothing was changed"));
        }
    }
    Ok(got)
}

/// Download the release file and check it. Returns its path and SHA-256.
pub fn download(src: &Source, offer: &Offer, stop: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(PathBuf, [u8; 32]), String> {
    let dir = make_cache()?;
    clean_cache(&dir);
    let sums = match &offer.sums {
        Some(s) => {
            let f = dir.join("SHA256SUMS.txt");
            fetch(src, &s.url, &f, MAX_SUMS, false, stop, &mut |_| {}).map_err(|e| format!("downloading SHA256SUMS.txt: {e}"))?;
            let t = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()));
            let _ = std::fs::remove_file(&f);
            Some(t?)
        }
        None => None,
    };
    let file = dir.join(&offer.asset.name);
    fetch(src, &offer.asset.url, &file, offer.asset.size, false, stop, progress).map_err(|e| format!("downloading {}: {e}", offer.asset.name))?;
    match verify(&file, offer, sums.as_deref()) {
        Ok(hash) => Ok((file, hash)),
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            Err(e)
        }
    }
}

/// Remove what earlier downloads left in the downloads folder.
fn clean_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with("bananasplit") || name == "SHA256SUMS.txt" || name.starts_with("releases.json") || name == "unpacked";
        if !ours {
            continue;
        }
        if p.is_dir() && !p.is_symlink() {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Does `program --version` say it is BananaSplit `version`?
pub fn check_program(program: &Path, version: &str) -> Result<(), String> {
    let mut c = command(program);
    c.arg("--version");
    let never = AtomicBool::new(false);
    let (code, out, err) = run(c, Duration::from_secs(20), &never, &mut || Ok(())).map_err(|e| format!("the new version did not run: {e}"))?;
    let line = first_line(&out);
    let said = line.strip_prefix("BananaSplit ").and_then(|r| r.split_whitespace().next());
    match said {
        Some(v) if code == Some(0) && v == version => Ok(()),
        Some(v) if code == Some(0) => Err(format!("the downloaded program says it is BananaSplit {}, not {version}", cut(v, 40))),
        _ => {
            let what = if line.is_empty() { first_line(&err) } else { line };
            Err(format!("the new version does not run on this computer ({})", if what.is_empty() { "no answer".to_string() } else { cut(what, 200) }))
        }
    }
}

/// Linux: the program from the portable package (`NAME/bananasplit`).
#[cfg(unix)]
fn unpack(tgz: &Path, version: &str) -> Result<PathBuf, String> {
    let tar = which("tar").ok_or("tar is needed to unpack the update")?;
    let member = format!("bananasplit-{version}-linux-x86_64/bananasplit");
    let dir = tgz.parent().unwrap_or(Path::new(".")).join("unpacked");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let never = AtomicBool::new(false);
    let mut c = command(&tar);
    c.arg("-xzf").arg(tgz).arg("-C").arg(&dir).arg("--no-same-owner").arg(&member);
    match run(c, Duration::from_secs(120), &never, &mut || Ok(())) {
        Ok((Some(0), _, _)) => {}
        Ok((_, _, err)) => return Err(format!("could not unpack the update: {}", cut(first_line(&err), 200))),
        Err(e) => return Err(format!("could not unpack the update: {e}")),
    }
    let bin = dir.join(&member);
    let meta = std::fs::symlink_metadata(&bin).map_err(|_| format!("the package has no {member}"))?;
    if !meta.file_type().is_file() || meta.len() == 0 {
        return Err(format!("{member} in the package is not a program file"));
    }
    Ok(bin)
}

/// Windows: where the running program was moved by an update (helper
/// processes are started from it: they must be the same version).
#[cfg(windows)]
static MOVED: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Windows: the program's own path, where the new version now is (Windows
/// may name the running program by the file it was moved to).
#[cfg(windows)]
static ORIGINAL: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(windows)]
pub fn moved_program() -> Option<PathBuf> {
    lock(&MOVED).clone()
}

#[cfg(windows)]
pub fn original_program() -> Option<PathBuf> {
    lock(&ORIGINAL).clone()
}

fn retry<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    // A virus scanner may hold a new file for a moment.
    let mut tries = 0;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if tries < 15 && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The new program file next to `target`, before it takes its place.
fn staging_name(target: &Path) -> PathBuf {
    let dir = target.parent().unwrap_or(Path::new("."));
    if cfg!(windows) {
        dir.join(format!("bananasplit.update-{}.exe", std::process::id()))
    } else {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bananasplit".into());
        dir.join(format!(".{name}.update-{}", std::process::id()))
    }
}

/// Set while program files are being renamed: quitting waits for it, so
/// the program is never left without its file (Windows renames twice).
static RENAMING: AtomicBool = AtomicBool::new(false);

struct Renaming;
impl Renaming {
    fn start() -> Renaming {
        RENAMING.store(true, SeqCst);
        Renaming
    }
}
impl Drop for Renaming {
    fn drop(&mut self) {
        RENAMING.store(false, SeqCst);
    }
}

/// Put `new` (a checked BananaSplit `version`) in the place of each target.
/// The first target is the running program; nothing is changed unless the
/// new program runs here. A later target (the app menu's copy) that cannot
/// be replaced is a warning: the running program is new by then.
pub fn replace(new: &Path, targets: &[PathBuf], version: &str) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for (k, target) in targets.iter().enumerate() {
        let result = replace_one(new, target, version, k == 0);
        match result {
            Ok(()) => {}
            Err(e) if k == 0 => return Err(e),
            Err(e) => warnings.push(format!("{} was not updated ({e})", target.display())),
        }
    }
    Ok(warnings)
}

fn replace_one(new: &Path, target: &Path, version: &str, first: bool) -> Result<(), String> {
    {
        let k = usize::from(!first);
        let stage = staging_name(target);
        let _ = std::fs::remove_file(&stage);
        let staged = (|| -> Result<(), String> {
            std::fs::copy(new, &stage).map_err(|e| format!("{}: {e}", stage.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", stage.display()))?;
            }
            // to disk before it takes the old one's place (Windows flushes
            // only a handle opened for writing)
            std::fs::OpenOptions::new()
                .write(true)
                .open(&stage)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("{}: {e}", stage.display()))?;
            if k == 0 {
                check_program(&stage, version)?;
            }
            Ok(())
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_file(&stage);
            return Err(e);
        }
        let _renaming = Renaming::start();
        #[cfg(unix)]
        if let Err(e) = retry(|| std::fs::rename(&stage, target)) {
            let _ = std::fs::remove_file(&stage);
            return Err(format!("{}: {e}", target.display()));
        }
        #[cfg(windows)]
        {
            // A running program cannot be overwritten, but it can be renamed.
            let mut old = PathBuf::from(format!("{}.old", target.display()));
            if old.exists() && std::fs::remove_file(&old).is_err() {
                old = PathBuf::from(format!("{}.old-{}", target.display(), std::process::id()));
            }
            if let Err(e) = retry(|| std::fs::rename(target, &old)) {
                let _ = std::fs::remove_file(&stage);
                return Err(format!("{}: {e}", target.display()));
            }
            if let Err(e) = retry(|| std::fs::rename(&stage, target)) {
                let _ = std::fs::rename(&old, target);
                let _ = std::fs::remove_file(&stage);
                return Err(format!("{}: {e}", target.display()));
            }
            if k == 0 {
                // The first update of this session moved the program that
                // runs; a second one moves the first one's file, not this.
                lock(&MOVED).get_or_insert(old);
                lock(&ORIGINAL).get_or_insert(target.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Is `f`, in the program's folder, something an update left there? The
/// old Windows program (`bananasplit.exe.old`, in use until the old
/// BananaSplit ended) or an unfinished new file of an update that is not
/// running any more (not this process's, not a live one's).
fn stale(windows: bool, exe: &str, f: &str, me: u32, alive: &dyn Fn(&str) -> bool) -> bool {
    let pid = |p: &str| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()) && p != me.to_string() && !alive(p);
    if windows {
        f == format!("{exe}.old")
            || f.strip_prefix(&format!("{exe}.old-")).is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            || f.strip_prefix("bananasplit.update-").and_then(|r| r.strip_suffix(".exe")).is_some_and(pid)
    } else {
        f.strip_prefix(&format!(".{exe}.update-")).is_some_and(pid)
    }
}

/// Remove what an update left next to the program (see [`stale`]).
pub fn clean_leftovers() {
    let Some(exe) = self_path() else { return };
    let Some(dir) = exe.parent() else { return };
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if stale(cfg!(windows), &name, &f, std::process::id(), &pid_alive) && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Linux: install the new .deb. From the app, the desktop asks for the
/// password; from a terminal (`interactive`), pkexec asks in it.
#[cfg(unix)]
fn install_deb(deb: &Path, version: &str, stop: &AtomicBool, interactive: bool) -> Result<(), String> {
    let Some(pkexec) = which("pkexec") else {
        return Err(format!("installing a package needs pkexec, which is not here. Install it in a terminal:  sudo apt install {}", deb.display()));
    };
    install_deb_with(&pkexec, Path::new(APT_GET), Path::new(DEB_PROGRAM), deb, version, stop, interactive)
}

#[cfg(unix)]
fn install_deb_with(pkexec: &Path, apt: &Path, program: &Path, deb: &Path, version: &str, stop: &AtomicBool, interactive: bool) -> Result<(), String> {
    let result = if interactive {
        // In the terminal's foreground, with its input and output: pkexec's
        // text agent reads the password there.
        crate::sys::cloexec_all();
        std::process::Command::new(pkexec)
            .arg(apt)
            .arg("install")
            .arg("-y")
            .arg(deb)
            .status()
            .map(|s| (s.code(), String::new(), String::new()))
            .map_err(|e| format!("cannot run {}: {e}", pkexec.display()))
    } else {
        let mut c = command(pkexec);
        c.arg(apt).arg("install").arg("-y").arg(deb);
        // No time limit: the password dialog waits for the user.
        run(c, Duration::from_secs(24 * 3600), stop, &mut || Ok(()))
    };
    match result {
        Ok((Some(0), _, _)) => {}
        Ok((Some(126), _, _)) => return Err("installing was cancelled (no password given)".into()),
        Ok((Some(127), _, err)) => {
            let why = first_line(&err);
            return Err(format!("could not get permission to install the package{}", if why.is_empty() { String::new() } else { format!(" ({})", cut(why, 200)) }));
        }
        Ok((code, _, err)) => {
            let last = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
            return Err(format!("apt-get failed (exit {}): {}", code.map_or("?".into(), |c| c.to_string()), cut(last, 200)));
        }
        Err(e) => return Err(e),
    }
    check_program(program, version)
}

/// Install a downloaded, checked release file. Returns where it went, and
/// warnings (a copy that could not be updated).
pub fn install(method: &Method, offer: &Offer, file: &Path, stop: &AtomicBool, interactive: bool) -> Result<(String, Vec<String>), String> {
    let _ = (stop, interactive);
    match method {
        Method::Manual(why) => Err(why.clone()),
        Method::Deb => {
            #[cfg(unix)]
            {
                install_deb(file, &offer.version, stop, interactive)?;
                Ok((DEB_PROGRAM.to_string(), Vec::new()))
            }
            #[cfg(windows)]
            Err("no packages on Windows".into())
        }
        Method::Replace(targets) => {
            #[cfg(unix)]
            let program = unpack(file, &offer.version)?;
            #[cfg(windows)]
            let program = file.to_path_buf();
            let r = replace(&program, targets, &offer.version);
            #[cfg(unix)]
            if let Some(d) = program.parent().and_then(Path::parent) {
                let _ = std::fs::remove_dir_all(d); // the unpacked folder
            }
            let warnings = r?;
            Ok((targets.iter().map(|t| t.display().to_string()).collect::<Vec<_>>().join(", "), warnings))
        }
    }
}

// ---------------------------------------------------------------------------
// The updater of the GUI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Not checked yet.
    Idle,
    Checking,
    /// This is the newest version.
    Current,
    /// A newer version: `offer`.
    Available,
    Downloading,
    /// The .deb is downloaded and checked; installing it needs a click.
    Prepared,
    Installing,
    /// Installed: restart BananaSplit to use it.
    Ready,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::Current => "current",
            Phase::Available => "available",
            Phase::Downloading => "downloading",
            Phase::Prepared => "prepared",
            Phase::Installing => "installing",
            Phase::Ready => "ready",
            Phase::Failed => "failed",
        }
    }

    fn busy(self) -> bool {
        matches!(self, Phase::Checking | Phase::Downloading | Phase::Installing)
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub offer: Option<Offer>,
    pub message: String,
    pub got: u64,
    pub total: u64,
    pub checked: Option<Instant>,
    /// The version this session installed (it runs after a restart).
    pub installed: Option<String>,
    pub method: Method,
    /// The .deb, downloaded and checked, and its SHA-256 then.
    pub prepared: Option<PathBuf>,
    pub prepared_hash: Option<[u8; 32]>,
    /// Bumped on every change but progress (the page fetches the notes).
    pub rev: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Job {
    Check { auto: bool },
    Install { auto: bool },
}

struct Inner {
    st: Status,
    job: Option<Job>,
    next_auto: Option<Instant>,
    /// Something the session line should say (an automatic update).
    event: Option<String>,
    /// Automatic installs of this version that failed: after two, it is
    /// left to the user (no new download every hour).
    auto_fails: (String, u32),
    done: bool,
}

pub struct Updater {
    inner: Mutex<Inner>,
    cv: Condvar,
    stop: AtomicBool,
    src: Source,
    started: Instant,
    first: Duration,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Updater {
    pub fn start() -> Arc<Updater> {
        let src = Source::from_env();
        let first = if src.is_test() {
            std::env::var(DELAY_ENV).ok().and_then(|s| s.parse().ok()).map_or(FIRST_CHECK, Duration::from_secs_f64)
        } else {
            FIRST_CHECK
        };
        let u = Arc::new(Updater {
            inner: Mutex::new(Inner {
                st: Status {
                    phase: Phase::Idle,
                    offer: None,
                    message: String::new(),
                    got: 0,
                    total: 0,
                    checked: None,
                    installed: None,
                    method: Method::Manual(String::new()),
                    prepared: None,
                    prepared_hash: None,
                    rev: 0,
                },
                job: None,
                next_auto: None,
                event: None,
                auto_fails: (String::new(), 0),
                done: false,
            }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            src,
            started: Instant::now(),
            first,
        });
        let w = u.clone();
        let spawned = std::thread::Builder::new().name("bs-update".into()).spawn(move || w.work());
        if spawned.is_err() {
            lock(&u.inner).done = true;
        }
        u
    }

    pub fn status(&self) -> Status {
        lock(&self.inner).st.clone()
    }

    fn set(&self, f: impl FnOnce(&mut Status)) {
        let mut g = lock(&self.inner);
        f(&mut g.st);
        g.st.rev += 1;
    }

    fn queue(&self, job: Job) -> Result<(), String> {
        let mut g = lock(&self.inner);
        if g.done {
            return Err("the updater is not running".into());
        }
        if g.st.phase.busy() || g.job.is_some() {
            return Err("already busy with an update".into());
        }
        g.job = Some(job);
        // busy from now on, also for the page's next frame
        g.st.phase = match job {
            Job::Check { .. } => Phase::Checking,
            Job::Install { .. } => Phase::Installing,
        };
        g.st.rev += 1;
        drop(g);
        self.cv.notify_all();
        Ok(())
    }

    /// "Check now".
    pub fn check(&self) -> Result<String, String> {
        self.queue(Job::Check { auto: false }).map(|()| "checking for updates".into())
    }

    /// "Install": the version found by the last check.
    pub fn install(&self) -> Result<String, String> {
        let st = self.status();
        match (&st.offer, st.phase) {
            (Some(o), Phase::Available | Phase::Prepared | Phase::Failed) if newer(&o.version, st.installed.as_deref().unwrap_or(VERSION)) => {
                if let Method::Manual(why) = &st.method {
                    return Err(why.clone());
                }
                self.queue(Job::Install { auto: false }).map(|()| format!("installing BananaSplit {}", o.version))
            }
            (_, Phase::Ready) => Err("the update is installed: restart BananaSplit to use it".into()),
            _ => Err("there is no new version to install (check for updates first)".into()),
        }
    }

    /// A restart would start the version this session installed.
    pub fn ready(&self) -> bool {
        let g = lock(&self.inner);
        g.st.installed.is_some() && !g.st.phase.busy()
    }

    /// What the session line should say about an automatic update.
    pub fn take_event(&self) -> Option<String> {
        lock(&self.inner).event.take()
    }

    /// Called by the GUI's loop every few seconds: with automatic updates
    /// on, check a minute after the start and then once a day.
    pub fn tick(&self, auto: bool) {
        let mut g = lock(&self.inner);
        if !auto {
            g.next_auto = None;
            return;
        }
        let now = Instant::now();
        let next = *g.next_auto.get_or_insert_with(|| (self.started + self.first).max(now + self.first.min(Duration::from_secs(5))));
        if now < next || g.st.phase.busy() || g.job.is_some() || g.done {
            return;
        }
        g.next_auto = Some(now + RECHECK);
        g.job = Some(Job::Check { auto: true });
        drop(g);
        self.cv.notify_all();
    }

    pub fn shutdown(&self) {
        self.stop.store(true, SeqCst);
        let g = lock(&self.inner);
        self.cv.notify_all();
        // Not for long: a password dialog may be open (its apt-get goes on).
        let g = self.cv.wait_timeout_while(g, Duration::from_secs(3), |i| !i.done);
        drop(g);
        // But never in the middle of renaming the program's files.
        let t0 = Instant::now();
        while RENAMING.load(SeqCst) && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn work(&self) {
        let m = method();
        self.set(|s| s.method = m);
        loop {
            let job = {
                let mut g = lock(&self.inner);
                loop {
                    if self.stop.load(SeqCst) {
                        g.done = true;
                        self.cv.notify_all();
                        return;
                    }
                    if let Some(j) = g.job {
                        break j;
                    }
                    g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            };
            // A panic here must not leave the updater "busy" for good.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                Job::Check { auto } => self.do_check(auto),
                Job::Install { auto } => self.do_install(auto),
            }));
            let mut g = lock(&self.inner);
            if r.is_err() {
                g.st.phase = Phase::Failed;
                g.st.message = "the updater ran into an internal error".into();
                g.st.rev += 1;
            }
            if g.job == Some(job) {
                g.job = None;
            }
        }
    }

    fn do_check(&self, auto: bool) {
        let method = method();
        let pkg = method.package();
        self.set(|s| {
            s.phase = Phase::Checking;
            s.message = "checking for updates…".into();
            s.method = method.clone();
        });
        let found = find(&self.src, pkg, &self.stop);
        let mut install = false;
        {
            let mut g = lock(&self.inner);
            let inner = &mut *g;
            let s = &mut inner.st;
            s.checked = Some(Instant::now());
            s.rev += 1;
            let have = s.installed.clone().unwrap_or_else(|| VERSION.to_string());
            match found {
                Err(e) => {
                    if s.installed.is_some() {
                        // what was installed still waits for the restart
                        s.phase = Phase::Ready;
                        s.message = format!("BananaSplit {have} is installed: restart BananaSplit to use it. (Checking again failed: {e})");
                    } else {
                        s.phase = Phase::Failed;
                        s.message = format!("Could not check for updates: {e}");
                    }
                    if auto {
                        inner.next_auto = Some(Instant::now() + RETRY);
                    }
                }
                Ok(Some(o)) if newer(&o.version, &have) => {
                    let keep = s.prepared.as_ref().is_some_and(|p| p.is_file()) && s.offer.as_ref().is_some_and(|p| p.version == o.version && p.asset == o.asset);
                    if !keep {
                        s.prepared = None;
                        s.prepared_hash = None;
                    }
                    s.message = match &method {
                        Method::Manual(why) => format!("BananaSplit {} is available. It cannot install itself here: {why}.", o.version),
                        _ if keep => format!("BananaSplit {} is downloaded and checked: install it now (asks for your password).", o.version),
                        _ => format!("BananaSplit {} is available.", o.version),
                    };
                    s.phase = if keep { Phase::Prepared } else { Phase::Available };
                    let gave_up = inner.auto_fails.0 == o.version && inner.auto_fails.1 >= 2;
                    if auto && !gave_up {
                        if matches!(method, Method::Manual(_)) || keep {
                            inner.event = Some(format!("BananaSplit {} is available: Menu ▸ Updates", o.version));
                        } else {
                            install = true;
                        }
                    }
                    s.offer = Some(o);
                }
                Ok(o) => {
                    if s.installed.is_some() {
                        s.phase = Phase::Ready;
                        s.message = format!("BananaSplit {have} is installed: restart BananaSplit to use it.");
                    } else {
                        s.phase = Phase::Current;
                        s.message = format!("BananaSplit {VERSION} is the newest version.");
                    }
                    s.offer = o;
                    s.prepared = None;
                    s.prepared_hash = None;
                }
            }
        }
        if install {
            self.do_install(true);
        }
    }

    fn do_install(&self, auto: bool) {
        let st = self.status();
        let Some(offer) = st.offer.clone() else { return };
        let method = st.method.clone();
        if let Method::Manual(why) = &method {
            self.set(|s| s.message = why.clone());
            return;
        }
        let v = offer.version.clone();
        let fail = |e: String| {
            let mut g = lock(&self.inner);
            g.st.phase = Phase::Failed;
            g.st.message = format!("Updating to {v} failed: {e}");
            g.st.rev += 1;
            if auto {
                g.event = Some(format!("Automatic update to BananaSplit {v} failed: Menu ▸ Updates"));
                g.next_auto = Some(Instant::now() + RETRY);
                if g.auto_fails.0 == v {
                    g.auto_fails.1 += 1;
                } else {
                    g.auto_fails = (v.clone(), 1);
                }
            }
        };
        if method == Method::Deb && self.src.is_test() {
            // a package is installed as root: only ever from GitHub
            return fail("a package is installed only from GitHub, not from a test server".into());
        }
        // The .deb that was already downloaded and checked, still unchanged.
        let ready = st.prepared.clone().zip(st.prepared_hash).filter(|(p, h)| sha256_file(p).ok() == Some(*h));
        let (file, hash) = match ready {
            Some(f) => f,
            None => {
                self.set(|s| {
                    s.phase = Phase::Downloading;
                    s.got = 0;
                    s.total = offer.asset.size;
                    s.message = format!("downloading BananaSplit {v}…");
                    s.prepared = None;
                    s.prepared_hash = None;
                });
                let mut progress = |n: u64| lock(&self.inner).st.got = n.min(offer.asset.size);
                match download(&self.src, &offer, &self.stop, &mut progress) {
                    Ok(f) => f,
                    Err(e) => return fail(e),
                }
            }
        };
        if method == Method::Deb && auto {
            let mut g = lock(&self.inner);
            g.st.phase = Phase::Prepared;
            g.st.prepared = Some(file);
            g.st.prepared_hash = Some(hash);
            g.st.message = format!("BananaSplit {v} is downloaded and checked: install it now (asks for your password).");
            g.st.rev += 1;
            g.event = Some(format!("BananaSplit {v} is ready to install: Menu ▸ Updates"));
            return;
        }
        self.set(|s| {
            s.phase = Phase::Installing;
            s.message = if method == Method::Deb { format!("installing BananaSplit {v} (enter your password)…") } else { format!("installing BananaSplit {v}…") };
        });
        let r = install(&method, &offer, &file, &self.stop, false);
        if r.is_err() && method == Method::Deb {
            // kept (checked) for another try, and for the command the message names
            let mut g = lock(&self.inner);
            g.st.prepared = Some(file.clone());
            g.st.prepared_hash = Some(hash);
        } else if let Some(d) = file.parent() {
            clean_cache(d);
        }
        match r {
            Ok((_, warnings)) => {
                let mut g = lock(&self.inner);
                g.st.phase = Phase::Ready;
                g.st.installed = Some(v.clone());
                g.st.prepared = None;
                g.st.prepared_hash = None;
                let extra = if warnings.is_empty() { String::new() } else { format!(" Note: {}.", warnings.join("; ")) };
                g.st.message = format!("BananaSplit {v} is installed. Restart BananaSplit to use it.{extra}");
                g.st.rev += 1;
                if auto {
                    g.event = Some(format!("BananaSplit {v} was installed: restart BananaSplit to use it (Menu ▸ Updates)"));
                }
            }
            Err(e) => fail(e),
        }
    }

    /// For the page's state (every frame): small.
    pub fn json(&self, o: &mut String, auto: bool) {
        let st = self.status();
        let _ = write!(o, "{{\"auto\":{auto},\"phase\":\"{}\",\"current\":", st.phase.as_str());
        crate::gui::jstr(o, VERSION);
        o.push_str(",\"latest\":");
        match &st.offer {
            Some(of) => crate::gui::jstr(o, &of.version),
            None => o.push_str("null"),
        }
        o.push_str(",\"installed\":");
        match &st.installed {
            Some(v) => crate::gui::jstr(o, v),
            None => o.push_str("null"),
        }
        o.push_str(",\"message\":");
        crate::gui::jstr(o, &st.message);
        let ago = st.checked.map(|c| c.elapsed().as_secs());
        let can = matches!(st.phase, Phase::Available | Phase::Prepared | Phase::Failed)
            && !matches!(st.method, Method::Manual(_))
            && st.offer.as_ref().is_some_and(|of| newer(&of.version, st.installed.as_deref().unwrap_or(VERSION)));
        let _ = write!(
            o,
            ",\"got\":{},\"total\":{},\"method\":\"{}\",\"canInstall\":{can},\"checkedAgo\":{},\"rev\":{}}}",
            st.got,
            st.total,
            st.method.as_str(),
            ago.map_or("null".into(), |a| a.to_string()),
            st.rev
        );
    }

    /// `GET /api/update`: the release's page and notes too.
    pub fn details_json(&self) -> String {
        let st = self.status();
        let mut o = String::from("{\"page\":");
        crate::gui::jstr(&mut o, st.offer.as_ref().map_or(RELEASES_PAGE, |of| of.page.as_str()));
        o.push_str(",\"notes\":");
        crate::gui::jstr(&mut o, st.offer.as_ref().map_or("", |of| of.notes.as_str()));
        o.push_str(",\"manual\":");
        match &st.method {
            Method::Manual(why) if !why.is_empty() => crate::gui::jstr(&mut o, why),
            _ => o.push_str("null"),
        }
        let _ = write!(o, ",\"rev\":{}}}", st.rev);
        o
    }

    /// The page to open for "Release page".
    pub fn page(&self) -> String {
        self.status().offer.map_or(RELEASES_PAGE.to_string(), |o| o.page)
    }
}

// ---------------------------------------------------------------------------
// Restarting into the new version
// ---------------------------------------------------------------------------

/// Set by "Restart now": `main` starts the program again once this one has
/// saved its settings and let go of the audio.
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// The command line for the restarted BananaSplit: the same, but with its
/// window (`--hidden`, and on Windows `--no-open`, start without one).
pub fn restart_args(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| *a != "--hidden" && !(cfg!(windows) && *a == "--no-open")).cloned().collect()
}

/// Start the program again (it waits for this process to end).
/// `drop_env`: variables this process set for itself.
pub fn spawn_restarted(drop_env: &[&str]) -> Result<(), String> {
    let program = self_path().ok_or("cannot find this program's file")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut c = command(&program);
    c.args(restart_args(&args)).stdout(Stdio::null()).stderr(Stdio::null());
    for v in drop_env.iter().chain(&["DESKTOP_STARTUP_ID", "XDG_ACTIVATION_TOKEN", WAIT_ENV]) {
        c.env_remove(v);
    }
    c.env(WAIT_ENV, std::process::id().to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Out of a job that would end it with this process, where allowed.
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        c.creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        if c.spawn().is_ok() {
            return Ok(());
        }
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c.spawn().map(drop).map_err(|e| format!("cannot start {}: {e}", program.display()))
}

/// Another update is still writing it (Linux).
fn pid_alive(pid: &str) -> bool {
    #[cfg(unix)]
    return pid.parse().is_ok_and(alive);
    #[cfg(windows)]
    {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const WAIT_TIMEOUT: u32 = 0x102;
        let Ok(pid) = pid.parse::<u32>() else { return false };
        // SAFETY: a handle we open and close; a zero wait only asks.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            CloseHandle(h);
            running
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // a zombie (exited, not yet reaped by its parent) is gone
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|s| s.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()).is_some_and(|c| c != 'Z' && c != 'X'))
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(h: *mut std::ffi::c_void, ms: u32) -> u32;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
}

/// A restarted BananaSplit: wait (20 s at most) for the one it replaces to
/// end. Call while the program is still single-threaded.
pub fn wait_for_previous() {
    let Some(v) = std::env::var_os(WAIT_ENV) else { return };
    // SAFETY: called at the start of main, before any thread is started.
    unsafe { std::env::remove_var(WAIT_ENV) };
    let Some(pid) = v.to_str().and_then(|s| s.parse::<u32>().ok()).filter(|&p| p != 0 && p != std::process::id()) else { return };
    #[cfg(unix)]
    {
        let t0 = Instant::now();
        while alive(pid) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(windows)]
    {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        // SAFETY: a handle we open and close; WaitForSingleObject only waits.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                WaitForSingleObject(h, 20_000);
                CloseHandle(h);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `bananasplit --update`
// ---------------------------------------------------------------------------

/// Check, and install a new version if there is one (no restart: the next
/// start runs it).
/// `running`: a BananaSplit app runs (Windows: it would start its window
/// from the new file, which is not its version; update it from its menu).
pub fn cli(running: bool) -> Result<(), String> {
    let src = Source::from_env();
    let never = AtomicBool::new(false);
    let method = method();
    println!("bananasplit: this is BananaSplit {VERSION}; checking {} …", if src.https { "GitHub" } else { src.api.as_str() });
    let Some(offer) = find(&src, method.package(), &never)?.filter(|o| newer(&o.version, VERSION)) else {
        println!("bananasplit: BananaSplit {VERSION} is the newest version");
        return Ok(());
    };
    println!("bananasplit: BananaSplit {} is available ({})", offer.version, offer.page);
    if let Method::Manual(why) = &method {
        return Err(format!("it cannot install itself here: {why}"));
    }
    if running {
        return Err("BananaSplit is running: update it from its Menu ▸ Check for updates…, or quit it first".into());
    }
    if method == Method::Deb && !src.https {
        return Err("a package is installed only from GitHub, not from a test server".into());
    }
    println!("bananasplit: downloading {} ({:.1} MB) …", offer.asset.name, offer.asset.size as f64 / 1e6);
    let (file, _) = download(&src, &offer, &never, &mut |_| {})?;
    println!("bananasplit: the download matches the release's checksums");
    #[cfg(unix)]
    if method == Method::Deb && !crate::sys::stdin_is_tty() {
        return Err(format!("installing the package needs your password; in a terminal run:  sudo apt install {}", file.display()));
    }
    let r = install(&method, &offer, &file, &never, true);
    if r.is_ok() || method != Method::Deb {
        if let Some(d) = file.parent() {
            clean_cache(d);
        }
    }
    let (r, warnings) = r?;
    println!("bananasplit: installed BananaSplit {} ({r}); a running BananaSplit uses it after a restart", offer.version);
    for w in warnings {
        println!("bananasplit: note: {w}");
    }
    Ok(())
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
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "01", "1.", "-", "\"a", "\"\u{1}\"", "tru", "[1] 2", "{1:2}", "\"\\x\"", "\"\\u12\""] {
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

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("2.7.4", "2.7.3"));
        assert!(newer("2.10.0", "2.9.9"));
        assert!(newer("3", "2.99.99"));
        assert!(newer("2.7.3.1", "2.7.3"));
        assert!(!newer("2.7.3", "2.7.3"));
        assert!(!newer("2.7", "2.7.0"));
        assert!(!newer("2.7.2", "2.7.10"));
        assert!(!newer("2.7.x", "1.0"));
        assert!(!newer("", "1.0"));
        assert!(!newer("1.0.0.0.0", "0.1"));
        assert!(newer(VERSION, "0.0.1") && parse_version(VERSION).is_some());
        assert_eq!(Package::Tarball.version_of("bananasplit-2.7.4-linux-x86_64.tar.gz").as_deref(), Some("2.7.4"));
        assert_eq!(Package::Deb.version_of("bananasplit_2.7.4_amd64.deb").as_deref(), Some("2.7.4"));
        assert_eq!(Package::WindowsExe.version_of("bananasplit-2.7.4-windows-x64.exe").as_deref(), Some("2.7.4"));
        assert_eq!(Package::WindowsExe.version_of("bananasplit-2.7.4-windows-x64.zip"), None);
        assert_eq!(Package::Tarball.version_of("bananasplit-2.7-beta-linux-x86_64.tar.gz"), None);
        assert_eq!(Package::Tarball.version_of("bananasplit_2.7.tar.gz"), None);
        for p in [Package::Tarball, Package::Deb, Package::WindowsExe] {
            assert_eq!(p.version_of(&p.file_name("10.0.1")).as_deref(), Some("10.0.1"));
        }
    }

    #[test]
    fn only_a_server_on_this_computer_stands_in_for_github() {
        assert_eq!(Source::local("http://127.0.0.1:8123").unwrap().download, "http://127.0.0.1:8123/LudaLunizz/BananaSplit/releases/download/");
        assert!(Source::local("http://localhost:8123").is_some());
        for bad in ["http://127.0.0.1", "http://127.0.0.1:", "http://127.0.0.1:0", "http://127.0.0.1:99999", "http://127.0.0.1:80/x",
                    "http://127.0.0.10:80", "https://evil.example:80", "http://127.0.0.1.evil:80", "http://127.0.0.1:80@evil"] {
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
            release("v2.7.10", false, false, &[("bananasplit-2.7.10-windows-x64.exe", 10, Some(&d)), ("SHA256SUMS.txt", 300, None)]),
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
        assert_eq!(o.notes, "Fixes.\nMore.");
        assert_eq!(o.page, "https://github.com/LudaLunizz/BananaSplit/releases/tag/v2.7.10");
        let t = pick(&j, Package::Tarball, &src).unwrap().unwrap();
        assert_eq!((t.version.as_str(), t.asset.digest, t.sums.is_none()), ("2.8.0", None, true));
        assert_eq!(pick(&j, Package::Deb, &src).unwrap(), None);
        assert!(pick(&Json::parse(r#"{"message":"API rate limit exceeded"}"#).unwrap(), Package::Deb, &src).unwrap_err().contains("rate limit"));
        assert_eq!(pick(&Json::parse("[]").unwrap(), Package::Deb, &src).unwrap(), None);
    }

    /// GitHub's real answer (the releases of 25 October 2026: 2.7.3, the
    /// 2.7.1 pre-release, 2.7.0).
    #[test]
    fn githubs_real_release_list_is_understood() {
        let j = Json::parse(include_str!("../tests/update/github-releases.json")).unwrap();
        let src = Source::github();
        let want = [
            (Package::Tarball, "bananasplit-2.7.3-linux-x86_64.tar.gz", "205c5e561e406b0b6a92c98f16ed5318aad0428f273d8d7ed68cb37a6e097f03"),
            (Package::Deb, "bananasplit_2.7.3_amd64.deb", "b7032402e0195141f50d1bfa446dffab918c80630e01d03c31feb40961fc94ff"),
            (Package::WindowsExe, "bananasplit-2.7.3-windows-x64.exe", "247e304c69470d6a65b4467a859263448b38a6cfdd59574f901830bb930dd9e0"),
        ];
        for (pkg, name, digest) in want {
            let o = pick(&j, pkg, &src).unwrap().unwrap();
            assert_eq!((o.version.as_str(), o.tag.as_str(), o.asset.name.as_str()), ("2.7.3", "v2.7.3", name));
            assert_eq!(o.asset.digest, parse_hex32(digest));
            assert_eq!(o.asset.url, format!("https://github.com/LudaLunizz/BananaSplit/releases/download/v2.7.3/{name}"));
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
        assert!(one("https://evil.example/LudaLunizz/BananaSplit/releases/download/v9/bananasplit_9.0.0_amd64.deb", 5, "").is_none());
        assert!(one("http://github.com/LudaLunizz/BananaSplit/releases/download/v9/bananasplit_9.0.0_amd64.deb", 5, "").is_none());
        assert!(one("https://github.com/LudaLunizz/BananaSplit/releases/download/v9/other.deb", 5, "").is_none(), "the link names the file");
        assert!(one("https://github.com/LudaLunizz/BananaSplit/releases/download/v9/x y/bananasplit_9.0.0_amd64.deb", 5, "").is_none());
        assert!(one(good, 0, "").is_none(), "empty");
        assert!(one(good, MAX_ASSET + 1, "").is_none(), "too big");
        let j = format!(
            r#"[{{"tag_name":"v9","draft":false,"prerelease":false,"assets":[{{"name":"bananasplit_9.0.0_amd64.deb","state":"starter","size":5,"browser_download_url":"{good}"}}]}}]"#
        );
        assert_eq!(pick(&Json::parse(&j).unwrap(), Package::Deb, &src).unwrap(), None, "an upload that did not finish");
        let j = format!(r#"[{{"tag_name":"v9","assets":[{{"name":"bananasplit_9.0.0_amd64.deb","size":5,"browser_download_url":"{good}"}}]}}]"#);
        assert_eq!(pick(&Json::parse(&j).unwrap(), Package::Deb, &src).unwrap(), None, "draft/prerelease must say false");
    }

    #[test]
    fn curl_versions_are_read() {
        assert_eq!(parse_curl_version("curl 8.4.0 (Windows) libcurl/8.4.0 Schannel\nRelease-Date: x\n"), Some((8, 4)));
        assert_eq!(parse_curl_version("curl 7.55.1 (Windows) libcurl/7.55.1 WinSSL\n"), Some((7, 55)));
        assert!(parse_curl_version("curl 7.55.1").unwrap() < (7, 70) && (7, 70) <= parse_curl_version("curl 7.70.0").unwrap());
        assert_eq!(parse_curl_version("wget 1.21"), None);
        assert_eq!(parse_curl_version(""), None);
    }

    #[test]
    fn download_links_cannot_leave_the_releases() {
        let p = "https://github.com/LudaLunizz/BananaSplit/releases/download/";
        assert!(clean_url(&format!("{p}v9/bananasplit_9.0.0_amd64.deb"), p));
        assert!(clean_url(&format!("{p}v9/a.b..c.deb"), p), "dots inside a name are fine");
        for bad in ["v9/../../../other/x.deb", "v9/./x.deb", "v9/%2e%2e/x.deb", "v9/%2E%2E/x.deb", "..", "v9/x .deb", "v9/x\"y.deb"] {
            assert!(!clean_url(&format!("{p}{bad}"), p), "{bad}");
        }
        assert!(!clean_url("https://github.com/LudaLunizz/BananaSplit/", p));
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
        let text = format!("{a}  bananasplit-2.7.4-windows-x64.exe\n{b} *bananasplit_2.7.4_amd64.deb\r\n");
        assert_eq!(sum_for(&text, "bananasplit-2.7.4-windows-x64.exe"), parse_hex32(&a));
        assert_eq!(sum_for(&text, "bananasplit_2.7.4_amd64.deb"), parse_hex32(&b));
        assert_eq!(sum_for(&text, "bananasplit-2.7.4-windows-x64.zip"), None);
        assert_eq!(sum_for(&text, "windows-x64.exe"), None);
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bs-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn offer_for(file: &Path, digest: Option<[u8; 32]>) -> Offer {
        let size = std::fs::metadata(file).unwrap().len();
        Offer {
            version: "9.9.9".into(),
            tag: "v9.9.9".into(),
            page: RELEASES_PAGE.into(),
            notes: String::new(),
            asset: Asset { name: "bananasplit_9.9.9_amd64.deb".into(), size, url: String::new(), digest },
            sums: None,
        }
    }

    #[test]
    fn a_download_must_match_every_checksum_the_release_has() {
        let d = temp("verify");
        let f = d.join("bananasplit_9.9.9_amd64.deb");
        std::fs::write(&f, b"the package").unwrap();
        let good = sha256(b"the package");
        let bad = sha256(b"something else");
        let sums = |h: &[u8; 32]| format!("{}  bananasplit_9.9.9_amd64.deb\n", hex(h));
        assert!(verify(&f, &offer_for(&f, Some(good)), None).is_ok());
        assert!(verify(&f, &offer_for(&f, None), Some(&sums(&good))).is_ok());
        assert!(verify(&f, &offer_for(&f, Some(good)), Some(&sums(&good))).is_ok());
        assert!(verify(&f, &offer_for(&f, Some(bad)), None).unwrap_err().contains("does not match GitHub's checksum"));
        assert!(verify(&f, &offer_for(&f, Some(good)), Some(&sums(&bad))).unwrap_err().contains("SHA256SUMS.txt"));
        assert!(verify(&f, &offer_for(&f, None), Some("")).unwrap_err().contains("does not list"));
        assert!(verify(&f, &offer_for(&f, None), None).unwrap_err().contains("no checksum"));
        let mut o = offer_for(&f, Some(good));
        o.asset.size += 1;
        assert!(verify(&f, &o, None).unwrap_err().contains("bytes"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_what_an_update_left_is_cleaned_up() {
        let dead = |_: &str| false;
        let live = |p: &str| p == "77";
        // Windows
        assert!(stale(true, "bananasplit.exe", "bananasplit.exe.old", 5, &dead));
        assert!(stale(true, "bananasplit.exe", "bananasplit.exe.old-123", 5, &dead));
        assert!(stale(true, "bananasplit.exe", "bananasplit.update-123.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-5.exe", 5, &dead), "this process's own");
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-77.exe", 5, &live), "a running update's");
        assert!(!stale(true, "bananasplit.exe", "bananasplit.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.exe.old-x", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "README.md", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "other.exe.old", 5, &dead));
        assert!(stale(true, "bs-2.7.4.exe", "bs-2.7.4.exe.old", 5, &dead), "named after the program file");
        // Linux
        assert!(stale(false, "bananasplit", ".bananasplit.update-123", 5, &dead));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-5", 5, &dead));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-77", 5, &live));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-12a", 5, &dead));
        assert!(!stale(false, "bananasplit", "bananasplit.exe.old", 5, &dead), "Windows' names are not Linux's");
        assert!(!stale(false, "bananasplit", "bananasplit", 5, &dead));
    }

    #[test]
    fn a_restart_brings_the_window_back() {
        let a: Vec<String> = ["--gui", "--hidden", "--config", "/x/--hidden.ini", "--gui-port", "9000"].iter().map(|s| s.to_string()).collect();
        assert_eq!(restart_args(&a), ["--gui", "--config", "/x/--hidden.ini", "--gui-port", "9000"]);
        let b: Vec<String> = ["--gui", "--no-open"].iter().map(|s| s.to_string()).collect();
        assert_eq!(restart_args(&b), if cfg!(windows) { vec!["--gui"] } else { vec!["--gui", "--no-open"] });
    }

    /// A one-file HTTP server on 127.0.0.1 for `fetch`: answers every
    /// request with `status` and `body`.
    fn serve(status: &'static str, body: Vec<u8>, hits: usize) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, Write};
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..hits {
                let Ok((s, _)) = l.accept() else { break };
                let mut r = std::io::BufReader::new(s);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                seen.push(head);
                let mut s = r.into_inner();
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
            seen
        });
        (port, h)
    }

    #[test]
    fn downloads_go_through_curl_and_fail_cleanly() {
        if find_tool().is_none() {
            eprintln!("no curl/wget here: skipped");
            return;
        }
        let d = temp("fetch");
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (port, h) = serve("200 OK", body.clone(), 1);
        let src = Source::local(&format!("http://127.0.0.1:{port}")).unwrap();
        let never = AtomicBool::new(false);
        let dest = d.join("file");
        let mut last = 0;
        fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, body.len() as u64, true, &never, &mut |n| last = n).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(last, body.len() as u64, "progress reaches the size");
        let head = h.join().unwrap().join("");
        assert!(head.contains("User-Agent: BananaSplit/"), "{head}");
        assert!(head.contains("Accept: application/vnd.github+json"), "{head}");

        let (port, h) = serve("404 Not Found", b"nope".to_vec(), 1);
        let e = fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 100, false, &never, &mut |_| {}).unwrap_err();
        assert!(e.contains("404") || e.contains("error"), "{e}");
        assert!(!dest.exists() && !d.join("file.part").exists(), "nothing left behind");
        h.join().unwrap();

        let (port, h) = serve("200 OK", body.clone(), 1);
        let e = fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {}).unwrap_err();
        assert!(e.contains("larger"), "{e}");
        assert!(!dest.exists());
        h.join().unwrap();

        // nothing listening
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert!(fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {}).is_err());
        // https only, when the source is GitHub
        let e = fetch(&Source::github(), &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {}).unwrap_err();
        assert!(!e.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    fn script(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_program_is_replaced_only_by_one_that_runs_and_has_the_version() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("replace");
        let (a, b) = (d.join("app"), d.join("bin"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (t1, t2) = (a.join("bananasplit"), b.join("bananasplit"));
        std::fs::write(&t1, b"old one").unwrap();
        std::fs::write(&t2, b"old two").unwrap();
        let new = d.join("new");
        script(&new, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
        // the wrong version: nothing changes, nothing is left behind
        let e = replace(&new, &[t1.clone(), t2.clone()], "9.9.8").unwrap_err();
        assert!(e.contains("says it is BananaSplit 9.9.9, not 9.9.8"), "{e}");
        assert_eq!(std::fs::read(&t1).unwrap(), b"old one");
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 1, "the staged file is removed");
        // one that does not run
        let broken = d.join("broken");
        script(&broken, "#!/bin/sh\nexit 3\n");
        assert!(replace(&broken, std::slice::from_ref(&t1), "9.9.9").unwrap_err().contains("does not run on this computer"));
        assert_eq!(std::fs::read(&t1).unwrap(), b"old one");
        // the right one: both copies, executable
        replace(&new, &[t1.clone(), t2.clone()], "9.9.9").unwrap();
        for t in [&t1, &t2] {
            assert_eq!(std::fs::read(t).unwrap(), std::fs::read(&new).unwrap());
            assert_eq!(std::fs::metadata(t).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 1);
        assert!(check_program(&t1, "9.9.9").is_ok());
        assert!(check_program(&t1, "9.9").is_err(), "the whole version");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_that_cannot_be_replaced_after_the_running_one_is_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("partial");
        let (a, b) = (d.join("app"), d.join("ro"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (t1, t2) = (a.join("bananasplit"), b.join("bananasplit"));
        std::fs::write(&t1, b"old").unwrap();
        std::fs::write(&t2, b"old").unwrap();
        let new = d.join("new");
        script(&new, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
        // a second "copy" whose folder is gone: it cannot be written
        let gone = d.join("gone").join("bananasplit");
        let w = replace(&new, &[t1.clone(), gone.clone()], "9.9.9").unwrap();
        assert_eq!(std::fs::read(&t1).unwrap(), std::fs::read(&new).unwrap(), "the running program is new");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("was not updated"), "{w:?}");
        // the first one failing is an error
        assert!(replace(&new, &[gone, t2.clone()], "9.9.9").is_err());
        assert_eq!(std::fs::read(&t2).unwrap(), b"old");
        let _ = std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_package_goes_through_pkexec_apt_get() {
        let d = temp("deb");
        let log = d.join("log");
        let pkexec = d.join("pkexec");
        let program = d.join("bananasplit");
        let deb = d.join("bananasplit_9.9.9_amd64.deb");
        std::fs::write(&deb, b"deb").unwrap();
        let never = AtomicBool::new(false);
        let apt = Path::new("/usr/bin/apt-get");
        for (code, installs, want) in [(0, true, ""), (126, false, "cancelled"), (127, false, "could not get permission"), (100, false, "apt-get failed (exit 100)")] {
            script(&pkexec, &format!("#!/bin/sh\necho \"$@\" > {}\necho 'E: something' >&2\nexit {code}\n", log.display()));
            script(&program, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
            let r = install_deb_with(&pkexec, apt, &program, &deb, "9.9.9", &never, false);
            assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), format!("/usr/bin/apt-get install -y {}", deb.display()));
            match installs {
                true => assert!(r.is_ok(), "{r:?}"),
                false => assert!(r.as_ref().unwrap_err().contains(want), "{code}: {r:?}"),
            }
        }
        // installed, but the program is not the new version: an error
        script(&pkexec, "#!/bin/sh\nexit 0\n");
        script(&program, "#!/bin/sh\necho 'BananaSplit 2.7.3'\n");
        assert!(install_deb_with(&pkexec, apt, &program, &deb, "9.9.9", &never, false).unwrap_err().contains("not 9.9.9"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_package_unpacks_to_its_program_only() {
        if which("tar").is_none() || which("gzip").is_none() {
            return;
        }
        let d = temp("unpack");
        let top = d.join("src").join("bananasplit-9.9.9-linux-x86_64");
        std::fs::create_dir_all(&top).unwrap();
        std::fs::write(top.join("bananasplit"), b"program").unwrap();
        std::fs::write(top.join("install.sh"), b"#!/bin/sh\n").unwrap();
        let tgz = d.join("bananasplit-9.9.9-linux-x86_64.tar.gz");
        let ok = Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(d.join("src")).arg("bananasplit-9.9.9-linux-x86_64").status().unwrap();
        assert!(ok.success());
        let bin = unpack(&tgz, "9.9.9").unwrap();
        assert_eq!(std::fs::read(&bin).unwrap(), b"program");
        assert!(!bin.parent().unwrap().join("install.sh").exists(), "only the program");
        assert!(unpack(&tgz, "9.9.8").is_err(), "the version's folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_zombie_is_not_waited_for() {
        assert!(alive(std::process::id()));
        let mut c = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        let pid = c.id();
        let t0 = Instant::now();
        while alive(pid) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(pid), "exited, not yet reaped");
        c.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn the_updater_is_idle_until_asked() {
        let u = Updater::start();
        // no automatic check while it is off, however long it runs
        for _ in 0..3 {
            u.tick(false);
        }
        assert_eq!(u.status().phase, Phase::Idle);
        assert!(u.install().unwrap_err().contains("no new version"));
        let mut o = String::new();
        u.json(&mut o, false);
        assert!(o.starts_with(&format!("{{\"auto\":false,\"phase\":\"idle\",\"current\":\"{VERSION}\",\"latest\":null,")), "{o}");
        Json::parse(&o).unwrap();
        Json::parse(&u.details_json()).unwrap();
        u.shutdown();
        assert!(u.check().is_err(), "stopped");
    }
}
