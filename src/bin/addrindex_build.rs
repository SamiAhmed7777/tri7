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

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use tri7_core::addrindex;
use tri7_core::blockindex::BlockStore;
use tri7_core::legacy::{Block, Reader};

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
        let dir = std::env::temp_dir().join(format!("tri7-quarantine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
    }
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
    let expected: Vec<(u64, String)> = {
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
    };
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

    let (conn, _) = match addrindex::begin(&state_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let resume_off: u64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(v AS INTEGER)),0) FROM meta WHERE k='offset'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut pos = if full { 0 } else { resume_off };
    let mut unresolved: u64 = 0;
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
        let real_height = match oracle.get(&display).copied() {
            Some(h) => h, // authoritative: tipstate knows this block's height
            None => match store.block_by_hash(&hash) {
            Ok(Some(hit)) => match hit.height {
                Some(h) if h <= tip => h,
                _ => {
                    // The store returned the frame but no usable height:
                    // skipping silently would stamp the index "complete" while
                    // canonical blocks are missing from it.
                    unresolved += 1;
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

        let mut txs: Vec<(Vec<u8>, Vec<(u32, i64, Vec<u8>)>, Vec<(Vec<u8>, u32)>)> = Vec::new();
        for tx in &block.vtx {
            let txid = tx.hash().to_vec();
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
            txs.push((txid, outs, ins));
        }

        if !in_txn {
            if let Err(e) = conn.execute_batch("BEGIN") {
                eprintln!("begin: {e}");
                std::process::exit(1);
            }
            in_txn = true;
        }
        match addrindex::insert_block(&conn, real_height, &txs) {
            Ok((o, s)) => {
                outputs += o;
                spends += s;
            }
            Err(e) => {
                eprintln!("h{real_height}: {e}");
                std::process::exit(1);
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
        addrindex::finish(&conn, chain_tip.max(last_height)).ok();
        addrindex::set_complete(&conn, true).ok();
    } else {
        // partial: keep the covered height and say so explicitly
        addrindex::finish(&conn, last_height).ok();
        addrindex::set_complete(&conn, false).ok();
        eprintln!(
            "addrindex: INCOMPLETE — stopped before the end of the store; index covers h{last_height} of h{chain_tip}"
        );
    }
    println!(
        "addrindex: done — {blocks} blocks, {outputs} outputs, {spends} spends in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
