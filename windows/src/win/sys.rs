//! Windows helpers used by the GUI and main loop.

use crate::win::ffi::*;
use crate::win::console;
use std::ffi::c_void;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use crate::win::console::{install_signal_handlers, leave_own_console, pause_on_error, spawn_input_thread, stdin_is_tty, RawTerminal, QUIT};

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
