//! tri7d RPC surface (phase 5b) — small HTTP/JSON-RPC server on loopback.
//!
//! DESIGN: tri7d is intended to REPLACE the legacy daemon, so every everyday
//! wallet/chain query is served NATIVELY from tri7's own small-footprint
//! state; the legacy-RPC proxy is a TRANSITIONAL fallback only, and every
//! response envelope is tagged with a "source" field ("tri7" | "legacy-proxy")
//! so cutover progress is measurable. Each method below documents its source:
//!
//! NATIVE (source "tri7"):
//!   gettip                — tri7 tipstate (height/blockhash/at_daemon_tip)
//!   getblockcount         — tri7 tipstate
//!   getblockhash <h>      — tri7 blockindex height map (window-authoritative
//!                           inside [base..tip], frame-order below it); proxy
//!                           fallback for heights the map can't answer
//!   getblock <hash|height>— tri7 blockindex: raw frame read from blk file,
//!                           verified + parsed locally; proxy fallback while
//!                           the index warms
//!   getrawtransaction     — tri7 blockindex tx map + local parse; proxy
//!                           fallback while the index warms
//!   getinfo               — tri7 tipstate + replay balances snapshot + P2P
//!   getdifficulty         — tri7 tipstate window + difficulty module
//!   getconnectioncount    — tri7 P2P session state
//!   getstakemodifier <h>  — tri7 tipstate window entries
//!   getbalance            — tri7 cached replay balances (balances.json)
//!   listunspent           — tri7 cached replay wallet UTXO snapshot
//!                           (wallet_utxos.json)
//!   listwalletaddresses   — tri7 cached replay balances
//!   getreceivedbyaddress  — tri7 cached replay balances (received map)
//!   validateaddress       — tri7 address codec (pure function)
//!   health                — tri7 self-status (no external dependency)
//!
//! PROXY-FALLBACK (source "legacy-proxy", transitional — forwarded read-only
//! to 127.0.0.1:19112 when native data can't serve the call; NEVER forwards
//! wallet-write methods): getblock/getblockhash/getrawtransaction misses and
//! daemon-only reads (getrawmempool, getmempoolinfo, gettransaction,
//! getstakinginfo, getnetworkhashps, getconnectioncount-style info).
//!
//! SECURITY: loopback-only bind (enforced), no auth on loopback (same trust
//! model as the legacy daemon), 1 MB request cap, per-request read timeouts,
//! read-only allowlist — send/move/walletpassphrase-style methods are never
//! forwarded anywhere.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::address::{script_to_dest, ScriptDest};
use crate::blockindex::{BlockHit, TxHit};
use crate::consensus::block_is_proof_of_stake;
use crate::p2p::internal_to_display;

// ---------------------------------------------------------------------------
// Versions / constants
// ---------------------------------------------------------------------------

/// tri7's own RPC version marker (daemon-replacement build, phase 5b).
pub const TRI7_RPC_VERSION: i64 = 70000;
pub const TRI7_SUBVERSION: &str = "/tri7:7.0.0/";
/// Wire protocol version tri7 speaks (p2p.rs PROTOCOL_VERSION).
pub const TRI7_PROTOCOL_VERSION: i64 = 70206;
/// Default request cap: 1 MB (task requirement).
pub const DEFAULT_MAX_BODY: usize = 1_048_576;

/// Methods served natively (see module docs for the data source of each).
pub const NATIVE_METHODS: &[&str] = &[
    "gettip",
    "getblockcount",
    "getbestblockhash",
    "getblockhash",
    "getblock",
    "getrawtransaction",
    "getinfo",
    "getdifficulty",
    "getconnectioncount",
    "getstakemodifier",
    "getbalance",
    "listunspent",
    "listwallettx",
    "listwalletaddresses",
    "getreceivedbyaddress",
    "validateaddress",
    "health",
    "getsyncinfo",
    // ---- phase 7 milestone 2: broadcast / staking / connections / smsg ----
    "sendrawtransaction",
    "getstakinginfo",
    "staking",
    "getpeerinfo",
    "smsgsend",
    "smsginbox",
    "smsgoutbox",
    "smsgscandir",
    "smsgpurge",
    "smsgaddaddress",
    "smsgkeys",
    "smsglocalinfo",
    // ---- parity audit 2026-11-03: legacy info/util methods (rpcmining /
    // rpcnet / rpcblockchain / rpcwallet surfaces servable from tri7's own
    // providers — no ops/daemon needed) ----
    "help",
    "getblockheader",
    "getblockbynumber",
    "getblockchaininfo",
    "getwalletinfo",
    "getnetworkinfo",
    "getmininginfo",
    "getsubsidy",
    "estimatefee",
    "verifymessage",
];

/// The NATIVE_METHODS subset routed to the live NodeOps engine (wallet keys,
/// peer sessions, staking loop, smsg store). Everything else is served from
/// the passive snapshots/window.
pub const OPS_METHODS: &[&str] = &[
    "sendrawtransaction",
    "getstakinginfo",
    "staking",
    "getpeerinfo",
    "smsgsend",
    "smsginbox",
    "smsgoutbox",
    "smsgscandir",
    "smsgpurge",
    "smsgaddaddress",
    "smsgkeys",
    "smsglocalinfo",
];

/// Methods allowed to fall back to the legacy proxy when the native path
/// can't answer (transitional cutover aid). Read-only set ONLY — anything
/// not in NATIVE_METHODS ∪ PROXY_FALLBACK_METHODS is rejected with -32601
/// and never forwarded (wallet-write safety).
pub const PROXY_FALLBACK_METHODS: &[&str] = &[
    "getblock",
    "getblockhash",
    "getrawtransaction",
    "gettransaction",
    "getreceivedbyaddress",
    "validateaddress",
    "getinfo",
    "getdifficulty",
    "getconnectioncount",
    "listunspent",
    "getrawmempool",
    "getmempoolinfo",
    "getnetworkhashps",
    "getwalletinfo",
];

// ---------------------------------------------------------------------------
// Data-source traits (implemented by follow_tip's live state; stubbed in tests)
// ---------------------------------------------------------------------------

pub struct TipView {
    pub height: u64,
    pub blockhash: String, // display hex
    pub daemon_tip: Option<i64>,
}

pub struct EntryView {
    pub height: u64,
    pub hash: String,
    pub modifier: u64,
    pub generated: bool,
    pub checksum: u32,
    pub pos: bool,
    pub proof: String, // display hex of hashProofOfStake
    pub entropy: u8,
    pub time: u32,
    pub bits: u32,
}

/// tri7's own tip/window state (tipstate).
pub trait TipProvider: Send + Sync {
    fn tip(&self) -> TipView;
    /// (window_base, entries)
    fn window(&self) -> (u64, usize);
    fn entry_at(&self, h: u64) -> Option<EntryView>;
    /// nBits of the last window block.
    fn last_bits(&self) -> Option<u32>;
    /// next required target (difficulty module) for PoW (false) / PoS (true).
    fn next_target_bits(&self, pos: bool) -> Option<u32>;
    fn peers(&self) -> u32;
    fn uptime_secs(&self) -> u64;
    fn state_write_age_secs(&self) -> Option<u64>;
}

pub struct BalanceView {
    pub sats: i64,
    /// chain-wide supply from the snapshot totals (moneysupply field)
    pub supply_sats: i64,
    pub coverage_height: u64,
    pub coverage_hash: String,
    pub addresses: usize,
    pub utxos: usize,
}

pub struct AddressEntry {
    pub address: String,
    pub balance_sats: i64,
    pub utxos: u64,
    pub received_regular_sats: i64,
    pub received_all_sats: i64,
}

#[derive(Clone, Debug)]
pub struct UtxoRow {
    pub txid: String, // display hex
    pub n: u32,
    pub address: String,
    pub script_hex: String,
    pub value_sats: i64,
    pub height: u64,
    /// transaction timestamp as the chain stores it (unix seconds). The
    /// legacy client dates history rows from THIS field, so matching it keeps
    /// dates identical across clients. 0 = unknown (pre-seed rows).
    pub tx_time: i64,
    pub prev_kind: String,
}

/// Wallet data from the cached replay-derived snapshots (balances.json /
/// wallet_utxos.json — the phase-1e replay pattern). No live UTXO set is
/// maintained; coverage is bounded by the snapshot's height.
pub trait WalletProvider: Send + Sync {
    fn balance(&self) -> Option<BalanceView>;
    /// (coverage height, coverage hash) of the loaded snapshots.
    fn coverage(&self) -> Option<(u64, String)>;
    fn address_entries(&self) -> Vec<AddressEntry>;
    /// lifetime regular-tx receipts (daemon getreceivedbyaddress semantics:
    /// P2PKH-script outputs only). None = never received → treated as 0.
    fn received_regular(&self, addr: &str) -> Option<i64>;
    /// wallet membership (balances.json wallet address list).
    fn is_mine(&self, addr: &str) -> bool;
    fn utxos(&self) -> Vec<UtxoRow>;
    /// Full wallet history (history.json, built by the txhistory bin).
    /// Empty = no history file yet; the RPC then answers with an empty list
    /// rather than an error so the GUI can render "run a rescan".
    fn history(&self) -> Vec<crate::history::HistoryRec> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Replay-derived wallet snapshots (balances.json / wallet_utxos.json).
// The balances bin writes both files at the end of a replay; follow_tip and
// the RPC layer share THESE loaders so the bin and lib cannot drift.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WalletAddrRow {
    pub address: String,
    pub balance_sats: i64,
    pub utxos: u64,
    pub received_regular_sats: i64,
    pub received_all_sats: i64,
}

#[derive(Clone, Debug)]
pub struct BalancesSnap {
    pub loaded: bool,
    pub path: PathBuf,
    pub height: u64,
    pub coverage_hash: String,
    pub total_wallet_sats: i64,
    pub total_supply_sats: i64,
    pub wallet: Vec<WalletAddrRow>,
    pub received: HashMap<String, i64>,
    pub mine: HashSet<String>,
}

/// Load balances.json. Never fails: an unavailable/unparseable file yields
/// `loaded: false` (the getbalance family then errors on use, and the
/// coverage gate refuses listunspent).
pub fn load_balances_snap(path: &PathBuf) -> BalancesSnap {
    let mut snap = BalancesSnap {
        loaded: false,
        path: path.clone(),
        height: 0,
        coverage_hash: String::new(),
        total_wallet_sats: 0,
        total_supply_sats: 0,
        wallet: Vec::new(),
        received: HashMap::new(),
        mine: HashSet::new(),
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "rpc: balances snapshot {} unavailable — getbalance/listwalletaddresses/\
             getreceivedbyaddress will error (deploy it with --balances)",
            path.display()
        );
        return snap;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("rpc: balances snapshot {} unparseable", path.display());
        return snap;
    };
    snap.height = v["height"].as_u64().unwrap_or(0);
    snap.coverage_hash = v["covered_tip_hash"].as_str().unwrap_or("").to_string();
    snap.total_wallet_sats = v["totals"]["wallet_balance_sats"].as_i64().unwrap_or(0);
    snap.total_supply_sats = v["totals"]["total_balance_sats"].as_i64().unwrap_or(0);
    if let Some(arr) = v["wallet"].as_array() {
        for w in arr {
            let addr = w["address"].as_str().unwrap_or("").to_string();
            snap.mine.insert(addr.clone());
            snap.wallet.push(WalletAddrRow {
                address: addr,
                balance_sats: w["balance_sats"].as_i64().unwrap_or(0),
                utxos: w["utxos"].as_u64().unwrap_or(0),
                received_regular_sats: w["received_regular_sats"].as_i64().unwrap_or(0),
                received_all_sats: w["received_all_sats"].as_i64().unwrap_or(0),
            });
        }
    }
    if let Some(m) = v["received_regular"].as_object() {
        for (k, val) in m {
            snap.received.insert(k.clone(), val.as_i64().unwrap_or(0));
        }
    }
    snap.loaded = !snap.wallet.is_empty();
    eprintln!(
        "rpc: balances snapshot loaded from {} (h{}, {} wallet addrs, wallet {} sats, {} received-addrs)",
        path.display(),
        snap.height,
        snap.wallet.len(),
        snap.total_wallet_sats,
        snap.received.len()
    );
    snap
}

#[derive(Clone, Debug)]
pub struct UtxosSnap {
    pub loaded: bool,
    pub height: u64,
    pub coverage_hash: String,
    pub rows: Vec<UtxoRow>,
}

/// Load wallet_utxos.json. Never fails: an unavailable/unparseable file
/// yields `loaded: false`; a parsed file — even with ZERO UTXOs — is loaded
/// state (an empty wallet legitimately yields an empty listunspent array).
pub fn load_utxos_snap(path: &PathBuf) -> UtxosSnap {
    let mut snap = UtxosSnap {
        loaded: false,
        height: 0,
        coverage_hash: String::new(),
        rows: Vec::new(),
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "rpc: wallet UTXO snapshot {} unavailable — listunspent will error",
            path.display()
        );
        return snap;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("rpc: wallet UTXO snapshot {} unparseable", path.display());
        return snap;
    };
    snap.height = v["height"].as_u64().unwrap_or(0);
    snap.coverage_hash = v["covered_tip_hash"].as_str().unwrap_or("").to_string();
    if let Some(arr) = v["utxos"].as_array() {
        for u in arr {
            snap.rows.push(UtxoRow {
                txid: u["txid"].as_str().unwrap_or("").to_string(),
                n: u["n"].as_u64().unwrap_or(0) as u32,
                address: u["address"].as_str().unwrap_or("").to_string(),
                script_hex: u["script_hex"].as_str().unwrap_or("").to_string(),
                value_sats: u["value_sats"].as_i64().unwrap_or(0),
                height: u["height"].as_u64().unwrap_or(0),
                tx_time: u["tx_time"].as_i64().unwrap_or(0),
                prev_kind: u["prev_kind"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    snap.loaded = true;
    eprintln!(
        "rpc: wallet UTXO snapshot loaded from {} (h{}, {} UTXOs, {} sats)",
        path.display(),
        snap.height,
        snap.rows.len(),
        snap.rows.iter().map(|r| r.value_sats).sum::<i64>()
    );
    snap
}

/// Wallet backed by the two cached replay snapshots. [`WalletProvider::coverage`]
/// is the STRICT gate every confirmations consumer must pass: both snapshots
/// loaded, both coverage hashes nonempty, heights equal, hashes exactly equal.
pub struct SnapshotWallet {
    pub balances: BalancesSnap,
    pub utxos: UtxosSnap,
}

impl WalletProvider for SnapshotWallet {
    fn balance(&self) -> Option<BalanceView> {
        if !self.balances.loaded {
            return None;
        }
        let b = &self.balances;
        Some(BalanceView {
            sats: b.total_wallet_sats,
            supply_sats: b.total_supply_sats,
            coverage_height: b.height,
            coverage_hash: b.coverage_hash.clone(),
            addresses: b.wallet.len(),
            utxos: b.wallet.iter().map(|w| w.utxos).sum::<u64>() as usize,
        })
    }

    fn coverage(&self) -> Option<(u64, String)> {
        // STRICT: listunspent/getbalance confirmations may only key on a
        // snapshot pair that (a) is fully loaded on BOTH sides, (b) carries a
        // nonempty coverage hash on BOTH sides, and (c) agrees on height AND
        // tip hash exactly (i.e. produced by the SAME replay run). Anything
        // else → None → the RPC layer refuses rather than serve confirmations
        // counted against the wrong tip.
        if !self.balances.loaded || !self.utxos.loaded {
            return None;
        }
        if self.balances.coverage_hash.is_empty() || self.utxos.coverage_hash.is_empty() {
            return None;
        }
        if self.balances.height != self.utxos.height {
            return None;
        }
        if self.balances.coverage_hash != self.utxos.coverage_hash {
            return None;
        }
        Some((self.utxos.height, self.utxos.coverage_hash.clone()))
    }

    fn address_entries(&self) -> Vec<AddressEntry> {
        self.balances
            .wallet
            .iter()
            .map(|w| AddressEntry {
                address: w.address.clone(),
                balance_sats: w.balance_sats,
                utxos: w.utxos,
                received_regular_sats: w.received_regular_sats,
                received_all_sats: w.received_all_sats,
            })
            .collect()
    }

    fn received_regular(&self, addr: &str) -> Option<i64> {
        self.balances.received.get(addr).copied()
    }

    fn is_mine(&self, addr: &str) -> bool {
        self.balances.mine.contains(addr)
    }

    fn utxos(&self) -> Vec<UtxoRow> {
        self.utxos.rows.clone()
    }
}

/// Disk-backed block/tx index (blockindex.rs) — the native source for
/// getblock / getrawtransaction / getblockhash.
pub trait BlockDataProvider: Send + Sync {
    /// true once the index is built and serving.
    fn ready(&self) -> bool;
    /// status for health/progress reporting.
    fn status_json(&self) -> Value;
    /// verbose legacy-style block JSON (hash or height); Ok(None) = not found.
    fn block_by_hash(&self, hash_display: &str) -> Result<Option<Value>, String>;
    fn block_by_height(&self, h: u64) -> Result<Option<Value>, String>;
    /// Raw wire hex (header+vtx+sig) by 64-hex hash or height — the legacy
    /// `getblock <id> false` form. Default: unsupported (Ok(None), reported
    /// as "Block not found").
    fn raw_block(
        &self,
        _hash_display: Option<&str>,
        _height: Option<u64>,
    ) -> Result<Option<String>, String> {
        let _ = (_hash_display, _height);
        Ok(None)
    }
    /// verbose object, or raw hex string when verbose=false; Ok(None) = not found.
    fn tx(&self, txid_display: &str, verbose: bool) -> Result<Option<Value>, String>;
    /// main-chain hash (display) at height; Ok(None) = height unavailable.
    fn hash_at(&self, h: u64) -> Result<Option<String>, String>;
}

/// Transitional proxy to the legacy daemon RPC (read-only allowlist above).
pub trait Proxy: Send + Sync {
    /// Forward a parsed request object; returns (http status, envelope).
    fn forward(&self, req: &Value) -> Result<(u16, Value), String>;
    fn describe(&self) -> String;
    /// cheap liveness probe for health.
    fn probe(&self) -> bool;
}

/// Node-owned operations that need the daemon's live machinery (wallet keys,
/// peers, staking loop, smsg engine). Implemented by follow_tip; stubbed in
/// tests. Every method here is NATIVE-ONLY — never proxied, never forwarded.
///
///   sendrawtransaction — consensus-validate + pending-relay set + inv relay
///   getstakinginfo / staking — staking loop status + persisted on|off switch
///   getpeerinfo — real per-session connection records (transport flags)
///   smsg* — the smsg engine (store, buckets, wallet keys)
pub trait NodeOps: Send + Sync {
    /// Ok(result value) or Err((rpc error code, message)).
    fn call(&self, method: &str, params: &Value) -> Result<Value, (i64, String)>;
    /// cheap liveness for health (false before the engine is wired)
    fn ready(&self) -> bool {
        true
    }
}

pub struct Deps<'a> {
    pub tip: &'a dyn TipProvider,
    pub wallet: &'a dyn WalletProvider,
    pub blocks: &'a dyn BlockDataProvider,
    pub proxy: Option<&'a dyn Proxy>,
    pub ops: Option<&'a dyn NodeOps>,
}

// ---------------------------------------------------------------------------
// Envelopes + dispatch
// ---------------------------------------------------------------------------

fn env_ok(id: &Value, result: Value) -> Value {
    json!({"result": result, "error": Value::Null, "id": id, "source": "tri7"})
}

fn env_err(id: &Value, code: i64, msg: &str) -> Value {
    json!({
        "result": Value::Null,
        "error": {"code": code, "message": msg},
        "id": id,
        "source": "tri7"
    })
}

fn unknown_method_msg(method: &str) -> String {
    format!(
        "method not found: {} (tri7 native: {:?}; legacy-proxy fallback: {:?}; wallet-write and \
         unknown methods are never forwarded)",
        method, NATIVE_METHODS, PROXY_FALLBACK_METHODS
    )
}

enum NativeOutcome {
    Ok(Value),
    Err(i64, String),
    /// native path can't answer — proxy fallback if allowed+configured
    Miss(String),
}

/// Handle one raw request body (single object or batch array).
/// Returns (http status, response body).
pub fn handle_body(deps: &Deps, body: &str) -> (u16, String) {
    let parsed: Result<Value, _> = serde_json::from_str(body);
    let v = match parsed {
        Ok(v) => v,
        Err(e) => {
            return (
                200,
                env_err(&Value::Null, -32700, &format!("Parse error: {}", e)).to_string(),
            )
        }
    };
    match v {
        Value::Array(items) => {
            if items.is_empty() {
                return (
                    200,
                    env_err(&Value::Null, -32600, "Invalid Request: empty batch").to_string(),
                );
            }
            let outs: Vec<Value> = items.iter().map(|it| handle_one(deps, it).1).collect();
            (200, serde_json::to_string(&outs).unwrap())
        }
        Value::Object(_) => {
            let (_, env) = handle_one(deps, &v);
            (200, env.to_string())
        }
        _ => (
            200,
            env_err(
                &Value::Null,
                -32600,
                "Invalid Request: expected object or array",
            )
            .to_string(),
        ),
    }
}

fn handle_one(deps: &Deps, req: &Value) -> (&'static str, Value) {
    if !req.is_object() {
        return (
            "tri7",
            env_err(&Value::Null, -32600, "Invalid Request: not an object"),
        );
    }
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = match req.get("method").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return (
                "tri7",
                env_err(&id, -32600, "Invalid Request: missing method string"),
            )
        }
    };
    let params = req.get("params").cloned().unwrap_or_else(|| json!([]));

    // proxy-only methods (never in the native table): forward directly
    if !NATIVE_METHODS.contains(&method.as_str()) {
        return if PROXY_FALLBACK_METHODS.contains(&method.as_str()) {
            forward_to_proxy(deps, req, &id, "daemon-only read (not native yet)")
        } else {
            ("tri7", env_err(&id, -32601, &unknown_method_msg(&method)))
        };
    }

    match native_call(deps, &method, &params) {
        NativeOutcome::Ok(v) => ("tri7", env_ok(&id, v)),
        NativeOutcome::Err(code, msg) => ("tri7", env_err(&id, code, &msg)),
        NativeOutcome::Miss(why) => {
            if PROXY_FALLBACK_METHODS.contains(&method.as_str()) {
                forward_to_proxy(deps, req, &id, &why)
            } else {
                (
                    "tri7",
                    env_err(
                        &id,
                        -1,
                        &format!(
                            "native path unavailable ({}) and no legacy proxy configured (cutover mode)",
                            why
                        ),
                    ),
                )
            }
        }
    }
}

/// Forward the original request to the legacy proxy, tagging the envelope.
fn forward_to_proxy(deps: &Deps, req: &Value, id: &Value, why: &str) -> (&'static str, Value) {
    if let Some(p) = deps.proxy {
        return match p.forward(req) {
            Ok((_status, mut env)) => {
                // tag the legacy envelope so cutover progress is measurable
                // without touching its result/error/id
                if let Some(obj) = env.as_object_mut() {
                    obj.insert("source".into(), json!("legacy-proxy"));
                }
                ("legacy-proxy", env)
            }
            Err(e) => (
                "tri7",
                env_err(
                    id,
                    -32001,
                    &format!(
                        "legacy proxy unavailable ({}): {} — tri7 native miss: {}",
                        p.describe(),
                        e,
                        why
                    ),
                ),
            ),
        };
    }
    (
        "tri7",
        env_err(
            id,
            -1,
            &format!(
                "native path unavailable ({}) and no legacy proxy configured (cutover mode)",
                why
            ),
        ),
    )
}

// ---------------------------------------------------------------------------
// Parameter helpers (positional arrays AND named objects, like the daemon)
// ---------------------------------------------------------------------------

fn param_get<'a>(params: &'a Value, pos: usize, name: &str) -> Option<&'a Value> {
    match params {
        Value::Array(a) => a.get(pos),
        Value::Object(o) => o.get(name),
        _ => None,
    }
}

fn p_u64(params: &Value, pos: usize, name: &str) -> Result<Option<u64>, (i64, String)> {
    let v = match param_get(params, pos, name) {
        None => return Ok(None),
        Some(v) => v,
    };
    let parsed = v
        .as_u64()
        .or_else(|| {
            v.as_i64()
                .and_then(|x| if x >= 0 { Some(x as u64) } else { None })
        })
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()));
    parsed.map(Some).ok_or((
        -8,
        format!("invalid parameter '{}' — expected integer", name),
    ))
}

fn p_str(params: &Value, pos: usize, name: &str) -> Result<Option<String>, (i64, String)> {
    match param_get(params, pos, name) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err((
            -8,
            format!("invalid parameter '{}' — expected string", name),
        )),
    }
}

fn p_bool(params: &Value, pos: usize, name: &str) -> Result<Option<bool>, (i64, String)> {
    let v = match param_get(params, pos, name) {
        None => return Ok(None),
        Some(v) => v,
    };
    if let Some(b) = v.as_bool() {
        return Ok(Some(b));
    }
    if let Some(n) = v.as_i64() {
        return Ok(Some(n != 0));
    }
    Err((
        -8,
        format!("invalid parameter '{}' — expected bool or int", name),
    ))
}

// ---------------------------------------------------------------------------
// Native handlers (each documents its data source — see module docs)
// ---------------------------------------------------------------------------

/// Convert a Result<_, (i64, String)> into an early NativeOutcome::Err.
macro_rules! ptry {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err((code, msg)) => return NativeOutcome::Err(code, msg),
        }
    };
}

/// The daemon-parity difficulty object shared by getdifficulty and getinfo.
/// Daemon semantics (verified live): "proof-of-stake" = GetDifficulty of the
/// LAST block's nBits (bit-exact match); "proof-of-work" = difficulty of the
/// next required PoW target — the legacy daemon walks its FULL index for the
/// last PoW blocks, which our window cannot reproduce on a static chain (the
/// window holds no PoW blocks), so tri7 reports the window-based next target.
fn difficulty_json(deps: &Deps) -> Value {
    let pos = deps.tip.last_bits().map(difficulty_from_bits);
    let pow = deps.tip.next_target_bits(false).map(difficulty_from_bits);
    json!({
        "proof-of-work": pow,
        "proof-of-stake": pos,
        "proof-of-work_note": "next-required-target over the tri7 window (legacy walks the full index for PoW blocks)",
    })
}

fn native_call(deps: &Deps, method: &str, params: &Value) -> NativeOutcome {
    match method {
        // ---- tri7 tipstate ----
        "gettip" => {
            let t = deps.tip.tip();
            NativeOutcome::Ok(json!({
                "height": t.height,
                "blockhash": t.blockhash,
                "at_daemon_tip": t.daemon_tip.map_or(false, |dt| dt >= 0 && t.height == dt as u64),
            }))
        }
        "getblockcount" => NativeOutcome::Ok(json!(deps.tip.tip().height)),
        "getbestblockhash" => {
            // Source: tri7 tipstate window tip (the followed chain's tip hash).
            // Post-cutover this method was LOST (legacy-only); stake_mint's
            // daemon-tip guard and any daemon-parity consumer need it.
            NativeOutcome::Ok(json!(deps.tip.tip().blockhash))
        }

        "getblockhash" => {
            // Source: tri7 blockindex height map; proxy fallback when the map
            // can't answer (heights outside it).
            let h = match ptry!(p_u64(params, 0, "height")) {
                Some(h) => h,
                None => return NativeOutcome::Err(-8, "expected height parameter".into()),
            };
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            match deps.blocks.hash_at(h) {
                Ok(Some(s)) => NativeOutcome::Ok(json!(s)),
                Ok(None) => {
                    NativeOutcome::Miss(format!("height {} not covered by the height map", h))
                }
                Err(e) => NativeOutcome::Err(-1, e),
            }
        }

        // ---- tri7 blockindex (disk frames) ----
        "getblock" => {
            // accepts a 64-hex hash OR an integer height (string or number —
            // legacy clients send both)
            let as_str = p_str(params, 0, "block hash/height");
            let as_num = p_u64(params, 0, "height");
            if as_str.is_err() && as_num.is_err() {
                return NativeOutcome::Err(-8, "expected block hash or height".into());
            }
            let (raw, height_from_num) = match (as_str, as_num) {
                (Ok(Some(s)), _) => (Some(s), None),
                (_, Ok(Some(n))) => (None, Some(n)),
                _ => return NativeOutcome::Err(-8, "expected block hash or height".into()),
            };
            let (is_hex_hash, height) = match &raw {
                Some(s) => {
                    let hex = s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit());
                    let h = if hex { None } else { s.parse::<u64>().ok() };
                    if !hex && h.is_none() {
                        return NativeOutcome::Err(
                            -8,
                            "invalid parameter — expected 64-hex block hash or integer height"
                                .into(),
                        );
                    }
                    (hex, h)
                }
                None => (false, height_from_num),
            };
            let verbose = ptry!(p_bool(params, 1, "verbose")).unwrap_or(true);
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            if !verbose {
                // legacy raw-hex form: getblock <hash|height> false
                let hd = if is_hex_hash { raw.as_deref() } else { None };
                let hh = if is_hex_hash { None } else { height };
                return match deps.blocks.raw_block(hd, hh) {
                    Ok(Some(hex)) => NativeOutcome::Ok(json!(hex)),
                    Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                    Err(e) => NativeOutcome::Err(-1, e),
                };
            }
            let res = if is_hex_hash {
                deps.blocks
                    .block_by_hash(raw.as_deref().expect("hex hash present"))
            } else {
                deps.blocks.block_by_height(height.expect("height present"))
            };
            match res {
                Ok(Some(v)) => NativeOutcome::Ok(v),
                Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                Err(e) => NativeOutcome::Err(-1, e),
            }
        }

        "getrawtransaction" => {
            let txid = match ptry!(p_str(params, 0, "txid")) {
                Some(s) => s,
                None => return NativeOutcome::Err(-8, "expected txid parameter".into()),
            };
            let verbose = ptry!(p_bool(params, 1, "verbose")).unwrap_or(false);
            if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
                return NativeOutcome::Err(-8, "invalid txid — expected 64 hex chars".into());
            }
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            match deps.blocks.tx(&txid, verbose) {
                Ok(Some(v)) => NativeOutcome::Ok(v),
                Ok(None) => {
                    NativeOutcome::Err(-5, "No information available about transaction".into())
                }
                Err(e) => NativeOutcome::Err(-1, e),
            }
        }

        // ---- tri7 tipstate window + difficulty module ----
        "getstakemodifier" => {
            let h = match ptry!(p_u64(params, 0, "height")) {
                Some(h) => h,
                None => return NativeOutcome::Err(-8, "expected height parameter".into()),
            };
            let (base, entries) = deps.tip.window();
            let tip = deps.tip.tip().height;
            if h < base || h > tip || entries == 0 {
                return NativeOutcome::Err(
                    -8,
                    format!("height {} outside tri7 window [{}..{}]", h, base, tip),
                );
            }
            match deps.tip.entry_at(h) {
                Some(e) => NativeOutcome::Ok(json!({
                    "height": e.height,
                    "modifier": format!("{:016x}", e.modifier),
                    "generated": e.generated,
                    "checksum": format!("{:08x}", e.checksum),
                    "proof_of_stake": e.pos,
                    "proofhash": e.proof,
                    "entropybit": e.entropy,
                })),
                None => NativeOutcome::Err(-1, "window entry missing (internal)".into()),
            }
        }

        "getdifficulty" => {
            // daemon parity shape; sources: tri7 tipstate window + difficulty
            // module (see difficulty_json)
            NativeOutcome::Ok(difficulty_json(deps))
        }

        "getconnectioncount" => NativeOutcome::Ok(json!(deps.tip.peers())),

        // ---- tri7 replay-derived wallet snapshots ----
        "getbalance" => match deps.wallet.balance() {
            Some(b) => NativeOutcome::Ok(json!(sats_to_tri(b.sats))),
            None => NativeOutcome::Err(
                -1,
                "wallet balance snapshot unavailable (balances file not loaded)".into(),
            ),
        },

        "listwalletaddresses" => {
            let entries = deps.wallet.address_entries();
            if entries.is_empty() {
                return NativeOutcome::Err(-1, "wallet balances snapshot unavailable".into());
            }
            NativeOutcome::Ok(json!(entries
                .iter()
                .map(|e| json!({
                    "address": e.address,
                    "balance_sats": e.balance_sats,
                    "balance": sats_to_tri(e.balance_sats),
                    "utxos": e.utxos,
                    "received_regular_sats": e.received_regular_sats,
                    "received_all_sats": e.received_all_sats,
                }))
                .collect::<Vec<_>>()))
        }

        "getreceivedbyaddress" => {
            // Source: tri7 cached replay balances (received_regular map from
            // balances.json — daemon P2PKH-script-only semantics).
            let addr = match ptry!(p_str(params, 0, "address")) {
                Some(a) => a,
                None => return NativeOutcome::Err(-8, "expected address parameter".into()),
            };
            // minconf accepted for daemon-compat; the snapshot is fully
            // confirmed at its coverage height, so minconf is informational
            let _minconf = ptry!(p_u64(params, 1, "minconf")).unwrap_or(1);
            // daemon parity: getreceivedbyaddress is WALLET-scoped — an address
            // not owned by the wallet reports 0 even if the chain paid it
            let sats = if deps.wallet.is_mine(&addr) {
                deps.wallet.received_regular(&addr).unwrap_or(0)
            } else {
                0
            };
            NativeOutcome::Ok(json!(sats_to_tri(sats)))
        }

        "validateaddress" => {
            let addr = match ptry!(p_str(params, 0, "address")) {
                Some(a) => a,
                None => return NativeOutcome::Err(-8, "expected address parameter".into()),
            };
            let mut o = Map::new();
            match decode_address(&addr) {
                Some((version, h160)) => {
                    o.insert("isvalid".into(), json!(true));
                    o.insert("address".into(), json!(addr));
                    o.insert("ismine".into(), json!(deps.wallet.is_mine(&addr)));
                    o.insert("version".into(), json!(version));
                    o.insert("pubkey_hash".into(), json!(hex_encode(&h160)));
                    o.insert(
                        "kind".into(),
                        json!(if version == crate::address::SCRIPT_ADDRESS {
                            "scripthash"
                        } else {
                            "pubkeyhash"
                        }),
                    );
                }
                None => {
                    o.insert("isvalid".into(), json!(false));
                    o.insert("address".into(), json!(addr));
                }
            }
            NativeOutcome::Ok(Value::Object(o))
        }

        "listunspent" => {
            let minconf = ptry!(p_u64(params, 0, "minconf")).unwrap_or(1);
            let maxconf = ptry!(p_u64(params, 1, "maxconf")).unwrap_or(9_999_999);
            // Coverage must be present AND consistent across BOTH snapshots —
            // a missing or mismatched pair rejects every row (never serve
            // confirmations keyed against the wrong height).
            let coverage = deps.wallet.coverage();
            let cov_h = match coverage {
                Some((h, _)) => h,
                None => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet snapshots unavailable or inconsistent (balance/UTXO coverage mismatch) — refresh with the balances bin".into(),
                    );
                }
            };
            let rows: Vec<Value> = deps
                .wallet
                .utxos()
                .iter()
                .filter(|u| {
                    let conf = cov_h.saturating_sub(u.height) + 1;
                    conf >= minconf && conf <= maxconf
                })
                .map(|u| {
                    let conf = cov_h.saturating_sub(u.height) + 1;
                    json!({
                        "txid": u.txid,
                        "vout": u.n,
                        "address": u.address,
                        "scriptPubKey": u.script_hex,
                        "amount": sats_to_tri(u.value_sats),
                        "amount_sats": u.value_sats,
                        "confirmations": conf,
                        "height": u.height,
                        "tx_time": u.tx_time,
                        "time": u.tx_time,
                        "blocktime": u.tx_time,
                        "spendable": true,
                        "solvable": true,
                        "prev_kind": u.prev_kind,
                    })
                })
                .collect();
            NativeOutcome::Ok(json!(rows))
        }

        // full wallet history: every receive/spend ever seen for these keys,
        // dated from the containing block (legacy client parity)
        "listwallettx" => {
            let rows = deps.wallet.history();
            let out: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "txid": r.txid,
                        "height": r.height,
                        "time": r.time,
                        "amount": sats_to_tri(r.value_sats),
                        "amount_sats": r.value_sats,
                        "address": r.address,
                        "direction": r.direction,
                        "category": r.direction,
                        "kind": r.kind,
                        "spent": r.spent,
                    })
                })
                .collect();
            NativeOutcome::Ok(json!({ "rows": out, "count": rows.len(), "source": "history.json" }))
        }

        // ---- composite ----
        "getinfo" => {
            let t = deps.tip.tip();
            let bal = deps.wallet.balance();
            NativeOutcome::Ok(json!({
                "version": TRI7_RPC_VERSION,
                "subversion": TRI7_SUBVERSION,
                "protocolversion": TRI7_PROTOCOL_VERSION,
                "walletversion": 60000,
                "balance": bal.as_ref().map(|b| sats_to_tri(b.sats)),
                "blocks": t.height,
                "timeoffset": 0,
                "connections": deps.tip.peers(),
                "proxy": "",
                "difficulty": difficulty_json(deps),
                "testnet": false,
                "moneysupply": bal.as_ref().map(|b| sats_to_tri(b.supply_sats)),
                "moneysupply_coverage_height": bal.as_ref().map(|b| b.coverage_height),
                "balance_coverage_height": bal.as_ref().map(|b| b.coverage_height),
                "paytxfee": 0.0,
                "errors": "",
            }))
        }

        "getsyncinfo" => {
            let t = deps.tip.tip();
            let sp = crate::sync_progress::snapshot();
            let (syncing, phase_str, body) = match sp {
                Some(s) => (
                    s.phase != crate::sync_progress::SyncPhase::Follow,
                    s.phase.as_str(),
                    json!({
                        "height": s.height,
                        "target_height": s.target_height,
                        "blocks_per_sec": s.blocks_per_sec,
                        "eta_secs": s.eta_secs,
                    }),
                ),
                None => (false, "follow", json!({})),
            };
            NativeOutcome::Ok(json!({
                "syncing": syncing,
                "phase": phase_str,
                "height": t.height,
                "body": body,
            }))
        }

        "health" => {
            let t = deps.tip.tip();
            let (base, entries) = deps.tip.window();
            let legacy = match deps.proxy {
                Some(p) => {
                    if p.probe() {
                        "up"
                    } else {
                        "down"
                    }
                }
                None => "disabled",
            };
            // real per-session connection records when the node ops engine is
            // wired (transport flags clearnet/tor/i2p); else the bare counter
            let peers_detail = match deps.ops {
                Some(o) if o.ready() => match o.call("getpeerinfo", &json!([])) {
                    Ok(v) => v,
                    Err(_) => json!([]),
                },
                _ => json!([]),
            };
            NativeOutcome::Ok(json!({
                "status": "ok",
                "height": t.height,
                "blockhash": t.blockhash,
                "daemon_tip": t.daemon_tip,
                "at_daemon_tip": t.daemon_tip.map_or(false, |dt| dt >= 0 && t.height == dt as u64),
                "state_write_age_secs": deps.tip.state_write_age_secs(),
                "window": {"base": base, "entries": entries},
                "peers": deps.tip.peers(),
                "peer_detail": peers_detail,
                "node_ops": match deps.ops { Some(o) => o.ready(), None => false },
                "uptime_secs": deps.tip.uptime_secs(),
                "block_index": deps.blocks.status_json(),
                "legacy_rpc": legacy,
                "native_methods": NATIVE_METHODS,
            }))
        }

        // ---- parity audit 2026-11-03: legacy info/util methods ----
        "help" => {
            // legacy tableRPC.help(): command list (one per line) with no
            // arg; tri7 has no per-command help text store, so help <cmd>
            // reports membership natively.
            let cmd = ptry!(p_str(params, 0, "command"));
            match cmd {
                None => {
                    let mut all: Vec<&str> = NATIVE_METHODS.to_vec();
                    all.extend_from_slice(PROXY_FALLBACK_METHODS);
                    all.sort_unstable();
                    NativeOutcome::Ok(json!(all.join("\n")))
                }
                Some(c) => {
                    if NATIVE_METHODS.contains(&c.as_str()) {
                        NativeOutcome::Ok(json!(format!("{} (tri7 native)", c)))
                    } else if PROXY_FALLBACK_METHODS.contains(&c.as_str()) {
                        NativeOutcome::Ok(json!(format!("{} (legacy proxy fallback)", c)))
                    } else {
                        NativeOutcome::Err(-1, format!("unknown command: {}", c))
                    }
                }
            }
        }

        "getblockheader" => {
            // legacy rpcblockchain getblockheader: header fields of the
            // verbose block object (source: tri7 blockindex frames), minus
            // the tx/size body fields. nMint/blocktrust/chaintrust live in
            // the legacy index only — emitted null, never guessed.
            let id = match (
                p_str(params, 0, "block hash/height"),
                p_u64(params, 0, "height"),
            ) {
                (Ok(Some(s)), _) => BlockId::Hash(s),
                (_, Ok(Some(n))) => BlockId::Height(n),
                _ => return NativeOutcome::Err(-8, "expected block hash or height".into()),
            };
            let verbose = ptry!(p_bool(params, 1, "verbose")).unwrap_or(true);
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            if !verbose {
                // legacy raw form: serialized 80-byte header hex — the first
                // 80 bytes of the raw block wire (header precedes vtx+sig).
                let (hd, hh) = match &id {
                    BlockId::Hash(s) => (Some(s.as_str()), None),
                    BlockId::Height(n) => (None, Some(*n)),
                };
                return match deps.blocks.raw_block(hd, hh) {
                    Ok(Some(hex)) => {
                        let bytes = hex_decode(&hex);
                        if bytes.len() < 80 {
                            NativeOutcome::Err(-1, "block shorter than a header".into())
                        } else {
                            NativeOutcome::Ok(json!(hex_encode(&bytes[..80])))
                        }
                    }
                    Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                    Err(e) => NativeOutcome::Err(-1, e),
                };
            }
            let res = match &id {
                BlockId::Hash(s) => deps.blocks.block_by_hash(s),
                BlockId::Height(n) => deps.blocks.block_by_height(*n),
            };
            let block = match res {
                Ok(Some(v)) => v,
                Ok(None) => return NativeOutcome::Err(-5, "Block not found".into()),
                Err(e) => return NativeOutcome::Err(-1, e),
            };
            // nextblockhash comes from the height map (h+1)
            let next = match block.get("height").and_then(Value::as_u64) {
                Some(h) => match deps.blocks.hash_at(h + 1) {
                    Ok(Some(s)) => json!(s),
                    _ => Value::Null,
                },
                None => Value::Null,
            };
            let get = |k: &str| block.get(k).cloned().unwrap_or(Value::Null);
            NativeOutcome::Ok(json!({
                "hash": get("hash"),
                "confirmations": get("confirmations"),
                "height": get("height"),
                "version": get("version"),
                "merkleroot": get("merkleroot"),
                "mint": Value::Null,
                "time": get("time"),
                "nonce": get("nonce"),
                "bits": get("bits"),
                "difficulty": get("difficulty"),
                "blocktrust": Value::Null,
                "chaintrust": Value::Null,
                "flags": get("flags"),
                "proofhash": get("proofhash"),
                "entropybit": get("entropybit"),
                "modifier": get("modifier"),
                "modifierchecksum": get("modifierchecksum"),
                "previousblockhash": get("previousblockhash"),
                "nextblockhash": next,
            }))
        }

        "getblockbynumber" => {
            // legacy alias of getblock <height> (rpcblockchain.cpp)
            let h = match ptry!(p_u64(params, 0, "height")) {
                Some(h) => h,
                None => return NativeOutcome::Err(-8, "expected height parameter".into()),
            };
            let verbose = ptry!(p_bool(params, 1, "verbose")).unwrap_or(true);
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            if !verbose {
                return match deps.blocks.raw_block(None, Some(h)) {
                    Ok(Some(hex)) => NativeOutcome::Ok(json!(hex)),
                    Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                    Err(e) => NativeOutcome::Err(-1, e),
                };
            }
            match deps.blocks.block_by_height(h) {
                Ok(Some(v)) => NativeOutcome::Ok(v),
                Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                Err(e) => NativeOutcome::Err(-1, e),
            }
        }

        "getblockchaininfo" => {
            // legacy rpcblockchain getblockchaininfo: chain identity + tip +
            // difficulty + supply (tri7's supply comes from the replay
            // snapshot with its coverage height; the legacy daemon reads its
            // full txdb — the coverage fields say exactly where this comes
            // from).
            let t = deps.tip.tip();
            let bal = deps.wallet.balance();
            NativeOutcome::Ok(json!({
                "chain": "main",
                "blocks": t.height,
                "bestblockhash": t.blockhash,
                "difficulty": difficulty_json(deps),
                "moneysupply": bal.as_ref().map(|b| sats_to_tri(b.supply_sats)),
                "moneysupply_sats": bal.as_ref().map(|b| b.supply_sats),
                "moneysupply_coverage_height": bal.as_ref().map(|b| b.coverage_height),
                "verificationprogress": 1.0,
                "chain_trust_note": "legacy chaintrust/blocks fields are full-index state tri7 does not build",
            }))
        }

        "getwalletinfo" => {
            // legacy rpcwallet getwalletinfo: tri7's wallet view is the
            // replay snapshot (balance/coverage/addresses/utxos). Legacy
            // keypool/txcount fields are emitted null — tri7 keeps no
            // keypool and its snapshot carries no tx-count.
            let bal = deps.wallet.balance();
            match bal {
                Some(b) => NativeOutcome::Ok(json!({
                    "walletversion": 60000,
                    "balance": sats_to_tri(b.sats),
                    "balance_sats": b.sats,
                    "txcount": Value::Null,
                    "keypoololdest": Value::Null,
                    "keypoolsize": Value::Null,
                    "unlocked_until": Value::Null,
                    "paytxfee": 0.0,
                    "coverage_height": b.coverage_height,
                    "coverage_hash": b.coverage_hash,
                    "addresses": b.addresses,
                    "utxos": b.utxos,
                })),
                None => NativeOutcome::Err(
                    -1,
                    "wallet balance snapshot unavailable (balances file not loaded)".into(),
                ),
            }
        }

        "getnetworkinfo" => {
            // legacy rpcnet getnetworkinfo
            NativeOutcome::Ok(json!({
                "version": TRI7_RPC_VERSION,
                "subversion": TRI7_SUBVERSION,
                "protocolversion": TRI7_PROTOCOL_VERSION,
                "timeoffset": 0,
                "connections": deps.tip.peers(),
                "uptime_secs": deps.tip.uptime_secs(),
                "transports": "clearnet+tor+i2p",
                "localaddresses": [],
                "warnings": "",
            }))
        }

        "getmininginfo" => {
            // legacy rpcmining getmininginfo: composed from the tri7 tip +
            // difficulty module + the live ops engine's staking stats when
            // wired (weight/pooledtx/search-interval). blockvalue is the PoW
            // subsidy schedule at the tip height (0 above h9001 — the live
            // chain mints PoS rewards, not PoW).
            let t = deps.tip.tip();
            let ops_info = match deps.ops {
                Some(o) if o.ready() => o.call("getstakinginfo", &json!([])).ok(),
                _ => None,
            };
            let ops_get = |k: &str| ops_info
                .as_ref()
                .and_then(|v| v.get(k))
                .cloned()
                .unwrap_or(Value::Null);
            NativeOutcome::Ok(json!({
                "blocks": t.height,
                "currentblocksize": ops_get("currentblocksize"),
                "currentblocktx": ops_get("currentblocktx"),
                "difficulty": difficulty_json(deps),
                "blockvalue": sats_to_tri(pow_subsidy_sats(t.height)),
                "netmhashps": 0.0,
                "netstakeweight": ops_get("netstakeweight"),
                "errors": "",
                "pooledtx": ops_get("pooledtx"),
                "stakeweight": {
                    "minimum": Value::Null,
                    "maximum": Value::Null,
                    "combined": ops_get("weight"),
                },
                "search-interval": ops_get("search-interval"),
                "stakeinterest": 0.33, // COIN_YEAR_REWARD = 33 * CENT (main.h:61)
                "testnet": false,
            }))
        }

        "getsubsidy" => {
            // legacy rpcmining getsubsidy [nTarget]: the PoW subsidy at the
            // tip (GetProofOfWorkReward schedule); the nTarget param is
            // parsed for compat and ignored exactly as the legacy one is.
            let _ = ptry!(p_str(params, 0, "nTarget"));
            let t = deps.tip.tip();
            NativeOutcome::Ok(json!(sats_to_tri(pow_subsidy_sats(t.height))))
        }

        "estimatefee" => {
            // legacy rpcblockchain estimatefee: "Triangles uses a static
            // minimum fee structure" — returns MIN_TX_FEE (= CENT/100 =
            // 100 sats = 0.0001 TRI, main.h:58/util.h:53). tri7 has no
            // nTransactionFee override, so the constant is exact.
            let _nblocks = ptry!(p_u64(params, 0, "nblocks"));
            NativeOutcome::Ok(json!(sats_to_tri(MIN_TX_FEE_SATS)))
        }

        "verifymessage" => {
            // legacy rpcwallet verifymessage <address> <signature> <message>
            // (strMessageMagic double-hash + compact recoverable sig; the
            // exact parity implementation lives in gui::state — the same
            // functions behind the GUI sign/verify pages).
            let addr = match ptry!(p_str(params, 0, "address")) {
                Some(a) => a,
                None => return NativeOutcome::Err(-8, "expected address parameter".into()),
            };
            let sig = match ptry!(p_str(params, 1, "signature")) {
                Some(s) => s,
                None => return NativeOutcome::Err(-8, "expected signature parameter".into()),
            };
            let msg = match ptry!(p_str(params, 2, "message")) {
                Some(m) => m,
                None => return NativeOutcome::Err(-8, "expected message parameter".into()),
            };
            if crate::address::address_to_payload(&addr).is_err() {
                return NativeOutcome::Err(-5, "Invalid address".into());
            }
            match crate::gui::state::verify_message(&addr, &msg, &sig) {
                Ok(ok) => NativeOutcome::Ok(json!(ok)),
                Err(e) => {
                    if e.contains("length") || e.contains("base64") {
                        NativeOutcome::Err(-5, "Malformed base64 encoding".into())
                    } else {
                        NativeOutcome::Err(-5, e)
                    }
                }
            }
        }

        m if OPS_METHODS.contains(&m) => match deps.ops {
            Some(o) if o.ready() => match o.call(m, params) {
                Ok(v) => NativeOutcome::Ok(v),
                Err((code, msg)) => NativeOutcome::Err(code, msg),
            },
            Some(_) => NativeOutcome::Err(-1, "node ops engine still warming up".into()),
            None => NativeOutcome::Err(
                -1,
                "node ops engine not wired — run tri7d with --wallet (staking/smsg/broadcast need wallet keys)".into(),
            ),
        },

        _ => NativeOutcome::Err(-32601, unknown_method_msg(method)),
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers — legacy-style JSON from blockindex hits. Used by the
// live adapter; unit-tested here with synthetic hits.
// ---------------------------------------------------------------------------

/// Which form a block id parameter arrived in (getblockheader dispatch).
enum BlockId {
    Hash(String),
    Height(u64),
}

/// Legacy MIN_TX_FEE (main.h:58 = (1 * CENT) / 100, util.h:53 CENT = 10000):
/// 100 sats = 0.0001 TRI — the static fee policy estimatefee reports.
pub const MIN_TX_FEE_SATS: i64 = 100;

/// Legacy GetProofOfWorkReward (main.cpp:1518-1536) — the PoW subsidy
/// schedule by height, in sats (COIN = 1e6). Height 0 (genesis) follows the
/// h<100 branch legacy takes for any best block below 100.
pub fn pow_subsidy_sats(height: u64) -> i64 {
    if height >= 9001 {
        0
    } else if height >= 7000 {
        10_000_000
    } else if height >= 3000 {
        5_000_000
    } else if height >= 1000 {
        10_000_000
    } else if height >= 100 {
        20_000_000
    } else {
        1_000_000
    }
}

/// Inverse of hex_encode (RPC hex-string params → bytes).
/// Contract: an odd-length string or any non-hex character yields an EMPTY
/// vec (all tri7 call sites treat empty as invalid) — this is a parse
/// helper, not a validator; use decode errors explicitly where rejection
/// must be distinguishable.
pub fn hex_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    if bytes.len() % 2 == 0 {
        for pair in bytes.chunks(2) {
            match (val(pair[0]), val(pair[1])) {
                (Some(h), Some(l)) => out.push((h << 4) | l),
                _ => return Vec::new(),
            }
        }
    }
    out
}

pub fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Legacy-parity amount conversion: the chain's COIN is 1e6 (consensus.rs),
/// so daemon doubles = sats / 1_000_000 — verified against the live daemon
/// (getinfo moneysupply 222549.124568 = 222549124568 sats / 1e6, and
/// getreceivedbyaddress vectors).
pub fn sats_to_tri(sats: i64) -> f64 {
    sats as f64 / 1_000_000.0
}

/// The daemon's own GetDifficulty (rpcmining.cpp double arithmetic) — this is
/// the exact formula legacy clients see, down to float rounding:
///   nShift = (bits >> 24) & 0xff
///   dDiff  = 0x0000ffff / (bits & 0x00ffffff)
///   dDiff *= 256^(29 - nShift)   (multiply below 29, divide above)
pub fn difficulty_from_bits(bits: u32) -> f64 {
    let n_shift = ((bits >> 24) & 0xff) as i32;
    let mut d = 0x0000_ffffu32 as f64 / (bits & 0x00ff_ffff) as f64;
    let mut s = n_shift;
    while s < 29 {
        d *= 256.0;
        s += 1;
    }
    while s > 29 {
        d /= 256.0;
        s -= 1;
    }
    d
}

/// Legacy-style verbose block JSON from a verified frame hit.
/// `tip_height` feeds confirmations; `next_hash` (already known or looked up)
/// feeds nextblockhash; `entry` carries the window's modifier state when the
/// block is inside the tipstate window.
pub fn format_block_json(
    hit: &BlockHit,
    tip_height: u64,
    next_hash: Option<String>,
    entry: Option<EntryView>,
) -> Value {
    let pos = block_is_proof_of_stake(&hit.block);
    let confirmations = hit.height.map(|h| tip_height.saturating_sub(h) + 1);
    let prev = if hit.block.prev_hash == [0u8; 32] {
        Value::Null
    } else {
        json!(internal_to_display(&hit.block.prev_hash))
    };
    let (modifier, checksum, proofhash, entropybit, flags) = match &entry {
        Some(e) => (
            json!(format!("{:016x}", e.modifier)),
            json!(format!("{:08x}", e.checksum)),
            json!(e.proof),
            json!(e.entropy),
            json!(if e.pos {
                if e.generated {
                    "proof-of-stake stake-modifier"
                } else {
                    "proof-of-stake"
                }
            } else if e.generated {
                "stake-modifier"
            } else {
                ""
            }),
        ),
        None => (
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            json!(if pos { "proof-of-stake" } else { "" }),
        ),
    };
    json!({
        "hash": internal_to_display(&hit.hash),
        "confirmations": confirmations,
        "size": hit.body_len,
        "height": hit.height,
        "version": hit.block.version,
        "merkleroot": internal_to_display(&hit.block.merkle_root),
        "tx": hit.block.vtx.iter().map(|t| internal_to_display(&t.hash())).collect::<Vec<_>>(),
        "time": hit.block.time,
        "nonce": hit.block.nonce,
        "bits": format!("{:08x}", hit.block.bits),
        "difficulty": difficulty_from_bits(hit.block.bits),
        "previousblockhash": prev,
        "nextblockhash": next_hash.map(Value::String).unwrap_or(Value::Null),
        "entropybit": entropybit,
        "modifier": modifier,
        "modifierchecksum": checksum,
        "proofhash": proofhash,
        "flags": flags,
    })
}

/// Legacy-style verbose tx JSON (+ the raw hex for non-verbose calls).
pub fn format_tx_json(hit: &TxHit, tip_height: u64) -> (Value, String) {
    let raw = hit.tx.serialize();
    let hex = hex_encode(&raw);
    let confirmations = hit.block_height.map(|h| tip_height.saturating_sub(h) + 1);
    let vin: Vec<Value> = hit
        .tx
        .vin
        .iter()
        .map(|v| {
            let coinbase = v.prev_hash == [0u8; 32] && v.prev_n == u32::MAX;
            if coinbase {
                json!({
                    "coinbase": hex_encode(&v.script_sig),
                    "sequence": v.sequence,
                })
            } else {
                json!({
                    "txid": internal_to_display(&v.prev_hash),
                    "vout": v.prev_n,
                    "scriptSig": {"hex": hex_encode(&v.script_sig)},
                    "sequence": v.sequence,
                })
            }
        })
        .collect();
    let vout: Vec<Value> = hit
        .tx
        .vout
        .iter()
        .enumerate()
        .map(|(n, o)| {
            let dest = script_to_dest(&o.script_pubkey);
            let (kind, asm, addresses) = match &dest {
                Some(ScriptDest::PubkeyHash(h)) => (
                    "pubkeyhash",
                    format!(
                        "OP_DUP OP_HASH160 {} OP_EQUALVERIFY OP_CHECKSIG",
                        hex_encode(h)
                    ),
                    vec![crate::address::payload_to_address(
                        &dest.as_ref().unwrap().payload(),
                    )],
                ),
                Some(ScriptDest::ScriptHash(h)) => (
                    "scripthash",
                    format!("OP_HASH160 {} OP_EQUAL", hex_encode(h)),
                    vec![crate::address::payload_to_address(
                        &dest.as_ref().unwrap().payload(),
                    )],
                ),
                Some(ScriptDest::Pubkey33(k)) => (
                    "pubkey",
                    format!("{} OP_CHECKSIG", hex_encode(k)),
                    vec![crate::address::payload_to_address(
                        &dest.as_ref().unwrap().payload(),
                    )],
                ),
                Some(ScriptDest::Pubkey65(k)) => (
                    "pubkey",
                    format!("{} OP_CHECKSIG", hex_encode(k)),
                    vec![crate::address::payload_to_address(
                        &dest.as_ref().unwrap().payload(),
                    )],
                ),
                None => {
                    if o.value == 0 && o.script_pubkey.is_empty() {
                        ("nulldata", String::new(), vec![])
                    } else {
                        ("nonstandard", hex_encode(&o.script_pubkey), vec![])
                    }
                }
            };
            let mut spk = Map::new();
            spk.insert("asm".into(), json!(asm));
            spk.insert("reqSigs".into(), json!(1));
            spk.insert("type".into(), json!(kind));
            if addresses.is_empty() {
                spk.insert("addresses".into(), Value::Null);
            } else {
                spk.insert(
                    "addresses".into(),
                    json!(addresses.iter().map(|a| a.as_str()).collect::<Vec<_>>()),
                );
            }
            // tri7 extension: the raw script hex (the daemon omits it)
            spk.insert("script_hex".into(), json!(hex_encode(&o.script_pubkey)));
            json!({
                "value": sats_to_tri(o.value),
                "value_sats": o.value,
                "n": n,
                "scriptPubKey": Value::Object(spk),
            })
        })
        .collect();
    let obj = json!({
        "hex": hex,
        "txid": internal_to_display(&hit.txid),
        "version": hit.tx.version,
        "time": hit.block_time,
        "tx_time": hit.tx.time,
        "locktime": hit.tx.locktime,
        "vin": vin,
        "vout": vout,
        "blockhash": internal_to_display(&hit.block_hash),
        "height": hit.block_height,
        "confirmations": confirmations,
    });
    (obj, hex)
}

// ---------------------------------------------------------------------------
// Address codec — decode side of address.rs's base58check encoder
// ---------------------------------------------------------------------------

fn base58_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut rank = [255u8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        rank[c as usize] = i as u8;
    }
    let bytes = s.as_bytes();
    let mut zeros = 0usize;
    while zeros < bytes.len() && bytes[zeros] == b'1' {
        zeros += 1;
    }
    // little-endian bignum accumulate, mirroring address.rs's encoder
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    for &c in &bytes[zeros..] {
        let val = rank[c as usize];
        if val == 255 {
            return None;
        }
        let mut carry = val as u32;
        for ch in out.iter_mut() {
            let v = (*ch as u32) * 58 + carry;
            *ch = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut full = vec![0u8; zeros];
    full.extend(out.iter().rev());
    Some(full)
}

/// base58check decode → (version byte, hash160 payload). Checks the 4-byte
/// sha256d checksum and the 25-byte length.
pub fn decode_address(addr: &str) -> Option<(u8, [u8; 20])> {
    let raw = base58_decode(addr)?;
    if raw.len() != 25 {
        return None;
    }
    let payload = &raw[..21];
    let check = &raw[21..];
    if &crate::legacy::sha256d(payload)[..4] != check {
        return None;
    }
    // Only mainnet pubkey (65) and script (28) versions are supported —
    // checksum-valid foreign-network or reserved versions must NOT validate.
    let version = payload[0];
    if version != 65 && version != 28 {
        return None;
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&payload[1..]);
    Some((version, h))
}

// ---------------------------------------------------------------------------
// HTTP layer
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct HttpServerConfig {
    pub debug: bool,
    pub max_body: usize,
    pub read_timeout: Duration,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        HttpServerConfig {
            debug: false,
            max_body: DEFAULT_MAX_BODY,
            read_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug)]
pub struct HttpReject {
    pub status: u16,
    pub message: String,
}

/// Read one POST request; returns the body string or an HTTP-level rejection.
pub fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<String, HttpReject> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    // header block (capped at 64 KB)
    let split = loop {
        if let Some(p) = find_split(&buf) {
            break p;
        }
        if buf.len() > 65_536 {
            return Err(HttpReject {
                status: 400,
                message: "request headers too large".into(),
            });
        }
        let n = stream.read(&mut chunk).map_err(|e| HttpReject {
            status: 408,
            message: format!("read: {}", e),
        })?;
        if n == 0 {
            return Err(HttpReject {
                status: 400,
                message: "connection closed before request complete".into(),
            });
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let mut lines = head.lines();
    let reqline = lines.next().unwrap_or("");
    if !reqline.starts_with("POST ") {
        return Err(HttpReject {
            status: 405,
            message: "only POST is supported (JSON-RPC)".into(),
        });
    }
    let mut content_length: Option<usize> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse::<usize>().ok();
            }
        }
    }
    let cl = content_length.ok_or_else(|| HttpReject {
        status: 400,
        message: "missing Content-Length".into(),
    })?;
    if cl > max_body {
        return Err(HttpReject {
            status: 413,
            message: format!("request body {} exceeds cap {} bytes", cl, max_body),
        });
    }
    let mut body = buf[split + 4..].to_vec();
    let deadline = Instant::now() + Duration::from_secs(30);
    while body.len() < cl {
        if Instant::now() > deadline {
            return Err(HttpReject {
                status: 408,
                message: "timed out reading request body".into(),
            });
        }
        let n = stream.read(&mut chunk).map_err(|e| HttpReject {
            status: 408,
            message: format!("body read: {}", e),
        })?;
        if n == 0 {
            return Err(HttpReject {
                status: 400,
                message: "connection closed mid-body".into(),
            });
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > max_body {
            return Err(HttpReject {
                status: 413,
                message: "request body exceeds cap".into(),
            });
        }
    }
    if body.len() > cl {
        body.truncate(cl); // pipelined extra bytes discarded (Connection: close)
    }
    String::from_utf8(body).map_err(|_| HttpReject {
        status: 400,
        message: "request body is not valid UTF-8".into(),
    })
}

fn find_split(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

pub fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text(status),
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())
}

/// Server state bundle (Arc'd across accept threads).
pub struct ServerDeps {
    pub tip: Arc<dyn TipProvider>,
    pub wallet: Arc<dyn WalletProvider>,
    pub blocks: Arc<dyn BlockDataProvider>,
    pub proxy: Option<Arc<dyn Proxy>>,
    pub ops: Option<Arc<dyn NodeOps>>,
    pub http: HttpServerConfig,
}

/// Accept loop — one thread per connection, one request per connection
/// (Connection: close). Loopback load is trivial; this keeps the code tiny.
pub fn serve(deps: Arc<ServerDeps>, listener: TcpListener) -> ! {
    for conn in listener.incoming() {
        let deps = Arc::clone(&deps);
        match conn {
            Ok(mut stream) => {
                std::thread::Builder::new()
                    .name("rpc-conn".into())
                    .spawn(move || {
                        let _ = stream.set_read_timeout(Some(deps.http.read_timeout));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
                        let started = Instant::now();
                        let body = match read_request(&mut stream, deps.http.max_body) {
                            Ok(b) => b,
                            Err(rej) => {
                                let msg = json!({
                                    "result": Value::Null,
                                    "error": {"code": -32700, "message": rej.message},
                                    "id": Value::Null,
                                    "source": "tri7"
                                })
                                .to_string();
                                let _ = write_response(&mut stream, rej.status, &msg);
                                return;
                            }
                        };
                        let d = Deps {
                            tip: &*deps.tip,
                            wallet: &*deps.wallet,
                            blocks: &*deps.blocks,
                            proxy: deps.proxy.as_deref(),
                            ops: deps.ops.as_deref(),
                        };
                        let (status, resp) = handle_body(&d, &body);
                        let _ = write_response(&mut stream, status, &resp);
                        if deps.http.debug {
                            // debug-level call log: method, status, duration
                            let method = serde_json::from_str::<Value>(&body)
                                .ok()
                                .and_then(|v| {
                                    v.get("method").and_then(|m| m.as_str()).map(String::from)
                                })
                                .unwrap_or_else(|| "<batch/parse-fail>".into());
                            eprintln!(
                                "[rpc-debug] {} status={} dur={:?} req_bytes={} resp_bytes={}",
                                method,
                                status,
                                started.elapsed(),
                                body.len(),
                                resp.len()
                            );
                        }
                    })
                    .ok();
            }
            Err(e) => {
                eprintln!("[rpc] accept error: {}", e);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    unreachable!("listener.incoming() never ends");
}

/// Bind strictly to a loopback address — refuses anything else.
pub fn bind_loopback(bind: &str, port: u16) -> Result<TcpListener, String> {
    let loopback = match bind {
        "localhost" => "127.0.0.1",
        b => b,
    };
    let is_loopback = loopback == "127.0.0.1" || loopback.starts_with("127.") || loopback == "::1";
    if !is_loopback {
        return Err(format!(
            "refusing to bind RPC to non-loopback address {} (loopback-only policy)",
            bind
        ));
    }
    TcpListener::bind((loopback, port)).map_err(|e| format!("bind {}:{}: {}", loopback, port, e))
}

// ---------------------------------------------------------------------------
// Legacy HTTP client (used by the transitional proxy adapter in follow_tip)
// ---------------------------------------------------------------------------

/// POST a JSON-RPC body to the legacy daemon; returns (http status, body).
/// Distinct error strings so callers can distinguish connect-refused from
/// timeout. The body is returned regardless of status (the daemon uses
/// HTTP 500 for JSON-RPC application errors with a valid envelope body).
pub fn legacy_post(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    body: &str,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let addr: std::net::SocketAddr = format!("{}:{}", host, port)
        .parse()
        .map_err(|e| format!("bad host:port {}: {}", host, e))?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect: {}", e))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set timeout: {}", e))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("set timeout: {}", e))?;
    let auth = format!(
        "Basic {}",
        base64_encode_fast(format!("{}:{}", user, pass).as_bytes())
    );
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        host,
        port,
        auth,
        body.len(),
        body
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    let mut buf = Vec::new();
    match stream.read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            return Err(format!(
                "timeout after {:?} (legacy daemon wedged or too slow)",
                timeout
            ))
        }
        Err(e) => return Err(format!("read: {}", e)),
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let split = text
        .find("\r\n\r\n")
        .ok_or("no header/body split in response")?;
    let status_line = text.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {}", status_line))?;
    Ok((status, text[split + 4..].to_string()))
}

fn base64_encode_fast(data: &[u8]) -> String {
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
// Tests — stub in-memory providers, no archive dependency
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const TIP_H: u64 = 2_204_064;
    const TIP_HASH: &str = "5b074b34b0b1a4074b8a1a765da89e6e18af6fdc12a24b80e2b41a0a4d9bb899";
    const WIN_BASE: u64 = 2_199_465;
    const MOD_H: u64 = 2_203_769;

    struct StubTip;
    impl TipProvider for StubTip {
        fn tip(&self) -> TipView {
            TipView {
                height: TIP_H,
                blockhash: TIP_HASH.into(),
                daemon_tip: Some(TIP_H as i64),
            }
        }
        fn window(&self) -> (u64, usize) {
            (WIN_BASE, 4600)
        }
        fn entry_at(&self, h: u64) -> Option<EntryView> {
            if h == MOD_H {
                Some(EntryView {
                    height: h,
                    hash: TIP_HASH.into(),
                    modifier: 0x1122_3344_5566_7788,
                    generated: true,
                    checksum: 0xaabb_ccdd,
                    pos: true,
                    proof: "aa".repeat(32),
                    entropy: 1,
                    time: 1_700_000_000,
                    bits: 0x2000_ffff,
                })
            } else {
                None
            }
        }
        fn last_bits(&self) -> Option<u32> {
            Some(0x2000_ffff)
        }
        fn next_target_bits(&self, pos: bool) -> Option<u32> {
            if pos {
                Some(0x2000_bb46) // differs from last_bits — proves independence
            } else {
                Some(0x2000_ffff)
            }
        }
        fn peers(&self) -> u32 {
            1
        }
        fn uptime_secs(&self) -> u64 {
            120
        }
        fn state_write_age_secs(&self) -> Option<u64> {
            Some(11)
        }
    }

    struct StubWallet {
        balance: Option<i64>,
        received: HashMap<String, i64>,
        mine: Vec<String>,
        utxos: Vec<UtxoRow>,
    }
    impl Default for StubWallet {
        fn default() -> Self {
            let mut received = HashMap::new();
            received.insert(
                "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY".to_string(),
                33_665_763i64,
            );
            StubWallet {
                balance: Some(3_036_990_591),
                received,
                mine: vec!["T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY".to_string()],
                utxos: vec![UtxoRow {
                    txid: "4d0be43cab1112d57fcfc2ea825e71bb00339ad0460fa9c8f57b56dec9f022f7".into(),
                    n: 0,
                    address: "TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7".into(),
                    script_hex: "76a91486cb0e9408ee60c88d72a126801a29904d7770df88ac".into(),
                    value_sats: 100,
                    height: 2_031_567,
                    tx_time: 1_700_000_000,
                    prev_kind: "regular".into(),
                }],
            }
        }
    }
    impl WalletProvider for StubWallet {
        fn balance(&self) -> Option<BalanceView> {
            self.balance.map(|sats| BalanceView {
                sats,
                supply_sats: 222_507_326_452,
                coverage_height: 2_201_090,
                coverage_hash: "f9031dee5b47c020534ba6c339b46aee7c25a5a14ea9588b4042689796cc6307"
                    .into(),
                addresses: 248,
                utxos: 177,
            })
        }
        fn coverage(&self) -> Option<(u64, String)> {
            Some((2_201_090, "f9031dee".into()))
        }
        fn address_entries(&self) -> Vec<AddressEntry> {
            vec![AddressEntry {
                address: "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY".into(),
                balance_sats: 33_665_763,
                utxos: 1,
                received_regular_sats: 33_665_763,
                received_all_sats: 33_665_763,
            }]
        }
        fn received_regular(&self, addr: &str) -> Option<i64> {
            self.received.get(addr).copied()
        }
        fn is_mine(&self, addr: &str) -> bool {
            self.mine.iter().any(|a| a == addr)
        }
        fn utxos(&self) -> Vec<UtxoRow> {
            self.utxos.clone()
        }
    }

    struct StubBlocks {
        ready: bool,
    }
    impl BlockDataProvider for StubBlocks {
        fn ready(&self) -> bool {
            self.ready
        }
        fn status_json(&self) -> Value {
            if self.ready {
                json!({"state": "ready", "frames": 4, "heights_ok": true})
            } else {
                json!({"state": "building", "frames_done": 2})
            }
        }
        fn block_by_hash(&self, _h: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn block_by_height(&self, _h: u64) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn tx(&self, _t: &str, _v: bool) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn hash_at(&self, h: u64) -> Result<Option<String>, String> {
            if h <= 10 {
                Ok(Some(format!("{:064x}", h)))
            } else {
                Ok(None)
            }
        }
    }

    #[derive(Default)]
    struct StubProxy {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }
    impl Proxy for StubProxy {
        fn forward(&self, req: &Value) -> Result<(u16, Value), String> {
            self.calls
                .lock()
                .unwrap()
                .push(req["method"].as_str().unwrap_or("").to_string());
            if self.fail {
                return Err("connect: connection refused".into());
            }
            Ok((
                200,
                json!({"result": {"echo": req["method"]}, "error": Value::Null, "id": req.get("id").cloned().unwrap_or(Value::Null)}),
            ))
        }
        fn describe(&self) -> String {
            "legacy 127.0.0.1:19112".into()
        }
        fn probe(&self) -> bool {
            !self.fail
        }
    }

    fn deps<'a>(
        tip: &'a StubTip,
        wallet: &'a StubWallet,
        blocks: &'a StubBlocks,
        proxy: Option<&'a StubProxy>,
    ) -> Deps<'a> {
        Deps {
            tip,
            wallet,
            blocks,
            proxy: proxy.map(|p| p as &dyn Proxy),
            ops: None,
        }
    }

    fn call(deps: &Deps, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc": "1.0", "id": 7, "method": method, "params": params});
        let (_, out) = handle_body(deps, &body.to_string());
        serde_json::from_str(&out).unwrap()
    }

    #[test]
    fn parse_error_returns_32700() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let (status, out) = handle_body(&d, "this is not json");
        assert_eq!(status, 200);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], -32700);
        assert_eq!(v["source"], "tri7");
    }

    #[test]
    fn batch_request_yields_array_of_envelopes() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let body = json!([
            {"id": 1, "method": "getblockcount"},
            {"id": 2, "method": "gettip"},
            "garbage-element",
        ])
        .to_string();
        let (_, out) = handle_body(&d, &body);
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["result"], json!(TIP_H));
        assert_eq!(arr[0]["source"], "tri7");
        assert_eq!(arr[1]["result"]["height"], json!(TIP_H));
        assert_eq!(arr[2]["error"]["code"], -32600);
    }

    #[test]
    fn empty_batch_is_invalid_request() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let (_, out) = handle_body(&d, "[]");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], -32600);
    }

    #[test]
    fn gettip_and_getblockcount_from_tipstate() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "gettip", json!([]));
        assert_eq!(v["result"]["height"], json!(TIP_H));
        assert_eq!(v["result"]["blockhash"], json!(TIP_HASH));
        assert_eq!(v["result"]["at_daemon_tip"], json!(true));
        let v = call(&d, "getblockcount", json!([]));
        assert_eq!(v["result"], json!(TIP_H));
        // id passthrough
        assert_eq!(v["id"], 7);
    }

    #[test]
    fn getbestblockhash_native_from_tipstate() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getbestblockhash", json!([]));
        assert_eq!(v["result"], json!(TIP_HASH));
        // it must be a registered native method (never proxy-forwarded)
        assert!(NATIVE_METHODS.contains(&"getbestblockhash"));
    }

    #[test]
    fn getstakemodifier_in_and_out_of_window() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        // positional
        let v = call(&d, "getstakemodifier", json!([MOD_H]));
        assert_eq!(v["result"]["modifier"], "1122334455667788");
        assert_eq!(v["result"]["checksum"], "aabbccdd");
        assert_eq!(v["result"]["generated"], true);
        // named
        let v = call(&d, "getstakemodifier", json!({"height": MOD_H}));
        assert_eq!(v["result"]["modifier"], "1122334455667788");
        // outside window
        let v = call(&d, "getstakemodifier", json!([100]));
        assert_eq!(v["error"]["code"], -8);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside tri7 window"));
    }

    #[test]
    fn getdifficulty_uses_daemon_arithmetic_and_shape() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getdifficulty", json!([]));
        // daemon shape: an object with both proof types; "proof-of-stake"
        // comes from LAST block bits (0x2000ffff in the stub) → exactly 2^-24
        let diff = v["result"]["proof-of-stake"].as_f64().unwrap();
        let expect = 1.0 / (1u64 << 24) as f64;
        assert_eq!(diff, expect, "got {}", diff);
        // pow comes from the next required PoW target (same stub value here)
        assert_eq!(v["result"]["proof-of-work"], v["result"]["proof-of-stake"]);
        // the daemon's own formula, reproduced exactly:
        assert_eq!(difficulty_from_bits(0x1d00ffff), 1.0);
        assert_eq!(difficulty_from_bits(0x2000ffff), 1.0 / (1u64 << 24) as f64);
        assert_eq!(
            difficulty_from_bits(0x2000bb46),
            8.147741845052824e-08 // live daemon vector (tip block)
        );
        // monotonic: tighter target → higher difficulty
        assert!(difficulty_from_bits(0x1d00ffff) > difficulty_from_bits(0x2000ffff));
    }

    #[test]
    fn getbalance_matches_replay_total() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getbalance", json!([]));
        assert_eq!(v["source"], "tri7");
        // balances.json wallet total 3,036,990,591 raw units; COIN = 1e6
        // (consensus.rs) → 3036.990591, matching the daemon's double display
        assert_eq!(v["result"], json!(3036.990591));
        // missing snapshot → clear error
        let mut w2 = StubWallet::default();
        w2.balance = None;
        let d2 = deps(&t, &w2, &b, None);
        let v = call(&d2, "getbalance", json!([]));
        assert_eq!(v["error"]["code"], -1);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("snapshot unavailable"));
    }

    #[test]
    fn listwalletaddresses_reports_entries() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listwalletaddresses", json!([]));
        let arr = v["result"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["address"], "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY");
        assert_eq!(arr[0]["balance_sats"], 33_665_763i64);
    }

    #[test]
    fn getreceivedbyaddress_known_and_unknown() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(
            &d,
            "getreceivedbyaddress",
            json!(["T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY", 0]),
        );
        // wallet-owned, 33,665,763 received → 33.665763 (COIN = 1e6)
        assert_eq!(v["result"], json!(33.665763));
        // never received → 0
        let v = call(
            &d,
            "getreceivedbyaddress",
            json!(["TAD4jcQzsz9bDcAMDcJjsxJaH7schCUk8g"]),
        );
        assert_eq!(v["result"], json!(0.0));
        // WALLET-SCOPED parity: an address the chain paid but the wallet does
        // not own reports 0 (matches the live daemon's behavior)
        let mut w2 = StubWallet::default();
        w2.mine.clear(); // T9yWe13... no longer wallet-owned
        w2.received
            .insert("T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY".into(), 33_665_763);
        let d2 = deps(&t, &w2, &b, None);
        let v = call(
            &d2,
            "getreceivedbyaddress",
            json!(["T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY", 0]),
        );
        assert_eq!(v["result"], json!(0.0));
    }

    #[test]
    fn validateaddress_decodes_and_flags_mine() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        // wallet address from the live snapshot (P2PKH, version 65)
        let v = call(
            &d,
            "validateaddress",
            json!(["T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY"]),
        );
        assert_eq!(v["result"]["isvalid"], true);
        assert_eq!(v["result"]["ismine"], true);
        assert_eq!(v["result"]["version"], 65);
        assert_eq!(v["result"]["kind"], "pubkeyhash");
        // valid but not wallet-owned
        let v = call(
            &d,
            "validateaddress",
            json!(["TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7"]),
        );
        assert_eq!(v["result"]["isvalid"], true);
        assert_eq!(v["result"]["ismine"], false);
        // garbage
        let v = call(&d, "validateaddress", json!(["NOT_AN_ADDRESS"]));
        assert_eq!(v["result"]["isvalid"], false);
        // checksum tamper on an otherwise valid string
        let v = call(
            &d,
            "validateaddress",
            json!(["T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyZ"]),
        );
        assert_eq!(v["result"]["isvalid"], false);
    }

    #[test]
    fn listunspent_filters_by_confirmations() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listunspent", json!([]));
        let arr = v["result"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        // coverage h2201090, utxo at h2031567 → conf = 169524
        assert_eq!(arr[0]["confirmations"], 169_524i64);
        assert_eq!(arr[0]["amount_sats"], 100);
        assert_eq!(arr[0]["amount"], json!(0.0001));
        let v = call(&d, "listunspent", json!([170000]));
        assert_eq!(v["result"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn getinfo_composite_fields() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getinfo", json!([]));
        assert_eq!(v["result"]["blocks"], json!(TIP_H));
        assert_eq!(v["result"]["connections"], 1);
        assert_eq!(v["result"]["balance"], json!(3036.990591));
        assert_eq!(v["result"]["protocolversion"], TRI7_PROTOCOL_VERSION);
        assert_eq!(v["source"], "tri7");
    }

    #[test]
    fn health_reports_all_subsystems() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "health", json!([]));
        assert_eq!(v["result"]["status"], "ok");
        assert_eq!(v["result"]["legacy_rpc"], "up");
        assert_eq!(v["result"]["block_index"]["state"], "ready");
        assert_eq!(
            v["result"]["native_methods"].as_array().unwrap().len(),
            NATIVE_METHODS.len()
        );
    }

    #[test]
    fn unknown_method_rejected_and_not_forwarded() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "sendtoaddress", json!(["addr", 1.0]));
        assert_eq!(v["error"]["code"], -32601);
        // SECURITY: wallet-write methods must never hit the proxy
        assert!(p.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn proxy_only_method_is_forwarded_and_marked() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "getrawmempool", json!([]));
        assert_eq!(v["source"], "legacy-proxy");
        assert_eq!(v["result"]["echo"], "getrawmempool");
        assert_eq!(p.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn native_miss_falls_back_to_proxy_while_warming() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: false };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "getblock", json!(["ab".repeat(32)]));
        assert_eq!(v["source"], "legacy-proxy");
        assert_eq!(v["result"]["echo"], "getblock");
        let v = call(&d, "getrawtransaction", json!(["ab".repeat(32), 1]));
        assert_eq!(v["source"], "legacy-proxy");
        // height beyond the stub height map → fallback too
        let v = call(&d, "getblockhash", json!([999_999_999]));
        assert_eq!(v["source"], "legacy-proxy");
    }

    #[test]
    fn native_miss_without_proxy_is_a_clear_error() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: false };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getblock", json!(["ab".repeat(32)]));
        assert_eq!(v["error"]["code"], -1);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no legacy proxy configured (cutover mode)"));
    }

    #[test]
    fn proxy_transport_error_is_reported() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let mut p = StubProxy::default();
        p.fail = true;
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "getrawmempool", json!([]));
        assert_eq!(v["error"]["code"], -32001);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("connection refused"));
    }

    #[test]
    fn not_found_when_index_ready_is_native_error() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "getblock", json!(["ab".repeat(32)]));
        assert_eq!(v["error"]["code"], -5);
        assert_eq!(v["source"], "tri7");
        assert!(p.calls.lock().unwrap().is_empty()); // no pointless legacy round-trip
    }

    // ---- HTTP layer tests (real sockets, loopback) ----

    #[test]
    fn legacy_post_times_out_against_hung_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let child = std::thread::spawn(move || {
            // accept and HOLD the connection open, never respond — the
            // client must hit its read timeout, not an EOF
            let _conn = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let started = Instant::now();
        let res = legacy_post(
            "127.0.0.1",
            port,
            "u",
            "p",
            "{}",
            Duration::from_millis(300),
        );
        assert!(res.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(res.unwrap_err().contains("timeout"));
        child.join().unwrap();
    }

    #[test]
    fn legacy_post_preserves_non_200_status_and_body() {
        use std::io::Write as _;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let child = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf); // drain request
            let body = r#"{"result":null,"error":{"code":-5,"message":"nope"},"id":1}"#;
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            s.write_all(resp.as_bytes()).unwrap();
        });
        let (status, body) =
            legacy_post("127.0.0.1", port, "u", "p", "{}", Duration::from_secs(5)).unwrap();
        child.join().unwrap();
        assert_eq!(status, 500);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], -5);
    }

    #[test]
    fn read_request_rejects_oversized_body() {
        use std::io::Write as _;
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx_chan, rx) = mpsc::channel();
        let child = std::thread::spawn(move || {
            // SERVER side: read_request is what tri7d runs on inbound sockets
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let res = read_request(&mut s, DEFAULT_MAX_BODY);
            tx_chan.send(res.map(|b| b.len())).unwrap();
        });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let hdr = "POST / HTTP/1.1\r\nContent-Length: 2000000\r\n\r\n";
        stream.write_all(hdr.as_bytes()).unwrap();
        let res = rx.recv().unwrap();
        match res {
            Err(rej) => {
                assert_eq!(rej.status, 413);
                assert!(rej.message.contains("exceeds cap"));
            }
            Ok(n) => panic!("oversized request must be rejected, got {} bytes", n),
        }
        child.join().unwrap();
    }

    #[test]
    fn read_request_parses_simple_post() {
        use std::io::Write as _;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = r#"{"method":"getblockcount"}"#;
        let child = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let req = format!(
                "POST / HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            s.write_all(req.as_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let got = read_request(&mut stream, DEFAULT_MAX_BODY).unwrap();
        assert_eq!(got, body);
        child.join().unwrap();
    }

    #[test]
    fn bind_loopback_refuses_non_loopback() {
        assert!(bind_loopback("0.0.0.0", 19999).is_err());
        assert!(bind_loopback("192.168.1.5", 19999).is_err());
        let l = bind_loopback("127.0.0.1", 0).unwrap();
        assert!(l.local_addr().is_ok());
    }

    // ---- formatting helper tests (synthetic hits, no chain data) ----

    #[test]
    fn format_tx_json_verbose_and_hex_agree() {
        use crate::legacy::{Tx, Vin, Vout};
        let tx = Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [9u8; 32],
                prev_n: 3,
                script_sig: vec![0xab, 0xcd],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 123_456_789,
                script_pubkey: {
                    // P2PKH to hash160 00112233445566778899aabbccddeeff00112233
                    let mut s = vec![0x76, 0xa9, 0x14];
                    s.extend_from_slice(&[
                        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                        0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33,
                    ]);
                    s.extend_from_slice(&[0x88, 0xac]);
                    s
                },
            }],
            locktime: 0,
        };
        let hit = TxHit {
            tx: tx.clone(),
            txid: crate::legacy::sha256d(&tx.serialize()),
            block_hash: [7u8; 32],
            block_height: Some(2_000_000),
            block_time: 1_700_000_500,
        };
        let (obj, hex) = format_tx_json(&hit, 2_001_000);
        // hex round-trip
        assert_eq!(hex.len(), tx.serialize().len() * 2);
        // confirmations vs tip
        assert_eq!(obj["confirmations"], 1001i64);
        // daemon parity: "time" is the containing BLOCK time; tx nTime rides
        // as "tx_time"
        assert_eq!(obj["time"], 1_700_000_500i64);
        assert_eq!(obj["tx_time"], 1_700_000_000i64);
        assert_eq!(obj["vin"][0]["txid"], internal_to_display(&[9u8; 32]));
        assert_eq!(obj["vout"][0]["value_sats"], 123_456_789i64);
        assert_eq!(obj["vout"][0]["scriptPubKey"]["type"], "pubkeyhash");
        assert_eq!(
            obj["vout"][0]["scriptPubKey"]["addresses"][0],
            crate::address::payload_to_address(
                &ScriptDest::PubkeyHash([
                    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
                    0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33,
                ])
                .payload()
            )
        );
    }

    #[test]
    fn format_block_json_fields() {
        use crate::legacy::{Block, Tx, Vin, Vout};
        let block = Block {
            version: 2,
            prev_hash: [1u8; 32],
            merkle_root: [2u8; 32],
            time: 1_700_000_000,
            bits: 0x2000ffff,
            nonce: 42,
            vtx: vec![Tx {
                version: 1,
                time: 1_700_000_000,
                vin: vec![Vin {
                    prev_hash: [0u8; 32],
                    prev_n: u32::MAX,
                    script_sig: vec![],
                    sequence: u32::MAX,
                }],
                vout: vec![Vout {
                    value: 0,
                    script_pubkey: vec![],
                }],
                locktime: 0,
            }],
            block_sig: vec![],
        };
        let hit = BlockHit {
            block,
            hash: [5u8; 32],
            off: 0,
            body_len: 200,
            height: Some(2_199_500),
        };
        let entry = EntryView {
            height: 2_199_500,
            hash: "x".into(),
            modifier: 0xdead_beef_cafe_babe,
            generated: true,
            checksum: 0x01020304,
            pos: true,
            proof: "ee".repeat(32),
            entropy: 0,
            time: 1_700_000_000,
            bits: 0x2000ffff,
        };
        let v = format_block_json(&hit, 2_204_064, Some("next".into()), Some(entry));
        assert_eq!(v["height"], 2_199_500i64);
        assert_eq!(v["confirmations"], 4565i64);
        assert_eq!(v["modifier"], "deadbeefcafebabe");
        assert_eq!(v["modifierchecksum"], "01020304");
        assert_eq!(v["flags"], "proof-of-stake stake-modifier");
        assert_eq!(v["bits"], "2000ffff");
        assert_eq!(v["nextblockhash"], "next");
        // no entry → null modifier family
        let v2 = format_block_json(&hit, 2_204_064, None, None);
        assert_eq!(v2["modifier"], Value::Null);
        assert_eq!(v2["nextblockhash"], Value::Null);
    }

    // ---- SnapshotWallet::coverage STRICT gate (REAL loaders + temp files) ----

    const COV_H: u64 = 2_201_090;
    const COV_HASH: &str = "f9031dee5b47c020534ba6c339b46aee7c25a5a14ea9588b4042689796cc6307";
    const COV_WALLET_ADDR: &str = "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY";

    fn cov_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tri7_cov_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_snap(path: &PathBuf, v: &Value) {
        std::fs::write(path, serde_json::to_string(v).unwrap()).unwrap();
    }

    /// balances.json body with one funded wallet address (nonempty wallet list
    /// is what marks the snapshot `loaded`).
    fn balances_body(height: u64, hash: &str) -> Value {
        json!({
            "height": height,
            "covered_tip_hash": hash,
            "totals": {
                "wallet_balance_sats": 33_665_763i64,
                "total_balance_sats": 222_507_326_452i64
            },
            "wallet": [{
                "address": COV_WALLET_ADDR,
                "balance_sats": 33_665_763i64,
                "utxos": 1u64,
                "received_regular_sats": 33_665_763i64,
                "received_all_sats": 33_665_763i64
            }],
            "received_regular": { COV_WALLET_ADDR: 33_665_763i64 }
        })
    }

    /// wallet_utxos.json body with `rows` UTXOs (0 rows = valid zero-UTXO
    /// snapshot; row height matches the stub listunspent fixture: conf 169524).
    fn utxos_body(height: u64, hash: &str, rows: usize) -> Value {
        let utxos: Vec<Value> = (0..rows)
            .map(|i| {
                json!({
                    "txid": format!("{:064x}", i),
                    "n": 0u32,
                    "address": COV_WALLET_ADDR,
                    "script_hex": "76a91486cb0e9408ee60c88d72a126801a29904d7770df88ac",
                    "value_sats": 100i64,
                    "height": 2_031_567u64,
                    "prev_kind": "regular"
                })
            })
            .collect();
        json!({ "height": height, "covered_tip_hash": hash, "utxos": utxos })
    }

    /// Write both snapshot files and load them through the REAL loaders.
    fn load_pair(dir: &PathBuf, bal: &Value, utx: &Value) -> SnapshotWallet {
        let bp = dir.join("balances.json");
        let up = dir.join("wallet_utxos.json");
        write_snap(&bp, bal);
        write_snap(&up, utx);
        SnapshotWallet {
            balances: load_balances_snap(&bp),
            utxos: load_utxos_snap(&up),
        }
    }

    #[test]
    fn coverage_accepts_consistent_snapshot_pair() {
        let dir = cov_dir("ok");
        let w = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H, COV_HASH, 2),
        );
        assert!(w.balances.loaded && w.utxos.loaded);
        assert_eq!(w.coverage(), Some((COV_H, COV_HASH.to_string())));
    }

    #[test]
    fn coverage_accepts_zero_utxo_snapshot() {
        let dir = cov_dir("zero");
        let w = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H, COV_HASH, 0),
        );
        // an empty wallet_utxos.json is LOADED state, not an error
        assert!(w.utxos.loaded);
        assert!(w.utxos.rows.is_empty());
        assert_eq!(w.coverage(), Some((COV_H, COV_HASH.to_string())));
    }

    #[test]
    fn coverage_rejects_missing_coverage_hash() {
        let dir = cov_dir("nohash");
        // empty hash on the UTXO side
        let w = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H, "", 1),
        );
        assert_eq!(w.coverage(), None);
        // empty hash on the balances side
        let w2 = load_pair(
            &dir,
            &balances_body(COV_H, ""),
            &utxos_body(COV_H, COV_HASH, 1),
        );
        assert_eq!(w2.coverage(), None);
        // empty on both sides
        let w3 = load_pair(&dir, &balances_body(COV_H, ""), &utxos_body(COV_H, "", 1));
        assert_eq!(w3.coverage(), None);
    }

    #[test]
    fn coverage_rejects_height_mismatch() {
        let dir = cov_dir("hmiss");
        let w = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H + 1, COV_HASH, 1),
        );
        assert_eq!(w.coverage(), None);
    }

    #[test]
    fn coverage_rejects_hash_mismatch() {
        let dir = cov_dir("xmiss");
        let w = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H, &"ab".repeat(32), 1),
        );
        assert_eq!(w.coverage(), None);
    }

    #[test]
    fn coverage_rejects_unloaded_balances() {
        let dir = cov_dir("nobal");
        // balances.json present but wallet list empty → loaded=false
        let mut bal = balances_body(COV_H, COV_HASH);
        bal["wallet"] = json!([]);
        bal["received_regular"] = json!({});
        let w = load_pair(&dir, &bal, &utxos_body(COV_H, COV_HASH, 1));
        assert!(!w.balances.loaded);
        assert_eq!(w.coverage(), None);
    }

    #[test]
    fn coverage_rejects_unloaded_utxos() {
        let dir = cov_dir("noutx");
        let bp = dir.join("balances.json");
        write_snap(&bp, &balances_body(COV_H, COV_HASH));
        let missing = dir.join("wallet_utxos_missing.json");
        let w = SnapshotWallet {
            balances: load_balances_snap(&bp),
            utxos: load_utxos_snap(&missing),
        };
        assert!(!w.utxos.loaded);
        assert_eq!(w.coverage(), None);
    }

    /// End-to-end: the listunspent handler over the REAL SnapshotWallet —
    /// consistent pair serves rows; any inconsistency is a hard RPC error.
    #[test]
    fn listunspent_gates_on_real_snapshot_coverage() {
        let t = StubTip;
        let b = StubBlocks { ready: true };
        let dir = cov_dir("rpc");
        let good = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H, COV_HASH, 1),
        );
        let d = Deps {
            tip: &t,
            wallet: &good,
            blocks: &b,
            proxy: None,
            ops: None,
        };
        let v = call(&d, "listunspent", json!([]));
        let arr = v["result"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["confirmations"], 169_524i64); // h2201090 - h2031567 + 1
        assert_eq!(arr[0]["amount_sats"], 100);

        // height mismatch → error envelope, never rows
        let bad_h = load_pair(
            &dir,
            &balances_body(COV_H, COV_HASH),
            &utxos_body(COV_H + 7, COV_HASH, 1),
        );
        let d = Deps {
            tip: &t,
            wallet: &bad_h,
            blocks: &b,
            proxy: None,
            ops: None,
        };
        let v = call(&d, "listunspent", json!([]));
        assert!(v["error"].is_object());
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("coverage mismatch"));
        assert!(v.get("result").unwrap_or(&Value::Null).is_null());

        // unloaded UTXO side → same hard error
        let bp = dir.join("balances2.json");
        write_snap(&bp, &balances_body(COV_H, COV_HASH));
        let no_utx = SnapshotWallet {
            balances: load_balances_snap(&bp),
            utxos: load_utxos_snap(&dir.join("absent.json")),
        };
        let d = Deps {
            tip: &t,
            wallet: &no_utx,
            blocks: &b,
            proxy: None,
            ops: None,
        };
        let v = call(&d, "listunspent", json!([]));
        assert!(v["error"].is_object());
    }


use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Node ops stub recording the last call.
struct StubOps {
    live: AtomicBool,
}
impl StubOps {
    fn new(live: bool) -> Self {
        StubOps {
            live: AtomicBool::new(live),
        }
    }
}
impl NodeOps for StubOps {
    fn call(&self, method: &str, _params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "sendrawtransaction" => Ok(json!({"txid": "abc123", "announced_to": 2})),
            "getstakinginfo" => Ok(json!({"enabled": true, "staking": false, "weight": 42})),
            "staking" => Ok(json!({"staking": true})),
            "getpeerinfo" => Ok(json!({"count": 0, "by_transport": {}, "peers": []})),
            _ => Err((-3, format!("stub: unhandled {}", method))),
        }
    }
    fn ready(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }
}

struct T;
impl TipProvider for T {
    fn tip(&self) -> TipView {
        TipView { height: 5, blockhash: "ab".repeat(32), daemon_tip: Some(5) }
    }
    fn window(&self) -> (u64, usize) { (0, 6) }
    fn entry_at(&self, _h: u64) -> Option<EntryView> { None }
    fn last_bits(&self) -> Option<u32> { Some(0x2000ffff) }
    fn next_target_bits(&self, _pos: bool) -> Option<u32> { None }
    fn peers(&self) -> u32 { 1 }
    fn uptime_secs(&self) -> u64 { 1 }
    fn state_write_age_secs(&self) -> Option<u64> { None }
}

#[test]
fn ops_methods_route_to_the_engine_and_never_to_the_proxy() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let ops = StubOps::new(true);
    let d = Deps {
        tip: &t,
        wallet: &w,
        blocks: &b,
        proxy: None,
        ops: Some(&ops),
    };
    let v = call(&d, "sendrawtransaction", json!(["deadbeef"]));
    assert_eq!(v["result"]["txid"], "abc123");
    assert_eq!(v["result"]["announced_to"], 2);
    assert_eq!(v["source"], "tri7");
    let v = call(&d, "getstakinginfo", json!([]));
    assert_eq!(v["result"]["weight"], 42);
    let v = call(&d, "staking", json!(["on"]));
    assert_eq!(v["result"]["staking"], true);
    // NATIVE list must contain every ops method (routing completeness)
    for m in OPS_METHODS {
        assert!(
            NATIVE_METHODS.contains(m),
            "{} in OPS_METHODS but not NATIVE_METHODS",
            m
        );
    }
}

#[test]
fn ops_not_wired_is_a_clear_native_error_and_getstakinginfo_is_never_proxied() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d, "smsginbox", json!([]));
    assert_eq!(v["error"]["code"], -1);
    assert!(v["error"]["message"].as_str().unwrap().contains("--wallet"));
    // getstakinginfo left the proxy fallback set when it went native
    assert!(!PROXY_FALLBACK_METHODS.contains(&"getstakinginfo"));
    assert!(NATIVE_METHODS.contains(&"getstakinginfo"));
    // warming engine → distinct message
    let ops = StubOps::new(false);
    let d2 = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
    let v = call(&d2, "getstakinginfo", json!([]));
    assert!(v["error"]["message"].as_str().unwrap().contains("warming up"));
}

#[test]
fn health_carries_peer_detail_and_node_ops_flags() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let ops = StubOps::new(true);
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
    let v = call(&d, "health", json!([]));
    assert_eq!(v["result"]["node_ops"], true);
    assert!(v["result"]["peer_detail"].is_object());
    let d2 = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d2, "health", json!([]));
    assert_eq!(v["result"]["node_ops"], false);
}

// ---- parity audit 2026-11-03: legacy info/util RPCs ----

/// Block provider returning a realistic verbose block JSON + raw hex,
/// mirroring the live adapter's shapes.
struct BlocksJson;
impl BlockDataProvider for BlocksJson {
    fn ready(&self) -> bool {
        true
    }
    fn status_json(&self) -> Value {
        json!({"state": "ready"})
    }
    fn block_by_hash(&self, h: &str) -> Result<Option<Value>, String> {
        if h == BLOCK_HASH {
            Ok(Some(self.by_height_json()))
        } else {
            Ok(None)
        }
    }
    fn block_by_height(&self, h: u64) -> Result<Option<Value>, String> {
        if h == BLOCK_H {
            Ok(Some(self.by_height_json()))
        } else {
            Ok(None)
        }
    }
    fn raw_block(&self, hd: Option<&str>, hh: Option<u64>) -> Result<Option<String>, String> {
        let hit = (hd == Some(BLOCK_HASH)) || (hh == Some(BLOCK_H));
        Ok(if hit {
            // 100-byte fake block: header 80 + 20 bytes of body
            let mut v = vec![0u8; 80];
            v[0] = 7; // version marker, asserted via header-hex slice
            v.extend_from_slice(&[0xAA; 20]);
            Some(hex_encode(&v))
        } else {
            None
        })
    }
    fn tx(&self, _t: &str, _v: bool) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn hash_at(&self, h: u64) -> Result<Option<String>, String> {
        Ok(if h == BLOCK_H {
            Some(BLOCK_HASH.into())
        } else {
            None
        })
    }
}
impl BlocksJson {
    fn by_height_json(&self) -> Value {
        json!({
            "hash": BLOCK_HASH,
            "confirmations": 11,
            "size": 100,
            "height": BLOCK_H,
            "version": 7,
            "merkleroot": "cc".repeat(32),
            "tx": ["tx1", "tx2"],
            "time": 1_700_000_100,
            "nonce": 42,
            "bits": "2000ffff",
            "difficulty": difficulty_from_bits(0x2000_ffff),
            "previousblockhash": "dd".repeat(32),
            "nextblockhash": Value::Null,
            "entropybit": 1,
            "modifier": "1122334455667788",
            "modifierchecksum": "aabbccdd",
            "proofhash": "ee".repeat(32),
            "flags": "proof-of-stake",
        })
    }
}
const BLOCK_H: u64 = 8;
const BLOCK_HASH: &str = "5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b";

#[test]
fn pow_subsidy_schedule_matches_legacy_getproofowreward() {
    // main.cpp:1518-1536 schedule (COIN = 1e6)
    assert_eq!(pow_subsidy_sats(0), 1_000_000);
    assert_eq!(pow_subsidy_sats(99), 1_000_000);
    assert_eq!(pow_subsidy_sats(100), 20_000_000);
    assert_eq!(pow_subsidy_sats(999), 20_000_000);
    assert_eq!(pow_subsidy_sats(1000), 10_000_000);
    assert_eq!(pow_subsidy_sats(2999), 10_000_000);
    assert_eq!(pow_subsidy_sats(3000), 5_000_000);
    assert_eq!(pow_subsidy_sats(6999), 5_000_000);
    assert_eq!(pow_subsidy_sats(7000), 10_000_000);
    assert_eq!(pow_subsidy_sats(9000), 10_000_000);
    assert_eq!(pow_subsidy_sats(9001), 0);
    assert_eq!(pow_subsidy_sats(2_204_064), 0, "live era mints PoS, not PoW");
}

#[test]
fn help_lists_every_native_and_proxy_method() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = deps(&t, &w, &b, None);
    let v = call(&d, "help", json!([]));
    let text = v["result"].as_str().unwrap();
    for m in NATIVE_METHODS.iter().chain(PROXY_FALLBACK_METHODS.iter()) {
        assert!(text.split('\n').any(|l| l == *m), "help must list {}", m);
    }
    // membership: native vs proxy vs unknown
    let v = call(&d, "help", json!(["getblockcount"]));
    assert!(v["result"].as_str().unwrap().contains("tri7 native"));
    let v = call(&d, "help", json!(["gettransaction"]));
    assert!(v["result"].as_str().unwrap().contains("legacy proxy fallback"));
    let v = call(&d, "help", json!(["nosuchcommand"]));
    assert_eq!(v["error"]["code"], -1);
}

#[test]
fn getblockheader_is_the_block_json_without_body_fields() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = BlocksJson;
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    // by hash and by height both work
    let v = call(&d, "getblockheader", json!([BLOCK_HASH]));
    assert_eq!(v["result"]["height"], json!(BLOCK_H));
    let v = call(&d, "getblockheader", json!([BLOCK_H]));
    assert_eq!(v["result"]["height"], json!(BLOCK_H));
    // header = block minus body
    let r = &v["result"];
    assert!(r.get("tx").is_none(), "no tx array in a header");
    assert!(r.get("size").is_none(), "no size in a header");
    assert_eq!(r["merkleroot"], json!("cc".repeat(32)));
    assert_eq!(r["bits"], json!("2000ffff"));
    assert_eq!(r["modifier"], json!("1122334455667788"));
    assert_eq!(r["previousblockhash"], json!("dd".repeat(32)));
    // legacy index-only fields are null, never invented
    assert!(r["mint"].is_null());
    assert!(r["blocktrust"].is_null());
    assert!(r["chaintrust"].is_null());
    // nextblockhash from the height map: h+1 not present in this stub → null
    assert!(r["nextblockhash"].is_null());
    // not found
    let v = call(&d, "getblockheader", json!(["00".repeat(32)]));
    assert_eq!(v["error"]["code"], -5);
    // param validation
    let v = call(&d, "getblockheader", json!([]));
    assert_eq!(v["error"]["code"], -8);
}

#[test]
fn getblockheader_raw_form_is_the_80_byte_header_hex() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = BlocksJson;
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d, "getblockheader", json!([BLOCK_HASH, false]));
    let hdr = v["result"].as_str().unwrap();
    let bytes = hex_decode(hdr);
    assert_eq!(bytes.len(), 80, "exactly the header");
    assert_eq!(bytes[0], 7, "first header byte = block version marker");
    // the body bytes (0xAA filler) must NOT appear
    assert!(hex_decode(hdr)[80..].is_empty());
    // by height raw form too
    let v = call(&d, "getblockheader", json!([BLOCK_H, false]));
    assert_eq!(v["result"].as_str().unwrap().len(), 160);
}

#[test]
fn getblockbynumber_aliases_getblock_height() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = BlocksJson;
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d, "getblockbynumber", json!([BLOCK_H]));
    assert_eq!(v["result"]["hash"], json!(BLOCK_HASH));
    assert_eq!(v["result"]["tx"].as_array().unwrap().len(), 2, "verbose keeps tx");
    let v = call(&d, "getblockbynumber", json!([BLOCK_H, false]));
    assert_eq!(v["result"].as_str().unwrap().len(), 200, "raw keeps the whole block");
    let v = call(&d, "getblockbynumber", json!([9999]));
    assert_eq!(v["error"]["code"], -5);
}

#[test]
fn getblockchaininfo_and_getnetworkinfo_from_tip_and_snapshot() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = deps(&t, &w, &b, None);
    let v = call(&d, "getblockchaininfo", json!([]));
    assert_eq!(v["result"]["chain"], "main");
    assert_eq!(v["result"]["blocks"], json!(TIP_H));
    assert_eq!(v["result"]["bestblockhash"], json!(TIP_HASH));
    assert_eq!(v["result"]["moneysupply_sats"], json!(222_507_326_452i64));
    assert_eq!(v["result"]["verificationprogress"], json!(1.0));
    let v = call(&d, "getnetworkinfo", json!([]));
    assert_eq!(v["result"]["version"], json!(TRI7_RPC_VERSION));
    assert_eq!(v["result"]["subversion"], json!(TRI7_SUBVERSION));
    assert_eq!(v["result"]["protocolversion"], json!(TRI7_PROTOCOL_VERSION));
    assert_eq!(v["result"]["connections"], json!(1));
}

#[test]
fn getwalletinfo_serves_snapshot_and_errors_without_one() {
    let t = StubTip;
    let b = StubBlocks { ready: true };
    let w = StubWallet::default();
    let d = deps(&t, &w, &b, None);
    let v = call(&d, "getwalletinfo", json!([]));
    assert_eq!(v["result"]["balance_sats"], json!(3_036_990_591i64));
    assert_eq!(v["result"]["walletversion"], json!(60000));
    assert_eq!(v["result"]["coverage_height"], json!(2_201_090));
    assert_eq!(v["result"]["addresses"], json!(248));
    // legacy keypool fields exist but are honestly null
    assert!(v["result"]["keypoolsize"].is_null());
    // missing snapshot → -1, not a fake zero balance
    let mut w2 = StubWallet::default();
    w2.balance = None;
    let d2 = deps(&t, &w2, &b, None);
    let v = call(&d2, "getwalletinfo", json!([]));
    assert_eq!(v["error"]["code"], -1);
}

#[test]
fn getmininginfo_composes_ops_stats_and_nulls_when_engine_down() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    // live engine: staking stats flow through
    let ops = StubOps::new(true);
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
    let v = call(&d, "getmininginfo", json!([]));
    assert_eq!(v["result"]["stakeweight"]["combined"], json!(42));
    assert_eq!(v["result"]["blocks"], json!(TIP_H));
    assert_eq!(v["result"]["blockvalue"], json!(0.0), "PoW subsidy is 0 above h9001");
    assert_eq!(v["result"]["stakeinterest"], json!(0.33));
    // engine down: stats null, structure intact
    let d2 = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d2, "getmininginfo", json!([]));
    assert!(v["result"]["netstakeweight"].is_null());
    assert!(v["result"]["stakeweight"]["combined"].is_null());
}

#[test]
fn getsubsidy_and_estimatefee_are_static_legacy_policy() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = deps(&t, &w, &b, None);
    let v = call(&d, "getsubsidy", json!([]));
    assert_eq!(v["result"], json!(0.0), "tip above h9001: PoW subsidy 0");
    let v = call(&d, "getsubsidy", json!(["2000ffff"])); // nTarget tolerated
    assert_eq!(v["result"], json!(0.0));
    let v = call(&d, "estimatefee", json!([1]));
    assert_eq!(v["result"], json!(0.0001), "MIN_TX_FEE = CENT/100 = 100 sats");
    let v = call(&d, "estimatefee", json!([6]));
    assert_eq!(v["result"], json!(0.0001), "static fee policy ignores nblocks");
}

#[test]
fn verifymessage_signs_and_verifies_in_legacy_param_order() {
    let t = StubTip;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = deps(&t, &w, &b, None);
    // a real signature from the shared sign/verify implementation
    let secret = [7u8; 32];
    let secp = secp256k1::Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    let addr = crate::wallet::pubkey_to_address(&pk.serialize());
    let sig = crate::gui::state::sign_message(&secret, "hello tri").unwrap();
    let v = call(&d, "verifymessage", json!([addr, sig, "hello tri"]));
    assert_eq!(v["result"], json!(true));
    // wrong message → false (not an error)
    let v = call(&d, "verifymessage", json!([addr, sig, "different"]));
    assert_eq!(v["result"], json!(false));
    // invalid address → -5 Invalid address (legacy RPC_INVALID_ADDRESS_OR_KEY)
    let v = call(&d, "verifymessage", json!(["notanaddress", sig, "m"]));
    assert_eq!(v["error"]["code"], -5);
    assert_eq!(v["error"]["message"], "Invalid address");
    // malformed base64 → -5 Malformed base64 encoding
    let v = call(&d, "verifymessage", json!([addr, "!!!not-base64!!!", "m"]));
    assert_eq!(v["error"]["code"], -5);
    assert_eq!(v["error"]["message"], "Malformed base64 encoding");
}
}
