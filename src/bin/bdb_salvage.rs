//! bdb_salvage — recover keys from a Berkeley DB wallet that no installed BDB
//! library can open.
//!
//! Two of Sami's files (wallet - main.dat, wallet-OLD-CONFLICTED.dat) are btree
//! version 10, which is written by Berkeley DB 6.2. Only 5.3 is available here,
//! so `db_dump` refuses them ("unsupported btree version: 10"). Rather than
//! depend on Oracle's download, this scans the raw page bytes for the record
//! PAYLOAD, which does not change between btree versions:
//!
//!   SEQUENCE { INTEGER 1, OCTET STRING 32, [0] curve OID, [1] BIT STRING pubkey }
//!
//! The anchor is `04 20` + 32 secret bytes; the pubkey follows inside the same
//! DER blob. Every candidate is PROVEN: the public key is re-derived from the
//! secret and must appear in the surrounding bytes. A random `04 20` in a page
//! cannot survive that check, which is what makes scanning safe.
//!
//! Usage: bdb_salvage <bdb-file> <out-wallet.dat>

use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: bdb_salvage <bdb-file> <out-wallet.dat>");
        std::process::exit(2);
    }
    let src = Path::new(&a[1]);
    let dest = Path::new(&a[2]);
    let bytes = match std::fs::read(src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {e}", src.display());
            std::process::exit(1);
        }
    };
    eprintln!("scanned file: {} bytes", bytes.len());

    let secp = secp256k1::Secp256k1::signing_only();
    let mut found: Vec<[u8; 32]> = Vec::new();
    let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut candidates = 0usize;

    let mut i = 0usize;
    while i + 34 + 70 <= bytes.len() {
        if bytes[i] == 0x04 && bytes[i + 1] == 0x20 {
            let mut sec = [0u8; 32];
            sec.copy_from_slice(&bytes[i + 2..i + 34]);
            candidates += 1;
            if let Ok(sk) = secp256k1::SecretKey::from_slice(&sec) {
                let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
                let comp = pk.serialize();
                let uncomp = pk.serialize_uncompressed();
                // the pubkey must sit inside this DER blob (OID + BIT STRING
                // follow the octet string), not somewhere later in the file
                let window = &bytes[i + 34..(i + 34 + 90).min(bytes.len())];
                let matched = contains(window, &uncomp) || contains(window, &comp);
                if matched && seen.insert(comp.to_vec()) {
                    found.push(sec);
                }
            }
        }
        i += 1;
    }

    eprintln!(
        "candidates: {candidates}; proven key pairs: {}",
        found.len()
    );
    if found.is_empty() {
        eprintln!("nothing recovered — this file does not contain DER key records");
        std::process::exit(1);
    }

    if dest.exists() {
        eprintln!("refusing to overwrite {}", dest.display());
        std::process::exit(1);
    }
    use tri7_core::wallet::{create_wallet_file, WalletHandle};
    if let Err(e) = create_wallet_file(dest) {
        eprintln!("create {}: {e}", dest.display());
        std::process::exit(1);
    }
    let mut wh = match WalletHandle::open_readwrite(dest) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("open {}: {e}", dest.display());
            std::process::exit(1);
        }
    };
    let mut added = 0usize;
    for s in &found {
        match wh.add_key(s, "") {
            Ok(_) => added += 1,
            Err(e) => eprintln!("skip key: {e}"),
        }
    }
    wh.close();
    println!("recovered {added} keys into {}", dest.display());
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}
