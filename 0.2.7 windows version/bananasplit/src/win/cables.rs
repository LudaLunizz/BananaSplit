//! Detection of optional VB-Audio virtual cable drivers on Windows.
use super::route;
use super::wasapi::Enumerator;
use bananasplit::config::Config;

pub const VBCABLE_PAGE: &str = "https://vb-audio.com/Cable/";

pub fn installed_cables(en: &Enumerator) -> Vec<String> {
    let mut v = Vec::new();
    for capture in [false, true] {
        if let Ok(devs) = en.devices(capture) {
            v.extend(devs.into_iter().map(|d| d.info.name).filter(|n| route::is_vb_cable(n)));
        }
    }
    v
}

/// Report missing drivers and let the user install them from the vendor.
pub fn first_run(en: &Enumerator, _cfg: &mut Config, _headless: bool, notes: &mut Vec<String>) {
    if installed_cables(en).is_empty() {
        notes.push(format!("VB-Audio virtual cables are not installed; install them from {VBCABLE_PAGE} to use virtual app inputs and outputs"));
    }
}
