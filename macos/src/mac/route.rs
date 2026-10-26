//! Resolving `connect =` entries to Core Audio devices (pure logic, no
//! macOS calls, so it is unit-tested on every platform).
//!
//! On macOS a `connect` value is `@default` (the device chosen in System
//! Settings › Sound), `@physical` (the default device, skipping software
//! devices so the mixer never feeds itself) or a case-insensitive part of a
//! device's name as macOS shows it, e.g. `BlackHole 2ch` or `USB`.
//!
//! macOS has one device per piece of hardware, often with inputs and
//! outputs, where Windows has an endpoint per direction. So a virtual cable
//! is one device too: what is played into BlackHole comes out of its input.
//! BananaSplit gives each channel whose cable is on a cable device of its
//! own: a strip records it (apps play into it), a bus plays into it (apps
//! record it as their microphone).

use bananasplit::config::{DEFAULT_DEVICE, PHYSICAL};
use bananasplit::engine::{BUS_IDS, NUM_STRIPS};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevInfo {
    /// The name macOS shows, e.g. "MacBook Pro Microphone".
    pub name: String,
    /// The device's UID: the same after a restart or a replug.
    pub id: String,
    /// A software device (transport type "virtual"): BlackHole, Loopback,
    /// Zoom's or Microsoft Teams' audio device, ...
    pub virt: bool,
}

/// Where BlackHole, the free virtual cable most people use on macOS, is
/// published (with its installer and Homebrew instructions).
pub const BLACKHOLE_PAGE: &str = "https://github.com/ExistentialAudio/BlackHole";

/// A virtual cable BananaSplit can give a channel: a loopback device whose
/// input carries what is played into it. BlackHole (2ch, 16ch, 64ch, or a
/// build with a name of its own that starts with "BlackHole"), VB-Audio's
/// VB-Cable, Rogue Amoeba's Loopback (its devices are called "Loopback
/// Audio" until renamed) and the old Soundflower.
pub fn is_cable(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.starts_with("blackhole") || n.starts_with("vb-cable") || n.starts_with("loopback audio") || n.starts_with("soundflower")
}

/// A pattern that names a virtual cable: a missing cable is reported as
/// "not installed" rather than as an error (most people only have some).
pub fn names_cable(pattern: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    ["blackhole", "vb-cable", "loopback audio", "soundflower"].iter().any(|c| p.contains(c))
}

/// Software devices and cables. Never picked for `@physical`: if the
/// system's default output is BlackHole (so all desktop audio goes through
/// the mixer), A1 must still go to real speakers.
pub fn is_virtual(d: &DevInfo) -> bool {
    d.virt || is_cable(&d.name)
}

/// The system's default device, when it is a virtual cable: `@default` does
/// not use it. BananaSplit records the cables and plays into them, so
/// following such a default would feed the mixer into itself. That happens
/// with two everyday setups: the default output set to BlackHole (all
/// desktop sound through the mixer: A1 would play into the cable a strip
/// records), and the default input set to BlackHole (the virtual mic for
/// every app: the Mic strip would record what B1 plays into it).
pub fn default_cable<'a>(devs: &'a [DevInfo], default_id: Option<&str>) -> Option<&'a DevInfo> {
    default_id.and_then(|id| devs.iter().find(|d| d.id == id)).filter(|d| is_cable(&d.name))
}

/// Find the device for one `connect` entry.
pub fn resolve<'a>(pattern: &str, devs: &'a [DevInfo], default_id: Option<&str>) -> Option<&'a DevInfo> {
    if pattern == DEFAULT_DEVICE {
        // Whatever macOS uses by default, unless that is a virtual cable
        // (see `default_cable`): then the real device `@physical` picks.
        if default_cable(devs, default_id).is_some() {
            return resolve(PHYSICAL, devs, default_id);
        }
        return default_id.and_then(|id| devs.iter().find(|d| d.id == id)).or_else(|| devs.iter().find(|d| !is_cable(&d.name)));
    }
    if pattern == PHYSICAL {
        let default = default_id.and_then(|id| devs.iter().find(|d| d.id == id));
        return match default {
            Some(d) if !is_virtual(d) => Some(d),
            _ => devs.iter().find(|d| !is_virtual(d)),
        };
    }
    let p = pattern.trim().to_lowercase();
    if p.is_empty() {
        return None;
    }
    let lower: Vec<String> = devs.iter().map(|d| d.name.to_lowercase()).collect();
    // exact name, then prefix, then anywhere in the name
    if let Some(i) = lower.iter().position(|n| *n == p) {
        return Some(&devs[i]);
    }
    if let Some(i) = lower.iter().position(|n| n.starts_with(&p)) {
        return Some(&devs[i]);
    }
    lower.iter().position(|n| n.contains(&p)).map(|i| &devs[i])
}

/// A cable that a bus plays into and a strip records feeds the mixer into
/// itself as soon as that strip goes to that bus: a warning for each such
/// pair. `strip_devs[s]` are the indices in `caps` that strip `s` records,
/// `bus_devs[b]` the ones in `rens` that bus `b` plays on.
pub fn cable_feedback(caps: &[DevInfo], rens: &[DevInfo], strip_devs: &[Vec<usize>], bus_devs: &[Vec<usize>]) -> Vec<String> {
    let mut warnings = Vec::new();
    for (b, played) in bus_devs.iter().enumerate() {
        for &i in played {
            let cable = &rens[i];
            if !is_cable(&cable.name) {
                continue;
            }
            for (s, recorded) in strip_devs.iter().enumerate().take(NUM_STRIPS) {
                if recorded.iter().any(|&k| caps[k].id == cable.id) {
                    warnings.push(format!(
                        "bus {bus} plays into {} and strip {strip} records that cable: if strip {strip} goes to {bus} (directly or through a send), it feeds back",
                        cable.name,
                        bus = BUS_IDS[b],
                        strip = s + 1,
                    ));
                }
            }
        }
    }
    warnings
}

/// Names in the order people expect: digits compared as numbers, so
/// "BlackHole 2ch" comes before "BlackHole 16ch"; otherwise without regard
/// to case.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
        let mut s = String::new();
        while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
            s.push(c);
            it.next();
        }
        s
    }
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (m, n) = (digits(&mut x), digits(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                match m.len().cmp(&n.len()).then_with(|| m.cmp(n)) {
                    Ordering::Equal => {}
                    o => return o,
                }
            }
            (Some(c), Some(d)) => {
                match c.to_lowercase().cmp(d.to_lowercase()) {
                    Ordering::Equal => {}
                    o => return o,
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| DevInfo { name: n.to_string(), id: format!("uid{i}"), virt: is_cable(n) || n.contains("Zoom") })
            .collect()
    }

    #[test]
    fn default_never_follows_macos_into_a_cable() {
        let outs = devs(&["BlackHole 2ch", "MacBook Pro Speakers", "External Headphones"]);
        // default output BlackHole (desktop sound through the mixer): A1
        // plays on the speakers, not into the cable a strip records
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("uid0")).unwrap().id, "uid1");
        assert_eq!(default_cable(&outs, Some("uid0")).unwrap().id, "uid0");
        // a real default is followed as it is
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("uid2")).unwrap().id, "uid2");
        assert!(default_cable(&outs, Some("uid2")).is_none());
        // no default known: not a cable either
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, None).unwrap().id, "uid1");
        // default input BlackHole (the virtual mic for every app): the Mic
        // strip records the real microphone
        let ins = devs(&["BlackHole 2ch", "MacBook Pro Microphone"]);
        assert_eq!(resolve(DEFAULT_DEVICE, &ins, Some("uid0")).unwrap().id, "uid1");
        // only cables: nothing, rather than a loop
        assert!(resolve(DEFAULT_DEVICE, &devs(&["BlackHole 2ch"]), Some("uid0")).is_none());
    }

    #[test]
    fn physical_skips_software_devices() {
        let d = devs(&["ZoomAudioDevice", "BlackHole 16ch", "MacBook Air Speakers", "USB Audio CODEC"]);
        // the default is Zoom's device -> the first real one instead
        assert_eq!(resolve(PHYSICAL, &d, Some("uid0")).unwrap().id, "uid2");
        // default is a real device -> use it
        assert_eq!(resolve(PHYSICAL, &d, Some("uid3")).unwrap().id, "uid3");
        assert_eq!(resolve(PHYSICAL, &d, None).unwrap().id, "uid2");
        assert!(resolve(PHYSICAL, &devs(&["BlackHole 2ch", "ZoomAudioDevice"]), None).is_none());
        // @default follows a software device that is not a cable (Zoom's
        // device as the default output stays the user's choice)
        assert_eq!(resolve(DEFAULT_DEVICE, &d, Some("uid0")).unwrap().id, "uid0");
    }

    #[test]
    fn exact_beats_prefix_beats_part() {
        let d = devs(&["BlackHole 16ch", "BlackHole 2ch", "My Mic Pro", "Mic", "USB Mic"]);
        assert_eq!(resolve("blackhole 2ch", &d, None).unwrap().id, "uid1");
        assert_eq!(resolve("BlackHole", &d, None).unwrap().id, "uid0");
        assert_eq!(resolve("mic", &d, None).unwrap().id, "uid3");
        assert_eq!(resolve("my mic", &d, None).unwrap().id, "uid2");
        assert_eq!(resolve("usb", &d, None).unwrap().id, "uid4");
        assert!(resolve("BlackHole 64ch", &d, None).is_none());
        assert!(resolve("  ", &d, None).is_none());
    }

    #[test]
    fn cables_are_recognised() {
        for n in ["BlackHole 2ch", "BlackHole 16ch", "blackhole 64ch", "BlackHole Discord", "VB-Cable", "Loopback Audio", "Loopback Audio 2", "Soundflower (2ch)"] {
            assert!(is_cable(n), "{n}");
        }
        for n in ["MacBook Pro Speakers", "ZoomAudioDevice", "Microsoft Teams Audio", "Aggregate Device", "My Loopback", "Black Hole"] {
            assert!(!is_cable(n), "{n}");
        }
        assert!(names_cable("BlackHole 16ch") && names_cable("vb-cable") && !names_cable("USB"));
        let zoom = DevInfo { name: "ZoomAudioDevice".into(), id: "z".into(), virt: true };
        assert!(is_virtual(&zoom) && !is_cable(&zoom.name));
    }

    #[test]
    fn feedback_through_a_cable_is_found() {
        let caps = devs(&["MacBook Pro Microphone", "BlackHole 2ch", "BlackHole 16ch"]);
        let rens = devs(&["MacBook Pro Speakers", "BlackHole 2ch", "BlackHole 16ch"]);
        let (mut strips, mut buses) = (vec![vec![]; 6], vec![vec![]; 5]);
        // strip 2 records BlackHole 2ch, B1 plays into BlackHole 16ch: fine
        strips[0] = vec![0];
        strips[1] = vec![1];
        buses[0] = vec![0];
        buses[3] = vec![2];
        assert!(cable_feedback(&caps, &rens, &strips, &buses).is_empty());
        // A1 plays into BlackHole 2ch as well
        buses[0] = vec![0, 1];
        assert_eq!(
            cable_feedback(&caps, &rens, &strips, &buses),
            ["bus A1 plays into BlackHole 2ch and strip 2 records that cable: if strip 2 goes to A1 (directly or through a send), it feeds back"]
        );
        // a real device on both sides is no cable
        buses[0] = vec![0];
        strips[2] = vec![0];
        assert!(cable_feedback(&caps, &rens, &strips, &buses).is_empty());
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["BlackHole 64ch", "blackhole 16ch", "BlackHole 2ch", "Loopback Audio 10", "Loopback Audio 2", "Loopback Audio"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["BlackHole 2ch", "blackhole 16ch", "BlackHole 64ch", "Loopback Audio", "Loopback Audio 2", "Loopback Audio 10"]);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Less, "equal numbers: by the text, so the order is total");
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
    }
}
