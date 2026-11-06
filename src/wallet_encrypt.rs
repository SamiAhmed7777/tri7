//! Legacy wallet encryption, implemented from the legacy SOURCE (no fixture
//! needed — the source is the specification).
//!
//! Spec, read out of the legacy tree:
//!   crypter.h  CMasterKey  = { vchCryptedKey, vchSalt, nDerivationMethod,
//!                             nDeriveIterations, vchOtherDerivationParameters }
//!              serialized in exactly that order, vectors as
//!              CompactSize+bytes, ints as VARINT.
//!   crypter.cpp SetKeyFromPassphrase: method 0 = EVP_BytesToKey(AES-256-CBC,
//!              SHA-512, salt, passphrase, nDeriveIterations).
//!   wallet.cpp EncryptWallet: 32-byte random master key; salt 8 random bytes;
//!              iterations tuned to ~2.5 s with a 25000 floor (25000 default);
//!              mkey.encrypted_key = AES(master key) under the derived pair.
//!   keystore.cpp:133 EncryptSecret(master, key.GetSecret(fCompressed),
//!              vchPubKey.GetHash(), out) — so the ckey plaintext is EXACTLY the
//!              32-byte secret (no checksum, unlike Bitcoin), and the IV is
//!              SHA256d(pubkey)[0..16].
//!   walletdb.h WriteCryptedKey: key = ("ckey", pubkey.Raw()); the plaintext
//!              "key" row is ERASED.
//!   Validation is not a checksum: a wrong passphrase yields a wrong master key,
//!              so the decrypted secret no longer derives the record's pubkey.
//!              That check is what proves the passphrase.

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::walletcrypto::{decrypt, derive_key_iv, encrypt, KEY_SIZE};

pub const DEFAULT_ITERATIONS: u32 = 25_000;

pub struct EncryptReport {
    pub keys: usize,
    pub mnemonic: bool,
    pub iterations: u32,
}

fn sha256d(data: &[u8]) -> [u8; 32] {
    let a = Sha256::digest(data);
    let b = Sha256::digest(a);
    let mut out = [0u8; 32];
    out.copy_from_slice(&b);
    out
}

/// Bitcoin CompactSize / VARINT.
fn push_cs(out: &mut Vec<u8>, n: usize) {
    let n = n as u64;
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
}

/// Derive the AES IV for a key from its public key: SHA256d(pubkey)[0..16].
pub fn iv_for_pubkey(pubkey: &[u8]) -> [u8; 16] {
    let h = sha256d(pubkey);
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&h[..16]);
    iv
}

/// Serialize a CMasterKey value (order matters — see the module comment).
pub fn encode_master_key(crypted_key: &[u8], salt: &[u8], method: u32, iterations: u32) -> Vec<u8> {
    let mut v = Vec::new();
    push_cs(&mut v, crypted_key.len());
    v.extend_from_slice(crypted_key);
    push_cs(&mut v, salt.len());
    v.extend_from_slice(salt);
    // serialize.h: Serialize(Stream&, unsigned int) is WRITEDATA — a raw
    // 4-byte little-endian int, NOT a CompactSize and NOT a 7-bit varint.
    v.extend_from_slice(&method.to_le_bytes());
    v.extend_from_slice(&iterations.to_le_bytes());
    push_cs(&mut v, 0); // vchOtherDerivationParameters
    v
}

/// Parse the same structure. Returns (crypted_key, salt, method, iterations).
pub fn decode_master_key(v: &[u8]) -> Result<(Vec<u8>, Vec<u8>, u32, u32), String> {
    let mut p = 0usize;
    let n = read_cs(v, &mut p)? as usize;
    if v.len() < p + n {
        return Err("mkey: crypted key overruns".into());
    }
    let ck = v[p..p + n].to_vec();
    p += n;
    let sn = read_cs(v, &mut p)? as usize;
    if v.len() < p + sn {
        return Err("mkey: salt overruns".into());
    }
    let salt = v[p..p + sn].to_vec();
    p += sn;
    if v.len() < p + 8 {
        return Err("mkey: derivation fields truncated".into());
    }
    let method = u32::from_le_bytes(v[p..p + 4].try_into().unwrap());
    p += 4;
    let iterations = u32::from_le_bytes(v[p..p + 4].try_into().unwrap());
    p += 4;
    Ok((ck, salt, method, iterations))
}

fn read_cs(v: &[u8], p: &mut usize) -> Result<u64, String> {
    let b = *v.get(*p).ok_or("varint: eof")? as usize;
    *p += 1;
    match b {
        0..=0xfc => Ok(b as u64),
        0xfd => {
            let s = v.get(*p..*p + 2).ok_or("varint: eof")?;
            *p += 2;
            Ok(u16::from_le_bytes([s[0], s[1]]) as u64)
        }
        0xfe => {
            let s = v.get(*p..*p + 4).ok_or("varint: eof")?;
            *p += 4;
            Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64)
        }
        _ => {
            let s = v.get(*p..*p + 8).ok_or("varint: eof")?;
            *p += 8;
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            Ok(u64::from_le_bytes(a))
        }
    }
}

fn key_blob(section: &str, suffix_cs: &[u8]) -> Vec<u8> {
    let mut k = Vec::new();
    push_cs(&mut k, section.len());
    k.extend_from_slice(section.as_bytes());
    k.extend_from_slice(suffix_cs);
    k
}

fn pubkey_suffix(pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    push_cs(&mut s, pubkey.len());
    s.extend_from_slice(pubkey);
    s
}

/// Encrypt every plaintext `key` row in place: write `ckey`, erase `key`.
pub fn encrypt_wallet(path: &std::path::Path, passphrase: &str) -> Result<EncryptReport, String> {
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    // refuse if already encrypted. The prefix must be compared as a BLOB: a
    // TEXT literal never matches a BLOB column under SQLite's type rules, which
    // made the first version of this guard dead code.
    let mut mkey_prefix = Vec::new();
    push_cs(&mut mkey_prefix, 4);
    mkey_prefix.extend_from_slice(b"mkey");
    let already: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM main WHERE substr(key,1,5) = ?1",
            params![mkey_prefix],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if already > 0 {
        return Err("wallet is already encrypted".into());
    }

    let mut secret: [u8; 32] = [0u8; 32];
    getrandom::getrandom(&mut secret).map_err(|e| format!("entropy: {e}"))?;
    let mut salt = [0u8; 8];
    getrandom::getrandom(&mut salt).map_err(|e| format!("entropy: {e}"))?;
    let iterations = DEFAULT_ITERATIONS;
    let (dk, div) = derive_key_iv(passphrase, &salt, iterations);
    let crypted_master = encrypt(&dk, &div, &secret);
    let mkey_value = encode_master_key(&crypted_master, &salt, 0, iterations);

    // mkey row key: CS("mkey") + 4-byte id (id 1)
    let mut mk = Vec::new();
    push_cs(&mut mk, 4);
    mk.extend_from_slice(b"mkey");
    mk.extend_from_slice(&1u32.to_le_bytes());

    let mut rows: Vec<(Vec<u8>, Vec<u8>)> = Vec::new(); // (pubkey33, der value)
    {
        let mut st = conn
            .prepare("SELECT key, value FROM main")
            .map_err(|e| e.to_string())?;
        let mut q = st.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = q.next().map_err(|e| e.to_string())? {
            let k: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            let v: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            // CS(3) "key" + CS(33) pubkey
            if k.len() > 8 && k[0] == 3 && &k[1..4] == b"key" {
                let pl = k[4] as usize;
                if k.len() >= 5 + pl && pl == 33 {
                    rows.push((k[5..5 + pl].to_vec(), v));
                }
            }
        }
    }
    if rows.is_empty() {
        return Err("no plaintext key rows found".into());
    }

    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let secp = secp256k1::Secp256k1::signing_only();
    let mut done = 0usize;
    let mut failed = 0usize;
    for (pubkey, der) in &rows {
        // DER EC private key: pull the OCTET STRING (04 20 <32>) and PROVE it by
        // re-deriving the public key.
        let Some(sec) = extract_der_secret(der) else {
            failed += 1;
            continue;
        };
        let derived = match secp256k1::SecretKey::from_slice(&sec) {
            Ok(sk) => secp256k1::PublicKey::from_secret_key(&secp, &sk)
                .serialize()
                .to_vec(),
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        if &derived != pubkey {
            // never encrypt a key whose secret we cannot prove, and never
            // proceed as if it had been encrypted
            failed += 1;
            continue;
        }
        let iv = iv_for_pubkey(pubkey);
        let ct = encrypt(&secret, &iv, &sec);
        let mut val = Vec::new();
        push_cs(&mut val, ct.len());
        val.extend_from_slice(&ct);
        let ck = key_blob("ckey", &pubkey_suffix(pubkey));
        tx.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            params![ck, val],
        )
        .map_err(|e| e.to_string())?;
        let pk = key_blob("key", &pubkey_suffix(pubkey));
        tx.execute("DELETE FROM main WHERE key = ?1", params![pk])
            .map_err(|e| e.to_string())?;
        done += 1;
    }
    // HD mnemonic has no legacy equivalent; encrypt it with the same scheme so
    // it is not left in the clear next to encrypted keys.
    let mut mnemonic_done = false;
    {
        let mut mk2 = Vec::new();
        push_cs(&mut mk2, 10);
        mk2.extend_from_slice(b"hdmnemonic");
        if let Ok(v) = tx.query_row(
            "SELECT value FROM main WHERE key = ?1",
            params![mk2.clone()],
            |r| r.get::<_, Vec<u8>>(0),
        ) {
            // value = CS(len) + text
            if v.is_empty() {
                return Err("hdmnemonic row is empty".into());
            }
            let n = v[0] as usize;
            if v.len() >= 1 + n {
                let text = v[1..1 + n].to_vec();
                let iv = [0u8; 16];
                let ct = encrypt(&secret, &iv, &text);
                let mut val = Vec::new();
                push_cs(&mut val, ct.len());
                val.extend_from_slice(&ct);
                let mut hk = Vec::new();
                push_cs(&mut hk, 12);
                hk.extend_from_slice(b"hdcmnemonic");
                tx.execute(
                    "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
                    params![hk, val],
                )
                .ok();
                tx.execute("DELETE FROM main WHERE key = ?1", params![mk2]).ok();
                mnemonic_done = true;
            }
        }
    }
    if failed > 0 {
        // An mkey written while plaintext rows remain is the worst outcome:
        // the wallet would claim to be encrypted while some keys are not.
        drop(tx);
        return Err(format!(
            "{failed} of {} key rows could not be proven against their public key —              aborting without writing mkey (wallet left unchanged)",
            rows.len()
        ));
    }
    tx.execute(
        "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
        params![mk, mkey_value],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(EncryptReport {
        keys: done,
        mnemonic: mnemonic_done,
        iterations,
    })
}

fn extract_der_secret(der: &[u8]) -> Option<[u8; 32]> {
    let mut i = 0usize;
    while i + 34 <= der.len() {
        if der[i] == 0x04 && der[i + 1] == 0x20 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&der[i + 2..i + 34]);
            return Some(s);
        }
        i += 1;
    }
    None
}

/// Decrypt every ckey with a passphrase. Each secret is PROVEN against the
/// record's pubkey, so a wrong passphrase fails loudly instead of returning
/// garbage keys.
pub fn decrypt_wallet(
    path: &std::path::Path,
    passphrase: &str,
) -> Result<Vec<(Vec<u8>, [u8; 32])>, String> {
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    // find mkey
    let mut mk_k: Vec<u8> = Vec::new();
    push_cs(&mut mk_k, 4);
    mk_k.extend_from_slice(b"mkey");
    let mk_prefix = mk_k.clone();
    let mut st = conn
        .prepare("SELECT key, value FROM main WHERE substr(key,1,5) = ?1")
        .map_err(|e| e.to_string())?;
    let mut q = st.query(params![mk_prefix]).map_err(|e| e.to_string())?;
    let mut mkey_value: Option<Vec<u8>> = None;
    while let Some(row) = q.next().map_err(|e| e.to_string())? {
        let v: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        mkey_value = Some(v);
    }
    let mv = mkey_value.ok_or("wallet has no mkey row (not encrypted)")?;
    let (crypted_master, salt, method, iterations) = decode_master_key(&mv)?;
    if method != 0 {
        return Err(format!(
            "unsupported derivation method {method} (only EVP_BytesToKey/SHA-512)"
        ));
    }
    let (dk, div) = derive_key_iv(passphrase, &salt, iterations);
    let master = decrypt(&dk, &div, &crypted_master)
        .map_err(|_| "wrong passphrase (the master key did not decrypt)".to_string())?;
    if master.len() != KEY_SIZE {
        return Err(format!("mkey: master key is {} bytes", master.len()));
    }
    let mut master_key = [0u8; KEY_SIZE];
    master_key.copy_from_slice(&master);

    let mut out: Vec<(Vec<u8>, [u8; 32])> = Vec::new();
    let mut st2 = conn
        .prepare("SELECT key, value FROM main")
        .map_err(|e| e.to_string())?;
    let mut q2 = st2.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = q2.next().map_err(|e| e.to_string())? {
        let k: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        let v: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        if !(k.len() > 8 && k[0] == 4 && &k[1..5] == b"ckey") {
            continue;
        }
        let pl = k[5] as usize;
        if k.len() < 6 + pl || pl != 33 {
            continue;
        }
        let pubkey = k[6..6 + pl].to_vec();
        // value = CS(len) + ciphertext
        if v.is_empty() {
            continue;
        }
        let n = v[0] as usize;
        if v.len() < 1 + n {
            continue;
        }
        let iv = iv_for_pubkey(&pubkey);
        let sec = decrypt(&master_key, &iv, &v[1..1 + n])
            .map_err(|_| "wrong passphrase (a key did not decrypt)".to_string())?;
        if sec.len() != 32 {
            continue;
        }
        let mut s = [0u8; 32];
        s.copy_from_slice(&sec);
        let secp = secp256k1::Secp256k1::signing_only();
        let derived = match secp256k1::SecretKey::from_slice(&s) {
            Ok(sk) => secp256k1::PublicKey::from_secret_key(&secp, &sk)
                .serialize()
                .to_vec(),
            Err(_) => continue,
        };
        if derived != pubkey {
            return Err(
                "wrong passphrase (a decrypted key does not match its public key)".into(),
            );
        }
        out.push((pubkey, s));
    }
    if out.is_empty() {
        return Err("no ckey rows decrypted".into());
    }
    Ok(out)
}

/// True when the wallet carries mkey/ckey rows.
pub fn is_encrypted(path: &std::path::Path) -> bool {
    let Ok(conn) = Connection::open(path) else { return false };
    let mut pk = Vec::new();
    push_cs(&mut pk, 4);
    pk.extend_from_slice(b"mkey");
    conn.query_row(
        "SELECT COUNT(*) FROM main WHERE substr(key,1,5) = ?1",
        params![pk],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}


/// Re-encrypt the master key under a new passphrase (legacy ChangePassphrase).
/// The keys themselves are NOT re-encrypted: only the mkey row changes, exactly
/// as the legacy code does it.
pub fn change_passphrase(
    path: &std::path::Path,
    old_pass: &str,
    new_pass: &str,
) -> Result<u32, String> {
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut mk_k = Vec::new();
    push_cs(&mut mk_k, 4);
    mk_k.extend_from_slice(b"mkey");
    let mut st = conn
        .prepare("SELECT key, value FROM main WHERE substr(key,1,5) = ?1")
        .map_err(|e| e.to_string())?;
    let mut q = st.query(params![mk_k]).map_err(|e| e.to_string())?;
    let mut found: Option<(Vec<u8>, Vec<u8>)> = None;
    while let Some(row) = q.next().map_err(|e| e.to_string())? {
        let k: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        let v: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        found = Some((k, v));
    }
    let (rowkey, mv) = found.ok_or("wallet is not encrypted")?;
    let (crypted_master, salt, method, iterations) = decode_master_key(&mv)?;
    if method != 0 {
        return Err(format!("unsupported derivation method {method}"));
    }
    let (dk, div) = derive_key_iv(old_pass, &salt, iterations);
    let master = decrypt(&dk, &div, &crypted_master)
        .map_err(|_| "wrong passphrase (the master key did not decrypt)".to_string())?;

    // fresh salt + iterations, new passphrase, same master key
    let mut new_salt = [0u8; 8];
    getrandom::getrandom(&mut new_salt).map_err(|e| format!("entropy: {e}"))?;
    let (dk2, div2) = derive_key_iv(new_pass, &new_salt, iterations);
    let recrypted = encrypt(&dk2, &div2, &master);
    let new_value = encode_master_key(&recrypted, &new_salt, method, iterations);
    conn.execute(
        "UPDATE main SET value = ?1 WHERE key = ?2",
        params![new_value, rowkey],
    )
    .map_err(|e| e.to_string())?;

    // prove the new passphrase opens the wallet before returning
    let check = decrypt_wallet(path, new_pass)?;
    Ok(check.len() as u32)
}

/// Strip encryption: rewrite every ckey row as a plaintext "key" row and delete
/// mkey. Requires the passphrase and PROVES each secret against its pubkey.
pub fn decrypt_wallet_file(
    path: &std::path::Path,
    passphrase: &str,
) -> Result<usize, String> {
    let keys = decrypt_wallet(path, passphrase)?;
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut done = 0usize;
    for (pubkey, sec) in &keys {
        // write the plaintext key row in the legacy DER form tri7 uses
        let der = tri7_core_der(sec, pubkey)?;
        let pk = key_blob("key", &pubkey_suffix(pubkey));
        tx.execute(
            "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
            params![pk, der],
        )
        .map_err(|e| e.to_string())?;
        let ck = key_blob("ckey", &pubkey_suffix(pubkey));
        tx.execute("DELETE FROM main WHERE key = ?1", params![ck])
            .map_err(|e| e.to_string())?;
        done += 1;
    }
    let mut mk_k = Vec::new();
    push_cs(&mut mk_k, 4);
    mk_k.extend_from_slice(b"mkey");
    tx.execute("DELETE FROM main WHERE substr(key,1,5) = ?1", params![mk_k])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(done)
}

/// The legacy DER encoding of a private key (i2d_ECPrivateKey shape), which is
/// what tri7's plaintext rows hold: SEQUENCE { INTEGER 1, OCTET STRING <32>,
/// [0] curve OID, [1] BIT STRING pubkey }.
fn tri7_core_der(secret: &[u8; 32], pubkey: &[u8]) -> Result<Vec<u8>, String> {
    crate::wallet::encode_der_privkey(secret, pubkey)
}

/// Public keys of an encrypted wallet, for a node that starts LOCKED: it can
/// watch its addresses and keep syncing, but cannot sign until the passphrase
/// is supplied.
pub fn read_pubkeys(path: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut st = conn
        .prepare("SELECT key FROM main")
        .map_err(|e| e.to_string())?;
    let mut q = st.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = q.next().map_err(|e| e.to_string())? {
        let k: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        if k.len() > 8 && k[0] == 4 && &k[1..5] == b"ckey" {
            let pl = k[5] as usize;
            if k.len() >= 6 + pl && pl == 33 {
                out.push(k[6..6 + pl].to_vec());
            }
        }
    }
    Ok(out)
}
