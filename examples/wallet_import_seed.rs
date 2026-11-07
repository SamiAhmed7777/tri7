//! wallet_import_seed — import a BIP39 phrase's HD keys into an EXISTING
//! wallet file (the wallet keeps every key it already has; the derived set is
//! added alongside them).
//!
//! usage:
//!   wallet_import_seed <existing-wallet.dat> --mnemonic "<24 words>" [count]
//!
//! Same derivation as wallet_from_seed / sweep destinations:
//! seed::derive_triangles (m/44'/2222'/0'/0/i, empty passphrase).

use tri7_core::seed;
use tri7_core::wallet::WalletHandle;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: wallet_import_seed <existing-wallet.dat> --mnemonic \"<24 words>\" [count]");
        std::process::exit(2);
    }
    let raw = a[3].clone();
    let m = seed::normalize_words(&raw).unwrap_or(raw);
    if !seed::check_mnemonic(&m) {
        eprintln!("invalid mnemonic (word membership or checksum)");
        std::process::exit(1);
    }
    let count: usize = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(10);
    let mut wh = WalletHandle::open_readwrite(std::path::Path::new(&a[1]))
        .expect("open wallet (read-write)");
    let mut added = 0usize;
    let mut skipped = 0usize;
    for i in 0..count as u32 {
        let s = seed::derive_triangles(&m, "", 0, 0, i).expect("derive key");
        match wh.add_key(&s, &format!("seed/{i}")) {
            Ok(k) => {
                println!("addr {i}: {} (imported)", k.address);
                added += 1;
            }
            Err(e) if e.contains("already exists") => {
                println!("addr {i}: (already in wallet — skipped)");
                skipped += 1;
            }
            Err(e) => {
                eprintln!("index {i}: {e}");
                std::process::exit(1);
            }
        }
    }
    wh.close();
    println!("imported {added}, already held {skipped}");
}
