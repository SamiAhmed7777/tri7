# ROUND 2 — make multisig actually SPENDABLE from the wallet

The judge rejected round 1 with D. All five objections are legitimate. Fix them.

## The core defect

`build_spend_outputs_floor` signs every selected input with `sign_input`, and
`sign_input` explicitly errors for `SpendType::ScriptHash`. So a user can create a
multisig address, receive coins, and **never spend them** — worse than not having
the feature. `assert_owns_prevout` also returns `Ok(())` for ANY P2SH prevout
without checking that the wallet holds the redeem script or enough keys, so
selection can adopt coins it cannot spend.

## Required changes

### 1. A script book, not 22 struct edits
Do NOT add fields to `SpendInput` (22 construction sites across sweep/gui/bin).
Instead add in `src/multisig.rs`:

```rust
pub struct ScriptBook {
    pub scripts: std::collections::HashMap<[u8; 20], Vec<u8>>, // scriptID -> redeem
    pub keys: std::collections::HashMap<Vec<u8>, [u8; 32]>,    // pubkey  -> secret
}
impl ScriptBook {
    pub fn from_wallet(w: &crate::wallet::Wallet, keys: &[([u8; 32], Vec<u8>)]) -> Self;
    pub fn redeem_for(&self, script_pubkey: &[u8]) -> Option<&Vec<u8>>;
    pub fn held_keys(&self, redeem: &[u8]) -> Vec<([u8; 32], Vec<u8>)>; // only keys in the script
    pub fn can_spend(&self, script_pubkey: &[u8]) -> Result<(), String>; // redeem present + keys >= m
}
```

### 2. Ownership must be earned
- `SpendInput::assert_owns_prevout` keeps today's behaviour for PubKey/PubkeyHash and
  **returns an error for ScriptHash** ("P2SH prevout needs a script book").
- Add `assert_owns_prevout_with(&self, book: &ScriptBook) -> Result<(), String>` which
  for ScriptHash requires: redeem found for the scriptPubKey, `hash160(redeem)` matches,
  and the number of held keys >= m. Use it everywhere a book is available.
- Any code path that has no book must therefore REFUSE P2SH inputs (safe default).

### 3. Spend path
- Add `build_spend_outputs_floor_with_book(inputs, outs, change, rate, floor,
  book: &ScriptBook, n_time, spend_height)`. The existing
  `build_spend_outputs_floor` delegates with an empty book (so P2SH inputs are
  refused there, unchanged behaviour for single-key wallets).
- In the signing loop, branch on `solver_type`:
  - PubKey/PubkeyHash → `sign_input` (unchanged);
  - ScriptHash → `sign_multisig_partial(tx, i, redeem, &book.held_keys(redeem))`
    then `assemble_multisig_scriptsig(&sigs, redeem)` and assign the scriptSig.
    If the partial set is below m, return a clear error naming the input and the
    shortfall.
- `spend::build_spend` keeps its signature; add `build_spend_with_book` as the
  multisig-capable variant.

### 4. `assemble_multisig_scriptsig` must stop trusting the caller
Change its signature to `assemble_multisig_scriptsig(sigs: &[Option<Vec<u8>>], redeem: &[u8])`
— parse `m` and the pubkeys from the redeem script, require `sigs.len()` to equal
the pubkey count, and require at least `m` filled. Update its callers/tests.

### 5. Wire it into the real builders
- `src/bin/follow_tip.rs::build_wallet_tx`: build a `ScriptBook` from the live wallet
  (see item 6) and call the `_with_book` variant. This is the path that must be able
  to spend multisig coins.
- `src/gui/state.rs::build_signed_send`: same (it already has the wallet + key map).
- Where a pool is selected, skip P2SH inputs whose `can_spend` fails instead of
  adopting them (with a log line), so an unrelated P2SH UTXO cannot wedge a spend.

### 6. Wallet accessors
`src/wallet.rs` already has `put_redeem_script` / `get_redeem_script` /
`list_redeem_scripts`. Add whatever `ScriptBook::from_wallet` needs (e.g. iterate
redeem scripts + read keys) without changing the on-disk schema.

### 7. Tests (all must pass)
- T-A end-to-end: create 2-of-3, store the redeem script, build a UTXO paying the
  P2SH address, then call `build_spend_outputs_floor_with_book` and assert:
  the returned plan's input verifies via `verify_input` AND `connect_inputs_check`,
  and the scriptSig starts with `OP_0` and ends with the redeem script.
- T-B same but only ONE key in the book → the builder returns an error mentioning
  the shortfall (no partial plan).
- T-C `assert_owns_prevout` (no book) REJECTS a P2SH prevout; the `_with` form
  accepts it when the book has the redeem + m keys.
- T-D `ScriptBook::can_spend` false for an unrelated P2SH (no redeem), true for ours.
- T-E `assemble_multisig_scriptsig` rejects a slot count != pubkey count and derives
  m from the script (no caller-supplied m anywhere).
- T-F selection: a pool containing one unrelated P2SH UTXO skips it and still builds
  a valid P2PKH spend.
- Keep T1–T10 from round 1 green.

## Also fix (judge, buildability)
The submission must include `Cargo.toml` and `Cargo.lock` so the reviewer can see the
manifest; do not modify them.

## Constraints (unchanged)
No git state changes; no new dependencies; Tor-only (no P2P changes); do not touch
fees, GUI layout, BDB import semantics, or the updater.

## Report
Files touched, each test name with its assertion, and the last 3 lines of
`cargo test --release --lib` and `cargo build --release --target x86_64-pc-windows-gnu --bins`.
State plainly anything not done.
