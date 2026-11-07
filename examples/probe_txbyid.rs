// probe: does tx_by_id resolve the coin's parent on DNS2's store?
use tri7_core::blockindex::BlockStore;
fn main() {
    let store = BlockStore::new(
        Some(std::path::PathBuf::from("/var/lib/tri7/blk0001.dat")),
        Some(std::path::PathBuf::from("/var/lib/tri7/index")),
    );
    let ready = store.load_cache_only();
    println!("cache loaded: {}", ready);
    // internal-order prevout as it appears in tx.vin.prev_hash
    let internal: [u8; 32] = [
        0x97,0xf5,0x93,0x2d,0x4b,0x80,0x6a,0xa7,0xc6,0xab,0xd4,0xc2,0x09,0x02,0xcc,0x3a,
        0x34,0xbe,0x8b,0xd3,0xed,0x9d,0x2a,0xe0,0x8f,0xfb,0x21,0xd5,0x16,0x69,0xcc,0xef,
    ];
    match store.tx_by_id(&internal) {
        Ok(Some(hit)) => {
            println!("FOUND: vout count = {}", hit.tx.vout.len());
            for (i, o) in hit.tx.vout.iter().enumerate() {
                println!("  vout {}: {} sats, script {} bytes", i, o.value, o.script_pubkey.len());
            }
            println!("block_height: {:?}", hit.block_height);
            println!("block_time: {}", hit.block_time);
        }
        Ok(None) => println!("tx_by_id: None (not found)"),
        Err(e) => println!("tx_by_id ERR: {}", e),
    }
}
