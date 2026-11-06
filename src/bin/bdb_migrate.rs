//! bdb_migrate — move a legacy Berkeley-DB wallet into tri7's SQLite wallet.
//!
//! tri7 only reads the SQLite `wallet.dat` format. Every older wallet in
//! E:\Coins\TRI (and every wallet produced by the pre-SQLite client) is
//! Berkeley DB, so a holder on an old client currently has NO path in. This is
//! that path.
//!
//! It reads the BDB file through `db5.3_dump -p` (the Berkeley DB dump tool),
//! parses the `key` / `keymeta` / `name` records, and writes a fresh SQLite
//! wallet with the same secrets, birth times and labels.
//!
//! Usage:
//!   bdb_migrate <bdb_wallet.dat> <new_sqlite_wallet.dat> [--dry-run]
//!
//! Encrypted BDB wallets (mkey/ckey present) are REFUSED, not guessed at: the
//! decryption path needs a passphrase UI and a verified ckey layout, and a
//! half-right import of somebody's keys is worse than a clear refusal.

use std::collections::HashMap;
use std::process::Command;
use tri7_core::wallet::{create_wallet_file, WalletHandle};

/// Undo `db_dump -p` escaping: backslash followed by two hex digits, or a
/// single escaped character (\t \n \\ etc).
fn unescape(line: &str) -> Vec<u8> {
    let b = line.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            // two hex digits?
            if i + 3 < b.len() + 1 {
                let h = (b[i + 1] as char).to_digit(16);
                let l = b.get(i + 2).and_then(|c| (*c as char).to_digit(16));
                if let (Some(h), Some(l)) = (h, l) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
            }
            match b[i + 1] {
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
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Parse `db_dump -p` output into (key, value) pairs. The format is:
///   " <key>" on one line, " <value>" on the next, ending with "HEADER=END".
fn parse_dump(text: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    let mut in_body = false;
    for line in text.lines() {
        if line.starts_with("HEADER=END") {
            in_body = true;
            continue;
        }
        if !in_body || !line.starts_with(' ') {
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: bdb_migrate <bdb_wallet.dat> <new_sqlite_wallet.dat> [--dry-run]");
        std::process::exit(2);
    }
    let src = &args[1];
    let dest = &args[2];
    let dry = args.iter().any(|a| a == "--dry-run");

    // Prefer db5.3_dump, fall back to db_dump.
    let (tool, out) = ["db5.3_dump", "db_dump"]
        .iter()
        .find_map(|t| {
            Command::new(t)
                .args(["-p", src])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| (*t, o.stdout))
        })
        .unwrap_or_else(|| {
            eprintln!("bdb_migrate: neither db5.3_dump nor db_dump could dump {src}");
            std::process::exit(2);
        });
    let text = String::from_utf8_lossy(&out).to_string();
    let pairs = parse_dump(&text);
    println!("{tool}: {} records", pairs.len());

    // Encrypted wallets are refused on purpose (see the module comment).
    let encrypted = pairs.iter().any(|(k, _)| {
        k.windows(4).any(|w| w == b"mkey")
            || k.windows(4).any(|w| w == b"ckey")
            || k.windows(11).any(|w| w == b"hdcmnemonic")
    });
    if encrypted {
        eprintln!(
            "bdb_migrate: {src} is ENCRYPTED (mkey/ckey rows). Refusing to guess at the\n\
             key layout — export the key with the old client (dumpprivkey / dumpwallet)\n\
             and import it with: importprivkey / importwallet."
        );
        std::process::exit(3);
    }

    let mut keys: Vec<[u8; 32]> = Vec::new();
    let mut keymeta: HashMap<Vec<u8>, i64> = HashMap::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut skipped = 0usize;
    // BDB keys are: CompactSize(len(section)) + section + CompactSize(len(suffix))
    // + suffix. For CKey records the suffix is the 33-byte compressed PUBLIC KEY
    // (not the hash160), which is what the dump shows as "\03key!\02\03..." —
    // 0x21 == 33.
    fn split_key(k: &[u8]) -> Option<(&str, &[u8])> {
        if k.is_empty() {
            return None;
        }
        let n = k[0] as usize;
        if k.len() < 1 + n {
            return None;
        }
        let section = std::str::from_utf8(&k[1..1 + n]).ok()?;
        let rest = &k[1 + n..];
        if rest.is_empty() {
            return Some((section, &[]));
        }
        let m = rest[0] as usize;
        if rest.len() < 1 + m {
            return Some((section, &rest[1..]));
        }
        Some((section, &rest[1..1 + m]))
    }

    for (k, v) in &pairs {
        let Some((section, suffix)) = split_key(k) else { continue };
        match section {
            // key: suffix = 33-byte pubkey; value = 32-byte secret + flag
            "key" if suffix.len() == 33 && v.len() >= 39 => {
                // The stored value is a DER EC private key (the same encoding
                // tri7 writes), NOT a raw 32-byte secret: the dump shows
                // SEQUENCE { INTEGER 1, OCTET STRING <32 bytes>, [0] curve OID,
                // [1] public key }. Pull the OCTET STRING and then PROVE it by
                // re-deriving the public key and comparing with the pubkey in
                // the record's suffix. A wrong parse cannot survive that check.
                let mut sec = None;
                let mut i = 0usize;
                while i + 34 <= v.len() {
                    if v[i] == 0x04 && v[i + 1] == 0x20 {
                        let mut s = [0u8; 32];
                        s.copy_from_slice(&v[i + 2..i + 34]);
                        sec = Some(s);
                        break;
                    }
                    i += 1;
                }
                let Some(sec) = sec else {
                    skipped += 1;
                    continue;
                };
                let secp = secp256k1::Secp256k1::signing_only();
                let derived_pub = match secp256k1::SecretKey::from_slice(&sec) {
                    Ok(sk) => secp256k1::PublicKey::from_secret_key(&secp, &sk)
                        .serialize()
                        .to_vec(),
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                if derived_pub != suffix {
                    skipped += 1;
                    continue; // not this key's secret — refuse rather than guess
                }
                keys.push(sec);
            }
            // keymeta: suffix = pubkey; value = int32 version + int64 birth
            "keymeta" if v.len() >= 12 => {
                let t = i64::from_le_bytes(v[4..12].try_into().unwrap());
                keymeta.insert(suffix.to_vec(), t);
            }
            // name: suffix = address string; value = CompactSize(label) + label
            "name" => {
                let addr = String::from_utf8_lossy(suffix).to_string();
                let lab = if !v.is_empty() {
                    let l = v[0] as usize;
                    if v.len() >= 1 + l {
                        String::from_utf8_lossy(&v[1..1 + l]).to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                labels.push((addr, lab));
            }
            _ => {}
        }
    }
    keys.sort();
    keys.dedup();
    println!(
        "parsed: {} unique keys, {} keymeta rows, {} labels{}",
        keys.len(),
        keymeta.len(),
        labels.len(),
        if skipped > 0 {
            format!(", {skipped} malformed key rows skipped")
        } else {
            String::new()
        }
    );
    if keys.is_empty() {
        eprintln!("bdb_migrate: no key records found — is this really a wallet file?");
        std::process::exit(1);
    }

    if dry {
        for s in keys.iter().take(3) {
            println!(
                "  would import {}",
                tri7_core::keytool::secret_to_address(s, true).unwrap_or_default()
            );
        }
        println!("(dry run — nothing written)");
        return;
    }

    if let Err(e) = create_wallet_file(std::path::Path::new(dest)) {
        eprintln!("create {dest}: {e}");
        std::process::exit(2);
    }
    let mut wh = match WalletHandle::open_readwrite(std::path::Path::new(dest)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("open {dest}: {e}");
            std::process::exit(2);
        }
    };
    let mut imported = 0usize;
    let mut errors = 0usize;
    let label_of: HashMap<String, String> = labels.into_iter().collect();
    for sec in &keys {
        let addr = match tri7_core::keytool::secret_to_address(sec, true) {
            Ok(a) => a,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let label = label_of.get(&addr).cloned().unwrap_or_default();
        match wh.add_key(sec, &label) {
            Ok(_) => imported += 1,
            Err(_) => errors += 1,
        }
    }
    wh.close();

    // add_key stamps birth time = now. The old client recorded the real one, so
    // rewrite the keymeta rows from the parsed records (the only metadata the
    // old wallet carried that tri7 preserves).
    let mut births = 0usize;
    if !keymeta.is_empty() {
        match rusqlite::Connection::open(dest) {
            Ok(conn) => {
                for (pubkey, birth) in keymeta.iter() {
                    let kb = tri7_core::wallet::encode_keymeta_keyblob(pubkey);
                    let val = tri7_core::wallet::encode_keymeta_value(*birth);
                    if let Ok(n) = conn.execute(
                        "UPDATE main SET value=?1 WHERE key=?2",
                        rusqlite::params![val, kb],
                    ) {
                        births += n;
                    }
                }
            }
            Err(e) => eprintln!("keymeta rewrite skipped: {e}"),
        }
    }
    println!(
        "imported {imported} keys into {dest} ({errors} refused, {births} birth times restored)"
    );
}
