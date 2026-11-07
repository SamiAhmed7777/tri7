//! Wallet key export/import tooling — the legacy rpcdump.cpp surface
//! (dumpprivkey / dumpwallet / importwallet / checkwallet) for tri7's
//! SQLite-native wallet. Parity-audit gap (2026-11-03): tri7 had no key
//! export path at all; the legacy client's dumpwallet format is the
//! interchange format Sami's existing backups use.
//!
//! Formats (byte-verified against triangles_v5/src/rpcdump.cpp + base58.h):
//!
//! - WIF secret key: base58check(version 193 = 128 + PUBKEY_ADDRESS(65)),
//!   payload = [0xC1] + 32-byte secret [+ 0x01 when compressed].
//!   NOTE: NOT bitcoin's 0x80 — this chain folds its address version into
//!   the secret-key version (base58.h:410 SetData(128 + PUBKEY_ADDRESS)).
//! - dumpwallet line: `<WIF> <epoch> label=<escaped> # addr=<address>` and
//!   `<WIF> <epoch> change=1 # addr=<address>` / `reserve=1` variants.
//!   Comment lines start with '#'. importwallet parses exactly this shape
//!   (rpcdump.cpp:141-228), including `\xNN` escapes from EncodeDumpString.
//!
//! Write safety mirrors wallet.rs rails: import opens read-write (auto
//! .tri7bak backup on first write, BEGIN IMMEDIATE per key), dump opens
//! read-only. Encrypted wallets are refused (tri7 policy).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;

use crate::address::{base58, base58_decode};
use crate::legacy::sha256d;
use crate::wallet::{read_wallet, WalletKey, WalletKeys};

/// mainnet secret-key version byte: 128 + PUBKEY_ADDRESS(65) (base58.h:410).
pub const WIF_VERSION: u8 = 193; // 0xC1

// ---------------------------------------------------------------------------
// WIF codec
// ---------------------------------------------------------------------------

fn base58check(data: &[u8]) -> String {
    let mut full = Vec::with_capacity(data.len() + 4);
    full.extend_from_slice(data);
    full.extend_from_slice(&sha256d(data)[..4]);
    base58(&full)
}

/// Legacy CTrianglesSecret::ToString — WIF for a 32-byte secret.
/// `compressed` appends the 0x01 suffix (base58.h:411-412).
pub fn wif_encode(secret: &[u8; 32], compressed: bool) -> String {
    let mut data = Vec::with_capacity(34);
    data.push(WIF_VERSION);
    data.extend_from_slice(secret);
    if compressed {
        data.push(1);
    }
    base58check(&data)
}

/// Inverse of wif_encode — validates the version byte and checksum.
pub fn wif_decode(wif: &str) -> Result<([u8; 32], bool), String> {
    let full = base58_decode(wif)?;
    // version(1) + secret(32) [+ compressed flag(1)] + checksum(4)
    if full.len() != 38 && full.len() != 37 {
        return Err(format!(
            "WIF decoded length {} (expected 38 or 37)",
            full.len()
        ));
    }
    let expect = &sha256d(&full[..full.len() - 4])[..4];
    if &full[full.len() - 4..] != expect {
        return Err("WIF: bad base58check checksum".into());
    }
    if full[0] != WIF_VERSION {
        return Err(format!(
            "WIF version byte {:#04x} != {:#04x} (mainnet secret)",
            full[0], WIF_VERSION
        ));
    }
    let compressed = full.len() == 38;
    if compressed && full[33] != 1 {
        return Err(format!("WIF: compressed suffix byte {} != 1", full[33]));
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&full[1..33]);
    Ok((secret, compressed))
}

/// Sanity: the secret must hash to the claimed address (never import/export
/// a key whose WIF/label pair disagrees).
pub fn secret_to_address(secret: &[u8; 32], compressed: bool) -> Result<String, String> {
    let secp = secp256k1::Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(secret).map_err(|e| format!("bad secret: {}", e))?;
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    let pk33 = if compressed {
        pk.serialize().to_vec()
    } else {
        pk.serialize_uncompressed().to_vec()
    };
    Ok(crate::wallet::pubkey_to_address(&pk33))
}

// ---------------------------------------------------------------------------
// dumpwallet
// ---------------------------------------------------------------------------

/// Legacy EncodeDumpString: plain printable text passes through; anything
/// outside 0x20..=0x7e (plus '\\' and '"') becomes \xNN; backslash doubled.
pub fn encode_dump_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(*b as char),
            _ => out.push_str(&format!("\\x{:02x}", b)),
        }
    }
    out
}

/// Legacy DecodeDumpString (inverse).
pub fn decode_dump_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                    continue;
                }
                b'x' if i + 3 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).unwrap_or("");
                    if let Ok(v) = u8::from_str_radix(hex, 16) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One dumpwallet row, legacy field order.
#[derive(Debug)]
pub struct DumpRow {
    pub wif: String,
    pub created: i64,
    /// label=... / change=1 / reserve=1 classification. tri7 has no keypool,
    /// so reserve never occurs; keys without a label export as change=1
    /// (the legacy bucket for unlabeled keys).
    pub tag: String,
    pub address: String,
}

/// Dump every key in the wallet (read-only) to legacy-format lines.
/// Sorted by creation time then address, matching the legacy dump order.
pub fn dump_wallet(wallet_path: &Path) -> Result<(Vec<DumpRow>, usize), String> {
    let wk: WalletKeys = read_wallet(wallet_path)?;
    if wk.encrypted {
        return Err(format!(
            "{}: encrypted wallet — tri7 policy refuses key export while the keystore is encrypted",
            wallet_path.display()
        ));
    }
    let labels: BTreeMap<String, String> = wk.names.iter().cloned().collect();
    let mut rows: Vec<DumpRow> = wk
        .keys
        .iter()
        .map(|k| DumpRow {
            tag: match labels.get(&k.address) {
                Some(l) if !l.is_empty() => format!("label={}", encode_dump_string(l)),
                _ => "change=1".to_string(),
            },
            wif: wif_encode(&k.secret, k.compressed),
            created: k.created.unwrap_or(0),
            address: k.address.clone(),
        })
        .collect();
    rows.sort_by(|a, b| (a.created, &a.address).cmp(&(b.created, &b.address)));
    let n = rows.len();
    Ok((rows, n))
}

/// Render the legacy dumpwallet comment header + rows to a writer.
/// `best_block` is (height, hash) when the caller has chain state (tri7d
/// tools do; a bare export passes None and the header omits the line).
pub fn write_dump<W: Write>(w: &mut W, rows: &[DumpRow], best_block: Option<(u64, &str)>) -> std::io::Result<()> {
    writeln!(w, "# Wallet dump created by Triangles 7.0.0 (tri7)")?;
    writeln!(w, "# * Created on {}", chrono_epoch())?;
    if let Some((h, hash)) = best_block {
        writeln!(w, "# * Best block at time of backup was {} ({})", h, hash)?;
    }
    writeln!(w)?;
    for r in rows {
        writeln!(w, "{} {} {} # addr={}", r.wif, r.created, r.tag, r.address)?;
    }
    Ok(())
}

fn chrono_epoch() -> String {
    // EncodeDumpTime = asctime UTC; build without external crates.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = now / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = ((now / 3600) % 24, (now / 60) % 60, now % 60);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, hh, mm, ss)
}

/// Inverse of civil_from_days (Hinnant): days since 1970-01-01 from a
/// proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as u64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

/// Parse the legacy client's ISO-8601 UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`
/// (Triangles v5.5.5.0 dumpwallet writes TimeString, not epoch). Strict about
/// the SHAPE (exactly 20 bytes, fixed separators) and the RANGES (month
/// 1-12, day 1-31, hour <=23, min <=59, sec <=60) but DELIBERATELY PERMISSIVE
/// about per-month calendar validity: `created` is wallet metadata only
/// (keymeta is stamped at import), so an impossible day such as 2021-02-30
/// normalizes harmlessly in the epoch arithmetic instead of failing the line.
/// Anything else is None and the line is skipped, exactly like a non-epoch
/// field was before.
fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hh, mm, ss) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + hh * 3_600 + mm * 60 + ss)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// importwallet
// ---------------------------------------------------------------------------

pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Parse a legacy dumpwallet file into (wif, created, label) triples —
/// rpcdump.cpp:141-228 line grammar: split on whitespace; field 0 = WIF,
/// field 1 = epoch; later fields: `label=...` (label until '#'),
/// `change=1`/`reserve=1` (no label write), `#...` ends the line.
pub fn parse_dump_lines<R: BufRead>(r: R) -> Result<Vec<(String, i64, Option<String>)>, String> {
    let mut out = Vec::new();
    for (ln, line) in r.lines().enumerate() {
        let line = line.map_err(|e| format!("read line {}: {}", ln + 1, e))?;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 2 {
            continue;
        }
        let created = match toks[1].parse::<i64>() {
            Ok(t) => t,
            Err(_) => match parse_iso8601_utc(toks[1]) {
                Some(t) => t,
                // Neither epoch nor ISO-8601: not a dump key line we can
                // interpret (the old client never wrote anything else).
                None => continue,
            },
        };
        let mut label = None;
        let mut labeled = false;
        for tok in toks.iter().skip(2) {
            if tok.starts_with('#') {
                break;
            }
            if *tok == "change=1" || *tok == "reserve=1" {
                labeled = false;
            }
            if let Some(l) = tok.strip_prefix("label=") {
                label = Some(decode_dump_string(l));
                labeled = true;
            }
        }
        if !labeled {
            label = None;
        }
        out.push((toks[0].to_string(), created, label));
    }
    Ok(out)
}

/// Import keys from a dumpwallet file into the wallet at `wallet_path`.
/// Uses WalletHandle::open_readwrite (auto-backup, per-key IMMEDIATE tx,
/// duplicate = skip-not-overwrite — add_key errors on existing keys and we
/// count that as "skipped", matching the legacy printf behaviour).
/// Compressed-form WIFs only: an uncompressed-form WIF maps the secret to a
/// DIFFERENT address (hash160 of the 65-byte pubkey) and tri7's write path
/// stores compressed key rows only — import refuses it loudly rather than
/// binding the secret to the wrong address (dumpwallet export keeps the
/// flag faithful, so the secret stays recoverable from the dump file).
/// NOTE (documented divergence): keymeta create-time is stamped at import
/// (wallet.rs add_key contract); the dump's epoch is preserved only in the
/// written label/name row choice. Rewriting keymeta would require touching
/// wallet.rs, which a sibling task owns.
pub fn import_wallet(wallet_path: &Path, dump_path: &Path) -> Result<ImportReport, String> {
    let file = std::fs::File::open(dump_path).map_err(|e| format!("open {}: {}", dump_path.display(), e))?;
    let entries = parse_dump_lines(std::io::BufReader::new(file))?;
    let mut rep = ImportReport {
        imported: 0,
        skipped: 0,
        failed: 0,
        errors: Vec::new(),
    };
    // Group by key so each wallet key imports exactly once even when the
    // dump carries the same WIF twice.
    let mut seen = std::collections::HashSet::new();
    let mut handle = crate::wallet::WalletHandle::open_readwrite(wallet_path)?;
    for (wif, _created, label) in entries {
        if !seen.insert(wif.clone()) {
            rep.skipped += 1;
            continue;
        }
        let (secret, compressed) = match wif_decode(&wif) {
            Ok(v) => v,
            Err(e) => {
                rep.failed += 1;
                rep.errors.push(format!("skip {}: {}", &wif[..wif.len().min(12)], e));
                continue;
            }
        };
        // tri7's wallet write path stores compressed key rows only (wallet.rs
        // add_key derives the 33-byte pubkey). An uncompressed-form WIF has a
        // DIFFERENT address (hash160 of the 65-byte pubkey) — importing it
        // compressed would silently bind the secret to the wrong address.
        // Refuse loudly instead; the secret is still recoverable from the
        // dump file (and dumpwallet exports preserve the flag faithfully).
        if !compressed {
            rep.failed += 1;
            rep.errors.push(format!(
                "skip {}: uncompressed-form WIF — tri7's wallet write path stores compressed key rows only; the secret cannot be bound to its uncompressed address",
                &wif[..wif.len().min(12)]
            ));
            continue;
        }
        let addr = match secret_to_address(&secret, true) {
            Ok(a) => a,
            Err(e) => {
                rep.failed += 1;
                rep.errors.push(e);
                continue;
            }
        };
        match handle.add_key(&secret, label.as_deref().unwrap_or("")) {
            Ok(_) => rep.imported += 1,
            Err(e) if e.contains("already exists") => rep.skipped += 1,
            Err(e) => {
                rep.failed += 1;
                rep.errors.push(format!("{}: {}", addr, e));
            }
        }
    }
    handle.close();
    Ok(rep)
}

// ---------------------------------------------------------------------------
// checkwallet
// ---------------------------------------------------------------------------

/// Legacy checkwallet parity (rpcwallet.cpp checkwallet): verify the
/// container opens, SQLite integrity_check passes, and the key rows
/// re-derive (secret → pubkey → address) consistently. Repair advice is
/// out of scope for tri7 (the legacy repairwallet scans for orphaned
/// BDB records that cannot exist in the SQLite format).
pub struct CheckReport {
    pub integrity: String,
    pub keys: usize,
    pub names: usize,
    pub encrypted: bool,
    pub bad_keys: usize,
    pub total_rows: i64,
    pub wallet_version: Option<i32>,
}

impl CheckReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "integrity": self.integrity,
            "keys": self.keys,
            "names": self.names,
            "encrypted": self.encrypted,
            "bad_keys": self.bad_keys,
            "total_rows": self.total_rows,
            "wallet_version": self.wallet_version,
        })
    }
}

pub fn check_wallet(wallet_path: &Path) -> Result<CheckReport, String> {
    let wk = read_wallet(wallet_path)?;
    let conn = rusqlite::Connection::open_with_flags(
        wallet_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("open {}: {}", wallet_path.display(), e))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("integrity_check: {}", e))?;
    let mut bad_keys = 0usize;
    for k in &wk.keys {
        // The wallet loader already parsed the DER; re-derive the address
        // from the secret to prove the row is internally consistent.
        // compressed=true is valid under the compressed-only store invariant
        // (tri7's write path always derives the 33-byte pubkey); rows the
        // loader marked uncompressed are checked against their own form.
        let ok = if k.compressed {
            matches!(secret_to_address(&k.secret, true), Ok(ref a) if *a == k.address)
        } else {
            matches!(secret_to_address(&k.secret, false), Ok(ref a) if *a == k.address)
        };
        if !ok {
            bad_keys += 1;
        }
    }
    let fk_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM main", [], |r| r.get(0))
        .map_err(|e| format!("count: {}", e))?;
    Ok(CheckReport {
        integrity,
        keys: wk.keys.len(),
        names: wk.names.len(),
        encrypted: wk.encrypted,
        bad_keys,
        total_rows: fk_rows,
        wallet_version: wk.version,
    })
}

/// Plain struct (named to avoid exporting serde_json::Value as Value_).

/// dumpprivkey parity: one address's WIF from the wallet (read-only).
pub fn dump_privkey(wallet_path: &Path, address: &str) -> Result<String, String> {
    let wk = read_wallet(wallet_path)?;
    if wk.encrypted {
        return Err("encrypted wallet — key export refused".into());
    }
    let k: Option<&WalletKey> = wk.keys.iter().find(|k| k.address == address);
    match k {
        Some(k) => Ok(wif_encode(&k.secret, k.compressed)),
        None => Err(format!(
            "address {} not found in wallet {}",
            address,
            wallet_path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Independently derived vector (python base58check, version 193):
    /// compressed WIF for secret 000102..1f.
    const SECRET: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const WIF_COMPRESSED: &str = "VYuhntikpDe3pvxHTMadvdKcsT6eKKrBR8wZfxN1ej6car9cznqH";

    #[test]
    fn wif_matches_independent_vector_and_roundtrips() {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&hex(SECRET));
        let wif = wif_encode(&secret, true);
        assert_eq!(wif, WIF_COMPRESSED, "WIF encoding must match vector");
        let (back, compressed) = wif_decode(&wif).expect("decode");
        assert_eq!(back, secret);
        assert!(compressed);
        // uncompressed form roundtrips too (and differs — 0x01 suffix)
        let wif_u = wif_encode(&secret, false);
        assert_ne!(wif_u, wif);
        let (back_u, comp_u) = wif_decode(&wif_u).unwrap();
        assert_eq!(back_u, secret);
        assert!(!comp_u);
        // corruption is caught: flip a payload byte, checksum must fail
        let mut chars: Vec<char> = wif.chars().collect();
        let last = chars.pop().unwrap();
        chars.push(if last == 'z' { 'y' } else { 'z' });
        let corrupted: String = chars.into_iter().collect();
        assert!(wif_decode(&corrupted).is_err(), "corrupted WIF must fail");
        // wrong version (bitcoin 0x80) is rejected, not silently accepted
        let mut data = vec![0x80u8];
        data.extend_from_slice(&secret);
        data.push(1);
        let btc_wif = base58check(&data);
        assert!(wif_decode(&btc_wif).is_err(), "bitcoin-flavor WIF must be rejected");
    }

    #[test]
    fn dump_string_escapes_roundtrip() {
        assert_eq!(encode_dump_string("plain-text_1"), "plain-text_1");
        assert_eq!(encode_dump_string("a\\b"), "a\\\\b");
        assert_eq!(encode_dump_string("x\u{00e9}y"), "x\\xc3\\xa9y");
        assert_eq!(decode_dump_string(&encode_dump_string("a\\b\"c\u{00e9}\u{0001}")), "a\\b\"c\u{00e9}\u{0001}");
        assert_eq!(decode_dump_string("plain"), "plain");
    }

    #[test]
    fn parse_dump_lines_grammar_matches_rpcdump() {
        let body = "\
# Wallet dump created by Triangles
Td1fJetqDSFIBtVvAt4RDDbnR9RFweJKxyTB9inKGBbiB5dSFxuG 1700000000 label=Main # addr=TQ8..
SecondNotARealWifButLongEnoughToBeIgnoredByParser? no  # bad epoch skipped
Tq123456789012345678901234567890123456789012345678901234567890abc 1700000001 change=1 # addr=T..
Tq999999999999999999999999999999999999999999999999999999999999xyz 1700000002 reserve=1 # addr=T..
garbage
";
        let got = parse_dump_lines(body.as_bytes()).unwrap();
        assert_eq!(got.len(), 3, "comment/garbage/bad-epoch lines skipped");
        assert_eq!(got[0].1, 1700000000);
        assert_eq!(got[0].2.as_deref(), Some("Main"));
        assert_eq!(got[1].2, None, "change=1 clears the label");
        assert_eq!(got[2].2, None, "reserve=1 clears the label");
    }

    #[test]
    fn parse_dump_lines_accepts_iso8601_created() {
        // Verbatim grammar of a Triangles v5.5.5.0 dumpwallet (the 2026-04
        // dump this fix was written against): ISO-8601 TimeString in field 1,
        // `label=` with an empty value, and `change=1` variants.
        let body = "\
# Wallet dump created by Triangles v5.5.5.0 (Apr  4 2026, 08:28:42)
Vx1x2x3x4x5x6x7x8x9x0xAxBxCxDxExFxGxHxJxKxLxMxN 2021-02-17T23:26:09Z label= # addr=TH7U7iH884zZ99FG7zyh2B3wMUHisFtUHX
Vy1y2y3y4y5y6y7y8y9y0yAyByCyDyEyFyGyHyJyKyLyMyN 2021-02-17T23:26:09Z change=1 # addr=TPw5Z7neRj2B9hySevsxht7MJUEC6wz8u8
Vz1z2z3z4z5z6z7z8z9z0zAzBzCzDzEzFzGzHzJzKzLzMzN 2026-04-05T06:08:40Z label=Mining # addr=TBxrQx34J2mRvkmixnrdUynwzEC3wPdgb3
Vbad 2021-13-45T99:99:99Z label=nope # addr=TNotARealAddress
Vbad2 not-a-timestamp label=nope # addr=TNotARealAddress
";
        let got = parse_dump_lines(body.as_bytes()).unwrap();
        assert_eq!(got.len(), 3, "malformed timestamps skipped, ISO ones kept");
        assert_eq!(got[0].1, 1_613_604_369, "2021-02-17T23:26:09Z epoch");
        assert_eq!(got[0].2.as_deref(), Some(""), "label= means an empty label");
        assert_eq!(got[1].1, 1_613_604_369);
        assert_eq!(got[1].2, None, "change=1 clears the label");
        assert_eq!(got[2].2.as_deref(), Some("Mining"));
    }

    #[test]
    fn iso8601_utc_known_vectors_and_rejections() {
        assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_utc("2021-02-17T23:26:09Z"), Some(1_613_604_369));
        assert_eq!(parse_iso8601_utc("2026-11-06T23:26:09Z"), Some(1_794_007_569));
        // date -> days -> date identity across a leap-year boundary
        let (y, m, d) = civil_from_days(days_from_civil(2000, 2, 29));
        assert_eq!((y, m, d), (2000, 2, 29));
        assert_eq!(parse_iso8601_utc("2021-13-01T00:00:00Z"), None, "month 13");
        // Day validity is checked 1..=31 only: created is wallet METADATA
        // (keymeta is stamped at import anyway), so an impossible calendar
        // day like Feb 30 normalizes harmlessly in the epoch arithmetic
        // instead of being a parse error.
        assert_eq!(parse_iso8601_utc("2021-02-30T00:00:00Z"), Some(1_614_643_200));
        assert_eq!(parse_iso8601_utc("2021-02-17 23:26:09"), None, "missing T/Z separators");
        assert_eq!(parse_iso8601_utc("1700000000"), None, "epoch is not ISO; the epoch branch handles it");
    }

    /// Build a synthetic wallet file through the REAL write path (same
    /// pattern as wallet.rs tests) — never touch Sami's real wallet here.
    /// open_readwrite NEVER creates a file (by design), so the empty
    /// TRIw container is created first with rusqlite.
    fn make_wallet(dir: &Path, keys: &[([u8; 32], &str)]) -> std::path::PathBuf {
        let p = dir.join("wallet.dat");
        {
            let conn = rusqlite::Connection::open(&p).expect("create container");
            conn.execute_batch(
                "PRAGMA application_id = 1414687095; -- 0x54526977 'TRIw'
                 PRAGMA user_version = 1;
                 CREATE TABLE main (key BLOB, value BLOB);",
            )
            .expect("schema");
        }
        let mut h = crate::wallet::WalletHandle::open_readwrite(&p).expect("open rw");
        for (sec, label) in keys {
            h.add_key(sec, label).expect("add key");
        }
        h.close();
        p
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tri7-keytool-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dump_then_import_roundtrip_into_fresh_wallet() {
        let dir = tmpdir("roundtrip");
        let sec_a = [1u8; 32];
        let sec_b = [2u8; 32];
        let wallet = make_wallet(&dir, &[(sec_a, "Main"), (sec_b, "")]);
        // dump
        let (rows, n) = dump_wallet(&wallet).expect("dump");
        assert_eq!(n, 2);
        let dump_path = dir.join("dump.txt");
        let mut f = std::fs::File::create(&dump_path).unwrap();
        write_dump(&mut f, &rows, Some((2_204_064, "ab".repeat(32).as_str()))).unwrap();
        drop(f);
        let text = std::fs::read_to_string(&dump_path).unwrap();
        assert!(text.starts_with("# Wallet dump created by Triangles"));
        assert!(text.contains("Best block at time of backup was 2204064"));
        // import into a FRESH wallet file (open_readwrite never creates)
        let target = dir.join("target.dat");
        {
            let conn = rusqlite::Connection::open(&target).unwrap();
            conn.execute_batch(
                "PRAGMA application_id = 1414687095; -- 'TRIw'
                 PRAGMA user_version = 1;
                 CREATE TABLE main (key BLOB, value BLOB);",
            )
            .unwrap();
        }
        let rep = import_wallet(&target, &dump_path).expect("import");
        assert_eq!(rep.imported, 2, "both keys land");
        assert_eq!(rep.failed, 0);
        // labels carried: 'Main' labeled key, unlabeled key
        let wk = read_wallet(&target).unwrap();
        assert_eq!(wk.keys.len(), 2);
        let labels: std::collections::HashMap<String, String> = wk.names.iter().cloned().collect();
        let addr_a = secret_to_address(&sec_a, true).unwrap();
        assert_eq!(labels.get(&addr_a).map(String::as_str), Some("Main"));
        // the unlabeled wallet key carries no REAL label ("" name row or none)
        let addr_b = secret_to_address(&sec_b, true).unwrap();
        assert!(
            matches!(labels.get(&addr_b).map(String::as_str), None | Some("")),
            "unlabeled key must not gain a real label: {:?}",
            labels.get(&addr_b)
        );
        // legacy-parity observable: the unlabeled key exported as change=1
        let row_b = rows.iter().find(|r| r.address == addr_b).expect("row for b");
        assert_eq!(row_b.tag, "change=1");
        // funds identical: same secrets landed
        assert!(wk.keys.iter().any(|k| k.secret == sec_a));
        assert!(wk.keys.iter().any(|k| k.secret == sec_b));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_is_idempotent_and_counts_skips() {
        let dir = tmpdir("idem");
        let sec = [9u8; 32];
        let wallet = make_wallet(&dir, &[(sec, "Keep")]);
        let (rows, _) = dump_wallet(&wallet).unwrap();
        let dump_path = dir.join("dump.txt");
        {
            let mut f = std::fs::File::create(&dump_path).unwrap();
            write_dump(&mut f, &rows, None).unwrap();
        }
        let rep = import_wallet(&wallet, &dump_path).expect("import into same wallet");
        assert_eq!(rep.imported, 0, "existing key is skipped, never overwritten");
        assert_eq!(rep.skipped, 1);
        // re-import into the same wallet keeps exactly one copy
        let wk = read_wallet(&wallet).unwrap();
        assert_eq!(wk.keys.iter().filter(|k| k.secret == sec).count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dump_privkey_and_checkwallet() {
        let dir = tmpdir("privcheck");
        let sec = [0x42u8; 32];
        let wallet = make_wallet(&dir, &[(sec, "")]);
        let addr = secret_to_address(&sec, true).unwrap();
        let wif = dump_privkey(&wallet, &addr).expect("dumpprivkey");
        let (back, _) = wif_decode(&wif).unwrap();
        assert_eq!(back, sec);
        assert!(dump_privkey(&wallet, "TNotAWalletAddress000000000000000000").is_err());
        let chk = check_wallet(&wallet).expect("check");
        assert_eq!(chk.integrity, "ok");
        assert_eq!(chk.keys, 1);
        assert_eq!(chk.bad_keys, 0);
        assert!(!chk.encrypted);
        // garbage file is refused loudly
        let bad = dir.join("garbage.dat");
        std::fs::write(&bad, b"not a wallet at all").unwrap();
        assert!(check_wallet(&bad).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn uncompressed_wif_import_is_refused_loudly() {
        // An uncompressed-form WIF maps the secret to a DIFFERENT address
        // (hash160 of the 65-byte pubkey). tri7's write path is
        // compressed-only, so import must refuse — never bind the secret to
        // the compressed address silently.
        let dir = tmpdir("uncomp");
        let p = dir.join("w.dat");
        {
            let conn = rusqlite::Connection::open(&p).unwrap();
            conn.execute_batch(
                "PRAGMA application_id = 1414687095; -- 'TRIw'
                 PRAGMA user_version = 1;
                 CREATE TABLE main (key BLOB, value BLOB);",
            )
            .unwrap();
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&hex(SECRET));
        let dump_path = dir.join("u.txt");
        std::fs::write(
            &dump_path,
            format!("{} 1700000000 change=1 # addr=uncompressed\n", wif_encode(&secret, false)),
        )
        .unwrap();
        let rep = import_wallet(&p, &dump_path).expect("container opens");
        assert_eq!(rep.imported, 0, "nothing silently imported");
        assert_eq!(rep.failed, 1);
        assert!(
            rep.errors[0].contains("uncompressed-form WIF"),
            "refusal names the divergence: {:?}",
            rep.errors
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encrypted_wallet_refuses_export() {
        let dir = tmpdir("enc");
        let p = dir.join("enc.dat");
        // Build a REAL encrypted-shaped container the way the loader reads
        // it: application_id 'TRIw', user_version 1, one `main(key,value)`
        // table carrying an mkey record (read_wallet ⇒ encrypted=true).
        {
            let conn = rusqlite::Connection::open(&p).unwrap();
            conn.execute_batch(
                "PRAGMA application_id = 1414687095; -- 0x54526977 'TRIw'
                 PRAGMA user_version = 1;
                 CREATE TABLE main (key BLOB, value BLOB);
                 INSERT INTO main VALUES (x'046d6b6579', x'0000000000000000'); -- <04>'mkey'
                 ",
            )
            .unwrap();
        }
        assert!(read_wallet(&p).is_err(), "loader refuses the encrypted container outright");
        let err = dump_wallet(&p).expect_err("dump must refuse encrypted wallets");
        assert!(err.contains("encrypted"), "refusal names the policy: {}", err);
        let err2 = dump_privkey(&p, "Tx").expect_err("dumpprivkey must refuse");
        assert!(err2.contains("encrypted"));
        // import path: open_readwrite refuses encrypted containers outright
        let dump_path = dir.join("d.txt");
        std::fs::write(&dump_path, "# nothing\n").unwrap();
        assert!(import_wallet(&p, &dump_path).is_err(), "import must refuse");
        // nonexistent paths error loudly before anything else
        assert!(dump_wallet(&dir.join("nope.dat")).is_err());
        assert!(dump_privkey(&dir.join("nope.dat"), "Tx").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
