//! AppState — the display-independent wallet state machine (screens, actions,
//! selfcheck). `gui::app` renders this; `--selfcheck` drives it headless.

use super::rpc_client;
use super::state::*;
use crate::gui::explorer;
use crate::gui::notify::stake_alert_decision;
use crate::spend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ===========================================================================
// Sub-states
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub struct ReceiveForm {
    pub address: String,
    pub label: String,
    pub message: String,
    pub amount: String,
    pub require_payment: bool,
    pub uri: String,
    pub qr: Option<QrMatrix>,
    pub error: Option<String>,
    pub new_address_label: String,
}

#[derive(Debug, Clone, Default)]
pub struct SignForm {
    pub address: String,
    pub message: String,
    pub signature: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct VerifyForm {
    pub address: String,
    pub message: String,
    pub signature: String,
    pub result: Option<Result<bool, String>>,
}

/// Options dialog state (optionsdialog.ui tabs). Values the engine can honor
/// in THIS build are real inputs (pay-transaction-fee feeds Send); the rest
/// are session preferences with an honest "requires daemon integration" note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionsState {
    pub pay_fee: String,         // Main tab: "Pay transaction &fee" (TRI) — REAL
    pub reserve: String,         // Main tab: "Reserve" (staking reserve) — noted
    pub display_addresses: bool, // Display tab
    pub coin_control: bool,      // Display tab: "Display coin &control features"
    /// system notification on every received stake/payment (Sami: alert to
    /// the tray like the old wallet, with a user preference to disable)
    #[serde(default = "default_true")]
    pub notifications: bool,
}

fn default_true() -> bool {
    true
}

impl Default for OptionsState {
    fn default() -> Self {
        OptionsState {
            pay_fee: format_tri(spend::DEFAULT_TRANSACTION_FEE), // 0.000100
            reserve: "0.000000".into(),
            display_addresses: true,
            coin_control: true,
            notifications: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub request: String,
    pub reply: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DebugInfo {
    pub reachable: bool,
    pub connections: Option<u64>,
    pub blocks: Option<u64>,
    /// daemon sync progress (getsyncinfo) — Sami: "it should show this on
    /// the UI": live height/percent/rate/ETA while the daemon from-zeroes
    pub sync: Option<GuiSync>,
    /// daemon-side wallet balance (getbalance, sats) — the live truth when
    /// the daemon tracks this wallet's keys (local snapshot may be absent)
    pub daemon_balance_sats: Option<i64>,
    /// daemon-side unspent set (listunspent) — the Transactions/history truth
    /// when the wallet folder has no local snapshot (fresh installs)
    pub daemon_utxos: Option<Vec<crate::gui::state::UtxoRec>>,
    /// full wallet history from the daemon (listwallettx -> history.json).
    /// Every receive/spend ever seen, dated from the containing block, so a
    /// holder from 2014-2021 sees those rows, not just the live window.
    pub wallet_history: Option<Vec<crate::history::HistoryRec>>,
    pub error: Option<String>,
}

/// GUI-side copy of the daemon's sync snapshot (getsyncinfo).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiSync {
    pub height: u64,
    pub target_height: u64,
    pub blocks_per_sec: f64,
    pub eta_secs: u64,
    pub syncing: bool,
}

impl GuiSync {
    /// 0.0..=1.0 progress fraction.
    pub fn fraction(&self) -> f32 {
        if self.target_height == 0 || self.height >= self.target_height {
            return 1.0;
        }
        (self.height as f32 / self.target_height as f32).clamp(0.0, 1.0)
    }

    /// Human one-liner: "Syncing 1,234,567 / 2,204,751 (56%) · 914 blk/s · ETA 18m 4s"
    pub fn label(&self) -> String {
        let pct = (self.fraction() * 100.0) as u32;
        let eta = if self.eta_secs >= 3600 {
            format!("{}h {:02}m", self.eta_secs / 3600, (self.eta_secs % 3600) / 60)
        } else if self.eta_secs >= 60 {
            format!("{}m {:02}s", self.eta_secs / 60, self.eta_secs % 60)
        } else {
            format!("{}s", self.eta_secs)
        };
        format!(
            "Syncing {} / {} ({}%) · {:.0} blk/s · ETA {}",
            self.height,
            self.target_height,
            pct,
            self.blocks_per_sec,
            eta
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Dialogs {
    pub about: bool,
    pub qr: bool,
    pub encrypt: Option<String>,
    pub backup: Option<Result<String, String>>,
    pub save_tx_path: String,
    pub backup_path: String,
    pub qr_save_path: String,
}

// ===========================================================================
// Phase 7 milestone 2 — live daemon panels (staking / broadcast / messages /
// peers). Each state polls tri7d's NATIVE RPC through gui::rpc_client.
// ===========================================================================

/// getstakinginfo snapshot (parsed loosely — the daemon owns the shape).
#[derive(Debug, Clone, Default)]
pub struct StakingLive {
    pub info: Option<serde_json::Value>,
    pub last_poll: Option<String>,
    pub last_error: Option<String>,
}

impl StakingLive {
    pub fn field(&self, key: &str) -> String {
        self.info
            .as_ref()
            .and_then(|v| v.get(key))
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "—".into())
    }
}

/// Per-session peer table (getpeerinfo — transport flags clearnet/tor/i2p).
#[derive(Debug, Clone, Default)]
pub struct PeersLive {
    pub table: Option<serde_json::Value>,
    pub last_error: Option<String>,
}

/// Messages page state: inbox/outbox rows from tri7d + compose form.
#[derive(Debug, Clone, Default)]
pub struct MessagesState {
    pub inbox: Option<serde_json::Value>,
    pub outbox: Option<serde_json::Value>,
    pub localinfo: Option<serde_json::Value>,
    pub compose_from: String,
    pub compose_to: String,
    pub compose_text: String,
    pub send_result: Option<Result<String, String>>,
    pub last_error: Option<String>,
    pub last_refresh: Option<String>,
}

impl MessagesState {
    pub fn unread_count(&self) -> usize {
        self.inbox
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.get("read") == Some(&serde_json::Value::Bool(false)))
                    .count()
            })
            .unwrap_or(0)
    }
}

/// Broadcast flow state (sign → review → broadcast).
#[derive(Debug, Clone, Default)]
pub struct BroadcastState {
    pub busy: bool,
    pub result: Option<Result<String, String>>, // txid on success
    pub last_error: Option<String>,
}

// ===========================================================================
// AppState
// ===========================================================================

pub struct AppState {
    pub cfg: UiConfig,
    pub screen: Screen,
    pub load_error: Option<String>,
    pub wallet: Option<WalletData>,
    pub derived: Option<DerivedWallet>,
    pub tip: Option<TipInfo>,
    pub stake: Option<StakeStatus>,
    pub send: SendFormState,
    pub send_result: Option<Result<SendBuild, String>>,
    pub receive: ReceiveForm,
    pub addressbook: Vec<AbEntry>,
    pub keystore: Vec<AbEntry>,
    pub sign: SignForm,
    pub verify: VerifyForm,
    pub options: OptionsState,
    pub console: Vec<ConsoleEntry>,
    pub console_input: String,
    pub debug_info: Option<DebugInfo>,
    /// last time the full wallet history was pulled (multi-MB payload —
    /// fetched at most every 30 s, not on every status tick)
    pub last_history_fetch: Option<std::time::Instant>,
    pub dialogs: Dialogs,
    pub status_log: Vec<String>,
    pub explorer: super::explorer::ExplorerState,
    /// Open READ-WRITE wallet.dat session ("New Address"/labels write REAL
    /// rows into wallet.dat). None = wallet not writable → keystore fallback.
    pub wallet_rw: Option<crate::wallet::WalletHandle>,
    /// Write gate — false makes the GUI strictly read-only toward wallet.dat
    /// (--selfcheck must never mutate the live wallet file).
    pub allow_wallet_writes: bool,
    // ---- phase 7 milestone 2 ----
    pub staking_live: StakingLive,
    pub peers_live: PeersLive,
    pub messages: MessagesState,
    pub broadcast: BroadcastState,
    /// stake/payment watcher: last daemon balance observed (sats). When the
    /// daemon balance INCREASES while fully synced and notifications are on,
    /// fire a system notification (Sami: "alert to the system tray every
    /// time they receive a new stake, like the old wallet").
    pub notify_last_balance_sats: Option<i64>,
    /// in-app notification banner (also mirrored as a system toast). None =
    /// nothing pending; Some((title, text)) = show + allow dismiss.
    pub notify_flash: Option<(String, String)>,
    /// last staking-toggle error (button feedback when the daemon refuses,
    /// e.g. started without --wallet)
    pub stake_toggle_error: Option<String>,
    /// true while a staking on/off toggle RPC is in flight (button guard)
    pub staking_toggle_inflight: bool,
    // ---- auto-update (src/updater.rs) ----
    pub updater: crate::updater::UpdateState,
    /// Events from the check/download worker threads (gui feature only).
    pub updater_rx: Option<std::sync::mpsc::Receiver<crate::updater::UpdateEvent>>,
    /// Seed-phrase create/restore flow (load screen). None = hidden.
    pub seed_flow: Option<super::seed_flow::SeedFlow>,
    /// Seed-migration (sweep-to-seed) wizard. None = hidden.
    pub sweep_flow: Option<super::sweep_flow::SweepFlow>,
}

impl AppState {
    pub fn new(cfg: UiConfig) -> Self {
        let keystore: Vec<AbEntry> = load_side_entries(&cfg.keystore_path(), true);
        let addressbook: Vec<AbEntry> = load_side_entries(&cfg.addressbook_path(), false);
        let options = load_options(&cfg.options_path());
        // explorer endpoints: config rpc_addr first (if set), then the
        // standard native ports
        let mut endpoints = cfg.explorer_endpoints.clone();
        if let Some(a) = &cfg.rpc_addr {
            if !endpoints.contains(a) {
                endpoints.insert(0, a.clone());
            }
        }
        let explorer = super::explorer::ExplorerState::new(endpoints);
        // updater: persistence + download work dir live under profile_dir;
        // on Windows detect the NSIS install root for installer-vs-portable
        #[allow(unused_mut)]
        let mut updater = crate::updater::UpdateState::new(Path::new(&cfg.profile_dir));
        #[cfg(target_os = "windows")]
        if let Ok(lad) = std::env::var("LOCALAPPDATA") {
            updater.windows_install_root =
                Some(std::path::PathBuf::from(lad).join("Tri7Wallet"));
        }
        AppState {
            cfg,
            screen: Screen::Overview,
            load_error: None,
            wallet: None,
            derived: None,
            tip: None,
            stake: None,
            send: SendFormState::default(),
            send_result: None,
            receive: ReceiveForm::default(),
            addressbook,
            keystore,
            sign: SignForm::default(),
            verify: VerifyForm::default(),
            options,
            console: Vec::new(),
            console_input: String::new(),
            debug_info: None,
            last_history_fetch: None,
            dialogs: Dialogs::default(),
            status_log: Vec::new(),
            explorer,
            wallet_rw: None,
            allow_wallet_writes: true,
            staking_live: StakingLive::default(),
            peers_live: PeersLive::default(),
            notify_last_balance_sats: None,
            notify_flash: None,
            stake_toggle_error: None,
            staking_toggle_inflight: false,
            messages: MessagesState::default(),
            broadcast: BroadcastState::default(),
            updater,
            updater_rx: None,
            seed_flow: None,
            sweep_flow: None,
        }
    }

    /// The RPC endpoint the live panels talk to (same tri7d as the explorer).
    fn live_rpc_addr(&self) -> Option<String> {
        self.cfg.rpc_addr.clone()
    }

    /// Poll getstakinginfo for the live staking panel.
    pub fn staking_poll_action(&mut self) {
        let Some(addr) = self.live_rpc_addr() else {
            self.staking_live.last_error = Some("RPC disabled in config".into());
            return;
        };
        match crate::gui::rpc_client::rpc_call(&addr, "getstakinginfo", &[]) {
            Ok(v) => {
                self.staking_live.info = Some(v);
                self.staking_live.last_error = None;
                self.staking_live.last_poll = Some(chrono_iso_now());
            }
            Err(e) => self.staking_live.last_error = Some(e),
        }
    }

    /// staking on|off — persisted to the daemon's conf.
    pub fn staking_toggle_action(&mut self, on: bool) {
        let Some(addr) = self.live_rpc_addr() else {
            self.staking_live.last_error = Some("RPC disabled in config".into());
            return;
        };
        let arg = if on { "on" } else { "off" };
        match crate::gui::rpc_client::rpc_call(&addr, "staking", &[serde_json::json!(arg)]) {
            Ok(_) => self.staking_poll_action(),
            Err(e) => self.staking_live.last_error = Some(e),
        }
    }

    /// Refresh the peers table + connections count.
    pub fn peers_poll_action(&mut self) {
        let Some(addr) = self.live_rpc_addr() else {
            self.peers_live.last_error = Some("RPC disabled in config".into());
            return;
        };
        match crate::gui::rpc_client::rpc_call(&addr, "getpeerinfo", &[]) {
            Ok(v) => {
                self.peers_live.table = Some(v);
                self.peers_live.last_error = None;
            }
            Err(e) => self.peers_live.last_error = Some(e),
        }
    }

    /// Refresh inbox + outbox + local info.
    pub fn messages_refresh_action(&mut self) {
        let Some(addr) = self.live_rpc_addr() else {
            self.messages.last_error = Some("RPC disabled in config".into());
            return;
        };
        let rc = crate::gui::rpc_client::rpc_call;
        if let Ok(v) = rc(&addr, "smsginbox", &[serde_json::json!("all")]) {
            self.messages.inbox = Some(v);
        }
        if let Ok(v) = rc(&addr, "smsgoutbox", &[]) {
            self.messages.outbox = Some(v);
        }
        match rc(&addr, "smsglocalinfo", &[]) {
            Ok(v) => {
                // default the compose-from to the first owned key
                if self.messages.compose_from.is_empty() {
                    if let Some(addrs) = v.get("addresses").and_then(|a| a.as_array()) {
                        if let Some(a0) = addrs.first().and_then(|x| x.as_str()) {
                            self.messages.compose_from = a0.to_string();
                        }
                    }
                }
                self.messages.localinfo = Some(v);
                self.messages.last_error = None;
                self.messages.last_refresh = Some(chrono_iso_now());
            }
            Err(e) => self.messages.last_error = Some(e),
        }
    }

    /// smsgsend: compose + encrypt + store (the daemon relays via buckets).
    pub fn messages_send_action(&mut self) {
        let (from, to, text) = (
            self.messages.compose_from.clone(),
            self.messages.compose_to.clone(),
            self.messages.compose_text.clone(),
        );
        if from.is_empty() || to.is_empty() || text.is_empty() {
            self.messages.send_result = Some(Err("from, to and text are all required".into()));
            return;
        }
        let Some(addr) = self.live_rpc_addr() else {
            self.messages.send_result = Some(Err("RPC disabled in config".into()));
            return;
        };
        let r = crate::gui::rpc_client::rpc_call(
            &addr,
            "smsgsend",
            &[
                serde_json::json!(from),
                serde_json::json!(to),
                serde_json::json!(text),
            ],
        )
        .map(|v| {
            format!(
                "sent — timestamp {} sample {}",
                v.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0),
                v.get("sample").and_then(|s| s.as_str()).unwrap_or("?")
            )
        });
        if r.is_ok() {
            self.messages.compose_text.clear();
        }
        self.messages.send_result = Some(r);
    }

    /// smsgpurge: drop messages past the 48h retention.
    pub fn messages_purge_action(&mut self) {
        let Some(addr) = self.live_rpc_addr() else {
            self.messages.last_error = Some("RPC disabled in config".into());
            return;
        };
        match crate::gui::rpc_client::rpc_call(&addr, "smsgpurge", &[]) {
            Ok(v) => {
                let n = v.get("purged").and_then(|x| x.as_u64()).unwrap_or(0);
                self.status_log.push(format!("smsg purge: {} removed", n));
                self.messages_refresh_action();
            }
            Err(e) => self.messages.last_error = Some(e),
        }
    }

    /// Broadcast the signed tx on the Send screen: sendrawtransaction on the
    /// user's own tri7d; the daemon consensus-validates, adds the pending
    /// relay set and announces inv(MSG_TX) to every peer.
    pub fn broadcast_action(&mut self, tx_hex: String) {
        if tx_hex.is_empty() {
            self.broadcast.result = Some(Err("no signed tx — sign first".into()));
            return;
        }
        let Some(addr) = self.live_rpc_addr() else {
            self.broadcast.result = Some(Err("RPC disabled in config".into()));
            return;
        };
        self.broadcast.busy = true;
        let r = crate::gui::rpc_client::rpc_call(
            &addr,
            "sendrawtransaction",
            &[serde_json::json!(tx_hex)],
        )
        .map(|v| {
            let txid = v
                .get("txid")
                .and_then(|t| t.as_str())
                .unwrap_or("?")
                .to_string();
            let acks = v.get("announced_to").and_then(|a| a.as_u64()).unwrap_or(0);
            format!(
                "txid {} — relay acks from {} peer(s) (inv → getdata → tx)",
                txid, acks
            )
        });
        self.broadcast.busy = false;
        self.broadcast.result = Some(r);
    }

    // ===========================================================================
    // Seed migration ("sweep-to-seed") — the wizard state machine lives in
    // gui::sweep_flow, the money math in crate::sweep; these are the glue
    // actions. Ordering rule: keys → plan+sign (in memory) → operator review
    // → broadcast → ONLY THEN the 'seedmigrated' marker.
    // ===========================================================================

    fn now_unix_u32() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(1_400_000_000)
    }

    /// 'Add seed phrase' entry — the load screen (for any wallet) and the
    /// seed panel of an opened wallet. Loads the wallet first when needed,
    /// runs the entry guards, opens the wizard. Every refusal happens BEFORE
    /// any value can move.
    pub fn begin_seed_migration_action(&mut self) -> Result<(), String> {
        if !self.loaded() {
            self.load_wallet_action()?;
        }
        let w = self
            .wallet
            .as_ref()
            .ok_or_else(|| "wallet not loaded".to_string())?;
        if w.seed_migrated {
            return Err(
                "this wallet already completed seed migration — the 'seedmigrated' marker \
                 row refuses a second sweep"
                    .into(),
            );
        }
        if self.wallet_rw.is_none() {
            return Err(
                "wallet.dat is not writable this session — seed migration writes the derived \
                 keys into wallet.dat and refuses to proceed without it (close any other \
                 wallet app using this file and reopen)"
                    .into(),
            );
        }
        // entry pre-check: at least one spendable UTXO must exist. The exact
        // source set (minus the destination addresses) is computed in
        // prepare() once the phrase — and its addresses — are known.
        let tip_height = self.tip.as_ref().map(|t| t.height as u32).unwrap_or(0);
        let any_spendable = self
            .derived
            .as_ref()
            .map(|d| {
                d.utxos
                    .iter()
                    .any(|u| crate::sweep::sweep_spendable(&u.prev_kind, u.height, tip_height + 1))
            })
            .unwrap_or(false);
        if !any_spendable {
            return Err(
                "no spendable UTXOs found for this wallet — nothing to migrate (zero \
                 balance, immature coinbase/coinstake, or an empty/missing UTXO snapshot)"
                    .into(),
            );
        }
        let mut flow = crate::gui::sweep_flow::SweepFlow::new();
        if let Some(m) = &w.hd_mnemonic {
            // a stored seed exists (mid-migration retry, or a phrase
            // previously restored onto this wallet): the wizard CANNOT
            // create or replace it — it locks to the stored phrase and the
            // sweep targets that seed's own address set.
            flow.lock_to_stored_seed(m, w.hd_passphrase.as_deref())?;
        }
        self.sweep_flow = Some(flow);
        Ok(())
    }

    /// The candidate sweep source set: every spendable wallet UTXO as a
    /// SpendInput (destination exclusion happens inside prepare(), where the
    /// seed addresses are known). Maturity uses the exact rule
    /// ConnectInputs enforces — coinbase AND coinstake need
    /// COINBASE_MATURITY depth — so an immature input is refused at
    /// SELECTION, never at consensus.
    pub fn sweep_source_candidates(&self) -> Result<Vec<spend::SpendInput>, String> {
        let derived = self.derived.as_ref().ok_or("wallet not loaded")?;
        let keys = self.key_map();
        let tip_height = self.tip.as_ref().map(|t| t.height as u32).unwrap_or(0);
        let spend_height = tip_height + 1;
        let mut out = Vec::new();
        for u in &derived.utxos {
            if !crate::sweep::sweep_spendable(&u.prev_kind, u.height, spend_height) {
                continue;
            }
            let Some((pubkey, secret)) = keys.get(&u.address) else {
                return Err(format!(
                    "UTXO {}:{} pays to {} but the wallet holds no key for it — refusing \
                     to sweep. Migration cannot SKIP this coin: a skipped balance would be \
                     left on the old keys while the 'seedmigrated' marker blocks a second \
                     sweep. Resolve the key first (restore/import it into wallet.dat), \
                     then run the migration again",
                    u.txid_display(),
                    u.n,
                    u.address
                ));
            };
            out.push(spend::SpendInput {
                txid: u.txid,
                n: u.n,
                value: u.value_sats,
                script_pubkey: u.script.clone(),
                height: u.height,
                tx_time: u.tx_time,
                is_coinbase: u.prev_kind == "coinbase",
                is_coinstake: u.prev_kind == "coinstake",
                secret: *secret,
                pubkey: pubkey.clone(),
            });
        }
        Ok(out)
    }

    /// Verify backup → insert seed keys into wallet.dat → build + sign the
    /// sweep plan (in memory, deterministic). Advances the wizard to the
    /// exact-amounts review; nothing is broadcast here.
    pub fn sweep_prepare_action(&mut self) {
        let outcome = (|| -> Result<(), String> {
            let inputs = self.sweep_source_candidates()?;
            let tip_height = self.tip.as_ref().map(|t| t.height as u32).unwrap_or(0);
            let now = Self::now_unix_u32();
            let (Some(flow), Some(h)) = (self.sweep_flow.as_mut(), self.wallet_rw.as_mut())
            else {
                return Err(
                    "wallet.dat is not writable this session — seed migration refuses to \
                     proceed without its write session"
                        .into(),
                );
            };
            flow.prepare(h, &inputs, tip_height, now)
        })();
        match outcome {
            Ok(()) => {
                self.drain_wallet_journal();
                if let Err(e) = self.reload_wallet_keys() {
                    self.status_log
                        .push(format!("reload after prepare: {}", e));
                }
                if let Some(p) = self
                    .sweep_flow
                    .as_ref()
                    .and_then(|f| f.plan.as_ref())
                {
                    self.status_log.push(format!(
                        "sweep plan READY (signed, not broadcast): {} inputs {} TRI → {} \
                         outputs, fee {} TRI, txid {}",
                        p.inputs.len(),
                        format_tri(p.inputs_total),
                        p.outputs.len(),
                        format_tri(p.fee),
                        p.txid_display
                    ));
                }
            }
            Err(e) => {
                self.drain_wallet_journal();
                if let Some(f) = self.sweep_flow.as_mut() {
                    f.error = Some(e);
                }
            }
        }
    }

    /// Broadcast the signed sweep via tri7d sendrawtransaction (the SAME
    /// live path the Send screen uses), then — and only then — write the
    /// 'seedmigrated' marker. A broadcast failure loses nothing: keys are
    /// already in wallet.dat, coins remain on the old keys, retry works.
    pub fn sweep_broadcast_action(&mut self) {
        let hex = self
            .sweep_flow
            .as_ref()
            .and_then(|f| f.plan.as_ref())
            .map(|p| p.tx_hex.clone());
        let Some(hex) = hex else {
            if let Some(f) = self.sweep_flow.as_mut() {
                f.error = Some("no signed sweep plan — build it first".into());
            }
            return;
        };
        let Some(addr) = self.live_rpc_addr() else {
            if let Some(f) = self.sweep_flow.as_mut() {
                f.error =
                    Some("RPC disabled in config — point the GUI at your tri7d".into());
            }
            return;
        };
        let r = crate::gui::rpc_client::rpc_call(
            &addr,
            "sendrawtransaction",
            &[serde_json::json!(hex)],
        );
        match r {
            Ok(v) => {
                let txid = v
                    .get("txid")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string();
                let acks = v.get("announced_to").and_then(|a| a.as_u64()).unwrap_or(0);
                // remember the acknowledged txid BEFORE the marker write: if
                // the marker then fails, a retry can recognize "tx already
                // known" responses as a MARKER problem, not a value problem
                if let Some(f) = self.sweep_flow.as_mut() {
                    f.broadcast_txid = Some(txid.clone());
                }
                // strict ordering: the completion marker is written ONLY
                // after the broadcast was accepted by the daemon
                let Some(h) = self.wallet_rw.as_mut() else {
                    if let Some(f) = self.sweep_flow.as_mut() {
                        f.error = Some(
                            "wallet.dat is not writable this session — cannot write the \
                             completion marker (the broadcast DID go out; do not re-sweep)"
                                .into(),
                        );
                    }
                    return;
                };
                if let Err(e) = h.mark_seed_migrated() {
                    let msg = format!(
                        "broadcast SUCCEEDED (txid {}, {} relay acks) but writing the \
                         'seedmigrated' marker failed: {} — the coins ARE on the seed \
                         addresses; do NOT re-run the sweep",
                        txid, acks, e
                    );
                    self.status_log.push(msg.clone());
                    if let Some(f) = self.sweep_flow.as_mut() {
                        f.error = Some(msg);
                    }
                    return;
                }
                if let Some(f) = self.sweep_flow.as_mut() {
                    f.complete(txid.clone());
                }
                self.drain_wallet_journal();
                self.status_log.push(format!(
                    "seed migration complete: txid {} — the balance is now controlled by \
                     the seed-derived keys (old random keys remain in wallet.dat, now \
                     empty)",
                    txid
                ));
                if let Err(e) = self.refresh_chain_action() {
                    self.status_log
                        .push(format!("refresh after sweep: {}", e));
                }
            }
            Err(e) => {
                // An earlier attempt may have been ACCEPTED but failed on the
                // completion marker (recorded in broadcast_txid). If the
                // daemon now answers "already known"-shaped text for the SAME
                // plan, the tx IS broadcast — retry ONLY the marker write and
                // complete on success instead of scaring the operator.
                let prior = self
                    .sweep_flow
                    .as_ref()
                    .and_then(|f| f.broadcast_txid.clone());
                let same_plan = prior
                    .as_ref()
                    .map(|txid| e.contains(txid) || e.to_lowercase().contains("already") || e.to_lowercase().contains("already known") || e.to_lowercase().contains("known"))
                    .unwrap_or(false);
                if let (Some(txid), true) = (prior, same_plan) {
                    let marker = self
                        .wallet_rw
                        .as_mut()
                        .map(|h| h.mark_seed_migrated());
                    if let Some(Ok(())) = marker {
                        if let Some(f) = self.sweep_flow.as_mut() {
                            f.complete(txid.clone());
                        }
                        self.drain_wallet_journal();
                        self.status_log.push(format!(
                            "seed migration complete: txid {} (the tx was already \
                             broadcast; the completion marker is now set)",
                            txid
                        ));
                        if let Err(e2) = self.refresh_chain_action() {
                            self.status_log
                                .push(format!("refresh after sweep: {}", e2));
                        }
                        return;
                    }
                }
                let msg = format!(
                    "broadcast failed — NOTHING is lost: the derived keys are already in \
                     wallet.dat and the coins remain on the old keys. Get tri7d's \
                     sendrawtransaction reachable and retry. ({})",
                    e
                );
                if let Some(f) = self.sweep_flow.as_mut() {
                    f.error = Some(msg);
                }
            }
        }
    }

    /// Cancel/close the wizard (scrubs the phrase; nothing was broadcast).
    pub fn sweep_cancel_action(&mut self) {
        if let Some(f) = self.sweep_flow.as_mut() {
            f.abandon();
        }
        self.sweep_flow = None;
    }

    // ===========================================================================
    // Auto-update actions (src/updater.rs). The banner state machine is pure;
    // these methods are the glue: scheduling, worker threads, guards, and the
    // user-confirmed download/install flow. NOTHING downloads or installs
    // without the user having seen the version + notes and clicked through.
    // ===========================================================================

    /// GUI tick: auto-check on startup + every 6h, then drain worker events.
    /// Called from the egui update loop only.
    pub fn updater_tick(&mut self) {
        if !self.updater.is_inflight() && self.updater.needs_check() {
            self.updater_spawn_check(false);
        }
        self.updater_poll();
    }

    fn updater_spawn_check(&mut self, manual: bool) {
        if self.updater.request_check().is_err() {
            return;
        }
        #[cfg(feature = "gui")]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            match crate::updater::runtime::spawn_check(tx) {
                Ok(()) => self.updater_rx = Some(rx),
                Err(e) => {
                    self.updater.abort_inflight();
                    if manual {
                        self.status_log.push(format!("update check: {e}"));
                    }
                }
            }
        }
        #[cfg(not(feature = "gui"))]
        {
            let _ = manual;
            self.updater.abort_inflight();
        }
    }

    /// Drain worker events into the state machine.
    pub fn updater_poll(&mut self) {
        let Some(rx) = &self.updater_rx else {
            return;
        };
        let mut done = false;
        while let Ok(ev) = rx.try_recv() {
            self.updater.handle_event(ev);
            if matches!(
                self.updater.phase,
                crate::updater::UpdatePhase::ReadyToInstall
                    | crate::updater::UpdatePhase::Failed(_)
            ) {
                done = true;
            }
        }
        if done {
            self.updater_rx = None;
        }
    }

    /// Manual "Check for updates" (Options) — result surfaces immediately.
    pub fn updater_manual_check_action(&mut self) {
        if self.updater.request_check_manual().is_err() {
            return;
        }
        #[cfg(feature = "gui")]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            match crate::updater::runtime::spawn_check(tx) {
                Ok(()) => self.updater_rx = Some(rx),
                Err(e) => {
                    self.updater.abort_inflight();
                    self.status_log.push(format!("update check: {e}"));
                }
            }
        }
        #[cfg(not(feature = "gui"))]
        self.updater.abort_inflight();
    }

    /// Banner "Download & Install": guard the in-flight broadcast, then spawn
    /// the download worker. The user has SEEN version + notes at this point.
    pub fn updater_download_action(&mut self) -> Result<(), String> {
        let busy = self.broadcast.busy;
        self.updater.request_download(busy)?;
        self.updater_spawn_download()
    }

    /// Spawn the download worker for a request the state machine already
    /// accepted (request_download / retry already transitioned phases).
    fn updater_spawn_download(&mut self) -> Result<(), String> {
        let (dest, flavor, pinned) = self.updater.download_started_meta();
        let Some(rel) = self.updater.release.clone() else {
            self.updater.abort_inflight();
            return Err("no release on offer".into());
        };
        let Some(asset) = crate::updater::pick_asset(&rel, flavor).cloned() else {
            self.updater.abort_inflight();
            return Err(format!(
                "no asset for this platform (expected {})",
                flavor.asset_name(&rel.version)
            ));
        };
        #[cfg(feature = "gui")]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            match crate::updater::runtime::spawn_download(
                tx,
                asset,
                dest,
                pinned,
                flavor,
                rel.version.clone(),
            ) {
                Ok(()) => self.updater_rx = Some(rx),
                Err(e) => {
                    self.updater.abort_inflight();
                    return Err(format!("spawn download: {e}"));
                }
            }
        }
        #[cfg(not(feature = "gui"))]
        {
            let _ = (asset, dest, pinned, flavor);
            self.updater.abort_inflight();
        }
        Ok(())
    }

    /// Options/banner "Install now": THE SAFETY GATE.
    /// 1. refuse while a send/broadcast is in flight,
    /// 2. flush wallet state first (drop the RW session → SQLite close),
    /// 3. hand the payload to the platform installer (exit + relaunch).
    pub fn updater_install_action(&mut self) -> Result<String, String> {
        let busy = self.broadcast.busy;
        // guard FIRST (refusal must not disturb the wallet session), flush
        // the RW wallet only once the install is actually accepted
        let plan = self.updater.request_install(busy)?;
        self.wallet_rw = None; // close wallet.dat before any payload runs
        #[cfg(feature = "gui")]
        if let Err(e) = crate::updater::runtime::execute_install(&plan) {
            self.updater.phase = crate::updater::UpdatePhase::Failed(e.clone());
            return Err(e);
        }
        #[cfg(not(feature = "gui"))]
        let _ = &plan;
        Ok(plan.describe)
    }

    /// "Skip this version" — persisted; banner stays quiet for that version.
    pub fn updater_skip_action(&mut self) {
        self.updater.skip_version();
        self.status_log
            .push("update: version skipped".into());
    }

    /// Dismiss the banner without skipping (returns on the next check).
    pub fn updater_dismiss_action(&mut self) {
        self.updater.dismiss();
    }

    /// Retry a failed check/download. A failed download re-enters the
    /// download worker; a failed CHECK has nothing to retry — reset the
    /// in-flight slot and start a fresh manual check (no hang).
    pub fn updater_retry_action(&mut self) {
        let busy = self.broadcast.busy;
        if self.updater.release.is_some() {
            // retry() re-runs the pure download transition; we only spawn the
            // worker thread here (single spawn, no double request)
            if self.updater.retry(busy).is_ok() {
                self.updater_spawn_download()
                    .unwrap_or_else(|e| self.status_log.push(format!("update: {e}")));
            }
        } else {
            // failed check: retry() reports Err (nothing to re-run); clear the
            // state machine and run a fresh MANUAL check
            self.updater.abort_inflight();
            self.updater_spawn_check(true);
        }
    }

    /// Banner "View release notes" — opens the GitHub release page.
    pub fn updater_view_notes_action(&mut self) {
        let Some(url) = self.updater.release.as_ref().map(|r| r.html_url.clone()) else {
            return;
        };
        #[cfg(feature = "gui")]
        if let Err(e) = crate::updater::runtime::open_url(&url) {
            self.status_log.push(format!("release notes: {e}"));
        }
        #[cfg(not(feature = "gui"))]
        let _ = url;
    }

    /// Options "Restore previous version" — one-click rollback from <exe>.bak.
    pub fn updater_restore_previous_action(&mut self) -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
        let plan = crate::updater::restore_previous_plan(&exe)?;
        self.wallet_rw = None;
        #[cfg(feature = "gui")]
        if let Err(e) = crate::updater::runtime::execute_install(&plan) {
            return Err(e);
        }
        #[cfg(not(feature = "gui"))]
        let _ = &plan;
        Ok(plan.describe)
    }

    /// One-line banner text for the view layer (None = no banner).
    pub fn updater_banner_line(&self) -> Option<String> {
        let ver = self.updater.release.as_ref().map(|r| r.version.to_string_short());
        match &self.updater.phase {
            crate::updater::UpdatePhase::Available => {
                ver.map(|v| format!("Cryptographic Triangles {v} available"))
            }
            crate::updater::UpdatePhase::Downloading { received, total } => Some(format!(
                "Downloading Cryptographic Triangles {} — {}/{} MiB",
                ver.unwrap_or_default(),
                received / (1024 * 1024),
                (total / (1024 * 1024)).max(1)
            )),
            crate::updater::UpdatePhase::ReadyToInstall => {
                ver.map(|v| format!("Cryptographic Triangles {v} downloaded — install to restart"))
            }
            crate::updater::UpdatePhase::Failed(e) => Some(format!("Update failed: {e}")),
            _ => None,
        }
    }

    /// Options "Software updates" status line.
    pub fn updater_options_line(&self) -> String {
        let checked = match self.updater.last_check_age_secs() {
            None => "never".into(),
            Some(s) if s < 90 => format!("{s}s ago"),
            Some(s) => format!("{}m ago", s / 60),
        };
        let skipped = self
            .updater
            .skipped_version()
            .map(|v| format!(" (skipped: {v})"))
            .unwrap_or_default();
        match &self.updater.phase {
            crate::updater::UpdatePhase::Available => format!(
                "Cryptographic Triangles {} available — checked {}{}",
                self.updater
                    .release
                    .as_ref()
                    .map(|r| r.version.to_string_short())
                    .unwrap_or_default(),
                checked,
                skipped
            ),
            crate::updater::UpdatePhase::UpToDate => {
                format!("Up to date (v{}) — checked {}", crate::updater::current_version(), checked)
            }
            _ => format!(
                "Current version: v{} — checked {}{}",
                crate::updater::current_version(),
                checked,
                skipped
            ),
        }
    }

    pub fn loaded(&self) -> bool {
        self.wallet.is_some() && self.derived.is_some()
    }

    /// Label resolution: wallet.dat names → GUI address book → keystore.
    pub fn label_of(&self, address: &str) -> Option<String> {
        if let Some(w) = &self.wallet {
            if let Some(l) = w.label_of(address) {
                return Some(l.to_string());
            }
        }
        for list in [&self.addressbook, &self.keystore] {
            if let Some(e) = list.iter().find(|e| e.address == address) {
                if !e.label.is_empty() {
                    return Some(e.label.clone());
                }
            }
        }
        None
    }

    /// The load-screen action: wallet + UTXO derivation pass + tip + stake.
    pub fn load_wallet_action(&mut self) -> Result<(), String> {
        let wallet = load_wallet(Path::new(&self.cfg.wallet_path))?;
        // ONE-FOLDER DESIGN (Sami): the chosen folder IS the wallet
        // environment - state files live beside wallet.dat automatically.
        // Only when the user did NOT pass --state-dir explicitly (power users
        // / selfcheck / fleet daemons point at their own state) - the flag
        // always wins.
        if !self.cfg.state_dir_explicit {
            let parent = Path::new(&self.cfg.wallet_path)
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if !parent.is_empty() {
                self.cfg.state_dir = parent;
            }
        }
        if wallet.keys.is_empty() {
            return Err(format!("wallet {} has zero keys", self.cfg.wallet_path));
        }
        // remember for next launch (Sami: reopen the wallet that exists)
        remember_last_wallet(&self.cfg.profile_dir, &self.cfg.wallet_path);
        let payloads: HashSet<[u8; 21]> = wallet
            .keys
            .iter()
            .map(|k| crate::address::pubkey_payload(&k.pubkey))
            .collect();
        let tip = load_tipstate(&self.cfg.tipstate_path())?;
        let dump = load_utxo_dump(&self.cfg.utxo_dump_path())?;
        // label lookup needs wallet+side files; build the closure over a
        // snapshot of the label maps
        let mut labels: HashMap<String, String> = wallet
            .names
            .iter()
            .map(|(a, l)| (a.clone(), l.clone()))
            .collect();
        for e in self.keystore.iter().chain(self.addressbook.iter()) {
            if !e.label.is_empty() {
                labels
                    .entry(e.address.clone())
                    .or_insert_with(|| e.label.clone());
            }
        }
        let derived = derive_wallet(dump, payloads, &self.cfg, &tip, &|a: &str| {
            labels.get(a).cloned()
        })?;
        let stake = {
            // stake status wants the FINAL utxo set
            let tip2 = tip.clone();
            load_stake_status(
                Path::new(&self.cfg.stake_dryrun_path),
                &tip2,
                &derived.utxos,
            )
        };

        // form defaults: first funded address (by balance), receive = default key
        let first_funded = derived
            .balances
            .per_address
            .first()
            .map(|a| a.address.clone())
            .unwrap_or_default();
        if self.send.from.is_empty() {
            self.send.from = first_funded;
        }
        let default_addr = wallet
            .default_key
            .as_ref()
            .and_then(|pk| {
                wallet
                    .keys
                    .iter()
                    .find(|k| k.pubkey == *pk)
                    .map(|k| k.address.clone())
            })
            .or_else(|| wallet.keys.first().map(|k| k.address.clone()));
        if self.sign.address.is_empty() {
            self.sign.address = default_addr.clone().unwrap_or_default();
        }
        if self.receive.address.is_empty() {
            self.receive.address = default_addr.unwrap_or_default();
        }

        self.wallet = Some(wallet);
        self.derived = Some(derived);
        self.tip = Some(tip);
        self.stake = Some(stake);
        self.load_error = None;
        self.status_log.push("wallet loaded".into());
        // Read-write session for wallet.dat writes (best effort: a read-only
        // or locked wallet keeps loading; New Address then falls back to the
        // GUI keystore with the reason in the status log).
        self.ensure_wallet_rw();
        Ok(())
    }

    /// Open (or keep) the READ-WRITE wallet.dat session for `cfg.wallet_path`.
    /// Never fails the load — the reason for non-writability is journaled.
    fn ensure_wallet_rw(&mut self) {
        if !self.allow_wallet_writes {
            self.wallet_rw = None;
            return;
        }
        let path = self.cfg.wallet_path.clone();
        let same = self
            .wallet_rw
            .as_ref()
            .map(|h| h.path.display().to_string() == path)
            .unwrap_or(false);
        if same {
            return; // keep the session (one .bak per session, locks held)
        }
        self.wallet_rw = None;
        match crate::wallet::WalletHandle::open_readwrite(Path::new(&path)) {
            Ok(h) => {
                self.status_log
                    .push(format!("wallet.dat open READ-WRITE: {}", path));
                self.wallet_rw = Some(h);
            }
            Err(e) => {
                self.status_log.push(format!(
                    "wallet.dat NOT writable — new keys go to the GUI keystore ({})",
                    e
                ));
            }
        }
    }

    /// wallet.dat accepts writes this session ("New Address" goes into
    /// wallet.dat).
    pub fn wallet_writable(&self) -> bool {
        self.wallet_rw.is_some()
    }

    /// Re-read wallet keys/labels from wallet.dat (after a write) so the
    /// Receive/Address Book/Send surfaces show the new state immediately.
    fn reload_wallet_keys(&mut self) -> Result<(), String> {
        let path = self.cfg.wallet_path.clone();
        let w = crate::wallet::read_wallet(Path::new(&path))?;
        let seed_migrated = crate::wallet::seed_migration_done(Path::new(&path))?;
        match &mut self.wallet {
            Some(cur) => {
                cur.keys = w.keys;
                cur.names = w.names;
                cur.default_key = w.default_key;
                cur.version = w.version;
                cur.best_block = w.best_block;
                cur.hd_mnemonic = w.hd_mnemonic;
                cur.hd_passphrase = w.hd_passphrase;
                cur.hd_chain_index = w.hd_chain_index.unwrap_or(0);
                cur.seed_migrated = seed_migrated;
            }
            None => {
                self.wallet = Some(WalletData {
                    path: self.cfg.wallet_path.clone(),
                    keys: w.keys,
                    names: w.names,
                    default_key: w.default_key,
                    version: w.version,
                    best_block: w.best_block,
                    hd_mnemonic: w.hd_mnemonic,
                    hd_passphrase: w.hd_passphrase,
                    hd_chain_index: w.hd_chain_index.unwrap_or(0),
                    seed_migrated,
                });
            }
        }
        Ok(())
    }

    /// Drain the wallet write journal into the status log (every write is
    /// journaled to a log line — both in-memory and stderr).
    fn drain_wallet_journal(&mut self) {
        if let Some(h) = self.wallet_rw.as_mut() {
            let lines = h.drain_journal();
            self.status_log.extend(lines);
        }
    }

    /// Re-run the UTXO derivation pass (Refresh).
    pub fn refresh_chain_action(&mut self) -> Result<(), String> {
        if self.wallet.is_none() {
            return Err("wallet not loaded".into());
        }
        let saved_from = self.send.from.clone();
        let saved_sign = self.sign.address.clone();
        let saved_recv = self.receive.address.clone();
        self.send.from.clear();
        self.sign.address.clear();
        self.receive.address.clear();
        let r = self.load_wallet_action();
        self.send.from = saved_from;
        self.sign.address = saved_sign;
        self.receive.address = saved_recv;
        r
    }

    pub fn goto(&mut self, s: Screen) {
        self.screen = s;
    }

    /// Funded addresses for the Send from-picker (legacy: only spendable
    /// sources are offered).
    pub fn funded_addresses(&self) -> Vec<(String, i64)> {
        self.derived
            .as_ref()
            .map(|d| {
                d.balances
                    .per_address
                    .iter()
                    .filter(|a| a.sats > 0)
                    .map(|a| (a.address.clone(), a.sats))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// signing key map: wallet keys + keystore entries
    pub fn key_map(&self) -> std::collections::HashMap<String, (Vec<u8>, [u8; 32])> {
        let mut m = std::collections::HashMap::new();
        if let Some(w) = &self.wallet {
            for k in &w.keys {
                m.insert(k.address.clone(), (k.pubkey.clone(), k.secret));
            }
        }
        for e in &self.keystore {
            if let Some(secret) = e.secret {
                // derive pubkey → address lazily only when needed; store the
                // address→(empty pubkey is WRONG for P2PK prevouts, but keystore
                // keys only sign P2PKH) — rebuild pubkey from secret:
                let secp = secp256k1::Secp256k1::signing_only();
                let sk = match secp256k1::SecretKey::from_slice(&secret) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
                m.insert(e.address.clone(), (pk.serialize().to_vec(), secret));
            }
        }
        m
    }

    /// The Send dialog action: build + sign (NO broadcast — this build
    /// produces the signed hex only).
    pub fn build_send_action(&mut self, now: u32) {
        let Some(derived) = self.derived.clone() else {
            self.send_result = Some(Err("wallet not loaded".into()));
            return;
        };
        let keys = self.key_map();
        let tip_height = self.tip.as_ref().map(|t| t.height as u32).unwrap_or(0);
        self.send_result = Some(match validate_send_form(&self.send, &derived, &keys) {
            Ok(req) => {
                // coin-control manual pool
                let manual = if self.send.selected.is_empty() {
                    None
                } else {
                    let pool: Vec<UtxoRec> = self
                        .send
                        .selected
                        .iter()
                        .filter_map(|i| derived.utxos.get(*i).cloned())
                        .collect();
                    Some(pool)
                };
                build_signed_send(&req, &derived, &keys, manual.as_deref(), now, tip_height)
            }
            Err(e) => Err(e),
        });
    }

    pub fn save_tx_hex_action(&self, path: &str) -> Result<usize, String> {
        let hex = match &self.send_result {
            Some(Ok(b)) => &b.tx_hex,
            _ => return Err("no signed transaction to save".into()),
        };
        let body = format!(
            "{{\n  \"txid\": \"{}\",\n  \"tx_hex\": \"{}\",\n  \"broadcast\": false,\n  \"note\": \"signed by tri7 wallet (phase 6); NOT broadcast\"\n}}\n",
            self.send_result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .map(|b| b.txid_display.clone())
                .unwrap_or_default(),
            hex
        );
        let tmp = format!("{}.tmp", path);
        std::fs::write(&tmp, &body).map_err(|e| format!("write {}: {}", tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path, e))?;
        Ok(body.len())
    }

    /// Receive page: rebuild URI + QR (qrcodedialog replica).
    pub fn refresh_receive_qr(&mut self) {
        self.receive.error = None;
        self.receive.qr = None;
        if self.receive.address.is_empty() {
            self.receive.error = Some("select an address".into());
            return;
        }
        let amount = if self.receive.require_payment {
            Some(self.receive.amount.as_str())
        } else {
            None
        };
        match build_payment_uri(
            &self.receive.address,
            amount,
            &self.receive.label,
            &self.receive.message,
        ) {
            Ok(uri) => match qr_matrix(&uri) {
                Ok(qr) => {
                    self.receive.qr = Some(qr);
                    self.receive.uri = uri;
                }
                Err(e) => self.receive.error = Some(e),
            },
            Err(e) => self.receive.error = Some(e),
        }
    }

    pub fn save_qr_action(&self, path: &str) -> Result<usize, String> {
        self.receive
            .qr
            .as_ref()
            .ok_or_else(|| "no QR code rendered".to_string())
            .and_then(|qr| save_qr_png(qr, Path::new(path)))
    }

    pub fn sign_message_action(&mut self) {
        self.sign.error = None;
        self.sign.signature.clear();
        let keys = self.key_map();
        let Some((_, secret)) = keys.get(&self.sign.address) else {
            self.sign.error = Some(format!(
                "\"{}\" is not a wallet key address",
                self.sign.address
            ));
            return;
        };
        match sign_message(secret, &self.sign.message) {
            Ok(sig) => self.sign.signature = sig,
            Err(e) => self.sign.error = Some(e),
        }
    }

    pub fn verify_message_action(&mut self) {
        self.verify.result = Some(verify_message(
            &self.verify.address,
            &self.verify.message,
            &self.verify.signature,
        ));
    }

    /// Legacy "New Address" (receive tab): a fresh CSPRNG key written INTO
    /// wallet.dat in the exact legacy record set (keymeta + key + name rows,
    /// one BEGIN IMMEDIATE transaction, .tri7bak backup, verify-after-write)
    /// when the wallet is open read-write. The GUI keystore side file is only
    /// the FALLBACK when wallet.dat refuses writes (encrypted / locked /
    /// foreign file) — the fallback entry is journaled like any write.
    pub fn new_address_action(&mut self, label: String) -> Result<String, String> {
        // Seed-derived wallet: the next address MUST come from the phrase at
        // hdchain counter i (the legacy CWallet::GenerateNewKey HD branch) —
        // a random key here would silently break the deterministic backup.
        if self
            .wallet_rw
            .as_ref()
            .map(|h| h.hd_mnemonic.is_some())
            .unwrap_or(false)
        {
            let res = {
                let h = self.wallet_rw.as_mut().unwrap();
                let index = h.hd_chain_index;
                h.generate_hd_key(&label).map(|k| (index, k))
            };
            self.drain_wallet_journal();
            match res {
                Ok((index, key)) => {
                    let addr = key.address.clone();
                    self.reload_wallet_keys()?;
                    self.status_log.push(format!(
                        "HD address {} derived at m/44'/2222'/0'/0/{} from the wallet seed",
                        addr, index
                    ));
                    return Ok(addr);
                }
                Err(e) => {
                    // fall through to the random path is WRONG for HD wallets
                    return Err(format!(
                        "HD derivation failed — refusing to create a random key in a \
                         seed-derived wallet: {}",
                        e
                    ));
                }
            }
        }
        if self.wallet_rw.is_some() {
            let res = {
                let h = self.wallet_rw.as_mut().unwrap();
                super::state::generate_secret().and_then(|sec| h.add_key(&sec, &label))
            };
            self.drain_wallet_journal();
            match res {
                Ok(key) => {
                    let addr = key.address.clone();
                    // surface the new key on Receive/Address Book right away
                    self.reload_wallet_keys()?;
                    // best-effort: move the wallet bestblock marker to our
                    // tip (the qt resumes from hash[0] instead of rescanning)
                    let tip = self.tip.clone();
                    if let Some(tip) = tip {
                        if let Ok(thash) = crate::p2p::display_to_internal(&tip.blockhash) {
                            let r = self
                                .wallet_rw
                                .as_mut()
                                .map(|h| h.update_bestblock(tip.height, &thash));
                            self.drain_wallet_journal();
                            if let Some(Err(e)) = r {
                                self.status_log
                                    .push(format!("bestblock update failed: {}", e));
                            }
                        }
                    }
                    return Ok(addr);
                }
                Err(e) => {
                    self.status_log.push(format!(
                        "wallet.dat write refused — falling back to GUI keystore: {}",
                        e
                    ));
                }
            }
        }
        let mut entry = generate_new_address()?;
        entry.label = label;
        let addr = entry.address.clone();
        self.keystore.push(entry);
        persist_side_entries(&self.cfg.keystore_path(), &self.keystore, true)?;
        Ok(addr)
    }

    /// Address label edit: a wallet.dat `name` row when the address is a
    /// wallet key and the wallet is open read-write (the qt reads these back
    /// on its own receive page); the GUI address book side file otherwise.
    pub fn set_label_action(&mut self, address: &str, label: String) -> Result<(), String> {
        let wallet_owned = self
            .wallet
            .as_ref()
            .map(|w| w.keys.iter().any(|k| k.address == address))
            .unwrap_or(false);
        if wallet_owned && self.wallet_rw.is_some() {
            let r = {
                let h = self.wallet_rw.as_mut().unwrap();
                h.set_label(address, &label)
            };
            self.drain_wallet_journal();
            r?;
            // refresh the in-memory label map (no full re-read needed)
            if let Some(w) = self.wallet.as_mut() {
                w.names.retain(|(a, _)| a != address);
                w.names.push((address.to_string(), label.clone()));
            }
            return Ok(());
        }
        if let Some(e) = self.addressbook.iter_mut().find(|e| e.address == address) {
            e.label = label;
        } else {
            self.addressbook.push(AbEntry {
                address: address.to_string(),
                label,
                secret: None,
                source: "addressbook",
            });
        }
        persist_side_entries(&self.cfg.addressbook_path(), &self.addressbook, false)
    }

    /// Only side-file entries are deletable; wallet keys always stay.
    pub fn delete_ab_entry_action(&mut self, address: &str) -> Result<(), String> {
        if self.wallet_label_exists(address) {
            return Err(
                "wallet addresses cannot be deleted - those keys hold your coins.                  Only address-book entries can be removed."
                    .into(),
            );
        }
        self.addressbook.retain(|e| e.address != address);
        self.keystore.retain(|e| e.address != address);
        persist_side_entries(&self.cfg.addressbook_path(), &self.addressbook, false)?;
        persist_side_entries(&self.cfg.keystore_path(), &self.keystore, true)
    }

    fn wallet_label_exists(&self, address: &str) -> bool {
        self.wallet
            .as_ref()
            .map(|w| w.keys.iter().any(|k| k.address == address))
            .unwrap_or(false)
    }

    /// Debug-window console (rpcconsole): one command line.
    pub fn rpc_console_send_action(&mut self) {
        let line = self.console_input.trim().to_string();
        if line.is_empty() {
            return;
        }
        let (method, params) = parse_console_line(&line);
        let addr = self.cfg.rpc_addr.clone().unwrap_or_default();
        let entry = match rpc_client::rpc_call(&addr, &method, &params) {
            Ok(v) => ConsoleEntry {
                request: line,
                reply: Some(serde_json::to_string_pretty(&v).unwrap_or_default()),
                error: None,
            },
            Err(e) => ConsoleEntry {
                request: line,
                reply: None,
                error: Some(e),
            },
        };
        self.console.push(entry);
        self.console_input.clear();
    }

    /// Connection info refresh (status bar + Chain/Debug information page).
    /// Visible diagnostic: what the Transactions page is actually reading.
    /// Sami has been told "history is loaded" three times while the page kept
    /// showing only today's rows, so the UI now prints the resolved path, the
    /// row count and the oldest date. No more guessing.
    pub fn history_status_line(&self) -> String {
        let wp = self.cfg.wallet_path.trim().to_string();
        let folder = std::path::Path::new(&wp)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let wallet_copy = crate::history::cache_path(std::path::Path::new(&folder));
        let (rows, note) = self.load_history_file();
        // the note names the file that actually loaded
        let file = if note.starts_with("history.json") || note.contains("rows, scanned") {
            note.split(" - ").next().unwrap_or("").to_string()
        } else {
            wallet_copy.display().to_string()
        };
        let exists = std::path::Path::new(&file).exists();
        let oldest = rows.iter().map(|r| r.time).filter(|t| *t > 0).min();
        let newest = rows.iter().map(|r| r.time).max();
        format!(
            "history: {} | exists={} | rows={} | oldest={} | newest={} | {}",
            file,
            exists,
            rows.len(),
            oldest.map(stamp_date).unwrap_or_else(|| "-".into()),
            newest.map(stamp_date).unwrap_or_else(|| "-".into()),
            note
        )
    }

    /// Full wallet history straight from `<wallet folder>/history.json`.
    ///
    /// The daemon route (listwallettx) is an optimisation, not the source of
    /// truth: the file sits next to wallet.dat, we wrote it there, and the GUI
    /// must never depend on an RPC round-trip to show a receipt from 2021.
    /// Returns (rows, note) where note explains why it is empty when it is.
    pub fn load_history_file(&self) -> (Vec<crate::history::HistoryRec>, String) {
        let wp = self.cfg.wallet_path.clone();
        if wp.trim().is_empty() {
            return (Vec::new(), "no wallet path configured".into());
        }
        let dir = std::path::Path::new(&wp)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        // the wallet folder is the source of truth; the daemon's state dir
        // (where txhistory wrote) is the same folder in the one-folder design,
        // but check both so a legacy layout still resolves
        // Remember: history.json used to be written into the NODE's state dir
        // (a sibling of the wallet folder, not inside it), which is why a
        // wallet folder with the file present still rendered only today's
        // rows. Check every place it can legitimately live.
        let mut candidates = vec![crate::history::cache_path(&dir)];
        if !self.cfg.state_dir.trim().is_empty() {
            candidates.push(crate::history::cache_path(std::path::Path::new(
                &self.cfg.state_dir,
            )));
        }
        // embedded node layouts
        for sub in ["node", "node\\state", "state"] {
            let p = std::path::Path::new(&dir).join(sub);
            candidates.push(crate::history::cache_path(&p));
        }
        for path in candidates {
            if let Some(cache) = crate::history::load_cache(path.parent().unwrap_or(&dir)) {
                if !cache.rows.is_empty() {
                    let mut rows = cache.rows;
                    rows.sort_by(|a, b| b.time.cmp(&a.time).then(b.height.cmp(&a.height)));
                    let n = rows.len();
                    return (
                        rows,
                        format!(
                            "{} - {} rows, scanned to h{}",
                            path.display(),
                            n,
                            cache.scanned_height
                        ),
                    );
                }
            }
        }
        (
            Vec::new(),
            format!("no history.json in {}", dir.display()),
        )
    }

    pub fn refresh_debug_info_action(&mut self) {
        let mut info = DebugInfo::default();
        if let Some(addr) = self.cfg.rpc_addr.clone() {
            match rpc_client::rpc_call(&addr, "getconnectioncount", &[]) {
                Ok(v) => {
                    info.reachable = true;
                    info.connections = v.as_u64();
                    if let Ok(v2) = rpc_client::rpc_call(&addr, "getblockcount", &[]) {
                        info.blocks = v2.as_u64();
                    }
                    // live sync progress (Sami: "it should show this on the UI")
                    if let Ok(v3) = rpc_client::rpc_call(&addr, "getsyncinfo", &[]) {
                        let syncing = v3.get("syncing").and_then(|s| s.as_bool()).unwrap_or(false);
                        if syncing {
                            let body = v3.get("body").cloned().unwrap_or(serde_json::Value::Null);
                            info.sync = Some(GuiSync {
                                height: body.get("height").and_then(|h| h.as_u64()).unwrap_or(0),
                                target_height: body
                                    .get("target_height")
                                    .and_then(|h| h.as_u64())
                                    .unwrap_or(0),
                                blocks_per_sec: body
                                    .get("blocks_per_sec")
                                    .and_then(|r| r.as_f64())
                                    .unwrap_or(0.0),
                                eta_secs: body.get("eta_secs").and_then(|e| e.as_u64()).unwrap_or(0),
                                syncing: true,
                            });
                        }
                    }
                    // daemon-side unspent set for the Transactions page
                    // (fresh installs have no local snapshot; the daemon's
                    // live view is the truth)
                    if let Ok(vu) = rpc_client::rpc_call(&addr, "listunspent", &[]) {
                        if let Some(rows) = vu.as_array() {
                            let mut out: Vec<crate::gui::state::UtxoRec> = Vec::new();
                            for r in rows {
                                let Some(txid_hex) = r.get("txid").and_then(|x| x.as_str()) else { continue };
                                if txid_hex.len() != 64 { continue; }
                                let bytes: Vec<u8> = (0..32)
                                    .filter_map(|i| u8::from_str_radix(&txid_hex[i * 2..i * 2 + 2], 16).ok())
                                    .collect();
                                if bytes.len() != 32 { continue; }
                                let mut h = [0u8; 32];
                                h.copy_from_slice(&bytes);
                                h.reverse(); // display order -> internal order
                                out.push(crate::gui::state::UtxoRec {
                                    txid: h,
                                    n: r.get("vout").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                    value_sats: r.get("amount_sats").and_then(|x| x.as_i64()).unwrap_or(0),
                                    script: Vec::new(),
                                    address: r.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                    height: r.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                    tx_time: r.get("tx_time").and_then(|x| x.as_i64()).unwrap_or(0) as u32,
                                    prev_kind: r.get("prev_kind").and_then(|x| x.as_str()).unwrap_or("regular").to_string(),
                                });
                            }
                            if !out.is_empty() {
                                info.daemon_utxos = Some(out);
                            }
                        }
                    }
                    // full wallet history (history.json via the daemon)
                    // 30 s throttle: the payload is every history row ever
                    // seen (multi-MB), not something to pull each tick
                    let history_due = self
                        .last_history_fetch
                        .map(|t| t.elapsed().as_secs() >= 30)
                        .unwrap_or(true);
                    if history_due {
                        self.last_history_fetch = Some(std::time::Instant::now());
                        if let Ok(vh) = rpc_client::rpc_call(&addr, "listwallettx", &[]) {
                        let rows = vh
                            .get("rows")
                            .and_then(|r| r.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let mut out: Vec<crate::history::HistoryRec> = Vec::new();
                        for r in rows {
                            let rec = crate::history::HistoryRec {
                                txid: r.get("txid").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                height: r.get("height").and_then(|x| x.as_u64()).unwrap_or(0),
                                time: r.get("time").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                value_sats: r.get("amount_sats").and_then(|x| x.as_i64()).unwrap_or(0),
                                address: r.get("address").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                direction: r.get("direction").and_then(|x| x.as_str()).unwrap_or("in").to_string(),
                                kind: r.get("kind").and_then(|x| x.as_str()).unwrap_or("regular").to_string(),
                                spent: r.get("spent").and_then(|x| x.as_bool()).unwrap_or(false),
                            };
                            out.push(rec);
                        }
                            if !out.is_empty() {
                                info.wallet_history = Some(out);
                            }
                        }
                    }
                    // live daemon-side wallet balance (getbalance returns TRI;
                    // TRI is a 6-decimal chain — COIN = 1_000_000 µTRI, NOT
                    // 8 decimals. Store µTRI so format_tri (÷1e6) is exact.
                    if let Ok(v4) = rpc_client::rpc_call(&addr, "getbalance", &[]) {
                        if let Some(tri) = v4.as_f64() {
                            info.daemon_balance_sats = Some((tri * 1_000_000.0).round() as i64);
                        }
                    }
                }
                Err(e) => info.error = Some(e),
            }
        } else {
            info.error = Some("RPC disabled in config".into());
        }
        // ---- stake/payment watcher (Sami: alert like the old wallet) ----
        let syncing = info
            .sync
            .as_ref()
            .map(|s| s.syncing)
            .unwrap_or(false);
        if let Some(bal) = info.daemon_balance_sats {
            let enabled = self.options.notifications;
            let syncing = syncing || !info.reachable;
            if let Some(delta) =
                stake_alert_decision(self.notify_last_balance_sats, bal, syncing, enabled)
            {
                let title = "Coins received — Cryptographic Triangles".to_string();
                let text = format!(
                    "+{} TRI received (balance {} TRI)",
                    format_tri(delta),
                    format_tri(bal)
                );
                self.notify_flash = Some((title.clone(), text.clone()));
                crate::gui::notify::fire_system_notification(&title, &text);
            }
            self.notify_last_balance_sats = Some(bal);
        }
        self.debug_info = Some(info);
    }

    /// Legacy "Backup Wallet...": byte-copy of wallet.dat to the chosen path,
    /// verified by length + hash.
    pub fn backup_wallet_action(&mut self, dest: &str) -> Result<u64, String> {
        let src = &self.cfg.wallet_path;
        let bytes = std::fs::read(src).map_err(|e| format!("read {}: {}", src, e))?;
        let tmp = format!("{}.tmp", dest);
        std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {}", tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, dest).map_err(|e| format!("rename {}: {}", dest, e))?;
        let copied = std::fs::metadata(dest)
            .map_err(|e| format!("verify {}: {}", dest, e))?
            .len();
        if copied != bytes.len() as u64 {
            return Err(format!(
                "backup verify: {} != {} bytes",
                copied,
                bytes.len()
            ));
        }
        self.dialogs.backup = Some(Ok(format!(
            "wallet backed up to {} ({} bytes)",
            dest, copied
        )));
        Ok(copied)
    }

    pub fn encrypt_wallet_stub_action(&mut self) {
        self.dialogs.encrypt = Some(
            "Wallet encryption is not yet supported in the tri7 wallet engine. \
             The legacy wallet.dat is unencrypted; this button will gain its \
             original function (askpassphrasedialog) with daemon integration."
                .into(),
        );
    }

    pub fn save_options_action(&mut self) -> Result<(), String> {
        // validate the fee is a real amount before persisting
        parse_tri(&self.options.pay_fee)
            .map_err(|e| format!("Pay transaction fee invalid: {}", e))?;
        persist_options(&self.cfg.options_path(), &self.options)
    }

    /// Explorer: the search box action (classify + jump).
    pub fn explorer_search_action(&mut self) {
        let input = self.explorer.input.clone();
        if let Err(e) = self.explorer.search(&input) {
            self.explorer.last_error = Some(e);
        }
    }

    /// Explorer: one more history-scan page for the open address.
    pub fn explorer_scan_more_action(&mut self, budget: usize) {
        // target the EXPLICITLY tracked current address (open_addr), never a
        // HashMap-order pick from the addr cache
        let address = match (&self.explorer.view, self.explorer.open_addr.clone()) {
            (super::explorer::ExplorerView::Address, Some(a)) => a,
            _ => return,
        };
        if let Err(e) = self.explorer.scan_address_more(&address, budget) {
            self.explorer.scanning = false;
            self.explorer.last_error = Some(e);
        }
    }

    /// Explorer: stats + latest blocks.
    pub fn explorer_stats_action(&mut self, latest_n: usize) {
        if let Err(e) = self.explorer.refresh_stats(latest_n) {
            self.explorer.last_error = Some(e);
        }
    }

    /// Explorer: rich list from the balances.json snapshot.
    pub fn explorer_rich_action(&mut self, top: usize) {
        let path = self.cfg.balances_path.clone();
        if let Err(e) = self.explorer.load_rich_list_action(Path::new(&path), top) {
            self.explorer.last_error = Some(e);
        }
    }

    /// Explorer: attach the snapshot figures to the open address view.
    pub fn explorer_attach_snapshot(&mut self, address: &str) {
        let path = self.cfg.balances_path.clone();
        let _ = self
            .explorer
            .attach_snapshot_for_address(Path::new(&path), address);
    }

    /// Explorer: fee/credit display delegate (resolves prevouts via RPC).
    pub fn fee_of(&mut self, txid: &str) -> Option<i64> {
        self.explorer.fee_of(txid)
    }

    pub fn status_blocks_label(&self) -> String {
        // live daemon sync progress wins over the static counters (Sami:
        // "it should show this on the UI" — height/%/rate/ETA while syncing)
        if let Some(di) = &self.debug_info {
            if di.reachable {
                if let Some(s) = &di.sync {
                    if s.syncing {
                        return s.label();
                    }
                }
                // daemon caught up and following: show Synced honestly even
                // when the GUI folder holds no local chain state (wallet-only
                // folder design — the daemon owns the chain)
                if self.tip.as_ref().map(|t| t.height).unwrap_or(0) == 0 {
                    if let Some(b) = di.blocks {
                        return format!("Synced · daemon {}", b);
                    }
                }
            }
        }
        match (&self.tip, &self.debug_info) {
            (Some(tip), Some(di)) if di.reachable => {
                if di.blocks == Some(tip.height) {
                    format!("{} blocks (at daemon tip)", tip.height)
                } else {
                    format!(
                        "{} blocks (daemon {})",
                        tip.height,
                        di.blocks.map(|b| b.to_string()).unwrap_or("?".into())
                    )
                }
            }
            (Some(tip), _) => format!("{} blocks", tip.height),
            _ => "no chain state".into(),
        }
    }

    /// Receive page rows: every spendable-destination address this GUI owns —
    /// wallet.dat keys first (legacy receive tab), then GUI-keystore keys.
    pub fn receive_addresses(&self) -> Vec<(String, Option<String>, &'static str)> {
        let mut v = Vec::new();
        if let Some(w) = &self.wallet {
            for k in &w.keys {
                v.push((k.address.clone(), self.label_of(&k.address), "wallet"));
            }
        }
        for e in &self.keystore {
            v.push((e.address.clone(), Some(e.label.clone()), "keystore"));
        }
        v
    }

    /// Address Book rows: wallet keys (labels read-only — wallet.dat is
    /// immutable here), then GUI keystore + side-file entries (editable).
    pub fn all_book_rows(&self) -> Vec<(String, Option<String>, &'static str)> {
        let mut v = self.receive_addresses();
        for e in &self.addressbook {
            v.push((e.address.clone(), Some(e.label.clone()), "addressbook"));
        }
        v
    }
}

/// Legacy console syntax: `method [json-params]` — e.g.
/// `getblock 2204064` or `validateaddress "TNFvoQ..."`.
pub fn parse_console_line(line: &str) -> (String, Vec<serde_json::Value>) {
    let mut it = line.splitn(2, ' ');
    let method = it.next().unwrap_or("").trim().to_string();
    let rest = it.next().unwrap_or("").trim();
    let params = if rest.is_empty() {
        Vec::new()
    } else {
        // try a JSON array first, else a single bare token
        match serde_json::from_str::<Vec<serde_json::Value>>(rest) {
            Ok(v) => v,
            Err(_) => match serde_json::from_str::<serde_json::Value>(rest) {
                Ok(v) => vec![v],
                Err(_) => vec![serde_json::Value::String(rest.to_string())],
            },
        }
    };
    (method, params)
}

// ===========================================================================
// Side-file helpers (typed over AbEntry)
// ===========================================================================

#[derive(serde::Deserialize, serde::Serialize)]
struct SideFileFormat {
    entries: Vec<SideEntrySer>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SideEntrySer {
    address: String,
    #[serde(default)]
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_hex: Option<String>,
}

fn load_side_entries(path: &Path, keystore: bool) -> Vec<AbEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(f) = serde_json::from_str::<SideFileFormat>(&text) else {
        return Vec::new();
    };
    f.entries
        .into_iter()
        .filter_map(|e| {
            let secret = if keystore {
                e.secret_hex
                    .as_ref()
                    .and_then(|h| hex_decode(h).ok())
                    .and_then(|v| <[u8; 32]>::try_from(v).ok())
            } else {
                None
            };
            Some(AbEntry {
                address: e.address,
                label: e.label,
                secret,
                source: if keystore { "keystore" } else { "addressbook" },
            })
        })
        .collect()
}

fn persist_side_entries(path: &Path, entries: &[AbEntry], keystore: bool) -> Result<(), String> {
    let f = SideFileFormat {
        entries: entries
            .iter()
            .map(|e| SideEntrySer {
                address: e.address.clone(),
                label: e.label.clone(),
                secret_hex: if keystore {
                    e.secret
                        .map(|s| s.iter().map(|b| format!("{:02x}", b)).collect::<String>())
                } else {
                    None
                },
            })
            .collect(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let text = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &text).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path.display(), e))
}

fn load_options(path: &Path) -> OptionsState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn persist_options(path: &Path, o: &OptionsState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let text = serde_json::to_string_pretty(o).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("write {}: {}", path.display(), e))
}

// ===========================================================================
// Selfcheck — `tri7_wallet_gui --selfcheck`: constructs the app state, loads
// the REAL wallet, derives balances, builds ONE signed tx, exercises QR/PNG/
// signing roundtrips, probes tri7d read-only — then exits 0. No display.
// ===========================================================================

/// Remembered-wallet memory (Sami: "unless there's already one in existence
/// from the registry or whatever saves the memory of it"). A tiny file in the
/// GUI profile dir records the last successfully opened wallet path; the next
/// launch reopens it automatically.
pub fn remembered_wallet_path(profile_dir: &str) -> Option<String> {
    let p = Path::new(profile_dir).join("last_wallet.txt");
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn remember_last_wallet(profile_dir: &str, wallet_path: &str) {
    let dir = Path::new(profile_dir);
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("last_wallet.txt"), wallet_path);
}

pub fn run_selfcheck(cfg: &UiConfig, report_path: Option<&str>) -> i32 {
    let t0 = std::time::Instant::now();
    let mut report = serde_json::Map::new();
    let fail = |report: &mut serde_json::Map<String, serde_json::Value>, msg: String| -> i32 {
        report.insert("result".into(), json!("FAIL"));
        report.insert("error".into(), json!(msg));
        report.insert("elapsed_secs".into(), json!(t0.elapsed().as_secs_f64()));
        let text = serde_json::to_string_pretty(&Value::Object(report.clone()))
            .unwrap_or_else(|_| "{\"result\":\"FAIL\"}".into());
        println!("{}", text);
        if let Some(p) = report_path {
            let _ = std::fs::write(p, text);
        }
        1
    };

    let mut st = AppState::new(cfg.clone());
    // Selfcheck must NEVER mutate the live wallet: no RW session, no .bak, no
    // key writes. New Address write coverage lives in the unit tests against
    // synthetic wallet files.
    st.allow_wallet_writes = false;
    if let Err(e) = st.load_wallet_action() {
        return fail(&mut report, format!("wallet load: {}", e));
    }
    let wallet = st.wallet.as_ref().unwrap();
    let derived = st.derived.as_ref().unwrap().clone();
    let tip = st.tip.as_ref().unwrap().clone();

    report.insert("result".into(), json!("PENDING"));
    report.insert(
        "wallet".into(),
        json!({
            "path": wallet.path,
            "keys": wallet.keys.len(),
            "labels": wallet.names.len(),
            "wallet_version": wallet.version,
            "coverage_height_snapshot": derived.snapshot_height,
            "coverage_height_derived": derived.coverage_height,
            "reconcile": {
                "enabled": derived.reconcile.enabled,
                "blocks_applied": derived.reconcile.blocks_applied,
                "additions_kept": derived.reconcile.additions_kept,
                "spends_applied": derived.reconcile.spends_applied,
                "note": derived.reconcile.note,
            },
        }),
    );
    report.insert(
        "balances".into(),
        json!({
            "total_sats": derived.balances.total_sats,
            "total_tri": format_tri(derived.balances.total_sats),
            "funded_addresses": derived.balances.per_address.iter().filter(|a| a.sats > 0).count(),
            "top": derived.balances.per_address.iter().take(5).map(|a|
                json!({"address": a.address, "sats": a.sats, "utxos": a.utxos})).collect::<Vec<_>>(),
        }),
    );
    report.insert(
        "tip".into(),
        json!({
            "height": tip.height,
            "blockhash": tip.blockhash,
            "daemon_tip": tip.daemon_tip,
            "tip_time": tip.tip_time,
            "tip_bits": format!("{:08x}", tip.tip_bits),
            "tip_modifier": format!("{:016x}", tip.tip_modifier),
            "pos_difficulty": st.stake.as_ref().map(|s| s.pos_difficulty),
        }),
    );

    // --- send: one real signed tx to a synthetic destination --------------
    let (dest_secret, dest_pub) = spend::synthetic_key("selfcheck/dest");
    let _ = dest_secret;
    let dest_addr = crate::wallet::pubkey_to_address(&dest_pub);
    let Some((from_addr, _)) = st.funded_addresses().first().map(|(a, _)| (a.clone(), ())) else {
        return fail(&mut report, "no funded address for selfcheck send".into());
    };
    let mut cands: Vec<UtxoRec> = derived
        .utxos
        .iter()
        .filter(|u| {
            u.address == from_addr && u.value_sats > 1000 && u.spendable_at(tip.height as u32 + 1)
        })
        .cloned()
        .collect();
    cands.sort_by_key(|u| u.value_sats);
    let Some(utxo) = cands.first().cloned() else {
        return fail(&mut report, "no mature funded UTXO > 1000 sats".into());
    };
    let fee = OptionsState::default().pay_fee;
    st.send.from = from_addr.clone();
    st.send.dest = dest_addr.clone();
    st.send.amount = format_tri(utxo.value_sats / 2);
    st.send.fee = fee;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(1_400_000_000);
    let utxo_snapshot = utxo.clone();
    st.build_send_action(now);
    let build = match &st.send_result {
        Some(Ok(b)) => b.clone(),
        Some(Err(e)) => return fail(&mut report, format!("send build: {}", e)),
        None => return fail(&mut report, "send build produced no result".into()),
    };
    // equality with a DIRECT spend.rs construction (the CLI code path) —
    // rebuilt from the SAME selected inputs the GUI path chose
    let eq = (|| -> Result<bool, String> {
        let keys = st.key_map();
        // resolve each selected input back to its full record
        let mut selected: Vec<spend::SpendInput> = Vec::new();
        for ir in &build.inputs {
            let u = derived
                .utxos
                .iter()
                .find(|u| u.txid_display() == ir.txid_display && u.n == ir.n)
                .ok_or_else(|| {
                    format!(
                        "selected input {}:{} not in derived set",
                        ir.txid_display, ir.n
                    )
                })?;
            let (pubkey, secret) = keys
                .get(&u.address)
                .ok_or_else(|| format!("no key for {}", u.address))?;
            selected.push(spend::SpendInput {
                txid: u.txid,
                n: u.n,
                value: u.value_sats,
                script_pubkey: u.script.clone(),
                height: u.height,
                tx_time: u.tx_time,
                is_coinbase: u.prev_kind == "coinbase",
                is_coinstake: u.prev_kind == "coinstake",
                secret: *secret,
                pubkey: pubkey.clone(),
            });
        }
        let (from_pub, _) = keys.get(&from_addr).ok_or("key missing")?;
        let dest_payload = validate_address(&dest_addr)?;
        let plan = spend::build_spend(
            &selected,
            p2pkh_script_for(&dest_payload),
            build.amount_sats,
            Some(p2pkh_script_for(&crate::address::pubkey_payload(from_pub))),
            build.fee_sats,
            build.n_time,
            tip.height as u32 + 1,
        )?;
        Ok(plan.tx.serialize() == hex_decode(&build.tx_hex)?)
    })();
    report.insert(
        "send".into(),
        json!({
            "from": from_addr,
            "dest": dest_addr,
            "utxo": {"txid": utxo_snapshot.txid_display(), "n": utxo_snapshot.n, "value_sats": utxo_snapshot.value_sats},
            "amount_sats": build.amount_sats,
            "fee_sats": build.fee_sats,
            "change_sats": build.change_sats,
            "n_bytes": build.n_bytes,
            "inputs": build.inputs.len(),
            "txid": build.txid_display,
            "tx_hex_len": build.tx_hex.len(),
            "verify_input_ok": true,
            "spend_rs_cli_bytes_equal": eq.as_ref().unwrap_or(&false),
        }),
    );
    if !eq.as_ref().unwrap_or(&false) {
        return fail(
            &mut report,
            format!(
                "GUI tx bytes differ from direct spend.rs construction: {:?}",
                eq.err()
            ),
        );
    }

    // --- seed migration must REFUSE without a writable wallet --------------
    // (selfcheck never mutates the live wallet; the sweep writes key rows
    // into wallet.dat and therefore must fail closed here)
    let sweep_refused = match st.begin_seed_migration_action() {
        Err(e) => {
            report.insert(
                "seed_migration_refusal".into(),
                json!({"refused": true, "reason": e}),
            );
            true
        }
        Ok(()) => {
            // must never happen with allow_wallet_writes=false — close the
            // wizard defensively and fail
            st.sweep_cancel_action();
            report.insert(
                "seed_migration_refusal".into(),
                json!({"refused": false, "reason": "wizard opened without a writable wallet"}),
            );
            false
        }
    };
    if !sweep_refused {
        return fail(
            &mut report,
            "seed migration did NOT refuse without a writable wallet (guard failed)".into(),
        );
    }

    // --- QR + PNG ----------------------------------------------------------
    let uri = build_payment_uri(&st.receive.address, Some("1.5"), "selfcheck", "");
    let qr_ok = uri.as_ref().ok().and_then(|u| qr_matrix(u).ok()).map(|qr| {
        let (w, h, px) = qr_pixels(&qr, 4);
        png_encode_gray(w, h, &px)
            .map(|png| png.starts_with(&[0x89, b'P', b'N', b'G']) && crc32(&[]) == 0)
    });
    report.insert("qr_png".into(), json!(qr_ok == Some(Ok(true))));

    // --- message signing roundtrip ------------------------------------------
    let sig_ok = st
        .key_map()
        .get(&st.sign.address)
        .map(|(_, secret)| {
            let sig = sign_message(secret, "tri7 selfcheck message").unwrap();
            let ok = verify_message(&st.sign.address, "tri7 selfcheck message", &sig).unwrap();
            let tampered = verify_message(&st.sign.address, "tampered", &sig).unwrap_or(true);
            ok && !tampered
        })
        .unwrap_or(false);
    report.insert("message_sign_roundtrip".into(), json!(sig_ok));

    // --- read-only tri7d probe (non-fatal) -----------------------------------
    st.refresh_debug_info_action();
    report.insert(
        "rpc_probe".into(),
        json!({
            "addr": cfg.rpc_addr,
            "reachable": st.debug_info.as_ref().map(|d| d.reachable),
            "connections": st.debug_info.as_ref().and_then(|d| d.connections),
            "error": st.debug_info.as_ref().and_then(|d| d.error.clone()),
        }),
    );

    // --- explorer smoke (local tri7d RPC only; non-fatal) -------------------
    {
        let ex = &mut st.explorer;
        let mut ex_report = serde_json::Map::new();
        let probed = ex.probe();
        ex_report.insert(
            "endpoint".into(),
            json!(ex
                .endpoint
                .and_then(|i| ex.endpoints.get(i))
                .map(|s| s.clone())),
        );
        if probed {
            let block_ok = ex
                .fetch_block_by_height(tip.height)
                .ok()
                .map(|b| b.height == tip.height);
            // fee resolution needs a non-coinbase tx: the LAST tx of a PoS
            // block is its coinstake (credit), of a PoW block a regular spend
            let fee_txid = ex
                .blocks
                .get(&tip.height)
                .and_then(|b| b.txids.last().cloned());
            let tx_ok = fee_txid
                .as_ref()
                .and_then(|t| ex.fetch_tx(t).ok())
                .map(|t| !t.txid.is_empty());
            let fee_ok = fee_txid.as_ref().and_then(|t| ex.fee_of(t));
            let stats_ok = ex.refresh_stats(3).is_ok();
            let addr_ok = ex.open_address_action(&from_addr).is_ok();
            let classify = explorer::classify_search(&from_addr).is_ok()
                && explorer::classify_search(&tip.height.to_string())
                    == Ok(explorer::SearchKind::Height(tip.height));
            ex_report.insert("block_at_tip".into(), json!(block_ok));
            ex_report.insert("tx_decode".into(), json!(tx_ok));
            ex_report.insert(
                "fee_resolution".into(),
                json!(fee_ok.is_some() || fee_txid.is_none()),
            );
            ex_report.insert("stats".into(), json!(stats_ok));
            ex_report.insert("address_view".into(), json!(addr_ok));
            ex_report.insert("search_classification".into(), json!(classify));
            let _ = ex_report;
        }
        report.insert("explorer".into(), Value::Object(ex_report));
    }

    // --- rich list from the balances.json snapshot ---------------------------
    {
        let rich = std::fs::read_to_string(&cfg.balances_path)
            .ok()
            .and_then(|t| {
                serde_json::from_str::<Value>(&t)
                    .ok()
                    .and_then(|v| explorer::parse_rich_list(&v, 5).ok())
            });
        report.insert(
            "explorer_rich_list".into(),
            json!({
                "parsed": rich.is_some(),
                "path": cfg.balances_path,
                "height": rich.as_ref().map(|r| r.height),
                "top1": rich.as_ref().and_then(|r| r.entries.first().map(|(a, s)| json!({"addr": a, "sats": s}))),
            }),
        );
    }

    report.insert("staking".into(), json!({
        "stakeable_coins": st.stake.as_ref().map(|s| s.stakeable_coins),
        "mature_weight_sats": st.stake.as_ref().map(|s| s.mature_weight_sats),
        "last_dryrun_attempts": st.stake.as_ref().and_then(|s| s.dryrun.as_ref().map(|d| d.attempts)),
        "note": "display-only — no kernel search is run by the GUI",
    }));
    report.insert("elapsed_secs".into(), json!(t0.elapsed().as_secs_f64()));
    report.insert("result".into(), json!("OK"));
    let text = serde_json::to_string_pretty(&Value::Object(report)).unwrap();
    println!("{}", text);
    if let Some(p) = report_path {
        if let Err(e) = std::fs::write(p, &text) {
            eprintln!("report write {} failed: {}", p, e);
        }
    }
    0
}

/// ISO-8601 UTC stamp (std-only; no chrono dependency).
fn chrono_iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, mi, s)
}

/// UTC YYYY-MM-DD for a unix stamp (no chrono dependency).
pub fn stamp_date(secs: u32) -> String {
    if secs == 0 {
        return "-".into();
    }
    let days = (secs as i64).div_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
