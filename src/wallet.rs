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
