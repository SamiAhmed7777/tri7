# ITEM 36 — DEFERRED-REORG STALE-DROP (fork-body livelock of a race loser)

## What you are grading
A single-file Rust change to a blockchain node's adoption-body dispatch gate
(src/node_embedded.rs, the tri7 Triangles rewrite, master @ bce1b9d).
`item36.diff` is the complete change: 261 insertions, 8 deletions —
(a) the pure predicate `pending_entry_is_stale` gains a 4th parameter
`reorg_pending`; (b) a new accessor `pending_adoption_live(Instant)`; (c) the
one and only production call site (the adoption-body dispatch pop, ~line
15133 post-patch) reads `reorg_pending` once per dispatch pass; (d) the
item-35 regression test updated to the 4-arg signature with `false` (all
existing verdicts unchanged); (e) two new regression tests. No production
call site other than the dispatch pop exists (verified: grep shows only
line 15133 + tests call it).

## Why (the live incident this fixes)
2026-11-20 00:30–00:57Z, DNS3 (live fleet node): DNS3 minted h2215652
`98071c4c` and LOST an equal-work race against DNS2's canonical `40df35c3`
(same height, different hash). DNS3's fork choice correctly selected the
heavier branch on headers and DEFERRED the rollback until the branch's first
body validates (PendingAdoption, the T7-03 design). But item-35's dispatch-pop
stale gate (`height <= tip || hash_in_chain`) then dropped the branch's FORK
BODY h2215652 every ~30s: the fork body sits AT the pre-rollback tip height
with a not-yet-in-chain hash, so `height <= tip` judged it "canonically dead",
it was never fetched, its children could never validate, the rollback never
completed, fork choice re-fired, re-queued, re-dropped — a deterministic
livelock. The item-35 give-up cap could never fire because each drop
`body_tries.remove()`s the entry (fresh attempts every rotation). DNS3 was
stuck 27 minutes until an operator restart. This is a correctness+liveness fix
for the consensus-adjacent reorg path; DNS2 (the fleet's sole heavy staker)
and Pi still run the pre-fix binary, so a lost race there would starve the
whole fleet's mint cadence.

## The fix's semantics
`reorg_pending = a live PendingAdoption exists` (set exactly at
ForkChoice::Switch; cleared at rollback completion, at adoption refusal, and
by the 300s TTL — `pending_adoption_live` expires an out-of-TTL record exactly
like `pending_adoption_for` does). While reorg_pending: the pending queue
holds ONLY the selected branch's bodies (the Switch arm clears the queue via
the item-28 cancel-on-supersede before re-queuing just the branch), so nothing
in it is canonically dead by height — ONLY the in_chain arm may drop. Without
a live pending record the gate is byte-identical to item 35. Bounded either
way: TTL 300s < give-up cap ~15min, so a permanently unservable branch body
falls back to the item-35 drop behavior when the record expires.
Lock-ordering safety of the new call-site read: the dispatch loop already
holds `st_shared` when it calls `pending_adoption_live` (PENDING_ADOPTION
mutex); no code path takes PENDING_ADOPTION and then st_shared
(set/clear/pending_adoption_for/pending_adoption_live touch only their own
mutex), so the existing order st_shared → PENDING_ADOPTION is preserved.

## Tests (all run on the shipping bytes, this exact worktree)
- NEW `deferred_reorg_suspends_the_height_arm_of_the_stale_gate`: the pure
  predicate — with reorg_pending=true the at-tip fork body (the DNS3 slot
  h2215652) and above-tip children dispatch; already-in-chain still drops;
  reorg_pending=false keeps every item-35 verdict.
- NEW `deferred_reorg_fork_body_at_tip_dispatches_and_the_rollback_completes`:
  the full DNS3 replay — chain with a losing tip at h105, a 3-block heavier
  branch forking at h104 (first body at h105 == tip height, different hash),
  recon_intake → Switched{fork:104, adopted:3} with the tip untouched; the
  queue head is the fork body AT the tip height; the fixed gate dispatches it
  (the pre-fix gate verdict asserted for contrast); the body validates against
  the fork ancestor (the main loop's exact live:false probe shape),
  rollback_tip completes to h104, the body advances the tip onto the branch,
  the pending record is consumed, and the children drain h106→h107 with no
  stale-drop. Pinned with PoW-mined bodies (mk_body_block) through the full
  validate_block gate.
- UPDATED item-35 test `pending_entry_stale_below_tip_or_already_in_chain`:
  identical assertions with reorg_pending=false.
- Suite results on the shipping bytes: `cargo test --lib` = 871 passed,
  0 failed, 1 ignored (baseline 869/0/1 + the 2 new tests); integration
  cli_exit 5 + consensus_wallet 15 + legacy 24 + spend_corpus 2, all green.

## Rubric
Grade the change for: correctness of the gate semantics (does suspending ONLY
the height arm during a pending reorg reintroduce any of the poisoning the
item-35 fix removed? is the no-pending path truly byte-identical?), the
bounded-rotation claim (TTL vs give-up interplay), lock ordering, test
coverage of the incident shape, and any missed interaction with the item-35
give-up/demote/stall machinery in the same dispatch path.

## Answer format
A single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
