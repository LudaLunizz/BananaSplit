//! macOS channel menus: Core Audio devices, and each channel's virtual
//! cable.
//!
//! A cable on macOS is one loopback device (BlackHole and the like): what
//! is played into it comes out of its input. Every channel whose cable is
//! on gets one of its own, strips first, then buses: a strip records it
//! (apps play into it), a bus plays into it (apps record it as their
//! microphone). Cables have no channel names, so a channel keeps the one it
//! was given (`<config folder>/cables.txt`): switching another channel's
//! cable on or off, a restart, or a cable gone for a moment (a driver
//! update) moves no channel's apps to another channel.

#![allow(clippy::needless_range_loop)] // index loops mirror the channel layout

use super::cables;
use super::route::{self, DevInfo};
use super::{Snapshot, snapshot};
use crate::gui::{ChanView, DevOp, jstr};
use bananasplit::config::{Config, DEFAULT_DEVICE, PHYSICAL};
use bananasplit::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const NUM_CHANNELS: usize = NUM_STRIPS + NUM_BUSES;

/// The cable each channel uses (by channel: strips, then buses).
pub type Cables = [Option<String>; NUM_CHANNELS];

/// Channel `ch`'s id in the page and in `cables.txt`: "1" .. "6", "A1" .. "B2".
fn id_of(ch: usize) -> String {
    if ch < NUM_STRIPS { (ch + 1).to_string() } else { BUS_IDS[ch - NUM_STRIPS].to_string() }
}

/// "strip 3", "A1": for messages.
fn name_of(ch: usize) -> String {
    if ch < NUM_STRIPS { format!("strip {}", ch + 1) } else { BUS_IDS[ch - NUM_STRIPS].to_string() }
}

fn connect(cfg: &Config, ch: usize) -> &Vec<String> {
    if ch < NUM_STRIPS { &cfg.strips[ch].connect } else { &cfg.buses[ch - NUM_STRIPS].connect }
}

fn connect_mut(cfg: &mut Config, ch: usize) -> &mut Vec<String> {
    if ch < NUM_STRIPS { &mut cfg.strips[ch].connect } else { &mut cfg.buses[ch - NUM_STRIPS].connect }
}

fn cable_on(cfg: &Config, ch: usize) -> bool {
    if ch < NUM_STRIPS { cfg.strips[ch].cable } else { cfg.buses[ch - NUM_STRIPS].cable }
}

fn set_cable(cfg: &mut Config, ch: usize, on: bool) {
    if ch < NUM_STRIPS {
        cfg.strips[ch].cable = on;
    } else {
        cfg.buses[ch - NUM_STRIPS].cable = on;
    }
}

/// The devices of channel `ch`'s side (inputs for a strip, outputs for a
/// bus) and that side's default device.
fn side(s: &Snapshot, ch: usize) -> (&[DevInfo], Option<&str>) {
    if ch < NUM_STRIPS { (&s.caps, s.def_cap.as_deref()) } else { (&s.rens, s.def_ren.as_deref()) }
}

/// Does one of channel `ch`'s own `connect` lines bring in device `name`?
fn brings_in(cfg: &Config, ch: usize, devs: &[DevInfo], def: Option<&str>, name: &str) -> bool {
    connect(cfg, ch).iter().any(|x| x.eq_ignore_ascii_case(name) || route::resolve(x, devs, def).is_some_and(|d| d.name.eq_ignore_ascii_case(name)))
}

/// Which cable each channel gets. `held`: what each had before, which it
/// keeps while its cable is on and the cable is installed; the others get
/// a free one, strips first, in order. A cable the channel's own `connect`
/// lines already bring in is not its cable.
pub fn plan(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo], held: &Cables) -> Cables {
    let installed = cables::installed(caps, rens);
    let mut used = vec![false; installed.len()];
    let mut out: Cables = Default::default();
    let own = |ch: usize, c: &str| {
        let devs = if ch < NUM_STRIPS { caps } else { rens };
        connect(cfg, ch).iter().any(|x| x != DEFAULT_DEVICE && x != PHYSICAL && route::resolve(x, devs, None).is_some_and(|d| d.name.eq_ignore_ascii_case(c)))
    };
    // the ones channels had
    for ch in 0..NUM_CHANNELS {
        if !cable_on(cfg, ch) {
            continue;
        }
        if let Some(k) = held[ch].as_deref().and_then(|h| installed.iter().position(|c| c == h)) {
            if !used[k] && !own(ch, &installed[k]) {
                used[k] = true;
                out[ch] = Some(installed[k].clone());
            }
        }
    }
    // free ones for the rest
    for ch in 0..NUM_CHANNELS {
        if !cable_on(cfg, ch) || out[ch].is_some() {
            continue;
        }
        if let Some(k) = (0..installed.len()).find(|&k| !used[k] && !own(ch, &installed[k])) {
            used[k] = true;
            out[ch] = Some(installed[k].clone());
        }
    }
    out
}

/// What to keep for next time: the cables channels use now, and the one
/// a channel had whose cable is not installed right now (it gets it back
/// when the cable is back). A channel whose cable is off keeps none.
pub fn to_hold(cfg: &Config, now: &Cables, held: &Cables, installed: &[String]) -> Cables {
    std::array::from_fn(|ch| {
        if !cable_on(cfg, ch) {
            return None;
        }
        now[ch].clone().or_else(|| held[ch].clone().filter(|h| !installed.contains(h) && !now.contains(&Some(h.clone()))))
    })
}

/// `cables.txt`: one `<channel> = <cable>` line per channel with one.
pub fn held_text(c: &Cables) -> String {
    let mut o = String::from("# The virtual cable of each channel, kept by BananaSplit so a channel keeps its own.\n");
    for (ch, name) in c.iter().enumerate() {
        if let Some(n) = name {
            o.push_str(&format!("{} = {n}\n", id_of(ch)));
        }
    }
    o
}

pub fn parse_held(text: &str) -> Cables {
    let mut c: Cables = Default::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((id, name)) = line.split_once('=') else { continue };
        let (id, name) = (id.trim(), name.trim());
        if let Some(ch) = (0..NUM_CHANNELS).find(|&ch| id_of(ch).eq_ignore_ascii_case(id)) {
            if !name.is_empty() {
                c[ch] = Some(name.to_string());
            }
        }
    }
    c
}

struct Held {
    file: Option<PathBuf>,
    cables: Cables,
}

static HELD: Mutex<Held> = Mutex::new(Held { file: None, cables: [const { None }; NUM_CHANNELS] });

fn held() -> MutexGuard<'static, Held> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keep the channels' cables in `file` (next to the config); read what it
/// holds now.
pub fn keep_cables_in(file: &Path) {
    let mut h = held();
    h.cables = std::fs::read_to_string(file).map(|t| parse_held(&t)).unwrap_or_default();
    h.file = Some(file.to_path_buf());
}

/// The cables for these devices now; `held` becomes what to keep.
fn settle(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo], held: &mut Cables) -> Cables {
    let now = plan(cfg, caps, rens, held);
    *held = to_hold(cfg, &now, held, &cables::installed(caps, rens));
    now
}

/// Run `f` on the held cables; write them to the file when they changed.
fn with_held<T>(f: impl FnOnce(&mut Cables) -> T) -> T {
    let mut h = held();
    let before = h.cables.clone();
    let r = f(&mut h.cables);
    if h.cables != before {
        if let Some(file) = &h.file {
            let _ = bananasplit::config::write_replacing(file, &held_text(&h.cables));
        }
    }
    r
}

/// The cables for these devices now, remembered.
fn current(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo]) -> Cables {
    with_held(|h| settle(cfg, caps, rens, h))
}

/// The cables for a device list the caller already has (the audio graph
/// uses this, so what the menus show is what is connected).
pub fn cable_extra(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo]) -> ([Option<String>; NUM_STRIPS], [Option<String>; NUM_BUSES]) {
    let c = current(cfg, caps, rens);
    (std::array::from_fn(|s| c[s].clone()), std::array::from_fn(|b| c[NUM_STRIPS + b].clone()))
}

/// The devices channel `ch`'s `connect` lines bring in now, its cable
/// left out (that has a section of its own).
fn connected<'a>(cfg: &Config, ch: usize, devs: &'a [DevInfo], def: Option<&str>, cable: Option<&str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = Vec::new();
    for x in connect(cfg, ch) {
        if let Some(d) = route::resolve(x, devs, def) {
            if Some(d.name.as_str()) != cable && !v.contains(&d.name.as_str()) {
                v.push(&d.name);
            }
        }
    }
    v
}

fn candidates(o: &mut String, devs: &[DevInfo], def: Option<&str>) {
    o.push('[');
    for (k, d) in devs.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str("{\"client\":");
        jstr(o, &d.name);
        o.push_str(",\"label\":");
        jstr(o, &d.name);
        o.push_str(",\"kind\":\"device\",\"default\":");
        o.push_str(if def == Some(d.id.as_str()) { "true" } else { "false" });
        o.push('}');
    }
    o.push(']');
}

/// `GET /api/devices`: the channel menus' contents.
pub fn list_json(cfg: &Config, s: &Snapshot, cables: &Cables) -> String {
    let none = cables::installed(&s.caps, &s.rens).is_empty();
    let mut o = String::from("{\"cables\":{\"ok\":");
    o.push_str(if none { "false" } else { "true" });
    o.push_str(",\"why\":");
    jstr(&mut o, if none { cables::MISSING } else { "" });
    o.push_str("},\"pulse\":false,\"strip\":");
    candidates(&mut o, &s.caps, s.def_cap.as_deref());
    o.push_str(",\"bus\":");
    candidates(&mut o, &s.rens, s.def_ren.as_deref());
    o.push_str(",\"play\":[],\"rec\":[],\"ch\":[");
    for ch in 0..NUM_CHANNELS {
        if ch > 0 {
            o.push(',');
        }
        let (devs, def) = side(s, ch);
        let cable = cables[ch].as_deref();
        o.push_str("{\"id\":");
        jstr(&mut o, &id_of(ch));
        o.push_str(",\"cable\":{\"on\":");
        o.push_str(if cable.is_some() { "true" } else { "false" });
        o.push_str(",\"name\":");
        jstr(&mut o, cable.unwrap_or(if ch < NUM_STRIPS { "Virtual cable" } else { "Virtual microphone" }));
        o.push_str("},\"conn\":[");
        for (k, n) in connected(cfg, ch, devs, def, cable).iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, n);
        }
        o.push_str("],\"connect\":[");
        for (k, p) in connect(cfg, ch).iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, p);
        }
        o.push_str("]}");
    }
    o.push_str("]}");
    o
}

/// Why channel `ch`'s cable can't be switched on although cables are
/// installed.
fn no_free_cable(installed: usize) -> String {
    format!(
        "All {installed} installed virtual cable(s) are in use by other channels: switch another channel's cable off, or install another one (BlackHole 16ch, 64ch: Menu › Virtual cables…)"
    )
}

/// One change from a channel menu (`held`: the channels' cables, kept up
/// to date); a few words on what it did.
pub fn apply(op: DevOp, cfg: &mut Config, s: &Snapshot, held: &mut Cables) -> Result<String, String> {
    let check = |ch: usize| if ch < NUM_CHANNELS { Ok(ch) } else { Err("bad channel".to_string()) };
    match op {
        DevOp::List => Ok(list_json(cfg, s, &settle(cfg, &s.caps, &s.rens, held))),
        DevOp::Client { ch, client, on } => {
            let ch = check(ch)?;
            let (devs, def) = side(s, ch);
            if on {
                // a line that brings it in already is enough
                if !brings_in(cfg, ch, devs, def, &client) {
                    connect_mut(cfg, ch).push(client.clone());
                }
                Ok(if ch < NUM_STRIPS { format!("{} takes in {client}", name_of(ch)) } else { format!("{} plays on {client}", name_of(ch)) })
            } else {
                // every line that brings it in goes (a `@default` line too),
                // so it stays away
                connect_mut(cfg, ch).retain(|x| !(x.eq_ignore_ascii_case(&client) || route::resolve(x, devs, def).is_some_and(|d| d.name.eq_ignore_ascii_case(&client))));
                Ok(format!("{} and {client} disconnected", name_of(ch)))
            }
        }
        DevOp::Default { ch, on } => {
            let ch = check(ch)?;
            let what = if ch < NUM_STRIPS { "default input" } else { "default output" };
            let list = connect_mut(cfg, ch);
            if on {
                if !list.iter().any(|x| x == DEFAULT_DEVICE) {
                    list.push(DEFAULT_DEVICE.into());
                }
                Ok(format!("{} follows the macOS {what}", name_of(ch)))
            } else {
                list.retain(|x| x != DEFAULT_DEVICE);
                Ok(format!("{} no longer follows the macOS {what}", name_of(ch)))
            }
        }
        DevOp::Unpattern { ch, pattern } => {
            let ch = check(ch)?;
            let list = connect_mut(cfg, ch);
            let before = list.len();
            list.retain(|x| *x != pattern);
            if list.len() == before {
                return Err(format!("{} has no line \"connect = {pattern}\"", name_of(ch)));
            }
            Ok(format!("removed \"connect = {pattern}\" from {}", name_of(ch)))
        }
        DevOp::Nothing { ch } => {
            let ch = check(ch)?;
            connect_mut(cfg, ch).clear();
            let cable = settle(cfg, &s.caps, &s.rens, held)[ch].is_some();
            Ok(format!("{} is not connected to anything{}", name_of(ch), if cable { " but its virtual cable" } else { "" }))
        }
        DevOp::Cable { ch, on } => {
            let ch = check(ch)?;
            if !on {
                set_cable(cfg, ch, false);
                settle(cfg, &s.caps, &s.rens, held);
                return Ok(format!("{}'s virtual cable is off", name_of(ch)));
            }
            let installed = cables::installed(&s.caps, &s.rens).len();
            if installed == 0 {
                return Err(cables::MISSING.into());
            }
            set_cable(cfg, ch, true);
            match settle(cfg, &s.caps, &s.rens, held)[ch].clone() {
                Some(name) => Ok(if ch < NUM_STRIPS {
                    format!("\"{name}\" is on: choose it as an app's output to play into {}", name_of(ch))
                } else {
                    format!("\"{name}\" is on: choose it as the microphone in an app to record {}", name_of(ch))
                }),
                None => {
                    set_cable(cfg, ch, false);
                    settle(cfg, &s.caps, &s.rens, held);
                    Err(no_free_cable(installed))
                }
            }
        }
        DevOp::Stream { .. } => Err("macOS doesn't let BananaSplit move another app's sound: choose the channel's cable as the output (or the microphone) in the app itself".into()),
    }
}

/// A channel menu's request, with the devices as they are now.
pub fn handle(op: DevOp, cfg: &mut Config) -> Result<String, String> {
    let s = snapshot()?;
    with_held(|h| apply(op, cfg, &s, h))
}

/// What each channel is connected to, in a few words, and whether it has
/// its cable: for the page's channel buttons.
pub fn views_of(cfg: &Config, s: &Snapshot, cables: &Cables) -> Vec<ChanView> {
    (0..NUM_CHANNELS)
        .map(|ch| {
            let (devs, def) = side(s, ch);
            let cable = cables[ch].as_deref();
            let mut parts = connected(cfg, ch, devs, def, cable);
            parts.extend(cable);
            let dev = if parts.is_empty() { "Not connected".to_string() } else { parts.join(" · ") };
            ChanView { dev, cable: cable.is_some() }
        })
        .collect()
}

pub fn views(cfg: &Config) -> Vec<ChanView> {
    match snapshot() {
        Ok(s) => {
            let c = current(cfg, &s.caps, &s.rens);
            views_of(cfg, &s, &c)
        }
        Err(_) => vec![ChanView { dev: "Not connected".into(), cable: false }; NUM_CHANNELS],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names.iter().map(|n| DevInfo { name: n.to_string(), id: format!("uid-{n}"), virt: route::is_cable(n) }).collect()
    }

    fn snap() -> Snapshot {
        Snapshot {
            caps: devs(&["MacBook Pro Microphone", "BlackHole 2ch", "BlackHole 16ch", "USB Mic"]),
            rens: devs(&["MacBook Pro Speakers", "BlackHole 2ch", "BlackHole 16ch", "External Headphones"]),
            def_cap: Some("uid-MacBook Pro Microphone".into()),
            def_ren: Some("uid-MacBook Pro Speakers".into()),
        }
    }

    const NONE: Cables = [const { None }; NUM_CHANNELS];

    fn names(c: &Cables) -> Vec<(usize, &str)> {
        c.iter().enumerate().filter_map(|(ch, n)| n.as_deref().map(|n| (ch, n))).collect()
    }

    #[test]
    fn strips_first_then_buses_one_cable_each() {
        let s = snap();
        let cfg = Config::default(); // every strip, B1 and B2
        let c = plan(&cfg, &s.caps, &s.rens, &NONE);
        assert_eq!(names(&c), [(0, "BlackHole 2ch"), (1, "BlackHole 16ch")]);
        // without cables: none
        assert_eq!(names(&plan(&cfg, &s.caps[..1], &s.rens[..1], &NONE)), []);
    }

    #[test]
    fn a_channel_keeps_its_cable() {
        let s = snap();
        let mut cfg = Config::default();
        let c = plan(&cfg, &s.caps, &s.rens, &NONE);
        let held = to_hold(&cfg, &c, &NONE, &cables::installed(&s.caps, &s.rens));
        // strip 1's cable off: strip 2 keeps BlackHole 16ch, strip 3 gets
        // the free BlackHole 2ch (it doesn't move strip 2's apps)
        cfg.strips[0].cable = false;
        let c = plan(&cfg, &s.caps, &s.rens, &held);
        assert_eq!(names(&c), [(1, "BlackHole 16ch"), (2, "BlackHole 2ch")]);
        let held = to_hold(&cfg, &c, &held, &cables::installed(&s.caps, &s.rens));
        // and after a restart (the file read back) it stays so
        let back = parse_held(&held_text(&held));
        assert_eq!(back, held);
        assert_eq!(plan(&cfg, &s.caps, &s.rens, &back), c);
        // switching strip 1 on again takes nobody's cable
        cfg.strips[0].cable = true;
        assert_eq!(names(&plan(&cfg, &s.caps, &s.rens, &back)), [(1, "BlackHole 16ch"), (2, "BlackHole 2ch")]);
    }

    #[test]
    fn a_cable_gone_for_a_moment_comes_back_to_its_channel() {
        let s = snap();
        let cfg = Config::default();
        let mut held = NONE;
        held[1] = Some("BlackHole 2ch".into());
        held[0] = Some("BlackHole 16ch".into());
        let c = plan(&cfg, &s.caps, &s.rens, &held);
        assert_eq!(names(&c), [(0, "BlackHole 16ch"), (1, "BlackHole 2ch")]);
        // BlackHole 2ch is gone (a driver update): strip 2 has none for now,
        // and keeps its claim
        let (caps, rens) = (devs(&["MacBook Pro Microphone", "BlackHole 16ch"]), devs(&["MacBook Pro Speakers", "BlackHole 16ch"]));
        let c = plan(&cfg, &caps, &rens, &held);
        assert_eq!(names(&c), [(0, "BlackHole 16ch")]);
        let held = to_hold(&cfg, &c, &held, &cables::installed(&caps, &rens));
        assert_eq!(held[1].as_deref(), Some("BlackHole 2ch"));
        // back: strip 2 gets it again, not strip 3
        assert_eq!(names(&plan(&cfg, &s.caps, &s.rens, &held)), [(0, "BlackHole 16ch"), (1, "BlackHole 2ch")]);
    }

    #[test]
    fn a_cable_the_channel_connects_itself_is_not_its_cable() {
        let s = snap();
        let mut cfg = Config::default();
        cfg.strips[0].connect = vec!["BlackHole 2ch".into()];
        let c = plan(&cfg, &s.caps, &s.rens, &NONE);
        assert_eq!(names(&c), [(0, "BlackHole 16ch"), (1, "BlackHole 2ch")]);
    }

    #[test]
    fn menu_list() {
        let s = snap();
        let mut cfg = Config::default();
        cfg.strips[1].connect = vec!["usb".into(), "Nothing Here".into()];
        let c = plan(&cfg, &s.caps, &s.rens, &NONE);
        let j = list_json(&cfg, &s, &c);
        assert!(j.starts_with(r#"{"cables":{"ok":true,"why":""},"pulse":false,"strip":[{"client":"MacBook Pro Microphone","label":"MacBook Pro Microphone","kind":"device","default":true},"#), "{j}");
        // strip 1: the default mic, its cable apart
        assert!(j.contains(r#"{"id":"1","cable":{"on":true,"name":"BlackHole 2ch"},"conn":["MacBook Pro Microphone"],"connect":["@default"]}"#), "{j}");
        // strip 2: the USB mic through a part of its name; the line that matches nothing is only a line
        assert!(j.contains(r#"{"id":"2","cable":{"on":true,"name":"BlackHole 16ch"},"conn":["USB Mic"],"connect":["usb","Nothing Here"]}"#), "{j}");
        assert!(j.contains(r#"{"id":"3","cable":{"on":false,"name":"Virtual cable"},"conn":[],"connect":[]}"#), "{j}");
        assert!(j.contains(r#"{"id":"B1","cable":{"on":false,"name":"Virtual microphone"},"conn":[],"connect":[]}"#), "{j}");
        assert!(j.ends_with("]}"));
        // no cables at all
        let bare = Snapshot { caps: devs(&["MacBook Pro Microphone"]), rens: devs(&["MacBook Pro Speakers"]), def_cap: None, def_ren: None };
        assert!(list_json(&cfg, &bare, &NONE).starts_with(&format!(r#"{{"cables":{{"ok":false,"why":"{}"}}"#, cables::MISSING)));
    }

    #[test]
    fn channel_buttons() {
        let s = snap();
        let mut cfg = Config::default();
        cfg.buses[1].connect = vec!["External Headphones".into()];
        let c = plan(&cfg, &s.caps, &s.rens, &NONE);
        let v = views_of(&cfg, &s, &c);
        assert_eq!(v[0], ChanView { dev: "MacBook Pro Microphone · BlackHole 2ch".into(), cable: true });
        assert_eq!(v[1], ChanView { dev: "BlackHole 16ch".into(), cable: true });
        assert_eq!(v[2], ChanView { dev: "Not connected".into(), cable: false });
        assert_eq!(v[NUM_STRIPS], ChanView { dev: "MacBook Pro Speakers".into(), cable: false });
        assert_eq!(v[NUM_STRIPS + 1].dev, "External Headphones");
    }

    #[test]
    fn menu_changes() {
        let s = snap();
        let mut held = NONE;
        let mut cfg = Config::default();
        // a device on and off; the @default line that brings it in goes too
        assert_eq!(apply(DevOp::Client { ch: 0, client: "USB Mic".into(), on: true }, &mut cfg, &s, &mut held).unwrap(), "strip 1 takes in USB Mic");
        assert_eq!(cfg.strips[0].connect, ["@default", "USB Mic"]);
        apply(DevOp::Client { ch: 0, client: "usb mic".into(), on: true }, &mut cfg, &s, &mut held).unwrap();
        assert_eq!(cfg.strips[0].connect.len(), 2, "already in");
        assert_eq!(apply(DevOp::Client { ch: 0, client: "MacBook Pro Microphone".into(), on: false }, &mut cfg, &s, &mut held).unwrap(), "strip 1 and MacBook Pro Microphone disconnected");
        assert_eq!(cfg.strips[0].connect, ["USB Mic"]);
        assert_eq!(apply(DevOp::Client { ch: NUM_STRIPS, client: "External Headphones".into(), on: true }, &mut cfg, &s, &mut held).unwrap(), "A1 plays on External Headphones");
        // the default
        assert_eq!(apply(DevOp::Default { ch: 0, on: true }, &mut cfg, &s, &mut held).unwrap(), "strip 1 follows the macOS default input");
        assert_eq!(apply(DevOp::Default { ch: 0, on: false }, &mut cfg, &s, &mut held).unwrap(), "strip 1 no longer follows the macOS default input");
        assert_eq!(cfg.strips[0].connect, ["USB Mic"]);
        // a config line
        assert!(apply(DevOp::Unpattern { ch: 0, pattern: "nope".into() }, &mut cfg, &s, &mut held).is_err());
        assert!(apply(DevOp::Unpattern { ch: 0, pattern: "USB Mic".into() }, &mut cfg, &s, &mut held).is_ok());
        assert_eq!(apply(DevOp::Nothing { ch: NUM_STRIPS }, &mut cfg, &s, &mut held).unwrap(), "A1 is not connected to anything");
        assert!(cfg.buses[0].connect.is_empty());
        assert!(apply(DevOp::Stream { rec: false, id: 1, ch: Some(0) }, &mut cfg, &s, &mut held).is_err());
        assert!(apply(DevOp::Nothing { ch: 11 }, &mut cfg, &s, &mut held).is_err());
    }

    #[test]
    fn switching_cables() {
        let s = snap();
        let mut held = NONE;
        let mut cfg = Config::default();
        for st in &mut cfg.strips {
            st.cable = false;
        }
        cfg.buses[3].cable = false;
        cfg.buses[4].cable = false;
        // B1 on: the first free cable
        let m = apply(DevOp::Cable { ch: NUM_STRIPS + 3, on: true }, &mut cfg, &s, &mut held).unwrap();
        assert!(m.starts_with("\"BlackHole 2ch\" is on: choose it as the microphone in an app to record B1"), "{m}");
        assert!(apply(DevOp::Cable { ch: 2, on: true }, &mut cfg, &s, &mut held).unwrap().starts_with("\"BlackHole 16ch\" is on: choose it as an app's output to play into strip 3"));
        // a third: none is free, nothing changes
        let e = apply(DevOp::Cable { ch: 0, on: true }, &mut cfg, &s, &mut held).unwrap_err();
        assert!(e.starts_with("All 2 installed virtual cable(s) are in use"), "{e}");
        assert!(!cfg.strips[0].cable);
        assert_eq!(apply(DevOp::Cable { ch: 2, on: false }, &mut cfg, &s, &mut held).unwrap(), "strip 3's virtual cable is off");
        // no cables installed at all
        let bare = Snapshot { caps: devs(&["MacBook Pro Microphone"]), rens: devs(&["MacBook Pro Speakers"]), def_cap: None, def_ren: None };
        assert_eq!(apply(DevOp::Cable { ch: 0, on: true }, &mut cfg, &bare, &mut held).unwrap_err(), cables::MISSING);
    }
}
