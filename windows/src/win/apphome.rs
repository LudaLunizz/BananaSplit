//! Apps keep their strip on Windows (Linux: `linux/src/apphome.rs`).
//!
//! An app goes to a strip by playing into the strip's cable ("Splitter 2",
//! or the "CABLE Input" of a VB-Audio pair), chosen in Windows' Volume mixer
//! (Settings › System › Sound › Volume mixer) or in the app's own settings.
//! Windows remembers the Volume mixer choice per program path, and forgets
//! it when the path changes: Discord installs every update into a new
//! folder (`app-1.0.9xxx\Discord.exe`), so after an update it plays on the
//! default device instead of its strip, and a recording of that strip stays
//! silent. A reinstalled cable driver gives the cables new IDs, with the
//! same effect.
//!
//! So BananaSplit remembers it, in `app-strips` next to the config (the
//! same file and format as on Linux): an app the Volume mixer puts on a
//! strip's cable is noted. A noted app that follows the default device (or
//! names a device that is not there) is put back on its strip's cable once,
//! when BananaSplit first sees its process with sound open. An app put on a
//! device of the computer on purpose is forgotten; an app moved after that
//! is left alone. One `<strip> <app>` per line (`2 Discord`) can also be
//! written by hand; a change applies while BananaSplit runs.
//!
//! Apps are known by their program's file name without `.exe` (`Discord`);
//! case does not matter. This file has no Windows calls (see
//! `apppolicy.rs`), so its logic is tested on every platform.

use bananasplit::engine::NUM_STRIPS;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const FILE: &str = "app-strips";

/// A process with a sound session open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub pid: u32,
    /// The name it is remembered by ([`key`]).
    pub key: String,
    /// The playback device Windows gives it (endpoint ID), "" when it
    /// follows the default.
    pub set_to: String,
}

/// The name an app is remembered by: its program's file name without
/// `.exe` (`C:\...\app-1.0.9187\Discord.exe` → `Discord`).
pub fn key(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => name[..i].to_string(),
        _ => name.to_string(),
    }
}

/// The strip `app` is noted on (the name in any case).
fn home_of(homes: &BTreeMap<String, usize>, app: &str) -> Option<usize> {
    homes.get(app).or_else(|| homes.iter().find(|(a, _)| a.eq_ignore_ascii_case(app)).map(|(_, k)| k)).copied()
}

/// What to do after one look at the apps.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// (app, strip) to remember.
    pub learn: Vec<(String, usize)>,
    /// Apps put on a device of the computer on purpose.
    pub forget: Vec<String>,
    /// (process id, strip) to put back.
    pub moves: Vec<(u32, usize)>,
    /// Processes looked at for good (moved, or nothing to do for them).
    pub done: Vec<u32>,
}

/// Decide for `apps`. `cables[k]` is the endpoint ID apps play into to
/// reach strip k, while that cable is there; `present` tells whether an
/// endpoint ID is a playback device that is there now; `cable` whether it
/// is a virtual cable (BananaSplit records those: never "the computer's");
/// `done` are the processes already looked at.
pub fn plan(
    homes: &BTreeMap<String, usize>,
    apps: &[App],
    cables: &[Option<String>],
    present: impl Fn(&str) -> bool,
    cable: impl Fn(&str) -> bool,
    done: &HashSet<u32>,
) -> Plan {
    // (endpoint IDs hold GUIDs: Windows may print them in either case)
    let strip_of = |id: &str| cables.iter().position(|c| c.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(id)));
    let mut p = Plan::default();
    for a in apps {
        // where Windows puts it on purpose; a device that is gone counts
        // as the default, which is where Windows plays it then
        let set = Some(a.set_to.as_str()).filter(|id| !id.is_empty() && present(id));
        if let Some(id) = set {
            match strip_of(id) {
                Some(k) => {
                    if home_of(homes, &a.key) != Some(k) && !p.learn.iter().any(|(x, _)| *x == a.key) {
                        p.learn.push((a.key.clone(), k));
                    }
                }
                None if !cable(id) && home_of(homes, &a.key).is_some() && !p.forget.contains(&a.key) => {
                    p.forget.push(a.key.clone())
                }
                None => {}
            }
        }
        if done.contains(&a.pid) {
            continue;
        }
        let Some(k) = home_of(homes, &a.key) else {
            p.done.push(a.pid);
            continue;
        };
        if cables.get(k).and_then(Option::as_deref).is_none() {
            continue; // its strip's cable is not there (yet): look again later
        }
        if set.is_none() && !p.forget.contains(&a.key) {
            p.moves.push((a.pid, k));
        }
        p.done.push(a.pid); // put back, on its cable already, or put elsewhere on purpose
    }
    p
}

/// Parse `app-strips`: lines "<strip 1-6> <app>"; `#` starts a comment line.
pub fn parse(text: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some((n, app)) = l.split_once(char::is_whitespace)
            && let Ok(n @ 1..=NUM_STRIPS) = n.parse::<usize>()
        {
            m.insert(app.trim().to_string(), n - 1);
        }
    }
    m
}

pub fn render(m: &BTreeMap<String, usize>) -> String {
    let mut o = String::from(
        "# Apps BananaSplit puts on their strip whenever they start playing (strip number, app).\r\n\
         # Putting an app on a strip's cable in Windows' Volume mixer adds it; putting it on a device of the computer removes it.\r\n\
         # You can edit this file: one \"<strip 1-6> <app>\" per line, e.g. \"2 Discord\" (the program's name without .exe).\r\n",
    );
    for (app, k) in m {
        o.push_str(&format!("{} {app}\r\n", k + 1));
    }
    o
}

pub struct AppHomes {
    path: PathBuf,
    homes: BTreeMap<String, usize>,
    /// The file's modification time when it was last read or written.
    stamp: Option<SystemTime>,
    done: HashSet<u32>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl AppHomes {
    /// The apps remembered next to the config file `config`.
    pub fn load(config: &Path) -> AppHomes {
        let path = config.parent().map_or_else(|| PathBuf::from(FILE), |d| d.join(FILE));
        let homes = std::fs::read_to_string(&path).map(|t| parse(&t)).unwrap_or_default();
        let stamp = mtime(&path);
        AppHomes { path, homes, stamp, done: HashSet::new() }
    }

    /// Read the file again if someone edited it.
    fn reload(&mut self) {
        let now = mtime(&self.path);
        if now != self.stamp {
            self.homes = std::fs::read_to_string(&self.path).map(|t| parse(&t)).unwrap_or_default();
            self.stamp = now;
            self.done.clear(); // apply the new list to the apps that play now
        }
    }

    fn save(&mut self) -> Result<(), String> {
        std::fs::write(&self.path, render(&self.homes))
            .map_err(|e| format!("cannot save {}: {e}", self.path.display()))?;
        self.stamp = mtime(&self.path);
        Ok(())
    }

    /// Look at the apps (see [`plan`] for the other arguments); put apps
    /// back on their strips with `put(pid, endpoint id)`. Returns a line
    /// for the user per app put back.
    pub fn poll(
        &mut self,
        apps: &[App],
        cables: &[Option<String>],
        present: impl Fn(&str) -> bool,
        cable: impl Fn(&str) -> bool,
        names: &[String],
        mut put: impl FnMut(u32, &str) -> Result<(), String>,
    ) -> Vec<String> {
        self.reload();
        let p = plan(&self.homes, apps, cables, present, cable, &self.done);
        let before = self.homes.clone();
        for (app, k) in p.learn {
            self.homes.retain(|a, _| !a.eq_ignore_ascii_case(&app));
            self.homes.insert(app, k);
        }
        for app in p.forget {
            self.homes.retain(|a, _| !a.eq_ignore_ascii_case(&app));
        }
        let mut notes = Vec::new();
        for (pid, k) in p.moves {
            let (Some(id), Some(a)) = (cables[k].as_deref(), apps.iter().find(|a| a.pid == pid)) else {
                continue;
            };
            let name = names.get(k).map_or(String::new(), |n| format!(" ({n})"));
            match put(pid, id) {
                Ok(()) => notes.push(format!("{} is back on strip {}{name}", a.key, k + 1)),
                Err(e) => notes.push(format!("could not put {} back on strip {}{name}: {e}", a.key, k + 1)),
            }
        }
        self.done.extend(p.done);
        // forget processes that ended, so the set stays small
        self.done.retain(|pid| apps.iter().any(|a| a.pid == *pid));
        if self.homes != before
            && let Err(e) = self.save()
        {
            notes.push(e);
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // endpoints: "spk" = speakers, "s1".."s6" = the strips' cables, "b1" = a bus cable
    fn cables() -> Vec<Option<String>> {
        (1..=6).map(|k| Some(format!("s{k}"))).collect()
    }
    fn present(id: &str) -> bool {
        id == "spk" || id == "b1" || (id.len() == 2 && id.starts_with('s'))
    }
    fn cable(id: &str) -> bool {
        id != "spk"
    }
    fn app(pid: u32, key: &str, set_to: &str) -> App {
        App { pid, key: key.into(), set_to: set_to.into() }
    }
    fn homes(v: &[(&str, usize)]) -> BTreeMap<String, usize> {
        v.iter().map(|(a, k)| (a.to_string(), *k)).collect()
    }
    fn none() -> HashSet<u32> {
        HashSet::new()
    }

    #[test]
    fn the_program_name_is_the_key() {
        assert_eq!(key(r"C:\Users\x\AppData\Local\Discord\app-1.0.9187\Discord.exe"), "Discord");
        assert_eq!(key(r"C:\Program Files\Spotify\SPOTIFY.EXE"), "SPOTIFY");
        assert_eq!(key("strawberry"), "strawberry");
        assert_eq!(key(r"C:\x\.exe"), "");
        assert_eq!(key(r"C:\x\é.exe"), "é");
    }

    #[test]
    fn an_app_put_on_a_strip_is_remembered() {
        let p = plan(&BTreeMap::new(), &[app(7, "Discord", "s2")], &cables(), present, cable, &none());
        assert_eq!(p.learn, [("Discord".to_string(), 1)]);
        assert!(p.moves.is_empty());
        assert_eq!(p.done, [7]);
    }

    #[test]
    fn following_the_default_teaches_nothing() {
        let p = plan(&BTreeMap::new(), &[app(7, "Discord", "")], &cables(), present, cable, &none());
        assert!(p.learn.is_empty() && p.forget.is_empty() && p.moves.is_empty());
    }

    /// Discord updated into a new folder: Windows forgot its device.
    #[test]
    fn an_app_on_the_default_goes_back_to_its_strip() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[app(9, "Discord", "")], &cables(), present, cable, &none());
        assert_eq!(p.moves, [(9, 1)]);
        assert_eq!(p.done, [9]);
        // the cable driver was reinstalled: its old ID is gone
        let p = plan(&h, &[app(9, "Discord", "old-s2")], &cables(), present, cable, &none());
        assert_eq!(p.moves, [(9, 1)]);
        assert!(p.forget.is_empty());
    }

    /// Review #10: endpoint IDs are compared without case.
    #[test]
    fn an_endpoint_id_in_other_case_is_the_same_device() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[app(9, "Discord", "S3")], &cables(), |id| present(&id.to_lowercase()), cable, &none());
        assert_eq!(p.learn, [("Discord".to_string(), 2)], "put on strip 3 on purpose");
        assert!(p.moves.is_empty() && p.forget.is_empty());
    }

    #[test]
    fn already_there_is_done() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[app(9, "Discord", "s2")], &cables(), present, cable, &none());
        assert!(p.moves.is_empty() && p.learn.is_empty());
        assert_eq!(p.done, [9]);
    }

    #[test]
    fn a_process_is_moved_once_and_then_left_alone() {
        let h = homes(&[("Discord", 1)]);
        let done: HashSet<u32> = [9].into();
        let p = plan(&h, &[app(9, "Discord", "")], &cables(), present, cable, &done);
        assert!(p.moves.is_empty(), "someone moved it after we did");
    }

    #[test]
    fn put_elsewhere_on_purpose_is_respected_and_forgotten() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[app(9, "Discord", "spk")], &cables(), present, cable, &none());
        assert!(p.moves.is_empty());
        assert_eq!(p.forget, ["Discord"]);
        // on another strip's cable: that strip is its home now
        let p = plan(&h, &[app(9, "Discord", "s5")], &cables(), present, cable, &none());
        assert!(p.moves.is_empty());
        assert_eq!(p.learn, [("Discord".to_string(), 4)]);
        // on a cable that is no strip's (a bus's): left there, still remembered
        let p = plan(&h, &[app(9, "Discord", "b1")], &cables(), present, cable, &none());
        assert!(p.moves.is_empty() && p.forget.is_empty() && p.learn.is_empty());
    }

    /// Two processes of one app: one put on the speakers on purpose, one
    /// following the default. The app is forgotten, so neither is moved.
    #[test]
    fn a_forgotten_app_is_not_moved_in_the_same_look() {
        let h = homes(&[("chrome", 2)]);
        let p = plan(&h, &[app(1, "chrome", "spk"), app(2, "chrome", "")], &cables(), present, cable, &none());
        assert_eq!(p.forget, ["chrome"]);
        assert!(p.moves.is_empty());
    }

    #[test]
    fn names_match_in_any_case() {
        let h = homes(&[("discord", 1)]);
        assert_eq!(plan(&h, &[app(9, "Discord", "")], &cables(), present, cable, &none()).moves, [(9, 1)]);
    }

    #[test]
    fn waits_for_its_strips_cable() {
        let h = homes(&[("Discord", 1)]);
        let mut c = cables();
        c[1] = None;
        let p = plan(&h, &[app(9, "Discord", "")], &c, present, cable, &none());
        assert!(p.moves.is_empty() && p.done.is_empty(), "looked at again once the cable is there");
    }

    #[test]
    fn other_apps_are_not_touched() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[app(3, "firefox", "")], &cables(), present, cable, &none());
        assert!(p.moves.is_empty() && p.learn.is_empty());
        assert_eq!(p.done, [3]);
    }

    #[test]
    fn the_file_round_trips() {
        let h = homes(&[("Discord", 1), ("Spotify", 2)]);
        assert_eq!(parse(&render(&h)), h);
        assert_eq!(parse("# c\r\n\r\n2 Discord\r\n9 Nope\nx Bad\n"), homes(&[("Discord", 1)]));
    }

    #[test]
    fn poll_moves_learns_and_saves() {
        let d = std::env::temp_dir().join(format!("bs-win-apphome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(FILE);
        std::fs::write(&file, "2 Discord\n").unwrap();
        let mut a = AppHomes::load(&d.join("bananasplit.ini"));
        let names: Vec<String> = ["Mic", "Discord", "Desktop", "4", "Music", "6"].map(String::from).to_vec();
        let mut put = Vec::new();
        let apps = [app(9, "Discord", ""), app(4, "Spotify", "s5")];
        let notes = a.poll(&apps, &cables(), present, cable, &names, |pid, id| {
            put.push((pid, id.to_string()));
            Ok(())
        });
        assert_eq!(put, [(9, "s2".to_string())]);
        assert_eq!(notes, ["Discord is back on strip 2 (Discord)"]);
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()), homes(&[("Discord", 1), ("Spotify", 4)]));
        // the next look moves nothing again
        let again = a.poll(&apps, &cables(), present, cable, &names, |_, _| panic!("moved twice"));
        assert!(again.is_empty());
        // a failed move is reported
        let notes =
            a.poll(&[app(11, "Discord", "")], &cables(), present, cable, &names, |_, _| Err("0x80070005".into()));
        assert_eq!(notes, ["could not put Discord back on strip 2 (Discord): 0x80070005"]);
        // edited by hand while it runs (a later modification time)
        std::fs::write(&file, "# mine\n3 Discord\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
        let mut put = Vec::new();
        a.poll(&[app(11, "Discord", "s2")], &cables(), present, cable, &names, |pid, id| {
            put.push((pid, id.to_string()));
            Ok(())
        });
        // Discord sits on strip 2's cable on purpose: that is learned, not undone
        assert!(put.is_empty());
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()), homes(&[("Discord", 1)]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
