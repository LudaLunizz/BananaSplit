//! The plain parts of the CoreMIDI ports (`mac/midi.rs`): walking a
//! `MIDIPacketList` and naming the ports. They take bytes and strings, so they
//! are tested on every platform (`cargo test --lib`).
//!
//! The packet layout, from `MIDIServices.h` (inside `#pragma pack(push, 4)`,
//! so the 8-byte time stamp does not push anything to an 8-byte boundary):
//!
//! ```text
//! MIDIPacketList  numPackets: u32           +0
//!                 packet[0]                 +4
//! MIDIPacket      timeStamp: u64            +0
//!                 length: u16               +8
//!                 data: [u8; length]        +10
//! ```
//!
//! The next packet starts right after the data (`MIDIPacketNext`), rounded up
//! to a 4-byte address on ARM (`TARGET_CPU_ARM || TARGET_CPU_ARM64`), not
//! rounded on Intel.

/// Bytes before the first packet: `numPackets`.
pub const LIST_HEADER: usize = 4;
/// Bytes of a packet before its data: `timeStamp` and `length`.
pub const PACKET_HEADER: usize = 10;

/// Does `MIDIPacketNext` round up to 4 bytes on this machine? (Apple silicon:
/// yes; Intel: no.)
pub const ALIGNED: bool = cfg!(any(target_arch = "aarch64", target_arch = "arm"));

/// Where the packet after the one at `off` starts, `len` its data bytes;
/// `base` is the list's address (the rounding is of the address, as in
/// `MIDIPacketNext`, so a list that is not 4-aligned is walked right too).
pub fn next_packet(base: usize, off: usize, len: usize, aligned: bool) -> usize {
    let end = off + PACKET_HEADER + len;
    if aligned { ((base.wrapping_add(end) + 3) & !3).wrapping_sub(base) } else { end }
}

/// Every packet's data in a packet list, in order. `at(off, len)` gives the
/// `len` bytes at offset `off` of the list (None: the list is shorter, and the
/// walk stops): a slice in the tests, the list's memory in the CoreMIDI
/// callback. It is only asked for the header and for ranges the headers before
/// it describe. Allocates nothing (it runs in CoreMIDI's read callback).
pub fn walk_packets<'a>(
    at: impl Fn(usize, usize) -> Option<&'a [u8]>,
    base: usize,
    aligned: bool,
    mut each: impl FnMut(&'a [u8]),
) {
    let Some(&[a, b, c, d]) = at(0, 4) else { return };
    let n = u32::from_ne_bytes([a, b, c, d]);
    let mut off = LIST_HEADER;
    for _ in 0..n {
        let Some(&[l0, l1]) = at(off + 8, 2) else { return };
        let len = u16::from_ne_bytes([l0, l1]) as usize;
        let Some(data) = at(off + PACKET_HEADER, len) else { return };
        each(data);
        off = next_packet(base, off, len, aligned);
    }
}

/// The names ports are listed by: their display names (as Audio MIDI Setup
/// shows them), in the order given; a port without a name is "MIDI port", and
/// a second port with a name already taken gets " (2)", a third " (3)" (two
/// identical controllers). The order is CoreMIDI's, so the n-th name is the
/// n-th source or destination.
pub fn port_names(raw: impl IntoIterator<Item = Option<String>>) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for r in raw {
        let base = r.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "MIDI port".into());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet list as CoreMIDI lays it out, starting at address `base`.
    fn list(base: usize, packets: &[&[u8]], aligned: bool) -> Vec<u8> {
        let mut v = (packets.len() as u32).to_ne_bytes().to_vec();
        for (i, p) in packets.iter().enumerate() {
            v.extend_from_slice(&(1000 + i as u64).to_ne_bytes()); // timeStamp
            v.extend_from_slice(&(p.len() as u16).to_ne_bytes());
            v.extend_from_slice(p);
            let off = v.len() - p.len() - PACKET_HEADER;
            v.resize(next_packet(base, off, p.len(), aligned), 0xee); // padding
        }
        v
    }

    fn walk(v: &[u8], base: usize, aligned: bool) -> Vec<Vec<u8>> {
        let mut got = Vec::new();
        walk_packets(|o, l| v.get(o..o + l), base, aligned, |d| got.push(d.to_vec()));
        got
    }

    #[test]
    fn packets_are_walked_on_arm_and_intel() {
        // a CC, then a note on and one more by running status (5 bytes: the
        // next packet is 3 bytes off a 4-byte boundary), then pitch bend
        let packets: [&[u8]; 3] = [&[0xb0, 7, 100], &[0x90, 36, 127, 38, 0], &[0xe0, 0x7f, 0x7f]];
        for aligned in [true, false] {
            for base in [0, 4, 8, 2] {
                let v = list(base, &packets, aligned);
                assert_eq!(walk(&v, base, aligned), packets.map(<[u8]>::to_vec), "aligned {aligned}, base {base}");
            }
        }
        // the two rules really lay out differently: 4 + 13 -> 20 against 17
        assert_eq!(next_packet(0, 4, 3, true), 20);
        assert_eq!(next_packet(0, 4, 3, false), 17);
        assert_eq!(next_packet(0, 20, 6, true), 36, "already on a boundary: not moved");
        assert_eq!(next_packet(2, 4, 3, true), 18, "the address is rounded, not the offset");
        assert_eq!(list(0, &packets, true).len(), 4 + 16 + 16 + 16);
        assert_eq!(list(0, &packets, false).len(), 4 + 13 + 15 + 13);
    }

    #[test]
    fn running_status_bytes_pass_through_whole() {
        // the second packet carries only data bytes (running status of the
        // first): the walk hands them over as they are, and the per-port
        // `Parser` the callback feeds (gui/midi.rs) resolves them
        let v = list(0, &[&[0x90, 36, 127], &[37, 0]], true);
        let got = walk(&v, 0, true);
        assert_eq!(got, [vec![0x90, 36, 127], vec![37, 0]]);
    }

    #[test]
    fn short_or_empty_lists_stop_the_walk() {
        assert!(walk(&[], 0, true).is_empty());
        assert!(walk(&0u32.to_ne_bytes(), 0, true).is_empty());
        let mut v = list(0, &[&[0xb0, 1, 2], &[0xb0, 3, 4]], false);
        v.truncate(v.len() - 1); // the second packet's data is cut short
        assert_eq!(walk(&v, 0, false), [vec![0xb0, 1, 2]]);
        let mut v = list(0, &[&[0xc0, 5]], true);
        v[0] = 9; // says 9 packets, has 1
        assert_eq!(walk(&v, 0, true), [vec![0xc0, 5]]);
    }

    #[test]
    fn ports_are_named_uniquely_in_order() {
        let names = port_names([
            Some("X-TOUCH MINI".to_string()),
            Some("nanoKONTROL2 SLIDER/KNOB".into()),
            Some("X-TOUCH MINI".into()),
            None,
            Some(" ".into()),
            Some("X-TOUCH MINI".into()),
        ]);
        assert_eq!(
            names,
            [
                "X-TOUCH MINI",
                "nanoKONTROL2 SLIDER/KNOB",
                "X-TOUCH MINI (2)",
                "MIDI port",
                "MIDI port (2)",
                "X-TOUCH MINI (3)"
            ]
        );
    }
}
