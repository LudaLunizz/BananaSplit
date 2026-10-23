//! Terminal control surface (ANSI escape codes, no dependencies).
//!
//! ```text
//!   #  NAME             GAIN  L          R          PEAK M S O E  A1 A2 A3 B1 B2
//! > 1● Mic             -12.0  ████████·· ███████···  -18 M · O E  A1 A2 A3 B1 B2
//!  A2  Blue Headphones  +0.0  ████······ ████······  -31 ·   · ·
//! ```
//!
//! Fits 80 columns with 15-character names; a wider terminal gives the
//! extra columns to the names (up to 32, the longest name BananaSplit accepts).
//! A longer name ends in `…`; the line under the mixer always shows the
//! selected row's full name.
//!
//! `●` after the number = channel is linked to the recorder (`c` toggles it,
//! `R` starts/stops a recording). `E` in the flags = the channel's EQ is on
//! (`e` toggles it); shift-`E` opens the EQ page, where the six bands of the
//! selected channel are edited. The line under the header shows VBAN when it
//! is on (`v` switches it on and off).

use std::fmt::Write as _;
use std::sync::atomic::Ordering::Relaxed;
use bananasplit::config::Config;
use bananasplit::engine::{BUS_IDS, MAX_GAIN_DB, MIN_GAIN_DB, NUM_BUSES, NUM_STRIPS, Shared, gain_to_db};
use bananasplit::eq::{self, BANDS, Band, DEFAULT_BANDS, Kind};
use bananasplit::recorder::source_id;

pub const ROWS: usize = NUM_STRIPS + NUM_BUSES;
/// The screen is laid out for at least this many columns.
pub const MIN_COLS: usize = 80;
const METER_W: usize = 10;
const NAME_W: usize = 15;
const MAX_NAME_W: usize = 32;
const FLOOR_DB: f32 = -60.0;
const DECAY_DB_PER_FRAME: f32 = 1.5; // ~45 dB/s at 30 fps

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Char(u8),
}

/// Decode raw terminal bytes into keys. An incomplete escape sequence at the
/// end of `buf` is left in place for the next call.
pub fn decode(buf: &mut Vec<u8>) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let b = buf[i];
        if b == 0x1b {
            if i + 1 >= buf.len() {
                break; // wait for more bytes
            }
            if buf[i + 1] == b'[' || buf[i + 1] == b'O' {
                if i + 2 >= buf.len() {
                    break;
                }
                // skip parameters like "1;2" until the final byte
                let mut j = i + 2;
                while j < buf.len() && (buf[j].is_ascii_digit() || buf[j] == b';') {
                    j += 1;
                }
                if j >= buf.len() {
                    break;
                }
                match buf[j] {
                    b'A' => keys.push(Key::Up),
                    b'B' => keys.push(Key::Down),
                    b'C' => keys.push(Key::Right),
                    b'D' => keys.push(Key::Left),
                    _ => {}
                }
                i = j + 1;
            } else {
                i += 1; // lone ESC: ignore
            }
        } else {
            keys.push(Key::Char(b));
            i += 1;
        }
    }
    buf.drain(..i.min(buf.len()));
    keys
}

pub enum Action {
    None,
    Quit,
    Save,
    Reconnect,
    /// Start or stop the recorder.
    Record,
    /// Switch VBAN on or off.
    ToggleVban,
    /// Something to say on the status line.
    Note(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Mixer,
    /// The selected channel's EQ bands.
    Eq,
}

/// Columns of the EQ page that ←→ moves between.
const EQ_FIELDS: usize = 4;

/// One line of recorder status, filled in by the main loop.
#[derive(Debug, Default, Clone)]
pub struct RecLine {
    pub on: bool,
    pub text: String,
}

pub struct View {
    pub sel: usize,
    meters: [[f32; 2]; ROWS],
    /// Input levels (what comes in, also while a row is muted).
    in_meters: [[f32; 2]; ROWS],
    pub status: String,
    pub rec: RecLine,
    /// Terminal width (the main loop updates it every frame).
    pub cols: usize,
    /// Latency advice from the startup check (its own line, under the status).
    pub latency: Option<String>,
    pub page: Page,
    /// EQ page: selected band and column (type, freq, gain, Q).
    pub band: usize,
    pub field: usize,
}

impl Default for View {
    fn default() -> Self {
        Self {
            sel: 0,
            meters: [[FLOOR_DB; 2]; ROWS],
            in_meters: [[FLOOR_DB; 2]; ROWS],
            status: String::new(),
            rec: RecLine::default(),
            cols: MIN_COLS,
            latency: None,
            page: Page::Mixer,
            band: 0,
            field: 1,
        }
    }
}

/// One step of an EQ value: `dir` = -1 / +1, `coarse` = bigger steps.
pub fn eq_step(b: Band, field: usize, dir: i32, coarse: bool) -> Band {
    let mut b = b;
    match field {
        0 => {
            let i = Kind::ALL.iter().position(|k| *k == b.kind).unwrap_or(0) as i32;
            b.kind = Kind::ALL[(i + dir).rem_euclid(Kind::ALL.len() as i32) as usize];
        }
        1 => {
            let f: f32 = if coarse { 2f32.powf(1.0 / 3.0) } else { 2f32.powf(1.0 / 12.0) };
            b.freq = eq::round_freq((b.freq * f.powi(dir)).clamp(eq::MIN_FREQ, eq::MAX_FREQ));
        }
        2 if b.kind.has_gain() => {
            let d = if coarse { 3.0 } else { 0.5 };
            b.gain_db = eq::round_gain((b.gain_db + d * dir as f32).clamp(eq::MIN_GAIN_DB, eq::MAX_GAIN_DB));
        }
        3 => {
            let f: f32 = if coarse { 1.5 } else { 2f32.powf(1.0 / 6.0) };
            b.q = eq::round_q((b.q * f.powi(dir)).clamp(eq::MIN_Q, eq::MAX_Q));
        }
        _ => {}
    }
    b
}

/// `100 Hz`, `1.50 kHz`, `12.0 kHz`.
pub fn fmt_freq(f: f32) -> String {
    if f < 1000.0 {
        format!("{f:.0} Hz")
    } else if f < 10_000.0 {
        format!("{:.2} kHz", f / 1000.0)
    } else {
        format!("{:.1} kHz", f / 1000.0)
    }
}

/// Width of the EQ curve on the EQ page, in characters.
const CURVE_W: usize = 64;
/// dB per Braille dot row. 0 dB sits exactly on dot row 6 of 12, so a flat
/// curve with a tiny ripple stays one straight line.
const CURVE_DB_PER_DOT: f64 = 3.0;

/// The EQ's response drawn with Braille dots: 3 rows of `CURVE_W` cells,
/// 20 Hz to 20 kHz across, +18 dB to -15 dB down (lower values sit on the
/// bottom row).
fn curve(on: bool, bands: &[Band; BANDS], sample_rate: u32) -> [String; 3] {
    let sr = sample_rate.max(8000) as f64;
    let (lo, hi) = (20f64.ln(), 20_000f64.ln());
    let cols = 2 * CURVE_W;
    let mut cells = [[0u8; CURVE_W]; 3];
    for x in 0..cols {
        let f = (lo + (hi - lo) * x as f64 / (cols - 1) as f64).exp();
        let db: f64 = if on { bands.iter().filter(|b| b.on).map(|b| eq::band_db(b, sr, f.min(0.49 * sr))).sum() } else { 0.0 };
        let row = (6.0 - db / CURVE_DB_PER_DOT).round().clamp(0.0, 11.0) as usize;
        let (cell_row, dot_row) = (row / 4, row % 4);
        // Braille dot bits: left column rows 0-3 = 0x01 0x02 0x04 0x40, right = 0x08 0x10 0x20 0x80
        let bit = if x % 2 == 0 { [0x01, 0x02, 0x04, 0x40][dot_row] } else { [0x08, 0x10, 0x20, 0x80][dot_row] };
        cells[cell_row][x / 2] |= bit;
    }
    cells.map(|r| r.iter().map(|&b| char::from_u32(0x2800 + b as u32).unwrap_or(' ')).collect())
}

/// `20  100  1k  10k  20k` under the curve.
fn curve_axis() -> String {
    let mut line = vec![' '; CURVE_W + 2];
    let (lo, hi) = (20f64.ln(), 20_000f64.ln());
    for (f, label) in [(20.0f64, "20"), (100.0, "100"), (1000.0, "1k"), (10_000.0, "10k"), (20_000.0, "20k")] {
        let x = ((f.ln() - lo) / (hi - lo) * (CURVE_W - 1) as f64).round() as usize;
        let start = x.min(line.len() - label.len());
        for (i, c) in label.chars().enumerate() {
            line[start + i] = c;
        }
    }
    line.into_iter().collect()
}

/// Cut `s` to `max` visible characters (ANSI colour codes don't count),
/// ending in `…` if anything was cut.
fn fit(s: &str, max: usize) -> String {
    if visible_len(s) <= max {
        return s.to_string();
    }
    let mut o = String::with_capacity(s.len());
    let mut n = 0;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            o.push(c);
            for d in it.by_ref() {
                o.push(d);
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if n + 1 >= max {
            o.push('…');
            o.push_str("\x1b[0m");
            break;
        } else {
            o.push(c);
            n += 1;
        }
    }
    o
}

/// Plain text cut to `max` characters, ending in `…` if anything was cut.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(['…']).collect()
}

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            n += 1;
        }
    }
    n
}

pub struct Info<'a> {
    pub client: &'a str,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub cpu: f32,
    pub xruns: u32,
    pub oversize: u32,
    pub lat_in: u32,
    pub lat_out: u32,
    pub cables: usize,
    /// VBAN summary for the line under the header ("" = VBAN is off).
    pub vban: &'a str,
}

fn step_gain(cur: f32, delta: f32) -> f32 {
    (((cur + delta) * 10.0).round() / 10.0).clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

impl View {
    pub fn handle(&mut self, sh: &Shared, key: Key) -> Action {
        match self.page {
            Page::Mixer => self.handle_mixer(sh, key),
            Page::Eq => self.handle_eq(sh, key),
        }
    }

    fn handle_eq(&mut self, sh: &Shared, key: Key) -> Action {
        let p = sh.eq(self.sel);
        let band = &p.bands[self.band];
        let step = |dir: i32, coarse: bool| band.store(&eq_step(band.load(), self.field, dir, coarse));
        match key {
            Key::Up | Key::Char(b'k') => self.band = self.band.saturating_sub(1),
            Key::Down | Key::Char(b'j') | Key::Char(b'\t') => self.band = (self.band + 1).min(BANDS - 1),
            Key::Left | Key::Char(b'h') => self.field = self.field.saturating_sub(1),
            Key::Right | Key::Char(b'l') => self.field = (self.field + 1).min(EQ_FIELDS - 1),
            Key::Char(b',') | Key::Char(b'-') => step(-1, false),
            Key::Char(b'.') | Key::Char(b'+') | Key::Char(b'=') => step(1, false),
            Key::Char(b'<') => step(-1, true),
            Key::Char(b'>') => step(1, true),
            Key::Char(b' ') => {
                band.on.fetch_xor(true, Relaxed);
            }
            Key::Char(b'0') => band.store(&DEFAULT_BANDS[self.band]),
            Key::Char(b'e') => {
                p.on.fetch_xor(true, Relaxed);
            }
            Key::Char(b'[') => self.sel = (self.sel + ROWS - 1) % ROWS,
            Key::Char(b']') => self.sel = (self.sel + 1) % ROWS,
            Key::Char(b'E') | Key::Char(b'\r') | Key::Char(b'\n') => self.page = Page::Mixer,
            Key::Char(b'R') => return Action::Record,
            Key::Char(b'r') => return Action::Reconnect,
            Key::Char(b'w') => return Action::Save,
            Key::Char(b'v') => return Action::ToggleVban,
            Key::Char(b'q') | Key::Char(3) | Key::Char(4) => return Action::Quit,
            _ => {}
        }
        Action::None
    }

    fn handle_mixer(&mut self, sh: &Shared, key: Key) -> Action {
        let is_strip = self.sel < NUM_STRIPS;
        let gain = |v: &Self| {
            if v.sel < NUM_STRIPS { &sh.strips[v.sel].gain_db } else { &sh.buses[v.sel - NUM_STRIPS].gain_db }
        };
        let toggle = |a: &std::sync::atomic::AtomicBool| {
            a.fetch_xor(true, Relaxed);
        };
        match key {
            Key::Up | Key::Char(b'k') => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Char(b'j') | Key::Char(b'\t') => self.sel = (self.sel + 1).min(ROWS - 1),
            Key::Left | Key::Char(b'h') => gain(self).store(step_gain(gain(self).load(), -1.0)),
            Key::Right | Key::Char(b'l') => gain(self).store(step_gain(gain(self).load(), 1.0)),
            Key::Char(b',') => gain(self).store(step_gain(gain(self).load(), -0.1)),
            Key::Char(b'.') => gain(self).store(step_gain(gain(self).load(), 0.1)),
            Key::Char(b'0') => gain(self).store(0.0),
            Key::Char(b'm') => {
                if is_strip { toggle(&sh.strips[self.sel].mute) } else { toggle(&sh.buses[self.sel - NUM_STRIPS].mute) }
            }
            Key::Char(b'o') => {
                if is_strip { toggle(&sh.strips[self.sel].mono) } else { toggle(&sh.buses[self.sel - NUM_STRIPS].mono) }
            }
            Key::Char(b's') if is_strip => toggle(&sh.strips[self.sel].solo),
            Key::Char(c @ b'1'..=b'5') if is_strip => sh.toggle_route(self.sel, (c - b'1') as usize),
            // a bus feeds another bus (never itself, never in a loop)
            Key::Char(c @ b'1'..=b'5') => {
                let (from, to) = (self.sel - NUM_STRIPS, (c - b'1') as usize);
                let r = &sh.buses[from].routes;
                if r.load(Relaxed) & (1 << to) != 0 {
                    r.fetch_and(!(1u8 << to), Relaxed);
                } else if from == to {
                    return Action::Note(format!("{} cannot feed itself", BUS_IDS[from]));
                } else if sh.send_loops(from, to) {
                    return Action::Note(format!("{} already reaches {}: that would be a loop", BUS_IDS[to], BUS_IDS[from]));
                } else {
                    r.fetch_or(1 << to, Relaxed);
                }
            }
            // rows are numbered like the recorder sources: strips 1-6, then A1..B2
            Key::Char(b'c') => sh.toggle_rec_link(self.sel),
            Key::Char(b'e') => {
                sh.eq(self.sel).on.fetch_xor(true, Relaxed);
            }
            Key::Char(b'E') => self.page = Page::Eq,
            Key::Char(b'v') => return Action::ToggleVban,
            Key::Char(b'R') => return Action::Record,
            Key::Char(b'r') => return Action::Reconnect,
            Key::Char(b'w') => return Action::Save,
            Key::Char(b'q') | Key::Char(3) | Key::Char(4) => return Action::Quit, // q, Ctrl-C, Ctrl-D
            _ => {}
        }
        Action::None
    }

    /// Current meter levels in dB (`[row][L/R]`, floor -60), after ballistics.
    pub fn meters(&self) -> &[[f32; 2]; ROWS] {
        &self.meters
    }

    /// Input levels in dB, like [`View::meters`]: what comes into each row
    /// (after mono and input gain), also while it is muted.
    pub fn in_meters(&self) -> &[[f32; 2]; ROWS] {
        &self.in_meters
    }

    /// Pull peaks from the audio thread and apply meter ballistics.
    pub fn update_meters(&mut self, sh: &Shared) {
        let fall = |cur: f32, pk: f32| gain_to_db(pk).max(FLOOR_DB).max(cur - DECAY_DB_PER_FRAME).max(FLOOR_DB);
        for r in 0..ROWS {
            for ch in 0..2 {
                let (pk, inp) = if r < NUM_STRIPS {
                    (sh.strips[r].peak[ch].take(), sh.strips[r].input_peak[ch].take())
                } else {
                    (sh.buses[r - NUM_STRIPS].peak[ch].take(), sh.buses[r - NUM_STRIPS].input_peak[ch].take())
                };
                self.meters[r][ch] = fall(self.meters[r][ch], pk);
                self.in_meters[r][ch] = fall(self.in_meters[r][ch], inp);
            }
        }
    }

    /// Width of the NAME column for the current terminal width.
    fn name_w(&self) -> usize {
        NAME_W + self.cols.saturating_sub(MIN_COLS).min(MAX_NAME_W - NAME_W)
    }

    pub fn render(&self, sh: &Shared, cfg: &Config, info: &Info, conn: &[String]) -> String {
        let mut o = String::with_capacity(8192);
        o.push_str("\x1b[H");
        let line_end = "\x1b[0m\x1b[K\n";
        let cols = self.cols.max(MIN_COLS);
        let nw = self.name_w();
        // sel+id+link, name, gain, two meters, peak, flags, routes
        let row_w = 4 + 1 + nw + 1 + 6 + 2 + METER_W + 1 + METER_W + 6 + 7 + 2 + 14; // 79 at 80 cols

        let period_ms = if info.sample_rate > 0 { info.buffer_size as f64 * 1000.0 / info.sample_rate as f64 } else { 0.0 };
        let (rt_ms, measured) = if info.sample_rate > 0 && (info.lat_in > 0 || info.lat_out > 0) {
            ((info.lat_in + info.lat_out) as f64 * 1000.0 / info.sample_rate as f64, true)
        } else {
            (2.0 * period_ms, false)
        };
        let ok = rt_ms < 10.0;
        let _ = write!(o, "\x1b[1m BananaSplit\x1b[0m  6 strips · 5 buses · JACK client \"{}\" · {} cables{line_end}", info.client, info.cables);
        let _ = write!(
            o,
            " {} Hz · {} fr · {:.2} ms/period · RTT {}{:.2} ms {} · DSP {:.0}% · xruns {}{}{line_end}",
            info.sample_rate,
            info.buffer_size,
            period_ms,
            if measured { "" } else { "~" },
            rt_ms,
            if ok { "\x1b[32m[OK <10ms]\x1b[0m" } else { "\x1b[31;1m[>=10ms!]\x1b[0m" },
            info.cpu,
            info.xruns,
            if info.oversize > 0 { format!(" \x1b[31moversize {}\x1b[0m", info.oversize) } else { String::new() },
        );
        // VBAN (in place of a blank line)
        let _ = write!(o, " {}{line_end}", fit(info.vban, cols - 2));
        match self.page {
            Page::Mixer => self.render_mixer(&mut o, sh, cfg, nw, row_w),
            Page::Eq => self.render_eq(&mut o, sh, cfg, info, cols),
        }

        let _ = write!(o, "{line_end}");
        let sel_name = if self.sel < NUM_STRIPS { &cfg.strips[self.sel].name } else { &cfg.buses[self.sel - NUM_STRIPS].name };
        let conn_text = conn.get(self.sel).map_or("", String::as_str);
        let arrow = if self.sel < NUM_STRIPS { "←" } else { "→" };
        let sel_line = fit(&format!("\x1b[1m{sel_name}\x1b[0m {arrow} {conn_text}"), cols - 2);
        let _ = write!(o, " {sel_line}{line_end}");
        // Recorder line (in place of a blank line, so the screen still fits 24 rows).
        let rec_text = fit(&self.rec.text, cols - 9);
        if self.rec.on {
            let _ = write!(o, " \x1b[41;97m ● REC \x1b[0m {rec_text}{line_end}");
        } else {
            let _ = write!(o, " \x1b[2m ○ rec \x1b[0m \x1b[2m{rec_text}{line_end}");
        }
        match self.page {
            Page::Mixer => {
                let _ = write!(o, "\x1b[2m ↑↓ select  ←→ gain ±1 dB  , . ±0.1  0 = 0 dB  m mute  s solo  o mono  e/E EQ{line_end}");
                let _ = write!(o, "\x1b[2m 1-5 route to A1-B2  c rec link  R record  v VBAN  r reconnect  w save  q quit{line_end}");
            }
            Page::Eq => {
                let _ = write!(o, "\x1b[2m ↑↓ band  ←→ field  , . change  < > faster  space band on/off  0 flat band{line_end}");
                let _ = write!(o, "\x1b[2m e EQ on/off  [ ] other channel  E or Enter: mixer  R record  w save  q quit{line_end}");
            }
        }
        let _ = write!(o, " {}{line_end}", fit(&self.status, cols - 2));
        // Row 23 of 24 (the last row stays empty so the screen never scrolls).
        match &self.latency {
            Some(m) => {
                let _ = write!(o, " \x1b[33m{}{line_end}", fit(m, cols - 2));
            }
            None if !ok => {
                let _ = write!(o, " \x1b[31mround trip >= 10 ms: run `bananasplit --low-latency` once (README: Latency){line_end}");
            }
            None => {}
        }
        o.push_str("\x1b[J");
        o
    }

    /// The 13 lines of the mixer table (header, 6 strips, separator, 5 buses).
    fn render_mixer(&self, o: &mut String, sh: &Shared, cfg: &Config, nw: usize, row_w: usize) {
        let line_end = "\x1b[0m\x1b[K\n";
        let _ = write!(
            o,
            "\x1b[2m  #  {:<nw$} {:>6}  {:<mw$} {:<mw$} PEAK M S O E  A1 A2 A3 B1 B2{line_end}",
            "NAME",
            "GAIN",
            "L",
            "R",
            mw = METER_W
        );

        let any_solo = sh.strips.iter().any(|s| s.solo.load(Relaxed));
        let links = sh.rec_links.load(Relaxed);
        for r in 0..ROWS {
            if r == NUM_STRIPS {
                let _ = write!(o, "\x1b[2m  ── buses {}{line_end}", "─".repeat(row_w - 11));
            }
            let selected = r == self.sel;
            let eq_on = sh.eq(r).on.load(Relaxed);
            let (id, name, gain, mute, solo, mono, routes) = if r < NUM_STRIPS {
                let p = &sh.strips[r];
                (
                    format!("{}", r + 1),
                    cfg.strips[r].name.as_str(),
                    p.gain_db.load(),
                    p.mute.load(Relaxed),
                    Some(p.solo.load(Relaxed)),
                    p.mono.load(Relaxed),
                    p.routes.load(Relaxed),
                )
            } else {
                let b = r - NUM_STRIPS;
                let p = &sh.buses[b];
                let sends = p.routes.load(Relaxed) & !(1 << b);
                (BUS_IDS[b].to_string(), cfg.buses[b].name.as_str(), p.gain_db.load(), p.mute.load(Relaxed), None, p.mono.load(Relaxed), sends)
            };
            let dimmed = r < NUM_STRIPS && any_solo && solo == Some(false);
            let name = ellipsize(name, nw);
            let dim = if dimmed { "\x1b[2m" } else { "" };
            let link = if links & (1 << r) != 0 { format!("\x1b[31m●\x1b[0m{dim}") } else { " ".into() };
            let _ = write!(
                o,
                "{}{}{:>2}{} {:<nw$} {:>+6.1}  ",
                if selected { "\x1b[7m>\x1b[0m" } else { " " },
                dim,
                id,
                link,
                name,
                gain
            );
            // A silenced row (muted, or soloed out) shows what comes in, grey.
            let silenced = mute || dimmed;
            let m = if silenced { &self.in_meters[r] } else { &self.meters[r] };
            meter(o, m[0], silenced);
            o.push(' ');
            meter(o, m[1], silenced);
            let pk = self.meters[r][0].max(self.meters[r][1]);
            // always 6 columns, so the flags line up on silent rows too
            if pk <= FLOOR_DB {
                let _ = write!(o, "   -∞ ");
            } else {
                let _ = write!(o, " {:>4.0} ", pk);
            }
            flag(o, mute, 'M', "41;97");
            o.push(' ');
            match solo {
                Some(v) => flag(o, v, 'S', "43;30"),
                None => o.push(' '),
            }
            o.push(' ');
            flag(o, mono, 'O', "46;30");
            o.push(' ');
            flag(o, eq_on, 'E', "45;97");
            o.push_str("  ");
            for (b, idb) in BUS_IDS.iter().enumerate() {
                let sep = if b > 0 { " " } else { "" };
                if r == NUM_STRIPS + b {
                    let _ = write!(o, "{sep}\x1b[2m──\x1b[0m"); // a bus never feeds itself
                } else if routes & (1 << b) != 0 {
                    let _ = write!(o, "{sep}\x1b[42;30m{idb}\x1b[0m");
                } else {
                    let _ = write!(o, "{sep}\x1b[2m{idb}\x1b[0m");
                }
            }
            o.push_str(line_end);
        }
    }

    /// The EQ page, in the 13 lines the mixer table uses.
    fn render_eq(&self, o: &mut String, sh: &Shared, cfg: &Config, info: &Info, cols: usize) {
        let line_end = "\x1b[0m\x1b[K\n";
        let ch = self.sel;
        let p = sh.eq(ch);
        let on = p.on.load(Relaxed);
        let bands = p.bands();
        let name = if ch < NUM_STRIPS { &cfg.strips[ch].name } else { &cfg.buses[ch - NUM_STRIPS].name };
        let state = if on { "\x1b[45;97m ON \x1b[0m" } else { "\x1b[2m OFF \x1b[0m" };
        let head = fit(&format!("\x1b[1mEQ  {} {}\x1b[0m  {state}", source_id(ch), ellipsize(name, 32)), cols - 2);
        let _ = write!(o, "  {head}{line_end}");
        let _ = write!(o, "\x1b[2m   BAND  ON  TYPE         FREQ         GAIN       Q{line_end}");
        for (k, b) in bands.iter().enumerate() {
            let sel = k == self.band;
            let mark = if sel { "\x1b[7m>\x1b[0m" } else { " " };
            let dim = if b.on && on { "" } else { "\x1b[2m" };
            let cell = |i: usize, text: String| -> String {
                if sel && i == self.field { format!("\x1b[7m{text}\x1b[0m{dim}") } else { text }
            };
            let gain = if b.kind.has_gain() { format!("{:+5.1} dB", b.gain_db) } else { format!("{:>8}", "—") };
            let _ = write!(
                o,
                "  {mark}{dim}  {}    {}   {}  {}   {}  {}{line_end}",
                k + 1,
                if b.on { "●" } else { "○" },
                cell(0, format!("{:<10}", b.kind.label())),
                cell(1, format!("{:>9}", fmt_freq(b.freq))),
                cell(2, gain),
                cell(3, format!("{:>5.2}", b.q)),
            );
        }
        let _ = write!(o, "{line_end}");
        let rows = curve(on, &bands, info.sample_rate);
        for (i, r) in rows.iter().enumerate() {
            let label = ["+18", "  0", "-15"][i];
            let _ = write!(o, "\x1b[2m {label} \x1b[0m\x1b[36m{r}{line_end}");
        }
        let _ = write!(o, "\x1b[2m     {}{line_end}", curve_axis());
    }
}

fn flag(o: &mut String, on: bool, c: char, color: &str) {
    if on {
        let _ = write!(o, "\x1b[{color}m{c}\x1b[0m");
    } else {
        o.push_str("\x1b[2m·\x1b[0m");
    }
}

/// A level meter; `grey`: a silenced row's input level.
fn meter(o: &mut String, db: f32, grey: bool) {
    let lit = (((db - FLOOR_DB) / -FLOOR_DB) * METER_W as f32).round().clamp(0.0, METER_W as f32) as usize;
    for i in 0..METER_W {
        // cell i covers up to this level
        let cell_db = FLOOR_DB + (i + 1) as f32 * (-FLOOR_DB / METER_W as f32);
        if i < lit {
            let col = if grey { "90" } else if cell_db > -3.0 { "31" } else if cell_db > -12.0 { "33" } else { "32" };
            let _ = write!(o, "\x1b[{col}m█");
        } else {
            o.push_str("\x1b[2m·");
        }
    }
    o.push_str("\x1b[0m");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut o = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                o.push(c);
            }
        }
        o
    }

    #[test]
    fn decode_keys_and_partial_sequences() {
        let mut b = b"q\x1b[A\x1b[1;2B\x1bOCx\x1b[".to_vec();
        let k = decode(&mut b);
        assert_eq!(k, vec![Key::Char(b'q'), Key::Up, Key::Down, Key::Right, Key::Char(b'x')]);
        assert_eq!(b, b"\x1b[");
        b.push(b'D');
        assert_eq!(decode(&mut b), vec![Key::Left]);
        assert!(b.is_empty());
    }

    #[test]
    fn handle_controls_shared_state() {
        let sh = Shared::default();
        let mut v = View::default();
        v.handle(&sh, Key::Right);
        v.handle(&sh, Key::Char(b','));
        assert!((sh.strips[0].gain_db.load() - 0.9).abs() < 1e-6);
        for _ in 0..100 {
            v.handle(&sh, Key::Right);
        }
        assert_eq!(sh.strips[0].gain_db.load(), MAX_GAIN_DB);
        v.handle(&sh, Key::Char(b'4'));
        assert_eq!(sh.strips[0].routes.load(Relaxed), 0b01000);
        v.handle(&sh, Key::Char(b'm'));
        assert!(sh.strips[0].mute.load(Relaxed));
        for _ in 0..50 {
            v.handle(&sh, Key::Down);
        }
        assert_eq!(v.sel, ROWS - 1);
        v.handle(&sh, Key::Char(b's')); // no solo on buses
        // a bus feeds other buses with 1-5, never itself, never in a loop
        assert!(matches!(v.handle(&sh, Key::Char(b'1')), Action::None));
        assert_eq!(sh.buses[NUM_BUSES - 1].routes.load(Relaxed), 0b00001, "B2 -> A1");
        assert!(matches!(v.handle(&sh, Key::Char(b'5')), Action::Note(n) if n.contains("itself")));
        v.sel = NUM_STRIPS; // A1
        assert!(matches!(v.handle(&sh, Key::Char(b'5')), Action::Note(n) if n.contains("loop")));
        assert_eq!(sh.buses[0].routes.load(Relaxed), 0, "A1 -> B2 would loop");
        v.sel = ROWS - 1;
        v.handle(&sh, Key::Char(b'1'));
        assert_eq!(sh.buses[NUM_BUSES - 1].routes.load(Relaxed), 0, "1 again stops it");
        v.handle(&sh, Key::Char(b'm'));
        assert!(sh.buses[NUM_BUSES - 1].mute.load(Relaxed));
        // recorder: c links the selected row (B2 = source 10), R asks to record
        v.handle(&sh, Key::Char(b'c'));
        assert_eq!(sh.rec_links.load(Relaxed), 1 << 10);
        v.sel = 0;
        v.handle(&sh, Key::Char(b'c'));
        v.handle(&sh, Key::Char(b'c'));
        v.handle(&sh, Key::Char(b'c'));
        assert_eq!(sh.rec_links.load(Relaxed), (1 << 10) | 1);
        assert!(matches!(v.handle(&sh, Key::Char(b'R')), Action::Record));
        assert!(matches!(v.handle(&sh, Key::Char(b'r')), Action::Reconnect));
        assert!(matches!(v.handle(&sh, Key::Char(b'q')), Action::Quit));
    }

    #[test]
    fn render_does_not_panic() {
        let sh = Shared::default();
        let cfg = Config::default();
        let mut v = View::default();
        sh.strips[0].peak[0].take();
        v.update_meters(&sh);
        let info = Info { client: "BananaSplit", sample_rate: 48000, buffer_size: 128, cpu: 1.0, xruns: 0, oversize: 0, lat_in: 128, lat_out: 256, cables: 8, vban: "" };
        let s = v.render(&sh, &cfg, &info, &[]);
        assert!(s.contains("RTT 8.00 ms"));
        // every line fits an 80-column terminal
        let plain = strip_ansi(&s);
        for line in plain.lines() {
            assert!(line.chars().count() <= 80, "{} cols: {line}", line.chars().count());
        }
        assert!(s.contains("Mic"));
        // ...also while recording with a very long file name and linked rows,
        // and the screen still fits a 24-row terminal
        sh.rec_links.store(0b111_1111_1111, Relaxed);
        sh.strips[2].solo.store(true, Relaxed);
        v.rec = RecLine { on: true, text: format!("00:00:01  multi  {}", "x".repeat(200)) };
        let s = v.render(&sh, &cfg, &info, &[]);
        let plain = strip_ansi(&s);
        for line in plain.lines() {
            assert!(line.chars().count() <= 80, "{} cols: {line}", line.chars().count());
        }
        assert!(plain.lines().count() < 24, "{} rows", plain.lines().count());
        assert!(plain.contains("● REC"));
        assert!(plain.contains(" 1● Mic"));
        assert!(plain.contains("A1● Speakers"));
        assert!(plain.contains('…'));
    }

    #[test]
    fn muted_rows_show_their_input_grey_and_buses_their_sends() {
        let sh = Shared::default();
        let cfg = Config::default();
        let mut v = View::default();
        sh.strips[1].mute.store(true, Relaxed);
        sh.strips[1].input_peak[0].push_for_test(0.5);
        sh.strips[1].peak[0].push_for_test(0.0);
        sh.strips[2].peak[0].push_for_test(0.5);
        sh.buses[3].routes.store(0b00001, Relaxed); // B1 -> A1
        v.update_meters(&sh);
        assert!(v.in_meters()[1][0] > -7.0 && v.meters()[1][0] <= FLOOR_DB);
        let info = Info { client: "BananaSplit", sample_rate: 48000, buffer_size: 128, cpu: 1.0, xruns: 0, oversize: 0, lat_in: 128, lat_out: 256, cables: 8, vban: "" };
        let s = v.render(&sh, &cfg, &info, &[]);
        let row = |id: &str| s.lines().find(|l| strip_ansi(l).starts_with(id)).unwrap().to_string();
        assert!(row("  2").contains("\x1b[90m█"), "strip 2 is muted: its input, grey");
        assert!(row("  3").contains("\x1b[32m█") && !row("  3").contains("\x1b[90m"), "strip 3 in colour");
        let b1 = strip_ansi(&row(" B1"));
        assert!(b1.ends_with("A1 A2 A3 ── B2"), "{b1}");
        assert!(row(" B1").contains("\x1b[42;30mA1"), "B1 feeds A1");
        for line in strip_ansi(&s).lines() {
            assert!(line.chars().count() <= 80, "{} cols: {line}", line.chars().count());
        }
    }

    fn fits(s: &str, cols: usize) -> Vec<String> {
        let plain = strip_ansi(s);
        for line in plain.lines() {
            assert!(line.chars().count() < cols, "{} cols (limit {}): {line}", line.chars().count(), cols - 1);
        }
        plain.lines().map(str::to_string).collect()
    }

    #[test]
    fn long_names_at_80_and_wider() {
        let sh = Shared::default();
        let mut cfg = Config::default();
        cfg.buses[1].name = "Blue Headphones".into(); // the review: "Blue Headph"
        cfg.strips[3].name = "Game capture (Elgato HD60 S+)".into(); // 29 chars
        let info = Info { client: "BananaSplit", sample_rate: 48000, buffer_size: 128, cpu: 1.0, xruns: 3, oversize: 0, lat_in: 416, lat_out: 416, cables: 8, vban: "" };
        cfg.apply(&sh);
        let mut v = View {
            status: format!("\x1b[33mstrip 2: nothing matches \"^alsa_input.usb-Blue\" — {}\x1b[0m", "x".repeat(100)),
            sel: NUM_STRIPS + 1,
            ..View::default()
        };
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 80);
        // RTT >= 10 ms gets its own line even while the status line is busy
        assert!(lines.iter().any(|l| l.contains("run `bananasplit --low-latency` once")), "{lines:#?}");
        v.latency = Some(format!("round trip 17.3 ms at 128 frames (goal < 10 ms): run `bananasplit --low-latency` {}", "x".repeat(20)));
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 80);
        assert!(lines.len() < 24, "{} rows", lines.len());
        assert!(lines.last().unwrap().starts_with(" round trip 17.3 ms") && lines.last().unwrap().ends_with('…'));
        assert!(lines.iter().any(|l| l.starts_with(">A2  Blue Headphones  ")), "{lines:#?}");
        assert!(lines.iter().any(|l| l == " Blue Headphones → "), "full name of the selected row");
        assert!(lines.iter().any(|l| l.starts_with("  4  Game capture (…  ")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.ends_with('…')), "long status is cut, not wrapped");
        assert!(lines.iter().any(|l| l.contains("RTT 17.33 ms [>=10ms!]")));
        // header columns line up with the rows (strip 1 is mono: "· · O")
        let hdr = lines.iter().find(|l| l.contains("NAME")).unwrap();
        let row = lines.iter().find(|l| l.starts_with("  1")).unwrap();
        let at = |s: &str, needle: &str| s.find(needle).map(|b| s[..b].chars().count()).unwrap();
        assert_eq!(at(hdr, "A1 A2"), at(row, "A1 A2"));
        assert_eq!(at(hdr, "M S O"), at(row, "· · O"));
        assert_eq!(at(hdr, "GAIN"), at(row, "+0.0"), "GAIN ends where the value ends");
        assert_eq!(at(hdr, "PEAK"), at(row, "-∞") - 2);
        // silent and loud rows have the same layout (PEAK used to be 7 or 8 wide)
        let mut v2 = View { sel: ROWS - 1, ..View::default() };
        v2.meters[1] = [-20.0, -24.0];
        let lines = fits(&v2.render(&sh, &cfg, &info, &[]), 80);
        let s1 = lines.iter().find(|l| l.starts_with("  1")).unwrap();
        let s2 = lines.iter().find(|l| l.starts_with("  2")).unwrap();
        assert_eq!(s1.chars().count(), s2.chars().count(), "\n{s1}\n{s2}");
        assert!(s2.contains("  -20 ·") && s1.contains("   -∞ ·"), "\n{s1}\n{s2}");
        // a wider terminal shows more of the name
        v.cols = 100;
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 100);
        assert!(lines.iter().any(|l| l.starts_with("  4  Game capture (Elgato HD60 S+) ")), "{lines:#?}");
        v.cols = 300; // capped at 32-char names
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 300);
        assert!(lines.iter().filter(|l| l.contains("NAME")).all(|l| l.chars().count() < 100));
    }

    #[test]
    fn eq_keys_and_page() {
        let sh = Shared::default();
        let mut cfg = Config::default();
        cfg.buses[1].name = "Blue Headphones".into();
        let mut v = View { sel: NUM_STRIPS + 1, ..View::default() }; // A2
        assert!(matches!(v.handle(&sh, Key::Char(b'e')), Action::None));
        assert!(sh.buses[1].eq.on.load(Relaxed), "e switches the row's EQ on");
        v.handle(&sh, Key::Char(b'E'));
        assert_eq!(v.page, Page::Eq);
        // band 1, frequency column: one step is a semitone
        v.handle(&sh, Key::Char(b'.'));
        assert_eq!(sh.buses[1].eq.bands[0].freq.load(), 106.0);
        v.handle(&sh, Key::Right); // gain
        v.handle(&sh, Key::Char(b'>'));
        v.handle(&sh, Key::Char(b'.'));
        assert_eq!(sh.buses[1].eq.bands[0].gain_db.load(), 3.5);
        v.handle(&sh, Key::Right); // Q
        v.handle(&sh, Key::Char(b'<'));
        assert_eq!(sh.buses[1].eq.bands[0].q.load(), 0.47);
        v.handle(&sh, Key::Right); // stays on the last column
        assert_eq!(v.field, 3);
        // band 2: type (bell -> low shelf), then off
        v.handle(&sh, Key::Down);
        for _ in 0..5 {
            v.handle(&sh, Key::Left);
        }
        v.handle(&sh, Key::Char(b'.'));
        v.handle(&sh, Key::Char(b' '));
        let b2 = sh.buses[1].eq.bands[1].load();
        assert_eq!((b2.kind, b2.on), (Kind::LowShelf, false));
        let info = Info { client: "BananaSplit", sample_rate: 48000, buffer_size: 128, cpu: 1.0, xruns: 0, oversize: 0, lat_in: 128, lat_out: 128, cables: 8, vban: "" };
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 80);
        assert!(lines.len() < 24, "{} rows", lines.len());
        assert!(lines.iter().any(|l| l.contains("EQ  A2 Blue Headphones") && l.contains(" ON ")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("1    ●   low shelf      106 Hz    +3.5 dB   0.47")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("2    ○   low shelf      250 Hz")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("10.0 kHz")));
        let dots = |l: &str| l.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c));
        assert!(lines.iter().any(|l| l.starts_with(" +18 ")) && lines.iter().any(|l| l.starts_with("   0 ") && dots(l)), "a curve");
        assert!(lines.iter().any(|l| l.contains("20") && l.contains("1k") && l.contains("20k")));
        assert!(lines.iter().any(|l| l.contains("E or Enter: mixer")));
        // a cut has no gain to change
        sh.buses[1].eq.bands[2].kind.store(Kind::LowCut as u8, Relaxed);
        let before = sh.buses[1].eq.bands[2].load();
        v.band = 2;
        v.field = 2;
        v.handle(&sh, Key::Char(b'>'));
        assert_eq!(sh.buses[1].eq.bands[2].load(), before);
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 80);
        assert!(lines.iter().any(|l| l.contains("low cut") && l.contains("—")));
        // 0 resets the band, [ ] change channel, e works here too
        v.handle(&sh, Key::Char(b'0'));
        assert_eq!(sh.buses[1].eq.bands[2].load(), DEFAULT_BANDS[2]);
        v.handle(&sh, Key::Char(b']'));
        assert_eq!(v.sel, NUM_STRIPS + 2);
        v.handle(&sh, Key::Char(b'['));
        v.handle(&sh, Key::Char(b'['));
        v.handle(&sh, Key::Char(b'['));
        assert_eq!(v.sel, NUM_STRIPS - 1);
        v.sel = ROWS - 1;
        v.handle(&sh, Key::Char(b']'));
        assert_eq!(v.sel, 0, "wraps around");
        v.handle(&sh, Key::Char(b'e'));
        assert!(sh.strips[0].eq.on.load(Relaxed));
        assert!(matches!(v.handle(&sh, Key::Char(b'w')), Action::Save));
        assert!(matches!(v.handle(&sh, Key::Char(b'v')), Action::ToggleVban));
        v.handle(&sh, Key::Char(b'\r'));
        assert_eq!(v.page, Page::Mixer);
        // back on the mixer: both EQ flags show
        let lines = fits(&v.render(&sh, &cfg, &info, &[]), 80);
        let row = |p: &str| lines.iter().find(|l| l.starts_with(p)).unwrap().clone();
        assert!(row("> 1").contains("   -∞ · · · E  A1"), "{}", row("> 1"));
        assert!(row(" A2").contains("·   · E"), "{}", row(" A2"));
        assert!(row("  2").contains("· · · ·"));
    }

    #[test]
    fn curve_has_a_steady_zero_line() {
        let flat = curve(true, &eq::DEFAULT_BANDS, 48000);
        let line0: String = std::iter::repeat_n('⠤', CURVE_W).collect();
        let empty: String = std::iter::repeat_n('⠀', CURVE_W).collect();
        assert_eq!(flat, [empty.clone(), line0.clone(), empty.clone()]);
        assert_eq!(curve(false, &eq::DEFAULT_BANDS, 48000), flat, "EQ off draws 0 dB");
        // a narrow bell leaves the rest of the curve (within ±0.1 dB) on the 0 dB line
        let mut b = eq::DEFAULT_BANDS;
        b[3] = Band { on: true, kind: Kind::Bell, freq: 1500.0, gain_db: 9.0, q: 3.38 };
        let c = curve(true, &b, 48000);
        let mid: Vec<char> = c[1].chars().collect();
        let top: Vec<char> = c[0].chars().collect();
        let x = |f: f64| ((f.ln() - 20f64.ln()) / (20_000f64.ln() - 20f64.ln()) * (2 * CURVE_W - 1) as f64).round() as usize / 2;
        for (i, ch) in mid.iter().enumerate() {
            if i + 6 < x(1500.0) || i > x(1500.0) + 6 {
                assert_eq!(*ch, '⠤', "column {i}");
            }
        }
        assert_ne!(top[x(1500.0)], '⠀', "+9 dB reaches the top row");
        // full scale: +18 on the top dot row, -15 and below on the bottom one
        b[3].gain_db = 24.0;
        b[3].q = 0.5;
        assert!(curve(true, &b, 48000)[0].chars().nth(x(1500.0)).is_some_and(|c| (c as u32 - 0x2800) & 0x09 != 0));
        b[3].gain_db = -24.0;
        assert!(curve(true, &b, 48000)[2].chars().nth(x(1500.0)).is_some_and(|c| (c as u32 - 0x2800) & 0xC0 != 0));
    }

    #[test]
    fn eq_steps_stay_in_range() {
        let b = Band { on: true, kind: Kind::Bell, freq: 19_500.0, gain_db: 23.0, q: 0.11 };
        assert_eq!(eq_step(b, 1, 1, true).freq, eq::MAX_FREQ);
        assert_eq!(eq_step(Band { freq: 21.0, ..b }, 1, -1, true).freq, eq::MIN_FREQ);
        assert_eq!(eq_step(b, 2, 1, true).gain_db, eq::MAX_GAIN_DB);
        assert_eq!(eq_step(b, 3, -1, false).q, eq::MIN_Q);
        assert_eq!(eq_step(Band { q: 29.0, ..b }, 3, 1, true).q, eq::MAX_Q);
        assert_eq!(eq_step(b, 0, -1, false).kind, Kind::Notch, "types wrap around");
        // every frequency step moves (no rounding trap at the bottom)
        let mut f = Band { freq: 20.0, ..b };
        for _ in 0..119 {
            let g = eq_step(f, 1, 1, false);
            assert!(g.freq > f.freq || g.freq == eq::MAX_FREQ, "{} -> {}", f.freq, g.freq);
            f = g;
        }
        assert_eq!(fmt_freq(99.6), "100 Hz");
        assert_eq!(fmt_freq(1500.0), "1.50 kHz");
        assert_eq!(fmt_freq(12_000.0), "12.0 kHz");
    }

    #[test]
    fn vban_line() {
        let sh = Shared::default();
        let cfg = Config::default();
        let v = View::default();
        let text = format!("\x1b[1mVBAN\x1b[0m UDP 6980 · in: 1 of 2 receiving · out: 1 of 1 sending · {}", "x".repeat(60));
        let info = Info { client: "BananaSplit", sample_rate: 48000, buffer_size: 128, cpu: 1.0, xruns: 0, oversize: 0, lat_in: 128, lat_out: 128, cables: 8, vban: &text };
        let lines = fits(&v.render(&sh, &cfg, &info, &["Yeti + VBAN in1 \"Stream1\" (48 kHz)".into()]), 80);
        assert!(lines[2].starts_with(" VBAN UDP 6980 · in: 1 of 2 receiving") && lines[2].ends_with('…'), "{}", lines[2]);
        assert!(lines.iter().any(|l| l == " Mic ← Yeti + VBAN in1 \"Stream1\" (48 kHz)"));
        assert!(lines.len() < 24);
    }

    #[test]
    fn fit_and_ellipsize() {
        assert_eq!(ellipsize("Blue Headphones", 15), "Blue Headphones");
        assert_eq!(ellipsize("Blue Headphones!", 15), "Blue Headphone…");
        assert_eq!(fit("\x1b[33mabcdef\x1b[0m", 6), "\x1b[33mabcdef\x1b[0m");
        let f = fit("\x1b[33mabcdefg\x1b[0m", 6);
        assert_eq!(strip_ansi(&f), "abcde…");
        assert!(f.starts_with("\x1b[33m"));
    }
}
