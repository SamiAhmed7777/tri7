//! tri7 light wallet v1 — a WATCH-ONLY monitor over the public TRI explorer
//! REST surface (pipeline P6.2; spec: revenue-engine KNOWLEDGE/
//! tri-agent-light-wallet-spec.md, 2026-11-26).
//!
//! WHAT THIS IS: a light, no-node, no-chain-download watcher. You feed it an
//! explicit list of TRI addresses (derived OFFLINE by the air-gapped
//! triangles-paper-wallet tool, BIP39 m/44'/2222' coin type 2222); it polls
//! the explorer REST API and prints EXPLORER-REPORTED balances, UTXO counts
//! and recent history.
//!
//! WHAT THIS IS NOT: it cannot spend, stake, or broadcast. By design this
//! module links against none of the spend/stake/sweep builders, never
//! touches key material, and accepts no seed input (a seed in a watch tool
//! is a prompt/log leak away from total compromise). Spending and staking
//! stay on a full tri7 node: the light wallet is the *monitor*, tri7 is the
//! *actor*.
//!
//! # Trust model (every figure is "explorer-reported", never
//! "spendable-verified")
//!
//! The explorer's `chain_tip` / `index_complete` / `index_height` fields are
//! the server's own assertions, not proofs; HTTPS protects transit only.
//! The client therefore fails CLOSED rather than guessing:
//!
//! * **Health gate** — every read must report `index_complete: true` and an
//!   index height within `max_lag` blocks of the reported chain tip; the
//!   observed lag is disclosed on every line. (Live behaviour, verified
//!   2026-11-26: the addrindex reports `index_complete: true` while sitting
//!   a few blocks behind the tip during normal operation — a hard
//!   `index_height == chain_tip` gate would fail every real read, so v1
//!   bounds the lag instead and discloses it.)
//! * **Snapshot coherence** — only `/rest/address/<A>/balance` carries
//!   `chain_tip`/`index_height`/`index_complete` (utxos/txids are bare
//!   arrays, live-verified 2026-11-26), so one refresh anchors on the
//!   balance response and enforces `balance == Σ utxo satoshis` as the
//!   cross-call coherence check. A mismatch is retried, then reported as an
//!   INCONSISTENT view (heights shown), never as a number.
//! * **Divergence rule** — an address whose explorer-reported `received`
//!   exceeds the whole chain money supply while the addrindex UTXO view is
//!   empty (the TRDuu34… class: known unresolved addrindex-vs-richlist
//!   accounting divergence) is reported INCONSISTENT/UNAVAILABLE — never a
//!   bare 0 and never any second-scanner figure.
//! * **Transport hardening** — HTTPS with certificate validation (plain
//!   HTTP only for loopback test stubs), redirects never followed, connect
//!   and per-read timeouts, a hard response-size cap, `application/json`
//!   content type required, and strict typed JSON parsing — anything
//!   malformed fails closed.
//! * **No broadcast path** — the explorer REST surface is GET-only; this
//!   tool sends no other method and must not pretend to be able to spend.
//!
//! Feature-gated behind `light-wallet` (pulls ureq/rustls) so headless
//! `tri7d` builds stay TLS-free — same rationale as the gui-gated updater.

use std::fmt;
use std::io::Read;
use std::net::IpAddr;
use std::thread::sleep;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Public TRI explorer REST base (the only network endpoint this tool knows).
pub const DEFAULT_EXPLORER: &str = "https://blocks.cryptographic-triangles.org";

/// Mainnet P2PKH address version byte (mirrors keytool.rs: WIF_VERSION
/// 193 = 128 + PUBKEY_ADDRESS 65). v1 watches P2PKH addresses only.
pub const PUBKEY_ADDRESS_VERSION: u8 = 65;

/// Hard cap on any single HTTP response body (10 MiB; the largest real
/// response — a 14k-txid history — is well under 2 MiB).
pub const BODY_CAP: u64 = 10 * 1024 * 1024;

/// Sanity cap on the watch list.
pub const MAX_ADDRESSES: usize = 1000;

#[derive(Debug, Clone)]
pub struct Config {
    /// Explorer base URL, e.g. `https://host` (no trailing slash needed).
    pub explorer: String,
    /// Max index-lag (blocks) behind the chain tip tolerated before failing
    /// closed. Live index runs a few blocks behind during normal operation.
    pub max_lag: u64,
    /// Retries per address per refresh on stale/incoherent reads.
    pub retries: u32,
    /// How many recent txids to include per address.
    pub txids_limit: usize,
    /// Per-read timeout for HTTP responses.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            explorer: DEFAULT_EXPLORER.to_string(),
            max_lag: 10,
            retries: 2,
            txids_limit: 5,
            timeout: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors — every variant fails closed; Gate-class failures are retryable.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LwError {
    /// Bad configuration (insecure scheme, malformed URL) — never retried.
    Config(String),
    /// DNS/socket/TLS failure.
    Transport(String),
    /// Non-200 status (or a redirect, which is never followed).
    Status(u16),
    /// Response is not `application/json`.
    ContentType(String),
    /// Response body (or declared length) exceeds BODY_CAP.
    BodyTooLarge { cap: u64 },
    /// JSON that does not parse or violates the strict schema.
    Malformed(String),
    /// Index reports incomplete/stale/backwards — retry, then fail closed.
    StaleIndex(String),
    /// Snapshot incoherent (balance ≠ Σ utxos across one refresh) — retry,
    /// then report an inconsistent view.
    Incoherent(String),
}

impl fmt::Display for LwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LwError::Config(s) => write!(f, "config: {s}"),
            LwError::Transport(s) => write!(f, "transport: {s}"),
            LwError::Status(c) => write!(f, "unexpected HTTP status {c} (redirects are never followed)"),
            LwError::ContentType(s) => write!(f, "unexpected content type: {s}"),
            LwError::BodyTooLarge { cap } => write!(f, "response body exceeds cap {cap} bytes"),
            LwError::Malformed(s) => write!(f, "malformed response: {s}"),
            LwError::StaleIndex(s) => write!(f, "stale/incomplete index: {s}"),
            LwError::Incoherent(s) => write!(f, "inconsistent view: {s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Hardened HTTP GET (ureq: rustls, no redirects, timeouts, size cap).
// ---------------------------------------------------------------------------

struct Http {
    agent: ureq::Agent,
}

impl Http {
    fn new(timeout: Duration) -> Self {
        let agent = ureq::AgentBuilder::new()
            .redirects(0) // never follow a redirect silently
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(timeout)
            .build();
        Http { agent }
    }

    /// GET `url`, returning the body bytes. Fails closed on transport error,
    /// any non-200 status (including 3xx), non-JSON content type, declared
    /// or actual body size over the cap.
    fn get_json(&self, url: &str) -> Result<Vec<u8>, LwError> {
        let resp = match self.agent.get(url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => return Err(LwError::Status(code)),
            Err(e) => return Err(LwError::Transport(format!("{e:?}"))),
        };
        if resp.status() != 200 {
            return Err(LwError::Status(resp.status()));
        }
        let ctype = resp.header("content-type").unwrap_or("").to_string();
        if !ctype.starts_with("application/json") {
            return Err(LwError::ContentType(ctype));
        }
        if let Some(cl) = resp.header("content-length") {
            if let Ok(n) = cl.trim().parse::<u64>() {
                if n > BODY_CAP {
                    return Err(LwError::BodyTooLarge { cap: BODY_CAP });
                }
            }
        }
        let mut body = Vec::new();
        let mut chunk = [0u8; 16384];
        let mut reader = resp.into_reader();
        loop {
            let n = reader
                .read(&mut chunk)
                .map_err(|e| LwError::Transport(format!("body read: {e}")))?;
            if n == 0 {
                break;
            }
            if body.len() as u64 + n as u64 > BODY_CAP {
                return Err(LwError::BodyTooLarge { cap: BODY_CAP });
            }
            body.extend_from_slice(&chunk[..n]);
        }
        Ok(body)
    }
}

/// Validate the explorer base URL and normalize (strip trailing '/').
/// HTTPS anywhere; plain HTTP only for loopback hosts (test stubs) —
/// anything else fails before a socket is opened. Userinfo (`user:pass@`)
/// is rejected outright: it can disguise a non-loopback host as a loopback
/// one (`http://127.0.0.1:80@evil/`), so it is never worth the risk.
pub fn validate_base_url(url: &str) -> Result<String, LwError> {
    let bad = |s: String| LwError::Config(format!("explorer URL: {s}"));
    let url = url.trim().trim_end_matches('/').to_string();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| bad(format!("no scheme: {url}")))?;
    if rest.is_empty() {
        return Err(bad("empty authority".into()));
    }
    if rest.contains('@') {
        return Err(bad("userinfo ('@') is not allowed".into()));
    }
    let host_port = rest.split('/').next().unwrap_or("");
    if host_port.is_empty() {
        return Err(bad("no host".into()));
    }
    if host_port.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(bad("control characters in host".into()));
    }
    if host_port.contains('%') {
        // percent-encoded hosts never resolve to what they look like
        return Err(bad("percent-encoding in host is not allowed".into()));
    }
    let (host, port) = split_host_port(host_port).map_err(bad)?;
    let scheme_ok = match scheme {
        "https" => true,
        "http" => {
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false);
            if !loopback {
                return Err(bad(format!(
                    "plain HTTP is only allowed for loopback hosts, not {host}"
                )));
            }
            true
        }
        other => {
            return Err(bad(format!("unsupported scheme {other}://")));
        }
    };
    if !scheme_ok {
        return Err(bad("internal: scheme check failed".into()));
    }
    // port (if present) already validated as u16 by split_host_port
    let _ = port;
    Ok(url)
}

/// Split `host[:port]` / `[v6]:port` into (host, port). Fails closed on
/// empty/malformed ports and junk after an IPv6 literal.
fn split_host_port(host_port: &str) -> Result<(String, Option<u16>), String> {
    if host_port.starts_with('[') {
        let end = host_port
            .find(']')
            .ok_or_else(|| "unterminated IPv6 literal".to_string())?;
        let host = host_port[1..end].to_string();
        let after = &host_port[end + 1..];
        let port = if after.is_empty() {
            None
        } else if let Some(p) = after.strip_prefix(':') {
            if p.is_empty() {
                return Err("empty port".into());
            }
            Some(p.parse::<u16>().map_err(|_| format!("bad port {p}"))?)
        } else {
            return Err(format!("junk after IPv6 literal: {after}"));
        };
        return Ok((host, port));
    }
    match host_port.split_once(':') {
        Some((h, p)) => {
            if h.is_empty() || p.is_empty() {
                return Err("empty host or port".into());
            }
            let port = p.parse::<u16>().map_err(|_| format!("bad port {p}"))?;
            Ok((h.to_string(), Some(port)))
        }
        None => Ok((host_port.to_string(), None)),
    }
}

/// Validate a TRI mainnet P2PKH address (base58check + version byte).
pub fn validate_address(s: &str) -> Result<(), String> {
    let payload = crate::address::address_to_payload(s)
        .map_err(|e| format!("invalid address {s}: {e}"))?;
    if payload[0] != PUBKEY_ADDRESS_VERSION {
        return Err(format!(
            "address {s} is not a mainnet P2PKH address (version byte {} != {}); v1 watches P2PKH only",
            payload[0], PUBKEY_ADDRESS_VERSION
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Strict REST schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChainInfo {
    chain: String,
    blocks: u64,
    bestblockhash: String,
    difficulty: ChainDifficulty,
    moneysupply: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChainDifficulty {
    #[serde(rename = "proof-of-stake")]
    proof_of_stake: f64,
    #[serde(rename = "proof-of-work")]
    proof_of_work: f64,
    #[serde(rename = "proof-of-work_note")]
    proof_of_work_note: String,
}

impl ChainInfo {
    fn validate(self) -> Result<Self, LwError> {
        if self.chain != "main" {
            return Err(LwError::Malformed(format!("chain {} != main", self.chain)));
        }
        if self.blocks == 0 {
            return Err(LwError::Malformed("blocks == 0".into()));
        }
        if !self.moneysupply.is_finite() || self.moneysupply <= 0.0 {
            return Err(LwError::Malformed(format!(
                "moneysupply {} not a positive finite number",
                self.moneysupply
            )));
        }
        Ok(self)
    }
}

/// `/rest/address/<A>/balance` — the ONLY response that carries the
/// observation anchor (chain_tip / index_complete / index_height).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressBalance {
    balance: i64,
    chain_tip: u64,
    index_complete: bool,
    index_height: u64,
    received: i64,
}

impl AddressBalance {
    fn validate(self) -> Result<Self, LwError> {
        if self.balance < 0 || self.received < 0 {
            return Err(LwError::Malformed(format!(
                "negative balance/received ({}/{})",
                self.balance, self.received
            )));
        }
        if self.chain_tip == 0 {
            return Err(LwError::Malformed("chain_tip == 0".into()));
        }
        if self.index_height > self.chain_tip {
            return Err(LwError::Malformed(format!(
                "index_height {} > chain_tip {}",
                self.index_height, self.chain_tip
            )));
        }
        Ok(self)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestUtxo {
    address: String,
    height: u64,
    outputIndex: u64,
    satoshis: i64,
    txid: String,
    #[allow(dead_code)]
    script: String,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_utxos(utxos: &[RestUtxo], addr: &str) -> Result<(), LwError> {
    for u in utxos {
        if !is_hex64(&u.txid) {
            return Err(LwError::Malformed(format!("utxo txid not 64-hex: {}", u.txid)));
        }
        if u.satoshis < 0 {
            return Err(LwError::Malformed(format!("utxo negative satoshis: {}", u.satoshis)));
        }
        if u.height == 0 {
            return Err(LwError::Malformed("utxo height == 0".into()));
        }
        if u.address != addr {
            return Err(LwError::Malformed(format!(
                "utxo address {} != requested {} (mixed response)",
                u.address, addr
            )));
        }
    }
    Ok(())
}

fn validate_txids(txids: &[String]) -> Result<(), LwError> {
    for t in txids {
        if !is_hex64(t) {
            return Err(LwError::Malformed(format!("txid not 64-hex: {t}")));
        }
    }
    Ok(())
}

fn fetch_json<T: serde::de::DeserializeOwned>(http: &Http, url: &str) -> Result<T, LwError> {
    let body = http.get_json(url)?;
    serde_json::from_slice(&body).map_err(|e| LwError::Malformed(format!("JSON parse: {e}")))
}

// ---------------------------------------------------------------------------
// Refresh: the pinned observation with fail-closed gates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Coherent snapshot: all gates passed, balance == Σ utxos.
    Ok,
    /// Read completed but is not trustworthy as a figure (divergence class,
    /// or a snapshot that never became coherent) — heights/context shown.
    Inconsistent,
    /// Could not obtain a usable read (stale index, transport, malformed).
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddressReport {
    pub address: String,
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance_sats: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo_sum_sats: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_sats: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_count: Option<usize>,
    /// Height of the addrindex the figures were read at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_height: Option<u64>,
    /// Chain tip the explorer reported at the same moment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_tip: Option<u64>,
    /// Last N txids as served by the explorer (ordering is the server's).
    pub recent_txids: Vec<String>,
    /// Human explanation for non-Ok verdicts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshReport {
    pub explorer: String,
    pub chain_tip: u64,
    pub moneysupply_sats: i64,
    pub addresses: Vec<AddressReport>,
}

struct AttemptData {
    bal: AddressBalance,
    utxo_count: usize,
    utxo_sum: i64,
    tx_count: usize,
    recent: Vec<String>,
}

/// One coherent attempt for one address: balance + utxos + txids, gated.
fn attempt_address(
    http: &Http,
    base: &str,
    addr: &str,
    cfg: &Config,
    chaininfo_tip: u64,
) -> Result<AttemptData, LwError> {
    let bal: AddressBalance = fetch_json(http, &format!("{base}/rest/address/{addr}/balance"))?;
    let bal = bal.validate()?;
    let utxos: Vec<RestUtxo> =
        fetch_json(http, &format!("{base}/rest/address/{addr}/utxos"))?;
    validate_utxos(&utxos, addr)?;
    let txids: Vec<String> = fetch_json(http, &format!("{base}/rest/address/{addr}/txids"))?;
    validate_txids(&txids)?;

    if !bal.index_complete {
        return Err(LwError::StaleIndex(format!(
            "index_complete=false at index height {} (chain tip {})",
            bal.index_height, bal.chain_tip
        )));
    }
    if bal.chain_tip < chaininfo_tip {
        return Err(LwError::StaleIndex(format!(
            "chain tip went backwards ({} < chaininfo blocks {})",
            bal.chain_tip, chaininfo_tip
        )));
    }
    let lag = bal.chain_tip - bal.index_height;
    if lag > cfg.max_lag {
        return Err(LwError::StaleIndex(format!(
            "index lags chain tip by {lag} blocks (max {}): index height {}, tip {}",
            cfg.max_lag, bal.index_height, bal.chain_tip
        )));
    }
    let utxo_sum: i64 = utxos.iter().try_fold(0i64, |acc, u| {
        acc.checked_add(u.satoshis)
            .ok_or_else(|| LwError::Malformed("utxo satoshi sum overflows i64".into()))
    })?;
    if utxo_sum != bal.balance {
        return Err(LwError::Incoherent(format!(
            "balance {} != Σ utxos {} at index height {} / tip {}",
            bal.balance, utxo_sum, bal.index_height, bal.chain_tip
        )));
    }
    let tx_count = txids.len();
    let limit = cfg.txids_limit.min(tx_count);
    Ok(AttemptData {
        recent: txids[tx_count - limit..].to_vec(),
        utxo_count: utxos.len(),
        utxo_sum,
        tx_count,
        bal,
    })
}

/// Full per-address refresh with bounded retries; never returns a guess.
fn refresh_address(
    http: &Http,
    base: &str,
    addr: &str,
    cfg: &Config,
    moneysupply_sats: i64,
    chaininfo_tip: u64,
) -> AddressReport {
    let mut observations: Vec<String> = Vec::new();
    for attempt in 0..=cfg.retries {
        match attempt_address(http, base, addr, cfg, chaininfo_tip) {
            Ok(d) => {
                // Divergence rule (spec §3): the known-unresolved accounting
                // divergence class. Never a bare 0, never a richlist figure.
                if d.bal.received > moneysupply_sats && d.bal.balance == 0 {
                    return AddressReport {
                        address: addr.to_string(),
                        verdict: Verdict::Inconsistent,
                        // The disputed zero figures are deliberately NOT
                        // populated — the JSON surface must not expose
                        // balance/utxo data for this class either (only the
                        // non-disputed context: received, tx count, heights).
                        balance_sats: None,
                        utxo_count: None,
                        utxo_sum_sats: None,
                        received_sats: Some(d.bal.received),
                        tx_count: Some(d.tx_count),
                        index_height: Some(d.bal.index_height),
                        chain_tip: Some(d.bal.chain_tip),
                        recent_txids: Vec::new(),
                        note: Some(format!(
                            "high-throughput address: explorer-reported received {} sats exceeds the chain money supply ({} sats) while the addrindex UTXO view is empty; the addrindex figure is not established for this address class (known unresolved addrindex-vs-scanner divergence) — showing neither 0 nor any second-scanner figure",
                            d.bal.received, moneysupply_sats
                        )),
                    };
                }
                return AddressReport {
                    address: addr.to_string(),
                    verdict: Verdict::Ok,
                    balance_sats: Some(d.bal.balance),
                    utxo_count: Some(d.utxo_count),
                    utxo_sum_sats: Some(d.utxo_sum),
                    received_sats: Some(d.bal.received),
                    tx_count: Some(d.tx_count),
                    index_height: Some(d.bal.index_height),
                    chain_tip: Some(d.bal.chain_tip),
                    recent_txids: d.recent,
                    note: None,
                };
            }
            Err(e) => {
                observations.push(format!("attempt {}: {e}", attempt + 1));
                if attempt < cfg.retries {
                    sleep(Duration::from_millis(150));
                }
            }
        }
    }
    // Fail closed: Incoherent → "inconsistent view" (spec §2 snapshot rule);
    // everything else → unavailable, with the last observation shown.
    let last = observations.last().cloned().unwrap_or_default();
    let (verdict, note) = if last.contains("inconsistent view") {
        (
            Verdict::Inconsistent,
            format!("{} attempts: {}", observations.len(), observations.join("; ")),
        )
    } else {
        (
            Verdict::Unavailable,
            format!("{} attempts: {}", observations.len(), observations.join("; ")),
        )
    };
    AddressReport {
        address: addr.to_string(),
        verdict,
        balance_sats: None,
        utxo_count: None,
        utxo_sum_sats: None,
        received_sats: None,
        tx_count: None,
        index_height: None,
        chain_tip: None,
        recent_txids: Vec::new(),
        note: Some(note),
    }
}

/// One full refresh of every address. Fails (Err) only if the refresh-level
/// context (chaininfo) cannot be read; per-address problems become verdicts.
pub fn refresh(cfg: &Config, addresses: &[String]) -> Result<RefreshReport, LwError> {
    let base = validate_base_url(&cfg.explorer)?;
    let http = Http::new(cfg.timeout);
    // Refresh-level context read, with the same bounded retry the address
    // reads get: the live explorer occasionally answers in seconds, and a
    // one-shot CLI should not die on a single slow response.
    let url = format!("{base}/rest/chaininfo");
    let mut ci: Option<ChainInfo> = None;
    let mut last_err: Option<LwError> = None;
    for attempt in 0..=2u32 {
        match fetch_json::<ChainInfo>(&http, &url) {
            Ok(c) => match c.validate() {
                Ok(v) => {
                    ci = Some(v);
                    break;
                }
                // malformed schema: fail closed immediately, no retry
                Err(e) => return Err(e),
            },
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    sleep(Duration::from_millis(300));
                }
            }
        }
    }
    let ci = ci.ok_or_else(|| last_err.unwrap())?;
    let moneysupply_sats = (ci.moneysupply * 1_000_000.0).round() as i64;
    let chaininfo_tip = ci.blocks;
    let addresses = addresses
        .iter()
        .filter(|a| !a.trim().is_empty())
        .map(|a| a.trim().to_string())
        .collect::<Vec<_>>();
    if addresses.len() > MAX_ADDRESSES {
        return Err(LwError::Config(format!(
            "{} addresses exceeds cap {MAX_ADDRESSES}",
            addresses.len()
        )));
    }
    // Library-level guard: even a caller that skipped CLI validation cannot
    // smuggle a malformed address into a request path.
    for a in &addresses {
        validate_address(a).map_err(LwError::Config)?;
    }
    let reports = addresses
        .iter()
        .map(|a| refresh_address(&http, &base, a, cfg, moneysupply_sats, chaininfo_tip))
        .collect();
    Ok(RefreshReport {
        explorer: base,
        chain_tip: chaininfo_tip,
        moneysupply_sats,
        addresses: reports,
    })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn sats_to_tri(s: i64) -> String {
    format!("{}.{:06}", s / 1_000_000, (s % 1_000_000) as i64)
}

/// Human output. Non-Ok verdicts never print a balance figure — the note
/// carries the context instead (spec §3: never a bare 0, never a guess).
pub fn render_human(r: &RefreshReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "tri7 light wallet — WATCH-ONLY; all figures explorer-reported (not spendable-verified)"
    );
    let _ = writeln!(
        s,
        "explorer {} | chain tip {} | money supply {} TRI",
        r.explorer,
        r.chain_tip,
        sats_to_tri(r.moneysupply_sats)
    );
    for a in &r.addresses {
        let _ = writeln!(s, "{}", a.address);
        match a.verdict {
            Verdict::Ok => {
                let _ = writeln!(
                    s,
                    "  balance   {} TRI (explorer-reported @ index height {}, chain tip {})",
                    sats_to_tri(a.balance_sats.unwrap_or(0)),
                    a.index_height.unwrap_or(0),
                    a.chain_tip.unwrap_or(0)
                );
                let _ = writeln!(
                    s,
                    "  utxos     {} (Σ == balance, cross-checked)",
                    a.utxo_count.unwrap_or(0)
                );
                let _ = writeln!(
                    s,
                    "  received  {} TRI across {} txs (as served)",
                    sats_to_tri(a.received_sats.unwrap_or(0)),
                    a.tx_count.unwrap_or(0)
                );
                if !a.recent_txids.is_empty() {
                    let heads: Vec<String> =
                        a.recent_txids.iter().map(|t| t[..8].to_string()).collect();
                    let _ = writeln!(s, "  recent    {}", heads.join(" "));
                }
                let _ = writeln!(s, "  verdict   OK");
            }
            Verdict::Inconsistent => {
                let _ = writeln!(
                    s,
                    "  verdict   INCONSISTENT — {}",
                    a.note.as_deref().unwrap_or("-")
                );
            }
            Verdict::Unavailable => {
                let _ = writeln!(
                    s,
                    "  verdict   UNAVAILABLE (fail closed) — {}",
                    a.note.as_deref().unwrap_or("-")
                );
            }
        }
    }
    s
}

/// JSON output for agents (same data, machine-readable).
pub fn render_json(r: &RefreshReport) -> String {
    serde_json::to_string_pretty(r).unwrap_or_else(|e| {
        format!(
            "{{\"error\":\"render: {e}\"}}"
        )
    })
}

// ---------------------------------------------------------------------------
// Tests (spec §7 acceptance checks 1, 3, 4, 5-stub, 6 — stubbed; live
// checks 2 and 5 run against the real explorer in the tick evidence).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// The paper-wallet roundtrip vector (triangles-paper-wallet
    /// build/tests/roundtrip, run 2026-11-26): BIP39 "abandon"×11 + "about",
    /// path m/44'/2222'/0'/0/0.
    const VECTOR_ADDRESS: &str = "TDrHzzu8pwHh1i4p2UwCFcQi3ktsBXpDDy";

    // -- acceptance check 1 (tri7 half): derivation parity vs paper-wallet --
    #[test]
    fn derivation_parity_pinned_vector() {
        let mnemonic =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let sk = crate::seed::derive_triangles(mnemonic, "", 0, 0, 0).unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        let secret = secp256k1::SecretKey::from_slice(&sk).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &secret);
        let addr = crate::wallet::pubkey_to_address(&pk.serialize());
        assert_eq!(addr, VECTOR_ADDRESS);
    }

    // -- stub server ----------------------------------------------------------
    type Handler = Arc<dyn Fn(&str, usize) -> (u16, &'static str, String) + Send + Sync>;

    fn spawn_stub(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let handler = handler.clone();
                let hits = hits2.clone();
                std::thread::spawn(move || handle_conn(stream, handler, hits));
            }
        });
        (url, hits)
    }

    fn handle_conn(mut stream: TcpStream, handler: Handler, hits: Arc<Mutex<Vec<String>>>) {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        let mut req = String::new();
        if reader.read_line(&mut req).is_err() {
            return;
        }
        let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
        loop {
            let mut l = String::new();
            if reader.read_line(&mut l).is_err() || l.trim().is_empty() {
                break;
            }
        }
        let n = {
            let mut h = hits.lock().unwrap();
            h.push(path.clone());
            h.iter().filter(|p| **p == path).count()
        };
        let (status, ctype, body) = handler(&path, n);
        let reason = match status {
            200 => "OK",
            302 => "Found",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Status",
        };
        // "\x01HUGE" = declare an enormous Content-Length (size-cap test).
        let (cl, body_out) = if body == "\u{1}HUGE" {
            (999_999_999u64, String::new())
        } else {
            (body.len() as u64, body)
        };
        let mut resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {cl}\r\nConnection: close\r\n"
        );
        if status == 302 {
            resp.push_str("Location: https://example.invalid/x\r\n");
        }
        resp.push_str("\r\n");
        resp.push_str(&body_out);
        let _ = stream.write_all(resp.as_bytes());
    }

    // -- canned responses ------------------------------------------------------
    fn ci(blocks: u64, ms: f64) -> String {
        format!(
            "{{\"chain\":\"main\",\"blocks\":{blocks},\"bestblockhash\":\"{blocks:064x}\",\"difficulty\":{{\"proof-of-stake\":1.0,\"proof-of-work\":1.0,\"proof-of-work_note\":\"n\"}},\"moneysupply\":{ms}}}"
        )
    }
    fn bal(balance: i64, tip: u64, idx_h: u64, complete: bool, received: i64) -> String {
        format!(
            "{{\"balance\":{balance},\"chain_tip\":{tip},\"index_complete\":{complete},\"index_height\":{idx_h},\"received\":{received}}}"
        )
    }
    fn utxos_json(entries: &[(i64, usize)]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(s, i)| {
                format!(
                    "{{\"address\":\"{VECTOR_ADDRESS}\",\"height\":100,\"outputIndex\":0,\"satoshis\":{s},\"script\":\"00\",\"txid\":\"{:064x}\"}}",
                    i
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }
    fn txids_json(ids: &[String]) -> String {
        format!("[{}]", ids.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(","))
    }
    fn hex_id(i: usize) -> String {
        format!("{i:064x}")
    }

    fn cfg_for(url: String) -> Config {
        Config {
            explorer: url,
            max_lag: 10,
            retries: 2,
            txids_limit: 5,
            timeout: Duration::from_secs(5),
        }
    }

    fn count(hits: &Arc<Mutex<Vec<String>>>, suffix: &str) -> usize {
        hits.lock().unwrap().iter().filter(|p| p.ends_with(suffix)).count()
    }

    // happy path: coherent snapshot → Ok, figures present, JSON round-trips
    #[test]
    fn happy_path_ok_and_json() {
        let (url, hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(1000, 1000, 1000, true, 5000))
            } else if path.ends_with("/utxos") {
                (200, "application/json", utxos_json(&[(700, 1), (300, 2)]))
            } else if path.ends_with("/txids") {
                (200, "application/json", txids_json(&[hex_id(1), hex_id(2), hex_id(3)]))
            } else {
                (404, "application/json", "{}".into())
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        let a = &rep.addresses[0];
        assert_eq!(a.verdict, Verdict::Ok);
        assert_eq!(a.balance_sats, Some(1000));
        assert_eq!(a.utxo_count, Some(2));
        assert_eq!(a.utxo_sum_sats, Some(1000));
        assert_eq!(a.tx_count, Some(3));
        assert_eq!(a.recent_txids.len(), 3);
        assert_eq!(count(&hits, "/balance"), 1);
        let human = render_human(&rep);
        assert!(human.contains("explorer-reported"));
        let json = render_json(&rep);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["addresses"][0]["verdict"], "ok");
        assert_eq!(v["addresses"][0]["balance_sats"], 1000);
    }

    // acceptance check 3 (stubbed): index_complete=false → fail closed
    #[test]
    fn index_incomplete_fails_closed() {
        let (url, hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(1000, 1000, 990, false, 5000))
            } else if path.ends_with("/utxos") {
                (200, "application/json", utxos_json(&[(1000, 1)]))
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        let a = &rep.addresses[0];
        assert_eq!(a.verdict, Verdict::Unavailable);
        assert!(a.note.as_deref().unwrap().contains("index_complete=false"));
        assert_eq!(a.balance_sats, None);
        assert_eq!(count(&hits, "/balance"), 3); // 1 + 2 retries
        let human = render_human(&rep);
        assert!(human.contains("UNAVAILABLE"));
    }

    // retry-then-success: transient staleness recovers on retry
    #[test]
    fn retry_then_success() {
        let (url, hits) = spawn_stub(Arc::new(move |path: &str, n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                // first read: incomplete; second: caught up
                if n == 1 {
                    (200, "application/json", bal(1000, 1000, 980, false, 5000))
                } else {
                    (200, "application/json", bal(1000, 1000, 1000, true, 5000))
                }
            } else if path.ends_with("/utxos") {
                (200, "application/json", utxos_json(&[(1000, 1)]))
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Ok);
        assert_eq!(count(&hits, "/balance"), 2);
    }

    // acceptance check 4 (stubbed): incoherent snapshot (balance@H1 vs
    // utxos@H2 class) → retries, then "inconsistent view", never a number
    #[test]
    fn snapshot_incoherence_fails_closed_as_inconsistent() {
        let (url, hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(1000, 1000, 1000, true, 5000))
            } else if path.ends_with("/utxos") {
                // Σ = 90 ≠ balance 100 on every attempt
                (200, "application/json", utxos_json(&[(90, 1)]))
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        let a = &rep.addresses[0];
        assert_eq!(a.verdict, Verdict::Inconsistent);
        let note = a.note.as_deref().unwrap();
        assert!(note.contains("inconsistent view"));
        assert!(note.contains("height"));
        assert_eq!(a.balance_sats, None);
        assert_eq!(count(&hits, "/balance"), 3);
    }

    // chain tip going backwards vs chaininfo → fail closed
    #[test]
    fn chain_tip_backwards_fails_closed() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(1000, 999, 999, true, 5000))
            } else if path.ends_with("/utxos") {
                (200, "application/json", utxos_json(&[(1000, 1)]))
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Unavailable);
        assert!(rep.addresses[0].note.as_deref().unwrap().contains("backwards"));
    }

    // acceptance check 5 (stubbed half): TRDuu34-class divergence →
    // INCONSISTENT, never a bare 0 balance line
    #[test]
    fn divergence_class_is_inconsistent_not_zero() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                // money supply 100 TRI = 100_000_000 sats
                (200, "application/json", ci(1000, 100.0))
            } else if path.ends_with("/balance") {
                // received 1.25T sats >> supply, balance 0 (coherent: Σ utxos = 0)
                (200, "application/json", bal(0, 1000, 1000, true, 1_252_241_417_957))
            } else if path.ends_with("/utxos") {
                (200, "application/json", "[]".into())
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        let a = &rep.addresses[0];
        assert_eq!(a.verdict, Verdict::Inconsistent);
        assert!(a.note.as_deref().unwrap().contains("high-throughput"));
        let human = render_human(&rep);
        assert!(!human.contains("balance   0.000000"));
        // agent-facing JSON must not expose the disputed zero figures either
        let json = render_json(&rep);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["addresses"][0].get("balance_sats").is_none());
        assert!(v["addresses"][0].get("utxo_sum_sats").is_none());
        assert!(v["addresses"][0].get("utxo_count").is_none());
        // non-disputed context stays for diagnosis
        assert_eq!(v["addresses"][0]["received_sats"], 1_252_241_417_957i64);
        assert_eq!(v["addresses"][0]["verdict"], "inconsistent");
    }

    // acceptance check 6 (stubbed): malformed JSON / non-200 / redirect /
    // wrong content type / oversized → all fail closed
    #[test]
    fn malformed_json_fails_closed() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", "{not json".into())
            } else {
                (200, "application/json", "[]".into())
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Unavailable);
        assert!(rep.addresses[0].note.as_deref().unwrap().contains("Malformed") || rep.addresses[0].note.as_deref().unwrap().contains("malformed"));
    }

    #[test]
    fn non_200_and_redirect_and_type_fail_closed() {
        for bad in ["500", "404", "302", "text"] {
            let (url, _hits) = spawn_stub(Arc::new(move |path: &str, _n: usize| {
                if path.ends_with("/rest/chaininfo") {
                    (200, "application/json", ci(1000, 226993.898137))
                } else if path.ends_with("/balance") {
                    match bad {
                        "500" => (500, "application/json", "{}".into()),
                        "404" => (404, "application/json", "{}".into()),
                        "302" => (302, "application/json", "{}".into()),
                        _ => (200, "text/html", "<html/>".into()),
                    }
                } else {
                    (200, "application/json", "[]".into())
                }
            }));
            let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
            let a = &rep.addresses[0];
            assert_eq!(a.verdict, Verdict::Unavailable, "case {bad}");
            assert!(a.balance_sats.is_none(), "case {bad}");
        }
    }

    #[test]
    fn oversized_body_fails_closed() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", "\u{1}HUGE".into())
            } else {
                (200, "application/json", "[]".into())
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        let a = &rep.addresses[0];
        assert_eq!(a.verdict, Verdict::Unavailable);
        assert!(a.note.as_deref().unwrap().contains("cap"));
    }

    // txid hex strictness
    #[test]
    fn non_hex_txid_fails_closed() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(0, 1000, 1000, true, 0))
            } else if path.ends_with("/utxos") {
                (200, "application/json", "[]".into())
            } else {
                (200, "application/json", txids_json(&["zz".repeat(32)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Unavailable);
    }

    // insecure http to a non-loopback host is rejected before any socket
    #[test]
    fn insecure_non_loopback_rejected() {
        for bad in [
            "http://192.0.2.5",
            "http://example.com",
            "ftp://example.com",
            // userinfo disguise: loopback-looking userinfo, real host after '@'
            "http://127.0.0.1:80@example.com",
            "https://user:pass@example.com",
            // percent-encoded host
            "http://%31%32%37.0.0.1/",
            // control characters
            "http://exa mple.com",
            // malformed ports
            "http://127.0.0.1:port",
            "http://127.0.0.1:",
            "http://127.0.0.1:99999",
            // junk after an IPv6 literal
            "http://[::1]junk",
            // unterminated IPv6 literal
            "http://[::1",
        ] {
            let cfg = Config {
                explorer: bad.into(),
                ..Default::default()
            };
            assert!(refresh(&cfg, &[VECTOR_ADDRESS.into()]).is_err(), "case {bad}");
        }
    }

    // base-URL validation matrix (no server needed): what IS allowed
    #[test]
    fn url_validation_accepts() {
        for good in [
            "https://blocks.cryptographic-triangles.org",
            "https://host.example:8443",
            "http://127.0.0.1:3911",
            "http://127.0.0.1",
            "http://localhost",
            "http://localhost:8080",
            "http://[::1]",
            "http://[::1]:8080",
            "http://127.0.0.1:3911/",
            "http://127.0.0.1:3911/rest",
        ] {
            assert!(validate_base_url(good).is_ok(), "case {good}");
        }
        assert_eq!(
            validate_base_url("http://127.0.0.1:3911/").unwrap(),
            "http://127.0.0.1:3911"
        );
        // the userinfo bypass class must be rejected BEFORE any socket: a
        // standards-compliant client would connect to example.com here
        assert!(validate_base_url("http://127.0.0.1:80@example.com").is_err());
    }

    // unknown JSON fields are rejected (deny_unknown_fields = strict schema)
    #[test]
    fn unknown_fields_rejected() {
        let (url, _hits) = spawn_stub(Arc::new(|path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (
                    200,
                    "application/json",
                    format!(
                        "{{\"balance\":0,\"chain_tip\":1000,\"index_complete\":true,\"index_height\":1000,\"received\":0,\"extra\":\"surprise\"}}"
                    ),
                )
            } else {
                (200, "application/json", "[]".into())
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Unavailable);
        assert!(rep.addresses[0].note.as_deref().unwrap().contains("unknown field"));
    }

    // satoshi sums use checked arithmetic — an overflowing utxo set fails
    // closed instead of panicking (debug) or wrapping (release)
    #[test]
    fn utxo_sum_overflow_fails_closed() {
        let big = i64::MAX;
        let (url, _hits) = spawn_stub(Arc::new(move |path: &str, _n: usize| {
            if path.ends_with("/rest/chaininfo") {
                (200, "application/json", ci(1000, 226993.898137))
            } else if path.ends_with("/balance") {
                (200, "application/json", bal(big, 1000, 1000, true, big))
            } else if path.ends_with("/utxos") {
                (
                    200,
                    "application/json",
                    utxos_json(&[(big, 1), (big, 2)]),
                )
            } else {
                (200, "application/json", txids_json(&[hex_id(1)]))
            }
        }));
        let rep = refresh(&cfg_for(url), &[VECTOR_ADDRESS.into()]).unwrap();
        assert_eq!(rep.addresses[0].verdict, Verdict::Unavailable);
        assert!(rep.addresses[0].note.as_deref().unwrap().contains("overflows"));
    }

    // address validation: base58check + version byte enforced
    #[test]
    fn address_validation() {
        assert!(validate_address(VECTOR_ADDRESS).is_ok());
        assert!(validate_address("TRnotarealaddressXXXXXXXXXXXXXXXXX").is_err());
    }
}
