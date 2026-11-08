//! What the shared GUI layer (`gui/` at the root of the repository) needs from
//! Linux. The list of items is the platform contract documented in
//! `gui/mod.rs`; every app has the same one.

use crate::gui::{Form, Hub};
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
    let owner = socket_owner(&t4, peer, us).or_else(|| {
        let t6 = std::fs::read_to_string("/proc/net/tcp6").ok()?;
        socket_owner(&t6, mapped(peer), mapped(us))
    });
    owner.map(|o| o == crate::sys::uid())
}

/// The owner (uid) of the socket at the other end of a loopback connection:
/// in `/proc/net/tcp` (or `tcp6`, for a dual-stack IPv6 socket that came in
/// as `::ffff:127.0.0.1`), its line has `peer` as local and `us` as remote
/// address. The kernel prints each 32-bit word of an address in host order.
fn socket_owner(table: &str, peer: SocketAddr, us: SocketAddr) -> Option<u32> {
    let hex = |a: SocketAddr| {
        let words = |b: &[u8]| {
            b.chunks(4).map(|w| format!("{:08X}", u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))).collect::<String>()
        };
        let ip = match a {
            SocketAddr::V4(v) => words(&v.ip().octets()),
            SocketAddr::V6(v) => words(&v.ip().octets()),
        };
        format!("{ip}:{:04X}", a.port())
    };
    let (p, u) = (hex(peer), hex(us));
    table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 7 && f[1].eq_ignore_ascii_case(&p) && f[2].eq_ignore_ascii_case(&u))
            .then(|| f[7].parse().ok())
            .flatten()
    })
}

/// `a` as an IPv4-mapped IPv6 address (how a dual-stack socket sees it).
fn mapped(a: SocketAddr) -> SocketAddr {
    match a {
        SocketAddr::V4(v) => SocketAddr::from((v.ip().to_ipv6_mapped(), v.port())),
        v6 => v6,
    }
}

/// The folder above `bananasplit/` where a running GUI notes its port
/// (`$XDG_RUNTIME_DIR/bananasplit/<client>.port`).
/// In a Flatpak only `$XDG_RUNTIME_DIR/app/<app ID>` is shared by its instances.
pub fn port_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| match crate::desktop::flatpak_id() {
            Some(id) => p.join("app").join(id),
            None => p,
        })
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache")))
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

    #[test]
    fn only_our_user_gets_in() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:2256 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0 100 0 0 10 0\n\
           1: 0100007F:A1B2 0100007F:2256 01 00000000:00000000 00:00000000 00000000  1001        0 2 1 0 20 4 30 10 -1\n\
           2: 0100007F:2256 0100007F:A1B2 01 00000000:00000000 00:00000000 00000000  1000        0 3 1 0 20 4 30 10 -1\n";
        let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
        if cfg!(target_endian = "little") {
            assert_eq!(socket_owner(table, at(0xA1B2), at(0x2256)), Some(1001), "the client's own line");
            assert_eq!(socket_owner(table, at(0xA1B3), at(0x2256)), None);
            // a dual-stack IPv6 client (Java, .NET) is listed in tcp6 as ::ffff:127.0.0.1
            let t6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
               0: 0000000000000000FFFF00000100007F:A1B2 0000000000000000FFFF00000100007F:2256 01 00000000:00000000 00:00000000 00000000  1002        0 4 1 0 20 4 30 10 -1\n";
            assert_eq!(socket_owner(t6, mapped(at(0xA1B2)), mapped(at(0x2256))), Some(1002));
            assert_eq!(socket_owner(t6, at(0xA1B2), at(0x2256)), None, "IPv4 form does not match a tcp6 line");
        }
        // a real connection: the owner is this process's user
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (srv, _) = l.accept().unwrap();
        if let Ok(t) = std::fs::read_to_string("/proc/net/tcp") {
            assert_eq!(socket_owner(&t, srv.peer_addr().unwrap(), srv.local_addr().unwrap()), Some(crate::sys::uid()));
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
