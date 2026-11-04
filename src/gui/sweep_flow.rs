//! Seed migration ("sweep-to-seed") wizard — a headless, display-independent
//! state machine (the app.rs view layer renders it), same pattern as
//! seed_flow.rs.
//!
//! What the wizard does, in order:
//!   1. Menu → the user generates a NEW 24-word phrase (or restores an
//!      existing one). When the wallet ALREADY has a stored seed (a
//!      mid-migration retry, or a phrase previously restored onto this
//!      wallet), the flow LOCKS to the stored phrase — a wallet can never
//!      hold two seeds (wallet.rs set_hd_seed refuses too).
//!   2. prepare(): THE SAFETY LADDER, run before any signing:
//!        a. double-sweep guard — refuse if the `seedmigrated` marker is set;
//!        b. roundtrip proof — the same mnemonic is restored on a FRESH
//!           wallet file and the resulting addresses must be IDENTICAL to
//!           the direct derivation (a mnemonic that cannot be proven to
//!           reproduce its addresses never gets near the money);
//!        c. the derived keys are inserted into the SAME wallet.dat via
//!           wallet.rs add_key (idempotent: rows already present from an
//!           earlier attempt are skipped), hdchain jumps past the inserted
//!           indices, defaultkey becomes the first seed address;
//!        d. the sweep plan is built + SIGNED in memory (deterministic
//!           RFC6979 signatures, byte-identical every rebuild) — nothing is
//!           broadcast yet.
//!   3. PlanReview → the operator sees the EXACT amounts (every input, every
//!      output, the fee, the txid) — the same sign→review→broadcast three
//!      step the Send screen uses. One explicit click broadcasts.
//!   4. Broadcast via tri7d sendrawtransaction. ONLY after a successful
//!      broadcast is the `seedmigrated` marker written. On broadcast failure
//!      NOTHING is lost: the keys are already in wallet.dat and the coins
//!      remain on the old keys — the flow stays on PlanReview and can retry.
//!
//! Failure semantics (money path): every refusal happens BEFORE value moves;
//! the two irreversible steps (broadcast, marker) are strictly ordered so no
//! failure state can strand the wallet without its coins.

use crate::seed::SecretString;
use crate::sweep::{self, SweepDest};

/// One swept input line for the exact-amounts review table (display only —
/// selection and signing never depend on it).
#[derive(Debug, Clone)]
pub struct PlanInputLine {
    pub txid_display: String,
    pub n: u32,
    pub value_sats: i64,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SweepStep {
    /// Choose create vs restore (only shown when the wallet has no stored
    /// seed).
    Menu,
    /// 24 words shown ONCE (copy/save), explicit "I saved it" gate.
    CreateReveal,
    /// Confirm by re-entering the words.
    CreateConfirm,
    /// Type/paste a phrase (+ optional passphrase) to restore.
    RestoreInput,
    /// Mnemonic validated (or locked to the stored row); prepare() pending.
    SeedVerified,
    /// Signed sweep plan ready; exact-amounts review; awaiting broadcast.
    PlanReview,
    /// Broadcast confirmed by tri7d; marker written. Carries the txid.
    Done { txid: String },
}

/// The exact-amounts review payload (everything the pre-flight screen and
/// the signed-tx review show).
#[derive(Debug, Clone)]
pub struct SweepPlanView {
    /// One line per swept UTXO (txid display, output index, value, address).
    pub inputs: Vec<PlanInputLine>,
    pub inputs_total: i64,
    pub fee: i64,
    /// (address, value) in output order — where every sat lands.
    pub outputs: Vec<(String, i64)>,
    pub txid_display: String,
    pub tx_hex: String,
    pub n_bytes: usize,
    /// Derived keys added to wallet.dat by THIS attempt.
    pub keys_inserted: usize,
    /// Derived keys already present (earlier attempt / restore-onto wallet).
    pub keys_already_present: usize,
}

pub struct SweepFlow {
    pub step: SweepStep,
    /// The validated phrase (create flow) or the restored/stored phrase —
    /// zeroized on drop. The stored-row phrase is used BYTE-EXACT (wallet.rs
    /// derive_hd_key convention).
    pub mnemonic: Option<SecretString>,
    /// BIP39 passphrase ("25th word"); empty = none.
    pub passphrase: String,
    /// Text buffer for confirm/restore input.
    pub words_input: String,
    /// CreateReveal gate: the user asserted the words were saved.
    pub saved_confirmed: bool,
    /// The phrase came from the wallet's stored hdmnemonic row — the
    /// create/restore steps are hidden and CANNOT change it.
    pub locked_seed: bool,
    /// The sweep destinations, filled by prepare() (derivation order 0..9).
    pub dests: Vec<SweepDest>,
    /// Destination addresses for display (same order as dests).
    pub addresses: Vec<String>,
    /// The signed plan (also the broadcast payload), set by prepare().
    pub plan: Option<SweepPlanView>,
    pub error: Option<String>,
}

impl SweepFlow {
    pub fn new() -> Self {
        SweepFlow {
            step: SweepStep::Menu,
            mnemonic: None,
            passphrase: String::new(),
            words_input: String::new(),
            saved_confirmed: false,
            locked_seed: false,
            dests: Vec::new(),
            addresses: Vec::new(),
            plan: None,
            error: None,
        }
    }

    /// BEGIN CREATE: generate a fresh 24-word phrase (OS CSPRNG).
    pub fn begin_create(&mut self) {
        match crate::seed::generate_mnemonic_24() {
            Ok(m) => {
                self.mnemonic = Some(SecretString(m));
                self.words_input.clear();
                self.saved_confirmed = false;
                self.error = None;
                self.step = SweepStep::CreateReveal;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// BEGIN RESTORE: paste-a-phrase step.
    pub fn begin_restore(&mut self) {
        self.words_input.clear();
        self.passphrase.clear();
        self.error = None;
        self.step = SweepStep::RestoreInput;
    }

    /// LOCK to the wallet's STORED phrase (retry / restore-onto path). The
    /// stored row is used byte-exactly; deriving from it here proves it is
    /// usable before any step is shown.
    pub fn lock_to_stored_seed(
        &mut self,
        stored_mnemonic: &str,
        stored_passphrase: Option<&str>,
    ) -> Result<(), String> {
        if self.step != SweepStep::Menu {
            return Err("flow already past the menu".into());
        }
        // a corrupt/malformed stored row must refuse HERE with a clear
        // message, never mid-migration (deriving succeeds on any string —
        // the checksum is what proves the phrase is a real mnemonic)
        if !crate::seed::check_mnemonic(stored_mnemonic) {
            return Err(
                "the wallet's stored seed phrase is not a valid BIP39 mnemonic (unknown \
                 word or bad checksum) — refusing to migrate"
                    .into(),
            );
        }
        // and it must derive before any step is shown
        let _ = crate::seed::derive_triangles(stored_mnemonic, stored_passphrase.unwrap_or(""), 0, 0, 0)?;
        self.mnemonic = Some(SecretString(stored_mnemonic.to_string()));
        self.passphrase = stored_passphrase.unwrap_or("").to_string();
        self.locked_seed = true;
        self.error = None;
        self.step = SweepStep::SeedVerified;
        Ok(())
    }

    /// CreateReveal → CreateConfirm (gated on the "I saved it" checkbox).
    pub fn confirm_saved(&mut self) {
        if !self.saved_confirmed {
            self.error =
                Some("tick the box to confirm you wrote the words down before continuing".into());
            return;
        }
        self.error = None;
        self.step = SweepStep::CreateConfirm;
    }

    /// CreateConfirm: re-entered words must match the generated phrase
    /// exactly (whitespace-insensitive).
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
        self.step = SweepStep::SeedVerified;
    }

    /// RestoreInput: validate the typed phrase and move to SeedVerified.
    /// Whitespace runs are canonicalized (the BIP39-standard restore).
    pub fn submit_restore(&mut self) {
        match crate::seed::normalize_words(&self.words_input) {
            Some(canonical) if crate::seed::check_mnemonic(&canonical) => {
                self.mnemonic = Some(SecretString(canonical));
                self.error = None;
                self.step = SweepStep::SeedVerified;
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

    /// THE PREPARE STEP — roundtrip proof, wallet key insertion, then the
    /// signed sweep plan. Nothing is broadcast here; a failure anywhere
    /// leaves the wallet's value untouched (writes before signing are key
    /// rows only, which are safe by themselves).
    pub fn prepare(
        &mut self,
        wallet: &mut crate::wallet::WalletHandle,
        inputs: &[crate::spend::SpendInput],
        tip_height: u32,
        now: u32,
    ) -> Result<(), String> {
        if self.step != SweepStep::SeedVerified {
            return Err("flow is not in the seed-verified state".into());
        }
        if wallet.seed_migrated {
            return Err(
                "this wallet already completed seed migration — the 'seedmigrated' marker \
                 row refuses a second sweep"
                    .into(),
            );
        }
        let mnemonic = self
            .mnemonic
            .as_ref()
            .map(|s| s.0.clone())
            .ok_or_else(|| "no validated phrase in flight".to_string())?;
        if inputs.is_empty() {
            return Err(
                "no spendable UTXOs found for this wallet — nothing to migrate (zero \
                 balance, immature coinbase/coinstake, or an empty/missing UTXO snapshot)"
                    .into(),
            );
        }

        // (1) destinations, straight from the phrase
        let dests = sweep::derive_sweep_destinations(&mnemonic, &self.passphrase)?;

        // (2) ROUNDTRIP PROOF before any signing: restore the same mnemonic
        // on a fresh wallet file and require identical addresses. A phrase
        // that cannot reproduce its addresses is refused — this is the check
        // that makes "your seed is your backup" true BEFORE value moves.
        let roundtrip = roundtrip_verify(&mnemonic, &self.passphrase)?;
        let want: Vec<String> = dests.iter().map(|d| d.address.clone()).collect();
        if roundtrip != want {
            return Err(format!(
                "address roundtrip FAILED: restoring the phrase on a fresh wallet derived \
                 {} addresses, expected {} — migration refused (the phrase would not \
                 reproduce its own addresses)",
                roundtrip.len(),
                want.len()
            ));
        }

        // (3) insert the derived keys into the SAME wallet.dat (the qt sees
        // them). Idempotent: keys already present from an earlier attempt are
        // counted, never re-written (add_key refuses duplicates by design).
        let mut keys_inserted = 0usize;
        let mut keys_already_present = 0usize;
        if wallet.hd_mnemonic.is_none() {
            wallet.set_hd_seed(&mnemonic, &self.passphrase)?;
        }
        let existing: std::collections::HashSet<String> =
            crate::wallet::read_wallet(&wallet.path)?.keys.iter().map(|k| k.address.clone()).collect();
        for (i, d) in dests.iter().enumerate() {
            if existing.contains(&d.address) {
                keys_already_present += 1;
                continue;
            }
            let secret = crate::seed::derive_triangles(&mnemonic, &self.passphrase, 0, 0, i as u32)?;
            wallet.add_key(
                &secret,
                &format!("seed migration m/44'/2222'/0'/0/{}", i),
            )?;
            keys_inserted += 1;
        }
        // the counter must jump past the inserted indices or the next
        // GenerateNewKey (qt or tri7) re-derives an existing row
        wallet.advance_hdchain_to(sweep::SWEEP_ADDR_COUNT as i64)?;
        wallet.write_defaultkey(&dests[0].pubkey)?;

        // (4) the ONE consolidation transaction, signed in memory. The source
        // set excludes the destination addresses (coins already on seed
        // addresses are left alone); the destination keys inserted above are
        // NOT needed for signing — the sources sign with the wallet's
        // existing keys.
        let sources = sweep::sweep_source_inputs(inputs, &dests);
        if sources.is_empty() {
            return Err(
                "every spendable UTXO already sits on the seed-derived addresses — nothing \
                 to migrate"
                    .into(),
            );
        }
        let plan = sweep::plan_sweep(&sources, &dests, 0, now, tip_height + 1)?;

        self.dests = dests;
        self.addresses = self.dests.iter().map(|d| d.address.clone()).collect();
        self.plan = Some(SweepPlanView {
            inputs: sources
                .iter()
                .map(|i| PlanInputLine {
                    txid_display: crate::legacy::hash_to_display(&i.txid),
                    n: i.n,
                    value_sats: i.value,
                    address: display_address_of(&i.script_pubkey),
                })
                .collect(),
            inputs_total: plan.inputs_total,
            fee: plan.fee,
            outputs: plan.outputs.clone(),
            txid_display: crate::legacy::hash_to_display(&plan.tx.hash()),
            tx_hex: plan
                .tx
                .serialize()
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect(),
            n_bytes: plan.n_bytes,
            keys_inserted,
            keys_already_present,
        });
        self.error = None;
        self.step = SweepStep::PlanReview;
        Ok(())
    }

    /// Record a successful broadcast (AppState performs the RPC + marker
    /// write and calls this).
    pub fn complete(&mut self, txid: String) {
        self.words_input.clear();
        self.passphrase.clear();
        // the phrase stays OUT of memory after completion — it lives in
        // wallet.dat's hdmnemonic row (the legacy design) and the user's
        // backup
        self.mnemonic = None;
        self.step = SweepStep::Done { txid };
    }

    /// Abandon the flow entirely (scrubs the phrase; nothing was broadcast).
    pub fn abandon(&mut self) {
        self.mnemonic = None;
        self.words_input.clear();
        self.passphrase.clear();
        self.error = None;
        self.plan = None;
        self.dests.clear();
        self.addresses.clear();
        self.step = SweepStep::Menu;
    }

    /// The reveal text (create flow only; consumed by the view layer).
    pub fn revealed_words(&self) -> Option<String> {
        self.mnemonic.as_deref().map(|s| s.to_string())
    }
}

impl Default for SweepFlow {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort source address for the review table (display only — the
/// signing and selection never depend on it).
fn display_address_of(script_pubkey: &[u8]) -> String {
    match crate::address::script_to_dest(script_pubkey) {
        Some(dest) => crate::address::payload_to_address(&dest.payload()),
        None => String::new(),
    }
}

/// Restore the SAME mnemonic on a FRESH wallet file and return the derived
/// addresses — the roundtrip proof required before any signing. The temp
/// wallet is removed afterwards.
fn roundtrip_verify(mnemonic: &str, passphrase: &str) -> Result<Vec<String>, String> {
    let dir = std::env::temp_dir().join(format!(
        "tri7_sweep_roundtrip_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("roundtrip temp dir: {}", e))?;
    let path = dir.join("wallet.dat");
    let result = (|| -> Result<Vec<String>, String> {
        // restore_wallet_from_seed refuses an EXISTING file — a fresh path is
        // guaranteed by the nano-timestamped directory above.
        let out = crate::wallet::restore_wallet_from_seed(
            &path,
            mnemonic,
            passphrase,
            sweep::SWEEP_ADDR_COUNT,
        )?;
        Ok(out.keys.iter().map(|k| k.address.clone()).collect())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::SpendInput;

    const MN12: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MN24: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    fn flow() -> SweepFlow {
        SweepFlow::new()
    }

    fn test_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tri7_sweepflow_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A wallet file with `n` synthetic RANDOM keys (the pre-migration
    /// shape) and no seed.
    fn random_key_wallet(dir: &std::path::Path, tag: &str, n: usize) -> std::path::PathBuf {
        let path = dir.join(format!("wallet_{}.dat", tag));
        crate::wallet::create_wallet_file(&path).unwrap();
        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        h.write_fresh_identity_rows().unwrap();
        for i in 0..n {
            let secret = crate::legacy::sha256d(format!("{}/key{}", tag, i).as_bytes());
            h.add_key(&secret, "").unwrap();
        }
        h.close();
        path
    }

    fn p2pkh_input(wallet_path: &std::path::Path, key_idx: usize, value: i64, height: u32) -> SpendInput {
        let w = crate::wallet::read_wallet(wallet_path).unwrap();
        let k = &w.keys[key_idx];
        SpendInput {
            txid: crate::legacy::sha256d(format!("utxo{}:{}", key_idx, value).as_bytes()),
            n: 0,
            value,
            script_pubkey: crate::spend::script_pubkey_p2pkh(&k.pubkey),
            height,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: k.secret,
            pubkey: k.pubkey.clone(),
        }
    }

    #[test]
    fn wizard_create_state_machine_full() {
        let mut f = flow();
        assert_eq!(f.step, SweepStep::Menu);
        f.begin_create();
        assert_eq!(f.step, SweepStep::CreateReveal);
        let words = f.revealed_words().unwrap();
        assert_eq!(crate::seed::split_words(&words).len(), 24);
        assert!(crate::seed::check_mnemonic(&words));

        // the reveal gate refuses until the box is ticked
        f.confirm_saved();
        assert_eq!(f.step, SweepStep::CreateReveal);
        assert!(f.error.is_some());
        f.saved_confirmed = true;
        f.confirm_saved();
        assert_eq!(f.step, SweepStep::CreateConfirm);

        // a wrong confirmation stays on CreateConfirm
        f.words_input = "abandon ".repeat(24);
        f.submit_create_confirm();
        assert_eq!(f.step, SweepStep::CreateConfirm);
        assert!(f.error.as_deref().unwrap().contains("do not match"));

        f.words_input = words.clone();
        f.submit_create_confirm();
        assert_eq!(f.step, SweepStep::SeedVerified);
        // phrase still held for prepare (scrubbed only on complete/abandon)
        assert!(f.revealed_words().is_some());
    }

    #[test]
    fn wizard_restore_whitespace_and_checksum_gates() {
        let mut f = flow();
        f.begin_restore();
        assert_eq!(f.step, SweepStep::RestoreInput);
        // junk is refused and stays on RestoreInput
        f.words_input = "not a real phrase at all".into();
        f.submit_restore();
        assert_eq!(f.step, SweepStep::RestoreInput);
        assert!(f.error.as_deref().unwrap().contains("invalid mnemonic"));
        // checksum corruption refused
        f.words_input = MN12.replace("about", "actor");
        f.submit_restore();
        assert_eq!(f.step, SweepStep::RestoreInput);
        // whitespace-mangled valid phrase advances
        f.words_input = format!("\t{}\n", MN12);
        f.submit_restore();
        assert_eq!(f.step, SweepStep::SeedVerified);
    }

    #[test]
    fn lock_to_stored_seed_refuses_a_corrupt_row_and_jumps_to_verified() {
        let mut f = flow();
        f.lock_to_stored_seed("not real words", None).unwrap_err();
        assert_eq!(f.step, SweepStep::Menu);
        f.lock_to_stored_seed(MN24, Some("25th")).unwrap();
        assert_eq!(f.step, SweepStep::SeedVerified);
        assert!(f.locked_seed);
        assert_eq!(f.passphrase, "25th");
        // locked twice is a programming error, refused
        assert!(f.lock_to_stored_seed(MN12, None).is_err());
    }

    #[test]
    fn prepare_proves_roundtrip_inserts_keys_and_builds_the_plan() {
        let dir = test_dir("prepare");
        let path = random_key_wallet(&dir, "prepare", 2);
        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        assert!(!h.seed_migrated);
        assert!(h.hd_mnemonic.is_none());

        // 4 spendable UTXOs across the two random keys (multi-UTXO source set)
        let inputs = vec![
            p2pkh_input(&path, 0, 3_000_000, 100),
            p2pkh_input(&path, 0, 1_500_000, 101),
            p2pkh_input(&path, 1, 2_000_000, 102),
            p2pkh_input(&path, 1, 404_000, 103),
        ];
        let inputs_total: i64 = inputs.iter().map(|i| i.value).sum();

        let mut f = flow();
        f.begin_create();
        let words = f.revealed_words().unwrap();
        f.saved_confirmed = true;
        f.confirm_saved();
        f.words_input = words.clone();
        f.submit_create_confirm();
        f.prepare(&mut h, &inputs, 150, 1_500_000_000).unwrap();

        // wizard advanced to the review with a complete plan
        assert_eq!(f.step, SweepStep::PlanReview);
        let plan = f.plan.as_ref().unwrap();
        assert_eq!(plan.inputs.len(), 4);
        assert_eq!(plan.inputs_total, inputs_total);
        assert_eq!(plan.outputs.len(), sweep::SWEEP_ADDR_COUNT);
        let out_sum: i64 = plan.outputs.iter().map(|(_, v)| v).sum();
        assert_eq!(out_sum + plan.fee, inputs_total);
        assert_eq!(plan.keys_inserted, 10);
        assert_eq!(plan.keys_already_present, 0);
        assert!(!plan.tx_hex.is_empty());
        // the destination addresses on the flow match the plan outputs
        assert_eq!(f.addresses.len(), 10);
        for (i, a) in f.addresses.iter().enumerate() {
            assert_eq!(plan.outputs[i].0, *a);
        }

        // wallet.dat state after prepare (qt-visible, exact legacy rows):
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 12); // 2 random + 10 derived
        assert!(w.hd_mnemonic.is_some());
        assert_eq!(w.hd_chain_index, Some(10));
        // defaultkey == the first derived address's pubkey
        let d0 = sweep::derive_sweep_destinations(&words, "").unwrap();
        assert_eq!(w.default_key.as_deref(), Some(d0[0].pubkey.as_slice()));
        // all 10 derived addresses present
        for d in &d0 {
            assert!(w.keys.iter().any(|k| k.address == d.address), "{}", d.address);
        }
        // and the derivation through the wallet's OWN path agrees
        let h2 = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        assert_eq!(h2.hd_chain_index, 10);
        let sk = h2.derive_hd_key(9).unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        let pk = secp256k1::PublicKey::from_secret_key(
            &secp,
            &secp256k1::SecretKey::from_slice(&sk).unwrap(),
        );
        assert_eq!(crate::wallet::pubkey_to_address(&pk.serialize()), d0[9].address);

        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_retry_path_is_idempotent_on_a_partially_migrated_wallet() {
        let dir = test_dir("retry");
        let path = random_key_wallet(&dir, "retry", 1);
        // simulate an earlier attempt that crashed after inserting 3 of the
        // 10 keys: seed set, indices 0..2 present, hdchain = 3
        {
            let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
            h.set_hd_seed(MN12, "").unwrap();
            for i in 0..3u32 {
                let secret = crate::seed::derive_triangles(MN12, "", 0, 0, i).unwrap();
                h.add_key(&secret, "").unwrap();
            }
            h.advance_hdchain_to(3).unwrap();
            h.close();
        }

        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        let inputs = vec![p2pkh_input(&path, 0, 9_000_000, 100)];
        let mut f = flow();
        f.lock_to_stored_seed(MN12, None).unwrap();
        f.prepare(&mut h, &inputs, 150, 1_500_000_000).unwrap();

        let plan = f.plan.as_ref().unwrap();
        assert_eq!(plan.keys_already_present, 3);
        assert_eq!(plan.keys_inserted, 7);
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1 + 10);
        assert_eq!(w.hd_chain_index, Some(10));
        // the sweep source excluded nothing (random-key coin) and the math holds
        let out_sum: i64 = plan.outputs.iter().map(|(_, v)| v).sum();
        assert_eq!(out_sum + plan.fee, 9_000_000);
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_refuses_an_already_migrated_wallet() {
        let dir = test_dir("migrated");
        let path = random_key_wallet(&dir, "migrated", 1);
        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        h.mark_seed_migrated().unwrap();
        let inputs = vec![p2pkh_input(&path, 0, 1_000_000, 100)];
        let mut f = flow();
        f.begin_restore();
        f.words_input = MN12.into();
        f.submit_restore();
        let err = f.prepare(&mut h, &inputs, 150, 1_500_000_000).unwrap_err();
        assert!(err.contains("seedmigrated"), "{}", err);
        assert_eq!(f.step, SweepStep::SeedVerified); // state unchanged
        // and the wallet is untouched (no keys added, no seed set)
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1);
        assert!(w.hd_mnemonic.is_none());
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_refuses_zero_inputs_and_all_on_destinations() {
        let dir = test_dir("zeropaths");
        let path = random_key_wallet(&dir, "zeropaths", 1);
        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();

        // zero inputs at all — refused BEFORE anything is written
        let mut f = flow();
        f.begin_restore();
        f.words_input = MN12.into();
        f.submit_restore();
        let err = f.prepare(&mut h, &[], 150, 1_500_000_000).unwrap_err();
        assert!(err.contains("no spendable UTXOs"), "{}", err);
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1);
        assert!(w.hd_mnemonic.is_none());

        // inputs that pay TO the destination addresses → nothing left to
        // sweep. The seed + derived keys ARE already in wallet.dat by then
        // (safe, valueless writes by design — the retry path relies on them)
        // but NO plan is produced and the flow stays on SeedVerified.
        let d0 = crate::seed::derive_triangles(MN12, "", 0, 0, 0).unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        let pk = secp256k1::PublicKey::from_secret_key(
            &secp,
            &secp256k1::SecretKey::from_slice(&d0).unwrap(),
        );
        let dest_input = SpendInput {
            txid: crate::legacy::sha256d(b"dest-prevout"),
            n: 0,
            value: 5_000_000,
            script_pubkey: crate::spend::script_pubkey_p2pkh(&pk.serialize()),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: d0,
            pubkey: pk.serialize().to_vec(),
        };
        let err = f.prepare(&mut h, &[dest_input], 150, 1_500_000_000).unwrap_err();
        assert!(err.contains("already sits on the seed-derived addresses"), "{}", err);
        assert_eq!(f.step, SweepStep::SeedVerified);
        assert!(f.plan.is_none());
        let w = crate::wallet::read_wallet(&path).unwrap();
        assert_eq!(w.keys.len(), 1 + 10); // seed-derived keys written (harmless)
        assert!(w.hd_mnemonic.is_some());
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_step_is_refused() {
        let dir = test_dir("wrongstep");
        let path = random_key_wallet(&dir, "wrongstep", 1);
        let mut h = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        let inputs = vec![p2pkh_input(&path, 0, 1_000_000, 100)];
        let mut f = flow();
        // still on Menu — prepare must refuse
        assert!(f.prepare(&mut h, &inputs, 150, 1_500_000_000).is_err());
        h.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn complete_carries_txid_and_scrubs_the_phrase() {
        let mut f = flow();
        f.begin_restore();
        f.words_input = format!(" {} ", MN24);
        f.passphrase = "pp".into();
        f.submit_restore();
        assert_eq!(f.passphrase, "pp");
        f.complete("0123abcd".into());
        assert_eq!(f.step, SweepStep::Done { txid: "0123abcd".into() });
        assert!(f.mnemonic.is_none());
        assert!(f.words_input.is_empty());
        assert!(f.passphrase.is_empty());
    }

    #[test]
    fn abandon_scrubs_everything() {
        let mut f = flow();
        f.begin_create();
        assert!(f.revealed_words().is_some());
        f.words_input = "leftover".into();
        f.passphrase = "sekrit".into();
        f.abandon();
        assert_eq!(f.step, SweepStep::Menu);
        assert!(f.mnemonic.is_none());
        assert!(f.words_input.is_empty());
        assert!(f.passphrase.is_empty());
        assert!(f.plan.is_none());
        assert!(f.dests.is_empty());
    }
}
