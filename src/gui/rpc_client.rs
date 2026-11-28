//! JSON-RPC client for the tri7d native surface (127.0.0.1:19114).
//! Pure std (no HTTP crate) — mirrors follow_tip.rs's probe. Used by the
//! Debug window (rpcconsole), the connection-info panel, and the passphrase
//! dialog. The allow list is deliberately RESTRICTED: reads, the staking /
//! messaging operations the GUI drives, and the wallet-file passphrase verbs
//! (encrypt / change / unlock / lock / decrypt). Anything that moves coins or
//! exports secrets (sendtoaddress, dumpprivkey, dumpwallet, import*, move) is
//! refused client-side before a byte hits the wire;
//! this client additionally restricts itself to the read-only method allow
//! list as defense in depth.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Methods the GUI may call. Mirrors the NATIVE read surface (HANDOFF 5b)
/// plus the phase-7 milestone-2 wallet-owned operations the GUI drives on the
/// user's behalf (Send screen Broadcast, staking panel on|off, Messages
/// compose) — these go to the user's OWN tri7d on loopback; the daemon still
/// validates everything server-side. Anything else → refused client-side
/// before a byte hits the wire.
pub const ALLOWED_METHODS: &[&str] = &[
    "gettip",
    "getblockcount",
    "getblockhash",
    "getblock",
    "getrawtransaction",
    "getinfo",
    // balance CARD atomicity (2026-11-11 judge blocker): the 5s poll reads
    // the total AND its coverage facts (source/height/partial) from ONE
    // getwalletinfo payload. The verb was missing here, so EVERY getwalletinfo
    // call died on this gate and the card's number/label facts silently
    // never loaded (the tests bypassed rpc_call, so nobody saw it).
    "getwalletinfo",
    // sync progress — same silent-failure class: the poll has asked for
    // getsyncinfo since the "it should show this on the UI" work, but the
    // allow list refused every call, so the status bar never saw it.
    "getsyncinfo",
    "getdifficulty",
    "getconnectioncount",
    "getstakemodifier",
    "getbalance",
    "listunspent",
    "listwallettx",
    "getlocalinfo",
    "listwalletaddresses",
    "getreceivedbyaddress",
    // explorer 2026-11-07: the daemon's native rich-list verb (GUI explorer
    // prefers it, falls back to the balances.json snapshot when the node is
    // older than the verb)
    "richlist",
    "validateaddress",
    "health",
    // phase 7 milestone 2
    "getstakinginfo",
    "staking",
    // legacy parity 2026-11-09 (GUI-POLISH): the Options reserve field reads
    // the node's current value and pushes edits on Apply through this verb
    "reservebalance",
    "sendrawtransaction",
    "getpeerinfo",
    "smsgsend",
    "smsginbox",
    "smsgoutbox",
    "smsgscandir",
    "smsgpurge",
    "smsgaddaddress",
    "smsgkeys",
    "smsglocalinfo",
    // legacy parity 2026-11-09 (workqueue item 11): the RPC console can drive
    // the runtime toggles + option store + rescans (no GUI page calls these)
    "smsgenable",
    "smsgdisable",
    "smsgoptions",
    "smsgscanchain",
    "smsgscanbuckets",
    // legacy parity 2026-11-09 (SMSG read-side): the RPC console can list
    // buckets + look up / import peer keys (no GUI page calls these)
    "smsgbuckets",
    "smsggetpubkey",
    "smsgaddkey",
    // wallet file state (File ▸ Encrypt / Change / Unlock / Remove) — the
    // passphrase is never stored by the GUI, and the node validates it
    "encryptwallet",
    "walletpassphrase",
    "walletpassphrasechange",
    "walletlock",
    "decryptwallet",
    // the fee the NODE charges (daemon-side validated to 0..0.25 TRI, moves
    // no coins) — the Options panel pushes the typed fee there. Without it
    // on the list the whole Options apply failed with "not on the GUI
    // read-only allow list" and NO option could be applied.
    "settxfee",
];

pub fn is_allowed(method: &str) -> bool {
    ALLOWED_METHODS.contains(&method)
}

/// TRI7-RPC-05 (partial): the GUI's RPC surface is LOOPBACK-ONLY. A configured
/// `rpc_addr` naming any other host would put the wallet's verbs — including
/// the passphrase and broadcast verbs — on the wire toward a machine we do not
/// own, so it is refused before a byte is written. Accepts exactly the
/// operator-written loopback spellings: 127.0.0.1, localhost, [::1], ::1
/// (port optional, brackets optional).
pub fn is_loopback_addr(addr: &str) -> bool {
    let host = if let Some(rest) = addr.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, _)) => format!("[{h}]"),
            None => return false, // malformed bracket form
        }
    } else {
        match addr.rsplit_once(':') {
            Some((h, _)) => h.to_string(),
            None => addr.to_string(),
        }
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    )
}

/// Ordinary-person error wording (UX audit 2026-11-14): every error leaving
/// this client leads with a human sentence; the raw technical detail stays
/// appended after an em-dash so support/debugging keeps the specifics.
/// Connect failures mean "the node isn't up (yet)"; `rpc error:` envelopes
/// carry the daemon's own message, which is usually already human (e.g. a
/// wrong-passphrase reply) - shown alone when extractable.
fn humanize_error(raw: String) -> String {
    if raw.starts_with("connect ") {
        format!(
            "Can't reach your node — it may still be starting up. Try again in a moment. (raw: {})",
            raw
        )
    } else if raw.starts_with("rpc error:") {
        // extract the daemon's own "message" — it is already human (e.g. a
        // wrong-passphrase reply); show it ALONE (judge r1: no JSON appendix)
        if let Some(json_part) = raw.strip_prefix("rpc error: ") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_part) {
                if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
                    return msg.to_string();
                }
            }
        }
        raw
    } else if raw.starts_with("send: ") || raw.starts_with("read: ") {
        format!("Lost contact with your node while talking to it. ({})", raw)
    } else {
        raw
    }
}

// (TRI7-RPC-01) the embedded node publishes a per-boot capability token in
// the state dir; the supervisor hands it to us and every rpc_call presents
// it as Bearer auth alongside the node's Basic auth for human callers.
static RPC_TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_rpc_token(token: &str) {
    if let Ok(mut g) = RPC_TOKEN.lock() {
        *g = if token.is_empty() { None } else { Some(token.to_string()) };
    }
}

pub fn rpc_token() -> Option<String> {
    RPC_TOKEN.lock().ok().and_then(|g| g.clone())
}

/// (2026-11-22 diagnostics) whether a capability token is loaded — for the
/// GUI's own refresh diagnostics (does the supervisor handoff reach us?).
pub fn rpc_token_is_set() -> bool {
    rpc_token().is_some()
}

// ---------------------------------------------------------------------------
// (2026-11-27, sami-pc) THE UI THREAD NEVER WAITS ON THE NODE.
//
// fused30 routed every GUI read through node_embedded::local_rpc — the
// in-process dispatcher — so each read ran ON THE RENDER THREAD. Two things
// live behind that door: providers that do real work (getbalance walks the
// live UTXO set and hashes every candidate output) and the wallet live view's
// mutex, which the node thread holds while it seeds. Sampled on the laptop
// (2026-11-27): the render thread had spent 16 h at ~70 % of a core, 40 of 60
// samples parked on that mutex's futex and the rest inside RIPEMD160 — one
// frame per minute, a window Windows reports as hung. The person at the
// screen should never notice the node at all.
//
// RULE: the dispatcher runs on a short-lived worker.
//   READ  — shows the answer it already has the moment a fresh one is not
//           ready within READ_FRESH_WAIT; the worker keeps going and the next
//           frame shows the new answer.
//   ACTION — never answered from a cache and never abandoned: the caller
//           waits for the result of its own call.
// ---------------------------------------------------------------------------

/// Verbs that change state, or whose exact answer the caller acts on.
const ACTION_METHODS: &[&str] = &[
    "staking",
    "reservebalance",
    "settxfee",
    "sendrawtransaction",
    "encryptwallet",
    "walletpassphrase",
    "walletpassphrasechange",
    "walletlock",
    "decryptwallet",
    "smsgsend",
    "smsgpurge",
    "smsgaddaddress",
    "smsgaddkey",
    "smsgenable",
    "smsgdisable",
    "smsgoptions",
    "smsgscandir",
    "smsgscanchain",
    "smsgscanbuckets",
];

/// A READ younger than this is shown again as-is: the same verb is asked
/// several times per frame (three balance cards), and every ask would
/// otherwise re-run the provider.
const READ_COALESCE: Duration = Duration::from_millis(300);
/// How long a READ that already has something to show waits for a fresh one.
const READ_FRESH_WAIT: Duration = Duration::from_millis(400);
/// How long a READ with nothing to show yet waits for its FIRST answer (the
/// socket path's own connect budget — the same "starting up" wait as before).
const READ_FIRST_WAIT: Duration = Duration::from_millis(1500);
/// An ACTION waits this long for its own result.
const ACTION_WAIT: Duration = Duration::from_secs(30);
/// Distinct (method, params) answers kept.
const ANSWER_CAP: usize = 48;
/// Node calls running at once. A page that asks for hundreds of distinct
/// things in one frame must not start hundreds of threads; over the cap a
/// READ shows what it has (or "starting up") and an ACTION runs inline.
const WORKER_CAP: usize = 8;

static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
static NEXT_FLIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

type RpcAnswer = Result<Value, String>;
type Run = Arc<dyn Fn() -> RpcAnswer + Send + Sync>;

#[derive(Default)]
struct CacheEntry {
    /// completed answers so far; a waiter that saw N returns once it sees > N
    gen: u64,
    answer: Option<RpcAnswer>,
    at: Option<Instant>,
    /// the id of the call running now, if any
    inflight: Option<u64>,
}

#[derive(Default)]
struct AnswerCache {
    map: HashMap<String, CacheEntry>,
    /// insertion order, for eviction
    order: Vec<String>,
}

static ANSWERS: Mutex<Option<AnswerCache>> = Mutex::new(None);
/// signalled whenever any call publishes an answer
static CHANGED: Condvar = Condvar::new();

/// Test seam: the engine keeps answers between calls by design, so a test
/// that swaps the dispatcher must start from an empty cache.
#[cfg(test)]
pub(crate) fn reset_answers_for_tests() {
    if let Ok(mut g) = ANSWERS.lock() {
        *g = None;
    }
    CHANGED.notify_all();
}

/// Forget every finished answer: an ACTION may change what they describe.
fn forget_settled_answers() {
    if let Ok(mut g) = ANSWERS.lock() {
        if let Some(c) = g.as_mut() {
            c.map.retain(|_, e| e.inflight.is_some());
            c.order.retain(|k| c.map.contains_key(k));
        }
    }
    CHANGED.notify_all();
}

/// What a caller sees when it starts waiting.
#[derive(Clone, Copy)]
struct Seen {
    gen: u64,
    has_answer: bool,
    age: Option<Duration>,
    inflight: bool,
}

fn seen_none() -> Seen {
    Seen {
        gen: 0,
        has_answer: false,
        age: None,
        inflight: false,
    }
}

fn is_action(method: &str) -> bool {
    ACTION_METHODS.contains(&method)
}

/// Distinguishes calls. The key is the exact parameter text, not a digest of
/// it: two different calls that shared a digest would be handed each other's
/// answers, and in a wallet that must be impossible rather than improbable.
fn cache_key(method: &str, params: &[Value]) -> String {
    let text = serde_json::to_string(params).unwrap_or_default();
    format!("{method}:{text}")
}

fn snapshot(key: &str) -> Option<Seen> {
    let mut guard = ANSWERS.lock().ok()?;
    let c = guard.get_or_insert_with(AnswerCache::default);
    let e = c.map.get(key)?;
    Some(Seen {
        gen: e.gen,
        has_answer: e.answer.is_some(),
        age: e.at.map(|t| t.elapsed()),
        inflight: e.inflight.is_some(),
    })
}

fn stored(key: &str) -> Option<RpcAnswer> {
    ANSWERS
        .lock()
        .ok()?
        .as_ref()?
        .map
        .get(key)?
        .answer
        .clone()
}

fn evict(c: &mut AnswerCache) {
    while c.map.len() > ANSWER_CAP {
        let victim = c
            .order
            .iter()
            .find(|k| c.map.get(*k).map(|e| e.inflight.is_none()).unwrap_or(false))
            .cloned();
        let Some(victim) = victim else { break };
        c.map.remove(&victim);
        c.order.retain(|k| k != &victim);
    }
}

/// A finished call publishes its answer and releases every waiter.
fn finish(key: &str, answer: RpcAnswer) {
    if let Ok(mut guard) = ANSWERS.lock() {
        let c = guard.get_or_insert_with(AnswerCache::default);
        if !c.map.contains_key(key) {
            c.order.push(key.to_string());
        }
        let e = c.map.entry(key.to_string()).or_default();
        e.answer = Some(answer);
        e.at = Some(Instant::now());
        e.gen += 1;
        e.inflight = None;
        evict(c);
    }
    CHANGED.notify_all();
}

/// Owned by the worker thread of one call. If that thread dies without
/// publishing (a panic), it publishes an error in its place so no caller is
/// left waiting on a call that will never answer. It only touches the flight
/// it owns: a later call for the same key has its own id.
struct Flight {
    key: String,
    id: u64,
}

impl Drop for Flight {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, AtomicOrdering::SeqCst);
        let mine = ANSWERS
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .and_then(|c| c.map.get(&self.key))
                    .map(|e| e.inflight == Some(self.id))
            })
            .unwrap_or(false);
        if mine {
            finish(&self.key, Err("The wallet's node stopped answering — please try again.".into()));
        }
    }
}

/// Mark `key` as running and hand `run` to a worker. Returns:
/// - `Ok(true)`  a worker was started,
/// - `Ok(false)` an identical call is already running (the caller waits on it),
/// - `Err(())`   no cache or no thread available: the caller runs it inline.
fn start_flight(key: &str, run: Run) -> Result<bool, ()> {
    let marked = ANSWERS.lock().map(|mut guard| {
        let c = guard.get_or_insert_with(AnswerCache::default);
        if !c.map.contains_key(key) {
            c.order.push(key.to_string());
        }
        let e = c.map.entry(key.to_string()).or_default();
        if e.inflight.is_some() {
            None
        } else {
            let id = NEXT_FLIGHT.fetch_add(1, AtomicOrdering::SeqCst);
            e.inflight = Some(id);
            Some(id)
        }
    });
    let began = match marked {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(false),
        Err(_) => return Err(()),
    };
    // take a slot first, then look: two callers that both checked before taking
    // could both pass the check and overshoot the cap
    if ACTIVE_WORKERS.fetch_add(1, AtomicOrdering::SeqCst) >= WORKER_CAP {
        ACTIVE_WORKERS.fetch_sub(1, AtomicOrdering::SeqCst);
        abandon(key, began);
        return Err(());
    }
    let k = key.to_string();
    let spawned = std::thread::Builder::new()
        .name("tri7-rpc".into())
        .spawn(move || {
            let _flight = Flight { key: k.clone(), id: began };
            let answer = run();
            finish(&k, answer);
        });
    if spawned.is_ok() {
        Ok(true)
    } else {
        // the thread never started, so no Flight will ever clear the flag
        ACTIVE_WORKERS.fetch_sub(1, AtomicOrdering::SeqCst);
        abandon(key, began);
        Err(())
    }
}

/// Release a flight that never got a worker, so the key is not stuck.
fn abandon(key: &str, id: u64) {
    if let Ok(mut guard) = ANSWERS.lock() {
        if let Some(c) = guard.as_mut() {
            if let Some(e) = c.map.get_mut(key) {
                if e.inflight == Some(id) {
                    e.inflight = None;
                }
            }
        }
    }
    CHANGED.notify_all();
}

/// Block until this call's answer is published or the budget runs out.
fn wait_for(key: &str, seen: Seen, budget: Duration) -> Option<RpcAnswer> {
    let deadline = Instant::now() + budget;
    let mut guard = match ANSWERS.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };
    loop {
        let fresh = guard
            .as_ref()
            .and_then(|c| c.map.get(key))
            .map(|e| e.gen > seen.gen && e.answer.is_some())
            .unwrap_or(false);
        if fresh {
            return guard.as_ref().and_then(|c| c.map.get(key)).and_then(|e| e.answer.clone());
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        let (g, _) = CHANGED.wait_timeout(guard, deadline - now).unwrap_or_else(|p| p.into_inner());
        guard = g;
    }
}

/// The one call path for everything the GUI asks of the embedded node.
fn cached_call(key: String, action: bool, run: Run) -> RpcAnswer {
    // READ: an answer younger than the coalesce window is shown again as-is.
    if !action {
        if let Some(a) = coalescible(&key) {
            return a;
        }
    }
    let seen = snapshot(&key).unwrap_or_else(seen_none);
    match start_flight(&key, run.clone()) {
        Ok(true) | Ok(false) => {}
        Err(()) => {
            // over the worker cap or no thread: an ACTION runs inline (it must
            // run exactly once); a READ shows what it has.
            if action {
                let answer = run();
                finish(&key, answer.clone());
                forget_settled_answers();
                return answer;
            }
            return stored(&key)
                .unwrap_or_else(|| Err("The wallet is still starting up — this appears in a moment.".into()));
        }
    }
    let budget = if action {
        ACTION_WAIT
    } else if seen.has_answer {
        READ_FRESH_WAIT
    } else {
        READ_FIRST_WAIT
    };
    let answer = wait_for(&key, seen, budget);
    if action {
        // whatever an ACTION did, the READs describe a different wallet now
        forget_settled_answers();
        return answer.unwrap_or_else(|| {
            Err("The wallet's node did not answer in time. Check whether it went through before trying again.".into())
        });
    }
    if let Some(a) = answer {
        return a;
    }
    // The node is busy with something bigger: show the previous answer now;
    // the worker keeps going and the next frame shows the new one.
    stored(&key).unwrap_or_else(|| Err("The wallet is still starting up — this appears in a moment.".into()))
}

/// A READ asked again within READ_COALESCE gets the same answer, provided the
/// call is not still running and the answer was not an error (an error is
/// retried, not replayed).
fn coalescible(key: &str) -> Option<RpcAnswer> {
    let g = ANSWERS.lock().ok()?;
    let e = g.as_ref()?.map.get(key)?;
    if e.inflight.is_some() || e.answer.is_none() || e.answer.as_ref().map(|a| a.is_err()).unwrap_or(true) {
        return None;
    }
    let age = e.at?.elapsed();
    if age < READ_COALESCE {
        e.answer.clone()
    } else {
        None
    }
}

/// embedded-mode `rpc_call`: the dispatcher runs off the UI thread.
fn embedded_call(method: &str, params: &[Value]) -> RpcAnswer {
    let m = method.to_string();
    let p = params.to_vec();
    cached_call(
        cache_key(method, params),
        is_action(method),
        Arc::new(move || crate::node_embedded::local_rpc(&m, &p).map_err(humanize_error)),
    )
}

pub fn rpc_call(addr: &str, method: &str, params: &[Value]) -> Result<Value, String> {
    if !is_allowed(method) {
        return Err(format!(
            "method \"{}\" is not on the GUI read-only allow list",
            method
        ));
    }
    // (fused30) EMBEDDED MODE: when the node runs in THIS process and its
    // RPC stack is up, route the call through the in-process dispatcher —
    // the same providers and the same handle_body the loopback listener
    // serves. No socket to race, no per-boot capability token to hand
    // off: the balance card's getwalletinfo answers the moment the RPC
    // stack exists (sami-laptop 2026-11-23: the card froze at "Not
    // connected yet" + 0.000 TRI because the HTTP path 401'd on a token
    // the warmup never handed over, while the daemon served getbalance
    // 250.0 externally).
    if crate::node_embedded::local_rpc_ready() {
        return embedded_call(method, params);
    }
    // loopback-only (TRI7-RPC-05): never send wallet verbs to a remote host
    if !is_loopback_addr(addr) {
        let msg = format!(
            "refused rpc_addr {addr:?}: the wallet only talks to its own node on \
             127.0.0.1, localhost or [::1]"
        );
        crate::node_embedded::embedded_log(&format!("rpc_client: {}", msg));
        return Err(msg);
    }
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {}", addr, e))?
        .collect();
    let mut last_err = String::new();
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_millis(1500)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = format!("connect {}: {}", a, e),
        }
    }
    let mut stream = stream.ok_or_else(|| humanize_error(last_err))?;
    stream
        // history.json can be several MB (tens of thousands of rows);
        // loopback is fast but the whole body must arrive inside one
        // read window, so allow more than a health-check timeout
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;

    let body = json!({"jsonrpc":"1.0","id":"tri7-gui","method":method,"params":params}).to_string();
    let auth_hdr = rpc_token()
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        addr,
        auth_hdr,
        body.len(),
        body
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| humanize_error(format!("send: {}", e)))?;
    let mut text = String::new();
    stream
        .read_to_string(&mut text)
        .map_err(|e| humanize_error(format!("read: {}", e)))?;
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&text);
    let v: Value = {
        let envelope: Value =
            serde_json::from_str(body).map_err(|e| format!("rpc json: {} — {:?}", e, body))?;
        let err = envelope.get("error").filter(|e| !e.is_null());
        if let Some(e) = err {
            return Err(humanize_error(format!("rpc error: {}", e)));
        }
        envelope.get("result").cloned().unwrap_or(Value::Null)
    };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn rpc_call_parses_canned_response_and_enforces_allowlist() {
        let _serial = serial();
        // allow-list enforcement happens before any I/O
        assert!(!is_allowed("sendtoaddress"));
        assert!(!is_allowed("dumpprivkey"));
        assert!(!is_allowed("move"));
        // the passphrase verbs ARE reachable: the Encrypt dialog drives them
        assert!(is_allowed("walletpassphrase"));
        assert!(is_allowed("encryptwallet"));
        assert!(is_allowed("walletpassphrasechange"));
        assert!(is_allowed("decryptwallet"));
        assert!(is_allowed("getconnectioncount"));
        assert!(rpc_call("127.0.0.1:1", "sendtoaddress", &[]).is_err());

        // canned loopback server: exactly the tri7d envelope
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = r#"{"result":{"connections":1},"error":null}"#;
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut s, &mut buf);
                let _ = s.write_all(http.as_bytes());
            }
        });
        let v = rpc_call(&format!("127.0.0.1:{}", port), "getconnectioncount", &[])
            .expect("canned call must parse");
        assert_eq!(v["connections"], 1);
    }

    #[test]
    fn rpc_call_unreachable_is_clean_error() {
        let _serial = serial();
        let err = rpc_call("127.0.0.1:9", "getblockcount", &[]).unwrap_err();
        assert!(err.contains("connect"), "unexpected error: {}", err);
    }

    /// (fused30) THE routing the balance card depends on: with the embedded
    /// node's in-process dispatcher registered, rpc_call must answer from
    /// the closure — no socket, no capability token, so a port NOTHING
    /// listens on still answers (this is the branch that ends the 401
    /// "Not connected yet" + 0.000 TRI freeze).
    #[test]
    fn rpc_call_routes_in_process_when_the_embedded_dispatcher_is_live() {
        let _serial = serial();
        crate::node_embedded::set_local_rpc(Box::new(|method, params| {
            Ok(serde_json::json!({
                "via": "in-process",
                "method": method,
                "params": params,
            }))
        }));
        let r = rpc_call("127.0.0.1:1", "getconnectioncount", &[]);
        // global hygiene: sibling tests must not route through the stub
        crate::node_embedded::clear_local_rpc();
        let v = r.expect("the in-process dispatcher answers without a socket");
        assert_eq!(v["via"], "in-process");
        assert_eq!(v["method"], "getconnectioncount");
        // error envelopes still flow through the humanizer (the engine keeps a
        // fresh answer for READ_COALESCE, so start the second half clean)
        reset_answers_for_tests();
        crate::node_embedded::set_local_rpc(Box::new(|_m, _p| {
            Err("rpc error: {\"code\":-1,\"message\":\"nope\"}".to_string())
        }));
        let e = rpc_call("127.0.0.1:1", "getconnectioncount", &[]).unwrap_err();
        crate::node_embedded::clear_local_rpc();
        assert_eq!(e, "nope", "rpc error envelopes are humanized as on the wire");
    }

    /// (2026-11-21, sami-pc) regression pin: the per-boot capability token
    /// must ride the Authorization header VERBATIM ("Bearer <token>" + CRLF)
    /// and no redaction-style placeholder may ever reach the wire. The GUI's
    /// whole daemon poll depends on these bytes being exact.
    #[test]
    fn rpc_call_sends_the_real_bearer_token() {
        let _serial = serial();
        set_rpc_token("test-capability-token");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cap = std::sync::Arc::clone(&captured);
        let body = r#"{"result":{"connections":1},"error":null}"#;
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
                *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = s.write_all(http.as_bytes());
            }
        });
        let v = rpc_call(&format!("127.0.0.1:{}", port), "getconnectioncount", &[])
            .expect("canned call must parse");
        assert_eq!(v["connections"], 1);
        let req = captured.lock().unwrap().clone();
        assert!(
            req.contains("Authorization: Bearer test-capability-token\r\n"),
            "the real token must ride the Authorization header: {req:?}"
        );
        assert!(
            !req.contains("***"),
            "the redaction placeholder must never reach the wire: {req:?}"
        );
        set_rpc_token(""); // global hygiene for sibling tests
    }

    /// TRI7-RPC-05: only loopback spellings are accepted, and a non-loopback
    /// rpc_addr is refused BEFORE any connection attempt (no DNS, no wire).
    #[test]
    fn non_loopback_rpc_addr_is_refused_before_io() {
        let _serial = serial();
        for ok in [
            "127.0.0.1:19114",
            "localhost:19114",
            "[::1]:19114",
            "::1:19114",
            "127.0.0.1",
        ] {
            assert!(is_loopback_addr(ok), "{ok} must be accepted");
        }
        for bad in [
            "10.0.0.5:19114",
            "192.168.1.20:19112",
            "node.example.com:19114",
            "[2001:db8::1]:19114",
            "0.0.0.0:19114",
            "",
        ] {
            assert!(!is_loopback_addr(bad), "{bad} must be refused");
        }
        let e = rpc_call("10.0.0.5:19114", "getblockcount", &[]).unwrap_err();
        assert!(
            e.contains("refused rpc_addr"),
            "refusal must be explicit, not a connect error: {e}"
        );
    }

    /// UX audit: every error class leaving the client leads with a human
    /// sentence (the raw detail stays inside parens for support).
    #[test]
    fn humanize_error_leads_with_plain_language() {
        // connect failure → "starting up" reassurance + raw kept
        let e = humanize_error("connect 127.0.0.1:19114: Connection refused (os error 111)".into());
        assert!(e.starts_with("Can't reach your node"), "{e}");
        assert!(e.contains("(raw:"), "raw detail kept for support: {e}");

        // rpc envelope with a daemon message → message ALONE (no JSON blob)
        let e = humanize_error(
            "rpc error: {\"code\":-14,\"message\":\"The wallet passphrase entered was incorrect\"}".into(),
        );
        assert_eq!(e, "The wallet passphrase entered was incorrect");

        // rpc envelope without a parseable message → passes through
        let e = humanize_error("rpc error: not-json".into());
        assert_eq!(e, "rpc error: not-json");

        // send/read failures → lost-contact wording, raw kept
        let e = humanize_error("send: broken pipe".into());
        assert!(e.starts_with("Lost contact with your node"), "{e}");
        let e = humanize_error("read: timed out".into());
        assert!(e.starts_with("Lost contact with your node"), "{e}");

        // unknown prefix → untouched
        let e = humanize_error("resolve localhost: boom".into());
        assert_eq!(e, "resolve localhost: boom");
    }

    // -----------------------------------------------------------------------
    // The UI thread never waits on the node (2026-11-27). Each test drives the
    // real engine with a stand-in dispatcher so the timings are deterministic.
    // -----------------------------------------------------------------------
    fn slow(answer: i64, millis: u64) -> Run {
        Arc::new(move || {
            std::thread::sleep(Duration::from_millis(millis));
            Ok(json!(answer))
        })
    }
    fn val(a: &RpcAnswer) -> i64 {
        a.as_ref().unwrap().as_i64().unwrap()
    }

    /// rpc_call's globals are process-wide — the capability token, the
    /// embedded dispatcher stub and the answer engine — so every test that
    /// goes through rpc_call takes turns with the others in this module.
    static RPC_CALLS: Mutex<()> = Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        let g = RPC_CALLS.lock().unwrap_or_else(|p| p.into_inner());
        reset_answers_for_tests();
        g
    }

    /// A READ with nothing to show yet gets its first answer; once it has one
    /// it never waits longer than READ_FRESH_WAIT for the next — it shows the
    /// previous answer while the worker finishes.
    #[test]
    fn read_shows_the_previous_answer_instead_of_waiting() {
        let _serial = serial();
        let key = format!("test:read-previous:{}", std::process::id());
        let t0 = Instant::now();
        let first = cached_call(key.clone(), false, slow(7, 400));
        assert_eq!(val(&first), 7);
        assert!(t0.elapsed() >= Duration::from_millis(400), "the first answer must be the real one");

        // inside the coalesce window: the same answer, no new worker
        let t1 = Instant::now();
        assert_eq!(val(&cached_call(key.clone(), false, slow(99, 5000))), 7);
        assert!(t1.elapsed() < Duration::from_millis(50), "coalesced ask must be instant");

        // past the window with a slow node: the PREVIOUS answer, quickly
        std::thread::sleep(READ_COALESCE + Duration::from_millis(20));
        let t2 = Instant::now();
        assert_eq!(val(&cached_call(key.clone(), false, slow(8, 1500))), 7, "must not block for the new answer");
        assert!(
            t2.elapsed() < Duration::from_millis(1200),
            "a READ with something to show waited {:?} — longer than the worker itself",
            t2.elapsed()
        );
        // the worker's answer lands for the NEXT ask
        std::thread::sleep(Duration::from_millis(1700));
        assert_eq!(
            val(&cached_call(key.clone(), false, slow(99, 5000))),
            8,
            "the worker's answer must be what the next ask shows"
        );
    }

    /// An ACTION is never served from a cache: every call runs, and the
    /// caller gets the result of its own call.
    #[test]
    fn action_always_runs_and_waits_for_its_own_result() {
        let _serial = serial();
        let key = format!("test:action-runs:{}", std::process::id());
        let runs = Arc::new(AtomicUsize::new(0));
        for expected in 1..=2usize {
            let r = runs.clone();
            let answer = cached_call(
                key.clone(),
                true,
                Arc::new(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    Ok(json!(r.fetch_add(1, AtomicOrdering::SeqCst) + 1))
                }),
            );
            assert_eq!(val(&answer), expected as i64, "an ACTION must not repeat an earlier answer");
        }
        assert_eq!(runs.load(AtomicOrdering::SeqCst), 2);
    }

    /// A worker that panics must release its callers with an error, never
    /// leave them waiting on a call that will never answer.
    #[test]
    fn a_panicking_worker_releases_its_callers() {
        let _serial = serial();
        let key = format!("test:panic:{}", std::process::id());
        let t0 = Instant::now();
        let boom: Run = Arc::new(|| panic!("stand-in dispatcher failure"));
        let a = cached_call(key.clone(), false, boom);
        assert!(a.is_err(), "a panicked call must surface as an error");
        assert!(t0.elapsed() < Duration::from_secs(1), "released promptly");
        // and the key is usable again
        assert_eq!(val(&cached_call(key, false, slow(3, 1))), 3);
    }

    /// Hundreds of distinct asks in one frame must not start hundreds of
    /// threads: past the cap a READ answers from what it has.
    #[test]
    fn worker_pool_is_bounded() {
        let _serial = serial();
        let base = format!("test:cap:{}:", std::process::id());
        for i in 0..(WORKER_CAP * 6) {
            let _ = cached_call(format!("{base}{i}"), false, slow(1, 400));
        }
        assert!(
            ACTIVE_WORKERS.load(AtomicOrdering::SeqCst) <= WORKER_CAP,
            "more node workers than the cap"
        );
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(ACTIVE_WORKERS.load(AtomicOrdering::SeqCst), 0, "workers must all come home");
    }

    /// The same verb with two different arguments: each gets its own answer,
    /// never the other's (the key is the exact text, so nothing can collide).
    #[test]
    fn different_calls_keep_their_own_answers() {
        let _serial = serial();
        for (text, answer) in [("one", 1), ("two", 2)] {
            let key = cache_key("getbalance", &[serde_json::json!([text])]);
            let got = cached_call(key, false, slow(answer, 50));
            assert_eq!(val(&got), answer, "handed another call's answer");
        }
        assert_ne!(
            cache_key("sendtoaddress", &[serde_json::json!(["one"])]),
            cache_key("sendtoaddress", &[serde_json::json!(["two"])])
        );
    }
}
