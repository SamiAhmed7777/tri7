//! tri7 — the Triangles 7 command line.
//!
//! One binary for wallet, chain, staking, sweep and explorer operations.
//! Everything routes through the same tested library functions the node and
//! GUI use (tri7_core::{keytool, seed, sweep, spend, wallet}) — this binary is
//! a thin argv layer, not a second implementation.
//!
//! Online subcommands take --rpc http://127.0.0.1:PORT (a loopback node).
//!
//! Exit codes: 0 ok, 1 refused/failed, 2 usage.

use clap::{Parser, Subcommand};
use std::collections::HashMap;

fn post_json(url: &str, body: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("rpc url must be http://host:port")?;
    let (host, port) = rest
        .split_once(':')
        .ok_or("rpc url must be http://host:port")?;
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect((
        host,
        port.parse::<u16>().map_err(|e| e.to_string())?,
    ))
    .map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(60))).ok();
    s.set_write_timeout(Some(std::time::Duration::from_secs(30))).ok();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
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
    v.get("error")
        .and_then(|e| if e.is_null() { None } else { Some(e.to_string()) })
}

fn rpc_call(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({"method": method, "params": params});
    let resp = post_json(url, &body.to_string())?;
    if let Some(e) = rpc_err(&resp) {
        return Err(e);
    }
    rpc_result(&resp).ok_or_else(|| "null result".to_string())
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

fn fmt_tri(sats: i64) -> String {
    format!("{}.{:06}", sats / 1_000_000, sats % 1_000_000)
}

// ---------------------------------------------------------------------------
// wallet keys (from the wallet file, via the keytool lib)
// ---------------------------------------------------------------------------

struct WalletKeys {
    /// address -> (secret, pubkey)
    by_address: HashMap<String, ([u8; 32], Vec<u8>)>,
    /// payload -> (secret, pubkey)
    by_payload: HashMap<[u8; 21], ([u8; 32], Vec<u8>)>,
}

fn load_wallet_keys(path: &str) -> Result<WalletKeys, String> {
    let rows = tri7_core::keytool::dump_wallet(std::path::Path::new(path))?;
    let secp = secp256k1::Secp256k1::signing_only();
    let mut out = WalletKeys {
        by_address: HashMap::new(),
        by_payload: HashMap::new(),
    };
    for row in rows.0 {
        let (secret, _compressed) = match tri7_core::keytool::wif_decode(&row.wif) {
            Ok(x) => x,
            Err(_) => continue,
        };
        let sk = match secp256k1::SecretKey::from_slice(&secret) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = tri7_core::address::pubkey_payload(&pk);
        out.by_address.insert(row.address.clone(), (secret, pk.clone()));
        out.by_payload.insert(payload, (secret, pk));
    }
    Ok(out)
}

#[derive(Parser)]
#[command(name = "tri7", version, about = "Triangles 7 command line", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Wallet file operations
    Wallet {
        #[command(subcommand)]
        cmd: WalletCmd,
    },
    /// Chain queries against a node RPC
    Chain {
        #[arg(long)]
        rpc: String,
        #[command(subcommand)]
        cmd: ChainCmd,
    },
    /// Staking control against a node RPC
    Staking {
        #[arg(long)]
        rpc: String,
        #[command(subcommand)]
        cmd: StakingCmd,
    },
    /// Sweep wallet coins into a BIP39 seed's 10-address set
    Sweep {
        /// wallet file whose coins are swept
        #[arg(long)]
        wallet: String,
        /// node RPC (coins + relay)
        #[arg(long)]
        rpc: String,
        /// file holding the 24-word phrase (one line)
        #[arg(long)]
        mnemonic_file: String,
        /// comma-separated addresses to leave untouched (e.g. pending-pool sources)
        #[arg(long)]
        exclude: Option<String>,
        /// build and print without broadcasting
        #[arg(long)]
        dry_run: bool,
    },
    /// Send an amount to one address (coins from the wallet file, matched via the node)
    Send {
        #[arg(long)]
        wallet: String,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        to: String,
        /// amount in TRI (6 decimals)
        #[arg(long)]
        amount: f64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Rich list from a balances.json snapshot (phase-1e replay artifact)
    Richlist {
        #[arg(long)]
        file: String,
        #[arg(long, default_value = "25")]
        top: usize,
    },
}

#[derive(Subcommand)]
enum WalletCmd {
    /// Create a wallet file from a NEW 24-word phrase (phrase printed once)
    CreateSeed {
        #[arg(long)]
        out: String,
        #[arg(long, default_value = "10")]
        count: usize,
    },
    /// Add a phrase's derived keys to an EXISTING wallet file
    ImportSeed {
        #[arg(long)]
        wallet: String,
        #[arg(long)]
        mnemonic_file: String,
        #[arg(long, default_value = "10")]
        count: usize,
    },
    /// Add N random keys (first address printed)
    NewAddress {
        #[arg(long)]
        wallet: String,
        #[arg(long, default_value = "1")]
        count: usize,
    },
    /// Key count + address count of a wallet file
    Info {
        #[arg(long)]
        wallet: String,
    },
    /// Dump all keys (WIF) to a legacy-format file
    DumpWallet {
        #[arg(long)]
        wallet: String,
        #[arg(long)]
        out: String,
    },
    /// Import a legacy dump file
    ImportWallet {
        #[arg(long)]
        wallet: String,
        #[arg(long)]
        dump: String,
    },
    /// Check wallet integrity
    CheckWallet {
        #[arg(long)]
        wallet: String,
    },
}

#[derive(Subcommand)]
enum ChainCmd {
    /// Current tip height
    Tip,
    /// Best block hash
    BestBlockHash,
    /// Block hash at a height
    BlockHash {
        #[arg(long)]
        height: u64,
    },
    /// Raw transaction by txid (verbose JSON)
    RawTransaction {
        #[arg(long)]
        txid: String,
    },
    /// Number of peer connections
    Connections,
}

#[derive(Subcommand)]
enum StakingCmd {
    /// Staking status (enabled/attempts/coins)
    Info,
    /// Turn staking on (persists in the node's conf)
    On,
    /// Turn staking off (persists in the node's conf)
    Off,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => std::process::ExitCode::from(0),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Commands::Wallet { cmd } => wallet_cmd(cmd),
        Commands::Chain { rpc, cmd } => chain_cmd(&rpc, cmd),
        Commands::Staking { rpc, cmd } => staking_cmd(&rpc, cmd),
        Commands::Sweep {
            wallet,
            rpc,
            mnemonic_file,
            exclude,
            dry_run,
        } => sweep_cmd(&wallet, &rpc, &mnemonic_file, exclude, dry_run),
        Commands::Send {
            wallet,
            rpc,
            to,
            amount,
            dry_run,
        } => send_cmd(&wallet, &rpc, &to, amount, dry_run),
        Commands::Richlist { file, top } => richlist_cmd(&file, top),
    }
}

fn wallet_cmd(cmd: WalletCmd) -> Result<(), String> {
    match cmd {
        WalletCmd::CreateSeed { out, count } => {
            if std::path::Path::new(&out).exists() {
                return Err(format!("refusing to overwrite {out}"));
            }
            let m = tri7_core::seed::generate_mnemonic_24()?;
            tri7_core::wallet::create_wallet_file(std::path::Path::new(&out))?;
            let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(std::path::Path::new(&out))
                .map_err(|e| e.to_string())?;
            for i in 0..count as u32 {
                let s = tri7_core::seed::derive_triangles(&m, "", 0, 0, i)?;
                let k = wh.add_key(&s, &format!("seed/{i}")).map_err(|e| e.to_string())?;
                println!("addr {i}: {}", k.address);
            }
            wh.close();
            println!("\nMNEMONIC (24 words — store securely, printed once):\n{m}");
            Ok(())
        }
        WalletCmd::ImportSeed {
            wallet,
            mnemonic_file,
            count,
        } => {
            let m = std::fs::read_to_string(&mnemonic_file)
                .map_err(|e| format!("read {}: {e}", mnemonic_file))?;
            let m = tri7_core::seed::normalize_words(&m).unwrap_or(m);
            if !tri7_core::seed::check_mnemonic(&m) {
                return Err("invalid mnemonic".into());
            }
            let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(std::path::Path::new(&wallet))
                .map_err(|e| e.to_string())?;
            let (mut added, mut held) = (0usize, 0usize);
            for i in 0..count as u32 {
                let s = tri7_core::seed::derive_triangles(&m, "", 0, 0, i)?;
                match wh.add_key(&s, &format!("seed/{i}")) {
                    Ok(k) => {
                        println!("addr {i}: {} (imported)", k.address);
                        added += 1;
                    }
                    Err(_) => {
                        println!("addr {i}: (already held)");
                        held += 1;
                    }
                }
            }
            wh.close();
            println!("imported {added}, already held {held}");
            Ok(())
        }
        WalletCmd::NewAddress { wallet, count } => {
            let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(std::path::Path::new(&wallet))
                .map_err(|e| e.to_string())?;
            for i in 0..count {
                let mut s = [0u8; 32];
                getrandom::getrandom(&mut s).map_err(|e| e.to_string())?;
                let k = wh.add_key(&s, &format!("cli/{i}")).map_err(|e| e.to_string())?;
                println!("{}", k.address);
            }
            wh.close();
            Ok(())
        }
        WalletCmd::Info { wallet } => {
            let rows = tri7_core::keytool::dump_wallet(std::path::Path::new(&wallet))
                .map_err(|e| e.to_string())?;
            let mut addrs = std::collections::HashSet::new();
            for r in rows.0 {
                addrs.insert(r.address);
            }
            println!("keys: {} | distinct addresses: {}", rows.1, addrs.len());
            Ok(())
        }
        WalletCmd::DumpWallet { wallet, out } => {
            let (rows, n) = tri7_core::keytool::dump_wallet(std::path::Path::new(&wallet))
                .map_err(|e| e.to_string())?;
            let mut f = std::fs::File::create(&out).map_err(|e| format!("create {}: {e}", out))?;
            tri7_core::keytool::write_dump(&mut f, &rows, None)
                .map_err(|e| e.to_string())?;
            println!("dumped {n} key(s) to {out}");
            Ok(())
        }
        WalletCmd::ImportWallet { wallet, dump } => {
            let rep = tri7_core::keytool::import_wallet(std::path::Path::new(&wallet), std::path::Path::new(&dump))
                .map_err(|e| e.to_string())?;
            println!(
                "imported {} skipped {} failed {}",
                rep.imported, rep.skipped, rep.failed
            );
            Ok(())
        }
        WalletCmd::CheckWallet { wallet } => {
            let rep = tri7_core::keytool::check_wallet(std::path::Path::new(&wallet))
                .map_err(|e| e.to_string())?;
            println!("integrity: {} | bad keys: {}", rep.integrity, rep.bad_keys);
            Ok(())
        }
    }
}

fn chain_cmd(rpc: &str, cmd: ChainCmd) -> Result<(), String> {
    let v = match cmd {
        ChainCmd::Tip => rpc_call(rpc, "getblockcount", serde_json::json!([]))?,
        ChainCmd::BestBlockHash => rpc_call(rpc, "getbestblockhash", serde_json::json!([]))?,
        ChainCmd::BlockHash { height } => {
            rpc_call(rpc, "getblockhash", serde_json::json!([height]))?
        }
        ChainCmd::RawTransaction { txid } => {
            rpc_call(rpc, "getrawtransaction", serde_json::json!([txid, true]))?
        }
        ChainCmd::Connections => rpc_call(rpc, "getconnectioncount", serde_json::json!([]))?,
    };
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
    Ok(())
}

fn staking_cmd(rpc: &str, cmd: StakingCmd) -> Result<(), String> {
    let v = match cmd {
        StakingCmd::Info => rpc_call(rpc, "getstakinginfo", serde_json::json!([]))?,
        StakingCmd::On => rpc_call(rpc, "staking", serde_json::json!(["on"]))?,
        StakingCmd::Off => rpc_call(rpc, "staking", serde_json::json!(["off"]))?,
    };
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
    Ok(())
}

fn sweep_cmd(
    wallet: &str,
    rpc: &str,
    mnemonic_file: &str,
    exclude: Option<String>,
    dry: bool,
) -> Result<(), String> {
    let m = std::fs::read_to_string(mnemonic_file)
        .map_err(|e| format!("read {}: {e}", mnemonic_file))?;
    let m = tri7_core::seed::normalize_words(&m).unwrap_or(m);
    if !tri7_core::seed::check_mnemonic(&m) {
        return Err("invalid mnemonic".into());
    }
    let dests = tri7_core::sweep::derive_sweep_destinations(&m, "")?;
    let dest_set: std::collections::HashSet<String> =
        dests.iter().map(|d| d.address.clone()).collect();
    let excludes: Vec<String> = exclude
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();

    let keys = load_wallet_keys(wallet)?;
    let tip = rpc_call(rpc, "getblockcount", serde_json::json!([]))?
        .as_u64()
        .unwrap_or(0) as u32;
    let unspent = rpc_call(rpc, "listunspent", serde_json::json!([]))?
        .as_array()
        .cloned()
        .ok_or("listunspent did not return an array")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let mut inputs: Vec<tri7_core::spend::SpendInput> = Vec::new();
    let (mut sk_dest, mut sk_excl, mut sk_imm, mut sk_nokey) = (0, 0, 0, 0);
    for u in &unspent {
        let addr = u.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let value = u.get("amount_sats").and_then(|x| x.as_i64()).unwrap_or(0);
        if value <= 0 || dest_set.contains(&addr) {
            continue;
        }
        if excludes.contains(&addr) {
            sk_excl += 1;
            continue;
        }
        let prev_kind = u.get("prev_kind").and_then(|x| x.as_str()).unwrap_or("regular").to_string();
        let height = u.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let tx_time = u.get("tx_time").and_then(|x| x.as_i64()).unwrap_or(0) as u32;
        if !tri7_core::sweep::sweep_spendable(&prev_kind, height, tip.saturating_add(1)) {
            sk_imm += 1;
            continue;
        }
        let script_pubkey = hex_decode(
            u.get("scriptPubKey").and_then(|x| x.as_str()).unwrap_or(""),
        )?;
        let Some(dest) = tri7_core::address::script_to_dest(&script_pubkey) else {
            sk_nokey += 1;
            continue;
        };
        let Some((secret, pubkey)) = keys.by_payload.get(&dest.payload()) else {
            sk_nokey += 1;
            continue;
        };
        let txid_display = u.get("txid").and_then(|x| x.as_str()).unwrap_or("");
        let mut txid = [0u8; 32];
        for j in 0..32 {
            txid[j] = u8::from_str_radix(
                txid_display.get(j * 2..j * 2 + 2).unwrap_or("zz"),
                16,
            )
            .map_err(|_| "bad txid in listunspent")?;
        }
        txid.reverse();
        inputs.push(tri7_core::spend::SpendInput {
            txid,
            n: u.get("vout").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            value,
            script_pubkey,
            height,
            tx_time,
            is_coinbase: prev_kind == "coinbase",
            is_coinstake: prev_kind == "coinstake",
            secret: *secret,
            pubkey: pubkey.clone(),
        });
    }
    let sources = tri7_core::sweep::sweep_source_inputs(&inputs, &dests);
    let total: i64 = sources.iter().map(|s| s.value).sum();
    println!(
        "sweep: {} coins, {} TRI (skipped: dest/excluded/immature/no-key = {}/{}/{}/{})",
        sources.len(),
        fmt_tri(total),
        inputs.len() - sources.len(),
        sk_excl,
        sk_imm,
        sk_nokey
    );
    let now_secs = now;
    let plan = tri7_core::sweep::plan_sweep(&sources, &dests, 0, now_secs, tip.saturating_add(1))?;
    println!(
        "plan: fee {} sats, {} bytes, {} inputs",
        plan.fee, plan.n_bytes, plan.selected.len()
    );
    for (addr, v) in &plan.outputs {
        println!("  {} <- {} TRI", addr, fmt_tri(*v));
    }
    if dry {
        println!("DRY RUN — not broadcast");
        return Ok(());
    }
    let hex = hex_encode(&plan.tx.serialize());
    let body = serde_json::json!({"method": "sendrawtransaction", "params": [hex]});
    let resp = post_json(rpc, &body.to_string())?;
    if let Some(e) = rpc_err(&resp) {
        return Err(format!("relay: {e}"));
    }
    println!(
        "RELAYED: txid {}",
        tri7_core::legacy::hash_to_display(&plan.tx.hash())
    );
    Ok(())
}

fn send_cmd(
    wallet: &str,
    rpc: &str,
    to: &str,
    amount: f64,
    dry: bool,
) -> Result<(), String> {
    if !(amount > 0.0) {
        return Err("amount must be positive".into());
    }
    let payload = tri7_core::address::address_to_payload(to)
        .map_err(|e| format!("invalid address: {e}"))?;
    let keys = load_wallet_keys(wallet)?;
    let tip = rpc_call(rpc, "getblockcount", serde_json::json!([]))?
        .as_u64()
        .unwrap_or(0) as u32;
    let unspent = rpc_call(rpc, "listunspent", serde_json::json!([]))?
        .as_array()
        .cloned()
        .ok_or("listunspent did not return an array")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let amount_sats = (amount * 1_000_000.0).round() as i64;
    let mut inputs: Vec<tri7_core::spend::SpendInput> = Vec::new();
    for u in &unspent {
        let addr = u.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let Some((secret, pubkey)) = keys.by_address.get(&addr) else {
            continue;
        };
        let script_pubkey = hex_decode(
            u.get("scriptPubKey").and_then(|x| x.as_str()).unwrap_or(""),
        )?;
        inputs.push(tri7_core::spend::SpendInput {
            txid: {
                let t = u.get("txid").and_then(|x| x.as_str()).unwrap_or("");
                let mut t = hex_decode(t).map_err(|_| "bad txid")?;
                t.reverse();
                let mut a = [0u8; 32];
                a.copy_from_slice(&t);
                a
            },
            n: u.get("vout").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            value: u.get("amount_sats").and_then(|x| x.as_i64()).unwrap_or(0),
            script_pubkey,
            height: u.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            tx_time: u.get("tx_time").and_then(|x| x.as_i64()).unwrap_or(0) as u32,
            is_coinbase: u.get("prev_kind").and_then(|x| x.as_str()) == Some("coinbase"),
            is_coinstake: u.get("prev_kind").and_then(|x| x.as_str()) == Some("coinstake"),
            secret: *secret,
            pubkey: pubkey.clone(),
        });
    }
    // change returns to the first input's own address (a wallet key)
    let change_script = inputs
        .first()
        .map(|i| i.script_pubkey.clone())
        .ok_or("no wallet-owned coins available")?;
    let out = tri7_core::legacy::Vout {
        value: amount_sats,
        script_pubkey: {
            let mut sc = vec![0x76u8, 0xa9, 0x14];
            sc.extend_from_slice(&payload[1..]);
            sc.push(0x88);
            sc.push(0xac);
            sc
        },
    };
    let plan = tri7_core::spend::build_spend_outputs(
        &inputs,
        vec![out],
        Some(change_script),
        tri7_core::spend::MIN_FEE_SATS,
        now,
        tip.saturating_add(1),
    )?;
    println!(
        "plan: fee {} sats, change {} sats, {} bytes",
        plan.fee, plan.change, plan.tx.serialize().len()
    );
    if dry {
        println!("DRY RUN — not broadcast");
        return Ok(());
    }
    let hex = hex_encode(&plan.tx.serialize());
    let body = serde_json::json!({"method": "sendrawtransaction", "params": [hex]});
    let resp = post_json(rpc, &body.to_string())?;
    if let Some(e) = rpc_err(&resp) {
        return Err(format!("relay: {e}"));
    }
    println!("RELAYED: txid {}", tri7_core::legacy::hash_to_display(&plan.tx.hash()));
    Ok(())
}

fn richlist_cmd(file: &str, top: usize) -> Result<(), String> {
    let raw = std::fs::read_to_string(file).map_err(|e| format!("read {file}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("parse: {e}"))?;
    let height = v.get("height").ok_or("balances.json missing height")?;
    let map = v
        .get("balances")
        .and_then(|b| b.as_object())
        .ok_or("balances.json missing balances map")?;
    let mut rows: Vec<(String, i64)> = map
        .iter()
        .filter_map(|(a, s)| Some((a.clone(), s.as_i64()?)))
        .filter(|(_, s)| *s > 0)
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    println!("height {}", height);
    for (i, (a, s)) in rows.iter().take(top).enumerate() {
        println!("{:>4}. {} {} TRI", i + 1, a, fmt_tri(*s));
    }
    Ok(())
}

