//! Stake/payment alert decision (Sami: "alert to the system tray every time
//! they receive a new stake, just like the old wallet — and the user decides
//! whether to receive those push notifications").
//!
//! Pure decision core (unit-tested); the side effect (system toast + in-app
//! banner) lives in notify.rs / the Overview renderer.
//!
//! Rules:
//! - first observation establishes the baseline — never fires (a fresh GUI
//!   must not scream the whole balance on open)
//! - fires only on an INCREASE of at least one dust-minimum output (546 sats)
//!   while the daemon is fully synced (never during the from-zero walk, where
//!   balances legitimately climb from zero)
//! - sends/decreases never fire
//! - `enabled=false` (user preference) suppresses everything

/// consensus dust minimum — anything smaller isn't a real credit
pub const MIN_ALERT_DELTA_SATS: i64 = 546;

pub fn stake_alert_decision(
    prev: Option<i64>,
    new: i64,
    syncing: bool,
    enabled: bool,
) -> Option<i64> {
    if !enabled || syncing {
        return None;
    }
    let prev = prev?;
    if new > prev && new - prev >= MIN_ALERT_DELTA_SATS {
        Some(new - prev)
    } else {
        None
    }
}

/// Fire the OS-level notification. Windows: a real toast via WinRT
/// (tauri-winrt-notification). Other platforms: no-op for now (the in-app
/// banner still shows; the daemon-side event remains visible in the log).
pub fn fire_system_notification(title: &str, text: &str) {
    #[cfg(target_os = "windows")]
    {
        // toast on the user's desktop; failures are non-fatal (in-app banner
        // already carries the message). App id names the source in Action
        // Center — use the product name so the toast is branded.
        let _ = tauri_winrt_notification::Toast::new("Cryptographic Triangles")
            .title(title)
            .text1(text)
            .show();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (title, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_never_fires() {
        assert_eq!(stake_alert_decision(None, 3_036_990_591, false, true), None);
    }

    #[test]
    fn increase_while_synced_fires() {
        assert_eq!(
            stake_alert_decision(Some(1_000_000), 1_050_000, false, true),
            Some(50_000)
        );
    }

    #[test]
    fn increase_while_syncing_is_suppressed() {
        assert_eq!(
            stake_alert_decision(Some(1_000_000), 1_050_000, true, true),
            None
        );
    }

    #[test]
    fn disabled_suppresses_everything() {
        assert_eq!(
            stake_alert_decision(Some(1_000_000), 1_050_000, false, false),
            None
        );
    }

    #[test]
    fn decrease_never_fires() {
        assert_eq!(
            stake_alert_decision(Some(2_000_000), 1_500_000, false, true),
            None
        );
    }

    #[test]
    fn subdust_increase_ignored() {
        assert_eq!(
            stake_alert_decision(Some(1_000_000), 1_000_400, false, true),
            None
        );
    }
}
