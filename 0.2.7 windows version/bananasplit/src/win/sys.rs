//! Windows helpers used by the GUI and main loop.

use crate::win::ffi::*;
use crate::win::console;
use std::ffi::c_void;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use crate::win::console::{install_signal_handlers, pause_on_error, spawn_input_thread, stdin_is_tty, RawTerminal, QUIT};

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
    prefix: u8,
    suffix: u8,
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
                        let ip = (*sa).sin_addr;
                        let b = ip as u32;
                        let bytes = b.to_le_bytes();
                        let ipv4 = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
                        let prefix = ua.on_link_prefix_length;
                        let mask = if prefix == 0 { 0 } else { (!0u32) << (32 - prefix.min(32)) };
                        let broadcast = Ipv4Addr::from((b | (!mask)) as u32);
                        out.push(LocalIp { iface: iface.clone(), ip: ipv4, broadcast: Some(broadcast) });
                    }
                    u = ua.next;
                }
            }
            cur = a.next;
        }
        out
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
