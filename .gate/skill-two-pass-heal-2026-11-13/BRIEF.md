# Skill delta: tri7-fleet-chain-state §16 — repairwallet two-pass heal verification

## What changed
ONE new bullet inserted into section 16 (staking races / serving truth), placed
after the item-20 STALE-LIVE-VIEW bullet and before the item-22 from-zero
bullet. No other content touched (verified by copy: this bundle's SKILL.md IS
the post-patch file).

## The lesson (live-caught 2026-11-13 01:0xZ on DNS2)
A repairwallet pass that reports {"mismatched spent coins":N,"amount affected
by repair":...} does NOT guarantee stale_live_view cleared. DNS2 carried a
raced-mint facts-mismatch pair (coin f7b4da80...:1 claims h2210711 with
block_time 1794530407 vs the chain slot's 1794530424; first_utc 00:42:55Z):
- pass-1 repaired 3 OTHER mismatched coins / 43.969306 TRI, but
  stale_live_view PERSISTED (same example coin, same first_utc, ticks kept
  climbing 96→106).
- pass-2 answered {"wallet check passed":true} and the re-probe showed
  stale_live_view:null — VERIFIED cleared.
- getbalance was identical (1901.160559) before pass-2 and after: the phantom
  coins never entered the balance; the interim exclusion is non-fatal
  (staking enabled, mints flowed: 2 blocks minted during the heal window).

## The rule added to the skill
Re-probe getstakinginfo after EVERY repairwallet pass; repeat until BOTH:
"wallet check passed":true AND stale_live_view:null.

## Acceptance checks
1. The bundle's SKILL.md contains the new bullet verbatim in §16 (grep the
   marker string "does NOT guarantee the stale view cleared").
2. The item-20 and item-22 bullets remain intact around it (ordering).
3. No other section modified.
