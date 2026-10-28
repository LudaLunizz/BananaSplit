//! Detection of the optional virtual cable drivers on Windows (Splitter,
//! BananaSplit's own, and VB-Audio's), and what the app's "Virtual cables"
//! window shows about them.
use super::route::{self, DevInfo};
use super::wasapi::Enumerator;
use bananasplit::config::Config;
use bananasplit::engine::{NUM_BUSES, NUM_STRIPS};

pub const VBCABLE_PAGE: &str = "https://vb-audio.com/Cable/";
/// The channel menus and the status line, while none is installed.
pub const MISSING: &str = "No virtual cables are installed (they are optional): Menu › Virtual cables… explains what they are for";

fn infos(en: &Enumerator, capture: bool) -> Result<Vec<DevInfo>, String> {
    Ok(en.devices(capture)?.into_iter().map(|d| d.info).collect())
}

/// Report missing drivers (a note for the status line). True if none is
/// installed, by the same test as the channel menus (`chanmenu::has_cables`).
pub fn first_run(en: &Enumerator, _cfg: &mut Config, headless: bool, notes: &mut Vec<String>) -> bool {
    let (caps, rens) = match (infos(en, true), infos(en, false)) {
        (Ok(c), Ok(r)) => (c, r),
        (Err(e), _) | (_, Err(e)) => {
            notes.push(e);
            return false;
        }
    };
    let missing = !super::chanmenu::has_cables(&caps, &rens);
    if missing && headless {
        notes.push(format!(
            "No virtual cables are installed (optional); install Splitter, BananaSplit's own ({}), or VB-Audio's ({VBCABLE_PAGE}) to use virtual app inputs and outputs",
            route::SPLITTER_PAGE
        ));
    } else if missing {
        notes.push(MISSING.into());
    }
    missing
}

/// `GET /api/cables`: the cables (Splitter's, then VB-Audio's pairs),
/// Voicemeeter's devices and the Windows defaults as they are now. An error
/// when Windows can't list its devices: then nothing is known about the
/// cables, which is not the same as "none installed".
pub fn report() -> Result<String, String> {
    let _com = super::ffi::ComInit::mta()?;
    let en = Enumerator::new()?;
    let (caps, rens) = (infos(&en, true)?, infos(&en, false)?);
    Ok(report_json(&caps, &rens, en.default_id(true).as_deref(), en.default_id(false).as_deref()))
}

fn report_json(caps: &[DevInfo], rens: &[DevInfo], def_cap: Option<&str>, def_ren: Option<&str>) -> String {
    use crate::gui::jstr;
    // Splitter: whether any of its endpoints is there, and which channels'
    // cables are complete (both halves; one may be switched off in Windows)
    let (mut cables, mut missing) = (Vec::new(), Vec::new());
    for ch in 0..NUM_STRIPS + NUM_BUSES {
        let complete = route::splitter_endpoint(rens, ch, route::SplitterEnd::Input).is_some()
            && route::splitter_endpoint(caps, ch, route::SplitterEnd::Output).is_some();
        if complete { &mut cables } else { &mut missing }.push(route::splitter_name(ch));
    }
    let found = caps.iter().chain(rens).any(|d| route::is_splitter(&d.name));
    let mut o = String::from("{\"splitter\":{\"found\":");
    o.push_str(if found { "true" } else { "false" });
    for (key, list) in [("cables", &cables), ("missing", &missing)] {
        o.push_str(",\"");
        o.push_str(key);
        o.push_str("\":[");
        for (k, n) in list.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, n);
        }
        o.push(']');
    }
    o.push_str("},\"pairs\":[");
    for (k, p) in super::chanmenu::cable_pairs(caps, rens).iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        jstr(&mut o, p);
    }
    o.push_str("],\"voicemeeter\":[");
    let mut vm: Vec<&str> = Vec::new();
    for d in rens.iter().chain(caps) {
        let n = route::short_name(&d.name);
        if d.name.to_ascii_lowercase().contains("voicemeeter") && !vm.contains(&n) {
            vm.push(n);
        }
    }
    for (k, n) in vm.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        jstr(&mut o, n);
    }
    o.push(']');
    // the Windows defaults: installing a cable can make it the default
    for (key, devs, def) in [("output", rens, def_ren), ("input", caps, def_cap)] {
        o.push_str(",\"");
        o.push_str(key);
        o.push_str("\":");
        match def.and_then(|id| devs.iter().find(|d| d.id == id)) {
            Some(d) => {
                o.push_str("{\"name\":");
                jstr(&mut o, route::short_name(&d.name));
                o.push_str(if route::is_cable(&d.name) { ",\"cable\":true}" } else { ",\"cable\":false}" });
            }
            None => o.push_str("null"),
        }
    }
    o.push('}');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names.iter().enumerate().map(|(i, n)| DevInfo { name: n.to_string(), id: format!("id{i}") }).collect()
    }

    /// No Splitter at all.
    const NO_SPLITTER: &str = r#"{"splitter":{"found":false,"cables":[],"missing":["Splitter 1","Splitter 2","Splitter 3","Splitter 4","Splitter 5","Splitter 6","Splitter A1","Splitter A2","Splitter A3","Splitter B1","Splitter B2"]},"#;

    #[test]
    fn report_with_voicemeeter_and_no_cables() {
        let rens = devs(&["Headphones (Headset)", "Voicemeeter Input (VB-Audio Voicemeeter VAIO)", "Voicemeeter AUX Input (VB-Audio Voicemeeter AUX VAIO)"]);
        let caps = devs(&["Microphone (Headset)", "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)"]);
        assert_eq!(
            report_json(&caps, &rens, Some("id0"), Some("id1")),
            NO_SPLITTER.to_string() + r#""pairs":[],"voicemeeter":["Voicemeeter Input","Voicemeeter AUX Input","Voicemeeter Out B1"],"output":{"name":"Voicemeeter Input","cable":false},"input":{"name":"Microphone","cable":false}}"#
        );
    }

    #[test]
    fn report_says_when_a_cable_is_the_default() {
        let rens = devs(&["CABLE Input (VB-Audio Virtual Cable)", "CABLE-A Input (VB-Audio Cable A)", "Speakers (Realtek(R) Audio)"]);
        let caps = devs(&["CABLE Output (VB-Audio Virtual Cable)", "CABLE-A Output (VB-Audio Cable A)"]);
        assert_eq!(
            report_json(&caps, &rens, None, Some("id0")),
            NO_SPLITTER.to_string() + r#""pairs":["CABLE","CABLE-A"],"voicemeeter":[],"output":{"name":"CABLE Input","cable":true},"input":null}"#
        );
    }

    #[test]
    fn report_lists_splitter_cables() {
        // all of Splitter but bus A2's microphone half (switched off in
        // Windows), with "Splitter B1" as every app's default microphone
        let rens: Vec<String> = (0..NUM_STRIPS + NUM_BUSES)
            .map(|ch| {
                let n = route::splitter_name(ch);
                if ch < NUM_STRIPS { format!("{n} (Splitter Cables)") } else { format!("{n} Input (Splitter Cables)") }
            })
            .chain(["Speakers (Realtek(R) Audio)".to_string()])
            .collect();
        let caps: Vec<String> = (0..NUM_STRIPS + NUM_BUSES)
            .filter(|&ch| ch != NUM_STRIPS + 1)
            .map(|ch| {
                let n = route::splitter_name(ch);
                if ch < NUM_STRIPS { format!("{n} Output (Splitter Cables)") } else { format!("{n} (Splitter Cables)") }
            })
            .collect();
        let rens = devs(&rens.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let caps = devs(&caps.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        // caps: strips' outputs id0..id5, then A1 (id6), A3 (id7), B1 (id8), B2 (id9)
        assert_eq!(
            report_json(&caps, &rens, Some("id8"), Some("id11")),
            r#"{"splitter":{"found":true,"cables":["Splitter 1","Splitter 2","Splitter 3","Splitter 4","Splitter 5","Splitter 6","Splitter A1","Splitter A3","Splitter B1","Splitter B2"],"missing":["Splitter A2"]},"pairs":[],"voicemeeter":[],"output":{"name":"Speakers","cable":false},"input":{"name":"Splitter B1","cable":true}}"#
        );
    }
}
