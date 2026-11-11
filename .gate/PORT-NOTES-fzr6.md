# PORT-NOTES-fzr6 — the five fz-rec fixes grafted onto master 457a702 (item-23 adoption-body pipeline)

Method: worktree `/root/tri7-lanes/fzrec` (branch `lane/fz-rec-adoption`) reset
to 457a702; each fix ported from the item-22 reference (commit `15fd2a2`, the
graded r6 tree; also `.gate/lane.diff` + `PORT-NOTES.md` inside its wip commit
`1e4d866`). Conflict rule applied everywhere: item-23's pipeline structures
WIN where the implementations overlap. Single file touched:
`src/node_embedded.rs`. Tests 731 → 739 (+8 ported fns).

Baseline verified before porting: git grep at 457a702 for walk_empty /
gapfill_range / GAPFILL_WINDOW / ADOPTION STALLED / FETCHERS LAGGING / BODY
SOURCE DEMOTED / ADVERTISER_FAIL_LIMIT → all absent (no duplicates).

## Fix 1 — walk_empty_verdict (empty locator reply from a peer claiming ahead = loud Err)
- `fn walk_empty_verdict` after `requeue_unsent_batch` (src/node_embedded.rs:1090
  post-edit numbering; pure, testable).
- Called in `sync_from_zero` at the `all_headers.is_empty()` branch (was a
  silent `return Ok(())  // nothing to do — at tip`): `walk_empty_verdict(
  peer_tip, st.tip_height)?` then `return Ok(())` for the genuinely-at-tip
  case. The Err propagates to the boot reconnect loop, which re-dials.
- Interaction with item-23: none (the boot walk is disjoint from the
  follow-loop body pipeline).

## Fix 2 — tip_poll_locator deepest-first
- `tip_poll_locator` parity branch now `vec![stg.tip_hash, prev_hash]`
  (function at src/node_embedded.rs:1187 post-edit). Genesis h0 keeps the
  tip-only form (underflow guard untouched).
- Interaction: the recon claim-divergence/empty-reply triggers in
  handle_message consume these replies unchanged; deepest-first means parity
  replies chain at the tip instead of re-serving our own tip header.
- Tests: new `tip_poll_locator_orders_deepest_first` + the two-hash
  assertions inside `tip_poll_locator_at_genesis_tip_never_underflows`
  updated (loc[0] = tip, loc[1] = h1).

## Fix 3 — reconnect de-pinned from tip_height
- Boot reconnect loop in `main_args` (from-zero Err arm):
  `let start = attempts as usize` replaces
  `let start = st.tip_height.min(usize::MAX as u64) as usize` — `start` is
  the dial-order OFFSET in connect_and_handshake, so each attempt meets a
  different peer. The walk-lie Err (fix 1) is what lands here.
- Interaction: none.

## Fix 4 — GAPFILL_WINDOW=50 multi-height gap-fill + worker dial rotation + FETCHERS LAGGING
- `const GAPFILL_WINDOW: usize = 50` in the consts block (after
  BODY_TRIES_WASHOUT).
- `fn gapfill_range(len, first_h, next_h, window) -> (usize, usize)` next to
  walk_empty_verdict (window covers next_h FIRST, clamps at the walk end).
- `sync_from_zero`: windowed gap-fill request — `gapfill_range` slice of the
  walked `hashes`, one getdata for the whole window, replies matched via a
  `want_h: HashMap<Hash, u64>` (any window body accepted, inserted at its own
  height); one loud `from-zero: gap-fill recovered hA..hB (N body(s))
  directly from a peer` span line per recovered window (replaces the
  per-height line). Bodies are still hash-verified against their walked
  headers at consumption (reassembly_pop → validate).
- `let mut last_worker_health` timer (init backdated 60s like last_gapfill);
  fail-loud `from-zero: FETCHERS LAGGING at hN — validator starved Ns;
  cursor X claimed, Y validated, Z buffered, W orphan range(s) pending
  re-fetch, A/B worker thread(s) alive — no validated body has landed in
  this span; gap-fill is covering recovery while the worker sessions
  deliver` after the gap-fill block (60s stall / 60s cadence, exact wording
  per spec).
- `fetch_worker`: its `connect` closure now dials
  `connect_and_handshake(&cfg_for_worker, stats, start_height, worker_id)`
  (was hardcoded 0) — each worker starts its dial rotation at its own index.
- Interaction: none (phase-B body crawl is disjoint from the follow-loop
  recon machinery).

## Fix 5 — demotion windows + cooldown probes (grafted onto item-23's pipeline)
- Consts: `ADVERTISER_FAIL_LIMIT: u32 = 2`, `DEMOTION_COOLDOWN_SECS: u64 =
  120` (next to the item-23 body consts).
- `fn credit_body_miss_round(aged, body_tries, peer_misses, demoted_at,
  adv_fail_limit, cooldown, timeout, now) -> Vec<String>`: ONE miss per label
  per age-out ROUND (labels deduped across the aged set, read from THEIR
  `BodyTries.tried`); crossing the limit arms `demoted_at[label] = now` and
  emits the `BODY SOURCE DEMOTED for 120s: <label> accepted getdata but
  served nothing for N round(s) (45s each) — it re-enters selection after
  the cooldown with a fresh probe budget` line.
- `fn expire_body_demotions(peer_misses, demoted_at, now, cooldown)`: windows
  older than the cooldown expire and the peer's round budget resets (fresh
  probe; failing again re-arms — no all-demoted deadlock).
- `choose_body_source_label` / `resolve_body_send_target` gained a
  `demoted_at: &HashMap<String, Instant>` param; eligibility = not in
  `avoid` AND not under an active window, in every tier INCLUDING the
  exhaustion fallback (a demoted advertiser leaves the fallback ladder too).
  All-demoted → None → item-23's no-target path requeues ("no live peer can
  take N body request(s) — requeued (no pin)") until expiry unblocks.
  The 13 pre-existing item-23 test call sites updated with `&HashMap::new()`
  (behavior identical at empty).
- Serving restores immediately: main loop builds
  `body_tries_before_pump: HashMap<Hash, Vec<String>>` (tried labels per
  in-flight hash) right before the message pump and calls
  `fn clear_served_peer_misses(tried_before, outstanding_after,
  peer_misses, demoted_at)` after it — hashes REMOVED from outstanding by
  the pump ARRIVED (validated); every tried label's counter + window clears.
  A kept out-of-order request (refresh_body_request_clock) stays in
  outstanding → no false clear; a failed cached-reinjection likewise keeps
  the hash (item-23 r6 refresh behavior) → no false clear.
- Main-loop wiring: `body_peer_misses`, `body_demoted_at` declared with the
  other pipeline state; per tick: age-out → expire_body_demotions →
  credit_body_miss_round (logs) → dispatch (resolve gains `&body_demoted_at`).
- Deliberately NOT ported from the reference: `sweep_stale_outstanding`,
  `try_dispatch_getdata`, `queue_getdata_batch`, `getdata_misses` per-hash
  rounds, the `rot` fallback-ladder rotation — all superseded by item-23's
  45s age-out + group_batch_by_history + per-hash avoid rotation (their
  tested structures win; the task's conflict rules).

## Fix 6 — tip-frozen stall summary (complementary to item-23's per-body STALL)
- `struct AdoptionStall { last_tip, progress_at, logged_at }` + pure
  `fn adoption_stall_tick(...)` after clear_served_peer_misses; const
  `ADOPTION_STALL_LOG_SECS: u64 = 60`.
- Main loop: `let mut adoption_stall` + `last_body_getdata:
  Option<(String, Instant)>` (recorded on each successful body-group send in
  the dispatch's Ok arm); per tick, after the dispatch block and BEFORE the
  pump: queued OR outstanding-only window → oldest outstanding age from
  body_tries.sent_at, outbound label list, `adoption_stall_tick` → log the
  `ADOPTION STALLED: ...` line; else-branch resets the tracker on the live
  tip. Oldest-age reads body_tries (master's outstanding is Hash→height; the
  reference carried (height, Instant, label) — not ported, item-23's split
  maps win).

## item-23 interactions / invariants kept
- 45s age-out, chain-order requeue, per-history group dispatch, cached-
  successor reinjection, refresh_body_request_clock keep-alive, send-failure
  no-pin requeue, MAX_BODY_ATTEMPTS=6 `adoption body STALL` line, `stats.
  adoption_body_stalls`, BODY_TRIES_WASHOUT=12: ALL kept verbatim.
- The demotion graft reads THEIR BodyTries.tried labels — no new pipeline
  bookkeeping invented.
- item-21 claim-tip/advertiser-first semantics: advertiser preference intact
  for clean peers; demotion only excludes peers that PROVED non-serving.
- Test signatures: `choose_body_source_label`/`resolve_body_send_target`
  gained the demoted_at param; item-23's 8 tests kept (empty-map call sites),
  12 older wiring tests untouched.

## Test landings (all in mod tests, after failed_reinjection_...)
- tip_poll_locator_orders_deepest_first
- from_zero_empty_walk_when_peer_claims_ahead_is_fatal_not_at_tip
- gapfill_window_covers_next_h_first_and_clamps_at_the_walk_end
- adoption_stall_fires_on_outstanding_only_window
- advertiser_yields_to_rotation_while_demotion_is_active
- demotion_arms_expires_and_recovers_via_body_tries_labels
- send_failure_records_no_dispatch_via_their_pipeline
- mk_body_block + recon_adoption_of_1999_block_branch_persists_tip_and_does_not_refire

## Known-untouched (deliberate)
- The stale-link gate dropping a still-delivering session (idle 90s while
  delivering) — OPEN inconsistency, needs its own lane (per item-22's notes).
