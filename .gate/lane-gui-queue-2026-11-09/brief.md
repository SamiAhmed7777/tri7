# Judge brief: tri7 lane-gui-queue — three legacy-parity completions from GUI-OPTIONS-AUDIT.md

## What this artifact is
One code change on tri7 (branch lane/gui-queue, base a6ce9fe; master's src/gui
files are identical to base — clean merge expected). Implemented by a MiniMax
Code (M3) delegate child; after a step-limit stop, a same-session continuation
correctly found item 1 already done and made no changes (run-2 zero-diff —
the final work is run-1's). Parent re-verified.

## The three items (audit rows 29 / 31 / 18)
1. **"Display addresses in transaction list" now WIRED** (audit row 29 claimed
   a dead toggle — TRUE on base: master's row builder did not honour the
   option). NEW `legacy_address_cell(address, label, display_addresses)`
   (app_state.rs:115) implements legacy `lookupAddress(address, tooltip=false)`
   (transactiontablemodel.cpp:370-383) EXACTLY: ON -> `Label (address)`;
   OFF -> `Label` only; unlabeled -> raw address; tooltip path untouched
   (always carries the address, legacy tooltip=true). The tx table cell now
   renders `view.address_cell` (app.rs) and the builder passes the LIVE
   `self.options.display_addresses` (app_state.rs:2106).
2. **Onion checkbox real** (row 31): NEW OptionsState field
   `show_onion_address` (default ON, serde default_true, legacy
   OptionsModel::ShowOnionAddress) + `transport_chip_visible()` gates the
   status bar's "⇗ Tor / I2P" chip (app.rs), replacing the always-on button.
3. **Detach-databases row DELETED** (row 18's recommendation) + the unused
   `tray_icon_only` field removed (grep-verified unread).

## Tests (+108 in src/gui/tests.rs)
- legacy_address_cell: ON -> "savings (addr)", OFF -> "savings", unlabeled ->
  raw address; options default assertions; onion-chip visibility; detach-row
  absence. Full suite on PRIVATE target dir (isolated from sibling lanes):
  **537 passed; 0 failed; 1 ignored** (base a6ce9fe = 535 passed + 1 ignored;
  +2 new test fns, helpers not counted as tests).

## Scope
git status = src/gui/app.rs, src/gui/app_state.rs, src/gui/tests.rs — exactly
the authorized file set. No consensus/daemon/docs changes.

## Files in this bundle
brief.md; guiq.diff (+198/-31); tests-guiq.txt (isolated-target suite tail).

## ROUND 2 — judge blocker addressed
legacy_address_cell corrected to BYTE-EXACT lookupAddress semantics
(transactiontablemodel.cpp:370-383): unlabeled -> `(address)` ALWAYS
parenthesized in both option states (legacy's second if fires on the empty
label alone); labeled + OFF -> `label ` (legacy's trailing-space quirk,
transactiontablemodel.cpp:372, kept byte-exact); labeled + ON -> `label
(address)`. Helper doc-comment now carries the full truth table. Tests
rewritten to the corrected expectations, including a row-view test for the
unlabeled path through tx_row_view (both option states). Full suite re-run:
see tests-guiq-r2.txt.
