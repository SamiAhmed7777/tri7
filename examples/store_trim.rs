//! store_trim — truncate a block store at a given height.
//!
//! Use case (real incident): a store copied from another node carries STALE
//! tail frames from a branch the chain later abandoned. Backfill then reads a
//! body whose prev-link doesn't match the validated chain and the daemon
//! exits 4 ("backfill: link failure"). Trimming at the first bad height lets
//! the daemon refetch that range from peers.
//!
//! Usage: store_trim <blk_path> <height> [--dry-run]
//!
//! Steps: build the index (cached), look up height→offset, truncate the file
//! at that offset. The caller should delete the index cache afterwards so the
//! next build threads only the surviving frames.

use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: store_trim <blk_path> <height> [--dry-run]");
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let height: u64 = match args[2].parse() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("height parse: {}", e);
            std::process::exit(2);
        }
    };
    let dry = args.iter().any(|a| a == "--dry-run");
    if !blk.exists() {
        eprintln!("store not found: {}", blk.display());
        std::process::exit(2);
    }
    let len_before = std::fs::metadata(&blk).map(|m| m.len()).unwrap_or(0);
    let cache = blk.parent().unwrap_or(std::path::Path::new(".")).join("trim_index");
    let store = tri7_core::blockindex::BlockStore::new(Some(blk.clone()), Some(cache));
    // wait for the index build (bounded)
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        match store.status() {
            tri7_core::blockindex::StoreStatus::Ready { frames, heights_ok, .. } => {
                eprintln!("index ready: {} frames, heights_ok={}", frames, heights_ok);
                break;
            }
            tri7_core::blockindex::StoreStatus::Failed(e) => {
                eprintln!("index build failed: {}", e);
                std::process::exit(1);
            }
            _ => {
                if Instant::now() > deadline {
                    eprintln!("index build timed out");
                    std::process::exit(1);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    let offset = match store.frame_offset(height) {
        Some(o) => o,
        None => {
            eprintln!("height {} not covered by the index", height);
            std::process::exit(1);
        }
    };
    println!(
        "blk={} height={} offset={} size={} trim_to={} ({} bytes removed)",
        blk.display(),
        height,
        offset,
        len_before,
        offset,
        len_before.saturating_sub(offset)
    );
    if dry {
        println!("dry-run: no changes written");
        return;
    }
    match std::fs::OpenOptions::new().write(true).open(&blk) {
        Ok(f) => {
            if let Err(e) = f.set_len(offset) {
                eprintln!("truncate failed: {}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("open for write failed: {}", e);
            std::process::exit(1);
        }
    }
    println!("trimmed. delete the index cache dir before restarting the daemon.");
}
