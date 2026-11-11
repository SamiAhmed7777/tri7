//! Legacy P2P wire protocol — exact mirror of triangles_v5 (v6.2.6.7) framing
//! and message payloads (net.h CNode::BeginMessage/EndMessage, protocol.h
//! CMessageHeader, main.cpp ProcessMessage).
//!
//! Message header (24 bytes, protocol.h:33-66):
//!   magic[4] | command[12] (null-padded, ASCII 0x20..=0x7E, all-zero after
//!   first NUL) | length u32 LE | checksum u32 LE.
//! Checksum = first 4 bytes of Hash(payload) = double-SHA256 (util.h:502-510),
//! memcpy'd into a u32 (net.h:505-509, main.cpp ProcessMessages reads the same
//! 4 digest bytes LE).
//!
//! pchMessageStart (main.cpp:4849): { 0x70, 0x35, 0x22, 0x05 } — the same 4
//! bytes as the blk0001.dat frame magic (u32 LE 0x05223570).
//!
//! Default P2P port (protocol.h:19 GetDefaultPort): mainnet 24112, testnet
//! 24111. The live DNS2 daemon overrides with -port (triangles.conf port=24113).
//!
//! Protocol versions (version.h): PROTOCOL_VERSION 70206, MIN_PROTO_VERSION
//! 70205, INIT_PROTO_VERSION 209, CADDR_TIME_VERSION 70200. The version
//! message is serialized on a stream at INIT_PROTO_VERSION, so its CAddress
//! fields carry NO nTime (protocol.h:104-106).

use crate::legacy::{sha256d, Block, Hash, Reader};
use sha3::{Digest, Sha3_256};

// ---------------------------------------------------------------------------
// Constants (protocol.h / net.h / version.h / serialize.h)
// ---------------------------------------------------------------------------
/// main.cpp:4849 — `unsigned char pchMessageStart[4] = { 0x70, 0x35, 0x22, 0x05 }`.
pub const MAGIC: [u8; 4] = [0x70, 0x35, 0x22, 0x05];
/// protocol.h CMessageHeader::HEADER_SIZE = 4 + 12 + 4 + 4.
pub const HEADER_SIZE: usize = 24;
/// serialize.h:30 MAX_SIZE — the header length cap enforced by IsValid().
pub const MAX_PAYLOAD: u32 = 0x0200_0000;
/// version.h:33 — the live v6.2.6.7 protocol version.
pub const PROTOCOL_VERSION: i32 = 70206;
/// version.h:37 — peers below this are disconnected (obsolete version).
pub const MIN_PROTO_VERSION: i32 = 70205;
/// version.h:41 — the version-message stream version (no CAddress nTime).
pub const INIT_PROTO_VERSION: i32 = 209;
/// protocol.h GetDefaultPort mainnet (testnet 24111).
pub const DEFAULT_PORT: u16 = 24112;
/// protocol.h:70 NODE_NETWORK service bit.
pub const NODE_NETWORK: u64 = 1;
/// net.h:78-79 inventory types.
pub const MSG_TX: u32 = 1;
pub const MSG_BLOCK: u32 = 2;

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------
/// First 4 digest bytes of double-SHA256, read little-endian (net.h:505-509
/// memcpy of uint256's first bytes into an unsigned int on x86).
pub fn checksum(payload: &[u8]) -> u32 {
    let h = sha256d(payload);
    u32::from_le_bytes([h[0], h[1], h[2], h[3]])
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------
fn push_compact_size(w: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        w.push(n as u8);
    } else if n <= 0xffff {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        w.push(0xfe);
        w.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        w.push(0xff);
        w.extend_from_slice(&n.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Frame encode / decode
// ---------------------------------------------------------------------------
/// Build one wire message: header + payload (net.h BeginMessage/EndMessage).
pub fn encode_message(command: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
    let cb = command.as_bytes();
    if cb.is_empty() || cb.len() > 12 {
        return Err(format!("command {:?} must be 1..=12 bytes", command));
    }
    for &b in cb {
        if !(0x20..=0x7e).contains(&b) {
            return Err(format!(
                "command {:?} has non-printable byte {:#x}",
                command, b
            ));
        }
    }
    if payload.len() as u64 > MAX_PAYLOAD as u64 {
        return Err(format!("payload too large: {}", payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    out.extend_from_slice(&MAGIC); // 0..4
    out.extend_from_slice(cb); // 4..4+cmd
    out.resize(4 + 12, 0); // pad command field to offset 16
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // 16..20
    out.extend_from_slice(&checksum(payload).to_le_bytes()); // 20..24
    out.extend_from_slice(payload);
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct RawMessage {
    pub command: String,
    pub payload: Vec<u8>,
}

/// Incremental byte-stream decoder. Strict: bad magic, malformed command,
/// oversized length or checksum mismatch is an Err (caller disconnects —
/// matching ProcessMessages' "INVALID MESSAGESTART" behavior).
pub struct MessageDecoder {
    buf: Vec<u8>,
}

impl MessageDecoder {
    pub fn new() -> Self {
        MessageDecoder { buf: Vec::new() }
    }

    /// Buffer cap: one max-size message plus slack. Beyond that the peer is
    /// flooding (the legacy client caps vRecv at SendBufferSize()-ish levels).
    const BUF_CAP: usize = (0x0200_0000 as usize) + HEADER_SIZE + (1 << 20);

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<RawMessage>, String> {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > Self::BUF_CAP {
            return Err(format!("decode buffer overflow: {} bytes", self.buf.len()));
        }
        let mut out = Vec::new();
        loop {
            if self.buf.len() < HEADER_SIZE {
                break;
            }
            if self.buf[0..4] != MAGIC {
                return Err(format!(
                    "bad magic {:02x?} (want {:02x?})",
                    &self.buf[0..4],
                    MAGIC
                ));
            }
            // CMessageHeader::IsValid() command rule (protocol.cpp:46-64):
            // ASCII 0x20..=0x7E, and all zeros after the first zero byte.
            // A command filling ALL 12 bytes (no NUL) is also valid — e.g.
            // old clients' 'filterclear'-era 11-char names fit exactly 12
            // when extended. Command becomes the full printable field.
            let cmd_bytes = &self.buf[4..16];
            let mut command: Option<String> = None;
            let mut saw_nul = false;
            for (i, &b) in cmd_bytes.iter().enumerate() {
                if b == 0 {
                    if cmd_bytes[i..].iter().any(|&x| x != 0) {
                        return Err("command field has non-zero bytes after NUL".into());
                    }
                    command = Some(String::from_utf8_lossy(&cmd_bytes[..i]).into_owned());
                    saw_nul = true;
                    break;
                }
                if !(0x20..=0x7e).contains(&b) {
                    return Err(format!("command byte {:#x} not printable", b));
                }
            }
            let command = match command {
                Some(c) if !c.is_empty() => c,
                None if saw_nul == false => {
                    // all 12 bytes printable: accept as a full-width command
                    String::from_utf8_lossy(cmd_bytes).into_owned()
                }
                _ => return Err("empty command field".into()),
            };
            let len = u32::from_le_bytes([self.buf[16], self.buf[17], self.buf[18], self.buf[19]])
                as usize;
            if len as u64 > MAX_PAYLOAD as u64 {
                return Err(format!("payload length {} > MAX_SIZE", len));
            }
            if self.buf.len() < HEADER_SIZE + len {
                break; // wait for the rest of the payload
            }
            let payload_start = HEADER_SIZE;
            let payload_end = HEADER_SIZE + len;
            let expect = checksum(&self.buf[payload_start..payload_end]);
            let got = u32::from_le_bytes([self.buf[20], self.buf[21], self.buf[22], self.buf[23]]);
            if got != expect {
                return Err(format!(
                    "checksum mismatch for {:?}: hdr {:08x} want {:08x}",
                    command, got, expect
                ));
            }
            let payload = self.buf[payload_start..payload_end].to_vec();
            self.buf.drain(..payload_end);
            out.push(RawMessage { command, payload });
        }
        Ok(out)
    }
}

impl Default for MessageDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CAddress / version handshake
// ---------------------------------------------------------------------------
/// OnionCat prefix (netbase.cpp pchOnionCat): ip[0..6] of a v3 onion CNetAddr.
pub const ONIONCAT: [u8; 6] = [0xFD, 0x87, 0xD8, 0x7E, 0xEB, 0x43];
/// GarliCat prefix (netbase.cpp pchGarliCat): i2p is IMPLIED by this ip
/// prefix — there is NO i2p flag byte on the legacy wire.
pub const GARLICAT: [u8; 6] = [0xFD, 0x60, 0xDB, 0x4D, 0xDD, 0xB5];

// ---------------------------------------------------------------------------
// Tor v3 onion label encoding (rend-spec-v3 §F): the address TEXT is
// base32(pubkey(32) || checksum(2) || version(1)=0x03) where
// checksum = sha3_256(".onion checksum" || pubkey || 0x03)[..2].
// This is a TEXT ENCODING of the 32-byte ed25519 pubkey tor generated —
// no curve arithmetic, no key generation, no derivation of secrets happens
// here. Given a pubkey, the hostname is fully determined; that is what lets
// wire-learned gossip be reconstructed into a dialable `.onion` hostname.
// ---------------------------------------------------------------------------

/// v3 label checksum: first 2 bytes of
/// `sha3_256(".onion checksum" || pubkey || 0x03)`.
pub fn onion_v3_checksum(pubkey: &[u8; 32]) -> [u8; 2] {
    let mut h = Sha3_256::new();
    h.update(b".onion checksum");
    h.update(pubkey);
    h.update([0x03u8]);
    let d = h.finalize();
    [d[0], d[1]]
}

/// The 56-char lowercase base32 label (without the `.onion` suffix) for a
/// v3 service pubkey — the inverse of `WireAddr::from_onion_label`'s decode.
pub fn onion_v3_label(pubkey: &[u8; 32]) -> String {
    let mut raw = [0u8; 35];
    raw[..32].copy_from_slice(pubkey);
    let cs = onion_v3_checksum(pubkey);
    raw[32] = cs[0];
    raw[33] = cs[1];
    raw[34] = 0x03;
    WireAddr::base32_encode_rfc4648(&raw)
}

/// Legacy wire layout (verified against /opt/triangles-v5/src, netbase.h:88-93,
/// netbase.cpp SetSpecial/ToStringIP, protocol.h:84-109):
///
///   CNetAddr  = ip[16] | tor_v3_pubkey[32] | m_is_tor_v3 (1 byte)  = 49
///   CService  = CNetAddr + port (big-endian u16)                   = 51
///   CAddress  = nTime(u32 LE) | nServices(u64 LE) | CService       = 63
///
/// v3 onion: ip[0..6] = FD 87 D8 7E EB 43 (OnionCat), rest zero;
/// tor_v3_pubkey = the 32 bytes from the onion label; m_is_tor_v3 = true.
/// i2p is implied by ip[0..6] == FD 60 DB 4D DD B5 (GarliCat).
///
/// The tri7 shape (ip + key + flags + port, 60 bytes without time) is the
/// STANDARD ENCODED form — what every deployed tri7 node parses. The legacy
/// C++ 51-byte layout is still PARSED leniently so C++ daemons and mixed
/// fleets interop during the migration; the only legacy encoder tri7 ships is
/// `encode_service`, for C++-daemon compatibility.
///
/// The version-message form omits nTime because the version stream version
/// is INIT_PROTO_VERSION (209) < CADDR_TIME_VERSION.
#[derive(Debug, Clone, PartialEq)]
pub struct WireAddr {
    pub services: u64,
    pub ip: [u8; 16],
    pub tor_key: [u8; 32],
    pub is_tor_v3: bool,
    /// Parsed from the tri7 shape's i2p flag byte (serialized there) or the
    /// GarliCat ip prefix; the legacy C++ wire has no i2p byte, so a legacy
    /// parse always yields this from the prefix.
    pub is_i2p: bool,
    pub port: u16,
}

impl WireAddr {
    /// IPv4 peer in IPv4-mapped form (netbase.h IsIPv4: 10 zero bytes +
    /// 0xff 0xff prefix).
    pub fn ipv4(a: u8, b: u8, c: u8, d: u8, port: u16, services: u64) -> Self {
        let mut ip = [0u8; 16];
        ip[10] = 0xff;
        ip[11] = 0xff;
        ip[12] = a;
        ip[13] = b;
        ip[14] = c;
        ip[15] = d;
        WireAddr {
            services,
            ip,
            tor_key: [0u8; 32],
            is_tor_v3: false,
            is_i2p: false,
            port,
        }
    }

    /// v3 onion wire form (SetSpecial parity): OnionCat ip prefix, rest zero,
    /// the label's 32 key bytes, m_is_tor_v3 = true.
    pub fn onion_v3(key: [u8; 32], port: u16, services: u64) -> Self {
        let mut ip = [0u8; 16];
        ip[..6].copy_from_slice(&ONIONCAT);
        WireAddr {
            services,
            ip,
            tor_key: key,
            is_tor_v3: true,
            is_i2p: false,
            port,
        }
    }

    /// Wire form of a published v3 onion hostname. The address already exists
    /// (Tor's `<HiddenServiceDir>/hostname` file or an ADD_ONION reply);
    /// this only PARSES it into the wire's tor_v3_pubkey bytes — netbase.cpp
    /// SetSpecial (615-632) parity. Round-2 gate: unlike the lenient legacy
    /// SetSpecial, a text label's 2 checksum bytes ARE verified
    /// (`sha3_256(".onion checksum" || pubkey || 0x03)[..2]`) and the version
    /// byte must be 0x03 — a label that fails either is not an address tor
    /// could ever have served, so it is rejected instead of stored.
    pub fn from_onion_label(label: &str, port: u16, services: u64) -> Result<WireAddr, String> {
        let label = label.strip_suffix(".onion").unwrap_or(label);
        if label.len() != 56 {
            return Err(format!(
                "onion label must be 56 chars, got {}",
                label.len()
            ));
        }
        let raw = Self::base32_decode_rfc4648(label)
            .ok_or_else(|| format!("onion label has non-base32 chars"))?;
        if raw.len() != 35 {
            return Err(format!(
                "onion label decodes to {} bytes, want 35",
                raw.len()
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw[..32]);
        if raw[32..34] != onion_v3_checksum(&key) {
            return Err("onion label checksum mismatch — rejecting (not a real v3 address)".into());
        }
        if raw[34] != 0x03 {
            return Err(format!(
                "onion label version byte is {:02x}, want 03",
                raw[34]
            ));
        }
        Ok(WireAddr::onion_v3(key, port, services))
    }

    /// Reconstructed v3 onion HOSTNAME for this address:
    /// `base32(pubkey(32) || checksum(2) || 0x03) + ".onion"` where
    /// `checksum = sha3_256(".onion checksum" || pubkey || 0x03)[..2]`.
    /// The checksum is a pure text-encoding function OF the 32-byte pubkey
    /// tor generated (no curve arithmetic here), so this reconstructs —
    /// never fabricates — the address tor serves for that key. This is what
    /// makes wire-learned gossip dialable: the store keeps the hostname and
    /// the dial loop dispatches on `.onion`. None for non-v3 addresses.
    pub fn onion_hostname(&self) -> Option<String> {
        if !self.is_tor_v3 {
            return None;
        }
        Some(format!("{}.onion", onion_v3_label(&self.tor_key)))
    }

    /// RFC 4648 base32 DECODE (unpadded, A-Z2-7). Tor v3 labels are base32
    /// text of 35 bytes; Tor's control port hands us the label already
    /// encoded, and the legacy SetSpecial only ever DECODES labels — so
    /// decoding a published address into wire bytes is pure text parsing.
    fn base32_decode_rfc4648(s: &str) -> Option<Vec<u8>> {
        const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        let s = s.trim_end_matches('=');
        if s.is_empty() {
            return Some(Vec::new());
        }
        // RFC 4648: only these unpadded lengths are canonical
        if !matches!(s.len() % 8, 0 | 2 | 4 | 5 | 7) {
            return None;
        }
        let mut out = Vec::with_capacity(s.len() * 5 / 8);
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        for &ch in s.as_bytes() {
            let up = ch.to_ascii_uppercase();
            let v = ALPHABET.iter().position(|&c| c == up)? as u32;
            buf = (buf << 5) | v;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Some(out)
    }

    /// RFC 4648 base32 ENCODE (lowercase, unpadded, a-z2-7) — the inverse of
    /// `base32_decode_rfc4648` and exactly tor's label text form. This is
    /// bit-packing, not cryptography: the ed25519 key bytes come from tor,
    /// this only spells them out the way a v3 onion address is written.
    fn base32_encode_rfc4648(data: &[u8]) -> String {
        const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
        let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        for &b in data {
            buf = (buf << 8) | b as u32;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((buf >> bits) & 0x1f) as usize] as char);
            }
        }
        if bits > 0 {
            // final partial group: left-align the remaining bits
            out.push(ALPHABET[((buf << (5 - bits)) & 0x1f) as usize] as char);
        }
        out
    }

    /// Text identity usable as a peer-store key:
    ///  - tor_v3 → the reconstructed `.onion` HOSTNAME
    ///    (`onion_hostname()` — the v3 checksum is a pure encoding of the
    ///    tor-generated pubkey, so the hostname is exact, and it feeds the
    ///    dial loop directly: `host.ends_with(".onion")` → dialable).
    ///  - IPv4-mapped → dotted quad (never dialed on a Tor-only network,
    ///    but round-trippable for the codec).
    ///  - other shapes → None.
    pub fn store_key(&self) -> Option<String> {
        if self.is_tor_v3 {
            self.onion_hostname()
        } else if self.is_ipv4_mapped() {
            Some(format!(
                "{}.{}.{}.{}",
                self.ip[12], self.ip[13], self.ip[14], self.ip[15]
            ))
        } else {
            None
        }
    }

    /// Inverse of `store_key`: hostname (`.onion`, 56-char label), 64-char
    /// hex tor key, or dotted quad → wire form.
    pub fn from_store_key(key: &str, port: u16, services: u64) -> Option<WireAddr> {
        let lower = key.to_ascii_lowercase();
        if lower.len() == 62 && lower.ends_with(".onion") {
            return WireAddr::from_onion_label(&lower, port, services).ok();
        }
        if key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            let raw = hex_decode(key).ok()?;
            let mut tor_key = [0u8; 32];
            tor_key.copy_from_slice(&raw);
            return Some(WireAddr::onion_v3(tor_key, port, services));
        }
        let octets: Vec<u8> = key.split('.').filter_map(|o| o.parse().ok()).collect();
        if octets.len() == 4 {
            return Some(WireAddr::ipv4(
                octets[0], octets[1], octets[2], octets[3], port, services,
            ));
        }
        None
    }

    pub fn is_ipv4_mapped(&self) -> bool {
        self.ip[..10] == [0u8; 10] && self.ip[10] == 0xff && self.ip[11] == 0xff
    }

    pub fn is_i2p_prefix(&self) -> bool {
        self.ip[..6] == GARLICAT
    }

    /// CService (netbase.h): ip + tor key + flag + port BE = 51 bytes.
    pub fn encode_service(&self, w: &mut Vec<u8>) {
        w.extend_from_slice(&self.ip);
        w.extend_from_slice(&self.tor_key);
        w.push(self.is_tor_v3 as u8);
        w.extend_from_slice(&self.port.to_be_bytes());
    }

    /// CAddress without nTime (version-message form): the TRI7 shape —
    /// services + ip + key + m_is_tor_v3 + m_is_i2p + port BE = 60 bytes.
    /// This is the standard tri7 wire form and the fleet-compat default: the
    /// 51-byte LEGACY C++ shape tri7 briefly emitted made every deployed node
    /// reject the handshake ("short read: want 116"). The legacy C++ 51-byte
    /// CService form remains available via `encode_service`, and the decoders
    /// accept both shapes.
    pub fn encode_no_time(&self, w: &mut Vec<u8>) {
        w.extend_from_slice(&self.services.to_le_bytes());
        w.extend_from_slice(&self.ip);
        w.extend_from_slice(&self.tor_key);
        w.push(self.is_tor_v3 as u8);
        w.push(self.is_i2p as u8);
        w.extend_from_slice(&self.port.to_be_bytes());
    }

    /// CAddress with nTime (addr-message entry): time + the tri7 shape
    /// above = 64 bytes — the form deployed tri7 peers parse.
    pub fn encode_caddress(&self, time: u32) -> Vec<u8> {
        let mut w = Vec::with_capacity(64);
        w.extend_from_slice(&time.to_le_bytes());
        self.encode_no_time(&mut w);
        w
    }

    /// Legacy-shape decode (59 bytes without time). i2p comes from the
    /// GarliCat ip prefix, never from a wire byte.
    pub fn decode_no_time(r: &mut Reader) -> Result<WireAddr, String> {
        let services = r.i64le()? as u64;
        let (ip, tor_key, is_tor_v3) = decode_cnetaddr_body(r)?;
        // legacy: port (big-endian u16) directly after the flag byte
        let port = read_port_be(r)?;
        let mut addr = WireAddr {
            services,
            ip,
            tor_key,
            is_tor_v3,
            is_i2p: false,
            port,
        };
        if addr.is_i2p_prefix() {
            addr.is_i2p = true;
        }
        Ok(addr)
    }

    /// tri7-shape decode (60 bytes without time): ip + m_is_tor_v3 +
    /// m_is_i2p + port — TWO flag bytes before the port. This IS the encode
    /// output (`encode_no_time`); the decoder is also leniently accepted from
    /// peers so mixed fleets interop.
    pub fn decode_no_time_compat(r: &mut Reader) -> Result<WireAddr, String> {
        let services = r.i64le()? as u64;
        let (ip, tor_key, is_tor_v3) = decode_cnetaddr_body(r)?;
        let is_i2p = r.u8()? != 0;
        let port = read_port_be(r)?;
        Ok(WireAddr {
            services,
            ip,
            tor_key,
            is_tor_v3,
            is_i2p,
            port,
        })
    }
}

/// The 47-byte CNetAddr body shared by both accepted shapes: ip[16] +
/// tor_v3_pubkey[32] + m_is_tor_v3(1). The tail differs per shape (legacy:
/// port; tri7 shape: i2p flag then port) and is consumed by the caller.
fn decode_cnetaddr_body(r: &mut Reader) -> Result<([u8; 16], [u8; 32], bool), String> {
    let mut ip = [0u8; 16];
    for byte in ip.iter_mut() {
        *byte = r.u8()?;
    }
    let mut tor_key = [0u8; 32];
    for byte in tor_key.iter_mut() {
        *byte = r.u8()?;
    }
    let is_tor_v3 = r.u8()? != 0;
    Ok((ip, tor_key, is_tor_v3))
}

fn read_port_be(r: &mut Reader) -> Result<u16, String> {
    let hi = r.u8()? as u16;
    let lo = r.u8()? as u16;
    Ok((hi << 8) | lo)
}

/// version payload (main.cpp:4862-4891 read order: nVersion, nServices, nTime,
/// addrMe, addrFrom, nNonce, strSubVer, nStartingHeight).
#[derive(Debug, Clone, PartialEq)]
pub struct VersionPayload {
    pub version: i32,
    pub services: u64,
    pub timestamp: i64,
    pub addr_you: WireAddr,
    pub addr_me: WireAddr,
    pub nonce: u64,
    pub user_agent: String,
    pub start_height: i32,
}

/// CNode::PushVersion field order (net.cpp:773-774).
pub fn encode_version(v: &VersionPayload) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(&v.version.to_le_bytes());
    w.extend_from_slice(&v.services.to_le_bytes());
    w.extend_from_slice(&v.timestamp.to_le_bytes());
    v.addr_you.encode_no_time(&mut w);
    v.addr_me.encode_no_time(&mut w);
    w.extend_from_slice(&v.nonce.to_le_bytes());
    push_compact_size(&mut w, v.user_agent.len() as u64);
    w.extend_from_slice(v.user_agent.as_bytes());
    w.extend_from_slice(&v.start_height.to_le_bytes());
    w
}

/// Lenient parse mirroring the daemon's version handler: subver and
/// start_height are optional tail fields (main.cpp:4884-4893 `if
/// (!vRecv.empty())`).
///
/// The CAddress fields are parsed across BOTH wire shapes: the tri7 shape
/// (60-byte CAddress-no-time) first — tri7 is the destination format — then
/// the legacy C++ layout (59-byte CAddress-no-time). The shapes are
/// discriminated by total consumed length — exactly one shape consumes a
/// well-formed payload with zero trailing bytes.
///
/// Deliberately permissive tail, in tri7-first order: if neither shape
/// consumes exactly, a tri7 parse with leftover bytes (e.g. a newer peer
/// appended unknown fields) is preferred, then a legacy parse with leftover
/// bytes. Either way the peer is not dropped. Pinned by
/// `version_message_parses_both_shapes` and
/// `unknown_trailing_fields_do_not_drop_the_peer`.
pub fn parse_version(payload: &[u8]) -> Result<VersionPayload, String> {
    // Exact matches first, tri7 before legacy (tri7 is the destination
    // format); each branch's error is kept for the final report.
    let mut compat_err = None;
    let mut legacy_err = None;
    for (tri7_shape, err_slot) in [(true, &mut compat_err), (false, &mut legacy_err)] {
        match parse_version_shape(payload, tri7_shape) {
            Ok((v, 0)) => return Ok(v),
            Ok(_) => {}
            Err(e) => *err_slot = Some(e),
        }
    }
    // Neither shape consumed exactly: prefer the TRI7 interpretation with a
    // non-canonical tail over the legacy one (tri7 is the standard; an
    // unknown-appended-fields payload from a newer peer must not be
    // mis-read as legacy), then legacy-with-tail as the last resort.
    if let Ok((v, _)) = parse_version_shape(payload, true) {
        return Ok(v);
    }
    if let Ok((v, _)) = parse_version_shape(payload, false) {
        return Ok(v);
    }
    Err(compat_err
        .or(legacy_err)
        .unwrap_or_else(|| "version payload unparseable".into()))
}

fn parse_version_shape(
    payload: &[u8],
    tri7_shape: bool,
) -> Result<(VersionPayload, usize), String> {
    let mut r = Reader::new(payload);
    let version = r.i32le()?;
    let services = r.i64le()? as u64;
    let timestamp = r.i64le()?;
    let (addr_you, addr_me) = if tri7_shape {
        (
            WireAddr::decode_no_time_compat(&mut r)?,
            WireAddr::decode_no_time_compat(&mut r)?,
        )
    } else {
        (
            WireAddr::decode_no_time(&mut r)?,
            WireAddr::decode_no_time(&mut r)?,
        )
    };
    let nonce = r.i64le()? as u64;
    let user_agent = if r.remaining() > 0 {
        String::from_utf8_lossy(&r.var_bytes()?).into_owned()
    } else {
        String::new()
    };
    let start_height = if r.remaining() >= 4 { r.i32le()? } else { -1 };
    Ok((
        VersionPayload {
            version,
            services,
            timestamp,
            addr_you,
            addr_me,
            nonce,
            user_agent,
            start_height,
        },
        r.remaining(),
    ))
}

// ---------------------------------------------------------------------------
// Requests and simple messages
// ---------------------------------------------------------------------------
/// CBlockLocator wire form (main.h:1566-1571): stream-version i32 prefix, then
/// CompactSize count, then hashes. The version prefix is the STREAM version
/// (70206 on a post-verack connection), not a locator-specific constant.
pub fn encode_locator_body(locator: &[Hash]) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    push_compact_size(&mut w, locator.len() as u64);
    for h in locator {
        w.extend_from_slice(h);
    }
    w
}

/// getheaders / getblocks payload: locator + hashStop (main.cpp:5305-5307,
/// 5378-5380 read `locator >> hashStop`).
pub fn build_getheaders(locator: &[Hash], stop: &Hash) -> Vec<u8> {
    let mut w = encode_locator_body(locator);
    w.extend_from_slice(stop);
    w
}

pub fn build_getblocks(locator: &[Hash], stop: &Hash) -> Vec<u8> {
    let mut w = encode_locator_body(locator);
    w.extend_from_slice(stop);
    w
}

/// getdata payload: CompactSize count + [i32 LE type + hash] per entry.
pub fn build_getdata(inv: &[(u32, Hash)]) -> Vec<u8> {
    let mut w = Vec::new();
    push_compact_size(&mut w, inv.len() as u64);
    for (t, h) in inv {
        w.extend_from_slice(&t.to_le_bytes());
        w.extend_from_slice(h);
    }
    w
}

/// ping payload: u64 nonce LE (BIP0031) — exactly what parse_ping reads
/// back. tri7 sends these on the keepalive cadence (poll tick) and answers
/// them (handle_message), so every healthy session carries inbound traffic
/// even when the chain is quiet at tip.
pub fn build_ping(nonce: u64) -> Vec<u8> {
    nonce.to_le_bytes().to_vec()
}

pub fn build_pong(nonce: u64) -> Vec<u8> {
    nonce.to_le_bytes().to_vec()
}

/// inv payload (announcement): CompactSize count + [i32 LE type + hash] per
/// entry — same wire shape as getdata (protocol.h CInv).
pub fn build_inv(inv: &[(u32, Hash)]) -> Vec<u8> {
    let mut w = Vec::new();
    push_compact_size(&mut w, inv.len() as u64);
    for (t, h) in inv {
        w.extend_from_slice(&t.to_le_bytes());
        w.extend_from_slice(h);
    }
    w
}

/// headers payload (getheaders response): CompactSize count + header-only
/// CBlock entries, mirroring parse_headers / main.cpp PushBlockHeaders
/// (GetBlockHeader() leaves vtx+vchBlockSig empty — 82 bytes per entry:
/// 80-byte header + CompactSize-0 vtx + CompactSize-0 vchBlockSig, exactly
/// what Block::read parses back).
pub fn encode_headers(headers: &[Block]) -> Vec<u8> {
    let mut w = Vec::new();
    push_compact_size(&mut w, headers.len() as u64);
    for b in headers {
        w.extend_from_slice(&b.header_bytes());
        push_compact_size(&mut w, 0); // vtx count: header-only entry
        push_compact_size(&mut w, 0); // vchBlockSig: empty
    }
    w
}

// ---------------------------------------------------------------------------
// Inbound payload parsers
// ---------------------------------------------------------------------------
/// inv payload (protocol.h CInv: int type + uint256 hash).
pub fn parse_inv(payload: &[u8]) -> Result<Vec<(u32, Hash)>, String> {
    let mut r = Reader::new(payload);
    let n = r.compact_size()? as usize;
    if n > 50_000 {
        return Err(format!("inv count {} > MAX_INV_SZ", n));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let t = r.i32le()? as u32;
        let h = r.hash32()?;
        out.push((t, h));
    }
    r.expect_done()?;
    Ok(out)
}

/// headers payload: CompactSize count + header-only CBlock entries
/// (GetBlockHeader() leaves vtx and vchBlockSig empty — 82 bytes each).
pub fn parse_headers(payload: &[u8]) -> Result<Vec<Block>, String> {
    let mut r = Reader::new(payload);
    let n = r.compact_size()? as usize;
    if n > 2000 {
        // the serving side caps at nLimit = 2000 (main.cpp getheaders)
        return Err(format!("headers count {} > 2000", n));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let b = Block::read(&mut r)?;
        if !b.vtx.is_empty() {
            return Err("headers entry carries transactions".into());
        }
        if !b.block_sig.is_empty() {
            return Err("headers entry carries a block signature".into());
        }
        out.push(b);
    }
    r.expect_done()?;
    Ok(out)
}

/// ping payload: u64 nonce when the peer is past BIP0031 (main.cpp:5793-5800).
pub fn parse_ping(payload: &[u8]) -> Result<u64, String> {
    if payload.is_empty() {
        return Ok(0);
    }
    if payload.len() != 8 {
        return Err(format!("ping payload length {}", payload.len()));
    }
    Ok(u64::from_le_bytes(payload.try_into().unwrap()))
}

// ---------------------------------------------------------------------------
// addr / getaddr (net.cpp ProcessMessage "getaddr"/"addr"; the legacy client
// gossiped CAddress vectors so CAddrMan stayed warm — tri7 reimplements the
// same gossip so the network survives dead seeds)
// ---------------------------------------------------------------------------
/// protocol.h/net.h: the legacy client caps a served addr vector at 1000
/// entries (vAddr.size() > MAX_ADDR_SIZE check); the same cap applies to
/// what we accept.
pub const MAX_ADDR_ENTRIES: usize = 1000;

/// One addr entry: CAddress = nTime(u32 LE) + nServices(u64 LE) + CService.
#[derive(Debug, Clone, PartialEq)]
pub struct AddrEntry {
    pub time: u32,
    pub addr: WireAddr,
}

/// addr payload: CompactSize count + N × CAddress(64 bytes, tri7 shape).
pub fn encode_addr(entries: &[AddrEntry]) -> Vec<u8> {
    let mut w = Vec::with_capacity(5 + entries.len() * 63);
    push_compact_size(&mut w, entries.len() as u64);
    for e in entries {
        w.extend_from_slice(&e.addr.encode_caddress(e.time));
    }
    w
}

/// addr payload parse. Strict, never panics:
///  - count > 1000 → Err (legacy MAX_ADDR_SIZE);
///  - body must be exactly count × 63 (legacy C++) or count × 64 (tri7
///    shape, accepted leniently); anything else (truncation, trailing junk)
///    → Err.
pub fn parse_addr(payload: &[u8]) -> Result<Vec<AddrEntry>, String> {
    let mut r = Reader::new(payload);
    let n = r.compact_size()? as usize;
    if n > MAX_ADDR_ENTRIES {
        return Err(format!(
            "addr count {} > MAX_ADDR_SIZE {}",
            n, MAX_ADDR_ENTRIES
        ));
    }
    let body = r.remaining();
    let legacy = 63usize;
    let compat = 64usize;
    let shape = if body == n.checked_mul(legacy).unwrap_or(usize::MAX) {
        legacy
    } else if body == n.checked_mul(compat).unwrap_or(usize::MAX) {
        compat
    } else {
        return Err(format!(
            "addr payload body {} bytes does not match {} entries (63 or 64 bytes each)",
            body, n
        ));
    };
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let time = r.u32le()?;
        let addr = if shape == legacy {
            WireAddr::decode_no_time(&mut r)?
        } else {
            WireAddr::decode_no_time_compat(&mut r)?
        };
        out.push(AddrEntry { time, addr });
    }
    r.expect_done()?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Display/byte-order helpers
// ---------------------------------------------------------------------------
pub fn display_to_internal(hex_display: &str) -> Result<Hash, String> {
    let b = hex_decode(hex_display)?;
    if b.len() != 32 {
        return Err(format!("hash hex length {}", b.len()));
    }
    let mut h = [0u8; 32];
    for i in 0..32 {
        h[i] = b[31 - i];
    }
    Ok(h)
}

pub fn internal_to_display(h: &Hash) -> String {
    crate::legacy::hash_to_display(h)
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char).to_digit(16).ok_or("bad hex digit")?;
        let lo = (bytes[i + 1] as char).to_digit(16).ok_or("bad hex digit")?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests — wire-framing roundtrips, checksum verification, compactsize paths
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_byte_full_command_accepted() {
        // a command filling all 12 bytes with no NUL is valid
        let frame = encode_message("filterclearX", b"").unwrap();
        let mut dec = MessageDecoder::new();
        let msgs = dec.feed(&frame).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].command, "filterclearX");
    }

    #[test]
    fn isfinal_height_vs_time_locks_and_sequence_override() {
        // exact port of CTransaction::IsFinal (main.h:573-586)
        fn is_final(locktime: u32, height: usize, block_time: u32, all_inputs_final: bool) -> bool {
            let lt = locktime as i64;
            let lock_ok = if lt == 0 {
                true
            } else if lt < 500_000_000 {
                lt < height as i64
            } else {
                lt < block_time as i64
            };
            // C++ loop: ANY non-final input ⇒ false; all-final ⇒ true
            lock_ok || all_inputs_final
        }
        // height lock (lt < 500M): final iff lt < height (STRICT)
        assert!(is_final(1000, 1001, 9_999_999, false));
        assert!(!is_final(1000, 1000, 9_999_999, false)); // equal = NOT final
                                                          // timestamp lock (lt >= 500M): final iff lt < block_time (STRICT)
        assert!(is_final(600_000_000, 0, 600_000_001, false));
        assert!(!is_final(600_000_000, 0, 600_000_000, false));
        // sequence: unsatisfied locktime + ALL inputs final ⇒ final
        assert!(is_final(999_999_999, 1, 1, true));
        // sequence: unsatisfied locktime + MIXED inputs (one final, one not) ⇒ NOT final
        assert!(!is_final(999_999_999, 1, 1, false));
    }

    fn sample_version(nonce: u64, height: i32) -> VersionPayload {
        VersionPayload {
            version: PROTOCOL_VERSION,
            services: NODE_NETWORK,
            timestamp: 1_790_000_000,
            addr_you: WireAddr::ipv4(127, 0, 0, 1, 24113, NODE_NETWORK),
            addr_me: WireAddr::ipv4(127, 0, 0, 1, 24112, NODE_NETWORK),
            nonce,
            user_agent: "/tri7:7.0.0/".into(),
            start_height: height,
        }
    }

    #[test]
    fn checksum_matches_double_sha256_vectors() {
        // Computed with python hashlib: dsha256(payload)[:4] as LE u32.
        assert_eq!(checksum(b""), 0xe2e0_f65d);
        assert_eq!(checksum(b"verack"), 0x0e8b_3c2e);
        assert_eq!(checksum(b"hello world"), 0xb8d4_62bc);
    }

    #[test]
    fn frame_roundtrip_version_message() {
        let v = sample_version(0x1122_3344_5566_7788, 2_204_064);
        let frame = encode_message("version", &encode_version(&v)).unwrap();
        // header layout checks: magic + padded command + length + checksum
        assert_eq!(&frame[0..4], &MAGIC);
        assert_eq!(&frame[4..11], b"version");
        assert_eq!(frame[11], 0);
        let len = u32::from_le_bytes([frame[16], frame[17], frame[18], frame[19]]) as usize;
        assert_eq!(len, frame.len() - HEADER_SIZE);
        assert_eq!(
            u32::from_le_bytes([frame[20], frame[21], frame[22], frame[23]]),
            checksum(&frame[HEADER_SIZE..])
        );
        // decode through the incremental decoder
        let mut dec = MessageDecoder::new();
        let msgs = dec.feed(&frame).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].command, "version");
        let got = parse_version(&msgs[0].payload).unwrap();
        assert_eq!(got, v);
    }

    #[test]
    fn split_feed_decodes_one_message_across_chunks() {
        let v = sample_version(42, 100);
        let frame = encode_message("version", &encode_version(&v)).unwrap();
        let mut dec = MessageDecoder::new();
        assert!(dec.feed(&frame[..10]).unwrap().is_empty());
        assert!(dec.feed(&frame[10..HEADER_SIZE - 3]).unwrap().is_empty());
        let msgs = dec.feed(&frame[HEADER_SIZE - 3..]).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(parse_version(&msgs[0].payload).unwrap(), v);
    }

    #[test]
    fn checksum_mismatch_and_bad_magic_rejected() {
        let mut frame = encode_message("verack", b"").unwrap();
        // corrupt the checksum byte
        let n = frame.len();
        frame[n - 1] ^= 0x01;
        let mut dec = MessageDecoder::new();
        assert!(dec.feed(&frame).is_err());
        // wrong magic
        let mut frame = encode_message("verack", b"").unwrap();
        frame[0] = 0x00;
        let mut dec = MessageDecoder::new();
        assert!(dec.feed(&frame).is_err());
        // command field with a non-zero byte AFTER the first NUL
        // (IsValid rule, protocol.cpp:57-60); "verack" ends at byte 10, so a
        // stray byte at 11 sits after the first NUL
        let mut frame = encode_message("verack", b"").unwrap();
        frame[11] = b'x';
        let mut dec = MessageDecoder::new();
        assert!(dec.feed(&frame).is_err());
        // a printable extension before the NULs ("verackx") is VALID
        let mut frame = encode_message("verack", b"").unwrap();
        frame[10] = b'x';
        let mut dec = MessageDecoder::new();
        assert!(dec.feed(&frame).is_ok());
    }

    #[test]
    fn oversized_and_nonprintable_commands_rejected() {
        assert!(encode_message("", b"").is_err());
        assert!(encode_message("thirteenbytes!!", b"").is_err());
        assert!(encode_message("has\tspace", b"").is_err());
    }

    #[test]
    fn compactsize_paths_in_version_and_locator() {
        // nonce above u32 exercises the 8-byte path, user_agent length stays
        // 1-byte; a locator with 0xfd+ entries exercises the 3-byte prefix.
        let v = sample_version(u64::MAX - 5, i32::MAX);
        let frame = encode_message("version", &encode_version(&v)).unwrap();
        let msgs = MessageDecoder::new().feed(&frame).unwrap();
        assert_eq!(parse_version(&msgs[0].payload).unwrap(), v);

        let hashes: Vec<Hash> = (0..300u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0..4].copy_from_slice(&i.to_le_bytes());
                h
            })
            .collect();
        let payload = build_getheaders(&hashes, &[9u8; 32]);
        // locator body = 4 (version) + 3 (0xfd compactsize) + 300*32 + 32 stop
        assert_eq!(payload.len(), 4 + 3 + 300 * 32 + 32);
        let mut r = Reader::new(&payload);
        assert_eq!(r.i32le().unwrap(), PROTOCOL_VERSION);
        assert_eq!(r.compact_size().unwrap(), 300);
        for i in 0..300u32 {
            let mut want = [0u8; 32];
            want[0..4].copy_from_slice(&i.to_le_bytes());
            assert_eq!(r.hash32().unwrap(), want);
        }
        assert_eq!(r.hash32().unwrap(), [9u8; 32]);
        r.expect_done().unwrap();
    }

    #[test]
    fn headers_parse_reads_header_only_entries() {
        // Build two 82-byte header-only CBlock entries the way GetBlockHeader
        // serializes: 80 header bytes + vtx count 0 + sig length 0.
        let mut hdr = Vec::new();
        hdr.extend_from_slice(&6i32.to_le_bytes());
        hdr.extend_from_slice(&[1u8; 32]); // prev
        hdr.extend_from_slice(&[2u8; 32]); // merkle
        hdr.extend_from_slice(&1_790_000_000u32.to_le_bytes());
        hdr.extend_from_slice(&0x2000_bb46u32.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        let mut payload = Vec::new();
        push_compact_size(&mut payload, 2);
        payload.extend_from_slice(&hdr);
        payload.push(0x00); // vtx count
        payload.push(0x00); // sig length
        payload.extend_from_slice(&hdr);
        payload.push(0x00);
        payload.push(0x00);
        let headers = parse_headers(&payload).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1].prev_hash, [1u8; 32]);
        assert_eq!(headers[1].merkle_root, [2u8; 32]);
        assert_eq!(headers[1].bits, 0x2000_bb46);
        assert!(headers[1].vtx.is_empty() && headers[1].block_sig.is_empty());
        // hash9 over the 80 header bytes must reproduce the same value for the
        // identical entry twice
        assert_eq!(headers[0].header_bytes(), headers[1].header_bytes());
    }

    #[test]
    fn inv_and_ping_roundtrip() {
        let inv = vec![
            (
                MSG_BLOCK,
                display_to_internal(
                    "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021",
                )
                .unwrap(),
            ),
            (MSG_TX, [7u8; 32]),
        ];
        let payload = build_getdata(&inv);
        assert_eq!(parse_inv(&payload).unwrap(), inv);
        assert_eq!(
            parse_ping(&build_pong(0xdead_beef_cafe_1234)).unwrap(),
            0xdead_beef_cafe_1234
        );
        assert_eq!(parse_ping(&[]).unwrap(), 0);
    }

    /// build_inv (announcements) is byte-identical in shape to build_getdata:
    /// a relayed inv must parse back through parse_inv for any consumer.
    #[test]
    fn build_inv_roundtrips_through_parse_inv() {
        let inv = vec![
            (MSG_BLOCK, [0x11u8; 32]),
            (MSG_BLOCK, display_to_internal(
                "0f67543090749ba77cebc918241e1e25553d10bd4ea7da395df8aabbada14a3f",
            ).unwrap()),
        ];
        let payload = build_inv(&inv);
        assert_eq!(parse_inv(&payload).unwrap(), inv);
        // and matches the getdata encoding byte-for-byte for the same entries
        assert_eq!(payload, build_getdata(&inv));
    }

    /// encode_headers (getheaders response) must parse back through
    /// parse_headers and round-trip to the identical header bytes.
    #[test]
    fn encode_headers_roundtrips_through_parse_headers() {
        fn header(prev: u8, t: u32) -> Block {
            Block {
                version: 6,
                prev_hash: [prev; 32],
                merkle_root: [prev + 1; 32],
                time: t,
                bits: 0x2000_ffff,
                nonce: 0,
                vtx: Vec::new(),
                block_sig: Vec::new(),
            }
        }
        let headers = vec![header(1, 1_793_000_000), header(2, 1_793_000_600)];
        let payload = encode_headers(&headers);
        let parsed = parse_headers(&payload).unwrap();
        assert_eq!(parsed.len(), 2);
        for (a, b) in headers.iter().zip(parsed.iter()) {
            assert_eq!(a.header_bytes(), b.header_bytes());
            assert!(b.vtx.is_empty() && b.block_sig.is_empty());
        }
        // exact per-entry size: 80 header + 1 vtx-count + 1 sig-len = 82
        let n = payload[0] as usize; // compact_size count (small n → 1 byte)
        assert_eq!(payload.len(), 1 + n * 82);
    }

    #[test]
    fn genesis_display_hash_converts_to_internal_order() {
        let internal =
            display_to_internal("7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021")
                .unwrap();
        // display order is the byte-reverse of internal order
        assert_eq!(internal[0], 0x21);
        assert_eq!(internal[31], 0x7e);
        assert_eq!(
            internal_to_display(&internal),
            "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021"
        );
    }

    #[test]
    fn wireaddr_ipv4_mapped_layout_is_60_bytes() {
        let a = WireAddr::ipv4(192, 168, 1, 5, 24113, NODE_NETWORK);
        let mut w = Vec::new();
        a.encode_no_time(&mut w);
        // tri7 fleet-shape CAddress-no-time: 8 services + 16 ip + 32 torkey
        // + 1 tor_v3 flag + 1 i2p flag + 2 port (BE) = 60 bytes
        assert_eq!(w.len(), 60);
        // IPv4-mapped prefix sits at ip[10], ip[11] → absolute 18, 19
        assert_eq!(w[18], 0xff);
        assert_eq!(w[19], 0xff);
        assert_eq!(&w[20..24], &[192, 168, 1, 5]);
        // flags then port big-endian at the tail
        assert_eq!(w[56], 0, "tor_v3 flag");
        assert_eq!(w[57], 0, "i2p flag");
        assert_eq!(&w[58..60], &24113u16.to_be_bytes());
        let mut r = Reader::new(&w);
        assert_eq!(WireAddr::decode_no_time_compat(&mut r).unwrap(), a);
        // round-trips through a full version message too
        let mut v = sample_version(7, 10);
        v.addr_you = a.clone();
        let payload = encode_version(&v);
        assert_eq!(parse_version(&payload).unwrap(), v);
    }

    // The verified-live DNS2 hidden service (docs/network-seeds.md). Its
    // label base32-decodes (python: base64.b32decode) to 35 bytes:
    // pubkey32 || checksum2 || version — the wire carries only the 32 key
    // bytes.
    const LIVE_SEED_ONION: &str = "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion";
    const LIVE_SEED_KEY_HEX: &str =
        "35eb13d77044da2f29c189e519139e0b04544db8081ee8edd1db859cd2edf1cb";

    fn live_seed_addr() -> WireAddr {
        let mut key = [0u8; 32];
        for (i, b) in hex_decode(LIVE_SEED_KEY_HEX).unwrap().into_iter().enumerate() {
            key[i] = b;
        }
        WireAddr::onion_v3(key, 24113, NODE_NETWORK)
    }

    /// A1 (updated): the DEFAULT encode is the tri7 fleet shape the deployed
    /// fleet parses — CAddress-no-time = 60 bytes, CAddress = 64 bytes — with
    /// exact hex for the verified-live seed onion. The legacy C++ 51-byte
    /// CService form is retained as `encode_service` for C++ daemons, and the
    /// decoders accept all shapes.
    #[test]
    fn wireaddr_encodes_the_fleet_shapes_and_roundtrips() {
        let a = live_seed_addr();
        let mut svc = Vec::new();
        a.encode_no_time(&mut svc);
        assert_eq!(svc.len(), 60);
        let svc_hex: String = svc.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            svc_hex,
            "0100000000000000"     // nServices 1 LE
                .to_string()
                + "fd87d87eeb4300000000000000000000" // OnionCat ip[16]
                + LIVE_SEED_KEY_HEX
                + "01"   // m_is_tor_v3
                + "00"   // m_is_i2p
                + "5e31" // port 24113 BE
        );
        let caddr = a.encode_caddress(0x67ac_1d3d);
        assert_eq!(caddr.len(), 64);
        let hex: String = caddr.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "3d1dac67" // nTime 0x67ac1d3d LE
                .to_string()
                + &svc_hex
        );
        // the whole 64-byte entry parses back through parse_addr
        let entries = parse_addr(&{
            let mut w = Vec::new();
            push_compact_size(&mut w, 1);
            w.extend_from_slice(&caddr);
            w
        })
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].time, 0x67ac_1d3d);
        assert_eq!(entries[0].addr, a);

        // retained legacy C++ CService form: 51 bytes, unchanged
        let mut legacy = Vec::new();
        a.encode_service(&mut legacy);
        assert_eq!(legacy.len(), 51);
        let legacy_hex: String = legacy.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            legacy_hex,
            "fd87d87eeb4300000000000000000000".to_string()
                + LIVE_SEED_KEY_HEX
                + "01"   // m_is_tor_v3
                + "5e31" // port 24113 BE
        );
    }

    /// A2: a hand-built LEGACY 63-byte CAddress fixture parses to the
    /// expected host (onion key + OnionCat prefix), port and flag.
    #[test]
    fn legacy_caddress_fixture_parses() {
        // nTime | nServices | ip[16] | tor_v3_pubkey[32] | flag | port BE
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&1_790_000_000u32.to_le_bytes()); // nTime
        fixture.extend_from_slice(&NODE_NETWORK.to_le_bytes()); // nServices
        fixture.extend_from_slice(&ONIONCAT);
        fixture.extend_from_slice(&[0u8; 10]);
        fixture.extend_from_slice(&[0x42u8; 32]); // tor_v3_pubkey
        fixture.push(1); // m_is_tor_v3
        fixture.extend_from_slice(&24113u16.to_be_bytes());
        assert_eq!(fixture.len(), 63);
        let entries = parse_addr(&{
            let mut w = Vec::new();
            push_compact_size(&mut w, 1);
            w.extend_from_slice(&fixture);
            w
        })
        .unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.time, 1_790_000_000);
        assert_eq!(e.addr.services, NODE_NETWORK);
        assert_eq!(e.addr.port, 24113);
        assert!(e.addr.is_tor_v3);
        assert!(!e.addr.is_i2p);
        assert_eq!(e.addr.tor_key, [0x42u8; 32]);
        assert_eq!(&e.addr.ip[..6], &ONIONCAT);
        // CService-only (51 bytes) via decode_no_time after the services word
        let mut w = Vec::new();
        w.extend_from_slice(&NODE_NETWORK.to_le_bytes());
        w.extend_from_slice(&fixture[12..]); // ip..port of the CAddress
        let mut r = Reader::new(&w);
        let back = WireAddr::decode_no_time(&mut r).unwrap();
        assert_eq!(back, e.addr);
    }

    /// A3: the TRI7 shape (60-byte CAddress-no-time with the m_is_i2p byte,
    /// 64-byte CAddress with time) parses — tri7 nodes interop with this form
    /// and with the legacy C++ 63-byte form.
    #[test]
    fn tri7_shape_parses() {
        // nTime | nServices | ip[16] | tor_v3_pubkey[32] | flag | i2p flag | port BE
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&1_791_000_000u32.to_le_bytes());
        fixture.extend_from_slice(&NODE_NETWORK.to_le_bytes());
        fixture.extend_from_slice(&ONIONCAT);
        fixture.extend_from_slice(&[0u8; 10]);
        fixture.extend_from_slice(&[0x77u8; 32]);
        fixture.push(1); // m_is_tor_v3
        fixture.push(0); // m_is_i2p
        fixture.extend_from_slice(&24113u16.to_be_bytes());
        assert_eq!(fixture.len(), 64);
        let entries = parse_addr(&{
            let mut w = Vec::new();
            push_compact_size(&mut w, 1);
            w.extend_from_slice(&fixture);
            w
        })
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].addr.is_tor_v3);
        assert!(!entries[0].addr.is_i2p);
        assert_eq!(entries[0].addr.port, 24113);
        assert_eq!(entries[0].addr.tor_key, [0x77u8; 32]);
        // an i2p flag byte set on the tri7 shape is preserved (layout: time4
        // services8 ip16 key32 tor-flag@60 i2p-flag@61 port@62..64)
        let mut i2p_fixture = fixture.clone();
        i2p_fixture[12..18].copy_from_slice(&GARLICAT);
        i2p_fixture[60] = 0; // m_is_tor_v3
        i2p_fixture[61] = 1; // m_is_i2p
        let entries = parse_addr(&{
            let mut w = Vec::new();
            push_compact_size(&mut w, 1);
            w.extend_from_slice(&i2p_fixture);
            w
        })
        .unwrap();
        assert!(entries[0].addr.is_i2p);
        assert!(!entries[0].addr.is_tor_v3);
    }

    /// A4: parse_addr returns Err (never panics) on truncation and on an
    /// oversized count.
    #[test]
    fn parse_addr_errors_never_panics() {
        let entry = live_seed_addr().encode_caddress(1);
        // truncated: valid count, short body
        let mut trunc = Vec::new();
        push_compact_size(&mut trunc, 2);
        trunc.extend_from_slice(&entry);
        assert!(parse_addr(&trunc).is_err());
        // truncated mid-entry under the legacy size (64 body vs count 1 →
        // legacy 63 doesn't match, compat 64 doesn't match)
        let mut trunc = Vec::new();
        push_compact_size(&mut trunc, 1);
        trunc.extend_from_slice(&entry[..40]);
        assert!(parse_addr(&trunc).is_err());
        // count = 5000 → over the cap
        let mut big = Vec::new();
        push_compact_size(&mut big, 5000);
        big.extend_from_slice(&entry);
        assert!(parse_addr(&big).is_err());
        // empty and junk payloads
        assert!(parse_addr(&[]).is_err());
        assert!(parse_addr(&[0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
        // trailing junk (one extra byte would look like the 64-byte
        // tri7 shape and parse leniently — two cannot match either form)
        let mut junk = Vec::new();
        push_compact_size(&mut junk, 1);
        junk.extend_from_slice(&entry);
        junk.extend_from_slice(&[0, 0]);
        assert!(parse_addr(&junk).is_err());
    }

    /// encode_addr ↔ parse_addr round-trip; the getaddr codec carries
    /// multiple entries and both shapes at their respective sizes.
    #[test]
    fn encode_addr_roundtrips_through_parse_addr() {
        let entries = vec![
            AddrEntry {
                time: 1_792_222_222,
                addr: live_seed_addr(),
            },
            AddrEntry {
                time: 1_792_222_223,
                addr: WireAddr::ipv4(10, 1, 2, 3, 24112, NODE_NETWORK),
            },
        ];
        let payload = encode_addr(&entries);
        // count(1) + 2×64 (tri7 CAddress with nTime)
        assert_eq!(payload.len(), 1 + 2 * 64);
        assert_eq!(parse_addr(&payload).unwrap(), entries);
        // an empty addr payload round-trips as zero entries
        assert!(parse_addr(&encode_addr(&[])).unwrap().is_empty());
    }

    /// i2p on the wire: the FLEET shape carries the i2p flag byte faithfully,
    /// while the retained legacy CService form has no i2p byte at all — the
    /// GarliCat ip prefix is what implies i2p on decode (netbase.cpp IsI2P).
    #[test]
    fn i2p_implied_by_garlicat_prefix_not_a_wire_byte() {
        let mut a = live_seed_addr();
        a.is_tor_v3 = false;
        a.ip[..6].copy_from_slice(&GARLICAT);
        a.is_i2p = true;
        // fleet shape: the flag byte is carried and round-trips EXACTLY —
        // decode with the tri7-shape decoder and assert the whole address
        // (a wrong decoder here would read the i2p flag as the port's high
        // byte, which the prefix-inference previously masked)
        let mut w = Vec::new();
        a.encode_no_time(&mut w);
        assert_eq!(w.len(), 60);
        let mut r = Reader::new(&w);
        let back = WireAddr::decode_no_time_compat(&mut r).unwrap();
        assert_eq!(back, a, "fleet-shape round-trip must preserve every field");
        assert!(back.is_i2p);
        assert!(!back.is_tor_v3);
        assert_eq!(back.port, 24113);
        // legacy CService form: NO i2p byte on the wire (ip + key + flag + port);
        // decode still derives i2p from the GarliCat prefix. Separate decoder
        // (59-byte legacy no_time) and separate equality — this shape cannot
        // carry the flag, so only prefix inference can set it.
        let mut legacy = Vec::new();
        legacy.extend_from_slice(&a.services.to_le_bytes());
        a.encode_service(&mut legacy);
        assert_eq!(legacy.len(), 8 + 51);
        let mut r = Reader::new(&legacy);
        let back = WireAddr::decode_no_time(&mut r).unwrap();
        assert_eq!(back, a, "legacy round-trip must also preserve every field");
        assert!(back.is_i2p, "GarliCat prefix implies i2p on decode");
        assert!(!back.is_tor_v3);
        // a legacy-encoded i2p CAddress parses with the flag derived
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&0u32.to_le_bytes());
        fixture.extend_from_slice(&NODE_NETWORK.to_le_bytes());
        fixture.extend_from_slice(&GARLICAT);
        fixture.extend_from_slice(&[0u8; 10]);
        fixture.extend_from_slice(&[0u8; 32]);
        fixture.push(0);
        fixture.extend_from_slice(&24113u16.to_be_bytes());
        let entries = parse_addr(&{
            let mut w = Vec::new();
            push_compact_size(&mut w, 1);
            w.extend_from_slice(&fixture);
            w
        })
        .unwrap();
        assert!(entries[0].addr.is_i2p);
    }

    /// The published hostname parses to the exact wire key bytes (SetSpecial
    /// parity), store_key reconstructs the HOSTNAME (dialable), and
    /// store_key/from_store_key round-trip a wire-learned peer.
    #[test]
    fn onion_label_parses_to_wire_key_and_store_key_roundtrips() {
        let a = WireAddr::from_onion_label(LIVE_SEED_ONION, 24113, NODE_NETWORK).unwrap();
        assert_eq!(a, live_seed_addr());
        assert!(a.is_tor_v3);
        // store key for a wire-learned onion is the reconstructed HOSTNAME
        // (the peer store dials `.onion` hosts — hex keys never would)
        let key = a.store_key().unwrap();
        assert_eq!(key, LIVE_SEED_ONION);
        let back = WireAddr::from_store_key(&key, 24113, NODE_NETWORK).unwrap();
        assert_eq!(back, a);
        // a hostname itself round-trips too
        let host = WireAddr::from_store_key(LIVE_SEED_ONION, 24113, NODE_NETWORK).unwrap();
        assert_eq!(host, a);
        // legacy peers.json files may still carry the hex tor key — parsed
        // for relay compatibility
        let legacy = WireAddr::from_store_key(LIVE_SEED_KEY_HEX, 24113, NODE_NETWORK).unwrap();
        assert_eq!(legacy, a);
        // bad labels rejected
        assert!(WireAddr::from_onion_label("tooshort.onion", 24113, 1).is_err());
        assert!(WireAddr::from_onion_label(
            "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
            24113,
            1
        )
        .is_err());
    }

    /// RFC 4648 base32 encode: known vector, unpadded lowercase form, and
    /// encode/decode round-trip at every length (the 35-byte onion label is
    /// the exact case: 280 bits = 56 chars, no padding).
    #[test]
    fn base32_encode_rfc4648_vectors_and_roundtrip() {
        // RFC 4648 §10 test vector for "foobar", unpadded + lowercase
        assert_eq!(WireAddr::base32_encode_rfc4648(b"foobar"), "mzxw6ytboi");
        // and the decoder agrees (it accepts both cases, strips '=')
        assert_eq!(
            WireAddr::base32_decode_rfc4648("MZXW6YTBOI======").unwrap(),
            b"foobar".to_vec()
        );
        // round-trip at every length 0..=70 (exercises every padding class)
        for n in 0..=70usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let enc = WireAddr::base32_encode_rfc4648(&data);
            assert!(enc
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
            assert_eq!(
                WireAddr::base32_decode_rfc4648(&enc).unwrap(),
                data,
                "round-trip failed at length {}",
                n
            );
        }
        // the live seed's 35 bytes encode to exactly its 56-char label
        let raw = WireAddr::base32_decode_rfc4648(
            LIVE_SEED_ONION.strip_suffix(".onion").unwrap(),
        )
        .unwrap();
        assert_eq!(raw.len(), 35);
        assert_eq!(
            WireAddr::base32_encode_rfc4648(&raw),
            LIVE_SEED_ONION.strip_suffix(".onion").unwrap(),
            "encode must reproduce tor's label spelling exactly"
        );
    }

    /// THE known-answer test: the live seed's hostname decodes to its
    /// 32-byte pubkey, and that pubkey reconstructs — through the v3
    /// checksum + base32 encode — to EXACTLY the same hostname. Also proves
    /// the checksum is the real SHA3-256(".onion checksum") value: a wrong
    /// checksum would produce a different label.
    #[test]
    fn onion_hostname_reconstructs_the_live_seed_exactly() {
        let a = WireAddr::from_onion_label(LIVE_SEED_ONION, 24113, NODE_NETWORK).unwrap();
        assert_eq!(a.tor_key.len(), 32);
        // the decoded key bytes are the documented seed pubkey
        let want: [u8; 32] = hex_decode(LIVE_SEED_KEY_HEX).unwrap().try_into().unwrap();
        assert_eq!(a.tor_key, want);
        // decode → reconstruct → the SAME hostname, byte for byte
        assert_eq!(
            a.onion_hostname().as_deref(),
            Some(LIVE_SEED_ONION),
            "live seed must round-trip to itself"
        );
        // and the free-function label form agrees (56 chars, no suffix)
        let label = onion_v3_label(&a.tor_key);
        assert_eq!(label.len(), 56);
        assert_eq!(
            format!("{}.onion", label),
            LIVE_SEED_ONION
        );
        // non-v3 addresses have no hostname
        assert_eq!(WireAddr::ipv4(10, 0, 0, 1, 24113, 1).onion_hostname(), None);
    }

    /// A label whose 2 checksum bytes (or version byte) were tampered is
    /// REJECTED — no hostname is fabricated from it. Every base32 position
    /// matters: flipping any one char breaks the parse.
    #[test]
    fn from_onion_label_rejects_checksum_and_version_tamper() {
        let label = LIVE_SEED_ONION.strip_suffix(".onion").unwrap();
        let valid = WireAddr::from_onion_label(label, 24113, NODE_NETWORK).unwrap();
        assert_eq!(valid.onion_hostname().as_deref(), Some(LIVE_SEED_ONION));
        // flip chars in all three regions: pubkey bits (0), checksum bits
        // (56 chars * 5 bits = 280; bytes 32..35 start at char 51), version
        // bits (last char) — every flip must be refused
        for pos in [0usize, 25, 51, 54, 55] {
            let mut bad: Vec<char> = label.chars().collect();
            bad[pos] = if bad[pos] == 'a' { 'b' } else { 'a' };
            let bad: String = bad.into_iter().collect();
            let err = WireAddr::from_onion_label(&bad, 24113, NODE_NETWORK)
                .err()
                .expect("tampered label must be rejected");
            assert!(
                err.contains("checksum") || err.contains("version"),
                "pos {}: unexpected error {:?}",
                pos,
                err
            );
        }
    }

    /// version messages: the tri7 fleet shape is the default encode and
    /// the lenient parser still consumes a hand-built LEGACY-shape payload
    /// (59-byte addrs) — both directions, both shapes.
    #[test]
    fn version_message_parses_both_shapes() {
        let v = sample_version(9_876, 1234);

        // fleet encode → lenient parse (tri7 shape branch)
        let payload = encode_version(&v);
        let (back, trailing) = parse_version_shape(&payload, true).unwrap();
        assert_eq!(trailing, 0);
        assert_eq!(back, v);
        assert_eq!(parse_version(&payload).unwrap(), v);

        // hand-built LEGACY-shape payload (59-byte addrs) still parses
        let mut legacy = Vec::new();
        legacy.extend_from_slice(&v.version.to_le_bytes());
        legacy.extend_from_slice(&v.services.to_le_bytes());
        legacy.extend_from_slice(&v.timestamp.to_le_bytes());
        for addr in [&v.addr_you, &v.addr_me] {
            legacy.extend_from_slice(&addr.services.to_le_bytes());
            addr.encode_service(&mut legacy);
        }
        legacy.extend_from_slice(&v.nonce.to_le_bytes());
        push_compact_size(&mut legacy, v.user_agent.len() as u64);
        legacy.extend_from_slice(v.user_agent.as_bytes());
        legacy.extend_from_slice(&v.start_height.to_le_bytes());
        // a tri7-shape parse of a legacy payload cannot consume it fully —
        // the discriminator between the two shapes: legacy branch consumes
        // exactly, tri7 branch errors on the shifted fields
        let legacy_only = parse_version_shape(&legacy, false).unwrap();
        assert_eq!(legacy_only.1, 0);
        assert!(
            parse_version_shape(&legacy, true).is_err(),
            "the tri7-shape parse must reject a legacy payload"
        );
        // and parse_version resolves it to the legacy shape
        let back = parse_version(&legacy).unwrap();
        assert_eq!(back, v);
        assert_eq!(back.addr_me.port, v.addr_me.port);
    }

    /// The permissive tail is deliberate and pinned in BOTH directions:
    /// a tri7 payload with unknown appended fields resolves via the tri7
    /// branch (tri7 is the standard), and a legacy payload with unknown
    /// appended fields resolves via the legacy branch instead of dropping
    /// the peer.
    #[test]
    fn unknown_trailing_fields_do_not_drop_the_peer() {
        // tri7 payload + unknown future fields
        let v = sample_version(4_04_04, 77);
        let mut payload = encode_version(&v);
        payload.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let back = parse_version(&payload).expect("tri7 + trailing must resolve");
        assert_eq!(back, v);

        // legacy payload + unknown future fields
        let mut legacy = Vec::new();
        legacy.extend_from_slice(&v.version.to_le_bytes());
        legacy.extend_from_slice(&v.services.to_le_bytes());
        legacy.extend_from_slice(&v.timestamp.to_le_bytes());
        for addr in [&v.addr_you, &v.addr_me] {
            legacy.extend_from_slice(&addr.services.to_le_bytes());
            addr.encode_service(&mut legacy);
        }
        legacy.extend_from_slice(&v.nonce.to_le_bytes());
        push_compact_size(&mut legacy, v.user_agent.len() as u64);
        legacy.extend_from_slice(v.user_agent.as_bytes());
        legacy.extend_from_slice(&v.start_height.to_le_bytes());
        legacy.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let back = parse_version(&legacy).expect("legacy + trailing must resolve");
        assert_eq!(back, v);
    }
}
