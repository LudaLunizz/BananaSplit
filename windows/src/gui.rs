//! Graphical control surface (`bananasplit --gui`), with no dependencies: a tiny
//! HTTP server on 127.0.0.1 serves one self-contained page (`gui.html`,
//! embedded in the binary) and streams the mixer state to it.
//!
//! * `GET  /`            the page
//! * `GET  /api/events`  Server-Sent Events: one JSON state object per UI frame
//! * `GET  /api/state`   the latest JSON state, once
//! * `GET  /api/takes`   the recordings in the recorder folder (JSON)
//! * `GET  /api/wave?key=K&n=N`  a recording's waveform overview (JSON)
//! * `GET  /api/ping`    `bananasplit-gui` (lets a second `bananasplit --gui` find this one)
//! * `GET  /api/devices` what each channel can be connected to (its menu, JSON)
//! * `GET  /api/update`  the updater's release page and notes (JSON)
//! * `GET  /api/cables`  Splitter and VB-Audio cables, Voicemeeter, Windows defaults (JSON)
//! * `POST /api/cmd`     one control change, form-encoded (`op=gain&ch=0&v=-3.5`)
//!
//! All mixer parameters (EQ, input gain and pan included) are atomics in
//! [`Shared`], so the page changes them exactly like the terminal UI does,
//! with the same click-free ramps. VBAN settings go to [`Vban`], which
//! applies them within a few milliseconds; transport commands go to the
//! [`Player`] and the [`Recorder`]. Things that need the main loop (save,
//! reconnect, rename, quit, window and theme settings) are queued as
//! [`Request`]s.
//!
//! The page is shown by the app window process (`shell.rs`), managed here
//! by [`WindowProc`], or in a browser when WebKitGTK is not installed.
//!
//! Security: listens on 127.0.0.1 only. Every request must carry a `Host`
//! naming this server (defeats DNS rebinding). A `POST` must also carry
//! `X-BananaSplit: 1` and, if the browser sends one, a same-origin `Origin`; a
//! custom header forces a CORS preflight, which this server never grants,
//! so other web pages open in the browser cannot drive the mixer.

use crate::tui::{Info, ROWS};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};
use bananasplit::config::{Config, Theme, parse_pan, parse_trim, parse_zoom};
use bananasplit::engine::{BUS_IDS, MAX_GAIN_DB, MAX_TRIM_DB, MIN_GAIN_DB, MIN_TRIM_DB, NUM_BUSES, NUM_STRIPS, Shared, clamp_db};
use bananasplit::eq::{self, BANDS, DEFAULT_BANDS, Kind};
use bananasplit::player::{self, PlayState, PlayStatus, Player, ScanCache, TakeInfo};
use bananasplit::recorder::{self, ALL_LINKS, Format, Mode, NUM_SOURCES, Recorder, Settings, Status};
use bananasplit::scenes::{self, SceneStore};
use bananasplit::vban::{self, FromAddr, InState, NUM_IN, NUM_OUT, OutState, SampleFormat, Vban, VbanConfig, VbanStatus};

pub const DEFAULT_PORT: u16 = 8790;
const PAGE: &str = include_str!("gui.html");
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const MAX_CONNS: usize = 32;
const MAX_STREAMS: usize = 8;
const MAX_NAME: usize = 32;

/// Work for the main loop.
#[derive(Debug, PartialEq)]
pub enum Request {
    Save,
    Reconnect,
    Quit,
    /// Rename channel (0..5 strips, 6..10 buses).
    Rename(usize, String),
    /// Bring the window up (a second `bananasplit --gui`); the startup /
    /// activation token of the launch, if any.
    Show(String),
    Theme(Theme),
    CloseToTray(bool),
    Zoom(f32),
    /// The current scene ("" = none).
    Scene(String),
    /// "Install updates automatically" on or off.
    AutoUpdate(bool),
    /// Quit and start the version the updater installed.
    Restart,
    /// What a channel is connected to (JACK, the cables, the apps), answered
    /// by the main loop.
    Dev(DevOp, Reply),
}

/// A change to what a channel is connected to, or the list for its menu.
#[derive(Debug, Clone, PartialEq)]
pub enum DevOp {
    /// The menus' contents (JSON).
    List,
    /// Connect (or disconnect) channel `ch` and JACK client `client`.
    Client { ch: usize, client: String, on: bool },
    /// `connect = @default` on or off.
    Default { ch: usize, on: bool },
    /// Remove one `connect` line (and what only it connected).
    Unpattern { ch: usize, pattern: String },
    /// Disconnect everything (the channel's cable stays).
    Nothing { ch: usize },
    /// The channel's virtual cable on or off.
    Cable { ch: usize, on: bool },
    /// Move an app's stream (`rec`: a recording one) into channel `ch`'s
    /// cable; None: back to the default device.
    Stream { rec: bool, id: u32, ch: Option<usize> },
}

/// Where the main loop sends the answer to a [`Request::Dev`].
pub struct Reply(pub mpsc::Sender<Result<String, String>>);

impl PartialEq for Reply {
    fn eq(&self, _: &Reply) -> bool {
        true
    }
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply")
    }
}

impl Reply {
    pub fn send(self, r: Result<String, String>) {
        let _ = self.0.send(r);
    }
}

const THEMES: [Theme; 3] = [Theme::Dark, Theme::Light, Theme::Auto];

/// State shared between the main loop and the HTTP threads.
pub struct Hub {
    pub shared: Arc<Shared>,
    pub recorder: Arc<Recorder>,
    pub vban: Arc<Vban>,
    pub player: Arc<Player>,
    pub takes: Arc<Takes>,
    pub scenes: SceneStore,
    scene_list: Mutex<Vec<String>>,
    /// Index into THEMES, for the page as it is served.
    theme: AtomicU8,
    zoom: AtomicU32,
    pub autostart: AtomicBool,
    pub installed: AtomicBool,
    /// The first start without virtual cables: the page opens its
    /// "Virtual cables" window by itself, once.
    pub cable_welcome: AtomicBool,
    state: Mutex<(u64, Arc<String>)>,
    cond: Condvar,
    names: Mutex<Vec<String>>,
    requests: Mutex<Vec<Request>>,
    closing: AtomicBool,
    conns: AtomicUsize,
    streams: AtomicUsize,
    /// The JACK client name, told to a second launch that probes the port.
    client: Mutex<String>,
    /// Bus sends are checked for loops and set under this lock.
    routing: Mutex<()>,
    /// The updater (the GUI has one; tests may not).
    updater: Mutex<Option<Arc<crate::update::Updater>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    pub fn new(
        shared: Arc<Shared>,
        recorder: Arc<Recorder>,
        vban: Arc<Vban>,
        player: Arc<Player>,
        takes: Arc<Takes>,
        scenes: SceneStore,
    ) -> Arc<Hub> {
        let list = scenes.list();
        Arc::new(Hub {
            shared,
            recorder,
            vban,
            player,
            takes,
            scenes,
            scene_list: Mutex::new(list),
            theme: AtomicU8::new(0),
            zoom: AtomicU32::new(1f32.to_bits()),
            autostart: AtomicBool::new(false),
            installed: AtomicBool::new(false),
            cable_welcome: AtomicBool::new(false),
            state: Mutex::new((0, Arc::new("{}".into()))),
            cond: Condvar::new(),
            names: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            closing: AtomicBool::new(false),
            conns: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
            client: Mutex::new(String::new()),
            routing: Mutex::new(()),
            updater: Mutex::new(None),
        })
    }

    pub fn set_updater(&self, u: Arc<crate::update::Updater>) {
        *lock(&self.updater) = Some(u);
    }

    fn updater(&self) -> Result<Arc<crate::update::Updater>, String> {
        lock(&self.updater).clone().ok_or_else(|| "no updater here".to_string())
    }

    /// Ask the main loop (JACK, the cables, the config) and wait for its answer.
    pub fn ask(&self, op: DevOp) -> Result<String, String> {
        let (tx, rx) = mpsc::channel();
        self.request(Request::Dev(op, Reply(tx)));
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "BananaSplit is busy; try again".to_string())?
    }

    /// Which mixer this is (its JACK client name), for `/api/ping`.
    pub fn set_client(&self, name: &str) {
        *lock(&self.client) = name.to_string();
    }

    /// The 11 channel names (used for multi-track file names).
    pub fn set_names(&self, names: Vec<String>) {
        *lock(&self.names) = names;
    }

    /// New state for every connected page.
    pub fn publish(&self, json: String) {
        let mut st = lock(&self.state);
        st.0 = st.0.wrapping_add(1);
        st.1 = Arc::new(json);
        drop(st);
        self.cond.notify_all();
    }

    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *lock(&self.requests))
    }

    fn request(&self, r: Request) {
        lock(&self.requests).push(r);
    }

    /// End all event streams (on quit).
    pub fn close(&self) {
        self.closing.store(true, SeqCst);
        self.cond.notify_all();
    }

    /// Theme and zoom of the page as it is served (no flash on load).
    pub fn set_look(&self, theme: Theme, zoom: f32) {
        self.theme.store(THEMES.iter().position(|t| *t == theme).unwrap_or(0) as u8, SeqCst);
        self.zoom.store(zoom.to_bits(), SeqCst);
    }

    pub fn refresh_scenes(&self) {
        *lock(&self.scene_list) = self.scenes.list();
    }

    pub fn scene_list(&self) -> Vec<String> {
        lock(&self.scene_list).clone()
    }

    fn page(&self) -> String {
        let theme = THEMES[self.theme.load(SeqCst) as usize % THEMES.len()];
        let zoom = f32::from_bits(self.zoom.load(SeqCst));
        let attrs = match theme {
            Theme::Auto => format!(" data-zoom=\"{zoom}\""),
            t => format!(" data-theme=\"{}\" data-zoom=\"{zoom}\"", t.as_str()),
        };
        PAGE.replacen("<html lang=\"en\">", &format!("<html lang=\"en\"{attrs}>"), 1)
    }
}

// ---------------------------------------------------------------------------
// Recordings in the recorder folder
// ---------------------------------------------------------------------------

struct TakesInner {
    rev: u64,
    list: Arc<Vec<TakeInfo>>,
    folder: PathBuf,
}

/// A cached waveform: (key, frames, bytes, buckets) -> JSON.
type Wave = ((String, u64, u64, usize), Arc<String>);

/// Watches the recorder folder (a background thread rescans it every few
/// seconds, and at once when kicked) and caches waveform overviews.
pub struct Takes {
    inner: Mutex<TakesInner>,
    kick: (Mutex<bool>, Condvar),
    stop: AtomicBool,
    waves: Mutex<Vec<Wave>>,
}

impl Takes {
    pub fn start(folder: PathBuf) -> Arc<Takes> {
        let t = Arc::new(Takes {
            inner: Mutex::new(TakesInner { rev: 0, list: Arc::new(Vec::new()), folder }),
            kick: (Mutex::new(true), Condvar::new()),
            stop: AtomicBool::new(false),
            waves: Mutex::new(Vec::new()),
        });
        let t2 = t.clone();
        let _ = std::thread::Builder::new().name("bs-takes".into()).spawn(move || t2.run());
        t
    }

    fn run(&self) {
        let mut cache = ScanCache::default();
        while !self.stop.load(SeqCst) {
            {
                let (m, c) = &self.kick;
                let g = lock(m);
                let (mut g, _) = c.wait_timeout_while(g, Duration::from_secs(3), |k| !*k).unwrap_or_else(|e| e.into_inner());
                *g = false;
            }
            if self.stop.load(SeqCst) {
                break;
            }
            let folder = lock(&self.inner).folder.clone();
            // A file the scan trips over must not end the take list for the
            // rest of the session: keep the last list and try again later.
            let scanned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| player::scan(&folder, &mut cache)));
            let Ok(list) = scanned else {
                cache = ScanCache::default();
                continue;
            };
            let mut g = lock(&self.inner);
            let same = g.folder == folder && *g.list == list;
            if !same && g.folder == folder {
                g.rev += 1;
                g.list = Arc::new(list);
            }
        }
    }

    /// Rescan now (a recording finished, the folder changed).
    pub fn kick(&self) {
        let (m, c) = &self.kick;
        *lock(m) = true;
        c.notify_all();
    }

    pub fn set_folder(&self, folder: PathBuf) {
        let mut g = lock(&self.inner);
        if g.folder != folder {
            g.folder = folder;
            g.list = Arc::new(Vec::new());
            g.rev += 1;
            drop(g);
            self.kick();
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, SeqCst);
        self.kick();
    }

    pub fn snapshot(&self) -> (u64, Arc<Vec<TakeInfo>>) {
        let g = lock(&self.inner);
        (g.rev, g.list.clone())
    }

    pub fn find(&self, key: &str) -> Option<TakeInfo> {
        lock(&self.inner).list.iter().find(|t| t.key == key).cloned()
    }

    /// `{"key":…,"n":…,"secs":…,"peaks":[…]}` for take `key`.
    pub fn wave_json(&self, key: &str, n: usize) -> Result<Arc<String>, String> {
        let take = self.find(key).ok_or("no such recording")?;
        let id = (take.key.clone(), take.frames, take.bytes, n);
        if let Some((_, j)) = lock(&self.waves).iter().find(|(k, _)| *k == id) {
            return Ok(j.clone());
        }
        let peaks = player::overview(&take, n)?;
        let mut o = String::with_capacity(16 + 4 * n);
        o.push_str("{\"key\":");
        jstr(&mut o, &take.key);
        let _ = write!(o, ",\"n\":{n},\"secs\":");
        jnum(&mut o, take.secs(), 3);
        o.push_str(",\"peaks\":[");
        for (i, p) in peaks.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            let _ = write!(o, "{p}");
        }
        o.push_str("]}");
        let j = Arc::new(o);
        let mut w = lock(&self.waves);
        w.push((id, j.clone()));
        if w.len() > 24 {
            w.remove(0);
        }
        Ok(j)
    }

    /// The list for `GET /api/takes`, oldest first (TAKE 1 is the first).
    pub fn list_json(&self) -> String {
        let (rev, list) = self.snapshot();
        let mut o = String::with_capacity(256 + 160 * list.len());
        let _ = write!(o, "{{\"rev\":{rev},\"takes\":[");
        for (i, t) in list.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"key\":");
            jstr(&mut o, &t.key);
            let _ = write!(o, ",\"n\":{},\"when\":", i + 1);
            jopt(&mut o, t.when.as_deref());
            o.push_str(",\"label\":");
            jstr(&mut o, &t.label());
            o.push_str(",\"tracks\":[");
            for (k, tr) in t.tracks.iter().enumerate() {
                if k > 0 {
                    o.push(',');
                }
                jstr(&mut o, &tr.label);
            }
            o.push_str("],\"secs\":");
            jnum(&mut o, t.secs(), 3);
            let _ = write!(o, ",\"sr\":{},\"fmt\":", t.sample_rate);
            jstr(&mut o, &t.fmt);
            let _ = write!(o, ",\"bytes\":{}}}", t.bytes);
        }
        o.push_str("]}");
        o
    }
}

// ---------------------------------------------------------------------------
// State as JSON
// ---------------------------------------------------------------------------

pub(crate) fn jstr(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
}

fn jnum(o: &mut String, v: f64, decimals: usize) {
    let v = if v.is_finite() { v } else { 0.0 };
    let _ = write!(o, "{v:.decimals$}");
}

/// Remove ANSI escape sequences (status texts are shared with the TUI).
pub fn plain(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
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

/// What the page shows about a channel's connections.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChanView {
    /// In a few words: what it takes in / plays on ("Yeti · Discord").
    pub dev: String,
    /// Its virtual cable exists.
    pub cable: bool,
}

pub struct Frame<'a> {
    pub info: &'a Info<'a>,
    pub cfg: &'a Config,
    pub meters: &'a [[f32; 2]; ROWS],
    /// Input levels (shown greyed while a channel is muted).
    pub in_meters: &'a [[f32; 2]; ROWS],
    pub conn: &'a [String],
    pub chans: &'a [ChanView],
    /// Bumped when what the channels' menus list changes.
    pub dev_rev: u64,
    /// This computer's IPv4 addresses (for the VBAN window).
    pub ips: &'a [crate::sys::LocalIp],
    pub status: &'a str,
    pub rec: &'a Status,
    pub rec_settings: &'a Settings,
    pub vban: &'a VbanStatus,
    pub vban_cfg: &'a VbanConfig,
    pub extra: &'a Extra<'a>,
}

/// Player, recordings, disk, scenes and window state for the page.
pub struct Extra<'a> {
    pub player: &'a PlayStatus,
    pub play_to: u8,
    pub takes_rev: u64,
    pub takes_count: usize,
    pub newest: Option<&'a str>,
    /// (total, free) bytes where recordings go.
    pub disk: Option<(u64, u64)>,
    pub scenes: &'a [String],
    pub autostart: bool,
    pub installed: bool,
    /// "app" (our own window), "browser" or "none".
    pub window: &'a str,
    pub tray: bool,
    /// The updater's state (a JSON object, `Updater::json`).
    pub update: &'a str,
}

fn jopt(o: &mut String, s: Option<&str>) {
    match s {
        Some(s) => jstr(o, s),
        None => o.push_str("null"),
    }
}

fn in_state(s: InState) -> &'static str {
    match s {
        InState::Off => "off",
        InState::Blocked => "blocked",
        InState::Waiting => "waiting",
        InState::Receiving => "receiving",
    }
}

fn out_state(s: OutState) -> &'static str {
    match s {
        OutState::Off => "off",
        OutState::Blocked => "blocked",
        OutState::Sending => "sending",
    }
}

fn vban_json(o: &mut String, st: &VbanStatus, c: &VbanConfig, sample_rate: u32, ips: &[crate::sys::LocalIp]) {
    let _ = write!(o, "{{\"on\":{},\"port\":{},\"ips\":[", c.enabled, c.port);
    for (k, a) in ips.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str("{\"if\":");
        jstr(o, &a.iface);
        let _ = write!(o, ",\"ip\":\"{}\",\"bc\":", a.ip);
        jopt(o, a.broadcast.map(|b| b.to_string()).as_deref());
        o.push('}');
    }
    o.push_str("],\"listening\":");
    match st.listening {
        Some(p) => {
            let _ = write!(o, "{p}");
        }
        None => o.push_str("null"),
    }
    o.push_str(",\"error\":");
    jopt(o, if c.enabled { st.error.as_deref() } else { None });
    o.push_str(",\"in\":[");
    for (k, s) in c.ins.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        let t = &st.ins[k];
        let _ = write!(o, "{{\"on\":{},\"name\":", s.on);
        jstr(o, &s.name);
        o.push_str(",\"from\":");
        jstr(o, &s.from.as_string());
        let _ = write!(o, ",\"strip\":{},\"buffer\":{},\"state\":\"{}\",\"text\":", s.strip, s.buffer_ms, in_state(t.state));
        jstr(o, &t.describe(s));
        let _ = write!(o, ",\"lost\":{},\"dropouts\":{},\"ppm\":", t.lost, t.underruns);
        jnum(o, t.clock_ppm as f64, 1);
        o.push('}');
    }
    o.push_str("],\"out\":[");
    for (k, s) in c.outs.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        let t = &st.outs[k];
        let _ = write!(o, "{{\"on\":{},\"name\":", s.on);
        jstr(o, &s.name);
        o.push_str(",\"to\":");
        jstr(o, &s.to.map_or(String::new(), |ip| ip.to_string()));
        let _ = write!(
            o,
            ",\"port\":{},\"src\":{},\"format\":\"{}\",\"channels\":{},\"state\":\"{}\",\"text\":",
            s.port,
            s.source,
            s.format.as_str(),
            s.channels,
            out_state(t.state)
        );
        jstr(o, &t.describe(s, sample_rate));
        let _ = write!(o, ",\"packets\":{},\"errors\":{}}}", t.packets, t.errors);
    }
    o.push_str("],\"seen\":[");
    for (k, s) in st.seen.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str("{\"name\":");
        jstr(o, &s.name);
        o.push_str(",\"from\":");
        jstr(o, &s.from.ip().to_string());
        let _ = write!(o, ",\"sr\":{},\"ch\":{},\"format\":", s.sample_rate, s.channels);
        jstr(o, s.format.label());
        o.push('}');
    }
    o.push_str("]}");
}

pub fn state_json(sh: &Shared, f: &Frame) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let i = f.info;
    let mut o = String::with_capacity(4096);
    let period = if i.sample_rate > 0 { i.buffer_size as f64 * 1000.0 / i.sample_rate as f64 } else { 0.0 };
    let measured = i.sample_rate > 0 && (i.lat_in > 0 || i.lat_out > 0);
    let rtt = if measured { (i.lat_in + i.lat_out) as f64 * 1000.0 / i.sample_rate as f64 } else { 2.0 * period };
    o.push_str("{\"client\":");
    jstr(&mut o, i.client);
    let _ = write!(o, ",\"sr\":{},\"bs\":{},\"period\":", i.sample_rate, i.buffer_size);
    jnum(&mut o, period, 2);
    o.push_str(",\"rtt\":");
    jnum(&mut o, rtt, 2);
    let _ = write!(o, ",\"measured\":{measured},\"cpu\":");
    jnum(&mut o, i.cpu as f64, 1);
    let _ = write!(o, ",\"xruns\":{},\"oversize\":{},\"cables\":{},\"devRev\":{},\"status\":", i.xruns, i.oversize, i.cables, f.dev_rev);
    jstr(&mut o, &plain(f.status));
    let any_solo = sh.strips.iter().any(|s| s.solo.load(Relaxed));
    let links = sh.rec_links.load(Relaxed);
    let _ = write!(o, ",\"anySolo\":{any_solo},\"ch\":[");
    for r in 0..ROWS {
        if r > 0 {
            o.push(',');
        }
        let strip = r < NUM_STRIPS;
        let (name, gain, mute, mono) = if strip {
            let p = &sh.strips[r];
            (&f.cfg.strips[r].name, p.gain_db.load(), p.mute.load(Relaxed), p.mono.load(Relaxed))
        } else {
            let p = &sh.buses[r - NUM_STRIPS];
            (&f.cfg.buses[r - NUM_STRIPS].name, p.gain_db.load(), p.mute.load(Relaxed), p.mono.load(Relaxed))
        };
        o.push_str("{\"id\":");
        jstr(&mut o, recorder::source_id(r));
        o.push_str(",\"name\":");
        jstr(&mut o, name);
        let _ = write!(o, ",\"strip\":{strip},\"gain\":");
        jnum(&mut o, gain as f64, 1);
        let (trim, pan) = if strip {
            (sh.strips[r].trim_db.load(), sh.strips[r].pan.load())
        } else {
            (sh.buses[r - NUM_STRIPS].trim_db.load(), sh.buses[r - NUM_STRIPS].pan.load())
        };
        o.push_str(",\"trim\":");
        jnum(&mut o, trim as f64, 1);
        o.push_str(",\"pan\":");
        jnum(&mut o, pan as f64, 2);
        let _ = write!(o, ",\"mute\":{mute},\"mono\":{mono},\"link\":{}", links & (1 << r) != 0);
        if strip {
            let p = &sh.strips[r];
            let _ = write!(o, ",\"solo\":{},\"routes\":{}", p.solo.load(Relaxed), p.routes.load(Relaxed));
        } else {
            let _ = write!(o, ",\"routes\":{}", sh.bus_routes()[r - NUM_STRIPS]);
        }
        o.push_str(",\"m\":[");
        jnum(&mut o, f.meters[r][0] as f64, 1);
        o.push(',');
        jnum(&mut o, f.meters[r][1] as f64, 1);
        o.push_str("],\"mi\":[");
        jnum(&mut o, f.in_meters[r][0] as f64, 1);
        o.push(',');
        jnum(&mut o, f.in_meters[r][1] as f64, 1);
        o.push_str("],\"conn\":");
        jstr(&mut o, f.conn.get(r).map_or("", String::as_str));
        let cv = f.chans.get(r).cloned().unwrap_or_default();
        o.push_str(",\"dev\":");
        jstr(&mut o, &cv.dev);
        let _ = write!(o, ",\"cable\":{}", cv.cable);
        // (last, so a naive reader of the fields above still finds them
        // before the first closing brace)
        let e = sh.eq(r);
        let _ = write!(o, ",\"eq\":{{\"on\":{},\"b\":[", e.on.load(Relaxed));
        for (k, b) in e.bands().iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            let _ = write!(o, "[{},\"{}\",", u8::from(b.on), b.kind.as_str());
            jnum(&mut o, b.freq as f64, 0);
            o.push(',');
            jnum(&mut o, b.gain_db as f64, 1);
            o.push(',');
            jnum(&mut o, b.q as f64, 2);
            o.push(']');
        }
        o.push_str("]}}");
    }
    let st = f.rec;
    let s = f.rec_settings;
    let _ = write!(
        o,
        "],\"rec\":{{\"on\":{},\"takeMode\":\"{}\",\"mode\":\"{}\",\"format\":\"{}\",\"kbps\":{},\"sr\":{},\"folder\":",
        st.on,
        st.mode.as_str(),
        s.mode.as_str(),
        s.format.as_str(),
        s.kbps,
        st.sample_rate
    );
    jstr(&mut o, &s.folder);
    o.push_str(",\"path\":");
    jstr(&mut o, &recorder::expand_home(&s.folder).display().to_string());
    o.push_str(",\"secs\":");
    jnum(&mut o, st.seconds(), 2);
    let _ = write!(o, ",\"dropped\":{},\"clipped\":{},\"tracks\":{},\"files\":[", st.dropped, st.clipped, st.tracks);
    for (k, p) in st.files.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        jstr(&mut o, &p.display().to_string());
    }
    o.push_str("],\"error\":");
    match &st.error {
        Some(e) => jstr(&mut o, e),
        None => o.push_str("null"),
    }
    o.push_str(",\"note\":");
    jopt(&mut o, st.note.as_deref());
    o.push_str("},\"vban\":");
    vban_json(&mut o, f.vban, f.vban_cfg, i.sample_rate, f.ips);
    let x = f.extra;
    let p = x.player;
    let _ = write!(o, ",\"player\":{{\"state\":\"{}\",\"take\":", p.state.as_str());
    jopt(&mut o, p.take.as_deref());
    o.push_str(",\"pos\":");
    jnum(&mut o, p.pos, 2);
    o.push_str(",\"dur\":");
    jnum(&mut o, p.dur, 2);
    let _ = write!(o, ",\"underruns\":{},\"error\":", p.underruns);
    jopt(&mut o, p.error.as_deref());
    let _ = write!(o, "}},\"playTo\":{},\"takes\":{{\"rev\":{},\"count\":{},\"newest\":", x.play_to, x.takes_rev, x.takes_count);
    jopt(&mut o, x.newest);
    o.push_str("},\"disk\":");
    match x.disk {
        Some((total, free)) => {
            let _ = write!(o, "{{\"total\":{total},\"free\":{free}}}");
        }
        None => o.push_str("null"),
    }
    o.push_str(",\"scene\":");
    jstr(&mut o, &f.cfg.gui.scene);
    o.push_str(",\"scenes\":[");
    for (k, s) in x.scenes.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        jstr(&mut o, s);
    }
    let g = &f.cfg.gui;
    let _ = write!(
        o,
        "],\"app\":{{\"theme\":\"{}\",\"closeToTray\":{},\"zoom\":{},\"autostart\":{},\"installed\":{},\"window\":\"{}\",\"tray\":{}}}",
        g.theme.as_str(),
        g.close_to_tray,
        g.zoom,
        x.autostart,
        x.installed,
        x.window,
        x.tray
    );
    o.push_str(",\"update\":");
    o.push_str(if x.update.is_empty() { "null" } else { x.update });
    o.push('}');
    o
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

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

struct Form(Vec<(String, String)>);

impl Form {
    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }
    fn req(&self, k: &str) -> Result<&str, String> {
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

/// "C", "L25", "R100".
pub fn pan_text(p: f32) -> String {
    let n = (p * 100.0).round() as i32;
    match n {
        0 => "C".into(),
        n if n < 0 => format!("L{}", -n),
        n => format!("R{n}"),
    }
}

fn set_bit_u8(a: &std::sync::atomic::AtomicU8, bit: usize, on: bool) {
    if on { a.fetch_or(1 << bit, SeqCst) } else { a.fetch_and(!(1u8 << bit), SeqCst) };
}

/// Apply one control change. Returns a short message for the page.
/// The take in the player, as the take list has it now, if that is not
/// what the player loaded: a take selected while it was being recorded has
/// grown since (resuming or seeking would stop at the old end).
fn changed_take(hub: &Hub, st: &PlayStatus) -> Option<TakeInfo> {
    let t = hub.takes.find(st.take.as_deref()?)?;
    ((t.secs() - st.dur).abs() > 1e-6).then_some(t)
}

pub fn command(hub: &Hub, body: &str) -> Result<String, String> {
    let f = Form(parse_form(body)?);
    let sh = &*hub.shared;
    let op = f.req("op")?;
    match op {
        "gain" => {
            let c = f.ch()?;
            let v: f32 = f.req("v")?.parse().map_err(|_| "gain must be a number")?;
            if !v.is_finite() {
                return Err("gain must be finite".into());
            }
            let v = clamp_db((v * 10.0).round() / 10.0);
            if c < NUM_STRIPS { sh.strips[c].gain_db.store(v) } else { sh.buses[c - NUM_STRIPS].gain_db.store(v) }
            Ok(format!("{v:+.1} dB"))
        }
        "trim" => {
            let c = f.ch()?;
            let v = parse_trim(f.req("v")?)?;
            if c < NUM_STRIPS { sh.strips[c].trim_db.store(v) } else { sh.buses[c - NUM_STRIPS].trim_db.store(v) }
            Ok(format!("gain {v:+.1} dB"))
        }
        "pan" => {
            let c = f.ch()?;
            let v = parse_pan(f.req("v")?)?;
            if c < NUM_STRIPS { sh.strips[c].pan.store(v) } else { sh.buses[c - NUM_STRIPS].pan.store(v) }
            Ok(pan_text(v))
        }
        "mute" | "mono" => {
            let (c, on) = (f.ch()?, f.flag()?);
            let a = match (op, c < NUM_STRIPS) {
                ("mute", true) => &sh.strips[c].mute,
                ("mute", false) => &sh.buses[c - NUM_STRIPS].mute,
                (_, true) => &sh.strips[c].mono,
                (_, false) => &sh.buses[c - NUM_STRIPS].mono,
            };
            a.store(on, SeqCst);
            Ok("ok".into())
        }
        "solo" => {
            let c = f.ch()?;
            if c >= NUM_STRIPS {
                return Err("buses have no solo".into());
            }
            sh.strips[c].solo.store(f.flag()?, SeqCst);
            Ok("ok".into())
        }
        "route" => {
            let c = f.ch()?;
            let b: usize = f.req("bus")?.parse().ok().filter(|&b| b < NUM_BUSES).ok_or("bad bus")?;
            let on = f.flag()?;
            if c < NUM_STRIPS {
                set_bit_u8(&sh.strips[c].routes, b, on);
                return Ok("ok".into());
            }
            // a bus feeding another bus: never itself, never in a loop
            let from = c - NUM_STRIPS;
            let _g = lock(&hub.routing);
            if on && from == b {
                return Err(format!("{} cannot feed itself", BUS_IDS[from]));
            }
            if on && sh.send_loops(from, b) {
                return Err(format!("{} already feeds {} (through its sends): that would be a loop", BUS_IDS[b], BUS_IDS[from]));
            }
            set_bit_u8(&sh.buses[from].routes, b, on);
            Ok("ok".into())
        }
        // ---- what a channel is connected to (the main loop does it) ----
        "cable" => hub.ask(DevOp::Cable { ch: f.ch()?, on: f.flag()? }),
        "dev" => {
            let client = f.req("client")?.to_string();
            if client.is_empty() || client.len() > 320 || client.chars().any(char::is_control) {
                return Err("bad client name".into());
            }
            hub.ask(DevOp::Client { ch: f.ch()?, client, on: f.flag()? })
        }
        "devdefault" => hub.ask(DevOp::Default { ch: f.ch()?, on: f.flag()? }),
        "devpattern" => hub.ask(DevOp::Unpattern { ch: f.ch()?, pattern: f.req("pattern")?.to_string() }),
        "devnone" => hub.ask(DevOp::Nothing { ch: f.ch()? }),
        "stream" => {
            let rec = match f.req("kind")? {
                "play" => false,
                "rec" => true,
                k => return Err(format!("unknown stream kind {k:?}")),
            };
            let id: u32 = f.req("id")?.parse().map_err(|_| "bad stream id")?;
            let ch = match f.get("ch") {
                None | Some("" | "none") => None,
                Some(_) => Some(f.ch()?),
            };
            hub.ask(DevOp::Stream { rec, id, ch })
        }
        "link" => {
            let (c, on) = (f.ch()?, f.flag()?);
            if on { sh.rec_links.fetch_or(1 << c, SeqCst) } else { sh.rec_links.fetch_and(!(1u16 << c), SeqCst) };
            Ok("ok".into())
        }
        "rec" => {
            let rec = &hub.recorder;
            if f.flag()? {
                let names = lock(&hub.names).clone();
                let files = rec.start(sh.rec_links.load(SeqCst) & ALL_LINKS, &names, &crate::sys::local_timestamp())?;
                Ok(match files.len() {
                    1 => format!("recording to {}", files[0].display()),
                    n => format!("recording {n} tracks to {}", files[0].parent().map_or(String::new(), |p| p.display().to_string())),
                })
            } else {
                rec.stop();
                Ok("recording stopped".into())
            }
        }
        "recset" => {
            let rec = &hub.recorder;
            let mut s = rec.settings();
            if let Some(m) = f.get("mode") {
                s.mode = Mode::parse(m)?;
            }
            if let Some(fm) = f.get("format") {
                s.format = Format::parse(fm)?;
            }
            if let Some(k) = f.get("kbps") {
                s.kbps = recorder::parse_kbps(k)?;
            }
            if let Some(d) = f.get("folder") {
                s.folder = d.trim().to_string();
            }
            rec.set_settings(s)?;
            Ok(if rec.is_recording() { "applies to the next recording".into() } else { "ok".into() })
        }
        "name" => {
            let c = f.ch()?;
            let v = f.req("v")?.trim();
            if v.is_empty() || v.chars().count() > MAX_NAME || v.chars().any(char::is_control) {
                return Err(format!("names are 1-{MAX_NAME} characters"));
            }
            hub.request(Request::Rename(c, v.to_string()));
            Ok("ok".into())
        }
        "eq" => {
            let (c, on) = (f.ch()?, f.flag()?);
            sh.eq(c).on.store(on, SeqCst);
            Ok(if on { "EQ on" } else { "EQ off" }.into())
        }
        "band" => {
            let c = f.ch()?;
            let k = f.index("band", BANDS)?;
            let p = &sh.eq(c).bands[k];
            let mut b = p.load();
            if let Some(on) = f.bool_of("on")? {
                b.on = on;
            }
            if let Some(t) = f.get("type") {
                b.kind = Kind::parse(t)?;
            }
            if let Some(v) = f.num_of("f", "frequency", eq::MIN_FREQ, eq::MAX_FREQ)? {
                b.freq = eq::round_freq(v);
            }
            if let Some(v) = f.num_of("g", "gain", eq::MIN_GAIN_DB, eq::MAX_GAIN_DB)? {
                b.gain_db = eq::round_gain(v);
            }
            if let Some(v) = f.num_of("q", "Q", eq::MIN_Q, eq::MAX_Q)? {
                b.q = eq::round_q(v);
            }
            p.store(&b);
            Ok("ok".into())
        }
        "eqreset" => {
            let c = f.ch()?;
            sh.eq(c).store_bands(&DEFAULT_BANDS);
            Ok("EQ bands reset (flat)".into())
        }
        "vban" => {
            let on = f.flag()?;
            hub.vban.update(|c| {
                c.enabled = on;
                Ok(())
            })?;
            Ok(if on { "VBAN on" } else { "VBAN off" }.into())
        }
        "vbanport" => {
            let port = vban::parse_port(f.req("v")?)?;
            hub.vban.update(|c| {
                c.port = port;
                Ok(())
            })?;
            Ok(format!("VBAN listens on UDP port {port}"))
        }
        "vbanin" => {
            let k = f.index("slot", NUM_IN)?;
            let name = vban_name(&f)?;
            let from = f.get("from").map(FromAddr::parse).transpose()?;
            let strip = f.get("strip").map(vban::parse_strip).transpose()?;
            let buffer = f.get("buffer").map(vban::parse_buffer_ms).transpose()?;
            let on = f.bool_of("on")?;
            hub.vban.update(|c| {
                let s = &mut c.ins[k];
                if let Some(v) = name {
                    s.name = v;
                }
                if let Some(v) = from {
                    s.from = v;
                }
                if let Some(v) = strip {
                    s.strip = v;
                }
                if let Some(v) = buffer {
                    s.buffer_ms = v;
                }
                if let Some(v) = on {
                    s.on = v;
                }
                Ok(())
            })?;
            Ok("ok".into())
        }
        "vbanout" => {
            let k = f.index("slot", NUM_OUT)?;
            let name = vban_name(&f)?;
            let to = f.get("to").map(vban::parse_to).transpose()?;
            let port = f.get("port").map(vban::parse_port).transpose()?;
            let src = f.get("src").map(vban::parse_source).transpose()?;
            let format = f.get("format").map(SampleFormat::parse_send).transpose()?;
            let channels = f.get("channels").map(vban::parse_channels).transpose()?;
            let on = f.bool_of("on")?;
            hub.vban.update(|c| {
                let s = &mut c.outs[k];
                if let Some(v) = name {
                    s.name = v;
                }
                if let Some(v) = to {
                    s.to = v;
                }
                if let Some(v) = port {
                    s.port = v;
                }
                if let Some(v) = src {
                    s.source = v;
                }
                if let Some(v) = format {
                    s.format = v;
                }
                if let Some(v) = channels {
                    s.channels = v;
                }
                if let Some(v) = on {
                    s.on = v;
                }
                Ok(())
            })?;
            Ok("ok".into())
        }
        // ---- player ----
        "play" => {
            let p = &hub.player;
            let st = p.status();
            let from = f.num_of("from", "position", 0.0, 1e9)?.map(f64::from);
            let key = match f.get("key") {
                Some(k) => k.to_string(),
                None => match (&st.take, hub.takes.snapshot().1.last()) {
                    (Some(k), _) => k.clone(),
                    (None, Some(t)) => t.key.clone(),
                    (None, None) => return Err("there are no recordings to play yet".into()),
                },
            };
            if st.take.as_deref() == Some(key.as_str()) && from.is_none() {
                if st.state == PlayState::Playing {
                    return Ok("playing".into());
                }
                match changed_take(hub, &st) {
                    Some(t) => p.play(&t, st.pos, true)?,
                    None => p.resume()?,
                }
            } else {
                let take = hub.takes.find(&key).ok_or("that recording is no longer in the recorder folder")?;
                p.play(&take, from.unwrap_or(0.0), true)?;
            }
            Ok("playing".into())
        }
        "take" => {
            let key = f.req("key")?;
            let take = hub.takes.find(key).ok_or("that recording is no longer in the recorder folder")?;
            hub.player.play(&take, 0.0, false)?;
            Ok(format!("{} selected", take.label()))
        }
        "pause" => {
            hub.player.pause()?;
            Ok("paused".into())
        }
        "stop" => {
            hub.player.stop()?;
            Ok("stopped".into())
        }
        "seek" => {
            let v = f.num_of("v", "position", 0.0, 1e9)?.ok_or("missing \"v\"")?;
            let st = hub.player.status();
            match changed_take(hub, &st) {
                Some(t) => hub.player.play(&t, f64::from(v), st.state == PlayState::Playing)?,
                None => hub.player.seek(f64::from(v))?,
            }
            Ok("ok".into())
        }
        "playto" => {
            let b: usize = f.req("bus")?.parse().ok().filter(|&b| b < NUM_BUSES).ok_or("bad bus")?;
            let on = f.flag()?;
            let m = hub.player.routes();
            hub.player.set_routes(if on { m | 1 << b } else { m & !(1u8 << b) });
            Ok("ok".into())
        }
        "openfolder" => {
            let dir = recorder::expand_home(&hub.recorder.settings().folder);
            crate::desktop::open_folder(&dir)?;
            Ok(format!("opened {}", dir.display()))
        }
        // ---- scenes ----
        "scene" => {
            let name = scenes::validate_name(f.req("name")?)?.to_string();
            let msg = match f.req("act")? {
                "load" => {
                    hub.scenes.load(&name, sh)?;
                    hub.request(Request::Scene(name.clone()));
                    format!("scene \"{name}\" loaded")
                }
                "save" => {
                    hub.scenes.save(&name, sh)?;
                    hub.refresh_scenes();
                    hub.request(Request::Scene(name.clone()));
                    format!("scene \"{name}\" saved")
                }
                "delete" => {
                    hub.scenes.delete(&name)?;
                    hub.refresh_scenes();
                    hub.request(Request::Scene(String::new()));
                    format!("scene \"{name}\" deleted")
                }
                a => return Err(format!("unknown scene action {a:?}")),
            };
            Ok(msg)
        }
        // ---- app settings ----
        "theme" => {
            hub.request(Request::Theme(Theme::parse(f.req("v")?)?));
            Ok("ok".into())
        }
        "closetray" => {
            hub.request(Request::CloseToTray(f.flag()?));
            Ok("ok".into())
        }
        "zoom" => {
            hub.request(Request::Zoom(parse_zoom(f.req("v")?)?));
            Ok("ok".into())
        }
        "autostart" => {
            let msg = crate::desktop::set_autostart(f.flag()?)?;
            hub.autostart.store(crate::desktop::autostart_enabled(), SeqCst);
            Ok(msg)
        }
        "install" => {
            let msg = crate::desktop::install()?;
            hub.installed.store(crate::desktop::menu_installed(), SeqCst);
            Ok(msg)
        }
        "cables" => match f.req("a")? {
            "splitter" => {
                crate::desktop::open_page(crate::win::route::SPLITTER_PAGE)?;
                Ok("opening the Splitter page in your browser".into())
            }
            "page" => {
                crate::desktop::open_page(crate::win::cables::VBCABLE_PAGE)?;
                Ok("opening the VB-Audio page in your browser".into())
            }
            "sound" => {
                crate::desktop::open_sound_settings()?;
                Ok("opening the Windows sound settings".into())
            }
            "seen" => {
                hub.cable_welcome.store(false, SeqCst);
                Ok("ok".into())
            }
            a => Err(format!("unknown cables action {a:?}")),
        },
        "autoupdate" => {
            hub.request(Request::AutoUpdate(f.flag()?));
            Ok("ok".into())
        }
        "update" => {
            let u = hub.updater()?;
            match f.req("a")? {
                "check" => u.check(),
                "install" => u.install(),
                "restart" => {
                    if !u.ready() {
                        return Err("no update is installed yet".into());
                    }
                    hub.request(Request::Restart);
                    Ok("restarting BananaSplit".into())
                }
                "page" => {
                    crate::desktop::open_page(&u.page())?;
                    Ok("opening the release page in your browser".into())
                }
                a => Err(format!("unknown update action {a:?}")),
            }
        }
        "show" => {
            let token: String = f.get("token").unwrap_or("").chars().filter(|c| c.is_ascii_graphic()).take(256).collect();
            hub.request(Request::Show(token));
            Ok("ok".into())
        }
        "save" => {
            hub.request(Request::Save);
            Ok("saving".into())
        }
        "reconnect" => {
            hub.request(Request::Reconnect);
            Ok("reconnecting".into())
        }
        "quit" => {
            hub.request(Request::Quit);
            Ok("quitting".into())
        }
        _ => Err(format!("unknown op {op:?}")),
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub query: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Read one request. Errors are (status, message).
pub fn read_request(s: &mut impl Read) -> Result<Req, (u16, &'static str)> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return Err((431, "request header too large"));
        }
        let n = s.read(&mut tmp).map_err(|_| (408, "request timeout"))?;
        if n == 0 {
            return Err((400, "incomplete request"));
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    if head_end > MAX_HEAD {
        return Err((431, "request header too large"));
    }
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| (400, "bad request"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target, version) = (first.next().unwrap_or(""), first.next().unwrap_or(""), first.next().unwrap_or(""));
    if method.is_empty() || !target.starts_with('/') || !version.starts_with("HTTP/1.") || first.next().is_some() {
        return Err((400, "bad request line"));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (k, v) = line.split_once(':').ok_or((400, "bad header"))?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut req = Req { method: method.to_string(), path: path.to_string(), query: query.to_string(), headers, body: Vec::new() };
    if req.header("transfer-encoding").is_some() {
        return Err((411, "send a Content-Length"));
    }
    let len = match req.header("content-length") {
        None => 0,
        Some(v) => v.parse::<usize>().map_err(|_| (400, "bad Content-Length"))?,
    };
    if len > MAX_BODY {
        return Err((413, "request body too large"));
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).map_err(|_| (408, "request timeout"))?;
        if n == 0 {
            return Err((400, "incomplete body"));
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(len);
    req.body = body;
    Ok(req)
}

fn origins(port: u16) -> [String; 3] {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")]
}

/// The request was addressed to this server (not a rebound DNS name).
pub fn host_ok(req: &Req, port: u16) -> bool {
    req.header("host").is_some_and(|h| origins(port).iter().any(|o| o.eq_ignore_ascii_case(h)))
}

/// A state-changing request comes from our own page.
pub fn csrf_ok(req: &Req, port: u16) -> bool {
    if req.header("x-bananasplit") != Some("1") {
        return false;
    }
    if req.header("sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        return false;
    }
    match req.header("origin") {
        None => true,
        Some(o) => origins(port).iter().any(|h| o.eq_ignore_ascii_case(&format!("http://{h}"))),
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8], extra: &str) {
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n{extra}\r\n",
        reason(code),
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
    let _ = s.shutdown(Shutdown::Write);
}

const PAGE_HEADERS: &str = "Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
style-src 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n";

/// The owner (uid) of the socket at the other end of a loopback connection:
/// in `/proc/net/tcp` (or `tcp6`, for a dual-stack IPv6 socket that came in
/// as `::ffff:127.0.0.1`), its line has `peer` as local and `us` as remote
/// address. The kernel prints each 32-bit word of an address in host order.
#[cfg(unix)]
pub fn socket_owner(table: &str, peer: SocketAddr, us: SocketAddr) -> Option<u32> {
    let hex = |a: SocketAddr| {
        let words = |b: &[u8]| b.chunks(4).map(|w| format!("{:08X}", u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))).collect::<String>();
        let ip = match a {
            SocketAddr::V4(v) => words(&v.ip().octets()),
            SocketAddr::V6(v) => words(&v.ip().octets()),
        };
        format!("{ip}:{:04X}", a.port())
    };
    let (p, u) = (hex(peer), hex(us));
    table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 7 && f[1].eq_ignore_ascii_case(&p) && f[2].eq_ignore_ascii_case(&u)).then(|| f[7].parse().ok()).flatten()
    })
}

/// `a` as an IPv4-mapped IPv6 address (how a dual-stack socket sees it).
fn mapped(a: SocketAddr) -> SocketAddr {
    match a {
        SocketAddr::V4(v) => SocketAddr::from((v.ip().to_ipv6_mapped(), v.port())),
        v6 => v6,
    }
}

/// Only processes of the user BananaSplit runs as may use it: other users
/// on this computer can reach 127.0.0.1 too. (Without /proc, as in 2.5.)
fn peer_allowed(s: &TcpStream) -> bool {
    let (Ok(peer), Ok(us)) = (s.peer_addr(), s.local_addr()) else { return false };
    #[cfg(windows)]
    {
        return crate::sys::same_user(peer, us);
    }
    #[cfg(unix)]
    {
        let Ok(t4) = std::fs::read_to_string("/proc/net/tcp") else { return true };
        let owner = socket_owner(&t4, peer, us).or_else(|| {
            let t6 = std::fs::read_to_string("/proc/net/tcp6").ok()?;
            socket_owner(&t6, mapped(peer), mapped(us))
        });
        owner == Some(crate::sys::uid())
    }
}

fn handle(mut s: TcpStream, hub: &Hub, port: u16) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    if !peer_allowed(&s) {
        return respond(&mut s, 403, "text/plain; charset=utf-8", b"BananaSplit only answers the user it runs as", "");
    }
    let req = match read_request(&mut s) {
        Ok(r) => r,
        Err((code, msg)) => return respond(&mut s, code, "text/plain; charset=utf-8", msg.as_bytes(), ""),
    };
    if !host_ok(&req, port) {
        return respond(&mut s, 403, "text/plain; charset=utf-8", b"wrong Host", "");
    }
    let text = "text/plain; charset=utf-8";
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            respond(&mut s, 200, "text/html; charset=utf-8", hub.page().as_bytes(), PAGE_HEADERS)
        }
        ("GET", "/api/ping") => {
            let who = format!("X-BananaSplit-Client: {}\r\n", url_encode(&lock(&hub.client)));
            respond(&mut s, 200, text, b"bananasplit-gui", &who)
        }
        ("GET", "/api/update") => match hub.updater() {
            Ok(u) => respond(&mut s, 200, "application/json", u.details_json().as_bytes(), ""),
            Err(e) => respond(&mut s, 404, text, e.as_bytes(), ""),
        },
        ("GET", "/api/cables") => {
            let json = crate::win::cables::report(hub.cable_welcome.load(SeqCst));
            respond(&mut s, 200, "application/json", json.as_bytes(), "")
        }
        ("GET", "/api/takes") => respond(&mut s, 200, "application/json", hub.takes.list_json().as_bytes(), ""),
        ("GET", "/api/devices") => match hub.ask(DevOp::List) {
            Ok(j) => respond(&mut s, 200, "application/json", j.as_bytes(), ""),
            Err(e) => respond(&mut s, 503, text, e.as_bytes(), ""),
        },
        ("GET", "/api/wave") => {
            let q = parse_form(&req.query).map(Form);
            let r = q.and_then(|q| {
                let key = q.req("key")?.to_string();
                let n = q.get("n").map_or(Ok(480), |v| v.parse::<usize>().map_err(|_| "bad n".to_string()))?;
                hub.takes.wave_json(&key, n.clamp(16, 2048))
            });
            match r {
                Ok(j) => respond(&mut s, 200, "application/json", j.as_bytes(), ""),
                Err(e) => respond(&mut s, 404, text, e.as_bytes(), ""),
            }
        }
        ("GET", "/api/state") => {
            let json = lock(&hub.state).1.clone();
            respond(&mut s, 200, "application/json", json.as_bytes(), "")
        }
        ("GET", "/api/events") => stream(s, hub),
        ("POST", "/api/cmd") => {
            if !csrf_ok(&req, port) {
                return respond(&mut s, 403, text, b"forbidden", "");
            }
            let Ok(body) = std::str::from_utf8(&req.body) else {
                return respond(&mut s, 400, text, b"body is not UTF-8", "");
            };
            match command(hub, body) {
                Ok(m) => respond(&mut s, 200, text, m.as_bytes(), ""),
                Err(e) => respond(&mut s, 400, text, e.as_bytes(), ""),
            }
        }
        ("GET", _) | ("POST", _) => respond(&mut s, 404, text, b"not found", ""),
        _ => respond(&mut s, 405, text, b"method not allowed", "Allow: GET, POST\r\n"),
    }
}

struct Count<'a>(&'a AtomicUsize);
impl Drop for Count<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

fn stream(mut s: TcpStream, hub: &Hub) {
    if hub.streams.fetch_add(1, SeqCst) >= MAX_STREAMS {
        hub.streams.fetch_sub(1, SeqCst);
        return respond(&mut s, 503, "text/plain", b"too many windows open", "");
    }
    let _count = Count(&hub.streams);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
                X-Content-Type-Options: nosniff\r\nConnection: keep-alive\r\n\r\nretry: 1000\n\n";
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut last = u64::MAX;
    loop {
        let (seq, json) = {
            let g = lock(&hub.state);
            let (g, _) = hub
                .cond
                .wait_timeout_while(g, Duration::from_secs(1), |st| st.0 == last && !hub.closing.load(SeqCst))
                .unwrap_or_else(|e| e.into_inner());
            (g.0, g.1.clone())
        };
        if hub.closing.load(SeqCst) {
            break;
        }
        let msg = if seq != last {
            last = seq;
            format!("data: {json}\n\n")
        } else {
            ": ping\n\n".to_string()
        };
        if s.write_all(msg.as_bytes()).is_err() {
            break;
        }
    }
}

/// Listen on 127.0.0.1:`port` (if it is busy and was not asked for
/// explicitly, on a free port instead). Returns the port in use.
pub fn start(hub: Arc<Hub>, port: u16, explicit: bool) -> Result<u16, String> {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => l,
        Err(e) if !explicit && e.kind() == std::io::ErrorKind::AddrInUse => TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("cannot open a port for the GUI: {e}"))?,
        Err(e) => return Err(format!("cannot listen on 127.0.0.1:{port} for the GUI: {e}")),
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::Builder::new()
        .name("bs-gui".into())
        .spawn(move || accept_loop(&listener, &hub, port))
        .map_err(|e| format!("cannot start the GUI thread: {e}"))?;
    Ok(port)
}

fn accept_loop(l: &TcpListener, hub: &Arc<Hub>, port: u16) {
    for conn in l.incoming() {
        if hub.closing.load(SeqCst) {
            break;
        }
        let Ok(mut s) = conn else {
            std::thread::sleep(Duration::from_millis(20)); // e.g. out of file descriptors
            continue;
        };
        if hub.conns.fetch_add(1, SeqCst) >= MAX_CONNS {
            hub.conns.fetch_sub(1, SeqCst);
            respond(&mut s, 503, "text/plain", b"busy", "");
            continue;
        }
        let h = hub.clone();
        let spawned = std::thread::Builder::new().name("bs-gui-conn".into()).spawn(move || {
            let _count = Count(&h.conns);
            handle(s, &h, port);
        });
        if spawned.is_err() {
            hub.conns.fetch_sub(1, SeqCst);
        }
    }
}

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// Where a running GUI notes its port (`$XDG_RUNTIME_DIR/bananasplit/<client>.port`),
/// so that launching it again and `--quit` find it when 8790 was busy.
fn port_file(client: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache")))?;
        let name: String = client.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        return Some(base.join("bananasplit").join(format!("{name}.port")));
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local")))
            .unwrap_or_else(std::env::temp_dir);
        let name: String = client.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        Some(base.join("bananasplit").join(format!("{name}.port")))
    }
}

/// Note the port this GUI listens on.
pub fn note_port(client: &str, port: u16) {
    let Some(f) = port_file(client) else { return };
    if let Some(d) = f.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d);
        }
        #[cfg(windows)]
        {
            let _ = std::fs::create_dir_all(d);
        }
    }
    let tmp = f.with_extension("port-tmp");
    if std::fs::write(&tmp, format!("{port}\n")).is_ok() && std::fs::rename(&tmp, &f).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Remove the note again, if it is still this GUI's.
pub fn forget_port(client: &str, port: u16) {
    if let Some(f) = port_file(client) {
        if noted(&f) == Some(port) {
            let _ = std::fs::remove_file(&f);
        }
    }
}

fn noted(f: &std::path::Path) -> Option<u16> {
    std::fs::read_to_string(f).ok()?.trim().parse().ok()
}

/// The port of a BananaSplit GUI that is already running for `client`:
/// `explicit` if given, else the noted port, else the default port.
pub fn find_running(client: &str, explicit: Option<u16>) -> Option<u16> {
    if let Some(p) = explicit {
        return probe(p, client).then_some(p);
    }
    let noted = port_file(client).and_then(|f| noted(&f)).filter(|&p| p != DEFAULT_PORT);
    noted.into_iter().chain([DEFAULT_PORT]).find(|&p| probe(p, client))
}

/// Is a BananaSplit GUI for JACK client `client` answering on this port?
/// (One that does not say which client it is, from before 2.6, counts.)
pub fn probe(port: u16, client: &str) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(800)));
    let req = format!("GET /api/ping HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.take(4096).read_to_end(&mut buf);
    if !(buf.starts_with(b"HTTP/1.1 200") && buf.ends_with(b"\r\n\r\nbananasplit-gui")) {
        return false;
    }
    let text = String::from_utf8_lossy(&buf);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let said = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("x-bananasplit-client").then(|| v.trim().to_string())
    });
    said.is_none_or(|c| c == url_encode(client))
}

/// Open the page in its own app window (Chromium-family `--app` mode), or
/// in the default browser. Returns false if nothing could be started.
#[cfg(unix)]
pub fn open_window(url: &str) -> bool {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    #[cfg(unix)]
    crate::sys::cloexec_all();
    let app = format!("--app={url}");
    let browsers = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "brave-browser",
        "brave",
        "microsoft-edge",
        "vivaldi",
    ];
    let mut tries: Vec<(&str, &str)> = browsers.iter().map(|b| (*b, app.as_str())).collect();
    tries.push(("xdg-open", url));
    for (bin, arg) in tries {
        let child = Command::new(bin)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0) // not killed by the terminal's Ctrl-C
            .spawn();
        if let Ok(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.wait(); // reap it
            });
            return true;
        }
    }
    false
}

/// `bananasplit.exe` is a console program: its window process gets no
/// console window of its own, and does not keep the mixer's open.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Open the page in a native desktop window of its own, not the mixer's
/// (`WindowProc`): only when the running mixer cannot be asked to show its
/// window. Closing it closes it.
#[cfg(windows)]
pub fn open_window(url: &str) -> bool {
    use std::os::windows::process::CommandExt;
    let Ok(exe) = crate::desktop::helper_exe() else { return false };
    Command::new(exe)
        .arg(WINDOW_FLAG)
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Ask a running BananaSplit GUI to bring its window up. False if nothing
/// (or an older BananaSplit without this request) answered.
pub fn request_show(port: u16, token: &str) -> bool {
    post_cmd(port, &format!("op=show&token={}", url_encode(token))).is_some_and(|(code, _)| code == 200)
}

/// Ask a running BananaSplit GUI to quit.
pub fn request_quit(port: u16) -> Option<(u16, String)> {
    post_cmd(port, "op=quit")
}

fn url_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            let _ = write!(o, "%{b:02X}");
        }
    }
    o
}

fn post_cmd(port: u16, body: &str) -> Option<(u16, String)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let req = format!(
        "POST /api/cmd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-BananaSplit: 1\r\nContent-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let code = text.strip_prefix("HTTP/1.1 ")?.get(..3)?.parse().ok()?;
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_string();
    Some((code, body))
}

// ---------------------------------------------------------------------------
// The app window process (shell.rs)
// ---------------------------------------------------------------------------

pub const WINDOW_FLAG: &str = "--window";
/// argv[0] of the helper processes (also when started from /proc/self/exe).
#[cfg(unix)]
const APP_ARG0: &str = "bananasplit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinMode {
    /// Our own window (GTK + WebKitGTK), with the tray icon.
    App,
    /// Only the tray icon; the page is shown in a browser.
    Tray,
}

#[derive(Debug, PartialEq)]
pub enum WinMsg {
    /// Show the page in a browser (tray mode).
    Open,
    /// Start or stop recording (tray menu).
    Record,
    Quit,
    Size(u32, u32),
    /// The window process ended.
    Gone,
}

pub struct WinOpts<'a> {
    pub url: &'a str,
    pub hidden: bool,
    pub close_to_tray: bool,
    pub size: Option<(u32, u32)>,
    pub token: Option<&'a str>,
}

/// One line from the window process.
pub fn parse_win_line(line: &str) -> Option<WinMsg> {
    let mut w = line.split_whitespace();
    match w.next()? {
        "open" => Some(WinMsg::Open),
        "record" => Some(WinMsg::Record),
        "quit" => Some(WinMsg::Quit),
        "size" => {
            let (a, b) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
            Some(WinMsg::Size(a, b))
        }
        _ => None,
    }
}

/// The window process, seen from the mixer.
pub struct WindowProc {
    child: Child,
    stdin: Option<ChildStdin>,
    pub mode: WinMode,
    /// It has a tray icon (`ready app tray`): closing the window hides it there.
    pub tray: bool,
    rx: mpsc::Receiver<WinMsg>,
    gone: bool,
}

impl WindowProc {
    /// Start it and wait until it says what it can do. `Err` explains why
    /// there is neither a window nor a tray icon (then use a browser).
    pub fn spawn(o: &WinOpts) -> Result<WindowProc, String> {
        let exe = crate::desktop::helper_exe()?;
        let mut cmd = Command::new(exe);
        #[cfg(unix)]
        cmd.arg0(APP_ARG0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.arg(WINDOW_FLAG).arg(o.url).arg("--close-to-tray").arg(if o.close_to_tray { "1" } else { "0" });
        if o.hidden {
            cmd.arg("--hidden");
        }
        if let Some((w, h)) = o.size {
            cmd.arg("--size").arg(format!("{w}x{h}"));
        }
        if let Some(t) = o.token.filter(|t| !t.is_empty()) {
            cmd.arg("--token").arg(t);
        }
        // The launch token was for this window, not for anything it starts.
        cmd.env_remove("DESKTOP_STARTUP_ID").env_remove("XDG_ACTIVATION_TOKEN");
        // Windows: its messages come as `fail …` lines (it has no console).
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(if cfg!(windows) { Stdio::null() } else { Stdio::inherit() });
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the window: {e}"))?;
        let stdout = child.stdout.take().ok_or("no pipe from the window")?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(WinMode, bool), String>>();
        let reader = std::thread::Builder::new().name("bs-window".into()).spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(r) = ready.take() {
                    let first = if let Some(m) = line.strip_prefix("ready ") {
                        let mode = if m.starts_with("app") { WinMode::App } else { WinMode::Tray };
                        Ok((mode, m.split_whitespace().any(|w| w == "tray")))
                    } else {
                        Err(line.strip_prefix("fail ").unwrap_or(&line).to_string())
                    };
                    let failed = first.is_err();
                    let _ = r.send(first);
                    if failed {
                        break;
                    }
                    continue;
                }
                if let Some(m) = parse_win_line(&line) {
                    if tx.send(m).is_err() {
                        break;
                    }
                }
            }
            let _ = tx.send(WinMsg::Gone);
        });
        if reader.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cannot start a thread".into());
        }
        let stdin = child.stdin.take();
        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok((mode, tray))) => Ok(WindowProc { child, stdin, mode, tray, rx, gone: false }),
            Ok(Err(why)) => {
                let _ = child.wait();
                Err(why)
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                Err("the window did not start".into())
            }
        }
    }

    pub fn send(&mut self, line: &str) {
        if let Some(s) = &mut self.stdin {
            if s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).and_then(|_| s.flush()).is_err() {
                self.stdin = None;
            }
        }
    }

    pub fn poll(&mut self) -> Vec<WinMsg> {
        let v: Vec<WinMsg> = self.rx.try_iter().collect();
        if v.contains(&WinMsg::Gone) {
            self.gone = true;
        }
        v
    }

    pub fn alive(&mut self) -> bool {
        !self.gone && matches!(self.child.try_wait(), Ok(None))
    }

    /// Close it (it saves nothing; the mixer has the state).
    pub fn close(mut self) {
        self.send("quit");
        self.stdin = None; // end of input: it quits even if it missed "quit"
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// The page hard-codes the fader range and the channel layout.
const _: () = assert!(MIN_GAIN_DB == -60.0 && MAX_GAIN_DB == 12.0, "gui.html assumes -60..+12 dB");
const _: () = assert!(MIN_TRIM_DB == -24.0 && MAX_TRIM_DB == 24.0, "gui.html assumes -24..+24 dB of input gain");
const _: () = assert!(NUM_STRIPS == 6 && NUM_BUSES == 5, "gui.html assumes 6 strips and 5 buses");

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;
    use bananasplit::recorder::RecTap;

    /// A hub in a folder of its own. Tests run in parallel and `hub_in`
    /// starts by removing its folder, so two tests sharing one would delete
    /// each other's recordings (on Windows, mid-take).
    fn hub() -> Arc<Hub> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        hub_in(&format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Relaxed)))
    }

    /// A hub whose recorder folder and scenes live in a fresh folder.
    fn hub_in(tag: &str) -> Arc<Hub> {
        let base = std::env::temp_dir().join(format!("bananasplit-gui-test-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("rec");
        let rec = Recorder::new(
            RecTap::new(),
            48000,
            Settings { folder: dir.display().to_string(), ..Settings::default() },
        );
        let vban = Vban::start(bananasplit::vban::VbanTap::new(), 48000, VbanConfig::default());
        let player = Arc::new(Player::new(bananasplit::player::PlayTap::new(), 48000));
        let takes = Takes::start(dir);
        Hub::new(Shared::new(), Arc::new(rec), Arc::new(vban), player, takes, SceneStore::new(base.join("scenes")))
    }

    fn extra<'a>(p: &'a PlayStatus, scenes: &'a [String]) -> Extra<'a> {
        Extra {
            player: p,
            play_to: 1,
            takes_rev: 3,
            takes_count: 2,
            newest: Some("2026-10-21_14-03-05"),
            disk: Some((64_000_000_000, 52_000_000_000)),
            scenes,
            autostart: false,
            installed: true,
            window: "app",
            tray: true,
            update: r#"{"auto":false,"phase":"idle"}"#,
        }
    }

    fn req(raw: &str) -> Result<Req, (u16, &'static str)> {
        read_request(&mut raw.as_bytes())
    }

    #[test]
    fn form_and_url_decoding() {
        let f = parse_form("op=name&ch=2&v=Voice+%26+Chat%21&x=").unwrap();
        assert_eq!(f[2], ("v".into(), "Voice & Chat!".into()));
        assert_eq!(f[3], ("x".into(), String::new()));
        assert!(parse_form("v=%zz").is_err());
        assert!(parse_form("v=%e2%82").is_err(), "invalid UTF-8");
        assert_eq!(parse_form("").unwrap(), vec![]);
    }

    #[test]
    fn request_parsing_and_limits() {
        let r = req("POST /api/cmd?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8790\r\nContent-Length: 5\r\n\r\nop=sx").unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.body.as_slice()), ("POST", "/api/cmd", &b"op=sx"[..]));
        assert_eq!(r.header("host"), Some("127.0.0.1:8790"));
        assert_eq!(req("GET / HTTP/1.1\r\nHost: x").unwrap_err().0, 400, "incomplete");
        assert_eq!(req("GARBAGE\r\n\r\n").unwrap_err().0, 400);
        assert_eq!(req("GET / HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n").unwrap_err().0, 413);
        assert_eq!(req("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap_err().0, 411);
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD + 10));
        assert_eq!(req(&big).unwrap_err().0, 431);
    }

    #[test]
    fn host_and_csrf_checks() {
        let ok = req("POST /api/cmd HTTP/1.1\r\nHost: localhost:8790\r\nX-BananaSplit: 1\r\nOrigin: http://localhost:8790\r\n\r\n")
            .unwrap();
        assert!(host_ok(&ok, 8790) && csrf_ok(&ok, 8790));
        assert!(!host_ok(&ok, 8791));
        let rebound = req("GET / HTTP/1.1\r\nHost: evil.example:8790\r\n\r\n").unwrap();
        assert!(!host_ok(&rebound, 8790));
        let no_header = req("POST /api/cmd HTTP/1.1\r\nHost: 127.0.0.1:8790\r\n\r\n").unwrap();
        assert!(!csrf_ok(&no_header, 8790));
        let foreign =
            req("POST /api/cmd HTTP/1.1\r\nHost: 127.0.0.1:8790\r\nX-BananaSplit: 1\r\nOrigin: https://evil.example\r\n\r\n")
                .unwrap();
        assert!(!csrf_ok(&foreign, 8790));
        let cross = req("POST /api/cmd HTTP/1.1\r\nHost: 127.0.0.1:8790\r\nX-BananaSplit: 1\r\nSec-Fetch-Site: cross-site\r\n\r\n")
            .unwrap();
        assert!(!csrf_ok(&cross, 8790));
    }

    #[test]
    fn commands_change_shared_state() {
        let h = hub();
        let sh = &h.shared;
        assert_eq!(command(&h, "op=gain&ch=0&v=-3.04").unwrap(), "-3.0 dB");
        assert_eq!(sh.strips[0].gain_db.load(), -3.0);
        command(&h, "op=gain&ch=10&v=99").unwrap();
        assert_eq!(sh.buses[4].gain_db.load(), MAX_GAIN_DB);
        assert!(command(&h, "op=gain&ch=0&v=NaN").is_err());
        assert!(command(&h, "op=gain&ch=11&v=0").is_err());
        command(&h, "op=mute&ch=7&v=1").unwrap();
        assert!(sh.buses[1].mute.load(Relaxed));
        command(&h, "op=mono&ch=2&v=1").unwrap();
        assert!(sh.strips[2].mono.load(Relaxed));
        command(&h, "op=solo&ch=5&v=1").unwrap();
        assert!(sh.strips[5].solo.load(Relaxed));
        assert!(command(&h, "op=solo&ch=6&v=1").is_err(), "no solo on buses");
        command(&h, "op=route&ch=1&bus=3&v=1").unwrap();
        command(&h, "op=route&ch=1&bus=0&v=1").unwrap();
        command(&h, "op=route&ch=1&bus=0&v=1").unwrap(); // idempotent, not a toggle
        assert_eq!(sh.strips[1].routes.load(Relaxed), 0b01001);
        command(&h, "op=route&ch=1&bus=3&v=0").unwrap();
        assert_eq!(sh.strips[1].routes.load(Relaxed), 0b00001);
        assert!(command(&h, "op=route&ch=1&bus=5&v=1").is_err());
        command(&h, "op=link&ch=0&v=1").unwrap();
        command(&h, "op=link&ch=9&v=1").unwrap();
        assert_eq!(sh.rec_links.load(Relaxed), (1 << 9) | 1);
        command(&h, "op=link&ch=0&v=0").unwrap();
        assert_eq!(sh.rec_links.load(Relaxed), 1 << 9);
        command(&h, "op=recset&mode=multi&format=wav32f&folder=+%2Ftmp%2Fx+").unwrap();
        let s = h.recorder.settings();
        assert_eq!((s.mode, s.format, s.folder.as_str()), (Mode::Multi, Format::Wav32f, "/tmp/x"));
        assert!(command(&h, "op=recset&format=ogg").is_err());
        assert!(command(&h, "op=name&ch=3&v=").is_err());
        assert!(command(&h, "op=name&ch=3&v=a%0Ab").is_err());
        command(&h, "op=name&ch=3&v=+Voice+").unwrap();
        // EQ
        command(&h, "op=eq&ch=7&v=1").unwrap();
        assert!(sh.buses[1].eq.on.load(Relaxed));
        command(&h, "op=band&ch=0&band=5&type=lowcut&f=80.4&q=0.707&on=0").unwrap();
        let b = sh.strips[0].eq.bands[5].load();
        assert_eq!((b.kind, b.freq, b.q, b.on, b.gain_db), (Kind::LowCut, 80.0, 0.71, false, 0.0));
        command(&h, "op=band&ch=0&band=5&g=-3.04").unwrap();
        assert_eq!(sh.strips[0].eq.bands[5].load().gain_db, -3.0);
        for bad in ["op=band&ch=0&band=6&g=1", "op=band&ch=0&band=0&g=30", "op=band&ch=0&band=0&f=10", "op=band&ch=0&band=0&q=0", "op=band&ch=0&band=0&type=comb", "op=band&ch=0&band=0&g=NaN", "op=band&ch=11&band=0&g=1", "op=band&ch=0&band=0&on=2"] {
            assert!(command(&h, bad).is_err(), "{bad}");
        }
        command(&h, "op=eqreset&ch=0").unwrap();
        assert_eq!(sh.strips[0].eq.bands(), DEFAULT_BANDS);
        // VBAN
        command(&h, "op=vban&v=1").unwrap();
        command(&h, "op=vbanport&v=6990").unwrap();
        command(&h, "op=vbanin&slot=2&on=1&name=+Voicemeeter+&from=192.168.1.20&strip=4&buffer=20").unwrap();
        command(&h, "op=vbanout&slot=7&on=1&name=Mix&to=192.168.1.255&port=7000&src=B2&format=float32&channels=1").unwrap();
        let c = h.vban.config();
        assert!(c.enabled && c.port == 6990);
        assert_eq!(c.ins[2], vban::InStream { on: true, name: "Voicemeeter".into(), from: FromAddr::Ip([192, 168, 1, 20].into()), strip: 3, buffer_ms: 20 });
        assert_eq!(c.outs[7], vban::OutStream { on: true, name: "Mix".into(), to: Some([192, 168, 1, 255].into()), port: 7000, source: 10, format: SampleFormat::F32, channels: 1 });
        for bad in ["op=vbanin&slot=8&on=1", "op=vbanin&slot=0&name=0123456789abcdefg", "op=vbanin&slot=0&from=1.2.3", "op=vbanin&slot=0&strip=0", "op=vbanin&slot=0&buffer=999", "op=vbanout&slot=0&to=any", "op=vbanout&slot=0&src=C1", "op=vbanout&slot=0&format=mp3", "op=vbanout&slot=0&channels=3", "op=vbanout&slot=0&port=0", "op=vbanport&v=x"] {
            assert!(command(&h, bad).is_err(), "{bad}");
        }
        assert_eq!(h.vban.config(), c, "a rejected change changes nothing");
        command(&h, "op=vbanin&slot=2&from=").unwrap();
        assert_eq!(h.vban.config().ins[2].from, FromAddr::Unset);
        command(&h, "op=vban&v=0").unwrap();
        command(&h, "op=save").unwrap();
        command(&h, "op=quit").unwrap();
        assert_eq!(h.take_requests(), vec![Request::Rename(3, "Voice".into()), Request::Save, Request::Quit]);
        assert!(h.take_requests().is_empty());
        assert!(command(&h, "op=explode").is_err());
        assert!(command(&h, "ch=1").is_err());
    }

    #[test]
    fn record_command_starts_and_stops() {
        let h = hub();
        h.set_names((0..NUM_SOURCES).map(|i| format!("n{i}")).collect());
        let dir = recorder::expand_home(&h.recorder.settings().folder);
        assert!(command(&h, "op=rec&v=1").unwrap_err().contains("link"));
        command(&h, "op=link&ch=6&v=1").unwrap();
        let msg = command(&h, "op=rec&v=1").unwrap();
        assert!(msg.starts_with("recording to ") && msg.ends_with("_mix.mp3"), "{msg}");
        assert!(h.recorder.is_recording());
        command(&h, "op=rec&v=0").unwrap();
        assert!(!h.recorder.is_recording());
        h.recorder.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_is_well_formed_and_escaped() {
        let h = hub();
        let mut cfg = Config::default();
        cfg.strips[0].name = "Mic \"1\" <b>\\".into();
        h.shared.rec_links.store(1, Relaxed);
        h.shared.strips[0].routes.store(0b01000, Relaxed);
        let info = Info {
            client: "BananaSplit",
            sample_rate: 48000,
            buffer_size: 128,
            cpu: f32::NAN,
            xruns: 3,
            oversize: 0,
            lat_in: 128,
            lat_out: 256,
            cables: 8,
            vban: "",
        };
        let meters = [[-60.0f32, -12.5]; ROWS];
        let st = h.recorder.status();
        h.shared.strips[2].eq.on.store(true, Relaxed);
        h.shared.buses[0].eq.bands[3].gain_db.store(-4.5);
        let mut vcfg = VbanConfig { enabled: true, ..VbanConfig::default() };
        vcfg.ins[1].on = true;
        vcfg.ins[1].name = "Voice <1>".into();
        vcfg.outs[0].to = Some([192, 168, 1, 20].into());
        let mut vst = VbanStatus { listening: Some(6980), ..VbanStatus::default() };
        vst.ins[1].state = InState::Waiting;
        vst.seen.push(vban::Seen {
            name: "Stream1".into(),
            from: "10.0.0.2:6980".parse().unwrap(),
            sample_rate: 44100,
            channels: 2,
            format: SampleFormat::I24,
            last: std::time::Instant::now(),
        });
        h.shared.strips[1].trim_db.store(6.0);
        h.shared.buses[4].pan.store(-0.25);
        h.shared.buses[3].routes.store(0b00001, Relaxed); // B1 -> A1
        cfg.gui.scene = "Live <Room>".into();
        let ps = h.player.status();
        let scene_list = vec!["Live <Room>".to_string()];
        let x = extra(&ps, &scene_list);
        let in_meters = [[-30.0f32, -31.5]; ROWS];
        let mut chans = vec![ChanView { dev: "Not connected".into(), cable: false }; ROWS];
        chans[0] = ChanView { dev: "Yeti <USB>".into(), cable: true };
        let ips = [crate::sys::LocalIp { iface: "enp3s0".into(), ip: [192, 168, 1, 23].into(), broadcast: Some([192, 168, 1, 255].into()) }];
        let json = state_json(
            &h.shared,
            &Frame {
                info: &info,
                cfg: &cfg,
                meters: &meters,
                in_meters: &in_meters,
                conn: &["Firefox".into()],
                chans: &chans,
                dev_rev: 42,
                ips: &ips,
                status: "\x1b[33msaved\x1b[0m\nok",
                rec: &st,
                rec_settings: &h.recorder.settings(),
                vban: &vst,
                vban_cfg: &vcfg,
                extra: &x,
            },
        );
        assert!(!json.contains('\n'), "one SSE data line");
        let (lt, gt) = (format!("\\u{:04x}", '<' as u32), format!("\\u{:04x}", '>' as u32));
        assert!(json.contains(&format!(r#""name":"Mic \"1\" {lt}b{gt}\\""#)), "{json}");
        assert!(!json.contains('<') && !json.contains('>'));
        assert!(json.contains(r#""status":"saved\nok""#));
        assert!(json.contains(r#""rtt":8.00"#) && json.contains(r#""cpu":0.0"#));
        assert!(json.contains(r#""id":"1","name""#) && json.contains(r#""id":"B2""#));
        assert!(json.contains(&format!(r#""link":true,"solo":false,"routes":8,"m":[-60.0,-12.5],"mi":[-30.0,-31.5],"conn":"Firefox","dev":"Yeti {lt}USB{gt}","cable":true,"eq":{{"on":false,"b":[[1,"lowshelf",100,0.0,0.71],[1,"bell",250,0.0,1.00],"#)), "{json}");
        assert!(json.contains(r#""id":"B1","name":"Virtual B1","strip":false,"gain":0.0,"trim":0.0,"pan":0.00,"mute":false,"mono":false,"link":false,"routes":1,"m":"#), "{json}");
        assert!(json.contains(r#""cables":8,"devRev":42,"#));
        assert!(json.contains(r#"[1,"highshelf",10000,0.0,0.71]]}},{"id":"2""#));
        assert!(json.contains(r#""id":"3","name":"Desktop","strip":true,"gain":0.0,"trim":0.0,"pan":0.00,"mute":false,"mono":false,"link":false,"solo":false,"routes":0,"m":[-60.0,-12.5],"mi":[-30.0,-31.5],"conn":"","dev":"Not connected","cable":false,"eq":{"on":true"#), "{json}");
        assert!(json.contains(r#""id":"2","name":"Discord","strip":true,"gain":0.0,"trim":6.0,"pan":0.00,"#), "{json}");
        assert!(json.contains(r#""id":"B2","name":"Virtual B2","strip":false,"gain":0.0,"trim":0.0,"pan":-0.25,"#), "{json}");
        assert!(json.contains(r#"[1,"bell",1500,-4.5,1.00]"#));
        assert!(json.contains(r#""rec":{"on":false"#) && json.contains(r#""error":null,"note":null},"vban":{"on":true,"port":6980,"ips":[{"if":"enp3s0","ip":"192.168.1.23","bc":"192.168.1.255"}],"listening":6980,"error":null,"in":[{"on":false,"name":"Stream1","from":"","strip":5,"buffer":10,"state":"off""#), "{json}");
        let (lt, gt) = (format!("\\u{:04x}", '<' as u32), format!("\\u{:04x}", '>' as u32));
        assert!(json.contains(&format!(r#"{{"on":true,"name":"Voice {lt}1{gt}","from":"","strip":5,"buffer":10,"state":"waiting","text":"waiting for \"Voice {lt}1{gt}\" from any sender""#)), "{json}");
        assert!(json.contains(r#""out":[{"on":false,"name":"Stream1","to":"192.168.1.20","port":6980,"src":9,"format":"int16","channels":2,"state":"off","text":"off""#), "{json}");
        assert!(json.contains(r#""seen":[{"name":"Stream1","from":"10.0.0.2","sr":44100,"ch":2,"format":"24-bit"}]},"player":"#), "{json}");
        assert!(json.contains(r#","player":{"state":"stopped","take":null,"pos":0.00,"dur":0.00,"underruns":0,"error":null},"playTo":1,"takes":{"rev":3,"count":2,"newest":"2026-10-21_14-03-05"},"disk":{"total":64000000000,"free":52000000000},"#), "{json}");
        assert!(json.ends_with(&format!(r#""scene":"Live {lt}Room{gt}","scenes":["Live {lt}Room{gt}"],"app":{{"theme":"dark","closeToTray":true,"zoom":1,"autostart":false,"installed":true,"window":"app","tray":true}},"update":{{"auto":false,"phase":"idle"}}}}"#)), "{json}");
        // balanced brackets outside strings
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for c in json.chars() {
            match (in_str, esc, c) {
                (true, true, _) => esc = false,
                (true, false, '\\') => esc = true,
                (true, false, '"') => in_str = false,
                (false, _, '"') => in_str = true,
                (false, _, '{' | '[') => depth += 1,
                (false, _, '}' | ']') => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert!(!in_str);
    }

    #[test]
    fn input_gain_pan_player_and_settings_commands() {
        let h = hub_in("cmds");
        let sh = &h.shared;
        assert_eq!(command(&h, "op=trim&ch=0&v=6.04").unwrap(), "gain +6.0 dB");
        assert_eq!(sh.strips[0].trim_db.load(), 6.0);
        command(&h, "op=trim&ch=10&v=-99").unwrap();
        assert_eq!(sh.buses[4].trim_db.load(), MIN_TRIM_DB);
        assert!(command(&h, "op=trim&ch=0&v=nan").is_err());
        assert_eq!(command(&h, "op=pan&ch=1&v=-0.254").unwrap(), "L25");
        assert_eq!(sh.strips[1].pan.load(), -0.25);
        assert_eq!(command(&h, "op=pan&ch=7&v=3").unwrap(), "R100");
        assert_eq!(command(&h, "op=pan&ch=7&v=0").unwrap(), "C");
        assert!(command(&h, "op=pan&ch=11&v=0").is_err());
        command(&h, "op=playto&bus=3&v=1").unwrap();
        command(&h, "op=playto&bus=0&v=0").unwrap();
        assert_eq!(h.player.routes(), 0b01000);
        assert!(command(&h, "op=playto&bus=5&v=1").is_err());
        // no recordings yet
        assert!(command(&h, "op=play").unwrap_err().contains("no recordings"));
        assert!(command(&h, "op=play&key=nope").unwrap_err().contains("no longer"));
        assert!(command(&h, "op=take&key=nope").is_err());
        assert!(command(&h, "op=seek&v=-1").is_err());
        command(&h, "op=pause").unwrap();
        command(&h, "op=stop").unwrap();
        // scenes
        sh.strips[2].gain_db.store(-9.0);
        assert_eq!(command(&h, "op=scene&act=save&name=Live+Room").unwrap(), "scene \"Live Room\" saved");
        assert_eq!(h.scene_list(), ["Live Room"]);
        sh.strips[2].gain_db.store(0.0);
        command(&h, "op=scene&act=load&name=Live+Room").unwrap();
        assert_eq!(sh.strips[2].gain_db.load(), -9.0);
        assert!(command(&h, "op=scene&act=load&name=..%2Fx").is_err());
        assert!(command(&h, "op=scene&act=rename&name=x").is_err());
        command(&h, "op=scene&act=delete&name=Live+Room").unwrap();
        assert!(h.scene_list().is_empty());
        // app settings go to the main loop
        command(&h, "op=theme&v=light").unwrap();
        command(&h, "op=closetray&v=0").unwrap();
        command(&h, "op=zoom&v=1.25").unwrap();
        command(&h, "op=show&token=abc%0A%20d").unwrap();
        assert!(command(&h, "op=theme&v=pink").is_err());
        assert!(command(&h, "op=zoom&v=9").is_err());
        assert_eq!(
            h.take_requests(),
            vec![
                Request::Scene("Live Room".into()),
                Request::Scene("Live Room".into()),
                Request::Scene(String::new()),
                Request::Theme(Theme::Light),
                Request::CloseToTray(false),
                Request::Zoom(1.25),
                Request::Show("abcd".into()),
            ]
        );
        h.player.shutdown();
        h.takes.shutdown();
    }

    #[test]
    fn takes_and_waveforms_are_served() {
        let h = hub_in("takes");
        let folder = recorder::expand_home(&h.recorder.settings().folder);
        std::fs::create_dir_all(&folder).unwrap();
        // written elsewhere and moved in, so the scanner never sees half a file
        let tmp = folder.join("tmp.part");
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(&recorder::wav_header(Format::Wav16, 48000, 4 * 4800)).unwrap();
        for i in 0..4800i16 {
            let v = if i < 2400 { 0i16 } else { 16384 };
            f.write_all(&v.to_le_bytes()).unwrap();
            f.write_all(&v.to_le_bytes()).unwrap();
        }
        drop(f);
        std::fs::rename(&tmp, folder.join("2026-10-21_14-03-05_mix.wav")).unwrap();
        h.takes.kick();
        let t0 = Instant::now();
        while h.takes.snapshot().1.is_empty() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let j = h.takes.list_json();
        assert!(j.contains(r#""takes":[{"key":"2026-10-21_14-03-05","n":1,"when":"2026-10-21 14:03:05","label":"mix","tracks":["mix"],"secs":0.100,"sr":48000,"fmt":"16-bit","bytes":"#), "{j}");
        let w = h.takes.wave_json("2026-10-21_14-03-05", 4).unwrap();
        assert_eq!(w.as_str(), r#"{"key":"2026-10-21_14-03-05","n":4,"secs":0.100,"peaks":[0,0,50,50]}"#);
        assert!(h.takes.wave_json("missing", 4).is_err());
        // the player can take it from here
        command(&h, "op=take&key=2026-10-21_14-03-05").unwrap();
        let t0 = Instant::now();
        while h.player.status().take.is_none() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let st = h.player.status();
        assert_eq!((st.state, st.take.as_deref()), (PlayState::Stopped, Some("2026-10-21_14-03-05")));
        assert!((st.dur - 0.1).abs() < 1e-9);
        // and over HTTP
        let port = start(h.clone(), 0, true).unwrap();
        let get = |path: &str| -> String {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out
        };
        assert!(get("/api/takes").contains(r#""label":"mix""#));
        assert!(get("/api/wave?key=2026-10-21_14-03-05&n=16").contains(r#""n":16,"#));
        assert!(get("/api/wave?key=nope").starts_with("HTTP/1.1 404"));
        h.close();
        h.player.shutdown();
        h.takes.shutdown();
        let _ = std::fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn a_take_loaded_while_it_was_recorded_plays_to_its_new_end() {
        let h = hub_in("grown");
        let folder = recorder::expand_home(&h.recorder.settings().folder);
        std::fs::create_dir_all(&folder).unwrap();
        let key = "2026-10-21_14-03-05";
        // a take of `frames` frames, written elsewhere and moved in
        let write = |frames: u32| {
            let tmp = folder.join("tmp.part");
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(&recorder::wav_header(Format::Wav16, 48000, 4 * frames)).unwrap();
            f.write_all(&vec![0u8; 4 * frames as usize]).unwrap();
            drop(f);
            std::fs::rename(&tmp, folder.join(format!("{key}_mix.wav"))).unwrap();
            h.takes.kick();
        };
        let wait = |what: &str, ok: &dyn Fn() -> bool| {
            let t0 = Instant::now();
            while !ok() {
                assert!(t0.elapsed() < Duration::from_secs(5), "{what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let listed = |secs: f64| h.takes.find(key).is_some_and(|t| (t.secs() - secs).abs() < 1e-9);
        write(4800); // 0.1 s: selected while it records
        wait("listed", &|| listed(0.1));
        command(&h, &format!("op=take&key={key}")).unwrap();
        wait("loaded", &|| (h.player.status().dur - 0.1).abs() < 1e-9);
        write(48000); // recording went on: 1 s
        wait("listed again", &|| listed(1.0));
        // seeking past the old end plays the whole take now
        command(&h, "op=seek&v=0.5").unwrap();
        wait("reloaded by seek", &|| (h.player.status().dur - 1.0).abs() < 1e-9);
        let st = h.player.status();
        assert!((st.pos - 0.5).abs() < 1e-6, "{st:?}");
        // and Play (resume) after it grew again
        write(96000);
        wait("listed a third time", &|| listed(2.0));
        command(&h, "op=play").unwrap();
        wait("reloaded by play", &|| (h.player.status().dur - 2.0).abs() < 1e-9);
        h.close();
        h.player.shutdown();
        h.takes.shutdown();
        let _ = std::fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn page_gets_the_theme_and_zoom() {
        let h = hub_in("page");
        assert!(h.page().starts_with("<!doctype html>\n<html lang=\"en\" data-theme=\"dark\" data-zoom=\"1\">"));
        h.set_look(Theme::Auto, 1.25);
        assert!(h.page().contains("<html lang=\"en\" data-zoom=\"1.25\">"));
        h.set_look(Theme::Light, 1.0);
        assert!(h.page().contains("<html lang=\"en\" data-theme=\"light\" data-zoom=\"1\">"));
        h.player.shutdown();
        h.takes.shutdown();
    }

    #[test]
    fn window_process_lines() {
        assert_eq!(parse_win_line("quit"), Some(WinMsg::Quit));
        assert_eq!(parse_win_line("record"), Some(WinMsg::Record));
        assert_eq!(parse_win_line("open"), Some(WinMsg::Open));
        assert_eq!(parse_win_line("size 1600 900"), Some(WinMsg::Size(1600, 900)));
        assert_eq!(parse_win_line("size x"), None);
        assert_eq!(parse_win_line("bogus"), None);
        assert_eq!(url_encode("a b/c&d=é"), "a%20b%2Fc%26d%3D%C3%A9");
        assert_eq!(pan_text(-1.0), "L100");
        assert_eq!(pan_text(0.004), "C");
    }

    #[cfg(unix)]
    #[test]
    fn only_our_user_gets_in() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:2256 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0 100 0 0 10 0\n\
           1: 0100007F:A1B2 0100007F:2256 01 00000000:00000000 00:00000000 00000000  1001        0 2 1 0 20 4 30 10 -1\n\
           2: 0100007F:2256 0100007F:A1B2 01 00000000:00000000 00:00000000 00000000  1000        0 3 1 0 20 4 30 10 -1\n";
        let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
        if cfg!(target_endian = "little") {
            assert_eq!(socket_owner(table, at(0xA1B2), at(0x2256)), Some(1001), "the client's own line");
            assert_eq!(socket_owner(table, at(0xA1B3), at(0x2256)), None);
            // a dual-stack IPv6 client (Java, .NET) is listed in tcp6 as ::ffff:127.0.0.1
            let t6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
               0: 0000000000000000FFFF00000100007F:A1B2 0000000000000000FFFF00000100007F:2256 01 00000000:00000000 00:00000000 00000000  1002        0 4 1 0 20 4 30 10 -1\n";
            assert_eq!(socket_owner(t6, mapped(at(0xA1B2)), mapped(at(0x2256))), Some(1002));
            assert_eq!(socket_owner(t6, at(0xA1B2), at(0x2256)), None, "IPv4 form does not match a tcp6 line");
        }
        // a real connection: the owner is this process's user
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (srv, _) = l.accept().unwrap();
        if let Ok(t) = std::fs::read_to_string("/proc/net/tcp") {
            assert_eq!(socket_owner(&t, srv.peer_addr().unwrap(), srv.local_addr().unwrap()), Some(crate::sys::uid()));
            assert!(peer_allowed(&srv));
        }
        drop(c);
    }

    #[test]
    fn page_is_self_contained() {
        assert!(PAGE.starts_with("<!doctype html>\n<html lang=\"en\">"), "Hub::page() injects the theme here");
        // no external resources: everything works offline and under the CSP
        for bad in ["http://", "https://", "<link", "src=\"", "localStorage"] {
            assert!(!PAGE.contains(bad), "page references {bad}");
        }
        assert!(PAGE.contains("X-BananaSplit"));
        assert!(PAGE.contains("/api/events") && PAGE.contains("/api/takes") && PAGE.contains("/api/wave?"));
        // dark (the default), light, and auto (follow the desktop)
        assert!(PAGE.contains(r#":root[data-theme="light"]"#) && PAGE.contains(r#":root:not([data-theme])"#));
        // every command the page sends exists
        for op in ["gain", "trim", "pan", "mute", "mono", "solo", "route", "link", "rec", "recset", "name", "eq", "band",
                   "eqreset", "vban", "vbanport", "vbanin", "vbanout", "play", "pause", "stop", "seek", "take", "playto",
                   "openfolder", "scene", "theme", "closetray", "zoom", "autostart", "install", "save", "reconnect", "quit",
                   "autoupdate", "update"] {
            let sent = [format!("op: \"{op}\""), format!("data-op=\"{op}\""), format!("(ch, \"{op}\""), format!("(\"{op}\", k")];
            assert!(sent.iter().any(|p| PAGE.contains(p.as_str())), "page never sends {op}");
        }
    }

    #[test]
    fn plain_strips_ansi() {
        assert_eq!(plain("\x1b[31;1msave failed\x1b[0m: x"), "save failed: x");
    }

    #[test]
    fn server_round_trip() {
        let h = hub();
        h.set_client("Banana Split");
        let port = start(h.clone(), 0, true).unwrap();
        assert!(port > 0);
        assert!(probe(port, "Banana Split"));
        assert!(!probe(port, "BananaSplit"), "another mixer's GUI is not ours");
        h.publish("{\"x\":1}".into());
        let get = |path: &str, extra: &str| -> String {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out
        };
        let page = get("/", "");
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("Content-Security-Policy"));
        assert!(get("/api/state", "").ends_with("{\"x\":1}"));
        assert!(get("/nope", "").starts_with("HTTP/1.1 404"));
        let post = |body: &str, hdr: &str| -> String {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            write!(s, "POST /api/cmd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{hdr}Content-Length: {}\r\n\r\n{body}", body.len())
                .unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out
        };
        assert!(post("op=mute&ch=0&v=1", "").starts_with("HTTP/1.1 403"));
        assert!(!h.shared.strips[0].mute.load(Relaxed));
        assert!(post("op=mute&ch=0&v=1", "X-BananaSplit: 1\r\n").starts_with("HTTP/1.1 200"));
        assert!(h.shared.strips[0].mute.load(Relaxed));
        assert!(post("op=bogus", "X-BananaSplit: 1\r\n").starts_with("HTTP/1.1 400"));
        // event stream delivers published state
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write!(s, "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut got = String::new();
        let mut buf = [0u8; 1024];
        while !got.contains("data: {\"x\":1}\n\n") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0);
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(got.contains("text/event-stream"));
        h.publish("{\"x\":2}".into());
        while !got.contains("data: {\"x\":2}\n\n") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0);
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        h.close();
        h.recorder.shutdown();
    }
}
