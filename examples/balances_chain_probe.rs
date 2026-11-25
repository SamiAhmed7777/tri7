//! Chain-ordered balances replay rehearsal over a node's archive COPY.
//!
//! Read-only towards the node: it takes a COPY of blk0001.dat (the store opens
//! its file write-capable) and a copy of tipstate.json, rebuilds the block
//! index in-process exactly as a node boot does (archive + the tipstate
//! window as the threading oracle, the window's slots seeded into the chain
//! overlay), writes no index cache, then runs the chain-ordered balances
//! replay against it. The verdict printed at the end is what a deploy of the
//! same code would produce on that node: coverage, tip, order, why the walk
//! stopped if it did, supply, timing.
//!
//! usage: balances_chain_probe <blk0001.dat copy> <tipstate.json copy> <out balances.json>
//!
//! Exit 0 = the walk ran (read its coverage); 1 = the index or the replay
//! failed; 2 = usage.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tri7_core::balances_build::{build_balances_snapshot, BalancesBuildOpts, CanonicalBlocks};
use tri7_core::blockindex::{BlockStore, BuildOracle, StoreStatus};
use tri7_core::p2p::{display_to_internal, internal_to_display};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!(
            "usage: balances_chain_probe <blk0001.dat copy> <tipstate.json copy> <out balances.json>"
        );
        std::process::exit(2);
    }
    let blk = PathBuf::from(&args[1]);
    let tipstate = PathBuf::from(&args[2]);
    let out = PathBuf::from(&args[3]);

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tipstate).expect("read tipstate.json"))
            .expect("tipstate.json parses");
    let base = doc["window_base"].as_u64().expect("window_base");
    let entries = doc["entries"].as_array().expect("entries");
    assert!(!entries.is_empty(), "empty tipstate window");
    // the window must thread: the oracle is only as good as its links
    for pair in entries.windows(2) {
        assert_eq!(
            pair[1]["prev"].as_str().expect("entry.prev"),
            pair[0]["hash"].as_str().expect("entry.hash"),
            "tipstate window does not thread at height {:?}",
            pair[1]["height"]
        );
    }
    let hashes: Vec<_> = entries
        .iter()
        .map(|e| display_to_internal(e["hash"].as_str().expect("entry.hash")).expect("hash hex"))
        .collect();
    let tip = base + hashes.len() as u64 - 1;
    let tip_display = entries.last().unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    eprintln!(
        "oracle: window_base h{base} .. tip h{tip} ({} entries), tipstate height {:?}",
        entries.len(),
        doc["height"]
    );

    let store = BlockStore::new(Some(blk.clone()), None);
    store.spawn_build(Some(BuildOracle {
        base,
        hashes: hashes.clone(),
        tip,
    }));
    let t0 = Instant::now();
    let (status, index_secs) = loop {
        match store.status() {
            s @ StoreStatus::Ready { .. } => break (s, t0.elapsed().as_secs_f64()),
            StoreStatus::Failed(e) => {
                eprintln!("index build FAILED: {e}");
                std::process::exit(1);
            }
            _ => {
                if t0.elapsed() > Duration::from_secs(3600) {
                    eprintln!("index build did not finish in an hour");
                    std::process::exit(1);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    };
    eprintln!("index: {status:?} — built in {index_secs:.1}s");
    // the node boot's seed_chain_slots: the window's lineage wins the slots
    for (i, h) in hashes.iter().enumerate() {
        store.set_chain_slot(base + i as u64, *h);
    }
    let at_tip = store
        .block_by_height(tip)
        .expect("block_by_height(tip)")
        .expect("the store serves the window tip");
    assert_eq!(
        internal_to_display(&at_tip.hash),
        tip_display,
        "the built store's block at the window tip is not the window's tip"
    );

    let chain: &dyn CanonicalBlocks = &*store;
    let opts = BalancesBuildOpts {
        blk: &blk,
        canonical: Some(chain),
        out: &out,
        wallet: None,
        utxos_out: None,
        end_height: 0,
        replay_target: Some(tip as usize),
        allow_stale: true,
    };
    let t1 = Instant::now();
    let mut progress = |blocks: usize, live: usize| {
        eprintln!(
            "... {blocks} blocks ({live} live UTXOs) at {:.1}s",
            t1.elapsed().as_secs_f64()
        );
    };
    let rep = match build_balances_snapshot(&opts, &mut progress) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("chain-ordered balances replay FAILED: {e}");
            std::process::exit(1);
        }
    };
    let reached = (rep.covered_to as u64) >= tip;
    println!(
        "{}",
        serde_json::json!({
            "replay_order": rep.replay_order,
            "blocks": rep.blocks,
            "covered_to": rep.covered_to,
            "replay_target": tip,
            "reached_target": reached,
            "cap_note": rep.cap_note,
            "tip_hash": rep.tip_hash_display,
            "tip_is_window_tip": rep.tip_hash_display == tip_display,
            "supply_sats": rep.total_balance_sats,
            "live_utxos": rep.live_utxos,
            "index_build_secs": (index_secs * 10.0).round() / 10.0,
            "replay_secs": (t1.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "out": out.display().to_string(),
        })
    );
}
