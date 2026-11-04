//! THROWAWAY diagnostic (not part of the deliverable): reproduce the
//! from-zero pre-sync BlockStore build on an EMPTY blocks file.
use tri7_core::blockindex::{BlockStore, BuildOracle};
use std::time::Duration;

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    std::fs::create_dir_all(format!("{}/index", dir)).unwrap();
    let blk = format!("{}/blk0001.dat", dir);
    if !std::path::Path::new(&blk).exists() {
        std::fs::File::create(&blk).unwrap();
    }
    println!("blk file: {} bytes", std::fs::metadata(&blk).unwrap().len());
    let store = BlockStore::new(Some(blk.clone().into()), Some(format!("{}/index", dir).into()));
    let oracle = BuildOracle {
        base: 12925,
        hashes: (0u64..4600).map(|i| {
            let mut h = [0u8; 32];
            h[0..8].copy_from_slice(&i.to_le_bytes());
            h
        }).collect(),
        tip: 17524,
    };
    store.spawn_build(Some(oracle));
    for i in 0..10 {
        std::thread::sleep(Duration::from_secs(1));
        println!("t={}s status={:?}", i + 1, store.status());
        if let tri7_core::blockindex::StoreStatus::Ready { .. } = store.status() {
            break;
        }
    }
}
