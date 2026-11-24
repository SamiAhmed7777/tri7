//! Height-map audit over a real node store (fleet item 44, RPC-HEIGHTMAP-TRUTH).
//!
//! READ-ONLY: the block index is built in memory with NO cache dir, so nothing
//! is written and the live node's file is only read. Reports the thread result,
//! every gap in the height map below the threaded tip, and the map's answer for
//! the heights / block hashes given on the command line. Run it before and
//! after a threading change on the same store to prove the change on real bytes.
//!
//!   cargo run --release --example heightmap_audit -- \
//!       /var/lib/tri7/blk0001.dat /var/lib/tri7 2208645 a4fa0080… 2208646
//!
//! A numeric argument probes by height (getblockhash + the height getblock
//! reports for that block); a 64-hex argument probes a block by hash (the
//! height getblock would report — `None` for a block off the active chain).
//! The threading oracle (authoritative window [base..=tip]) is read from
//! <state-dir>/tipstate.json, exactly as the node builds it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tri7_core::blockindex::{BlockStore, BuildOracle, StoreStatus};
use tri7_core::p2p::display_to_internal;

fn oracle_from_tipstate(dir: &std::path::Path) -> Option<BuildOracle> {
    let raw = std::fs::read_to_string(dir.join("tipstate.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entries = v.get("entries")?.as_array()?;
    let base = entries.first()?.get("height")?.as_u64()?;
    let tip = v.get("height")?.as_u64()?;
    let mut hashes = Vec::with_capacity(entries.len());
    for e in entries {
        // tipstate stores display hex; the oracle wants internal byte order
        hashes.push(display_to_internal(e.get("hash")?.as_str()?).ok()?);
    }
    Some(BuildOracle { base, hashes, tip })
}

/// Linux /proc/self/status field in kB (VmHWM = peak resident set).
fn proc_status_kb(field: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(field).and_then(|r| r.trim().split(' ').next()?.parse().ok()))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: heightmap_audit <blocks-file> <state-dir> [height | block-hash ...]");
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let state_dir = PathBuf::from(&args[2]);
    let oracle = oracle_from_tipstate(&state_dir);
    match &oracle {
        Some(o) => println!(
            "== HEIGHTMAP AUDIT == {}  oracle: window h{}..=h{} ({} entries)",
            blk.display(),
            o.base,
            o.tip,
            o.hashes.len()
        ),
        None => println!("== HEIGHTMAP AUDIT == {}  oracle: none", blk.display()),
    }

    // No cache dir: the build is never saved and the live index is untouched.
    let store = BlockStore::new(Some(blk), None);
    let wall = Instant::now();
    store.spawn_build(oracle);
    loop {
        match store.status() {
            StoreStatus::Ready { .. } | StoreStatus::Failed(_) => break,
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    match store.status() {
        StoreStatus::Ready {
            frames,
            heights_ok,
            threaded_tip,
            build_secs,
            ..
        } => {
            println!(
                "build: ready in {build_secs}s (wall {:.1}s)  frames {frames}  heights_ok {heights_ok}  \
                 threaded_tip {}  peak_rss {} MB",
                wall.elapsed().as_secs_f64(),
                threaded_tip.map(|t| format!("h{t}")).unwrap_or_else(|| "none".into()),
                proc_status_kb("VmHWM:").unwrap_or(0) / 1024,
            );
            let tip = threaded_tip.unwrap_or(0);
            let mut gaps = 0u64;
            let mut runs: Vec<(u64, u64)> = Vec::new();
            let mut run: Option<(u64, u64)> = None;
            for h in 0..=tip {
                if store.frame_offset(h).is_none() {
                    gaps += 1;
                    run = Some(match run {
                        Some((a, _)) => (a, h),
                        None => (h, h),
                    });
                } else if let Some(r) = run.take() {
                    runs.push(r);
                }
            }
            if let Some(r) = run {
                runs.push(r);
            }
            println!(
                "height map: {gaps} of {} heights at or below the threaded tip have NO frame, in {} gap run(s)",
                tip + 1,
                runs.len()
            );
            for (a, b) in runs.iter().take(12) {
                println!("  gap h{a}..h{b}  ({} heights)", b - a + 1);
            }
            if runs.len() > 12 {
                println!("  … {} more gap run(s)", runs.len() - 12);
            }
            for arg in &args[3..] {
                match arg.parse::<u64>() {
                    Ok(h) => {
                        let answer = store.hash_at_height(h).ok().flatten();
                        match answer {
                            Some(hash) => {
                                let hit = display_to_internal(&hash)
                                    .ok()
                                    .and_then(|k| store.block_by_hash(&k).ok().flatten());
                                println!(
                                    "  by height h{h}: {hash}  (getblock height: {})",
                                    hit.and_then(|x| x.height)
                                        .map(|x| format!("Some({x})"))
                                        .unwrap_or_else(|| "None".into())
                                );
                            }
                            None => println!("  by height h{h}: MISS (no block on the map at this height)"),
                        }
                    }
                    Err(_) => match display_to_internal(arg) {
                        Ok(k) => {
                            let hit = store.block_by_hash(&k).ok().flatten();
                            match hit {
                                Some(x) => println!(
                                    "  by hash {arg}: stored, getblock height: {}",
                                    x.height.map(|x| format!("Some({x})")).unwrap_or_else(|| "None".into())
                                ),
                                None => println!("  by hash {arg}: not in the store"),
                            }
                        }
                        Err(e) => println!("  {arg}: not a height or a block hash ({e})"),
                    },
                }
            }
        }
        StoreStatus::Failed(e) => {
            println!("build: FAILED — {e}");
            std::process::exit(1);
        }
        _ => unreachable!("loop exits only on Ready/Failed"),
    }
}
