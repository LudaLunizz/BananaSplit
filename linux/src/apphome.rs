//! Apps keep their strip when BananaSplit starts again.
//!
//! Quitting removes the virtual cables, so an app that played into one
//! (Discord into "BananaSplit-In2-Discord") loses it. When the cables come
//! back, PipeWire puts it back only if it kept its stream and remembered
//! where it was put; an app that opens a new stream (Discord does) lands on
//! the default output instead, often another strip's cable, and a recording
//! of its own strip's bus stays silent.
//!
//! So BananaSplit remembers it: an app put on a strip's cable (by a
//! channel's menu, the sound settings or the app's own device setting; its
//! stream's `target.object` names the cable) is noted in `app-strips` next
//! to the config. A stream of a noted app that is not put anywhere else is
//! moved back to its strip once, when BananaSplit first sees it (at the
//! start, and whenever the app opens a new stream). An app put anywhere
//! else on purpose (unticked in the menu, say) is forgotten, and a stream
//! that someone moves afterwards is left alone.
//!
//! An app ticked in a strip's menu is noted at once, also an app that does
//! not choose its own device (Strawberry: moving its stream sets no
//! `target.object`); moving it back to a device of the computer in the menu
//! forgets it. The file can be edited by hand too, one `<strip> <app>` per
//! line (`5 strawberry`); a change applies while BananaSplit runs.
//!
//! Apps are known by their program's file name (`Discord`), else their
//! application.name, else their stream's node.name; case does not matter.
//! The file is separate from the config, so older versions still read the
//! config.

use crate::cables;
use crate::devices::{PulseInfo, Stream};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use bananasplit::engine::NUM_STRIPS;

const FILE: &str = "app-strips";

/// The name an app is remembered by.
pub fn key(s: &Stream) -> &str {
    if s.binary.is_empty() { &s.app } else { &s.binary }
}

/// The strip `app` is noted on (the name in any case).
fn home_of(homes: &BTreeMap<String, usize>, app: &str) -> Option<usize> {
    homes.get(app).or_else(|| homes.iter().find(|(a, _)| a.eq_ignore_ascii_case(app)).map(|(_, k)| k)).copied()
}

/// What to do after one look at the apps' streams.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// (app, strip) to remember.
    pub learn: Vec<(String, usize)>,
    /// Apps put somewhere else on purpose.
    pub forget: Vec<String>,
    /// (stream id, strip) to move back.
    pub moves: Vec<(u32, usize)>,
    /// Streams looked at for good (moved, or nothing to do for them).
    pub done: Vec<u32>,
}

/// Decide for `streams` (the apps that play). `sink` gives a sink index's
/// node.name; `cables[k]` is strip k's cable node.name while the cable is
/// there; `ours` tells whether a node.name is one of BananaSplit's cables;
/// `done` are the streams already looked at.
pub fn plan(
    homes: &BTreeMap<String, usize>,
    streams: &[Stream],
    sink: impl Fn(u32) -> Option<String>,
    cables: &[Option<String>],
    ours: impl Fn(&str) -> bool,
    done: &HashSet<u32>,
) -> Plan {
    let strip_of = |node: &str| cables.iter().position(|c| c.as_deref() == Some(node));
    let mut p = Plan::default();
    for s in streams {
        let app = key(s);
        let now = sink(s.target);
        if !s.pinned.is_empty() && now.as_deref() == Some(s.pinned.as_str()) {
            // playing where someone put it
            match strip_of(&s.pinned) {
                Some(k) => {
                    if home_of(homes, app) != Some(k) && !p.learn.iter().any(|(a, _)| a == app) {
                        p.learn.push((app.to_string(), k));
                    }
                }
                None if !ours(&s.pinned) && home_of(homes, app).is_some() && !p.forget.iter().any(|a| a == app) => p.forget.push(app.to_string()),
                None => {}
            }
        }
        if done.contains(&s.id) {
            continue;
        }
        let Some(k) = home_of(homes, app) else {
            p.done.push(s.id);
            continue;
        };
        let Some(cable) = cables.get(k).and_then(Option::as_deref) else {
            continue; // its strip's cable is not there (yet): look again later
        };
        if now.as_deref() == Some(cable) {
            p.done.push(s.id);
        } else if s.pinned.is_empty() || s.pinned == cable || (ours(&s.pinned) && strip_of(&s.pinned).is_none()) {
            // following the default, or put on its cable (or one of ours that
            // is gone) and not on it
            p.moves.push((s.id, k));
            p.done.push(s.id);
        } else {
            p.done.push(s.id); // put elsewhere on purpose
        }
    }
    p
}

/// Parse `app-strips`: lines "<strip 1-6> <app>"; `#` starts a comment line.
pub fn parse(text: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some((n, app)) = l.split_once(char::is_whitespace)
            && let Ok(n @ 1..=NUM_STRIPS) = n.parse::<usize>() {
                m.insert(app.trim().to_string(), n - 1);
            }
    }
    m
}

pub fn render(m: &BTreeMap<String, usize>) -> String {
    let mut o = String::from(
        "# Apps BananaSplit puts on their strip whenever they start playing (strip number, app).\n\
         # Ticking an app in a strip's menu adds it; moving it back to a device of the computer removes it.\n\
         # You can edit this file: one \"<strip 1-6> <app>\" per line, e.g. \"5 strawberry\".\n",
    );
    for (app, k) in m {
        o.push_str(&format!("{} {app}\n", k + 1));
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
            self.done.clear(); // apply the new list to the streams that play now
        }
    }

    fn save(&mut self) -> Result<(), String> {
        std::fs::write(&self.path, render(&self.homes)).map_err(|e| format!("cannot save {}: {e}", self.path.display()))?;
        self.stamp = mtime(&self.path);
        Ok(())
    }

    /// The menu put `app` on strip `strip` (`None`: on a device of the
    /// computer): note it, or forget it.
    pub fn set(&mut self, app: &str, strip: Option<usize>) -> Result<(), String> {
        self.reload();
        let before = self.homes.clone();
        self.homes.retain(|a, _| !a.eq_ignore_ascii_case(app));
        if let Some(k) = strip.filter(|k| *k < NUM_STRIPS) {
            self.homes.insert(app.to_string(), k);
        }
        if self.homes == before { Ok(()) } else { self.save() }
    }

    /// Look at the latest report; move apps back to their strips. Returns
    /// a line for the user per app moved.
    pub fn poll(&mut self, info: &PulseInfo, set: &cables::CableSet, live: &cables::Cables, names: &[String]) -> Vec<String> {
        let Some(d) = info.devices.as_ref() else { return Vec::new() };
        self.reload();
        let present = |n: &str| d.list.iter().any(|x| !x.is_source && x.name == n);
        let strips: Vec<Option<String>> =
            (0..NUM_STRIPS).map(|k| live.live(k).map(|c| c.node_name.clone()).filter(|n| present(n))).collect();
        let sink = |i: u32| info.node_name(i, false).map(str::to_string);
        let ours = |n: &str| set.all().any(|c| c.node_name == n);
        let p = plan(&self.homes, &info.playing, sink, &strips, ours, &self.done);
        let before = self.homes.clone();
        for (app, k) in p.learn {
            self.homes.retain(|a, _| !a.eq_ignore_ascii_case(&app));
            self.homes.insert(app, k);
        }
        for app in p.forget {
            self.homes.retain(|a, _| !a.eq_ignore_ascii_case(&app));
        }
        let mut notes = Vec::new();
        for (id, k) in p.moves {
            let (Some(node), Some(s)) = (strips[k].as_deref(), info.playing.iter().find(|s| s.id == id)) else { continue };
            let name = names.get(k).map_or(String::new(), |n| format!(" ({n})"));
            match cables::pactl(&["move-sink-input", &id.to_string(), node]) {
                Ok(_) => notes.push(format!("{} is back on strip {}{name}", key(s), k + 1)),
                Err(e) => notes.push(format!("could not put {} back on strip {}{name}: {e}", key(s), k + 1)),
            }
        }
        self.done.extend(p.done);
        // forget streams that ended, so the set stays small
        self.done.retain(|id| info.playing.iter().any(|s| s.id == *id));
        if self.homes != before
            && let Err(e) = self.save() {
                notes.push(e);
            }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(id: u32, binary: &str, target: u32, pinned: &str) -> Stream {
        Stream {
            id,
            app: "WEBRTC VoiceEngine".into(),
            media: String::new(),
            binary: binary.into(),
            node: String::new(),
            desc: String::new(),
            nick: String::new(),
            target,
            pinned: pinned.into(),
        }
    }

    // sinks: 50 = speakers, 51..56 = bananasplit_in1..in6
    fn sink(i: u32) -> Option<String> {
        match i {
            50 => Some("speakers".into()),
            51..=56 => Some(format!("bananasplit_in{}", i - 50)),
            _ => None,
        }
    }
    fn cables() -> Vec<Option<String>> {
        (1..=6).map(|k| Some(format!("bananasplit_in{k}"))).collect()
    }
    fn ours(n: &str) -> bool {
        n.starts_with("bananasplit_")
    }
    fn homes(v: &[(&str, usize)]) -> BTreeMap<String, usize> {
        v.iter().map(|(a, k)| (a.to_string(), *k)).collect()
    }

    #[test]
    fn an_app_put_on_a_strip_is_remembered() {
        let p = plan(&BTreeMap::new(), &[st(7, "Discord", 52, "bananasplit_in2")], sink, &cables(), ours, &HashSet::new());
        assert_eq!(p.learn, [("Discord".to_string(), 1)]);
        assert!(p.moves.is_empty());
        assert_eq!(p.done, [7]);
    }

    #[test]
    fn following_the_default_teaches_nothing() {
        let p = plan(&BTreeMap::new(), &[st(7, "Discord", 53, "")], sink, &cables(), ours, &HashSet::new());
        assert!(p.learn.is_empty() && p.forget.is_empty() && p.moves.is_empty());
    }

    /// The 2.8.6 update: Discord's new stream followed the default output
    /// (strip 3's cable), so the recording of its strip's bus was silent.
    #[test]
    fn a_new_stream_on_the_default_goes_back_to_its_strip() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[st(9, "Discord", 53, "")], sink, &cables(), ours, &HashSet::new());
        assert_eq!(p.moves, [(9, 1)]);
        assert_eq!(p.done, [9]);
        // also when it waits on the speakers, or still names its old cable
        assert_eq!(plan(&h, &[st(9, "Discord", 50, "")], sink, &cables(), ours, &HashSet::new()).moves, [(9, 1)]);
        assert_eq!(plan(&h, &[st(9, "Discord", 50, "bananasplit_in2")], sink, &cables(), ours, &HashSet::new()).moves, [(9, 1)]);
    }

    #[test]
    fn a_stream_is_moved_once_and_then_left_alone() {
        let h = homes(&[("Discord", 1)]);
        let done: HashSet<u32> = [9].into();
        let p = plan(&h, &[st(9, "Discord", 53, "")], sink, &cables(), ours, &done);
        assert!(p.moves.is_empty(), "someone moved it after we did");
    }

    #[test]
    fn put_elsewhere_on_purpose_is_respected_and_forgotten() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[st(9, "Discord", 50, "speakers")], sink, &cables(), ours, &HashSet::new());
        assert!(p.moves.is_empty());
        assert_eq!(p.forget, ["Discord"]);
        // on another strip's cable on purpose: that strip is its home now
        let p = plan(&h, &[st(9, "Discord", 55, "bananasplit_in5")], sink, &cables(), ours, &HashSet::new());
        assert!(p.moves.is_empty());
        assert_eq!(p.learn, [("Discord".to_string(), 4)]);
    }

    /// Strawberry names no device of its own: its stream has no
    /// `target.object`, and only its node.name says who it is.
    fn strawberry(id: u32, target: u32) -> Stream {
        Stream { app: "strawberry".into(), node: "strawberry".into(), ..st(id, "", target, "") }
    }

    #[test]
    fn an_app_written_by_hand_goes_to_its_strip_in_any_case() {
        let h = homes(&[("Strawberry", 4)]);
        let p = plan(&h, &[strawberry(5, 53)], sink, &cables(), ours, &HashSet::new());
        assert_eq!(p.moves, [(5, 4)]);
        assert!(p.learn.is_empty() && p.forget.is_empty());
        // already there: nothing to do
        assert!(plan(&h, &[strawberry(5, 55)], sink, &cables(), ours, &HashSet::new()).moves.is_empty());
    }

    #[test]
    fn the_menu_notes_and_forgets_apps_and_hand_edits_apply() {
        let d = std::env::temp_dir().join(format!("bs-apphome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(FILE);
        std::fs::write(&file, "2 Discord\n").unwrap();
        let mut a = AppHomes::load(&d.join("bananasplit.ini"));
        a.set("strawberry", Some(4)).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(parse(&text), homes(&[("Discord", 1), ("strawberry", 4)]), "{text}");
        assert!(text.starts_with('#'), "explains itself: {text}");
        // moved to another strip, then back to the computer's device
        a.set("Strawberry", Some(2)).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()), homes(&[("Discord", 1), ("Strawberry", 2)]));
        a.set("strawberry", None).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()), homes(&[("Discord", 1)]));
        // edited by hand while it runs (a later modification time)
        std::fs::write(&file, "# mine\n5 strawberry\n2 Discord\nnot a line\n7 too high\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
        a.done.insert(99);
        a.reload();
        assert_eq!(a.homes, homes(&[("Discord", 1), ("strawberry", 4)]));
        assert!(a.done.is_empty(), "the new list applies to the streams playing now");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn waits_for_its_strips_cable() {
        let h = homes(&[("Discord", 1)]);
        let mut c = cables();
        c[1] = None;
        let p = plan(&h, &[st(9, "Discord", 50, "")], sink, &c, ours, &HashSet::new());
        assert!(p.moves.is_empty() && p.done.is_empty(), "looked at again once the cable is there");
    }

    #[test]
    fn other_apps_are_not_touched() {
        let h = homes(&[("Discord", 1)]);
        let p = plan(&h, &[st(3, "firefox", 53, "")], sink, &cables(), ours, &HashSet::new());
        assert!(p.moves.is_empty() && p.learn.is_empty());
        assert_eq!(p.done, [3]);
    }

    #[test]
    fn the_file_round_trips() {
        let h = homes(&[("Discord", 1), ("WEBRTC VoiceEngine", 5)]);
        assert_eq!(parse(&render(&h)), h);
        assert_eq!(parse("# c\n\n2 Discord\n9 Nope\nx Bad\n"), homes(&[("Discord", 1)]));
    }
}
