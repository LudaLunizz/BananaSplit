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
use bananasplit::recorder::RecTap;
use bananasplit::vban::VbanTap;
use coreaudio::{Graph, RATE, Stats};
use route::DevInfo;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::time::{Duration, Instant};

/// Never happens: devices that fail are retried, there is no server that
/// can go away (kept for the main loop, which is shared with Linux).
pub const SERVER_GONE: &str = "the macOS audio system stopped";

/// What a strip says when macOS doesn't let BananaSplit record.
pub const MIC_DENIED: &str = "macOS doesn't let BananaSplit record (every input is silent): allow it in System Settings › Privacy & Security › Microphone, then restart BananaSplit";

/// How often to look for added/removed devices and default-device changes.
const RESCAN: Duration = Duration::from_secs(2);
/// After reconnecting the devices because one stalled: how long the audio
/// must run before another stall is reconnected again.
const STALL_REARM: Duration = Duration::from_secs(60);

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
        o.push_str(if input { "Input devices (use in [stripN] connect =)\n" } else { "Output devices (use in [bus.XX] connect =)\n" });
        let def = coreaudio::default_uid(input);
        for d in all.iter().filter(|d| if input { d.inputs > 0 } else { d.outputs > 0 }) {
            let n = if input { d.inputs } else { d.outputs };
            o.push_str(&format!(
                "    {}{}{}  {n} ch, {} kHz\n",
                d.info.name,
                if def.as_deref() == Some(d.info.id.as_str()) { "  [default]" } else { "" },
                if route::is_cable(&d.info.name) { "  (virtual cable)" } else if d.info.virt { "  (virtual)" } else { "" },
                d.rate / 1000.0,
            ));
        }
        o.push('\n');
    }
    Ok(o)
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
    /// Said once with the next connection's errors.
    note: Option<String>,
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
        let signature = coreaudio::signature();
        let stats = Arc::new(Stats::default());
        shared.sample_rate.store(RATE, SeqCst);
        // Start audio right away; the first connect() reuses this graph.
        let graph = match Graph::build(cfg, &shared, &stats, rec_tap.clone(), vban_tap.clone(), Some(play_tap.clone()), None) {
            Ok(g) => Some(g),
            Err(e) => {
                notes.push(e);
                None
            }
        };
        Ok(Session {
            shared,
            stats,
            graph,
            signature,
            last_scan: Instant::now(),
            dirty: false,
            stall_retried: None,
            bench: None,
            note: None,
            rec_tap,
            vban_tap,
            play_tap: Some(play_tap),
        })
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
            return true;
        }
        if self.graph.is_none() || self.dirty {
            self.dirty = true; // retry a failed build now and then
            return true;
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
                self.note = Some(format!("{} still delivers no audio: the mixer goes on without waiting for it", s.name));
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
    pub fn connect(&mut self, cfg: &Config, force: bool) -> (Vec<String>, Vec<String>) {
        if force || self.dirty || self.graph.is_none() {
            self.graph = None; // stop the old IOProcs first
            self.dirty = false;
            self.signature = coreaudio::signature();
            self.last_scan = Instant::now();
            self.shared.sample_rate.store(RATE, SeqCst);
            match Graph::build(cfg, &self.shared, &self.stats, self.rec_tap.clone(), self.vban_tap.clone(), self.play_tap.clone(), self.bench.as_deref()) {
                Ok(g) => self.graph = Some(g),
                Err(e) => return (Vec::new(), vec![e]),
            }
        }
        let g = self.graph.as_ref().expect("graph built");
        let mut errs = g.errs.clone();
        if g.inputs > 0 && ffi::mic_access() == Some(false) {
            errs.insert(0, MIC_DENIED.into());
        }
        if let Some(n) = self.note.take() {
            errs.insert(0, n);
        }
        (g.desc.clone(), errs)
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
    }
}
