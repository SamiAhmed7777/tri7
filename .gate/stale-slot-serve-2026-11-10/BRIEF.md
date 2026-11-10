# STALE-SLOT-SERVE — chain-slot overlay (fleet workqueue item 18, work items 1+2 code half)

## The live defect (found 2026-11-10 ~01:2xZ during item 17's staking re-enable)

DNS2 (sole heavy staker) accepted a raced follower block (DNS3's
a55aa6d3@2208649) into its store + height map, then its own chain refilled
that slot height with its own block (259ede68). After that:

- `getblockhash` RPC (threaded lineage) answered 259ede68
- P2P `getheaders` serving (`collect_headers` → `store.block_by_height`)
  served the STALE a55aa6d3 body for the same slot

Any follower whose window crossed that height got a header chain that breaks
mid-batch. DNS3's item-17 linkage guard correctly REFUSES the unconnected
serve — which also means it can never converge onto DNS2: the fleet is
deadlocked (DNS3 fork-frozen at 2208650, staking off everywhere but DNS2).

## Root cause (two layers, one principle)

Height→body resolution never consults the LIVE chain:

1. **Appendix first-arrival** (`block_by_height`/`hash_at_height` appendix
   scan): the FIRST appended frame carrying `height == Some(h)` wins — a
   raced body that arrived before our own chain's block shadows it forever.
2. **Frozen build-time height map** (`height_off`): threaded once at index
   build; a reorg that switches the chain in-window (the PC's 81-block judge
   switch at 01:05:07Z) leaves the map on the abandoned branch.
3. **`recent.at_height` fallback** in `collect_headers`: "latest arrival
   wins" — after a LOST race that is exactly the body our chain does not
   contain.
4. **No tip cap**: bodies stored ABOVE our tip (raced pushes) were servable
   as if they were our chain.

## The fix — the store serves the chain, never a heuristic

`src/blockindex.rs` (lib):

- `BlockStore` gains a **chain-slot overlay**: `chain_slots: height → Hash`
  (+ `chain_tip` = highest registered slot), written ONLY by the follow loop:
  - `set_chain_slot(h, hash)` — boot seed (whole tipstate window = the
    threaded lineage) + every tip advance;
  - `drop_chain_slots_above(new_tip)` — rollback (abandoned branch heights
    serve NOTHING until the adopted branch re-fills them);
  - `drop_chain_slots_below(new_base)` — window slide (bounded tail).
- New private `height_slot()` resolution used by ALL three height lookups
  (`block_by_height`, `hash_at_height`, `raw_body_by_height`):
  1. nothing above the registered chain tip (guard);
  2. overlay hit → if the legacy resolution agrees, serve it (fast path);
     if it disagrees, fetch the CHAIN's body BY HASH (new
     `slot_body_by_hash`: main candidates + full appendix scan, since the
     by-key map holds only the first arrival) and count + log the divergence
     once per height: `[blockidx] STALE SLOT h{N}: height map/appendix serves
     {hash} but the live chain slot is {hash}` — this log IS the fleet's
     stale-slot audit (work item 2: no store copy needed, every slot the
     legacy map would have mis-served shows in the journal);
  3. no overlay (read-only consumers: GUI explorer, tx prevout lookup) →
     legacy semantics EXACTLY as before (pinned by test).
- `hash_at_height` answers the overlay hash directly when the body is absent
  — getblockhash can never contradict serving.
- Above-tip guard: `chain_slot_hash(h)` returns None for h > chain tip —
  `collect_headers`' store-miss fallback now serves the CHAIN slot's body
  from the recent cache instead of a blind latest-arrival `at_height`.

`src/bin/follow_tip.rs` (bin):

- process-wide `LIVE_STORE` slot (same pattern as LIVE_ADDRINDEX — the accept
  sites don't thread a store handle through every helper);
- `seed_chain_slots(&st)` at boot, right after the store is created, BEFORE
  any peer can connect: every tipstate window entry registers its slot (this
  is work item 2's "rebuild the height map from the threaded tip", in RAM,
  per boot);
- `advance_with_block` notes `(h, hash)` at the tip advance (all three
  advance sites funnel through it: live push, backfill validate, self-mint);
- `rollback_tip` drops slots above the new tip (all three rollback call
  sites: backfill, recon judge, gap);
- `ChainState::trim` drops the below-base tail.

## Tests (554 lib +4, 75 follow_tip +1, all green locally)

- `chain_slot_overlay_beats_first_arrival_appendix` — THE DNS2 race shape:
  reproduces the defect (documents that the legacy path serves the raced
  body), then proves all three lookups serve the chain body + stale-slot
  counter fires.
- `no_serving_above_the_registered_chain_tip` — post-rollback guard: the map
  still holds the abandoned body, nothing serves above the chain tip;
  re-adoption raises the guard and serving resumes with the chain body.
- `chain_slot_repoints_a_refilled_in_window_slot` — the frozen-map layer:
  branch flipped in-window → the chain's slot wins; window slide bounds the
  tail; tip preserved.
- `without_a_registered_chain_legacy_resolution_is_untouched` — read-only
  consumers keep the exact legacy first-arrival semantics; zero counter.
- `chain_slot_wiring_seed_rollback_and_window_slide_follow_the_threaded_tip`
  (follow_tip) — the real wiring: `seed_chain_slots` + `chain_slot_note` +
  `rollback_tip` + slide against a live BlockStore through the global slot,
  with cleanup so no other test sees it.

## Ops sequence this change unlocks (this tick, after CI green)

1. Deploy to DNS2 first (sole staker, the stale-slot holder): its boot seed
   re-points slot 2208649 to 259ede68 immediately; the journal's STALE SLOT
   lines quantify what the legacy map would have mis-served (audit).
2. DNS3 restarts on the new binary and must converge from its own fork tip
   (a55aa6d3@2208649 + child @2208650) onto canonical WITHOUT a store
   re-copy — the reorg judge's real acceptance test (work item 3).
3. Only then Pi/DNS3 staking re-enable (persisted) and watch a live race
   (work item 4; at difficulty ~2.4e-7 a 100-TRI staker mints within
   minutes — the skill note is work item 5).

## Scope guard

- No consensus change: validation, fork choice, backfill clamps (item 17)
  untouched. This is SERVING truth only — peers are never handed a body the
  serving node's own chain does not contain at that height.
- `frame_offset` (ops trim example) intentionally left on the frozen map.
- The overlay is RAM-only; a restart re-seeds from the tipstate. Read-only
  BlockStore consumers (GUI, prevout lookup) never seed → legacy behavior.
