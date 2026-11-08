//! Opt-in chain-wide address index — parity with the legacy `-addressindex=1`.
//!
//! Legacy serves `getaddressbalance` / `getaddressutxos` / `getaddresstxids`
//! ONLY when started with `-addressindex=1`; otherwise those RPCs throw
//! "Address index not enabled. Start with -addressindex=1"
//! (`rpcblockchain.cpp:464`). So a wallet-scoped shortcut would answer
//! differently from legacy. This module builds the real thing: every output of
//! every block, in SQLite, with spends recorded so balances are net.
//!
//! Storage: `<state_dir>/addressindex.db`
//!   out(addr TEXT, txid BLOB, n INT, value INT, height INT, spent_height INT NULL)
//!   tx_addr(txid BLOB, addr TEXT)   -- for getaddresstxids

use rusqlite::{params, Connection};
use std::path::Path;

pub struct IndexStats {
    pub blocks: u64,
    pub outputs: u64,
    pub spends: u64,
    pub seconds: f64,
}

pub fn db_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("addressindex.db")
}

pub fn open(path: &Path) -> Result<Connection, String> {
    let c = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    Ok(c)
}

pub fn exists(state_dir: &Path) -> bool {
    db_path(state_dir).exists()
}

fn create_schema(c: &Connection) -> Result<(), String> {
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=OFF;
         CREATE TABLE IF NOT EXISTS out(
             addr TEXT NOT NULL,
             txid BLOB NOT NULL,
             n INTEGER NOT NULL,
             value INTEGER NOT NULL,
             height INTEGER NOT NULL,
             script BLOB,
             spent_height INTEGER,
             PRIMARY KEY(addr, txid, n)
         );
         CREATE INDEX IF NOT EXISTS out_addr_val ON out(addr, spent_height, value);
         -- spend marking and spend-side tx_addr rows look up by (txid,n); the
         -- primary key starts with addr, so without this every input scanned
         -- the whole table (measured: the build crawled at ~17 blocks/s)
         CREATE INDEX IF NOT EXISTS out_tx ON out(txid, n);
         CREATE TABLE IF NOT EXISTS tx_addr(txid BLOB NOT NULL, addr TEXT NOT NULL,
             height INTEGER NOT NULL, PRIMARY KEY(txid, addr));
         CREATE INDEX IF NOT EXISTS tx_addr_addr ON tx_addr(addr, height);
         CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);",
    )
    .map_err(|e| format!("schema: {e}"))?;
    Ok(())
}

fn display_to_internal(hexs: &str) -> Option<[u8; 32]> {
    if hexs.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[31 - i] = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Build (or extend) the index from the block store.
///
/// `blocks` is the caller's iterator of (height, block) so this module does not
/// depend on how the store is threaded; the bin does that part and calls
/// `insert_block` per block. Kept as free functions so a test can feed
/// synthetic blocks without a store.
pub fn begin(state_dir: &Path) -> Result<(Connection, u64), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("mkdir: {e}"))?;
    let c = open(&db_path(state_dir))?;
    create_schema(&c)?;
    let last: u64 = c
        .query_row("SELECT COALESCE(MAX(CAST(v AS INTEGER)),0) FROM meta WHERE k='height'", [], |r| r.get(0))
        .unwrap_or(0);
    Ok((c, last))
}

pub fn insert_block(
    c: &Connection,
    height: u64,
    txs: &[(Vec<u8>, Vec<(u32, i64, Vec<u8>)>, Vec<(Vec<u8>, u32)>)],
) -> Result<(u64, u64), String> {
    // txs: (txid internal, outputs (n, value, script), spends (prev_txid, n))
    let mut outs = 0u64;
    let mut spends = 0u64;
    // The CALLER owns the transaction: committing per block costs an fsync per
    // block (~60 ms here), which is what made this crawl. Batch in the builder.
    for (txid, vout, vin) in txs {
        for (n, value, script) in vout {
            let Some(dest) = crate::address::script_to_dest(script) else {
                continue; // OP_RETURN / non-standard: not addressable
            };
            let addr = crate::address::payload_to_address(&dest.payload());
            c.prepare_cached(
                "INSERT OR IGNORE INTO out(addr,txid,n,value,height,script) VALUES(?,?,?,?,?,?)",
            )
            .map_err(|e| format!("out stmt: {e}"))?
            .execute(params![addr, txid, *n as i64, value, height as i64, script])
            .map_err(|e| format!("out insert: {e}"))?;
            c.prepare_cached("INSERT OR IGNORE INTO tx_addr(txid,addr,height) VALUES(?,?,?)")
                .map_err(|e| format!("tx_addr stmt: {e}"))?
                .execute(params![txid, addr, height as i64])
                .map_err(|e| format!("tx_addr insert: {e}"))?;
            outs += 1;
        }
        for (prev_txid, n) in vin {
            // mark the spent output at its own address row
            // Mark the prevout spent AND record the spending tx against the
            // address it came from: legacy getaddresstxids lists both payments
            // TO an address and transactions SPENDING its coins.
            let mut st = c
                .prepare_cached(
                    "UPDATE out SET spent_height=? WHERE txid=? AND n=? AND spent_height IS NULL RETURNING addr",
                )
                .map_err(|e| format!("spend stmt: {e}"))?;
            let spent_addrs: Vec<String> = st
                .query_map(params![height as i64, prev_txid, *n as i64], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|e| format!("spend query: {e}"))?
                .filter_map(|r| r.ok())
                .collect();
            drop(st);
            for a in spent_addrs {
                c.prepare_cached("INSERT OR IGNORE INTO tx_addr(txid,addr,height) VALUES(?,?,?)")
                    .map_err(|e| format!("tx_addr spend stmt: {e}"))?
                    .execute(params![txid, a, height as i64])
                    .map_err(|e| format!("tx_addr spend insert: {e}"))?;
            }
            spends += 1;
        }
    }
    Ok((outs, spends))
}

/// Whether the index reached the end of the store (true) or stopped early.
pub fn set_complete(c: &Connection, complete: bool) -> Result<(), String> {
    c.execute(
        "INSERT INTO meta(k,v) VALUES('complete',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![if complete { "1" } else { "0" }],
    )
    .map_err(|e| format!("meta complete: {e}"))?;
    Ok(())
}

pub fn is_complete(c: &Connection) -> bool {
    c.query_row(
        "SELECT COALESCE(MAX(v),'0') FROM meta WHERE k='complete'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v == "1")
    .unwrap_or(false)
}

/// Record the store file offset of the last committed slice (resume point).
pub fn set_offset(c: &Connection, off: u64) -> Result<(), String> {
    c.execute(
        "INSERT INTO meta(k,v) VALUES('offset',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![off.to_string()],
    )
    .map_err(|e| format!("meta offset: {e}"))?;
    Ok(())
}

pub fn finish(c: &Connection, height: u64) -> Result<(), String> {
    c.execute(
        "INSERT INTO meta(k,v) VALUES('height',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![height.to_string()],
    )
    .map_err(|e| format!("meta: {e}"))?;
    Ok(())
}

/// getaddressbalance: {balance, received} in sats.
pub fn balance(c: &Connection, addrs: &[String]) -> Result<(i64, i64), String> {
    let mut bal = 0i64;
    let mut recv = 0i64;
    let mut st = c
        .prepare("SELECT value, spent_height FROM out WHERE addr=?1")
        .map_err(|e| e.to_string())?;
    for a in addrs {
        let rows = st
            .query_map(params![a], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (v, spent) = row.map_err(|e| e.to_string())?;
            recv += v;
            if spent.is_none() {
                bal += v;
            }
        }
    }
    Ok((bal, recv))
}

pub struct UtxoRowOut {
    pub address: String,
    pub txid: [u8; 32],
    pub n: u32,
    pub value: i64,
    pub height: u64,
    pub script: Vec<u8>,
}

pub fn utxos(c: &Connection, addrs: &[String]) -> Result<Vec<UtxoRowOut>, String> {
    let mut out = Vec::new();
    let mut st = c
        .prepare("SELECT addr, txid, n, value, height, script FROM out WHERE addr=?1 AND spent_height IS NULL ORDER BY height")
        .map_err(|e| e.to_string())?;
    for a in addrs {
        let rows = st
            .query_map(params![a], |r| {
                let txid_blob: Vec<u8> = r.get(1)?;
                let mut txid = [0u8; 32];
                if txid_blob.len() == 32 {
                    txid.copy_from_slice(&txid_blob);
                }
                Ok(UtxoRowOut {
                    address: r.get(0)?,
                    txid,
                    n: r.get::<_, i64>(2)? as u32,
                    value: r.get(3)?,
                    height: r.get::<_, i64>(4)? as u64,
                    script: r.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
                })
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
    }
    Ok(out)
}

pub fn txids(c: &Connection, addrs: &[String]) -> Result<Vec<[u8; 32]>, String> {
    let mut out = Vec::new();
    let mut st = c
        .prepare("SELECT txid FROM tx_addr WHERE addr=?1 ORDER BY height, txid")
        .map_err(|e| e.to_string())?;
    for a in addrs {
        let rows = st
            .query_map(params![a], |r| r.get::<_, Vec<u8>>(0))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let b = row.map_err(|e| e.to_string())?;
            if b.len() == 32 {
                let mut t = [0u8; 32];
                t.copy_from_slice(&b);
                out.push(t);
            }
        }
    }
    // preserve height order from SQL while removing duplicates
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(*t));
    Ok(out)
}

/// Keeps the index in step with block acceptance instead of rebuilding it.
///
/// Opt-in stays opt-in: it only opens when `addressindex.db` already exists.
/// If the DB is behind (a gap), apply() refuses rather than writing a block out
/// of order, because a missing output row would make a later spend unmarkable
/// and the balance would drift. The 5-minute builder closes any such gap.
pub struct LiveIndex {
    conn: Connection,
    next_height: u64,
}

impl LiveIndex {
    pub fn open_if_built(state_dir: &Path) -> Option<LiveIndex> {
        if !exists(state_dir) {
            return None;
        }
        let conn = open(&db_path(state_dir)).ok()?;
        let next = index_height(&conn) + 1;
        Some(LiveIndex {
            conn,
            next_height: next,
        })
    }

    pub fn apply(&mut self, block: &crate::legacy::Block, height: u64) -> Result<(), String> {
        if height != self.next_height {
            return Err(format!(
                "index is at h{}, block is h{} (builder will catch up)",
                self.next_height.saturating_sub(1),
                height
            ));
        }
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
        // One transaction, held by rusqlite's RAII guard: ANY error after
        // this point - a failed output insert, a failed meta write, a failed
        // height update - rolls the whole block back. The index never keeps
        // half of a block.
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("begin: {e}"))?;
        insert_block(&tx, height, &txs)?;
        // Record the hash of the block this index has applied: the height
        // alone does not prove the index is on the SAME chain the node is
        // serving. The hash does.
        let block_hash = crate::legacy::hash9(&block_header_bytes(block));
        tx.execute(
            "INSERT INTO meta(k,v) VALUES('tip_hash',?1) \
             ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [hex32(&block_hash)],
        )
        .map_err(|e| format!("tip_hash: {e}"))?;
        finish(&tx, height)?;
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        self.next_height = height + 1;
        Ok(())
    }
}

/// Legacy failure text when the index is absent.
pub const NOT_ENABLED: &str = "Address index not enabled. Start with -addressindex=1";

/// Whether the live address index may be served as the wallet's whole
/// spendable set.
///
/// It may be when ALL of these hold:
///  - the index is FINISHED (`complete`) - a partial build does not know
///    every coin;
///  - the index has applied exactly the chain the node serves: its applied
///    height equals the chain height AND the hash it recorded for that block
///    equals the chain's hash at that height. The height alone would not
///    prove the index is on the same chain; the hash does;
///  - the caller knows that hash (an unknown chain hash means we cannot
///    prove anything).
///
/// The facts (height, hash, complete) come from the index itself; the
/// verdict is computed here so every caller - the daemon and the tests -
/// uses the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authority {
    /// the index may be served as the whole spendable set
    pub authoritative: bool,
    /// the height the index has applied (its coverage height)
    pub coverage_height: u64,
    /// the hash the index recorded for that height
    pub index_tip_hash: Option<[u8; 32]>,
    /// the index is a finished index
    pub complete: bool,
}

/// The facts the verdict is computed from.
#[derive(Debug, Clone, Copy)]
pub struct IndexFacts {
    pub coverage_height: u64,
    pub index_tip_hash: Option<[u8; 32]>,
    pub complete: bool,
}

/// Read the facts from an open index.
pub fn facts(c: &Connection) -> IndexFacts {
    IndexFacts {
        coverage_height: index_height(c),
        index_tip_hash: index_tip_hash(c),
        complete: is_complete(c),
    }
}

/// The single rule for "may the live index be served as the whole set".
pub fn authority(facts: IndexFacts, chain_height: u64, chain_hash: Option<[u8; 32]>) -> Authority {
    let same_chain = match (facts.index_tip_hash, chain_hash) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    Authority {
        authoritative: facts.complete && same_chain && facts.coverage_height == chain_height,
        coverage_height: facts.coverage_height,
        index_tip_hash: facts.index_tip_hash,
        complete: facts.complete,
    }
}

/// The wallet's spendable set and the index's facts, read under ONE
/// transaction: a block applied while we read must not tear the answer
/// (the utxos from one height and the height/hash of another).
pub fn wallet_view(
    c: &mut Connection,
    addrs: &[String],
) -> Result<(IndexFacts, Vec<UtxoRowOut>), String> {
    // rusqlite's RAII transaction: the read snapshot is held until it is
    // committed, and a failure rolls it back.
    let tx = c.transaction().map_err(|e| format!("begin: {e}"))?;
    let facts = facts(&tx);
    let rows = utxos(&tx, addrs)?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    Ok((facts, rows))
}

/// The hash the index recorded for the last block it applied.
pub fn index_tip_hash(c: &Connection) -> Option<[u8; 32]> {
    let hex: String = c
        .query_row(
            "SELECT v FROM meta WHERE k='tip_hash'",
            [],
            |r| r.get(0),
        )
        .ok()?;
    let bytes = crate::p2p::hex_decode(&hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn hex32(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{:02x}", b)).collect()
}

/// The 80-byte header of a block - the bytes the block hash is taken over.
fn block_header_bytes(block: &crate::legacy::Block) -> Vec<u8> {
    let mut w: Vec<u8> = Vec::with_capacity(80);
    w.extend_from_slice(&block.version.to_le_bytes());
    w.extend_from_slice(&block.prev_hash);
    w.extend_from_slice(&block.merkle_root);
    w.extend_from_slice(&block.time.to_le_bytes());
    w.extend_from_slice(&block.bits.to_le_bytes());
    w.extend_from_slice(&block.nonce.to_le_bytes());
    w.truncate(80);
    w
}

pub fn index_height(c: &Connection) -> u64 {
    c.query_row(
        "SELECT COALESCE(MAX(CAST(v AS INTEGER)),0) FROM meta WHERE k='height'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Helper used by the builder bin: display-order hex for a stored txid.
pub fn txid_hex(txid: &[u8; 32]) -> String {
    let mut r = *txid;
    r.reverse();
    r.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Internal order from display hex (for callers holding display strings).
pub fn txid_blob(display_hex: &str) -> Option<[u8; 32]> {
    display_to_internal(display_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("tri7addrindex-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// p2pkh script for a 20-byte payload (OP_DUP OP_HASH160 20 <h> OP_EQUALVERIFY OP_CHECKSIG)
    fn p2pkh(h: u8) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend(std::iter::repeat(h).take(20));
        s.extend_from_slice(&[0x88, 0xac]);
        s
    }

    #[test]
    fn insert_spend_and_query() {
        let dir = tmpdir("q");
        let (c, _) = begin(&dir).unwrap();
        let txid_a = [1u8; 32];
        let txid_b = [2u8; 32];
        // block 1: txid_a pays 5_000_000 to address(p2pkh(7))
        insert_block(
            &c,
            1,
            &[(txid_a.to_vec(), vec![(0, 5_000_000, p2pkh(7))], vec![])],
        )
        .unwrap();
        finish(&c, 1).unwrap();
        let addr = crate::address::payload_to_address(
            &crate::address::script_to_dest(&p2pkh(7)).unwrap().payload(),
        );
        let (bal, recv) = balance(&c, &[addr.clone()]).unwrap();
        assert_eq!((bal, recv), (5_000_000, 5_000_000));
        assert_eq!(utxos(&c, &[addr.clone()]).unwrap().len(), 1);

        // block 2 spends it
        insert_block(
            &c,
            2,
            &[(txid_b.to_vec(), vec![(0, 4_900_000, p2pkh(8))], vec![(txid_a.to_vec(), 0)])],
        )
        .unwrap();
        finish(&c, 2).unwrap();
        let (bal2, recv2) = balance(&c, &[addr.clone()]).unwrap();
        assert_eq!(bal2, 0, "spent output must not count toward balance");
        assert_eq!(recv2, 5_000_000, "received is lifetime");
        assert!(utxos(&c, &[addr.clone()]).unwrap().is_empty());
        // legacy getaddresstxids lists BOTH the payment to the address and the
        // transaction that spent it, in height order
        let ids = txids(&c, &[addr]).unwrap();
        assert_eq!(ids.len(), 2, "payment + spend");
        assert_eq!(ids[0], txid_a);
        assert_eq!(ids[1], txid_b);
    }
}
