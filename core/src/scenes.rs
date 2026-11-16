//! Scenes: named snapshots of the mixer.
//!
//! A scene holds what you set while mixing: every strip's and bus's fader,
//! input gain, pan, mute, solo, mono, routing (a bus's sends to other buses
//! too), EQ, each strip's gate and compressor, and which channels are linked to the recorder. It does not hold names, device connections,
//! VBAN or recorder settings, so loading a scene never rewires anything.
//! Nor does it hold the buses' limiters: a limit stays where it was put
//! until it is moved, whatever scene is loaded.
//! Loading goes through the same click-free ramps as moving a fader.
//!
//! Scenes are small INI files in `scenes/` next to the config file
//! (`~/.config/bananasplit/scenes/Live Room.ini`), in the config file's own
//! format, so they can be read and edited by hand.

use crate::config::{Config, ParseError, format_routes, write_dynamics, write_eq};
use crate::engine::{BUS_IDS, Shared};
use crate::error::{IoError, into_string};
use crate::recorder;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The longest scene name, in characters.
pub const MAX_NAME: usize = 40;

/// Why a scene could not be saved, loaded or deleted.
#[derive(Debug)]
#[non_exhaustive]
pub enum SceneError {
    /// The name is not allowed ([`validate_name`] says which are).
    Name,
    /// A file of that name exists and is not a BananaSplit scene; it is left
    /// alone.
    NotAScene(PathBuf),
    /// The scene file or its folder could not be read, written or removed.
    Io(IoError),
    /// The scene file is not valid; nothing was changed.
    Parse {
        /// The scene file.
        path: PathBuf,
        /// The first line that is wrong.
        source: ParseError,
    },
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneError::Name => write!(
                f,
                "scene names are 1-{MAX_NAME} letters, digits, spaces and - _ . , ( ) & + ' ! (not starting with a dot)"
            ),
            SceneError::NotAScene(path) => {
                write!(f, "{} is not a BananaSplit scene; it is left alone", path.display())
            }
            SceneError::Io(e) => e.fmt(f),
            SceneError::Parse { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SceneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SceneError::Io(e) => Some(e),
            SceneError::Parse { source, .. } => Some(source),
            SceneError::Name | SceneError::NotAScene(_) => None,
        }
    }
}

impl From<IoError> for SceneError {
    fn from(e: IoError) -> SceneError {
        SceneError::Io(e)
    }
}

into_string!(SceneError);

/// A scene name: 1-40 letters, digits, spaces and `-_.,()&+'!`, not starting
/// with a dot or a space. Returns it trimmed.
///
/// # Errors
/// [`SceneError::Name`] when the name is not allowed.
pub fn validate_name(name: &str) -> Result<&str, SceneError> {
    let n = name.trim();
    let ok_char = |c: char| c.is_alphanumeric() || " -_.,()&+'!".contains(c);
    if n.is_empty() || n.chars().count() > MAX_NAME || n.starts_with('.') || !n.chars().all(ok_char) {
        return Err(SceneError::Name);
    }
    Ok(n)
}

/// The first line of every scene file; only such files are listed,
/// replaced or deleted.
const HEADER: &str = "# BananaSplit scene";

fn is_scene(path: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 3 + HEADER.len()];
    let Ok(n) = std::fs::File::open(path).and_then(|f| f.take(b.len() as u64).read(&mut b)) else { return false };
    // (saved by an editor that starts files with a byte order mark: Windows)
    let b = b[..n].strip_prefix(BOM.as_bytes()).unwrap_or(&b[..n]);
    b.starts_with(HEADER.as_bytes())
}

/// The UTF-8 byte order mark some editors (Notepad) put at the start of a file.
const BOM: &str = "\u{feff}";

/// The scene file for the current mixer state.
pub fn to_ini(sh: &Shared) -> String {
    let mut c = Config::default();
    c.capture(sh);
    let mut o = String::new();
    let _ = writeln!(o, "{HEADER}: faders, input gain, pan, mute, solo, mono, routing, EQ, gate,");
    let _ = writeln!(
        o,
        "# compressor and recorder links. Names, devices, VBAN and recorder settings are not part of a scene."
    );
    for (i, s) in c.strips.iter().enumerate() {
        let _ = writeln!(o, "\n[strip{}]", i + 1);
        let _ = writeln!(o, "gain_db = {}", s.gain_db);
        let _ = writeln!(o, "trim_db = {}", s.trim_db);
        let _ = writeln!(o, "pan = {}", s.pan);
        let _ = writeln!(o, "mute = {}", s.mute);
        let _ = writeln!(o, "solo = {}", s.solo);
        let _ = writeln!(o, "mono = {}", s.mono);
        let _ = writeln!(o, "routes = {}", format_routes(s.routes));
        write_eq(&mut o, &s.eq);
        write_dynamics(&mut o, &s.dynamics);
    }
    for (i, b) in c.buses.iter().enumerate() {
        let _ = writeln!(o, "\n[bus.{}]", BUS_IDS[i]);
        let _ = writeln!(o, "gain_db = {}", b.gain_db);
        let _ = writeln!(o, "trim_db = {}", b.trim_db);
        let _ = writeln!(o, "pan = {}", b.pan);
        let _ = writeln!(o, "mute = {}", b.mute);
        let _ = writeln!(o, "mono = {}", b.mono);
        // only when used, so that BananaSplit 2.6 can still load the scene
        if b.routes != 0 {
            let _ = writeln!(o, "routes = {}", format_routes(b.routes));
        }
        write_eq(&mut o, &b.eq);
    }
    let _ = writeln!(o, "\n[recorder]");
    let _ = writeln!(o, "links = {}", recorder::format_links(c.recorder.links));
    o
}

/// `gate`, `comp` and their settings (strips only).
fn is_dynamics_key(key: &str) -> bool {
    use crate::dynamics::{COMP, GATE};
    key == "gate" || key == "comp" || GATE.iter().chain(&COMP).any(|s| s.key == key)
}

/// Check that `text` only holds keys a scene may hold, then parse it with
/// the config parser.
///
/// # Errors
/// A [`ParseError`] with the line of the first key, section or value that a
/// scene may not hold.
pub fn parse(text: &str) -> Result<Config, ParseError> {
    let text = text.strip_prefix(BOM).unwrap_or(text);
    let mut section = String::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(h) = line.strip_prefix('[') {
            section = h.trim_end_matches(']').trim().to_ascii_lowercase();
            let ok = section == "recorder"
                || section.strip_prefix("strip").is_some_and(|n| n.parse::<u8>().is_ok())
                || section.starts_with("bus.");
            if !ok {
                return Err(ParseError { line: i + 1, msg: format!("[{section}] does not belong in a scene") });
            }
            continue;
        }
        let key = line.split('=').next().unwrap_or("").trim();
        let mixer = ["gain_db", "trim_db", "pan", "mute", "mono", "eq"].contains(&key)
            || (key.len() == 3 && key.starts_with("eq") && key.as_bytes()[2].is_ascii_digit());
        let ok = match section.as_str() {
            "recorder" => key == "links",
            s if s.starts_with("strip") => mixer || key == "solo" || key == "routes" || is_dynamics_key(key),
            _ => mixer || key == "routes",
        };
        if !ok {
            return Err(ParseError { line: i + 1, msg: format!("{key:?} does not belong in a scene") });
        }
    }
    Config::parse(text)
}

/// Scene files in one folder.
#[derive(Debug, Clone)]
pub struct SceneStore {
    dir: PathBuf,
}

impl SceneStore {
    /// A store that keeps its scene files in `dir`. The folder is created on
    /// the first save, not here.
    pub fn new(dir: PathBuf) -> SceneStore {
        SceneStore { dir }
    }

    /// `scenes/` next to the config file.
    pub fn for_config(config: &Path) -> SceneStore {
        SceneStore::new(config.parent().map_or_else(|| PathBuf::from("scenes"), |p| p.join("scenes")))
    }

    /// The folder the scene files are kept in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf, SceneError> {
        Ok(self.dir.join(format!("{}.ini", validate_name(name)?)))
    }

    /// Scene names, sorted without regard to case.
    pub fn list(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut v: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().into_string().ok()?;
                let stem = n.strip_suffix(".ini")?;
                (validate_name(stem).ok() == Some(stem) && e.path().is_file() && is_scene(&e.path()))
                    .then(|| stem.to_string())
            })
            .collect();
        v.sort_by_key(|n| n.to_lowercase());
        v
    }

    /// Is there a scene called `name`? False for a name that is not allowed
    /// and for a file of that name that is not a BananaSplit scene.
    pub fn exists(&self, name: &str) -> bool {
        self.path(name).is_ok_and(|p| p.is_file() && is_scene(&p))
    }

    /// A file of that name that is not a scene is never touched.
    fn check_ours(path: &Path) -> Result<(), SceneError> {
        if path.exists() && !is_scene(path) {
            return Err(SceneError::NotAScene(path.to_path_buf()));
        }
        Ok(())
    }

    /// Save the mixer as `name` (replacing a scene of that name).
    ///
    /// # Errors
    /// [`SceneError::Name`], [`SceneError::NotAScene`] when a file of that
    /// name is not a scene, or [`SceneError::Io`].
    pub fn save(&self, name: &str, sh: &Shared) -> Result<(), SceneError> {
        let path = self.path(name)?;
        Self::check_ours(&path)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| IoError::new(&self.dir, e))?;
        Ok(crate::config::write_replacing(&path, &to_ini(sh))?)
    }

    /// Load scene `name` into the mixer. All or nothing at the file level:
    /// the whole file is read and checked (`parse`) before anything is set,
    /// so a bad file changes nothing. A good one is then applied value by
    /// value (`Config::apply`), each through its usual ramp; the audio thread
    /// may run a cycle between two of them.
    ///
    /// # Errors
    /// [`SceneError::Name`], [`SceneError::Io`] when the file cannot be read,
    /// or [`SceneError::Parse`] when it is not a valid scene. In every case
    /// the mixer is left as it was.
    pub fn load(&self, name: &str, sh: &Shared) -> Result<(), SceneError> {
        let path = self.path(name)?;
        let text = std::fs::read_to_string(&path).map_err(|e| IoError::new(&path, e))?;
        let mut cfg = parse(&text).map_err(|source| SceneError::Parse { path: path.clone(), source })?;
        // the limiters stay where they are
        for (b, p) in cfg.buses.iter_mut().zip(&sh.buses) {
            b.limit_db = p.limit.load();
        }
        cfg.apply(sh);
        Ok(())
    }

    /// Remove scene `name`; a file that is not a scene is never touched.
    ///
    /// # Errors
    /// [`SceneError::Name`], [`SceneError::NotAScene`] or
    /// [`SceneError::Io`].
    pub fn delete(&self, name: &str) -> Result<(), SceneError> {
        let path = self.path(name)?;
        Self::check_ours(&path)?;
        Ok(std::fs::remove_file(&path).map_err(|e| IoError::new(&path, e))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    #[test]
    fn names() {
        assert_eq!(validate_name("  Live Room 06 ").unwrap(), "Live Room 06");
        assert!(validate_name("Podcast (Mon) & Fri!").is_ok());
        assert!(validate_name("Café").is_ok());
        for bad in ["", "  ", ".hidden", "a/b", "../x", "a\\b", "x\ny", &"a".repeat(41), "a:b", "nul\0"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_scene_saved_with_a_byte_order_mark_is_listed_and_loads() {
        // (copied to Windows, edited in Notepad, copied back)
        let dir = std::env::temp_dir().join(format!("bs-scenes-bom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SceneStore::new(dir.clone());
        let sh = Shared::new();
        sh.strips[1].gain_db.store(-9.0);
        store.save("Win", &sh).unwrap();
        let text = std::fs::read_to_string(dir.join("Win.ini")).unwrap();
        std::fs::write(dir.join("WinBom.ini"), format!("\u{feff}{text}")).unwrap();
        assert_eq!(store.list(), ["Win", "WinBom"]);
        let sh2 = Shared::new();
        store.load("WinBom", &sh2).unwrap();
        assert_eq!(sh2.strips[1].gain_db.load(), -9.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_list_load_delete() {
        let dir = std::env::temp_dir().join(format!("bs-scenes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SceneStore::new(dir.clone());
        assert!(store.list().is_empty());
        let sh = Shared::new();
        sh.strips[2].gain_db.store(-7.5);
        sh.strips[2].trim_db.store(6.0);
        sh.strips[2].pan.store(-0.25);
        sh.strips[2].routes.store(0b10010, Relaxed);
        sh.strips[4].solo.store(true, Relaxed);
        sh.buses[1].pan.store(0.5);
        sh.buses[1].mute.store(true, Relaxed);
        sh.buses[3].eq.on.store(true, Relaxed);
        sh.buses[3].eq.bands[2].gain_db.store(-3.0);
        sh.buses[4].routes.store(0b00010, Relaxed); // B2 -> V2
        sh.strips[0].dynamics.gate_on.store(true, Relaxed);
        sh.strips[0].dynamics.comp[crate::dynamics::C_RATIO].store(6.5);
        sh.rec_links.store(0b100_0000_0001, Relaxed);
        store.save("Live Room", &sh).unwrap();
        store.save("acoustic", &Shared::new()).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join(".hidden.ini"), "x").unwrap();
        assert_eq!(store.list(), ["acoustic", "Live Room"]);
        assert!(store.exists("Live Room") && !store.exists("Other"));
        // loading puts every value back
        let sh2 = Shared::new();
        store.load("Live Room", &sh2).unwrap();
        let (mut a, mut b) = (Config::default(), Config::default());
        a.capture(&sh);
        b.capture(&sh2);
        assert_eq!(a, b);
        assert_eq!(sh2.strips[2].pan.load(), -0.25);
        assert_eq!(sh2.buses[4].routes.load(Relaxed), 0b00010);
        assert!(sh2.strips[0].dynamics.gate_on.load(Relaxed));
        assert_eq!(sh2.strips[0].dynamics.comp[crate::dynamics::C_RATIO].load(), 6.5);
        // a scene never touches names, connections or cables
        let text = std::fs::read_to_string(dir.join("Live Room.ini")).unwrap();
        assert!(
            !text.contains("name") && !text.contains("connect") && !text.contains("vban") && !text.contains("cable")
        );
        // a strip with its gate and compressor as they come writes neither (2.8.15 can load it)
        assert_eq!((text.matches("gate = ").count(), text.matches("comp_ratio = ").count()), (1, 1));
        // a bus without sends writes none (2.6 can load the scene)
        assert_eq!(text.matches("routes = ").count(), 6 + 1);
        store.delete("Live Room").unwrap();
        assert_eq!(store.list(), ["acoustic"]);
        assert!(store.load("Live Room", &sh2).is_err());
        assert!(store.save("../evil", &sh2).is_err());
        // an .ini that is not a scene (a config kept next to it): not listed,
        // never replaced or deleted
        std::fs::write(dir.join("mine.ini"), "[global]\nclient_name = x\n").unwrap();
        assert_eq!(store.list(), ["acoustic"]);
        assert!(!store.exists("mine"));
        assert!(matches!(store.save("mine", &sh2), Err(SceneError::NotAScene(p)) if p.ends_with("mine.ini")));
        assert!(store.delete("mine").is_err());
        assert_eq!(std::fs::read_to_string(dir.join("mine.ini")).unwrap(), "[global]\nclient_name = x\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limiters_stay_where_they_are() {
        let dir = std::env::temp_dir().join(format!("bs-scenes-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SceneStore::new(dir.clone());
        let sh = Shared::new();
        sh.buses[3].limit.store(-9.0);
        sh.buses[3].gain_db.store(-4.0);
        store.save("talk", &sh).unwrap();
        assert!(!std::fs::read_to_string(dir.join("talk.ini")).unwrap().contains("limit"));
        // another limit now: loading the scene moves the fader, not the limit
        let sh2 = Shared::new();
        sh2.buses[3].limit.store(-3.0);
        sh2.buses[0].limit.store(-12.0);
        store.load("talk", &sh2).unwrap();
        assert_eq!(sh2.buses[3].gain_db.load(), -4.0);
        assert_eq!(sh2.buses[3].limit.load(), -3.0);
        assert_eq!(sh2.buses[0].limit.load(), -12.0);
        // and a scene file may not set one
        assert!(parse("[bus.B1]\nlimit_db = -6\n").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_name_is_its_own_error() {
        let dir = std::env::temp_dir().join(format!("bs-scene-name-{}", std::process::id()));
        let store = SceneStore::new(dir.clone());
        assert!(matches!(validate_name(".hidden"), Err(SceneError::Name)));
        let text = SceneError::Name.to_string();
        assert!(text.contains("1-40 letters"), "{text}");
        let sh = Shared::new();
        assert!(matches!(store.save("../x", &sh), Err(SceneError::Name)));
        assert!(matches!(store.load("nope", &sh), Err(SceneError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound));
        assert!(!dir.exists(), "a refused name creates nothing");
    }

    #[test]
    fn strict_parsing() {
        assert!(parse("[strip1]\ngain_db = -3\n[bus.B2]\nmono = true\n[recorder]\nlinks = 1\n").is_ok());
        assert_eq!(parse("[strip1]\nname = x\n").unwrap_err().line, 2);
        assert!(parse("[strip1]\nconnect = @default\n").is_err());
        assert!(parse("[global]\nclient_name = x\n").is_err());
        assert!(parse("[vban]\nenabled = true\n").is_err());
        assert!(parse("[recorder]\nfolder = /tmp\n").is_err());
        assert!(parse("[bus.A1]\nsolo = true\n").is_err());
        assert!(parse("[bus.B1]\nroutes = A1\n").is_ok());
        assert!(parse("[bus.B1]\ncable = on\n").is_err());
        assert!(parse("[strip1]\ncable = off\n").is_err());
        assert!(parse("[strip1]\ngain_db = loud\n").is_err());
        assert!(parse("[strip9]\n").is_err());
        assert_eq!(SceneStore::for_config(Path::new("/x/bananasplit.ini")).dir(), Path::new("/x/scenes"));
    }
}
