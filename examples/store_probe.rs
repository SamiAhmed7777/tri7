//! THROWAWAY diagnostic (not part of the deliverable): prove the from-zero
//! store paths — (1) an EMPTY file builds Ready, (2) a MID-CHAIN file (first
//! frame is not genesis) anchors via the oracle window and threads heights.
use tri7_core::blockindex::{BlockStore, BuildOracle, StoreStatus};
use tri7_core::legacy::hash9;
use std::time::Duration;

fn oracle(base: u64, hashes: Vec<[u8; 32]>, tip: u64) -> BuildOracle {
    BuildOracle { base, hashes, tip }
}

fn dummy_block(prev: &[u8; 32], time_diff: u32) -> Vec<u8> {
    // minimal fake frame body: version+prev+merkle+time+bits+nonce+1 coinbase
    let mut b = Vec::new();
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(prev);
    b.extend_from_slice(&[0u8; 32]); // merkle
    b.extend_from_slice(&1_700_000_000u32.wrapping_add(time_diff).to_le_bytes());
    b.extend_from_slice(&0x207fffffu32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.push(1); // tx count
    b.extend_from_slice(&1u32.to_le_bytes()); // vin count
    b.extend_from_slice(&[0u8; 32]); // null prev
    b.extend_from_slice(&0xffffffffu32.to_le_bytes());
    b.push(4);
    b.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    b.extend_from_slice(&0xffffffffu32.to_le_bytes()); // sequence
    b.push(1); // vout count
    b.extend_from_slice(&50u64.to_le_bytes()); // value
    b.push(0); // empty script
    b.extend_from_slice(&0u32.to_le_bytes()); // locktime
    b
}

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    std::fs::create_dir_all(format!("{}/index", dir)).unwrap();
    let blk = format!("{}/blk0001.dat", dir);

    // --- case 1: empty file (from-zero fresh start) ---
    std::fs::File::create(&blk).unwrap();
    let store = BlockStore::new(Some(blk.clone().into()), Some(format!("{}/index", dir).into()));
    store.spawn_build(Some(oracle(0, vec![[0u8; 32]], 0)));
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(300));
        if let StoreStatus::Ready { frames, heights_ok, .. } = store.status() {
            println!("case1 empty file: Ready frames={} heights_ok={}", frames, heights_ok);
            break;
        }
    }
    drop(store);

    // --- case 2: mid-chain file: 3 frames, NO genesis frame; the oracle
    // claims the first frame is h100 (real Hash9s so in-window checks pass) ---
    std::fs::remove_file(&blk).unwrap();
    let mut prev = [0x11u8; 32];
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut real_hashes: Vec<[u8; 32]> = Vec::new();
    for i in 0..3u32 {
        let body = dummy_block(&prev, i * 120);
        real_hashes.push(hash9(&body[..80]));
        let mut frame = Vec::new();
        frame.extend_from_slice(&0x0522_3570u32.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&body);
        frames.push(frame);
        prev = real_hashes.last().unwrap().clone();
    }
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&blk).unwrap();
        for fr in &frames {
            f.write_all(fr).unwrap();
        }
    }
    let store = BlockStore::new(Some(blk.clone().into()), Some(format!("{}/index2", dir).into()));
    store.spawn_build(Some(oracle(100, real_hashes, 102)));
    let mut last = StoreStatus::Building { frames_done: 0 };
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(300));
        last = store.status();
        match &last {
            StoreStatus::Ready { frames, heights_ok, .. } => {
                println!("case2 mid-chain file: Ready frames={} heights_ok={}", frames, heights_ok);
                break;
            }
            StoreStatus::Failed(e) => {
                println!("case2 mid-chain file: FAILED: {}", e);
                break;
            }
            _ => {}
        }
    }
    if let StoreStatus::Building { .. } = last {
        println!("case2: still building (TIMEOUT)");
    }
}
