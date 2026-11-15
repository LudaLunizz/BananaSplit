//! MIDI ports on Linux: the ALSA sequencer, loaded at run time with `dlopen`
//! (like libjack), so BananaSplit needs no MIDI library to build or to start.
//! PipeWire and JACK bridge their MIDI into the sequencer too, so every
//! controller shows up here.
//!
//! BananaSplit appears as the sequencer client "BananaSplit" with an input
//! port (connected from the chosen controller) and an output port (connected
//! to it). Input is read on a thread of its own, turned back into MIDI bytes by
//! ALSA's own `snd_midi_event` decoder and parsed into the shared inbox
//! (`gui::midi`); hotplug is the shared code's port listing every two seconds.

use crate::dl::{self, dl_api};
use crate::gui::midi::{Inbox, Io, Parser};
use std::ffi::{CStr, c_char, c_int, c_long, c_short, c_uint, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::JoinHandle;

type Seq = *mut c_void;

/// `snd_seq_event_t`: 28 bytes, 4-aligned. Only the fields set here by name:
/// `queue` (3), `source.port` (13), `dest` (14, 15); `snd_midi_event`
/// fills and reads the rest.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct Event([u8; 28]);
const _: () = assert!(size_of::<Event>() == 28);

/// `snd_seq_addr_t`: a client and one of its ports.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
struct Addr {
    client: u8,
    port: u8,
}

impl Addr {
    fn of(client: c_int, port: c_int) -> Addr {
        Addr { client: client as u8, port: port as u8 }
    }
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int;
}

dl_api! {
    struct Api {
        snd_seq_open: unsafe extern "C" fn(*mut Seq, *const c_char, c_int, c_int) -> c_int,
        snd_seq_close: unsafe extern "C" fn(Seq) -> c_int,
        snd_seq_set_client_name: unsafe extern "C" fn(Seq, *const c_char) -> c_int,
        snd_seq_client_id: unsafe extern "C" fn(Seq) -> c_int,
        snd_seq_create_simple_port: unsafe extern "C" fn(Seq, *const c_char, c_uint, c_uint) -> c_int,
        snd_seq_connect_from: unsafe extern "C" fn(Seq, c_int, c_int, c_int) -> c_int,
        snd_seq_connect_to: unsafe extern "C" fn(Seq, c_int, c_int, c_int) -> c_int,
        snd_seq_disconnect_to: unsafe extern "C" fn(Seq, c_int, c_int, c_int) -> c_int,
        snd_seq_nonblock: unsafe extern "C" fn(Seq, c_int) -> c_int,
        snd_seq_event_input: unsafe extern "C" fn(Seq, *mut *mut Event) -> c_int,
        snd_seq_event_output_direct: unsafe extern "C" fn(Seq, *mut Event) -> c_int,
        snd_seq_poll_descriptors_count: unsafe extern "C" fn(Seq, c_short) -> c_int,
        snd_seq_poll_descriptors: unsafe extern "C" fn(Seq, *mut PollFd, c_uint, c_short) -> c_int,
        snd_seq_client_info_malloc: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        snd_seq_client_info_free: unsafe extern "C" fn(*mut c_void),
        snd_seq_client_info_set_client: unsafe extern "C" fn(*mut c_void, c_int),
        snd_seq_client_info_get_client: unsafe extern "C" fn(*const c_void) -> c_int,
        snd_seq_client_info_get_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
        snd_seq_query_next_client: unsafe extern "C" fn(Seq, *mut c_void) -> c_int,
        snd_seq_port_info_malloc: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        snd_seq_port_info_free: unsafe extern "C" fn(*mut c_void),
        snd_seq_port_info_set_client: unsafe extern "C" fn(*mut c_void, c_int),
        snd_seq_port_info_set_port: unsafe extern "C" fn(*mut c_void, c_int),
        snd_seq_port_info_get_port: unsafe extern "C" fn(*const c_void) -> c_int,
        snd_seq_port_info_get_capability: unsafe extern "C" fn(*const c_void) -> c_uint,
        snd_seq_port_info_get_name: unsafe extern "C" fn(*const c_void) -> *const c_char,
        snd_seq_query_next_port: unsafe extern "C" fn(Seq, *mut c_void) -> c_int,
        snd_midi_event_new: unsafe extern "C" fn(usize, *mut *mut c_void) -> c_int,
        snd_midi_event_free: unsafe extern "C" fn(*mut c_void),
        snd_midi_event_no_status: unsafe extern "C" fn(*mut c_void, c_int),
        snd_midi_event_decode: unsafe extern "C" fn(*mut c_void, *mut u8, c_long, *const Event) -> c_long,
        snd_midi_event_encode: unsafe extern "C" fn(*mut c_void, *const u8, c_long, *mut Event) -> c_long,
        snd_midi_event_reset_encode: unsafe extern "C" fn(*mut c_void),
        snd_seq_port_subscribe_malloc: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        snd_seq_port_subscribe_free: unsafe extern "C" fn(*mut c_void),
        snd_seq_port_subscribe_set_sender: unsafe extern "C" fn(*mut c_void, *const Addr),
        snd_seq_port_subscribe_set_dest: unsafe extern "C" fn(*mut c_void, *const Addr),
        snd_seq_get_port_subscription: unsafe extern "C" fn(Seq, *const c_void) -> c_int,
    }
}

const OPEN_DUPLEX: c_int = 3;
const CAP_READ: c_uint = 1 << 0;
const CAP_WRITE: c_uint = 1 << 1;
const CAP_SUBS_READ: c_uint = 1 << 5;
const CAP_SUBS_WRITE: c_uint = 1 << 6;
const CAP_NO_EXPORT: c_uint = 1 << 7;
const TYPE_MIDI_GENERIC: c_uint = 1 << 1;
const TYPE_APPLICATION: c_uint = 1 << 20;
const POLLIN: c_short = 1;
const QUEUE_DIRECT: u8 = 253;
const ADDRESS_SUBSCRIBERS: u8 = 254;
const ADDRESS_UNKNOWN: u8 = 253;
/// `SND_SEQ_EVENT_NONE`: the encoder wants more bytes.
const EVENT_NONE: u8 = 0;

/// A sequencer client opened by name: `Ok(None)` when the library is there
/// but the sequencer is not (no `/dev/snd/seq`).
struct Client {
    api: &'static Api,
    seq: Seq,
    port: c_int,
}

// SAFETY: a sequencer handle is used by one thread at a time: the input's by
// its reader thread, the output's by the MIDI thread (each `Client` is moved,
// never shared).
unsafe impl Send for Client {}

impl Client {
    fn open(api: &'static Api, port_name: &CStr, caps: c_uint) -> Result<Client, String> {
        let mut seq: Seq = std::ptr::null_mut();
        // SAFETY: an out pointer and a NUL-terminated name.
        let e = unsafe { (api.snd_seq_open)(&mut seq, c"default".as_ptr(), OPEN_DUPLEX, 0) };
        if e < 0 || seq.is_null() {
            return Err(format!("cannot open the ALSA sequencer (error {e}); is /dev/snd/seq there?"));
        }
        // SAFETY: an open handle and NUL-terminated names.
        let port = unsafe {
            (api.snd_seq_set_client_name)(seq, c"BananaSplit".as_ptr());
            (api.snd_seq_create_simple_port)(seq, port_name.as_ptr(), caps, TYPE_MIDI_GENERIC | TYPE_APPLICATION)
        };
        if port < 0 {
            // SAFETY: the handle opened above, closed once.
            unsafe { (api.snd_seq_close)(seq) };
            return Err(format!("cannot create a sequencer port (error {port})"));
        }
        Ok(Client { api, seq, port })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: the handle opened in `open`, closed once.
        unsafe { (self.api.snd_seq_close)(self.seq) };
    }
}

/// One port of another client, with the name it is listed by.
#[derive(Debug, Clone, PartialEq)]
struct PortRef {
    name: String,
    client: c_int,
    port: c_int,
    caps: c_uint,
}

/// "Client: Port", or the port's name when it already starts with the client's
/// (most USB controllers: "X-TOUCH MINI MIDI 1"); a second device with the
/// same name gets " (2)".
fn port_name(client: &str, port: &str, taken: &[PortRef]) -> String {
    let base = if port.starts_with(client) { port.to_string() } else { format!("{client}: {port}") };
    let mut name = base.clone();
    let mut k = 2;
    while taken.iter().any(|p| p.name == name) {
        name = format!("{base} ({k})");
        k += 1;
    }
    name
}

/// Clients whose ports are not controllers: the kernel's own, the
/// sequencer's Midi Through, PipeWire's bridges and BananaSplit itself.
fn hidden(client: &str, id: c_int, us: c_int) -> bool {
    id == 0 || id == us || client == "BananaSplit" || client.starts_with("PipeWire-")
}

pub struct Alsa {
    api: &'static Api,
    /// For listing and sending (the output port).
    out: Client,
    /// The input: its reader thread and its stop flag.
    reader: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    /// The input's connection (controller -> our input port), checked by `alive`.
    in_sub: Option<(Addr, Addr)>,
    /// Where the output port is connected to.
    dest: Option<(c_int, c_int)>,
    encoder: *mut c_void,
}

// SAFETY: `encoder` and `out` are only used by the thread that owns `Alsa`
// (the MIDI thread).
unsafe impl Send for Alsa {}

static API: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(|| {
        let h = dl::open(&[c"libasound.so.2", c"libasound.so"])?;
        // SAFETY: the libasound these signatures come from (alsa/seq.h, seq_midi_event.h).
        unsafe { Api::load(h) }
    })
    .as_ref()
    .map_err(|e| format!("libasound could not be loaded ({e})"))
}

impl Alsa {
    pub fn open() -> Result<Alsa, String> {
        let api = api()?;
        let out = Client::open(api, c"BananaSplit MIDI out", CAP_READ | CAP_SUBS_READ)?;
        let mut encoder = std::ptr::null_mut();
        // SAFETY: an out pointer; 256 bytes is ALSA's own default buffer.
        if unsafe { (api.snd_midi_event_new)(256, &mut encoder) } < 0 || encoder.is_null() {
            return Err("cannot create a MIDI encoder".into());
        }
        Ok(Alsa { api, out, reader: None, in_sub: None, dest: None, encoder })
    }

    /// Every port another client offers with the capabilities `want`.
    fn ports(&self, want: c_uint) -> Vec<PortRef> {
        let a = self.api;
        let mut list: Vec<PortRef> = Vec::new();
        let (mut ci, mut pi) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: out pointers for ALSA's own info objects, freed below.
        if unsafe { (a.snd_seq_client_info_malloc)(&mut ci) } < 0 {
            return list;
        }
        // SAFETY: as above.
        if unsafe { (a.snd_seq_port_info_malloc)(&mut pi) } < 0 {
            // SAFETY: allocated above.
            unsafe { (a.snd_seq_client_info_free)(ci) };
            return list;
        }
        // SAFETY: the handle and the info objects are valid for the whole walk;
        // the names ALSA returns live in the info objects until the next query.
        unsafe {
            let us = (a.snd_seq_client_id)(self.out.seq);
            (a.snd_seq_client_info_set_client)(ci, -1);
            while (a.snd_seq_query_next_client)(self.out.seq, ci) >= 0 {
                let id = (a.snd_seq_client_info_get_client)(ci);
                let cname = CStr::from_ptr((a.snd_seq_client_info_get_name)(ci)).to_string_lossy().into_owned();
                if hidden(&cname, id, us) {
                    continue;
                }
                (a.snd_seq_port_info_set_client)(pi, id);
                (a.snd_seq_port_info_set_port)(pi, -1);
                while (a.snd_seq_query_next_port)(self.out.seq, pi) >= 0 {
                    let caps = (a.snd_seq_port_info_get_capability)(pi);
                    if caps & want != want || caps & CAP_NO_EXPORT != 0 {
                        continue;
                    }
                    let pname = CStr::from_ptr((a.snd_seq_port_info_get_name)(pi)).to_string_lossy().into_owned();
                    let name = port_name(&cname, &pname, &list);
                    list.push(PortRef { name, client: id, port: (a.snd_seq_port_info_get_port)(pi), caps });
                }
            }
            (a.snd_seq_port_info_free)(pi);
            (a.snd_seq_client_info_free)(ci);
        }
        list
    }

    /// Whether the sequencer still has the connection `from -> to`. The
    /// kernel drops a connection when either port goes away, and a controller
    /// plugged in again gets a new port that nothing is connected to.
    fn connected(&self, from: Addr, to: Addr) -> bool {
        let a = self.api;
        let mut sub = std::ptr::null_mut();
        // SAFETY: an out pointer for ALSA's own subscription object, freed below.
        if unsafe { (a.snd_seq_port_subscribe_malloc)(&mut sub) } < 0 || sub.is_null() {
            return true; // cannot tell: leave the port as it is
        }
        // SAFETY: the object made above and two addresses that live for the call;
        // the query reads the kernel's subscription list.
        let e = unsafe {
            (a.snd_seq_port_subscribe_set_sender)(sub, &from);
            (a.snd_seq_port_subscribe_set_dest)(sub, &to);
            let e = (a.snd_seq_get_port_subscription)(self.out.seq, sub);
            (a.snd_seq_port_subscribe_free)(sub);
            e
        };
        e >= 0
    }

    fn find(&self, want: c_uint, name: &str) -> Result<PortRef, String> {
        self.ports(want).into_iter().find(|p| p.name == name).ok_or_else(|| format!("no MIDI port {name:?}"))
    }
}

/// The input's thread: read events, turn them into bytes, parse them into the
/// inbox; check the stop flag every 200 ms.
fn read_loop(c: Client, inbox: Arc<Inbox>, stop: Arc<AtomicBool>) {
    let a = c.api;
    let mut dec = std::ptr::null_mut();
    // SAFETY: an out pointer.
    if unsafe { (a.snd_midi_event_new)(256, &mut dec) } < 0 || dec.is_null() {
        return;
    }
    // SAFETY: the decoder just made: every message with its own status byte.
    unsafe { (a.snd_midi_event_no_status)(dec, 1) };
    let mut fds = [PollFd { fd: -1, events: 0, revents: 0 }, PollFd { fd: -1, events: 0, revents: 0 }];
    // SAFETY: an open handle; at most 2 descriptors are taken.
    let n = unsafe {
        (a.snd_seq_nonblock)(c.seq, 1);
        let n = (a.snd_seq_poll_descriptors_count)(c.seq, POLLIN).clamp(0, 2);
        (a.snd_seq_poll_descriptors)(c.seq, fds.as_mut_ptr(), n as c_uint, POLLIN)
    }
    .max(0) as u64;
    let mut parser = Parser::default();
    let mut bytes = [0u8; 256];
    while !stop.load(Relaxed) {
        // SAFETY: `n` descriptors filled in above.
        unsafe { poll(fds.as_mut_ptr(), n, 200) };
        loop {
            let mut ev: *mut Event = std::ptr::null_mut();
            // SAFETY: a non-blocking read; the event is ALSA's, valid until the next read.
            let r = unsafe { (a.snd_seq_event_input)(c.seq, &mut ev) };
            if r < 0 || ev.is_null() {
                break;
            }
            // SAFETY: an event from snd_seq_event_input and a buffer of its length.
            let len = unsafe { (a.snd_midi_event_decode)(dec, bytes.as_mut_ptr(), bytes.len() as c_long, ev) };
            if len > 0 {
                parser.feed(&bytes[..len as usize], |m| inbox.push(m));
            }
        }
    }
    // SAFETY: made above, freed once.
    unsafe { (a.snd_midi_event_free)(dec) };
}

impl Io for Alsa {
    fn inputs(&mut self) -> Vec<String> {
        self.ports(CAP_READ | CAP_SUBS_READ).into_iter().map(|p| p.name).collect()
    }

    fn outputs(&mut self) -> Vec<String> {
        self.ports(CAP_WRITE | CAP_SUBS_WRITE).into_iter().map(|p| p.name).collect()
    }

    fn open_input(&mut self, name: &str, inbox: Arc<Inbox>) -> Result<(), String> {
        self.close_input();
        let p = self.find(CAP_READ | CAP_SUBS_READ, name)?;
        let c = Client::open(self.api, c"BananaSplit MIDI in", CAP_WRITE | CAP_SUBS_WRITE)?;
        // SAFETY: an open handle and its own port.
        let (e, us) = unsafe {
            ((self.api.snd_seq_connect_from)(c.seq, c.port, p.client, p.port), (self.api.snd_seq_client_id)(c.seq))
        };
        if e < 0 {
            return Err(format!("cannot connect from {name:?} (error {e})"));
        }
        let sub = (Addr::of(p.client, p.port), Addr::of(us, c.port));
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::Builder::new()
            .name("bs-midi-in".into())
            .spawn(move || read_loop(c, inbox, s))
            .map_err(|e| format!("cannot start the MIDI input thread: {e}"))?;
        self.reader = Some((t, stop));
        self.in_sub = Some(sub);
        Ok(())
    }

    fn close_input(&mut self) {
        self.in_sub = None;
        if let Some((t, stop)) = self.reader.take() {
            stop.store(true, Relaxed);
            let _ = t.join(); // the client closes with the thread
        }
    }

    fn open_output(&mut self, name: &str) -> Result<(), String> {
        self.close_output();
        let p = self.find(CAP_WRITE | CAP_SUBS_WRITE, name)?;
        // SAFETY: the open output handle and its own port.
        let e = unsafe { (self.api.snd_seq_connect_to)(self.out.seq, self.out.port, p.client, p.port) };
        if e < 0 {
            return Err(format!("cannot connect to {name:?} (error {e})"));
        }
        self.dest = Some((p.client, p.port));
        Ok(())
    }

    fn close_output(&mut self) {
        if let Some((c, p)) = self.dest.take() {
            // SAFETY: the open output handle; the device may be gone already (an error then).
            unsafe { (self.api.snd_seq_disconnect_to)(self.out.seq, self.out.port, c, p) };
        }
    }

    fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        if self.dest.is_none() {
            return Ok(());
        }
        let a = self.api;
        let mut ev = Event([0; 28]);
        // SAFETY: the encoder made in `open`; it reads `msg.len()` bytes.
        let used = unsafe {
            (a.snd_midi_event_reset_encode)(self.encoder);
            (a.snd_midi_event_encode)(self.encoder, msg.as_ptr(), msg.len() as c_long, &mut ev)
        };
        if used <= 0 || ev.0[0] == EVENT_NONE {
            return Err(format!("cannot encode MIDI message {msg:02x?}"));
        }
        // the macros snd_seq_ev_set_source / _subs / _direct
        ev.0[3] = QUEUE_DIRECT;
        ev.0[13] = self.out.port as u8;
        ev.0[14] = ADDRESS_SUBSCRIBERS;
        ev.0[15] = ADDRESS_UNKNOWN;
        // SAFETY: an open handle and a complete event.
        let e = unsafe { (a.snd_seq_event_output_direct)(self.out.seq, &mut ev) };
        if e < 0 { Err(format!("cannot send MIDI (error {e})")) } else { Ok(()) }
    }

    fn alive(&mut self) -> (bool, bool) {
        // SAFETY: the open output handle.
        let us = unsafe { (self.api.snd_seq_client_id)(self.out.seq) };
        let input = self.in_sub.is_none_or(|(from, to)| self.connected(from, to));
        let output = self.dest.is_none_or(|(c, p)| self.connected(Addr::of(us, self.out.port), Addr::of(c, p)));
        (input, output)
    }
}

impl Drop for Alsa {
    fn drop(&mut self) {
        self.close_input();
        self.close_output();
        // SAFETY: made in `open`, freed once.
        unsafe { (self.api.snd_midi_event_free)(self.encoder) };
    }
}

/// `gui_platform::midi_io()`.
pub fn io() -> Result<Box<dyn Io>, String> {
    Alsa::open().map(|a| Box::new(a) as Box<dyn Io>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_names_are_stable_and_unique() {
        let mut taken = Vec::new();
        let n = port_name("X-TOUCH MINI", "X-TOUCH MINI MIDI 1", &taken);
        assert_eq!(n, "X-TOUCH MINI MIDI 1");
        taken.push(PortRef { name: n, client: 20, port: 0, caps: 0 });
        assert_eq!(port_name("X-TOUCH MINI", "X-TOUCH MINI MIDI 1", &taken), "X-TOUCH MINI MIDI 1 (2)");
        assert_eq!(port_name("nanoKONTROL2", "CTRL", &taken), "nanoKONTROL2: CTRL");
        assert!(hidden("PipeWire-System", 144, 130) && hidden("System", 0, 130) && hidden("Anything", 130, 130));
        assert!(!hidden("Midi Through", 14, 130));
    }

    /// The real sequencer, through the kernel's Midi Through port: what goes
    /// out comes back in, parsed, and the port list sees it. Skipped where
    /// there is no sequencer (CI containers).
    #[test]
    fn a_round_trip_through_the_kernel_sequencer() {
        let Ok(mut io) = Alsa::open() else {
            eprintln!("no ALSA sequencer here: skipped");
            return;
        };
        let name = "Midi Through Port-0";
        let (ins, outs) = (io.inputs(), io.outputs());
        if !ins.iter().any(|n| n == name) || !outs.iter().any(|n| n == name) {
            eprintln!("no Midi Through port here: skipped ({ins:?})");
            return;
        }
        let inbox = Inbox::new(64);
        io.open_input(name, inbox.clone()).unwrap();
        io.open_output(name).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        for m in [[0xb0u8, 7, 100], [0x90, 36, 0], [0xe0, 0x7f, 0x7f]] {
            io.send(&m).unwrap();
        }
        io.send(&[0xc5, 9]).unwrap();
        let t = std::time::Instant::now();
        let mut got = Vec::new();
        while got.len() < 4 && t.elapsed() < std::time::Duration::from_secs(2) {
            while let Some(m) = inbox.pop() {
                got.push(crate::gui::midi::unpack(m));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            got,
            [[0xb0, 7, 100], [0x90, 36, 0], [0xe0, 0x7f, 0x7f], [0xc5, 9, 0]],
            "velocity-0 Note On stays a Note On"
        );
        assert_eq!(io.alive(), (true, true), "both connections are there");
        assert!(!io.connected(Addr::of(0, 0), Addr::of(0, 1)), "a connection nobody made");
        // the connection goes (as when the controller is unplugged) while the
        // port keeps its name: `alive` says so, so the next scan reconnects
        let (c, p) = io.dest.unwrap();
        // SAFETY: the open output handle and the port it is connected to.
        unsafe { (io.api.snd_seq_disconnect_to)(io.out.seq, io.out.port, c, p) };
        assert_eq!(io.alive(), (true, false));
        io.close_input();
        io.close_output();
        assert_eq!(io.alive(), (true, true), "nothing open: nothing lost");
    }
}
