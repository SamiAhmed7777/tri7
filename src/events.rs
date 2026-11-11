//! Node event push: Server-Sent Events (SSE) on the RPC listener — the
//! pure-Rust replacement for the legacy daemon's zmqpublishnotifier
//! (hashblock/hashtx pub, v6-extra parity class). No C-linked ZMQ dep (the
//! windows-gnu cross-build pain stays out); consumers get the same facts on
//! `GET /events` as `data: {json}\n\n` frames.
//!
//! SECURITY POSTURE — inherited, not re-decided: /events rides the SAME
//! listener as the JSON-RPC surface, which binds loopback-only
//! (rpc::bind_loopback refuses anything else — same trust model as the
//! legacy daemon: "any local process may read"). No auth, no TLS, no origin
//! checks are added here; if the RPC bind policy ever changes, /events
//! changes with it automatically because it has no listener of its own.
//! Never expose the RPC bind to a network interface without re-reviewing
//! this surface too.
//!
//! CONCURRENCY LAW (why validation can never stall):
//!   - every subscriber owns ONE bounded std::sync::mpsc channel;
//!   - producers publish with `try_send` ONLY — a slow or dead consumer
//!     never blocks a producer thread (tip advance, mempool accept, staking
//!     toggle, the RPC accept loop);
//!   - a subscriber whose channel is full is treated as disconnected and
//!     dropped on the spot (dropped events, not dropped liveness);
//!   - the SSE writer thread detects disconnect (write error / socket EOF /
//!     channel-closed) and removes itself from the hub;
//!   - subscribers are capped (16) — a cap-full subscribe is refused with an
//!     honest 503 before any SSE headers are written.
//!
//! When no ops engine is live (no hub), the endpoint answers an honest 404
//! (rpc::serve handles that side); the producers here simply no-op.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

/// Maximum concurrent SSE subscribers (loopback consumer cap).
pub const SUBSCRIBER_CAP: usize = 16;
/// Frames a subscriber may queue before it is dropped as too slow.
const CHANNEL_BOUND: usize = 64;
/// SSE keepalive interval (a comment frame — clients ignore it; the write
/// also doubles as a liveness probe: a gone consumer errors the write).
pub const KEEPALIVE_SECS: u64 = 15;

/// One live event fan-out hub. Cheap to clone via Arc.
pub struct EventHub {
    subs: Mutex<HashMap<u64, SyncSender<String>>>,
    next_id: AtomicU64,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    pub fn new() -> Self {
        EventHub {
            subs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a subscriber. None = cap reached (caller answers 503).
    /// The returned id MUST be passed to unsubscribe when the stream ends.
    pub fn subscribe(&self) -> Option<(u64, Receiver<String>)> {
        let mut subs = self.subs.lock().ok()?;
        if subs.len() >= SUBSCRIBER_CAP {
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_BOUND);
        subs.insert(id, tx);
        Some((id, rx))
    }

    /// Remove a subscriber (SSE thread cleanup on disconnect/cap-drop).
    pub fn unsubscribe(&self, id: u64) {
        if let Ok(mut subs) = self.subs.lock() {
            subs.remove(&id);
        }
    }

    /// Fan out one pre-rendered frame. NEVER blocks: `try_send` only; a
    /// full (too slow) or disconnected receiver is dropped from the hub.
    /// Hold time on the lock is one non-blocking try_send per subscriber.
    pub fn publish(&self, frame: String) {
        let Ok(mut subs) = self.subs.lock() else {
            return;
        };
        let mut dead: Vec<u64> = Vec::new();
        for (id, tx) in subs.iter() {
            match tx.try_send(frame.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // slow consumer — drop, producers must never wait
                    dead.push(*id);
                }
                Err(TrySendError::Disconnected(_)) => {
                    dead.push(*id);
                }
            }
        }
        for id in dead {
            subs.remove(&id);
        }
    }

    /// Live subscriber count (tests + observability).
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().map(|s| s.len()).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Process-wide live hub (the producers hook deep in the follow loops, where
// no adapter handle is threaded — same pattern as LIVE_STORE/LIVE_ADDRINDEX)
// ---------------------------------------------------------------------------

static LIVE_HUB: OnceLock<Arc<EventHub>> = OnceLock::new();

/// The hub the live ops engine registered. None = no engine has run.
pub fn live_hub() -> Option<Arc<EventHub>> {
    LIVE_HUB.get().cloned()
}

/// Engine-side wiring: get-or-create THE hub for this process. Producers
/// publish through the helpers below; the RPC /events endpoint reaches the
/// same hub via NodeOps::event_hub, so availability tracks the live engine.
pub fn live_hub_or_init() -> Arc<EventHub> {
    LIVE_HUB.get_or_init(|| Arc::new(EventHub::new())).clone()
}

/// Frame shape: one SSE data line + blank line. Payload is compact JSON on
/// a single line (serde_json never emits raw newlines inside strings).
pub fn frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

// ---------------------------------------------------------------------------
// Producer one-liners (no-ops while no engine is live — never block)
// ---------------------------------------------------------------------------

/// A block was accepted and the tip advanced (legacy zmq hashblock).
pub fn publish_block(height: u64, hash_display: &str, time: u64) {
    let payload = serde_json::json!({
        "type": "block", "height": height, "hash": hash_display, "time": time,
    })
    .to_string();
    if let Some(hub) = live_hub() {
        hub.publish(frame(&payload));
    }
}

/// A transaction was accepted into the mempool (legacy zmq hashtx).
pub fn publish_tx(txid: &str, time: u64) {
    let payload = serde_json::json!({
        "type": "tx", "txid": txid, "time": time,
    })
    .to_string();
    if let Some(hub) = live_hub() {
        hub.publish(frame(&payload));
    }
}

/// Staking was switched on/off (tri7 extension; legacy had no equivalent).
pub fn publish_staking(enabled: bool) {
    let payload = serde_json::json!({
        "type": "staking", "enabled": enabled,
    })
    .to_string();
    if let Some(hub) = live_hub() {
        hub.publish(frame(&payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_data_line_plus_blank_line() {
        let f = frame(r#"{"type":"block"}"#);
        assert_eq!(f, "data: {\"type\":\"block\"}\n\n");
    }

    #[test]
    fn producer_helpers_noop_without_a_live_hub() {
        // no live_hub_or_init in this test: must not panic, must not publish
        publish_block(1, "ab".repeat(32).as_str(), 1);
        publish_tx("cd".repeat(32).as_str(), 1);
        publish_staking(true);
        assert!(live_hub().is_none());
    }
}
