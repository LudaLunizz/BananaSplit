//! macOS session: Core Audio devices + virtual cables (BlackHole and other
//! loopback devices, optional drivers of their own).
//!
//! | Linux (version 2)                  | macOS                                                  |
//! |------------------------------------|--------------------------------------------------------|
//! | JACK client, one graph clock       | Core Audio IOProcs, master clock + drift-corrected rings |
//! | PipeWire null-sink cables (pactl)  | loopback devices (BlackHole, ...), one per channel     |
//! | JACK port regex in `connect`       | part of a device name in `connect`                     |
//! | termios, signals                   | signals; the app window and menu bar icon (`app.rs`)   |

pub mod cables;
pub mod catypes;
pub mod chanmenu;
pub mod coreaudio;
pub mod ffi;
pub mod route;

use bananasplit::config::Config;
use bananasplit::engine::Shared;
use bananasplit::player::PlayTap;
use bananasplit::pure::{Access, Backoff, STARTED_AFTER_WAIT, WAITING_FOR_MIC, access_note};
use bananasplit::recorder::RecTap;
use bananasplit::vban::VbanTap;
use coreaudio::{Graph, RATE, Stats};
use route::DevInfo;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Never happens: devices that fail are retried, there is no server that
/// can go away (kept for the main loop, which is shared with Linux).
pub const SERVER_GONE: &str = "the macOS audio system stopped";

/// How often to look for added/removed devices and default-device changes.
const RESCAN: Duration = Duration::from_secs(2);
/// After reconnecting the devices because one stalled: how long the audio
/// must run before another stall is reconnected again.
const STALL_REARM: Duration = Duration::from_secs(60);
/// A start still waiting this long is waiting for the user's answer to
/// macOS's microphone question: the status line says so.
const SAY_WAITING: Duration = Duration::from_millis(300);

/// A device start running on its own thread, because macOS may ask the
/// user about the microphone first and `AudioDeviceStart` waits for the
/// answer: the menu and the page keep working meanwhile.
struct Pending {
    rx: Receiver<Result<Graph, String>>,
    since: Instant,
    /// The status line said "waiting" already.
    told: bool,
}

pub struct Status {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub cpu: f32,
}

/// The devices as macOS shows them now: the ones with inputs, the ones
/// with outputs (a device with both is in both) and the defaults' UIDs.
pub struct Snapshot {
    pub caps: Vec<DevInfo>,
    pub rens: Vec<DevInfo>,
    pub def_cap: Option<String>,
    pub def_ren: Option<String>,
}

pub fn snapshot() -> Result<Snapshot, String> {
    let all = coreaudio::devices()?;
    Ok(Snapshot {
        caps: all.iter().filter(|d| d.inputs > 0).map(|d| d.info.clone()).collect(),
        rens: all.iter().filter(|d| d.outputs > 0).map(|d| d.info.clone()).collect(),
        def_cap: coreaudio::default_uid(true),
        def_ren: coreaudio::default_uid(false),
    })
}

/// The virtual cable check at start (a note when none is installed). True
/// if none is installed: on the very first start, the app's page opens its
/// "Virtual cables" window (`cables::report`).
pub fn first_run(headless: bool, notes: &mut Vec<String>) -> bool {
    match snapshot() {
        Ok(s) => cables::first_run(&s, headless, notes),
        Err(e) => {
            notes.push(e);
            false
        }
    }
}

/// `bananasplit --list-devices`: the names to use in `connect =` lines.
pub fn list_devices() -> Result<String, String> {
    let all = coreaudio::devices()?;
    let mut o = String::new();
    for input in [true, false] {
        o.push_str(if input {
            "Input devices (use in [stripN] connect =)\n"
        } else {
            "Output devices (use in [bus.XX] connect =)\n"
        });
        let def = coreaudio::default_uid(input);
        for d in all.iter().filter(|d| if input { d.inputs > 0 } else { d.outputs > 0 }) {
            let n = if input { d.inputs } else { d.outputs };
            o.push_str(&format!(
                "    {}{}{}  {n} ch, {} kHz\n",
                d.info.name,
                if def.as_deref() == Some(d.info.id.as_str()) { "  [default]" } else { "" },
                if route::is_cable(&d.info.name) {
                    "  (virtual cable)"
                } else if d.info.virt {
                    "  (virtual)"
                } else {
                    ""
                },
                d.rate / 1000.0,
            ));
        }
        o.push('\n');
    }
    Ok(o)
}

/// `bananasplit --status`: every device (name, UID, rate, channels) and
/// the defaults, read only: no stream starts and nothing asks for a
/// permission. Err only when Core Audio itself fails.
pub fn status() -> Result<String, String> {
    let all = coreaudio::devices()?;
    let devs: Vec<bananasplit::pure::StatusDevice> = all
        .iter()
        .map(|d| bananasplit::pure::StatusDevice {
            name: d.info.name.clone(),
            uid: d.info.id.clone(),
            rate: d.rate,
            inputs: d.inputs,
            outputs: d.outputs,
        })
        .collect();
    let (def_out, def_in) = (coreaudio::default_uid(false), coreaudio::default_uid(true));
    Ok(bananasplit::pure::status_report(&devs, def_out.as_deref(), def_in.as_deref()))
}

pub struct Session {
    shared: Arc<Shared>,
    stats: Arc<Stats>,
    graph: Option<Graph>,
    signature: String,
    last_scan: Instant,
    dirty: bool,
    /// When a stalled device was reconnected. Not again until the devices
    /// change, or the audio ran for STALL_REARM since without a stall (a
    /// device that keeps stalling would otherwise cut everyone's audio
    /// every few seconds).
    stall_retried: Option<Instant>,
    /// The UID of an output device that clocked the mixer and stalled again
    /// after the reconnect: until the devices change, the mixer runs on
    /// another clock and that device only plays, as soon as it runs again.
    /// Otherwise everything would wait for it: every other output, the
    /// recorder, VBAN.
    bench: Option<String>,
    /// When the devices were reconnected because an IOProc panicked or
    /// silenced cycles (`Stats::poisoned`, `Stats::bad_cycles`): not again
    /// for STALL_REARM, so a fault that comes back at once can't cut
    /// everyone's audio every few seconds.
    guard_retried: Option<Instant>,
    /// `Stats::bad_cycles` when it was last reported.
    bad_seen: u64,
    /// Said once with the next connection's errors.
    note: Option<String>,
    /// A start waiting for the microphone question (see `Pending`).
    pending: Option<Pending>,
    /// What it returned, until `connect` takes it.
    arrived: Option<Result<Graph, String>>,
    /// Failed starts are tried again after 1 s, 2 s, 4 s, ... up to 30 s,
    /// and right away when the devices change.
    backoff: Backoff,
    retry_at: Option<Instant>,
    /// Why the last start failed (while there is no graph).
    last_err: Option<String>,
    rec_tap: Arc<RecTap>,
    vban_tap: Arc<VbanTap>,
    play_tap: Option<Arc<PlayTap>>,
}

impl Session {
    pub fn open(
        cfg: &Config,
        shared: Arc<Shared>,
        rec_tap: Arc<RecTap>,
        vban_tap: Arc<VbanTap>,
        play_tap: Arc<PlayTap>,
        notes: &mut Vec<String>,
    ) -> Result<Session, String> {
        let _ = notes; // a failed start is said by the first connect()
        let mut s = Session {
            shared,
            stats: Arc::new(Stats::default()),
            graph: None,
            signature: String::new(),
            last_scan: Instant::now(),
            dirty: false,
            stall_retried: None,
            bench: None,
            guard_retried: None,
            bad_seen: 0,
            note: None,
            pending: None,
            arrived: None,
            backoff: Backoff::default(),
            retry_at: None,
            last_err: None,
            rec_tap,
            vban_tap,
            play_tap: Some(play_tap),
        };
        // Start audio right away; the first connect() reuses this graph.
        s.start(cfg);
        Ok(s)
    }

    /// Stop the old graph, then open every configured device again: on a
    /// thread of its own while macOS hasn't asked about the microphone yet
    /// (the answer comes back through `pending`), otherwise right here, as
    /// it always did.
    fn start(&mut self, cfg: &Config) {
        self.graph = None; // stop the old IOProcs first
        self.dirty = false;
        self.signature = coreaudio::signature();
        self.last_scan = Instant::now();
        self.shared.sample_rate.store(RATE, SeqCst);
        if ffi::mic_access() == Access::NotDetermined {
            let (tx, rx) = mpsc::channel();
            let (cfg, shared, stats) = (cfg.clone(), self.shared.clone(), self.stats.clone());
            let (rec, vban, play) = (self.rec_tap.clone(), self.vban_tap.clone(), self.play_tap.clone());
            let bench = self.bench.clone();
            let spawned = std::thread::Builder::new().name("bs-start".into()).spawn(move || {
                let built = Graph::build(&cfg, &shared, &stats, rec, vban, play, bench.as_deref());
                // (Session gone: the graph stops here)
                let _ = tx.send(built);
            });
            if spawned.is_ok() {
                self.pending = Some(Pending { rx, since: Instant::now(), told: false });
                return;
            }
        }
        let built = Graph::build(
            cfg,
            &self.shared,
            &self.stats,
            self.rec_tap.clone(),
            self.vban_tap.clone(),
            self.play_tap.clone(),
            self.bench.as_deref(),
        );
        self.install(built);
    }

    /// Take a start's result: the new graph, or why it failed (tried again
    /// after the next backoff delay).
    fn install(&mut self, built: Result<Graph, String>) {
        match built {
            Ok(g) => {
                self.graph = Some(g);
                self.backoff.reset();
                self.retry_at = None;
                self.last_err = None;
            }
            Err(e) => {
                self.retry_at = Some(Instant::now() + self.backoff.next_delay());
                self.last_err = Some(e);
            }
        }
    }

    /// Devices that fail are retried and the graph is rebuilt, so there is
    /// no fatal "server gone" state on macOS.
    pub fn shutdown(&self) -> bool {
        false
    }

    /// Edge-triggered: true once when a device appeared, disappeared or
    /// changed its rate, a default device changed, or a device stopped
    /// delivering audio. The caller then calls `connect`, which rebuilds
    /// the graph.
    pub fn take_changed(&mut self) -> bool {
        // A start waiting for the microphone question: nothing else until
        // it is back (the devices are looked at again after that).
        if let Some(p) = &mut self.pending {
            let got = match p.rx.try_recv() {
                Ok(built) => built,
                Err(TryRecvError::Disconnected) => Err("the audio start stopped before it finished".into()),
                Err(TryRecvError::Empty) => {
                    if !p.told && p.since.elapsed() >= SAY_WAITING {
                        p.told = true;
                        self.note = Some(WAITING_FOR_MIC.into());
                        return true;
                    }
                    return false;
                }
            };
            if p.told {
                self.note = Some(STARTED_AFTER_WAIT.into());
            }
            self.pending = None;
            self.arrived = Some(got);
            return true;
        }
        if self.last_scan.elapsed() < RESCAN {
            return false;
        }
        self.last_scan = Instant::now();
        let sig = coreaudio::signature();
        if sig != self.signature {
            self.signature = sig;
            self.dirty = true;
            self.stall_retried = None;
            self.bench = None;
            self.guard_retried = None;
            // the devices changed: a failed start is tried again now, and
            // its backoff starts over
            self.backoff.reset();
            self.retry_at = None;
            return true;
        }
        if self.dirty {
            return true;
        }
        if self.graph.is_none() {
            // a failed start, tried again after its backoff delay
            if self.retry_at.is_none_or(|t| Instant::now() >= t) {
                self.dirty = true;
                return true;
            }
            return false;
        }
        if self.guard_retried.is_none_or(|t| t.elapsed() >= STALL_REARM) {
            let bad = self.stats.bad_cycles.load(Relaxed);
            let note = if self.stats.poisoned.swap(false, SeqCst) {
                Some(
                    "the audio stopped on a device because of an error in BananaSplit: reconnecting the devices".into(),
                )
            } else if bad != self.bad_seen {
                Some(format!(
                    "{} audio cycles were silenced: a device's buffers changed shape while it ran; reconnecting the devices",
                    bad.wrapping_sub(self.bad_seen)
                ))
            } else {
                None
            };
            if note.is_some() {
                self.bad_seen = bad;
                self.guard_retried = Some(Instant::now());
                self.dirty = true;
                self.note = note;
                return true;
            }
        }
        match self.graph.as_mut().and_then(Graph::stalled) {
            Some(s) if self.stall_retried.is_none() => {
                self.stall_retried = Some(Instant::now());
                self.dirty = true;
                self.note = Some(format!("{} stopped delivering audio: reconnecting the devices", s.name));
                return true;
            }
            Some(s) if s.master && self.bench.is_none() => {
                self.bench = Some(s.uid);
                self.dirty = true;
                self.note =
                    Some(format!("{} still delivers no audio: the mixer goes on without waiting for it", s.name));
                return true;
            }
            Some(_) => {}
            None => {
                if self.stall_retried.is_some_and(|t| t.elapsed() >= STALL_REARM) {
                    self.stall_retried = None;
                }
            }
        }
        false
    }

    /// (Re)open every configured device if anything changed. `force` = the
    /// user asked for it (Reconnect, a channel menu).
    /// While a start waits for the microphone question it returns at once
    /// (with the "waiting" note when there is one); a forced reconnect
    /// meanwhile happens as soon as the answer is in.
    pub fn connect(&mut self, cfg: &Config, force: bool) -> (Vec<String>, Vec<String>) {
        if let Some(built) = self.arrived.take() {
            self.install(built);
        }
        if self.pending.is_some() {
            self.dirty |= force;
            return (Vec::new(), self.note.take().into_iter().collect());
        }
        let retry_due = self.retry_at.is_none_or(|t| Instant::now() >= t);
        if force || self.dirty || (self.graph.is_none() && retry_due) {
            self.start(cfg);
            if self.pending.is_some() {
                return (Vec::new(), self.note.take().into_iter().collect());
            }
        }
        let mut errs = Vec::new();
        let desc = match &self.graph {
            Some(g) => {
                errs = g.errs.clone();
                if g.inputs > 0
                    && let Some(n) = access_note(ffi::mic_access())
                {
                    errs.insert(0, n.into());
                }
                g.desc.clone()
            }
            None => {
                errs.extend(self.last_err.clone());
                Vec::new()
            }
        };
        if let Some(n) = self.note.take() {
            errs.insert(0, n);
        }
        (desc, errs)
    }

    /// What each channel is connected to now, without (re)building
    /// anything: for every frame of the UI. Nothing while the graph could
    /// not be built (it is retried on the rescan tick, see `take_changed`).
    pub fn wiring(&self) -> Vec<String> {
        self.graph.as_ref().map_or_else(Vec::new, |g| g.desc.clone())
    }

    pub fn status(&self) -> Status {
        Status { sample_rate: RATE, buffer_size: self.stats.block.load(Relaxed), cpu: self.stats.cpu.load() }
    }

    /// (input side, output side) latency estimate in frames.
    pub fn io_latency(&self) -> (u32, u32) {
        (self.stats.lat_in.load(Relaxed), self.stats.lat_out.load(Relaxed))
    }

    pub fn cable_count(&self) -> usize {
        self.graph.as_ref().map_or(0, |g| g.cables)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.graph = None;
        // A start still waiting for the microphone question: give it a
        // moment, so its devices stop here; otherwise its thread stops them
        // when it is done (the channel is closed by then).
        if let Some(p) = self.pending.take() {
            drop(p.rx.recv_timeout(Duration::from_secs(2)));
        }
        self.arrived = None;
    }
}
