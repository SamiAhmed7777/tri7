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
    store.spawn_build(None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    let tip = loop {
        match store.status() {
            tri7_core::blockindex::StoreStatus::Ready { threaded_tip, .. } => match threaded_tip {
                Some(t) => break t,
                None => {
                    eprintln!("store index has no threaded height yet");
                    std::process::exit(2);
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
        let real_height = match store.block_by_hash(&hash) {
            Ok(Some(hit)) => match hit.height {
                Some(h) if h <= tip => h,
                _ => {
                    pos += (8 + blen) as u64;
                    continue;
                }
            },
            _ => {
                pos += (8 + blen) as u64;
                continue; // reorg leftover / not in the chain
            }
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
    if reached_eof {
        addrindex::finish(&conn, tip).ok();
        addrindex::set_complete(&conn, true).ok();
    } else {
        // partial: keep the covered height and say so explicitly
        addrindex::finish(&conn, last_height).ok();
        addrindex::set_complete(&conn, false).ok();
        eprintln!(
            "addrindex: INCOMPLETE — stopped before the end of the store; index covers h{last_height} of h{tip}"
        );
    }
    println!(
        "addrindex: done — {blocks} blocks, {outputs} outputs, {spends} spends in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
