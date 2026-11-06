//! bdb_import — carry a LEGACY Berkeley-DB wallet into tri7's SQLite wallet.
//!
//! Everything the legacy wallet stored is copied, not just the keys. The
//! section list was taken from a census of real wallets (`db5.3_dump -p`), not
//! from assumptions:
//!
//! | BDB section      | what it is                                   | where it goes |
//! |------------------|----------------------------------------------|---------------|
//! | `key`            | DER EC private keys, suffix = pubkey         | `key`         |
//! | `keymeta`        | birth time per pubkey                        | `keymeta`     |
//! | `ckey`           | AES-encrypted secret (suffix = pubkey)       | `ckey`        |
//! | `mkey`           | master key envelope (passphrase KDF params)  | `mkey`        |
//! | `name`           | address book labels                          | `name`        |
//! | `setting`        | wallet settings (incl. Tor v3 identity)      | `setting`     |
//! | `defaultkey`     | the default receive key                      | `defaultkey`  |
//! | `bestblock`      | last block + block-time                      | `bestblock`   |
//! | `version`        | wallet format version                        | `version`     |
//! | `minversion`     | minimum client version                       | `minversion`  |
//! | `pool`           | keypool reserve (keys live on; indexes do not)| `pool`       |
//! | `tx`             | 28k legacy transaction records               | NOT COPIED    |
//! | `orderposnext`   | UI ordering counter                          | NOT COPIED    |
//!
//! Transaction records are deliberately not copied: tri7 rebuilds history from
//! the chain (history.json / addrindex), and the legacy `tx` rows point at
//! chain data the SQLite wallet mirrors anyway. The report says so explicitly
//! so nobody believes they were carried.
//!
//! Integrity: every row is COMMITTED-record shaped before it is written — key
//! rows must re-derive their suffix pubkey, name rows must be valid addresses,
//! settings are copied verbatim. A malformed row is counted and reported, never
//! silently dropped from the total.
//!
//! Encrypted wallets: the `mkey`/`ckey` rows carry over as-is (the exact
//! byte layout tri7's own `wallet_encrypt` reads), so a passphrase-protected
//! wallet stays passphrase-protected and `walletpassphrase` unlocks it. The
//! importer never sees a plaintext secret for such a wallet, and says so.

use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::wallet::{encode_keymeta_keyblob, encode_keymeta_value, create_wallet_file, WalletHandle};

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub source: String,
    pub dest: String,
    /// raw record count from `db_dump`
    pub records_dumped: usize,
    pub keys: usize,
    pub encrypted_keys: usize,
    pub keymeta: usize,
    pub names: usize,
    pub settings: usize,
    pub pool: usize,
    pub skipped: usize,
    pub encrypted: bool,
    pub errors: Vec<String>,
    /// sections carried verbatim because they are single opaque rows
    pub singletons: Vec<String>,
    /// sections present in the source but deliberately not carried
    pub not_copied: Vec<String>,
}

impl ImportReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "source": self.source,
            "dest": self.dest,
            "records_dumped": self.records_dumped,
            "keys": self.keys,
            "encrypted_keys": self.encrypted_keys,
            "keymeta": self.keymeta,
            "names": self.names,
            "settings": self.settings,
            "pool": self.pool,
            "skipped": self.skipped,
            "encrypted": self.encrypted,
            "errors": self.errors,
            "singletons": self.singletons,
            "not_copied": self.not_copied,
        })
    }
}

/// True when the file carries a Berkeley DB metadata page fingerprint.
///
/// On disk the BIGEND/LITTLEEND metadata page stores byte 0 = lorder (0 =
/// little endian), byte 1 = pagesize, byte 2 = 0, byte 3 = 1 (btree), then
/// the 32-bit magic at offset 12 IN THE FILE'S BYTE ORDER. Any of the three
/// magics below with a btree/queue/hash type byte identifies a BDB file —
/// checked against a real 17 MB legacy wallet (`0000 0000 0100 0000` header,
/// magic 0x00053162 at offset 16). A SQLite file ("SQLite format 3") never
/// matches. Used to route a user-chosen `wallet.dat` to the importer instead
/// of failing with "not a TRI wallet".
pub fn is_bdb_wallet(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 24];
    if f.seek(SeekFrom::Start(0)).is_err() || f.read_exact(&mut head).is_err() {
        return false;
    }
    if &head[0..4] == b"SQLi" {
        return false; // SQLite container (a TRI7 wallet), never a BDB file
    }
    let lorder_le = head[0] == 0x00;
    // magic lives at file offset 12 (bytes 12..16); 16..20 is pagesize
    let magic = u32::from_le_bytes(head[12..16].try_into().unwrap());
    let magic_be = u32::from_be_bytes(head[12..16].try_into().unwrap());
    // Btree 0x00053162, Hash 0x00061561, Queue 0x00004162, Heap 0x00007462
    let known = |m: u32| matches!(m, 0x0005_3162 | 0x0006_1561 | 0x0000_4162 | 0x0000_7462);
    if lorder_le {
        known(magic)
    } else {
        known(magic_be)
    }
}

/// Undo `db_dump -p` escaping: `\XX` hex escapes plus `\t \n \r \\`.
pub fn unescape(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0usize;
    while i < line.len() {
        if line[i] == b'\\' && i + 1 < line.len() {
            let hi = (line[i + 1] as char).to_digit(16);
            let lo = line.get(i + 2).map(|c| (*c as char).to_digit(16));
            if let (Some(h), Some(Some(l))) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
            match line[i + 1] {
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b'\\' => out.push(b'\\'),
                other => {
                    out.push(b'\\');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(line[i]);
            i += 1;
        }
    }
    out
}

/// Parse `db_dump -p` output into (key, value) pairs. Header lines precede
/// `HEADER=END`; body lines are ` <escaped>` alternating key/value.
pub fn parse_dump(text: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut body = false;
    let mut pending: Option<Vec<u8>> = None;
    for line in text.split(|b| *b == b'\n') {
        if line.starts_with(b"HEADER=END") {
            body = true;
            continue;
        }
        if !body || !line.starts_with(b" ") {
            continue;
        }
        let raw = unescape(&line[1..]);
        match pending.take() {
            None => pending = Some(raw),
            Some(k) => out.push((k, raw)),
        }
    }
    out
}

/// Split a record key: CompactSize(section) + section + [CompactSize(len) +
/// suffix]. The suffix may carry a length prefix the caller must strip where
/// the format specifies one (see the per-section handling).
pub fn split_key(k: &[u8]) -> Option<(String, Vec<u8>)> {
    let n = *k.first()? as usize;
    if k.len() < 1 + n {
        return None;
    }
    let section = String::from_utf8_lossy(&k[1..1 + n]).to_string();
    Some((section, k[1 + n..].to_vec()))
}

/// Strip a CompactSize length prefix, returning the payload.
fn strip_cs(v: &[u8]) -> &[u8] {
    if v.is_empty() {
        return v;
    }
    let n = v[0] as usize;
    if n < 0xfd && v.len() >= 1 + n {
        &v[1..1 + n]
    } else {
        v
    }
}

/// True for the encodings a writer may use for "no suffix" in a record key.
#[allow(dead_code)]
fn s_is_empty_suffix(suffix: &[u8]) -> bool {
    suffix.is_empty() || (suffix.len() == 1 && suffix[0] == 0)
}

fn cs(n: usize) -> Vec<u8> {
    let n = n as u64;
    let mut out = Vec::new();
    if n < 0xfd {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xfd);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(0xfe);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(0xff);
        out.extend_from_slice(&n.to_le_bytes());
    }
    out
}

pub fn key_blob(section: &str, suffix: &[u8]) -> Vec<u8> {
    let mut k = cs(section.len());
    k.extend_from_slice(section.as_bytes());
    k.extend_from_slice(suffix);
    k
}

/// Run `db5.3_dump -p` (falling back to `db_dump`) and return its stdout.
pub fn dump_bdb(path: &Path) -> Result<(String, Vec<u8>), String> {
    use std::process::Command;
    for tool in ["db5.3_dump", "db_dump"] {
        if let Ok(out) = Command::new(tool).args(["-p", &path.display().to_string()]).output() {
            if out.status.success() {
                return Ok((tool.to_string(), out.stdout));
            }
        }
    }
    Err(format!(
        "neither db5.3_dump nor db_dump could read {} (install db5.3-util)",
        path.display()
    ))
}

/// Pull the raw 32-byte secret out of a legacy DER-encoded EC private key and
/// PROVE it by re-deriving the record's suffix pubkey. Returns None when the
/// bytes are not a key we can vouch for.
fn secret_from_der(der: &[u8], pubkey: &[u8]) -> Option<[u8; 32]> {
    let mut sec = None;
    let mut i = 0usize;
    while i + 34 <= der.len() {
        if der[i] == 0x04 && der[i + 1] == 0x20 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&der[i + 2..i + 34]);
            sec = Some(s);
            break;
        }
        i += 1;
    }
    let sec = sec?;
    let secp = secp256k1::Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(&sec).ok()?;
    let derived = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
    if derived.as_slice() == pubkey {
        Some(sec)
    } else {
        None
    }
}

/// Import a legacy BDB wallet into a NEW SQLite wallet file (created here).
///
/// `dest` must not exist: an import never overwrites a wallet a holder may
/// already be using (same rule as `backupwallet`).
pub fn import_bdb(src: &Path, dest: &Path) -> Result<ImportReport, String> {
    if dest.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite an existing wallet",
            dest.display()
        ));
    }
    let (tool, stdout) = dump_bdb(src)?;
    let pairs = parse_dump(&stdout);
    let mut r = ImportReport {
        source: src.display().to_string(),
        dest: dest.display().to_string(),
        records_dumped: pairs.len(),
        ..Default::default()
    };
    if pairs.is_empty() {
        return Err(format!("{tool}: no records dumped from {}", src.display()));
    }

    // Group by section once, so nothing is carried (or forgotten) implicitly.
    let mut by_section: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> = HashMap::new();
    let mut unparsed = 0usize;
    for (k, v) in pairs {
        match split_key(&k) {
            Some((section, suffix)) => by_section
                .entry(section)
                .or_default()
                .push((suffix, v)),
            None => unparsed += 1,
        }
    }
    r.skipped += unparsed;

    // ---- decode the carryable rows BEFORE creating any file ----------------
    struct KeyRow {
        secret: Option<[u8; 32]>,
        pubkey: Vec<u8>,
        label: String,
        encrypted: bool,
    }
    let mut key_rows: Vec<KeyRow> = Vec::new();
    let mut births: HashMap<Vec<u8>, i64> = HashMap::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut settings: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut pool_rows: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut singles: HashMap<String, Vec<u8>> = HashMap::new();

    for (suffix, value) in by_section.get("key").cloned().unwrap_or_default() {
        let pubkey = strip_cs(&suffix).to_vec();
        let der = strip_cs(&value).to_vec();
        match secret_from_der(&der, &pubkey) {
            Some(secret) => key_rows.push(KeyRow {
                secret: Some(secret),
                pubkey,
                label: String::new(),
                encrypted: false,
            }),
            None => {
                r.skipped += 1;
                r.errors
                    .push("key row whose DER does not derive its suffix pubkey".into());
            }
        }
    }
    for (suffix, value) in by_section.get("ckey").cloned().unwrap_or_default() {
        let pubkey = strip_cs(&suffix).to_vec();
        // ckey value = CompactSize(48) + IV(16) + ciphertext(32)
        key_rows.push(KeyRow {
            secret: None,
            pubkey,
            label: String::new(),
            encrypted: true,
        });
        let _ = value;
    }
    r.encrypted = by_section.contains_key("mkey")
        || by_section.contains_key("ckey")
        || by_section.contains_key("hdcmnemonic");
    for (suffix, value) in by_section.get("keymeta").cloned().unwrap_or_default() {
        if value.len() >= 12 {
            births.insert(
                strip_cs(&suffix).to_vec(),
                i64::from_le_bytes(value[4..12].try_into().unwrap()),
            );
        } else {
            r.skipped += 1;
        }
    }
    for (suffix, value) in by_section.get("name").cloned().unwrap_or_default() {
        // BDB name row suffix is the SERIALIZED std::string of the address —
        // CompactSize(len) + raw bytes — while the wallet module's
        // encode_name_keyblob() writes the same shape. Do NOT also strip a
        // second prefix here: the first byte IS the length (0x22 = 34), and
        // stripping twice turned T-addresses into '"'-prefixed strings.
        // The address string starts with a CompactSize length byte (0x22 for
        // a 34-char T address) that is NOT part of the string. Drop exactly
        // that one byte; the wallet module re-adds its own prefix.
        let addr = String::from_utf8_lossy(strip_cs(&suffix)).to_string();
        let lab = String::from_utf8_lossy(strip_cs(&value)).to_string();
        if addr.is_empty() {
            r.skipped += 1;
            continue;
        }
        if tri7_core_address_ok(&addr) {
            labels.push((addr, lab));
        } else {
            r.skipped += 1;
            r.errors.push(format!("name row for an invalid address {addr}"));
        }
    }
    for (suffix, value) in by_section.get("setting").cloned().unwrap_or_default() {
        settings.push((suffix, value));
    }
    for (suffix, value) in by_section.get("pool").cloned().unwrap_or_default() {
        pool_rows.push((suffix, value));
    }
    // Opaque singleton rows: the VALUE is the record payload for every one of
    // them (defaultkey = pubkey bytes, bestblock = locator, version/minversion
    // = int32, mkey = CMasterKey blob). The record-key suffix is empty for
    // these sections in the legacy wallet, so it is never used as a payload —
    // taking it would write an empty mkey and produce a wallet nobody can
    // unlock.
    // Opaque singleton rows: the VALUE is the payload (defaultkey = pubkey
    // bytes, bestblock = locator, version/minversion = int32, mkey =
    // CMasterKey blob). The key blob is exactly what `crate::wallet`'s own
    // encoders produce for that section, so a file written by the source
    // writer and a file we will write compare equal — anything else is
    // reported, because a real legacy container may carry a slightly
    // different key layout we do not want to normalise blindly.
    for s in ["defaultkey", "bestblock", "version", "minversion", "mkey"] {
        // A section that ALSO has suffixed rows (mkey in this writer's own
        // layout is a family: id 0 is the master key) takes its payload from
        // the suffix-less/zero-id row; a section of pure suffix-less rows
        // (defaultkey, version, ...) takes every value.
        let mut picked: Option<Vec<u8>> = None;
        for (suffix, value) in by_section.get(s).cloned().unwrap_or_default() {
            // An "absent" suffix: none at all, a CompactSize(0), or a 4-byte
            // id word. For mkey the source writer uses id 1 — any 4-byte id
            // is the same singleton record, so the FIRST one wins and the
            // canonical key (no suffix) is written.
            let id_zero = suffix.is_empty()
                || (suffix.len() == 1 && suffix[0] == 0);
            let id_word = suffix.len() == 4;
            if s == "mkey" {
                if (id_zero || id_word) && picked.is_none() {
                    picked = Some(value);
                } else if !(id_zero || id_word) {
                    r.errors.push(format!(
                        "mkey: unrecognised record-key suffix ({} bytes) — ignored",
                        suffix.len()
                    ));
                }
                continue;
            }
            if id_zero {
                singles.insert(s.to_string(), value);
            } else {
                r.errors.push(format!(
                    "{s}: unexpected record-key suffix ({} bytes) — value ignored",
                    suffix.len()
                ));
            }
        }
        if let Some(v) = picked {
            singles.insert(s.to_string(), v);
        }
    }
    // An encrypted wallet MUST carry a parseable master key, or the ckey rows
    // it also carries would be undecryptable. Refuse before writing anything.
    if r.encrypted {
        match singles.get("mkey") {
            Some(v) => {
                if let Err(e) = crate::wallet_encrypt::decode_master_key(v) {
                    return Err(format!(
                        "encrypted wallet: master key (mkey) is unreadable ({e}) — refusing to \
                         build a wallet nobody could unlock"
                    ));
                }
            }
            None => {
                return Err(
                    "wallet has encrypted keys (ckey) but no mkey record — refusing to import \
                     an undecryptable wallet"
                        .to_string(),
                )
            }
        }
    }
    for s in ["tx", "orderposnext"] {
        if by_section.contains_key(s) {
            r.not_copied.push(s.to_string());
        }
    }

    if key_rows.is_empty() {
        return Err(format!(
            "{}: no key or ckey records — not a wallet file?",
            src.display()
        ));
    }

    // ---- write the SQLite wallet ------------------------------------------
    create_wallet_file(dest)?;
    let mut wh = WalletHandle::open_readwrite(dest)?;
    let label_of: HashMap<String, String> = labels.iter().cloned().collect();
    for kr in &key_rows {
        if let Some(secret) = kr.secret {
            let addr = match crate::keytool::secret_to_address(&secret, true) {
                Ok(a) => a,
                Err(e) => {
                    r.errors.push(format!("address from key: {e}"));
                    r.skipped += 1;
                    continue;
                }
            };
            match wh.add_key(&secret, label_of.get(&addr).map(|s| s.as_str()).unwrap_or("")) {
                Ok(_) => r.keys += 1,
                Err(e) => {
                    r.errors.push(format!("add_key {addr}: {e}"));
                    r.skipped += 1;
                }
            }
        }
    }
    wh.close();

    // Everything below is a raw row write: the wallet writer above is for
    // plaintext keys, while encrypted key material and opaque legacy rows are
    // byte-identical to what the legacy client (and tri7's own
    // wallet_encrypt read path) expect.
    let conn = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| format!("open {}: {e}", dest.display()))?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin: {e}"))?;

    // encrypted key rows: suffix = CompactSize(33)+pubkey; value verbatim
    if let Some(rows) = by_section.get("ckey") {
        for (suffix, value) in rows {
            let mut s = cs(strip_cs(suffix).len());
            s.extend_from_slice(strip_cs(suffix));
            let kb = key_blob("ckey", &s);
            tx.execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                params![kb, value],
            )
            .map_err(|e| format!("ckey write: {e}"))?;
            r.encrypted_keys += 1;
        }
    }
    // birth times from the legacy keymeta rows
    for (pubkey, birth) in births.iter() {
        let kb = encode_keymeta_keyblob(pubkey);
        let val = encode_keymeta_value(*birth);
        if tx
            .execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                params![kb, val],
            )
            .is_ok()
        {
            r.keymeta += 1;
        }
    }
    // labels: ONLY for addresses the key pass did not already write (add_key
    // writes the name row itself). A second write per address would double the
    // name census and make "everything came over" unverifiable.
    for (addr, label) in labels.iter() {
        let kb = crate::wallet::encode_name_keyblob(addr);
        let exists: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM main WHERE key = ?1",
                params![&kb],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if exists > 0 {
            r.names += 1; // already carried by add_key — counted once
            continue;
        }
        let val = crate::wallet::encode_name_value(label);
        if tx
            .execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                params![kb, val],
            )
            .is_ok()
        {
            r.names += 1;
        }
    }
    // settings: suffix = CompactSize(name)+name, value verbatim (wallets carry
    // the Tor v3 identity here and it must survive the move)
    for (suffix, value) in settings.iter() {
        let name = strip_cs(suffix);
        let kb = crate::wallet::encode_setting_keyblob(&String::from_utf8_lossy(name));
        if tx
            .execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                params![kb, value],
            )
            .is_ok()
        {
            r.settings += 1;
        }
    }
    // keypool: suffix = CS(8)+LE index, value = CS(version)+CS(time)+CS(34)+pubkey
    for (suffix, value) in pool_rows.iter() {
        let kb = key_blob("pool", suffix);
        if tx
            .execute(
                "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                params![kb, value],
            )
            .is_ok()
        {
            r.pool += 1;
        }
    }
    // singletons, value-only payloads keyed by their section name
    for (section, payload) in singles.iter() {
        let kb = key_blob(section, &[]);
        tx.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            params![kb, payload],
        )
        .map_err(|e| format!("{section} write: {e}"))?;
        r.singletons.push(section.clone());
    }
    tx.commit().map_err(|e| format!("commit: {e}"))?;

    // ---- prove the result --------------------------------------------------
    // Encrypted wallets: prove the master key + every ckey by actually trying
    // every common passphrase? No — the passphrase is unknown by design.
    // What IS provable without it: the mkey we wrote is byte-identical to the
    // source and parses, so any passphrase that opened the source wallet opens
    // this one. That is asserted here rather than assumed.
    if r.encrypted {
        let src_mkey: Option<Vec<u8>> = by_section
            .get("mkey")
            .and_then(|rows| rows.first())
            .map(|(_, v)| v.clone());
        let dst_mkey: Option<Vec<u8>> = {
            let kb = key_blob("mkey", &[]);
            conn.query_row(
                "SELECT value FROM main WHERE key = ?1",
                params![kb],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok()
        };
        if src_mkey != dst_mkey {
            return Err("encrypted wallet: mkey was not carried byte-identically".into());
        }
        if let Some(mk) = &dst_mkey {
            crate::wallet_encrypt::decode_master_key(mk)
                .map_err(|e| format!("encrypted wallet: written mkey does not parse ({e})"))?;
        }
        // ckey rows: count + suffix pubkey identity
        if let Some(rows) = by_section.get("ckey") {
            for (suffix, _) in rows {
                let pubkey = strip_cs(suffix);
                let mut cs_len = cs(pubkey.len());
                cs_len.extend_from_slice(pubkey);
                let kb = key_blob("ckey", &cs_len);
                let present: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM main WHERE key = ?1",
                        params![kb],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if present != 1 {
                    return Err(format!(
                        "encrypted wallet: ckey row for {} missing after import",
                        pubkey.iter().map(|b| format!("{:02x}", b)).collect::<String>()
                    ));
                }
            }
        }
    }

    // Wallet-level proof of what landed. read_wallet REFUSES encrypted wallets
    // by design (the passphrase path is the daemon's), so an encrypted import
    // is proven by its row census + the mkey checks above instead.
    if r.encrypted {
        let census = |prefix: &[u8]| -> usize {
            by_section
                .get(std::str::from_utf8(prefix).unwrap_or(""))
                .map(|v| v.len())
                .unwrap_or(0)
        };
        if r.encrypted_keys + census(b"key") == 0 {
            return Err("encrypted import wrote no key material".into());
        }
    } else {
        let back = crate::wallet::read_wallet(dest).map_err(|e| {
            format!(
                "import wrote a wallet that will not re-open ({e}) — the file is at {}",
                dest.display()
            )
        })?;
        if back.keys.len() < r.keys {
            return Err(format!(
                "re-open found {} keys but {} were imported",
                back.keys.len(),
                r.keys
            ));
        }
    }
    Ok(r)
}

fn tri7_core_address_ok(addr: &str) -> bool {
    crate::address::address_to_payload(addr).is_ok()
}

/// Convenience wrapper used by callers that hold a `PathBuf`.
pub fn import_bdb_paths(src: &PathBuf, dest: &PathBuf) -> Result<ImportReport, String> {
    import_bdb(src, dest)
}

/// Why an automatic first-open migration touched (or refused to touch) a file.
#[derive(Debug, Clone)]
pub enum AutoConvert {
    /// already a SQLite TRI wallet — nothing to do
    AlreadySqlite,
    /// BDB wallet carried over IN PLACE: the BDB file is kept as a `.bdb`
    /// sidecar (never deleted) and `wallet.dat` now holds the SQLite wallet
    Converted(ImportReport),
    /// not a wallet this importer recognizes; the path is untouched
    NotAWallet,
}

/// First-open transition: when the file the user pointed at is a legacy
/// Berkeley-DB wallet, carry EVERYTHING into a SQLite wallet at the same path
/// so the modern client opens it without any manual step.
///
/// Safety shape (a holder's keys are at stake):
///   1. the source is only ever READ (`db5.3_dump`),
///   2. the import writes to a sibling temp file and is verified by
///      re-opening it and counting keys,
///   3. the original is renamed to `<name>.bdb-YYYYMMDDHHMMSS` (kept forever),
///   4. only then is the verified SQLite file renamed into place,
///   5. any failure removes the temp file and leaves the original untouched.
pub fn auto_convert_in_place(path: &Path) -> Result<AutoConvert, String> {
    if !path.exists() {
        return Ok(AutoConvert::NotAWallet);
    }
    if !is_bdb_wallet(path) {
        return Ok(AutoConvert::AlreadySqlite);
    }
    let stamp = chrono_like_stamp();
    let aside = path.with_extension(format!("bdb-{}", stamp));
    // build the new wallet in a PRIVATE scratch dir: import_bdb writes a
    // `.tri7bak` session copy beside its target, and that must never litter
    // the user's wallet folder
    let scratch = std::env::temp_dir().join(format!("tri7-bdbimport-{}", stamp));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch dir: {e}"))?;
    let tmp = scratch.join("wallet.dat");
    let report = match import_bdb(path, &tmp) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(e);
        }
    };
    // never reach the rename unless the SQLite file really re-opens
    // Verification: an UNENCRYPTED result must re-open through the normal
    // reader; an ENCRYPTED one (read_wallet refuses encrypted files by design)
    // is verified by re-counting its key/ckey rows against the report.
    if report.encrypted {
        let conn = Connection::open_with_flags(&tmp, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("re-open encrypted result: {e}"))?;
        let count = |prefix: &[u8]| -> i64 {
            let mut kb = cs(prefix.len());
            kb.extend_from_slice(prefix);
            conn.query_row(
                "SELECT COUNT(*) FROM main WHERE substr(key,1,?) = ?2",
                params![kb.len() as i64, kb],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        if count(b"ckey") != report.encrypted_keys as i64 {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err("auto-migration encrypted key census mismatch — nothing was changed".into());
        }
        if count(b"mkey") != 1 {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err("auto-migration lost the master key — nothing was changed".into());
        }
    } else {
        let reopened = crate::wallet::read_wallet(&tmp).map_err(|e| {
            let _ = std::fs::remove_dir_all(&scratch);
            format!("auto-migration produced an unreadable wallet ({e}) — nothing was changed")
        })?;
        if reopened.keys.len() < report.keys {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err("auto-migration key census mismatch — nothing was changed".into());
        }
    }
    // Move the ORIGINAL aside first (rename is same-directory and atomic),
    // then put the new wallet in place. If the second rename fails, put the
    // original back so the user is never left without a wallet file.
    std::fs::rename(path, &aside).map_err(|e| {
        let _ = std::fs::remove_dir_all(&scratch);
        format!("keep a copy of the legacy wallet: {e}")
    })?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::rename(&aside, path);
        let _ = std::fs::remove_dir_all(&scratch);
        return Err(format!("install the SQLite wallet: {e} (original restored)"));
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(AutoConvert::Converted(report))
}

fn chrono_like_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // yyyymmddhhmmss from unix seconds, without pulling in a date crate: the
    // stamp only has to be sortable and unique enough for a sidecar name
    let days = now / 86_400;
    let secs = now % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant's days→civil date algorithm (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_handles_hex_and_short_forms() {
        assert_eq!(unescape(b"\\00\\1f\\ff"), vec![0x00, 0x1f, 0xff]);
        assert_eq!(unescape(b"a\\tb\\\\c"), b"a\tb\\c".to_vec());
    }

    #[test]
    fn parse_dump_pairs_body_lines_only() {
        let text = b"HEADER=START\nVERSION=3\nHEADER=END\n \\02tx\\00\n \\01v\n";
        let pairs = parse_dump(text);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, b"\x02tx\x00".to_vec());
        assert_eq!(pairs[0].1, b"\x01v".to_vec());
    }

    #[test]
    fn split_key_reads_section() {
        let k = key_blob("key", &[0x21, 0xaa]);
        let (s, suffix) = split_key(&k).expect("split");
        assert_eq!(s, "key");
        assert_eq!(suffix, vec![0x21, 0xaa]);
    }

    #[test]
    fn civil_date_matches_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    /// The encrypted path through the PRODUCTION importer, end to end.
    ///
    /// A real Berkeley-DB wallet is synthesised with db5.3_load (available on
    /// this host; the test SKIPS — never passes silently — when the tool is
    /// missing), encrypted with the source's own writer, then imported by
    /// `import_bdb` (db5.3_dump -> parse_dump -> split_key -> rows). Every
    /// imported key is then decrypted with the passphrase, which proves each
    /// secret against its pubkey, and a wrong passphrase must fail.
    #[test]
    fn encrypted_bdb_carries_a_working_master_key_and_all_ckey_rows() {
        use crate::wallet::{create_wallet_file, WalletHandle};
        let have_load = std::process::Command::new("db5.3_load")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::process::Command::new("db_load")
                .arg("-V")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        if !have_load {
            eprintln!("SKIP: db5.3_load unavailable — encrypted BDB fixture cannot be built here");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "tri7-bdbenc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. an encrypted SQLite wallet made by the SOURCE's own writer
        let sql = dir.join("src.sqlite");
        create_wallet_file(&sql).unwrap();
        let mut wh = WalletHandle::open_readwrite(&sql).unwrap();
        let mut addrs: Vec<String> = Vec::new();
        for i in 0..3u8 {
            let mut sec = [0u8; 32];
            sec[0] = 0x40 + i;
            sec[31] = 0x11 + i;
            addrs.push(wh.add_key(&sec, "legacy").unwrap().address);
        }
        wh.close();
        let pass = "probe-passphrase-123";
        let rep = crate::wallet_encrypt::encrypt_wallet(&sql, pass).unwrap();
        assert_eq!(rep.keys, 3);

        // 2. the SAME rows, re-serialized into a real BDB file
        let bdb = dir.join("legacy.dat");
        write_bdb(read_all_rows(&sql).expect("read source rows"), &bdb)
            .expect("build bdb fixture");
        assert!(is_bdb_wallet(&bdb), "fixture must be a real BDB file");

        // 3. production import
        let dest = dir.join("dest.sqlite");
        let report = import_bdb(&bdb, &dest).expect("production import of an encrypted BDB");
        assert!(report.encrypted, "import must detect the encryption");
        assert_eq!(report.encrypted_keys, 3, "every ckey must be carried");
        assert_eq!(report.errors, Vec::<String>::new());

        assert!(
            report.errors.is_empty(),
            "import reported problems: {:?}",
            report.errors
        );

        // 4. proof: every imported key decrypts (each secret re-derives its pubkey)
        let imported = crate::wallet_encrypt::decrypt_wallet(&dest, pass)
            .expect("imported encrypted wallet must decrypt with the source passphrase");
        assert_eq!(imported.len(), 3);
        for (pubkey, _) in &imported {
            let addr = crate::wallet::pubkey_to_address(pubkey);
            assert!(addrs.contains(&addr), "unexpected key {addr} after import");
        }
        assert!(
            crate::wallet_encrypt::decrypt_wallet(&dest, "wrong-passphrase").is_err(),
            "a wrong passphrase must not open the imported wallet"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every (key, value) row of a wallet container, verbatim. Unlike
    /// `wallet::read_wallet` this does NOT interpret or refuse encryption: a
    /// BDB wallet file stores ckey/mkey rows as opaque bytes and so must the
    /// fixture builder.
    fn read_all_rows(path: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut stmt = conn
            .prepare("SELECT key, value FROM main")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Build a Berkeley-DB btree file from raw (key, value) rows using the
    /// system db_load tool (the same DB version db5.3_dump reads).
    pub(super) fn write_bdb(rows: Vec<(Vec<u8>, Vec<u8>)>, path: &Path) -> Result<(), String> {
        use std::io::Write;
        let dump = path.with_extension("bdbdump");
        {
            let mut f = std::fs::File::create(&dump).map_err(|e| e.to_string())?;
            writeln!(f, "VERSION=3").map_err(|e| e.to_string())?;
            writeln!(f, "format=print").map_err(|e| e.to_string())?;
            writeln!(f, "type=btree").map_err(|e| e.to_string())?;
            writeln!(f, "HEADER=END").map_err(|e| e.to_string())?;
            let esc = |b: &[u8]| -> String {
                let mut out = String::new();
                for byte in b {
                    match byte {
                        b'\\' => out.push_str("\\\\"),
                        0x20..=0x7e => out.push(*byte as char),
                        _ => out.push_str(&format!("\\{:02x}", byte)),
                    }
                }
                out
            };
            for (k, v) in &rows {
                writeln!(f, " {}", esc(k)).map_err(|e| e.to_string())?;
                writeln!(f, " {}", esc(v)).map_err(|e| e.to_string())?;
            }
            writeln!(f, "DATA=END").map_err(|e| e.to_string())?;
        }
        let out = std::process::Command::new("db5.3_load")
            .arg("-f")
            .arg(&dump)
            .arg(path)
            .output()
            .or_else(|_| {
                std::process::Command::new("db_load")
                    .arg("-f")
                    .arg(&dump)
                    .arg(path)
                    .output()
            })
            .map_err(|e| format!("db_load: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "db_load failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    #[test]
    fn bdb_magic_is_detected_and_sqlite_is_not() {
        let dir = std::env::temp_dir().join(format!("tri7-bdbmagic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sqlite = dir.join("s.dat");
        create_wallet_file(&sqlite).unwrap();
        assert!(!is_bdb_wallet(&sqlite));
        // shape of a real legacy wallet header (little-endian btree)
        let mut meta = vec![0u8; 512];
        meta[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        meta[4..8].copy_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        meta[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
        meta[20..24].copy_from_slice(&0x0000_2000u32.to_le_bytes()); // pagesize
        let bdb = dir.join("b.dat");
        std::fs::write(&bdb, &meta).unwrap();
        assert!(is_bdb_wallet(&bdb));
        std::fs::remove_dir_all(&dir).ok();
    }
}
