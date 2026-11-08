//! What the shared GUI layer (`gui/` at the root of the repository) needs from
//! Linux. The list of items is the platform contract documented in
//! `gui/mod.rs`; every app has the same one.

use crate::gui::peer::proc_net_tcp::{mapped, owner};
use crate::gui::{Form, Hub, portdir};
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The main page.
pub const PAGE: &str = include_str!("gui.html");

pub use crate::desktop::{autostart_enabled, install, menu_installed, open_folder, open_page, set_autostart};
pub use crate::sys::{LocalIp, local_timestamp};
pub use crate::update::Updater;

/// Whose process is at the other end of a loopback connection: `Some(true)`
/// ours, `Some(false)` another user's, `None` not (yet) in the system's list,
/// so the caller asks again. (Without /proc, as in 2.5: ours.)
pub fn peer_is_ours(peer: SocketAddr, us: SocketAddr) -> Option<bool> {
    let Ok(t4) = std::fs::read_to_string("/proc/net/tcp") else { return Some(true) };
    let owner = owner(&t4, peer, us).or_else(|| {
        let t6 = std::fs::read_to_string("/proc/net/tcp6").ok()?;
        owner(&t6, mapped(peer), mapped(us))
    });
    owner.map(|o| o == crate::sys::uid())
}

/// The folder above `bananasplit/` where a running GUI notes its port
/// (`$XDG_RUNTIME_DIR/bananasplit/<client>.port`).
/// In a Flatpak only `$XDG_RUNTIME_DIR/app/<app ID>` is shared by its instances.
pub fn port_dir() -> Option<PathBuf> {
    let var = std::env::var_os;
    portdir::linux(var("XDG_RUNTIME_DIR"), var("HOME"), crate::desktop::flatpak_id())
}

/// Make `dir` (and what is above it) if it is missing, for this user only.
pub fn create_private_dir(dir: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
}

/// Linux has no extra `GET` endpoint.
pub fn get(_path: &str) -> Option<Result<String, String>> {
    None
}

/// Linux has no extra `op`.
pub fn command(_hub: &Hub, _op: &str, _form: &Form) -> Option<Result<String, String>> {
    None
}

/// argv[0] of the helper processes (also when started from /proc/self/exe).
const APP_ARG0: &str = "bananasplit";

/// The window process, not started yet: this program again, with the name it
/// is known by.
pub fn window_command() -> Result<Command, String> {
    let mut cmd = Command::new(crate::desktop::helper_exe()?);
    cmd.arg0(APP_ARG0);
    Ok(cmd)
}

/// Where the window process's error output goes: this one's.
pub fn window_stderr() -> Stdio {
    Stdio::inherit()
}

/// Open the page in its own app window (Chromium-family `--app` mode), or
/// in the default browser. Returns false if nothing could be started.
pub fn open_window(url: &str) -> bool {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    crate::sys::cloexec_all();
    let app = format!("--app={url}");
    let browsers = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "brave-browser",
        "brave",
        "microsoft-edge",
        "vivaldi",
    ];
    let mut tries: Vec<(&str, &str)> = browsers.iter().map(|b| (*b, app.as_str())).collect();
    tries.push(("xdg-open", url));
    for (bin, arg) in tries {
        let child = Command::new(bin)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0) // not killed by the terminal's Ctrl-C
            .spawn();
        if let Ok(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.wait(); // reap it
            });
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    /// A real connection, in the real /proc: the owner is this process's user.
    #[test]
    fn only_our_user_gets_in() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (srv, _) = l.accept().unwrap();
        if let Ok(t) = std::fs::read_to_string("/proc/net/tcp") {
            assert_eq!(owner(&t, srv.peer_addr().unwrap(), srv.local_addr().unwrap()), Some(crate::sys::uid()));
            assert_eq!(
                peer_is_ours(srv.peer_addr().unwrap(), srv.local_addr().unwrap()),
                Some(true),
                "the server sees its own user"
            );
            assert_eq!(
                peer_is_ours(c.local_addr().unwrap(), c.peer_addr().unwrap()),
                Some(true),
                "and so does the client, of the server's end"
            );
        }
        drop(c);
    }
}
