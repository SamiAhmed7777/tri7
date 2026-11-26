//! addrindex_build — build the opt-in chain-wide address index
//! (`<state_dir>/addressindex.db`): the data behind getaddressbalance /
//! getaddressutxos / getaddresstxids (legacy `-addressindex=1` parity).
//!
//! Walks the block store SEQUENTIALLY (frames in file order) and maps each
//! frame to its real height through the store index, exactly like the history
//! scanner. Random-access-by-height measured ~17 blocks/s; the sequential walk
//! avoids a seek + header re-read per block.
//!
//! Resumable: the file offset of the last committed slice is stored in meta.
//!
//! Usage:
//!   addrindex_build <block_store.dat> <state_dir> [--full]
//!   addrindex_build <block_store.dat> <state_dir> --deep-wallet-repair[=dry|apply]
//!                [--deep-from <height>] [--deep-wallet <wallet file>]
//!
//! The deep wallet-row repair (item 53) is what the run above cannot do: it
//! walks the CANONICAL chain from the wallet's oldest row — by height through
//! the store's own chain map, never file order — and rebuilds that wallet's
//! rows only. Bare `--deep-wallet-repair` is the dry run: the walk and the
//! plan, nothing written. `=apply` writes it, in ONE transaction, after
//! asking the tipstate oracle whether the chain is still the one it walked.
//! The `--deep` arguments are parsed strictly: a mistyped one stops the run.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use tri7_core::addrindex;
use tri7_core::blockindex::BlockStore;
use tri7_core::legacy::{Block, Reader};

/// A block in the shape the index stores it.
fn block_txs(block: &Block) -> addrindex::BlockTxs {
    block
        .vtx
        .iter()
        .map(|tx| {
            let outs: Vec<(u32, i64, Vec<u8>)> = tx
                .vout
                .iter()
                .enumerate()
                .map(|(i, o)| (i as u32, o.value, o.script_pubkey.clone()))
                .collect();
            let ins: Vec<(Vec<u8>, u32)> = tx
                .vin
                .iter()
                .filter(|v| !v.prev_hash.iter().all(|b| *b == 0))
                .map(|v| (v.prev_hash.to_vec(), v.prev_n))
                .collect();
            (tx.hash().to_vec(), outs, ins)
        })
        .collect()
}

/// The canonical chain as this run sees it: the tipstate window (read once,
/// at start — the same authority that gives the walk its heights) and the
/// store it just threaded. Read-only.
///
/// Below the window the store's own height map answers. It is frozen at
/// build time and only the frames the threading chained — prev_hash to
/// prev_hash, down from the oracle — occupy a height. Item 53's deep
/// repair walks those heights; the window repair (item 52) never asks
/// below its base.
struct ChainView<'a> {
    base: u64,
    tip: u64,
    /// internal-order hashes: hashes[h - base]
    hashes: Vec<[u8; 32]>,
    store: &'a BlockStore,
    /// the last block `block` served, (height, hash)
    last: std::cell::RefCell<Option<(u64, [u8; 32])>>,
}

impl addrindex::CanonicalChain for ChainView<'_> {
    fn base(&self) -> u64 {
        self.base
    }
    fn tip(&self) -> u64 {
        self.tip
    }
    fn hash_at(&self, height: u64) -> Option<[u8; 32]> {
        if height > self.tip {
            return None;
        }
        if height >= self.base {
            if let Some(h) = self.hashes.get((height - self.base) as usize) {
                return Some(*h);
            }
        }
        self.store.canonical_hash_at_height(height)
    }
    fn block(&self, height: u64) -> Option<addrindex::BlockTxs> {
        let hash = self.hash_at(height)?;
        match self.store.block_by_hash(&hash) {
            Ok(Some(hit)) => {
                // a chain walk, not a lookup: where heights follow one
                // another, each block must name the one before it
                let broken = match &*self.last.borrow() {
                    Some((prev_h, prev_hash)) if *prev_h + 1 == height => {
                        hit.block.prev_hash != *prev_hash
                    }
                    _ => false,
                };
                *self.last.borrow_mut() = Some((height, hash));
                if broken {
                    eprintln!(
                        "canonical h{height} does not name the block at h{} — the height map is not a chain here",
                        height - 1
                    );
                    return None;
                }
                Some(block_txs(&hit.block))
            }
            _ => None,
        }
    }
    fn tx_height(&self, txid: &[u8; 32]) -> Result<Option<u64>, String> {
        // every frame holding the transaction; a frame counts only where the
        // chain's own hash at its height IS its block (or below the window,
        // where the threading gives heights to chained frames only)
        for hit in self.store.tx_frames_by_id(txid)? {
            if let Some(h) = hit.block_height {
                if self.hash_at(h).map(|want| want == hit.block_hash).unwrap_or(true) {
                    return Ok(Some(h));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "../testsupport.rs"]
mod testsupport;

const MAGIC: u32 = 0x0522_3570;

/// Fail-closed quarantine: remove the index db (and its WAL/SHM), verifying
/// absence. If removal fails, rename to a `.quarantine` name; if that fails,
/// truncate the main db to zero bytes so SQLite treats it as not-a-database.
/// Returns Err only when the main db still exists non-empty after all steps.
fn quarantine_index_db(db: &std::path::Path) -> Result<(), String> {
    let side = |s: &str| {
        let mut n = db.as_os_str().to_owned();
        n.push(s);
        std::path::PathBuf::from(n)
    };
    let wal = side("-wal");
    let shm = side("-shm");
    let all = [db, &wal, &shm];

    // Attempt 1: delete all three.
    for f in all.iter() {
        let _ = std::fs::remove_file(f);
    }
    // Attempt 2: rename survivors to quarantine names.
    if all.iter().any(|f| f.exists()) {
        for (f, tag) in [(db, "main"), (&wal, "wal"), (&shm, "shm")] {
            if f.exists() {
                let mut q = f.as_os_str().to_owned();
                q.push(format!(".quarantine-{tag}"));
                let _ = std::fs::rename(f, &q);
            }
        }
    }
    // Attempt 3: truncate survivors to zero bytes (SQLite then treats them as
    // corrupt / not-a-database).
    for f in all.iter() {
        if f.exists() {
            let _ = std::fs::write(f, b"");
        }
    }
    // Verified disposition: NOTHING may remain non-empty — a leftover WAL with
    // the main db gone can still resurrect data on the next open.
    for f in all.iter() {
        if f.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            return Err(format!("could not make {} unusable", f.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quarantine cascade must leave NO usable index behind: main db,
    /// WAL and SHM all gone from the directory.
    #[test]
    fn quarantine_removes_the_db_so_consumers_see_no_index() {
        let dir = testsupport::test_dir("tri7-quarantine");
        let db = dir.join("addressindex.db");
        std::fs::write(&db, b"not-a-real-db-but-nonempty").unwrap();
        std::fs::write(dir.join("addressindex.db-wal"), b"wal").unwrap();
        std::fs::write(dir.join("addressindex.db-shm"), b"shm").unwrap();
        quarantine_index_db(&db).expect("quarantine must succeed on a normal directory");
        // ALL three must be gone (or zero-length): a surviving WAL can
        // resurrect the index on the next open.
        for f in [db.clone(), dir.join("addressindex.db-wal"), dir.join("addressindex.db-shm")] {
            let gone = !f.exists()
                || f.metadata().map(|m| m.len() == 0).unwrap_or(true);
            assert!(gone, "{} must not survive quarantine", f.display());
        }
        // NOTE: the rename/truncate fallback arms cannot be force-activated in
        // a test running as root (root bypasses directory write-bits), so the
        // fail-closed final check (nothing non-empty survives) is the tested
        // guarantee.
    }

    /// Every `--deep` argument is either known or the run stops: a mistyped
    /// flag must not turn what was meant as a dry run into something else.
    #[test]
    fn deep_flags_parse_strictly() {
        let p = |s: &[&str]| {
            parse_deep_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>())
        };
        let dry = (DeepArgs { on: true, apply: false, from: 0 }, None);
        assert_eq!(p(&["--deep-wallet-repair"]).unwrap(), dry);
        assert_eq!(p(&["--deep-wallet-repair=dry"]).unwrap(), dry);
        assert_eq!(p(&[]).unwrap(), (DeepArgs::default(), None));
        assert_eq!(
            p(&["--deep-wallet-repair=apply", "--deep-from", "12", "--deep-wallet", "w.dat"])
                .unwrap(),
            (
                DeepArgs {
                    on: true,
                    apply: true,
                    from: 12
                },
                Some("w.dat".to_string())
            )
        );
        // an argument that is not a --deep one is not this parser's to judge
        assert!(!p(&["--full"]).unwrap().0.on);
        for bad in [
            vec!["--deep-wallet-repair=dry-run"],
            vec!["--deep-wallet-repaire"],
            vec!["--deep-from", "5"],
            vec!["--deep-wallet", "w.dat"],
            vec!["--deep-wallet-repair", "--deep-from"],
            vec!["--deep-wallet-repair", "--deep-from", "abc"],
            vec!["--deep-wallet-repair", "--deep-wallet", "--deep-from"],
        ] {
            assert!(p(&bad).is_err(), "{bad:?} must be refused");
        }
    }
}

/// The tipstate oracle as it stands on disk right now: (height, display
/// hash), ascending. Read once for the window and again by the deep repair
/// right before it writes — a reorg that lands under the walk is seen then,
/// not applied over.
fn tipstate_entries(state_dir: &std::path::Path) -> Vec<(u64, String)> {
    let mut v: Vec<(u64, String)> = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(state_dir.join("tipstate.json")) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(entries) = val.get("entries").and_then(|e| e.as_array()) {
                for e in entries {
                    if let (Some(h), Some(hash)) = (
                        e.get("height").and_then(|x| x.as_u64()),
                        e.get("hash").and_then(|x| x.as_str()),
                    ) {
                        v.push((h, hash.to_string()));
                    }
                }
            }
        }
    }
    v.sort_by_key(|(h, _)| *h);
    v.dedup_by_key(|(h, _)| *h);
    v
}

/// The deep-repair command line: everything after the two paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeepArgs {
    /// `--deep-wallet-repair` was given
    on: bool,
    /// `=apply` was given (false: the dry run, and nothing can write without it)
    apply: bool,
    /// `--deep-from` (0: the wallet's oldest row)
    from: u64,
}

/// Parse the deep-repair arguments strictly: an argument that starts with
/// `--deep` is either one this knows or the run stops. A mistyped flag must
/// not turn what was meant as a dry run into something else. Returns the
/// flags and the `--deep-wallet` path, if any.
fn parse_deep_args(args: &[String]) -> Result<(DeepArgs, Option<String>), String> {
    let mut d = DeepArgs::default();
    let mut wallet: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--deep-wallet-repair" || a == "--deep-wallet-repair=dry" {
            d.on = true;
        } else if a == "--deep-wallet-repair=apply" {
            d.on = true;
            d.apply = true;
        } else if a == "--deep-from" || a == "--deep-wallet" {
            let Some(value) = args.get(i + 1).filter(|v| !v.starts_with("--")) else {
                return Err(format!("{a} needs a value"));
            };
            if a == "--deep-from" {
                d.from = value
                    .parse()
                    .map_err(|_| format!("--deep-from wants a height, got {value}"))?;
            } else {
                wallet = Some(value.clone());
            }
            i += 1;
        } else if a.starts_with("--deep") {
            return Err(format!("unknown argument {a}"));
        }
        i += 1;
    }
    if !d.on && (d.from != 0 || wallet.is_some()) {
        return Err("--deep-from and --deep-wallet go with --deep-wallet-repair".into());
    }
    Ok((d, wallet))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: addrindex_build <block_store.dat> <state_dir> [--full]");
        std::process::exit(2);
    }
    let store_path = PathBuf::from(&args[1]);
    let state_dir = PathBuf::from(&args[2]);
    let full = args.iter().any(|a| a == "--full");
    // Item 53's deep wallet-row repair, for the heights the verify below
    // cannot reach. Bare `--deep-wallet-repair` is the dry run: the walk
    // and the plan, nothing written; `=apply` writes it.
    let (deep, deep_apply, deep_from, deep_wallet) = match parse_deep_args(&args[3..]) {
        Ok((d, wallet)) => (d.on, d.apply, d.from, wallet),
        Err(e) => {
            eprintln!("usage: {e}");
            std::process::exit(2);
        }
    };
    if deep && full {
        eprintln!(
            "refusing: --full removes the index the deep repair reads — build it first, repair it after"
        );
        std::process::exit(2);
    }

    if full {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!(
                "{}{}",
                addrindex::db_path(&state_dir).display(),
                suffix
            ));
        }
    }

    let store = BlockStore::new(Some(store_path.clone()), None);
    // The chain tip must come from the tipstate oracle BEFORE the wait: the
    // store's threaded tip starts near genesis and a naive "any value" break
    // fired at h11229, leaving 2.19M blocks unindexed (the whole point of the
    // tool). Wait until threading has reached the tip, with a generous
    // deadline for slow boards.
    //
    // The BUILDER needs the same window as a BuildOracle: spawn_build(None)
    // chains POSITIONALLY and stalls at the first stale fork frame — live
    // 2026-11-11 on DNS2, where three runs froze at h2208645 (need h2209332)
    // for a full hour each while the SAME file threaded to the tip
    // oracle-first (store_gapfix passes the oracle; hash-first authority
    // skips stale frames by hash). Refuse rather than stall.
    // Read from disk, not remembered: the deep repair reads it a second
    // time right before it writes.
    let expected: Vec<(u64, String)> = tipstate_entries(&state_dir);
    let oracle: std::collections::HashMap<String, u64> =
        expected.iter().map(|(h, s)| (s.clone(), *h)).collect();
    println!("tipstate oracle: {} hashes", oracle.len());
    let chain_tip = expected.last().map(|(h, _)| *h).unwrap_or(0);
    if expected.is_empty() || chain_tip == 0 {
        eprintln!(
            "tipstate oracle is empty or has no positive height — cannot determine the chain tip; refusing to index"
        );
        std::process::exit(2);
    }
    let window_base = expected[0].0;
    // the window in internal order, for the verify (bodies are fetched by hash)
    let window_hashes: Vec<[u8; 32]> = expected
        .iter()
        .map(|(_, hex)| tri7_core::p2p::display_to_internal(hex))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| {
            eprintln!("tipstate hash unusable: {e} — refusing to index");
            std::process::exit(2);
        });
    let build_oracle = {
        let base = expected[0].0;
        let mut hashes = Vec::with_capacity(expected.len());
        for (i, (h, hex)) in expected.iter().enumerate() {
            if *h != base + i as u64 {
                eprintln!(
                    "tipstate window non-contiguous at h{h} (expected h{}) — threading without a contiguous oracle stalls at the first stale frame; refusing to index",
                    base + i as u64
                );
                std::process::exit(2);
            }
            match tri7_core::p2p::display_to_internal(hex) {
                Ok(b) => hashes.push(b),
                Err(e) => {
                    eprintln!("tipstate hash {hex} unusable: {e} — refusing to index");
                    std::process::exit(2);
                }
            }
        }
        tri7_core::blockindex::BuildOracle {
            base,
            tip: base + hashes.len() as u64 - 1,
            hashes,
        }
    };
    store.spawn_build(Some(build_oracle));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let tip = loop {
        match store.status() {
            tri7_core::blockindex::StoreStatus::Ready { threaded_tip, .. } => match threaded_tip {
                Some(t) if t >= chain_tip && chain_tip > 0 => break t,
                Some(t) => {
                    if std::time::Instant::now() > deadline {
                        eprintln!(
                            "block index threading stalled at h{t} (need h{chain_tip}); raise the deadline or run on a faster board"
                        );
                        std::process::exit(2);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                }
                None => {
                    // transient: the threading thread has not published a tip
                    // yet — keep waiting within the deadline rather than
                    // aborting the build on a startup race
                    if std::time::Instant::now() > deadline {
                        eprintln!("block index threading never reported a tip");
                        std::process::exit(2);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                }
            },
            tri7_core::blockindex::StoreStatus::Failed(e) => {
                eprintln!("block index: {e}");
                std::process::exit(2);
            }
            _ => {
                if std::time::Instant::now() > deadline {
                    eprintln!("block index build timed out");
                    std::process::exit(2);
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    };
    println!("store threaded to h{tip}");

    let (mut conn, _) = match addrindex::begin(&state_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // BEFORE anything new is indexed: prove every height the chain can vouch
    // for still holds the chain's own block, and repair the ones that do not.
    // A run resumes from its last slice commit and re-walks, so a block the
    // node minted and lost to a heavier branch was indexed while it was the
    // tip and then skipped on every later walk — its outputs stayed unspent
    // under a coverage height the chain does vouch for (item 52: one coin
    // counted twice). The walk below never indexes a second block at a
    // height; this puts the index right before it starts.
    let view = ChainView {
        base: window_base,
        tip: chain_tip,
        hashes: window_hashes,
        store: &store,
        last: std::cell::RefCell::new(None),
    };
    // Item 53: the wallet's rows BELOW that window. The file-order catch-up
    // gave a fork's losing branch its rows there and nothing ever checked
    // them (DNS2, 2026-11-26: 218 of 391 listed rows were outputs of
    // orphaned blocks). Walk the canonical chain from the wallet's oldest
    // row and put those rows right — a dry run unless `=apply` was given.
    // The daemon writes this index while it runs, so every write is
    // conditional on the row still being what the snapshot saw.
    if deep {
        let wallet = match deep_wallet {
            Some(p) => PathBuf::from(p),
            None => state_dir.join("wallet.dat"),
        };
        // the wallet's own addresses; the key material is never read here
        let addrs: Vec<String> = match tri7_core::wallet::read_wallet(&wallet) {
            Ok(w) => w.keys.iter().map(|k| k.address.clone()).collect(),
            Err(e) => {
                eprintln!(
                    "FAIL: deep wallet repair cannot read {} ({e}) — nothing written",
                    wallet.display()
                );
                std::process::exit(2);
            }
        };
        if addrs.is_empty() {
            eprintln!(
                "FAIL: {} holds no key addresses — nothing to scope, nothing written",
                wallet.display()
            );
            std::process::exit(2);
        }
        let started = std::time::Instant::now();
        // the chain must still be the one that was walked, right before the write
        let intact = || {
            let now: std::collections::HashMap<u64, String> =
                tipstate_entries(&state_dir).into_iter().collect();
            expected
                .iter()
                .all(|(h, hex)| now.get(h) == Some(hex))
        };
        let report = match addrindex::deep_wallet_repair(
            &mut conn,
            &view,
            &addrs,
            deep_from,
            deep_apply,
            &intact,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("FAIL: deep wallet repair: {e}");
                std::process::exit(3);
            }
        };
        let tri = |v: i64| {
            format!("{}.{:06}", v / 1_000_000, v.rem_euclid(1_000_000))
        };
        println!(
            "deep wallet repair {} — {} address(es), walked h{}..h{} in {:.1}s",
            if report.applied {
                "APPLIED"
            } else {
                "DRY RUN (nothing written)"
            },
            report.addresses,
            report.from,
            report.to,
            started.elapsed().as_secs_f64()
        );
        println!(
            "  output rows: {} before, {} after",
            report.rows_before, report.rows_after
        );
        println!(
            "  spendable before: {} rows, {} TRI",
            report.unspent_before.0,
            tri(report.unspent_before.1)
        );
        println!(
            "  rows whose block is not canonical: {} removed, {} TRI",
            report.phantom_outputs,
            tri(report.phantom_value)
        );
        println!(
            "  spend markings: {} cleared ({} TRI back to spendable), {} moved to the canonical spend, {} written",
            report.markings_cleared,
            tri(report.restored_value),
            report.markings_moved,
            report.markings_set
        );
        println!(
            "  rows re-heighted: {}, values corrected: {}",
            report.reheighted, report.values_fixed
        );
        println!(
            "  canonical outputs the index never held: {} written, {} TRI",
            report.missing_outputs,
            tri(report.missing_value)
        );
        println!(
            "  history rows: {} removed, {} moved, {} written",
            report.history_removed, report.history_moved, report.history_added
        );
        println!(
            "  left alone (not this walk's to judge): {}; raced (changed under the write): {}",
            report.left_alone, report.raced
        );
        println!(
            "  spendable after ({}): {} rows, {} TRI",
            if report.applied {
                "measured"
            } else {
                "projected"
            },
            report.unspent_after.0,
            tri(report.unspent_after.1)
        );
    }
    let verify = match addrindex::verify_and_repair(&mut conn, &view) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "FAIL: address index verify h{}..h{}: {e} — index NOT marked complete",
                window_base, chain_tip
            );
            if let Err(ce) = addrindex::set_complete(&conn, false) {
                eprintln!("FAIL: could not clear the completion flag ({ce})");
                std::process::exit(4);
            }
            std::process::exit(3);
        }
    };
    if verify.clean {
        println!(
            "address index verify: h{}..h{} — every height holds the chain's block",
            verify.checked_from, verify.checked_to
        );
    } else {
        println!(
            "address index verify: h{}..h{} held another branch's content from h{} — repaired: removed {} phantom output(s), moved {} output row(s) to their block's height, fixed {} spend marking(s), removed {} and moved {} history row(s), wrote {} missing block(s)",
            verify.checked_from,
            verify.checked_to,
            verify.bad_from,
            verify.phantom_outputs,
            verify.reheighted,
            verify.markings_fixed,
            verify.tx_rows_removed,
            verify.tx_rows_moved,
            verify.blocks_reapplied
        );
    }
    let resume_off: u64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(v AS INTEGER)),0) FROM meta WHERE k='offset'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut pos = if full { 0 } else { resume_off };
    let mut unresolved: u64 = 0;
    let mut stale_skipped: u64 = 0;
    let mut stale_height: u64 = 0;
    let mut already_held: u64 = 0;
    let mut lookup_err: Option<String> = None;
    if pos > 0 {
        println!("resuming at file offset {pos}");
    }

    let mut f = match std::fs::File::open(&store_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open store: {e}");
            std::process::exit(2);
        }
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let started = std::time::Instant::now();
    let mut blocks = 0u64;
    let mut outputs = 0u64;
    let mut spends = 0u64;
    let mut in_txn = false;
    let mut last_slice_off = pos;
    // the hash of the last block whose slice COMMITTED (meta tip_hash tracks
    // the committed coverage — a torn tail that gets re-walked on resume must
    // not claim a hash its height doesn't cover)
    let mut last_committed_hash: Option<[u8; 32]> = None;
    // the hash of the last block ACCEPTED this run (committed by the final
    // tail COMMIT at eof — the honest proof for the eof height)
    let mut last_accepted_hash: Option<[u8; 32]> = None;
    // the height of that same block — the OTHER half of the eof stamp pair
    // (meta.height and meta.tip_hash must describe the SAME block)
    let mut last_accepted_height: Option<u64> = None;
    // The height the index actually covers. Stamping `tip` after an abort (a
    // corrupt frame, a parse error) would advertise a complete index that is
    // not complete, so this only advances on a committed slice and only
    // reaches `tip` when the walk really ended.
    let mut last_height = addrindex::index_height(&conn);
    let mut reached_eof = false;

    loop {
        if pos + 8 > len {
            reached_eof = true;
            break;
        }
        if let Err(e) = f.seek(SeekFrom::Start(pos)) {
            eprintln!("seek: {e}");
            break;
        }
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let blen = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if magic != MAGIC || blen == 0 {
            eprintln!("bad frame at offset {pos} (magic {magic:08x}) — stopping");
            break;
        }
        let mut body = vec![0u8; blen];
        if f.read_exact(&mut body).is_err() {
            break;
        }
        let mut r = Reader::new(&body);
        let block = match Block::read(&mut r) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("parse at offset {pos}: {e}");
                break;
            }
        };
        let hash = block.hash();
        let display = tri7_core::p2p::internal_to_display(&hash);
        let oracle_height = oracle.get(&display).copied();
        let real_height = match oracle_height {
            Some(h) => h, // authoritative: tipstate knows this block's height
            None => match store.block_by_hash(&hash) {
            Ok(Some(hit)) => match hit.height {
                Some(h) if h <= tip => h,
                _ => {
                    // The store returned the frame but no usable height. This
                    // whole arm is the ORACLE-MISS path: the frame's hash is
                    // NOT the tipstate's hash at any window height, and the
                    // hash-first build assigns heights only to oracle frames
                    // and positionally-chained frames — so this is a stale
                    // fork leftover (indexed as a frame, height-less by
                    // design). Not canonical: skipping is CORRECT, the same
                    // skip-by-design as the Ok(None) arm below. The old
                    // fail-closed treatment dates from the oracle-less build,
                    // where heights were unreliable and a canonical block
                    // could genuinely hide here — live 2026-11-11 on DNS2:
                    // after the threading fix, exactly 1 such leftover in the
                    // fork-war tail failed every walk ("FAIL: 1 canonical
                    // frame(s) skipped") and the index could never complete
                    // in the reorg world. Lookup ERRORS (below) and anything
                    // the oracle DOES claim stay fail-closed via `unresolved`.
                    if stale_skipped == 0 {
                        println!(
                            "walk: stale fork leftover at offset {pos} (hash {display} not in the tipstate window) — skipping non-canonical frames by design"
                        );
                    }
                    stale_skipped += 1;
                    pos += (8 + blen) as u64;
                    continue;
                }
            },
            Ok(None) => {
                // The store knows nothing of this frame: a reorg leftover /
                // not a canonical block. Skipped by design.
                pos += (8 + blen) as u64;
                continue;
            }
            Err(e) => {
                // A lookup ERROR is not evidence the block is absent — treat
                // it as unresolved so the run refuses to stamp completion.
                unresolved += 1;
                lookup_err = Some(format!("{}: {}", display, e));
                pos += (8 + blen) as u64;
                continue;
            }
            },
        };

        let txs = block_txs(&block);

        if !in_txn {
            if let Err(e) = conn.execute_batch("BEGIN") {
                eprintln!("begin: {e}");
                std::process::exit(1);
            }
            in_txn = true;
        }
        match addrindex::apply_block(&conn, real_height, &hash, &txs) {
            Ok((addrindex::Applied::Applied, o, s)) => {
                outputs += o;
                spends += s;
                last_accepted_hash = Some(hash);
                last_accepted_height = Some(real_height);
            }
            Ok((addrindex::Applied::AlreadyApplied, _, _)) => {
                // this run re-walks everything since its last slice commit;
                // a block the index already holds exactly is done
                already_held += 1;
                last_accepted_hash = Some(hash);
                last_accepted_height = Some(real_height);
            }
            Err(_) if oracle_height.is_none() => {
                // The store's height for this frame contradicts a height the
                // run just proved holds the chain's own block: the height is
                // stale, not the record. Skipping the frame is correct.
                if stale_height == 0 {
                    println!(
                        "walk: frame {display} claims h{real_height}, which holds the chain's block — skipping (stale height)"
                    );
                }
                stale_height += 1;
                pos += (8 + blen) as u64;
                continue;
            }
            Err(e) => {
                // The tipstate itself puts this block at a height that
                // already holds another one. Never index two blocks at one
                // height: stop, and leave the index refusing to pose as
                // complete. The next run's verify puts it right.
                eprintln!("FAIL: h{real_height}: {e} — index NOT marked complete");
                let _ = conn.execute_batch("ROLLBACK");
                if let Err(ce) = addrindex::set_complete(&conn, false) {
                    eprintln!("FAIL: could not clear the completion flag ({ce})");
                    std::process::exit(4);
                }
                std::process::exit(3);
            }
        }
        blocks += 1;
        pos += (8 + blen) as u64;

        if blocks % 500 == 0 {
            if let Err(e) = conn.execute_batch("COMMIT") {
                eprintln!("commit: {e}");
                std::process::exit(1);
            }
            in_txn = false;
            last_slice_off = pos;
            last_height = real_height;
            addrindex::finish(&conn, last_height).ok();
            addrindex::set_offset(&conn, pos).ok();
            // the proof the lag-aware money-path verdict reads: the hash of
            // the block AT the committed height (this slice's last block)
            last_committed_hash = Some(hash);
            addrindex::set_tip_hash(&conn, &hash).ok();
        }
        if blocks % 20_000 == 0 {
            let bps = blocks as f64 / started.elapsed().as_secs_f64().max(0.001);
            println!(
                "addrindex: {blocks} blocks ({bps:.0} blocks/s, {outputs} outputs, {spends} spends, h{real_height})"
            );
        }
    }

    if in_txn {
        let _ = conn.execute_batch("COMMIT");
    }
    // Save the last COMMITTED slice, not the position of a bad frame: saving
    // `pos` here would make every future resume halt on the same frame.
    addrindex::set_offset(&conn, last_slice_off).ok();
    if unresolved > 0 {
        // Canonical frames whose height could not be resolved were skipped:
        // the index is INCOMPLETE, so it must not be stamped complete — a
        // wallet reading it would silently miss those blocks' addresses.
        // Explicitly clear any stale complete flag from a previous run. If
        // even THIS clear fails, fail CLOSED: quarantine the index db —
        // because a database that says complete=true while canonical frames
        // are missing is worse than no database — every consumer must see
        // "no index" rather than a lying one.
        if let Err(e) = addrindex::set_complete(&conn, false) {
            eprintln!(
                "FAIL: could not clear the completion flag after unresolved skips ({e}) — quarantining the index db so no consumer can treat an incomplete index as complete"
            );
            drop(conn);
            let db = addrindex::db_path(&state_dir).display().to_string();
            if let Err(qe) = quarantine_index_db(std::path::Path::new(&db)) {
                eprintln!("FAIL: quarantine also failed ({qe}) — the incomplete index may still be present and MUST be removed manually before any wallet reads it");
                std::process::exit(4);
            }
            eprintln!("index db quarantined: consumers now see \"no index\"");
            std::process::exit(3);
        }
        eprintln!(
            "FAIL: {unresolved} canonical frame(s) skipped — index NOT marked complete{}",
            lookup_err
                .as_ref()
                .map(|e| format!("; last lookup error: {e}"))
                .unwrap_or_default()
        );
        std::process::exit(3);
    }
    if reached_eof {
        // eof: stamp THE PAIR, not two independent facts — meta.height and
        // meta.tip_hash must describe the SAME block, because the lag-aware
        // money-path verdict reads the chain's hash AT meta.height and
        // compares it with meta.tip_hash. The old stamp here paired
        // finish(chain_tip.max(last_height)) — the START-of-walk chain tip
        // — with set_tip_hash(last_accepted), a frame accepted BEYOND that
        // tip whenever blocks landed mid-walk (DNS2 2026-11-17: h2213928
        // stamped with the wallet's own h2213929 mint hash; the verdict
        // refused and the stale wallet-live window — 1823 TRI of the real
        // 5368 — served forever). eof_stamp keeps the pair self-consistent
        // and never advances the height past the content on an empty walk.
        let stamp = addrindex::eof_stamp(
            blocks,
            last_accepted_height.zip(last_accepted_hash),
            last_committed_hash.map(|h| (last_height, h)),
            last_height,
        );
        addrindex::finish(&conn, stamp.height).ok();
        if let Some(h) = stamp.tip_hash {
            addrindex::set_tip_hash(&conn, &h).ok();
        }
        addrindex::set_complete(&conn, true).ok();
    } else {
        // partial: keep the covered height and say so explicitly
        addrindex::finish(&conn, last_height).ok();
        if let Some(h) = last_committed_hash {
            addrindex::set_tip_hash(&conn, &h).ok();
        }
        addrindex::set_complete(&conn, false).ok();
        eprintln!(
            "addrindex: INCOMPLETE — stopped before the end of the store; index covers h{last_height} of h{chain_tip}"
        );
    }
    println!(
        "addrindex: done — {blocks} blocks, {outputs} outputs, {spends} spends, {stale_skipped} stale fork leftover(s) skipped, {stale_height} stale height(s) skipped, {already_held} block(s) already held in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
