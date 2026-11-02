//! Phase 3 correctness anchor: prove the SignatureHash port against REAL
//! mint-era signatures from the archive (blk0001.dat).
//!
//! Chain reality (sig_probe.rs, full 2.2M-block pass): regular transactions
//! carry ZERO bare-P2PK outputs — every regular output is P2PKH; the only
//! bare-pubkey prevouts in the chain are coinstake outputs (Solver PubKey is
//! the template PoS block signatures use, vtx[1].vout[1]). The historical
//! corpus therefore collects BOTH existing single-sig populations, all with
//! REGULAR spending txs (never coinbase/coinstake spenders):
//!   - p2pk-prev: 1-push scriptSig over a bare-pubkey prevout (the prev tx
//!     may be a coinstake — recorded per entry as prev_tx_kind);
//!   - p2pkh-prev: 2-push scriptSig over a P2PKH prevout.
//! The SignatureHash scheme under test is prevout-script-agnostic (scriptCode
//! = the previous output's scriptPubKey, whatever standard form), so both
//! populations anchor the exact 0.1.6-era algorithm.
//!
//! Walks the live chain with a rolling prev-tx cache (regular + coinstake
//! txs; coinbase excluded entirely), and for every candidate input runs the
//! FULL verification path: spend::verify_input → spend::signature_hash →
//! ECDSA against the previous output's pubkey / pubkey hash.
//!
//! Contract (mirrors the house coverage rules):
//!   exit 0 ONLY if at least 20 historical signatures verified with STRICT
//!   DER encodings (>= 1 from the p2pk-prev population), zero hard ECDSA
//!   failures, and the walk reached its stop condition (target collected /
//!   max height reached) — a stale branch or premature EOF before the stop
//!   is a failure. Pre-strict-ECDSA encodings (root-caused CRAPCHAIN-era
//!   class) are counted as skips, never asserted.
//!
//! NO network traffic, NO daemon access: the archive file is read directly.
//!
//! Usage: historical_sig_corpus [blk_file] [start_height] [target]
//!        [--out path] [--max-height H]
//! Output: corpus JSON (prev/spend tx hex pairs) for the checked-in test.

use std::collections::{HashMap, VecDeque};

use tri7_core::consensus;
use tri7_core::legacy::{hash_to_display, Reader, Tx};
use tri7_core::replay::{check_coverage, ArchiveWalker, Step};
use tri7_core::spend::{self, SpendType};

fn die(msg: &str) -> ! {
    eprintln!("FAIL: {}", msg);
    std::process::exit(1);
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

/// A cached prev tx: serialization (for the corpus dump) plus its standard
/// outputs with the Solver classification. Coinbase txs are never cached.
struct CachedPrev {
    ser: Vec<u8>,
    is_coinstake: bool,
    height: usize,
    vouts: Vec<(u32, &'static str, Vec<u8>)>, // (n, "p2pk"|"p2pkh", script)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/.triangles/blk0001.dat".to_string());
    let start_height: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(9002);
    let target: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/root/tri7/corpus/historical_sigs.json".to_string());
    let max_height: usize = args
        .iter()
        .position(|a| a == "--max-height")
        .and_then(|i| args.get(i + 1).cloned())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_500_000);

    // Rolling prev-tx cache: insertion-ordered, bounded.
    const CACHE_CAP: usize = 150_000;
    let mut cache: HashMap<[u8; 32], CachedPrev> = HashMap::new();
    let mut order: VecDeque<[u8; 32]> = VecDeque::new();

    let mut walker = ArchiveWalker::open(&path).unwrap_or_else(|e| die(&e));
    let mut stale_at: Option<usize> = None;
    let mut covered_to = 0usize;
    let mut reached_stop = false;
    let mut hard_failures = 0usize;
    let mut broken_era_failures = 0usize;
    let mut debug_samples: Vec<String> = Vec::new();
    let mut sigs_total = 0usize;
    let mut sigs_p2pk_prev = 0usize;
    let mut sigs_p2pkh_prev = 0usize;
    let mut distinct_spends = 0usize;
    let mut sampled: std::collections::HashMap<[u8; 32], usize> = std::collections::HashMap::new();
    let mut lax_verified = 0usize;
    let mut unverifiable_skipped = 0usize;
    let mut prevs_not_cached = 0usize;
    let mut candidates_seen = 0usize;
    let mut corpus: Vec<String> = Vec::new();

    loop {
        let (height, block, _) = match walker.step() {
            Err(e) => die(&e),
            Ok(Step::Eof) => break,
            Ok(Step::StaleBranch(h)) => {
                stale_at = Some(h);
                break;
            }
            Ok(Step::Block(h, b, hash)) => (h, b, hash),
        };
        covered_to = height;

        if height >= start_height {
            for tx in &block.vtx {
                let is_coinbase = consensus::tx_is_coin_base(tx);
                let is_coinstake = consensus::tx_is_coin_stake(tx);
                if is_coinbase {
                    continue; // coinbase txs are neither cached nor sampled
                }
                if !is_coinstake {
                    // Only REGULAR txs are sampled as spenders.
                    for (vin_idx, vin) in tx.vin.iter().enumerate() {
                        let pushes = match spend::parse_pushes(&vin.script_sig) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        if pushes.len() != 1 && pushes.len() != 2 {
                            continue;
                        }
                        candidates_seen += 1;
                        let Some(prev) = cache.get(&vin.prev_hash) else {
                            prevs_not_cached += 1;
                            continue;
                        };
                        let Some((_, kind, script)) =
                            prev.vouts.iter().find(|(n, _, _)| *n == vin.prev_n)
                        else {
                            continue;
                        };
                        // prevout sanity: cached bytes must hash to the prevout id
                        let parsed = Tx::read(&mut Reader::new(&prev.ser))
                            .unwrap_or_else(|e| die(&format!("cached prev tx parse: {}", e)));
                        if parsed.hash() != vin.prev_hash || parsed.vout.len() as u32 <= vin.prev_n
                        {
                            die(&format!(
                                "cached prev tx {} does not match prevout (corrupt cache)",
                                hash_to_display(&vin.prev_hash)
                            ));
                        }
                        // the Solver shape must agree with the scriptSig push count
                        let want = match *kind {
                            "p2pk" => 1usize,
                            _ => 2usize,
                        };
                        if pushes.len() != want {
                            continue;
                        }
                        match spend::verify_input(tx, vin_idx, script) {
                            Ok(v) => {
                                let prev_kind = if prev.is_coinstake { "coinstake" } else { "regular" };
                                if v.der_mode == "strict" {
                                    sigs_total += 1;
                                    match *kind {
                                        "p2pk" => sigs_p2pk_prev += 1,
                                        _ => sigs_p2pkh_prev += 1,
                                    }
                                    // diversity: at most 2 entries per spending tx
                                    let spend_id = tx.hash();
                                    let per_tx = sampled.entry(spend_id).or_insert(0);
                                    if *per_tx < 2 && corpus.len() < target {
                                        *per_tx += 1;
                                        if *per_tx == 1 {
                                            distinct_spends += 1;
                                        }
                                        corpus.push(format!(
                                            concat!(
                                                "{{\"height\":{},\"vin_index\":{},\"prev_n\":{},",
                                                "\"prev_class\":\"{}\",\"prev_tx_kind\":\"{}\",",
                                                "\"prev_tx_hex\":\"{}\",\"spend_tx_hex\":\"{}\",",
                                                "\"prev_script_hex\":\"{}\",\"sighash_hex\":\"{}\",",
                                                "\"hash_type\":{},\"der_mode\":\"strict\"}}"
                                            ),
                                            height,
                                            vin_idx,
                                            vin.prev_n,
                                            kind,
                                            prev_kind,
                                            hex(&prev.ser),
                                            hex(&tx.serialize()),
                                            hex(script),
                                            hex(&v.sighash),
                                            v.hash_type
                                        ));
                                    }
                                    if distinct_spends >= 20 && corpus.len() >= 20 {
                                        println!(
                                            "... target reached: {} strict historical signatures \
                                             over {} distinct spending txs at height {} \
                                             (p2pk-prev {}, p2pkh-prev {})",
                                            sigs_total, distinct_spends, height,
                                            sigs_p2pk_prev, sigs_p2pkh_prev
                                        );
                                        reached_stop = true;
                                    }
                                } else {
                                    lax_verified += 1;
                                }
                            }
                            Err(e) => {
                                // A parseable signature that fails ECDSA is either
                                // (a) the known pre-strict-ECDSA CRAPCHAIN-era
                                // population (h <= 17691 — deep history accepted
                                // under assume-valid; root-caused in the handoff,
                                // do not re-open) or (b) a REAL SignatureHash
                                // failure. Only (b) kills the run.
                                let parseable = e.contains("ECDSA verify failed")
                                    || e.contains("hash mismatch")
                                    || e.contains("bad pubkey");
                                if parseable {
                                    if height > consensus::CRAPCHAIN_CUTOFF_BLOCK {
                                        eprintln!(
                                            "HARD FAIL at height {} tx {} vin {}: {}",
                                            height,
                                            hash_to_display(&tx.hash()),
                                            vin_idx,
                                            e
                                        );
                                        hard_failures += 1;
                                    } else {
                                        broken_era_failures += 1;
                                        if debug_samples.len() < 5 {
                                            debug_samples.push(format!(
                                                concat!(
                                                    "{{\"height\":{},\"vin_index\":{},\"prev_class\":\"{}\",",
                                                    "\"error\":\"{}\",\"prev_tx_hex\":\"{}\",",
                                                    "\"spend_tx_hex\":\"{}\",\"prev_script_hex\":\"{}\"}}"
                                                ),
                                                height,
                                                vin_idx,
                                                kind,
                                                e.replace('"', "'"),
                                                hex(&prev.ser),
                                                hex(&tx.serialize()),
                                                hex(script)
                                            ));
                                        }
                                    }
                                } else {
                                    unverifiable_skipped += 1;
                                }
                            }
                        }
                    }
                }
                // Cache the tx itself (regular + coinstake; coinbase excluded).
                let mut vouts: Vec<(u32, &'static str, Vec<u8>)> = Vec::new();
                for (n, o) in tx.vout.iter().enumerate() {
                    if o.value <= 0 {
                        continue;
                    }
                    match spend::solver_type(&o.script_pubkey) {
                        Some(SpendType::PubKey(_)) => {
                            vouts.push((n as u32, "p2pk", o.script_pubkey.clone()))
                        }
                        Some(SpendType::PubkeyHash(_)) => {
                            vouts.push((n as u32, "p2pkh", o.script_pubkey.clone()))
                        }
                        _ => {}
                    }
                }
                if !vouts.is_empty() {
                    let txid = tx.hash();
                    if cache
                        .insert(
                            txid,
                            CachedPrev {
                                ser: tx.serialize(),
                                is_coinstake,
                                height,
                                vouts,
                            },
                        )
                        .is_none()
                    {
                        order.push_back(txid);
                        while order.len() > CACHE_CAP {
                            if let Some(old) = order.pop_front() {
                                cache.remove(&old);
                            }
                        }
                    }
                }
            }
        }

        if reached_stop || height >= max_height {
            break;
        }
    }

    // ---- coverage contract --------------------------------------------------
    if hard_failures > 0 {
        die(&format!(
            "{} hard ECDSA failures above the CRAPCHAIN cutoff (h{}) — SignatureHash \
             does not match the mint-time clients",
            hard_failures,
            consensus::CRAPCHAIN_CUTOFF_BLOCK
        ));
    }
    if sigs_total < 20 || sigs_p2pk_prev < 1 || distinct_spends < 20 {
        die(&format!(
            "insufficient corpus: {} strict signatures / {} distinct spenders (<20, or no p2pk-prev entry) \
             over heights {}..{} (p2pk-prev {}, p2pkh-prev {}, lax {}, unverifiable-skipped {}, \
             uncached prevouts {}) — move the window or raise --max-height",
            sigs_total, distinct_spends, start_height, covered_to, sigs_p2pk_prev,
            sigs_p2pkh_prev, lax_verified, unverifiable_skipped, prevs_not_cached
        ));
    }
    if !reached_stop {
        check_coverage(stale_at, covered_to, max_height, false)
            .unwrap_or_else(|e| die(&format!("incomplete walk: {} ({} sigs verified)", e, sigs_total)));
    }

    // ---- corpus JSON ---------------------------------------------------------
    let mut j = String::with_capacity(1 << 20);
    j.push_str("{\n");
    j.push_str(&format!("  \"blk_file\": \"{}\",\n", path));
    j.push_str(&format!("  \"heights_scanned\": [{}, {}],\n", start_height, covered_to));
    j.push_str("  \"note\": \"");
    j.push_str("regular txs carry zero bare-P2PK outputs on this chain (sig_probe full-archive pass); ");
    j.push_str("p2pk-prev entries spend coinstake bare-pubkey outputs; spenders are always regular txs");
    j.push_str("\",\n");
    j.push_str(&format!("  \"strict_verified\": {},\n", sigs_total));
    j.push_str(&format!("  \"strict_verified_p2pk_prev\": {},\n", sigs_p2pk_prev));
    j.push_str(&format!("  \"strict_verified_p2pkh_prev\": {},\n", sigs_p2pkh_prev));
    j.push_str(&format!("  \"lax_verified\": {},\n", lax_verified));
    j.push_str(&format!("  \"unverifiable_skipped\": {},\n", unverifiable_skipped));
    j.push_str(&format!("  \"hard_failures\": {},\n", hard_failures));
    j.push_str(&format!(
        "  \"broken_era_failures\": {},\n",
        broken_era_failures
    ));
    j.push_str("  \"broken_era_note\": \"");
    j.push_str("ECDSA-failing signatures at h<=17691 (CRAPCHAIN era, pre-strict-ECDSA, ");
    j.push_str("accepted by the live daemon under assume-valid; root-caused — not re-opened)");
    j.push_str("\",\n");
    j.push_str("  \"broken_era_samples\": [\n");
    j.push_str(&debug_samples.join(",\n"));
    j.push_str("\n  ],\n");
    j.push_str(&format!("  \"candidate_single_sig_inputs\": {},\n", candidates_seen));
    j.push_str(&format!("  \"prevouts_not_cached\": {},\n", prevs_not_cached));
    j.push_str("  \"signatures\": [\n");
    j.push_str(&corpus.join(",\n"));
    j.push_str("\n  ]\n}\n");
    std::fs::create_dir_all(std::path::Path::new(&out_path).parent().unwrap())
        .unwrap_or_else(|e| die(&format!("mkdir: {}", e)));
    let tmp = format!("{}.tmp", out_path);
    std::fs::write(&tmp, &j).unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
    std::fs::rename(&tmp, &out_path).unwrap_or_else(|e| die(&format!("rename {}: {}", out_path, e)));

    println!(
        "verified: {} strict (+{} lax) historical signatures over heights {}..{} \
         (p2pk-prev {}, p2pkh-prev {}), {} hard failures, {} unverifiable skipped, corpus → {}",
        sigs_total, lax_verified, start_height, covered_to, sigs_p2pk_prev,
        sigs_p2pkh_prev, hard_failures, unverifiable_skipped, out_path
    );
    println!("HISTORICAL SIGNATURE CORPUS PASSED");
}
