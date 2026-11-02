//! Phase 1e: per-address balances from full-archive replay.
//!
//! Replays the live chain (genesis..stale-branch stop) over the shared
//! ArchiveWalker skeleton, maintaining the live UTXO set with per-output
//! address payloads (kernel::UtxoSet — bare-pubkey / P2PKH / P2SH extraction
//! in tri7_core::address), and reports:
//!
//!   1. Total chain balance — the sum of all live UTXO values. The deployed
//!      daemon tracks the same quantity as nMoneySupply (running
//!      nValueOut − nValueIn per active-chain block, main.cpp:2497), so at
//!      equal heights replay total == daemon moneysupply exactly.
//!   2. Per-address live balances for every address derivable from the live
//!      wallet (248 keys → pubkey → hash160 → version-65 address).
//!   3. Per-address received totals in the EXACT semantics of the daemon's
//!      getreceivedbyaddress <addr> 0 (rpcwallet.cpp:497-536): regular txs
//!      only (coinbase AND coinstake excluded), P2PKH scripts only (the RPC
//!      compares scriptPubKey bytes against the address's P2PKH script), all
//!      in-wallet txs at depth >= 0 → live-chain outputs only. The all-script
//!      all-tx received total is reported alongside as a diagnostic.
//!
//! Non-empty outputs with no recognizable destination (OP_RETURN, multisig,
//! non-standard) are counted and their value bucketed as unattributed — they
//! stay part of the total chain balance.
//!
//! Usage: balances [blk_file] [end_height] [--allow-stale] [--wallet path]
//!                [--out path]
//! Exit 0 ONLY if: the wallet was read with at least one key, at least one
//! non-empty output was processed (coverage), and the replay reached
//! end_height (or stopped at the stale branch WITH --allow-stale).
//! Anomaly counters (untracked spends / duplicate txids) are reported in the
//! JSON, never silently dropped.

use tri7_core::address::{payload_to_address, pubkey_payload};
use tri7_core::consensus;
use tri7_core::kernel::UtxoSet;
use tri7_core::replay::{check_coverage, ArchiveWalker, Step};
use tri7_core::wallet;
use std::collections::HashMap;

/// One live wallet-owned UTXO (phase-3 spend record for --utxos-out).
struct WalletUtxo {
    value: i64,
    script: Vec<u8>,
    height: u32,
    tx_time: u32,
    from_coinbase: bool,
    from_coinstake: bool,
}

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
    let wallet_path = args
        .iter()
        .position(|a| a == "--wallet")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/root/.triangles/wallet.dat".to_string());
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/root/tri7/balances.json".to_string());
    // Optional phase-3 dump: every LIVE wallet-owned UTXO as a spend record
    // (script bytes included, so spend:: can sign it directly).
    let utxos_out_path = args
        .iter()
        .position(|a| a == "--utxos-out")
        .and_then(|i| args.get(i + 1).cloned());

    // ---- wallet: every address derivable from the live keys ----------------
    let wall = wallet::read_wallet(std::path::Path::new(&wallet_path))
        .unwrap_or_else(|e| die(&format!("wallet read failed: {}", e)));
    if wall.keys.is_empty() {
        die("wallet has zero keys");
    }
    // (payload, canonical address string) per key — payloads may in principle
    // collide (same key imported twice); distinct payloads are what matters.
    let mut wallet_payloads: Vec<([u8; 21], String)> = wall
        .keys
        .iter()
        .map(|k| (pubkey_payload(&k.pubkey), k.address.clone()))
        .collect();
    for (payload, addr) in &wallet_payloads {
        debug_assert_eq!(&payload_to_address(payload), addr);
    }
    wallet_payloads.sort();
    wallet_payloads.dedup();
    let distinct_wallet_payloads = wallet_payloads.len();

    // ---- replay ------------------------------------------------------------
    let mut walker = ArchiveWalker::open(&path).unwrap_or_else(|e| die(&e));
    let mut utxos = UtxoSet::new();
    // daemon getreceivedbyaddress semantics (regular txs, P2PKH scripts only)
    let mut received_regular: HashMap<[u8; 21], i64> = HashMap::new();
    // diagnostic: every non-empty output with a destination, any tx kind
    let mut received_all: HashMap<[u8; 21], i64> = HashMap::new();

    let mut stale_at: Option<usize> = None;
    let mut blocks = 0usize;
    let mut pos_blocks = 0usize;
    let mut outputs_processed = 0u64; // every non-empty vout seen on the live chain
    let mut outputs_attributed = 0u64; // ...whose script resolves to an address
    let mut regular_txs = 0u64;
    let mut untracked_spends = 0usize;
    let mut duplicate_txids = 0usize;
    let mut tip_hash = [0u8; 32];
    // wallet-owned live UTXOs for the --utxos-out dump (same lifecycle rules
    // as UtxoSet: per-tx all-or-nothing spend removal, IsEmpty never tracked)
    let mut wallet_utxo_set: HashMap<(tri7_core::legacy::Hash, u32), WalletUtxo> = HashMap::new();
    let wallet_payload_set: std::collections::HashSet<[u8; 21]> =
        wallet_payloads.iter().map(|(p, _)| *p).collect();

    loop {
        let (_height, block, hash) = match walker.step() {
            Err(e) => die(&e),
            Ok(Step::Eof) => break,
            Ok(Step::StaleBranch(h)) => {
                stale_at = Some(h);
                break;
            }
            Ok(Step::Block(h, b, hash)) => (h, b, hash),
        };

        for tx in &block.vtx {
            let is_coinbase = consensus::tx_is_coin_base(tx);
            let is_coinstake = consensus::tx_is_coin_stake(tx);
            if !is_coinbase && !is_coinstake {
                regular_txs += 1;
            }
            for vout in &tx.vout {
                // CTxOut::IsEmpty(): the coinstake vout[0] marker (value 0,
                // empty script) creates no UTXO and carries no address.
                if vout.value == 0 && vout.script_pubkey.is_empty() {
                    continue;
                }
                outputs_processed += 1;
                let dest = match tri7_core::address::script_to_dest(&vout.script_pubkey) {
                    Some(d) => d,
                    None => continue,
                };
                outputs_attributed += 1;
                let payload = dest.payload();
                *received_all.entry(payload).or_insert(0) += vout.value;
                // getreceivedbyaddress: coinbase/coinstake txs are skipped
                // outright; P2PKH is the only form its script equality hits.
                if !is_coinbase && !is_coinstake && dest.is_pubkey_hash() {
                    *received_regular.entry(payload).or_insert(0) += vout.value;
                }
            }
        }

        // wallet-owned UTXO lifecycle, mirroring UtxoSet::apply_block_reported
        // (per-tx all-or-nothing spend removal; runs BEFORE apply_block so the
        // "all vins resolve" check reads the pre-application set)
        for tx in &block.vtx {
            let is_coinbase = consensus::tx_is_coin_base(tx);
            let is_coinstake = consensus::tx_is_coin_stake(tx);
            if !is_coinbase {
                let all_resolve = tx
                    .vin
                    .iter()
                    .all(|vin| utxos.get(&(vin.prev_hash, vin.prev_n)).is_some());
                if all_resolve {
                    for vin in &tx.vin {
                        wallet_utxo_set.remove(&(vin.prev_hash, vin.prev_n));
                    }
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                if vout.value == 0 && vout.script_pubkey.is_empty() {
                    continue; // IsEmpty marker
                }
                let Some(dest) = tri7_core::address::script_to_dest(&vout.script_pubkey) else {
                    continue;
                };
                let payload = dest.payload();
                if wallet_payload_set.contains(&payload) {
                    wallet_utxo_set.insert(
                        (tx.hash(), n as u32),
                        WalletUtxo {
                            value: vout.value,
                            script: vout.script_pubkey.clone(),
                            height: _height as u32,
                            tx_time: tx.time,
                            from_coinbase: is_coinbase,
                            from_coinstake: is_coinstake,
                        },
                    );
                }
            }
        }

        let rep = utxos.apply_block_reported(&block, _height);
        untracked_spends += rep.untracked_spends;
        duplicate_txids += rep.duplicate_txids;

        blocks += 1;
        tip_hash = hash;
        if consensus::block_is_proof_of_stake(&block) {
            pos_blocks += 1;
        }
        if blocks % 100_000 == 0 {
            println!(
                "... {} blocks ({} outputs, {} live UTXOs)",
                blocks,
                outputs_processed,
                utxos.len()
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }

    // ---- coverage contract (shared skeleton) --------------------------------
    let covered_to = walker.height().saturating_sub(1);
    if outputs_processed == 0 {
        eprintln!(
            "FAIL: zero outputs processed over heights 0..{} — no coverage",
            covered_to
        );
        std::process::exit(1);
    }
    if let Err(e) = check_coverage(stale_at, covered_to, end_height, allow_stale) {
        eprintln!("FAIL: {}", e);
        std::process::exit(1);
    }

    // ---- aggregate ----------------------------------------------------------
    let totals = utxos.address_balances();
    let attributed_sats: i64 = totals.per_address.values().map(|(v, _)| v).sum();
    let attributed_utxos: u64 = totals.per_address.values().map(|(_, n)| n).sum();
    let total_balance_sats = attributed_sats + totals.unattributed_sats;
    let live_utxos = attributed_utxos + totals.unattributed_utxos;

    // wallet slice
    let mut wallet_balance_sats = 0i64;
    let mut wallet_utxos = 0u64;
    let mut wallet_received_regular_sats = 0i64;
    let mut wallet_received_all_sats = 0i64;
    let mut funded_wallet_addresses = 0usize;
    let mut wallet_rows: Vec<[String; 5]> = Vec::with_capacity(wallet_payloads.len());
    for (payload, addr) in &wallet_payloads {
        let (bal, n) = totals.per_address.get(payload).copied().unwrap_or((0, 0));
        let rec_r = received_regular.get(payload).copied().unwrap_or(0);
        let rec_a = received_all.get(payload).copied().unwrap_or(0);
        wallet_balance_sats += bal;
        wallet_utxos += n;
        wallet_received_regular_sats += rec_r;
        wallet_received_all_sats += rec_a;
        if bal != 0 || rec_r != 0 || rec_a != 0 {
            funded_wallet_addresses += 1;
        }
        wallet_rows.push([
            addr.clone(),
            bal.to_string(),
            n.to_string(),
            rec_r.to_string(),
            rec_a.to_string(),
        ]);
    }

    // ---- JSON ----------------------------------------------------------------
    // {"totals":{...}, "wallet":[...], "balances":{addr:sats},
    //  "received_regular":{addr:sats}} — sorted keys, sats everywhere.
    let mut balances_sorted: Vec<(&[u8; 21], &(i64, u64))> = totals.per_address.iter().collect();
    balances_sorted.sort_by_key(|(p, _)| payload_to_address(p));
    let mut received_sorted: Vec<(&[u8; 21], &i64)> = received_regular.iter().collect();
    received_sorted.sort_by_key(|(p, _)| payload_to_address(p));

    let mut j = String::with_capacity(1 << 22);
    j.push_str("{\n");
    j.push_str(&format!("  \"height\": {},\n", covered_to));
    j.push_str(&format!(
        "  \"covered_tip_hash\": \"{}\",\n",
        tri7_core::legacy::hash_to_display(&tip_hash)
    ));
    j.push_str("  \"totals\": {\n");
    j.push_str(&format!("    \"blocks_replayed\": {},\n", blocks));
    j.push_str(&format!("    \"pos_blocks\": {},\n", pos_blocks));
    j.push_str(&format!("    \"live_utxos\": {},\n", live_utxos));
    j.push_str(&format!("    \"total_balance_sats\": {},\n", total_balance_sats));
    j.push_str(&format!(
        "    \"attributed_balance_sats\": {},\n",
        attributed_sats
    ));
    j.push_str(&format!(
        "    \"unattributed_balance_sats\": {},\n",
        totals.unattributed_sats
    ));
    j.push_str(&format!(
        "    \"unattributed_utxos\": {},\n",
        totals.unattributed_utxos
    ));
    j.push_str(&format!("    \"outputs_processed\": {},\n", outputs_processed));
    j.push_str(&format!(
        "    \"outputs_attributed_to_address\": {},\n",
        outputs_attributed
    ));
    j.push_str(&format!("    \"regular_txs\": {},\n", regular_txs));
    j.push_str(&format!("    \"untracked_spends\": {},\n", untracked_spends));
    j.push_str(&format!("    \"duplicate_txids\": {},\n", duplicate_txids));
    j.push_str(&format!(
        "    \"wallet_key_count\": {},\n",
        wall.keys.len()
    ));
    j.push_str(&format!(
        "    \"wallet_addresses_distinct\": {},\n",
        distinct_wallet_payloads
    ));
    j.push_str(&format!(
        "    \"wallet_funded_addresses\": {},\n",
        funded_wallet_addresses
    ));
    j.push_str(&format!(
        "    \"wallet_balance_sats\": {},\n",
        wallet_balance_sats
    ));
    j.push_str(&format!("    \"wallet_utxos\": {},\n", wallet_utxos));
    j.push_str(&format!(
        "    \"wallet_received_regular_sats\": {},\n",
        wallet_received_regular_sats
    ));
    j.push_str(&format!(
        "    \"wallet_received_all_sats\": {}\n",
        wallet_received_all_sats
    ));
    j.push_str("  },\n");
    j.push_str("  \"wallet\": [\n");
    for (i, row) in wallet_rows.iter().enumerate() {
        j.push_str(&format!(
            "    {{\"address\": \"{}\", \"balance_sats\": {}, \"utxos\": {}, \"received_regular_sats\": {}, \"received_all_sats\": {}}}{}\n",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            if i + 1 == wallet_rows.len() { "" } else { "," }
        ));
    }
    j.push_str("  ],\n");
    j.push_str("  \"balances\": {");
    for (i, (payload, (bal, _))) in balances_sorted.iter().enumerate() {
        j.push_str(&format!(
            "{}\"{}\":{}",
            if i % 6 == 0 { "\n    " } else { " " },
            payload_to_address(payload),
            bal
        ));
        if i + 1 != balances_sorted.len() {
            j.push(',');
        }
    }
    j.push_str("\n  },\n");
    j.push_str("  \"received_regular\": {");
    for (i, (payload, sats)) in received_sorted.iter().enumerate() {
        j.push_str(&format!(
            "{}\"{}\":{}",
            if i % 6 == 0 { "\n    " } else { " " },
            payload_to_address(payload),
            sats
        ));
        if i + 1 != received_sorted.len() {
            j.push(',');
        }
    }
    j.push_str("\n  }\n}\n");
    std::fs::write(&out_path, &j)
        .unwrap_or_else(|e| die(&format!("write {}: {}", out_path, e)));

    // ---- wallet UTXO dump (phase 3) ------------------------------------------
    if let Some(dump_path) = &utxos_out_path {
        let mut rows: Vec<(tri7_core::legacy::Hash, u32, &WalletUtxo)> = wallet_utxo_set
            .iter()
            .map(|((t, n), u)| (*t, *n, u))
            .collect();
        rows.sort_by(|a, b| {
            (a.2.height, tri7_core::legacy::hash_to_display(&a.0), a.1).cmp(&(
                b.2.height,
                tri7_core::legacy::hash_to_display(&b.0),
                b.1,
            ))
        });
        let mut d = String::with_capacity(1 << 20);
        d.push_str("{\n");
        d.push_str(&format!("  \"height\": {},\n", covered_to));
        d.push_str(&format!("  \"count\": {},\n", rows.len()));
        d.push_str(&format!(
            "  \"total_value_sats\": {},\n",
            rows.iter().map(|(_, _, u)| u.value).sum::<i64>()
        ));
        d.push_str("  \"utxos\": [\n");
        for (i, (txid, n, u)) in rows.iter().enumerate() {
            let payload = tri7_core::address::script_to_dest(&u.script).map(|dest| dest.payload());
            let addr = payload
                .map(|p| payload_to_address(&p))
                .unwrap_or_else(|| "".to_string());
            let kind = if u.from_coinbase {
                "coinbase"
            } else if u.from_coinstake {
                "coinstake"
            } else {
                "regular"
            };
            d.push_str(&format!(
                "    {{\"txid\": \"{}\", \"n\": {}, \"value_sats\": {}, \"script_hex\": \"{}\", \
                 \"address\": \"{}\", \"height\": {}, \"tx_time\": {}, \"prev_kind\": \"{}\"}}{}\n",
                tri7_core::legacy::hash_to_display(txid),
                n,
                u.value,
                u.script.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
                addr,
                u.height,
                u.tx_time,
                kind,
                if i + 1 == rows.len() { "" } else { "," }
            ));
        }
        d.push_str("  ]\n}\n");
        let tmp = format!("{}.tmp", dump_path);
        std::fs::write(&tmp, &d)
            .unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
        std::fs::rename(&tmp, dump_path)
            .unwrap_or_else(|e| die(&format!("rename {}: {}", dump_path, e)));
        println!(
            "utxo dump: {} wallet-owned live UTXOs ({} sats) -> {}",
            rows.len(),
            rows.iter().map(|(_, _, u)| u.value).sum::<i64>(),
            dump_path
        );
    }

    // ---- report --------------------------------------------------------------
    println!(
        "replay: {} blocks (genesis..{}), tip {}",
        blocks,
        covered_to,
        tri7_core::legacy::hash_to_display(&tip_hash)
    );
    println!(
        "outputs: {} non-empty processed, {} attributed to an address, {} live UTXOs held",
        outputs_processed, outputs_attributed, live_utxos
    );
    println!(
        "supply: total live balance {} sats ({} attributed + {} unattributed in {} UTXOs)",
        total_balance_sats, attributed_sats, totals.unattributed_sats, totals.unattributed_utxos
    );
    println!(
        "anomalies: {} untracked spends, {} duplicate-txid overwrites",
        untracked_spends, duplicate_txids
    );
    println!(
        "wallet: {} keys / {} distinct addresses, {} funded; balance {} sats in {} UTXOs; received(regular/P2PKH) {} sats; received(all) {} sats",
        wall.keys.len(),
        distinct_wallet_payloads,
        funded_wallet_addresses,
        wallet_balance_sats,
        wallet_utxos,
        wallet_received_regular_sats,
        wallet_received_all_sats
    );
    // top 10 wallet balances for eyeballing
    let mut top: Vec<(i64, u64, &String)> = Vec::new();
    for (payload, addr) in &wallet_payloads {
        let (bal, n) = totals.per_address.get(payload).copied().unwrap_or((0, 0));
        if bal > 0 {
            top.push((bal, n, addr));
        }
    }
    top.sort_by(|a, b| b.0.cmp(&a.0));
    println!("top wallet balances (sats):");
    for (bal, n, addr) in top.iter().take(10) {
        println!("  {:>16} in {:>3} UTXOs  {}", bal, n, addr);
    }
    println!("JSON written: {}", out_path);
    println!("ALL BALANCE CHECKS PASSED");
}
