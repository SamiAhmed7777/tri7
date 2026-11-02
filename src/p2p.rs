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
/// One serialized CService on the v6 wire (netbase.h:205-216): ip[16] (FLATDATA)
/// + tor_v3_pubkey[32] (FLATDATA) + m_is_tor_v3 (bool→1 byte) + m_is_i2p
/// (bool→1 byte) + port u16 BIG-endian. CAddress adds nServices u64 before it
/// (protocol.h:96-109); the version-message form omits nTime because the
/// version message stream version is INIT_PROTO_VERSION (209) < CADDR_TIME_VERSION.
#[derive(Debug, Clone, PartialEq)]
pub struct WireAddr {
    pub services: u64,
    pub ip: [u8; 16],
    pub tor_key: [u8; 32],
    pub is_tor_v3: bool,
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

    pub fn encode_no_time(&self, w: &mut Vec<u8>) {
        w.extend_from_slice(&self.services.to_le_bytes());
        w.extend_from_slice(&self.ip);
        w.extend_from_slice(&self.tor_key);
        w.push(self.is_tor_v3 as u8);
        w.push(self.is_i2p as u8);
        w.extend_from_slice(&self.port.to_be_bytes());
    }

    pub fn decode_no_time(r: &mut Reader) -> Result<WireAddr, String> {
        let services = r.i64le()? as u64;
        let mut ip = [0u8; 16];
        for byte in ip.iter_mut() {
            *byte = r.u8()?;
        }
        let mut tor_key = [0u8; 32];
        for byte in tor_key.iter_mut() {
            *byte = r.u8()?;
        }
        let is_tor_v3 = r.u8()? != 0;
        let is_i2p = r.u8()? != 0;
        let hi = r.u8()? as u16;
        let lo = r.u8()? as u16;
        Ok(WireAddr {
            services,
            ip,
            tor_key,
            is_tor_v3,
            is_i2p,
            port: (hi << 8) | lo,
        })
    }
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
pub fn parse_version(payload: &[u8]) -> Result<VersionPayload, String> {
    let mut r = Reader::new(payload);
    let version = r.i32le()?;
    let services = r.i64le()? as u64;
    let timestamp = r.i64le()?;
    let addr_you = WireAddr::decode_no_time(&mut r)?;
    let addr_me = WireAddr::decode_no_time(&mut r)?;
    let nonce = r.i64le()? as u64;
    let user_agent = if r.remaining() > 0 {
        String::from_utf8_lossy(&r.var_bytes()?).into_owned()
    } else {
        String::new()
    };
    let start_height = if r.remaining() >= 4 { r.i32le()? } else { -1 };
    Ok(VersionPayload {
        version,
        services,
        timestamp,
        addr_you,
        addr_me,
        nonce,
        user_agent,
        start_height,
    })
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

pub fn build_pong(nonce: u64) -> Vec<u8> {
    nonce.to_le_bytes().to_vec()
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
            user_agent: "/tri7:0.2.0/".into(),
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
        // 8 services + 16 ip + 32 torkey + 1 + 1 flags + 2 port (BE)
        assert_eq!(w.len(), 60);
        // IPv4-mapped prefix sits at ip[10], ip[11] → absolute 18, 19
        assert_eq!(w[18], 0xff);
        assert_eq!(w[19], 0xff);
        assert_eq!(&w[20..24], &[192, 168, 1, 5]);
        // port big-endian at the tail
        assert_eq!(&w[58..60], &24113u16.to_be_bytes());
        let mut r = Reader::new(&w);
        assert_eq!(WireAddr::decode_no_time(&mut r).unwrap(), a);
    }
}
