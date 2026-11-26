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
         CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);
         -- WHICH block the index applied at each height. The coverage height
         -- and its tip hash (meta) prove only the TOP block; an orphaned
         -- block indexed below it left its outputs in place with nothing
         -- recording it (DNS2 2026-11-25, item 52: a re-staked coin counted
         -- twice for hours). One row per applied height, written in the same
         -- transaction as the block's rows.
         CREATE TABLE IF NOT EXISTS blk(height INTEGER PRIMARY KEY, hash BLOB NOT NULL);",
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

/// One transaction of a block as the index stores it:
/// (txid internal, outputs (n, value, script), spends (prev_txid, n)).
pub type BlockTx = (Vec<u8>, Vec<(u32, i64, Vec<u8>)>, Vec<(Vec<u8>, u32)>);
/// One block in the shape every writer hands the index.
pub type BlockTxs = Vec<BlockTx>;

/// What `apply_block` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// the block's outputs and spend markings are in the index
    Applied,
    /// this very block already held the height: nothing was written
    AlreadyApplied,
}

/// The hash of the block the index applied at `height`; None when it never
/// recorded one there (heights indexed before the index kept this table).
pub fn block_at(c: &Connection, height: u64) -> Result<Option<[u8; 32]>, String> {
    let got: Option<Vec<u8>> = c
        .query_row("SELECT hash FROM blk WHERE height=?1", [height as i64], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("blk read h{height}: {e}"))?;
    Ok(got.and_then(|v| {
        let mut h = [0u8; 32];
        if v.len() == 32 {
            h.copy_from_slice(&v);
            Some(h)
        } else {
            None
        }
    }))
}

/// Record which block holds a height. Written with the block's own rows; the
/// repair is the only writer that replaces a row, after removing that block's.
pub fn record_block(c: &Connection, height: u64, hash: &[u8; 32]) -> Result<(), String> {
    c.execute(
        "INSERT OR REPLACE INTO blk(height,hash) VALUES(?1,?2)",
        params![height as i64, &hash[..]],
    )
    .map_err(|e| format!("blk write h{height}: {e}"))?;
    Ok(())
}

/// The lowest height the index recorded a block for (None: none yet).
pub fn first_block_height(c: &Connection) -> Result<Option<u64>, String> {
    let lowest: Option<i64> = c
        .query_row("SELECT MIN(height) FROM blk", [], |r| r.get(0))
        .map_err(|e| format!("blk min: {e}"))?;
    Ok(lowest.map(|h| h as u64))
}

/// Apply one block at one height, exactly once. Two blocks never share a
/// height: when a DIFFERENT block already holds it the call refuses and
/// changes nothing. The caller owns the transaction.
pub fn apply_block(
    c: &Connection,
    height: u64,
    hash: &[u8; 32],
    txs: &[BlockTx],
) -> Result<(Applied, u64, u64), String> {
    match block_at(c, height)? {
        Some(held) if held == *hash => return Ok((Applied::AlreadyApplied, 0, 0)),
        Some(held) => {
            return Err(format!(
                "the address index already holds block {} at this height and refuses to add {} beneath it",
                txid_hex(&held),
                txid_hex(hash)
            ))
        }
        None => {}
    }
    let (outs, spends) = insert_block(c, height, txs)?;
    record_block(c, height, hash)?;
    Ok((Applied::Applied, outs, spends))
}

/// What the repair needs from the canonical chain. Everything is answered by
/// the chain's own view — the tipstate window and the store it threaded — so
/// a test can hand in a synthetic chain.
pub trait CanonicalChain {
    /// the lowest height the chain can vouch for
    fn base(&self) -> u64;
    /// the chain's tip
    fn tip(&self) -> u64;
    /// the chain's block hash (internal order) at `height`
    fn hash_at(&self, height: u64) -> Option<[u8; 32]>;
    /// the canonical block at `height`, in the shape `insert_block` takes
    fn block(&self, height: u64) -> Option<BlockTxs>;
    /// the canonical height of a transaction; Err when the lookup itself
    /// failed (an error is never read as "not canonical")
    fn tx_height(&self, txid: &[u8; 32]) -> Result<Option<u64>, String>;
}

/// What one run of the verify found and did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RepairReport {
    /// every height checked holds the chain's block (nothing was written)
    pub clean: bool,
    /// first height checked
    pub checked_from: u64,
    /// last height checked
    pub checked_to: u64,
    /// first height that held something else (0 when clean)
    pub bad_from: u64,
    /// output rows of blocks that are not canonical: removed
    pub phantom_outputs: u64,
    /// output rows standing at a height that is not their block's: moved
    pub reheighted: u64,
    /// spend markings of blocks that are not canonical: removed or set to
    /// the canonical spend
    pub markings_fixed: u64,
    /// history rows (getaddresstxids) of blocks that are not canonical: removed
    pub tx_rows_removed: u64,
    /// history rows standing at a height that is not their transaction's: moved
    pub tx_rows_moved: u64,
    /// canonical blocks whose rows were missing: written
    pub blocks_reapplied: u64,
}

/// Prove the index against the chain, and repair it where it is wrong.
///
/// The money-path verdict compares the chain's hash with the hash the index
/// recorded AT ITS COVERAGE HEIGHT. That proves the top block only: a block
/// the node minted and lost to a heavier branch was indexed while it was the
/// tip, and every later catch-up skipped its frame and indexed the winning
/// branch above it. The orphan's outputs stayed, unspent, under a coverage
/// height the chain does vouch for — one staked coin counted twice (DNS2,
/// 2026-11-25: 12.608710 TRI).
///
/// So the proof is made per height. Every height the chain can vouch for,
/// from the lowest one the index recorded a block for, must hold the chain's
/// own block. The first height that does not — or that holds no recorded
/// block at all, which means a writer that does not record applied there —
/// starts a repair of every height from there to the coverage height: each
/// output row must stand at a height whose canonical block holds its
/// transaction, each spend marking must be a canonical block's spend, and
/// each canonical block's rows are written where they are missing.
///
/// An index built before the index kept this table (no recorded block at
/// all) is audited the same way, over the whole window — and so is a
/// HALF-recorded one: a `blk` row at the tail does not narrow the audit,
/// because it proves only its own height, not the heights beneath it.
pub fn verify_and_repair(
    c: &mut Connection,
    chain: &dyn CanonicalChain,
) -> Result<RepairReport, String> {
    let coverage = index_height(c);
    let mut rep = RepairReport::default();
    if coverage == 0 {
        rep.clean = true;
        return Ok(rep);
    }
    // The audit range is the chain's whole vouchable window — NEVER clamped
    // down to where `blk` rows begin. A `blk` row proves exactly one height;
    // a half-recorded index (a new writer stamped the tail before the first
    // catch-up repair ran) must not mask the unaudited legacy heights below
    // it. Recorded-ness is a per-height fact the loop below reads through
    // `block_at`; it is not a range selector.
    let from = chain.base();
    let to = coverage.min(chain.tip());
    rep.checked_from = from;
    rep.checked_to = to;
    if from > to {
        rep.clean = true;
        return Ok(rep);
    }
    for h in from..=to {
        let indexed = block_at(c, h)?;
        let bad = match (indexed, chain.hash_at(h)) {
            (None, None) => false, // the chain cannot vouch for this height
            (Some(a), Some(b)) => a != b,
            (None, Some(_)) => true, // a canonical height nothing is recorded for
            (Some(_), None) => false,
        };
        if bad {
            rep.bad_from = h;
            reconcile(c, chain, from, to, &mut rep)?;
            return Ok(rep);
        }
    }
    rep.clean = true;
    Ok(rep)
}

/// Bring every height in `from..=to` in line with the canonical chain, in
/// ONE transaction: a failure leaves the index exactly as it was.
fn reconcile(
    c: &mut Connection,
    chain: &dyn CanonicalChain,
    from: u64,
    to: u64,
    rep: &mut RepairReport,
) -> Result<(), String> {
    use std::collections::{HashMap, HashSet};
    let tx = c.transaction().map_err(|e| format!("repair begin: {e}"))?;
    // Pass 1 — the canonical content of every height in the range.
    let span = (to - from + 1) as usize;
    let mut txids_at: Vec<HashSet<Vec<u8>>> = Vec::with_capacity(span);
    // txid -> the heights its canonical blocks hold it at
    let mut locations: HashMap<Vec<u8>, Vec<u64>> = HashMap::new();
    // outpoint -> the lowest canonical height that spends it in the range
    let mut canonical_spend: HashMap<(Vec<u8>, u32), u64> = HashMap::new();
    for h in from..=to {
        let Some(txs) = chain.block(h) else {
            return Err(format!(
                "the canonical block h{h} cannot be read — not repairing an index against a chain it cannot see"
            ));
        };
        let mut ids = HashSet::with_capacity(txs.len());
        for (txid, _outs, spends) in &txs {
            ids.insert(txid.clone());
            locations.entry(txid.clone()).or_default().push(h);
            for (prev, n) in spends {
                canonical_spend.entry((prev.clone(), *n)).or_insert(h);
            }
        }
        txids_at.push(ids);
    }
    // Pass 2 — every index row the range can touch.
    let mut rows: Vec<(i64, Vec<u8>, i64, i64, Option<i64>)> = Vec::new();
    {
        let mut st = tx
            .prepare(
                "SELECT rowid, txid, n, height, spent_height FROM out \
                 WHERE height BETWEEN ?1 AND ?2 OR spent_height BETWEEN ?1 AND ?2",
            )
            .map_err(|e| format!("repair scan: {e}"))?;
        let got = st
            .query_map(params![from as i64, to as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|e| format!("repair scan: {e}"))?;
        for r in got {
            rows.push(r.map_err(|e| format!("repair scan row: {e}"))?);
        }
    }
    let mut gone: HashSet<i64> = HashSet::new();
    // Where a transaction's canonical block is, when it is not at this height.
    fn elsewhere(
        chain: &dyn CanonicalChain,
        locations: &HashMap<Vec<u8>, Vec<u64>>,
        txid: &[u8],
    ) -> Result<Option<u64>, String> {
        if let Some(hs) = locations.get(txid) {
            return Ok(hs.first().copied());
        }
        let id: [u8; 32] = txid[..]
            .try_into()
            .map_err(|_| "a txid is 32 bytes".to_string())?;
        chain.tx_height(&id)
    }
    // Pass 3 — outputs: a row stands only where a canonical block created it.
    for (rowid, txid, _n, height, _spent) in &rows {
        let h = *height as u64;
        if h < from || h > to || txids_at[(h - from) as usize].contains(txid) {
            continue;
        }
        match elsewhere(chain, &locations, txid)? {
            Some(canonical) => {
                tx.execute("UPDATE out SET height=?2 WHERE rowid=?1", params![rowid, canonical as i64])
                    .map_err(|e| format!("repair out height: {e}"))?;
                tx.execute("UPDATE tx_addr SET height=?2 WHERE txid=?1", params![txid, canonical as i64])
                    .map_err(|e| format!("repair history height: {e}"))?;
                rep.reheighted += 1;
            }
            None => {
                tx.execute("DELETE FROM out WHERE rowid=?1", [rowid])
                    .map_err(|e| format!("repair out delete: {e}"))?;
                gone.insert(*rowid);
                rep.phantom_outputs += 1;
            }
        }
    }
    // Pass 4 — spend markings: only a canonical block's spend marks a coin.
    for (rowid, txid, n, _height, spent) in &rows {
        if gone.contains(rowid) {
            continue;
        }
        let Some(s) = *spent else { continue };
        if s < from as i64 || s > to as i64 {
            continue;
        }
        let want = canonical_spend
            .get(&(txid.clone(), *n as u32))
            .copied();
        if want == Some(s as u64) {
            continue;
        }
        tx.execute(
            "UPDATE out SET spent_height=?2 WHERE rowid=?1",
            params![rowid, want.map(|w| w as i64)],
        )
        .map_err(|e| format!("repair marking: {e}"))?;
        rep.markings_fixed += 1;
    }
    // Pass 5 — history rows (getaddresstxids) name canonical transactions only.
    let mut history: Vec<(i64, Vec<u8>, i64)> = Vec::new();
    {
        let mut st = tx
            .prepare("SELECT rowid, txid, height FROM tx_addr WHERE height BETWEEN ?1 AND ?2")
            .map_err(|e| format!("repair history scan: {e}"))?;
        let got = st
            .query_map(params![from as i64, to as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, i64>(2)?))
            })
            .map_err(|e| format!("repair history scan: {e}"))?;
        for r in got {
            history.push(r.map_err(|e| format!("repair history row: {e}"))?);
        }
    }
    for (rowid, txid, height) in history {
        let h = height as u64;
        if h < from || h > to || txids_at[(h - from) as usize].contains(&txid) {
            continue;
        }
        match elsewhere(chain, &locations, &txid)? {
            Some(canonical) => {
                tx.execute("UPDATE tx_addr SET height=?2 WHERE rowid=?1", params![rowid, canonical as i64])
                    .map_err(|e| format!("repair history move: {e}"))?;
                rep.tx_rows_moved += 1;
            }
            None => {
                tx.execute("DELETE FROM tx_addr WHERE rowid=?1", [rowid])
                    .map_err(|e| format!("repair history delete: {e}"))?;
                rep.tx_rows_removed += 1;
            }
        }
    }
    // Pass 6 — write every canonical block whose rows were not there, and
    // record the height whatever the case.
    for h in from..=to {
        let Some(hash) = chain.hash_at(h) else {
            continue;
        };
        if block_at(&tx, h)? != Some(hash) {
            let Some(txs) = chain.block(h) else {
                return Err(format!("the canonical block h{h} cannot be read"));
            };
            insert_block(&tx, h, &txs)?;
            rep.blocks_reapplied += 1;
        }
        record_block(&tx, h, &hash)?;
    }
    tx.commit().map_err(|e| format!("repair commit: {e}"))
}

/// What one deep repair of a wallet's rows found and did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeepReport {
    /// first height walked
    pub from: u64,
    /// last height walked
    pub to: u64,
    /// the plan was written (false: a dry run — nothing was written)
    pub applied: bool,
    /// addresses in scope
    pub addresses: u64,
    /// output rows the scope held before, and holds after
    pub rows_before: u64,
    pub rows_after: u64,
    /// the scope's spendable set before: (rows, sats)
    pub unspent_before: (u64, i64),
    /// ... and after: measured when applied, projected on a dry run
    pub unspent_after: (u64, i64),
    /// rows whose transaction is on no canonical block: removed
    pub phantom_outputs: u64,
    /// their value
    pub phantom_value: i64,
    /// rows standing at a height that is not their block's: moved
    pub reheighted: u64,
    /// rows whose value is not their output's: corrected
    pub values_fixed: u64,
    /// a spend marking of a block that is not canonical: removed — the
    /// coin is spendable again
    pub markings_cleared: u64,
    /// ... or set to the canonical spend
    pub markings_moved: u64,
    /// a canonical spend the index had not marked: written
    pub markings_set: u64,
    /// value returned to the spendable set by the cleared markings
    pub restored_value: i64,
    /// canonical outputs of the scope the index did not hold: written
    pub missing_outputs: u64,
    /// their value
    pub missing_value: i64,
    /// history rows (getaddresstxids) of transactions that are not
    /// canonical: removed
    pub history_removed: u64,
    /// history rows standing at a height that is not their transaction's:
    /// moved
    pub history_moved: u64,
    /// history rows the index did not hold: written
    pub history_added: u64,
    /// rows, or a row's spend marking, the walk cannot judge: left as they
    /// stood. A marking the walk did not see is never rewritten (though a
    /// height the walk does know is still corrected — that row counts as
    /// moved, not here).
    pub left_alone: u64,
    /// rows that changed after the snapshot: never overwritten
    pub raced: u64,
}

/// Bring one wallet's rows in line with the canonical chain over heights
/// `verify_and_repair` never reaches — below the vouchable window.
///
/// Item 52's repair proves the window the tipstate vouches for. Nothing
/// beneath it was ever checked: an index built before that fix walked the
/// store in FILE order and gave every frame that had a height its rows —
/// the losing branch of a fork included. On DNS2 (2026-11-26) 218 of the
/// wallet's 391 listed rows / 3581.96 TRI were outputs of orphaned blocks,
/// and 204 real coins those orphans had "spent" were missing from the list.
///
/// So this walks the chain the index is judged against — by height, each
/// block the chain's own — from `from` to the index's coverage, and keeps
/// for the addresses in `addrs` only what that walk shows:
///   * an output row stands only where a canonical block created it: a row
///     whose transaction is on no canonical block is removed, one standing
///     at another height is moved;
///   * a coin is spent only where a canonical block spends it: a marking of
///     a block that is not canonical is removed (or set to the real spend),
///     and a canonical spend the index never marked is written;
///   * a canonical output the index never held is written;
///   * history rows (getaddresstxids) name canonical transactions only.
/// Nothing else in the index — other addresses, `blk`, `meta` — is touched.
///
/// A row the walk cannot judge is left alone and counted: one created
/// below `from`, or marked by a block above the walk (the live chain's,
/// not this snapshot's). `from` 0 starts at the wallet's oldest row, which
/// is why that default leaves only the second class. `apply` false is the
/// dry run: the same walk and the same plan, and not one byte written.
/// `intact` is asked once, right before the write: the chain must still be
/// the one that was walked.
pub fn deep_wallet_repair(
    c: &mut Connection,
    chain: &dyn CanonicalChain,
    addrs: &[String],
    from: u64,
    apply: bool,
    intact: &dyn Fn() -> bool,
) -> Result<DeepReport, String> {
    use std::collections::{HashMap, HashSet};
    let mut scope_addrs: Vec<&str> = addrs.iter().map(String::as_str).collect();
    scope_addrs.sort_unstable();
    scope_addrs.dedup();
    let scope: HashSet<&str> = scope_addrs.iter().copied().collect();
    let mut rep = DeepReport {
        from,
        applied: apply,
        addresses: scope.len() as u64,
        ..DeepReport::default()
    };
    let to = index_height(c).min(chain.tip());
    rep.to = to;
    // The scope's rows as they stand, from ONE read snapshot. Read
    // transactions only, in either mode: the dry run leaves no trace.
    c.execute_batch("BEGIN").map_err(|e| format!("deep repair begin: {e}"))?;
    let got = scope_rows(c, &scope_addrs);
    let ended = c.execute_batch("ROLLBACK").map_err(|e| format!("deep repair end read: {e}"));
    let (rows, hist_rows) = got?;
    ended?;
    // `from` 0: the wallet's oldest row is where its coins begin — nothing
    // below it can be one of theirs
    let asked_for = from != 0;
    let from = if from == 0 {
        rows
            .iter()
            .map(|r| r.height)
            .chain(hist_rows.iter().map(|h| h.height))
            .filter(|h| *h > 0)
            .min()
            .map(|h| h as u64)
            .unwrap_or(1)
    } else {
        from
    };
    rep.from = from;
    if rows.is_empty() && hist_rows.is_empty() {
        // nothing of this wallet's in the index: the ordinary build writes
        // it, not this repair — and there is nothing to walk for
        return Ok(rep);
    }
    if from > to {
        if asked_for {
            return Err(format!(
                "the walk would start at h{from}, above where it ends (h{to}) — nothing written"
            ));
        }
        // the wallet's oldest row stands above the index's coverage: nothing
        // of it lies where this repair walks
        return Ok(rep);
    }
    rep.rows_before = rows.len() as u64;
    for r in &rows {
        count_unspent(&mut rep.unspent_before, r.spent, r.value);
    }
    // The walk.
    // outpoint -> the scope addresses whose coin it is
    let mut owners: HashMap<(Vec<u8>, u32), Vec<String>> = HashMap::new();
    for r in &rows {
        let who = owners.entry((r.txid.clone(), r.n as u32)).or_default();
        if !who.contains(&r.addr) {
            who.push(r.addr.clone());
        }
    }
    // canonical outputs of the scope: (height, value, address, script)
    let mut created: HashMap<(Vec<u8>, u32), (u64, i64, String, Vec<u8>)> = HashMap::new();
    // the lowest canonical height that spends a coin of the scope
    let mut spent_at: HashMap<(Vec<u8>, u32), u64> = HashMap::new();
    // (txid, address) -> the lowest canonical height of a transaction that
    // pays the address or spends its coin — what `insert_block` records
    let mut history: HashMap<(Vec<u8>, String), u64> = HashMap::new();
    for h in from..=to {
        let Some(txs) = chain.block(h) else {
            return Err(format!(
                "the canonical block h{h} cannot be read — not repairing an index against a chain it cannot see"
            ));
        };
        for (txid, outs, ins) in &txs {
            for (n, value, script) in outs {
                let Some(dest) = crate::address::script_to_dest(script) else {
                    continue; // OP_RETURN / non-standard: not addressable
                };
                let addr = crate::address::payload_to_address(&dest.payload());
                if !scope.contains(addr.as_str()) {
                    continue;
                }
                let key = (txid.clone(), *n);
                created
                    .entry(key.clone())
                    .or_insert((h, *value, addr.clone(), script.clone()));
                let who = owners.entry(key).or_default();
                if !who.contains(&addr) {
                    who.push(addr.clone());
                }
                history.entry((txid.clone(), addr)).or_insert(h);
            }
            for (prev, n) in ins {
                let key = (prev.clone(), *n);
                let Some(who) = owners.get(&key) else {
                    continue;
                };
                spent_at.entry(key).or_insert(h);
                for a in who {
                    history.entry((txid.clone(), a.clone())).or_insert(h);
                }
            }
        }
    }
    // The plan.
    struct Edit {
        rowid: i64,
        old_height: i64,
        old_value: i64,
        old_spent: Option<i64>,
        height: u64,
        value: i64,
        spent: Option<i64>,
    }
    let mut edits: Vec<Edit> = Vec::new();
    // (rowid, the height and the marking the snapshot saw)
    let mut drops: Vec<(i64, i64, Option<i64>)> = Vec::new();
    let mut adds: Vec<(String, Vec<u8>, i64, i64, u64, Vec<u8>, Option<i64>)> = Vec::new();
    // every row as the plan leaves it: (marking, value)
    let mut final_rows: Vec<(Option<i64>, i64)> = Vec::new();
    let mut rows_after = rows.len() as u64;
    let have: HashSet<(String, Vec<u8>, i64)> = rows
        .iter()
        .map(|r| (r.addr.clone(), r.txid.clone(), r.n))
        .collect();
    for r in &rows {
        let key = (r.txid.clone(), r.n as u32);
        let (mut height, mut value, mut spent) = (r.height as u64, r.value, r.spent);
        // what the walk says the marking is: Some(Some(h)) spent there,
        // Some(None) unspent, None not for this walk to judge
        let want: Option<Option<u64>>;
        // A row above the walk is the live chain's, not this snapshot's.
        // The newest blocks reach the index before the store threads them,
        // and "no canonical block" — the store's answer for a frame it has
        // not threaded — must never be read as "orphan" for such a row.
        if r.height > to as i64 {
            rep.left_alone += 1;
            final_rows.push((spent, value));
            continue;
        }
        match created.get(&key) {
            // created on the walk: its whole life is seen
            Some((ch, cvalue, _, _)) => {
                if *ch != height {
                    rep.reheighted += 1;
                    height = *ch;
                }
                if *cvalue != value {
                    rep.values_fixed += 1;
                    value = *cvalue;
                }
                want = Some(spent_at.get(&key).copied());
            }
            // created below the walk's start: the row says so itself
            None if r.height < from as i64 => {
                rep.left_alone += 1;
                final_rows.push((spent, value));
                continue;
            }
            None => {
                let id: [u8; 32] = r.txid[..]
                    .try_into()
                    .map_err(|_| "a txid is 32 bytes".to_string())?;
                match chain.tx_height(&id)? {
                    // on the walk, yet not an output of this address there
                    Some(ch) if ch >= from => {
                        rep.left_alone += 1;
                        final_rows.push((spent, value));
                        continue;
                    }
                    // canonical, but below the walk's start — the row stood
                    // above it, so its height is wrong at least
                    Some(ch) => {
                        // the height is the transaction's own to set
                        let moved = ch != height;
                        if moved {
                            rep.reheighted += 1;
                            height = ch;
                        }
                        // only a spend the walk saw can be judged; a row
                        // counted as moved is not counted again as left alone
                        match spent_at.get(&key) {
                            Some(s) => want = Some(Some(*s)),
                            None => {
                                if !moved {
                                    rep.left_alone += 1;
                                }
                                want = None;
                            }
                        }
                    }
                    // on no canonical block: the orphan's own output
                    None => {
                        drops.push((r.rowid, r.height, r.spent));
                        rep.phantom_outputs += 1;
                        rep.phantom_value += r.value;
                        rows_after -= 1;
                        continue;
                    }
                }
            }
        }
        if let Some(w) = want {
            // A marking that names a block above the walk belongs to the
            // live chain, not to this snapshot: it is never rewritten. (The
            // row's height or value is still the walk's to correct; such a
            // row is edited with this marking carried through unchanged, and
            // the write stays conditional on it.)
            if r.spent.map_or(false, |s| s as u64 > to) {
                rep.left_alone += 1;
            } else {
                let w = w.map(|x| x as i64);
                if w != r.spent {
                    match (r.spent, w) {
                        (Some(_), None) => {
                            rep.markings_cleared += 1;
                            rep.restored_value += value;
                        }
                        (Some(_), Some(_)) => rep.markings_moved += 1,
                        (None, Some(_)) => rep.markings_set += 1,
                        (None, None) => {}
                    }
                    spent = w;
                }
            }
        }
        if height != r.height as u64 || value != r.value || spent != r.spent {
            edits.push(Edit {
                rowid: r.rowid,
                old_height: r.height,
                old_value: r.value,
                old_spent: r.spent,
                height,
                value,
                spent,
            });
        }
        final_rows.push((spent, value));
    }
    for (key, (ch, cvalue, caddr, cscript)) in &created {
        if have.contains(&(caddr.clone(), key.0.clone(), key.1 as i64)) {
            continue;
        }
        let spent = spent_at.get(key).copied().map(|x| x as i64);
        adds.push((
            caddr.clone(),
            key.0.clone(),
            key.1 as i64,
            *cvalue,
            *ch,
            cscript.clone(),
            spent,
        ));
        rep.missing_outputs += 1;
        rep.missing_value += cvalue;
        rows_after += 1;
        final_rows.push((spent, *cvalue));
    }
    // History rows: a transaction's, at the height the chain holds it.
    let mut hist_drops: Vec<i64> = Vec::new();
    let mut hist_moves: Vec<(i64, u64)> = Vec::new();
    for hrow in &hist_rows {
        match history.get(&(hrow.txid.clone(), hrow.addr.clone())) {
            Some(ch) => {
                if *ch as i64 != hrow.height {
                    rep.history_moved += 1;
                    hist_moves.push((hrow.rowid, *ch));
                }
            }
            None => {
                // above the walk or below its start: not for it to judge
                if hrow.height > to as i64 || hrow.height < from as i64 {
                    rep.left_alone += 1;
                    continue;
                }
                let id: [u8; 32] = hrow.txid[..]
                    .try_into()
                    .map_err(|_| "a txid is 32 bytes".to_string())?;
                match chain.tx_height(&id)? {
                    None => {
                        rep.history_removed += 1;
                        hist_drops.push(hrow.rowid);
                    }
                    Some(ch) => {
                        if ch != hrow.height as u64 {
                            rep.history_moved += 1;
                            hist_moves.push((hrow.rowid, ch));
                        } else {
                            rep.left_alone += 1;
                        }
                    }
                }
            }
        }
    }
    let hist_have: HashSet<(Vec<u8>, String)> = hist_rows
        .iter()
        .map(|h| (h.txid.clone(), h.addr.clone()))
        .collect();
    let mut hist_adds: Vec<(Vec<u8>, String, u64)> = Vec::new();
    for ((txid, addr), ch) in &history {
        if !hist_have.contains(&(txid.clone(), addr.clone())) {
            rep.history_added += 1;
            hist_adds.push((txid.clone(), addr.clone(), *ch));
        }
    }
    rep.rows_after = rows_after;
    let mut projected = (0u64, 0i64);
    for (spent, value) in &final_rows {
        count_unspent(&mut projected, *spent, *value);
    }
    if !apply {
        rep.unspent_after = projected;
        return Ok(rep);
    }
    if !intact() {
        return Err("the chain changed under the walk — nothing written; run again".into());
    }
    // ONE transaction, and every write conditional on the row still being
    // what the snapshot saw: a row the live chain moved meanwhile is left
    // to the window repair, never overwritten.
    let tx = c.transaction().map_err(|e| format!("deep repair write begin: {e}"))?;
    for (rowid, old_height, old_spent) in &drops {
        let changed = tx
            .execute(
                "DELETE FROM out WHERE rowid=?1 AND height=?2 AND spent_height IS ?3",
                params![rowid, old_height, old_spent],
            )
            .map_err(|e| format!("deep repair delete: {e}"))?;
        if changed == 0 {
            rep.raced += 1;
        }
    }
    for e in &edits {
        let changed = tx
            .execute(
                "UPDATE out SET height=?2, value=?3, spent_height=?4 \
                 WHERE rowid=?1 AND height=?5 AND value=?6 AND spent_height IS ?7",
                params![
                    e.rowid,
                    e.height as i64,
                    e.value,
                    e.spent,
                    e.old_height,
                    e.old_value,
                    e.old_spent
                ],
            )
            .map_err(|e| format!("deep repair write: {e}"))?;
        if changed == 0 {
            rep.raced += 1;
        }
    }
    for (addr, txid, n, value, height, script, spent) in &adds {
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO out(addr,txid,n,value,height,script,spent_height) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![addr, txid, *n as i64, value, *height as i64, script, spent],
            )
            .map_err(|e| format!("deep repair insert: {e}"))?;
        if changed == 0 {
            rep.raced += 1;
        }
    }
    for rowid in &hist_drops {
        tx.execute("DELETE FROM tx_addr WHERE rowid=?1", [rowid])
            .map_err(|e| format!("deep repair history delete: {e}"))?;
    }
    for (rowid, height) in &hist_moves {
        tx.execute(
            "UPDATE tx_addr SET height=?2 WHERE rowid=?1",
            params![rowid, *height as i64],
        )
        .map_err(|e| format!("deep repair history move: {e}"))?;
    }
    for (txid, addr, height) in &hist_adds {
        tx.execute(
            "INSERT OR IGNORE INTO tx_addr(txid,addr,height) VALUES(?1,?2,?3)",
            params![txid, addr, *height as i64],
        )
        .map_err(|e| format!("deep repair history insert: {e}"))?;
    }
    tx.commit().map_err(|e| format!("deep repair commit: {e}"))?;
    rep.unspent_after = spendable_set(c, &scope_addrs)?;
    Ok(rep)
}

/// One wallet row as the deep repair reads it.
struct ScopeRow {
    rowid: i64,
    addr: String,
    txid: Vec<u8>,
    n: i64,
    value: i64,
    height: i64,
    spent: Option<i64>,
}

/// One history row (getaddresstxids) as the deep repair reads it.
struct ScopeHistory {
    rowid: i64,
    txid: Vec<u8>,
    addr: String,
    height: i64,
}

fn scope_rows(c: &Connection, addrs: &[&str]) -> Result<(Vec<ScopeRow>, Vec<ScopeHistory>), String> {
    let mut rows: Vec<ScopeRow> = Vec::new();
    let mut hist: Vec<ScopeHistory> = Vec::new();
    for a in addrs {
        let mut st = c
            .prepare("SELECT rowid, txid, n, value, height, spent_height FROM out WHERE addr=?1")
            .map_err(|e| format!("deep repair scan: {e}"))?;
        let got = st
            .query_map(params![a], |r| {
                Ok(ScopeRow {
                    rowid: r.get(0)?,
                    addr: a.to_string(),
                    txid: r.get(1)?,
                    n: r.get(2)?,
                    value: r.get(3)?,
                    height: r.get(4)?,
                    spent: r.get(5)?,
                })
            })
            .map_err(|e| format!("deep repair scan: {e}"))?;
        for r in got {
            rows.push(r.map_err(|e| format!("deep repair scan row: {e}"))?);
        }
        drop(st);
        let mut st = c
            .prepare("SELECT rowid, txid, height FROM tx_addr WHERE addr=?1")
            .map_err(|e| format!("deep repair history scan: {e}"))?;
        let got = st
            .query_map(params![a], |r| {
                Ok(ScopeHistory {
                    rowid: r.get(0)?,
                    txid: r.get(1)?,
                    addr: a.to_string(),
                    height: r.get(2)?,
                })
            })
            .map_err(|e| format!("deep repair history scan: {e}"))?;
        for r in got {
            hist.push(r.map_err(|e| format!("deep repair history row: {e}"))?);
        }
    }
    Ok((rows, hist))
}

fn count_unspent(set: &mut (u64, i64), spent: Option<i64>, value: i64) {
    if spent.is_none() {
        set.0 += 1;
        set.1 += value;
    }
}

/// The scope's spendable set straight from the index: (rows, sats).
fn spendable_set(c: &Connection, addrs: &[&str]) -> Result<(u64, i64), String> {
    let mut set = (0u64, 0i64);
    for a in addrs {
        let (rows, sum): (i64, i64) = c
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(value),0) FROM out WHERE addr=?1 AND spent_height IS NULL",
                params![a],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("deep repair spendable: {e}"))?;
        set.0 += rows as u64;
        set.1 += sum;
    }
    Ok(set)
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
/// If the DB is behind (a gap), the in-order `apply` refuses rather than
/// writing a block out of order, because a missing output row would make a
/// later spend unmarkable and the balance would drift. The catch-up builder
/// closes any such gap — and (item 54 B-i) so does the daemon itself through
/// [`LiveIndex::catch_up_range`] + [`LiveIndex::apply_after_catchup`] when the
/// store holds the canonical bodies in between.
pub struct LiveIndex {
    conn: Connection,
    next_height: u64,
}

/// (item 54 B-i) The most blocks one in-process catch-up applies. DNS2's
/// steady state under the 10-minute builder is a handful of blocks; a gap
/// wider than this means nobody has indexed for a long time and the
/// builder's file-order walk is the right tool, not a loop of store reads.
pub const LIVE_CATCHUP_MAX: u64 = 256;

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

    /// The daemon's per-block write for a caller that applies every block in
    /// order (tests, tools). The daemon itself uses
    /// [`Self::catch_up_range`] + [`Self::apply_after_catchup`].
    pub fn apply(&mut self, block: &crate::legacy::Block, height: u64) -> Result<(), String> {
        self.apply_after_catchup(block, height, &[])
    }

    /// (item 54 B-i) The applied height as the database holds it right now.
    pub fn applied_height(&self) -> u64 {
        stored_height(&self.conn).unwrap_or(0)
    }

    /// (item 54 B-i, 2026-11-26 DNS2) The heights the daemon must supply so
    /// that a block accepted at `height` can be written: every height from
    /// the index's applied height + 1 up to (not including) `height`, read
    /// from the DATABASE and not from the in-memory cursor.
    ///
    /// The cursor was the defect. It was set once at open and advanced only
    /// by this handle's own successful writes, while the catch-up builder —
    /// another process — moved the database underneath it. On DNS2 the
    /// daemon opened with the index at h2219657 and refused every block for
    /// 16+ hours ("index is at h2219657, block is h2220420"), so the index
    /// advanced only when the 10-minute builder ran and listunspent kept
    /// coins spent by the newest blocks listed for minutes (item 53 E2).
    ///
    /// `None` when nothing is to be fetched: the index is current, the
    /// builder is at or past this block's height, or the gap is wider than
    /// [`LIVE_CATCHUP_MAX`] (the builder's walk is the tool for that).
    pub fn catch_up_range(&mut self, height: u64) -> Result<Option<std::ops::Range<u64>>, String> {
        self.next_height = stored_height(&self.conn)? + 1;
        Ok(match height.checked_sub(self.next_height) {
            Some(gap) if gap > 0 && gap <= LIVE_CATCHUP_MAX => Some(self.next_height..height),
            _ => None,
        })
    }

    /// (item 54 B-i) The daemon's per-block write: `block` accepted at
    /// `height`, preceded by `missed` — the chain's blocks for the heights
    /// [`Self::catch_up_range`] named, oldest first.
    ///
    /// The cursor is re-read from the database first, so a builder run
    /// between the two calls can only shrink the gap. The whole gap must be
    /// in hand BEFORE anything is written: a missing body refuses the write
    /// outright and leaves the index exactly as it was (the builder catches
    /// up). Every block still passes the parent check, so a catch-up can
    /// never climb over an orphan the builder has not rewound yet.
    pub fn apply_after_catchup(
        &mut self,
        block: &crate::legacy::Block,
        height: u64,
        missed: &[(u64, crate::legacy::Block)],
    ) -> Result<(), String> {
        self.next_height = stored_height(&self.conn)? + 1;
        if height > self.next_height {
            let from = self.next_height;
            // the gap [from, height) must be covered height by height
            let mut at = from;
            for (h, _) in missed {
                if *h < at {
                    // the builder landed this one in between: nothing to write
                    continue;
                }
                if *h != at {
                    break;
                }
                at += 1;
            }
            if at != height {
                return Err(format!(
                    "index is at h{}, block is h{height} — the chain's body at h{at} is not in hand (builder will catch up)",
                    from - 1
                ));
            }
            // a write that fails part-way keeps the blocks already written
            // (each one canonical and parent-checked); the rest is refused
            for (h, b) in missed.iter().filter(|(h, _)| *h >= from) {
                self.apply_in_order(b, *h)?;
            }
        }
        self.apply_in_order(block, height)
    }

    /// One block, only directly above the index's applied height.
    fn apply_in_order(&mut self, block: &crate::legacy::Block, height: u64) -> Result<(), String> {
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

        // Never write above an orphaned block, and never a second block at
        // a height. The catch-up builder once indexed the node's own mint
        // while it was the tip; the heavier branch that replaced it was then
        // indexed ABOVE it, and the orphan's unspent outputs stayed under a
        // coverage height the chain does vouch for (item 52). The builder
        // now verifies and repairs at the start of every run; this is the
        // same rule for the daemon's own writes: the block the index holds
        // just below must be this block's parent.
        match appendable(&self.conn, height, &block.prev_hash, &block_hash)? {
            Appendable::Yes => {}
            Appendable::AlreadyApplied => {
                // the builder got here first, with this very block
                self.next_height = height + 1;
                return Ok(());
            }
            Appendable::Held(other) => {
                return Err(format!(
                    "index already holds block {} at h{height} — not adding a second one",
                    txid_hex(&other)
                ));
            }
            Appendable::Orphaned { at, held } => {
                return Err(format!(
                    "index holds orphaned block {} at h{at}, not this block's parent — the catch-up builder rewinds it",
                    txid_hex(&held)
                ));
            }
        }

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
        txs: &[BlockTx],
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
        match apply_block(&tx, height, block_hash, txs)? {
            (Applied::Applied, _, _) => {}
            (Applied::AlreadyApplied, _, _) => {
                // checked again under the transaction: another writer landed
                // this very block in between — nothing to add
                tx.commit().map_err(|e| format!("commit: {e}"))?;
                return Ok(());
            }
        }
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

/// (item 54 B-i) The daemon's index write, in two lock windows.
///
/// 1. under the index lock: [`LiveIndex::catch_up_range`] — how far the
///    database is behind the accepted block;
/// 2. with NO index lock held: the chain's bodies for that range, read from
///    the block store (`by_height`). The store has its own lock; the two are
///    never nested here, so no RPC reader that holds one can wait on the
///    other;
/// 3. under the index lock again: [`LiveIndex::apply_after_catchup`] — the
///    gap, then the block, after re-reading the applied height (a builder
///    run in between can only shrink the gap).
///
/// Returns how many blocks below `height` the call wrote, and the write's
/// own result for the caller's log. Without `by_height` (tests, tools) or
/// with nothing to fetch this is exactly the old single-window write.
pub fn apply_live_block<G>(
    lock: &dyn Fn() -> Option<G>,
    block: &crate::legacy::Block,
    height: u64,
    by_height: Option<&dyn Fn(u64) -> Option<crate::legacy::Block>>,
) -> (usize, Result<(), String>)
where
    G: std::ops::DerefMut<Target = Option<LiveIndex>>,
{
    // a height the database will not give is no reason to skip the write:
    // step 3 reads it again and refuses there if a gap remains
    let range = match lock() {
        Some(mut g) => match g.as_mut() {
            Some(ix) => ix.catch_up_range(height).unwrap_or(None),
            None => return (0, Err("live index absent".to_string())),
        },
        None => return (0, Err("live index lock unavailable".to_string())),
    };
    let missed: Vec<(u64, crate::legacy::Block)> = match (range, by_height) {
        (Some(r), Some(f)) if !r.is_empty() => r.filter_map(|h| f(h).map(|b| (h, b))).collect(),
        _ => Vec::new(),
    };
    let Some(mut g) = lock() else {
        return (0, Err("live index lock unavailable".to_string()));
    };
    let Some(ix) = g.as_mut() else {
        return (0, Err("live index absent".to_string()));
    };
    let from = ix.applied_height().saturating_add(1);
    let r = ix.apply_after_catchup(block, height, &missed);
    let caught_up = match &r {
        // only the blocks below `height` that were still missing count
        Ok(()) => missed.iter().filter(|(h, _)| *h >= from).count(),
        Err(_) => 0,
    };
    (caught_up, r)
}

/// Whether a block may be appended at `height`. Read-only; `apply_once`
/// repeats the height check under its own transaction.
enum Appendable {
    Yes,
    /// this very block already holds the height
    AlreadyApplied,
    /// a different block already holds the height
    Held([u8; 32]),
    /// the block just below is not this block's parent: an orphan the
    /// catch-up builder has not rewound yet
    Orphaned { at: u64, held: [u8; 32] },
}

/// (item 54 B-i) The applied height as the DATABASE holds it right now —
/// whoever wrote it, this handle or the catch-up builder — with a read
/// failure reported instead of read as zero (a cursor moved to h0 on a busy
/// read would refuse every block with a misleading height).
fn stored_height(c: &Connection) -> Result<u64, String> {
    c.query_row(
        "SELECT COALESCE(MAX(CAST(v AS INTEGER)),0) FROM meta WHERE k='height'",
        [],
        |r| r.get(0),
    )
    .map_err(|e| format!("index height: {e}"))
}

fn appendable(
    c: &Connection,
    height: u64,
    prev_hash: &[u8; 32],
    block_hash: &[u8; 32],
) -> Result<Appendable, String> {
    match block_at(c, height)? {
        Some(held) if held == *block_hash => return Ok(Appendable::AlreadyApplied),
        Some(held) => return Ok(Appendable::Held(held)),
        None => {}
    }
    if height == 0 {
        return Ok(Appendable::Yes);
    }
    // the block the index holds just below; for heights indexed before the
    // index kept the block table, the stamped coverage pair is the record
    let below = match block_at(c, height - 1)? {
        Some(held) => Some(held),
        None if index_height(c) == height - 1 => index_tip_hash(c),
        None => None,
    };
    match below {
        Some(held) if held == *prev_hash => Ok(Appendable::Yes),
        Some(held) => Ok(Appendable::Orphaned {
            at: height - 1,
            held,
        }),
        None => Ok(Appendable::Yes),
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

    fn tmpdir(tag: &str) -> crate::testsupport::TestDir {
        crate::testsupport::test_dir(&format!("tri7addrindex-{tag}"))
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

    // ---- item 52: an orphaned block indexed beneath a canonical tip ----

    /// A synthetic canonical chain. `set` re-points a height: a reorg.
    struct Chain {
        base: u64,
        blocks: Vec<Option<([u8; 32], BlockTxs)>>,
        /// where the chain's vouchable window begins, when it is not the
        /// first block the test chain holds (None: `base`)
        vouch: Option<u64>,
    }

    impl Chain {
        fn new(base: u64, tip: u64) -> Chain {
            Chain {
                base,
                blocks: vec![None; (tip - base + 1) as usize],
                vouch: None,
            }
        }
        fn set(&mut self, h: u64, hash: [u8; 32], txs: BlockTxs) {
            self.blocks[(h - self.base) as usize] = Some((hash, txs));
        }
        fn txs(&self, h: u64) -> BlockTxs {
            self.blocks[(h - self.base) as usize].as_ref().unwrap().1.clone()
        }
    }

    impl CanonicalChain for Chain {
        fn base(&self) -> u64 {
            self.vouch.unwrap_or(self.base)
        }
        fn tip(&self) -> u64 {
            self.base + self.blocks.len() as u64 - 1
        }
        fn hash_at(&self, height: u64) -> Option<[u8; 32]> {
            self.blocks
                .get(height.checked_sub(self.base)? as usize)?
                .as_ref()
                .map(|b| b.0)
        }
        fn block(&self, height: u64) -> Option<BlockTxs> {
            self.blocks
                .get(height.checked_sub(self.base)? as usize)?
                .as_ref()
                .map(|b| b.1.clone())
        }
        fn tx_height(&self, txid: &[u8; 32]) -> Result<Option<u64>, String> {
            Ok(self
                .blocks
                .iter()
                .position(|b| {
                    b.as_ref()
                        .map(|(_, txs)| txs.iter().any(|(t, _, _)| t == txid))
                        .unwrap_or(false)
                })
                .map(|i| self.base + i as u64))
        }
    }

    fn hash_of(h: u64) -> [u8; 32] {
        crate::legacy::sha256d(&h.to_le_bytes())
    }

    /// A transaction paying `values` to the staker, spending `spends`.
    fn stake_tx(tag: u8, values: &[i64], spends: Vec<([u8; 32], u32)>) -> BlockTx {
        (
            vec![tag; 32],
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (i as u32, *v, p2pkh(0xA1)))
                .collect(),
            spends.into_iter().map(|(p, n)| (p.to_vec(), n)).collect(),
        )
    }

    fn staker() -> String {
        let dest = crate::address::script_to_dest(&p2pkh(0xA1)).expect("p2pkh");
        crate::address::payload_to_address(&dest.payload())
    }

    /// The index's spendable set for the staker: (rows, sum).
    fn spendable(c: &mut Connection) -> (usize, i64) {
        let rows = utxos(c, &[staker()]).expect("utxos");
        (rows.len(), rows.iter().map(|u| u.value).sum())
    }

    /// h1..=h10 each pay the staker 1000. The node then mints h11 on top of
    /// h10, re-staking the h5 coin into 600 + 420. A heavier branch replaces
    /// h11, re-stakes the SAME coin at h12 into 600 + 420 again, and pays the
    /// staker 500 at h11 and at h13. What is really spendable: nine untouched
    /// coins, 500, the re-stake and 500. What the index listed: that, plus
    /// the orphan's two outputs.
    fn scene() -> (Chain, BlockTxs) {
        let mut chain = Chain::new(1, 13);
        for h in 1..=10u64 {
            chain.set(h, hash_of(h), vec![stake_tx(h as u8, &[1000], vec![])]);
        }
        let coin5 = ([5u8; 32], 0u32);
        let mint = vec![stake_tx(0xC1, &[600, 420], vec![coin5])];
        chain.set(11, hash_of(11), vec![stake_tx(0xD0, &[500], vec![])]);
        chain.set(12, hash_of(12), vec![stake_tx(0xD1, &[600, 420], vec![coin5])]);
        chain.set(13, hash_of(13), vec![stake_tx(0xD2, &[500], vec![])]);
        (chain, mint)
    }
    const TRUTH: i64 = 9 * 1000 + 500 + 600 + 420 + 500;
    const DOUBLE: i64 = TRUTH + 600 + 420;

    /// THE regression, on an index built before the index recorded its
    /// blocks (every fleet index at the time). The catch-up indexes the node's
    /// own mint while it is the tip; the heavier branch orphans it and the
    /// coin is re-staked there; the next catch-up skips the stale frame and
    /// indexes the winning branch ABOVE the orphan. The chain vouches for the
    /// coverage height, so the money path serves the index with the orphan's
    /// two outputs still in it — one coin, twice.
    #[test]
    fn orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_legacy_index() {
        let dir = tmpdir("orphan-legacy");
        let (mut c, _) = begin(&dir).expect("begin");
        let (chain, mint) = scene();
        // the catch-up as it ran: rows only, no block recorded
        for h in 1..=10 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        finish(&c, 10).unwrap();
        set_tip_hash(&c, &hash_of(10)).unwrap();
        set_complete(&c, true).unwrap();
        // run 1: the node's own mint IS the tip — indexed
        insert_block(&c, 11, &mint).expect("insert");
        finish(&c, 11).unwrap();
        set_tip_hash(&c, &hash_of(11)).unwrap();
        // run 2, after the heavier branch: the stale frame is skipped and
        // the winning branch is indexed above the orphan
        for h in 11..=13 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        finish(&c, 13).unwrap();
        set_tip_hash(&c, &hash_of(13)).unwrap();
        // the defect, reproduced
        let (facts, rows) = wallet_view(&mut c, &[staker()]).expect("view");
        assert!(authority_with_lag(facts, 13, Some(hash_of(13)), MAX_INDEX_LAG).authoritative);
        assert_eq!(rows.iter().filter(|u| u.txid == [0xC1u8; 32]).count(), 2);
        assert_eq!(rows.iter().map(|u| u.value).sum::<i64>(), DOUBLE);
        // the repair
        let report = verify_and_repair(&mut c, &chain).expect("verify");
        assert_eq!(
            (report.clean, report.checked_from, report.checked_to, report.bad_from),
            (false, 1, 13, 1)
        );
        assert_eq!(
            (report.phantom_outputs, report.markings_fixed, report.tx_rows_removed),
            (2, 1, 1)
        );
        assert_eq!(spendable(&mut c), (13, TRUTH));
        // the re-staked coin is spent by the CANONICAL re-stake, not the orphan
        let coin: (i64, Option<i64>) = c
            .query_row(
                "SELECT height, spent_height FROM out WHERE txid=?1 AND n=0",
                [[5u8; 32].to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(coin, (5, Some(12)));
        for h in 1..=13 {
            assert_eq!(block_at(&c, h).unwrap(), Some(hash_of(h)));
        }
        // nothing left to do
        assert!(verify_and_repair(&mut c, &chain).expect("verify").clean);
        assert_eq!(spendable(&mut c), (13, TRUTH));
    }

    /// The race the range floor must not lose: a new writer records ONLY the
    /// tail block while every height beneath it is legacy (never recorded,
    /// phantoms included). The first recorded height is h13 — the audit must
    /// still start at the chain's base, or the whole legacy window below
    /// h13 goes unaudited and the phantoms survive forever behind one
    /// recorded block.
    #[test]
    fn a_half_recorded_index_is_audited_below_its_first_recorded_block() {
        let dir = tmpdir("orphan-half-recorded");
        let (mut c, _) = begin(&dir).expect("begin");
        let (chain, mint) = scene();
        // the legacy index: rows only, no block recorded anywhere
        for h in 1..=10 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        finish(&c, 10).unwrap();
        set_tip_hash(&c, &hash_of(10)).unwrap();
        set_complete(&c, true).unwrap();
        // run 1: the node's own mint IS the tip — indexed, unrecorded
        insert_block(&c, 11, &mint).expect("insert");
        finish(&c, 11).unwrap();
        set_tip_hash(&c, &hash_of(11)).unwrap();
        // run 2: the winning branch is indexed above the orphan
        for h in 11..=13 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        finish(&c, 13).unwrap();
        set_tip_hash(&c, &hash_of(13)).unwrap();
        // a new writer stamps the tail block only — the index is now
        // half-recorded: first_block_height() is 13, and the naive floor
        // `max(base, first_recorded)` would audit nothing at all
        record_block(&c, 13, &hash_of(13)).expect("record tail");
        // the defect, still present beneath the recorded tail
        let (facts, rows) = wallet_view(&mut c, &[staker()]).expect("view");
        assert!(authority_with_lag(facts, 13, Some(hash_of(13)), MAX_INDEX_LAG).authoritative);
        assert_eq!(rows.iter().filter(|u| u.txid == [0xC1u8; 32]).count(), 2);
        assert_eq!(rows.iter().map(|u| u.value).sum::<i64>(), DOUBLE);
        // the audit reads the whole vouchable window, h1..h13
        let report = verify_and_repair(&mut c, &chain).expect("verify");
        assert_eq!((report.checked_from, report.checked_to, report.bad_from), (1, 13, 1));
        assert_eq!(report.phantom_outputs, 2);
        assert_eq!(spendable(&mut c), (13, TRUTH));
        for h in 1..=13 {
            assert_eq!(block_at(&c, h).unwrap(), Some(hash_of(h)));
        }
        assert!(verify_and_repair(&mut c, &chain).expect("verify").clean);
    }

    /// The same scene once every writer records its block. A writer that
    /// does not check its parent (the daemon before this fix) can still bury
    /// an orphan under canonical blocks; the per-height proof finds it by the
    /// recorded block at h11 — and finds a height nothing is recorded for.
    #[test]
    fn orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_recorded_index() {
        let dir = tmpdir("orphan-recorded");
        let (mut c, _) = begin(&dir).expect("begin");
        let (chain, mint) = scene();
        for h in 1..=10 {
            assert_eq!(
                apply_block(&c, h, &hash_of(h), &chain.txs(h)).unwrap().0,
                Applied::Applied
            );
        }
        finish(&c, 10).unwrap();
        set_tip_hash(&c, &hash_of(10)).unwrap();
        set_complete(&c, true).unwrap();
        // catch-up run 1: the node's own mint IS the tip — indexed, recorded
        apply_block(&c, 11, &[0xC1u8; 32], &mint).unwrap();
        finish(&c, 11).unwrap();
        set_tip_hash(&c, &[0xC1u8; 32]).unwrap();
        // the parentless writer buries it; h12 and h13 are never recorded
        for h in 11..=13 {
            insert_block(&c, h, &chain.txs(h)).unwrap();
        }
        finish(&c, 13).unwrap();
        set_tip_hash(&c, &hash_of(13)).unwrap();
        assert_eq!(spendable(&mut c), (15, DOUBLE));
        let report = verify_and_repair(&mut c, &chain).unwrap();
        assert_eq!(
            (report.clean, report.bad_from, report.phantom_outputs, report.markings_fixed, report.tx_rows_removed),
            (false, 11, 2, 1, 1)
        );
        // h11's canonical rows were missing under the orphan; h12, h13 were never recorded
        assert_eq!(report.blocks_reapplied, 3);
        assert_eq!(spendable(&mut c), (13, TRUTH));
        for h in 1..=13 {
            assert_eq!(block_at(&c, h).unwrap(), Some(hash_of(h)));
        }
        assert!(verify_and_repair(&mut c, &chain).unwrap().clean);
        // a height nothing is recorded for is a height an unrecording writer
        // touched: found, and put right
        c.execute("DELETE FROM blk WHERE height=12", []).unwrap();
        let report = verify_and_repair(&mut c, &chain).unwrap();
        assert_eq!((report.clean, report.bad_from), (false, 12));
        assert_eq!(spendable(&mut c), (13, TRUTH));
        assert!(verify_and_repair(&mut c, &chain).unwrap().clean);
    }

    /// The same transaction in an orphaned block and, later, in a canonical
    /// one at another height: its row was first written at the orphan's
    /// height and the canonical insert was ignored. It IS a canonical output
    /// — moved to its block's height, not deleted.
    #[test]
    fn a_transaction_re_mined_on_the_winning_branch_keeps_its_output() {
        let dir = tmpdir("re-mined");
        let (mut c, _) = begin(&dir).unwrap();
        let mut chain = Chain::new(1, 12);
        for h in 1..=10 {
            chain.set(h, hash_of(h), vec![stake_tx(h as u8, &[1000], vec![])]);
        }
        let again = stake_tx(0xE7, &[700, 300], vec![]);
        chain.set(11, hash_of(11), vec![stake_tx(0xD0, &[500], vec![])]);
        chain.set(12, hash_of(12), vec![again.clone()]);
        for h in 1..=10 {
            insert_block(&c, h, &chain.txs(h)).unwrap();
        }
        insert_block(&c, 11, &vec![again.clone()]).unwrap(); // the orphan, while it was the tip
        for h in 11..=12 {
            insert_block(&c, h, &chain.txs(h)).unwrap(); // h12's insert is ignored: the row exists
        }
        finish(&c, 12).unwrap();
        let row: i64 = c
            .query_row("SELECT height FROM out WHERE txid=?1 AND n=0", [[0xE7u8; 32].to_vec()], |r| r.get(0))
            .unwrap();
        assert_eq!(row, 11);
        let report = verify_and_repair(&mut c, &chain).unwrap();
        assert_eq!((report.phantom_outputs, report.reheighted), (0, 2));
        let row: i64 = c
            .query_row("SELECT height FROM out WHERE txid=?1 AND n=0", [[0xE7u8; 32].to_vec()], |r| r.get(0))
            .unwrap();
        assert_eq!(row, 12);
        assert_eq!(spendable(&mut c), (13, 10 * 1000 + 500 + 1000));
    }

    /// The scene item 53 found on DNS2 below the vouchable window: an index
    /// built in FILE order gave a fork's losing branch its rows. h1..=h10
    /// pay the staker 1000 each. The node's own branch lands first — C1
    /// at "h11" re-stakes coin 5, C2 at "h12" re-stakes coin 6 — and is
    /// indexed while it is the tip. The heavier branch then wins: D0 pays
    /// 500 at h11, D1 re-stakes coin 5 at h12, D2 pays 500 and spends coin
    /// 8 at h13. Only h13 (the window) is vouched for. Spendable in truth:
    /// eight untouched coins, D1's two, and two payments of 500.
    fn deep_scene() -> (Chain, [BlockTx; 2]) {
        let mut chain = Chain::new(1, 13);
        chain.vouch = Some(13);
        for h in 1..=10u64 {
            chain.set(h, hash_of(h), vec![stake_tx(h as u8, &[1000], vec![])]);
        }
        let orphans = [
            stake_tx(0xC1, &[600, 420], vec![([5u8; 32], 0u32)]),
            stake_tx(0xC2, &[700, 300], vec![([6u8; 32], 0u32)]),
        ];
        chain.set(11, hash_of(11), vec![stake_tx(0xD0, &[500], vec![])]);
        chain.set(
            12,
            hash_of(12),
            vec![stake_tx(0xD1, &[600, 420], vec![([5u8; 32], 0u32)])],
        );
        chain.set(
            13,
            hash_of(13),
            vec![stake_tx(0xD2, &[500], vec![([8u8; 32], 0u32)])],
        );
        (chain, orphans)
    }

    /// The index as the file-order catch-up left it: the orphans' rows and
    /// markings, the winner's above them, no block recorded.
    fn legacy_index(dir: &std::path::Path, chain: &Chain, orphans: &[BlockTx]) -> Connection {
        let (mut c, _) = begin(dir).expect("begin");
        for h in 1..=10 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        // the node's own branch, while it is the tip
        insert_block(&c, 11, &orphans[..1]).expect("insert");
        insert_block(&c, 12, &orphans[1..]).expect("insert");
        for h in 11..=13 {
            insert_block(&c, h, &chain.txs(h)).expect("insert");
        }
        finish(&c, 13).unwrap();
        c
    }
    const DEEP_LEGACY: (usize, i64) = (15, 11_040);
    const DEEP_TRUTH: (usize, i64) = (12, 10_020);
    /// the same sets in the report's width
    fn wide(set: (usize, i64)) -> (u64, i64) {
        (set.0 as u64, set.1)
    }

    /// THE regression: the orphans' four outputs are listed and two real
    /// coins they "spent" are hidden, both below the window item 52's
    /// repair can reach. The deep repair removes the phantom outputs, sets
    /// coin 5's marking to the chain's own re-stake and returns coin 6.
    #[test]
    fn deep_repair_drops_a_forks_losing_branch_from_below_the_window() {
        let dir = tmpdir("deep-orphans");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        assert_eq!(spendable(&mut c), DEEP_LEGACY);
        // item 52's repair sees the window only: h13 is put right, the orphans stay
        let window = verify_and_repair(&mut c, &chain).unwrap();
        assert_eq!((window.clean, window.blocks_reapplied), (false, 1));
        assert_eq!(spendable(&mut c), DEEP_LEGACY);

        let intact = || true;
        // the dry run: the same walk, the same plan, nothing written
        let dry = deep_wallet_repair(&mut c, &chain, &[staker()], 1, false, &intact).unwrap();
        assert!(!dry.applied);
        assert_eq!((dry.from, dry.to), (1, 13));
        assert_eq!((dry.phantom_outputs, dry.phantom_value), (4, 2_020));
        assert_eq!(
            (dry.markings_moved, dry.markings_cleared, dry.restored_value),
            (1, 1, 1_000)
        );
        assert_eq!(
            (dry.unspent_before, dry.unspent_after),
            (wide(DEEP_LEGACY), wide(DEEP_TRUTH))
        );
        // eighteen rows stand, three of them spent; the orphans' four go
        assert_eq!((dry.rows_before, dry.rows_after), (18, 14));
        assert_eq!(
            (dry.history_removed, dry.missing_outputs, dry.left_alone, dry.raced),
            (2, 0, 0, 0)
        );
        assert_eq!(spendable(&mut c), DEEP_LEGACY, "a dry run writes nothing");

        let done = deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact).unwrap();
        assert_eq!(done, DeepReport { applied: true, ..dry });
        assert_eq!(spendable(&mut c), DEEP_TRUTH);
        // every row stands where a canonical block created it, and coin 5
        // is spent where the chain spends it
        let coin5: Option<i64> = c
            .query_row(
                "SELECT spent_height FROM out WHERE txid=?1 AND n=0",
                [[5u8; 32].to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(coin5, Some(12));
        let orphans_left: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM out WHERE txid IN (?1, ?2)",
                [[0xC1u8; 32].to_vec(), [0xC2u8; 32].to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans_left, 0);
        let orphan_history: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tx_addr WHERE txid IN (?1, ?2)",
                [[0xC1u8; 32].to_vec(), [0xC2u8; 32].to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_history, 0);
        // again: nothing left to do
        let again = deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact).unwrap();
        assert_eq!(
            (again.unspent_before, again.unspent_after),
            (wide(DEEP_TRUTH), wide(DEEP_TRUTH))
        );
        assert_eq!(
            again.phantom_outputs
                + again.reheighted
                + again.values_fixed
                + again.markings_cleared
                + again.markings_moved
                + again.markings_set
                + again.missing_outputs
                + again.history_removed
                + again.history_moved
                + again.history_added,
            0
        );
    }

    #[test]
    fn deep_repair_writes_what_the_index_never_held() {
        let dir = tmpdir("deep-unheld");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        // the h3 coin was never indexed, and coin 8's canonical spend was
        // never marked
        c.execute("DELETE FROM out WHERE txid=?1", [[3u8; 32].to_vec()])
            .unwrap();
        c.execute("DELETE FROM tx_addr WHERE txid=?1", [[3u8; 32].to_vec()])
            .unwrap();
        c.execute(
            "UPDATE out SET spent_height=NULL WHERE txid=?1",
            [[8u8; 32].to_vec()],
        )
        .unwrap();
        assert_eq!(spendable(&mut c), (15, 11_040));
        let intact = || true;
        let r = deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact).unwrap();
        assert_eq!(
            (r.missing_outputs, r.missing_value, r.markings_set, r.history_added),
            (1, 1_000, 1, 1)
        );
        assert_eq!(spendable(&mut c), DEEP_TRUTH);
    }

    #[test]
    fn deep_repair_writes_nothing_when_the_chain_moved_under_the_walk() {
        let dir = tmpdir("deep-moved");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        let intact = || false;
        let err = deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact)
            .expect_err("must refuse to write over a reorg");
        assert!(err.contains("changed under the walk"), "{err}");
        assert_eq!(spendable(&mut c), DEEP_LEGACY);
    }

    /// Why the walk starts at 1: coins created below its start keep the
    /// markings it cannot judge — here the orphans' spends of coins 5 and 6.
    #[test]
    fn a_walk_that_starts_late_leaves_the_coins_it_cannot_judge() {
        let dir = tmpdir("deep-late");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        let intact = || true;
        // the orphans' outputs stand at h11/h12, on the walk: removed. Coins
        // 5 and 6 were created at h5/h6, below it: their orphan markings
        // stay. Left alone counts the ten coins and their ten history rows.
        let r = deep_wallet_repair(&mut c, &chain, &[staker()], 11, true, &intact).unwrap();
        assert_eq!(
            (r.phantom_outputs, r.markings_cleared, r.markings_moved, r.left_alone),
            (4, 0, 0, 20)
        );
        // the seven coins still listed, and the winner's 2020
        assert_eq!(spendable(&mut c), (11, 9_020));
        // the whole chain is what decides them
        deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact).unwrap();
        assert_eq!(spendable(&mut c), DEEP_TRUTH);
    }

    /// A row above the walk is the live chain's, not this snapshot's: a coin
    /// of a block the walk ends below stays listed, with its history row.
    #[test]
    fn a_row_above_the_walk_is_left_alone() {
        let dir = tmpdir("deep-above");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        insert_block(&c, 14, &[stake_tx(0xE9, &[777], vec![])]).expect("insert");
        assert_eq!(spendable(&mut c), (16, 11_040 + 777));
        let intact = || true;
        let r = deep_wallet_repair(&mut c, &chain, &[staker()], 1, true, &intact).unwrap();
        assert_eq!((r.from, r.to), (1, 13));
        assert_eq!(r.left_alone, 2, "the coin and its history row");
        assert_eq!(r.rows_after, 15);
        assert_eq!(spendable(&mut c), (13, DEEP_TRUTH.1 + 777));
        let fresh: Option<i64> = c
            .query_row(
                "SELECT spent_height FROM out WHERE txid=?1 AND n=0",
                [[0xE9u8; 32].to_vec()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fresh, None, "the coin above the walk is still unspent");
    }

    /// A row can stand ABOVE its transaction's block yet below the walk's
    /// start — a wrong height on a real coin. The height is the transaction's
    /// own to set; the marking the walk did not see is not judged. The row
    /// counts once: as moved, never also as left alone.
    #[test]
    fn a_row_above_its_block_below_the_walks_start_is_moved_not_judged() {
        let dir = tmpdir("deep-misheight");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        // coins 3 and 5 (created h3/h5) claim h9
        for tag in [3u8, 5u8] {
            c.execute(
                "UPDATE out SET height=9 WHERE txid=?1 AND n=0",
                [[tag; 32].to_vec()],
            )
            .unwrap();
        }
        let intact = || true;
        // from h7: both coins are below the start. Coin 3 is never spent on
        // the walk: unmarked still, left so. Coin 5 is spent by D1 at h12: the
        // orphan's h11 marking moves there. Coins 1, 2, 4, 6 and the six
        // history rows of their transactions are left alone.
        let r = deep_wallet_repair(&mut c, &chain, &[staker()], 7, true, &intact).unwrap();
        assert_eq!((r.from, r.to), (7, 13));
        assert_eq!((r.reheighted, r.markings_moved, r.markings_cleared), (2, 1, 0));
        assert_eq!((r.phantom_outputs, r.left_alone), (4, 10));
        for (tag, want) in [(3u8, (3, None)), (5u8, (5, Some(12)))] {
            let got: (i64, Option<i64>) = c
                .query_row(
                    "SELECT height, spent_height FROM out WHERE txid=?1 AND n=0",
                    [[tag; 32].to_vec()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(got, want);
        }
        assert_eq!(spendable(&mut c), (11, 9_020));
    }

    /// A start above where the walk ends is refused when it was asked for,
    /// and is an empty report when the wallet's own rows all stand above the
    /// index's coverage.
    #[test]
    fn a_walk_asked_to_start_above_its_end_is_refused() {
        let dir = tmpdir("deep-empty");
        let (chain, orphans) = deep_scene();
        let mut c = legacy_index(&dir, &chain, &orphans);
        let intact = || true;
        let err = deep_wallet_repair(&mut c, &chain, &[staker()], 14, true, &intact)
            .expect_err("a start above the walk's end");
        assert!(err.contains("above where it ends"), "{err}");
        assert_eq!(spendable(&mut c), DEEP_LEGACY);
        // an index that holds nothing of the wallet's: nothing to walk for
        let empty =
            deep_wallet_repair(&mut c, &chain, &["nobody".to_string()], 0, true, &intact).unwrap();
        assert_eq!((empty.addresses, empty.rows_before, empty.phantom_outputs), (1, 0, 0));
    }

    fn real_block(
        prev: [u8; 32],
        tag: u8,
        values: &[i64],
        spends: Vec<([u8; 32], u32)>,
        script: &[u8],
    ) -> crate::legacy::Block {
        crate::legacy::Block {
            version: 1,
            prev_hash: prev,
            merkle_root: [tag; 32],
            time: 1_700_010_000 + tag as u32,
            bits: 0x1d00_ffff,
            nonce: tag as u32,
            // the tag rides in the tx too, so no two test transactions share bytes
            vtx: vec![crate::legacy::Tx {
                version: 1,
                time: tag as u32,
                vin: spends
                    .iter()
                    .map(|(p, n)| crate::legacy::Vin {
                        prev_hash: *p,
                        prev_n: *n,
                        script_sig: vec![],
                        sequence: 0xffff_ffff,
                    })
                    .collect(),
                vout: values
                    .iter()
                    .map(|v| crate::legacy::Vout {
                        value: *v,
                        script_pubkey: script.to_vec(),
                    })
                    .collect(),
                locktime: 0,
            }],
            block_sig: vec![],
        }
    }

    fn real_txs(block: &crate::legacy::Block) -> BlockTxs {
        block
            .vtx
            .iter()
            .map(|tx| {
                (
                    tx.hash().to_vec(),
                    tx.vout
                        .iter()
                        .enumerate()
                        .map(|(i, o)| (i as u32, o.value, o.script_pubkey.clone()))
                        .collect(),
                    tx.vin
                        .iter()
                        .filter(|v| !v.prev_hash.iter().all(|b| *b == 0))
                        .map(|v| (v.prev_hash.to_vec(), v.prev_n))
                        .collect(),
                )
            })
            .collect()
    }

    /// The daemon's own write (LiveIndex::apply) on the production path,
    /// with real blocks: it never stacks a block on an orphan, and never
    /// writes a second block at a height.
    #[test]
    fn the_daemon_never_writes_above_an_orphan_or_twice_at_one_height() {
        let dir = tmpdir("daemon-guard");
        let (mut c, _) = begin(&dir).expect("begin");
        let script = p2pkh(0xA1);
        let mut blocks = Vec::new();
        let mut prev = hash_of(0);
        for h in 1..=10u64 {
            let b = real_block(prev, h as u8, &[1000], vec![], &script);
            prev = b.hash();
            blocks.push(b);
        }
        let mut chain = Chain::new(1, 13);
        for (i, b) in blocks.iter().enumerate() {
            chain.set(i as u64 + 1, b.hash(), real_txs(b));
        }
        let ten = blocks[9].hash();
        let coin5 = (blocks[4].vtx[0].hash(), 0u32);
        let mint = real_block(ten, 0xC1, &[600, 420], vec![coin5], &script);
        let c11 = real_block(ten, 0xD0, &[500], vec![], &script);
        let c12 = real_block(c11.hash(), 0xD1, &[600, 420], vec![coin5], &script);
        let c13 = real_block(c12.hash(), 0xD2, &[500], vec![], &script);

        let mut live = LiveIndex::open_if_built(&dir).expect("live index");
        for (i, b) in blocks.iter().enumerate() {
            live.apply(b, i as u64 + 1).expect("the daemon appends in order");
        }
        assert_eq!(spendable(&mut c), (10, 10_000));
        live.apply(&mint, 11).expect("the node's own mint is the tip: indexed");
        assert_eq!(spendable(&mut c), (11, 9 * 1000 + 1020));

        // the heavier branch
        chain.set(11, c11.hash(), real_txs(&c11));
        chain.set(12, c12.hash(), real_txs(&c12));
        chain.set(13, c13.hash(), real_txs(&c13));
        // h11 holds the mint, which is not h12's parent: refused, nothing written
        let err = live.apply(&c12, 12).expect_err("must refuse to write above an orphan");
        assert!(err.contains("orphaned"), "{err}");
        assert_eq!(index_height(&c), 11);
        assert_eq!(block_at(&c, 12).unwrap(), None);
        assert_eq!(spendable(&mut c), (11, 9 * 1000 + 1020));

        // the catch-up builder rewinds
        let report = verify_and_repair(&mut c, &chain).unwrap();
        assert_eq!((report.clean, report.phantom_outputs), (false, 2));
        // at h11 the canonical chain has not re-staked the coin yet: it is
        // spendable again, exactly as the chain stands
        assert_eq!(spendable(&mut c), (11, 10 * 1000 + 500));
        // a fresh handle appends on the repaired index
        let mut live = LiveIndex::open_if_built(&dir).expect("live index");
        live.apply(&c12, 12).expect("h11 is the chain's block now, and this block's parent");
        assert_eq!(spendable(&mut c), (12, 9 * 1000 + 500 + 1020));
        // the builder got h13 first: the daemon's write is a no-op, not a copy
        apply_block(&c, 13, &c13.hash(), &real_txs(&c13)).unwrap();
        live.apply(&c13, 13).expect("this very block already holds the height");
        assert_eq!(spendable(&mut c), (13, TRUTH));
    }

    // ---- item 54 B-i: the daemon's live index follows the chain -----------

    /// A base of ten blocks written by the daemon's own handle (each pays the
    /// staker 1000), then the chain above it: D0 pays 500 at h11, D1
    /// re-stakes coin 5 at h12, D2 pays 500 at h13, D3 re-stakes coin 7 at
    /// h14. Fully indexed: eight untouched coins + 500 + 600 + 420 + 500 + 700.
    struct LagScene {
        dir: crate::testsupport::TestDir,
        /// the daemon's handle, behind the same lock shape the daemon uses
        cell: std::sync::Mutex<Option<LiveIndex>>,
        c: Connection,
        /// index 0 is h1 … index 13 is h14
        chain: Vec<crate::legacy::Block>,
    }
    const LAG_TRUTH: (usize, i64) = (13, 10_720);

    fn lag_scene(tag: &str) -> LagScene {
        let dir = tmpdir(tag);
        let (mut c, _) = begin(dir.path()).expect("begin");
        let script = p2pkh(0xA1);
        let mut chain = Vec::new();
        let mut prev = hash_of(0);
        for h in 1..=10u64 {
            let b = real_block(prev, h as u8, &[1000], vec![], &script);
            prev = b.hash();
            chain.push(b);
        }
        let ten = prev;
        let coin5 = (chain[4].vtx[0].hash(), 0u32);
        let coin7 = (chain[6].vtx[0].hash(), 0u32);
        let c11 = real_block(ten, 0xD0, &[500], vec![], &script);
        let c12 = real_block(c11.hash(), 0xD1, &[600, 420], vec![coin5], &script);
        let c13 = real_block(c12.hash(), 0xD2, &[500], vec![], &script);
        let c14 = real_block(c13.hash(), 0xD3, &[700], vec![coin7], &script);
        chain.extend([c11, c12, c13, c14]);
        let mut live = LiveIndex::open_if_built(dir.path()).expect("live index");
        for (i, b) in chain.iter().take(10).enumerate() {
            live.apply(b, i as u64 + 1).expect("the daemon appends in order");
        }
        assert_eq!(spendable(&mut c), (10, 10_000));
        LagScene {
            dir,
            cell: std::sync::Mutex::new(Some(live)),
            c,
            chain,
        }
    }

    /// The daemon's accept-path write for the chain's block at `height`.
    fn daemon_write(
        s: &LagScene,
        height: u64,
        by_height: Option<&dyn Fn(u64) -> Option<crate::legacy::Block>>,
    ) -> (usize, Result<(), String>) {
        apply_live_block(
            &|| s.cell.lock().ok(),
            &s.chain[(height - 1) as usize],
            height,
            by_height,
        )
    }

    /// The catch-up builder's write, as another process would make it.
    fn builder_write(c: &Connection, chain: &[crate::legacy::Block], from: u64, to: u64) {
        for h in from..=to {
            let b = &chain[(h - 1) as usize];
            apply_block(c, h, &b.hash(), &real_txs(b)).expect("builder insert");
        }
        finish(c, to).expect("finish");
    }

    /// The chain's body at a height, as the store serves it.
    fn store_of(chain: Vec<crate::legacy::Block>) -> impl Fn(u64) -> Option<crate::legacy::Block> {
        move |h: u64| chain.get(h as usize - 1).cloned()
    }

    fn assert_fully_indexed(s: &mut LagScene) {
        assert_eq!(index_height(&s.c), 14);
        for h in 11..=14 {
            assert_eq!(block_at(&s.c, h).unwrap(), Some(s.chain[(h - 1) as usize].hash()));
        }
        assert_eq!(spendable(&mut s.c), LAG_TRUTH);
    }

    /// (item 54 B-i) THE DEFECT, DNS2 2026-11-26: the catch-up builder is
    /// another process. It moved the database to h13 while the daemon's
    /// handle still stood at h11 — and the handle's cursor was advanced only
    /// by its own writes, so every later block was refused ("index is at
    /// h10, block is h14") for the life of the process and the index moved
    /// only when the 10-minute builder ran. The cursor now follows the
    /// database: the very next block is written.
    #[test]
    fn a_stale_daemon_cursor_follows_the_builder_and_keeps_writing() {
        let mut s = lag_scene("lag-stale-cursor");
        builder_write(&s.c, &s.chain, 11, 13);
        assert_eq!(index_height(&s.c), 13);
        {
            let g = s.cell.lock().unwrap();
            assert_eq!(g.as_ref().unwrap().next_height, 11, "the handle still believes h11 is next");
        }
        let (caught_up, r) = daemon_write(&s, 14, None);
        r.expect("the next block is written, not refused");
        assert_eq!(caught_up, 0, "the builder had already written the gap");
        assert_fully_indexed(&mut s);
        // and the same handle keeps writing on its own from here
        let c15 = real_block(s.chain[13].hash(), 0xD4, &[800], vec![], &p2pkh(0xA1));
        let (caught_up, r) = apply_live_block(&|| s.cell.lock().ok(), &c15, 15, None);
        r.expect("the handle writes the next block itself");
        assert_eq!((caught_up, index_height(&s.c)), (0, 15));
    }

    /// (item 54 B-i) The daemon missed h11..=h13 — nobody indexed them. With
    /// the chain's bodies from the store it closes the gap itself, in order,
    /// in the same write as the block it just accepted: coin 5 (spent at
    /// h12) and coin 7 (spent at h14) both leave the spendable set AT ONCE.
    /// That is the freshness lag of item 53 E2 — rows spent by the newest
    /// blocks stayed listed until the builder's next run, minutes later.
    #[test]
    fn a_daemon_that_missed_blocks_catches_up_from_the_store_in_process() {
        let mut s = lag_scene("lag-catch-up");
        let by_height = store_of(s.chain.clone());
        let (caught_up, r) = daemon_write(&s, 14, Some(&by_height));
        r.expect("gap + block are written");
        assert_eq!(caught_up, 3);
        assert_fully_indexed(&mut s);
        let c: (i64, Option<i64>) = s
            .c
            .query_row(
                "SELECT height, spent_height FROM out WHERE txid=?1 AND n=0",
                [s.chain[4].vtx[0].hash().to_vec()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(c, (5, Some(12)), "coin 5 is marked spent by the catch-up's h12");
    }

    /// A gap is climbed only with every body in hand: one missing refuses
    /// the whole write — not even the blocks that WERE in hand are written,
    /// so the index is exactly as it was for the builder's walk.
    #[test]
    fn a_catch_up_without_every_body_writes_nothing() {
        let mut s = lag_scene("lag-missing-body");
        let chain = s.chain.clone();
        let by_height = move |h: u64| {
            if h == 12 {
                None
            } else {
                chain.get(h as usize - 1).cloned()
            }
        };
        let (caught_up, r) = daemon_write(&s, 14, Some(&by_height));
        let e = r.expect_err("a gap is not climbed on a missing body");
        assert!(e.contains("h12") && e.contains("builder will catch up"), "{e}");
        assert_eq!(caught_up, 0);
        assert_eq!(index_height(&s.c), 10);
        assert_eq!(block_at(&s.c, 11).unwrap(), None, "h11 was in hand and still not written");
        assert_eq!(spendable(&mut s.c), (10, 10_000));
    }

    /// A builder run landing while the store is read (between the two lock
    /// windows) only shrinks the gap: its blocks are left alone, the rest
    /// are written, and the block the daemon accepted is written once.
    #[test]
    fn a_builder_run_between_the_lock_windows_only_shrinks_the_gap() {
        let mut s = lag_scene("lag-builder-races");
        let chain = s.chain.clone();
        let builder = open(&db_path(s.dir.path())).expect("builder connection");
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let asked_in = asked.clone();
        let by_height = move |h: u64| {
            asked_in.borrow_mut().push(h);
            if asked_in.borrow().len() == 1 {
                // the 10-minute run lands now
                builder_write(&builder, &chain, 11, 12);
            }
            chain.get(h as usize - 1).cloned()
        };
        let (caught_up, r) = daemon_write(&s, 14, Some(&by_height));
        r.expect("written");
        assert_eq!(*asked.borrow(), vec![11, 12, 13]);
        assert_eq!(caught_up, 1, "h13 only: the builder wrote h11 and h12");
        assert_fully_indexed(&mut s);
    }

    /// Lock discipline: exactly two index-lock windows, and the store is
    /// read in neither — the store has its own lock, and the two are never
    /// nested (no RPC reader holding one may wait on the other).
    #[test]
    fn the_store_is_read_outside_the_index_lock() {
        let s = lag_scene("lag-lock-windows");
        let chain = s.chain.clone();
        let locks = std::sync::atomic::AtomicUsize::new(0);
        let read_under_lock = std::sync::atomic::AtomicBool::new(false);
        let cell = &s.cell;
        let lock = || {
            locks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            cell.lock().ok()
        };
        let by_height = |h: u64| {
            if cell.try_lock().is_err() {
                read_under_lock.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            chain.get(h as usize - 1).cloned()
        };
        let (caught_up, r) = apply_live_block(&lock, &s.chain[13], 14, Some(&by_height));
        r.expect("written");
        assert_eq!(caught_up, 3);
        assert_eq!(locks.load(std::sync::atomic::Ordering::SeqCst), 2, "two lock windows");
        assert!(
            !read_under_lock.load(std::sync::atomic::Ordering::SeqCst),
            "the store was read under the index lock"
        );
    }

    /// The bound: a gap AT the limit is climbed; one block more is left to
    /// the builder's walk, and nothing is written in that case.
    #[test]
    fn a_gap_wider_than_the_bound_is_left_to_the_builder() {
        let script = p2pkh(0xA1);
        let mut chain = Vec::new();
        let mut prev = hash_of(0);
        // tag + value together keep every transaction's bytes distinct
        for h in 1..=(10 + LIVE_CATCHUP_MAX + 2) {
            let b = real_block(
                prev,
                (h % 250) as u8 + 1,
                &[(h / 250) as i64 + 1],
                vec![],
                &script,
            );
            prev = b.hash();
            chain.push(b);
        }
        // a handle at h10 that missed the whole gap
        let mut scene = |tag: &str| {
            let dir = tmpdir(tag);
            let (mut c, _) = begin(dir.path()).expect("begin");
            let mut live = LiveIndex::open_if_built(dir.path()).expect("live index");
            for (i, b) in chain.iter().take(10).enumerate() {
                live.apply(b, i as u64 + 1).expect("in order");
            }
            (dir, c, std::sync::Mutex::new(Some(live)))
        };
        let by_height = store_of(chain.clone());
        // the bound is a count of MISSING blocks: h11..=h267 under a block
        // accepted at h268 is LIVE_CATCHUP_MAX of them — climbed
        let at = 10 + LIVE_CATCHUP_MAX + 1;
        let (_d, c, cell) = scene("lag-bound-climbed");
        let (caught_up, r) =
            apply_live_block(&|| cell.lock().ok(), &chain[(at - 1) as usize], at, Some(&by_height));
        r.expect("a gap at the bound is climbed");
        assert_eq!((caught_up as u64, index_height(&c)), (LIVE_CATCHUP_MAX, at));
        // one missing block more
        let beyond = at + 1;
        let (_d2, c2, cell2) = scene("lag-bound-refused");
        let (caught_up, r) = apply_live_block(
            &|| cell2.lock().ok(),
            &chain[(beyond - 1) as usize],
            beyond,
            Some(&by_height),
        );
        let e = r.expect_err("one block past the bound is the builder's");
        assert!(e.contains("builder will catch up"), "{e}");
        assert_eq!(caught_up, 0);
        assert_eq!(index_height(&c2), 10, "nothing written");
    }

    /// The gap is still the daemon's own write path: a catch-up can never
    /// climb over an orphan the builder has not rewound. The index holds an
    /// orphan at h11 (the node's own mint, while it was the tip); the
    /// canonical chain's h12 is refused exactly as before.
    #[test]
    fn a_catch_up_never_climbs_over_an_orphan() {
        let mut s = lag_scene("lag-orphan");
        let script = p2pkh(0xA1);
        let ten = s.chain[9].hash();
        let mint = real_block(ten, 0xC1, &[600, 420], vec![(s.chain[4].vtx[0].hash(), 0u32)], &script);
        {
            let mut g = s.cell.lock().unwrap();
            g.as_mut().unwrap().apply(&mint, 11).expect("the mint, while it is the tip");
        }
        // the builder rewound nothing; the node accepted the canonical h13
        // whose parent (h12) the index does not hold
        let by_height = store_of(s.chain.clone());
        let (caught_up, r) = daemon_write(&s, 13, Some(&by_height));
        let e = r.expect_err("never above an orphan");
        assert!(e.contains("orphaned") || e.contains("not adding a second one"), "{e}");
        assert_eq!(caught_up, 0);
        assert_eq!(index_height(&s.c), 11);
        assert_eq!(block_at(&s.c, 12).unwrap(), None, "nothing written above the orphan");
        // the builder repairs; the daemon's SAME handle (no restart) then
        // closes what the repair rewound and writes the next block — the
        // cursor follows the database
        let mut chain = Chain::new(1, 14);
        for (i, b) in s.chain.iter().enumerate() {
            chain.set(i as u64 + 1, b.hash(), real_txs(b));
        }
        let report = verify_and_repair(&mut s.c, &chain).expect("verify");
        assert!(!report.clean, "the orphan is found");
        // the repair is an in-place reconcile, not a rewind: the orphan's
        // height now holds the canonical block and the coverage stands
        assert_eq!(index_height(&s.c), 11);
        assert_eq!(block_at(&s.c, 11).unwrap(), Some(s.chain[10].hash()));
        // the daemon's SAME handle (no restart) then closes what the repair
        // did not reach and writes the next block — the cursor follows the
        // database
        let (caught_up, r) = daemon_write(&s, 14, Some(&by_height));
        r.expect("the repaired index is appended to by the old handle");
        assert_eq!(caught_up, 2, "h12 and h13: the repair had already written h11");
        assert_fully_indexed(&mut s);
    }
}
