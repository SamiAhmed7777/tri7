# Artifact: tri7 GUI UX pass 2 — first-run doors + plain-language send/receive (operator directive)

Kind: code (GUI-only change; every hunk is inside src/gui/**)
Repo: /root/tri7 (master 1075100 + this working-tree diff, 6 modified files + 1 new file, all under src/gui/)
Full test suite: gui module **166 passed / 0 failed** (baseline gui suite + 11 new tests; suite re-run green on this exact tree). cargo check --lib --features gui: 0 errors. cargo build --release --features gui --bin tri7_wallet_gui: exit 0.

## Operator directive
Pass 1 (merged commit 897024c) was "the wallet speaks ordinary-people language". Pass 2 extends it:
the FIRST-RUN flow (what a brand-new user sees) and the SEND/RECEIVE flows, killing the remaining jargon.

## What changed (all src/gui/**)
1. **First open no longer generates a wallet behind the user's back.** New
   `AppState.first_run_welcome` drives a "Welcome to Triangles" screen with three doors:
   Create a new wallet / Restore a wallet you already have / Open a wallet file. Nothing is
   generated until a door is clicked (before, a user wanting to RESTORE was dropped into
   freshly generated words and had to discover Cancel).
2. **Two seed-flow voices**: flows entered from the welcome menu get FIRST_RUN_NOTE (what the
   words DO); flows opened from an existing wallet keep HONEST_NOTE (why a phrase cannot be
   retro-fitted onto pre-HD keys). Same screen, different reader.
3. **Plain words module** (`plain_words.rs`, new): every pass-2 English string lives in one
   display-independent module whose constants double as i18n keys — so unit tests assert the
   copy without a screenshot. 24 new keys added to ALL 11 language tables (40 → 64 keys each).
4. **Jargon demoted, not deleted**: inputs → "your addresses", TxID → "payment ID",
   broadcast → "send now", raw signed transaction → "the payment as raw data (for support)",
   derivation path demoted to a tooltip/parenthetical, "6 decimals" → "0.000001 is the
   smallest amount", Estimated network fee → "Network fee" + "What this send will cost".
5. **Empty states explicit**: Overview shows "No coins yet — open Receive Coins…" for a loaded
   zero-balance wallet; Send says there is nothing to send and points at Receive instead of a
   silently empty address picker.
6. **Dead/duplicate UI removed**: a duplicated paragraph on the load screen; a seed-migration
   entry that could only error from that screen (moved to Receive where a loaded wallet can sweep).
7. **Send progress rail renamed to the user's steps**: "Who and how much" / "Check it over" / "Sent".
8. **i18n discipline enforced by test**: the completeness test now covers PHASE1+PHASE2 and FAILS
   on any pass-2 entry whose translation is byte-identical to English (brand nouns exempt on
   phase-1 keys). A jargon-guard test scans all pass-2 copy for mnemonic/BIP39/pre-HD/UTXO/
   input/checksum/derivation/m44/sats/txid/hex so future edits cannot quietly reintroduce them.

## Tests (+11, gui module 166/0)
doors render + welcome precedence, jargon guard, empty-balance rule, fee sentence, amount hint,
backup line, seed-flow voice split, i18n wiring (PHASE2 complete everywhere), flag default, and the
extended completeness test. All pre-existing gui tests pass unchanged except one seed_flow assertion
updated to the new plain-language error text.

## Verification (re-run by the parent on this exact tree)
- cargo test --lib gui:: → 166 passed / 0 failed
- cargo check --lib --features gui → 0 errors
- cargo build --release --features gui --bin tri7_wallet_gui → exit 0
- diff scope: `git status --porcelain` modified files = exactly the 6 declared gui files (+1 new gui file);
  nothing outside src/gui/ changed.
