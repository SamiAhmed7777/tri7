//! Where do blocks sit in the blocks FILE, and is the canonical lineage below
//! the window base even stored? (fleet item 44, RPC-HEIGHTMAP-TRUTH)
//!
//! READ-ONLY: the block index is built in memory with NO cache dir. Walks the
//! canonical lineage BACK from the window's first frame (tipstate entry 0)
//! through stored parents, printing the frame offset (= file order) every few
//! steps, and prints the frame offset of every block hash given on the
//! command line.
//!
//!   cargo run --release --example chain_order -- \
//!       /var/lib/tri7/blk0001.dat /var/lib/tri7 <window-base-height> <steps> [hash ...]

use std::path::PathBuf;
use std::time::Duration;

use tri7_core::blockindex::{BlockStore, StoreStatus};
use tri7_core::p2p::{display_to_internal, internal_to_display};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: chain_order <blocks-file> <state-dir> <window-base-height> <steps> [hash ...]");
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let state_dir = PathBuf::from(&args[2]);
    let base: u64 = args[3].parse().expect("window base height");
    let steps: u64 = args[4].parse().expect("steps");

    let store = BlockStore::new(Some(blk), None);
    store.spawn_build(None);
    loop {
        match store.status() {
            StoreStatus::Ready { .. } | StoreStatus::Failed(_) => break,
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if let StoreStatus::Failed(e) = store.status() {
        eprintln!("build FAILED — {e}");
        std::process::exit(1);
    }

    let raw = std::fs::read_to_string(state_dir.join("tipstate.json")).expect("tipstate.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("tipstate json");
    let e0 = &v["entries"][0];
    let e0_h = e0["height"].as_u64().expect("entries[0].height");
    println!(
        "== CHAIN ORDER == window first entry: h{e0_h} {}",
        e0["hash"].as_str().unwrap_or("?")
    );
    let mut probes: Vec<(String, String)> = vec![("window-first".into(), e0["hash"].as_str().expect("hash").into())];
    for a in &args[5..] {
        probes.push((a.clone(), a.clone()));
    }
    for (label, disp) in &probes {
        let key = display_to_internal(disp).expect("display hash");
        match store.block_by_hash(&key) {
            Ok(Some(h)) => println!("{label}: stored at frame offset {}", h.off),
            Ok(None) => println!("{label}: NOT in the store"),
            Err(e) => println!("{label}: lookup error {e}"),
        }
    }

    // canonical lineage: parent hops from the window's first frame
    let mut cur = display_to_internal(e0["hash"].as_str().expect("hash")).expect("hash");
    let mut h = base;
    let mut missing = 0u64;
    for i in 0..=steps {
        let Some(hit) = store.block_by_hash(&cur).unwrap_or(None) else {
            println!("canonical h{h} {}: NOT in the store (missing bodies start here)", internal_to_display(&cur));
            missing += 1;
            // cannot walk further without the parent
            break;
        };
        if i <= 3 || i % 250 == 0 || i == steps {
            println!("canonical h{h}: frame offset {}", hit.off);
        }
        if h == 0 {
            break;
        }
        cur = hit.block.prev_hash;
        h -= 1;
    }
    let _ = missing;
}
