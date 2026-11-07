//! Phase 1e: per-address balances from full-archive replay (CLI form).
//!
//! The replay/aggregate/write engine lives in tri7_core::balances_build —
//! this bin is the standalone CLI over it (the tri7d `--balances` builder
//! thread calls the SAME function in-process, so the snapshot a node serves
//! through the native `richlist` verb and the one this bin writes can never
//! drift).
//!
//! Replays the live chain (genesis..stale-branch stop) and reports:
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
//!                [--out path] [--utxos-out path]
//! Exit 0 ONLY if: the wallet was read with at least one key (when given),
//! at least one non-empty output was processed (coverage), and the replay
//! reached end_height (or stopped at the stale branch WITH --allow-stale).
//! Anomaly counters (untracked spends / duplicate txids) are reported in the
//! JSON, never silently dropped.

use std::io::Write;
use tri7_core::balances_build::{build_balances_snapshot, BalancesBuildOpts};

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
        .and_then(|i| args.get(i + 1).cloned());
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

    let mut progress = |blocks: usize, live_utxos: usize| {
        println!("... {} blocks ({} live UTXOs)", blocks, live_utxos);
        let _ = std::io::stdout().flush();
    };
    let opts = BalancesBuildOpts {
        blk: std::path::Path::new(&path),
        out: std::path::Path::new(&out_path),
        wallet: wallet_path.as_deref().map(std::path::Path::new),
        utxos_out: utxos_out_path.as_deref().map(std::path::Path::new),
        end_height,
        allow_stale,
    };
    let rep = match build_balances_snapshot(&opts, &mut progress) {
        Ok(r) => r,
        Err(e) => die(&e),
    };

    // ---- report --------------------------------------------------------------
    println!(
        "replay: {} blocks (genesis..{}), tip {}",
        rep.blocks, rep.covered_to, rep.tip_hash_display
    );
    println!(
        "outputs: {} non-empty processed, {} attributed to an address, {} live UTXOs held",
        rep.outputs_processed, rep.outputs_attributed, rep.live_utxos
    );
    println!(
        "supply: total live balance {} sats ({} attributed + {} unattributed in {} UTXOs)",
        rep.total_balance_sats, rep.attributed_sats, rep.unattributed_sats, rep.unattributed_utxos
    );
    println!(
        "anomalies: {} untracked spends, {} duplicate-txid overwrites",
        rep.untracked_spends, rep.duplicate_txids
    );
    if let Some(dump_path) = &rep.utxos_out_path {
        println!(
            "utxo dump: {} wallet-owned live UTXOs ({} sats) -> {}",
            rep.utxos_count, rep.utxos_total_sats, dump_path.display()
        );
    }
    println!(
        "wallet: {} keys / {} distinct addresses, {} funded; balance {} sats in {} UTXOs; received(regular/P2PKH) {} sats; received(all) {} sats",
        rep.wallet_key_count,
        rep.wallet_addresses_distinct,
        rep.wallet_funded_addresses,
        rep.wallet_balance_sats,
        rep.wallet_utxos,
        rep.wallet_received_regular_sats,
        rep.wallet_received_all_sats
    );
    // top 10 wallet balances for eyeballing — re-read them from the snapshot
    // (the aggregation stays in the lib; the CLI keeps only display extras)
    if let Ok(v) =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&rep.out_path).unwrap_or_default())
    {
        let mut top: Vec<(i64, String)> = v["wallet"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| {
                        let b = w["balance_sats"].as_i64()?;
                        if b <= 0 {
                            return None;
                        }
                        let addr = w["address"].as_str()?.to_string();
                        Some((b, addr))
                    })
                    .collect()
            })
            .unwrap_or_default();
        top.sort_by(|a, b| b.0.cmp(&a.0));
        if !top.is_empty() {
            println!("top wallet balances (sats):");
            for (bal, addr) in top.iter().take(10) {
                println!("  {:>16}  {}", bal, addr);
            }
        }
    }
    println!("JSON written: {}", rep.out_path.display());
    println!("ALL BALANCE CHECKS PASSED");
}
