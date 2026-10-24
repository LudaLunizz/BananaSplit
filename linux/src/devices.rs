//! Which device is which.
//!
//! PipeWire knows devices by node.name
//! (`alsa_input.usb-Blue_Microphones_Yeti-00.analog-stereo`), but its JACK
//! layer names their ports after the description
//! (`Yeti Stereo Microphone Analog Stereo:capture_FL`), adding `-NN` when
//! two devices share a description. `connect` patterns are matched against
//! the JACK names. This module bridges the two for `@default`, `@physical`,
//! `bananasplit --list-ports` and the hints shown when a pattern matches nothing.
//! Device information comes from `pactl` (C locale); without it (or under
//! plain JACK2) `@default` falls back to `@physical`.

use crate::cables::{self, CableSet};
use crate::jack::{JackClient, PORT_IS_INPUT, PORT_IS_OUTPUT, PORT_IS_PHYSICAL, regex_escape};
use crate::sys;
use std::cell::OnceCell;
use std::ffi::c_ulong;
use std::fmt::Write as _;
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use bananasplit::config::{Config, DEFAULT_DEVICE, PHYSICAL};

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The sound server's index ("Sink #52"), 0 if not given.
    pub index: u32,
    /// PipeWire node.name.
    pub name: String,
    pub description: String,
    pub nick: Option<String>,
    /// Source (input device) or sink (output device).
    pub is_source: bool,
    /// A "Monitor of <output>" source.
    pub is_monitor: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Devices {
    pub pipewire: bool,
    pub default_source: Option<String>,
    pub default_sink: Option<String>,
    pub list: Vec<Device>,
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.trim().strip_prefix(key).map(str::trim)
}

/// Parse `pactl list sinks` / `pactl list sources` (C locale).
fn parse_list(text: &str, is_source: bool) -> Vec<Device> {
    let mut out = Vec::new();
    let mut cur: Option<Device> = None;
    for line in text.lines() {
        if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
            // "Sink #52" / "Source #53" starts the next device
            out.extend(cur.take());
            let index = line.rsplit_once('#').and_then(|(_, n)| n.trim().parse().ok()).unwrap_or(0);
            cur = Some(Device { index, name: String::new(), description: String::new(), nick: None, is_source, is_monitor: false });
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        if let Some(v) = field(line, "Name:") {
            d.name = v.into();
        } else if let Some(v) = field(line, "Description:") {
            d.description = v.into();
        } else if let Some(v) = field(line, "Monitor of Sink:") {
            d.is_monitor |= v != "n/a";
        } else if let Some(v) = field(line, "node.nick = ") {
            d.nick = Some(v.trim_matches('"').to_string()).filter(|n| !n.is_empty());
        } else if let Some(v) = field(line, "device.class = ") {
            d.is_monitor |= v.trim_matches('"') == "monitor";
        }
    }
    out.extend(cur);
    out.retain(|d| !d.name.is_empty());
    for d in &mut out {
        d.is_monitor |= is_source && d.name.ends_with(".monitor");
    }
    out
}

/// Parse `pactl info`, `pactl list sinks` and `pactl list sources`.
pub fn parse(info: &str, sinks: &str, sources: &str) -> Devices {
    let mut d = Devices::default();
    let set = |v: &str| Some(v.to_string()).filter(|s| !s.is_empty() && !s.starts_with('@'));
    for line in info.lines() {
        if let Some(v) = field(line, "Server Name:") {
            d.pipewire = v.contains("PipeWire");
        } else if let Some(v) = field(line, "Default Sink:") {
            d.default_sink = set(v);
        } else if let Some(v) = field(line, "Default Source:") {
            d.default_source = set(v);
        }
    }
    d.list = parse_list(sinks, false);
    d.list.extend(parse_list(sources, true));
    d
}

impl Devices {
    pub fn query() -> Result<Devices, String> {
        let info = cables::pactl(&["info"])?;
        let sinks = cables::pactl(&["list", "sinks"])?;
        let sources = cables::pactl(&["list", "sources"])?;
        Ok(parse(&info, &sinks, &sources))
    }
}

/// An app's audio stream as the sound server sees it: a sink input (it
/// plays) or a source output (it records).
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub id: u32,
    /// `application.name`, else the program's file name, else the node name.
    pub app: String,
    /// `media.name` ("YouTube", "Playback", …), may be empty.
    pub media: String,
    /// The program's file name, may be empty.
    pub binary: String,
    /// Its PipeWire node.name, may be empty.
    pub node: String,
    /// Its node.description and node.nick (most streams have none).
    pub desc: String,
    pub nick: String,
    /// The sink or source it plays to / records from (its index).
    pub target: u32,
}

impl Stream {
    /// Is JACK client `client` this stream? PipeWire's JACK layer shows
    /// every stream as a JACK client too, named after its node.description,
    /// else its node.nick, else its node.name (the application.name unless
    /// the app set one); "application.name/<that>" when the two differ; plus
    /// "-<id>" when the name is taken.
    pub fn is_jack_client(&self, client: &str) -> bool {
        let base = client.rsplit_once('-').filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map_or(client, |(b, _)| b);
        let names = [self.desc.as_str(), self.nick.as_str(), self.node.as_str()];
        let joined = names.iter().filter(|n| !n.is_empty() && !self.app.is_empty()).map(|n| format!("{}/{n}", self.app));
        names.iter().map(|n| n.to_string()).chain([self.app.clone()]).chain(joined).filter(|n| !n.is_empty()).any(|n| n == client || n == base)
    }
}

/// Parse `pactl list sink-inputs` (`target_key` "Sink:") or `pactl list
/// source-outputs` ("Source:"), C locale. Peak meters (pavucontrol's) and
/// streams of BananaSplit itself are left out.
#[cfg(test)]
pub fn parse_streams(text: &str, target_key: &str) -> Vec<Stream> {
    parse_streams_all(text, target_key).0
}

/// The apps' streams, as [`parse_streams`], and separately the streams that
/// are no apps (peak meters, monitors, BananaSplit itself): their JACK
/// clients are not programs to offer either.
pub fn parse_streams_all(text: &str, target_key: &str) -> (Vec<Stream>, Vec<Stream>) {
    let (mut out, mut hidden) = (Vec::new(), Vec::new());
    let mut cur: Option<(Stream, bool)> = None; // stream, skip
    let finish = |c: Option<(Stream, bool)>, out: &mut Vec<Stream>, hidden: &mut Vec<Stream>| {
        if let Some((mut st, skip)) = c {
            if st.app.is_empty() {
                st.app = if !st.binary.is_empty() {
                    st.binary.clone()
                } else if !st.node.is_empty() {
                    st.node.clone()
                } else {
                    format!("Stream {}", st.id)
                };
            }
            if skip { hidden.push(st) } else { out.push(st) }
        }
    };
    for line in text.lines() {
        if !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
            finish(cur.take(), &mut out, &mut hidden);
            let id = line.rsplit_once('#').and_then(|(_, n)| n.trim().parse().ok()).unwrap_or(0);
            let st = Stream {
                id,
                app: String::new(),
                media: String::new(),
                binary: String::new(),
                node: String::new(),
                desc: String::new(),
                nick: String::new(),
                target: u32::MAX,
            };
            cur = Some((st, false));
            continue;
        }
        let Some((st, skip)) = cur.as_mut() else { continue };
        let prop = |k: &str| field(line, k).map(|v| v.trim_matches('"').to_string());
        if let Some(v) = field(line, target_key) {
            st.target = v.parse().unwrap_or(u32::MAX);
        } else if let Some(v) = prop("application.name = ") {
            st.app = v;
        } else if let Some(v) = prop("media.name = ") {
            *skip |= v == "Peak detect";
            st.media = v;
        } else if let Some(v) = prop("application.process.binary = ") {
            *skip |= v == "bananasplit";
            st.binary = v;
        } else if let Some(v) = prop("node.name = ") {
            st.node = v;
        } else if let Some(v) = prop("node.description = ") {
            st.desc = v;
        } else if let Some(v) = prop("node.nick = ") {
            st.nick = v;
        } else if let Some(v) = prop("application.id = ") {
            *skip |= v == "org.PulseAudio.pavucontrol";
        } else if let Some(v) = prop("stream.monitor = ") {
            *skip |= v == "true";
        }
    }
    finish(cur, &mut out, &mut hidden);
    (out, hidden)
}

/// What the sound server knows beyond JACK: devices, the defaults and the
/// apps' streams (for the channels' menus).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PulseInfo {
    /// None: no pactl, or the server is not PipeWire.
    pub devices: Option<Devices>,
    pub playing: Vec<Stream>,
    pub recording: Vec<Stream>,
    /// Streams that are no apps (peak meters, monitors, BananaSplit).
    pub other: Vec<Stream>,
}

impl PulseInfo {
    pub fn query() -> PulseInfo {
        let Ok(devices) = Devices::query() else { return PulseInfo::default() };
        if !devices.pipewire {
            return PulseInfo { devices: Some(devices), ..PulseInfo::default() };
        }
        let (playing, mut other) = cables::pactl(&["list", "sink-inputs"]).map(|t| parse_streams_all(&t, "Sink:")).unwrap_or_default();
        let (recording, more) = cables::pactl(&["list", "source-outputs"]).map(|t| parse_streams_all(&t, "Source:")).unwrap_or_default();
        other.extend(more);
        PulseInfo { devices: Some(devices), playing, recording, other }
    }

    /// Is JACK client `client` one of the sound server's streams (an app
    /// that plays or records, or a peak meter)? Those are listed as apps,
    /// not as JACK programs.
    pub fn is_stream(&self, client: &str) -> bool {
        self.playing.iter().chain(&self.recording).chain(&self.other).any(|s| s.is_jack_client(client))
    }

    /// node.name of the sink (`source` false) or source with this index.
    pub fn node_name(&self, index: u32, source: bool) -> Option<&str> {
        self.devices.as_ref()?.list.iter().find(|d| d.index == index && d.is_source == source).map(|d| d.name.as_str())
    }

    /// Description of the sink / source with this index.
    pub fn description(&self, index: u32, source: bool) -> Option<&str> {
        self.devices.as_ref()?.list.iter().find(|d| d.index == index && d.is_source == source).map(|d| d.description.as_str())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct WatchInner {
    info: Mutex<(u64, Arc<PulseInfo>)>,
    /// Something changed (or someone asked): refresh soon.
    dirty: Mutex<bool>,
    cond: Condvar,
    quit: AtomicBool,
    events: Mutex<Option<Child>>,
    /// `pactl subscribe` runs and reports the server's changes.
    listening: AtomicBool,
    /// The threads reading `pactl subscribe`.
    readers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// How many times the server was asked (whether anything changed or not).
    queries: AtomicU64,
    /// How many times the server went away: `pactl subscribe` lost it after
    /// it had reached it, or it answered again after it did not
    /// (pipewire-pulse restarted, probably).
    restarts: AtomicU64,
    /// It did: start it again at once (not 10 s after the last try).
    again: AtomicBool,
}

/// Keeps a [`PulseInfo`] up to date in its own thread: refreshed when
/// `pactl subscribe` reports a change (at most every 300 ms), when asked,
/// and every 30 s anyway. Without `pactl subscribe` (it could not start, or
/// it ended because pipewire-pulse restarted) it refreshes every 3 s and
/// starts `pactl subscribe` again while the server answers: at once when
/// it had been running (the server went away; see [`PulseWatch::restarts`]),
/// else every 10 s. When no server has answered for a minute (JACK2
/// without PulseAudio, say) it only looks every 10 s.
pub struct PulseWatch {
    inner: Arc<WatchInner>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// Start `pactl subscribe` and the thread that reads it.
fn subscribe(inner: &Arc<WatchInner>) {
    // The readers of earlier `pactl subscribe`s have ended: free them (their
    // stacks then serve the next reader), so restarts never pile them up.
    {
        let mut readers = lock(&inner.readers);
        let (done, running): (Vec<_>, Vec<_>) = readers.drain(..).partition(|r| r.is_finished());
        for r in done {
            let _ = r.join();
        }
        *readers = running;
    }
    // It runs as long as BananaSplit: it must not inherit libjack's
    // descriptors (not all are close-on-exec), or after a crash it keeps
    // BananaSplit's JACK client looking alive and a new start fails.
    sys::cloexec_all();
    // (its own process group: Ctrl-C in a terminal stops BananaSplit, which
    // then stops it, rather than both at once)
    let sub = Command::new("pactl")
        .arg("subscribe")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = sub else { return };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    if let Some(mut old) = lock(&inner.events).replace(child) {
        let _ = old.kill();
        let _ = old.wait();
    }
    inner.listening.store(true, SeqCst);
    let i = inner.clone();
    let reader = std::thread::Builder::new().name("bs-pulse-ev".into()).spawn(move || {
        let started = Instant::now();
        // `pactl subscribe` prints one line per change on the server
        for line in std::io::BufReader::new(out).lines() {
            let Ok(l) = line else { break };
            // our own `pactl list` calls show up as client events
            if l.contains("on client") {
                continue;
            }
            *lock(&i.dirty) = true;
            i.cond.notify_all();
        }
        // it ended (the server went away): look again, poll from now on
        // (one that ends at once never reached a server)
        if started.elapsed() >= Duration::from_millis(500) && !i.quit.load(SeqCst) {
            i.restarts.fetch_add(1, SeqCst);
            i.again.store(true, SeqCst);
        }
        i.listening.store(false, SeqCst);
        *lock(&i.dirty) = true;
        i.cond.notify_all();
    });
    match reader {
        Ok(h) => lock(&inner.readers).push(h),
        Err(_) => inner.listening.store(false, SeqCst),
    }
}

impl PulseWatch {
    pub fn start() -> PulseWatch {
        let inner = Arc::new(WatchInner {
            info: Mutex::new((0, Arc::new(PulseInfo::default()))),
            dirty: Mutex::new(true),
            cond: Condvar::new(),
            quit: AtomicBool::new(false),
            events: Mutex::new(None),
            listening: AtomicBool::new(false),
            readers: Mutex::new(Vec::new()),
            queries: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            again: AtomicBool::new(false),
        });
        subscribe(&inner);
        let mut threads = Vec::new();
        let i = inner.clone();
        if let Ok(h) = std::thread::Builder::new().name("bs-pulse".into()).spawn(move || watch_thread(&i)) {
            threads.push(h);
        }
        PulseWatch { inner, threads: Mutex::new(threads) }
    }

    /// The latest information and its revision (bumped when it changes).
    pub fn snapshot(&self) -> (u64, Arc<PulseInfo>) {
        let g = lock(&self.inner.info);
        (g.0, g.1.clone())
    }

    /// Bumped whenever the server was asked again, even if nothing changed
    /// (something can come and go between two looks).
    pub fn queries(&self) -> u64 {
        self.inner.queries.load(SeqCst)
    }

    /// Bumped whenever the server went away while BananaSplit watched it
    /// (pipewire-pulse restarted, say): `pactl subscribe` lost it, or it
    /// answers again after it did not.
    pub fn restarts(&self) -> u64 {
        self.inner.restarts.load(SeqCst)
    }

    /// Refresh as soon as possible (after a change BananaSplit made).
    pub fn kick(&self) {
        *lock(&self.inner.dirty) = true;
        self.inner.cond.notify_all();
    }

    pub fn shutdown(&self) {
        self.inner.quit.store(true, SeqCst);
        *lock(&self.inner.dirty) = true;
        self.inner.cond.notify_all();
        // the watch thread first: it starts no `pactl subscribe` after this
        for h in lock(&self.threads).drain(..) {
            let _ = h.join();
        }
        if let Some(mut c) = lock(&self.inner.events).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        for h in lock(&self.inner.readers).drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for PulseWatch {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn watch_thread(inner: &Arc<WatchInner>) {
    let mut last: Option<Instant> = None;
    let mut tried = Instant::now();
    // the server answered the last time it was asked, and when it last did
    let mut answers = true;
    let mut answered: Option<Instant> = None;
    while !inner.quit.load(SeqCst) {
        {
            let mut dirty = lock(&inner.dirty);
            loop {
                // without `pactl subscribe`: every 3 s, or every 10 s once no
                // server has answered for a minute (JACK2 without PulseAudio)
                let lately = answered.is_some_and(|t| t.elapsed() < Duration::from_secs(60));
                let every = Duration::from_secs(match (inner.listening.load(SeqCst), answers || lately) {
                    (true, _) => 30,
                    (false, true) => 3,
                    (false, false) => 10,
                });
                let since = last.map_or(every, |t| t.elapsed());
                if *dirty || since >= every || inner.quit.load(SeqCst) {
                    break;
                }
                let wait = (every - since).max(Duration::from_millis(10));
                dirty = inner.cond.wait_timeout(dirty, wait).unwrap_or_else(|e| e.into_inner()).0;
            }
            *dirty = false;
        }
        if inner.quit.load(SeqCst) {
            break;
        }
        if !inner.listening.load(SeqCst) && answers && (inner.again.swap(false, SeqCst) || tried.elapsed() >= Duration::from_secs(10)) {
            tried = Instant::now();
            subscribe(inner);
        }
        let info = PulseInfo::query();
        if info.devices.is_some() {
            // back after it did not answer: it restarted (or came up late)
            if !answers {
                inner.restarts.fetch_add(1, SeqCst);
                inner.again.store(true, SeqCst);
            }
            answered = Some(Instant::now());
        }
        answers = info.devices.is_some();
        last = Some(Instant::now());
        {
            let mut g = lock(&inner.info);
            if *g.1 != info {
                g.0 += 1;
                g.1 = Arc::new(info);
            }
        }
        inner.queries.fetch_add(1, SeqCst);
        // at most every 300 ms, however busy the server is
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// How well JACK client name `client` matches device label `label`:
/// 3 exact, 2 with PipeWire's "-NN" de-duplication suffix, 1 cut off at
/// JACK's client-name limit, 0 no match.
fn label_score(label: &str, client: &str) -> u8 {
    if label.is_empty() {
        return 0;
    }
    if client == label {
        return 3;
    }
    if let Some(rest) = client.strip_prefix(label).and_then(|r| r.strip_prefix('-')) {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return 2;
        }
    }
    if client.chars().count() >= 60 && label.starts_with(client) {
        return 1;
    }
    0
}

/// Does JACK client `client` belong to device `d`? (Description is the
/// default; nick with `jack.short-name`; node.name as a last resort.)
pub fn client_score(d: &Device, client: &str) -> u8 {
    [Some(d.description.as_str()), d.nick.as_deref(), Some(d.name.as_str())]
        .into_iter()
        .flatten()
        .map(|l| label_score(l, client))
        .max()
        .unwrap_or(0)
}

pub fn client_of(port: &str) -> &str {
    port.split_once(':').map_or(port, |(c, _)| c)
}

fn short_of(port: &str) -> &str {
    port.split_once(':').map_or(port, |(_, s)| s)
}

/// PipeWire flags an output device's monitor ports as outputs (and
/// sometimes as physical); feeding them into a strip records the speakers.
pub fn is_monitor_port(port: &str) -> bool {
    short_of(port).starts_with("monitor")
}

/// The ports to take from a set of matches: without the monitor ports when
/// there are others. PipeWire lists a device's input and output as one
/// JACK client when they have the same name (`capture_MONO`, `monitor_FL`,
/// `monitor_FR`); taking "the first two" there would put the speakers'
/// monitor on a strip's right side. Only monitors (`^Speakers:monitor_`):
/// those are what was asked for.
pub fn prefer_non_monitor(mut ports: Vec<String>) -> Vec<String> {
    if ports.iter().any(|p| !is_monitor_port(p)) {
        ports.retain(|p| !is_monitor_port(p));
    }
    ports
}

type Group = (String, Vec<String>);

/// The JACK client a `connect` value names exactly, if it is a `^Name:`
/// line as the channel menus write it (regex characters escaped).
pub fn literal_client(pattern: &str) -> Option<String> {
    let body = pattern.strip_prefix('^')?.strip_suffix(':')?;
    let mut name = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => name.push(chars.next()?),
            ':' => return None,
            c if "\\.^$|?*+()[]{}".contains(c) => return None,
            c => name.push(c),
        }
    }
    (!name.is_empty() && regex_escape(&name) == body).then_some(name)
}

/// PipeWire's JACK layer tells apart nodes of different programs that
/// share a name by adding "-<node id>" to the one that came later
/// ("Yeti Stereo Microphone Analog Stereo-63"); node ids change when the
/// device comes back or the computer restarts. `Name-63` gives `Name`.
/// Node ids have two digits or more (the first ids are PipeWire's own
/// objects), so "HDMI-2" is a name of its own.
pub fn numbered(client: &str) -> Option<&str> {
    let (base, n) = client.rsplit_once('-')?;
    (!base.is_empty() && n.len() >= 2 && n.bytes().all(|b| b.is_ascii_digit())).then_some(base)
}

/// Ports grouped by JACK client, in the order JACK lists them.
fn groups(ports: Vec<String>) -> Vec<Group> {
    let mut g: Vec<Group> = Vec::new();
    for p in ports {
        let c = client_of(&p).to_string();
        match g.iter_mut().find(|(k, _)| *k == c) {
            Some((_, v)) => v.push(p),
            None => g.push((c, vec![p])),
        }
    }
    g
}

/// Index of the group that belongs to `d` (best match, first on ties).
fn best_index(gs: &[Group], d: &Device) -> Option<usize> {
    let mut best: Option<(u8, usize)> = None;
    for (i, (client, _)) in gs.iter().enumerate() {
        let s = client_score(d, client);
        if s > 0 && best.is_none_or(|(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    best.map(|(_, i)| i)
}

/// Like [`best_index`] for a device's own ports: a group with nothing but
/// monitor ports (what an output plays) is never an input device, even when
/// PipeWire gave the output the device's name and the input the "-NN" one.
fn device_index(gs: &[Group], d: &Device) -> Option<usize> {
    let mut best: Option<(u8, usize)> = None;
    for (i, (client, ports)) in gs.iter().enumerate() {
        if ports.iter().all(|p| is_monitor_port(p)) {
            continue;
        }
        let s = client_score(d, client);
        if s > 0 && best.is_none_or(|(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    best.map(|(_, i)| i)
}

/// Which side of the mixer a pattern is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// A strip takes audio from JACK *output* ports (capture, monitor, apps).
    Strip,
    /// A bus sends audio to JACK *input* ports (playback, apps).
    Bus,
}

impl Side {
    pub fn flag(self) -> c_ulong {
        match self {
            Side::Strip => PORT_IS_OUTPUT,
            Side::Bus => PORT_IS_INPUT,
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Strip => Side::Bus,
            Side::Bus => Side::Strip,
        }
    }
    /// "default input" (microphone) / "default output" (speakers).
    fn device(self) -> &'static str {
        match self {
            Side::Strip => "input",
            Side::Bus => "output",
        }
    }
}

/// What a `connect` value resolved to.
#[derive(Debug, Default)]
pub struct Resolved {
    /// Full JACK port names, best first (BananaSplit uses the first two).
    pub ports: Vec<String>,
    /// Why nothing matched, or what fallback was used.
    pub note: Option<String>,
    /// A regex that matched nothing (worth listing the real port names).
    pub missed: bool,
    /// `@default` or `@physical` could not use the desktop's default device
    /// (why): `ports` are another one's (`@physical`'s choice).
    pub fallback: Option<String>,
}

/// What a JACK client a channel can be connected to is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A sound card, microphone, headset… (physical ports).
    Device,
    /// A program with JACK ports (an app, or another mixer).
    App,
    /// What an output device plays ("monitor" ports): feeding it into a
    /// strip records the speakers (and can loop).
    Monitor,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Device => "device",
            Kind::App => "app",
            Kind::Monitor => "monitor",
        }
    }
}

/// One JACK client a channel can be connected to.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The JACK client name (`^client:` connects it).
    pub client: String,
    /// What to call it: the device's short name if PipeWire knows one.
    pub label: String,
    pub kind: Kind,
    /// The desktop's default input (strips) or output (buses).
    pub default: bool,
}

/// Resolves `connect` values against the live JACK graph. Asks pactl at
/// most once (lazily), so make one per auto-connect pass.
pub struct Resolver<'a> {
    client: &'a JackClient,
    /// "BananaSplit:" — never connect the mixer to itself.
    own: String,
    cables: CableSet,
    devs: OnceCell<Result<Devices, String>>,
    /// Per side: the JACK clients that `^Name:` lines name exactly.
    claimed: [Vec<String>; 2],
}

impl<'a> Resolver<'a> {
    pub fn new(client: &'a JackClient, own_client: &str, cables: CableSet) -> Self {
        Resolver { client, own: format!("{own_client}:"), cables, devs: OnceCell::new(), claimed: [Vec::new(), Vec::new()] }
    }

    /// Know which clients the config's `^Name:` lines name exactly: a line
    /// for a client PipeWire numbered never takes one of those instead
    /// (see [`Resolver::renumbered`]).
    pub fn with_lines(mut self, cfg: &Config) -> Self {
        let named = |lines: &mut dyn Iterator<Item = &String>| lines.filter_map(|p| literal_client(p)).collect::<Vec<String>>();
        self.claimed = [
            named(&mut cfg.strips.iter().flat_map(|s| &s.connect)),
            named(&mut cfg.buses.iter().flat_map(|b| &b.connect)),
        ];
        self
    }

    /// Use what is known about the devices already (no pactl call); None:
    /// nothing is known (no pactl).
    pub fn with_devices(self, devs: Option<Devices>) -> Self {
        let _ = self.devs.set(devs.ok_or_else(|| "pactl is not available".to_string()));
        self
    }

    /// Everything one side can be connected to, one entry per JACK client,
    /// without BananaSplit itself and its cables: devices first, then
    /// programs, then (strips) what output devices play.
    pub fn candidates(&self, side: Side) -> Vec<Candidate> {
        let physical = self.ports(None, side.flag() | PORT_IS_PHYSICAL);
        let default = self.default_device(side).ok();
        let devs = self.devices().ok();
        let gs: Vec<Group> = groups(self.ports(None, side.flag())).into_iter().filter(|(c, _)| !self.cables.owns(c, c)).collect();
        let default_i = default.and_then(|d| device_index(&gs, d));
        let mut out: Vec<Candidate> = gs
            .iter()
            .enumerate()
            .map(|(i, (client, ports))| {
                let kind = if ports.iter().all(|p| is_monitor_port(p)) {
                    Kind::Monitor
                } else if ports.iter().any(|p| physical.contains(p)) {
                    Kind::Device
                } else {
                    Kind::App
                };
                let dev = devs.and_then(|d| {
                    d.list.iter().filter(|x| !x.is_monitor && client_score(x, client) >= 2).max_by_key(|x| client_score(x, client))
                });
                let label = dev
                    .and_then(|d| d.nick.clone().filter(|n| n.len() < client.len()))
                    .unwrap_or_else(|| client.clone());
                Candidate { client: client.clone(), label, kind, default: default_i == Some(i) }
            })
            .collect();
        let rank = |k: Kind| match k {
            Kind::Device => 0,
            Kind::App => 1,
            Kind::Monitor => 2,
        };
        out.sort_by_key(|c| (rank(c.kind), !c.default));
        out
    }

    fn devices(&self) -> Result<&Devices, String> {
        self.devs.get_or_init(Devices::query).as_ref().map_err(Clone::clone)
    }

    /// Ports matching a regex (or all with `None`), without our own.
    pub fn ports(&self, pattern: Option<&str>, flags: c_ulong) -> Vec<String> {
        self.client.ports(pattern, flags).into_iter().filter(|p| !p.starts_with(&self.own)).collect()
    }

    /// The desktop's default device for this side, or why it can't be used.
    fn default_device(&self, side: Side) -> Result<&Device, String> {
        let d = self.devices()?;
        if !d.pipewire {
            return Err("the sound server is not PipeWire".into());
        }
        let name = match side {
            Side::Strip => &d.default_source,
            Side::Bus => &d.default_sink,
        };
        let name = name.as_deref().ok_or_else(|| format!("no default {} is set", side.device()))?;
        let dev = d.list.iter().find(|x| x.name == name && x.is_source == (side == Side::Strip));
        let desc = dev.map_or(name, |x| x.description.as_str());
        if self.cables.owns(name, desc) {
            return Err(format!("the default {} is BananaSplit's own \"{desc}\"", side.device()));
        }
        let dev = dev.ok_or_else(|| format!("pactl does not list the default {} \"{name}\"", side.device()))?;
        if dev.is_monitor {
            return Err(format!("the default input \"{desc}\" records an output (that would loop)"));
        }
        Ok(dev)
    }

    pub fn resolve(&self, pattern: &str, side: Side) -> Resolved {
        match pattern {
            PHYSICAL => self.physical(side, None),
            DEFAULT_DEVICE => match self.default_device(side) {
                Ok(dev) => {
                    let gs = groups(self.ports(None, side.flag()).into_iter().filter(|p| !is_monitor_port(p)).collect());
                    match best_index(&gs, dev) {
                        Some(i) => Resolved { ports: gs.into_iter().nth(i).map(|g| g.1).unwrap_or_default(), ..Default::default() },
                        None => Resolved {
                            note: Some(format!("the default {} \"{}\" has no JACK ports yet", side.device(), dev.description)),
                            ..Default::default()
                        },
                    }
                }
                Err(why) => {
                    let r = self.physical(side, Some(format!("{DEFAULT_DEVICE}: {why}; using {PHYSICAL}")));
                    Resolved { fallback: Some(why), ..r }
                }
            },
            pat => {
                let ports = self.ports(Some(pat), side.flag());
                if !ports.is_empty() {
                    return Resolved { ports: prefer_non_monitor(ports), ..Default::default() };
                }
                self.renumbered(pat, side).unwrap_or_else(|| Resolved {
                    note: Some(format!("nothing matches \"{pat}\"{}", self.miss_hint(pat, side))),
                    missed: true,
                    ..Default::default()
                })
            }
        }
    }

    /// A channel menu's line for a JACK client that PipeWire had numbered
    /// (`^Name-63:`, see [`numbered`]) after the number changed: the one
    /// client on this side now called "Name" or "Name-NN" that no `^Name:`
    /// line names exactly. None: there is none (or the line is no such
    /// line); a note when several fit (a microphone and what an output
    /// plays, say), since BananaSplit can't tell which one it was.
    fn renumbered(&self, pat: &str, side: Side) -> Option<Resolved> {
        let client = literal_client(pat)?;
        let base = numbered(&client)?;
        let claimed = &self.claimed[side as usize];
        let gs: Vec<Group> = groups(self.ports(None, side.flag()))
            .into_iter()
            .filter(|(c, _)| (c == base || numbered(c) == Some(base)) && !self.cables.owns(c, c) && !claimed.contains(c))
            .collect();
        match gs.as_slice() {
            [] => None,
            [(_, ports)] => Some(Resolved { ports: prefer_non_monitor(ports.clone()), ..Default::default() }),
            many => {
                let names: Vec<String> = many.iter().map(|(c, _)| format!("\"{c}\"")).collect();
                Some(Resolved {
                    note: Some(format!(
                        "nothing matches \"{pat}\": PipeWire numbers JACK names that are taken twice, and now {} are there; \
                         tick the right one in the channel's menu",
                        names.join(" and ")
                    )),
                    ..Default::default()
                })
            }
        }
    }

    /// Physical ports of one device: the default device if it is physical,
    /// else the first one JACK lists (and `fallback` says why).
    fn physical(&self, side: Side, note: Option<String>) -> Resolved {
        let gs = groups(self.ports(None, side.flag() | PORT_IS_PHYSICAL).into_iter().filter(|p| !is_monitor_port(p)).collect());
        if gs.is_empty() {
            let what = if side == Side::Strip { "capture" } else { "playback" };
            let prefix = note.map_or(String::new(), |n| format!("{n}, but "));
            return Resolved { note: Some(format!("{prefix}there is no physical {what} device")), ..Default::default() };
        }
        let (i, fallback) = match self.default_device(side) {
            Ok(d) => match best_index(&gs, d) {
                Some(i) => (i, None),
                None => (0, Some(format!("the default {} \"{}\" is not a physical device", side.device(), d.description))),
            },
            Err(why) => (0, Some(why)),
        };
        Resolved { ports: gs.into_iter().nth(i).map(|g| g.1).unwrap_or_default(), note, fallback, ..Default::default() }
    }

    /// Why a regex probably matched nothing, as a short suffix.
    fn miss_hint(&self, pat: &str, side: Side) -> String {
        if sys::regex_match(pat, "").is_none() {
            return " (not a valid regular expression)".into();
        }
        let wrong = self.ports(Some(pat), side.other().flag());
        if let Some(p) = wrong.first() {
            return match side {
                Side::Strip => format!(" that sends audio; \"{p}\" is an input (a strip needs capture, monitor or app output ports)"),
                Side::Bus => format!(" that takes audio; \"{p}\" is an output (a bus needs playback or app input ports)"),
            };
        }
        let all = groups(self.ports(None, side.flag()));
        if let Ok(devs) = self.devices() {
            for dev in &devs.list {
                let group = best_index(&all, dev).map(|i| &all[i]);
                let hit = sys::regex_match(pat, &dev.name) == Some(true)
                    || group.is_some_and(|(_, ports)| {
                        ports.iter().any(|p| sys::regex_match(pat, &format!("{}:{}", dev.name, short_of(p))) == Some(true))
                    });
                if hit {
                    return match group {
                        Some((client, _)) => format!(
                            " — that is the PipeWire node name; JACK calls this device \"{client}\": use connect = ^{}:",
                            regex_escape(client)
                        ),
                        None => format!(" — that is the PipeWire node name of \"{}\", which has no such JACK ports", dev.description),
                    };
                }
            }
        }
        for (client, ports) in &all {
            if sys::regex_match(pat, client) == Some(true) {
                return format!(" — JACK port names are client:port, e.g. \"{}\"", ports[0]);
            }
        }
        let low = pat.to_lowercase();
        if let Some(p) = all.iter().flat_map(|g| &g.1).find(|p| sys::regex_match(&low, &p.to_lowercase()) == Some(true)) {
            return format!(" — names are case-sensitive: did you mean \"{p}\"?");
        }
        " (bananasplit --list-ports shows the names)".into()
    }

    /// One line per JACK client on this side: `Name  (port port …)`.
    pub fn listing(&self, side: Side) -> Vec<String> {
        groups(self.ports(None, side.flag()))
            .into_iter()
            .map(|(client, ports)| format!("{client}  ({})", shorts(&ports)))
            .collect()
    }

    /// The report printed by `bananasplit --list-ports`.
    pub fn report(&self) -> String {
        let mut o = String::new();
        let devs = self.devices().ok();
        let _ = writeln!(o, "connect = patterns match these JACK names (client:port), not PipeWire node names.");
        let _ = writeln!(o, "BananaSplit connects the first two matching ports to L and R (one port: to both).");
        for side in [Side::Strip, Side::Bus] {
            let head = match side {
                Side::Strip => "\nFor a strip, [strip1] … [strip6] (ports that send audio):",
                Side::Bus => "\nFor a bus, [bus.A1] … [bus.B2] (ports that take audio):",
            };
            let _ = writeln!(o, "{head}");
            let physical = self.ports(None, side.flag() | PORT_IS_PHYSICAL);
            let default = self.default_device(side).ok();
            let gs = groups(self.ports(None, side.flag()));
            if gs.is_empty() {
                let _ = writeln!(o, "  (none)");
            }
            let default_i = default.and_then(|d| device_index(&gs, d));
            for (i, (client, ports)) in gs.iter().enumerate() {
                let mut tags: Vec<String> = Vec::new();
                if ports.iter().any(|p| physical.contains(p)) {
                    tags.push("physical".into());
                }
                if default_i == Some(i) {
                    tags.push(format!("default {}", side.device()));
                }
                if ports.iter().all(|p| is_monitor_port(p)) {
                    tags.push("records an output (can loop)".into());
                }
                if self.cables.owns(client, client) {
                    tags.push("BananaSplit cable".into());
                }
                let node = devs.and_then(|d| {
                    d.list
                        .iter()
                        .filter(|x| x.is_source == (side == Side::Strip) || ports.iter().all(|p| is_monitor_port(p)))
                        .filter(|x| client_score(x, client) >= 2)
                        .map(|x| x.name.as_str())
                        .find(|n| *n != client)
                });
                if let Some(n) = node {
                    tags.push(format!("PipeWire node {n}"));
                }
                let _ = writeln!(o, "  {client:<38}  {}", shorts(ports));
                if !tags.is_empty() {
                    let _ = writeln!(o, "      {}", tags.join(" · "));
                }
                let _ = writeln!(o, "      connect = ^{}:", regex_escape(client));
            }
        }
        let _ = writeln!(o);
        for kw in [DEFAULT_DEVICE, PHYSICAL] {
            let show = |side: Side| {
                let r = self.resolve(kw, side);
                match (r.ports.first(), r.note) {
                    (Some(p), None) => format!("\"{}\"", client_of(p)),
                    (Some(p), Some(n)) => format!("\"{}\" ({n})", client_of(p)),
                    (None, n) => format!("nothing ({})", n.unwrap_or_default()),
                }
            };
            let _ = writeln!(o, "{kw:<9} → strips: {}", show(Side::Strip));
            let _ = writeln!(o, "{:<9} → buses:  {}", "", show(Side::Bus));
        }
        if devs.is_none() {
            let _ = writeln!(o, "(pactl is not available, so default devices and PipeWire node names are not shown)");
        }
        o
    }
}

/// Short port names of one client, at most 6 (`a b c … +4`).
fn shorts(ports: &[String]) -> String {
    let mut v: Vec<&str> = ports.iter().take(6).map(|p| short_of(p)).collect();
    let more = format!("+{}", ports.len().saturating_sub(6));
    if ports.len() > 6 {
        v.push(&more);
    }
    v.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "Server String: /run/user/1000/pulse/native\nServer Name: PulseAudio (on PipeWire 1.2.7)\n\
        Default Sink: alsa_output.usb-Blue_Yeti-00.analog-stereo\nDefault Source: alsa_input.usb-Blue_Yeti-00.analog-stereo\n";
    const SOURCES: &str = "Source #48\n\tState: SUSPENDED\n\tName: alsa_input.usb-Razer_Kiyo-02.analog-stereo\n\
        \tDescription: Razer Kiyo Analog Stereo\n\tDriver: PipeWire\n\tMonitor of Sink: n/a\n\tProperties:\n\
        \t\tnode.name = \"alsa_input.usb-Razer_Kiyo-02.analog-stereo\"\n\t\tnode.nick = \"Razer Kiyo\"\n\
        \t\tdevice.description = \"Razer Kiyo\"\n\n\
        Source #52\n\tName: alsa_input.usb-Blue_Yeti-00.analog-stereo\n\tDescription: Yeti Stereo Microphone Analog Stereo\n\
        \tMonitor of Sink: n/a\n\tProperties:\n\t\tnode.nick = \"Yeti Stereo Microphone\"\n\n\
        Source #53\n\tName: alsa_output.usb-Blue_Yeti-00.analog-stereo.monitor\n\
        \tDescription: Monitor of Yeti Stereo Microphone Analog Stereo\n\tMonitor of Sink: alsa_output.usb-Blue_Yeti-00.analog-stereo\n";
    const SINKS: &str = "Sink #51\n\tName: alsa_output.usb-Blue_Yeti-00.analog-stereo\n\
        \tDescription: Yeti Stereo Microphone Analog Stereo\n\tMonitor Source: alsa_output.usb-Blue_Yeti-00.analog-stereo.monitor\n";

    #[test]
    fn parses_pactl() {
        let d = parse(INFO, SINKS, SOURCES);
        assert!(d.pipewire);
        assert_eq!(d.default_source.as_deref(), Some("alsa_input.usb-Blue_Yeti-00.analog-stereo"));
        assert_eq!(d.default_sink.as_deref(), Some("alsa_output.usb-Blue_Yeti-00.analog-stereo"));
        assert_eq!(d.list.len(), 4);
        let sink = &d.list[0];
        assert!(!sink.is_source && !sink.is_monitor && sink.description == "Yeti Stereo Microphone Analog Stereo");
        let kiyo = &d.list[1];
        assert!(kiyo.is_source && !kiyo.is_monitor);
        assert_eq!(kiyo.description, "Razer Kiyo Analog Stereo");
        assert_eq!(kiyo.nick.as_deref(), Some("Razer Kiyo"));
        assert!(d.list[3].is_monitor && d.list[3].is_source);
        // JACK2 + PulseAudio, or no defaults
        let p = parse("Server Name: pulseaudio\nDefault Sink: @DEFAULT_SINK@\n", "", "");
        assert!(!p.pipewire && p.default_sink.is_none() && p.list.is_empty());
    }

    #[test]
    fn device_indices_and_app_streams() {
        let d = parse(INFO, SINKS, SOURCES);
        assert_eq!(d.list.iter().map(|x| x.index).collect::<Vec<_>>(), [51, 48, 52, 53]);
        let play = "Sink Input #201\n\tDriver: PipeWire\n\tSink: 51\n\tProperties:\n\t\tapplication.name = \"Discord\"\n\t\tmedia.name = \"Playback\"\n\n\
                    Sink Input #202\n\tSink: 51\n\tProperties:\n\t\tapplication.process.binary = \"firefox\"\n\t\tnode.name = \"Firefox\"\n\n\
                    Sink Input #203\n\tSink: 52\n\tProperties:\n\t\tnode.name = \"speech-dispatcher\"\n";
        let st = parse_streams(play, "Sink:");
        assert_eq!(st.len(), 3);
        assert_eq!((st[0].id, st[0].app.as_str(), st[0].media.as_str(), st[0].target), (201, "Discord", "Playback", 51));
        assert_eq!((st[1].app.as_str(), st[1].binary.as_str()), ("firefox", "firefox"), "no application.name: the program");
        assert_eq!(st[2].app, "speech-dispatcher", "else the node name");
        // pavucontrol's peak meters, monitor streams and BananaSplit itself are no apps
        let rec = "Source Output #301\n\tSource: 52\n\tProperties:\n\t\tapplication.name = \"OBS\"\n\n\
                   Source Output #302\n\tSource: 53\n\tProperties:\n\t\tmedia.name = \"Peak detect\"\n\n\
                   Source Output #303\n\tSource: 53\n\tProperties:\n\t\tapplication.id = \"org.PulseAudio.pavucontrol\"\n\n\
                   Source Output #304\n\tSource: 53\n\tProperties:\n\t\tstream.monitor = \"true\"\n\n\
                   Source Output #305\n\tSource: 52\n\tProperties:\n\t\tapplication.process.binary = \"bananasplit\"\n\n\
                   Source Output #306\n\tSource: garbage\n";
        let st = parse_streams(rec, "Source:");
        assert_eq!(st.iter().map(|x| x.id).collect::<Vec<_>>(), [301, 306]);
        assert_eq!((st[1].target, st[1].app.as_str()), (u32::MAX, "Stream 306"));
        assert!(parse_streams("", "Sink:").is_empty());
        // the streams that are no apps come back separately
        let (apps, other) = parse_streams_all(rec, "Source:");
        assert_eq!(apps.len(), 2);
        assert_eq!(other.iter().map(|x| x.id).collect::<Vec<_>>(), [302, 303, 304, 305]);
        let info = PulseInfo { devices: Some(d), playing: Vec::new(), recording: apps, other };
        assert_eq!(info.node_name(52, true), Some("alsa_input.usb-Blue_Yeti-00.analog-stereo"));
        assert_eq!(info.node_name(52, false), None, "52 is a source, not a sink");
        assert_eq!(info.description(51, false), Some("Yeti Stereo Microphone Analog Stereo"));
        assert!(info.is_stream("OBS") && info.is_stream("OBS-77") && !info.is_stream("OBS Studio"));
    }

    /// PipeWire's JACK layer shows every app stream as a JACK client too:
    /// named after its node.name (the application.name unless the app set
    /// one), "app/node" when they differ, "-<id>" added when taken.
    #[test]
    fn app_streams_are_known_by_their_jack_names() {
        let s = |app: &str, node: &str| Stream {
            id: 1,
            app: app.into(),
            media: String::new(),
            binary: String::new(),
            node: node.into(),
            desc: String::new(),
            nick: String::new(),
            target: 0,
        };
        assert!(s("Firefox", "Firefox").is_jack_client("Firefox"));
        assert!(s("Firefox", "Firefox").is_jack_client("Firefox-123"));
        assert!(s("Firefox", "").is_jack_client("Firefox"));
        assert!(s("Chromium", "WEBRTC VoiceEngine").is_jack_client("Chromium/WEBRTC VoiceEngine"));
        assert!(s("Chromium", "WEBRTC VoiceEngine").is_jack_client("WEBRTC VoiceEngine-12"));
        assert!(!s("Firefox", "Firefox").is_jack_client("Firefox Nightly"));
        assert!(!s("Firefox", "Firefox").is_jack_client("Firefox-x"));
        assert!(!s("", "").is_jack_client(""));
        assert!(!s("Ardour", "").is_jack_client("ardour"));
        // named after its description (module-echo-cancel, loopbacks), with
        // or without the application in front
        let echo = Stream { desc: "Echo-Cancel Capture".into(), nick: "echo-cancel".into(), ..s("PipeWire", "echo-cancel-capture") };
        assert!(echo.is_jack_client("Echo-Cancel Capture"));
        assert!(echo.is_jack_client("PipeWire/Echo-Cancel Capture"));
        assert!(echo.is_jack_client("echo-cancel"));
        assert!(echo.is_jack_client("Echo-Cancel Capture-88"));
        assert!(!echo.is_jack_client("Echo-Cancel Source"));
    }

    /// A device whose mono mic and stereo output share a name is one JACK
    /// client: its mic, not the output's monitor, goes to both sides.
    #[test]
    fn monitor_ports_only_when_nothing_else_matches() {
        let v = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(prefer_non_monitor(v(&["H:capture_MONO", "H:monitor_FL", "H:monitor_FR"])), ["H:capture_MONO"]);
        assert_eq!(prefer_non_monitor(v(&["H:monitor_FL", "H:capture_FL", "H:capture_FR"])), ["H:capture_FL", "H:capture_FR"]);
        assert_eq!(prefer_non_monitor(v(&["Speakers:monitor_FL", "Speakers:monitor_FR"])), ["Speakers:monitor_FL", "Speakers:monitor_FR"]);
        assert!(prefer_non_monitor(Vec::new()).is_empty());
    }

    #[test]
    fn jack_names_map_to_devices() {
        let d = parse(INFO, SINKS, SOURCES);
        let yeti_in = &d.list[2];
        assert_eq!(client_score(yeti_in, "Yeti Stereo Microphone Analog Stereo"), 3);
        assert_eq!(client_score(yeti_in, "Yeti Stereo Microphone Analog Stereo-63"), 2);
        assert_eq!(client_score(yeti_in, "Yeti Stereo Microphone"), 3); // nick (jack.short-name)
        assert_eq!(client_score(yeti_in, "Yeti Stereo Microphone Analog Stereo-6x"), 0);
        assert_eq!(client_score(yeti_in, "Yeti"), 0);
        assert_eq!(client_score(&d.list[1], "Yeti Stereo Microphone Analog Stereo"), 0);
        let long = Device { index: 0, name: "n".into(), description: "x".repeat(80), nick: None, is_source: true, is_monitor: false };
        assert_eq!(client_score(&long, &"x".repeat(63)), 1);
        assert_eq!(client_score(&long, &"x".repeat(20)), 0);
    }

    #[test]
    fn menu_lines_and_numbered_names() {
        assert_eq!(literal_client("^Yeti Stereo Microphone Analog Stereo-63:").as_deref(), Some("Yeti Stereo Microphone Analog Stereo-63"));
        assert_eq!(literal_client(r"^Built-in Audio \(HDMI\) \[2\]:").as_deref(), Some("Built-in Audio (HDMI) [2]"));
        assert_eq!(literal_client("^system:capture_1"), None, "a port, not a client");
        assert_eq!(literal_client("^Yeti.*:"), None, "a regex");
        assert_eq!(literal_client("Yeti:"), None);
        assert_eq!(literal_client(r"^Ye\ti:"), None, "not escaped the way the menus escape");
        assert_eq!(literal_client("^:"), None);
        assert_eq!(numbered("Yeti Stereo Microphone Analog Stereo-63"), Some("Yeti Stereo Microphone Analog Stereo"));
        assert_eq!(numbered("Firefox-1074"), Some("Firefox"));
        assert_eq!(numbered("HDMI-2"), None, "one digit: part of the name");
        assert_eq!(numbered("WH-1000XM4"), None);
        assert_eq!(numbered("-63"), None);
        assert_eq!(numbered("Yeti"), None);
    }

    #[test]
    fn default_input_is_never_a_monitor_group() {
        // PipeWire gave the output (playback + monitor) the device's name
        // and the input the -NN one
        let ports = ["Yeti Stereo Microphone Analog Stereo:monitor_FL", "Yeti Stereo Microphone Analog Stereo:monitor_FR", "Yeti Stereo Microphone Analog Stereo-63:capture_FL", "Yeti Stereo Microphone Analog Stereo-63:capture_FR"];
        let gs = groups(ports.iter().map(|s| s.to_string()).collect());
        let d = parse(INFO, SINKS, SOURCES);
        assert_eq!(best_index(&gs, &d.list[2]), Some(0));
        assert_eq!(device_index(&gs, &d.list[2]), Some(1));
        assert_eq!(device_index(&gs[..1], &d.list[2]), None);
    }

    #[test]
    fn grouping_and_best_group() {
        let ports = ["Razer Kiyo Analog Stereo:capture_FL", "Razer Kiyo Analog Stereo:capture_FR", "Yeti Stereo Microphone Analog Stereo-63:capture_FL", "Yeti Stereo Microphone Analog Stereo-63:capture_FR", "Yeti Stereo Microphone Analog Stereo:capture_FL"];
        let gs = groups(ports.iter().map(|s| s.to_string()).collect());
        assert_eq!(gs.len(), 3);
        assert_eq!(gs[0].1.len(), 2);
        let d = parse(INFO, SINKS, SOURCES);
        assert_eq!(best_index(&gs, &d.list[2]), Some(2), "exact name beats the -NN copy");
        assert_eq!(best_index(&gs[..2], &d.list[2]), Some(1));
        assert_eq!(best_index(&gs, &d.list[1]), Some(0));
        assert!(is_monitor_port("Speakers:monitor_FL") && !is_monitor_port("Yeti:capture_FL"));
        assert_eq!(shorts(&gs[0].1), "capture_FL capture_FR");
        let many: Vec<String> = (1..=9).map(|i| format!("system:capture_{i}")).collect();
        assert_eq!(shorts(&many), "capture_1 capture_2 capture_3 capture_4 capture_5 capture_6 +3");
    }
}
