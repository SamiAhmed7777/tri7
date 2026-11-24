//! READ-ONLY raw frame scan: where does a block sit in the blocks FILE, how
//! many times, and what sits around it? (fleet item 44, RPC-HEIGHTMAP-TRUTH)
//!
//! No index, no cache dir: the file is decoded frame by frame exactly the way
//! `build_index` reads it, and every occurrence of each block hash given on
//! the command line is listed with its neighbours. The tipstate window (if a
//! state dir is given) supplies the heights, so an out-of-order or duplicated
//! canonical frame is visible at a glance.
//!
//!   cargo run --release --example frame_scan -- \
//!       /var/lib/tri7/blk0001.dat /var/lib/tri7 <display-hex-hash> [hash ...]

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use tri7_core::blockindex::{FRAME_MAGIC, MAX_FRAME};
use tri7_core::legacy::hash9;
use tri7_core::p2p::{display_to_internal, internal_to_display};

type Hash = [u8; 32];

struct Frame {
    off: u64,
    hash: Hash,
    prev: Hash,
}

fn short(h: &Hash) -> String {
    internal_to_display(h).chars().take(8).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: frame_scan <blocks-file> <state-dir|-> <display-hex-hash> [hash ...]");
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let mut oracle: HashMap<Hash, u64> = HashMap::new();
    if args[2] != "-" {
        let raw = std::fs::read_to_string(PathBuf::from(&args[2]).join("tipstate.json"))
            .expect("tipstate.json");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("tipstate json");
        for e in v["entries"].as_array().expect("entries") {
            let h = display_to_internal(e["hash"].as_str().expect("hash")).expect("hash hex");
            oracle.insert(h, e["height"].as_u64().expect("height"));
        }
    }
    println!(
        "== FRAME SCAN == {}  oracle window: {} entries",
        blk.display(),
        oracle.len()
    );

    let start = std::time::Instant::now();
    let mut r = std::io::BufReader::with_capacity(1 << 20, File::open(&blk).expect("open blocks file"));
    let mut frames: Vec<Frame> = Vec::new();
    let mut occ: HashMap<Hash, Vec<u32>> = HashMap::new();
    let mut hdr = [0u8; 8];
    let mut body: Vec<u8> = Vec::new();
    let mut off: u64 = 0;
    loop {
        let mut got = 0usize;
        while got < 8 {
            match r.read(&mut hdr[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => panic!("read header at offset {off}: {e}"),
            }
        }
        if got == 0 {
            break; // clean EOF
        }
        if got < 8 {
            println!("torn header at offset {off} ({got} bytes) — ignored");
            break;
        }
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if magic != FRAME_MAGIC {
            println!("bad magic {magic:08x} at offset {off} — scan stops here");
            break;
        }
        if len == 0 || len > MAX_FRAME {
            println!("implausible frame length {len} at offset {off} — scan stops here");
            break;
        }
        body.clear();
        body.resize(len, 0);
        let mut got = 0usize;
        while got < len {
            match r.read(&mut body[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => panic!("read body at offset {off}: {e}"),
            }
        }
        if got < len {
            println!("torn body at offset {off} ({got}/{len} bytes) — ignored");
            break;
        }
        let hash = hash9(&body[..80]);
        let mut prev = [0u8; 32];
        prev.copy_from_slice(&body[4..36]);
        occ.entry(hash).or_default().push(frames.len() as u32);
        frames.push(Frame { off, hash, prev });
        off += (8 + len) as u64;
    }
    println!(
        "scanned {} frames, {} bytes, in {:.1}s",
        frames.len(),
        off,
        start.elapsed().as_secs_f64()
    );

    // Duplicated CANONICAL frames (same hash stored more than once AND the
    // hash is in the tipstate window): how common, and where.
    let mut dup_canon = 0usize;
    let mut dup_total = 0usize;
    for (h, seqs) in &occ {
        if seqs.len() > 1 {
            dup_total += 1;
            if oracle.contains_key(h) {
                dup_canon += 1;
                if dup_canon <= 40 {
                    let hh = oracle[h];
                    let offs: Vec<String> = seqs.iter().map(|s| format!("#{}@{}", s, frames[*s as usize].off)).collect();
                    println!("DUP canonical h{hh} {} ×{}: {}", short(h), seqs.len(), offs.join("  "));
                }
            }
        }
    }
    println!("duplicated hashes: {dup_total} (of which canonical-in-window: {dup_canon})");

    for arg in &args[3..] {
        let key = match display_to_internal(arg) {
            Ok(k) => k,
            Err(e) => {
                println!("{arg}: bad hash hex ({e})");
                continue;
            }
        };
        let seqs = occ.get(&key).cloned().unwrap_or_default();
        println!("\n--- {} ({}) occurrences: {}", short(&key), arg, seqs.len());
        for s in &seqs {
            let lo = (*s as usize).saturating_sub(6);
            let hi = (*s as usize + 6).min(frames.len() - 1);
            println!("  around occurrence #{s} @ offset {}:", frames[*s as usize].off);
            for i in lo..=hi {
                let f = &frames[i];
                let mark = if i as u32 == *s { "<<" } else { "  " };
                let height = match oracle.get(&f.hash) {
                    Some(h) => format!("h{h}"),
                    None => "  -----".to_string(),
                };
                // where is this frame's parent? (latest occurrence stored BEFORE it)
                let link = match occ.get(&f.prev).and_then(|v| v.iter().rev().find(|p| **p < i as u32)) {
                    Some(p) if *p + 1 == i as u32 => format!("prev=#{} (adjacent)", p),
                    Some(p) => format!("prev=#{} (gap of {} frames)", p, i as u32 - p - 1),
                    None => "prev NOT stored earlier".to_string(),
                };
                let dup = occ[&f.hash].len();
                let dupm = if dup > 1 { format!(" DUP×{dup}") } else { String::new() };
                println!("  {mark} #{i:<8} @ {:<12} {} {} {}{dupm}", f.off, short(&f.hash), height, link);
            }
        }
    }
}
