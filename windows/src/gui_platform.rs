//! What the shared GUI layer (`gui/` at the root of the repository) needs from
//! Windows. The list of items is the platform contract documented in
//! `gui/mod.rs`; every app has the same one.

use crate::gui::{Form, Hub, WINDOW_FLAG, portdir};
use std::net::SocketAddr;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering::SeqCst;

/// The main page.
pub const PAGE: &str = include_str!("gui.html");

pub use crate::desktop::{autostart_enabled, install, menu_installed, open_folder, open_page, set_autostart};
pub use crate::sys::{LocalIp, local_timestamp};
pub use crate::update::Updater;

/// Whose process is at the other end of a loopback connection: `Some(true)`
/// ours, `None` not (yet) in the system's list of connections (or not ours:
/// it cannot tell), so the caller asks again.
pub fn peer_is_ours(peer: SocketAddr, us: SocketAddr) -> Option<bool> {
    crate::sys::same_user(peer, us).then_some(true)
}

/// The folder above `bananasplit\` where a running GUI notes its port.
pub fn port_dir() -> Option<PathBuf> {
    let var = std::env::var_os;
    Some(portdir::windows(var("LOCALAPPDATA"), var("APPDATA"), var("USERPROFILE"), std::env::temp_dir()))
}

/// Make `dir` (and what is above it) if it is missing. (The per-user folder
/// it is in is the user's own already.)
pub fn create_private_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
}

/// `GET /api/cables`: Splitter and VB-Audio cables, Voicemeeter, Windows defaults.
pub fn get(path: &str) -> Option<Result<String, String>> {
    (path == "/api/cables").then(crate::win::cables::report)
}

/// The "Virtual cables" window's buttons (`op=cables&a=...`).
pub fn command(hub: &Hub, op: &str, f: &Form) -> Option<Result<String, String>> {
    (op == "cables").then(|| match f.req("a")? {
        "splitter" => {
            open_page(crate::win::route::SPLITTER_PAGE)?;
            Ok("opening the Splitter page in your browser".into())
        }
        "page" => {
            open_page(crate::win::cables::VBCABLE_PAGE)?;
            Ok("opening the VB-Audio page in your browser".into())
        }
        "sound" => {
            crate::desktop::open_sound_settings()?;
            Ok("opening the Windows sound settings".into())
        }
        "seen" => {
            hub.cable_welcome.store(false, SeqCst);
            Ok("ok".into())
        }
        a => Err(format!("unknown cables action {a:?}")),
    })
}

/// `bananasplit.exe` is a console program: its window process gets no
/// console window of its own, and does not keep the mixer's open.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The window process, not started yet: this program again, without a console.
pub fn window_command() -> Result<Command, String> {
    let mut cmd = Command::new(crate::desktop::helper_exe()?);
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

/// Where the window process's error output goes: nowhere. Its messages come
/// as `fail …` lines (it has no console).
pub fn window_stderr() -> Stdio {
    Stdio::null()
}

/// Open the page in a native desktop window of its own, not the mixer's
/// (`WindowProc`): only when the running mixer cannot be asked to show its
/// window. Closing it closes it.
pub fn open_window(url: &str) -> bool {
    let Ok(exe) = crate::desktop::helper_exe() else { return false };
    Command::new(exe)
        .arg(WINDOW_FLAG)
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}
