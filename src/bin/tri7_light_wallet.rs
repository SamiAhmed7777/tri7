//! tri7_light_wallet — watch-only TRI light wallet CLI (pipeline P6.2).
//!
//! Monitors explicitly listed TRI addresses over the public explorer REST
//! surface: no node, no chain download, no keys, cannot spend or stake.
//! See `src/light_wallet.rs` for the trust model and docs/light-wallet.md
//! for usage. Build: `cargo build --features light-wallet --bin
//! tri7_light_wallet`.

use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use clap::Parser;

use tri7_core::light_wallet::{self, Config, Verdict, MAX_ADDRESSES};

#[derive(Parser)]
#[command(
    name = "tri7_light_wallet",
    version,
    about = "Watch-only TRI light wallet: explorer-reported balances for explicitly listed addresses.\nNo node, no chain download, no keys — cannot spend, stake, or broadcast.",
    after_help = "Exit codes: 0 all addresses OK; 2 some address INCONSISTENT/UNAVAILABLE (fail closed); 1 usage/config/transport error."
)]
struct Args {
    /// TRI address to watch (repeatable)
    #[arg(long = "address", value_name = "TR_ADDRESS")]
    address: Vec<String>,
    /// File with one TR… address per line ('#' comments and blank lines ignored)
    #[arg(long = "addresses", value_name = "FILE")]
    addresses_file: Option<PathBuf>,
    /// Explorer REST base URL (default: the public TRI explorer)
    #[arg(long, default_value = light_wallet::DEFAULT_EXPLORER)]
    explorer: String,
    /// Max index lag behind the chain tip (blocks) before failing closed
    #[arg(long, default_value_t = 10)]
    max_lag: u64,
    /// Retries per address per refresh on stale/incoherent reads
    #[arg(long, default_value_t = 2)]
    retries: u32,
    /// Recent txids shown per address
    #[arg(long, default_value_t = 5)]
    recent: usize,
    /// Per-read HTTP timeout (seconds)
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    /// Emit machine-readable JSON (for agents) instead of the human table
    #[arg(long)]
    json: bool,
    /// Poll forever, refreshing every SECONDS
    #[arg(long, value_name = "SECONDS")]
    watch: Option<u64>,
}

fn main() {
    let args = Args::parse();

    // Collect + validate the explicit watch list (spec §4: explicit list,
    // no gap scanning; v1 has no seed/xpub input by design).
    let mut addrs: Vec<String> = Vec::new();
    if let Some(path) = &args.addresses_file {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                exit(1);
            }
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            addrs.push(line.to_string());
        }
    }
    addrs.extend(args.address.iter().cloned());

    if addrs.is_empty() {
        eprintln!("error: no addresses — pass --address TR… and/or --addresses FILE");
        exit(1);
    }
    // Dedupe (order-preserving) + validate.
    let mut seen = std::collections::HashSet::new();
    let mut list: Vec<String> = Vec::new();
    for a in addrs {
        if seen.insert(a.clone()) {
            list.push(a);
        }
    }
    if list.len() > MAX_ADDRESSES {
        eprintln!("error: {} addresses exceeds cap {MAX_ADDRESSES}", list.len());
        exit(1);
    }
    for a in &list {
        if let Err(e) = light_wallet::validate_address(a) {
            eprintln!("error: {e}");
            exit(1);
        }
    }

    let cfg = Config {
        explorer: args.explorer.clone(),
        max_lag: args.max_lag,
        retries: args.retries,
        txids_limit: args.recent,
        timeout: Duration::from_secs(args.timeout),
    };

    loop {
        match light_wallet::refresh(&cfg, &list) {
            Ok(report) => {
                if args.json {
                    println!("{}", light_wallet::render_json(&report));
                } else {
                    println!("{}", light_wallet::render_human(&report));
                }
                let any_bad = report.addresses.iter().any(|a| a.verdict != Verdict::Ok);
                if args.watch.is_none() {
                    exit(if any_bad { 2 } else { 0 });
                }
            }
            Err(e) => {
                // Refresh-level failure (chaininfo unreachable/malformed).
                if args.watch.is_none() {
                    eprintln!("error: {e}");
                    exit(1);
                }
                eprintln!("error: {e} (retrying on next watch cycle)");
            }
        }
        match args.watch {
            Some(secs) => {
                std::thread::sleep(Duration::from_secs(secs.max(1)));
                println!();
            }
            None => break,
        }
    }
}
