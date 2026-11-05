//! wallet_newaddr — exercise the exact writer that the node's `getnewaddress`
//! RPC uses, against a wallet COPY, so the live wallet is never touched.
//!
//! Usage: wallet_newaddr <wallet_copy.dat> [label]

use std::path::PathBuf;
use tri7_core::wallet::{read_wallet, WalletHandle};

fn count(path: &PathBuf) -> usize {
    read_wallet(path).map(|w| w.keys.len()).unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: wallet_newaddr <wallet_copy.dat> [label]");
        std::process::exit(2);
    }
    let path = PathBuf::from(&args[1]);
    let label = args.get(2).cloned().unwrap_or_default();

    let before = count(&path);
    let mut wh = match WalletHandle::open_readwrite(&path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("open {}: {e}", path.display());
            std::process::exit(2);
        }
    };
    let hd = wh.hd_mnemonic.is_some();
    println!("wallet: {before} keys before, hd={hd}");

    let address = if hd {
        match wh.generate_hd_key(&label) {
            Ok(k) => k.address,
            Err(e) => {
                wh.close();
                eprintln!("generate_hd_key: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let mut secret = [0u8; 32];
        if let Err(e) = getrandom::getrandom(&mut secret) {
            wh.close();
            eprintln!("keygen entropy: {e}");
            std::process::exit(1);
        }
        match wh.add_key(&secret, &label) {
            Ok(k) => k.address,
            Err(e) => {
                wh.close();
                eprintln!("add_key: {e}");
                std::process::exit(1);
            }
        }
    };
    wh.close();

    // Re-read from disk: a key that only lived in memory would be missing.
    let after_keys = read_wallet(&path).map(|w| w.keys).unwrap_or_default();
    let persisted = after_keys.iter().any(|k| k.address == address);
    println!("new address: {address}");
    println!(
        "after reopen: {} keys (+{}), new key present: {persisted}",
        after_keys.len(),
        after_keys.len().saturating_sub(before)
    );
    if !persisted {
        std::process::exit(1);
    }
}
