//! API v1: one command model for every client (scripts, Companion, the MIDI
//! bindings). A value in the mixer has a **path** (`/strips/1/gain_db`,
//! `/buses/B1/mute`, `/strips/2/sends/V1`, `/recorder/recording`); a command
//! sets it, toggles it, steps it or fades it. Every change is made as the
//! typed [`Cmd`] of `POST /api/cmd`, through the same `apply`, so the two APIs
//! cannot drift apart: v1 only adds paths, relative changes, fades and batches.
//!
//! `docs/api.md` describes it; the parsing here needs no mixer and is tested
//! on its own, the rest against a test mixer in `mod.rs`.

use super::api::ApiError;
use super::apply::apply;
use super::cmd::{Cmd, SceneAct};
use super::json::{self, Value};
use super::{Hub, jnum, jstr, lock};
use bananasplit::engine::{
    BUS_IDS, BusParams, MAX_GAIN_DB, MAX_TRIM_DB, MIN_GAIN_DB, MIN_TRIM_DB, NUM_BUSES, NUM_STRIPS, StripParams,
    bus_index, clamp_db, clamp_pan, clamp_trim,
};
use bananasplit::limiter::{LIMIT_MIN_DB, LIMIT_OFF_DB, clamp_limit};
use bananasplit::scenes;
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
    /// Strips only.
    Gate,
    /// Strips only.
    Comp,
    /// Buses only.
    Limit,
    /// Feeds bus `b` (a strip's route, a bus's send).
    Send(usize),
}

/// A value the API can read and change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `ch` 0..5 = strips 1..6, 6..10 = buses V1..B2.
    Chan { ch: usize, p: Param },
    /// The recorder is recording.
    Recording,
}

/// How a target's value is kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// On or off: read as 0 or 1.
    Flag,
    /// A number from `min` to `max`, rounded to `step`.
    Number { min: f32, max: f32, step: f32, unit: &'static str },
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

impl Target {
    /// Read a path (`/strips/1/gain_db`). Buses go by name (`V1` to `B2`; the
    /// old `A1` to `A3` too), strips by number (1 to 6).
    pub fn parse(path: &str) -> Result<Target, String> {
        let bad = || format!("unknown path {path:?}");
        let parts: Vec<&str> = path.strip_prefix('/').ok_or_else(bad)?.split('/').collect();
        if parts == ["recorder", "recording"] {
            return Ok(Target::Recording);
        }
        let (strip, ch) = match parts.as_slice() {
            ["strips", n, ..] => {
                let n: usize = n.parse().ok().filter(|n| (1..=NUM_STRIPS).contains(n)).ok_or_else(bad)?;
                (true, n - 1)
            }
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
            ["solo"] if strip => Param::Solo,
            ["gate"] if strip => Param::Gate,
            ["comp"] if strip => Param::Comp,
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

    /// The path, as `parse` reads it (buses by their current names).
    pub fn path(&self) -> String {
        let Target::Chan { ch, p } = *self else { return "/recorder/recording".into() };
        let mut o = if ch < NUM_STRIPS {
            format!("/strips/{}/", ch + 1)
        } else {
            format!("/buses/{}/", BUS_IDS[ch - NUM_STRIPS])
        };
        o.push_str(match p {
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
            Param::Send(b) => {
                let _ = write!(o, "sends/{}", BUS_IDS[b]);
                return o;
            }
        });
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
            if strip {
                ps.extend([Param::Gate, Param::Comp]);
            } else {
                ps.push(Param::Limit);
            }
            ps.extend((0..NUM_BUSES).filter(|&b| strip || b != ch - NUM_STRIPS).map(Param::Send));
            v.extend(ps.into_iter().map(|p| Target::Chan { ch, p }));
        }
        v.push(Target::Recording);
        v
    }

    pub fn kind(&self) -> Kind {
        match self {
            Target::Chan { p: Param::Gain, .. } => {
                Kind::Number { min: MIN_GAIN_DB, max: MAX_GAIN_DB, step: 0.1, unit: "dB" }
            }
            Target::Chan { p: Param::Trim, .. } => {
                Kind::Number { min: MIN_TRIM_DB, max: MAX_TRIM_DB, step: 0.1, unit: "dB" }
            }
            Target::Chan { p: Param::Pan, .. } => Kind::Number { min: -1.0, max: 1.0, step: 0.01, unit: "" },
            Target::Chan { p: Param::Limit, .. } => {
                Kind::Number { min: LIMIT_MIN_DB, max: LIMIT_OFF_DB, step: 0.1, unit: "dB" }
            }
            _ => Kind::Flag,
        }
    }

    /// A value made valid for this target: clamped and rounded (a flag: 0 or 1).
    pub fn fit(&self, v: f32) -> f32 {
        if !v.is_finite() {
            return self.read_default();
        }
        match (self, self.kind()) {
            (_, Kind::Flag) => (v >= 0.5) as u8 as f32,
            (Target::Chan { p: Param::Gain, .. }, Kind::Number { step, .. }) => clamp_db(round_to(v, step)),
            (Target::Chan { p: Param::Trim, .. }, Kind::Number { step, .. }) => clamp_trim(round_to(v, step)),
            (Target::Chan { p: Param::Pan, .. }, Kind::Number { step, .. }) => clamp_pan(round_to(v, step)),
            (_, Kind::Number { step, .. }) => clamp_limit(round_to(v, step)),
        }
    }

    fn read_default(&self) -> f32 {
        match self {
            Target::Chan { p: Param::Limit, .. } => LIMIT_OFF_DB,
            _ => 0.0,
        }
    }

    /// The value as a fader position, 0 to 1 (the gain on the page's taper,
    /// the rest straight across its range; a flag is 0 or 1).
    pub fn norm_of(&self, v: f32) -> f32 {
        match (self, self.kind()) {
            (Target::Chan { p: Param::Gain, .. }, _) => db_to_pos(v),
            (_, Kind::Number { min, max, .. }) => ((v - min) / (max - min)).clamp(0.0, 1.0),
            (_, Kind::Flag) => (v >= 0.5) as u8 as f32,
        }
    }

    /// A fader position (0 to 1) as a value, [`fit`](Self::fit) for the target.
    pub fn value_at(&self, n: f32) -> f32 {
        let n = if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 };
        self.fit(match (self, self.kind()) {
            (Target::Chan { p: Param::Gain, .. }, _) => pos_to_db(n),
            (_, Kind::Number { min, max, .. }) => min + n * (max - min),
            (_, Kind::Flag) => n,
        })
    }

    /// The value now.
    pub fn read(&self, hub: &Hub) -> f32 {
        let sh = &*hub.shared;
        let Target::Chan { ch, p } = *self else { return hub.recorder.is_recording() as u8 as f32 };
        let flag = |b: bool| b as u8 as f32;
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
            Param::Gate => flag(strip.is_some_and(|s| s.dynamics.gate_on.load(SeqCst))),
            Param::Comp => flag(strip.is_some_and(|s| s.dynamics.comp_on.load(SeqCst))),
            Param::Limit => match c {
                Chan::Bus(b) => b.limit.load(),
                Chan::Strip(_) => LIMIT_OFF_DB,
            },
            Param::Send(bus) => flag(either!(x => x.routes.load(SeqCst)) & (1 << bus) != 0),
        }
    }

    /// The command that sets the target to `v` (already [`fit`](Self::fit)).
    fn cmd(&self, v: f32) -> Cmd {
        let on = v >= 0.5;
        let Target::Chan { ch, p } = *self else { return Cmd::Rec { on } };
        match p {
            Param::Gain => Cmd::Gain { ch, db: v },
            Param::Trim => Cmd::Trim { ch, db: v },
            Param::Pan => Cmd::Pan { ch, pan: v },
            Param::Mute => Cmd::Mute { ch, on },
            Param::Mono => Cmd::Mono { ch, on },
            Param::Solo => Cmd::Solo { strip: ch, on },
            Param::Record => Cmd::Link { ch, on },
            Param::Eq => Cmd::Eq { ch, on },
            Param::Gate => Cmd::Gate { strip: ch, on },
            Param::Comp => Cmd::Comp { strip: ch, on },
            Param::Limit => Cmd::Limit { bus: ch - NUM_STRIPS, db: v },
            Param::Send(bus) => Cmd::Route { ch, bus, on },
        }
    }

    /// Set the value (made valid first) and answer with what the page shows.
    /// A fade running on this target stops: the last change wins.
    pub fn write(&self, hub: &Hub, v: f32) -> Result<String, ApiError> {
        hub.fades.cancel(*self);
        self.write_raw(hub, v)
    }

    fn write_raw(&self, hub: &Hub, v: f32) -> Result<String, ApiError> {
        let v = self.fit(v);
        // Unchanged: nothing to do (the recorder would refuse a second start).
        if *self == Target::Recording && self.read(hub) == v {
            return Ok(if v >= 0.5 { "recording" } else { "not recording" }.into());
        }
        apply(hub, self.cmd(v))
    }
}

/// One command of `POST /api/v1/command`, checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Set {
        t: Target,
        v: f32,
    },
    Toggle {
        t: Target,
    },
    /// Add `by` (numbers only).
    Step {
        t: Target,
        by: f32,
    },
    /// Move to `to` over `ms` milliseconds (numbers only).
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
        Kind::Flag => Err(format!("{what} needs a number, and {} is on or off", t.path())),
    }
}

impl Command {
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
                    (Some(v), Kind::Flag) => {
                        return Err(format!("{} is on or off: give true or false, not {}", t.path(), v.kind()));
                    }
                    (Some(v), Kind::Number { .. }) => {
                        return Err(format!("{} needs a number, not {}", t.path(), v.kind()));
                    }
                };
                Command::Set { t, v }
            }
            "toggle" => {
                let t = target_of(o)?;
                if t.kind() != Kind::Flag {
                    return Err(format!("only on/off values toggle, and {} is a number", t.path()));
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
        let done = |t: Target, text: String| Ok((text, Some((t, t.read(hub)))));
        match self {
            Command::Set { t, v } => done(*t, t.write(hub, *v)?),
            Command::Toggle { t } => done(*t, t.write(hub, 1.0 - t.read(hub))?),
            Command::Step { t, by } => done(*t, t.write(hub, t.read(hub) + by)?),
            Command::Fade { t, to, ms } => {
                let to = t.fit(*to);
                if *ms == 0 {
                    return done(*t, t.write(hub, to)?);
                }
                hub.fades.start(*t, t.read(hub), to, *ms);
                Ok((format!("fading to {to} over {ms} ms"), Some((*t, t.read(hub)))))
            }
            Command::Recall { scene } => {
                hub.fades.cancel_all();
                Ok((apply(hub, Cmd::Scene { name: scene.clone(), act: SceneAct::Load })?, None))
            }
        }
    }
}

/// What one command did: the page's text and, for a value, the value now.
pub type Outcome = Result<(String, Option<(Target, f32)>), ApiError>;

fn result_json(o: &mut String, r: &Outcome) {
    match r {
        Ok((text, val)) => {
            o.push_str("{\"ok\":true,\"text\":");
            jstr(o, text);
            if let Some((t, v)) = val {
                o.push_str(",\"path\":");
                jstr(o, &t.path());
                o.push_str(",\"value\":");
                value_json(o, *t, *v);
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

fn value_json(o: &mut String, t: Target, v: f32) {
    match t.kind() {
        Kind::Flag => o.push_str(if v >= 0.5 { "true" } else { "false" }),
        Kind::Number { step, .. } => jnum(o, v as f64, if step < 0.1 { 2 } else { 1 }),
    }
}

/// `POST /api/v1/command`: one command object, or a list of them.
///
/// A list is checked whole before anything is done, so a typo in its third
/// command changes nothing; then each runs in order, and the answer has each
/// one's result. One command answers its result alone, or its error.
pub fn command(hub: &Hub, body: &str) -> Result<String, ApiError> {
    let v = json::parse(body)?;
    if let Value::Arr(list) = &v {
        if list.is_empty() || list.len() > MAX_BATCH {
            return Err(format!("a batch holds 1 to {MAX_BATCH} commands").into());
        }
        let cmds = list
            .iter()
            .enumerate()
            .map(|(i, c)| Command::parse(c).map_err(|e| format!("command {}: {e}", i + 1)))
            .collect::<Result<Vec<_>, _>>()?;
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
    let r = Command::parse(&v)?.run(hub);
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
            }
        }
        o.push_str(",\"value\":");
        value_json(&mut o, *t, t.read(hub));
        o.push('}');
    }
    o.push_str("]}");
    o
}

/// "Mic: fader", "B1: send to V1", "Recorder: recording".
pub fn label(t: Target, names: &[String]) -> String {
    let Target::Chan { ch, p } = t else { return "Recorder: recording".into() };
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
        Param::Gate => "gate".into(),
        Param::Comp => "compressor".into(),
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
                // gains move evenly on the fader, not in dB
                let v = match f.t {
                    Target::Chan { p: Param::Gain, .. } => {
                        pos_to_db(db_to_pos(f.from) + (db_to_pos(f.to) - db_to_pos(f.from)) * k)
                    }
                    _ => f.from + (f.to - f.from) * k,
                };
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
        // 6 strips x (5 + solo + record, eq + gate, comp + 5 sends) + 5 buses x (5 + record, eq + limit + 4 sends) + recorder
        assert_eq!(Target::all().len(), 6 * 15 + 5 * 12 + 1);
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
    }
}
