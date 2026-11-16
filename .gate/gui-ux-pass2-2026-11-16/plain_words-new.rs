//! Plain-language layer (UX pass 2) — the first-run doors, the empty-balance
//! state and the send/receive wording, kept display-independent so
//! `cargo test --lib gui::` can assert the flow without a display.
//!
//! The rule for every string here: say what the user GETS, never how the code
//! works. Technical detail (derivation paths, inputs, byte sizes, txids) stays
//! available — as hover text on the same control — but it never leads.
//!
//! Every English constant is an i18n key: `i18n::tr(lang, plain_words::SEND_TO)`
//! renders it, so pass 1's language coverage covers pass 2's copy too. Adding a
//! constant here REQUIRES adding it to `i18n::PHASE2_KEYS` (the completeness
//! test in tests.rs fails otherwise).

use crate::gui::state as s;

// ---------------------------------------------------------------------------
// First run — the three doors
// ---------------------------------------------------------------------------

pub const WELCOME_TITLE: &str = "Welcome to Triangles";

pub const WELCOME_BODY: &str = "A wallet holds your keys and your coins. Creating one takes about a \
     minute: you get 24 words to write down, and those words are the only way to get your coins \
     back if this computer is lost or broken.";

pub const CREATE_WALLET: &str = "Create a new wallet";
pub const RESTORE_WALLET: &str = "Restore a wallet you already have";
pub const OPEN_WALLET_FILE: &str = "Open a wallet file";

/// One first-run door: the button label and the single line explaining it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryPoint {
    pub label: &'static str,
    pub hint: &'static str,
}

/// The three doors a brand-new user is offered, in the order they are shown.
/// "Create" first (the common case), then restore, then the escape hatch for
/// anyone who already has a wallet.dat and no phrase.
pub fn first_run_entry_points() -> [EntryPoint; 3] {
    [
        EntryPoint {
            label: CREATE_WALLET,
            hint: "New to Triangles. You will see 24 words to write down.",
        },
        EntryPoint {
            label: RESTORE_WALLET,
            hint: "You have the 24 words (or 12) from a wallet created earlier.",
        },
        EntryPoint {
            label: OPEN_WALLET_FILE,
            hint: "You have a wallet.dat file on this computer or a USB stick.",
        },
    ]
}

/// TRUE while the welcome menu owns the screen: a first run is pending and
/// nothing else (a loaded wallet, an active seed flow) has taken over yet.
pub fn welcome_visible(welcome_pending: bool, loaded: bool, seed_flow_active: bool) -> bool {
    welcome_pending && !loaded && !seed_flow_active
}

/// The line under the welcome buttons: what happens to the wallet file.
pub fn welcome_footer(wallet_path: &str) -> String {
    if wallet_path.trim().is_empty() {
        "The wallet is saved as a file on this computer. You can move or copy it at any time."
            .to_string()
    } else {
        format!(
            "The wallet is saved as a file on this computer — {}. You can move or copy it at any time.",
            wallet_path.trim()
        )
    }
}

// ---------------------------------------------------------------------------
// Empty balance — a loaded wallet that simply has no coins yet
// ---------------------------------------------------------------------------

pub const NO_COINS_TITLE: &str = "No coins yet";
pub const NO_COINS_BODY: &str = "Coins you receive will appear here. Open Receive Coins to get an \
     address you can share.";

/// The same situation on the SEND screen: a wallet with nothing to spend can
/// only produce a disabled form, so it says so and points at Receive instead.
pub const NO_COINS_SEND: &str = "You have no coins to send yet. Open Receive Coins to get an \
     address you can share — coins sent to it appear on the Overview.";

/// Empty-balance notice for a LOADED wallet whose balances are all zero.
/// `None` once any coin exists (confirmed, staking, unconfirmed or immature) —
/// the balance card then speaks for itself.
pub fn empty_balance_notice(loaded: bool, total_sats: i64) -> Option<(&'static str, &'static str)> {
    if loaded && total_sats <= 0 {
        Some((NO_COINS_TITLE, NO_COINS_BODY))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Send — what it costs, in words a first-time user can act on
// ---------------------------------------------------------------------------

pub const SEND_TO: &str = "Send to:";
pub const HOW_MUCH: &str = "How much:";
pub const NETWORK_FEE: &str = "Network fee:";
pub const FEE_HINT: &str =
    "Optional. Leave it empty and the wallet pays the network's standard fee.";
pub const AMOUNT_HINT: &str = "Up to 6 decimals — 0.000001 is the smallest amount.";
pub const REVIEW_SEND: &str = "Review before sending";
pub const SEND_NOW: &str = "Send now";

/// "What this send costs" line. Fee + where it comes from + the network's
/// standard fee, in one sentence instead of a bare number and the word
/// "inputs" (which means nothing to the person paying).
///
/// `sources` is the number of the user's own addresses the payment draws on —
/// called "inputs" everywhere else in the codebase.
pub fn fee_cost_line(est_sats: i64, standard_sats: i64, sources: usize) -> String {
    let from = match sources {
        0 | 1 => "taken from your own address".to_string(),
        n => format!("taken from {} of your addresses", n),
    };
    format!(
        "{} {} in total, {}. The network's standard fee is {} {}.",
        s::format_tri(est_sats),
        s::TICKER,
        from,
        s::format_tri(standard_sats),
        s::TICKER
    )
}

/// Amount field: TRI is a whole-number-plus-decimals currency; "6 decimals" on
/// its own does not tell a first-time user what the smallest amount is.
pub fn amount_hint() -> &'static str {
    AMOUNT_HINT
}

/// The send screen's one-sentence promise about leaving the machine. Kept as
/// the footer under the form so nobody clicks "Send now" wondering what it
/// transmits.
pub const LOCAL_SIGNING_LINE: &str =
    "Nothing leaves this computer until you click Send now. Your wallet signs the payment here, \
     then hands it to the network.";

// ---------------------------------------------------------------------------
// Receive — the address is the product; the derivation path is the detail
// ---------------------------------------------------------------------------

pub const NEW_ADDRESS: &str = "Create a new address";
pub const COPY_ADDRESS: &str = "Copy address";
pub const YOUR_ADDRESS: &str = "Your address to share";

/// Backup explanation for a seeded wallet. The old line led with
/// "m/44'/2222'/0'/0/{n}" — true, and useless to the person reading it. The
/// path still exists, as hover text ([`seed_derivation_path`]), for anyone who
/// wants it.
pub const SEED_BACKUP_LINE: &str = "Backed up by your 24 words. New addresses are created from \
     them automatically — keep the words safe and every address (and every coin) can be restored \
     on any computer.";

/// The derivation path, for the hover text beside [`SEED_BACKUP_LINE`].
pub fn seed_derivation_path(next_index: u32) -> String {
    format!("m/44'/2222'/0'/0/{}", next_index)
}

/// Backup explanation for a wallet with individual (pre-HD) keys — no phrase
/// can ever reproduce those keys, so the file is the backup.
pub const FILE_BACKUP_LINE: &str = "This wallet has no 24-word backup: copy its wallet file to \
     keep the keys safe. That file is the only copy — without it the coins cannot be recovered.";
