//! store_tail — copy the last N bytes of a block store into a new store file,
//! aligned to a real frame boundary.
//!
//! Why: a follower node at the chain tip only needs the recent window bodies
//! (wallet live view / staking / balance). The tri7 block index supports
//! mid-chain anchoring: a store whose first frame sits at an arbitrary height
//! threads its heights from the daemon-anchored window (tipstate). So instead
//! of re-syncing 2.2M blocks, take the tail slice of a canonical store.
//!
//! Usage: store_tail <src_blk> <dst_blk> <bytes>

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const MAGIC: u32 = 0x0522_3570;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: store_tail <src_blk> <dst_blk> <bytes>");
        std::process::exit(2);
    }
    let src = PathBuf::from(&args[1]);
    let dst = PathBuf::from(&args[2]);
    let want: u64 = match args[3].parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bytes parse: {}", e);
            std::process::exit(2);
        }
    };
    let mut f = match std::fs::File::open(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {}: {}", src.display(), e);
            std::process::exit(2);
        }
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if want >= len {
        eprintln!("source is {} bytes, nothing to trim", len);
        std::process::exit(2);
    }
    let mut start = len - want;
    // align forward to the next frame header: magic + plausible body length
    let scan_limit = (start + 65_536).min(len.saturating_sub(8));
    let mut buf = [0u8; 8];
    loop {
        if start >= scan_limit {
            eprintln!("no frame boundary found in the first 64 KiB of the slice");
            std::process::exit(1);
        }
        f.seek(SeekFrom::Start(start)).ok();
        if f.read_exact(&mut buf).is_err() {
            eprintln!("read failed near offset {}", start);
            std::process::exit(1);
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let blen = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u64;
        if magic == MAGIC && blen > 100 && blen < 4_000_000 && start + 8 + blen <= len {
            break;
        }
        start += 1;
    }
    let n = len - start;
    eprintln!(
        "slice: {:.2} MB from offset {} to EOF ({} bytes)",
        n as f64 / 1_048_576.0,
        start,
        n
    );
    f.seek(SeekFrom::Start(start)).ok();
    let mut out = match std::fs::File::create(&dst) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("create {}: {}", dst.display(), e);
            std::process::exit(1);
        }
    };
    let mut copy = std::io::BufReader::new(f.take(n));
    match std::io::copy(&mut copy, &mut out) {
        Ok(w) => println!("wrote {} bytes to {}", w, dst.display()),
        Err(e) => {
            eprintln!("copy failed: {}", e);
            std::process::exit(1);
        }
    }
    let _ = out.flush();
}
