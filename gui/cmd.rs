//! `POST /api/cmd`, as a typed command: [`Cmd::parse`] reads and checks the form
//! (it needs no mixer, so its rules are tested on their own) and says what is
//! asked; `apply` (in `apply.rs`) does it. A refusal is an [`ApiError`].
//!
//! The checks run in the order a request is read, so a request with several
//! faults gets the same message it always did.

use super::MAX_NAME;
use super::api::ApiError;
use bananasplit::config::{Theme, parse_pan, parse_trim, parse_zoom};
use bananasplit::dynamics::{COMP, GATE};
use bananasplit::engine::{NUM_BUSES, NUM_STRIPS, clamp_db};
use bananasplit::eq::{self, BANDS, Kind};
use bananasplit::limiter;
use bananasplit::recorder::{self, Format, Mode, NUM_SOURCES};
use bananasplit::scenes;
use bananasplit::vban::{self, FromAddr, NUM_IN, NUM_OUT, SampleFormat};
use std::net::Ipv4Addr;

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let (h, l) = (hex(*b.get(i + 1)?)?, hex(*b.get(i + 2)?)?);
                out.push(h << 4 | l);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// `application/x-www-form-urlencoded` body -> key/value pairs.
pub fn parse_form(body: &str) -> Result<Vec<(String, String)>, String> {
    body.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match (url_decode(k), url_decode(v)) {
                (Some(k), Some(v)) => Ok((k, v)),
                _ => Err("malformed form data".to_string()),
            }
        })
        .collect()
}

/// The fields of one `POST /api/cmd`. `get` and `req` are also what a
/// `gui_platform::command` reads its own fields with.
#[derive(Debug, PartialEq)]
pub struct Form(Vec<(String, String)>);

impl Form {
    /// The fields of a query string or a form body.
    pub fn parse(body: &str) -> Result<Form, String> {
        parse_form(body).map(Form)
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }
    pub fn req(&self, k: &str) -> Result<&str, String> {
        self.get(k).ok_or_else(|| format!("missing {k:?}"))
    }
    fn flag(&self) -> Result<bool, String> {
        match self.req("v")? {
            "1" | "true" | "on" => Ok(true),
            "0" | "false" | "off" => Ok(false),
            v => Err(format!("expected 0 or 1, got {v:?}")),
        }
    }
    fn ch(&self) -> Result<usize, String> {
        self.req("ch")?.parse::<usize>().ok().filter(|&c| c < NUM_SOURCES).ok_or_else(|| "bad channel".into())
    }
    /// `ch`, which must be an input strip (gate and compressor are on strips only).
    fn strip(&self) -> Result<usize, String> {
        let c = self.ch()?;
        if c < NUM_STRIPS { Ok(c) } else { Err("gate and compressor are on the input strips only".into()) }
    }
    /// `bus`: one of the buses.
    fn bus(&self) -> Result<usize, String> {
        self.req("bus")?.parse().ok().filter(|&b| b < NUM_BUSES).ok_or_else(|| "bad bus".to_string())
    }
    fn index(&self, k: &str, n: usize) -> Result<usize, String> {
        self.req(k)?.parse::<usize>().ok().filter(|&i| i < n).ok_or_else(|| format!("bad {k}"))
    }
    fn bool_of(&self, k: &str) -> Result<Option<bool>, String> {
        match self.get(k) {
            None => Ok(None),
            Some("1" | "true" | "on") => Ok(Some(true)),
            Some("0" | "false" | "off") => Ok(Some(false)),
            Some(v) => Err(format!("{k}: expected 0 or 1, got {v:?}")),
        }
    }
    fn num_of(&self, k: &str, what: &str, lo: f32, hi: f32) -> Result<Option<f32>, String> {
        let Some(v) = self.get(k) else { return Ok(None) };
        let x: f32 = v.trim().parse().map_err(|_| format!("{what} must be a number"))?;
        if !x.is_finite() || x < lo || x > hi {
            return Err(format!("{what} must be {lo} to {hi}"));
        }
        Ok(Some(x))
    }
}

/// `on`, `name` and the other fields of a VBAN stream form, checked.
fn vban_name(f: &Form) -> Result<Option<String>, String> {
    f.get("name")
        .map(|v| {
            let v = v.trim();
            vban::proto::validate_name(v).map(|_| v.to_string())
        })
        .transpose()
}

/// Which of a strip's gate or compressor settings (an index into `GATE` or `COMP`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DynSetting {
    Gate(usize),
    Comp(usize),
}

/// What a scene command does with the named scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SceneAct {
    Load,
    Save,
    Delete,
}

/// What an update command asks for. An unknown or missing action is only
/// refused once it is known there is an updater, as it always was.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateAct {
    Check,
    Install,
    Restart,
    Page,
    Unknown(String),
    Missing,
}

/// One command of the page (or another client), checked: channels are in range
/// (0..5 strips, 6..10 buses), numbers are numbers, names are names.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    /// A channel's output gain in dB, to 0.1 and within the fader's range.
    Gain {
        ch: usize,
        db: f32,
    },
    /// A channel's input gain.
    Trim {
        ch: usize,
        db: f32,
    },
    Pan {
        ch: usize,
        pan: f32,
    },
    /// A bus's limiter, dBFS (`LIMIT_OFF_DB` = off); `bus` is 0..4.
    Limit {
        bus: usize,
        db: f32,
    },
    Mute {
        ch: usize,
        on: bool,
    },
    Mono {
        ch: usize,
        on: bool,
    },
    /// (An input strip: buses have no solo.)
    Solo {
        strip: usize,
        on: bool,
    },
    Route {
        ch: usize,
        bus: usize,
        on: bool,
    },
    // ---- what a channel is connected to (the main loop does it) ----
    Cable {
        ch: usize,
        on: bool,
    },
    Dev {
        ch: usize,
        client: String,
        on: bool,
    },
    DevDefault {
        ch: usize,
        on: bool,
    },
    DevPattern {
        ch: usize,
        pattern: String,
    },
    DevNone {
        ch: usize,
    },
    Stream {
        rec: bool,
        id: u32,
        ch: Option<usize>,
    },
    // ---- recording ----
    Link {
        ch: usize,
        on: bool,
    },
    Rec {
        on: bool,
    },
    RecSet {
        mode: Option<Mode>,
        format: Option<Format>,
        kbps: Option<u32>,
        folder: Option<String>,
    },
    Name {
        ch: usize,
        name: String,
    },
    // ---- EQ, gate and compressor ----
    Eq {
        ch: usize,
        on: bool,
    },
    Band {
        ch: usize,
        band: usize,
        on: Option<bool>,
        kind: Option<Kind>,
        freq: Option<f32>,
        gain_db: Option<f32>,
        q: Option<f32>,
    },
    EqReset {
        ch: usize,
    },
    Gate {
        strip: usize,
        on: bool,
    },
    Comp {
        strip: usize,
        on: bool,
    },
    /// A setting, clamped to its range.
    DynSet {
        strip: usize,
        setting: DynSetting,
        value: f32,
    },
    DynReset {
        strip: usize,
    },
    // ---- VBAN ----
    Vban {
        on: bool,
    },
    VbanPort {
        port: u16,
    },
    VbanIn {
        slot: usize,
        name: Option<String>,
        from: Option<FromAddr>,
        strip: Option<usize>,
        buffer_ms: Option<u32>,
        on: Option<bool>,
    },
    VbanOut {
        slot: usize,
        name: Option<String>,
        to: Option<Option<Ipv4Addr>>,
        port: Option<u16>,
        source: Option<usize>,
        format: Option<SampleFormat>,
        channels: Option<u8>,
        on: Option<bool>,
    },
    // ---- player ----
    Play {
        from: Option<f64>,
        key: Option<String>,
    },
    Take {
        key: String,
    },
    Pause,
    Stop,
    Seek {
        to: f64,
    },
    PlayTo {
        bus: usize,
        on: bool,
    },
    OpenFolder,
    // ---- scenes ----
    Scene {
        name: String,
        act: SceneAct,
    },
    // ---- app settings ----
    Theme(Theme),
    CloseToTray(bool),
    Zoom(f32),
    Autostart(bool),
    Install,
    AutoUpdate(bool),
    Update(UpdateAct),
    /// Bring the window up (the launch's activation token, printable ASCII only).
    Show {
        token: String,
    },
    Save,
    Reconnect,
    Quit,
    /// An op only one system has (`gui_platform::command` knows it, or nobody does).
    Platform {
        op: String,
        form: Form,
    },
}

impl Cmd {
    /// Read one `POST /api/cmd` body.
    pub fn parse(body: &str) -> Result<Cmd, ApiError> {
        let f = Form(parse_form(body)?);
        let op = f.req("op")?;
        Ok(match op {
            "gain" => {
                let ch = f.ch()?;
                let v: f32 = f.req("v")?.parse().map_err(|_| "gain must be a number")?;
                if !v.is_finite() {
                    return Err("gain must be finite".into());
                }
                Cmd::Gain { ch, db: clamp_db((v * 10.0).round() / 10.0) }
            }
            "trim" => Cmd::Trim { ch: f.ch()?, db: parse_trim(f.req("v")?)? },
            "pan" => Cmd::Pan { ch: f.ch()?, pan: parse_pan(f.req("v")?)? },
            "limit" => {
                let ch = f.ch()?;
                if ch < NUM_STRIPS {
                    return Err("only buses have a limiter".into());
                }
                Cmd::Limit { bus: ch - NUM_STRIPS, db: limiter::parse_limit(f.req("v")?)? }
            }
            "mute" => Cmd::Mute { ch: f.ch()?, on: f.flag()? },
            "mono" => Cmd::Mono { ch: f.ch()?, on: f.flag()? },
            "solo" => {
                let strip = f.ch()?;
                if strip >= NUM_STRIPS {
                    return Err("buses have no solo".into());
                }
                Cmd::Solo { strip, on: f.flag()? }
            }
            "route" => {
                let ch = f.ch()?;
                let bus = f.bus()?;
                Cmd::Route { ch, bus, on: f.flag()? }
            }
            "cable" => Cmd::Cable { ch: f.ch()?, on: f.flag()? },
            "dev" => {
                let client = f.req("client")?.to_string();
                if client.is_empty() || client.len() > 320 || client.chars().any(char::is_control) {
                    return Err("bad client name".into());
                }
                Cmd::Dev { ch: f.ch()?, client, on: f.flag()? }
            }
            "devdefault" => Cmd::DevDefault { ch: f.ch()?, on: f.flag()? },
            "devpattern" => Cmd::DevPattern { ch: f.ch()?, pattern: f.req("pattern")?.to_string() },
            "devnone" => Cmd::DevNone { ch: f.ch()? },
            "stream" => {
                let rec = match f.req("kind")? {
                    "play" => false,
                    "rec" => true,
                    k => return Err(format!("unknown stream kind {k:?}").into()),
                };
                let id: u32 = f.req("id")?.parse().map_err(|_| "bad stream id")?;
                let ch = match f.get("ch") {
                    None | Some("" | "none") => None,
                    Some(_) => Some(f.ch()?),
                };
                Cmd::Stream { rec, id, ch }
            }
            "link" => Cmd::Link { ch: f.ch()?, on: f.flag()? },
            "rec" => Cmd::Rec { on: f.flag()? },
            "recset" => {
                let mode = f.get("mode").map(Mode::parse).transpose()?;
                let format = f.get("format").map(Format::parse).transpose()?;
                let kbps = f.get("kbps").map(recorder::parse_kbps).transpose()?;
                let folder = f.get("folder").map(|d| d.trim().to_string());
                Cmd::RecSet { mode, format, kbps, folder }
            }
            "name" => {
                let ch = f.ch()?;
                let v = f.req("v")?.trim();
                if v.is_empty() || v.chars().count() > MAX_NAME || v.chars().any(char::is_control) {
                    return Err(format!("names are 1-{MAX_NAME} characters").into());
                }
                Cmd::Name { ch, name: v.to_string() }
            }
            "eq" => Cmd::Eq { ch: f.ch()?, on: f.flag()? },
            "band" => {
                let ch = f.ch()?;
                let band = f.index("band", BANDS)?;
                let on = f.bool_of("on")?;
                let kind = f.get("type").map(Kind::parse).transpose()?;
                let freq = f.num_of("f", "frequency", eq::MIN_FREQ, eq::MAX_FREQ)?.map(eq::round_freq);
                let gain_db = f.num_of("g", "gain", eq::MIN_GAIN_DB, eq::MAX_GAIN_DB)?.map(eq::round_gain);
                let q = f.num_of("q", "Q", eq::MIN_Q, eq::MAX_Q)?.map(eq::round_q);
                Cmd::Band { ch, band, on, kind, freq, gain_db, q }
            }
            "eqreset" => Cmd::EqReset { ch: f.ch()? },
            "gate" => Cmd::Gate { strip: f.strip()?, on: f.flag()? },
            "comp" => Cmd::Comp { strip: f.strip()?, on: f.flag()? },
            "dynset" => {
                let strip = f.strip()?;
                let key = f.req("key")?;
                let (setting, spec) = if let Some(i) = GATE.iter().position(|s| s.key == key) {
                    (DynSetting::Gate(i), &GATE[i])
                } else if let Some(i) = COMP.iter().position(|s| s.key == key) {
                    (DynSetting::Comp(i), &COMP[i])
                } else {
                    return Err(format!("unknown setting {key:?}").into());
                };
                let v = f.num_of("v", spec.key, spec.min, spec.max)?.ok_or("missing \"v\"")?;
                Cmd::DynSet { strip, setting, value: spec.clamp(v) }
            }
            "dynreset" => Cmd::DynReset { strip: f.strip()? },
            "vban" => Cmd::Vban { on: f.flag()? },
            "vbanport" => Cmd::VbanPort { port: vban::parse_port(f.req("v")?)? },
            "vbanin" => {
                let slot = f.index("slot", NUM_IN)?;
                let name = vban_name(&f)?;
                let from = f.get("from").map(FromAddr::parse).transpose()?;
                let strip = f.get("strip").map(vban::parse_strip).transpose()?;
                let buffer_ms = f.get("buffer").map(vban::parse_buffer_ms).transpose()?;
                let on = f.bool_of("on")?;
                Cmd::VbanIn { slot, name, from, strip, buffer_ms, on }
            }
            "vbanout" => {
                let slot = f.index("slot", NUM_OUT)?;
                let name = vban_name(&f)?;
                let to = f.get("to").map(vban::parse_to).transpose()?;
                let port = f.get("port").map(vban::parse_port).transpose()?;
                let source = f.get("src").map(vban::parse_source).transpose()?;
                let format = f.get("format").map(SampleFormat::parse_send).transpose()?;
                let channels = f.get("channels").map(vban::parse_channels).transpose()?;
                let on = f.bool_of("on")?;
                Cmd::VbanOut { slot, name, to, port, source, format, channels, on }
            }
            "play" => Cmd::Play {
                from: f.num_of("from", "position", 0.0, 1e9)?.map(f64::from),
                key: f.get("key").map(str::to_string),
            },
            "take" => Cmd::Take { key: f.req("key")?.to_string() },
            "pause" => Cmd::Pause,
            "stop" => Cmd::Stop,
            "seek" => Cmd::Seek { to: f64::from(f.num_of("v", "position", 0.0, 1e9)?.ok_or("missing \"v\"")?) },
            "playto" => {
                let bus = f.bus()?;
                Cmd::PlayTo { bus, on: f.flag()? }
            }
            "openfolder" => Cmd::OpenFolder,
            "scene" => {
                let name = scenes::validate_name(f.req("name")?)?.to_string();
                let act = match f.req("act")? {
                    "load" => SceneAct::Load,
                    "save" => SceneAct::Save,
                    "delete" => SceneAct::Delete,
                    a => return Err(format!("unknown scene action {a:?}").into()),
                };
                Cmd::Scene { name, act }
            }
            "theme" => Cmd::Theme(Theme::parse(f.req("v")?)?),
            "closetray" => Cmd::CloseToTray(f.flag()?),
            "zoom" => Cmd::Zoom(parse_zoom(f.req("v")?)?),
            "autostart" => Cmd::Autostart(f.flag()?),
            "install" => Cmd::Install,
            "autoupdate" => Cmd::AutoUpdate(f.flag()?),
            "update" => Cmd::Update(match f.get("a") {
                Some("check") => UpdateAct::Check,
                Some("install") => UpdateAct::Install,
                Some("restart") => UpdateAct::Restart,
                Some("page") => UpdateAct::Page,
                Some(a) => UpdateAct::Unknown(a.to_string()),
                None => UpdateAct::Missing,
            }),
            "show" => Cmd::Show {
                token: f.get("token").unwrap_or("").chars().filter(|c| c.is_ascii_graphic()).take(256).collect(),
            },
            "save" => Cmd::Save,
            "reconnect" => Cmd::Reconnect,
            "quit" => Cmd::Quit,
            op => Cmd::Platform { op: op.to_string(), form: f },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command, or the message a refusal gives.
    fn p(body: &str) -> Result<Cmd, String> {
        Cmd::parse(body).map_err(|e| e.msg)
    }

    fn refused(body: &str) -> String {
        p(body).unwrap_err()
    }

    #[test]
    fn a_command_is_read_into_its_parts() {
        assert_eq!(p("op=gain&ch=2&v=-3.54"), Ok(Cmd::Gain { ch: 2, db: -3.5 }), "to a tenth of a dB");
        assert_eq!(
            p("op=gain&ch=6&v=99"),
            Ok(Cmd::Gain { ch: 6, db: bananasplit::engine::MAX_GAIN_DB }),
            "within the fader"
        );
        assert_eq!(p("op=trim&ch=1&v=6"), Ok(Cmd::Trim { ch: 1, db: 6.0 }));
        assert_eq!(p("op=limit&ch=9&v=-6.04"), Ok(Cmd::Limit { bus: 3, db: -6.0 }));
        assert_eq!(p("op=limit&ch=6&v=off"), Ok(Cmd::Limit { bus: 0, db: limiter::LIMIT_OFF_DB }));
        assert_eq!(p("op=limit&ch=10&v=-99"), Ok(Cmd::Limit { bus: 4, db: limiter::LIMIT_MIN_DB }));
        assert!(p("op=limit&ch=2&v=-6").is_err(), "strips have none");
        assert!(p("op=limit&ch=7&v=nan").is_err());
        assert_eq!(p("op=mute&ch=10&v=1"), Ok(Cmd::Mute { ch: 10, on: true }));
        assert_eq!(p("op=mono&ch=0&v=off"), Ok(Cmd::Mono { ch: 0, on: false }));
        assert_eq!(p("op=solo&ch=3&v=true"), Ok(Cmd::Solo { strip: 3, on: true }));
        assert_eq!(p("op=route&ch=7&bus=3&v=1"), Ok(Cmd::Route { ch: 7, bus: 3, on: true }));
        assert_eq!(p("op=dev&ch=0&client=Firefox&v=1"), Ok(Cmd::Dev { ch: 0, client: "Firefox".into(), on: true }));
        assert_eq!(p("op=stream&kind=rec&id=42&ch=none"), Ok(Cmd::Stream { rec: true, id: 42, ch: None }));
        assert_eq!(p("op=stream&kind=play&id=7&ch=3"), Ok(Cmd::Stream { rec: false, id: 7, ch: Some(3) }));
        assert_eq!(
            p("op=name&ch=2&v=++Voice+%26+Chat++"),
            Ok(Cmd::Name { ch: 2, name: "Voice & Chat".into() }),
            "trimmed"
        );
        assert_eq!(p("op=seek&v=12.5"), Ok(Cmd::Seek { to: 12.5 }));
        assert_eq!(
            p("op=play&from=3&key=2026-10-21_14-03-05"),
            Ok(Cmd::Play { from: Some(3.0), key: Some("2026-10-21_14-03-05".into()) })
        );
        assert_eq!(p("op=play"), Ok(Cmd::Play { from: None, key: None }));
        assert_eq!(p("op=quit"), Ok(Cmd::Quit));
        assert_eq!(p("op=theme&v=light"), Ok(Cmd::Theme(Theme::Light)));
        assert_eq!(p("op=scene&name=Live&act=save"), Ok(Cmd::Scene { name: "Live".into(), act: SceneAct::Save }));
        assert_eq!(p("op=vbanport&v=6981"), Ok(Cmd::VbanPort { port: 6981 }));
    }

    /// What each field may be, and what is said when it is not.
    #[test]
    fn what_is_wrong_is_said() {
        assert_eq!(refused(""), "missing \"op\"");
        assert_eq!(refused("op=gain"), "missing \"ch\"");
        assert_eq!(refused("op=gain&ch=11&v=0"), "bad channel");
        assert_eq!(refused("op=gain&ch=-1&v=0"), "bad channel");
        assert_eq!(refused("op=gain&ch=0&v=loud"), "gain must be a number");
        assert_eq!(refused("op=gain&ch=0&v=nan"), "gain must be finite");
        assert_eq!(refused("op=mute&ch=0&v=yes"), "expected 0 or 1, got \"yes\"");
        assert_eq!(refused("op=mute&ch=0"), "missing \"v\"");
        assert_eq!(refused("op=route&ch=0&bus=5&v=1"), "bad bus");
        assert_eq!(refused("op=playto&bus=x&v=1"), "bad bus");
        assert_eq!(refused("op=stream&kind=tape&id=1"), "unknown stream kind \"tape\"");
        assert_eq!(refused("op=stream&kind=play&id=x"), "bad stream id");
        assert_eq!(refused("op=band&ch=7&band=6"), "bad band");
        assert_eq!(refused("op=band&ch=7&band=1&g=99"), "gain must be -24 to 24");
        assert_eq!(refused("op=band&ch=7&band=1&type=comb"), Kind::parse("comb").unwrap_err());
        assert_eq!(refused("op=name&ch=0&v="), format!("names are 1-{MAX_NAME} characters"));
        assert_eq!(refused("op=name&ch=0&v=%01x"), format!("names are 1-{MAX_NAME} characters"));
        assert_eq!(refused("op=dev&ch=0&client=&v=1"), "bad client name");
        assert_eq!(refused("op=dynset&ch=0&key=nope&v=1"), "unknown setting \"nope\"");
        assert_eq!(refused("op=seek"), "missing \"v\"");
        assert_eq!(refused("op=seek&v=-1"), "position must be 0 to 1000000000");
        assert!(Cmd::parse("op=%zz").is_err(), "a broken percent escape");
    }

    /// When a request is wrong in several ways, the first thing read says so: the
    /// checks run in the order they always did.
    #[test]
    fn the_first_fault_in_reading_order_is_the_one_said() {
        // the channel is read before the value, the value before the flag
        assert_eq!(refused("op=gain&v=x"), "missing \"ch\"");
        assert_eq!(refused("op=gain&ch=99&v=x"), "bad channel");
        assert_eq!(refused("op=mute&ch=99&v=7"), "bad channel");
        // a bus has no solo, whatever the flag says
        assert_eq!(refused("op=solo&ch=7&v=7"), "buses have no solo");
        assert_eq!(refused("op=solo&ch=7"), "buses have no solo");
        // the client's name before its channel
        assert_eq!(refused("op=dev&ch=99&client=&v=1"), "bad client name");
        // the scene's name before its action
        assert_eq!(refused("op=scene&name=&act=x"), scenes::validate_name("").unwrap_err());
        assert_eq!(refused("op=scene&name=Live&act=x"), "unknown scene action \"x\"");
        // the setting's name before its value; a strip before both
        assert_eq!(refused("op=dynset&ch=0&key=nope&v=x"), "unknown setting \"nope\"");
        assert_eq!(refused("op=dynset&ch=9&key=nope&v=x"), "gate and compressor are on the input strips only");
        // route: channel, bus, flag
        assert_eq!(refused("op=route&ch=99&bus=99"), "bad channel");
        assert_eq!(refused("op=route&ch=0&bus=99"), "bad bus");
    }

    /// A refusal that depends on the mixer (an updater, recordings) is not the parser's:
    /// the command is read, and `apply` says no.
    #[test]
    fn what_only_the_mixer_can_know_is_left_to_it() {
        assert_eq!(p("op=update"), Ok(Cmd::Update(UpdateAct::Missing)));
        assert_eq!(p("op=update&a=bogus"), Ok(Cmd::Update(UpdateAct::Unknown("bogus".into()))));
        assert_eq!(p("op=update&a=restart"), Ok(Cmd::Update(UpdateAct::Restart)));
        assert_eq!(
            p("op=route&ch=6&bus=0&v=1"),
            Ok(Cmd::Route { ch: 6, bus: 0, on: true }),
            "V1 feeding V1 is for apply to refuse"
        );
    }

    #[test]
    fn the_rest_of_the_fields_are_optional_where_they_were() {
        assert_eq!(
            p("op=band&ch=7&band=1&g=-4.54&f=1000"),
            Ok(Cmd::Band { ch: 7, band: 1, on: None, kind: None, freq: Some(1000.0), gain_db: Some(-4.5), q: None })
        );
        assert_eq!(p("op=recset"), Ok(Cmd::RecSet { mode: None, format: None, kbps: None, folder: None }));
        assert_eq!(
            p("op=recset&mode=multi&folder=++/tmp/x++"),
            Ok(Cmd::RecSet { mode: Some(Mode::Multi), format: None, kbps: None, folder: Some("/tmp/x".into()) })
        );
        assert_eq!(
            p("op=vbanin&slot=1&on=1"),
            Ok(Cmd::VbanIn { slot: 1, name: None, from: None, strip: None, buffer_ms: None, on: Some(true) })
        );
        // a token is what a launch hands over: printable ASCII, 256 characters at most
        assert_eq!(p("op=show&token=a%20b%01c"), Ok(Cmd::Show { token: "abc".into() }));
        let long = format!("op=show&token={}", "x".repeat(300));
        assert_eq!(p(&long), Ok(Cmd::Show { token: "x".repeat(256) }));
        assert_eq!(p("op=show"), Ok(Cmd::Show { token: String::new() }));
    }

    /// An op the system adds (or nobody knows) is passed on whole.
    #[test]
    fn an_op_of_the_system_is_passed_on() {
        match p("op=cables&a=page") {
            Ok(Cmd::Platform { op, form }) => {
                assert_eq!(op, "cables");
                assert_eq!(form.get("a"), Some("page"));
            }
            other => panic!("{other:?}"),
        }
    }
}
