//! Windows channel menus: WASAPI devices and VB-Audio virtual cables.

use crate::gui::DevOp;
use bananasplit::config::{Config, DEFAULT_DEVICE, PHYSICAL};
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
    ok: bool,
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
    // detect CABLE, CABLE-A, CABLE-B, CABLE-C, CABLE-D
    for b in ["cable-d", "cable-c", "cable-b", "cable-a", "cable"] {
        if n.starts_with(b) || n.contains(&format!("({b})")) || n.contains(&format!(" {b}")) {
            // normalize base
            let base = match b {
                "cable" => "CABLE".to_string(),
                x => format!("CABLE-{}", &x[6..7].to_uppercase()),
            };
            return Some(base);
        }
    }
    None
}

fn enumerate() -> Result<(Vec<wasapi::Device>, Vec<wasapi::Device>), String> {
    let _com = ffi::ComInit::mta();
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
            ok: false,
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
        if let Some(o) = by_base_out.get(b) {
            if let Some(i) = by_base_in.get(b) {
                families.push(Family { base: b.to_string(), out_friendly: o.clone(), in_friendly: i.clone() });
            }
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

fn build_plan_info(cfg: &Config, caps: &[route::DevInfo], rens: &[route::DevInfo]) -> Plan {
    let families = families(caps, rens);
    let ok = !families.is_empty();
    let why = if ok { String::new() } else { crate::win::cables::MISSING.to_string() };
    // assign: first all strips in order, then buses? priority strips then buses for the rest; default cable true for all
    let mut used: HashSet<usize> = HashSet::new();
    let mut strip_cable = [const { None }; NUM_STRIPS];
    let mut bus_cable = [const { None }; NUM_BUSES];
    let mut strip_cable_label = [const { None }; NUM_STRIPS];
    let mut bus_cable_label = [const { None }; NUM_BUSES];
    // assign to strips first
    for s in 0..NUM_STRIPS {
        if !cfg.strips[s].cable { continue; }
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
    // assign to buses
    for b in 0..NUM_BUSES {
        if !cfg.buses[b].cable { continue; }
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
    Plan { strip_cable, bus_cable, strip_cable_label, bus_cable_label, families, ok, why }
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

fn effective_conn_strip(s: usize, cfg: &Config, plan: &Plan, all: &[String]) -> Vec<String> {
    let mut v: Vec<String> = cfg.strips[s].connect.iter().cloned().collect();
    if let Some(c) = &plan.strip_cable[s] {
        // add friendly name if not present
        let lc = c.to_lowercase();
        if !v.iter().any(|x| x.to_lowercase()==lc || route::resolve(x, &route_devs(), None).map_or(false, |d| d.name.to_lowercase()==lc)) {
            // better just push friendly
            if !v.iter().any(|x| x.to_lowercase().contains(&lc.replace("(vb-audio virtual cable)","").replace("(vb-audio cable","").trim().to_lowercase()) || lc.contains(&x.to_lowercase())) {
                // simple dedupe by substring of friendly
            }
        }
        // push friendly name as connect equivalent? GUI expects connect patterns; but easier push friendly if not matching
        let exists = v.iter().any(|x| {
            let xl = x.to_lowercase();
            xl == lc || route::resolve(x, &fake_devs(), None).map_or(false, |d| d.name.to_lowercase()==lc)
        });
        if !exists { v.push(c.clone()); }
    }
    // dedupe
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for x in v {
        let k = x.to_lowercase();
        if seen.contains(&k) { continue; }
        seen.insert(k);
        out.push(x);
    }
    out
}

fn fake_devs() -> Vec<crate::win::route::DevInfo> {
    let (caps,rens)=enumerate().unwrap_or_default();
    caps.into_iter().chain(rens.into_iter()).map(|d| crate::win::route::DevInfo{name:d.info.name,id:d.info.id}).collect()
}

fn route_devs() -> Vec<crate::win::route::DevInfo> {
    fake_devs()
}

pub fn list(cfg: &Config) -> String {
    let _com = ffi::ComInit::mta();
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
    format!("{{\"cables\":{{\"ok\":{},\"why\":\"{}\"}},\"pulse\":false,\"strip\":[{}],\"bus\":[{}],\"play\":[],\"rec\":[],\"ch\":[{}]}}", if plan.ok {"true"} else {"false"}, json_escape(&plan.why), strip_cand.join(","), bus_cand.join(","), ch_out)
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
                    let exists = list.iter().any(|x| x.to_lowercase()==c2.to_lowercase() || route::resolve(x, &route_devs(), None).map_or(false,|d| d.name.to_lowercase()==c2.to_lowercase()));
                    if !exists { list.push(c2); }
                } else {
                    list.retain(|x| {
                        if x.to_lowercase()==c2.to_lowercase() { return false; }
                        if let Some(d)=route::resolve(x,&route_devs(),None){ if d.name.to_lowercase()==c2.to_lowercase(){ return false; } }
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
                        if let Some(d)=route::resolve(x,&route_devs(),None){ if d.name.to_lowercase()==client.to_lowercase(){ return false; } }
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
            if on && p0.families.is_empty() {
                return Err(format!(
                    "No VB-Audio virtual cable installed. Install VB-CABLE from {} to enable per-channel cables.",
                    crate::win::cables::VBCABLE_PAGE
                ));
            }
            if ch < NUM_STRIPS {
                cfg.strips[ch].cable = on;
                if on {
                    let p2 = build_plan(cfg);
                    if p2.strip_cable[ch].is_none() {
                        cfg.strips[ch].cable = false;
                        return Err(format!(
                            "All {} installed VB-Audio cable pair(s) are already used. Install more (CABLE-A to CABLE-D) or switch another channel's cable off.",
                            p0.families.len()
                        ));
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
                        return Err(format!(
                            "All {} installed VB-Audio cable pair(s) are already used. Install more (CABLE-A to CABLE-D) or switch another channel's cable off.",
                            p0.families.len()
                        ));
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
