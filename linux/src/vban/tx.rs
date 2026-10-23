//! Sending one VBAN stream (network thread): take what the engine put in
//! the out-slot ring and send it as packets.
//!
//! Packets carry a whole number of engine periods' worth of audio when they
//! can: with 128-frame periods every period goes out as one 128-sample
//! packet as soon as it is mixed; a 1024-frame period becomes four 256s
//! (or eight 128s when 256 would not fit 1436 bytes). The frame counter
//! goes up by one per packet, as receivers expect.

use super::proto::{self, AudioHeader, SampleFormat};
use super::ring::Ring;
use std::net::{SocketAddrV4, UdpSocket};

/// Samples per packet for an engine period of `period` frames, when at
/// most `max` fit a packet: the period itself if it fits, else its largest
/// divisor that fits (if that is at least half of `max`), else `max`.
pub fn packet_frames(period: usize, max: usize) -> usize {
    let max = max.max(1);
    if period == 0 {
        return max;
    }
    if period <= max {
        return period;
    }
    let d = (1..=max).rev().find(|d| period % d == 0).unwrap_or(1);
    if d * 2 >= max { d } else { max }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TxStats {
    pub packets: u64,
    pub errors: u64,
    pub last_error: Option<String>,
    /// Frames dropped because sending fell behind.
    pub dropped: u64,
}

pub struct TxStream {
    frame: u32,
    pending: Vec<f32>,
    packet: Vec<u8>,
    pub stats: TxStats,
}

impl Default for TxStream {
    fn default() -> Self {
        TxStream { frame: 0, pending: Vec::with_capacity(2 * 8192), packet: vec![0u8; proto::MAX_PACKET], stats: TxStats::default() }
    }
}

/// What a stream sends.
#[derive(Debug, Clone, Copy)]
pub struct TxParams<'a> {
    pub name: &'a [u8; proto::NAME_LEN],
    pub to: SocketAddrV4,
    pub sr_index: u8,
    pub format: SampleFormat,
    pub channels: usize,
    pub period: usize,
    /// Keep at most this many frames waiting (the newest).
    pub max_pending: usize,
}

impl TxStream {
    /// Forget queued audio (the stream was switched off or on).
    pub fn discard(&mut self, ring: &Ring) {
        ring.consume(ring.available());
        self.pending.clear();
    }

    pub fn reset_stats(&mut self) {
        self.stats = TxStats::default();
    }

    /// Move everything the engine produced into packets and send them.
    pub fn pump(&mut self, ring: &Ring, sock: &UdpSocket, p: &TxParams) {
        let avail = ring.available() & !1;
        for i in 0..avail {
            self.pending.push(ring.read(i));
        }
        ring.consume(avail);
        let max_pending = 2 * p.max_pending.max(proto::MAX_SAMPLES);
        if self.pending.len() > max_pending {
            let excess = (self.pending.len() - max_pending) & !1;
            self.pending.drain(..excess);
            self.stats.dropped += (excess / 2) as u64;
        }
        let n = packet_frames(p.period, proto::max_samples(p.channels, p.format));
        let mut done = 0;
        while self.pending.len() - done >= 2 * n {
            let h = AudioHeader { sr_index: p.sr_index, samples: n, channels: p.channels, format: p.format, name: *p.name, frame: self.frame };
            proto::write_header(&mut self.packet, &h);
            let len = proto::encode(&self.pending[done..done + 2 * n], p.channels, p.format, &mut self.packet[proto::HEADER_LEN..]);
            match sock.send_to(&self.packet[..proto::HEADER_LEN + len], p.to) {
                Ok(_) => self.stats.packets += 1,
                Err(e) => {
                    self.stats.errors += 1;
                    self.stats.last_error = Some(e.to_string());
                }
            }
            self.frame = self.frame.wrapping_add(1);
            done += 2 * n;
        }
        self.pending.drain(..done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vban::proto::{Packet, name_field};
    use std::time::Duration;

    #[test]
    fn packet_sizes() {
        assert_eq!(packet_frames(128, 256), 128);
        assert_eq!(packet_frames(256, 256), 256);
        assert_eq!(packet_frames(1024, 256), 256);
        assert_eq!(packet_frames(1024, 239), 128);
        assert_eq!(packet_frames(1024, 179), 128);
        assert_eq!(packet_frames(480, 239), 160);
        assert_eq!(packet_frames(64, 179), 64);
        assert_eq!(packet_frames(1031, 256), 256, "a prime period: full packets");
        assert_eq!(packet_frames(0, 256), 256);
    }

    #[test]
    fn sends_spec_conformant_packets() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let to = match rx.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ring = Ring::new(1 << 14);
        let mut st = TxStream::default();
        let name = name_field("Stream2");
        // 3 periods of 1024 frames: 24-bit stereo -> 128 per packet
        let frames: Vec<f32> = (0..3 * 1024).flat_map(|i| [i as f32 / 4096.0, -(i as f32) / 4096.0]).collect();
        assert!(ring.push(&frames));
        let p = TxParams { name: &name, to, sr_index: 3, format: SampleFormat::I24, channels: 2, period: 1024, max_pending: 48_000 };
        st.pump(&ring, &tx, &p);
        assert_eq!(st.stats.packets, 24);
        let mut buf = [0u8; 2000];
        let mut seen = 0usize;
        for k in 0..24u32 {
            let n = rx.recv(&mut buf).unwrap();
            assert!(n <= proto::MAX_PACKET);
            let Ok(Packet::Audio(h, d)) = proto::parse(&buf[..n]) else { panic!("{:?}", proto::parse(&buf[..n])) };
            assert_eq!((h.sample_rate(), h.samples, h.channels, h.format, h.frame), (48_000, 128, 2, SampleFormat::I24, k));
            assert_eq!(h.name_bytes(), b"Stream2");
            let mut out = Vec::new();
            proto::decode_stereo(d, 2, SampleFormat::I24, &mut out);
            for (j, s) in out.chunks(2).enumerate() {
                let i = (seen + j) as f32;
                assert!((s[0] - i / 4096.0).abs() < 1e-6 && (s[1] + i / 4096.0).abs() < 1e-6);
            }
            seen += 128;
        }
        // mono 16-bit, a partial period waits for the rest
        let p = TxParams { channels: 1, format: SampleFormat::I16, period: 128, ..p };
        assert!(ring.push(&vec![0.25f32; 2 * 100]));
        st.pump(&ring, &tx, &p);
        assert_eq!(st.stats.packets, 24);
        assert!(ring.push(&vec![0.25f32; 2 * 28]));
        st.pump(&ring, &tx, &p);
        assert_eq!(st.stats.packets, 25);
        let n = rx.recv(&mut buf).unwrap();
        let Ok(Packet::Audio(h, d)) = proto::parse(&buf[..n]) else { panic!() };
        assert_eq!((h.samples, h.channels, h.frame, n), (128, 1, 24, 28 + 256));
        assert_eq!(i16::from_le_bytes([d[0], d[1]]), 8192);
    }

    #[test]
    fn falls_behind_gracefully() {
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ring = Ring::new(1 << 16);
        let mut st = TxStream::default();
        let name = name_field("x");
        assert!(ring.push(&vec![0.0f32; 2 * 20_000]));
        let p = TxParams {
            name: &name,
            to: "127.0.0.1:9".parse().unwrap(),
            sr_index: 3,
            format: SampleFormat::I16,
            channels: 2,
            period: 128,
            max_pending: 4800,
        };
        st.pump(&ring, &tx, &p);
        assert_eq!(st.stats.dropped, 20_000 - 4800);
        assert_eq!(st.stats.packets + st.stats.errors, 4800 / 128);
    }
}
