//! Persistent peer store — `peers.json` in the state dir.
//!
//! The legacy client kept a `CAddrMan` and flushed it to `peers.dat`; tri7
//! keeps the same facts in a small JSON file so discovery survives restarts:
//! peers learned from `addr` gossip, configured seeds and our own address all
//! land here, and the dial loop orders by score with exponential backoff on
//! repeated failures.
//!
//! File shape (atomic write: temp + rename):
//!
//! ```json
//! { "peers": [ { "host": "<onion|ip|tor-key-hex>", "port": 24113,
//!                "services": 1, "source": "seed|config|addr|listen",
//!                "last_ok": <unix>, "last_fail": <unix>,
//!                "ok_count": n, "fail_count": n } ] }
//! ```
//!
//! `host` forms (see `crate::p2p::WireAddr::store_key`): an onion hostname
//! (dialable) — wire-learned `addr` gossip reconstructs the v3 hostname from
//! its 32-byte tor pubkey via the SHA3 label checksum
//! (`WireAddr::onion_hostname`), so every tor_v3 entry lands dialable;
//! legacy files may still carry a 64-char hex tor key (relay-only) or a
//! dotted quad (never dialed — the network is Tor-only). The store cap
//! is 1000 entries; a corrupt file yields an EMPTY store plus a log line,
//! never an abort.

use crate::p2p::{AddrEntry, WireAddr, MAX_ADDR_ENTRIES};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Store cap — matches the legacy addr vector cap.
pub const PEER_STORE_CAP: usize = MAX_ADDR_ENTRIES;

/// unix seconds at "now"
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn log_peer_store(msg: &str) {
    eprintln!("[peers] {}", msg);
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PeerEntryJson {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub services: u64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub last_ok: u64,
    #[serde(default)]
    pub last_fail: u64,
    #[serde(default)]
    pub ok_count: u64,
    #[serde(default)]
    pub fail_count: u64,
    // PEER-HEALTH: what the peer actually SERVED us (blocks / bytes) and when
    // — a handshake alone is not health. Older files omit these (default 0).
    #[serde(default)]
    pub served_blocks: u64,
    #[serde(default)]
    pub served_bytes: u64,
    #[serde(default)]
    pub last_served: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeerEntry {
    pub host: String,
    pub port: u16,
    pub services: u64,
    pub source: String,
    pub last_ok: u64,
    pub last_fail: u64,
    pub ok_count: u64,
    pub fail_count: u64,
    /// blocks/bytes this peer actually SERVED us (PEER-HEALTH)
    pub served_blocks: u64,
    pub served_bytes: u64,
    /// last time it served a block (0 = never)
    pub last_served: u64,
}

impl PeerEntry {
    pub fn new(host: &str, port: u16, services: u64, source: &str, _now: u64) -> Self {
        PeerEntry {
            host: host.to_string(),
            port,
            services,
            source: source.to_string(),
            last_ok: 0,
            last_fail: 0,
            ok_count: 0,
            fail_count: 0,
            served_blocks: 0,
            served_bytes: 0,
            last_served: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------
#[derive(Serialize, Deserialize, Default)]
struct PeerFile {
    peers: Vec<PeerEntryJson>,
}

#[derive(Clone, Debug, Default)]
pub struct PeerStore {
    peers: Vec<PeerEntry>,
    /// identities we must never store as peers: our own onion hostname and
    /// the hex of our own tor_v3 key
    self_idents: Vec<String>,
    /// TRI7 (gossip amplification): a mutation landed that is not on disk yet.
    /// Callers that merge `addr`/`version` gossip on every message use
    /// `save_throttled`, which coalesces the full-file rewrite to at most one
    /// per `PEER_PERSIST_INTERVAL` and flushes whatever is still pending on
    /// drop.
    dirty: bool,
    /// when the last coalesced write landed (None = never)
    last_persist: Option<std::time::Instant>,
    /// where coalesced writes go (set by the first throttled save)
    persist_path: Option<PathBuf>,
}

/// Gossip amplification: every `addr`/`version` message used to trigger a
/// FULL peers.json rewrite (load → merge → pretty-print → tmp+rename). One
/// write per 30s is ample for facts that only feed the next dial rotation.
pub const PEER_PERSIST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

impl PeerStore {
    /// Load `<state-dir>/peers.json`. Missing file → empty store. Corrupt
    /// file → empty store + a log line (never an abort — the legacy client
    /// similarly discarded a bad peers.dat).
    pub fn load(path: &Path) -> PeerStore {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<PeerFile>(&text) {
                Ok(file) => {
                    let peers: Vec<PeerEntry> = file
                        .peers
                        .into_iter()
                        .map(|j| PeerEntry {
                            host: j.host,
                            port: j.port,
                            services: j.services,
                            source: j.source,
                            last_ok: j.last_ok,
                            last_fail: j.last_fail,
                            ok_count: j.ok_count,
                            fail_count: j.fail_count,
                            served_blocks: j.served_blocks,
                            served_bytes: j.served_bytes,
                            last_served: j.last_served,
                        })
                        .filter(|e| !e.host.is_empty())
                        .take(PEER_STORE_CAP)
                        .collect();
                    PeerStore {
                        peers,
                        self_idents: Vec::new(),
                        dirty: false,
                        last_persist: None,
                        persist_path: None,
                    }
                }
                Err(e) => {
                    log_peer_store(&format!(
                        "corrupt {} ({}): starting with an empty peer store",
                        path.display(),
                        e
                    ));
                    PeerStore::default()
                }
            },
            Err(_) => PeerStore::default(), // missing == empty, no noise
        }
    }

    /// Atomic save: write `peers.json.tmp`, then rename over the target
    /// (with the Windows access-denied retry loop used by every state-file
    /// writer in this crate). Applies the 1000-entry cap.
    ///
    /// A save whose bytes already equal what is on disk is a NO-OP: the
    /// gossip paths call this after every `addr`/`version` message, and most
    /// of those messages change nothing — rewriting the identical file was
    /// pure amplification. (Permissions are re-checked on unix so a legacy
    /// world-readable file is still tightened.)
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = self.serialize()?;
        if let Ok(on_disk) = std::fs::read(path) {
            if on_disk == text.as_bytes() && Self::perms_ok(path) {
                return Ok(());
            }
        }
        self.write_text(path, &text)
    }

    /// Serialize the store (the exact bytes `save` writes).
    fn serialize(&self) -> Result<String, String> {
        let file = PeerFile {
            peers: self
                .peers
                .iter()
                .take(PEER_STORE_CAP)
                .map(|e| PeerEntryJson {
                    host: e.host.clone(),
                    port: e.port,
                    services: e.services,
                    source: e.source.clone(),
                    last_ok: e.last_ok,
                    last_fail: e.last_fail,
                    ok_count: e.ok_count,
                    fail_count: e.fail_count,
                    served_blocks: e.served_blocks,
                    served_bytes: e.served_bytes,
                    last_served: e.last_served,
                })
                .collect(),
        };
        serde_json::to_string_pretty(&file).map_err(|e| e.to_string())
    }

    /// unix: the file must already be 0600 for the no-op path to apply
    fn perms_ok(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(path) {
                Ok(m) => m.permissions().mode() & 0o777 == 0o600,
                Err(_) => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            true
        }
    }

    fn write_text(&self, path: &Path, text: &str) -> Result<(), String> {
        // STATE-LOCK (F1): a pid-unique tmp. A fixed `peers.json.tmp` let two
        // writers of the same process (loop + RPC) interleave halves of
        // different snapshots before the rename.
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("chmod {}: {}", tmp.display(), e))?;
        }
        let mut last: Option<std::io::Error> = None;
        for attempt in 0..12 {
            match std::fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(
                        50 * (attempt + 1) as u64,
                    ));
                }
            }
        }
        Err(format!(
            "rename {}: {}",
            path.display(),
            last.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    /// Coalesced save for the gossip hot paths (TRI7 amplification fix): mark
    /// dirty and write at most once per `PEER_PERSIST_INTERVAL`; whatever is
    /// still pending is flushed by `flush()` / on Drop. Returns Ok(true) when
    /// this call actually wrote.
    ///
    /// NOTE: callers that ACKNOWLEDGE a mutation to a user (addnode) must keep
    /// using `save` — an ack that is still in the coalescing window would lie.
    pub fn save_throttled(&mut self, path: &Path) -> Result<bool, String> {
        self.persist_path = Some(path.to_path_buf());
        if !self.dirty {
            return Ok(false);
        }
        let due = match self.last_persist {
            None => true,
            Some(t) => t.elapsed() >= PEER_PERSIST_INTERVAL,
        };
        if !due {
            return Ok(false);
        }
        self.save(path)?;
        self.dirty = false;
        self.last_persist = Some(std::time::Instant::now());
        Ok(true)
    }

    /// Persist anything still pending (shutdown path). No-op when clean.
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let Some(path) = self.persist_path.clone() else {
            return Ok(());
        };
        self.save(&path)?;
        self.dirty = false;
        self.last_persist = Some(std::time::Instant::now());
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn peers(&self) -> &[PeerEntry] {
        &self.peers
    }

    /// Register our own identities (onion hostname and/or hex tor key) so
    /// `merge_addr` can skip announcements of ourselves — the legacy
    /// addrman never adds its own address. Also PURGES any store entry that
    /// matches a self identity: a poisoned row (our own onion learned via
    /// gossip BEFORE the identity was known — e.g. a node whose onion is
    /// published from the tor hostname file rather than onion-state.json)
    /// survives every ingest filter forever and the dialer re-dials itself
    /// on every rotation (fleet catch 2026-11-18: DNS2 held its own onion
    /// with 494+65 failed self-dials). Boot re-registers identities, so the
    /// poison cannot survive a restart. Returns the number of rows purged.
    pub fn set_self(&mut self, idents: &[String]) -> usize {
        for id in idents {
            if !id.is_empty() && !self.self_idents.iter().any(|s| s == id) {
                self.self_idents.push(id.clone());
            }
        }
        let before = self.peers.len();
        let idents = self.self_idents.clone();
        self.peers
            .retain(|e| !idents.iter().any(|s| *s == e.host));
        let removed = before - self.peers.len();
        if removed > 0 {
            self.dirty = true;
        }
        removed
    }
    fn is_self(&self, host: &str) -> bool {
        self.self_idents.iter().any(|s| s == host)
    }

    /// Insert or update by `host:port`. New peers take `source`; existing
    /// entries keep their original source unless it was never named.
    /// Keeps the store capped: on overflow the worst entry (lowest score)
    /// is evicted.
    pub fn upsert(
        &mut self,
        host: &str,
        port: u16,
        services: u64,
        source: &str,
        now: u64,
    ) -> bool {
        if host.is_empty() || self.is_self(host) {
            return false;
        }
        if let Some(e) = self
            .peers
            .iter_mut()
            .find(|e| e.host == host && e.port == port)
        {
            if services != 0 {
                e.services = services;
            }
            if e.source.is_empty() {
                e.source = source.to_string();
            }
            self.dirty = true;
            return true;
        }
        self.peers.push(PeerEntry {
            host: host.to_string(),
            port,
            services,
            source: source.to_string(),
            last_ok: 0,
            last_fail: 0,
            ok_count: 0,
            fail_count: 0,
            served_blocks: 0,
            served_bytes: 0,
            last_served: 0,
        });
        if self.peers.len() > PEER_STORE_CAP {
            // evict the worst: never-answered with most failures first
            let worst = (0..self.peers.len())
                .min_by_key(|&i| score_key(&self.peers[i], now))
                .unwrap_or(0);
            self.peers.remove(worst);
        }
        self.dirty = true;
        true
    }

    /// A dial/handshake succeeded.
    pub fn record_ok(&mut self, host: &str, port: u16, now: u64) {
        if let Some(e) = self
            .peers
            .iter_mut()
            .find(|e| e.host == host && e.port == port)
        {
            e.last_ok = now;
            e.ok_count += 1;
            e.fail_count = 0;
            self.dirty = true;
        }
    }

    /// A dial/handshake failed.
    pub fn record_fail(&mut self, host: &str, port: u16, now: u64) {
        if let Some(e) = self
            .peers
            .iter_mut()
            .find(|e| e.host == host && e.port == port)
        {
            e.last_fail = now;
            e.fail_count += 1;
            self.dirty = true;
        }
    }

    /// The peer actually SERVED us data (PEER-HEALTH): blocks and bytes.
    /// This — not the handshake — is what "healthy" means; a peer that
    /// handshakes perfectly and never serves must sink in the dial order.
    pub fn record_served(&mut self, host: &str, port: u16, now: u64, blocks: u64, bytes: u64) {
        if let Some(e) = self
            .peers
            .iter_mut()
            .find(|e| e.host == host && e.port == port)
        {
            e.served_blocks = e.served_blocks.saturating_add(blocks);
            e.served_bytes = e.served_bytes.saturating_add(bytes);
            e.last_served = now;
            self.dirty = true;
        }
    }

    /// The peer VIOLATED expectations (stale/duplicate-only reply, protocol
    /// violation, or a served block that did not extend): count it as a
    /// failure so the backoff and the dial order see it. Distinct from a dial
    /// failure only in intent — both mean "this peer is not useful now".
    pub fn record_unserved(&mut self, host: &str, port: u16, now: u64) {
        if let Some(e) = self
            .peers
            .iter_mut()
            .find(|e| e.host == host && e.port == port)
        {
            e.last_fail = now;
            e.fail_count = e.fail_count.saturating_add(1);
            self.dirty = true;
        }
    }

    /// The (host, port) a peer label refers to, if the label IS an endpoint
    /// (`out:<host>:<port>` / `in:<host>:<port>`). Used by the loop to map a
    /// served-block event back onto the store without threading the store
    /// through every call site.
    pub fn endpoint_of_label(label: &str) -> Option<(String, u16)> {
        let rest = label
            .strip_prefix("out:")
            .or_else(|| label.strip_prefix("in:"))?;
        let (h, p) = rest.rsplit_once(':')?;
        Some((h.to_string(), p.parse().ok()?))
    }

    /// Drop the (host, port) entry — the `addnode <node> remove` verb (the
    /// legacy client erased the string from mapMultiArgs["-addnode"]; tri7's
    /// persistent equivalent is this store). Returns whether it was present
    /// (the legacy verb errors on remove of an unknown node).
    pub fn remove(&mut self, host: &str, port: u16) -> bool {
        let before = self.peers.len();
        self.peers
            .retain(|e| !(e.host == host && e.port == port));
        let removed = self.peers.len() != before;
        if removed {
            self.dirty = true;
        }
        removed
    }

    /// Whether a (host, port) entry exists — the `addnode <node> add`
    /// duplicate check ("Node already added.").
    pub fn contains(&self, host: &str, port: u16) -> bool {
        self.peers.iter().any(|e| e.host == host && e.port == port)
    }

    /// Merge `addr` gossip into the store (net.cpp ProcessMessage "addr"
    /// relay). Overlay-only (Sami: "I2P IS FIRST-CLASS … the fleet is
    /// dual-network"): tor_v3 AND i2p (GarliCat / `is_i2p`) entries are
    /// stored, keyed by their reconstructed `.onion` / `.b32.i2p` HOSTNAME
    /// (see `WireAddr::store_key`) so the dial loop can connect to them;
    /// cleartext IP gossip is still NEVER stored. Entries matching our own
    /// identity are skipped.
    /// Returns the number of entries merged (new or refreshed).
    pub fn merge_addr(&mut self, entries: &[AddrEntry], now: u64) -> usize {
        let mut merged = 0;
        for e in entries {
            if !(e.addr.is_tor_v3 || e.addr.is_i2p) {
                continue; // overlay-only network: never store cleartext gossip
            }
            let Some(host) = e.addr.store_key() else {
                continue;
            };
            if self.is_self(&host) {
                continue;
            }
            if self.upsert(&host, e.addr.port, e.addr.services, "addr", now) {
                merged += 1;
            }
        }
        merged
    }

    /// Peers as wire `addr` entries for serving a getaddr — up to `cap`
    /// (never above MAX_ADDR_ENTRIES), best-seen first, entries only (no
    /// per-entry freshness lie: the time field carries `now` like the legacy
    /// relay did with its own adjust).
    pub fn addr_entries(&self, now: u64, cap: usize) -> Vec<AddrEntry> {
        let cap = cap.min(MAX_ADDR_ENTRIES);
        let mut ordered: Vec<&PeerEntry> = self.peers.iter().collect();
        ordered.sort_by_key(|e| std::cmp::Reverse(score_key(e, now)));
        ordered
            .into_iter()
            .take(cap)
            .filter_map(|e| {
                let addr = WireAddr::from_store_key(&e.host, e.port, e.services)?;
                Some(AddrEntry { time: now.min(u32::MAX as u64) as u32, addr })
            })
            .collect()
    }

    /// Dial order: peers that have EVER answered first (most recently OK
    /// first), then configured seeds, then the rest — all filtered by
    /// exponential backoff (`fail_count` → 2^n seconds, capped at an hour),
    /// except seeds which are config-mandated and always surfaced last
    /// within their group. Returns `(host, port)` pairs ready to dial.
    pub fn dial_order(
        &self,
        now: u64,
        seeds: &[(String, u16)],
    ) -> Vec<(String, u16)> {
        let mut answered: Vec<&PeerEntry> = Vec::new();
        let mut fresh: Vec<&PeerEntry> = Vec::new();
        for e in &self.peers {
            if e.ok_count > 0 {
                answered.push(e);
            } else if in_backoff(e, now) {
                continue;
            } else {
                fresh.push(e);
            }
        }
        // PEER-HEALTH: rank primarily by RECENT SERVE activity. A peer that
        // handshook but never served (served_blocks == 0) sorts BELOW every
        // peer that has produced blocks recently, no matter how fresh its
        // handshake was — that is exactly the "100% to the never-serving
        // peer" shape this fixes.
        answered.sort_by_key(|e| {
            std::cmp::Reverse((
                e.served_blocks > 0,
                e.last_served,
                e.last_ok.saturating_add(e.served_blocks.min(1_000_000)),
            ))
        });
        fresh.sort_by_key(|e| {
            let is_seed = seeds.iter().any(|(h, p)| h == &e.host && *p == e.port);
            (!is_seed, e.fail_count, e.host.clone())
        });
        let mut out: Vec<(String, u16)> = answered
            .iter()
            .map(|e| (e.host.clone(), e.port))
            .collect();
        out.extend(fresh.iter().map(|e| (e.host.clone(), e.port)));
        // configured seeds not already known get appended (they are always
        // worth a try — the network's last resort)
        for (h, p) in seeds {
            if !out.iter().any(|(eh, ep)| eh == h && ep == p) {
                out.push((h.clone(), *p));
            }
        }
        out
    }
}

/// Shutdown-drop: anything the coalesced path still has pending goes to disk
/// when the store is dropped (a session's last gossip merges are not lost).
impl Drop for PeerStore {
    fn drop(&mut self) {
        if self.dirty {
            let _ = self.flush();
        }
    }
}

/// Sort key: answered-and-recent first, then fewer failures. Higher is
/// better for `addr_entries` (used reversed), lower is evicted first.
fn score_key(e: &PeerEntry, _now: u64) -> (u64, u64, u64) {
    // served blocks dominate: an addr entry worth re-serving is one that has
    // actually produced data (PEER-HEALTH)
    (
        e.served_blocks,
        e.last_served.max(e.last_ok),
        u64::MAX - e.fail_count,
    )
}

/// Exponential backoff by fail_count: 2^n seconds since the last failure,
/// capped at one hour. A peer that has never failed is always ready.
fn in_backoff(e: &PeerEntry, now: u64) -> bool {
    if e.fail_count == 0 || e.last_fail == 0 {
        return false;
    }
    let delay = 1u64.checked_shl(e.fail_count.min(63) as u32).unwrap_or(u64::MAX);
    let delay = delay.min(3600);
    now.saturating_sub(e.last_fail) < delay
}

// ---------------------------------------------------------------------------
// Tests — round-trip, corrupt-file behaviour, dial ordering / backoff
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str =
        "35eb13d77044da2f29c189e519139e0b04544db8081ee8edd1db859cd2edf1cb";
    const KEY_B: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "tri7-peers-test-{}-{}.json",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// A5: save/load round-trip preserves every field.
    #[test]
    fn peer_store_roundtrips() {
        let path = tmp_path("roundtrip");
        let mut store = PeerStore::default();
        store.set_self(&["myonion.onion".to_string(), KEY_A.to_string()]);
        store.upsert(KEY_B, 24113, 1, "addr", 1000);
        store.record_ok(KEY_B, 24113, 1100);
        store.upsert("gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion", 24113, 1, "seed", 1000);
        store.record_fail("gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion", 24113, 1050);
        store.record_fail("gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion", 24113, 1060);
        store.save(&path).unwrap();

        let loaded = PeerStore::load(&path);
        assert_eq!(loaded.len(), 2);
        let hex_peer = loaded
            .peers()
            .iter()
            .find(|e| e.host == KEY_B)
            .expect("hex peer persisted");
        assert_eq!(hex_peer.port, 24113);
        assert_eq!(hex_peer.services, 1);
        assert_eq!(hex_peer.source, "addr");
        assert_eq!(hex_peer.last_ok, 1100);
        assert_eq!(hex_peer.ok_count, 1);
        assert_eq!(hex_peer.fail_count, 0);
        let seed_peer = loaded.peers().iter().find(|e| e.source == "seed").unwrap();
        assert_eq!(seed_peer.fail_count, 2);
        assert_eq!(seed_peer.last_fail, 1060);
        let _ = std::fs::remove_file(&path);
    }

    /// (item 34) set_self PURGES poisoned self rows: a store that learned
    /// our own onion via gossip BEFORE the identity was registered (the
    /// DNS2 self-dial shape — 494+65 failed dials to its own onion) must
    /// lose the row at the next registration, and the ingest filter must
    /// then hold. Non-self rows survive untouched.
    #[test]
    fn set_self_purges_existing_self_entries_from_the_store() {
        const OWN: &str = "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion";
        let mut store = PeerStore::default();
        // poisoned BEFORE identity registration: the gossip of ourselves
        // enters the store on both fleet ports (:24113 AND :24112)
        assert!(store.upsert(OWN, 24113, 1, "addr", 1000));
        assert!(store.upsert(OWN, 24112, 1, "addr", 1001));
        assert!(store.upsert(KEY_B, 24113, 1, "addr", 1002));
        assert_eq!(store.len(), 3);
        // registration purges both poisoned rows, keeps the healthy one
        let removed = store.set_self(&[OWN.to_string()]);
        assert_eq!(removed, 2, "both self rows purged");
        assert_eq!(store.len(), 1);
        assert!(!store.contains(OWN, 24113));
        assert!(!store.contains(OWN, 24112));
        assert!(store.contains(KEY_B, 24113));
        // the ingest filter now refuses the same host at both ports
        assert!(!store.upsert(OWN, 24113, 1, "addr", 1003));
        assert!(!store.upsert(OWN, 24112, 1, "addr", 1004));
        assert_eq!(store.len(), 1);
        // re-registration of the same identity is a no-op (nothing to purge)
        assert_eq!(store.set_self(&[OWN.to_string()]), 0);
    }

    /// A5: a corrupt file yields an EMPTY store plus a log line — never an
    /// abort, never partial data.
    #[test]
    fn corrupt_file_gives_empty_store_and_log() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, "{ not json !!!").unwrap();
        let store = PeerStore::load(&path);
        assert!(store.is_empty());
        // valid JSON but wrong shape is equally non-fatal
        std::fs::write(&path, "[1,2,3]").unwrap();
        assert!(PeerStore::load(&path).is_empty());
        // a missing file is just an empty store with no drama
        let _ = std::fs::remove_file(&path);
        assert!(PeerStore::load(&path).is_empty());
    }

    /// Gossip amplification (peers.rs): the coalesced save writes at most
    /// once per `PEER_PERSIST_INTERVAL` and a shutdown flush persists whatever
    /// is still pending — so a burst of addr/version merges costs ONE rewrite
    /// per window instead of one per message.
    #[test]
    fn coalesced_save_writes_once_per_window_and_flushes_pending() {
        let path = tmp_path("coalesce");
        let mut store = PeerStore::default();
        store.upsert(KEY_A, 24113, 1, "addr", 1000);
        assert!(
            store.save_throttled(&path).unwrap(),
            "the first dirty save must write"
        );
        let first = std::fs::read(&path).unwrap();
        store.upsert(KEY_B, 24113, 1, "addr", 1001);
        assert!(
            !store.save_throttled(&path).unwrap(),
            "a second save inside the 30s window is deferred, not written"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            first,
            "the file must be untouched inside the coalescing window"
        );
        // shutdown-drop path: flush persists the pending merge
        store.flush().unwrap();
        let reloaded = PeerStore::load(&path);
        assert!(reloaded.contains(KEY_B, 24113), "flushed merge reaches disk");
        let _ = std::fs::remove_file(&path);
    }

    /// Dial ordering: best-seen first, then fresh, then seeds; failing
    /// peers drop into exponential backoff and come back after it lapses.
    #[test]
    fn dial_order_best_seen_first_then_seeds_with_backoff() {
        let mut store = PeerStore::default();
        // A: answered recently (best)
        store.upsert(KEY_A, 24113, 1, "addr", 100);
        store.record_ok(KEY_A, 24113, 5000);
        // B: answered long ago
        store.upsert(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion",
            24113,
            1,
            "addr",
            100,
        );
        store.record_ok("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24113, 1000);
        // C: never answered, one failure (ready: backoff 2s < 4000-2000)
        let c = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb7777.onion";
        store.upsert(c, 24113, 1, "addr", 100);
        store.record_fail(c, 24113, 2000);
        // D: never answered, deep backoff (fail_count 5 → 32s; now 4000, last_fail 3990 → skipped)
        let d = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc9999.onion";
        store.upsert(d, 24113, 1, "addr", 100);
        for _ in 0..5 {
            store.record_fail(d, 24113, 3990);
        }
        let seeds = vec![(
            "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion".to_string(),
            24113u16,
        )];
        let order = store.dial_order(4000, &seeds);
        let names: Vec<&str> = order.iter().map(|(h, _)| h.as_str()).collect();
        // best-seen: A (most recent ok) before B; C ready; D skipped (backoff); seed appended
        assert_eq!(names[0], KEY_A);
        assert_eq!(names[1], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion");
        assert_eq!(names[2], c);
        assert!(names.contains(&"gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion"));
        assert!(!names.contains(&d), "backed-off peer must be skipped");
        // fresh peers with a configured seed sort seed-first in the fresh band
        let seed_peer = {
            let mut s = PeerStore::default();
            s.upsert(&seeds[0].0, 24113, 1, "seed", 100);
            s.upsert(c, 24113, 1, "addr", 100);
            s
        };
        let order = seed_peer.dial_order(4000, &seeds);
        assert_eq!(order[0].0, seeds[0].0, "seed precedes unknown failures");
        // after the backoff window the skipped peer returns
        let order = store.dial_order(3990 + 32 + 1, &seeds);
        assert!(order.iter().any(|(h, _)| h == d), "backoff lapses");
    }

    /// addr merge: tor_v3 entries land keyed by their reconstructed HOSTNAME,
    /// self announcements and cleartext gossips are skipped, and the store
    /// re-serves them as wire addr entries.
    #[test]
    fn merge_addr_skips_self_and_cleartext_and_serves_back() {
        let mut store = PeerStore::default();
        let onion_a = WireAddr::onion_v3(
            crate::p2p::hex_decode(KEY_A).unwrap().try_into().unwrap(),
            24113,
            1,
        );
        let host_a = onion_a.onion_hostname().unwrap();
        store.set_self(&[host_a]);
        let onion_b = WireAddr::onion_v3([0x99u8; 32], 24113, 1);
        let host_b = onion_b.onion_hostname().unwrap();
        let clear = WireAddr::ipv4(10, 0, 0, 1, 24113, 1);
        let entries = vec![
            AddrEntry { time: 10, addr: onion_a.clone() }, // self → skipped
            AddrEntry { time: 11, addr: onion_b.clone() }, // kept
            AddrEntry { time: 12, addr: clear },           // Tor-only → skipped
        ];
        let merged = store.merge_addr(&entries, 100);
        assert_eq!(merged, 1);
        assert_eq!(store.len(), 1);
        // the stored entry IS B's reconstructed hostname (dialable), with
        // B's port
        assert_eq!(store.peers()[0].host, host_b);
        assert!(store.peers()[0].host.ends_with(".onion"));
        assert_eq!(store.peers()[0].port, 24113);
        // re-serving yields exactly the kept peer in wire form
        let served = store.addr_entries(200, 1000);
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].addr, onion_b);
        assert_eq!(served[0].addr.store_key().unwrap(), host_b);
        // merge is idempotent by host:port
        let merged = store.merge_addr(&entries, 300);
        assert_eq!(merged, 1);
        assert_eq!(store.len(), 1);
    }

    /// END-TO-END discovery (BRIEF_TOR_ROUND2 item 4): node B's onion at
    /// port 24113 travels as an `addr` PAYLOAD (real wire bytes), node A
    /// parses and merges it, and A's stored entry AND dial plan then contain
    /// B's `.onion:24113` hostname — i.e. a dial is now possible. A's own
    /// announcement is skipped.
    #[test]
    fn discovery_end_to_end_addr_payload_yields_dialable_onion() {
        const SEED: &str = "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion";
        // Node B: a real v3 onion publishing the fleet port. Build B's wire
        // address from its published hostname (as B itself would).
        let b = WireAddr::from_onion_label(SEED, 24113, crate::p2p::NODE_NETWORK).unwrap();
        let host_b = b.onion_hostname().unwrap();
        assert_eq!(host_b, SEED);

        // Node A: own identity is another v3 onion on the same port.
        let a = WireAddr::onion_v3([0x2au8; 32], 24113, crate::p2p::NODE_NETWORK);
        let host_a = a.onion_hostname().unwrap();
        let mut store_a = PeerStore::default();
        store_a.set_self(&[host_a.clone()]);

        // B's addr message on the wire (encode → the bytes that travel).
        let payload = crate::p2p::encode_addr(&[AddrEntry {
            time: 1_792_222_222,
            addr: b.clone(),
        }]);
        // A receives and parses the payload (legacy codec, not a struct copy)
        let parsed = crate::p2p::parse_addr(&payload).expect("addr payload parses");
        assert_eq!(parsed.len(), 1);

        // A merges the gossip into its peer store
        let merged = store_a.merge_addr(&parsed, 1_000);
        assert_eq!(merged, 1, "B's entry must merge");
        assert_eq!(store_a.len(), 1);
        // stored entry is B's hostname at B's port — dialable form
        assert_eq!(store_a.peers()[0].host, SEED);
        assert_eq!(store_a.peers()[0].port, 24113);
        assert!(store_a.peers()[0].host.ends_with(".onion"));

        // A's own announcement (address bookkeeping, e.g. a reflected
        // self-advertisement) is skipped
        let self_gossip = vec![AddrEntry {
            time: 1_100,
            addr: a.clone(),
        }];
        assert_eq!(store_a.merge_addr(&self_gossip, 1_100), 0, "self skipped");
        assert_eq!(store_a.len(), 1);

        // A's dial plan (the order the dial loop walks) contains B
        let plan = store_a.dial_order(2_000, &[]);
        assert_eq!(
            plan[0],
            (SEED.to_string(), 24113u16),
            "dial plan must contain B's .onion:24113"
        );
        assert!(plan.iter().all(|(h, _)| h != &host_a), "A never dials itself");

        // and B survives a store save/load (discovery persists restarts)
        let path = tmp_path("e2e-discovery");
        store_a.save(&path).unwrap();
        let reloaded = PeerStore::load(&path);
        assert!(
            reloaded
                .dial_order(2_100, &[])
                .iter()
                .any(|(h, p)| h == SEED && *p == 24113),
            "B's onion:24113 survives the reload"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// PEER-HEALTH: the dial order ranks by RECENT SERVE activity — a peer
    /// that handshook a second ago but never served blocks must sort BELOW a
    /// peer that served a while back (the "100% to the never-serving peer"
    /// shape). Served counters also round-trip the store.
    #[test]
    fn dial_order_prefers_serving_peers_over_handshake_only() {
        let mut store = PeerStore::default();
        let server = "serveronlyserveronlyserveronlyserveronlyserver5555.onion";
        let talker = "talkertalkertalkertalkertalkertalkertalker7777.onion";
        store.upsert(server, 24113, 1, "addr", 100);
        store.upsert(talker, 24113, 1, "addr", 100);
        // both handshook; the talker far more recently
        store.record_ok(server, 24113, 1_000);
        store.record_ok(talker, 24113, 9_000);
        // only the server actually produced blocks
        store.record_served(server, 24113, 2_000, 64, 64 * 1024);
        let order = store.dial_order(9_100, &[]);
        assert_eq!(
            order[0].0, server,
            "the peer that SERVES must lead: {order:?}"
        );
        // served counters persist
        let path = tmp_path("peer-health");
        store.save(&path).unwrap();
        let loaded = PeerStore::load(&path);
        let e = loaded.peers().iter().find(|e| e.host == server).unwrap();
        assert_eq!(e.served_blocks, 64);
        assert_eq!(e.served_bytes, 64 * 1024);
        assert_eq!(e.last_served, 2_000);
        let _ = std::fs::remove_file(&path);
        // record_unserved feeds the same backoff the dial order honours
        store.record_unserved(talker, 24113, 9_100);
        assert!(store.peers().iter().any(|e| e.host == talker && e.fail_count == 1));
    }

    /// The 1000-entry cap holds on save and on upsert.
    #[test]
    fn store_cap_enforced() {
        let mut store = PeerStore::default();
        for i in 0..(PEER_STORE_CAP + 10) {
            let host = format!("{:064x}", i);
            store.upsert(&host, 24113, 1, "addr", 100);
        }
        assert_eq!(store.len(), PEER_STORE_CAP);
        // best (answered) entries survive eviction: mark one OK after the fill
        store.record_ok(&format!("{:064x}", 5), 24113, 1000);
        let served = store.addr_entries(2000, 1000);
        assert!(served.len() <= 1000);
    }

    /// addnode add/remove semantics over the store: `contains` drives the
    /// legacy "Node already added." check, `remove` drops ONLY the named
    /// endpoint and reports whether it was present ("Node not found in
    /// addnode list.").
    #[test]
    fn addnode_add_remove_on_the_store() {
        let mut store = PeerStore::default();
        store.upsert("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24113, 1, "addnode", 100);
        store.upsert("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb7777.onion", 24113, 1, "addr", 100);
        // add of an existing endpoint is the legacy duplicate error
        assert!(store.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24113));
        assert!(!store.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24000));
        // remove is per host AND port, and reports presence
        assert!(store.remove("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24113));
        assert!(!store.remove("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa5555.onion", 24113));
        assert_eq!(store.len(), 1);
        assert_eq!(store.peers()[0].host, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb7777.onion");
    }

    /// HIGH-4a (dual-stack directive): `merge_addr` accepts i2p (GarliCat /
    /// is_i2p) gossip alongside tor_v3 — keyed by the reconstructed
    /// `.b32.i2p` hostname, re-servable over getaddr — while cleartext IP
    /// gossip stays rejected. Without the i2p arm every wire-learned i2p
    /// peer is silently discarded and discovery is tor-only.
    #[test]
    fn merge_addr_accepts_i2p_and_tor_rejects_clearnet() {
        let mut store = PeerStore::default();
        // an i2p identity: hash → wire form (as a gossiper would encode it)
        let mut hash = [0u8; 32];
        for (i, b) in hash.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x33;
        }
        let i2p_addr = WireAddr::i2p_from_hash(&hash, 24113, crate::p2p::NODE_NETWORK);
        let i2p_host = i2p_addr.i2p_hostname().unwrap();
        assert!(i2p_host.ends_with(".b32.i2p"));
        // a tor_v3 identity (still accepted)
        let onion = WireAddr::onion_v3([0x44u8; 32], 24113, crate::p2p::NODE_NETWORK);
        let onion_host = onion.onion_hostname().unwrap();
        // a cleartext quad (still rejected — overlay-only network)
        let clear = WireAddr::ipv4(192, 168, 1, 5, 24113, crate::p2p::NODE_NETWORK);
        let merged = store.merge_addr(
            &[
                AddrEntry { time: 1, addr: i2p_addr.clone() },
                AddrEntry { time: 2, addr: onion.clone() },
                AddrEntry { time: 3, addr: clear },
            ],
            100,
        );
        assert_eq!(merged, 2, "i2p + tor merge, clearnet rejected");
        assert_eq!(store.len(), 2);
        let hosts: Vec<&str> = store.peers().iter().map(|e| e.host.as_str()).collect();
        assert!(hosts.contains(&i2p_host.as_str()), "i2p keyed by b32 hostname");
        assert!(hosts.contains(&onion_host.as_str()));
        // re-served over getaddr in wire form (the from_store_key inverse)
        let served = store.addr_entries(200, 1000);
        assert_eq!(served.len(), 2);
        assert!(
            served.iter().any(|e| e.addr == i2p_addr),
            "i2p entry re-encodes to the same wire form"
        );
        // and the dial plan carries the dialable b32 hostname
        let plan = store.dial_order(300, &[]);
        assert!(plan.iter().any(|(h, _)| *h == i2p_host));
    }
}
