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
    // EVERY connection gets a busy timeout. The builder holds one write
    // transaction per block batch, and the daemon's apply path and RPC readers
    // hit the same file: with no timeout they failed instantly with
    // SQLITE_BUSY ("database is locked"), and the accept loop treats an apply
    // failure as "skip this block" — the index then froze at that height for
    // good because next_height never advanced past it.
    c.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(|e| format!("busy_timeout {}: {e}", path.display()))?;
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

/// Record the hash of the last applied block. The daemon's per-block append
/// (LiveIndex::apply) writes this on every accepted block; the catch-up
/// builder writes it too, for the last block of each committed slice. THE
/// LAG-AWARE MONEY-PATH VERDICT (authority_with_lag, 2026-11-17) proves
/// same-chain from THIS fact: a builder-maintained index without it carries
/// no proof and is refused (fail closed) — exactly what the fleet ran into
/// when only the 10-minute builder advanced DNS2's index.
pub fn set_tip_hash(c: &Connection, hash: &[u8; 32]) -> Result<(), String> {
    c.execute(
        "INSERT INTO meta(k,v) VALUES('tip_hash',?1) \
         ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        [hex32(hash)],
    )
    .map_err(|e| format!("tip_hash: {e}"))?;
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

/// One output resolved by outpoint — the `gettxout` spentness oracle.
/// `spent_height` NULL means the index has never seen the coin spent;
/// a value means it was consumed at that height.
pub struct OutpointRow {
    pub address: String,
    pub value: i64,
    pub height: u64,
    pub spent_height: Option<u64>,
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
        // LIVE connection pragmas: WAL + synchronous=NORMAL. The daemon reads
        // this DB from RPC threads while the accept path writes, and the
        // builder's delete-journal/OFF settings made every reader block behind
        // a writer (and vice versa). WAL is persistent in the DB file; the
        // builder already creates the DB in WAL (create_schema), so this only
        // re-asserts it and sets the per-connection sync level.
        if let Err(e) = conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        ) {
            // not fatal: the index still serves reads; WAL is the concurrency
            // optimization, not a correctness requirement
            eprintln!("[addrindex] live pragmas not applied (wal/normal): {e}");
        }
        let next = index_height(&conn) + 1;
        Some(LiveIndex {
            conn,
            next_height: next,
        })
    }

    /// The authority verdict for this index against the chain's own tip
    /// (facts are read from the open connection; see `authority`).
    pub fn authority_at(
        &self,
        chain_height: u64,
        chain_hash: Option<[u8; 32]>,
    ) -> Authority {
        authority(facts(&self.conn), chain_height, chain_hash)
    }

    /// One output by outpoint — `gettxout`'s spentness oracle when the live
    /// address index covers the chain. Uses the (txid, n) index the spend
    /// marking already relies on.
    pub fn outpoint(&self, txid: &[u8; 32], n: u32) -> Result<Option<OutpointRow>, String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT addr, value, height, spent_height, script \
                 FROM out WHERE txid=?1 AND n=?2",
            )
            .map_err(|e| e.to_string())?;
        let txid_blob = txid.to_vec();
        let mut rows = st
            .query_map(params![txid_blob, n as i64], |r| {
                Ok(OutpointRow {
                    address: r.get(0)?,
                    value: r.get(1)?,
                    height: r.get::<_, i64>(2)? as u64,
                    spent_height: r.get::<_, Option<i64>>(3)?.map(|h| h as u64),
                    script: r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                })
            })
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(row) => row.map_err(|e| e.to_string()).map(Some),
            None => Ok(None),
        }
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
        // Record the hash of the block this index has applied: the height
        // alone does not prove the index is on the SAME chain the node is
        // serving. The hash does.
        let block_hash = crate::legacy::hash9(&block_header_bytes(block));

        // A busy database must NOT be a permanent skip: the caller logs the
        // failure and moves on, so next_height never advances and the index
        // freezes at that height for the rest of the process. With the busy
        // timeout in place a lock is rare; these retries cover the window
        // between the timeout expiring and the builder's batch committing.
        const ATTEMPTS: usize = 3;
        let mut last = String::new();
        for attempt in 1..=ATTEMPTS {
            match self.apply_once(height, &txs, &block_hash) {
                Ok(()) => {
                    self.next_height = height + 1;
                    return Ok(());
                }
                Err(e) => {
                    let retryable = is_busy(&e);
                    last = e;
                    if !retryable || attempt == ATTEMPTS {
                        break;
                    }
                    eprintln!(
                        "[addrindex] h{height} apply hit a lock (attempt {attempt}/{ATTEMPTS}): \
                         {last} — retrying in 250ms"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }
        }
        Err(last)
    }

    /// One all-or-nothing write of a block: every output, spend marking, meta
    /// row and height update in ONE transaction, so a failure leaves the index
    /// exactly as it was (retryable).
    fn apply_once(
        &mut self,
        height: u64,
        txs: &[(Vec<u8>, Vec<(u32, i64, Vec<u8>)>, Vec<(Vec<u8>, u32)>)],
        block_hash: &[u8; 32],
    ) -> Result<(), String> {
        // One transaction, held by rusqlite's RAII guard: ANY error after
        // this point - a failed output insert, a failed meta write, a failed
        // height update - rolls the whole block back. The index never keeps
        // half of a block.
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("begin: {e}"))?;
        insert_block(&tx, height, txs)?;
        tx.execute(
            "INSERT INTO meta(k,v) VALUES('tip_hash',?1) \
             ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [hex32(block_hash)],
        )
        .map_err(|e| format!("tip_hash: {e}"))?;
        finish(&tx, height)?;
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(())
    }
}

/// SQLITE_BUSY / "database is locked" in any of the shapes rusqlite reports.
fn is_busy(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("database is locked") || l.contains("busy") || l.contains("locked")
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

/// How far behind the chain tip an index may trail while still serving as
/// the wallet's whole spendable set. The index's SPENTNESS for every height
/// it has applied is canonical (per-height append, spend-marked in the same
/// transaction), so an index a few blocks behind the tip answers honestly
/// AS OF its coverage height — the tier machinery already labels that
/// coverage on every money-path response. Requiring exact-equality made the
/// verdict practically unreachable: DNS2's index trails the tip 0..~30
/// blocks (the 10-minute catch-up builder plus continuous minting), so the
/// designed tier-1 cure never engaged and the stale tier-2 pair served
/// forever (the 2026-11-17 wallet-ledger defect: 1713 TRI listed, 5329 TRI
/// canonically live). 64 blocks ≈ 16 minutes at the observed mint cadence —
/// comfortably above the builder's cadence, tiny next to the wallet's
/// maturity windows.
pub const MAX_INDEX_LAG: u64 = 64;

/// The money-path rule: same-chain proof AT the index's applied height plus
/// a bounded lag. `chain_hash_at_index_height` is the CHAIN's hash at
/// `facts.coverage_height` (the caller resolves it from its own chain
/// state); when it matches the hash the index recorded for that same
/// height, the index provably applied the chain this node serves, and its
/// unspent set is the wallet's truth as of that height. An index AHEAD of
/// the chain tip, unfinished, on another chain, or with an unresolvable
/// chain hash refuses exactly as before (fail closed).
pub fn authority_with_lag(
    facts: IndexFacts,
    chain_tip_height: u64,
    chain_hash_at_index_height: Option<[u8; 32]>,
    max_lag: u64,
) -> Authority {
    let same_chain = match (facts.index_tip_hash, chain_hash_at_index_height) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    let not_ahead = facts.coverage_height <= chain_tip_height;
    let within_lag = chain_tip_height.saturating_sub(facts.coverage_height) <= max_lag;
    Authority {
        authoritative: facts.complete && same_chain && not_ahead && within_lag,
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

/// The (coverage height, proof hash) pair the catch-up builder stamps at
/// end-of-walk. THE PAIR MUST DESCRIBE THE SAME BLOCK: the lag-aware
/// money-path verdict ([`authority_with_lag`]) resolves the CHAIN's hash at
/// `height` and compares it with the index's recorded tip_hash — pairing the
/// start-of-walk chain tip with a frame accepted beyond it (blocks landed
/// mid-walk), or advancing the height when nothing was accepted, mismatches
/// the proof and tier-1 refuses forever (DNS2 2026-11-17: meta.height
/// 2213928 stamped with h2213929's hash — the wallet's own mint landed
/// mid-walk — so the stale wallet-live window served 1823 TRI while 5368
/// TRI were canonically live).
///
/// When the walk accepted anything, the honest coverage is the LAST
/// accepted block (its height proves its hash, and the content includes
/// everything up to it). When it accepted nothing, the previous
/// (height, hash) pair still stands — return the previous height and no
/// new hash so the caller keeps the previous stamp intact.
pub struct EofStamp {
    /// meta.height: the coverage the content actually proves.
    pub height: u64,
    /// meta.tip_hash: the hash recorded FOR `height` (None: keep whatever
    /// is already stamped — the previous pair is still self-consistent).
    pub tip_hash: Option<[u8; 32]>,
}

pub fn eof_stamp(
    blocks_accepted: u64,
    last_accepted: Option<(u64, [u8; 32])>,
    last_committed: Option<(u64, [u8; 32])>,
    prev_height: u64,
) -> EofStamp {
    match last_accepted {
        Some((h, hash)) if blocks_accepted > 0 => EofStamp {
            height: h,
            tip_hash: Some(hash),
        },
        _ => EofStamp {
            height: last_committed.map(|(h, _)| h).unwrap_or(prev_height),
            tip_hash: last_committed.map(|(_, hash)| hash),
        },
    }
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

    /// THE mid-walk-mint regression (DNS2 2026-11-17): the walk started at
    /// coverage h2213922 with the oracle tip at h2213928; h2213929 (the
    /// wallet's own mint) landed MID-WALK and was accepted via the store
    /// resolution; fewer than 500 blocks meant no slice commit. The old EOF
    /// stamp paired `finish(chain_tip.max(last_height))` (h2213928) with
    /// `set_tip_hash(last_accepted)` (h2213929's hash) — a pair no chain can
    /// prove, so tier-1 refused forever and the stale wallet-live window
    /// (1823 TRI) served while 5368 TRI were canonically live. The stamp
    /// must name the pair the CONTENT proves: (h2213929, its own hash).
    #[test]
    fn eof_stamp_pairs_the_last_accepted_block_with_its_own_height() {
        let h28 = [0x28u8; 32];
        let h29 = [0x29u8; 32];
        let h22 = [0x22u8; 32];
        // accepted h2213929, no slice commit this run, previous coverage
        // h2213922 (start-of-walk chain tip h2213928 is IRRELEVANT to the
        // honest pair — that is the bug)
        let s = eof_stamp(466, Some((2_213_929, h29)), None, 2_213_922);
        assert_eq!(s.height, 2_213_929, "coverage must be the last accepted block's height, not the start-of-walk chain tip");
        assert_eq!(s.tip_hash, Some(h29), "the proof hash must be the hash OF the coverage height");
        // the OLD formula's output, pinned as the thing that must never
        // come back: max(start tip, prev coverage) next to a newer hash
        let old_height = 2_213_928u64.max(2_213_922);
        assert_ne!((old_height, Some(h29)), (s.height, s.tip_hash));
        // and the old pair genuinely fails the verdict while the new one
        // passes (chain hash AT height vs recorded hash)
        let facts_old = IndexFacts { coverage_height: old_height, index_tip_hash: Some(h29), complete: true };
        let facts_new = IndexFacts { coverage_height: s.height, index_tip_hash: s.tip_hash, complete: true };
        let chain_hash_at = |h: u64| -> Option<[u8; 32]> {
            match h { 2_213_928 => Some(h28), 2_213_929 => Some(h29), _ => None }
        };
        assert!(!authority_with_lag(facts_old, 2_213_930, chain_hash_at(old_height), MAX_INDEX_LAG).authoritative,
            "the old mis-paired stamp must refuse");
        assert!(authority_with_lag(facts_new, 2_213_930, chain_hash_at(s.height), MAX_INDEX_LAG).authoritative,
            "the corrected stamp must serve within the lag bound");
    }

    /// A slice commit followed by a tail beyond it: the pair still comes
    /// from the last accepted block, not the last committed slice.
    #[test]
    fn eof_stamp_prefers_the_accepted_tail_over_the_last_slice() {
        let h1 = [0xA1u8; 32];
        let h29 = [0x29u8; 32];
        let s = eof_stamp(700, Some((2_213_929, h29)), Some((2_213_900, h1)), 2_213_899);
        assert_eq!(s.height, 2_213_929);
        assert_eq!(s.tip_hash, Some(h29));
    }

    /// Nothing accepted (resume with nothing new): keep the previous pair
    /// INTACT — never advance the height past the hash it is paired with.
    #[test]
    fn eof_stamp_keeps_the_previous_pair_when_nothing_was_accepted() {
        let s = eof_stamp(0, None, None, 2_213_922);
        assert_eq!(s.height, 2_213_922, "the height must not chase a tip the content does not cover");
        assert_eq!(s.tip_hash, None, "no new hash — the caller keeps the stamped one");
        // the fallback when only the slice-committed pair exists
        let h1 = [0xA1u8; 32];
        let s2 = eof_stamp(0, None, Some((2_213_900, h1)), 2_213_899);
        assert_eq!(s2.height, 2_213_900);
        assert_eq!(s2.tip_hash, Some(h1));
    }

    /// DB-level: the stamped pair reads back through facts()/index_tip_hash
    /// and passes the verdict the daemon runs — the full loop the wallet's
    /// money path takes.
    #[test]
    fn eof_stamp_written_to_the_db_passes_the_lag_aware_verdict() {
        let dir = tmpdir("eofpair");
        let (c, _) = begin(&dir).unwrap();
        let h29 = [0x29u8; 32];
        let s = eof_stamp(466, Some((2_213_929, h29)), None, 2_213_922);
        finish(&c, s.height).unwrap();
        if let Some(h) = s.tip_hash {
            set_tip_hash(&c, &h).unwrap();
        }
        set_complete(&c, true).unwrap();
        let f = facts(&c);
        assert_eq!(f.coverage_height, 2_213_929);
        assert_eq!(f.index_tip_hash, Some(h29));
        assert!(f.complete);
        let verdict = authority_with_lag(f, 2_213_930, Some(h29), MAX_INDEX_LAG);
        assert!(verdict.authoritative);
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

    #[test]
    fn authority_with_lag_serves_a_bounded_lag_on_the_same_chain() {
        let tip_h = [0xAAu8; 32];
        let mut facts = IndexFacts {
            coverage_height: 0,
            index_tip_hash: Some(tip_h),
            complete: true,
        };
        // exact coverage: the old exact rule and the lag rule agree
        facts.coverage_height = 100;
        assert!(authority(facts, 100, Some(tip_h)).authoritative);
        assert!(authority_with_lag(facts, 100, Some(tip_h), MAX_INDEX_LAG).authoritative);
        // lag 6 (DNS2's steady state under the 10-minute catch-up builder):
        // the exact rule refuses, the lag rule serves — same-chain proven AT
        // the index height
        assert!(!authority(facts, 106, Some(tip_h)).authoritative);
        assert!(authority_with_lag(facts, 106, Some(tip_h), MAX_INDEX_LAG).authoritative);
        // lag at the bound passes, one past it refuses
        assert!(authority_with_lag(facts, 100 + MAX_INDEX_LAG, Some(tip_h), MAX_INDEX_LAG)
            .authoritative);
        assert!(
            !authority_with_lag(facts, 100 + MAX_INDEX_LAG + 1, Some(tip_h), MAX_INDEX_LAG)
                .authoritative
        );
        // a DIFFERENT chain hash at the index height refuses at ANY lag (the
        // index applied a fork — its spentness is not this chain's truth)
        assert!(
            !authority_with_lag(facts, 106, Some([0xBBu8; 32]), MAX_INDEX_LAG).authoritative,
            "same-chain proof is mandatory"
        );
        // an unresolvable chain hash refuses (fail closed)
        assert!(!authority_with_lag(facts, 106, None, MAX_INDEX_LAG).authoritative);
        // an index AHEAD of the chain tip refuses (nonsense state)
        assert!(!authority_with_lag(facts, 99, Some(tip_h), MAX_INDEX_LAG).authoritative);
        // an UNFINISHED index refuses even at zero lag
        facts.coverage_height = 100;
        facts.complete = false;
        assert!(!authority_with_lag(facts, 100, Some(tip_h), MAX_INDEX_LAG).authoritative);
    }
}
