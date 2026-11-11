# GLM stand-in judge brief — tri7 fleet-chain-state skill delta (item-10 completion tick)

Artifact: tri7-fleet-chain-state skill delta, 2026-11-09 tick (workqueue item
10 COIN-SPLIT completion).

Kind: skill reference documentation (ops runbook knowledge), no code changes
this tick — the tick was fleet repair + verification.

What the delta records:
1. NEW reference `references/2026-11-09-fork-cascade.md`: the 2026-11-09
   fleet-wide fork cascade (PC fork #2, DNS3 fork, Pi fork) — root causes per
   node, the exact repair recipe per node, and the PC store-swap pathology
   (torn-tail store → blockidx build aborts with 0 frames → auto-from-zero →
   the from-zero path OVERWRITES tipstate.json with genesis state, poisoning
   every later start; the fix sequence: frame-boundary walk of the canonical
   file, boundary-exact trim, tipstate re-push only with the daemon verifiably
   dead, gap-frame construction from getblock-0 raw hex appended out of order
   — the index builder is hash-first so append order does not matter).
2. Completion addendum to `references/2026-11-08-coin-split.md`: the five
   item-10 completion steps with measured evidence (seed census 0.000000 on
   all 10 seed addrs; rebalance 181.0 TRI mined tx fcbac1c6…; PC staking ON
   persisted with attempts advancing + tip parity exact at h2208182; DNS3
   repaired with three checks passing, staking re-enabled in-RAM).
3. SKILL.md pointer updates so the cascade doc is discoverable from the
   fork/coin-split sections.

Claims to verify while grading:
- The lessons are stated as reusable rules (what to do, why), not a diary.
- The PC store-repair sequence is complete enough that an operator could
  repeat it: boundary walk → trim → dead-daemon-verified tipstate push →
  gap-frame build from DNS2 raw blocks → append → single clean load.
- No secrets: no private keys, WIFs, seed phrases, or RPC passwords in the
  delta text (the fleet facts quote hashes/txids/addresses only — public data).
- Consistency with the measured facts in the workqueue PROGRESS line
  (/root/tri7-lab/TRI7-WORKQUEUE.md, final entry, included as a file).

Out of scope for this grade (recorded, not fixed): the PC balances-snapshot
accounting artifact (book-address outputs counted as wallet coins) — future
code work, documented honestly as an artifact, not claimed as fixed.
