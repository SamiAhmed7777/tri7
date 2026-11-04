//! Pure state machine for the tri7 GUI wallet (phase 6).
//!
//! NOTHING in this file touches egui or a display: `cargo test` and
//! `tri7_wallet_gui --selfcheck` run headless against these functions. The
//! view layer (`gui::app`, feature `gui`) only renders this state.
//!
//! Data sources:
//!   - wallet.dat          tri7_core::wallet — read-only for balances/spends;
//!                         opened READ-WRITE ([`crate::wallet::WalletHandle`])
//!                         so "New Address" and label edits write REAL legacy
//!                         rows into wallet.dat (keystore is only the fallback
//!                         when the wallet refuses writes)
//!   - wallet_utxos.json   the phase-1e `balances --utxos-out` snapshot
//!   - tipstate.json       phase-2/5a follower state (tip + window entries)
//!   - blk0001.dat + index live-tail reconciliation via the phase-5b
//!                         blockindex caches (heights > snapshot coverage)
//!   - stake_dryrun.json   phase-4 staking dry-run result (display only)
//!   - 127.0.0.1:19114     read-only JSON-RPC probes (Debug window/connections)
//!
//! Legacy identity: page set, labels, palette and icons are carried over from
//! triangles_v5/src/qt (v6.2.6.7, the current legacy wallet). The palette map
//! lives in [`theme`]; the copied PNG assets live in `src/gui/assets/`.

use crate::legacy::{hash_to_display, Block, Hash, Tx};
use crate::spend;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

// ===========================================================================
// Legacy identity — palette (extracted from triangles_v5/src/qt/forms/*.ui)
// ===========================================================================

/// The v6 (Black-Pharao) stylesheet, transplanted:
/// mainwindow.ui — MainWindow/centralWidget #000, QComboBox/QLineEdit bg
/// #1c1c1c, global text #e32105, hover/pressed #3d0e04, QMenu #AF272E with
/// items #deeef1 selected #9D0B17/#E9D9D8, header #AB7F82, disabled #e0e0e0.
pub mod theme {
    pub type Rgb = (u8, u8, u8);

    pub const WINDOW_BG: Rgb = (0x00, 0x00, 0x00); // MainWindow background
    pub const PANEL_BG: Rgb = (0x1c, 0x1c, 0x1c); // inputs / combo boxes
    pub const ACCENT: Rgb = (0xe3, 0x21, 0x05); // the Triangles red
    pub const DEEP_RED: Rgb = (0x3d, 0x0e, 0x04); // hover / selection fill
    pub const MENU_BG: Rgb = (0xAF, 0x27, 0x2E); // QMenu background
    pub const MENU_TEXT: Rgb = (0xde, 0xee, 0xf1); // QMenu::item color
    pub const MENU_SEL_BG: Rgb = (0x9D, 0x0B, 0x17); // QMenu::item:selected bg
    pub const MENU_SEL_TEXT: Rgb = (0xE9, 0xD9, 0xD8); // QMenu::item:selected fg
    pub const HEADER_BG: Rgb = (0xAB, 0x7F, 0x82); // QWidget[is_header=true]
    pub const DISABLED: Rgb = (0xe0, 0xe0, 0xe0); // QLineEdit:disabled
    pub const TEXT_DIM: Rgb = (0x8f, 0x3a, 0x2c); // muted variant of ACCENT

    /// "145,33,5"-style triple, handy for tests.
    pub fn rgb_str(c: Rgb) -> String {
        format!("{},{},{}", c.0, c.1, c.2)
    }
}

// Branding (aboutdialog.ui: "About Cryptographic Triangles"; header logo PNG).
pub const APP_TITLE: &str = "Cryptographic Triangles";
pub const TICKER: &str = "TRI";
/// main.cpp:445 — exact legacy message-signing magic.
pub const MESSAGE_MAGIC: &str = "Triangles Signed Message:\n";
/// Legacy BIP21-ish scheme (qrcodedialog.cpp getURI: "Triangles:<addr>...").
pub const URI_SCHEME: &str = "Triangles:";

/// Legacy sidebar order (mainwindow.ui) + the pages Sami's carry-over spec
/// requires. Overview FIRST, exactly as the legacy wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Send,
    Receive,
    AddressBook,
    Transactions,
    Staking,
    Messages,
    SignMessage,
    VerifyMessage,
    Options,
    Explorer,
}

impl Screen {
    /// Exact legacy button labels (mainwindow.ui <string> entries).
    pub fn label(&self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Send => "Send Coins",
            Screen::Receive => "Receive Coins",
            Screen::AddressBook => "Address Book",
            Screen::Transactions => "Transactions",
            Screen::Staking => "Staking",
            Screen::Messages => "Messages",
            Screen::SignMessage => "Sign Message",
            Screen::VerifyMessage => "Verify Message",
            Screen::Options => "Options",
            Screen::Explorer => "Block Explorer",
        }
    }
    /// menu_16 icon asset stem (assets/menu_16/<stem>.png), carried over from
    /// triangles_v5/src/qt/res/icons/menu_16/.
    pub fn icon(&self) -> &'static str {
        match self {
            Screen::Overview => "overview",
            Screen::Send => "send",
            Screen::Receive => "receive",
            Screen::AddressBook => "addressbook",
            Screen::Transactions => "transactions",
            Screen::Staking => "staking",
            Screen::Messages => "messages",
            Screen::SignMessage => "sign",
            Screen::VerifyMessage => "verify",
            Screen::Options => "options",
            Screen::Explorer => "search",
        }
    }
    pub const NAV_ORDER: [Screen; 11] = [
        Screen::Overview,
        Screen::Send,
        Screen::Receive,
        Screen::AddressBook,
        Screen::Transactions,
        Screen::Staking,
        Screen::Messages,
        Screen::SignMessage,
        Screen::VerifyMessage,
        Screen::Options,
        Screen::Explorer,
    ];
}

// ===========================================================================
// Config
// ===========================================================================

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub wallet_path: String,
    pub state_dir: String, // tipstate.json, wallet_utxos.json, blk0001.dat, index/
    /// explicit --state-dir was passed: the one-folder auto-derivation (from
    /// the wallet file's parent) must NOT override it (CLI-flag only)
    pub state_dir_explicit: bool,
    pub stake_dryrun_path: String,
    pub rpc_addr: Option<String>, // tri7d native RPC (loopback), None = disable
    pub profile_dir: String,      // GUI side files (keystore/addressbook/options)
    pub balances_path: String,    // phase-1e snapshot (explorer rich list)
    pub explorer_endpoints: Vec<String>, // tried in order; 19112 native first
}

impl Default for UiConfig {
    fn default() -> Self {
        // Platform-native defaults: Linux paths on unix, %APPDATA%/-style paths
        // on Windows (the hardcoded /root/... defaults made the Windows build
        // unusable — the load screen fought the operator with unix paths).
        if cfg!(windows) {
            let appdata = std::env::var("APPDATA")
                .unwrap_or_else(|_| format!("{}\\AppData\\Roaming", std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Public".into())));
            let local = std::env::var("LOCALAPPDATA")
                .unwrap_or_else(|_| format!("{}\\AppData\\Local", std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Public".into())));
            UiConfig {
                wallet_path: format!("{appdata}\\Triangles\\wallet.dat"),
                state_dir: format!("{local}\\Tri7Wallet\\state"),
                state_dir_explicit: false,
                stake_dryrun_path: format!("{local}\\Tri7Wallet\\stake_dryrun.json"),
                rpc_addr: Some("127.0.0.1:19114".into()),
                profile_dir: format!("{local}\\Tri7Wallet\\profile"),
                balances_path: format!("{local}\\Tri7Wallet\\balances.json"),
                explorer_endpoints: explorer_defaults(),
            }
        } else {
            UiConfig {
                wallet_path: "/root/.triangles/wallet.dat".into(),
                state_dir: "/var/lib/tri7".into(),
                state_dir_explicit: false,
                stake_dryrun_path: "/root/tri7/stake_dryrun.json".into(),
                rpc_addr: Some("127.0.0.1:19112".into()),
                profile_dir: "/root/.tri7_gui".into(),
                balances_path: "/root/tri7/balances.json".into(),
                explorer_endpoints: explorer_defaults(),
            }
        }
    }
}

/// Explorer endpoints: the native tri7d surface on 19112 first (post-cutover),
/// then the transitional 19114 port, then whatever rpc_addr says.
fn explorer_defaults() -> Vec<String> {
    vec!["127.0.0.1:19112".into(), "127.0.0.1:19114".into()]
}

impl UiConfig {
    pub fn tipstate_path(&self) -> PathBuf {
        Path::new(&self.state_dir).join("tipstate.json")
    }
    pub fn utxo_dump_path(&self) -> PathBuf {
        Path::new(&self.state_dir).join("wallet_utxos.json")
    }
    pub fn archive_path(&self) -> PathBuf {
        Path::new(&self.state_dir).join("blk0001.dat")
    }
    pub fn index_dir(&self) -> PathBuf {
        Path::new(&self.state_dir).join("index")
    }
    pub fn keystore_path(&self) -> PathBuf {
        Path::new(&self.profile_dir).join("keystore.json")
    }
    pub fn addressbook_path(&self) -> PathBuf {
        Path::new(&self.profile_dir).join("addressbook.json")
    }
    pub fn options_path(&self) -> PathBuf {
        Path::new(&self.profile_dir).join("options.json")
    }
}

// ===========================================================================
// Amounts — TRI has 6 decimals (COIN = 1_000_000, consensus.rs)
// ===========================================================================

pub fn format_tri(sats: i64) -> String {
    let neg = sats < 0;
    let abs = sats.unsigned_abs();
    let whole = abs / 1_000_000;
    let frac = abs % 1_000_000;
    if neg && sats != 0 {
        format!("-{}.{}", whole, format!("{:06}", frac))
    } else {
        format!("{}.{}", whole, format!("{:06}", frac))
    }
}

/// Parse a TRI amount string (legacy BitcoinAmountField semantics: 6-dp
/// fixed-point, no exponent). Returns sats.
pub fn parse_tri(s: &str) -> Result<i64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty amount".into());
    }
    let (whole, frac) = match t.split_once('.') {
        Some((w, f)) => (w, f),
        None => (t, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err("empty amount".into());
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid amount \"{}\"", s));
    }
    if frac.len() > 6 {
        return Err(format!("more than 6 decimals: \"{}\"", s));
    }
    let whole_v: u64 = if whole.is_empty() {
        0
    } else {
        whole
            .parse()
            .map_err(|_| format!("invalid amount \"{}\"", s))?
    };
    let mut frac_v: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse()
            .map_err(|_| format!("invalid amount \"{}\"", s))?
    };
    for _ in frac.len()..6 {
        frac_v *= 10;
    }
    let sats = whole_v
        .checked_mul(1_000_000)
        .and_then(|w| w.checked_add(frac_v))
        .ok_or_else(|| format!("amount out of range: \"{}\"", s))?;
    if sats > spend::MAX_MONEY as u64 {
        return Err(format!("amount out of MoneyRange: \"{}\"", s));
    }
    Ok(sats as i64)
}

// ===========================================================================
// Addresses
// ===========================================================================

// ---------------------------------------------------------------------------
// base58 decode (legacy DecodeBase58Check) — address.rs only ships the
// encoder, so the GUI carries the decoder: big-number division over the
// same 58-char alphabet, leading '1's → zero bytes.
// ---------------------------------------------------------------------------

pub fn is_base58_char(c: char) -> bool {
    const ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    ALPHABET.contains(c)
}

pub fn base58_decode(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut map = [255u8; 128];
    for (i, &c) in ALPHABET.iter().enumerate() {
        map[c as usize] = i as u8;
    }
    let mut zeros = 0usize;
    let bytes = s.as_bytes();
    while zeros < bytes.len() && bytes[zeros] == b'1' {
        zeros += 1;
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() * 137 / 100 + 1);
    for &c in &bytes[zeros..] {
        let val = *map
            .get(c as usize)
            .filter(|&&v| v != 255)
            .ok_or_else(|| format!("invalid base58 character {:?}", c as char))?;
        let mut carry = val as u32;
        for byte in out.iter_mut().rev() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut full = vec![0u8; zeros];
    full.extend_from_slice(&out);
    Ok(full)
}

/// base58check decode + version-byte check (PUBKEY_ADDRESS = 65, 'T…').
/// Returns the 21-byte payload on success.
pub fn validate_address(s: &str) -> Result<[u8; 21], String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("address is empty".into());
    }
    if t.chars().any(|c| !is_base58_char(c)) {
        return Err(format!("\"{}\" contains non-base58 characters", t));
    }
    let raw = base58_decode(t)?;
    if raw.len() != 25 {
        return Err(format!("address length {} != 25", raw.len()));
    }
    let (payload, checksum) = raw.split_at(21);
    let expect = crate::legacy::sha256d(payload);
    if checksum != &expect[..4] {
        return Err("address checksum mismatch".into());
    }
    if payload[0] != crate::address::PUBKEY_ADDRESS {
        return Err(format!(
            "wrong version byte {:#04x} (expected {:#04x})",
            payload[0],
            crate::address::PUBKEY_ADDRESS
        ));
    }
    let mut out = [0u8; 21];
    out.copy_from_slice(payload);
    Ok(out)
}

/// P2PKH scriptPubKey for a validated address payload.
pub fn p2pkh_script_for(payload: &[u8; 21]) -> Vec<u8> {
    let mut s = Vec::with_capacity(25);
    s.push(spend::OP_DUP);
    s.push(spend::OP_HASH160);
    s.push(20);
    s.extend_from_slice(&payload[1..]);
    s.push(spend::OP_EQUALVERIFY);
    s.push(spend::OP_CHECKSIG);
    s
}

// ===========================================================================
// tipstate.json (chain panel) — light parse, own struct (follow_tip owns its)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct TipInfo {
    pub height: u64,
    pub blockhash: String,
    pub updated_utc: String,
    pub last_advance_utc: String,
    pub window_base: u64,
    pub daemon_tip: Option<i64>,
    pub tip_time: u32,
    pub tip_bits: u32,
    pub tip_modifier: u64,
    pub tip_generated: bool,
    pub tip_pos: bool,
}

#[derive(Deserialize)]
struct TipStateLight {
    height: u64,
    blockhash: String,
    #[serde(default)]
    updated_utc: String,
    #[serde(default)]
    last_advance_utc: String,
    window_base: u64,
    #[serde(default)]
    daemon_tip: Option<i64>,
    #[serde(default)]
    entries: Vec<TipEntryLight>,
}

#[derive(Deserialize)]
struct TipEntryLight {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    time: u32,
    #[serde(default)]
    bits: u32,
    #[serde(default)]
    pos: bool,
    #[serde(default)]
    modifier: u64,
    #[serde(default)]
    generated: bool,
}

pub fn load_tipstate(path: &Path) -> Result<TipInfo, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // MISSING STATE FILE IS FINE: a brand-new wallet folder has no
            // chain state yet. Open with an empty local state - live chain
            // data flows from tri7d RPC. (Sami directive: the chosen folder
            // IS the wallet environment; nothing blocks opening the wallet.)
            return Ok(TipInfo {
                height: 0,
                blockhash: String::new(),
                updated_utc: String::new(),
                last_advance_utc: String::new(),
                window_base: 0,
                daemon_tip: None,
                tip_time: 0,
                tip_bits: 0,
                tip_modifier: 0,
                tip_generated: false,
                tip_pos: false,
            });
        }
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    let f: TipStateLight =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let last = f.entries.last();
    Ok(TipInfo {
        height: f.height,
        blockhash: f.blockhash,
        updated_utc: f.updated_utc,
        last_advance_utc: f.last_advance_utc,
        window_base: f.window_base,
        daemon_tip: f.daemon_tip,
        tip_time: last.map(|e| e.time).unwrap_or(0),
        tip_bits: last.map(|e| e.bits).unwrap_or(0),
        tip_modifier: last.map(|e| e.modifier).unwrap_or(0),
        tip_generated: last.map(|e| e.generated).unwrap_or(false),
        tip_pos: last.map(|e| e.pos).unwrap_or(false),
    })
}

/// Oracle for blockindex::load_caches — EXACTLY the construction tri7d uses
/// (follow_tip.rs: daemon-anchored window hashes from tipstate entries).
pub fn build_oracle(_tip: &TipInfo, path: &Path) -> Result<crate::blockindex::BuildOracle, String> {
    // MISSING tipstate.json is fine (Sami directive: the chosen folder is the
    // whole wallet environment; a fresh folder has no chain state yet). An
    // empty oracle means the index caches can't be anchored -> tail
    // reconciliation reports "showing the snapshot as-is" and balances come
    // from the wallet keys + tri7d RPC. Never block opening the wallet.
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(crate::blockindex::BuildOracle {
                base: 0,
                hashes: Vec::new(),
                tip: 0,
            });
        }
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    let f: TipStateLight =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let mut hashes = Vec::with_capacity(f.entries.len());
    for e in &f.entries {
        hashes.push(crate::p2p::display_to_internal(&e.hash)?);
    }
    Ok(crate::blockindex::BuildOracle {
        base: f.window_base,
        hashes,
        tip: f.height,
    })
}

// ===========================================================================
// Wallet
// ===========================================================================

pub struct WalletData {
    pub path: String,
    pub keys: Vec<crate::wallet::WalletKey>,
    pub names: Vec<(String, String)>,
    pub default_key: Option<Vec<u8>>,
    pub version: Option<i32>,
    pub best_block: Option<Hash>,
    /// Seed wallet (legacy `hdmnemonic` row): the stored phrase. None = the
    /// wallet is a pre-HD random-key wallet (which can NEVER gain a seed).
    pub hd_mnemonic: Option<String>,
    /// BIP39 passphrase row (absent = none).
    pub hd_passphrase: Option<String>,
    /// Next HD derivation index (legacy `hdchain` counter).
    pub hd_chain_index: i64,
    /// Seed-migration sweep completed (the `seedmigrated` setting row) —
    /// doubles as the double-sweep guard on the GUI surface.
    pub seed_migrated: bool,
}

pub fn load_wallet(path: &Path) -> Result<WalletData, String> {
    let w = crate::wallet::read_wallet(path)?;
    Ok(WalletData {
        path: path.display().to_string(),
        keys: w.keys,
        names: w.names,
        default_key: w.default_key,
        version: w.version,
        best_block: w.best_block,
        hd_mnemonic: w.hd_mnemonic,
        hd_passphrase: w.hd_passphrase,
        hd_chain_index: w.hd_chain_index.unwrap_or(0),
        seed_migrated: crate::wallet::seed_migration_done(path)?,
    })
}

impl WalletData {
    /// address -> (secret, pubkey) lookup for signing.
    pub fn key_by_address(&self) -> HashMap<&str, &crate::wallet::WalletKey> {
        self.keys.iter().map(|k| (k.address.as_str(), k)).collect()
    }
    pub fn label_of(&self, address: &str) -> Option<&str> {
        self.names
            .iter()
            .find(|(a, _)| a == address)
            .map(|(_, l)| l.as_str())
    }
}

// ===========================================================================
// UTXO derivation: snapshot + live-tail reconciliation
// ===========================================================================

#[derive(Debug, Clone)]
pub struct UtxoRec {
    pub txid: Hash, // internal byte order (vin.prev_hash form)
    pub n: u32,
    pub value_sats: i64,
    pub script: Vec<u8>,
    pub address: String,
    pub height: u32,
    /// chain-stored transaction timestamp (unix seconds) — the same field the
    /// legacy client dates its history from, so dates match across clients
    pub tx_time: u32,
    pub prev_kind: String, // "regular" | "coinbase" | "coinstake"
}

impl UtxoRec {
    pub fn txid_display(&self) -> String {
        hash_to_display(&self.txid)
    }
    /// Coinbase outputs need COINBASE_MATURITY confirmations to spend
    /// (main.cpp ConnectInputs); stake/regular outputs are spendable once in
    /// a block.
    pub fn spendable_at(&self, spend_height: u32) -> bool {
        if self.prev_kind == "coinbase" {
            spend_height.saturating_sub(self.height) >= spend::COINBASE_MATURITY as u32
        } else {
            true
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UtxoDump {
    pub height: u32,
    #[serde(default)]
    pub covered_tip_hash: String,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub total_value_sats: i64,
    pub utxos: Vec<DumpUtxo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DumpUtxo {
    pub txid: String,
    pub n: u32,
    pub value_sats: i64,
    pub script_hex: String,
    #[serde(default)]
    pub address: String,
    pub height: u32,
    pub tx_time: u32,
    pub prev_kind: String,
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| format!("hex: {}", e)))
        .collect()
}

pub fn load_utxo_dump(path: &Path) -> Result<UtxoDump, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // no UTXO snapshot in the folder yet - the GUI shows balances via
            // the wallet keys + tri7d RPC instead (Sami directive: the chosen
            // folder is the whole wallet environment; missing side files never
            // block opening)
            return Ok(UtxoDump {
                height: 0,
                covered_tip_hash: String::new(),
                count: 0,
                total_value_sats: 0,
                utxos: Vec::new(),
            });
        }
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    let d: UtxoDump =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    if d.utxos.is_empty() {
        return Err(format!("dump {} has zero UTXOs", path.display()));
    }
    Ok(d)
}

impl UtxoDump {
    pub fn into_recs(self) -> Result<Vec<UtxoRec>, String> {
        self.utxos
            .into_iter()
            .map(|u| {
                let txid_disp = u.txid.clone();
                let txid = display_txid_to_internal(&txid_disp)?;
                Ok(UtxoRec {
                    txid,
                    n: u.n,
                    value_sats: u.value_sats,
                    script: hex_decode(&u.script_hex)?,
                    address: u.address,
                    height: u.height,
                    tx_time: u.tx_time,
                    prev_kind: u.prev_kind,
                })
            })
            .collect()
    }
}

/// The dump's txid is DISPLAY order (uint256::ToString prints internal bytes
/// reversed) — vin.prev_hash needs internal order.
pub fn display_txid_to_internal(s: &str) -> Result<Hash, String> {
    let b = hex_decode(s).map_err(|e| format!("txid {}: {}", s, e))?;
    if b.len() != 32 {
        return Err(format!("txid {} length != 32", s));
    }
    let mut out = [0u8; 32];
    for (i, byte) in b.iter().enumerate() {
        out[31 - i] = *byte;
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    pub enabled: bool,
    pub blocks_applied: usize,
    pub additions_found: usize,
    pub additions_kept: usize,
    pub spends_applied: usize,
    pub heights: (u32, u64), // (snapshot coverage, applied up to)
    pub source: String,
    pub note: String,
}

/// Derive the live wallet UTXO set: the snapshot, then every live block ABOVE
/// the snapshot's coverage applied by (txid, n): spend removals + wallet-owned
/// additions (additions that were themselves spent later in the tail are
/// dropped, exactly as stake_mint.rs does).
///
/// Offsets come from the phase-5b blockindex caches; `offsets[i]` is the frame
/// offset of height (dump_height + 1 + i) in the archive. This split (offsets
/// injected) is the testable seam; [`tail_offsets`] resolves the real thing.
pub fn reconcile_tail_with_offsets(
    snapshot: Vec<UtxoRec>,
    snapshot_height: u32,
    snapshot_tip_hash: Option<&str>,
    wallet_payloads: &HashSet<[u8; 21]>,
    archive: &Path,
    offsets: &[u64],
    tip_height: u64,
) -> Result<(Vec<UtxoRec>, ReconcileReport), String> {
    let mut report = ReconcileReport {
        enabled: true,
        heights: (snapshot_height, tip_height),
        source: "blockindex height→offset + blk0001.dat".into(),
        ..Default::default()
    };
    if offsets.is_empty() {
        report.note = "no tail blocks above the snapshot".into();
        return Ok((snapshot, report));
    }
    let mut file =
        std::fs::File::open(archive).map_err(|e| format!("open {}: {}", archive.display(), e))?;
    let mut spent: HashSet<(Hash, u32)> = HashSet::new();
    let mut additions: Vec<UtxoRec> = Vec::new();
    let mut additions_found_total = 0usize;
    let mut prev: Option<Hash> = snapshot_tip_hash
        .map(display_txid_to_internal)
        .transpose()?;

    for (i, off) in offsets.iter().enumerate() {
        let h = snapshot_height as u64 + 1 + i as u64;
        let buf = read_frame_at(&mut file, *off)?;
        let mut r = crate::legacy::Reader::new(&buf);
        let b = Block::read(&mut r)?;
        // Integrity gate: live-tail chain must link exactly (the archive has
        // interleaved stale frames — a wrong offset would break this instantly).
        if let Some(p) = prev {
            if b.prev_hash != p {
                return Err(format!(
                    "tail block h{} does not link to its predecessor (prev {:?} != expected {:?})",
                    h,
                    hash_to_display(&b.prev_hash),
                    hash_to_display(&p)
                ));
            }
        }
        prev = Some(b.hash());
        let before = additions.len();
        apply_tail_bookkeeping(
            &b,
            h as u32,
            snapshot_height,
            wallet_payloads,
            &mut spent,
            &mut additions,
        );
        additions_found_total += additions.len() - before;
        report.blocks_applied += 1;
    }

    let kept: Vec<UtxoRec> = additions
        .into_iter()
        .filter(|u| !spent.contains(&(u.txid, u.n)))
        .collect();
    report.additions_found = kept.len() + additions_found_total;
    report.additions_kept = kept.len();
    report.spends_applied = spent.len();
    let mut out: Vec<UtxoRec> = snapshot
        .into_iter()
        .filter(|u| !spent.contains(&(u.txid, u.n)))
        .collect();
    out.extend(kept.iter().cloned());
    out.sort_by(|a, b| (a.height, a.txid, a.n).cmp(&(b.height, b.txid, b.n)));
    Ok((out, report))
}

/// Phase-4 parity: wallet reconciliation for blocks ABOVE the snapshot
/// (stake_mint.rs apply_tail_bookkeeping).
fn apply_tail_bookkeeping(
    b: &Block,
    height_abs: u32,
    snapshot_height: u32,
    wallet_payloads: &HashSet<[u8; 21]>,
    spent: &mut HashSet<(Hash, u32)>,
    additions: &mut Vec<UtxoRec>,
) {
    if height_abs <= snapshot_height {
        return;
    }
    let is_coinbase = |tx: &Tx| {
        tx.vin.len() == 1 && tx.vin[0].prev_hash == [0u8; 32] && tx.vin[0].prev_n == u32::MAX
    };
    for tx in &b.vtx {
        if !is_coinbase(tx) {
            for vin in &tx.vin {
                spent.insert((vin.prev_hash, vin.prev_n));
            }
        }
        let kind = if is_coinbase(tx) {
            "coinbase"
        } else if crate::consensus::tx_is_coin_stake(tx) {
            "coinstake"
        } else {
            "regular"
        };
        let txid = tx.hash();
        let tx_time = tx.time;
        for (n, vout) in tx.vout.iter().enumerate() {
            if vout.value == 0 && vout.script_pubkey.is_empty() {
                continue; // IsEmpty marker
            }
            let Some(dest) = crate::address::script_to_dest(&vout.script_pubkey) else {
                continue;
            };
            let payload = dest.payload();
            if !wallet_payloads.contains(&payload) {
                continue;
            }
            additions.push(UtxoRec {
                txid,
                n: n as u32,
                value_sats: vout.value,
                script: vout.script_pubkey.clone(),
                address: crate::address::payload_to_address(&payload),
                height: height_abs,
                tx_time,
                prev_kind: kind.into(),
            });
        }
    }
}

/// Read one blk0001.dat frame at a known offset (magic u32 LE + len u32 LE +
/// data). Frame magic = the wire magic [70 35 22 05] LE = 0x0522_3570.
pub fn read_frame_at(f: &mut std::fs::File, off: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    f.seek(SeekFrom::Start(off))
        .map_err(|e| format!("seek {}: {}", off, e))?;
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr)
        .map_err(|e| format!("frame header @{}: {}", off, e))?;
    let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    if magic != 0x0522_3570 {
        return Err(format!("frame @{}: bad magic {:#010x}", off, magic));
    }
    let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    if len > 8 << 20 {
        return Err(format!("frame @{}: implausible length {}", off, len));
    }
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)
        .map_err(|e| format!("frame body @{}: {}", off, e))?;
    Ok(buf)
}

/// Resolve tail offsets via the tri7d blockindex caches (loads <1s per
/// HANDOFF phase 5b). Requires `heights_ok` (authoritative heights).
pub fn tail_offsets(
    index_dir: &Path,
    archive: &Path,
    oracle: &crate::blockindex::BuildOracle,
    snapshot_height: u32,
    tip_height: u64,
) -> Result<Option<Vec<u64>>, String> {
    let blk_len = std::fs::metadata(archive)
        .map_err(|e| format!("stat {}: {}", archive.display(), e))?
        .len();
    let build = crate::blockindex::load_caches(index_dir, blk_len, Some(oracle))?;
    let Some(ib) = build else {
        return Ok(None);
    };
    if !ib.heights_ok {
        return Ok(None);
    }
    let mut offs = Vec::new();
    for h in snapshot_height as u64 + 1..=tip_height {
        match ib.blocks.offset_by_height(h) {
            Some(off) => offs.push(off),
            None => return Ok(None), // gap — refuse instead of partial truth
        }
    }
    Ok(Some(offs))
}

// ===========================================================================
// Balances
// ===========================================================================

#[derive(Debug, Clone)]
pub struct AddressBalance {
    pub address: String,
    pub label: Option<String>,
    pub sats: i64,
    pub utxos: usize,
}

#[derive(Debug, Clone)]
pub struct Balances {
    pub total_sats: i64,
    pub per_address: Vec<AddressBalance>,
}

/// Balance header aggregation: total + per-address from the UTXO set.
pub fn compute_balances(utxos: &[UtxoRec], label_of: &dyn Fn(&str) -> Option<String>) -> Balances {
    let mut agg: HashMap<String, (i64, usize)> = HashMap::new();
    let mut total = 0i64;
    for u in utxos {
        let e = agg.entry(u.address.clone()).or_insert((0, 0));
        e.0 = e.0.saturating_add(u.value_sats);
        e.1 += 1;
        total = total.saturating_add(u.value_sats);
    }
    let mut per: Vec<AddressBalance> = agg
        .into_iter()
        .map(|(address, (sats, utxo_n))| AddressBalance {
            label: label_of(&address),
            address,
            sats,
            utxos: utxo_n,
        })
        .collect();
    per.sort_by(|a, b| b.sats.cmp(&a.sats).then(a.address.cmp(&b.address)));
    Balances {
        total_sats: total,
        per_address: per,
    }
}

#[derive(Debug, Clone)]
pub struct DerivedWallet {
    pub utxos: Vec<UtxoRec>,
    pub balances: Balances,
    pub snapshot_height: u32,
    pub coverage_height: u64, // snapshot height, or tip after reconciliation
    pub reconcile: ReconcileReport,
}

/// Full derivation pass: snapshot → tail reconciliation (best effort) →
/// balances. Falls back to snapshot-only with an explanatory note when the
/// blockindex caches are absent/stale. `label_of` resolves address labels
/// (wallet names + GUI address book).
pub fn derive_wallet(
    dump: UtxoDump,
    wallet_payloads: HashSet<[u8; 21]>,
    cfg: &UiConfig,
    tip: &TipInfo,
    label_of: &dyn Fn(&str) -> Option<String>,
) -> Result<DerivedWallet, String> {
    let snapshot_height = dump.height;
    let snapshot_tip = dump.covered_tip_hash.clone();
    let snapshot = dump.into_recs()?;
    let (utxos, reconcile) = match tail_offsets(
        &cfg.index_dir(),
        &cfg.archive_path(),
        &build_oracle(tip, &cfg.tipstate_path())?,
        snapshot_height,
        tip.height,
    ) {
        Ok(Some(offsets)) => reconcile_tail_with_offsets(
            snapshot,
            snapshot_height,
            Some(snapshot_tip.as_str()),
            &wallet_payloads,
            &cfg.archive_path(),
            &offsets,
            tip.height,
        )?,
        Ok(None) => {
            let mut r = ReconcileReport::default();
            r.note = "blockindex caches absent/stale — showing the snapshot as-is".into();
            (snapshot, r)
        }
        Err(e) => {
            let mut r = ReconcileReport::default();
            r.note = format!("tail reconciliation failed: {} — showing the snapshot", e);
            (snapshot, r)
        }
    };
    // coverage_height = tip.height ONLY when reconciliation actually ran over
    // the tail (Ok(Some) arm — reconcile.enabled means the UTXOs were rebuilt
    // to `tip.height`). The Ok(None)/Err fallbacks return the SNAPSHOT
    // untouched, so their coverage stays the snapshot height: reconcile.enabled
    // is false in exactly those arms, making the pairing sound. Keep the two
    // in lockstep if you touch the match above.
    let coverage = if reconcile.enabled {
        tip.height
    } else {
        snapshot_height as u64
    };
    let balances = compute_balances(&utxos, label_of);
    Ok(DerivedWallet {
        utxos,
        balances,
        snapshot_height,
        coverage_height: coverage,
        reconcile,
    })
}

// ===========================================================================
// Send (sign-only; NO broadcast exists in this build)
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub struct SendFormState {
    pub from: String,
    pub dest: String,
    pub amount: String,
    pub fee: String,
    /// Coin Control: manually selected input indices into derived.utxos
    /// (empty = automatic largest-first selection).
    pub selected: Vec<usize>,
    pub use_custom_change: bool,
    pub custom_change: String,
}

#[derive(Debug, Clone)]
pub struct SendRequest {
    pub from: String,
    pub dest_payload: [u8; 21],
    pub amount_sats: i64,
    pub fee_sats: i64,
    pub change_payload: [u8; 21],
}

/// Validate the form exactly like the legacy Send Coins dialog (labels from
/// sendcoinsdialog.ui). Returns the resolved request.
pub fn validate_send_form(
    form: &SendFormState,
    derived: &DerivedWallet,
    keys: &HashMap<String, (Vec<u8>, [u8; 32])>, // address -> (pubkey, secret)
) -> Result<SendRequest, String> {
    if form.from.is_empty() {
        return Err("Select a sending address (from-address)".into());
    }
    let (_, _from_secret) = keys
        .get(&form.from)
        .ok_or_else(|| format!("\"{}\" is not a wallet key address", form.from))?;
    let dest_payload = validate_address(&form.dest)
        .map_err(|e| format!("The recipient address is not valid: {}", e))?;
    let amount =
        parse_tri(&form.amount).map_err(|e| format!("The amount to pay is not valid: {}", e))?;
    if amount <= 0 {
        return Err("The amount to pay must be larger than 0".into());
    }
    let fee = if form.fee.trim().is_empty() {
        0i64
    } else {
        parse_tri(&form.fee).map_err(|e| format!("The fee is not valid: {}", e))?
    };
    let change_payload = if form.use_custom_change && !form.custom_change.trim().is_empty() {
        validate_address(&form.custom_change)
            .map_err(|e| format!("The custom change address is not valid: {}", e))?
    } else {
        // change returns to the sending address (P2PKH of the same key)
        let (from_pub, _) = keys
            .get(&form.from)
            .ok_or_else(|| "from key vanished".to_string())?;
        crate::address::pubkey_payload(from_pub)
    };
    // funds check against the from-address pool (or the coin-control selection)
    let pool: Vec<&UtxoRec> = if form.selected.is_empty() {
        derived
            .utxos
            .iter()
            .filter(|u| u.address == form.from)
            .collect()
    } else {
        let mut v = Vec::new();
        for i in &form.selected {
            let u = derived
                .utxos
                .get(*i)
                .ok_or_else(|| format!("coin control index {} out of range", i))?;
            if u.address != form.from {
                return Err("Coin control inputs must belong to the sending address".into());
            }
            v.push(u);
        }
        v
    };
    let avail: i64 = pool.iter().map(|u| u.value_sats).sum();
    if amount + fee > avail {
        return Err("Insufficient funds!".into());
    }
    Ok(SendRequest {
        from: form.from.clone(),
        dest_payload,
        amount_sats: amount,
        fee_sats: fee,
        change_payload,
    })
}

#[derive(Debug, Clone)]
pub struct SendBuild {
    pub txid_display: String,
    pub tx_hex: String,
    pub inputs: Vec<InputRef>,
    pub amount_sats: i64,
    pub fee_sats: i64,
    pub change_sats: i64,
    pub n_bytes: usize,
    pub n_time: u32,
}

#[derive(Debug, Clone)]
pub struct InputRef {
    pub txid_display: String,
    pub n: u32,
    pub value_sats: i64,
    pub address: String,
}

/// Build + sign a spend from the GUI state via spend::build_spend — the SAME
/// bytes the spend.rs CLI path produces for identical inputs (asserted by
/// tests and by --selfcheck). Signing is offline; nothing is broadcast.
pub fn build_signed_send(
    req: &SendRequest,
    derived: &DerivedWallet,
    keys: &HashMap<String, (Vec<u8>, [u8; 32])>,
    manual_pool: Option<&[UtxoRec]>,
    now: u32,
    tip_height: u32,
) -> Result<SendBuild, String> {
    if req.amount_sats <= 0 {
        return Err("The amount to pay must be larger than 0".into());
    }
    // candidate pool: coin-control selection, else the from-address's UTXOs.
    // BOTH paths apply the same maturity filter — a manually selected
    // immature coinbase/coinstake must be rejected at SELECTION time, not
    // handed to consensus to fail later.
    let pool_owned: Vec<UtxoRec>;
    let pool: Vec<UtxoRec> = match manual_pool {
        Some(p) => {
            for u in p {
                if !u.spendable_at(tip_height + 1) {
                    return Err(
                        "A selected coin-control input is not yet spendable (immature coinbase/coinstake — uncheck it)"
                            .to_string(),
                    );
                }
            }
            p.to_vec()
        }
        None => {
            pool_owned = derived
                .utxos
                .iter()
                .filter(|u| u.address == req.from && u.spendable_at(tip_height + 1))
                .cloned()
                .collect();
            pool_owned
        }
    };
    if pool.is_empty() {
        return Err("No spendable inputs for the selected address".into());
    }
    let inputs: Vec<spend::SpendInput> = pool
        .iter()
        .map(|u| {
            let (pubkey, secret) = keys
                .get(&u.address)
                .ok_or_else(|| format!("no key for {}", u.address))?;
            Ok(spend::SpendInput {
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
            })
        })
        .collect::<Result<_, String>>()?;

    let target = req
        .amount_sats
        .checked_add(req.fee_sats)
        .ok_or_else(|| "amount + fee overflow".to_string())?;
    // Legacy dialog wording (sendcoinsdialog.ui) — checked before selection so
    // the operator sees the exact legacy error, not the raw selector text.
    let pool_total: i64 = pool.iter().map(|u| u.value_sats).sum();
    if target > pool_total {
        return Err("Insufficient funds!".into());
    }
    let selected = spend::select_utxos_covering(&inputs, target)?;
    let dest_script = p2pkh_script_for(&req.dest_payload);
    let change_script = p2pkh_script_for(&req.change_payload);
    let n_time = now.max(selected.iter().map(|i| i.tx_time).max().unwrap_or(0));
    let spend_height = tip_height + 1;
    let plan = spend::build_spend(
        &selected,
        dest_script,
        req.amount_sats,
        Some(change_script),
        req.fee_sats,
        n_time,
        spend_height,
    )?;
    // self-verify exactly like the CLI dry runs
    for (i, input) in selected.iter().enumerate() {
        spend::verify_input(&plan.tx, i, &input.script_pubkey)?;
    }
    spend::connect_inputs_check(&plan.tx, &selected, spend_height)?;
    Ok(SendBuild {
        txid_display: hash_to_display(&plan.tx.hash()),
        tx_hex: plan
            .tx
            .serialize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect(),
        inputs: selected
            .iter()
            .map(|i| InputRef {
                txid_display: hash_to_display(&i.txid),
                n: i.n,
                value_sats: i.value,
                address: String::new(),
            })
            .collect(),
        amount_sats: plan.tx.vout[0].value,
        fee_sats: plan.fee,
        change_sats: plan.change,
        n_bytes: plan.tx.serialize().len(),
        n_time,
    })
}

// ===========================================================================
// Staking panel (read-only)
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub struct LastDryrun {
    pub attempts: u64,
    pub kernel_hits: u64,
    pub coins_considered: u64,
    pub coins_meeting_min_age: u64,
    pub reward_sats: i64,
    pub n_time: u32,
    pub split: bool,
    pub block_hash: String,
    pub search_time: u64,
}

#[derive(Debug, Clone)]
pub struct StakeStatus {
    pub dryrun: Option<LastDryrun>,
    pub pos_difficulty: f64,
    pub modifier: u64,
    pub modifier_generated: bool,
    /// mature stakeable weight: sum of UTXOs whose containing block is at
    /// least STAKE_MIN_AGE old (kernel min-age filter).
    pub mature_weight_sats: i64,
    pub stakeable_coins: usize,
    pub note: String,
}

pub fn load_stake_status(dryrun_path: &Path, tip: &TipInfo, utxos: &[UtxoRec]) -> StakeStatus {
    let dryrun = load_last_dryrun(dryrun_path);
    let tip_time = tip.tip_time as i64;
    let mut mature = 0i64;
    let mut coins = 0usize;
    for u in utxos {
        if u.value_sats > 0
            && (u.tx_time as i64) + crate::consensus::STAKE_MIN_AGE as i64 <= tip_time.max(0)
        {
            mature = mature.saturating_add(u.value_sats);
            coins += 1;
        }
    }
    StakeStatus {
        pos_difficulty: crate::rpc::difficulty_from_bits(tip.tip_bits),
        modifier: tip.tip_modifier,
        modifier_generated: tip.tip_generated,
        mature_weight_sats: mature,
        stakeable_coins: coins,
        dryrun,
        note: if tip.tip_pos {
            "tip is proof-of-stake".into()
        } else {
            "tip is proof-of-work".into()
        },
    }
}

#[derive(Deserialize)]
struct DryrunFile {
    #[serde(default)]
    block: serde_json::Value,
    #[serde(default)]
    coinstake: serde_json::Value,
    #[serde(default)]
    kernel_search: serde_json::Value,
}

pub fn load_last_dryrun(path: &Path) -> Option<LastDryrun> {
    let text = std::fs::read_to_string(path).ok()?;
    let f: DryrunFile = serde_json::from_str(&text).ok()?;
    Some(LastDryrun {
        attempts: f.kernel_search["attempts"].as_u64().unwrap_or(0),
        kernel_hits: f.kernel_search["kernel_hits"].as_u64().unwrap_or(0),
        coins_considered: f.kernel_search["coins_considered"].as_u64().unwrap_or(0),
        coins_meeting_min_age: f.kernel_search["coins_meeting_min_age"]
            .as_u64()
            .unwrap_or(0),
        reward_sats: f.coinstake["reward_sats"].as_i64().unwrap_or(0),
        n_time: f.coinstake["n_time"].as_u64().unwrap_or(0) as u32,
        split: f.coinstake["split"].as_bool().unwrap_or(false),
        block_hash: f.block["hash"].as_str().unwrap_or("").to_string(),
        search_time: f.kernel_search["search_time"].as_u64().unwrap_or(0),
    })
}

// ===========================================================================
// QR code + PNG writer (receive page / qrcodedialog replica)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct QrMatrix {
    pub size: usize,
    pub modules: Vec<bool>, // row-major [y * size + x]
}

pub fn qr_matrix(text: &str) -> Result<QrMatrix, String> {
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low)
        .map_err(|e| format!("QR encode: {:?}", e))?;
    let size = code.size() as usize;
    let mut modules = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            modules.push(code.get_module(x as i32, y as i32));
        }
    }
    Ok(QrMatrix { size, modules })
}

/// Legacy payment URI (qrcodedialog.cpp getURI): "Triangles:<addr>" with
/// optional ?amount=&label=&message= params (skipped when empty).
pub fn build_payment_uri(
    address: &str,
    amount_tri: Option<&str>,
    label: &str,
    message: &str,
) -> Result<String, String> {
    validate_address(address)?;
    let mut uri = format!("{}{}", URI_SCHEME, address.trim());
    let mut params: Vec<String> = Vec::new();
    if let Some(a) = amount_tri {
        if !a.trim().is_empty() {
            let sats = parse_tri(a)?;
            params.push(format!("amount={}", format_tri(sats)));
        }
    }
    if !label.trim().is_empty() {
        params.push(format!("label={}", uri_encode(label.trim())));
    }
    if !message.trim().is_empty() {
        params.push(format!("message={}", uri_encode(message.trim())));
    }
    if !params.is_empty() {
        uri.push('?');
        uri.push_str(&params.join("&"));
    }
    Ok(uri)
}

fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ---- minimal PNG writer (8-bit grayscale, stored-deflate IDAT) ------------

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode a grayscale (0 = white, 255 = black) image as PNG. `pixels` must be
/// `width * height` bytes.
pub fn png_encode_gray(width: usize, height: usize, pixels: &[u8]) -> Result<Vec<u8>, String> {
    if pixels.len() != width * height {
        return Err(format!(
            "png: pixel buffer {} != {}x{}",
            pixels.len(),
            width,
            height
        ));
    }
    let mut raw = Vec::with_capacity((width + 1) * height);
    for y in 0..height {
        raw.push(0u8); // filter: none
        raw.extend_from_slice(&pixels[y * width..(y + 1) * width]);
    }
    // zlib wrapper with stored (uncompressed) deflate blocks
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.push(0x78);
    z.push(0x01);
    let (mut i, mut last) = (0usize, false);
    while !last {
        let chunk_end = (i + 65535).min(raw.len());
        last = chunk_end == raw.len();
        let n = chunk_end - i;
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..chunk_end]);
        i = chunk_end;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit, grayscale, no filter/interlace
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &z);
    png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Render a QR matrix to grayscale pixels for PNG export (legacy qrcodedialog
/// look: white background, black modules, 4-module quiet zone).
pub fn qr_pixels(qr: &QrMatrix, quiet: usize) -> (usize, usize, Vec<u8>) {
    let dim = qr.size + quiet * 2;
    let mut px = vec![0xFFu8; dim * dim];
    for y in 0..qr.size {
        for x in 0..qr.size {
            if qr.modules[y * qr.size + x] {
                px[(y + quiet) * dim + (x + quiet)] = 0x00;
            }
        }
    }
    (dim, dim, px)
}

pub fn save_qr_png(qr: &QrMatrix, path: &Path) -> Result<usize, String> {
    let (w, h, px) = qr_pixels(qr, 4);
    let png = png_encode_gray(w, h, &px)?;
    let tmp = path.with_extension("png.tmp");
    std::fs::write(&tmp, &png).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path.display(), e))?;
    Ok(png.len())
}

// ===========================================================================
// Message signing (signmessagepage/verifymessagepage parity)
// ===========================================================================

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

/// The legacy hash of a signed message: CDataStream(strMessageMagic) <<
/// std::string(msg) → compact-size-prefixed string, Hash() = dsha256.
/// (main.cpp strMessageMagic; wallet.cpp signmessage RPC.)
pub fn message_hash(message: &str) -> Hash {
    let mut buf = Vec::with_capacity(MESSAGE_MAGIC.len() + message.len() + 9);
    buf.extend_from_slice(MESSAGE_MAGIC.as_bytes());
    push_compact_size(&mut buf, message.len() as u64);
    buf.extend_from_slice(message.as_bytes());
    crate::legacy::sha256d(&buf)
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut vals = Vec::with_capacity(s.len());
    for c in s.trim().bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return Err(format!("invalid base64 byte {:#04x}", c)),
        };
        vals.push(v);
    }
    let mut out = Vec::with_capacity(vals.len() * 3 / 4);
    for chunk in vals.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let n = chunk.iter().fold(0u32, |acc, &v| acc << 6 | v as u32)
            << (6 * (4 - chunk.len()) as u32);
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Sign a message with a wallet key → base64 of the 65-byte compact
/// recoverable signature (legacy CKey::SignCompact, compressed pubkey form:
/// header 31 + recid).
pub fn sign_message(secret: &[u8; 32], message: &str) -> Result<String, String> {
    let msg = message_hash(message);
    let secp = secp256k1::Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(secret).map_err(|e| format!("bad secret: {}", e))?;
    let m = secp256k1::Message::from_digest(msg);
    let sig = secp.sign_ecdsa_recoverable(&m, &sk);
    let (recid, compact) = sig.serialize_compact();
    let mut sig65 = Vec::with_capacity(65);
    sig65.push(31 + recid.to_i32() as u8);
    sig65.extend_from_slice(&compact);
    Ok(base64_encode(&sig65))
}

/// Verify a signed message: recover the pubkey from the compact signature and
/// compare its hash160-address against `address` (legacy VerifyCompact).
pub fn verify_message(address: &str, message: &str, sig_b64: &str) -> Result<bool, String> {
    let payload = validate_address(address)?;
    let raw = base64_decode(sig_b64)?;
    if raw.len() != 65 {
        return Err(format!("signature length {} != 65", raw.len()));
    }
    let header = raw[0];
    let (recid, compressed) = match header {
        27..=30 => (header - 27, false),
        31..=34 => (header - 31, true),
        _ => return Err(format!("invalid signature header {}", header)),
    };
    let recid = secp256k1::ecdsa::RecoveryId::from_i32(recid as i32)
        .map_err(|_| "invalid recovery id".to_string())?;
    let sig = secp256k1::ecdsa::RecoverableSignature::from_compact(&raw[1..], recid)
        .map_err(|e| format!("bad compact signature: {}", e))?;
    let msg = message_hash(message);
    let secp = secp256k1::Secp256k1::verification_only();
    let m = secp256k1::Message::from_digest(msg);
    let pk = secp
        .recover_ecdsa(&m, &sig)
        .map_err(|e| format!("recover: {}", e))?;
    let pk_bytes = if compressed {
        pk.serialize().to_vec()
    } else {
        pk.serialize_uncompressed().to_vec()
    };
    let expect = crate::address::pubkey_payload(&pk_bytes);
    Ok(expect == payload)
}

// ===========================================================================
// Address book + GUI keystore side files (wallet.dat stays read-only)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct AbEntry {
    pub address: String,
    pub label: String,
    /// secret only for GUI-generated keys (keystore.json) — wallet.dat keys
    /// keep their secret inside the wallet.
    pub secret: Option<[u8; 32]>,
    pub source: &'static str, // "wallet" | "keystore" | "addressbook"
}

#[derive(Deserialize, Serialize, Default)]
struct SideFile {
    #[serde(default)]
    entries: Vec<SideEntry>,
}

#[derive(Clone, Deserialize, Serialize)]
struct SideEntry {
    address: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    secret_hex: Option<String>,
    #[serde(default)]
    created: Option<u64>,
}

fn load_side_file(path: &Path) -> Vec<SideEntry> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str::<SideFile>(&t)
            .map(|f| f.entries)
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_side_file(path: &Path, entries: &[SideEntry]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let f = SideFile {
        entries: entries.to_vec(),
    };
    let text = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {}", path.display(), e))
}

/// Fresh CSPRNG 32-byte secret for a new receive key: the OS CSPRNG via the
/// getrandom crate (/dev/urandom on unix, BCryptGenRandom on windows) — 32
/// bytes → SecretKey, retried on the ~2^-128 invalid scalar chance.
pub fn generate_secret() -> Result<[u8; 32], String> {
    loop {
        let mut buf = [0u8; 32];
        getrandom::getrandom(&mut buf).map_err(|e| format!("csprng: {}", e))?;
        match secp256k1::SecretKey::from_slice(&buf) {
            Ok(k) => return Ok(k.secret_bytes()),
            Err(_) => continue, // astronomically rare; retry with fresh bytes
        }
    }
}

/// Generate a fresh offline key for receiving — the KEYSTORE fallback shape
/// (used when wallet.dat itself is not writable). Primary path writes the key
/// INTO wallet.dat via `WalletHandle::add_key`. Entropy is the OS CSPRNG
/// ([`generate_secret`]).
pub fn generate_new_address() -> Result<AbEntry, String> {
    let secret = generate_secret()?;
    let secp = secp256k1::Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(&secret).map_err(|e| format!("bad secret: {}", e))?;
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    let pubkey = pk.serialize().to_vec();
    Ok(AbEntry {
        address: crate::wallet::pubkey_to_address(&pubkey),
        label: String::new(),
        secret: Some(secret),
        source: "keystore",
    })
}
