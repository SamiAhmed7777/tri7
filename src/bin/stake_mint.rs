//! Phase 4 — staking-mint DRY RUN (stake_mint).
//!
//! Searches a proof-of-stake kernel over the wallet's REAL live UTXOs at the
//! current tip, constructs the full candidate block (coinbase + coinstake +
//! signature) and SELF-VALIDATES it with tri7_core's own acceptance path
//! (the same code validate_pos / follow_tip enforce). NOTHING is broadcast,
//! no socket is opened, the daemon and /root/.triangles are never written —
//! the only outputs are stdout lines and the local JSON report.
//!
//! Offline inputs (all read-only):
//!   - tipstate.json          — the follow_tip window: 4600 verified entries
//!                              (absolute heights, modifier / generated /
//!                              proof / checksum state) for h2199465..2204064.
//!   - wallet.dat             — the live wallet keys (read-only SQLite read).
//!   - wallet_utxos.json      — the phase-3 dump: wallet UTXOs live at h2201090.
//!   - blk0001.dat            — read-only archive scan: sequential live span
//!                              genesis..2201090, then the live tail
//!                              h2201091..2204064 reconstructed by Hash9
//!                              matching against the tipstate entry hashes
//!                              (the daemon interleaves reorged stale frames,
//!                              so frame order is not chain order there).
//!
//! Modifier oracle: the replay recomputes stake modifiers from genesis
//! (validate_pos rules, era interval 1200/300 at h17692) and must reproduce
//! tipstate's daemon-anchored (modifier, generated) at EVERY window height,
//! plus the checksum chain wherever tipstate carries non-zero checksums
//! (anchored at the first non-zero one, exactly follow_tip's load rule).
//! Any mismatch is a hard failure.
//!
//! Usage:
//!   stake_mint [--state-dir /root/tri7] [--wallet /root/.triangles/wallet.dat]
//!              [--utxo-dump /root/tri7/wallet_utxos.json]
//!              [--archive /root/.triangles/blk0001.dat]
//!              [--out /root/tri7/stake_dryrun.json] [--search-secs 60]
//!              [--max-coins N]
//!
//! Exit codes: 0 = dry run complete (report written; a kernel solution may or
//! may not exist — see the report), 2 = usage, 3 = chain/window verification
//! failure, 4 = a found solution FAILED self-validation, 5 = input loading
//! failure.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use tri7_core::address::{script_to_dest, ScriptDest};
use tri7_core::kernel::compute_next_stake_modifier_era;
use tri7_core::legacy::{hash_to_display, Block, Hash, Reader};
use tri7_core::replay::{IndexEntry, ReplayIndex};
use tri7_core::spend::{solver_type, SpendType};
use tri7_core::stake::{
    assemble_pos_block, create_coinstake, sign_block, validate_dryrun_block, PointOutputs,
    StakeCandidate,
};
use tri7_core::wallet;

const EXIT_USAGE: i32 = 2;
const EXIT_CHAIN: i32 = 3;
const EXIT_VALIDATE: i32 = 4;
const EXIT_INPUT: i32 = 5;

fn die(code: i32, msg: &str) -> ! {
    eprintln!("FAIL({}): {}", code, msg);
    std::process::exit(code);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn log(msg: &str) {
    println!("[stake_mint] {}", msg);
}

fn compact_size_len(n: u64) -> usize {
    if n < 0xfd {
        1
    } else if n <= 0xffff {
        3
    } else if n <= 0xffff_ffff {
        5
    } else {
        9
    }
}

// ---------------------------------------------------------------------------
// tipstate.json (follow_tip's persisted window)
// ---------------------------------------------------------------------------
#[derive(Deserialize)]
struct EntryRec {
    height: u64, // absolute
    hash: String,
    prev: String,
    time: u32,
    bits: u32,
    pos: bool,
    entropy: u8,
    modifier: u64,
    generated: bool,
    proof: String,
    checksum: u32,
}

#[derive(Deserialize)]
struct TipStateFile {
    height: u64,
    blockhash: String,
    window_base: u64,
    entries: Vec<EntryRec>,
}

// ---------------------------------------------------------------------------
// wallet_utxos.json (balances --utxos-out dump, live at its `height`)
// ---------------------------------------------------------------------------
#[derive(Clone, Deserialize)]
struct DumpUtxo {
    txid: String,
    n: u32,
    value_sats: i64,
    script_hex: String,
    #[allow(dead_code)]
    address: String,
    height: u32,
    tx_time: u32,
    prev_kind: String,
}

#[derive(Deserialize)]
struct UtxoDump {
    height: u32,
    count: usize,
    total_value_sats: i64,
    utxos: Vec<DumpUtxo>,
}

fn hex_to_internal(s: &str) -> Hash {
    tri7_core::p2p::display_to_internal(s).unwrap_or_else(|e| die(EXIT_INPUT, &e))
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect()
}

fn script_hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{:02x}", b)).collect()
}

fn main() {
    let mut state_dir = "/root/tri7".to_string();
    let mut wallet_path = "/root/.triangles/wallet.dat".to_string();
    let mut dump_path = "/root/tri7/wallet_utxos.json".to_string();
    let mut archive = "/root/.triangles/blk0001.dat".to_string();
    let mut out_path = "/root/tri7/stake_dryrun.json".to_string();
    let mut search_secs: i64 = 60;
    let mut max_coins = usize::MAX;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0usize;
    while i < args.len() {
        let need = |i: &mut usize, args: &[String]| -> String {
            *i += 1;
            args.get(*i)
                .cloned()
                .unwrap_or_else(|| die(EXIT_USAGE, "missing arg value"))
        };
        match args[i].as_str() {
            "--state-dir" => state_dir = need(&mut i, &args),
            "--wallet" => wallet_path = need(&mut i, &args),
            "--utxo-dump" => dump_path = need(&mut i, &args),
            "--archive" => archive = need(&mut i, &args),
            "--out" => out_path = need(&mut i, &args),
            "--search-secs" => {
                search_secs = need(&mut i, &args)
                    .parse()
                    .unwrap_or_else(|_| die(EXIT_USAGE, "--search-secs"))
            }
            "--max-coins" => {
                max_coins = need(&mut i, &args)
                    .parse()
                    .unwrap_or_else(|_| die(EXIT_USAGE, "--max-coins"))
            }
            _ => die(EXIT_USAGE, &format!("unknown arg {}", args[i])),
        }
        i += 1;
    }
    if !(1..=60).contains(&search_secs) {
        die(
            EXIT_USAGE,
            "--search-secs must be 1..=60 (nMaxStakeSearchInterval=60)",
        );
    }

    // ---- 1. tipstate --------------------------------------------------------
    let tip_path = format!("{}/tipstate.json", state_dir);
    let text = std::fs::read_to_string(&tip_path)
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("read {}: {}", tip_path, e)));
    let tip: TipStateFile = serde_json::from_str(&text)
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("parse tipstate: {}", e)));
    if tip.entries.is_empty() {
        die(EXIT_INPUT, "tipstate has no entries");
    }
    let window_base = tip.window_base;
    let tip_height_abs = tip.height;
    let window: Vec<(Hash, Hash, &EntryRec)> = tip
        .entries
        .iter()
        .map(|r| (hex_to_internal(&r.hash), hex_to_internal(&r.prev), r))
        .collect();
    let mut window_by_hash: HashMap<Hash, usize> = HashMap::new();
    for (wh, (h, _, _)) in window.iter().enumerate() {
        window_by_hash.insert(*h, wh);
    }
    log(&format!(
        "tipstate: tip h{} {} · window h{}..h{} ({} entries)",
        tip_height_abs,
        tip.blockhash,
        window_base,
        window_base + tip.entries.len() as u64 - 1,
        tip.entries.len()
    ));

    // ---- 2. wallet keys -------------------------------------------------------
    let wall = wallet::read_wallet(std::path::Path::new(&wallet_path))
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("wallet read: {}", e)));
    if wall.keys.is_empty() {
        die(EXIT_INPUT, "wallet has zero keys");
    }
    let mut key_by_payload: HashMap<[u8; 21], &wallet::WalletKey> = HashMap::new();
    let mut key_by_pubkey: HashMap<Vec<u8>, &wallet::WalletKey> = HashMap::new();
    for k in &wall.keys {
        key_by_payload.insert(tri7_core::address::pubkey_payload(&k.pubkey), k);
        key_by_pubkey.insert(k.pubkey.clone(), k);
    }
    log(&format!(
        "wallet: {} keys read ({} distinct addresses), encrypted={}",
        wall.keys.len(),
        key_by_payload.len(),
        wall.encrypted
    ));

    // ---- 3. utxo dump ---------------------------------------------------------
    let dump_text = std::fs::read_to_string(&dump_path)
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("read {}: {}", dump_path, e)));
    let dump: UtxoDump = serde_json::from_str(&dump_text)
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("parse utxo dump: {}", e)));
    log(&format!(
        "utxo dump: {} wallet UTXOs / {} sats live at h{} (snapshot)",
        dump.count, dump.total_value_sats, dump.height
    ));

    let wallet_payloads: HashSet<[u8; 21]> = key_by_payload.keys().copied().collect();

    // candidate txids to locate in the archive (dump txids; grown by tail adds)
    let mut wanted_txids: HashSet<Hash> = dump
        .utxos
        .iter()
        .map(|u| hex_to_internal(&u.txid))
        .collect();

    // ---- 4. archive scan --------------------------------------------------------
    let mut rp = Replay {
        idx: ReplayIndex::new(),
        tx_locations: HashMap::new(),
        tail_additions: Vec::new(),
        tail_spends: Vec::new(),
        modifier_matches: 0usize,
        checksum_matches: 0usize,
        genesis_checksum_ok: false,
        prefix_blocks: 0usize,
        window_blocks: 0usize,
    };
    scan_archive(
        &archive,
        window_base,
        &window_by_hash,
        &window,
        &mut wanted_txids,
        &wallet_payloads,
        dump.height,
        &mut rp,
    );
    if !rp.genesis_checksum_ok {
        die(
            EXIT_CHAIN,
            "genesis modifier checksum did not match 0x0e00670b",
        );
    }
    if rp.idx.tip().map(|t| t.height as u64) != Some(tip_height_abs) {
        die(
            EXIT_CHAIN,
            &format!(
                "replay tip h{} != tipstate tip h{}",
                rp.idx.tip().map(|t| t.height).unwrap_or(0),
                tip_height_abs
            ),
        );
    }
    if rp.idx.tip().map(|t| hash_to_display(&t.hash)) != Some(tip.blockhash.clone()) {
        die(EXIT_CHAIN, "replay tip hash != tipstate tip hash");
    }
    log(&format!(
        "modifier oracle: {}/{} window heights matched the daemon-anchored state, {} checksum steps verified — any mismatch exited above",
        rp.modifier_matches,
        window.len(),
        rp.checksum_matches
    ));

    // ---- 5. wallet live set at the tip ---------------------------------------
    // Start from the dump snapshot (live at dump.height) and apply the tail
    // blocks above it: remove spent outpoints, add wallet-owned outputs —
    // additions that were themselves spent later in the tail are dropped too.
    let spent: HashSet<(Hash, u32)> = rp.tail_spends.iter().copied().collect();
    let mut live: Vec<DumpUtxo> = dump
        .utxos
        .iter()
        .filter(|u| !spent.contains(&(hex_to_internal(&u.txid), u.n)))
        .cloned()
        .collect();
    let additions_total = rp.tail_additions.len();
    let base_live = live.len();
    live.extend(
        rp.tail_additions
            .iter()
            .filter(|u| !spent.contains(&(hex_to_internal(&u.txid), u.n)))
            .cloned(),
    );
    let additions_kept = live.len() - base_live;
    let balance: i64 = live.iter().map(|u| u.value_sats).sum();
    log(&format!(
        "live wallet at tip h{}: {} UTXOs / {} sats ({} tail additions kept of {} found, {} tail spends applied)",
        tip_height_abs,
        live.len(),
        balance,
        additions_kept,
        additions_total,
        rp.tail_spends.len()
    ));

    // ---- 6. build the candidate list -----------------------------------------
    let mut candidates: Vec<StakeCandidate> = Vec::new();
    let mut skipped_non_standard = 0usize;
    let mut skipped_no_key = 0usize;
    let mut skipped_unlocated = 0usize;
    for u in &live {
        let Some(script) = hex_decode(&u.script_hex) else {
            skipped_non_standard += 1;
            continue;
        };
        let txid = hex_to_internal(&u.txid);
        let Some(&(block_height, tx_offset)) = rp.tx_locations.get(&txid) else {
            skipped_unlocated += 1;
            continue;
        };
        let block_time = rp.idx.entries[block_height].time;
        // resolve the owning wallet key (the C++ keystore lookup would break
        // the coin otherwise)
        let key = match solver_type(&script) {
            Some(SpendType::PubKey(pk)) => key_by_pubkey.get(&pk).copied(),
            Some(SpendType::PubkeyHash(h)) => key_by_payload
                .get(&ScriptDest::PubkeyHash(h).payload())
                .copied(),
            _ => None,
        };
        let Some(k) = key else {
            skipped_no_key += 1;
            continue;
        };
        candidates.push(StakeCandidate {
            txid,
            n: u.n,
            value: u.value_sats,
            script_pubkey: script,
            block_height,
            block_time,
            tx_time: u.tx_time,
            tx_offset,
            secret: k.secret,
            pubkey: k.pubkey.clone(),
        });
    }
    candidates.sort_by(|a, b| {
        a.block_height
            .cmp(&b.block_height)
            .then_with(|| a.txid.cmp(&b.txid))
            .then_with(|| a.n.cmp(&b.n))
    });
    let total_eligible = candidates.len();
    candidates.truncate(max_coins);
    log(&format!(
        "candidates: {} staking-eligible UTXOs ({} non-standard, {} key-unresolved, {} unlocated){}",
        candidates.len(),
        skipped_non_standard,
        skipped_no_key,
        skipped_unlocated,
        if total_eligible > candidates.len() { " · capped by --max-coins" } else { "" }
    ));

    // ---- 7. next target + median ------------------------------------------------
    let n_bits = tri7_core::difficulty::get_next_target_required(&rp.idx, true);
    let median_time_past = {
        let n = rp.idx.entries.len();
        let mut times: Vec<u32> = rp.idx.entries[n - 11..].iter().map(|e| e.time).collect();
        times.sort_unstable();
        times[5]
    };
    let tip_entry = rp.idx.tip().unwrap().clone();
    log(&format!(
        "next target for h{} (PoS): {:08x} · tip time {} · MTP {}",
        tip_entry.height + 1,
        n_bits,
        tip_entry.time,
        median_time_past
    ));

    // ---- 8. the StakeMiner kernel search ----------------------------------------
    let search_time = now_secs() as u32;
    log(&format!(
        "kernel search: {} coins × up to {} backward seconds from nTime={} …",
        candidates.len(),
        search_secs,
        search_time
    ));
    let started = std::time::Instant::now();
    let (plan, rep) = create_coinstake(
        &rp.idx,
        &candidates,
        n_bits,
        0, // fees: the dry run carries no additional wallet txs
        search_time,
        search_secs,
        balance,
        tip_entry.height + 1,
    )
    .unwrap_or_else(|e| die(EXIT_CHAIN, &format!("kernel search: {}", e)));
    log(&format!(
        "search done in {:.1}s: coins considered {} (min-age {}), attempts {}, kernel hits {}, zero-reward aborts {}, credit-aborts {}",
        started.elapsed().as_secs_f64(),
        rep.coins_considered,
        rep.coins_meeting_min_age,
        rep.attempts,
        rep.kernel_hits,
        rep.zero_reward_aborts,
        rep.credit_over_balance_aborts
    ));

    let mut report = serde_json::json!({
        "mode": "dry-run — no network, nothing broadcast, daemon untouched",
        "tip": {
            "height": tip_height_abs,
            "blockhash": tip.blockhash,
            "time": tip_entry.time,
        },
        "next_height": tip_entry.height + 1,
        "n_bits": format!("{:08x}", n_bits),
        "wallet": {
            "keys": wall.keys.len(),
            "live_utxos": live.len(),
            "balance_sats": balance,
            "candidates": candidates.len(),
        },
        "kernel_search": {
            "coins_considered": rep.coins_considered,
            "coins_meeting_min_age": rep.coins_meeting_min_age,
            "attempts": rep.attempts,
            "kernel_hits": rep.kernel_hits,
            "zero_reward_aborts": rep.zero_reward_aborts,
            "credit_over_balance_aborts": rep.credit_over_balance_aborts,
            "search_secs": search_secs,
            "search_time": search_time,
        },
        "modifier_oracle": {
            "window_heights_checked": rp.modifier_matches,
            "checksum_steps_verified": rp.checksum_matches,
            "genesis_checksum_ok": rp.genesis_checksum_ok,
            "archive_prefix_blocks": rp.prefix_blocks,
            "archive_window_blocks_reconstructed": rp.window_blocks,
        },
        "solution_found": plan.is_some(),
    });

    let Some(plan) = plan else {
        report["note"] = serde_json::json!(
            "attempts exhausted — no wallet UTXO solved the kernel at the current target within the search interval"
        );
        write_report(&out_path, &report);
        log("RESULT: no kernel solution within the search interval (attempts exhausted)");
        return;
    };

    // ---- 9. assemble + sign the candidate block --------------------------------
    log(&format!(
        "KERNEL SOLVED: coin {}:{} ({} sats, h{}), nTime {} (-{} s), modifier {:016x}, coinDays {}, hashProof {}",
        hash_to_display(&plan.signer.txid),
        plan.signer.n,
        plan.signer.value,
        plan.signer.block_height,
        plan.kernel.n_time,
        plan.kernel.seconds_back,
        plan.kernel.modifier,
        plan.kernel.coin_day_weight,
        hash_to_display(&plan.kernel.hash_proof_of_stake),
    ));
    let mut block = assemble_pos_block(&tip_entry, median_time_past, &plan.tx, &[], n_bits)
        .unwrap_or_else(|e| die(EXIT_CHAIN, &format!("block assembly: {}", e)));
    sign_block(&mut block, &plan.signer.secret)
        .unwrap_or_else(|e| die(EXIT_CHAIN, &format!("block signature: {}", e)));
    let block_hash_display = hash_to_display(&block.hash());
    log(&format!(
        "candidate block: {} (vtx {}, time {}, bits {:08x}, sig {} B)",
        block_hash_display,
        block.vtx.len(),
        block.time,
        block.bits,
        block.block_sig.len()
    ));

    // ---- 10. self-validation (tri7_core acceptance path) ------------------------
    let mut point: HashMap<(Hash, u32), tri7_core::kernel::UtxoMeta> = HashMap::new();
    for input in &plan.inputs {
        point.insert(
            (input.txid, input.n),
            tri7_core::kernel::UtxoMeta {
                height: input.block_height,
                tx_offset: input.tx_offset,
                tx_time: input.tx_time,
                value: input.value,
            },
        );
    }
    let outputs = PointOutputs(point);
    let validation = validate_dryrun_block(&rp.idx, &outputs, &block, now_secs() as i64);
    match validation {
        Ok(v) => {
            // VerifySignature layer (kernel.cpp:408-410 / ConnectInputs): the
            // coinstake's own input scriptSigs must ECDSA-verify against their
            // prevout scripts (the block-signature check is separate).
            for (i, input) in plan.inputs.iter().enumerate() {
                tri7_core::spend::verify_input(&plan.tx, i, &input.script_pubkey).unwrap_or_else(
                    |e| die(EXIT_VALIDATE, &format!("coinstake input {} sig: {}", i, e)),
                );
            }
            log(&format!(
                "SELF-VALIDATION PASSED: kernel meets target (coinDays {}), reward paid {} == calculated {} (coin age {} sat-days), sig verified, nBits {:08x} exact",
                v.kernel.coin_day_weight,
                v.stake_reward_paid,
                v.stake_reward_calculated,
                v.coin_age,
                block.bits
            ));
            report["coinstake"] = serde_json::json!({
                "kernel_txid": hash_to_display(&plan.signer.txid),
                "kernel_n": plan.signer.n,
                "kernel_block_height": plan.signer.block_height,
                "kernel_block_time": plan.signer.block_time,
                "kernel_tx_offset": plan.signer.tx_offset,
                "kernel_coin_sats": plan.signer.value,
                "n_time": plan.kernel.n_time,
                "seconds_back": plan.kernel.seconds_back,
                "modifier": format!("{:016x}", plan.kernel.modifier),
                "hash_proof_of_stake": hash_to_display(&plan.kernel.hash_proof_of_stake),
                "coin_day_weight": plan.kernel.coin_day_weight.to_string(),
                "coin_age_sat_days": plan.coin_age.to_string(),
                "reward_sats": plan.reward,
                "credit_sats": plan.credit,
                "split": plan.split,
                "inputs": plan.inputs.iter().map(|i| serde_json::json!({
                    "txid": hash_to_display(&i.txid),
                    "n": i.n,
                    "value_sats": i.value,
                    "height": i.block_height,
                })).collect::<Vec<_>>(),
            });
            report["block"] = serde_json::json!({
                "hash": block_hash_display,
                "prev": hash_to_display(&block.prev_hash),
                "merkle": hash_to_display(&block.merkle_root),
                "time": block.time,
                "bits": format!("{:08x}", block.bits),
                "nonce": block.nonce,
                "vtx": block.vtx.len(),
                "coinbase_scriptsig_hex": script_hex(&block.vtx[0].vin[0].script_sig),
                "block_sig_len": block.block_sig.len(),
            });
            report["self_validation"] = serde_json::json!({
                "link_ok": v.link_ok,
                "structure_ok": v.structure_ok,
                "merkle_ok": v.merkle_ok,
                "time_ok": v.time_ok,
                "n_bits_ok": v.n_bits_ok,
                "kernel_meets_target": v.kernel_meets_target,
                "signature_ok": v.signature_ok,
                "finality_ok": v.finality_ok,
                "reward_ok": v.reward_ok,
                "size_ok": v.size_ok,
                "stake_reward_paid": v.stake_reward_paid,
                "stake_reward_calculated": v.stake_reward_calculated,
                "coin_age_sat_days": v.coin_age.to_string(),
                "coinstake_input_sigs_verified": plan.inputs.len(),
                "verdict": "PASSED — candidate block fully valid against the tri7 acceptance path",
            });
            report["note"] =
                serde_json::json!("candidate block validated but NOT broadcast — dry run only");
            write_report(&out_path, &report);
            log(&format!(
                "RESULT: kernel solution found and self-validated — candidate block {}",
                block_hash_display
            ));
        }
        Err(e) => {
            report["self_validation"] = serde_json::json!({ "verdict": "FAILED", "error": e });
            write_report(&out_path, &report);
            die(EXIT_VALIDATE, &format!("self-validation FAILED: {}", e));
        }
    }
}

fn write_report(path: &str, v: &serde_json::Value) {
    let text = serde_json::to_string_pretty(v).unwrap();
    let tmp = format!("{}.tmp", path);
    if let Err(e) = std::fs::write(&tmp, &text) {
        die(EXIT_INPUT, &format!("write {}: {}", tmp, e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        die(EXIT_INPUT, &format!("rename report: {}", e));
    }
    println!("report -> {}", path);
}

// ---------------------------------------------------------------------------
// Chain replay state
// ---------------------------------------------------------------------------
struct Replay {
    idx: ReplayIndex,
    /// (block height, tx offset) per candidate txid — kernel prevout metadata.
    tx_locations: HashMap<Hash, (usize, u32)>,
    /// Wallet-owned outputs appearing in blocks ABOVE the dump snapshot.
    tail_additions: Vec<DumpUtxo>,
    /// Wallet outpoints spent in blocks ABOVE the dump snapshot.
    tail_spends: Vec<(Hash, u32)>,
    modifier_matches: usize,
    checksum_matches: usize,
    genesis_checksum_ok: bool,
    prefix_blocks: usize,
    window_blocks: usize,
}

// ---------------------------------------------------------------------------
// The single archive pass:
//   - prefix (genesis..): sequential prev-link walk over the clean live span,
//     computing stake modifiers per validate_pos and locating candidate txids;
//   - tail: every frame is Hash9'd; frames whose hash appears in the tipstate
//     window set are LIVE chain blocks (the daemon interleaves reorged stale
//     frames there, so frame order is not chain order) — they are stored and
//     appended to the index in HEIGHT order afterwards, prev-link verified
//     against tipstate's recorded prev hashes, modifiers recomputed and
//     verified against tipstate's daemon-anchored values, checksum chain
//     verified wherever tipstate carries non-zero checksums.
// ---------------------------------------------------------------------------
fn scan_archive(
    path: &str,
    window_base: u64,
    window_by_hash: &HashMap<Hash, usize>,
    window: &[(Hash, Hash, &EntryRec)],
    wanted_txids: &mut HashSet<Hash>,
    wallet_payloads: &HashSet<[u8; 21]>,
    dump_height: u32,
    rp: &mut Replay,
) {
    use std::io::Write as _;
    use tri7_core::blkfile::BlkFileReader;

    let mut rdr = BlkFileReader::open(path)
        .unwrap_or_else(|e| die(EXIT_INPUT, &format!("open archive: {}", e)));

    let mut tail_blocks: HashMap<usize, Block> = HashMap::new();
    let mut prefix_broken = false;
    let mut frames = 0usize;
    let t_start = std::time::Instant::now();

    loop {
        let frame = match rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => die(EXIT_CHAIN, &format!("archive frame: {}", e)),
        };
        frames += 1;
        if frames % 500_000 == 0 {
            log(&format!(
                "archive scan: {} frames ({:.0}s) — prefix {}, tail {}",
                frames,
                t_start.elapsed().as_secs_f64(),
                rp.idx.entries.len(),
                tail_blocks.len()
            ));
            let _ = std::io::stdout().flush();
        }
        if frame.data.len() < 80 {
            die(EXIT_CHAIN, "frame shorter than a header");
        }
        // Cheap pre-filter: Hash9 over the 80-byte header decides everything.
        let hdr_hash = tri7_core::legacy::hash9(&frame.data[..80]);
        if let Some(&wh) = window_by_hash.get(&hdr_hash) {
            // live window block — parse fully and store for ordered replay
            let mut r = Reader::new(&frame.data);
            let b = match Block::read(&mut r) {
                Ok(b) => b,
                Err(e) => die(EXIT_CHAIN, &format!("window block parse: {}", e)),
            };
            if r.expect_done().is_err() {
                die(EXIT_CHAIN, "window block trailing bytes");
            }
            let abs = window_base + wh as u64;
            if b.hash() != hdr_hash || b.prev_hash != window[wh].1 {
                die(
                    EXIT_CHAIN,
                    &format!("window block h{} linkage/hash mismatch", abs),
                );
            }
            collect_tx_locations(&b, abs as usize, wanted_txids, rp);
            tail_blocks.insert(wh, b);
            continue;
        }
        if prefix_broken {
            continue; // stale-branch frame after the seam — not a live block
        }
        // prefix path: parse fully; verify sequential linkage
        let mut r = Reader::new(&frame.data);
        let b = match Block::read(&mut r) {
            Ok(b) => b,
            Err(e) => die(EXIT_CHAIN, &format!("prefix block parse: {}", e)),
        };
        let expect_height = rp.idx.entries.len();
        let links = if expect_height == 0 {
            b.prev_hash == [0u8; 32]
        } else {
            b.prev_hash == rp.idx.entries[expect_height - 1].hash
        };
        if !links {
            // end of the sequential live span (the archive's stale-branch seam)
            prefix_broken = true;
            continue;
        }
        push_prefix_block(&b, hdr_hash, wanted_txids, rp);
    }

    // The prefix must reach the window base exactly: its tip is window[0]'s prev.
    let (_, first_prev, _) = window[0];
    let prefix_tip = match rp.idx.tip() {
        Some(t) => t.clone(),
        None => die(EXIT_CHAIN, "empty prefix — archive has no live blocks"),
    };
    if prefix_tip.hash != first_prev {
        die(
            EXIT_CHAIN,
            &format!(
                "prefix tip h{} {} does not chain to the window base prev {} — archive cannot reconstruct the chain offline",
                prefix_tip.height,
                hash_to_display(&prefix_tip.hash),
                hash_to_display(&first_prev)
            ),
        );
    }
    rp.prefix_blocks = rp.idx.entries.len();

    // Append the window in height order, computing + verifying modifiers.
    let mut prev_cs: Option<u32> = None; // checksum anchor (follow_tip load rule)
    let mut anchored = false;
    for wh in 0..window.len() {
        let (h, prev, rec) = window[wh];
        let Some(b) = tail_blocks.get(&wh) else {
            die(
                EXIT_CHAIN,
                &format!(
                    "live window block h{} ({}) missing from the archive — cannot reconstruct the tail offline",
                    window_base + wh as u64,
                    hash_to_display(&h)
                ),
            );
        };
        if b.prev_hash != prev || b.hash() != h {
            die(
                EXIT_CHAIN,
                &format!("window block h{} linkage broken", rec.height),
            );
        }
        let height_abs = rec.height as usize;
        // modifier for this height (era interval, validate_pos rules)
        let interval: u32 = if height_abs < 17692 { 1200 } else { 300 };
        let (modifier, generated) = if height_abs == 0 {
            (0u64, true)
        } else {
            compute_next_stake_modifier_era(&rp.idx, Some(height_abs - 1), interval)
                .unwrap_or_else(|e| die(EXIT_CHAIN, &format!("modifier at h{}: {}", height_abs, e)))
        };
        if modifier != rec.modifier || generated != rec.generated {
            die(
                EXIT_CHAIN,
                &format!(
                    "MODIFIER MISMATCH at h{}: replayed {:016x}/{} vs daemon-anchored {:016x}/{}",
                    height_abs, modifier, generated as u8, rec.modifier, rec.generated as u8
                ),
            );
        }
        rp.modifier_matches += 1;
        // checksum chain: the daemon stores zero checksums below the FastImport
        // persistence boundary, so the FIRST non-zero checksum cannot be
        // re-derived locally (its true previous checksum was never persisted) —
        // ADOPT it as the anchor and verify every non-zero step after it.
        // This is byte-for-byte follow_tip's rpc_seed_window / load_state rule.
        let entropy = h[0] & 1;
        let flags = tri7_core::kernel::index_flags(rec.pos, entropy, generated);
        let proof = hex_to_internal(&rec.proof);
        let expect_cs =
            tri7_core::kernel::stake_modifier_checksum(prev_cs, flags, &proof, modifier);
        if rec.checksum != 0 && !anchored {
            anchored = true;
            prev_cs = Some(rec.checksum); // adopt the daemon's anchor
            log(&format!(
                "checksum chain anchored at h{} ({:08x} adopted — daemon FastImport boundary)",
                height_abs, rec.checksum
            ));
        } else if rec.checksum != 0 {
            if expect_cs != rec.checksum {
                die(
                    EXIT_CHAIN,
                    &format!(
                        "CHECKSUM MISMATCH at h{}: computed {:08x} vs tipstate {:08x}",
                        height_abs, expect_cs, rec.checksum
                    ),
                );
            }
            rp.checksum_matches += 1;
            prev_cs = Some(rec.checksum);
        } else {
            prev_cs = Some(expect_cs);
        }
        // wallet reconciliation above the dump snapshot
        apply_tail_bookkeeping(b, height_abs as u32, dump_height, wallet_payloads, rp);
        rp.idx.push(IndexEntry {
            height: height_abs,
            hash: b.hash(),
            prev_hash: b.prev_hash,
            time: b.time,
            bits: b.bits,
            is_proof_of_stake: rec.pos,
            entropy_bit: entropy,
            stake_modifier: modifier,
            generated_stake_modifier: generated,
            hash_proof_of_stake: proof,
        });
        rp.window_blocks += 1;
    }
    if rp.window_blocks != window.len() {
        die(
            EXIT_CHAIN,
            &format!("window coverage {} != {}", rp.window_blocks, window.len()),
        );
    }

    // genesis modifier checkpoint (kernel.cpp:20-21) — the anchor of the whole
    // replayed modifier chain.
    let g = &rp.idx.entries[0];
    let flags =
        tri7_core::kernel::index_flags(false, tri7_core::kernel::stake_entropy_bit(&g.hash), true);
    let cs = tri7_core::kernel::stake_modifier_checksum(None, flags, &[0u8; 32], g.stake_modifier);
    rp.genesis_checksum_ok = cs == tri7_core::kernel::GENESIS_MODIFIER_CHECKSUM;
    if !rp.genesis_checksum_ok {
        die(
            EXIT_CHAIN,
            &format!("genesis modifier checksum {:08x} != 0x0e00670b", cs),
        );
    }

    log(&format!(
        "archive scan complete: {} frames read, prefix {} blocks (genesis..h{}), window {} blocks reconstructed ({:.1}s)",
        frames,
        rp.prefix_blocks,
        rp.prefix_blocks - 1,
        rp.window_blocks,
        t_start.elapsed().as_secs_f64()
    ));
}

/// Prefix-block processing: modifier computation + candidate tx locations.
fn push_prefix_block(b: &Block, hash: Hash, wanted_txids: &mut HashSet<Hash>, rp: &mut Replay) {
    let height = rp.idx.entries.len();
    let interval: u32 = if height < 17692 { 1200 } else { 300 };
    let (modifier, generated) = if height == 0 {
        (0u64, true)
    } else {
        compute_next_stake_modifier_era(&rp.idx, Some(height - 1), interval)
            .unwrap_or_else(|e| die(EXIT_CHAIN, &format!("modifier at h{}: {}", height, e)))
    };
    if height == 0 {
        let flags = tri7_core::kernel::index_flags(false, hash[0] & 1, true);
        let cs = tri7_core::kernel::stake_modifier_checksum(None, flags, &[0u8; 32], modifier);
        rp.genesis_checksum_ok = cs == tri7_core::kernel::GENESIS_MODIFIER_CHECKSUM;
    }
    collect_tx_locations(b, height, wanted_txids, rp);
    rp.idx.push(IndexEntry {
        height,
        hash,
        prev_hash: b.prev_hash,
        time: b.time,
        bits: b.bits,
        is_proof_of_stake: tri7_core::consensus::block_is_proof_of_stake(b),
        entropy_bit: hash[0] & 1,
        stake_modifier: modifier,
        generated_stake_modifier: generated,
        hash_proof_of_stake: [0u8; 32], // heritage zeros below the live era
    });
}

/// Record (txid → block height, tx offset) for every tx we may need as a
/// kernel prevout.
fn collect_tx_locations(
    b: &Block,
    height: usize,
    wanted_txids: &mut HashSet<Hash>,
    rp: &mut Replay,
) {
    let mut offset = (80 + compact_size_len(b.vtx.len() as u64)) as u32;
    for tx in &b.vtx {
        let txid = tx.hash();
        if wanted_txids.contains(&txid) && !rp.tx_locations.contains_key(&txid) {
            rp.tx_locations.insert(txid, (height, offset));
        }
        offset += tx.serialize().len() as u32;
    }
}

/// Wallet reconciliation for blocks ABOVE the dump snapshot: spend removals
/// and wallet-owned output additions. The candidate txid set grows with the
/// additions so their kernel metadata gets located too.
fn apply_tail_bookkeeping(
    b: &Block,
    height_abs: u32,
    dump_height: u32,
    wallet_payloads: &HashSet<[u8; 21]>,
    rp: &mut Replay,
) {
    if height_abs <= dump_height {
        // inside the dump snapshot's coverage — nothing to reconcile
        return;
    }
    let is_coinbase = |tx: &tri7_core::Tx| {
        tx.vin.len() == 1 && tx.vin[0].prev_hash == [0u8; 32] && tx.vin[0].prev_n == u32::MAX
    };
    for tx in &b.vtx {
        if !is_coinbase(tx) {
            for vin in &tx.vin {
                rp.tail_spends.push((vin.prev_hash, vin.prev_n));
            }
        }
        for (n, vout) in tx.vout.iter().enumerate() {
            if vout.value == 0 && vout.script_pubkey.is_empty() {
                continue; // IsEmpty marker
            }
            let dest = match script_to_dest(&vout.script_pubkey) {
                Some(d) => d,
                None => continue,
            };
            if !wallet_payloads.contains(&dest.payload()) {
                continue;
            }
            let txid = tx.hash();
            rp.tail_additions.push(DumpUtxo {
                txid: hash_to_display(&txid),
                n: n as u32,
                value_sats: vout.value,
                script_hex: vout
                    .script_pubkey
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect(),
                address: String::new(),
                height: height_abs,
                tx_time: tx.time,
                prev_kind: if is_coinbase(tx) {
                    "coinbase".into()
                } else if tri7_core::consensus::tx_is_coin_stake(tx) {
                    "coinstake".into()
                } else {
                    "regular".into()
                },
            });
            // ensure the output's tx is locatable (it may not have been in the
            // wanted set when this frame streamed)
            if !rp.tx_locations.contains_key(&txid) {
                let mut offset = (80 + compact_size_len(b.vtx.len() as u64)) as u32;
                for t2 in &b.vtx {
                    if t2.hash() == txid {
                        rp.tx_locations.insert(txid, (height_abs as usize, offset));
                        break;
                    }
                    offset += t2.serialize().len() as u32;
                }
            }
        }
    }
}
