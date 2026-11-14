//! MIDI ports on macOS: CoreMIDI, declared by hand like the rest of `mac/`.
//!
//! BananaSplit is the MIDI client "BananaSplit" with one input port (the chosen
//! controller's source is connected to it) and one output port (sent to the
//! chosen destination). Ports are listed by their display names, as Audio
//! MIDI Setup shows them; hotplug is the shared code's port listing every two
//! seconds (`gui::midi`).
//!
//! Input arrives on CoreMIDI's own high-priority thread, in the read callback:
//! it walks the packet list (`bananasplit::pure::midi`) and parses the bytes
//! into the shared inbox, allocating nothing and taking no lock.
//!
//! **The run loop.** CoreMIDI keeps a client's lists of sources and
//! destinations up to date only while that client's notifications are handled,
//! and they are delivered on the run loop of the thread that first called
//! `MIDIClientCreate`. `io()` is called on the mixer's thread ("bs-main"),
//! which runs no run loop, and the app's main thread is the window's (and
//! headless, nothing runs one). So the client is made on the MIDI thread, the
//! first time a port is listed, and that thread runs its own run loop for a
//! moment (`pump`) before every listing: the notifications are handled there,
//! and a controller plugged in shows up in the next listing.

use super::ffi::{CFRelease, CFStringCreateWithCString, CFStringRef, UTF8, string_of};
use crate::gui::midi::{Inbox, Io, Parser};
use bananasplit::pure::midi::{ALIGNED, port_names, walk_packets};
use std::cell::UnsafeCell;
use std::ffi::{CStr, c_void};
use std::ptr::{null, null_mut};
use std::sync::Arc;

type OSStatus = i32;
type MIDIObjectRef = u32;
type MIDIClientRef = MIDIObjectRef;
type MIDIPortRef = MIDIObjectRef;
type MIDIEndpointRef = MIDIObjectRef;
/// `ItemCount` and `ByteCount`: `unsigned long`.
type ItemCount = usize;
type ByteCount = usize;
type MIDITimeStamp = u64;
/// `MIDIPacketList *` and `MIDIPacket *`: walked by hand (`pure::midi`).
type PacketList = c_void;
type Packet = c_void;

/// `MIDINotifyProc`: `const MIDINotification *`, refCon.
type NotifyProc = extern "C" fn(*const c_void, *mut c_void);
/// `MIDIReadProc`: the packet list, the port's refCon, the connection's refCon.
type ReadProc = extern "C" fn(*const PacketList, *mut c_void, *mut c_void);

#[link(name = "CoreMIDI", kind = "framework")]
unsafe extern "C" {
    /// A `CFStringRef` constant.
    static kMIDIPropertyDisplayName: CFStringRef;
    fn MIDIClientCreate(
        name: CFStringRef,
        notify: Option<NotifyProc>,
        notify_ref_con: *mut c_void,
        out_client: *mut MIDIClientRef,
    ) -> OSStatus;
    fn MIDIClientDispose(client: MIDIClientRef) -> OSStatus;
    // Deprecated since macOS 11 in favour of MIDIInputPortCreateWithProtocol
    // (a block, and MIDI 2.0 event lists); it still works and needs no blocks.
    fn MIDIInputPortCreate(
        client: MIDIClientRef,
        name: CFStringRef,
        read: ReadProc,
        ref_con: *mut c_void,
        out_port: *mut MIDIPortRef,
    ) -> OSStatus;
    fn MIDIOutputPortCreate(client: MIDIClientRef, name: CFStringRef, out_port: *mut MIDIPortRef) -> OSStatus;
    fn MIDIGetNumberOfSources() -> ItemCount;
    fn MIDIGetSource(index: ItemCount) -> MIDIEndpointRef;
    fn MIDIGetNumberOfDestinations() -> ItemCount;
    fn MIDIGetDestination(index: ItemCount) -> MIDIEndpointRef;
    fn MIDIObjectGetStringProperty(obj: MIDIObjectRef, property: CFStringRef, out: *mut CFStringRef) -> OSStatus;
    fn MIDIPortConnectSource(port: MIDIPortRef, source: MIDIEndpointRef, conn_ref_con: *mut c_void) -> OSStatus;
    fn MIDIPortDisconnectSource(port: MIDIPortRef, source: MIDIEndpointRef) -> OSStatus;
    fn MIDIPacketListInit(list: *mut PacketList) -> *mut Packet;
    fn MIDIPacketListAdd(
        list: *mut PacketList,
        list_size: ByteCount,
        cur: *mut Packet,
        time: MIDITimeStamp,
        n_data: ByteCount,
        data: *const u8,
    ) -> *mut Packet;
    fn MIDISend(port: MIDIPortRef, dest: MIDIEndpointRef, list: *const PacketList) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// A `CFRunLoopMode` (a `CFStringRef`) constant.
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
}

/// `kCFRunLoopRunHandledSource`: one source was handled, there may be more.
const RUN_HANDLED_SOURCE: i32 = 4;

/// What the read callback of one connection needs. Made for each source
/// connected and never freed while the client lives (see `retired`).
struct Conn {
    inbox: Arc<Inbox>,
    /// Only CoreMIDI's read thread touches it (one connection, one thread).
    parser: UnsafeCell<Parser>,
}

/// The read callback, on CoreMIDI's thread: the packets' bytes, parsed into
/// the inbox. No allocation, no lock.
extern "C" fn read(list: *const PacketList, _port_ref_con: *mut c_void, conn: *mut c_void) {
    if list.is_null() || conn.is_null() {
        return;
    }
    // SAFETY: `conn` is the `Conn` given to MIDIPortConnectSource, alive until
    // the client is disposed (it is retired, not freed, on disconnect); only
    // this thread uses its parser.
    let conn = unsafe { &*(conn as *const Conn) };
    // SAFETY: as above: nothing else touches the parser.
    let parser = unsafe { &mut *conn.parser.get() };
    let base = list as *const u8;
    walk_packets(
        // SAFETY: CoreMIDI's list for the time of this call; the walk asks only
        // for its header and the ranges its packet headers describe.
        |off, len| Some(unsafe { std::slice::from_raw_parts(base.add(off), len) }),
        base as usize,
        ALIGNED,
        |bytes| parser.feed(bytes, |m| conn.inbox.push(m)),
    );
}

/// Client notifications (devices added, removed, renamed): nothing to do with
/// them, but asking for them makes CoreMIDI deliver them to this thread's run
/// loop, which is what keeps the client's port lists current.
extern "C" fn notify(_msg: *const c_void, _ref_con: *mut c_void) {}

/// A CFString of `s`, released when dropped.
struct CfStr(CFStringRef);

impl CfStr {
    fn new(s: &CStr) -> CfStr {
        // SAFETY: a NUL-terminated string; a null result is checked by the callers' OSStatus.
        CfStr(unsafe { CFStringCreateWithCString(null(), s.as_ptr(), UTF8) })
    }
}

impl Drop for CfStr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created in `new`, released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Run this thread's run loop until nothing more is waiting (at most a few
/// sources, without waiting), so CoreMIDI's notifications to the client made
/// on it are handled.
fn pump() {
    for _ in 0..32 {
        // SAFETY: the default mode constant; a zero timeout never blocks.
        if unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 1) } != RUN_HANDLED_SOURCE {
            break;
        }
    }
}

/// The display name of an endpoint, as Audio MIDI Setup shows it.
fn display_name(e: MIDIEndpointRef) -> Option<String> {
    let mut s: CFStringRef = null();
    // SAFETY: an out pointer; the property constant is CoreMIDI's.
    if unsafe { MIDIObjectGetStringProperty(e, kMIDIPropertyDisplayName, &mut s) } != 0 || s.is_null() {
        return None;
    }
    // SAFETY: a CFString CoreMIDI handed over (the caller releases it),
    // released here once read.
    unsafe {
        let text = string_of(s);
        CFRelease(s);
        text
    }
}

/// The sources (`input`) or destinations, with the names they are listed by.
fn endpoints(input: bool) -> Vec<(String, MIDIEndpointRef)> {
    // SAFETY: plain queries; an index that is gone by now gives 0.
    let refs: Vec<MIDIEndpointRef> = unsafe {
        if input {
            (0..MIDIGetNumberOfSources()).map(|i| MIDIGetSource(i)).collect()
        } else {
            (0..MIDIGetNumberOfDestinations()).map(|i| MIDIGetDestination(i)).collect()
        }
    };
    let refs: Vec<MIDIEndpointRef> = refs.into_iter().filter(|&e| e != 0).collect();
    let names = port_names(refs.iter().map(|&e| display_name(e)));
    names.into_iter().zip(refs).collect()
}

fn find(input: bool, name: &str) -> Result<MIDIEndpointRef, String> {
    endpoints(input)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, e)| e)
        .ok_or_else(|| format!("no MIDI port {name:?}"))
}

/// The client and its two ports, made on the MIDI thread.
struct Client {
    client: MIDIClientRef,
    input: MIDIPortRef,
    output: MIDIPortRef,
}

impl Client {
    fn create() -> Result<Client, String> {
        let mut client: MIDIClientRef = 0;
        let name = CfStr::new(c"BananaSplit");
        // SAFETY: a CFString name, a callback that lives forever, an out pointer.
        let st = unsafe { MIDIClientCreate(name.0, Some(notify), null_mut(), &mut client) };
        if st != 0 || client == 0 {
            return Err(format!("cannot create a CoreMIDI client (error {st})"));
        }
        let (mut input, mut output): (MIDIPortRef, MIDIPortRef) = (0, 0);
        let (in_name, out_name) = (CfStr::new(c"BananaSplit in"), CfStr::new(c"BananaSplit out"));
        // SAFETY: the client just made, CFString names, a callback that lives
        // forever (its connections bring their own refCon), out pointers.
        let (a, b) = unsafe {
            (
                MIDIInputPortCreate(client, in_name.0, read, null_mut(), &mut input),
                MIDIOutputPortCreate(client, out_name.0, &mut output),
            )
        };
        if a != 0 || b != 0 {
            // SAFETY: the client made above, disposed once (with its ports).
            unsafe { MIDIClientDispose(client) };
            return Err(format!("cannot create the CoreMIDI ports (error {})", if a != 0 { a } else { b }));
        }
        Ok(Client { client, input, output })
    }
}

pub struct CoreMidi {
    /// Made at the first use, on the MIDI thread (see the module's notes);
    /// the error, when it could not be.
    client: Option<Result<Client, String>>,
    /// The source connected to the input port, and its connection.
    source: Option<(MIDIEndpointRef, *mut Conn)>,
    /// Connections of sources disconnected since: CoreMIDI does not promise
    /// that a read callback already running has returned when
    /// MIDIPortDisconnectSource does, so they are freed only after the client
    /// is disposed. A few dozen bytes for each time the input is opened.
    retired: Vec<*mut Conn>,
    /// Where `send` sends.
    dest: Option<MIDIEndpointRef>,
}

// SAFETY: the raw `Conn` pointers are owned by `CoreMidi` (only freed in its
// Drop) and their contents are only used by CoreMIDI's read thread; the
// CoreMIDI references are plain numbers. `CoreMidi` itself is used by one
// thread at a time (it is moved to the MIDI thread).
unsafe impl Send for CoreMidi {}

impl CoreMidi {
    /// The client, made now if it is not yet; this thread's run loop is run
    /// first, so the port lists are current.
    fn client(&mut self) -> Result<&Client, String> {
        let c = self.client.get_or_insert_with(Client::create).as_ref().map_err(Clone::clone)?;
        pump();
        Ok(c)
    }
}

impl Io for CoreMidi {
    fn inputs(&mut self) -> Vec<String> {
        if self.client().is_err() {
            return Vec::new();
        }
        endpoints(true).into_iter().map(|(n, _)| n).collect()
    }

    fn outputs(&mut self) -> Vec<String> {
        if self.client().is_err() {
            return Vec::new();
        }
        endpoints(false).into_iter().map(|(n, _)| n).collect()
    }

    fn open_input(&mut self, name: &str, inbox: Arc<Inbox>) -> Result<(), String> {
        self.close_input();
        let port = self.client()?.input;
        let src = find(true, name)?;
        let conn = Box::into_raw(Box::new(Conn { inbox, parser: UnsafeCell::new(Parser::default()) }));
        // SAFETY: our input port, a source CoreMIDI listed, and a refCon that
        // stays alive until the client is disposed.
        let st = unsafe { MIDIPortConnectSource(port, src, conn.cast()) };
        if st != 0 {
            self.retired.push(conn); // never connected, but kept like the others
            return Err(format!("cannot connect from {name:?} (error {st})"));
        }
        self.source = Some((src, conn));
        Ok(())
    }

    fn close_input(&mut self) {
        let Some((src, conn)) = self.source.take() else { return };
        if let Some(Ok(c)) = &self.client {
            // SAFETY: our input port; the source may be gone already (an error then).
            unsafe { MIDIPortDisconnectSource(c.input, src) };
        }
        self.retired.push(conn);
    }

    fn open_output(&mut self, name: &str) -> Result<(), String> {
        self.close_output();
        self.client()?;
        self.dest = Some(find(false, name)?);
        Ok(())
    }

    fn close_output(&mut self) {
        // MIDISend names the destination each time: nothing is connected.
        self.dest = None;
    }

    fn send(&mut self, msg: &[u8]) -> Result<(), String> {
        let (Some(dest), Some(Ok(c))) = (self.dest, &self.client) else { return Ok(()) };
        // A list with one packet: 4 + 10 + 3 bytes, with room to spare; aligned
        // for its u32 and u64 fields.
        #[repr(C, align(8))]
        struct Buf([u8; 64]);
        let mut buf = Buf([0; 64]);
        let list: *mut PacketList = buf.0.as_mut_ptr().cast();
        // SAFETY: a buffer of `size_of::<Buf>()` bytes, given as the list's size;
        // MIDIPacketListAdd copies `msg.len()` bytes from `msg` (time 0 = now).
        let added = unsafe {
            let first = MIDIPacketListInit(list);
            MIDIPacketListAdd(list, size_of::<Buf>(), first, 0, msg.len(), msg.as_ptr())
        };
        if added.is_null() {
            return Err(format!("cannot send MIDI message {msg:02x?}"));
        }
        // SAFETY: our output port, a destination CoreMIDI listed (a stale one is
        // an error), a complete list.
        let st = unsafe { MIDISend(c.output, dest, list) };
        if st != 0 { Err(format!("cannot send MIDI (error {st})")) } else { Ok(()) }
    }
}

impl Drop for CoreMidi {
    fn drop(&mut self) {
        self.close_input();
        if let Some(Ok(c)) = self.client.take() {
            // SAFETY: the client made in `create`, disposed once with its ports
            // (CoreMIDI calls none of its callbacks after this).
            unsafe { MIDIClientDispose(c.client) };
        }
        for conn in self.retired.drain(..) {
            // SAFETY: made by Box::into_raw in `open_input`, freed once, after
            // CoreMIDI let go of it (above).
            drop(unsafe { Box::from_raw(conn) });
        }
    }
}

/// `gui_platform::midi_io()`. Makes nothing yet: the client is made on the
/// MIDI thread the first time it lists the ports (see the module's notes).
pub fn io() -> Result<Box<dyn Io>, String> {
    Ok(Box::new(CoreMidi { client: None, source: None, retired: Vec::new(), dest: None }))
}
