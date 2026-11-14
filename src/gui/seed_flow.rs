//! Seed-phrase (BIP39) create/restore flows for the GUI — a headless,
//! display-independent state machine (the app.rs view layer renders it).
//!
//! Scheme provenance: this drives `crate::wallet::{create_wallet_from_seed,
//! restore_wallet_from_seed}` which write the EXACT legacy record set
//! (`hdmnemonic`/`hdpassphrase`/`hdchain` + grade-A `key`/`keymeta`/`name`
//! rows), so mnemonics are interchangeable with the original qt wallet.
//!
//! Honest-capability note (shown in every step): wallets whose keys were
//! created RANDOMLY (all pre-HD wallets) have no seed phrase and CANNOT
//! gain one retroactively — the keys are not derived from any phrase, so no
//! phrase can reproduce them. Seed phrases create NEW wallets or restore
//! existing phrases.
//!
//! Mnemonic hygiene: the phrase lives in [`SecretString`] (zeroized on
//! drop) and is scrubbed the moment the wallet write succeeds or the flow
//! is abandoned.

use crate::seed::SecretString;

/// How many keys a created/restored wallet is seeded with (indices
/// 0..count-1 written as legacy key rows; the qt continues from `hdchain`).
pub const DEFAULT_KEY_COUNT: usize = 100;
pub const MAX_KEY_COUNT: usize = 10_000;

pub const HONEST_NOTE: &str = "Seed phrases cannot be added to an existing wallet: every pre-HD \
     wallet's keys were created randomly and no phrase can ever reproduce them. A seed phrase \
     creates a NEW wallet or restores an existing phrase — and the 24 words then become the \
     ONLY backup of every address that wallet will ever derive.";

#[derive(Debug, Clone, PartialEq)]
pub enum SeedFlowStep {
    /// Choose create vs restore.
    Menu,
    /// 24 words shown ONCE (copy/save), explicit "I saved it" gate.
    CreateReveal,
    /// Confirm by re-entering the words.
    CreateConfirm,
    /// Type/paste a phrase (+ optional passphrase) to restore.
    RestoreInput,
    /// Mnemonic validated; wallet path + key count chosen; write pending.
    Ready,
    /// Written and verified. Carries the derived addresses.
    Done(Vec<String>),
}

pub struct SeedFlow {
    pub step: SeedFlowStep,
    /// The generated phrase (create flow) — zeroized on drop.
    pub mnemonic: Option<SecretString>,
    /// BIP39 passphrase ("25th word"); empty = none.
    pub passphrase: String,
    /// Text buffer for confirm/restore input.
    pub words_input: String,
    /// CreateReveal gate: the user asserted the words were saved.
    pub saved_confirmed: bool,
    /// Target wallet file.
    pub wallet_path: String,
    /// How many keys to derive and write.
    pub key_count: usize,
    pub error: Option<String>,
    /// UX audit (H2): true when create was auto-begun on first open — the
    /// reveal screen then tells a restoring user how to get to Restore.
    pub auto_began: bool,
}

impl SeedFlow {
    pub fn new(default_wallet_path: String) -> Self {
        SeedFlow {
            step: SeedFlowStep::Menu,
            mnemonic: None,
            passphrase: String::new(),
            words_input: String::new(),
            saved_confirmed: false,
            wallet_path: default_wallet_path,
            key_count: DEFAULT_KEY_COUNT,
            error: None,
            auto_began: false,
        }
    }

    /// BEGIN CREATE: generate a fresh 24-word phrase (OS CSPRNG) and move to
    /// the one-time reveal. Failures (CSPRNG) surface as step errors.
    pub fn begin_create(&mut self) {
        match crate::seed::generate_mnemonic_24() {
            Ok(m) => {
                self.mnemonic = Some(SecretString(m));
                self.words_input.clear();
                self.saved_confirmed = false;
                self.error = None;
                self.step = SeedFlowStep::CreateReveal;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// BEGIN RESTORE: paste-a-phrase step.
    pub fn begin_restore(&mut self) {
        self.words_input.clear();
        self.passphrase.clear();
        self.error = None;
        self.step = SeedFlowStep::RestoreInput;
    }

    /// CreateReveal → CreateConfirm (gated on the "I saved it" checkbox).
    pub fn confirm_saved(&mut self) {
        if !self.saved_confirmed {
            self.error =
                Some("tick the box to confirm you wrote the words down before continuing".into());
            return;
        }
        self.error = None;
        self.step = SeedFlowStep::CreateConfirm;
    }

    /// CreateConfirm: re-entered words must match the generated phrase
    /// exactly (whitespace-insensitive, like the restore path). Only a full
    /// match advances to Ready.
    pub fn submit_create_confirm(&mut self) {
        let Some(m) = self.mnemonic.as_ref() else {
            self.error = Some("no generated phrase in flight".into());
            return;
        };
        let want = crate::seed::split_words(m);
        let got = crate::seed::split_words(&self.words_input);
        if want != got {
            self.error = Some(format!(
                "the words you typed do not match ({} of {} entered) — a mismatched \
                 confirmation means the backup is wrong",
                got.len(),
                want.len()
            ));
            return;
        }
        self.error = None;
        self.step = SeedFlowStep::Ready;
    }

    /// RestoreInput: validate the typed phrase (checksum + wordlist) and
    /// move to Ready. Whitespace-runs in the input are canonicalized (the
    /// BIP39-standard restore; see seed.rs for the legacy raw-string note).
    pub fn submit_restore(&mut self) {
        match crate::seed::normalize_words(&self.words_input) {
            Some(canonical) if crate::seed::check_mnemonic(&canonical) => {
                self.mnemonic = Some(SecretString(canonical));
                self.error = None;
                self.step = SeedFlowStep::Ready;
            }
            _ => {
                self.error = Some(
                    "invalid mnemonic: unknown word or bad checksum (12/15/18/21/24 words \
                     from the BIP39 English wordlist)"
                        .into(),
                );
            }
        }
    }

    /// READY → DONE: write the wallet (exact legacy records) and scrub the
    /// in-memory phrase. Returns the derived addresses for the summary.
    pub fn execute(&mut self) -> Result<Vec<String>, String> {
        if self.step != SeedFlowStep::Ready {
            return Err("flow is not in the ready state".into());
        }
        if self.wallet_path.trim().is_empty() {
            return Err("choose a wallet file path first".into());
        }
        if self.key_count == 0 || self.key_count > MAX_KEY_COUNT {
            return Err(format!(
                "key count out of range 1..={}: {}",
                MAX_KEY_COUNT, self.key_count
            ));
        }
        let mnemonic = self
            .mnemonic
            .as_ref()
            .map(|s| s.0.clone())
            .ok_or_else(|| "no validated phrase in flight".to_string())?;
        let path = std::path::PathBuf::from(self.wallet_path.trim());
        let out = crate::wallet::restore_wallet_from_seed(
            &path,
            &mnemonic,
            &self.passphrase,
            self.key_count,
        )?;
        let addresses: Vec<String> = out.keys.iter().map(|k| k.address.clone()).collect();
        // scrub the phrase from memory — it now lives ONLY in wallet.dat's
        // hdmnemonic row (the legacy design) and the user's backup
        self.mnemonic = None;
        self.words_input.clear();
        self.passphrase.clear();
        self.step = SeedFlowStep::Done(addresses.clone());
        Ok(addresses)
    }

    /// Abandon the flow entirely (scrubs the phrase).
    pub fn abandon(&mut self) {
        self.mnemonic = None;
        self.words_input.clear();
        self.passphrase.clear();
        self.error = None;
        self.step = SeedFlowStep::Menu;
    }

    /// The reveal text (create flow only; consumed by the view layer).
    pub fn revealed_words(&self) -> Option<String> {
        self.mnemonic.as_deref().map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> SeedFlow {
        SeedFlow::new("/tmp/tri7_seed_flow_test/wallet.dat".into())
    }

    const MN12: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MN24: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    #[test]
    fn create_flow_full_state_machine() {
        let dir = std::env::temp_dir().join(format!("tri7seedflow_create_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = SeedFlow::new(dir.join("wallet.dat").display().to_string());
        assert_eq!(f.step, SeedFlowStep::Menu);

        f.begin_create();
        assert_eq!(f.step, SeedFlowStep::CreateReveal);
        let words = f.revealed_words().unwrap();
        assert_eq!(crate::seed::split_words(&words).len(), 24);
        assert!(crate::seed::check_mnemonic(&words));

        // the reveal gate refuses until the box is ticked
        f.confirm_saved();
        assert_eq!(f.step, SeedFlowStep::CreateReveal);
        assert!(f.error.is_some());
        f.saved_confirmed = true;
        f.confirm_saved();
        assert_eq!(f.step, SeedFlowStep::CreateConfirm);

        // a wrong confirmation stays on CreateConfirm
        f.words_input = "abandon ".repeat(24);
        f.submit_create_confirm();
        assert_eq!(f.step, SeedFlowStep::CreateConfirm);
        assert!(f.error.as_deref().unwrap().contains("do not match"));

        // the exact words advance to Ready and execute writes the wallet
        f.words_input = words.clone();
        f.submit_create_confirm();
        assert_eq!(f.step, SeedFlowStep::Ready);
        let addresses = f.execute().unwrap();
        assert_eq!(addresses.len(), DEFAULT_KEY_COUNT);
        assert_eq!(f.step, SeedFlowStep::Done(addresses.clone()));
        // phrase scrubbed after the write
        assert!(f.mnemonic.is_none());
        assert!(f.revealed_words().is_none());
        // every address re-derives from the phrase (which wallet.dat holds)
        let w = crate::wallet::read_wallet(std::path::Path::new(
            &dir.join("wallet.dat").display().to_string(),
        ))
        .unwrap();
        assert_eq!(w.keys.len(), DEFAULT_KEY_COUNT);
        assert_eq!(w.keys.iter().map(|k| k.address.clone()).collect::<Vec<_>>(), addresses);
        assert!(w.hd_mnemonic.is_some());
        assert_eq!(w.hd_chain_index, Some(DEFAULT_KEY_COUNT as i64));
    }

    #[test]
    fn restore_flow_whitespace_insensitive_with_passphrase() {
        let dir = std::env::temp_dir().join(format!("tri7seedflow_restore_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = SeedFlow::new(dir.join("wallet.dat").display().to_string());
        f.begin_restore();
        assert_eq!(f.step, SeedFlowStep::RestoreInput);

        // junk is refused and stays on RestoreInput
        f.words_input = "not a real phrase at all".into();
        f.submit_restore();
        assert_eq!(f.step, SeedFlowStep::RestoreInput);
        assert!(f.error.as_deref().unwrap().contains("invalid mnemonic"));

        // a checksum-corrupted phrase is refused
        f.words_input = MN12.replace("about", "actor");
        f.submit_restore();
        assert_eq!(f.step, SeedFlowStep::RestoreInput);

        // newline/tab-mangled paste of a valid phrase advances; the
        // canonical form derives the canonical keys
        f.words_input = format!("\t{}\n", MN12);
        f.passphrase = "25th word".into();
        f.submit_restore();
        assert_eq!(f.step, SeedFlowStep::Ready);
        f.key_count = 3;
        let addresses = f.execute().unwrap();
        assert_eq!(addresses.len(), 3);
        for (i, a) in addresses.iter().enumerate() {
            let sec = crate::seed::derive_triangles(MN12, "25th word", 0, 0, i as u32).unwrap();
            let secp = secp256k1::Secp256k1::signing_only();
            let sk = secp256k1::SecretKey::from_slice(&sec).unwrap();
            let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
            assert_eq!(crate::wallet::pubkey_to_address(&pk.serialize()), *a);
        }
    }

    #[test]
    fn restore_refuses_to_overwrite_a_seeded_wallet() {
        let dir = std::env::temp_dir().join(format!("tri7seedflow_dup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.dat");
        crate::wallet::restore_wallet_from_seed(&path, MN12, "", 1).unwrap();

        let mut f = SeedFlow::new(path.display().to_string());
        f.begin_restore();
        f.words_input = MN24.into();
        f.submit_restore();
        assert_eq!(f.step, SeedFlowStep::Ready);
        f.key_count = 1;
        let err = f.execute().unwrap_err();
        assert!(err.contains("already has an HD seed"), "{}", err);
        // flow stays ready (caller can fix the path); the file is untouched
        assert_eq!(f.step, SeedFlowStep::Ready);
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.hd_mnemonic.as_deref(), Some(MN12));
    }

    #[test]
    fn abandon_scrubs_everything() {
        let mut f = flow();
        f.begin_create();
        assert!(f.revealed_words().is_some());
        f.words_input = "leftover".into();
        f.passphrase = "sekrit".into();
        f.abandon();
        assert_eq!(f.step, SeedFlowStep::Menu);
        assert!(f.mnemonic.is_none());
        assert!(f.words_input.is_empty());
        assert!(f.passphrase.is_empty());
    }

    #[test]
    fn execute_rejects_bad_targets_and_counts() {
        let mut f = flow();
        f.begin_restore();
        f.words_input = MN12.into();
        f.submit_restore();
        // empty path
        f.wallet_path = "  ".into();
        assert!(f.execute().unwrap_err().contains("wallet file path"));
        f.wallet_path = "/proc/definitely/impossible/wallet.dat".into();
        assert!(f.execute().is_err());
        f.wallet_path = "/tmp/tri7seedflow_badcount/wallet.dat".into();
        f.key_count = 0;
        assert!(f.execute().unwrap_err().contains("out of range"));
        f.key_count = MAX_KEY_COUNT + 1;
        assert!(f.execute().unwrap_err().contains("out of range"));
    }
}
