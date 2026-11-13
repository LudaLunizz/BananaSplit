//! Windows channel menus: WASAPI devices and virtual cables: BananaSplit's
//! own (Splitter: one cable per channel) and VB-Audio's.

use crate::gui::{DevOp, jstr};
use bananasplit::config::{Config, DEFAULT_DEVICE, PHYSICAL};
use bananasplit::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS};
use std::collections::{BTreeMap, HashSet};

use crate::win::route::{self, DevInfo};
use crate::win::{ffi, wasapi};

#[derive(Clone)]
struct Plan {
    strip_cable: [Option<String>; NUM_STRIPS], // friendly name of capture side added
    bus_cable: [Option<String>; NUM_BUSES],    // friendly name of render side added
    strip_cable_label: [Option<String>; NUM_STRIPS],
    bus_cable_label: [Option<String>; NUM_BUSES],
    families: Vec<Family>,
    /// Some Splitter endpoint is installed.
    splitter: bool,
    ok: bool,
    /// No cable is installed (not: the devices could not be listed).
    none: bool,
    why: String,
}

#[derive(Clone)]
struct Family {
    base: String,
    out_friendly: String, // render: CABLE Input
    in_friendly: String,  // capture: CABLE Output
}

fn is_vb_pair_base(name: &str) -> Option<String> {
    let n = name.to_ascii_lowercase();
    if !n.contains("vb-audio") {
        return None;
    }
    // Hi-Fi Cable is a pair of its own ("Hi-Fi Cable Input (VB-Audio Hi-Fi
    // Cable)"), not one half of CABLE
    if n.contains("hi-fi cable") {
        return Some("Hi-Fi Cable".to_string());
    }
    // detect CABLE, CABLE-A, CABLE-B, CABLE-C, CABLE-D
    for b in ["cable-d", "cable-c", "cable-b", "cable-a", "cable"] {
        if n.starts_with(b) || n.contains(&format!("({b})")) || n.contains(&format!(" {b}")) {
            // normalize base
            let base = match b {
                "cable" => "CABLE".to_string(),
                x => format!("CABLE-{}", x[6..7].to_uppercase()),
            };
            return Some(base);
        }
    }
    None
}

/// The devices and the Windows defaults, read once for each request so the
/// plan, the lists and the toggles all see the same devices.
struct Devs {
    caps: Vec<DevInfo>,
    rens: Vec<DevInfo>,
    def_cap: Option<String>,
    def_ren: Option<String>,
}

impl Devs {
    /// The devices channel `ch` can connect: recording ones for a strip,
    /// playback ones for a bus.
    fn of(&self, ch: usize) -> &[DevInfo] {
        if ch < NUM_STRIPS { &self.caps } else { &self.rens }
    }
}

fn read_devs() -> Result<Devs, String> {
    let _com = ffi::ComInit::mta()?;
    let en = wasapi::Enumerator::new()?;
    let info =
        |capture| -> Result<Vec<DevInfo>, String> { Ok(en.devices(capture)?.into_iter().map(|d| d.info).collect()) };
    Ok(Devs { caps: info(true)?, rens: info(false)?, def_cap: en.default_id(true), def_ren: en.default_id(false) })
}

/// The plan for the devices read, or why there is none.
fn plan_for(cfg: &Config, devs: &Result<Devs, String>) -> Plan {
    match devs {
        Ok(d) => build_plan_info(cfg, &d.caps, &d.rens),
        Err(e) => Plan {
            strip_cable: Default::default(),
            bus_cable: Default::default(),
            strip_cable_label: Default::default(),
            bus_cable_label: Default::default(),
            families: vec![],
            splitter: false,
            ok: false,
            none: false,
            why: e.clone(),
        },
    }
}

/// The cable assignment for a device list the caller already has (the audio
/// graph uses this so what the menus show matches what is connected).
pub fn cable_extra(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo]) -> (Vec<Option<String>>, Vec<Option<String>>) {
    let p = build_plan_info(cfg, caps, rens);
    (p.strip_cable.to_vec(), p.bus_cable.to_vec())
}

/// For each strip, the name of the playback device apps play into to reach
/// it ("Splitter 2 (Splitter Cables)", or a VB-Audio pair's "CABLE Input");
/// `None` when the strip has no cable.
pub fn strip_inputs(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo]) -> Vec<Option<String>> {
    build_plan_info(cfg, caps, rens).strip_cable_label.to_vec()
}

/// The complete VB-Audio cable pairs (both ends installed), CABLE first.
fn families(caps: &[DevInfo], rens: &[DevInfo]) -> Vec<Family> {
    // find families
    let mut by_base_out: BTreeMap<String, String> = BTreeMap::new();
    let mut by_base_in: BTreeMap<String, String> = BTreeMap::new();
    for d in rens {
        if let Some(base) = is_vb_pair_base(&d.name) {
            by_base_out.insert(base.clone(), d.name.clone());
        }
    }
    for d in caps {
        if let Some(base) = is_vb_pair_base(&d.name) {
            by_base_in.insert(base.clone(), d.name.clone());
        }
    }
    let mut families: Vec<Family> = Vec::new();
    // prefer order CABLE, CABLE-A, CABLE-B, CABLE-C, CABLE-D
    let order = ["CABLE", "CABLE-A", "CABLE-B", "CABLE-C", "CABLE-D"];
    for b in order {
        if let Some(o) = by_base_out.get(b)
            && let Some(i) = by_base_in.get(b)
        {
            families.push(Family { base: b.to_string(), out_friendly: o.clone(), in_friendly: i.clone() });
        }
    }
    // also any others
    for (b, o) in &by_base_out {
        if families.iter().any(|f| f.base == *b) {
            continue;
        }
        if let Some(i) = by_base_in.get(b) {
            families.push(Family { base: b.clone(), out_friendly: o.clone(), in_friendly: i.clone() });
        }
    }
    families
}

/// The names of the complete cable pairs, e.g. ["CABLE", "CABLE-A"].
pub fn cable_pairs(caps: &[DevInfo], rens: &[DevInfo]) -> Vec<String> {
    families(caps, rens).into_iter().map(|f| f.base).collect()
}

/// Whether any cable BananaSplit can use is installed: a Splitter endpoint
/// or a complete VB-Audio pair. The menus, the status line and the first
/// start's welcome all go by this.
pub fn has_cables(caps: &[DevInfo], rens: &[DevInfo]) -> bool {
    caps.iter().chain(rens).any(|d| route::is_splitter(&d.name)) || !families(caps, rens).is_empty()
}

/// Whether `connect` entry `pattern` connects the device called `client`,
/// looked up in `devs` (the devices of the channel's own kind, so a bus's
/// "Splitter 1" is the playback device, not "Splitter 1 Output"). `@default`
/// and `@physical` never do: they have a switch of their own, and the device
/// they stand for depends on the Windows default.
fn connects(pattern: &str, client: &str, devs: &[DevInfo]) -> bool {
    let client = client.to_lowercase();
    pattern.to_lowercase() == client
        || (pattern != DEFAULT_DEVICE
            && pattern != PHYSICAL
            && route::resolve(pattern, devs, None).is_some_and(|d| d.name.to_lowercase() == client))
}

/// A device ticked (`on`) or unticked in a channel's menu: add it to the
/// channel's `connect` list unless an entry already connects it, or remove
/// every entry that connects it.
fn toggle(list: &mut Vec<String>, client: String, on: bool, devs: &[DevInfo]) {
    if !on {
        list.retain(|p| !connects(p, &client, devs));
    } else if !list.iter().any(|p| connects(p, &client, devs)) {
        list.push(client);
    }
}

fn build_plan_info(cfg: &Config, caps: &[DevInfo], rens: &[DevInfo]) -> Plan {
    let families = families(caps, rens);
    let splitter = caps.iter().chain(rens).any(|d| route::is_splitter(&d.name));
    let ok = has_cables(caps, rens);
    let why = if ok { String::new() } else { crate::win::cables::MISSING.to_string() };
    let mut used: HashSet<usize> = HashSet::new();
    let mut strip_cable = [const { None }; NUM_STRIPS];
    let mut bus_cable = [const { None }; NUM_BUSES];
    let mut strip_cable_label = [const { None }; NUM_STRIPS];
    let mut bus_cable_label = [const { None }; NUM_BUSES];
    // Splitter first: every channel has a cable of its own, so switching
    // one on or off never moves another. Strip N records "Splitter N
    // Output" (apps play into "Splitter N"); bus B1 plays into "Splitter B1
    // Input" (apps record "Splitter B1"). A cable counts only with both
    // halves there (one may be switched off in the Windows sound settings),
    // on the same Splitter device.
    for s in 0..NUM_STRIPS {
        if !cfg.strips[s].cable {
            continue;
        }
        if let Some((inp, out)) = route::splitter_pair(caps, rens, s) {
            strip_cable[s] = Some(caps[out].name.clone());
            strip_cable_label[s] = Some(rens[inp].name.clone());
        }
    }
    for b in 0..NUM_BUSES {
        if !cfg.buses[b].cable {
            continue;
        }
        if let Some((inp, out)) = route::splitter_pair(caps, rens, NUM_STRIPS + b) {
            bus_cable[b] = Some(rens[inp].name.clone());
            bus_cable_label[b] = Some(caps[out].name.clone());
        }
    }
    // VB-Audio pairs for the channels Splitter doesn't cover: strips first,
    // in order, then buses. A pair the channel already connects by its own
    // `connect` entries is skipped.
    for s in 0..NUM_STRIPS {
        if !cfg.strips[s].cable || strip_cable[s].is_some() {
            continue;
        }
        let free = families.iter().enumerate().find(|(idx, f)| {
            !used.contains(idx) && !cfg.strips[s].connect.iter().any(|p| connects(p, &f.in_friendly, caps))
        });
        if let Some((idx, f)) = free {
            used.insert(idx);
            // the strip records CABLE Output; apps play into CABLE Input
            strip_cable[s] = Some(f.in_friendly.clone());
            strip_cable_label[s] = Some(f.out_friendly.clone());
        }
    }
    for b in 0..NUM_BUSES {
        if !cfg.buses[b].cable || bus_cable[b].is_some() {
            continue;
        }
        let free = families.iter().enumerate().find(|(idx, f)| {
            !used.contains(idx) && !cfg.buses[b].connect.iter().any(|p| connects(p, &f.out_friendly, rens))
        });
        if let Some((idx, f)) = free {
            used.insert(idx);
            // the bus plays into CABLE Input; apps record CABLE Output
            bus_cable[b] = Some(f.out_friendly.clone());
            bus_cable_label[b] = Some(f.in_friendly.clone());
        }
    }
    Plan { strip_cable, bus_cable, strip_cable_label, bus_cable_label, families, splitter, ok, none: !ok, why }
}

/// Why channel `ch`'s cable can't be switched on, although some virtual
/// cable is installed.
fn no_free_cable(p: &Plan, ch: usize) -> String {
    if p.splitter {
        format!(
            "{} is missing (switched off or renamed in the Windows sound settings?), and no VB-Audio cable pair is free",
            route::splitter_name(ch)
        )
    } else {
        format!(
            "All {} installed VB-Audio cable pair(s) are already used. Install Splitter, BananaSplit's own cables (one for every channel: Menu › Virtual cables…), or more VB-Audio cables, or switch another channel's cable off.",
            p.families.len()
        )
    }
}

/// Channel `ch`'s `connect` list: strips, then buses.
fn connect_of(cfg: &mut Config, ch: usize) -> Result<&mut Vec<String>, String> {
    match ch.checked_sub(NUM_STRIPS) {
        None => Ok(&mut cfg.strips[ch].connect),
        Some(b) if b < NUM_BUSES => Ok(&mut cfg.buses[b].connect),
        Some(_) => Err("bad channel".into()),
    }
}

/// Channel `ch`'s cable switch.
fn cable_of(cfg: &mut Config, ch: usize) -> Result<&mut bool, String> {
    match ch.checked_sub(NUM_STRIPS) {
        None => Ok(&mut cfg.strips[ch].cable),
        Some(b) if b < NUM_BUSES => Ok(&mut cfg.buses[b].cable),
        Some(_) => Err("bad channel".into()),
    }
}

/// The devices a strip (`devs` = recording ones) or a bus can pick, as JSON
/// objects.
fn candidates(o: &mut String, devs: &[DevInfo], default: Option<&str>) {
    for (k, d) in devs.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str("{\"client\":");
        jstr(o, &d.name);
        o.push_str(",\"label\":");
        jstr(o, route::short_name(&d.name));
        o.push_str(",\"kind\":\"device\",\"default\":");
        o.push_str(if default == Some(d.id.as_str()) { "true" } else { "false" });
        o.push('}');
    }
}

fn list(cfg: &Config) -> String {
    list_with(cfg, &read_devs())
}

/// The menus' contents (JSON). Without the devices (no COM, or Windows
/// can't list them) the device lists are empty and `why` says so.
fn list_with(cfg: &Config, devs: &Result<Devs, String>) -> String {
    let plan = plan_for(cfg, devs);
    let (caps, rens, def_cap, def_ren) = match devs {
        Ok(d) => (&d.caps[..], &d.rens[..], d.def_cap.as_deref(), d.def_ren.as_deref()),
        Err(_) => (&[][..], &[][..], None, None),
    };
    let flag = |b: bool| if b { "true" } else { "false" };
    let mut o = String::from("{\"cables\":{\"ok\":");
    o.push_str(flag(plan.ok));
    o.push_str(",\"none\":");
    o.push_str(flag(plan.none));
    o.push_str(",\"why\":");
    jstr(&mut o, &plan.why);
    o.push_str("},\"pulse\":false,\"strip\":[");
    candidates(&mut o, caps, def_cap);
    o.push_str("],\"bus\":[");
    candidates(&mut o, rens, def_ren);
    o.push_str("],\"play\":[],\"rec\":[],\"ch\":[");
    for ch in 0..NUM_STRIPS + NUM_BUSES {
        let (id, connect, cable, label) = match ch.checked_sub(NUM_STRIPS) {
            None => ((ch + 1).to_string(), &cfg.strips[ch].connect, &plan.strip_cable[ch], &plan.strip_cable_label[ch]),
            Some(b) => (BUS_IDS[b].to_string(), &cfg.buses[b].connect, &plan.bus_cable[b], &plan.bus_cable_label[b]),
        };
        if ch > 0 {
            o.push(',');
        }
        o.push_str("{\"id\":");
        jstr(&mut o, &id);
        o.push_str(",\"cable\":{\"on\":");
        o.push_str(flag(cable.is_some()));
        o.push_str(",\"name\":");
        jstr(&mut o, route::short_name(label.as_deref().unwrap_or("Virtual cable")));
        // what is connected, by short name: the entries, plus the cable
        // unless an entry already names it
        o.push_str("},\"conn\":[");
        let extra = cable.as_ref().filter(|c| !connect.iter().any(|x| x.to_lowercase() == c.to_lowercase()));
        for (k, x) in connect.iter().chain(extra).enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, route::short_name(x));
        }
        // and the entries as written
        o.push_str("],\"connect\":[");
        for (k, x) in connect.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            jstr(&mut o, x);
        }
        o.push_str("]}");
    }
    o.push_str("]}");
    o
}

fn apply_dev(cfg: &mut Config, op: DevOp) -> Result<String, String> {
    match op {
        DevOp::List => Ok(list(cfg)),
        DevOp::Client { ch, client, on } => {
            let devs = read_devs();
            // without the devices only an entry that is the device's own
            // name can be matched
            let known = devs.as_ref().map_or(&[][..], |d| d.of(ch));
            toggle(connect_of(cfg, ch)?, client, on, known);
            Ok(list_with(cfg, &devs))
        }
        DevOp::Default { ch, on } => {
            let entries = connect_of(cfg, ch)?;
            if !on {
                entries.retain(|x| x != DEFAULT_DEVICE);
            } else if !entries.iter().any(|x| x == DEFAULT_DEVICE) {
                entries.push(DEFAULT_DEVICE.to_string());
            }
            Ok(list(cfg))
        }
        DevOp::Unpattern { ch, pattern } => {
            connect_of(cfg, ch)?.retain(|x| *x != pattern);
            Ok(list(cfg))
        }
        DevOp::Nothing { ch } => {
            connect_of(cfg, ch)?.clear();
            Ok(list(cfg))
        }
        DevOp::Cable { ch, on } => {
            let devs = read_devs();
            let p0 = plan_for(cfg, &devs);
            if on && !p0.ok {
                // no cable installed (or the devices can't be listed)
                return Err(p0.why);
            }
            *cable_of(cfg, ch)? = on;
            if on {
                let p2 = plan_for(cfg, &devs);
                let got = match ch.checked_sub(NUM_STRIPS) {
                    None => p2.strip_cable[ch].is_some(),
                    Some(b) => p2.bus_cable[b].is_some(),
                };
                if !got {
                    *cable_of(cfg, ch)? = false;
                    return Err(no_free_cable(&p0, ch));
                }
            }
            Ok(list_with(cfg, &devs))
        }
        DevOp::Stream { .. } => Err("Windows does not let BananaSplit move another app's stream; pick the device in the app's own audio settings.".into()),
    }
}

pub fn handle(op: DevOp, cfg: &mut Config) -> Result<String, String> {
    apply_dev(cfg, op)
}

/// A one-line "what this channel is connected to" and cable flag for the page.
pub fn views(cfg: &Config) -> Vec<crate::gui::ChanView> {
    let plan = plan_for(cfg, &read_devs());
    let mut v = Vec::with_capacity(NUM_STRIPS + NUM_BUSES);
    for ch in 0..NUM_STRIPS + NUM_BUSES {
        let (connect, cable, label) = match ch.checked_sub(NUM_STRIPS) {
            None => (&cfg.strips[ch].connect, &plan.strip_cable[ch], &plan.strip_cable_label[ch]),
            Some(b) => (&cfg.buses[b].connect, &plan.bus_cable[b], &plan.bus_cable_label[b]),
        };
        let parts: Vec<&str> = connect.iter().chain(label).map(|x| route::short_name(x)).collect();
        let dev = if parts.is_empty() { "(nothing connected)".to_string() } else { parts.join(" + ") };
        v.push(crate::gui::ChanView { dev, cable: cable.is_some() });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<DevInfo> {
        names.iter().enumerate().map(|(i, n)| DevInfo { name: n.to_string(), id: format!("id{i}") }).collect()
    }

    /// Every Splitter endpoint, minus the ones named in `without`.
    fn splitter(without: &[&str]) -> (Vec<DevInfo>, Vec<DevInfo>) {
        let (mut caps, mut rens) = (Vec::new(), Vec::new());
        for ch in 0..NUM_STRIPS + NUM_BUSES {
            let n = route::splitter_name(ch);
            let (input, output) =
                if ch < NUM_STRIPS { (n.clone(), format!("{n} Output")) } else { (format!("{n} Input"), n) };
            if !without.contains(&input.as_str()) {
                rens.push(format!("{input} (Splitter Cables)"));
            }
            if !without.contains(&output.as_str()) {
                caps.push(format!("{output} (Splitter Cables)"));
            }
        }
        let caps: Vec<&str> = caps.iter().map(|s| s.as_str()).collect();
        let rens: Vec<&str> = rens.iter().map(|s| s.as_str()).collect();
        (devs(&caps), devs(&rens))
    }

    const VB_CAPS: [&str; 2] = ["CABLE Output (VB-Audio Virtual Cable)", "CABLE-A Output (VB-Audio Cable A)"];
    const VB_RENS: [&str; 2] = ["CABLE Input (VB-Audio Virtual Cable)", "CABLE-A Input (VB-Audio Cable A)"];

    #[test]
    fn splitter_gives_every_channel_its_own_cable() {
        let mut cfg = Config::default();
        for b in &mut cfg.buses {
            b.cable = true;
        }
        let (caps, rens) = splitter(&[]);
        let p = build_plan_info(&cfg, &caps, &rens);
        assert!(p.ok && p.splitter && p.why.is_empty());
        for s in 0..NUM_STRIPS {
            assert_eq!(
                p.strip_cable[s].as_deref(),
                Some(format!("Splitter {} Output (Splitter Cables)", s + 1).as_str())
            );
            assert_eq!(
                p.strip_cable_label[s].as_deref(),
                Some(format!("Splitter {} (Splitter Cables)", s + 1).as_str())
            );
        }
        for (b, id) in bananasplit::engine::LEGACY_BUS_IDS.iter().enumerate() {
            assert_eq!(p.bus_cable[b].as_deref(), Some(format!("Splitter {id} Input (Splitter Cables)").as_str()));
            assert_eq!(p.bus_cable_label[b].as_deref(), Some(format!("Splitter {id} (Splitter Cables)").as_str()));
        }
    }

    #[test]
    fn default_config_uses_strips_and_b_buses() {
        // strips and B1, B2 have their cables on; V1..V3 none
        let cfg = Config::default();
        let (caps, rens) = splitter(&[]);
        let p = build_plan_info(&cfg, &caps, &rens);
        assert!(p.strip_cable.iter().all(|c| c.is_some()));
        assert_eq!(p.bus_cable.iter().map(|c| c.is_some()).collect::<Vec<_>>(), [false, false, false, true, true]);
    }

    #[test]
    fn switching_one_cable_off_moves_no_other() {
        let mut cfg = Config::default();
        cfg.strips[1].cable = false;
        cfg.buses[3].cable = false;
        let (caps, rens) = splitter(&[]);
        let p = build_plan_info(&cfg, &caps, &rens);
        assert!(p.strip_cable[1].is_none());
        assert_eq!(p.strip_cable[2].as_deref(), Some("Splitter 3 Output (Splitter Cables)"));
        assert!(p.bus_cable[3].is_none());
        assert_eq!(p.bus_cable[4].as_deref(), Some("Splitter B2 Input (Splitter Cables)"));
    }

    #[test]
    fn vb_audio_covers_what_splitter_does_not() {
        let cfg = Config::default();
        // strip 2's "Splitter 2" and bus B1's "Splitter B1" switched off in Windows
        let (mut caps, mut rens) = splitter(&["Splitter 2", "Splitter B1"]);
        caps.extend(devs(&VB_CAPS).into_iter().map(|mut d| {
            d.id.push_str("vb");
            d
        }));
        rens.extend(devs(&VB_RENS).into_iter().map(|mut d| {
            d.id.push_str("vb");
            d
        }));
        let p = build_plan_info(&cfg, &caps, &rens);
        // strips first: strip 2 gets CABLE, then bus B1 gets CABLE-A
        assert_eq!(p.strip_cable[1].as_deref(), Some(VB_CAPS[0]));
        assert_eq!(p.strip_cable_label[1].as_deref(), Some(VB_RENS[0]));
        assert_eq!(p.bus_cable[3].as_deref(), Some(VB_RENS[1]));
        assert_eq!(p.bus_cable_label[3].as_deref(), Some(VB_CAPS[1]));
        // the others keep their Splitter cables
        assert_eq!(p.strip_cable[0].as_deref(), Some("Splitter 1 Output (Splitter Cables)"));
        assert_eq!(p.bus_cable[4].as_deref(), Some("Splitter B2 Input (Splitter Cables)"));
    }

    #[test]
    fn splitter_wins_over_vb_audio() {
        // both installed, every cable on: VB-Audio's pairs stay free
        let mut cfg = Config::default();
        for b in &mut cfg.buses {
            b.cable = true;
        }
        let (mut caps, mut rens) = splitter(&[]);
        caps.extend(devs(&VB_CAPS).into_iter().map(|mut d| {
            d.id.push_str("vb");
            d
        }));
        rens.extend(devs(&VB_RENS).into_iter().map(|mut d| {
            d.id.push_str("vb");
            d
        }));
        let p = build_plan_info(&cfg, &caps, &rens);
        let all: Vec<&str> = p.strip_cable.iter().chain(&p.bus_cable).map(|c| c.as_deref().unwrap()).collect();
        assert_eq!(all.len(), NUM_STRIPS + NUM_BUSES);
        assert!(all.iter().all(|c| c.ends_with("(Splitter Cables)")), "{all:?}");
    }

    #[test]
    fn only_vb_audio_as_before() {
        let cfg = Config::default();
        let p = build_plan_info(&cfg, &devs(&VB_CAPS), &devs(&VB_RENS));
        assert!(p.ok && !p.splitter);
        assert_eq!(p.strip_cable[0].as_deref(), Some(VB_CAPS[0]));
        assert_eq!(p.strip_cable[1].as_deref(), Some(VB_CAPS[1]));
        assert!(p.strip_cable[2].is_none() && p.bus_cable[3].is_none());
        assert!(
            no_free_cable(&p, 2)
                .starts_with("All 2 installed VB-Audio cable pair(s) are already used. Install Splitter")
        );
    }

    #[test]
    fn hi_fi_cable_is_a_pair_of_its_own() {
        let caps = ["CABLE Output (VB-Audio Virtual Cable)", "Hi-Fi Cable Output (VB-Audio Hi-Fi Cable)"];
        let rens = ["Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)", "CABLE Input (VB-Audio Virtual Cable)"];
        assert_eq!(cable_pairs(&devs(&caps), &devs(&rens)), ["CABLE", "Hi-Fi Cable"]);
        let p = build_plan_info(&Config::default(), &devs(&caps), &devs(&rens));
        // strip 1 records CABLE Output, so apps must play into CABLE Input, not Hi-Fi's
        assert_eq!((p.strip_cable[0].as_deref(), p.strip_cable_label[0].as_deref()), (Some(caps[0]), Some(rens[1])));
        assert_eq!((p.strip_cable[1].as_deref(), p.strip_cable_label[1].as_deref()), (Some(caps[1]), Some(rens[0])));
        // half a pair is no cable
        assert!(!has_cables(&devs(&caps[1..]), &devs(&["Speakers (Realtek(R) Audio)"])));
        assert!(has_cables(&devs(&caps[1..]), &devs(&rens[..1])));
    }

    #[test]
    fn no_cables_at_all() {
        let p = build_plan_info(
            &Config::default(),
            &devs(&["Microphone (USB Mic)"]),
            &devs(&["Speakers (Realtek(R) Audio)"]),
        );
        assert!(!p.ok && p.none && !p.splitter);
        assert_eq!(p.why, crate::win::cables::MISSING);
        assert!(p.strip_cable.iter().chain(&p.bus_cable).all(|c| c.is_none()));
    }

    #[test]
    fn a_missing_splitter_cable_is_named() {
        let cfg = Config::default();
        let (caps, rens) = splitter(&["Splitter 4 Output", "Splitter B2"]);
        let p = build_plan_info(&cfg, &caps, &rens);
        assert!(p.ok && p.strip_cable[3].is_none() && p.bus_cable[4].is_none());
        assert_eq!(
            no_free_cable(&p, 3),
            "Splitter 4 is missing (switched off or renamed in the Windows sound settings?), and no VB-Audio cable pair is free"
        );
        assert!(no_free_cable(&p, NUM_STRIPS + 4).starts_with("Splitter B2 is missing"));
    }

    #[test]
    fn a_bus_unticks_its_splitter_cable() {
        // what the bus menu matches against: the playback devices only
        let (_, rens) = splitter(&[]);
        let mut list = vec!["Splitter 1".to_string(), "Splitter B1".to_string()];
        toggle(&mut list, "Splitter 1 (Splitter Cables)".into(), false, &rens);
        assert_eq!(list, ["Splitter B1"]);
        // "Splitter B1" on a bus is the bus's "Splitter B1 Input"
        toggle(&mut list, "Splitter B1 Input (Splitter Cables)".into(), false, &rens);
        assert!(list.is_empty());
        // and a strip's "Splitter 1" is its recording half
        let (caps, _) = splitter(&[]);
        let mut list = vec!["Splitter 1".to_string()];
        toggle(&mut list, "Splitter 1 Output (Splitter Cables)".into(), true, &caps);
        assert_eq!(list, ["Splitter 1"]);
        toggle(&mut list, "Splitter 1 Output (Splitter Cables)".into(), false, &caps);
        assert!(list.is_empty());
    }

    #[test]
    fn a_device_toggle_leaves_default_alone() {
        let caps = devs(&["Microphone (USB Mic)", "Line In (Realtek(R) Audio)"]);
        let mut list = vec![DEFAULT_DEVICE.to_string(), PHYSICAL.to_string()];
        // ticking the first real device adds it, although @default and
        // @physical would pick it without a Windows default
        toggle(&mut list, caps[0].name.clone(), true, &caps);
        assert_eq!(list, [DEFAULT_DEVICE, PHYSICAL, "Microphone (USB Mic)"]);
        // unticking it removes only that entry
        toggle(&mut list, caps[0].name.clone(), false, &caps);
        assert_eq!(list, [DEFAULT_DEVICE, PHYSICAL]);
        // a pattern and the name itself count, case aside
        let mut list = vec!["usb mic".to_string(), "MICROPHONE (USB MIC)".to_string(), "Line".to_string()];
        toggle(&mut list, caps[0].name.clone(), false, &caps);
        assert_eq!(list, ["Line"]);
        // without the device list only the name itself matches
        let mut list = vec!["usb mic".to_string()];
        toggle(&mut list, caps[0].name.clone(), true, &[]);
        assert_eq!(list, ["usb mic", "Microphone (USB Mic)"]);
    }

    #[test]
    fn vb_audio_skips_a_pair_the_channel_connects_already() {
        // strip 1 records CABLE Output by an entry of its own: its cable is
        // CABLE-A, and strip 2 gets CABLE
        let mut cfg = Config::default();
        cfg.strips[0].connect = vec!["CABLE Output".into()];
        let p = build_plan_info(&cfg, &devs(&VB_CAPS), &devs(&VB_RENS));
        assert_eq!(p.strip_cable[0].as_deref(), Some(VB_CAPS[1]));
        assert_eq!(p.strip_cable[1].as_deref(), Some(VB_CAPS[0]));
        // the same for a bus that plays into CABLE Input already
        let mut cfg = Config::default();
        for s in &mut cfg.strips {
            s.cable = false;
        }
        cfg.buses[3].connect = vec!["cable input".into()];
        let p = build_plan_info(&cfg, &devs(&VB_CAPS), &devs(&VB_RENS));
        assert_eq!(p.bus_cable[3].as_deref(), Some(VB_RENS[1]));
        assert_eq!(p.bus_cable[4].as_deref(), Some(VB_RENS[0]));
    }

    #[test]
    fn the_menu_json() {
        let mut cfg = Config::default();
        cfg.strips[0].connect = vec!["@default".into(), "Mic \"<1>\"".into()];
        // as up to 2.8.24: the first bus on the default output, B2 on none
        cfg.buses[0].connect = vec!["@default".into()];
        cfg.buses[4].connect = vec![];
        let (caps, rens) = splitter(&[]);
        let devs = Devs { caps, rens, def_cap: Some("id0".into()), def_ren: None };
        let o = list_with(&cfg, &Ok(devs));
        assert!(o.starts_with(r#"{"cables":{"ok":true,"none":false,"why":""},"pulse":false,"strip":[{"client":"Splitter 1 Output (Splitter Cables)","label":"Splitter 1 Output","kind":"device","default":true},"#), "{o}");
        assert!(o.contains(r#""ch":[{"id":"1","cable":{"on":true,"name":"Splitter 1"},"conn":["@default","Mic \"\u003c1\u003e\"","Splitter 1 Output"],"connect":["@default","Mic \"\u003c1\u003e\""]},"#), "{o}");
        assert!(
            o.contains(
                r#"{"id":"V1","cable":{"on":false,"name":"Virtual cable"},"conn":["@default"],"connect":["@default"]}"#
            ),
            "{o}"
        );
        assert!(
            o.ends_with(
                r#"{"id":"B2","cable":{"on":true,"name":"Splitter B2"},"conn":["Splitter B2 Input"],"connect":[]}]}"#
            ),
            "{o}"
        );
        // the devices could not be read: empty lists and the reason
        assert_eq!(
            list_with(&Config::default(), &Err("no COM".into())).split(",\"ch\"").next().unwrap(),
            r#"{"cables":{"ok":false,"none":false,"why":"no COM"},"pulse":false,"strip":[],"bus":[],"play":[],"rec":[]"#
        );
    }

    #[test]
    fn halves_of_two_splitter_devices_give_no_cable() {
        let cfg = Config::default();
        let caps = devs(&["Splitter 1 Output (Splitter Cables)"]);
        let rens = devs(&["Splitter 1 (2- Splitter Cables)"]);
        let p = build_plan_info(&cfg, &caps, &rens);
        assert!(p.ok && p.splitter && p.strip_cable[0].is_none());
    }
}
