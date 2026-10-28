//! macOS desktop integration: Finder and the browser (`open`), starting at
//! login (a LaunchAgent of the user's own) and dialogs (`osascript`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The app's identifier: its bundle ID and its LaunchAgent's label.
pub const APP_ID: &str = "io.github.ludalunizz.bananasplit";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The program file that runs (inside BananaSplit.app when it runs from
/// the app).
pub fn program_path() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

/// Opened where it was downloaded (not moved in Finder first), the app
/// runs from a temporary copy macOS makes of it ("App Translocation"),
/// which is gone after a restart.
pub fn translocated(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "AppTranslocation")
}

/// Start a program with `args` in the background and reap it.
fn run_detached(program: &str, args: &[&str]) -> Result<(), String> {
    crate::sys::cloexec_all();
    use std::os::unix::process::CommandExt;
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0) // not stopped by a terminal's Ctrl-C
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    std::thread::spawn(move || {
        let mut c = child;
        let _ = c.wait(); // reap it
    });
    Ok(())
}

/// Show a folder in Finder (made first if it isn't there yet).
pub fn open_folder(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let p = path.to_str().ok_or("the folder's name is not valid UTF-8")?;
    run_detached("/usr/bin/open", &[p])
}

/// Open a web page (a release's page, a driver's page) or the mixer's own
/// page in the default browser.
pub fn open_page(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://127.0.0.1:") {
        return Err("not a web page".into());
    }
    run_detached("/usr/bin/open", &[url])
}

/// System Settings › Sound: to check, or set back, the default devices.
pub fn open_sound_settings() -> Result<(), String> {
    run_detached("/usr/bin/open", &["x-apple.systempreferences:com.apple.preference.sound"])
}

/// Audio MIDI Setup: devices' rates and formats, aggregate devices.
pub fn open_midi_setup() -> Result<(), String> {
    run_detached("/usr/bin/open", &["-b", "com.apple.audio.AudioMIDISetup"])
}

// ---------------------------------------------------------------------------
// Start with the system: ~/Library/LaunchAgents/<APP_ID>.plist
// ---------------------------------------------------------------------------

fn agent_file() -> Option<PathBuf> {
    Some(home()?.join("Library").join("LaunchAgents").join(format!("{APP_ID}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The LaunchAgent that starts `exe` at login, in the background (only
/// the menu bar icon shows).
pub fn agent_plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{APP_ID}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>--no-open</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy())
    )
}

/// The program a LaunchAgent starts (its first argument).
pub fn agent_program(plist: &str) -> Option<String> {
    let rest = &plist[plist.find("<key>ProgramArguments</key>")?..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = start + rest[start..].find("</string>")?;
    Some(xml_unescape(rest[start..end].trim()))
}

pub fn set_autostart(enable: bool) -> Result<String, String> {
    let f = agent_file().ok_or("no home folder")?;
    if enable {
        let exe = program_path().ok_or("cannot find this program's file")?;
        if translocated(&exe) {
            return Err("BananaSplit runs from a temporary copy macOS made of it, because it was opened where it was downloaded: drag BananaSplit into Applications in Finder, open it from there, then switch this on again".into());
        }
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        bananasplit::config::write_replacing(&f, &agent_plist(&exe))?;
        Ok("BananaSplit will start when you log in, in the menu bar.".into())
    } else {
        match std::fs::remove_file(&f) {
            Ok(()) => Ok("BananaSplit will not start when you log in.".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok("BananaSplit will not start when you log in.".into())
            }
            Err(e) => Err(format!("{}: {e}", f.display())),
        }
    }
}

/// "Start with the system" is on: the LaunchAgent starts this very program.
/// One left behind by a copy elsewhere (an older version, before the app
/// was moved to Applications) does not count, so ticking the box points it
/// here.
pub fn autostart_enabled() -> bool {
    let (Some(f), Some(exe)) = (agent_file(), program_path()) else { return false };
    std::fs::read_to_string(f)
        .ok()
        .and_then(|t| agent_program(&t))
        .is_some_and(|p| std::fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p)) == exe)
}

/// On macOS the app is put into Applications by dragging it there: there
/// is no app menu entry to add.
pub fn menu_installed() -> bool {
    true
}

pub fn install() -> Result<String, String> {
    Err("Drag BananaSplit into your Applications folder to keep it there.".into())
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

/// `s` as an AppleScript string literal.
pub fn applescript_string(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A dialog, shown by `osascript` (any thread may ask for one); waits
/// until it is closed.
fn dialog(text: &str, critical: bool) {
    let mut script = format!("display alert \"BananaSplit\" message {}", applescript_string(text));
    if critical {
        script.push_str(" as critical");
    }
    let _ = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn error_dialog(text: &str) {
    dialog(text, true);
}

/// The app window could not open. The mixer runs anyway, so its page opens
/// in the browser.
pub fn no_window(why: &str, mixer_url: &str) {
    dialog(
        &format!("BananaSplit's window could not open:\n{why}\n\nThe mixer is running, and opens in your web browser."),
        false,
    );
    let _ = open_page(mixer_url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_round_trip() {
        let exe = Path::new("/Applications/Banana & Split <2>.app/Contents/MacOS/bananasplit");
        let p = agent_plist(exe);
        assert!(
            p.contains("<string>/Applications/Banana &amp; Split &lt;2&gt;.app/Contents/MacOS/bananasplit</string>"),
            "{p}"
        );
        assert!(p.contains(&format!("<string>{APP_ID}</string>")) && p.contains("<string>--no-open</string>"));
        assert_eq!(agent_program(&p).as_deref(), Some(exe.to_str().unwrap()));
        assert_eq!(agent_program("<plist><dict></dict></plist>"), None);
    }

    #[test]
    fn translocated_copies() {
        assert!(translocated(Path::new(
            "/private/var/folders/x1/abc/T/AppTranslocation/0F3A-11/d/BananaSplit.app/Contents/MacOS/bananasplit"
        )));
        assert!(!translocated(Path::new("/Applications/BananaSplit.app/Contents/MacOS/bananasplit")));
        assert!(!translocated(Path::new("/Users/me/AppTranslocationNotes/BananaSplit.app/Contents/MacOS/bananasplit")));
    }

    #[test]
    fn applescript_strings() {
        assert_eq!(applescript_string("a \"b\" c\\d\ne"), "\"a \\\"b\\\" c\\\\d\\ne\"");
        assert_eq!(applescript_string("Ü"), "\"Ü\"");
    }
}
