//! repair_band — refill a block store's hole from a peer that still holds the
//! blocks, using the CHAIN'S OWN LINKAGE as the verification.
//!
//! The hole this repairs: the fork surgery rolled a store back to a
//! pre-fork copy, so a range of canonical blocks (the "band") exists in a
//! backup file but not in the live store. The tipstate window does not
//! cover that range (it starts above it), so the repair cannot use the
//! tipstate as its oracle - it uses the chain itself:
//!
//!   1. walk BACKWARD from the canonical anchor block (a hash we already
//!      trust) through the peer's blocks, following each block's
//!      previousblockhash;
//!   2. stop when the walk reaches the hash of the block the live store
//!      ends with - that is proof the walk found the exact canonical chain
//!      between the two, with no fork and no gap;
//!   3. replay the collected blocks FORWARD into the live store, checking
//!      every prev_hash link as it goes.
//!
//! Usage:
//!   repair_band <state_dir> <peer_host:port> <anchor_hash> <base_height>
//!
//! The peer is a daemon whose RPC answers getblock <hash> (the backup file
//! mounted as its block store).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use tri7_core::blockindex::BlockStore;

fn post_json(host: &str, port: u16, body: &str) -> Result<String, String> {
    let mut s = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .ok();
    s.set_write_timeout(Some(std::time::Duration::from_secs(60)))
        .ok();
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
    let v: serde_json::Value = serde_json::from_str(resp).ok()?;
    v.get("result").cloned()
}

fn get_block(host: &str, port: u16, hash: &str) -> Result<serde_json::Value, String> {
    let body = format!("{{\"method\":\"getblock\",\"params\":[\"{hash}\"]}}");
    let resp = post_json(host, port, &body)?;
    let r = rpc_result(&resp).ok_or_else(|| format!("no result for {hash}: {resp}"))?;
    if r.is_null() {
        return Err(format!("block {hash} not found on the peer"));
    }
    Ok(r)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: repair_band <state_dir> <peer_host:port> <anchor_hash> <base_height>"
        );
        std::process::exit(2);
    }
    let state_dir = PathBuf::from(&args[1]);
    let peer = &args[2];
    let (host, port) = peer.split_once(':').expect("peer host:port");
    let port: u16 = port.parse().expect("peer port");
    let anchor = args[3].clone();
    let base_height: u64 = args[4].parse().expect("base height");

    let store = BlockStore::new(
        Some(state_dir.join("blk0001.dat")),
        Some(state_dir.join("index")),
    );
    store.spawn_build(None);
    loop {
        match store.status() {
            tri7_core::blockindex::StoreStatus::Ready { frames, .. } => {
                println!("index ready: {frames} frames");
                break;
            }
            tri7_core::blockindex::StoreStatus::Failed(e) => {
                eprintln!("index build failed: {e}");
                std::process::exit(2);
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }

    // The live store's last block - the walk must land exactly on it.
    let base_hash = match store.hash_at_height(base_height) {
        Ok(Some(h)) => h,
        _ => {
            eprintln!("the live store has no hash at h{base_height}");
            std::process::exit(3);
        }
    };
    println!("live store h{base_height} = {base_hash}");

    // 1. walk backward from the anchor through the peer
    let mut chain: Vec<String> = Vec::new();
    let mut cur = anchor.clone();
    loop {
        let b = get_block(host, port, &cur).unwrap_or_else(|e| {
            eprintln!("walk stopped at {cur}: {e}");
            std::process::exit(4);
        });
        let prev = b["previousblockhash"].as_str().unwrap_or("").to_string();
        let h = b["hash"].as_str().unwrap_or("").to_string();
        chain.push(h.clone());
        if prev == base_hash {
            println!("walk reached the live store's tip after {} blocks", chain.len());
            break;
        }
        if chain.len() > 20_000 {
            eprintln!("walk did not reach the live store's tip (20000 blocks)");
            std::process::exit(5);
        }
        cur = prev;
    }
    chain.reverse(); // forward order

    // 2. replay forward into the live store, checking every link
    let mut height = base_height + 1;
    let mut appended = 0u64;
    for hash in &chain {
        let b = get_block(host, port, hash).unwrap_or_else(|e| {
            eprintln!("fetch {hash}: {e}");
            std::process::exit(6);
        });
        let prev = b["previousblockhash"].as_str().unwrap_or("");
        let h = b["hash"].as_str().unwrap_or("");
        let hex = b["hex"].as_str().unwrap_or("");
        if hex.is_empty() {
            eprintln!("peer did not return the raw block for {hash}");
            std::process::exit(7);
        }
        let raw = decode_hex(hex).expect("hex");
        // the link check: this block's prev_hash must equal the previous one
        let expect_prev = if appended == 0 {
            base_hash.clone()
        } else {
            chain[(appended - 1) as usize].clone()
        };
        if prev != expect_prev {
            eprintln!("link break at h{height}: {prev} != {expect_prev}");
            std::process::exit(8);
        }
        let _ = h;
        match store.append_block(&raw, Some(height)) {
            Ok(_) => {
                appended += 1;
                height += 1;
            }
            Err(e) => {
                eprintln!("append at h{height}: {e}");
                std::process::exit(9);
            }
        }
    }
    println!("appended {appended} canonical blocks (h{}..h{})", base_height + 1, height - 1);
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16).ok_or("bad hex")?;
        let lo = (b[i + 1] as char).to_digit(16).ok_or("bad hex")?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(out)
}
