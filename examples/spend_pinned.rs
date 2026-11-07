//! spend_pinned — build + sign a self-spend from a PINNED wallet-owned
//! prevout of a wallet FILE copy, ready for sendrawtransaction elsewhere.
//!
//! Unlike spend_live (which picks a freshly-minted coinstake from the running
//! node's own wallet), this pins the exact prevout on the command line so a
//! specific historical coin is provably the one being spent (2026-11-07:
//! proving the old mining-era funds on TBxrQx34 move end-to-end).
//!
//! Usage:
//!   spend_pinned --wallet PATH --rpc-port PORT
//!                --prev-txid <display-order txid> --prev-vout N
//!                --prev-height H --prev-time T
//!                --dest ADDR --amount SATS --fee SATS --out FILE
//! Exit 0 on a signed+verified tx (JSON with tx_hex written to --out).
//! The wallet file is opened READ-ONLY; nothing is broadcast here.

use serde_json::{json, Value};
use tri7_core::legacy::{Reader, Tx};
use tri7_core::spend;

fn die(msg: &str) -> ! {
    eprintln!("spend_pinned: {}", msg);
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
        s.read_to_end(&mut buf).map_err(|e| format!("rpc read: {}", e))?;
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

/// P2PKH scriptPubKey from an address payload (21-byte version + hash160):
/// OP_DUP OP_HASH160 PUSH20 <hash160 = payload[1..]> OP_EQUALVERIFY OP_CHECKSIG.
fn p2pkh_script_for_payload(payload: &[u8; 21]) -> Vec<u8> {
    let mut s = Vec::with_capacity(25);
    s.push(0x76); // OP_DUP
    s.push(0xa9); // OP_HASH160
    s.push(0x14); // PUSH20
    s.extend_from_slice(&payload[1..]);
    s.push(0x88); // OP_EQUALVERIFY
    s.push(0xac); // OP_CHECKSIG
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let need = |name: &str| -> String {
        arg(&args, name).unwrap_or_else(|| die(&format!("missing {}", name)))
    };
    let wallet_path = need("--wallet");
    let rpc_port: u16 = arg(&args, "--rpc-port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(19112);
    let prev_txid_display = need("--prev-txid");
    let prev_vout: u32 = need("--prev-vout").parse().unwrap_or_else(|_| die("bad vout"));
    let prev_height: u32 = need("--prev-height").parse().unwrap_or_else(|_| die("bad height"));
    let prev_time: i64 = need("--prev-time").parse().unwrap_or_else(|_| die("bad time"));
    let prev_time_u32: u32 = u32::try_from(prev_time).unwrap_or_else(|_| die("bad time range"));
    let dest_addr = need("--dest");
    let amount: i64 = need("--amount").parse().unwrap_or_else(|_| die("bad amount"));
    let fee: i64 = arg(&args, "--fee")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let out_path = arg(&args, "--out").unwrap_or_else(|| "/root/tri7-lab/spend_pinned.json".into());
    let rpc = Rpc { port: rpc_port };

    // 1. fetch + parse the parent tx of the pinned prevout
    let raw = rpc
        .call("getrawtransaction", json!([prev_txid_display]))
        .unwrap_or_else(|e| die(&e))
        .as_str()
        .unwrap_or_else(|| die("getrawtransaction: no hex"))
        .to_string();
    let raw_bytes = hex_decode(&raw);
    let parent = Tx::read(&mut Reader::new(&raw_bytes))
        .unwrap_or_else(|e| die(&format!("parent parse: {}", e)));

    // 2. the pinned output must exist, hold value, and be owned by a wallet key
    if parent.vout.len() <= prev_vout as usize {
        die(&format!("parent has no vout {}", prev_vout));
    }
    let vout = &parent.vout[prev_vout as usize];
    let wall = tri7_core::wallet::read_wallet(std::path::Path::new(&wallet_path))
        .unwrap_or_else(|e| die(&format!("wallet read: {}", e)));
    let owned = wall
        .keys
        .iter()
        .enumerate()
        .find(|(_, k)| {
            vout.script_pubkey == spend::script_pubkey_p2pk(&k.pubkey).unwrap_or_default()
                || vout.script_pubkey == spend::script_pubkey_p2pkh(&k.pubkey)
        })
        .unwrap_or_else(|| die("pinned output is NOT owned by a key in this wallet"));
    let (key_idx, src_key) = owned;
    if vout.value <= 0 {
        die("pinned output holds no value");
    }

    // 3. destination script from the address; change returns to the source key
    let dest_payload = tri7_core::address::address_to_payload(&dest_addr)
        .unwrap_or_else(|e| die(&format!("dest address: {}", e)));
    let dest_script = p2pkh_script_for_payload(&dest_payload);
    let change_script = spend::script_pubkey_p2pkh(&src_key.pubkey);
    if vout.value <= amount + fee {
        die(&format!(
            "input {} sats too small for amount {} + fee {}",
            vout.value, amount, fee
        ));
    }

    // 4. build + sign + self-verify (mirrors spend_live's proven path)
    let input = spend::SpendInput {
        txid: {
            let th = parent.hash();
            let mut h = [0u8; 32];
            h.copy_from_slice(&th);
            h
        },
        n: prev_vout,
        value: vout.value,
        script_pubkey: vout.script_pubkey.clone(),
        height: prev_height,
        tx_time: prev_time_u32,
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
        prev_height.max(1),
    )
    .unwrap_or_else(|e| die(&format!("build_spend: {}", e)));
    spend::verify_input(&plan.tx, 0, &vout.script_pubkey)
        .unwrap_or_else(|e| die(&format!("verify_input: {}", e)));
    spend::connect_inputs_check(&plan.tx, &[input.clone()], prev_height.max(1))
        .unwrap_or_else(|e| die(&format!("connect_inputs_check: {}", e)));

    // 5. emit
    let txid_internal = plan.tx.hash();
    let display = tri7_core::p2p::internal_to_display(&txid_internal);
    let report = json!({
        "mode": "pinned-self-spend",
        "broadcast": false,
        "input": {
            "txid_display": prev_txid_display,
            "n": prev_vout,
            "value_sats": vout.value,
            "address": src_key.address,
            "height": prev_height,
        },
        "to_address": dest_addr,
        "amount_sats": amount,
        "fee_sats": plan.fee,
        "change_sats": plan.change,
        "bytes": plan.tx.serialize().len(),
        "txid": display,
        "tx_hex": hex_encode(&plan.tx.serialize()),
        "wallet_keys_total": wall.keys.len(),
        "signing_key_index": key_idx,
    });
    let tmp = format!("{}.tmp", out_path);
    std::fs::write(&tmp, serde_json::to_string_pretty(&report).unwrap())
        .unwrap_or_else(|e| die(&format!("write {}: {}", tmp, e)));
    std::fs::rename(&tmp, &out_path).unwrap_or_else(|e| die(&format!("rename: {}", e)));
    println!(
        "spend_pinned: {}:{} ({} sats, {}) -> {} ({} sats) fee {} — txid {} signed+verified -> {}",
        prev_txid_display,
        prev_vout,
        vout.value,
        src_key.address,
        dest_addr,
        amount,
        plan.fee,
        display,
        out_path
    );
    println!("SPEND_PINNED OK");
}
