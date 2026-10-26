//! Resolving `connect =` entries to Windows audio devices (pure logic, no
//! Windows calls, so it is unit-tested on every platform).
//!
//! On Windows a `connect` value is either `@physical` (the default device,
//! skipping virtual cables so the mixer never feeds itself) or a
//! case-insensitive fragment of a device's name as shown in the Windows
//! sound settings, e.g. `CABLE Output` or `Headset Earphone`.

use bananasplit::config::{DEFAULT_DEVICE, PHYSICAL};
use bananasplit::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevInfo {
    /// Friendly name, e.g. "CABLE Output (VB-Audio Virtual Cable)".
    pub name: String,
    /// Endpoint ID string (stable across reboots).
    pub id: String,
}

/// Virtual cables and other software devices. Never picked for `@physical`:
/// if the Windows default output is "CABLE Input" (so all desktop audio
/// goes through the mixer), A1 must still go to real speakers.
pub fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("vb-audio")
        || n.starts_with("cable")
        || n.contains("voicemeeter")
        || n.contains("virtual")
        || n.contains("hi-fi cable")
        || is_splitter(name)
}

/// True for VB-Audio cable endpoints (what the install prompt looks for).
pub fn is_vb_cable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("vb-audio") && (n.starts_with("cable") || n.contains("cable"))
}

/// The device name of BananaSplit's own cables (the Splitter driver), as
/// Windows shows it after each endpoint's name: "Splitter 3 (Splitter
/// Cables)".
pub const SPLITTER_DEVICE: &str = "Splitter Cables";

/// Where the Splitter driver is published.
pub const SPLITTER_PAGE: &str = "https://github.com/LudaLunizz/Splitter/releases";

/// The two halves of a Splitter cable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitterEnd {
    /// The playback device: apps play into a strip's ("Splitter 3"), and a
    /// bus plays into its own ("Splitter B1 Input").
    Input,
    /// The recording device: a strip records its own ("Splitter 3
    /// Output"), and apps record a bus's as their microphone ("Splitter
    /// B1").
    Output,
}

/// Endpoints of the Splitter driver (any of its cables): the device name in
/// the last parentheses. Windows numbers devices whose names clash, e.g. a
/// second Splitter device left behind by a reinstall: "Splitter 1 (2-
/// Splitter Cables)".
pub fn is_splitter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let Some(device) = n.rfind('(').and_then(|open| n[open + 1..].strip_suffix(')')) else {
        return false;
    };
    let device = match device.split_once("- ") {
        Some((num, rest)) if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) => rest,
        _ => device,
    };
    device == SPLITTER_DEVICE.to_ascii_lowercase()
}

/// The name of the half of channel `ch`'s Splitter cable that other apps
/// use: "Splitter 3" for strip 3 (channel 2), "Splitter B1" for bus B1.
/// Channels are strips 1-6, then buses A1..B2.
pub fn splitter_name(ch: usize) -> String {
    if ch < NUM_STRIPS {
        format!("Splitter {}", ch + 1)
    } else {
        format!("Splitter {}", BUS_IDS[ch - NUM_STRIPS])
    }
}

/// Which channel's Splitter cable an endpoint belongs to, and which half,
/// from its name: "Splitter 2 (Splitter Cables)" is the input of strip 2's
/// cable (channel 1), "Splitter 2 Output" its output; "Splitter B1 Input"
/// is the input of bus B1's cable, "Splitter B1" its output (the
/// microphone). An endpoint renamed in the Windows sound settings is still
/// a Splitter endpoint (`is_splitter`) but no longer says which cable it
/// is: `None`.
pub fn splitter_cable(name: &str) -> Option<(usize, SplitterEnd)> {
    if !is_splitter(name) {
        return None;
    }
    let endpoint = short_name(name).trim().to_ascii_lowercase();
    let rest = endpoint.strip_prefix("splitter ")?;
    let (id, suffix) = match rest.split_once(' ') {
        Some((id, suffix)) => (id, Some(suffix)),
        None => (rest, None),
    };
    if let Some(s) = (0..NUM_STRIPS).find(|s| id == (s + 1).to_string()) {
        // a strip's cable: apps play into "Splitter N", the strip records
        // "Splitter N Output"
        return match suffix {
            None => Some((s, SplitterEnd::Input)),
            Some("output") => Some((s, SplitterEnd::Output)),
            Some(_) => None,
        };
    }
    let b = (0..NUM_BUSES).find(|&b| id.eq_ignore_ascii_case(BUS_IDS[b]))?;
    // a bus's cable: the bus plays into "Splitter B1 Input", apps record
    // "Splitter B1"
    match suffix {
        None => Some((NUM_STRIPS + b, SplitterEnd::Output)),
        Some("input") => Some((NUM_STRIPS + b, SplitterEnd::Input)),
        Some(_) => None,
    }
}

/// The endpoint of channel `ch`'s Splitter cable that is half `end`, in
/// `devs` (the playback devices for `Input`, the recording ones for
/// `Output`).
pub fn splitter_endpoint(devs: &[DevInfo], ch: usize, end: SplitterEnd) -> Option<&DevInfo> {
    devs.iter().find(|d| splitter_cable(&d.name) == Some((ch, end)))
}

/// A Splitter cable that a bus plays into and a strip records feeds the
/// mixer into itself as soon as that strip goes to that bus: a warning for
/// each such pair. `strip_devs[s]` are the indices in `caps` that strip `s`
/// records, `bus_devs[b]` the ones in `rens` that bus `b` plays on.
pub fn splitter_feedback(caps: &[DevInfo], rens: &[DevInfo], strip_devs: &[Vec<usize>], bus_devs: &[Vec<usize>]) -> Vec<String> {
    let mut warnings = Vec::new();
    for ch in 0..NUM_STRIPS + NUM_BUSES {
        let at = |devs: &[DevInfo], end: SplitterEnd| devs.iter().position(|d| splitter_cable(&d.name) == Some((ch, end)));
        let (Some(input), Some(output)) = (at(rens, SplitterEnd::Input), at(caps, SplitterEnd::Output)) else {
            continue;
        };
        for (b, played) in bus_devs.iter().enumerate() {
            if !played.contains(&input) {
                continue;
            }
            for (s, recorded) in strip_devs.iter().enumerate() {
                if recorded.contains(&output) {
                    warnings.push(format!(
                        "bus {bus} plays into {} and strip {strip} records that cable: if strip {strip} goes to {bus} (directly or through a send), it feeds back",
                        short_name(&rens[input].name),
                        bus = BUS_IDS[b],
                        strip = s + 1,
                    ));
                }
            }
        }
    }
    warnings
}

/// Any virtual cable BananaSplit records or plays into: VB-Audio's or its
/// own (Splitter).
pub fn is_cable(name: &str) -> bool {
    is_vb_cable(name) || is_splitter(name)
}

/// A pattern that names a virtual cable. Missing cables are reported as
/// "not installed" rather than as errors (most people only have some).
pub fn names_cable(pattern: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    p.contains("cable") || p.starts_with("splitter")
}

/// The Windows default device, when it is a virtual cable (VB-Audio's or
/// Splitter): `@default` does not use it. BananaSplit records the cables
/// and plays into them, so following such a default would feed the mixer
/// into itself. That happens with two everyday setups: the default output
/// set to "CABLE Input" or "Splitter 1" (all desktop sound through the
/// mixer: A1 would play into the cable a strip records), and the default
/// input set to "CABLE Output" or "Splitter B1" (the virtual mic for every
/// app: the Mic strip would record what B1 plays into it).
pub fn default_cable<'a>(devs: &'a [DevInfo], default_id: Option<&str>) -> Option<&'a DevInfo> {
    default_id.and_then(|id| devs.iter().find(|d| d.id == id)).filter(|d| is_cable(&d.name))
}

/// Find the device for one `connect` entry.
pub fn resolve<'a>(
    pattern: &str,
    devs: &'a [DevInfo],
    default_id: Option<&str>,
) -> Option<&'a DevInfo> {
    if pattern == DEFAULT_DEVICE {
        // Whatever Windows uses by default, unless that is a virtual cable
        // (see `default_cable`): then the real device `@physical` picks.
        if default_cable(devs, default_id).is_some() {
            return resolve(PHYSICAL, devs, default_id);
        }
        return default_id
            .and_then(|id| devs.iter().find(|d| d.id == id))
            .or_else(|| devs.iter().find(|d| !is_cable(&d.name)));
    }
    if pattern == PHYSICAL {
        let default = default_id.and_then(|id| devs.iter().find(|d| d.id == id));
        return match default {
            Some(d) if !is_virtual(&d.name) => Some(d),
            _ => devs.iter().find(|d| !is_virtual(&d.name)),
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

/// Short name for the UI: "CABLE Output (VB-Audio Virtual Cable)" -> "CABLE Output".
pub fn short_name(name: &str) -> &str {
    match name.find(" (") {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| DevInfo {
                name: n.to_string(),
                id: format!("id{i}"),
            })
            .collect()
    }

    #[test]
    fn default_never_follows_windows_into_a_cable() {
        let outs = devs(&[
            "CABLE Input (VB-Audio Virtual Cable)",
            "Speakers (Realtek(R) Audio)",
            "Headphones (USB Headset)",
        ]);
        // default output "CABLE Input" (desktop sound through the mixer):
        // A1 plays on the speakers, not into the cable a strip records
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("id0")).unwrap().id, "id1");
        assert_eq!(default_cable(&outs, Some("id0")).unwrap().id, "id0");
        // a real default is followed as before
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("id2")).unwrap().id, "id2");
        assert!(default_cable(&outs, Some("id2")).is_none());
        // no default known: not a cable either
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, None).unwrap().id, "id1");
        // default input "CABLE Output" (the virtual mic for every app): the
        // Mic strip records the real microphone, not what B1 plays into it
        let ins = devs(&["CABLE Output (VB-Audio Virtual Cable)", "Microphone (Yeti Stereo Microphone)"]);
        assert_eq!(resolve(DEFAULT_DEVICE, &ins, Some("id0")).unwrap().id, "id1");
        // only cables: nothing, rather than a loop
        assert!(resolve(DEFAULT_DEVICE, &devs(&["CABLE Input (VB-Audio Virtual Cable)"]), Some("id0")).is_none());
    }

    #[test]
    fn physical_skips_virtual_default() {
        let d = devs(&[
            "CABLE Input (VB-Audio Virtual Cable)",
            "Speakers (Realtek(R) Audio)",
            "Headphones (USB Headset)",
        ]);
        // default is the cable -> pick the first real device instead
        assert_eq!(resolve(PHYSICAL, &d, Some("id0")).unwrap().id, "id1");
        // default is a real device -> use it
        assert_eq!(resolve(PHYSICAL, &d, Some("id2")).unwrap().id, "id2");
        // no default at all
        assert_eq!(resolve(PHYSICAL, &d, None).unwrap().id, "id1");
        assert!(
            resolve(
                PHYSICAL,
                &devs(&["CABLE Input (VB-Audio Virtual Cable)"]),
                Some("id0")
            )
            .is_none()
        );
    }

    #[test]
    fn cable_names_do_not_collide() {
        let d = devs(&[
            "CABLE-A Output (VB-Audio Cable A)",
            "CABLE Output (VB-Audio Virtual Cable)",
            "CABLE-B Output (VB-Audio Cable B)",
        ]);
        assert_eq!(resolve("CABLE Output", &d, None).unwrap().id, "id1");
        assert_eq!(resolve("cable-b output", &d, None).unwrap().id, "id2");
        assert_eq!(resolve("Cable A", &d, None).unwrap().id, "id0");
        assert!(resolve("CABLE-C Output", &d, None).is_none());
        assert!(resolve("", &d, None).is_none());
    }

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let d = devs(&["My Mic Pro", "Mic", "USB Mic"]);
        assert_eq!(resolve("mic", &d, None).unwrap().id, "id1");
        assert_eq!(resolve("my mic", &d, None).unwrap().id, "id0");
        assert_eq!(resolve("usb", &d, None).unwrap().id, "id2");
    }

    #[test]
    fn classification() {
        assert!(is_vb_cable("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(is_vb_cable("CABLE-C Input (VB-Audio Cable C)"));
        assert!(!is_vb_cable("Speakers (Realtek(R) Audio)"));
        assert!(is_virtual("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"));
        assert!(!is_virtual("Microphone (HD Pro Webcam C920)"));
        assert!(names_cable("CABLE-D Output"));
        assert_eq!(
            short_name("CABLE Output (VB-Audio Virtual Cable)"),
            "CABLE Output"
        );
        assert_eq!(short_name("Mic"), "Mic");
    }

    #[test]
    fn splitter_cables_belong_to_channels() {
        use SplitterEnd::{Input, Output};
        // strips: apps play into "Splitter N", the strip records its Output
        assert_eq!(splitter_cable("Splitter 1 (Splitter Cables)"), Some((0, Input)));
        assert_eq!(splitter_cable("Splitter 6 Output (Splitter Cables)"), Some((5, Output)));
        // buses: the bus plays into "Splitter B1 Input", apps record "Splitter B1"
        assert_eq!(splitter_cable("Splitter A1 Input (Splitter Cables)"), Some((6, Input)));
        assert_eq!(splitter_cable("Splitter A3 (Splitter Cables)"), Some((8, Output)));
        assert_eq!(splitter_cable("Splitter B1 (Splitter Cables)"), Some((9, Output)));
        assert_eq!(splitter_cable("Splitter B2 Input (Splitter Cables)"), Some((10, Input)));
        // case doesn't matter, the name does
        assert_eq!(splitter_cable("SPLITTER 3 output (splitter cables)"), Some((2, Output)));
        assert_eq!(splitter_cable("splitter b2 (SPLITTER CABLES)"), Some((10, Output)));
        for name in [
            "Splitter 7 (Splitter Cables)",
            "Splitter 0 Output (Splitter Cables)",
            "Splitter 01 (Splitter Cables)",
            "Splitter +1 (Splitter Cables)",
            "Splitter 1x (Splitter Cables)",
            "Splitter 1 Input (Splitter Cables)",
            "Splitter B1 Output (Splitter Cables)",
            "Splitter B3 (Splitter Cables)",
            "Splitter C1 Input (Splitter Cables)",
            "Splitter (Splitter Cables)",
            "Splitter 2 Output Output (Splitter Cables)",
        ] {
            assert_eq!(splitter_cable(name), None, "{name}");
            assert!(is_splitter(name), "{name}");
        }
        // Windows' number for a second Splitter device doesn't hide the cable
        assert_eq!(splitter_cable("Splitter 5 Output (2- Splitter Cables)"), Some((4, Output)));
        assert_eq!(splitter_cable("Splitter B1 (13- Splitter Cables)"), Some((9, Output)));
        assert!(is_cable("Splitter 1 (2- Splitter Cables)") && is_virtual("Splitter 1 (2- Splitter Cables)"));
        assert_eq!(short_name("Splitter 1 (2- Splitter Cables)"), "Splitter 1");
        for name in ["Splitter 1 (x- Splitter Cables)", "Splitter 1 (- Splitter Cables)", "Splitter 1 (2- 3- Splitter Cables)", "Splitter 1 (Splitter Cables) x", "Splitter 1 Splitter Cables)"] {
            assert!(!is_splitter(name), "{name}");
        }
        // an endpoint renamed with parentheses of its own
        assert!(is_splitter("Game (loud) (Splitter Cables)"));
        assert_eq!(splitter_cable("Game (loud) (Splitter Cables)"), None);
        // renamed in the sound settings: still a Splitter endpoint, no cable
        assert!(is_splitter("Discord out (Splitter Cables)"));
        assert_eq!(splitter_cable("Discord out (Splitter Cables)"), None);
        // someone else's device that happens to be called Splitter
        assert!(!is_splitter("Splitter 1 (USB Audio Splitter)"));
        assert_eq!(splitter_cable("Splitter B1 (USB Audio Splitter)"), None);
        // Splitter endpoints are virtual cables
        assert!(is_virtual("Splitter 2 (Splitter Cables)"));
        assert!(is_virtual("Splitter B2 (Splitter Cables)"));
        assert!(is_cable("Splitter 2 Output (Splitter Cables)"));
        assert!(!is_vb_cable("Splitter 2 Output (Splitter Cables)"));
        assert!(names_cable("Splitter 4"));
        assert!(names_cable("splitter b1"));
        assert_eq!(short_name("Splitter B1 Input (Splitter Cables)"), "Splitter B1 Input");
        // every channel's name parses back to that channel's output half
        // (strips) or names the microphone half (buses)
        for ch in 0..NUM_STRIPS + NUM_BUSES {
            let n = splitter_name(ch);
            let input = if ch < NUM_STRIPS { n.clone() } else { format!("{n} Input") };
            let output = if ch < NUM_STRIPS { format!("{n} Output") } else { n.clone() };
            assert_eq!(splitter_cable(&format!("{input} ({SPLITTER_DEVICE})")), Some((ch, Input)), "{input}");
            assert_eq!(splitter_cable(&format!("{output} ({SPLITTER_DEVICE})")), Some((ch, Output)), "{output}");
        }
        assert_eq!(splitter_name(2), "Splitter 3");
        assert_eq!(splitter_name(NUM_STRIPS + 3), "Splitter B1");
    }

    #[test]
    fn splitter_endpoints_are_found_by_channel() {
        use SplitterEnd::{Input, Output};
        let caps = devs(&[
            "Microphone (USB Mic)",
            "Splitter 1 Output (Splitter Cables)",
            "Splitter B1 (Splitter Cables)",
        ]);
        let rens = devs(&["Splitter 1 (Splitter Cables)", "Splitter B1 Input (Splitter Cables)"]);
        assert_eq!(splitter_endpoint(&caps, 0, Output).unwrap().id, "id1");
        assert_eq!(splitter_endpoint(&rens, 0, Input).unwrap().id, "id0");
        assert_eq!(splitter_endpoint(&caps, 9, Output).unwrap().id, "id2");
        assert_eq!(splitter_endpoint(&rens, 9, Input).unwrap().id, "id1");
        // a half looked for in the wrong list isn't there
        assert!(splitter_endpoint(&caps, 0, Input).is_none());
        assert!(splitter_endpoint(&rens, 9, Output).is_none());
        // a missing cable
        assert!(splitter_endpoint(&caps, 1, Output).is_none());
    }

    #[test]
    fn splitter_feedback_is_found() {
        let caps = devs(&[
            "Microphone (USB Mic)",
            "Splitter 1 Output (Splitter Cables)",
            "Splitter B1 (Splitter Cables)",
        ]);
        let rens = devs(&[
            "Speakers (Realtek(R) Audio)",
            "Splitter 1 (Splitter Cables)",
            "Splitter B1 Input (Splitter Cables)",
        ]);
        let none: Vec<Vec<usize>> = vec![vec![]; NUM_STRIPS];
        let quiet: Vec<Vec<usize>> = vec![vec![]; NUM_BUSES];
        // the usual setup: strip 1 records its cable, B1 plays into its own,
        // A1 plays on the speakers
        let mut strips = none.clone();
        strips[0] = vec![1];
        strips[1] = vec![0];
        let mut buses = quiet.clone();
        buses[0] = vec![0];
        buses[3] = vec![2];
        assert!(splitter_feedback(&caps, &rens, &strips, &buses).is_empty());
        // A1 plays into strip 1's cable too
        buses[0] = vec![0, 1];
        assert_eq!(
            splitter_feedback(&caps, &rens, &strips, &buses),
            ["bus A1 plays into Splitter 1 and strip 1 records that cable: if strip 1 goes to A1 (directly or through a send), it feeds back"]
        );
        // strip 2 records B1's microphone as well
        buses[0] = vec![0];
        strips[1] = vec![0, 2];
        assert_eq!(
            splitter_feedback(&caps, &rens, &strips, &buses),
            ["bus B1 plays into Splitter B1 Input and strip 2 records that cable: if strip 2 goes to B1 (directly or through a send), it feeds back"]
        );
        // nobody plays into B1's cable: its microphone is silent, no loop
        buses[3] = vec![];
        assert!(splitter_feedback(&caps, &rens, &strips, &buses).is_empty());
        // and nothing at all without Splitter
        assert!(splitter_feedback(&caps[..1], &rens[..1], &none, &quiet).is_empty());
    }

    #[test]
    fn default_never_follows_windows_into_splitter() {
        // default output "Splitter 1" (all desktop sound into strip 1): A1
        // plays on the speakers, not back into the cable strip 1 records
        let outs = devs(&["Splitter 1 (Splitter Cables)", "Speakers (Realtek(R) Audio)"]);
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("id0")).unwrap().id, "id1");
        assert_eq!(default_cable(&outs, Some("id0")).unwrap().id, "id0");
        assert_eq!(resolve(PHYSICAL, &outs, Some("id0")).unwrap().id, "id1");
        // the same with a second Splitter device, numbered by Windows
        let outs = devs(&["Splitter 1 (2- Splitter Cables)", "Speakers (Realtek(R) Audio)"]);
        assert_eq!(resolve(DEFAULT_DEVICE, &outs, Some("id0")).unwrap().id, "id1");
        // default input "Splitter B1" (bus B1 as every app's microphone):
        // the Mic strip records the real microphone
        let ins = devs(&["Splitter B1 (Splitter Cables)", "Microphone (USB Mic)"]);
        assert_eq!(resolve(DEFAULT_DEVICE, &ins, Some("id0")).unwrap().id, "id1");
        assert_eq!(resolve(PHYSICAL, &ins, Some("id0")).unwrap().id, "id1");
        // no default known: not a Splitter cable either
        assert_eq!(resolve(DEFAULT_DEVICE, &ins, None).unwrap().id, "id1");
        // a pattern finds exactly its cable
        let all = devs(&[
            "Splitter 1 Output (Splitter Cables)",
            "Splitter 2 Output (Splitter Cables)",
            "Splitter A1 (Splitter Cables)",
            "Splitter B1 (Splitter Cables)",
        ]);
        assert_eq!(resolve("Splitter 2 Output", &all, None).unwrap().id, "id1");
        assert_eq!(resolve("Splitter B1", &all, None).unwrap().id, "id3");
        assert_eq!(resolve("splitter 1", &all, None).unwrap().id, "id0");
    }
}

