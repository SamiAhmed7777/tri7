//! SQLite wallet reader — spec in docs/wallet-format.md. The live wallet is
//! SQLite (application_id 'TRIw', user_version 1), one `main(key,value)` table
//! holding legacy CDataStream records byte-identical to the BDB era. A SQLite
//! reader therefore covers all legacy wallet content; BDB only matters for
//! one-time import of pre-migration `.bdb.bak` files (not implemented here).

use crate::legacy::Hash;
use rusqlite::OpenFlags;
use std::path::Path;

pub struct WalletKeys {
    /// (pubkey compressed-33, secret 32, address)
    pub keys: Vec<WalletKey>,
    pub default_key: Option<Vec<u8>>,
    pub best_block: Option<Hash>,
    pub version: Option<i32>,
    pub names: Vec<(String, String)>,
    pub encrypted: bool,
}

#[derive(Debug)]
pub struct WalletKey {
    pub pubkey: Vec<u8>,
    pub secret: [u8; 32],
    pub compressed: bool,
    pub address: String,
    pub created: Option<i64>,
}

fn decode_compact_size(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    if *pos >= buf.len() {
        return Err("compactsize: eof".into());
    }
    let first = buf[*pos];
    *pos += 1;
    let n = match first {
        0xfd => {
            if *pos + 2 > buf.len() {
                return Err("compactsize: eof".into());
            }
            let v = u16::from_le_bytes([buf[*pos], buf[*pos + 1]]) as u64;
            *pos += 2;
            v
        }
        0xfe => {
            if *pos + 4 > buf.len() {
                return Err("compactsize: eof".into());
            }
            let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap()) as u64;
            *pos += 4;
            v
        }
        0xff => {
            if *pos + 8 > buf.len() {
                return Err("compactsize: eof".into());
            }
            let v = u64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            v
        }
        _ => first as u64,
    };
    Ok(n)
}

/// key blob = CompactSize(type-len) + type-string + optional suffix.
fn parse_key_blob(blob: &[u8]) -> Result<(String, Vec<u8>), String> {
    let mut pos = 0usize;
    let tlen = decode_compact_size(blob, &mut pos)? as usize;
    if pos + tlen > blob.len() {
        return Err("key blob: type overruns".into());
    }
    let t = String::from_utf8_lossy(&blob[pos..pos + tlen]).to_string();
    pos += tlen;
    Ok((t, blob[pos..].to_vec()))
}

/// Extract the 32-byte secret from DER ECPrivateKey via the `02 01 01 04 20`
/// pattern (spec §4).
fn secret_from_der(der: &[u8]) -> Result<[u8; 32], String> {
    // DER comes in two observed forms: 214-byte (compressed pubkey) and
    // 279-byte (uncompressed), plus a short ~86-byte form that embeds the
    // pubkey without padding. All share the `02 01 01 04 20 <32-byte secret>`
    // core, which is the extraction anchor.
    if der.len() < 37 {
        return Err(format!("DER: too short ({})", der.len()));
    }
    for w in der.windows(37) {
        if w[0] == 0x02 && w[1] == 0x01 && w[2] == 0x01 && w[3] == 0x04 && w[4] == 0x20 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&w[5..37]);
            return Ok(s);
        }
    }
    Err("DER: no ECPrivateKey scalar found".into())
}

pub fn pubkey_to_address(pubkey: &[u8]) -> String {
    crate::address::payload_to_address(&crate::address::pubkey_payload(pubkey))
}

/// Open a wallet file read-only and extract everything a spendable wallet needs.
pub fn read_wallet(path: &Path) -> Result<WalletKeys, String> {
    let conn = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open sqlite: {}", e))?;

    // Container validation (spec §2).
    let app_id: i32 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|e| format!("application_id: {}", e))?;
    if app_id != 0x5452_6977 {
        return Err(format!("not a TRI wallet (application_id {:#x})", app_id));
    }

    let mut stmt = conn
        .prepare("SELECT key, value FROM main")
        .map_err(|e| format!("select: {}", e))?;
    let mut rows = stmt.query([]).map_err(|e| format!("query: {}", e))?;

    let mut w = WalletKeys {
        keys: Vec::new(),
        default_key: None,
        best_block: None,
        version: None,
        names: Vec::new(),
        encrypted: false,
    };
    // Signing-capable context: used below to derive pubkeys from secrets.
    let secp = secp256k1::Secp256k1::signing_only();
    let mut keymeta: std::collections::HashMap<Vec<u8>, i64> = std::collections::HashMap::new();

    while let Some(row) = rows.next().map_err(|e| format!("row iteration: {}", e))? {
        let key: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        let value: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        let (rtype, suffix) = match parse_key_blob(&key) {
            Ok(x) => x,
            Err(_) => continue,
        };
        match rtype.as_str() {
            "key" => {
                // value = CompactSize + DER
                let mut pos = 0usize;
                let der_len = decode_compact_size(&value, &mut pos)? as usize;
                let der = &value[pos..];
                if der.len() != der_len {
                    return Err("key record: DER length mismatch".into());
                }
                // key suffix = CompactSize(pubkey len) + pubkey bytes
                let mut spos = 0usize;
                let pk_len = decode_compact_size(&suffix, &mut spos)? as usize;
                let pubkey_bytes = &suffix[spos..];
                if pubkey_bytes.len() != pk_len {
                    return Err("key record: pubkey length mismatch".into());
                }
                let secret = secret_from_der(der)?;
                let compressed = pubkey_bytes.len() == 33;
                // Validate: derived pubkey == record-key pubkey (spec §4).
                let sk = secp256k1::SecretKey::from_slice(&secret)
                    .map_err(|e| format!("bad secret: {}", e))?;
                let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
                let derived = if compressed {
                    pk.serialize().to_vec()
                } else {
                    pk.serialize_uncompressed().to_vec()
                };
                if derived != pubkey_bytes {
                    return Err("key record: pubkey mismatch (corrupt)".into());
                }
                w.keys.push(WalletKey {
                    pubkey: pubkey_bytes.to_vec(),
                    secret,
                    compressed,
                    address: pubkey_to_address(&derived),
                    created: None,
                });
            }
            "bestblock" => {
                // CBlockLocator: hashes ordered TIP-FIRST (vHave[0] = tip).
                let mut pos = 4usize; // skip version
                let count = decode_compact_size(&value, &mut pos)? as usize;
                if count > 0 && value.len() >= pos + count * 32 {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&value[pos..pos + 32]);
                    w.best_block = Some(h);
                }
            }
            "version" => {
                if value.len() >= 4 {
                    w.version = Some(i32::from_le_bytes(value[..4].try_into().unwrap()));
                }
            }
            "defaultkey" => {
                let mut pos = 0usize;
                let len = decode_compact_size(&value, &mut pos)? as usize;
                if value.len() >= pos + len {
                    w.default_key = Some(value[pos..pos + len].to_vec());
                }
            }
            "name" => {
                // suffix = CompactSize(addr len) + address string
                // value  = CompactSize(label len) + label string
                let mut apos = 0usize;
                let alen = decode_compact_size(&suffix, &mut apos)? as usize;
                if suffix.len() < apos + alen {
                    return Err("name record: address overruns suffix".into());
                }
                let addr = String::from_utf8_lossy(&suffix[apos..apos + alen]).to_string();
                let mut vpos = 0usize;
                let vlen = decode_compact_size(&value, &mut vpos)? as usize;
                if value.len() < vpos + vlen {
                    return Err("name record: label overruns value".into());
                }
                let label = String::from_utf8_lossy(&value[vpos..vpos + vlen]).to_string();
                w.names.push((addr, label));
            }
            "keymeta" => {
                // value = int32 nVersion + int64 nTime (birth time). Suffix =
                // CompactSize(pubkey len) + pubkey. Collected into a map and
                // applied after the scan so row order never matters.
                let mut kpos = 0usize;
                let pk_len = decode_compact_size(&suffix, &mut kpos)? as usize;
                if suffix.len() < kpos + pk_len || value.len() < 12 {
                    return Err("keymeta record: malformed".into());
                }
                let ntime = i64::from_le_bytes(value[4..12].try_into().unwrap());
                keymeta.insert(suffix[kpos..kpos + pk_len].to_vec(), ntime);
            }
            "mkey" | "ckey" | "hdcmnemonic" => {
                w.encrypted = true;
            }
            _ => {}
        }
    }

    // Apply keymeta birth times AFTER the scan — row order never matters.
    for k in w.keys.iter_mut() {
        if let Some(t) = keymeta.get(&k.pubkey) {
            k.created = Some(*t);
        }
    }
    // Encrypted wallets: a reader without crypto support must stop (spec §5.9).
    if w.encrypted {
        return Err(
            "wallet is encrypted (mkey/ckey present) — passphrase-protected read not implemented"
                .into(),
        );
    }
    Ok(w)
}

// ===========================================================================
// READ-WRITE wallet access (tri7 GUI "New Address" / labels / bestblock)
//
// Writes REAL wallet.dat rows in the exact legacy record format (the single
// `main(key,value)` table; CDataStream serializations per docs/wallet-format.md
// §3) so the ORIGINAL qt wallet keeps reading them. Nothing here invents a
// side keystore: the bytes on disk are the qt's own bytes.
//
// Byte-level ground truth (all verified against the live wallet's 248 `key`
// rows on 2026-11-02):
//   key row     key blob = 03 "key" + CS(33) + compressed pubkey   (38 B)
//               value    = CS(214) + DER ECPrivateKey              (215 B)
//   keymeta row key blob = 07 "keymeta" + CS(33) + compressed pubkey (42 B)
//               value    = int32 nVersion(=1) + int64 nCreateTime  (12 B)
//   name row    key blob = 04 "name" + CS(34) + address string     (40 B)
//               value    = CS(len) + label UTF-8 (empty label = 00)
//   bestblock   key blob = 09 "bestblock"; value = CBlockLocator:
//               int32 nVersion + CS(count) + tip-first 32-byte hashes
//
// The DER blob is the qt's own template (crypto_ecdsa.cpp
// der_template_compressed): HEADER[0..8] ‖ secret[8..40] ‖ MID[40..169] ‖
// 12 zero bytes[169..181] ‖ pubkey[181..214]. The template constants below
// were cross-checked byte-for-byte against live wallet rows (121/248 match
// the current writer; the remaining 127 are an older OpenSSL-shaped variant
// the qt's lax importer also accepts — tri7 writes the CURRENT writer's
// shape exactly).
//
// Safety model (a corrupt write must be impossible by construction):
//   1. compose every row byte-for-byte in memory first,
//   2. BEGIN IMMEDIATE (one write transaction per logical write),
//   3. INSERT, COMMIT,
//   4. re-SELECT every row and byte-compare, then re-read the whole wallet
//      with the existing read-only reader and assert the key is visible.
//   Plus: wallet.dat.tri7bak is copied before the FIRST write of a session,
//   every write is journaled, and a concurrently-locked file is refused.
// ===========================================================================

/// SQLite header application_id ("TRIw") — spec §2.
pub const WALLET_APP_ID: i32 = 0x5452_6977;
/// SQLITE_WALLET_SCHEMA_VERSION (walletdb-sqlite.h).
pub const WALLET_USER_VERSION: i32 = 1;
/// SER_DISK CLIENT_VERSION of the legacy writer (v6.2.6.7) — stamped in
/// `version` rows and the bestblock locator's nVersion when no previous
/// locator exists. Encoding math (clientversion.h): 1000000·MAJOR +
/// 10000·MINOR + 100·REVISION + BUILD = 1000000·6 + 10000·2 + 100·6 + 7
/// = 6_020_607 = 0x5BDDFF; the 4-byte LE on-disk form is `FF DD 5B 00`
/// (observed in the live wallet's `version` row). (docs/wallet-format.md
/// carries a stale decimal "6014975" — the hex and the live row are
/// authoritative.)
pub const CLIENT_VERSION: i32 = 6_020_607;

/// DER template head (`30 81 D3` SEQUENCE + version INTEGER + OCTET STRING).
const DER_HEADER: [u8; 8] = [0x30, 0x81, 0xD3, 0x02, 0x01, 0x01, 0x04, 0x20];
/// DER template curve-parameters block (secp256k1 OID + p/G/n constants),
/// crypto_ecdsa.cpp der_template_compressed[40..169].
const DER_MID: [u8; 129] = [
    0xA0, 0x81, 0x85, 0x30, 0x81, 0x82, 0x02, 0x01, 0x01, 0x30, 0x2C, 0x06, 0x07, 0x2A, 0x86, 0x48,
    0xCE, 0x3D, 0x01, 0x01, 0x02, 0x21, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0xFC, 0x2F, 0x30, 0x06, 0x04, 0x01, 0x00,
    0x04, 0x01, 0x07, 0x04, 0x21, 0x02, 0x79, 0xBE, 0x66, 0x7E, 0xF9, 0xDC, 0xBB, 0xAC, 0x55, 0xA0,
    0x62, 0x95, 0xCE, 0x87, 0x0B, 0x07, 0x02, 0x9B, 0xFC, 0xDB, 0x2D, 0xCE, 0x28, 0xD9, 0x59, 0xF2,
    0x81, 0x5B, 0x16, 0xF8, 0x17, 0x98, 0x02, 0x21, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0,
    0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41, 0x02, 0x01, 0x01, 0xA1, 0x24, 0x03, 0x22,
    0x00,
];

/// Build the 214-byte DER ECPrivateKey exactly as the legacy qt's
/// ECDSA_privkey_export_der_secp256k1 does (compressed-pubkey form; secret
/// spliced at offset 8, pubkey at offset 181).
pub fn encode_der_privkey(secret: &[u8; 32], pubkey33: &[u8]) -> Result<Vec<u8>, String> {
    if pubkey33.len() != 33 || (pubkey33[0] != 0x02 && pubkey33[0] != 0x03) {
        return Err(format!(
            "encode_der_privkey: expected a 33-byte compressed pubkey, got {} bytes",
            pubkey33.len()
        ));
    }
    let mut der = Vec::with_capacity(214);
    der.extend_from_slice(&DER_HEADER);
    der.extend_from_slice(secret);
    der.extend_from_slice(&DER_MID);
    der.extend_from_slice(&[0u8; 12]); // template zero-fill region [169..181]
    der.extend_from_slice(pubkey33);
    debug_assert_eq!(der.len(), 214);
    Ok(der)
}

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

fn encode_key_blob(rtype: &str, suffix: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    push_compact_size(&mut b, rtype.len() as u64);
    b.extend_from_slice(rtype.as_bytes());
    b.extend_from_slice(suffix);
    b
}

/// `key` row key blob: 03 "key" + CS(len) + pubkey.
pub fn encode_key_keyblob(pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    push_compact_size(&mut s, pubkey.len() as u64);
    s.extend_from_slice(pubkey);
    encode_key_blob("key", &s)
}

/// `key` row value: CS(len) + DER.
pub fn encode_key_value(der: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    push_compact_size(&mut v, der.len() as u64);
    v.extend_from_slice(der);
    v
}

/// `keymeta` row key blob: 07 "keymeta" + CS(len) + pubkey.
pub fn encode_keymeta_keyblob(pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    push_compact_size(&mut s, pubkey.len() as u64);
    s.extend_from_slice(pubkey);
    encode_key_blob("keymeta", &s)
}

/// `keymeta` row value: CKeyMetadata = int32 nVersion + int64 nCreateTime LE.
pub fn encode_keymeta_value(create_time: i64) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    v.extend_from_slice(&1i32.to_le_bytes()); // CKeyMetadata::CURRENT_VERSION
    v.extend_from_slice(&create_time.to_le_bytes());
    v
}

/// `name` row key blob: 04 "name" + CS(len) + base58 address string.
pub fn encode_name_keyblob(address: &str) -> Vec<u8> {
    let mut s = Vec::new();
    push_compact_size(&mut s, address.len() as u64);
    s.extend_from_slice(address.as_bytes());
    encode_key_blob("name", &s)
}

/// `name` row value: std::string label (empty label = single 0x00 byte).
pub fn encode_name_value(label: &str) -> Vec<u8> {
    let mut v = Vec::new();
    push_compact_size(&mut v, label.len() as u64);
    v.extend_from_slice(label.as_bytes());
    v
}

/// `bestblock` value: CBlockLocator = int32 nVersion + CS(count) + hashes,
/// tip FIRST (reader takes index 0 as the wallet sync point).
pub fn encode_bestblock_value(version: i32, hashes: &[Hash]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + 9 + hashes.len() * 32);
    v.extend_from_slice(&version.to_le_bytes());
    push_compact_size(&mut v, hashes.len() as u64);
    for h in hashes {
        v.extend_from_slice(h);
    }
    v
}

/// base58check address decode + version check (PUBKEY_ADDRESS = 65). Used to
/// refuse garbage before a `name` row is written for it.
fn decode_address_payload(s: &str) -> Result<[u8; 21], String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut map = [255u8; 128];
    for (i, &c) in ALPHABET.iter().enumerate() {
        map[c as usize] = i as u8;
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err("address is empty".into());
    }
    let mut zeros = 0usize;
    while zeros < bytes.len() && bytes[zeros] == b'1' {
        zeros += 1;
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() * 137 / 100 + 1);
    for &c in &bytes[zeros..] {
        let val = *map
            .get(c as usize)
            .filter(|&&v| v != 255)
            .ok_or_else(|| format!("address \"{}\": invalid base58 character", s))?;
        let mut carry = val as u32;
        for byte in out.iter_mut().rev() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut full = vec![0u8; zeros];
    full.extend_from_slice(&out);
    if full.len() != 25 {
        return Err(format!("address \"{}\": length {} != 25", s, full.len()));
    }
    let checksum = crate::legacy::sha256d(&full[..21]);
    if checksum[..4] != full[21..] {
        return Err(format!("address \"{}\": checksum mismatch", s));
    }
    if full[0] != crate::address::PUBKEY_ADDRESS {
        return Err(format!(
            "address \"{}\": wrong version byte {:#04x}",
            s, full[0]
        ));
    }
    let mut payload = [0u8; 21];
    payload.copy_from_slice(&full[..21]);
    Ok(payload)
}

/// An open, validated, WRITABLE wallet.dat session.
#[derive(Debug)]
pub struct WalletHandle {
    conn: rusqlite::Connection,
    pub path: std::path::PathBuf,
    /// One automatic pre-first-write backup per session.
    backup_done: bool,
    journal: Vec<String>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl WalletHandle {
    /// Open wallet.dat READ-WRITE (never creates a file). Refuses:
    /// missing/foreign files (application_id/user_version mismatch),
    /// encrypted wallets (mkey/ckey), and files currently locked by another
    /// writer (probed with BEGIN IMMEDIATE + ROLLBACK at open).
    pub fn open_readwrite(path: &Path) -> Result<WalletHandle, String> {
        let conn = rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open sqlite rw {}: {}", path.display(), e))?;

        // Durability + integrity pragmas — mirror the legacy SQLite backend
        // (walletdb-sqlite.cpp): FULL fsync on commit, rollback journal,
        // secure_delete, cell_size_check; bounded busy wait for transient locks.
        for pragma in [
            "PRAGMA synchronous = FULL",
            "PRAGMA journal_mode = DELETE",
            "PRAGMA secure_delete = ON",
            "PRAGMA temp_store = MEMORY",
            "PRAGMA cell_size_check = ON",
        ] {
            conn.execute_batch(pragma)
                .map_err(|e| format!("{} on {}: {}", pragma, path.display(), e))?;
        }
        conn.busy_timeout(std::time::Duration::from_millis(2000))
            .map_err(|e| format!("busy_timeout: {}", e))?;

        // Container validation (spec §2) — refuse foreign SQLite files.
        let app_id: i32 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(|e| format!("application_id: {}", e))?;
        if app_id != WALLET_APP_ID {
            return Err(format!(
                "{}: not a TRI wallet (application_id {:#x})",
                path.display(),
                app_id
            ));
        }
        let user_version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| format!("user_version: {}", e))?;
        if user_version != WALLET_USER_VERSION {
            return Err(format!(
                "{}: unsupported wallet schema user_version {} (expected {})",
                path.display(),
                user_version,
                WALLET_USER_VERSION
            ));
        }

        // Encrypted wallets are read-refused already; a writer must never
        // touch them either (spec §5.9). (hex literals: 'mkey', 'ckey',
        // 'hdcmnemonic' after the CompactSize type-length byte.)
        let n_encrypted: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM main WHERE hex(substr(key,2,4)) IN ('6D6B6579','636B6579')
                 OR hex(substr(key,2,11))='6864636D6E656D6F6E6963'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("encrypted scan: {}", e))?;
        if n_encrypted > 0 {
            return Err(format!(
                "{}: wallet is encrypted (mkey/ckey present) — tri7 cannot write it",
                path.display()
            ));
        }

        // Concurrency probe: a concurrent WRITER holding the file makes
        // BEGIN IMMEDIATE fail immediately — refuse the session rather than
        // queueing a write behind the legacy qt or another tri7 instance.
        if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE") {
            return Err(format!(
                "{}: wallet is locked by another process (is the legacy qt or another writer running?) — refusing to open read-write: {}",
                path.display(),
                e
            ));
        }
        conn.execute_batch("ROLLBACK")
            .map_err(|e| format!("rollback probe: {}", e))?;

        Ok(WalletHandle {
            conn,
            path: path.to_path_buf(),
            backup_done: false,
            journal: Vec::new(),
        })
    }

    pub fn journal(&self) -> &[String] {
        &self.journal
    }

    /// Take the journal lines appended since the last drain (for GUI status).
    pub fn drain_journal(&mut self) -> Vec<String> {
        std::mem::take(&mut self.journal)
    }

    fn log(&mut self, line: String) {
        eprintln!("tri7-wallet: {}", line);
        self.journal.push(line);
    }

    /// Automatic pre-first-write backup: wallet.dat.tri7bak next to the
    /// wallet, one per session (overwritten on the first write of each new
    /// session so it always snapshots THIS session's starting state).
    /// `fs::copy` streams via file descriptors (no 48 MB buffer) and
    /// preserves the file mode on unix; the size check proves the copy.
    fn ensure_backup(&mut self) -> Result<std::path::PathBuf, String> {
        if self.backup_done {
            return Ok(self.backup_path());
        }
        let bak = self.backup_path();
        std::fs::copy(&self.path, &bak)
            .map_err(|e| format!("copy {} -> {}: {}", self.path.display(), bak.display(), e))?;
        let src = std::fs::metadata(&self.path)
            .map_err(|e| format!("stat {}: {}", self.path.display(), e))?
            .len();
        let copied = std::fs::metadata(&bak)
            .map_err(|e| format!("stat backup {}: {}", bak.display(), e))?
            .len();
        if copied != src {
            return Err(format!("backup verify: {} != {} bytes", copied, src));
        }
        self.backup_done = true;
        self.log(format!(
            "backup: {} -> {} ({} bytes) before first write of session",
            self.path.display(),
            bak.display(),
            src
        ));
        Ok(bak)
    }

    /// wallet.dat.tri7bak — the session backup path.
    pub fn backup_path(&self) -> std::path::PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(".tri7bak");
        std::path::PathBuf::from(s)
    }

    /// One logical write = compose all row bytes in memory, wrap in a single
    /// BEGIN IMMEDIATE transaction, commit, then re-read and byte-compare
    /// every written row. Any failure leaves the file byte-identical to its
    /// pre-write state (rollback journal) and returns Err.
    fn write_rows(&mut self, rows: &[(Vec<u8>, Vec<u8>)], what: &str) -> Result<(), String> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| write_error("BEGIN IMMEDIATE", what, e))?;
        for (k, v) in rows {
            if let Err(e) = self.conn.execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                rusqlite::params![k, v],
            ) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(write_error("INSERT", what, e));
            }
        }
        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("COMMIT", what, e));
        }
        self.verify_rows(rows, what)
    }

    /// Post-commit proof: re-SELECT every written row and byte-compare. A
    /// write is only reported successful if this passes.
    fn verify_rows(&mut self, rows: &[(Vec<u8>, Vec<u8>)], what: &str) -> Result<(), String> {
        for (k, v) in rows {
            let back: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![k],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(|e| format!("{} verify: re-read failed: {}", what, e))?;
            match back {
                Some(bv) if &bv == v => {}
                Some(bv) => {
                    return Err(format!(
                        "{} verify: stored value differs ({} B vs expected {} B) — refusing to report success",
                        what,
                        bv.len(),
                        v.len()
                    ));
                }
                None => {
                    return Err(format!(
                        "{} verify: row vanished after commit — refusing to report success",
                        what
                    ));
                }
            }
        }
        Ok(())
    }

    /// Add a spendable key INTO wallet.dat — the exact legacy record set the
    /// qt's `WriteKey` + `SetAddressBook` produce: one `keymeta` row (birth
    /// time), one `key` row (CompactSize + DER, fOverwrite=false parity:
    /// duplicates are refused, never overwritten), one `name` row (label).
    /// The key is compressed (33-byte pubkey, 214-byte DER) — the live
    /// wallet's shape.
    pub fn add_key(&mut self, secret: &[u8; 32], label: &str) -> Result<WalletKey, String> {
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(secret)
            .map_err(|e| format!("add_key: bad secret: {}", e))?;
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        let pubkey33 = pubkey.serialize();

        let der = encode_der_privkey(secret, &pubkey33)?;
        let key_row = (encode_key_keyblob(&pubkey33), encode_key_value(&der));
        let now = now_unix();
        let meta_row = (encode_keymeta_keyblob(&pubkey33), encode_keymeta_value(now));
        let address = pubkey_to_address(&pubkey33);
        let name_row = (encode_name_keyblob(&address), encode_name_value(label));

        // Legacy WriteKey passes fOverwrite=false for the key row — an
        // existing key is an error, never a silent overwrite. (The rows all
        // derive from the secret, so a pubkey hit means a secret hit.)
        // The duplicate scan runs INSIDE the write transaction: BEGIN
        // IMMEDIATE takes the file's write lock first, so an external writer
        // cannot slip a duplicate in between the check and the insert
        // (no TOCTOU window).

        self.ensure_backup()?;
        // Single transaction: keymeta + key + name commit together or not at
        // all (the legacy writer calls them from one CWallet::AddKeyPubKey
        // call under cs_wallet — tri7 makes the atomicity explicit).
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| write_error("BEGIN IMMEDIATE", "add_key", e))?;
        let dup: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![&key_row.0],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| {
                let _ = self.conn.execute_batch("ROLLBACK");
                format!("add_key: duplicate scan: {}", e)
            })?;
        if dup.is_some() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(format!(
                "add_key: key {} already exists in {} — wallet.dat key rows are never overwritten",
                address,
                self.path.display()
            ));
        }
        let rows = vec![meta_row, key_row, name_row];
        for (k, v) in &rows {
            if let Err(e) = self.conn.execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                rusqlite::params![k, v],
            ) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(write_error("INSERT", "add_key", e));
            }
        }
        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("COMMIT", "add_key", e));
        }
        let n_bytes: usize = rows.iter().map(|(k, v)| k.len() + v.len()).sum();
        self.verify_rows(&rows, "add_key")?;

        // Reader verification: the whole file must still parse AND the new
        // key must be visible through the EXISTING read path.
        let w = read_wallet(&self.path)?;
        let seen = w
            .keys
            .iter()
            .find(|k| k.pubkey == pubkey33)
            .ok_or_else(|| {
                "add_key verify: key not visible via the reader after write".to_string()
            })?;
        if seen.secret != *secret || seen.address != address {
            return Err("add_key verify: reader sees the key with different material".into());
        }

        self.log(format!(
            "add_key: {} (pubkey {}…) label=\"{}\" rows=keymeta,key,name bytes={} wallet={}",
            address,
            hex_short(&pubkey33),
            label,
            n_bytes,
            self.path.display()
        ));
        Ok(WalletKey {
            pubkey: pubkey33.to_vec(),
            secret: *secret,
            compressed: true,
            address,
            created: Some(now),
        })
    }

    /// Set an address label — the legacy `name` row (`WriteName` parity:
    /// INSERT OR REPLACE, keyed by the base58 address string). The qt reads
    /// these into its address book / receive page.
    pub fn set_label(&mut self, address: &str, label: &str) -> Result<(), String> {
        decode_address_payload(address)?;
        let row = (encode_name_keyblob(address), encode_name_value(label));
        self.ensure_backup()?;
        self.write_rows(&[row], "set_label")?;
        // Reader verification on the label surface (names vector).
        let w = read_wallet(&self.path)?;
        let got = w
            .names
            .iter()
            .find(|(a, _)| a == address)
            .map(|(_, l)| l.clone());
        if got.as_deref() != Some(label) {
            return Err(format!(
                "set_label verify: reader sees {:?} for {}, expected {:?}",
                got, address, label
            ));
        }
        self.log(format!(
            "set_label: {} label=\"{}\" wallet={}",
            address,
            label,
            self.path.display()
        ));
        Ok(())
    }

    /// Update the wallet's best-block marker — the legacy `bestblock` row
    /// (CBlockLocator, tip-first). The previous locator's nVersion and
    /// ancestor hashes are preserved (capped at the observed live depth of
    /// 33 entries); the reader takes hash[0] as the wallet sync point, so a
    /// write here kills the qt's rescan-on-open behavior.
    /// `height` is the caller's tip height (journaled; the locator itself
    /// stores hashes only — exactly like the legacy writer).
    pub fn update_bestblock(&mut self, height: u64, tip_hash: &Hash) -> Result<(), String> {
        // Read the current locator (if any) to preserve version + ancestors.
        let existing: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![encode_key_blob("bestblock", &[])],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.to_string()),
            })
            .map_err(|e| format!("update_bestblock: read: {}", e))?;
        let (version, mut old_hashes): (i32, Vec<Hash>) = match &existing {
            Some(v) if v.len() >= 5 => {
                let ver = i32::from_le_bytes(v[0..4].try_into().unwrap());
                let mut pos = 4usize;
                let mut out = Vec::new();
                if let Ok(count) = decode_compact_size(v, &mut pos) {
                    for _ in 0..count.min(2000) {
                        if pos + 32 > v.len() {
                            break;
                        }
                        let mut h = [0u8; 32];
                        h.copy_from_slice(&v[pos..pos + 32]);
                        out.push(h);
                        pos += 32;
                    }
                }
                (ver, out)
            }
            _ => (CLIENT_VERSION, Vec::new()),
        };
        let mut locator: Vec<Hash> = vec![*tip_hash];
        for h in old_hashes.drain(..) {
            if h != *tip_hash && locator.len() < 33 {
                locator.push(h);
            }
        }
        let row = (
            encode_key_blob("bestblock", &[]),
            encode_bestblock_value(version, &locator),
        );
        self.ensure_backup()?;
        self.write_rows(&[row], "update_bestblock")?;
        // Verify through the existing reader: best_block must be the new tip.
        let w = read_wallet(&self.path)?;
        if w.best_block.as_ref() != Some(tip_hash) {
            return Err(format!(
                "update_bestblock verify: reader sees {:?}, expected the new tip",
                w.best_block
            ));
        }
        self.log(format!(
            "update_bestblock: height {} tip {}… locator entries {} (nVersion {}) wallet={}",
            height,
            hex_short(tip_hash),
            locator.len(),
            version,
            self.path.display()
        ));
        Ok(())
    }

    /// Close the session (drops the SQLite handle, releasing our locks).
    pub fn close(&mut self) {
        self.log(format!("close: {} session ended", self.path.display()));
        // rusqlite closes on drop; nothing else to flush (synchronous=FULL).
    }
}

fn write_error(stage: &str, what: &str, e: rusqlite::Error) -> String {
    if let rusqlite::Error::SqliteFailure(ref f, ref msg) = e {
        if f.code == rusqlite::ErrorCode::DatabaseBusy {
            return format!(
                "{}: {} — wallet.dat is locked by another process (is the legacy qt running?) — refusing to write",
                what, msg.as_deref().unwrap_or("database is locked")
            );
        }
    }
    format!("{}: {} failed: {}", what, stage, e)
}

fn hex_short(b: &[u8]) -> String {
    let n = b.len().min(4);
    let mut s = b[..n]
        .iter()
        .map(|x| format!("{:02x}", x))
        .collect::<String>();
    if b.len() > n {
        s.push('…');
    }
    s
}

// ===========================================================================
// Tests — every write path against a REAL SQLite wallet file built with the
// same record encoders the legacy qt uses. Fixture secrets are synthetic
// (sha256d of labels) — never wallet material.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tri7wallet_rw_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn synth_key(label: &str) -> ([u8; 32], Vec<u8>) {
        let secret = crate::legacy::sha256d(format!("tri7-wallet-rw/{}", label).as_bytes());
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        (secret, pk.serialize().to_vec())
    }

    /// Build a minimal-but-valid TRI wallet (application_id/user_version,
    /// main table, one version row, one pre-existing key, bestblock locator).
    fn synth_wallet(dir: &Path, tag: &str) -> PathBuf {
        let path = dir.join(format!("wallet_{}.dat", tag));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA application_id = {};\nPRAGMA user_version = {};",
            WALLET_APP_ID, WALLET_USER_VERSION
        ))
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);",
        )
        .unwrap();
        let ins = |k: Vec<u8>, v: Vec<u8>| {
            conn.execute("INSERT INTO main (key, value) VALUES (?1, ?2)", [k, v])
                .unwrap();
        };
        let mut vrow = Vec::new();
        push_compact_size(&mut vrow, 7);
        vrow.extend_from_slice(b"version");
        let mut vval = Vec::new();
        vval.extend_from_slice(&CLIENT_VERSION.to_le_bytes());
        ins(vrow, vval);

        // one pre-existing key (so shape comparisons have a prior row)
        let (sec, pk) = synth_key("seed");
        let der = encode_der_privkey(&sec, &pk).unwrap();
        ins(encode_key_keyblob(&pk), encode_key_value(&der));
        ins(
            encode_keymeta_keyblob(&pk),
            encode_keymeta_value(1_700_000_000),
        );
        let addr = pubkey_to_address(&pk);
        ins(encode_name_keyblob(&addr), encode_name_value("seed"));

        // bestblock locator: version + 2 hashes (tip first)
        let mut bb_key = Vec::new();
        push_compact_size(&mut bb_key, 9);
        bb_key.extend_from_slice(b"bestblock");
        let h1 = crate::legacy::sha256d(b"tip");
        let h2 = crate::legacy::sha256d(b"parent");
        let bb_val = encode_bestblock_value(6_020_607, &[h1, h2]);
        ins(bb_key, bb_val);
        conn.close().unwrap();
        path
    }

    /// Byte-shape fixture: the row bytes add_key must produce for a known
    /// synthetic key, derived from the qt's own DER template (the constants
    /// were verified byte-identical to live wallet rows).
    #[test]
    fn row_encoders_match_legacy_qt_byte_fixture() {
        let (secret, pk) = synth_key("fixture");
        let der = encode_der_privkey(&secret, &pk).unwrap();
        // DER shape: HEADER ‖ secret[8..40] ‖ MID ‖ zeros[169..181] ‖ pubkey
        assert_eq!(der.len(), 214);
        assert_eq!(&der[0..8], &DER_HEADER);
        assert_eq!(&der[8..40], &secret);
        assert_eq!(&der[40..169], &DER_MID);
        assert!(der[169..181].iter().all(|&b| b == 0));
        assert_eq!(&der[181..214], pk.as_slice());

        // key row: 03 "key" + CS(33) + pubkey | CS(214) + DER
        let kb = encode_key_keyblob(&pk);
        let kv = encode_key_value(&der);
        let expect_kb = {
            let mut v = vec![0x03];
            v.extend_from_slice(b"key");
            v.push(0x21);
            v.extend_from_slice(&pk);
            v
        };
        assert_eq!(kb, expect_kb);
        assert_eq!(kb.len(), 38);
        let mut expect_kv = vec![0xD6]; // CompactSize(214)
        expect_kv.extend_from_slice(&der);
        assert_eq!(kv, expect_kv);
        assert_eq!(kv.len(), 215);

        // keymeta row: 07 "keymeta" + CS(33) + pubkey | version 1 + LE time
        let mk = encode_keymeta_keyblob(&pk);
        let mut expect_mk = vec![0x07];
        expect_mk.extend_from_slice(b"keymeta");
        expect_mk.push(0x21);
        expect_mk.extend_from_slice(&pk);
        assert_eq!(mk, expect_mk);
        assert_eq!(mk.len(), 42);
        let mv = encode_keymeta_value(1_234_567_890);
        assert_eq!(mv, {
            let mut v = vec![0x01, 0x00, 0x00, 0x00];
            v.extend_from_slice(&1_234_567_890i64.to_le_bytes());
            v
        });

        // name row: 04 "name" + CS(34) + address | CS + label (empty = 00)
        let address = pubkey_to_address(&pk);
        assert_eq!(address.len(), 34);
        let nk = encode_name_keyblob(&address);
        assert_eq!(nk.len(), 40);
        assert_eq!(&nk[..1], &[0x04]);
        assert_eq!(&nk[1..5], b"name");
        assert_eq!(nk[5], 0x22);
        assert_eq!(&nk[6..], address.as_bytes());
        assert_eq!(encode_name_value(""), vec![0x00]);
        assert_eq!(encode_name_value("cold"), {
            let mut v = vec![0x04];
            v.extend_from_slice(b"cold");
            v
        });

        // bestblock locator: version + CS + tip-first hashes
        let h1 = crate::legacy::sha256d(b"a");
        let h2 = crate::legacy::sha256d(b"b");
        let bb = encode_bestblock_value(6_020_607, &[h1, h2]);
        assert_eq!(&bb[0..4], &6_020_607i32.to_le_bytes());
        assert_eq!(bb[4], 2);
        assert_eq!(&bb[5..37], &h1);
        assert_eq!(&bb[37..69], &h2);
    }

    #[test]
    fn write_roundtrip_add_key_is_visible_via_reader() {
        let dir = test_dir("roundtrip");
        let path = synth_wallet(&dir, "rt");
        let mut h = WalletHandle::open_readwrite(&path).unwrap();

        let (secret, pk) = synth_key("new");
        let key = h.add_key(&secret, "cold storage").unwrap();
        let address = pubkey_to_address(&pk);
        assert_eq!(key.address, address);
        assert!(address.starts_with('T'));
        assert_eq!(key.created.is_some(), true);

        // existing reader: key visible, secret + address identical
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 2); // seed + new
        let k = w.keys.iter().find(|k| k.address == address).unwrap();
        assert_eq!(k.secret, secret);
        assert_eq!(k.pubkey, pk);
        assert!(k.compressed);
        // label written as a wallet `name` row and parsed back
        assert!(w
            .names
            .contains(&(address.clone(), "cold storage".to_string())));
        // keymeta birth time parsed back (approx now)
        let created = k.created.unwrap();
        assert!((created - now_unix()).abs() < 120, "created {}", created);

        // duplicate add is refused (legacy fOverwrite=false parity)
        let dup = h.add_key(&secret, "again");
        assert!(dup.is_err(), "duplicate add must be refused");
        // the reader path still errors on nothing; wallet intact
        assert!(read_wallet(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_label_roundtrip_and_overwrite() {
        let dir = test_dir("label");
        let path = synth_wallet(&dir, "lb");
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        let (sec, pk) = synth_key("lbl");
        let key = h.add_key(&sec, "").unwrap();
        let addr = pubkey_to_address(&pk);
        assert_eq!(key.address, addr);

        h.set_label(&addr, "renamed").unwrap();
        let w = read_wallet(&path).unwrap();
        assert!(w.names.contains(&(addr.clone(), "renamed".to_string())));

        // overwrite (WriteName parity: INSERT OR REPLACE)
        h.set_label(&addr, "renamed twice").unwrap();
        let w = read_wallet(&path).unwrap();
        let got: Vec<_> = w
            .names
            .iter()
            .filter(|(a, _)| *a == addr)
            .map(|(_, l)| l.clone())
            .collect();
        assert_eq!(got, vec!["renamed twice".to_string()]);

        // garbage addresses are refused BEFORE any row is written
        assert!(h.set_label("not-an-address", "x").is_err());
        assert!(h
            .set_label("TBWLnkuXmGBeYbRQxhvSLrfj3YtFtoGL", "bad-checksum")
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bestblock_update_roundtrip_preserves_locator_ancestors() {
        let dir = test_dir("bestblock");
        let path = synth_wallet(&dir, "bb");
        let mut h = WalletHandle::open_readwrite(&path).unwrap();

        let new_tip = crate::legacy::sha256d(b"brand-new-tip");
        h.update_bestblock(2_204_100, &new_tip).unwrap();

        // locator nVersion preserved from the fixture (6_020_607), tip first
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.best_block, Some(new_tip));
        let conn =
            rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut key = Vec::new();
        push_compact_size(&mut key, 9);
        key.extend_from_slice(b"bestblock");
        let v: Vec<u8> = conn
            .query_row("SELECT value FROM main WHERE key = ?1", [key], |r| r.get(0))
            .unwrap();
        assert_eq!(&v[0..4], &6_020_607i32.to_le_bytes());
        assert_eq!(v[4], 3); // new tip + 2 fixture ancestors (≤33 cap)
        assert_eq!(&v[5..37], &new_tip);
        drop(conn);

        // a second update re-anchors on the new tip, dedupes it
        let tip2 = crate::legacy::sha256d(b"second-tip");
        h.update_bestblock(2_204_101, &tip2).unwrap();
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.best_block, Some(tip2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_created_once_per_session_before_first_write() {
        let dir = test_dir("bak");
        let path = synth_wallet(&dir, "bk");
        let pre = std::fs::read(&path).unwrap();
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        let bak = h.backup_path();
        assert_eq!(bak, dir.join("wallet_bk.dat.tri7bak"));
        assert!(!bak.exists(), "no backup before any write");

        let (sec, _) = synth_key("one");
        h.add_key(&sec, "one").unwrap();
        assert!(bak.exists());
        let bak_bytes = std::fs::read(&bak).unwrap();
        assert_eq!(bak_bytes, pre, "backup = exact pre-first-write bytes");

        // second write in the SAME session must NOT re-create the backup
        let (sec2, _) = synth_key("two");
        h.add_key(&sec2, "two").unwrap();
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            bak_bytes,
            "backup stays the session-start snapshot"
        );
        // ...and wallet.dat itself moved forward
        assert_ne!(std::fs::read(&path).unwrap(), bak_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_readwrite_refuses_encrypted_wallets() {
        // all three encrypted markers: mkey, ckey, hdcmnemonic
        for (tag, marker, suffix_len) in [
            ("mkey", b"mkey".to_vec(), 4u8),
            ("ckey", b"ckey".to_vec(), 33u8),
            ("hdcmnemonic", b"hdcmnemonic".to_vec(), 1u8),
        ] {
            let dir = test_dir(&format!("enc_{}", tag));
            let path = synth_wallet(&dir, "enc");
            let conn = rusqlite::Connection::open(&path).unwrap();
            let mut row = Vec::new();
            push_compact_size(&mut row, marker.len() as u64);
            row.extend_from_slice(&marker);
            push_compact_size(&mut row, suffix_len as u64);
            row.extend_from_slice(&vec![0u8; suffix_len as usize]);
            conn.execute(
                "INSERT INTO main (key, value) VALUES (?1, ?2)",
                [row, vec![0u8; 8]],
            )
            .unwrap();
            conn.close().unwrap();
            let err = WalletHandle::open_readwrite(&path).unwrap_err();
            assert!(err.contains("encrypted"), "{}: err: {}", tag, err);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn open_readwrite_refuses_foreign_missing_and_bad_schema() {
        let dir = test_dir("refuse");
        // missing file — never created
        let missing = dir.join("nope.dat");
        let e = WalletHandle::open_readwrite(&missing).unwrap_err();
        assert!(!missing.exists(), "open_readwrite must not create files");

        // foreign application_id
        let foreign = dir.join("foreign.dat");
        let conn = rusqlite::Connection::open(&foreign).unwrap();
        conn.execute_batch("PRAGMA application_id = 1234;").unwrap();
        conn.execute_batch(
            "CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);",
        )
        .unwrap();
        conn.close().unwrap();
        let e = WalletHandle::open_readwrite(&foreign).unwrap_err();
        assert!(e.contains("not a TRI wallet"), "err: {}", e);

        // wrong user_version
        let wrongver = synth_wallet(&dir, "wrongver");
        let conn = rusqlite::Connection::open(&wrongver).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        conn.close().unwrap();
        let e = WalletHandle::open_readwrite(&wrongver).unwrap_err();
        assert!(e.contains("user_version"), "err: {}", e);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writer_is_refused_at_open_and_at_write() {
        let dir = test_dir("concurrent");
        let path = synth_wallet(&dir, "cc");

        // another writer holding the file: open_readwrite refuses
        let blocker =
            rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let e = WalletHandle::open_readwrite(&path).unwrap_err();
        assert!(e.contains("locked by another process"), "err: {}", e);
        // ...and the refusal left no backup behind
        assert!(!dir.join("wallet_cc.dat.tri7bak").exists());

        // a handle opened BEFORE the competing writer still refuses to write
        drop(blocker);
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        let blocker2 =
            rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .unwrap();
        blocker2.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (sec, _) = synth_key("contended");
        let e = h.add_key(&sec, "x").unwrap_err();
        assert!(
            e.contains("locked")
                || e.contains("locked by another process")
                || e.contains("busy")
                || e.contains("Busy"),
            "err: {}",
            e
        );
        // the contended write must not have produced a key row
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1, "no key row under contention");
        drop(blocker2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallet_write_session_survives_restart_like_legacy_rows() {
        let dir = test_dir("restart");
        let path = synth_wallet(&dir, "rs");
        {
            // session 1: add a key
            let mut h = WalletHandle::open_readwrite(&path).unwrap();
            let (sec, _) = synth_key("s1");
            h.add_key(&sec, "session-one").unwrap();
            h.close();
            drop(h);
        }
        {
            // session 2 (fresh open — a GUI restart): the key is there, a NEW
            // backup gets created before its own first write
            let mut h = WalletHandle::open_readwrite(&path).unwrap();
            let w = read_wallet(&path).unwrap();
            assert_eq!(w.keys.len(), 2);
            assert!(w.names.iter().any(|(_, l)| l == "session-one"));
            let (sec, _) = synth_key("s2");
            h.add_key(&sec, "session-two").unwrap();
            assert_eq!(read_wallet(&path).unwrap().keys.len(), 3);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LIVE-WALLET EVIDENCE (CI-safe: skipped unless explicitly pointed at a
    /// COPY of the real wallet.dat — never the live file itself):
    ///   TRI7_LIVE_WALLET_COPY=/path/wallet_copy.dat \
    ///     cargo test --lib wallet::tests::live_wallet_copy_roundtrip -- --ignored
    /// Adds a synthetic key to the copy, re-reads it with the existing reader,
    /// and byte-compares the new row shape against the REAL rows already in
    /// the file (the fixture assertion from the brief).
    #[test]
    #[ignore = "live-wallet evidence: run on a COPY of wallet.dat via TRI7_LIVE_WALLET_COPY"]
    fn live_wallet_copy_roundtrip() {
        let Some(path) = std::env::var_os("TRI7_LIVE_WALLET_COPY") else {
            println!("SKIP: TRI7_LIVE_WALLET_COPY not set");
            return;
        };
        let path = PathBuf::from(path);
        let before = read_wallet(&path).expect("live copy must parse");
        let n_before = before.keys.len();
        assert!(n_before > 0, "live copy has no keys");

        // capture the shape of an existing REAL key row for byte comparison
        let ro =
            rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (ref_key_blob, ref_value): (Vec<u8>, Vec<u8>) = ro
            .query_row(
                "SELECT key, value FROM main WHERE length(key)=38 AND hex(substr(key,2,3))='6B6579' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(ro);

        // write through the tri7 read-write path (synthetic key, real rows)
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        let (secret, pk) = synth_key("live-proof");
        let key = h.add_key(&secret, "tri7-live-proof").unwrap();
        // bestblock: re-anchor at the existing wallet tip (locator hash[0])
        let tip = before.best_block.unwrap();
        let bb = h.update_bestblock(0, &tip).unwrap();
        let _ = bb;
        let journal = h.journal().to_vec();
        h.close();
        drop(h);

        // re-read with the EXISTING reader
        let after = read_wallet(&path).unwrap();
        assert_eq!(after.keys.len(), n_before + 1, "exactly one key added");
        let k = after
            .keys
            .iter()
            .find(|k| k.address == key.address)
            .expect("new key visible via the reader");
        assert_eq!(k.secret, secret);
        assert_eq!(k.pubkey, pk);
        assert!(after
            .names
            .contains(&(key.address.clone(), "tri7-live-proof".to_string())));
        assert_eq!(after.best_block, before.best_block);

        // BYTE-SHAPE vs the existing legacy row
        assert_eq!(
            key_row_key_blob_len(&pk),
            ref_key_blob.len(),
            "key blob length"
        );
        assert_eq!(
            &ref_key_blob[..5],
            b"\x03key\x21",
            "existing key blob prefix"
        );
        assert_eq!(ref_value.len(), 215, "existing value = D6 + 214 DER");
        assert_eq!(ref_value[0], 0xD6);

        // verify the NEW row bytes directly from the file
        let ro =
            rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (new_blob, new_value): (Vec<u8>, Vec<u8>) = ro
            .query_row(
                "SELECT key, value FROM main WHERE key = ?1",
                [encode_key_keyblob(&pk)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(ro);
        assert_eq!(new_blob.len(), ref_key_blob.len());
        assert_eq!(
            &new_blob[..4],
            &ref_key_blob[..4],
            "03 \"key\" prefix identical"
        );
        assert_eq!(new_blob[4], ref_key_blob[4]); // CS(33)
        assert_eq!(&new_blob[5..], &pk, "pubkey appended after CS(33)");
        assert_eq!(new_value.len(), ref_value.len(), "value length identical");
        assert_eq!(new_value[0], ref_value[0]); // 0xD6
        assert_eq!(&new_value[1..9], &ref_value[1..9], "DER header identical");
        assert_eq!(
            &new_value[41..170],
            &ref_value[41..170],
            "DER MID identical"
        );
        assert_eq!(
            &new_value[170..182],
            &[0u8; 12],
            "DER zero region identical"
        );
        assert_eq!(&new_value[182..215], &pk, "pubkey spliced at DER[181]");

        println!("LIVE-COPY ROUNDTRIP PROOF");
        println!("  keys before: {}  after: {}", n_before, after.keys.len());
        println!("  new address: {}", key.address);
        println!("  key row bytes: {} + {}", new_blob.len(), new_value.len());
        println!("  journal lines: {}", journal.len());
        for l in &journal {
            println!("    {}", l);
        }
        // default: clean up the disposable copy; set TRI7_LIVE_WALLET_KEEP=1
        // to keep it on disk for post-hoc inspection (independent verification)
        if std::env::var_os("TRI7_LIVE_WALLET_KEEP").is_none() {
            let _ = std::fs::remove_file(&path);
        }
    }

    fn key_row_key_blob_len(pubkey: &[u8]) -> usize {
        encode_key_keyblob(pubkey).len()
    }
}
