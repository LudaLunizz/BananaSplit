//! Persistent configuration (simple INI format, no dependencies).
//!
//! ```ini
//! [global]
//! client_name = BananaSplit
//! buffer_size = 128
//! force_buffer_size = false
//! create_cables = true
//!
//! [strip1]
//! name = Mic
//! gain_db = 0.0       # fader
//! trim_db = 0.0       # input gain (the GAIN knob), -24 .. +24
//! pan = 0.0           # -1 (left) .. 0 (centre) .. +1 (right)
//! mute = false
//! solo = false
//! mono = true
//! routes = B1
//! connect = @default
//!
//! [bus.A1]
//! name = Speakers
//! gain_db = 0.0
//! trim_db = 0.0
//! pan = 0.0           # balance
//! mute = false
//! mono = false
//! connect = @default
//! routes = B1         # buses this bus feeds after its fader (never in a loop)
//! cable = on          # a virtual microphone of this bus (default: B1, B2)
//!
//! [recorder]
//! folder = ~/Music/BananaSplit
//! format = wav24      # wav16 | wav24 | wav32f
//! mode = mix          # mix (one stereo file) | multi (one file per channel)
//! links = 1,A1        # channels feeding the recorder: 1-6, A1-A3, B1-B2 or none
//! play_to = A1        # buses the player plays recordings into
//!
//! [vban]
//! enabled = true
//! port = 6980
//!
//! [vban.in1]
//! on = true
//! name = Stream1
//! from = 192.168.1.20 # the sender's IPv4 address, or any
//! strip = 4           # the strip it is added to
//! buffer_ms = 10      # reserve kept in the jitter buffer
//!
//! [vban.out1]
//! on = true
//! name = Stream1
//! to = 192.168.1.20   # the receiver (a .255 address broadcasts)
//! port = 6980
//! source = B1         # 1-6 (a strip, after its fader) or A1-B2 (a bus)
//! format = int16      # int16 | int24 | int32 | float32
//! channels = 2        # 1 = mono (L+R)/2, 2 = stereo
//!
//! [gui]
//! theme = dark        # dark | light | auto (follow the desktop)
//! close_to_tray = true  # closing the window keeps BananaSplit running
//! zoom = 1.0          # 0.5 .. 2
//! window = 1780x1000  # size of the app window (written when it closes; empty = fit the screen)
//! scene = Live Room   # the scene loaded or saved last (shown in the GUI)
//! auto_update = false # install new versions by itself (default off: check only when asked)
//! ```
//!
//! Every strip and bus also has `eq = on | off` and up to six bands,
//! `eq1` .. `eq6 = type freq_hz gain_db q [off]` (see [`crate::eq`]); a band
//! that is not written is the flat default.
//!
//! Every strip has `cable = on | off` too (its virtual output device, on by
//! default); a bus's `cable` is a virtual microphone with the bus's mix (on
//! by default for B1 and B2), so any channel can be a hardware or a virtual
//! one, or both.
//!
//! `connect` may be repeated. Each value is `@default` (the system's default
//! input device for a strip / default output device for a bus, as chosen in
//! the desktop's sound settings), `@physical` (a physical capture / playback
//! device, preferring the default one), or a JACK port-name regular
//! expression. The first two matching ports are connected to L/R; a single
//! (mono) port is connected to both. An empty `connect =` line clears the
//! list.
//!
//! Parsing is strict: unknown sections/keys and bad values are errors with a
//! line number, so a typo can never be silently ignored. A file without a
//! `[recorder]` or `[vban]` section or EQ keys (written by an older BananaSplit)
//! loads with their defaults.

use crate::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS, Shared, clamp_db, clamp_pan, clamp_trim};
use crate::eq::{BANDS, Band, DEFAULT_BANDS, EqSettings};
use crate::recorder::{self, ALL_LINKS, Format, Mode, Settings};
use crate::vban::{self, FromAddr, NUM_IN, NUM_OUT, SampleFormat, VbanConfig};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

pub const PHYSICAL: &str = "@physical";
pub const DEFAULT_DEVICE: &str = "@default";

/// A `connect` value: one of the `@` keywords, or a device-name fragment.
fn check_connect(v: &str) -> Result<(), String> {
    if v.starts_with('@') && v != PHYSICAL && v != DEFAULT_DEVICE {
        return Err(format!("unknown connect keyword {v:?} (expected {DEFAULT_DEVICE}, {PHYSICAL} or part of a device name)"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecorderConfig {
    pub folder: String,
    pub format: Format,
    pub mode: Mode,
    /// Linked channels, same bit layout as [`Shared::rec_links`].
    pub links: u16,
    /// Buses the player plays into (bit b = bus b).
    pub play_to: u8,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        let s = Settings::default();
        RecorderConfig { folder: s.folder, format: s.format, mode: s.mode, links: 1 << NUM_STRIPS, play_to: 1 } // A1
    }
}

impl RecorderConfig {
    pub fn settings(&self) -> Settings {
        Settings { folder: self.folder.clone(), format: self.format, mode: self.mode }
    }

    pub fn set_settings(&mut self, s: &Settings) {
        self.folder = s.folder.clone();
        self.format = s.format;
        self.mode = s.mode;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StripConfig {
    pub name: String,
    pub gain_db: f32,
    pub trim_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub mono: bool,
    pub routes: u8,
    pub connect: Vec<String>,
    /// Its virtual cable (an output device apps can play into).
    pub cable: bool,
    pub eq: EqSettings,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BusConfig {
    pub name: String,
    pub gain_db: f32,
    pub trim_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub mono: bool,
    /// Buses this one feeds after its fader (bit d = bus d; never itself,
    /// never in a loop).
    pub routes: u8,
    pub connect: Vec<String>,
    /// Its virtual cable (a microphone apps can record its mix from).
    pub cable: bool,
    pub eq: EqSettings,
}

impl BusConfig {
    /// Buses A1-A3 start as hardware outputs, B1-B2 as virtual microphones.
    pub fn default_cable(bus: usize) -> bool {
        bus >= crate::engine::NUM_PHYSICAL_BUSES
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub client_name: String,
    /// Requested period in frames (0 = don't request anything).
    pub buffer_size: u32,
    /// Call jack_set_buffer_size() if the server runs a larger period.
    /// Under JACK2 this changes the period for every client.
    pub force_buffer_size: bool,
    /// Create PipeWire virtual cables via `pactl`: the ones switched on with
    /// `cable` (by default one output device per strip and one virtual
    /// microphone per B bus).
    pub create_cables: bool,
    pub strips: [StripConfig; NUM_STRIPS],
    pub buses: [BusConfig; NUM_BUSES],
    pub recorder: RecorderConfig,
    pub vban: VbanConfig,
    pub gui: GuiConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    /// Follow the desktop's light / dark setting.
    Auto,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Auto => "auto",
        }
    }

    pub fn parse(v: &str) -> Result<Theme, String> {
        match v.trim().to_ascii_lowercase().as_str() {
            "dark" => Ok(Theme::Dark),
            "light" => Ok(Theme::Light),
            "auto" => Ok(Theme::Auto),
            _ => Err(format!("unknown theme {v:?} (expected dark, light or auto)")),
        }
    }
}

pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 2.0;

/// Settings of the graphical window (`[gui]`).
#[derive(Debug, Clone, PartialEq)]
pub struct GuiConfig {
    pub theme: Theme,
    /// Closing the window keeps BananaSplit running (in the tray, or
    /// minimized if there is no tray); off = closing it quits.
    pub close_to_tray: bool,
    pub zoom: f32,
    /// Size of the app window, as the user left it.
    pub window: Option<(u32, u32)>,
    /// The scene loaded or saved last ("" = none).
    pub scene: String,
    /// Check for new versions once a day and install them once they are
    /// two days old (off: only when the user asks). Written only when on,
    /// so an older BananaSplit can still read the file.
    pub auto_update: bool,
}

impl Default for GuiConfig {
    fn default() -> Self {
        GuiConfig { theme: Theme::Dark, close_to_tray: true, zoom: 1.0, window: None, scene: String::new(), auto_update: false }
    }
}

/// `1780x1000` (or empty: none).
pub fn parse_window_size(v: &str) -> Result<Option<(u32, u32)>, String> {
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let bad = || format!("window size must look like 1780x1000, got {v:?}");
    let (w, h) = v.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h): (u32, u32) = (w.trim().parse().map_err(|_| bad())?, h.trim().parse().map_err(|_| bad())?);
    if !(320..=16384).contains(&w) || !(240..=16384).contains(&h) {
        return Err(bad());
    }
    Ok(Some((w, h)))
}

pub fn parse_zoom(v: &str) -> Result<f32, String> {
    let z: f32 = v.trim().parse().map_err(|_| format!("expected a number, got {v:?}"))?;
    if !z.is_finite() || !(MIN_ZOOM..=MAX_ZOOM).contains(&z) {
        return Err(format!("zoom must be {MIN_ZOOM} to {MAX_ZOOM}"));
    }
    Ok((z * 100.0).round() / 100.0)
}

impl Default for Config {
    fn default() -> Self {
        let strip = |name: &str, routes: u8, mono: bool, connect: &[&str]| StripConfig {
            name: name.into(),
            gain_db: 0.0,
            trim_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            mono,
            routes,
            connect: connect.iter().map(|s| s.to_string()).collect(),
            cable: true,
            eq: EqSettings::default(),
        };
        let bus = |name: &str, connect: &[&str], cable: bool| BusConfig {
            name: name.into(),
            gain_db: 0.0,
            trim_db: 0.0,
            pan: 0.0,
            mute: false,
            mono: false,
            routes: 0,
            connect: connect.iter().map(|s| s.to_string()).collect(),
            cable,
            eq: EqSettings::default(),
        };
        const A1: u8 = 1;
        const A2: u8 = 2;
        const B1: u8 = 8;
        Config {
            client_name: "BananaSplit".into(),
            buffer_size: 128,
            force_buffer_size: false,
            create_cables: true,
            strips: [
                strip("Mic", B1, true, &[DEFAULT_DEVICE]),
                strip("Discord", A1 | A2, false, &[]),
                strip("Desktop", A1, false, &[]),
                strip("Game", A1, false, &[]),
                strip("Music", A1, false, &[]),
                strip("Aux", A1, false, &[]),
            ],
            buses: [
                bus("Speakers", &[DEFAULT_DEVICE], false),
                bus("Headphones", &[], false),
                bus("A3", &[], false),
                bus("Virtual B1", &[], true),
                bus("Virtual B2", &[], true),
            ],
            recorder: RecorderConfig::default(),
            vban: VbanConfig::default(),
            gui: GuiConfig::default(),
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}
impl std::error::Error for ParseError {}

enum Section {
    None,
    Global,
    Gui,
    Strip(usize),
    Bus(usize),
    Recorder,
    Vban,
    VbanIn(usize),
    VbanOut(usize),
}

/// `eq`, `eq1` .. `eq6` keys of a strip or bus.
fn eq_key(eq: &mut EqSettings, k: &str, v: &str) -> Option<Result<(), String>> {
    if k == "eq" {
        return Some(parse_bool(v).map(|b| eq.on = b));
    }
    let n: usize = k.strip_prefix("eq")?.parse().ok()?;
    if !(1..=BANDS).contains(&n) {
        return Some(Err(format!("unknown key {k:?} (EQ bands are eq1..eq{BANDS})")));
    }
    Some(Band::parse(v).map(|b| eq.bands[n - 1] = b))
}

pub(crate) fn write_eq(o: &mut String, eq: &EqSettings) {
    let _ = writeln!(o, "eq = {}", if eq.on { "on" } else { "off" });
    for (i, b) in eq.bands.iter().enumerate() {
        if *b != DEFAULT_BANDS[i] {
            let _ = writeln!(o, "eq{} = {}", i + 1, b.to_config());
        }
    }
}

fn vban_slot(h: &str, prefix: &str, n: usize) -> Option<Result<usize, String>> {
    let k: usize = h.get(..prefix.len()).filter(|p| p.eq_ignore_ascii_case(prefix)).and(h.get(prefix.len()..))?.parse().ok()?;
    Some(if (1..=n).contains(&k) { Ok(k - 1) } else { Err(format!("unknown section [{h}] (VBAN streams are {prefix}1..{prefix}{n})")) })
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("expected true/false, got {v:?}")),
    }
}

fn parse_num(v: &str) -> Result<f32, String> {
    let g: f32 = v.parse().map_err(|_| format!("expected a number, got {v:?}"))?;
    if !g.is_finite() {
        return Err(format!("expected a finite number, got {v:?}"));
    }
    Ok(g)
}

fn parse_gain(v: &str) -> Result<f32, String> {
    parse_num(v).map(clamp_db)
}

/// Input gain, dB (clamped to its range, rounded to 0.1 dB).
pub fn parse_trim(v: &str) -> Result<f32, String> {
    parse_num(v).map(|g| (clamp_trim(g) * 10.0).round() / 10.0)
}

/// Pan / balance, -1 .. 1 (clamped, rounded to 0.01).
pub fn parse_pan(v: &str) -> Result<f32, String> {
    parse_num(v).map(|p| (clamp_pan(p) * 100.0).round() / 100.0)
}

pub fn parse_routes(v: &str) -> Result<u8, String> {
    let mut mask = 0u8;
    for tok in v.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if tok.eq_ignore_ascii_case("none") {
            continue;
        }
        match BUS_IDS.iter().position(|b| b.eq_ignore_ascii_case(tok)) {
            Some(i) => mask |= 1 << i,
            None => return Err(format!("unknown bus {tok:?} (expected A1, A2, A3, B1, B2)")),
        }
    }
    Ok(mask)
}

pub fn format_routes(mask: u8) -> String {
    let v: Vec<&str> = (0..NUM_BUSES).filter(|b| mask & (1 << b) != 0).map(|b| BUS_IDS[b]).collect();
    if v.is_empty() { "none".into() } else { v.join(",") }
}

/// An error on a line with a `#` after its value: the value runs to the end
/// of the line (names may contain `#`), so say where comments go.
fn with_comment_hint(raw: &str, msg: String) -> String {
    let tail = raw.find(['=', ']']).map_or("", |i| &raw[i + 1..]);
    if tail.contains(" #") || tail.contains("\t#") {
        format!("{msg} (a comment goes on a line of its own: a value runs to the end of its line)")
    } else {
        msg
    }
}

fn valid_client_name(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ParseError> {
        // Notepad may save the file as UTF-8 with a byte order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut cfg = Config::default();
        let mut sec = Section::None;
        // A `connect` list from the file replaces the default list the first
        // time it is seen in a section, then appends.
        let mut strip_seen = [false; NUM_STRIPS];
        let mut bus_seen = [false; NUM_BUSES];
        // Line of each bus's `routes` (to point at it if they make a loop).
        let mut send_line = [0usize; NUM_BUSES];

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let err = |msg: String| ParseError { line: line_no, msg: with_comment_hint(raw, msg) };
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(h) = line.strip_prefix('[') {
                let h = h.strip_suffix(']').ok_or_else(|| err("missing ']'".into()))?.trim();
                sec = if h.eq_ignore_ascii_case("global") {
                    Section::Global
                } else if h.eq_ignore_ascii_case("gui") {
                    Section::Gui
                } else if h.eq_ignore_ascii_case("recorder") {
                    Section::Recorder
                } else if h.eq_ignore_ascii_case("vban") {
                    Section::Vban
                } else if let Some(r) = vban_slot(h, "vban.in", NUM_IN) {
                    Section::VbanIn(r.map_err(err)?)
                } else if let Some(r) = vban_slot(h, "vban.out", NUM_OUT) {
                    Section::VbanOut(r.map_err(err)?)
                } else if let Some(n) = h.strip_prefix("strip") {
                    match n.parse::<usize>() {
                        Ok(i) if (1..=NUM_STRIPS).contains(&i) => Section::Strip(i - 1),
                        _ => return Err(err(format!("unknown section [{h}] (strips are strip1..strip{NUM_STRIPS})"))),
                    }
                } else if let Some(id) = h.strip_prefix("bus.") {
                    match BUS_IDS.iter().position(|b| b.eq_ignore_ascii_case(id)) {
                        Some(i) => Section::Bus(i),
                        None => return Err(err(format!("unknown section [{h}] (buses are bus.A1..bus.B2)"))),
                    }
                } else {
                    return Err(err(format!("unknown section [{h}]")));
                };
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| err("expected key = value".into()))?;
            let (k, v) = (k.trim(), v.trim());
            match sec {
                Section::None => return Err(err("key outside of a section".into())),
                Section::Global => match k {
                    "client_name" => {
                        if !valid_client_name(v) {
                            return Err(err("client_name must be 1-32 chars of [A-Za-z0-9_-]".into()));
                        }
                        cfg.client_name = v.into();
                    }
                    "buffer_size" => {
                        let n: u32 = v.parse().map_err(|_| err(format!("bad buffer_size {v:?}")))?;
                        if n != 0 && (!n.is_power_of_two() || !(16..=8192).contains(&n)) {
                            return Err(err("buffer_size must be 0 or a power of two in 16..=8192".into()));
                        }
                        cfg.buffer_size = n;
                    }
                    "force_buffer_size" => cfg.force_buffer_size = parse_bool(v).map_err(err)?,
                    "create_cables" => cfg.create_cables = parse_bool(v).map_err(err)?,
                    _ => return Err(err(format!("unknown key {k:?} in [global]"))),
                },
                Section::Gui => match k {
                    "theme" => cfg.gui.theme = Theme::parse(v).map_err(err)?,
                    "close_to_tray" => cfg.gui.close_to_tray = parse_bool(v).map_err(err)?,
                    "zoom" => cfg.gui.zoom = parse_zoom(v).map_err(err)?,
                    "window" => cfg.gui.window = parse_window_size(v).map_err(err)?,
                    "scene" => {
                        if !v.is_empty() {
                            crate::scenes::validate_name(v).map_err(err)?;
                        }
                        cfg.gui.scene = v.into();
                    }
                    "auto_update" => cfg.gui.auto_update = parse_bool(v).map_err(err)?,
                    _ => return Err(err(format!("unknown key {k:?} in [gui]"))),
                },
                Section::Strip(i) => {
                    let s = &mut cfg.strips[i];
                    match k {
                        "name" => s.name = v.into(),
                        "gain_db" => s.gain_db = parse_gain(v).map_err(err)?,
                        "trim_db" => s.trim_db = parse_trim(v).map_err(err)?,
                        "pan" => s.pan = parse_pan(v).map_err(err)?,
                        "mute" => s.mute = parse_bool(v).map_err(err)?,
                        "solo" => s.solo = parse_bool(v).map_err(err)?,
                        "mono" => s.mono = parse_bool(v).map_err(err)?,
                        "routes" => s.routes = parse_routes(v).map_err(err)?,
                        "cable" => s.cable = parse_bool(v).map_err(err)?,
                        "connect" => {
                            check_connect(v).map_err(err)?;
                            if !strip_seen[i] {
                                s.connect.clear();
                                strip_seen[i] = true;
                            }
                            if v.is_empty() { s.connect.clear() } else { s.connect.push(v.into()) }
                        }
                        _ => match eq_key(&mut s.eq, k, v) {
                            Some(r) => r.map_err(err)?,
                            None => return Err(err(format!("unknown key {k:?} in [strip{}]", i + 1))),
                        },
                    }
                }
                Section::Bus(i) => {
                    let b = &mut cfg.buses[i];
                    match k {
                        "name" => b.name = v.into(),
                        "gain_db" => b.gain_db = parse_gain(v).map_err(err)?,
                        "trim_db" => b.trim_db = parse_trim(v).map_err(err)?,
                        "pan" => b.pan = parse_pan(v).map_err(err)?,
                        "mute" => b.mute = parse_bool(v).map_err(err)?,
                        "mono" => b.mono = parse_bool(v).map_err(err)?,
                        "routes" => {
                            let r = parse_routes(v).map_err(err)?;
                            if r & (1 << i) != 0 {
                                return Err(err(format!("bus {} cannot feed itself", BUS_IDS[i])));
                            }
                            b.routes = r;
                            send_line[i] = line_no;
                        }
                        "cable" => b.cable = parse_bool(v).map_err(err)?,
                        "connect" => {
                            check_connect(v).map_err(err)?;
                            if !bus_seen[i] {
                                b.connect.clear();
                                bus_seen[i] = true;
                            }
                            if v.is_empty() { b.connect.clear() } else { b.connect.push(v.into()) }
                        }
                        _ => match eq_key(&mut b.eq, k, v) {
                            Some(r) => r.map_err(err)?,
                            None => return Err(err(format!("unknown key {k:?} in [bus.{}]", BUS_IDS[i]))),
                        },
                    }
                }
                Section::Recorder => {
                    let r = &mut cfg.recorder;
                    match k {
                        "folder" => {
                            recorder::validate_folder(v).map_err(err)?;
                            r.folder = v.into();
                        }
                        "format" => r.format = Format::parse(v).map_err(err)?,
                        "mode" => r.mode = Mode::parse(v).map_err(err)?,
                        "links" => r.links = recorder::parse_links(v).map_err(err)?,
                        "play_to" => r.play_to = parse_routes(v).map_err(err)?,
                        _ => return Err(err(format!("unknown key {k:?} in [recorder]"))),
                    }
                }
                Section::Vban => match k {
                    "enabled" => cfg.vban.enabled = parse_bool(v).map_err(err)?,
                    "port" => cfg.vban.port = vban::parse_port(v).map_err(err)?,
                    _ => return Err(err(format!("unknown key {k:?} in [vban]"))),
                },
                Section::VbanIn(i) => {
                    let s = &mut cfg.vban.ins[i];
                    match k {
                        "on" => s.on = parse_bool(v).map_err(err)?,
                        "name" => {
                            vban::proto::validate_name(v).map_err(err)?;
                            s.name = v.into();
                        }
                        "from" => s.from = FromAddr::parse(v).map_err(err)?,
                        "strip" => s.strip = vban::parse_strip(v).map_err(err)?,
                        "buffer_ms" => s.buffer_ms = vban::parse_buffer_ms(v).map_err(err)?,
                        _ => return Err(err(format!("unknown key {k:?} in [vban.in{}]", i + 1))),
                    }
                }
                Section::VbanOut(i) => {
                    let s = &mut cfg.vban.outs[i];
                    match k {
                        "on" => s.on = parse_bool(v).map_err(err)?,
                        "name" => {
                            vban::proto::validate_name(v).map_err(err)?;
                            s.name = v.into();
                        }
                        "to" => s.to = vban::parse_to(v).map_err(err)?,
                        "port" => s.port = vban::parse_port(v).map_err(err)?,
                        "source" => s.source = vban::parse_source(v).map_err(err)?,
                        "format" => s.format = SampleFormat::parse_send(v).map_err(err)?,
                        "channels" => s.channels = vban::parse_channels(v).map_err(err)?,
                        _ => return Err(err(format!("unknown key {k:?} in [vban.out{}]", i + 1))),
                    }
                }
            }
        }
        // Bus sends must not make a loop (a bus would hear itself).
        let sends: [u8; NUM_BUSES] = std::array::from_fn(|b| cfg.buses[b].routes);
        for (b, &from) in BUS_IDS.iter().enumerate() {
            for (d, &to) in BUS_IDS.iter().enumerate() {
                if sends[b] & (1 << d) != 0 && crate::engine::reach(&sends, d) & (1 << b) != 0 {
                    return Err(ParseError {
                        line: send_line[b],
                        msg: format!("bus {from} feeding {to} makes a loop ({to} already reaches {from})"),
                    });
                }
            }
        }
        Ok(cfg)
    }

    pub fn to_ini(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "# BananaSplit configuration — written by BananaSplit; comments are not preserved.");
        let _ = writeln!(o, "# connect = @default | @physical | <JACK port regex>   (may be repeated;");
        let _ = writeln!(o, "#           `bananasplit --list-ports` shows the port names and a ready-made line)");
        let _ = writeln!(o, "# eq = on | off;  eq1 .. eq6 = type freq_hz gain_db q [off], type = bell | lowshelf |");
        let _ = writeln!(o, "#           highshelf | lowcut | highcut | notch. A band not written here is flat:");
        let _ = writeln!(o, "#           lowshelf 100, bell 250 / 600 / 1500 / 4000, highshelf 10000, all +0 dB");
        let _ = writeln!(o, "# cable = on | off: the channel's virtual cable (written when it differs from the");
        let _ = writeln!(o, "#           default: on for the strips and B1, B2, off for A1-A3)");
        let _ = writeln!(o, "# routes on a bus = the other buses it feeds after its fader (never in a loop)");
        let _ = writeln!(o, "\n[global]");
        let _ = writeln!(o, "client_name = {}", self.client_name);
        let _ = writeln!(o, "buffer_size = {}", self.buffer_size);
        let _ = writeln!(o, "force_buffer_size = {}", self.force_buffer_size);
        let _ = writeln!(o, "create_cables = {}", self.create_cables);
        let g = &self.gui;
        let _ = writeln!(o, "\n[gui]");
        let _ = writeln!(o, "theme = {}", g.theme.as_str());
        let _ = writeln!(o, "close_to_tray = {}", g.close_to_tray);
        let _ = writeln!(o, "zoom = {}", g.zoom);
        let _ = writeln!(o, "window = {}", g.window.map_or(String::new(), |(w, h)| format!("{w}x{h}")));
        let _ = writeln!(o, "scene = {}", g.scene);
        if g.auto_update {
            let _ = writeln!(o, "auto_update = true");
        }
        for (i, s) in self.strips.iter().enumerate() {
            let _ = writeln!(o, "\n[strip{}]", i + 1);
            let _ = writeln!(o, "name = {}", s.name);
            let _ = writeln!(o, "gain_db = {}", s.gain_db);
            let _ = writeln!(o, "trim_db = {}", s.trim_db);
            let _ = writeln!(o, "pan = {}", s.pan);
            let _ = writeln!(o, "mute = {}", s.mute);
            let _ = writeln!(o, "solo = {}", s.solo);
            let _ = writeln!(o, "mono = {}", s.mono);
            let _ = writeln!(o, "routes = {}", format_routes(s.routes));
            if s.connect.is_empty() {
                let _ = writeln!(o, "connect =");
            }
            for c in &s.connect {
                let _ = writeln!(o, "connect = {c}");
            }
            if !s.cable {
                let _ = writeln!(o, "cable = off");
            }
            write_eq(&mut o, &s.eq);
        }
        for (i, b) in self.buses.iter().enumerate() {
            let _ = writeln!(o, "\n[bus.{}]", BUS_IDS[i]);
            let _ = writeln!(o, "name = {}", b.name);
            let _ = writeln!(o, "gain_db = {}", b.gain_db);
            let _ = writeln!(o, "trim_db = {}", b.trim_db);
            let _ = writeln!(o, "pan = {}", b.pan);
            let _ = writeln!(o, "mute = {}", b.mute);
            let _ = writeln!(o, "mono = {}", b.mono);
            // (only when used, so that BananaSplit 2.6 can still read the file)
            if b.routes != 0 {
                let _ = writeln!(o, "routes = {}", format_routes(b.routes));
            }
            if b.connect.is_empty() {
                let _ = writeln!(o, "connect =");
            }
            for c in &b.connect {
                let _ = writeln!(o, "connect = {c}");
            }
            if b.cable != BusConfig::default_cable(i) {
                let _ = writeln!(o, "cable = {}", if b.cable { "on" } else { "off" });
            }
            write_eq(&mut o, &b.eq);
        }
        let r = &self.recorder;
        let _ = writeln!(o, "\n[recorder]");
        let _ = writeln!(o, "# links: 1-6 = strips, A1-A3 / B1-B2 = buses; mode = mix | multi");
        let _ = writeln!(o, "folder = {}", r.folder);
        let _ = writeln!(o, "format = {}", r.format.as_str());
        let _ = writeln!(o, "mode = {}", r.mode.as_str());
        let _ = writeln!(o, "links = {}", recorder::format_links(r.links));
        let _ = writeln!(o, "play_to = {}", format_routes(r.play_to));
        let v = &self.vban;
        let _ = writeln!(o, "\n[vban]");
        let _ = writeln!(o, "# Network audio, compatible with Voicemeeter's VBAN. Streams (up to 8 each way):");
        let _ = writeln!(o, "#   [vban.in1] .. [vban.in8]:   on, name, from = <sender IPv4> | any, strip = 1-6, buffer_ms");
        let _ = writeln!(o, "#   [vban.out1] .. [vban.out8]: on, name, to = <receiver IPv4>, port, source = 1-6 | A1-B2,");
        let _ = writeln!(o, "#                               format = int16 | int24 | int32 | float32, channels = 1 | 2");
        let _ = writeln!(o, "enabled = {}", v.enabled);
        let _ = writeln!(o, "port = {}", v.port);
        for (i, s) in v.ins.iter().enumerate() {
            if *s == vban::InStream::default_for(i) {
                continue;
            }
            let _ = writeln!(o, "\n[vban.in{}]", i + 1);
            let _ = writeln!(o, "on = {}", s.on);
            let _ = writeln!(o, "name = {}", s.name);
            let _ = writeln!(o, "from = {}", s.from.as_string());
            let _ = writeln!(o, "strip = {}", s.strip + 1);
            let _ = writeln!(o, "buffer_ms = {}", s.buffer_ms);
        }
        for (i, s) in v.outs.iter().enumerate() {
            if *s == vban::OutStream::default_for(i) {
                continue;
            }
            let _ = writeln!(o, "\n[vban.out{}]", i + 1);
            let _ = writeln!(o, "on = {}", s.on);
            let _ = writeln!(o, "name = {}", s.name);
            let _ = writeln!(o, "to = {}", s.to.map_or(String::new(), |ip| ip.to_string()));
            let _ = writeln!(o, "port = {}", s.port);
            let _ = writeln!(o, "source = {}", recorder::source_id(s.source));
            let _ = writeln!(o, "format = {}", s.format.as_str());
            let _ = writeln!(o, "channels = {}", s.channels);
        }
        o
    }

    /// Push mixer parameters into the shared real-time state.
    pub fn apply(&self, sh: &Shared) {
        for (s, p) in self.strips.iter().zip(&sh.strips) {
            p.gain_db.store(clamp_db(s.gain_db));
            p.trim_db.store(clamp_trim(s.trim_db));
            p.pan.store(clamp_pan(s.pan));
            p.mute.store(s.mute, Relaxed);
            p.solo.store(s.solo, Relaxed);
            p.mono.store(s.mono, Relaxed);
            p.routes.store(s.routes & 0b11111, Relaxed);
            p.eq.apply(&s.eq);
        }
        for (i, (b, p)) in self.buses.iter().zip(&sh.buses).enumerate() {
            p.gain_db.store(clamp_db(b.gain_db));
            p.trim_db.store(clamp_trim(b.trim_db));
            p.pan.store(clamp_pan(b.pan));
            p.mute.store(b.mute, Relaxed);
            p.mono.store(b.mono, Relaxed);
            p.routes.store(b.routes & crate::engine::ALL_BUSES & !(1 << i), Relaxed);
            p.eq.apply(&b.eq);
        }
        sh.rec_links.store(self.recorder.links & ALL_LINKS, Relaxed);
    }

    /// Pull current mixer parameters back from the shared state.
    pub fn capture(&mut self, sh: &Shared) {
        for (s, p) in self.strips.iter_mut().zip(&sh.strips) {
            s.gain_db = p.gain_db.load();
            s.trim_db = p.trim_db.load();
            s.pan = p.pan.load();
            s.mute = p.mute.load(Relaxed);
            s.solo = p.solo.load(Relaxed);
            s.mono = p.mono.load(Relaxed);
            s.routes = p.routes.load(Relaxed);
            s.eq = p.eq.settings();
        }
        let sends = sh.bus_routes();
        for (i, (b, p)) in self.buses.iter_mut().zip(&sh.buses).enumerate() {
            b.gain_db = p.gain_db.load();
            b.trim_db = p.trim_db.load();
            b.pan = p.pan.load();
            b.mute = p.mute.load(Relaxed);
            b.mono = p.mono.load(Relaxed);
            b.routes = sends[i];
            b.eq = p.eq.settings();
        }
        self.recorder.links = sh.rec_links.load(Relaxed) & ALL_LINKS;
    }

    pub fn default_path() -> PathBuf {
        config_home().join("bananasplit").join("bananasplit.ini")
    }

    /// Where versions up to 2.4, still called vmix, kept the config.
    pub fn legacy_path() -> PathBuf {
        config_home().join("vmix").join("vmix.ini")
    }

    /// First start after the rename from vmix: if `path` does not exist yet
    /// and the old `legacy` file does, write its settings to `path`. The old
    /// file is left alone, so vmix 2.4 still starts with it. The old default
    /// JACK name "vmix" becomes "BananaSplit" (which renames the virtual
    /// cables too); a name the user picked is kept. Returns a note for the
    /// user when something was copied.
    pub fn migrate_legacy(path: &Path, legacy: &Path) -> Result<Option<String>, String> {
        if path.exists() {
            return Ok(None);
        }
        let text = match std::fs::read_to_string(legacy) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", legacy.display())),
        };
        let mut cfg = Config::parse(&text).map_err(|e| format!("{}: {e}", legacy.display()))?;
        let renamed = cfg.client_name == LEGACY_CLIENT_NAME;
        if renamed {
            cfg.client_name = Config::default().client_name;
        }
        cfg.save(path)?;
        let mut note = format!("copied the vmix settings from {} to {}", legacy.display(), path.display());
        if renamed {
            note.push_str(&format!(
                "; the virtual cables are now called {}-In1 … and {}-Out-B1 …, so pick them again in your apps",
                cfg.client_name, cfg.client_name
            ));
        }
        Ok(Some(note))
    }

    /// Load from `path`; if it does not exist, write the defaults there.
    pub fn load_or_create(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save(path)?;
                Ok(cfg)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Load from `path`, or the defaults if it does not exist. Never writes.
    pub fn load_or_default(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Atomic save (write temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
        }
        write_replacing(path, &self.to_ini())
    }
}

/// Write `text` to `path` all at once: into a temporary file next to it,
/// renamed over it. A symlink (a config kept with the dotfiles) stays a
/// link, and the file it points at is written; the file keeps its
/// permissions (`chmod 600`).
pub fn write_replacing(path: &Path, text: &str) -> Result<(), String> {
    let target = real_file(path);
    let mut tmp = target.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(m) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", target.display())
    })
}

/// The file `path` names, through symlinks (also one whose file does not
/// exist yet).
fn real_file(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..16 {
        match std::fs::read_link(&p) {
            Ok(to) => p = if to.is_absolute() { to } else { p.parent().unwrap_or(Path::new("")).join(to) },
            Err(_) => break,
        }
    }
    p
}


/// The JACK name vmix (this program before 2.5) used by default.
pub const LEGACY_CLIENT_NAME: &str = "vmix";

/// `%APPDATA%\bananasplit` on Windows.
fn config_home() -> PathBuf {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Roaming")))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn saving_keeps_a_symlink_and_the_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("bs-cfg-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dotfiles")).unwrap();
        std::fs::create_dir_all(d.join("cfg")).unwrap();
        let real = d.join("dotfiles/bananasplit.ini");
        let link = d.join("cfg/bananasplit.ini");
        let mut c = Config::default();
        c.save(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink("../dotfiles/bananasplit.ini", &link).unwrap();
        c.strips[0].name = "Saved through the link".into();
        c.save(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(Config::load_or_default(&real).unwrap().strips[0].name, "Saved through the link");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        // a link whose file is not there yet: the file is made where it points
        let _ = std::fs::remove_file(&real);
        c.save(&link).unwrap();
        assert!(real.is_file() && std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(!d.join("cfg/bananasplit.ini.tmp").exists() && !d.join("dotfiles/bananasplit.ini.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn automatic_updates_are_off_unless_switched_on() {
        assert!(!Config::default().gui.auto_update);
        assert!(!Config::parse("[gui]\ntheme = dark\n").unwrap().gui.auto_update);
        let mut c = Config::default();
        c.gui.auto_update = true;
        let text = c.to_ini();
        assert!(text.contains("\nscene = \nauto_update = true\n"), "{text}");
        assert_eq!(Config::parse(&text).unwrap(), c);
        assert!(!Config::parse("[gui]\nauto_update = off\n").unwrap().gui.auto_update);
        assert!(Config::parse("[gui]\nauto_update = sometimes\n").is_err());
    }

    #[test]
    fn a_file_with_a_byte_order_mark_loads() {
        let c = Config::parse("\u{feff}[global]\nbuffer_size = 256\n").unwrap();
        assert_eq!(c.buffer_size, 256);
    }

    #[test]
    fn a_comment_after_a_value_is_explained() {
        let e = Config::parse("[global]\nbuffer_size = 128   # period\n").unwrap_err();
        assert!(e.msg.contains("a comment goes on a line of its own"), "{e:?}");
        let e = Config::parse("[vban.in1]   # incoming\n").unwrap_err();
        assert!(e.msg.contains("a comment goes on a line of its own"), "{e:?}");
        // a name may contain '#'
        assert_eq!(Config::parse("[strip1]\nname = Mic #2\n").unwrap().strips[0].name, "Mic #2");
        let e = Config::parse("[global]\nbuffer_size = 12x\n").unwrap_err();
        assert!(!e.msg.contains("comment"), "{e:?}");
    }

    use super::*;

    #[test]
    fn default_round_trips() {
        let c = Config::default();
        assert_eq!(Config::parse(&c.to_ini()).unwrap(), c);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn modified_round_trips() {
        let mut c = Config::default();
        c.client_name = "mix_2".into();
        c.buffer_size = 64;
        c.strips[4].gain_db = -12.5;
        c.strips[4].routes = 0b10101;
        c.strips[4].connect = vec!["^Firefox:output_F[LR]$".into(), "a{1,2}".into()];
        c.strips[0].connect.clear();
        c.buses[2].mono = true;
        c.buses[2].connect = vec!["^USB Audio:playback_F[LR]$".into()];
        c.recorder.folder = "/srv/rec dir/BananaSplit".into();
        c.recorder.format = Format::Wav32f;
        c.recorder.mode = Mode::Multi;
        c.recorder.links = 0b110_0010_0001;
        assert_eq!(Config::parse(&c.to_ini()).unwrap(), c);
        c.recorder.links = 0;
        assert_eq!(Config::parse(&c.to_ini()).unwrap(), c);
        // EQ
        c.strips[0].eq.on = true;
        c.strips[0].eq.bands[0] = Band { on: true, kind: crate::eq::Kind::LowCut, freq: 80.0, gain_db: 0.0, q: 0.71 };
        c.strips[0].eq.bands[4].gain_db = 3.5;
        c.buses[1].eq.bands[5] = Band { on: false, ..c.buses[1].eq.bands[5] };
        c.buses[4].eq.on = true;
        assert_eq!(Config::parse(&c.to_ini()).unwrap(), c);
        // VBAN
        c.vban.enabled = true;
        c.vban.port = 6990;
        c.vban.ins[0] = vban::InStream { on: true, name: "Voicemeeter Out".into(), from: FromAddr::Ip([192, 168, 1, 20].into()), strip: 3, buffer_ms: 0 };
        c.vban.ins[7].from = FromAddr::Any;
        c.vban.outs[2] = vban::OutStream { on: true, name: "Mix".into(), to: Some([192, 168, 1, 255].into()), port: 6981, source: 0, format: SampleFormat::F32, channels: 1 };
        c.vban.outs[3].to = Some([10, 0, 0, 1].into());
        let text = c.to_ini();
        assert_eq!(Config::parse(&text).unwrap(), c);
        assert_eq!(text.matches("\n[vban.").count(), 4, "only streams that differ from the defaults are written");
    }

    #[test]
    fn input_gain_pan_player_and_gui() {
        let mut c = Config::default();
        c.strips[1].trim_db = 6.5;
        c.strips[1].pan = -0.35;
        c.buses[4].trim_db = -24.0;
        c.buses[4].pan = 1.0;
        c.recorder.play_to = 0b01001;
        c.gui = GuiConfig { theme: Theme::Auto, close_to_tray: false, zoom: 1.25, window: Some((1600, 900)), scene: "Live Room 06".into(), auto_update: false };
        let text = c.to_ini();
        assert_eq!(Config::parse(&text).unwrap(), c);
        assert!(text.contains("\n[gui]\ntheme = auto\nclose_to_tray = false\nzoom = 1.25\nwindow = 1600x900\nscene = Live Room 06\n"));
        assert!(!text.contains("auto_update"), "off (the default) is not written: an older BananaSplit reads the file");
        assert_eq!(Config::parse("[gui]\nwindow =\n").unwrap().gui.window, None);
        assert!(Config::parse("[gui]\nwindow = 10x10\n").is_err());
        assert!(text.contains("play_to = A1,B1\n"));
        // clamped and rounded like the GUI does it
        let c = Config::parse("[strip1]\ntrim_db = 99\npan = -3\n[bus.A2]\ntrim_db = 3.04\npan = 0.123\n").unwrap();
        assert_eq!((c.strips[0].trim_db, c.strips[0].pan), (crate::engine::MAX_TRIM_DB, -1.0));
        assert_eq!((c.buses[1].trim_db, c.buses[1].pan), (3.0, 0.12));
        for bad in ["[strip1]\npan = left\n", "[strip1]\ntrim_db = nan\n", "[gui]\ntheme = blue\n", "[gui]\nzoom = 3\n",
                    "[gui]\nzoom = x\n", "[gui]\nscene = ../x\n", "[gui]\nwidth = 3\n", "[recorder]\nplay_to = C1\n"] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
        // a 2.5 config (none of these keys) loads with the defaults
        let old = Config::parse("[global]\nclient_name = BananaSplit\n[strip1]\ngain_db = -3\n").unwrap();
        assert_eq!((old.strips[0].trim_db, old.strips[0].pan, old.recorder.play_to), (0.0, 0.0, 1));
        assert_eq!(old.gui, GuiConfig::default());
        assert_eq!(GuiConfig::default().theme, Theme::Dark);
        // apply / capture carry them
        let sh = Shared::new();
        let mut c = Config::default();
        c.strips[5].trim_db = -4.5;
        c.buses[0].pan = -0.5;
        c.apply(&sh);
        assert_eq!((sh.strips[5].trim_db.load(), sh.buses[0].pan.load()), (-4.5, -0.5));
        let mut d = Config::default();
        d.capture(&sh);
        assert_eq!(c, d);
    }

    #[test]
    fn eq_and_vban_errors() {
        let e = Config::parse("[strip2]\neq7 = bell 1000 0 1\n").unwrap_err();
        assert!(e.line == 2 && e.msg.contains("eq7"), "{e}");
        assert!(Config::parse("[bus.A1]\neq1 = bell 1000 0\n").is_err());
        assert!(Config::parse("[bus.A1]\neq = maybe\n").is_err());
        assert!(Config::parse("[strip1]\neqx = 1\n").unwrap_err().msg.contains("eqx"));
        let c = Config::parse("[strip3]\neq = on\neq2 = notch 50 0 10\n").unwrap();
        assert!(c.strips[2].eq.on && c.strips[2].eq.bands[1].kind == crate::eq::Kind::Notch);
        assert_eq!(Config::parse("[vban.in9]\n").unwrap_err().line, 1);
        assert!(Config::parse("[vban.out0]\n").is_err());
        assert!(Config::parse("[vban]\nport = 0\n").is_err());
        assert!(Config::parse("[vban.in1]\nname = 0123456789abcdefg\n").is_err());
        assert!(Config::parse("[vban.in1]\nfrom = 192.168.1.300\n").is_err());
        assert!(Config::parse("[vban.in1]\nstrip = 7\n").is_err());
        assert!(Config::parse("[vban.in1]\nbuffer_ms = 500\n").is_err());
        assert!(Config::parse("[vban.out1]\nsource = B3\n").is_err());
        assert!(Config::parse("[vban.out1]\nformat = mp3\n").is_err());
        assert!(Config::parse("[vban.out1]\nchannels = 8\n").is_err());
        assert!(Config::parse("[vban.out1]\nto = any\n").is_err());
        assert!(Config::parse("[vban.out1]\nvolume = 1\n").unwrap_err().msg.contains("volume"));
        let c = Config::parse("[VBAN]\nenabled = yes\n[vban.IN2]\non = 1\nfrom = any\n[vban.out8]\nsource = a2\nformat = 24\n").unwrap();
        assert!(c.vban.enabled && c.vban.ins[1].on && c.vban.ins[1].from == FromAddr::Any);
        assert_eq!((c.vban.outs[7].source, c.vban.outs[7].format), (7, SampleFormat::I24));
        // older files (no EQ, no [vban]) load with the defaults
        let old = Config::parse("[global]\nclient_name = BananaSplit\n[strip1]\nname = Mic\n").unwrap();
        assert_eq!((&old.strips[0].eq, &old.vban), (&EqSettings::default(), &VbanConfig::default()));
    }

    #[test]
    fn recorder_section() {
        // a config written before the recorder existed still loads
        let old = "[global]\nclient_name = BananaSplit\n[strip1]\nname = Mic\n";
        assert_eq!(Config::parse(old).unwrap().recorder, RecorderConfig::default());
        let c = Config::parse("[Recorder]\nfolder = ~/rec\nformat = wav16\nmode = multi\nlinks = 2, b1\n").unwrap();
        assert_eq!(c.recorder.folder, "~/rec");
        assert_eq!(c.recorder.format, Format::Wav16);
        assert_eq!(c.recorder.mode, Mode::Multi);
        assert_eq!(c.recorder.links, 0b010_0000_0010);
        let e = Config::parse("[recorder]\nformat = mp3\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(Config::parse("[recorder]\nmode = stereo\n").is_err());
        assert!(Config::parse("[recorder]\nlinks = 1,C9\n").unwrap_err().msg.contains("C9"));
        assert!(Config::parse("[recorder]\nfolder =\n").is_err());
        assert!(Config::parse("[recorder]\ngain = 1\n").unwrap_err().msg.contains("gain"));
    }

    #[test]
    fn strict_errors_have_line_numbers() {
        let e = Config::parse("[global]\nbuffer_size = 100\n").unwrap_err();
        assert_eq!(e.line, 2);
        let e = Config::parse("[strip7]\n").unwrap_err();
        assert_eq!(e.line, 1);
        let e = Config::parse("[strip1]\nroutes = A1,C3\n").unwrap_err();
        assert!(e.msg.contains("C3"));
        let e = Config::parse("[bus.A1]\nvolume = 3\n").unwrap_err();
        assert!(e.msg.contains("volume"));
        assert!(Config::parse("gain_db = 1\n").is_err());
        assert!(Config::parse("[global]\nclient_name = bad name\n").is_err());
        assert!(Config::parse("[strip1]\ngain_db = inf\n").is_err());
        let e = Config::parse("[strip1]\nconnect = @defualt\n").unwrap_err();
        assert!(e.line == 2 && e.msg.contains("@defualt"), "{e}");
        assert!(Config::parse("[bus.A2]\nconnect = @mic\n").is_err());
    }

    #[test]
    fn connect_keywords() {
        let c = Config::parse("[strip1]\nconnect = @physical\nconnect = @default\n[bus.A1]\nconnect = @physical\n").unwrap();
        assert_eq!(c.strips[0].connect, [PHYSICAL, DEFAULT_DEVICE]);
        assert_eq!(c.buses[0].connect, [PHYSICAL]);
        // the default config picks the desktop's default devices
        let d = Config::default();
        assert_eq!(d.strips[0].connect, [DEFAULT_DEVICE]);
        assert_eq!(d.buses[0].connect, [DEFAULT_DEVICE]);
    }

    #[test]
    fn load_or_default_never_writes() {
        let path = std::env::temp_dir().join(format!("bananasplit-test-nowrite-{}", std::process::id())).join("bananasplit.ini");
        assert_eq!(Config::load_or_default(&path).unwrap(), Config::default());
        assert!(!path.exists() && !path.parent().unwrap().exists());
    }

    #[test]
    fn bus_sends_and_cables() {
        let mut c = Config::default();
        assert_eq!(c.buses.iter().map(|b| b.cable).collect::<Vec<_>>(), [false, false, false, true, true]);
        assert!(c.strips.iter().all(|s| s.cable));
        let text = c.to_ini();
        assert!(!text.contains("\ncable =") && !text.contains("[bus.A1]\nname = Speakers\ngain_db = 0\ntrim_db = 0\npan = 0\nmute = false\nmono = false\nroutes"),
            "defaults are not written (2.6 can read the file)");
        c.buses[3].routes = 0b00001; // B1 -> A1
        c.buses[4].routes = 0b01000; // B2 -> B1
        c.buses[0].cable = true;
        c.buses[4].cable = false;
        c.strips[2].cable = false;
        let text = c.to_ini();
        assert!(text.contains("routes = A1\nconnect =\n") && text.contains("cable = off"), "{text}");
        assert_eq!(Config::parse(&text).unwrap(), c);
        // apply / capture
        let sh = Shared::new();
        c.apply(&sh);
        assert_eq!(sh.bus_routes(), [0, 0, 0, 0b00001, 0b01000]);
        let mut d = Config::default();
        d.capture(&sh);
        assert_eq!(d.buses[3].routes, 0b00001);
        // errors: itself, a loop (pointing at the line that closes it), a bad id
        assert!(Config::parse("[bus.A2]\nroutes = A2\n").unwrap_err().msg.contains("itself"));
        let e = Config::parse("[bus.A1]\nroutes = B1\n[bus.B1]\nroutes = B2\n[bus.B2]\nroutes = A1\n").unwrap_err();
        assert!(e.msg.contains("loop") && e.line == 2, "{e}");
        assert!(Config::parse("[bus.A1]\nroutes = C1\n").is_err());
        assert!(Config::parse("[bus.A1]\ncable = maybe\n").is_err());
        assert!(Config::parse("[strip1]\ncable = off\n").is_ok_and(|c| !c.strips[0].cable));
    }

    #[test]
    fn gains_are_clamped_and_routes_parse() {
        let c = Config::parse("[strip2]\ngain_db = 99\nroutes = b2, a1 ,none\n").unwrap();
        assert_eq!(c.strips[1].gain_db, crate::engine::MAX_GAIN_DB);
        assert_eq!(c.strips[1].routes, 0b10001);
        assert_eq!(parse_routes("").unwrap(), 0);
        assert_eq!(format_routes(0), "none");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn apply_and_capture() {
        let sh = Shared::new();
        let mut c = Config::default();
        c.strips[3].gain_db = -3.0;
        c.strips[3].solo = true;
        c.buses[4].mute = true;
        c.recorder.links = 0b100_0000_1000;
        c.strips[1].eq.on = true;
        c.strips[1].eq.bands[3].gain_db = -6.0;
        c.buses[2].eq.bands[0].kind = crate::eq::Kind::LowCut;
        c.apply(&sh);
        assert_eq!(sh.rec_links.load(Relaxed), 0b100_0000_1000);
        let mut d = Config::default();
        d.capture(&sh);
        assert_eq!(c, d);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("bananasplit-test-{}", std::process::id()));
        let path = dir.join("sub").join("bananasplit.ini");
        let c = Config::load_or_create(&path).unwrap();
        assert_eq!(c, Config::default());
        let mut c2 = c.clone();
        c2.strips[0].name = "Voice".into();
        c2.save(&path).unwrap();
        assert_eq!(Config::load_or_create(&path).unwrap(), c2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_the_vmix_config_once() {
        let dir = std::env::temp_dir().join(format!("bananasplit-test-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join("vmix").join("vmix.ini");
        let path = dir.join("bananasplit").join("bananasplit.ini");
        // nothing to copy
        assert_eq!(Config::migrate_legacy(&path, &legacy).unwrap(), None);
        assert!(!path.exists());
        // the old default name is replaced, everything else is kept
        let mut old = Config { client_name: "vmix".into(), ..Config::default() };
        old.strips[0].name = "Yeti".into();
        old.recorder.folder = "~/Music/vmix".into();
        old.save(&legacy).unwrap();
        let note = Config::migrate_legacy(&path, &legacy).unwrap().unwrap();
        assert!(note.contains("BananaSplit-In1"), "{note}");
        let new = Config::load_or_default(&path).unwrap();
        assert_eq!(new.client_name, "BananaSplit");
        assert_eq!(new.strips[0].name, "Yeti");
        assert_eq!(new.recorder.folder, "~/Music/vmix");
        assert!(legacy.exists(), "the old file must stay for vmix 2.4");
        // never twice: the new file wins from now on
        assert_eq!(Config::migrate_legacy(&path, &legacy).unwrap(), None);
        // a name the user chose is kept
        std::fs::remove_file(&path).unwrap();
        old.client_name = "studio".into();
        old.save(&legacy).unwrap();
        let note = Config::migrate_legacy(&path, &legacy).unwrap().unwrap();
        assert!(!note.contains("pick them again"), "{note}");
        assert_eq!(Config::load_or_default(&path).unwrap().client_name, "studio");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
