//! Virtual cables on macOS: loopback devices of an optional driver of their
//! own (BlackHole, VB-Cable, Loopback, Soundflower), and what the app's
//! "Virtual cables" window shows about them. BananaSplit never installs a
//! driver itself.

use super::Snapshot;
use super::route::{self, DevInfo};
use crate::gui::jstr;

/// The channel menus and the status line, while none is installed.
pub const MISSING: &str = "No virtual cables are installed (they are optional): Menu › Virtual cables… explains what they are for";

/// The cables that can serve a channel: loopback devices with an input
/// and an output (one device on macOS), in the order of their names.
pub fn installed(caps: &[DevInfo], rens: &[DevInfo]) -> Vec<String> {
    let mut v: Vec<String> = caps
        .iter()
        .filter(|c| route::is_cable(&c.name) && rens.iter().any(|r| r.id == c.id))
        .map(|c| c.name.clone())
        .collect();
    v.sort_by(|a, b| route::natural_cmp(a, b));
    v.dedup();
    v
}

/// Report a missing driver (a note for the status line). True if no cable
/// is installed.
pub fn first_run(s: &Snapshot, headless: bool, notes: &mut Vec<String>) -> bool {
    let missing = installed(&s.caps, &s.rens).is_empty();
    if missing && headless {
        notes.push(format!(
            "No virtual cables are installed (optional); install BlackHole ({}) to use virtual app inputs and outputs",
            route::BLACKHOLE_PAGE
        ));
    } else if missing {
        notes.push(MISSING.into());
    }
    missing
}

/// `GET /api/cables`: the cables, and the macOS defaults as they are now.
/// `welcome`: the first start, so the page opens the window by itself.
pub fn report(welcome: bool) -> String {
    match super::snapshot() {
        Ok(s) => report_json(&s, welcome, None),
        Err(e) => {
            let none = Snapshot { caps: Vec::new(), rens: Vec::new(), def_cap: None, def_ren: None };
            report_json(&none, welcome, Some(&e))
        }
    }
}

fn report_json(s: &Snapshot, welcome: bool, error: Option<&str>) -> String {
    let mut o = String::from("{\"error\":");
    match error {
        Some(e) => jstr(&mut o, e),
        None => o.push_str("null"),
    }
    o.push_str(",\"cables\":[");
    for (k, c) in installed(&s.caps, &s.rens).iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        jstr(&mut o, c);
    }
    o.push(']');
    // the macOS defaults: installing a cable can make it the default
    for (key, devs, def) in [("output", &s.rens, &s.def_ren), ("input", &s.caps, &s.def_cap)] {
        o.push_str(",\"");
        o.push_str(key);
        o.push_str("\":");
        match def.as_deref().and_then(|id| devs.iter().find(|d| d.id == id)) {
            Some(d) => {
                o.push_str("{\"name\":");
                jstr(&mut o, &d.name);
                o.push_str(if route::is_cable(&d.name) { ",\"cable\":true}" } else { ",\"cable\":false}" });
            }
            None => o.push_str("null"),
        }
    }
    o.push_str(",\"welcome\":");
    o.push_str(if welcome { "true" } else { "false" });
    o.push('}');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names.iter().map(|n| DevInfo { name: n.to_string(), id: format!("uid-{n}"), virt: route::is_cable(n) }).collect()
    }

    #[test]
    fn installed_cables_have_both_directions_in_name_order() {
        let caps = devs(&["MacBook Pro Microphone", "BlackHole 64ch", "BlackHole 2ch", "Loopback Audio"]);
        // Loopback Audio set up as an input only: no cable
        let rens = devs(&["MacBook Pro Speakers", "BlackHole 2ch", "BlackHole 64ch"]);
        assert_eq!(installed(&caps, &rens), ["BlackHole 2ch", "BlackHole 64ch"]);
        assert!(installed(&caps[..1], &rens[..1]).is_empty());
    }

    #[test]
    fn report_says_what_is_there() {
        let s = Snapshot {
            caps: devs(&["MacBook Pro Microphone", "BlackHole 2ch"]),
            rens: devs(&["MacBook Pro Speakers", "BlackHole 2ch"]),
            def_cap: Some("uid-MacBook Pro Microphone".into()),
            def_ren: Some("uid-BlackHole 2ch".into()),
        };
        assert_eq!(
            report_json(&s, true, None),
            r#"{"error":null,"cables":["BlackHole 2ch"],"output":{"name":"BlackHole 2ch","cable":true},"input":{"name":"MacBook Pro Microphone","cable":false},"welcome":true}"#
        );
        let none = Snapshot { caps: vec![], rens: vec![], def_cap: None, def_ren: None };
        assert_eq!(
            report_json(&none, false, Some("cannot list the audio devices")),
            r#"{"error":"cannot list the audio devices","cables":[],"output":null,"input":null,"welcome":false}"#
        );
    }

    #[test]
    fn first_run_notes() {
        let s = Snapshot { caps: devs(&["MacBook Pro Microphone"]), rens: devs(&["MacBook Pro Speakers"]), def_cap: None, def_ren: None };
        let mut notes = Vec::new();
        assert!(first_run(&s, false, &mut notes));
        assert_eq!(notes, [MISSING]);
        notes.clear();
        assert!(first_run(&s, true, &mut notes));
        assert!(notes[0].contains(route::BLACKHOLE_PAGE));
        let s = Snapshot { caps: devs(&["BlackHole 2ch"]), rens: devs(&["BlackHole 2ch"]), def_cap: None, def_ren: None };
        assert!(!first_run(&s, false, &mut notes));
    }
}
