//! Process-wide shutdown flag.
//!
//! The RPC layer is shared by the daemon and by tools; asking for shutdown has
//! to be expressible from anywhere without threading a handle through every
//! call site. The daemon's main loop polls this flag exactly like its SIGINT
//! handler, so `stop` and SIGTERM take the same resumable path.

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// (TRI7-RPC-02) set ONLY by the RPC `stop` verb. The supervisor treats an
/// exit that follows it as intentional (never a restart); SIGTERM, GUI exit
/// and the supervisor's own cooperative stops use `request()` alone, so they
/// cannot be mistaken for an operator stop.
static RPC_STOP: AtomicBool = AtomicBool::new(false);

/// Ask the node to stop after the current iteration.
pub fn request() {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Ask the node to stop AND record that an operator's RPC `stop` asked for
/// it (the supervisor attributes the exit to the operator, not a crash).
pub fn request_rpc() {
    RPC_STOP.store(true, Ordering::Relaxed);
    request();
}

/// Has a stop been requested?
pub fn requested() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

/// Was the pending/last stop an explicit RPC `stop`?
pub fn rpc_stopped() -> bool {
    RPC_STOP.load(Ordering::Relaxed)
}

/// Clear a pending stop. MUST run at the start of every node run: the flag is
/// process-global, and in the single-exe GUI a new run inherits the previous
/// run's `stop` (RPC `stop` would otherwise make every supervisor respawn
/// exit on its first loop iteration, burning all restarts).
pub fn reset() {
    SHUTDOWN.store(false, Ordering::Relaxed);
    RPC_STOP.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_sticky_until_the_next_run_resets_it() {
        // NOTE: process-global; this test only asserts the transition, never
        // that it is still false elsewhere.
        request();
        assert!(requested());
        // TRI7-RPC-02: every node run resets it, so a respawn must not exit
        // on the previous run's stop.
        reset();
        assert!(!requested());
    }
}
