//! MIDI ports on Windows: WinMM (`winmm.dll`, part of every Windows), so
//! BananaSplit needs no MIDI library or driver of its own.
//!
//! WinMM lists input and output devices by number; the numbers change when a
//! controller is plugged in or out, so a port is chosen by its name and its
//! number is looked up again each time it is opened. Input arrives on a WinMM
//! thread through a callback that only pushes into the shared inbox
//! (`gui::midi`): WinMM forbids allocating, locking or calling other MIDI
//! functions there. Hotplug is the shared code's port listing every two
//! seconds.
//!
//! Windows 10 lets only one program open a MIDI port at a time; Windows 11
//! with Windows MIDI Services (24H2 and later) lets several. A port that is in
//! use says so.
//!
//! The device side (`WinMm`) is Windows only; the plain parts (names, which
//! messages pass, packing) are functions with tests that run anywhere.

/// `szPname` (UTF-16, NUL-terminated unless it fills the array) as text.
fn name_of(raw: &[u16]) -> String {
    let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..n]).trim().to_string()
}

/// The names ports are listed by, in device-number order: a second device with
/// the same name gets " (2)", like on Linux. (Windows itself often numbers
/// twins already: "2- nanoKONTROL2".)
fn unique_names(raw: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for base in raw {
        let mut name = base.clone();
        let mut k = 2;
        while list.contains(&name) {
            name = format!("{base} ({k})");
            k += 1;
        }
        list.push(name);
    }
    list
}

/// A `MIM_DATA` short message (`status | d1 << 8 | d2 << 16`) back to its
/// bytes, and how many of them it has, for `gui::midi::Parser`: the parser
/// then drops what it drops in every other app (system common and real-time
/// messages: clock, active sensing). Anything that is not a channel message is
/// cut to its status byte; data bytes are 7 bits, whatever the driver left in
/// the rest of the word.
fn short_bytes(msg: usize) -> ([u8; 3], usize) {
    let b = [msg as u8, (msg >> 8) as u8 & 0x7f, (msg >> 16) as u8 & 0x7f];
    let n = match b[0] & 0xf0 {
        0xc0 | 0xd0 => 2,
        0x80..=0xe0 => 3,
        _ => 1,
    };
    (b, n)
}

/// One channel message (2 or 3 bytes) packed for `midiOutShortMsg`.
fn outgoing(msg: &[u8]) -> Result<u32, String> {
    match *msg {
        [s @ 0x80..=0xef, d1] => Ok(s as u32 | (d1 as u32 & 0x7f) << 8),
        [s @ 0x80..=0xef, d1, d2] => Ok(s as u32 | (d1 as u32 & 0x7f) << 8 | (d2 as u32 & 0x7f) << 16),
        _ => Err(format!("not a channel message: {msg:02x?}")),
    }
}

/// `MMSYSERR_ALLOCATED`: another program has the port open.
const MMSYSERR_ALLOCATED: u32 = 4;

/// Why `midiInOpen` / `midiOutOpen` failed, in words.
fn open_error(name: &str, code: u32) -> String {
    if code == MMSYSERR_ALLOCATED {
        format!("{name:?} is in use by another program (Windows 10 lets only one program open a MIDI port)")
    } else {
        format!("cannot open MIDI port {name:?} (WinMM error {code})")
    }
}

#[cfg(windows)]
pub use device::io;

#[cfg(windows)]
mod device {
    use super::{name_of, open_error, outgoing, short_bytes, unique_names};
    use crate::gui::midi::{Inbox, Io, Parser};
    use std::ffi::c_void;
    use std::sync::Arc;

    type Handle = *mut c_void;

    /// `MIDIINCAPSW` (only the name is read).
    #[repr(C)]
    struct InCaps {
        _mid: u16,
        _pid: u16,
        _driver_version: u32,
        name: [u16; 32],
        _support: u32,
    }
    const _: () = assert!(size_of::<InCaps>() == 76);

    /// `MIDIOUTCAPSW` (only the name and the technology are read).
    #[repr(C)]
    struct OutCaps {
        _mid: u16,
        _pid: u16,
        _driver_version: u32,
        name: [u16; 32],
        technology: u16,
        _voices: u16,
        _notes: u16,
        _channel_mask: u16,
        _support: u32,
    }
    const _: () = assert!(size_of::<OutCaps>() == 84);

    type InProc = unsafe extern "system" fn(Handle, u32, usize, usize, usize);

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn midiInGetNumDevs() -> u32;
        fn midiInGetDevCapsW(id: usize, caps: *mut InCaps, size: u32) -> u32;
        fn midiInOpen(h: *mut Handle, id: u32, callback: usize, instance: usize, flags: u32) -> u32;
        fn midiInStart(h: Handle) -> u32;
        fn midiInStop(h: Handle) -> u32;
        fn midiInReset(h: Handle) -> u32;
        fn midiInClose(h: Handle) -> u32;
        fn midiOutGetNumDevs() -> u32;
        fn midiOutGetDevCapsW(id: usize, caps: *mut OutCaps, size: u32) -> u32;
        fn midiOutOpen(h: *mut Handle, id: u32, callback: usize, instance: usize, flags: u32) -> u32;
        fn midiOutShortMsg(h: Handle, msg: u32) -> u32;
        fn midiOutClose(h: Handle) -> u32;
    }

    const CALLBACK_NULL: u32 = 0;
    const CALLBACK_FUNCTION: u32 = 0x3_0000;
    const MIM_DATA: u32 = 0x3c3;
    /// `MOD_SWSYNTH`: a software synthesizer (the Microsoft GS Wavetable
    /// Synth), not a controller: feedback sent there would play notes.
    const MOD_SWSYNTH: u16 = 7;

    /// Every input's name, in device-number order.
    fn inputs() -> Vec<String> {
        // SAFETY: no arguments.
        let n = unsafe { midiInGetNumDevs() };
        unique_names((0..n).map(|id| {
            // SAFETY: an all-zero MIDIINCAPSW is a valid value (integers and an array).
            let mut c: InCaps = unsafe { std::mem::zeroed() };
            // SAFETY: a caps struct of the size passed; a device gone since the count
            // returns an error and leaves it zero (an empty name).
            unsafe { midiInGetDevCapsW(id as usize, &mut c, size_of::<InCaps>() as u32) };
            name_of(&c.name)
        }))
    }

    /// Every output's name with its device number; software synthesizers are
    /// left out but keep their place, so twins are numbered the same way each
    /// time.
    fn outputs() -> Vec<(String, u32)> {
        // SAFETY: no arguments.
        let n = unsafe { midiOutGetNumDevs() };
        let caps: Vec<(String, u16)> = (0..n)
            .map(|id| {
                // SAFETY: an all-zero MIDIOUTCAPSW is a valid value.
                let mut c: OutCaps = unsafe { std::mem::zeroed() };
                // SAFETY: as in `inputs`.
                unsafe { midiOutGetDevCapsW(id as usize, &mut c, size_of::<OutCaps>() as u32) };
                (name_of(&c.name), c.technology)
            })
            .collect();
        let names = unique_names(caps.iter().map(|c| c.0.clone()));
        names
            .into_iter()
            .zip(caps)
            .zip(0..)
            .filter(|((_, c), _)| c.1 != MOD_SWSYNTH)
            .map(|((n, _), id)| (n, id))
            .collect()
    }

    /// WinMM's input callback, on its own thread. Only the inbox is touched:
    /// no allocation, no lock, no other WinMM call (the callback rules).
    unsafe extern "system" fn on_input(_h: Handle, msg: u32, inbox: usize, p1: usize, _p2: usize) {
        if msg == MIM_DATA {
            let (bytes, n) = short_bytes(p1);
            // SAFETY: `inbox` is the `Arc<Inbox>` kept in `Input` until midiInClose
            // has returned, after which WinMM calls this no more.
            let inbox = unsafe { &*(inbox as *const Inbox) };
            // A fresh parser each time: a short message is whole (WinMM resolves
            // running status), and the parser lives on the stack.
            Parser::default().feed(&bytes[..n], |m| inbox.push(m));
        }
    }

    /// An open input. The inbox lives here so the callback's pointer stays
    /// valid until the handle is closed.
    struct Input {
        h: Handle,
        _inbox: Arc<Inbox>,
    }

    impl Drop for Input {
        fn drop(&mut self) {
            // SAFETY: the handle opened in `open_input`, closed once; no buffers were
            // added, so midiInClose does not fail on pending ones. Once it returns,
            // the callback no longer runs and the inbox may go.
            unsafe {
                midiInStop(self.h);
                midiInReset(self.h);
                midiInClose(self.h);
            }
        }
    }

    pub struct WinMm {
        input: Option<Input>,
        output: Option<Handle>,
    }

    // SAFETY: WinMM handles are not tied to the thread that opened them; `WinMm`
    // is owned and used by one thread (the MIDI thread), and the callback only
    // reaches the inbox, which is shared safely.
    unsafe impl Send for WinMm {}

    impl Io for WinMm {
        fn inputs(&mut self) -> Vec<String> {
            inputs()
        }

        fn outputs(&mut self) -> Vec<String> {
            outputs().into_iter().map(|(n, _)| n).collect()
        }

        fn open_input(&mut self, name: &str, inbox: Arc<Inbox>) -> Result<(), String> {
            self.close_input();
            let id = inputs().iter().position(|n| n == name).ok_or_else(|| format!("no MIDI port {name:?}"))?;
            let mut h: Handle = std::ptr::null_mut();
            let cb: InProc = on_input;
            // SAFETY: an out pointer, a callback of WinMM's MidiInProc signature, and
            // the inbox's address, kept alive by `Input` until the handle is closed.
            let e =
                unsafe { midiInOpen(&mut h, id as u32, cb as usize, Arc::as_ptr(&inbox) as usize, CALLBACK_FUNCTION) };
            if e != 0 || h.is_null() {
                return Err(open_error(name, e));
            }
            let input = Input { h, _inbox: inbox }; // closes the handle if starting fails
            // SAFETY: the handle just opened.
            let e = unsafe { midiInStart(input.h) };
            if e != 0 {
                return Err(format!("cannot start MIDI port {name:?} (WinMM error {e})"));
            }
            self.input = Some(input);
            Ok(())
        }

        fn close_input(&mut self) {
            self.input = None;
        }

        fn open_output(&mut self, name: &str) -> Result<(), String> {
            self.close_output();
            let id = outputs()
                .into_iter()
                .find_map(|(n, id)| (n == name).then_some(id))
                .ok_or_else(|| format!("no MIDI port {name:?}"))?;
            let mut h: Handle = std::ptr::null_mut();
            // SAFETY: an out pointer; no callback.
            let e = unsafe { midiOutOpen(&mut h, id, 0, 0, CALLBACK_NULL) };
            if e != 0 || h.is_null() {
                return Err(open_error(name, e));
            }
            self.output = Some(h);
            Ok(())
        }

        fn close_output(&mut self) {
            if let Some(h) = self.output.take() {
                // SAFETY: the handle opened in `open_output`, closed once; short
                // messages are sent synchronously, so nothing is pending.
                unsafe { midiOutClose(h) };
            }
        }

        fn send(&mut self, msg: &[u8]) -> Result<(), String> {
            let Some(h) = self.output else { return Ok(()) };
            let m = outgoing(msg)?;
            // SAFETY: an open output handle.
            let e = unsafe { midiOutShortMsg(h, m) };
            if e != 0 { Err(format!("cannot send MIDI (WinMM error {e})")) } else { Ok(()) }
        }
    }

    impl Drop for WinMm {
        fn drop(&mut self) {
            self.close_input();
            self.close_output();
        }
    }

    /// `gui_platform::midi_io()`: WinMM is part of Windows, so this does not fail.
    pub fn io() -> Result<Box<dyn Io>, String> {
        Ok(Box::new(WinMm { input: None, output: None }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The real WinMM: listing works, and a port that is not there is
        /// refused by name (CI machines have no MIDI devices; a desktop may).
        #[test]
        fn winmm_lists_ports_and_refuses_unknown_ones() {
            let mut io = io().unwrap();
            let (ins, outs) = (io.inputs(), io.outputs());
            assert!(!outs.iter().any(|n| n.contains("GS Wavetable")), "{outs:?}");
            let name = "no such MIDI port";
            assert!(!ins.iter().chain(&outs).any(|n| n == name));
            assert!(io.open_input(name, Inbox::new(16)).unwrap_err().contains("no MIDI port"));
            assert!(io.open_output(name).unwrap_err().contains("no MIDI port"));
            assert_eq!(io.send(&[0x90, 36, 127]), Ok(()), "nothing open: nothing sent");
        }

        /// What the callback pushes for each short message: channel messages
        /// as they are, clock and active sensing not at all.
        #[test]
        fn the_callback_passes_channel_messages_only() {
            let inbox = Inbox::new(16);
            let at = Arc::as_ptr(&inbox) as usize;
            for msg in [0x64_07_b0, 0xf8, 0x7f_09_c5, 0xfe, 0x00_24_90, 0x01_f2] {
                // SAFETY: the inbox outlives the call; the handle is not used.
                unsafe { on_input(std::ptr::null_mut(), MIM_DATA, at, msg, 0) };
            }
            // SAFETY: as above; MIM_OPEN, not data, so ignored.
            unsafe { on_input(std::ptr::null_mut(), 0x3c1, at, 0x64_07_b0, 0) };
            let got: Vec<[u8; 3]> = std::iter::from_fn(|| inbox.pop()).map(crate::gui::midi::unpack).collect();
            assert_eq!(got, [[0xb0, 7, 100], [0xc5, 9, 0], [0x90, 36, 0]]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_up_to_the_nul_and_made_unique() {
        let mut raw = [0u16; 32];
        for (i, c) in "X-TOUCH MINI".encode_utf16().enumerate() {
            raw[i] = c;
        }
        assert_eq!(name_of(&raw), "X-TOUCH MINI");
        let full: Vec<u16> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ012345".encode_utf16().collect();
        assert_eq!(name_of(&full), "ABCDEFGHIJKLMNOPQRSTUVWXYZ012345", "a name that fills the array");
        let names = unique_names(["nanoKONTROL2", "Launchpad", "nanoKONTROL2", "nanoKONTROL2"].map(String::from));
        assert_eq!(names, ["nanoKONTROL2", "Launchpad", "nanoKONTROL2 (2)", "nanoKONTROL2 (3)"]);
    }

    #[test]
    fn short_messages_come_in_as_their_bytes() {
        assert_eq!(short_bytes(0x64_07_b0), ([0xb0, 7, 100], 3));
        assert_eq!(short_bytes(0x00_24_90), ([0x90, 36, 0], 3), "velocity-0 Note On stays a Note On");
        assert_eq!(short_bytes(0x7f_09_c5).1, 2, "a program change has one data byte");
        assert_eq!(short_bytes(0x40_d3).1, 2, "channel pressure too");
        assert_eq!(short_bytes(0x5a00_0000 | 0xff_ff_e0), ([0xe0, 0x7f, 0x7f], 3), "7-bit data, 3 bytes");
        for sys in [0xf8, 0xfe, 0xff, 0x01_f2, 0xf0] {
            assert_eq!(short_bytes(sys).1, 1, "{sys:#x}: the status byte only");
        }
    }

    #[test]
    fn channel_messages_go_out_packed() {
        assert_eq!(outgoing(&[0xb0, 7, 100]), Ok(0x64_07_b0));
        assert_eq!(outgoing(&[0xc5, 9]), Ok(0x09_c5));
        assert_eq!(outgoing(&[0x90, 0x80, 0xff]), Ok(0x7f_00_90), "data bytes are 7 bits");
        assert!(outgoing(&[]).is_err() && outgoing(&[0xf8]).is_err() && outgoing(&[0x07, 1, 2]).is_err());
        assert!(outgoing(&[0xf0, 1, 2]).is_err() && outgoing(&[0x90, 1, 2, 3]).is_err());
    }

    #[test]
    fn a_port_in_use_says_so() {
        assert!(open_error("X-TOUCH MINI", 4).contains("in use by another program"));
        assert_eq!(open_error("X", 2), "cannot open MIDI port \"X\" (WinMM error 2)");
    }
}
