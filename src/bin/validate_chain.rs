//! Milestone 1 validation: parse the real chain's blk0001.dat, verify the
//! genesis block hash against the legacy client's compiled-in constant, verify
//! merkle roots, and verify the first N blocks link + hash correctly.
//!
//! Exit codes: 0 = all requested blocks validated, 1 = any verification
//! failure or parse error (hard failures abort the run).

use tri7_core::blkfile::BlkFileReader;
use tri7_core::legacy::hash_to_display;

/// hashGenesisBlockOfficial from triangles_v5/src/main.h
const GENESIS_OFFICIAL: &str = "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021";

fn fail(msg: &str) -> ! {
    eprintln!("FAIL: {}", msg);
    std::process::exit(1);
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/root/.triangles/blk0001.dat".to_string());
    let n_want: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let mut rdr = BlkFileReader::open(&path).unwrap_or_else(|e| fail(&format!("open: {}", e)));
    // --whole-file: archive scan mode where clean EOF before n_want is success
    // (blk archives end at the tip; reorged branches already aborted the run
    // via the link check). Default is strict: EOF before n_want is a failure.
    let whole_file = std::env::args().any(|a| a == "--whole-file");
    let mut height = 0usize;
    let mut prev: Option<[u8; 32]> = None;

    loop {
        let frame = match rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => fail(&format!("frame error at height {}: {}", height, e)),
        };
        let block = match frame.block() {
            Ok(b) => b,
            Err(e) => fail(&format!("parse error at height {}: {}", height, e)),
        };

        // Linkage check: genesis prev MUST be all-zero; every later block
        // prev MUST equal the running tip.
        if height == 0 {
            if block.prev_hash != [0u8; 32] {
                fail("genesis prev_hash is not zero");
            }
        } else if block.prev_hash != prev.unwrap() {
            fail(&format!(
                "link failure at height {}: block prev {} != chain tip {}",
                height,
                hash_to_display(&block.prev_hash),
                hash_to_display(&prev.unwrap())
            ));
        }

        let h = block.hash();
        let mr = block.merkle_root();

        if height == 0 {
            let disp = hash_to_display(&h);
            println!("genesis hash      = {}", disp);
            println!("expected          = {}", GENESIS_OFFICIAL);
            if disp != GENESIS_OFFICIAL {
                fail("genesis hash mismatch");
            }
            println!("genesis MATCH");
            if mr != block.merkle_root {
                fail("genesis merkle mismatch");
            }
            println!("genesis merkle MATCH");
        } else if height < 5 || height % 100 == 0 {
            println!(
                "block {:>7}  hash {}  txs {}  merkle-ok {}",
                height,
                hash_to_display(&h),
                block.vtx.len(),
                mr == block.merkle_root
            );
        }
        if mr != block.merkle_root {
            fail(&format!(
                "merkle mismatch at height {}: computed {} != header {}",
                height,
                hash_to_display(&mr),
                hash_to_display(&block.merkle_root)
            ));
        }

        prev = Some(h);
        height += 1;
        if height >= n_want {
            println!("reached requested height {}; chain valid so far", height);
            std::process::exit(0);
        }
    }
    println!("total blocks validated: {}", height);
    if height < n_want && !whole_file {
        fail(&format!(
            "EOF at {} blocks before explicitly requested {} (use --whole-file for archive scans)",
            height, n_want
        ));
    }
    std::process::exit(0);
}
