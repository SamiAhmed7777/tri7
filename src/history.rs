//! Full-chain wallet history.
//!
//! The live view only covers the rolling window (a few thousand blocks) — it
//! answers "what do I hold right now". A holder who has been in this chain
//! since 2014 needs the other question: "what did I ever receive and spend,
//! and when". That requires walking the block store from genesis (or from a
//! cached checkpoint) and matching every output/input against the wallet keys.
//!
//! Design notes:
//! - One sequential pass over the store. Frames are appended in chain order, so
//!   a single seek-and-read loop is far faster than 2.2M random lookups.
//! - Height comes from the sequential counter; the cache records the last
//!   scanned height AND byte offset so the next run only reads the tail.
//! - Rows are per wallet *event* (received output / spent transaction), dated
//!   from the containing block's nTime — the same stamp the legacy client
//!   prints, which is why its dates and ours agree.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::legacy::{Block, Reader};
use crate::wallet::WalletKeys;

const MAGIC: u32 = 0x0522_3570;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRec {
    /// display-order hex
    pub txid: String,
    pub height: u64,
    /// block nTime (unix seconds) — what the legacy client shows
    pub time: u32,
    /// signed sats: positive = received, negative = sent
    pub value_sats: i64,
    pub address: String,
    /// "in" | "out"
    pub direction: String,
    /// "regular" | "coinbase" | "coinstake"
    pub kind: String,
    /// true when the received output has been spent since
    pub spent: bool,
    /// legacy sub-row index within its transaction (transactionrecord.h idx)
    #[serde(default)]
    pub idx: u32,
    /// legacy kind label key: "recv_with" | "recv_from" | "send" | "self" | "mined"
    #[serde(default)]
    pub legacy_kind: String,
    /// produced by a proof-of-stake block (coinstake output)
    #[serde(default)]
    pub coinstake: bool,
    /// for a coinstake: the stake principal returned in the same transaction.
    /// wallet.cpp GetStake() sums the WHOLE transaction value, so the Stake
    /// row needs principal + reward, not the reward alone.
    #[serde(default)]
    pub stake_principal: i64,
    /// OUR INPUT ADDRESSES when this tx spent our coins (the txids whose
    /// prevouts the generator resolved from its own rows map). Empty = the tx
    /// did not spend our coins, OR the row predates this field (older
    /// history.json files — listaddressgroupings then contributes output
    /// singletons only for that tx; a rescan completes the input side).
    #[serde(default)]
    pub input_addresses: Vec<String>,
    /// legacy transactionrecord.cpp decomposition for a stake:
    /// credit = nNet = GetCredit() - GetDebit() — the REWARD only, not the
    /// whole stake output (one row per coinstake, subsequent our-outputs
    /// skipped by the hashPrev guard). recv_total - spent_total here.
    /// None on rows written before this field existed and on non-stake rows
    /// (render falls back to the raw output value; a real net is > 0, and
    /// Some distinguishes a known net from an unknown one).
    #[serde(default)]
    pub stake_net: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryCache {
    pub scanned_height: u64,
    pub scanned_offset: u64,
    pub rows: Vec<HistoryRec>,
}

pub fn cache_path(state_dir: &Path) -> PathBuf {
    state_dir.join("history.json")
}

pub fn load_cache(state_dir: &Path) -> Option<HistoryCache> {
    let raw = std::fs::read_to_string(cache_path(state_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn dedupe(rows: &mut Vec<HistoryRec>) {
    // Durations are checked from these rows; a duplicated receive inflates the
    // balance forever. Key on (txid, value, height, direction, address).
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| {
        seen.insert((
            r.txid.clone(),
            r.value_sats,
            r.height,
            r.direction.clone(),
            r.address.clone(),
        ))
    });
    rows.sort_by(|a, b| a.height.cmp(&b.height).then(a.txid.cmp(&b.txid)));
}

pub fn save_cache(state_dir: &Path, cache: &HistoryCache) -> Result<(), String> {
    let mut cache = cache.clone();
    dedupe(&mut cache.rows);
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("mkdir {}: {}", state_dir.display(), e))?;
    // A scan that starts at genesis checkpoints an EMPTY cache immediately.
    // If that run is killed, the empty file used to replace real history and
    // the wallet silently showed zero transactions forever (seen on sami-pc:
    // 29k rows replaced by {"scanned_height":0,"rows":[]}).
    if cache.rows.is_empty() && cache.scanned_height == 0 {
        if let Ok(old_raw) = std::fs::read_to_string(cache_path(state_dir)) {
            if let Ok(old) = serde_json::from_str::<HistoryCache>(&old_raw) {
                if !old.rows.is_empty() {
                    return Ok(());
                }
            }
        }
    }
    let tmp = state_dir.join("history.json.tmp");
    let raw = serde_json::to_string(&cache).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, raw).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, cache_path(state_dir))
        .map_err(|e| format!("rename history cache: {}", e))
}

fn display_hex(h: &[u8; 32]) -> String {
    let mut r = *h;
    r.reverse();
    r.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Payload -> address for every wallet key (P2PKH version+hash160).
fn our_payloads(wallet: &WalletKeys) -> (HashMap<[u8; 21], String>, HashMap<Vec<u8>, String>) {
    let mut by_payload = HashMap::new();
    let mut by_pubkey = HashMap::new();
    for k in &wallet.keys {
        let p = crate::address::pubkey_payload(&k.pubkey);
        let addr = crate::address::payload_to_address(&p);
        by_payload.insert(p, addr.clone());
        by_pubkey.insert(k.pubkey.clone(), addr);
    }
    (by_payload, by_pubkey)
}

/// Which of our addresses (if any) does this output script pay?
fn out_owner(
    script: &[u8],
    by_payload: &HashMap<[u8; 21], String>,
    by_pubkey: &HashMap<Vec<u8>, String>,
) -> Option<String> {
    if let Some(d) = crate::address::script_to_dest(script) {
        if let Some(a) = by_payload.get(&d.payload()) {
            return Some(a.clone());
        }
    }
    // P2PK (early coinbase outputs on this chain pay a bare pubkey)
    if script.len() == 35 && script[0] == 33 && script[34] == 0xac {
        if let Some(a) = by_pubkey.get(&script[1..34].to_vec()) {
            return Some(a.clone());
        }
    }
    None
}

/// Scan the store and produce history rows. `from` is the first height to
/// scan (cache height + 1 for incremental runs); `offset_hint` is the byte
/// offset of that height's frame when the cache knows it.
pub fn scan(
    store_path: &Path,
    state_dir: &Path,
    wallet: &WalletKeys,
    from: u64,
    offset_hint: Option<u64>,
) -> Result<HistoryCache, String> {
    let (by_payload, by_pubkey) = our_payloads(wallet);
    let mut cache = if from > 0 {
        load_cache(state_dir).unwrap_or_default()
    } else {
        HistoryCache::default()
    };
    // rows index: outpoint -> row position (to mark spends later)
    let mut owned: HashMap<([u8; 32], u32), usize> = HashMap::new();
    for (i, r) in cache.rows.iter().enumerate() {
        if r.direction == "in" && !r.spent {
            if let Ok(bytes) = hex_to_internal(&r.txid) {
                // n is not stored in the row; recovery of spends uses txid only
                for n in 0..4u32 {
                    owned.insert((bytes, n), i);
                }
            }
        }
    }

    // Height map from the store's own index. The frame order in the .dat file
    // is NOT chain order (reorg leftovers are appended above the threaded
    // tip), so a plain frame counter after genesis mislabels every row that
    // follows a gap. The index knows the real height of each block hash.
    let store = crate::blockindex::BlockStore::new(Some(store_path.to_path_buf()), None);
    // Oracle from tipstate: without the window hashes the thread stops at the
    // first reorg leftover (3,600+ blocks short of the tip), which silently
    // truncated history and made the dates regress. With the oracle the map
    // reaches the window base and everything below it is authoritative.
    let oracle = load_oracle(state_dir);
    store.spawn_build(oracle.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    let mut threaded_tip = None;
    while std::time::Instant::now() < deadline {
        if let crate::blockindex::StoreStatus::Ready {
            threaded_tip: t, ..
        } = store.status()
        {
            threaded_tip = t;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    if let Some(tip) = threaded_tip {
        eprintln!("history: store index ready, threaded tip h{}", tip);
    }

    // One-time migration (2026-11-09 incident): rows written before the
    // stake_net field existed carry None, and the GUI then rendered every
    // stake roll as "Mined +<principal> TRI" instead of legacy's net (the
    // reward, transactionrecord.cpp nNet). Recompute the net for every stored
    // coinstake row whose input side the store can prove (exact outpoints —
    // the same recv_total - spent_total walk apply_block runs for new rows).
    // The fill persists through the ordinary save_cache below (atomic
    // tmp+rename; the empty-cache guard at scanned_height == 0 stays intact),
    // so this costs one store pass on the first rescan after the upgrade and
    // nothing on every later run. Rows whose inputs do not resolve stay None
    // and render the honest NET-UNKNOWN fallback.
    if from > 0 && cache.rows.iter().any(|r| r.coinstake && r.stake_net.is_none()) {
        match backfill_stake_nets(&store, &mut cache) {
            Ok(n) if n > 0 => eprintln!(
                "history: backfilled {} pre-field stake row(s) to the legacy net",
                n
            ),
            Ok(_) => {}
            Err(e) => eprintln!("history: stake-net backfill skipped: {}", e),
        }
    }

    let mut f = std::fs::File::open(store_path)
        .map_err(|e| format!("open {}: {}", store_path.display(), e))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut pos = if from > 0 {
        offset_hint.unwrap_or(0)
    } else {
        0
    };
    let mut height = from;
    let mut hdr = [0u8; 8];
    let started = std::time::Instant::now();
    let mut scanned = 0u64;

    loop {
        if pos + 8 > len {
            break;
        }
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let blen = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if magic != MAGIC {
            return Err(format!("bad frame magic at offset {}", pos));
        }
        let mut body = vec![0u8; blen];
        f.read_exact(&mut body).map_err(|e| e.to_string())?;
        let mut r = Reader::new(&body);
        let block =
            Block::read(&mut r).map_err(|e| format!("block at height {}: {}", height, e))?;
        // true height: store index by hash; frames above the threaded tip are
        // stale and skipped entirely
        let hash = block.hash();
        let real_height = match store.block_by_hash(&hash).ok().flatten().and_then(|h| h.height) {
            Some(h) => h,
            None => {
                // not in the chain (reorg leftover / above tip): skip
                pos += 8 + blen as u64;
                continue;
            }
        };
        if let Some(tip) = threaded_tip {
            if real_height > tip {
                pos += 8 + blen as u64;
                continue;
            }
        }
        apply_block(
            &block,
            real_height,
            &by_payload,
            &by_pubkey,
            &mut cache.rows,
            &mut owned,
        );
        pos += 8 + blen as u64;
        height += 1;
        scanned += 1;
        if real_height > cache.scanned_height {
            cache.scanned_height = real_height;
            cache.scanned_offset = pos;
        }
        if scanned % 50_000 == 0 {
            let bps = scanned as f64 / started.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "history: h{} ({:.0} blocks/s, {} rows)",
                height - 1,
                bps,
                cache.rows.len()
            );
        }
    }
    eprintln!(
        "history: scan complete at h{} — {} rows in {:.1}s",
        cache.scanned_height,
        cache.rows.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(cache)
}

fn hex_to_internal(s: &str) -> Result<[u8; 32], ()> {
    if s.len() != 64 {
        return Err(());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| ())?;
    }
    out.reverse();
    Ok(out)
}

fn apply_block(
    block: &Block,
    height: u64,
    by_payload: &HashMap<[u8; 21], String>,
    by_pubkey: &HashMap<Vec<u8>, String>,
    rows: &mut Vec<HistoryRec>,
    owned: &mut HashMap<([u8; 32], u32), usize>,
) {
    let time = block.time;
    for (ti, tx) in block.vtx.iter().enumerate() {
        let txid_internal = tx.hash();
        let txid = display_hex(&txid_internal);
        let is_coinbase = ti == 0 && tx.vin.iter().any(|v| v.prev_n == u32::MAX);
        let is_coinstake = ti == 1
            && tx.vin.iter().any(|v| v.prev_n != u32::MAX)
            && tx.vout.first().map(|o| o.value == 0).unwrap_or(false);

        // spends of our coins
        let mut spent_total: i64 = 0;
        // addresses our inputs paid FROM (resolved from the rows map — the
        // prevouts of an ours-vin are by definition outputs we received).
        // Consumed by listaddressgroupings' spend branch.
        let mut spent_from: Vec<String> = Vec::new();
        for vin in &tx.vin {
            if vin.prev_n == u32::MAX {
                continue;
            }
            if let Some(idx) = owned.get(&(vin.prev_hash, vin.prev_n)).copied() {
                if let Some(row) = rows.get_mut(idx) {
                    if !spent_from.contains(&row.address) {
                        spent_from.push(row.address.clone());
                    }
                    if !row.spent {
                        row.spent = true;
                        spent_total += row.value_sats;
                    }
                }
            }
        }

        // outputs paying us
        let rows_before_tx = rows.len();
        let mut recv_total: i64 = 0;
        let mut first_addr = String::new();
        for (n, vout) in tx.vout.iter().enumerate() {
            if let Some(addr) = out_owner(&vout.script_pubkey, by_payload, by_pubkey) {
                recv_total += vout.value;
                if first_addr.is_empty() {
                    first_addr = addr.clone();
                }
                let kind = if is_coinbase {
                    "coinbase"
                } else if is_coinstake {
                    "coinstake"
                } else {
                    "regular"
                };
                rows.push(HistoryRec {
                    txid: txid.clone(),
                    height,
                    time,
                    value_sats: vout.value,
                    address: addr,
                    direction: "in".into(),
                    kind: kind.into(),
                    spent: false,
                    // one row per output to us, indexed like the Qt model
                    idx: n as u32,
                    legacy_kind: if is_coinbase || is_coinstake {
                        "mined".into()
                    } else {
                        "recv_with".into()
                    },
                    coinstake: is_coinstake,
                    // principal = outputs paying us in this tx minus this one;
                    // for a normal coinstake that is the returned stake
                    stake_principal: if is_coinstake {
                        tx.vout
                            .iter()
                            .enumerate()
                            .filter(|(i, o)| {
                                *i != n
                                    && out_owner(&o.script_pubkey, by_payload, by_pubkey).is_some()
                            })
                            .map(|(_, o)| o.value)
                            .sum()
                    } else {
                        0
                    },
                    // our inputs' addresses (empty when this tx spent nothing
                    // of ours)
                    input_addresses: spent_from.clone(),
                    // filled after the outputs loop (needs recv_total +
                    // spent_total); None keeps old files loadable
                    stake_net: None,
                });
                owned.insert((txid_internal, n as u32), rows.len() - 1);
            }
        }

        // legacy nNet for every coinstake row of this tx: the reward, not the
        // principal+reward output value (transactionrecord.cpp
        // `sub.credit = nNet > 0 ? nNet : GetValueOut() - nDebit`). Both
        // sides are known only after the outputs loop, so fill the rows this
        // call pushed.
        if is_coinstake {
            let net = recv_total - spent_total;
            let pushed = rows.len() - rows_before_tx;
            for r in rows.iter_mut().rev().take(pushed) {
                if r.coinstake {
                    r.stake_net = Some(net);
                }
            }
        }

        // a spend where the received side is smaller than what left = an
        // outgoing payment (legacy wallet lists it as a debit row)
        if spent_total > 0 && recv_total < spent_total && !is_coinstake {
            rows.push(HistoryRec {
                txid,
                height,
                time,
                value_sats: recv_total - spent_total, // negative
                address: first_addr,
                direction: "out".into(),
                kind: "regular".into(),
                spent: true,
                idx: 0,
                legacy_kind: "send".into(),
                coinstake: false,
                stake_principal: 0,
                input_addresses: spent_from.clone(),
                stake_net: None,
            });
        }
    }
}

/// Exact outpoint ownership over stored rows: (txid internal, output idx) →
/// (row position, row height). The input side the backfill resolves prevouts
/// against — rows carry `idx` (the output index), so the match is exact, not
/// the n<4 over-approximation the incremental spend-marking walk uses.
pub(crate) type OwnedIndex = HashMap<([u8; 32], u32), (usize, u64)>;

pub(crate) fn build_owned_index(rows: &[HistoryRec]) -> OwnedIndex {
    let mut m = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.direction != "in" {
            continue;
        }
        if let Ok(bytes) = hex_to_internal(&r.txid) {
            m.insert((bytes, r.idx), (i, r.height));
        }
    }
    m
}

/// Legacy net for ONE stake tx from stored rows + the raw tx from the store
/// (transactionrecord.cpp: `sub.credit = nNet = GetCredit() - GetDebit()`,
/// with `nNet > 0 ? nNet : GetValueOut() - nDebit` — the REWARD only).
///
/// spent_total walks the tx's prevouts against the rows' exact (txid, idx)
/// outpoints — the same walk `apply_block` runs for new rows, each input
/// strictly older than the stake block (a stake kernel is always an earlier
/// coin; the height guard also rejects idx-less rows defaulting to 0).
///
/// A coinstake's inputs are by consensus ALL ours, so every vin must resolve
/// to a stored row: one that does not means the FILE is missing a coin
/// (scan gap or the pre-stake_net files' dedupe, which never keyed idx) and
/// the row data cannot prove this debit — the net stays None. The credit
/// side is the RAW tx's own output sum (legacy GetValueOut parity), not the
/// rows: a lost output row would otherwise understate the credit and turn a
/// normal stake's net negative (observed live: one h2192180 stake computed
/// -12.8M sats from incomplete rows; from the raw tx it is a ~+44k reward).
///
/// None = the input side is not fully provable or the result is not a
/// positive reward, and the caller MUST keep stake_net None so the render
/// stays honest (the gross amount then shows with a NET-UNKNOWN tooltip,
/// never silently as income).
pub(crate) fn compute_backfill_net(
    rows: &[HistoryRec],
    owned: &OwnedIndex,
    stake_height: u64,
    raw_vout_sats: i64,
    vins: &[([u8; 32], u32)],
) -> Option<i64> {
    if vins.is_empty() {
        return None;
    }
    let mut spent_total = 0i64;
    for vin in vins {
        let &(pos, row_height) = owned.get(vin)?;
        if row_height >= stake_height {
            return None;
        }
        spent_total += rows[pos].value_sats;
    }
    let net = raw_vout_sats.checked_sub(spent_total)?;
    if net <= 0 {
        None
    } else {
        Some(net)
    }
}

/// One-time migration (2026-11-09 incident): history.json rows written
/// before the `stake_net` field existed carry None, and the GUI rendered
/// every stake roll as "Mined +<principal> TRI" instead of legacy's net (the
/// reward). For every stored coinstake row, resolve the stake tx from the
/// block store's tx index, walk its prevouts against the rows' exact
/// outpoints — the same recv_total - spent_total walk `apply_block` runs for
/// new rows — and fill the legacy net. The caller persists the fill through
/// the ordinary `save_cache` (atomic tmp+rename; the empty-cache guard at
/// scanned_height == 0 stays intact). Rows whose inputs do not resolve in
/// the store keep None on purpose: unknown must stay unknown.
/// Returns the number of rows backfilled.
pub fn backfill_stake_nets(
    store: &crate::blockindex::BlockStore,
    cache: &mut HistoryCache,
) -> Result<usize, String> {
    // pending stakes grouped by txid: the row positions of each stake tx
    let mut pending: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in cache.rows.iter().enumerate() {
        if r.coinstake && r.stake_net.is_none() && r.direction == "in" {
            pending
                .entry(r.txid.clone())
                .or_insert_with(Vec::new)
                .push(i);
        }
    }
    if pending.is_empty() {
        return Ok(0);
    }
    let owned = build_owned_index(&cache.rows);
    let mut filled = 0usize;
    for (txid, positions) in pending.iter() {
        let txid_int = match hex_to_internal(txid) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let hit = match store.tx_by_id(&txid_int) {
            Ok(Some(h)) => h,
            _ => continue, // tx not in the store: net stays unknown
        };
        let stake_height = match hit.block_height {
            Some(h) => h,
            None => continue, // heightless frame: the guard cannot be applied
        };
        // credit side from the RAW tx (legacy GetValueOut): every vout sums
        // in, the vout0 stake marker is 0 by the coinstake definition
        let raw_vout_sats: i64 = hit.tx.vout.iter().map(|v| v.value).sum();
        let vins: Vec<([u8; 32], u32)> = hit
            .tx
            .vin
            .iter()
            .filter(|v| v.prev_n != u32::MAX)
            .map(|v| (v.prev_hash, v.prev_n))
            .collect();
        let net = match compute_backfill_net(&cache.rows, &owned, stake_height, raw_vout_sats, &vins)
        {
            Some(n) => n,
            None => continue,
        };
        for &p in positions {
            cache.rows[p].stake_net = Some(net);
        }
        filled += positions.len();
    }
    Ok(filled)
}

/// Chain-threading oracle from `<state_dir>/tipstate.json` (window entries).
fn load_oracle(state_dir: &Path) -> Option<crate::blockindex::BuildOracle> {
    let raw = std::fs::read_to_string(state_dir.join("tipstate.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entries = v.get("entries")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let base = entries.first()?.get("height")?.as_u64()?;
    let tip = v.get("height").and_then(|h| h.as_u64())?;
    let mut hashes = Vec::with_capacity(entries.len());
    for e in entries {
        let hex = e.get("hash")?.as_str()?;
        if hex.len() != 64 {
            return None;
        }
        let mut raw = [0u8; 32];
        for i in 0..32 {
            raw[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        // tipstate stores display hex
        raw.reverse();
        hashes.push(raw);
    }
    Some(crate::blockindex::BuildOracle { base, hashes, tip })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_is_display_order() {
        let s = "aa".repeat(31) + "bb";
        let internal = hex_to_internal(&s).unwrap();
        assert_eq!(display_hex(&internal), s);
    }

    #[test]
    fn out_owner_matches_p2pkh_and_p2pk() {
        // p2pkh for a known pubkey
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        let addr = crate::address::payload_to_address(&payload);
        let mut by_payload = HashMap::new();
        by_payload.insert(payload, addr.clone());
        let mut by_pubkey = HashMap::new();
        by_pubkey.insert(pubkey.clone(), addr.clone());

        let mut p2pkh = vec![0x76, 0xa9, 0x14];
        p2pkh.extend_from_slice(&payload[1..21]);
        p2pkh.extend_from_slice(&[0x88, 0xac]);
        assert_eq!(
            out_owner(&p2pkh, &by_payload, &by_pubkey).as_deref(),
            Some(addr.as_str())
        );

        let mut p2pk = vec![33u8];
        p2pk.extend_from_slice(&pubkey);
        p2pk.push(0xac);
        assert_eq!(
            out_owner(&p2pk, &by_payload, &by_pubkey).as_deref(),
            Some(addr.as_str())
        );
        assert!(out_owner(&[0x6a, 0x01, 0x02], &by_payload, &by_pubkey).is_none());
    }

    #[test]
    fn cache_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tri7-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = HistoryCache {
            scanned_height: 42,
            scanned_offset: 1234,
            rows: vec![HistoryRec {
                txid: "ab".repeat(32),
                height: 41,
                time: 1_700_000_000,
                value_sats: 500,
                address: "T".into(),
                direction: "in".into(),
                kind: "regular".into(),
                spent: false,
                idx: 0,
                legacy_kind: "recv_with".into(),
                coinstake: false,
                stake_principal: 0,
                input_addresses: Vec::new(),
                stake_net: None,
            }],
        };
        save_cache(&dir, &c).unwrap();
        let back = load_cache(&dir).unwrap();
        assert_eq!(back.scanned_height, 42);
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].value_sats, 500);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// transactionrecord.cpp stake decomposition: the row's rendered amount
    /// is nNet (GetCredit - GetDebit = the reward), never the principal+
    /// reward output value; one net per tx shared by every coinstake row of
    /// that tx; non-stake rows keep 0 (unknown net).
    #[test]
    fn coinstake_rows_carry_the_legacy_net_reward() {
        use crate::legacy::{Block, Tx, Vin, Vout};
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[11u8; 32]).unwrap();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        let addr = crate::address::payload_to_address(&payload);
        let mut by_payload = HashMap::new();
        by_payload.insert(payload.clone(), addr.clone());
        let mut by_pubkey = HashMap::new();
        by_pubkey.insert(pubkey.clone(), addr.clone());
        let p2pkh = {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&payload[1..21]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        };

        let mut rows: Vec<HistoryRec> = Vec::new();
        let mut owned: HashMap<([u8; 32], u32), usize> = HashMap::new();

        // h100: a coinbase paying us 33_333_333
        let coinbase = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 33_333_333,
                script_pubkey: p2pkh.clone(),
            }],
            locktime: 0,
        };
        let cb_block = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_000,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![coinbase],
            block_sig: Vec::new(),
        };
        apply_block(&cb_block, 100, &by_payload, &by_pubkey, &mut rows, &mut owned);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stake_net, None, "non-stake rows have no net");

        // h101: single-output stake — 33_333_333 in, 33_372_923 out => +39_590
        let stake1 = Tx {
            version: 1,
            time: 1_700_000_300,
            vin: vec![Vin {
                prev_hash: cb_block.vtx[0].hash(),
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: Vec::new(),
                },
                Vout {
                    value: 33_372_923,
                    script_pubkey: p2pkh.clone(),
                },
            ],
            locktime: 0,
        };
        let stake_block1 = Block {
            version: 1,
            prev_hash: [2u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_300,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![empty_tx(), stake1],
            block_sig: Vec::new(),
        };
        apply_block(
            &stake_block1,
            101,
            &by_payload,
            &by_pubkey,
            &mut rows,
            &mut owned,
        );
        let s1: Vec<&HistoryRec> = rows.iter().filter(|r| r.coinstake).collect();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].value_sats, 33_372_923, "raw output value preserved");
        assert_eq!(s1[0].stake_net, Some(39_590), "rendered amount = the reward");

        // h102: split stake — 33_372_923 in, two our-outputs 20_000_000 +
        // 13_373_003 => recv 33_373_003, net +80 on BOTH rows
        let stake2 = Tx {
            version: 1,
            time: 1_700_000_600,
            vin: vec![Vin {
                prev_hash: stake_block1.vtx[1].hash(),
                prev_n: 1,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: Vec::new(),
                },
                Vout {
                    value: 20_000_000,
                    script_pubkey: p2pkh.clone(),
                },
                Vout {
                    value: 13_373_003,
                    script_pubkey: p2pkh.clone(),
                },
            ],
            locktime: 0,
        };
        let stake_block2 = Block {
            version: 1,
            prev_hash: [3u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_600,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![empty_tx(), stake2],
            block_sig: Vec::new(),
        };
        apply_block(
            &stake_block2,
            102,
            &by_payload,
            &by_pubkey,
            &mut rows,
            &mut owned,
        );
        let s2: Vec<&HistoryRec> = rows.iter().filter(|r| r.coinstake).skip(1).collect();
        assert_eq!(s2.len(), 2, "per-output rows preserved for ownership");
        assert_eq!(s2[0].value_sats, 20_000_000);
        assert_eq!(s2[1].value_sats, 13_373_003);
        assert_eq!(s2[0].stake_net, Some(80));
        assert_eq!(s2[1].stake_net, Some(80));
    }

    /// listaddressgroupings' spend branch feeds on HistoryRec.input_addresses:
    /// every row of a tx that spent our coins carries the addresses those
    /// inputs paid FROM (resolved from the rows map), rows of receive-only
    /// txs carry none.
    #[test]
    fn spend_rows_carry_the_input_addresses() {
        use crate::legacy::{Block, Tx, Vin, Vout};
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[12u8; 32]).unwrap();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        let addr = crate::address::payload_to_address(&payload);
        let mut by_payload = HashMap::new();
        by_payload.insert(payload.clone(), addr.clone());
        let mut by_pubkey = HashMap::new();
        by_pubkey.insert(pubkey.clone(), addr.clone());
        let p2pkh = {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&payload[1..21]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        };

        let mut rows: Vec<HistoryRec> = Vec::new();
        let mut owned: HashMap<([u8; 32], u32), usize> = HashMap::new();

        // h100: coinbase paying us 10 TRI
        let coinbase = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 10_000_000,
                script_pubkey: p2pkh.clone(),
            }],
            locktime: 0,
        };
        let cb_block = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_000,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![coinbase],
            block_sig: Vec::new(),
        };
        apply_block(&cb_block, 100, &by_payload, &by_pubkey, &mut rows, &mut owned);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].input_addresses.is_empty(), "receive-only tx has no input side");

        // h101: we spend that coin — pay a FOREIGN address 4 TRI, change 6
        // back to us (recv < spent => the debit "send" row is emitted)
        let spend = Tx {
            version: 1,
            time: 1_700_000_100,
            vin: vec![Vin {
                prev_hash: cb_block.vtx[0].hash(),
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 6_000_000,
                    script_pubkey: p2pkh.clone(),
                },
                Vout {
                    value: 4_000_000,
                    script_pubkey: {
                        // foreign payload (nobody's key)
                        let mut s = vec![0x76, 0xa9, 0x14];
                        s.extend_from_slice(&[0xAB; 20]);
                        s.extend_from_slice(&[0x88, 0xac]);
                        s
                    },
                },
            ],
            locktime: 0,
        };
        let spend_block = Block {
            version: 1,
            prev_hash: [2u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_100,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![empty_tx(), spend],
            block_sig: Vec::new(),
        };
        apply_block(
            &spend_block,
            101,
            &by_payload,
            &by_pubkey,
            &mut rows,
            &mut owned,
        );
        assert_eq!(rows.len(), 3, "change row + foreign-free send row appended");
        let send = rows.iter().find(|r| r.direction == "out").unwrap();
        assert_eq!(
            send.input_addresses,
            vec![addr.clone()],
            "the send row carries the address the spent coin came from"
        );
        let change = rows
            .iter()
            .find(|r| r.direction == "in" && r.height == 101)
            .unwrap();
        assert_eq!(
            change.input_addresses,
            vec![addr],
            "the change row of the spend tx carries the same input side"
        );
        assert_eq!(
            rows[0].input_addresses,
            Vec::<String>::new(),
            "the earlier receive row is untouched"
        );
    }

    /// old history.json files predate input_addresses: serde(default) must
    /// load them (empty vec), and the field roundtrips on new files.
    #[test]
    fn history_rows_without_input_addresses_still_load() {
        let old = serde_json::json!({
            "txid": "ab".repeat(32),
            "height": 100,
            "time": 1_700_000_000u32,
            "value_sats": 500,
            "address": "T",
            "direction": "in",
            "kind": "regular",
            "spent": false,
            "idx": 0,
            "legacy_kind": "recv_with",
            "coinstake": false,
            "stake_principal": 0,
            "stake_net": null,
        });
        let rec: HistoryRec = serde_json::from_value(old).unwrap();
        assert!(rec.input_addresses.is_empty());
        let back = serde_json::to_value(&rec).unwrap();
        let re: HistoryRec = serde_json::from_value(back).unwrap();
        assert_eq!(re.input_addresses, rec.input_addresses);
    }

    /// 2026-11-09 incident, gap 1 — the pure half of the backfill: a
    /// pre-stake_net coinstake row gets the legacy net backfilled from the
    /// store's proof of its input side (the raw tx's output sum minus the
    /// our-rows its prevouts consumed, exact (txid, idx) outpoints, each
    /// older than the stake block). Rows the file lost (dedupe never keyed
    /// idx / scan gaps) make the debit side unprovable — ALL vins must
    /// resolve — and a non-positive result is not a reward: both keep None.
    #[test]
    fn backfill_recomputes_pre_field_stake_rows_to_the_legacy_net() {
        use crate::legacy::Tx;
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[13u8; 32]).unwrap();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        let addr = crate::address::payload_to_address(&payload);
        let p2pkh = {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&payload[1..21]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        };
        // the funding coinbase tx, exactly as the chain stored it
        let cb = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![crate::legacy::Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![crate::legacy::Vout {
                value: 33_333_333,
                script_pubkey: p2pkh.clone(),
            }],
            locktime: 0,
        };
        let cb_txid = display_hex(&cb.hash());
        let rows = vec![
            HistoryRec {
                txid: cb_txid.clone(),
                height: 100,
                time: 1_700_000_000,
                value_sats: 33_333_333,
                address: addr.clone(),
                direction: "in".into(),
                kind: "coinbase".into(),
                spent: true,
                idx: 0,
                legacy_kind: "mined".into(),
                coinstake: false,
                stake_principal: 0,
                input_addresses: Vec::new(),
                stake_net: None,
            },
            // the pre-field stake row: value = principal+reward output, net unknown
            HistoryRec {
                txid: "ab".repeat(32),
                height: 101,
                time: 1_700_000_300,
                value_sats: 33_372_923,
                address: addr,
                direction: "in".into(),
                kind: "coinstake".into(),
                spent: false,
                idx: 1,
                legacy_kind: "mined".into(),
                coinstake: true,
                stake_principal: 0,
                input_addresses: Vec::new(),
                stake_net: None,
            },
        ];
        let owned = build_owned_index(&rows);
        let vins = vec![(hex_to_internal(&cb_txid).unwrap(), 0u32)];
        assert_eq!(
            compute_backfill_net(&rows, &owned, 101, 33_372_923, &vins),
            Some(39_590),
            "legacy nNet = raw tx outputs - our consumed coin"
        );
        // a second kernel coin the file LOST (dedupe never keyed idx): the
        // debit side is unprovable -> None, never a half-computed "net"
        let half_lost = vec![vins[0], ([9u8; 32], 0u32)];
        assert_eq!(
            compute_backfill_net(&rows, &owned, 101, 66_700_000, &half_lost),
            None
        );
        // inputs the store cannot prove at all: keep None
        let unknown = vec![([9u8; 32], 0u32)];
        assert_eq!(
            compute_backfill_net(&rows, &owned, 101, 33_372_923, &unknown),
            None
        );
        // same-block input (idx-less rows defaulting to 0 must not pair here):
        // the height guard rejects it, so the net stays unknown
        let owned_same = build_owned_index(&rows);
        assert_eq!(
            compute_backfill_net(&rows, &owned_same, 100, 33_333_333, &vins),
            None
        );
        // a non-positive result is not a stake reward (lost output row):
        // keep None rather than book a stake that "lost" money
        assert_eq!(
            compute_backfill_net(&rows, &owned, 101, 33_333_333, &vins),
            None
        );
    }

    /// 2026-11-09 incident, gap 1 — the store half of the backfill: a real
    /// BlockStore index (oracle-threaded frames) resolves the pre-field
    /// stake's prevout and fills the legacy net; a coinstake row whose tx is
    /// NOT in the store keeps None. The fill round-trips through save_cache,
    /// so the migration persists for the GUI.
    #[test]
    fn backfill_stake_nets_fills_from_the_store_and_keeps_unknown_rows_none() {
        use crate::blockindex::{BlockStore, BuildOracle};
        use crate::legacy::{Block, Tx, Vin, Vout};
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[14u8; 32]).unwrap();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        let addr = crate::address::payload_to_address(&payload);
        let p2pkh = {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&payload[1..21]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        };

        // h100 coinbase -> h101 single-output stake (33_333_333 in,
        // 33_372_923 out => net +39_590), chained and oracle-threaded
        let coinbase = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 33_333_333,
                script_pubkey: p2pkh.clone(),
            }],
            locktime: 0,
        };
        let cb_block = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_700_000_000,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![coinbase],
            block_sig: Vec::new(),
        };
        let stake1 = Tx {
            version: 1,
            time: 1_700_000_300,
            vin: vec![Vin {
                prev_hash: cb_block.vtx[0].hash(),
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: Vec::new(),
                },
                Vout {
                    value: 33_372_923,
                    script_pubkey: p2pkh.clone(),
                },
            ],
            locktime: 0,
        };
        let stake_block1 = Block {
            version: 1,
            prev_hash: cb_block.hash(),
            merkle_root: [0u8; 32],
            time: 1_700_000_300,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![empty_tx(), stake1],
            block_sig: Vec::new(),
        };

        let dir = std::env::temp_dir().join(format!("tri7-backfill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dat = dir.join("blk0001.dat");
        let mut f = std::fs::File::create(&dat).unwrap();
        for b in [&cb_block, &stake_block1] {
            let body = b.serialize();
            use std::io::Write as _;
            f.write_all(&MAGIC.to_le_bytes()).unwrap();
            f.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&body).unwrap();
        }
        drop(f);
        let oracle = BuildOracle {
            base: 100,
            hashes: vec![cb_block.hash(), stake_block1.hash()],
            tip: 101,
        };
        let store = BlockStore::new(Some(dat.clone()), None);
        store.spawn_build(Some(oracle));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            if let crate::blockindex::StoreStatus::Ready { .. } = store.status() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(ready, "test store index must become ready");

        // pre-field cache: the coinbase row + the stake row with stake_net
        // None, plus a coinstake whose tx the store does NOT have
        let stake_txid = display_hex(&stake_block1.vtx[1].hash());
        let cache = HistoryCache {
            scanned_height: 101,
            scanned_offset: 999,
            rows: vec![
                HistoryRec {
                    txid: display_hex(&cb_block.vtx[0].hash()),
                    height: 100,
                    time: 1_700_000_000,
                    value_sats: 33_333_333,
                    address: addr.clone(),
                    direction: "in".into(),
                    kind: "coinbase".into(),
                    spent: true,
                    idx: 0,
                    legacy_kind: "mined".into(),
                    coinstake: false,
                    stake_principal: 0,
                    input_addresses: Vec::new(),
                    stake_net: None,
                },
                HistoryRec {
                    txid: stake_txid.clone(),
                    height: 101,
                    time: 1_700_000_300,
                    value_sats: 33_372_923,
                    address: addr.clone(),
                    direction: "in".into(),
                    kind: "coinstake".into(),
                    spent: false,
                    idx: 1,
                    legacy_kind: "mined".into(),
                    coinstake: true,
                    stake_principal: 0,
                    input_addresses: Vec::new(),
                    stake_net: None,
                },
                HistoryRec {
                    txid: "ee".repeat(32),
                    height: 101,
                    time: 1_700_000_300,
                    value_sats: 44_444_444,
                    address: addr,
                    direction: "in".into(),
                    kind: "coinstake".into(),
                    spent: false,
                    idx: 1,
                    legacy_kind: "mined".into(),
                    coinstake: true,
                    stake_principal: 0,
                    input_addresses: Vec::new(),
                    stake_net: None,
                },
            ],
        };
        let mut cache = cache;
        let filled = backfill_stake_nets(&store, &mut cache).unwrap();
        assert_eq!(filled, 1, "only the store-provable stake is filled");
        assert_eq!(cache.rows[1].stake_net, Some(39_590));
        assert_eq!(
            cache.rows[2].stake_net, None,
            "tx absent from the store: net stays unknown"
        );
        assert_eq!(cache.rows[0].stake_net, None, "non-stake row untouched");

        // the fill persists: save_cache + load_cache keeps the net
        save_cache(&dir, &cache).unwrap();
        let back = load_cache(&dir).unwrap();
        assert_eq!(back.rows[1].stake_net, Some(39_590));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The empty-scan guard (seen live on sami-pc): a genesis-started scan
    /// checkpoints an EMPTY cache immediately; if that file landed over a
    /// real history the wallet showed zero transactions forever. save_cache
    /// must refuse to write an empty scanned_height == 0 cache over rows.
    #[test]
    fn save_cache_refuses_empty_scan_over_a_non_empty_file() {
        let dir = std::env::temp_dir().join(format!("tri7-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = HistoryCache {
            scanned_height: 2_205_300,
            scanned_offset: 999,
            rows: vec![HistoryRec {
                txid: "ab".repeat(32),
                height: 2_205_299,
                time: 1_700_000_000,
                value_sats: 500,
                address: "T".into(),
                direction: "in".into(),
                kind: "regular".into(),
                spent: false,
                idx: 0,
                legacy_kind: "recv_with".into(),
                coinstake: false,
                stake_principal: 0,
                input_addresses: Vec::new(),
                stake_net: None,
            }],
        };
        save_cache(&dir, &real).unwrap();
        let before = std::fs::read_to_string(cache_path(&dir)).unwrap();
        save_cache(&dir, &HistoryCache::default()).unwrap();
        let after = std::fs::read_to_string(cache_path(&dir)).unwrap();
        assert_eq!(before, after, "empty scan must not replace real history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// minimal non-coinbase filler for vtx[0] of test blocks (produces no
    /// history rows)
    fn empty_tx() -> crate::legacy::Tx {
        crate::legacy::Tx {
            version: 1,
            time: 1_700_000_000,
            vin: Vec::new(),
            vout: Vec::new(),
            locktime: 0,
        }
    }
}
