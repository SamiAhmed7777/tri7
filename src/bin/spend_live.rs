//! spend_live — build + sign a REAL self-spend from a confirmed coinstake
//! output of the running node's own wallet, ready for sendrawtransaction.
//!
//! Phase 7 milestone 2 live-proof tool. The tx pays a SMALL amount to another
//! address OF THE SAME WALLET (to self), change returns to the source key,
//! fee at the floor. Nothing broadcasts here — the caller posts tx_hex to the
//! daemon's sendrawtransaction RPC.
//!
//! Why a freshly-minted coinstake output: it is unspent at tip BY
//! CONSTRUCTION (only this wallet's keys own it and nothing spends wallet
//! outputs here), while the h2201090 snapshots used by other tooling may
//! carry stale (already-spent) UTXOs.
//!
//! Usage:
//!   spend_live --wallet PATH --rpc-port 19112 [--height H]
//!              [--amount SATS] [--fee SATS] --out FILE
//! Exit 0 on a signed+verified tx (JSON written to --out), non-zero on any
//! failure. The wallet file is opened READ-ONLY.

use serde_json::{json, Value};
use tri7_core::legacy::{Reader, Tx};
use tri7_core::spend;

fn die(msg: &str) -> ! {
    eprintln!("spend_live: {}", msg);
    std::process::exit(3);
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

struct Rpc {
    port: u16,
}

impl Rpc {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
            .to_string();
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", self.port))
            .map_err(|e| format!("rpc connect: {}", e))?;
        let req = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        s.write_all(req.as_bytes())
            .map_err(|e| format!("rpc write: {}", e))?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf)
            .map_err(|e| format!("rpc read: {}", e))?;
        let text = String::from_utf8_lossy(&buf).to_string();
        let payload = text
            .split("\r\n\r\n")
            .nth(1)
            .ok_or_else(|| format!("rpc: malformed HTTP response"))?;
        let v: Value =
            serde_json::from_str(payload.trim()).map_err(|e| format!("rpc json: {}", e))?;
        if !v["error"].is_null() {
            return Err(format!("rpc {}: {}", method, v["error"]));
        }
        Ok(v["result"].clone())
    }
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wallet_path = arg(&args, "--wallet").unwrap_or_else(|| die("usage: spend_live --wallet PATH --rpc-port PORT [--height H] [--amount SATS] [--fee SATS] --out FILE"));
    let rpc_port: u16 = arg(&args, "--rpc-port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(19112);
    let amount: i64 = arg(&args, "--amount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000); // 1 TRI — "small amount to self"
    let fee: i64 = arg(&args, "--fee").and_then(|s| s.parse().ok()).unwrap_or(100); // fee floor
    let out_path = arg(&args, "--out").unwrap_or_else(|| "/root/tri7/spend_live.json".into());
    let rpc = Rpc { port: rpc_port };

    // 1. tip + target block (a few back so the coinstake output is deep and
    //    not the one the NEXT mint would most likely re-stake)
    let tip = rpc
        .call("getblockcount", json!([]))
        .unwrap_or_else(|e| die(&e))
        .as_i64()
        .unwrap_or_else(|| die("getblockcount: no result")) as i64;
    let height: i64 = arg(&args, "--height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(tip - 3);
    let blk = rpc
        .call("getblock", json!([height.to_string()]))
        .unwrap_or_else(|e| die(&e));
    if blk["flags"].as_str() != Some("proof-of-stake") {
        die(&format!("h{} is not a PoS block", height));
    }
    let txids: Vec<String> = blk["tx"]
        .as_array()
        .expect("getblock tx array")
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    if txids.len() < 2 {
        die("block has no coinstake tx");
    }

    // 2. fetch + parse the coinstake tx (vtx[1] of a PoS block)
    let raw = rpc
        .call("getrawtransaction", json!([txids[1]]))
        .unwrap_or_else(|e| die(&e))
        .as_str()
        .unwrap_or_else(|| die("getrawtransaction: no hex"))
        .to_string();
    let raw_bytes = hex_decode(&raw);
    let stake_tx = Tx::read(&mut Reader::new(&raw_bytes))
        .unwrap_or_else(|e| die(&format!("coinstake parse: {}", e)));

    // 3. wallet keys — read-only
    let wall = tri7_core::wallet::read_wallet(std::path::Path::new(&wallet_path))
        .unwrap_or_else(|e| die(&format!("wallet read: {}", e)));

    // 4. find the wallet-owned non-empty output of the coinstake.
    // Structural guard: on a PoS block vtx[1] IS the coinstake, and a
    // coinstake carries an empty-marker vout[0] (value 0 + empty script,
    // main.cpp IsEmpty) — refuse to spend from anything that doesn't match
    // (spend_live assumes the coinstake layout; a wrong guess here would
    // target an output we may not own).
    if stake_tx.vout.first().map(|v| v.value == 0 && v.script_pubkey.is_empty()) != Some(true) {
        die("tx does not look like a coinstake (vout[0] is not the empty marker)");
    }
    if stake_tx.vin.len() != 1 || stake_tx.vin[0].prev_hash == [0u8; 32] {
        die("tx does not look like a coinstake (single non-null stake input expected)");
    }
    let picked: Option<(usize, i64, Vec<u8>, usize)> = stake_tx
        .vout
        .iter()
        .enumerate()
        .skip(1) // vout[0] is the empty marker
        .find_map(|(n, vout)| {
            if vout.value <= 0 || vout.script_pubkey.is_empty() {
                return None;
            }
            wall.keys.iter().enumerate().find_map(|(ki, k)| {
                let p2pk = spend::script_pubkey_p2pk(&k.pubkey).unwrap();
                if vout.script_pubkey == p2pk
                    || vout.script_pubkey == spend::script_pubkey_p2pkh(&k.pubkey)
                {
                    Some((n, vout.value, vout.script_pubkey.clone(), ki))
                } else {
                    None
                }
            })
        });
    let (n_out, value, script, key_idx) =
        picked.unwrap_or_else(|| die("no wallet-owned output in the coinstake tx"));
    let src_key = &wall.keys[key_idx];
    let input_tx_time = stake_tx.time;

    // 5. destination: ANOTHER wallet address (to self), change → source key
    let dest_key = wall
        .keys
        .iter()
        .enumerate()
        .find(|(ki, _)| *ki != key_idx)
        .map(|(_, k)| k)
        .unwrap_or_else(|| die("wallet has only one key"));
    let dest_script = spend::script_pubkey_p2pkh(&dest_key.pubkey);
    let change_script = spend::script_pubkey_p2pkh(&src_key.pubkey);
    if value <= amount + fee {
        die(&format!(
            "input {} sats too small for amount {} + fee {}",
            value, amount, fee
        ));
    }

    // 6. build + sign + self-verify
    let input = spend::SpendInput {
        txid: {
            let mut h = [0u8; 32];
            let th = stake_tx.hash();
            h.copy_from_slice(&th);
            h
        },
        n: n_out as u32,
        value,
        script_pubkey: script.clone(),
        height: height as u32,
        tx_time: input_tx_time,
        is_coinbase: false,
        is_coinstake: false,
        secret: src_key.secret,
        pubkey: src_key.pubkey.clone(),
    };
    let plan = spend::build_spend(
        &[input.clone()],
        dest_script,
        amount,
        Some(change_script),
        fee,
        input.tx_time.max(1_400_000_000),
        height as u32,
    )
    .unwrap_or_else(|e| die(&format!("build_spend: {}", e)));
    spend::verify_input(&plan.tx, 0, &script)
        .unwrap_or_else(|e| die(&format!("verify_input: {}", e)));
    spend::connect_inputs_check(&plan.tx, &[input.clone()], height as u32)
        .unwrap_or_else(|e| die(&format!("connect_inputs_check: {}", e)));

    // 7. emit
    let txid_internal = plan.tx.hash();
    let mut disp = [0u8; 32];
    disp.copy_from_slice(&txid_internal);
    let display = tri7_core::p2p::internal_to_display(&disp);
    let report = json!({
        "mode": "live-self-spend",
        "broadcast": false,
        "source_block": height,
        "input": {
            "txid_display": txids[1],
            "n": n_out,
            "value_sats": value,
            "address": src_key.address,
            "height": height,
        },
        "to_address": dest_key.address,
        "amount_sats": amount,
        "fee_sats": plan.fee,
        "change_sats": plan.change,
        "bytes": plan.tx.serialize().len(),
        "txid": display,
        "tx_hex": hex_encode(&plan.tx.serialize()),
    });
    let tmp = format!("{}.tmp", out_path);
    std::fs::write(&tmp, serde_json::to_string_pretty(&report).unwrap())
        .unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
    std::fs::rename(&tmp, &out_path).unwrap_or_else(|e| die(&format!("rename: {}", e)));
    println!(
        "spend_live: h{} coinstake out {}:{} ({} sats) → {} ({} sats to self) fee {} — txid {} signed+verified → {}",
        height,
        txids[1],
        n_out,
        value,
        dest_key.address,
        amount,
        plan.fee,
        display,
        out_path
    );
    println!("SPEND_LIVE OK");
}
