//! wallet_from_seed — create a wallet file from a BIP39 24-word phrase.
//!
//! usage:
//!   wallet_from_seed <new-wallet.dat> --generate [count]
//!       generate a fresh 24-word phrase (printed to stdout ONCE, at the end,
//!       so the addresses appear first and the phrase can be redirected to a
//!       protected file without mixing streams)
//!   wallet_from_seed <new-wallet.dat> --mnemonic "<24 words>" [count]
//!       rebuild from an existing phrase; addresses must byte-match the ones
//!       the original run printed (roundtrip check)
//!
//! Derived keys use the wallet's own HD continuation path
//! (seed::derive_triangles, m/44'/2222'/0'/0/i, empty passphrase), i in
//! 0..count — the same convention sweep.rs uses for destinations.

use tri7_core::seed;
use tri7_core::wallet::{create_wallet_file, WalletHandle};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!(
            "usage: wallet_from_seed <new-wallet.dat> --generate [count]\n       wallet_from_seed <new-wallet.dat> --mnemonic \"<24 words>\" [count]"
        );
        std::process::exit(2);
    }
    let path = std::path::Path::new(&a[1]);
    if path.exists() {
        eprintln!("refusing to overwrite {}", path.display());
        std::process::exit(1);
    }
    let generating = a[2] == "--generate";
    let (mnemonic, count) = if generating {
        let n: usize = a.get(3).and_then(|x| x.parse().ok()).unwrap_or(10);
        match seed::generate_mnemonic_24() {
            Ok(m) => (m, n),
            Err(e) => {
                eprintln!("mnemonic generation failed: {e}");
                std::process::exit(1);
            }
        }
    } else if a[2] == "--mnemonic" {
        let raw = a.get(3).cloned().unwrap_or_default();
        let m = seed::normalize_words(&raw).unwrap_or(raw);
        if !seed::check_mnemonic(&m) {
            eprintln!("invalid mnemonic (word membership or checksum)");
            std::process::exit(1);
        }
        (m, a.get(4).and_then(|x| x.parse().ok()).unwrap_or(10))
    } else {
        eprintln!("expected --generate or --mnemonic");
        std::process::exit(2);
    };

    create_wallet_file(path).expect("create wallet file");
    let mut wh = WalletHandle::open_readwrite(path).expect("open wallet");
    for i in 0..count as u32 {
        let s = seed::derive_triangles(&mnemonic, "", 0, 0, i).expect("derive key");
        let k = wh.add_key(&s, &format!("seed/{i}")).expect("add key");
        println!("addr {i}: {}", k.address);
    }
    wh.close();
    println!("wrote {count} seed-derived keys to {}", path.display());
    if generating {
        println!("\nMNEMONIC (24 words - store securely, shown once):\n{mnemonic}");
    }
}
