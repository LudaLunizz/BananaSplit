//! What the shared GUI layer (`gui/` at the root of the repository) needs from
//! macOS. The list of items is the platform contract documented in
//! `gui/mod.rs`; every app has the same one. (macOS shows the page in a window
//! of its own process, `mac/app.rs`, so it has no `window_command`.)

use crate::gui::{Form, Hub, portdir};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;

/// The system, as the page is told it (`<html data-platform>`).
pub const PLATFORM: &str = "macos";

pub use crate::desktop::{autostart_enabled, install, menu_installed, open_folder, open_page, set_autostart};
pub use crate::sys::{LocalIp, local_timestamp};
pub use crate::update::Updater;

/// Whose process is at the other end of a loopback connection: `Some(true)`
/// ours, `None` not (yet) in the kernel's list of sockets (or not ours: it
/// cannot tell), so the caller asks again.
pub fn peer_is_ours(peer: SocketAddr, us: SocketAddr) -> Option<bool> {
    crate::sys::same_user(peer, us).then_some(true)
}

/// The folder above `bananasplit/` where a running GUI notes its port
/// (`~/Library/Caches/bananasplit/<client>.port`).
pub fn port_dir() -> Option<PathBuf> {
    portdir::macos(std::env::var_os("HOME"))
}

/// Make `dir` (and what is above it) if it is missing, for this user only.
pub fn create_private_dir(dir: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
}

/// Random bytes for the API token, from the kernel (`/dev/urandom`).
pub fn random_bytes(buf: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).map_err(|e| format!("/dev/urandom: {e}"))
}

/// Write `text` to `path` for this user only: a new file made with mode 0600
/// (never readable by others, not even for a moment), renamed over it.
pub fn write_private_file(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp-new");
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// `GET /api/cables`: the virtual cables (BlackHole, ...) and the macOS defaults.
pub fn get(path: &str) -> Option<Result<String, String>> {
    (path == "/api/cables").then(crate::mac::cables::report)
}

/// The "Virtual cables" window's buttons (`op=cables&a=...`).
pub fn command(hub: &Hub, op: &str, f: &Form) -> Option<Result<String, String>> {
    (op == "cables").then(|| match f.req("a")? {
        "page" => {
            open_page(crate::mac::route::BLACKHOLE_PAGE)?;
            Ok("opening BlackHole's page in your browser".into())
        }
        "sound" => {
            crate::desktop::open_sound_settings()?;
            Ok("opening System Settings › Sound".into())
        }
        "midi" => {
            crate::desktop::open_midi_setup()?;
            Ok("opening Audio MIDI Setup".into())
        }
        "seen" => {
            hub.cable_welcome.store(false, SeqCst);
            Ok("ok".into())
        }
        a => Err(format!("unknown cables action {a:?}")),
    })
}
