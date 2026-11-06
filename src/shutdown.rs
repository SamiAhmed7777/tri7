//! Process-wide shutdown flag.
//!
//! The RPC layer is shared by the daemon and by tools; asking for shutdown has
//! to be expressible from anywhere without threading a handle through every
//! call site. The daemon's main loop polls this flag exactly like its SIGINT
//! handler, so `stop` and SIGTERM take the same resumable path.

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Ask the node to stop after the current iteration.
pub fn request() {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Has a stop been requested?
pub fn requested() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_sticky() {
        // NOTE: process-global; this test only asserts the transition, never
        // that it is still false elsewhere.
        request();
        assert!(requested());
    }
}
