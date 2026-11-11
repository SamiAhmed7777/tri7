//! THROWAWAY diagnostic: can the store serve the chain BACKWARD from the
//! tipstate window's first entry, using each block's prev_hash, all the way
//! down to the store's threaded tip? If yes, the "missing" bodies are
//! physically in the file and only the height map lost them.
//!
//!   cargo run --release --example probe_store_backwalk -- <blk.dat> <tipstate.json> <index_dir>

use tri7_core::blockindex::BlockStore;

fn hex_to_hash(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out: [u8; 32] = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn hash_hex(h: &[u8; 32]) -> String {
    let b: [u8; 32] = *h;
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let blk = args.get(1).cloned().unwrap_or_default();
    let tipstate = args.get(2).cloned().unwrap_or_default();
    let idx = args.get(3).cloned().unwrap_or_default();

    let text = std::fs::read_to_string(&tipstate).expect("tipstate");
    let st: serde_json::Value = serde_json::from_str(&text).expect("tipstate json");
    let entries = st["entries"].as_array().cloned().unwrap_or_default();
    let base = st["window_base"].as_u64().unwrap_or(0);
    println!("tipstate window_base: {}", base);
    let first = entries
        .iter()
        .filter(|e| e["height"].as_u64() == Some(base))
        .next()
        .cloned()
        .unwrap_or_default();
    let start_hash = hex_to_hash(first["hash"].as_str().unwrap_or("")).expect("window hash");
    println!("window first: h{} {}", base, hash_hex(&start_hash));

    let store = BlockStore::new(
        Some(std::path::PathBuf::from(blk)),
        Some(std::path::PathBuf::from(idx)),
    );
    store.spawn_build(None);
    loop {
        match store.status() {
            tri7_core::blockindex::StoreStatus::Ready { frames, .. } => {
                println!("index ready: {} frames", frames);
                break;
            }
            tri7_core::blockindex::StoreStatus::Failed(e) => {
                eprintln!("index build failed: {}", e);
                std::process::exit(2);
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }

    // walk backward: each block's prev_hash is the next block's hash
    let mut cur = start_hash;
    let mut height = base;
    let mut steps = 0u64;
    loop {
        let hit = match store.block_by_hash(&cur) {
            Ok(Some(h)) => h,
            Ok(None) => {
                println!("STOP at h{}: hash {} NOT in the store", height, hash_hex(&cur));
                break;
            }
            Err(e) => {
                println!("STOP at h{}: error {}", height, e);
                break;
            }
        };
        let prev: [u8; 32] = hit.block.prev_hash;
        // is this height already mapped in the height map?
        let mapped = store.hash_at_height(height).ok().flatten();
        if steps % 200 == 0 || mapped.is_none() {
            println!(
                "h{} {} mapped={:?}",
                height,
                &hash_hex(&cur)[..16],
                mapped.as_ref().map(|s| &s[..16])
            );
        }
        if height == 0 {
            break;
        }
        height -= 1;
        cur = prev;
        steps += 1;
        if steps > 20_000 {
            println!("stopped after {} steps", steps);
            break;
        }
    }
    println!("walked {} blocks back to h{}", steps, height);
}
