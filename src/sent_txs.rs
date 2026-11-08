//! Wallet sent-tx record — the persistence behind legacy `resendtx`
//! (rpcwallet.cpp:1826, `ResendWalletTransactions(true)`).
//!
//! Legacy resends *wallet* transactions that are not in a block yet. tri7's
//! wallet has no per-tx mapWallet: a relayed spend lives only in the
//! in-memory `TxPool`, so a daemon restart forgets every unconfirmed wallet
//! send — exactly the situation legacy resendtx exists to recover from.
//! This module keeps a small JSON record BESIDE the wallet file
//! (`sent_txs.json`, same directory as `history.json`): `relay_built`
//! appends one entry per relayed wallet send, and the `resendtx` verb
//! re-accepts each recorded tx into the pool and re-announces it. Entries
//! whose tx has since been mined are pruned by the resend pass, so the
//! record stays a bounded pending-set, not a log.

use std::path::{Path, PathBuf};

/// Process-global lock that makes the record's read-modify-write sequences
/// atomic: `relay_built`'s append and `resendtx`'s load→save-rewrite both
/// run under it, so a send landing mid-resend cannot be dropped by the
/// survivors rewrite. It is always the FIRST lock taken on any path that
/// touches the record (before pool/wl) — see the lock-order note in
/// follow_tip.rs relay_built/resendtx.
static RECORD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn record_lock() -> std::sync::MutexGuard<'static, ()> {
    RECORD_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Bound the record: the 200 most recent wallet sends. A pending send that
/// never mines keeps its prevouts locked in the live view anyway; the cap
/// only stops an ancient unmined record from growing forever. Entries kept
/// across passes (transient pool rejections) are deliberately given no
/// time-based expiry: each pass costs one pool accept + one store lookup
/// per entry, the cap bounds the set, and the pass prunes an entry the
/// moment its tx mines.
pub const SENT_TXS_CAP: usize = 200;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SentTxEntry {
    pub txid: String,
    /// raw transaction hex (the pool re-parses this on resend)
    pub hex: String,
    /// RFC-3339 UTC when the node first relayed it
    pub first_relayed_utc: String,
}

fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // civil-from-days (Howard Hinnant's algorithm), same approach as
    // rpc.rs fmt_utc_timestamp — no external chrono dependency.
    let days = (now / 86_400) as i64;
    let secs = now % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// `sent_txs.json` lives beside the wallet file (the `history.json` home).
pub fn path_for(wallet_path: &Path) -> PathBuf {
    wallet_path
        .parent()
        .map(|p| p.join("sent_txs.json"))
        .unwrap_or_else(|| PathBuf::from("sent_txs.json"))
}

/// Load the record. Missing file = never sent anything (empty). A corrupt
/// file is an honest error — the caller must not silently resend from a
/// record it cannot read.
pub fn load(path: &Path) -> Result<Vec<SentTxEntry>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        entries: Vec<SentTxEntry>,
    }
    let f: File = serde_json::from_str(&text)
        .map_err(|e| format!("parse {}: {} (move it aside and resend manually)", path.display(), e))?;
    Ok(f.entries)
}

/// Atomic write (tmp + rename, unique tmp name so concurrent sends in one
/// process cannot collide on the tmp path). Public: the `resendtx` verb
/// rewrites the record after pruning mined entries.
pub fn save(path: &Path, entries: &[SentTxEntry]) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension(format!(
        "json.tmp.{}",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("create {}: {}", tmp.display(), e))?;
        let body = serde_json::json!({ "version": 1, "entries": entries });
        f.write_all(serde_json::to_string_pretty(&body).unwrap().as_bytes())
            .map_err(|e| format!("write {}: {}", tmp.display(), e))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {}", tmp.display(), path.display(), e)
    })
}

/// Record one relayed wallet send. Re-recording the same txid (a retry of
/// the same send reaching `relay_built` twice) replaces the old entry
/// instead of duplicating it. Returns the record size after the write.
pub fn record(path: &Path, txid: &str, hex: &str) -> Result<usize, String> {
    let mut entries = load(path)?;
    entries.retain(|e| e.txid != txid);
    entries.push(SentTxEntry {
        txid: txid.to_string(),
        hex: hex.to_string(),
        first_relayed_utc: now_iso(),
    });
    if entries.len() > SENT_TXS_CAP {
        // drop the OLDEST (front of the append order)
        let drop = entries.len() - SENT_TXS_CAP;
        entries.drain(..drop);
    }
    save(path, &entries)?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tri7senttx_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn record_load_roundtrip_and_missing_file_is_empty() {
        let dir = tmp("roundtrip");
        let p = dir.join("sent_txs.json");
        assert!(load(&p).unwrap().is_empty(), "missing file = empty record");
        record(&p, "ab11", "hexa").unwrap();
        record(&p, "cd22", "hexc").unwrap();
        let es = load(&p).unwrap();
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].txid, "ab11");
        assert_eq!(es[1].hex, "hexc");
        assert!(es[0].first_relayed_utc.contains('T') && es[0].first_relayed_utc.ends_with('Z'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rerecording_same_txid_replaces_not_duplicates() {
        let dir = tmp("dedupe");
        let p = dir.join("sent_txs.json");
        record(&p, "ab11", "hexa").unwrap();
        record(&p, "cd22", "hexc").unwrap();
        record(&p, "ab11", "hexa2").unwrap();
        let es = load(&p).unwrap();
        assert_eq!(es.len(), 2, "dedupe by txid");
        let ab = es.iter().find(|e| e.txid == "ab11").unwrap();
        assert_eq!(ab.hex, "hexa2");
        // replaced entry keeps the newest position (append order)
        assert_eq!(es.last().unwrap().txid, "ab11");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_drops_the_oldest_entries() {
        let dir = tmp("cap");
        let p = dir.join("sent_txs.json");
        for i in 0..(SENT_TXS_CAP + 5) {
            record(&p, &format!("tx{i:04}"), "h").unwrap();
        }
        let es = load(&p).unwrap();
        assert_eq!(es.len(), SENT_TXS_CAP);
        assert_eq!(es[0].txid, format!("tx0005"), "oldest 5 dropped");
        assert_eq!(es.last().unwrap().txid, format!("tx{:04}", SENT_TXS_CAP + 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_record_is_an_error_not_a_silent_empty() {
        let dir = tmp("corrupt");
        let p = dir.join("sent_txs.json");
        std::fs::write(&p, "{not json at all").unwrap();
        assert!(load(&p).is_err(), "corrupt record must surface, not read empty");
        // empty file (crash between create and write) reads as empty
        std::fs::write(&p, "").unwrap();
        assert!(load(&p).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_for_sits_beside_the_wallet() {
        let p = path_for(Path::new("/var/lib/tri7/wallet.dat"));
        assert_eq!(p, PathBuf::from("/var/lib/tri7/sent_txs.json"));
        // no parent → cwd-relative honest fallback, never a panic
        let q = path_for(Path::new("wallet.dat"));
        assert!(q.ends_with("sent_txs.json"));
    }
}
