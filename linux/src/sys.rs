//! Tiny libc shims: signals, terminal raw mode and size, POSIX regex
//! (no external crates).

use std::ffi::{CString, c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

// glibc/musl `struct termios` layout below is the same on these targets.
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "x86",
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "riscv64"
)))]
compile_error!("termios layout only verified for x86, x86_64, arm, aarch64 and riscv64 Linux");

#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
    /// Safety margin in case a libc writes a slightly larger struct.
    _pad: [u8; 64],
}

const ISIG: u32 = 0o000001;
const ICANON: u32 = 0o000002;
const ECHO: u32 = 0o000010;
const IEXTEN: u32 = 0o100000;
const ICRNL: u32 = 0o000400;
const IXON: u32 = 0o002000;
const VTIME: usize = 5;
const VMIN: usize = 6;
const TCSANOW: c_int = 0;

const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGQUIT: c_int = 3;
const SIGTERM: c_int = 15;
const SIG_IGN: usize = 1;
/// Same value on every architecture allowed by the check above.
const TIOCGWINSZ: c_ulong = 0x5413;
const REG_EXTENDED: c_int = 1;
const REG_NOSUB: c_int = 8; // glibc and musl agree

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// Opaque storage for `regex_t` (64 bytes on glibc, less on musl).
#[repr(C, align(16))]
struct RegexT([u8; 256]);

unsafe extern "C" {
    fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, act: c_int, t: *const Termios) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
    fn ioctl(fd: c_int, req: c_ulong, ...) -> c_int;
    fn regcomp(re: *mut RegexT, pattern: *const c_char, cflags: c_int) -> c_int;
    fn regexec(re: *const RegexT, s: *const c_char, nmatch: usize, pmatch: *mut c_void, eflags: c_int) -> c_int;
    fn regfree(re: *mut RegexT);
    fn close(fd: c_int) -> c_int;
    fn getuid() -> u32;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
}

/// Mark every file descriptor above stderr close-on-exec, before starting
/// a program that may run for long (a file manager, a browser): libjack's
/// sockets are not always marked, and a child holding them keeps the JACK
/// client of a quit or killed BananaSplit looking alive.
pub fn cloexec_all() {
    const F_GETFD: c_int = 1;
    const F_SETFD: c_int = 2;
    const FD_CLOEXEC: c_int = 1;
    let fds: Vec<c_int> = std::fs::read_dir("/proc/self/fd")
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

/// The user this process runs as.
pub fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { getuid() }
}

/// Close every file descriptor above stderr. For the cable watchdog: it is
/// started after libjack opened its sockets, and C code does not always
/// mark them close-on-exec. Holding them would keep a killed BananaSplit's JACK
/// client looking alive.
pub fn close_inherited_fds() {
    let fds: Vec<c_int> = std::fs::read_dir("/proc/self/fd")
        .map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default();
    for fd in fds.into_iter().filter(|&fd| fd > 2) {
        // SAFETY: closing an fd this process does not use (or one that is
        // already gone, e.g. the directory handle above) is harmless.
        unsafe { close(fd) };
    }
}

/// Set by SIGINT/SIGTERM/SIGHUP (and by `q` in the UI).
pub static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: c_int) {
    // Atomic store is async-signal-safe.
    QUIT.store(true, SeqCst);
}

pub fn install_signal_handlers() {
    let h = on_signal as extern "C" fn(c_int) as usize;
    for s in [SIGINT, SIGTERM, SIGHUP] {
        // SAFETY: handler only performs an atomic store.
        unsafe { signal(s, h) };
    }
}

/// For the cable watchdog: survive Ctrl-C, `systemctl stop` and a closed
/// terminal, so it is still there when the main process is gone.
pub fn ignore_termination_signals() {
    for s in [SIGINT, SIGTERM, SIGHUP, SIGQUIT] {
        // SAFETY: SIG_IGN is always a valid disposition.
        unsafe { signal(s, SIG_IGN) };
    }
}

/// Total and free (for unprivileged users) bytes of the file system that
/// holds `path`, or of its nearest existing parent. 64-bit targets only
/// (`struct statvfs` is all 64-bit fields there on glibc and musl).
pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    #[cfg(target_pointer_width = "64")]
    {
        #[repr(C)]
        struct StatVfs {
            bsize: u64,
            frsize: u64,
            blocks: u64,
            bfree: u64,
            bavail: u64,
            files: u64,
            ffree: u64,
            favail: u64,
            fsid: u64,
            flag: u64,
            namemax: u64,
            _spare: [u8; 128],
        }
        unsafe extern "C" {
            fn statvfs(path: *const c_char, buf: *mut StatVfs) -> c_int;
        }
        let mut p = path;
        loop {
            if p.exists() {
                let c = CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
                // SAFETY: plain-old-data out-parameter with room to spare.
                let mut st: StatVfs = unsafe { std::mem::zeroed() };
                if unsafe { statvfs(c.as_ptr(), &mut st) } != 0 {
                    return None;
                }
                let unit = if st.frsize > 0 { st.frsize } else { st.bsize };
                return Some((st.blocks.saturating_mul(unit), st.bavail.saturating_mul(unit)));
            }
            p = p.parent()?;
        }
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let _ = path;
        None
    }
}

/// An IPv4 address of this computer (for the VBAN window: what to type
/// into Voicemeeter on the other computer).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalIp {
    /// The network interface (`enp3s0`, `wlan0`).
    pub iface: String,
    pub ip: std::net::Ipv4Addr,
    /// Its network's broadcast address, if it has one.
    pub broadcast: Option<std::net::Ipv4Addr>,
}

/// Interfaces that are usually not the LAN (containers, VMs, VPNs): listed last.
fn virtual_iface(name: &str) -> bool {
    ["docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "lxc", "lxd", "podman", "cni", "flannel", "tun", "tap", "wg", "tailscale", "zt", "ham"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// This computer's IPv4 addresses that are up and running (an unplugged
/// cable or an idle Docker bridge is skipped), without loopback: the LAN
/// ones first (wired, then the rest), then container, VM and VPN interfaces.
pub fn local_ipv4() -> Vec<LocalIp> {
    #[repr(C)]
    struct IfAddrs {
        next: *mut IfAddrs,
        name: *const c_char,
        flags: std::ffi::c_uint,
        addr: *const SockAddrIn,
        netmask: *const SockAddrIn,
        broadaddr: *const SockAddrIn,
        data: *mut c_void,
    }
    #[repr(C)]
    struct SockAddrIn {
        family: u16,
        port: u16,
        addr: [u8; 4],
        zero: [u8; 8],
    }
    unsafe extern "C" {
        fn getifaddrs(ifap: *mut *mut IfAddrs) -> c_int;
        fn freeifaddrs(ifa: *mut IfAddrs);
    }
    const AF_INET: u16 = 2;
    const IFF_UP: std::ffi::c_uint = 0x1;
    const IFF_BROADCAST: std::ffi::c_uint = 0x2;
    const IFF_LOOPBACK: std::ffi::c_uint = 0x8;
    const IFF_RUNNING: std::ffi::c_uint = 0x40;
    let mut out = Vec::new();
    let mut head: *mut IfAddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a linked list that freeifaddrs releases; every
    // pointer is checked before it is read.
    unsafe {
        if getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let i = &*p;
            p = i.next;
            if i.addr.is_null() || (*i.addr).family != AF_INET || i.flags & (IFF_UP | IFF_RUNNING) != IFF_UP | IFF_RUNNING || i.flags & IFF_LOOPBACK != 0 {
                continue;
            }
            let name = if i.name.is_null() { String::new() } else { std::ffi::CStr::from_ptr(i.name).to_string_lossy().into_owned() };
            let ip = std::net::Ipv4Addr::from((*i.addr).addr);
            let broadcast = (i.flags & IFF_BROADCAST != 0 && !i.broadaddr.is_null() && (*i.broadaddr).family == AF_INET)
                .then(|| std::net::Ipv4Addr::from((*i.broadaddr).addr))
                .filter(|b| !b.is_unspecified());
            if !out.iter().any(|x: &LocalIp| x.ip == ip) {
                out.push(LocalIp { iface: name, ip, broadcast });
            }
        }
        freeifaddrs(head);
    }
    let rank = |x: &LocalIp| (virtual_iface(&x.iface), x.ip.is_link_local(), !(x.iface.starts_with("en") || x.iface.starts_with("eth")));
    out.sort_by_key(|x| rank(x));
    out
}

pub fn stdin_is_tty() -> bool {
    unsafe { isatty(0) == 1 && isatty(1) == 1 }
}

/// Width of the terminal on stdout, if it is one.
pub fn term_cols() -> Option<usize> {
    let mut w = Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCGWINSZ writes one struct winsize.
    let ok = unsafe { ioctl(1, TIOCGWINSZ, &mut w as *mut Winsize) } == 0;
    (ok && w.ws_col > 0).then_some(w.ws_col as usize)
}

/// Does `text` match the POSIX extended regex `pattern` (the same dialect
/// JACK uses for port patterns)? `None` if the pattern does not compile.
pub fn regex_match(pattern: &str, text: &str) -> Option<bool> {
    let (p, t) = (CString::new(pattern).ok()?, CString::new(text).ok()?);
    let mut re = RegexT([0; 256]);
    // SAFETY: `re` is large and aligned enough for regex_t on glibc and musl;
    // it is freed only after a successful regcomp.
    unsafe {
        if regcomp(&mut re, p.as_ptr(), REG_EXTENDED | REG_NOSUB) != 0 {
            return None;
        }
        let hit = regexec(&re, t.as_ptr(), 0, std::ptr::null_mut(), 0) == 0;
        regfree(&mut re);
        Some(hit)
    }
}

static ORIGINAL: OnceLock<Termios> = OnceLock::new();

fn restore_tty() {
    if let Some(t) = ORIGINAL.get() {
        unsafe { tcsetattr(0, TCSANOW, t) };
    }
    // show cursor, leave alternate screen, reset colours
    print!("\x1b[0m\x1b[?25h\x1b[?1049l");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Raw keyboard input + alternate screen; restored on drop and on panic.
pub struct RawTerminal;

impl RawTerminal {
    pub fn enter() -> Result<RawTerminal, String> {
        let mut t: Termios = unsafe { std::mem::zeroed() };
        if unsafe { tcgetattr(0, &mut t) } != 0 {
            return Err("stdin is not a terminal (use --headless)".into());
        }
        let _ = ORIGINAL.set(t);
        let mut raw = t;
        raw.c_lflag &= !(ICANON | ECHO | ISIG | IEXTEN);
        raw.c_iflag &= !(ICRNL | IXON);
        raw.c_cc[VMIN] = 1;
        raw.c_cc[VTIME] = 0;
        if unsafe { tcsetattr(0, TCSANOW, &raw) } != 0 {
            return Err("cannot set terminal raw mode".into());
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_tty();
            prev(info);
        }));
        print!("\x1b[?1049h\x1b[?25l\x1b[2J");
        Ok(RawTerminal)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        restore_tty();
    }
}

/// Current local time as `YYYY-MM-DD_HH-MM-SS` (for recording file names).
///
/// Uses `localtime_r` on 64-bit targets, where `time_t` is 64-bit on both
/// glibc and musl. On 32-bit targets its width differs between libcs, so
/// UTC is used there instead (marked with a trailing `Z`).
pub fn local_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    #[cfg(target_pointer_width = "64")]
    {
        // Leading fields of glibc/musl `struct tm`; `_rest` covers the
        // remaining ints, tm_gmtoff and tm_zone with room to spare.
        #[repr(C)]
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
        // SAFETY: plain-old-data out-parameter large enough for struct tm.
        let mut tm: Tm = unsafe { std::mem::zeroed() };
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
        assert!(virtual_iface("docker0") && virtual_iface("tailscale0") && !virtual_iface("enp3s0") && !virtual_iface("wlan0"));
    }

    #[test]
    fn cloexec_all_marks_inherited_descriptors() {
        let f = std::fs::File::open("/proc/self/stat").unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: F_DUPFD (0) makes a copy without close-on-exec, closed below.
        let fd = unsafe { fcntl(f.as_raw_fd(), 0, 3) };
        assert!(fd > 2);
        assert_eq!(unsafe { fcntl(fd, 1) } & 1, 0);
        cloexec_all();
        assert_eq!(unsafe { fcntl(fd, 1) } & 1, 1);
        unsafe { close(fd) };
    }

    #[test]
    fn posix_regex() {
        assert_eq!(regex_match("^alsa_input\\.usb-Blue", "alsa_input.usb-Blue_Yeti:capture_FL"), Some(true));
        assert_eq!(regex_match("^Firefox:output_F[LR]$", "Firefox:output_FR"), Some(true));
        assert_eq!(regex_match("^Firefox:output_F[LR]$", "Firefox:output_FRX"), Some(false));
        assert_eq!(regex_match("capture_(1|2)", "system:capture_2"), Some(true));
        assert_eq!(regex_match("([unclosed", "x"), None);
    }

    #[test]
    fn disk_space_of_a_folder_that_does_not_exist_yet() {
        let (total, free) = disk_space(&std::env::temp_dir().join("bs-no-such-dir/a/b")).unwrap();
        assert!(total > 0 && free <= total, "{total} {free}");
    }

    #[test]
    fn timestamps() {
        assert_eq!(utc_timestamp(0), "1970-01-01_00-00-00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29_00-00-00Z");
        assert_eq!(utc_timestamp(1_792_339_880), "2026-10-18_16-11-20Z");
        assert_eq!(utc_timestamp(-1), "1969-12-31_23-59-59Z");
        let t = local_timestamp();
        assert!(t.len() >= 19 && t.as_bytes()[4] == b'-' && t.as_bytes()[10] == b'_', "{t}");
        assert!(t.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '_' || c == 'Z'));
    }
}
