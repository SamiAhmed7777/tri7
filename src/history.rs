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
        for vin in &tx.vin {
            if vin.prev_n == u32::MAX {
                continue;
            }
            if let Some(idx) = owned.get(&(vin.prev_hash, vin.prev_n)).copied() {
                if let Some(row) = rows.get_mut(idx) {
                    if !row.spent {
                        row.spent = true;
                        spent_total += row.value_sats;
                    }
                }
            }
        }

        // outputs paying us
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
                });
                owned.insert((txid_internal, n as u32), rows.len() - 1);
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
            });
        }
    }
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
            }],
        };
        save_cache(&dir, &c).unwrap();
        let back = load_cache(&dir).unwrap();
        assert_eq!(back.scanned_height, 42);
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].value_sats, 500);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
