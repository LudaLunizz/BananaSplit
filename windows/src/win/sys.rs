//! Windows helpers used by the GUI and main loop.

use crate::win::ffi::*;
use crate::win::console;
use std::ffi::c_void;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use crate::win::console::{install_signal_handlers, pause_on_error, spawn_input_thread, stdin_is_tty, RawTerminal, QUIT};

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

#[repr(C)]
struct RawSockaddr {
    family: u16,
    data: [u8; 14],
}

#[repr(C)]
struct SocketAddress {
    sock: *const RawSockaddr,
    len: i32,
}

#[repr(C)]
struct IpAdapterUnicastAddress {
    len: u32,
    flags: u32,
    next: *mut IpAdapterUnicastAddress,
    addr: SocketAddress,
    // IP_PREFIX_ORIGIN, IP_SUFFIX_ORIGIN and IP_DAD_STATE are C enums: 4 bytes each.
    prefix: u32,
    suffix: u32,
    dad: u32,
    valid_lifetime: u32,
    preferred_lifetime: u32,
    lease_lifetime: u32,
    on_link_prefix_length: u8,
}

#[repr(C)]
struct IpAdapterAddresses {
    len: u32,
    if_index: u32,
    next: *mut IpAdapterAddresses,
    adapter_name: *const i8,
    first_unicast: *mut IpAdapterUnicastAddress,
    first_anycast: *mut c_void,
    first_multicast: *mut c_void,
    first_dns: *mut c_void,
    dns_suffix: *const u16,
    description: *const u16,
    friendly_name: *const u16,
    phys_addr: [u8; 8],
    phys_len: u32,
    flags: u32,
    mtu: u32,
    if_type: u32,
    oper_status: u32,
    ipv6_if_index: u32,
    zone: [u32; 16],
    first_prefix: *mut c_void,
    transmit_link_speed: u64,
    receive_link_speed: u64,
    first_wins: *mut c_void,
}

#[repr(C)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut c_void,
        adapter_addresses: *mut IpAdapterAddresses,
        size: *mut u32,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDiskFreeSpaceExW(
        dir: *const u16,
        free_bytes_avail: *mut u64,
        total_bytes: *mut u64,
        total_free: *mut u64,
    ) -> i32;
}

const AF_INET: u32 = 2;
const GAA_FLAG_SKIP_ANYCAST: u32 = 0x2;
const GAA_FLAG_SKIP_MULTICAST: u32 = 0x4;
const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x8;

pub fn local_ipv4() -> Vec<LocalIp> {
    let mut size: u32 = 0;
    unsafe {
        let r = GetAdaptersAddresses(AF_INET, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER, std::ptr::null_mut(), std::ptr::null_mut(), &mut size);
        if r != 111 && r != 0 { return Vec::new(); }
        if size == 0 { return Vec::new(); }
        let mut buf = vec![0u8; size as usize];
        let addrs = buf.as_mut_ptr() as *mut IpAdapterAddresses;
        if GetAdaptersAddresses(AF_INET, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER, std::ptr::null_mut(), addrs, &mut size) != 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut cur = addrs;
        while !cur.is_null() {
            let a = &*cur;
            if a.oper_status == 1 { // Up
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

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: i32, af: u32, class: u32, reserved: u32) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn GetCurrentProcess() -> HANDLE;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: HANDLE, access: u32, token: *mut HANDLE) -> i32;
    fn GetTokenInformation(token: HANDLE, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn EqualSid(a: *mut c_void, b: *mut c_void) -> i32;
}

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

/// The process that owns the connection from `peer` to `us`, found in a
/// table from `tcp_table`: its row has `peer` as the local and `us` as the
/// remote end. Addresses are stored as their bytes; ports in network order
/// in the low two bytes of a DWORD.
pub fn owner_pid(table: &[u8], v6: bool, peer: SocketAddr, us: SocketAddr) -> Option<u32> {
    let u32_at = |i: usize| table.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let port_at = |i: usize| table.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let v6_of = |a: SocketAddr| match a {
        SocketAddr::V4(v) => v.ip().to_ipv6_mapped().octets(),
        SocketAddr::V6(v) => v.ip().octets(),
    };
    let n = u32_at(0)? as usize;
    let row = if v6 { 56 } else { 24 };
    (0..n).find_map(|k| {
        let r = 4 + k * row;
        let (local, lport, remote, rport, pid) = if v6 {
            (table.get(r..r + 16)?.to_vec(), port_at(r + 20)?, table.get(r + 24..r + 40)?.to_vec(), port_at(r + 44)?, u32_at(r + 52)?)
        } else {
            (table.get(r + 4..r + 8)?.to_vec(), port_at(r + 8)?, table.get(r + 12..r + 16)?.to_vec(), port_at(r + 16)?, u32_at(r + 20)?)
        };
        let addr = |a: SocketAddr| -> Vec<u8> {
            match (v6, a) {
                (false, SocketAddr::V4(v)) => v.ip().octets().to_vec(),
                (false, SocketAddr::V6(v)) => v.ip().to_ipv4_mapped().map(|i| i.octets().to_vec()).unwrap_or_default(),
                (true, a) => v6_of(a).to_vec(),
            }
        };
        (local == addr(peer) && lport == peer.port() && remote == addr(us) && rport == us.port()).then_some(pid)
    })
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
/// process cannot be looked at): no. If Windows gives no tables at all, as
/// before: yes.
pub fn same_user(peer: SocketAddr, us: SocketAddr) -> bool {
    let tables = [(false, tcp_table(AF_INET)), (true, tcp_table(AF_INET6))];
    if tables.iter().all(|(_, t)| t.is_none()) {
        return true;
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
        if ProcessIdToSessionId(pid, &mut theirs) != 0 && ProcessIdToSessionId(GetCurrentProcessId(), &mut ours) != 0 && theirs != ours {
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
    let mut t = SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, milliseconds: 0 };
    unsafe { GetLocalTime(&mut t); }
    format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

#[repr(C)]
struct SystemTime { year: u16, month: u16, day_of_week: u16, day: u16, hour: u16, minute: u16, second: u16, milliseconds: u16 }
unsafe extern "system" { fn GetLocalTime(out: *mut SystemTime); }

pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut cur = path.to_path_buf();
    loop {
        let w = to_wide(cur.as_os_str());
        let mut free_avail: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free_avail, &mut total, &mut total_free) };
        if ok != 0 {
            return Some((total_free, free_avail));
        }
        if !cur.pop() { break; }
    }
    None
}

pub fn cloexec_all() {}
pub fn uid() -> u32 { 0 }

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

    /// A table as `GetExtendedTcpTable` writes it: the count, then rows.
    fn v4_table(rows: &[([u8; 4], u16, [u8; 4], u16, u32)]) -> Vec<u8> {
        let mut t = (rows.len() as u32).to_le_bytes().to_vec();
        for (la, lp, ra, rp, pid) in rows {
            t.extend_from_slice(&5u32.to_le_bytes()); // ESTABLISHED
            t.extend_from_slice(la);
            t.extend_from_slice(&[(lp >> 8) as u8, *lp as u8, 0, 0]);
            t.extend_from_slice(ra);
            t.extend_from_slice(&[(rp >> 8) as u8, *rp as u8, 0, 0]);
            t.extend_from_slice(&pid.to_le_bytes());
        }
        t
    }

    #[test]
    fn connection_owner_from_the_table() {
        let lo = [127, 0, 0, 1];
        let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
        // the server's own end (pid 10) and the client's (pid 42)
        let t = v4_table(&[(lo, 8790, lo, 51000, 10), (lo, 51000, lo, 8790, 42), (lo, 51001, lo, 8790, 43)]);
        assert_eq!(owner_pid(&t, false, at(51000), at(8790)), Some(42));
        assert_eq!(owner_pid(&t, false, at(51001), at(8790)), Some(43));
        assert_eq!(owner_pid(&t, false, at(51002), at(8790)), None);
        assert_eq!(owner_pid(&t[..10], false, at(51000), at(8790)), None, "a cut-off table");
        // a dual-stack client (::ffff:127.0.0.1) in the IPv6 table
        let mapped = Ipv4Addr::LOCALHOST.to_ipv6_mapped().octets();
        let mut t6 = 1u32.to_le_bytes().to_vec();
        t6.extend_from_slice(&mapped);
        t6.extend_from_slice(&0u32.to_le_bytes());
        t6.extend_from_slice(&[0xC7, 0x38, 0, 0]); // 51000
        t6.extend_from_slice(&mapped);
        t6.extend_from_slice(&0u32.to_le_bytes());
        t6.extend_from_slice(&[0x22, 0x56, 0, 0]); // 8790
        t6.extend_from_slice(&5u32.to_le_bytes());
        t6.extend_from_slice(&77u32.to_le_bytes());
        assert_eq!(owner_pid(&t6, true, at(51000), at(8790)), Some(77));
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_from_this_process_is_ours() {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (srv, _) = l.accept().unwrap();
        let t = tcp_table(AF_INET).expect("the IPv4 TCP table");
        let pid = unsafe { GetCurrentProcessId() };
        assert_eq!(owner_pid(&t, false, srv.peer_addr().unwrap(), srv.local_addr().unwrap()), Some(pid));
        assert!(same_user(srv.peer_addr().unwrap(), srv.local_addr().unwrap()));
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
