//! Block explorer (phase 6 addition) — chain search from inside the wallet.
//!
//! Data source: the LOCAL tri7d RPC only (127.0.0.1:19112 native surface,
//! fallbacks configurable — every tri7 install ships the full chain, so the
//! explorer is fully local; it NEVER phones home).
//!
//! Search box accepts: block height (number), block hash or txid (64 hex —
//! resolved as block first, then tx), or a TRI address (base58 'T…').
//!
//! Address history is NOT a native RPC method (see explorer RPC gaps): it is
//! reconstructed by a bounded BACKWARD block scan (newest → older, page size
//! `scan_budget_blocks`), cached, with partial results + a spinner in the
//! view. The rich list comes from the wallet-side balances.json snapshot
//! (full-archive replay @h2201090) because the daemon has no rich-list
//! method — the coverage height is shown honestly.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use super::rpc_client;

// ===========================================================================
// search classification
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKind {
    Height(u64),
    BlockHash(String),
    Txid(String),
    Address(String),
}

pub fn classify_search(input: &str) -> Result<SearchKind, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("empty search".into());
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let h: u64 = t
            .parse()
            .map_err(|_| format!("height {} out of range", t))?;
        return Ok(SearchKind::Height(h));
    }
    if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(SearchKind::BlockHash(t.to_lowercase()));
    }
    if t.len() >= 25 && t.len() <= 34 && t.starts_with('T') {
        // validate shape locally; full checksum validation happens via RPC
        if let Ok(payload) = super::state::validate_address(t) {
            return Ok(SearchKind::Address(crate::address::payload_to_address(
                &payload,
            )));
        }
    }
    Err(format!(
        "\"{}\" is not a block height, block hash, txid, or TRI address",
        t
    ))
}

// ===========================================================================
// parsed views (tri7d JSON shapes, captured live 2026-11-02)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct VinView {
    pub prev_txid: Option<String>, // None = coinbase
    pub prev_n: Option<u32>,
    pub script_sig_hex: String,
    pub coinbase: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VoutView {
    pub n: u32,
    pub value_sats: i64,
    pub script_type: String,
    pub script_hex: String,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TxView {
    pub txid: String,
    pub version: i32,
    pub time: u32,
    pub tx_time: u32,
    pub blockhash: Option<String>,
    pub height: Option<u64>,
    pub confirmations: Option<u64>,
    pub size: usize,
    pub vin: Vec<VinView>,
    pub vout: Vec<VoutView>,
}

#[derive(Debug, Clone)]
pub struct BlockView {
    pub height: u64,
    pub hash: String,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub merkleroot: String,
    pub time: u32,
    pub bits: String,
    pub difficulty: f64,
    pub size: usize,
    pub nonce: u32,
    pub version: i32,
    pub entropybit: Option<u8>,
    pub pos: bool,
    pub stake_modifier_generated: bool,
    pub modifier: Option<String>,
    pub modifierchecksum: Option<String>,
    pub proofhash: Option<String>,
    pub confirmations: Option<u64>,
    pub txids: Vec<String>,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}
fn u(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}
fn i64_of(v: &Value, key: &str) -> Option<i64> {
    // value_sats is exact; value (float TRI) is the fallback
    if let Some(x) = v.get(key).and_then(|x| x.as_i64()) {
        return Some(x);
    }
    v.get(key).and_then(|x| x.as_f64()).map(|f| {
        // 6-decimal fixed point round trip (TRI = 1e6 sats)
        let s = (f * 1_000_000.0).round() as i64;
        s
    })
}

pub fn parse_vout(v: &Value) -> VoutView {
    let spk = v.get("scriptPubKey").cloned().unwrap_or(Value::Null);
    let script_hex = s(&spk, "script_hex")
        .or_else(|| s(&spk, "hex"))
        .unwrap_or_default();
    let mut addresses = Vec::new();
    if let Some(arr) = spk.get("addresses").and_then(|a| a.as_array()) {
        for a in arr {
            if let Some(x) = a.as_str() {
                addresses.push(x.to_string());
            }
        }
    }
    VoutView {
        n: v.get("n").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        value_sats: i64_of(v, "value_sats")
            .or_else(|| i64_of(v, "value"))
            .unwrap_or(0),
        script_type: s(&spk, "type").unwrap_or_else(|| "unknown".into()),
        script_hex,
        addresses,
    }
}

/// Attribute a vout to an address: the RPC `addresses` array when present,
/// else decode the script locally (bare-P2PK coinstake outputs carry no
/// address in the RPC but their script pays a pubkey we can hash).
pub fn addr_of_vout(vout: &VoutView) -> Option<String> {
    if let Some(a) = vout.addresses.first() {
        return Some(a.clone());
    }
    let script = super::state::hex_decode(&vout.script_hex).ok()?;
    match crate::address::script_to_dest(&script)? {
        crate::address::ScriptDest::PubkeyHash(h) => {
            let mut payload = [0u8; 21];
            payload[0] = crate::address::PUBKEY_ADDRESS;
            payload[1..].copy_from_slice(&h);
            Some(crate::address::payload_to_address(&payload))
        }
        crate::address::ScriptDest::Pubkey33(pk) => {
            Some(crate::wallet::pubkey_to_address(&pk.to_vec()))
        }
        crate::address::ScriptDest::Pubkey65(pk) => {
            Some(crate::wallet::pubkey_to_address(&pk.to_vec()))
        }
        crate::address::ScriptDest::ScriptHash(_) => None,
    }
}

pub fn parse_tx(v: &Value) -> Result<TxView, String> {
    let obj_v = v.clone();
    let obj = v.as_object().ok_or("tx JSON not an object")?;
    let mut vin = Vec::new();
    for vi in obj.get("vin").and_then(|x| x.as_array()).unwrap_or(&vec![]) {
        let cb = s(vi, "coinbase");
        let prev = s(vi, "txid");
        vin.push(VinView {
            prev_txid: prev,
            prev_n: vi.get("vout").and_then(|x| x.as_u64()).map(|x| x as u32),
            script_sig_hex: vi
                .get("scriptSig")
                .and_then(|x| x.get("hex"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            coinbase: cb,
        });
    }
    let mut vout = Vec::new();
    for vo in obj
        .get("vout")
        .and_then(|x| x.as_array())
        .unwrap_or(&vec![])
    {
        vout.push(parse_vout(vo));
    }
    let hex = s(&obj_v, "hex").unwrap_or_default();
    Ok(TxView {
        txid: s(&obj_v, "txid").ok_or("tx JSON missing txid")?,
        version: obj_v.get("version").and_then(|x| x.as_i64()).unwrap_or(1) as i32,
        time: u(&obj_v, "time").unwrap_or(0) as u32,
        tx_time: u(&obj_v, "tx_time").unwrap_or_else(|| u(&obj_v, "time").unwrap_or(0)) as u32,
        blockhash: s(&obj_v, "blockhash"),
        height: u(&obj_v, "height"),
        confirmations: u(&obj_v, "confirmations"),
        size: hex.len() / 2,
        vin,
        vout,
    })
}

pub fn parse_block(v: &Value) -> Result<BlockView, String> {
    let obj_v = v.clone();
    let obj = v.as_object().ok_or("block JSON not an object")?;
    let flags = s(&obj_v, "flags").unwrap_or_default();
    Ok(BlockView {
        height: u(&obj_v, "height").ok_or("block JSON missing height")?,
        hash: s(&obj_v, "hash").ok_or("block JSON missing hash")?,
        prev: s(&obj_v, "previousblockhash"),
        next: s(&obj_v, "nextblockhash").filter(|x| !x.is_empty() && x != "null"),
        merkleroot: s(&obj_v, "merkleroot").unwrap_or_default(),
        time: u(&obj_v, "time").unwrap_or(0) as u32,
        bits: s(&obj_v, "bits").unwrap_or_default(),
        difficulty: obj_v
            .get("difficulty")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0),
        size: u(&obj_v, "size").unwrap_or(0) as usize,
        nonce: u(&obj_v, "nonce").unwrap_or(0) as u32,
        version: obj_v.get("version").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
        entropybit: obj_v
            .get("entropybit")
            .and_then(|x| x.as_u64())
            .map(|x| x as u8),
        pos: flags.contains("proof-of-stake"),
        stake_modifier_generated: flags.contains("stake-modifier"),
        modifier: s(&obj_v, "modifier"),
        modifierchecksum: s(&obj_v, "modifierchecksum"),
        proofhash: s(&obj_v, "proofhash")
            .filter(|x| x != "0000000000000000000000000000000000000000000000000000000000000000"),
        confirmations: u(&obj_v, "confirmations"),
        txids: obj
            .get("tx")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|y| y.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The stake reward of a coinstake tx: sum(vout[1..]) − sum(vin values).
/// Coinstake marker: vout[0] is the empty value-0 marker (nulldata).
pub fn tx_is_coinstake(tx: &TxView) -> bool {
    tx.vout
        .first()
        .map(|v| v.value_sats == 0 && (v.script_hex.is_empty() || v.script_type == "nulldata"))
        .unwrap_or(false)
}

pub fn tx_is_coinbase(tx: &TxView) -> bool {
    tx.vin
        .first()
        .map(|v| v.prev_txid.is_none())
        .unwrap_or(false)
}

/// Fee/credit from the tx + its prevouts. Returns (fee_sats or stake credit).
/// Coinbase → None. Regular → Some(negative?) fee = vin − vout.
/// Coinstake → Some(credit) = (vout[1..]) − vin (negative = reward paid).
pub fn compute_fee(
    tx: &TxView,
    fetch: &dyn Fn(&str, &[Value]) -> Result<Value, String>,
    cache: &mut HashMap<String, TxView>,
    vin_budget: usize,
) -> Option<i64> {
    if tx_is_coinbase(tx) {
        return None;
    }
    let mut vin_total = 0i64;
    let mut resolved = 0usize;
    for vi in &tx.vin {
        let Some(prev_txid) = &vi.prev_txid else {
            continue;
        };
        if resolved >= vin_budget {
            return None; // refuse partial truth
        }
        let prev = match cache.get(prev_txid) {
            Some(t) => t.clone(),
            None => {
                let v = fetch("getrawtransaction", &[json!(prev_txid), json!(1)]).ok()?;
                let t = parse_tx(&v).ok()?;
                cache.insert(prev_txid.clone(), t.clone());
                t
            }
        };
        let value = prev.vout.get(vi.prev_n.unwrap_or(0) as usize)?.value_sats;
        vin_total = vin_total.checked_add(value)?;
        resolved += 1;
    }
    let vout_total: i64 = if tx_is_coinstake(tx) {
        tx.vout.iter().skip(1).map(|v| v.value_sats).sum()
    } else {
        tx.vout.iter().map(|v| v.value_sats).sum()
    };
    Some(vin_total.checked_sub(vout_total)?)
}

// ===========================================================================
// rich list (balances.json snapshot — the daemon has no rich-list method)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct RichListView {
    pub height: u64,
    pub entries: Vec<(String, i64)>, // (address, sats) sorted desc
    pub total_chain_sats: i64,
    pub source: String,
}

/// balances.json (phase-1e full-archive replay): {height, balances:{addr:sats}}.
pub fn parse_rich_list(v: &Value, top: usize) -> Result<RichListView, String> {
    let height = u(v, "height").ok_or("balances.json missing height")?;
    let map = v
        .get("balances")
        .and_then(|x| x.as_object())
        .ok_or("balances.json missing balances map")?;
    let mut entries: Vec<(String, i64)> = map
        .iter()
        .filter_map(|(k, val)| val.as_i64().map(|s| (k.clone(), s)))
        .filter(|(_, s)| *s > 0)
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    entries.truncate(top);
    let total = v
        .get("totals")
        .and_then(|t| t.get("total_balance_sats"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    Ok(RichListView {
        height,
        entries,
        total_chain_sats: total,
        source: format!(
            "balances.json snapshot h{} (full-archive replay) — the daemon RPC has no rich-list method",
            height
        ),
    })
}

// ===========================================================================
// Explorer state
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerView {
    Landing,
    Address,
    Tx,
    Block,
    Stats,
    Rich,
}

#[derive(Debug, Clone)]
pub struct AddrHit {
    pub height: u64,
    pub txid: String,
    pub received: bool, // true = output paying this address; false = input spending from it
    pub sats: i64,
}

#[derive(Debug, Clone, Default)]
pub struct AddrView {
    pub address: String,
    pub valid: bool,
    pub ismine: Option<bool>,
    pub kind: Option<String>,
    pub received_tri: Option<f64>, // getreceivedbyaddress(addr, 0) — regular P2PKH only
    pub received_snapshot_sats: Option<i64>, // balances.json (h2201090 coverage)
    pub balance_snapshot_sats: Option<i64>, // balances.json live balance at snapshot
    pub history: Vec<AddrHit>,
    pub newest_scanned: Option<u64>,
    pub oldest_scanned: Option<u64>,
    pub exhausted: bool, // scan reached height 1
    pub blocks_scanned: usize,
}

pub struct ExplorerState {
    pub input: String,
    pub view: ExplorerView,
    pub endpoints: Vec<String>,
    pub endpoint: Option<usize>,
    pub blocks: HashMap<u64, BlockView>,
    pub block_height_of_hash: HashMap<String, u64>,
    pub txs: HashMap<String, TxView>,
    pub fees: HashMap<String, i64>,
    pub addr: HashMap<String, AddrView>,
    pub stats: Option<Value>,
    pub latest: Vec<BlockView>,
    pub rich: Option<RichListView>,
    pub scanning: bool,
    pub last_error: Option<String>,
    pub scan_budget_blocks: usize,
    /// the tx/block the user navigated to (view target — map iteration order
    /// is meaningless, so this is tracked explicitly)
    pub open_tx: Option<String>,
    pub open_block: Option<u64>,
    /// the address view currently displayed ("scan more" targets THIS, never
    /// an arbitrary map entry)
    pub open_addr: Option<String>,
}

pub const DEFAULT_ENDPOINTS: &[&str] = &["127.0.0.1:19112", "127.0.0.1:19114"];

impl ExplorerState {
    pub fn new(endpoints: Vec<String>) -> Self {
        ExplorerState {
            input: String::new(),
            view: ExplorerView::Landing,
            endpoints: if endpoints.is_empty() {
                DEFAULT_ENDPOINTS.iter().map(|s| s.to_string()).collect()
            } else {
                endpoints
            },
            endpoint: None,
            blocks: HashMap::new(),
            block_height_of_hash: HashMap::new(),
            txs: HashMap::new(),
            fees: HashMap::new(),
            addr: HashMap::new(),
            stats: None,
            latest: Vec::new(),
            rich: None,
            scanning: false,
            last_error: None,
            scan_budget_blocks: 300,
            open_tx: None,
            open_block: None,
            open_addr: None,
        }
    }

    fn fetch(&self, method: &str, params: &[Value]) -> Result<Value, String> {
        let ep = self
            .endpoint
            .and_then(|i| self.endpoints.get(i))
            .ok_or_else(|| "explorer: no RPC endpoint probed yet".to_string())?;
        rpc_client::rpc_call(ep, method, params)
    }

    /// Probe each endpoint until one answers getblockcount (19112 native is
    /// first: every tri7 install serves the full chain there).
    pub fn probe(&mut self) -> bool {
        for (i, ep) in self.endpoints.iter().enumerate() {
            if rpc_client::rpc_call(ep, "getblockcount", &[]).is_ok() {
                self.endpoint = Some(i);
                return true;
            }
        }
        false
    }

    fn require_endpoint(&mut self) -> Result<(), String> {
        if self.endpoint.is_none() && !self.probe() {
            return Err(format!(
                "no explorer RPC reachable (tried {:?})",
                self.endpoints
            ));
        }
        Ok(())
    }

    pub fn fetch_block_by_height(&mut self, h: u64) -> Result<BlockView, String> {
        if let Some(b) = self.blocks.get(&h) {
            return Ok(b.clone());
        }
        self.require_endpoint()?;
        let hash = self
            .fetch("getblockhash", &[json!(h)])?
            .as_str()
            .ok_or("getblockhash returned non-string")?
            .to_string();
        self.fetch_block_by_hash(&hash)
    }

    pub fn fetch_block_by_hash(&mut self, hash: &str) -> Result<BlockView, String> {
        if let Some(&h) = self.block_height_of_hash.get(hash) {
            if let Some(b) = self.blocks.get(&h) {
                return Ok(b.clone());
            }
        }
        self.require_endpoint()?;
        let v = self.fetch("getblock", &[json!(hash)])?;
        let b = parse_block(&v)?;
        self.block_height_of_hash.insert(b.hash.clone(), b.height);
        if let Some(prev) = &b.prev {
            self.block_height_of_hash
                .entry(prev.clone())
                .or_insert(b.height.saturating_sub(1));
        }
        if let Some(next) = &b.next {
            self.block_height_of_hash
                .entry(next.clone())
                .or_insert(b.height + 1);
        }
        self.blocks.insert(b.height, b.clone());
        Ok(b)
    }

    pub fn fetch_tx(&mut self, txid: &str) -> Result<TxView, String> {
        if let Some(t) = self.txs.get(txid) {
            return Ok(t.clone());
        }
        self.require_endpoint()?;
        let v = self.fetch("getrawtransaction", &[json!(txid), json!(1)])?;
        let t = parse_tx(&v)?;
        self.txs.insert(txid.to_string(), t.clone());
        Ok(t)
    }

    /// fee display helper: coinbase None, coinstake credit, regular fee
    pub fn fee_of(&mut self, txid: &str) -> Option<i64> {
        let tx = self.fetch_tx(txid).ok()?;
        let fetch = |m: &str, p: &[Value]| self.fetch(m, p);
        let mut cache = HashMap::new();
        let fee = compute_fee(&tx, &fetch, &mut cache, 16)?;
        for (k, v) in cache {
            self.txs.entry(k).or_insert(v);
        }
        self.fees.insert(txid.to_string(), fee);
        Some(fee)
    }

    /// The search box: classify + jump to the right view.
    pub fn search(&mut self, input: &str) -> Result<ExplorerView, String> {
        self.last_error = None;
        let kind = classify_search(input)?;
        let view = match kind {
            SearchKind::Height(h) => {
                let b = self.fetch_block_by_height(h)?;
                self.open_block = Some(b.height);
                ExplorerView::Block
            }
            SearchKind::BlockHash(h) => {
                match self.fetch_block_by_hash(&h) {
                    Ok(b) => {
                        self.open_block = Some(b.height);
                        ExplorerView::Block
                    }
                    Err(_) => {
                        // not a block — try as a txid before giving up
                        self.fetch_tx(&h)?;
                        self.open_tx = Some(h);
                        ExplorerView::Tx
                    }
                }
            }
            SearchKind::Txid(t) => {
                self.fetch_tx(&t)?;
                self.open_tx = Some(t);
                ExplorerView::Tx
            }
            SearchKind::Address(a) => {
                self.open_address_action(&a)?;
                ExplorerView::Address
            }
        };
        self.view = view.clone();
        Ok(view)
    }

    pub fn open_address_action(&mut self, address: &str) -> Result<(), String> {
        self.require_endpoint()?;
        let va = self.fetch("validateaddress", &[json!(address)])?;
        let mut av = AddrView {
            address: address.to_string(),
            valid: va.get("isvalid").and_then(|x| x.as_bool()).unwrap_or(false),
            ismine: va.get("ismine").and_then(|x| x.as_bool()),
            kind: va
                .get("kind")
                .and_then(|x| x.as_str())
                .map(|x| x.to_string()),
            ..Default::default()
        };
        if let Ok(r) = self.fetch("getreceivedbyaddress", &[json!(address), json!(0)]) {
            av.received_tri = r.as_f64();
        }
        self.addr.insert(address.to_string(), av);
        self.open_addr = Some(address.to_string());
        self.view = ExplorerView::Address;
        Ok(())
    }

    /// Attach the balances.json snapshot figures to an address view.
    pub fn attach_snapshot_for_address(
        &mut self,
        path: &Path,
        address: &str,
    ) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let bal = v
            .get("balances")
            .and_then(|m| m.get(address))
            .and_then(|x| x.as_i64());
        let recv = v
            .get("received_regular")
            .and_then(|m| m.get(address))
            .and_then(|x| x.as_i64());
        if let Some(av) = self.addr.get_mut(address) {
            av.balance_snapshot_sats = bal;
            av.received_snapshot_sats = recv;
        }
        Ok(())
    }

    /// One backward-scan page for the address view (history reconstruction).
    /// Returns the number of hits added. Newest → oldest, stopping at height 1
    /// or after `budget` blocks. `self.scanning` drives the view spinner.
    pub fn scan_address_more(
        &mut self,
        address: &str,
        budget_blocks: usize,
    ) -> Result<usize, String> {
        self.require_endpoint()?;
        let tip = self
            .fetch("getblockcount", &[])?
            .as_u64()
            .ok_or("getblockcount returned non-number")?;
        // resume BELOW the oldest block of the previous page (no overlap)
        let start = match self.addr.get(address).and_then(|a| a.oldest_scanned) {
            None => tip,
            Some(o) if o > 1 => o - 1,
            Some(_) => {
                if let Some(av) = self.addr.get_mut(address) {
                    av.exhausted = true;
                }
                return Ok(0);
            }
        };
        if start == 0 {
            if let Some(av) = self.addr.get_mut(address) {
                av.exhausted = true;
            }
            return Ok(0);
        }
        self.scanning = true;
        let stop = start.saturating_sub(budget_blocks as u64 - 1).max(1);
        let mut hits: Vec<AddrHit> = Vec::new();
        let mut blocks_done = 0usize;
        let mut h = start;
        while h >= stop {
            let block = self.fetch_block_by_height(h)?;
            for txid in &block.txids {
                // tolerate per-tx fetch failures — a scan must survive RPC
                // gaps instead of dying mid-page
                let Ok(tx) = self.fetch_tx(txid) else {
                    continue;
                };
                // outputs paying the address = Received
                for vout in &tx.vout {
                    if addr_of_vout(vout).as_deref() == Some(address) && vout.value_sats > 0 {
                        hits.push(AddrHit {
                            height: h,
                            txid: txid.clone(),
                            received: true,
                            sats: vout.value_sats,
                        });
                    }
                }
                // inputs spending the address's previous outputs = Sent
                for vin in &tx.vin {
                    let Some(prev_txid) = &vin.prev_txid else {
                        continue;
                    };
                    // only resolve prevouts we already have cached (cheap);
                    // full unspent-graph reconstruction is an RPC gap
                    if let Some(prev) = self.txs.get(prev_txid) {
                        if let Some(vo) = prev.vout.get(vin.prev_n.unwrap_or(0) as usize) {
                            if addr_of_vout(vo).as_deref() == Some(address) && vo.value_sats > 0 {
                                hits.push(AddrHit {
                                    height: h,
                                    txid: txid.clone(),
                                    received: false,
                                    sats: vo.value_sats,
                                });
                            }
                        }
                    }
                }
            }
            blocks_done += 1;
            if h == 1 {
                break;
            }
            h -= 1;
        }
        let exhausted = stop == 1;
        let added = hits.len();
        // entry must exist even when the caller never ran the address action
        // first (defensive: scan-then-view)
        let av = self.addr.entry(address.to_string()).or_default();
        av.address = address.to_string();
        av.history.extend(hits);
        av.blocks_scanned += blocks_done;
        av.newest_scanned = Some(av.newest_scanned.map(|n| n.max(start)).unwrap_or(start));
        av.oldest_scanned = Some(av.oldest_scanned.map(|o| o.min(stop)).unwrap_or(stop));
        av.exhausted = exhausted || av.exhausted;
        self.scanning = false;
        Ok(added)
    }

    /// Stats panel (money supply, tip, difficulty, connections) + latest
    /// blocks list (newest first, `n` entries, PoW/PoS typed).
    pub fn refresh_stats(&mut self, latest_n: usize) -> Result<(), String> {
        self.require_endpoint()?;
        self.stats = Some(self.fetch("getinfo", &[])?);
        self.latest.clear();
        let tip = self
            .stats
            .as_ref()
            .and_then(|s| s.get("blocks"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let mut h = tip;
        while self.latest.len() < latest_n && h >= 1 {
            let b = self.fetch_block_by_height(h)?;
            self.latest.push(b.clone());
            if h == 1 {
                break;
            }
            h -= 1;
        }
        self.view = ExplorerView::Stats;
        Ok(())
    }

    pub fn load_rich_list_action(&mut self, path: &Path, top: usize) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        self.rich = Some(parse_rich_list(&v, top)?);
        self.view = ExplorerView::Rich;
        Ok(())
    }
}

// ===========================================================================
// tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_search_kinds() {
        assert_eq!(classify_search("0"), Ok(SearchKind::Height(0)));
        assert_eq!(
            classify_search("2204067"),
            Ok(SearchKind::Height(2_204_067))
        );
        let hex64 = "0f67543090749ba77cebc918241e1e25553d10bd4ea7da395df8aabbada14a3f";
        assert_eq!(
            classify_search(hex64),
            Ok(SearchKind::BlockHash(hex64.into()))
        );
        assert_eq!(
            classify_search("TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7"),
            Ok(SearchKind::Address(
                "TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7".into()
            ))
        );
        assert!(classify_search("").is_err());
        assert!(classify_search("zzz").is_err());
        assert!(classify_search("TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md8").is_err()); // checksum
        assert!(classify_search("0f6754").is_err()); // short hex → not a hash
        assert!(classify_search("123abc").is_err()); // mixed
    }

    #[test]
    fn parse_block_from_live_rpc_shape() {
        // captured verbatim from tri7d getblock @tip 2204067 (2026-11-02)
        let v: Value = serde_json::from_str(
            r#"{"bits":"2000ffff","confirmations":1,"difficulty":5.960464477539063e-08,
                "entropybit":1,"flags":"proof-of-stake stake-modifier",
                "hash":"0f67543090749ba77cebc918241e1e25553d10bd4ea7da395df8aabbada14a3f",
                "height":2204067,
                "merkleroot":"6148d10e90cd4f3f05cf3c1267f808c56d954b3bde84315d5281a82326298669",
                "modifier":"0000000000000003","modifierchecksum":"248db65a",
                "nextblockhash":null,"nonce":0,"previousblockhash":"e493519700000000",
                "proofhash":"00a0ad5bcee5a7cb609f5d031c4975074fe2a116723ec4d083451e633119407f",
                "size":1377,"time":1793642570,"tx":["3acd130de049a109c7babd3fdb6152a214add8b11f9679832b0c34f185c330b4",
                "2638cb595b6d5ea09eccdfda7a39f7129bb098542ea60fb6b13af6124b9bec60"],"version":2}"#,
        )
        .unwrap();
        let b = parse_block(&v).unwrap();
        assert_eq!(b.height, 2_204_067);
        assert!(b.pos, "flags carry proof-of-stake");
        assert!(b.stake_modifier_generated);
        assert_eq!(b.modifier.as_deref(), Some("0000000000000003"));
        assert_eq!(b.modifierchecksum.as_deref(), Some("248db65a"));
        assert!(b.proofhash.is_some());
        assert_eq!(b.bits, "2000ffff");
        assert_eq!(b.txids.len(), 2);
        assert!(b.next.is_none());
    }

    #[test]
    fn parse_tx_from_live_rpc_shape_and_attribute_addresses() {
        // captured verbatim from tri7d getrawtransaction <txid> 1 (a coinstake)
        let v: Value = serde_json::from_str(
            r#"{"txid":"2638cb595b6d5ea09eccdfda7a39f7129bb098542ea60fb6b13af6124b9bec60",
                "version":1,"time":1793642570,"tx_time":1793642570,"locktime":0,
                "blockhash":"0f67543090749ba77cebc918241e1e25553d10bd4ea7da395df8aabbada14a3f",
                "confirmations":1,"height":2204067,"hex":"0100",
                "vin":[{"txid":"fba90ab383a7a5608c7f6d5785fff75053b41ffdab694a4da61ad9048a339f25",
                        "vout":1,"sequence":4294967295,
                        "scriptSig":{"hex":"47304402207f"}}],
                "vout":[
                  {"n":0,"value":0.0,"value_sats":0,
                   "scriptPubKey":{"asm":"","hex":"","script_hex":"","type":"nulldata","reqSigs":1,"addresses":null}},
                  {"n":1,"value":12.499483,"value_sats":12499483,
                   "scriptPubKey":{"asm":"21 02.. ac","hex":"2102","script_hex":"2102","type":"pubkey","reqSigs":1,"addresses":null}}]}"#,
        )
        .unwrap();
        let t = parse_tx(&v).unwrap();
        assert_eq!(
            t.txid,
            "2638cb595b6d5ea09eccdfda7a39f7129bb098542ea60fb6b13af6124b9bec60"
        );
        assert_eq!(t.height, Some(2_204_067));
        assert_eq!(t.size, 2);
        assert!(tx_is_coinstake(&t), "vout0 nulldata+0 marks the coinstake");
        assert!(!tx_is_coinbase(&t));
        // bare-P2PK output attributed LOCALLY via the script (RPC addresses null)
        let (_, pk) = crate::spend::synthetic_key("explorer/pk");
        let mut t2 = t.clone();
        t2.vout[1].script_hex = crate::spend::script_pubkey_p2pk(&pk)
            .unwrap()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        t2.vout[1].addresses = vec![];
        assert_eq!(
            addr_of_vout(&t2.vout[1]).as_deref(),
            Some(crate::wallet::pubkey_to_address(&pk).as_str())
        );
        assert_eq!(addr_of_vout(&t.vout[0]), None); // nulldata
                                                    // coinstake vout value carries value_sats exactly
        assert_eq!(t.vout[1].value_sats, 12_499_483);
    }

    #[test]
    fn fee_and_stake_credit_from_prevouts() {
        let (_, pk_a) = crate::spend::synthetic_key("fee/a");
        let (sec_b, pk_b) = crate::spend::synthetic_key("fee/b");
        let _ = sec_b;
        // prevout tx: pays 2_000_000 to B
        let prev = TxView {
            txid: "prev".into(),
            version: 1,
            time: 0,
            tx_time: 0,
            blockhash: None,
            height: None,
            confirmations: None,
            size: 0,
            vin: vec![],
            vout: vec![VoutView {
                n: 0,
                value_sats: 2_000_000,
                script_type: "pubkeyhash".into(),
                script_hex: String::new(),
                addresses: vec![],
            }],
        };
        let spend_tx = TxView {
            txid: "spend".into(),
            version: 1,
            time: 0,
            tx_time: 0,
            blockhash: None,
            height: None,
            confirmations: None,
            size: 0,
            vin: vec![VinView {
                prev_txid: Some("prev".into()),
                prev_n: Some(0),
                script_sig_hex: String::new(),
                coinbase: None,
            }],
            vout: vec![
                VoutView {
                    n: 0,
                    value_sats: 1_500_000,
                    script_type: "pubkeyhash".into(),
                    script_hex: String::new(),
                    addresses: vec![],
                },
                VoutView {
                    n: 1,
                    value_sats: 400_000,
                    script_type: "pubkeyhash".into(),
                    script_hex: String::new(),
                    addresses: vec![],
                },
            ],
        };
        let mut cache = HashMap::new();
        cache.insert("prev".into(), prev);
        let fetch_fail = |_m: &str, _p: &[Value]| -> Result<Value, String> { Err("no io".into()) };
        let fee = compute_fee(&spend_tx, &fetch_fail, &mut cache, 16).unwrap();
        assert_eq!(fee, 100_000, "fee = vin 2M − (1.5M + 0.4M)");
        // coinstake: vout0 marker skipped, credit = vin − vout[1..]
        let mut stake = spend_tx.clone();
        stake.vout.insert(
            0,
            VoutView {
                n: 0,
                value_sats: 0,
                script_type: "nulldata".into(),
                script_hex: String::new(),
                addresses: vec![],
            },
        );
        let credit = compute_fee(&stake, &fetch_fail, &mut cache, 16).unwrap();
        assert_eq!(credit, 100_000);
        // coinbase → None (fee undefined)
        let mut cb = spend_tx.clone();
        cb.vin[0] = VinView {
            prev_txid: None,
            prev_n: None,
            script_sig_hex: String::new(),
            coinbase: Some("04..".into()),
        };
        assert_eq!(compute_fee(&cb, &fetch_fail, &mut cache, 16), None);
        // budget exhausted → refuse partial truth
        let mut big = spend_tx.clone();
        big.vin = (0..20)
            .map(|i| VinView {
                prev_txid: Some(format!("p{}", i)),
                prev_n: Some(0),
                script_sig_hex: String::new(),
                coinbase: None,
            })
            .collect();
        assert_eq!(compute_fee(&big, &fetch_fail, &mut cache, 16), None);
        let _ = pk_a;
        let _ = pk_b;
    }

    #[test]
    fn rich_list_parses_balances_snapshot_and_sorts() {
        let v: Value = serde_json::from_str(
            r#"{"height": 2201090, "covered_tip_hash": "f903",
                "totals": {"total_balance_sats": 222507326452},
                "balances": {"T9yWe13uxKMJkG1YihqrYv3qVt7HNc9WyY": 33665763,
                             "TAA": 100, "TBB": 999999999},
                "received_regular": {}}"#,
        )
        .unwrap();
        let rl = parse_rich_list(&v, 2).unwrap();
        assert_eq!(rl.height, 2_201_090);
        assert_eq!(rl.entries.len(), 2);
        assert_eq!(rl.entries[0].1, 999_999_999, "sorted desc");
        assert_eq!(rl.total_chain_sats, 222_507_326_452);
        assert!(rl.source.contains("h2201090"));
        assert!(rl.source.contains("no rich-list method"));
    }

    #[test]
    fn backward_scan_pages_and_caches_with_injected_fetcher() {
        // synthetic explorer over a canned endpoint: 5 blocks, the address
        // receives in block tip and tip-2, spends in tip-3
        let dir = std::env::temp_dir().join(format!("tri7gui_explorer_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (_, pk) = crate::spend::synthetic_key("scan/target");
        let addr = crate::wallet::pubkey_to_address(&pk);
        let script = crate::spend::script_pubkey_p2pkh(&pk);
        let script_hex: String = script.iter().map(|b| format!("{:02x}", b)).collect();
        let other_hex = "2102ff";

        let mut served = std::collections::HashMap::new();
        for h in 1u64..=5u64 {
            let recv_out = format!(
                r#"{{"n":0,"value":1.0,"value_sats":1000000,"scriptPubKey":{{"type":"pubkeyhash","script_hex":"{}","addresses":["{}"]}}}}"#,
                script_hex, addr
            );
            let other_out = format!(
                r#"{{"n":0,"value":2.0,"value_sats":2000000,"scriptPubKey":{{"type":"pubkeyhash","script_hex":"{}","addresses":["TOTHER"]}}}}"#,
                other_hex
            );
            let (vout_a, vin_a, txs) = match h {
                5 => (
                    format!("[{},{}]", recv_out, other_out),
                    "[]".to_string(),
                    vec!["t5".to_string()],
                ),
                4 => (
                    format!("[{}]", other_out),
                    "[]".to_string(),
                    vec!["t4".to_string()],
                ),
                3 => (
                    format!("[{}]", other_out),
                    format!(
                        r#"[{{"txid":"t5","vout":0,"sequence":4294967295,"scriptSig":{{"hex":""}}}}]"#
                    ),
                    vec!["t3".to_string()],
                ),
                2 => (
                    format!("[{},{}]", recv_out, other_out),
                    "[]".to_string(),
                    vec!["t2".to_string()],
                ),
                _ => (
                    format!("[{}]", other_out),
                    "[]".to_string(),
                    vec![format!("t{}", h)],
                ),
            };
            let tx_json = match h {
                5 => Some(format!(
                    r#"{{"txid":"t5","version":1,"time":179364000{h},"tx_time":179364000{h},"height":{h},"confirmations":{c},"blockhash":"bh{h}","hex":"","vin":[],"vout":{vout_a}}}"#,
                    h = h,
                    c = 6 - h,
                    vout_a = vout_a
                )),
                3 => Some(format!(
                    r#"{{"txid":"t3","version":1,"time":179364000{h},"tx_time":179364000{h},"height":{h},"confirmations":{c},"blockhash":"bh{h}","hex":"","vin":{vin},"vout":{vout_a}}}"#,
                    h = h,
                    c = 6 - h,
                    vin = vin_a,
                    vout_a = vout_a
                )),
                2 => Some(format!(
                    r#"{{"txid":"t2","version":1,"time":179364000{h},"tx_time":179364000{h},"height":{h},"confirmations":{c},"blockhash":"bh{h}","hex":"","vin":[],"vout":{vout_a}}}"#,
                    h = h,
                    c = 6 - h,
                    vout_a = vout_a
                )),
                _ => None,
            };
            let block = format!(
                r#"{{"hash":"bh{h}","height":{h},"bits":"2000ffff","difficulty":1.0,"size":100,
                    "nonce":0,"version":2,"entropybit":0,"flags":"{flags}",
                    "merkleroot":"00","time":179364000{h},
                    "previousblockhash":"{prev}","nextblockhash":"{next}",
                    "modifier":"0000000000000000","modifierchecksum":"00000000",
                    "tx":{txs}}}"#,
                h = h,
                prev = if h > 1 {
                    format!("bh{}", h - 1)
                } else {
                    "00".to_string()
                },
                next = if h < 5 {
                    format!("bh{}", h + 1)
                } else {
                    "null".to_string()
                },
                flags = if h == 5 {
                    "proof-of-stake stake-modifier"
                } else {
                    ""
                },
                txs = serde_json::to_string(&txs).unwrap(),
            );
            served.insert(
                format!("getblockhash:{}", h),
                serde_json::json!(format!("bh{}", h)),
            );
            served.insert(
                format!("getblock:{}", h),
                serde_json::from_str::<Value>(&block).unwrap(),
            );
            if let Some(tj) = tx_json {
                served.insert(
                    format!("getrawtransaction:1:t{}", h),
                    serde_json::from_str::<Value>(&tj).unwrap(),
                );
            }
        }
        served.insert("getblockcount:".to_string(), serde_json::json!(5));

        // canned HTTP server loop
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = std::sync::Arc::new(served);
        let served2 = served.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let served = served2.clone();
                std::thread::spawn(move || {
                    use std::io::{Read as _, Write as _};
                    let mut buf = [0u8; 8192];
                    let n = sock.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                    let Ok(v) = serde_json::from_str::<Value>(body) else {
                        return;
                    };
                    let m = v["method"].as_str().unwrap_or("").to_string();
                    let p = v["params"].clone();
                    let key = match (m.as_str(), p.as_array()) {
                        ("getblockcount", _) => "getblockcount:".into(),
                        ("getblockhash", Some(a)) => {
                            format!("getblockhash:{}", a[0].as_u64().unwrap_or(0))
                        }
                        ("getblock", Some(a)) => format!(
                            "getblock:{}",
                            a[0].as_str()
                                .unwrap_or("")
                                .trim_start_matches("bh")
                                .parse::<u64>()
                                .unwrap_or(0)
                        ),
                        ("getrawtransaction", Some(a)) => format!(
                            "getrawtransaction:{}:{}",
                            a.get(1).map(|x| x.as_u64().unwrap_or(0)).unwrap_or(0),
                            a[0].as_str().unwrap_or("")
                        ),
                        _ => String::new(),
                    };
                    let resp_body = match served.get(&key) {
                        Some(r) => json!({"result": r.clone(), "error": null}).to_string(),
                        None => json!({"result": null, "error": {"code": -5, "message": format!("missing {}", key)}}).to_string(),
                    };
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = sock.write_all(http.as_bytes());
                });
            }
        });
        let mut ex = ExplorerState::new(vec![format!("127.0.0.1:{}", port)]);
        assert!(ex.probe(), "canned endpoint must answer");
        ex.scan_budget_blocks = 2; // page size 2 → 3 pages over 5 blocks
        let added = ex.scan_address_more(&addr, ex.scan_budget_blocks).unwrap();
        assert_eq!(added, 1, "first page (5,4): one receive @5");
        let av = ex.addr.get(&addr).unwrap();
        assert_eq!(av.newest_scanned, Some(5));
        assert_eq!(av.oldest_scanned, Some(4));
        assert!(!av.exhausted);
        let added = ex.scan_address_more(&addr, ex.scan_budget_blocks).unwrap();
        assert_eq!(added, 2, "second page (3,2): spend @3 + receive @2");
        let added = ex.scan_address_more(&addr, ex.scan_budget_blocks).unwrap();
        assert_eq!(added, 0, "third page (1): nothing");
        let av = ex.addr.get(&addr).unwrap();
        assert!(av.exhausted, "scan reached height 1");
        assert_eq!(av.blocks_scanned, 5);
        assert_eq!(av.history.len(), 3);
        let recv_total: i64 = av
            .history
            .iter()
            .filter(|h| h.received)
            .map(|h| h.sats)
            .sum();
        assert_eq!(recv_total, 2_000_000);
        let spend_hit = av.history.iter().find(|h| !h.received).unwrap();
        assert_eq!(spend_hit.height, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn endpoint_probe_falls_back_to_second_port() {
        // first endpoint is a closed port, second is live
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                use std::io::{Read as _, Write as _};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let body = r#"{"result":2204067,"error":null}"#;
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(http.as_bytes());
            }
        });
        let mut ex = ExplorerState::new(vec!["127.0.0.1:9".into(), format!("127.0.0.1:{}", port)]);
        assert!(ex.probe());
        assert_eq!(ex.endpoint, Some(1));
        // the listener keeps serving: probe + this follow-up call both land
        assert_eq!(
            rpc_client::rpc_call(
                ex.endpoints[ex.endpoint.unwrap()].as_str(),
                "getblockcount",
                &[]
            )
            .unwrap(),
            2_204_067
        );
    }
}
