# Judge brief: tri7 lane-sweep-fix — seed-migration sweep key-match made corruption-proof

## The operator-blocking incident (live PC wallet, 2026-11-10)
The seed-phrase sweep migration refused: "UTXO 4d0be43c…:0 pays to
TNFvoQdbk7VDJ8oZD1EbKkrPmZAECh6Md7 but the wallet holds no key for it" —
while the daemon's listunspent shows that exact UTXO as solvable+spendable at
address TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7 (VALID checksum). The refusal's
address string FAILS checksum validation (isvalid=false) — two alphabet chars
differ from the real one. Root cause: the wallet's STORED address string for
this key is corrupted (legacy BDB→SQLite import era) and the sweep matched
keys by stored string.

## Root cause (child + parent verified)
NOT the encoder: version byte 65 + hash160 86cb0e9408ee60c88d72a126801a2990
4d7770df encodes to exactly the daemon-valid address (pinned by test). The
incident hash160 quoted in the task brief had absorbed the scriptPubKey's
OP_EQUALVERIFY byte (…0df88, 21 hex bytes) — the true 20-byte hash160 ends
…0df; the child caught the discrepancy and pinned the true vector with a
comment.
Fix: `AppState::sweep_source_candidates` matches UTXOs to keys by HASH160
from the UTXO's scriptPubKey (P2PKH bytes 3..23) against each key's
pubkey-derived hash160 (33/65-byte pubkeys only; empty pubkeys excluded so
hash160("") can't match). Stored-string lookup kept as a fast path. The
refusal message prints a freshly ENCODED address from the script hash160,
never the stored string. Fail-closed refusal policy unchanged. No wallet I/O
in tests (synthetic keys); no consensus/wallet-code changes.

## Tests (+3)
- address::sweep_incident_hash160_encodes_to_the_live_address — the exact
  live vector, payload + full-script paths.
- address::corrupted_sweep_address_fails_checksum_validation — encoder and
  validator agree the corrupted string is invalid.
- gui::sweep_key_match_uses_script_hash160_and_survives_corrupted_stored_
  addresses — selection succeeds when key and UTXO carry two DIFFERENT
  corrupted stored strings; uncorrupted fast path still selects; a truly
  unknown key still refuses WITH the fresh encoded address (never the
  stored string).

## Parent verification (adopting session, private target dir)
FULL cargo test --lib: **553 passed; 0 failed; 1 ignored**. Scope: src/
address.rs, src/gui/app_state.rs, src/gui/tests.rs only (+292/-2).
NOTE: the parent's own round-1 evidence in the task brief carried the same
absorbed-88 hash160 error — the child's correction was verified by the parent
against the daemon's live listunspent script before grading.

## Files in this bundle
brief.md; sweep-fix.diff (+292/-2); tests-sweep.txt (isolated suite tail).

## ROUND 2 — judge blocker addressed (hash160 is the SOLE authority)
The stored-string fast path is GONE. Selection now matches the script's own
parsed destination against a pubkey-hash160 map built from the wallet keys:
P2PKH → keys_by_hash160.get(hash160-from-script); bare-pubkey → hash160 of the
exact key bytes the script carries; P2SH/non-standard → None → refuse.
No string anywhere can select a key. Refusals print freshly-encoded addresses
of the script's real destination (P2PKH unchanged; bare-pubkey/P2SH now print
their real address instead of "<script not attributable>").
Judge's collision regression tests added and GENUINE: key A's stored string
equals the UTXO's stored string (checksum-clean) with A.hash160 ≠ script
hash160, key B.hash160 == script hash160 with a differing stored string →
selection returns B; under the round-1 code (string precedence) this test
FAILS, pinning the fix. Second test: only the colliding key exists → refuse
with the fresh address, never echoing the stored string.
Suite: 555 passed / 0 failed / 1 ignored (553 + the 2 collision tests),
private target-sweep; cargo check --bin tri7_wallet_gui zero new warnings.
