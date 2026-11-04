//! store_slice_hash — copy a block store starting at the frame whose block
//! hash equals a given value (typically the tipstate window BASE hash).
//!
//! Why: a follower at the chain tip needs the recent window bodies for the
//! wallet live view. The index anchors mid-chain via the window oracle, so a
//! slice that starts EXACTLY at the window base hash anchors cleanly and the
//! seed then finds every body from base..tip.
//!
//! Usage: store_slice_hash <src_blk> <dst_blk> <hash_display_hex>

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const MAGIC: u32 = 0x0522_3570;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: store_slice_hash <src_blk> <dst_blk> <hash_display_hex>");
        std::process::exit(2);
    }
    let src = PathBuf::from(&args[1]);
    let dst = PathBuf::from(&args[2]);
    let target = args[3].to_ascii_lowercase();
    let mut f = match std::fs::File::open(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {}: {}", src.display(), e);
            std::process::exit(2);
        }
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut pos: u64 = 0;
    let mut hdr = [0u8; 8];
    let mut frames: u64 = 0;
    loop {
        if pos + 8 > len {
            eprintln!("hash not found in {} frames", frames);
            std::process::exit(1);
        }
        f.seek(SeekFrom::Start(pos)).ok();
        if f.read_exact(&mut hdr).is_err() {
            eprintln!("header read failed at {}", pos);
            std::process::exit(1);
        }
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let blen = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if magic != MAGIC {
            eprintln!("bad magic at {} (frame {})", pos, frames);
            std::process::exit(1);
        }
        let mut body = vec![0u8; blen];
        if f.read_exact(&mut body).is_err() {
            eprintln!("body read failed at frame {}", frames);
            std::process::exit(1);
        }
        // block hash = Hash9 of the 80-byte header (first 80 bytes of body)
        if body.len() >= 80 {
            let mut h = tri7_core::legacy::hash9(&body[..80]);
            h.reverse();
            let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
            if hex == target {
                let n = len - pos;
                eprintln!(
                    "found at frame {} offset {} — copying {:.2} MB",
                    frames,
                    pos,
                    n as f64 / 1_048_576.0
                );
                f.seek(SeekFrom::Start(pos)).ok();
                let mut out = match std::fs::File::create(&dst) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("create {}: {}", dst.display(), e);
                        std::process::exit(1);
                    }
                };
                let mut r = std::io::BufReader::new(f.take(n));
                match std::io::copy(&mut r, &mut out) {
                    Ok(w) => println!("wrote {} bytes to {}", w, dst.display()),
                    Err(e) => {
                        eprintln!("copy: {}", e);
                        std::process::exit(1);
                    }
                }
                let _ = out.flush();
                return;
            }
        }
        pos += 8 + blen as u64;
        frames += 1;
        if frames % 500_000 == 0 {
            eprintln!("scanned {} frames at {} MB", frames, pos / 1_048_576);
        }
    }
}
