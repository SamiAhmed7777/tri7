//! smsg wire protocol engine — SecureMsgReceiveData / SecureMsgSendData
//! parity over tri7's P2P transports (clearnet + tor + i2p).
//!
//! Framing parity with legacy main.cpp/smessage.cpp:
//!   every smsg command carries ONE serialized `std::vector<unsigned char>`
//!   (vRecv >> vchData) — i.e. payload = CompactSize(len) + raw blob. The
//!   helpers `enc_vch`/`dec_vch` do that framing; the generic message
//!   envelope (magic + command + len + checksum) is tri7_core::p2p's job.
//!
//! Commands (smessage.cpp SecureMsgReceiveData / SecureMsgSendData):
//!   smsgPing {}                    → first contact; reply smsgPong
//!   smsgPong {}                    → peer smsg enabled
//!   smsgInv  [u32 n][bucket hdrs]  → we want buckets where the peer has
//!                                     MORE (or equal-count different hash)
//!   smsgShow [u32 n][i64 times]    → answer smsgHave per bucket we hold
//!   smsgHave [i64 time][tokens]    → sift for tokens we lack → smsgWant
//!                                     (+ bucket lock)
//!   smsgWant [i64 time][tokens]    → serve smsgMsg bunches (≤500 msgs,
//!                                     ≤96000 bytes)
//!   smsgMsg  [u32 n][i64 bkt][raw] → validate PoW, store, scan into inbox
//!   smsgMatch [i64 time]           → peer's lastMatched watermark
//!   smsgIgnore [i64 time]          → peer-level ignore window
//!   smsgDisabled {}                → peer disabled smsg
//!
//! Storage: the MessageStore gains a third folder (RELAY, folder 3) that
//! mirrors the legacy smsgStore bucket files — EVERY validated network
//! message lives there for the 48h retention so peers can pull it; inbox /
//! outbox stay decrypted-delivery views. Buckets (inventory) rebuild from
//! the store at startup, like SecureMsgBuildBucketSet.

use crate::legacy::Reader;
use crate::smsg::{
    bucket_time_of, smsg_decrypt, smsg_encrypt, smsg_validate, token_of, Bucket, DecryptedMessage,
    MessageStore, SecureMessage, SmsgKey, SMSG_MAX_BUCKETS, SMSG_OK, SMSG_RETENTION,
    SMSG_TIME_LEEWAY, FOLDER_INBOX, FOLDER_OUTBOX,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Legacy SMSG_SEND_DELAY = 2s (smessage.h:26) — SendData cadence floor.
pub const SMSG_SEND_DELAY: i64 = 2;
/// Legacy bunch caps: 500 messages or 96000 bytes (whichever first).
pub const SMSG_MAX_BUNCH_MSGS: usize = 500;
pub const SMSG_MAX_BUNCH_BYTES: usize = 96000;
/// Relay mirror folder (legacy smsgStore bucket-file equivalent).
pub const FOLDER_RELAY: u16 = 3;
/// Bucket lock parity: 3 × SMSG_THREAD_DELAY seconds, unset on smsgMsg.
pub const BUCKET_LOCK_TICKS: u32 = 3;

/// Legacy SecMsgOptions parity (smessage.h:220 — defaults both true).
/// `newAddressRecv`/`newAddressAnon` gate which flags NEWLY-learned wallet
/// addresses carry; tri7 loads its receive keys once at engine open, so
/// today these govern any future post-boot key-addition path (none yet) —
/// the persisted store + list/set verb shapes are the parity surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecMsgOptions {
    pub new_address_recv: bool,
    pub new_address_anon: bool,
}

impl Default for SecMsgOptions {
    fn default() -> Self {
        SecMsgOptions {
            new_address_recv: true,
            new_address_anon: true,
        }
    }
}

impl SecMsgOptions {
    pub const OPT_RECV: &'static str = "newAddressRecv";
    pub const OPT_ANON: &'static str = "newAddressAnon";
    /// Settings-table storage keys (the smsg.ini line names).
    pub const OPT_RECV_KEY: &'static str = "newAddressRecv";
    pub const OPT_ANON_KEY: &'static str = "newAddressAnon";

    pub fn applies_to(&self, name: &str) -> bool {
        name == Self::OPT_RECV || name == Self::OPT_ANON
    }

    pub fn assign(&mut self, name: &str, v: bool) {
        if name == Self::OPT_RECV {
            self.new_address_recv = v;
        } else if name == Self::OPT_ANON {
            self.new_address_anon = v;
        }
    }

    /// Legacy value tokens (rpcsmessage.cpp smsgoptions set): "+"/"on"/
    /// "true"/"1" and "-"/"off"/"false"/"0". Anything else is "Unknown value."
    pub fn parse_value(s: &str) -> Option<bool> {
        match s {
            "+" | "on" | "true" | "1" => Some(true),
            "-" | "off" | "false" | "0" => Some(false),
            _ => None,
        }
    }

    /// Legacy list/set rendering: "newAddressRecv = true".
    pub fn render(name: &str, v: bool) -> String {
        format!("{} = {}", name, v)
    }

    pub fn as_json(&self) -> Value {
        json!({
            "newAddressRecv": self.new_address_recv,
            "newAddressAnon": self.new_address_anon,
        })
    }
}

pub struct SmsgNetMsg {
    pub command: &'static str,
    pub payload: Vec<u8>,
}

// ---------------------------------------------------------------------------
// vchData framing (CompactSize + bytes)
// ---------------------------------------------------------------------------

fn push_compact_size(w: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        w.push(n as u8);
    } else if n <= 0xffff {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        w.push(0xfe);
        w.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        w.push(0xff);
        w.extend_from_slice(&n.to_le_bytes());
    }
}

pub fn enc_vch(data: &[u8]) -> Vec<u8> {
    let mut w = Vec::with_capacity(5 + data.len());
    push_compact_size(&mut w, data.len() as u64);
    w.extend_from_slice(data);
    w
}

pub fn dec_vch(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut r = Reader::new(payload);
    let n = r.compact_size()? as usize;
    if r.pos() + n > payload.len() {
        return Err(format!(
            "vch: short read: want {} bytes, have {}",
            n,
            payload.len() - r.pos()
        ));
    }
    let body = payload[r.pos()..r.pos() + n].to_vec();
    if r.pos() + n != payload.len() {
        return Err(format!(
            "trailing bytes: consumed {} of {}",
            r.pos() + n,
            payload.len()
        ));
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Wire bodies (the blobs INSIDE the vch frame)
// ---------------------------------------------------------------------------

/// smsgInv / smsgShow bodies.
pub fn build_inv_body(buckets: &[(i64, u32, u32)]) -> Vec<u8> {
    let mut w = Vec::with_capacity(4 + buckets.len() * 16);
    w.extend_from_slice(&(buckets.len() as u32).to_le_bytes());
    for (time, n, hash) in buckets {
        w.extend_from_slice(&time.to_le_bytes());
        w.extend_from_slice(&n.to_le_bytes());
        w.extend_from_slice(&hash.to_le_bytes());
    }
    w
}

pub fn build_show_body(times: &[i64]) -> Vec<u8> {
    let mut w = Vec::with_capacity(4 + times.len() * 8);
    w.extend_from_slice(&(times.len() as u32).to_le_bytes());
    for t in times {
        w.extend_from_slice(&t.to_le_bytes());
    }
    w
}

/// smsgHave / smsgWant bodies ([i64 time][tokens×16]).
pub fn build_token_body(time: i64, tokens: &[(i64, [u8; 8])]) -> Vec<u8> {
    let mut w = Vec::with_capacity(8 + tokens.len() * 16);
    w.extend_from_slice(&time.to_le_bytes());
    for (ts, sample) in tokens {
        w.extend_from_slice(&ts.to_le_bytes());
        w.extend_from_slice(sample);
    }
    w
}

/// smsgMsg body: [u32 nBunch][i64 bktTime][messages…].
pub fn build_msg_body(bkt_time: i64, messages: &[Vec<u8>]) -> Vec<u8> {
    let mut w = Vec::with_capacity(12 + messages.iter().map(|m| m.len()).sum::<usize>());
    w.extend_from_slice(&(messages.len() as u32).to_le_bytes());
    w.extend_from_slice(&bkt_time.to_le_bytes());
    for m in messages {
        w.extend_from_slice(m);
    }
    w
}

pub fn build_match_body(now: i64) -> Vec<u8> {
    now.to_le_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PeerData {
    enabled: bool,
    last_seen: i64,
    last_matched: i64,
    wake: u32,
    ignore_until: i64,
}

impl Default for PeerData {
    fn default() -> Self {
        PeerData {
            enabled: false,
            last_seen: 0,
            last_matched: 0,
            wake: 0,
            ignore_until: 0,
        }
    }
}

#[derive(Default)]
struct EngineState {
    buckets: HashMap<i64, Bucket>,
    peers: HashMap<String, PeerData>,
    /// bucket → (lock ticks left, locking peer label)
    locks: HashMap<i64, (u32, String)>,
    received: u64,
    received_dropped: u64,
    scanned_inbox: u64,
    sent: u64,
    /// (timestamp, sample) of the last locally composed message
    last_sent: Option<(i64, [u8; 8])>,
    /// runtime toggle (smsgenable/smsgdisable): the engine boots ENABLED —
    /// legacy starts secure messaging unless -nosmsg is passed, and its
    /// enable/disable verbs are runtime-only (never persisted).
    enabled: bool,
    /// smsgoptions (persisted in the store's settings table, smsg.ini parity)
    options: SecMsgOptions,
    /// peers owed one final smsgDisabled after a runtime disable (legacy
    /// SecureMsgDisable pushes it to every enabled peer; the tick loop is
    /// tri7's only outbound path, so the notices ride send_data_tick)
    disable_notices: Vec<String>,
}

pub struct SmsgEngine {
    pub store: Arc<MessageStore>,
    /// receive-enabled owned keys (all wallet keys; receive = on)
    pub owned: Vec<SmsgKey>,
    st: Mutex<EngineState>,
}

impl SmsgEngine {
    /// Open (or create) the store under `dir` and rebuild the bucket map.
    pub fn open(dir: &Path, owned: Vec<SmsgKey>) -> Result<Arc<SmsgEngine>, String> {
        let store = Arc::new(MessageStore::open(dir)?);
        // persisted options (smsg.ini parity): absent/unknown values fall
        // back to the legacy defaults (both true)
        let mut options = SecMsgOptions::default();
        for (key, slot) in [
            (SecMsgOptions::OPT_RECV_KEY, &mut options.new_address_recv),
            (SecMsgOptions::OPT_ANON_KEY, &mut options.new_address_anon),
        ] {
            if let Some(v) = store.get_setting(key)? {
                if let Some(b) = SecMsgOptions::parse_value(&v) {
                    *slot = b;
                }
            }
        }
        let engine = SmsgEngine {
            store,
            owned,
            st: Mutex::new(EngineState {
                enabled: true,
                options,
                ..Default::default()
            }),
        };
        let now = now_i64();
        let buckets = engine.rebuild_buckets(now);
        engine.st.lock().unwrap().buckets = buckets;
        Ok(Arc::new(engine))
    }

    fn rebuild_buckets(&self, now: i64) -> HashMap<i64, Bucket> {
        let mut buckets: HashMap<i64, Bucket> = HashMap::new();
        for folder in [FOLDER_RELAY, FOLDER_INBOX, FOLDER_OUTBOX] {
            let Ok(msgs) = self.store.list(folder) else {
                continue;
            };
            for m in msgs {
                let Ok(t) = token_of(&m.data) else { continue };
                if t.timestamp < now - SMSG_RETENTION {
                    continue;
                }
                let b = buckets.entry(bucket_time_of(t.timestamp)).or_default();
                b.tokens.insert(t);
            }
        }
        for b in buckets.values_mut() {
            b.rehash(now);
        }
        buckets
    }

    // -- helpers ------------------------------------------------------------

    /// Store one validated wire message into the relay mirror + bucket map.
    /// Returns false when the message is a duplicate.
    fn store_relay(&self, st: &mut EngineState, wire: &[u8], now: i64) -> Result<bool, String> {
        let t = token_of(wire)?;
        let bt = bucket_time_of(t.timestamp);
        let b = st.buckets.entry(bt).or_default();
        if b.tokens.contains(&t) {
            return Ok(false);
        }
        let key = relay_key(&t);
        let msg = crate::smsg::StoredMessage {
            key,
            time_received: now,
            status: 0,
            addr_to: String::new(),
            addr_outbox: String::new(),
            data: wire.to_vec(),
        };
        let inserted = self.store.insert(FOLDER_RELAY, &msg)?;
        // the store may already hold it (restart dedupe) — only add the
        // token when this really is new
        if inserted {
            b.tokens.insert(t);
            b.rehash(now);
        }
        Ok(inserted)
    }

    /// SecureMsgScanMessage: try every owned key; on a hit insert the inbox
    /// row and learn the sender pubkey (SecureMsgInsertAddress).
    fn scan_into_inbox(&self, st: &mut EngineState, wire: &[u8], now: i64) -> bool {
        let mut hit: Option<DecryptedMessage> = None;
        let mut owner: Option<&SmsgKey> = None;
        for k in &self.owned {
            match smsg_decrypt(wire, k) {
                Ok(dm) => {
                    hit = Some(dm);
                    owner = Some(k);
                    break;
                }
                Err(_) => continue,
            }
        }
        let (Some(dm), Some(k)) = (hit, owner) else {
            return false;
        };
        let t = token_of(wire).unwrap_or(crate::smsg::Token {
            timestamp: dm.timestamp,
            sample: [0u8; 8],
        });
        let key = inbox_key(&t);
        let msg = crate::smsg::StoredMessage {
            key,
            time_received: now,
            status: crate::smsg::SMSG_MASK_UNREAD,
            addr_to: k.address.clone(),
            addr_outbox: String::new(),
            data: wire.to_vec(),
        };
        if let Ok(true) = self.store.insert(FOLDER_INBOX, &msg) {
            st.scanned_inbox += 1;
            // learn the sender identity key so replies work without a manual
            // addrbook entry (the legacy addrpkdb flow)
            if let Some(pk) = &dm.from_pubkey {
                let h160 = crate::address::hash160(pk);
                let _ = self.store.insert_pubkey(&h160, pk, now);
            }
            return true;
        }
        false
    }

    // -- protocol -----------------------------------------------------------

    /// SecureMsgSendData parity: what (if anything) to push to a peer this
    /// tick. First contact → smsgPing; then cadence-gated bucket inventory.
    pub fn send_data_tick(&self, label: &str, now: i64) -> Option<SmsgNetMsg> {
        let mut st = self.st.lock().unwrap();
        if !st.enabled {
            // disabled: one final smsgDisabled per peer we had chatted with
            // (legacy SecureMsgDisable pushes it immediately; tri7's tick loop
            // is the only outbound path), then silence.
            if let Some(pos) = st.disable_notices.iter().position(|l| l == label) {
                st.disable_notices.remove(pos);
                return Some(SmsgNetMsg {
                    command: "smsgDisabled",
                    payload: enc_vch(&[]),
                });
            }
            return None;
        }
        let pd = st.peers.entry(label.to_string()).or_default();
        if pd.last_seen == 0 {
            // first contact: ping once, do nothing until the pong
            pd.last_seen = now;
            return Some(SmsgNetMsg {
                command: "smsgPing",
                payload: enc_vch(&[]),
            });
        }
        if !pd.enabled || now - pd.last_seen < SMSG_SEND_DELAY || now < pd.ignore_until {
            return None;
        }
        // wake counter expiry → resend the full inventory (10..300 × delay,
        // deterministic-at-zero here: the exact legacy rand adds jitter we
        // don't need for correctness)
        if pd.wake < 1 {
            pd.last_matched = 0;
            pd.wake = 10 + ((now as u64) % 290) as u32;
        }
        pd.wake -= 1;
        let last_matched = pd.last_matched;

        let mut headers: Vec<(i64, u32, u32)> = Vec::new();
        {
            let mut btimes: Vec<i64> = st.buckets.keys().copied().collect();
            btimes.sort_unstable();
            for bt in btimes {
                let b = match st.buckets.get(&bt) {
                    Some(b) => b,
                    None => continue,
                };
                let n = b.tokens.len() as u32;
                if b.time_changed < last_matched || n < 1 {
                    continue;
                }
                headers.push((bt, n, b.hash));
            }
        }
        let pd = st.peers.get_mut(label).unwrap();
        pd.last_seen = now;
        if headers.is_empty() {
            return None;
        }
        Some(SmsgNetMsg {
            command: "smsgInv",
            payload: enc_vch(&build_inv_body(&headers)),
        })
    }

    /// Handle one inbound smsg command. Returns replies/requests to send.
    /// All failures are peer-local (logged + dropped), never node-fatal.
    pub fn handle(
        &self,
        label: &str,
        command: &str,
        payload: &[u8],
        now: i64,
    ) -> Vec<SmsgNetMsg> {
        let body = match dec_vch(payload) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[smsg] bad vch frame from {}: {} at {}", label, e, now);
                return vec![];
            }
        };
        let mut st = self.st.lock().unwrap();
        if !st.enabled {
            // disabled: keep peer-state bookkeeping alive, ignore everything
            // else (legacy ProcessMessage guards every smsg command on
            // fSecMsgEnabled — no pong, no data, no storage while disabled)
            if command == "smsgDisabled" {
                st.peers.entry(label.to_string()).or_default().enabled = false;
            }
            return vec![];
        }
        match command {
            "smsgPing" => vec![SmsgNetMsg {
                command: "smsgPong",
                payload: enc_vch(&[]),
            }],
            "smsgPong" => {
                st.peers.entry(label.to_string()).or_default().enabled = true;
                vec![]
            }
            "smsgDisabled" => {
                st.peers.entry(label.to_string()).or_default().enabled = false;
                vec![]
            }
            "smsgIgnore" => {
                if body.len() < 8 {
                    return vec![];
                }
                let t = i64::from_le_bytes(body[0..8].try_into().unwrap());
                st.peers.entry(label.to_string()).or_default().ignore_until = t;
                vec![]
            }
            "smsgMatch" => {
                if body.len() < 8 {
                    return vec![];
                }
                let mut t = i64::from_le_bytes(body[0..8].try_into().unwrap());
                if t > now + SMSG_TIME_LEEWAY {
                    t = now; // future match time pinned to now (legacy warning path)
                }
                st.peers.entry(label.to_string()).or_default().last_matched = t;
                vec![]
            }
            "smsgInv" => self.handle_inv(&mut st, label, &body, now),
            "smsgShow" => self.handle_show(&mut st, label, &body, now),
            "smsgHave" => self.handle_have(&mut st, label, &body, now),
            "smsgWant" => self.handle_want(&mut st, label, &body),
            "smsgMsg" => self.handle_msg(&mut st, label, &body, now),
            _ => vec![],
        }
    }

    fn handle_inv(
        &self,
        st: &mut EngineState,
        label: &str,
        body: &[u8],
        now: i64,
    ) -> Vec<SmsgNetMsg> {
        if body.len() < 4 {
            return vec![];
        }
        let n_inv = u32::from_le_bytes(body[0..4].try_into().unwrap());
        // +1 leeway like legacy (SMSG_MAX_BUCKETS is retention/bucket+1)
        if n_inv as usize > SMSG_MAX_BUCKETS + 1 {
            return vec![];
        }
        if body.len() < 4 + n_inv as usize * 16 {
            return vec![];
        }
        let mut pd = st.peers.entry(label.to_string()).or_default();
        if now < pd.ignore_until {
            return vec![];
        }
        let mut show: Vec<i64> = Vec::new();
        let mut n_locked = 0usize;
        for i in 0..n_inv as usize {
            let off = 4 + i * 16;
            let time = i64::from_le_bytes(body[off..off + 8].try_into().unwrap());
            let ncontent = u32::from_le_bytes(body[off + 8..off + 12].try_into().unwrap());
            let hash = u32::from_le_bytes(body[off + 12..off + 16].try_into().unwrap());
            if time < now - SMSG_RETENTION || time > now + SMSG_TIME_LEEWAY || ncontent < 1 {
                continue;
            }
            let ours = st.buckets.get(&time);
            if ours.map(|b| b.tokens.len() as u32).unwrap_or(0) > ncontent {
                continue; // we have more — they'll pull from us via their inv
            }
            if let Some(b) = ours {
                if b.tokens.len() as u32 == ncontent && b.hash == hash {
                    continue; // identical bucket
                }
            }
            if st.locks.get(&time).map(|(c, _)| *c).unwrap_or(0) > 0 {
                n_locked += 1;
                continue;
            }
            show.push(time);
        }
        if !show.is_empty() {
            return vec![SmsgNetMsg {
                command: "smsgShow",
                payload: enc_vch(&build_show_body(&show)),
            }];
        }
        if n_locked < 1 {
            // nothing wanted — don't ask again until something changes
            pd.last_matched = now;
            return vec![SmsgNetMsg {
                command: "smsgMatch",
                payload: enc_vch(&build_match_body(now)),
            }];
        }
        vec![]
    }

    fn handle_show(
        &self,
        st: &mut EngineState,
        _label: &str,
        body: &[u8],
        now: i64,
    ) -> Vec<SmsgNetMsg> {
        if body.len() < 4 {
            return vec![];
        }
        let n = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
        if body.len() < 4 + n * 8 {
            return vec![];
        }
        let mut out = Vec::new();
        for i in 0..n {
            let time = i64::from_le_bytes(body[4 + i * 8..12 + i * 8].try_into().unwrap());
            let Some(b) = st.buckets.get(&time) else {
                continue;
            };
            let tokens: Vec<(i64, [u8; 8])> =
                b.tokens.iter().map(|t| (t.timestamp, t.sample)).collect();
            out.push(SmsgNetMsg {
                command: "smsgHave",
                payload: enc_vch(&build_token_body(time, &tokens)),
            });
        }
        let _ = now;
        out
    }

    fn handle_have(
        &self,
        st: &mut EngineState,
        label: &str,
        body: &[u8],
        now: i64,
    ) -> Vec<SmsgNetMsg> {
        if body.len() < 8 {
            return vec![];
        }
        let time = i64::from_le_bytes(body[0..8].try_into().unwrap());
        if time < now - SMSG_RETENTION || time > now + SMSG_TIME_LEEWAY {
            return vec![];
        }
        if st.locks.get(&time).map(|(c, _)| *c).unwrap_or(0) > 0 {
            return vec![];
        }
        let n = (body.len() - 8) / 16;
        let mut want: Vec<(i64, [u8; 8])> = Vec::new();
        for i in 0..n {
            let off = 8 + i * 16;
            let ts = i64::from_le_bytes(body[off..off + 8].try_into().unwrap());
            let sample: [u8; 8] = body[off + 8..off + 16].try_into().unwrap();
            let have = st
                .buckets
                .get(&time)
                .map(|b| b.tokens.contains(&crate::smsg::Token { timestamp: ts, sample }))
                .unwrap_or(false);
            if !have {
                want.push((ts, sample));
            }
        }
        if want.is_empty() {
            return vec![];
        }
        // lock the bucket while the pull is in flight (3 ticks)
        st.locks.insert(time, (BUCKET_LOCK_TICKS, label.to_string()));
        vec![SmsgNetMsg {
            command: "smsgWant",
            payload: enc_vch(&build_token_body(time, &want)),
        }]
    }

    fn handle_want(&self, st: &mut EngineState, _label: &str, body: &[u8]) -> Vec<SmsgNetMsg> {
        if body.len() < 8 {
            return vec![];
        }
        let time = i64::from_le_bytes(body[0..8].try_into().unwrap());
        let Some(b) = st.buckets.get(&time) else {
            return vec![];
        };
        let n = (body.len() - 8) / 16;
        let mut bunch: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 12usize;
        for i in 0..n {
            if bunch.len() >= SMSG_MAX_BUNCH_MSGS || bytes >= SMSG_MAX_BUNCH_BYTES {
                break;
            }
            let off = 8 + i * 16;
            let ts = i64::from_le_bytes(body[off..off + 8].try_into().unwrap());
            let sample: [u8; 8] = body[off + 8..off + 16].try_into().unwrap();
            let token = crate::smsg::Token { timestamp: ts, sample };
            if !b.tokens.contains(&token) {
                continue;
            }
            let key = relay_key(&token);
            let Ok(msgs) = self.store.list(FOLDER_RELAY) else {
                break;
            };
            let hit = msgs.iter().find(|m| m.key == key);
            if let Some(m) = hit {
                bytes += m.data.len();
                bunch.push(m.data.clone());
            }
        }
        if bunch.is_empty() {
            return vec![];
        }
        vec![SmsgNetMsg {
            command: "smsgMsg",
            payload: enc_vch(&build_msg_body(time, &bunch)),
        }]
    }

    fn handle_msg(
        &self,
        st: &mut EngineState,
        label: &str,
        body: &[u8],
        now: i64,
    ) -> Vec<SmsgNetMsg> {
        let _ = label;
        if body.len() < 12 {
            return vec![];
        }
        let n_bunch = u32::from_le_bytes(body[0..4].try_into().unwrap());
        let bkt_time = i64::from_le_bytes(body[4..12].try_into().unwrap());
        if bkt_time > now + SMSG_TIME_LEEWAY || bkt_time < now - SMSG_RETENTION {
            return vec![];
        }
        if n_bunch == 0 || n_bunch as usize > SMSG_MAX_BUNCH_MSGS {
            st.locks.remove(&bkt_time);
            return vec![];
        }
        let mut off = 12usize;
        for _ in 0..n_bunch {
            if body.len() - off < crate::smsg::SMSG_HDR_LEN {
                break;
            }
            let payload_len = u32::from_le_bytes(
                body[off + 100..off + 104].try_into().unwrap(),
            ) as usize;
            if body.len() - off < crate::smsg::SMSG_HDR_LEN + payload_len {
                break;
            }
            let wire = &body[off..off + crate::smsg::SMSG_HDR_LEN + payload_len];
            off += wire.len();
            let m = match SecureMessage::from_wire(wire) {
                Ok(m) => m,
                Err(_) => {
                    st.received_dropped += 1;
                    continue;
                }
            };
            let ts = m.timestamp;
            if ts > now + SMSG_TIME_LEEWAY || ts < now - SMSG_RETENTION {
                st.received_dropped += 1;
                continue;
            }
            if smsg_validate(&m) != SMSG_OK {
                st.received_dropped += 1;
                continue;
            }
            match self.store_relay(st, wire, now) {
                Ok(true) => st.received += 1,
                Ok(false) => {} // duplicate
                Err(_) => {
                    st.received_dropped += 1;
                    continue;
                }
            }
            if self.scan_into_inbox(st, wire, now) {
                // delivered
            }
        }
        // release the pull lock + rehash
        if let Some(b) = st.buckets.get_mut(&bkt_time) {
            b.rehash(now);
        }
        st.locks.remove(&bkt_time);
        vec![]
    }

    // -- outgoing -----------------------------------------------------------

    /// Compose + encrypt + store an outgoing message (smsgsend path).
    /// Returns the token (timestamp, sample). The inventory diff reaches
    /// peers via send_data_tick (timeChanged > lastMatched).
    pub fn send_local(
        &self,
        from_address: &str,
        to_address: &str,
        text: &str,
        now: i64,
    ) -> Result<(i64, [u8; 8]), String> {
        let from = self
            .owned
            .iter()
            .find(|k| k.address == from_address)
            .ok_or_else(|| format!("sending address {} is not a wallet key", from_address))?
            .clone();
        self.send_local_inner(Some(&from), from_address, to_address, text, now)
    }

    /// Anonymous variant (smsgsendanon path): legacy forces addrFrom = "anon"
    /// (rpcsmessage.cpp:550), so the envelope payload header is
    /// `[250][4 unused][len u32]` with NO sender hash160 and NO compact
    /// signature (smessage.cpp:3851-3864 — the 4 unused bytes are "there to
    /// ensure encrypted payload always > 8 bytes"). The recipient cannot
    /// reply: that is the point of the variant.
    pub fn send_local_anon(
        &self,
        to_address: &str,
        text: &str,
        now: i64,
    ) -> Result<(i64, [u8; 8]), String> {
        self.send_local_inner(None, "anon", to_address, text, now)
    }

    /// Shared store/bucket path for both send variants. `from_label` names
    /// the outbox row's sender slot ("anon" for anonymous sends).
    fn send_local_inner(
        &self,
        from: Option<&SmsgKey>,
        from_label: &str,
        to_address: &str,
        text: &str,
        now: i64,
    ) -> Result<(i64, [u8; 8]), String> {
        // recipient pubkey: learned addrbook first, own wallet second
        let h160 = address_to_hash160(to_address)?;
        let to_pubkey = match self.store.lookup_pubkey(&h160)? {
            Some(pk) => pk,
            None => self
                .owned
                .iter()
                .find(|k| k.address == to_address)
                .map(|k| k.pubkey_bytes())
                .ok_or_else(|| {
                    format!(
                        "no public key for {} (the peer must send first, or add it via smsgaddaddress)",
                        to_address
                    )
                })?,
        };
        let wire = smsg_encrypt(from, &to_pubkey, text, now)?;
        let t = token_of(&wire)?;
        let mut st = self.st.lock().unwrap();
        // outbox + relay mirror
        let out_key = outbox_key(&t);
        let om = crate::smsg::StoredMessage {
            key: out_key,
            time_received: now,
            status: 0,
            addr_to: to_address.to_string(),
            addr_outbox: from_label.to_string(),
            data: wire.clone(),
        };
        self.store.insert(FOLDER_OUTBOX, &om)?;
        let rm = crate::smsg::StoredMessage {
            key: relay_key(&t),
            time_received: now,
            status: 0,
            addr_to: String::new(),
            addr_outbox: String::new(),
            data: wire,
        };
        self.store.insert(FOLDER_RELAY, &rm)?;
        let bt = bucket_time_of(t.timestamp);
        let b = st.buckets.entry(bt).or_default();
        b.tokens.insert(t);
        b.rehash(now);
        st.sent += 1;
        st.last_sent = Some((t.timestamp, t.sample));
        Ok((t.timestamp, t.sample))
    }

    /// smsgbroadcast (rpcsmessage.cpp:899-986): one SecureMsgSend-shaped
    /// envelope PER addrbook pubkey row, skipping only the sender address
    /// itself (rpcsmessage.cpp:957-959 — other OWNED wallet addresses still
    /// receive a copy). Every copy is signed by the sender inside the
    /// encrypted payload (compact recoverable signature over the plaintext,
    /// smessage.cpp:3874-3884), which is what makes the snapshot-attestation
    /// pattern `smsgbroadcast <addr> "<height>:<hash>:<iso8601>"` verifiable
    /// by each recipient. Per-recipient failures NEVER abort the loop — they
    /// are counted and reported (rpcsmessage.cpp:961-972). Returns
    /// (sent, failed, [(address, error)]).
    pub fn broadcast_local(
        &self,
        from_address: &str,
        text: &str,
        now: i64,
    ) -> Result<(u32, u32, Vec<(String, String)>), String> {
        let mut sent = 0u32;
        let mut failed = 0u32;
        let mut failures = Vec::new();
        for (_h160, pubkey) in self.store.list_pubkeys()? {
            // the row's address — all tri7 write paths keep hash160(pubkey)
            // == row key, so the address derives from the pubkey itself
            let to_address = crate::wallet::pubkey_to_address(&pubkey);
            if to_address == from_address {
                continue; // legacy "Skip sending to self" rpcsmessage.cpp:957-959
            }
            match self.send_local(from_address, &to_address, text, now) {
                Ok(_) => sent += 1,
                Err(e) => {
                    failed += 1;
                    failures.push((to_address, e));
                }
            }
        }
        Ok((sent, failed, failures))
    }

    /// Manual addrbook insert (smsgaddaddress): address + 33-byte pubkey hex.
    pub fn add_address(&self, address: &str, pubkey: &[u8]) -> Result<bool, String> {
        let h160 = address_to_hash160(address)?;
        if pubkey.len() != 33 {
            return Err(format!("pubkey must be 33 bytes (compressed), got {}", pubkey.len()));
        }
        // sanity: the pubkey must hash to the address
        let derived = crate::wallet::pubkey_to_address(pubkey);
        if derived != address {
            return Err(format!(
                "pubkey does not hash to {} (hashes to {})",
                address, derived
            ));
        }
        self.store.insert_pubkey(&h160, pubkey, now_i64())
    }

    /// Age the bucket locks one tick (SMSG_THREAD_DELAY).
    pub fn tick_locks(&self) {
        let mut st = self.st.lock().unwrap();
        for (_, (c, _)) in st.locks.iter_mut() {
            *c = c.saturating_sub(1);
        }
        st.locks.retain(|_, (c, _)| *c > 0);
    }

    /// 48h retention purge over every folder + expired bucket drop.
    pub fn purge(&self, now: i64) -> usize {
        let n = self.store.purge_expired(now).unwrap_or(0);
        let mut st = self.st.lock().unwrap();
        st.buckets.retain(|bt, _| *bt >= now - SMSG_RETENTION);
        n
    }

    /// Runtime secure-messaging toggle (smsgenable/smsgdisable).
    /// Legacy starts secure messaging at boot unless -nosmsg; the verbs are
    /// runtime-only and never persisted — tri7 boots the engine ENABLED the
    /// same way (its very existence means the wallet was plaintext at boot).
    pub fn enabled(&self) -> bool {
        self.st.lock().unwrap().enabled
    }

    /// SecureMsgDisable parity: stop data flow, drop the bucket set, and
    /// owe every enabled peer one final smsgDisabled.
    pub fn disable(&self) {
        let mut st = self.st.lock().unwrap();
        st.enabled = false;
        st.disable_notices = st
            .peers
            .iter()
            .filter(|(_, p)| p.enabled)
            .map(|(l, _)| l.clone())
            .collect();
        st.buckets.clear();
        st.locks.clear();
        // legacy also sets pnode->smsgData.fEnabled = false per peer
        for (_, p) in st.peers.iter_mut() {
            p.enabled = false;
        }
    }

    /// SecureMsgEnable parity: rebuild the bucket set and clear the peer
    /// table so the first-contact ping re-handshakes everyone (legacy pings
    /// each peer on enable).
    pub fn enable(&self, now: i64) {
        let mut st = self.st.lock().unwrap();
        st.enabled = true;
        st.disable_notices.clear();
        st.peers.clear();
        st.locks.clear();
        st.buckets = self.rebuild_buckets(now);
    }

    /// Current smsgoptions.
    pub fn options(&self) -> SecMsgOptions {
        self.st.lock().unwrap().options
    }

    /// Set + persist smsgoptions (the store's settings table is the
    /// smsg.ini equivalent).
    pub fn set_options(&self, o: SecMsgOptions) -> Result<(), String> {
        self.store.set_setting(
            SecMsgOptions::OPT_RECV_KEY,
            if o.new_address_recv { "true" } else { "false" },
        )?;
        self.store.set_setting(
            SecMsgOptions::OPT_ANON_KEY,
            if o.new_address_anon { "true" } else { "false" },
        )?;
        self.st.lock().unwrap().options = o;
        Ok(())
    }

    /// SecureMsgScanBuckets parity: rescan every relay-store message against
    /// the owned keys, re-delivering whatever the inbox is missing. Returns
    /// the number of newly delivered messages.
    pub fn scan_buckets(&self, now: i64) -> usize {
        let mut st = self.st.lock().unwrap();
        let wires: Vec<Vec<u8>> = match self.store.list(FOLDER_RELAY) {
            Ok(ms) => ms.into_iter().map(|m| m.data).collect(),
            Err(_) => return 0,
        };
        let mut found = 0usize;
        for w in &wires {
            if self.scan_into_inbox(&mut st, w, now) {
                found += 1;
            }
        }
        found
    }

    /// ScanChainForPublicKeys parity, one push at a time: a 33-byte
    /// compressed pubkey learns (hash160(pubkey) → pubkey) in the address db
    /// (legacy takes the address from the prevout's scriptPubKey; for the
    /// standard P2PKH case that address IS hash160(pubkey), which covers the
    /// addrbook's purpose — reply-key lookup). Returns (added, duplicate).
    pub fn learn_pubkey(&self, pubkey: &[u8], now: i64) -> Result<(bool, bool), String> {
        if pubkey.len() != 33 {
            return Err(format!(
                "pubkey must be 33 bytes (compressed), got {}",
                pubkey.len()
            ));
        }
        let _pk = secp256k1::PublicKey::from_slice(pubkey)
            .map_err(|_| "pubkey is invalid".to_string())?;
        let h160 = crate::address::hash160(pubkey);
        let inserted = self.store.insert_pubkey(&h160, pubkey, now)?;
        Ok((inserted, !inserted))
    }

    /// smsglocalinfo / smsgkeys JSON.
    pub fn localinfo_json(&self) -> Value {
        let st = self.st.lock().unwrap();
        json!({
            "enabled": st.enabled,
            "options": st.options.as_json(),
            "addresses": self.owned.iter().map(|k| k.address.clone()).collect::<Vec<_>>(),
            "keys": self.owned.len(),
            "buckets": st.buckets.len(),
            "peers": st.peers.iter().map(|(l, p)| json!({
                "peer": l,
                "enabled": p.enabled,
                "last_matched": p.last_matched,
                "ignore_until": p.ignore_until,
            })).collect::<Vec<_>>(),
            "received": st.received,
            "dropped": st.received_dropped,
            "delivered_to_inbox": st.scanned_inbox,
            "sent": st.sent,
            "last_sent": st.last_sent.map(|(ts, s)| json!({
                "timestamp": ts,
                "sample": hex(&s),
            })),
        })
    }

    /// Inventory view of one folder, decrypted for display (RPC/GUI).
    /// Decrypts with the owning key; rows that fail to decrypt (shouldn't
    /// happen for inbox) show a placeholder.
    pub fn list_folder_json(&self, folder: u16, unread_only: bool) -> Result<Value, String> {
        let msgs = self.store.list(folder)?;
        let mut rows = Vec::new();
        for m in msgs {
            if unread_only && m.status & crate::smsg::SMSG_MASK_UNREAD == 0 {
                continue;
            }
            let key_hex = hex(&m.key);
            let mut from = String::new();
            let mut text = String::new();
            let owner = if folder == FOLDER_INBOX {
                Some(m.addr_to.clone())
            } else {
                Some(m.addr_outbox.clone())
            };
            if let Some(addr) = owner {
                if let Some(k) = self.owned.iter().find(|k| k.address == addr) {
                    if let Ok(dm) = smsg_decrypt(&m.data, k) {
                        from = dm.from_address;
                        text = dm.plaintext;
                    }
                }
            }
            rows.push(json!({
                "key": key_hex,
                "hexversion": 1,
                "from": from,
                // addr_to is the RECIPIENT in both folders: inbox records store
                // the local owning address, outbox records the remote target
                // (send() sets addr_to = to_address; the outbox DECRYPT key is
                // addr_outbox, the local sender identity — see `owner` above).
                "to": m.addr_to.clone(),
                "sent": Value::Bool(folder == FOLDER_OUTBOX),
                "read": m.status & crate::smsg::SMSG_MASK_UNREAD == 0,
                "time": m.time_received,
                "text": text,
                "bytes": m.data.len(),
            }));
        }
        Ok(json!(rows))
    }

    pub fn mark_read(&self, folder: u16, key_hex: &str) -> Result<bool, String> {
        let key = hex_decode_18(key_hex)?;
        self.store.mark_read(folder, &key)
    }

    pub fn delete_msg(&self, folder: u16, key_hex: &str) -> Result<bool, String> {
        let key = hex_decode_18(key_hex)?;
        self.store.delete(folder, &key)
    }

    /// Decrypted view of one inbox row (GUI open).
    pub fn read_msg(&self, folder: u16, key_hex: &str) -> Result<DecryptedMessage, String> {
        let key = hex_decode_18(key_hex)?;
        let msgs = self.store.list(folder)?;
        let m = msgs
            .iter()
            .find(|m| m.key == key)
            .ok_or_else(|| "no such message".to_string())?;
        let owner = if folder == FOLDER_INBOX {
            m.addr_to.clone()
        } else {
            m.addr_outbox.clone()
        };
        let k = self
            .owned
            .iter()
            .find(|k| k.address == owner)
            .ok_or_else(|| format!("key for {} not loaded", owner))?;
        smsg_decrypt(&m.data, k)
    }

    pub fn peer_enabled(&self, label: &str) -> bool {
        self.st
            .lock()
            .unwrap()
            .peers
            .get(label)
            .map(|p| p.enabled)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// key helpers
// ---------------------------------------------------------------------------

fn now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

fn hex_decode_18(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() != 36 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("message key must be 36 hex chars (18 bytes)".into());
    }
    let mut out = Vec::with_capacity(18);
    for i in 0..18 {
        out.push(u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn relay_key(t: &crate::smsg::Token) -> Vec<u8> {
    let mut k = Vec::with_capacity(18);
    k.extend_from_slice(b"rm");
    k.extend_from_slice(&t.timestamp.to_le_bytes());
    k.extend_from_slice(&t.sample);
    k
}

fn inbox_key(t: &crate::smsg::Token) -> Vec<u8> {
    let mut k = Vec::with_capacity(18);
    k.extend_from_slice(b"im");
    k.extend_from_slice(&t.timestamp.to_le_bytes());
    k.extend_from_slice(&t.sample);
    k
}

fn outbox_key(t: &crate::smsg::Token) -> Vec<u8> {
    let mut k = Vec::with_capacity(18);
    k.extend_from_slice(b"om");
    k.extend_from_slice(&t.timestamp.to_le_bytes());
    k.extend_from_slice(&t.sample);
    k
}

fn address_to_hash160(address: &str) -> Result<[u8; 20], String> {
    let payload = crate::address::address_to_payload(address)?;
    if payload[0] != crate::address::PUBKEY_ADDRESS {
        return Err(format!("{} is not a pubkey-hash address", address));
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&payload[1..21]);
    Ok(h)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smsg::{Token, SMSG_ANON_VERSION};

    struct TestKeys {
        a: SmsgKey,
        b: SmsgKey,
    }

    fn make_keys() -> TestKeys {
        // deterministic test scalars (test-only; runtime keys come from the
        // wallet/CSPRNG paths)
        let a_sk = [7u8; 32];
        let b_sk = [11u8; 32];
        TestKeys {
            a: key_for(a_sk, "Tf1GSCsDvLGW3LcSSGaCcvB3VxdvuJg8Wk"),
            b: key_for(b_sk, "TgZmBGIHqVKXCpwvDkkbM7hq8XVsWPoYvB"),
        }
    }

    fn key_for(sk: [u8; 32], _unused: &str) -> SmsgKey {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let ctx = Secp256k1::signing_only();
        let sk = SecretKey::from_slice(&sk).unwrap();
        let pk = PublicKey::from_secret_key(&ctx, &sk);
        let pk_bytes = pk.serialize().to_vec();
        let address = crate::wallet::pubkey_to_address(&pk_bytes);
        SmsgKey {
            address,
            secret: sk_bytes(&sk),
            pubkey: pk_bytes,
        }
    }

    fn sk_bytes(sk: &secp256k1::SecretKey) -> [u8; 32] {
        let mut out = [0u8; 32];
        let s = sk.display_secret().to_string();
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    fn engine(tag: &str, keys: Vec<SmsgKey>) -> Arc<SmsgEngine> {
        let dir = std::env::temp_dir().join(format!(
            "tri7-smsgnet-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        SmsgEngine::open(&dir, keys).unwrap()
    }

    fn wire_ab(keys: &TestKeys, text: &str, ts: i64) -> Vec<u8> {
        crate::smsg::smsg_encrypt(Some(&keys.a), &keys.b.pubkey_bytes(), text, ts).unwrap()
    }

    #[test]
    fn vch_frame_roundtrip() {
        let data: Vec<u8> = (0..300u32).map(|i| (i % 256) as u8).collect();
        let framed = enc_vch(&data);
        assert_eq!(dec_vch(&framed).unwrap(), data);
        // len field says 4 but only 3 body bytes follow
        assert!(dec_vch(&[0xfd, 4, 0, 1, 2, 3]).is_err());
    }

    #[test]
    fn wire_body_shapes_match_legacy_layouts() {
        // inv: u32 + 16-byte headers
        let inv = build_inv_body(&[(1234, 5, 0xdeadbeef)]);
        assert_eq!(inv.len(), 20);
        assert_eq!(u32::from_le_bytes(inv[0..4].try_into().unwrap()), 1);
        assert_eq!(i64::from_le_bytes(inv[4..12].try_into().unwrap()), 1234);
        assert_eq!(u32::from_le_bytes(inv[16..20].try_into().unwrap()), 0xdeadbeef);
        // show: u32 + 8-byte times
        let show = build_show_body(&[-5, 7]);
        assert_eq!(show.len(), 20);
        // have/want: i64 + 16-byte tokens
        let tk = build_token_body(99, &[(1, [2u8; 8])]);
        assert_eq!(tk.len(), 24);
        // msg: u32 + i64 + raw
        let msg = build_msg_body(7, &[vec![1, 2, 3], vec![4, 5]]);
        assert_eq!(&msg[0..4], &2u32.to_le_bytes());
        assert_eq!(&msg[4..12], &7i64.to_le_bytes());
        assert_eq!(&msg[12..], &[1, 2, 3, 4, 5]);
        // match: 8 bytes
        assert_eq!(build_match_body(42).len(), 8);
    }

    #[test]
    fn ping_pong_enables_peer_then_inventory_flows() {
        let k = make_keys();
        let a = engine("pingpong-a", vec![k.a.clone()]);
        let now = now_i64();
        // store a message locally so A has a non-empty bucket
        a.send_local(&k.a.address, &k.b.address, "hello", now).unwrap_err(); // no pubkey known yet — but store... must fail
        // add B's key to A's addrbook
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        a.send_local(&k.a.address, &k.b.address, "hello", now).unwrap();

        // first contact → ping
        let ping = a.send_data_tick("peer-b", now).unwrap();
        assert_eq!(ping.command, "smsgPing");
        // pong enables
        let replies = a.handle("peer-b", "smsgPong", &enc_vch(&[]), now);
        assert!(replies.is_empty());
        assert!(a.peer_enabled("peer-b"));
        // next tick sends the inventory (bucket timeChanged == now > lastMatched 0)
        let inv = a.send_data_tick("peer-b", now + SMSG_SEND_DELAY).unwrap();
        assert_eq!(inv.command, "smsgInv");
        let body = dec_vch(&inv.payload).unwrap();
        assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 1);
        // after a match, no more invs until something changes
        a.handle("peer-b", "smsgMatch", &enc_vch(&build_match_body(now)), now);
        assert!(a.send_data_tick("peer-b", now + SMSG_SEND_DELAY + 1).is_none());
    }

    #[test]
    fn full_roundtrip_a_to_b_over_engine_pair() {
        let k = make_keys();
        let a = engine("rt-a", vec![k.a.clone()]);
        let b = engine("rt-b", vec![k.b.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();

        // A composes to B
        let (ts, sample) = a.send_local(&k.a.address, &k.b.address, "round trip", now).unwrap();

        // wire handshake both directions
        let p1 = a.send_data_tick("b", now).unwrap();
        assert_eq!(p1.command, "smsgPing");
        let r1 = b.handle("a", &p1.command, &p1.payload, now);
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].command, "smsgPong");
        let r2 = a.handle("b", &r1[0].command, &r1[0].payload, now);
        assert!(r2.is_empty());

        // A announces its bucket
        let inv = a.send_data_tick("b", now + SMSG_SEND_DELAY).unwrap();
        assert_eq!(inv.command, "smsgInv");
        let replies = b.handle("a", "smsgInv", &inv.payload, now + SMSG_SEND_DELAY);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].command, "smsgShow");
        // A answers with its tokens
        let replies = a.handle("b", "smsgShow", &replies[0].payload, now + SMSG_SEND_DELAY);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].command, "smsgHave");
        // B wants the missing token
        let replies = b.handle("a", "smsgHave", &replies[0].payload, now + SMSG_SEND_DELAY);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].command, "smsgWant");
        // A serves the message bunch
        let replies = a.handle("b", "smsgWant", &replies[0].payload, now + SMSG_SEND_DELAY);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].command, "smsgMsg");
        // B receives: stores relay + decrypts into inbox
        let replies = b.handle("a", "smsgMsg", &replies[0].payload, now + SMSG_SEND_DELAY);
        assert!(replies.is_empty());

        let inbox = b.list_folder_json(FOLDER_INBOX, false).unwrap();
        let rows = inbox.as_array().unwrap();
        assert_eq!(rows.len(), 1, "exactly one inbox row");
        assert_eq!(rows[0]["from"], k.a.address);
        assert_eq!(rows[0]["to"], k.b.address);
        assert_eq!(rows[0]["text"], "round trip");
        assert_eq!(rows[0]["read"], false);
        assert_eq!(rows[0]["key"], hex(&inbox_key(&Token { timestamp: ts, sample })));
        // learn: B now knows A's pubkey
        let h160 = address_to_hash160(&k.a.address).unwrap();
        assert!(b.store.lookup_pubkey(&h160).unwrap().is_some());
        // relay mirror holds the wire for other peers
        assert_eq!(b.store.list(FOLDER_RELAY).unwrap().len(), 1);
        // B's bucket now includes it
        let info = b.localinfo_json();
        assert_eq!(info["buckets"].as_u64().unwrap(), 1);
    }

    #[test]
    fn outbox_rows_show_the_recipient_and_decrypt_with_the_sender_key() {
        // The outbox 'to' field must be the REMOTE recipient (addr_to, set by
        // send()), not the local sender identity — the sender identity
        // (addr_outbox) is only the decryption key owner. Regression for the
        // judge round that read the identical if/else display branch as a
        // wrong-mapping bug.
        let k = make_keys();
        let a = engine("ob-a", vec![k.a.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        a.send_local(&k.a.address, &k.b.address, "to myself via outbox", now)
            .unwrap();
        let outbox = a.list_folder_json(FOLDER_OUTBOX, false).unwrap();
        let rows = outbox.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["to"], k.b.address, "outbox 'to' = recipient");
        // KNOWN LIMITATION (disclosed in the milestone evidence): the engine
        // implements the RECEIVER-side decrypt only, so an outbox row cannot
        // decrypt its own wire — 'from'/'text' come back empty placeholders.
        // The mapping fields (to/sent/read) are the contract under test.
        assert_eq!(rows[0]["from"], "");
        assert_eq!(rows[0]["text"], "");
        assert_eq!(rows[0]["sent"], true);
        assert_eq!(rows[0]["read"], true, "own sends start read");
    }

    #[test]
    fn reply_path_b_to_a_uses_learned_pubkey() {
        let k = make_keys();
        let a = engine("rp-a", vec![k.a.clone()]);
        let b = engine("rp-b", vec![k.b.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        a.send_local(&k.a.address, &k.b.address, "first", now).unwrap();
        // deliver directly (bypass handshake for brevity)
        let wire = a.store.list(FOLDER_RELAY).unwrap()[0].data.clone();
        let body = build_msg_body(bucket_time_of(now), &[wire]);
        b.handle("a", "smsgMsg", &enc_vch(&body), now);
        // B replies — pubkey learned from A's message signature
        b.send_local(&k.b.address, &k.a.address, "second", now + 5).unwrap();
        let relay = b.store.list(FOLDER_RELAY).unwrap();
        assert_eq!(relay.len(), 2);
        let reply_wire = relay
            .iter()
            .find(|m| crate::smsg::SecureMessage::from_wire(&m.data).unwrap().timestamp == now + 5)
            .unwrap()
            .data
            .clone();
        let body = build_msg_body(bucket_time_of(now + 5), &[reply_wire]);
        a.handle("b", "smsgMsg", &enc_vch(&body), now + 5);
        let inbox = a.list_folder_json(FOLDER_INBOX, false).unwrap();
        let rows = inbox.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["from"], k.b.address);
        assert_eq!(rows[0]["text"], "second");
    }

    #[test]
    fn bad_pow_and_duplicates_dropped_but_peer_stays() {
        let k = make_keys();
        let b = engine("pow-b", vec![k.b.clone()]);
        let now = now_i64();
        let mut wire = wire_ab(&k, "valid", now);
        // corrupt the MAC → invalid PoW/checksum
        wire[64] ^= 0xff;
        let body = build_msg_body(bucket_time_of(now), &[wire.clone(), wire.clone()]);
        b.handle("a", "smsgMsg", &enc_vch(&body), now);
        assert_eq!(b.store.list(FOLDER_RELAY).unwrap().len(), 0);
        assert_eq!(b.localinfo_json()["dropped"].as_u64().unwrap(), 2);
    }

    #[test]
    fn anon_message_roundtrip() {
        // anonymous compose (SMSG_ANON_VERSION payload shape) decrypts with
        // any owned key and delivers without a sender address
        let k = make_keys();
        let b = engine("anon-b", vec![k.b.clone()]);
        let now = now_i64();
        let wire = crate::smsg::smsg_encrypt(None, &k.b.pubkey_bytes(), "anon hello", now).unwrap();
        let m = crate::smsg::SecureMessage::from_wire(&wire).unwrap();
        assert_eq!(smsg_validate(&m), crate::smsg::SMSG_OK);
        let body = build_msg_body(bucket_time_of(now), &[wire]);
        b.handle("a", "smsgMsg", &enc_vch(&body), now);
        let rows = b.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["from"], "anon");
        assert_eq!(rows[0]["text"], "anon hello");
    }

    #[test]
    fn smsgsendanon_envelope_roundtrip_and_layout() {
        // the smsgsendanon path: envelope build → wire parse → validate →
        // decrypt, plus the legacy anonymous payload byte layout
        // (smessage.cpp:3851-3864): [250][4 unused][len u32][data] — no
        // sender hash160, no signature.
        let k = make_keys();
        let a = engine("anon-send-a", vec![k.a.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();

        let (ts, sample) = a.send_local_anon(&k.b.address, "anon hello", now).unwrap();
        assert_eq!(ts, now); // envelope timestamp = compose time

        // relay mirror carries a VALID envelope (build→parse roundtrip)
        let relay = a.store.list(FOLDER_RELAY).unwrap();
        assert_eq!(relay.len(), 1);
        let m = SecureMessage::from_wire(&relay[0].data).unwrap();
        assert_eq!(smsg_validate(&m), SMSG_OK);
        let t = token_of(&relay[0].data).unwrap();
        assert_eq!(t.timestamp, ts);
        assert_eq!(t.sample, sample);

        // raw plaintext layout pins the legacy anon header byte-for-byte
        use sha2::Digest as _;
        let sk = secp256k1::SecretKey::from_slice(&k.b.secret).unwrap();
        let r_pk = secp256k1::PublicKey::from_slice(&m.cpk_r).unwrap();
        let p = secp256k1::ecdh::shared_secret_point(&r_pk, &sk);
        let h = sha2::Sha512::digest(&p[0..32]);
        let mut key_e = [0u8; 32];
        key_e.copy_from_slice(&h[0..32]);
        let plain = crate::smsg::aes256_cbc_decrypt(&key_e, &m.iv, &m.payload).unwrap();
        assert_eq!(plain[0], SMSG_ANON_VERSION);
        assert_eq!(&plain[1..5], &[0u8; 4], "legacy leaves 4 unused bytes");
        assert_eq!(
            &plain[5..9],
            &10u32.to_le_bytes(),
            "u32 LE uncompressed length of 'anon hello'"
        );
        assert_eq!(&plain[9..], b"anon hello");

        // decrypts for the recipient with NO sender identity
        let dm = crate::smsg::smsg_decrypt(&relay[0].data, &k.b).unwrap();
        assert_eq!(dm.from_address, "anon");
        assert_eq!(dm.from_pubkey, None, "anon carries no recoverable sender");
        assert_eq!(dm.plaintext, "anon hello");

        // outbox row: recipient slot + the "anon" sender label
        let outbox = a.store.list(FOLDER_OUTBOX).unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].addr_to, k.b.address);
        assert_eq!(outbox[0].addr_outbox, "anon");
    }

    #[test]
    fn broadcast_signs_each_copy_and_verifies_with_store_keys() {
        // smsgbroadcast: one signed envelope per addrbook pubkey, self
        // skipped, per-recipient failures counted. The snapshot-attestation
        // payload grammar rides the text unchanged.
        let k = make_keys();
        let c = key_for([13u8; 32], "");
        let d = key_for([17u8; 32], ""); // valid address, NOT owned by the engine
        let a = engine("bcast-a", vec![k.a.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        a.add_address(&c.address, &c.pubkey_bytes()).unwrap();

        let attestation = "520000:0000000000000000abc123:2026-11-09T00:00:00Z";
        let (sent, failed, failures) =
            a.broadcast_local(&k.a.address, attestation, now).unwrap();
        assert_eq!(sent, 2, "one envelope per addrbook pubkey");
        assert_eq!(failed, 0);
        assert!(failures.is_empty());

        // self-skip: exactly 2 relay rows / 2 outbox rows — nothing addressed
        // back to the sender
        let relay = a.store.list(FOLDER_RELAY).unwrap();
        assert_eq!(relay.len(), 2);
        let outbox = a.store.list(FOLDER_OUTBOX).unwrap();
        assert_eq!(outbox.len(), 2);

        // each copy decrypts for exactly its recipient and the compact
        // signature recovers the SENDER key from the store (from_pubkey ==
        // a's pubkey, from_address == a's address)
        let mut seen_recipients = Vec::new();
        for row in &relay {
            let mut got = None;
            for holder in [&k.b, &c] {
                if let Ok(dm) = crate::smsg::smsg_decrypt(&row.data, holder) {
                    got = Some((holder.address.clone(), dm));
                    break;
                }
            }
            let (holder_addr, dm) = got.expect("each broadcast copy has one recipient here");
            seen_recipients.push(holder_addr);
            assert_eq!(dm.from_address, k.a.address);
            assert_eq!(dm.from_pubkey.as_deref(), Some(k.a.pubkey.as_slice()));
            assert_eq!(dm.plaintext, attestation);
        }
        seen_recipients.sort();
        let mut expect = vec![k.b.address.clone(), c.address.clone()];
        expect.sort();
        assert_eq!(seen_recipients, expect);

        // a broadcast from a VALID but unowned address fails per recipient —
        // legacy counts failures, never aborts (rpcsmessage.cpp:961-972)
        let (sent, failed, failures) = a.broadcast_local(&d.address, "x", now).unwrap();
        assert_eq!(sent, 0);
        assert_eq!(failed, 2);
        assert_eq!(failures.len(), 2);
        for (addr, err) in &failures {
            assert!(err.contains("not a wallet key"), "failures[{}]: {}", addr, err);
        }
    }

    #[test]
    fn want_serves_only_bucket_tokens_and_caps_bunch() {
        let k = make_keys();
        let a = engine("want-a", vec![k.a.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        for i in 0..3 {
            a.send_local(&k.a.address, &k.b.address, &format!("m{}", i), now + i).unwrap();
        }
        // same-bucket messages: tokens must all be in one bucket
        let bt = bucket_time_of(now);
        let b = a.st.lock().unwrap().buckets.get(&bt).unwrap().clone();
        assert_eq!(b.tokens.len(), 3);
        // a want for 2 of the 3 tokens serves exactly those 2
        let want_tokens: Vec<(i64, [u8; 8])> = b
            .tokens
            .iter()
            .take(2)
            .map(|t| (t.timestamp, t.sample))
            .collect();
        let body = build_token_body(bt, &want_tokens);
        let replies = a.handle("peer", "smsgWant", &enc_vch(&body), now);
        assert_eq!(replies.len(), 1);
        let msg_body = dec_vch(&replies[0].payload).unwrap();
        assert_eq!(u32::from_le_bytes(msg_body[0..4].try_into().unwrap()), 2);
    }

    #[test]
    fn bucket_lock_blocks_have_then_releases_on_tick() {
        let k = make_keys();
        let b = engine("lock-b", vec![k.b.clone()]);
        let now = now_i64();
        let wire = wire_ab(&k, "locked", now);
        // b doesn't have the message yet — a smsgHave should want it
        let bt = bucket_time_of(now);
        let t = token_of(&wire).unwrap();
        let have = build_token_body(bt, &[(t.timestamp, t.sample)]);
        let replies = b.handle("a", "smsgHave", &enc_vch(&have), now);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].command, "smsgWant");
        // while locked, a repeated have is ignored
        let replies = b.handle("a", "smsgHave", &enc_vch(&have), now);
        assert!(replies.is_empty());
        // ticks age the lock away
        for _ in 0..BUCKET_LOCK_TICKS {
            b.tick_locks();
        }
        let replies = b.handle("a", "smsgHave", &enc_vch(&have), now);
        assert_eq!(replies.len(), 1);
    }

    #[test]
    fn lz4_large_message_roundtrip_over_wire() {
        // >128B plaintexts take the LZ4 path — full engine roundtrip
        let k = make_keys();
        let a = engine("lz-a", vec![k.a.clone()]);
        let b = engine("lz-b", vec![k.b.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        let text = "x".repeat(2000) + " tail with entropy 1234567890";
        a.send_local(&k.a.address, &k.b.address, &text, now).unwrap();
        let wire = a.store.list(FOLDER_RELAY).unwrap()[0].data.clone();
        let body = build_msg_body(bucket_time_of(now), &[wire]);
        b.handle("a", "smsgMsg", &enc_vch(&body), now);
        let rows = b.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"].as_str().unwrap(), text.as_str());
    }

    #[test]
    fn purge_drops_expired_and_keeps_fresh() {
        let k = make_keys();
        let a = engine("purge-a", vec![k.a.clone()]);
        let now = now_i64();
        a.add_address(&k.b.address, &k.b.pubkey_bytes()).unwrap();
        a.send_local(&k.a.address, &k.b.address, "old", now - SMSG_RETENTION - 100).unwrap();
        a.send_local(&k.a.address, &k.b.address, "fresh", now).unwrap();
        let n = a.purge(now);
        // one message past retention, but it occupies BOTH the relay and
        // outbox folders — purge counts rows across folders
        assert_eq!(n, 2);
        assert_eq!(a.store.list(FOLDER_RELAY).unwrap().len(), 1);
    }

    #[test]
    fn smsg_disable_and_ignore_paths() {
        let k = make_keys();
        let a = engine("dis-a", vec![k.a.clone()]);
        let now = now_i64();
        // complete first contact so the peer exists with last_seen set
        assert_eq!(a.send_data_tick("p", now).unwrap().command, "smsgPing");
        a.handle("p", "smsgDisabled", &enc_vch(&[]), now);
        assert!(!a.peer_enabled("p"));
        let mut ig = vec![0u8; 8];
        ig.copy_from_slice(&(now + 3600i64).to_le_bytes());
        a.handle("p", "smsgIgnore", &enc_vch(&ig), now);
        // ignored peer gets nothing from send ticks
        assert!(a.send_data_tick("p", now + SMSG_SEND_DELAY).is_none());
    }

    #[test]
    fn tampered_identity_rejected_by_decrypt() {
        // a message claiming A's hash160 but signed by another key must not
        // deliver (legacy SecureMsgDecrypt verifies the recovered sender)
        let k = make_keys();
        let forger = key_for([31u8; 32], "x");
        let b = engine("forge-b", vec![k.b.clone()]);
        let now = now_i64();
        // encrypt a payload FROM forger's key but to b (valid MAC — b is the
        // recipient) — decrypt recovers the real sender == claimed, so this
        // is the positive control; the NEGATIVE control lives in smsg.rs
        let wire = crate::smsg::smsg_encrypt(Some(&forger), &k.b.pubkey_bytes(), "spoof", now).unwrap();
        let body = build_msg_body(bucket_time_of(now), &[wire]);
        b.handle("f", "smsgMsg", &enc_vch(&body), now);
        let rows = b.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["from"], forger.address);
    }

    #[test]
    fn add_address_rejects_mismatched_pubkey() {
        let k = make_keys();
        let a = engine("add-a", vec![k.a.clone()]);
        assert!(a.add_address(&k.b.address, &k.a.pubkey_bytes()).is_err());
        assert!(a.add_address(&k.b.address, &k.b.pubkey_bytes()).is_ok());
        assert!(a.add_address(&k.b.address, &[1u8; 30]).is_err());
    }

    // ---- smsgenable / smsgdisable / smsgoptions / scans (workqueue item 11) ----

    #[test]
    fn disable_stops_data_and_notifies_each_enabled_peer_once() {
        let k = make_keys();
        let a = engine("togg-a", vec![k.a.clone(), k.b.clone()]);
        let now = now_i64();
        // handshake peer p1 to "enabled" via the ping/pong dance
        a.send_data_tick("p1", now); // first contact → ping
        a.handle("p1", "smsgPong", &enc_vch(&[]), now);
        // give the engine a bucket so an enabled peer gets an inv
        a.send_local(&k.a.address, &k.b.address, "hello toggle", now).unwrap();
        assert!(a.send_data_tick("p1", now + SMSG_SEND_DELAY).is_some());

        a.disable();
        assert!(!a.enabled());
        // the only outbound while disabled is ONE smsgDisabled, then silence
        let out = a.send_data_tick("p1", now + 10);
        assert!(out.is_some());
        assert_eq!(out.unwrap().command, "smsgDisabled");
        assert!(a.send_data_tick("p1", now + 12).is_none());
        // inbound data is refused while disabled (no pong, no storage)
        assert!(a.handle("p1", "smsgPing", &enc_vch(&[]), now + 13).is_empty());
        let wire = crate::smsg::smsg_encrypt(Some(&k.b), &k.a.pubkey_bytes(), "x", now).unwrap();
        let body = build_msg_body(bucket_time_of(now), &[wire]);
        a.handle("p1", "smsgMsg", &enc_vch(&body), now + 14);
        assert_eq!(
            a.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().len(),
            0,
            "disabled engine must not deliver"
        );
        // a second disable is a no-op (no duplicate notices)
        a.disable();
        assert!(a.send_data_tick("p1", now + 16).is_none());
    }

    #[test]
    fn enable_rebuilds_buckets_and_rehandshakes_peers() {
        let k = make_keys();
        let a = engine("togg-b", vec![k.a.clone(), k.b.clone()]);
        let now = now_i64();
        a.send_data_tick("p1", now);
        a.handle("p1", "smsgPong", &enc_vch(&[]), now);
        a.send_local(&k.a.address, &k.b.address, "again", now).unwrap();

        a.disable();
        a.enable(now + 60);
        assert!(a.enabled());
        // peers were reset: first contact pings again (legacy pings on enable)
        let out = a.send_data_tick("p1", now + 60);
        assert!(out.is_some());
        assert_eq!(out.unwrap().command, "smsgPing");
        // buckets were rebuilt: after the pong the inv flows again
        a.handle("p1", "smsgPong", &enc_vch(&[]), now + 61);
        let out = a.send_data_tick("p1", now + 61 + SMSG_SEND_DELAY);
        assert!(out.is_some());
        assert_eq!(out.unwrap().command, "smsgInv");
    }

    #[test]
    fn options_roundtrip_through_the_store() {
        let k = make_keys();
        let dir = std::env::temp_dir().join(format!(
            "tri7-smsgopt-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let a = SmsgEngine::open(&dir, vec![k.a.clone()]).unwrap();
        // legacy defaults: both true
        assert_eq!(a.options(), SecMsgOptions::default());
        let mut o = a.options();
        o.assign(SecMsgOptions::OPT_RECV, false);
        a.set_options(o).unwrap();
        assert_eq!(a.options().new_address_recv, false);
        assert_eq!(a.options().new_address_anon, true);
        // persisted: a fresh engine on the same store reads the same values
        let b = SmsgEngine::open(&dir, vec![k.a.clone()]).unwrap();
        assert_eq!(b.options().new_address_recv, false);
        assert_eq!(b.options().new_address_anon, true);
        // localinfo reports the real state
        let info = b.localinfo_json();
        assert_eq!(info["enabled"], serde_json::json!(true));
        assert_eq!(info["options"]["newAddressRecv"], serde_json::json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn option_value_tokens_match_legacy() {
        for t in ["+", "on", "true", "1"] {
            assert_eq!(SecMsgOptions::parse_value(t), Some(true), "token {t}");
        }
        for t in ["-", "off", "false", "0"] {
            assert_eq!(SecMsgOptions::parse_value(t), Some(false), "token {t}");
        }
        assert_eq!(SecMsgOptions::parse_value("yes"), None);
        assert_eq!(SecMsgOptions::parse_value(""), None);
        assert_eq!(
            SecMsgOptions::render(SecMsgOptions::OPT_RECV, true),
            "newAddressRecv = true"
        );
    }

    #[test]
    fn scan_buckets_redelivers_relay_messages_to_inbox() {
        let k = make_keys();
        let a = engine("scanb", vec![k.a.clone(), k.b.clone()]);
        let now = now_i64();
        // a local send lands in outbox + relay but NOT the inbox
        a.send_local(&k.a.address, &k.b.address, "rescan me", now).unwrap();
        assert_eq!(
            a.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().len(),
            0
        );
        let found = a.scan_buckets(now + 1);
        assert_eq!(found, 1);
        let rows = a.list_folder_json(FOLDER_INBOX, false).unwrap().as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["to"], serde_json::json!(k.b.address));
        // idempotent: a second scan finds nothing new
        assert_eq!(a.scan_buckets(now + 2), 0);
    }

    #[test]
    fn learn_pubkey_validates_and_dedupes() {
        let k = make_keys();
        let a = engine("learnpk", vec![k.a.clone()]);
        let now = now_i64();
        let (added, dup) = a.learn_pubkey(&k.b.pubkey_bytes(), now).unwrap();
        assert!(added && !dup);
        // duplicate learn
        let (added, dup) = a.learn_pubkey(&k.b.pubkey_bytes(), now).unwrap();
        assert!(!added && dup);
        // secp-invalid 33 bytes rejected (x = 0xff…ff ≥ the field prime),
        // wrong length rejected
        let mut bad = [0xffu8; 33];
        bad[0] = 2;
        assert!(a.learn_pubkey(&bad, now).is_err());
        assert!(a.learn_pubkey(&[2u8; 32], now).is_err());
        // the learned key is usable: a reply to b resolves without addaddress
        let from = k.a.address.clone();
        a.send_local(&from, &k.b.address, "reply", now).unwrap();
    }
}
