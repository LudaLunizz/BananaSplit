//! Detection of optional VB-Audio virtual cable drivers on Windows, and
//! what the app's "Virtual cables" window shows about them.
use super::route::{self, DevInfo};
use super::wasapi::Enumerator;
use bananasplit::config::Config;

pub const VBCABLE_PAGE: &str = "https://vb-audio.com/Cable/";
/// The channel menus and the status line, while none is installed.
pub const MISSING: &str = "VB-Audio virtual cables are not installed (they are optional): Menu › Virtual cables… explains what they are for";

pub fn installed_cables(en: &Enumerator) -> Vec<String> {
    let mut v = Vec::new();
    for capture in [false, true] {
        if let Ok(devs) = en.devices(capture) {
            v.extend(devs.into_iter().map(|d| d.info.name).filter(|n| route::is_vb_cable(n)));
        }
    }
    v
}

/// Report missing drivers (a note for the status line). True if none is
/// installed.
pub fn first_run(en: &Enumerator, _cfg: &mut Config, headless: bool, notes: &mut Vec<String>) -> bool {
    let missing = installed_cables(en).is_empty();
    if missing && headless {
        notes.push(format!("VB-Audio virtual cables are not installed (optional); install them from {VBCABLE_PAGE} to use virtual app inputs and outputs"));
    } else if missing {
        notes.push(MISSING.into());
    }
    missing
}

/// `GET /api/cables`: the cables, Voicemeeter's devices and the Windows
/// defaults as they are now. `welcome`: the first start, so the page opens
/// the window by itself.
pub fn report(welcome: bool) -> String {
    let _com = super::ffi::ComInit::mta();
    let Ok(en) = Enumerator::new() else {
        return report_json(&[], &[], None, None, welcome);
    };
    let infos = |capture| en.devices(capture).map(|v| v.into_iter().map(|d| d.info).collect::<Vec<_>>()).unwrap_or_default();
    let (caps, rens) = (infos(true), infos(false));
    report_json(&caps, &rens, en.default_id(true).as_deref(), en.default_id(false).as_deref(), welcome)
}

fn report_json(caps: &[DevInfo], rens: &[DevInfo], def_cap: Option<&str>, def_ren: Option<&str>, welcome: bool) -> String {
    use crate::gui::jstr;
    let mut o = String::from("{\"pairs\":[");
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
                o.push_str(if route::is_vb_cable(&d.name) { ",\"cable\":true}" } else { ",\"cable\":false}" });
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
        names.iter().enumerate().map(|(i, n)| DevInfo { name: n.to_string(), id: format!("id{i}") }).collect()
    }

    #[test]
    fn report_with_voicemeeter_and_no_cables() {
        let rens = devs(&["Headphones (Headset)", "Voicemeeter Input (VB-Audio Voicemeeter VAIO)", "Voicemeeter AUX Input (VB-Audio Voicemeeter AUX VAIO)"]);
        let caps = devs(&["Microphone (Headset)", "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)"]);
        assert_eq!(
            report_json(&caps, &rens, Some("id0"), Some("id1"), true),
            r#"{"pairs":[],"voicemeeter":["Voicemeeter Input","Voicemeeter AUX Input","Voicemeeter Out B1"],"output":{"name":"Voicemeeter Input","cable":false},"input":{"name":"Microphone","cable":false},"welcome":true}"#
        );
    }

    #[test]
    fn report_says_when_a_cable_is_the_default() {
        let rens = devs(&["CABLE Input (VB-Audio Virtual Cable)", "CABLE-A Input (VB-Audio Cable A)", "Speakers (Realtek(R) Audio)"]);
        let caps = devs(&["CABLE Output (VB-Audio Virtual Cable)", "CABLE-A Output (VB-Audio Cable A)"]);
        assert_eq!(
            report_json(&caps, &rens, None, Some("id0"), false),
            r#"{"pairs":["CABLE","CABLE-A"],"voicemeeter":[],"output":{"name":"CABLE Input","cable":true},"input":null,"welcome":false}"#
        );
    }
}
