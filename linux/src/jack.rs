//! Minimal JACK client binding, loaded at runtime with `dlopen`.
//!
//! Loading at runtime (instead of linking) means one binary works with
//! JACK2's `libjack.so.0` and with PipeWire's drop-in `libjack.so.0`
//! (`pw-jack bananasplit`, or distro packages that make PipeWire the system
//! libjack). Only the ~20 functions BananaSplit needs are bound; signatures follow
//! `<jack/jack.h>`, `<jack/types.h>` and `<jack/port.h>`.
//!
//! On Debian and Ubuntu, `libjack.so.0` is usually JACK2's (other packages
//! pull it in) while PipeWire keeps its own in a private directory that
//! only `pw-jack` adds to the library path. When the system's libjack finds
//! no server and PipeWire is running, BananaSplit therefore tries
//! PipeWire's libjack from that directory, so it starts from the app menu
//! without `pw-jack`. `BANANASPLIT_PIPEWIRE_LIBJACK` names that file
//! explicitly.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

pub type Client = *mut c_void;
/// A raw `jack_port_t *`, for the function table only.
type RawPort = *mut c_void;

/// One of our registered ports. Only `register_audio_port` makes one, so
/// it is never null; it stays valid while its client is open.
#[derive(Clone, Copy)]
pub struct Port(NonNull<c_void>);

impl Port {
    /// The `jack_port_t *`, for calls made outside this module (the
    /// process callback's `jack_port_get_buffer`).
    pub fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }
}

pub const PORT_IS_INPUT: c_ulong = 0x1;
pub const PORT_IS_OUTPUT: c_ulong = 0x2;
pub const PORT_IS_PHYSICAL: c_ulong = 0x4;
const NO_START_SERVER: c_int = 0x01;
const USE_EXACT_NAME: c_int = 0x02;
const STATUS_NAME_NOT_UNIQUE: c_int = 0x04;
const EEXIST: c_int = 17; // JACK2 returns EEXIST, PipeWire's libjack -EEXIST
pub const LATENCY_CAPTURE: c_int = 0;
pub const LATENCY_PLAYBACK: c_int = 1;
pub const AUDIO_TYPE: &CStr = c"32 bit float mono audio";

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct LatencyRange {
    pub min: u32,
    pub max: u32,
}

pub type ProcessCb = unsafe extern "C" fn(nframes: u32, arg: *mut c_void) -> c_int;
pub type XrunCb = unsafe extern "C" fn(arg: *mut c_void) -> c_int;
pub type ShutdownCb = unsafe extern "C" fn(arg: *mut c_void);
pub type PortRegCb = unsafe extern "C" fn(port: u32, register: c_int, arg: *mut c_void);
pub type SrateCb = unsafe extern "C" fn(nframes: u32, arg: *mut c_void) -> c_int;
pub type ConnectCb = unsafe extern "C" fn(a: u32, b: u32, connect: c_int, arg: *mut c_void);
pub type MessageCb = unsafe extern "C" fn(msg: *const c_char);

unsafe extern "C" fn quiet(_msg: *const c_char) {}

crate::dl::dl_api! {
    pub struct Lib {
    jack_client_open: unsafe extern "C" fn(*const c_char, c_int, *mut c_int, ...) -> Client,
    jack_client_close: unsafe extern "C" fn(Client) -> c_int,
    jack_get_client_name: unsafe extern "C" fn(Client) -> *const c_char,
    jack_activate: unsafe extern "C" fn(Client) -> c_int,
    jack_deactivate: unsafe extern "C" fn(Client) -> c_int,
    jack_set_process_callback: unsafe extern "C" fn(Client, Option<ProcessCb>, *mut c_void) -> c_int,
    jack_set_xrun_callback: unsafe extern "C" fn(Client, Option<XrunCb>, *mut c_void) -> c_int,
    jack_set_port_registration_callback: unsafe extern "C" fn(Client, Option<PortRegCb>, *mut c_void) -> c_int,
    jack_set_sample_rate_callback: unsafe extern "C" fn(Client, Option<SrateCb>, *mut c_void) -> c_int,
    jack_set_port_connect_callback: unsafe extern "C" fn(Client, Option<ConnectCb>, *mut c_void) -> c_int,
    jack_on_shutdown: unsafe extern "C" fn(Client, Option<ShutdownCb>, *mut c_void),
    jack_port_register: unsafe extern "C" fn(Client, *const c_char, *const c_char, c_ulong, c_ulong) -> RawPort,
    jack_port_get_buffer: unsafe extern "C" fn(RawPort, u32) -> *mut c_void,
    jack_port_name: unsafe extern "C" fn(RawPort) -> *const c_char,
    jack_port_get_latency_range: unsafe extern "C" fn(RawPort, c_int, *mut LatencyRange),
    jack_connect: unsafe extern "C" fn(Client, *const c_char, *const c_char) -> c_int,
    jack_disconnect: unsafe extern "C" fn(Client, *const c_char, *const c_char) -> c_int,
    jack_port_get_all_connections: unsafe extern "C" fn(Client, RawPort) -> *mut *const c_char,
    jack_get_ports: unsafe extern "C" fn(Client, *const c_char, *const c_char, c_ulong) -> *mut *const c_char,
    jack_free: unsafe extern "C" fn(*mut c_void),
    jack_get_sample_rate: unsafe extern "C" fn(Client) -> u32,
    jack_get_buffer_size: unsafe extern "C" fn(Client) -> u32,
    jack_set_buffer_size: unsafe extern "C" fn(Client, u32) -> c_int,
    jack_cpu_load: unsafe extern "C" fn(Client) -> f32,
    jack_set_error_function: unsafe extern "C" fn(Option<MessageCb>),
    jack_set_info_function: unsafe extern "C" fn(Option<MessageCb>),
    }
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
static PW_LIB: OnceLock<Option<Lib>> = OnceLock::new();
/// The system's libjack could not reach a server but PipeWire's could:
/// every client from now on uses PipeWire's.
static USE_PW: AtomicBool = AtomicBool::new(false);

/// The libjack the dynamic loader finds (`libjack.so.0`): JACK2's, or
/// PipeWire's when the distribution installs it in its place, or whatever
/// `LD_LIBRARY_PATH` / `pw-jack` points at.
fn system_lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        match crate::dl::open(&[c"libjack.so.0", c"libjack.so"]) {
            // SAFETY: the handle is a libjack, which declares these functions.
            Ok(h) => unsafe { Lib::load(h) }.map_err(|e| format!("libjack has {e}")),
            Err(e) => Err(format!(
                "could not load libjack ({e}).\n  Install PipeWire's JACK support (Fedora: pipewire-jack-audio-connection-kit, \
                 Arch: pipewire-jack, Debian/Ubuntu: pipewire-jack) or JACK2."
            )),
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Where PipeWire keeps its libjack when it is not the system's libjack
/// (Debian and Ubuntu: `pw-jack` adds this directory to the library path).
pub fn pipewire_libjack_candidates() -> Vec<String> {
    if let Some(p) = std::env::var_os("BANANASPLIT_PIPEWIRE_LIBJACK").filter(|p| !p.is_empty()) {
        return vec![p.to_string_lossy().into_owned()];
    }
    let triple = if cfg!(target_arch = "x86_64") {
        "x86_64-linux-gnu"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-linux-gnu"
    } else if cfg!(target_arch = "arm") {
        "arm-linux-gnueabihf"
    } else if cfg!(target_arch = "x86") {
        "i386-linux-gnu"
    } else {
        "riscv64-linux-gnu"
    };
    [format!("/usr/lib/{triple}"), "/usr/lib64".into(), "/usr/lib".into(), "/usr/local/lib".into()]
        .iter()
        .map(|d| format!("{d}/pipewire-0.3/jack/libjack.so.0"))
        .collect()
}

/// PipeWire is running for this user (its socket exists).
fn pipewire_running() -> bool {
    if std::env::var_os("PIPEWIRE_REMOTE").is_some() {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| std::path::Path::new(&d).join("pipewire-0").exists())
}

/// PipeWire's libjack, loaded from its own directory.
fn pipewire_lib() -> Option<&'static Lib> {
    PW_LIB
        .get_or_init(|| {
            for path in pipewire_libjack_candidates() {
                if !std::path::Path::new(&path).exists() {
                    continue;
                }
                let Ok(c) = CString::new(path) else { continue };
                if let Ok(h) = crate::dl::open(&[&c]) {
                    // SAFETY: the handle is PipeWire's libjack, which declares these functions.
                    if let Ok(lib) = unsafe { Lib::load(h) } {
                        return Some(lib);
                    }
                }
            }
            None
        })
        .as_ref()
}

/// Load libjack once per process: the one in use after a fallback to
/// PipeWire's, else the system's.
pub fn lib() -> Result<&'static Lib, String> {
    if USE_PW.load(SeqCst)
        && let Some(l) = pipewire_lib() {
            return Ok(l);
        }
    system_lib()
}

fn status_text(st: c_int) -> String {
    const BITS: [(c_int, &str); 13] = [
        (0x01, "general failure"),
        (0x02, "invalid option"),
        (0x04, "name not unique"),
        (0x08, "server started"),
        (0x10, "unable to connect to the JACK/PipeWire server"),
        (0x20, "server communication error"),
        (0x40, "no such client"),
        (0x80, "unable to load internal client"),
        (0x100, "unable to initialize client"),
        (0x200, "unable to access shared memory"),
        (0x400, "client/server protocol version mismatch"),
        (0x800, "backend error"),
        (0x1000, "client zombified"),
    ];
    let v: Vec<&str> = BITS.iter().filter(|(b, _)| st & b != 0).map(|(_, s)| *s).collect();
    if v.is_empty() { format!("status 0x{st:x}") } else { v.join(", ") }
}

/// An open (not necessarily active) JACK client. Closing deactivates it.
pub struct JackClient {
    pub lib: &'static Lib,
    pub raw: Client,
}

// SAFETY: the JACK client API is thread-safe for the non-RT calls we make from the
// control thread; the raw pointer itself is just a handle.
unsafe impl Send for JackClient {}

impl JackClient {
    pub fn open(name: &str) -> Result<Self, String> {
        Self::open_with(name, true)
    }

    /// A short-lived helper client (`--list-ports`, `--cleanup`): any name
    /// JACK hands out is fine, it registers no ports and is never activated.
    /// libjack's own messages ("Cannot connect to server socket" …) are
    /// silenced: these commands print their own, and `--cleanup` runs from
    /// systemd's ExecStopPost, when the server may already be gone.
    pub fn open_aux(name: &str) -> Result<Self, String> {
        let pw = pipewire_running().then(pipewire_lib).flatten();
        for lib in [lib().ok(), pw].into_iter().flatten() {
            // SAFETY: `quiet` is a valid callback for the whole process lifetime.
            unsafe {
                (lib.jack_set_error_function)(Some(quiet));
                (lib.jack_set_info_function)(Some(quiet));
            }
        }
        Self::open_with(name, false)
    }

    fn open_with(name: &str, exact: bool) -> Result<Self, String> {
        let cname = CString::new(name).map_err(|_| "client name contains NUL".to_string())?;
        // USE_EXACT_NAME: a second instance with the same name fails here
        // instead of silently becoming "BananaSplit-01" and fighting over the cables.
        let opts = if exact { NO_START_SERVER | USE_EXACT_NAME } else { NO_START_SERVER };
        let open = |lib: &Lib| -> (Client, c_int) {
            let mut status: c_int = 0;
            // SAFETY: valid C string + out-param; no variadic args since we
            // pass neither JackServerName nor JackLoadName.
            let raw = unsafe { (lib.jack_client_open)(cname.as_ptr(), opts, &mut status) };
            (raw, status)
        };
        // The PipeWire fallback (see the module docs) is worth trying when
        // the system's libjack is missing or reaches no server.
        let fallback = || (!USE_PW.load(SeqCst) && pipewire_running()).then(pipewire_lib).flatten();
        let (lib, raw, status) = match lib() {
            Err(e) => match fallback() {
                Some(pw) => {
                    let (raw, st) = open(pw);
                    if raw.is_null() {
                        return Err(e);
                    }
                    USE_PW.store(true, SeqCst);
                    (pw, raw, st)
                }
                None => return Err(e),
            },
            Ok(lib) => {
                let pw = fallback().filter(|pw| pw.jack_client_open as usize != lib.jack_client_open as usize);
                if pw.is_some() {
                    // JACK2's "cannot connect to server" chatter would only confuse.
                    // SAFETY: `quiet` is valid for the whole process lifetime.
                    unsafe { (lib.jack_set_error_function)(Some(quiet)) };
                }
                let (raw, st) = open(lib);
                match pw {
                    Some(pw) if raw.is_null() && st & STATUS_NAME_NOT_UNIQUE == 0 => {
                        let (raw2, st2) = open(pw);
                        if raw2.is_null() {
                            (lib, raw2, st2)
                        } else {
                            USE_PW.store(true, SeqCst);
                            (pw, raw2, st2)
                        }
                    }
                    _ => (lib, raw, st),
                }
            }
        };
        if raw.is_null() && status & STATUS_NAME_NOT_UNIQUE != 0 {
            return Err(format!(
                "a JACK client named {name:?} already exists — is BananaSplit already running?\n  \
                 To run a second mixer, give it its own config with a different client_name."
            ));
        }
        if raw.is_null() {
            return Err(format!(
                "cannot open JACK client: {}.\n  Is PipeWire (with pipewire-jack) or jackd running?",
                status_text(status)
            ));
        }
        Ok(Self { lib, raw })
    }

    pub fn name(&self) -> String {
        // SAFETY: valid client; returned string is owned by JACK.
        let p = unsafe { (self.lib.jack_get_client_name)(self.raw) };
        // SAFETY: non-null here; JACK owns the name while the client is open.
        if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    }

    pub fn sample_rate(&self) -> u32 {
        // SAFETY: valid client (non-null from jack_client_open, closed only in Drop).
        unsafe { (self.lib.jack_get_sample_rate)(self.raw) }
    }

    pub fn buffer_size(&self) -> u32 {
        // SAFETY: valid client (non-null from jack_client_open, closed only in Drop).
        unsafe { (self.lib.jack_get_buffer_size)(self.raw) }
    }

    pub fn set_buffer_size(&self, n: u32) -> Result<(), String> {
        // SAFETY: valid client (non-null from jack_client_open, closed only in Drop).
        match unsafe { (self.lib.jack_set_buffer_size)(self.raw, n) } {
            0 => Ok(()),
            e => Err(format!("jack_set_buffer_size({n}) failed ({e})")),
        }
    }

    pub fn cpu_load(&self) -> f32 {
        // SAFETY: valid client (non-null from jack_client_open, closed only in Drop).
        unsafe { (self.lib.jack_cpu_load)(self.raw) }
    }

    pub fn register_audio_port(&self, short: &str, input: bool) -> Result<Port, String> {
        let n = CString::new(short).map_err(|_| "port name contains NUL".to_string())?;
        let flags = if input { PORT_IS_INPUT } else { PORT_IS_OUTPUT };
        // SAFETY: valid client and C strings.
        let p = unsafe { (self.lib.jack_port_register)(self.raw, n.as_ptr(), AUDIO_TYPE.as_ptr(), flags, 0) };
        NonNull::new(p).map(Port).ok_or_else(|| format!("cannot register JACK port {short:?}"))
    }

    pub fn port_name(&self, p: Port) -> String {
        // SAFETY: `p` was registered by a client, valid while that client is open.
        let s = unsafe { (self.lib.jack_port_name)(p.as_ptr()) };
        // SAFETY: non-null, a NUL-terminated name owned by the port.
        if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() }
    }

    pub fn latency(&self, p: Port, mode: c_int) -> LatencyRange {
        let mut r = LatencyRange::default();
        // SAFETY: `p` as above; `r` is a #[repr(C)] jack_latency_range_t out-param.
        unsafe { (self.lib.jack_port_get_latency_range)(p.as_ptr(), mode, &mut r) };
        r
    }

    /// Port names matching an (extended POSIX) regex, audio only, with flags.
    pub fn ports(&self, pattern: Option<&str>, flags: c_ulong) -> Vec<String> {
        let pat = pattern.and_then(|p| CString::new(p).ok());
        let pat_ptr = pat.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: valid client; NULL or valid C strings.
        let arr = unsafe { (self.lib.jack_get_ports)(self.raw, pat_ptr, AUDIO_TYPE.as_ptr(), flags) };
        self.take_names(arr)
    }

    /// The ports `p` (one of ours) is connected to.
    pub fn connections(&self, p: Port) -> Vec<String> {
        // SAFETY: valid client and port.
        let arr = unsafe { (self.lib.jack_port_get_all_connections)(self.raw, p.as_ptr()) };
        self.take_names(arr)
    }

    /// A NULL-terminated name array from libjack, as strings (and freed).
    fn take_names(&self, arr: *mut *const c_char) -> Vec<String> {
        let mut out = Vec::new();
        if arr.is_null() {
            return out;
        }
        let mut i = 0;
        loop {
            // SAFETY: NULL-terminated array returned by libjack.
            let s = unsafe { *arr.add(i) };
            if s.is_null() {
                break;
            }
            // SAFETY: a non-null entry of that array, a NUL-terminated port name.
            out.push(unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned());
            i += 1;
        }
        // SAFETY: `arr` came from libjack; freed once with jack_free after the last read.
        unsafe { (self.lib.jack_free)(arr.cast()) };
        out
    }

    /// Connect two ports by full name. Already-connected counts as success.
    pub fn connect(&self, src: &str, dst: &str) -> Result<(), String> {
        let (a, b) = match (CString::new(src), CString::new(dst)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return Err("port name contains NUL".into()),
        };
        // SAFETY: valid client; `a` and `b` are CStrings held in locals.
        match unsafe { (self.lib.jack_connect)(self.raw, a.as_ptr(), b.as_ptr()) } {
            0 => Ok(()),
            e if e == EEXIST || e == -EEXIST => Ok(()),
            e => Err(format!("connect {src} -> {dst} failed ({e})")),
        }
    }

    /// Disconnect two ports by full name. A port that went away in the
    /// meantime (its connections went with it) counts as success.
    pub fn disconnect(&self, src: &str, dst: &str) -> Result<(), String> {
        let (a, b) = match (CString::new(src), CString::new(dst)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return Err("port name contains NUL".into()),
        };
        // SAFETY: valid client; valid C strings.
        match unsafe { (self.lib.jack_disconnect)(self.raw, a.as_ptr(), b.as_ptr()) } {
            0 => Ok(()),
            e => {
                let gone = |name: &str| self.ports(Some(&format!("^{}$", regex_escape(name))), 0).is_empty();
                if gone(src) || gone(dst) {
                    return Ok(());
                }
                Err(format!("disconnect {src} -> {dst} failed ({e})"))
            }
        }
    }

    pub fn activate(&self) -> Result<(), String> {
        // SAFETY: valid client (non-null from jack_client_open, closed only in Drop).
        match unsafe { (self.lib.jack_activate)(self.raw) } {
            0 => Ok(()),
            e => Err(format!("jack_activate failed ({e})")),
        }
    }
}

impl Drop for JackClient {
    fn drop(&mut self) {
        // SAFETY: valid client; after close returns no callback runs again.
        unsafe {
            (self.lib.jack_deactivate)(self.raw);
            (self.lib.jack_client_close)(self.raw);
        }
    }
}

/// Escape a literal for use inside a POSIX extended regex.
pub fn regex_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape() {
        assert_eq!(regex_escape("a.b(c)"), r"a\.b\(c\)");
        assert_eq!(regex_escape("BananaSplit-In1"), "BananaSplit-In1");
    }

    #[test]
    fn pipewire_libjack_locations() {
        let c = pipewire_libjack_candidates();
        if std::env::var_os("BANANASPLIT_PIPEWIRE_LIBJACK").is_none() {
            assert!(c.iter().all(|p| p.ends_with("/pipewire-0.3/jack/libjack.so.0")));
            assert!(c.contains(&"/usr/lib64/pipewire-0.3/jack/libjack.so.0".to_string()));
            #[cfg(target_arch = "x86_64")]
            assert_eq!(c[0], "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0");
        }
    }

    #[test]
    fn status_bits() {
        assert!(status_text(0x11).contains("unable to connect"));
        assert_eq!(status_text(0), "status 0x0");
    }
}
