//! Resolving `connect =` entries to Windows audio devices (pure logic, no
//! Windows calls, so it is unit-tested on every platform).
//!
//! On Windows a `connect` value is either `@physical` (the default device,
//! skipping virtual cables so the mixer never feeds itself) or a
//! case-insensitive fragment of a device's name as shown in the Windows
//! sound settings, e.g. `CABLE Output` or `Headset Earphone`.

use bananasplit::config::{DEFAULT_DEVICE, PHYSICAL};

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
}

/// True for VB-Audio cable endpoints (what the install prompt looks for).
pub fn is_vb_cable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("vb-audio") && (n.starts_with("cable") || n.contains("cable"))
}

/// A pattern that names a virtual cable. Missing cables are reported as
/// "not installed" rather than as errors (most people only have some).
pub fn names_cable(pattern: &str) -> bool {
    pattern.to_ascii_lowercase().contains("cable")
}

/// Find the device for one `connect` entry.
pub fn resolve<'a>(
    pattern: &str,
    devs: &'a [DevInfo],
    default_id: Option<&str>,
) -> Option<&'a DevInfo> {
    if pattern == DEFAULT_DEVICE {
        // Whatever Windows uses by default, virtual cable or not.
        return default_id
            .and_then(|id| devs.iter().find(|d| d.id == id))
            .or_else(|| devs.first());
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
}

