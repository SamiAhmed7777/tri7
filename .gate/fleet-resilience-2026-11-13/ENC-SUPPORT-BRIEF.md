# Encrypted-wallet support — old encrypted Triangles wallets still work

Operator request (Sami, 2026-11-14, replying to the disabled Encrypt item):
"Put this from legacy and make sure it works right... so old encrypted
Triangles wallets still work."

## What already existed (discovered, not written here)

- `wallet_encrypt.rs`: the FULL legacy crypter port (CMasterKey serde,
  EVP_BytesToKey/SHA-512 KDF method 0, AES-256-CBC master+per-key encryption,
  IV = SHA256d(pubkey)[0..16], pubkey cross-check as the passphrase proof) —
  encrypt/decrypt/change-passphrase/is-encrypted, implemented from the legacy
  SOURCE with zero tests.
- The embedded node's locked-wallet machinery (locked flag, walletpassphrase
  timeout, sync-and-watch while locked).
- The GUI's session keystore (AbEntry.secret) whose entries already sign via
  the merged key map.

## What this commit adds

1. `wallet.rs`: `read_wallet_allow_encrypted` — the mkey/ckey scan returns
   the wallet SKELETON (balances/history rows; keys EMPTY — ckey rows need
   the passphrase) instead of refusing. The strict `read_wallet` keeps its
   refusal contract (keytool/bdb-import tests hold).
2. `gui/state.rs`: `load_wallet_allow_encrypted` → (WalletData, encrypted).
3. `gui/app_state.rs`: two-phase encrypted open:
   - `load_wallet_action` reads with the allow variant; on an encrypted store
     it parks the skeleton (`encrypted_pending`) and sets `encrypted_prompt`
     — the load screen then asks for the wallet's passphrase.
   - `unlock_encrypted_action(passphrase)` calls the legacy
     `decrypt_wallet`, injects every recovered key into the SESSION keystore
     (memory only — the file on disk stays encrypted and untouched), sets
     receive/sign defaults to the first decrypted address, and runs the
     normal derive/finish path. The merge into the signing key map is the
     pre-existing keystore behavior (AbEntry.secret).
   - A wrong passphrase surfaces decrypt_wallet's own human verdict
     ("wrong passphrase (the master key did not decrypt)") in the prompt.
4. `gui/app.rs`: the encrypted-prompt card on the load screen ("This wallet
   is encrypted — enter its passphrase… keys stay in memory only while the
   app runs — the file on disk stays encrypted."), Unlock wallet / Cancel.
5. `wallet_encrypt.rs` TEST MODULE (the crypter previously had ZERO tests):
   `encrypted_wallet_roundtrip_and_strict_refusal` — synthesizes a
   legacy-format store, encrypts, asserts: strict reader refuses,
   allow-encrypted reader returns the skeleton, the passphrase recovers the
   ORIGINAL key (pubkey cross-check), a wrong passphrase is rejected.

## Security posture

- Plaintext keys exist ONLY in process memory for the session; the wallet
  file stays encrypted on disk and is never rewritten.
- The passphrase is never persisted; failed unlocks leave the session
  un-loaded.
- The GUI cannot read BDB-era files directly (unchanged) — the existing
  BDB import lane migrates those first, then encryption applies as today.

## Verification

cargo test --lib: 795 passed / 0 failed / 1 ignored (+3 the roundtrip, uncompressed-legacy and AppState unlock-flow tests);
GUI featureful check EXIT_GUI=0; windows-gnu release --bins EXIT_WIN=0
(gates.log = exactly ONE final-tree run).

FILES: src/wallet.rs, src/gui/state.rs, src/gui/app_state.rs, src/gui/app.rs,
src/wallet_encrypt.rs

## Round-1 verdict (C, urn:ump:7tz3533yucqtwpsizsfqgeepbl5j55gentre3es5hatuxxai5ygq) — resolutions

- BLOCKER "wrong passphrase removes the pending wallet and exits the prompt
  (unretryable)" — FIXED: decrypt runs FIRST; the skeleton and prompt survive
  a failure, so the user can retry. `let Some(mut wallet) = ...take()` now
  happens only after a successful decryption.
- BLOCKER "finish_load derives payloads exclusively from wallet.keys, which
  is empty for the skeleton — balances/UTXOs would appear empty" — FIXED:
  unlock populates wallet.keys IN MEMORY from the decrypted secrets
  (WalletKey rows: pubkey/secret/compressed/address), so the existing
  derive path sees every decrypted address. The keystore entries remain as
  the session-signing mirror.
- BLOCKER-adjacent "the added test does not exercise the GUI unlock state
  machine" — ADDED: `encrypted_wallet_load_prompt_retry_and_unlock` in
  gui/tests.rs — encrypts a real store, runs load_wallet_action (prompt
  shown, wallet state still None), a wrong passphrase (Err + prompt +
  skeleton survive), the correct passphrase (wallet loaded, key in the
  signing map, derive ran, receive defaulted to the decrypted address).
- POLISH "unnecessary mutable binding" — the binding is now genuinely
  mutated (wallet.keys push); kept.

## Round-2 verdict (F, urn:ump:hfweeutimiwzfo6oa3z5axkxqzbeq4a6bdcwjphxhatthsppqoxq) — resolutions

- BLOCKER "decrypted secrets appended to the GUI keystore — keystore entries
  persist to keystore.json in plaintext hex, violating the memory-only
  claim" — FIXED by REMOVING the keystore insertion entirely. Decrypted keys
  live ONLY in wallet.keys (the in-memory WalletData model — not
  serialized, never persisted); key_map() already merges wallet.keys for
  signing. REGRESSION TEST added to the AppState unlock test: after unlock,
  no keystore entry carries the decrypted address.
- BLOCKER "only 33-byte compressed ckeys accepted; 2013-era uncompressed
  (65-byte) wallets cannot unlock" — FIXED: decrypt_wallet accepts 33-byte
  (0x02/0x03) AND 65-byte (0x04) ckeys, and verifies the recovered secret
  against the pubkey in the SAME encoding the row stored. NEW TEST:
  decrypt_wallet_supports_uncompressed_legacy_keys hand-builds an
  uncompressed ckey store with the legacy primitives and roundtrips it.
- BLOCKER "unlock mutates state before finish_load; a load failure loses
  the pending wallet" — FIXED TRANSACTIONALLY: decryption runs first (a
  wrong passphrase mutates nothing); the pending wallet is only consumed
  after finish_load SUCCEEDS — on failure it is restored to
  encrypted_pending with the prompt re-shown and an honest retry message.
- POLISH "brief says 793 while gates report 794" — the suite grew with the
  new tests; this brief states the FINAL count (795) matching gates.log.

## Round-3 verdict (C, urn:ump:nwdguvp33oiz6zvwzzih6aztftjutca7ad4pxexzkbz3dhip62cq) — resolutions

- BLOCKER "transactional restore puts the plaintext-bearing clone back into
  encrypted_pending — a retry re-appends duplicate keys" — FIXED: the
  KEYLESS skeleton (cloned BEFORE key population) is what goes back on a
  finish_load failure; the plaintext-bearing copy is dropped with the failed
  attempt. A retry decrypts fresh and appends into a fresh copy — each key
  exactly once.
- BLOCKER "the r3 transactional fix is unverified — no test forces
  finish_load to fail" — FIXED: the AppState test now CORRUPTS the UTXO
  snapshot file (unparseable JSON → derive fails), asserts the failure is
  transactional (keyless skeleton restored, no plaintext keys retained, no
  half-loaded wallet), then FIXES the file and asserts the retry loads each
  key exactly once.
- POLISH "stale keystore references in comments" — updated (wallet.rs +
  app_state.rs comments now say the keys live only in the in-memory
  WalletData model, never persisted to disk or keystore.json).
- POLISH "read_pubkeys should recognize 65-byte uncompressed ckeys (locked-
  node watch parity)" — DONE: read_pubkeys now accepts 33- and 65-byte ckeys
  so a locked node watches uncompressed-key addresses too.
