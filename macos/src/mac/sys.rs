//! Tiny libc shims for macOS: signals, the network addresses, free disk
//! space, local time (no external crates).
//!
//! The structs follow the macOS SDK headers; where they differ from Linux's
//! (`struct sockaddr_in` starts with a length byte, `struct statfs` is not
//! Linux's `statvfs`) that is noted.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

unsafe extern "C" {
    fn signal(sig: c_int, handler: usize) -> usize;
    fn isatty(fd: c_int) -> c_int;
    fn getuid() -> u32;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn flock(fd: c_int, operation: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

/// Set by SIGINT/SIGTERM/SIGHUP (and by the app's Quit). macOS sends
/// SIGTERM to the processes of a session that ends (log out, restart, shut
/// down): the mixer saves and stops.
pub static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: c_int) {
    // Atomic store is async-signal-safe.
    QUIT.store(true, SeqCst);
}

pub fn install_signal_handlers() {
    let h = on_signal as extern "C" fn(c_int) as usize;
    for s in [SIGINT, SIGTERM, SIGHUP] {
        // SAFETY: the handler only performs an atomic store.
        unsafe { signal(s, h) };
    }
}

pub fn stdin_is_tty() -> bool {
    // SAFETY: plain libc calls on the standard descriptors.
    unsafe { isatty(0) == 1 && isatty(1) == 1 }
}

/// The user this process runs as.
pub fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { getuid() }
}

/// Whether a process with this id exists (EPERM: it does, another user's).
/// Signal 0 is never delivered; it only asks.
pub fn process_exists(pid: u32) -> bool {
    const EPERM: c_int = 1;
    let Ok(pid) = c_int::try_from(pid) else { return false };
    // SAFETY: signal 0 is never delivered; pid > 0 names one process.
    pid > 0 && (unsafe { kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(EPERM))
}

/// Mark every file descriptor above stderr close-on-exec, before starting
/// a program that may run for long (a browser, Finder's `open`): one this
/// process opened without the flag would stay open in it.
pub fn cloexec_all() {
    const F_GETFD: c_int = 1;
    const F_SETFD: c_int = 2;
    const FD_CLOEXEC: c_int = 1;
    // macOS lists a process's own descriptors in /dev/fd
    let fds: Vec<c_int> = std::fs::read_dir("/dev/fd")
        .map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default();
    for fd in fds.into_iter().filter(|&fd| fd > 2) {
        // SAFETY: F_GETFD/F_SETFD only change this process's descriptor
        // flags; a descriptor that is already gone just returns -1.
        unsafe {
            let f = fcntl(fd, F_GETFD);
            if f >= 0 && f & FD_CLOEXEC == 0 {
                fcntl(fd, F_SETFD, f | FD_CLOEXEC);
            }
        }
    }
}

/// One BananaSplit per config file: an advisory lock (`flock`) on `path`,
/// held while the returned file stays open, so for the process's life (the
/// system lets go of it however the process ends). Ok(None): another
/// BananaSplit holds it.
pub fn instance_lock(path: &std::path::Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::fd::AsRawFd;
    const LOCK_EX: c_int = 2;
    const LOCK_NB: c_int = 4;
    const EWOULDBLOCK: i32 = 35;
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    // SAFETY: a descriptor of ours, open for the call.
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        return Ok(Some(f));
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(EWOULDBLOCK) { Ok(None) } else { Err(e) }
}

/// `struct statfs` of macOS (the 64-bit-inode one, the only one on Apple
/// silicon).
#[repr(C)]
struct StatFs {
    bsize: u32,
    iosize: i32,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    fsid: [i32; 2],
    owner: u32,
    fs_type: u32,
    flags: u32,
    fssubtype: u32,
    fstypename: [c_char; 16],
    mntonname: [c_char; 1024],
    mntfromname: [c_char; 1024],
    flags_ext: u32,
    reserved: [u32; 7],
}

const _: () = assert!(std::mem::size_of::<StatFs>() == 2168, "struct statfs of the macOS SDK");

unsafe extern "C" {
    // On Intel, the 64-bit-inode struct comes from statfs$INODE64.
    #[cfg_attr(target_arch = "x86_64", link_name = "statfs$INODE64")]
    fn statfs(path: *const c_char, buf: *mut StatFs) -> c_int;
}

/// Total and free (for this user) bytes of the file system that holds
/// `path`, or of its nearest existing parent.
pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    let mut p = path;
    loop {
        if p.exists() {
            let c = CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
            // SAFETY: plain-old-data out-parameter of the right size.
            let mut st: StatFs = unsafe { std::mem::zeroed() };
            // SAFETY: `c` is a NUL-terminated path and `st` a writable `struct statfs`.
            if unsafe { statfs(c.as_ptr(), &mut st) } != 0 {
                return None;
            }
            let unit = u64::from(st.bsize);
            return Some((st.blocks.saturating_mul(unit), st.bavail.saturating_mul(unit)));
        }
        p = p.parent()?;
    }
}

/// An IPv4 address of this computer (for the VBAN window: what to type
/// into Voicemeeter on the other computer).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalIp {
    /// The network interface (`en0`, `en1`).
    pub iface: String,
    pub ip: std::net::Ipv4Addr,
    /// Its network's broadcast address, if it has one.
    pub broadcast: Option<std::net::Ipv4Addr>,
}

/// Interfaces that are usually not the LAN (VPNs, VMs, Apple's peer-to-peer
/// links, the Personal Hotspot): listed last.
fn virtual_iface(name: &str) -> bool {
    [
        "utun", "bridge", "vmenet", "vmnet", "vnic", "awdl", "llw", "anpi", "ap", "gif", "stf", "ipsec", "ppp", "feth",
        "tap", "tun",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}

#[repr(C)]
struct IfAddrs {
    next: *mut IfAddrs,
    name: *const c_char,
    flags: c_uint,
    addr: *const SockAddrIn,
    netmask: *const SockAddrIn,
    /// `ifa_dstaddr`, which is `ifa_broadaddr` on a broadcast interface.
    broadaddr: *const SockAddrIn,
    data: *mut c_void,
}
/// BSD's `struct sockaddr_in`: a length byte, then a one-byte family.
#[repr(C)]
struct SockAddrIn {
    len: u8,
    family: u8,
    port: u16,
    addr: [u8; 4],
    zero: [u8; 8],
}
unsafe extern "C" {
    fn getifaddrs(ifap: *mut *mut IfAddrs) -> c_int;
    fn freeifaddrs(ifa: *mut IfAddrs);
}

/// This computer's IPv4 addresses that are up and running, without
/// loopback: the LAN ones first (`en*`: Ethernet and Wi-Fi), then VPNs and
/// virtual machines' networks.
pub fn local_ipv4() -> Vec<LocalIp> {
    const AF_INET: u8 = 2;
    const IFF_UP: c_uint = 0x1;
    const IFF_BROADCAST: c_uint = 0x2;
    const IFF_LOOPBACK: c_uint = 0x8;
    const IFF_RUNNING: c_uint = 0x40;
    let mut out = Vec::new();
    let mut head: *mut IfAddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a linked list that freeifaddrs releases;
    // every pointer is checked before it is read.
    unsafe {
        if getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let i = &*p;
            p = i.next;
            if i.addr.is_null()
                || (*i.addr).family != AF_INET
                || i.flags & (IFF_UP | IFF_RUNNING) != IFF_UP | IFF_RUNNING
                || i.flags & IFF_LOOPBACK != 0
            {
                continue;
            }
            let name = if i.name.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(i.name).to_string_lossy().into_owned()
            };
            let ip = std::net::Ipv4Addr::from((*i.addr).addr);
            let broadcast =
                (i.flags & IFF_BROADCAST != 0 && !i.broadaddr.is_null() && (*i.broadaddr).family == AF_INET)
                    .then(|| std::net::Ipv4Addr::from((*i.broadaddr).addr))
                    .filter(|b| !b.is_unspecified());
            if !out.iter().any(|x: &LocalIp| x.ip == ip) {
                out.push(LocalIp { iface: name, ip, broadcast });
            }
        }
        freeifaddrs(head);
    }
    let rank = |x: &LocalIp| (virtual_iface(&x.iface), x.ip.is_link_local(), !x.iface.starts_with("en"));
    out.sort_by_key(|x| rank(x));
    out
}

// The leading fields of `struct tm`; `_rest` covers the remaining ints,
// tm_gmtoff and tm_zone with room to spare, aligned as they are (8).
// `time_t` is 64-bit.
#[repr(C, align(8))]
struct Tm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    _rest: [u8; 128],
}
unsafe extern "C" {
    fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
}

/// Current local time as `YYYY-MM-DD_HH-MM-SS` (for recording file names).
pub fn local_timestamp() -> String {
    let secs =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    // SAFETY: plain-old-data out-parameter large enough for struct tm.
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals; localtime_r writes only `tm`.
    if !unsafe { localtime_r(&secs, &mut tm) }.is_null() {
        return format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            tm.year + 1900,
            tm.mon + 1,
            tm.mday,
            tm.hour,
            tm.min,
            tm.sec
        );
    }
    utc_timestamp(secs)
}

/// `YYYY-MM-DD_HH-MM-SSZ` in UTC (civil-from-days, proleptic Gregorian).
pub fn utc_timestamp(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses() {
        let ips = local_ipv4();
        assert!(ips.iter().all(|x| !x.ip.is_loopback() && !x.iface.is_empty()), "{ips:?}");
        assert!(
            virtual_iface("utun3")
                && virtual_iface("bridge100")
                && virtual_iface("awdl0")
                && !virtual_iface("en0")
                && !virtual_iface("en7")
        );
    }

    #[test]
    fn disk_space_of_a_folder_that_does_not_exist_yet() {
        let (total, free) = disk_space(&std::env::temp_dir().join("bs-no-such-dir/a/b")).unwrap();
        assert!(total > 0 && free <= total, "{total} {free}");
    }

    #[test]
    fn cloexec_all_marks_inherited_descriptors() {
        let f = std::fs::File::open("/dev/null").unwrap();
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn close(fd: c_int) -> c_int;
        }
        // SAFETY: F_DUPFD (0) makes a copy without close-on-exec, closed below.
        let fd = unsafe { fcntl(f.as_raw_fd(), 0, 3) };
        assert!(fd > 2);
        // SAFETY: F_GETFD (1) on a descriptor this test owns.
        assert_eq!(unsafe { fcntl(fd, 1) } & 1, 0);
        cloexec_all();
        // SAFETY: as above.
        assert_eq!(unsafe { fcntl(fd, 1) } & 1, 1);
        // SAFETY: closes the copy made above, which nothing else uses.
        unsafe { close(fd) };
    }

    #[test]
    fn one_instance_per_lock_file() {
        let p = std::env::temp_dir().join(format!("bs-instance-{}.lock", std::process::id()));
        let first = instance_lock(&p).unwrap();
        assert!(first.is_some());
        assert!(instance_lock(&p).unwrap().is_none(), "held by the first");
        drop(first);
        assert!(instance_lock(&p).unwrap().is_some(), "let go");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn timestamps() {
        assert_eq!(utc_timestamp(0), "1970-01-01_00-00-00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29_00-00-00Z");
        assert_eq!(utc_timestamp(1_792_339_880), "2026-10-18_16-11-20Z");
        assert_eq!(utc_timestamp(-1), "1969-12-31_23-59-59Z");
        let t = local_timestamp();
        assert!(t.len() >= 19 && t.as_bytes()[4] == b'-' && t.as_bytes()[10] == b'_', "{t}");
    }
}
