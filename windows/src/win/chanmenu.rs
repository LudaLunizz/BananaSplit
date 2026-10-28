//! Windows channel menus: WASAPI devices and virtual cables: BananaSplit's
//! own (Splitter: one cable per channel) and VB-Audio's.

use crate::gui::DevOp;
use bananasplit::config::{Config, DEFAULT_DEVICE};
use bananasplit::engine::{NUM_BUSES, NUM_STRIPS};
use std::collections::{HashSet, BTreeMap};

use crate::win::{ffi, route, wasapi};

#[derive(Clone)]
struct Plan {
    strip_cable: [Option<String>; NUM_STRIPS], // friendly name of capture side added
    bus_cable: [Option<String>; NUM_BUSES],     // friendly name of render side added
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
    if !n.contains("vb-audio") { return None; }
    // Hi-Fi Cable is a pair of its own ("Hi-Fi Cable Input (VB-Audio Hi-Fi
    // Cable)"), not one half of CABLE
    if n.contains("hi-fi cable") { return Some("Hi-Fi Cable".to_string()); }
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

fn enumerate() -> Result<(Vec<wasapi::Device>, Vec<wasapi::Device>), String> {
    let _com = ffi::ComInit::mta()?;
    let en = wasapi::Enumerator::new()?;
    let caps = en.devices(true)?; // capture
    let rens = en.devices(false)?; // render
    Ok((caps, rens))
}

fn build_plan(cfg: &Config) -> Plan {
    match enumerate() {
        Ok((caps, rens)) => {
            let cap_info: Vec<route::DevInfo> = caps.into_iter().map(|d| d.info).collect();
            let ren_info: Vec<route::DevInfo> = rens.into_iter().map(|d| d.info).collect();
            build_plan_info(cfg, &cap_info, &ren_info)
        }
        Err(e) => Plan {
            strip_cable: Default::default(),
            bus_cable: Default::default(),
            strip_cable_label: Default::default(),
            bus_cable_label: Default::default(),
            families: vec![],
            splitter: false,
            ok: false,
            none: false,
            why: e,
        },
    }
}

/// The cable assignment for a device list the caller already has (the audio
/// graph uses this so what the menus show matches what is connected).
pub fn cable_extra(
    cfg: &Config,
    caps: &[route::DevInfo],
    rens: &[route::DevInfo],
) -> (Vec<Option<String>>, Vec<Option<String>>) {
    let p = build_plan_info(cfg, caps, rens);
    (p.strip_cable.to_vec(), p.bus_cable.to_vec())
}

/// The complete VB-Audio cable pairs (both ends installed), CABLE first.
fn families(caps: &[route::DevInfo], rens: &[route::DevInfo]) -> Vec<Family> {
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
            && let Some(i) = by_base_in.get(b) {
                families.push(Family { base: b.to_string(), out_friendly: o.clone(), in_friendly: i.clone() });
            }
    }
    // also any others
    for (b,o) in &by_base_out {
        if families.iter().any(|f| f.base==*b) { continue; }
        if let Some(i)=by_base_in.get(b) {
            families.push(Family { base: b.clone(), out_friendly: o.clone(), in_friendly: i.clone() });
        }
    }
    families
}

/// The names of the complete cable pairs, e.g. ["CABLE", "CABLE-A"].
pub fn cable_pairs(caps: &[route::DevInfo], rens: &[route::DevInfo]) -> Vec<String> {
    families(caps, rens).into_iter().map(|f| f.base).collect()
}

/// Whether any cable BananaSplit can use is installed: a Splitter endpoint
/// or a complete VB-Audio pair. The menus, the status line and the first
/// start's welcome all go by this.
pub fn has_cables(caps: &[route::DevInfo], rens: &[route::DevInfo]) -> bool {
    caps.iter().chain(rens).any(|d| route::is_splitter(&d.name)) || !families(caps, rens).is_empty()
}

fn build_plan_info(cfg: &Config, caps: &[route::DevInfo], rens: &[route::DevInfo]) -> Plan {
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
    // halves there (one may be switched off in the Windows sound settings).
    for s in 0..NUM_STRIPS {
        if !cfg.strips[s].cable { continue; }
        if let (Some(out), Some(inp)) = (
            route::splitter_endpoint(caps, s, route::SplitterEnd::Output),
            route::splitter_endpoint(rens, s, route::SplitterEnd::Input),
        ) {
            strip_cable[s] = Some(out.name.clone());
            strip_cable_label[s] = Some(inp.name.clone());
        }
    }
    for b in 0..NUM_BUSES {
        if !cfg.buses[b].cable { continue; }
        if let (Some(inp), Some(out)) = (
            route::splitter_endpoint(rens, NUM_STRIPS + b, route::SplitterEnd::Input),
            route::splitter_endpoint(caps, NUM_STRIPS + b, route::SplitterEnd::Output),
        ) {
            bus_cable[b] = Some(inp.name.clone());
            bus_cable_label[b] = Some(out.name.clone());
        }
    }
    // VB-Audio pairs for the channels Splitter doesn't cover: strips first,
    // in order, then buses.
    for s in 0..NUM_STRIPS {
        if !cfg.strips[s].cable || strip_cable[s].is_some() { continue; }
        // find free family not conflicting with explicit connects
        for (idx,f) in families.iter().enumerate() {
            if used.contains(&idx) { continue; }
            // check if explicit connects use either end
            let mut conflict = false;
            for p in &cfg.strips[s].connect {
                let pl = p.to_lowercase();
                if pl.contains(&f.in_friendly.to_lowercase()) || pl.contains(&f.out_friendly.to_lowercase()) { conflict = true; }
            }
            if conflict { continue; }
            used.insert(idx);
            // strip captures from CABLE Output (f.in_friendly)
            strip_cable[s] = Some(f.in_friendly.clone());
            // label to show where to send apps is CABLE Input
            strip_cable_label[s] = Some(f.out_friendly.clone());
            break;
        }
    }
    for b in 0..NUM_BUSES {
        if !cfg.buses[b].cable || bus_cable[b].is_some() { continue; }
        for (idx,f) in families.iter().enumerate() {
            if used.contains(&idx) { continue; }
            let mut conflict = false;
            for p in &cfg.buses[b].connect {
                let pl = p.to_lowercase();
                if pl.contains(&f.in_friendly.to_lowercase()) || pl.contains(&f.out_friendly.to_lowercase()) { conflict = true; }
            }
            if conflict { continue; }
            used.insert(idx);
            // bus plays to CABLE Input
            bus_cable[b] = Some(f.out_friendly.clone());
            bus_cable_label[b] = Some(f.in_friendly.clone());
            break;
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

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len()*2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            _ => o.push(c),
        }
    }
    o
}

fn short(s: &str) -> String {
    route::short_name(s).to_string()
}

fn fake_devs() -> Vec<crate::win::route::DevInfo> {
    let (caps,rens)=enumerate().unwrap_or_default();
    caps.into_iter().chain(rens).map(|d| crate::win::route::DevInfo{name:d.info.name,id:d.info.id}).collect()
}

fn route_devs() -> Vec<crate::win::route::DevInfo> {
    fake_devs()
}

pub fn list(cfg: &Config) -> String {
    // Without COM the lists below are just empty.
    let _com = ffi::ComInit::mta().ok();
    let plan = build_plan(cfg);
    let (caps, rens) = enumerate().unwrap_or((vec![],vec![]));
    // build candidates
    let mut strip_cand: Vec<String> = Vec::new();
    let mut bus_cand: Vec<String> = Vec::new();
    // default capture/render
    let en = wasapi::Enumerator::new().ok();
    let def_cap = en.as_ref().and_then(|e| e.default_id(true));
    let def_ren = en.as_ref().and_then(|e| e.default_id(false));
    // strip candidates = capture
    for d in &caps {
        let client = d.info.name.clone();
        let label = short(&d.info.name);
        let def = def_cap.as_ref() == Some(&d.info.id);
        strip_cand.push(format!("{{\"client\":\"{}\",\"label\":\"{}\",\"kind\":\"device\",\"default\":{}}}", json_escape(&client), json_escape(&label), if def {"true"} else {"false"}));
    }
    for d in &rens {
        let client = d.info.name.clone();
        let label = short(&d.info.name);
        let def = def_ren.as_ref() == Some(&d.info.id);
        bus_cand.push(format!("{{\"client\":\"{}\",\"label\":\"{}\",\"kind\":\"device\",\"default\":{}}}", json_escape(&client), json_escape(&label), if def {"true"} else {"false"}));
    }
    // channels
    let mut ch_out = String::new();
    for i in 0..NUM_STRIPS {
        // effective conn friendly names
        let mut conn_list: Vec<String> = cfg.strips[i].connect.clone();
        if let Some(c)=&plan.strip_cable[i] {
            // ensure cable present as a friendly name reference
            let lc = c.to_lowercase();
            if !conn_list.iter().any(|x| x.to_lowercase()==lc) { conn_list.push(c.clone()); }
        }
        let conn_json = conn_list.iter().map(|x| format!("\"{}\"", json_escape(&short(x)))).collect::<Vec<_>>().join(",");
        // also full connect patterns
        let connp_json = cfg.strips[i].connect.iter().map(|x| format!("\"{}\"", json_escape(x))).collect::<Vec<_>>().join(",");
        // cable
        let cable_on = plan.strip_cable[i].is_some();
        let cable_name = plan.strip_cable_label[i].clone().unwrap_or_else(|| "Virtual cable".to_string());
        if i>0 { ch_out.push(','); }
        ch_out.push_str(&format!("{{\"id\":\"{}\",\"cable\":{{\"on\":{},\"name\":\"{}\"}},\"conn\":[{}],\"connect\":[{}]}}", i+1, if cable_on {"true"} else {"false"}, json_escape(&short(&cable_name)), conn_json, connp_json));
    }
    for i in 0..NUM_BUSES {
        let mut conn_list: Vec<String> = cfg.buses[i].connect.clone();
        if let Some(c)=&plan.bus_cable[i] {
            let lc = c.to_lowercase();
            if !conn_list.iter().any(|x| x.to_lowercase()==lc) { conn_list.push(c.clone()); }
        }
        let conn_json = conn_list.iter().map(|x| format!("\"{}\"", json_escape(&short(x)))).collect::<Vec<_>>().join(",");
        let connp_json = cfg.buses[i].connect.iter().map(|x| format!("\"{}\"", json_escape(x))).collect::<Vec<_>>().join(",");
        let cable_on = plan.bus_cable[i].is_some();
        let cable_name = plan.bus_cable_label[i].clone().unwrap_or_else(|| "Virtual cable".to_string());
        ch_out.push(',');
        let id = match i { 0=>"A1",1=>"A2",2=>"A3",3=>"B1",4=>"B2", _=>"A1" };
        ch_out.push_str(&format!("{{\"id\":\"{}\",\"cable\":{{\"on\":{},\"name\":\"{}\"}},\"conn\":[{}],\"connect\":[{}]}}", id, if cable_on {"true"} else {"false"}, json_escape(&short(&cable_name)), conn_json, connp_json));
    }
    format!("{{\"cables\":{{\"ok\":{},\"none\":{},\"why\":\"{}\"}},\"pulse\":false,\"strip\":[{}],\"bus\":[{}],\"play\":[],\"rec\":[],\"ch\":[{}]}}", plan.ok, plan.none, json_escape(&plan.why), strip_cand.join(","), bus_cand.join(","), ch_out)
}

fn apply_dev(cfg: &mut Config, op: DevOp) -> Result<String, String> {
    match op {
        DevOp::List => Ok(list(cfg)),
        DevOp::Client { ch, client, on } => {
            if ch < NUM_STRIPS {
                let mut list = cfg.strips[ch].connect.clone();
                // find matching device friendly name
                let c2 = client.clone();
                if on {
                    // add if not present
                    let exists = list.iter().any(|x| x.to_lowercase()==c2.to_lowercase() || route::resolve(x, &route_devs(), None).is_some_and(|d| d.name.to_lowercase()==c2.to_lowercase()));
                    if !exists { list.push(c2); }
                } else {
                    list.retain(|x| {
                        if x.to_lowercase()==c2.to_lowercase() { return false; }
                        if let Some(d)=route::resolve(x,&route_devs(),None) && d.name.to_lowercase()==c2.to_lowercase(){ return false; }
                        true
                    });
                }
                cfg.strips[ch].connect = list;
            } else {
                let b = ch - NUM_STRIPS;
                if b >= NUM_BUSES { return Err("bad channel".into()); }
                let mut list = cfg.buses[b].connect.clone();
                if on {
                    let exists = list.iter().any(|x| x.to_lowercase()==client.to_lowercase());
                    if !exists { list.push(client.clone()); }
                } else {
                    list.retain(|x| {
                        if x.to_lowercase()==client.to_lowercase() { return false; }
                        if let Some(d)=route::resolve(x,&route_devs(),None) && d.name.to_lowercase()==client.to_lowercase(){ return false; }
                        true
                    });
                }
                cfg.buses[b].connect = list;
            }
            Ok(list(cfg))
        }
        DevOp::Default { ch, on } => {
            if ch < NUM_STRIPS {
                let mut list = cfg.strips[ch].connect.clone();
                if on {
                    if !list.iter().any(|x| x==DEFAULT_DEVICE) { list.push(DEFAULT_DEVICE.to_string()); }
                } else {
                    list.retain(|x| x!=DEFAULT_DEVICE);
                }
                cfg.strips[ch].connect = list;
            } else {
                let b = ch - NUM_STRIPS;
                if b>=NUM_BUSES { return Err("bad channel".into()); }
                let mut list = cfg.buses[b].connect.clone();
                if on {
                    if !list.iter().any(|x| x==DEFAULT_DEVICE) { list.push(DEFAULT_DEVICE.to_string()); }
                } else {
                    list.retain(|x| x!=DEFAULT_DEVICE);
                }
                cfg.buses[b].connect = list;
            }
            Ok(list(cfg))
        }
        DevOp::Unpattern { ch, pattern } => {
            if ch < NUM_STRIPS {
                cfg.strips[ch].connect.retain(|x| x!=&pattern);
            } else {
                let b = ch-NUM_STRIPS; if b<NUM_BUSES { cfg.buses[b].connect.retain(|x| x!=&pattern); }
            }
            Ok(list(cfg))
        }
        DevOp::Nothing { ch } => {
            if ch < NUM_STRIPS { cfg.strips[ch].connect.clear(); } else { let b=ch-NUM_STRIPS; if b<NUM_BUSES { cfg.buses[b].connect.clear(); } }
            Ok(list(cfg))
        }
        DevOp::Cable { ch, on } => {
            let p0 = build_plan(cfg);
            if on && !p0.ok {
                // no cable installed (or the devices can't be listed)
                return Err(p0.why.clone());
            }
            if ch < NUM_STRIPS {
                cfg.strips[ch].cable = on;
                if on {
                    let p2 = build_plan(cfg);
                    if p2.strip_cable[ch].is_none() {
                        cfg.strips[ch].cable = false;
                        return Err(no_free_cable(&p0, ch));
                    }
                }
            } else {
                let b = ch - NUM_STRIPS;
                if b >= NUM_BUSES {
                    return Err("bad channel".into());
                }
                cfg.buses[b].cable = on;
                if on {
                    let p2 = build_plan(cfg);
                    if p2.bus_cable[b].is_none() {
                        cfg.buses[b].cable = false;
                        return Err(no_free_cable(&p0, ch));
                    }
                }
            }
            Ok(list(cfg))
        }
        DevOp::Stream { .. } => Err("Windows does not let BananaSplit move another app's stream; pick the device in the app's own audio settings.".into()),
    }
}

pub fn handle(op: DevOp, cfg: &mut Config) -> Result<String, String> {
    apply_dev(cfg, op)
}

/// A one-line "what this channel is connected to" and cable flag for the page.
pub fn views(cfg: &Config) -> Vec<crate::gui::ChanView> {
    let plan = build_plan(cfg);
    let mut v = Vec::with_capacity(NUM_STRIPS + NUM_BUSES);
    for i in 0..NUM_STRIPS {
        let mut parts: Vec<String> = cfg.strips[i].connect.iter().map(|x| short(x)).collect();
        if let Some(c) = &plan.strip_cable_label[i] {
            parts.push(short(c));
        }
        let dev = if parts.is_empty() { "(nothing connected)".to_string() } else { parts.join(" + ") };
        v.push(crate::gui::ChanView { dev, cable: plan.strip_cable[i].is_some() });
    }
    for i in 0..NUM_BUSES {
        let mut parts: Vec<String> = cfg.buses[i].connect.iter().map(|x| short(x)).collect();
        if let Some(c) = &plan.bus_cable_label[i] {
            parts.push(short(c));
        }
        let dev = if parts.is_empty() { "(nothing connected)".to_string() } else { parts.join(" + ") };
        v.push(crate::gui::ChanView { dev, cable: plan.bus_cable[i].is_some() });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs(names: &[&str]) -> Vec<route::DevInfo> {
        names.iter().enumerate().map(|(i, n)| route::DevInfo { name: n.to_string(), id: format!("id{i}") }).collect()
    }

    /// Every Splitter endpoint, minus the ones named in `without`.
    fn splitter(without: &[&str]) -> (Vec<route::DevInfo>, Vec<route::DevInfo>) {
        let (mut caps, mut rens) = (Vec::new(), Vec::new());
        for ch in 0..NUM_STRIPS + NUM_BUSES {
            let n = route::splitter_name(ch);
            let (input, output) = if ch < NUM_STRIPS { (n.clone(), format!("{n} Output")) } else { (format!("{n} Input"), n) };
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
            assert_eq!(p.strip_cable[s].as_deref(), Some(format!("Splitter {} Output (Splitter Cables)", s + 1).as_str()));
            assert_eq!(p.strip_cable_label[s].as_deref(), Some(format!("Splitter {} (Splitter Cables)", s + 1).as_str()));
        }
        for (b, id) in ["A1", "A2", "A3", "B1", "B2"].iter().enumerate() {
            assert_eq!(p.bus_cable[b].as_deref(), Some(format!("Splitter {id} Input (Splitter Cables)").as_str()));
            assert_eq!(p.bus_cable_label[b].as_deref(), Some(format!("Splitter {id} (Splitter Cables)").as_str()));
        }
    }

    #[test]
    fn default_config_uses_strips_and_b_buses() {
        // strips and B1, B2 have their cables on; A1..A3 are speakers
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
        caps.extend(devs(&VB_CAPS).into_iter().map(|mut d| { d.id.push_str("vb"); d }));
        rens.extend(devs(&VB_RENS).into_iter().map(|mut d| { d.id.push_str("vb"); d }));
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
        caps.extend(devs(&VB_CAPS).into_iter().map(|mut d| { d.id.push_str("vb"); d }));
        rens.extend(devs(&VB_RENS).into_iter().map(|mut d| { d.id.push_str("vb"); d }));
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
        assert!(no_free_cable(&p, 2).starts_with("All 2 installed VB-Audio cable pair(s) are already used. Install Splitter"));
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
        let p = build_plan_info(&Config::default(), &devs(&["Microphone (USB Mic)"]), &devs(&["Speakers (Realtek(R) Audio)"]));
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
}
