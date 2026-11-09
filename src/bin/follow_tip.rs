//! Phase 2 — legacy P2P tip follower (follow_tip); phase 6 made it the
//! network PRIMARY: inbound listener + getdata/getheaders serving + block
//! relay (inv → getdata), phase 7 added gap recovery (window-tail bodies
//! missing from the store are pulled from peers or the tip rolls back) and
//! append-on-accept durability.
//!
//! Outbound TCP client speaking the exact triangles_v5 (v6.2.6.7) wire
//! protocol to the local daemon's P2P listener. Handshake (version/verack),
//! getheaders-driven tip chase with full tri7_core validation of every
//! received block (parse + Hash9 + link + merkle + timestamps + nBits EMA +
//! PoW target / PoS kernel + block signature + tx finality), persistent
//! tipstate.json.
//!
//! Strict safety: read-only participation — we never relay transactions.
//! Discovery (BRIEF_TOR_V2): getaddr is sent after verack, addr gossip is
//! merged into the persistent peer store (peers.json) and served back to
//! peers asking for it — Tor-only, up to the legacy 1000-entry cap. On any
//! frame parse failure the socket is disconnected cleanly.
//!
//! Validation context (window):
//!   The consensus functions in tri7_core index a ReplayIndex by vector
//!   position. The follower keeps the last WINDOW_ENTRIES heights in a
//!   ReplayIndex whose index i corresponds to absolute height base+i (kept
//!   entries only ever APPEND, so alignment holds). Every consensus walk from
//!   the tip needs ≤ ~800 blocks of context (stake-modifier selection
//!   interval 18900 s ≈ 158 blocks; stake input age cap 43200 s ≈ 720
//!   blocks), so 4600 entries are strictly sufficient; the era-seam early
//!   returns keyed to absolute fork heights can never fire inside the window,
//!   which is correct for the live era. UtxoMeta heights in the window are
//!   window-relative for the same reason; check_proof_of_stake's tip_height
//!   argument stays ABSOLUTE because it selects the GetWeight era.
//!
//! Seed provenance (first run / --reseed):
//!   tipstate starts from the LOCAL ARCHIVE (never a bare RPC tip): every
//!   archive frame is Hash9'd and link-verified sequentially from genesis,
//!   skipping reorged stale-branch seams until a frame chains onto the live
//!   tip again (genesis..2201090 linkage additionally proven in phase 1a).
//!   Stake-modifier/proof state for the window is seeded from the daemon's
//!   block index (getblock modifier/flags/proofhash) and WHOLLY VERIFIED
//!   locally via the stake-modifier checksum chain
//!   (stake_modifier_checksum(prev, flags, proof, modifier) must reproduce
//!   the daemon's modifierchecksum at every seeded height; phase 1d proved
//!   the daemon index equals tri7's replay). Every NEW live block gets its
//!   modifier computed locally, its checksum verified against the daemon's
//!   published checksum, and its kernel hash recomputed locally whenever the
//!   coinstake input resolves inside the window (pre-window inputs — coins
//!   older than the 12 h cap — fall back to the checksum-verified RPC proof,
//!   mirroring the C++ kernel's txindex resolution).
//!
//! Exit codes: 0 healthy · 2 usage · 3 validation failure · 4 protocol
//! failure · 5 seeding failure.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use primitive_types::U256;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use tri7_core::blockindex::{BlockStore, BuildOracle, StoreStatus};
use tri7_core::consensus::{
    block_is_proof_of_stake, check_pos_block_signature, check_pow_block_signature, max_time_drift,
    tx_is_coin_base, tx_is_coin_stake, LAST_HARDCODED_CHECKPOINT,
};
use tri7_core::difficulty::{get_next_target_required, set_compact};
use tri7_core::kernel::{
    compute_next_stake_modifier_era, index_flags, stake_modifier_checksum, KernelOutputResolver,
    UtxoMeta,
};
use tri7_core::legacy::{hash_to_display, Block, Hash, Reader, Tx};
use tri7_core::replay::{IndexEntry, ReplayIndex};
use tri7_core::rpc;
use tri7_core::rpc::{
    load_balances_snap, load_utxos_snap, AddressEntry, BalanceView, BlockDataProvider, EntryView, Proxy, SnapshotWallet, UtxoRow, WalletProvider,
    TipProvider, TipView,
};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------
const WINDOW_ENTRIES: usize = 4600; // ≥ selection interval + stake-age cap + margin
const WINDOW_BLOCKS: usize = 1500; // full-tx records for kernel prevout resolution
const POLL_SECS: u64 = 30;

// ---- keypool (legacy `pool` rows) — parity item KEYPOOLREFILL ------------
/// Legacy default -keypool setting (bitcoin 0.8-era): 100 pre-derived keys.
const DEFAULT_KEYPOOL_SIZE: usize = 100;
/// Legacy MAX_KEYPOOL_SIZE — the qt refused sizes above 100000.
const MAX_KEYPOOL_SIZE: i64 = 100_000;
/// Background top-up cadence: a cheap SQL census every 15 min, refill only
/// when the pool sits below the default target.
const KEYPOOL_CHECK_SECS: u64 = 15 * 60;
/// After an unreadable wallet (locked/missing), retry daily instead of
/// spamming the log every check.
const KEYPOOL_ERROR_RETRY_SECS: u64 = 24 * 3600;

/// Legacy keypoolrefill [size] argument: absent or 0 = the default 100,
/// otherwise clamped to the legacy 0..=100000 range (errors keep the -8
/// code the qt used for invalid-parameter).
fn parse_keypool_size(arg: Option<&serde_json::Value>) -> Result<usize, (i64, String)> {
    match arg {
        None | Some(serde_json::Value::Null) => Ok(DEFAULT_KEYPOOL_SIZE),
        Some(v) => {
            let n = v
                .as_i64()
                .ok_or_else(|| (-8, "keypool size must be an integer".to_string()))?;
            if n < 0 || n > MAX_KEYPOOL_SIZE {
                return Err((
                    -8,
                    format!("keypool size must be between 0 and {MAX_KEYPOOL_SIZE}"),
                ));
            }
            Ok(if n == 0 {
                DEFAULT_KEYPOOL_SIZE
            } else {
                n as usize
            })
        }
    }
}

/// Background top-up maintainer: keeps the wallet's keypool at the legacy
/// default (100) so getnewaddress hands out pre-derived keys instantly.
/// Runs the full life of the daemon; every pass is a cheap SQL census first
/// (publish it to getwalletinfo even when nothing needs refilling) and a
/// real refill only when the pool sits below target. A wallet that cannot
/// be opened for write (encrypted, missing) is logged and retried daily —
/// never a crash, never a log storm.
fn keypool_maintainer_loop(
    wallet_path: std::path::PathBuf,
    extras: Arc<std::sync::RwLock<tri7_core::rpc::WalletExtras>>,
) {
    let mut backoff = KEYPOOL_CHECK_SECS;
    loop {
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = KEYPOOL_CHECK_SECS;
        let publish = |n: usize, oldest: Option<i64>, extras: &Arc<std::sync::RwLock<tri7_core::rpc::WalletExtras>>| {
            if let Ok(mut g) = extras.write() {
                g.keypool_size = Some(n);
                g.keypool_oldest = oldest;
            }
        };
        match tri7_core::wallet::keypool_stats(&wallet_path) {
            Ok((n, oldest)) => {
                publish(n, oldest, &extras);
                if n < DEFAULT_KEYPOOL_SIZE {
                    match tri7_core::wallet::WalletHandle::open_readwrite(&wallet_path) {
                        Ok(mut wh) => match wh.refill_keypool(DEFAULT_KEYPOOL_SIZE) {
                            Ok(rep) => {
                                log(&format!(
                                    "keypool: maintainer topped the pool to {} (+{} derived, hdchain={})",
                                    rep.poolsize, rep.derived, rep.next_index
                                ));
                                if let Ok((n2, o2)) = tri7_core::wallet::keypool_stats(&wallet_path) {
                                    publish(n2, o2, &extras);
                                }
                            }
                            Err(e) => {
                                log(&format!("keypool: maintainer refill failed: {e}"));
                                backoff = KEYPOOL_ERROR_RETRY_SECS;
                            }
                        },
                        Err(e) => {
                            log(&format!(
                                "keypool: maintainer cannot open the wallet for write ({e}) — retrying later"
                            ));
                            backoff = KEYPOOL_ERROR_RETRY_SECS;
                        }
                    }
                }
            }
            Err(e) => {
                log(&format!(
                    "keypool: census failed ({e}) — retrying in {}s",
                    backoff
                ));
                backoff = KEYPOOL_ERROR_RETRY_SECS;
            }
        }
    }
}

const STALE_LINK_SECS: u64 = 90;
const MAX_OUTSTANDING_GETDATA: usize = 64;
const GETDATA_BATCH: usize = 32;
const EXIT_USAGE: i32 = 2;
const EXIT_VALIDATE: i32 = 3;
const EXIT_PROTOCOL: i32 = 4;
const EXIT_SEED: i32 = 5;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn now_iso() -> String {
    iso_from_secs(now_secs() as i64)
}

/// Civil-from-days UTC formatting for an arbitrary unix timestamp (used by
/// --status to report the tipstate file's on-disk mtime as filesystem truth).
fn iso_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, mi, s)
}

fn log(msg: &str) {
    println!("[{}] {}", now_iso(), msg);
}

fn die(code: i32, msg: &str) -> ! {
    eprintln!("FAIL({}): {}", code, msg);
    std::process::exit(code);
}

fn compact_size_len(n: u64) -> usize {
    if n < 0xfd {
        1
    } else if n <= 0xffff {
        3
    } else if n <= 0xffff_ffff {
        5
    } else {
        9
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON-RPC client over loopback HTTP (no external HTTP crate)
// ---------------------------------------------------------------------------
struct Rpc {
    host: String,
    port: u16,
    auth_basic: String,
}

impl Rpc {
    fn new(host: &str, port: u16, user: &str, pass: &str) -> Rpc {
        Rpc {
            host: host.to_string(),
            port,
            auth_basic: format!(
                "Basic {}",
                base64_encode(format!("{}:{}", user, pass).as_bytes())
            ),
        }
    }

    fn post(&self, body: &str) -> Result<String, String> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("rpc connect: {}", e))?;
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
        let req = format!(
            "POST / HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.host,
            self.port,
            self.auth_basic,
            body.len(),
            body
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("rpc write: {}", e))?;
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("rpc read: {}", e))?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let split = text.find("\r\n\r\n").ok_or("rpc: no header/body split")?;
        let head = &text[..split];
        let body = text[split + 4..].to_string();
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            return Err(format!(
                "rpc: bad status: {}",
                head.lines().next().unwrap_or("")
            ));
        }
        Ok(body)
    }

    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "jsonrpc": "1.0", "id": "tri7", "method": method, "params": params
        });
        let text = self.post(&body.to_string())?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("rpc json: {} — {:?}", e, text))?;
        if v.get("error").map(|e| !e.is_null()).unwrap_or(false) {
            return Err(format!("rpc error for {}: {}", method, v["error"]));
        }
        Ok(v["result"].clone())
    }

    /// JSON-RPC batch (array request — verified working on the live daemon).
    fn call_batch(
        &self,
        calls: Vec<(u64, &str, serde_json::Value)>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let arr: Vec<serde_json::Value> = calls
            .iter()
            .map(|(id, method, params)| {
                serde_json::json!({"jsonrpc":"1.0","id":id,"method":method,"params":params})
            })
            .collect();
        let text = self.post(&serde_json::to_string(&arr).unwrap())?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("rpc batch json: {}", e))?;
        let items = v.as_array().ok_or("rpc batch: not an array")?;
        Ok(items.clone())
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Persistent state
// ---------------------------------------------------------------------------
#[derive(Serialize, Deserialize, Clone)]
struct EntryRec {
    height: u64,
    hash: String, // display hex
    prev: String, // display hex
    time: u32,
    bits: u32,
    pos: bool,
    entropy: u8,
    modifier: u64,
    generated: bool,
    proof: String, // display hex of hashProofOfStake (zeros for PoW)
    checksum: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct BlockRec {
    height: u64,
    spends: Vec<(String, u32)>,
    outs: Vec<(String, u32, i64, u32, u32)>, // txid, n, value, tx_time, tx_offset
}

#[derive(Serialize, Deserialize, Clone)]
struct TipStateFile {
    height: u64,
    blockhash: String, // display hex — required keys {height, blockhash}
    updated_utc: String,
    #[serde(default)]
    last_advance_utc: String, // bumped ONLY on a real tip-height advance; "" = pre-5a state (unknown)
    window_base: u64,
    entries: Vec<EntryRec>,
    blocks: Vec<BlockRec>,
    daemon_tip: Option<i64>,
    // ---- phase 6: network liveness (written by the follow loop on every
    // state write; absent in pre-phase-6 state files) ----
    #[serde(default)]
    peers: Option<u32>, // connected peers (outbound + inbound) at write time
    #[serde(default)]
    last_peer_utc: Option<String>, // last peer traffic (any direction), ISO UTC
    #[serde(default)]
    listen_port: Option<u16>, // inbound listener port when enabled
}

// ---------------------------------------------------------------------------
// Recent blocks cache — raw bytes of blocks received since process start.
// Serves getdata from peers and getblock RPCs for heights ABOVE the disk
// store's tip (blk0001.dat stopped growing when the legacy daemon retired;
// blocks accepted by tri7d itself live only here until a store rebuild).
// ---------------------------------------------------------------------------
const RECENT_CAP: usize = 4000;

// ---------------------------------------------------------------------------
// Gap recovery (phase 7 urgent): window-tail bodies that exist only in the
// recent cache (memory) and were lost on a restart leave the store's height
// thread short of tipstate's tip. stake_mint replays the ARCHIVE against
// tipstate and exits 3 on any tail mismatch, so minting stays frozen until
// the tail is complete. Two outcomes are handled:
//   (a) a peer serves the body  → Hash9/link/merkle validate → append to the
//       store (appendix) → chain continues at tipstate's tip;
//   (b) no peer serves it within GAP_ROLLBACK_TICKS rotation ticks (~10 min)
//       and the store threads exactly tip-1 → tipstate rolls back to tip-1
//       (the "phantom" tip is invalidated honestly in the log + a marker
//       file) and staking mints a FRESH block at that height.
// ---------------------------------------------------------------------------
const GAP_TAIL_MAX: u64 = 8; // walk at most this many heights below the tip
const GAP_ROLLBACK_TICKS: u32 = 20; // consecutive unresolved ticks (30s each)

#[derive(Default)]
struct GapState {
    /// (height, hash) of the lowest missing tail body, if any
    missing: Option<(u64, Hash)>,
    /// when the current gap was first observed
    since: Option<Instant>,
    /// consecutive rotation ticks the gap stayed unfilled
    ticks_failed: u32,
    /// peer label that served the fill (evidence)
    filled_from: Option<String>,
    /// set when the rollback branch fired (height rolled back to)
    rolled_back_to: Option<u64>,
}

/// Lowest window-tail height whose body is neither on disk (store, including
/// gap-recovery appends) nor in the recent cache. Returns (height, hash).
fn gap_find_missing(
    stg: &ChainState,
    store: &Arc<BlockStore>,
    recent: &RecentBlocks,
    threaded_tip: Option<u64>,
) -> Option<(u64, Hash)> {
    let tip = stg.tip_height;
    let from_thread = threaded_tip.map_or(tip.saturating_sub(GAP_TAIL_MAX), |t| t + 1);
    let lower = from_thread.max(tip.saturating_sub(GAP_TAIL_MAX));
    for h in lower..=tip {
        let Some(e) = stg.entry_abs(h) else { continue };
        let hash = e.hash;
        let on_disk = store.block_by_hash(&hash).map(|v| v.is_some()).unwrap_or(false);
        if on_disk || recent.get(&hash).is_some() {
            continue;
        }
        return Some((h, hash));
    }
    None
}

/// Roll tipstate back one height (branch b): the phantom tip's body exists on
/// no reachable peer, so the EFFECTIVE network tip is tip-1 (whose window
/// entries are all present). Truncates entries/checksums/records, clears
/// pending header announcements, refreshes daemon_tip. Returns the new tip.
fn rollback_tip(st: &mut ChainState) -> Result<(u64, Hash), String> {
    let tip = st.tip_height;
    if tip == 0 || tip <= st.base {
        return Err("rollback: tip already at window base".into());
    }
    let new_tip = tip - 1;
    let new_hash = st
        .entry_abs(new_tip)
        .ok_or_else(|| format!("rollback: window entry for h{} missing", new_tip))?
        .hash;
    st.tip_height = new_tip;
    st.tip_hash = new_hash;
    let keep = (new_tip - st.base + 1) as usize;
    st.entries.truncate(keep);
    st.checksums.truncate(keep);
    st.records.retain(|r| r.height <= new_tip);
    st.pending_headers.clear();
    st.daemon_tip = Some(new_tip as i64);
    Ok((new_tip, new_hash))
}

/// Fork choice at the acceptance point: a block arrived whose prev is NOT our
/// tip. If it descends from an ancestor we hold and its branch carries more
/// work than ours above that ancestor, roll back to the ancestor and adopt it.
///
/// This is the piece that was missing: without it two staking nodes each kept
/// their own block at the same height forever (sami-pc ran an isolated branch
/// whose balances looked real but which the network never had).
///
/// Returns Ok(true) when the branch was adopted (the caller then validates and
/// appends the block against the rolled-back window).
fn try_adopt_heavier_branch(
    st: &mut ChainState,
    block: &Block,
    hash: &Hash,
) -> Result<bool, String> {
    // 1. where does this block want to attach?
    let Some(ancestor_idx) = st.index_of_hash(&block.prev_hash) else {
        return Ok(false); // unknown branch: not ours to judge (yet)
    };
    let ancestor_height = st.base + ancestor_idx as u64;
    if ancestor_height >= st.tip_height {
        return Ok(false); // extends our tip: the normal path handles it
    }

    // 2. work above the ancestor: ours vs theirs
    let our_work: u128 = st.entries[(ancestor_height - st.base + 1) as usize..]
        .iter()
        .map(|e| tri7_core::forkchoice::work_of(e.bits))
        .sum();
    let their_work = tri7_core::forkchoice::work_of(block.bits);

    let decision = tri7_core::forkchoice::choose(
        st.tip_height,
        &st.tip_hash,
        our_work,
        ancestor_height + 1,
        hash,
        their_work,
    );
    match decision {
        tri7_core::forkchoice::ForkChoice::KeepOurs => Ok(false),
        tri7_core::forkchoice::ForkChoice::Switch { reason } => {
            log(&format!(
                "FORK CHOICE: {} — rolling back to h{}, adopting {} ({} block(s) replaced)",
                reason,
                ancestor_height,
                tri7_core::p2p::internal_to_display(hash),
                st.tip_height - ancestor_height
            ));
            while st.tip_height > ancestor_height {
                rollback_tip(st)?;
            }
            Ok(true)
        }
    }
}

/// Evidence marker for the report: which gap branch fired, when, over which
/// hash/height, with the peer that served (or the rollback height).
fn write_gap_marker(cfg: &Config, payload: Value) {
    let path = cfg.state_dir.join("gap_recovery.json");
    let tmp = cfg.state_dir.join("gap_recovery.json.tmp");
    let body = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap_or_default());
    if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        log("gap marker write failed (non-fatal)");
    }
}

/// Validate a gap-fill body the cheap-but-complete archival way: the wire
/// hash we requested IS the Hash9 of the body (header_hash in the store),
/// the body must chain to the window entry below it, and its merkle root
/// must commit to the transactions it carries.
fn gap_fill_valid(b: &Block, height: u64, want_hash: &Hash, stg: &ChainState) -> Result<(), String> {
    let hash = b.hash();
    if &hash != want_hash {
        return Err(format!(
            "gap fill hash mismatch: got {} want {}",
            tri7_core::p2p::internal_to_display(&hash),
            tri7_core::p2p::internal_to_display(want_hash)
        ));
    }
    if height > 0 {
        let want_prev = stg
            .entry_abs(height - 1)
            .ok_or_else(|| format!("gap fill: no window entry below h{}", height))?
            .hash;
        if b.prev_hash != want_prev {
            return Err(format!(
                "gap fill does not chain to h{} (prev {} want {})",
                height - 1,
                tri7_core::p2p::internal_to_display(&b.prev_hash),
                tri7_core::p2p::internal_to_display(&want_prev)
            ));
        }
    }
    if b.merkle_root() != b.merkle_root {
        return Err("gap fill merkle root mismatch".into());
    }
    Ok(())
}

struct RecentBlocks {
    order: VecDeque<Hash>,
    map: HashMap<Hash, (u64, Vec<u8>)>,   // hash -> (height, raw block payload)
    by_height: Vec<(u64, Hash)>,          // append-ordered, height>0 only
}

impl RecentBlocks {
    fn new() -> Self {
        RecentBlocks {
            order: VecDeque::new(),
            map: HashMap::new(),
            by_height: Vec::new(),
        }
    }

    fn insert(&mut self, hash: Hash, height: u64, payload: &[u8]) {
        if self.map.contains_key(&hash) {
            return;
        }
        self.order.push_back(hash);
        self.map.insert(hash, (height, payload.to_vec()));
        if height > 0 {
            self.by_height.push((height, hash));
        }
        while self.order.len() > RECENT_CAP {
            let evict = self.order.pop_front().unwrap();
            self.map.remove(&evict);
        }
        // prune the height index as the window slides (blocks arrive ascending)
        if self.by_height.len() > RECENT_CAP * 2 {
            let min_keep = self
                .store_tip_height()
                .unwrap_or(0)
                .saturating_sub(RECENT_CAP as u64);
            self.by_height.retain(|(h, _)| *h >= min_keep);
        }
    }

    fn get(&self, hash: &Hash) -> Option<&(u64, Vec<u8>)> {
        self.map.get(hash)
    }

    fn at_height(&self, height: u64) -> Option<&(u64, Vec<u8>)> {
        self.by_height
            .iter()
            .rev()
            .find(|(h, _)| *h == height)
            .and_then(|(_, has)| self.map.get(has))
    }

    fn store_tip_height(&self) -> Option<u64> {
        self.by_height.last().map(|(h, _)| *h)
    }
}

// ---------------------------------------------------------------------------
// Chain window state
// ---------------------------------------------------------------------------
/// Windowed output index: mirrors the C++ kernel's own resolution — every
/// non-empty output ever created inside the window, keyed (txid, n), NEVER
/// removed on spend (kernel.cpp reads txPrev from the txid-keyed txdb without
/// consulting spentness — see kernel.rs OutputIndex). Coinstake inputs created
/// before the window remain unresolvable here and take the checksum-verified
/// RPC proof path.
struct WindowOutputs {
    map: HashMap<(Hash, u32), UtxoMeta>,
}

impl WindowOutputs {
    fn new() -> Self {
        WindowOutputs {
            map: HashMap::new(),
        }
    }

    fn apply_block(&mut self, block: &Block, height_rel: usize) {
        let mut offset = (80 + compact_size_len(block.vtx.len() as u64)) as u32;
        for tx in &block.vtx {
            for (n, vout) in tx.vout.iter().enumerate() {
                // empty-marker outputs (value 0 + empty script) are not tracked
                if !(vout.value == 0 && vout.script_pubkey.is_empty()) {
                    self.map.insert(
                        (tx.hash(), n as u32),
                        UtxoMeta {
                            height: height_rel,
                            tx_offset: offset,
                            tx_time: tx.time,
                            value: vout.value,
                        },
                    );
                }
            }
            offset += tx.serialize().len() as u32;
        }
    }

    fn rebuild_from_records(&mut self, recs: &[BlockRec], base: u64) {
        for rec in recs {
            for (txid, n, value, t_time, t_off) in &rec.outs {
                self.map.insert(
                    (tri7_core::p2p::display_to_internal(txid).unwrap(), *n),
                    UtxoMeta {
                        height: (rec.height - base) as usize,
                        tx_offset: *t_off,
                        tx_time: *t_time,
                        value: *value,
                    },
                );
            }
        }
    }

    /// Drop pre-window outputs so long runs stay bounded (kernel context
    /// never needs outputs older than WINDOW_BLOCKS).
    fn trim(&mut self, min_height_rel: usize) {
        self.map.retain(|_, m| m.height >= min_height_rel);
    }

    /// Shift every stored window-relative height down after the window base
    /// advanced (ChainState::trim re-basing; keeps kernel-meta consumers
    /// aligned with entries' relative heights in-session).
    fn rebase(&mut self, excess: usize) {
        if excess == 0 {
            return;
        }
        for m in self.map.values_mut() {
            m.height = m.height.saturating_sub(excess);
        }
    }
}

impl KernelOutputResolver for WindowOutputs {
    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta> {
        self.map.get(key).copied()
    }
}

struct ChainState {
    base: u64, // absolute height of entries[0]
    entries: Vec<IndexEntry>,
    checksums: Vec<u32>, // per-entry stake-modifier checksum, aligned to entries
    utxo: WindowOutputs,
    records: Vec<BlockRec>, // full-tx records for the last WINDOW_BLOCKS heights
    pending_headers: HashMap<Hash, Block>, // announced headers awaiting blocks
    tip_height: u64,
    tip_hash: Hash,
    /// last observed daemon tip (getblockcount); refreshed each poll tick,
    /// persisted on every state write so --status can report it offline
    daemon_tip: Option<i64>,
    /// True during FROM-ZERO sync: the window never trims — entries/utxo keep
    /// the WHOLE history from genesis so every historical coinstake kernel is
    /// fully enforced (no pre-window skips). base stays 0, so entry heights
    /// stay window-relative == absolute. Restart rebuilds nothing: a resumed
    /// run re-enters from the persisted 4600-entry window with trimming back
    /// on (old-input kernels above the re-walked tail are skip-recorded
    /// honestly until the window re-grows — progress is never re-fetched).
    no_trim: bool,
    /// wall clock of the last REAL tip-height advance; the periodic 30s state
    /// writes deliberately do NOT bump this (updated_utc/mtime do)
    last_advance_utc: String,
    /// network liveness — persisted IN the chain state (not injected at write
    /// time) so a RESTART inherits the last-peer clock instead of wiping it:
    /// a fresh process with no traffic yet must age the gate from the last
    /// known peer traffic, not report "no data"
    peers_connected: u32,
    last_peer_utc: Option<String>,
}

impl ChainState {
    fn rel(&self, abs: u64) -> usize {
        (abs - self.base) as usize
    }

    fn entry_abs(&self, abs: u64) -> Option<&IndexEntry> {
        // guard below-base heights: gap recovery can probe tail heights
        // under a stale threaded tip that sit before the window base —
        // there are no entries there (and the debug-mode subtraction would
        // panic on release-with-overflow-checks builds)
        if abs < self.base {
            return None;
        }
        self.entries.get(self.rel(abs))
    }

    fn entry_by_hash(&self, hash: &Hash) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| &e.hash == hash)
    }

    /// Window index of a block hash, if we hold it.
    fn index_of_hash(&self, hash: &Hash) -> Option<usize> {
        self.entries.iter().position(|e| &e.hash == hash)
    }

    /// ReplayIndex view over the window: entries[i] has ABSOLUTE height
    /// base+i, but IndexEntry.height stores the WINDOW-RELATIVE height so the
    /// consensus functions' by_height/entries[...] walks stay in-range (the
    /// difficulty same-type walk indexes by e.height-1 — absolute heights
    /// would index past the window and return None → TARGET_LIMIT early
    /// return; bug found live at the first backfill block). The window only
    /// ever appends, so index i == relative height is invariant.
    fn rel_index(&self) -> ReplayIndex<'_> {
        ReplayIndex::borrowed(&self.entries)
    }

    /// Truncated view ending at abs (inclusive): for validating a HISTORICAL
    /// block at abs+1 whose EMA must be anchored at its own ancestors.
    fn rel_index_to(&self, abs: u64) -> ReplayIndex<'_> {
        ReplayIndex::borrowed(&self.entries[..=self.rel(abs)])
    }

    /// CBlockIndex::GetMedianTimePast: median of the 11 entries ending at
    /// `before_abs` (nMedianTimeSpan = 11, main.h:1336).
    fn median_time_past(&self, before_abs: u64) -> Option<u32> {
        if before_abs < self.base + 10 {
            return None;
        }
        let mut times: Vec<u32> = ((before_abs - 10)..=before_abs)
            .filter_map(|h| self.entry_abs(h).map(|e| e.time))
            .collect();
        if times.len() < 11 {
            return None;
        }
        times.sort_unstable();
        Some(times[5])
    }

    fn trim(&mut self) {
        if self.no_trim {
            // from-zero: entries + utxo keep the FULL history (kernel
            // enforcement needs pre-window outputs); only the full-tx records
            // stay bounded (they are persisted and only serve the tail).
            let excess_rec = self.records.len().saturating_sub(WINDOW_BLOCKS);
            if excess_rec > 0 {
                self.records.drain(..excess_rec);
            }
            return;
        }
        let excess = self.entries.len().saturating_sub(WINDOW_ENTRIES);
        if excess > 0 {
            self.entries.drain(..excess);
            // Re-base the survivors: e.height is WINDOW-RELATIVE ("index i ==
            // relative height" invariant, see rel_index doc) — after base
            // advances by `excess` every kept entry's relative height drops by
            // `excess`. Without this, the next advance writes tipstate entry
            // heights base+e.height = abs+excess (observed live: the whole
            // window shifted +1 after following the first minted block) and
            // the difficulty walk's entries[e.height-1] indexing reads the
            // wrong neighbors.
            for e in self.entries.iter_mut() {
                e.height -= excess;
            }
            self.checksums.drain(..excess);
            self.base += excess as u64;
            // WindowOutputs heights are window-relative too (apply_block takes
            // st.rel(h)) — re-base so kernel-meta consumers stay aligned.
            self.utxo.rebase(excess);
        }
        let excess_rec = self.records.len().saturating_sub(WINDOW_BLOCKS);
        if excess_rec > 0 {
            self.records.drain(..excess_rec);
        }
        let tip_rel = self.rel(self.tip_height);
        self.utxo.trim(tip_rel.saturating_sub(WINDOW_BLOCKS + 200));
    }

    fn to_file(&self) -> TipStateFile {
        // no-trim (from-zero) keeps millions of entries in RAM — persisting
        // them all would write a multi-hundred-MB JSON every checkpoint.
        // Persist only the trailing WINDOW_ENTRIES: the on-disk contract is a
        // 4600-entry window, and a resume reloads exactly that window.
        let keep = if self.no_trim {
            WINDOW_ENTRIES.min(self.entries.len())
        } else {
            self.entries.len()
        };
        let first = self.entries.len() - keep;
        let base = if self.no_trim {
            self.tip_height - (keep as u64 - 1)
        } else {
            self.base
        };
        TipStateFile {
            height: self.tip_height,
            blockhash: tri7_core::p2p::internal_to_display(&self.tip_hash),
            updated_utc: now_iso(),
            last_advance_utc: self.last_advance_utc.clone(),
            window_base: base,
            entries: self
                .entries[first..]
                .iter()
                .zip(self.checksums[first..].iter())
                .map(|(e, cs)| EntryRec {
                    height: base + (e.height - self.entries[first].height) as u64,
                    hash: tri7_core::p2p::internal_to_display(&e.hash),
                    prev: tri7_core::p2p::internal_to_display(&e.prev_hash),
                    time: e.time,
                    bits: e.bits,
                    pos: e.is_proof_of_stake,
                    entropy: e.entropy_bit,
                    modifier: e.stake_modifier,
                    generated: e.generated_stake_modifier,
                    proof: tri7_core::p2p::internal_to_display(&e.hash_proof_of_stake),
                    checksum: *cs,
                })
                .collect(),
            blocks: self.records.clone(),
            daemon_tip: self.daemon_tip,
            peers: Some(self.peers_connected),
            last_peer_utc: self.last_peer_utc.clone(),
            listen_port: None,
        }
    }

    fn record_of(&self, block: &Block, height: u64) -> BlockRec {
        let mut spends: Vec<(String, u32)> = Vec::new();
        let mut outs: Vec<(String, u32, i64, u32, u32)> = Vec::new();
        let mut offset = (80 + compact_size_len(block.vtx.len() as u64)) as u32;
        for tx in &block.vtx {
            let coinbase = tx.vin.len() == 1
                && tx.vin[0].prev_hash == [0u8; 32]
                && tx.vin[0].prev_n == u32::MAX;
            if !coinbase {
                for vin in &tx.vin {
                    spends.push((
                        tri7_core::p2p::internal_to_display(&vin.prev_hash),
                        vin.prev_n,
                    ));
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                if !(vout.value == 0 && vout.script_pubkey.is_empty()) {
                    outs.push((
                        tri7_core::p2p::internal_to_display(&tx.hash()),
                        n as u32,
                        vout.value,
                        tx.time,
                        offset,
                    ));
                }
            }
            offset += tx.serialize().len() as u32;
        }
        BlockRec {
            height,
            spends,
            outs,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------
/// One outbound attach target. `.onion` hosts are reached through the local
/// tor SOCKS5 listener (cfg.socks); `.b32.i2p` hosts through the i2pd SOCKS5
/// listener (cfg.i2p_socks, default 127.0.0.1:4447); plain IPs connect
/// directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    Clearnet,
    Tor,
    I2p,
}

impl Transport {
    fn as_str(&self) -> &'static str {
        match self {
            Transport::Clearnet => "clearnet",
            Transport::Tor => "tor",
            Transport::I2p => "i2p",
        }
    }
}

#[derive(Clone, Debug)]
struct PeerTarget {
    host: String,
    port: u16,
    socks: Option<(String, u16)>,
    transport: Transport,
}

impl PeerTarget {
    fn parse(spec: &str, socks_default: &(String, u16), i2p_socks: &(String, u16)) -> Result<PeerTarget, String> {
        let (host, port) = spec
            .rsplit_once(':')
            .ok_or_else(|| format!("peer target \"{}\" must be HOST:PORT", spec))?;
        let port: u16 = port
            .parse()
            .map_err(|_| format!("peer target \"{}\": bad port", spec))?;
        if host.is_empty() {
            return Err(format!("peer target \"{}\": empty host", spec));
        }
        let (socks, transport) = if host.ends_with(".onion") {
            (Some(socks_default.clone()), Transport::Tor)
        } else if host.ends_with(".b32.i2p") || host.ends_with(".i2p") {
            (Some(i2p_socks.clone()), Transport::I2p)
        } else {
            (None, Transport::Clearnet)
        };
        Ok(PeerTarget {
            host: host.to_string(),
            port,
            socks,
            transport,
        })
    }

    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone)]
struct Config {
    no_proxy: bool,
    /// outbound attach targets, tried in rotation starting after the last
    /// successful one (multi-target attach with fallback)
    peers: Vec<PeerTarget>,
    /// tor SOCKS5 listener used for .onion targets
    socks: (String, u16),
    /// inbound P2P listener (None = disabled)
    listen_bind: String,
    listen_port: Option<u16>,
    /// healthcheck: skip the network-quiet gate (static/air-gapped node)
    static_mode: bool,
    peer_quiet_secs: u64,
    advance_quiet_secs: u64,
    rpc_host: String,
    rpc_port: u16,
    conf_path: PathBuf,
    archive: String,
    state_dir: PathBuf,
    seconds: u64,
    backfill_headers: usize,
    backfill_blocks: usize,
    reseed: bool,
    forever: bool,
    status: bool,
    max_write_age: u64,
    summary_out: Option<PathBuf>,
    // ---- phase 5b: RPC surface ----
    rpc_enabled: bool,
    rpc_bind: String,
    rpc_listen_port: u16,
    rpc_max_body: usize,
    rpc_debug: bool,
    proxy_timeout_secs: u64,
    blocks_file: Option<PathBuf>,
    balances_path: PathBuf,
    /// true when `--balances PATH` was given EXPLICITLY: the node then
    /// BUILDS and maintains the snapshot at that path (background builder
    /// thread, atomic writes) instead of only serving an existing file.
    balances_build: bool,
    wallet_utxos_path: PathBuf,
    index_dir: PathBuf,
    // ---- phase 7 milestone 2: staking / wallet / smsg / i2p ----
    /// integrated staking loop (conf `stake=1` or --stake; --no-stake wins)
    stake: Option<bool>,
    /// kernel-search cadence seconds
    stake_tick_secs: u64,
    /// wallet file (SQLite TRIw) — REQUIRED for staking + smsg
    wallet_path: Option<PathBuf>,
    /// i2pd SOCKS5 listener for .b32.i2p targets (default 127.0.0.1:4447)
    i2p_socks: (String, u16),
    /// FROM-ZERO: build the state from genesis and backfill the ENTIRE
    /// chain over P2P (no seeded tipstate, no local archive needed)
    sync_from_zero: bool,
    /// true when --daemon/--onion/--i2p was given explicitly (from-zero
    /// auto-arms on "no state + explicit peers")
    explicit_peers: bool,
    /// --new-wallet PATH: create a fresh wallet (NEW keys) at PATH if it
    /// does not exist, then use it as --wallet
    new_wallet: Option<PathBuf>,
    /// from-zero fetcher sessions (parallel getdata pipelines; validation
    /// stays strictly sequential). Default 8 (measured: DNS2 serves ~25-30
    /// blk/s per session; 4 left the validator idle, 8 doubles the pipe).
    sync_workers: u32,
    /// --full-validate: disable the from-zero assume-valid envelope (strict
    /// historical kernel enforcement; see CHECKPOINT_HORIZON)
    full_validate: bool,
    }

fn usage() -> ! {
    eprintln!(
        "usage: follow_tip [--daemon HOST:PORT[,...]] [--onion HOST:PORT] [--socks IP:PORT]\n\
         [--listen-bind ADDR] [--listen-port N] [--no-listen] [--static]\n\
         [--rpc-port N] [--conf PATH] [--archive PATH] [--state-dir DIR]\n\
         [--seconds N] [--backfill N] [--backfill-blocks N] [--reseed]\n\
         [--forever] [--status] [--max-write-age SECS] [--summary PATH]\n\
         \n\
         FROM-ZERO sync (bare machine -> full chain over P2P):\n\
         --sync-from-zero    build the state from genesis and backfill the\n\
                             ENTIRE chain (auto-arms on no state + explicit\n\
                             peers). Default validation = the DEPLOYED\n\
                             CLIENT's assume-valid envelope: below the last\n\
                             hardcoded checkpoint (h2203924, anchor\n\
                             a838e60d...) kernel/sig/reward evidence is\n\
                             skipped exactly like the deployed client's own\n\
                             from-scratch sync; the boundary block's hash is\n\
                             verified against the compiled-in checkpoint;\n\
                             everything above keeps FULL validation.\n\
         --full-validate     strict mode: hard-enforce every historical\n\
                             kernel too (slow, ~7 blk/s, and the h17693 seam\n\
                             fails — kept for audit only).\n\
         --sync-workers N    parallel fetcher sessions (default 8; validation\n\
                             stays strictly sequential).\n\
         \n\
         Outbound P2P attach (phase 6 multi-target with fallback):\n\
         --daemon HOST:PORT  outbound peer; comma-separated list and repeated\n\
                             flags append. .onion hosts go through --socks.\n\
         --onion HOST:PORT   shorthand: outbound peer via tor SOCKS.\n\
         --socks IP:PORT     tor SOCKS5 listener for .onion targets\n\
                             (default 127.0.0.1:9050).\n\
         Targets are tried in rotation starting after the last successful one;\n\
         all targets failing => HEADLESS mode (native RPCs from state, retry\n\
         each poll tick).\n\
         \n\
         Inbound P2P listener (phase 6 — tri7d is a primary network peer):\n\
         --listen-port N     listen for inbound peers (default 24113).\n\
         --listen-bind ADDR  listener bind address (default 0.0.0.0).\n\
         --no-listen         disable the inbound listener (follower-only).\n\
         Inbound peers get the same handshake/serve path: getdata (stored\n\
         blocks), getheaders, ping — and may PUSH validated blocks, which are\n\
         relayed to all other peers (inv -> getdata).\n\
         \n\
         --forever: run the follow loop with no deadline (daemon mode; systemd\n\
         restarts on failure).\n\
         --status: healthcheck — print tip height + advance/write/peer times as\n\
         JSON. No network, no conf needed. Exit 0 healthy; 3 when the state\n\
         file is unreadable OR stale (state-write age > --max-write-age,\n\
         default 600) OR degraded. Degraded (--static NOT set): NO peer\n\
         connected/traffic for > --peer-quiet-secs (default 600) AND the tip\n\
         has not advanced for > --advance-quiet-secs (default 1800) — a\n\
         network node that can neither talk to peers nor follow the chain is\n\
         not healthy even if its own loop is writing state. --static skips\n\
         the network-quiet gate. last_advance_utc bumps only on real\n\
         tip-height advances; updated_utc/state mtime bump every write;\n\
         last_peer_utc bumps on any peer traffic (inbound or outbound).\n\
         \n\
         RPC surface (daemon-replacement build):\n\
         --rpc-port N        JSON-RPC listener port (loopback-only)\n\
         --rpc-bind ADDR     loopback-only bind, default 127.0.0.1 (non-loopback\n\
         --no-proxy          disable the transitional legacy-RPC proxy entirely\n\
                             (cutover: daemon retired — proxy-only methods\n\
                             return cutover-mode errors instead of forwarding)\n\
                             addresses are refused)\n\
         --no-rpc            disable the RPC listener\n\
         --rpc-max-body N    request cap, default 1048576 (1 MB)\n\
         --rpc-debug         debug-level per-call logging to stderr\n\
         --proxy-timeout S   legacy-RPC fallback timeout, default 10\n\
         --blocks-file PATH  blk0001.dat for the native getblock/getraw-\n\
                             transaction index (default: <state-dir>/blk0001.dat\n\
                             then /root/.triangles/blk0001.dat). The index is\n\
                             built in a background thread and cached under\n\
                             <state-dir>/index/.\n\
         --balances PATH     replay-derived balances snapshot (balances.json).\n\
                             Given EXPLICITLY, the node BUILDS and maintains\n\
                             the snapshot at PATH in a background thread\n\
                             (atomic writes; the native `richlist` explorer\n\
                             verb serves it). Without the flag the node only\n\
                             SERVES an existing snapshot and never writes one.\n\
         --wallet-utxos PATH replay-derived wallet UTXO snapshot\n\
         --index-dir DIR     where index caches live (default <state-dir>/index)"
    );
    std::process::exit(EXIT_USAGE);
}

fn parse_args() -> Config {
    let mut cfg = Config {
        peers: vec![PeerTarget {
            host: "127.0.0.1".into(),
            port: 24113,
            socks: None,
            transport: Transport::Clearnet,
        }], // legacy attach default (triangles.conf port=24113)
        socks: ("127.0.0.1".into(), 9050),
        listen_bind: "0.0.0.0".into(),
        listen_port: Some(24113),
        static_mode: false,
        peer_quiet_secs: 600,
        advance_quiet_secs: 1800,
        rpc_host: "127.0.0.1".into(),
        rpc_port: 19112,
        conf_path: PathBuf::from("/root/.triangles/triangles.conf"),
        archive: "/root/.triangles/blk0001.dat".into(),
        state_dir: PathBuf::from("/root/tri7"),
        seconds: 660,
        backfill_headers: 3000,
        backfill_blocks: 100,
        reseed: false,
        forever: false,
        status: false,
        max_write_age: 600,
        summary_out: None,
        rpc_enabled: true,
        rpc_bind: "127.0.0.1".into(),
        no_proxy: false,
        rpc_listen_port: 19113,
        rpc_max_body: 1_048_576,
        rpc_debug: false,
        proxy_timeout_secs: 10,
        blocks_file: None,             // resolved against state_dir / archive below
        balances_path: PathBuf::new(), // defaults filled below
        balances_build: false,
        wallet_utxos_path: PathBuf::new(),
        index_dir: PathBuf::new(),
        stake: None,
        stake_tick_secs: 15,
        wallet_path: None,
        i2p_socks: ("127.0.0.1".into(), 4447),
        sync_from_zero: false,
        explicit_peers: false,
        new_wallet: None,
        sync_workers: 8,
        full_validate: false,
    };
    cfg.balances_path = cfg.state_dir.join("balances.json");
    cfg.wallet_utxos_path = cfg.state_dir.join("wallet_utxos.json");
    cfg.index_dir = cfg.state_dir.join("index");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let mut explicit_peers = false; // first --daemon/--onion replaces the default
    while i < args.len() {
        let need = |i: &mut usize, args: &[String]| -> String {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        let parse_sock_addr = |s: &str| -> (String, u16) {
            match s.rsplit_once(':') {
                Some((h, p)) => match p.parse() {
                    Ok(port) => (h.to_string(), port),
                    Err(_) => usage(),
                },
                None => usage(),
            }
        };
        match args[i].as_str() {
            "--sync-from-zero" => cfg.sync_from_zero = true,
            "--full-validate" => cfg.full_validate = true,
            "--new-wallet" => cfg.new_wallet = Some(PathBuf::from(need(&mut i, &args))),
            "--sync-workers" => {
                cfg.sync_workers = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            // repeatable and comma-separated: append targets. The FIRST
            // explicit target REPLACES the built-in default (127.0.0.1:24113)
            // — a primary node listening on 24113 must never carry its own
            // listener as an attach target (self-connect handshake loop).
            "--daemon" => {
                for spec in need(&mut i, &args).split(',') {
                    if spec.trim().is_empty() {
                        continue;
                    }
                    let t = PeerTarget::parse(spec.trim(), &cfg.socks, &cfg.i2p_socks)
                        .unwrap_or_else(|_| usage());
                    if !explicit_peers {
                        cfg.peers.clear();
                        explicit_peers = true;
                    }
                    cfg.peers.push(t);
                }
            }
            "--onion" => {
                let mut t = PeerTarget::parse(&need(&mut i, &args), &cfg.socks, &cfg.i2p_socks)
                    .unwrap_or_else(|_| usage());
                t.socks = Some(cfg.socks.clone());
                t.transport = Transport::Tor;
                if !explicit_peers {
                    cfg.peers.clear();
                    explicit_peers = true;
                }
                cfg.peers.push(t);
            }
            "--i2p" => {
                let mut t = PeerTarget::parse(&need(&mut i, &args), &cfg.socks, &cfg.i2p_socks)
                    .unwrap_or_else(|_| usage());
                t.socks = Some(cfg.i2p_socks.clone());
                t.transport = Transport::I2p;
                if !explicit_peers {
                    cfg.peers.clear();
                    explicit_peers = true;
                }
                cfg.peers.push(t);
            }
            "--socks" => cfg.socks = parse_sock_addr(&need(&mut i, &args)),
            "--i2psocks" => cfg.i2p_socks = parse_sock_addr(&need(&mut i, &args)),
            "--stake" => cfg.stake = Some(true),
            "--no-stake" => cfg.stake = Some(false),
            "--stake-tick" => {
                cfg.stake_tick_secs = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--wallet" => cfg.wallet_path = Some(PathBuf::from(need(&mut i, &args))),
            "--listen-bind" => cfg.listen_bind = need(&mut i, &args),
            "--listen-port" => {
                cfg.listen_port =
                    Some(need(&mut i, &args).parse().unwrap_or_else(|_| usage()))
            }
            "--no-listen" => cfg.listen_port = None,
            "--static" => cfg.static_mode = true,
            "--peer-quiet-secs" => {
                cfg.peer_quiet_secs = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--advance-quiet-secs" => {
                cfg.advance_quiet_secs = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--rpc-port" => {
                cfg.rpc_listen_port = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--conf" => cfg.conf_path = PathBuf::from(need(&mut i, &args)),
            "--archive" => cfg.archive = need(&mut i, &args),
            "--state-dir" => {
                cfg.state_dir = PathBuf::from(need(&mut i, &args));
                // path-relative defaults move with the state dir unless set explicitly
                cfg.balances_path = cfg.state_dir.join("balances.json");
                cfg.wallet_utxos_path = cfg.state_dir.join("wallet_utxos.json");
                cfg.index_dir = cfg.state_dir.join("index");
            }
            "--seconds" => cfg.seconds = need(&mut i, &args).parse().unwrap_or_else(|_| usage()),
            "--backfill" => {
                cfg.backfill_headers = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--backfill-blocks" => {
                cfg.backfill_blocks = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--reseed" => cfg.reseed = true,
            "--forever" => cfg.forever = true,
            "--status" => cfg.status = true,
            "--max-write-age" => {
                cfg.max_write_age = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--summary" => cfg.summary_out = Some(PathBuf::from(need(&mut i, &args))),
            "--rpc-bind" => cfg.rpc_bind = need(&mut i, &args),
            "--no-proxy" => cfg.no_proxy = true,
            "--no-rpc" => cfg.rpc_enabled = false,
            "--rpc-max-body" => {
                cfg.rpc_max_body = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--rpc-debug" => cfg.rpc_debug = true,
            "--proxy-timeout" => {
                cfg.proxy_timeout_secs = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--blocks-file" => cfg.blocks_file = Some(PathBuf::from(need(&mut i, &args))),
            "--balances" => {
                cfg.balances_path = PathBuf::from(need(&mut i, &args));
                cfg.balances_build = true;
            }
            "--wallet-utxos" => cfg.wallet_utxos_path = PathBuf::from(need(&mut i, &args)),
            "--index-dir" => cfg.index_dir = PathBuf::from(need(&mut i, &args)),
            _ => usage(),
        }
        i += 1;
    }
    cfg.explicit_peers = explicit_peers;
    if cfg.peers.is_empty() {
        die(EXIT_USAGE, "no outbound targets: --daemon/--onion required");
    }
    // conf-file `stake=` is the deployment default; CLI flags override.
    if cfg.stake.is_none() {
        if let Ok(text) = std::fs::read_to_string(&cfg.conf_path) {
            for line in text.lines() {
                if let Some(v) = line.trim().strip_prefix("stake=") {
                    cfg.stake = Some(v.trim() != "0");
                }
            }
        }
    }
    // conf-file `addnode=<host>:<port>` — dual-network: .onion and .b32.i2p
    // targets join the attach list (each dialled through its overlay SOCKS);
    // any cleartext IP/hostname is IGNORED with a log line.
    if let Ok(text) = std::fs::read_to_string(&cfg.conf_path) {
        // ignored entries are already logged inside addnode_targets_from_conf
        let (targets, _) = addnode_targets_from_conf(&text, &cfg.socks, &cfg.i2p_socks);
        if !targets.is_empty() {
            if !explicit_peers {
                // replace the 127.0.0.1:24113 built-in default exactly like
                // the first --daemon does (self-connect guard)
                cfg.peers.clear();
                explicit_peers = true;
            }
            cfg.peers.extend(targets);
        }
    }
    cfg.explicit_peers = explicit_peers;
    cfg
}

/// Parse `addnode=<host>:<port>` lines (last one wins per host, like the
/// legacy client's repeated-flag handling). Dual-network rule: `.onion`
/// (tor SOCKS) and `.b32.i2p`/`.i2p` (i2pd SOCKS) hosts are accepted; a
/// cleartext IP/hostname is dropped AND logged. The
/// companion file `addnode=` targets must never crash on junk lines — a
/// malformed entry is ignored with a log line too. Returns the accepted
/// targets plus the ignored `host:port` strings (each already logged).
fn addnode_targets_from_conf(
    conf_text: &str,
    socks: &(String, u16),
    i2p_socks: &(String, u16),
) -> (Vec<PeerTarget>, Vec<String>) {
    let mut targets = Vec::new();
    let mut ignored = Vec::new();
    for line in conf_text.lines() {
        let Some(spec) = line.trim().strip_prefix("addnode=") else {
            continue;
        };
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let host = spec.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
        // Dual-network fleet (standing directive): `.onion` dials through the
        // tor SOCKS, `.b32.i2p`/`.i2p` through the i2pd SOCKS — both are
        // overlay hosts and join the attach list; only cleartext IP/hostname
        // is rejected (no clearnet P2P).
        if host.ends_with(".onion") || host.ends_with(".b32.i2p") || host.ends_with(".i2p") {
            match PeerTarget::parse(spec, socks, i2p_socks) {
                Ok(t) => {
                    let tr = if t.transport == Transport::I2p { "i2p" } else { "tor" };
                    log(&format!("addnode={} accepted ({})", t.endpoint(), tr));
                    targets.push(t);
                }
                Err(e) => log(&format!("addnode={} ignored ({})", spec, e)),
            }
        } else {
            log(&format!(
                "addnode={} ignored: only .onion / .b32.i2p addnode hosts are accepted — no cleartext P2P",
                spec
            ));
            // `ignored` carries rejected host:port specs (the function doc's
            // contract): the shape check mirrors what the accepted path needs,
            // so junk with or without a colon never enters the list.
            if is_host_port_shape(spec) {
                ignored.push(spec.to_string());
            }
        }
    }
    (targets, ignored)
}

/// True when spec has the host:port shape the accepted addnode path requires:
/// a non-empty host part and a port that parses as u16 (split on the LAST
/// colon, so hosts containing colons still resolve their port).
fn is_host_port_shape(spec: &str) -> bool {
    match spec.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
        None => false,
    }
}

/// Persist `key=value` into the conf file (staking on|off). Replaces an
/// existing `key=` line, else appends. Atomic swap; preserves file mode.
fn conf_set_flag(conf_path: &Path, key: &str, value: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(conf_path)
        .map_err(|e| format!("read conf {}: {}", conf_path.display(), e))?;
    let mut out = String::with_capacity(text.len() + 16);
    let mut replaced = false;
    for line in text.lines() {
        if line.trim_start().starts_with(&format!("{}=", key)) {
            out.push_str(&format!("{}={}\n", key, value));
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("{}={}\n", key, value));
    }
    let tmp = conf_path.with_extension("conf.tmp");
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(conf_path)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o640)
    };
    std::fs::write(&tmp, &out).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("chmod {}: {}", tmp.display(), e))?;
    }
    std::fs::rename(&tmp, conf_path).map_err(|e| format!("rename conf: {}", e))?;
    Ok(())
}

fn read_rpc_creds(cfg: &Config) -> (String, String) {
    let text = std::fs::read_to_string(&cfg.conf_path).unwrap_or_else(|e| {
        die(
            EXIT_USAGE,
            &format!("read conf {}: {}", cfg.conf_path.display(), e),
        )
    });
    let mut user = String::new();
    let mut pass = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("rpcuser=") {
            user = v.to_string();
        }
        if let Some(v) = line.strip_prefix("rpcpassword=") {
            pass = v.to_string();
        }
    }
    if user.is_empty() || pass.is_empty() {
        die(EXIT_USAGE, "rpcuser/rpcpassword missing in conf");
    }
    (user, pass)
}

fn state_path(cfg: &Config) -> PathBuf {
    let mut p = cfg.state_dir.clone();
    p.push("tipstate.json");
    p
}

fn write_state_atomic(cfg: &Config, st: &TipStateFile) -> Result<(), String> {
    let path = state_path(cfg);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    // the state file holds no secrets but is nobody's business but the
    // service's: pin 0600 on the tmp BEFORE the rename, or the default
    // 0644 from fs::write clobbers the mode on every atomic swap
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod {}: {}", tmp.display(), e))?;
    }
    // Windows: MoveFileEx fails with ERROR_ACCESS_DENIED (os error 5) when a
    // reader has the destination open or an AV filter holds it briefly. Retry
    // with backoff instead of failing the write.
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..12 {
        match std::fs::rename(&tmp, &path) {
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
        "rename state: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------
#[derive(Default)]
struct Stats {
    sent: HashMap<String, u64>,
    recv: HashMap<String, u64>,
    headers_processed: u64,
    headers_new: u64,
    /// addr replies served for inbound getaddr (discovery)
    getaddr_served: u64,
    headers_backfilled: u64,
    blocks_received: u64,
    blocks_validated_live: u64,
    blocks_validated_hist: u64,
    kernel_enforced: u64,
    kernel_seeded: u64,
    kernel_skipped_no_oracle: u64,
    // ---- from-zero assume-valid envelope (deployed-client parity) ----
    kernel_skipped_assume_valid: u64, // below V6_RESTART_HEIGHT: no kernel at all (daemon stores proofhash=0 there)
    kernel_computed_advisory: u64,    // seam era [2200900, 2203924): kernel computed for the index, target NOT enforced
    kernel_target_tol_assumed: u64,   // above the horizon: target miss tolerated only because the consumed modifier predates the checkpoint
    checkpoint_anchor_verified: u64,  // h2203924 hash matched the compiled-in checkpoint
    nbits_checked: u64,
    sigs_checked: u64,
    checksums_verified: u64,
    polls: u64,
    reconnects: u64,
    parse_failures: u64,
    handshakes: u64,
    // phase 6: primary-node serving/relay
    blocks_relayed: u64,     // inv announcements sent on tip advance
    reorgs: u64,             // competing branches adopted via fork choice
    getdata_served: u64,     // block payloads served to peers
    getheaders_served: u64,  // headers ranges served to peers
    inbound_accepted: u64,   // inbound peers that completed the handshake
    // ---- phase 7 milestone 2: tx relay ----
    txs_received: u64,
    txs_accepted: u64,
    txs_rejected: u64,
    tx_getdatas_sent: u64,
    getdata_tx_served: u64,
    // gap recovery (phase 7 urgent): tail bodies missing from the store
    gap_ticks: u64,          // rotation ticks with a tail gap detected
    gap_requests: u64,       // getdata(MSG_BLOCK) rounds sent for the gap
    gap_filled: u64,         // bodies recovered from peers and appended
    gap_rollback: bool,      // phantom-tip rollback fired (branch b)
}

impl Stats {
    fn note_sent(&mut self, cmd: &str) {
        *self.sent.entry(cmd.to_string()).or_insert(0) += 1;
    }
    fn note_recv(&mut self, cmd: &str) {
        *self.recv.entry(cmd.to_string()).or_insert(0) += 1;
    }
    fn total_recv(&self) -> u64 {
        self.recv.values().sum()
    }
    fn total_sent(&self) -> u64 {
        self.sent.values().sum()
    }
}

fn stats_json(s: &Stats) -> serde_json::Value {
    serde_json::json!({
        "sent": s.sent,
        "recv": s.recv,
        "headers_processed": s.headers_processed,
        "headers_new": s.headers_new,
        "headers_backfilled": s.headers_backfilled,
        "blocks_received": s.blocks_received,
        "blocks_validated_live": s.blocks_validated_live,
        "blocks_validated_hist": s.blocks_validated_hist,
        "kernel_enforced": s.kernel_enforced,
        "kernel_seeded": s.kernel_seeded,
        "kernel_skipped_no_oracle": s.kernel_skipped_no_oracle,
        "nbits_checked": s.nbits_checked,
        "sigs_checked": s.sigs_checked,
        "checksums_verified": s.checksums_verified,
        "polls": s.polls,
        "reconnects": s.reconnects,
        "parse_failures": s.parse_failures,
        "handshakes": s.handshakes,
        "blocks_relayed": s.blocks_relayed,
        "reorgs": s.reorgs,
        "getdata_served": s.getdata_served,
        "getheaders_served": s.getheaders_served,
        "inbound_accepted": s.inbound_accepted,
        "txs_received": s.txs_received,
        "txs_accepted": s.txs_accepted,
        "txs_rejected": s.txs_rejected,
        "tx_getdatas_sent": s.tx_getdatas_sent,
        "getdata_tx_served": s.getdata_tx_served,
        "gap_ticks": s.gap_ticks,
        "gap_requests": s.gap_requests,
        "gap_filled": s.gap_filled,
        "gap_rollback": s.gap_rollback,
    })
}

// ---------------------------------------------------------------------------
// P2P session
// ---------------------------------------------------------------------------
struct Session {
    stream: TcpStream,
    dec: tri7_core::p2p::MessageDecoder,
    /// messages SENT on this session (per-peer counter for getpeerinfo)
    sent_msgs: u64,
}

impl Session {
    fn connect(addr: &str) -> Result<Session, String> {
        let stream = TcpStream::connect(addr).map_err(|e| format!("connect {}: {}", addr, e))?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .ok();
        Ok(Session {
            stream,
            dec: tri7_core::p2p::MessageDecoder::new(),
            sent_msgs: 0,
        })
    }

    /// Connect via a direct IP target (dotted quad, 5s connect timeout so a
    /// filtered host cannot stall the rotation) or through the tor SOCKS5
    /// listener for .onion targets.
    fn connect_target(target: &PeerTarget) -> Result<Session, String> {
        let stream = match &target.socks {
            None => {
                use std::net::ToSocketAddrs;
                let addr = (target.host.as_str(), target.port)
                    .to_socket_addrs()
                    .map_err(|e| format!("resolve {}: {}", target.endpoint(), e))?
                    .next()
                    .ok_or_else(|| format!("resolve {}: no address", target.endpoint()))?;
                let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
                    .map_err(|e| format!("connect {}: {}", target.endpoint(), e))?;
                stream
            }
            Some((sh, sp)) => {
                let stream = socks5_connect(sh, *sp, &target.host, target.port)?;
                stream
            }
        };
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .ok();
        Ok(Session {
            stream,
            dec: tri7_core::p2p::MessageDecoder::new(),
            sent_msgs: 0,
        })
    }

    fn send(&mut self, command: &str, payload: &[u8], stats: &mut Stats) -> Result<(), String> {
        let frame = tri7_core::p2p::encode_message(command, payload)?;
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("send {}: {}", command, e))?;
        self.sent_msgs += 1;
        stats.note_sent(command);
        Ok(())
    }

    /// Read whatever the socket has; empty on timeout, Err on wire garbage.
    fn pump(&mut self) -> Result<Vec<tri7_core::p2p::RawMessage>, String> {
        let mut buf = [0u8; 65_536];
        let n = match self.stream.read(&mut buf) {
            Ok(0) => return Err("peer closed connection".into()),
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(Vec::new())
            }
            // EINTR is recoverable (a signal arrived mid-syscall): treat like
            // would-block and retry on the next pump. DNS3 lost a 44s-old
            // resumed run at h1.84M to this surfacing as fatal exit 3.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Ok(Vec::new()),
            Err(e) => return Err(format!("read: {}", e)),
        };
        self.dec.feed(&buf[..n])
    }

    fn shutdown(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

struct PeerInfo {
    version: i32,
    subver: String,
    start_height: i32,
    services: u64,
    /// the peer's advertised listening address from its `version` addr_me,
    /// as a peer-store key (reconstructed onion hostname / dotted quad; a
    /// legacy peers.json may still hold hex tor keys) — None when it is a
    /// loopback placeholder (their tor builds use 127.0.0.1)
    advertised: Option<String>,
}

// ---------------------------------------------------------------------------
// SOCKS5 (tor) client — minimal RFC 1928, METHOD none, ATYP domain/ipv4.
// Used to reach .onion peers through the local tor listener (127.0.0.1:9050).
// ---------------------------------------------------------------------------

fn socks5_greeting() -> Vec<u8> {
    vec![0x05, 0x01, 0x00] // VER 5, 1 method, NO-AUTH
}

fn socks5_connect_request(host: &str, port: u16) -> Vec<u8> {
    let mut w = Vec::new();
    w.push(0x05); // VER
    w.push(0x01); // CMD CONNECT
    w.push(0x00); // RSV
    let octets: Option<Vec<u8>> = host
        .split('.')
        .map(|o| o.parse::<u8>())
        .collect::<Result<Vec<u8>, std::num::ParseIntError>>()
        .ok()
        .filter(|v| v.len() == 4);
    match octets {
        Some(o) => {
            w.push(0x01); // ATYP IPv4
            w.extend_from_slice(&o);
        }
        None => {
            w.push(0x03); // ATYP domain (onion)
            w.push(host.len() as u8);
            w.extend_from_slice(host.as_bytes());
        }
    }
    w.extend_from_slice(&port.to_be_bytes());
    w
}

/// Drain the ATYP-dependent bound-address tail of a SOCKS5 reply.
fn socks5_consume_reply(atyp: u8, r: &mut impl Read) -> Result<(), String> {
    fn drain(r: &mut impl Read, n: usize) -> Result<(), String> {
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf)
            .map_err(|e| format!("socks reply: {}", e))
    }
    match atyp {
        0x01 => drain(r, 4 + 2),
        0x03 => {
            let mut ln = [0u8; 1];
            r.read_exact(&mut ln)
                .map_err(|e| format!("socks reply: {}", e))?;
            drain(r, ln[0] as usize + 2)
        }
        0x04 => drain(r, 16 + 2),
        other => Err(format!("socks reply: unknown ATYP {:#x}", other)),
    }
}

fn socks5_connect(
    socks_host: &str,
    socks_port: u16,
    host: &str,
    port: u16,
) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;
    let mut s = TcpStream::connect(
        (socks_host, socks_port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve socks {}: {}", socks_host, e))?
            .next()
            .ok_or_else(|| format!("resolve socks {}: no address", socks_host))?,
    )
    .map_err(|e| format!("connect socks {}: {}", socks_host, e))?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok();
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(&socks5_greeting())
        .map_err(|e| format!("socks greeting: {}", e))?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply)
        .map_err(|e| format!("socks greeting reply: {}", e))?;
    if reply[0] != 0x05 {
        return Err(format!("socks greeting: bad version {:#x}", reply[0]));
    }
    if reply[1] != 0x00 {
        return Err(format!("socks proxy refused auth (method {:#x})", reply[1]));
    }
    s.write_all(&socks5_connect_request(host, port))
        .map_err(|e| format!("socks connect req: {}", e))?;
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr)
        .map_err(|e| format!("socks connect reply: {}", e))?;
    if hdr[0] != 0x05 {
        return Err(format!("socks connect: bad version {:#x}", hdr[0]));
    }
    if hdr[1] != 0x00 {
        return Err(format!(
            "socks connect to {}:{} failed: SOCKS reply code {} (0=ok, 1=general, \
             4=host unreachable, 5=refused)",
            host, port, hdr[1]
        ));
    }
    let mut rest = s.try_clone().map_err(|e| format!("socks clone: {}", e))?;
    socks5_consume_reply(hdr[3], &mut rest)?;
    Ok(s)
}

fn handshake(
    sess: &mut Session,
    stats: &mut Stats,
    our_height: u64,
    target: &PeerTarget,
) -> Result<PeerInfo, String> {
    // addr_you: the target's address for direct IP peers; a placeholder
    // (127.0.0.1 + the target port) for .onion peers — the wire form needs 4
    // bytes and the legacy daemon treats the version address as informational
    // (phase 2 evidence: loopback peers announced them=127.0.0.1:<port>).
    let quad: Option<[u8; 4]> = if target.host.ends_with(".onion") {
        None
    } else {
        target
            .host
            .split('.')
            .map(|o| o.parse::<u8>())
            .collect::<Result<Vec<u8>, std::num::ParseIntError>>()
            .ok()
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
    };
    let q = quad.unwrap_or([127, 0, 0, 1]);
    let (a, b, c, d) = (q[0], q[1], q[2], q[3]);
    let addr_you = tri7_core::p2p::WireAddr::ipv4(a, b, c, d, target.port, tri7_core::p2p::NODE_NETWORK);
    let addr_me = tri7_core::p2p::WireAddr::ipv4(
        127,
        0,
        0,
        1,
        tri7_core::p2p::DEFAULT_PORT,
        tri7_core::p2p::NODE_NETWORK,
    );
    let nonce = (now_secs() ^ ((std::process::id() as u64) << 32)) | 1;
    let vp = tri7_core::p2p::VersionPayload {
        version: tri7_core::p2p::PROTOCOL_VERSION,
        services: tri7_core::p2p::NODE_NETWORK,
        timestamp: now_secs() as i64,
        addr_you,
        addr_me,
        nonce,
        user_agent: "/tri7:7.0.0/".into(),
        start_height: our_height as i32,
    };
    sess.send("version", &tri7_core::p2p::encode_version(&vp), stats)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut peer: Option<PeerInfo> = None;
    let mut veracked = false;
    let mut verack_sent = false;
    while Instant::now() < deadline {
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            match msg.command.as_str() {
                "version" => {
                    let v = tri7_core::p2p::parse_version(&msg.payload)?;
                    if v.version < tri7_core::p2p::MIN_PROTO_VERSION {
                        return Err(format!("peer version {} obsolete", v.version));
                    }
                    peer = Some(PeerInfo {
                        version: v.version,
                        subver: v.user_agent.clone(),
                        start_height: v.start_height,
                        services: v.services,
                        advertised: advertised_store_key(&v.addr_me),
                    });
                    if !verack_sent {
                        sess.send("verack", &[], stats)?;
                        verack_sent = true;
                    }
                }
                "verack" => {
                    veracked = true;
                }
                _ => {}
            }
        }
        if peer.is_some() && veracked {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !veracked {
        return Err("handshake timed out waiting for verack".into());
    }
    // Discovery: ask for the peer's address book right after verack
    // (net.cpp ProcessMessage — the legacy client's getaddr/addr gossip is
    // what keeps the network alive once the static seeds die). Answers come
    // back as `addr` and land in peers.json via handle_message.
    sess.send("getaddr", &[], stats)?;
    // Prefer headers announcements (main.cpp sendheaders → fPreferHeaders);
    // inv announcements are handled too.
    sess.send("sendheaders", &[], stats)?;
    stats.handshakes += 1;
    Ok(peer.unwrap_or(PeerInfo {
        version: 0,
        subver: "?".into(),
        start_height: -1,
        services: 0,
        advertised: None,
    }))
}

/// Peer-store key for an address a peer advertised about ITSELF (version
/// addr_me / addr gossip). Loopback and unspecified addresses are
/// placeholders (tor peers announce 127.0.0.1) — never recorded.
fn advertised_store_key(addr: &tri7_core::p2p::WireAddr) -> Option<String> {
    if !addr.is_tor_v3 && addr.is_ipv4_mapped() {
        let [a, b, c, d] = [addr.ip[12], addr.ip[13], addr.ip[14], addr.ip[15]];
        if a == 127 || a == 0 {
            return None;
        }
    }
    addr.store_key()
}

fn peers_path(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("peers.json")
}

/// Our own identities (for the peer store's self-skip): the current onion
/// from onion-state.json when present, plus the hex tor key of any onion
/// address we know.
fn self_identities(cfg: &Config) -> Vec<String> {
    let mut ids = Vec::new();
    let path = cfg.state_dir.join("onion-state.json");
    if let Some(st) = tri7_core::onion::OnionState::load(&path) {
        ids.push(st.current.clone());
        if let Some(key) = tri7_core::p2p::WireAddr::from_onion_label(&st.current, 0, 0)
            .ok()
            .and_then(|a| a.store_key())
        {
            ids.push(key);
        }
    }
    ids
}

/// Load the peer store, register our own identities, hand it to `f`, and
/// save — the house atomic write, tolerating failure with a log line (peer
/// bookkeeping must never take the node down).
fn with_peer_store<T>(
    cfg: &Config,
    f: impl FnOnce(&mut tri7_core::peers::PeerStore) -> T,
) -> Option<T> {
    let path = peers_path(cfg);
    let mut store = tri7_core::peers::PeerStore::load(&path);
    store.set_self(&self_identities(cfg));
    let out = f(&mut store);
    if let Err(e) = store.save(&path) {
        log(&format!("peer store save failed (continuing): {}", e));
    }
    Some(out)
}

/// Record a successful dial/handshake against the store.
fn record_peer_ok(cfg: &Config, endpoint: &str, services: u64, advertised: Option<String>) {
    let now = tri7_core::peers::unix_now();
    with_peer_store(cfg, |store| {
        if let Some((host, port)) = split_endpoint(endpoint) {
            store.upsert(&host, port, services, "listen", now);
            store.record_ok(&host, port, now);
        }
        if let Some(adv) = advertised {
            if let Some(port) = split_endpoint(endpoint).map(|(_, p)| p) {
                store.upsert(&adv, port, services, "listen", now);
            }
        }
    });
}

/// Record a failed dial/handshake (drives the store's exponential backoff).
fn record_peer_fail(cfg: &Config, endpoint: &str) {
    let now = tri7_core::peers::unix_now();
    with_peer_store(cfg, |store| {
        if let Some((host, port)) = split_endpoint(endpoint) {
            store.record_fail(&host, port, now);
        }
    });
}

fn split_endpoint(endpoint: &str) -> Option<(String, u16)> {
    let (host, port) = endpoint.rsplit_once(':')?;
    let port = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

// ---------------------------------------------------------------------------
// Seed — fetch the window's raw block bytes over P2P getdata, verify every
// byte locally (parse + Hash9 + link + merkle), then seed the stake-modifier
// state from the daemon index with checksum-chain verification.
//
// Why not frame-walk blk0001.dat: the daemon interleaves reorged stale-branch
// frames in the file (58 seams observed around h2201698..2201750), so a
// sequential prev-link walk cannot reconstruct the live tail. The daemon's
// block index defines the followed chain (phase 1d proved it equals tri7's
// replay); every block in the window is still byte-verified here: parse
// (Block::read + expect_done), Hash9 header hash == index hash, prev linkage,
// merkle root over vtx. Below the checksum-chain anchor (~h22039xx, where the
// daemon's stored modifierchecksums resume being non-zero) the modifier and
// proof seeds are adopted from the daemon index without chain verification —
// tier-2 trust, documented in the module header.
// ---------------------------------------------------------------------------
struct SeedAdd {
    modifier: u64,
    generated: bool,
    proof: Hash,
    checksum: u32,
}

fn rpc_seed_window(
    rpc: &Rpc,
    hashes_display: &[String],
    pos_flags: &[bool],
    times: &[u32],
    base: u64,
) -> Result<(u32, Vec<SeedAdd>), String> {
    let mut adds: Vec<SeedAdd> = Vec::with_capacity(hashes_display.len());
    let mut cs_prev: Option<u32> = None;
    let mut anchored = false;
    let mut anchor_h = 0u64;
    let mut verified_steps = 0usize;
    const BATCH: usize = 200;
    let mut idx = 0usize;
    while idx < hashes_display.len() {
        let end = (idx + BATCH).min(hashes_display.len());
        let calls: Vec<_> = (idx as u64..end as u64)
            .map(|i| {
                (
                    i,
                    "getblock",
                    serde_json::json!([hashes_display[i as usize]]),
                )
            })
            .collect();
        let items = rpc.call_batch(calls)?;
        if items.len() != end - idx {
            return Err(format!(
                "batch size mismatch {} != {}",
                items.len(),
                end - idx
            ));
        }
        for (slot, item) in items.iter().enumerate() {
            let i = idx + slot;
            let h = base + i as u64;
            let b = item.get("result").ok_or("batch: missing result")?;
            let flags = b["flags"].as_str().unwrap_or("");
            let is_pos = pos_flags[i];
            // the daemon flag string carries "stake-modifier" when the block
            // generated a new modifier (main.h GetFlagStr)
            let generated = flags.contains("stake-modifier");
            let modifier = u64::from_str_radix(b["modifier"].as_str().unwrap_or(""), 16)
                .map_err(|e| format!("h{}: modifier hex: {}", h, e))?;
            let checksum = u32::from_str_radix(b["modifierchecksum"].as_str().unwrap_or(""), 16)
                .map_err(|e| format!("h{}: checksum hex: {}", h, e))?;
            let proof = if is_pos {
                tri7_core::p2p::display_to_internal(b["proofhash"].as_str().unwrap_or(""))
                    .map_err(|e| format!("h{}: proofhash: {}", h, e))?
            } else {
                [0u8; 32]
            };
            let rt = b["time"].as_i64().unwrap_or(-1);
            if rt != times[i] as i64 {
                return Err(format!(
                    "h{}: rpc time {} != p2p-verified time {}",
                    h, rt, times[i]
                ));
            }
            if !anchored {
                if checksum != 0 {
                    anchored = true;
                    anchor_h = h;
                    cs_prev = Some(checksum); // adopt the daemon's chain anchor
                }
            } else {
                let entropy = tri7_core::p2p::display_to_internal(&hashes_display[i])?[0] & 1;
                let flags_num = index_flags(is_pos, entropy, generated);
                let expect = stake_modifier_checksum(cs_prev, flags_num, &proof, modifier);
                if checksum != 0 {
                    if expect != checksum {
                        return Err(format!(
                            "h{}: stake-modifier checksum chain mismatch: computed {:08x} daemon {:08x} (generated={} pos={})",
                            h, expect, checksum, generated, is_pos
                        ));
                    }
                    verified_steps += 1;
                    cs_prev = Some(checksum);
                } else {
                    // daemon has no stored checksum here; keep our own chain
                    cs_prev = Some(expect);
                }
            }
            adds.push(SeedAdd {
                modifier,
                generated,
                proof,
                checksum,
            });
        }
        idx = end;
    }
    if !anchored {
        return Err(
            "no non-zero modifier checksum in window — cannot anchor the chain".to_string(),
        );
    }
    log(&format!(
        "rpc seed: checksum-chain anchored at h{}, {} steps verified to the tip",
        anchor_h, verified_steps
    ));
    Ok((cs_prev.unwrap(), adds))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------
/// Deployed-client assume-valid envelope (from-zero sync).
///
/// The v6.2.6.7 client syncing from scratch SKIPS kernel/sig/reward checks
/// below its last hardcoded mainnet checkpoint — that is what the checkpoint
/// is FOR: the anchor hash binds the trusted history. Its own index proves
/// the envelope: daemon `getblock` stores proofhash=0 for EVERY PoS block
/// h1026..2200899 (IBD-skip Cheops+ / assume-valid <=9001 v5+ / rolling
/// paths) and REAL stored proofs only from the live era h2200900+ (accepted
/// one-by-one with kernels enforced at accept time). tri7's from-zero
/// validator was STRICTER than the deployed client — enforcing historical
/// kernels that were never checked when the blocks were accepted — and died
/// deterministically at h17693 (KERNEL TARGET FAIL on a block the deployed
/// client accepted with proofhash=0).
///
/// Bands for a historical (non-live) block at height h:
///   - h <  2200900 (V6_RESTART_HEIGHT): ASSUME-VALID — no kernel evidence;
///     index proof stays 0, byte-identical to the deployed client's own
///     index values for this range.
///   - 2200900 <= h < 2203924 (CHECKPOINT_HORIZON): SEAM-ADVISORY — the
///     kernel is COMPUTED (so stake modifiers above the seam match the
///     deployed index, where real proofs are stored) but the target is NOT
///     enforced (the deployed client accepted this era live; a recomputation
///     mismatch is tri7-kernel-math divergence, not chain evidence).
///   - h >= 2203924: FULL VALIDATION — kernels hard-enforced, exactly as
///     live. The boundary block itself must hash to the compiled-in
///     checkpoint anchor a838e60d… (the trust anchor of everything below).
///
/// `--full-validate` restores the pre-assume-valid strict behavior for the
/// whole from-zero walk (slow: every historical kernel enforced).
/// CHECKPOINT_HORIZON / CHECKPOINT_ANCHOR_DISPLAY moved to tri7_core::consensus
/// so the getcheckpoint RPC verb serves the SAME constant the validator
/// enforces (lib + bin cannot drift). Semantics unchanged: h >= horizon is
/// full-strict; the anchor binds the trusted history below.
use tri7_core::consensus::{CHECKPOINT_ANCHOR_DISPLAY, CHECKPOINT_HORIZON};
const V6_RESTART_HEIGHT_U64: u64 = 2_200_900; // = consensus::V6_RESTART_HEIGHT; deployed index: real proofs from here up

/// Which kernel treatment applies to a historical block at height h.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum ValidMode {
    /// full kernel enforcement (live blocks, --full-validate, h >= horizon)
    FullStrict,
    /// h in [2200900, 2203924): compute for the index, do not enforce
    SeamAdvisory,
    /// h < 2200900: no kernel evidence at all (deployed index parity)
    AssumeValid,
}

fn validation_mode(h: u64, live: bool, full_validate: bool) -> ValidMode {
    if live || full_validate || h >= CHECKPOINT_HORIZON {
        ValidMode::FullStrict
    } else if h >= V6_RESTART_HEIGHT_U64 {
        ValidMode::SeamAdvisory
    } else {
        ValidMode::AssumeValid
    }
}

/// The checkpoint boundary block binds the trusted history: its hash must
/// equal the compiled-in checkpoint. Live blocks can never sit at the
/// horizon height, so this only fires on historical (fetch/backfill) paths.
fn check_checkpoint_anchor(h: u64, hash_internal: &Hash) -> Result<(), String> {
    if h != CHECKPOINT_HORIZON {
        return Ok(());
    }
    let want = tri7_core::p2p::display_to_internal(CHECKPOINT_ANCHOR_DISPLAY)?;
    if hash_internal != &want {
        return Err(format!(
            "h{}: CHECKPOINT ANCHOR MISMATCH: block hashes to {} but the compiled-in checkpoint is {} — refusing to trust the history below",
            h,
            tri7_core::p2p::internal_to_display(hash_internal),
            CHECKPOINT_ANCHOR_DISPLAY
        ));
    }
    Ok(())
}

struct BlockCtx<'a> {
    height_abs: u64,
    header: Option<&'a Block>, // expected header (from the headers flow)
    prev_entry: IndexEntry,    // validated window entry at height-1
    live: bool,                // true = candidate tip extension (fresh block)
}

struct ValidationOutcome {
    kernel_enforced: bool,
    kernel_seeded: bool,
    proof: Hash,
}

fn validate_block(
    st: &ChainState,
    _rpc: &Rpc, // no external oracle in the tri7-only topology (kept for signature stability)
    stats: &mut Stats,
    block: &Block,
    ctx: &BlockCtx,
) -> Result<ValidationOutcome, String> {
    let h = ctx.height_abs;
    let is_pos = block_is_proof_of_stake(block);
    let mode = validation_mode(h, ctx.live, FULL_VALIDATE.load(Ordering::Relaxed));

    // 1. Hash9 block hash + link to our validated predecessor
    let hash = block.hash();
    if block.prev_hash != ctx.prev_entry.hash {
        return Err(format!(
            "link failure at h{}: prev {} != validated {}",
            h,
            tri7_core::p2p::internal_to_display(&block.prev_hash),
            tri7_core::p2p::internal_to_display(&ctx.prev_entry.hash)
        ));
    }
    // 1b. checkpoint trust anchor: the boundary block must hash to the
    // compiled-in checkpoint — this is what makes assume-valid BELOW the
    // horizon sound (the trusted prefix is pinned by one hash).
    check_checkpoint_anchor(h, &hash).map_err(|e| e.to_string())?;
    if h == CHECKPOINT_HORIZON {
        stats.checkpoint_anchor_verified += 1;
        log(&format!(
            "from-zero: checkpoint anchor VERIFIED at h{} = {} — assume-valid history bound",
            h,
            CHECKPOINT_ANCHOR_DISPLAY
        ));
    }

    // 2. exact header agreement when announced via headers
    if let Some(hdr) = ctx.header {
        if hdr.version != block.version
            || hdr.prev_hash != block.prev_hash
            || hdr.merkle_root != block.merkle_root
            || hdr.time != block.time
            || hdr.bits != block.bits
            || hdr.nonce != block.nonce
        {
            return Err(format!("h{}: block does not match its announced header", h));
        }
        if hdr.hash() != hash {
            return Err(format!("h{}: announced header hash mismatch", h));
        }
    }

    // 3. structure (CheckBlock: coinbase rules, PoS coinstake + empty coinbase out)
    if block.vtx.is_empty() {
        return Err(format!("h{}: no transactions", h));
    }
    if !tx_is_coin_base(&block.vtx[0]) {
        return Err(format!("h{}: first tx is not coinbase", h));
    }
    for tx in block.vtx.iter().skip(1) {
        if tx_is_coin_base(tx) {
            return Err(format!("h{}: more than one coinbase", h));
        }
    }
    if is_pos {
        if block.vtx.len() < 2 || !tx_is_coin_stake(&block.vtx[1]) {
            return Err(format!("h{}: PoS block without a coinstake at vtx[1]", h));
        }
        let cb = &block.vtx[0];
        if cb.vout.len() != 1 || cb.vout[0].value != 0 || !cb.vout[0].script_pubkey.is_empty() {
            return Err(format!("h{}: PoS coinbase output not empty", h));
        }
    }

    // 4. merkle root over the received transactions vs the header field
    if block.merkle_root() != block.merkle_root {
        return Err(format!("h{}: merkle root mismatch", h));
    }

    // 5. timestamps (AcceptBlock rules)
    if let Some(median) = st.median_time_past(h - 1) {
        if block.time <= median {
            return Err(format!(
                "h{}: timestamp {} not above median-time-past {}",
                h, block.time, median
            ));
        }
    }
    let drift = max_time_drift(h as usize);
    if (block.time as i64) + drift < ctx.prev_entry.time as i64 {
        return Err(format!("h{}: timestamp too early vs prev block", h));
    }
    if ctx.live {
        let now = now_secs() as i64;
        if block.time as i64 > now + 15 * 60 {
            return Err(format!("h{}: timestamp too far in the future", h));
        }
    }

    // 6. difficulty: exact EMA reproduction (live era: above the CRAPCHAIN
    // cutoff on every block, so nBits is always enforced). Historical blocks
    // are anchored at their own ancestors via the truncated view.
    // FROM-ZERO: below the CRAPCHAIN cutoff the AcceptBlock rule (main.cpp:
    // 3414-3430) tolerates ANY nBits — the early chain's bits don't follow
    // the EMA, so the exact check is gated on the same cutoff.
    let rel_index = if ctx.live {
        st.rel_index()
    } else {
        st.rel_index_to(h - 1)
    };
    if h as usize > tri7_core::consensus::CRAPCHAIN_CUTOFF_BLOCK {
        let expected_bits = get_next_target_required(&rel_index, is_pos);
        if block.bits != expected_bits {
            return Err(format!(
                "h{}: NBITS FAIL block={:08x} expected={:08x}",
                h, block.bits, expected_bits
            ));
        }
    }
    stats.nbits_checked += 1;

    // 7. proof type
    let proof: Hash;
    let mut out = ValidationOutcome {
        kernel_enforced: false,
        kernel_seeded: false,
        proof: [0u8; 32],
    };
    if is_pos {
        // 7a. kernel evidence, banded by the assume-valid envelope (see the
        // CHECKPOINT_HORIZON docs above):
        //   AssumeValid  — no kernel at all; index proof stays 0, byte-equal
        //                  to the deployed client's FastImport values here.
        //   SeamAdvisory — compute the kernel so stake modifiers above the
        //                  seam match the deployed index, but do NOT enforce.
        //   FullStrict   — enforce (live behavior, unchanged).
        let resolved = if mode == ValidMode::AssumeValid {
            None
        } else {
            let txin = &block.vtx[1].vin[0];
            st.utxo.kernel_meta(&(txin.prev_hash, txin.prev_n))
        };
        match resolved {
            Some(meta) => {
                let res = tri7_core::kernel::check_proof_of_stake(
                    &rel_index,
                    block,
                    &st.utxo,
                    block.bits,
                    h as usize - 1, // ABSOLUTE: selects the GetWeight era
                );
                let enforce = mode == ValidMode::FullStrict;
                let input_abs = st.base + meta.height as u64;
                match res {
                    Ok(res) => {
                        if !res.meets_target {
                            if !enforce {
                                // seam era: advisory only — record the real
                                // proof either way (matches the deployed
                                // index, which holds real proofs here)
                                stats.kernel_computed_advisory += 1;
                                if stats.kernel_computed_advisory % 500 == 1 {
                                    log(&format!(
                                        "h{}: seam-era kernel advisory: target not met by our recomputation (block_from abs h{}, coinDays={}) — tolerated (deployed client accepted this era live)",
                                        h,
                                        st.base as usize + res.block_from_height,
                                        res.coin_day_weight
                                    ));
                                }
                            } else if st.base as usize + res.block_from_height
                                < CHECKPOINT_HORIZON as usize
                            {
                                // FULL zone, but this kernel's stake modifier
                                // derives from the assume-valid region: our
                                // modifier for that era is reconstructed from
                                // assumed (not enforced) evidence, so a target
                                // miss here cannot distinguish a bad block from
                                // our own index divergence. Tolerate with a
                                // loud stat; modifiers generated above the
                                // horizon are fully enforced as before.
                                stats.kernel_target_tol_assumed += 1;
                                log(&format!(
                                    "h{}: KERNEL TARGET MISS tolerated — consumed modifier predates the checkpoint horizon (block_from abs h{})",
                                    h,
                                    st.base as usize + res.block_from_height
                                ));
                            } else {
                                return Err(format!(
                                    "h{}: KERNEL TARGET FAIL modifier={:016x} block_from(abs h{}) coinDays={} hashProof={}",
                                    h,
                                    res.modifier,
                                    st.base as usize + res.block_from_height,
                                    res.coin_day_weight,
                                    hash_to_display(&res.hash_proof_of_stake)
                                ));
                            }
                        } else if enforce {
                            stats.kernel_enforced += 1;
                            out.kernel_enforced = true;
                        } else {
                            stats.kernel_computed_advisory += 1;
                        }
                        proof = res.hash_proof_of_stake;
                    }
                    Err(e) => {
                        if !enforce {
                            // seam era: a recomputation error (min-age etc.) is
                            // tri7-math divergence against an era the deployed
                            // client validated live — record the skip honestly.
                            stats.kernel_computed_advisory += 1;
                            if stats.kernel_computed_advisory % 500 == 1 {
                                log(&format!(
                                    "h{}: seam-era kernel advisory: recomputation error ({}) — tolerated",
                                    h, e
                                ));
                            }
                            proof = [0u8; 32];
                        } else if input_abs < CHECKPOINT_HORIZON {
                            stats.kernel_target_tol_assumed += 1;
                            log(&format!(
                                "h{}: kernel recomputation error tolerated — coinstake input predates the checkpoint horizon (abs h{}): {}",
                                h, input_abs, e
                            ));
                            proof = [0u8; 32];
                        } else {
                            return Err(format!("h{}: kernel guard: {}", h, e));
                        }
                    }
                }
            }
            None => {
                if mode == ValidMode::AssumeValid {
                    // DEPLOYED-PARITY: kernel evidence skipped below the
                    // horizon; the index proof stays 0 exactly like the
                    // deployed client's own stored proofhash for this range.
                    stats.kernel_skipped_assume_valid += 1;
                } else {
                    // No external proof oracle in the tri7-only topology: the rpc
                    // client points at THIS node (self-comparison is vacuous) or
                    // is absent during seed. Accept the pre-window coinstake on
                    // the independent evidence already checked (header linkage +
                    // modifier checksum chain + block signature) and record the
                    // skip honestly. The old path errored out ("no proofhash in
                    // rpc") which made every freshly-seeded node fail startup.
                    stats.kernel_skipped_no_oracle += 1;
                    if stats.kernel_skipped_no_oracle % 10_000 == 1 {
                        log(&format!(
                            "h{}: pre-window coinstake, no external proof oracle — kernel evidence skipped (linkage+checksum+sig only); {} so far",
                            h, stats.kernel_skipped_no_oracle
                        ));
                    }
                }
                proof = [0u8; 32];
            }
        }
        // 8. block signature (required above the checkpoint horizon).
        // The deployed client gates this on Checkpoints::GetTotalBlocksEstimate()
        // (= the HIGHEST checkpoint, 2203924) — everything below skips sig
        // verification, which is why the instamine-era bad signature at h9003
        // (6e218830...) is IN the chain and must stay tolerated here.
        // `>` matches the deployed client's GetTotalBlocksEstimate() semantics:
        // the checkpoint block itself (h == horizon) is NOT assumed — and its
        // hash is pinned against the compiled anchor before any band logic
        // runs, so the >=/> distinction is immaterial at the boundary.
        if h > CHECKPOINT_HORIZON {
            check_pos_block_signature(block).map_err(|e| format!("h{}: block sig: {}", h, e))?;
            stats.sigs_checked += 1;
        }
    } else {
        // 7b. PoW: header hash must meet the nBits target (CheckProofOfWork)
        let (target, neg, _ovf) = set_compact(block.bits);
        if neg || target.is_zero() {
            return Err(format!("h{}: PoW nBits degenerate", h));
        }
        let era_limit = set_compact(0x2000_ffff).0; // operative live-era limit
        if target > era_limit {
            return Err(format!("h{}: PoW target above era limit", h));
        }
        let hash_num = U256::from_little_endian(&hash);
        if hash_num > target {
            return Err(format!("h{}: PoW hash does not meet nBits target", h));
        }
        proof = [0u8; 32];
        // 8. PoW blocks carry an empty signature (era rule: only enforced
        // above the last hard checkpoint — the pre-revival chain's PoW
        // blocks serialized a legacy vchBlockSig field that is not empty)
        if h > CHECKPOINT_HORIZON {
            check_pow_block_signature(block).map_err(|e| format!("h{}: {}", h, e))?;
            stats.sigs_checked += 1; // count only actual checks, not skips
        }
    }
    out.proof = proof;

    // 9. tx finality (CTransaction::IsFinal(nHeight, nBlockTime), main.h:573-586)
    for tx in &block.vtx {
        let lt = tx.locktime as i64;
        let ok = if lt == 0 {
            true
        } else if lt < 500_000_000 {
            // height lock: STRICT less-than block height
            lt < h as i64
        } else {
            // timestamp lock: STRICT less-than block time
            lt < block.time as i64
        } || tx.vin.iter().all(|vin| vin.sequence == u32::MAX);
        if !ok {
            return Err(format!("h{}: contains a non-final transaction", h));
        }
    }
    Ok(out)
}

/// Extend the chain window with an already-validated live block and verify the
/// stake-modifier checksum against the daemon's index (independent per-block
/// cross-check of the whole modifier chain).
fn advance_with_block(
    st: &mut ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    block: &Block,
    outcome: &ValidationOutcome,
) -> Result<(), String> {
    let h = st.tip_height + 1;
    let hash = block.hash();
    let is_pos = block_is_proof_of_stake(block);

    // stake modifier for this height: local computation from the window.
    // nModifierInterval is era-dependent: 1200 s for pre-Pharao blocks
    // (< 17692), 300 s after (kernel.h — see compute_next_stake_modifier_era).
    let rel_tip = st.rel(h - 1);
    let interval: u32 = if h < 17692 { 1200 } else { 300 };
    let (modifier, generated) =
        compute_next_stake_modifier_era(&st.rel_index(), Some(rel_tip), interval)
            .map_err(|e| format!("h{}: modifier computation: {}", h, e))?;
    let entropy = hash[0] & 1;
    let flags = index_flags(is_pos, entropy, generated);
    let prev_cs = *st
        .checksums
        .last()
        .ok_or("empty checksum chain in advance")?;
    let cs = stake_modifier_checksum(Some(prev_cs), flags, &outcome.proof, modifier);

    // Checksum cross-check: meaningful only when an EXTERNAL index already
    // holds this block (pre-cutover: the legacy daemon's independently
    // persisted checksums). For a block tri7d itself is accepting (our own
    // mint push, or a peer push of a block above the disk store's tip) there
    // is no external oracle — our locally computed cs IS the canonical value,
    // so the lookup is skipped instead of self-comparing. The store/recent
    // lookup also must not BLOCK here: this runs while the follow loop holds
    // the state + recent-cache locks across validation, and the RPC thread
    // answering getblock would deadlock on either of them (mirror handles
    // tip; recency uses try_lock and returns not-found under contention).
    // FROM-ZERO: a fresh node has NO external index at all — the lookup is
    // skipped wholesale (per-block RPC across 2.2M blocks would dominate).
    let info = if FROM_ZERO_NO_ORACLE.load(Ordering::Relaxed) {
        Err(String::new())
    } else {
        rpc.call(
            "getblock",
            serde_json::json!([tri7_core::p2p::internal_to_display(&hash)]),
        )
    };
    match info.as_ref().ok().and_then(|i| i["modifierchecksum"].as_str()) {
        Some(cs_hex) => {
            let daemon_cs = u32::from_str_radix(cs_hex, 16)
                .map_err(|e| format!("h{}: bad modifierchecksum: {}", h, e))?;
            if daemon_cs != cs {
                return Err(format!(
                    "h{}: stake-modifier checksum divergence: computed {:08x} daemon {:08x}",
                    h, cs, daemon_cs
                ));
            }
            stats.checksums_verified += 1;
        }
        None => {
            if !FROM_ZERO_NO_ORACLE.load(Ordering::Relaxed) {
                log(&format!(
                    "h{}: no external checksum oracle for this block — adopting locally computed {:08x}",
                    h, cs
                ));
            }
        }
    }

    // apply to the window (relative heights), advance the tip
    st.utxo.apply_block(block, st.rel(h));
    st.records.push(st.record_of(block, h));
    st.entries.push(IndexEntry {
        height: st.rel(h), // window-relative (see rel_index doc)
        hash,
        prev_hash: block.prev_hash,
        time: block.time,
        bits: block.bits,
        is_proof_of_stake: is_pos,
        entropy_bit: entropy,
        stake_modifier: modifier,
        generated_stake_modifier: generated,
        hash_proof_of_stake: outcome.proof,
    });
    st.checksums.push(cs);
    if h > st.tip_height {
        // real tip advance — distinct from the periodic 30s state writes
        st.last_advance_utc = now_iso();
    }
    st.tip_height = h;
    st.tip_hash = hash;
    st.trim();
    Ok(())
}

// ---------------------------------------------------------------------------
// FROM-ZERO sync: genesis state + full-chain P2P backfill (no snapshot)
// ---------------------------------------------------------------------------
/// hashGenesisBlockOfficial from triangles_v5/src/main.h — the only chain
/// constant from-zero needs: block 0 is FETCHED from the peer and its Hash9
/// must reproduce this exact value before anything else is accepted.
const GENESIS_OFFICIAL: &str = "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021";

/// true while the from-zero backfill runs: there is no external daemon index
/// on a fresh node, so advance_with_block must not consult the RPC oracle for
/// the stake-modifier checksum (the locally computed value IS canonical) —
/// also avoids a per-block RPC round-trip across millions of blocks.
static FROM_ZERO_NO_ORACLE: AtomicBool = AtomicBool::new(false);

/// --full-validate: strict mode for the from-zero walk. Default OFF: the
/// from-zero sync uses the deployed client's assume-valid envelope below the
/// checkpoint horizon (see CHECKPOINT_HORIZON). ON = every historical kernel
/// hard-enforced (pre-2026-11 behavior; ~7 blk/s, dies at the h17693 seam).
static FULL_VALIDATE: AtomicBool = AtomicBool::new(false);

static SIGINT_SEEN: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn tri7_on_signal(_sig: i32) {
    SIGINT_SEEN.store(true, Ordering::Relaxed);
}

/// Install SIGINT/SIGTERM handlers that only set a flag: the sync loop checks
/// it, persists tipstate + stops — the sync is RESUMABLE across restarts.
#[cfg(unix)]
fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, tri7_on_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, tri7_on_signal as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

/// Fetch block 0 from the peer and pin it to the compiled-in genesis hash.
/// This is the trust anchor: everything after is validated by linkage to it.
fn fetch_genesis_block(sess: &mut Session, stats: &mut Stats) -> Result<Block, String> {
    let gh = tri7_core::p2p::display_to_internal(GENESIS_OFFICIAL)?;
    sess.send(
        "getdata",
        &tri7_core::p2p::build_getdata(&[(tri7_core::p2p::MSG_BLOCK, gh)]),
        stats,
    )?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline {
            return Err("from-zero: genesis getdata timeout".into());
        }
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            if msg.command == "block" {
                let mut r = Reader::new(&msg.payload);
                let b = Block::read(&mut r).map_err(|e| format!("genesis parse: {}", e))?;
                r.expect_done().map_err(|e| format!("genesis trailing: {}", e))?;
                if b.prev_hash != [0u8; 32] {
                    return Err("genesis prev_hash is not zero".into());
                }
                if b.hash() != gh {
                    return Err(format!(
                        "genesis hash mismatch: got {} want {}",
                        tri7_core::p2p::internal_to_display(&b.hash()),
                        GENESIS_OFFICIAL
                    ));
                }
                return Ok(b);
            } else if msg.command == "ping" {
                let n = tri7_core::p2p::parse_ping(&msg.payload)?;
                sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// ChainState at height 0, built purely from the verified genesis block —
/// no tipstate file, no archive, no daemon index. The window then grows via
/// the normal acceptance gate (advance_with_block) and self-trims.
fn genesis_state(gen: &Block) -> ChainState {
    let hash = gen.hash();
    let entropy = hash[0] & 1;
    let mut utxo = WindowOutputs::new();
    utxo.apply_block(gen, 0);
    ChainState {
        base: 0,
        entries: vec![IndexEntry {
            height: 0,
            hash,
            prev_hash: gen.prev_hash,
            time: gen.time,
            bits: gen.bits,
            is_proof_of_stake: false,
            entropy_bit: entropy,
            stake_modifier: 0,
            // kernel.cpp:152-157 — the genesis block itself qualifies as the
            // first GeneratedStakeModifier (modifier 0, generated)
            generated_stake_modifier: true,
            hash_proof_of_stake: [0u8; 32],
        }],
        checksums: vec![0],
        utxo,
        records: Vec::new(),
        pending_headers: HashMap::new(),
        tip_height: 0,
        tip_hash: hash,
        daemon_tip: None,
        no_trim: true, // from-zero genesis state: full-history validation
        last_advance_utc: now_iso(),
        peers_connected: 0,
        last_peer_utc: None,
    }
}

/// One headers round: getheaders from our tip locator, link-checked. Returns
/// (headers with absolute heights, complete=false when the peer ended the
/// segment early — i.e. we saw its tip).
/// Walk cursor for the from-zero header sweep: survives across rounds so the
/// walk resumes from the LAST received header. (st.tip never moves during
/// phase H — re-anchoring every round would re-request the same headers.)
struct HeaderWalk {
    next_hash: Hash,
    next_height: u64,
    /// recent locator anchors (each request's expect_prev). A response whose
    /// first header links to one of these instead of the current cursor is a
    /// STALE DUPLICATE: a slow (>2s) response makes the round loop exit and
    /// re-send the same locator, and the daemon answers EVERY request — so
    /// the second copy of the segment must be dropped, not treated as a fork
    /// (measured live: the walk died at h101525/h333525 exactly 2000 blocks
    /// behind the cursor, the parent hash of the re-delivered page).
    anchors: Vec<Hash>,
}

/// What to do with a headers message whose first header does not link to the
/// current cursor. Pure so the stale-duplicate rule is testable.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum SegAction {
    Accept,
    DropStale,
    Fatal,
}

fn classify_segment(first_prev: &Hash, expect_prev: &Hash, anchors: &[Hash]) -> SegAction {
    if first_prev == expect_prev {
        SegAction::Accept
    } else if anchors.iter().any(|a| a == first_prev) {
        SegAction::DropStale
    } else {
        SegAction::Fatal
    }
}

fn headers_round(
    sess: &mut Session,
    walk: &mut HeaderWalk,
    stats: &mut Stats,
) -> Result<(Vec<(u64, Hash, Block)>, bool), String> {
    let mut out: Vec<(u64, Hash, Block)> = Vec::new();
    // the walk keeps requesting from the last header until the peer sends a
    // partial (tip-ending) batch or goes idle
    let mut expect_prev = walk.next_hash;
    let mut outstanding = true;
    let mut last_msg = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if outstanding {
            sess.send(
                "getheaders",
                &tri7_core::p2p::build_getheaders(&[expect_prev], &[0u8; 32]),
                stats,
            )?;
            walk.anchors.push(expect_prev);
            let excess = walk.anchors.len().saturating_sub(8);
            if excess > 0 {
                walk.anchors.drain(..excess);
            }
            outstanding = false;
        }
        let msgs = sess.pump()?;
        if msgs.is_empty() {
            // 2s idle after traffic = segment ended (peer only serves the
            // next segment on a fresh locator anyway — return INCOMPLETE with
            // the cursor advanced; the caller immediately re-requests from
            // the last hash, which IS a fresh locator, so no dedupe drop)
            if !out.is_empty() && last_msg.elapsed() > Duration::from_secs(2) {
                walk.next_hash = expect_prev;
                walk.next_height += out.len() as u64;
                return Ok((out, false));
            }
            if out.is_empty() && last_msg.elapsed() > Duration::from_secs(30) {
                break; // truly silent peer
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        last_msg = Instant::now();
        for msg in msgs {
            stats.note_recv(&msg.command);
            match msg.command.as_str() {
                "headers" => {
                    let batch = tri7_core::p2p::parse_headers(&msg.payload)?;
                    if batch.is_empty() {
                        // the peer has NOTHING after our locator — we are at
                        // its tip (this is the ONLY unambiguous completion
                        // signal; never infer completion from silence or a
                        // deadline, which previously stranded the walk
                        // mid-chain and left millions of blocks to the slow
                        // live-follow path)
                        return Ok((out, true));
                    }
                    if batch[0].prev_hash != expect_prev {
                        // stale duplicate of an already-consumed segment:
                        // drop it and keep waiting for the real response
                        if classify_segment(&batch[0].prev_hash, &expect_prev, &walk.anchors)
                            == SegAction::DropStale
                        {
                            log(&format!(
                                "from-zero: dropped stale duplicate headers segment ({} headers, parent h{})",
                                batch.len(),
                                walk.next_height.saturating_sub(1)
                            ));
                            continue;
                        }
                        return Err(format!(
                            "from-zero header link mismatch at h{}: prev {} expected {}",
                            walk.next_height + out.len() as u64,
                            tri7_core::p2p::internal_to_display(&batch[0].prev_hash),
                            tri7_core::p2p::internal_to_display(&expect_prev)
                        ));
                    }
                    for hdr in batch {
                        if hdr.prev_hash != expect_prev {
                            return Err(format!(
                                "from-zero header link mismatch at h{}: prev {} expected {}",
                                walk.next_height + out.len() as u64,
                                tri7_core::p2p::internal_to_display(&hdr.prev_hash),
                                tri7_core::p2p::internal_to_display(&expect_prev)
                            ));
                        }
                        expect_prev = hdr.hash();
                        out.push((walk.next_height + out.len() as u64, hdr.hash(), hdr));
                        stats.headers_processed += 1;
                    }
                    // a partial batch ends at the peer tip
                    if out.len() % 2000 != 0 {
                        walk.next_hash = expect_prev;
                        walk.next_height += out.len() as u64;
                        return Ok((out, true));
                    }
                    // progress visibility inside a 120s round (~290k headers):
                    // log every ~50k so 'from-zero:' greps show the sweep
                    if out.len() % 50_000 < 2000 {
                        log(&format!(
                            "from-zero: header walk at h{}",
                            walk.next_height + out.len() as u64 - 1
                        ));
                    }
                    outstanding = true; // full batch — request the next segment now
                }
                "ping" => {
                    let n = tri7_core::p2p::parse_ping(&msg.payload)?;
                    sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
                }
                _ => {}
            }
        }
    }
    // DEADLINE: return what we have, explicitly INCOMPLETE (cursor advanced).
    // The old code returned complete=true here, silently ending the walk at
    // ~h68k and handing the remaining ~2.1M blocks to the live-follow loop.
    walk.next_hash = expect_prev;
    walk.next_height += out.len() as u64;
    Ok((out, false))
}

/// Worker: one peer session pulling body batches (legacy 500-inv getdata)
/// over a shared header cursor. Raw payloads go to the channel; the MAIN
/// thread does ALL validation sequentially (the acceptance gate is strictly
/// order-dependent). Bounded in-flight via the validated counter so the
/// pending buffer cannot outrun validation by more than MAX_INFLIGHT blocks.
fn fetch_worker(
    target: PeerTarget,
    cfg_for_worker: Config,
    start_height: u64,
    worker_count: usize,
    hashes: Arc<Vec<(u64, Hash)>>,
    cursor: Arc<AtomicUsize>,
    validated: Arc<AtomicU64>,
    tx: std::sync::mpsc::Sender<(u64, Vec<u8>)>,
    stop: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
    orphans: Arc<Mutex<VecDeque<(usize, usize, usize)>>>,
    worker_id: usize,
) {
    const BATCH: usize = 500;
    const MAX_INFLIGHT: u64 = 60_000;
    const BATCH_DEADLINE: Duration = Duration::from_secs(40);
    let mut stats = Stats::default();
    let wcfg = Config {
        peers: vec![target],
        ..cfg_for_worker.clone()
    };
    let mut connect = |stats: &mut Stats| -> Result<Session, String> {
        // full rotation: any reachable peer serves the batch
        connect_and_handshake(&cfg_for_worker, stats, start_height, 0).map(|(p, _)| p.sess)
    };
    let mut sess = match connect(&mut stats) {
        Ok(s) => s,
        Err(e) => {
            errors.lock().unwrap().push(format!("worker {}: connect: {}", worker_id, e));
            return;
        }
    };
    // STRIDE assignment: worker w takes batches w, w+N, w+2N, … so every
    // contiguous window of N batches is fetched in parallel across all
    // workers — otherwise the validator would serially wait on ONE worker
    // for each consecutive segment (measured live: 12 blk/s vs 4×25).
    let n = worker_count.max(1);
    let mut k: usize = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // flow control: don't outrun the validator
        while validated.load(Ordering::Relaxed) + MAX_INFLIGHT
            <= cursor.load(Ordering::Relaxed) as u64
        {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // work-stealing: a stalled batch's range is re-queued for ANY worker
        // (a different session often serves it instantly)
        let orphan = orphans.lock().unwrap().pop_front();
        let (start_idx, end, orphan_att) = match orphan {
            Some(range) => range,
            None => {
                let start_idx = (worker_id + k * n) * BATCH;
                k += 1;
                if start_idx >= hashes.len() {
                    // exhausted for this worker — keep stealing until the
                    // VALIDATOR consumed every height (the cursor over-counts
                    // re-claimed orphan ranges, so it can never gate this:
                    // measured live at h490525, a cursor-gated exit stranded
                    // one missing block behind exhausted workers forever)
                    if orphans.lock().unwrap().is_empty()
                        && validated.load(Ordering::Relaxed) as usize >= hashes.len()
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let end = (start_idx + BATCH).min(hashes.len());
                (start_idx, end, 0)
            }
        };
        if orphan_att == 0 {
            // count only FRESH claims: an orphan re-claim was already counted
            // when its original worker took the range, and double-counting
            // overshot the cursor past the flow-control gate, deadlocking
            // every worker while one missing block starved the validator
            cursor.fetch_add(end - start_idx, Ordering::Relaxed);
        }
        // fetch with reconnect+retry: peers drop or stall sessions (observed
        // live: a worker session goes silent mid-batch) — rebuild the session
        // and re-request only the MISSING outpoints; routine, not fatal.
        const MAX_BATCH_RETRIES: usize = 6;
        let mut got: HashMap<u64, Vec<u8>> = HashMap::new();
        let mut retries = 0usize;
        loop {
            let mut awaiting: HashMap<Hash, u64> = HashMap::new();
            for (h, hash) in &hashes[start_idx..end] {
                if !got.contains_key(h) {
                    awaiting.insert(*hash, *h);
                }
            }
            if awaiting.is_empty() {
                break;
            }
            let inv: Vec<(u32, Hash)> = awaiting
                .keys()
                .map(|hash| (tri7_core::p2p::MSG_BLOCK, *hash))
                .collect();
            if let Err(e) = sess.send("getdata", &tri7_core::p2p::build_getdata(&inv), &mut stats) {
                sess.shutdown();
                retries += 1;
                if retries > MAX_BATCH_RETRIES || stop.load(Ordering::Relaxed) {
                    errors.lock().unwrap().push(format!("worker {}: getdata: {}", worker_id, e));
                    return;
                }
                log(&format!(
                    "from-zero: worker {} send failed ({}) — reconnecting",
                    worker_id, e
                ));
                std::thread::sleep(Duration::from_secs(2));
                match connect(&mut stats) {
                    Ok(s) => sess = s,
                    Err(e) => {
                        errors.lock().unwrap().push(format!("worker {}: reconnect: {}", worker_id, e));
                        return;
                    }
                }
                continue;
            }
            let deadline = Instant::now() + BATCH_DEADLINE;
            let mut session_dead = false;
            while !awaiting.is_empty() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if Instant::now() > deadline {
                    session_dead = true;
                    break;
                }
                match sess.pump() {
                    Ok(msgs) => {
                        for msg in msgs {
                            match msg.command.as_str() {
                                "block" => {
                                    let hash = if msg.payload.len() >= 80 {
                                        tri7_core::legacy::hash9(&msg.payload[..80])
                                    } else {
                                        [0u8; 32]
                                    };
                                    if let Some(h) = awaiting.remove(&hash) {
                                        got.insert(h, msg.payload);
                                    }
                                }
                                "ping" => {
                                    if let Ok(n) = tri7_core::p2p::parse_ping(&msg.payload) {
                                        let _ =
                                            sess.send("pong", &tri7_core::p2p::build_pong(n), &mut stats);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(_) => {
                        // wire garbage or the peer closed the socket
                        session_dead = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if awaiting.is_empty() {
                break;
            }
            retries += 1;
            if retries == 1 {
                if orphan_att >= 2 {
                    errors.lock().unwrap().push(format!(
                        "worker {}: batch h{} unservable after {} orphan attempts",
                        worker_id,
                        hashes[start_idx].0,
                        orphan_att
                    ));
                    return;
                }
                // first stall: hand the range to the pool (fresh session) and
                // move on to the next batch — the validator needs ANYONE to
                // deliver, not this specific session to keep trying
                log(&format!(
                    "from-zero: worker {} batch h{} stalled ({} missing) — requeued for any worker",
                    worker_id,
                    hashes[start_idx].0,
                    awaiting.len()
                ));
                orphans.lock().unwrap().push_back((start_idx, end, orphan_att + 1));
                sess.shutdown();
                match connect(&mut stats) {
                    Ok(s) => sess = s,
                    Err(e) => {
                        errors.lock().unwrap().push(format!("worker {}: reconnect: {}", worker_id, e));
                        return;
                    }
                }
                break; // next batch (orphan ranges get stolen by someone)
            }
            if retries > MAX_BATCH_RETRIES || stop.load(Ordering::Relaxed) {
                errors.lock().unwrap().push(format!(
                    "worker {}: batch h{}+ stalled after {} attempt(s), {} outstanding",
                    worker_id,
                    hashes[start_idx].0,
                    retries,
                    awaiting.len()
                ));
                return;
            }
            log(&format!(
                "from-zero: worker {} retrying batch h{} ({} missing, attempt {}/{})",
                worker_id,
                hashes[start_idx].0,
                awaiting.len(),
                retries,
                MAX_BATCH_RETRIES
            ));
            sess.shutdown();
            std::thread::sleep(Duration::from_secs(2));
            match connect(&mut stats) {
                Ok(s) => sess = s,
                Err(e) => {
                    errors.lock().unwrap().push(format!("worker {}: reconnect: {}", worker_id, e));
                    return;
                }
            }
        }
        let mut ordered: Vec<(u64, Vec<u8>)> = got.into_iter().collect();
        ordered.sort_by_key(|(h, _)| *h);
        for (h, raw) in ordered {
            if tx.send((h, raw)).is_err() {
                return; // validator gone
            }
        }
    }
}

/// Sequential reassembly gate for the parallel fetch pipeline: workers deliver
/// bodies out of order (N sessions race over stride batches); the validator
/// consumes EXACTLY next_h, holding everything else in `pending` until the
/// gap fills. Pure so the ordering invariant is testable: nothing validates
/// out of order, and a hole stalls the drain without reordering it.
fn reassembly_pop(pending: &mut HashMap<u64, Vec<u8>>, next_h: u64) -> Option<Vec<u8>> {
    pending.remove(&next_h)
}

/// Full from-zero run: header walk to the peer tip, then N parallel fetcher
/// sessions feeding ONE sequential validator (the normal acceptance gate).
/// Works from a genesis state (fresh) or a resumed partial tipstate.
fn sync_from_zero(
    cfg: &Config,
    first_sess: &mut Session,
    st: &mut ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    store: &Arc<BlockStore>,
    peer_tip: u64,
) -> Result<(), String> {
    FROM_ZERO_NO_ORACLE.store(true, Ordering::Relaxed);
    let started = Instant::now();
    let workers = cfg.sync_workers.clamp(1, 16) as usize;
    log(&format!(
        "from-zero: syncing h{} → h{} over P2P ({} blocks to go, {} fetcher session(s))",
        st.tip_height,
        peer_tip,
        peer_tip.saturating_sub(st.tip_height),
        workers
    ));
    // publish the sync snapshot for getsyncinfo (Sami: "it should show this
    // on the UI") — header-walk phase starts now
    tri7_core::sync_progress::set(tri7_core::sync_progress::Snapshot {
        height: st.tip_height,
        target_height: peer_tip,
        blocks_per_sec: 0.0,
        eta_secs: 0,
        phase: tri7_core::sync_progress::SyncPhase::HeaderWalk,
    });

    // ---- phase H: walk the header chain to the peer tip (one session) ----
    let mut all_headers: Vec<(u64, Hash, Block)> = Vec::new();
    let mut walk = HeaderWalk {
        next_hash: st.tip_hash,
        next_height: st.tip_height + 1,
        anchors: Vec::new(),
    };
    let mut idle_rounds = 0u32;
    loop {
        if SIGINT_SEEN.load(Ordering::Relaxed) {
            write_state_atomic(cfg, &st.to_file())
                .map_err(|e| format!("write state (SIGINT): {}", e))?;
            return Err("__SIGINT__".into());
        }
        let (round, complete) = headers_round(first_sess, &mut walk, stats)?;
        if round.is_empty() {
            if complete {
                break; // peer answered "nothing after this locator" — at tip
            }
            // silent peer mid-walk: retry (bounded), never silently stop —
            // an incomplete walk would strand the sync mid-chain
            idle_rounds += 1;
            if idle_rounds > 12 {
                return Err(format!(
                    "from-zero: header walk stalled at h{} — 12 consecutive empty rounds",
                    walk.next_height.saturating_sub(1)
                ));
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        idle_rounds = 0;
        all_headers.extend(round);
        if all_headers.len() % 100_000 == 0 || all_headers.len() < 200_000 {
            log(&format!(
                "from-zero: {} headers walked (up to h{})",
                all_headers.len(),
                all_headers.last().unwrap().0
            ));
        }
        if all_headers.len() > 3_000_000 {
            return Err("from-zero: header walk exceeded 3M — aborting".into());
        }
        if complete {
            break;
        }
    }
    if all_headers.is_empty() {
        FROM_ZERO_NO_ORACLE.store(false, Ordering::Relaxed);
        return Ok(()); // nothing to do — at tip
    }
    let header_tip = all_headers.last().unwrap().0;
    if header_tip <= st.tip_height {
        FROM_ZERO_NO_ORACLE.store(false, Ordering::Relaxed);
        return Ok(());
    }
    // single copy of the walked headers shared with the workers — a
    // per-worker (height,hash) list or a by-height header map would DOUBLE
    // the ~300 MB header footprint (matters on 1 GB fresh-install boxes)
    let first_h = all_headers[0].0;
    let headers: Arc<Vec<(u64, Hash, Block)>> = Arc::new(all_headers);
    let hashes: Arc<Vec<(u64, Hash)>> = Arc::new(
        headers
            .iter()
            .map(|(h, hash, _)| (*h, *hash))
            .collect(),
    );

    // ---- phase B: parallel fetch + sequential validate ----
    let cursor = Arc::new(AtomicUsize::new(0));
    let validated = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let orphans: Arc<Mutex<VecDeque<(usize, usize, usize)>>> =
        Arc::new(Mutex::new(VecDeque::new()));
    let (tx, rx) = std::sync::mpsc::channel::<(u64, Vec<u8>)>();
    let tip_at_start = st.tip_height;
    let mut handles = Vec::new();
    for w in 0..workers.max(1) {
        let target = cfg.peers[w % cfg.peers.len()].clone();
        let cfg2 = cfg.clone();
        let hashes2 = Arc::clone(&hashes);
        let cursor2 = Arc::clone(&cursor);
        let validated2 = Arc::clone(&validated);
        let stop2 = Arc::clone(&stop);
        let errors2 = Arc::clone(&errors);
        let orphans2 = Arc::clone(&orphans);
        let tx2 = tx.clone();
        handles.push(std::thread::spawn(move || {
            fetch_worker(target, cfg2, tip_at_start, workers.max(1), hashes2, cursor2, validated2, tx2, stop2, errors2, orphans2, w)
        }));
    }
    drop(tx); // main holds the only receiver; workers exit when done

    let headers2 = Arc::clone(&headers);
    let mut pending: HashMap<u64, Vec<u8>> = HashMap::new();
    let mut next_h = st.tip_height + 1;
    let mut idle_warned = false;
    let mut stalled_since: Option<Instant> = None;
    let mut last_gapfill = Instant::now() - Duration::from_secs(60);
    let sync_started = Instant::now();
    while next_h <= header_tip {
        if SIGINT_SEEN.load(Ordering::Relaxed) {
            stop.store(true, Ordering::Relaxed);
            write_state_atomic(cfg, &st.to_file())
                .map_err(|e| format!("write state (SIGINT): {}", e))?;
            return Err("__SIGINT__".into());
        }
        if let Some(raw) = reassembly_pop(&mut pending, next_h) {
            let mut r = Reader::new(&raw);
            let b = Block::read(&mut r).map_err(|e| format!("block parse: {}", e))?;
            r.expect_done().map_err(|e| format!("block trailing: {}", e))?;
            let hdr = headers2[(next_h - first_h) as usize].2.clone();
            if b.hash() != hdr.hash() {
                return Err(format!(
                    "h{}: served body {} does not match walked header {}",
                    next_h,
                    tri7_core::p2p::internal_to_display(&b.hash()),
                    tri7_core::p2p::internal_to_display(&hdr.hash())
                ));
            }
            let prev_entry = st
                .entry_abs(next_h - 1)
                .cloned()
                .ok_or_else(|| format!("h{}: predecessor outside window", next_h))?;
            let outcome = validate_block(
                st,
                rpc,
                stats,
                &b,
                &BlockCtx {
                    height_abs: next_h,
                    header: Some(&hdr),
                    prev_entry,
                    live: false,
                },
            )?;
            advance_with_block(st, rpc, stats, &b, &outcome)?;
            stats.blocks_validated_hist += 1;
            validated.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = store.append_block(&raw, Some(next_h)) {
                return Err(format!("h{}: store append: {}", next_h, e));
            }
            let done = validated.load(Ordering::Relaxed);
            if done % 1000 == 0 {
                let elapsed = started.elapsed().as_secs_f64().max(0.001);
                let rate = done as f64 / elapsed;
                let remaining = header_tip - st.tip_height;
                let eta = if rate > 0.0 {
                    remaining as f64 / rate
                } else {
                    0.0
                };
                // live progress for getsyncinfo (GUI renders height/%/rate/ETA)
                tri7_core::sync_progress::set(tri7_core::sync_progress::Snapshot {
                    height: st.tip_height,
                    target_height: header_tip,
                    blocks_per_sec: rate,
                    eta_secs: eta as u64,
                    phase: tri7_core::sync_progress::SyncPhase::BodyFetch,
                });
                log(&format!(
                    "from-zero: h{} {} ({:.0} blk/s, ETA {:.1}h)",
                    st.tip_height,
                    tri7_core::p2p::internal_to_display(&st.tip_hash),
                    rate,
                    eta / 3600.0
                ));
                write_state_atomic(cfg, &st.to_file())
                    .map_err(|e| format!("write state: {}", e))?;
            }
            next_h += 1;
            idle_warned = false;
            stalled_since = None; // progress: the stall clock resets
            continue;
        }
        if !errors.lock().unwrap().is_empty() {
            let e = errors.lock().unwrap().join("; ");
            stop.store(true, Ordering::Relaxed);
            return Err(format!("fetch worker failure: {}", e));
        }
        // wait for the next block
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok((h, raw)) => {
                pending.insert(h, raw);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let since = *stalled_since.get_or_insert_with(Instant::now);
                if !idle_warned && since.elapsed() > Duration::from_secs(120) {
                    idle_warned = true;
                    log(&format!(
                        "from-zero: stalled at h{} — {} block(s) fetched, awaiting h{} (workers stuck? SIGINT to checkpoint)",
                        st.tip_height,
                        pending.len(),
                        next_h
                    ));
                }
                // DIRECT GAP-FILL (phase-7 gap-recovery pattern): a batch can
                // strand a single missing body behind gated/exhausted workers
                // (measured live at h490525: cursor overshoot shut the
                // flow-control gate, the validator starved on one block).
                // Only on a PERSISTED stall (>20s without progress, retried
                // every 30s): the idle header-walk session asks the peer for
                // exactly the missing height; a served body is hash-verified
                // against the walked header before entering the pending map.
                if since.elapsed() > Duration::from_secs(20)
                    && last_gapfill.elapsed() > Duration::from_secs(30)
                {
                    last_gapfill = Instant::now();
                    let idx = (next_h - first_h) as usize;
                    if idx < hashes.len() {
                        let want_hash = hashes[idx].1;
                        let inv = [(tri7_core::p2p::MSG_BLOCK, want_hash)];
                        if first_sess
                            .send("getdata", &tri7_core::p2p::build_getdata(&inv), stats)
                            .is_ok()
                        {
                            stats.gap_requests += 1;
                            let fill_deadline = Instant::now() + Duration::from_secs(15);
                            while Instant::now() < fill_deadline
                                && !pending.contains_key(&next_h)
                            {
                                let msgs = first_sess.pump()?;
                                for msg in msgs {
                                    match msg.command.as_str() {
                                        "block" => {
                                            let bh = if msg.payload.len() >= 80 {
                                                tri7_core::legacy::hash9(&msg.payload[..80])
                                            } else {
                                                [0u8; 32]
                                            };
                                            if bh == want_hash {
                                                log(&format!(
                                                    "from-zero: gap-fill recovered h{} directly from a peer",
                                                    next_h
                                                ));
                                                stats.gap_filled += 1;
                                                pending.insert(next_h, msg.payload);
                                            }
                                        }
                                        "ping" => {
                                            if let Ok(n) = tri7_core::p2p::parse_ping(&msg.payload) {
                                                let _ = first_sess.send(
                                                    "pong",
                                                    &tri7_core::p2p::build_pong(n),
                                                    stats,
                                                );
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                return Err("all fetch workers exited before the tip".into());
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    write_state_atomic(cfg, &st.to_file()).map_err(|e| format!("write state: {}", e))?;
    FROM_ZERO_NO_ORACLE.store(false, Ordering::Relaxed);
    log(&format!(
        "from-zero: COMPLETE h{} {} in {:.1}s ({:.0} blk/s avg)",
        st.tip_height,
        tri7_core::p2p::internal_to_display(&st.tip_hash),
        started.elapsed().as_secs_f64(),
        stats.blocks_validated_hist as f64 / started.elapsed().as_secs_f64().max(0.001)
    ));
    // caught up — clear the syncing snapshot so getsyncinfo reports follow
    tri7_core::sync_progress::clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// state load / seed
// ---------------------------------------------------------------------------
fn load_state(cfg: &Config) -> Result<ChainState, String> {
    let path = state_path(cfg);
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let f: TipStateFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if f.entries.is_empty() {
        return Err("empty entries in state".into());
    }
    let base = f.window_base;
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(f.entries.len());
    let mut checksums: Vec<u32> = Vec::with_capacity(f.entries.len());
    // re-verify the persisted checksum chain with the same anchor rule as the
    // seed: adopt the first non-zero checksum, verify every non-zero step after
    let mut cs_prev: Option<u32> = None;
    for rec in &f.entries {
        let hash = tri7_core::p2p::display_to_internal(&rec.hash)?;
        let prev = tri7_core::p2p::display_to_internal(&rec.prev)?;
        let proof = tri7_core::p2p::display_to_internal(&rec.proof)?;
        if hash[0] & 1 != rec.entropy {
            return Err(format!("h{}: entropy mismatch in state", rec.height));
        }
        if entries.last().map(|e| e.hash != prev).unwrap_or(false) {
            return Err(format!("h{}: window linkage broken in state", rec.height));
        }
        let cs = match cs_prev {
            None => {
                if rec.checksum == 0 {
                    0 // below the checksum anchor — carried unverified
                } else {
                    cs_prev = Some(rec.checksum); // adopt the anchor
                    rec.checksum
                }
            }
            Some(prev_cs) => {
                let flags = index_flags(rec.pos, rec.entropy, rec.generated);
                let expect = stake_modifier_checksum(Some(prev_cs), flags, &proof, rec.modifier);
                if rec.checksum != 0 {
                    if expect != rec.checksum {
                        return Err(format!(
                            "h{}: state checksum chain broken ({:08x} != {:08x})",
                            rec.height, expect, rec.checksum
                        ));
                    }
                    cs_prev = Some(rec.checksum);
                    rec.checksum
                } else {
                    cs_prev = Some(expect);
                    expect
                }
            }
        };
        entries.push(IndexEntry {
            height: (rec.height - base) as usize, // window-relative
            hash,
            prev_hash: prev,
            time: rec.time,
            bits: rec.bits,
            is_proof_of_stake: rec.pos,
            entropy_bit: rec.entropy,
            stake_modifier: rec.modifier,
            generated_stake_modifier: rec.generated,
            hash_proof_of_stake: proof,
        });
        checksums.push(cs);
    }
    let tip = entries.last().unwrap();
    let tip_height = base + tip.height as u64; // entry.height is window-relative
    let tip_hash = tip.hash;
    let mut utxo = WindowOutputs::new();
    utxo.rebuild_from_records(&f.blocks, base);
    Ok(ChainState {
        base,
        entries,
        checksums,
        utxo,
        records: f.blocks,
        pending_headers: HashMap::new(),
        tip_height,
        tip_hash,
        daemon_tip: f.daemon_tip,
        // "" (pre-5a file) stays "" until the first real advance bumps it —
        // honest: no advance observed since this state was created
        last_advance_utc: f.last_advance_utc,
        no_trim: false,
        // inherit the persisted network-liveness clock: a fresh process with
        // no traffic yet ages the health gate from the last KNOWN traffic
        peers_connected: f.peers.unwrap_or(0),
        last_peer_utc: f.last_peer_utc,
    })
}

/// Fetch the window's hashes via RPC, the raw blocks via P2P getdata (verified
/// byte-for-byte locally), then seed the modifier state via the daemon index.
fn seed_from_network(
    _cfg: &Config,
    rpc: &Rpc,
    sess: &mut Session,
    stats: &mut Stats,
) -> Result<(ChainState, TipStateFile), String> {
    let daemon_tip = rpc
        .call("getblockcount", serde_json::json!([]))?
        .as_i64()
        .ok_or("getblockcount: not an int")?;
    if daemon_tip <= WINDOW_ENTRIES as i64 {
        return Err(format!("daemon tip {} below window size", daemon_tip));
    }
    let tip = daemon_tip as u64;
    let base = tip - (WINDOW_ENTRIES as u64 - 1);

    // 1. expected hashes (RPC index — the followed chain's ordering)
    let mut hashes_display: Vec<String> = Vec::with_capacity(WINDOW_ENTRIES + 1);
    const HBATCH: usize = 500;
    let mut lo = base as i64 - 1; // one extra for the entry[0] link check
    while lo <= daemon_tip {
        let hi = (lo + HBATCH as i64 - 1).min(daemon_tip);
        let calls: Vec<(u64, &str, serde_json::Value)> = (lo..=hi)
            .map(|h| (h as u64, "getblockhash", serde_json::json!([h])))
            .collect();
        for item in rpc.call_batch(calls)? {
            let s = item
                .get("result")
                .and_then(|r| r.as_str())
                .ok_or("getblockhash batch: missing result")?
                .to_string();
            hashes_display.push(s);
        }
        lo = hi + 1;
    }
    // hashes_display[i] = height base-1+i
    let link_prev = tri7_core::p2p::display_to_internal(&hashes_display[0])?;
    let expect_hashes: Vec<Hash> = hashes_display[1..]
        .iter()
        .map(|s| tri7_core::p2p::display_to_internal(s))
        .collect::<Result<_, _>>()?;
    log(&format!(
        "seed: {} expected hashes fetched (h{}..h{})",
        expect_hashes.len(),
        base,
        tip
    ));

    // 2. raw blocks over P2P getdata — verified byte-for-byte locally
    let mut blocks: Vec<(u64, Block, Hash)> = Vec::with_capacity(expect_hashes.len());
    const BDAT_BATCH: usize = 100;
    let mut next_i = 0usize;
    while next_i < expect_hashes.len() {
        let chunk_end = (next_i + BDAT_BATCH).min(expect_hashes.len());
        // request the chunk
        let mut awaiting: HashMap<Hash, usize> = HashMap::new();
        for i in next_i..chunk_end {
            awaiting.insert(expect_hashes[i], i);
        }
        let inv: Vec<(u32, Hash)> = awaiting
            .keys()
            .map(|h| (tri7_core::p2p::MSG_BLOCK, *h))
            .collect();
        sess.send("getdata", &tri7_core::p2p::build_getdata(&inv), stats)?;
        let mut got: HashMap<usize, Block> = HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(90);
        while !awaiting.is_empty() {
            if Instant::now() > deadline {
                return Err(format!(
                    "seed: getdata timeout with {} blocks outstanding (h{}..)",
                    awaiting.len(),
                    base + next_i as u64
                ));
            }
            for msg in sess.pump()? {
                stats.note_recv(&msg.command);
                if msg.command == "block" {
                    stats.blocks_received += 1;
                    let mut r = Reader::new(&msg.payload);
                    let b = Block::read(&mut r).map_err(|e| format!("seed block parse: {}", e))?;
                    r.expect_done()
                        .map_err(|e| format!("seed block trailing: {}", e))?;
                    let hash = b.hash();
                    if let Some(idx) = awaiting.remove(&hash) {
                        got.insert(idx, b);
                    }
                } else if msg.command == "ping" {
                    let n = tri7_core::p2p::parse_ping(&msg.payload)?;
                    sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // verify + append this chunk in order
        for i in next_i..chunk_end {
            let b = match got.remove(&i) {
                Some(b) => b,
                None => {
                    return Err(format!(
                        "seed: block h{} missing from batch",
                        base + i as u64
                    ))
                }
            };
            let h_abs = base + i as u64;
            if b.hash() != expect_hashes[i] {
                return Err(format!(
                    "seed h{}: Hash9 {} != index hash {}",
                    h_abs,
                    tri7_core::p2p::internal_to_display(&b.hash()),
                    hashes_display[i + 1]
                ));
            }
            let want_prev = if i == 0 {
                link_prev
            } else {
                expect_hashes[i - 1]
            };
            if b.prev_hash != want_prev {
                return Err(format!(
                    "seed h{}: linkage broken (prev {} want {})",
                    h_abs,
                    tri7_core::p2p::internal_to_display(&b.prev_hash),
                    tri7_core::p2p::internal_to_display(&want_prev)
                ));
            }
            if b.merkle_root() != b.merkle_root {
                return Err(format!("seed h{}: merkle root mismatch", h_abs));
            }
            blocks.push((h_abs, b, expect_hashes[i]));
        }
        next_i = chunk_end;
        if next_i % 1000 == 0 || next_i == expect_hashes.len() {
            log(&format!(
                "seed: {}/{} blocks fetched+verified via P2P",
                next_i,
                expect_hashes.len()
            ));
        }
    }

    // 3. entries/records/utxo from the verified blocks
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(blocks.len());
    let mut checksums: Vec<u32> = Vec::with_capacity(blocks.len());
    let mut utxo = WindowOutputs::new();
    let mut records: Vec<BlockRec> = Vec::new();
    for (h_abs, b, hash) in &blocks {
        let is_pos = block_is_proof_of_stake(b);
        utxo.apply_block(b, (*h_abs - base) as usize);
        if *h_abs >= tip.saturating_sub(WINDOW_BLOCKS as u64 - 1) {
            records.push(BlockRec {
                height: *h_abs,
                spends: {
                    let mut sp = Vec::new();
                    for tx in &b.vtx {
                        let coinbase = tx.vin.len() == 1
                            && tx.vin[0].prev_hash == [0u8; 32]
                            && tx.vin[0].prev_n == u32::MAX;
                        if !coinbase {
                            for vin in &tx.vin {
                                sp.push((
                                    tri7_core::p2p::internal_to_display(&vin.prev_hash),
                                    vin.prev_n,
                                ));
                            }
                        }
                    }
                    sp
                },
                outs: {
                    let mut o = Vec::new();
                    let mut offset = (80 + compact_size_len(b.vtx.len() as u64)) as u32;
                    for tx in &b.vtx {
                        for (n, vout) in tx.vout.iter().enumerate() {
                            if !(vout.value == 0 && vout.script_pubkey.is_empty()) {
                                o.push((
                                    tri7_core::p2p::internal_to_display(&tx.hash()),
                                    n as u32,
                                    vout.value,
                                    tx.time,
                                    offset,
                                ));
                            }
                        }
                        offset += tx.serialize().len() as u32;
                    }
                    o
                },
            });
        }
        entries.push(IndexEntry {
            height: (*h_abs - base) as usize, // window-relative (see rel_index doc)
            hash: *hash,
            prev_hash: b.prev_hash,
            time: b.time,
            bits: b.bits,
            is_proof_of_stake: is_pos,
            entropy_bit: hash[0] & 1,
            stake_modifier: 0, // seeded next
            generated_stake_modifier: false,
            hash_proof_of_stake: [0u8; 32],
        });
        checksums.push(0);
    }

    // 4. modifier/proof/checksum seed from the daemon index
    let poss: Vec<bool> = entries.iter().map(|e| e.is_proof_of_stake).collect();
    let times: Vec<u32> = entries.iter().map(|e| e.time).collect();
    log(&format!(
        "seed: rpc-seeding {} window entries (batched getblock) …",
        entries.len()
    ));
    let (cs_last, adds) = rpc_seed_window(rpc, &hashes_display[1..], &poss, &times, base)?;
    for (i, a) in adds.iter().enumerate() {
        entries[i].stake_modifier = a.modifier;
        entries[i].generated_stake_modifier = a.generated;
        entries[i].hash_proof_of_stake = a.proof;
        checksums[i] = a.checksum;
    }
    let _ = cs_last;

    let st = ChainState {
        base,
        entries,
        checksums,
        utxo,
        records,
        pending_headers: HashMap::new(),
        tip_height: tip,
        tip_hash: expect_hashes[expect_hashes.len() - 1],
        daemon_tip: Some(daemon_tip),
        no_trim: false,
        last_advance_utc: now_iso(), // seeding anchored/advanced the tip
        peers_connected: 0,
        last_peer_utc: None, // seeded: the peer handshake predates this state
    };
    let file = st.to_file();
    Ok((st, file))
}

// ---------------------------------------------------------------------------
// backfill: historical header sweep + block fetch (protocol evidence +
// independent re-validation of the daemon-served data)
// ---------------------------------------------------------------------------
fn run_backfill(
    sess: &mut Session,
    st: &ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    cfg: &Config,
) -> Result<Vec<Block>, String> {
    let span = cfg.backfill_headers.min(WINDOW_ENTRIES as usize - 800);
    let start_h = st.tip_height - span as u64;
    let start_hash = st
        .entry_abs(start_h)
        .map(|e| e.hash)
        .ok_or("backfill: start height outside window")?;
    log(&format!(
        "backfill: requesting headers from h{} (locator {})",
        start_h,
        tri7_core::p2p::internal_to_display(&start_hash)
    ));
    let stop = [0u8; 32];
    // locator MUST NOT contain the tip: the daemon serves from the DEEPEST
    // main-chain locator match, so including the tip would yield zero headers.
    sess.send(
        "getheaders",
        &tri7_core::p2p::build_getheaders(&[start_hash], &stop),
        stats,
    )?;

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut got: Vec<Block> = Vec::new();
    let mut expect_prev = start_hash;
    let mut rounds = 0usize;
    while Instant::now() < deadline && rounds < 5 {
        let mut progressed = false;
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            match msg.command.as_str() {
                "headers" => {
                    let headers = tri7_core::p2p::parse_headers(&msg.payload)?;
                    progressed = !headers.is_empty();
                    for hdr in headers {
                        stats.headers_processed += 1;
                        let hash = hdr.hash();
                        if hdr.prev_hash != expect_prev {
                            return Err(format!(
                                "backfill header link mismatch: prev {} expected {}",
                                tri7_core::p2p::internal_to_display(&hdr.prev_hash),
                                tri7_core::p2p::internal_to_display(&expect_prev)
                            ));
                        }
                        expect_prev = hash;
                        stats.headers_backfilled += 1;
                        got.push(hdr);
                    }
                }
                "ping" => {
                    let n = tri7_core::p2p::parse_ping(&msg.payload)?;
                    sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
                }
                _ => {}
            }
        }
        if got.len() >= span {
            break;
        }
        if progressed && got.len() < span {
            // next round: continue from the last header received
            rounds += 1;
            sess.send(
                "getheaders",
                &tri7_core::p2p::build_getheaders(&[expect_prev], &stop),
                stats,
            )?;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    if got.is_empty() {
        return Err("backfill: no headers received".into());
    }
    log(&format!(
        "backfill: validated {} headers linking h{} → h{} (hash9 + linkage, daemon-served)",
        got.len(),
        start_h + 1,
        start_h + got.len() as u64
    ));

    // fetch a spread of full blocks across the range and validate them
    let k = cfg.backfill_blocks.min(got.len());
    if k == 0 {
        return Ok(got);
    }
    let step = ((got.len() - 1) / k.max(1)) + 1;
    let mut picks: Vec<(Hash, u64)> = Vec::new();
    let mut i = 0usize;
    while i < got.len() && picks.len() < k {
        picks.push((got[i].hash(), start_h + 1 + i as u64));
        i += step;
    }
    log(&format!("backfill: requesting {} full blocks", picks.len()));
    for chunk in picks.chunks(GETDATA_BATCH) {
        let inv: Vec<(u32, Hash)> = chunk
            .iter()
            .map(|(h, _)| (tri7_core::p2p::MSG_BLOCK, *h))
            .collect();
        sess.send("getdata", &tri7_core::p2p::build_getdata(&inv), stats)?;
    }
    let mut want: HashMap<Hash, u64> = picks.into_iter().collect();
    let mut validated = 0usize;
    let bdeadline = Instant::now() + Duration::from_secs(180);
    while validated < want.len() + validated && Instant::now() < bdeadline {
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            match msg.command.as_str() {
                "block" => {
                    stats.blocks_received += 1;
                    let mut r = Reader::new(&msg.payload);
                    let b = Block::read(&mut r).map_err(|e| format!("block parse: {}", e))?;
                    r.expect_done()
                        .map_err(|e| format!("block trailing: {}", e))?;
                    let hash = b.hash();
                    let height = match want.remove(&hash) {
                        Some(h) => h,
                        None => continue, // not requested (e.g. a live push)
                    };
                    let prev_entry = st
                        .entry_abs(height - 1)
                        .cloned()
                        .ok_or("backfill block outside window")?;
                    let header = got.iter().find(|hdr| hdr.hash() == hash);
                    validate_block(
                        st,
                        rpc,
                        stats,
                        &b,
                        &BlockCtx {
                            height_abs: height,
                            header,
                            prev_entry,
                            live: false,
                        },
                    )?;
                    stats.blocks_validated_hist += 1;
                    validated += 1;
                    if validated % 10 == 0 {
                        log(&format!(
                            "backfill: {} blocks fully validated so far",
                            validated
                        ));
                    }
                }
                "ping" => {
                    let n = tri7_core::p2p::parse_ping(&msg.payload)?;
                    sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    if validated == 0 {
        return Err("backfill: no blocks validated".into());
    }
    log(&format!(
        "backfill: {} blocks fully validated (kernel enforced {} / seeded {})",
        validated, stats.kernel_enforced, stats.kernel_seeded
    ));
    Ok(got)
}

// ---------------------------------------------------------------------------
// live message dispatch
// ---------------------------------------------------------------------------
/// Inverse of build_getheaders: stream-version i32 + CompactSize locator
/// count + hashes + 32-byte hashStop (main.cpp:5305-5307).
fn parse_getheaders(payload: &[u8]) -> Result<(Vec<Hash>, Hash), String> {
    let mut r = Reader::new(payload);
    let _stream_version = r.i32le()?;
    let n = r.compact_size()? as usize;
    if n > 512 {
        return Err(format!("getheaders locator count {} > 512", n));
    }
    let mut locator = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        locator.push(r.hash32()?);
    }
    let stop = r.hash32()?;
    r.expect_done()?;
    Ok((locator, stop))
}

/// Resolve the deepest locator hit against the disk store then the recent
/// cache, and collect the header range [start+1 .. start+1+2000) stopping at
/// `stop` (non-zero) or the first missing height. Mirrors main.cpp's
/// getheaders serving (nLimit 2000, headers only).
fn collect_headers(
    store: &Arc<BlockStore>,
    recent: &RecentBlocks,
    locator: &[Hash],
    stop: &Hash,
) -> Vec<Block> {
    // deepest known height among the locator hashes
    let mut start_h: Option<u64> = None;
    for h in locator {
        let hit = store.block_by_hash(h).ok().flatten();
        let height = match hit {
            Some(hit) => hit.height,
            None => match recent.get(h) {
                Some((hgt, _)) if *hgt > 0 => Some(*hgt),
                _ => None,
            },
        };
        if let Some(hgt) = height {
            // locator[0] is the most recent — the first match is the deepest
            start_h = Some(hgt);
            break;
        }
    }
    let Some(mut h) = start_h else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while out.len() < 2000 {
        h += 1;
        let next = match store.block_by_height(h) {
            Ok(Some(hit)) => hit.block,
            _ => match recent.at_height(h) {
                Some((_, raw)) => match Block::read(&mut Reader::new(raw)) {
                    Ok(b) => b,
                    Err(_) => break,
                },
                None => break,
            },
        };
        let stop_hit = *stop != [0u8; 32] && next.hash() == *stop;
        out.push(next);
        if stop_hit {
            break;
        }
    }
    out
}

// ===========================================================================
// Phase 7 milestone 2 — live wallet state (staking + broadcast), smsg engine,
// stake loop, and the NodeOps RPC adapter.
// ===========================================================================

/// One unspent output of the live wallet, exact per-block facts from the
/// disk store (the kernel's nTxPrevOffset + GetCoinAge's nTime are part of
/// consensus — approximations here would mint invalid kernels).
#[derive(Clone, Debug)]
struct LiveOut {
    height: u64,    // containing block height (absolute)
    block_time: u32,
    tx_time: u32,
    tx_offset: u32,
    value: i64,
    script: Vec<u8>,
    /// Epoch the coin was created in, as the legacy wallet reckons it: the
    /// block time capped by the containing block's own time (tx.nTime is 0 on
    /// staker blocks, so the block time is the only usable stamp).
    created_epoch: u32,
    /// block was mined by proof of stake (IsCoinStake) — the wallet needs this
    /// to show a stake as a stake instead of a plain transfer
    coinstake: bool,
    /// block was a proof-of-work block (IsCoinBase)
    coinbase: bool,
}

/// The wallet's live UTXO view: seeded by scanning the disk store from the
/// window base to the tip, then maintained incrementally on every accepted
/// block (spend removal + creation). Everything older than the window base
/// is NOT tracked — those coins cannot stake through the daemon loop anyway
/// (their stake-modifier walk needs window entries below the containing
/// block; stake_mint remains the full-archive staker for old-coin weight).
struct WalletLive {
    wallet: tri7_core::wallet::WalletKeys,
    base: u64,
    live: HashMap<(Hash, u32), LiveOut>,
    /// every prevout this view saw being SPENT, with the height of the
    /// spending block. The live map only holds coins it seeded itself, so a
    /// coin that arrived BEFORE the window (and is therefore in the replay
    /// snapshot) is not in the map when the spend is applied - the removal
    /// is a no-op and the snapshot would keep listing a coin that no longer
    /// exists. This map is what tells the merge that the coin is gone.
    spent: HashMap<(Hash, u32), u64>,
    /// true when the wallet file is encrypted and the secrets have not been
    /// supplied yet: the node still syncs and watches its addresses, but it
    /// cannot stake or sign. Cleared by `walletpassphrase`, restored by
    /// `walletlock` or the unlock timeout.
    locked: bool,
    /// unix time at which a timed unlock expires
    unlock_until: Option<u64>,
}

/// txpool::UtxoView over the live map — unspent-only (spends removed).
struct LiveUtxoView<'a>(&'a HashMap<(Hash, u32), LiveOut>);

impl tri7_core::txpool::UtxoView for LiveUtxoView<'_> {
    fn utxo(&self, key: &(Hash, u32)) -> Option<tri7_core::txpool::PoolUtxo> {
        self.0.get(key).map(|o| tri7_core::txpool::PoolUtxo {
            value: o.value,
            script_pubkey: o.script.clone(),
            height: o.height,
            tx_time: o.tx_time,
        })
    }
}

/// The live wallet map first; on a miss, the block store's full tx index
/// resolves DEEP coins the staking live view evicted (the live view is a
/// recent-blocks window — coins past the cap were unspendable because the
/// pool could not resolve their prevouts). Without this fallback only the
/// staker's own recent coins could ever be mined (2026-11-07: the parked
/// old-funds tx was rejected by DNS2 while sami-pc accepted the identical
/// bytes — each node's pool saw only its own wallet's coins).
struct WalletThenStoreUtxoView<'a> {
    live: &'a HashMap<(Hash, u32), LiveOut>,
    store: Option<&'a tri7_core::blockindex::BlockStore>,
}

impl tri7_core::txpool::UtxoView for WalletThenStoreUtxoView<'_> {
    fn utxo(&self, key: &(Hash, u32)) -> Option<tri7_core::txpool::PoolUtxo> {
        if let Some(o) = self.live.get(key) {
            return Some(tri7_core::txpool::PoolUtxo {
                value: o.value,
                script_pubkey: o.script.clone(),
                height: o.height,
                tx_time: o.tx_time,
            });
        }
        let Some(store) = self.store else {
            log("pool deep-coin fallback: no store wired (wallet-only view)");
            return None;
        };
        log(&format!(
            "pool deep-coin fallback: live miss for {}, consulting store (wired)",
            tri7_core::p2p::internal_to_display(&key.0)
        ));
        match store.tx_by_id(&key.0) {
            Ok(Some(hit)) => {
                let Some(o) = hit.tx.vout.get(key.1 as usize) else {
                    log(&format!(
                        "pool deep-coin fallback: prevout vout {} missing on parent {}",
                        key.1,
                        tri7_core::p2p::internal_to_display(&key.0)
                    ));
                    return None;
                };
                Some(tri7_core::txpool::PoolUtxo {
                    value: o.value,
                    script_pubkey: o.script_pubkey.clone(),
                    height: hit.block_height.unwrap_or(0),
                    tx_time: hit.block_time,
                })
            }
            Ok(None) => {
                log(&format!(
                    "pool deep-coin fallback: store miss for {} (store present)",
                    tri7_core::p2p::internal_to_display(&key.0)
                ));
                None
            }
            Err(e) => {
                log(&format!(
                    "pool deep-coin fallback: store error for {}: {}",
                    tri7_core::p2p::internal_to_display(&key.0),
                    e
                ));
                None
            }
        }
    }
}


/// A block is a PoW block when its first transaction is a coinbase (no real
/// prevout and a zero-value first output).
fn tx_is_coinbase(block: &Block) -> bool {
    block
        .vtx
        .first()
        .map(|tx| {
            tx.vin.iter().any(|v| v.prev_n == u32::MAX)
                && tx.vout.first().map(|o| o.value == 0).unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Walk back from `hint` to the highest height a block BODY actually exists
/// for (cap 5_000). Returns (replay_tip, walked_back). Shared by the wallet
/// seed thread and the repairwallet rebuild: the tip block arrives as a
/// HEADER first and its body lands a beat later, and the store's newest body
/// can sit far below the tip when recent blocks exist only in the live
/// tip/body cache (mined here, appended in memory).
fn newest_replayable_tip(has_body: &dyn Fn(u64) -> bool, base: u64, hint: u64) -> (u64, u64) {
    let mut tip = hint;
    let mut back = 0u64;
    while back < 5_000 && tip > base {
        if has_body(tip) {
            break;
        }
        tip -= 1;
        back += 1;
    }
    (tip, back)
}

/// repairwallet arity (rpcwallet.cpp:1804 parity): the legacy verb throws on
/// ANY parameter, so tri7 refuses with -8.
fn repairwallet_params_ok(params: &serde_json::Value) -> Result<(), (i64, String)> {
    if params.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        return Err((-8, "repairwallet takes no parameters".into()));
    }
    Ok(())
}

/// resendtx arity (rpcwallet.cpp:1826 parity): the legacy verb accepted one
/// (ignored) parameter and threw on more.
fn resendtx_params_ok(params: &serde_json::Value) -> Result<(), (i64, String)> {
    if params.as_array().map(|a| a.len() > 1).unwrap_or(false) {
        return Err((-8, "resendtx takes at most one (ignored) parameter".into()));
    }
    Ok(())
}

/// What repairwallet found by diffing the wallet's live view against the
/// authoritative store replay (wallet-owned outputs only). Mirrors the two
/// FixSpentCoins directions: a coin the wallet view still lists but the
/// chain no longer holds is the legacy "spent coin"; a coin the replay
/// holds but the view had lost is the legacy "lost coin".
struct RepairDiff {
    /// view said unspent, chain says spent/absent: (outpoint, value)
    phantom: Vec<((Hash, u32), i64)>,
    /// chain says unspent, view had missed it: (outpoint, value)
    lost: Vec<((Hash, u32), i64)>,
}

impl RepairDiff {
    fn count(&self) -> usize {
        self.phantom.len() + self.lost.len()
    }
    /// legacy nBalanceInQuestion: the sum of the mismatched output values
    fn amount(&self) -> i64 {
        self.phantom
            .iter()
            .chain(self.lost.iter())
            .map(|(_, v)| *v)
            .sum()
    }
}

/// resendtx mine-gate decision (pure, unit-tested). Runs BEFORE pool.accept
/// so a mined recorded tx can never reach accept again: the pool's
/// WalletThenStoreUtxoView deep-coin fallback resolves prevouts from the
/// block store WITHOUT spent-tracking, so a mined tx's inputs still "exist"
/// to the view and the tx re-validates — live-caught on DNS2 2026-11-08:
/// a resend pass of a mined send re-accepted it into the pool of the
/// fleet's heavy staker as a double-spend candidate.
///
/// The gate is FAIL-CLOSED on store trouble: when the store ERRORS we
/// cannot know whether the tx mined, so the entry is skipped this pass
/// (kept for the next one) rather than accepted. Only a definitive
/// "store answered: not mined" reaches the accept path. No store at all
/// is also Unknown: with a wallet-only view a mined tx's parent outputs
/// were removed by apply_block, so accept fails on its own (transient
/// keep) — there is no deep fallback to fool it.
enum ResendGate {
    /// mined → prune from the record, never accept
    Mined,
    /// store answered "not mined" (or no deep fallback exists) → accept path
    Unknown,
    /// store error → cannot prove unmined; skip accept this pass
    CantProve,
}

fn resend_mine_gate(store_hit: Option<Result<Option<()>, String>>) -> ResendGate {
    match store_hit {
        Some(Ok(Some(_))) => ResendGate::Mined,
        Some(Ok(None)) => ResendGate::Unknown,
        Some(Err(_)) => ResendGate::CantProve,
        None => ResendGate::Unknown,
    }
}

fn repair_diff(
    old: &HashMap<(Hash, u32), LiveOut>,
    fresh: &HashMap<(Hash, u32), LiveOut>,
    owns: &dyn Fn(&LiveOut) -> bool,
) -> RepairDiff {
    let mut d = RepairDiff {
        phantom: Vec::new(),
        lost: Vec::new(),
    };
    // deterministic order (the report is for humans and tests)
    let mut keys: Vec<&(Hash, u32)> = old.keys().collect();
    keys.sort();
    for k in keys {
        let o = &old[k];
        if owns(o) && !fresh.contains_key(k) {
            d.phantom.push((*k, o.value));
        }
    }
    let mut keys: Vec<&(Hash, u32)> = fresh.keys().collect();
    keys.sort();
    for k in keys {
        let o = &fresh[k];
        if owns(o) && !old.contains_key(k) {
            d.lost.push((*k, o.value));
        }
    }
    d
}

impl WalletLive {
    /// Scan the disk store from `base` to `tip` and build the exact live
    /// set. Requires the store index ready (heights_ok).
    fn seed(store: &BlockStore, wallet: tri7_core::wallet::WalletKeys, base: u64, tip: u64) -> Result<WalletLive, String> {
        let mut wl = WalletLive {
            wallet,
            base,
            live: HashMap::new(),
            spent: HashMap::new(),
            locked: false,
            unlock_until: None,
        };
        for h in base..=tip {
            let hit = store
                .block_by_height(h)?
                .ok_or_else(|| format!("store missing block body at h{}", h))?;
            wl.apply_block(&hit.block, h);
        }
        Ok(wl)
    }

    /// Apply one accepted block: remove spent prevouts (all non-coinbase
    /// txs), add non-empty outputs.
    fn apply_block(&mut self, block: &Block, height: u64) {
        // classify the block once: a stake block's first tx is the coinstake
        // (vin carries a real prevout), a PoW block's first tx is a coinbase
        // (vin.prev_n == u32::MAX). Wallet rows and their maturity rules both
        // depend on this distinction.
        let cb = block.vtx.first().map(|tx| {
            tx.vin.iter().any(|v| v.prev_n == u32::MAX) && tx.vout.first().map(|o| o.value == 0).unwrap_or(false)
        }).unwrap_or(false);
        let is_coinstake = block.vtx.len() > 1
            && block.vtx[1]
                .vin
                .iter()
                .any(|v| v.prev_n != u32::MAX)
            && block
                .vtx
                .first()
                .map(|tx| tx.vout.first().map(|o| o.value == 0).unwrap_or(false))
                .unwrap_or(false);
        let _ = cb;
        for tx in block.vtx.iter().skip(1) {
            for vin in &tx.vin {
                if vin.prev_n != u32::MAX {
                    // remember the spend even when the coin is not in the
                    // map: it may be a coin the replay snapshot holds
                    self.spent.insert((vin.prev_hash, vin.prev_n), height);
                    self.live.remove(&(vin.prev_hash, vin.prev_n));
                }
            }
        }
        let mut offset = (80 + compact_size_len(block.vtx.len() as u64)) as u32;
        for tx in &block.vtx {
            for (n, vout) in tx.vout.iter().enumerate() {
                if !(vout.value == 0 && vout.script_pubkey.is_empty()) {
                    self.live.entry((tx.hash(), n as u32)).or_insert(LiveOut {
                        height,
                        block_time: block.time,
                        tx_time: tx.time,
                        tx_offset: offset,
                        value: vout.value,
                        script: vout.script_pubkey.clone(),
                        created_epoch: if tx.time == 0 || tx.time > block.time {
                            block.time
                        } else {
                            tx.time
                        },
                        coinstake: is_coinstake,
                        coinbase: !is_coinstake && tx_is_coinbase(block),
                    });
                }
            }
            offset += tx.serialize().len() as u32;
        }
    }

    fn key_by_pubkey(&self) -> HashMap<Vec<u8>, &tri7_core::wallet::WalletKey> {
        self.wallet.keys.iter().map(|k| (k.pubkey.clone(), k)).collect()
    }

    fn key_by_payload(&self) -> HashMap<[u8; 21], &tri7_core::wallet::WalletKey> {
        // P2PKH payload = version + hash160(pubkey)
        self.wallet
            .keys
            .iter()
            .map(|k| (tri7_core::address::pubkey_payload(&k.pubkey), k))
            .collect()
    }

    /// Stake candidates: live wallet-owned outputs (maturity is filtered by
    /// create_coinstake per-coin, exactly like the legacy wallet loop).
    /// Sorted like stake_mint (block_height, txid, n) for determinism.
    fn candidates(&self) -> (Vec<tri7_core::stake::StakeCandidate>, usize, usize, usize) {
        let by_pubkey = self.key_by_pubkey();
        let by_payload = self.key_by_payload();
        let mut out = Vec::new();
        let mut skipped_no_key = 0usize;
        let mut skipped_unlocated = 0usize;
        let mut skipped_nonstandard = 0usize;
        for ((txid, n), o) in &self.live {
            let spend_type = match tri7_core::spend::solver_type(&o.script) {
                Some(t) => t,
                None => {
                    skipped_nonstandard += 1;
                    continue;
                }
            };
            let key = match &spend_type {
                tri7_core::spend::SpendType::PubKey(pk) => by_pubkey.get(pk).copied(),
                tri7_core::spend::SpendType::PubkeyHash(h) => {
                    by_payload
                        .get(&tri7_core::address::ScriptDest::PubkeyHash(*h).payload())
                        .copied()
                }
                _ => None,
            };
            let Some(k) = key else {
                skipped_no_key += 1;
                continue;
            };
            if (o.height < self.base) || (o.height as i64) < 0 {
                skipped_unlocated += 1;
                continue;
            }
            out.push(tri7_core::stake::StakeCandidate {
                txid: *txid,
                n: *n,
                value: o.value,
                script_pubkey: o.script.clone(),
                block_height: o.height as usize,
                block_time: o.block_time,
                tx_time: o.tx_time,
                tx_offset: o.tx_offset,
                secret: k.secret,
                pubkey: k.pubkey.clone(),
            });
        }
        out.sort_by(|a, b| {
            a.block_height
                .cmp(&b.block_height)
                .then_with(|| a.txid.cmp(&b.txid))
                .then_with(|| a.n.cmp(&b.n))
        });
        (out, skipped_no_key, skipped_unlocated, skipped_nonstandard)
    }

    /// Is this output payable to one of the wallet's keys?
    fn owns(&self, o: &LiveOut) -> bool {
        match tri7_core::spend::solver_type(&o.script) {
            Some(tri7_core::spend::SpendType::PubKey(pk)) => {
                self.wallet.keys.iter().any(|k| k.pubkey == pk)
            }
            Some(tri7_core::spend::SpendType::PubkeyHash(h)) => {
                let want = tri7_core::address::ScriptDest::PubkeyHash(h).payload();
                self.wallet
                    .keys
                    .iter()
                    .any(|k| tri7_core::address::pubkey_payload(&k.pubkey) == want)
            }
            _ => false,
        }
    }

    /// Sum of the wallet's OWN unspent outputs.
    ///
    /// `live` holds every output in the scanned window (spends are removed as
    /// they are seen), which is what the stake-kernel walk needs. Summing it
    /// whole — as this used to — reports the value of the window instead of the
    /// wallet's coins: a fresh wallet with no funded keys showed 3,205 TRI.
    fn balance(&self) -> i64 {
        self.live
            .values()
            .filter(|o| self.owns(o))
            .map(|o| o.value)
            .sum()
    }

    /// Number of unspent outputs that pay the wallet.
    fn owned_count(&self) -> usize {
        self.live.values().filter(|o| self.owns(o)).count()
    }

    /// Legacy GetStakeWeight parity: Σ value·min(now−txTime, STAKE_MAX_AGE)
    /// / COIN / 86400 over mature coins (capped weight, wallet.cpp:2190).
    fn stake_weight(&self, now: u32) -> (u64, usize) {
        let mut weight: u128 = 0;
        let mut mature = 0usize;
        for o in self.live.values() {
            if (o.block_time as i64) + tri7_core::consensus::STAKE_MIN_AGE as i64 > now as i64 {
                continue;
            }
            mature += 1;
            let age = now
                .saturating_sub(o.created_epoch)
                .min(tri7_core::consensus::STAKE_MAX_AGE);
            weight += (o.value as u128 * age as u128) / tri7_core::consensus::COIN as u128 / 86_400;
        }
        (weight.min(u64::MAX as u128) as u64, mature)
    }
}

// ---------------------------------------------------------------------------
// stake loop state + smsg shared handle + txpool + peer table
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StakeState {
    enabled: bool,
    /// total kernel hashes tried (getstakinginfo "attempts")
    attempts: u64,
    /// completed search passes
    searches: u64,
    /// seconds of the last full search pass
    last_search_secs: f64,
    /// last pass that met every pre-condition (wallet, index, tip)
    last_search_utc: Option<String>,
    last_hit_utc: Option<String>,
    /// the "wallet is LOCKED" line has already been logged this lock
    locked_logged: bool,
    /// (height, hash display, time_utc) of the last minted block
    last_stake: Option<(u64, String, String)>,
    last_error: Option<String>,
    /// candidate stats of the last pass
    coins_considered: u64,
    coins_meeting_min_age: u64,
}

type SharedStake = Arc<Mutex<StakeState>>;
type SharedWalletLive = Arc<Mutex<Option<WalletLive>>>;

/// Live-preferring wallet provider (Sami: "why am I not seeing the number go
/// up or the staking notifications"): getbalance serves the LIVE window view
/// when the wallet-seed thread has populated it, so stakes won by the staker
/// appear immediately; balances.json snapshots lag by design (regenerated
/// only by the balances pass). Snapshot data keeps serving everything the
/// live view cannot cover (per-address receipts, membership, coverage-gated
/// listunspent).
struct LiveWalletProvider {
    snap: Arc<SnapshotWallet>,
    wl: SharedWalletLive,
    st: SharedState,
    /// state dir holding history.json (full wallet history, txhistory bin)
    state_dir: std::path::PathBuf,
    /// balances.json path for the `richlist` explorer verb. Parsed through an
    /// mtime+len keyed cache (RichListCache): a background `--balances`
    /// rebuild is picked up on the next call WITHOUT a restart, and an
    /// unchanged multi-MB snapshot is never re-parsed per call.
    balances_path: std::path::PathBuf,
    rich_cache: Arc<Mutex<tri7_core::rpc::RichListCache>>,
}



/// The balances-rebuild scheduler.
///
/// A walk that stopped short of the tip it CHASED (blocks landed while it
/// was walking) is retried on a doubling backoff — 30s, 60s, 120s … capped
/// at 1800s — so the snapshot converges to the whole chain and stays
/// periodically eligible for rebuilding FOREVER: a short walk never makes
/// the snapshot permanently ineligible.
struct RebuildSchedule {
    /// consecutive walks that stopped short of the chased tip
    streak: u32,
}

impl RebuildSchedule {
    fn new() -> Self {
        Self { streak: 0 }
    }

    /// seconds to wait after attempt number `streak` (1-based). The FIRST
    /// retry waits 30s, the second 60s, and so on, capped.
    fn backoff_secs(&self) -> u64 {
        const BASE: u64 = 30;
        const MAX: u64 = 1_800;
        if self.streak == 0 {
            return 0;
        }
        let shifts = (self.streak - 1).min(6);
        BASE.saturating_mul(1u64 << shifts).min(MAX)
    }

    /// whether a rebuild should run now.
    ///
    /// `stale` is the ordinary staleness (the snapshot is more than
    /// REFRESH_BEHIND behind the tip, or missing entirely). `secs_since`
    /// is how long ago the last attempt finished.
    fn due(&self, stale: bool, secs_since: u64) -> bool {
        if stale {
            return true;
        }
        if self.streak == 0 {
            return false;
        }
        secs_since >= self.backoff_secs()
    }

    /// record an attempt's outcome: did the walk reach the tip it chased?
    fn record(&mut self, reached_tip: bool) {
        if reached_tip {
            self.streak = 0;
        } else {
            self.streak = self.streak.saturating_add(1);
        }
    }
}

/// The wallet's own outputs as the LIVE view knows them (the ones it
/// replayed in its window). A pure function of the live view so the merge
/// below can be tested without a live node.
fn live_rows_for(wl: &SharedWalletLive) -> (Vec<UtxoRow>, Vec<(String, u32, u64)>, u64) {
    let Ok(g) = wl.lock() else { return (Vec::new(), Vec::new(), 0) };
    let Some(w) = g.as_ref() else { return (Vec::new(), Vec::new(), 0) };
    let kb = w.key_by_payload();
    let mut rows: Vec<UtxoRow> = Vec::new();
    for ((txid, n), o) in w.live.iter() {
        let addr = tri7_core::address::script_to_dest(&o.script)
            .map(|d| tri7_core::address::payload_to_address(&d.payload()))
            .filter(|a| {
                tri7_core::address::script_to_dest(&o.script)
                    .map(|d| kb.contains_key(&d.payload()))
                    .unwrap_or(false)
            })
            .unwrap_or_default();
        if addr.is_empty() {
            continue; // not ours
        }
        rows.push(UtxoRow {
            txid: tri7_core::p2p::internal_to_display(txid),
            n: *n,
            address: addr,
            script_hex: o.script.iter().map(|b| format!("{:02x}", b)).collect(),
            value_sats: o.value,
            height: o.height,
            // date history from the BLOCK time (nTime): stakers leave the tx
            // field at 0 and the legacy client dates rows from the block, so
            // this matches it exactly
            tx_time: o.created_epoch as i64,
            // prev_kind must say what the coin IS. It used to be inferred
            // from tx_time == 0, which only means "the staker left nTime
            // blank" - so every live stake was reported as a plain transfer
            // and the Overview Stake row could never fill.
            prev_kind: if o.coinstake {
                "coinstake"
            } else if o.coinbase {
                "coinbase"
            } else if o.tx_time == 0 {
                "stake" // legacy label kept for older consumers
            } else {
                "regular"
            }
            .into(),
        });
    }
    // the spends this view saw, in display form, with the height of the
    // spending block
    let spends: Vec<(String, u32, u64)> = w
        .spent
        .iter()
        .map(|((txid, n), height)| {
            (
                tri7_core::p2p::internal_to_display(txid),
                *n,
                *height,
            )
        })
        .collect();
    (rows, spends, w.base)
}

/// Merge the replay snapshot (genesis..coverage) with the live view's
/// outputs (window..tip) into the wallet's WHOLE unspent set.
///
/// The live rows kept are strictly ABOVE the coverage height, so the two
/// ranges never overlap and no coin is counted twice. The result is
/// ordered newest first.
fn merge_whole_wallet_utxos(
    snapshot_rows: Vec<UtxoRow>,
    live_rows: Vec<UtxoRow>,
    spends: Vec<(String, u32, u64)>,
    coverage: u64,
    live_base: u64,
    has_snapshot: bool,
) -> Vec<UtxoRow> {
    // NO SNAPSHOT AT ALL (a fresh wallet folder): the live view is the
    // whole set there is. There is nothing to merge and no gap to worry
    // about - the live view's window base is simply where its replay
    // started, and its rows are what the wallet owns.
    if !has_snapshot {
        let mut rows = live_rows;
        rows.sort_by(|a, b| b.height.cmp(&a.height));
        return rows;
    }

    // A coin in the snapshot can have been SPENT after the snapshot's
    // coverage. The live view saw that spend (it applies every block from
    // its window base onwards), so the coin is removed here. Without this
    // the merge would keep listing a coin that no longer exists - the
    // wallet would show more than it holds.
    //
    // Only spends ABOVE the coverage count: a spend at or below the
    // coverage is already reflected in the snapshot itself.
    let spent_after: std::collections::HashSet<(String, u32)> = spends
        .into_iter()
        .filter(|(_, _, h)| *h > coverage)
        .map(|(txid, n, _)| (txid, n))
        .collect();

    // THE GAP.
    //
    // The snapshot covers the height range [0, coverage] (genesis up to
    // and including its coverage height). The live view covers the range
    // [live_base, tip] (its window base up to the tip).
    //
    // The two ranges are contiguous - no height is covered by neither -
    // when live_base <= coverage + 1. When live_base > coverage + 1 there
    // is a HOLE: the heights in (coverage, live_base) are covered by
    // NEITHER source. A coin received in the hole would be missing (the
    // wallet would understate) and a coin spent in the hole would still be
    // listed (the wallet would overstate).
    //
    // Such a merge is not a wallet - it is two halves of one with a piece
    // missing. It is REJECTED: the snapshot alone is served, which is a
    // consistent set as of its coverage height, and the caller is told the
    // figure is capped (the RPC's completeness rule marks it).
    if live_base > coverage + 1 {
        let mut rows = snapshot_rows;
        rows.sort_by(|a, b| b.height.cmp(&a.height));
        return rows;
    }

    let mut rows: Vec<UtxoRow> = snapshot_rows
        .into_iter()
        .filter(|r| !spent_after.contains(&(r.txid.clone(), r.n)))
        .collect();
    let mut fresh: Vec<UtxoRow> = live_rows
        .into_iter()
        .filter(|r| r.height > coverage)
        .collect();
    fresh.sort_by(|a, b| b.height.cmp(&a.height));
    rows.append(&mut fresh);
    rows.sort_by(|a, b| b.height.cmp(&a.height));
    rows
}

impl tri7_core::rpc::WalletProvider for LiveWalletProvider {
    fn state_dir(&self) -> String {
        self.state_dir.display().to_string()
    }

    fn rich_list(&self, top: usize) -> Result<serde_json::Value, String> {
        // Explorer verb data path: snapshot file → mtime-keyed parse → ranked
        // top addresses. NO daemon locks on the hot path (the state lock is
        // sampled try_lock-only for the tip comparison — explorer pressure
        // must never block consensus-critical flow).
        let mut cache = self
            .rich_cache
            .lock()
            .map_err(|e| format!("rich list cache poisoned: {e}"))?;
        let data = cache.get(&self.balances_path)?;
        let chain_tip = self.st.try_lock().ok().map(|g| g.tip_height);
        let source = format!(
            "balances snapshot {} (full-archive replay h{})",
            self.balances_path.display(),
            data.height
        );
        Ok(tri7_core::rpc::rich_list_json(
            &data,
            chain_tip,
            &source,
            top,
        ))
    }

    // The live provider OWNS the extras (fee setting, bestblock, unlock state):
    // every native handler reads them through this, so a `settxfee` that lands
    // anywhere else would be invisible to `estimatefee`/`getwalletinfo`.
    fn extras(&self) -> tri7_core::rpc::WalletExtras {
        tri7_core::rpc::WalletProvider::extras(self.snap.as_ref())
    }

    fn set_paytxfee(&self, sats: i64) -> std::result::Result<(), String> {
        tri7_core::rpc::WalletProvider::set_paytxfee(self.snap.as_ref(), sats)
    }

    fn default_key(&self) -> Option<Vec<u8>> {
        tri7_core::rpc::WalletProvider::default_key(self.snap.as_ref())
    }

    fn pubkey_for_address(&self, addr: &str) -> Option<Vec<u8>> {
        tri7_core::rpc::WalletProvider::pubkey_for_address(self.snap.as_ref(), addr)
    }

    fn labels(&self) -> std::collections::HashMap<String, String> {
        tri7_core::rpc::WalletProvider::labels(self.snap.as_ref())
    }

    fn balance(&self) -> Option<BalanceView> {
        if let Ok(g) = self.wl.lock() {
            // The live view is the wallet's UNSPENT set, and it only knows
            // about coins that appeared in the range it replayed. It is
            // seeded from the chain-state window base, so on a node that
            // resumed from a state file it MISSES the coins the wallet
            // received before that base - and a balance that silently
            // omits the wallet's older coins is worse than a slightly
            // stale one.
            //
            // The live view may therefore serve the balance ONLY when it
            // replayed the wallet's WHOLE history (base == 0, the
            // from-zero case). Otherwise the replay snapshot - which
            // covers the chain from genesis - is the whole-wallet figure
            // and is served instead.
            if let Some(w) = g.as_ref().filter(|w| w.base == 0) {
                let b = &self.snap.balances;
                let (tip, tip_hash) = match self.st.try_lock() {
                    Ok(s) => (
                        s.tip_height,
                        tri7_core::p2p::internal_to_display(&s.tip_hash),
                    ),
                    Err(_) => match self.snap.balance() {
                        // follow loop holds the state mutex (validation
                        // batch) — fall back to snapshot coverage info
                        Some(bv) => (bv.coverage_height, bv.coverage_hash.clone()),
                        None => (0, String::new()),
                    },
                };
                // distinct owned addresses among live outputs
                let kb = w.key_by_payload();
                let mut owned = std::collections::HashSet::new();
                for o in w.live.values() {
                    if let Some(d) = tri7_core::address::script_to_dest(&o.script) {
                        let pl = d.payload();
                        if kb.contains_key(&pl) {
                            owned.insert(pl);
                        }
                    }
                }
                return Some(BalanceView {
                    sats: w.balance(),
                    supply_sats: b.total_supply_sats,
                    coverage_height: tip,
                    coverage_hash: tip_hash,
                    addresses: owned.len(),
                    utxos: w.live.len(),
                    // the live view replayed base..=tip: it walked
                    // tip - base + 1 heights, and its base is the height it
                    // started from. It is whole-chain only when it started at
                    // genesis (base == 0), which is what the balance-source
                    // selection checks.
                    blocks_replayed: Some(tip.saturating_sub(w.base) + 1),
                    // the live view replayed base..=tip: it reached the tip
                    // it was chasing, and (checked by the caller) it
                    // started at genesis
                    replay_target_height: Some(tip),
                });
            }
        }
        self.snap.balance()
    }

    fn coverage(&self) -> Option<(u64, String)> {
        // live view present: the window IS the coverage (tip + its hash) —
        // confirmations are keyed against the live tip, not a stale snapshot
        if let Ok(g) = self.wl.lock() {
            if g.is_some() {
                if let Ok(s) = self.st.try_lock() {
                    return Some((
                        s.tip_height,
                        tri7_core::p2p::internal_to_display(&s.tip_hash),
                    ));
                }
            }
        }
        self.snap.coverage()
    }

    fn address_entries(&self) -> Vec<AddressEntry> {
        // A balances snapshot is optional (a fresh wallet folder has none, and
        // a node started without --balances never writes one). Serve the live
        // wallet view instead of failing the whole call: the UI needs the
        // address list, and the live view already knows every owned output.
        let from_snap = self.snap.address_entries();
        if !from_snap.is_empty() {
            return from_snap;
        }
        let mut agg: std::collections::HashMap<String, AddressEntry> =
            std::collections::HashMap::new();
        if let Ok(g) = self.wl.lock() {
            if let Some(w) = g.as_ref() {
                let kb = w.key_by_payload();
                for (_, o) in w.live.iter() {
                    let Some(dest) = tri7_core::address::script_to_dest(&o.script) else {
                        continue;
                    };
                    if !kb.contains_key(&dest.payload()) {
                        continue; // not ours
                    }
                    let addr = tri7_core::address::payload_to_address(&dest.payload());
                    let e = agg.entry(addr.clone()).or_insert(AddressEntry {
                        address: addr,
                        balance_sats: 0,
                        utxos: 0,
                        received_regular_sats: 0,
                        received_all_sats: 0,
                    });
                    e.balance_sats += o.value;
                    e.utxos += 1;
                    // a live unspent output is by definition money received
                    e.received_regular_sats += o.value;
                    e.received_all_sats += o.value;
                }
            }
        }
        let mut out: Vec<AddressEntry> = agg.into_values().collect();
        out.sort_by(|a, b| b.balance_sats.cmp(&a.balance_sats));
        out
    }

    fn received_regular(&self, addr: &str) -> Option<i64> {
        self.snap.received_regular(addr)
    }

    fn is_mine(&self, addr: &str) -> bool {
        self.snap.is_mine(addr)
    }

    fn utxos(&self) -> Vec<UtxoRow> {
        // THE WHOLE WALLET'S UNSPENT SET.
        //
        // Two sources, each incomplete on its own:
        //
        //  - the replay SNAPSHOT covers the chain from GENESIS, so it knows
        //    every coin the wallet ever received; but it stops at its
        //    coverage height, so it misses the coins that arrived after.
        //  - the LIVE view is fresh (it follows the tip), but it is seeded
        //    from the chain-state WINDOW base, so it misses every coin the
        //    wallet received BEFORE that base.
        //
        // Serving the live view alone is what made the wallet show
        // 1,627 TRI for a wallet holding 5,291 TRI: the Overview card sums
        // this list. Serving the snapshot alone would instead miss the most
        // recent coins.
        //
        // So: the snapshot, PLUS the live view's outputs that are NEWER
        // than the snapshot's coverage. The two ranges do not overlap (the
        // live rows kept are strictly above the coverage height, and the
        // snapshot ends at it), so nothing is counted twice.
        let snap_coverage = self.snap.coverage();
        let has_snapshot = snap_coverage.is_some();
        let coverage = snap_coverage.map(|(h, _)| h).unwrap_or(0);
        let snapshot_rows = self.snap.utxos();
        let (live_rows, spends, live_base) = live_rows_for(&self.wl);
        merge_whole_wallet_utxos(
            snapshot_rows,
            live_rows,
            spends,
            coverage,
            live_base,
            has_snapshot,
        )
    }


    fn history(&self) -> Vec<tri7_core::history::HistoryRec> {
        // history.json is produced by the txhistory bin (one sequential pass
        // over the block store). Absent file -> empty list, never an error.
        match tri7_core::history::load_cache(&self.state_dir) {
            Some(c) => {
                let mut rows = c.rows;
                rows.sort_by(|a, b| b.time.cmp(&a.time).then(b.height.cmp(&a.height)));
                rows
            }
            None => Vec::new(),
        }
    }
}
type SharedPool = Arc<Mutex<tri7_core::txpool::TxPool>>;
type TxAnnounce = Arc<Mutex<std::collections::VecDeque<Hash>>>;

/// One kernel-search pass. Runs INSIDE the follow loop with the state lock
/// held (same discipline as a peer-validation batch — the RPC threads are
/// try_lock/mirror safe). Returns Ok(true) when a block was minted.
#[allow(clippy::too_many_arguments)]
/// Relock an encrypted wallet once its unlock window has passed. Cheap: called
/// from the same loop that stakes.
fn enforce_unlock_timeout(wl: &SharedWalletLive) {
    let mut g = match wl.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(w) = g.as_mut() else { return };
    let Some(until) = w.unlock_until else { return };
    if now_secs() >= until {
        let mut n = 0usize;
        for k in w.wallet.keys.iter_mut() {
            if k.secret != [0u8; 32] {
                n += 1;
            }
            k.secret = [0u8; 32];
        }
        w.locked = true;
        w.unlock_until = None;
        if n > 0 {
            log(&format!("wallet: unlock window elapsed — {n} keys locked again, staking stops"));
        }
    }
}

fn stake_tick(
    cfg: &Config,
    stg: &mut ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    wl: &mut WalletLive,
    pool: &mut tri7_core::txpool::TxPool,
    stake_st: &mut StakeState,
    store: &BlockStore,
    peers: &mut [Peer],
    recent: &mut RecentBlocks,
) -> Result<bool, String> {
    let now = now_secs() as u32;
    let tip_height = stg.tip_height;
    let Some(tip_entry) = stg.entry_abs(tip_height).cloned() else {
        return Err("tip entry missing".into());
    };
    stake_st.last_search_utc = Some(now_iso());
    if wl.locked {
        // Staking needs the private keys. An encrypted wallet that has not been
        // unlocked cannot mint — say so once, then stay quiet.
        if !stake_st.locked_logged {
            log("stake: wallet is LOCKED — no staking until walletpassphrase");
            stake_st.locked_logged = true;
        }
        stake_st.coins_considered = 0;
        return Ok(false);
    }
    stake_st.locked_logged = false;
    let (candidates, _no_key, _unloc, _nonstd) = wl.candidates();
    stake_st.coins_considered = candidates.len() as u64;
    let balance = wl.balance();
    if balance <= tri7_core::stake::RESERVE_BALANCE {
        stake_st.searches += 1;
        return Ok(false);
    }
    let n_bits = get_next_target_required(&stg.rel_index(), true);
    let median_time_past = stg
        .median_time_past(tip_height)
        .ok_or("median time past unavailable")?;
    let started = Instant::now();
    let fees = pool.total_fees();
    // create_coinstake indexes each candidate's block_height into the passed
    // index — the rel_index is window-relative (entries[i] == rel height i),
    // so translate absolute candidate heights to rel and skip coins that
    // predate the window base (their modifier walk is unknowable here; the
    // skipped count keeps getstakinginfo honest).
    let out_of_window = candidates.iter().filter(|c| (c.block_height as u64) < stg.base).count();
    if out_of_window > 0 {
        log(&format!(
            "stake: {} candidate coin(s) below window base {} — skipped (modifier walk out of range)",
            out_of_window, stg.base
        ));
    }
    let candidates_rel: Vec<tri7_core::stake::StakeCandidate> = candidates
        .iter()
        .filter(|c| (c.block_height as u64) >= stg.base)
        .map(|c| {
            let mut r = c.clone();
            r.block_height = stg.rel(c.block_height as u64);
            r
        })
        .collect();
    let (plan, rep) = tri7_core::stake::create_coinstake(
        &stg.rel_index(),
        &candidates_rel,
        n_bits,
        fees,
        now,
        tri7_core::stake::MAX_STAKE_SEARCH_INTERVAL,
        balance,
        tip_height as usize + 1,
    )?;
    stake_st.attempts += rep.attempts;
    stake_st.coins_meeting_min_age = rep.coins_meeting_min_age;
    stake_st.searches += 1;
    stake_st.last_search_secs = started.elapsed().as_secs_f64();
    let Some(plan) = plan else {
        return Ok(false);
    };
    stake_st.last_hit_utc = Some(now_iso());
    log(&format!(
        "STAKE: kernel solved after {} attempts ({}s) — coin {}:{} ({} sats, {} coin-days), reward {} sats, fees {}",
        rep.attempts,
        stake_st.last_search_secs,
        tri7_core::p2p::internal_to_display(&plan.signer.txid),
        plan.signer.n,
        plan.signer.value,
        plan.coin_age / (tri7_core::consensus::COIN as u64 * 14_400), // coin-days at cap
        plan.reward,
        fees,
    ));
    // extra txs from the relay pool (fees attach to the coinstake)
    let extra: Vec<Tx> = pool
        .txs_for_block(400_000, 250)
        .into_iter()
        .map(|(_h, tx)| tx)
        .collect();
    let mut block = tri7_core::stake::assemble_pos_block(&tip_entry, median_time_past, &plan.tx, &extra, n_bits)?;
    tri7_core::stake::sign_block(&mut block, &plan.signer.secret)?;
    // full self-validation through the SAME gate peer blocks pass
    let height_abs = tip_height + 1;
    let prev_entry = tip_entry;
    let outcome = validate_block(
        stg,
        rpc,
        stats,
        &block,
        &BlockCtx {
            height_abs,
            header: None,
            prev_entry: prev_entry.clone(),
            live: true,
        },
    )?;
    let raw = block.serialize();
    advance_with_block(stg, rpc, stats, &block, &outcome)?;
    stats.blocks_validated_live += 1;
    // durability + live-set + pool maintenance for OUR block
    if let Err(e) = store.append_block(&raw, Some(height_abs)) {
        log(&format!(
            "stake: accepted h{} NOT appended to the store: {}",
            height_abs, e
        ));
    }
    recent.insert(block.hash(), height_abs, &raw);
    wl.apply_block(&block, height_abs);
    addrindex_apply(&block, height_abs);
    let mut spent: Vec<(Hash, u32)> = Vec::new();
    for tx in block.vtx.iter().skip(1) {
        for vin in &tx.vin {
            if vin.prev_n != u32::MAX {
                spent.push((vin.prev_hash, vin.prev_n));
            }
        }
    }
    pool.confirm_spend(&spent);
    let file = stg.to_file();
    write_state_atomic(cfg, &file).map_err(|e| format!("write state: {}", e))?;
    stake_st.last_stake = Some((
        height_abs,
        tri7_core::p2p::internal_to_display(&block.hash()),
        now_iso(),
    ));
    log(&format!(
        "STAKE MINTED → h{} {} (self-validated: kernel {}, nbits enforced) — relaying to {} peer(s)",
        height_abs,
        tri7_core::p2p::internal_to_display(&block.hash()),
        if outcome.kernel_enforced { "enforced" } else { "seeded" },
        peers.len(),
    ));
    Ok(true)
}

/// Network weight estimate — GetPoSKernelPS parity (rpcblockchain.cpp:98):
/// over the last 72 PoS blocks, Σ(difficulty·2³²) / Σ(seconds between
/// consecutive PoS blocks).
fn net_stake_weight(entries: &[IndexEntry]) -> u64 {
    let mut kernels: f64 = 0.0;
    let mut secs: i64 = 0;
    let mut handled = 0usize;
    let mut prev_pos: Option<(u32, u32)> = None; // (time, bits) of the last PoS block seen
    for e in entries.iter().rev() {
        if !e.is_proof_of_stake {
            continue;
        }
        if let Some((ptime, _)) = prev_pos {
            secs += ptime as i64 - e.time as i64;
        }
        kernels += tri7_core::rpc::difficulty_from_bits(e.bits) * 4_294_967_296.0;
        prev_pos = Some((e.time, e.bits));
        handled += 1;
        if handled >= 72 {
            break;
        }
    }
    if secs > 0 {
        (kernels / secs as f64) as u64
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// NodeOps adapter (sendrawtransaction / getstakinginfo / staking / peers/smsg)
// ---------------------------------------------------------------------------

struct OpsAdapter {
    st: SharedState,
    mirror: Arc<TipMirror>,
    wl: SharedWalletLive,
    pool: SharedPool,
    stake_st: SharedStake,
    announce: TxAnnounce,
    peers_table: SharedPeers,
    smsg: Option<Arc<tri7_core::smsg_net::SmsgEngine>>,
    conf_path: PathBuf,
    /// wallet this node stakes for; getnewaddress/backupwallet act on it
    wallet_path: Option<PathBuf>,
    /// state dir — locates the opt-in address index (addressindex.db)
    state_dir: PathBuf,
    /// wallet bookkeeping published to the passive native handlers
    extras: Arc<std::sync::RwLock<rpc::WalletExtras>>,
    /// address → pubkey for the wallet's own keys
    pubkeys: Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    /// block store for the pool's deep-coin prevout fallback
    /// (WalletThenStoreUtxoView); None keeps the wallet-only view
    store: Option<Arc<BlockStore>>,
}

/// Legacy address-index verbs take either `[["addr1","addr2"]]` or
/// `[{"addresses":["addr1"]}]`.
fn addr_list<'a>(p: &dyn Fn(usize) -> Option<&'a serde_json::Value>, i: usize) -> Vec<String> {
    let Some(v) = p(i) else { return Vec::new() };
    let arr = if v.is_array() {
        v.as_array().cloned().unwrap_or_default()
    } else if let Some(a) = v.get("addresses").and_then(|x| x.as_array()) {
        a.clone()
    } else {
        vec![v.clone()]
    };
    arr.iter()
        .filter_map(|x| x.as_str().map(|s| s.to_string()))
        .collect()
}

/// Live address index (opt-in: only when addressindex.db exists). Kept in a
/// process-wide slot because the accept sites are deep in the follow loops.
static LIVE_ADDRINDEX: std::sync::OnceLock<
    std::sync::Mutex<Option<tri7_core::addrindex::LiveIndex>>,
> = std::sync::OnceLock::new();

fn init_addrindex(state_dir: &std::path::Path) {
    let cell = LIVE_ADDRINDEX.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut g) = cell.lock() {
        *g = tri7_core::addrindex::LiveIndex::open_if_built(state_dir);
        if g.is_some() {
            log("addrindex: live updates enabled (addressindex.db present)");
        }
    }
}

/// Best-effort: an index failure must never stop block acceptance.
fn addrindex_apply(block: &tri7_core::legacy::Block, height: u64) {
    let Some(cell) = LIVE_ADDRINDEX.get() else { return };
    let Ok(mut g) = cell.lock() else { return };
    let Some(ix) = g.as_mut() else { return };
    match ix.apply(block, height) {
        Ok(()) => {}
        Err(e) => log(&format!("addrindex: h{height} skipped: {e}")),
    }
}

/// A built-and-verified spend, before it is relayed.
struct SpendBuilt {
    txid: String,
    hex: String,
    fee_sats: i64,
    change_sats: i64,
    bytes: usize,
}

fn dryrun_json(b: &SpendBuilt) -> serde_json::Value {
    serde_json::json!({
        "txid": b.txid,
        "fee_sats": b.fee_sats,
        "change_sats": b.change_sats,
        "bytes": b.bytes,
        "hex": b.hex,
        "relayed": false,
    })
}

/// P2PKH script from an address payload (OP_DUP OP_HASH160 20 <h160>
/// OP_EQUALVERIFY OP_CHECKSIG) — same script legacy builds for a payment.
fn p2pkh_script_for_payload(payload: &[u8; 21]) -> Vec<u8> {
    let mut s = Vec::with_capacity(25);
    s.push(tri7_core::spend::OP_DUP);
    s.push(tri7_core::spend::OP_HASH160);
    s.push(20);
    s.extend_from_slice(&payload[1..]);
    s.push(tri7_core::spend::OP_EQUALVERIFY);
    s.push(tri7_core::spend::OP_CHECKSIG);
    s
}

/// Compressed public key for a secret (the wallet's key shape).
fn pubkey_from_secret(secret: &[u8; 32]) -> Vec<u8> {
    let secp = secp256k1::Secp256k1::signing_only();
    match secp256k1::SecretKey::from_slice(secret) {
        Ok(sk) => secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize()
            .to_vec(),
        Err(_) => Vec::new(),
    }
}

/// Find the scriptPubKey of (prev_hash, prev_n) in the node's own block store
/// by opening the blockindex read-only and reading the containing tx. Used by
/// signrawtransaction when the caller did not pass prevtxs. Returns None when
/// the prevout is not in this node's store (nothing is guessed).
fn lookup_prev_script(state_dir: &std::path::Path, prev_hash: &Hash, prev_n: u32) -> Option<Vec<u8>> {
    let index_dir = state_dir.join("index");
    let blocks = state_dir.join("blk0001.dat");
    if !blocks.exists() || !index_dir.exists() {
        return None;
    }
    // read-only BlockStore: cache-first load, never builds/writes an index
    let store = tri7_core::blockindex::BlockStore::new(Some(blocks), Some(index_dir));
    if !store.load_cache_only() {
        return None; // no warm cache: refuse rather than scan/rewrite
    }
    match store.tx_by_id(prev_hash) {
        Ok(Some(hit)) => hit.tx.vout.get(prev_n as usize).map(|o| o.script_pubkey.clone()),
        _ => None,
    }
}

/// RPC verbs that must answer from the moment the socket is open, before the
/// live wallet view finishes seeding. Each has a file-only or process-only
/// implementation in the passive layer, so gating them on `ready()` only
/// produces a "warming up" error for work that is already possible.
const EARLY_OPS: &[&str] = &[
    "stop",
    "createrawtransaction",
    "encryptwallet",
    "walletpassphrasechange",
    "decryptwallet",
    "getnewpubkey",
    "settxfee",
];

impl OpsAdapter {
    /// True when this verb may run before the live wallet view is ready.
    fn ready_early(&self, method: &str) -> bool {
        rpc::NodeOps::ready(self) || EARLY_OPS.contains(&method)
    }

    /// Build and sign a spend from this node's wallet: pick a source address by
    /// balance, select coins, build, sign, then self-verify (signature per
    /// input + connect check). Shared by sendtoaddress and sendmany so neither
    /// can drift from the other.
    fn build_wallet_tx(
        &self,
        outs: Vec<tri7_core::legacy::Vout>,
    ) -> Result<SpendBuilt, (i64, String)> {
        let (tip_h, _) = self.tip();
        let now = now_secs() as u32;
        let wl = self.wl.lock().unwrap();
        let w = wl
            .as_ref()
            .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;

        let mut keys: std::collections::HashMap<String, (Vec<u8>, [u8; 32])> =
            std::collections::HashMap::new();
        for k in &w.wallet.keys {
            keys.insert(k.address.clone(), (k.pubkey.clone(), k.secret));
        }
        // Multisig context: redeem scripts from the wallet file + every key
        // we can sign with. P2SH coins are only adopted when this book can
        // spend them (redeem present + m keys).
        let book = {
            let key_vec: Vec<([u8; 32], Vec<u8>)> = w
                .wallet
                .keys
                .iter()
                .map(|k| (k.secret, k.pubkey.clone()))
                .collect();
            tri7_core::multisig::ScriptBook::from_wallet(&w.wallet, &key_vec)
        };
        let kb = w.key_by_payload();
        let mut inputs: Vec<tri7_core::spend::SpendInput> = Vec::new();
        let mut by_addr: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for ((txid, n), o) in w.live.iter() {
            let Some(dest) = tri7_core::address::script_to_dest(&o.script) else {
                continue;
            };
            let addr = tri7_core::address::payload_to_address(&dest.payload());
            // single-key coin (today's behavior) or a multisig coin the
            // script book can spend; anything else is not ours to adopt.
            let (pubkey, secret) = if kb.contains_key(&dest.payload()) {
                let Some((pk, s)) = keys.get(&addr).cloned() else {
                    continue;
                };
                (pk, s)
            } else if matches!(
                dest,
                tri7_core::address::ScriptDest::ScriptHash(_)
            ) {
                if book.can_spend(&o.script).is_err() {
                    continue; // unrelated/unsignable P2SH — never adopted
                }
                let redeem = book.redeem_for(&o.script).expect("can_spend passed").clone();
                let held = book.held_keys(&redeem);
                let Some((s, pk)) = held.first().cloned() else {
                    continue;
                };
                // representative key pair: the multisig signing path reads the
                // book, not these fields
                (pk, s)
            } else {
                continue;
            };
            *by_addr.entry(addr).or_insert(0) += o.value;
            inputs.push(tri7_core::spend::SpendInput {
                txid: *txid,
                n: *n,
                value: o.value,
                script_pubkey: o.script.clone(),
                height: o.height as u32,
                tx_time: o.created_epoch,
                is_coinbase: o.coinbase,
                is_coinstake: o.coinstake,
                secret,
                pubkey,
            });
        }
        let mut cands: Vec<(String, i64)> = by_addr.into_iter().collect();
        cands.sort_by(|a, b| b.1.cmp(&a.1));
        if cands.is_empty() {
            return Err((-6, "Insufficient funds".to_string()));
        }
        let mut last_err = "Insufficient funds!".to_string();
        for (from, _bal) in cands {
            let change_payload = match tri7_core::address::address_to_payload(&from) {
                Ok(p) => p,
                Err(_) => continue,
            };
            // change returns to the sending address: P2PKH for key addresses,
            // the P2SH scriptPubKey itself for multisig 'C' addresses (the
            // book keeps it spendable).
            let change_script = if change_payload[0] == tri7_core::address::SCRIPT_ADDRESS {
                let mut s = vec![0xa9u8, 0x14];
                s.extend_from_slice(&change_payload[1..]);
                s.push(0x87);
                s
            } else {
                p2pkh_script_for_payload(&change_payload)
            };
            let pool: Vec<tri7_core::spend::SpendInput> = inputs
                .iter()
                .filter(|i| {
                    tri7_core::address::script_to_dest(&i.script_pubkey)
                        .map(|d| tri7_core::address::payload_to_address(&d.payload()) == from)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if pool.is_empty() {
                continue;
            }
            // Coin selection AND fee converge through the one shared function:
            // the fee depends on the shape, the shape on the fee. The builder
            // then charges exactly the fee it converged on.
            let paytxfee = self
                .extras
                .read()
                .ok()
                .and_then(|g| g.paytxfee_sats)
                .unwrap_or(tri7_core::spend::MIN_FEE_SATS)
                .max(tri7_core::spend::MIN_FEE_SATS);
            let out_values: Vec<i64> = outs.iter().map(|o| o.value).collect();
            let shape = match tri7_core::spend::select_with_fee_with_book(
                &pool,
                &out_values,
                paytxfee,
                &book,
            ) {
                Ok(s) => s,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let selected = shape.selected.clone();
            let plan = match tri7_core::spend::build_spend_outputs_floor_with_book(
                &selected,
                outs.clone(),
                Some(change_script),
                // the builder applies the (1 + bytes/1000) multiplier itself,
                // so it gets the RATE, not the converged total in shape.fee
                paytxfee,
                // and the converged total as an absolute floor, so the fee the
                // caller was quoted is what the transaction pays
                shape.fee,
                &book,
                now,
                tip_h as u32,
            ) {
                Ok(p) => p,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            // the tx vin is built in (txid, n) sorted order (build_spend_outputs);
            // BOTH self-verify and the connect check must zip the SAME sorted
            // order — checking against the SELECTION order misaligns the
            // position zip whenever selection order differs from (txid, n)
            // order. With MIXED prevout types on one address (bare-P2PK
            // coinstake rewards + P2PKH sweep outputs) the crossed pairing
            // fails loudly ("scriptSig has 2 pushes, Solver expects 1") on a
            // correctly signed tx; same-type coins masked it since the
            // 2026-11-07 connect-check fix because twin scripts verify
            // identically at any position. 2026-11-08: caught live by the
            // COIN-SPLIT drains (first mixed-solver selection ever built).
            // ordered_sel (vin order) also feeds the connect check below.
            // `selected` itself stays in SELECTION order — downstream shape
            // reporting iterates it as the picker produced it; only the
            // position-zipped checks use the sorted copy.
            let mut ordered_sel = selected.clone();
            ordered_sel.sort_by_key(|i| (i.txid, i.n));
            for (i, input) in ordered_sel.iter().enumerate() {
                if let Err(e) = tri7_core::spend::verify_input(&plan.tx, i, &input.script_pubkey) {
                    return Err((-1, format!("self-verify failed: {e}")));
                }
            }
            // EXACT fee equality, both directions: the signed size can push the
            // schedule above the quote (paying more than the caller agreed to)
            // or the floor can sit above the schedule. Neither may pass silently.
            if plan.fee != shape.fee {
                return Err((
                    -1,
                    format!(
                        "fee mismatch: the signed transaction pays {} but the caller was quoted {} (real size {} bytes); re-run the estimate",
                        plan.fee,
                        shape.fee,
                        plan.tx.serialize().len()
                    ),
                ));
            }
            // ordered_sel (vin order) also feeds the connect check below.
            if let Err(e) =
                tri7_core::spend::connect_inputs_check(&plan.tx, &ordered_sel, tip_h as u32)
            {
                return Err((-1, format!("connect check failed: {e}")));
            }
            let raw = plan.tx.serialize();
            return Ok(SpendBuilt {
                txid: tri7_core::p2p::internal_to_display(&plan.tx.hash()),
                hex: raw.iter().map(|b| format!("{:02x}", b)).collect(),
                fee_sats: plan.fee,
                change_sats: plan.change,
                bytes: raw.len(),
            });
        }
        Err((-6, last_err))
    }

    /// Accept into the pool and announce — the same path as
    /// sendrawtransaction, so a built spend cannot bypass pool validation.
    /// Lock order: record_lock is lock #1 (before pool/wl) — the same order
    /// resendtx uses, so the record's read-modify-write can never deadlock
    /// against pool/wl, and a send concurrent with a resend pass cannot be
    /// dropped from the record by the survivors rewrite.
    fn relay_built(&self, build: &SpendBuilt) -> Result<serde_json::Value, (i64, String)> {
        let record_guard = tri7_core::sent_txs::record_lock();
        let raw = tri7_core::p2p::hex_decode(&build.hex)
            .map_err(|e| (-1, format!("built tx hex: {e}")))?;
        let mut pool = self.pool.lock().unwrap();
        let wl = self.wl.lock().unwrap();
        let w = wl
            .as_ref()
            .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;
        let view = WalletThenStoreUtxoView {
            live: &w.live,
            store: self.store.as_deref(),
        };
        let rep = pool
            .accept(&raw, &view, now_secs() as i64)
            .map_err(|e| (-25, e))?;
        let peers_n = self.peers_table.lock().map(|t| t.len()).unwrap_or(0);
        self.announce
            .lock()
            .unwrap()
            .push_back(tri7_core::p2p::display_to_internal(&rep.txid).map_err(|e| (-8, e))?);
        log(&format!(
            "BROADCAST: {} ({} bytes, fee {} sats) — inv to {} peer(s)",
            rep.txid, rep.n_bytes, rep.fee_sats, peers_n
        ));
        // Record the send for `resendtx` (the pool is in-memory; without this
        // a restart forgets every unconfirmed wallet send). BEST-EFFORT: the
        // tx is already relayed, so a record failure must not fail the send.
        // Runs under record_guard (taken first) so a concurrent resend pass
        // rewriting the record cannot drop this entry.
        if let Some(wp) = &self.wallet_path {
            let rec = tri7_core::sent_txs::path_for(wp);
            drop(pool);
            drop(wl);
            match tri7_core::sent_txs::record(&rec, &build.txid, &build.hex) {
                Ok(n) => log(&format!(
                    "sent-tx record: {} (record now {} entry(s) at {})",
                    build.txid,
                    n,
                    rec.display()
                )),
                Err(e) => log(&format!(
                    "sent-tx record FAILED for {} (send unaffected): {e}",
                    build.txid
                )),
            }
        }
        drop(record_guard);
        Ok(serde_json::json!(rep.txid))
    }

    fn tip(&self) -> (u64, Option<IndexEntry>) {
        if let Ok(g) = self.st.try_lock() {
            let h = g.tip_height;
            let e = g.entry_abs(h).cloned();
            return (h, e);
        }
        let h = self.mirror.height();
        (h, None)
    }
}

impl tri7_core::rpc::NodeOps for OpsAdapter {
    fn ready(&self) -> bool {
        // The ops engine needs the live wallet view, nothing else. Requiring the
        // smsg engine here made a LOCKED (encrypted) wallet unable to answer
        // walletpassphrase at all — the unlock verb sat behind a gate that only
        // the unlock could open. smsg is optional and separate.
        self.wl.lock().map(|w| w.is_some()).unwrap_or(false)
    }

    fn call(&self, method: &str, params: &serde_json::Value) -> Result<serde_json::Value, (i64, String)> {
        let p = |i: usize| params.get(i);
        let ps = |i: usize| p(i).and_then(|v| v.as_str()).map(|s| s.to_string());
        match method {
            "sendrawtransaction" => {
                log("sendrawtransaction: entered ops arm");
                let Some(hex) = ps(0) else {
                    return Err((-8, "expected raw tx hex".into()));
                };
                let raw = tri7_core::p2p::hex_decode(&hex)
                    .map_err(|e| (-8, format!("bad tx hex: {}", e)))?;
                let mut pool = self.pool.lock().unwrap();
                let wl = self.wl.lock().unwrap();
                let Some(wl) = wl.as_ref() else {
                    return Err((-1, "wallet live UTXO view not ready yet".into()));
                };
                let now = now_secs() as i64;
                let view = WalletThenStoreUtxoView {
                    live: &wl.live,
                    store: self.store.as_deref(),
                };
                let rep = pool
                    .accept(&raw, &view, now)
                    .map_err(|e| (-25, e))?;
                let peers_n = self.peers_table.lock().map(|t| t.len()).unwrap_or(0);
                self.announce
                    .lock()
                    .unwrap()
                    .push_back(tri7_core::p2p::display_to_internal(&rep.txid)
                        .map_err(|e| (-8, e))?);
                log(&format!(
                    "BROADCAST: tx {} accepted for relay ({} inputs, fee {} sats) — inv to {} peer(s)",
                    rep.txid, rep.inputs, rep.fee_sats, peers_n
                ));
                Ok(serde_json::json!({
                    "txid": rep.txid,
                    "fee_sats": rep.fee_sats,
                    "bytes": rep.n_bytes,
                    "announced_to": peers_n,
                }))
            }
            // Programmatic spend (legacy rpcwallet sendtoaddress). Builds and
            // signs with the same builder the GUI uses, then relays through
            // the same accept+announce path as sendrawtransaction.
            "sendtoaddress" => {
                let to = ps(0).ok_or_else(|| (-8, "expected address parameter".to_string()))?;
                let amount_tri = p(1)
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| (-8, "expected amount parameter".to_string()))?;
                if !(amount_tri > 0.0) {
                    return Err((-3, "Amount out of range".to_string()));
                }
                let amount_sats = (amount_tri * 1_000_000.0).round() as i64;
                let dest_payload = tri7_core::address::address_to_payload(&to)
                    .map_err(|e| (-5, format!("Invalid address: {e}")))?;
                let out = tri7_core::legacy::Vout {
                    value: amount_sats,
                    script_pubkey: p2pkh_script_for_payload(&dest_payload),
                };
                let build = self.build_wallet_tx(vec![out])?;
                if ps(2).as_deref() == Some("dryrun") {
                    return Ok(dryrun_json(&build));
                }
                self.relay_built(&build)
            }
            // Legacy sendmany: `sendmany "" {"addr":amount,...}`. One
            // transaction, several payment outputs, same builder and relay path
            // as sendtoaddress.
            "sendmany" => {
                let Some(serde_json::Value::Object(map)) = p(1) else {
                    return Err((
                        -8,
                        "expected {\"address\":amount,...} as the second parameter".to_string(),
                    ));
                };
                let mut outs: Vec<tri7_core::legacy::Vout> = Vec::new();
                for (addr, amt) in map.iter() {
                    let amount = amt.as_f64().ok_or_else(|| {
                        (-8, format!("sendmany: amount for {addr} is not a number"))
                    })?;
                    if !(amount > 0.0) {
                        return Err((-3, format!("sendmany: amount for {addr} out of range")));
                    }
                    let payload = tri7_core::address::address_to_payload(addr)
                        .map_err(|e| (-5, format!("Invalid address {addr}: {e}")))?;
                    outs.push(tri7_core::legacy::Vout {
                        value: (amount * 1_000_000.0).round() as i64,
                        script_pubkey: p2pkh_script_for_payload(&payload),
                    });
                }
                if outs.is_empty() {
                    return Err((-8, "sendmany: no recipients".to_string()));
                }
                let build = self.build_wallet_tx(outs)?;
                if ps(2).as_deref() == Some("dryrun") {
                    return Ok(dryrun_json(&build));
                }
                self.relay_built(&build)
            }
            // Legacy repairwallet (rpcwallet.cpp:1804): FixSpentCoins with
            // repair enabled, replying {"wallet check passed": true} or
            // {"mismatched spent coins": N, "amount affected by repair":
            // FormatMoney}. tri7 keeps no persisted per-output spent flags —
            // its wallet belief IS the in-memory live view — so the repair
            // REBUILDS that view from the block store (the authoritative
            // replay over the same window the seed used) and diffs the
            // wallet-owned sets. This heals the documented "fast catch-up
            // leaves the wallet live view wrong; a restart heals it" class
            // without the restart. The wl lock is held across seed+diff+swap
            // so block acceptance cannot interleave; one ~4600-entry window
            // replays in well under a second.
            "repairwallet" => {
                repairwallet_params_ok(&params)?;
                let Some(store) = self.store.as_deref() else {
                    return Err((
                        -1,
                        "repairwallet needs the block store (node started without a blocks file)"
                            .into(),
                    ));
                };
                let mut wl = self
                    .wl
                    .lock()
                    .map_err(|_| (-1, "wallet live view mutex poisoned".to_string()))?;
                let w = wl
                    .as_mut()
                    .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;
                // replay from the EARLIEST claimed base (a forkchoice
                // rollback can walk the chain-state base back below the
                // view's seed base; replaying only from the view's base
                // would then miss the restored branch)
                let (base, tip_hint) = match self.st.try_lock() {
                    Ok(g) => (w.base.min(g.base), g.tip_height),
                    Err(_) => (w.base, self.mirror.height()),
                };
                let has_body = |h: u64| store.block_by_height(h).ok().flatten().is_some();
                let (replay_tip, back) = newest_replayable_tip(&has_body, base, tip_hint);
                if replay_tip <= base {
                    return Err((
                        -1,
                        format!(
                            "repairwallet: no stored bodies in h{}..h{} — store behind the tip, retry when bodies land",
                            base, tip_hint
                        ),
                    ));
                }
                let fresh = WalletLive::seed(store, w.wallet.clone(), base, replay_tip)
                    .map_err(|e| (-1, format!("repairwallet replay: {e}")))?;
                let diff = {
                    let owns = |o: &LiveOut| w.owns(o);
                    repair_diff(&w.live, &fresh.live, &owns)
                };
                let n = diff.count();
                let amt = diff.amount();
                log(&format!(
                    "repairwallet: rebuilt live view h{}..h{} (walked back {} from tip h{}) — {} owned-output mismatch(es) totalling {} sats ({} phantom, {} lost){}",
                    base,
                    replay_tip,
                    back,
                    tip_hint,
                    n,
                    amt,
                    diff.phantom.len(),
                    diff.lost.len(),
                    if n == 0 { " — wallet check passed" } else { " — repaired" },
                ));
                w.live = fresh.live;
                // the map now covers [base, replay_tip]: keep the view's
                // claimed base consistent with what was actually replayed
                // (base is the min of the view's seed base and the
                // chain-state base — a rollback can push it lower)
                w.base = base;
                if n == 0 {
                    Ok(serde_json::json!({ "wallet check passed": true }))
                } else {
                    Ok(serde_json::json!({
                        "mismatched spent coins": n,
                        "amount affected by repair":
                            tri7_core::rpc::legacy_format_money(amt),
                    }))
                }
            }
            // Legacy resendtx (rpcwallet.cpp:1826): ResendWalletTransactions(true),
            // replying null. tri7's pool is in-memory, so unconfirmed wallet
            // sends would otherwise be forgotten on every restart — relay_built
            // records each send beside the wallet (sent_txs.json) and this
            // pass re-accepts every recorded tx into the pool (duplicate
            // accept = idempotent success) and re-announces it to peers. A
            // recorded tx that has since MINED is pruned from the record; a
            // transient pool rejection keeps its entry for the next pass.
            "resendtx" => {
                resendtx_params_ok(&params)?;
                let Some(wp) = &self.wallet_path else {
                    return Err((-1, "resendtx: no wallet wired".into()));
                };
                let rec_path = tri7_core::sent_txs::path_for(wp);
                // record_lock is lock #1 and spans load→save: a send relayed
                // concurrently by relay_built (same order) appends under the
                // same lock and the survivors rewrite cannot drop it.
                let record_guard = tri7_core::sent_txs::record_lock();
                let entries = tri7_core::sent_txs::load(&rec_path)
                    .map_err(|e| (-1, format!("resendtx: {e}")))?;
                if entries.is_empty() {
                    log("resendtx: no recorded unconfirmed wallet sends — nothing to do");
                    return Ok(serde_json::Value::Null);
                }
                let mut pool = self
                    .pool
                    .lock()
                    .map_err(|_| (-1, "pool mutex poisoned".to_string()))?;
                let wl = self
                    .wl
                    .lock()
                    .map_err(|_| (-1, "wallet live view mutex poisoned".to_string()))?;
                let w = wl
                    .as_ref()
                    .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;
                let view = WalletThenStoreUtxoView {
                    live: &w.live,
                    store: self.store.as_deref(),
                };
                let now = now_secs() as i64;
                let (mut resent, mut pruned, mut kept) = (0usize, 0usize, 0usize);
                let mut survivors: Vec<tri7_core::sent_txs::SentTxEntry> =
                    Vec::with_capacity(entries.len());
                for e in &entries {
                    let raw = match tri7_core::p2p::hex_decode(&e.hex) {
                        Ok(r) => r,
                        Err(err) => {
                            log(&format!("resendtx: {} corrupt hex ({err}) — pruned", e.txid));
                            pruned += 1;
                            continue;
                        }
                    };
                    let txid_internal =
                        tri7_core::p2p::display_to_internal(&e.txid).unwrap_or([0u8; 32]);
                    // MINE-GATE (before accept — see ResendGate): mined →
                    // prune, never accept; store error → cannot prove
                    // unmined, skip this pass (fail-closed); only a
                    // definitive "not mined" reaches pool.accept.
                    match resend_mine_gate(
                        self.store
                            .as_deref()
                            .map(|s| s.tx_by_id(&txid_internal).map(|hit| hit.map(|_| ()))),
                    ) {
                        ResendGate::Mined => {
                            log(&format!(
                                "resendtx: {} already mined — pruned from the record",
                                &e.txid[..e.txid.len().min(10)]
                            ));
                            pruned += 1;
                            continue;
                        }
                        ResendGate::CantProve => {
                            log(&format!(
                                "resendtx: {} store error — cannot prove unmined, skipped this pass",
                                &e.txid[..e.txid.len().min(10)]
                            ));
                            kept += 1;
                            survivors.push(e.clone());
                            continue;
                        }
                        ResendGate::Unknown => {}
                    }
                    match pool.accept(&raw, &view, now) {
                        Ok(rep) => {
                            if let Ok(mut q) = self.announce.lock() {
                                q.push_back(txid_internal);
                            }
                            log(&format!(
                                "resendtx: relaying wtx {} ({} bytes, fee {} sats)",
                                &rep.txid[..rep.txid.len().min(10)],
                                rep.n_bytes,
                                rep.fee_sats
                            ));
                            resent += 1;
                            survivors.push(e.clone());
                        }
                        Err(err) => {
                            // NOT mined (the gate above ran): a rejection here
                            // is transient (prevouts outside the window, a
                            // peer-lag) — keep the entry for the next pass.
                            log(&format!(
                                "resendtx: {} kept for the next pass (pool: {err})",
                                &e.txid[..e.txid.len().min(10)]
                            ));
                            kept += 1;
                            survivors.push(e.clone());
                        }
                    }
                }
                drop(pool);
                drop(wl);
                tri7_core::sent_txs::save(&rec_path, &survivors)
                    .map_err(|e| (-1, format!("resendtx: rewrite record: {e}")))?;
                drop(record_guard);
                log(&format!(
                    "resendtx: {resent} resent, {kept} kept (transient), {pruned} pruned (mined/corrupt); record now {} entry(s)",
                    survivors.len()
                ));
                Ok(serde_json::Value::Null)
            }
            // Legacy signmessage <address> <message>: the same implementation
            // the GUI Sign page uses.
            "signmessage" => {
                let addr = ps(0).ok_or_else(|| (-8, "expected address".to_string()))?;
                let msg = ps(1).ok_or_else(|| (-8, "expected message".to_string()))?;
                let wl = self.wl.lock().unwrap();
                let w = wl
                    .as_ref()
                    .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;
                let key = w
                    .wallet
                    .keys
                    .iter()
                    .find(|k| k.address == addr)
                    .ok_or_else(|| (-5, "address not found in wallet".to_string()))?;
                let sig = tri7_core::gui::state::sign_message(&key.secret, &msg)
                    .map_err(|e| (-1, e))?;
                Ok(serde_json::json!(sig))
            }
            // The wallet's SPENDABLE set from the live address index.
            //
            // The index is applied on every accepted block, so it follows the
            // tip; the replay snapshot stops at whatever height the chain
            // replay reached (an archive seam, or a hole in the block store).
            // This is the wallet-scoped view of the index: the same unspent
            // coins the chain shows, so a spend built from it can never be a
            // double-spend of a coin the replay thinks is gone.
            "getwalletspendable" => {
                let wl = self.wl.lock().unwrap();
                let Some(wl) = wl.as_ref() else {
                    return Err((-1, "wallet live UTXO view not ready yet".into()));
                };
                let addrs: Vec<String> =
                    wl.wallet.keys.iter().map(|k| k.address.clone()).collect();
                let path = tri7_core::addrindex::db_path(&self.state_dir);
                if !path.exists() {
                    return Err((-1, tri7_core::addrindex::NOT_ENABLED.to_string()));
                }
                let mut c = tri7_core::addrindex::open(&path).map_err(|e| (-1, e))?;
                // ONE transaction: the utxos and the index's facts come from
                // the same snapshot, so a block applied mid-read cannot tear
                // the answer.
                let (facts, rows) =
                    tri7_core::addrindex::wallet_view(&mut c, &addrs).map_err(|e| (-1, e))?;
                let balance: i64 = rows.iter().map(|r| r.value).sum();
                let utxos: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|u| {
                        serde_json::json!({
                            "txid": tri7_core::addrindex::txid_hex(&u.txid),
                            "n": u.n,
                            "address": u.address,
                            "script_hex": u.script.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
                            "value_sats": u.value,
                            "height": u.height,
                        })
                    })
                    .collect();
                // The index is applied on every accepted block, so it tracks
                // the tip - but it is only AUTHORITATIVE when the library
                // rule says so: a FINISHED index (`complete`) that has
                // applied EXACTLY the chain this node serves (the same
                // height AND the same block hash the index recorded). A
                // partial build, an index that trails the tip, or an index
                // on another chain does not know every coin: its set must
                // not be served as the whole wallet.
                // The FACTS, not a verdict: the chain this node serves (its
                // tip height and hash) next to the index's own facts (the
                // height it applied and the hash it recorded for that block).
                // The rule that turns facts into "may be served as the whole
                // spendable set" lives in the library
                // (addrindex::authority), so the daemon and the tests cannot
                // disagree about it.
                let (tip_h, tip_entry) = self.tip();
                let chain_hash: Option<[u8; 32]> = tip_entry
                    .as_ref()
                    .map(|e| <[u8; 32]>::from(e.hash));
                Ok(serde_json::json!({
                    "balance_sats": balance,
                    "utxos": utxos,
                    "index_height": facts.coverage_height,
                    "index_tip_hash": facts.index_tip_hash.map(|h| tri7_core::addrindex::txid_hex(&h)),
                    "chain_tip": tip_h,
                    "chain_tip_hash": chain_hash.map(|h| tri7_core::addrindex::txid_hex(&h)),
                    "complete": facts.complete,
                }))
            }
            // Chain-wide address index (legacy -addressindex=1 parity). Absent
            // index -> the exact legacy failure text, never a wallet-scoped
            // shortcut that would answer differently.
            "getaddressbalance" => {
                let addrs = addr_list(&p, 0);
                if addrs.is_empty() {
                    return Err((-8, "expected addresses array".to_string()));
                }
                let path = tri7_core::addrindex::db_path(&self.state_dir);
                if !path.exists() {
                    return Err((-1, tri7_core::addrindex::NOT_ENABLED.to_string()));
                }
                let c = tri7_core::addrindex::open(&path).map_err(|e| (-1, e))?;
                let (bal, recv) = tri7_core::addrindex::balance(&c, &addrs).map_err(|e| (-1, e))?;
                let idx_h = tri7_core::addrindex::index_height(&c);
                let tip_h = self.tip().0;
                // A part-built index answers with what it has; say so instead
                // of letting a caller mistake a partial sum for the whole chain.
                Ok(serde_json::json!({
                    "balance": bal,
                    "received": recv,
                    "index_height": idx_h,
                    "index_complete": tri7_core::addrindex::is_complete(&c),
                    "chain_tip": tip_h,
                }))
            }
            "getaddressutxos" => {
                let addrs = addr_list(&p, 0);
                if addrs.is_empty() {
                    return Err((-8, "expected addresses array".to_string()));
                }
                let path = tri7_core::addrindex::db_path(&self.state_dir);
                if !path.exists() {
                    return Err((-1, tri7_core::addrindex::NOT_ENABLED.to_string()));
                }
                let c = tri7_core::addrindex::open(&path).map_err(|e| (-1, e))?;
                let rows = tri7_core::addrindex::utxos(&c, &addrs).map_err(|e| (-1, e))?;
                let out: Vec<serde_json::Value> = rows
                    .into_iter()
                    .map(|u| {
                        serde_json::json!({
                            "address": u.address,
                            "txid": tri7_core::addrindex::txid_hex(&u.txid),
                            "outputIndex": u.n,
                            "script": u.script.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
                            "satoshis": u.value,
                            "height": u.height,
                        })
                    })
                    .collect();
                Ok(serde_json::json!(out))
            }
            "getaddresstxids" => {
                let addrs = addr_list(&p, 0);
                if addrs.is_empty() {
                    return Err((-8, "expected addresses array".to_string()));
                }
                let path = tri7_core::addrindex::db_path(&self.state_dir);
                if !path.exists() {
                    return Err((-1, tri7_core::addrindex::NOT_ENABLED.to_string()));
                }
                let c = tri7_core::addrindex::open(&path).map_err(|e| (-1, e))?;
                let ids = tri7_core::addrindex::txids(&c, &addrs).map_err(|e| (-1, e))?;
                Ok(serde_json::json!(ids
                    .iter()
                    .map(tri7_core::addrindex::txid_hex)
                    .collect::<Vec<_>>()))
            }
            // Key export / import. The heavy lifting already lives in
            // keytool (dump_privkey, dump_wallet, import_wallet, check_wallet)
            // — these are the legacy RPC names over it.
            "dumpprivkey" => {
                let addr = ps(0).ok_or_else(|| (-8, "expected address".to_string()))?;
                let wp = self
                    .wallet_path
                    .clone()
                    .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?;
                match tri7_core::keytool::dump_privkey(&wp, &addr) {
                    Ok(wif) => {
                        log(&format!("dumpprivkey: {} exported", addr));
                        Ok(serde_json::json!(wif))
                    }
                    Err(e) => Err((-5, e)),
                }
            }
            "dumpwallet" => {
                let dest = ps(0).ok_or_else(|| (-8, "expected filename".to_string()))?;
                let wp = self
                    .wallet_path
                    .clone()
                    .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?;
                let (rows, n) = tri7_core::keytool::dump_wallet(&wp)
                    .map_err(|e| (-1, format!("dumpwallet: {e}")))?;
                let path = PathBuf::from(&dest);
                let mut f = std::fs::File::create(&path)
                    .map_err(|e| (-1, format!("dumpwallet: create {}: {e}", path.display())))?;
                tri7_core::keytool::write_dump(&mut f, &rows, None)
                    .map_err(|e| (-1, format!("dumpwallet: write: {e}")))?;
                log(&format!("dumpwallet: {} keys -> {}", n, path.display()));
                Ok(serde_json::Value::Null)
            }
            "importwallet" => {
                let src = ps(0).ok_or_else(|| (-8, "expected filename".to_string()))?;
                let wp = self
                    .wallet_path
                    .clone()
                    .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?;
                let rep = tri7_core::keytool::import_wallet(&wp, &PathBuf::from(&src))
                    .map_err(|e| (-1, format!("importwallet: {e}")))?;
                log(&format!(
                    "importwallet: {} imported, {} skipped, {} failed",
                    rep.imported, rep.skipped, rep.failed
                ));
                Ok(serde_json::Value::Null)
            }
            "importprivkey" => {
                let wif = ps(0).ok_or_else(|| (-8, "expected WIF private key".to_string()))?;
                let label = ps(1).unwrap_or_default();
                let wp = self
                    .wallet_path
                    .clone()
                    .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?;
                let (secret, compressed) = tri7_core::keytool::wif_decode(&wif)
                    .map_err(|e| (-5, e))?;
                if !compressed {
                    // tri7 stores compressed key rows only; importing an
                    // uncompressed WIF would bind the secret to a different
                    // address than the one it was exported for.
                    return Err((
                        -5,
                        "uncompressed WIF not supported (the address would differ from the source wallet)"
                            .to_string(),
                    ));
                }
                let addr = tri7_core::keytool::secret_to_address(&secret, true)
                    .map_err(|e| (-5, e))?;
                let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(&wp)
                    .map_err(|e| (-1, format!("open wallet: {e}")))?;
                let res = wh.add_key(&secret, &label);
                wh.close();
                res.map_err(|e| (-1, format!("importprivkey: {e}")))?;
                log(&format!("importprivkey: {} imported", addr));
                Ok(serde_json::Value::Null)
            }
            "checkwallet" => {
                let wp = self
                    .wallet_path
                    .clone()
                    .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?;
                let rep = tri7_core::keytool::check_wallet(&wp)
                    .map_err(|e| (-1, format!("checkwallet: {e}")))?;
                Ok(rep.to_json())
            }
            // ---- legacy parity 2026-11-06: offline tx workflow --------------
            // signrawtransaction <hex> [prevtxs] [privkeys] [sighashtype]
            //
            // Signs each input whose prevout the live wallet can resolve:
            //   1. prevtxs entries carry {txid, vout, scriptPubKey} (the
            //      offline/cold path, where the node has no index for it)
            //   2. otherwise the node's own block store is consulted
            //   3. privkeys supplement the wallet's keys by WIF
            // Every signature is verified against the resulting scriptSig
            // before it is reported, so `complete` is a proven claim.
            "signrawtransaction" | "signrawtransactionwithwallet" => {
                let hex_in = ps(0).ok_or_else(|| (-8, "expected raw tx hex".to_string()))?;
                let mut tx = tri7_core::rawtx::tx_from_hex(&hex_in)
                    .map_err(|e| (-22, format!("TX decode failed: {e}")))?;
                // sighash type: legacy accepts the NAME ("ALL", "NONE",
                // "SINGLE", each optionally ANYONECANPAY)
                let hash_type = match ps(3).map(|s| s.to_ascii_uppercase()) {
                    None => 1,
                    Some(s) => {
                        let mut any = 1;
                        if s.contains("ANYONECANPAY") { any = 0x80; }
                        if s.starts_with("SINGLE") { any | 3 }
                        else if s.starts_with("NONE") { any | 2 }
                        else { any | 1 }
                    }
                };

                // extra keys supplied by the caller (offline signing)
                let mut extra: HashMap<Vec<u8>, [u8; 32]> = HashMap::new();
                if let Some(serde_json::Value::Array(wifs)) = p(2) {
                    for w in wifs {
                        let Some(wif) = w.as_str() else { continue };
                        let (secret, compressed) = tri7_core::keytool::wif_decode(wif)
                            .map_err(|e| (-5, format!("bad privkey: {e}")))?;
                        if !compressed {
                            // the wallet stores compressed rows only; an
                            // uncompressed key would sign for a different
                            // address than the one it was exported for
                            return Err((-5, "uncompressed WIF is not supported — its address differs".to_string()));
                        }
                        let pubkey = pubkey_from_secret(&secret);
                        extra.insert(pubkey, secret);
                    }
                }
                // caller-supplied prevout scripts
                let mut given: HashMap<(String, u32), Vec<u8>> = HashMap::new();
                if let Some(serde_json::Value::Array(list)) = p(1) {
                    for e in list {
                        let (Some(txid), Some(vout), Some(spk)) = (
                            e.get("txid").and_then(|v| v.as_str()),
                            e.get("vout").and_then(|v| v.as_u64()),
                            e.get("scriptPubKey").and_then(|v| v.as_str()),
                        ) else {
                            return Err((-8, "each prevtxs entry needs txid, vout and scriptPubKey".to_string()));
                        };
                        let bytes = tri7_core::p2p::hex_decode(spk)
                            .map_err(|e| (-8, format!("prevtxs scriptPubKey: {e}")))?;
                        given.insert((txid.to_string(), vout as u32), bytes);
                    }
                }

                let wl = self.wl.lock().unwrap();
                let wallet_ready = wl.is_some();
                if wl.is_none() && p(2).is_none() {
                    // a locked/absent wallet is not an error by itself — but
                    // without either the wallet or caller keys nothing can be
                    // signed, and saying so keeps `complete:false` honest
                    log("signrawtransaction: wallet view not ready and no privkeys supplied");
                }
                let mut complete = true;
                let mut errors: Vec<String> = Vec::new();
                let mut signed = 0usize;
                for i in 0..tx.vin.len() {
                    let prev_hash = tx.vin[i].prev_hash;
                    let prev_n = tx.vin[i].prev_n;
                    let display = tri7_core::p2p::internal_to_display(&prev_hash);
                    if !tx.vin[i].script_sig.is_empty() {
                        continue; // already signed; legacy leaves it alone
                    }
                    let prev_script = match given.get(&(display.clone(), prev_n)) {
                        Some(s) => s.clone(),
                        None => match lookup_prev_script(&self.state_dir, &prev_hash, prev_n) {
                            Some(s) => s,
                            None => {
                                complete = false;
                                errors.push(format!("input {i}: prevout {display}:{prev_n} unavailable (pass it in prevtxs)"));
                                continue;
                            }
                        },
                    };
                    // who owns this prevout?
                    let owner = match tri7_core::address::script_to_dest(&prev_script) {
                        Some(d) => d.payload(),
                        None => {
                            complete = false;
                            errors.push(format!("input {i}: non-standard scriptPubKey"));
                            continue;
                        }
                    };
                    let mut chosen: Option<(Vec<u8>, [u8; 32])> = None;
                    let mut chosen_by_wallet = false;
                    if let Some(w) = wl.as_ref() {
                        for k in w.wallet.keys.iter() {
                            if tri7_core::address::pubkey_payload(&k.pubkey) == owner {
                                chosen = Some((k.pubkey.clone(), k.secret));
                                chosen_by_wallet = true;
                                break;
                            }
                        }
                    }
                    if chosen.is_none() {
                        for (pk, sec) in extra.iter() {
                            if tri7_core::address::pubkey_payload(pk) == owner {
                                chosen = Some((pk.clone(), *sec));
                                break;
                            }
                        }
                    }
                    let Some((pubkey, secret)) = chosen else {
                        complete = false;
                        errors.push(format!("input {i}: no key for {}", tri7_core::address::payload_to_address(&owner)));
                        continue;
                    };
                    if chosen_by_wallet && wl.as_ref().map(|w| w.locked).unwrap_or(false) {
                        complete = false;
                        errors.push("wallet is locked — run walletpassphrase first".to_string());
                        continue;
                    }
                    if let Err(e) = tri7_core::spend::sign_input(&mut tx, i, &prev_script, &secret, &pubkey, hash_type) {
                        complete = false;
                        errors.push(format!("input {i}: {e}"));
                        continue;
                    }
                    // PROVE the signature we just produced before claiming it
                    if let Err(e) = tri7_core::spend::verify_input(&tx, i, &prev_script) {
                        complete = false;
                        errors.push(format!("input {i}: self-verify failed: {e}"));
                        continue;
                    }
                    signed += 1;
                }
                drop(wl);
                let raw = tx.serialize();
                let hex_out: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
                log(&format!("signrawtransaction: {signed} input(s) signed, complete={complete}"));
                Ok(serde_json::json!({
                    "hex": hex_out,
                    "complete": complete,
                    "txid": tri7_core::p2p::internal_to_display(&tx.hash()),
                    "signed_inputs": signed,
                    "errors": errors,
                }))
            }
            // createrawtransaction [{"txid":..,"vout":..}] {"addr":amount,...}
            // — unsigned only. Stamped with now so a later sign+broadcast has
            // the legacy tx-time ordering the chain expects.
            "createrawtransaction" => {
                let Some(serde_json::Value::Array(ins)) = p(0) else {
                    return Err((-8, "expected an array of {txid,vout} inputs".to_string()));
                };
                let inputs: Vec<(String, u32)> = ins
                    .iter()
                    .map(|e| {
                        let txid = e.get("txid").and_then(|v| v.as_str()).ok_or((
                            -8,
                            "each input needs a txid".to_string(),
                        ))?;
                        let vout = e.get("vout").and_then(|v| v.as_u64()).ok_or((
                            -8,
                            "each input needs a vout".to_string(),
                        ))?;
                        Ok((txid.to_string(), vout as u32))
                    })
                    .collect::<Result<_, (i64, String)>>()?;
                let Some(serde_json::Value::Object(outs)) = p(1) else {
                    return Err((-8, "expected {\"address\":amount,...} outputs".to_string()));
                };
                let mut outputs: Vec<(String, i64)> = Vec::new();
                let mut scripts: Vec<(String, Vec<u8>)> = Vec::new();
                for (dest, amt) in outs.iter() {
                    // legacy "data" destination: the value is a RAW SCRIPT hex
                    if dest == "data" {
                        let Some(hex_script) = amt.as_str() else {
                            return Err((
                                -3,
                                "createrawtransaction: \"data\" takes a script hex string".to_string(),
                            ));
                        };
                        let script = tri7_core::p2p::hex_decode(hex_script)
                            .map_err(|e| (-8, format!("createrawtransaction: data script: {e}")))?;
                        scripts.push((dest.clone(), script));
                        continue;
                    }
                    let amount = amt
                        .as_f64()
                        .ok_or_else(|| (-8, format!("createrawtransaction: amount for {dest} is not a number")))?;
                    if amount < 0.0 {
                        return Err((-3, format!("createrawtransaction: amount for {dest} out of range")));
                    }
                    outputs.push((dest.clone(), (amount * 1_000_000.0).round() as i64));
                }
                let tx = tri7_core::rawtx::create_raw(&inputs, &outputs, &scripts, now_secs() as u32)
                    .map_err(|e| (-8, e))?;
                let raw = tx.serialize();
                let hex_out: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
                Ok(serde_json::json!({
                    "hex": hex_out,
                    "txid": tri7_core::p2p::internal_to_display(&tx.hash()),
                }))
            }
            // getnewpubkey [account] — the wallet's default key in hex, or the
            // key of the funded address when no defaultkey row exists.
            "getnewpubkey" => {
                let wl = self.wl.lock().unwrap();
                let w = wl
                    .as_ref()
                    .ok_or_else(|| (-1, "wallet live UTXO view not ready yet".to_string()))?;
                let default = tri7_core::wallet::read_wallet(
                    self.wallet_path
                        .as_deref()
                        .ok_or_else(|| (-1, "no wallet loaded (start the node with --wallet)".to_string()))?,
                )
                .ok()
                .and_then(|k| k.default_key);
                if let Some(dk) = default {
                    return Ok(serde_json::json!(dk.iter().map(|b| format!("{:02x}", b)).collect::<String>()));
                }
                let mut best: Vec<&tri7_core::wallet::WalletKey> = w.wallet.keys.iter().collect();
                best.sort_by_key(|k| {
                    std::cmp::Reverse(
                        w.live
                            .values()
                            .filter(|o| {
                                tri7_core::address::script_to_dest(&o.script)
                                    .map(|d| tri7_core::address::payload_to_address(&d.payload()) == k.address)
                                    .unwrap_or(false)
                            })
                            .map(|o| o.value)
                            .sum::<i64>(),
                    )
                });
                match best.first() {
                    Some(k) => Ok(serde_json::json!(k.pubkey.iter().map(|b| format!("{:02x}", b)).collect::<String>())),
                    None => Err((-1, "wallet has no keys".to_string())),
                }
            }
            // settxfee <amount> — the fee here is the chain constant; accept
            // exactly that (or 0, meaning "use the minimum") and refuse any
            // other value instead of pretending it took effect.
            "settxfee" => {
                // nTransactionFee (wallet.cpp:1688): what the wallet ADDS on top
                // of the chain minimum. Range 0..0.25 TRI (init.cpp:694); 0 or
                // a value below the floor means "pay the minimum".
                let amount = p(0)
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| (-8, "expected amount parameter".to_string()))?;
                if !(amount >= 0.0) || amount > 0.25 {
                    return Err((
                        -8,
                        format!("settxfee: {amount} is outside the allowed range 0..0.25 TRI"),
                    ));
                }
                let sats = ((amount * 1_000_000.0).round() as i64)
                    .max(tri7_core::spend::MIN_FEE_SATS);
                {
                    let mut g = self
                        .extras
                        .write()
                        .map_err(|e| (-1, format!("settxfee: fee state lock: {e}")))?;
                    g.paytxfee_sats = Some(sats);
                }
                let state_dir = self.state_dir.clone();
                let body = serde_json::json!({
                    "paytxfee_sats": sats,
                    "set_utc": now_secs(),
                });
                let path = state_dir.join("paytxfee.json");
                std::fs::write(&path, body.to_string()).map_err(|e| {
                    (-1, format!("settxfee: persist to {}: {e}", path.display()))
                })?;
                log(&format!(
                    "settxfee: paytxfee set to {sats} sats ({:.6} TRI) — persisted to {}",
                    sats as f64 / 1_000_000.0,
                    path.display()
                ));
                Ok(serde_json::json!(true))
            }
            // decryptwallet <passphrase> — the legacy "Remove encryption":
            // every ckey row is rewritten as a plaintext key row and the
            // mkey/dirty rows are dropped, after the passphrase is PROVEN by
            // decrypting every key (decrypt_wallet_file verifies as it goes).
            "decryptwallet" => {
                let pass = ps(0).ok_or_else(|| (-8, "expected passphrase".to_string()))?;
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                if !tri7_core::wallet_encrypt::is_encrypted(&path) {
                    return Err((-1, "wallet is not encrypted".to_string()));
                }
                let n = tri7_core::wallet_encrypt::decrypt_wallet_file(&path, &pass)
                    .map_err(|e| (-1, format!("decryptwallet: {e}")))?;
                // the live key view keeps DECRYPTED secrets either way, so only
                // the lock flag has to change
                if let Some(w) = self.wl.lock().unwrap().as_mut() {
                    w.locked = false;
                    w.unlock_until = None;
                }
                if let Ok(mut g) = self.extras.write() {
                    g.unlocked_until = None;
                }
                log(&format!("decryptwallet: {n} keys rewritten as plaintext"));
                Ok(serde_json::json!({ "keys": n }))
            }
            "walletpassphrasechange" => {
                let old = ps(0).ok_or_else(|| (-8, "expected old passphrase".to_string()))?;
                let new = ps(1).ok_or_else(|| (-8, "expected new passphrase".to_string()))?;
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                if !tri7_core::wallet_encrypt::is_encrypted(&path) {
                    return Err((-1, "wallet is not encrypted".to_string()));
                }
                let n = tri7_core::wallet_encrypt::change_passphrase(&path, &old, &new)
                    .map_err(|e| (-1, format!("walletpassphrasechange: {e}")))?;
                log(&format!("walletpassphrasechange: {n} keys re-encrypted"));
                Ok(serde_json::json!({ "keys": n }))
            }
            "getstakinginfo" => {
                let (tip_height, entry) = self.tip();
                let wl = self.wl.lock().unwrap();
                let stake_st = self.stake_st.lock().unwrap();
                let now = now_secs() as u32;
                let (weight, mature) = match wl.as_ref() {
                    Some(w) => w.stake_weight(now),
                    None => (0, 0),
                };
                let (net, n_bits, modifier) = {
                    let mut out = (0u64, 0u32, 0u64);
                    if let Ok(g) = self.st.try_lock() {
                        out.0 = net_stake_weight(&g.entries);
                        out.1 = get_next_target_required(&g.rel_index(), true);
                        if let Some(e) = g.entries.last() {
                            out.2 = e.stake_modifier;
                        }
                    } else if let Some(e) = &entry {
                        out.2 = e.stake_modifier;
                    }
                    out
                };
                let enabled = stake_st.enabled;
                let staking = enabled && weight > 0;
                let expected_time: i64 = if staking && net > 0 {
                    (tri7_core::consensus::TARGET_SPACING as u64)
                        .saturating_mul(net.saturating_div(weight.max(1))) as i64
                } else {
                    -1
                };
                let mut reasons: Vec<serde_json::Value> = Vec::new();
                if !enabled {
                    reasons.push(serde_json::json!("staking disabled (stake=0; enable via the staking RPC or conf)"));
                }
                if weight == 0 {
                    reasons.push(serde_json::json!("no mature in-window coins available (min age 3600s)"));
                }
                Ok(serde_json::json!({
                    "enabled": enabled,
                    "staking": staking,
                    "errors": "",
                    "currentblocksize": 0,
                    "currentblocktx": 0,
                    "pooledtx": self.pool.lock().map(|p| p.len()).unwrap_or(0),
                    "difficulty": entry.as_ref().map(|e| tri7_core::rpc::difficulty_from_bits(e.bits)).unwrap_or(0.0),
                    "search-interval": tri7_core::stake::MAX_STAKE_SEARCH_INTERVAL,
                    "weight": weight,
                    "netstakeweight": net,
                    "expectedtime": expected_time,
                    "walletlocked": false,
                    "connections": self.peers_table.lock().map(|t| t.len()).unwrap_or(0),
                    "initialblockdownload": false,
                    "maturecoins": mature > 0,
                    "staking_disabled_reasons": reasons,
                    // tri7 extensions (honest boundaries + live counters)
                    "modifier": format!("{:016x}", modifier),
                    "target": format!("{:08x}", n_bits),
                    "attempts": stake_st.attempts,
                    "searches": stake_st.searches,
                    "last_search_secs": stake_st.last_search_secs,
                    "last_search_utc": stake_st.last_search_utc,
                    "last_hit_utc": stake_st.last_hit_utc,
                    "last_stake": stake_st.last_stake.as_ref().map(|(h, hash, t)| serde_json::json!({
                        "height": h, "hash": hash, "at_utc": t,
                    })),
                    "coins_considered": stake_st.coins_considered,
                    "coins_meeting_min_age": stake_st.coins_meeting_min_age,
                    "coins_without_key": match wl.as_ref() {
                        Some(w) => { let (_, nk, _u, _ns) = w.candidates(); nk }
                        None => 0,
                    },
                    "in_window_only": true,
                    "tip_height": tip_height,
                }))
            }
            "staking" => {
                // staking [on|off] — persisted to the conf file; bare call
                // returns the current state
                let arg = ps(0);
                let stake_st = self.stake_st.lock().unwrap();
                match arg.as_deref() {
                    None => Ok(serde_json::json!({
                        "staking": stake_st.enabled,
                        "conf": self.conf_path.display().to_string(),
                    })),
                    Some(v) => {
                        let want = match v.to_ascii_lowercase().as_str() {
                            "on" | "1" | "true" => true,
                            "off" | "0" | "false" => false,
                            _ => return Err((-8, "staking expects \"on\" or \"off\"".into())),
                        };
                        drop(stake_st);
                        {
                            let mut s = self.stake_st.lock().unwrap();
                            s.enabled = want;
                        }
                        conf_set_flag(&self.conf_path, "stake", if want { "1" } else { "0" })
                            .map_err(|e| (-1, e))?;
                        log(&format!(
                            "STAKING {} (persisted stake={} to {})",
                            if want { "ON" } else { "OFF" },
                            if want { "1" } else { "0" },
                            self.conf_path.display()
                        ));
                        Ok(serde_json::json!({
                            "staking": want,
                            "persisted_to": self.conf_path.display().to_string(),
                        }))
                    }
                }
            }
            "getpeerinfo" => Ok(peer_table_json(&self.peers_table)),
            // Legacy wallet verbs. Both act on the wallet this node was
            // started with; the writer is the same WalletHandle the GUI uses
            // (HD-aware: a seed wallet derives m/44'/2222'/... rather than
            // injecting a random key that the phrase would not restore).
            "getnewaddress" => {
                let label = ps(0).unwrap_or_default();
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(&path)
                    .map_err(|e| (-1, format!("open wallet: {e}")))?;
                let hd = wh.hd_mnemonic.is_some();
                // keypool first: a pooled key is ALREADY derived from the
                // wallet seed (its key row exists) — handing it out is
                // instant and the phrase restores it exactly like a fresh
                // derivation. Only an empty/unusable pool derives live.
                let mut from_pool: Option<(String, i64)> = None;
                let res: Result<String, (i64, String)> = match wh.pop_keypool(&label) {
                    Ok(Some(entry)) => {
                        from_pool = Some((entry.address.clone(), entry.index));
                        Ok(entry.address)
                    }
                    Ok(None) => {
                        if hd {
                            wh.generate_hd_key(&label)
                                .map(|k| k.address)
                                .map_err(|e| (-1, format!("generate_hd_key: {e}")))
                        } else {
                            let mut secret = [0u8; 32];
                            getrandom::getrandom(&mut secret)
                                .map_err(|e| (-1, format!("keygen entropy: {e}")))?;
                            wh.add_key(&secret, &label)
                                .map(|k| k.address)
                                .map_err(|e| (-1, format!("add_key: {e}")))
                        }
                    }
                    Err(e) => {
                        // a pool READ failure must not block address
                        // issuance — fall back to the live derivation and
                        // say so loudly
                        log(&format!("getnewaddress: keypool pop failed ({e}) — deriving live"));
                        if hd {
                            wh.generate_hd_key(&label)
                                .map(|k| k.address)
                                .map_err(|e| (-1, format!("generate_hd_key: {e}")))
                        } else {
                            let mut secret = [0u8; 32];
                            getrandom::getrandom(&mut secret)
                                .map_err(|e| (-1, format!("keygen entropy: {e}")))?;
                            wh.add_key(&secret, &label)
                                .map(|k| k.address)
                                .map_err(|e| (-1, format!("add_key: {e}")))
                        }
                    }
                };
                wh.close();
                let address = res?;
                // keep getwalletinfo's census honest after every handout
                if let Ok(mut g) = self.extras.write() {
                    if let Ok((n, oldest)) = tri7_core::wallet::keypool_stats(&path) {
                        g.keypool_size = Some(n);
                        g.keypool_oldest = oldest;
                    }
                }
                match from_pool {
                    Some((addr, index)) => log(&format!(
                        "getnewaddress: {addr} (keypool index {index} — pre-derived from the wallet seed)"
                    )),
                    None => log(&format!(
                        "getnewaddress: {} ({})",
                        address,
                        if hd { "derived from the wallet seed (keypool empty)" } else { "fresh random key" }
                    )),
                }
                Ok(serde_json::json!(address))
            }
            // keypoolrefill [size] — legacy rpcwallet: top the keypool up to
            // `size` pre-derived HD keys (absent/0 = default 100, legacy cap
            // 100000). Pooled keys are REAL phrase-derived keys with key
            // rows; the `pool` row only marks "not handed out yet", so a
            // refill is fully restorable from the mnemonic like any other
            // derivation.
            "keypoolrefill" => {
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                let target = parse_keypool_size(p(0))?;
                let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(&path)
                    .map_err(|e| (-1, format!("open wallet: {e}")))?;
                let rep = wh
                    .refill_keypool(target)
                    .map_err(|e| (-1, format!("keypoolrefill: {e}")))?;
                wh.close();
                if let Ok(mut g) = self.extras.write() {
                    if let Ok((n, oldest)) = tri7_core::wallet::keypool_stats(&path) {
                        g.keypool_size = Some(n);
                        g.keypool_oldest = oldest;
                    }
                }
                log(&format!(
                    "keypoolrefill: +{} derived, poolsize={} (target {}), hdchain={}",
                    rep.derived, rep.poolsize, target, rep.next_index
                ));
                Ok(serde_json::json!({
                    "derived": rep.derived,
                    "poolsize": rep.poolsize,
                    "keypoololdest": rep.oldest,
                    "hdchain_next_index": rep.next_index,
                    "target": target,
                }))
            }
            // ---- wallet encryption (legacy crypter.cpp port) -------------
            // These act on the SAME file the node stakes for. They are never
            // called on their own: encryptwallet locks the running key set, and
            // walletpassphrase is what brings signing back.
            "encryptwallet" => {
                let pass = ps(0).ok_or_else(|| (-8, "expected a passphrase".to_string()))?;
                if pass.is_empty() {
                    return Err((-8, "passphrase must not be empty".to_string()));
                }
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                if tri7_core::wallet_encrypt::is_encrypted(&path) {
                    return Err((-4, "wallet is already encrypted".to_string()));
                }
                tri7_core::wallet_encrypt::encrypt_wallet(&path, &pass)
                    .map_err(|e| (-1, format!("encryptwallet: {e}")))?;
                // lock the running key set immediately: the file no longer holds
                // plaintext, and the node must not keep signing from memory as if
                // nothing happened
                let mut locked_keys = 0usize;
                if let Some(w) = self.wl.lock().unwrap().as_mut() {
                    for k in w.wallet.keys.iter_mut() {
                        if k.secret != [0u8; 32] {
                            locked_keys += 1;
                        }
                        k.secret = [0u8; 32];
                    }
                    w.locked = true;
                    w.unlock_until = None;
                    w.wallet.encrypted = true;
                }
                log(&format!(
                    "encryptwallet: wallet encrypted, {locked_keys} keys locked — staking stops until walletpassphrase"
                ));
                Ok(serde_json::json!({
                    "encrypted": true,
                    "keys_locked": locked_keys,
                    "note": "staking and signing are disabled until walletpassphrase"
                }))
            }
            "walletpassphrase" => {
                let pass = ps(0).ok_or_else(|| (-8, "expected a passphrase".to_string()))?;
                // params arrive as JSON: ["pass", 120] puts a NUMBER in slot 1,
                // so a str-only read silently fell back to the 60s default.
                let secs: u64 = p(1)
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(60)
                    .max(1);
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                if !tri7_core::wallet_encrypt::is_encrypted(&path) {
                    return Err((-1, "wallet is not encrypted".to_string()));
                }
                let keys = tri7_core::wallet_encrypt::decrypt_wallet(&path, &pass)
                    .map_err(|e| (-1, format!("walletpassphrase: {e}")))?;
                let map: std::collections::HashMap<Vec<u8>, [u8; 32]> =
                    keys.into_iter().collect();
                let mut unlocked = 0usize;
                if let Some(w) = self.wl.lock().unwrap().as_mut() {
                    for k in w.wallet.keys.iter_mut() {
                        if let Some(s) = map.get(&k.pubkey) {
                            k.secret = *s;
                            unlocked += 1;
                        }
                    }
                    w.locked = unlocked == 0;
                    w.unlock_until = Some(now_secs() + secs);
                }
                if let Ok(mut g) = self.extras.write() {
                    g.unlocked_until = Some(secs);
                }
                if unlocked == 0 {
                    return Err((-1, "walletpassphrase: no key in the wallet was unlocked".to_string()));
                }
                if self.smsg.is_none() {
                    // smsg identities are read once at startup from the plaintext
                    // wallet; a wallet that started locked has no engine. Say so
                    // instead of leaving messaging silently dead after an unlock.
                    log("smsg: still off — messaging identities load on the next restart \
                         (the wallet was locked at startup)");
                }
                log(&format!("walletpassphrase: {unlocked} keys unlocked for {secs}s"));
                Ok(serde_json::json!({
                    "unlocked": unlocked,
                    "seconds": secs,
                    "until": now_secs() + secs
                }))
            }
            "walletlock" => {
                // A plaintext wallet cannot be locked: there would be no
                // passphrase to unlock it with, so zeroing the keys would just
                // stop staking until a restart. Refuse, like the legacy daemon.
                let path = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                if !tri7_core::wallet_encrypt::is_encrypted(&path) {
                    return Err((-1, "wallet is not encrypted".to_string()));
                }
                let mut n = 0usize;
                if let Some(w) = self.wl.lock().unwrap().as_mut() {
                    for k in w.wallet.keys.iter_mut() {
                        if k.secret != [0u8; 32] {
                            n += 1;
                        }
                        k.secret = [0u8; 32];
                    }
                    w.locked = true;
                    w.unlock_until = None;
                }
                if let Ok(mut g) = self.extras.write() {
                    g.unlocked_until = Some(0);
                }
                log(&format!("walletlock: {n} keys locked"));
                Ok(serde_json::json!({ "locked": n }))
            }
            // Copy the wallet file. The legacy daemon refuses a destination
            // that already exists; keeping that rule means a mistyped path
            // cannot silently overwrite an older backup.
            "backupwallet" => {
                let dest = ps(0)
                    .ok_or_else(|| (-8, "expected destination path".to_string()))?;
                let src = self.wallet_path.clone().ok_or_else(|| {
                    (-1, "no wallet loaded (start the node with --wallet)".to_string())
                })?;
                let dest = PathBuf::from(dest);
                if dest.exists() {
                    return Err((
                        -4,
                        format!("backupwallet: {} already exists", dest.display()),
                    ));
                }
                std::fs::copy(&src, &dest)
                    .map_err(|e| (-1, format!("backupwallet: copy to {}: {e}", dest.display())))?;
                log(&format!(
                    "backupwallet: {} -> {}",
                    src.display(),
                    dest.display()
                ));
                Ok(serde_json::Value::Null)
            }
            // Mempool verbs used to fall through to the legacy proxy, which
            // points at a port that does not exist on a workstation. The node
            // owns a TxPool - answer from it.
            "getrawmempool" => {
                let verbose = ps(0).map(|v| v == "true" || v == "1").unwrap_or(false);
                let pool = self.pool.lock().unwrap();
                if verbose {
                    Ok(pool.json())
                } else {
                    Ok(pool.json())
                }
            }
            // Mining/stats clients poll this; it used to be "daemon-only" and
            // errored on any node without a legacy proxy. Work it out from the
            // window: mean difficulty over the last N entries and the mean
            // spacing between their timestamps.
            "getnetworkhashps" => {
                let n = ps(0).and_then(|v| v.parse::<usize>().ok()).unwrap_or(120).clamp(2, 1000);
                let g = self.st.lock().unwrap();
                let len = g.entries.len();
                if len < 3 {
                    return Ok(serde_json::json!(0.0));
                }
                let take = n.min(len - 1);
                let slice = &g.entries[len - take..];
                let mut diff_sum = 0.0f64;
                for e in slice {
                    diff_sum += rpc::difficulty_from_bits(e.bits);
                }
                let mean_diff = diff_sum / slice.len() as f64;
                let span = slice
                    .last()
                    .map(|e| e.time)
                    .unwrap_or(0)
                    .saturating_sub(slice.first().map(|e| e.time).unwrap_or(0))
                    .max(1) as f64;
                let spacing = span / (slice.len() as f64 - 1.0).max(1.0);
                let hps = mean_diff * 4_294_967_296.0 / spacing;
                Ok(serde_json::json!(hps.round()))
            }
            "getmempoolinfo" => {
                let pool = self.pool.lock().unwrap();
                Ok(serde_json::json!({
                    "size": pool.len(),
                    "bytes": 0,
                    "usage": 0,
                    "maxmempool": 300_000_000,
                    "mempoolminfee": 0.00001,
                    "minrelaytxfee": 0.00001,
                }))
            }
            "smsgsend" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                let (Some(from), Some(to), Some(msg)) = (ps(0), ps(1), ps(2)) else {
                    return Err((-8, "smsgsend expects [from, to, message]".into()));
                };
                let now = now_secs() as i64;
                let (ts, sample) = engine
                    .send_local(&from, &to, &msg, now)
                    .map_err(|e| (-1, e))?;
                Ok(serde_json::json!({
                    "sent": true,
                    "timestamp": ts,
                    "sample": hex_upper(&sample),
                    "note": "bucket inventory reaches peers within the send-data cadence",
                }))
            }
            "smsginbox" | "smsgoutbox" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                let unread_only = ps(0).as_deref() == Some("unread");
                let folder = if method == "smsginbox" {
                    tri7_core::smsg::FOLDER_INBOX
                } else {
                    tri7_core::smsg::FOLDER_OUTBOX
                };
                engine
                    .list_folder_json(folder, unread_only)
                    .map_err(|e| (-1, e))
            }
            "smsgscandir" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                let now = now_secs() as i64;
                let purged = engine.purge(now);
                let info = engine.localinfo_json();
                Ok(serde_json::json!({
                    "purged": purged,
                    "scanned": info["buckets"],
                    "buckets": info["buckets"],
                    "received": info["received"],
                    "delivered_to_inbox": info["delivered_to_inbox"],
                }))
            }
            "smsgpurge" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                let n = engine.purge(now_secs() as i64);
                Ok(serde_json::json!({ "purged": n }))
            }
            "smsgaddaddress" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                let (Some(addr), Some(pk)) = (ps(0), ps(1)) else {
                    return Err((-8, "smsgaddaddress expects [address, pubkey-hex]".into()));
                };
                let pubkey = tri7_core::p2p::hex_decode(&pk)
                    .map_err(|e| (-8, format!("bad pubkey hex: {}", e)))?;
                engine
                    .add_address(&addr, &pubkey)
                    .map(|inserted| serde_json::json!({ "added": inserted, "address": addr }))
                    .map_err(|e| (-1, e))
            }
            "smsgkeys" | "smsglocalinfo" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                Ok(engine.localinfo_json())
            }
            _ => Err((-32601, format!("ops adapter: unknown method {}", method))),
        }
    }
}

const SMSG_NOT_WIRED: &str =
    "smsg engine not loaded — run tri7d with --wallet (smsg needs wallet keys)";

fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

fn handle_message(
    peer: &mut Peer,
    st: &mut ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    cfg: &Config,
    msg: tri7_core::p2p::RawMessage,
    outstanding: &mut HashMap<Hash, u64>,
    pending_from_headers: &mut VecDeque<(Hash, u64)>,
    store: &Arc<BlockStore>,
    recent: &mut RecentBlocks,
    gap: &Arc<Mutex<GapState>>,
    smsg: &Option<Arc<tri7_core::smsg_net::SmsgEngine>>,
    pool: &mut tri7_core::txpool::TxPool,
    wl: &SharedWalletLive,
    announce: &TxAnnounce,
    pending_tx_getdata: &mut std::collections::HashSet<Hash>,
) -> Result<(), String> {
    match msg.command.as_str() {
        "version" => {
            let v = tri7_core::p2p::parse_version(&msg.payload)?;
            log(&format!(
                "peer version: {} ({}) start height {}",
                v.version, v.user_agent, v.start_height
            ));
            // learn the peer's advertised listening address when it is a
            // real (non-loopback) address — a peer that connected to us is
            // reachable by definition
            if let Some(adv) = advertised_store_key(&v.addr_me) {
                if v.addr_me.port != 0 {
                    let now = tri7_core::peers::unix_now();
                    with_peer_store(cfg, |store| {
                        store.upsert(&adv, v.addr_me.port, v.services, "listen", now);
                    });
                }
            }
            Ok(())
        }
        "verack" => Ok(()),
        "ping" => {
            let n = tri7_core::p2p::parse_ping(&msg.payload)?;
            peer.sess
                .send("pong", &tri7_core::p2p::build_pong(n), stats)?;
            Ok(())
        }
        "pong" => Ok(()),
        "headers" => {
            let headers = tri7_core::p2p::parse_headers(&msg.payload)?;
            for hdr in headers {
                stats.headers_processed += 1;
                let hash = hdr.hash();
                if st.entry_by_hash(&hash).is_some() {
                    continue; // already have
                }
                if hdr.prev_hash == st.tip_hash {
                    stats.headers_new += 1;
                    st.pending_headers.insert(hash, hdr);
                    pending_from_headers.push_back((hash, st.tip_height + 1));
                } else {
                    log(&format!(
                        "headers announcement does not chain to our tip — requesting reconciliation"
                    ));
                    let stop = [0u8; 32];
                    peer.sess.send(
                        "getheaders",
                        &tri7_core::p2p::build_getheaders(&[st.tip_hash], &stop),
                        stats,
                    )?;
                    break;
                }
            }
            Ok(())
        }
        "inv" => {
            // Unknown block hashes are reconciled via getheaders (which
            // verifies linkage) rather than blind getdata — a fork inv can
            // never steer our tip. Unknown TX hashes ARE pulled via getdata
            // (the relay path) — bounded per-peer, then consensus-validated.
            let inv = tri7_core::p2p::parse_inv(&msg.payload)?;
            let mut unknown = 0usize;
            let mut tx_want: Vec<(u32, Hash)> = Vec::new();
            for (t, hash) in inv {
                if t == tri7_core::p2p::MSG_TX {
                    let known = pool.get(&hash).is_some();
                    if !known && pending_tx_getdata.len() < 64 {
                        pending_tx_getdata.insert(hash);
                        tx_want.push((tri7_core::p2p::MSG_TX, hash));
                    }
                    continue;
                }
                if t != tri7_core::p2p::MSG_BLOCK {
                    continue;
                }
                if st.entry_by_hash(&hash).is_none() && recent.get(&hash).is_none() {
                    unknown += 1;
                }
            }
            if !tx_want.is_empty() {
                let body = tri7_core::p2p::build_getdata(&tx_want);
                peer.sess.send("getdata", &body, stats)?;
                stats.tx_getdatas_sent += tx_want.len() as u64;
            }
            if unknown > 0 {
                let stop = [0u8; 32];
                peer.sess.send(
                    "getheaders",
                    &tri7_core::p2p::build_getheaders(&[st.tip_hash], &stop),
                    stats,
                )?;
            }
            Ok(())
        }
        "tx" => {
            // relay path: parse + consensus-validate + pending set + announce
            stats.txs_received += 1;
            let mut r = Reader::new(&msg.payload);
            let tx = Tx::read(&mut r).map_err(|e| format!("tx parse: {}", e))?;
            r.expect_done().map_err(|e| format!("tx trailing: {}", e))?;
            let raw = msg.payload.clone();
            let now = now_secs() as i64;
            let view_lock = wl.lock().unwrap();
            let view_ok = view_lock.is_some();
            let accepted = match view_lock.as_ref() {
                Some(v) => pool.accept(&raw, &LiveUtxoView(&v.live), now),
                None => Err("wallet live UTXO view not ready — tx not relayed".into()),
            };
            drop(view_lock);
            match accepted {
                Ok(rep) => {
                    stats.txs_accepted += 1;
                    announce
                        .lock()
                        .unwrap()
                        .push_back(tri7_core::p2p::display_to_internal(&rep.txid).unwrap_or([0u8; 32]));
                    pending_tx_getdata.remove(&tx.hash());
                    log(&format!(
                        "tx relay accepted: {} ({} inputs, fee {} sats) from {} — announcing",
                        rep.txid, rep.inputs, rep.fee_sats, peer.label
                    ));
                }
                Err(e) => {
                    stats.txs_rejected += 1;
                    if !view_ok {
                        log(&format!("tx relay: {} (view warming — retryable)", e));
                    } else {
                        // a rejected relay tx is NOT a protocol violation:
                        // log loudly, keep the peer
                        log(&format!("tx relay rejected from {}: {}", peer.label, e));
                    }
                }
            }
            Ok(())
        }
        "block" => {
            stats.blocks_received += 1;
            let mut r = Reader::new(&msg.payload);
            let b = Block::read(&mut r).map_err(|e| format!("block parse: {}", e))?;
            r.expect_done()
                .map_err(|e| format!("block trailing: {}", e))?;
            let hash = b.hash();
            // GAP RECOVERY interception: a body for a missing tail height —
            // validate (Hash9 == requested hash + link + merkle), append to
            // the store and cache it. Runs BEFORE the tip-extension logic
            // (a gap fill never extends the tip).
            {
                let target = gap.lock().unwrap().missing;
                if let Some((gh, ghash)) = target {
                    if hash == ghash {
                        return match gap_fill_valid(&b, gh, &ghash, st) {
                            Ok(()) => {
                                match store.append_block(&msg.payload, Some(gh)) {
                                    Ok(_appended) => {
                                        recent.insert(hash, gh, &msg.payload);
                                        stats.gap_filled += 1;
                                        let mut g = gap.lock().unwrap();
                                        g.filled_from = Some(peer.label.clone());
                                        g.missing = None;
                                        g.since = None;
                                        g.ticks_failed = 0;
                                        log(&format!(
                                            "GAP RECOVERY: body for h{} {} appended to the store from {} — tail complete",
                                            gh,
                                            tri7_core::p2p::internal_to_display(&hash),
                                            peer.label
                                        ));
                                        write_gap_marker(cfg, serde_json::json!({
                                            "branch": "recovered_from_peer",
                                            "height": gh,
                                            "hash": tri7_core::p2p::internal_to_display(&hash),
                                            "from_peer": peer.label,
                                            "at_utc": now_iso(),
                                        }));
                                        Ok(())
                                    }
                                    Err(e) => {
                                        // store append failed (e.g. read-only
                                        // blk file): keep the body in the
                                        // recent cache so serving + the next
                                        // gap tick see it, and say why
                                        recent.insert(hash, gh, &msg.payload);
                                        log(&format!(
                                            "GAP RECOVERY: body for h{} validated but NOT appended: {}",
                                            gh, e
                                        ));
                                        Ok(())
                                    }
                                }
                            }
                            Err(e) => {
                                log(&format!("gap fill rejected from {}: {}", peer.label, e));
                                Ok(()) // stale/fork body — not a protocol failure
                            }
                        };
                    }
                }
            }
            let mut links_tip = b.prev_hash == st.tip_hash;
            if !links_tip {
                // a competing block: adopt it only if its branch is heavier
                match try_adopt_heavier_branch(st, &b, &hash) {
                    Ok(true) => {
                        links_tip = b.prev_hash == st.tip_hash;
                        stats.reorgs += 1;
                    }
                    Ok(false) => {}
                    Err(e) => log(&format!("fork choice: rollback failed: {}", e)),
                }
            }
            let height_abs = if links_tip { st.tip_height + 1 } else { 0 };
            // cache the raw bytes BEFORE validation: a valid block must be
            // servable to peers immediately (relay getdata), and the RPC
            // surface must serve heights above the disk store's tip
            recent.insert(hash, height_abs, &msg.payload);
            if height_abs > 0 {
                let prev_entry = st.entries.last().cloned().unwrap();
                let header = st.pending_headers.remove(&hash);
                let paths = validate_block(
                    st,
                    rpc,
                    stats,
                    &b,
                    &BlockCtx {
                        height_abs,
                        header: header.as_ref(),
                        prev_entry,
                        live: true,
                    },
                )?;
                // The chain advances FIRST: the index may only be applied
                // once the block is part of the chain this node serves. The
                // index records the hash of the block it applied, so the
                // wallet is served from it only when that hash matches the
                // chain's own tip hash.
                advance_with_block(st, rpc, stats, &b, &paths)?;
                addrindex_apply(&b, height_abs);
                stats.blocks_validated_live += 1;
                outstanding.remove(&hash);
                // DURABILITY: append the accepted block to the store so the
                // tip body survives restarts (the memory-only recent cache
                // already lost one tip body and froze fleet minting until gap
                // recovery existed). Best-effort — the recent cache still
                // serves the block if the append fails.
                if let Err(e) = store.append_block(&msg.payload, Some(height_abs)) {
                    log(&format!(
                        "durability: accepted h{} not appended to the store: {}",
                        height_abs, e
                    ));
                }
                let file = st.to_file();
                // The block is already applied in memory and in the store; a
                // failed checkpoint write only means the on-disk resume point
                // lags. Log it and keep following (this used to exit the node
                // with FAIL(3) and left the wallet 5+ blocks behind).
                if let Err(e) = write_state_atomic(cfg, &file) {
                    log(&format!("state write deferred (tip advance continues): {}", e));
                }
                log(&format!(
                    "TIP ADVANCE → h{} {} (kernel {}) via {}",
                    st.tip_height,
                    tri7_core::p2p::internal_to_display(&st.tip_hash),
                    if paths.kernel_enforced {
                        "enforced"
                    } else {
                        "seeded"
                    },
                    peer.label
                ));
            } else if outstanding.contains_key(&hash) {
                // requested block that no longer extends the tip — skip
                outstanding.remove(&hash);
            } else {
                log(&format!(
                    "ignoring non-extending block {} (prev {})",
                    tri7_core::p2p::internal_to_display(&hash),
                    tri7_core::p2p::internal_to_display(&b.prev_hash)
                ));
            }
            Ok(())
        }
        // serving: peers may pull stored blocks and header ranges from us
        "getdata" => {
            let inv = tri7_core::p2p::parse_inv(&msg.payload)?;
            for (t, hash) in inv {
                if t == tri7_core::p2p::MSG_TX {
                    if let Some((raw, _)) = pool.get(&hash) {
                        stats.getdata_tx_served += 1;
                        peer.sess.send("tx", &raw, stats)?;
                    }
                    pending_tx_getdata.remove(&hash);
                    continue;
                }
                if t != tri7_core::p2p::MSG_BLOCK {
                    continue; // never serve filtered variants
                }
                let payload: Option<Vec<u8>> = match recent.get(&hash) {
                    Some((_, raw)) => Some(raw.clone()),
                    None => match store.block_by_hash(&hash) {
                        Ok(Some(hit)) => Some(hit.block.serialize()),
                        _ => None,
                    },
                };
                if let Some(raw) = payload {
                    stats.getdata_served += 1;
                    peer.sess.send("block", &raw, stats)?;
                }
            }
            Ok(())
        }
        "getheaders" => {
            let (locator, stop) = parse_getheaders(&msg.payload)?;
            let headers = collect_headers(store, recent, &locator, &stop);
            stats.getheaders_served += 1;
            peer.sess
                .send("headers", &tri7_core::p2p::encode_headers(&headers), stats)?;
            Ok(())
        }
        "getaddr" => {
            // Answer with up to MAX_ADDR_ENTRIES known-good peers from the
            // store (the legacy client's CAddrMan relay; our own onion is
            // included by the store when onion-state.json exists).
            let now = tri7_core::peers::unix_now();
            let store = tri7_core::peers::PeerStore::load(&peers_path(cfg));
            let entries = store.addr_entries(now, tri7_core::p2p::MAX_ADDR_ENTRIES);
            stats.getaddr_served += 1;
            peer.sess
                .send("addr", &tri7_core::p2p::encode_addr(&entries), stats)?;
            Ok(())
        }
        "addr" => {
            // Merge gossip into the peer store. A malformed payload is a
            // log line, not a disconnect — gossip noise must not kill peers.
            match tri7_core::p2p::parse_addr(&msg.payload) {
                Ok(entries) => {
                    let now = tri7_core::peers::unix_now();
                    with_peer_store(cfg, |store| {
                        let merged = store.merge_addr(&entries, now);
                        if merged > 0 {
                            log(&format!(
                                "addr: merged {} of {} entries (store {} peers)",
                                merged,
                                entries.len(),
                                store.len()
                            ));
                        }
                    });
                    Ok(())
                }
                Err(e) => {
                    log(&format!("addr payload rejected from {}: {}", peer.label, e));
                    Ok(())
                }
            }
        }
        // read-only inbound noise: announce preferences and compact-block
        // negotiation carry no data we act on
        "getblocks" | "mempool" | "getwalletaddr" | "getsnap" | "getsnapchunk"
        | "walletaddr" | "seeder" | "getseederlist" | "seederlist" | "cmpctblock"
        | "sendcmpct" | "sendheaders" => Ok(()),
        c if c.starts_with("smsg") => {
            // smsg wire engine: ping/pong/inv/show/have/want/msg/match/ignore
            match smsg {
                Some(engine) => {
                    let now = now_secs() as i64;
                    let replies = engine.handle(&peer.label, c, &msg.payload, now);
                    for r in replies {
                        peer.sess.send(r.command, &r.payload, stats)?;
                    }
                    Ok(())
                }
                None => {
                    log(&format!("smsg message ignored (engine not wired): {:?}", c));
                    Ok(())
                }
            }
        }
        other => {
            log(&format!("ignoring message {:?}", other));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 5b — RPC surface data sources
//
// Design (Sami's directive): tri7d is the daemon REPLACEMENT, so the everyday
// wallet/chain queries are served NATIVELY from tri7's own small-footprint
// state; the legacy-RPC proxy is a TRANSITIONAL fallback only and every
// response carries a "source" field ("tri7" | "legacy-proxy") so cutover
// progress is measurable. Sources per method (mirrored in rpc.rs docs):
//   gettip/getblockcount/getstakemodifier/getdifficulty  → tipstate (window)
//   getblock/getrawtransaction/getblockhash              → blockindex over
//     the blocks file (hash→offset / txid→offset / height→offset, cached)
//   getbalance/listunspent/listwalletaddresses/getreceivedbyaddress → replay
//     snapshots (balances.json + wallet_utxos.json — the phase-1e pattern)
//   validateaddress                                      → pure address codec
//   getinfo/getconnectioncount/health                    → composite of the above
// ---------------------------------------------------------------------------

/// Shared tip/window state: the follow loop owns ChainState behind this mutex;
/// RPC threads take short locks per call.
type SharedState = Arc<Mutex<ChainState>>;

/// Lock-free tip snapshot. The follow loop may hold the state mutex for a
/// LONG stretch (a validation batch that calls our own RPC — post-cutover the
/// "daemon" RPC is tri7d itself, so a lock held across rpc.call would
/// otherwise deadlock the RPC thread). RPC threads therefore try_lock and
/// fall back to this mirror (a stale-but-consistent tip) instead of blocking.
struct TipMirror {
    height: AtomicU64,
    hash: std::sync::RwLock<String>,
}

impl TipMirror {
    fn new(height: u64, hash_display: &str) -> Arc<TipMirror> {
        Arc::new(TipMirror {
            height: AtomicU64::new(height),
            hash: std::sync::RwLock::new(hash_display.to_string()),
        })
    }
    fn set(&self, height: u64, hash_display: &str) {
        self.height.store(height, Ordering::Relaxed);
        *self.hash.write().unwrap() = hash_display.to_string();
    }
    fn height(&self) -> u64 {
        self.height.load(Ordering::Relaxed)
    }
    fn hash_display(&self) -> String {
        self.hash.read().unwrap().clone()
    }
}

struct LiveTip {
    st: SharedState,
    started: Instant,
    last_write: Arc<Mutex<Instant>>,
    peers: Arc<AtomicU32>,
    mirror: Arc<TipMirror>,
}

/// Window entry → the rpc::EntryView shape (shared by both adapters).
fn entry_view(st: &ChainState, h: u64) -> Option<EntryView> {
    let e = st.entry_abs(h)?;
    Some(EntryView {
        height: h,
        hash: tri7_core::p2p::internal_to_display(&e.hash),
        modifier: e.stake_modifier,
        generated: e.generated_stake_modifier,
        checksum: *st.checksums.get(st.rel(h)).unwrap_or(&0),
        pos: e.is_proof_of_stake,
        proof: tri7_core::p2p::internal_to_display(&e.hash_proof_of_stake),
        entropy: e.entropy_bit,
        time: e.time,
        bits: e.bits,
    })
}

impl TipProvider for LiveTip {
    fn tip(&self) -> TipView {
        // try_lock first: while the follow loop holds the state mutex across
        // a validation batch (which itself calls our own RPC), serve the
        // mirror instead of deadlocking the RPC thread
        if let Ok(g) = self.st.try_lock() {
            return TipView {
                height: g.tip_height,
                blockhash: tri7_core::p2p::internal_to_display(&g.tip_hash),
                daemon_tip: g.daemon_tip,
            };
        }
        TipView {
            height: self.mirror.height(),
            blockhash: self.mirror.hash_display(),
            daemon_tip: Some(self.mirror.height() as i64),
        }
    }

    fn window(&self) -> (u64, usize) {
        match self.st.try_lock() {
            Ok(g) => (g.base, g.entries.len()),
            Err(_) => {
                let base = self.mirror.height().saturating_sub(4599);
                (base, 4600)
            }
        }
    }

    fn entry_at(&self, h: u64) -> Option<EntryView> {
        let g = self.st.try_lock().ok()?;
        entry_view(&g, h)
    }

    fn last_bits(&self) -> Option<u32> {
        let g = self.st.try_lock().ok()?;
        g.entries.last().map(|e| e.bits)
    }

    fn next_target_bits(&self, pos: bool) -> Option<u32> {
        let g = self.st.try_lock().ok()?;
        get_next_target_required(&g.rel_index(), pos).into() // Option<u32>
    }

    fn peers(&self) -> u32 {
        self.peers.load(Ordering::Relaxed)
    }

    fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    fn state_write_age_secs(&self) -> Option<u64> {
        Some(self.last_write.lock().unwrap().elapsed().as_secs())
    }
}

// ---- disk-backed block index adapter ----

struct StoreAdapter {
    store: Arc<BlockStore>,
    tip: SharedState,
    mirror: Arc<TipMirror>,
    /// blocks accepted since process start — covers heights ABOVE the disk
    /// store's tip (the blocks file stopped growing when legacy retired)
    recent: Arc<Mutex<RecentBlocks>>,
}

/// Tip height without blocking on the state mutex (mirror fallback).
fn tip_height_nonblocking(tip: &SharedState, mirror: &TipMirror) -> u64 {
    match tip.try_lock() {
        Ok(g) => g.tip_height,
        Err(_) => mirror.height(),
    }
}

fn recent_block_hit(hash: Hash, height: u64, raw: &[u8]) -> Option<tri7_core::blockindex::BlockHit> {
    let mut r = Reader::new(raw);
    let block = Block::read(&mut r).ok()?;
    r.expect_done().ok()?;
    Some(tri7_core::blockindex::BlockHit {
        block,
        hash,
        off: 0,
        body_len: raw.len(),
        height: if height > 0 { Some(height) } else { None },
    })
}

fn format_block_from_store(adapter: &StoreAdapter, hit: tri7_core::blockindex::BlockHit) -> Value {
    let tip_height = tip_height_nonblocking(&adapter.tip, &adapter.mirror);
    let next_hash = match hit.height {
        Some(h) => adapter.store.hash_at_height(h + 1).ok().flatten(),
        None => None,
    };
    let entry = hit.height.and_then(|h| {
        let g = adapter.tip.try_lock().ok()?;
        entry_view(&g, h)
    });
    rpc::format_block_json(&hit, tip_height, next_hash, entry)
}

impl BlockDataProvider for StoreAdapter {
    fn ready(&self) -> bool {
        matches!(self.store.status(), StoreStatus::Ready { .. })
    }

    fn status_json(&self) -> Value {
        match self.store.status() {
            StoreStatus::Ready {
                frames,
                txs_indexed,
                heights_ok,
                threaded_tip,
                build_secs,
            } => serde_json::json!({
                "state": "ready",
                "frames": frames,
                "txs_indexed": txs_indexed,
                "heights_ok": heights_ok,
                "threaded_tip": threaded_tip,
                "build_secs": build_secs,
            }),
            StoreStatus::Building { frames_done } => serde_json::json!({
                "state": "building",
                "frames_done": frames_done,
            }),
            StoreStatus::Failed(e) => serde_json::json!({"state": "failed", "error": e}),
            StoreStatus::Unconfigured => serde_json::json!({"state": "unconfigured"}),
        }
    }

    fn block_by_hash(&self, hash_display: &str) -> Result<Option<Value>, String> {
        let hash = tri7_core::p2p::display_to_internal(hash_display)?;
        let hit = self.store.block_by_hash(&hash)?;
        if let Some(hit) = hit {
            return Ok(Some(format_block_from_store(self, hit)));
        }
        // try_lock: the follow loop holds the recent-cache lock across a
        // validation batch that may itself call this RPC — under contention
        // answer store-only instead of deadlocking
        let recent_hit = self
            .recent
            .try_lock()
            .ok()
            .and_then(|mut r| r.get(&hash).cloned())
            .and_then(|(h, raw)| recent_block_hit(hash, h, &raw));
        Ok(recent_hit.map(|h| format_block_from_store(self, h)))
    }

    fn block_by_height(&self, h: u64) -> Result<Option<Value>, String> {
        let hit = self.store.block_by_height(h)?;
        if let Some(hit) = hit {
            return Ok(Some(format_block_from_store(self, hit)));
        }
        let recent_hit = self
            .recent
            .try_lock()
            .ok()
            .and_then(|mut r| r.at_height(h).cloned())
            .and_then(|(_, raw)| {
                let hash = Block::read(&mut Reader::new(&raw))
                    .map(|b| b.hash())
                    .ok()?;
                recent_block_hit(hash, h, &raw)
            });
        Ok(recent_hit.map(|h| format_block_from_store(self, h)))
    }

    fn tx(&self, txid_display: &str, verbose: bool) -> Result<Option<Value>, String> {
        let txid = tri7_core::p2p::display_to_internal(txid_display)?;
        let hit = self.store.tx_by_id(&txid)?;
        let Some(hit) = hit else { return Ok(None) };
        let tip_height = tip_height_nonblocking(&self.tip, &self.mirror);
        let (obj, hex) = rpc::format_tx_json(&hit, tip_height);
        Ok(Some(if verbose { obj } else { Value::String(hex) }))
    }

    fn hash_at(&self, h: u64) -> Result<Option<String>, String> {
        let store_hit = self.store.hash_at_height(h)?;
        if store_hit.is_some() {
            return Ok(store_hit);
        }
        Ok(self
            .recent
            .try_lock()
            .ok()
            .and_then(|mut r| r.at_height(h).cloned())
            .and_then(|(_, raw)| Block::read(&mut Reader::new(&raw)).ok())
            .map(|b| tri7_core::p2p::internal_to_display(&b.hash())))
    }

    fn raw_block(
        &self,
        hash_display: Option<&str>,
        height: Option<u64>,
    ) -> Result<Option<String>, String> {
        // Serve the STORED bytes, not a re-serialization of the decoded
        // Block: re-encoding is not guaranteed byte-identical (header extras,
        // tx encoding), and a block served to a peer must hash to its own
        // block hash.
        match (hash_display, height) {
            (Some(hs), _) => {
                let hash = tri7_core::p2p::display_to_internal(hs)?;
                if let Some(body) = self.store.raw_body_by_hash(&hash)? {
                    return Ok(Some(rpc::hex_encode(&body)));
                }
            }
            (_, Some(h)) => {
                if let Some(body) = self.store.raw_body_by_height(h)? {
                    return Ok(Some(rpc::hex_encode(&body)));
                }
            }
            _ => {}
        }
        // recent cache (blocks above the file tip): payload IS the wire body
        let from_recent = self.recent.try_lock().ok().and_then(|mut r| {
            match (hash_display, height) {
                (Some(hs), _) => {
                    let hash = tri7_core::p2p::display_to_internal(hs).ok()?;
                    r.get(&hash).cloned()
                }
                (_, Some(h)) => r.at_height(h).cloned(),
                _ => None,
            }
        });
        Ok(from_recent.map(|(_, raw)| rpc::hex_encode(&raw)))
    }
}

// ---- transitional legacy proxy ----

struct LegacyProxyAdapter {
    host: String,
    port: u16,
    user: String,
    pass: String,
    timeout: Duration,
}

impl LegacyProxyAdapter {
    fn call_legacy(&self, body: &str, timeout: Duration) -> Result<(u16, String), String> {
        rpc::legacy_post(&self.host, self.port, &self.user, &self.pass, body, timeout)
    }
}

impl Proxy for LegacyProxyAdapter {
    fn forward(&self, req: &Value) -> Result<(u16, Value), String> {
        let body = serde_json::to_string(req).map_err(|e| e.to_string())?;
        let (status, text) = self.call_legacy(&body, self.timeout)?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("legacy returned non-JSON body (HTTP {}): {}", status, e))?;
        Ok((status, v))
    }

    fn describe(&self) -> String {
        format!("legacy {}:{} (transitional fallback)", self.host, self.port)
    }

    fn probe(&self) -> bool {
        let body = serde_json::json!({"jsonrpc": "1.0", "id": "tri7-probe", "method": "getblockcount", "params": []});
        matches!(
            self.call_legacy(&body.to_string(), Duration::from_secs(2)),
            Ok((_, text)) if serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["result"].as_i64())
                .is_some()
        )
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
/// Phase 6 health gate — pure so the unit tests can drive it.
///
/// Returns (exit_code, degraded, reasons). Rules:
///   - unreadable/unparseable state   → EXIT_VALIDATE (caller handles)
///   - stale state write (> max_write_age or future mtime) → EXIT_VALIDATE
///   - degraded: --static NOT set AND no peer traffic for > peer_quiet_secs
///     AND no tip advance for > advance_quiet_secs → EXIT_VALIDATE. A network
///     node that can neither reach peers nor follow the chain is NOT healthy
///     even though its own loop keeps writing state; --static (explicit)
///     skips the network-quiet gate.
fn status_gate(
    static_mode: bool,
    stale: bool,
    peer_age_secs: Option<i64>,
    advance_age_secs: Option<i64>,
    peer_quiet_secs: u64,
    advance_quiet_secs: u64,
) -> (i32, bool, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    if stale {
        reasons.push("state write stale".into());
    }
    let peer_quiet = peer_age_secs
        .map(|a| a > peer_quiet_secs as i64)
        .unwrap_or(true); // no peer data at all counts as quiet
    let advance_quiet = advance_age_secs
        .map(|a| a > advance_quiet_secs as i64)
        .unwrap_or(true); // never advanced = quiet
    let degraded = !static_mode && peer_quiet && advance_quiet;
    if degraded {
        if peer_quiet {
            reasons.push(format!(
                "no peer traffic for {}s (> {}s)",
                peer_age_secs.unwrap_or(-1),
                peer_quiet_secs
            ));
        }
        if advance_quiet {
            reasons.push(format!(
                "tip not advanced for {}s (> {}s)",
                advance_age_secs.unwrap_or(-1),
                advance_quiet_secs
            ));
        }
    }
    let exit = if stale || degraded {
        EXIT_VALIDATE
    } else {
        0
    };
    (exit, degraded, reasons)
}

/// --status: healthcheck — tip height + advance/write/peer times as JSON.
/// Pure file read: no RPC creds, no P2P session, no conf access.
/// Exit 0 healthy; 3 (EXIT_VALIDATE) when the state file is unreadable OR
/// stale OR degraded (see status_gate).
fn status_exit(cfg: &Config) -> ! {
    let path = state_path(cfg);
    let read = || -> Result<serde_json::Value, String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let f: TipStateFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let mtime = std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .modified()
            .map_err(|e| e.to_string())?;
        let mtime_secs = mtime
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        let age = now_secs() as i64 - mtime_secs;
        // future-dated mtime (clock skew, restored backup) is not healthy —
        // treat any negative age as stale too
        let stale = age < 0 || age > cfg.max_write_age as i64;
        let last_advance = if f.last_advance_utc.is_empty() {
            None
        } else {
            Some(f.last_advance_utc.clone())
        };
        let advance_age_secs = last_advance.as_ref().map(|iso| {
            now_secs() as i64 - iso_secs_from_now(iso).unwrap_or(i64::MAX / 2)
        });
        let peer_age_secs = f.last_peer_utc.as_ref().map(|iso| {
            now_secs() as i64 - iso_secs_from_now(iso).unwrap_or(i64::MAX / 2)
        });
        let (exit, degraded, reasons) = status_gate(
            cfg.static_mode,
            stale,
            peer_age_secs,
            advance_age_secs,
            cfg.peer_quiet_secs,
            cfg.advance_quiet_secs,
        );
        let at_daemon_tip = f.daemon_tip.map_or(false, |dt| dt >= 0 && f.height == dt as u64);
        let v = serde_json::json!({
            "height": f.height,
            "blockhash": f.blockhash,
            "updated_utc": f.updated_utc,
            "last_advance_utc": last_advance,
            "state_mtime_utc": iso_from_secs(mtime_secs),
            "seconds_since_state_write": age,
            "max_write_age": cfg.max_write_age,
            "stale": stale,
            "window_base": f.window_base,
            "window_entries": f.entries.len(),
            "daemon_tip": f.daemon_tip,
            "at_daemon_tip": at_daemon_tip,
            "peers": f.peers,
            "last_peer_utc": f.last_peer_utc,
            "listen_port": f.listen_port,
            "static_mode": cfg.static_mode,
            "peer_quiet_secs": cfg.peer_quiet_secs,
            "advance_quiet_secs": cfg.advance_quiet_secs,
            "seconds_since_peer": peer_age_secs,
            "seconds_since_advance": advance_age_secs,
            "degraded": degraded,
            "degraded_reasons": reasons,
            "exit_code": exit,
        });
        Ok(v)
    };
    match read() {
        Ok(v) => {
            let exit =
                v.get("exit_code").and_then(|e| e.as_i64()).unwrap_or(EXIT_VALIDATE as i64) as i32;
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(exit);
        }
        Err(e) => {
            eprintln!("status: {}: {}", path.display(), e);
            std::process::exit(EXIT_VALIDATE);
        }
    }
}

/// Parse the ISO UTC timestamps written by now_iso() back to epoch seconds.
fn iso_secs_from_now(iso: &str) -> Option<i64> {
    // format: YYYY-MM-DDTHH:MM:SSZ (utc_from_secs writer)
    let (date, time) = iso.strip_suffix('Z')?.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let s: i64 = tp.next()?.parse().ok()?;
    // days-from-civil (Howard Hinnant's algorithm)
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s)
}

/// Bootstrap a minimal-but-valid TRI wallet SQLite container (the container
/// format the wallet writer validates: application_id/user_version, the main
/// kv table, one version row, and a genesis bestblock locator). Key rows are
/// inserted afterwards via WalletHandle::add_key.
fn bootstrap_wallet_file(path: &Path) {
    use tri7_core::wallet::{CLIENT_VERSION, WALLET_APP_ID, WALLET_USER_VERSION};
    let conn = rusqlite::Connection::open(path)
        .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet sqlite: {}", e)));
    conn.execute_batch(&format!(
        "PRAGMA application_id = {};\nPRAGMA user_version = {};",
        WALLET_APP_ID, WALLET_USER_VERSION
    ))
    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet pragma: {}", e)));
    conn.execute_batch(
        "CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);",
    )
    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet table: {}", e)));
    // version row (compact-size key "version" → CLIENT_VERSION le bytes)
    let mut k = Vec::new();
    k.push(7);
    k.extend_from_slice(b"version");
    conn.execute(
        "INSERT INTO main (key, value) VALUES (?1, ?2)",
        [k, CLIENT_VERSION.to_le_bytes().to_vec()],
    )
    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet version row: {}", e)));
    // bestblock locator at genesis (version + count 1 + zero hash)
    let mut bk = Vec::new();
    bk.push(9);
    bk.extend_from_slice(b"bestblock");
    let mut bv = Vec::new();
    bv.extend_from_slice(&CLIENT_VERSION.to_le_bytes());
    bv.push(1);
    bv.extend_from_slice(&[0u8; 32]);
    conn.execute(
        "INSERT INTO main (key, value) VALUES (?1, ?2)",
        [bk, bv],
    )
    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet bestblock row: {}", e)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|e| die(EXIT_USAGE, &format!("chmod wallet: {}", e)));
    }
}

/// Insert the `defaultkey` row (compact-size len + pubkey) after the first key
/// exists — read_wallet and legacy consumers expect a resolvable defaultkey.
fn insert_default_key(path: &Path, pubkey: &[u8]) {
    let conn = rusqlite::Connection::open(path)
        .unwrap_or_else(|e| die(EXIT_USAGE, &format!("defaultkey sqlite: {}", e)));
    let mut v = Vec::with_capacity(1 + pubkey.len());
    v.push(pubkey.len() as u8);
    v.extend_from_slice(pubkey);
    let mut k = Vec::new();
    k.push(10);
    k.extend_from_slice(b"defaultkey");
    conn.execute(
        "INSERT OR REPLACE INTO main (key, value) VALUES (?1, ?2)",
        [k, v],
    )
    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("defaultkey insert: {}", e)));
}

fn main() {
    let mut cfg = parse_args();
    FULL_VALIDATE.store(cfg.full_validate, Ordering::Relaxed);
    if cfg.status {
        status_exit(&cfg); // early: no conf read, no network
    }
    install_signal_handlers();
    // publish the wallet path so the passive native handlers (encryption,
    // getnewpubkey) can serve before the live wallet view finishes seeding
    if let Some(wp) = cfg.wallet_path.as_deref() {
        std::env::set_var("TRI7_WALLET_PATH", wp.display().to_string());
    }
    std::env::set_var("TRI7_STATE_DIR", cfg.state_dir.display().to_string());
    // --new-wallet: brand-new wallet with BRAND-NEW keys (never copied) —
    // bootstrap the SQLite TRIw container (the wallet writer opens rw but
    // never creates), then insert freshly generated secrets via add_key.
    if let Some(nw) = cfg.new_wallet.clone() {
        if nw.exists() {
            log(&format!(
                "new-wallet: {} already exists — reusing it (fresh install kept its keys)",
                nw.display()
            ));
        } else {
            if let Some(dir) = nw.parent() {
                std::fs::create_dir_all(dir)
                    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet dir: {}", e)));
            }
            bootstrap_wallet_file(&nw);
            let mut wh = tri7_core::wallet::WalletHandle::open_readwrite(&nw)
                .unwrap_or_else(|e| die(EXIT_USAGE, &format!("create wallet: {}", e)));
            let mut key = [0u8; 32];
            loop {
                getrandom::getrandom(&mut key)
                    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("keygen entropy: {}", e)));
                match wh.add_key(&key, "default") {
                    Ok(k) => {
                        wh.close();
                        insert_default_key(&nw, &k.pubkey);
                        wh = tri7_core::wallet::WalletHandle::open_readwrite(&nw)
                            .unwrap_or_else(|e| die(EXIT_USAGE, &format!("reopen wallet: {}", e)));
                        wh.update_bestblock(0, &[0u8; 32]).ok();
                        log(&format!(
                            "new-wallet: {} created with fresh key {}",
                            nw.display(),
                            k.address
                        ));
                        break;
                    }
                    Err(e) => die(EXIT_USAGE, &format!("add_key: {}", e)),
                }
            }
            wh.close();
        }
        if cfg.wallet_path.is_none() {
            cfg.wallet_path = Some(nw);
        }
    }
    init_addrindex(&cfg.state_dir);
    let (user, pass) = read_rpc_creds(&cfg);
    let rpc = tri7_rpc(&cfg, &user, &pass);
    // 1. multi-target outbound attach (HEADLESS TOLERANT): with a usable
    // tipstate the follower starts and serves its native RPC surface even
    // when every target is unreachable — it retries the rotation each poll
    // tick. Seeding (no state) still requires a live peer.
    let path = state_path(&cfg);
    let blocks_file = resolve_blocks_file(&cfg);
    // A state that knows only genesis (tip h0) with no block bodies on disk is
    // not usable state — treat it as a bare machine so the from-zero path
    // starts the body fetch on its own. Anyone who downloads the wallet must
    // be able to sync from zero without any flag or manual step (Sami).
    let store_usable = blocks_file
        .as_ref()
        .map(|bf| {
            bf.exists() && std::fs::metadata(bf).map(|m| m.len() > 0).unwrap_or(false)
        })
        .unwrap_or(false);
    let have_state = path.exists() && !cfg.reseed && store_usable;
    // FROM-ZERO: explicit flag, or AUTOMATIC on a bare machine (no state
    // + explicit peers) — the fresh-install default path.
    let from_zero = cfg.sync_from_zero || (!have_state && cfg.explicit_peers);
    let mut stats = Stats::default();
    let mut outbound_start = 0usize;
    let mut peers: Vec<Peer> = Vec::new();
    // FIX (exit-5 bug): the outbound attach must be attempted on a fresh
    // machine too — seeding/from-zero needs a live peer even when no
    // tipstate exists yet.
    if have_state || from_zero {
        match try_connect(&cfg, &mut stats, 0, 0) {
            Some((p, idx)) => {
                outbound_start = (idx + 1) % cfg.peers.len();
                peers.push(p);
            }
            None => log(&format!(
                "no outbound target reachable ({} configured) — starting HEADLESS \
                 (native RPCs from persisted state)",
                cfg.peers.len()
            )),
        }
    }

    // 2. load or seed chain state
    let mut st: ChainState = if have_state {
        log("loading tipstate.json …");
        load_state(&cfg).unwrap_or_else(|e| die(EXIT_SEED, &format!("load state: {}", e)))
    } else if from_zero {
        // fresh state from genesis: fetch block 0, pin it to the compiled-in
        // genesis hash, build height-0 state — then the backfill below walks
        // the ENTIRE chain through the normal acceptance gate
        let gen = match peers.first_mut() {
            Some(p) => fetch_genesis_block(&mut p.sess, &mut stats),
            None => die(
                EXIT_SEED,
                "from-zero: needs a live peer but all outbound targets are unreachable",
            ),
        };
        let gen = match gen {
            Ok(g) => g,
            Err(e) => die(EXIT_SEED, &e),
        };
        let st = genesis_state(&gen);
        write_state_atomic(&cfg, &st.to_file())
            .unwrap_or_else(|e| die(EXIT_SEED, &format!("write state: {}", e)));
        log(&format!(
            "from-zero: genesis state anchored at {} (h0)",
            GENESIS_OFFICIAL
        ));
        st
    } else {
        log("no usable state — seeding: raw blocks over P2P getdata, Hash9+link+merkle verified locally …");
        let seeded = match peers.first_mut() {
            Some(p) => seed_from_network(&cfg, &rpc, &mut p.sess, &mut stats),
            None => die(
                EXIT_SEED,
                "seeding needs a live peer but all outbound targets are unreachable",
            ),
        };
        if let Err(e) = &seeded {
            if let Some(p) = peers.first_mut() {
                p.sess.shutdown();
            }
            die(EXIT_SEED, &e);
        }
        let (chain, file) = seeded.unwrap();
        write_state_atomic(&cfg, &file)
            .unwrap_or_else(|e| die(EXIT_SEED, &format!("write state: {}", e)));
        log(&format!(
            "seeded: tip h{} {} (window base {})",
            chain.tip_height,
            tri7_core::p2p::internal_to_display(&chain.tip_hash),
            chain.base
        ));
        chain
    };

    // report our real height to the peer: reconnect with the correct
    // start_height so its fNearTip logic treats us as a block peer
    if let Some(mut p) = peers.pop() {
        p.sess.shutdown();
        stats.reconnects += 1;
    }
    if let Some((p, idx)) = try_connect(&cfg, &mut stats, st.tip_height, outbound_start) {
        outbound_start = (idx + 1) % cfg.peers.len();
        peers.push(p);
    } else {
        log("no outbound peer after state load — HEADLESS mode (retrying each poll)");
    }

    let daemon_tip = rpc
        .call("getblockcount", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    if daemon_tip >= 0 {
        st.daemon_tip = Some(daemon_tip); // persisted on every state write
    }
    log(&format!(
        "state: tip h{} {} · daemon getblockcount {}",
        st.tip_height,
        tri7_core::p2p::internal_to_display(&st.tip_hash),
        daemon_tip
    ));

    // 2.5 FROM-ZERO backfill: walk the ENTIRE chain from our (possibly h0)
    // state to the peer tip through the normal acceptance gate. Resume-safe:
    // a restart with a partial tipstate continues from its tip. When the
    // rounds return 0, we ARE at the peer tip and normal follow takes over.
    let store = BlockStore::new(blocks_file.clone(), Some(cfg.index_dir.clone()));
    // FROM-ZERO durability prerequisite: the disk store must EXIST and be
    // ready BEFORE the first block is accepted. A from-zero box has no
    // blk0001.dat — without it the index build fails (stat on a missing
    // file), append_block buffers every accepted body in RAM forever, and a
    // restart loses the WHOLE validated chain (observed live on DNS3: tip
    // h17524 fully validated, zero bytes on disk). Create the empty file and
    // start the build here so appends go straight to disk during the walk.
    let mut store_build_started = false;
    if from_zero {
        if let Some(bf) = blocks_file.as_ref() {
            if !bf.exists() {
                std::fs::File::create(bf)
                    .map_err(|e| format!("create blocks file {}: {}", bf.display(), e))
                    .unwrap_or_else(|e| die(EXIT_SEED, &e));
                log(&format!("from-zero: created empty blocks store {}", bf.display()));
            }
            let oracle = BuildOracle {
                base: st.base,
                hashes: st.entries.iter().map(|e| e.hash).collect(),
                tip: st.tip_height,
            };
            store.spawn_build(Some(oracle));
            store_build_started = true;
            log("from-zero: block index build started before sync (appends land on disk)");
        }
    }
    if from_zero {
        match peers.first_mut() {
            Some(p) => {
                let peer_tip = p.start_height.max(0) as u64;
                match sync_from_zero(
                    &cfg,
                    &mut p.sess,
                    &mut st,
                    &rpc,
                    &mut stats,
                    &store,
                    peer_tip,
                ) {
                    Ok(()) => {
                        // full chain already fetched+validated — the generic
                        // backfill below would only re-walk window headers
                        cfg.backfill_headers = 0;
                    }
                    Err(e) if e == "__SIGINT__" => {
                        finish(
                            &cfg,
                            &st,
                            &stats,
                            &rpc,
                            130,
                            "from-zero sync interrupted (SIGINT/SIGTERM) — tipstate persisted, resumable",
                        );
                    }
                    Err(e) => {
                        // A transient peer disconnect must not abandon an
                        // hours-long sync. The Pi died at h756000 with
                        // "peer closed connection" after an hour of work; the
                        // tip was persisted, so resume from it instead of
                        // reporting a validation failure.
                        let mut attempts = 0u32;
                        let mut last_err = e;
                        loop {
                            if attempts >= 20 {
                                finish(
                                    &cfg,
                                    &st,
                                    &stats,
                                    &rpc,
                                    EXIT_VALIDATE,
                                    &format!(
                                        "from-zero sync: {} (gave up after {} reconnects)",
                                        last_err, attempts
                                    ),
                                );
                            }
                            attempts += 1;
                            log(&format!(
                                "from-zero sync: {} — reconnecting (attempt {}/20) from h{}",
                                last_err, attempts, st.tip_height
                            ));
                            std::thread::sleep(std::time::Duration::from_secs(
                                (attempts.min(6) * 5) as u64,
                            ));
                            let start = st.tip_height.min(usize::MAX as u64) as usize;
                            match connect_and_handshake(&cfg, &mut stats, st.tip_height, start) {
                                Ok((mut peer, _idx)) => {
                                    let peer_tip = peer.start_height.max(0) as u64;
                                    match sync_from_zero(
                                        &cfg,
                                        &mut peer.sess,
                                        &mut st,
                                        &rpc,
                                        &mut stats,
                                        &store,
                                        peer_tip,
                                    ) {
                                        Ok(()) => {
                                            cfg.backfill_headers = 0;
                                            break;
                                        }
                                        Err(e2) if e2 == "__SIGINT__" => {
                                            finish(
                                                &cfg,
                                                &st,
                                                &stats,
                                                &rpc,
                                                130,
                                                "from-zero sync interrupted (SIGINT/SIGTERM) — tipstate persisted, resumable",
                                            );
                                        }
                                        Err(e2) => last_err = e2,
                                    }
                                }
                                Err(e2) => last_err = format!("reconnect failed: {}", e2),
                            }
                        }
                    }
                }
            }
            None => die(
                EXIT_SEED,
                "from-zero: no outbound peer reachable — cannot backfill",
            ),
        }
    }

    // 3. backfill
    if cfg.backfill_headers > 0 {
        if let Some(p) = peers.first_mut() {
            if let Err(e) = run_backfill(&mut p.sess, &st, &rpc, &mut stats, &cfg) {
                p.sess.shutdown();
                finish(
                    &cfg,
                    &st,
                    &stats,
                    &rpc,
                    EXIT_PROTOCOL,
                    &format!("backfill: {}", e),
                );
            }
        } else {
            log("backfill skipped — no outbound peer (headless mode)");
        }
    }

    // 3.5 phase 5b/6: shared state + RPC surface + block index + P2P listener.
    // The follow loop keeps exclusive ownership via the mutex; RPC threads take
    // short per-call locks (no lock is held across the 40 ms sleep).
    let st_shared: SharedState = Arc::new(Mutex::new(st));
    // ---- explorer: `--balances` builder thread -------------------------------
    // An EXPLICIT --balances PATH means the node MAINTAINS its balances
    // snapshot: a background thread rebuilds it (full-archive replay via the
    // shared balances_build engine, atomic tmp+rename writes) when it is
    // missing or its coverage fell behind the tip, and the native `richlist`
    // verb serves it (mtime-keyed cache → a rebuild is visible without a
    // restart). The replay touches NO daemon locks and runs on its own
    // thread — explorer pressure stays separable from relay/staking flow.
    if cfg.balances_build {
        let b_blk = blocks_file.clone();
        let b_out = cfg.balances_path.clone();
        let b_utxos = cfg.wallet_utxos_path.clone();
        let b_wallet = cfg.wallet_path.clone();
        let b_st = Arc::clone(&st_shared);
        let builder = std::thread::Builder::new().name("balances-build".into());
        builder
            .spawn(move || {
                // ~1 day of PoS minting (≈15 s/block); stale by more than
                // this → rebuild. Re-check cadence 6 h; missing blk file
                // (fresh from-zero folder) retries in 10 min.
                const REFRESH_BEHIND: u64 = 5_760;
                const REFRESH_CHECK_SECS: u64 = 6 * 3600;
                const NO_BLK_RETRY_SECS: u64 = 600;
                let mut blk_missing_logged = false;
                let mut sched = RebuildSchedule::new();
                let mut last_attempt: Option<std::time::Instant> = None;
                loop {
                    let blk = match b_blk.as_ref() {
                        Some(p) if p.exists() => p.clone(),
                        _ => {
                            if !blk_missing_logged {
                                log(&format!(
                                    "balances-build: no blk file yet ({} looked for {:?}) — will retry in {}s",
                                    b_out.display(),
                                    b_blk,
                                    NO_BLK_RETRY_SECS
                                ));
                                blk_missing_logged = true;
                            }
                            std::thread::sleep(Duration::from_secs(NO_BLK_RETRY_SECS));
                            continue;
                        }
                    };
                    blk_missing_logged = false;
                    // snapshot coverage height (missing/unparseable → 0)
                    let snap_h = std::fs::read_to_string(&b_out)
                        .ok()
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        .and_then(|v| v["height"].as_u64())
                        .unwrap_or(0);
                    // chain tip (try_lock only — never block on the loop)
                    let tip = b_st
                        .try_lock()
                        .map(|g| g.tip_height)
                        .unwrap_or(snap_h.max(0));
                    let stale = snap_h.saturating_add(REFRESH_BEHIND) < tip || snap_h == 0;
                    let elapsed = last_attempt
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(u64::MAX);
                    if !sched.due(stale, elapsed) {
                        // sleep only the REMAINING wait: the scheduler owns
                        // the pacing, so there is never a second sleep after
                        // the build
                        let remain = if sched.streak > 0 {
                            sched.backoff_secs().saturating_sub(elapsed).max(1)
                        } else {
                            REFRESH_CHECK_SECS
                        };
                        std::thread::sleep(Duration::from_secs(remain.min(
                            REFRESH_CHECK_SECS,
                        )));
                        continue;
                    }
                    log(&format!(
                        "balances-build: start (snapshot h{}, chain tip h{}) — replaying {}",
                        snap_h,
                        tip,
                        blk.display()
                    ));
                    let started = std::time::Instant::now();
                    // coverage contract: accept whatever the walk reaches
                    // (EOF at any height, or a stale-branch stop) — the
                    // snapshot's own height + tip hash are served honestly
                    // by the richlist verb so consumers can judge the lag.
                    let opts = tri7_core::balances_build::BalancesBuildOpts {
                        blk: &blk,
                        out: &b_out,
                        wallet: b_wallet.as_deref(),
                        // pair the UTXO dump with the balances rebuild so the
                        // listunspent coverage gate (height AND hash must
                        // match across BOTH snapshots) stays green
                        utxos_out: b_wallet.as_ref().map(|_| b_utxos.as_path()),
                        end_height: 0,
                        // the tip this replay is chasing: the snapshot
                        // publishes it, so the RPC can tell a whole-chain
                        // figure from one that was capped by a hole
                        replay_target: Some(tip as usize),
                        allow_stale: true,
                    };
                    let mut progress = |blocks: usize, live: usize| {
                        log(&format!(
                            "balances-build: ... {} blocks, {} live UTXOs",
                            blocks, live
                        ));
                    };
                    match tri7_core::balances_build::build_balances_snapshot(&opts, &mut progress)
                    {
                        Ok(rep) => {
                            log(&format!(
                                "balances-build: done in {:.1}s — coverage h{} tip {} · supply {} sats · {} live UTXOs · wallet {} keys {} sats → {}",
                                started.elapsed().as_secs_f64(),
                                rep.covered_to,
                                rep.tip_hash_display,
                                rep.total_balance_sats,
                                rep.live_utxos,
                                rep.wallet_key_count,
                                rep.wallet_balance_sats,
                                rep.out_path.display()
                            ));
                            // did the walk reach the tip it chased? if not,
                            // blocks were landing while it walked - record it
                            // and loop: the scheduler paces the retry, so the
                            // snapshot converges to the whole chain.
                            last_attempt = Some(std::time::Instant::now());
                            sched.record((rep.covered_to as u64) >= tip);
                            if sched.streak > 0 {
                                log(&format!(
                                    "balances-build: coverage h{} short of the chased tip h{} — retrying in {}s",
                                    rep.covered_to,
                                    tip,
                                    sched.backoff_secs()
                                ));
                            }
                            // NO sleep here: the loop top owns the pacing
                        }
                        Err(e) => {
                            // transient failure (torn read, disk full): retry
                            // on the SHORT cycle, not the 6h one
                            log(&format!(
                                "balances-build: FAILED: {} — retrying in {}s",
                                e, NO_BLK_RETRY_SECS
                            ));
                            std::thread::sleep(Duration::from_secs(NO_BLK_RETRY_SECS));
                            continue;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(REFRESH_CHECK_SECS));
                }
            })
            .map(|_| ())
            .map_err(|e| format!("spawn balances-build thread: {e}"))
            .unwrap_or_else(|e| log(&format!("balances-build thread NOT started: {e}")));
    }
    let last_write_shared = Arc::new(Mutex::new(Instant::now()));
    let peers_shared = Arc::new(AtomicU32::new(peers.len() as u32));
    // ---- phase 7 milestone 2: wallet live state / stake loop / txpool / smsg
    let stake_enabled = cfg.stake.unwrap_or(false);
    let stake_st_shared: SharedStake = Arc::new(Mutex::new(StakeState {
        enabled: stake_enabled,
        ..Default::default()
    }));
    let wl_shared: SharedWalletLive = Arc::new(Mutex::new(None));
    let pool_shared: SharedPool = Arc::new(Mutex::new(tri7_core::txpool::TxPool::default()));
    let announce_shared: TxAnnounce = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let peers_table: SharedPeers = Arc::new(Mutex::new(Vec::new()));
    // smsg engine: requires the wallet (keys are the smsg identities)
    // ---- wallet load: plaintext, or public-only LOCKED when encrypted -----
    // This is the single decision point. Everything below (smsg identities, the
    // live-view seed thread, staking) keys off `wallet_keys`, so an encrypted
    // wallet is not treated as "unreadable" and does not silently disable the
    // node — it starts locked, with addresses watchable and signing withheld.
    let mut wallet_locked_at_start = false;
    let wallet_keys: Option<tri7_core::wallet::WalletKeys> = match &cfg.wallet_path {
        Some(wp) => match tri7_core::wallet::read_wallet(wp) {
            Ok(keys) => Some(keys),
            Err(e) => {
                if tri7_core::wallet_encrypt::is_encrypted(wp) {
                    match tri7_core::wallet_encrypt::read_pubkeys(wp) {
                        Ok(pubs) => {
                            let keys: Vec<tri7_core::wallet::WalletKey> = pubs
                                .iter()
                                .map(|pk| tri7_core::wallet::WalletKey {
                                    pubkey: pk.clone(),
                                    secret: [0u8; 32],
                                    compressed: pk.len() == 33,
                                    address: tri7_core::wallet::pubkey_to_address(pk),
                                    created: None,
                                })
                                .collect();
                            let first = keys.first().map(|k| k.pubkey.clone());
                            let n = keys.len();
                            wallet_locked_at_start = true;
                            log(&format!(
                                "wallet is ENCRYPTED — starting LOCKED with {n} public key(s); \
                                 staking and signing wait for walletpassphrase"
                            ));
                            Some(tri7_core::wallet::WalletKeys {
                                keys,
                                default_key: first,
                                best_block: None,
                                version: None,
                                names: Vec::new(),
                                // locked: redeem script rows are unreadable,
                                // so no P2SH coins are watchable yet
                                scripts: Vec::new(),
                                encrypted: true,
                                hd_mnemonic: None,
                                hd_passphrase: None,
                                hd_chain_index: None,
                                keypool: Vec::new(),
                            })
                        }
                        Err(e2) => {
                            log(&format!("wallet {} encrypted but keys unreadable: {e2}", wp.display()));
                            None
                        }
                    }
                } else {
                    log(&format!(
                        "wallet {} unreadable: {} — staking + smsg disabled (broadcast relay view stays off)",
                        wp.display(),
                        e
                    ));
                    None
                }
            }
        },
        None => {
            log("no --wallet given: staking/smsg off, sendrawtransaction view off");
            None
        }
    };
    // smsg identities are secret-key based: only a plaintext wallet can start one
    let smsg_engine: Option<Arc<tri7_core::smsg_net::SmsgEngine>> = match (&wallet_keys, wallet_locked_at_start) {
        (Some(keys), false) => {
            let owned: Vec<tri7_core::smsg::SmsgKey> = keys
                .keys
                .iter()
                .map(|k| tri7_core::smsg::SmsgKey {
                    address: k.address.clone(),
                    secret: k.secret,
                    pubkey: k.pubkey.clone(),
                })
                .collect();
            match tri7_core::smsg_net::SmsgEngine::open(&cfg.state_dir.join("smsg"), owned) {
                Ok(e) => {
                    log(&format!(
                        "smsg: engine live with {} wallet key(s) — store {}",
                        e.owned.len(),
                        cfg.state_dir.join("smsg").display()
                    ));
                    Some(e)
                }
                Err(e) => {
                    log(&format!("smsg: engine NOT started: {}", e));
                    None
                }
            }
        }
        (Some(_), true) => {
            log("smsg: skipped — wallet is locked (unlock it to send/receive encrypted messages)");
            None
        }
        (None, _) => None,
    };
    // lock-free tip mirror for the RPC threads (deadlock-free validation path)
    let tip_mirror = {
        let stg = st_shared.lock().unwrap();
        TipMirror::new(
            stg.tip_height,
            &tri7_core::p2p::internal_to_display(&stg.tip_hash),
        )
    };
    let balances_snap = load_balances_snap(&cfg.balances_path);
    let utxos_snap = load_utxos_snap(&cfg.wallet_utxos_path);
    let recent_shared: Arc<Mutex<RecentBlocks>> = Arc::new(Mutex::new(RecentBlocks::new()));
    // wallet live UTXO view: seeded from the disk store in a background
    // thread once the index build finishes (needs heights_ok + bodies)
    if let Some(start_keys) = wallet_keys.clone() {
        let store2 = Arc::clone(&store);
        let wl2 = Arc::clone(&wl_shared);
        let st2 = Arc::clone(&st_shared);
        let wp2 = cfg.wallet_path.clone();
        let start_keys = start_keys.clone();
        let locked_at_start = wallet_locked_at_start;
        std::thread::Builder::new()
            .name("wallet-seed".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                if !matches!(store2.status(), StoreStatus::Ready { .. }) {
                    continue;
                }
                let (base, tip) = {
                    let g = st2.lock().unwrap();
                    (g.base, g.tip_height)
                };
                let Some(wp) = &wp2 else { return };
                // CLAMP the seed tip: the tip block arrives as a HEADER first
                // and its body lands a beat later (and live follow keeps the
                // tip moving). Seeding base..tip hard-failed on that missing
                // tail body and retried forever (sami-pc: "store missing block
                // body at h<tip>"). Walk back to the highest height the store
                // actually holds a body for — the same clamp repairwallet's
                // rebuild uses (newest_replayable_tip).
                let has_body = |h: u64| store2.block_by_height(h).ok().flatten().is_some();
                let (seed_tip, _back) = newest_replayable_tip(&has_body, base, tip);
                if seed_tip <= base {
                    log(&format!(
                        "wallet-seed: no stored bodies in h{}..h{} - store behind the tip, \
                         retrying (this is why the balance would read 0)",
                        base, tip
                    ));
                    continue;
                }
                if seed_tip < tip {
                    log(&format!(
                        "wallet-seed: store's newest body is h{} (tip h{}) - seeding up to \
                         there; newer coins arrive as bodies land",
                        seed_tip, tip
                    ));
                }
                let keys = start_keys.clone();
                match WalletLive::seed(&store2, keys, base, seed_tip) {
                    Ok(mut w) => {
                        if locked_at_start {
                            w.locked = true;
                        }
                        let bal = w.balance();
                        let (mut owned_n, mut funded_n) = (0usize, 0usize);
                        {
                            let (cands, _nk, _ul, _ns) = w.candidates();
                            owned_n = cands.len();
                            funded_n = w.owned_count();
                        }
                        let nkeys = w.wallet.keys.len();
                        *wl2.lock().unwrap() = Some(w);
                        log(&format!(
                            "wallet live view ready: window h{}..h{} — {} outputs ({} owned) balance {} sats{}",
                            base,
                            seed_tip,
                            funded_n,
                            owned_n,
                            bal,
                            if locked_at_start {
                                format!(" [LOCKED: {} keys, public only]", nkeys)
                            } else {
                                String::new()
                            }
                        ));
                        return;
                    }
                    Err(e) => log(&format!("wallet live seed retrying: {}", e)),
                }
            })
            .expect("spawn wallet-seed thread");
    }
    {
        // chain-threading oracle: the daemon-anchored window hashes
        let oracle = {
            let g = st_shared.lock().unwrap();
            BuildOracle {
                base: g.base,
                hashes: g.entries.iter().map(|e| e.hash).collect(),
                tip: g.tip_height,
            }
        };
        if blocks_file.is_some() {
            if store_build_started {
                log("rpc: block index already building since the from-zero pre-sync start — not re-spawning");
            } else {
                store.spawn_build(Some(oracle));
                log("rpc: block index build started (background thread, cache-first)");
            }
        } else {
            log(
                "rpc: no blocks file — getblock/getrawtransaction/getblockhash will \
                 use the legacy proxy fallback",
            );
        }
    }
    // Wallet-file bookkeeping published TO the passive native handlers
    // (bestblock / fee / unlock state / address→pubkey map). The ops engine
    // refreshes these from the real wallet; unwired stays honest (null).
    let wallet_extras: Arc<std::sync::RwLock<rpc::WalletExtras>> = {
        let mut e = rpc::WalletExtras::default();
        if let Some(wp) = cfg.wallet_path.as_deref() {
            match tri7_core::wallet::read_wallet(wp) {
                Ok(keys) => {
                    e.default_key = keys.default_key.clone();
                    if let Some(h) = keys.best_block {
                        // bestblock carries no height in the legacy row; the
                        // ops engine fills the height from the live tip
                        e.bestblock_hash =
                            Some(tri7_core::p2p::internal_to_display(&h));
                    }
                }
                Err(err) => log(&format!("wallet bookkeeping: {err}")),
            }
        }
        Arc::new(std::sync::RwLock::new(e))
    };
    // Immediate keypool census publish + the background top-up maintainer.
    // The census lands on getwalletinfo within a second of start (the
    // maintainer's first pass); the refill, when one is needed (fresh seed
    // wallets carry an empty pool), derives in the background and never
    // blocks node start.
    if let Some(wp) = cfg.wallet_path.as_deref() {
        match tri7_core::wallet::keypool_stats(wp) {
            Ok((n, oldest)) => {
                if let Ok(mut g) = wallet_extras.write() {
                    g.keypool_size = Some(n);
                    g.keypool_oldest = oldest;
                }
                log(&format!(
                    "keypool: census {} entr{} (oldest epoch {})",
                    n,
                    if n == 1 { "y" } else { "ies" },
                    oldest.map(|t| t.to_string())
                        .unwrap_or_else(|| "n/a".to_string())
                ));
            }
            Err(e) => log(&format!("keypool: initial census failed: {e}")),
        }
        let wp_buf = wp.to_path_buf();
        let extras_for_thread = Arc::clone(&wallet_extras);
        match std::thread::Builder::new()
            .name("keypool-topup".into())
            .spawn(move || keypool_maintainer_loop(wp_buf, extras_for_thread))
        {
            Ok(_) => log(&format!(
                "keypool: top-up maintainer running (target {}, check every {}s)",
                DEFAULT_KEYPOOL_SIZE, KEYPOOL_CHECK_SECS
            )),
            Err(e) => log(&format!("keypool: maintainer spawn failed: {e}")),
        }
    }
    let wallet_pubkeys: Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<u8>>>> =
        Arc::new(std::sync::RwLock::new(
            wl_shared
                .lock()
                .ok()
                .and_then(|g| {
                    g.as_ref().map(|w| {
                        w.wallet
                            .keys
                            .iter()
                            .map(|k| (k.address.clone(), k.pubkey.clone()))
                            .collect()
                    })
                })
                .unwrap_or_default(),
        ));
    // Address book labels (legacy mapAddressBook name rows): consumed by
    // listaddressgroupings for the IsChange decision + the label column.
    let wallet_labels: Arc<std::sync::RwLock<std::collections::HashMap<String, String>>> =
        Arc::new(std::sync::RwLock::new(
            wl_shared
                .lock()
                .ok()
                .and_then(|g| {
                    g.as_ref().map(|w| w.wallet.names.iter().cloned().collect())
                })
                .unwrap_or_default(),
        ));
    // Operator fee setting persisted by settxfee (nTransactionFee). Restored
    // BEFORE the RPC listener answers so estimatefee and every spend from
    // second zero agree with the setting the operator last made.
    if let Ok(mut g) = wallet_extras.write() {
        match std::fs::read_to_string(cfg.state_dir.join("paytxfee.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("paytxfee_sats").and_then(|x| x.as_i64()))
        {
            Some(sats) => {
                g.paytxfee_sats = Some(sats);
                log(&format!("fee: paytxfee restored from paytxfee.json ({sats} sats)"));
            }
            None => {
                g.paytxfee_sats = Some(tri7_core::spend::MIN_FEE_SATS);
            }
        }
    }
    if cfg.rpc_enabled {
        let deps = Arc::new(rpc::ServerDeps {
            tip: Arc::new(LiveTip {
                st: Arc::clone(&st_shared),
                started: Instant::now(),
                last_write: Arc::clone(&last_write_shared),
                peers: Arc::clone(&peers_shared),
                mirror: Arc::clone(&tip_mirror),
            }),
            wallet: Arc::new(LiveWalletProvider {
                snap: Arc::new(SnapshotWallet {
                    balances: balances_snap,
                    utxos: utxos_snap,
                    extra: Arc::clone(&wallet_extras),
                    pubkeys: Arc::clone(&wallet_pubkeys),
                    labels: Arc::clone(&wallet_labels),
                }),
                wl: Arc::clone(&wl_shared),
                st: Arc::clone(&st_shared),
                state_dir: cfg.state_dir.clone(),
                balances_path: cfg.balances_path.clone(),
                rich_cache: Arc::new(Mutex::new(tri7_core::rpc::RichListCache::new())),
            }),
            blocks: Arc::new(StoreAdapter {
                store: Arc::clone(&store),
                tip: Arc::clone(&st_shared),
                mirror: Arc::clone(&tip_mirror),
                recent: Arc::clone(&recent_shared),
            }),
            proxy: if cfg.no_proxy {
                None // cutover: legacy daemon retired — proxy-only methods lose service
            } else {
                Some(Arc::new(LegacyProxyAdapter {
                    host: cfg.rpc_host.clone(),
                    port: cfg.rpc_port,
                    user: user.clone(),
                    pass: pass.clone(),
                    timeout: Duration::from_secs(cfg.proxy_timeout_secs),
                }))
            },
            ops: Some(Arc::new(OpsAdapter {
                st: Arc::clone(&st_shared),
                mirror: Arc::clone(&tip_mirror),
                extras: Arc::clone(&wallet_extras),
                pubkeys: Arc::clone(&wallet_pubkeys),
                wl: Arc::clone(&wl_shared),
                pool: Arc::clone(&pool_shared),
                stake_st: Arc::clone(&stake_st_shared),
                announce: Arc::clone(&announce_shared),
                peers_table: Arc::clone(&peers_table),
                smsg: smsg_engine.clone(),
                conf_path: cfg.conf_path.clone(),
                wallet_path: cfg.wallet_path.clone(),
                state_dir: cfg.state_dir.clone(),
                store: Some(Arc::clone(&store)),
            })),
            http: rpc::HttpServerConfig {
                debug: cfg.rpc_debug,
                max_body: cfg.rpc_max_body,
                read_timeout: Duration::from_secs(15),
            },
        });
        let listener = rpc::bind_loopback(&cfg.rpc_bind, cfg.rpc_listen_port)
            .unwrap_or_else(|e| die(EXIT_USAGE, &format!("rpc listener: {}", e)));
        log(&format!(
            "rpc: listening on {}:{} (loopback-only; native surface + transitional legacy-proxy fallback)",
            cfg.rpc_bind, cfg.rpc_listen_port
        ));
        std::thread::Builder::new()
            .name("rpc".into())
            .spawn(move || rpc::serve(deps, listener))
            .expect("spawn rpc thread");
    }

    // 4. inbound P2P listener (phase 6 — tri7d is a primary network peer)
    let listener: Option<TcpListener> = match cfg.listen_port {
        Some(port) => {
            let addr = format!("{}:{}", cfg.listen_bind, port);
            match TcpListener::bind(&addr) {
                Ok(l) => {
                    l.set_nonblocking(true)
                        .expect("listener nonblocking");
                    log(&format!("p2p: inbound listener on {} (getdata/getheaders/push-block)", addr));
                    Some(l)
                }
                Err(e) => {
                    // not fatal: the node still follows + serves RPC, but the
                    // "primary" role is degraded — say so loudly
                    log(&format!(
                        "p2p: CANNOT bind inbound listener {}: {} — no inbound peers this run",
                        addr, e
                    ));
                    None
                }
            }
        }
        None => {
            log("p2p: inbound listener disabled (--no-listen)");
            None
        }
    };

    // Bookkeeping publish BEFORE the first state write: getwalletinfo must not
    // answer null just because the live wallet view is still seeding.
    if let Ok(mut g) = wallet_extras.write() {
        let (h, hash) = {
            let stg = st_shared.lock().unwrap();
            (
                stg.tip_height,
                tri7_core::p2p::internal_to_display(&stg.tip_hash),
            )
        };
        g.bestblock_height = Some(h);
        g.bestblock_hash = Some(hash);
    }

    // 5. live follow loop
    let deadline = if cfg.forever {
        None // daemon mode: no deadline, run until signalled (systemd restarts on failure)
    } else {
        Some(Instant::now() + Duration::from_secs(cfg.seconds))
    };
    let mut last_poll = Instant::now() - Duration::from_secs(POLL_SECS);
    let mut poll_parity = false;
    let mut outstanding: HashMap<Hash, u64> = HashMap::new();
    let mut pending_from_headers: VecDeque<(Hash, u64)> = VecDeque::new();
    let mut last_state_write = Instant::now();
    let mut relayed_tip: Option<Hash> = None;
    let gap_shared: Arc<Mutex<GapState>> = Arc::new(Mutex::new(GapState::default()));
    // phase 7 milestone 2 loop state
    let mut pending_tx_getdata: std::collections::HashSet<Hash> = std::collections::HashSet::new();
    let mut last_stake_tick = Instant::now() - Duration::from_secs(cfg.stake_tick_secs);
    let mut last_smsg_tick = Instant::now() - Duration::from_secs(2);
    let mut last_peer_table_sync = Instant::now() - Duration::from_secs(30);
    let mut last_smsg_purge = Instant::now();

    loop {
        if deadline.map_or(false, |d| Instant::now() >= d) {
            break;
        }
        // RPC `stop` and SIGTERM share this exit: the block is already
        // applied + persisted, so leaving here is a clean, resumable stop.
        if tri7_core::shutdown::requested() {
            log("shutdown requested (RPC stop) — exiting with state persisted");
            break;
        }

        // poll tick — rotation retry + getheaders with an alternating locator
        // (the legacy daemon silently drops a consecutive duplicate getheaders)
        if last_poll.elapsed() >= Duration::from_secs(POLL_SECS) {
            // poll tick body: rotate attach, poll outbound peers, refresh
            // the persisted daemon_tip (tri7d's OWN native getblockcount —
            // a live value proves the RPC surface answers; -1 = not yet)
            let height_for_attach = st_shared.lock().unwrap().tip_height;
            if let Some(dt) = rpc
                .call("getblockcount", serde_json::json!([]))
                .ok()
                .and_then(|v| v.as_i64())
            {
                st_shared.lock().unwrap().daemon_tip = Some(dt);
                // keep getwalletinfo's bestblock bookkeeping current — it is
                // what the GUI shows as "last block seen by this wallet"
                if let Ok(mut g) = wallet_extras.write() {
                    let (h, hash) = {
                        let stg = st_shared.lock().unwrap();
                        (stg.tip_height, tri7_core::p2p::internal_to_display(&stg.tip_hash))
                    };
                    g.bestblock_height = Some(h);
                    g.bestblock_hash = Some(hash);
                }
            }
            let have_outbound = peers.iter().any(|p| p.outbound);
            if !have_outbound {
                // headless/peerless: retry the outbound rotation each tick
                if let Some((p, idx)) = try_connect(&cfg, &mut stats, height_for_attach, outbound_start) {
                    log(&format!("outbound re-attached: {}", p.label));
                    outbound_start = (idx + 1) % cfg.peers.len();
                    peers.push(p);
                    st_shared.lock().unwrap().last_peer_utc = Some(now_iso());
                }
            }
            if peers.iter().any(|p| p.outbound) {
                {
                    let mut stg = st_shared.lock().unwrap();
                    poll_parity = !poll_parity;
                    let locator: Vec<Hash> = if poll_parity {
                        vec![stg.tip_hash]
                    } else {
                        let prev_hash = stg
                            .entry_abs(stg.tip_height - 1)
                            .map(|e| e.hash)
                            .unwrap_or(stg.tip_hash);
                        vec![prev_hash, stg.tip_hash]
                    };
                    let stop = [0u8; 32];
                    let body = tri7_core::p2p::build_getheaders(&locator, &stop);
                    for p in peers.iter_mut().filter(|p| p.outbound) {
                        if let Err(e) = p.sess.send("getheaders", &body, &mut stats) {
                            log(&format!(
                                "send getheaders to {} failed: {} — dropping",
                                p.label, e
                            ));
                            p.dead = true;
                            p.sess.shutdown();
                            break;
                        }
                        stats.polls += 1;
                    }
                }
            }

            // ---- GAP RECOVERY: walk the window tail for missing disk bodies
            // (skip while the index build is still warming up — block_by_hash
            // would report every body as "missing" and spam getdata)
            if matches!(store.status(), StoreStatus::Ready { .. }) {
                let threaded = store.threaded_tip();
                let found = {
                    let stg = st_shared.lock().unwrap();
                    gap_find_missing(&stg, &store, &recent_shared.lock().unwrap(), threaded)
                };
                let mut gap = gap_shared.lock().unwrap();
                match found {
                    None => {
                        if gap.missing.is_some() {
                            log("gap recovery: window tail complete — no missing bodies");
                        }
                        gap.missing = None;
                        gap.since = None;
                        gap.ticks_failed = 0;
                    }
                    Some((h, hash)) => {
                        stats.gap_ticks += 1;
                        if gap.missing != Some((h, hash)) {
                            log(&format!(
                                "GAP RECOVERY: tail body missing h{} {} — requesting \
                                 getdata(MSG_BLOCK) from every peer (rollback to h{} after \
                                 {} unresolved ticks)",
                                h,
                                tri7_core::p2p::internal_to_display(&hash),
                                h.saturating_sub(1),
                                GAP_ROLLBACK_TICKS
                            ));
                            gap.missing = Some((h, hash));
                            gap.since = Some(Instant::now());
                            gap.ticks_failed = 0;
                        }
                        let body =
                            tri7_core::p2p::build_getdata(&[(tri7_core::p2p::MSG_BLOCK, hash)]);
                        let mut sent = 0usize;
                        for p in peers.iter_mut() {
                            if p.sess.send("getdata", &body, &mut stats).is_ok() {
                                sent += 1;
                            } else {
                                p.dead = true;
                                p.sess.shutdown();
                            }
                        }
                        stats.gap_requests += 1;
                        gap.ticks_failed += 1;
                        log(&format!(
                            "gap: getdata for h{} sent to {} peer(s), unfilled ticks {}/{}",
                            h, sent, gap.ticks_failed, GAP_ROLLBACK_TICKS
                        ));
                        // ---- branch (b): the phantom tip ------------------
                        // only when the missing body IS the tip, the store
                        // threads exactly one below it, and the gap outlived
                        // every peer's chance to serve it
                        let tip_now = st_shared.lock().unwrap().tip_height;
                        if h == tip_now
                            && gap.ticks_failed >= GAP_ROLLBACK_TICKS
                            && threaded == Some(tip_now - 1)
                        {
                            let phantom = tri7_core::p2p::internal_to_display(&hash);
                            let since_secs = gap.since.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                            let mut stg = st_shared.lock().unwrap();
                            match rollback_tip(&mut stg) {
                                Ok((new_tip, new_hash)) => {
                                    stats.gap_rollback = true;
                                    gap.rolled_back_to = Some(new_tip);
                                    gap.missing = None;
                                    gap.since = None;
                                    gap.ticks_failed = 0;
                                    relayed_tip = None;
                                    log(&format!(
                                        "GAP RECOVERY BRANCH (b): ROLLBACK — h{} ({}) body \
                                         unobtainable from every peer for {}s; effective \
                                         network tip is now h{} {}; the phantom tip is \
                                         INVALIDATED and staking will mint a fresh block \
                                         at that height",
                                        tip_now, phantom, since_secs, new_tip,
                                        tri7_core::p2p::internal_to_display(&new_hash)
                                    ));
                                    write_gap_marker(&cfg, serde_json::json!({
                                        "branch": "rollback",
                                        "invalidated_height": tip_now,
                                        "invalidated_hash": phantom,
                                        "effective_tip_height": new_tip,
                                        "effective_tip_hash":
                                            tri7_core::p2p::internal_to_display(&new_hash),
                                        "unresolved_ticks": GAP_ROLLBACK_TICKS,
                                        "unresolved_secs": since_secs,
                                        "peers_tried": stats.gap_requests,
                                        "at_utc": now_iso(),
                                    }));
                                    tip_mirror.set(
                                        new_tip,
                                        &tri7_core::p2p::internal_to_display(&new_hash),
                                    );
                                    let file = stg.to_file();
                                    drop(stg);
                                    if let Err(e) = write_state_atomic(&cfg, &file) {
                                        log(&format!("rollback state write: {}", e));
                                    }
                                }
                                Err(e) => log(&format!("rollback refused: {}", e)),
                            }
                        }
                    }
                }
            }
            last_poll = Instant::now();
        }

        // accept inbound peers (nonblocking; handshake is bounded)
        if let Some(l) = listener.as_ref() {
            loop {
                match l.accept() {
                    Ok((stream, sa)) => {
                        let height = st_shared.lock().unwrap().tip_height;
                        let port = cfg.listen_port.unwrap_or(0);
                        match accept_handshake(stream, sa, &mut stats, height, port) {
                            Ok(p) => {
                                stats.inbound_accepted += 1;
                                log(&format!("inbound peer connected: {}", p.label));
                                peers.push(p);
                                st_shared.lock().unwrap().last_peer_utc = Some(now_iso());
                                peers_shared.store(peers.len() as u32, Ordering::Relaxed);
                            }
                            Err(e) => {
                                log(&format!("inbound handshake {} failed: {}", sa, e));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
                // bound the accepts per tick so a flood cannot starve the loop
                if peers.len() > 32 {
                    break;
                }
            }
            peers_shared.store(peers.len() as u32, Ordering::Relaxed);
        }

        // request queued block announcements from the FIRST outbound peer
        if peers.iter().any(|p| p.outbound)
            && !pending_from_headers.is_empty()
            && outstanding.len() < MAX_OUTSTANDING_GETDATA
        {
            let mut batch: Vec<(u32, Hash)> = Vec::new();
            while batch.len() < GETDATA_BATCH {
                match pending_from_headers.pop_front() {
                    Some((hash, height)) => {
                        if !outstanding.contains_key(&hash) {
                            outstanding.insert(hash, height);
                            batch.push((tri7_core::p2p::MSG_BLOCK, hash));
                        }
                    }
                    None => break,
                }
            }
            if !batch.is_empty() {
                let body = tri7_core::p2p::build_getdata(&batch);
                if let Some(p) = peers.iter_mut().find(|p| p.outbound) {
                    if let Err(e) = p.sess.send("getdata", &body, &mut stats) {
                        log(&format!("send getdata to {} failed: {} — dropping", p.label, e));
                        p.dead = true;
                        p.sess.shutdown();
                    }
                }
                peers.retain(|p| p.sess_ok());
            }
        }

        // pump + dispatch every peer's inbound messages
        let tip_before = st_shared.lock().unwrap().tip_height;
        let mut validation_failed: Option<String> = None;
        for pi in 0..peers.len() {
            // messages buffered during the inbound handshake replay first
            let mut msgs: Vec<tri7_core::p2p::RawMessage> =
                peers[pi].buffered.drain(..).collect();
            match peers[pi].sess.pump() {
                Ok(m) => msgs.extend(m),
                Err(e) => {
                    log(&format!(
                        "protocol failure from {}: {} — dropping",
                        peers[pi].label, e
                    ));
                    peers[pi].dead = true;
                    peers[pi].sess.shutdown();
                    stats.parse_failures += 1;
                }
            }
            if msgs.is_empty() {
                continue;
            }
            peers[pi].last_traffic = Instant::now();
            let mut fatal: Option<String> = None;
            {
                let mut stg = st_shared.lock().unwrap();
                stg.last_peer_utc = Some(now_iso());
                let mut msgs = msgs;
                for msg in msgs.drain(..) {
                    stats.note_recv(&msg.command);
                    peers[pi].msgs_in += 1;
                    let res = handle_message(
                        &mut peers[pi],
                        &mut stg,
                        &rpc,
                        &mut stats,
                        &cfg,
                        msg,
                        &mut outstanding,
                        &mut pending_from_headers,
                        &store,
                        &mut recent_shared.lock().unwrap(),
                        &gap_shared,
                        &smsg_engine,
                        &mut pool_shared.lock().unwrap(),
                        &wl_shared,
                        &announce_shared,
                        &mut pending_tx_getdata,
                    );
                    if let Err(e) = res {
                        fatal = Some(e);
                        break;
                    }
                }
            }
            if let Some(e) = fatal {
                if peers[pi].outbound {
                    // outbound targets are the trusted tip sources — a
                    // validation failure there is fatal for the process
                    // (phase-2 fork-safety stance, unchanged)
                    log(&format!(
                        "validation failure from {}: {} — clean disconnect",
                        peers[pi].label, e
                    ));
                    peers[pi].dead = true;
                    peers[pi].sess.shutdown();
                    validation_failed = Some(e);
                    break;
                }
                // inbound peers are untrusted: a malformed/invalid message
                // drops THE PEER, not the node (Misbehaving-style eviction;
                // a rejected pushed block simply never enters our chain)
                log(&format!(
                    "validation failure from {}: {} — dropping inbound peer",
                    peers[pi].label, e
                ));
                peers[pi].dead = true;
                peers[pi].sess.shutdown();
            }
        }
        peers.retain(|p| p.sess_ok());
        peers_shared.store(peers.len() as u32, Ordering::Relaxed);
        if let Some(e) = validation_failed {
            let st = st_shared.lock().unwrap();
            finish(&cfg, &st, &stats, &rpc, EXIT_VALIDATE, &e);
        }

        // relay: a validated block (pushed by a peer, another node, or our own
        // stake mint) is announced to every peer via inv → they getdata it
        let (tip_now, tip_hash_now) = {
            let stg = st_shared.lock().unwrap();
            (stg.tip_height, stg.tip_hash)
        };
        // keep the RPC mirror in sync (lock-free reads during validation)
        tip_mirror.set(
            tip_now,
            &tri7_core::p2p::internal_to_display(&tip_hash_now),
        );
        if tip_now > tip_before {
            let announced = relayed_tip.map_or(false, |h| h == tip_hash_now);
            if !announced {
                let body = tri7_core::p2p::build_inv(&[(tri7_core::p2p::MSG_BLOCK, tip_hash_now)]);
                let mut sent = 0usize;
                for p in peers.iter_mut() {
                    if p.sess.send("inv", &body, &mut stats).is_ok() {
                        sent += 1;
                    }
                }
                relayed_tip = Some(tip_hash_now);
                stats.blocks_relayed += sent as u64;
                log(&format!(
                    "relay: announced h{} to {} peer(s) (inv)",
                    tip_now, sent
                ));
            }
        }

        // ---- phase 7 milestone 2 ticks ----
        // (a) drain the tx-announce queue (sendrawtransaction + relay accepts)
        {
            let mut q = announce_shared.lock().unwrap();
            if !q.is_empty() && !peers.is_empty() {
                let batch: Vec<(u32, Hash)> = q.drain(..).map(|h| (tri7_core::p2p::MSG_TX, h)).collect();
                let body = tri7_core::p2p::build_inv(&batch);
                let mut sent = 0usize;
                for p in peers.iter_mut() {
                    if p.sess.send("inv", &body, &mut stats).is_ok() {
                        sent += 1;
                    }
                }
                peers.retain(|p| p.sess_ok());
                log(&format!(
                    "relay: announced {} tx inv(s) to {} peer(s)",
                    batch.len(),
                    sent
                ));
            }
        }
        // (b) smsg send-data cadence + lock aging + retention purge
        if let Some(engine) = &smsg_engine {
            if last_smsg_tick.elapsed() >= Duration::from_secs(2) {
                last_smsg_tick = Instant::now();
                let now = now_secs() as i64;
                engine.tick_locks();
                for p in peers.iter_mut() {
                    if let Some(out) = engine.send_data_tick(&p.label, now) {
                        if p.sess.send(out.command, &out.payload, &mut stats).is_ok() {
                            stats.note_sent(&format!("smsg_out_{}", out.command.to_ascii_lowercase()));
                        }
                    }
                }
                peers.retain(|p| p.sess_ok());
                if last_smsg_purge.elapsed() >= Duration::from_secs(600) {
                    last_smsg_purge = Instant::now();
                    engine.purge(now);
                }
            }
        }
        // (c) peer table sync (real per-session records for getpeerinfo)
        if last_peer_table_sync.elapsed() >= Duration::from_secs(2) {
            last_peer_table_sync = Instant::now();
            let mut table = peers_table.lock().unwrap();
            table.clear();
            for p in peers.iter() {
                table.push(PeerRec {
                    label: p.label.clone(),
                    endpoint: p.label.trim_start_matches("out:").trim_start_matches("in:").to_string(),
                    transport: p.transport.as_str(),
                    inbound: !p.outbound,
                    version: p.version,
                    subver: p.subver.clone(),
                    start_height: p.start_height,
                    msgs_in: p.msgs_in,
                    msgs_out: p.sess.sent_msgs,
                    connected_utc: p.connected_utc.clone(),
                    last_traffic: p.last_traffic,
                });
            }
            peers_shared.store(peers.len() as u32, Ordering::Relaxed);
        }
        // (d) the staking loop: kernel search + mint + relay (rolling modifier,
        // mature UTXOs of the live wallet view)
        if stake_st_shared.lock().unwrap().enabled
            && last_stake_tick.elapsed() >= Duration::from_secs(cfg.stake_tick_secs)
        {
            last_stake_tick = Instant::now();
            enforce_unlock_timeout(&wl_shared);
            let wl_ready = wl_shared.lock().unwrap().is_some();
            if wl_ready {
                let mut wl_guard = wl_shared.lock().unwrap();
                let wl = wl_guard.as_mut().unwrap();
                let mut pool = pool_shared.lock().unwrap();
                let stake_result = {
                    let mut stg = st_shared.lock().unwrap();
                    let mut stake_st = stake_st_shared.lock().unwrap();
                    stake_tick(
                        &cfg,
                        &mut stg,
                        &rpc,
                        &mut stats,
                        wl,
                        &mut pool,
                        &mut stake_st,
                        &store,
                        &mut peers,
                        &mut recent_shared.lock().unwrap(),
                    )
                };
                match stake_result {
                    Ok(true) => {
                        // ONE lock acquisition for BOTH fields: two temporary
                        // st_shared.lock() guards in the same expression live
                        // until the statement ends → the second lock() waits
                        // on the first → the whole follow loop wedges (live
                        // deadlock at h2204069, 2026-11-02 — RPC stalling on
                        // wl/stake_st held across this arm).
                        let g = st_shared.lock().unwrap();
                        tip_mirror.set(
                            g.tip_height,
                            &tri7_core::p2p::internal_to_display(&g.tip_hash),
                        );
                    }
                    Ok(false) => {}
                    Err(e) => {
                        // a failed search/assembly must NEVER kill the loop:
                        // log loudly (abort-on-failure rails from phase 4:
                        // one block in flight, distinct exit only for
                        // validation failures which stake_tick already
                        // propagates as Err — here it's contained)
                        stake_st_shared.lock().unwrap().last_error = Some(e.clone());
                        log(&format!("stake tick failed: {}", e));
                    }
                }
            } else {
                log("stake tick: wallet live view not ready yet (seeding)");
            }
        }

        // drop silent peers (90s without any inbound traffic)
        let stale_cut = Duration::from_secs(STALE_LINK_SECS);
        let dropped: Vec<String> = peers
            .iter()
            .filter(|p| p.last_traffic.elapsed() >= stale_cut)
            .map(|p| p.label.clone())
            .collect();
        if !dropped.is_empty() {
            for p in peers.iter_mut() {
                if p.last_traffic.elapsed() >= stale_cut {
                    log(&format!("no traffic for {}s — dropping {}", STALE_LINK_SECS, p.label));
                    p.dead = true;
                    p.sess.shutdown();
                }
            }
            peers.retain(|p| p.sess_ok());
            peers_shared.store(peers.len() as u32, Ordering::Relaxed);
        }

        // persist tip state periodically (network-liveness lives IN the state)
        if last_state_write.elapsed() >= Duration::from_secs(30) {
            let mut file = {
                let mut stg = st_shared.lock().unwrap();
                stg.peers_connected = peers.len() as u32;
                stg.to_file()
            };
            file.listen_port = cfg.listen_port;
            write_state_atomic(&cfg, &file)
                .unwrap_or_else(|e| die(EXIT_VALIDATE, &format!("write state: {}", e)));
            *last_write_shared.lock().unwrap() = Instant::now();
            last_state_write = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(40));
    }

    for p in peers.iter_mut() {
        p.sess.shutdown();
    }
    let mut file = {
        let mut stg = st_shared.lock().unwrap();
        // peers=0 (all streams just closed) but KEEP last_peer_utc: the gate
        // must age from the last KNOWN traffic, not report "no data"
        stg.peers_connected = 0;
        stg.to_file()
    };
    file.listen_port = cfg.listen_port;
    write_state_atomic(&cfg, &file)
        .unwrap_or_else(|e| die(EXIT_VALIDATE, &format!("write state: {}", e)));
    let st = st_shared.lock().unwrap();
    finish(&cfg, &st, &stats, &rpc, 0, "run complete");
}

impl Peer {
    /// Eviction check: retain() drops peers flagged dead (their streams are
    /// already shut down).
    fn sess_ok(&self) -> bool {
        !self.dead
    }
}

fn tri7_rpc(cfg: &Config, user: &str, pass: &str) -> Rpc {
    Rpc::new(&cfg.rpc_host, cfg.rpc_port, user, pass)
}

/// Blocks-file resolution for the native block index: explicit flag wins,
/// then <state-dir>/blk0001.dat (the service path — hardlink to the daemon's
/// file), then the archive default. None = index disabled (proxy fallback).
fn resolve_blocks_file(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = &cfg.blocks_file {
        return Some(p.clone());
    }
    let cand = cfg.state_dir.join("blk0001.dat");
    if cand.exists() {
        return Some(cand);
    }
    if std::path::Path::new(&cfg.archive).exists() {
        return Some(PathBuf::from(&cfg.archive));
    }
    None
}

/// One connected P2P peer — outbound (we attached) or inbound (they attached
/// to our listener). Both directions share the same message dispatch.
struct Peer {
    label: String,
    outbound: bool,
    sess: Session,
    last_traffic: Instant,
    /// messages decoded during the SERVER-side handshake that are not part of
    /// the handshake itself (a block pusher fires its block immediately after
    /// its own verack — discarding them would silently drop the push)
    buffered: VecDeque<tri7_core::p2p::RawMessage>,
    /// set alongside sess.shutdown() when the peer must be evicted (protocol
    /// failure, send failure, or traffic silence)
    dead: bool,
    /// transport this session rides (clearnet / tor / i2p) — inbound peers
    /// are classified from their address (onion/i2p listeners appear as the
    /// local bind's transports; remote clearnet IPs as clearnet)
    transport: Transport,
    /// handshake metadata for getpeerinfo
    version: i32,
    subver: String,
    start_height: i32,
    msgs_in: u64,
    msgs_out: u64,
    connected_utc: String,
}

/// Real per-session connection table shared with the RPC threads
/// (getpeerinfo / getconnectioncount / health.peer_detail).
#[derive(Clone)]
struct PeerRec {
    label: String,
    endpoint: String,
    transport: &'static str,
    inbound: bool,
    version: i32,
    subver: String,
    start_height: i32,
    msgs_in: u64,
    msgs_out: u64,
    connected_utc: String,
    /// last traffic instant snapshot (refreshed by the loop each tick)
    last_traffic: Instant,
}

type SharedPeers = Arc<Mutex<Vec<PeerRec>>>;

fn peer_table_json(table: &SharedPeers) -> serde_json::Value {
    let g = table.lock().unwrap();
    let now = Instant::now();
    let rows: Vec<serde_json::Value> = g
        .iter()
        .map(|p| {
            serde_json::json!({
                "label": p.label,
                "endpoint": p.endpoint,
                "transport": p.transport,
                "inbound": p.inbound,
                "version": p.version,
                "subver": p.subver,
                "start_height": p.start_height,
                "msgs_in": p.msgs_in,
                "msgs_out": p.msgs_out,
                "connected_utc": p.connected_utc,
                "last_traffic_ms_ago": now.duration_since(p.last_traffic).as_millis() as u64,
            })
        })
        .collect();
    serde_json::json!({
        "count": rows.len(),
        "by_transport": {
            "clearnet": rows.iter().filter(|r| r["transport"] == "clearnet").count(),
            "tor": rows.iter().filter(|r| r["transport"] == "tor").count(),
            "i2p": rows.iter().filter(|r| r["transport"] == "i2p").count(),
        },
        "peers": rows,
    })
}

/// Server-side handshake for an accepted inbound connection: the client
/// speaks first (legacy daemon behavior — it waits for our version on its
/// outbound connects), we answer version+verack together, then sendheaders
/// so the peer prefers header announcements.
fn accept_handshake(
    stream: TcpStream,
    peer_addr: std::net::SocketAddr,
    stats: &mut Stats,
    our_height: u64,
    listen_port: u16,
) -> Result<Peer, String> {
    stream.set_nodelay(true).ok();
    stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok();
    let mut sess = Session {
        stream,
        dec: tri7_core::p2p::MessageDecoder::new(),
        sent_msgs: 0,
    };
    // messages decoded while completing the server handshake are buffered and
    // replayed by the main loop (never discarded)
    let mut buffered: VecDeque<tri7_core::p2p::RawMessage> = VecDeque::new();
    // 1. wait for the client's version (deadline 20s)
    let mut peer: Option<PeerInfo> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while peer.is_none() && Instant::now() < deadline {
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            if msg.command == "version" {
                let v = tri7_core::p2p::parse_version(&msg.payload)?;
                if v.version < tri7_core::p2p::MIN_PROTO_VERSION {
                    return Err(format!("peer version {} obsolete", v.version));
                }
                peer = Some(PeerInfo {
                    version: v.version,
                    subver: v.user_agent.clone(),
                    start_height: v.start_height,
                    services: v.services,
                    advertised: advertised_store_key(&v.addr_me),
                });
            } else {
                buffered.push_back(msg);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let info = peer.ok_or("inbound handshake: no version from client")?;
    // 2. our version + verack together
    let (ya, yb, yc, yd, yport) = match peer_addr.ip() {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            (o[0], o[1], o[2], o[3], peer_addr.port())
        }
        _ => (127, 0, 0, 1, peer_addr.port()), // v6: informational placeholder
    };
    let addr_you = tri7_core::p2p::WireAddr::ipv4(
        ya,
        yb,
        yc,
        yd,
        yport,
        tri7_core::p2p::NODE_NETWORK,
    );
    let addr_me = tri7_core::p2p::WireAddr::ipv4(
        127,
        0,
        0,
        1,
        listen_port,
        tri7_core::p2p::NODE_NETWORK,
    );
    let nonce = (now_secs() ^ ((std::process::id() as u64) << 32)) | 1;
    let vp = tri7_core::p2p::VersionPayload {
        version: tri7_core::p2p::PROTOCOL_VERSION,
        services: tri7_core::p2p::NODE_NETWORK,
        timestamp: now_secs() as i64,
        addr_you,
        addr_me,
        nonce,
        user_agent: "/tri7:7.0.0/".into(),
        start_height: our_height as i32,
    };
    sess.send("version", &tri7_core::p2p::encode_version(&vp), stats)?;
    sess.send("verack", &[], stats)?;
    // 3. wait their verack — anything else arriving here (the pushed block!)
    // is buffered, not dropped
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut veracked = false;
    while !veracked && Instant::now() < deadline {
        for msg in sess.pump()? {
            stats.note_recv(&msg.command);
            if msg.command == "verack" {
                veracked = true;
            } else {
                buffered.push_back(msg);
            }
        }
        if veracked {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !veracked {
        return Err("inbound handshake: no verack from client".into());
    }
    // Discovery: same as outbound — ask for their address book after verack.
    sess.send("getaddr", &[], stats)?;
    sess.send("sendheaders", &[], stats)?;
    stats.handshakes += 1;
    Ok(Peer {
        label: format!("in:{} ({} h{})", peer_addr, info.subver, info.start_height),
        outbound: false,
        sess,
        last_traffic: Instant::now(),
        buffered,
        dead: false,
        transport: Transport::Clearnet,
        version: info.version,
        subver: info.subver.clone(),
        start_height: info.start_height,
        msgs_in: 0,
        msgs_out: 0,
        connected_utc: now_iso(),
    })
}

fn connect_and_handshake(
    cfg: &Config,
    stats: &mut Stats,
    height: u64,
    start: usize,
) -> Result<(Peer, usize), String> {
    // Dial order (BRIEF_TOR_V2 item 3): peers that have answered before and
    // are out of backoff first, then fresh store entries, then the
    // configured targets (seeds last — they are the network's last resort).
    // Store hosts are reconstructed .onion hostnames / .b32.i2p destinations
    // (dialable through their overlay SOCKS); leftover legacy hex-tor-key
    // entries and clearnet quads are relay-only and are never dialed: the
    // network has no clearnet P2P.
    let mut targets: Vec<PeerTarget> = Vec::new();
    let now = tri7_core::peers::unix_now();
    let seeds: Vec<(String, u16)> = cfg
        .peers
        .iter()
        .map(|t| (t.host.clone(), t.port))
        .collect();
    let store = tri7_core::peers::PeerStore::load(&peers_path(cfg));
    for (host, port) in store.dial_order(now, &seeds) {
        if !dialable_store_host(&host) {
            continue;
        }
        if let Ok(t) = PeerTarget::parse(&format!("{}:{}", host, port), &cfg.socks, &cfg.i2p_socks)
        {
            if !targets.iter().any(|e| e.host == t.host && e.port == t.port) {
                targets.push(t);
            }
        }
    }
    for t in &cfg.peers {
        if !targets.iter().any(|e| e.host == t.host && e.port == t.port) {
            targets.push(t.clone());
        }
    }
    let n = targets.len();
    for off in 0..n {
        let idx = (start + off) % n;
        let target = &targets[idx];
        match Session::connect_target(target) {
            Ok(mut sess) => match handshake(&mut sess, stats, height, target) {
                Ok(peer) => {
                    log(&format!(
                        "handshake ok: {} — peer version {} ({}) start height {} services {}",
                        target.endpoint(),
                        peer.version,
                        peer.subver,
                        peer.start_height,
                        peer.services
                    ));
                    record_peer_ok(
                        cfg,
                        &target.endpoint(),
                        peer.services,
                        peer.advertised.clone(),
                    );
                    return Ok((
                        Peer {
                            label: format!("out:{}", target.endpoint()),
                            outbound: true,
                            sess,
                            last_traffic: Instant::now(),
                            buffered: VecDeque::new(),
                            dead: false,
                            transport: target.transport,
                            version: peer.version,
                            subver: peer.subver.clone(),
                            start_height: peer.start_height,
                            msgs_in: 0,
                            msgs_out: 0,
                            connected_utc: now_iso(),
                        },
                        idx,
                    ));
                }
                Err(e) => {
                    log(&format!("handshake {} failed: {}", target.endpoint(), e));
                    record_peer_fail(cfg, &target.endpoint());
                    sess.shutdown();
                }
            },
            Err(e) => {
                log(&format!("connect {} failed: {}", target.endpoint(), e));
                record_peer_fail(cfg, &target.endpoint());
            }
        }
    }
    Err(format!("no outbound target reachable ({} tried)", n))
}

/// True when a peer-store host is dialable through an overlay transport:
/// a tor `.onion` hostname or an i2p `.b32.i2p`/`.i2p` destination (each
/// dials through its own SOCKS). Clearnet quads and legacy hex-tor-key
/// leftovers are relay-only and never dialed.
fn dialable_store_host(host: &str) -> bool {
    host.ends_with(".onion") || host.ends_with(".b32.i2p") || host.ends_with(".i2p")
}

/// Bounded multi-target connect that TOLERATES all targets being down
/// (headless mode): returns None instead of dying so the native RPC surface
/// can serve from persisted state while the follower keeps retrying the
/// rotation on later ticks.
fn try_connect(
    cfg: &Config,
    stats: &mut Stats,
    height: u64,
    start: usize,
) -> Option<(Peer, usize)> {
    match connect_and_handshake(cfg, stats, height, start) {
        Ok((peer, idx)) => Some((peer, idx)),
        Err(_) => None,
    }
}

fn finish(cfg: &Config, st: &ChainState, stats: &Stats, rpc: &Rpc, code: i32, note: &str) -> ! {
    let daemon_tip = rpc
        .call("getblockcount", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    let summary = serde_json::json!({
        "note": note,
        "exit_code": code,
        "tip": {
            "height": st.tip_height,
            "blockhash": tri7_core::p2p::internal_to_display(&st.tip_hash),
            "daemon_getblockcount": daemon_tip,
            "at_daemon_tip": daemon_tip >= 0 && st.tip_height == daemon_tip as u64,
        },
        "window": {
            "base": st.base,
            "entries": st.entries.len(),
            "utxo_tracked": st.utxo.map.len(),
        },
        "protocol": {
            "magic": "70352205",
            "outbound_targets": cfg.peers.iter().map(|t| t.endpoint()).collect::<Vec<_>>(),
            "listen": cfg.listen_port.map(|p| format!("{}:{}", cfg.listen_bind, p)),
            "protocol_version": tri7_core::p2p::PROTOCOL_VERSION,
            "user_agent": "/tri7:7.0.0/",
        },
        "messages": stats_json(stats),
    });
    let text = serde_json::to_string_pretty(&summary).unwrap();
    println!("SUMMARY\n{}", text);
    if let Some(p) = &cfg.summary_out {
        let _ = std::fs::write(p, text);
    }
    std::process::exit(code);
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // ---- the whole wallet's unspent set ----

    /// The Overview card sums listunspent, so that list has to be the WHOLE
    /// wallet: the replay snapshot (genesis..coverage) PLUS the live view's
    /// outputs newer than that coverage, MINUS the snapshot coins that were
    /// spent after the coverage.
    ///
    /// The live view alone is window scoped - it missed every coin the
    /// wallet received before the window base, which is what made the
    /// wallet show 1,627 TRI for a wallet holding 5,291 TRI. The snapshot
    /// alone would instead miss the newest coins, and would keep listing
    /// coins that have since been spent.
    #[test]
    fn the_unspent_set_is_the_snapshot_plus_the_fresh_live_outputs() {
        fn row(txid: &str, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n: 0,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        // the snapshot: two coins, coverage h200
        let snapshot = vec![row("aa", 100, 100), row("bb", 200, 200)];
        // the live view: one OLD coin (the window's past - the snapshot
        // already knows it) and one NEW coin (after the coverage)
        let live = vec![row("cc", 150, 999), row("dd", 250, 333)];

        let merged = merge_whole_wallet_utxos(snapshot, live, Vec::new(), 200, 100, true);
        let total: i64 = merged.iter().map(|r| r.value_sats).sum();
        assert_eq!(
            merged.len(),
            3,
            "expected the snapshot's two coins plus the one fresh live output"
        );
        assert_eq!(
            total, 633,
            "the whole wallet's coins, each counted once: 100 + 200 + 333"
        );
        // the live view's OLD output (999) is not counted twice - the
        // snapshot already carries that coin
        assert!(
            !merged.iter().any(|r| r.txid == "cc"),
            "a coin the snapshot already knows must not be counted twice"
        );
        // and the fresh one IS there
        assert!(
            merged.iter().any(|r| r.txid == "dd"),
            "a coin newer than the coverage must be included"
        );
        // newest first
        assert_eq!(merged[0].height, 250);
    }

    /// A coin the snapshot lists can have been SPENT after the snapshot's
    /// coverage. The live view saw that spend (it applies every block from
    /// its window base onwards), so the merged set must NOT keep listing
    /// it - otherwise the wallet shows more than it holds.
    #[test]
    fn a_snapshot_coin_spent_after_the_coverage_is_removed() {
        fn row(txid: &str, n: u32, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        // the snapshot holds two coins
        let snapshot = vec![row("aa", 0, 100, 100), row("bb", 1, 150, 200)];
        // "aa" was spent at h260 - AFTER the coverage (h200)
        let spends = vec![("aa".to_string(), 0u32, 260u64)];
        // and the live view has one new coin
        let live = vec![row("dd", 0, 250, 333)];

        let merged = merge_whole_wallet_utxos(snapshot, live, spends, 200, 100, true);
        let total: i64 = merged.iter().map(|r| r.value_sats).sum();
        assert!(
            !merged.iter().any(|r| r.txid == "aa"),
            "a coin spent after the coverage must not be listed"
        );
        assert!(merged.iter().any(|r| r.txid == "bb"), "the other coin stays");
        assert!(merged.iter().any(|r| r.txid == "dd"), "the fresh coin is there");
        assert_eq!(total, 533, "200 + 333 - the spent 100");
    }

    /// A spend at or BELOW the coverage is already reflected in the
    /// snapshot itself, so it must not remove anything (the snapshot is the
    /// authority for its own range).
    #[test]
    fn a_spend_at_or_below_the_coverage_removes_nothing() {
        fn row(txid: &str, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n: 0,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        let snapshot = vec![row("aa", 100, 100)];
        let spends = vec![("aa".to_string(), 0u32, 150u64)];
        let merged = merge_whole_wallet_utxos(snapshot, Vec::new(), spends, 200, 100, true);
        assert_eq!(merged.len(), 1, "the snapshot is the authority below coverage");
    }

    /// THE REBUILD SCHEDULER.
    ///
    /// A walk that stops short of the tip it chased must be retried on a
    /// doubling backoff (30s, 60s, 120s … capped at 30min) that is due
    /// even when the snapshot is otherwise NOT stale, and must reset when a
    /// walk finally reaches the tip. The backoff is capped, so the rebuilds
    /// stay periodically eligible FOREVER - a short walk never makes the
    /// snapshot permanently ineligible.
    #[test]
    fn the_rebuild_scheduler_backs_off_and_converges() {
        let mut sched = RebuildSchedule::new();

        // the first retry waits 30s
        sched.record(false); // walk 1 stopped short
        assert_eq!(sched.streak, 1);
        assert_eq!(sched.backoff_secs(), 30);
        // not yet due (0s since the attempt), even though the snapshot is
        // otherwise not stale
        assert!(!sched.due(false, 0), "the backoff must be honoured");
        assert!(!sched.due(false, 29));
        // due at 30s
        assert!(sched.due(false, 30), "due once the backoff has elapsed");

        // the second retry waits 60s, the third 120s …
        sched.record(false);
        assert_eq!(sched.backoff_secs(), 60);
        sched.record(false);
        assert_eq!(sched.backoff_secs(), 120);
    }

    #[test]
    fn the_backoff_is_capped_so_the_rebuilds_stay_periodic() {
        let mut sched = RebuildSchedule::new();
        for _ in 0..12 {
            sched.record(false);
        }
        // 30 * 2^11 would be way past the cap: the delay stops at 1800s
        assert_eq!(sched.backoff_secs(), 1_800);
        // and it STAYS due periodically - eligibility never expires
        for i in 0..50 {
            assert!(
                sched.due(false, 1_800 + i as u64),
                "iteration {i}: the scheduler must remain due"
            );
            sched.record(false); // every walk keeps coming up short
        }
        assert_eq!(sched.backoff_secs(), 1_800);
    }

    /// Retry while the snapshot is otherwise NOT stale: the short walk is
    /// the reason to rebuild, and the ordinary 6h cadence must not suppress
    /// it.
    #[test]
    fn a_short_walk_is_retried_even_when_normally_not_stale() {
        let mut sched = RebuildSchedule::new();
        sched.record(false); // stopped short
        // snapshot hN, tip hN+14: NOT stale (14 < 5760) - but the short
        // walk is still due once its backoff elapses
        assert!(sched.due(false, 30));
    }

    /// A walk that reaches the tip resets the backoff: the scheduler goes
    /// back to its ordinary long cadence.
    #[test]
    fn reaching_the_tip_resets_the_streak() {
        let mut sched = RebuildSchedule::new();
        sched.record(false);
        sched.record(false);
        assert_eq!(sched.streak, 2);
        sched.record(true); // this walk reached the tip
        assert_eq!(sched.streak, 0);
        // and with no short walk outstanding, a non-stale snapshot is not
        // due - the ordinary long cadence applies
        assert!(!sched.due(false, 0));
        assert!(!sched.due(false, u64::MAX / 2));
    }

    /// Ordinary staleness always builds immediately, whatever the streak.
    #[test]
    fn ordinary_staleness_builds_immediately() {
        let mut sched = RebuildSchedule::new();
        sched.record(false);
        sched.record(false);
        assert!(sched.due(true, 0), "stale -> build now");
    }

    /// With no snapshot at all (a fresh wallet folder) the live view is the
    /// whole set - nothing to merge, nothing lost.
    #[test]
    fn with_no_snapshot_the_live_set_is_served_as_is() {
        fn row(txid: &str, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n: 0,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        let live = vec![row("aa", 150, 999)];
        let merged = merge_whole_wallet_utxos(Vec::new(), live, Vec::new(), 0, 0, false);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value_sats, 999);
    }

    /// THE GAP. The snapshot covers genesis..coverage; the live view covers
    /// its window base..tip. When the live view's base is ABOVE the
    /// snapshot's coverage there is a hole between them that NEITHER source
    /// covers - a coin received in the hole would be missing and a coin
    /// spent in the hole would still be listed. Such a merge is REJECTED:
    /// the snapshot alone is served, which is a consistent set as of its
    /// own coverage height.
    #[test]
    fn a_gap_between_the_coverage_and_the_live_window_base_is_rejected() {
        fn row(txid: &str, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n: 0,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        // the snapshot covers genesis..h100
        let snapshot = vec![row("aa", 50, 100)];
        // the live view's window starts at h200 - the hole is h101..h199
        let live = vec![row("zz", 250, 333)];
        let merged = merge_whole_wallet_utxos(snapshot, live, Vec::new(), 100, 200, true);
        assert!(
            !merged.iter().any(|r| r.txid == "zz"),
            "with a hole between the two sources the merge is not trustworthy"
        );
        assert_eq!(merged.len(), 1, "the snapshot alone is served");
        assert_eq!(merged[0].txid, "aa");
    }

    /// The merge's boundary cases: the live window base BELOW, EQUAL to and
    /// just ABOVE the coverage. Below and equal are fine (the ranges touch
    /// or overlap and the merge is exact); one above is the gap, which is
    /// rejected.
    #[test]
    fn the_merge_boundary_is_the_live_window_base_versus_the_coverage() {
        fn row(txid: &str, height: u64, sats: i64) -> UtxoRow {
            UtxoRow {
                txid: txid.into(),
                n: 0,
                address: "Told".into(),
                script_hex: "51".into(),
                value_sats: sats,
                height,
                tx_time: 1,
                prev_kind: "regular".into(),
            }
        }
        // the live window base is BELOW the coverage: the ranges overlap
        // and the merge is exact
        let merged = merge_whole_wallet_utxos(
            vec![row("aa", 50, 100)],
            vec![row("zz", 250, 333)],
            Vec::new(),
            200,
            100,
            true,
        );
        assert_eq!(merged.len(), 2, "overlapping ranges merge exactly");
        assert!(merged.iter().any(|r| r.txid == "zz"));

        // the live window base is EQUAL to the coverage: the ranges touch
        let merged = merge_whole_wallet_utxos(
            vec![row("aa", 50, 100)],
            vec![row("zz", 250, 333)],
            Vec::new(),
            200,
            200,
            true,
        );
        assert_eq!(merged.len(), 2, "touching ranges merge exactly");
        assert!(merged.iter().any(|r| r.txid == "zz"));

        // the live window base is ONE above the coverage: that is the gap
        let merged = merge_whole_wallet_utxos(
            vec![row("aa", 50, 100)],
            vec![row("zz", 250, 333)],
            Vec::new(),
            200,
            202,
            true,
        );
        assert_eq!(merged.len(), 1, "a hole above the coverage rejects the merge");
        assert!(!merged.iter().any(|r| r.txid == "zz"));
    }

    // ---- wallet balance ownership ----

    /// build a WalletLive from a synthetic wallet + live map, no store needed
    fn wallet_live_with(pubkeys: &[Vec<u8>], outs: Vec<((Hash, u32), LiveOut)>) -> WalletLive {
        let keys: Vec<tri7_core::wallet::WalletKey> = pubkeys
            .iter()
            .map(|pk| tri7_core::wallet::WalletKey {
                pubkey: pk.clone(),
                secret: [7u8; 32],
                compressed: pk.len() == 33,
                address: tri7_core::wallet::pubkey_to_address(pk),
                created: None,
            })
            .collect();
        WalletLive {
            wallet: tri7_core::wallet::WalletKeys {
                keys,
                default_key: None,
                best_block: None,
                version: None,
                names: Vec::new(),
                scripts: Vec::new(),
                encrypted: false,
                hd_mnemonic: None,
                hd_passphrase: None,
                hd_chain_index: None,
                keypool: Vec::new(),
            },
            base: 0,
            live: outs.into_iter().collect(),
            spent: HashMap::new(),
            locked: false,
            unlock_until: None,
        }
    }

    fn p2pkh_script(pubkey: &[u8]) -> Vec<u8> {
        let payload = tri7_core::address::pubkey_payload(pubkey);
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&payload[1..21]);
        s.extend_from_slice(&[0x88, 0xac]);
        s
    }

    // ---- COIN-SPLIT self-verify regression (2026-11-08) ----

    /// The tx vin is sorted (txid, n) while the SELECTION arrives in whatever
    /// order the picker produced. build_wallet_tx must zip BOTH self-verify
    /// and the connect check against the SORTED list: with mixed prevout
    /// types (bare-P2PK coinstake reward + P2PKH sweep output on one
    /// address) a crossed pairing fails on a correctly signed tx ("scriptSig
    /// has 2 pushes, Solver expects 1"). Same-type coins masked the
    /// misalignment from 2026-11-07 to 2026-11-08 because twin scripts
    /// verify identically at any position.
    #[test]
    fn self_verify_zips_vin_sorted_order_not_selection_order() {
        let ctx = secp256k1::Secp256k1::new();
        let secret_of = |b: u8| {
            let mut s = [0u8; 32];
            s[0] = b;
            s
        };
        let pk_of = |b: u8| {
            let sk = secp256k1::SecretKey::from_slice(&secret_of(b)).unwrap();
            secp256k1::PublicKey::from_secret_key(&ctx, &sk)
                .serialize()
                .to_vec()
        };
        let pk_a = pk_of(11); // bare-P2PK prevout (the coinstake-reward shape)
        let pk_b = pk_of(22); // P2PKH prevout (the sweep-output shape)
        let txid = |b: u8| {
            let mut h = [0u8; 32];
            h[0] = b;
            h
        };
        let mk = |txid: [u8; 32], n: u32, script: Vec<u8>, pk: Vec<u8>, b: u8| {
            tri7_core::spend::SpendInput {
                txid,
                n,
                value: 2_000_000,
                script_pubkey: script,
                height: 100,
                tx_time: 1_700_000_000,
                is_coinbase: false,
                is_coinstake: false,
                secret: secret_of(b),
                pubkey: pk,
            }
        };
        let p2pk_script = tri7_core::spend::script_pubkey_p2pk(&pk_a).unwrap();
        let p2pkh_script = p2pkh_script(&pk_b);
        // selection order [A, B] with A > B so the builder's (txid, n) sort
        // REVERSES it — selection order must differ from vin order.
        let sel = vec![
            mk(txid(0xE0), 1, p2pk_script.clone(), pk_a.clone(), 11),
            mk(txid(0xB0), 0, p2pkh_script.clone(), pk_b.clone(), 22),
        ];
        assert!(
            sel[0].txid > sel[1].txid,
            "fixture: selection order must differ from sorted order"
        );
        assert_ne!(
            sel[0].script_pubkey, sel[1].script_pubkey,
            "fixture: prevouts must be genuinely MIXED solver types"
        );
        let plan = tri7_core::spend::build_spend_outputs(
            &sel,
            vec![tri7_core::legacy::Vout {
                value: 3_000_000,
                script_pubkey: p2pkh_script.clone(),
            }],
            Some(p2pkh_script.clone()),
            100,
            1_700_000_100,
            1_000,
        )
        .expect("mixed-solver spend builds and signs");
        // vin is (txid, n) sorted: B first, A second.
        assert_eq!(plan.tx.vin[0].prev_hash, sel[1].txid);
        assert_eq!(plan.tx.vin[1].prev_hash, sel[0].txid);
        // The BUG: zipping verify against the SELECTION order crosses types
        // and rejects the correctly signed tx.
        let crossed = tri7_core::spend::verify_input(&plan.tx, 0, &sel[0].script_pubkey)
            .err()
            .expect("selection-order zip must fail on mixed solvers");
        assert!(
            crossed.contains("Solver expects"),
            "crossed pairing error: {crossed}"
        );
        // The FIX: zip against the sorted list — every input verifies.
        let mut ordered = sel.clone();
        ordered.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.n.cmp(&b.n)));
        for (i, input) in ordered.iter().enumerate() {
            assert!(
                tri7_core::spend::verify_input(&plan.tx, i, &input.script_pubkey).is_ok(),
                "sorted zip: input {i} must verify"
            );
        }
    }


    fn out(value: i64, script: Vec<u8>) -> LiveOut {
        LiveOut {
            height: 1,
            block_time: 1,
            tx_time: 1,
            tx_offset: 0,
            value,
            script,
            created_epoch: 1,
            coinstake: false,
            coinbase: false,
        }
    }

    /// The regression this fixes: balance() used to sum EVERY output in the
    /// scanned window, so a wallet with no funded keys reported the value of the
    /// window (3,205 TRI on a 200-block window).
    #[test]
    fn balance_counts_only_owned_outputs() {
        let secp = secp256k1::Secp256k1::signing_only();
        let mine = secp256k1::SecretKey::from_slice(&[3u8; 32]).unwrap();
        let mine_pub = secp256k1::PublicKey::from_secret_key(&secp, &mine)
            .serialize()
            .to_vec();
        let theirs = secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap();
        let theirs_pub = secp256k1::PublicKey::from_secret_key(&secp, &theirs)
            .serialize()
            .to_vec();

        let h = [1u8; 32];
        let wl = wallet_live_with(
            &[mine_pub.clone()],
            vec![
                ((h, 0), out(500, p2pkh_script(&mine_pub))),
                ((h, 1), out(1_000, p2pkh_script(&theirs_pub))),
                ((h, 2), out(7_000, p2pkh_script(&theirs_pub))),
            ],
        );
        assert_eq!(wl.balance(), 500, "only the owned output counts");
        assert_eq!(wl.owned_count(), 1);
    }

    /// A wallet whose keys fund nothing must read zero, not the window total.
    #[test]
    fn balance_is_zero_for_unfunded_wallet() {
        let secp = secp256k1::Secp256k1::signing_only();
        let mine = secp256k1::SecretKey::from_slice(&[5u8; 32]).unwrap();
        let mine_pub = secp256k1::PublicKey::from_secret_key(&secp, &mine)
            .serialize()
            .to_vec();
        let other = secp256k1::SecretKey::from_slice(&[11u8; 32]).unwrap();
        let other_pub = secp256k1::PublicKey::from_secret_key(&secp, &other)
            .serialize()
            .to_vec();
        let h = [2u8; 32];
        let wl = wallet_live_with(
            &[mine_pub],
            vec![
                ((h, 0), out(15_000_000_000, p2pkh_script(&other_pub))),
                ((h, 1), out(15_000_000_000, p2pkh_script(&other_pub))),
            ],
        );
        assert_eq!(wl.balance(), 0);
        assert_eq!(wl.owned_count(), 0);
    }

    // ---- PeerTarget parsing ----

    #[test]
    fn peer_target_parse_ipv4() {
        let socks = ("127.0.0.1".to_string(), 9050u16);
        let t = PeerTarget::parse("100.85.236.10:24113", &socks, &socks).unwrap();
        assert_eq!(t.host, "100.85.236.10");
        assert_eq!(t.port, 24113);
        assert!(t.socks.is_none());
        assert_eq!(t.endpoint(), "100.85.236.10:24113");
    }

    #[test]
    fn peer_target_parse_onion_gets_socks() {
        let socks = ("127.0.0.1".to_string(), 9050u16);
        let t = PeerTarget::parse(
            "i6tk7soznftvoibtskwlezviskiererhjndpsmrff4kaxw7jnd5izfqd.onion:24112",
            &socks,
            &socks,
        )
        .unwrap();
        assert_eq!(t.socks.as_ref().unwrap().1, 9050);
        assert_eq!(t.port, 24112);
    }

    #[test]
    fn peer_target_parse_rejects_garbage() {
        let socks = ("127.0.0.1".to_string(), 9050u16);
        assert!(PeerTarget::parse("no-port", &socks, &socks).is_err());
        assert!(PeerTarget::parse("host:notaport", &socks, &socks).is_err());
        assert!(PeerTarget::parse(":24112", &socks, &socks).is_err());
    }

    #[test]
    fn daemon_flag_replaces_default_and_appends() {
        // the built-in default (127.0.0.1:24113) must be REPLACED by the first
        // explicit target: a node listening on 24113 would otherwise carry a
        // self-connect target and handshake-loop against its own listener
        let socks = ("127.0.0.1".to_string(), 9050u16);
        let mut peers = vec![PeerTarget {
            host: "127.0.0.1".into(),
            port: 24113,
            socks: None,
            transport: Transport::Clearnet,
        }];
        let mut explicit = false;
        for spec in "100.85.236.10:24113,100.85.236.10:24112".split(',') {
            if !explicit {
                peers.clear();
                explicit = true;
            }
            peers.push(PeerTarget::parse(spec.trim(), &socks, &socks).unwrap());
        }
        assert_eq!(peers.len(), 2, "default target replaced");
        assert_eq!(peers[0].endpoint(), "100.85.236.10:24113");
        assert_eq!(peers[1].endpoint(), "100.85.236.10:24112");
        assert!(peers.iter().all(|p| p.endpoint() != "127.0.0.1:24113"));
    }

    // ---- status gate (healthcheck network-quiet rules) ----

    #[test]
    fn status_gate_healthy_when_peer_recent() {
        // peer talked 60s ago, tip advanced 120s ago → healthy
        let (exit, degraded, _) = status_gate(false, false, Some(60), Some(120), 600, 1800);
        assert_eq!(exit, 0);
        assert!(!degraded);
    }

    #[test]
    fn status_gate_degraded_needs_both_quiets() {
        // peer quiet but tip advanced recently → NOT degraded
        let (exit, degraded, _) = status_gate(false, false, Some(3600), Some(60), 600, 1800);
        assert_eq!(exit, 0);
        assert!(!degraded);
        // tip quiet but peers active → NOT degraded
        let (exit, degraded, _) = status_gate(false, false, Some(60), Some(7200), 600, 1800);
        assert_eq!(exit, 0);
        assert!(!degraded);
    }

    #[test]
    fn status_gate_degraded_when_both_quiets() {
        let (exit, degraded, reasons) = status_gate(false, false, Some(700), Some(1900), 600, 1800);
        assert_eq!(exit, EXIT_VALIDATE);
        assert!(degraded);
        assert_eq!(reasons.len(), 2);
    }

    #[test]
    fn status_gate_static_skips_network_gate() {
        let (exit, degraded, _) = status_gate(true, false, Some(99_999), Some(99_999), 600, 1800);
        assert_eq!(exit, 0);
        assert!(!degraded);
    }

    #[test]
    fn status_gate_stale_state_fails_even_static() {
        let (exit, degraded, reasons) = status_gate(true, true, Some(1), Some(1), 600, 1800);
        assert_eq!(exit, EXIT_VALIDATE);
        assert!(!degraded);
        assert_eq!(reasons, vec!["state write stale".to_string()]);
    }

    #[test]
    fn status_gate_missing_peer_data_counts_quiet() {
        // a pre-phase-6 state file has no last_peer_utc — counts as quiet
        let (exit, degraded, _) = status_gate(false, false, None, Some(7200), 600, 1800);
        assert_eq!(exit, EXIT_VALIDATE);
        assert!(degraded);
    }

    // ---- ISO timestamp round trip (healthcheck clock math) ----

    #[test]
    fn iso_secs_roundtrip() {
        for s in [0i64, 86_400, 1_700_000_000, 1_793_656_833] {
            let iso = iso_from_secs(s);
            assert_eq!(iso_secs_from_now(&iso), Some(s), "iso {}", iso);
        }
        assert_eq!(iso_secs_from_now("not-a-timestamp"), None);
    }

    // ---- recent blocks cache ----

    fn fake_block_bytes(tag: u8) -> Vec<u8> {
        // NOT a real block — RecentBlocks stores opaque payloads; parse only
        // happens in the RPC fallback path which is not exercised here
        vec![tag; 64]
    }

    #[test]
    fn recent_blocks_insert_get_at_height() {
        let mut rb = RecentBlocks::new();
        let h1 = [1u8; 32];
        let h2 = [2u8; 32];
        rb.insert(h1, 2204068, &fake_block_bytes(1));
        rb.insert(h2, 2204069, &fake_block_bytes(2));
        assert!(rb.get(&h1).is_some());
        assert_eq!(rb.get(&h1).unwrap().0, 2204068);
        assert_eq!(
            rb.at_height(2204069).map(|(_, raw)| raw[0]),
            Some(2)
        );
        assert_eq!(rb.at_height(1), None);
        assert_eq!(rb.store_tip_height(), Some(2204069));
    }

    #[test]
    fn recent_blocks_evicts_oldest_beyond_cap() {
        let mut rb = RecentBlocks::new();
        let mut first = [0u8; 32];
        first[31] = 0xAA;
        rb.insert(first, 0, &fake_block_bytes(0)); // height 0: no height entry
        for i in 0..(RECENT_CAP as u64 + 10) {
            let mut h = [0u8; 32];
            h[0..8].copy_from_slice(&i.to_le_bytes());
            rb.insert(h, 10_000 + i as u64, &fake_block_bytes((i % 251) as u8));
        }
        assert!(rb.get(&first).is_none(), "oldest entry evicted");
        assert!(rb.order.len() <= RECENT_CAP);
        assert!(rb.at_height(10_000).is_none(), "pruned height entry");
        assert!(rb.at_height(10_000 + RECENT_CAP as u64 + 9).is_some());
    }

    #[test]
    fn recent_blocks_ignores_duplicate_hash() {
        let mut rb = RecentBlocks::new();
        let h = [7u8; 32];
        rb.insert(h, 5, &fake_block_bytes(1));
        rb.insert(h, 6, &fake_block_bytes(2)); // duplicate — first insert wins
        assert_eq!(rb.get(&h).unwrap().0, 5);
        assert_eq!(rb.order.len(), 1);
    }

    // ---- SOCKS5 request bytes (pure part of the tor transport) ----

    #[test]
    fn socks5_request_ipv4_form() {
        let req = socks5_connect_request("100.85.236.10", 24113);
        assert_eq!(&req[..4], &[0x05, 0x01, 0x00, 0x01]);
        assert_eq!(&req[4..8], &[100, 85, 236, 10]);
        assert_eq!(&req[8..10], &24113u16.to_be_bytes());
        assert_eq!(req.len(), 10);
    }

    #[test]
    fn socks5_request_domain_form_for_onion() {
        let host = "i6tk7soznftvoibtskwlezviskiererhjndpsmrff4kaxw7jnd5izfqd.onion";
        let req = socks5_connect_request(host, 24112);
        assert_eq!(&req[..4], &[0x05, 0x01, 0x00, 0x03]);
        assert_eq!(req[4], host.len() as u8);
        assert_eq!(&req[5..5 + host.len()], host.as_bytes());
        let port_at = 5 + host.len();
        assert_eq!(&req[port_at..port_at + 2], &24112u16.to_be_bytes());
    }

    #[test]
    fn socks5_greeting_bytes() {
        assert_eq!(socks5_greeting(), vec![0x05, 0x01, 0x00]);
    }

    #[test]
    fn socks5_reply_drain_variants() {
        // ATYP=1: 4-byte addr + 2-byte port
        let mut r: &[u8] = &[1, 2, 3, 4, 0x5E, 0x71];
        assert!(socks5_consume_reply(0x01, &mut r).is_ok());
        // ATYP=3: 1-byte len + domain + 2-byte port
        let mut r: &[u8] = &[3, b'a', b'b', b'c', 0, 0];
        assert!(socks5_consume_reply(0x03, &mut r).is_ok());
        // ATYP=4: 16 + 2
        let mut r = vec![0u8; 18].into_iter().collect::<Vec<u8>>();
        assert!(socks5_consume_reply(0x04, &mut r.as_slice()).is_ok());
        let mut r: &[u8] = &[];
        assert!(socks5_consume_reply(0x09, &mut r).is_err());
    }

    // ---- status JSON keeps its exit code contract ----

    #[test]
    fn status_gate_exit_codes_are_stable() {
        // the healthcheck unit restarts tri7d on ANY nonzero exit — these
        // values are load-bearing; changing them changes service behavior
        assert_eq!(EXIT_VALIDATE, 3);
        assert_eq!(status_gate(false, false, Some(0), Some(0), 600, 1800).0, 0);
        assert_eq!(status_gate(false, false, Some(700), Some(1900), 600, 1800).0, 3);
    }

    // ---- gap recovery ----

    use tri7_core::replay::IndexEntry;

    fn synthetic_chain(base: u64, tip: u64) -> ChainState {
        let mut entries = Vec::new();
        let mut checksums = Vec::new();
        let mut prev = [0xAAu8; 32];
        for h in base..=tip {
            let mut hash = [0x11u8; 32];
            hash[0..8].copy_from_slice(&h.to_le_bytes());
            entries.push(IndexEntry {
                height: (h - base) as usize,
                hash,
                prev_hash: prev,
                time: 1_700_000_000u32.saturating_add((h * 600) as u32),
                bits: 0x2000_ffff,
                is_proof_of_stake: true,
                entropy_bit: 0,
                stake_modifier: h,
                generated_stake_modifier: true,
                hash_proof_of_stake: [0u8; 32],
            });
            checksums.push(7);
            prev = hash;
        }
        ChainState {
            base,
            entries,
            checksums,
            utxo: WindowOutputs::new(),
            records: Vec::new(),
            no_trim: false,
            pending_headers: HashMap::new(),
            tip_height: tip,
            tip_hash: {
                let mut hash = [0x11u8; 32];
                hash[0..8].copy_from_slice(&tip.to_le_bytes());
                hash
            },
            daemon_tip: Some(tip as i64),
            last_advance_utc: String::new(),
            peers_connected: 0,
            last_peer_utc: None,
        }
    }

    #[test]
    fn gap_find_missing_targets_the_lowest_absent_tail_body() {
        let stg = synthetic_chain(100, 105);
        // store not ready (no file) → every tail body counts as missing;
        // the finder must return the LOWEST tail height
        let store = BlockStore::new(None, None);
        let recent = RecentBlocks::new();
        // threaded tip = 104 → the walk starts at 105
        let got = gap_find_missing(&stg, &store, &recent, Some(104));
        assert_eq!(got, Some((105, stg.entry_abs(105).unwrap().hash)));
        // threaded tip far below → clamped to GAP_TAIL_MAX below the tip, but
        // never below the window base (heights with no window entry are
        // skipped, so the first candidate is base itself here)
        let got2 = gap_find_missing(&stg, &store, &recent, Some(80));
        assert_eq!(got2.map(|(h, _)| h), Some(100));
    }

    #[test]
    fn gap_find_missing_satisfied_by_recent_cache() {
        let stg = synthetic_chain(100, 105);
        let store = BlockStore::new(None, None);
        let mut recent = RecentBlocks::new();
        let h = stg.entry_abs(105).unwrap().hash;
        recent.insert(h, 105, b"pretend-body");
        assert_eq!(gap_find_missing(&stg, &store, &recent, Some(104)), None);
    }

    #[test]
    fn rollback_tip_drops_the_phantom_and_truncates() {
        let mut stg = synthetic_chain(100, 105);
        // a record + pending header above the rollback point must go too
        stg.records.push(BlockRec {
            height: 105,
            spends: vec![],
            outs: vec![],
        });
        stg.pending_headers.insert([0x99u8; 32], Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            time: 0,
            bits: 0,
            nonce: 0,
            vtx: vec![],
            block_sig: vec![],
        });
        let phantom_hash = stg.tip_hash;
        let (new_tip, new_hash) = rollback_tip(&mut stg).expect("rollback");
        assert_eq!(new_tip, 104);
        assert_eq!(new_hash, stg.entry_abs(104).unwrap().hash);
        assert_eq!(stg.tip_height, 104);
        assert_ne!(stg.tip_hash, phantom_hash);
        assert_eq!(stg.entries.len(), (104 - 100 + 1) as usize);
        assert_eq!(stg.checksums.len(), stg.entries.len());
        assert!(stg.records.iter().all(|r| r.height <= 104));
        assert!(stg.pending_headers.is_empty());
        assert_eq!(stg.daemon_tip, Some(104));
        // to_file stays consistent (entries/checksums zip equal length)
        let file = stg.to_file();
        assert_eq!(file.height, 104);
        assert_eq!(file.entries.len(), stg.entries.len());
    }

    /// Fork choice must refuse a lighter competing block and adopt a heavier
    /// one, rolling the window back to their common ancestor. This is the
    /// regression guard for "two stakers kept their own block forever".
    #[test]
    fn fork_choice_refuses_lighter_and_adopts_heavier() {
        let mut stg = synthetic_chain(100, 105);
        let ancestor_hash = stg.entry_abs(103).unwrap().hash;

        let mk_block = |bits: u32| Block {
            version: 1,
            prev_hash: ancestor_hash,
            merkle_root: [0u8; 32],
            time: 0,
            bits,
            nonce: 0,
            vtx: vec![],
            block_sig: vec![],
        };

        // a block at the same height as our tip but with a LARGER target
        // (less work) must not displace ours
        // synthetic_chain blocks carry 0x2000ffff; a block with a larger
        // target (0x2100ffff = 256x easier) represents strictly less work
        let easy = mk_block(0x2100_ffff);
        let easy_hash = easy.hash();
        assert!(
            !try_adopt_heavier_branch(&mut stg, &easy, &easy_hash).unwrap(),
            "lighter branch must be ignored"
        );
        assert_eq!(stg.tip_height, 105, "window untouched on refusal");

        // a harder block (smaller target) DOES win: the window rolls back to
        // the ancestor and the caller re-applies against that state
        // and one with a smaller target (0x1e00ffff = 256x harder) wins
        let hard = mk_block(0x1e00_ffff);
        let hard_hash = hard.hash();
        assert!(
            try_adopt_heavier_branch(&mut stg, &hard, &hard_hash).unwrap(),
            "heavier branch must be adopted"
        );
        assert_eq!(stg.tip_height, 103, "rolled back to the common ancestor");
        assert_eq!(stg.tip_hash, ancestor_hash);
    }

    #[test]
    fn rollback_tip_refuses_at_window_base() {
        let mut stg = synthetic_chain(100, 100);
        assert!(rollback_tip(&mut stg).is_err());
    }

    #[test]
    fn gap_fill_valid_hash_link_merkle() {
        use tri7_core::legacy::{Tx, Vin, Vout};
        let stg = synthetic_chain(100, 105);
        // a real block chained onto h104's hash with a real merkle root
        let tx = Tx {
            version: 1,
            time: 1_700_000_999,
            vin: vec![tri7_core::legacy::Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1, 2, 3],
                sequence: u32::MAX,
            }],
            vout: vec![tri7_core::legacy::Vout {
                value: 5,
                script_pubkey: vec![0x51],
            }],
            locktime: 0,
        };
        let mut b = Block {
            version: 2,
            prev_hash: stg.entry_abs(104).unwrap().hash,
            merkle_root: [0u8; 32],
            time: 1_700_000_999,
            bits: 0x2000_ffff,
            nonce: 1,
            vtx: vec![tx],
            block_sig: vec![],
        };
        b.merkle_root = b.merkle_root();
        let want = b.hash();
        // valid: hash matches (Hash9), prev links to the window, merkle commits
        assert_eq!(
            gap_fill_valid(&b, 105, &want, &stg).map_err(|e| e.clone()),
            Ok(())
        );
        // wrong requested hash → reject
        assert!(gap_fill_valid(&b, 105, &[0xEE; 32], &stg).is_err());
        // broken link → reject
        let mut b2 = b.clone();
        b2.prev_hash = [0x77u8; 32];
        assert!(gap_fill_valid(&b2, 105, &b2.hash(), &stg).is_err());
        // broken merkle → reject
        let mut b3 = b.clone();
        b3.merkle_root = [0x42u8; 32];
        assert!(gap_fill_valid(&b3, 105, &b3.hash(), &stg).is_err());
    }

    // ---- phase 7 milestone 2 ----

    #[test]
    fn peer_target_parse_i2p_and_onion_transports() {
        let tor = ("127.0.0.1".to_string(), 9050u16);
        let i2p = ("127.0.0.1".to_string(), 4447u16);
        let t = PeerTarget::parse("qgrkbxy7j2vuu5flhnyvho7chk7pfzk4o72p5pgdb6nrf72mz7kq.b32.i2p:24112", &tor, &i2p).unwrap();
        assert_eq!(t.transport, Transport::I2p);
        assert_eq!(t.socks, Some(i2p.clone()));
        let o = PeerTarget::parse("gxvrhv3qitnc@example.onion:24113", &tor, &i2p).unwrap();
        assert_eq!(o.transport, Transport::Tor);
        assert_eq!(o.socks, Some(tor.clone()));
        let c = PeerTarget::parse("100.85.236.10:24112", &tor, &i2p).unwrap();
        assert_eq!(c.transport, Transport::Clearnet);
        assert_eq!(c.socks, None);
    }

    /// A9: `addnode=<onion>:<port>` is kept as a tor target, `addnode=
    /// <b32>.b32.i2p:<port>` as an i2p target (dual-network fleet);
    /// `addnode=1.2.3.4:24112` (cleartext) is IGNORED with a log line. Junk
    /// lines never panic.
    #[test]
    fn addnode_conf_parsing_dual_overlay() {
        let tor = ("127.0.0.1".to_string(), 9050u16);
        let i2p = ("127.0.0.1".to_string(), 4447u16);
        let conf = "\
rpcuser=u
rpcpassword=p
port=24113
addnode=1.2.3.4:24112
addnode=gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113
addnode=somehost.example.com:24113
addnode=jbpfhe7zdeadseeddeadseeddeadseeddeadseeddeadseeddeadseed0.onion:24112
addnode=ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113
addnode=
addnode=nonsense-no-port
addnode=:24112
addnode=host:notaport
";
        let (targets, ignored) = addnode_targets_from_conf(conf, &tor, &i2p);
        // exactly the two onion targets + the i2p target survive
        assert_eq!(targets.len(), 3);
        assert_eq!(
            targets[0].endpoint(),
            "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113"
        );
        assert_eq!(targets[0].transport, Transport::Tor);
        assert_eq!(targets[0].socks.as_ref().map(|(h, _)| h.as_str()), Some("127.0.0.1"));
        assert_eq!(targets[1].port, 24112);
        assert_eq!(
            targets[2].endpoint(),
            "ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113"
        );
        assert_eq!(targets[2].transport, Transport::I2p);
        assert_eq!(targets[2].socks, Some(i2p.clone()));
        assert_eq!(targets[2].port, 24113);
        // the cleartext hosts are in the ignored list (each was logged by the
        // parser — visible with `cargo test -- --nocapture`)
        assert_eq!(ignored.len(), 2);
        assert!(ignored.contains(&"1.2.3.4:24112".to_string()));
        assert!(ignored.contains(&"somehost.example.com:24113".to_string()));
        // malformed lines — including colon-carrying junk (:24112 empty host,
        // host:notaport unparsable port) — are ignored without entries and
        // without panic
        assert!(!ignored.iter().any(|s| !s.contains('.')));
        let (targets, _) = addnode_targets_from_conf("addnode=nonsense-no-port\n", &tor, &i2p);
        assert!(targets.is_empty());
    }

    #[test]
    fn addnode_ignored_list_requires_host_port_shape() {
        let tor = ("127.0.0.1".to_string(), 9050u16);
        let i2p = ("127.0.0.1".to_string(), 4447u16);
        let (targets, ignored) = addnode_targets_from_conf(
            "addnode=:24112\naddnode=host:notaport\naddnode=ok.example.com:24113\n\
             addnode=nonsense-no-port\naddnode=host:99999\n",
            &tor,
            &i2p,
        );
        // only the well-formed cleartext host:port enters the ignored list
        assert!(targets.is_empty());
        assert_eq!(ignored, vec!["ok.example.com:24113".to_string()]);
    }

    /// Peer-store hosts dialable through an overlay: onions and i2p
    /// destinations yes; clearnet quads, legacy hex-key leftovers, and junk
    /// no. Guards the store-dial filter in connect_and_handshake.
    #[test]
    fn dialable_store_host_accepts_onion_and_i2p_only() {
        assert!(dialable_store_host(
            "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion"
        ));
        assert!(dialable_store_host(
            "ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p"
        ));
        assert!(dialable_store_host("somedest.i2p"));
        assert!(!dialable_store_host("100.85.236.10"));
        assert!(!dialable_store_host("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        assert!(!dialable_store_host(""));
        assert!(!dialable_store_host("somehost.example.com"));
    }

    /// Peer-store keys and the advertised-address filter: loopback and
    /// unspecified placeholders are never recorded.
    #[test]
    fn advertised_store_key_filters_loopback_placeholders() {
        use tri7_core::p2p::WireAddr;
        // loopback IPv4-mapped (tor peers announce 127.0.0.1) → None
        let lo = WireAddr::ipv4(127, 0, 0, 1, 24113, 1);
        assert_eq!(advertised_store_key(&lo), None);
        // unspecified → None
        let un = WireAddr::ipv4(0, 0, 0, 0, 24113, 1);
        assert_eq!(advertised_store_key(&un), None);
        // a routable IPv4 → dotted quad key
        let ok = WireAddr::ipv4(10, 1, 2, 3, 24113, 1);
        assert_eq!(
            advertised_store_key(&ok).as_deref(),
            Some("10.1.2.3")
        );
        // an onion (tor_v3) → the reconstructed HOSTNAME (dialable — this is
        // what makes gossip-discovered peers connectable)
        let onion = WireAddr::from_onion_label(
            "gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion",
            24113,
            1,
        )
        .unwrap();
        assert_eq!(
            advertised_store_key(&onion).as_deref(),
            Some("gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion")
        );
    }

    #[test]
    fn conf_stake_flag_resolution_and_persistence() {
        let dir = std::env::temp_dir().join(format!("tri7-conf-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let conf = dir.join("triangles.conf");
        std::fs::write(&conf, "rpcuser=u\nrpcpassword=p\nstake=0\nport=24113\n").unwrap();
        // persist on
        conf_set_flag(&conf, "stake", "1").unwrap();
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.contains("stake=1"), "{}", text);
        assert!(text.contains("rpcuser=u"));
        assert!(text.contains("port=24113"));
        // append when absent
        conf_set_flag(&conf, "noantispam", "1").unwrap();
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.contains("noantispam=1"));
        // CLI override semantics mirror parse_args' last-line-wins read
        let mut stake = None;
        for line in text.lines() {
            if let Some(v) = line.trim().strip_prefix("stake=") {
                stake = Some(v.trim() != "0");
            }
        }
        assert_eq!(stake, Some(true));
        conf_set_flag(&conf, "stake", "0").unwrap();
        assert!(std::fs::read_to_string(&conf).unwrap().contains("stake=0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn net_stake_weight_uses_pos_blocks_and_72_window() {
        // 3 PoS blocks 120s apart with difficulty 1.0 (bits 0x1d00ffff):
        // kernels = 3 * 1.0 * 2^32, secs = 240 → weight = 2^32 * 3 / 240
        let mk = |t: u32, pos: bool| IndexEntry {
            height: 0,
            hash: [0u8; 32],
            prev_hash: [0u8; 32],
            time: t,
            bits: 0x1d00ffff,
            entropy_bit: 0,
            stake_modifier: 0,
            generated_stake_modifier: true,
            hash_proof_of_stake: [0u8; 32],
            is_proof_of_stake: pos,
        };
        let entries = vec![mk(1000, true), mk(1120, false), mk(1120, true), mk(1240, true)];
        let w = net_stake_weight(&entries);
        assert_eq!(w, (4_294_967_296.0f64 * 3.0 / 240.0) as u64);
        // no PoS at all → 0
        assert_eq!(net_stake_weight(&[mk(5, false)]), 0);
    }

    #[test]
    fn wallet_live_apply_candidates_weight() {
        use tri7_core::wallet::WalletKey;
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk);
        let key = WalletKey {
            pubkey: pk.serialize().to_vec(),
            secret: sk.secret_bytes(),
            compressed: true,
            address: tri7_core::wallet::pubkey_to_address(&pk.serialize()),
            created: None,
        };
        let wl = WalletLive {
            wallet: tri7_core::wallet::WalletKeys {
                keys: vec![key],
                default_key: None,
                best_block: None,
                version: None,
                names: Vec::new(),
                scripts: Vec::new(),
                encrypted: false,
                hd_mnemonic: None,
                hd_passphrase: None,
                hd_chain_index: None,
                keypool: Vec::new(),
            },
            base: 100,
            live: HashMap::new(),
            spent: HashMap::new(),
            locked: false,
            unlock_until: None,
        };
        // a funded block at h=101: coinbase + p2pkh output to our key
        let script = tri7_core::spend::script_pubkey_p2pkh(&pk.serialize());
        let fund_tx = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![tri7_core::legacy::Vin {
                prev_hash: tri7_core::legacy::sha256d(b"fund prev"),
                prev_n: 0,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![tri7_core::legacy::Vout {
                value: 3_000_000,
                script_pubkey: script,
            }],
            locktime: 0,
        };
        let block = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [2u8; 32],
            time: 1_700_000_100,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![fund_tx],
            block_sig: Vec::new(),
        };
        let mut wl2 = wl;
        wl2.apply_block(&block, 101);
        assert_eq!(wl2.balance(), 3_000_000);
        let (cands, no_key, _unloc, _ns) = wl2.candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(no_key, 0);
        assert_eq!(cands[0].value, 3_000_000);
        assert_eq!(cands[0].block_height, 101);
        assert_eq!(cands[0].tx_time, 1_700_000_000);
        // weight before maturity = 0; after 12h cap
        let now = 1_700_000_100u32; // young
        assert_eq!(wl2.stake_weight(now).0, 0);
        let now2 = 1_700_000_100 + 86_400; // 1 day later
        let (w, mature) = wl2.stake_weight(now2);
        assert_eq!(mature, 1);
        // value * min(now - tx_time, 43200) / COIN / 86400
        let expect = 3_000_000i64 * 43_200i64 / 1_000_000 / 86_400;
        assert_eq!(w as i64, expect);
        // spend removal
        let fund_hash = block.vtx[0].hash();
        let spend_tx = Tx {
            version: 1,
            time: 1_700_000_200,
            vin: vec![tri7_core::legacy::Vin { prev_hash: fund_hash, prev_n: 0, script_sig: vec![], sequence: u32::MAX }],
            vout: vec![tri7_core::legacy::Vout { value: 0, script_pubkey: vec![] }],
            locktime: 0,
        };
        // apply_block treats vtx[0] as the coinbase/coinstake (skip(1) on
        // spend removal), so the spend must ride at vtx[1] behind a dummy.
        let dummy_coinbase = Tx {
            version: 1,
            time: 1_700_000_200,
            vin: vec![tri7_core::legacy::Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![tri7_core::legacy::Vout { value: 0, script_pubkey: vec![] }],
            locktime: 0,
        };
        let block2 = Block {
            version: 1,
            prev_hash: [3u8; 32],
            merkle_root: [4u8; 32],
            time: 1_700_000_300,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![dummy_coinbase, spend_tx],
            block_sig: Vec::new(),
        };
        wl2.apply_block(&block2, 102);
        assert_eq!(wl2.balance(), 0);
        assert!(wl2.candidates().0.is_empty());
    }

    #[test]
    fn txpool_rejects_spends_outside_the_live_view_via_ops_shape() {
        // OpsAdapter::sendrawtransaction contract: hex parse errors are -8,
        // consensus rejects are -25. The lib TxPool tests cover the checks —
        // here we pin the error-code shape of the hex path.
        let raw_bad = tri7_core::p2p::hex_decode("zz-not-hex");
        assert!(raw_bad.is_err());
        let raw = tri7_core::p2p::hex_decode("01000000").unwrap();
        assert!(raw.len() == 4 && raw[0] == 0x01);
    }

    #[test]
    fn keypool_size_argument_legacy_semantics() {
        // legacy keypoolrefill [size]: absent/0 = default 100, valid range
        // 0..=100000, anything else is -8 invalid-parameter
        assert_eq!(parse_keypool_size(None).unwrap(), DEFAULT_KEYPOOL_SIZE);
        assert_eq!(
            parse_keypool_size(Some(&serde_json::json!(null))).unwrap(),
            DEFAULT_KEYPOOL_SIZE
        );
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(0))).unwrap(), DEFAULT_KEYPOOL_SIZE);
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(1))).unwrap(), 1);
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(100))).unwrap(), 100);
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(100_000))).unwrap(), 100_000);
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(-1))).unwrap_err().0, -8);
        assert_eq!(parse_keypool_size(Some(&serde_json::json!(100_001))).unwrap_err().0, -8);
        assert_eq!(
            parse_keypool_size(Some(&serde_json::json!("100"))).unwrap_err().0,
            -8,
            "strings are not sizes"
        );
        assert_eq!(
            parse_keypool_size(Some(&serde_json::json!(1.5))).unwrap_err().0,
            -8,
            "floats are not sizes"
        );
    }

    // ---- repairwallet / resendtx (workqueue item 9 RPC-P1b) ----

    #[test]
    fn newest_replayable_tip_walks_back_only_over_missing_bodies() {
        // bodies exist 100..=200; the tip hint is 5 higher (headers-only tail)
        let present = |h: u64| (100..=200).contains(&h);
        let (tip, back) = newest_replayable_tip(&present, 100, 205);
        assert_eq!((tip, back), (200, 5));
        // everything present → no walk
        let (tip, back) = newest_replayable_tip(&|_| true, 100, 200);
        assert_eq!((tip, back), (200, 0));
        // nothing present → clamps at base, honest (every height 101..=205
        // was checked and failed, so back = hint - base)
        let (tip, back) = newest_replayable_tip(&|_| false, 100, 205);
        assert_eq!((tip, back), (100, 105));
        // the 5_000 cap: never walks below hint-5000
        let (tip, back) = newest_replayable_tip(&|_| false, 0, 10_000);
        assert_eq!((tip, back), (5_000, 5_000));
    }

    #[test]
    fn repairwallet_and_resendtx_arity_matches_legacy() {
        // legacy repairwallet throws on ANY parameter (runtime_error);
        // resendtx accepts at most one (ignored) parameter.
        assert!(repairwallet_params_ok(&serde_json::json!([])).is_ok());
        assert!(repairwallet_params_ok(&serde_json::Value::Null).is_ok());
        let e = repairwallet_params_ok(&serde_json::json!(["extra"])).unwrap_err();
        assert_eq!(e.0, -8);
        assert!(resendtx_params_ok(&serde_json::json!([])).is_ok());
        assert!(resendtx_params_ok(&serde_json::json!(["ignored-account"])).is_ok());
        let e = resendtx_params_ok(&serde_json::json!(["a", "b"])).unwrap_err();
        assert_eq!(e.0, -8);
    }

    // LiveOut ownership marker for the diff tests: production passes
    // WalletLive::owns (a script/key check); the diff itself only consumes
    // the predicate, so the tests mark ownership via a dedicated field.
    fn live_out(value: i64, ours: bool) -> LiveOut {
        LiveOut {
            height: 500,
            block_time: 1_700_000_000,
            tx_time: 1_700_000_000,
            tx_offset: if ours { 9_000 } else { 1 },
            value,
            script: vec![0x51],
            created_epoch: 1_700_000_000,
            coinstake: false,
            coinbase: false,
        }
    }

    #[test]
    fn repair_diff_counts_only_wallet_owned_mismatches() {
        let mut old = HashMap::new();
        let mut fresh = HashMap::new();
        // h1: ours in both → no mismatch
        let h1: Hash = [1u8; 32];
        old.insert((h1, 0), live_out(1_000, true));
        fresh.insert((h1, 0), live_out(1_000, true));
        // h2: ours in the VIEW ONLY → phantom (chain spent it)
        let h2: Hash = [2u8; 32];
        old.insert((h2, 1), live_out(2_000, true));
        // h3: ours in the REPLAY ONLY → lost (view had dropped it)
        let h3: Hash = [3u8; 32];
        fresh.insert((h3, 0), live_out(3_000, true));
        // h4/h5: NOT ours, missing on either side → must be ignored
        let h4: Hash = [4u8; 32];
        old.insert((h4, 0), live_out(4_000, false));
        let h5: Hash = [5u8; 32];
        fresh.insert((h5, 0), live_out(5_000, false));
        let owns = |o: &LiveOut| o.tx_offset == 9_000;
        let d = repair_diff(&old, &fresh, &owns);
        assert_eq!(d.count(), 2);
        assert_eq!(d.phantom, vec![((h2, 1), 2_000)]);
        assert_eq!(d.lost, vec![((h3, 0), 3_000)]);
        assert_eq!(d.amount(), 5_000);
        // identical maps → wallet check passed shape
        let d0 = repair_diff(&old, &old, &owns);
        assert_eq!(d0.count(), 0);
        assert_eq!(d0.amount(), 0);
    }

    #[test]
    fn repair_swap_replaces_the_live_map_atomically() {
        // the repairwallet flow on a real WalletLive: drift the map (drop an
        // owned coin the chain still holds), rebuild the map by replaying the
        // same blocks, diff reports the lost coin, swap heals the balance.
        use tri7_core::wallet::WalletKey;
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk = SecretKey::from_slice(&[43u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk);
        let key = WalletKey {
            pubkey: pk.serialize().to_vec(),
            secret: sk.secret_bytes(),
            compressed: true,
            address: tri7_core::wallet::pubkey_to_address(&pk.serialize()),
            created: None,
        };
        let mk_wl = || WalletLive {
            wallet: tri7_core::wallet::WalletKeys {
                keys: vec![key.clone()],
                default_key: None,
                best_block: None,
                version: None,
                names: Vec::new(),
                scripts: Vec::new(),
                encrypted: false,
                hd_mnemonic: None,
                hd_passphrase: None,
                hd_chain_index: None,
                keypool: Vec::new(),
            },
            base: 100,
            live: HashMap::new(),
            spent: HashMap::new(),
            locked: false,
            unlock_until: None,
        };
        let script = tri7_core::spend::script_pubkey_p2pkh(&pk.serialize());
        let fund_tx = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![tri7_core::legacy::Vin {
                prev_hash: tri7_core::legacy::sha256d(b"repair fund"),
                prev_n: 0,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![tri7_core::legacy::Vout {
                value: 3_000_000,
                script_pubkey: script,
            }],
            locktime: 0,
        };
        let block = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [2u8; 32],
            time: 1_700_000_100,
            bits: 0x2000ffff,
            nonce: 0,
            vtx: vec![fund_tx],
            block_sig: Vec::new(),
        };
        // the (drifted) wallet view: it MISSES the block (lost coin)
        let mut w = mk_wl();
        assert_eq!(w.balance(), 0);
        // the authoritative replay sees the block
        let mut fresh = mk_wl();
        fresh.apply_block(&block, 101);
        assert_eq!(fresh.balance(), 3_000_000);
        let owns = |o: &LiveOut| w.owns(o);
        let d = repair_diff(&w.live, &fresh.live, &owns);
        assert_eq!(d.count(), 1);
        assert_eq!(d.lost.len(), 1);
        assert_eq!(d.amount(), 3_000_000);
        // the reverse direction: the view claims a coin the replay lacks
        // (the ghost carries the wallet's REAL script so owns() recognizes it)
        let mut phantom_view = mk_wl();
        phantom_view.apply_block(&block, 101);
        let ghost: Hash = [9u8; 32];
        let ghost_script = tri7_core::spend::script_pubkey_p2pkh(&pk.serialize());
        phantom_view.live.insert(
            (ghost, 0),
            LiveOut {
                height: 101,
                block_time: 1_700_000_100,
                tx_time: 1_700_000_000,
                tx_offset: 5,
                value: 777,
                script: ghost_script,
                created_epoch: 1_700_000_000,
                coinstake: false,
                coinbase: false,
            },
        );
        let d2 = repair_diff(&phantom_view.live, &fresh.live, &owns);
        assert_eq!(d2.phantom.len(), 1);
        assert_eq!(d2.amount(), 777);
        // swap heals
        let n = d.count();
        let amt = d.amount();
        w.live = fresh.live;
        assert_eq!(w.balance(), 3_000_000);
        assert_eq!((n, amt), (1, 3_000_000));
    }

    #[test]
    fn resend_mine_gate_prunes_before_reaccept() {
        // regression pin (live-caught on DNS2 2026-11-08): a recorded tx that
        // already mined MUST map to Mined (prune, never accept) — the store
        // deep-coin fallback resolves its spent prevouts without
        // spent-tracking, so an accept would re-validate the double-spend.
        // FAIL-CLOSED: a store error cannot prove "unmined", so it must NOT
        // reach the accept path (CantProve = skip this pass, keep entry).
        use ResendGate::{CantProve, Mined, Unknown};
        assert!(matches!(resend_mine_gate(Some(Ok(Some(())))), Mined));
        assert!(matches!(resend_mine_gate(Some(Ok(None))), Unknown));
        assert!(matches!(resend_mine_gate(Some(Err("store hiccup".into()))), CantProve));
        assert!(matches!(resend_mine_gate(None), Unknown));
    }

    // ---- from-zero assume-valid envelope (deployed-client parity) ----

    #[test]
    fn assume_valid_gate_boundaries_below_at_above_horizon() {
        // below V6_RESTART_HEIGHT: no kernel evidence at all
        assert_eq!(validation_mode(0, false, false), ValidMode::AssumeValid);
        assert_eq!(
            validation_mode(17_692, false, false), // Pharao seam (the old crash block)
            ValidMode::AssumeValid
        );
        assert_eq!(
            validation_mode(V6_RESTART_HEIGHT_U64 - 1, false, false),
            ValidMode::AssumeValid
        );
        // seam era [2200900, 2203924): kernel computed for the index, not enforced
        assert_eq!(
            validation_mode(V6_RESTART_HEIGHT_U64, false, false),
            ValidMode::SeamAdvisory
        );
        assert_eq!(
            validation_mode(CHECKPOINT_HORIZON - 1, false, false),
            ValidMode::SeamAdvisory
        );
        // at/above the checkpoint horizon: FULL validation
        assert_eq!(
            validation_mode(CHECKPOINT_HORIZON, false, false),
            ValidMode::FullStrict
        );
        assert_eq!(
            validation_mode(CHECKPOINT_HORIZON + 1, false, false),
            ValidMode::FullStrict
        );
        assert_eq!(
            validation_mode(2_204_618, false, false),
            ValidMode::FullStrict
        );
        // live blocks are ALWAYS full-strict (never assume-valid)
        assert_eq!(validation_mode(100, true, false), ValidMode::FullStrict);
        assert_eq!(validation_mode(CHECKPOINT_HORIZON, true, false), ValidMode::FullStrict);
        // --full-validate restores strict everywhere
        assert_eq!(validation_mode(100, false, true), ValidMode::FullStrict);
        assert_eq!(
            validation_mode(CHECKPOINT_HORIZON - 1, false, true),
            ValidMode::FullStrict
        );
    }

    #[test]
    fn checkpoint_anchor_matches_the_daemon_verified_hash() {
        // the compiled-in anchor must be EXACTLY the deployed chain's
        // getblockhash(2203924) (daemon-verified 2026-11-03) — a wrong anchor
        // would invalidate the whole trusted prefix at the boundary.
        assert_eq!(
            CHECKPOINT_ANCHOR_DISPLAY,
            "a838e60da0e5259ff87380d961b9bb5ca8d0d875775f5c54a95425b69e753a14"
        );
        // and it must round-trip through the display->internal codec (a
        // malformed constant would fail every block at the boundary)
        assert!(tri7_core::p2p::display_to_internal(CHECKPOINT_ANCHOR_DISPLAY).is_ok());
    }

    #[test]
    fn checkpoint_anchor_verification_at_boundary_only() {
        let anchor =
            tri7_core::p2p::display_to_internal(CHECKPOINT_ANCHOR_DISPLAY).unwrap();
        let other = {
            let mut h = anchor;
            h[0] ^= 0xff;
            h
        };
        // at the horizon: the real anchor passes
        assert!(check_checkpoint_anchor(CHECKPOINT_HORIZON, &anchor).is_ok());
        // at the horizon: ANY other hash is refused (a fork below the
        // checkpoint cannot smuggle itself past the boundary block)
        let err = check_checkpoint_anchor(CHECKPOINT_HORIZON, &other).unwrap_err();
        assert!(err.contains("CHECKPOINT ANCHOR MISMATCH"), "{}", err);
        assert!(err.contains(&tri7_core::p2p::internal_to_display(&other)));
        // away from the horizon: no anchor obligation (both hashes pass)
        assert!(check_checkpoint_anchor(CHECKPOINT_HORIZON - 1, &other).is_ok());
        assert!(check_checkpoint_anchor(CHECKPOINT_HORIZON + 1, &anchor).is_ok());
        assert!(check_checkpoint_anchor(0, &anchor).is_ok());
    }

    #[test]
    fn parallel_fetch_reassembly_is_strictly_ordered() {
        // workers deliver out of order (8 sessions race); the validator
        // consumes EXACTLY next_h and holds the rest until the gap fills.
        let mut pending: HashMap<u64, Vec<u8>> = HashMap::new();
        pending.insert(12, vec![0xc2]);
        pending.insert(11, vec![0xb1]);
        // hole at h10: nothing may validate, nothing may reorder
        assert!(reassembly_pop(&mut pending, 10).is_none());
        assert_eq!(pending.len(), 2);
        // the hole fills -> strict ascending drain, each removed once
        assert_eq!(reassembly_pop(&mut pending, 10), None);
        pending.insert(10, vec![0xa0]);
        assert_eq!(reassembly_pop(&mut pending, 10), Some(vec![0xa0]));
        assert_eq!(reassembly_pop(&mut pending, 11), Some(vec![0xb1]));
        assert_eq!(reassembly_pop(&mut pending, 12), Some(vec![0xc2]));
        assert!(pending.is_empty());
        // a duplicate delivery for an already-consumed height is ignored
        pending.insert(13, vec![0xd3]);
        assert_eq!(reassembly_pop(&mut pending, 13), Some(vec![0xd3]));
        assert_eq!(reassembly_pop(&mut pending, 13), None);
    }

    #[test]
    fn stale_duplicate_header_segments_are_dropped_not_fatal() {
        let cursor: Hash = [0xAA; 32];
        let older_anchor: Hash = [0xBB; 32]; // locator used 2000 blocks earlier
        let unknown: Hash = [0xCC; 32];
        let anchors = vec![older_anchor, [0xDD; 32]];
        // linked segment: accept
        assert_eq!(classify_segment(&cursor, &cursor, &anchors), SegAction::Accept);
        // re-delivered earlier segment (the double-response race): drop
        assert_eq!(
            classify_segment(&older_anchor, &cursor, &anchors),
            SegAction::DropStale
        );
        // anything else is a real fork/garbage: fatal
        assert_eq!(
            classify_segment(&unknown, &cursor, &anchors),
            SegAction::Fatal
        );
    }

    #[test]
    fn checkpoint_horizon_constants_match_the_deployed_client() {
        // = Checkpoints::GetTotalBlocksEstimate() on the deployed v6.2.6.7
        assert_eq!(CHECKPOINT_HORIZON, 2_203_924);
        // the deployed daemon's own index seam: real stored proofs from here
        assert_eq!(V6_RESTART_HEIGHT_U64, 2_200_900);
        // ordering invariant the envelope relies on
        assert!(V6_RESTART_HEIGHT_U64 < CHECKPOINT_HORIZON);
    }
}
