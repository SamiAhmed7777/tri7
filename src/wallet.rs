//! SQLite wallet reader — spec in docs/wallet-format.md. The live wallet is
//! SQLite (application_id 'TRIw', user_version 1), one `main(key,value)` table
//! holding legacy CDataStream records byte-identical to the BDB era. A SQLite
//! reader therefore covers all legacy wallet content; BDB only matters for
//! one-time import of pre-migration `.bdb.bak` files (not implemented here).

use crate::legacy::Hash;
use rusqlite::OpenFlags;
use std::path::Path;

#[derive(Clone)]
pub struct WalletKeys {
    /// (pubkey compressed-33, secret 32, address)
    pub keys: Vec<WalletKey>,
    pub default_key: Option<Vec<u8>>,
    pub best_block: Option<Hash>,
    pub version: Option<i32>,
    pub names: Vec<(String, String)>,
    /// Redeem scripts stored in the wallet (legacy `script` rows), as
    /// (scriptID, redeem script) — the multisig P2SH watch list
    /// (CWallet::AddCScript parity). Consumed via multisig::ScriptBook.
    pub scripts: Vec<([u8; 20], Vec<u8>)>,
    pub encrypted: bool,
    /// HD seed wallet (legacy `hdmnemonic` row, plaintext mnemonic). None =
    /// wallet has no seed phrase (pre-HD random-key wallets).
    pub hd_mnemonic: Option<String>,
    /// BIP39 passphrase ("25th word") row content; absent = no passphrase.
    pub hd_passphrase: Option<String>,
    /// `hdchain` row: next HD derivation index (legacy int64 counter).
    pub hd_chain_index: Option<i64>,
    /// `pool` rows: the legacy keypool reserve (CKeyPool entries). Keys in
    /// the pool are REAL wallet keys (their `key` rows exist); the pool row
    /// only marks "pre-derived, not handed out yet". Imported legacy wallets
    /// carry these from the qt's last TopUpKeyPool.
    pub keypool: Vec<KeyPoolEntry>,
}

#[derive(Debug, Clone)]
pub struct WalletKey {
    pub pubkey: Vec<u8>,
    pub secret: [u8; 32],
    pub compressed: bool,
    pub address: String,
    pub created: Option<i64>,
}

/// One legacy keypool reservation (`pool` row → CKeyPool). `index` is the
/// BerkeleyDB record key (the qt's mapKeyPool key); the key material itself
/// lives in the wallet's normal `key` rows — pool membership only marks the
/// key as "pre-derived, available to hand out".
#[derive(Debug, Clone)]
pub struct KeyPoolEntry {
    pub index: i64,
    /// CKeyPool nTime (seconds).
    pub created: i64,
    pub pubkey: Vec<u8>,
    pub address: String,
}

/// Result of a refill: how many keys were newly derived + pooled, the
/// resulting pool size, the oldest pooled nTime, and the HD counter the
/// wallet carries after the refill.
#[derive(Debug, Clone)]
pub struct KeyPoolRefill {
    pub derived: usize,
    pub poolsize: usize,
    pub oldest: Option<i64>,
    pub next_index: i64,
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
/// Strip the address from a `name` record suffix, accepting BOTH layouts:
///   legacy:  CS(34) T…           (std::string serialization)
///   legacy-written-by-tri7: CS(34) CS(34) T…  (double prefix, pre-2026-11-06)
fn strip_name_suffix(suffix: &[u8]) -> Vec<u8> {
    // Every layout the chain has ever written prefixes the address with one
    // length byte, and 34 == 0x22 == ASCII '"' makes the first byte ambiguous.
    // Instead of pattern-matching shapes, try each plausible split and keep the
    // candidate that actually IS an address (chain version byte + length).
    fn is_addr(b: &[u8]) -> bool {
        (b.len() == 34 || b.len() == 35) && matches!(b.first(), Some(b'T') | Some(b'C'))
    }
    let mut candidates: Vec<&[u8]> = Vec::new();
    if is_addr(suffix) {
        candidates.push(suffix);
    }
    if suffix.len() > 1 {
        let n = suffix[0] as usize;
        if n < 0xfd && suffix.len() >= 1 + n {
            candidates.push(&suffix[1..1 + n]);
        }
        candidates.push(&suffix[1..]);
        if suffix.len() > 2 && suffix[1] as usize + 2 <= suffix.len() {
            let m = suffix[1] as usize;
            candidates.push(&suffix[2..2 + m]);
        }
    }
    for c in candidates {
        if is_addr(c) {
            return c.to_vec();
        }
    }
    // nothing looked like an address: strip one length prefix, best effort
    if suffix.len() > 1 && suffix[0] < 0xfd && suffix.len() >= 1 + suffix[0] as usize {
        return suffix[1..1 + suffix[0] as usize].to_vec();
    }
    suffix.to_vec()
}
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
        scripts: Vec::new(),
        encrypted: false,
        hd_mnemonic: None,
        hd_passphrase: None,
        hd_chain_index: None,
        keypool: Vec::new(),
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
                // suffix = address string (legacy single prefix, OR the double
                //          prefix earlier tri7 builds wrote — both accepted)
                // value  = CompactSize(label len) + label string
                let addr = String::from_utf8_lossy(&strip_name_suffix(&suffix)).to_string();
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
            "script" => {
                // Redeem script (CWallet::AddCScript → WriteCScript): key
                // suffix = the raw 20-byte scriptID (no inner CompactSize),
                // value = CS(len) + script. A malformed or hash-mismatched
                // row is skipped, never fatal — it is watch-list data, not
                // key material.
                if suffix.len() == 20 {
                    let mut vpos = 0usize;
                    if let Ok(len) = decode_compact_size(&value, &mut vpos) {
                        if value.len() >= vpos + len as usize {
                            let script = value[vpos..vpos + len as usize].to_vec();
                            let mut id = [0u8; 20];
                            id.copy_from_slice(&suffix);
                            if crate::address::hash160(&script) == id {
                                w.scripts.push((id, script));
                            }
                        }
                    }
                }
            }
            "hdmnemonic" => {
                // value = CompactSize(len) + mnemonic UTF-8 (legacy
                // WriteHDMnemonic → std::string serialization). Plaintext by
                // design in UNencrypted wallets (their scheme).
                w.hd_mnemonic = Some(cs_string(&value).map_err(|e| format!("hdmnemonic: {}", e))?);
            }
            "hdpassphrase" => {
                w.hd_passphrase = Some(cs_string(&value).map_err(|e| format!("hdpassphrase: {}", e))?);
            }
            "hdchain" => {
                // value = int64 LE (their WriteHDChain(int64_t) raw int).
                if value.len() >= 8 {
                    w.hd_chain_index = Some(i64::from_le_bytes(value[..8].try_into().unwrap()));
                }
            }
            "pool" => {
                // Legacy keypool reservation (WriteKeyPool → CKeyPool).
                // Ground truth byte layout, decoded from the live wallet's
                // 116 imported rows (DNS2 2026-11-07):
                //   key   = 04 "pool" + int64 LE record index (13 B)
                //   value = int32 LE version + int64 LE nTime
                //         + CS(pubkey len) + compressed pubkey (46 B)
                // A malformed row is skipped, never fatal — pool membership
                // is bookkeeping, not key material.
                if let Some(entry) = parse_pool_row(&suffix, &value) {
                    w.keypool.push(entry);
                }
            }
            "mkey" | "ckey" | "hdcmnemonic" | "hdcpassphrase" => {
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
    // Pool entries serve lowest-index-first; row scan order never matters.
    w.keypool.sort_by_key(|e| e.index);
    // Encrypted wallets: a reader without crypto support must stop (spec §5.9).
    if w.encrypted {
        return Err(
            "wallet is encrypted (mkey/ckey present) — passphrase-protected read not implemented"
                .into(),
        );
    }
    Ok(w)
}

/// The wallet's legacy `setting` rows (name, raw value) — a targeted query,
/// NOT part of read_wallet (the key-reader surface stays untouched; legacy
/// setting rows are "read if present, ignore if absent" compat records).
/// A malformed row is skipped, never fatal.
pub fn read_settings(path: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let conn = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open sqlite: {}", e))?;
    let mut stmt = conn
        .prepare(
            // hex() comparison on BOTH predicates — substr() on a BLOB
            // returns a BLOB, and a BLOB never string-compares equal to the
            // TEXT literal 'setting' (SQLite type ordering; the format spec
            // documents exactly this trap).
            "SELECT key, value FROM main
             WHERE hex(substr(key,1,1))='07' AND hex(substr(key,2,7))='73657474696E67'",
        )
        .map_err(|e| format!("select settings: {}", e))?;
    let mut rows = stmt.query([]).map_err(|e| format!("query settings: {}", e))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| format!("row iteration: {}", e))? {
        let key: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        let value: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        // defense in depth for the "skipped, never fatal" contract: the
        // query prefix already implies ≥ 8 bytes, but a truncated row must
        // be SKIPPED here, never sliced out of bounds
        if key.len() < 8 {
            continue;
        }
        // suffix = everything after the "setting" type string
        if let Ok(name) = parse_setting_name(&key[8..]) {
            out.push((name, value));
        }
    }
    Ok(out)
}

/// Cheap targeted keypool census (count + oldest nTime) — a SQL scan of the
/// `pool` rows only, no full-file key parse. This is what getwalletinfo and
/// the background top-up check read; a wallet with no pool rows answers
/// (0, None). Never errors on row shapes (skipped, never fatal).
///
/// Census semantics (deliberate, qt-parity): EVERY pool row counts,
/// including import artifacts whose `key` row is missing — the legacy
/// GetKeyPoolSize counted mapKeyPool entries the same way, and the operator
/// reading `keypoolsize` should see the pool as the FILE holds it. The
/// artifact-skip lives in pop_keypool (issuance path) where an unspendable
/// handout would actually matter; the two views are intentionally different
/// lenses on the same rows.
pub fn keypool_stats(path: &Path) -> Result<(usize, Option<i64>), String> {
    let conn = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open sqlite: {}", e))?;
    let mut stmt = conn
        .prepare(
            "SELECT value FROM main
             WHERE hex(substr(key,1,1))='04' AND hex(substr(key,2,4))='706F6F6C'",
        )
        .map_err(|e| format!("select pool rows: {}", e))?;
    let mut rows = stmt.query([]).map_err(|e| format!("query pool rows: {}", e))?;
    let mut count = 0usize;
    let mut oldest: Option<i64> = None;
    while let Some(row) = rows.next().map_err(|e| format!("row iteration: {}", e))? {
        let value: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        if value.len() < 13 {
            continue;
        }
        count += 1;
        let created = i64::from_le_bytes(value[4..12].try_into().unwrap());
        oldest = Some(oldest.map_or(created, |o| o.min(created)));
    }
    Ok((count, oldest))
}

/// Seed-migration completion flag: setting "seedmigrated" == int32 LE 1
/// (the live wallet's numeric-setting shape, `addrIncoming` parity). The
/// read-side mirror of WalletHandle::mark_seed_migrated.
pub fn seed_migration_done(path: &Path) -> Result<bool, String> {
    Ok(read_settings(path)?.iter().any(|(n, v)| {
        n == SEED_MIGRATED_SETTING && v == &encode_setting_value_i32(1)
    }))
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
//   hdmnemonic  key blob = 0A "hdmnemonic"; value = CS(len) + mnemonic UTF-8
//   hdpassphrase key blob = 0C "hdpassphrase"; value = CS(len) + passphrase
//               (absent = no passphrase; both crypted variants erased)
//   hdchain     key blob = 07 "hdchain"; value = int64 LE (next HD index)
//   defaultkey  key blob = 0A "defaultkey"; value = CS(33) + pubkey
//   version/minversion values = raw int32 LE (6020607 / 60000)
// (HD rows byte-matched to triangles_v5 walletdb.h WriteHDMnemonic /
//  WriteHDPassphrase / WriteHDChain / WriteDefaultKey serializations.)
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

/// `pool` row key blob: 04 "pool" + int64 LE record index (13 bytes).
/// Byte-matched to the live wallet's imported legacy rows (bdb_import keeps
/// the BerkeleyDB suffix byte-exact: the raw 8-byte LE index, no inner CS).
pub fn encode_pool_keyblob(index: i64) -> Vec<u8> {
    encode_key_blob("pool", &index.to_le_bytes())
}

/// Version int the legacy qt wrote in its pool rows (0x004DB174, read off
/// the live wallet's 116 imported rows — keep writing the same constant so
/// the qt's pool loader sees its own shape).
pub const KEYPOOL_ROW_VERSION: i32 = 0x004D_B174;

/// `pool` row value: int32 LE version + int64 LE nTime + CS(len) + pubkey
/// (46 bytes for a compressed key — the CKeyPool serialization).
pub fn encode_pool_value(create_time: i64, pubkey: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(13 + pubkey.len());
    v.extend_from_slice(&KEYPOOL_ROW_VERSION.to_le_bytes());
    v.extend_from_slice(&create_time.to_le_bytes());
    push_compact_size(&mut v, pubkey.len() as u64);
    v.extend_from_slice(pubkey);
    v
}

/// Decode one `pool` row. `suffix` is the key blob's bytes after "pool"
/// (the 8-byte LE record index); `value` is the CKeyPool serialization.
/// Returns None on any malformed input (skipped, never fatal — see the
/// reader arm).
fn parse_pool_row(suffix: &[u8], value: &[u8]) -> Option<KeyPoolEntry> {
    if suffix.len() != 8 || value.len() < 13 {
        return None;
    }
    let index = i64::from_le_bytes(suffix.try_into().ok()?);
    let created = i64::from_le_bytes(value[4..12].try_into().ok()?);
    let mut pos = 12usize;
    let pk_len = decode_compact_size(value, &mut pos).ok()? as usize;
    if value.len() != pos + pk_len || pk_len == 0 {
        return None;
    }
    let pubkey = value[pos..pos + pk_len].to_vec();
    let address = pubkey_to_address(&pubkey);
    Some(KeyPoolEntry {
        index,
        created,
        pubkey,
        address,
    })
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
///
/// NOTE (2026-11-06): this is the shape the wallet READER accepts, but the
/// LEGACY client writes these rows as `std::string` values, i.e. a bare
/// CompactSize-prefixed string with NO inner CompactSize:
/// `04 "name" 22 "T…"` (one length byte). Rows written by this helper appear
/// with a visible leading quote in a hex dump (`04 6e616d65 22 22 54…`) because
/// 0x22 is both the CS(34) byte and the ASCII '"'. Addresses are 34 chars, so
/// that byte doubles as the opening quote and the reader still parses them —
/// but a legacy QT reading a wallet we wrote would see a corrupt name. Use
/// `encode_legacy_name_keyblob` for any row that must be byte-identical to the
/// original client's.
pub fn encode_name_keyblob(address: &str) -> Vec<u8> {
    let mut s = Vec::new();
    push_compact_size(&mut s, address.len() as u64);
    s.extend_from_slice(address.as_bytes());
    encode_key_blob("name", &s)
}

/// `name` row key blob in the ORIGINAL client's byte layout: the address is a
/// serialized `std::string`, so the record key is `CS(4) "name" CS(34) T…` with
/// exactly ONE length prefix. Byte-identical to what the Qt wallet writes.
pub fn encode_legacy_name_keyblob(address: &str) -> Vec<u8> {
    let mut k = Vec::new();
    push_compact_size(&mut k, 4);
    k.extend_from_slice(b"name");
    push_compact_size(&mut k, address.len() as u64);
    k.extend_from_slice(address.as_bytes());
    k
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

/// Parse a CompactSize-prefixed UTF-8 string value (std::string CDataStream
/// serialization — the shape of `hdmnemonic`/`hdpassphrase`/`name` values).
fn cs_string(value: &[u8]) -> Result<String, String> {
    let mut pos = 0usize;
    let len = decode_compact_size(value, &mut pos)? as usize;
    if value.len() < pos + len {
        return Err("string overruns value".into());
    }
    Ok(String::from_utf8_lossy(&value[pos..pos + len]).to_string())
}

/// `version` row value: raw int32 LE (their `WriteVersion`/ssValue << int).
pub fn encode_version_value(version: i32) -> Vec<u8> {
    version.to_le_bytes().to_vec()
}

/// `minversion` row value: raw int32 LE (observed 60000 on live wallets).
pub fn encode_minversion_value(min_version: i32) -> Vec<u8> {
    min_version.to_le_bytes().to_vec()
}

/// `hdchain` row value: raw int64 LE — the legacy WriteHDChain(int64_t)
/// serialization of the HD derivation-index counter.
pub fn encode_hdchain_value(next_index: i64) -> Vec<u8> {
    next_index.to_le_bytes().to_vec()
}

/// `defaultkey` row value: CompactSize + pubkey bytes (their
/// WriteDefaultKey writes vchPubKey.Raw(), a raw byte vector).
pub fn encode_defaultkey_value(pubkey: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + pubkey.len() + 9);
    push_compact_size(&mut v, pubkey.len() as u64);
    v.extend_from_slice(pubkey);
    v
}

/// `script` row key blob: 06 "script" + <20-byte scriptID> — the legacy
/// CWalletDB::WriteCScript record (CWallet::AddCScript, wallet.cpp:190):
/// `Write(make_pair(string("script"), scriptID), redeemScript)`. The
/// scriptID is a uint160, which serializes as RAW bytes — unlike the
/// vector-suffixed rows above it carries NO inner CompactSize.
pub fn encode_script_keyblob(script_id: &[u8; 20]) -> Vec<u8> {
    encode_key_blob("script", script_id)
}

/// `script` row value: CS(len) + raw redeem script bytes (CScript is a
/// vector<unsigned char>: CompactSize + data, like encode_key_value).
pub fn encode_script_value(script: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(script.len() + 3);
    push_compact_size(&mut v, script.len() as u64);
    v.extend_from_slice(script);
    v
}

/// Decode a `script` row value (CS(len) + bytes) back to the raw redeem
/// script. A malformed row is an error, never a guess.
fn decode_script_value(value: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 0usize;
    let len = decode_compact_size(value, &mut pos)? as usize;
    if value.len() < pos + len {
        return Err("script record: payload overruns value".into());
    }
    Ok(value[pos..pos + len].to_vec())
}

/// tri7's single wallet setting name: the seed-migration sweep-completion
/// marker (the double-sweep guard).
pub const SEED_MIGRATED_SETTING: &str = "seedmigrated";

/// `setting` row key blob: 07 "setting" + CS(name len) + name — byte-shape
/// copied from the live wallet (`07 "setting" 0C "addrIncoming"`).
pub fn encode_setting_keyblob(name: &str) -> Vec<u8> {
    let mut s = Vec::new();
    push_compact_size(&mut s, name.len() as u64);
    s.extend_from_slice(name.as_bytes());
    encode_key_blob("setting", &s)
}

/// Numeric setting value: RAW int32 LE (live parity: `addrIncoming` = 4-byte
/// int, no CompactSize wrapper).
pub fn encode_setting_value_i32(v: i32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// `setting` row suffix → the setting name (CS(len) + name bytes).
fn parse_setting_name(suffix: &[u8]) -> Result<String, String> {
    let mut pos = 0usize;
    let len = decode_compact_size(suffix, &mut pos)? as usize;
    if suffix.len() < pos + len {
        return Err("setting record: name overruns suffix".into());
    }
    Ok(String::from_utf8_lossy(&suffix[pos..pos + len]).to_string())
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
    /// HD state loaded at open (mirrors the legacy wallet's hdMnemonic /
    /// hdPassphrase / nHDChainIndex in-memory fields). None mnemonic = the
    /// wallet is NOT seed-derived (pre-HD random-key wallet).
    pub hd_mnemonic: Option<String>,
    pub hd_passphrase: Option<String>,
    pub hd_chain_index: i64,
    /// Sweep-completion marker loaded from the `seedmigrated` setting row at
    /// open — the double-sweep guard.
    pub seed_migrated: bool,
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
        // 'hdcmnemonic', 'hdcpassphrase' after the CompactSize type-length
        // byte.)
        let n_encrypted: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM main WHERE hex(substr(key,2,4)) IN ('6D6B6579','636B6579')
                 OR hex(substr(key,2,11))='6864636D6E656D6F6E6963'
                 OR hex(substr(key,2,13))='686463706173737072617365'",
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

        // Load HD state (hdmnemonic / hdpassphrase / hdchain rows) — the
        // writer-side mirror of the reader's parse; absent rows = not an HD
        // wallet (index 0 is then the starting point if a seed is set).
        let load_cs_row = |blob: Vec<u8>| -> Result<Option<String>, String> {
            let v: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![blob],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(|e| format!("hd row read: {}", e))?;
            match v {
                Some(bytes) => Ok(Some(cs_string(&bytes)?)),
                None => Ok(None),
            }
        };
        let hd_mnemonic = load_cs_row(encode_key_blob("hdmnemonic", &[]))?;
        let hd_passphrase = load_cs_row(encode_key_blob("hdpassphrase", &[]))?;
        let hd_chain_index: i64 = conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![encode_key_blob("hdchain", &[])],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map(|v| {
                if v.len() >= 8 {
                    i64::from_le_bytes(v[..8].try_into().unwrap())
                } else {
                    // forensic clarity: a malformed hdchain row is loudly
                    // reported, never silently treated as 0
                    eprintln!(
                        "tri7-wallet: {} has a malformed hdchain row ({} bytes, expected 8) — treating as 0",
                        path.display(),
                        v.len()
                    );
                    0
                }
            })
            .unwrap_or(0);
        // Sweep-completion marker (the double-sweep guard for seed migration).
        let seed_migrated: bool = conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![encode_setting_keyblob(SEED_MIGRATED_SETTING)],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map(|v| v == encode_setting_value_i32(1))
            .unwrap_or(false);

        Ok(WalletHandle {
            conn,
            path: path.to_path_buf(),
            backup_done: false,
            journal: Vec::new(),
            hd_mnemonic,
            hd_passphrase,
            hd_chain_index,
            seed_migrated,
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
        let name_row = (encode_legacy_name_keyblob(&address), encode_name_value(label));

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
        // LEGACY layout: a new label must be readable by the original client,
        // so write the single-CompactSize form (see encode_legacy_name_keyblob)
        let row = (encode_legacy_name_keyblob(address), encode_name_value(label));
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

    /// Store a redeem script keyed by its scriptID (hash160 of the script)
    /// — the legacy CWallet::AddCScript → WriteCScript("script", scriptID)
    /// row (wallet.cpp:190). Idempotent: the row is keyed by the scriptID,
    /// so storing the same script twice is a no-op. Returns the scriptID.
    pub fn put_redeem_script(&mut self, script: &[u8]) -> Result<[u8; 20], String> {
        let id = crate::address::hash160(script);
        let row = (encode_script_keyblob(&id), encode_script_value(script));
        self.ensure_backup()?;
        self.write_rows(&[row], "put_redeem_script")?;
        // re-read proof: the stored value decodes back to the exact script
        match self.get_redeem_script(&id)? {
            Some(back) if back == script => {}
            other => {
                return Err(format!(
                    "put_redeem_script verify: reader sees {} bytes, expected {}",
                    other.map(|b| b.len()).unwrap_or(0),
                    script.len()
                ))
            }
        }
        self.log(format!(
            "put_redeem_script: scriptID {}… ({} bytes) wallet={}",
            hex_short(&id),
            script.len(),
            self.path.display()
        ));
        Ok(id)
    }

    /// Read a stored redeem script by its 20-byte scriptID.
    pub fn get_redeem_script(&self, script_id: &[u8; 20]) -> Result<Option<Vec<u8>>, String> {
        let v: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![encode_script_keyblob(script_id)],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| format!("get_redeem_script: {e}"))?;
        v.map(|bytes| decode_script_value(&bytes)).transpose()
    }

    /// Every stored redeem script as (scriptID, script) pairs, in key order.
    /// A malformed row is skipped, never fatal (mirrors read_settings).
    pub fn list_redeem_scripts(&self) -> Result<Vec<([u8; 20], Vec<u8>)>, String> {
        // prefix = 0x06 (CompactSize of "script") + "script"
        let mut prefix = Vec::with_capacity(7);
        prefix.push(0x06);
        prefix.extend_from_slice(b"script");
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM main WHERE substr(key,1,7) = ?1 ORDER BY key")
            .map_err(|e| format!("list_redeem_scripts: prepare: {e}"))?;
        let mut rows = stmt
            .query(rusqlite::params![prefix.as_slice()])
            .map_err(|e| format!("list_redeem_scripts: query: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let key: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            let value: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            // a script row is exactly 06 "script" <20-byte id>; truncated or
            // malformed rows are skipped like the settings reader skips them
            if key.len() != 7 + 20 {
                continue;
            }
            let mut id = [0u8; 20];
            id.copy_from_slice(&key[7..27]);
            if let Ok(script) = decode_script_value(&value) {
                out.push((id, script));
            }
        }
        Ok(out)
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

    // ---- HD seed wallet (byte-exact port of the legacy SetHDSeed /
    // GenerateNewKey record set: hdmnemonic / hdpassphrase / hdchain) ----

    /// Activate an HD seed on this wallet — port of CWallet::SetHDSeed's
    /// database writes (fGenerate=false path; generation lives in the caller
    /// via seed::generate_mnemonic_24):
    ///   - refuse if a seed already exists (their "refusing to replace it"),
    ///   - validate the mnemonic (hd::CheckMnemonic) and prove index 0
    ///     derives before touching the file,
    ///   - erase `hdcmnemonic` then write `hdmnemonic` (their
    ///     WriteHDMnemonic: exactly one of plain/crypted exists),
    ///   - write `hdpassphrase` when non-empty, erase BOTH passphrase rows
    ///     when empty (their WriteHDPassphrase/erase lifecycle),
    ///   - write `hdchain` = 0 (their SetHDSeed resets the counter),
    /// all in ONE transaction (their TxnBegin…TxnCommit).
    pub fn set_hd_seed(&mut self, mnemonic: &str, passphrase: &str) -> Result<(), String> {
        if self.hd_mnemonic.is_some() {
            return Err(
                "wallet already has an HD seed; refusing to replace it (existing random-key \
                 wallets and seeded wallets keep their seed forever)"
                    .into(),
            );
        }
        if !crate::seed::check_mnemonic(mnemonic) {
            return Err("invalid mnemonic (unknown word or bad checksum)".into());
        }
        // Derivability proof before any write (their SetHDSeed derives
        // index 0 first and aborts on failure).
        let _ = crate::seed::derive_triangles(mnemonic, passphrase, 0, 0, 0)?;

        // Erasures (their WriteHDMnemonic erases the crypted variant first;
        // empty passphrase = EraseHDPassphrase removes BOTH passphrase rows)
        // and writes, applied in one transaction.
        let mut erase: Vec<Vec<u8>> = vec![encode_key_blob("hdcmnemonic", &[])];
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = vec![(
            encode_key_blob("hdmnemonic", &[]),
            encode_name_value(mnemonic), // CS + UTF-8: their std::string form
        )];
        if passphrase.is_empty() {
            erase.push(encode_key_blob("hdpassphrase", &[]));
            erase.push(encode_key_blob("hdcpassphrase", &[]));
        } else {
            rows.push((
                encode_key_blob("hdpassphrase", &[]),
                encode_name_value(passphrase),
            ));
        }
        rows.push((encode_key_blob("hdchain", &[]), encode_hdchain_value(0)));

        self.ensure_backup()?;
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| write_error("BEGIN IMMEDIATE", "set_hd_seed", e))?;
        for blob in &erase {
            if let Err(e) = self
                .conn
                .execute("DELETE FROM main WHERE key = ?1", rusqlite::params![blob])
            {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(write_error("DELETE", "set_hd_seed", e));
            }
        }
        for (k, v) in &rows {
            if let Err(e) = self.conn.execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                rusqlite::params![k, v],
            ) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(write_error("INSERT", "set_hd_seed", e));
            }
        }
        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("COMMIT", "set_hd_seed", e));
        }
        // Post-commit proof: the written rows byte-match and the erasures
        // really erased.
        self.verify_rows(&rows, "set_hd_seed")?;
        for blob in &erase {
            let gone: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![blob],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other.to_string()),
                })
                .map_err(|e| format!("set_hd_seed verify: {}", e))?;
            if gone.is_some() {
                return Err("set_hd_seed verify: erased HD row still present".into());
            }
        }

        // Reader verification: the whole file must parse with the seed
        // visible through the EXISTING read path.
        let w = read_wallet(&self.path)?;
        if w.hd_mnemonic.as_deref() != Some(mnemonic) {
            return Err("set_hd_seed verify: reader does not see the written mnemonic".into());
        }
        if w.hd_chain_index != Some(0) {
            return Err("set_hd_seed verify: reader sees hdchain != 0".into());
        }

        // In-memory state mirrors the legacy CWallet fields.
        self.hd_mnemonic = Some(mnemonic.to_string());
        self.hd_passphrase = if passphrase.is_empty() {
            None
        } else {
            Some(passphrase.to_string())
        };
        self.hd_chain_index = 0;
        self.log(format!(
            "set_hd_seed: {}-word mnemonic + passphrase={} hdchain=0 wallet={}",
            crate::seed::split_words(mnemonic).len(),
            !passphrase.is_empty(),
            self.path.display()
        ));
        Ok(())
    }

    /// Derive the private scalar for the wallet's NEXT HD address (m/44'/
    /// 2222'/0'/0/hd_chain_index) from the STORED mnemonic row — port of
    /// CWallet::DeriveHDKey: the row content is used BYTE-EXACT (the same
    /// string the legacy wallet would PBKDF2), no normalization.
    pub fn derive_hd_key(&self, index: u32) -> Result<[u8; 32], String> {
        let Some(m) = self.hd_mnemonic.as_deref() else {
            return Err("wallet has no HD seed".into());
        };
        crate::seed::derive_triangles(m, self.hd_passphrase.as_deref().unwrap_or(""), 0, 0, index)
    }

    /// Derive and persist the wallet's next HD key — port of the legacy
    /// GenerateNewKey HD branch: keymeta + key + name + `hdchain` (index+1)
    /// rows commit in ONE transaction (their atomic TxnBegin…TxnCommit) and
    /// the counter advances by exactly 1. Keys are compressed (fUsedHD
    /// forces fCompressed). An existing key row is refused (fOverwrite=false
    /// parity, same as add_key).
    pub fn generate_hd_key(&mut self, label: &str) -> Result<WalletKey, String> {
        let index = self.hd_chain_index;
        if index < 0 || index > u32::MAX as i64 {
            return Err(format!("hdchain index out of range: {}", index));
        }
        let secret = self.derive_hd_key(index as u32)?;

        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret)
            .map_err(|e| format!("generate_hd_key: bad secret: {}", e))?;
        let pubkey33 = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
        let der = encode_der_privkey(&secret, &pubkey33)?;
        let key_row = (encode_key_keyblob(&pubkey33), encode_key_value(&der));
        let now = now_unix();
        let meta_row = (encode_keymeta_keyblob(&pubkey33), encode_keymeta_value(now));
        let address = pubkey_to_address(&pubkey33);
        let name_row = (encode_legacy_name_keyblob(&address), encode_name_value(label));
        let next = index + 1;
        let chain_row = (encode_key_blob("hdchain", &[]), encode_hdchain_value(next));

        self.ensure_backup()?;
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| write_error("BEGIN IMMEDIATE", "generate_hd_key", e))?;
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
                format!("generate_hd_key: duplicate scan: {}", e)
            })?;
        if dup.is_some() {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(format!(
                "generate_hd_key: key {} already exists in {} — hdchain counter inconsistent?",
                address,
                self.path.display()
            ));
        }
        let rows = vec![meta_row, key_row, name_row, chain_row];
        for (k, v) in &rows {
            if let Err(e) = self.conn.execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                rusqlite::params![k, v],
            ) {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(write_error("INSERT", "generate_hd_key", e));
            }
        }
        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("COMMIT", "generate_hd_key", e));
        }
        let n_bytes: usize = rows.iter().map(|(k, v)| k.len() + v.len()).sum();
        self.verify_rows(&rows, "generate_hd_key")?;

        // Reader verification: key visible via the read path, counter = n+1.
        let w = read_wallet(&self.path)?;
        let seen = w
            .keys
            .iter()
            .find(|k| k.pubkey == pubkey33)
            .ok_or_else(|| "generate_hd_key verify: key not visible via the reader".to_string())?;
        if seen.secret != secret || seen.address != address {
            return Err("generate_hd_key verify: reader sees different material".into());
        }
        if w.hd_chain_index != Some(next) {
            return Err(format!(
                "generate_hd_key verify: reader sees hdchain {:?}, expected {}",
                w.hd_chain_index, next
            ));
        }

        self.hd_chain_index = next;
        self.log(format!(
            "generate_hd_key: {} index={} (m/44'/2222'/0'/0/{}) label=\"{}\" rows=keymeta,key,name,hdchain bytes={} wallet={}",
            address,
            index,
            index,
            label,
            n_bytes,
            self.path.display()
        ));
        Ok(WalletKey {
            pubkey: pubkey33.to_vec(),
            secret,
            compressed: true,
            address,
            created: Some(now),
        })
    }

    /// Raise the `hdchain` counter to at least `min_next` (idempotent — a
    /// re-run that finds the counter already ≥ min_next is a no-op). Seed
    /// migration inserts indices 0..9 via add_key, so the counter MUST jump
    /// past them or the next GenerateNewKey (qt or tri7) re-derives an
    /// existing key row.
    pub fn advance_hdchain_to(&mut self, min_next: i64) -> Result<(), String> {
        if min_next < 0 {
            return Err(format!("advance_hdchain_to: negative target {}", min_next));
        }
        if self.hd_chain_index >= min_next {
            return Ok(());
        }
        let row = (
            encode_key_blob("hdchain", &[]),
            encode_hdchain_value(min_next),
        );
        self.ensure_backup()?;
        self.write_rows(&[row], "advance_hdchain_to")?;
        let w = read_wallet(&self.path)?;
        match w.hd_chain_index {
            Some(v) if v >= min_next => {}
            other => {
                return Err(format!(
                    "advance_hdchain_to verify: reader sees {:?}, expected ≥ {}",
                    other, min_next
                ))
            }
        }
        self.hd_chain_index = min_next;
        self.log(format!(
            "advance_hdchain_to: hdchain={} wallet={}",
            min_next,
            self.path.display()
        ));
        Ok(())
    }

    // ==== legacy keypool (`pool` rows) — parity item KEYPOOLREFILL =========

    /// Fresh ordered pool census through THIS handle (targeted SQL; never
    /// stale, never a full-file key parse).
    pub fn pool_entries(&self) -> Result<Vec<KeyPoolEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT key, value FROM main
                 WHERE hex(substr(key,1,1))='04' AND hex(substr(key,2,4))='706F6F6C'",
            )
            .map_err(|e| format!("pool_entries select: {}", e))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("pool_entries query: {}", e))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| format!("pool_entries iterate: {}", e))? {
            let key: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            let value: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            // blob = CS(4) "pool" + 8-byte LE index → suffix at byte 5
            if key.len() < 13 {
                continue;
            }
            if let Some(entry) = parse_pool_row(&key[5..], &value) {
                out.push(entry);
            }
        }
        out.sort_by_key(|e| e.index);
        Ok(out)
    }

    /// Register one key as pooled: the single-row atomic `pool` write with
    /// post-commit verify. A pre-insert scan logs when the index already
    /// carried a pool row (INSERT OR REPLACE would overwrite it) — under
    /// normal operation that never happens (the counter never reuses an
    /// index), so seeing the log means someone wrote pool rows by hand.
    /// Returns the written byte size for logging.
    fn register_pool_key(
        &mut self,
        index: i64,
        created: i64,
        pubkey: &[u8],
    ) -> Result<usize, String> {
        let kb = encode_pool_keyblob(index);
        let existing: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![&kb],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| format!("register_pool_key scan: {}", e))?;
        if existing.is_some() {
            self.log(format!(
                "register_pool_key: pool index {} ALREADY had a row — overwriting (manual pool rows?)",
                index
            ));
        }
        let row = (kb, encode_pool_value(created, pubkey));
        let n = row.0.len() + row.1.len();
        self.write_rows(&[row], "register_pool_key")?;
        Ok(n)
    }

    /// Refill the keypool up to `target` entries by deriving NEW HD keys
    /// (m/44'/2222'/0'/0/n through generate_hd_key, empty label — the
    /// legacy default-account parity) and pooling each one right after.
    /// Never shrinks an already-full pool; a pool at/above target is a
    /// no-op. HD (seed) wallets only — imported random-key wallets cannot
    /// be safely topped up, and that is stated, not papered over.
    ///
    /// Ordering: generate_hd_key commits its own verified 4-row transaction
    /// (keymeta/key/name/hdchain), THEN the pool row commits separately. A
    /// crash in between leaves the key owned but un-pooled — harmless by
    /// construction (the phrase restores it; the next refill derives fresh
    /// keys from the counter, never reusing an index).
    pub fn refill_keypool(&mut self, target: usize) -> Result<KeyPoolRefill, String> {
        if self.hd_mnemonic.is_none() {
            return Err(format!(
                "refill_keypool: {} has no HD seed — tri7 refills only seed wallets \
                 (an imported random-key wallet cannot be topped up from a phrase)",
                self.path.display()
            ));
        }
        let mut report = KeyPoolRefill {
            derived: 0,
            poolsize: self.pool_entries()?.len(),
            oldest: None,
            next_index: self.hd_chain_index,
        };
        while report.poolsize < target {
            let key = self.generate_hd_key("")?;
            let created = key.created.unwrap_or_else(now_unix);
            let index = self.hd_chain_index.saturating_sub(1);
            self.register_pool_key(index, created, &key.pubkey)?;
            report.derived += 1;
            report.next_index = self.hd_chain_index;
            report.oldest = Some(report.oldest.map_or(created, |o| o.min(created)));
            // Recount from the FILE, never increment blindly: a register at
            // an index that already carried a pool row REPLACES it (the qt's
            // WritePool is the same BDB key), so the count may not grow by
            // one. Blind counting desyncs the end-of-refill reader
            // verification — found live on DNS2, whose imported pool indices
            // (21..121) ran up to the HD counter (121) and a refill at the
            // counter replaced an imported row instead of adding one.
            report.poolsize = self.pool_entries()?.len();
        }
        // One full reader verification at the end: every pooled key must be
        // visible to the plain reader with its pool row in place.
        let w = read_wallet(&self.path)?;
        if w.keypool.len() != report.poolsize {
            return Err(format!(
                "refill_keypool verify: reader sees {} pool rows, expected {}",
                w.keypool.len(),
                report.poolsize
            ));
        }
        self.log(format!(
            "refill_keypool: +{} keys (poolsize={}, hdchain={} — m/44'/2222'/0'/0/n, empty label) wallet={}",
            report.derived,
            report.poolsize,
            report.next_index,
            self.path.display()
        ));
        Ok(report)
    }

    /// Hand out the lowest-index pooled entry whose `key` row exists:
    /// delete its pool row and relabel its name row in ONE atomic
    /// transaction, then verify both. Pool rows with no backing key are
    /// skipped loudly (a legacy import artifact must never produce an
    /// address the wallet cannot spend from). Returns None when the pool is
    /// empty or every remaining entry is unusable.
    pub fn pop_keypool(&mut self, label: &str) -> Result<Option<KeyPoolEntry>, String> {
        let entries = self.pool_entries()?;
        let mut usable: Vec<KeyPoolEntry> = Vec::new();
        for e in entries {
            let kb = encode_key_keyblob(&e.pubkey);
            let present: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![&kb],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(|e| format!("pop_keypool key scan: {}", e))?;
            if present.is_some() {
                usable.push(e);
            } else {
                self.log(format!(
                    "pop_keypool: pool index {} ({}) has NO key row — skipping \
                     (legacy import artifact)",
                    e.index, e.address
                ));
            }
        }
        let Some(entry) = usable.into_iter().next() else {
            return Ok(None);
        };
        let pool_kb = encode_pool_keyblob(entry.index);
        let name_kb = encode_legacy_name_keyblob(&entry.address);
        let name_row = (name_kb.clone(), encode_name_value(label));
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| write_error("BEGIN IMMEDIATE", "pop_keypool", e))?;
        if let Err(e) = self.conn.execute(
            "DELETE FROM main WHERE key = ?1",
            rusqlite::params![&pool_kb],
        ) {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("DELETE pool row", "pop_keypool", e));
        }
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            rusqlite::params![&name_row.0, &name_row.1],
        ) {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("INSERT name row", "pop_keypool", e));
        }
        if let Err(e) = self.conn.execute_batch("COMMIT") {
            let _ = self.conn.execute_batch("ROLLBACK");
            return Err(write_error("COMMIT", "pop_keypool", e));
        }
        // Post-commit proof: pool row gone, name row byte-exact.
        let gone: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![&pool_kb],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| format!("pop_keypool verify: {}", e))?;
        if gone.is_some() {
            return Err("pop_keypool verify: pool row still present after COMMIT".into());
        }
        let back: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![&name_row.0],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|e| format!("pop_keypool verify: {}", e))?;
        if back.as_deref() != Some(name_row.1.as_slice()) {
            return Err("pop_keypool verify: name row not byte-exact after COMMIT".into());
        }
        self.log(format!(
            "pop_keypool: {} (index={}) handed out with label=\"{}\" — key row kept, pool row erased wallet={}",
            entry.address,
            entry.index,
            label,
            self.path.display()
        ));
        Ok(Some(entry))
    }

    /// Write the seed-migration completion marker: setting "seedmigrated" =
    /// int32 LE 1 (the live wallet's numeric-setting shape). Written ONLY
    /// after the sweep broadcast succeeds — a failed broadcast leaves the
    /// marker unset so the migration can be retried.
    pub fn mark_seed_migrated(&mut self) -> Result<(), String> {
        let row = (
            encode_setting_keyblob(SEED_MIGRATED_SETTING),
            encode_setting_value_i32(1),
        );
        self.ensure_backup()?;
        self.write_rows(&[row], "mark_seed_migrated")?;
        if !seed_migration_done(&self.path)? {
            return Err(
                "mark_seed_migrated verify: the reader does not see the marker".into(),
            );
        }
        self.seed_migrated = true;
        self.log(format!(
            "mark_seed_migrated: setting '{}'=1 wallet={}",
            SEED_MIGRATED_SETTING,
            self.path.display()
        ));
        Ok(())
    }

    /// Fresh-wallet identity rows (their CreateWalletDatabase + first-open
    /// writes): `version` (SER_DISK CLIENT_VERSION) and `minversion`
    /// (60000, the observed live value). Only valid on a NEW empty wallet.
    pub fn write_fresh_identity_rows(&mut self) -> Result<(), String> {
        let rows = vec![
            (
                encode_key_blob("version", &[]),
                encode_version_value(CLIENT_VERSION),
            ),
            (
                encode_key_blob("minversion", &[]),
                encode_minversion_value(60000),
            ),
        ];
        self.write_rows(&rows, "write_fresh_identity_rows")
    }

    /// `defaultkey` row = the wallet's default receive key (their
    /// WriteDefaultKey: CS + pubkey bytes).
    pub fn write_defaultkey(&mut self, pubkey33: &[u8]) -> Result<(), String> {
        if pubkey33.len() != 33 {
            return Err("write_defaultkey: expected a 33-byte compressed pubkey".into());
        }
        let row = (
            encode_key_blob("defaultkey", &[]),
            encode_defaultkey_value(pubkey33),
        );
        self.ensure_backup()?;
        self.write_rows(&[row], "write_defaultkey")?;
        let w = read_wallet(&self.path)?;
        if w.default_key.as_deref() != Some(pubkey33) {
            return Err("write_defaultkey verify: reader sees a different default key".into());
        }
        self.log(format!(
            "write_defaultkey: pubkey {}… wallet={}",
            hex_short(pubkey33),
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

/// Result of a seed-wallet create/restore: the derived keys in derivation
/// order (index 0..count-1) and the final `hdchain` counter.
#[derive(Debug)]
pub struct SeedWalletOutcome {
    pub keys: Vec<WalletKey>,
    pub hd_chain_index: i64,
    pub mnemonic: String,
}

/// Create a brand-new, qt-readable wallet.dat container — the exact
/// walletdb-sqlite.cpp fresh-file shape: `main(key BLOB PRIMARY KEY, value
/// BLOB NOT NULL)`, application_id 0x5452_6977 ("TRIw"), user_version 1.
/// Refuses to touch an existing file (seed wallets are never overwritten).
pub fn create_wallet_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "{}: refusing to overwrite an existing wallet file",
            path.display()
        ));
    }
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("create {}: {}", path.display(), e))?;
    conn.execute_batch(&format!(
        "PRAGMA application_id = {};
         PRAGMA user_version = {};
         CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);",
        WALLET_APP_ID, WALLET_USER_VERSION
    ))
    .map_err(|e| format!("create schema {}: {}", path.display(), e))?;
    conn.close()
        .map_err(|e| format!("close {}: {}", path.display(), e.1))?;
    Ok(())
}

/// Build a seed-derived wallet at `path`:
///   - restore semantics: validates + canonicalizes the supplied mnemonic
///     (whitespace-insensitive like the BIP39 standard; see seed.rs for the
///     exact legacy-divergence note), then writes it and derives `count`
///     keys into EXACT legacy records;
///   - works on a fresh file (created here) or an existing wallet that has
///     NO seed yet (their hdrestore-onto-existing-wallet semantics: existing
///     keys are preserved, the seed is added);
///   - refuses any wallet that already has a seed (seed replacement is
///     impossible by definition).
pub fn restore_wallet_from_seed(
    path: &Path,
    mnemonic: &str,
    passphrase: &str,
    count: usize,
) -> Result<SeedWalletOutcome, String> {
    if count == 0 || count > 10_000 {
        return Err(format!("key count out of range 1..=10000: {}", count));
    }
    let canonical = crate::seed::normalize_words(mnemonic)
        .ok_or_else(|| "mnemonic is empty".to_string())?;
    if !crate::seed::check_mnemonic(&canonical) {
        return Err("invalid mnemonic (unknown word or bad checksum)".into());
    }
    let path_existed = path.exists();
    if !path_existed {
        create_wallet_file(path)?;
    }
    let mut h = WalletHandle::open_readwrite(path)?;
    if h.hd_mnemonic.is_some() {
        return Err(format!(
            "{}: wallet already has an HD seed; refusing to replace it",
            path.display()
        ));
    }
    let fresh = !path_existed;
    if fresh {
        // first-open rows the legacy qt writes on wallet creation
        h.write_fresh_identity_rows()?;
    }
    h.set_hd_seed(&canonical, passphrase)?;
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        keys.push(h.generate_hd_key("")?);
    }
    h.write_defaultkey(&keys[0].pubkey)?;
    let outcome = SeedWalletOutcome {
        keys,
        hd_chain_index: h.hd_chain_index,
        mnemonic: canonical,
    };
    h.close();
    Ok(outcome)
}

/// Create a brand-new seed wallet at a NEW path with a freshly generated
/// 24-word mnemonic (the GUI "Create new wallet" flow). The caller owns the
/// returned mnemonic — show it once, verify, never persist it anywhere else.
pub fn create_wallet_from_seed(
    path: &Path,
    passphrase: &str,
    count: usize,
) -> Result<SeedWalletOutcome, String> {
    let mnemonic = crate::seed::generate_mnemonic_24()?;
    let out = restore_wallet_from_seed(path, &mnemonic, passphrase, count)?;
    Ok(out)
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

    /// The label row is written in the ORIGINAL client's byte layout and the
    /// reader accepts BOTH that layout and the double-prefixed one earlier
    /// tri7 builds produced (a wallet holding both must not lose labels).
    #[test]
    fn name_rows_use_legacy_bytes_and_reader_accepts_both_layouts() {
        let addr = "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY";
        let legacy = encode_legacy_name_keyblob(addr);
        // record key prefix is CS(4)+"name"
        assert_eq!(&legacy[..5], &[0x04, b'n', b'a', b'm', b'e']);
        // then EXACTLY one length byte (34), then the address itself
        let body = &legacy[5..];
        assert_eq!(body[0] as usize, addr.len());
        assert_eq!(&body[1..1 + addr.len()], addr.as_bytes());
        assert_eq!(body.len(), 1 + addr.len(), "one prefix, not two");

        // By 2026-11 the two helpers emit the SAME bytes; the reader keeps
        // accepting an extra prefix as well, so wallets written by either
        // generation still yield their labels.
        let old_double = encode_name_keyblob(addr);
        assert_eq!(old_double, legacy);
        // on-disk doubled shape: 0x22 then a serialized std::string ('"T…')
        let mut doubled = vec![0x04u8, b'n', b'a', b'm', b'e', addr.len() as u8];
        doubled.push(addr.len() as u8);
        doubled.extend_from_slice(addr.as_bytes());
        let (t2, suffix2) = parse_key_blob(&doubled).unwrap();
        assert_eq!(t2, "name");
        assert_eq!(
            String::from_utf8_lossy(&strip_name_suffix(&suffix2)),
            addr,
            "doubled={:?} suffix={:?}",
            doubled,
            suffix2
        );

        for k in [legacy, old_double] {
            let (t, suffix) = parse_key_blob(&k).unwrap();
            assert_eq!(t, "name");
            assert_eq!(String::from_utf8_lossy(&strip_name_suffix(&suffix)), addr);
        }
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

    // ---- seed-migration marker + hdchain advance ----------------------------

    #[test]
    fn setting_row_encoders_match_the_live_legacy_shape() {
        // byte-shape copied from the live wallet's observed row:
        // 07 "setting" 0C "addrIncoming" | 84 57 3D 00
        let kb = encode_setting_keyblob("addrIncoming");
        let mut want = Vec::new();
        want.push(0x07);
        want.extend_from_slice(b"setting");
        want.push(0x0C);
        want.extend_from_slice(b"addrIncoming");
        assert_eq!(kb, want);
        // numeric value = RAW int32 LE (no CompactSize wrapper);
        // live bytes 84 57 3D 00 = 4_020_100 little-endian
        assert_eq!(encode_setting_value_i32(4_020_100), [0x84, 0x57, 0x3D, 0x00]);
        // the marker roundtrips through the targeted reader
        let dir = test_dir("setting_row");
        let path = synth_wallet(&dir, "setting_row");
        assert!(!seed_migration_done(&path).unwrap());
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        assert!(!h.seed_migrated);
        h.mark_seed_migrated().unwrap();
        assert!(h.seed_migrated);
        assert!(seed_migration_done(&path).unwrap());
        // idempotent re-mark is a no-op success
        h.mark_seed_migrated().unwrap();
        // the whole wallet still parses; the key reader surface is unchanged
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1);
        // other settings rows on the file are visible through read_settings
        let row = (
            encode_setting_keyblob("addrIncoming"),
            encode_setting_value_i32(4_020_100),
        );
        h.write_rows(&[row], "test_setting").unwrap();
        let settings = read_settings(&path).unwrap();
        let names: Vec<&str> = settings.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"seedmigrated"));
        assert!(names.contains(&"addrIncoming"));
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn advance_hdchain_to_is_an_idempotent_raise() {
        let dir = test_dir("hdchain_raise");
        let path = synth_wallet(&dir, "hdchain_raise");
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        assert_eq!(h.hd_chain_index, 0);
        h.advance_hdchain_to(10).unwrap();
        assert_eq!(h.hd_chain_index, 10);
        assert_eq!(read_wallet(&path).unwrap().hd_chain_index, Some(10));
        // raising to a LOWER target is a no-op (monotonic)
        h.advance_hdchain_to(3).unwrap();
        assert_eq!(h.hd_chain_index, 10);
        assert_eq!(read_wallet(&path).unwrap().hd_chain_index, Some(10));
        // raising to the same target: no-op
        h.advance_hdchain_to(10).unwrap();
        assert_eq!(h.hd_chain_index, 10);
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- legacy keypool (`pool` rows) — parity item KEYPOOLREFILL --------

    fn kp_seed_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tri7wallet_kp_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    const KP_MN12: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// Byte ground truth: one of the live wallet's 116 imported legacy pool
    /// rows (DNS2 wallet.dat, read 2026-11-07). The reader must decode it
    /// and the writer must re-encode it byte-exactly — that pair of proofs
    /// is what makes tri7-written pool rows loadable by the qt and
    /// qt-written rows loadable by tri7.
    #[test]
    fn pool_row_bytes_decode_live_ground_truth() {
        let key = crate::p2p::hex_decode("04706F6F6C1500000000000000").unwrap();
        let value = crate::p2p::hex_decode(
            "74B14D001F1A326A00000000210255CC3FAE56684BE18B63B1E02FD9BBBFF16569\
             133103E8C066A02184DD38DC7B",
        )
        .unwrap();
        assert_eq!(key.len(), 13); // 04 "pool" + int64 LE index
        assert_eq!(value.len(), 46); // i32 version + i64 time + CS(33) + pubkey
        let entry = parse_pool_row(&key[5..], &value).expect("live row parses");
        assert_eq!(entry.index, 21);
        assert_eq!(entry.created, 0x6A32_1A1F);
        assert_eq!(entry.pubkey.len(), 33);
        assert!(entry.address.starts_with('T'), "{}", entry.address);
        assert_eq!(entry.address, pubkey_to_address(&entry.pubkey));
        // re-encode is byte-exact
        assert_eq!(encode_pool_keyblob(entry.index), key);
        assert_eq!(encode_pool_value(entry.created, &entry.pubkey), value);
        // malformed shapes are skipped (None), never a panic
        assert!(parse_pool_row(&key[..4], &value).is_none(), "short index");
        assert!(parse_pool_row(&key[5..], &value[..12]).is_none(), "truncated value");
        let mut bad_len = value.clone();
        bad_len[12] = 0x40; // CS claims 64 bytes, only 33 present
        assert!(parse_pool_row(&key[5..], &bad_len).is_none(), "pubkey overruns");
    }

    /// Full keypool lifecycle on a seeded wallet: refill derives fresh
    /// phrase-restorable keys and pools them; refills at/above target are
    /// no-ops; pop hands out lowest-index-first, relabels, keeps the key
    /// row and erases the pool row; the drained pool answers None; the HD
    /// counter stays coherent through it all.
    #[test]
    fn keypool_refill_pop_and_census_lifecycle() {
        let dir = kp_seed_dir("keypool");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, KP_MN12, "", 2).unwrap();
        let w0 = read_wallet(&path).unwrap();
        assert!(w0.keypool.is_empty(), "fresh seed wallet: no pool rows");
        assert_eq!(keypool_stats(&path).unwrap(), (0, None));

        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        assert_eq!(wh.hd_chain_index, 2);
        let rep = wh.refill_keypool(5).unwrap();
        assert_eq!(rep.derived, 5);
        assert_eq!(rep.poolsize, 5);
        assert_eq!(wh.hd_chain_index, 7);
        let entries = wh.pool_entries().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].index, 2, "pool starts at the free counter");
        assert_eq!(entries[4].index, 6);
        assert!(
            entries.windows(2).all(|w| w[0].index < w[1].index),
            "pool serves lowest-index-first"
        );
        wh.close();

        let w1 = read_wallet(&path).unwrap();
        assert_eq!(w1.keypool.len(), 5);
        assert_eq!(w1.hd_chain_index, Some(7));
        assert_eq!(w1.keys.len(), 7);
        let (n, oldest) = keypool_stats(&path).unwrap();
        assert_eq!(n, 5);
        assert_eq!(oldest, Some(entries.iter().map(|e| e.created).min().unwrap()));
        // every pooled key is the PURE phrase derivation at its index and
        // carries its own key row (restorable AND spendable)
        for e in &entries {
            let expected =
                crate::seed::derive_triangles(KP_MN12, "", 0, 0, e.index as u32).unwrap();
            let k = w1
                .keys
                .iter()
                .find(|k| k.pubkey == e.pubkey)
                .expect("pooled key row present");
            assert_eq!(k.secret, expected, "index {}", e.index);
            assert_eq!(k.address, e.address);
        }

        // a refill at/above target is a no-op — never shrinks, never re-derives
        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        let rep2 = wh.refill_keypool(5).unwrap();
        assert_eq!(rep2.derived, 0);
        let rep3 = wh.refill_keypool(4).unwrap();
        assert_eq!(rep3.derived, 0);
        assert_eq!(wh.pool_entries().unwrap().len(), 5);

        // pop: lowest index first, relabels, keeps the key row, erases the
        // pool row
        let popped = wh.pop_keypool("operator").unwrap().expect("pool has entries");
        assert_eq!(popped.index, entries[0].index);
        wh.close();
        let w2 = read_wallet(&path).unwrap();
        assert_eq!(w2.keypool.len(), 4);
        assert!(!w2.keypool.iter().any(|e| e.index == popped.index));
        let k = w2
            .keys
            .iter()
            .find(|k| k.pubkey == popped.pubkey)
            .expect("key row kept after pop");
        assert_eq!(k.address, popped.address);
        assert!(
            w2.names
                .iter()
                .any(|(a, l)| a == &popped.address && l == "operator"),
            "pop relabels the handed-out key"
        );

        // drain the rest, then pop answers None; the HD counter continues
        // from where the refills left it (no re-derivation of consumed
        // indexes — those keys stay owned, just out of the pool)
        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        for _ in 0..4 {
            wh.pop_keypool("drain").unwrap().expect("entry");
        }
        assert!(wh.pop_keypool("x").unwrap().is_none());
        let k = wh.generate_hd_key("post").unwrap();
        let expected = crate::seed::derive_triangles(KP_MN12, "", 0, 0, 7).unwrap();
        assert_eq!(k.secret, expected);
        wh.close();
        assert_eq!(keypool_stats(&path).unwrap().0, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pool row with no backing key row (the shape a legacy import can
    /// leave behind) must be skipped loudly, never handed out — an address
    /// the wallet cannot spend from is worse than a live derivation.
    #[test]
    fn pop_skips_pool_rows_without_key_rows() {
        let dir = kp_seed_dir("keypool_skip");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, KP_MN12, "", 1).unwrap();
        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        wh.refill_keypool(4).unwrap(); // indices 1, 2, 3 (0 is the seed key)
        wh.close();
        // inject the artifact directly: pool index 0, no key row
        let conn = rusqlite::Connection::open(&path).unwrap();
        let bogus_pub = [0xABu8; 33];
        conn.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                encode_pool_keyblob(0),
                encode_pool_value(1_234_567, &bogus_pub)
            ],
        )
        .unwrap();
        drop(conn);

        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        let popped = wh
            .pop_keypool("x")
            .unwrap()
            .expect("skips the artifact, serves the lowest USABLE entry");
        assert_eq!(popped.index, 1, "index 0 had no key row — skipped");
        // repeated pops keep skipping; only-artifacts-left answers None
        let p2 = wh.pop_keypool("y").unwrap().unwrap();
        assert_eq!(p2.index, 2);
        let p3 = wh.pop_keypool("w").unwrap().unwrap();
        assert_eq!(p3.index, 3);
        let p4 = wh.pop_keypool("v").unwrap().unwrap();
        assert_eq!(p4.index, 4);
        assert!(wh.pop_keypool("z").unwrap().is_none());
        wh.close();
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keypool.len(), 1, "the artifact row is never consumed");
        assert_eq!(w.keypool[0].index, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Imported random-key wallets have no seed to derive from — the refill
    /// refuses honestly instead of inventing keys the phrase won't restore.
    #[test]
    fn refill_refuses_non_hd_wallet() {
        let dir = test_dir("refill_nohd");
        let path = synth_wallet(&dir, "refill_nohd");
        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        let err = wh.refill_keypool(5).unwrap_err();
        assert!(err.contains("no HD seed"), "{}", err);
        wh.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LIVE-FAILURE REGRESSION (DNS2 2026-11-07): an imported pool can carry
    /// rows up to the HD counter, so a refill register REPLACES an imported
    /// row instead of adding one — the loop must recount from the file or
    /// the end-of-refill reader verification desyncs ("reader sees 102,
    /// expected 103"). Pins the exact shape: pool row at the counter index,
    /// replaced by the derived key's row, count grows by 0 there.
    #[test]
    fn refill_recounts_when_register_replaces_an_imported_row() {
        let dir = kp_seed_dir("keypool_replace");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, KP_MN12, "", 2).unwrap(); // counter 2
        // inject the legacy artifact AT the counter: pool index 2, a pubkey
        // with no key row (the imported-set shape DNS2 had)
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                encode_pool_keyblob(2),
                encode_pool_value(1_700_000_000, &[0xCDu8; 33])
            ],
        )
        .unwrap();
        drop(conn);
        assert_eq!(keypool_stats(&path).unwrap().0, 1);

        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        let rep = wh.refill_keypool(4).unwrap();
        // index 2's register REPLACED the artifact (count stayed 1 there),
        // so reaching 4 took FOUR derivations (2,3,4,5), not three
        assert_eq!(rep.derived, 4);
        assert_eq!(rep.poolsize, 4);
        assert_eq!(wh.hd_chain_index, 6);
        wh.close();
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keypool.len(), 4);
        assert!(
            !w.keypool.iter().any(|e| e.pubkey == [0xCDu8; 33]),
            "the replaced artifact row is gone"
        );
        assert!(
            w.keypool.iter().all(|e| e.index >= 2),
            "indices 2..5, counter-coherent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Redeem scripts (P2SH multisig) round-trip through the wallet file as
    /// the legacy `script` rows: 06 "script" <20-byte scriptID> | CS + script.
    #[test]
    fn redeem_script_rows_roundtrip_and_are_idempotent() {
        let dir = test_dir("redeem");
        let path = synth_wallet(&dir, "redeem");
        let pks: Vec<Vec<u8>> = ["rw/m1", "rw/m2", "rw/m3"]
            .iter()
            .map(|l| synth_key(l).1)
            .collect();
        let redeem = crate::multisig::redeem_script(2, &pks).unwrap();
        let id = crate::address::hash160(&redeem);

        let mut wh = WalletHandle::open_readwrite(&path).unwrap();
        // the encoders produce the exact legacy row shape
        let kb = encode_script_keyblob(&id);
        assert_eq!(&kb[..7], b"\x06script");
        assert_eq!(kb.len(), 27);
        let val = encode_script_value(&redeem);
        assert_eq!(val[0] as usize, redeem.len());
        assert_eq!(&val[1..], &redeem);

        // store → read back → list
        assert_eq!(wh.put_redeem_script(&redeem).unwrap(), id);
        assert_eq!(
            wh.get_redeem_script(&id).unwrap().as_deref(),
            Some(&redeem[..])
        );
        let listed = wh.list_redeem_scripts().unwrap();
        assert_eq!(listed, vec![(id, redeem.clone())]);
        // idempotent: storing the same script again changes nothing
        assert_eq!(wh.put_redeem_script(&redeem).unwrap(), id);
        assert_eq!(wh.list_redeem_scripts().unwrap().len(), 1);
        // unknown scriptID → None
        assert!(wh.get_redeem_script(&[0x55u8; 20]).unwrap().is_none());
        wh.close();

        // the row really is in the file with the exact legacy bytes
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let back: Vec<u8> = conn
            .query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![kb],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(back, val);
        // and the whole file still reads cleanly through the standard reader
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1, "pre-existing key untouched");
    }
}

// ===========================================================================
// HD seed-wallet tests (src/seed.rs port of legacy hdwallet.cpp): record
// formats, interchangeability, cross-machine roundtrip, refusals.
// ===========================================================================
#[cfg(test)]
mod seed_tests {
    use super::*;
    use std::path::PathBuf;


    /// Minimal legacy (random-key, no seed) wallet for cross checks — local
    /// copy of the main tests' fixture shape (mod-private there).
    fn synth_legacy_wallet(dir: &Path, tag: &str) -> PathBuf {
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
        let secret = crate::legacy::sha256d(b"tri7-wallet-rw/seed");
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        let pk = pk.serialize();
        let der = encode_der_privkey(&secret, &pk).unwrap();
        let mut vrow = Vec::new();
        push_compact_size(&mut vrow, 7);
        vrow.extend_from_slice(b"version");
        conn.execute(
            "INSERT INTO main (key, value) VALUES (?1, ?2)",
            rusqlite::params![vrow, CLIENT_VERSION.to_le_bytes()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO main (key, value) VALUES (?1, ?2)",
            rusqlite::params![encode_key_keyblob(&pk), encode_key_value(&der)],
        )
        .unwrap();
        conn.close().unwrap();
        path
    }

    fn seed_test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tri7wallet_seed_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The canonical Trezor test phrase (12 words) and the official 24-word
    /// all-zero-entropy phrase; their derived keys are oracle-pinned in
    /// seed.rs against the legacy client's compiled hdwallet.cpp.
    const MN12: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MN24: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    #[test]
    fn create_then_reread_matches_legacy_derivation() {
        let dir = seed_test_dir("create");
        let path = dir.join("wallet.dat");
        let out = create_wallet_from_seed(&path, "", 5).unwrap();
        assert_eq!(out.keys.len(), 5);
        assert_eq!(out.hd_chain_index, 5);
        let words = out.mnemonic.split_whitespace().count();
        assert_eq!(words, 24);
        assert!(crate::seed::check_mnemonic(&out.mnemonic));

        // every derived key must equal the pure derivation at its index
        for (i, k) in out.keys.iter().enumerate() {
            let expected = crate::seed::derive_triangles(&out.mnemonic, "", 0, 0, i as u32).unwrap();
            assert_eq!(k.secret, expected, "index {}", i);
        }

        // re-read: HD fields + key census via the EXISTING reader
        let w = read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 5);
        assert_eq!(w.hd_mnemonic.as_deref(), Some(out.mnemonic.as_str()));
        assert_eq!(w.hd_passphrase, None);
        assert_eq!(w.hd_chain_index, Some(5));
        assert_eq!(w.default_key.as_deref(), Some(out.keys[0].pubkey.as_slice()));
    }

    #[test]
    fn hd_record_bytes_match_legacy_serialization() {
        // Byte-level assertions on the exact legacy record shapes written
        // into a seeded wallet (walletdb.h WriteHDMnemonic/WriteHDChain/
        // WriteDefaultKey serializations).
        let dir = seed_test_dir("bytes");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, MN12, "", 2).unwrap();

        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let get = |k: Vec<u8>| -> Vec<u8> {
            conn.query_row(
                "SELECT value FROM main WHERE key = ?1",
                rusqlite::params![k],
                |r| r.get(0),
            )
            .unwrap()
        };

        // hdmnemonic: key blob = 0A "hdmnemonic", value = CS + mnemonic
        let mut kb = vec![0x0a];
        kb.extend_from_slice(b"hdmnemonic");
        let hv = get(kb);
        assert_eq!(hv[0], MN12.len() as u8); // CS(len)
        assert_eq!(&hv[1..], MN12.as_bytes());

        // hdchain: key blob = 07 "hdchain", value = int64 LE
        let mut kb = vec![0x07];
        kb.extend_from_slice(b"hdchain");
        let cv = get(kb);
        assert_eq!(cv, 2i64.to_le_bytes());

        // defaultkey: key blob = 0A "defaultkey", value = CS(33) + pubkey
        let mut kb = vec![0x0a];
        kb.extend_from_slice(b"defaultkey");
        let dv = get(kb);
        assert_eq!(dv[0], 33);
        assert_eq!(dv.len(), 34);

        // version + minversion: raw int32 LE
        let mut kb = vec![0x07];
        kb.extend_from_slice(b"version");
        assert_eq!(get(kb), CLIENT_VERSION.to_le_bytes());
        let mut kb = vec![0x0a];
        kb.extend_from_slice(b"minversion");
        assert_eq!(get(kb), 60000i32.to_le_bytes());

        // no hdpassphrase / hdcmnemonic / hdcpassphrase rows exist
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM main WHERE hex(substr(key,2,11))='686463706173737072617365'
                 OR hex(substr(key,2,11))='6864636D6E656D6F6E6963'
                 OR hex(substr(key,2,13))='686463706173737072617365'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn key_row_is_exact_legacy_214b_der_shape() {
        // The seed-derived key rows must be byte-identical in SHAPE to the
        // grade-A-verified add_key rows (214-byte DER from the qt template).
        let dir = seed_test_dir("dershape");
        let path = dir.join("wallet.dat");
        let out = restore_wallet_from_seed(&path, MN12, "", 3).unwrap();
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for k in &out.keys {
            let mut kb = vec![0x03];
            kb.extend_from_slice(b"key");
            kb.push(33);
            kb.extend_from_slice(&k.pubkey);
            let v: Vec<u8> = conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![kb],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(v[0], 214); // CS(214)
            let der = &v[1..];
            assert_eq!(der.len(), 214);
            assert_eq!(&der[0..8], &DER_HEADER);
            assert_eq!(&der[8..40], &k.secret);
            assert_eq!(&der[181..214], k.pubkey.as_slice());
            // keymeta present with the same pubkey
            let mut mb = vec![0x07];
            mb.extend_from_slice(b"keymeta");
            mb.push(33);
            mb.extend_from_slice(&k.pubkey);
            let mv: Vec<u8> = conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![mb],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(mv.len(), 12); // int32 version + int64 time
            // name row present (empty label)
            let mut nb = vec![0x04];
            nb.extend_from_slice(b"name");
            nb.push(k.address.len() as u8);
            nb.extend_from_slice(k.address.as_bytes());
            let nv: Vec<u8> = conn
                .query_row(
                    "SELECT value FROM main WHERE key = ?1",
                    rusqlite::params![nb],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(nv, vec![0u8]); // CS(0) = empty label
        }
    }

    #[test]
    fn restore_on_fresh_wallet_reproduces_identical_keys() {
        // CROSS-MACHINE roundtrip proof: wallet A created from the phrase;
        // wallet B is a DIFFERENT FILE on a DIFFERENT PATH restored from the
        // same typed phrase (with messy whitespace, like a real paste).
        let dir = seed_test_dir("roundtrip");
        let a = dir.join("machine_a_wallet.dat");
        let b = dir.join("other").join("machine_b_wallet.dat");
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();

        let created = restore_wallet_from_seed(&a, MN24, "", 10).unwrap();
        let restored = restore_wallet_from_seed(&b, &format!("  {}\n", MN24), "", 10).unwrap();

        assert_eq!(created.keys.len(), restored.keys.len());
        for (ka, kb) in created.keys.iter().zip(restored.keys.iter()) {
            assert_eq!(ka.secret, kb.secret);
            assert_eq!(ka.address, kb.address);
            assert_eq!(ka.pubkey, kb.pubkey);
        }
        // and both match the pure derivation
        for i in 0usize..10 {
            let expected =
                crate::seed::derive_triangles(MN24, "", 0, 0, i as u32).unwrap();
            assert_eq!(created.keys[i].secret, expected);
        }
    }

    #[test]
    fn tri7_wallet_is_readable_by_legacy_record_shapes() {
        // The rows the ORIGINAL qt wallet reads (a "key" row keyed by
        // CS+pubkey, "keymeta" keyed likewise) — prove tri7's seeded wallet
        // contains exactly the row TYPES the qt's LoadWallet dispatches on.
        let dir = seed_test_dir("qtcensus");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, MN12, "", 4).unwrap();
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut stmt = conn.prepare("SELECT key FROM main").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut types = std::collections::BTreeMap::new();
        while let Some(r) = rows.next().unwrap() {
            let key: Vec<u8> = r.get(0).unwrap();
            let tlen = key[0] as usize;
            let t = String::from_utf8(key[1..1 + tlen].to_vec()).unwrap();
            *types.entry(t).or_insert(0) += 1;
        }
        // version, minversion, hdmnemonic, hdchain, defaultkey = 1 each;
        // 4 × {key, keymeta, name}
        assert_eq!(types.get("version"), Some(&1));
        assert_eq!(types.get("minversion"), Some(&1));
        assert_eq!(types.get("hdmnemonic"), Some(&1));
        assert_eq!(types.get("hdchain"), Some(&1));
        assert_eq!(types.get("defaultkey"), Some(&1));
        assert_eq!(types.get("key"), Some(&4));
        assert_eq!(types.get("keymeta"), Some(&4));
        assert_eq!(types.get("name"), Some(&4));
        assert_eq!(types.len(), 8);
    }

    #[test]
    fn restore_into_existing_random_key_wallet_preserves_keys() {
        // their hdrestore-onto-existing semantics: an OLD random-key wallet
        // gains a seed; its existing keys stay; new HD keys derive from the
        // phrase; hdchain counts only HD keys.
        let dir = seed_test_dir("existing");
        let path = synth_legacy_wallet(&dir, "legacy");
        let before = read_wallet(&path).unwrap();
        assert_eq!(before.keys.len(), 1);
        assert!(before.hd_mnemonic.is_none());

        let out = restore_wallet_from_seed(&path, MN12, "pass phrase", 3).unwrap();
        let after = read_wallet(&path).unwrap();
        assert_eq!(after.keys.len(), 4); // 1 random + 3 HD
        assert_eq!(after.keys[0].secret, before.keys[0].secret); // preserved
        assert_eq!(after.hd_mnemonic.as_deref(), Some(MN12));
        assert_eq!(after.hd_passphrase.as_deref(), Some("pass phrase"));
        assert_eq!(after.hd_chain_index, Some(3));
        // the 3 new keys are the HD ones (indices 0..2, WITH passphrase)
        for i in 0usize..3 {
            let expected =
                crate::seed::derive_triangles(MN12, "pass phrase", 0, 0, i as u32).unwrap();
            assert_eq!(out.keys[i].secret, expected);
        }
    }

    #[test]
    fn seed_replacement_and_second_seed_are_refused() {
        let dir = seed_test_dir("refuse");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, MN12, "", 1).unwrap();

        let err = restore_wallet_from_seed(&path, MN24, "", 1).unwrap_err();
        assert!(err.contains("already has an HD seed"), "{}", err);

        // even the same phrase again is refused (a seed is write-once)
        let err = restore_wallet_from_seed(&path, MN12, "", 1).unwrap_err();
        assert!(err.contains("already has an HD seed"), "{}", err);
    }

    #[test]
    fn bad_mnemonics_never_touch_the_file() {
        let dir = seed_test_dir("badmn");
        let path = dir.join("wallet.dat");
        // wrong checksum
        let err = restore_wallet_from_seed(&path, &MN12.replace("about", "abandon"), "", 1)
            .unwrap_err();
        assert!(err.contains("invalid mnemonic"), "{}", err);
        // unknown word
        let err =
            restore_wallet_from_seed(&path, &format!("{} zzzz", MN12), "", 1).unwrap_err();
        assert!(err.contains("invalid mnemonic"), "{}", err);
        // absurd count
        let err = restore_wallet_from_seed(&path, MN12, "", 0).unwrap_err();
        assert!(err.contains("out of range"), "{}", err);
        assert!(!path.exists(), "file must not be created on rejection");
    }

    #[test]
    fn generate_hd_key_continues_counter_and_refuses_duplicates() {
        let dir = seed_test_dir("continue");
        let path = dir.join("wallet.dat");
        restore_wallet_from_seed(&path, MN12, "", 2).unwrap();

        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        assert_eq!(h.hd_chain_index, 2);
        let k2 = h.generate_hd_key("").unwrap();
        let expected = crate::seed::derive_triangles(MN12, "", 0, 0, 2).unwrap();
        assert_eq!(k2.secret, expected);
        assert_eq!(h.hd_chain_index, 3);
        h.close();

        // re-opened handle continues from the row (cross-session proof)
        let mut h = WalletHandle::open_readwrite(&path).unwrap();
        assert_eq!(h.hd_chain_index, 3);
        let k3 = h.generate_hd_key("next").unwrap();
        assert_eq!(
            k3.secret,
            crate::seed::derive_triangles(MN12, "", 0, 0, 3).unwrap()
        );
        // wallets WITHOUT a seed refuse HD generation
        let old = synth_legacy_wallet(&dir, "noseed");
        let mut h2 = WalletHandle::open_readwrite(&old).unwrap();
        assert!(h2.hd_mnemonic.is_none());
        assert!(h2.generate_hd_key("").is_err());
    }

    #[test]
    fn create_wallet_file_refuses_existing() {
        let dir = seed_test_dir("refusecreate");
        let path = synth_legacy_wallet(&dir, "x");
        assert!(create_wallet_file(&path).is_err());
        let fresh = dir.join("fresh.dat");
        create_wallet_file(&fresh).unwrap();
        // container validates as a TRI wallet via the standard reader
        let w = read_wallet(&fresh).unwrap();
        assert!(w.keys.is_empty());
        assert!(w.hd_mnemonic.is_none());
    }

    #[test]
    fn passphrase_wallet_restores_identically_across_files() {
        let dir = seed_test_dir("passphrase");
        let a = dir.join("a.dat");
        let b = dir.join("b.dat");
        let created = restore_wallet_from_seed(&a, MN24, "tri7 25th word", 3).unwrap();
        let restored =
            restore_wallet_from_seed(&b, MN24, "tri7 25th word", 3).unwrap();
        for (ka, kb) in created.keys.iter().zip(restored.keys.iter()) {
            assert_eq!(ka.address, kb.address);
        }
        // and they differ from the no-passphrase derivation (passphrase is
        // load-bearing for restoration)
        let nopass = restore_wallet_from_seed(&dir.join("c.dat"), MN24, "", 1).unwrap();
        assert_ne!(created.keys[0].secret, nopass.keys[0].secret);
    }
}
