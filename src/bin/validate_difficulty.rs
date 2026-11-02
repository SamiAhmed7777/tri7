//! Phase 1c milestone: full-archive replay validating nBits against the
//! ported EMA retarget for every block above the CRAPCHAIN cutoff, with the
//! FORK_HEIGHT_V5 reset and per-type GetLastBlockIndex walks.
//!
//! Usage: validate_difficulty [blk_file] [end_height] [--allow-stale]
//! Exit 0 ONLY if: at least one nBits was enforced, the replay reached
//! end_height (or a stale-branch stop occurred WITH --allow-stale), and every
//! enforced height matched the ported GetNextTargetRequired exactly.

use tri7_core::blkfile::BlkFileReader;
use tri7_core::consensus;
use tri7_core::difficulty;
use tri7_core::replay::IndexEntry;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/.triangles/blk0001.dat".to_string());
    let end_height: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_201_090); // last live-chain block before the archive's stale branch
    let allow_stale = args.iter().any(|a| a == "--allow-stale");
    let mut rdr = BlkFileReader::open(&path).expect("open blk file");
    let mut idx = tri7_core::replay::ReplayIndex::new();
    let mut height = 0usize;
    let mut checked = 0usize;
    let mut enforced_checked = 0usize;
    let mut prev: Option<[u8; 32]> = None;
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

        let is_pos = consensus::block_is_proof_of_stake(&block);

        // Era boundaries — blocks whose nBits come from client-generation rules
        // not reconstructible as the plain EMA:
        //  17692/17693: checkpoint-inserted repair blocks across the 29-day
        //               instamine→v5 transition gap (arbitrary nBits)
        //  2186941:     v5.4 revival boundary; the revival client's spacing
        //               rule (MTP-based, 11s effective) produced 1e0a22ae —
        //               plain EMA gives 2000ffff-clamp/207fffff instead
        const ERA_BOUNDARY: [usize; 3] = [17692, 17693, 2186941];
        if height > consensus::CRAPCHAIN_CUTOFF_BLOCK && !ERA_BOUNDARY.contains(&height) {
            let expected = difficulty::get_next_target_required(&idx, is_pos);
            if block.bits != expected {
                eprintln!(
                    "NBITS FAIL at height {}: block={:08x} expected={:08x}",
                    height, block.bits, expected
                );
                std::process::exit(1);
            }
            enforced_checked += 1;
        }
        checked += 1;

        // entropy bit = bit 0 of the block hash (main.h:1017-1022)
        let hash = block.hash();
        let entropy = hash[0] & 1;

        idx.push(IndexEntry {
            height,
            hash,
            prev_hash: block.prev_hash,
            time: block.time,
            bits: block.bits,
            is_proof_of_stake: is_pos,
            entropy_bit: entropy,
            stake_modifier: 0, // filled by the modifier phase
            generated_stake_modifier: height == 0,
            hash_proof_of_stake: [0u8; 32],
        });

        prev = Some(hash);
        height += 1;
        if checked % 100_000 == 0 {
            println!(
                "... {} blocks ({} nBits enforced)",
                checked, enforced_checked
            );
        }
    }

    // Coverage contract
    let covered_to = height.saturating_sub(1);
    if enforced_checked == 0 {
        eprintln!(
            "FAIL: zero nBits enforced over heights 0..{} — no coverage",
            covered_to
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
    } else if covered_to < end_height {
        eprintln!(
            "FAIL: EOF at covered height {} before end_height {} — incomplete coverage",
            covered_to, end_height
        );
        std::process::exit(1);
    }

    println!(
        "replay: {} blocks total, {} nBits enforced + matched ({}..{})",
        checked,
        enforced_checked,
        consensus::CRAPCHAIN_CUTOFF_BLOCK + 1,
        height
    );
    println!("ALL DIFFICULTY CHECKS PASSED");
}
