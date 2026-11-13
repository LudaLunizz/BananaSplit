//! Windows helpers used by the GUI and main loop.

use crate::gui::peer::win_table::owner_pid;
use crate::win::ffi::*;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

pub use crate::win::console::{
    QUIT, RawTerminal, install_signal_handlers, pause_on_error, spawn_input_thread, stdin_is_tty,
};

/// Started by the user (a double-click, the Start menu), this process may
/// bring a window to the front. Pass that on, so the running BananaSplit's
/// window can come up when it is asked to.
pub fn allow_foreground() {
    // SAFETY: plain Win32 call.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

#[derive(Debug, Clone)]
pub struct LocalIp {
    pub iface: String,
    pub ip: Ipv4Addr,
    pub broadcast: Option<Ipv4Addr>,
}

const AF_INET: u32 = 2;
const GAA_FLAG_SKIP_ANYCAST: u32 = 0x2;
const GAA_FLAG_SKIP_MULTICAST: u32 = 0x4;
const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x8;

pub fn local_ipv4() -> Vec<LocalIp> {
    const FLAGS: u32 = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    // u64 elements: the list is read as IP_ADAPTER_ADDRESSES (8-byte aligned).
    let mut buf: Vec<u64> = Vec::new();
    let mut size: u32 = 0;
    let mut filled = false;
    // Adapters can change between asking for the size and filling it in.
    for _ in 0..5 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let p = if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr().cast() };
        // SAFETY: `size` is the buffer's size in bytes (0 with no buffer).
        let r = unsafe { GetAdaptersAddresses(AF_INET, FLAGS, std::ptr::null_mut(), p, &mut size) };
        if r == 0 && !buf.is_empty() {
            filled = true;
            break;
        }
        if (r != ERROR_BUFFER_OVERFLOW && r != 0) || size == 0 {
            return Vec::new();
        }
    }
    if !filled {
        return Vec::new();
    }
    // SAFETY: `filled`: GetAdaptersAddresses wrote its list into `buf` (8-byte aligned u64s), and every next/unicast/name/sockaddr pointer in it is NULL or points into `buf`, which outlives this block.
    unsafe {
        let addrs = buf.as_mut_ptr() as *mut IpAdapterAddresses;
        let mut out = Vec::new();
        let mut cur = addrs;
        while !cur.is_null() {
            let a = &*cur;
            if a.oper_status == 1 {
                // Up
                let iface = if !a.friendly_name.is_null() { from_wide(a.friendly_name) } else { String::new() };
                let mut u = a.first_unicast;
                while !u.is_null() {
                    let ua = &*u;
                    let sa = ua.addr.sock as *const SockaddrIn;
                    if !sa.is_null() && (*sa).sin_family == 2 {
                        // sin_addr is in network byte order: its bytes in memory are the octets.
                        let ipv4 = Ipv4Addr::from((*sa).sin_addr.to_ne_bytes());
                        if !ipv4.is_loopback() {
                            let broadcast = broadcast_of(ipv4, ua.on_link_prefix_length);
                            out.push(LocalIp { iface: iface.clone(), ip: ipv4, broadcast });
                        }
                    }
                    u = ua.next;
                }
            }
            cur = a.next;
        }
        out
    }
}

/// The broadcast address of `ip`'s network, given its prefix length; none
/// for a /31 or /32 (no room for one) or an impossible prefix.
fn broadcast_of(ip: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix > 30 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix)) };
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

// ---------------------------------------------------------------------------
// Which user is on the other end of a loopback connection
// ---------------------------------------------------------------------------

const AF_INET6: u32 = 23;
const TCP_TABLE_OWNER_PID_ALL: u32 = 5;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;

/// The TCP connection table with owning processes (`MIB_TCPTABLE_OWNER_PID`
/// for IPv4, `MIB_TCP6TABLE_OWNER_PID` for IPv6), as bytes.
fn tcp_table(af: u32) -> Option<Vec<u8>> {
    let mut size = 0u32;
    for _ in 0..5 {
        // u32 elements: the rows are read as bytes, but written as DWORDs.
        let mut buf = vec![0u32; (size as usize).div_ceil(4)];
        let p = if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr().cast() };
        // SAFETY: `size` is the buffer's size in bytes (0 with no buffer).
        let r = unsafe { GetExtendedTcpTable(p, &mut size, 0, af, TCP_TABLE_OWNER_PID_ALL, 0) };
        if r == 0 && !buf.is_empty() {
            let mut bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
            bytes.truncate(size as usize);
            return Some(bytes);
        }
        if r != ERROR_INSUFFICIENT_BUFFER && r != 0 {
            return None;
        }
    }
    None
}

/// The SID of the user a process runs as (`None` if it cannot be read).
fn process_user(process: HANDLE) -> Option<Vec<u8>> {
    // SAFETY: plain Win32 calls; the token handle is closed before return,
    // and the SID is copied out of the buffer it points into.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len) != 0;
        CloseHandle(token);
        if !ok {
            return None;
        }
        // TOKEN_USER starts with a pointer to the SID, which lies in `buf`.
        let sid = *(buf.as_ptr() as *const *const u8);
        if sid.is_null() {
            return None;
        }
        // A SID: revision, sub-authority count, 6-byte authority, then the
        // sub-authorities (4 bytes each).
        let count = usize::from(*sid.add(1));
        Some(std::slice::from_raw_parts(sid, 8 + 4 * count).to_vec())
    }
}

/// Is the process at the other end of this loopback connection run by the
/// same user as BananaSplit? Other users of this computer (another account,
/// fast user switching, Remote Desktop) can reach 127.0.0.1 too, and must
/// not drive the mixer. Unknown (the connection is not in the tables, or its
/// process cannot be looked at): no. If Windows gives no tables at all: no
/// as well (up to 2.8.24 it was let in).
pub fn same_user(peer: SocketAddr, us: SocketAddr) -> bool {
    let tables = [(false, tcp_table(AF_INET)), (true, tcp_table(AF_INET6))];
    if tables.iter().all(|(_, t)| t.is_none()) {
        crate::gui::peer::list_unreadable("the TCP connection tables (GetExtendedTcpTable)");
        return false;
    }
    let Some(pid) = tables.iter().find_map(|(v6, t)| t.as_deref().and_then(|t| owner_pid(t, *v6, peer, us))) else {
        return false;
    };
    // SAFETY: plain Win32 calls; the process handle is closed after use.
    unsafe {
        if pid == GetCurrentProcessId() {
            return true;
        }
        let (mut theirs, mut ours) = (0u32, 0u32);
        if ProcessIdToSessionId(pid, &mut theirs) != 0
            && ProcessIdToSessionId(GetCurrentProcessId(), &mut ours) != 0
            && theirs != ours
        {
            return false;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let them = process_user(h);
        CloseHandle(h);
        let (Some(mut them), Some(mut me)) = (them, process_user(GetCurrentProcess())) else { return false };
        EqualSid(them.as_mut_ptr().cast(), me.as_mut_ptr().cast()) != 0
    }
}

pub fn local_timestamp() -> String {
    let mut t =
        SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, milliseconds: 0 };
    // SAFETY: `t` is a local SYSTEMTIME with the SDK layout.
    unsafe {
        GetLocalTime(&mut t);
    }
    format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut cur = path.to_path_buf();
    loop {
        let w = to_wide(cur.as_os_str());
        let mut free_avail: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        // SAFETY: `w` is a NUL-terminated UTF-16 path held in a local; the out-pointers are local u64s.
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free_avail, &mut total, &mut total_free) };
        if ok != 0 {
            return Some((total_free, free_avail));
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

pub fn cloexec_all() {}

pub fn term_cols() -> Option<usize> {
    None
}

pub fn regex_match(_pattern: &str, _text: &str) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_struct_matches_the_sdk() {
        // IP_ADAPTER_UNICAST_ADDRESS_LH: OnLinkPrefixLength is at offset 56 on x64.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::offset_of!(IpAdapterUnicastAddress, on_link_prefix_length), 56);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::offset_of!(IpAdapterAddresses, oper_status), 104);
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_from_this_process_is_ours() {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (srv, _) = l.accept().unwrap();
        let t = tcp_table(AF_INET).expect("the IPv4 TCP table");
        // SAFETY: plain call, no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        assert_eq!(owner_pid(&t, false, srv.peer_addr().unwrap(), srv.local_addr().unwrap()), Some(pid));
        assert!(same_user(srv.peer_addr().unwrap(), srv.local_addr().unwrap()));
        // SAFETY: the current-process pseudo-handle is always valid and needs no CloseHandle.
        assert!(process_user(unsafe { GetCurrentProcess() }).is_some_and(|s| s.len() >= 12));
    }

    #[test]
    fn broadcast_addresses() {
        let ip = Ipv4Addr::new(192, 168, 1, 23);
        assert_eq!(broadcast_of(ip, 24), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(broadcast_of(ip, 16), Some(Ipv4Addr::new(192, 168, 255, 255)));
        assert_eq!(broadcast_of(Ipv4Addr::new(10, 1, 0, 232), 20), Some(Ipv4Addr::new(10, 1, 15, 255)));
        assert_eq!(broadcast_of(ip, 0), Some(Ipv4Addr::BROADCAST));
        assert_eq!(broadcast_of(ip, 31), None);
        assert_eq!(broadcast_of(ip, 32), None);
        assert_eq!(broadcast_of(ip, 128), None);
    }
}
