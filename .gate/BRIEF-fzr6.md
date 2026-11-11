# BRIEF — fz-rec r6: the five from-zero rescue fixes ported onto item-23's adoption-body pipeline

Base: master `457a702` (item-23 convergence-blocker fix, judge r7 B). Lane:
`lane/fz-rec-adoption` worktree `/root/tri7-lanes/fzrec`. UNCOMMITTED — handed
back for grading, no commit/push/judge/deploy.

One file: `src/node_embedded.rs`. No consensus changes, no new deps, no
behavior change outside the from-zero boot/walk paths and the follow-loop
body-request pipeline.

## What landed (the five fixes + the complementary stall summary)

1. **WALK LIE** — `walk_empty_verdict(peer_tip, our_tip)` (pure): an empty
   locator reply from a peer claiming a taller start height is a LOUD Err
   ("... served NOTHING after our locator ... reconnecting to another peer"),
   driving the boot reconnect loop; fix 3's per-attempt dial rotation then
   meets a peer that CAN serve the anchor. The old silent
   `return Ok(())  // at tip` skipped the entire bulk phase (laptop
   2026-11-11 from-zero boot).
2. **DEEPEST-FIRST tip_poll_locator** — the odd-parity two-hash form flipped
   to `[tip, prev]` (genesis h0 keeps the tip-only form; the genesis test now
   asserts both slots). collect_headers anchors at the FIRST match, so parity
   replies now chain at the real tip instead of re-serving our own tip header
   (which fed the recon judge fork==tip with our_work=0 → re-adopt-own-branch
   churn, laptop h64).
3. **RECONNECT DE-PIN** — the boot reconnect dial offset is the attempt
   counter (`let start = attempts as usize`), not tip_height (a tall tip used
   to pin every reconnect to the same dead first peer).
4. **GAPFILL_WINDOW=50** — phase-B gap-fill requests a WINDOW of consecutive
   heights (pure `gapfill_range`: window covers next_h FIRST, clamps at the
   walk end), reply matched through a want_h map, one loud
   "gap-fill recovered hA..hB (N body(s))" span line per recovered window;
   `fetch_worker` dials its rotation at its own worker index (the literal 0
   put all workers in lockstep on one store-order peer); fail-loud
   "from-zero: FETCHERS LAGGING at hN — validator starved Ns; cursor X
   claimed, Y validated, Z buffered, W orphan range(s) pending re-fetch,
   A/B worker thread(s) alive — no validated body has landed in this span;
   gap-fill is covering recovery while the worker sessions deliver" (60s
   stall / 60s cadence, exact wording per spec).
5. **DEMOTION WINDOWS + COOLDOWN PROBES** (grafted onto item-23's pipeline —
   their structures win): each body age-out round credits ONE miss to every
   label in the aged requests' `BodyTries.tried` (per-ROUND, not per-hash);
   crossing `ADVERTISER_FAIL_LIMIT=2` arms a window
   `body_demoted_at[label]=now` with a named
   `BODY SOURCE DEMOTED for 120s: <label> accepted getdata but served nothing
   for 2 round(s) (45s each) — it re-enters selection after the cooldown with
   a fresh probe budget` line; `choose_body_source_label` /
   `resolve_body_send_target` gained the `demoted_at` param and skip labels
   in `avoid` OR under an active window in EVERY tier (exhaustion fallback
   included — an all-demoted set yields None and the batch stays queued);
   windows older than `DEMOTION_COOLDOWN_SECS=120` expire each tick and the
   peer's round budget resets (fresh probe; failing again re-arms — no
   all-demoted deadlock); serving restores immediately: the main loop
   snapshots `body_tries` tried-labels before the message pump and runs
   `clear_served_peer_misses` after it (hashes REMOVED from outstanding
   ARRIVED → every tried label's counter + window clears).
6. **TIP-FROZEN STALL SUMMARY** — `AdoptionStall{last_tip, progress_at,
   logged_at}` + pure `adoption_stall_tick` firing
   `ADOPTION STALLED: N block(s) queued, M getdata outstanding, tip FROZEN at
   hT for Ss (oldest request Xs ago, last getdata to "L", outbound: [...]) —
   reconciliation cannot advance the tip until a peer serves these bodies`
   on a queued OR outstanding-only window when the tip is frozen >=
   `ADOPTION_STALL_LOG_SECS=60`, rate-limited, reset on tip movement.
   Complementary to item-23's per-body `adoption body STALL` at
   MAX_BODY_ATTEMPTS=6 (kept verbatim, `stats.adoption_body_stalls` kept).

## Conflict resolution (item-23 base wins)

- item-23's 45s `age_out_unanswered_bodies` + `requeue_unsent_batch` +
  `group_batch_by_history` + never-pin-on-unsent dispatch REPLACED the
  reference diff's 20s `sweep_stale_outstanding`, `try_dispatch_getdata`,
  and `queue_getdata_batch` — none of those were ported. The per-history
  group dispatch keeps its per-group `avoid` lists; demotion composes with
  them (a tried AND demoted peer is excluded either way).
- item-23's send-failure requeue kept verbatim (no pin on failed send).
- `rot` (per-batch fallback rotation from the reference) NOT ported: item-23
  rotates via per-hash avoid lists instead; the exhaustion fallback (retry
  advertiser → first live) is item-23's tested no-starvation semantics,
  now bounded by demotion windows (all-demoted → None → queued until expiry).
- `Stats.adoption_body_stalls` kept; no new Stats fields.

## Tests

Base 457a702 = 731 passed / 0 failed / 1 ignored (item-23's 8 new test fns
all kept, updated only where signatures gained the `demoted_at` param —
empty map = identical behavior). Ported/adapted from the item-22 reference
(15fd2a2):

- `tip_poll_locator_orders_deepest_first` (fails on base: locator[0] is prev)
- `from_zero_empty_walk_when_peer_claims_ahead_is_fatal_not_at_tip`
- `gapfill_window_covers_next_h_first_and_clamps_at_the_walk_end`
- `adoption_stall_fires_on_outstanding_only_window`
- `advertiser_yields_to_rotation_while_demotion_is_active`
- `demotion_arms_expires_and_recovers_via_body_tries_labels` (arm → refuse →
  expire → fresh probe → serve-restores, over the production helpers)
- `send_failure_records_no_dispatch_via_their_pipeline` (socket-level, item-23
  never-pin sequence)
- `recon_adoption_of_1999_block_branch_persists_tip_and_does_not_refire`
  (+ `mk_body_block` helper; drains 1999 real validated bodies to h2063,
  identical reply re-judges Ignored with reorgs==1)

## Gates

CARGO_TARGET_DIR=/root/tri7-lanes/target-fzrec: `cargo test --lib`,
`cargo test --release --lib`, `cargo check --bin tri7_wallet_gui --features
gui`, `cargo build --release --target x86_64-pc-windows-gnu --bins` — raw
output in `.gate/full-gates.log`. HARD STOPS respected: no commit, no push,
no judge, no deployment, master worktree untouched, no live-node work.
