//! MIDI controllers: faders, knobs and buttons that drive the mixer.
//!
//! A **binding** joins a control of the controller (its *source*: a CC, a
//! 14-bit CC pair, a note, pitch bend, an NRPN, a program change) to an API v1
//! path or a scene, with an explicit *mode* (how the control's messages are
//! read: absolute, one of three relative encodings, toggle, momentary,
//! trigger). Every change goes through [`v1::Target::write`], so MIDI drives
//! exactly the commands the page and the API do.
//!
//! * **Learn**: move a control; its source is caught and offered, with the mode
//!   its messages suggest. Nothing is changed while learning.
//! * **Soft-takeover** (pickup): an absolute knob or fader that does not match
//!   the value (moved in the page, the deck, the API, a scene) does nothing
//!   until it reaches or crosses it, so nothing jumps.
//! * **Feedback**: LEDs and motor faders follow every change, from any source
//!   (a velocity-0 Note On stays a Note On: some controllers ignore Note Off).
//!   A binding that just moved is not echoed for [`ECHO_MS`].
//! * One input and one output port, chosen by name and opened again when the
//!   device comes back (each system's ports are listed every [`SCAN`]).
//! * Clock, active sensing and other real-time bytes, and SysEx, are dropped
//!   before anything else sees them.
//!
//! The device side is the app's (`midi::io()` in each app: the ALSA sequencer
//! on Linux, WinMM on Windows, CoreMIDI on macOS) behind [`Io`]; everything
//! here is plain code, tested with a fake device. The bindings and the chosen
//! ports are kept in `midi.json` next to the config.

use super::json::{self, Value};
use super::v1::{Kind, Target};
use super::{Hub, jstr, lock};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How often the system's ports are listed (a device plugged in again is opened).
pub const SCAN: Duration = Duration::from_secs(2);
/// How often LEDs and motor faders are brought up to date.
const FEEDBACK: Duration = Duration::from_millis(30);
/// A binding that just received is not sent back to for this long.
pub const ECHO_MS: u64 = 250;
/// How long learning collects messages after the first (to tell a 14-bit pair
/// or a relative encoder from a plain CC).
const LEARN_LISTEN: Duration = Duration::from_millis(600);
/// Learning stops by itself after this.
const LEARN_TIMEOUT: Duration = Duration::from_secs(60);
/// The most bindings.
pub const MAX_BINDINGS: usize = 256;
/// Soft-takeover: a control this close to the value (of the travel) picks it up.
const PICKUP: f32 = 3.0 / 127.0;
/// One step of a relative encoder, as a share of the target's travel.
const REL_STEP: f32 = 0.01;

// ---------------------------------------------------------------------------
// Bytes to messages
// ---------------------------------------------------------------------------

/// One channel message (2 or 3 bytes), packed: `status | d1 << 8 | d2 << 16`.
pub type Packed = u32;

#[cfg(test)]
pub fn pack(m: &[u8]) -> Packed {
    m.iter().take(3).enumerate().fold(0, |a, (i, b)| a | (*b as u32) << (8 * i))
}

pub fn unpack(p: Packed) -> [u8; 3] {
    [p as u8, (p >> 8) as u8 & 0x7f, (p >> 16) as u8 & 0x7f]
}

/// Data bytes a channel message of this status carries.
fn data_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 1,
        _ => 2,
    }
}

/// A MIDI byte stream to channel messages: running status is resolved; SysEx,
/// system common and real-time bytes (clock, active sensing ...) are dropped.
/// It allocates nothing, so a device callback can run it.
#[derive(Debug, Default, Clone)]
pub struct Parser {
    running: u8,
    buf: [u8; 2],
    n: usize,
    sysex: bool,
}

impl Parser {
    /// Feed bytes; `out` gets each complete channel message.
    pub fn feed(&mut self, bytes: &[u8], mut out: impl FnMut(Packed)) {
        for &b in bytes {
            match b {
                0xf8..=0xff => {} // real time: may sit anywhere, even inside a message
                0xf0 => {
                    self.sysex = true;
                    self.running = 0;
                }
                0xf7 => self.sysex = false,
                0xf1..=0xf6 => {
                    self.sysex = false;
                    self.running = 0; // system common cancels running status
                }
                0x80..=0xef => {
                    self.sysex = false;
                    self.running = b;
                    self.n = 0;
                }
                _ if self.sysex || self.running == 0 => {}
                _ => {
                    self.buf[self.n] = b;
                    self.n += 1;
                    if self.n == data_len(self.running) {
                        let p = self.running as u32 | (self.buf[0] as u32) << 8;
                        out(if self.n == 2 { p | (self.buf[1] as u32) << 16 } else { p });
                        self.n = 0;
                    }
                }
            }
        }
    }
}

/// Messages from the device's thread to the MIDI thread: a fixed ring, one
/// writer and one reader, no locks and no allocation (a WinMM or CoreMIDI
/// callback may not do either). When it is full, new messages are dropped and
/// counted.
pub struct Inbox {
    buf: Box<[AtomicU32]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    pub dropped: AtomicU32,
}

impl Inbox {
    pub fn new(cap: usize) -> Arc<Inbox> {
        Arc::new(Inbox {
            buf: (0..cap.next_power_of_two()).map(|_| AtomicU32::new(0)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicU32::new(0),
        })
    }

    /// The device's side.
    pub fn push(&self, m: Packed) {
        let h = self.head.load(Relaxed);
        if h.wrapping_sub(self.tail.load(Acquire)) >= self.buf.len() {
            self.dropped.fetch_add(1, Relaxed);
            return;
        }
        self.buf[h & (self.buf.len() - 1)].store(m, Relaxed);
        self.head.store(h.wrapping_add(1), Release);
    }

    /// The MIDI thread's side.
    pub fn pop(&self) -> Option<Packed> {
        let t = self.tail.load(Relaxed);
        if t == self.head.load(Acquire) {
            return None;
        }
        let m = self.buf[t & (self.buf.len() - 1)].load(Relaxed);
        self.tail.store(t.wrapping_add(1), Release);
        Some(m)
    }
}

/// A system's MIDI ports, as the MIDI thread uses them. An implementation
/// lists ports by name, opens at most one input (its messages go to the
/// inbox, already parsed) and one output.
pub trait Io: Send {
    /// The ports controllers send on (we read them).
    fn inputs(&mut self) -> Vec<String>;
    /// The ports controllers listen on (we write them).
    fn outputs(&mut self) -> Vec<String>;
    fn open_input(&mut self, name: &str, inbox: Arc<Inbox>) -> Result<(), String>;
    fn close_input(&mut self);
    fn open_output(&mut self, name: &str) -> Result<(), String>;
    fn close_output(&mut self);
    /// One channel message (2 or 3 bytes).
    fn send(&mut self, msg: &[u8]) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Sources, modes and bindings
// ---------------------------------------------------------------------------

/// A control of the controller. Channels are 0..15 here, 1..16 in text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    Cc {
        ch: u8,
        cc: u8,
    },
    /// A 14-bit control: MSB on `msb` (0..31), LSB on `msb + 32`.
    Cc14 {
        ch: u8,
        msb: u8,
    },
    Note {
        ch: u8,
        note: u8,
    },
    /// Pitch bend (14 bits): the faders of Mackie Control surfaces.
    Pitch {
        ch: u8,
    },
    Nrpn {
        ch: u8,
        param: u16,
    },
    Program {
        ch: u8,
        prog: u8,
    },
}

impl Src {
    /// `cc:1:7`, `cc14:1:7`, `note:10:36`, `pb:1`, `nrpn:1:300`, `pc:1:5`.
    pub fn text(&self) -> String {
        match *self {
            Src::Cc { ch, cc } => format!("cc:{}:{cc}", ch + 1),
            Src::Cc14 { ch, msb } => format!("cc14:{}:{msb}", ch + 1),
            Src::Note { ch, note } => format!("note:{}:{note}", ch + 1),
            Src::Pitch { ch } => format!("pb:{}", ch + 1),
            Src::Nrpn { ch, param } => format!("nrpn:{}:{param}", ch + 1),
            Src::Program { ch, prog } => format!("pc:{}:{prog}", ch + 1),
        }
    }

    pub fn parse(s: &str) -> Result<Src, String> {
        let bad = || format!("bad MIDI control {s:?}");
        let p: Vec<&str> = s.trim().split(':').collect();
        let ch = |i: usize| -> Result<u8, String> {
            p.get(i).and_then(|c| c.parse::<u8>().ok()).filter(|c| (1..=16).contains(c)).map(|c| c - 1).ok_or_else(bad)
        };
        let num = |i: usize, max: u16| -> Result<u16, String> {
            p.get(i).and_then(|c| c.parse::<u16>().ok()).filter(|c| *c <= max).ok_or_else(bad)
        };
        let src = match (p.first().copied(), p.len()) {
            (Some("cc"), 3) => Src::Cc { ch: ch(1)?, cc: num(2, 127)? as u8 },
            (Some("cc14"), 3) => Src::Cc14 { ch: ch(1)?, msb: num(2, 31)? as u8 },
            (Some("note"), 3) => Src::Note { ch: ch(1)?, note: num(2, 127)? as u8 },
            (Some("pb"), 2) => Src::Pitch { ch: ch(1)? },
            (Some("nrpn"), 3) => Src::Nrpn { ch: ch(1)?, param: num(2, 16383)? },
            (Some("pc"), 3) => Src::Program { ch: ch(1)?, prog: num(2, 127)? as u8 },
            _ => return Err(bad()),
        };
        Ok(src)
    }

    /// For people: "CC 7 · ch 1", "Note 36 · ch 10".
    pub fn name(&self) -> String {
        match *self {
            Src::Cc { ch, cc } => format!("CC {cc} · ch {}", ch + 1),
            Src::Cc14 { ch, msb } => format!("CC {msb}/{} (14-bit) · ch {}", msb + 32, ch + 1),
            Src::Note { ch, note } => format!("Note {note} · ch {}", ch + 1),
            Src::Pitch { ch } => format!("Pitch bend · ch {}", ch + 1),
            Src::Nrpn { ch, param } => format!("NRPN {param} · ch {}", ch + 1),
            Src::Program { ch, prog } => format!("Program {prog} · ch {}", ch + 1),
        }
    }
}

/// How a control's messages are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The value is the control's position.
    Abs,
    /// Relative, two's complement: 1..63 up, 127..65 down.
    Rel2,
    /// Relative, around 64: 65.. up, 63.. down.
    Rel64,
    /// Relative, sign bit: 1..63 up, 65..127 down.
    RelSign,
    /// A press flips an on/off value.
    Toggle,
    /// On while held.
    Momentary,
    /// A press does it once (a scene).
    Trigger,
}

const MODES: [(Mode, &str); 7] = [
    (Mode::Abs, "abs"),
    (Mode::Rel2, "rel2"),
    (Mode::Rel64, "rel64"),
    (Mode::RelSign, "relsign"),
    (Mode::Toggle, "toggle"),
    (Mode::Momentary, "momentary"),
    (Mode::Trigger, "trigger"),
];

impl Mode {
    pub fn text(self) -> &'static str {
        MODES.iter().find(|(m, _)| *m == self).map_or("abs", |(_, t)| t)
    }
    pub fn parse(s: &str) -> Result<Mode, String> {
        MODES.iter().find(|(_, t)| *t == s).map(|(m, _)| *m).ok_or_else(|| format!("unknown MIDI mode {s:?}"))
    }
    fn relative(self) -> bool {
        matches!(self, Mode::Rel2 | Mode::Rel64 | Mode::RelSign)
    }
    /// Steps of a relative encoder in one message.
    fn steps(self, v: u8) -> i32 {
        let v = v as i32;
        match self {
            Mode::Rel2 => {
                if v < 64 {
                    v
                } else {
                    v - 128
                }
            }
            Mode::Rel64 => v - 64,
            Mode::RelSign => {
                if v < 64 {
                    v
                } else {
                    -(v - 64)
                }
            }
            _ => 0,
        }
    }
}

/// What a binding drives.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Value(Target),
    /// Load a saved scene.
    Scene(String),
}

impl Action {
    /// A path, or `scene:<name>`.
    pub fn text(&self) -> String {
        match self {
            Action::Value(t) => t.path(),
            Action::Scene(n) => format!("scene:{n}"),
        }
    }
    pub fn parse(s: &str) -> Result<Action, String> {
        match s.strip_prefix("scene:") {
            Some(n) => Ok(Action::Scene(bananasplit::scenes::validate_name(n)?.to_string())),
            None => Target::parse(s).map(Action::Value),
        }
    }
    pub fn label(&self, names: &[String]) -> String {
        match self {
            Action::Value(t) => super::v1::label(*t, names),
            Action::Scene(n) => format!("Scene: {n}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub src: Src,
    pub action: Action,
    pub mode: Mode,
    /// Soft-takeover (absolute controls on numbers).
    pub takeover: bool,
    /// Send the value back (LEDs, motor faders).
    pub feedback: bool,
}

/// The mode a control suggests for an action, and the modes it may have.
pub fn modes_for(src: Src, action: &Action) -> Vec<Mode> {
    match action {
        Action::Scene(_) => vec![Mode::Trigger],
        Action::Value(t) => match (t.kind(), src) {
            (Kind::Number { .. }, Src::Cc { .. }) => vec![Mode::Abs, Mode::Rel2, Mode::Rel64, Mode::RelSign],
            (Kind::Number { .. }, Src::Cc14 { .. } | Src::Pitch { .. } | Src::Nrpn { .. }) => vec![Mode::Abs],
            (Kind::Number { .. }, Src::Note { .. } | Src::Program { .. }) => vec![],
            (Kind::Flag, Src::Note { .. }) => vec![Mode::Toggle, Mode::Momentary],
            (Kind::Flag, Src::Program { .. }) => vec![Mode::Toggle],
            (Kind::Flag, _) => vec![Mode::Toggle, Mode::Abs, Mode::Momentary],
        },
    }
}

impl Binding {
    /// A binding, checked: the mode must fit the control and the action
    /// (`None`: the first that fits).
    pub fn new(
        src: Src,
        action: Action,
        mode: Option<Mode>,
        takeover: Option<bool>,
        feedback: Option<bool>,
    ) -> Result<Binding, String> {
        let ok = modes_for(src, &action);
        let Some(&first) = ok.first() else {
            return Err(format!(
                "{} is a button: it can switch things or load a scene, not set {}",
                src.name(),
                action.text()
            ));
        };
        let mode = mode.unwrap_or(first);
        if !ok.contains(&mode) {
            let list: Vec<&str> = ok.iter().map(|m| m.text()).collect();
            return Err(format!("{} on {} can be {}", src.name(), action.text(), list.join(", ")));
        }
        Ok(Binding {
            src,
            mode,
            takeover: takeover.unwrap_or(mode == Mode::Abs && !matches!(src, Src::Pitch { .. })),
            feedback: feedback.unwrap_or(!matches!(action, Action::Scene(_)) && !matches!(src, Src::Program { .. })),
            action,
        })
    }

    fn to_value(&self) -> Value {
        Value::Obj(vec![
            ("src".into(), Value::Str(self.src.text())),
            ("target".into(), Value::Str(self.action.text())),
            ("mode".into(), Value::Str(self.mode.text().into())),
            ("takeover".into(), Value::Bool(self.takeover)),
            ("feedback".into(), Value::Bool(self.feedback)),
        ])
    }

    fn from_value(v: &Value) -> Result<Binding, String> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).ok_or_else(|| format!("missing {k:?}"));
        Binding::new(
            Src::parse(s("src")?)?,
            Action::parse(s("target")?)?,
            Some(Mode::parse(s("mode")?)?),
            v.get("takeover").and_then(Value::as_bool),
            v.get("feedback").and_then(Value::as_bool),
        )
    }
}

// ---------------------------------------------------------------------------
// Messages to what they mean
// ---------------------------------------------------------------------------

/// What one message says about one control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// A position, 0 to 1.
    Pos(f32),
    /// A raw 7-bit value (CCs: relative steps, or a button at >= 64).
    Raw(u8),
    Press,
    Release,
}

/// The state the decoder keeps between messages: 14-bit halves and the
/// NRPN being addressed, per channel.
#[derive(Debug, Clone, Default)]
pub struct Decoder {
    msb: [[u8; 32]; 16],
    lsb: [[u8; 32]; 16],
    nrpn_param: [u16; 16],
    nrpn_val: [u16; 16],
    /// The channels whose CC 99/98 named a parameter (until another 99/98 or 101/100).
    nrpn_on: [bool; 16],
}

const N14: f32 = 16383.0;

impl Decoder {
    /// Every control one message speaks for, with what it says. A CC can
    /// count for a plain CC binding and as half of a 14-bit one; which binding
    /// exists decides which is used.
    pub fn decode(&mut self, m: Packed) -> Vec<(Src, Input)> {
        let [st, d1, d2] = unpack(m);
        let ch = st & 0x0f;
        let c = ch as usize;
        let mut out = Vec::new();
        match st & 0xf0 {
            0x90 if d2 > 0 => out.push((Src::Note { ch, note: d1 }, Input::Press)),
            0x90 | 0x80 => out.push((Src::Note { ch, note: d1 }, Input::Release)),
            0xe0 => out.push((Src::Pitch { ch }, Input::Pos(((d2 as u16) << 7 | d1 as u16) as f32 / N14))),
            0xc0 => out.push((Src::Program { ch, prog: d1 }, Input::Press)),
            0xb0 => {
                match d1 {
                    99 => {
                        self.nrpn_param[c] = (d2 as u16) << 7 | (self.nrpn_param[c] & 0x7f);
                        self.nrpn_on[c] = true;
                    }
                    98 => {
                        self.nrpn_param[c] = (self.nrpn_param[c] & !0x7f) | d2 as u16;
                        self.nrpn_on[c] = true;
                    }
                    100 | 101 => self.nrpn_on[c] = false, // an RPN
                    6 | 38 if self.nrpn_on[c] => {
                        let v = &mut self.nrpn_val[c];
                        *v = if d1 == 6 { (d2 as u16) << 7 | (*v & 0x7f) } else { (*v & !0x7f) | d2 as u16 };
                        out.push((Src::Nrpn { ch, param: self.nrpn_param[c] }, Input::Pos(*v as f32 / N14)));
                        return out;
                    }
                    _ => {}
                }
                out.push((Src::Cc { ch, cc: d1 }, Input::Raw(d2)));
                if d1 < 32 {
                    self.msb[c][d1 as usize] = d2;
                    let v = (d2 as u16) << 7 | self.lsb[c][d1 as usize] as u16;
                    out.push((Src::Cc14 { ch, msb: d1 }, Input::Pos(v as f32 / N14)));
                } else if d1 < 64 {
                    let k = (d1 - 32) as usize;
                    self.lsb[c][k] = d2;
                    let v = (self.msb[c][k] as u16) << 7 | d2 as u16;
                    out.push((Src::Cc14 { ch, msb: d1 - 32 }, Input::Pos(v as f32 / N14)));
                }
            }
            _ => {} // aftertouch: not a control
        }
        out
    }
}

/// "CC 7 = 100 · ch 1": the last message, for the MIDI window.
pub fn describe(m: Packed) -> String {
    let [st, d1, d2] = unpack(m);
    let ch = (st & 0x0f) + 1;
    match st & 0xf0 {
        0x90 if d2 > 0 => format!("Note {d1} on ({d2}) · ch {ch}"),
        0x90 | 0x80 => format!("Note {d1} off · ch {ch}"),
        0xb0 => format!("CC {d1} = {d2} · ch {ch}"),
        0xc0 => format!("Program {d1} · ch {ch}"),
        0xe0 => format!("Pitch bend {} · ch {ch}", (d2 as u16) << 7 | d1 as u16),
        0xa0 => format!("Aftertouch {d1} · ch {ch}"),
        _ => format!("Channel pressure {d1} · ch {ch}"),
    }
}

/// Learning: the first control that moves, and what it sent next.
#[derive(Debug, Clone, Default)]
pub struct Learn {
    /// Learning now.
    pub on: bool,
    started: Option<Instant>,
    first: Option<(Instant, Packed)>,
    /// Messages after the first (CC values, CCs), for the guesses.
    seen: Vec<Packed>,
    nrpn: Option<Src>,
}

impl Learn {
    fn start(&mut self, now: Instant) {
        *self = Learn { on: true, started: Some(now), ..Learn::default() };
    }

    fn feed(&mut self, m: Packed, src: &[(Src, Input)], now: Instant) {
        if !self.on {
            return;
        }
        if let Some(n @ Src::Nrpn { .. }) = src.iter().map(|s| s.0).find(|s| matches!(s, Src::Nrpn { .. })) {
            self.nrpn = Some(n);
        }
        let [st, d1, _] = unpack(m);
        // a release, or NRPN addressing on its own, is not what to learn
        let addressing = st & 0xf0 == 0xb0 && matches!(d1, 98..=101);
        let release = src.iter().any(|s| s.1 == Input::Release);
        match self.first {
            None if !addressing && !release => self.first = Some((now, m)),
            Some((t, _)) if now.duration_since(t) <= LEARN_LISTEN && self.seen.len() < 64 => self.seen.push(m),
            _ => {}
        }
    }

    /// The control caught, and the mode its messages suggest (`None`: no guess).
    pub fn caught(&self) -> Option<(Src, Option<Mode>)> {
        if let Some(n) = self.nrpn {
            return Some((n, Some(Mode::Abs)));
        }
        let (_, m) = self.first?;
        let [st, d1, _] = unpack(m);
        let ch = st & 0x0f;
        Some(match st & 0xf0 {
            0x90 | 0x80 => (Src::Note { ch, note: d1 }, None),
            0xe0 => (Src::Pitch { ch }, Some(Mode::Abs)),
            0xc0 => (Src::Program { ch, prog: d1 }, None),
            _ => {
                let same: Vec<u8> = std::iter::once(m)
                    .chain(self.seen.iter().copied())
                    .map(unpack)
                    .filter(|x| x[0] == st && x[1] == d1)
                    .map(|x| x[2])
                    .collect();
                let pair = d1 < 32 && self.seen.iter().map(|&x| unpack(x)).any(|x| x[0] == st && x[1] == d1 + 32);
                if pair {
                    (Src::Cc14 { ch, msb: d1 }, Some(Mode::Abs))
                } else if same.len() >= 2
                    && same.iter().all(|&v| (1..=15).contains(&v) || (113..=127).contains(&v))
                    && (same.iter().all(|&v| v <= 3 || v >= 125)
                        || same.iter().filter(|&&v| v == same[0]).count() * 2 > same.len())
                {
                    // tiny steps up (1, 2 ...) or down (127, 126 ...): an encoder
                    (Src::Cc { ch, cc: d1 }, Some(Mode::Rel2))
                } else if same.len() >= 2
                    && same.iter().all(|&v| v != 64 && (57..=71).contains(&v))
                    && same.iter().filter(|&&v| v == same[0]).count() * 2 > same.len()
                {
                    // the same small step around 64, again and again: an encoder
                    (Src::Cc { ch, cc: d1 }, Some(Mode::Rel64))
                } else {
                    (Src::Cc { ch, cc: d1 }, None)
                }
            }
        })
    }

    fn expired(&self, now: Instant) -> bool {
        self.on && self.started.is_some_and(|t| now.duration_since(t) > LEARN_TIMEOUT)
    }
}

// ---------------------------------------------------------------------------
// The bindings at work
// ---------------------------------------------------------------------------

/// What one binding remembers while it works.
#[derive(Debug, Clone, Default)]
struct Rt {
    /// Soft-takeover: the last position the control sent (0 to 1).
    last_in: Option<f32>,
    /// The value this binding wrote last, if it is still the value.
    wrote: Option<f32>,
    /// When it last received.
    moved: Option<Instant>,
    /// The value last sent back (or written): feedback is sent when it changes.
    fed: Option<f32>,
    /// A momentary button is held.
    held: bool,
}

/// Soft-takeover: may a control at `n` move a value at `cur` (both 0 to 1)?
/// Yes when this binding set the value last and nobody moved it since, when
/// the control is close to the value, or when it crossed it since its last
/// message.
pub fn picks_up(rt_last: Option<f32>, wrote: Option<f32>, n: f32, cur: f32) -> bool {
    if wrote.is_some_and(|w| (w - cur).abs() < 1e-4) || (n - cur).abs() <= PICKUP {
        return true;
    }
    rt_last.is_some_and(|l| (l - cur).signum() != (n - cur).signum())
}

/// The ports: what the person chose and what is open.
#[derive(Debug, Clone, Default)]
struct Ports {
    want_in: String,
    want_out: String,
    ins: Vec<String>,
    outs: Vec<String>,
    open_in: bool,
    open_out: bool,
    error: Option<String>,
}

struct Inner {
    bindings: Vec<Binding>,
    rt: Vec<Rt>,
    ports: Ports,
    learn: Learn,
    dec: Decoder,
    last: String,
    /// The device side could not be started (no MIDI on this system).
    unavailable: Option<String>,
    /// Ports chosen or bindings changed: the thread looks again.
    dirty: bool,
    /// Send every binding's value again (a port was opened).
    refeed: bool,
}

/// MIDI for one hub: the bindings, the ports and learning, shared by the MIDI
/// thread and the API.
pub struct Midi {
    file: PathBuf,
    inner: Mutex<Inner>,
    wake: Condvar,
    pub inbox: Arc<Inbox>,
    running: AtomicBool,
}

fn lockm(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    lock(m)
}

impl Midi {
    /// The bindings and ports from `file` (`midi.json`; missing: none).
    pub fn load(file: PathBuf) -> Midi {
        let mut inner = Inner {
            bindings: Vec::new(),
            rt: Vec::new(),
            ports: Ports::default(),
            learn: Learn::default(),
            dec: Decoder::default(),
            last: String::new(),
            unavailable: None,
            dirty: true,
            refeed: false,
        };
        if let Ok(text) = std::fs::read_to_string(&file) {
            match json::parse(&text) {
                Ok(v) => {
                    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    inner.ports.want_in = s("in");
                    inner.ports.want_out = s("out");
                    if let Some(Value::Arr(list)) = v.get("bindings") {
                        for b in list.iter().take(MAX_BINDINGS) {
                            match Binding::from_value(b) {
                                Ok(b) => inner.bindings.push(b),
                                Err(e) => eprintln!("bananasplit: {}: a MIDI binding is skipped: {e}", file.display()),
                            }
                        }
                    }
                }
                Err(e) => eprintln!("bananasplit: {}: {e}; no MIDI bindings loaded", file.display()),
            }
        }
        inner.rt = vec![Rt::default(); inner.bindings.len()];
        Midi {
            file,
            inner: Mutex::new(inner),
            wake: Condvar::new(),
            inbox: Inbox::new(1024),
            running: AtomicBool::new(false),
        }
    }

    fn save(&self, inner: &Inner) -> Result<(), String> {
        let v = Value::Obj(vec![
            ("in".into(), Value::Str(inner.ports.want_in.clone())),
            ("out".into(), Value::Str(inner.ports.want_out.clone())),
            ("bindings".into(), Value::Arr(inner.bindings.iter().map(Binding::to_value).collect())),
        ]);
        let mut o = String::new();
        json::write(&mut o, &v);
        o.push('\n');
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, o)
            .and_then(|_| std::fs::rename(&tmp, &self.file))
            .map_err(|e| format!("cannot save {}: {e}", self.file.display()))
    }

    fn changed(&self, inner: &mut Inner) -> Result<(), String> {
        inner.dirty = true;
        self.wake.notify_all();
        self.save(inner)
    }

    /// Learning on (forgets what was caught) or off.
    pub fn learn(&self, on: bool) {
        let mut i = lockm(&self.inner);
        if on {
            i.learn.start(Instant::now());
        } else {
            i.learn = Learn::default();
        }
    }

    /// Choose the ports (`None`: unchanged, `""`: none).
    pub fn set_ports(&self, input: Option<&str>, output: Option<&str>) -> Result<(), String> {
        let mut i = lockm(&self.inner);
        if let Some(n) = input {
            i.ports.want_in = n.to_string();
        }
        if let Some(n) = output {
            i.ports.want_out = n.to_string();
        }
        i.ports.error = None;
        self.changed(&mut i)
    }

    /// Add a binding, or replace the one on the same control.
    pub fn bind(&self, b: Binding) -> Result<(), String> {
        let mut i = lockm(&self.inner);
        match i.bindings.iter().position(|x| x.src == b.src) {
            Some(k) => {
                i.bindings[k] = b;
                i.rt[k] = Rt::default();
            }
            None if i.bindings.len() >= MAX_BINDINGS => return Err(format!("at most {MAX_BINDINGS} MIDI bindings")),
            None => {
                i.bindings.push(b);
                i.rt.push(Rt::default());
            }
        }
        i.learn = Learn::default();
        self.changed(&mut i)
    }

    /// Remove the binding on a control (`false`: there was none).
    pub fn unbind(&self, src: Src) -> Result<bool, String> {
        let mut i = lockm(&self.inner);
        let Some(k) = i.bindings.iter().position(|x| x.src == src) else { return Ok(false) };
        i.bindings.remove(k);
        i.rt.remove(k);
        self.changed(&mut i).map(|_| true)
    }

    #[cfg(test)]
    pub fn bindings(&self) -> Vec<Binding> {
        lockm(&self.inner).bindings.clone()
    }

    /// The `midi` object of the state.
    pub fn state_json(&self, o: &mut String, names: &[String]) {
        let i = lockm(&self.inner);
        let p = &i.ports;
        o.push_str("{\"available\":");
        o.push_str(if i.unavailable.is_none() && self.running.load(Relaxed) { "true" } else { "false" });
        o.push_str(",\"in\":");
        jstr(o, &p.want_in);
        o.push_str(",\"out\":");
        jstr(o, &p.want_out);
        let _ = write!(o, ",\"inOpen\":{},\"outOpen\":{},\"ins\":[", p.open_in, p.open_out);
        for (k, n) in p.ins.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(o, n);
        }
        o.push_str("],\"outs\":[");
        for (k, n) in p.outs.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(o, n);
        }
        o.push_str("],\"error\":");
        match i.unavailable.as_ref().or(p.error.as_ref()) {
            Some(e) => jstr(o, e),
            None => o.push_str("null"),
        }
        o.push_str(",\"last\":");
        jstr(o, &i.last);
        o.push_str(",\"learn\":");
        if !i.learn.on {
            o.push_str("null");
        } else {
            o.push_str("{\"src\":");
            match i.learn.caught() {
                None => o.push_str("null,\"name\":null,\"mode\":null"),
                Some((s, m)) => {
                    jstr(o, &s.text());
                    o.push_str(",\"name\":");
                    jstr(o, &s.name());
                    o.push_str(",\"mode\":");
                    match m {
                        Some(m) => jstr(o, m.text()),
                        None => o.push_str("null"),
                    }
                }
            }
            o.push('}');
        }
        o.push_str(",\"bindings\":[");
        for (k, b) in i.bindings.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            o.push_str("{\"src\":");
            jstr(o, &b.src.text());
            o.push_str(",\"name\":");
            jstr(o, &b.src.name());
            o.push_str(",\"target\":");
            jstr(o, &b.action.text());
            o.push_str(",\"label\":");
            jstr(o, &b.action.label(names));
            o.push_str(",\"mode\":");
            jstr(o, b.mode.text());
            let _ = write!(o, ",\"takeover\":{},\"feedback\":{}}}", b.takeover, b.feedback);
        }
        o.push_str("]}");
    }

    // ---- the MIDI thread's work ----

    /// One message from the controller.
    fn handle(&self, hub: &Hub, m: Packed, now: Instant) {
        // (binding, action, value): which binding moved, and what to do
        let mut todo: Vec<(usize, Action, Option<f32>)> = Vec::new();
        {
            let mut i = lockm(&self.inner);
            i.last = describe(m);
            let decoded = i.dec.decode(m);
            if i.learn.on {
                i.learn.feed(m, &decoded, now);
                return;
            }
            let Inner { bindings, rt, .. } = &mut *i;
            for (src, input) in decoded {
                for (k, (b, r)) in bindings.iter().zip(rt.iter_mut()).enumerate().filter(|(_, (b, _))| b.src == src) {
                    r.moved = Some(now);
                    if let Some((a, v)) = resolve(b, r, input, hub) {
                        todo.push((k, a, v));
                    }
                }
            }
        }
        // the writes go through the API, without the lock
        let mut wrote = Vec::new();
        for (k, a, v) in todo {
            match (&a, v) {
                (Action::Value(t), Some(v)) => {
                    if t.write(hub, v).is_ok() {
                        wrote.push((k, *t, t.read(hub)));
                    }
                }
                (Action::Scene(n), _) => {
                    let c = super::v1::Command::Recall { scene: n.clone() };
                    let _ = c.run(hub);
                }
                _ => {}
            }
        }
        let mut i = lockm(&self.inner);
        let Inner { bindings, rt, .. } = &mut *i;
        for (k, t, now_v) in wrote {
            // only the binding that moved: another control on the same value
            // still gets the new value sent back
            if let (Some(b), Some(r)) = (bindings.get(k), rt.get_mut(k))
                && b.action == Action::Value(t)
            {
                r.wrote = Some(t.norm_of(now_v));
                r.fed = Some(now_v);
            }
        }
    }

    /// Bring LEDs and motor faders up to date; `send` writes one message.
    fn feedback(&self, hub: &Hub, now: Instant, mut send: impl FnMut(&[u8])) {
        let mut i = lockm(&self.inner);
        let refeed = std::mem::take(&mut i.refeed);
        let Inner { bindings, rt, .. } = &mut *i;
        for (b, r) in bindings.iter().zip(rt.iter_mut()) {
            let Action::Value(t) = &b.action else { continue };
            if !b.feedback || matches!(b.src, Src::Program { .. }) {
                continue;
            }
            if r.moved.is_some_and(|m| now.duration_since(m) < Duration::from_millis(ECHO_MS)) {
                continue;
            }
            let v = t.read(hub);
            if !refeed && r.fed == Some(v) {
                continue;
            }
            r.fed = Some(v);
            if r.wrote.is_some_and(|w| (w - t.norm_of(v)).abs() >= 1e-4) {
                r.wrote = None; // moved by something else: pick up again
            }
            for m in encode(b.src, t.norm_of(v)) {
                send(&m[..msg_len(m[0])]);
            }
        }
    }
}

fn msg_len(status: u8) -> usize {
    1 + data_len(status)
}

/// The messages that put a control at `n` (0 to 1): what feedback sends.
pub fn encode(src: Src, n: f32) -> Vec<[u8; 3]> {
    let n = n.clamp(0.0, 1.0);
    let v7 = (n * 127.0).round() as u8;
    let v14 = (n * N14).round() as u16;
    let (hi, lo) = ((v14 >> 7) as u8, (v14 & 0x7f) as u8);
    match src {
        Src::Cc { ch, cc } => vec![[0xb0 | ch, cc, v7]],
        Src::Cc14 { ch, msb } => vec![[0xb0 | ch, msb, hi], [0xb0 | ch, msb + 32, lo]],
        Src::Pitch { ch } => vec![[0xe0 | ch, lo, hi]],
        Src::Nrpn { ch, param } => vec![
            [0xb0 | ch, 99, (param >> 7) as u8],
            [0xb0 | ch, 98, (param & 0x7f) as u8],
            [0xb0 | ch, 6, hi],
            [0xb0 | ch, 38, lo],
        ],
        // a velocity-0 Note On, not a Note Off: some controllers' LEDs ignore Note Off
        Src::Note { ch, note } => vec![[0x90 | ch, note, if n >= 0.5 { 127 } else { 0 }]],
        Src::Program { .. } => vec![],
    }
}

/// What a binding does with one input: the action and, for a value, the value
/// to set (`None`: nothing to do).
fn resolve(b: &Binding, r: &mut Rt, input: Input, hub: &Hub) -> Option<(Action, Option<f32>)> {
    if let Action::Scene(_) = &b.action {
        let press = match input {
            Input::Press => true,
            Input::Raw(v) => {
                let was = r.held;
                r.held = v >= 64;
                r.held && !was
            }
            _ => false,
        };
        return press.then(|| (b.action.clone(), None));
    }
    let Action::Value(t) = &b.action else { return None };
    let cur = t.read(hub);
    let v = match (b.mode, input) {
        (Mode::Abs, Input::Pos(n)) => {
            let c = t.norm_of(cur);
            let ok = !b.takeover || picks_up(r.last_in, r.wrote, n, c);
            r.last_in = Some(n);
            if !ok {
                return None;
            }
            t.value_at(n)
        }
        (Mode::Abs, Input::Raw(raw)) => {
            let n = raw as f32 / 127.0;
            if t.kind() == Kind::Flag {
                (raw >= 64) as u8 as f32
            } else {
                let c = t.norm_of(cur);
                let ok = !b.takeover || picks_up(r.last_in, r.wrote, n, c);
                r.last_in = Some(n);
                if !ok {
                    return None;
                }
                t.value_at(n)
            }
        }
        (m, Input::Raw(raw)) if m.relative() => {
            let steps = m.steps(raw);
            if steps == 0 {
                return None;
            }
            t.value_at(t.norm_of(cur) + steps as f32 * REL_STEP)
        }
        (Mode::Toggle, Input::Press) => 1.0 - cur.round(),
        (Mode::Toggle, Input::Raw(raw)) => {
            let was = r.held;
            r.held = raw >= 64;
            if !r.held || was {
                return None;
            }
            1.0 - cur.round()
        }
        (Mode::Momentary, Input::Press) => 1.0,
        (Mode::Momentary, Input::Release) => 0.0,
        (Mode::Momentary, Input::Raw(raw)) => (raw >= 64) as u8 as f32,
        _ => return None,
    };
    if t.fit(v) == t.fit(cur) && b.mode != Mode::Momentary {
        r.wrote = Some(t.norm_of(cur));
        return None;
    }
    Some((b.action.clone(), Some(v)))
}

/// Open, close and list the ports as they should be. Called every [`SCAN`]
/// and whenever a choice changed.
fn reconcile(midi: &Midi, io: &mut dyn Io) {
    let (want_in, want_out, open_in, open_out) = {
        let i = lockm(&midi.inner);
        (i.ports.want_in.clone(), i.ports.want_out.clone(), i.ports.open_in, i.ports.open_out)
    };
    let (ins, outs) = (io.inputs(), io.outputs());
    let mut err = None;
    let mut now_in = open_in;
    // a port that went away is closed; the wanted one is opened when it is there
    if open_in && (want_in.is_empty() || !ins.contains(&want_in)) {
        io.close_input();
        now_in = false;
    }
    if !now_in && !want_in.is_empty() && ins.contains(&want_in) {
        match io.open_input(&want_in, midi.inbox.clone()) {
            Ok(()) => now_in = true,
            Err(e) => err = Some(e),
        }
    }
    let mut now_out = open_out;
    if open_out && (want_out.is_empty() || !outs.contains(&want_out)) {
        io.close_output();
        now_out = false;
    }
    let mut refeed = false;
    if !now_out && !want_out.is_empty() && outs.contains(&want_out) {
        match io.open_output(&want_out) {
            Ok(()) => {
                now_out = true;
                refeed = true;
            }
            Err(e) => err = err.or(Some(e)),
        }
    }
    let mut i = lockm(&midi.inner);
    let p = &mut i.ports;
    (p.ins, p.outs, p.open_in, p.open_out) = (ins, outs, now_in, now_out);
    // a chosen port that is not there is said, not an error
    if err.is_some() || p.error.as_ref().is_some_and(|_| now_in || want_in.is_empty()) {
        p.error = err;
    }
    i.refeed |= refeed;
}

/// The MIDI thread: started by `gui::start_midi`, runs until the hub closes.
pub fn run(hub: &Hub, midi: &Midi, io: Result<Box<dyn Io>, String>) {
    let mut io = match io {
        Ok(io) => io,
        Err(e) => {
            lockm(&midi.inner).unavailable = Some(format!("MIDI is not available: {e}"));
            return;
        }
    };
    midi.running.store(true, Relaxed);
    let mut next_scan = Instant::now();
    let mut next_feed = Instant::now();
    loop {
        if hub.is_closing() {
            break;
        }
        let now = Instant::now();
        let dirty = std::mem::take(&mut lockm(&midi.inner).dirty);
        if dirty || now >= next_scan {
            reconcile(midi, io.as_mut());
            next_scan = now + SCAN;
        }
        while let Some(m) = midi.inbox.pop() {
            midi.handle(hub, m, Instant::now());
        }
        let (open_in, open_out) = {
            let mut i = lockm(&midi.inner);
            if i.learn.expired(now) {
                i.learn = Learn::default();
            }
            (i.ports.open_in, i.ports.open_out)
        };
        if open_out && now >= next_feed {
            let mut fail = None;
            midi.feedback(hub, now, |m| {
                if fail.is_none()
                    && let Err(e) = io.send(m)
                {
                    fail = Some(e);
                }
            });
            if let Some(e) = fail {
                lockm(&midi.inner).ports.error = Some(e);
            }
            next_feed = now + FEEDBACK;
        }
        // short naps while a controller is open (its messages wait in the inbox)
        let nap = if open_in || open_out { Duration::from_millis(4) } else { Duration::from_millis(500) };
        let i = lockm(&midi.inner);
        let _ = midi.wake.wait_timeout_while(i, nap, |i| !i.dirty && !hub.is_closing());
    }
    io.close_input();
    io.close_output();
    midi.running.store(false, Relaxed);
}

impl Midi {
    /// Wake the thread (on quit).
    pub fn wake(&self) {
        let _i = lockm(&self.inner);
        self.wake.notify_all();
    }
}

/// A fake device for the tests: ports by name, what was sent, what to feed.
#[cfg(test)]
pub mod fake {
    use super::*;

    #[derive(Default)]
    pub struct State {
        pub ins: Vec<String>,
        pub outs: Vec<String>,
        pub open_in: Option<String>,
        pub open_out: Option<String>,
        pub inbox: Option<Arc<Inbox>>,
        pub sent: Vec<Vec<u8>>,
        pub opens: usize,
    }

    #[derive(Clone, Default)]
    pub struct Fake(pub Arc<Mutex<State>>);

    impl Fake {
        /// The controller sends these bytes.
        pub fn play(&self, bytes: &[u8]) {
            let s = self.0.lock().unwrap();
            if let Some(ib) = &s.inbox {
                Parser::default().feed(bytes, |m| ib.push(m));
            }
        }
    }

    impl Io for Fake {
        fn inputs(&mut self) -> Vec<String> {
            self.0.lock().unwrap().ins.clone()
        }
        fn outputs(&mut self) -> Vec<String> {
            self.0.lock().unwrap().outs.clone()
        }
        fn open_input(&mut self, name: &str, inbox: Arc<Inbox>) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            s.open_in = Some(name.into());
            s.inbox = Some(inbox);
            s.opens += 1;
            Ok(())
        }
        fn close_input(&mut self) {
            let mut s = self.0.lock().unwrap();
            s.open_in = None;
            s.inbox = None;
        }
        fn open_output(&mut self, name: &str) -> Result<(), String> {
            self.0.lock().unwrap().open_out = Some(name.into());
            Ok(())
        }
        fn close_output(&mut self) {
            self.0.lock().unwrap().open_out = None;
        }
        fn send(&mut self, msg: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().sent.push(msg.to_vec());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> Vec<[u8; 3]> {
        let mut v = Vec::new();
        Parser::default().feed(bytes, |m| v.push(unpack(m)));
        v
    }

    #[test]
    fn the_parser_resolves_running_status_and_drops_the_rest() {
        assert_eq!(parse_all(&[0xb0, 7, 100, 7, 101, 0xf8, 7, 102]), [[0xb0, 7, 100], [0xb0, 7, 101], [0xb0, 7, 102]]);
        // clock in the middle of a message, active sensing, sysex
        assert_eq!(
            parse_all(&[0x90, 0xf8, 60, 0xfe, 127, 0xf0, 1, 2, 3, 0xf7, 0xc1, 5]),
            [[0x90, 60, 127], [0xc1, 5, 0]]
        );
        // system common cancels running status; stray data is dropped
        assert_eq!(parse_all(&[0xb0, 1, 2, 0xf3, 1, 3, 4]), [[0xb0, 1, 2]]);
        assert_eq!(parse_all(&[5, 6, 0xe0, 0, 64]), [[0xe0, 0, 64]]);
        assert_eq!(unpack(pack(&[0x92, 36, 0])), [0x92, 36, 0]);
    }

    #[test]
    fn the_inbox_keeps_order_and_counts_what_it_drops() {
        let ib = Inbox::new(4);
        for k in 0..6 {
            ib.push(k);
        }
        assert_eq!(ib.dropped.load(Relaxed), 2);
        assert_eq!(std::iter::from_fn(|| ib.pop()).collect::<Vec<_>>(), [0, 1, 2, 3]);
        ib.push(9);
        assert_eq!(ib.pop(), Some(9));
        assert_eq!(ib.pop(), None);
    }

    #[test]
    fn sources_and_modes_read_back() {
        for s in [
            Src::Cc { ch: 0, cc: 7 },
            Src::Cc14 { ch: 15, msb: 31 },
            Src::Note { ch: 9, note: 36 },
            Src::Pitch { ch: 3 },
            Src::Nrpn { ch: 0, param: 16383 },
            Src::Program { ch: 1, prog: 127 },
        ] {
            assert_eq!(Src::parse(&s.text()), Ok(s));
        }
        for bad in ["cc:0:7", "cc:17:7", "cc:1:128", "cc14:1:32", "pb", "pb:1:2", "nrpn:1:16384", "x:1:1", ""] {
            assert!(Src::parse(bad).is_err(), "{bad}");
        }
        for (m, t) in MODES {
            assert_eq!(Mode::parse(t), Ok(m));
        }
        assert_eq!((Mode::Rel2.steps(1), Mode::Rel2.steps(127), Mode::Rel2.steps(120)), (1, -1, -8));
        assert_eq!((Mode::Rel64.steps(65), Mode::Rel64.steps(62)), (1, -2));
        assert_eq!((Mode::RelSign.steps(3), Mode::RelSign.steps(67)), (3, -3));
    }

    fn t(p: &str) -> Action {
        Action::parse(p).unwrap()
    }

    #[test]
    fn a_binding_must_fit_its_control() {
        let fader = Src::Cc { ch: 0, cc: 7 };
        let b = Binding::new(fader, t("/strips/1/gain_db"), None, None, None).unwrap();
        assert_eq!((b.mode, b.takeover, b.feedback), (Mode::Abs, true, true));
        let pb = Binding::new(Src::Pitch { ch: 0 }, t("/strips/1/gain_db"), None, None, None).unwrap();
        assert!(!pb.takeover, "a motor fader is moved, not picked up");
        let btn = Src::Note { ch: 0, note: 16 };
        assert_eq!(Binding::new(btn, t("/strips/1/mute"), None, None, None).unwrap().mode, Mode::Toggle);
        assert!(Binding::new(btn, t("/strips/1/gain_db"), None, None, None).unwrap_err().contains("is a button"));
        assert!(
            Binding::new(fader, t("/strips/1/gain_db"), Some(Mode::Toggle), None, None)
                .unwrap_err()
                .contains("can be abs, rel2")
        );
        let sc = Binding::new(Src::Program { ch: 0, prog: 3 }, t("scene:Live"), None, None, None).unwrap();
        assert_eq!((sc.mode, sc.feedback), (Mode::Trigger, false));
        assert!(Action::parse("scene:.x").is_err());
        assert!(Action::parse("/nope").is_err());
    }

    #[test]
    fn decoding_cc_pairs_nrpn_notes_and_pitch() {
        let mut d = Decoder::default();
        let p = |d: &mut Decoder, m: [u8; 3]| d.decode(pack(&m));
        assert_eq!(p(&mut d, [0x90, 36, 100]), [(Src::Note { ch: 0, note: 36 }, Input::Press)]);
        assert_eq!(p(&mut d, [0x90, 36, 0]), [(Src::Note { ch: 0, note: 36 }, Input::Release)]);
        assert_eq!(p(&mut d, [0x80, 36, 64]), [(Src::Note { ch: 0, note: 36 }, Input::Release)]);
        assert_eq!(p(&mut d, [0xe1, 0x7f, 0x7f]), [(Src::Pitch { ch: 1 }, Input::Pos(1.0))]);
        // a 14-bit pair: MSB then LSB
        let a = p(&mut d, [0xb0, 9, 64]);
        assert_eq!(a[0], (Src::Cc { ch: 0, cc: 9 }, Input::Raw(64)));
        assert_eq!(a[1].0, Src::Cc14 { ch: 0, msb: 9 });
        let b = p(&mut d, [0xb0, 41, 127]);
        assert_eq!(b[1], (Src::Cc14 { ch: 0, msb: 9 }, Input::Pos((64 << 7 | 127) as f32 / N14)));
        // NRPN 300 = 2:44, value MSB and LSB
        p(&mut d, [0xb2, 99, 2]);
        p(&mut d, [0xb2, 98, 44]);
        let v = p(&mut d, [0xb2, 6, 127]);
        assert_eq!(v, [(Src::Nrpn { ch: 2, param: 300 }, Input::Pos((127 << 7) as f32 / N14))]);
        let v = p(&mut d, [0xb2, 38, 127]);
        assert_eq!(v, [(Src::Nrpn { ch: 2, param: 300 }, Input::Pos(1.0))]);
        // after an RPN, CC 6 is an ordinary CC again
        p(&mut d, [0xb2, 101, 0]);
        assert_eq!(p(&mut d, [0xb2, 6, 1])[0], (Src::Cc { ch: 2, cc: 6 }, Input::Raw(1)));
    }

    #[test]
    fn soft_takeover() {
        // far away and not crossing: no
        assert!(!picks_up(None, None, 0.9, 0.2));
        assert!(!picks_up(Some(0.95), None, 0.9, 0.2));
        // close: yes
        assert!(picks_up(None, None, 0.21, 0.2));
        // crossed between two messages: yes
        assert!(picks_up(Some(0.1), None, 0.3, 0.2));
        // this binding set it and nobody moved it since: yes
        assert!(picks_up(Some(0.5), Some(0.5), 0.9, 0.5));
    }

    #[test]
    fn feedback_messages() {
        assert_eq!(encode(Src::Cc { ch: 1, cc: 7 }, 1.0), [[0xb1, 7, 127]]);
        assert_eq!(encode(Src::Cc14 { ch: 0, msb: 1 }, 1.0), [[0xb0, 1, 127], [0xb0, 33, 127]]);
        assert_eq!(encode(Src::Pitch { ch: 0 }, 0.5), [[0xe0, 0, 64]]);
        assert_eq!(encode(Src::Note { ch: 0, note: 5 }, 0.0), [[0x90, 5, 0]], "velocity 0 Note On, never Note Off");
        assert_eq!(
            encode(Src::Nrpn { ch: 0, param: 300 }, 0.0),
            [[0xb0, 99, 2], [0xb0, 98, 44], [0xb0, 6, 0], [0xb0, 38, 0]]
        );
        assert!(encode(Src::Program { ch: 0, prog: 1 }, 1.0).is_empty());
    }

    fn learned(msgs: &[[u8; 3]]) -> Option<(Src, Option<Mode>)> {
        let mut l = Learn::default();
        let now = Instant::now();
        l.start(now);
        let mut d = Decoder::default();
        for m in msgs {
            let p = pack(m);
            let dec = d.decode(p);
            l.feed(p, &dec, now);
        }
        l.caught()
    }

    #[test]
    fn learning_guesses_the_kind_of_control() {
        assert_eq!(learned(&[]), None);
        assert_eq!(learned(&[[0xb0, 7, 10], [0xb0, 7, 12], [0xb0, 7, 15]]), Some((Src::Cc { ch: 0, cc: 7 }, None)));
        assert_eq!(learned(&[[0xb0, 1, 10], [0xb0, 33, 5]]), Some((Src::Cc14 { ch: 0, msb: 1 }, Some(Mode::Abs))));
        assert_eq!(
            learned(&[[0xb0, 16, 1], [0xb0, 16, 1], [0xb0, 16, 2]]),
            Some((Src::Cc { ch: 0, cc: 16 }, Some(Mode::Rel2)))
        );
        assert_eq!(learned(&[[0xb0, 16, 127], [0xb0, 16, 126]]), Some((Src::Cc { ch: 0, cc: 16 }, Some(Mode::Rel2))));
        assert_eq!(
            learned(&[[0xb0, 16, 65], [0xb0, 16, 65], [0xb0, 16, 65]]),
            Some((Src::Cc { ch: 0, cc: 16 }, Some(Mode::Rel64)))
        );
        assert_eq!(
            learned(&[[0x99, 36, 0], [0x99, 37, 90], [0x89, 37, 0]]),
            Some((Src::Note { ch: 9, note: 37 }, None)),
            "a release is not learned"
        );
        assert_eq!(learned(&[[0xe4, 0, 64]]), Some((Src::Pitch { ch: 4 }, Some(Mode::Abs))));
        assert_eq!(
            learned(&[[0xb0, 99, 2], [0xb0, 98, 44], [0xb0, 6, 64]]),
            Some((Src::Nrpn { ch: 0, param: 300 }, Some(Mode::Abs)))
        );
        assert_eq!(learned(&[[0xc3, 9, 0]]), Some((Src::Program { ch: 3, prog: 9 }, None)));
    }
}
