//! Whose process is at the other end of a loopback connection: reading each
//! system's own table of connections. Only the reading is here, as plain
//! functions of the table's bytes; fetching the table (`/proc/net/tcp`, the
//! kernel's `pcblist_n`, `GetExtendedTcpTable`) is the app's own `sys` code.
//!
//! Each reader is compiled for the system that uses it, and under `cargo test`
//! on every system: the Linux build tests the Windows and macOS readers too.

/// The system's list of connections could not be read, so whose program is
/// at the other end of a connection is unknown. That answer is "not ours":
/// the page and the API are refused rather than opened to every user of this
/// computer. Said once on stderr, so the reason can be found.
pub fn list_unreadable(what: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!("bananasplit: can't read {what}, so the page and the API refuse every connection");
    }
}

/// Linux: `/proc/net/tcp` and `tcp6`.
#[cfg(any(target_os = "linux", test))]
pub mod proc_net_tcp {
    use std::net::SocketAddr;

    /// The owner (uid) of the socket at the other end of a loopback connection:
    /// in `/proc/net/tcp` (or `tcp6`, for a dual-stack IPv6 socket that came in
    /// as `::ffff:127.0.0.1`), its line has `peer` as local and `us` as remote
    /// address. The kernel prints each 32-bit word of an address in host order.
    pub fn owner(table: &str, peer: SocketAddr, us: SocketAddr) -> Option<u32> {
        let hex = |a: SocketAddr| {
            let words = |b: &[u8]| {
                b.chunks(4).map(|w| format!("{:08X}", u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))).collect::<String>()
            };
            let ip = match a {
                SocketAddr::V4(v) => words(&v.ip().octets()),
                SocketAddr::V6(v) => words(&v.ip().octets()),
            };
            format!("{ip}:{:04X}", a.port())
        };
        let (p, u) = (hex(peer), hex(us));
        table.lines().skip(1).find_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 7 && f[1].eq_ignore_ascii_case(&p) && f[2].eq_ignore_ascii_case(&u))
                .then(|| f[7].parse().ok())
                .flatten()
        })
    }

    /// `a` as an IPv4-mapped IPv6 address (how a dual-stack socket sees it).
    pub fn mapped(a: SocketAddr) -> SocketAddr {
        match a {
            SocketAddr::V4(v) => SocketAddr::from((v.ip().to_ipv6_mapped(), v.port())),
            v6 => v6,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::Ipv4Addr;

        #[test]
        fn finds_the_uid_in_the_tables() {
            let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
               0: 0100007F:2256 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0 100 0 0 10 0\n\
               1: 0100007F:A1B2 0100007F:2256 01 00000000:00000000 00:00000000 00000000  1001        0 2 1 0 20 4 30 10 -1\n\
               2: 0100007F:2256 0100007F:A1B2 01 00000000:00000000 00:00000000 00000000  1000        0 3 1 0 20 4 30 10 -1\n";
            let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
            if cfg!(target_endian = "little") {
                assert_eq!(owner(table, at(0xA1B2), at(0x2256)), Some(1001), "the client's own line");
                assert_eq!(owner(table, at(0xA1B3), at(0x2256)), None);
                // a dual-stack IPv6 client (Java, .NET) is listed in tcp6 as ::ffff:127.0.0.1
                let t6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                   0: 0000000000000000FFFF00000100007F:A1B2 0000000000000000FFFF00000100007F:2256 01 00000000:00000000 00:00000000 00000000  1002        0 4 1 0 20 4 30 10 -1\n";
                assert_eq!(owner(t6, mapped(at(0xA1B2)), mapped(at(0x2256))), Some(1002));
                assert_eq!(owner(t6, at(0xA1B2), at(0x2256)), None, "IPv4 form does not match a tcp6 line");
            }
        }
    }
}

/// Who owns a TCP connection on this Mac: the kernel's list of TCP
/// sockets (`sysctl net.inet.tcp.pcblist_n`, what `netstat` reads), with
/// the user each socket belongs to. macOS has no /proc/net/tcp; this is
/// the same table.
///
/// The list is a `struct xinpgen` header, then one item per socket: a
/// `struct xinpcb_n` (addresses and ports), right after it a
/// `struct xsocket_n` (its user), then buffer, statistics and TCP records,
/// and a closing `xinpgen`. Every record starts with its length and its
/// kind and takes its length rounded up to 8 bytes. The structs are from
/// xnu's bsd/netinet/in_pcb.h and bsd/sys/socketvar.h, under
/// `#pragma pack(4)`: 64-bit fields are only 4-byte aligned.
#[cfg(any(target_os = "macos", test))]
pub mod pcblist {
    use std::net::SocketAddrV4;

    /// `XSO_SOCKET`.
    const XSO_SOCKET: u32 = 0x001;
    /// `XSO_INPCB`.
    const XSO_INPCB: u32 = 0x010;
    /// `INP_IPV4` in `inp_vflag`.
    const INP_IPV4: u8 = 0x1;

    /// The fields of `struct xinpcb_n` read here, at their offsets.
    mod inpcb {
        pub const SIZE: usize = 104;
        pub const FPORT: usize = 16;
        pub const LPORT: usize = 18;
        pub const VFLAG: usize = 44;
        /// `inp_dependfaddr` / `inp_dependladdr`: a `struct in_addr_4in6`
        /// each, its IPv4 address 12 bytes in.
        pub const FADDR4: usize = 48 + 12;
        pub const LADDR4: usize = 64 + 12;
    }

    /// The fields of `struct xsocket_n` read here.
    mod socket {
        pub const SIZE: usize = 104;
        pub const UID: usize = 64;
    }

    fn u16_be(b: &[u8], at: usize) -> u16 {
        u16::from_be_bytes([b[at], b[at + 1]])
    }

    fn u32_ne(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    fn ipv4(b: &[u8], at: usize) -> std::net::Ipv4Addr {
        std::net::Ipv4Addr::new(b[at], b[at + 1], b[at + 2], b[at + 3])
    }

    /// The user of the socket whose own end is `local` and whose other end is
    /// `remote` (IPv4), in a `pcblist_n` list; None if it isn't there.
    pub fn owner(list: &[u8], local: SocketAddrV4, remote: SocketAddrV4) -> Option<u32> {
        // the header: its length first
        if list.len() < 4 {
            return None;
        }
        let mut at = (u32_ne(list, 0) as usize).next_multiple_of(8);
        // the inpcb just read matched: its socket record says whose it is
        let mut matched = false;
        while at + 8 <= list.len() {
            let (len, kind) = (u32_ne(list, at) as usize, u32_ne(list, at + 4));
            if len < 8 || at + len > list.len() {
                break;
            }
            let r = &list[at..at + len];
            // (the closing xinpgen is 24 bytes: too short for either)
            if kind == XSO_INPCB && len >= inpcb::SIZE {
                matched = r[inpcb::VFLAG] & INP_IPV4 != 0
                    && u16_be(r, inpcb::LPORT) == local.port()
                    && u16_be(r, inpcb::FPORT) == remote.port()
                    && ipv4(r, inpcb::LADDR4) == *local.ip()
                    && ipv4(r, inpcb::FADDR4) == *remote.ip();
            } else if kind == XSO_SOCKET && len >= socket::SIZE {
                if matched {
                    return Some(u32_ne(r, socket::UID));
                }
            } else if kind == XSO_INPCB || kind == XSO_SOCKET {
                matched = false;
            }
            at += len.next_multiple_of(8);
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::Ipv4Addr;

        fn header(v: &mut Vec<u8>, count: u32) {
            v.extend_from_slice(&24u32.to_ne_bytes());
            v.extend_from_slice(&count.to_ne_bytes());
            v.extend_from_slice(&[0; 16]);
        }

        /// One socket's records: inpcb, socket, then a 48-byte stand-in for
        /// the buffer, statistics and TCP records.
        fn item(v: &mut Vec<u8>, local: SocketAddrV4, remote: SocketAddrV4, uid: u32, vflag: u8) {
            let mut i = vec![0u8; inpcb::SIZE];
            i[..4].copy_from_slice(&(inpcb::SIZE as u32).to_ne_bytes());
            i[4..8].copy_from_slice(&XSO_INPCB.to_ne_bytes());
            i[inpcb::FPORT..inpcb::FPORT + 2].copy_from_slice(&remote.port().to_be_bytes());
            i[inpcb::LPORT..inpcb::LPORT + 2].copy_from_slice(&local.port().to_be_bytes());
            i[inpcb::VFLAG] = vflag;
            i[inpcb::FADDR4..inpcb::FADDR4 + 4].copy_from_slice(&remote.ip().octets());
            i[inpcb::LADDR4..inpcb::LADDR4 + 4].copy_from_slice(&local.ip().octets());
            v.extend_from_slice(&i);
            let mut s = vec![0u8; socket::SIZE];
            s[..4].copy_from_slice(&(socket::SIZE as u32).to_ne_bytes());
            s[4..8].copy_from_slice(&XSO_SOCKET.to_ne_bytes());
            s[socket::UID..socket::UID + 4].copy_from_slice(&uid.to_ne_bytes());
            v.extend_from_slice(&s);
            let mut rest = vec![0u8; 48];
            rest[..4].copy_from_slice(&44u32.to_ne_bytes()); // rounded up to 48
            rest[4..8].copy_from_slice(&0x002u32.to_ne_bytes());
            v.extend_from_slice(&rest);
        }

        fn at(port: u16) -> SocketAddrV4 {
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
        }

        #[test]
        fn finds_the_client_end_and_its_user() {
            let mut v = Vec::new();
            header(&mut v, 3);
            // the server's end of the same connection, another user's socket, the client's end
            item(&mut v, at(8790), at(50123), 501, INP_IPV4);
            item(&mut v, at(50124), at(8790), 502, INP_IPV4);
            item(&mut v, at(50123), at(8790), 503, INP_IPV4);
            header(&mut v, 16); // the closing xinpgen; 16 = XSO_INPCB as a count
            assert_eq!(owner(&v, at(50123), at(8790)), Some(503));
            assert_eq!(owner(&v, at(50124), at(8790)), Some(502));
            assert_eq!(owner(&v, at(8790), at(50123)), Some(501));
            assert_eq!(owner(&v, at(50125), at(8790)), None);
            // another address, or an IPv6-only socket: not that connection
            assert_eq!(owner(&v, SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 50123), at(8790)), None);
            let mut w = Vec::new();
            header(&mut w, 1);
            item(&mut w, at(50123), at(8790), 503, 0x2);
            assert_eq!(owner(&w, at(50123), at(8790)), None);
        }

        #[test]
        fn a_cut_or_broken_list_is_no_answer() {
            let mut v = Vec::new();
            header(&mut v, 1);
            item(&mut v, at(50123), at(8790), 503, INP_IPV4);
            for n in [0, 3, 24, 24 + inpcb::SIZE, 24 + inpcb::SIZE + 60] {
                assert_eq!(owner(&v[..n], at(50123), at(8790)), None, "cut at {n}");
            }
            // a zero length ends the walk instead of looping
            let mut z = Vec::new();
            header(&mut z, 1);
            z.extend_from_slice(&[0; 16]);
            assert_eq!(owner(&z, at(50123), at(8790)), None);
        }
    }
}

/// Windows: `GetExtendedTcpTable`.
#[cfg(any(windows, test))]
pub mod win_table {
    use std::net::SocketAddr;

    /// The process that owns the connection from `peer` to `us`, found in a
    /// table from `tcp_table`: its row has `peer` as the local and `us` as the
    /// remote end. Addresses are stored as their bytes; ports in network order
    /// in the low two bytes of a DWORD.
    pub fn owner_pid(table: &[u8], v6: bool, peer: SocketAddr, us: SocketAddr) -> Option<u32> {
        let u32_at = |i: usize| table.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let port_at = |i: usize| table.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let v6_of = |a: SocketAddr| match a {
            SocketAddr::V4(v) => v.ip().to_ipv6_mapped().octets(),
            SocketAddr::V6(v) => v.ip().octets(),
        };
        let n = u32_at(0)? as usize;
        let row = if v6 { 56 } else { 24 };
        (0..n).find_map(|k| {
            let r = 4 + k * row;
            let (local, lport, remote, rport, pid) = if v6 {
                (
                    table.get(r..r + 16)?.to_vec(),
                    port_at(r + 20)?,
                    table.get(r + 24..r + 40)?.to_vec(),
                    port_at(r + 44)?,
                    u32_at(r + 52)?,
                )
            } else {
                (
                    table.get(r + 4..r + 8)?.to_vec(),
                    port_at(r + 8)?,
                    table.get(r + 12..r + 16)?.to_vec(),
                    port_at(r + 16)?,
                    u32_at(r + 20)?,
                )
            };
            let addr = |a: SocketAddr| -> Vec<u8> {
                match (v6, a) {
                    (false, SocketAddr::V4(v)) => v.ip().octets().to_vec(),
                    (false, SocketAddr::V6(v)) => {
                        v.ip().to_ipv4_mapped().map(|i| i.octets().to_vec()).unwrap_or_default()
                    }
                    (true, a) => v6_of(a).to_vec(),
                }
            };
            (local == addr(peer) && lport == peer.port() && remote == addr(us) && rport == us.port()).then_some(pid)
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::Ipv4Addr;

        /// One row: local address and port, remote address and port, owner pid.
        type V4Row = ([u8; 4], u16, [u8; 4], u16, u32);

        /// A table as `GetExtendedTcpTable` writes it: the count, then rows.
        fn v4_table(rows: &[V4Row]) -> Vec<u8> {
            let mut t = (rows.len() as u32).to_le_bytes().to_vec();
            for (la, lp, ra, rp, pid) in rows {
                t.extend_from_slice(&5u32.to_le_bytes()); // ESTABLISHED
                t.extend_from_slice(la);
                t.extend_from_slice(&[(lp >> 8) as u8, *lp as u8, 0, 0]);
                t.extend_from_slice(ra);
                t.extend_from_slice(&[(rp >> 8) as u8, *rp as u8, 0, 0]);
                t.extend_from_slice(&pid.to_le_bytes());
            }
            t
        }

        #[test]
        fn connection_owner_from_the_table() {
            let lo = [127, 0, 0, 1];
            let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
            // the server's own end (pid 10) and the client's (pid 42)
            let t = v4_table(&[(lo, 8790, lo, 51000, 10), (lo, 51000, lo, 8790, 42), (lo, 51001, lo, 8790, 43)]);
            assert_eq!(owner_pid(&t, false, at(51000), at(8790)), Some(42));
            assert_eq!(owner_pid(&t, false, at(51001), at(8790)), Some(43));
            assert_eq!(owner_pid(&t, false, at(51002), at(8790)), None);
            assert_eq!(owner_pid(&t[..10], false, at(51000), at(8790)), None, "a cut-off table");
            // a dual-stack client (::ffff:127.0.0.1) in the IPv6 table
            let mapped = Ipv4Addr::LOCALHOST.to_ipv6_mapped().octets();
            let mut t6 = 1u32.to_le_bytes().to_vec();
            t6.extend_from_slice(&mapped);
            t6.extend_from_slice(&0u32.to_le_bytes());
            t6.extend_from_slice(&[0xC7, 0x38, 0, 0]); // 51000
            t6.extend_from_slice(&mapped);
            t6.extend_from_slice(&0u32.to_le_bytes());
            t6.extend_from_slice(&[0x22, 0x56, 0, 0]); // 8790
            t6.extend_from_slice(&5u32.to_le_bytes());
            t6.extend_from_slice(&77u32.to_le_bytes());
            assert_eq!(owner_pid(&t6, true, at(51000), at(8790)), Some(77));
        }
    }
}
