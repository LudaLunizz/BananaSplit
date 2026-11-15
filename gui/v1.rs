//! API v1: one command model for every client (scripts, Companion, the MIDI
//! bindings). A value in the mixer has a **path** (`/strips/1/gain_db`,
//! `/buses/B1/mute`, `/strips/2/sends/V1`, `/strips/1/eq/2/freq_hz`,
//! `/vban/in/1/name`, `/player/state`, `/recorder/recording`); a command sets
//! it, toggles it, steps it or fades it. Every change is made as the typed
//! [`Cmd`] of `POST /api/cmd`, through the same `apply`, so the two APIs
//! cannot drift apart: v1 only adds paths, relative changes, fades and batches.
//!
//! `docs/api.md` describes it; the parsing here needs no mixer and is tested
//! on its own, the rest against a test mixer in `mod.rs`.

use super::api::ApiError;
use super::apply::apply;
use super::cmd::{Cmd, DynSetting, SceneAct};
use super::json::{self, Value};
use super::{Hub, jnum, jstr, lock};
use bananasplit::dynamics::{COMP, GATE, Spec};
use bananasplit::engine::{
    BUS_IDS, BusParams, MAX_GAIN_DB, MAX_TRIM_DB, MIN_GAIN_DB, MIN_TRIM_DB, NUM_BUSES, NUM_STRIPS, StripParams,
    bus_index, clamp_db, clamp_pan, clamp_trim,
};
use bananasplit::eq::{self, BANDS, DEFAULT_BANDS};
use bananasplit::limiter::{LIMIT_MIN_DB, LIMIT_OFF_DB, clamp_limit};
use bananasplit::player::PlayState;
use bananasplit::recorder::NUM_SOURCES;
use bananasplit::scenes;
use bananasplit::vban::{self, FromAddr, MAX_BUFFER_MS, NUM_IN, NUM_OUT, SampleFormat};
use std::fmt::Write as _;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The API's version, in `/api/v1/state` and `/api/v1/paths`.
pub const VERSION: u32 = 1;
/// The most commands one request may carry.
pub const MAX_BATCH: usize = 64;
/// The longest fade.
pub const MAX_FADE_MS: u32 = 60_000;
/// How often a running fade moves its value.
const FADE_TICK: Duration = Duration::from_millis(10);
/// The furthest the player seeks, seconds (as `op=seek` allows).
const MAX_POS_S: f32 = 1e9;
/// The longest take key accepted (the recorder's keys are dates, far shorter).
const MAX_KEY: usize = 256;

/// The EQ band types, by `eq::Kind` number.
const EQ_TYPES: [&str; 6] = ["bell", "lowshelf", "highshelf", "lowcut", "highcut", "notch"];
/// What an outgoing VBAN stream sends, by source number (`recorder::source_id`).
const SOURCES: [&str; NUM_SOURCES] = ["1", "2", "3", "4", "5", "6", "V1", "V2", "V3", "B1", "B2"];
/// The VBAN formats a stream can send, in the order of `SampleFormat::SEND`.
const FORMATS: [&str; 4] = ["int16", "int24", "int32", "float32"];
/// The player's states.
const PLAY_STATES: [&str; 3] = ["stopped", "playing", "paused"];

/// One setting of an EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandField {
    On,
    Type,
    Freq,
    Gain,
    Q,
}

impl BandField {
    const ALL: [BandField; 5] = [BandField::On, BandField::Type, BandField::Freq, BandField::Gain, BandField::Q];
    fn name(self) -> &'static str {
        match self {
            BandField::On => "on",
            BandField::Type => "type",
            BandField::Freq => "freq_hz",
            BandField::Gain => "gain_db",
            BandField::Q => "q",
        }
    }
    fn label(self) -> &'static str {
        match self {
            BandField::On => "on",
            BandField::Type => "type",
            BandField::Freq => "frequency",
            BandField::Gain => "gain",
            BandField::Q => "Q",
        }
    }
}

/// What a channel's path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Gain,
    Trim,
    Pan,
    Mute,
    Mono,
    /// Strips only.
    Solo,
    /// Linked to the recorder.
    Record,
    Eq,
    /// One setting of EQ band `band` (0..5).
    Band {
        band: usize,
        f: BandField,
    },
    /// Strips only.
    Gate,
    /// Strips only.
    Comp,
    /// A gate or compressor setting (strips only).
    Dyn(DynSetting),
    /// Buses only.
    Limit,
    /// Feeds bus `b` (a strip's route, a bus's send).
    Send(usize),
}

/// One setting of an incoming VBAN stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InField {
    On,
    Name,
    From,
    Strip,
    Buffer,
}

impl InField {
    const ALL: [InField; 5] = [InField::On, InField::Name, InField::From, InField::Strip, InField::Buffer];
    fn name(self) -> &'static str {
        match self {
            InField::On => "on",
            InField::Name => "name",
            InField::From => "from",
            InField::Strip => "strip",
            InField::Buffer => "buffer_ms",
        }
    }
    fn label(self) -> &'static str {
        match self {
            InField::On => "on",
            InField::Name => "name",
            InField::From => "sender",
            InField::Strip => "strip",
            InField::Buffer => "buffer",
        }
    }
}

/// One setting of an outgoing VBAN stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutField {
    On,
    Name,
    To,
    Port,
    Source,
    Format,
    Channels,
}

impl OutField {
    const ALL: [OutField; 7] = [
        OutField::On,
        OutField::Name,
        OutField::To,
        OutField::Port,
        OutField::Source,
        OutField::Format,
        OutField::Channels,
    ];
    fn name(self) -> &'static str {
        match self {
            OutField::On => "on",
            OutField::Name => "name",
            OutField::To => "to",
            OutField::Port => "port",
            OutField::Source => "source",
            OutField::Format => "format",
            OutField::Channels => "channels",
        }
    }
    fn label(self) -> &'static str {
        match self {
            OutField::To => "receiver",
            f => f.name(),
        }
    }
}

/// What a VBAN path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VbanParam {
    /// VBAN on or off.
    On,
    /// The UDP port it listens on.
    Port,
    /// Incoming stream `slot` (0..7).
    In { slot: usize, f: InField },
    /// Outgoing stream `slot` (0..7).
    Out { slot: usize, f: OutField },
}

/// What a player path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerParam {
    /// Playing (on) or paused (off).
    Playing,
    /// Stopped, playing or paused.
    State,
    /// Where it is in the take, seconds.
    Position,
    /// The loaded take's key.
    Take,
    /// Plays into bus `b`.
    Send(usize),
}

/// A value the API can read and change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `ch` 0..5 = strips 1..6, 6..10 = buses V1..B2.
    Chan {
        ch: usize,
        p: Param,
    },
    /// The recorder is recording.
    Recording,
    Vban(VbanParam),
    Player(PlayerParam),
}

/// How a target's value is kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// On or off: read as 0 or 1.
    Flag,
    /// A number from `min` to `max`, rounded to `step`.
    Number { min: f32, max: f32, step: f32, unit: &'static str },
    /// One of these names, kept as its index.
    Choice(&'static [&'static str]),
    /// Text (a name, an address, a take's key).
    Text,
}

impl Kind {
    /// What it is, for messages: "on or off", "a number" ...
    fn what(self) -> &'static str {
        match self {
            Kind::Flag => "on or off",
            Kind::Number { .. } => "a number",
            Kind::Choice(_) => "one of a list",
            Kind::Text => "text",
        }
    }
}

/// The page's fader taper (`TAPER` in gui.html): more travel around 0 dB. A
/// MIDI fader drives the gain the way the page's fader does.
const TAPER: [(f32, f32); 6] = [(-60.0, 0.0), (-36.0, 0.15), (-24.0, 0.3), (-12.0, 0.55), (0.0, 0.8), (12.0, 1.0)];

fn db_to_pos(db: f32) -> f32 {
    let db = db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if db <= d1 {
            return p0 + (p1 - p0) * (db - d0) / (d1 - d0);
        }
    }
    1.0
}

fn pos_to_db(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if p <= p1 {
            return d0 + (d1 - d0) * (p - p0) / (p1 - p0);
        }
    }
    MAX_GAIN_DB
}

/// Rounded as `/api/cmd` rounds (`(v * 10).round() / 10`), to the same bits.
fn round_to(v: f32, step: f32) -> f32 {
    let k = (1.0 / step).round();
    (v * k).round() / k
}

/// `"3"` of `/vban/in/3/...` as an index (`1..=n` -> `0..n`).
fn slot(s: &str, n: usize) -> Option<usize> {
    s.parse::<usize>().ok().filter(|i| (1..=n).contains(i)).map(|i| i - 1)
}

fn dyn_spec(s: DynSetting) -> &'static Spec {
    match s {
        DynSetting::Gate(i) => &GATE[i],
        DynSetting::Comp(i) => &COMP[i],
    }
}

/// A gate or compressor setting's name in a path: its config key without
/// `gate_`/`comp_`, with its unit (`threshold_db`, `attack_ms`, `ratio`).
fn dyn_name(s: &Spec) -> String {
    let base = s.key.split_once('_').map_or(s.key, |(_, b)| b);
    match s.unit {
        "dB" => format!("{base}_db"),
        "ms" => format!("{base}_ms"),
        _ => base.to_string(),
    }
}

/// The setting of `specs` (GATE or COMP) a path names.
fn dyn_of(specs: &[Spec], name: &str) -> Option<usize> {
    specs.iter().position(|s| dyn_name(s) == name)
}

impl Target {
    /// Read a path (`/strips/1/gain_db`). Buses go by name (`V1` to `B2`; the
    /// old `A1` to `A3` too), strips, EQ bands and VBAN streams by number
    /// (from 1).
    pub fn parse(path: &str) -> Result<Target, String> {
        let bad = || format!("unknown path {path:?}");
        let parts: Vec<&str> = path.strip_prefix('/').ok_or_else(bad)?.split('/').collect();
        match parts.as_slice() {
            ["recorder", "recording"] => return Ok(Target::Recording),
            ["vban", rest @ ..] => return Target::parse_vban(rest).ok_or_else(bad),
            ["player", rest @ ..] => return Target::parse_player(rest).ok_or_else(bad),
            _ => {}
        }
        let (strip, ch) = match parts.as_slice() {
            ["strips", n, ..] => (true, slot(n, NUM_STRIPS).ok_or_else(bad)?),
            ["buses", id, ..] => (false, NUM_STRIPS + bus_index(id).ok_or_else(bad)?),
            _ => return Err(bad()),
        };
        let p = match &parts[2..] {
            ["gain_db"] => Param::Gain,
            ["trim_db"] => Param::Trim,
            ["pan"] => Param::Pan,
            ["mute"] => Param::Mute,
            ["mono"] => Param::Mono,
            ["record"] => Param::Record,
            ["eq"] => Param::Eq,
            ["eq", b, f] => Param::Band {
                band: slot(b, BANDS).ok_or_else(bad)?,
                f: *BandField::ALL.iter().find(|x| x.name() == *f).ok_or_else(bad)?,
            },
            ["solo"] if strip => Param::Solo,
            ["gate"] if strip => Param::Gate,
            ["comp"] if strip => Param::Comp,
            ["gate", k] if strip => Param::Dyn(DynSetting::Gate(dyn_of(&GATE, k).ok_or_else(bad)?)),
            ["comp", k] if strip => Param::Dyn(DynSetting::Comp(dyn_of(&COMP, k).ok_or_else(bad)?)),
            ["limit_db"] if !strip => Param::Limit,
            ["sends", b] => Param::Send(bus_index(b).ok_or_else(bad)?),
            _ => return Err(bad()),
        };
        if let Param::Send(b) = p
            && !strip
            && ch - NUM_STRIPS == b
        {
            return Err(format!("{} cannot feed itself", BUS_IDS[b]));
        }
        Ok(Target::Chan { ch, p })
    }

    fn parse_vban(rest: &[&str]) -> Option<Target> {
        let p = match rest {
            ["on"] => VbanParam::On,
            ["port"] => VbanParam::Port,
            ["in", n, f] => VbanParam::In { slot: slot(n, NUM_IN)?, f: *InField::ALL.iter().find(|x| x.name() == *f)? },
            ["out", n, f] => {
                VbanParam::Out { slot: slot(n, NUM_OUT)?, f: *OutField::ALL.iter().find(|x| x.name() == *f)? }
            }
            _ => return None,
        };
        Some(Target::Vban(p))
    }

    fn parse_player(rest: &[&str]) -> Option<Target> {
        let p = match rest {
            ["playing"] => PlayerParam::Playing,
            ["state"] => PlayerParam::State,
            ["position_s"] => PlayerParam::Position,
            ["take"] => PlayerParam::Take,
            ["sends", b] => PlayerParam::Send(bus_index(b)?),
            _ => return None,
        };
        Some(Target::Player(p))
    }

    /// The path, as `parse` reads it (buses by their current names).
    pub fn path(&self) -> String {
        let (ch, p) = match *self {
            Target::Recording => return "/recorder/recording".into(),
            Target::Vban(p) => {
                return match p {
                    VbanParam::On => "/vban/on".into(),
                    VbanParam::Port => "/vban/port".into(),
                    VbanParam::In { slot, f } => format!("/vban/in/{}/{}", slot + 1, f.name()),
                    VbanParam::Out { slot, f } => format!("/vban/out/{}/{}", slot + 1, f.name()),
                };
            }
            Target::Player(p) => {
                return match p {
                    PlayerParam::Playing => "/player/playing".into(),
                    PlayerParam::State => "/player/state".into(),
                    PlayerParam::Position => "/player/position_s".into(),
                    PlayerParam::Take => "/player/take".into(),
                    PlayerParam::Send(b) => format!("/player/sends/{}", BUS_IDS[b]),
                };
            }
            Target::Chan { ch, p } => (ch, p),
        };
        let mut o = if ch < NUM_STRIPS {
            format!("/strips/{}/", ch + 1)
        } else {
            format!("/buses/{}/", BUS_IDS[ch - NUM_STRIPS])
        };
        let tail = match p {
            Param::Gain => "gain_db",
            Param::Trim => "trim_db",
            Param::Pan => "pan",
            Param::Mute => "mute",
            Param::Mono => "mono",
            Param::Solo => "solo",
            Param::Record => "record",
            Param::Eq => "eq",
            Param::Gate => "gate",
            Param::Comp => "comp",
            Param::Limit => "limit_db",
            Param::Band { band, f } => {
                let _ = write!(o, "eq/{}/{}", band + 1, f.name());
                return o;
            }
            Param::Dyn(s) => {
                let which = if matches!(s, DynSetting::Gate(_)) { "gate" } else { "comp" };
                let _ = write!(o, "{which}/{}", dyn_name(dyn_spec(s)));
                return o;
            }
            Param::Send(b) => {
                let _ = write!(o, "sends/{}", BUS_IDS[b]);
                return o;
            }
        };
        o.push_str(tail);
        o
    }

    /// Every target, in a stable order (`/api/v1/paths`, the MIDI menu).
    pub fn all() -> Vec<Target> {
        let mut v = Vec::new();
        for ch in 0..NUM_STRIPS + NUM_BUSES {
            let strip = ch < NUM_STRIPS;
            let mut ps = vec![Param::Gain, Param::Trim, Param::Pan, Param::Mute, Param::Mono];
            if strip {
                ps.push(Param::Solo);
            }
            ps.extend([Param::Record, Param::Eq]);
            for band in 0..BANDS {
                ps.extend(BandField::ALL.map(|f| Param::Band { band, f }));
            }
            if strip {
                ps.push(Param::Gate);
                ps.extend((0..GATE.len()).map(|i| Param::Dyn(DynSetting::Gate(i))));
                ps.push(Param::Comp);
                ps.extend((0..COMP.len()).map(|i| Param::Dyn(DynSetting::Comp(i))));
            } else {
                ps.push(Param::Limit);
            }
            ps.extend((0..NUM_BUSES).filter(|&b| strip || b != ch - NUM_STRIPS).map(Param::Send));
            v.extend(ps.into_iter().map(|p| Target::Chan { ch, p }));
        }
        v.push(Target::Recording);
        v.extend([VbanParam::On, VbanParam::Port].map(Target::Vban));
        for slot in 0..NUM_IN {
            v.extend(InField::ALL.map(|f| Target::Vban(VbanParam::In { slot, f })));
        }
        for slot in 0..NUM_OUT {
            v.extend(OutField::ALL.map(|f| Target::Vban(VbanParam::Out { slot, f })));
        }
        v.extend(
            [PlayerParam::Playing, PlayerParam::State, PlayerParam::Position, PlayerParam::Take]
                .into_iter()
                .chain((0..NUM_BUSES).map(PlayerParam::Send))
                .map(Target::Player),
        );
        v
    }

    pub fn kind(&self) -> Kind {
        let num = |min: f32, max: f32, step: f32, unit: &'static str| Kind::Number { min, max, step, unit };
        match *self {
            Target::Recording => Kind::Flag,
            Target::Chan { p, .. } => match p {
                Param::Gain => num(MIN_GAIN_DB, MAX_GAIN_DB, 0.1, "dB"),
                Param::Trim => num(MIN_TRIM_DB, MAX_TRIM_DB, 0.1, "dB"),
                Param::Pan => num(-1.0, 1.0, 0.01, ""),
                Param::Limit => num(LIMIT_MIN_DB, LIMIT_OFF_DB, 0.1, "dB"),
                Param::Band { f, .. } => match f {
                    BandField::On => Kind::Flag,
                    BandField::Type => Kind::Choice(&EQ_TYPES),
                    BandField::Freq => num(eq::MIN_FREQ, eq::MAX_FREQ, 1.0, "Hz"),
                    BandField::Gain => num(eq::MIN_GAIN_DB, eq::MAX_GAIN_DB, 0.1, "dB"),
                    BandField::Q => num(eq::MIN_Q, eq::MAX_Q, 0.01, ""),
                },
                Param::Dyn(s) => {
                    let s = dyn_spec(s);
                    num(s.min, s.max, 0.1, s.unit)
                }
                _ => Kind::Flag,
            },
            Target::Vban(p) => match p {
                VbanParam::On => Kind::Flag,
                VbanParam::Port => num(1.0, 65535.0, 1.0, ""),
                VbanParam::In { f, .. } => match f {
                    InField::On => Kind::Flag,
                    InField::Name | InField::From => Kind::Text,
                    InField::Strip => num(1.0, NUM_STRIPS as f32, 1.0, ""),
                    InField::Buffer => num(0.0, MAX_BUFFER_MS as f32, 1.0, "ms"),
                },
                VbanParam::Out { f, .. } => match f {
                    OutField::On => Kind::Flag,
                    OutField::Name | OutField::To => Kind::Text,
                    OutField::Port => num(1.0, 65535.0, 1.0, ""),
                    OutField::Source => Kind::Choice(&SOURCES),
                    OutField::Format => Kind::Choice(&FORMATS),
                    OutField::Channels => num(1.0, 2.0, 1.0, ""),
                },
            },
            Target::Player(p) => match p {
                PlayerParam::Playing | PlayerParam::Send(_) => Kind::Flag,
                PlayerParam::State => Kind::Choice(&PLAY_STATES),
                PlayerParam::Position => num(0.0, MAX_POS_S, 0.01, "s"),
                PlayerParam::Take => Kind::Text,
            },
        }
    }

    /// A number that may fade: a level or a sound setting. The VBAN settings
    /// and the player's position are numbers that only jump (a fade would
    /// reopen a socket or seek a hundred times a second).
    pub fn fades(&self) -> bool {
        matches!(self.kind(), Kind::Number { .. })
            && !matches!(self, Target::Vban(_) | Target::Player(PlayerParam::Position))
    }

    /// Does changing it over HTTP need the token (`token.rs`)? Recording, and
    /// sending audio over the network: VBAN on or off and every setting of an
    /// outgoing stream (where it goes, what it sends). The rest are mixer
    /// controls. (MIDI writes in this process and needs none.)
    pub fn needs_token(&self) -> bool {
        matches!(self, Target::Recording | Target::Vban(VbanParam::On | VbanParam::Out { .. }))
    }

    /// A MIDI control can drive it: an on/off value, or a number that fades.
    pub fn midi(&self) -> bool {
        self.kind() == Kind::Flag || self.fades()
    }

    /// A value made valid for this target: clamped and rounded (a flag: 0 or
    /// 1; a choice: an index of its list).
    pub fn fit(&self, v: f32) -> f32 {
        if !v.is_finite() {
            return self.read_default();
        }
        let (step, min, max) = match self.kind() {
            Kind::Flag => return (v >= 0.5) as u8 as f32,
            Kind::Choice(l) => return v.round().clamp(0.0, (l.len() - 1) as f32),
            Kind::Text => return 0.0,
            Kind::Number { step, min, max, .. } => (step, min, max),
        };
        let Target::Chan { p, .. } = *self else { return round_to(v, step).clamp(min, max) };
        match p {
            Param::Gain => clamp_db(round_to(v, step)),
            Param::Trim => clamp_trim(round_to(v, step)),
            Param::Pan => clamp_pan(round_to(v, step)),
            Param::Limit => clamp_limit(round_to(v, step)),
            Param::Band { f: BandField::Freq, .. } => eq::round_freq(v.clamp(min, max)),
            Param::Band { f: BandField::Gain, .. } => eq::round_gain(v.clamp(min, max)),
            Param::Band { f: BandField::Q, .. } => eq::round_q(v.clamp(min, max)),
            Param::Dyn(s) => dyn_spec(s).clamp(v),
            _ => round_to(v, step).clamp(min, max),
        }
    }

    fn read_default(&self) -> f32 {
        match (*self, self.kind()) {
            (Target::Chan { p: Param::Limit, .. }, _) => LIMIT_OFF_DB,
            (Target::Chan { p: Param::Dyn(s), .. }, _) => dyn_spec(s).def,
            (Target::Chan { p: Param::Band { band, f: BandField::Freq }, .. }, _) => DEFAULT_BANDS[band].freq,
            (Target::Chan { p: Param::Band { band, f: BandField::Q }, .. }, _) => DEFAULT_BANDS[band].q,
            (_, Kind::Number { min, max, .. }) => 0.0f32.clamp(min, max),
            _ => 0.0,
        }
    }

    /// Frequencies and Qs move on a log scale (an octave is the same travel
    /// everywhere), as on the page's EQ.
    fn log_scale(&self) -> bool {
        matches!(self, Target::Chan { p: Param::Band { f: BandField::Freq | BandField::Q, .. }, .. })
    }

    /// The value as a fader position, 0 to 1 (the gain on the page's taper,
    /// frequencies and Qs on a log scale, the rest straight across its range;
    /// a flag is 0 or 1).
    pub fn norm_of(&self, v: f32) -> f32 {
        match (self, self.kind()) {
            (Target::Chan { p: Param::Gain, .. }, _) => db_to_pos(v),
            (_, Kind::Number { min, max, .. }) if self.log_scale() => {
                ((v.clamp(min, max) / min).ln() / (max / min).ln()).clamp(0.0, 1.0)
            }
            (_, Kind::Number { min, max, .. }) => ((v - min) / (max - min)).clamp(0.0, 1.0),
            (_, Kind::Choice(l)) => (v / (l.len() - 1).max(1) as f32).clamp(0.0, 1.0),
            (_, Kind::Flag) => (v >= 0.5) as u8 as f32,
            (_, Kind::Text) => 0.0,
        }
    }

    /// A fader position (0 to 1) as a value, [`fit`](Self::fit) for the target.
    pub fn value_at(&self, n: f32) -> f32 {
        let n = if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 };
        self.fit(match (self, self.kind()) {
            (Target::Chan { p: Param::Gain, .. }, _) => pos_to_db(n),
            (_, Kind::Number { min, max, .. }) if self.log_scale() => min * (max / min).powf(n),
            (_, Kind::Number { min, max, .. }) => min + n * (max - min),
            (_, Kind::Choice(l)) => n * (l.len() - 1) as f32,
            (_, Kind::Flag | Kind::Text) => n,
        })
    }

    /// The value now (text values read as 0: [`text`](Self::text) has them).
    pub fn read(&self, hub: &Hub) -> f32 {
        let flag = |b: bool| b as u8 as f32;
        let (ch, p) = match *self {
            Target::Recording => return flag(hub.recorder.is_recording()),
            Target::Vban(p) => {
                let c = hub.vban.config();
                return match p {
                    VbanParam::On => flag(c.enabled),
                    VbanParam::Port => c.port as f32,
                    VbanParam::In { slot, f } => {
                        let s = &c.ins[slot];
                        match f {
                            InField::On => flag(s.on),
                            InField::Strip => (s.strip + 1) as f32,
                            InField::Buffer => s.buffer_ms as f32,
                            InField::Name | InField::From => 0.0,
                        }
                    }
                    VbanParam::Out { slot, f } => {
                        let s = &c.outs[slot];
                        match f {
                            OutField::On => flag(s.on),
                            OutField::Port => s.port as f32,
                            OutField::Source => s.source as f32,
                            OutField::Format => {
                                SampleFormat::SEND.iter().position(|x| *x == s.format).unwrap_or(0) as f32
                            }
                            OutField::Channels => s.channels as f32,
                            OutField::Name | OutField::To => 0.0,
                        }
                    }
                };
            }
            Target::Player(p) => {
                return match p {
                    PlayerParam::Playing => flag(hub.player.status().state == PlayState::Playing),
                    PlayerParam::State => match hub.player.status().state {
                        PlayState::Stopped => 0.0,
                        PlayState::Playing => 1.0,
                        PlayState::Paused => 2.0,
                    },
                    PlayerParam::Position => hub.player.status().pos as f32,
                    PlayerParam::Send(b) => flag(hub.player.routes() & (1 << b) != 0),
                    PlayerParam::Take => 0.0,
                };
            }
            Target::Chan { ch, p } => (ch, p),
        };
        let sh = &*hub.shared;
        /// The channel's controls: a strip or a bus, never both or neither.
        #[derive(Clone, Copy)]
        enum Chan<'a> {
            Strip(&'a StripParams),
            Bus(&'a BusParams),
        }
        let c = if ch < NUM_STRIPS { Chan::Strip(&sh.strips[ch]) } else { Chan::Bus(&sh.buses[ch - NUM_STRIPS]) };
        // A field both kinds have, read the same way.
        macro_rules! either {
            ($x:ident => $e:expr) => {
                match c {
                    Chan::Strip($x) => $e,
                    Chan::Bus($x) => $e,
                }
            };
        }
        let strip = match c {
            Chan::Strip(s) => Some(s),
            Chan::Bus(_) => None,
        };
        match p {
            Param::Gain => either!(x => x.gain_db.load()),
            Param::Trim => either!(x => x.trim_db.load()),
            Param::Pan => either!(x => x.pan.load()),
            Param::Mute => flag(either!(x => x.mute.load(SeqCst))),
            Param::Mono => flag(either!(x => x.mono.load(SeqCst))),
            Param::Solo => flag(strip.is_some_and(|s| s.solo.load(SeqCst))),
            Param::Record => flag(sh.rec_links.load(SeqCst) & (1 << ch) != 0),
            Param::Eq => flag(sh.eq(ch).on.load(SeqCst)),
            Param::Band { band, f } => {
                let b = sh.eq(ch).bands[band].load();
                match f {
                    BandField::On => flag(b.on),
                    BandField::Type => b.kind as u8 as f32,
                    BandField::Freq => b.freq,
                    BandField::Gain => b.gain_db,
                    BandField::Q => b.q,
                }
            }
            Param::Gate => flag(strip.is_some_and(|s| s.dynamics.gate_on.load(SeqCst))),
            Param::Comp => flag(strip.is_some_and(|s| s.dynamics.comp_on.load(SeqCst))),
            Param::Dyn(d) => strip.map_or(dyn_spec(d).def, |s| match d {
                DynSetting::Gate(i) => s.dynamics.gate[i].load(),
                DynSetting::Comp(i) => s.dynamics.comp[i].load(),
            }),
            Param::Limit => match c {
                Chan::Bus(b) => b.limit.load(),
                Chan::Strip(_) => LIMIT_OFF_DB,
            },
            Param::Send(bus) => flag(either!(x => x.routes.load(SeqCst)) & (1 << bus) != 0),
        }
    }

    /// A text value now (`None`: there is none, as with no take loaded).
    pub fn text(&self, hub: &Hub) -> Option<String> {
        match *self {
            Target::Vban(VbanParam::In { slot, f }) => {
                let s = &hub.vban.config().ins[slot];
                match f {
                    InField::Name => Some(s.name.clone()),
                    InField::From => Some(s.from.as_string()),
                    _ => None,
                }
            }
            Target::Vban(VbanParam::Out { slot, f }) => {
                let s = &hub.vban.config().outs[slot];
                match f {
                    OutField::Name => Some(s.name.clone()),
                    OutField::To => Some(s.to.map(|ip| ip.to_string()).unwrap_or_default()),
                    _ => None,
                }
            }
            Target::Player(PlayerParam::Take) => hub.player.status().take,
            _ => None,
        }
    }

    /// The value now, as JSON: `true`/`false`, a number, a choice's name, text
    /// (or `null`).
    pub fn value_json(&self, hub: &Hub) -> String {
        let mut o = String::new();
        match self.kind() {
            Kind::Text => match self.text(hub) {
                Some(s) => jstr(&mut o, &s),
                None => o.push_str("null"),
            },
            _ => number_json(&mut o, *self, self.read(hub)),
        }
        o
    }

    /// A choice's index for `name` (read as `/api/cmd` reads it: the EQ
    /// types' other names, the old bus names, `16` for `int16` ...).
    fn choice(&self, name: &str) -> Result<f32, String> {
        let i = match *self {
            Target::Chan { p: Param::Band { f: BandField::Type, .. }, .. } => eq::Kind::parse(name)? as usize,
            Target::Vban(VbanParam::Out { f: OutField::Source, .. }) => vban::parse_source(name)?,
            Target::Vban(VbanParam::Out { f: OutField::Format, .. }) => {
                let f = SampleFormat::parse_send(name)?;
                SampleFormat::SEND.iter().position(|x| *x == f).ok_or("not a format VBAN sends")?
            }
            Target::Player(PlayerParam::State) => PLAY_STATES
                .iter()
                .position(|s| *s == name)
                .ok_or_else(|| format!("unknown player state {name:?} (stopped, playing or paused)"))?,
            _ => return Err(format!("{} is not one of a list", self.path())),
        };
        Ok(i as f32)
    }

    /// The command that sets a text target to `s`, checked as `/api/cmd`
    /// checks it.
    fn text_cmd(&self, s: &str) -> Result<Cmd, String> {
        let name = |s: &str| {
            let s = s.trim();
            vban::proto::validate_name(s).map(|_| s.to_string())
        };
        let vin = |slot, name, from| Cmd::VbanIn { slot, name, from, strip: None, buffer_ms: None, on: None };
        let vout = |slot, name, to| Cmd::VbanOut {
            slot,
            name,
            to,
            port: None,
            source: None,
            format: None,
            channels: None,
            on: None,
        };
        Ok(match *self {
            Target::Vban(VbanParam::In { slot, f: InField::Name }) => vin(slot, Some(name(s)?), None),
            Target::Vban(VbanParam::In { slot, f: InField::From }) => vin(slot, None, Some(FromAddr::parse(s)?)),
            Target::Vban(VbanParam::Out { slot, f: OutField::Name }) => vout(slot, Some(name(s)?), None),
            Target::Vban(VbanParam::Out { slot, f: OutField::To }) => vout(slot, None, Some(vban::parse_to(s)?)),
            Target::Player(PlayerParam::Take) => {
                if s.is_empty() || s.chars().count() > MAX_KEY || s.chars().any(char::is_control) {
                    return Err(format!("a take's key is 1 to {MAX_KEY} characters (from /api/takes)"));
                }
                Cmd::Take { key: s.to_string() }
            }
            _ => return Err(format!("{} is {}, not text", self.path(), self.kind().what())),
        })
    }

    /// The command that sets the target to `v` (already [`fit`](Self::fit));
    /// `None` for a text target.
    fn cmd(&self, v: f32) -> Option<Cmd> {
        let on = v >= 0.5;
        Some(match *self {
            Target::Recording => Cmd::Rec { on },
            Target::Vban(p) => match p {
                VbanParam::On => Cmd::Vban { on },
                VbanParam::Port => Cmd::VbanPort { port: v as u16 },
                VbanParam::In { slot, f } => {
                    let (mut on_, mut strip, mut buffer_ms) = (None, None, None);
                    match f {
                        InField::On => on_ = Some(on),
                        InField::Strip => strip = Some(v as usize - 1),
                        InField::Buffer => buffer_ms = Some(v as u32),
                        InField::Name | InField::From => return None,
                    }
                    Cmd::VbanIn { slot, name: None, from: None, strip, buffer_ms, on: on_ }
                }
                VbanParam::Out { slot, f } => {
                    let (mut on_, mut port, mut source, mut format, mut channels) = (None, None, None, None, None);
                    match f {
                        OutField::On => on_ = Some(on),
                        OutField::Port => port = Some(v as u16),
                        OutField::Source => source = Some(v as usize),
                        OutField::Format => format = Some(SampleFormat::SEND[v as usize]),
                        OutField::Channels => channels = Some(v as u8),
                        OutField::Name | OutField::To => return None,
                    }
                    Cmd::VbanOut { slot, name: None, to: None, port, source, format, channels, on: on_ }
                }
            },
            Target::Player(p) => match p {
                PlayerParam::Playing if on => Cmd::Play { from: None, key: None },
                PlayerParam::Playing => Cmd::Pause,
                PlayerParam::State => match v as usize {
                    1 => Cmd::Play { from: None, key: None },
                    2 => Cmd::Pause,
                    _ => Cmd::Stop,
                },
                PlayerParam::Position => Cmd::Seek { to: f64::from(v) },
                PlayerParam::Send(bus) => Cmd::PlayTo { bus, on },
                PlayerParam::Take => return None,
            },
            Target::Chan { ch, p } => match p {
                Param::Gain => Cmd::Gain { ch, db: v },
                Param::Trim => Cmd::Trim { ch, db: v },
                Param::Pan => Cmd::Pan { ch, pan: v },
                Param::Mute => Cmd::Mute { ch, on },
                Param::Mono => Cmd::Mono { ch, on },
                Param::Solo => Cmd::Solo { strip: ch, on },
                Param::Record => Cmd::Link { ch, on },
                Param::Eq => Cmd::Eq { ch, on },
                Param::Band { band, f } => {
                    let mut c = (None, None, None, None, None);
                    match f {
                        BandField::On => c.0 = Some(on),
                        BandField::Type => c.1 = Some(eq::Kind::from_u8(v as u8)),
                        BandField::Freq => c.2 = Some(v),
                        BandField::Gain => c.3 = Some(v),
                        BandField::Q => c.4 = Some(v),
                    }
                    Cmd::Band { ch, band, on: c.0, kind: c.1, freq: c.2, gain_db: c.3, q: c.4 }
                }
                Param::Gate => Cmd::Gate { strip: ch, on },
                Param::Comp => Cmd::Comp { strip: ch, on },
                Param::Dyn(setting) => Cmd::DynSet { strip: ch, setting, value: v },
                Param::Limit => Cmd::Limit { bus: ch - NUM_STRIPS, db: v },
                Param::Send(bus) => Cmd::Route { ch, bus, on },
            },
        })
    }

    /// Set the value (made valid first) and answer with what the page shows.
    /// A fade running on this target stops: the last change wins.
    pub fn write(&self, hub: &Hub, v: f32) -> Result<String, ApiError> {
        hub.fades.cancel(*self);
        self.write_raw(hub, v)
    }

    fn write_raw(&self, hub: &Hub, v: f32) -> Result<String, ApiError> {
        let v = self.fit(v);
        // Unchanged: nothing to do (the recorder would refuse a second start,
        // and pausing a stopped player would load a take).
        let same = |yes: &str, no: &str| -> String { if v >= 0.5 { yes } else { no }.into() };
        match self {
            Target::Recording if self.read(hub) == v => return Ok(same("recording", "not recording")),
            Target::Player(PlayerParam::Playing) if self.read(hub) == v => return Ok(same("playing", "not playing")),
            _ => {}
        }
        let cmd = self.cmd(v).ok_or_else(|| format!("{} is text: set it with text", self.path()))?;
        apply(hub, cmd)
    }

    /// Set a text value (checked as `/api/cmd` checks it).
    pub fn write_text(&self, hub: &Hub, s: &str) -> Result<String, ApiError> {
        apply(hub, self.text_cmd(s)?)
    }
}

/// One command of `POST /api/v1/command`, checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A flag, a number or a choice (its index).
    Set {
        t: Target,
        v: f32,
    },
    /// A text value (already checked).
    SetText {
        t: Target,
        text: String,
    },
    Toggle {
        t: Target,
    },
    /// Add `by` (numbers only).
    Step {
        t: Target,
        by: f32,
    },
    /// Move to `to` over `ms` milliseconds (numbers that fade only).
    Fade {
        t: Target,
        to: f32,
        ms: u32,
    },
    /// Load a saved scene.
    Recall {
        scene: String,
    },
}

fn target_of(o: &Value) -> Result<Target, String> {
    let p = o.get("path").ok_or("missing \"path\"")?;
    Target::parse(p.as_str().ok_or("\"path\" must be text")?)
}

fn number_of(o: &Value, k: &str) -> Result<f32, String> {
    match o.get(k) {
        None => Err(format!("missing {k:?}")),
        Some(Value::Num(n)) => Ok(*n as f32),
        Some(v) => Err(format!("{k:?} must be a number, not {}", v.kind())),
    }
}

fn numbers_only(t: Target, what: &str) -> Result<(), String> {
    match t.kind() {
        Kind::Number { .. } => Ok(()),
        k => Err(format!("{what} needs a number, and {} is {}", t.path(), k.what())),
    }
}

impl Command {
    /// Over HTTP, it needs the token: its path does ([`Target::needs_token`]).
    pub fn needs_token(&self) -> bool {
        match self {
            Command::Set { t, .. }
            | Command::SetText { t, .. }
            | Command::Toggle { t }
            | Command::Step { t, .. }
            | Command::Fade { t, .. } => t.needs_token(),
            Command::Recall { .. } => false,
        }
    }

    /// One command object: `{"cmd": "set", "path": "...", "value": ...}`.
    pub fn parse(o: &Value) -> Result<Command, String> {
        if !matches!(o, Value::Obj(_)) {
            return Err(format!("a command is an object, not {}", o.kind()));
        }
        let cmd = o.get("cmd").ok_or("missing \"cmd\"")?.as_str().ok_or("\"cmd\" must be text")?;
        Ok(match cmd {
            "set" => {
                let t = target_of(o)?;
                let v = match (o.get("value"), t.kind()) {
                    (None, _) => return Err("missing \"value\"".into()),
                    (Some(Value::Bool(b)), Kind::Flag) => *b as u8 as f32,
                    (Some(Value::Num(n)), Kind::Flag) if *n == 0.0 || *n == 1.0 => *n as f32,
                    (Some(Value::Num(n)), Kind::Number { .. }) => *n as f32,
                    (Some(Value::Str(s)), Kind::Choice(_)) => t.choice(s)?,
                    (Some(Value::Str(s)), Kind::Text) => {
                        t.text_cmd(s)?;
                        return Ok(Command::SetText { t, text: s.clone() });
                    }
                    (Some(v), Kind::Flag) => {
                        return Err(format!("{} is on or off: give true or false, not {}", t.path(), v.kind()));
                    }
                    (Some(v), Kind::Number { .. }) => {
                        return Err(format!("{} needs a number, not {}", t.path(), v.kind()));
                    }
                    (Some(v), Kind::Choice(l)) => {
                        return Err(format!(
                            "{} is one of {}: give its name, not {}",
                            t.path(),
                            l.join(", "),
                            v.kind()
                        ));
                    }
                    (Some(v), Kind::Text) => return Err(format!("{} needs text, not {}", t.path(), v.kind())),
                };
                Command::Set { t, v }
            }
            "toggle" => {
                let t = target_of(o)?;
                if t.kind() != Kind::Flag {
                    return Err(format!("only on/off values toggle, and {} is {}", t.path(), t.kind().what()));
                }
                Command::Toggle { t }
            }
            "step" => {
                let t = target_of(o)?;
                numbers_only(t, "step")?;
                Command::Step { t, by: number_of(o, "by")? }
            }
            "fade" => {
                let t = target_of(o)?;
                numbers_only(t, "fade")?;
                if !t.fades() {
                    return Err(format!("{} does not fade (it is a setting that jumps): use set or step", t.path()));
                }
                let to = number_of(o, "to")?;
                let ms = number_of(o, "ms")?;
                if !(0.0..=MAX_FADE_MS as f32).contains(&ms) {
                    return Err(format!("\"ms\" must be 0 to {MAX_FADE_MS}"));
                }
                Command::Fade { t, to, ms: ms as u32 }
            }
            "recall" => {
                let s = o.get("scene").ok_or("missing \"scene\"")?.as_str().ok_or("\"scene\" must be text")?;
                Command::Recall { scene: scenes::validate_name(s)?.to_string() }
            }
            c => return Err(format!("unknown cmd {c:?} (set, toggle, step, fade, recall)")),
        })
    }

    /// Do it. Answers the page's short text and, for a value, the value now.
    pub fn run(&self, hub: &Hub) -> Outcome {
        let done = |t: Target, text: String| Ok((text, Some((t, t.value_json(hub)))));
        match self {
            Command::Set { t, v } => done(*t, t.write(hub, *v)?),
            Command::SetText { t, text } => done(*t, t.write_text(hub, text)?),
            Command::Toggle { t } => done(*t, t.write(hub, 1.0 - t.read(hub))?),
            Command::Step { t, by } => done(*t, t.write(hub, t.read(hub) + by)?),
            Command::Fade { t, to, ms } => {
                let to = t.fit(*to);
                if *ms == 0 {
                    return done(*t, t.write(hub, to)?);
                }
                hub.fades.start(*t, t.read(hub), to, *ms);
                done(*t, format!("fading to {to} over {ms} ms"))
            }
            Command::Recall { scene } => {
                hub.fades.cancel_all();
                Ok((apply(hub, Cmd::Scene { name: scene.clone(), act: SceneAct::Load })?, None))
            }
        }
    }
}

/// What one command did: the page's text and, for a value, the path and its
/// value now as JSON.
pub type Outcome = Result<(String, Option<(Target, String)>), ApiError>;

fn result_json(o: &mut String, r: &Outcome) {
    match r {
        Ok((text, val)) => {
            o.push_str("{\"ok\":true,\"text\":");
            jstr(o, text);
            if let Some((t, v)) = val {
                o.push_str(",\"path\":");
                jstr(o, &t.path());
                o.push_str(",\"value\":");
                o.push_str(v);
            }
            o.push('}');
        }
        Err(e) => {
            o.push_str("{\"ok\":false,\"error\":");
            jstr(o, &e.msg);
            o.push('}');
        }
    }
}

/// A flag, number or choice as JSON (`true`, `-6.5`, `"bell"`).
fn number_json(o: &mut String, t: Target, v: f32) {
    match t.kind() {
        Kind::Flag => o.push_str(if v >= 0.5 { "true" } else { "false" }),
        Kind::Number { step, .. } => jnum(
            o,
            v as f64,
            if step < 0.1 {
                2
            } else if step < 1.0 {
                1
            } else {
                0
            },
        ),
        Kind::Choice(l) => jstr(o, l.get(v as usize).copied().unwrap_or("")),
        Kind::Text => o.push_str("null"),
    }
}

/// `POST /api/v1/command`: one command object, or a list of them.
///
/// A list is checked whole before anything is done, so a typo in its third
/// command changes nothing; then each runs in order, and the answer has each
/// one's result. One command answers its result alone, or its error.
///
/// A command on a path with effects outside the mixer ([`Target::needs_token`])
/// is refused unless the request carried the token (`token_ok`); a batch is
/// refused whole, before anything in it is done.
pub fn command_over_http(hub: &Hub, body: &str, token_ok: bool) -> Result<String, ApiError> {
    let v = json::parse(body)?;
    let allowed = |c: &Command| if c.needs_token() && !token_ok { Err(hub.token_refused()) } else { Ok(()) };
    if let Value::Arr(list) = &v {
        if list.is_empty() || list.len() > MAX_BATCH {
            return Err(format!("a batch holds 1 to {MAX_BATCH} commands").into());
        }
        let cmds = list
            .iter()
            .enumerate()
            .map(|(i, c)| Command::parse(c).map_err(|e| format!("command {}: {e}", i + 1)))
            .collect::<Result<Vec<_>, _>>()?;
        cmds.iter().try_for_each(allowed)?;
        let results: Vec<_> = cmds.iter().map(|c| c.run(hub)).collect();
        let mut o = format!("{{\"ok\":{},\"results\":[", results.iter().all(Result::is_ok));
        for (i, r) in results.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            result_json(&mut o, r);
        }
        o.push_str("]}");
        return Ok(o);
    }
    let c = Command::parse(&v)?;
    allowed(&c)?;
    let r = c.run(hub);
    if let Err(e) = r {
        return Err(e);
    }
    let mut o = String::new();
    result_json(&mut o, &r);
    Ok(o)
}

/// `GET /api/v1/state`: the state of `/api/state` with the API's version and
/// `rev`, which goes up with every new state.
pub fn state(rev: u64, json: &str) -> String {
    let rest = json.trim_start().strip_prefix('{').unwrap_or("}");
    let sep = if rest.trim_start().starts_with('}') { "" } else { "," };
    format!("{{\"api\":{VERSION},\"rev\":{rev}{sep}{rest}")
}

/// `GET /api/v1/paths`: every path, its kind, range and value now, and a label
/// with the channel's name (`names`: the 11 channel names, may be empty).
pub fn paths_json(hub: &Hub, names: &[String]) -> String {
    let mut o = format!("{{\"api\":{VERSION},\"paths\":[");
    for (i, t) in Target::all().iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"path\":");
        jstr(&mut o, &t.path());
        o.push_str(",\"label\":");
        jstr(&mut o, &label(*t, names));
        match t.kind() {
            Kind::Flag => o.push_str(",\"kind\":\"flag\""),
            Kind::Number { min, max, step, unit } => {
                let _ = write!(o, ",\"kind\":\"number\",\"min\":{min},\"max\":{max},\"step\":{step},\"unit\":");
                jstr(&mut o, unit);
                let _ = write!(o, ",\"fade\":{}", t.fades());
            }
            Kind::Choice(l) => {
                o.push_str(",\"kind\":\"choice\",\"choices\":[");
                for (k, c) in l.iter().enumerate() {
                    if k > 0 {
                        o.push(',');
                    }
                    jstr(&mut o, c);
                }
                o.push(']');
            }
            Kind::Text => o.push_str(",\"kind\":\"text\""),
        }
        o.push_str(",\"value\":");
        o.push_str(&t.value_json(hub));
        let _ = write!(o, ",\"midi\":{}}}", t.midi());
    }
    o.push_str("]}");
    o
}

/// "Mic: fader", "B1: send to V1", "Recorder: recording", "VBAN in 1: name".
pub fn label(t: Target, names: &[String]) -> String {
    let (ch, p) = match t {
        Target::Chan { ch, p } => (ch, p),
        Target::Recording => return "Recorder: recording".into(),
        Target::Vban(p) => {
            return match p {
                VbanParam::On => "VBAN: on".into(),
                VbanParam::Port => "VBAN: port".into(),
                VbanParam::In { slot, f } => format!("VBAN in {}: {}", slot + 1, f.label()),
                VbanParam::Out { slot, f } => format!("VBAN out {}: {}", slot + 1, f.label()),
            };
        }
        Target::Player(p) => {
            return match p {
                PlayerParam::Playing => "Player: playing".into(),
                PlayerParam::State => "Player: state".into(),
                PlayerParam::Position => "Player: position".into(),
                PlayerParam::Take => "Player: take".into(),
                PlayerParam::Send(b) => format!("Player: to {}", BUS_IDS[b]),
            };
        }
    };
    let id = if ch < NUM_STRIPS { format!("Strip {}", ch + 1) } else { BUS_IDS[ch - NUM_STRIPS].to_string() };
    let who = match names.get(ch).map(|n| n.trim()).filter(|n| !n.is_empty() && *n != id) {
        Some(n) => format!("{id} ({n})"),
        None => id,
    };
    let what = match p {
        Param::Gain => "fader".to_string(),
        Param::Trim => "gain knob".into(),
        Param::Pan => if ch < NUM_STRIPS { "pan" } else { "balance" }.into(),
        Param::Mute => "mute".into(),
        Param::Mono => "mono".into(),
        Param::Solo => "solo".into(),
        Param::Record => "record".into(),
        Param::Eq => "EQ".into(),
        Param::Band { band, f } => format!("EQ band {} {}", band + 1, f.label()),
        Param::Gate => "gate".into(),
        Param::Comp => "compressor".into(),
        Param::Dyn(s) => {
            let which = if matches!(s, DynSetting::Gate(_)) { "gate" } else { "compressor" };
            format!("{which} {}", dyn_spec(s).label.to_lowercase())
        }
        Param::Limit => "limiter".into(),
        Param::Send(b) => format!("to {}", BUS_IDS[b]),
    };
    format!("{who}: {what}")
}

// ---------------------------------------------------------------------------
// Fades
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Fade {
    t: Target,
    from: f32,
    to: f32,
    start: Instant,
    ms: u32,
}

/// The fades running now. A thread (started with the server) moves each one
/// every [`FADE_TICK`]; it sleeps while there are none.
#[derive(Default)]
pub struct Fades {
    list: Mutex<Vec<Fade>>,
    wake: Condvar,
}

impl Fades {
    fn start(&self, t: Target, from: f32, to: f32, ms: u32) {
        let mut l = lock(&self.list);
        l.retain(|f| f.t != t);
        l.push(Fade { t, from, to, start: Instant::now(), ms });
        drop(l);
        self.wake.notify_all();
    }

    pub fn cancel(&self, t: Target) {
        lock(&self.list).retain(|f| f.t != t);
    }

    pub fn cancel_all(&self) {
        lock(&self.list).clear();
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        lock(&self.list).len()
    }

    /// Move every fade to where it should be at `now`; finished ones end on
    /// their target value and are dropped.
    pub fn tick(&self, hub: &Hub, now: Instant) {
        let due: Vec<(Target, f32)> = {
            let mut l = lock(&self.list);
            let mut due = Vec::with_capacity(l.len());
            l.retain(|f| {
                let k = now.saturating_duration_since(f.start).as_secs_f32() * 1000.0 / f.ms as f32;
                let k = k.clamp(0.0, 1.0);
                // evenly along the control: gains on the fader's taper, not
                // in dB; frequencies and Qs on their log scale
                let (a, b) = (f.t.norm_of(f.from), f.t.norm_of(f.to));
                let v = f.t.value_at(a + (b - a) * k);
                due.push((f.t, if k >= 1.0 { f.to } else { v }));
                k < 1.0
            });
            due
        };
        for (t, v) in due {
            let _ = t.write_raw(hub, v);
        }
    }

    /// The fade thread: runs until the hub closes.
    pub fn run(hub: &Hub) {
        loop {
            {
                let l = lock(&hub.fades.list);
                let _l = hub
                    .fades
                    .wake
                    .wait_while(l, |l| l.is_empty() && !hub.is_closing())
                    .unwrap_or_else(|e| e.into_inner());
            }
            if hub.is_closing() {
                return;
            }
            hub.fades.tick(hub, Instant::now());
            std::thread::sleep(FADE_TICK);
        }
    }

    /// Wake the thread to end it.
    pub fn wake(&self) {
        let _l = lock(&self.list);
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_read_back_as_written() {
        for t in Target::all() {
            assert_eq!(Target::parse(&t.path()), Ok(t), "{}", t.path());
        }
        // 6 strips x (5 + solo + record, eq + gate, comp + 5 sends + 6 bands x 5 + 5 gate + 6 comp settings)
        // + 5 buses x (5 + record, eq + limit + 4 sends + 6 bands x 5) + recorder
        // + VBAN (on, port, 8 in x 5, 8 out x 7) + player (playing, state, position, take + 5 sends)
        assert_eq!(Target::all().len(), 6 * (15 + 30 + 11) + 5 * (12 + 30) + 1 + (2 + 40 + 56) + 9);
        let mut seen = std::collections::HashSet::new();
        assert!(Target::all().iter().all(|t| seen.insert(t.path())), "no path twice");
        assert_eq!(Target::parse("/buses/A1/mute"), Ok(Target::Chan { ch: 6, p: Param::Mute }));
        assert_eq!(Target::parse("/strips/2/sends/b1"), Target::parse("/strips/2/sends/B1"));
    }

    #[test]
    fn refuses_paths_that_do_not_exist() {
        for bad in [
            "",
            "strips/1/mute",
            "/strips/0/mute",
            "/strips/7/mute",
            "/strips/x/mute",
            "/strips/1",
            "/strips/1/limit_db",
            "/buses/B1/solo",
            "/buses/B1/gate",
            "/buses/C1/mute",
            "/buses/B1/sends/B1",
            "/strips/1/sends/X9",
            "/strips/1/mute/x",
            "/recorder",
            "/recorder/recording/x",
            "/strips/1/eq/0/gain_db",
            "/strips/1/eq/7/gain_db",
            "/strips/1/eq/1/gain",
            "/strips/1/eq/1",
            "/buses/B1/gate/threshold_db",
            "/strips/1/gate/threshold",
            "/strips/1/comp/ratio_db",
            "/strips/1/gate/ratio",
            "/vban",
            "/vban/in/0/name",
            "/vban/in/9/name",
            "/vban/in/1/to",
            "/vban/out/1/from",
            "/vban/out/1/buffer_ms",
            "/player",
            "/player/position",
            "/player/sends/C1",
        ] {
            assert!(Target::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Target::parse("/buses/V2/sends/V2").unwrap_err(), "V2 cannot feed itself");
    }

    #[test]
    fn values_are_fit_to_their_target() {
        let gain = Target::parse("/strips/1/gain_db").unwrap();
        assert_eq!(gain.fit(-6.54), -6.5);
        assert_eq!(gain.fit(99.0), 12.0);
        assert_eq!(gain.fit(f32::NAN), 0.0);
        let pan = Target::parse("/buses/B1/pan").unwrap();
        assert_eq!(pan.fit(0.333), 0.33);
        let lim = Target::parse("/buses/B1/limit_db").unwrap();
        assert_eq!(lim.fit(-90.0), LIMIT_MIN_DB);
        assert_eq!(lim.fit(f32::NAN), LIMIT_OFF_DB);
        let mute = Target::parse("/buses/B1/mute").unwrap();
        assert_eq!((mute.fit(0.7), mute.fit(0.2)), (1.0, 0.0));
    }

    #[test]
    fn the_gain_follows_the_page_taper() {
        let gain = Target::parse("/strips/1/gain_db").unwrap();
        for (db, pos) in TAPER {
            assert!((gain.norm_of(db) - pos).abs() < 1e-6);
            assert!((gain.value_at(pos) - db).abs() < 0.051);
        }
        assert_eq!(gain.value_at(0.8), 0.0);
        assert_eq!(gain.value_at(-1.0), -60.0);
        let trim = Target::parse("/strips/1/trim_db").unwrap();
        assert_eq!((trim.value_at(0.5), trim.norm_of(24.0)), (0.0, 1.0));
        // every 7-bit step lands on a value that maps back to the same step
        for t in [gain, trim, Target::parse("/strips/1/pan").unwrap()] {
            for s in 0..128 {
                let n = s as f32 / 127.0;
                let back = (t.norm_of(t.value_at(n)) * 127.0).round() as i32;
                assert!((back - s).abs() <= 1, "{} step {s} -> {back}", t.path());
            }
        }
    }

    fn cmd(s: &str) -> Result<Command, String> {
        Command::parse(&json::parse(s).unwrap())
    }

    #[test]
    fn commands_are_checked() {
        let g = Target::parse("/strips/1/gain_db").unwrap();
        let m = Target::parse("/strips/1/mute").unwrap();
        assert_eq!(cmd(r#"{"cmd":"set","path":"/strips/1/gain_db","value":-3}"#), Ok(Command::Set { t: g, v: -3.0 }));
        assert_eq!(cmd(r#"{"cmd":"set","path":"/strips/1/mute","value":true}"#), Ok(Command::Set { t: m, v: 1.0 }));
        assert_eq!(cmd(r#"{"cmd":"set","path":"/strips/1/mute","value":0}"#), Ok(Command::Set { t: m, v: 0.0 }));
        assert_eq!(cmd(r#"{"cmd":"toggle","path":"/strips/1/mute"}"#), Ok(Command::Toggle { t: m }));
        assert_eq!(cmd(r#"{"cmd":"step","path":"/strips/1/gain_db","by":-1.5}"#), Ok(Command::Step { t: g, by: -1.5 }));
        assert_eq!(
            cmd(r#"{"cmd":"fade","path":"/strips/1/gain_db","to":-20,"ms":500}"#),
            Ok(Command::Fade { t: g, to: -20.0, ms: 500 })
        );
        assert_eq!(cmd(r#"{"cmd":"recall","scene":"Live"}"#), Ok(Command::Recall { scene: "Live".into() }));
        for (bad, says) in [
            (r#"[1]"#, "a command is an object"),
            (r#"{"path":"/strips/1/mute"}"#, "missing \"cmd\""),
            (r#"{"cmd":"jump","path":"/strips/1/mute"}"#, "unknown cmd"),
            (r#"{"cmd":"set","value":1}"#, "missing \"path\""),
            (r#"{"cmd":"set","path":"/strips/1/mute"}"#, "missing \"value\""),
            (r#"{"cmd":"set","path":"/strips/1/mute","value":"on"}"#, "is on or off"),
            (r#"{"cmd":"set","path":"/strips/1/mute","value":0.5}"#, "is on or off"),
            (r#"{"cmd":"set","path":"/strips/1/gain_db","value":true}"#, "needs a number"),
            (r#"{"cmd":"toggle","path":"/strips/1/gain_db"}"#, "only on/off values toggle"),
            (r#"{"cmd":"step","path":"/strips/1/mute","by":1}"#, "step needs a number"),
            (r#"{"cmd":"step","path":"/strips/1/gain_db"}"#, "missing \"by\""),
            (r#"{"cmd":"fade","path":"/strips/1/gain_db","to":0,"ms":70000}"#, "\"ms\" must be"),
            (r#"{"cmd":"fade","path":"/strips/1/gain_db","to":"x","ms":5}"#, "must be a number"),
            (r#"{"cmd":"recall","scene":".hidden"}"#, ""),
        ] {
            let e = cmd(if bad.starts_with('[') { "1" } else { bad }).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
    }

    #[test]
    fn the_new_paths_read_as_documented() {
        let t = |s: &str| Target::parse(s).unwrap();
        assert_eq!(t("/strips/2/eq/3/freq_hz"), Target::Chan { ch: 1, p: Param::Band { band: 2, f: BandField::Freq } });
        assert_eq!(t("/buses/b2/eq/6/q"), Target::Chan { ch: 10, p: Param::Band { band: 5, f: BandField::Q } });
        assert_eq!(t("/strips/6/comp/makeup_db"), Target::Chan { ch: 5, p: Param::Dyn(DynSetting::Comp(5)) });
        assert_eq!(t("/strips/1/comp/ratio"), Target::Chan { ch: 0, p: Param::Dyn(DynSetting::Comp(1)) });
        assert_eq!(t("/strips/1/gate/hold_ms"), Target::Chan { ch: 0, p: Param::Dyn(DynSetting::Gate(3)) });
        assert_eq!(t("/vban/out/8/format"), Target::Vban(VbanParam::Out { slot: 7, f: OutField::Format }));
        assert_eq!(t("/player/sends/A1"), Target::Player(PlayerParam::Send(0)));
        // every gate and compressor setting has a path, named after its key and unit
        let names: Vec<String> = GATE.iter().chain(&COMP).map(dyn_name).collect();
        assert_eq!(
            names,
            [
                "threshold_db",
                "range_db",
                "attack_ms",
                "hold_ms",
                "release_ms",
                "threshold_db",
                "ratio",
                "knee_db",
                "attack_ms",
                "release_ms",
                "makeup_db"
            ]
        );
        // the lists are the core's
        assert_eq!(EQ_TYPES.to_vec(), eq::Kind::ALL.map(|k| k.as_str()).to_vec());
        assert_eq!(FORMATS.to_vec(), SampleFormat::SEND.map(|f| f.as_str()).to_vec());
        assert!(SOURCES.iter().enumerate().all(|(i, s)| bananasplit::recorder::source_id(i) == *s));
        assert_eq!(PLAY_STATES, [PlayState::Stopped, PlayState::Playing, PlayState::Paused].map(|s| s.as_str()));
        // what may fade and what MIDI may drive
        assert!(t("/strips/1/eq/1/freq_hz").fades() && t("/strips/1/gate/attack_ms").midi());
        assert!(!t("/vban/port").fades() && !t("/vban/port").midi());
        assert!(!t("/player/position_s").fades() && !t("/player/position_s").midi());
        assert!(t("/player/playing").midi() && t("/vban/in/1/on").midi() && t("/strips/1/eq/1/on").midi());
        assert!(!t("/strips/1/eq/1/type").midi() && !t("/vban/in/1/name").midi());
    }

    #[test]
    fn new_values_are_clamped_and_rounded() {
        let t = |s: &str| Target::parse(s).unwrap();
        let f = t("/strips/1/eq/2/freq_hz");
        assert_eq!((f.fit(5.0), f.fit(99_999.0), f.fit(1000.4)), (20.0, 20_000.0, 1000.0));
        let g = t("/buses/V1/eq/1/gain_db");
        assert_eq!((g.fit(-40.0), g.fit(3.04)), (-24.0, 3.0));
        let q = t("/strips/1/eq/1/q");
        assert_eq!((q.fit(0.0), q.fit(99.0), q.fit(1.414)), (0.1, 30.0, 1.41));
        assert_eq!(q.fit(f32::NAN), DEFAULT_BANDS[0].q);
        let th = t("/strips/1/gate/threshold_db");
        assert_eq!((th.fit(-200.0), th.fit(5.0), th.fit(-12.34)), (-80.0, 0.0, -12.3));
        assert_eq!(th.fit(f32::NAN), GATE[0].def);
        let ratio = t("/strips/1/comp/ratio");
        assert_eq!((ratio.fit(0.5), ratio.fit(40.0)), (1.0, 20.0));
        let port = t("/vban/port");
        assert_eq!((port.fit(0.0), port.fit(70_000.0), port.fit(6980.4)), (1.0, 65535.0, 6980.0));
        let strip = t("/vban/in/1/strip");
        assert_eq!((strip.fit(0.0), strip.fit(9.0)), (1.0, 6.0));
        assert_eq!(t("/vban/in/1/buffer_ms").fit(500.0), 200.0);
        assert_eq!(t("/vban/out/1/channels").fit(7.0), 2.0);
        assert_eq!(t("/player/position_s").fit(-3.0), 0.0);
        assert_eq!(t("/strips/1/eq/1/type").fit(9.0), 5.0);
        // a frequency moves on a log scale: half way is 20 Hz x 1000^0.5
        assert!((f.value_at(0.5) - 632.0).abs() < 1.0, "{}", f.value_at(0.5));
        for t in [f, q, th] {
            for s in 0..128 {
                let n = s as f32 / 127.0;
                let back = (t.norm_of(t.value_at(n)) * 127.0).round() as i32;
                assert!((back - s).abs() <= 1, "{} step {s} -> {back}", t.path());
            }
        }
    }

    #[test]
    fn choices_and_text_are_checked() {
        let t = |s: &str| Target::parse(s).unwrap();
        let ty = t("/strips/1/eq/1/type");
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/strips/1/eq/1/type","value":"notch"}"#),
            Ok(Command::Set { t: ty, v: 5.0 })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/strips/1/eq/1/type","value":"hpf"}"#),
            Ok(Command::Set { t: ty, v: 3.0 })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/vban/out/2/source","value":"A1"}"#),
            Ok(Command::Set { t: t("/vban/out/2/source"), v: 6.0 })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/vban/out/2/format","value":"float32"}"#),
            Ok(Command::Set { t: t("/vban/out/2/format"), v: 3.0 })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/player/state","value":"paused"}"#),
            Ok(Command::Set { t: t("/player/state"), v: 2.0 })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/vban/in/1/from","value":"any"}"#),
            Ok(Command::SetText { t: t("/vban/in/1/from"), text: "any".into() })
        );
        assert_eq!(
            cmd(r#"{"cmd":"set","path":"/player/take","value":"2026-10-21_14-03-05"}"#),
            Ok(Command::SetText { t: t("/player/take"), text: "2026-10-21_14-03-05".into() })
        );
        for (bad, says) in [
            (r#"{"cmd":"set","path":"/strips/1/eq/1/type","value":"comb"}"#, "unknown EQ band type"),
            (r#"{"cmd":"set","path":"/strips/1/eq/1/type","value":2}"#, "is one of bell, lowshelf"),
            (r#"{"cmd":"set","path":"/vban/out/1/source","value":"X9"}"#, "expected a source"),
            (r#"{"cmd":"set","path":"/vban/out/1/format","value":"uint8"}"#, "unknown VBAN format"),
            (r#"{"cmd":"set","path":"/player/state","value":"rewinding"}"#, "unknown player state"),
            (r#"{"cmd":"set","path":"/vban/in/1/from","value":"10.0.0"}"#, "expected the sender's IPv4"),
            (r#"{"cmd":"set","path":"/vban/out/1/to","value":"0.0.0.0"}"#, "cannot receive a VBAN stream"),
            (r#"{"cmd":"set","path":"/vban/in/1/name","value":"a name far too long"}"#, "1-16 ASCII"),
            (r#"{"cmd":"set","path":"/vban/in/1/name","value":5}"#, "needs text"),
            (r#"{"cmd":"set","path":"/player/take","value":""}"#, "a take's key"),
            (r#"{"cmd":"toggle","path":"/strips/1/eq/1/type"}"#, "is one of a list"),
            (r#"{"cmd":"step","path":"/vban/in/1/name","by":1}"#, "step needs a number, and /vban/in/1/name is text"),
            (r#"{"cmd":"fade","path":"/vban/port","to":7000,"ms":100}"#, "does not fade"),
            (r#"{"cmd":"fade","path":"/player/position_s","to":10,"ms":100}"#, "does not fade"),
        ] {
            let e = cmd(bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
        // numbers that only jump still step
        assert_eq!(
            cmd(r#"{"cmd":"step","path":"/player/position_s","by":-10}"#),
            Ok(Command::Step { t: t("/player/position_s"), by: -10.0 })
        );
    }

    /// v1 and `/api/cmd` end in the same `Cmd`, so `apply` cannot tell them apart.
    #[test]
    fn v1_and_the_legacy_ops_make_the_same_cmd() {
        let v1 = |path: &str, v: f32| {
            let t = Target::parse(path).unwrap();
            t.cmd(t.fit(v)).unwrap()
        };
        let text = |path: &str, s: &str| Target::parse(path).unwrap().text_cmd(s).unwrap();
        let legacy = |body: &str| Cmd::parse(body).unwrap();
        for (a, b) in [
            (v1("/strips/1/eq/2/freq_hz", 1000.0), legacy("op=band&ch=0&band=1&f=1000")),
            (v1("/buses/B2/eq/6/gain_db", -4.54), legacy("op=band&ch=10&band=5&g=-4.54")),
            (v1("/strips/3/eq/1/q", 0.707), legacy("op=band&ch=2&band=0&q=0.707")),
            (v1("/strips/3/eq/1/on", 0.0), legacy("op=band&ch=2&band=0&on=0")),
            (v1("/strips/3/eq/4/type", 2.0), legacy("op=band&ch=2&band=3&type=highshelf")),
            (v1("/strips/2/gate/threshold_db", -42.04), legacy("op=dynset&ch=1&key=gate_threshold&v=-42.04")),
            (v1("/strips/6/comp/ratio", 4.0), legacy("op=dynset&ch=5&key=comp_ratio&v=4")),
            (v1("/strips/6/comp/makeup_db", 6.0), legacy("op=dynset&ch=5&key=comp_makeup&v=6")),
            (v1("/vban/on", 1.0), legacy("op=vban&v=1")),
            (v1("/vban/port", 6981.0), legacy("op=vbanport&v=6981")),
            (v1("/vban/in/1/strip", 3.0), legacy("op=vbanin&slot=0&strip=3")),
            (v1("/vban/in/8/buffer_ms", 40.0), legacy("op=vbanin&slot=7&buffer=40")),
            (v1("/vban/in/2/on", 1.0), legacy("op=vbanin&slot=1&on=1")),
            (text("/vban/in/2/name", "Mic"), legacy("op=vbanin&slot=1&name=Mic")),
            (text("/vban/in/2/from", "192.168.1.20"), legacy("op=vbanin&slot=1&from=192.168.1.20")),
            (v1("/vban/out/3/port", 6990.0), legacy("op=vbanout&slot=2&port=6990")),
            (v1("/vban/out/3/source", 9.0), legacy("op=vbanout&slot=2&src=B1")),
            (v1("/vban/out/3/format", 1.0), legacy("op=vbanout&slot=2&format=int24")),
            (v1("/vban/out/3/channels", 1.0), legacy("op=vbanout&slot=2&channels=1")),
            (v1("/vban/out/3/on", 0.0), legacy("op=vbanout&slot=2&on=0")),
            (text("/vban/out/3/name", "Out"), legacy("op=vbanout&slot=2&name=Out")),
            (text("/vban/out/3/to", "10.0.0.2"), legacy("op=vbanout&slot=2&to=10.0.0.2")),
            (v1("/player/state", 1.0), legacy("op=play")),
            (v1("/player/state", 2.0), legacy("op=pause")),
            (v1("/player/state", 0.0), legacy("op=stop")),
            (v1("/player/playing", 1.0), legacy("op=play")),
            (v1("/player/playing", 0.0), legacy("op=pause")),
            (v1("/player/position_s", 12.5), legacy("op=seek&v=12.5")),
            (text("/player/take", "2026-10-21_14-03-05"), legacy("op=take&key=2026-10-21_14-03-05")),
            (v1("/player/sends/B1", 1.0), legacy("op=playto&bus=3&v=1")),
            // and the old paths still do what they did
            (v1("/strips/1/gain_db", -6.54), legacy("op=gain&ch=0&v=-6.54")),
            (v1("/buses/B1/sends/V1", 1.0), legacy("op=route&ch=9&bus=0&v=1")),
        ] {
            assert_eq!(a, b);
        }
        // a text target has no number command
        assert_eq!(Target::parse("/player/take").unwrap().cmd(1.0), None);
    }

    /// What `linux/tests/e2e/api_doc.py` checks against a running mixer, here
    /// without one: every path's last part is in the API v1 section of api.md.
    #[test]
    fn every_path_is_in_the_document() {
        let doc = include_str!("../docs/api.md");
        let v1doc = &doc[doc.find("## API v1").unwrap()..doc.find("## Errors").unwrap()];
        for t in Target::all() {
            let p = t.path();
            let tail = if p.contains("/sends/") { "sends" } else { p.rsplit('/').next().unwrap() };
            assert!(
                [format!("`{tail}`"), format!("/{tail}`"), format!("{tail}/")]
                    .iter()
                    .any(|s| v1doc.contains(s.as_str())),
                "{p} is not documented"
            );
            let kind = match t.kind() {
                Kind::Flag => "`flag`",
                Kind::Number { .. } => "`number`",
                Kind::Choice(_) => "`choice`",
                Kind::Text => "`text`",
            };
            assert!(v1doc.contains(kind), "{kind}");
        }
    }

    #[test]
    fn the_state_gets_its_version_and_rev() {
        assert_eq!(state(7, r#"{"a":1}"#), r#"{"api":1,"rev":7,"a":1}"#);
        assert_eq!(state(0, "{}"), r#"{"api":1,"rev":0}"#);
    }

    #[test]
    fn labels_name_the_channel() {
        let names: Vec<String> = ["Mic", "", "", "", "", "", "V1", "", "", "Discord", ""].map(String::from).to_vec();
        assert_eq!(label(Target::parse("/strips/1/gain_db").unwrap(), &names), "Strip 1 (Mic): fader");
        assert_eq!(label(Target::parse("/buses/V1/mute").unwrap(), &names), "V1: mute");
        assert_eq!(label(Target::parse("/buses/B1/sends/V1").unwrap(), &names), "B1 (Discord): to V1");
        assert_eq!(label(Target::parse("/strips/2/pan").unwrap(), &[]), "Strip 2: pan");
        assert_eq!(label(Target::Recording, &names), "Recorder: recording");
        assert_eq!(
            label(Target::parse("/strips/1/eq/2/freq_hz").unwrap(), &names),
            "Strip 1 (Mic): EQ band 2 frequency"
        );
        assert_eq!(
            label(Target::parse("/strips/1/comp/makeup_db").unwrap(), &names),
            "Strip 1 (Mic): compressor make-up"
        );
        assert_eq!(label(Target::parse("/vban/out/2/to").unwrap(), &names), "VBAN out 2: receiver");
        assert_eq!(label(Target::parse("/player/sends/B2").unwrap(), &names), "Player: to B2");
    }

    /// Recording and sending audio over the network need the token; every
    /// other path (what the deck, a script or Companion moves) does not.
    #[test]
    fn which_paths_need_the_token() {
        let needs = |p: &str| Target::parse(p).unwrap().needs_token();
        for p in ["/recorder/recording", "/vban/on", "/vban/out/1/on", "/vban/out/2/to", "/vban/out/8/port"] {
            assert!(needs(p), "{p}");
        }
        for f in ["name", "source", "format", "channels"] {
            assert!(needs(&format!("/vban/out/3/{f}")), "{f}");
        }
        for p in [
            "/strips/1/gain_db",
            "/strips/1/record",
            "/buses/B1/mute",
            "/strips/2/sends/V1",
            "/vban/port",
            "/vban/in/1/on",
            "/vban/in/1/from",
            "/player/state",
            "/player/sends/B2",
        ] {
            assert!(!needs(p), "{p}");
        }
        // every path is one or the other, and the list covers all of /vban/out
        for t in Target::all() {
            let p = t.path();
            assert_eq!(
                t.needs_token(),
                p == "/recorder/recording" || p == "/vban/on" || p.starts_with("/vban/out/"),
                "{p}"
            );
        }
        // a command needs it when its path does; a scene recall never
        let c = |s: &str| cmd(s).unwrap().needs_token();
        assert!(c(r#"{"cmd":"set","path":"/recorder/recording","value":true}"#));
        assert!(c(r#"{"cmd":"toggle","path":"/vban/on"}"#));
        assert!(c(r#"{"cmd":"set","path":"/vban/out/1/to","value":"192.168.1.9"}"#));
        assert!(c(r#"{"cmd":"step","path":"/vban/out/1/port","by":1}"#));
        assert!(!c(r#"{"cmd":"set","path":"/strips/1/gain_db","value":-3}"#));
        assert!(!c(r#"{"cmd":"recall","scene":"Live"}"#));
    }
}
