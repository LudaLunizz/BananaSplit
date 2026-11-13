//! What the shared GUI layer (`gui/` at the root of the repository) needs from
//! Linux. The list of items is the platform contract documented in
//! `gui/mod.rs`; every app has the same one.

use crate::gui::peer::proc_net_tcp::{mapped, owner};
use crate::gui::{Form, Hub, portdir};
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The system, as the page is told it (`<html data-platform>`).
pub const PLATFORM: &str = "linux";

pub use crate::desktop::{autostart_enabled, install, menu_installed, open_folder, open_page, set_autostart};
pub use crate::sys::{LocalIp, local_timestamp};
pub use crate::update::Updater;

/// Whose process is at the other end of a loopback connection: `Some(true)`
/// ours, `Some(false)` another user's, `None` not (yet) in the system's list,
/// so the caller asks again. Without a readable /proc/net/tcp: not ours, the
/// connection is refused (up to 2.8.24 it was let in).
pub fn peer_is_ours(peer: SocketAddr, us: SocketAddr) -> Option<bool> {
    let t4 = std::fs::read_to_string("/proc/net/tcp").ok();
    ours_in(t4.as_deref(), || std::fs::read_to_string("/proc/net/tcp6").ok(), peer, us, crate::sys::uid())
}

/// [`peer_is_ours`] on the tables it was given (`tcp6` is read only when
/// the connection is not in `tcp`).
fn ours_in(
    t4: Option<&str>,
    t6: impl FnOnce() -> Option<String>,
    peer: SocketAddr,
    us: SocketAddr,
    uid: u32,
) -> Option<bool> {
    let Some(t4) = t4 else {
        crate::gui::peer::list_unreadable("/proc/net/tcp");
        return Some(false);
    };
    let owner = owner(t4, peer, us).or_else(|| owner(&t6()?, mapped(peer), mapped(us)));
    owner.map(|o| o == uid)
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

    /// No connection list to read: refused, not let in.
    #[test]
    fn an_unreadable_list_refuses() {
        let (p, u) = ("127.0.0.1:40000".parse().unwrap(), "127.0.0.1:8790".parse().unwrap());
        assert_eq!(ours_in(None, || None, p, u, 1000), Some(false));
        // the same connection in a readable list still gets in
        let lo = u32::from_ne_bytes([127, 0, 0, 1]);
        let t = format!(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid\n   0: {lo:08X}:9C40 {lo:08X}:2256 01 00000000:00000000 00:00000000 00000000  1000\n"
        );
        assert_eq!(ours_in(Some(&t), || None, p, u, 1000), Some(true));
        assert_eq!(ours_in(Some(&t), || None, p, u, 1001), Some(false));
        // not in either list (yet): ask again
        assert_eq!(ours_in(Some("header\n"), || Some("header\n".into()), p, u, 1000), None);
    }

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
