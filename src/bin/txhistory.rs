//! txhistory — build the full wallet history file (history.json) from a block
//! store, so the GUI can show every transaction the wallet ever took part in,
//! dated the way the legacy client dates them.
//!
//! Usage:
//!   txhistory <store_blk> <state_dir> <wallet.dat> [--full]
//!
//! Without --full the scan resumes from the cached height (history.json).
//! With --full it starts at genesis and rewrites the cache.

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: txhistory <store_blk> <state_dir> <wallet.dat> [--full]");
        std::process::exit(2);
    }
    let store = PathBuf::from(&args[1]);
    let state_dir = PathBuf::from(&args[2]);
    let wallet_path = PathBuf::from(&args[3]);
    let full = args.iter().any(|a| a == "--full");

    let wallet = match tri7_core::wallet::read_wallet(&wallet_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wallet {}: {}", wallet_path.display(), e);
            std::process::exit(2);
        }
    };
    eprintln!(
        "txhistory: {} keys from {}",
        wallet.keys.len(),
        wallet_path.display()
    );

    let (from, hint) = if full {
        (0u64, None)
    } else {
        match tri7_core::history::load_cache(&state_dir) {
            Some(c) => {
                eprintln!(
                    "txhistory: resuming after h{} ({} cached rows)",
                    c.scanned_height,
                    c.rows.len()
                );
                (c.scanned_height + 1, Some(c.scanned_offset))
            }
            None => (0u64, None),
        }
    };

    match tri7_core::history::scan(&store, &state_dir, &wallet, from, hint) {
        Ok(cache) => {
            if let Err(e) = tri7_core::history::save_cache(&state_dir, &cache) {
                eprintln!("save: {}", e);
                std::process::exit(1);
            }
            let ins = cache.rows.iter().filter(|r| r.direction == "in").count();
            let outs = cache.rows.iter().filter(|r| r.direction == "out").count();
            let first = cache.rows.iter().map(|r| r.time).min().unwrap_or(0);
            let last = cache.rows.iter().map(|r| r.time).max().unwrap_or(0);
            println!(
                "history.json: {} rows ({} in / {} out) scanned to h{} — dates {} .. {}",
                cache.rows.len(),
                ins,
                outs,
                cache.scanned_height,
                stamp(first),
                stamp(last)
            );
        }
        Err(e) => {
            eprintln!("scan failed: {}", e);
            std::process::exit(1);
        }
    }
}

/// UTC YYYY-MM-DD for a unix stamp (no chrono dependency).
fn stamp(secs: u32) -> String {
    if secs == 0 {
        return "-".into();
    }
    let days = (secs as i64).div_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
