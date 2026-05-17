// =============================================================================
// net/dns.rs — résolveur DNS minimal (A records, UDP /53).
//
// Construit une question DNS au format RFC 1035 :
//   Header (12 B) | QNAME (labels longueur-préfixés, terminé par 0) | QTYPE | QCLASS
//
// Envoie au DNS server reçu via DHCP, attend une réponse (busy-poll borné),
// extrait la 1ère réponse A (IPv4) trouvée.
//
// Pas de cache, pas de CNAME chain, pas de retry sur timeout — c'est un
// dépanneur. Suffisant pour `ping example.com` et `wget`.
// =============================================================================

use smoltcp::wire::Ipv4Address;
use crate::net::socket;

const DNS_PORT: u16 = 53;
const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;
const QUERY_TIMEOUT_MS: u64 = 3000;
const RECV_BUF_SIZE: usize = 512;

/// Résout `name` en IPv4. Retourne None si pas de DNS configuré, timeout,
/// ou nom invalide.
pub fn resolve_a(name: &str) -> Option<Ipv4Address> {
    if name.is_empty() || name.len() > 253 { return None; }
    let server = crate::net::dns_server()?;

    // Bind socket UDP éphémère.
    let local_port = 0xC000 | (crate::time::uptime_ms() as u16 & 0x3fff);
    let sock = socket::udp_bind(local_port).ok()?;

    let mut query = [0u8; 512];
    let qlen = build_query(name, 0x1234, &mut query)?;
    if socket::udp_send(sock, server, DNS_PORT, &query[..qlen]).is_err() {
        return None;
    }

    // Busy-poll borné dans le temps.
    let start = crate::time::uptime_ms();
    let mut buf = [0u8; RECV_BUF_SIZE];
    loop {
        crate::net::poll();
        match socket::udp_recv(sock, &mut buf) {
            Ok((n, _src, _port)) => {
                return parse_a_response(&buf[..n], 0x1234);
            }
            Err(_) => {
                if crate::time::uptime_ms() - start > QUERY_TIMEOUT_MS {
                    return None;
                }
                x86_64::instructions::hlt();
            }
        }
    }
}

fn build_query(name: &str, id: u16, out: &mut [u8]) -> Option<usize> {
    // Header.
    out[0] = (id >> 8) as u8;
    out[1] =  id       as u8;
    out[2] = 0x01; out[3] = 0x00; // flags : standard query, RD=1
    out[4] = 0x00; out[5] = 0x01; // QDCOUNT=1
    out[6] = 0x00; out[7] = 0x00; // ANCOUNT
    out[8] = 0x00; out[9] = 0x00; // NSCOUNT
    out[10]= 0x00; out[11]= 0x00; // ARCOUNT

    let mut off = 12usize;
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 { return None; }
        if off + 1 + label.len() > out.len() - 5 { return None; }
        out[off] = label.len() as u8; off += 1;
        for b in label.bytes() {
            out[off] = b; off += 1;
        }
    }
    out[off] = 0; off += 1;
    // QTYPE A
    out[off] = 0; out[off+1] = DNS_TYPE_A as u8; off += 2;
    // QCLASS IN
    out[off] = 0; out[off+1] = DNS_CLASS_IN as u8; off += 2;
    Some(off)
}

fn parse_a_response(buf: &[u8], expected_id: u16) -> Option<Ipv4Address> {
    if buf.len() < 12 { return None; }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    if id != expected_id { return None; }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    // Bit 15 = QR (must be 1 = response), bits 0-3 = RCODE (0 = no error).
    if flags & 0x8000 == 0 { return None; }
    if flags & 0x000f != 0 { return None; }

    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if an == 0 { return None; }

    // Skip past questions.
    let mut off = 12usize;
    for _ in 0..qd {
        off = skip_name(buf, off)?;
        off = off.checked_add(4)?; // QTYPE + QCLASS
        if off > buf.len() { return None; }
    }

    // Walk answers, return first A.
    for _ in 0..an {
        off = skip_name(buf, off)?;
        if off + 10 > buf.len() { return None; }
        let r_type  = u16::from_be_bytes([buf[off],   buf[off+1]]);
        let _class  = u16::from_be_bytes([buf[off+2], buf[off+3]]);
        let _ttl    = u32::from_be_bytes([buf[off+4], buf[off+5], buf[off+6], buf[off+7]]);
        let rdlen   = u16::from_be_bytes([buf[off+8], buf[off+9]]) as usize;
        off += 10;
        if off + rdlen > buf.len() { return None; }

        if r_type == DNS_TYPE_A && rdlen == 4 {
            return Some(Ipv4Address::new(buf[off], buf[off+1], buf[off+2], buf[off+3]));
        }
        off += rdlen;
    }
    None
}

/// Avance dans le buffer DNS à travers un domain name (compressed ou pas).
/// Retourne l'offset juste après. Suit les pointeurs (bit 0xC0) une seule fois.
fn skip_name(buf: &[u8], mut off: usize) -> Option<usize> {
    loop {
        if off >= buf.len() { return None; }
        let len = buf[off];
        if len == 0 { return Some(off + 1); }
        if len & 0xC0 == 0xC0 {
            // Compressed pointer : 2 bytes total, on n'a pas besoin de
            // suivre la cible pour skip.
            return Some(off + 2);
        }
        off += 1 + len as usize;
    }
}
