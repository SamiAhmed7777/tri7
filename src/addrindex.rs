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

/// Legacy failure text when the index is absent.
pub const NOT_ENABLED: &str = "Address index not enabled. Start with -addressindex=1";

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
