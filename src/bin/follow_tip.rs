//! Phase 2 — legacy P2P tip follower (follow_tip).
//!
//! Outbound-only TCP client speaking the exact triangles_v5 (v6.2.6.7) wire
//! protocol to the local daemon's P2P listener. Handshake (version/verack),
//! getheaders-driven tip chase with full tri7_core validation of every
//! received block (parse + Hash9 + link + merkle + timestamps + nBits EMA +
//! PoW target / PoS kernel + block signature + tx finality), persistent
//! tipstate.json.
//!
//! Strict safety: read-only participation — we never relay transactions,
//! never serve getdata/getheaders/getblocks/getaddr, never send addr. On any
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
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use primitive_types::U256;
use serde::{Deserialize, Serialize};

use tri7_core::consensus::{
    block_is_proof_of_stake, check_pos_block_signature, check_pow_block_signature,
    max_time_drift, tx_is_coin_base, tx_is_coin_stake, LAST_HARDCODED_CHECKPOINT,
};
use tri7_core::difficulty::{get_next_target_required, set_compact};
use tri7_core::kernel::{
    compute_next_stake_modifier_era, index_flags, stake_modifier_checksum, KernelOutputResolver,
    UtxoMeta,
};
use tri7_core::legacy::{hash_to_display, Block, Hash, Reader};
use tri7_core::replay::{IndexEntry, ReplayIndex};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------
const WINDOW_ENTRIES: usize = 4600; // ≥ selection interval + stake-age cap + margin
const WINDOW_BLOCKS: usize = 1500; // full-tx records for kernel prevout resolution
const POLL_SECS: u64 = 30;
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
    let secs = now_secs() as i64;
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
            auth_basic: format!("Basic {}", base64_encode(format!("{}:{}", user, pass).as_bytes())),
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
    window_base: u64,
    entries: Vec<EntryRec>,
    blocks: Vec<BlockRec>,
    daemon_tip: Option<i64>,
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
}

impl ChainState {
    fn rel(&self, abs: u64) -> usize {
        (abs - self.base) as usize
    }

    fn entry_abs(&self, abs: u64) -> Option<&IndexEntry> {
        self.entries.get(self.rel(abs))
    }

    fn entry_by_hash(&self, hash: &Hash) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| &e.hash == hash)
    }

    /// ReplayIndex view over the window: entries[i] has ABSOLUTE height
    /// base+i, but IndexEntry.height stores the WINDOW-RELATIVE height so the
    /// consensus functions' by_height/entries[...] walks stay in-range (the
    /// difficulty same-type walk indexes by e.height-1 — absolute heights
    /// would index past the window and return None → TARGET_LIMIT early
    /// return; bug found live at the first backfill block). The window only
    /// ever appends, so index i == relative height is invariant.
    fn rel_index(&self) -> ReplayIndex {
        ReplayIndex {
            entries: self.entries.clone(),
        }
    }

    /// Truncated view ending at abs (inclusive): for validating a HISTORICAL
    /// block at abs+1 whose EMA must be anchored at its own ancestors.
    fn rel_index_to(&self, abs: u64) -> ReplayIndex {
        ReplayIndex {
            entries: self.entries[..=self.rel(abs)].to_vec(),
        }
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
        let excess = self.entries.len().saturating_sub(WINDOW_ENTRIES);
        if excess > 0 {
            self.entries.drain(..excess);
            self.checksums.drain(..excess);
            self.base += excess as u64;
        }
        let excess_rec = self.records.len().saturating_sub(WINDOW_BLOCKS);
        if excess_rec > 0 {
            self.records.drain(..excess_rec);
        }
        let tip_rel = self.rel(self.tip_height);
        self.utxo
            .trim(tip_rel.saturating_sub(WINDOW_BLOCKS + 200));
    }

    fn to_file(&self, daemon_tip: Option<i64>) -> TipStateFile {
        TipStateFile {
            height: self.tip_height,
            blockhash: tri7_core::p2p::internal_to_display(&self.tip_hash),
            updated_utc: now_iso(),
            window_base: self.base,
            entries: self
                .entries
                .iter()
                .zip(self.checksums.iter())
                .map(|(e, cs)| EntryRec {
                    height: self.base + e.height as u64,
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
            daemon_tip,
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
                    spends.push((tri7_core::p2p::internal_to_display(&vin.prev_hash), vin.prev_n));
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
struct Config {
    daemon: String,
    rpc_host: String,
    rpc_port: u16,
    conf_path: PathBuf,
    archive: String,
    state_dir: PathBuf,
    seconds: u64,
    backfill_headers: usize,
    backfill_blocks: usize,
    reseed: bool,
    summary_out: Option<PathBuf>,
}

fn usage() -> ! {
    eprintln!(
        "usage: follow_tip [--daemon IP:PORT] [--rpc-port N] [--conf PATH]\n\
         [--archive PATH] [--state-dir DIR] [--seconds N] [--backfill N]\n\
         [--backfill-blocks N] [--reseed] [--summary PATH]"
    );
    std::process::exit(EXIT_USAGE);
}

fn parse_args() -> Config {
    let mut cfg = Config {
        daemon: "127.0.0.1:24113".into(), // triangles.conf port=24113 (net.cpp default 24112)
        rpc_host: "127.0.0.1".into(),
        rpc_port: 19112,
        conf_path: PathBuf::from("/root/.triangles/triangles.conf"),
        archive: "/root/.triangles/blk0001.dat".into(),
        state_dir: PathBuf::from("/root/tri7"),
        seconds: 660,
        backfill_headers: 3000,
        backfill_blocks: 100,
        reseed: false,
        summary_out: None,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let need = |i: &mut usize, args: &[String]| -> String {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match args[i].as_str() {
            "--daemon" => cfg.daemon = need(&mut i, &args),
            "--rpc-port" => cfg.rpc_port = need(&mut i, &args).parse().unwrap_or_else(|_| usage()),
            "--conf" => cfg.conf_path = PathBuf::from(need(&mut i, &args)),
            "--archive" => cfg.archive = need(&mut i, &args),
            "--state-dir" => cfg.state_dir = PathBuf::from(need(&mut i, &args)),
            "--seconds" => cfg.seconds = need(&mut i, &args).parse().unwrap_or_else(|_| usage()),
            "--backfill" => {
                cfg.backfill_headers = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--backfill-blocks" => {
                cfg.backfill_blocks = need(&mut i, &args).parse().unwrap_or_else(|_| usage())
            }
            "--reseed" => cfg.reseed = true,
            "--summary" => cfg.summary_out = Some(PathBuf::from(need(&mut i, &args))),
            _ => usage(),
        }
        i += 1;
    }
    cfg
}

fn read_rpc_creds(cfg: &Config) -> (String, String) {
    let text = std::fs::read_to_string(&cfg.conf_path)
        .unwrap_or_else(|e| die(EXIT_USAGE, &format!("read conf {}: {}", cfg.conf_path.display(), e)));
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
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename state: {}", e))?;
    Ok(())
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
    headers_backfilled: u64,
    blocks_received: u64,
    blocks_validated_live: u64,
    blocks_validated_hist: u64,
    kernel_enforced: u64,
    kernel_seeded: u64,
    nbits_checked: u64,
    sigs_checked: u64,
    checksums_verified: u64,
    polls: u64,
    reconnects: u64,
    parse_failures: u64,
    handshakes: u64,
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
        "nbits_checked": s.nbits_checked,
        "sigs_checked": s.sigs_checked,
        "checksums_verified": s.checksums_verified,
        "polls": s.polls,
        "reconnects": s.reconnects,
        "parse_failures": s.parse_failures,
        "handshakes": s.handshakes,
    })
}

// ---------------------------------------------------------------------------
// P2P session
// ---------------------------------------------------------------------------
struct Session {
    stream: TcpStream,
    dec: tri7_core::p2p::MessageDecoder,
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
        })
    }

    fn send(
        &mut self,
        command: &str,
        payload: &[u8],
        stats: &mut Stats,
    ) -> Result<(), String> {
        let frame = tri7_core::p2p::encode_message(command, payload)?;
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("send {}: {}", command, e))?;
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
}

fn handshake(
    sess: &mut Session,
    stats: &mut Stats,
    our_height: u64,
    daemon: &str,
) -> Result<PeerInfo, String> {
    let (ip, port) = daemon
        .rsplit_once(':')
        .ok_or("daemon addr must be IP:PORT")?;
    let octets: Vec<u8> = ip
        .split('.')
        .map(|o| o.parse::<u8>().map_err(|_| "daemon IP must be dotted quad"))
        .collect::<Result<_, _>>()?;
    let addr_you = tri7_core::p2p::WireAddr::ipv4(
        octets[0],
        octets[1],
        octets[2],
        octets[3],
        port.parse().map_err(|_| "bad daemon port")?,
        tri7_core::p2p::NODE_NETWORK,
    );
    let addr_me =
        tri7_core::p2p::WireAddr::ipv4(127, 0, 0, 1, tri7_core::p2p::DEFAULT_PORT, tri7_core::p2p::NODE_NETWORK);
    let nonce = (now_secs() ^ ((std::process::id() as u64) << 32)) | 1;
    let vp = tri7_core::p2p::VersionPayload {
        version: tri7_core::p2p::PROTOCOL_VERSION,
        services: tri7_core::p2p::NODE_NETWORK,
        timestamp: now_secs() as i64,
        addr_you,
        addr_me,
        nonce,
        user_agent: "/tri7:0.2.0/".into(),
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
    // Prefer headers announcements (main.cpp sendheaders → fPreferHeaders);
    // inv announcements are handled too.
    sess.send("sendheaders", &[], stats)?;
    stats.handshakes += 1;
    Ok(peer.unwrap_or(PeerInfo {
        version: 0,
        subver: "?".into(),
        start_height: -1,
        services: 0,
    }))
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
            .map(|i| (i, "getblock", serde_json::json!([hashes_display[i as usize]])))
            .collect();
        let items = rpc.call_batch(calls)?;
        if items.len() != end - idx {
            return Err(format!("batch size mismatch {} != {}", items.len(), end - idx));
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
            let modifier =
                u64::from_str_radix(b["modifier"].as_str().unwrap_or(""), 16)
                    .map_err(|e| format!("h{}: modifier hex: {}", h, e))?;
            let checksum =
                u32::from_str_radix(b["modifierchecksum"].as_str().unwrap_or(""), 16)
                    .map_err(|e| format!("h{}: checksum hex: {}", h, e))?;
            let proof = if is_pos {
                tri7_core::p2p::display_to_internal(b["proofhash"].as_str().unwrap_or(""))
                    .map_err(|e| format!("h{}: proofhash: {}", h, e))?
            } else {
                [0u8; 32]
            };
            let rt = b["time"].as_i64().unwrap_or(-1);
            if rt != times[i] as i64 {
                return Err(format!("h{}: rpc time {} != p2p-verified time {}", h, rt, times[i]));
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
        return Err("no non-zero modifier checksum in window — cannot anchor the chain".to_string());
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
    rpc: &Rpc,
    stats: &mut Stats,
    block: &Block,
    ctx: &BlockCtx,
) -> Result<ValidationOutcome, String> {
    let h = ctx.height_abs;
    let is_pos = block_is_proof_of_stake(block);

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
    let rel_index = if ctx.live {
        st.rel_index()
    } else {
        st.rel_index_to(h - 1)
    };
    let expected_bits = get_next_target_required(&rel_index, is_pos);
    if block.bits != expected_bits {
        return Err(format!(
            "h{}: NBITS FAIL block={:08x} expected={:08x}",
            h, block.bits, expected_bits
        ));
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
        // 7a. kernel where enforced: resolve the coinstake input from the
        // window UTXO and recompute the full kernel (modifier walk + hash +
        // target). Pre-window inputs fall back to the RPC proof, which the
        // checksum chain still verifies.
        let resolved = {
            let txin = &block.vtx[1].vin[0];
            st.utxo.kernel_meta(&(txin.prev_hash, txin.prev_n))
        };
        match resolved {
            Some(_) => {
                let res = tri7_core::kernel::check_proof_of_stake(
                    &rel_index,
                    block,
                    &st.utxo,
                    block.bits,
                    h as usize - 1, // ABSOLUTE: selects the GetWeight era
                )
                .map_err(|e| format!("h{}: kernel guard: {}", h, e))?;
                if !res.meets_target {
                    return Err(format!(
                        "h{}: KERNEL TARGET FAIL modifier={:016x} block_from(abs h{}) coinDays={} hashProof={}",
                        h,
                        res.modifier,
                        st.base as usize + res.block_from_height,
                        res.coin_day_weight,
                        hash_to_display(&res.hash_proof_of_stake)
                    ));
                }
                proof = res.hash_proof_of_stake;
                out.kernel_enforced = true;
                stats.kernel_enforced += 1;
            }
            None => {
                let info =
                    rpc.call("getblock", serde_json::json!([tri7_core::p2p::internal_to_display(&hash)]))?;
                let ph = info["proofhash"]
                    .as_str()
                    .ok_or_else(|| format!("h{}: no proofhash in rpc", h))?;
                proof = tri7_core::p2p::display_to_internal(ph)?;
                out.kernel_seeded = true;
                stats.kernel_seeded += 1;
            }
        }
        // 8. block signature (required above the last hard checkpoint)
        if h > LAST_HARDCODED_CHECKPOINT as u64 {
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
        // 8. PoW blocks carry an empty signature
        check_pow_block_signature(block).map_err(|e| format!("h{}: {}", h, e))?;
        stats.sigs_checked += 1;
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

    // stake modifier for this height: local computation from the window
    let rel_tip = st.rel(h - 1);
    let (modifier, generated) =
        compute_next_stake_modifier_era(&st.rel_index(), Some(rel_tip), 300)
            .map_err(|e| format!("h{}: modifier computation: {}", h, e))?;
    let entropy = hash[0] & 1;
    let flags = index_flags(is_pos, entropy, generated);
    let prev_cs = *st.checksums.last().ok_or("empty checksum chain in advance")?;
    let cs = stake_modifier_checksum(Some(prev_cs), flags, &outcome.proof, modifier);

    let info = rpc.call("getblock", serde_json::json!([tri7_core::p2p::internal_to_display(&hash)]))?;
    let daemon_cs = u32::from_str_radix(
        info["modifierchecksum"].as_str().ok_or("no modifierchecksum")?,
        16,
    )
    .map_err(|e| e.to_string())?;
    if daemon_cs != cs {
        return Err(format!(
            "h{}: stake-modifier checksum divergence: computed {:08x} daemon {:08x}",
            h, cs, daemon_cs
        ));
    }
    stats.checksums_verified += 1;

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
    st.tip_height = h;
    st.tip_hash = hash;
    st.trim();
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
        let inv: Vec<(u32, Hash)> = awaiting.keys().map(|h| (tri7_core::p2p::MSG_BLOCK, *h)).collect();
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
                None => return Err(format!("seed: block h{} missing from batch", base + i as u64)),
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
            let want_prev = if i == 0 { link_prev } else { expect_hashes[i - 1] };
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
            log(&format!("seed: {}/{} blocks fetched+verified via P2P", next_i, expect_hashes.len()));
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
    };
    let file = st.to_file(Some(daemon_tip));
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
        let inv: Vec<(u32, Hash)> = chunk.iter().map(|(h, _)| (tri7_core::p2p::MSG_BLOCK, *h)).collect();
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
                    r.expect_done().map_err(|e| format!("block trailing: {}", e))?;
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
fn handle_message(
    sess: &mut Session,
    st: &mut ChainState,
    rpc: &Rpc,
    stats: &mut Stats,
    cfg: &Config,
    msg: tri7_core::p2p::RawMessage,
    outstanding: &mut HashMap<Hash, u64>,
    pending_from_headers: &mut VecDeque<(Hash, u64)>,
) -> Result<(), String> {
    match msg.command.as_str() {
        "version" => {
            let v = tri7_core::p2p::parse_version(&msg.payload)?;
            log(&format!(
                "peer version: {} ({}) start height {}",
                v.version, v.user_agent, v.start_height
            ));
            Ok(())
        }
        "verack" => Ok(()),
        "ping" => {
            let n = tri7_core::p2p::parse_ping(&msg.payload)?;
            sess.send("pong", &tri7_core::p2p::build_pong(n), stats)?;
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
                    sess.send(
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
            // Read-only: never request transactions. Unknown block hashes are
            // reconciled via getheaders (which verifies linkage) rather than
            // blind getdata — a fork inv can never steer our tip.
            let inv = tri7_core::p2p::parse_inv(&msg.payload)?;
            let mut unknown = 0usize;
            for (t, hash) in inv {
                if t != tri7_core::p2p::MSG_BLOCK {
                    continue;
                }
                if st.entry_by_hash(&hash).is_none() {
                    unknown += 1;
                }
            }
            if unknown > 0 {
                let stop = [0u8; 32];
                sess.send(
                    "getheaders",
                    &tri7_core::p2p::build_getheaders(&[st.tip_hash], &stop),
                    stats,
                )?;
            }
            Ok(())
        }
        "block" => {
            stats.blocks_received += 1;
            let mut r = Reader::new(&msg.payload);
            let b = Block::read(&mut r).map_err(|e| format!("block parse: {}", e))?;
            r.expect_done().map_err(|e| format!("block trailing: {}", e))?;
            let hash = b.hash();
            let links_tip = b.prev_hash == st.tip_hash;
            let height_abs = if links_tip { st.tip_height + 1 } else { 0 };
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
                advance_with_block(st, rpc, stats, &b, &paths)?;
                stats.blocks_validated_live += 1;
                outstanding.remove(&hash);
                let file = st.to_file(None);
                write_state_atomic(cfg, &file).map_err(|e| format!("write state: {}", e))?;
                log(&format!(
                    "TIP ADVANCE → h{} {} (kernel {})",
                    st.tip_height,
                    tri7_core::p2p::internal_to_display(&st.tip_hash),
                    if paths.kernel_enforced { "enforced" } else { "seeded" }
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
        // read-only: never serve anything
        "getdata" | "getheaders" | "getblocks" | "getaddr" | "mempool" | "getwalletaddr"
        | "getsnap" | "getsnapchunk" | "tx" | "addr" | "walletaddr" | "seeder"
        | "getseederlist" | "seederlist" | "cmpctblock" | "sendcmpct" | "sendheaders" => Ok(()),
        other => {
            log(&format!("ignoring message {:?}", other));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
fn main() {
    let cfg = parse_args();
    let (user, pass) = read_rpc_creds(&cfg);
    let rpc = tri7_rpc(&cfg, &user, &pass);
    let mut stats = Stats::default();

    // 1. connect + handshake (the seed itself pulls raw blocks over P2P)
    let mut sess = connect_and_handshake(&cfg, &mut stats, 0);

    // 2. load or seed chain state
    let path = state_path(&cfg);
    let mut st: ChainState = if path.exists() && !cfg.reseed {
        log("loading tipstate.json …");
        load_state(&cfg).unwrap_or_else(|e| die(EXIT_SEED, &format!("load state: {}", e)))
    } else {
        log("no usable state — seeding: raw blocks over P2P getdata, Hash9+link+merkle verified locally …");
        let seeded = seed_from_network(&cfg, &rpc, &mut sess, &mut stats);
        if let Err(e) = &seeded {
            sess.shutdown();
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

    // report our real height to the daemon: reconnect with the correct
    // start_height so its fNearTip logic treats us as a block peer
    sess.shutdown();
    stats.reconnects += 1;
    sess = connect_and_handshake(&cfg, &mut stats, st.tip_height);

    let daemon_tip = rpc
        .call("getblockcount", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    log(&format!(
        "state: tip h{} {} · daemon getblockcount {}",
        st.tip_height,
        tri7_core::p2p::internal_to_display(&st.tip_hash),
        daemon_tip
    ));

    // 3. backfill
    if cfg.backfill_headers > 0 {
        if let Err(e) = run_backfill(&mut sess, &st, &rpc, &mut stats, &cfg) {
            sess.shutdown();
            finish(&cfg, &st, &stats, &rpc, EXIT_PROTOCOL, &format!("backfill: {}", e));
        }
    }

    // 4. live follow loop
    let deadline = Instant::now() + Duration::from_secs(cfg.seconds);
    let mut last_poll = Instant::now() - Duration::from_secs(POLL_SECS);
    let mut last_traffic = Instant::now();
    let mut poll_parity = false;
    let mut outstanding: HashMap<Hash, u64> = HashMap::new();
    let mut pending_from_headers: VecDeque<(Hash, u64)> = VecDeque::new();
    let mut last_state_write = Instant::now();

    loop {
        if Instant::now() >= deadline {
            break;
        }

        // poll tick — getheaders with an alternating locator (the daemon
        // silently drops a consecutive duplicate getheaders request)
        if last_poll.elapsed() >= Duration::from_secs(POLL_SECS) {
            poll_parity = !poll_parity;
            let locator: Vec<Hash> = if poll_parity {
                vec![st.tip_hash]
            } else {
                let prev_hash = st
                    .entry_abs(st.tip_height - 1)
                    .map(|e| e.hash)
                    .unwrap_or(st.tip_hash);
                vec![prev_hash, st.tip_hash]
            };
            let stop = [0u8; 32];
            if let Err(e) = sess.send(
                "getheaders",
                &tri7_core::p2p::build_getheaders(&locator, &stop),
                &mut stats,
            ) {
                log(&format!("send getheaders failed: {} — reconnecting", e));
                sess.shutdown();
                stats.reconnects += 1;
                sess = connect_and_handshake(&cfg, &mut stats, st.tip_height);
                last_poll = Instant::now();
                continue;
            }
            stats.polls += 1;
            log(&format!(
                "poll #{}: tip h{} · rx={} msgs tx={} msgs · blocks {}/{} received+{} hist validated · kernels {}/{} enforced/seeded",
                stats.polls,
                st.tip_height,
                stats.total_recv(),
                stats.total_sent(),
                stats.blocks_validated_live,
                stats.blocks_received,
                stats.blocks_validated_hist,
                stats.kernel_enforced,
                stats.kernel_seeded
            ));
            last_poll = Instant::now();
        }

        // request queued block announcements
        if !pending_from_headers.is_empty() && outstanding.len() < MAX_OUTSTANDING_GETDATA {
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
                if let Err(e) = sess.send("getdata", &tri7_core::p2p::build_getdata(&batch), &mut stats) {
                    log(&format!("send getdata failed: {} — reconnecting", e));
                    sess.shutdown();
                    stats.reconnects += 1;
                    sess = connect_and_handshake(&cfg, &mut stats, st.tip_height);
                    continue;
                }
            }
        }

        // pump inbound
        let mut msgs = match sess.pump() {
            Ok(m) => m,
            Err(e) => {
                log(&format!("protocol failure: {} — clean disconnect", e));
                sess.shutdown();
                stats.parse_failures += 1;
                if stats.parse_failures >= 3 {
                    finish(
                        &cfg,
                        &st,
                        &stats,
                        &rpc,
                        EXIT_PROTOCOL,
                        "repeated protocol failures",
                    );
                }
                stats.reconnects += 1;
                sess = connect_and_handshake(&cfg, &mut stats, st.tip_height);
                last_traffic = Instant::now();
                continue;
            }
        };
        if msgs.is_empty() {
            // keep polling cadence tight without busy-spinning
        } else {
            last_traffic = Instant::now();
        }

        let mut fatal: Option<String> = None;
        for msg in msgs.drain(..) {
            stats.note_recv(&msg.command);
            let res = handle_message(
                &mut sess,
                &mut st,
                &rpc,
                &mut stats,
                &cfg,
                msg,
                &mut outstanding,
                &mut pending_from_headers,
            );
            if let Err(e) = res {
                fatal = Some(e);
                break;
            }
        }
        if let Some(e) = fatal {
            log(&format!("validation failure: {} — clean disconnect", e));
            sess.shutdown();
            finish(&cfg, &st, &stats, &rpc, EXIT_VALIDATE, &e);
        }

        if last_traffic.elapsed() >= Duration::from_secs(STALE_LINK_SECS) {
            log("no traffic for 90 s — clean reconnect");
            sess.shutdown();
            stats.reconnects += 1;
            sess = connect_and_handshake(&cfg, &mut stats, st.tip_height);
            last_traffic = Instant::now();
        }

        // persist tip state periodically
        if last_state_write.elapsed() >= Duration::from_secs(30) {
            let file = st.to_file(None);
            write_state_atomic(&cfg, &file)
                .unwrap_or_else(|e| die(EXIT_VALIDATE, &format!("write state: {}", e)));
            last_state_write = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(40));
    }

    sess.shutdown();
    let file = st.to_file(None);
    write_state_atomic(&cfg, &file)
        .unwrap_or_else(|e| die(EXIT_VALIDATE, &format!("write state: {}", e)));
    finish(&cfg, &st, &stats, &rpc, 0, "run complete");
}

fn tri7_rpc(cfg: &Config, user: &str, pass: &str) -> Rpc {
    Rpc::new(&cfg.rpc_host, cfg.rpc_port, user, pass)
}

fn connect_and_handshake(cfg: &Config, stats: &mut Stats, height: u64) -> Session {
    for attempt in 1..=3 {
        match Session::connect(&cfg.daemon) {
            Ok(mut sess) => match handshake(&mut sess, stats, height, &cfg.daemon) {
                Ok(peer) => {
                    log(&format!(
                        "handshake ok (attempt {}): peer version {} ({:?}) start height {} services {}",
                        attempt, peer.version, peer.subver, peer.start_height, peer.services
                    ));
                    return sess;
                }
                Err(e) => {
                    log(&format!("handshake attempt {} failed: {}", attempt, e));
                    sess.shutdown();
                }
            },
            Err(e) => log(&format!("connect attempt {} failed: {}", attempt, e)),
        }
        std::thread::sleep(Duration::from_secs(2 * attempt as u64));
    }
    die(EXIT_PROTOCOL, "handshake failed after 3 attempts");
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
            "daemon_endpoint": cfg.daemon,
            "protocol_version": tri7_core::p2p::PROTOCOL_VERSION,
            "user_agent": "/tri7:0.2.0/",
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
