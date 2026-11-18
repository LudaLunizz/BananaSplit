//! Recording apps keep their bus when BananaSplit starts again.
//!
//! The counterpart of [`crate::apphome`] for the apps that *record*: Vesktop
//! records two things (your microphone, and the sound of a screen share), and
//! the sound server's own memory of where an app was put is keyed by the
//! program's name, so it cannot keep two streams of one program apart. When
//! BananaSplit restarts, its virtual microphones go away and come back, and
//! the stream that was on B2 falls back to the default microphone (B1).
//!
//! So BananaSplit remembers it in `app-buses` next to the config, one rule
//! per line: `<bus> <app>`, or `<bus> <app> @ <what the stream asks for>`
//! when one program has several streams. The second part is the stream's
//! `target.object`, the node the program itself asked for and that does not
//! change when the stream is moved: Vesktop's microphone asks for the cable
//! you picked in its settings, its screen share asks for
//! `vencord-screen-share`. For example:
//!
//! ```text
//! B2 vesktop @ vencord-screen-share
//! ```
//!
//! A stream that matches a rule is moved to that bus's virtual microphone
//! once, when BananaSplit first sees it (at the start, and whenever the app
//! opens a new stream); a stream that someone moves afterwards is left alone.
//! A stream that follows the default (or whose own cable is gone) is moved
//! by a rule without `@`; a stream put on one of BananaSplit's live
//! microphones by the program itself is left where it chose to be, unless a
//! rule names exactly what it asks for. Ticking an app in a bus's menu
//! writes the rule at once, unticking it takes the rule out; the file can be
//! edited by hand, and a change applies while BananaSplit runs.
//!
//! Apps are known as in `app-strips`: the program's file name, else the
//! application.name, else the node.name; case does not matter.

use crate::apphome::key;
use crate::cables;
use crate::devices::{PulseInfo, Stream};
use bananasplit::engine::{BUS_IDS, NUM_BUSES, NUM_STRIPS};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const FILE: &str = "app-buses";

/// One line of `app-buses`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The bus (0 = V1 … 4 = B2).
    pub bus: usize,
    pub app: String,
    /// The node the stream asks for (`target.object`); empty: any stream of the app.
    pub asks: String,
}

impl Rule {
    fn same_stream(&self, app: &str, asks: &str) -> bool {
        self.app.eq_ignore_ascii_case(app) && self.asks == asks
    }
}

/// Parse `app-buses`: `<bus> <app>` or `<bus> <app> @ <asks>`; `#` starts a
/// comment line. A line that is not one of those is skipped. A later line
/// for the same app and stream replaces an earlier one.
pub fn parse(text: &str) -> Vec<Rule> {
    let mut rules: Vec<Rule> = Vec::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((bus, rest)) = l.split_once(char::is_whitespace) else { continue };
        let Some(bus) = BUS_IDS.iter().position(|b| b.eq_ignore_ascii_case(bus)) else { continue };
        let (app, asks) = match rest.split_once(" @ ") {
            Some((a, t)) => (a.trim(), t.trim()),
            None => (rest.trim(), ""),
        };
        if app.is_empty() || app.contains(" @") {
            continue;
        }
        rules.retain(|r| !r.same_stream(app, asks));
        rules.push(Rule { bus, app: app.to_string(), asks: asks.to_string() });
    }
    rules
}

pub fn render(rules: &[Rule]) -> String {
    let mut o = String::from(
        "# Apps BananaSplit puts on a bus's virtual microphone whenever they start recording (bus, app).\n\
         # Ticking an app in a bus's menu adds it; moving it back to the default microphone removes it.\n\
         # You can edit this file: one \"<bus V1-V3, B1, B2> <app>\" per line, e.g. \"B2 obs\".\n\
         # A program with two streams (Vesktop: microphone and screen share) names the one it means\n\
         # by what the stream asks for: \"B2 vesktop @ vencord-screen-share\".\n",
    );
    for r in rules {
        if r.asks.is_empty() {
            o.push_str(&format!("{} {}\n", BUS_IDS[r.bus], r.app));
        } else {
            o.push_str(&format!("{} {} @ {}\n", BUS_IDS[r.bus], r.app, r.asks));
        }
    }
    o
}

/// The rule for stream `s`: the one that names what it asks for, else the
/// one for the app alone.
fn rule_for<'a>(rules: &'a [Rule], s: &Stream) -> Option<&'a Rule> {
    let app = key(s);
    let named = rules.iter().find(|r| !r.asks.is_empty() && r.same_stream(app, &s.pinned));
    named.or_else(|| rules.iter().find(|r| r.asks.is_empty() && r.app.eq_ignore_ascii_case(app)))
}

/// What to do after one look at the apps' streams: (stream id, bus) to move,
/// and the streams looked at for good.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub moves: Vec<(u32, usize)>,
    pub done: Vec<u32>,
}

/// Decide for `streams` (the apps that record). `source` gives a source
/// index's node.name; `cables[b]` is bus `b`'s virtual microphone node.name
/// while it is there; `ours` tells whether a node.name is one of
/// BananaSplit's cables; `done` are the streams already looked at.
pub fn plan(
    rules: &[Rule],
    streams: &[Stream],
    source: impl Fn(u32) -> Option<String>,
    cables: &[Option<String>],
    ours: impl Fn(&str) -> bool,
    done: &HashSet<u32>,
) -> Plan {
    let live_bus = |node: &str| cables.iter().any(|c| c.as_deref() == Some(node));
    let mut p = Plan::default();
    for s in streams {
        if done.contains(&s.id) {
            continue;
        }
        let Some(rule) = rule_for(rules, s) else {
            p.done.push(s.id);
            continue;
        };
        let Some(cable) = cables.get(rule.bus).and_then(Option::as_deref) else {
            continue; // its bus's microphone is not there (yet): look again later
        };
        if source(s.target).as_deref() == Some(cable) {
            p.done.push(s.id);
            continue;
        }
        // A rule that names what the stream asks for is the user's word. A rule
        // for the app alone moves a stream that follows the default or whose
        // own cable is gone, but not one the program put on another live
        // microphone of ours, or on some other device, on purpose.
        let movable = !rule.asks.is_empty()
            || s.pinned.is_empty()
            || s.pinned == cable
            || (ours(&s.pinned) && !live_bus(&s.pinned));
        if movable {
            p.moves.push((s.id, rule.bus));
        }
        p.done.push(s.id);
    }
    p
}

pub struct AppBuses {
    path: PathBuf,
    rules: Vec<Rule>,
    /// The file's modification time when it was last read or written.
    stamp: Option<SystemTime>,
    done: HashSet<u32>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl AppBuses {
    /// The rules next to the config file `config`.
    pub fn load(config: &Path) -> AppBuses {
        let path = config.parent().map_or_else(|| PathBuf::from(FILE), |d| d.join(FILE));
        let rules = std::fs::read_to_string(&path).map(|t| parse(&t)).unwrap_or_default();
        let stamp = mtime(&path);
        AppBuses { path, rules, stamp, done: HashSet::new() }
    }

    /// Read the file again if someone edited it.
    fn reload(&mut self) {
        let now = mtime(&self.path);
        if now != self.stamp {
            self.rules = std::fs::read_to_string(&self.path).map(|t| parse(&t)).unwrap_or_default();
            self.stamp = now;
            self.done.clear(); // apply the new list to the streams that record now
        }
    }

    fn save(&mut self) -> Result<(), String> {
        std::fs::write(&self.path, render(&self.rules))
            .map_err(|e| format!("cannot save {}: {e}", self.path.display()))?;
        self.stamp = mtime(&self.path);
        Ok(())
    }

    /// The menu put the stream of `app` that asks for `asks` on bus `bus`
    /// (`None`: back on the default microphone): note it, or forget it.
    pub fn set(&mut self, app: &str, asks: &str, bus: Option<usize>) -> Result<(), String> {
        self.reload();
        let before = self.rules.clone();
        self.rules.retain(|r| !r.same_stream(app, asks));
        if let Some(bus) = bus.filter(|b| *b < NUM_BUSES) {
            self.rules.push(Rule { bus, app: app.to_string(), asks: asks.to_string() });
        }
        if self.rules == before { Ok(()) } else { self.save() }
    }

    /// Look at the latest report; move recording apps to their buses.
    /// Returns a line for the user per stream moved.
    pub fn poll(
        &mut self,
        info: &PulseInfo,
        set: &cables::CableSet,
        live: &cables::Cables,
        names: &[String],
    ) -> Vec<String> {
        let Some(d) = info.devices.as_ref() else { return Vec::new() };
        self.reload();
        let present = |n: &str| d.list.iter().any(|x| x.is_source && x.name == n);
        let buses: Vec<Option<String>> = (0..NUM_BUSES)
            .map(|b| live.live(NUM_STRIPS + b).map(|c| c.node_name.clone()).filter(|n| present(n)))
            .collect();
        let source = |i: u32| info.node_name(i, true).map(str::to_string);
        let ours = |n: &str| set.all().any(|c| c.node_name == n);
        let p = plan(&self.rules, &info.recording, source, &buses, ours, &self.done);
        let mut notes = Vec::new();
        for (id, b) in p.moves {
            let (Some(node), Some(s)) = (buses[b].as_deref(), info.recording.iter().find(|s| s.id == id)) else {
                continue;
            };
            let name = names.get(NUM_STRIPS + b).map_or(String::new(), |n| format!(" ({n})"));
            match cables::pactl(&["move-source-output", &id.to_string(), node]) {
                Ok(_) => notes.push(format!("{} is back on {}{name}", key(s), BUS_IDS[b])),
                Err(e) => notes.push(format!("could not put {} back on {}{name}: {e}", key(s), BUS_IDS[b])),
            }
        }
        self.done.extend(p.done);
        // forget streams that ended, so the set stays small
        self.done.retain(|id| info.recording.iter().any(|s| s.id == *id));
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(id: u32, binary: &str, target: u32, pinned: &str) -> Stream {
        Stream {
            id,
            app: "vesktop".into(),
            media: "RecordStream".into(),
            binary: binary.into(),
            node: "vesktop".into(),
            desc: String::new(),
            nick: String::new(),
            target,
            pinned: pinned.into(),
        }
    }

    // sources: 60 = the default microphone, 61 = bananasplit_out_b1, 62 = bananasplit_out_b2
    fn source(i: u32) -> Option<String> {
        match i {
            60 => Some("mic".into()),
            61 => Some("bananasplit_out_b1".into()),
            62 => Some("bananasplit_out_b2".into()),
            _ => None,
        }
    }
    /// V1 .. B2 virtual microphones; V1-V3 have none, B1 and B2 are there.
    fn cables() -> Vec<Option<String>> {
        vec![None, None, None, Some("bananasplit_out_b1".into()), Some("bananasplit_out_b2".into())]
    }
    fn ours(n: &str) -> bool {
        n.starts_with("bananasplit_")
    }
    fn rules(text: &str) -> Vec<Rule> {
        parse(text)
    }
    fn go(r: &[Rule], s: &[Stream]) -> Plan {
        plan(r, s, source, &cables(), ours, &HashSet::new())
    }

    #[test]
    fn the_file_is_read_and_written_back() {
        let r =
            rules("# mine\nb2 vesktop @ vencord-screen-share\nB1 OBS Studio\nV9 nothing\nB2\nB2 x @ \nbogus line\n");
        assert_eq!(
            r,
            [
                Rule { bus: 4, app: "vesktop".into(), asks: "vencord-screen-share".into() },
                Rule { bus: 3, app: "OBS Studio".into(), asks: String::new() },
            ]
        );
        let text = render(&r);
        assert!(text.starts_with('#'), "explains itself: {text}");
        assert_eq!(parse(&text), r, "{text}");
        // the same app and stream again: the later line wins
        assert_eq!(rules("B1 obs\nB2 OBS\n"), [Rule { bus: 4, app: "OBS".into(), asks: String::new() }]);
    }

    /// The restart of 2.9.5: Vesktop's screen share asks for
    /// `vencord-screen-share`, its microphone for B1's cable; after the
    /// restart both record B1's microphone, and only the screen share is moved.
    #[test]
    fn the_screen_share_goes_back_to_b2_and_the_microphone_stays() {
        let r = rules("B2 vesktop @ vencord-screen-share\n");
        let mic = st(467, "vesktop", 61, "bananasplit_out_b1");
        let share = st(42461, "vesktop", 61, "vencord-screen-share");
        let p = go(&r, &[mic, share]);
        assert_eq!(p.moves, [(42461, 4)]);
        assert_eq!(p.done, [467, 42461]);
    }

    #[test]
    fn a_stream_that_is_already_there_is_left_and_moved_once() {
        let r = rules("B2 vesktop @ vencord-screen-share\n");
        let share = st(7, "vesktop", 62, "vencord-screen-share");
        let p = go(&r, std::slice::from_ref(&share));
        assert!(p.moves.is_empty());
        assert_eq!(p.done, [7]);
        // moved once; someone moved it away afterwards: left alone
        let away = st(7, "vesktop", 60, "vencord-screen-share");
        let done: HashSet<u32> = [7].into();
        assert!(plan(&r, &[away], source, &cables(), ours, &done).moves.is_empty());
    }

    #[test]
    fn a_rule_without_asks_moves_a_stream_that_follows_the_default() {
        let r = rules("B2 obs\n");
        let obs =
            |pinned: &str, target| Stream { app: "OBS".into(), binary: "obs".into(), ..st(3, "obs", target, pinned) };
        assert_eq!(go(&r, &[obs("", 60)]).moves, [(3, 4)]);
        // its own cable is gone (a name of ours that is no microphone now)
        assert_eq!(go(&r, &[obs("bananasplit_out_v1", 60)]).moves, [(3, 4)]);
        // it asked for a live microphone of ours on purpose (B1): left there
        let p = go(&r, &[obs("bananasplit_out_b1", 61)]);
        assert!(p.moves.is_empty());
        assert_eq!(p.done, [3]);
        // it asked for some other device on purpose: left alone too
        assert!(go(&r, &[obs("alsa_input.usb-mic", 60)]).moves.is_empty());
    }

    #[test]
    fn an_app_without_a_rule_and_a_bus_without_a_microphone_wait_or_pass() {
        let r = rules("B2 vesktop @ vencord-screen-share\nV1 obs\n");
        // no rule for it: nothing to do, never looked at again
        let p = go(&r, &[st(9, "discord", 60, "")]);
        assert!(p.moves.is_empty());
        assert_eq!(p.done, [9]);
        // V1 has no virtual microphone now: look again later
        let obs = Stream { binary: "obs".into(), app: "OBS".into(), ..st(3, "obs", 60, "") };
        let p = go(&r, &[obs]);
        assert!(p.moves.is_empty() && p.done.is_empty());
    }

    #[test]
    fn the_menu_notes_and_forgets_streams_and_hand_edits_apply() {
        let d = std::env::temp_dir().join(format!("bs-apprec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(FILE);
        std::fs::write(&file, "B1 obs\n").unwrap();
        let mut a = AppBuses::load(&d.join("bananasplit.ini"));
        a.set("vesktop", "vencord-screen-share", Some(4)).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            parse(&text),
            [
                Rule { bus: 3, app: "obs".into(), asks: String::new() },
                Rule { bus: 4, app: "vesktop".into(), asks: "vencord-screen-share".into() },
            ],
            "{text}"
        );
        // the microphone stream of the same app is its own rule
        a.set("vesktop", "bananasplit_out_b1", Some(3)).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()).len(), 3);
        // moved to another bus, then back to the default microphone
        a.set("Vesktop", "vencord-screen-share", Some(2)).unwrap();
        assert!(
            parse(&std::fs::read_to_string(&file).unwrap())
                .iter()
                .any(|r| r.bus == 2 && r.asks == "vencord-screen-share")
        );
        a.set("vesktop", "vencord-screen-share", None).unwrap();
        a.set("vesktop", "bananasplit_out_b1", None).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(&file).unwrap()).len(), 1);
        // edited by hand while it runs (a later modification time)
        std::fs::write(&file, "# mine\nB2 vesktop @ vencord-screen-share\nnot a line\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
        a.done.insert(99);
        a.reload();
        assert_eq!(a.rules.len(), 1);
        assert!(a.done.is_empty(), "streams are looked at again");
        let _ = std::fs::remove_dir_all(&d);
    }
}
