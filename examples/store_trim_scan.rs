//! store_trim_scan — truncate a block store at a height using ONLY a
//! sequential frame scan (no index, no locks, no dependencies beyond std).
//!
//! Store layout (tri7 blk store): repeated frames of
//!   [magic u32 LE = 0x05223570][body_len u32 LE][body ...]
//! Frame 0 is genesis, frame N is height N (a store copied from a synced
//! node starts at h0). Stale tails from an abandoned branch are exactly the
//! frames at/after the first link mismatch — trimming there lets the daemon
//! refetch that range from peers.
//!
//! Usage: store_trim_scan <blk_path> <height> [--dry-run]

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

const MAGIC: u32 = 0x0522_3570;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: store_trim_scan <blk_path> <height> [--dry-run]");
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let target: u64 = match args[2].parse() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("height parse: {}", e);
            std::process::exit(2);
        }
    };
    let dry = args.iter().any(|a| a == "--dry-run");
    let mut f = match std::fs::File::open(&blk) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {}: {}", blk.display(), e);
            std::process::exit(2);
        }
    };
    let file_len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut pos: u64 = 0;
    let mut height: u64 = 0;
    let mut buf = vec![0u8; 8];
    loop {
        if pos + 8 > file_len {
            eprintln!(
                "reached EOF at offset {} (frame ~h{}) before h{}",
                pos, height, target
            );
            std::process::exit(1);
        }
        f.seek(SeekFrom::Start(pos)).ok();
        if f.read_exact(&mut buf).is_err() {
            eprintln!("read header at {}: unexpected EOF", pos);
            std::process::exit(1);
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u64;
        if magic != MAGIC {
            eprintln!(
                "bad frame magic at offset {} (frame ~h{}, magic {:#010x}) — not a tri7 store?",
                pos, height, magic
            );
            std::process::exit(1);
        }
        if height == target {
            println!(
                "blk={} height={} frame_offset={} size={} trim_to={} ({} bytes removed)",
                blk.display(),
                height,
                pos,
                file_len,
                pos,
                file_len.saturating_sub(pos)
            );
            if dry {
                println!("dry-run: no changes written");
                return;
            }
            match std::fs::OpenOptions::new().write(true).open(&blk) {
                Ok(fw) => {
                    if let Err(e) = fw.set_len(pos) {
                        eprintln!("truncate failed: {}", e);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("open for write: {}", e);
                    std::process::exit(1);
                }
            }
            println!("trimmed. delete the daemon's index cache dir, then restart it.");
            return;
        }
        pos += 8 + len;
        height += 1;
        if height % 500_000 == 0 {
            eprintln!("scan: h{} at {} MB", height, pos / 1_048_576);
        }
    }
}
