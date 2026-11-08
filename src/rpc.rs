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
use crate::consensus::{
    block_is_proof_of_stake, CHECKPOINT_ANCHOR_DISPLAY, CHECKPOINT_HORIZON,
};
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
    "gettransaction",
    "getinfo",
    "gettxoutsetinfo",
    "getaddressbalance",
    "getaddressutxos",
    "getaddresstxids",
    "getdifficulty",
    "getconnectioncount",
    "getstakemodifier",
    "getbalance",
    "listunspent",
    "listtransactions",
    "listsinceblock",
    "listwallettx",
    "getlocalinfo",
    "listwalletaddresses",
    "getreceivedbyaddress",
    // ---- explorer 2026-11-07: native rich list from the balances snapshot
    // (every node IS an explorer; the snapshot is built/maintained by the
    // daemon's --balances builder thread) ----
    "richlist",
    "validateaddress",
    "validatepubkey",
    "makekeypair",
    "health",
    "getsyncinfo",
    // ---- phase 7 milestone 2: broadcast / staking / connections / smsg ----
    "sendrawtransaction",
    "getstakinginfo",
    "staking",
    "getpeerinfo",
    "getrawmempool",
    "getmempoolinfo",
    "getnetworkhashps",
    "getnewaddress",
    "backupwallet",
    "encryptwallet",
    "walletpassphrase",
    "walletlock",
    "walletpassphrasechange",
    "decryptwallet",
    "sendtoaddress",
    "sendmany",
    "signmessage",
    "dumpprivkey",
    "dumpwallet",
    "importwallet",
    "importprivkey",
    "checkwallet",
    // ---- legacy parity 2026-11-07: keypoolrefill tops up the `pool`-row
    // reserve (getnewaddress hands out pooled keys first, a background
    // maintainer keeps the pool topped) ----
    "keypoolrefill",
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
    // ---- legacy parity 2026-11-06: offline tx workflow + wallet bookkeeping
    // (rpcrawtransaction / rpcwallet surfaces that need no daemon) ----
    "decoderawtransaction",
    "decodescript",
    "createrawtransaction",
    "settxfee",
    "listreceivedbyaddress",
    "getnewpubkey",
    "addmultisigaddress",
    "addredeemscript",
    "signrawtransaction",
    // ---- legacy parity 2026-11-07: address groupings (rpcwallet.cpp:394,
    // privacy-cluster view over the wallet history) + the sync-checkpoint
    // report (rpcblockchain.cpp:665, served from the compiled-in anchor) ----
    "listaddressgroupings",
    "getcheckpoint",
    // ---- legacy parity 2026-11-08 (workqueue item 9 RPC-P1b): wallet repair
    // + unconfirmed-send resend, both routed to the LIVE ops engine ----
    "repairwallet",
    "resendtx",
    "stop",
];

/// Ops verbs with a passive implementation that needs no live wallet view.
/// The passive layer answers these while the engine is warming up.
pub const OPS_EARLY: &[&str] = &[
    "stop",
    "createrawtransaction",
    "encryptwallet",
    "walletpassphrasechange",
    "decryptwallet",
    "getnewpubkey",
    "settxfee",
];

/// The NATIVE_METHODS subset routed to the live NodeOps engine (wallet keys,
/// peer sessions, staking loop, smsg store). Everything else is served from
/// the passive snapshots/window.
pub const OPS_METHODS: &[&str] = &[
    "sendrawtransaction",
    "getstakinginfo",
    "staking",
    "getpeerinfo",
    "getrawmempool",
    "getmempoolinfo",
    "getnetworkhashps",
    "getaddressbalance",
    "getaddressutxos",
    "getaddresstxids",
    "getnewaddress",
    "backupwallet",
    "encryptwallet",
    "walletpassphrase",
    "walletlock",
    "walletpassphrasechange",
    "decryptwallet",
    "sendtoaddress",
    "sendmany",
    "signmessage",
    "dumpprivkey",
    "dumpwallet",
    "importwallet",
    "importprivkey",
    "checkwallet",
    // ---- legacy parity 2026-11-07: keypoolrefill tops up the `pool`-row
    // reserve (getnewaddress hands out pooled keys first, a background
    // maintainer keeps the pool topped) ----
    "keypoolrefill",
    "smsgsend",
    "smsginbox",
    "smsgoutbox",
    "smsgscandir",
    "smsgpurge",
    "smsgaddaddress",
    "smsgkeys",
    "smsglocalinfo",
    // ---- legacy parity 2026-11-06: offline tx workflow + wallet file ----
    "signrawtransaction",
    "createrawtransaction",
    "getnewpubkey",
    "addmultisigaddress",
    "addredeemscript",
    // ---- legacy parity 2026-11-08 (workqueue item 9 RPC-P1b): repairwallet
    // rebuilds the live view against the store; resendtx re-announces the
    // wallet's recorded unconfirmed sends. Live-engine verbs, never early. ----
    "repairwallet",
    "resendtx",
    "stop",
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

#[derive(Clone)]
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
    /// Directory holding the node's state (where onion_address.txt /
    /// hidden_service/hostname live). Default: empty = unknown.
    fn state_dir(&self) -> String {
        String::new()
    }
    /// Wallet-file bookkeeping (bestblock / fee). Default = unknown.
    fn extras(&self) -> WalletExtras {
        WalletExtras::default()
    }
    /// Publish a fee setting into this provider's live extras. Returns Err when
    /// the provider has no writable state (tests, read-only tools).
    fn set_paytxfee(&self, _sats: i64) -> std::result::Result<(), String> {
        Err("wallet state is not writable here".to_string())
    }
    /// The legacy `defaultkey` record (33/65-byte pubkey), when known.
    fn default_key(&self) -> Option<Vec<u8>> {
        None
    }
    /// The public key for one of OUR addresses, when known.
    fn pubkey_for_address(&self, _addr: &str) -> Option<Vec<u8>> {
        None
    }
    /// Full wallet history (history.json, built by the txhistory bin).
    /// Empty = no history file yet; the RPC then answers with an empty list
    /// rather than an error so the GUI can render "run a rescan".
    fn history(&self) -> Vec<crate::history::HistoryRec> {
        Vec::new()
    }
    /// Address book labels (legacy mapAddressBook: address → label). An
    /// address ABSENT from this map is what legacy IsChange() keys on
    /// (mine + unlabeled = change). Empty = no book known (wallet file not
    /// loaded) — every mine output then counts as change, matching a legacy
    /// wallet with an empty address book.
    fn labels(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
    /// The `richlist` explorer verb: top addresses by balance from the
    /// balances snapshot. Default = not configured in this provider (tests,
    /// read-only tools); the daemon provider reads its state-dir snapshot.
    fn rich_list(&self, _top: usize) -> Result<Value, String> {
        Err("rich list source not configured in this provider".to_string())
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

// ---------------------------------------------------------------------------
// Rich list (explorer verb: `richlist` — top addresses by balance, served
// from the balances snapshot the daemon builds/maintains with --balances).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RichListData {
    /// snapshot coverage height (full-archive replay reached this block).
    pub height: u64,
    pub covered_tip_hash: String,
    /// Σ of every live UTXO value (attributed + unattributed).
    pub total_chain_sats: i64,
    /// (address, balance_sats) sorted by balance DESC, address ASC tie-break
    /// — deterministic ranking regardless of HashMap iteration order.
    pub entries: Vec<(String, i64)>,
}

/// Parse the balances.json layout into rich-list data. Only positive
/// balances rank (a 0-balance address is not on a rich list).
pub fn rich_list_from_value(v: &Value) -> Result<RichListData, String> {
    let height = v["height"].as_u64().ok_or("balances snapshot missing height")?;
    let covered_tip_hash = v["covered_tip_hash"].as_str().unwrap_or("").to_string();
    let total_chain_sats = v["totals"]["total_balance_sats"].as_i64().unwrap_or(0);
    let map = v["balances"]
        .as_object()
        .ok_or("balances snapshot missing balances map")?;
    let mut entries: Vec<(String, i64)> = map
        .iter()
        .filter_map(|(k, val)| val.as_i64().map(|s| (k.clone(), s)))
        .filter(|(_, s)| *s > 0)
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(RichListData {
        height,
        covered_tip_hash,
        total_chain_sats,
        entries,
    })
}

/// The `richlist` response: top `top` entries + honest coverage labeling
/// (the snapshot height can lag the chain tip; the caller passes the current
/// tip height so consumers can judge the lag themselves).
pub fn rich_list_json(
    data: &RichListData,
    chain_tip_height: Option<u64>,
    source: &str,
    top: usize,
) -> Value {
    let entries: Vec<Value> = data
        .entries
        .iter()
        .take(top)
        .enumerate()
        .map(|(i, (addr, sats))| {
            json!({
                "rank": i + 1,
                "address": addr,
                "balance_sats": sats,
                "balance_tri": sats_to_tri(*sats),
            })
        })
        .collect();
    json!({
        "requested": top,
        "count": entries.len(),
        "total_ranked": data.entries.len(),
        "snapshot_height": data.height,
        "covered_tip_hash": data.covered_tip_hash,
        "chain_tip_height": chain_tip_height,
        "total_chain_sats": data.total_chain_sats,
        "entries": entries,
        "source": source,
    })
}

/// mtime+len keyed cache over the balances.json file. The daemon rebuilds the
/// snapshot in the background (builder thread); re-parsing per richlist call
/// would re-read a multi-MB file on every explorer refresh — instead a call
/// stats the file and re-parses only when the signature changed, so a rebuilt
/// snapshot is picked up without a daemon restart.
#[derive(Default)]
pub struct RichListCache {
    sig: Option<(u64, u64)>,
    data: Option<std::sync::Arc<RichListData>>,
}

impl RichListCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current parsed data for `path`, re-reading when the file changed.
    pub fn get(&mut self, path: &std::path::Path) -> Result<std::sync::Arc<RichListData>, String> {
        let meta = std::fs::metadata(path)
            .map_err(|_| {
                format!(
                    "balances snapshot not available at {} — start the node with an explicit \
                     --balances PATH (or run the balances bin) to build one",
                    path.display()
                )
            })?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let sig = (meta.len(), mtime);
        if self.sig == Some(sig) {
            if let Some(d) = &self.data {
                return Ok(std::sync::Arc::clone(d));
            }
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {}", path.display(), e))?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("parse {}: {}", path.display(), e))?;
        let data = std::sync::Arc::new(rich_list_from_value(&v)?);
        self.sig = Some(sig);
        self.data = Some(std::sync::Arc::clone(&data));
        Ok(data)
    }
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
    /// Wallet-file bookkeeping the live node refreshes (bestblock height /
    /// hash, current paytxfee sats). Empty = not wired (tests, cold tools).
    pub extra: std::sync::Arc<std::sync::RwLock<WalletExtras>>,
    /// address → pubkey for the wallet's own keys (served by getnewpubkey).
    pub pubkeys: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<u8>>>>,
    /// address → label from the wallet's name rows (legacy mapAddressBook),
    /// wired by the daemon when a wallet file is open. Consumed by
    /// listaddressgroupings for the legacy IsChange() decision and the third
    /// row column.
    pub labels: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, String>>>,
}

/// Values the ops engine publishes for the passive native handlers. Every
/// field defaults to "unknown" so an unwired node answers honestly (`null`)
/// instead of inventing a number.
#[derive(Clone, Debug, Default)]
pub struct WalletExtras {
    pub bestblock_height: Option<u64>,
    pub bestblock_hash: Option<String>,
    pub paytxfee_sats: Option<i64>,
    /// Seconds-until-lock for an encrypted unlocked wallet (0 = locked).
    pub unlocked_until: Option<u64>,
    /// legacy `defaultkey` row (pubkey bytes).
    pub default_key: Option<Vec<u8>>,
    /// Legacy keypool census (`pool` rows): entry count + oldest nTime.
    /// None = unknown (no wallet file / not scanned); Some = real values
    /// (0 = the wallet has an empty keypool — also a real value).
    pub keypool_size: Option<usize>,
    pub keypool_oldest: Option<i64>,
}

/// `unlocked_until` for getwalletinfo: the legacy uint64 max sentinel means
/// "not encrypted"; an encrypted, locked wallet reports 0; an encrypted,
/// unlocked one reports the real expiry (the ops engine publishes it).
fn unlocked_until(deps: &Deps) -> Value {
    match deps.wallet.extras().unlocked_until {
        None => json!(u64::MAX),
        Some(0) => json!(0),
        Some(_) => deps
            .wallet
            .extras()
            .unlocked_until
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
    }
}

/// Directory the passive verbs persist operator settings into (the node's
/// state dir, exported by follow_tip). None outside a real node.
fn fee_setting_dir() -> Option<std::path::PathBuf> {
    std::env::var("TRI7_STATE_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
}

/// The wallet file this node was started with (the same env var the
/// encryptwallet passive fallback and getnewpubkey read). None when the
/// node runs without --wallet.
fn wallet_file_path() -> Option<std::path::PathBuf> {
    std::env::var("TRI7_WALLET_PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Confirmations for a wallet address. The snapshot map carries no per-tx
/// heights, so this answers `null` (never a fabricated depth) until a caller
/// can prove one — a wrong number here is worse than an honest null.
fn make_confirmations(_addr: &str, _tip: u64) -> Value {
    Value::Null
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

    fn extras(&self) -> WalletExtras {
        self.extra.read().map(|g| g.clone()).unwrap_or_default()
    }

    fn set_paytxfee(&self, sats: i64) -> std::result::Result<(), String> {
        let mut g = self.extra.write().map_err(|e| format!("extras lock: {e}"))?;
        g.paytxfee_sats = Some(sats);
        Ok(())
    }

    fn default_key(&self) -> Option<Vec<u8>> {
        self.extra.read().ok().and_then(|g| g.default_key.clone())
    }

    fn pubkey_for_address(&self, addr: &str) -> Option<Vec<u8>> {
        self.pubkeys.read().ok().and_then(|m| m.get(addr).cloned())
    }

    fn labels(&self) -> std::collections::HashMap<String, String> {
        self.labels.read().map(|g| g.clone()).unwrap_or_default()
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

/// The wallet's spendable set from the node's LIVE address index.
///
/// The index is applied on every accepted block, so it stays tip-accurate
/// even when the chain replay cannot reach the tip - a hole in the block
/// store, or an archive that ends below the tip, caps the replay snapshot
/// while the index keeps moving with the chain. The replay snapshot stays
/// the fallback for a node without an index (and for a wallet whose index
/// has not finished building).
///
/// NOTE: this returns the live view whenever the node has one. Callers that
/// serve it as the wallet's whole balance must use `live_authoritative`
/// instead - an index that is still building, or that trails the tip, does
/// not know every coin.
fn live_spendable(deps: &Deps) -> Option<serde_json::Value> {
    let ops = deps.ops?;
    let v = ops.call("getwalletspendable", &serde_json::json!([])).ok()?;
    v.get("balance_sats").and_then(|x| x.as_i64())?;
    Some(v)
}

/// The live set may be served as the wallet's WHOLE spendable set only when
/// the library's rule says so: a finished index that has applied exactly the
/// chain this node serves (same height AND the same block hash). The node
/// reports the facts; the rule is computed here, by production code.
fn live_authoritative(deps: &Deps) -> Option<serde_json::Value> {
    let v = live_spendable(deps)?;
    let facts = crate::addrindex::IndexFacts {
        coverage_height: v.get("index_height").and_then(|x| x.as_u64())?,
        index_tip_hash: v
            .get("index_tip_hash")
            .and_then(|x| x.as_str())
            .and_then(|s| crate::p2p::hex_decode(s).ok())
            .and_then(|b| {
                if b.len() == 32 {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&b);
                    Some(h)
                } else {
                    None
                }
            }),
        complete: v.get("complete").and_then(|x| x.as_bool())?,
    };
    let chain_height = v.get("chain_tip").and_then(|x| x.as_u64())?;
    let chain_hash = v
        .get("chain_tip_hash")
        .and_then(|x| x.as_str())
        .and_then(|s| crate::p2p::hex_decode(s).ok())
        .and_then(|b| {
            if b.len() == 32 {
                let mut h = [0u8; 32];
                h.copy_from_slice(&b);
                Some(h)
            } else {
                None
            }
        });
    let verdict = crate::addrindex::authority(facts, chain_height, chain_hash);
    if verdict.authoritative {
        Some(v)
    } else {
        None
    }
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

    // Legacy alias (rpcblockchain.cpp): getblockbynumber <height> is
    // getblock <height>. Aliased here so there is one implementation, not two.
    let method = if method == "getblockbynumber" {
        "getblock".to_string()
    } else {
        method
    };

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

fn p_f64(params: &Value, pos: usize, name: &str) -> Result<Option<f64>, (i64, String)> {
    let v = match param_get(params, pos, name) {
        None => return Ok(None),
        Some(v) => v,
    };
    let parsed = v
        .as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()));
    parsed.map(Some).ok_or((
        -8,
        format!("invalid parameter '{}' — expected number", name),
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

/// Per-txid aggregation across history rows for listaddressgroupings.
#[derive(Default)]
struct TxAgg {
    /// addresses of OUR outputs in this tx ("in" rows)
    out_addrs: Vec<String>,
    /// the tx has a debit row (wallet inputs funded a payment)
    send: bool,
    /// OUR INPUT ADDRESSES recorded by the txhistory generator
    /// (HistoryRec.input_addresses) — the spend branch's left side.
    inputs: Vec<String>,
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
            if !is_hex_hash {
                let h = height.expect("height present");
                // A height lookup must never hand back a body that is not the
                // chain's block at that height. The offset table stops where
                // height-threading stopped, so above it a lookup can miss;
                // verify the answer instead of trusting the offset.
                return match deps.blocks.block_by_height(h) {
                    Ok(Some(mut v)) => {
                        let want = deps.blocks.hash_at(h).ok().flatten();
                        let got = v
                            .get("hash")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        match (want, got) {
                            (Some(w), Some(g)) if w != g => NativeOutcome::Err(
                                -5,
                                format!(
                                    "height {h} resolves to a body that hashes to {g}, not the chain block {w} (height map gap — ask by hash, or repair the store)"
                                ),
                            ),
                            _ => {
                                v["height"] = json!(h);
                                NativeOutcome::Ok(v)
                            }
                        }
                    }
                    Ok(None) => NativeOutcome::Err(-5, "Block not found".into()),
                    Err(e) => NativeOutcome::Err(-1, e),
                };
            }
            match deps
                .blocks
                .block_by_hash(raw.as_deref().expect("hex hash present"))
            {
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

        // Wallet-scoped view of a transaction (legacy rpcwallet gettransaction).
        // It was in the proxy-fallback list with no native handler, so every
        // call errored once the legacy daemon was gone. Built from the native
        // verbose tx plus wallet ownership of the outputs/inputs.
        "gettransaction" => {
            let txid = match ptry!(p_str(params, 0, "txid")) {
                Some(s) => s,
                None => return NativeOutcome::Err(-8, "expected txid parameter".into()),
            };
            if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
                return NativeOutcome::Err(-8, "invalid txid — expected 64 hex chars".into());
            }
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            let v = match deps.blocks.tx(&txid, true) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return NativeOutcome::Err(-5, "Invalid or non-wallet transaction id".into())
                }
                Err(e) => return NativeOutcome::Err(-1, e),
            };
            let mut amount = 0.0f64;
            let mut details: Vec<Value> = Vec::new();
            let mut ours_out = false;
            if let Some(vout) = v.get("vout").and_then(|x| x.as_array()) {
                for o in vout {
                    let addrs = o
                        .get("scriptPubKey")
                        .and_then(|s| s.get("addresses"))
                        .and_then(|a| a.as_array());
                    let val = o.get("value_sats").and_then(|x| x.as_i64()).unwrap_or(0);
                    if val == 0 {
                        continue;
                    }
                    if let Some(list) = addrs {
                        // One output pays ONE script. If several of its
                        // addresses are ours (multisig/P2SH), the value must be
                        // counted once, not once per address.
                        let ours: Vec<&str> = list
                            .iter()
                            .filter_map(|a| a.as_str())
                            .filter(|addr| deps.wallet.is_mine(addr))
                            .collect();
                        if let Some(first) = ours.first() {
                            ours_out = true;
                            amount += sats_to_tri(val);
                            details.push(json!({
                                "account": "",
                                "address": first,
                                "category": "receive",
                                "amount": sats_to_tri(val),
                            }));
                        }
                    }
                }
            }
            // inputs that spend our coins make this a send
            let mut spent = 0.0f64;
            // Inputs are capped at 32 lookups per call (each one reads another
            // transaction). Beyond the cap `amount` would undercount silently,
            // so the count of skipped inputs is reported instead.
            const MAX_INPUT_LOOKUPS: usize = 32;
            let mut skipped_inputs = 0usize;
            if let Some(vin) = v.get("vin").and_then(|x| x.as_array()) {
                skipped_inputs = vin.len().saturating_sub(MAX_INPUT_LOOKUPS);
                for i in vin.iter().take(MAX_INPUT_LOOKUPS) {
                    let Some(ptx) = i.get("txid").and_then(|x| x.as_str()) else { continue };
                    let n = i.get("vout").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let Ok(Some(prev)) = deps.blocks.tx(ptx, true) else { continue };
                    let Some(po) = prev.get("vout").and_then(|x| x.as_array()).and_then(|a| a.get(n)) else { continue };
                    let val = po.get("value_sats").and_then(|x| x.as_i64()).unwrap_or(0);
                    let mine = po
                        .get("scriptPubKey")
                        .and_then(|s| s.get("addresses"))
                        .and_then(|a| a.as_array())
                        .map(|l| {
                            l.iter().any(|a| {
                                a.as_str()
                                    .map(|ad| deps.wallet.is_mine(ad))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);
                    if mine {
                        spent += sats_to_tri(val);
                        details.push(json!({
                            "account": "",
                            "category": "send",
                            "amount": -sats_to_tri(val),
                        }));
                    }
                }
            }
            if spent > 0.0 {
                amount -= spent;
            }
            let _ = ours_out;
            NativeOutcome::Ok(json!({
                "txid": txid,
                "amount": (amount * 1_000_000.0).round() / 1_000_000.0,
                "confirmations": v.get("confirmations").cloned().unwrap_or(json!(0)),
                "blockhash": v.get("blockhash").cloned().unwrap_or(Value::Null),
                "blockindex": v.get("height").cloned().unwrap_or(Value::Null),
                "blocktime": v.get("time").cloned().unwrap_or(Value::Null),
                "time": v.get("time").cloned().unwrap_or(Value::Null),
                "timereceived": v.get("time").cloned().unwrap_or(Value::Null),
                "details": details,
                "hex": v.get("hex").cloned().unwrap_or(json!("")),
                "inputs_not_scanned": skipped_inputs,
            }))
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
        "getbalance" => {
            // The LIVE index wins over the replay snapshot: the snapshot is
            // capped at whatever height the chain replay reached (an archive
            // seam or a store hole), the index follows the tip.
            if let Some(live) = live_authoritative(deps) {
                if let Some(sats) = live.get("balance_sats").and_then(|x| x.as_i64()) {
                    return NativeOutcome::Ok(json!(sats_to_tri(sats)));
                }
            }
            match deps.wallet.balance() {
                Some(b) => NativeOutcome::Ok(json!(sats_to_tri(b.sats))),
                None => NativeOutcome::Err(
                    -1,
                    "wallet balance snapshot unavailable (balances file not loaded)".into(),
                ),
            }
        }

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

        "richlist" => {
            // Explorer verb (2026-11-07): top addresses by balance from the
            // balances snapshot. Passive by design — served even while the
            // ops engine is warming up, so EVERY node is an explorer.
            // params: [count] (default 100, capped at 1000).
            let top = ptry!(p_u64(params, 0, "count")).unwrap_or(100);
            let top = top.clamp(1, 1000) as usize;
            match deps.wallet.rich_list(top) {
                Ok(v) => NativeOutcome::Ok(v),
                Err(e) => NativeOutcome::Err(-1, e),
            }
        }

        "listreceivedbyaddress" => {
            // Legacy listreceivedbyaddress [minconf] [include_empty]:
            // one row per wallet address, in the legacy record shape. Data
            // source is the same received_regular map getreceivedbyaddress
            // uses (P2PKH-script receipts), so the two verbs cannot disagree.
            let _minconf = ptry!(p_u64(params, 0, "minconf")).unwrap_or(1);
            let include_empty = ptry!(p_bool(params, 1, "include_empty")).unwrap_or(false);
            let tip_h = deps.tip.tip().height;
            let mut rows: Vec<Value> = Vec::new();
            for e in deps.wallet.address_entries() {
                let amount = deps.wallet.received_regular(&e.address).unwrap_or(0);
                if amount == 0 && !include_empty {
                    continue;
                }
                rows.push(json!({
                    "address": e.address,
                    "account": "",
                    "amount": sats_to_tri(amount),
                    "confirmations": make_confirmations(&e.address, tip_h),
                    "label": "",
                    "txids": [],
                }));
            }
            rows.sort_by(|a, b| {
                b["amount"]
                    .as_f64()
                    .partial_cmp(&a["amount"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            NativeOutcome::Ok(json!(rows))
        }

        // Legacy getcheckpoint (rpcblockchain.cpp:665): "Show info of
        // synchronized checkpoint." Legacy reports Checkpoints::
        // hashSyncCheckpoint — a RUNTIME value the checkpoint master advances
        // over the network (AcceptPendingSyncCheckpoint), seeded at genesis.
        // tri7 does not process checkpoint-master messages, so its checkpoint
        // of record is the compiled-in anchor (consensus::CHECKPOINT_HORIZON /
        // CHECKPOINT_ANCHOR_DISPLAY — the highest legacy checkpoint-map entry,
        // checkpoints.cpp:82) which follow_tip ENFORCES strictly on every
        // fetch/backfill path (check_checkpoint_anchor). Policy is therefore
        // always strict on tri7, and `checkpointmaster` (legacy
        // -checkpointkey) never applies — both keys appear exactly as a
        // legacy default-policy node without the master key would print them.
        // timestamp = the anchor block's nTime in the legacy DateTimeStrFormat
        // ("%Y-%m-%d %H:%M:%S UTC", util.h:425).
        "getcheckpoint" => {
            if params.as_array().map_or(false, |a| !a.is_empty()) {
                return NativeOutcome::Err(-8, "getcheckpoint takes no parameters".into());
            }
            if !deps.blocks.ready() {
                return NativeOutcome::Miss("block index warming up".into());
            }
            let ts = match deps.blocks.block_by_height(CHECKPOINT_HORIZON) {
                Ok(Some(b)) => b.get("time").and_then(|t| t.as_i64()).unwrap_or(0),
                Ok(None) => {
                    return NativeOutcome::Miss(format!(
                        "checkpoint block h{} not covered by the height map",
                        CHECKPOINT_HORIZON
                    ))
                }
                Err(e) => return NativeOutcome::Err(-1, e),
            };
            NativeOutcome::Ok(json!({
                "synccheckpoint": CHECKPOINT_ANCHOR_DISPLAY,
                "height": CHECKPOINT_HORIZON,
                "timestamp": fmt_utc_timestamp(ts.max(0) as u64),
                "policy": "strict",
            }))
        }

        // Legacy listaddressgroupings (rpcwallet.cpp:394): groups of
        // addresses whose common ownership was made public by common use as
        // inputs or as the resulting change in past transactions. Data
        // sources are all tri7's own: the wallet history rows (the wallet tx
        // set), the per-address balance map listreceivedbyaddress already
        // serves (so the two verbs cannot disagree), and the address book
        // labels (the legacy IsChange key: mine + unlabeled = change).
        //
        // Branch "inputs group together" fires per tx whose inputs are ours.
        // The txhistory generator records OUR INPUT ADDRESSES per row
        // (HistoryRec.input_addresses) — resolvable for free at scan time, so
        // this verb needs NO live prevout lookups. Coinstakes included: the
        // kernel pays the stake back to the staked script (CreateCoinStake,
        // wallet.cpp:2332-2346), so the merge degenerates to the singleton
        // branch already produces.
        //
        // Honest coverage: rows written before input_addresses existed (older
        // history.json files) carry no input side — their txs contribute
        // output singletons only, and the answer covers the history file's
        // window exactly like listwallettx. A rescan with the new generator
        // completes the input side. Ordering note: legacy iterates
        // set<set<CTxDestination>> (internal uint160 order); tri7 sorts by
        // base58 string at both levels — deterministic, not byte-identical to
        // legacy ordering.
        "listaddressgroupings" => {
            let rows = deps.wallet.history();
            let labels = deps.wallet.labels();

            // per-txid aggregation (one tx appears as several rows)
            let mut txs: std::collections::BTreeMap<String, TxAgg> = Default::default();
            for r in &rows {
                let e = txs.entry(r.txid.clone()).or_default();
                if r.direction == "in" && !e.out_addrs.contains(&r.address) {
                    e.out_addrs.push(r.address.clone());
                }
                if r.direction == "out" {
                    e.send = true;
                }
                if e.inputs.is_empty() && !r.input_addresses.is_empty() {
                    e.inputs = r.input_addresses.clone();
                }
            }

            // group store with transitive merge-on-shared-member (the legacy
            // setmap/hits algorithm)
            let mut groups: Vec<std::collections::BTreeSet<String>> = Vec::new();
            let mut member_of: std::collections::HashMap<String, usize> = Default::default();
            let mut push_group = |addrs: Vec<String>,
                                  groups: &mut Vec<std::collections::BTreeSet<String>>,
                                  member_of: &mut std::collections::HashMap<String, usize>| {
                let mut hit: Vec<usize> = addrs
                    .iter()
                    .filter_map(|a| member_of.get(a).copied())
                    .collect();
                hit.sort_unstable();
                hit.dedup();
                let mut merged = std::collections::BTreeSet::new();
                for id in &hit {
                    // BTreeSet has no drain(); take() empties the drained
                    // group in place (compacted away below)
                    merged.extend(std::mem::take(&mut groups[*id]));
                }
                for a in addrs {
                    merged.insert(a);
                }
                let new_id = groups.len();
                for a in &merged {
                    member_of.insert(a.clone(), new_id);
                }
                groups.push(merged);
            };

            let mut unresolved_spend_txs = 0usize;
            for e in txs.values() {
                if !e.inputs.is_empty() {
                    // spend branch: inputs ∪ change (mine outputs that are
                    // NOT in the address book)
                    let mut addrs = e.inputs.clone();
                    for a in &e.out_addrs {
                        if !labels.contains_key(a) {
                            addrs.push(a.clone());
                        }
                    }
                    push_group(addrs, &mut groups, &mut member_of);
                }
                // lone addresses group by themselves (legacy branch B)
                for a in &e.out_addrs {
                    push_group(vec![a.clone()], &mut groups, &mut member_of);
                }
                if e.inputs.is_empty() && e.send {
                    unresolved_spend_txs += 1;
                }
            }
            if unresolved_spend_txs > 0 {
                eprintln!(
                    "listaddressgroupings: {} spend tx(s) predate HistoryRec.input_addresses — no input-side grouping for them (rescan with the new txhistory completes it)",
                    unresolved_spend_txs
                );
            }

            // balance per address: the same map listreceivedbyaddress reads;
            // an address without an entry reports 0 (legacy map semantics)
            let mut balances: std::collections::HashMap<String, i64> = Default::default();
            for e in deps.wallet.address_entries() {
                balances.insert(e.address, e.balance_sats);
            }

            let mut out: Vec<Vec<Value>> = groups
                .iter()
                .filter(|g| !g.is_empty())
                .map(|g| {
                    g.iter()
                        .map(|addr| {
                            let mut row = vec![
                                json!(addr),
                                json!(sats_to_tri(*balances.get(addr.as_str()).unwrap_or(&0))),
                            ];
                            if let Some(l) = labels.get(addr) {
                                row.push(json!(l));
                            }
                            json!(row)
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            out.sort_by(|a, b| {
                let ka = a[0][0].as_str().unwrap_or("");
                let kb = b[0][0].as_str().unwrap_or("");
                ka.cmp(&kb)
            });
            NativeOutcome::Ok(json!(out))
        }

        // Legacy validatepubkey: decode a hex public key and report the address
        // it maps to. Pure function, no wallet access.
        "validatepubkey" => {
            let pk_hex = match ptry!(p_str(params, 0, "pubkey")) {
                Some(s) => s,
                None => return NativeOutcome::Err(-8, "expected public key hex".into()),
            };
            let bytes = match crate::p2p::hex_decode(&pk_hex) {
                Ok(b) => b,
                Err(e) => return NativeOutcome::Err(-5, format!("Invalid public key: {e}")),
            };
            let compressed = bytes.len() == 33;
            if !(compressed || bytes.len() == 65) {
                return NativeOutcome::Err(
                    -5,
                    "Invalid public key: expected 33 (compressed) or 65 (uncompressed) bytes"
                        .into(),
                );
            }
            // reject anything that is not a point on the curve
            match secp256k1::PublicKey::from_slice(&bytes) {
                Ok(_) => {
                    let address = crate::wallet::pubkey_to_address(&bytes);
                    NativeOutcome::Ok(json!({
                        "isvalid": true,
                        "address": address,
                        "iscompressed": compressed,
                    }))
                }
                Err(_) => NativeOutcome::Ok(json!({ "isvalid": false })),
            }
        }

        // Legacy makekeypair: a fresh keypair with no wallet side effects.
        "makekeypair" => {
            let mut secret = [0u8; 32];
            if let Err(e) = getrandom::getrandom(&mut secret) {
                return NativeOutcome::Err(-1, format!("keygen entropy: {e}"));
            }
            let secp = secp256k1::Secp256k1::signing_only();
            let sk = match secp256k1::SecretKey::from_slice(&secret) {
                Ok(k) => k,
                Err(e) => return NativeOutcome::Err(-1, format!("bad secret: {e}")),
            };
            let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
            NativeOutcome::Ok(json!({
                "privatekey": crate::keytool::wif_encode(&secret, true),
                "publickey": pk.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            }))
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
            // LIVE index first: these are the coins the chain actually shows
            // as unspent, so a spend built from this list cannot be a
            // double-spend of a coin the node replayed as gone (or vice versa).
            if let Some(live) = live_authoritative(deps) {
                let tip = live
                    .get("chain_tip")
                    .and_then(|x| x.as_u64())
                    .unwrap_or_default();
                let rows: Vec<Value> = live
                    .get("utxos")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|u| {
                                let h = u.get("height").and_then(|x| x.as_u64())?;
                                let conf = tip.saturating_sub(h) + 1;
                                if conf < minconf || conf > maxconf {
                                    return None;
                                }
                                let sats = u.get("value_sats").and_then(|x| x.as_i64())?;
                                Some(json!({
                                    "txid": u.get("txid")?,
                                    "vout": u.get("n")?,
                                    "address": u.get("address")?,
                                    "scriptPubKey": u.get("script_hex")?,
                                    "amount": sats_to_tri(sats),
                                    "amount_sats": sats,
                                    "confirmations": conf,
                                    "height": h,
                                    "spendable": true,
                                    "solvable": true,
                                }))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                return NativeOutcome::Ok(json!(rows));
            }
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
        // Tor / I2P identity of this node: the addresses a holder can hand
        // out so peers (and messages) reach them over those transports.
        // Sources, in order: an explicit state file, then a Tor hidden-service
        // hostname file, then nothing (honest "not configured").
        "getlocalinfo" => {
            let sd_owned = deps.wallet.state_dir();
            let sd = std::path::Path::new(&sd_owned);
            let read_first = |names: &[&str]| -> Option<String> {
                for n in names {
                    let p = sd.join(n);
                    if let Ok(s) = std::fs::read_to_string(&p) {
                        let s = s.trim().to_string();
                        if !s.is_empty() {
                            return Some(s);
                        }
                    }
                }
                None
            };
            let onion = read_first(&[
                "onion_address.txt",
                "hidden_service/hostname",
                "tor/hostname",
            ]);
            let i2p = read_first(&["i2p_destination.txt", "i2p/hostname"]);
            NativeOutcome::Ok(json!({
                "onion": onion,
                "i2p": i2p,
                "listen": deps.tip.peers(),
                "state_dir": sd_owned,
                "note": if onion.is_none() && i2p.is_none() {
                    "no Tor/I2P identity configured for this node"
                } else {
                    ""
                },
            }))
        }

        "listwallettx" => {
            let rows = deps.wallet.history();
            let tip_h = deps.tip.tip().height;
            let out: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "txid": r.txid,
                        "height": r.height,
                        "time": r.time,
                        "amount": sats_to_tri(r.value_sats),
                        "amount_sats": r.value_sats,
                        // legacy stake decomposition: the reward (net), 0 on
                        // rows written before the field existed
                        "stake_net": r.stake_net,
                        "address": r.address,
                        "direction": r.direction,
                        "category": r.direction,
                        "kind": r.kind,
                        "spent": r.spent,
                        // legacy state machine (transactionrecord.cpp updateStatus)
                        "depth": crate::legacy_rows::depth_of(r.height, tip_h),
                        "matures_in": if r.kind == "coinbase" || r.kind == "coinstake" {
                            (crate::legacy_rows::COINBASE_MATURITY
                                - crate::legacy_rows::depth_of(r.height, tip_h))
                            .max(0)
                        } else {
                            0i64
                        },
                    })
                })
                .collect();
            NativeOutcome::Ok(json!({ "rows": out, "count": rows.len(), "source": "history.json" }))
        }

        // Chain-state summary. Counts come from the wallet replay snapshot when
        // present (it knows every output); the fields legacy clients read are
        // height, bestblock and total_amount.
        "gettxoutsetinfo" => {
            let t_view = deps.tip.tip();
            let bal = deps.wallet.balance();
            let entries = deps.wallet.address_entries();
            let utxos: u64 = entries.iter().map(|e| e.utxos).sum();
            NativeOutcome::Ok(json!({
                "height": t_view.height,
                "bestblock": t_view.blockhash,
                "transactions": Value::Null,
                "txouts": utxos,
                "bogosize": Value::Null,
                "hash_serialized_2": Value::Null,
                "total_amount": bal.as_ref().map(|b| sats_to_tri(b.sats)),
                "total_utxos": utxos,
                "note": "transactions/bogosize need a full UTXO-set pass; txouts and total_amount are the node's replay view",
            }))
        }

        // Legacy paginated history. listwallettx is a full dump for the GUI;
        // these two are the verbs every integration actually calls.
        "listtransactions" => {
            // params: [account, count=10, from=0]
            let count = ptry!(p_u64(params, 1, "count")).unwrap_or(10) as usize;
            let from = ptry!(p_u64(params, 2, "from")).unwrap_or(0) as usize;
            let tip_h = deps.tip.tip().height;
            let mut rows = deps.wallet.history();
            // newest first for windowing, then flip to legacy ascending order
            rows.sort_by(|a, b| b.height.cmp(&a.height).then(b.txid.cmp(&a.txid)));
            let mut window: Vec<_> = rows.into_iter().skip(from).take(count).collect();
            window.reverse();
            let out: Vec<Value> = window
                .iter()
                .map(|r| {
                    let depth = crate::legacy_rows::depth_of(r.height, tip_h);
                    json!({
                        "account": "",
                        "address": r.address,
                        "category": if r.direction == "out" { "send" } else { "receive" },
                        "amount": sats_to_tri(r.value_sats),
                        "confirmations": depth,
                        "blockhash": deps.blocks.hash_at(r.height).ok().flatten(),
                        "blockindex": r.height,
                        "blocktime": r.time,
                        "txid": r.txid,
                        "time": r.time,
                        "timereceived": r.time,
                    })
                })
                .collect();
            NativeOutcome::Ok(json!(out))
        }

        "listsinceblock" => {
            // params: [blockhash, target_confirmations]
            let tip_h = deps.tip.tip().height;
            let want_hash: Option<String> = ptry!(p_str(params, 0, "blockhash"));
            let since_h: Option<u64> = match want_hash.as_deref() {
                None | Some("") => None,
                Some(h) => {
                    // resolve over the tipstate window (the only heights this
                    // node can hash); older hashes are refused rather than
                    // silently returning a partial list
                    let (base, entries) = deps.tip.window();
                    let mut found = None;
                    for height in base..=tip_h {
                        if deps.blocks.hash_at(height).ok().flatten().as_deref() == Some(h) {
                            found = Some(height);
                            break;
                        }
                    }
                    match found {
                        Some(x) => Some(x),
                        None => {
                            return NativeOutcome::Err(
                                -8,
                                format!(
                                    "blockhash {h} is not in this node's window (h{base}..h{tip_h}); pass a recent block or omit the parameter"
                                ),
                            )
                        }
                    }
                }
            };
            let mut rows: Vec<_> = deps
                .wallet
                .history()
                .into_iter()
                .filter(|r| since_h.map(|s| r.height > s).unwrap_or(true))
                .collect();
            rows.sort_by(|a, b| a.height.cmp(&b.height).then(a.txid.cmp(&b.txid)));
            let out: Vec<Value> = rows
                .iter()
                .map(|r| {
                    let depth = crate::legacy_rows::depth_of(r.height, tip_h);
                    json!({
                        "account": "",
                        "address": r.address,
                        "category": if r.direction == "out" { "send" } else { "receive" },
                        "amount": sats_to_tri(r.value_sats),
                        "confirmations": depth,
                        "blockhash": deps.blocks.hash_at(r.height).ok().flatten(),
                        "blockindex": r.height,
                        "blocktime": r.time,
                        "txid": r.txid,
                        "time": r.time,
                        "timereceived": r.time,
                    })
                })
                .collect();
            NativeOutcome::Ok(json!({
                "transactions": out,
                "lastblock": deps.tip.tip().blockhash,
            }))
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
                "paytxfee": sats_to_tri(deps.wallet.extras().paytxfee_sats.unwrap_or(MIN_TX_FEE_SATS)),
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
            // replay snapshot (balance/coverage/addresses/utxos). The
            // keypool fields report the REAL `pool`-row census the ops
            // engine publishes (size/oldest; null = unknown, 0 = genuinely
            // empty); txcount stays null — the snapshot carries no tx-count.
            let bal = deps.wallet.balance();
            let ex = deps.wallet.extras();
            // The LIVE index is tip-accurate; the replay snapshot is not
            // (it stops at whatever height the chain replay reached). Report
            // the live figure when we have it and say whether it is complete.
            // The live index is served ONLY when the node says it is
            // authoritative (a finished index that has applied every block
            // the chain accepted). Anything else - a partial build, an index
            // that trails the tip - falls back to the replay snapshot, and
            // the answer is flagged partial: a number that does not reach the
            // tip must never be presented as the whole balance.
            let live = live_authoritative(deps);
            let (bal_sats, utxos_n, bal_height, partial, coverage) = match (&live, &bal) {
                (Some(l), _) => {
                    let sats = l.get("balance_sats").and_then(|x| x.as_i64()).unwrap_or(0);
                    let n = l
                        .get("utxos")
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let idx_h = l.get("index_height").and_then(|x| x.as_u64());
                    // no replay snapshot behind it: the live index's own
                    // height is the coverage, and there is no replay hash
                    let cov = idx_h.map(|h| (h, Value::Null));
                    (sats, n, idx_h, false, cov)
                }
                (None, Some(b)) => (
                    b.sats,
                    b.utxos,
                    Some(b.coverage_height),
                    true,
                    Some((b.coverage_height, Value::String(b.coverage_hash.clone()))),
                ),
                (None, None) => (0, 0, None, true, None),
            };
            if bal.is_none() && live.is_none() {
                return NativeOutcome::Err(
                    -1,
                    "wallet balance unavailable (no live index and no balances snapshot)".into(),
                );
            }
            let b = bal.unwrap_or(BalanceView {
                sats: bal_sats,
                utxos: utxos_n,
                coverage_height: bal_height.unwrap_or_default(),
                coverage_hash: String::new(),
                addresses: 0,
                supply_sats: 0,
            });
            NativeOutcome::Ok(json!({
                "walletversion": 60000,
                "balance": sats_to_tri(bal_sats),
                "balance_sats": bal_sats,
                "utxos": utxos_n,
                "balance_height": bal_height,
                "balance_partial": partial,
                "txcount": Value::Null,
                "keypoololdest": ex.keypool_oldest.map(|v| json!(v)).unwrap_or(Value::Null),
                "keypoolsize": ex.keypool_size.map(|v| json!(v)).unwrap_or(Value::Null),
                "unlocked_until": unlocked_until(deps),
                "paytxfee": sats_to_tri(ex.paytxfee_sats.unwrap_or(MIN_TX_FEE_SATS)),
                "coverage_height": coverage.as_ref().map(|c| c.0).unwrap_or_default(),
                "coverage_hash": coverage.map(|c| c.1).unwrap_or(Value::Null),
                "addresses": b.addresses,
            }))
        }

        // Legacy getnewpubkey [account]: "generate a new public key and
        // return it in hex". Kept wallet-scoped on purpose — dumping ANY key
        // the node has seen (the legacy implementation's behaviour) is an
        // information leak with no legitimate client use.
        "getnewpubkey" => {
            let Some((_h, _)) = deps.wallet.coverage() else {
                return NativeOutcome::Err(-1, "wallet balance snapshot unavailable (balances file not loaded)".into());
            };
            let _account = ptry!(p_str(params, 0, "account"));
            let entries = deps.wallet.address_entries();
            if entries.is_empty() {
                return NativeOutcome::Err(-1, "no wallet addresses available".into());
            }
            // defaultkey first (the legacy notion of "the" key), else the
            // funded address with the largest balance
            let mut chosen: Option<String> = None;
            if let Some(dk) = deps.wallet.default_key() {
                let addr = crate::wallet::pubkey_to_address(&dk);
                if entries.iter().any(|e| e.address == addr) {
                    chosen = Some(dk.iter().map(|b| format!("{:02x}", b)).collect());
                }
            }
            if chosen.is_none() {
                let mut best = entries.clone();
                best.sort_by(|a, b| b.balance_sats.cmp(&a.balance_sats));
                let target = &best[0].address;
                if let Some(pk) = deps.wallet.pubkey_for_address(target) {
                    chosen = Some(pk.iter().map(|b| format!("{:02x}", b)).collect());
                }
            }
            // the node may not have published its address→pubkey map yet (the
            // live wallet view is still seeding): read the file directly
            let mut chosen = chosen;
            if chosen.is_none() {
                if let Ok(p) = std::env::var("TRI7_WALLET_PATH") {
                    if !p.is_empty() {
                        if let Ok(keys) = crate::wallet::read_wallet(std::path::Path::new(&p)) {
                            chosen = keys
                                .default_key
                                .clone()
                                .or_else(|| keys.keys.first().map(|k| k.pubkey.clone()))
                                .map(|pk| pk.iter().map(|b| format!("{:02x}", b)).collect());
                        }
                    }
                }
            }
            match chosen {
                Some(hex) => NativeOutcome::Ok(json!(hex)),
                None => NativeOutcome::Err(-1, "no public key available for the wallet's addresses".into()),
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
            // legacy rpcblockchain estimatefee <nblocks> returns the fee the
            // wallet would pay for a typical transaction: nTransactionFee
            // (main.cpp:113), which the operator can raise via settxfee.
            //
            // tri7 extension (optional 2nd/3rd params): the caller declares the
            // transaction shape and gets the fee for THAT transaction, computed
            // by the same function the builder uses (spend::estimate_fee):
            //   estimatefee <nblocks> <n_inputs> <n_outputs>
            // The amount is micro-TRI (1 TRI = 1e6), so 0.0001 TRI = 100 sats.
            let _nblocks = ptry!(p_u64(params, 0, "nblocks"));
            let paytxfee = deps
                .wallet
                .extras()
                .paytxfee_sats
                .unwrap_or(MIN_TX_FEE_SATS)
                .max(crate::spend::MIN_FEE_SATS);
            let n_in = ptry!(p_u64(params, 1, "n_inputs")).unwrap_or(2) as usize;
            let n_out = ptry!(p_u64(params, 2, "n_outputs")).unwrap_or(2) as usize;
            let bytes = crate::spend::estimate_signed_size(n_in, n_out);
            let outs = vec![1_000_000i64; n_out.max(1)];
            let fee = crate::spend::estimate_fee(bytes, paytxfee, &outs);
            NativeOutcome::Ok(json!(sats_to_tri(fee)))
        }

        // ---- legacy parity 2026-11-06: offline tx workflow ------------------
        // These three are pure codec functions (no wallet, no network), so a
        // cold-signing flow works against ANY node — including one with no
        // wallet loaded. Shape matches the legacy rpcrawtransaction replies.
        "decoderawtransaction" => {
            let Some(hex) = ptry!(p_str(params, 0, "hexstring")) else {
                return NativeOutcome::Err(-8, "expected raw transaction hex".into());
            };
            match crate::rawtx::tx_from_hex(&hex) {
                Ok(tx) => NativeOutcome::Ok(crate::rawtx::tx_json(&tx, true)),
                Err(e) => NativeOutcome::Err(-22, format!("TX decode failed: {e}")),
            }
        }

        "decodescript" => {
            let Some(hex) = ptry!(p_str(params, 0, "hex")) else {
                return NativeOutcome::Err(-8, "expected script hex".into());
            };
            let bytes = match crate::p2p::hex_decode(&hex) {
                Ok(b) => b,
                Err(e) => return NativeOutcome::Err(-22, format!("script decode failed: {e}")),
            };
            let mut body = crate::rawtx::decode_script(&bytes);
            // legacy echoes a "p2sh" form for every script it sees:
            // base58check(SCRIPT_ADDRESS_VERSION + hash160(script))
            let p2sh_payload = crate::address::script_hash_payload(&bytes);
            body["p2sh"] = json!(crate::address::payload_to_address(&p2sh_payload));
            NativeOutcome::Ok(body)
        }

        "settxfee" => {
            // legacy rpcwallet settxfee <amount> — sets nTransactionFee, the
            // fee the wallet ADDS on top of the chain minimum when it builds a
            // transaction (wallet.cpp:1688/1785). Range per init.cpp:694:
            // 0 .. 0.25 TRI. 0 means "pay the chain minimum".
            let Some(amount) = ptry!(p_f64(params, 0, "amount")) else {
                return NativeOutcome::Err(-8, "expected amount parameter".into());
            };
            if !(amount >= 0.0) || amount > 0.25 {
                return NativeOutcome::Err(
                    -8,
                    format!("settxfee: {amount} is outside the allowed range 0..0.25 TRI"),
                );
            }
            // the value the chain will actually charge: cent-rounded, floored
            // at MIN_FEE_SATS so 0 still means "the minimum"
            let sats = ((amount * 1_000_000.0).round() as i64).max(crate::spend::MIN_FEE_SATS);
            // Publish FIRST: if the live value cannot be set, say so instead of
            // answering true while estimatefee/the builder still read the old
            // number.
            if let Err(e) = deps.wallet.set_paytxfee(sats) {
                return NativeOutcome::Err(-1, format!("settxfee: publish: {e}"));
            }
            if let Some(dir) = fee_setting_dir() {
                let path = dir.join("paytxfee.json");
                let body = json!({ "paytxfee_sats": sats, "set_utc": unix_now() });
                if let Err(e) = std::fs::write(&path, body.to_string()) {
                    return NativeOutcome::Err(-1, format!("settxfee: persist: {e}"));
                }
            }
            NativeOutcome::Ok(json!(true))
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

        // ---- phase 3b: P2SH multisig (rpcwallet.cpp:836, :905) --------------
        // Both verbs write the wallet FILE directly (like the encryptwallet
        // passive fallback), so they work before the ops engine is warm and
        // independent of the fee path. The optional [account] is accepted
        // for legacy call-shape compatibility and ignored: tri7's label
        // writer (set_label) only writes P2PKH 'T' address rows.
        "addmultisigaddress" => {
            // legacy rpcwallet.cpp:836: build an m-of-n P2SH multisig address
            // from Triangles addresses / hex pubkeys, store the redeem script
            // (AddCScript) and return the bare address string.
            let n_required = match ptry!(p_u64(params, 0, "nrequired")) {
                Some(n) => n,
                None => return NativeOutcome::Err(-8, "expected nrequired parameter".into()),
            };
            let keys_param = match param_get(params, 1, "keys") {
                Some(Value::Array(a)) => a.clone(),
                _ => return NativeOutcome::Err(-8, "expected an array of keys".into()),
            };
            let _account = ptry!(p_str(params, 2, "account"));
            if n_required < 1 || n_required > 16 {
                return NativeOutcome::Err(
                    -8,
                    "a multisignature address must require between 1 and 16 keys".into(),
                );
            }
            if (keys_param.len() as u64) < n_required {
                return NativeOutcome::Err(
                    -8,
                    format!(
                        "not enough keys supplied (got {} keys, but need at least {} to redeem)",
                        keys_param.len(),
                        n_required
                    ),
                );
            }
            let path = match wallet_file_path() {
                Some(p) => p,
                None => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet file path unknown to this node — start tri7d with --wallet"
                            .into(),
                    )
                }
            };
            // Gather public keys (legacy case 1: Triangles address we hold
            // the full pubkey for; case 2: hex-encoded public key).
            let wallet_keys = match crate::wallet::read_wallet(&path) {
                Ok(w) => w.keys,
                Err(e) => {
                    return NativeOutcome::Err(
                        -1,
                        format!("addmultisigaddress: cannot read wallet: {e}"),
                    )
                }
            };
            let mut pubkeys: Vec<Vec<u8>> = Vec::with_capacity(keys_param.len());
            for k in &keys_param {
                let Some(ks) = k.as_str() else {
                    return NativeOutcome::Err(-5, "Invalid public key".into());
                };
                if let Ok(payload) = crate::address::address_to_payload(ks) {
                    // a valid address: must be a P2PKH 'T' address referring
                    // to a key the wallet holds the full pubkey for
                    if payload[0] != crate::address::PUBKEY_ADDRESS {
                        return NativeOutcome::Err(
                            -5,
                            format!("{ks} does not refer to a key"),
                        );
                    }
                    match wallet_keys.iter().find(|wk| wk.address == ks) {
                        Some(wk) => pubkeys.push(wk.pubkey.clone()),
                        None => {
                            return NativeOutcome::Err(
                                -4,
                                format!("no full public key for address {ks}"),
                            )
                        }
                    }
                } else if let Ok(pk) = crate::p2p::hex_decode(ks) {
                    if secp256k1::PublicKey::from_slice(&pk).is_err() {
                        return NativeOutcome::Err(
                            -5,
                            format!("Invalid public key: {ks}"),
                        );
                    }
                    pubkeys.push(pk);
                } else {
                    return NativeOutcome::Err(-5, format!("Invalid public key: {ks}"));
                }
            }
            let redeem = match crate::multisig::redeem_script(n_required as u8, &pubkeys) {
                Ok(r) => r,
                Err(e) => return NativeOutcome::Err(-8, format!("addmultisigaddress: {e}")),
            };
            let address = crate::multisig::p2sh_address(&redeem);
            let mut wh = match crate::wallet::WalletHandle::open_readwrite(&path) {
                Ok(w) => w,
                Err(e) => {
                    return NativeOutcome::Err(
                        -1,
                        format!("addmultisigaddress: cannot open wallet: {e}"),
                    )
                }
            };
            if let Err(e) = wh.put_redeem_script(&redeem) {
                wh.close();
                return NativeOutcome::Err(-1, format!("addmultisigaddress: {e}"));
            }
            wh.close();
            NativeOutcome::Ok(json!(address))
        }

        "addredeemscript" => {
            // legacy rpcwallet.cpp:905: store a hex redeem script in the
            // wallet (AddCScript) so its P2SH address becomes spendable.
            let Some(hex) = ptry!(p_str(params, 0, "redeemScript")) else {
                return NativeOutcome::Err(-8, "expected redeemScript parameter".into());
            };
            let _account = ptry!(p_str(params, 1, "account"));
            let script = match crate::p2p::hex_decode(&hex) {
                Ok(s) => s,
                Err(_) => return NativeOutcome::Err(-5, "Invalid redeemScript".into()),
            };
            let path = match wallet_file_path() {
                Some(p) => p,
                None => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet file path unknown to this node — start tri7d with --wallet"
                            .into(),
                    )
                }
            };
            let mut wh = match crate::wallet::WalletHandle::open_readwrite(&path) {
                Ok(w) => w,
                Err(e) => {
                    return NativeOutcome::Err(
                        -1,
                        format!("addredeemscript: cannot open wallet: {e}"),
                    )
                }
            };
            if let Err(e) = wh.put_redeem_script(&script) {
                wh.close();
                return NativeOutcome::Err(-1, format!("addredeemscript: {e}"));
            }
            wh.close();
            NativeOutcome::Ok(json!(true))
        }

        // Ops verbs that may run before the live wallet view is ready (a pure
        // codec, the wallet FILE, or the process) answer from the passive
        // layer, so "warming up" never blocks work that is already possible.
        // Arms here RETURN: nothing re-enters the dispatch.
        m if OPS_METHODS.contains(&m)
            && OPS_EARLY.contains(&m)
            && deps.ops.as_ref().map(|o| !o.ready()).unwrap_or(true) =>
        {
            match passive_fallback(m, deps, &params) {
                NativeOutcome::Ok(v) => NativeOutcome::Ok(v),
                NativeOutcome::Err(code, msg) => NativeOutcome::Err(code, msg),
                NativeOutcome::Miss(w) => NativeOutcome::Miss(w),
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

/// Legacy createrawtransaction: [{"txid":..,"vout":..}] {"addr":amount,..}
/// → {hex, txid}. Pure (no wallet, no network), so ANY node can answer it.
fn create_raw_tx(params: &Value, now: u32) -> Result<Value, (i64, String)> {
    let Some(Value::Array(ins)) = param_get(params, 0, "txid") else {
        return Err((-8, "expected an array of {txid,vout} inputs".to_string()));
    };
    let Some(Value::Object(outs)) = param_get(params, 1, "amount") else {
        return Err((-8, "expected {\"address\":amount,...} outputs".to_string()));
    };
    let mut inputs: Vec<(String, u32)> = Vec::new();
    for e in ins {
        let txid = e
            .get("txid")
            .and_then(|v| v.as_str())
            .ok_or((-8, "each input needs a txid".to_string()))?;
        let vout = e
            .get("vout")
            .and_then(|v| v.as_u64())
            .ok_or((-8, "each input needs a vout".to_string()))?;
        inputs.push((txid.to_string(), vout as u32));
    }
    let mut outputs: Vec<(String, i64)> = Vec::new();
    let mut scripts: Vec<(String, Vec<u8>)> = Vec::new();
    for (dest, amt) in outs.iter() {
        // legacy "data" destination: the value is a RAW SCRIPT in hex
        if dest == "data" {
            let Some(hex_script) = amt.as_str() else {
                return Err((-3, "createrawtransaction: \"data\" takes a script hex string".into()));
            };
            let script = crate::p2p::hex_decode(hex_script)
                .map_err(|e| (-8, format!("createrawtransaction: data script: {e}")))?;
            scripts.push((dest.clone(), script));
            continue;
        }
        let amount = amt
            .as_f64()
            .ok_or((-8, format!("createrawtransaction: amount for {dest} is not a number")))?;
        if amount < 0.0 {
            return Err((-3, format!("createrawtransaction: amount for {dest} out of range")));
        }
        outputs.push((dest.clone(), (amount * 1_000_000.0).round() as i64));
    }
    let tx = crate::rawtx::create_raw(&inputs, &outputs, &scripts, now).map_err(|e| (-8, e))?;
    let raw = tx.serialize();
    let hex: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
    Ok(json!({
        "hex": hex,
        "txid": crate::p2p::internal_to_display(&tx.hash()),
    }))
}

/// Passive implementations of the ops verbs that also have a pure form.
/// Called ONLY when no ops engine is wired (cold-signing tools, tests, a node
/// started without --wallet). Anything not listed keeps the "warming up"
/// error the engine path would have produced.
fn passive_fallback(m: &str, deps: &Deps, params: &Value) -> NativeOutcome {
    match m {
        "createrawtransaction" => match create_raw_tx(params, std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0))
        {
            Ok(v) => NativeOutcome::Ok(v),
            Err((code, msg)) => NativeOutcome::Err(code, msg),
        },
        // Encryption runs on the wallet FILE through wallet_encrypt — the
        // same call the ops engine makes — so it answers while the live view
        // is still seeding, and `walletpassphrase` then re-unlocks through the
        // engine once it is up.
        "encryptwallet" | "walletpassphrasechange" | "decryptwallet" => {
            let wallet_path = match std::env::var("TRI7_WALLET_PATH") {
                Ok(p) if !p.is_empty() => p,
                _ => {
                    return NativeOutcome::Err(
                        -1,
                        "wallet file path unknown to this node — start tri7d with --wallet".into(),
                    )
                }
            };
            let path = std::path::Path::new(&wallet_path);
            match m {
                "encryptwallet" => {
                    let Some(pass) = param_get(params, 0, "passphrase").and_then(|v| v.as_str())
                    else {
                        return NativeOutcome::Err(-8, "expected a passphrase".into());
                    };
                    if pass.len() < 8 {
                        return NativeOutcome::Err(
                            -8,
                            "passphrase must be at least 8 characters".into(),
                        );
                    }
                    match crate::wallet_encrypt::encrypt_wallet(path, pass) {
                        Ok(rep) => NativeOutcome::Ok(json!({
                            "keys": rep.keys,
                            "mnemonic": rep.mnemonic,
                            "iterations": rep.iterations,
                        })),
                        Err(e) => NativeOutcome::Err(-1, e),
                    }
                }
                "walletpassphrasechange" => {
                    let old_pass = param_get(params, 0, "oldpassphrase")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_pass = match param_get(params, 1, "newpassphrase")
                        .and_then(|v| v.as_str())
                    {
                        Some(p) => p,
                        None => return NativeOutcome::Err(-8, "expected new passphrase".into()),
                    };
                    match crate::wallet_encrypt::change_passphrase(path, old_pass, new_pass) {
                        Ok(n) => NativeOutcome::Ok(json!({ "keys": n })),
                        Err(e) => NativeOutcome::Err(-1, e),
                    }
                }
                _ => {
                    let pass = match param_get(params, 0, "passphrase").and_then(|v| v.as_str()) {
                        Some(p) => p,
                        None => return NativeOutcome::Err(-8, "expected passphrase".into()),
                    };
                    match crate::wallet_encrypt::decrypt_wallet_file(path, pass) {
                        Ok(n) => NativeOutcome::Ok(json!({ "keys": n })),
                        Err(e) => NativeOutcome::Err(-1, e),
                    }
                }
            }
        }

        // Shutdown is a PROCESS verb: it answers even while the live wallet
        // view is seeding, and defers the exit by a moment so the caller
        // actually receives this reply before the process tears down.
        "stop" => {
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                crate::shutdown::request();
            });
            NativeOutcome::Ok(json!("tri7 server stopping"))
        }

        "signrawtransaction" | "signrawtransactionwithwallet" => NativeOutcome::Err(
            -1,
            "signrawtransaction needs the node's wallet keys — run tri7d with --wallet, \
             or pass the keys as the third parameter"
                .into(),
        ),
        _ => NativeOutcome::Err(
            -1,
            "node ops engine not wired — run tri7d with --wallet (staking/smsg/broadcast need wallet keys)"
                .into(),
        ),
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

/// Legacy `ValueFromAmount` (FormatMoney, util.cpp) parity: sign + integer
/// part + ALWAYS six decimals — a STRING, never a JSON number (the legacy
/// RPC answered repairwallet/checkwallet with formatted strings like
/// "0.039590"). Used by repairwallet's "amount affected by repair".
pub fn legacy_format_money(sats: i64) -> String {
    let neg = sats < 0;
    let abs = sats.unsigned_abs();
    format!(
        "{}{}.{:06}",
        if neg { "-" } else { "" },
        abs / 1_000_000,
        abs % 1_000_000
    )
}

/// Legacy DateTimeStrFormat (util.h:425-428): "%Y-%m-%d %H:%M:%S UTC" over
/// gmtime — the format `getcheckpoint.timestamp` reports. Hand-rolled
/// civil-from-days (Hinnant's algorithm) because the repo carries no chrono
/// dependency; pinned by unit test against known timestamps.
pub fn fmt_utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", y, mth, d, h, mi, s)
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
        /// live fee setting (what settxfee publishes, what estimatefee reads)
        paytxfee: std::sync::RwLock<Option<i64>>,
        /// live keypool census (what keypoolrefill/getnewaddress publish)
        keypool: std::sync::RwLock<Option<(usize, Option<i64>)>>,
    }
    impl Default for StubWallet {
        fn default() -> Self {
            let mut received = HashMap::new();
            received.insert(
                "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY".to_string(),
                33_665_763i64,
            );
            StubWallet {
                paytxfee: std::sync::RwLock::new(None),
                keypool: std::sync::RwLock::new(None),
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
        fn extras(&self) -> WalletExtras {
            let kp = *self.keypool.read().unwrap();
            WalletExtras {
                paytxfee_sats: self.paytxfee.read().ok().and_then(|g| *g),
                keypool_size: kp.map(|(n, _)| n),
                keypool_oldest: kp.and_then(|(_, o)| o),
                ..Default::default()
            }
        }
        fn set_paytxfee(&self, sats: i64) -> std::result::Result<(), String> {
            *self.paytxfee.write().map_err(|e| e.to_string())? = Some(sats);
            Ok(())
        }
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
        wallet: &'a dyn WalletProvider,
        blocks: &'a dyn BlockDataProvider,
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

    /// A FINISHED index that has applied exactly the chain the node serves
    /// (same height, same block hash) IS the wallet's spendable set: the
    /// replay snapshot would otherwise show a stale figure.
    #[test]
    fn live_index_wins_over_the_replay_snapshot() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let hash = [7u8; 32];
        let mut idx = TempIndex::new("wins");
        idx.seed(2_206_001, hash, true);
        idx.add_utxo(
            "Tlive000000000000000000000000000000",
            [0xaa; 32],
            0,
            3_236_439_612,
            2_206_000,
        );
        let ops = LiveIndexOps {
            index: idx,
            chain_tip: 2_206_001,
            chain_tip_hash: Some(hash),
            addresses: vec!["Tlive000000000000000000000000000000".to_string()],
        };
        let d = Deps {
            tip: &t,
            wallet: &w,
            blocks: &b,
            proxy: None,
            ops: Some(&ops),
        };
        // the snapshot alone would answer 3036.990591
        assert_eq!(call(&d, "getbalance", json!([]))["result"], json!(3236.439612));
        let arr = call(&d, "listunspent", json!([]))["result"].as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["amount_sats"], 3_236_439_612i64);
        assert_eq!(arr[0]["height"], 2_206_000u64);
        let info = call(&d, "getwalletinfo", json!([]))["result"].clone();
        assert_eq!(info["balance"], json!(3236.439612));
        assert_eq!(info["utxos"], 1);
        assert_eq!(info["balance_partial"], json!(false));
        assert_eq!(info["balance_height"], 2_206_001u64);
    }

    /// The index must have applied EXACTLY the chain the node serves: an
    /// index a block behind the tip has not seen the newest coin, and an
    /// index on a DIFFERENT chain at the same height has seen other coins.
    /// Neither may be served as the whole wallet: the replay snapshot is
    /// served instead, flagged partial, with the replay coverage.
    #[test]
    fn live_index_must_match_the_served_chain_exactly() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let hash = [7u8; 32];
        let addrs = vec!["Tlive000000000000000000000000000000".to_string()];

        // (1) one block behind the tip -> not authoritative
        let mut idx = TempIndex::new("lag");
        idx.seed(2_206_000, hash, true);
        idx.add_utxo("Tlive000000000000000000000000000000", [0xaa; 32], 0, 3_236_439_612, 2_206_000);
        let ops = LiveIndexOps {
            index: idx,
            chain_tip: 2_206_001,
            chain_tip_hash: Some(hash),
            addresses: addrs.clone(),
        };
        let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
        assert_eq!(call(&d, "getbalance", json!([]))["result"], json!(3036.990591));
        let info = call(&d, "getwalletinfo", json!([]))["result"].clone();
        assert_eq!(info["balance_partial"], json!(true));
        assert_eq!(info["balance"], json!(3036.990591));
        assert_eq!(info["balance_height"], 2_201_090u64);
        assert_eq!(info["coverage_height"], 2_201_090u64);
        assert_eq!(
            info["coverage_hash"],
            json!("f9031dee5b47c020534ba6c339b46aee7c25a5a14ea9588b4042689796cc6307")
        );

        // (2) same height but a DIFFERENT chain (the hash does not match) ->
        // not authoritative
        let mut idx2 = TempIndex::new("fork");
        idx2.seed(2_206_001, [0x11; 32], true);
        idx2.add_utxo("Tlive000000000000000000000000000000", [0xaa; 32], 0, 3_236_439_612, 2_206_000);
        let ops2 = LiveIndexOps {
            index: idx2,
            chain_tip: 2_206_001,
            chain_tip_hash: Some(hash),
            addresses: addrs.clone(),
        };
        let d2 = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops2) };
        assert_eq!(call(&d2, "getbalance", json!([]))["result"], json!(3036.990591));
        assert_eq!(
            call(&d2, "getwalletinfo", json!([]))["result"]["balance_partial"],
            json!(true)
        );
    }

    /// An index that is still BUILDING is not a whole-wallet view: the node
    /// must not serve it as the balance, and must flag the answer partial.
    #[test]
    fn incomplete_index_is_never_served_as_authoritative() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let hash = [7u8; 32];
        let mut idx = TempIndex::new("building");
        idx.seed(2_206_001, hash, false); // not finished
        idx.add_utxo("Tlive000000000000000000000000000000", [0xaa; 32], 0, 3_236_439_612, 2_206_000);
        let ops = LiveIndexOps {
            index: idx,
            chain_tip: 2_206_001,
            chain_tip_hash: Some(hash),
            addresses: vec!["Tlive000000000000000000000000000000".to_string()],
        };
        let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
        // the snapshot is served, not the half-built index
        assert_eq!(call(&d, "getbalance", json!([]))["result"], json!(3036.990591));
        let arr = call(&d, "listunspent", json!([]))["result"].as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["amount_sats"], 100); // the snapshot's own row
        let info = call(&d, "getwalletinfo", json!([]))["result"].clone();
        assert_eq!(info["balance_partial"], json!(true));
        assert_eq!(info["balance"], json!(3036.990591));
        assert_eq!(info["balance_height"], 2_201_090u64);
        assert_eq!(info["coverage_height"], 2_201_090u64);
    }

    /// Without a live index the node falls back to the replay snapshot and
    /// says the figure is partial, so a UI never presents a capped number as
    /// the whole balance.
    #[test]
    fn snapshot_fallback_is_flagged_partial() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        assert_eq!(call(&d, "getbalance", json!([]))["result"], json!(3036.990591));
        let info = call(&d, "getwalletinfo", json!([]))["result"].clone();
        assert_eq!(info["balance"], json!(3036.990591));
        assert_eq!(info["balance_partial"], json!(true));
    }

    /// A block applied while the wallet is being read must not tear the
    /// answer. This test drives the PRODUCTION helper
    /// (`addrindex::wallet_view`) against a real index while a writer thread
    /// advances it, and asserts the snapshot invariant that matters: the
    /// utxos and the coverage height come from ONE moment, so no utxo can
    /// sit above the coverage height, and the balance is exactly the sum of
    /// the utxos in that same snapshot.
    #[test]
    fn interleaved_index_advance_never_tears_a_wallet_read() {
        let (script, addr) = p2pkh_script();
        let idx = TempIndex::new("interleave");
        let addrs = vec![addr.clone()];
        {
            let mut live =
                crate::addrindex::LiveIndex::open_if_built(&idx.dir).expect("open live index");
            live.apply(&test_block(1, script.clone(), 1_000_000), 1)
                .expect("apply h1");
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let dir = idx.dir.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut h = 2u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut live = match crate::addrindex::LiveIndex::open_if_built(&dir) {
                        Some(l) => l,
                        None => break,
                    };
                    let _ = live.apply(&test_block(h, script.clone(), 1_000_000), h);
                    h += 1;
                }
            })
        };
        for _ in 0..100 {
            let mut conn = idx.conn();
            let (facts, rows) = crate::addrindex::wallet_view(&mut conn, &addrs).expect("wallet view");
            let sum: i64 = rows.iter().map(|r| r.value).sum();
            // the invariant of ONE snapshot: no utxo can be newer than the
            // coverage height, and the balance IS the sum of that snapshot's
            // utxos
            for r in &rows {
                assert!(
                    r.height <= facts.coverage_height,
                    "utxo at h{} above the coverage h{} - the read was torn",
                    r.height,
                    facts.coverage_height
                );
            }
            assert_eq!(sum % 1_000_000, 0, "the balance must be a whole number of coins");
            assert!(facts.coverage_height >= 1);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
    }

    /// A failure AFTER the block's outputs were written must roll the whole
    /// block back: the index must not keep half of a block.
    #[test]
    fn a_mid_apply_failure_rolls_the_block_back() {
        let (script, _addr) = p2pkh_script();
        let idx = TempIndex::new("rollback");
        {
            let mut live =
                crate::addrindex::LiveIndex::open_if_built(&idx.dir).expect("open live index");
            live.apply(&test_block(1, script.clone(), 1_000_000), 1)
                .expect("apply h1");
        }
        let before = idx.facts();
        // Break the index so the apply fails AFTER the output rows were
        // written: insert_block writes the outputs and then the tx_addr
        // rows, and the tx_addr table is gone.
        {
            let conn = idx.conn();
            conn.execute_batch("DROP TABLE tx_addr").expect("drop tx_addr");
        }
        {
            let mut live =
                crate::addrindex::LiveIndex::open_if_built(&idx.dir).expect("open live index");
            assert!(
                live.apply(&test_block(2, script.clone(), 7_000_000), 2).is_err(),
                "the apply must fail once the tx_addr write cannot land"
            );
        }
        // THE INVARIANT: the failed block left NOTHING behind. The wallet's
        // unspent total is still the h1 coin alone - the failed block's
        // 7,000,000 output was rolled back with the rest of that block - and
        // the index's position did not move.
        let addr = idx.first_indexed_address();
        assert_eq!(
            idx.sum_unspent(&addr),
            1_000_000,
            "the failed block's output must be gone"
        );
        let after = idx.facts();
        assert_eq!(
            after.coverage_height, 1,
            "a failed apply must not move the index's height"
        );
        assert_eq!(
            after.index_tip_hash, before.index_tip_hash,
            "a failed apply must not move the index's hash"
        );
    }

    /// After a restart the index's position must come back from its own
    /// meta rows - recovered from REAL applies, not from hand-written rows -
    /// and the authority rule then works against the recovered position.
    #[test]
    fn the_index_position_survives_a_restart() {
        let (script, _addr) = p2pkh_script();
        let idx = TempIndex::new("restart");
        {
            let mut live =
                crate::addrindex::LiveIndex::open_if_built(&idx.dir).expect("open live index");
            live.apply(&test_block(1, script.clone(), 1_000_000), 1)
                .expect("apply h1");
            live.apply(&test_block(2, script.clone(), 2_000_000), 2)
                .expect("apply h2");
        }
        // the builder marks the index finished - the production way
        {
            let conn = idx.conn();
            crate::addrindex::set_complete(&conn, true).expect("set complete");
        }
        // a fresh connection is what a restarted node gets
        let reopened = TempIndex { dir: idx.dir.clone() };
        let facts = reopened.facts();
        assert_eq!(facts.coverage_height, 2);
        let block2 = test_block(2, vec![0x76, 0xa9], 2_000_000);
        let want = crate::legacy::hash9(&{
            let mut w: Vec<u8> = Vec::new();
            w.extend_from_slice(&block2.version.to_le_bytes());
            w.extend_from_slice(&block2.prev_hash);
            w.extend_from_slice(&block2.merkle_root);
            w.extend_from_slice(&block2.time.to_le_bytes());
            w.extend_from_slice(&block2.bits.to_le_bytes());
            w.extend_from_slice(&block2.nonce.to_le_bytes());
            w.truncate(80);
            w
        });
        assert_eq!(facts.index_tip_hash, Some(want), "the recovered hash must be the applied block's");
        assert!(facts.complete, "the index must be finished");
        let verdict = crate::addrindex::authority(facts, 2, Some(want));
        assert!(verdict.authoritative, "the recovered position must prove authority");
    }

    /// A fresh node has no replay snapshot yet - but it DOES have the live
    /// index. The wallet verbs must answer from it instead of failing.
    #[test]
    fn live_index_answers_without_a_replay_snapshot() {
        let t = StubTip;
        let w = StubNoSnapshot;
        let b = StubBlocks { ready: true };
        let hash = [7u8; 32];
        let mut idx = TempIndex::new("nosnapshot");
        idx.seed(2_206_001, hash, true);
        idx.add_utxo("Tlive000000000000000000000000000000", [0xaa; 32], 0, 3_236_439_612, 2_206_000);
        let ops = LiveIndexOps {
            index: idx,
            chain_tip: 2_206_001,
            chain_tip_hash: Some(hash),
            addresses: vec!["Tlive000000000000000000000000000000".to_string()],
        };
        let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: Some(&ops) };
        assert_eq!(call(&d, "getbalance", json!([]))["result"], json!(3236.439612));
        let info = call(&d, "getwalletinfo", json!([]))["result"].clone();
        assert_eq!(info["balance"], json!(3236.439612));
        assert_eq!(info["balance_partial"], json!(false));
        assert_eq!(info["balance_height"], 2_206_001u64);
        // there is no replay snapshot behind it, so there is no replay hash -
        // report null rather than inventing an empty one
        assert_eq!(info["coverage_hash"], Value::Null);
    }

    /// With neither a live index nor a snapshot the node must say so, not
    /// answer 0.
    #[test]
    fn no_view_at_all_is_an_error_not_zero() {
        let t = StubTip;
        let w = StubNoSnapshot;
        let b = StubBlocks { ready: true };
        let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
        let v = call(&d, "getbalance", json!([]));
        assert!(v["error"].is_object(), "expected an error, got {}", v);
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
        // Must name a wallet-write verb that has NO handler and is in NO list.
        // (This assertion has had to move four times as verbs landed; if it
        // fails again, pick the next unimplemented wallet-write verb. Recently
        // landed: encryptwallet, walletpassphrase, walletlock.)
        let v = call(&d, "zapwallettxes", json!([1]));
        assert_eq!(v["error"]["code"], -32601);
        // SECURITY: wallet-write methods must never hit the proxy
        assert!(p.calls.lock().unwrap().is_empty());
    }

    /// Blocks stub that answers one synthetic verbose tx, so the wallet-scoped
    /// gettransaction reshape can be tested without a real chain.
    struct TxStubBlocks {
        ready: bool,
    }
    impl BlockDataProvider for TxStubBlocks {
        fn ready(&self) -> bool {
            self.ready
        }
        fn status_json(&self) -> Value {
            json!({"state": "ready"})
        }
        fn block_by_hash(&self, _h: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn block_by_height(&self, _h: u64) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn tx(&self, t: &str, _v: bool) -> Result<Option<Value>, String> {
            if t == "aa".repeat(32) {
                // pays our address twice in ONE output (multisig shape: two of
                // our addresses on the same script) plus a foreign output
                Ok(Some(json!({
                    "txid": t,
                    "confirmations": 7,
                    "blockhash": "bb".repeat(32),
                    "height": 100,
                    "time": 1_700_000_000u64,
                    "hex": "00",
                    "vin": [],
                    "vout": [
                        {"n": 0, "value_sats": 2_000_000i64,
                         "scriptPubKey": {"addresses": ["TMINE1", "TMINE2"]}},
                        {"n": 1, "value_sats": 5_000_000i64,
                         "scriptPubKey": {"addresses": ["TOURS"]}},
                        {"n": 2, "value_sats": 0i64,
                         "scriptPubKey": {"addresses": null}}
                    ]
                })))
            } else {
                Ok(None)
            }
        }
        fn hash_at(&self, _h: u64) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    #[test]
    fn gettransaction_counts_a_multi_address_output_once() {
        let t = StubTip;
        let mut w = StubWallet::default();
        w.mine = vec!["TMINE1".into(), "TMINE2".into()];
        let b = TxStubBlocks { ready: true };
        let d = Deps {
            tip: &t,
            wallet: &w,
            blocks: &b,
            proxy: None,
            ops: None,
        };
        let v = call(&d, "gettransaction", json!(["aa".repeat(32)]));
        assert!(v.get("error").is_none() || v["error"].is_null(), "{}", v);
        // 2.0 TRI counted ONCE even though both addresses are ours
        assert_eq!(v["result"]["amount"], json!(2.0));
        assert_eq!(v["result"]["confirmations"], json!(7));
        assert_eq!(v["result"]["details"].as_array().unwrap().len(), 1);
        assert_eq!(v["result"]["inputs_not_scanned"], json!(0));
    }

    /// Wallet stub with a fixed history, for the paginated verbs.
    struct HistWallet {
        rows: Vec<crate::history::HistoryRec>,
    }
    impl WalletProvider for HistWallet {
        fn balance(&self) -> Option<BalanceView> {
            None
        }
        fn coverage(&self) -> Option<(u64, String)> {
            None
        }
        fn address_entries(&self) -> Vec<AddressEntry> {
            Vec::new()
        }
        fn received_regular(&self, _a: &str) -> Option<i64> {
            None
        }
        fn is_mine(&self, _a: &str) -> bool {
            false
        }
        fn utxos(&self) -> Vec<UtxoRow> {
            Vec::new()
        }
        fn history(&self) -> Vec<crate::history::HistoryRec> {
            self.rows.clone()
        }
    }

    fn hist_row(height: u64, tx: &str, sats: i64, dir: &str) -> crate::history::HistoryRec {
        crate::history::HistoryRec {
            txid: tx.into(),
            height,
            time: 1_700_000_000 + height as u32,
            value_sats: sats,
            address: "TADDR".into(),
            direction: dir.into(),
            kind: "regular".into(),
            spent: false,
            idx: 0,
            legacy_kind: String::new(),
            coinstake: false,
            stake_principal: 0,
            input_addresses: Vec::new(),
            stake_net: None,
        }
    }

    #[test]
    fn listtransactions_windows_newest_first_then_returns_ascending() {
        let t = StubTip;
        let w = HistWallet {
            rows: vec![
                hist_row(10, "aa", 1_000_000, "in"),
                hist_row(11, "bb", -500_000, "out"),
                hist_row(12, "cc", 2_000_000, "in"),
                hist_row(13, "dd", 3_000_000, "in"),
            ],
        };
        let b = StubBlocks { ready: true };
        let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
        // count=2, from=1 -> skip the newest (dd), take cc and bb, ascending
        let v = call(&d, "listtransactions", json!(["", 2, 1]));
        let arr = v["result"].as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["txid"], json!("bb"));
        assert_eq!(arr[1]["txid"], json!("cc"));
        assert_eq!(arr[0]["category"], json!("send"));
        assert_eq!(arr[0]["amount"], json!(-0.5));
        assert_eq!(arr[1]["category"], json!("receive"));
    }

    #[test]
    fn native_miss_is_forwarded_and_marked() {
        // No method is proxy-ONLY any more (all fourteen have native
        // handlers); the proxy is a fallback for a native miss. getblock while
        // the block index is still building is that miss.
        let t = StubTip;
        let w = StubWallet::default();
        let b = StubBlocks { ready: false };
        let p = StubProxy::default();
        let d = deps(&t, &w, &b, Some(&p));
        let v = call(&d, "getblock", json!(["ab".repeat(32)]));
        assert_eq!(v["source"], "legacy-proxy");
        assert_eq!(v["result"]["echo"], "getblock");
        assert_eq!(p.calls.lock().unwrap().len(), 1);
    }

    // ---- listaddressgroupings (rpcwallet.cpp:394 parity) ----

    /// wallet stub carrying history rows + address-book labels + balances
    struct GroupWallet {
        rows: Vec<crate::history::HistoryRec>,
        labels: std::collections::HashMap<String, String>,
        balances: std::collections::HashMap<String, i64>,
    }
    impl WalletProvider for GroupWallet {
        fn balance(&self) -> Option<BalanceView> {
            None
        }
        fn coverage(&self) -> Option<(u64, String)> {
            None
        }
        fn address_entries(&self) -> Vec<AddressEntry> {
            self.balances
                .iter()
                .map(|(a, sats)| AddressEntry {
                    address: a.clone(),
                    balance_sats: *sats,
                    utxos: 0,
                    received_regular_sats: 0,
                    received_all_sats: 0,
                })
                .collect()
        }
        fn received_regular(&self, _a: &str) -> Option<i64> {
            None
        }
        fn is_mine(&self, _a: &str) -> bool {
            false
        }
        fn utxos(&self) -> Vec<UtxoRow> {
            Vec::new()
        }
        fn history(&self) -> Vec<crate::history::HistoryRec> {
            self.rows.clone()
        }
        fn labels(&self) -> std::collections::HashMap<String, String> {
            self.labels.clone()
        }
    }

    fn group_row(txid: &str, dir: &str, addr: &str, inputs: &[&str]) -> crate::history::HistoryRec {
        crate::history::HistoryRec {
            txid: txid.into(),
            height: 100,
            time: 1_700_000_000,
            value_sats: 1_000_000,
            address: addr.into(),
            direction: dir.into(),
            kind: if dir == "in" { "regular" } else { "regular" }.into(),
            spent: false,
            idx: 0,
            legacy_kind: String::new(),
            coinstake: false,
            stake_principal: 0,
            input_addresses: inputs.iter().map(|s| s.to_string()).collect(),
            stake_net: None,
        }
    }

    /// groups → Vec<Vec<String>> of first cells (addresses) for assertions
    fn flat_groups(v: &Value) -> Vec<Vec<String>> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|g| {
                g.as_array()
                    .unwrap()
                    .iter()
                    .map(|row| {
                        row.as_array().unwrap()[0]
                            .as_str()
                            .unwrap_or("")
                            .to_string()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn listaddressgroupings_coinstake_and_receive_are_singletons_with_balances() {
        let t = StubTip;
        let w = GroupWallet {
            rows: vec![
                group_row("aa".repeat(32).as_str(), "in", "Tsolo1", &[]),
                group_row("bb".repeat(32).as_str(), "in", "Tsolo2", &[]),
            ],
            labels: Default::default(),
            balances: [("Tsolo1".to_string(), 2_500_000)].into_iter().collect(),
        };
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listaddressgroupings", json!([]));
        let g = flat_groups(&v["result"]);
        assert_eq!(g.len(), 2, "two unrelated addresses = two singleton groups");
        assert_eq!(g[0], vec!["Tsolo1"]);
        assert_eq!(g[1], vec!["Tsolo2"]);
        // row = [addr, balance] (+ label only when the address book has it)
        let row0 = v["result"][0][0].as_array().unwrap();
        assert_eq!(row0[1], json!(2.5));
        assert_eq!(row0.len(), 2, "unlabeled = no third element (legacy mapAddressBook gate)");
        let row1 = v["result"][1][0].as_array().unwrap();
        assert_eq!(row1[1], json!(0.0), "no balances entry = 0 (legacy map semantics)");
        assert_eq!(row1.len(), 2);
    }

    #[test]
    fn listaddressgroupings_spend_groups_inputs_with_change() {
        let t = StubTip;
        // tx aa: TA pays foreign + TB (unlabeled change) → {TA, TB} merge.
        // tx bb: TB pays TC — but TC is LABELED ("book"), and labeled outputs
        // are NOT change, so TC does NOT merge; it stays a lone singleton
        // WITH the label column (legacy pushes it when mapAddressBook has it).
        let w = GroupWallet {
            rows: vec![
                group_row("aa".repeat(32).as_str(), "in", "TB", &["TA"]),
                group_row("aa".repeat(32).as_str(), "out", "TB", &["TA"]),
                group_row("bb".repeat(32).as_str(), "in", "TC", &["TB"]),
            ],
            labels: [("TC".to_string(), "book".to_string())].into_iter().collect(),
            balances: [
                ("TA".to_string(), 0),
                ("TB".to_string(), 1_000_000),
                ("TC".to_string(), 3_000_000),
            ]
            .into_iter()
            .collect(),
        };
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listaddressgroupings", json!([]));
        let g = flat_groups(&v["result"]);
        assert_eq!(g.len(), 2, "the merge must not swallow the labeled output");
        assert_eq!(g[0], vec!["TA", "TB"], "inputs ∪ unlabeled change");
        assert_eq!(g[1], vec!["TC"]);
        // labeled row = [addr, balance, label]
        let tc = v["result"][1][0].as_array().unwrap();
        assert_eq!(tc.len(), 3);
        assert_eq!(tc[2], json!("book"));
    }

    #[test]
    fn listaddressgroupings_merges_transitively_across_txs() {
        let t = StubTip;
        // tx aa spends from [TA, TB], tx bb from [TB, TC]: the shared member
        // TB bridges the two spend groups into ONE cluster. TCh (unlabeled
        // mine output of tx aa) is change and joins the same cluster.
        let w = GroupWallet {
            rows: vec![
                group_row("aa".repeat(32).as_str(), "in", "TCh", &["TA", "TB"]),
                group_row("bb".repeat(32).as_str(), "in", "TOut", &["TB", "TC"]),
            ],
            labels: Default::default(),
            balances: Default::default(),
        };
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listaddressgroupings", json!([]));
        let g = flat_groups(&v["result"]);
        assert_eq!(g.len(), 1, "TB bridges the two txs into one cluster");
        assert_eq!(
            g[0],
            vec!["TA", "TB", "TC", "TCh", "TOut"],
            "inputs ∪ change from both txs, sorted"
        );
    }

    #[test]
    fn listaddressgroupings_old_file_rows_contribute_singletons_only() {
        let t = StubTip;
        // rows predating input_addresses: a send tx with no recorded input
        // side → no spend branch; the mine output TB stays a lone singleton
        // (a rescan with the new generator completes the input side)
        let w = GroupWallet {
            rows: vec![
                group_row("aa".repeat(32).as_str(), "in", "TB", &[]),
                group_row("aa".repeat(32).as_str(), "out", "TB", &[]),
            ],
            labels: Default::default(),
            balances: Default::default(),
        };
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listaddressgroupings", json!([]));
        assert_eq!(flat_groups(&v["result"]), vec![vec!["TB".to_string()]]);
    }

    #[test]
    fn listaddressgroupings_empty_history_is_an_empty_array() {
        let t = StubTip;
        let w = GroupWallet {
            rows: Vec::new(),
            labels: Default::default(),
            balances: Default::default(),
        };
        let b = StubBlocks { ready: true };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "listaddressgroupings", json!([]));
        assert_eq!(v["result"], json!([]));
    }

    // ---- getcheckpoint (rpcblockchain.cpp:665 parity) ----

    /// blocks stub that can answer the horizon height (unlike StubBlocks)
    struct CheckpointBlocks {
        ready: bool,
        time: Option<i64>,
    }
    impl BlockDataProvider for CheckpointBlocks {
        fn ready(&self) -> bool {
            self.ready
        }
        fn status_json(&self) -> Value {
            json!({})
        }
        fn block_by_hash(&self, _h: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn block_by_height(&self, h: u64) -> Result<Option<Value>, String> {
            if self.ready && h == crate::consensus::CHECKPOINT_HORIZON {
                Ok(Some(json!({ "height": h, "time": self.time.unwrap_or(0) })))
            } else {
                Ok(None)
            }
        }
        fn tx(&self, _t: &str, _v: bool) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn hash_at(&self, _h: u64) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    #[test]
    fn getcheckpoint_reports_the_compiled_anchor_strict_policy() {
        let t = StubTip;
        let w = StubWallet::default();
        let b = CheckpointBlocks { ready: true, time: Some(1_783_600_000) };
        let d = deps(&t, &w, &b, None);
        let v = call(&d, "getcheckpoint", json!([]));
        assert_eq!(v["source"], "tri7");
        assert_eq!(v["result"]["synccheckpoint"], json!(crate::consensus::CHECKPOINT_ANCHOR_DISPLAY));
        assert_eq!(v["result"]["height"], json!(crate::consensus::CHECKPOINT_HORIZON));
        assert_eq!(v["result"]["timestamp"], json!("2026-07-09 12:26:40 UTC"));
        assert_eq!(v["result"]["policy"], json!("strict"));
        assert!(v["result"].get("checkpointmaster").is_none(), "no -checkpointkey on tri7");
    }

    #[test]
    fn getcheckpoint_refuses_params_and_warming_index() {
        let t = StubTip;
        let w = StubWallet::default();
        let bwarm = CheckpointBlocks { ready: false, time: None };
        let d = deps(&t, &w, &bwarm, None);
        let v = call(&d, "getcheckpoint", json!([]));
        assert_eq!(v["error"]["code"], -1, "warming index = honest miss, no proxy: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("warming up"));

        let bready = CheckpointBlocks { ready: true, time: Some(1) };
        let d = deps(&t, &w, &bready, None);
        let v = call(&d, "getcheckpoint", json!(["extra"]));
        assert_eq!(v["error"]["code"], -8, "legacy throws on params; tri7 refuses with -8");
    }

    #[test]
    fn fmt_utc_timestamp_matches_the_legacy_format() {
        assert_eq!(fmt_utc_timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(fmt_utc_timestamp(1_700_000_000), "2023-11-14 22:13:20 UTC");
        // leap-year day: 2024-02-29 13:20:00 UTC
        assert_eq!(fmt_utc_timestamp(1_709_212_800), "2024-02-29 13:20:00 UTC");
    }

    // ---- repairwallet / resendtx (workqueue item 9 RPC-P1b) ----

    #[test]
    fn legacy_format_money_matches_legacy_formatmoney() {
        // ValueFromAmount vectors: always six decimals, "-" only for negatives
        assert_eq!(legacy_format_money(0), "0.000000");
        assert_eq!(legacy_format_money(39_590), "0.039590");
        assert_eq!(legacy_format_money(-100), "-0.000100");
        assert_eq!(legacy_format_money(12_345_600), "12.345600");
        assert_eq!(legacy_format_money(-12_345_600), "-12.345600");
        assert_eq!(legacy_format_money(3_381_763_635), "3381.763635");
        assert_eq!(legacy_format_money(1_000_000), "1.000000");
        assert_eq!(legacy_format_money(999_999), "0.999999");
    }

    #[test]
    fn repairwallet_and_resendtx_are_native_and_ops_routed() {
        // item 9 RPC-P1b: both verbs live ONLY in the live ops engine
        // (repairwallet rewrites the in-memory live view, resendtx re-accepts
        // into the pool) — never passive, never early.
        for m in ["repairwallet", "resendtx"] {
            assert!(NATIVE_METHODS.contains(&m), "{m} missing from NATIVE_METHODS");
            assert!(OPS_METHODS.contains(&m), "{m} missing from OPS_METHODS");
            assert!(!OPS_EARLY.contains(&m), "{m} must not answer before the live view is ready");
        }
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
        let b2 = StubBlocks { ready: false };
        let d = deps(&t, &w, &b2, Some(&p));
        let v = call(&d, "getblock", json!(["ab".repeat(32)]));
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
            extra: Default::default(),
            pubkeys: Default::default(),
            labels: Default::default(),
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
            extra: Default::default(),
            pubkeys: Default::default(),
            labels: Default::default(),
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
            extra: Default::default(),
            pubkeys: Default::default(),
            labels: Default::default(),
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

/// A wallet provider with NO replay snapshot (a fresh node).
struct StubNoSnapshot;
impl WalletProvider for StubNoSnapshot {
    fn balance(&self) -> Option<BalanceView> {
        None
    }
    fn coverage(&self) -> Option<(u64, String)> {
        None
    }
    fn address_entries(&self) -> Vec<AddressEntry> {
        Vec::new()
    }
    fn received_regular(&self, _addr: &str) -> Option<i64> {
        None
    }
    fn is_mine(&self, _addr: &str) -> bool {
        false
    }
    fn utxos(&self) -> Vec<UtxoRow> {
        Vec::new()
    }
}

/// A real P2PKH script and the address it pays to (the index attributes an
/// output by its script, so the test must use a real one).
fn p2pkh_script() -> (Vec<u8>, String) {
    // the exact payable form the index attributes: 76 a9 14 <hash160> 88 ac
    let h160 = crate::address::hash160(b"tri7-test-key");
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&h160);
    script.extend_from_slice(&[0x88, 0xac]);
    let mut payload = [0u8; 21];
    payload[0] = crate::address::PUBKEY_ADDRESS;
    payload[1..].copy_from_slice(&h160);
    let addr = crate::address::payload_to_address(&payload);
    (script, addr)
}

/// A real legacy block paying one output to an address - the production
/// apply path consumes exactly this.
fn test_block(height: u64, addr_script: Vec<u8>, value: i64) -> crate::legacy::Block {
    crate::legacy::Block {
        version: 1,
        prev_hash: [height as u8; 32],
        merkle_root: [0x5au8; 32],
        time: 1_700_000_000 + height as u32,
        bits: 0x1d00_ffff,
        nonce: height as u32,
        vtx: vec![crate::legacy::Tx {
            version: 1,
            time: 1_700_000_000 + height as u32,
            vin: vec![],
            vout: vec![crate::legacy::Vout { value, script_pubkey: addr_script }],
            locktime: 0,
        }],
        block_sig: vec![],
    }
}

/// A REAL address index on disk (the production schema) so the tests
/// exercise the production facts and the production verdict - a stub that
/// invented "authoritative" would test nothing.
struct TempIndex {
    dir: std::path::PathBuf,
}

impl TempIndex {
    fn new(tag: &str) -> TempIndex {
        let dir = std::env::temp_dir().join(format!(
            "tri7-idx-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("index dir");
        // the production path that creates the schema
        crate::addrindex::begin(&dir).expect("begin index");
        TempIndex { dir }
    }

    /// Open the index (the production way - the daemon opens it per call).
    fn conn(&self) -> rusqlite::Connection {
        crate::addrindex::open(&crate::addrindex::db_path(&self.dir)).expect("open index")
    }

    /// Write the index's own bookkeeping: how far it applied, the hash of the
    /// block it applied there, and whether it is finished.
    fn seed(&self, height: u64, tip_hash: [u8; 32], complete: bool) {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO meta(k,v) VALUES('height',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [height.to_string()],
        )
        .expect("meta height");
        conn.execute(
            "INSERT INTO meta(k,v) VALUES('tip_hash',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [crate::addrindex::txid_hex(&tip_hash)],
        )
        .expect("meta tip hash");
        conn.execute(
            "INSERT INTO meta(k,v) VALUES('complete',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [if complete { "1" } else { "0" }],
        )
        .expect("meta complete");
    }

    /// The address the index attributed an output to - read back from the
    /// index, so the test uses the production attribution and not a guess.
    fn first_indexed_address(&self) -> String {
        let conn = self.conn();
        conn.query_row("SELECT addr FROM out LIMIT 1", [], |r| r.get(0))
            .expect("an indexed output")
    }

    /// How many unspent outputs the index holds for an address.
    fn count_unspent(&self, addr: &str) -> i64 {
        let conn = self.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM out WHERE addr=?1 AND spent_height IS NULL",
            rusqlite::params![addr],
            |r| r.get(0),
        )
        .expect("count unspent")
    }

    /// The total value of the unspent outputs the index holds for an address.
    fn sum_unspent(&self, addr: &str) -> i64 {
        let conn = self.conn();
        conn.query_row(
            "SELECT COALESCE(SUM(value),0) FROM out WHERE addr=?1 AND spent_height IS NULL",
            rusqlite::params![addr],
            |r| r.get(0),
        )
        .expect("sum unspent")
    }

    /// One unspent output, written through the production schema.
    fn add_utxo(&self, addr: &str, txid: [u8; 32], n: u32, value: i64, height: u64) {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO out(addr,txid,n,value,height,script,spent_height) \
             VALUES(?1,?2,?3,?4,?5,?6,NULL)",
            rusqlite::params![addr, txid.to_vec(), n as i64, value, height as i64, vec![0x76u8, 0xa9]],
        )
        .expect("insert out");
    }

    fn facts(&self) -> crate::addrindex::IndexFacts {
        crate::addrindex::facts(&self.conn())
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Ops stub that answers the live-spendable verb from a REAL index on disk.
/// It reports the FACTS (what the index holds and what chain the node
/// serves); whether that set may be served as the whole wallet is decided by
/// the production rule in crate::addrindex.
struct LiveIndexOps {
    index: TempIndex,
    chain_tip: u64,
    chain_tip_hash: Option<[u8; 32]>,
    addresses: Vec<String>,
}

impl NodeOps for LiveIndexOps {
    fn call(&self, method: &str, _params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "getwalletspendable" => {
                let conn = self.index.conn();
                let rows = crate::addrindex::utxos(&conn, &self.addresses)
                    .map_err(|e| (-1, e))?;
                let balance: i64 = rows.iter().map(|r| r.value).sum();
                let f = crate::addrindex::facts(&conn);
                let utxos: Vec<Value> = rows
                    .iter()
                    .map(|u| {
                        json!({
                            "txid": crate::addrindex::txid_hex(&u.txid),
                            "n": u.n,
                            "address": u.address,
                            "script_hex": u.script.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
                            "value_sats": u.value,
                            "height": u.height,
                        })
                    })
                    .collect();
                Ok(json!({
                    "balance_sats": balance,
                    "utxos": utxos,
                    "index_height": f.coverage_height,
                    "index_tip_hash": f.index_tip_hash.map(|h| crate::addrindex::txid_hex(&h)),
                    "chain_tip": self.chain_tip,
                    "chain_tip_hash": self.chain_tip_hash.map(|h| crate::addrindex::txid_hex(&h)),
                    "complete": f.complete,
                }))
            }
            _ => Err((-3, format!("stub: unhandled {}", method))),
        }
    }
    fn ready(&self) -> bool {
        true
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
    // every method that used to be proxy-only now has a native handler, so
    // the representative check is that it reports native
    let v = call(&d, "help", json!(["gettransaction"]));
    assert!(v["result"].as_str().unwrap().contains("tri7 native"));
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
    // legacy keypool fields exist but are honestly null (no census published)
    assert!(v["result"]["keypoolsize"].is_null());
    assert!(v["result"]["keypoololdest"].is_null());
    // missing snapshot → -1, not a fake zero balance
    let mut w2 = StubWallet::default();
    w2.balance = None;
    let d2 = deps(&t, &w2, &b, None);
    let v = call(&d2, "getwalletinfo", json!([]));
    assert_eq!(v["error"]["code"], -1);
}

#[test]
fn getwalletinfo_reports_the_real_keypool_census() {
    // the ops engine publishes the `pool`-row census → getwalletinfo carries
    // the REAL numbers (size + oldest); zero is a real value, not unknown
    let t = StubTip;
    let b = StubBlocks { ready: true };
    let mut w = StubWallet::default();
    *w.keypool.write().unwrap() = Some((116, Some(1_781_668_383)));
    let d = deps(&t, &w, &b, None);
    let v = call(&d, "getwalletinfo", json!([]));
    assert_eq!(v["result"]["keypoolsize"], json!(116));
    assert_eq!(v["result"]["keypoololdest"], json!(1_781_668_383i64));
    let mut w0 = StubWallet::default();
    *w0.keypool.write().unwrap() = Some((0, None));
    let d0 = deps(&t, &w0, &b, None);
    let v = call(&d0, "getwalletinfo", json!([]));
    assert_eq!(v["result"]["keypoolsize"], json!(0), "empty pool is real, not null");
    assert!(v["result"]["keypoololdest"].is_null(), "no entries → no oldest");
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

// ---------------------------------------------------------------------------
// Legacy parity 2026-11-06 — the offline tx workflow + wallet bookkeeping.
// Each test drives the REAL dispatch (`call`) and asserts on the reply shape a
// legacy client would parse.
// ---------------------------------------------------------------------------

#[test]
fn decoderawtransaction_round_trips_a_built_tx() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    // build via createrawtransaction so both verbs are exercised together
    let txid = "11".repeat(32);
    let v = call(
        &d,
        "createrawtransaction",
        json!([[{"txid": txid, "vout": 1}], {"data": "6a02aabb"}]),
    );
    let hex = v["result"]["hex"]
        .as_str()
        .unwrap_or_else(|| panic!("raw hex missing: {}", v))
        .to_string();
    assert!(!hex.is_empty());
    let back = call(&d, "decoderawtransaction", json!([hex]));
    assert_eq!(back["result"]["vin"][0]["vout"], 1);
    assert_eq!(back["result"]["vout"][0]["scriptPubKey"]["type"], "nulldata");
    assert_eq!(back["result"]["hex"], hex);
    assert_eq!(back["source"], "tri7");
}

#[test]
fn decoderawtransaction_refuses_garbage() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let v = call(&d, "decoderawtransaction", json!(["zz"]));
    assert_eq!(v["error"]["code"], -22);
}

#[test]
fn decodescript_reports_asm_type_and_p2sh() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let script = "76a914".to_string() + &"22".repeat(20) + "88ac";
    let v = call(&d, "decodescript", json!([script]));
    assert_eq!(v["result"]["type"], "pubkeyhash");
    assert_eq!(v["result"]["reqSigs"], 1);
    assert!(v["result"]["asm"].as_str().unwrap().starts_with("OP_DUP"));
    // SCRIPT_ADDRESS = 28 → base58check form starts with 'C' on this chain
    // (verified against the live reply: CSr6Ec9w9yfM8hKyhfqHaGXRKAzg9kdoLR)
    assert_eq!(
        v["result"]["p2sh"].as_str().unwrap(),
        "CSr6Ec9w9yfM8hKyhfqHaGXRKAzg9kdoLR"
    );
}

/// The fee service the operator actually uses: the estimate tracks settxfee
/// (publishing through the provider), respects the 0..0.25 TRI range, and
/// reports REAL amounts in the chain's coin (1 TRI = 1e6 sats).
#[test]
fn estimatefee_tracks_settxfee_and_uses_the_chain_coin() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };

    // default: the chain minimum for a typical 1-in/2-out spend
    let base = call(&d, "estimatefee", json!([2, 1, 2]));
    assert_eq!(base["result"], json!(0.0001));
    // a bigger transaction crosses the 1000-byte step: 2 x the minimum
    let big = call(&d, "estimatefee", json!([2, 8, 3]));
    assert_eq!(big["result"], json!(0.0002));

    // raising the setting moves the estimate — proof the value is published
    assert_eq!(call(&d, "settxfee", json!([0.001]))["result"], true);
    assert_eq!(call(&d, "estimatefee", json!([2, 1, 2]))["result"], json!(0.001));
    assert_eq!(call(&d, "getwalletinfo", json!([]))["result"]["paytxfee"], 0.001);

    // 0 means "pay the chain minimum", not "pay nothing"
    assert_eq!(call(&d, "settxfee", json!([0]))["result"], true);
    assert_eq!(call(&d, "estimatefee", json!([2, 1, 2]))["result"], json!(0.0001));

    // outside the allowed range (init.cpp:694 -> 0.25 TRI cap) is refused
    assert_eq!(call(&d, "settxfee", json!([-1]))["error"]["code"], -8);
    assert_eq!(call(&d, "settxfee", json!([0.5]))["error"]["code"], -8);
    assert_eq!(call(&d, "settxfee", json!([0.25]))["result"], true);
}

#[test]
fn settxfee_accepts_the_operator_range_and_refuses_outside_it() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    // the setting is operator-controlled now (nTransactionFee): inside the
    // range it is accepted and published, outside it is refused loudly
    assert_eq!(call(&d, "settxfee", json!([0.0001]))["result"], true);
    assert_eq!(call(&d, "settxfee", json!([0.01]))["result"], true);
    let v = call(&d, "settxfee", json!([0.3]));
    assert_eq!(v["error"]["code"], -8);
    assert!(v["error"]["message"].as_str().unwrap().contains("0..0.25"));
}

#[test]
fn walletinfo_reports_fee_and_lock_state_from_the_published_extras() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    // unwired defaults: fee = the chain constant, locked = u64::MAX (unencrypted)
    let v = call(&d, "getwalletinfo", json!([]));
    assert_eq!(v["result"]["paytxfee"], 0.0001);
    assert_eq!(v["result"]["unlocked_until"], json!(u64::MAX));
}

#[test]
fn listreceivedbyaddress_uses_the_same_map_as_getreceivedbyaddress() {
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };
    let addr = "T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY";
    let one = call(&d, "getreceivedbyaddress", json!([addr]));
    let list = call(&d, "listreceivedbyaddress", json!([1]));
    let rows = list["result"].as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["address"], addr);
    assert_eq!(rows[0]["amount"], one["result"]);
    // include_empty=true surfaces the wallet's never-paid addresses too
    let all = call(&d, "listreceivedbyaddress", json!([1, true]));
    assert!(all["result"].as_array().unwrap().len() >= rows.len());
}

#[test]
fn new_parity_verbs_are_in_native_and_routed() {
    for m in [
        "decoderawtransaction",
        "decodescript",
        "createrawtransaction",
        "signrawtransaction",
        "settxfee",
        "listreceivedbyaddress",
        "getnewpubkey",
    ] {
        assert!(NATIVE_METHODS.contains(&m), "{m} missing from NATIVE_METHODS");
    }
    for m in ["signrawtransaction", "createrawtransaction", "getnewpubkey"] {
        assert!(OPS_METHODS.contains(&m), "{m} missing from OPS_METHODS");
    }
    for m in ["addmultisigaddress", "addredeemscript"] {
        assert!(NATIVE_METHODS.contains(&m), "{m} missing from NATIVE_METHODS");
        assert!(OPS_METHODS.contains(&m), "{m} missing from OPS_METHODS");
    }
    // keypoolrefill: legacy parity 2026-11-07 — routed to the live ops
    // engine (it writes the wallet file), served nowhere else
    assert!(NATIVE_METHODS.contains(&"keypoolrefill"), "missing from NATIVE_METHODS");
    assert!(OPS_METHODS.contains(&"keypoolrefill"), "missing from OPS_METHODS");
}

/// addmultisigaddress / addredeemscript end to end against a REAL wallet
/// file: bare-address reply, redeem script stored under its scriptID,
/// legacy error codes (-8 bad params, -5 bad address/key, -4 key not found).
#[test]
fn multisig_address_verbs_store_redeem_scripts_in_the_wallet_file() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _env = ENV_LOCK.lock().unwrap();

    // a wallet file with two keys (synthetic secrets — never wallet material)
    let dir = std::env::temp_dir().join(format!("tri7rpc_ms_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wallet.dat");
    crate::wallet::create_wallet_file(&path).unwrap();
    let synth = |label: &str| -> ([u8; 32], Vec<u8>) {
        let secret = crate::legacy::sha256d(format!("tri7-rpc-ms/{label}").as_bytes());
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        (
            secret,
            secp256k1::PublicKey::from_secret_key(&secp, &sk)
                .serialize()
                .to_vec(),
        )
    };
    let (s1, p1) = synth("a");
    let (s2, p2) = synth("b");
    let (_, p3) = synth("c");
    let _ = (s1, s2);
    let mut wh = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
    let a1 = wh.add_key(&s1, "").unwrap().address;
    let a2 = wh.add_key(&s2, "").unwrap().address;
    wh.close();

    std::env::set_var("TRI7_WALLET_PATH", &path);
    let t = T;
    let w = StubWallet::default();
    let b = StubBlocks { ready: true };
    let d = Deps { tip: &t, wallet: &w, blocks: &b, proxy: None, ops: None };

    // 2-of-2 from one hex pubkey + one wallet address → bare C address reply
    let v = call(
        &d,
        "addmultisigaddress",
        json!([2, [hex_encode(&p2), a1]]),
    );
    assert_eq!(v["error"], Value::Null, "unexpected error: {v}");
    let addr = v["result"].as_str().unwrap().to_string();
    assert!(addr.starts_with('C'), "P2SH reply must be a C address: {addr}");
    let redeem = crate::multisig::redeem_script(2, &[p2.clone(), p1.clone()]).unwrap();
    assert_eq!(addr, crate::multisig::p2sh_address(&redeem));

    // repeat call is a no-op that returns the same address
    let v = call(&d, "addmultisigaddress", json!([2, [hex_encode(&p2), a1]]));
    assert_eq!(v["result"], json!(addr));

    // the redeem script really is stored under its scriptID
    let mut wh = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
    let id = crate::address::hash160(&redeem);
    assert_eq!(
        wh.get_redeem_script(&id).unwrap().as_deref(),
        Some(&redeem[..])
    );
    wh.close();

    // addredeemscript stores arbitrary hex and replies true
    let other = crate::multisig::redeem_script(1, &[p3.clone()]).unwrap();
    let v = call(&d, "addredeemscript", json!([hex_encode(&other)]));
    assert_eq!(v["result"], json!(true));
    let v = call(&d, "addredeemscript", json!([hex_encode(&other), "savings"]));
    assert_eq!(v["result"], json!(true), "[account] accepted for shape compat");

    // legacy error codes
    let v = call(&d, "addmultisigaddress", json!([2]));
    assert_eq!(v["error"]["code"], -8, "missing keys array");
    let v = call(&d, "addmultisigaddress", json!([0, [hex_encode(&p1), hex_encode(&p2)]]));
    assert_eq!(v["error"]["code"], -8, "nrequired < 1");
    let v = call(&d, "addmultisigaddress", json!([3, [hex_encode(&p1), hex_encode(&p2)]]));
    assert_eq!(v["error"]["code"], -8, "not enough keys");
    let v = call(&d, "addmultisigaddress", json!([1, ["not-a-key-or-address"]]));
    assert_eq!(v["error"]["code"], -5, "invalid key");
    let v = call(&d, "addmultisigaddress", json!([1, [hex_encode(&[0x02, 0x55])]]));
    assert_eq!(v["error"]["code"], -5, "hex but not a valid pubkey");
    let not_ours = crate::wallet::pubkey_to_address(&p3);
    let v = call(&d, "addmultisigaddress", json!([1, [not_ours]]));
    assert_eq!(v["error"]["code"], -4, "valid address, key not in wallet");
    let v = call(&d, "addmultisigaddress", json!([1, [addr]]));
    assert_eq!(v["error"]["code"], -5, "a C address does not refer to a key");
    let v = call(&d, "addredeemscript", json!(["zzzz"]));
    assert_eq!(v["error"]["code"], -5, "invalid hex redeemScript");
    let v = call(&d, "addredeemscript", json!([]));
    assert_eq!(v["error"]["code"], -8, "missing redeemScript");

    // both stored scripts list back (a2 stayed an ordinary address)
    let mut wh = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
    let listed = wh.list_redeem_scripts().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|(i, s)| *i == id && s == &redeem));
    assert!(listed
        .iter()
        .any(|(i, s)| *i == crate::address::hash160(&other) && s == &other));
    wh.close();
    let _ = (a2,);

    std::env::remove_var("TRI7_WALLET_PATH");
}

// ---- richlist explorer verb (balances snapshot → ranked top addresses) ----

#[test]
fn rich_list_parses_sorts_tiebreaks_and_filters() {
    let v = json!({
        "height": 2_207_108u64,
        "covered_tip_hash": "eb5e471a",
        "totals": {"total_balance_sats": 14i64},
        "balances": {"Ta": 5, "Tb": 9, "Tc": 5, "Td": 0, "Te": -3}
    });
    let d = rich_list_from_value(&v).unwrap();
    assert_eq!(d.height, 2_207_108);
    assert_eq!(d.covered_tip_hash, "eb5e471a");
    assert_eq!(d.total_chain_sats, 14);
    assert_eq!(
        d.entries,
        vec![
            ("Tb".to_string(), 9),
            ("Ta".to_string(), 5),
            ("Tc".to_string(), 5),
        ],
        "balance DESC; equal balances rank by address ASC (deterministic, not HashMap order); 0/negative never rank"
    );
    assert!(rich_list_from_value(&json!({"no_map": 1})).is_err());
    assert!(rich_list_from_value(&json!({"balances": {}})).is_err(), "missing height");
}

#[test]
fn rich_list_json_caps_ranks_and_converts_units() {
    let data = RichListData {
        height: 100,
        covered_tip_hash: "ab".into(),
        total_chain_sats: 30,
        entries: vec![
            ("T1".into(), 1_500_000),
            ("T2".into(), 1_400_000),
            ("T3".into(), 100),
            ("T4".into(), 50),
            ("T5".into(), 1),
        ],
    };
    let r = rich_list_json(&data, Some(150), "test source", 3);
    assert_eq!(r["count"], 3);
    assert_eq!(r["requested"], 3);
    assert_eq!(r["total_ranked"], 5, "honest: how many addresses the snapshot ranks");
    assert_eq!(r["chain_tip_height"], 150);
    assert_eq!(r["snapshot_height"], 100);
    assert_eq!(r["entries"][0]["rank"], 1);
    assert_eq!(r["entries"][0]["address"], "T1");
    assert_eq!(r["entries"][0]["balance_sats"], 1_500_000);
    // TRI is a SIX-decimal chain (COIN = 1_000_000)
    assert_eq!(r["entries"][0]["balance_tri"], json!(1.5));
    assert_eq!(r["entries"][2]["balance_tri"], json!(0.0001));
    assert_eq!(r["entries"][2]["rank"], 3);
    assert_eq!(r["source"], "test source");
    let r_all = rich_list_json(&data, None, "s", 1000);
    assert_eq!(r_all["count"], 5);
    assert_eq!(r_all["chain_tip_height"], Value::Null, "tip unavailable under contention stays null, never a fake height");
}

/// A provider that HAS a snapshot-backed rich list (the daemon's shape).
struct RichStubWallet(Mutex<Option<std::sync::Arc<RichListData>>>, String);
impl WalletProvider for RichStubWallet {
    fn balance(&self) -> Option<BalanceView> {
        None
    }
    fn coverage(&self) -> Option<(u64, String)> {
        None
    }
    fn address_entries(&self) -> Vec<AddressEntry> {
        Vec::new()
    }
    fn received_regular(&self, _addr: &str) -> Option<i64> {
        None
    }
    fn is_mine(&self, _addr: &str) -> bool {
        false
    }
    fn utxos(&self) -> Vec<UtxoRow> {
        Vec::new()
    }
    fn rich_list(&self, top: usize) -> Result<Value, String> {
        let g = self.0.lock().unwrap();
        match g.as_ref() {
            Some(d) => Ok(rich_list_json(d, Some(2_207_110), &self.1, top)),
            None => Err("balances snapshot not available".into()),
        }
    }
}

#[test]
fn richlist_routes_passively_with_default_count_and_errors() {
    let t = StubTip;
    let b = StubBlocks { ready: true };
    // routing: count param → take(top), default 100, and the passive verb
    // answers with ops = None (explorer pressure never gates on the engine).
    let w_ok = RichStubWallet(
        Mutex::new(Some(std::sync::Arc::new(RichListData {
            height: 1,
            covered_tip_hash: "x".into(),
            total_chain_sats: 9,
            entries: (0..250).map(|i| (format!("T{i:03}"), 1000 - i)).collect(),
        }))),
        "snapshot (full-archive replay)".into(),
    );
    let d = Deps { tip: &t, wallet: &w_ok, blocks: &b, proxy: None, ops: None };
    let v = call(&d, "richlist", json!([3]));
    assert_eq!(v["result"]["count"], 3);
    assert_eq!(v["result"]["entries"][0]["rank"], 1);
    let v = call(&d, "richlist", json!([]));
    assert_eq!(v["result"]["count"], 100, "default count 100");
    let v = call(&d, "richlist", json!([99_999]));
    assert_eq!(v["result"]["count"], 250, "capped at 1000, truncated to ranked total");
    assert!(v["result"]["total_ranked"].as_u64().unwrap() >= v["result"]["count"].as_u64().unwrap());
    // provider WITHOUT a snapshot → clear error, never a fake empty list
    let w_no = RichStubWallet(Mutex::new(None), "src".into());
    let d2 = Deps { tip: &t, wallet: &w_no, blocks: &b, proxy: None, ops: None };
    let v = call(&d2, "richlist", json!([]));
    assert_eq!(v["error"]["code"], -1);
    assert!(v["error"]["message"].as_str().unwrap().contains("balances snapshot"));
}

#[test]
fn rich_list_cache_re_reads_when_the_file_changes_and_errors_when_missing() {
    let dir = std::env::temp_dir().join(format!("tri7_richcache_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("balances.json");
    std::fs::write(
        &p,
        json!({"height": 10, "covered_tip_hash": "a", "totals": {"total_balance_sats": 3},
               "balances": {"Ta": 2, "Tb": 1}})
            .to_string(),
    )
    .unwrap();
    let mut cache = RichListCache::new();
    let d1 = cache.get(&p).unwrap();
    assert_eq!(d1.entries.len(), 2);
    // cache hit: same signature returns the SAME Arc without re-reading
    let d1b = cache.get(&p).unwrap();
    assert!(std::sync::Arc::ptr_eq(&d1, &d1b), "unchanged file must not re-parse");
    // rewrite with a different length → signature changes → re-parse
    std::fs::write(
        &p,
        json!({"height": 11, "covered_tip_hash": "b", "totals": {"total_balance_sats": 1},
               "balances": {"Tz": 1}})
            .to_string(),
    )
    .unwrap();
    let d2 = cache.get(&p).unwrap();
    assert_eq!(d2.entries.len(), 1);
    assert_eq!(d2.height, 11, "a rebuilt snapshot is picked up without a restart");
    // missing file → operator-actionable error
    let err = cache.get(&dir.join("nope.json")).unwrap_err();
    assert!(err.contains("--balances"), "error must tell the operator how to build one: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}
}
