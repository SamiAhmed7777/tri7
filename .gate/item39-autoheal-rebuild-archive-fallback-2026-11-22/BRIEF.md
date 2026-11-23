# ITEM 39 — AUTOHEAL-REBUILD-ARCHIVE-FALLBACK (judge brief)

Date: 2026-11-22 · Repo: /root/tri7 (master) · Files: src/rpc.rs, src/node_embedded.rs
Evidence: /root/tri7-lab/continuity_2026-11-22-2102.out, item39_probe_2026-11-22-2105.out, item39_followup_2026-11-22-2110.out

## Defect (found live on DNS2, 21:00:44–21:05:02Z, RACE 42 aftermath)

The in-process AUTOHEAL repairwallet remedy (triggered when DNS2's own mint
h2217700 was orphaned by a 2.04x-work branch) rebuilt the wallet live view
over h2212919..h2217702. For ~4 minutes the money surfaces then served
WRONG figures as if current:

- getbalance: balance_source=wallet-live plainbal 1832.171267 — a −3712.88
  TRI under-report vs the live-index truth 5545.048964 (coins credited in
  h2208645..h2212919 = the coverage hole between the archive pair's frozen
  height and the rebuild's walk base).
- listunspent: 215 rows 1811.638517 — byte-identical to the frozen Nov-19
  archive pair (the merge silently dropped the live contribution).
- TRsxnwXe rows=0 — the item-38 custody bar transiently vanished.

Custody was never at risk (addrindex carried truth; full self-restoration
by 21:11:06Z). Impact class: every future raced-mint AUTOHEAL reopens the
window; alarm-integrity defect.

## Root causes (3, all in this diff)

1. **Tier-1.5 gate was a TIP test, not a SUPERSET test.**
   `wallet_total()` preferred the wallet-live view over the snapshot pair
   whenever `coverage_height (chain tip) >= pair coverage`. A view rebuilt
   over [h2212919..tip] passes that while missing everything below its
   floor — the −3713 TRI hole served as `balance_partial=false`.
2. **No rebuild-in-progress signal.** The repairwallet/AUTOHEAL replay
   holds the live-view mutex across seed+diff+swap; mid-rebuild the merge
   (`live_rows_for`) failed to lock and silently returned no live rows
   (the pure-archive listunspent), `balance()`/`coverage()` would block,
   and nothing labeled the window.
3. **Full-history rebuild cost.** Commit 1db2457 (already on master) seeds
   the live view from h0, so future rebuild replays can hold the mutex for
   ~70s — blocking every money RPC for that long is its own defect.

## Fix

src/rpc.rs:
- `LiveWalletTotal` gains `coverage_floor: u64` (the view's seed base;
  the view covers [floor, tip]).
- `WalletProvider` gains `fn live_view_rebuilding(&self) -> bool { false }`.
- `wallet_total()`: tier 1.5 requires `coverage_floor <= pair_coverage`
  in addition to the existing tip gate (superset test); tier 1.5 is
  skipped entirely while `live_view_rebuilding()`; the tier-2 `partial`
  probe skips the `balance()` call (same mutex) mid-rebuild and takes the
  honest default (pair trails tip → `balance_partial=true`).
src/node_embedded.rs:
- `LIVE_VIEW_REBUILDING: AtomicBool` + `live_view_rebuilding()` +
  RAII `begin_live_view_rebuild()` guard (clears on every exit path).
- `LiveWalletProvider`: overrides `live_view_rebuilding`; publishes
  `coverage_floor: w.base` in `live_wallet_total` (and answers None while
  rebuilding); `coverage()`, `balance()`, `utxos()` skip the mutex-locking
  live branch mid-rebuild (utxos serves pair rows alone — the SAME rows
  wallet_total tier 2 sums, so total == sum(list) is preserved and the
  confirmations key at the pair's coverage, labeled partial).
- `autoheal_stale_live_view()` and the `repairwallet` RPC verb raise the
  flag for the whole remedy (the caller holds the mutex there).

Note on spec (i) of item 39: the hole itself is structurally closed by
1db2457 (boot seed from h0 ⇒ `wl.base=0` ⇒ AUTOHEAL min() replays from
genesis — never a hole again). This diff makes the surfaces honest for
any residual/mid-flight case and deploys both.

## Tests (4 new, rpc.rs; suite must stay green)

- `item39_holey_live_view_falls_to_the_labelled_pair_tier` — the exact
  DNS2 shape: floor above pair coverage ⇒ snapshot-pair + partial=true,
  never the hole figure; getbalance == sum(listunspent) at the tier.
- `item39_superset_live_view_still_wins_tier_15` — pins the 2026-11-16
  fix (genesis-floor view still serves wallet-live, partial=false).
- `item39_rebuilding_flag_skips_tier_15_to_the_labelled_pair` — mid-flight
  rebuild ⇒ snapshot-pair + partial=true even for a ready superset view.
- `item39_seeded_gap_inside_pair_era_falls_to_the_labelled_pair` — the
  debris-skip gap gate (judge round-1 blocker): first skip inside the
  pair's era ⇒ snapshot-pair + partial=true even with a passing floor.

Suite: 889 passed / 0 failed (885 pre-existing + 4 new); windows-gnu
target check clean.

## Judge round 1 (GRADE=C) — blockers addressed in this revision

1. **False superset via debris-tolerant seed skips.** `WalletLive::seed`
   (fused23 debris tolerance) skips torn/missing blocks and `base` alone
   lies about the floor — a skip INSIDE the pair's era passes a floor-only
   gate. FIXED: `WalletLive` now carries `first_skip: Option<u64>` (the
   lowest unread height, set by the seed loop); `LiveWalletTotal` carries
   `first_skip_height`; the tier-1.5 gate refuses the view unless every
   skip sits strictly ABOVE the pair's coverage. Regression test
   `item39_seeded_gap_inside_pair_era_falls_to_the_labelled_pair` pins it.
2. **Silent Err(_) skip = unlabeled hole.** FIXED: the seed's skip
   summary moved from bare `eprintln!` to the node's structured `log()`
   channel (lands in node.log, alarmable) and now states that tier 1.5
   will refuse the view over any pair covering the skipped height or
   above — the hole is labeled on the view itself, not just greppable.
3. **Nested/overlapping guards.** FIXED: `LIVE_VIEW_REBUILDING` is now an
   `AtomicU64` counter (fetch_add/fetch_sub) — the AUTOHEAL path and the
   `repairwallet` verb can overlap (one waits on the mutex), and a bool
   would have let the first guard's drop clear the other's window.
4. Blast-radius note: the debris-tolerance skip itself is commit e873b42
   (already on master, operator lane) — this diff only makes its hole
   HONEST (carried + gated + logged), it does not change skip behavior.

## Risk

- Tier-1.5 floor gate: any provider that previously published a view with
  an implicit floor above the pair now answers from the labelled pair —
  the intended fix. Genesis-seeded views (floor 0) are unaffected.
- The daemon flag is process-global (same pattern as LIVE_BRIDGE);
  set/clear are RAII-scoped to the two repair paths only.
- Existing tier tests updated mechanically (`rebuilding: false` field).

## Verification plan (post-judge)

cargo test (all suites) → windows-gnu target check → commit → push →
deploy on DNS2 (pull, release build, systemctl restart tri7d) → item-38
battery + parity x3 → live exercise: run `repairwallet` on DNS2 and take
a CONCURRENT getwalletinfo mid-hold, expecting the labelled pair tier
(balance_source=snapshot-pair, balance_partial=true) with no RPC stall.
