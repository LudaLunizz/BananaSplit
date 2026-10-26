//! Windows session: WASAPI shared-mode streams + VB-Audio virtual cables.
//!
//! | Linux (version 2)                  | Windows                                           |
//! |------------------------------------|---------------------------------------------------|
//! | JACK client, one graph clock       | WASAPI streams, master clock + drift-corrected rings |
//! | PipeWire null-sink cables (pactl)  | VB-Audio cables (driver; prompt to install)       |
//! | JACK port regex in `connect`       | part of a device name in `connect`                |
//! | termios raw mode, signals          | console raw mode, console control handler        |

pub mod cables;
pub mod chanmenu;
pub mod console;
pub mod endsession;
pub mod ffi;
pub mod route;
pub mod wasapi;

pub use console::{
    QUIT, RawTerminal, install_signal_handlers, pause_on_error, spawn_input_thread, stdin_is_tty,
};

use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::time::{Duration, Instant};
use bananasplit::config::Config;
use bananasplit::engine::Shared;
use bananasplit::player::PlayTap;
use bananasplit::recorder::RecTap;
use bananasplit::vban::VbanTap;
use wasapi::{Enumerator, Graph, RATE, Stats};

pub const SERVER_GONE: &str = "the Windows audio service stopped";
pub const LATENCY_HINT: &str =
    "round trip >= 60 ms: pick a different A1 device or close apps using exclusive mode";
/// WASAPI shared mode typically measures 25-45 ms here; flag only real problems.
pub const RTT_LIMIT_MS: f64 = 60.0;

/// How often to look for added/removed devices and default-device changes.
const RESCAN: Duration = Duration::from_secs(2);

pub struct Status {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub cpu: f32,
}

/// The VB-CABLE check at start (a note when none is installed). True if
/// none is installed: on the very first start, the app's page opens its
/// "Virtual cables" window (`cables::report`).
pub fn first_run(
    cfg: &mut Config,
    _path: &std::path::Path,
    headless: bool,
    notes: &mut Vec<String>,
) -> bool {
    let _com = ffi::ComInit::mta();
    match Enumerator::new() {
        Ok(en) => cables::first_run(&en, cfg, headless, notes),
        Err(e) => {
            notes.push(e);
            false
        }
    }
}

/// `vmix --list-devices`: the names to use in `connect =` lines.
pub fn list_devices() -> Result<String, String> {
    let _com = ffi::ComInit::mta();
    let en = Enumerator::new()?;
    let mut o = String::new();
    for (cap, title, hint) in [
        (
            true,
            "Recording devices (use in [stripN] connect =)",
            "default",
        ),
        (
            false,
            "Playback devices (use in [bus.XX] connect =)",
            "default",
        ),
    ] {
        let def = en.default_id(cap);
        o.push_str(title);
        o.push('\n');
        for d in en.devices(cap)? {
            let mark = if def.as_deref() == Some(d.info.id.as_str()) {
                format!("  [{hint}]")
            } else {
                String::new()
            };
            let virt = if route::is_virtual(&d.info.name) {
                "  (virtual)"
            } else {
                ""
            };
            o.push_str(&format!("    {}{mark}{virt}\n", d.info.name));
        }
        o.push('\n');
    }
    Ok(o)
}

pub struct Session {
    shared: Arc<Shared>,
    stats: Arc<Stats>,
    graph: Option<Graph>,
    // Declared after `graph` so the audio threads stop before COM is torn down.
    en: Enumerator,
    signature: String,
    last_scan: Instant,
    dirty: bool,
    built_at: Instant,
    rec_tap: Arc<RecTap>,
    vban_tap: Arc<VbanTap>,
    play_tap: Option<Arc<PlayTap>>,
    _com: ffi::ComInit,
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
        let com = ffi::ComInit::mta();
        let en = Enumerator::new()?;
        let signature = en.signature();
        let stats = Arc::new(Stats::default());
        shared.sample_rate.store(RATE, SeqCst);
        // Start audio right away; the first connect() reuses this graph.
        let graph = match Graph::build(&en, cfg, &shared, &stats, rec_tap.clone(), vban_tap.clone(), Some(play_tap.clone())) {
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
            rec_tap,
            vban_tap,
            play_tap: Some(play_tap),
            en,
            signature,
            last_scan: Instant::now(),
            dirty: false,
            built_at: Instant::now(),
            _com: com,
        })
    }

    pub fn label(&self) -> String {
        "WASAPI shared mode".into()
    }

    /// The audio threads stop by themselves on device errors and the graph
    /// is rebuilt, so there is no fatal "server gone" state on Windows.
    pub fn shutdown(&self) -> bool {
        false
    }

    /// Edge-triggered: true once when a device appeared/disappeared, a
    /// default device changed, or a stream died. The caller debounces and
    /// then calls `connect`, which rebuilds the graph.
    pub fn take_changed(&mut self) -> bool {
        let mut fresh = false;
        if self.stats.invalidated.swap(false, Relaxed) {
            self.dirty = true;
            // Rebuild right away, unless we just did (a device that keeps
            // failing is then retried on the slower rescan tick below).
            fresh = self.built_at.elapsed() >= Duration::from_secs(1);
        }
        if self.last_scan.elapsed() >= RESCAN {
            self.last_scan = Instant::now();
            let sig = self.en.signature();
            if sig != self.signature {
                self.signature = sig;
                self.dirty = true;
                fresh = true;
            } else if self.graph.is_none() || self.dirty {
                self.dirty = true; // retry a failed build now and then
                fresh = true;
            }
        }
        fresh
    }

    /// (Re)open every configured device if anything changed. `force` = the
    /// user pressed `r`.
    pub fn connect(&mut self, cfg: &Config, force: bool) -> (Vec<String>, Vec<String>) {
        if force || self.dirty || self.graph.is_none() {
            self.graph = None; // stop the old threads first
            self.dirty = false;
            self.signature = self.en.signature();
            self.last_scan = Instant::now();
            self.built_at = Instant::now();
            self.shared.sample_rate.store(RATE, SeqCst);
            match Graph::build(&self.en, cfg, &self.shared, &self.stats, self.rec_tap.clone(), self.vban_tap.clone(), self.play_tap.clone()) {
                Ok(g) => self.graph = Some(g),
                Err(e) => return (Vec::new(), vec![e]),
            }
        }
        let g = self.graph.as_ref().expect("graph built");
        (g.desc.clone(), g.errs.clone())
    }

    /// What each channel is connected to now, without (re)building anything:
    /// for every frame of the UI. Nothing while the graph could not be built
    /// (it is retried on the rescan tick, see `take_changed`).
    pub fn wiring(&self) -> Vec<String> {
        self.graph.as_ref().map_or_else(Vec::new, |g| g.desc.clone())
    }

    pub fn status(&self) -> Status {
        Status {
            sample_rate: RATE,
            buffer_size: self.stats.block.load(Relaxed),
            cpu: self.stats.cpu.load(),
        }
    }

    /// (capture side, playback side) latency estimate in frames.
    pub fn io_latency(&self) -> (u32, u32) {
        (
            self.stats.lat_in.load(Relaxed),
            self.stats.lat_out.load(Relaxed),
        )
    }

    pub fn cable_count(&self) -> usize {
        self.graph.as_ref().map_or(0, |g| g.cables)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.graph = None;
        console::CLEAN_EXIT.store(true, SeqCst);
    }
}







