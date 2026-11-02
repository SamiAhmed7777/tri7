//! Milestone 2: verify PoS block signatures across the live chain archive.
//!   PoS → ECDSA(Hash9 header) must verify against vtx[1].vout[1] bare pubkey
//!   PoW → signature must be empty
//! Exercises classification + Hash9 + secp256k1 together on real chain bytes.
//!
//! Usage: verify_sigs [blk_file] [start_height] [end_height] [--allow-stale]
//! Exit 0 ONLY if: every block in range passes its check, end_height was
//! reached, and at least one signature was verified. A stale-branch
//! discontinuity fails unless --allow-stale is passed (archive artifact).

use tri7_core::blkfile::BlkFileReader;
use tri7_core::consensus;
use tri7_core::legacy::hash_to_display;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/.triangles/blk0001.dat".to_string());
    let start_height: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2201000);
    let end_height: usize = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(start_height + 200);
    let allow_stale = args.iter().any(|a| a == "--allow-stale");

    let mut rdr = BlkFileReader::open(&path).expect("open blk file");
    let mut height = 0usize;
    let mut prev: Option<[u8; 32]> = None;
    let mut sig_checked = 0usize;
    let mut pos_blocks = 0usize;
    let mut pow_blocks = 0usize;
    let mut stale_at: Option<usize> = None;

    loop {
        let frame = match rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                eprintln!("frame error at {}: {}", height, e);
                std::process::exit(1);
            }
        };
        let block = match frame.block() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("parse error at {}: {}", height, e);
                std::process::exit(1);
            }
        };
        if height == 0 {
            if block.prev_hash != [0u8; 32] {
                eprintln!("genesis prev not zero");
                std::process::exit(1);
            }
        } else if block.prev_hash != prev.unwrap() {
            stale_at = Some(height);
            break;
        }

        if height >= start_height {
            let is_pos = consensus::block_is_proof_of_stake(&block);
            if is_pos {
                pos_blocks += 1;
                if let Err(e) = consensus::check_pos_block_signature(&block) {
                    eprintln!(
                        "SIG FAIL at height {} hash {}: {}",
                        height,
                        hash_to_display(&block.hash()),
                        e
                    );
                    std::process::exit(1);
                }
                sig_checked += 1;
            } else {
                pow_blocks += 1;
                if let Err(e) = consensus::check_pow_block_signature(&block) {
                    eprintln!("POW-SIG FAIL at height {}: {}", height, e);
                    std::process::exit(1);
                }
            }
            if sig_checked > 0 && sig_checked % 50_000 == 0 {
                println!(
                    "... {} signatures verified (height {})",
                    sig_checked, height
                );
            }
        }

        prev = Some(block.hash());
        height += 1;
        if height > end_height {
            break;
        }
    }

    let covered_to = height.saturating_sub(1); // last fully processed height
    let reached = covered_to >= end_height;
    // Coverage contract: zero sigs or premature EOF/stale-stop = failure;
    // --allow-stale only forgives an archive stale-branch discontinuity.
    if sig_checked == 0 {
        eprintln!(
            "FAIL: zero signatures checked over heights {}..{} — no coverage",
            start_height, height
        );
        std::process::exit(1);
    }
    if let Some(h) = stale_at {
        if !allow_stale {
            eprintln!(
                "FAIL: stale-branch discontinuity at height {} before end_height {} (use --allow-stale)",
                h, end_height
            );
            std::process::exit(1);
        }
    } else if !reached {
        eprintln!(
            "FAIL: EOF at covered height {} before end_height {} — incomplete coverage",
            covered_to, end_height
        );
        std::process::exit(1);
    }

    println!(
        "verified: {} signatures over {} PoS + {} PoW blocks (heights {}..{})",
        sig_checked, pos_blocks, pow_blocks, start_height, height
    );
    println!("ALL SIGNATURE CHECKS PASSED");
}
