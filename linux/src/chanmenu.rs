//! What each channel is connected to, for the app's channel menus.
//!
//! Every strip and every bus has a menu. A strip's lists what it can take
//! in: sound cards and microphones (JACK connections), the apps that play
//! (moved into the strip's virtual cable, like choosing the app's output in
//! the sound settings), other JACK programs, what an output device plays,
//! the desktop's default input, and the strip's own virtual cable. A bus's
//! lists where it can play: output devices, the apps that record (moved to
//! the bus's virtual microphone), other JACK programs, the default output,
//! and its own virtual microphone. So any channel can be a hardware one, a
//! virtual one, or both.
//!
//! The menus work on the live JACK graph and the config together: what you
//! tick is connected at once and becomes a `connect =` line (or a `cable =`
//! switch), so it comes back after a restart and when a device is plugged
//! in again; what you untick is disconnected and its line removed.

use crate::backend::Backend;
use crate::cables::{CableSet, Cables, NUM_CHANNELS, pactl};
use crate::devices::{Candidate, PulseInfo, Side, client_score};
use crate::gui::{ChanView, DevOp};
use crate::jack::regex_escape;
use crate::sys;
use bananasplit::config::{Config, DEFAULT_DEVICE};
use bananasplit::engine::NUM_STRIPS;
use bananasplit::recorder::source_id;
use std::fmt::Write as _;

fn side(ch: usize) -> Side {
    if ch < NUM_STRIPS { Side::Strip } else { Side::Bus }
}

fn connect_list(cfg: &mut Config, ch: usize) -> &mut Vec<String> {
    if ch < NUM_STRIPS { &mut cfg.strips[ch].connect } else { &mut cfg.buses[ch - NUM_STRIPS].connect }
}

fn set_cable(cfg: &mut Config, ch: usize, on: bool) {
    if ch < NUM_STRIPS {
        cfg.strips[ch].cable = on;
    } else {
        cfg.buses[ch - NUM_STRIPS].cable = on;
    }
}

/// Is JACK client `client` (one of channel `ch`'s connections) the
/// channel's own virtual cable?
fn is_own_cable(cables: &Cables, ch: usize, client: &str) -> bool {
    cables.live(ch).is_some_and(|c| sys::regex_match(&c.jack_regex(), &format!("{client}:")) == Some(true))
}

/// The JACK clients connected to each channel, without its own cable.
pub fn connections(backend: &Backend, cables: &Cables) -> Vec<Vec<String>> {
    (0..NUM_CHANNELS)
        .map(|ch| backend.connected_clients(ch).into_iter().filter(|c| !is_own_cable(cables, ch, c)).collect())
        .collect()
}

/// A shorter name for a JACK client, from what PipeWire knows about it.
fn short_label(info: &PulseInfo, client: &str) -> String {
    let nick = info.devices.as_ref().and_then(|d| {
        d.list
            .iter()
            .filter(|x| !x.is_monitor && client_score(x, client) >= 2)
            .max_by_key(|x| client_score(x, client))
            .and_then(|x| x.nick.clone())
    });
    match nick {
        Some(n) if n.len() < client.len() => n,
        _ => client.to_string(),
    }
}

/// The channel whose virtual cable is the sink (`rec` false) or source with
/// index `target`.
fn channel_of(info: &PulseInfo, cables: &Cables, target: u32, rec: bool) -> Option<usize> {
    let node = info.node_name(target, rec)?;
    let range = if rec { NUM_STRIPS..NUM_CHANNELS } else { 0..NUM_STRIPS };
    range.into_iter().find(|&ch| cables.live(ch).is_some_and(|c| c.node_name == node))
}

/// What the page shows on each channel's menu button.
pub fn views(cables: &Cables, info: &PulseInfo, conns: &[Vec<String>]) -> Vec<ChanView> {
    (0..NUM_CHANNELS)
        .map(|ch| {
            let rec = ch >= NUM_STRIPS;
            let mut parts: Vec<String> = conns.get(ch).into_iter().flatten().map(|c| short_label(info, c)).collect();
            let streams = if rec { &info.recording } else { &info.playing };
            for s in streams {
                if channel_of(info, cables, s.target, rec) == Some(ch) && !parts.contains(&s.app) {
                    parts.push(s.app.clone());
                }
            }
            let cable = cables.live(ch).is_some();
            let dev = if !parts.is_empty() {
                parts.join(" · ")
            } else if cable {
                if rec { "Virtual mic".into() } else { "Virtual cable".into() }
            } else {
                "Not connected".into()
            };
            ChanView { dev, cable }
        })
        .collect()
}

fn jstr(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
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

fn cands_json(o: &mut String, cs: &[Candidate]) {
    o.push('[');
    for (k, c) in cs.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str("{\"client\":");
        jstr(o, &c.client);
        o.push_str(",\"label\":");
        jstr(o, &c.label);
        let _ = write!(o, ",\"kind\":\"{}\",\"default\":{}}}", c.kind.as_str(), c.default);
    }
    o.push(']');
}

/// The menus' contents (`GET /api/devices`).
pub fn list_json(backend: &Backend, cfg: &Config, cables: &Cables, info: &PulseInfo, conns: &[Vec<String>]) -> String {
    let devs = info.devices.clone();
    let pulse = info.devices.as_ref().is_some_and(|d| d.pipewire);
    let mut o = String::with_capacity(4096);
    o.push_str("{\"cables\":");
    match cables.unavailable() {
        Some(why) => {
            o.push_str("{\"ok\":false,\"why\":");
            jstr(&mut o, why);
            o.push('}');
        }
        None => o.push_str("{\"ok\":true,\"why\":null}"),
    }
    let _ = write!(o, ",\"pulse\":{pulse},\"strip\":");
    cands_json(&mut o, &backend.candidates(cfg, cables, devs.clone(), Side::Strip));
    o.push_str(",\"bus\":");
    cands_json(&mut o, &backend.candidates(cfg, cables, devs, Side::Bus));
    for (key, rec) in [("play", false), ("rec", true)] {
        let _ = write!(o, ",\"{key}\":[");
        let list = if rec { &info.recording } else { &info.playing };
        for (k, s) in list.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            let _ = write!(o, "{{\"id\":{},\"app\":", s.id);
            jstr(&mut o, &s.app);
            o.push_str(",\"media\":");
            jstr(&mut o, &s.media);
            o.push_str(",\"on\":");
            jstr(&mut o, info.description(s.target, rec).unwrap_or(""));
            let ch = channel_of(info, cables, s.target, rec).map_or(-1, |c| c as i64);
            let _ = write!(o, ",\"ch\":{ch}}}");
        }
        o.push(']');
    }
    o.push_str(",\"ch\":[");
    let set = CableSet::for_config(cfg);
    for ch in 0..NUM_CHANNELS {
        if ch > 0 {
            o.push(',');
        }
        let live = cables.live(ch);
        let name = live.or(set.spec(ch)).map_or(String::new(), |c| c.description.clone());
        let _ = write!(o, "{{\"id\":\"{}\",\"cable\":{{\"on\":{},\"name\":", source_id(ch), live.is_some());
        jstr(&mut o, &name);
        o.push_str("},\"conn\":[");
        for (k, c) in conns.get(ch).into_iter().flatten().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, c);
        }
        o.push_str("],\"connect\":[");
        let list = if ch < NUM_STRIPS { &cfg.strips[ch].connect } else { &cfg.buses[ch - NUM_STRIPS].connect };
        for (k, p) in list.iter().enumerate() {
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

/// The JACK clients a `connect` value reaches on channel `ch` now.
fn reached(backend: &Backend, cfg: &Config, cables: &Cables, info: &PulseInfo, ch: usize, pattern: &str, among: &[String]) -> Vec<String> {
    among
        .iter()
        .filter(|c| backend.pattern_reaches(cfg, cables, info.devices.clone(), ch, pattern, c))
        .cloned()
        .collect()
}

/// Switch channel `ch`'s virtual cable on (if it has none).
fn cable_on(cfg: &mut Config, cables: &mut Cables, ch: usize) -> Result<String, String> {
    let mut spec = CableSet::for_config(cfg).spec(ch).cloned().ok_or("bad channel")?;
    spec.enabled = true;
    cables.add(ch, &spec)?;
    set_cable(cfg, ch, true);
    Ok(spec.description)
}

/// Carry out one menu action. `cables` and `cfg` change with it; the
/// caller saves nothing (the config is saved like every other control).
pub fn handle(op: DevOp, backend: &Backend, cfg: &mut Config, cables: &mut Cables, info: &PulseInfo) -> Result<String, String> {
    let check = |ch: usize| if ch < NUM_CHANNELS { Ok(ch) } else { Err("bad channel".to_string()) };
    // "strip 3", "A1"
    let id = |ch: usize| if ch < NUM_STRIPS { format!("strip {}", ch + 1) } else { source_id(ch).to_string() };
    match op {
        DevOp::List => {
            let conns = connections(backend, cables);
            Ok(list_json(backend, cfg, cables, info, &conns))
        }
        DevOp::Client { ch, client, on } => {
            let ch = check(ch)?;
            let pat = format!("^{}:", regex_escape(&client));
            if on {
                backend.connect_client(ch, &client)?;
                let list = connect_list(cfg, ch);
                if !list.contains(&pat) {
                    list.push(pat);
                }
                Ok(if ch < NUM_STRIPS { format!("{} takes in {client}", id(ch)) } else { format!("{} plays on {client}", id(ch)) })
            } else {
                // every line that brings this client in goes, so it stays away
                let list = connect_list(cfg, ch).clone();
                let keep: Vec<String> = list
                    .iter()
                    .filter(|p| **p != pat && reached(backend, cfg, cables, info, ch, p, std::slice::from_ref(&client)).is_empty())
                    .cloned()
                    .collect();
                *connect_list(cfg, ch) = keep;
                backend.disconnect_client(ch, &client)?;
                Ok(format!("{} and {client} disconnected", id(ch)))
            }
        }
        DevOp::Default { ch, on } => {
            let ch = check(ch)?;
            let what = if side(ch) == Side::Strip { "default input" } else { "default output" };
            if on {
                let list = connect_list(cfg, ch);
                if !list.iter().any(|p| p == DEFAULT_DEVICE) {
                    list.push(DEFAULT_DEVICE.into());
                }
                let w = backend.autoconnect(cfg, cables);
                Ok(match w.errs.into_iter().find(|e| e.contains(DEFAULT_DEVICE)) {
                    Some(e) => e,
                    None => format!("{} follows the desktop's {what}", id(ch)),
                })
            } else {
                let all: Vec<String> = backend.connected_clients(ch);
                let was = reached(backend, cfg, cables, info, ch, DEFAULT_DEVICE, &all);
                connect_list(cfg, ch).retain(|p| p != DEFAULT_DEVICE);
                unhook(backend, cfg, cables, info, ch, &was)?;
                Ok(format!("{} no longer follows the desktop's {what}", id(ch)))
            }
        }
        DevOp::Unpattern { ch, pattern } => {
            let ch = check(ch)?;
            let all: Vec<String> = backend.connected_clients(ch);
            let was = reached(backend, cfg, cables, info, ch, &pattern, &all);
            let list = connect_list(cfg, ch);
            let before = list.len();
            list.retain(|p| *p != pattern);
            if list.len() == before {
                return Err(format!("{} has no line \"connect = {pattern}\"", id(ch)));
            }
            unhook(backend, cfg, cables, info, ch, &was)?;
            Ok(format!("removed \"connect = {pattern}\" from {}", id(ch)))
        }
        DevOp::Nothing { ch } => {
            let ch = check(ch)?;
            connect_list(cfg, ch).clear();
            for c in backend.connected_clients(ch) {
                if !is_own_cable(cables, ch, &c) {
                    backend.disconnect_client(ch, &c)?;
                }
            }
            Ok(format!("{} is not connected to anything{}", id(ch), if cables.live(ch).is_some() { " but its virtual cable" } else { "" }))
        }
        DevOp::Cable { ch, on } => {
            let ch = check(ch)?;
            if on {
                let name = cable_on(cfg, cables, ch)?;
                Ok(if ch < NUM_STRIPS {
                    format!("\"{name}\" is on: choose it as an app's output to play into {}", id(ch))
                } else {
                    format!("\"{name}\" is on: choose it as the microphone in an app to record {}", id(ch))
                })
            } else {
                cables.remove(ch)?;
                set_cable(cfg, ch, false);
                Ok(format!("{}'s virtual cable is off (its apps went back to the default device)", id(ch)))
            }
        }
        DevOp::Stream { rec, id: sid, ch } => {
            let list = if rec { &info.recording } else { &info.playing };
            let app = list.iter().find(|s| s.id == sid).map_or_else(|| format!("stream {sid}"), |s| s.app.clone());
            let (cmd, what) = if rec { ("move-source-output", "records") } else { ("move-sink-input", "plays into") };
            match ch {
                Some(ch) => {
                    let ch = check(ch)?;
                    if rec != (ch >= NUM_STRIPS) {
                        return Err(if rec { "a recording app takes a bus's mix".into() } else { "a playing app goes into a strip".into() });
                    }
                    if cables.live(ch).is_none() {
                        cable_on(cfg, cables, ch)?;
                    }
                    let node = cables.live(ch).map(|c| c.node_name.clone()).ok_or("the virtual cable is not there")?;
                    pactl(&[cmd, &sid.to_string(), &node])?;
                    Ok(format!("{app} now {what} {}", id(ch)))
                }
                None => {
                    let d = info.devices.as_ref().ok_or("pactl is not available")?;
                    let set = CableSet::for_config(cfg).with_live(cables);
                    let ours = |name: &str| d.list.iter().any(|x| x.name == name && set.owns(&x.name, &x.description));
                    let default = if rec { d.default_source.as_deref() } else { d.default_sink.as_deref() };
                    let target = default
                        .filter(|n| !ours(n))
                        .map(str::to_string)
                        .or_else(|| {
                            d.list.iter().find(|x| x.is_source == rec && !x.is_monitor && !set.owns(&x.name, &x.description)).map(|x| x.name.clone())
                        })
                        .ok_or("there is no other device to move it to")?;
                    pactl(&[cmd, &sid.to_string(), &target])?;
                    let desc = d.list.iter().find(|x| x.name == target).map_or(target.clone(), |x| x.description.clone());
                    Ok(format!("{app} now {} {desc}", if rec { "records" } else { "plays on" }))
                }
            }
        }
    }
}

/// Disconnect the clients in `was` that no remaining `connect` line of
/// channel `ch` brings in.
fn unhook(backend: &Backend, cfg: &Config, cables: &Cables, info: &PulseInfo, ch: usize, was: &[String]) -> Result<(), String> {
    let list = if ch < NUM_STRIPS { &cfg.strips[ch].connect } else { &cfg.buses[ch - NUM_STRIPS].connect };
    for c in was {
        let still = list.iter().any(|p| !reached(backend, cfg, cables, info, ch, p, std::slice::from_ref(c)).is_empty());
        if !still {
            backend.disconnect_client(ch, c)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Device, Devices, Stream};

    fn info() -> PulseInfo {
        let dev = |index, name: &str, desc: &str, nick: Option<&str>, is_source| Device {
            index,
            name: name.into(),
            description: desc.into(),
            nick: nick.map(Into::into),
            is_source,
            is_monitor: false,
        };
        PulseInfo {
            devices: Some(Devices {
                pipewire: true,
                default_source: Some("alsa_input.yeti".into()),
                default_sink: Some("alsa_output.speakers".into()),
                list: vec![
                    dev(50, "alsa_output.speakers", "Speakers Analog Stereo", Some("Speakers"), false),
                    dev(51, "bananasplit_in2", "BananaSplit-In2-Discord", None, false),
                    dev(52, "alsa_input.yeti", "Yeti Stereo Microphone Analog Stereo", Some("Yeti Stereo Microphone"), true),
                    dev(53, "bananasplit_out_b1", "BananaSplit-Out-B1", None, true),
                ],
            }),
            playing: vec![
                Stream { id: 7, app: "Discord".into(), media: "Playback".into(), binary: String::new(), target: 51 },
                Stream { id: 8, app: "Firefox".into(), media: "YouTube".into(), binary: String::new(), target: 50 },
            ],
            recording: vec![Stream { id: 9, app: "OBS".into(), media: String::new(), binary: String::new(), target: 53 }],
        }
    }

    #[test]
    fn menu_buttons_say_what_each_channel_has() {
        let cfg = Config::default();
        let set = CableSet::for_config(&cfg);
        let mut cables = Cables::none("test");
        cables.test_live(1, set.spec(1).unwrap().clone());
        cables.test_live(9, set.spec(9).unwrap().clone());
        cables.test_live(10, set.spec(10).unwrap().clone());
        let info = info();
        let mut conns = vec![Vec::new(); NUM_CHANNELS];
        conns[0] = vec!["Yeti Stereo Microphone Analog Stereo".to_string()];
        conns[6] = vec!["Speakers Analog Stereo".to_string(), "HDMI 3".to_string()];
        let v = views(&cables, &info, &conns);
        assert_eq!(v[0].dev, "Yeti Stereo Microphone");
        assert!(!v[0].cable);
        assert_eq!(v[1].dev, "Discord", "the app that plays into strip 2's cable");
        assert!(v[1].cable);
        assert_eq!(v[2].dev, "Not connected");
        assert_eq!(v[6].dev, "Speakers · HDMI 3");
        assert_eq!(v[9].dev, "OBS", "the app that records B1's virtual mic");
        assert_eq!(v[10].dev, "Virtual mic");
        assert_eq!(channel_of(&info, &cables, 51, false), Some(1));
        assert_eq!(channel_of(&info, &cables, 50, false), None);
        assert_eq!(channel_of(&info, &cables, 53, true), Some(9));
        cables.test_clear();
    }
}
