//! Who owns a TCP connection on this Mac: the kernel's list of TCP
//! sockets (`sysctl net.inet.tcp.pcblist_n`, what `netstat` reads), with
//! the user each socket belongs to. macOS has no /proc/net/tcp; this is
//! the same table.
//!
//! The list is a `struct xinpgen` header, then one item per socket: a
//! `struct xinpcb_n` (addresses and ports), right after it a
//! `struct xsocket_n` (its user), then buffer, statistics and TCP records,
//! and a closing `xinpgen`. Every record starts with its length and its
//! kind and takes its length rounded up to 8 bytes. The structs are from
//! xnu's bsd/netinet/in_pcb.h and bsd/sys/socketvar.h, under
//! `#pragma pack(4)`: 64-bit fields are only 4-byte aligned.

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
