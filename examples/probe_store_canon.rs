//! probe_store_canon — what is REALLY in the blocks file?
//!
//! The daemon's height map and the file's contents are two different things.
//! This probe reads the raw frames and reports, from the bytes alone:
//!   - every frame's hash and its prev_hash (bytes 4..36 of the header),
//!   - the canonical chain walked BACKWARD from the tipstate window's base
//!     using nothing but those prev links,
//!   - how far down that walk reaches, and
//!   - how many frames in the file are NOT on that canonical chain (the
//!     debris a fork leaves behind).
//!
//! It answers one question: is the chain's history physically present in the
//! file, or is it really missing?
//!
//!   cargo run --release --example probe_store_canon -- <blk.dat> <tipstate.json>

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use tri7_core::legacy::hash9;
use tri7_core::p2p::{display_to_internal, internal_to_display};

const FRAME_MAGIC: u32 = 0x0522_3570;

fn hex_to_hash(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let blk = args.get(1).cloned().unwrap_or_default();
    let tipstate = args.get(2).cloned().unwrap_or_default();

    // ---- pass 1: scan every frame, record hash + prev_hash + offset
    let file = File::open(&blk).expect("open blocks file");
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut hdr = [0u8; 8];
    let mut body: Vec<u8> = Vec::new();
    let mut off: u64 = 0;
    let mut frames = 0u64;
    let mut by_hash: HashMap<[u8; 32], u64> = HashMap::new();
    let mut prev_of: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    loop {
        match r.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(_) => break,
        }
        let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if magic == 0 {
            off += 8;
            continue;
        }
        if magic != FRAME_MAGIC || len == 0 {
            eprintln!("bad frame at offset {off}: magic {magic:08x} len {len}");
            break;
        }
        body.clear();
        body.resize(len, 0);
        if r.read_exact(&mut body).is_err() {
            eprintln!("truncated frame at offset {off}");
            break;
        }
        if body.len() >= 80 {
            let h = hash9(&body[..80]);
            let mut prev = [0u8; 32];
            prev.copy_from_slice(&body[4..36]);
            by_hash.insert(h, off);
            prev_of.insert(h, prev);
        }
        frames += 1;
        off += (8 + len) as u64;
    }
    println!("frames in file: {frames}");
    println!("unique hashes: {}", by_hash.len());
    println!("file bytes: {off}");

    // ---- the anchor: the tipstate window's first entry (a height we trust)
    let text = std::fs::read_to_string(&tipstate).expect("tipstate");
    let st: serde_json::Value = serde_json::from_str(&text).expect("tipstate json");
    let entries = st["entries"].as_array().cloned().unwrap_or_default();
    let base = st["window_base"].as_u64().unwrap_or(0);
    let anchor_hex = entries
        .iter()
        .find(|e| e["height"].as_u64() == Some(base))
        .and_then(|e| e["hash"].as_str())
        .unwrap_or("")
        .to_string();
    // the tipstate stores DISPLAY hex; the file's prev links are INTERNAL order
    let anchor = display_to_internal(&anchor_hex).expect("anchor hash");
    println!("\nwindow base: h{base} = {anchor_hex}");

    // the canonical parent of the window base is the block BEFORE it
    let parent = prev_of.get(&anchor).copied();
    let parent_hex = parent.map(|p| internal_to_display(&p)).unwrap_or_default();
    println!("canonical parent of window base: {parent_hex}");

    // ---- walk the canonical chain backward through the FILE's own prev links
    let mut cur = parent;
    let mut depth = 0u64;
    let mut lowest: Option<[u8; 32]> = None;
    let mut on_chain: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let mut breaks = 0u64;
    while let Some(h) = cur {
        if !by_hash.contains_key(&h) {
            println!(
                "\nwalk stopped after {depth} blocks: parent {} NOT in the file",
                internal_to_display(&h)
            );
            breaks += 1;
            break;
        }
        on_chain.insert(h);
        lowest = Some(h);
        depth += 1;
        cur = prev_of.get(&h).copied();
        if depth > 5_000_000 {
            break;
        }
    }
    println!("canonical chain walked back: {depth} blocks from the window base");
    if let Some(l) = lowest {
        println!("lowest reachable hash on that chain: {}", internal_to_display(&l));
        // is the lowest one genesis? genesis has an all-zero prev
        let p = prev_of.get(&l).copied().unwrap_or([0u8; 32]);
        println!(
            "its prev is {} (all-zero => genesis)",
            if p == [0u8; 32] { "ZERO" } else { "non-zero" }
        );
    }
    println!("frames NOT on the canonical chain: {}", by_hash.len() - on_chain.len());

    // ---- is the DAEMON's current tip physically in the file?
    let tip_hex = st["blockhash"].as_str().unwrap_or("").to_string();
    let tip_h = st["height"].as_u64().unwrap_or(0);
    println!("\ndaemon tip: h{tip_h} = {tip_hex}");
    match display_to_internal(&tip_hex) {
        Ok(t) => {
            println!("tip body present in file: {}", by_hash.contains_key(&t));
            if let Some(p) = prev_of.get(&t) {
                println!("tip's parent in file: {}", by_hash.contains_key(p));
            }
        }
        Err(e) => println!("tip hash parse: {e}"),
    }

    // ---- how far UP does the canonical chain in the file reach?
    // Walk FORWARD from genesis using the file's own child links.
    let mut child_of: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    for (h, p) in prev_of.iter() {
        child_of.insert(*p, *h);
    }
    let genesis = lowest.unwrap_or([0u8; 32]);
    let mut cur = genesis;
    let mut up = 0u64;
    while let Some(c) = child_of.get(&cur) {
        on_chain.insert(*c);
        cur = *c;
        up += 1;
        if up > 5_000_000 { break; }
    }
    println!("canonical chain reachable FORWARD from genesis: {up} blocks");
    println!("highest canonical hash in file: {}", internal_to_display(&cur));
    println!("frames still off-chain: {}", by_hash.len() - on_chain.len());
    let _ = display_to_internal;
}
