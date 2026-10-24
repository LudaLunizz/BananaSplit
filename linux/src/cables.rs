//! Virtual cables (PipeWire).
//!
//! * A **virtual output device per strip** (`Audio/Sink`), e.g.
//!   "BananaSplit-In3-Desktop". Point any application (or the system default output)
//!   at it and its audio lands on that strip. This is the equivalent of
//!   "VB-Audio CABLE Input" / "Voicemeeter Input".
//! * A **virtual microphone per bus** (`Audio/Source/Virtual`), e.g.
//!   "BananaSplit-Out-B1". Pick it as the microphone in Discord/OBS/etc. This is the
//!   equivalent of "Voicemeeter Output" / "CABLE Output".
//!
//! Every channel's cable can be switched on or off (`cable =` in the config,
//! or the channel's menu in the app, while BananaSplit runs), so any strip
//! can be a hardware input, a virtual one, or both, and any bus a hardware
//! output, a virtual microphone, or both. By default every strip and B1, B2
//! have theirs.
//!
//! Cables are created with `pactl load-module module-null-sink`, which
//! pipewire-pulse implements natively. They live in the same PipeWire graph
//! and are driven by the same clock as the mixer, so they add no buffering
//! and stay sample-aligned.
//!
//! A cable that disappears while BananaSplit runs (pipewire-pulse
//! restarted, or someone unloaded its module) is made again at once; one
//! that someone removes again within a minute stays off (switch it on in the
//! channel's menu), so BananaSplit never fights whoever removes it. A
//! restart of the sound server that BananaSplit noticed does not count as
//! removing them.
//!
//! Removal, from most to least common:
//! * normal exit (q, the GUI's power button, Ctrl-C, SIGTERM): `Drop`;
//! * BananaSplit killed with SIGKILL, or crashed: a small watchdog process
//!   (`bananasplit --reap-cables NAME`, started next to the cables) notices that
//!   its pipe from BananaSplit closed without the "done" line and removes them;
//! * watchdog killed too (e.g. `systemctl kill -s KILL`): the next start,
//!   `bananasplit --cleanup`, or the systemd unit's `ExecStopPost`.
//!
//! Every path matches modules by exact `sink_name`, never by a module id
//! remembered from earlier (once a cable's module is gone, pipewire-pulse
//! gives its id to the next module anyone loads), so nothing that is not a
//! cable of this BananaSplit (same `client_name`) is ever touched.

use crate::devices::Devices;
use crate::jack::regex_escape;
use crate::sys;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use bananasplit::config::Config;
use bananasplit::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS};

/// Hidden command line flag of the watchdog process.
pub const REAPER_FLAG: &str = "--reap-cables";
/// Last line BananaSplit sends the watchdog on a normal exit.
const DISARM: &str = "done";

/// Strips 1-6, then buses A1..B2.
pub const NUM_CHANNELS: usize = NUM_STRIPS + NUM_BUSES;

#[derive(Debug, Clone, PartialEq)]
pub struct CableSpec {
    /// PipeWire node.name (stable, used for cleanup).
    pub node_name: String,
    /// Human-readable name shown in sound settings.
    pub description: String,
    /// true = virtual microphone (a bus), false = virtual output (a strip).
    pub is_source: bool,
    /// Switched on in the config (`cable = on`).
    pub enabled: bool,
}

impl CableSpec {
    /// Regex matching this cable's JACK client name. PipeWire's JACK layer
    /// names ports after node.description by default (node.name if
    /// `jack.short-name` is set), so accept either.
    pub fn jack_regex(&self) -> String {
        format!("^({}|{})[^:]*:", regex_escape(&self.node_name), regex_escape(&self.description))
    }
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let t = t.trim_matches('-').to_string();
    t.chars().take(24).collect()
}

/// The cables every channel can have (whether switched on or not), plus the
/// cables that exist under an older name (a channel renamed while it runs).
#[derive(Debug, Clone)]
pub struct CableSet {
    pub strips: Vec<CableSpec>, // NUM_STRIPS
    pub buses: Vec<CableSpec>,  // NUM_BUSES (A1..B2)
    /// Live cables whose description is not the configured one any more.
    pub older: Vec<CableSpec>,
}

impl CableSet {
    /// The cable of channel `ch` (strips 0-5, then buses).
    pub fn spec(&self, ch: usize) -> Option<&CableSpec> {
        if ch < NUM_STRIPS { self.strips.get(ch) } else { self.buses.get(ch - NUM_STRIPS) }
    }

    /// Also count these live cables as ours (they may carry an older name).
    pub fn with_live(mut self, cables: &Cables) -> CableSet {
        for c in cables.live.iter().flatten() {
            if !self.all().any(|x| x.description == c.description) {
                self.older.push(c.clone());
            }
        }
        self
    }

    pub fn for_config(cfg: &Config) -> CableSet {
        let prefix = node_prefix(&cfg.client_name);
        let strips = (0..NUM_STRIPS)
            .map(|i| {
                let nm = sanitize(&cfg.strips[i].name);
                CableSpec {
                    node_name: format!("{prefix}_in{}", i + 1),
                    description: if nm.is_empty() {
                        format!("{}-In{}", cfg.client_name, i + 1)
                    } else {
                        format!("{}-In{}-{nm}", cfg.client_name, i + 1)
                    },
                    is_source: false,
                    enabled: cfg.strips[i].cable,
                }
            })
            .collect();
        let buses = (0..NUM_BUSES)
            .map(|b| CableSpec {
                node_name: format!("{prefix}_out_{}", BUS_IDS[b].to_ascii_lowercase()),
                description: format!("{}-Out-{}", cfg.client_name, BUS_IDS[b]),
                is_source: true,
                enabled: cfg.buses[b].cable,
            })
            .collect();
        CableSet { strips, buses, older: Vec::new() }
    }

    pub fn all(&self) -> impl Iterator<Item = &CableSpec> {
        self.strips.iter().chain(self.buses.iter()).chain(self.older.iter())
    }

    pub fn node_names(&self) -> Vec<String> {
        self.all().map(|c| c.node_name.clone()).collect()
    }

    /// Is this PipeWire node (by node.name or description) one of our cables?
    pub fn owns(&self, node_name: &str, description: &str) -> bool {
        self.all().any(|c| c.node_name == node_name || c.description == description)
    }
}

/// Run pactl (C locale, so its output can be parsed).
pub fn pactl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("pactl").args(args).env("LC_ALL", "C").stdin(Stdio::null()).output().map_err(|e| {
        format!("cannot run pactl ({e}); install pulseaudio-utils (Fedora, Debian, Ubuntu) or libpulse (Arch)")
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Module ids in `pactl list short modules` output that are null-sinks with
/// one of `node_names` as their exact `sink_name`.
fn matching_modules(list: &str, node_names: &[String]) -> Vec<String> {
    let ours: Vec<String> = node_names.iter().map(|n| format!("sink_name={n}")).collect();
    list.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let (Some(id), Some(name), args) = (f.next(), f.next(), f.next().unwrap_or("")) else { return None };
            (name == "module-null-sink" && args.split_whitespace().any(|a| ours.iter().any(|o| o == a)))
                .then(|| id.to_string())
        })
        .collect()
}

/// Unload every cable module with one of these node names. Returns how many
/// were removed.
pub fn sweep(node_names: &[String]) -> Result<usize, String> {
    let list = pactl(&["list", "short", "modules"])?;
    Ok(matching_modules(&list, node_names).iter().filter(|id| pactl(&["unload-module", id]).is_ok()).count())
}

/// Is a cable module with this node name loaded?
fn loaded(node_name: &str) -> Result<bool, String> {
    let list = pactl(&["list", "short", "modules"])?;
    Ok(!matching_modules(&list, &[node_name.to_string()]).is_empty())
}

/// A cable that someone removes again this soon after BananaSplit made it
/// again is left off.
const REMAKE_GRACE: Duration = Duration::from_secs(60);
/// How long BananaSplit keeps trying to make a cable again when that fails
/// (the sound server may still be starting).
const REMAKE_TRIES: Duration = Duration::from_secs(30);

/// What [`Cables::check`] did about a cable that had disappeared.
#[derive(Debug, Clone, PartialEq)]
pub enum CableEvent {
    /// Channel, cable name: it is there again.
    Remade(usize, String),
    /// Channel, cable name, and the error if it could not be made again
    /// (None: it was removed again soon after it was made again): it stays off.
    Lost(usize, String, Option<String>),
}

/// "strip 3", "B1"
fn channel_id(ch: usize) -> String {
    if ch < NUM_STRIPS { format!("strip {}", ch + 1) } else { BUS_IDS[ch - NUM_STRIPS].to_string() }
}

/// `"a"`, `"a" and "b"`, `"a", "b" and "c"`
fn quoted_list(names: &[&str]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    match q.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => q.concat(),
    }
}

/// One line for the status (and the log) about what [`Cables::check`] did,
/// if anything (`menus`: the channels have menus to switch a cable on).
pub fn summary(events: &[CableEvent], menus: bool) -> Option<String> {
    let mut parts = Vec::new();
    let remade: Vec<&str> = events.iter().filter_map(|e| if let CableEvent::Remade(_, n) = e { Some(n.as_str()) } else { None }).collect();
    match remade.as_slice() {
        [] => {}
        [one] => parts.push(format!(
            "the virtual cable \"{one}\" had disappeared (pipewire-pulse restarted, or its module was unloaded): made it again"
        )),
        many => parts.push(format!(
            "the virtual cables {} had disappeared (pipewire-pulse restarted, or their modules were unloaded): made them again",
            quoted_list(many)
        )),
    }
    let again: Vec<(usize, &str)> =
        events.iter().filter_map(|e| if let CableEvent::Lost(ch, n, None) = e { Some((*ch, n.as_str())) } else { None }).collect();
    match again.as_slice() {
        [] => {}
        [(ch, one)] => parts.push(format!(
            "the virtual cable \"{one}\" was removed again, so BananaSplit leaves it off ({})",
            if menus { format!("switch it on in {}'s menu", channel_id(*ch)) } else { "restart BananaSplit to make it again".into() }
        )),
        many => parts.push(format!(
            "the virtual cables {} were removed again, so BananaSplit leaves them off ({})",
            quoted_list(&many.iter().map(|(_, n)| *n).collect::<Vec<_>>()),
            if menus { "switch them on in their channels' menus" } else { "restart BananaSplit to make them again" }
        )),
    }
    for e in events {
        if let CableEvent::Lost(ch, n, Some(err)) = e {
            parts.push(format!(
                "the virtual cable \"{n}\" disappeared and could not be made again ({err}); {}",
                if menus { format!("switch it on in {}'s menu", channel_id(*ch)) } else { "restart BananaSplit to make it again".into() }
            ));
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// What the node names of a BananaSplit's cables start with
/// (`bananasplit_in1`): its client name in lower case, `-` as `_`.
pub fn node_prefix(client: &str) -> String {
    client.to_ascii_lowercase().replace('-', "_")
}

/// The cable watchdogs of this user (other than this process): PID and the
/// client name of the BananaSplit each one watches. Another user's cables
/// are in their own sound server, so their watchdogs don't count.
fn reapers() -> Vec<(u32, String)> {
    use std::os::unix::fs::MetadataExt;
    let me = std::process::id();
    let uid = |pid: &str| std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()).ok();
    let Some(mine) = uid("self") else { return Vec::new() };
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me && uid(&pid.to_string()) == Some(mine))
        .filter_map(|pid| {
            let b = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let mut a = b.split(|&c| c == 0).skip(1);
            (a.next() == Some(REAPER_FLAG.as_bytes())).then_some(())?;
            Some((pid, String::from_utf8_lossy(a.next()?).into_owned()))
        })
        .collect()
}

/// PIDs of cable watchdogs for `client` (other than this process).
fn reapers_of(client: &str) -> Vec<u32> {
    reapers().into_iter().filter(|(_, c)| c == client).map(|(pid, _)| pid).collect()
}

/// Another BananaSplit that is running and whose cables have the names
/// `client`'s would have: client names that differ only in upper and lower
/// case or `-` and `_` ("BananaSplit", "bananasplit"). `jack_clients`: the
/// clients JACK lists with a BananaSplit's ports. Its name, if so.
pub fn sharing_names(client: &str, jack_clients: &[String]) -> Option<String> {
    let mine = node_prefix(client);
    let others = reapers().into_iter().map(|(_, c)| c).chain(jack_clients.iter().cloned());
    others.into_iter().find(|c| c != client && node_prefix(c) == mine)
}

/// Why there are no cables when [`sharing_names`] found `other`.
pub fn sharing_message(client: &str, other: &str) -> String {
    format!(
        "no virtual cables: BananaSplit \"{other}\" is running, and its cables have the names this one's (\"{client}\") would \
         have ({}_in1, …); give one of them a client_name that differs by more than upper/lower case and dashes",
        node_prefix(client)
    )
}

/// Wait (up to `timeout`) until no cable watchdog for `client` is running.
/// Called after BananaSplit owns its JACK name, so any watchdog left belongs to a
/// BananaSplit that has just died and is removing its cables right now; creating
/// new cables before it finishes could race with it. Returns false on timeout.
pub fn wait_for_reapers(client: &str, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if reapers_of(client).is_empty() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// `bananasplit --reap-cables NAME`: read the cable node names from stdin, then
/// wait. If BananaSplit sends "done" it removed the cables itself; if the pipe just
/// closes, BananaSplit died without cleaning up, so remove them here.
pub fn reaper_main(client: &str) -> ExitCode {
    sys::ignore_termination_signals();
    sys::close_inherited_fds();
    let mut names = Vec::new();
    for line in std::io::stdin().lock().lines() {
        match line {
            Ok(l) if l == DISARM => return ExitCode::SUCCESS,
            Ok(l) if !l.is_empty() => names.push(l),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    match sweep(&names) {
        Ok(0) => {}
        Ok(n) => eprintln!("bananasplit: removed {n} virtual cables of \"{client}\" (BananaSplit did not exit cleanly)"),
        Err(e) => eprintln!("bananasplit: could not remove the virtual cables of \"{client}\": {e}"),
    }
    ExitCode::SUCCESS
}

struct Reaper {
    child: Child,
    stdin: ChildStdin,
}

fn spawn_reaper(client: &str, set: &CableSet) -> Result<Reaper, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args([REAPER_FLAG, client])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no pipe")?;
    let mut text = set.node_names().join("\n");
    text.push('\n');
    if let Err(e) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.to_string());
    }
    Ok(Reaper { child, stdin })
}

/// Load one cable; returns its module id (only to check pactl's answer:
/// see [`Cables`]).
fn load(c: &CableSpec) -> Result<u32, String> {
    let sink_name = format!("sink_name={}", c.node_name);
    let props = format!("sink_properties=device.description={}", c.description);
    let node_desc = format!("node.description={}", c.description);
    let mut args = vec!["load-module", "module-null-sink", sink_name.as_str(), props.as_str(), node_desc.as_str()];
    if c.is_source {
        args.push("media.class=Audio/Source/Virtual");
    } else {
        args.push("media.class=Audio/Sink");
    }
    args.push("channels=2");
    args.push("channel_map=front-left,front-right");
    let out = pactl(&args)?;
    out.trim().parse::<u32>().map_err(|_| format!("unexpected pactl output for {}: {out:?}", c.description))
}

/// Loaded cable modules; unloaded on drop. Modules are found by their
/// exact `sink_name` whenever one is unloaded, not by the id they had.
pub struct Cables {
    /// Per channel (strips 1-6, then A1..B2): the cable it has.
    live: Vec<Option<CableSpec>>,
    /// When BananaSplit last made the channel's cable again.
    remade: Vec<Option<Instant>>,
    /// Since when making the channel's cable again has been failing.
    failing: Vec<Option<Instant>>,
    reaper: Option<Reaper>,
    /// Why no cable can be made (`create_cables = false`, `--no-cables`,
    /// not PipeWire, no pactl), or None.
    off: Option<String>,
}

impl Cables {
    /// Start the watchdog and create the cables that are switched on.
    /// Returns the guard plus non-fatal warnings. Leftovers of an earlier
    /// BananaSplit must already be gone (see [`sweep`]).
    pub fn create(client: &str, set: &CableSet) -> (Cables, Vec<String>) {
        let mut warnings = Vec::new();
        let mut me = Cables::none("");
        me.off = None;

        match pactl(&["info"]) {
            Ok(info) if info.contains("PipeWire") => {}
            Ok(_) => {
                warnings.push(
                    "virtual cables need PipeWire (pipewire-pulse); the sound server is not PipeWire, skipping cables".into(),
                );
                me.off = Some("virtual cables need PipeWire".into());
                return (me, warnings);
            }
            Err(e) => {
                warnings.push(format!("no virtual cables: {e}"));
                me.off = Some(format!("no virtual cables: {e}"));
                return (me, warnings);
            }
        }

        // The watchdog first, so even a kill during creation is cleaned up.
        // It learns every cable a channel can have, so cables switched on
        // later are covered too.
        match spawn_reaper(client, set) {
            Ok(r) => me.reaper = Some(r),
            Err(e) => warnings.push(format!(
                "cable watchdog not started ({e}); if BananaSplit is killed, run `bananasplit --cleanup` to remove its cables"
            )),
        }

        for ch in 0..NUM_CHANNELS {
            let Some(c) = set.spec(ch).filter(|c| c.enabled) else { continue };
            match load(c) {
                Ok(_) => me.live[ch] = Some(c.clone()),
                Err(e) => warnings.push(e),
            }
        }
        (me, warnings)
    }

    /// No cables, and why (shown when someone switches one on).
    pub fn none(why: &str) -> Cables {
        Cables {
            live: vec![None; NUM_CHANNELS],
            remade: vec![None; NUM_CHANNELS],
            failing: vec![None; NUM_CHANNELS],
            reaper: None,
            off: Some(why.to_string()),
        }
    }

    pub fn count(&self) -> usize {
        self.live.iter().flatten().count()
    }

    /// Making a cable again failed and BananaSplit tries again (see
    /// [`Cables::check`]): worth looking at the sound server again soon.
    pub fn retrying(&self) -> bool {
        self.failing.iter().any(Option::is_some)
    }

    /// The cable channel `ch` has now.
    pub fn live(&self, ch: usize) -> Option<&CableSpec> {
        self.live.get(ch).and_then(Option::as_ref)
    }

    /// Why no cable can be made, if so.
    pub fn unavailable(&self) -> Option<&str> {
        self.off.as_deref()
    }

    /// Switch channel `ch`'s cable on (`spec`) while BananaSplit runs (or
    /// make it again, if it has disappeared).
    pub fn add(&mut self, ch: usize, spec: &CableSpec) -> Result<(), String> {
        if let Some(why) = &self.off {
            return Err(why.clone());
        }
        if ch >= NUM_CHANNELS {
            return Ok(());
        }
        if let Some(c) = &self.live[ch] {
            if loaded(&c.node_name)? {
                return Ok(());
            }
            self.live[ch] = None;
        }
        load(spec)?;
        self.live[ch] = Some(spec.clone());
        // switched on by hand: should it disappear, it is made again
        self.remade[ch] = None;
        self.failing[ch] = None;
        Ok(())
    }

    /// Switch channel `ch`'s cable off. PipeWire moves the apps that played
    /// into it (or recorded from it) to the default device. A cable that is
    /// already gone is simply off.
    pub fn remove(&mut self, ch: usize) -> Result<(), String> {
        let Some(c) = self.live.get(ch).and_then(Option::as_ref) else { return Ok(()) };
        let list = pactl(&["list", "short", "modules"])?;
        for id in matching_modules(&list, std::slice::from_ref(&c.node_name)) {
            pactl(&["unload-module", &id])?;
        }
        self.live[ch] = None;
        self.failing[ch] = None;
        Ok(())
    }

    /// Make the cables that disappeared while BananaSplit runs again
    /// (pipewire-pulse restarted, or someone unloaded their module), with
    /// their current names (`set`). `devices` is the sound server's latest
    /// device list (None: it cannot be asked right now); a cable missing
    /// from it is looked up among the loaded modules before anything is
    /// done, since the list can be older than a cable made a moment ago.
    /// `restarted`: the server went away since the last call, so cables that
    /// are gone went with it (nobody removed them).
    pub fn check(&mut self, set: &CableSet, devices: Option<&Devices>, restarted: bool) -> Vec<CableEvent> {
        if restarted {
            self.remade.iter_mut().for_each(|t| *t = None);
        }
        let Some(d) = devices.filter(|_| self.off.is_none()) else { return Vec::new() };
        let gone: Vec<usize> = (0..NUM_CHANNELS)
            .filter(|&ch| self.live(ch).is_some_and(|c| !d.list.iter().any(|x| x.name == c.node_name && x.is_source == c.is_source)))
            .collect();
        // the cables that are there (again) need no more tries
        for ch in (0..NUM_CHANNELS).filter(|ch| !gone.contains(ch)) {
            self.failing[ch] = None;
        }
        if gone.is_empty() {
            return Vec::new();
        }
        let Ok(list) = pactl(&["list", "short", "modules"]) else { return Vec::new() };
        let mut events = Vec::new();
        for ch in gone {
            let Some(c) = self.live[ch].clone() else { continue };
            // (the device list can be older than a cable made a moment ago)
            if !matching_modules(&list, std::slice::from_ref(&c.node_name)).is_empty() {
                self.failing[ch] = None;
                continue;
            }
            let spec = set.spec(ch).cloned().unwrap_or(c);
            if self.remade[ch].is_some_and(|t| t.elapsed() < REMAKE_GRACE) {
                self.live[ch] = None;
                self.failing[ch] = None;
                events.push(CableEvent::Lost(ch, spec.description, None));
                continue;
            }
            match load(&spec) {
                Ok(_) => {
                    self.remade[ch] = Some(Instant::now());
                    self.failing[ch] = None;
                    events.push(CableEvent::Remade(ch, spec.description.clone()));
                    self.live[ch] = Some(spec);
                }
                // the server may still be starting: try again at the next
                // look, for a while (the channel keeps its cable meanwhile)
                Err(e) => {
                    if self.failing[ch].get_or_insert_with(Instant::now).elapsed() >= REMAKE_TRIES {
                        self.live[ch] = None;
                        self.failing[ch] = None;
                        events.push(CableEvent::Lost(ch, spec.description, Some(e)));
                    }
                }
            }
        }
        events
    }
}

#[cfg(test)]
impl Cables {
    /// Pretend channel `ch` has `spec` as its cable (nothing is loaded).
    pub fn test_live(&mut self, ch: usize, spec: CableSpec) {
        self.live[ch] = Some(spec);
    }

    /// Forget the pretend cables (so that dropping unloads nothing).
    pub fn test_clear(&mut self) {
        self.live.iter_mut().for_each(|c| *c = None);
    }
}

impl Drop for Cables {
    fn drop(&mut self) {
        let names: Vec<String> = self.live.iter_mut().filter_map(Option::take).map(|c| c.node_name).collect();
        if !names.is_empty() {
            let _ = sweep(&names);
        }
        // Tell the watchdog the cables are gone, then let it exit.
        if let Some(mut r) = self.reaper.take() {
            let _ = writeln!(r.stdin, "{DISARM}");
            drop(r.stdin);
            let _ = r.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_and_safe() {
        let mut cfg = Config::default();
        cfg.strips[2].name = "Desk top (main)!".into();
        cfg.buses[1].cable = true;
        cfg.strips[5].cable = false;
        let set = CableSet::for_config(&cfg);
        assert_eq!(set.strips.len(), NUM_STRIPS);
        assert_eq!(set.buses.len(), NUM_BUSES);
        assert_eq!(set.strips[2].node_name, "bananasplit_in3");
        assert_eq!(set.strips[2].description, "BananaSplit-In3-Desk-top--main");
        assert_eq!(set.buses[3].node_name, "bananasplit_out_b1");
        assert_eq!(set.buses[4].description, "BananaSplit-Out-B2");
        assert_eq!(set.buses[0].description, "BananaSplit-Out-A1");
        assert!(set.buses.iter().all(|b| b.is_source) && set.strips.iter().all(|s| !s.is_source));
        let on: Vec<bool> = set.strips.iter().chain(&set.buses).map(|c| c.enabled).collect();
        assert_eq!(on, [true, true, true, true, true, false, false, true, false, true, true]);
        assert_eq!(set.spec(7).unwrap().node_name, "bananasplit_out_a2");
        assert!(set.spec(11).is_none());
        // every cable a channel can have is swept, switched on or not
        assert_eq!(set.node_names().len(), NUM_CHANNELS);
        assert_eq!(set.strips[0].jack_regex(), "^(bananasplit_in1|BananaSplit-In1-Mic)[^:]*:");
        for c in set.all() {
            assert!(!c.description.contains(' '), "pactl args must not contain spaces");
        }
        assert!(set.owns("bananasplit_out_b1", "whatever"));
        assert!(set.owns("x", "BananaSplit-In1-Mic"));
        assert!(set.owns("x", "BananaSplit-Out-A1"));
        assert!(!set.owns("alsa_input.usb-Blue", "Yeti Stereo Microphone"));
        // a live cable made before a rename still counts as ours
        let mut cables = Cables::none("test");
        let mut old = set.strips[2].clone();
        old.description = "BananaSplit-In3-Desktop".into();
        cables.live[2] = Some(old);
        let set = set.with_live(&cables);
        assert!(set.owns("x", "BananaSplit-In3-Desktop") && set.owns("x", "BananaSplit-In3-Desk-top--main"));
        cables.live[2] = None; // (not unloaded by Drop)
        assert_eq!(cables.count(), 0);
        assert!(cables.add(0, &set.strips[0]).unwrap_err().contains("test"));
    }

    #[test]
    fn what_happened_to_cables_in_one_line() {
        use CableEvent::*;
        assert_eq!(summary(&[], true), None);
        let one = summary(&[Remade(2, "BananaSplit-In3-Desktop".into())], true).unwrap();
        assert!(one.starts_with("the virtual cable \"BananaSplit-In3-Desktop\" had disappeared") && one.ends_with("made it again"), "{one}");
        let all = summary(&[Remade(0, "A".into()), Remade(1, "B".into()), Remade(9, "C".into())], true).unwrap();
        assert!(all.starts_with("the virtual cables \"A\", \"B\" and \"C\" had disappeared") && all.ends_with("made them again"), "{all}");
        let off = summary(&[Lost(9, "BananaSplit-Out-B1".into(), None)], true).unwrap();
        assert!(off.ends_with("(switch it on in B1's menu)"), "{off}");
        let two = summary(&[Lost(3, "D".into(), None), Lost(4, "E".into(), None), Remade(0, "A".into())], false).unwrap();
        assert!(two.contains("made it again; the virtual cables \"D\" and \"E\" were removed again") && two.ends_with("(restart BananaSplit to make them again)"), "{two}");
        let err = summary(&[Lost(2, "F".into(), Some("pactl: Connection refused".into()))], true).unwrap();
        assert!(err.contains("could not be made again (pactl: Connection refused); switch it on in strip 3's menu"), "{err}");
    }

    #[test]
    fn names_that_share_cables() {
        assert_eq!(node_prefix("BananaSplit"), "bananasplit");
        assert_eq!(node_prefix("Banana-Split"), node_prefix("banana_split"));
        let jack = vec!["bananasplit".to_string(), "Studio".to_string()];
        assert_eq!(sharing_names("BananaSplit", &jack).as_deref(), Some("bananasplit"));
        assert_eq!(sharing_names("bananasplit", &jack), None, "itself");
        assert_eq!(sharing_names("Studio-2", &jack), None);
        assert!(sharing_message("BananaSplit", "bananasplit").contains("bananasplit_in1"));
    }

    #[test]
    fn module_matching_is_exact() {
        let list = "7\tmodule-null-sink\tsink_name=bananasplit_in3 sink_properties=device.description=old\n\
                    8\tmodule-null-sink\tsink_name=bananasplit_in30\n\
                    9\tmodule-null-sink\tsink_name=someone_else\n\
                    10\tmodule-loopback\tsink_name=bananasplit_in1\n\
                    11\tmodule-null-sink\tsink_name=bananasplit_out_b2 channels=2\n\
                    12\tmodule-null-sink\tsink_name=bananasplit_out_a3\n";
        let names = CableSet::for_config(&Config::default()).node_names();
        assert_eq!(matching_modules(list, &names), ["7", "11", "12"]);
        assert!(matching_modules(list, &[]).is_empty());
    }
}
