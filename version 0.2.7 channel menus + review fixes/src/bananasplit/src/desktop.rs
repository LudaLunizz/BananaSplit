//! Desktop integration: the app-menu entry, the icon and starting with the
//! session (XDG autostart), written into the user's own folders.
//!
//! * `bananasplit --install` (or "Add to the app menu" in the GUI) copies
//!   the program to `~/.local/bin/bananasplit` and writes
//!   `~/.local/share/applications/bananasplit.desktop` and the icons under
//!   `~/.local/share/icons/hicolor/`. Packages (.deb) install the same
//!   entry system-wide instead.
//! * "Start with the system" writes `~/.config/autostart/bananasplit.desktop`,
//!   which runs `bananasplit --gui --hidden`: the mixer starts in the
//!   tray, like Voicemeeter's "Run at startup".
//!
//! Every file written here carries an `X-BananaSplit-…` line, and only such
//! files are ever changed or removed.

use std::path::{Path, PathBuf};

pub const APP_ID: &str = "bananasplit";
const MENU_MARK: &str = "X-BananaSplit-Installed-By=bananasplit --install";
const AUTOSTART_MARK: &str = "X-BananaSplit-Autostart=true";

pub const ICON_SVG: &[u8] = include_bytes!("../assets/icons/bananasplit.svg");
pub const ICON_PNGS: [(u32, &[u8]); 7] = [
    (16, include_bytes!("../assets/icons/bananasplit-16.png")),
    (24, include_bytes!("../assets/icons/bananasplit-24.png")),
    (32, include_bytes!("../assets/icons/bananasplit-32.png")),
    (48, include_bytes!("../assets/icons/bananasplit-48.png")),
    (64, include_bytes!("../assets/icons/bananasplit-64.png")),
    (128, include_bytes!("../assets/icons/bananasplit-128.png")),
    (256, include_bytes!("../assets/icons/bananasplit-256.png")),
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(fallback)))
}

pub fn data_home() -> Option<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn config_home() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config")
}

fn data_dirs() -> Vec<PathBuf> {
    let v = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or("/usr/local/share:/usr/share".into());
    v.split(':').filter(|d| d.starts_with('/')).map(PathBuf::from).collect()
}

pub fn menu_entry_path() -> Option<PathBuf> {
    data_home().map(|d| d.join("applications").join(format!("{APP_ID}.desktop")))
}

pub fn autostart_path() -> Option<PathBuf> {
    config_home().map(|d| d.join("autostart").join(format!("{APP_ID}.desktop")))
}

fn user_bin() -> Option<PathBuf> {
    home().map(|h| h.join(".local/bin").join(APP_ID))
}

/// Quote a path for an `Exec=` line (Desktop Entry Specification).
pub fn exec_quote(path: &str) -> String {
    let safe = !path.is_empty() && path.chars().all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c));
    let mut s = String::new();
    if safe {
        s.push_str(path);
    } else {
        // Inside quotes: \ " ` $ are escaped with a backslash; then the
        // whole value is a desktop-file string, where \ is written \\.
        s.push('"');
        for c in path.chars() {
            match c {
                '"' | '`' | '$' => {
                    s.push_str("\\\\");
                    s.push(c);
                }
                '\\' => s.push_str("\\\\\\\\"),
                '%' => s.push_str("%%"),
                '\n' | '\r' | '\t' => s.push(' '),
                c => s.push(c),
            }
        }
        s.push('"');
    }
    if safe { s.replace('%', "%%") } else { s }
}

/// The program to start: the AppImage when running from one, else this file.
pub fn program_path() -> Option<PathBuf> {
    if let Some(a) = std::env::var_os("APPIMAGE").filter(|a| !a.is_empty()).map(PathBuf::from) {
        if a.is_file() {
            return Some(a);
        }
    }
    std::env::current_exe().ok().map(installed_path)
}

/// When an update replaced the program while it runs, Linux names it
/// "<path> (deleted)"; the file now at <path> is the program to start.
fn installed_path(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(p) if Path::new(p).is_file() => PathBuf::from(p),
        _ => exe,
    }
}

/// The file to start this program's helper processes (the window, the
/// error dialog) from: this very program, also after an update replaced it
/// while it runs (the helper must speak the same protocol).
pub fn helper_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    if exe.to_str().is_some_and(|s| s.ends_with(" (deleted)")) || !exe.is_file() {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    Ok(exe)
}

/// A program in a system folder was installed by a package.
fn is_system_path(p: &Path) -> bool {
    ["/usr/", "/opt/", "/snap/", "/nix/", "/app/"].iter().any(|d| p.starts_with(d))
}

/// `TryExec=` for an absolute program path: menus and the session skip the
/// entry once the program is gone (a removed package, a deleted folder).
fn try_exec(exec: &str) -> String {
    if !exec.starts_with('/') || exec.chars().any(char::is_control) {
        return String::new();
    }
    format!("TryExec={}\n", exec.replace('\\', "\\\\"))
}

pub fn menu_entry(exec: &str) -> String {
    let e = exec_quote(exec);
    let t = try_exec(exec);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=BananaSplit\n\
         GenericName=Virtual audio mixer\n\
         Comment=Mix, route and record audio: 6 strips, 5 buses, EQ, VBAN and a recorder\n\
         {t}\
         Exec={e} --gui\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n\
         Categories=AudioVideo;Audio;Mixer;\n\
         Keywords=mixer;audio;jack;pipewire;recorder;voicemeeter;vban;\n\
         Actions=quit;\n\
         {MENU_MARK}\n\
         \n\
         [Desktop Action quit]\n\
         Name=Quit BananaSplit\n\
         Exec={e} --quit\n"
    )
}

pub fn autostart_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=BananaSplit\n\
         Comment=Start the BananaSplit mixer in the tray\n\
         {}\
         Exec={} --gui --hidden\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-GNOME-Autostart-Delay=3\n\
         X-KDE-autostart-after=panel\n\
         {AUTOSTART_MARK}\n",
        try_exec(exec),
        exec_quote(exec)
    )
}

fn ours(path: &Path, mark: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.lines().any(|l| l.trim() == mark))
}

/// The `contrib/bananasplit.desktop` of 2.5 and earlier, copied by hand
/// into `~/.local/share/applications`: `--install` replaces it.
fn old_contrib_entry(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| {
        let has = |l: &str| t.lines().any(|x| x.trim() == l);
        has("Name=BananaSplit") && has("Comment=6-strip virtual audio mixer and recorder")
    })
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = path.with_extension("bananasplit-tmp");
    std::fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Is there an app-menu entry (the user's own, or a package's)?
pub fn menu_installed() -> bool {
    let name = format!("applications/{APP_ID}.desktop");
    data_home().into_iter().chain(data_dirs()).any(|d| d.join(&name).is_file())
}

pub fn autostart_enabled() -> bool {
    let Some(p) = autostart_path() else { return false };
    let Ok(t) = std::fs::read_to_string(&p) else { return false };
    !t.lines().map(str::trim).any(|l| l == "Hidden=true" || l == "X-GNOME-Autostart-enabled=false")
}

/// Turn starting with the session on or off.
pub fn set_autostart(on: bool) -> Result<String, String> {
    let path = autostart_path().ok_or("no home folder")?;
    if on {
        if path.exists() && !ours(&path, AUTOSTART_MARK) {
            return Err(format!("{} exists and was not made by BananaSplit; remove it first", path.display()));
        }
        let prog = program_path().ok_or("cannot find this program's path")?;
        write_atomic(&path, autostart_entry(&prog.to_string_lossy()).as_bytes())?;
        Ok("BananaSplit starts in the tray when you log in".into())
    } else {
        if path.exists() {
            if !ours(&path, AUTOSTART_MARK) {
                return Err(format!("{} was not made by BananaSplit; remove it yourself", path.display()));
            }
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok("BananaSplit no longer starts when you log in".into())
    }
}

fn run_quietly(cmd: &str, args: &[&str]) {
    let _ = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Keep the caches of the user's own folders valid, if there are any.
/// None is created: a cache nobody updates hides what is added later, and
/// menus and icon themes find new files without one.
fn refresh_caches(data: &Path) {
    let apps = data.join("applications");
    if apps.join("mimeinfo.cache").is_file() {
        run_quietly("update-desktop-database", &["-q", &apps.to_string_lossy()]);
    }
    let hicolor = data.join("icons/hicolor");
    if hicolor.join("icon-theme.cache").is_file() {
        run_quietly("gtk-update-icon-cache", &["-q", "-t", "-f", &hicolor.to_string_lossy()]);
    }
}

fn icon_paths(data: &Path) -> Vec<(PathBuf, &'static [u8])> {
    let hi = data.join("icons/hicolor");
    let mut v = vec![(hi.join("scalable/apps").join(format!("{APP_ID}.svg")), ICON_SVG)];
    for (size, png) in ICON_PNGS {
        v.push((hi.join(format!("{size}x{size}/apps")).join(format!("{APP_ID}.png")), png));
    }
    v
}

/// `bananasplit --install`: this program into `~/.local/bin`, with its
/// app-menu entry and icons.
pub fn install() -> Result<String, String> {
    let prog = program_path().ok_or("cannot find this program's path")?;
    if is_system_path(&prog) && std::env::var_os("APPIMAGE").is_none() {
        return Err(format!("{} was installed by a package, which already added BananaSplit to the app menu", prog.display()));
    }
    let data = data_home().ok_or("no home folder")?;
    let menu = menu_entry_path().ok_or("no home folder")?;
    if menu.exists() && !ours(&menu, MENU_MARK) && !old_contrib_entry(&menu) {
        return Err(format!("{} exists and was not made by BananaSplit; remove it first", menu.display()));
    }
    // The AppImage is the program; a plain binary is copied to ~/.local/bin.
    let target = if std::env::var_os("APPIMAGE").is_some() {
        prog.clone()
    } else {
        let bin = user_bin().ok_or("no home folder")?;
        let same = std::fs::canonicalize(&prog).ok() == std::fs::canonicalize(&bin).ok();
        if !same {
            let bytes = std::fs::read(&prog).map_err(|e| format!("{}: {e}", prog.display()))?;
            write_atomic(&bin, &bytes)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", bin.display()))?;
        }
        bin
    };
    for (path, bytes) in icon_paths(&data) {
        write_atomic(&path, bytes)?;
    }
    let exec = target.to_string_lossy().into_owned();
    write_atomic(&menu, menu_entry(&exec).as_bytes())?;
    if let Some(a) = autostart_path().filter(|a| ours(a, AUTOSTART_MARK)) {
        write_atomic(&a, autostart_entry(&exec).as_bytes())?;
    }
    refresh_caches(&data);
    Ok(format!("BananaSplit is in the app menu ({}), program {}", menu.display(), target.display()))
}

/// `bananasplit --uninstall`: remove what `--install` and the autostart
/// switch wrote. Settings and recordings stay.
pub fn uninstall() -> Result<String, String> {
    let mut removed = Vec::new();
    let menu = menu_entry_path().ok_or("no home folder")?;
    let installed = ours(&menu, MENU_MARK);
    if installed {
        // The copy in ~/.local/bin goes only if this entry starts it.
        let entry = std::fs::read_to_string(&menu).unwrap_or_default();
        let bin = user_bin()
            .filter(|b| b.is_file())
            .filter(|b| entry.lines().any(|l| l == format!("Exec={} --gui", exec_quote(&b.to_string_lossy()))));
        std::fs::remove_file(&menu).map_err(|e| format!("{}: {e}", menu.display()))?;
        removed.push(menu.display().to_string());
        if let Some(data) = data_home() {
            let mut icons = 0;
            for (p, bytes) in icon_paths(&data) {
                if std::fs::read(&p).is_ok_and(|b| b == bytes) && std::fs::remove_file(&p).is_ok() {
                    icons += 1;
                }
            }
            if icons > 0 {
                removed.push(format!("its {icons} icons"));
            }
            refresh_caches(&data);
        }
        if let Some(bin) = bin {
            if std::fs::remove_file(&bin).is_ok() {
                removed.push(bin.display().to_string());
            }
        }
    }
    if let Some(a) = autostart_path().filter(|a| ours(a, AUTOSTART_MARK)) {
        std::fs::remove_file(&a).map_err(|e| format!("{}: {e}", a.display()))?;
        removed.push(a.display().to_string());
    }
    if removed.is_empty() {
        return Ok("nothing to remove (BananaSplit was not installed with --install)".into());
    }
    Ok(format!("removed {}; your settings and recordings are kept", removed.join(", ")))
}

/// Open a folder in the file manager.
pub fn open_folder(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::sys::cloexec_all();
    let child = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run xdg-open: {e}"))?;
    std::thread::spawn(move || {
        let mut c = child;
        let _ = c.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_are_quoted() {
        assert_eq!(exec_quote("/usr/bin/bananasplit"), "/usr/bin/bananasplit");
        assert_eq!(exec_quote("/home/a b/bananasplit"), "\"/home/a b/bananasplit\"");
        assert_eq!(exec_quote("/x/$y\"z`w"), "\"/x/\\\\$y\\\\\"z\\\\`w\"");
        assert_eq!(exec_quote("/x\\y"), "\"/x\\\\\\\\y\"");
        assert_eq!(exec_quote("/100%/b"), "\"/100%%/b\"");
        let e = menu_entry("/home/me/.local/bin/bananasplit");
        assert!(e.contains("\nExec=/home/me/.local/bin/bananasplit --gui\n"));
        assert!(e.contains("\nExec=/home/me/.local/bin/bananasplit --quit\n"));
        assert!(e.starts_with("[Desktop Entry]\nType=Application\nName=BananaSplit\n"));
        assert!(autostart_entry("/opt/b s/bananasplit").contains("Exec=\"/opt/b s/bananasplit\" --gui --hidden\n"));
        assert!(e.lines().all(|l| !l.starts_with(' ')), "no indentation leaks into the file");
        // TryExec: the entry disappears with the program
        assert!(e.contains("\nComment=Mix, route and record audio: 6 strips, 5 buses, EQ, VBAN and a recorder\nTryExec=/home/me/.local/bin/bananasplit\nExec="));
        assert!(autostart_entry("/opt/b s/bananasplit").contains("\nTryExec=/opt/b s/bananasplit\nExec="));
        assert!(!menu_entry("bananasplit").contains("TryExec"));
        assert!(!menu_entry("/a\nb").contains("TryExec"));
        assert!(menu_entry("/x\\y").contains("\nTryExec=/x\\\\y\n"));
    }

    #[test]
    fn replaced_program_paths() {
        let d = std::env::temp_dir().join(format!("bs-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("bananasplit");
        std::fs::write(&p, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", p.display()));
        assert_eq!(installed_path(deleted.clone()), p, "the new file at the same place");
        std::fs::remove_file(&p).unwrap();
        assert_eq!(installed_path(deleted.clone()), deleted, "nothing there: unchanged");
        assert_eq!(installed_path(PathBuf::from("/usr/bin/bananasplit")), PathBuf::from("/usr/bin/bananasplit"));
        assert!(helper_exe().unwrap().is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn icons_are_real_files() {
        assert!(ICON_SVG.starts_with(b"<svg") || ICON_SVG.starts_with(b"<?xml"));
        for (size, png) in ICON_PNGS {
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
            let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
            assert_eq!((w, h), (size, size));
        }
    }
}
