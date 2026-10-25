//! VBAN: audio over the network, compatible with Voicemeeter.
//!
//! Up to 8 incoming and 8 outgoing streams, like Voicemeeter Banana:
//!
//! * An **incoming** stream is identified by its name and its sender's IP
//!   address (the stream name is compared without regard to case). It is
//!   added to the input of one strip (1-6), before that strip's gain, EQ,
//!   mute and routing, so it mixes like anything else on that strip. Any
//!   PCM format, 1-8+ channels (channels 1 and 2 are used; mono goes to
//!   both sides), any VBAN sample rate up to 4x the mixer's: the stream is
//!   resampled to the mixer's rate and its clock drift is corrected (see
//!   `rx.rs`).
//! * An **outgoing** stream sends one of the 11 channels (a strip after its
//!   fader, or a bus) to an IPv4 address and port (a `.255` broadcast
//!   address works too), at the mixer's sample rate, as 16-, 24- or 32-bit
//!   PCM or 32-bit float, stereo or mono.
//!
//! One UDP socket, bound to `port` (6980 by default, Voicemeeter's) on all
//! interfaces, receives every incoming stream and sends every outgoing
//! one, so packets leave from port 6980 like Voicemeeter's do. Nothing
//! listens while VBAN is off (the default).
//!
//! Threads: one receives (decode, resample, fill the engine's rings), one
//! sends (empty the rings, packetize). The audio callback only touches
//! preallocated rings and atomics (`tap.rs`).

pub mod proto;
pub mod resample;
pub mod ring;
pub mod rx;
pub mod tap;
pub mod tx;

pub use proto::{DEFAULT_PORT, SampleFormat};
pub use tap::{NUM_IN, NUM_OUT, VbanRt, VbanTap};

use crate::engine::NUM_STRIPS;
use crate::recorder::{NUM_SOURCES, source_id};
use rx::RxStream;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tap::NONE;
use tx::{TxParams, TxStream};

pub const MAX_BUFFER_MS: u32 = 200;
pub const DEFAULT_BUFFER_MS: u32 = 10;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Who may send an incoming stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromAddr {
    /// Not set yet: the stream cannot run.
    Unset,
    /// Any sender.
    Any,
    Ip(Ipv4Addr),
}

impl FromAddr {
    pub fn parse(v: &str) -> Result<FromAddr, String> {
        let v = v.trim();
        if v.is_empty() {
            Ok(FromAddr::Unset)
        } else if v.eq_ignore_ascii_case("any") {
            Ok(FromAddr::Any)
        } else {
            v.parse().map(FromAddr::Ip).map_err(|_| format!("expected the sender's IPv4 address (like 192.168.1.20) or any, got {v:?}"))
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            FromAddr::Unset => String::new(),
            FromAddr::Any => "any".into(),
            FromAddr::Ip(ip) => ip.to_string(),
        }
    }

    pub fn accepts(&self, ip: Ipv4Addr) -> bool {
        match self {
            FromAddr::Unset => false,
            FromAddr::Any => true,
            FromAddr::Ip(a) => *a == ip,
        }
    }
}

/// An IPv4 destination for an outgoing stream ("" = not set).
pub fn parse_to(v: &str) -> Result<Option<Ipv4Addr>, String> {
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let ip: Ipv4Addr = v.parse().map_err(|_| format!("expected the receiver's IPv4 address (like 192.168.1.20), got {v:?}"))?;
    if ip.is_unspecified() || ip.is_multicast() {
        return Err(format!("{ip} cannot receive a VBAN stream"));
    }
    Ok(Some(ip))
}

pub fn parse_port(v: &str) -> Result<u16, String> {
    v.trim().parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(|| format!("expected a UDP port 1-65535, got {v:?}"))
}

/// `1`..`6` -> strip index.
pub fn parse_strip(v: &str) -> Result<usize, String> {
    v.trim().parse::<usize>().ok().filter(|s| (1..=NUM_STRIPS).contains(s)).map(|s| s - 1).ok_or_else(|| format!("expected a strip 1-{NUM_STRIPS}, got {v:?}"))
}

/// `1`..`6`, `A1`..`B2` -> source index (same numbering as the recorder).
pub fn parse_source(v: &str) -> Result<usize, String> {
    let v = v.trim();
    (0..NUM_SOURCES).find(|&i| source_id(i).eq_ignore_ascii_case(v)).ok_or_else(|| format!("expected a source 1-6, A1, A2, A3, B1 or B2, got {v:?}"))
}

pub fn parse_buffer_ms(v: &str) -> Result<u32, String> {
    v.trim().parse::<u32>().ok().filter(|&b| b <= MAX_BUFFER_MS).ok_or_else(|| format!("expected a buffer of 0-{MAX_BUFFER_MS} ms, got {v:?}"))
}

pub fn parse_channels(v: &str) -> Result<u8, String> {
    match v.trim() {
        "1" | "mono" => Ok(1),
        "2" | "stereo" => Ok(2),
        _ => Err(format!("expected channels = 1 (mono) or 2 (stereo), got {v:?}")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InStream {
    pub on: bool,
    pub name: String,
    pub from: FromAddr,
    /// Strip index 0..=5.
    pub strip: usize,
    /// Audio kept in reserve at the buffer's lowest point, ms.
    pub buffer_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutStream {
    pub on: bool,
    pub name: String,
    pub to: Option<Ipv4Addr>,
    pub port: u16,
    /// Source index 0..=10 (strips 1-6, buses A1-B2).
    pub source: usize,
    pub format: SampleFormat,
    pub channels: u8,
}

impl InStream {
    pub fn default_for(k: usize) -> InStream {
        InStream { on: false, name: format!("Stream{}", k + 1), from: FromAddr::Unset, strip: NUM_STRIPS - 1, buffer_ms: DEFAULT_BUFFER_MS }
    }

    /// Why this stream cannot run, if it cannot.
    pub fn problem(&self) -> Option<&'static str> {
        match self.from {
            FromAddr::Unset => Some("set the sender's IP address (or any)"),
            _ => None,
        }
    }
}

impl OutStream {
    pub fn default_for(k: usize) -> OutStream {
        OutStream {
            on: false,
            name: format!("Stream{}", k + 1),
            to: None,
            port: DEFAULT_PORT,
            source: NUM_STRIPS + 3, // B1
            format: SampleFormat::I16,
            channels: 2,
        }
    }

    pub fn problem(&self) -> Option<&'static str> {
        if self.to.is_none() { Some("set the receiver's IP address") } else { None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VbanConfig {
    pub enabled: bool,
    pub port: u16,
    pub ins: [InStream; NUM_IN],
    pub outs: [OutStream; NUM_OUT],
}

impl Default for VbanConfig {
    fn default() -> Self {
        VbanConfig {
            enabled: false,
            port: DEFAULT_PORT,
            ins: std::array::from_fn(InStream::default_for),
            outs: std::array::from_fn(OutStream::default_for),
        }
    }
}

impl VbanConfig {
    fn in_runs(&self, k: usize) -> bool {
        let s = &self.ins[k];
        self.enabled && s.on && s.problem().is_none()
    }

    fn out_runs(&self, k: usize) -> bool {
        let s = &self.outs[k];
        self.enabled && s.on && s.problem().is_none()
    }

    /// Incoming stream `k` that the same packets would also match (an
    /// earlier one with the same name and an overlapping sender).
    pub fn shadowed_by(&self, k: usize) -> Option<usize> {
        let s = &self.ins[k];
        (0..k).find(|&j| {
            let o = &self.ins[j];
            o.on && o.name.eq_ignore_ascii_case(&s.name)
                && match (o.from, s.from) {
                    (FromAddr::Any, FromAddr::Unset) | (FromAddr::Unset, _) | (_, FromAddr::Unset) => false,
                    (FromAddr::Any, _) | (_, FromAddr::Any) => true,
                    (FromAddr::Ip(a), FromAddr::Ip(b)) => a == b,
                }
        })
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InState {
    #[default]
    Off,
    /// Switched on but missing something (see `error`).
    Blocked,
    /// Waiting for packets.
    Waiting,
    Receiving,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InStatus {
    pub state: InState,
    pub from: Option<SocketAddrV4>,
    pub sample_rate: u32,
    pub channels: usize,
    pub format: Option<SampleFormat>,
    pub latency_ms: Option<f32>,
    pub clock_ppm: f32,
    pub packets: u64,
    pub lost: u64,
    pub late: u64,
    pub underruns: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutState {
    #[default]
    Off,
    Blocked,
    Sending,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutStatus {
    pub state: OutState,
    pub packets: u64,
    pub errors: u64,
    pub dropped: u64,
    pub error: Option<String>,
}

/// A stream that arrives but matches no incoming stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub name: String,
    pub from: SocketAddrV4,
    pub sample_rate: u32,
    pub channels: usize,
    pub format: SampleFormat,
    pub last: Instant,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VbanStatus {
    /// The port incoming streams are received on (None: not listening).
    pub listening: Option<u16>,
    /// Socket problem (e.g. the port is taken).
    pub error: Option<String>,
    pub ins: [InStatus; NUM_IN],
    pub outs: [OutStatus; NUM_OUT],
    pub seen: Vec<Seen>,
    /// Datagrams that were not VBAN audio (other sub-protocols, garbage),
    /// or our own outgoing streams coming back (see `is_local`).
    pub ignored: u64,
}

fn khz(sr: u32) -> String {
    if sr % 1000 == 0 { format!("{} kHz", sr / 1000) } else { format!("{:.1} kHz", sr as f64 / 1000.0) }
}

impl InStatus {
    /// One line for the UIs.
    pub fn describe(&self, cfg: &InStream) -> String {
        match self.state {
            InState::Off => "off".into(),
            InState::Blocked => self.error.clone().unwrap_or_default(),
            InState::Waiting => match &self.error {
                Some(e) => e.clone(),
                None => format!("waiting for \"{}\" from {}", cfg.name, match cfg.from { FromAddr::Ip(ip) => ip.to_string(), _ => "any sender".into() }),
            },
            InState::Receiving => {
                let mut s = format!(
                    "{} · {} ch · {}",
                    khz(self.sample_rate),
                    self.channels,
                    self.format.map_or("", SampleFormat::label)
                );
                if let Some(a) = self.from {
                    s.push_str(&format!(" from {}", a.ip()));
                }
                if let Some(l) = self.latency_ms {
                    s.push_str(&format!(" · {l:.0} ms"));
                }
                if self.underruns > 0 {
                    s.push_str(&format!(" · {} dropouts", self.underruns));
                }
                if let Some(e) = &self.error {
                    s.push_str(&format!(" · {e}"));
                }
                s
            }
        }
    }
}

impl OutStatus {
    pub fn describe(&self, cfg: &OutStream, sample_rate: u32) -> String {
        match self.state {
            OutState::Off => "off".into(),
            OutState::Blocked => self.error.clone().unwrap_or_default(),
            OutState::Sending => {
                let to = cfg.to.map_or(String::new(), |ip| format!("{ip}:{}", cfg.port));
                let mut s = format!(
                    "to {to} · {} · {} · {}",
                    khz(sample_rate),
                    if cfg.channels == 1 { "mono" } else { "stereo" },
                    cfg.format.label()
                );
                if let Some(e) = &self.error {
                    s.push_str(&format!(" · {e}"));
                }
                s
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Inner {
    tap: Arc<VbanTap>,
    sample_rate: u32,
    cfg: Mutex<(u64, VbanConfig)>,
    status: Mutex<VbanStatus>,
    socket: Mutex<Option<Arc<UdpSocket>>>,
    quit: AtomicBool,
    /// Config version each thread's part of `status` describes.
    rx_ver: AtomicU64,
    tx_ver: AtomicU64,
}

impl Inner {
    fn rate(&self) -> u32 {
        match self.tap.rate.load(Relaxed) {
            0 => self.sample_rate,
            r => r,
        }
    }

    fn config_if_newer(&self, seen: u64) -> Option<(u64, VbanConfig)> {
        let g = lock(&self.cfg);
        (g.0 != seen).then(|| (g.0, g.1.clone()))
    }
}

/// VBAN control: thread-safe, used by the UIs.
pub struct Vban {
    inner: Arc<Inner>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Vban {
    /// Start the network threads. `sample_rate` is the mixer's.
    pub fn start(tap: Arc<VbanTap>, sample_rate: u32, cfg: VbanConfig) -> Vban {
        let inner = Arc::new(Inner {
            tap,
            sample_rate,
            cfg: Mutex::new((1, cfg)),
            status: Mutex::new(VbanStatus::default()),
            socket: Mutex::new(None),
            quit: AtomicBool::new(false),
            rx_ver: AtomicU64::new(0),
            tx_ver: AtomicU64::new(0),
        });
        let mut threads = Vec::new();
        let i = inner.clone();
        if let Ok(h) = std::thread::Builder::new().name("bs-vban-rx".into()).spawn(move || rx_thread(&i)) {
            threads.push(h);
        }
        let i = inner.clone();
        if let Ok(h) = std::thread::Builder::new().name("bs-vban-tx".into()).spawn(move || tx_thread(&i)) {
            threads.push(h);
        }
        Vban { inner, threads: Mutex::new(threads) }
    }

    /// The mixer's sample rate: the engine's once it runs, else the one
    /// given to [`Vban::start`].
    pub fn sample_rate(&self) -> u32 {
        self.inner.rate()
    }

    pub fn config(&self) -> VbanConfig {
        lock(&self.inner.cfg).1.clone()
    }

    /// Replace the configuration (applied within a few milliseconds).
    pub fn set_config(&self, cfg: VbanConfig) {
        let mut g = lock(&self.inner.cfg);
        if g.1 != cfg {
            g.0 = g.0.wrapping_add(1);
            g.1 = cfg;
        }
    }

    /// Change the configuration in place.
    pub fn update<R>(&self, f: impl FnOnce(&mut VbanConfig) -> Result<R, String>) -> Result<R, String> {
        let mut g = lock(&self.inner.cfg);
        let mut c = g.1.clone();
        let r = f(&mut c)?;
        if g.1 != c {
            g.0 = g.0.wrapping_add(1);
            g.1 = c;
        }
        Ok(r)
    }

    pub fn status(&self) -> VbanStatus {
        lock(&self.inner.status).clone()
    }

    /// Does `status()` describe the current configuration yet? (Just after
    /// a change, the threads take a few milliseconds to apply it.)
    pub fn settled(&self) -> bool {
        let v = lock(&self.inner.cfg).0;
        self.inner.rx_ver.load(SeqCst) == v && self.inner.tx_ver.load(SeqCst) == v
    }

    /// Stop both threads and close the socket. Idempotent.
    pub fn shutdown(&self) {
        self.inner.quit.store(true, SeqCst);
        for h in lock(&self.threads).drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for Vban {
    fn drop(&mut self) {
        self.shutdown();
    }
}

const STATUS_EVERY: Duration = Duration::from_millis(100);
const REBIND_EVERY: Duration = Duration::from_secs(2);
const SEEN_FOR: Duration = Duration::from_secs(5);

fn bind(port: u16) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("UDP port {port} is in use by another program; incoming VBAN streams are off")
        } else {
            format!("cannot listen on UDP port {port}: {e}")
        }
    })?;
    let _ = s.set_broadcast(true);
    s.set_read_timeout(Some(Duration::from_millis(20))).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Is `ip` an address of this computer? A packet from one of them, sent
/// from the port we listen on, is one of our own outgoing streams coming
/// back (sent to a broadcast address, or to ourselves). Playing it would
/// feed the stream back into itself, so it is dropped.
fn is_local(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|s| {
                s.connect((ip, 9))?;
                s.local_addr()
            })
            .is_ok_and(|a| a.ip() == std::net::IpAddr::V4(ip))
}

fn rx_thread(inner: &Inner) {
    let tap = &*inner.tap;
    let mut fs = inner.rate();
    let mut cfg = VbanConfig::default();
    let mut version = 0u64;
    let mut sock: Option<Arc<UdpSocket>> = None;
    let mut bind_error: Option<String> = None;
    let mut last_bind = Instant::now() - REBIND_EVERY;
    let mut streams: [RxStream; NUM_IN] = std::array::from_fn(|_| RxStream::default());
    let mut errors: [Option<String>; NUM_IN] = Default::default();
    let mut seen: Vec<Seen> = Vec::new();
    let mut ignored = 0u64;
    let mut own_port: Option<u16> = None;
    let mut local: Vec<(Ipv4Addr, bool)> = Vec::new(); // is_local() answers so far
    // any UDP datagram fits (VBAN packets are at most 1464 bytes, but a
    // sender that ignores that limit still gets through)
    let mut buf = vec![0u8; 65_536];
    let mut last_status = Instant::now() - STATUS_EVERY;

    while !inner.quit.load(SeqCst) {
        let now = Instant::now();
        // The audio server changed its sample rate: every stream starts
        // again, resampled to the new rate.
        if inner.rate() != fs {
            fs = inner.rate();
            for k in 0..NUM_IN {
                streams[k].stop(&tap.ins[k]);
                errors[k] = None;
            }
        }
        if let Some((v, c)) = inner.config_if_newer(version) {
            version = v;
            let old = std::mem::replace(&mut cfg, c);
            if old.port != cfg.port || old.enabled != cfg.enabled {
                sock = None;
                own_port = None;
                *lock(&inner.socket) = None;
                bind_error = None;
                last_bind = now - REBIND_EVERY;
            }
            for k in 0..NUM_IN {
                let slot = &tap.ins[k];
                let (was, runs) = (old.in_runs(k), cfg.in_runs(k));
                let (a, b) = (&old.ins[k], &cfg.ins[k]);
                if runs {
                    slot.ring();
                    if !was || !a.name.eq_ignore_ascii_case(&b.name) || a.from != b.from {
                        streams[k].stop(slot);
                        errors[k] = None;
                    }
                    slot.strip.store(b.strip as u8, Relaxed);
                } else {
                    slot.strip.store(NONE, Relaxed);
                    if was {
                        streams[k].stop(slot);
                    }
                    errors[k] = None;
                }
            }
        }
        if cfg.enabled && sock.is_none() && now.duration_since(last_bind) >= REBIND_EVERY {
            last_bind = now;
            match bind(cfg.port) {
                Ok(s) => {
                    let s = Arc::new(s);
                    own_port = s.local_addr().ok().map(|a| a.port());
                    *lock(&inner.socket) = Some(s.clone());
                    sock = Some(s);
                    bind_error = None;
                }
                Err(e) => bind_error = Some(e),
            }
        }

        match &sock {
            Some(s) => match s.recv_from(&mut buf) {
                Ok((n, SocketAddr::V4(from))) => {
                    let now = Instant::now();
                    let own = Some(from.port()) == own_port && {
                        let ip = *from.ip();
                        match local.iter().find(|(a, _)| *a == ip) {
                            Some(&(_, l)) => l,
                            None => {
                                if local.len() >= 64 {
                                    local.clear();
                                }
                                let l = is_local(ip);
                                local.push((ip, l));
                                l
                            }
                        }
                    };
                    match proto::parse(&buf[..n]) {
                        _ if own => ignored += 1,
                        Ok(proto::Packet::Audio(h, data)) => {
                            let k = (0..NUM_IN).find(|&k| {
                                cfg.in_runs(k) && proto::name_matches(h.name_bytes(), &cfg.ins[k].name) && cfg.ins[k].from.accepts(*from.ip())
                            });
                            match k {
                                Some(k) => {
                                    let r = streams[k].packet(tap, k, &h, data, from, now, fs, cfg.ins[k].buffer_ms as f64);
                                    errors[k] = r.err();
                                    if streams[k].from != Some(from) {
                                        // a second sender of this stream (not played)
                                        note_seen(&mut seen, &h, from, now);
                                    }
                                }
                                None => note_seen(&mut seen, &h, from, now),
                            }
                        }
                        _ => ignored += 1,
                    }
                }
                Ok(_) => ignored += 1, // IPv6: not on an IPv4 socket
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(20)), // e.g. ICMP errors reported on the socket
            },
            None => std::thread::sleep(Duration::from_millis(20)),
        }

        let now = Instant::now();
        if now.duration_since(last_status) >= STATUS_EVERY {
            last_status = now;
            seen.retain(|s| now.duration_since(s.last) < SEEN_FOR);
            let mut st = lock(&inner.status);
            st.listening = sock.as_ref().and_then(|s| s.local_addr().ok()).map(|a| a.port());
            st.error = if cfg.enabled { bind_error.clone() } else { None };
            st.seen = seen.clone();
            st.ignored = ignored;
            for k in 0..NUM_IN {
                let s = &streams[k];
                let c = &cfg.ins[k];
                st.ins[k] = if !(cfg.enabled && c.on) {
                    InStatus::default()
                } else if let Some(p) = c.problem() {
                    InStatus { state: InState::Blocked, error: Some(p.into()), ..InStatus::default() }
                } else if !s.receiving(now) {
                    let error = errors[k].clone().or_else(|| cfg.shadowed_by(k).map(|j| format!("in{} receives this stream", j + 1))).or_else(|| {
                        seen.iter()
                            .find(|x| x.name.eq_ignore_ascii_case(&c.name))
                            .map(|x| format!("\"{}\" arrives from {}, not from {}", x.name, x.from.ip(), c.from.as_string()))
                    });
                    InStatus { state: InState::Waiting, error, ..InStatus::default() }
                } else {
                    InStatus {
                        state: InState::Receiving,
                        from: s.from,
                        sample_rate: s.sample_rate,
                        channels: s.channels,
                        format: s.format,
                        latency_ms: s.latency_ms(fs).map(|v| v as f32),
                        clock_ppm: (s.drift() * 1e6) as f32,
                        packets: s.stats.packets,
                        lost: s.stats.lost,
                        late: s.stats.late,
                        underruns: tap.ins[k].underruns.load(Relaxed),
                        error: errors[k].clone(),
                    }
                };
            }
            drop(st);
            inner.rx_ver.store(version, SeqCst);
        }
    }
    for slot in &tap.ins {
        slot.strip.store(NONE, Relaxed);
    }
    *lock(&inner.socket) = None;
}

fn note_seen(seen: &mut Vec<Seen>, h: &proto::AudioHeader, from: SocketAddrV4, now: Instant) {
    let name = h.name_lossy();
    if let Some(s) = seen.iter_mut().find(|s| s.name == name && s.from == from) {
        s.sample_rate = h.sample_rate();
        s.channels = h.channels;
        s.format = h.format;
        s.last = now;
    } else if seen.len() < 16 {
        seen.push(Seen { name, from, sample_rate: h.sample_rate(), channels: h.channels, format: h.format, last: now });
    }
}

#[allow(clippy::needless_range_loop)] // k indexes the config, the slots and the streams
fn tx_thread(inner: &Inner) {
    let tap = &*inner.tap;
    let mut fs = inner.rate();
    let mut sr_index = proto::sr_index(fs);
    let mut cfg = VbanConfig::default();
    let mut version = 0u64;
    let mut streams: [TxStream; NUM_OUT] = std::array::from_fn(|_| TxStream::default());
    let mut names = [[0u8; proto::NAME_LEN]; NUM_OUT];
    let mut fallback: Option<Arc<UdpSocket>> = None;
    let mut fallback_error: Option<String> = None;
    let mut last_status = Instant::now() - STATUS_EVERY;

    while !inner.quit.load(SeqCst) {
        // The audio server changed its sample rate: packets announce the new
        // one (what is queued at the old rate is dropped).
        if inner.rate() != fs {
            fs = inner.rate();
            sr_index = proto::sr_index(fs);
            for k in 0..NUM_OUT {
                let slot = &tap.outs[k];
                if cfg.out_runs(k) && sr_index.is_some() {
                    streams[k].discard(slot.ring());
                    streams[k].reset_stats();
                    slot.dropped.store(0, Relaxed);
                    slot.source.store(cfg.outs[k].source as u8, Relaxed);
                } else {
                    slot.source.store(NONE, Relaxed);
                }
            }
        }
        if let Some((v, c)) = inner.config_if_newer(version) {
            version = v;
            let old = std::mem::replace(&mut cfg, c);
            for k in 0..NUM_OUT {
                let slot = &tap.outs[k];
                let runs = cfg.out_runs(k) && sr_index.is_some();
                names[k] = proto::name_field(&cfg.outs[k].name);
                if runs {
                    let ring = slot.ring();
                    if !(old.out_runs(k) && sr_index.is_some()) {
                        streams[k].discard(ring);
                        streams[k].reset_stats();
                        slot.dropped.store(0, Relaxed);
                    }
                    slot.source.store(cfg.outs[k].source as u8, Relaxed);
                } else {
                    slot.source.store(NONE, Relaxed);
                }
            }
        }
        let period = tap.period.load(Relaxed) as usize;
        let any = (0..NUM_OUT).any(|k| cfg.out_runs(k)) && sr_index.is_some();
        let sock = if any {
            let shared = lock(&inner.socket).clone();
            shared.or_else(|| {
                // not listening (port taken): send from any free port
                if fallback.is_none() {
                    match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
                        Ok(s) => {
                            let _ = s.set_broadcast(true);
                            fallback = Some(Arc::new(s));
                            fallback_error = None;
                        }
                        Err(e) => fallback_error = Some(format!("cannot open a UDP socket: {e}")),
                    }
                }
                fallback.clone()
            })
        } else {
            None
        };
        if let (Some(s), Some(sri)) = (&sock, sr_index) {
            for k in 0..NUM_OUT {
                if !cfg.out_runs(k) {
                    continue;
                }
                let c = &cfg.outs[k];
                let Some(ip) = c.to else { continue };
                let p = TxParams {
                    name: &names[k],
                    to: SocketAddrV4::new(ip, c.port),
                    sr_index: sri,
                    format: c.format,
                    channels: c.channels as usize,
                    period,
                    max_pending: fs as usize / 5,
                };
                streams[k].pump(tap.outs[k].ring(), s, &p);
            }
        }

        let now = Instant::now();
        if now.duration_since(last_status) >= STATUS_EVERY {
            last_status = now;
            let mut st = lock(&inner.status);
            for k in 0..NUM_OUT {
                let c = &cfg.outs[k];
                let s = &streams[k].stats;
                st.outs[k] = if !(cfg.enabled && c.on) {
                    OutStatus::default()
                } else if let Some(p) = c.problem() {
                    OutStatus { state: OutState::Blocked, error: Some(p.into()), ..OutStatus::default() }
                } else if sr_index.is_none() {
                    OutStatus { state: OutState::Blocked, error: Some(format!("VBAN has no {fs} Hz sample rate")), ..OutStatus::default() }
                } else if sock.is_none() {
                    OutStatus { state: OutState::Blocked, error: fallback_error.clone(), ..OutStatus::default() }
                } else {
                    OutStatus {
                        state: OutState::Sending,
                        packets: s.packets,
                        errors: s.errors,
                        dropped: s.dropped + tap.outs[k].dropped.load(Relaxed) as u64,
                        error: s.last_error.clone(),
                    }
                };
            }
            drop(st);
            inner.tx_ver.store(version, SeqCst);
        }
        // Poll about twice per engine period (1-10 ms).
        let wait = if any { (period as u64 * 500_000 / fs.max(1) as u64).clamp(1000, 10_000) } else { 20_000 };
        std::thread::sleep(Duration::from_micros(wait));
    }
    for slot in &tap.outs {
        slot.source.store(NONE, Relaxed);
    }
}

// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values() {
        assert_eq!(FromAddr::parse("").unwrap(), FromAddr::Unset);
        assert_eq!(FromAddr::parse(" ANY ").unwrap(), FromAddr::Any);
        assert_eq!(FromAddr::parse("192.168.1.20").unwrap(), FromAddr::Ip(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(FromAddr::parse("192.168.1").is_err() && FromAddr::parse("pc.local").is_err());
        for f in [FromAddr::Unset, FromAddr::Any, FromAddr::Ip(Ipv4Addr::new(10, 0, 0, 1))] {
            assert_eq!(FromAddr::parse(&f.as_string()).unwrap(), f);
        }
        assert!(FromAddr::Any.accepts(Ipv4Addr::LOCALHOST) && !FromAddr::Unset.accepts(Ipv4Addr::LOCALHOST));
        assert_eq!(parse_to("192.168.1.255").unwrap(), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(parse_to("").unwrap(), None);
        assert!(parse_to("0.0.0.0").is_err() && parse_to("224.0.0.1").is_err() && parse_to("any").is_err());
        assert_eq!(parse_port("6980").unwrap(), 6980);
        assert!(parse_port("0").is_err() && parse_port("70000").is_err());
        assert_eq!(parse_strip("6").unwrap(), 5);
        assert!(parse_strip("7").is_err() && parse_strip("A1").is_err());
        assert_eq!(parse_source("b1").unwrap(), 9);
        assert_eq!(parse_source("3").unwrap(), 2);
        assert!(parse_source("B3").is_err());
        assert_eq!(parse_buffer_ms("200").unwrap(), 200);
        assert!(parse_buffer_ms("201").is_err());
        assert_eq!(parse_channels("mono").unwrap(), 1);
    }

    #[test]
    fn shadowing() {
        let mut c = VbanConfig::default();
        c.ins[0].on = true;
        c.ins[0].from = FromAddr::Any;
        c.ins[3].on = true;
        c.ins[3].name = "STREAM1".into();
        c.ins[3].from = FromAddr::Ip(Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(c.shadowed_by(3), Some(0));
        c.ins[0].name = "Other".into();
        assert_eq!(c.shadowed_by(3), None);
    }

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Two VBAN instances over loopback: B1 of one goes into strip 3 of the
    /// other, through the real threads and sockets.
    #[test]
    fn loopback_between_two_instances() {
        use crate::engine::{MAX_FRAMES, NUM_BUSES};
        // a free port for the receiver
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (ta, tb) = (VbanTap::new(), VbanTap::new());
        let mut ca = VbanConfig { enabled: true, port: 0, ..VbanConfig::default() }; // the sender listens anywhere
        ca.outs[1] = OutStream { on: true, name: "Mix".into(), to: Some(Ipv4Addr::LOCALHOST), port, source: 9, format: SampleFormat::I24, channels: 2 };
        let mut cb = VbanConfig { enabled: true, port, ..VbanConfig::default() };
        cb.ins[4] = InStream { on: true, name: "mix".into(), from: FromAddr::Ip(Ipv4Addr::LOCALHOST), strip: 2, buffer_ms: 5 };
        let (va, vb) = (Vban::start(ta.clone(), 48_000, ca), Vban::start(tb.clone(), 48_000, cb));
        wait_for("both rings", || ta.outs[1].source.load(Relaxed) == 9 && tb.ins[4].strip.load(Relaxed) == 2);
        // engine A produces a constant on B1 (as if B1 carried it), engine B pulls
        let (mut rta, mut rtb) = (VbanRt::default(), VbanRt::default());
        let strips = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
        let mut store = vec![vec![0.0f32; 128]; NUM_BUSES * 2];
        store[6].fill(0.5);
        store[7].fill(-0.25);
        let mut sc = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
        let t0 = Instant::now();
        let mut got = false;
        let mut k = 0u32;
        while t0.elapsed() < Duration::from_secs(5) && !got {
            {
                let mut it = store.iter_mut();
                let buses: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| [it.next().unwrap().as_mut_slice(), it.next().unwrap().as_mut_slice()]);
                rta.push(&ta, 128, &strips, &[true; NUM_STRIPS], &buses);
            }
            sc.fill(0.0);
            rtb.pull(&tb, 128, &mut sc, Instant::now());
            // once playing steadily, strip 3 carries exactly what B1 sent (24-bit)
            let l = &sc[4 * MAX_FRAMES..][..128];
            if k > 50 && l.iter().all(|&v| (v - 0.5).abs() < 1e-6) {
                assert!(sc[5 * MAX_FRAMES..][..128].iter().all(|&v| (v + 0.25).abs() < 1e-6));
                got = true;
            }
            k += 1;
            std::thread::sleep(Duration::from_micros(2667));
        }
        assert!(got, "strip 3 never received B1");
        wait_for("status", || vb.status().ins[4].state == InState::Receiving && va.status().outs[1].state == OutState::Sending);
        let si = &vb.status().ins[4];
        assert_eq!((si.sample_rate, si.channels, si.format), (48_000, 2, Some(SampleFormat::I24)));
        assert!(si.describe(&vb.config().ins[4]).starts_with("48 kHz · 2 ch · 24-bit from 127.0.0.1"));
        assert_eq!(vb.status().listening, Some(port));
        // a stream nobody configured shows up as "seen"
        va.update(|c| {
            c.outs[1].name = "Other".into();
            Ok(())
        })
        .unwrap();
        wait_for("seen", || {
            let mut it = store.iter_mut();
            let buses: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| [it.next().unwrap().as_mut_slice(), it.next().unwrap().as_mut_slice()]);
            rta.push(&ta, 128, &strips, &[true; NUM_STRIPS], &buses);
            vb.status().seen.iter().any(|s| s.name == "Other")
        });
        // switching VBAN off closes the socket and stops the streams
        vb.update(|c| {
            c.enabled = false;
            Ok(())
        })
        .unwrap();
        wait_for("off", || vb.status().listening.is_none() && tb.ins[4].strip.load(Relaxed) == NONE);
        va.shutdown();
        vb.shutdown();
        assert_eq!(ta.outs[1].source.load(Relaxed), NONE);
    }

    /// A stream sent to our own port (to ourselves, or to a broadcast
    /// address) comes back from our own port: it must not be played, or it
    /// would feed back into itself.
    #[test]
    fn our_own_stream_coming_back_is_dropped() {
        use crate::engine::{MAX_FRAMES, NUM_BUSES};
        assert!(is_local(Ipv4Addr::LOCALHOST) && is_local(Ipv4Addr::new(127, 0, 0, 2)));
        let elsewhere = Ipv4Addr::new(203, 0, 113, 77); // TEST-NET-3: nobody's address
        assert!(!is_local(elsewhere));
        // this computer's own network address (if it has a route out) is local
        let s = UdpSocket::bind("0.0.0.0:0").unwrap();
        if s.connect((elsewhere, 9)).is_ok()
            && let Ok(SocketAddr::V4(a)) = s.local_addr()
        {
            assert!(is_local(*a.ip()), "{a}");
        }
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let tap = VbanTap::new();
        let mut c = VbanConfig { enabled: true, port, ..VbanConfig::default() };
        c.outs[0] = OutStream { on: true, name: "Loop".into(), to: Some(Ipv4Addr::LOCALHOST), port, source: 0, ..OutStream::default_for(0) };
        c.ins[0] = InStream { on: true, name: "Loop".into(), from: FromAddr::Any, strip: 0, buffer_ms: 5 };
        let v = Vban::start(tap.clone(), 48_000, c);
        wait_for("rings", || tap.outs[0].source.load(Relaxed) == 0 && tap.ins[0].strip.load(Relaxed) == 0);
        let mut rt = VbanRt::default();
        let mut strips = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
        strips[..256].fill(0.5);
        let mut store = vec![vec![0.0f32; 128]; NUM_BUSES * 2];
        let mut sc = vec![0.0f32; NUM_STRIPS * 2 * MAX_FRAMES];
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(1000) {
            let mut it = store.iter_mut();
            let buses: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| [it.next().unwrap().as_mut_slice(), it.next().unwrap().as_mut_slice()]);
            rt.push(&tap, 128, &strips, &[true; NUM_STRIPS], &buses);
            sc.fill(0.0);
            rt.pull(&tap, 128, &mut sc, Instant::now());
            assert!(sc.iter().all(|&x| x == 0.0), "our own stream was played");
            std::thread::sleep(Duration::from_micros(2667));
        }
        wait_for("status", || v.status().outs[0].packets > 100 && v.status().ignored > 100);
        let st = v.status();
        assert_eq!(st.ins[0].state, InState::Waiting);
        assert_eq!(st.ins[0].packets, 0);
        assert!(st.seen.is_empty(), "{:?}", st.seen);
        v.shutdown();
    }

    #[test]
    fn port_in_use_is_reported_and_sending_still_works() {
        let busy = UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let rport = rx.local_addr().unwrap().port();
        let tap = VbanTap::new();
        let mut c = VbanConfig { enabled: true, port, ..VbanConfig::default() };
        c.outs[0] = OutStream { on: true, to: Some(Ipv4Addr::LOCALHOST), port: rport, source: 0, ..OutStream::default_for(0) };
        let v = Vban::start(tap.clone(), 44_100, c);
        wait_for("error", || v.status().error.as_deref().is_some_and(|e| e.contains("in use")));
        wait_for("source", || tap.outs[0].source.load(Relaxed) == 0);
        let mut rt = VbanRt::default();
        let mut strips = vec![0.0f32; NUM_STRIPS * 2 * crate::engine::MAX_FRAMES];
        strips[..256].fill(0.5);
        let mut store = vec![vec![0.0f32; 256]; crate::engine::NUM_BUSES * 2];
        let mut it = store.iter_mut();
        let buses: [[&mut [f32]; 2]; crate::engine::NUM_BUSES] = std::array::from_fn(|_| [it.next().unwrap().as_mut_slice(), it.next().unwrap().as_mut_slice()]);
        rt.push(&tap, 256, &strips, &[true; NUM_STRIPS], &buses);
        let mut buf = [0u8; 2000];
        let n = rx.recv(&mut buf).unwrap();
        let Ok(proto::Packet::Audio(h, _)) = proto::parse(&buf[..n]) else { panic!() };
        assert_eq!((h.sample_rate(), h.samples, h.name_bytes()), (44_100, 256, &b"Stream1"[..]));
        v.shutdown();
    }
}
