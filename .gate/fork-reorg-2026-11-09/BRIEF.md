# ITEM 17 FORK-REORG — brief for the GLM judge (2026-11-09)

Repo: /root/tri7 @ master (parent 9723e33). Diff: follow_tip.rs only
(634+/43−). Local: 531 lib + 71 follow_tip green (+7 new tests),
windows-gnu cross-build pending in CI.

## The two defects (both found live on the fleet; queue item 17)

### (a) backfill boot crash-loop
`run_backfill` requested headers from `tip - span` and the peer serves them
UP TO ITS OWN TIP. When the peer was ahead, the served tail sat ABOVE the
local window; the stride body-picks landed there; each such body has no
local prev entry → `backfill block outside window` → exit(4) → systemd
restart → same peer → same abort (6+ cycles observed on DNS3 before the
`--backfill 0` unit drop-in was applied as ops mitigation).

Fix:
- `backfill_pick_indices(got_len, k, tip, start_h)` — extracted pure stride
  picker that CLAMPS picks to heights ≤ the local window tip; run_backfill
  logs the clamp and proceeds (or returns early when zero in-window picks).
- The body handler treats a served body with no local window entry as
  SKIP+WARN (belt for anything the clamp misses) — never fatal.
- span clamped to tip_height (underflow guard for tiny windows); tip==0
  short-circuits (nothing to backfill into).

### (b) heavier-chain reorg in the reconciliation path (the fork wedge root cause)
Before this change there was NO way to resolve a multi-block fork:
- `try_adopt_heavier_branch` (the live-body path) judges ONE body's work vs
  our whole branch above the ancestor — a single fork body can never beat a
  multi-block branch, so the judge always said KeepOurs;
- the "headers" handler, on any announcement that did not chain to our tip,
  re-sent getheaders with a TIP-ONLY locator. A peer that never saw our tip
  (a fork we hold but it does not) answers from genesis / answers nothing →
  the handler re-requested the same locator → infinite loop (the DNS3
  "wedged in a reconciliation loop"; every fleet fork wedge in the tick log
  died exactly here).

Fix (all in the "headers" handler + new helpers):
- `build_recon_locator(st)` — window-spanning locator: tip + every 128th
  entry down to the base (≤40 anchors). A peer that never saw our tip can
  now find an in-window common ancestor and serve the branch from there.
  Applied to ALL three reconciliation send-sites (competing-branch retry,
  mixed-batch retry, unknown-inv reconciliation).
- `recon_intake(...)` — judges a non-chaining batch as a competing branch:
  locates the in-window fork point from the first header's prev; accumulates
  the branch (dedup set); judges when the batch is complete (under the 2000
  daemon serving cap) OR their accumulated work already beats ours (work
  only grows — early judge on continuation batches); compares TOTAL branch
  work vs our work above the fork via the existing
  `tri7_core::forkchoice::choose` (same rules as the legacy
  CBlockIndex::nChainWork comparison). Strictly heavier → `rollback_tip`
  down to the fork, their headers queued into pending_headers +
  pending_from_headers with absolute heights → bodies flow through the
  NORMAL getdata → validate_block (full consensus incl. PoS kernel) path —
  a header-work switch never advances the chain past an unvalidated body.
  Lighter/tie → Ignored and the accumulator CLOSES (the loop ends — the
  anti-wedge property).
- No in-window fork point → one retry with the anchor locator; still
  unresolved after RECON_MAX_UNRESOLVED (2) tries → drop the peer
  (supervisor reconnects) instead of spinning forever.
- `intake_header_batch` — the chaining walk refactored to a running
  expected-prev: multi-block announcements no longer fire a spurious
  reconciliation on the second header; returns true when a header was
  unlinked mid-batch (caller reconciles with the window locator).

## Honest deviations / notes for the judge
- The accumulator is process-global, not per-peer: a batch from a different
  peer whose first prev ≠ the accumulated last hash simply starts a fresh
  branch (reset). Fleet reality is 1-2 outbound peers.
- recon_intake logs via the bin logger (stdout/journal); tests print 2 log
  lines — cosmetic.
- `try_adopt_heavier_branch` (single-body judge) is intentionally UNCHANGED:
  it is still correct for the rare pushed-fork-body case; the header-batch
  judge now covers the branch case that made it useless.
- Post-reorg wallet-live-view drift is a KNOWN pre-existing artifact
  (item-9 log: rollback does not roll the wallet live map; repairwallet
  heals). Out of scope here; unchanged behavior.
- Disk store keeps rolled-back branch bodies (append-only; both branches'
  bodies coexist keyed by hash) — same pattern the proven single-block fork
  switch has used since the forkchoice commit.

## Tests (+7)
- backfill_pick_indices_clamp_to_the_local_window_tip (the crash-loop shape:
  3100 served, picks clamped, none above the tip)
- recon_heavier_branch_switches_and_queues_the_validated_body_path
- recon_lighter_branch_is_ignored_and_the_reconciliation_closes (incl. tie)
- recon_unknown_fork_point_escalates_then_drops_the_peer
- recon_accumulates_cap_sized_batches_before_judging (2000-cap continuation
  + early work-triggered judge; 65536x-easy bits so 2000 < our 4 blocks)
- recon_locator_spans_the_window_down_to_the_base
- intake_header_batch_accepts_multi_block_announcements_without_reconciling

## Judge rounds (GLM stand-in; Codex quota-walled)
- r1 B zero blockers urn:ump:gohjpa2wq6sil2vvaun7fvi6ysi2rmim75kuhd5vsv3bpctlzx6q
  — polish taken: judged_work captured before reset (KeepOurs log printed 0);
  continuation anchor = last ADDED header (duplicate replies cannot move it
  backwards); empty reply judges+closes an exactly-cap-sized branch (shared
  recon_dispatch helper).
- r2 B zero blockers urn:ump:ewk24kipmpsroowjv2hixlf3rz2etzcq5gvcuuyghkrzwfxuh7sq
  — polish taken: KeepOurs log uses the real fork height; Switch-path
  rollback failure ABORTS the adoption (no queued pendings over a partially
  rolled window — the branch re-arrives and re-judges); recon.unresolved
  resets on every close; stats double-count comment.
  r2 note (per-peer accumulator) recorded for a future tick — the disclosed
  global-reset-on-mismatch compromise stands (fleet runs 1-2 outbound peers).
- r3 B zero blockers FINAL urn:ump:75sgeyn4a2vd5m6som4uxqbpaex2vqwddsnzcykzz2f62xlvka4a
  — 3 polish recorded, none blocking: (1) early work-triggered judge on
  >2000-header forks can cycle unresolved→retry (correct outcomes, chatty;
  fleet forks are ≤600 blocks — revisit if a >2000-block fork ever appears);
  (2) continuation-of-in-flight-batch routing — ALREADY satisfied (the gate
  sends such batches to recon_intake, which detects the continuation via
  last_hash); (3) headers_processed counts duplicates (cosmetic).

## Deploy/exercise plan (post-judge, same tick)
Push → CI green → deploy DNS2 + DNS3 + Pi + sami-pc → remove the DNS3
`--backfill 0` drop-in (Pi's too) → restart DNS3 while DNS2 is ahead →
prove the boot completes into the follow loop with no exit(4) (the (a)
live proof) → fork-switch exercised by pointing DNS3 at its own stale
tipstate is NOT attempted on live money infra; the switch path is proven
by the unit tests + the DNS2-anchored reconciliation smoke (headers batch
from DNS2 while DNS3 follows). Then (c): re-enable staking on Pi + DNS3
via the persisted conf/RPC, verify attempts advance and four-way tip
parity holds. DNS2 stays sole heavy staker; sami-pc staking stays OFF
(persisted) per the item-15 end-state.
