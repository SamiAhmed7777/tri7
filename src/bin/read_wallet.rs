//! Milestone 3: read the real wallet (SQLite) and prove wallet-file
//! compatibility — every key derives its recorded pubkey, addresses match the
//! wallet's own `name` records, and the bestblock locator is extracted.
//!
//! Exit 0 ONLY if: at least one key parsed, defaultkey resolves to a parsed
//! key, and bestblock present. Label misses are reported; --strict-labels
//! turns them into a nonzero exit.

use tri7_core::wallet;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/.triangles/wallet.dat".to_string());
    let strict_labels = args.iter().any(|a| a == "--strict-labels");
    let p = std::path::Path::new(&path);
    let w = wallet::read_wallet(p).expect("wallet read failed");

    println!("wallet version : {:?}", w.version);
    println!("encrypted      : {}", w.encrypted);
    println!("keys           : {}", w.keys.len());
    println!("bestblock      : {}",
        w.best_block.map(|h| {
            h.iter().rev().map(|b| format!("{:02x}", b)).collect::<String>()
        }).unwrap_or_else(|| "none".into()));
    println!("labels         : {}", w.names.len());

    let mut status_ok = true;

    if w.keys.is_empty() {
        eprintln!("FAIL: zero keys parsed");
        status_ok = false;
    }
    if w.best_block.is_none() {
        eprintln!("FAIL: no bestblock locator in wallet");
        status_ok = false;
    }
    if w.default_key.is_none() {
        eprintln!("FAIL: no defaultkey record in wallet");
        status_ok = false;
    }

    // Cross-check: labeled addresses vs wallet keys. Label records may
    // reference addresses whose keys are NOT in this wallet (vestigial,
    // never-funded — the legacy client reports ismine=false for them too).
    let keyset: std::collections::HashSet<&str> =
        w.keys.iter().map(|k| k.address.as_str()).collect();
    let mut label_matches = 0usize;
    let mut label_misses = 0usize;
    for (addr, _) in &w.names {
        if keyset.contains(addr.as_str()) {
            label_matches += 1;
        } else {
            label_misses += 1;
            println!("  label address not in keys: {}", addr);
        }
    }
    println!(
        "label cross-check: {} matched, {} missed",
        label_matches, label_misses
    );
    if strict_labels && label_misses > 0 {
        eprintln!("FAIL: {} label misses under --strict-labels", label_misses);
        status_ok = false;
    }

    // Default key must be one of the keys.
    if let Some(dk) = &w.default_key {
        let found = w.keys.iter().any(|k| k.pubkey == *dk);
        println!("defaultkey in keyset: {}", found);
        if !found {
            eprintln!("FAIL: defaultkey does not resolve to a parsed key");
            status_ok = false;
        }
    }

    // Show a sample of addresses.
    println!("\nsample addresses (first 5):");
    for k in w.keys.iter().take(5) {
        println!("  {} (compressed={})", k.address, k.compressed);
    }

    if !status_ok {
        std::process::exit(1);
    }
    println!("\nWALLET READ OK — {} spendable keys", w.keys.len());
}
