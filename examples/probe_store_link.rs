//! THROWAWAY diagnostic: find the first height where the disk store's chain
//! stops linking (the balances replay stops with "stale branch" there).
//!   cargo run --release --example probe_store_link -- <blk.dat> <from> <to>
use tri7_core::blockindex::BlockStore;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_default();
    let from: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let to: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let store = BlockStore::new(Some(std::path::PathBuf::from(path.clone())), std::env::args().nth(4).map(std::path::PathBuf::from));
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
    let mut prev_hash: Option<[u8; 32]> = None;
    let mut holes = 0usize;
    for h in from..=to {
        match store.block_by_height(h) {
            Ok(Some(hit)) => {
                let hash: [u8; 32] = hit.hash.into();
                if let Some(p) = prev_hash {
                    let want: [u8; 32] = hit.block.prev_hash.into();
                    if p != want {
                        println!(
                            "LINK BREAK at h{}: this block's prev_hash {} != block(h-1) hash {}",
                            h,
                            hex(&want),
                            hex(&p)
                        );
                        println!("  block(h) hash = {}", hex(&hash));
                        break;
                    }
                }
                prev_hash = Some(hash);
            }
            Ok(None) => {
                holes += 1;
                println!("MISSING body at h{}", h);
                if holes > 5 {
                    break;
                }
            }
            Err(e) => {
                println!("ERROR at h{}: {}", h, e);
                break;
            }
        }
    }
    println!("done");
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}
