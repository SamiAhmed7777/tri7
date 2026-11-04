//! Daemon sync-progress snapshot — Sami's directive: "it should show this on
//! the UI". The follow daemon updates the global snapshot during from-zero
//! (phase transitions + every 1000-block tick); the RPC serves it via
//! `getsyncinfo`; the GUI renders height / percent / rate / ETA live.
//!
//! Lock-free for readers under contention: a single Mutex<Option<Snapshot>>
//! is fine here — writers update at most ~once/second, readers poll ~1Hz.

use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub height: u64,
    pub target_height: u64,
    pub blocks_per_sec: f64,
    pub eta_secs: u64,
    pub phase: SyncPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    /// walking the header chain from genesis (from-zero)
    HeaderWalk,
    /// fetching + validating bodies behind the walk
    BodyFetch,
    /// caught up — following the network live
    Follow,
}

impl SyncPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncPhase::HeaderWalk => "header-walk",
            SyncPhase::BodyFetch => "body-fetch",
            SyncPhase::Follow => "follow",
        }
    }
}

static CUR: Mutex<Option<Snapshot>> = Mutex::new(None);

/// Publish the current sync snapshot (daemon side).
pub fn set(s: Snapshot) {
    if let Ok(mut g) = CUR.lock() {
        *g = Some(s);
    }
}

/// Clear the snapshot (daemon shutdown / no sync in progress).
pub fn clear() {
    if let Ok(mut g) = CUR.lock() {
        *g = None;
    }
}

/// Read the current snapshot (RPC / GUI side). None = no from-zero run has
/// reported — the daemon is either a plain follower or not running.
pub fn snapshot() -> Option<Snapshot> {
    CUR.lock().ok().and_then(|g| *g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_set_get_clear_roundtrip() {
        clear();
        assert!(snapshot().is_none());
        set(Snapshot {
            height: 1_000,
            target_height: 2_204_751,
            blocks_per_sec: 914.0,
            eta_secs: 2_400,
            phase: SyncPhase::BodyFetch,
        });
        let s = snapshot().expect("snapshot present");
        assert_eq!(s.height, 1_000);
        assert_eq!(s.target_height, 2_204_751);
        assert!((s.blocks_per_sec - 914.0).abs() < 1e-9);
        assert_eq!(s.phase, SyncPhase::BodyFetch);
        assert_eq!(s.phase.as_str(), "body-fetch");
        clear();
        assert!(snapshot().is_none());
    }

    #[test]
    fn phase_strings() {
        assert_eq!(SyncPhase::HeaderWalk.as_str(), "header-walk");
        assert_eq!(SyncPhase::Follow.as_str(), "follow");
    }
}
