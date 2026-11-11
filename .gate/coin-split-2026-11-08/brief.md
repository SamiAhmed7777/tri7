# COIN-SPLIT pre-land blocker fix — build_wallet_tx self-verify must zip the vin-sorted input list

Workqueue item 10 (COIN-SPLIT) live ops caught a REAL spend-builder defect on
the first send attempt. No coin moved (the failure is pre-broadcast, build-time).

## Live failure (2026-11-08 04:03-04:05Z, DNS2 19112, evidence live-failure-driver-log.jsonl)

`sendtoaddress` draining a seed address (18 coins: bare-P2PK coinstake rewards
+ P2PKH sweep outputs on ONE address — the first MIXED-solver selection ever
built) died twice with:

    self-verify failed: VerifyScript: scriptSig has 2 pushes, Solver expects 1

A second, separate dryrun-probe defect (selection "insufficient funds (have
X, need Y)" when fresh stake rewards are still immature) is fixed in the OPS
driver, not the node: the driver backs the amount off to the selectable pool.

## Root cause

`build_spend_outputs` signs the tx with `vin` in (txid, n) SORTED order
(documented). The 2026-11-07 fix sorted the list for `connect_inputs_check`,
but `build_wallet_tx`'s self-verify loop (follow_tip.rs ~5363) still iterated
the UNSORTED selection list. The position zip therefore pairs vin[i] with the
WRONG prevout script whenever selection order != sorted order.

- Same-type coins (every prior live spend since 2026-11-07) masked it: twin
  scripts verify identically at any position, so the misaligned check passed.
- Mixed types (P2PK + P2PKH) cross the Solver classes: a 1-push P2PK scriptSig
  checked against a P2PKH prevout (or vice versa) fails the ScriptSigArgsExpected
  count — on a CORRECTLY SIGNED transaction.

## Fix

One behavioral change in `OpsAdapter::build_wallet_tx` (src/bin/follow_tip.rs):
compute `ordered_sel` (the (txid, n) sort) ONCE before self-verify and zip BOTH
`verify_input` and `connect_inputs_check` against it. No signing, selection,
fee, or relay logic changed.

## Regression test

`tests::self_verify_zips_vin_sorted_order_not_selection_order` (follow_tip):
builds a real mixed-solver 2-input spend (real secp256k1 keys, P2PK + P2PKH
prevouts, selection order deliberately reversed from sort order) and asserts:
1. vin is (txid, n) sorted;
2. zipping verify against the SELECTION order fails with "Solver expects"
   (the live bug, pinned);
3. zipping against the SORTED order verifies every input.

## Test evidence (local run, this tree)

- cargo test --lib: 491 passed, 0 failed (1 ignored)
- cargo test --bin follow_tip: 49 passed, 0 failed (incl. the new regression)
- windows-gnu all-bins cross-build: see build-log.txt in this bundle

## Deployment scope

All four fleet nodes carry follow_tip (any node can build a send). Deploy
DNS2 (build from this tree) + sami-pc/Pi/DNS3 (CI artifact / host build)
before re-running the COIN-SPLIT drains.
