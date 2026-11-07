//! seed_sweep — sweep ALL wallet-owned coins into a BIP39 phrase's 10-address
//! set (m/44'/2222'/0'/0/0..9), built and signed OFFLINE from the wallet file.
//!
//! usage:
//!   seed_sweep <wallet.dat> <rpc-url> --mnemonic "<24 words>" [--exclude A1,A2] [--dry-run]
//!
//! Coins come live from the node (listunspent, scriptPubKey included) and are
//! matched to wallet keys by SECRET-DERIVED payloads — no stored-pubkey
//! dependency, so legacy-migrated rows sweep too. Immature coinstake/coinbase
//! inputs are refused (sweep_spendable = the consensus depth rule). The signed
//! consolidation tx is built by sweep::plan_sweep (CreateTransaction parity)
//! and relayed via sendrawtransaction on the same RPC.

use std::collections::HashMap;

const SWEEP_ADDR_COUNT: usize = 10;

fn post_json(url: &str, body: &str) -> Result<String, String> {
    // url: http://127.0.0.1:PORT
    let rest = url.strip_prefix("http://").ok_or("rpc url must be http://host:port")?;
    let (host, port) = rest
        .split_once(':')
        .ok_or("rpc url must be http://host:port")?;
    let mut s = std::net::TcpStream::connect((host, port.parse::<u16>().map_err(|e| e.to_string())?))
        .map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(60))).ok();
    s.set_write_timeout(Some(std::time::Duration::from_secs(30))).ok();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    use std::io::{Read, Write};
    s.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| format!("read: {e}"))?;
    Ok(out)
}

fn rpc_result(resp: &str) -> Option<serde_json::Value> {
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or(resp);
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    v.get("result").cloned()
}

fn rpc_err(resp: &str) -> Option<String> {
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or(resp);
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    v.get("error").and_then(|e| {
        if e.is_null() {
            None
        } else {
            Some(e.to_string())
        }
    })
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex".into());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut wallet = None;
    let mut rpc = None;
    let mut mnemonic = None;
    let mut excludes: Vec<String> = Vec::new();
    let mut dry = false;
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--mnemonic" => {
                i += 1;
                mnemonic = Some(a.get(i).cloned().unwrap_or_default());
            }
            "--exclude" => {
                i += 1;
                excludes = a.get(i).map(|s| s.split(',').map(|x| x.trim().to_string()).collect()).unwrap_or_default();
            }
            "--dry-run" => dry = true,
            other if wallet.is_none() => wallet = Some(other.to_string()),
            other if rpc.is_none() => rpc = Some(other.to_string()),
            other => {
                eprintln!("unexpected arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let (Some(wallet), Some(rpc), Some(mnemonic)) = (wallet, rpc, mnemonic) else {
        eprintln!("usage: seed_sweep <wallet.dat> <rpc-url> --mnemonic \"<24 words>\" [--exclude A1,A2] [--dry-run]");
        std::process::exit(2);
    };

    // destinations: the phrase's own 10-address set
    let m = tri7_core::seed::normalize_words(&mnemonic).unwrap_or(mnemonic);
    if !tri7_core::seed::check_mnemonic(&m) {
        eprintln!("invalid mnemonic");
        std::process::exit(1);
    }
    let dests = match tri7_core::sweep::derive_sweep_destinations(&m, "") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("derive destinations: {e}");
            std::process::exit(1);
        }
    };
    let mut dest_set: std::collections::HashSet<String> =
        dests.iter().map(|d| d.address.clone()).collect();
    for x in &excludes {
        dest_set.insert(x.clone());
    }

    // wallet keys: address -> (secret, pubkey), derived from secrets
    let rows = match tri7_core::keytool::dump_wallet(std::path::Path::new(&wallet)) {
        Ok((r, _)) => r,
        Err(e) => {
            eprintln!("read wallet: {e}");
            std::process::exit(1);
        }
    };
    let secp = secp256k1::Secp256k1::signing_only();
    let mut by_payload: HashMap<[u8; 21], (Vec<u8>, [u8; 32])> = HashMap::new();
    let mut by_pubkey: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
    for row in &rows {
        let (secret, compressed) = match tri7_core::keytool::wif_decode(&row.wif) {
            Ok(x) => x,
            Err(_) => continue,
        };
        let _ = compressed;
        let sk = match secp256k1::SecretKey::from_slice(&secret) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize().to_vec();
        let payload = tri7_core::address::pubkey_payload(&pk);
        by_payload.insert(payload, (pk.clone(), secret));
        by_pubkey.insert(pk, secret);
    }
    println!("wallet keys loaded: {}", by_payload.len());

    // live coins from the node
    let body = serde_json::json!({"method": "listunspent"});
    let resp = match post_json(&rpc, &body.to_string()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("listunspent: {e}");
            std::process::exit(1);
        }
    };
    let Some(arr) = rpc_result(&resp).and_then(|v| v.as_array().cloned()) else {
        eprintln!("listunspent failed: {}", &resp[..resp.len().min(200)]);
        std::process::exit(1);
    };

    let tip: u32 = {
        let body = serde_json::json!({"method": "getblockcount"});
        let resp = post_json(&rpc, &body.to_string()).unwrap_or_default();
        rpc_result(&resp).and_then(|v| v.as_u64()).unwrap_or(0) as u32
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);

    let mut inputs: Vec<tri7_core::spend::SpendInput> = Vec::new();
    let mut skipped_dest = 0usize;
    let mut skipped_excluded = 0usize;
    let mut skipped_immature = 0usize;
    let mut skipped_nokey = 0usize;
    let mut skipped_zero = 0usize;
    for u in &arr {
        let addr = u.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let value = u.get("amount_sats").and_then(|x| x.as_i64()).unwrap_or(0);
        if value <= 0 {
            skipped_zero += 1;
            continue;
        }
        if dest_set.contains(&addr) {
            skipped_dest += 1;
            continue;
        }
        if excludes.contains(&addr) {
            skipped_excluded += 1;
            continue;
        }
        let prev_kind = u.get("prev_kind").and_then(|x| x.as_str()).unwrap_or("regular").to_string();
        let height = u.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let tx_time = u.get("tx_time").and_then(|x| x.as_i64()).unwrap_or(0) as u32;
        if !tri7_core::sweep::sweep_spendable(&prev_kind, height, tip.saturating_add(1)) {
            skipped_immature += 1;
            continue;
        }
        let script_hex = u.get("scriptPubKey").and_then(|x| x.as_str()).unwrap_or("");
        let script_pubkey = match hex_decode(script_hex) {
            Ok(s) => s,
            Err(_) => {
                skipped_nokey += 1;
                continue;
            }
        };
        let Some(dest) = tri7_core::address::script_to_dest(&script_pubkey) else {
            skipped_nokey += 1;
            continue;
        };
        let payload = dest.payload();
        let addr_from_payload = tri7_core::address::payload_to_address(&payload);
        let Some((pubkey, secret)) = by_payload.get(&payload).map(|(p, s)| (p.clone(), *s)) else {
            skipped_nokey += 1;
            continue;
        };
        let txid_display = u.get("txid").and_then(|x| x.as_str()).unwrap_or("");
        let mut txid = [0u8; 32];
        if txid_display.len() == 64 {
            let ok = (0..32)
                .all(|i| u8::from_str_radix(&txid_display[i * 2..i * 2 + 2], 16).is_ok());
            if ok {
                for j in 0..32 {
                    txid[j] = u8::from_str_radix(&txid_display[j * 2..j * 2 + 2], 16).unwrap_or(0);
                }
                txid.reverse();
            }
        }
        inputs.push(tri7_core::spend::SpendInput {
            txid,
            n: u.get("vout").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            value,
            script_pubkey,
            height,
            tx_time,
            is_coinbase: prev_kind == "coinbase",
            is_coinstake: prev_kind == "coinstake",
            secret,
            pubkey,
        });
        let _ = addr_from_payload;
    }
    println!(
        "coins: {} sweepable | skipped: dest {skipped_dest}, excluded {skipped_excluded}, immature {skipped_immature}, no-key {skipped_nokey}, zero {skipped_zero}",
        inputs.len()
    );

        // drop coins already on the seed addresses (sweep convention: they stay)
    let sources = tri7_core::sweep::sweep_source_inputs(&inputs, &dests);
    let total: i64 = sources.iter().map(|s| s.value).sum();
    println!(
        "sweep sources: {} coins, {} sats ({}.{:06} TRI)",
        sources.len(),
        total,
        total / 1_000_000,
        total % 1_000_000
    );

let total: i64 = sources.iter().map(|s| s.value).sum();
    println!(
        "sweep sources: {} coins, {} sats ({}.{:06} TRI)",
        sources.len(),
        total,
        total / 1_000_000,
        total % 1_000_000
    );

    let plan = match tri7_core::sweep::plan_sweep(&sources, &dests, 0, now, tip.saturating_add(1)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("plan_sweep: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "plan: {} inputs -> {} outputs, fee {} sats, {} bytes",
        plan.selected.len(),
        plan.outputs.len(),
        plan.fee,
        plan.n_bytes
    );
    for (addr, v) in &plan.outputs {
        println!("  {} <- {}.{} TRI", addr, v / 1_000_000, v % 1_000_000);
    }
    let hex = hex_encode(&plan.tx.serialize());
    if dry {
        println!("\nDRY RUN — tx not broadcast. hex:\n{hex}");
        return;
    }
    let body = serde_json::json!({"method": "sendrawtransaction", "params": [hex]});
    match post_json(&rpc, &body.to_string()) {
        Ok(resp) => {
            if let Some(e) = rpc_err(&resp) {
                eprintln!("relay error: {e}");
                std::process::exit(1);
            }
            println!(
                "RELAYED: txid {}",
                tri7_core::legacy::hash_to_display(&plan.tx.hash())
            );
        }
        Err(e) => {
            eprintln!("relay: {e}");
            std::process::exit(1);
        }
    }
}
