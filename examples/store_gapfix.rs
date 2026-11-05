//! store_gapfix — fill heights a fork left missing in a block store by
//! fetching ONLY those canonical blocks from a peer and appending them.
//!
//! When a node mines its own branch and later adopts the heavier chain, the
//! blocks file keeps the abandoned frames for that height range and the
//! canonical bodies were never downloaded. Positional chaining cannot index a
//! body that sits after a foreign frame, so those heights stay "not covered by
//! the height map" even though tipstate knows their hashes.
//!
//! Repair: for each missing height in the authoritative window, GET the raw
//! block from a peer and check sha256d(header) against the hash stored in
//! tipstate — an identity check against local authority, not "trust the peer".
//! Only then append.
//!
//! Usage:
//!   store_gapfix <state_dir> <peer_host:port> [--dry-run]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use tri7_core::blockindex::{BlockStore, BuildOracle};

fn post_json(host: &str, port: u16, body: &str) -> Result<String, String> {
    let mut s = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
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

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16).ok_or("bad hex nibble")?;
        let lo = (b[i + 1] as char).to_digit(16).ok_or("bad hex nibble")?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(out)
}

fn rpc_result(resp: &str) -> Option<String> {
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or(resp);
    let i = body.find("\"result\"")?;
    let v = body[i + 8..].trim_start().trim_start_matches(':').trim_start();
    if let Some(rest) = v.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// tipstate.json: window_base + an ordered entries list, each entry carrying
/// its own height and hash ("hash" in the file, display byte order).
fn load_expected(state_dir: &std::path::Path) -> Option<Vec<(u64, String)>> {
    let path = state_dir.join("tipstate.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entries = v.get("entries")?.as_array()?;
    let mut out: Vec<(u64, String)> = Vec::new();
    for e in entries {
        let h = e.get("height")?.as_u64()?;
        let hash = e
            .get("hash")
            .or_else(|| e.get("blockhash"))
            .and_then(|x| x.as_str())?
            .to_string();
        out.push((h, hash));
    }
    out.sort_by_key(|(h, _)| *h);
    out.dedup_by_key(|(h, _)| *h);
    Some(out)
}

/// The tipstate window as a BuildOracle: display hex -> internal byte order.
fn oracle_from(expected: &[(u64, String)]) -> Option<BuildOracle> {
    if expected.is_empty() {
        return None;
    }
    let base = expected[0].0;
    let mut hashes = Vec::with_capacity(expected.len());
    for (h, hex) in expected {
        if *h != base + hashes.len() as u64 {
            return None; // non-contiguous window: not usable as an oracle
        }
        hashes.push(tri7_core::p2p::display_to_internal(hex).ok()?);
    }
    Some(BuildOracle {
        base,
        tip: base + hashes.len() as u64 - 1,
        hashes,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: store_gapfix <state_dir> <peer_host:port> [--dry-run]");
        std::process::exit(2);
    }
    let state_dir = PathBuf::from(&args[1]);
    let peer = args[2].clone();
    let dry = args.iter().any(|a| a == "--dry-run");

    let expected = match load_expected(&state_dir) {
        Some(x) => x,
        None => {
            eprintln!("no usable tipstate.json in {}", state_dir.display());
            std::process::exit(2);
        }
    };
    println!(
        "oracle window h{}..h{} ({} heights)",
        expected.first().map(|x| x.0).unwrap_or(0),
        expected.last().map(|x| x.0).unwrap_or(0),
        expected.len()
    );

    let store = BlockStore::new(
        Some(state_dir.join("blk0001.dat")),
        Some(state_dir.join("index")),
    );
    // Build WITH the oracle: hash-first acceptance is what lets a frame that
    // cannot chain (another branch sits before it) still get its height.
    let oracle = oracle_from(&expected);
    store.spawn_build(oracle);
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

    let mut missing: Vec<(u64, String)> = Vec::new();
    for (h, want) in expected.iter() {
        match store.hash_at_height(*h) {
            Ok(Some(got)) if &got == want => {}
            _ => missing.push((*h, want.clone())),
        }
    }
    println!(
        "window coverage: {} present, {} missing",
        expected.len() - missing.len(),
        missing.len()
    );
    if missing.is_empty() {
        println!("nothing to repair");
        return;
    }
    println!(
        "missing: h{}..h{} ({} blocks)",
        missing[0].0,
        missing[missing.len() - 1].0,
        missing.len()
    );

    let (host, port) = match peer.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(19112)),
        None => (peer.clone(), 19112),
    };

    let mut fixed = 0usize;
    let mut refused = 0usize;
    for (h, want) in &missing {
        // Ask by HASH: the canonical hash is known from tipstate, and a hash
        // lookup cannot be off-by-N the way a gap-adjacent height lookup can.
        let body = format!("{{\"method\":\"getblock\",\"params\":[\"{want}\",false]}}");
        let resp = post_json(&host, port, &body).unwrap_or_default();
        let hex = match rpc_result(&resp) {
            Some(x) => x,
            None => {
                eprintln!("h{h}: no body ({})", resp.chars().take(100).collect::<String>());
                refused += 1;
                continue;
            }
        };
        let bytes = match decode_hex(&hex) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("h{h}: bad hex: {e}");
                refused += 1;
                continue;
            }
        };
        if bytes.len() < 80 {
            eprintln!("h{h}: body too short");
            refused += 1;
            continue;
        }
        // header hash is hash9 (the chain's own function), NOT sha256d.
        let got = tri7_core::p2p::internal_to_display(&tri7_core::legacy::hash9(&bytes[..80]));
        if &got != want {
            eprintln!("h{h}: HASH MISMATCH peer={got} local-authority={want} — REFUSED");
            refused += 1;
            continue;
        }
        if dry {
            println!("h{h}: verified {got} (dry run)");
            continue;
        }
        match store.append_block(&bytes, Some(*h)) {
            Ok(_) => fixed += 1,
            Err(e) => {
                eprintln!("h{h}: append failed: {e}");
                refused += 1;
            }
        }
    }
    println!("repaired {fixed}/{} refused {refused}", missing.len());
    if !dry {
        println!("restart the node so the index rebuild picks the appended bodies up");
    }
}
