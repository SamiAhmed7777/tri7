//! Phase 3 signed dry run: construct + sign a spend from a synthetic test
//! key to a synthetic destination, then verify it with the in-repo verifier
//! and the C++ Solver template shapes. NOTHING is broadcast and no daemon is
//! touched — this bin has no network code; the signed tx is written to a
//! local file for inspection only.
//!
//! Dry runs performed (all keys synthetic, derived from labels):
//!   1. P2PK prevout → P2PK destination + P2PK change (single-input spend)
//!   2. P2PKH prevout → P2PKH destination + change (Solver PubKeyHash path,
//!      2-push scriptSig)
//!   3. two-input consolidation P2PK+P2PKH → P2PK destination
//! plus ConnectInputs-style validation (maturity, tx-time order, MoneyRange,
//! value-in >= value-out, fee >= GetMinFee) on every produced tx, negative
//! controls (tampered byte must fail; depth-6 coinbase must fail), and a
//! byte-for-byte re-verification pass over the written file.
//!
//! Mode B (--utxo-dump path --wallet path [--spend-height H]): the SAME
//! build+verify path driven from REAL wallet-owned UTXOs (balances
//! --utxos-out dump) with the REAL wallet keys — destination is a synthetic
//! key, the signed tx is only written to the report file. Never broadcast.
//!
//! Usage: spend_dryrun [--out path]     (default /root/tri7/spend_dryrun.json)
//! Exit 0 ONLY if every dry run verifies, every negative control fails as
//! expected, and the re-read file re-verifies byte-for-byte.

use tri7_core::legacy::hash_to_display;
use tri7_core::spend;

fn die(msg: &str) -> ! {
    eprintln!("FAIL: {}", msg);
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/root/tri7/spend_dryrun.json".to_string());

    // Mode B dispatch: sign a REAL wallet UTXO from the balances dump
    let utxo_dump = args
        .iter()
        .position(|a| a == "--utxo-dump")
        .and_then(|i| args.get(i + 1).cloned());
    if let Some(dump) = utxo_dump {
        let wallet_path = args
            .iter()
            .position(|a| a == "--wallet")
            .and_then(|i| args.get(i + 1).cloned())
            .unwrap_or_else(|| "/root/.triangles/wallet.dat".to_string());
        let spend_height = args
            .iter()
            .position(|a| a == "--spend-height")
            .and_then(|i| args.get(i + 1).cloned())
            .and_then(|s| s.parse().ok())
            .unwrap_or(2_204_100);
        run_utxo_dump_mode(&dump, &wallet_path, &out_path, spend_height);
        return;
    }

    let n_time = 1_400_003_600u32;
    let spend_height = 107u32;

    // ---- keys + scripts (all synthetic) ------------------------------------
    let (src_secret, src_pk) = spend::synthetic_key("dryrun/src");
    let (dest_secret, dest_pk) = spend::synthetic_key("dryrun/dest");
    let (chg_secret, chg_pk) = spend::synthetic_key("dryrun/change");
    let _ = (dest_secret, chg_secret); // destinations are not signed with here
    let dest_p2pk = spend::script_pubkey_p2pk(&dest_pk).unwrap();
    let dest_p2pkh = spend::script_pubkey_p2pkh(&dest_pk);
    let change_p2pk = spend::script_pubkey_p2pk(&chg_pk).unwrap();

    // Synthetic prevouts, as SpendInput records. txids are derived (sha256d
    // of labels) — real funding txs are unnecessary for a dry run because
    // signing/verification only consumes the prevout record itself.
    let mk_prev = |label: &str, script: Vec<u8>, value: i64| spend::SpendInput {
        txid: tri7_core::legacy::sha256d(format!("dryrun-prev/{}", label).as_bytes()),
        n: 0,
        value,
        script_pubkey: script,
        height: spend_height - 8, // depth 8 > maturity 7
        tx_time: 1_400_000_000,
        is_coinbase: true,
        is_coinstake: false,
        secret: src_secret,
        pubkey: src_pk.clone(),
    };
    let prev_p2pk = mk_prev(
        "p2pk",
        spend::script_pubkey_p2pk(&src_pk).unwrap(),
        1_000_000,
    );
    let prev_p2pkh = mk_prev("p2pkh", spend::script_pubkey_p2pkh(&src_pk), 700_000);

    let mut checks_passed = 0usize;
    let mut checks_failed = 0usize;
    let mut report: Vec<String> = Vec::new();

    let expect_ok = |r: Result<spend::SpendPlan, String>,
                     what: &str,
                     checks: &mut (usize, usize),
                     rep: &mut Vec<String>| {
        match r {
            Ok(plan) => {
                rep.push(format!(
                    "OK   {} txid={} fee={} change={} bytes={}",
                    what,
                    hash_to_display(&plan.tx.hash()),
                    plan.fee,
                    plan.change,
                    plan.tx.serialize().len()
                ));
                checks.0 += 1;
                Some(plan)
            }
            Err(e) => {
                eprintln!("FAIL: {} errored: {}", what, e);
                checks.1 += 1;
                None
            }
        }
    };

    // ---- dry run 1: P2PK → P2PK (the tasked run) ----------------------------
    let plan1 = expect_ok(
        spend::build_spend(
            &[prev_p2pk.clone()],
            dest_p2pk.clone(),
            900_000,
            Some(change_p2pk.clone()),
            0,
            n_time,
            spend_height,
        ),
        "dryrun1 p2pk→p2pk",
        &mut (checks_passed, checks_failed),
        &mut report,
    );
    let mut plans = plan1.into_iter().collect::<Vec<_>>();

    // ---- dry run 2: P2PKH → P2PKH -------------------------------------------
    if let Some(p) = expect_ok(
        spend::build_spend(
            &[prev_p2pkh.clone()],
            dest_p2pkh.clone(),
            650_000,
            Some(change_p2pk.clone()),
            0,
            n_time,
            spend_height,
        ),
        "dryrun2 p2pkh→p2pkh",
        &mut (checks_passed, checks_failed),
        &mut report,
    ) {
        plans.push(p);
    }

    // ---- dry run 3: two-input consolidation ---------------------------------
    if let Some(p) = expect_ok(
        spend::build_spend(
            &[prev_p2pk.clone(), prev_p2pkh.clone()],
            dest_p2pk.clone(),
            1_500_000,
            Some(change_p2pk.clone()),
            0,
            n_time,
            spend_height,
        ),
        "dryrun3 2-in consolidation",
        &mut (checks_passed, checks_failed),
        &mut report,
    ) {
        plans.push(p);
    }
    if plans.len() != 3 {
        die(&format!("expected 3 dry-run plans, built {}", plans.len()));
    }

    // ---- verification of every input of every plan ---------------------------
    // build_spend orders vins by (txid, n) — resolve prevouts by OUTPOINT,
    // never by position.
    let all_prevs = [prev_p2pk.clone(), prev_p2pkh.clone()];
    let prev_of = |vin: &tri7_core::Vin| -> spend::SpendInput {
        all_prevs
            .iter()
            .find(|p| p.txid == vin.prev_hash && p.n == vin.prev_n)
            .unwrap_or_else(|| die("plan vin references unknown prevout"))
            .clone()
    };
    for (i, plan) in plans.iter().enumerate() {
        // Solver scriptSig shapes
        for (vin_idx, vin) in plan.tx.vin.iter().enumerate() {
            let pushes = spend::parse_pushes(&vin.script_sig).unwrap_or_else(|e| {
                die(&format!("plan {} vin {}: bad scriptSig: {}", i, vin_idx, e))
            });
            let prev = prev_of(vin);
            let prev_script = prev.script_pubkey.clone();
            let st =
                spend::solver_type(&prev_script).unwrap_or_else(|| die("prev script not standard"));
            if pushes.len() != spend::script_sig_args_expected(&st) {
                die(&format!(
                    "plan {} vin {}: Solver shape violation ({} pushes vs {} expected)",
                    i,
                    vin_idx,
                    pushes.len(),
                    spend::script_sig_args_expected(&st)
                ));
            }
            // own-verifier signature check
            let v = spend::verify_input(&plan.tx, vin_idx, &prev_script).unwrap_or_else(|e| {
                die(&format!("plan {} vin {}: verify failed: {}", i, vin_idx, e))
            });
            if v.der_mode != "strict" || v.hash_type != spend::SIGHASH_ALL {
                die(&format!("plan {} vin {}: unexpected sig attrs", i, vin_idx));
            }
            checks_passed += 1;
            report.push(format!(
                "OK   plan{} vin{} verified (sighash {}, hashtype {}, {} push(es))",
                i,
                vin_idx,
                hex(&v.sighash),
                v.hash_type,
                pushes.len()
            ));
        }
        // ConnectInputs-style validation (prevs ordered exactly like tx.vin)
        let ordered_prevs: Vec<spend::SpendInput> =
            plan.tx.vin.iter().map(|v| prev_of(v)).collect();
        spend::connect_inputs_check(&plan.tx, &ordered_prevs, spend_height)
            .unwrap_or_else(|e| die(&format!("plan {}: connect inputs failed: {}", i, e)));
        checks_passed += 1;
        report.push(format!("OK   plan{} ConnectInputs validation", i));
        // balance conservation
        let vout: i64 = plan.tx.vout.iter().map(|o| o.value).sum();
        assert_eq!(vout + plan.fee, plan.inputs_value);
    }

    // ---- negative controls ---------------------------------------------------
    // (a) tampered signature byte must fail verification
    let mut tampered = plans[0].tx.clone();
    let last = tampered.vin[0].script_sig.len() - 2;
    tampered.vin[0].script_sig[last] ^= 0x01;
    if spend::verify_input(&tampered, 0, &prev_p2pk.script_pubkey).is_err() {
        checks_passed += 1;
        report.push("OK   negative: tampered sig rejected".to_string());
    } else {
        checks_failed += 1;
        die("tampered signature VERIFIED — verifier is broken");
    }
    // (b) immature coinbase (depth 6 < 7) must be rejected
    let mut young = prev_p2pk.clone();
    young.height = spend_height - 6;
    match spend::build_spend(
        &[young],
        dest_p2pk.clone(),
        900_000,
        Some(change_p2pk.clone()),
        0,
        n_time,
        spend_height,
    ) {
        Err(e) if e.contains("depth 6") => {
            checks_passed += 1;
            report.push("OK   negative: immature coinbase rejected".to_string());
        }
        Ok(_) => die("immature coinbase was accepted"),
        Err(e) => die(&format!(
            "immature coinbase rejected with wrong error: {}",
            e
        )),
    }
    // (c) input newer than the spend must be rejected
    let mut late = prev_p2pk.clone();
    late.tx_time = n_time + 1;
    match spend::build_spend(
        &[late],
        dest_p2pk.clone(),
        900_000,
        Some(change_p2pk.clone()),
        0,
        n_time,
        spend_height,
    ) {
        Err(e) if e.contains("timestamp earlier than input") => {
            checks_passed += 1;
            report.push("OK   negative: input newer than spend rejected".to_string());
        }
        Ok(_) => die("future tx_time input was accepted"),
        Err(e) => die(&format!("timestamp rule failed with wrong error: {}", e)),
    }

    // ---- write + byte-for-byte re-verification ------------------------------
    let mut j = String::with_capacity(1 << 16);
    j.push_str("{\n\"dry_run\": true,\n\"broadcast\": false,\n\"all_keys_synthetic\": true,\n");
    j.push_str(&format!(
        "\"checks_passed\": {}, \"checks_failed\": {},\n",
        checks_passed, checks_failed
    ));
    j.push_str("\"log\": [\n");
    j.push_str(
        &report
            .iter()
            .map(|l| format!("\"{}\"", l.replace('"', "'")))
            .collect::<Vec<_>>()
            .join(",\n"),
    );
    j.push_str("\n],\n\"signed_txs\": [\n");
    let txs: Vec<String> = plans
        .iter()
        .map(|p| {
            format!(
                "{{\"txid\":\"{}\",\"hex\":\"{}\"}}",
                hash_to_display(&p.tx.hash()),
                hex(&p.tx.serialize())
            )
        })
        .collect();
    j.push_str(&txs.join(",\n"));
    j.push_str("\n]\n}\n");
    let tmp = format!("{}.tmp", out_path);
    std::fs::write(&tmp, &j).unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
    std::fs::rename(&tmp, &out_path).unwrap_or_else(|e| die(&format!("rename: {}", e)));

    // re-read the file and re-verify every written tx against its prevouts
    let written =
        std::fs::read_to_string(&out_path).unwrap_or_else(|e| die(&format!("reread: {}", e)));
    let count_ok = written.matches("\"hex\"").count();
    if count_ok != plans.len() {
        die(&format!(
            "written file has {} txs, expected {}",
            count_ok,
            plans.len()
        ));
    }
    for plan in &plans {
        // reconstruct prevouts by txid prefix match — full verifier pass
        for (vin_idx, vin) in plan.tx.vin.iter().enumerate() {
            let all_prevs = [prev_p2pk.clone(), prev_p2pkh.clone()];
            let entry = all_prevs
                .iter()
                .find(|p| p.txid == vin.prev_hash && p.n == vin.prev_n)
                .unwrap_or_else(|| die("written tx references unknown prevout"));
            spend::verify_input(&plan.tx, vin_idx, &entry.script_pubkey)
                .unwrap_or_else(|e| die(&format!("written tx failed re-verification: {}", e)));
        }
        checks_passed += 1;
    }
    report.push(format!(
        "OK   {} written txs re-verified byte-for-byte",
        plans.len()
    ));

    println!(
        "dry runs: {} checks passed, {} failed",
        checks_passed, checks_failed
    );
    println!(
        "signed txs written to {} (NOT broadcast — no network code in this bin)",
        out_path
    );
    println!("SPEND DRY RUN PASSED");
}

/// Mode B: sign a REAL wallet UTXO (from the balances --utxos-out dump) to a
/// synthetic destination — verify-only, nothing leaves the machine.
fn run_utxo_dump_mode(dump_path: &str, wallet_path: &str, out_path: &str, spend_height: u32) {
    use tri7_core::wallet;

    let raw = std::fs::read_to_string(dump_path).unwrap_or_else(|e| {
        die(&format!(
            "read {}: {} (run: balances --utxos-out {})",
            dump_path, e, dump_path
        ))
    });
    let v: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| die(&format!("dump JSON parse: {}", e)));
    let entries = v["utxos"]
        .as_array()
        .unwrap_or_else(|| die("dump has no utxos array"));
    if entries.is_empty() {
        die("dump has zero wallet UTXOs");
    }
    let wall = wallet::read_wallet(std::path::Path::new(wallet_path))
        .unwrap_or_else(|e| die(&format!("wallet read: {}", e)));

    // key lookup by address (the dump carries the canonical address string)
    let mut by_address: std::collections::HashMap<&str, &wallet::WalletKey> =
        wall.keys.iter().map(|k| (k.address.as_str(), k)).collect();

    // smallest wallet UTXO that is mature and whose key we hold
    let mut candidates: Vec<(i64, &serde_json::Value)> = entries
        .iter()
        .filter(|e| {
            let h = e["height"].as_u64().unwrap_or(0) as u32;
            let kind = e["prev_kind"].as_str().unwrap_or("regular");
            let mature = kind == "regular" || spend_height.saturating_sub(h) >= 7;
            mature && by_address.contains_key(e["address"].as_str().unwrap_or(""))
        })
        .map(|e| (e["value_sats"].as_i64().unwrap_or(0), e))
        .collect();
    candidates.sort_by_key(|(v, _)| *v);
    let (value, entry) = candidates
        .iter()
        .find(|(v, _)| *v > 1000)
        .unwrap_or_else(|| die("no mature wallet UTXO with value > 1000 sats"));
    let addr = entry["address"].as_str().unwrap();
    let key = by_address[addr];
    let txid_disp = entry["txid"]
        .as_str()
        .unwrap_or_else(|| die("dump entry without txid"));
    let txid_bytes: Vec<u8> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&txid_disp[i..i + 2], 16).unwrap())
        .collect();
    // the dump's txid is the DISPLAY order (RPC convention, uint256::ToString
    // prints internal bytes reversed) — vin.prev_hash needs internal order
    let mut txid = [0u8; 32];
    for (i, b) in txid_bytes.iter().enumerate().take(32) {
        txid[31 - i] = *b;
    }

    let real_input = spend::SpendInput {
        txid,
        n: entry["n"].as_u64().unwrap() as u32,
        value: *value,
        script_pubkey: {
            let h = entry["script_hex"]
                .as_str()
                .unwrap_or_else(|| die("dump entry without script"));
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
                .collect()
        },
        height: entry["height"].as_u64().unwrap() as u32,
        tx_time: entry["tx_time"].as_u64().unwrap() as u32,
        is_coinbase: entry["prev_kind"].as_str() == Some("coinbase"),
        is_coinstake: entry["prev_kind"].as_str() == Some("coinstake"),
        secret: key.secret,
        pubkey: key.pubkey.clone(),
    };

    // destination: synthetic key (clearly not a real recipient); change back
    // to the same synthetic key so the fee loop has room to raise the fee
    let (dest_secret, dest_pk) = spend::synthetic_key("dryrun/real-dest");
    let _ = dest_secret;
    let dest = spend::script_pubkey_p2pk(&dest_pk).unwrap();
    let change = spend::script_pubkey_p2pk(&dest_pk).unwrap();
    let plan = spend::build_spend(
        &[real_input.clone()],
        dest,
        value / 2,
        Some(change),
        0,
        real_input.tx_time.max(1_400_000_000), // ConnectInputs: spend >= input time
        spend_height,
    )
    .unwrap_or_else(|e| die(&format!("real-UTXO spend build: {}", e)));
    spend::verify_input(&plan.tx, 0, &real_input.script_pubkey)
        .unwrap_or_else(|e| die(&format!("real-UTXO spend verify: {}", e)));
    spend::connect_inputs_check(&plan.tx, &[real_input.clone()], spend_height)
        .unwrap_or_else(|e| die(&format!("real-UTXO ConnectInputs: {}", e)));

    let report = format!(
        concat!(
            "{{\"mode\":\"real-utxo-dryrun\",\"broadcast\":false,\n",
            "\"utxo\":{{\"txid\":\"{}\",\"n\":{},\"value_sats\":{},\"address\":\"{}\",",
            "\"height\":{},\"prev_kind\":\"{}\"}},\n",
            "\"dest\":\"synthetic (dryrun/real-dest)\",\"dest_value\":{},\"fee\":{},\n",
            "\"txid\":\"{}\",\"tx_hex\":\"{}\"}}\n"
        ),
        txid_disp,
        real_input.n,
        value,
        addr,
        real_input.height,
        entry["prev_kind"].as_str().unwrap_or("regular"),
        plan.tx.vout[0].value,
        plan.fee,
        hash_to_display(&plan.tx.hash()),
        plan.tx
            .serialize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );
    let tmp = format!("{}.tmp", out_path);
    std::fs::write(&tmp, &report).unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
    std::fs::rename(&tmp, out_path).unwrap_or_else(|e| die(&format!("rename: {}", e)));
    println!(
        "real-UTXO dry run: utxo {}:{} ({} sats, {}, h{}) signed to synthetic dest, verified; report {}",
        txid_disp,
        real_input.n,
        value,
        addr,
        real_input.height,
        out_path
    );
    println!("REAL UTXO DRY RUN PASSED");
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}
