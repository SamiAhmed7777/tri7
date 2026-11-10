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

## ROUND 2 (same tick, live-caught on the fleet): the locator-anchor guard

Deploying r1 to DNS2+DNS3 exposed the SECOND-ORDER stale-slot defect live:
DNS3 (new binary, seeded from its own fork lineage) still could not converge.
Its journal looped every minute: `recon: served header at h2208650 breaks the
window chain (prev 259ede68 expected a55aa6d3) — judging the chained prefix
only`.

Root cause: the serving peer in DNS3's peer set (Pi, still on the pre-fix
binary) holds DNS3's pushed race blocks in its APPENDIX (height-tagged
runtime appends) AND the canonical chain in its frozen height map. DNS3's
recon locator contains its own branch hashes (a55aa6d3@2208649, child); the
peer's locator resolution is hash-keyed (`store.block_by_hash`) and resolved
a55aa6d3 at height 2208649 — so the serve ANCHORED at 2208649 and served the
peer's own 2208650 block from the height map (prev 259ede68). DNS3's guard
correctly refused to link and the reconciliation never judged.

Fix (same principle, one layer deeper): a locator hash may only anchor a
serve when it is a body OUR LIVE CHAIN actually holds at that height —
`store.chain_slot_hash(hit_height) == Some(hash)` — applied to BOTH the
store hit and the recent-cache hit in `collect_headers`' anchor loop.
Unregistered stores keep the legacy behavior exactly (pinned). A re-scanned
(heightless) raced frame anchors nothing either way (also pinned — pre-change
parity).

Fleet consequence: with the fix deployed on every serving node, a follower
whose locator holds branch hashes gets served from the deepest SHARED chain
anchor — the reply chains into the follower's window, the judge sees the real
fork, and the heavier-branch switch can fire. This is the missing piece for
work item (3) (convergence without a re-copy).

Tests: 557 lib + 76 follow_tip green.
- `stale_locator_body_cannot_anchor_the_serve_past_our_refilled_slot` (new,
  bin): pins the live Pi shape — raced-hash-only locator anchors nothing;
  raced+shared locator serves from the shared ancestor (b3,b4); clean locator
  unchanged; appendix-height raced body still anchors on an UNREGISTERED
  store (legacy); re-scanned heightless raced frame anchors nothing either way.
- `encode_frame` exported from the lib so bin tests build byte-identical
  stores (FRAME_MAGIC was private).

## ROUND 3 verdict: GRADE B zero blockers (urn:ump:yvjjj75cjordoaf5m3vlnfwicnuh3lsg32pswestxk7dj63cedaq).
POLISH TAKEN (the substantive one): raw_body_by_height's UNREGISTERED path is
restored to the exact legacy semantics — height-map frame proven by
hash_at_height, or nothing (the old no-proof-no-serve guard; a map-miss never
served an appendix body). The chain-registered path keeps the height_slot
resolution. Test legs added to
without_a_registered_chain_legacy_resolution_is_untouched:
raw_body_by_height(3) (appendix-only height) → None;
raw_body_by_height(2) (in-map) → the proven body.
Also taken: blank line after encode_frame.
RECORDED (cosmetic): audit seen-set clear-vs-LRU (8192 bound, re-log only);
chain_tip Mutex+slots RwLock two-acquisition window (benign — rollback drops
slots then re-advances re-note them; a torn read serves the legacy body for
one lookup); encode_frame consumers = bin tests + future ops tooling (doc'd).

## ROUND 4 note
Base moved under us (sibling lanes merged lane/drawer + lane/sweep-fix onto
master while this item ran — both graded B, judge urns in their commits); the
lib suite is 562 now (the lanes' +5). diff-r4-final.patch is THIS item's
complete diff vs the new master base; the two previously submitted patches
remain superseded. Suite: 562 lib + 76 follow_tip green on the merged tree.

## ROUND 5 (same tick, the THIRD live-caught layer): the empty-reply fork evidence

With the locator guard deployed on Pi+DNS3+DNS2, DNS3 STILL sat silent at
its fork tip: zero journal lines, only poll/reply traffic. Decode: this
protocol has NO proactive block relay — peers discover tips purely through
getheaders REPLIES (the mint relay log line notwithstanding, the only
block/headers sends are getdata/getheaders answers). DNS3's poll locator
holds only ITS OWN branch hashes ([tip], [prev,tip]); DNS2 (which never held
the raced bodies) resolves nothing and serves an EMPTY headers message — and
the empty-reply handler (item 17b) only judged when a branch was already
accumulating. Silent dead-end, the third way the fleet fork could never
heal.

FIX: an empty reply to a TIP-locator poll while the peer's handshake
start_height is ABOVE our tip is fork evidence — "nothing after your tip" +
"I am N ahead" can only both hold if our tip is not on their chain. The
handler now re-asks with the window locator (60 s cooldown on the per-peer
ReconAcc, so a keep-ours tie cannot re-request every poll); the window
reply chains at the shared ancestor and the judge fires. recon_intake itself
is unchanged and remains covered by the r1-r4 tests.

Tests: 567 lib + 76 follow_tip green (the lanes' sweep tests included).
Deployed on top of r2 on DNS3 (sha e902ffd4…): the convergence acceptance
test (work item 3) runs immediately after — DNS3 must switch to canonical
WITHOUT a store re-copy.

## ROUND 5 verdict: GRADE C (urn:ump:4twr4glvkcjbmh2rfa3qpnlvdf6jfmktzyyc7c5rep76rwvnu3na), both blockers taken:
1. (unpinned headline fix) → the trigger condition is EXTRACTED into
   empty_reply_fork_recon_due(peer_start_height, tip_height,
   awaiting_window_reply, last_request) and pinned by
   empty_tip_poll_reply_triggers_window_recon_only_as_fork_evidence:
   fires on fork evidence (peer above our tip) with nothing in flight and
   the cooldown expired; never when the peer is at/below our tip; never
   while our own window re-ask is in flight; never inside the 60 s cooldown.
2. (empty echo of our own re-ask loops forever on a disjoint peer) → new
   ReconAcc.awaiting_window_reply: SET whenever a window-locator request is
   sent (the fork-evidence path AND the NeedAnchorLocator path), CLEARED the
   moment any non-empty headers reply arrives. An empty reply while in
   flight is "no overlap", not evidence — the trigger never fires on its own
   echo.

## ROUND 6 note
diff-r6-final.patch = the complete delta vs master 3eb9226 (the r2 locator
guard + r5 empty-evidence + r6 blockers), the as-committed-to-be tree.
Suite: 567 lib + 77 follow_tip green; release all-bins green.
