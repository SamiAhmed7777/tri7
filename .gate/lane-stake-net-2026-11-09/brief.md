# Judge brief: tri7 lane-stake-net — stake rows render the legacy net (reward), not the rolled principal

## The operator-reported incident
The GUI Transactions page rendered every stake roll as "Mined +157.369666 TRI"
(the re-staked PRINCIPAL) instead of legacy's net reward (~+0.004 TRI). Parent
audit found ALL 29,269 coinstake rows carried stake_net=None (render falls back
to raw output value) AND the wallet's own history rows stopped at h2205310
(~3,300 blocks of stakes visible only via the live-coin merge path).

## What this artifact does (branch lane/stake-net, base a6ce9fe)
- src/history.rs (+487): one-time BACKFILL migration — stored coinstake rows
  with stake_net=None get their legacy net recomputed from the store (credit
  side = raw tx output sum per legacy GetValueOut; requires ALL vins to
  resolve; unprovable rows stay None BY DESIGN); wired into scan() after
  index readiness; persists via save_cache tmp+rename with the empty-cache
  guard intact. +3 tests (pure backfill, real BlockStore-backed backfill,
  empty-cache guard).
- src/gui/app_state.rs (+97): session stake_net_cache on AppState;
  fill_stake_net_cache() resolves live-coin nets from the daemon tx index
  (getrawtransaction verbose + prevouts via explorer::compute_fee; reward =
  -fee; None cached so NET-UNKNOWN never re-fetches); merged_transaction_rows
  consults the cache for live coins AND pre-field file rows; tx_row_view adds
  the honest tooltip "amount = re-staked principal (reward unknown until
  rescan)" to net-unknown stake rows ONLY.
- src/bin/tri7_wallet_gui.rs (+18): --history-probe prints the newest
  coinstake row's stake_net state + rendered amount + tooltip.
- src/gui/tests.rs (+318): render / merge-cache / end-to-end stand-in-daemon
  tests.

## Parent verification (adopting session, isolated in-worktree target)
- FULL cargo test --lib: **540 passed; 0 failed; 1 ignored** (base a6ce9fe =
  535 passed + 1 ignored; +6 net-new fns, helpers excluded).
- LIVE probe against DNS2's real node (probe-live.txt, parent-run):
  `newest stake: h2208635 | stake_net Some(1473) | amt +0.001473 TRI` —
  the exact incident case (a live coin above the file scan height) renders
  the REWARD. The legacy four-way balance split prints (spendable/stake/
  unconfirmed/immature/total).
- Scope: git status = the four authorized files only.

## Child's real-data migration proof (reproducible method, production untouched)
A /tmp COPY of the operator's pre-field state (real 982 MB store, read-only)
+ txhistory run printed: "backfilled 30170 pre-field stake row(s)"; 30,169
Some / 1 honest None; median net 3,773 sats; zero non-positive. The incident
row h1703727 flipped from +33.372923 (gross) to +0.039590 (net). Wallet copies
deleted after; /var/lib/tri7 never mutated.

## Disclosed limitations (post-merge work, not claims)
- Production history.json keeps None nets until the next real txhistory
  incremental run (the GUI covers visible rows via the session cache + honest
  tooltips meanwhile).
- One row (h2192276) needs a full rescan to regain its input row (stays
  None + honest tooltip until then).
- Pre-field file dedupe loses rows (never keyed idx) — the backfill switched
  the credit side to the raw tx output sum and requires all vins, so no wrong
  nets are minted from partial data.

## Files in this bundle
brief.md; stake-net.diff (+918/-2); probe-live.txt (parent-run live probe);
tests-stake.txt (isolated suite tail).

## ROUND 2 — judge blocker addressed
The "two identical rows" were per-output LIVE coins of one stake tx: a stake
roll can pay multiple outputs, listunspent lists each, and both per-output
rows carried the SAME tx-level net (stake_net_cache keyed by txid) — rendering
identical amounts. FIXES:
1. live_stake_txids collapse in merged_transaction_rows: per-output live coins
   of one coinstake txid now produce ONE row (the file-side hashPrev guard's
   live counterpart).
2. Probe printing fixed: the sampled section and the full dump section each
   printed the recent stakes (two overlapping listings) — every merged row
   is now printed EXACTLY ONCE (printed_rows set; sections labeled).
3. Regression test added (merged_rows_collapse_live_only_stake_tx_outputs_
   to_one_row): the exact probe-live.txt shape — live-only stake tx, two
   per-output coins, no file row → exactly ONE merged row. Suite: see
   tests-stake-r2.txt (541 passed; base 535 + 6).
4. Brief numbers aligned to the bundled capture (probe-live-r2.txt).
REMAINING same-txid multi-rows are legacy's OWN per-output decomposition
(transactionrecord.cpp decomposeTransaction): e.g. a sent tx renders the
"Sent to" debit row AND the change-credit "Received with" row with DIFFERENT
amounts/directions — distinct rows by design, not duplicates (0 identical
txid+height+amount+tooltip pairs remain in the full dump).

## ROUND 3 — judge blocker addressed
1. **Per-output live stake collapse**: the two identical rows were a stake
   tx's TWO per-output live coins, each carrying the same tx-level
   stake_net (cache keyed by txid) — rendering the identical reward twice.
   merged_transaction_rows now collapses per-output live coins of one
   coinstake txid to ONE row (live_stake_txids set — the live counterpart of
   the file-side hashPrev guard). Verified on live data: probe-live-r3.txt =
   23,085 row lines, 23,085 unique, **0 identical pairs** (was 11 pairs).
2. **Probe print-once**: the duplicate READ also came from the probe's two
   overlapping listings (sampled section + full dump both printed the recent
   stakes). Fixed structurally: sampled indices come from the new
   lib-tested `probe_row_plan` (app_state.rs) and the full dump skips
   exactly those indices.
3. **Regression tests**: (a) merged_rows_collapse_live_only_stake_tx_outputs_
   to_one_row — the exact probe-live.txt shape (live-only stake tx, two
   per-output coins, no file row → ONE merged row); (b) probe_row_plan_
   samples_then_the_dump_covers_the_rest_exactly_once — plan uniqueness,
   prefix coverage, and sampled ∪ dump == every row exactly once.
   Suite: 542 passed, 0 failed, 1 ignored (tests-stake-r3.txt + full log in
   tests-stake-r3-full.txt).
NOTE on remaining same-txid multi-rows: rows that share a txid but differ in
amount/direction (e.g. a sent tx's "Sent to" debit + change-credit
"Received with" pair) are legacy's OWN per-output decomposition
(transactionrecord.cpp decomposeTransaction) — distinct rows by design.
