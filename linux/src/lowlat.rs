//! `bananasplit --low-latency`: install the PipeWire low-latency settings from
//! `contrib/pipewire` (embedded in the binary) and apply them right away,
//! plus the one-time startup advice when the round trip is too long.
//!
//! Files go to `$XDG_CONFIG_HOME/pipewire/{pipewire,pipewire-pulse}.conf.d/`
//! (`~/.config/...`). An existing file with other contents is never
//! overwritten or removed. The graph settings are also applied to the
//! running PipeWire with `pw-metadata -n settings`, so no restart is needed
//! for the quantum; the pipewire-pulse part applies when it restarts.
//!
//! Up to 2.4 this program was called vmix and installed the same settings
//! as `10-vmix-lowlatency.conf`. Those files (only if unchanged) count as
//! installed, are replaced by `--low-latency` and removed by `--low-latency off`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const FILE: &str = "10-bananasplit-lowlatency.conf";
pub const PW_CONF: &str = include_str!("../contrib/pipewire/pipewire.conf.d/10-bananasplit-lowlatency.conf");
pub const PULSE_CONF: &str = include_str!("../contrib/pipewire/pipewire-pulse.conf.d/10-bananasplit-lowlatency.conf");
/// What vmix 2.4 and older installed (same settings, other name and comments).
pub const LEGACY_FILE: &str = "10-vmix-lowlatency.conf";
const LEGACY_PW_CONF: &str = include_str!("legacy/10-vmix-lowlatency.pipewire.conf");
const LEGACY_PULSE_CONF: &str = include_str!("legacy/10-vmix-lowlatency.pipewire-pulse.conf");
/// Period pinned by `PW_CONF` (frames at 48 kHz).
pub const QUANTUM: u32 = 128;
/// Live settings, in the order they are applied (min first, so max >= min).
const LIVE: [(&str, &str); 3] = [("clock.min-quantum", "64"), ("clock.max-quantum", "128"), ("clock.quantum", "128")];
pub const RESTART: &str = "systemctl --user restart pipewire pipewire-pulse wireplumber";

/// `$XDG_CONFIG_HOME/pipewire` or `~/.config/pipewire`.
pub fn pw_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pipewire")
}

fn targets(dir: &Path) -> [(PathBuf, &'static str); 2] {
    [(dir.join("pipewire.conf.d").join(FILE), PW_CONF), (dir.join("pipewire-pulse.conf.d").join(FILE), PULSE_CONF)]
}

fn legacy_targets(dir: &Path) -> [(PathBuf, &'static str); 2] {
    [
        (dir.join("pipewire.conf.d").join(LEGACY_FILE), LEGACY_PW_CONF),
        (dir.join("pipewire-pulse.conf.d").join(LEGACY_FILE), LEGACY_PULSE_CONF),
    ]
}

/// Is `path` exactly `want`?
fn is(path: &Path, want: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s == want)
}

/// Remove `path` if it is exactly vmix's file; one line for the report.
fn remove_legacy(path: &Path, want: &str) -> Result<Option<String>, String> {
    if !is(path, want) {
        return Ok(None);
    }
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(format!("removed {} (installed by vmix)", path.display())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Missing,
    Installed,
    /// The graph file exists but is not BananaSplit's version.
    Changed,
}

/// Is the graph part (the one that matters for latency) installed?
pub fn state(dir: &Path) -> State {
    let [(path, want), _] = targets(dir);
    let [(old, old_want), _] = legacy_targets(dir);
    match std::fs::read_to_string(path) {
        Ok(s) if s == want => State::Installed,
        Ok(_) => State::Changed,
        Err(_) if is(&old, old_want) => State::Installed,
        Err(_) => State::Missing,
    }
}

/// Write both files (replacing vmix's). Returns one line per file.
pub fn install(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for ((path, want), (old, old_want)) in targets(dir).into_iter().zip(legacy_targets(dir)) {
        match std::fs::read_to_string(&path) {
            Ok(s) if s == want => out.push(format!("already installed: {}", path.display())),
            Ok(_) => {
                out.push(format!(
                    "left alone (not BananaSplit's version, compare it with contrib/pipewire): {}",
                    path.display()
                ));
                continue; // keep vmix's file too: the user manages these
            }
            Err(_) => {
                if let Some(p) = path.parent() {
                    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
                }
                std::fs::write(&path, want).map_err(|e| format!("{}: {e}", path.display()))?;
                out.push(format!("installed {}", path.display()));
            }
        }
        out.extend(remove_legacy(&old, old_want)?);
    }
    Ok(out)
}

/// Remove both files (and vmix's), only if they are exactly the versions
/// this program wrote.
pub fn remove(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for ((path, want), (old, old_want)) in targets(dir).into_iter().zip(legacy_targets(dir)) {
        let legacy = remove_legacy(&old, old_want)?;
        match std::fs::read_to_string(&path) {
            Ok(s) if s == want => {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                out.push(format!("removed {}", path.display()));
            }
            Ok(_) => out.push(format!("left alone (not BananaSplit's version): {}", path.display())),
            Err(_) if legacy.is_some() => {}
            Err(_) => out.push(format!("not installed: {}", path.display())),
        }
        out.extend(legacy);
    }
    Ok(out)
}

fn pw_metadata(args: &[&str]) -> Result<(), String> {
    let st = Command::new("pw-metadata")
        .args(["-n", "settings"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run pw-metadata ({e})"))?;
    if st.success() { Ok(()) } else { Err(format!("pw-metadata {} failed ({st})", args.join(" "))) }
}

/// Apply (or, with `on = false`, drop) the graph settings on the running
/// PipeWire, without a restart.
pub fn apply_live(on: bool) -> Result<(), String> {
    for (k, v) in LIVE {
        if on { pw_metadata(&["0", k, v])? } else { pw_metadata(&["-d", "0", k])? }
    }
    Ok(())
}

/// `bananasplit --low-latency [off]`.
pub fn command(on: bool) -> Result<(), String> {
    let dir = pw_dir();
    let lines = if on { install(&dir)? } else { remove(&dir)? };
    for l in lines {
        println!("{l}");
    }
    match apply_live(on) {
        Ok(()) if on => {
            println!(
                "applied to the running PipeWire: {QUANTUM}-frame periods ({:.2} ms at 48 kHz)",
                QUANTUM as f64 / 48.0
            );
            println!("the smaller default buffers for browsers/Discord apply after: {RESTART}");
        }
        Ok(()) => println!("the running PipeWire is back to its configured periods"),
        Err(e) => println!("could not apply it live ({e}); to apply it now: {RESTART}  (briefly interrupts all audio)"),
    }
    Ok(())
}

/// What BananaSplit knows about its latency, a moment after starting.
pub struct Facts {
    pub sample_rate: u32,
    /// Current period (frames).
    pub period: u32,
    /// `buffer_size` from the config (0 = none requested).
    pub requested: u32,
    /// Measured capture / playback latency (frames; 0 = not measured).
    pub lat_in: u32,
    pub lat_out: u32,
    /// `Some(false)` = plain JACK2, `None` = unknown.
    pub pipewire: Option<bool>,
    pub state: State,
}

/// Round trip in ms: measured if possible, else two periods.
pub fn round_trip_ms(sample_rate: u32, period: u32, lat_in: u32, lat_out: u32) -> f64 {
    let sr = sample_rate.max(1) as f64;
    if lat_in > 0 || lat_out > 0 { (lat_in + lat_out) as f64 * 1000.0 / sr } else { 2.0 * period as f64 * 1000.0 / sr }
}

/// One line of advice if the round trip is 10 ms or more, or the period is
/// larger than the config asks for. Short enough for the terminal UI's
/// 80 columns, with the fix in it.
pub fn advice(f: &Facts) -> Option<String> {
    let rtt = round_trip_ms(f.sample_rate, f.period, f.lat_in, f.lat_out);
    let too_big = f.requested > 0 && f.period > f.requested;
    if rtt < 10.0 && !too_big {
        return None;
    }
    let now = format!("round trip {rtt:.1} ms at {} frames", f.period);
    Some(if f.pipewire == Some(false) {
        let p = if f.requested > 0 { f.requested } else { QUANTUM };
        format!("{now}: start jackd with -p {p}")
    } else {
        match f.state {
            State::Missing => format!("{now} (goal < 10 ms): `bananasplit --low-latency`"),
            State::Changed => format!("{now}; {FILE} was changed"),
            State::Installed if f.period > QUANTUM => {
                format!("{}-frame periods despite the low-latency config: restart PipeWire", f.period)
            }
            State::Installed => {
                let extra = rtt - 2.0 * f.period as f64 * 1000.0 / f.sample_rate.max(1) as f64;
                format!("{now}; the sound card adds {extra:.1} ms (see README)")
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(period: u32, lat_in: u32, lat_out: u32, state: State) -> Facts {
        Facts { sample_rate: 48000, period, requested: 128, lat_in, lat_out, pipewire: Some(true), state }
    }

    #[test]
    fn advice_cases() {
        // 128 frames, 8 ms measured: nothing to say
        assert_eq!(advice(&facts(128, 128, 256, State::Missing)), None);
        // the review's box: 17.33 ms round trip, config not installed
        let a = advice(&facts(128, 416, 416, State::Missing)).unwrap();
        assert_eq!(a, "round trip 17.3 ms at 128 frames (goal < 10 ms): `bananasplit --low-latency`");
        // 1024-frame default quantum, not measured yet: ~42.7 ms
        let a = advice(&facts(1024, 0, 0, State::Missing)).unwrap();
        assert!(a.contains("1024 frames") && a.contains("42.7 ms"), "{a}");
        // installed but not active
        assert!(advice(&facts(1024, 0, 0, State::Installed)).unwrap().contains("restart PipeWire"));
        // installed and active, but the card adds buffering
        let a = advice(&facts(128, 416, 416, State::Installed)).unwrap();
        assert!(a.contains("the sound card adds 12.0 ms"), "{a}");
        assert!(advice(&facts(128, 416, 416, State::Changed)).unwrap().contains("was changed"));
        // plain JACK2
        let mut f = facts(1024, 0, 0, State::Missing);
        f.pipewire = Some(false);
        assert!(advice(&f).unwrap().contains("jackd with -p 128"));
        // every message fits the terminal UI's line (78 columns) in the worst case
        for st in [State::Missing, State::Changed, State::Installed] {
            for (p, l) in [(1024, 0), (8192, 9999), (128, 9999)] {
                for pw in [Some(true), Some(false), None] {
                    let f = Facts {
                        sample_rate: 44100,
                        period: p,
                        requested: 128,
                        lat_in: l,
                        lat_out: l,
                        pipewire: pw,
                        state: st,
                    };
                    if let Some(a) = advice(&f) {
                        assert!(a.chars().count() <= 78, "{} chars: {a}", a.chars().count());
                    }
                }
            }
        }
        // a larger period than requested is worth a word even under 10 ms
        let mut f = facts(256, 0, 0, State::Missing);
        f.sample_rate = 192_000;
        assert!(advice(&f).is_some());
        f.requested = 0;
        assert!(advice(&f).is_none());
    }

    #[test]
    fn install_and_remove() {
        let dir = std::env::temp_dir().join(format!("bananasplit-lowlat-{}", std::process::id()));
        assert_eq!(state(&dir), State::Missing);
        let l = install(&dir).unwrap();
        assert!(l.iter().all(|s| s.starts_with("installed")), "{l:?}");
        assert_eq!(state(&dir), State::Installed);
        assert_eq!(std::fs::read_to_string(dir.join("pipewire.conf.d").join(FILE)).unwrap(), PW_CONF);
        assert!(install(&dir).unwrap().iter().all(|s| s.starts_with("already")));
        // a file the user changed is never touched
        let pulse = dir.join("pipewire-pulse.conf.d").join(FILE);
        std::fs::write(&pulse, "# mine\n").unwrap();
        let l = remove(&dir).unwrap();
        assert!(l[0].starts_with("removed") && l[1].starts_with("left alone"), "{l:?}");
        assert_eq!(std::fs::read_to_string(&pulse).unwrap(), "# mine\n");
        assert_eq!(state(&dir), State::Missing);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(PW_CONF.contains("max-quantum   = 128"));
    }

    #[test]
    fn vmix_files_are_recognised_replaced_and_removed() {
        let dir = std::env::temp_dir().join(format!("bananasplit-lowlat-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |sub: &str, text: &str| {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join(LEGACY_FILE), text).unwrap();
        };
        let old_pw = dir.join("pipewire.conf.d").join(LEGACY_FILE);
        let old_pulse = dir.join("pipewire-pulse.conf.d").join(LEGACY_FILE);
        put("pipewire.conf.d", LEGACY_PW_CONF);
        put("pipewire-pulse.conf.d", LEGACY_PULSE_CONF);
        // vmix --low-latency was run: no advice to run it again
        assert_eq!(state(&dir), State::Installed);
        // --low-latency off removes vmix's files
        let l = remove(&dir).unwrap();
        assert_eq!(l.iter().filter(|s| s.contains("(installed by vmix)")).count(), 2, "{l:?}");
        assert!(!old_pw.exists() && !old_pulse.exists());
        assert_eq!(state(&dir), State::Missing);
        // --low-latency replaces them by ours
        put("pipewire.conf.d", LEGACY_PW_CONF);
        put("pipewire-pulse.conf.d", LEGACY_PULSE_CONF);
        let l = install(&dir).unwrap();
        assert_eq!(l.iter().filter(|s| s.starts_with("installed")).count(), 2, "{l:?}");
        assert!(!old_pw.exists() && !old_pulse.exists(), "{l:?}");
        assert_eq!(state(&dir), State::Installed);
        // a vmix file someone edited is never touched
        put("pipewire.conf.d", "# mine\n");
        remove(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&old_pw).unwrap(), "# mine\n");
        // the old files hold the same settings as ours
        for key in ["clock.rate", "min-quantum", "max-quantum", "default.clock.quantum"] {
            let line = |c: &str| c.lines().find(|l| l.contains(key)).map(str::trim).map(String::from);
            assert_eq!(line(LEGACY_PW_CONF), line(PW_CONF), "{key}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
