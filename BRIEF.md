# TASK — P2SH multisig spend support in tri7 (Rust)

You are working in the git worktree `/root/tri7-multisig` (branch `feat/p2sh-multisig`).
This is a Rust crate for a Peercoin-style chain ("Triangles"). Consensus facts below
are verified from the legacy C++ at `/opt/triangles-v5/src` (READ-ONLY reference; never
modify it).

## Why

tri7 can already DERIVE a P2SH address (`crate::address::SCRIPT_ADDRESS = 28`,
used by `decodescript`), but it cannot validate or sign a spend FROM a P2SH output:
`SpendType` only has `PubKey` and `PubkeyHash`, `solver_type()` returns `None` for
`OP_HASH160 <20> OP_EQUAL`, and `OP_CHECKMULTISIG` appears only in the raw-tx decoder
table (`src/rawtx.rs`), never in evaluation. The chain itself fully supports P2SH and
CHECKMULTISIG (legacy `script.cpp`: opcode at :1043, multisig template at :1328,
`IsPayToScriptHash` branches at :1333 and :1721; legacy RPC has `addmultisigaddress`
at rpcwallet.cpp:836 and `addredeemscript` at :905; `CWallet::AddCScript` at wallet.cpp:190).

Goal: create an m-of-n P2SH multisig address, store the redeem script, build and sign a
spend from it, and verify that spend exactly the way the network would.

## HARD CONSTRAINTS

1. **Do NOT run any git state-changing command** (no `git add`, `commit`, `checkout`,
   `switch`, `stash`, `reset`, `branch`, `merge`, `rebase`, `push`). The parent commits.
2. **Do NOT add dependencies** to Cargo.toml. Use only what the crate already has
   (`secp256k1`, `sha2`, `ripemd`, `serde_json`, …). Check `src/spend.rs` for what is
   already imported for hashing and signing.
3. **Do NOT touch**: fee logic (`estimate_fee`, `select_with_fee`, `build_spend_outputs_floor`),
   the GUI, P2P, the updater, `bdb_import.rs` except one additive change (item 8).
4. Keep every existing test passing. Do not "fix" unrelated code.
5. Never touch `/root/.triangles/` or any real wallet. Tests must build their own keys.

## Required work (all 8 items)

1. **P2SH template recognition.** Add `SpendType::ScriptHash([u8; 20])` and make
   `solver_type()` recognize exactly this scriptPubKey shape:
   `OP_HASH160 (0xa9) 0x14 <20-byte hash> OP_EQUAL (0x87)`.
   Update `script_sig_args_expected` for the new variant.
   `assert_owns_prevout` must NOT reject a P2SH prevout as non-standard any more.

2. **Redeem-script construction.** New module `src/multisig.rs` (add `pub mod multisig;`
   to `src/lib.rs`). Provide:
   - `pub fn redeem_script(m: u8, pubkeys: &[Vec<u8>]) -> Result<Vec<u8>, String>`
     producing `OP_m <push pubkey>… OP_n OP_CHECKMULTISIG`.
     Use OP_1..OP_16 (0x51..0x60) for m/n. Pubkeys are 33-byte compressed → push prefix 0x21.
     Reject m == 0, m > n, n > 16, and any pubkey that is not a valid secp256k1 point.
   - `pub fn p2sh_address(script: &[u8]) -> String` using `crate::address` with
     `SCRIPT_ADDRESS` (version 28). Do not hand-roll base58.
   - `pub fn parse_multisig(script: &[u8]) -> Result<(u8, Vec<Vec<u8>>), String>`
     (the inverse, used by verification and tests).

3. **P2SH spending path in `src/spend.rs`.** A P2SH scriptSig is
   `OP_0 <sig1> <sig2> … <redeemScript>` (the OP_0 is Item 4's dummy).
   Implement verification for it inside the existing `verify_input` flow:
   - last push must be the redeem script; `hash160(redeem) == h` from the scriptPubKey,
     else fail with a clear message (mirror of `script.cpp:1721`'s IsPayToScriptHash branch);
   - then evaluate the redeem script as the effective scriptPubKey against the remaining
     pushes (that is exactly what the legacy interpreter does).

4. **OP_CHECKMULTISIG evaluation — consensus-critical.** Implement m-of-n verification:
   pop n, then n pubkeys, then m, then m sigs, **plus ONE EXTRA stack element that is
   popped and ignored** (the historical off-by-one in Bitcoin/Peercoin: scriptSig's
   leading OP_0). Signatures must be matched against pubkeys **in order** (each sig must
   match a pubkey at or after the last matched index). Enforce
   `SIGHASH_ALL`-signed ECDSA via the existing signature-hash helper in `src/spend.rs`
   with **scriptCode = the redeem script** (not the P2SH scriptPubKey).
   A scriptSig WITHOUT the leading OP_0 dummy MUST fail verification. Write that as a test.

5. **Partial signing.** `pub fn sign_multisig_partial(tx, n_in, redeem_script, keys) -> Result<Vec<Option<Vec<u8>>>, String>`
   returning one slot per pubkey in the redeem script, filled when the wallet holds that
   key. Provide `pub fn assemble_multisig_scriptsig(sigs: &[Option<Vec<u8>>], m: usize, redeem: &[u8]) -> Result<Vec<u8>, String>`
   that fails unless at least m signatures are present, and emits them in **pubkey order**
   with the leading OP_0.

6. **Wallet storage for redeem scripts.** In `src/wallet.rs`, add helpers to store and read
   a redeem script keyed by its `scriptID` (hash160 of the script), using the existing
   `main(key BLOB, value BLOB)` table and a `script` prefix byte, mirroring how other
   prefixed rows are written there. Provide `put_redeem_script` / `get_redeem_script` /
   `list_redeem_scripts`. Do not change the existing schema.

7. **RPC verbs.** In `src/rpc.rs` implement and register:
   - `addmultisigaddress <nrequired> ["pubkey"|"address", …] [account]` → returns the P2SH
     address and stores the redeem script (legacy reply shape: a bare address string).
   - `addredeemscript <hex> [account]` → stores it, returns `true`.
   Both belong in the same method lists the neighbouring wallet verbs use
   (`NATIVE_METHODS` / `OPS_METHODS`), and must work before/independent of the fee path.
   Return legacy-style error codes (-8 bad params, -5 bad address/key, -4 key not found).

8. **Import the legacy `script` section (additive only).** In `src/bdb_import.rs`, if the
   dumped wallet carries a `script` section, carry those rows into the new wallet's
   `script`-prefixed rows so an imported wallet keeps its redeem scripts. If the section
   is absent, change nothing. Keep it idempotent like the existing metadata anchoring.

## Acceptance targets — measure and report each number

Write unit tests (in the modules, `#[cfg(test)]`) covering at least:

- T1 `redeem_script(2, 3 keys)` → exact expected hex; report it. Expect
  `52 21 <pk1> 21 <pk2> 21 <pk3> 53 ae` (m=2, n=3) — state the measured length.
- T2 its P2SH address starts with `C` (version 28) and is stable across runs; report it.
- T3 `solver_type(p2sh_scriptpubkey)` returns `ScriptHash(h)` where `h == hash160(redeem)`.
- T4 a full 2-of-3 spend signs with two keys, verifies via `verify_input`, and the
  assembled scriptSig starts with `OP_0`; report signature byte lengths.
- T5 the same spend with the leading OP_0 REMOVED fails verification (dummy required).
- T6 wrong redeem script (hash mismatch) fails with a hash-mismatch error.
- T7 signatures out of pubkey order fail; assembled in order pass.
- T8 partial signing with only one key returns 1 filled slot and
  `assemble_multisig_scriptsig` errors (needs m=2).
- T9 `parse_multisig(redeem_script(2,3))` round-trips to `(2, 3 pubkeys)`.
- T10 a P2SH 1-of-1 spend also verifies (boundary case).

Also verify these commands and paste their **last 3 lines** each:

```
cargo test --release --lib
cargo build --release --target x86_64-pc-windows-gnu --bins
```

Both must be green. If the Windows target is unavailable, say so explicitly instead of
claiming success.

## Reporting

When done, print a summary with:
- every file you touched (paths only),
- the measured values for T1–T10 (hex, addresses, lengths, counts),
- the two command result lines,
- anything you could NOT do, stated plainly.

Do not claim a test passed unless you ran it and saw the pass line.
