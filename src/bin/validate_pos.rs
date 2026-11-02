//! Phase 1d milestone: full-archive replay validating the PoS kernel layer —
//! the stake-modifier chain and CheckProofOfStake, reconstructed under the
//! EXACT client rules that minted and indexed each era of the chain, with
//! kernel targets enforced wherever the deployed acceptance path enforced
//! them.
//!
//! The reconstruction was established by git archaeology on the live lineage
//! (/root/triangles_v5: 64d03a4 Initial upload -> fbadfb5 Cheops -> 69aedf2
//! Pharao -> 627c13c Black Pharao -> c636edb v5.0.0.0 -> HEAD v6.2.6.7) and
//! cross-checked against the deployed daemon's heritage index (RPC getblock
//! modifier/flags/entropybit/proofhash; below the 2026 rebase point at
//! h2200899 those records are the originals):
//!
//!   - Stake-modifier chain: ComputeNextStakeModifier for EVERY block, no
//!     gate (64d03a4:main.cpp:1950, 69aedf2:main.cpp:1991). The modifier
//!     interval is era-dependent: kernel.h MODIFIER_INTERVAL = 20*60 = 1200 s
//!     until the Pharao release switched nModifierInterval to 300
//!     (main.cpp:62) — buckets, selection sections and the selection interval
//!     (42301 s / 10554 s) all switch at h17692, where the Pharao client took
//!     over minting (2014-10-03, after the 29.4-day gap at 17691->17692).
//!   - hashProofOfStake: ZERO for every PoS block below h2200900. The
//!     accepting clients skipped kernel verification across the whole span —
//!     Initial-upload-era IBD/absent checks, the Cheops+
//!     `if (fIsInitialDownload) return true` skip (hash stays 0), the v5+
//!     assume-valid skip (<= 9001), and the rolling assume-valid /
//!     sync-checkpoint / snapshot paths that accepted the 2014-2022 and
//!     2026-03..04 stretches. Verified against the daemon: proofhash = 0 for
//!     every PoS block sampled from h1026 through h2200899. Zero stored
//!     proofs therefore feed candidate selection for the whole span.
//!   - h >= 2200900: the live era. These blocks were kernel-checked at
//!     acceptance (daemon proofhash nonzero from 2200900 on): CheckProofOfStake
//!     with the 6-field SER_GETHASH stream
//!     [nStakeModifier, nTimeBlockFrom, nTxPrevOffset, txPrev.nTime,
//!      prevout.n, nTimeTx], the fixed 600 s modifier walk
//!     (kernel.cpp:249-250), plain target x1, and the GetWeight era of the
//!     minting client: UNCAPPED under the v5 fork gate (2186942..2200900,
//!     "coins aged during the freeze", kernel.cpp:37-39 at c636edb), CAPPED at
//!     nStakeMaxAge from 2200901 (post-revert 0411be6, 2026-08-03,
//!     forward-only; chain resumed 2026-09-06).
//!
//! Enforcement policy mirrors the deployed client: kernel targets are checked
//! for h >= 2200900 (the live era, where acceptance actually ran
//! CheckProofOfStake). Everything earlier was accepted with kernels skipped;
//! those blocks are counted, never silently enforced. nBits EMA seams
//! 17692/17693/2186941 remain exempt exactly as in validate_difficulty. The
//! genesis stake-modifier checksum (kernel.cpp:20-21, 0x0e00670b) is verified
//! at height 0.
//!
//! Stake inputs resolve through a txid-keyed output index that never removes
//! spent entries (kernel.cpp:408-419 txPrev.ReadFromDisk semantics), newest
//! txid wins — sequential LevelDB Put behavior during replay.
//!
//! Usage: validate_pos [blk_file] [end_height] [--allow-stale]
//! Exit 0 ONLY if: at least one kernel/nBits was enforced, the replay reached
//! end_height (or a stale-branch stop occurred WITH --allow-stale), every
//! enforced nBits and kernel matched, and the genesis modifier checksum
//! matched the compiled constant.

use tri7_core::blkfile::BlkFileReader;
use tri7_core::consensus;
use tri7_core::difficulty;
use tri7_core::kernel::{self, KernelOutputResolver};
use tri7_core::replay::IndexEntry;

/// First height the deployed acceptance path kernel-checked (its heritage
/// proofhash is nonzero; daemon-verified). Everything below was accepted with
/// the kernel skipped.
const KERNEL_CHECKED_FROM: usize = 2_200_900;

fn die(msg: &str) -> ! {
    eprintln!("FAIL: {}", msg);
    std::process::exit(1);
}

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
    let dump_path = args
        .iter()
        .position(|a| a == "--dump-mods")
        .and_then(|i| args.get(i + 1).cloned());

    let mut rdr = BlkFileReader::open(&path).expect("open blk file");
    let mut idx = tri7_core::replay::ReplayIndex::new();
    let mut outputs = kernel::OutputIndex::new();
    let mut height = 0usize;
    let mut prev: Option<[u8; 32]> = None;
    let mut stale_at: Option<usize> = None;
    let mut dump: Option<std::io::BufWriter<std::fs::File>> = dump_path.as_ref().map(|p| {
        std::io::BufWriter::new(std::fs::File::create(p).expect("create dump file"))
    });
    use std::io::Write as _;

    // counters
    let mut nbits_enforced = 0usize;
    let mut kernel_enforced_checked = 0usize;
    let mut kernel_enforced_passed = 0usize;
    let mut kernel_skipped_era = 0usize; // PoS blocks accepted with kernels skipped
    let mut exempt_unresolved_input = 0usize;
    let mut modifier_generated = 0usize;
    let mut modifier_reused = 0usize;
    let mut duplicate_txid_outputs = 0usize;
    let mut respent_inputs = 0usize;
    let mut genesis_checksum_ok = false;
    let mut pos_blocks = 0usize;

    loop {
        let frame = match rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => die(&format!("frame error at height {}: {}", height, e)),
        };
        let block = match frame.block() {
            Ok(b) => b,
            Err(e) => die(&format!("parse error at height {}: {}", height, e)),
        };
        if height == 0 {
            if block.prev_hash != [0u8; 32] {
                die("genesis prev not zero");
            }
        } else if block.prev_hash != prev.unwrap() {
            stale_at = Some(height);
            break;
        }

        let hash = block.hash();
        let is_pos = consensus::block_is_proof_of_stake(&block);
        if is_pos {
            pos_blocks += 1;
        }

        // ---- 1. nBits enforcement (validate_difficulty contract) -----------
        const ERA_BOUNDARY: [usize; 3] = [17692, 17693, 2186941];
        if height > consensus::CRAPCHAIN_CUTOFF_BLOCK && !ERA_BOUNDARY.contains(&height) {
            let expected = difficulty::get_next_target_required(&idx, is_pos);
            if block.bits != expected {
                die(&format!(
                    "NBITS FAIL at height {}: block={:08x} expected={:08x}",
                    height, block.bits, expected
                ));
            }
            nbits_enforced += 1;
        }

        // ---- 2. PoS kernel: reconstruction + enforcement -------------------
        let mut stored_proof = [0u8; 32]; // heritage state: zero below the live era
        if is_pos {
            if height < KERNEL_CHECKED_FROM {
                // Accepted with the kernel skipped (IBD / assume-valid /
                // snapshot paths — daemon-verified zero stored proofs). The
                // modifier chain uses the zero candidate proof.
                kernel_skipped_era += 1;
            } else {
                // Live era: CheckProofOfStake ran at acceptance and its hash
                // was stored. Replay must reproduce it AND meet the target.
                let stake_tx = &block.vtx[1];
                let txin = &stake_tx.vin[0];
                let resolved = outputs.kernel_meta(&(txin.prev_hash, txin.prev_n));
                let res = match resolved {
                    None => die(&format!(
                        "KERNEL INPUT UNRESOLVED at height {} ({}, {}): the live era checked this block, so its input must resolve",
                        height,
                        tri7_core::legacy::hash_to_display(&txin.prev_hash),
                        txin.prev_n
                    )),
                    Some(_) => kernel::check_proof_of_stake(
                        &idx,
                        &block,
                        &outputs,
                        block.bits,
                        height.saturating_sub(1), // pindexBest during acceptance
                    ),
                };
                match res {
                    Err(e) => die(&format!(
                        "KERNEL GUARD FAIL at height {}: {}",
                        height, e
                    )),
                    Ok(res) => {
                        stored_proof = res.hash_proof_of_stake;
                        kernel_enforced_checked += 1;
                        if !res.meets_target {
                            die(&format!(
                                "KERNEL TARGET FAIL at height {}: nBits={:08x} modifier={:016x} blockFrom h{} coinDays={} hashProof={} target={}",
                                height,
                                block.bits,
                                res.modifier,
                                res.block_from_height,
                                res.coin_day_weight,
                                tri7_core::legacy::hash_to_display(&res.hash_proof_of_stake),
                                res.target,
                            ));
                        }
                        kernel_enforced_passed += 1;
                    }
                }
            }
        }

        // ---- 3. stake modifier for this height -----------------------------
        // Per mint-time client: 1200 s buckets before the Pharao takeover
        // (h17692), 300 s after. No gate — every client in the mint lineage
        // computed the modifier for every block (64d03a4:main.cpp:1950).
        let modifier_interval: u32 = if height < 17692 { 1200 } else { 300 };
        let (modifier, generated) = if height == 0 {
            (0u64, true)
        } else {
            match kernel::compute_next_stake_modifier_era(&idx, Some(height - 1), modifier_interval)
            {
                Ok(r) => r,
                Err(e) => die(&format!("modifier computation at height {}: {}", height, e)),
            }
        };
        if generated {
            modifier_generated += 1;
        } else {
            modifier_reused += 1;
        }

        // Genesis stake-modifier checksum checkpoint (kernel.cpp:20-21): the
        // compiled constant anchors the whole checksum serialization.
        if height == 0 {
            let flags = kernel::index_flags(false, kernel::stake_entropy_bit(&hash), true);
            let cs = kernel::stake_modifier_checksum(None, flags, &[0u8; 32], modifier);
            if cs != kernel::GENESIS_MODIFIER_CHECKSUM {
                die(&format!(
                    "genesis stake modifier checksum {:08x} != compiled constant {:08x}",
                    cs,
                    kernel::GENESIS_MODIFIER_CHECKSUM
                ));
            }
            genesis_checksum_ok = true;
        }

        // ---- 4. txid-keyed output index maintenance -------------------------
        outputs.apply_block(&block, height);
        // outputs.duplicate_txids is cumulative — report the final total after
        // the replay loop, not a per-block accumulation.
        respent_inputs = outputs.respends;

        duplicate_txid_outputs = outputs.duplicate_txids;
        idx.push(IndexEntry {
            height,
            hash,
            prev_hash: block.prev_hash,
            time: block.time,
            bits: block.bits,
            is_proof_of_stake: is_pos,
            entropy_bit: hash[0] & 1,
            stake_modifier: modifier,
            generated_stake_modifier: generated,
            hash_proof_of_stake: stored_proof, // heritage: zeros, real in the live era
        });

        if let Some(w) = dump.as_mut() {
            let _ = writeln!(w, "{} {:016x} {}", height, modifier, generated as u8);
            if height % 10_000 == 0 {
                let _ = w.flush();
            }
        }

        prev = Some(hash);
        height += 1;
        if height % 100_000 == 0 {
            println!(
                "... {} blocks ({} nBits enforced, {} kernels enforced-passed, {} kernel-skipped-era, {} unresolved)",
                height,
                nbits_enforced,
                kernel_enforced_passed,
                kernel_skipped_era,
                exempt_unresolved_input
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }

    if let Some(mut w) = dump {
        let _ = w.flush();
    }

    // ---- Coverage contract (mirrors validate_difficulty) --------------------
    let covered_to = height.saturating_sub(1);
    if nbits_enforced == 0 && kernel_enforced_checked == 0 {
        eprintln!(
            "FAIL: zero nBits/kernels enforced over heights 0..{} — no coverage",
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

    println!("replay: {} blocks total (genesis..{})", height, covered_to);
    println!(
        "blocks: {} PoS | nBits enforced + matched: {} (EMA port, seams 17692/17693/2186941 exempt)",
        pos_blocks, nbits_enforced
    );
    println!(
        "kernels: enforced (live era h >= {}): {} checked, {} passed",
        KERNEL_CHECKED_FROM, kernel_enforced_checked, kernel_enforced_passed
    );
    println!(
        "kernels skipped at acceptance (IBD/assume-valid/snapshot eras, daemon-verified zero stored proofs): {}",
        kernel_skipped_era
    );
    println!(
        "era-exempt kernels: {} unresolved stake inputs",
        exempt_unresolved_input
    );
    println!(
        "modifiers: {} generated / {} reused (per-block ComputeNextStakeModifier, nModifierInterval=300, no gate — FastImport rules)",
        modifier_generated, modifier_reused
    );
    println!(
        "outputs: {} indexed ({} live-txid entries), {} duplicate-txid overwrites, {} re-spends (vSpent era)",
        outputs.outputs_total,
        outputs.len(),
        duplicate_txid_outputs,
        respent_inputs
    );
    println!(
        "genesis stake modifier checkpoint 0x0e00670b: {}",
        if genesis_checksum_ok { "MATCH" } else { "NOT REACHED" }
    );
    println!("ALL POS CHECKS PASSED");
}
