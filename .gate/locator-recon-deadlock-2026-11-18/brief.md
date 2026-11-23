# Item 33 LOCATOR-RECON-DEADLOCK — brief for the judge

Artifact: `src/node_embedded.rs` diff (diff.patch in this bundle). Kind: consensus-adjacent P2P
reconciliation fix on tri7 (Rust). The change is fleet-safety: it makes a multi-block chain
fork self-heal without operator restarts.

## The live defect (found 2026-11-18 13:32-13:58Z, journals quoted verbatim in evidence.txt)

An EQUAL-WORK race at h2214622 (DNS2 `54ad469d` vs DNS3 `8e3cb0cb`, identical work
1191528982848795695906816) opened a multi-block branch divergence that did NOT self-resolve —
the first stuck fleet split (25+ min; every prior race resolved in minutes). DNS2 (heavier,
canonical) solo-climbed to h2214633 while Pi+DNS3 froze at h2214624. The fleet only converged
after the operator restarted both followers (fresh handshakes).

Journal forensics (three hosts) show the deadlock is TWO coupled defects, NOT a serve-side gap:

1. THE LATCH (mutes the follower forever): every empty-reply recon trigger sets
   `awaiting_window_reply=true` and the ONLY code that clears it is a NON-EMPTY reply
   (`note_reply_nonempty`). In the split, the followers' re-asks went to same-tip peers
   (Pi↔DNS3 held one tip; DNS2 was not a live peer of DNS3), and a same-tip peer CORRECTLY
   answers a window-locator request with an empty reply ("nothing after your tip" is literally
   true when both sides sit on one tip). That empty echo was swallowed as "no overlap" and the
   flag never cleared: every LATER empty poll reply was then classified as that one request's
   echo, the streak froze, and the trigger could never fire again. Live proof: Pi's follow loop
   logged NOTHING from 13:34:21Z to its 14:02:31Z restart; DNS3 nothing from 13:38:32Z to its
   13:58:58Z restart — ~60s-cadence loops, 17-28 min of zero lines. DNS2 by contrast kept
   firing every 60s because its re-asks were answered NON-EMPTY (its followers serve their
   stale 3-block branch), so its flag cleared each cycle — "NOT heavier — ignored, reconciliation
   closed" (arithmetically correct: its branch was heavier).

2. THE MISAIMED RE-ASK (the branch never flowed): the evidence trigger sends the window-locator
   re-ask to exactly ONE peer — the peer whose empty reply crossed the streak. With all
   same-tip peers answering empty, the heavier branch one hop away (DNS2, which answers a
   stride locator with a full branch serve — proven by DNS2's own judges and by both
   post-restart boots adopting DNS2's chain through exactly this reply path) was never asked.

Contributing: the tip-POLL locator carried only [tip] / [tip, prev] — both entries sit on OUR
side of any fork, so a forked peer can never anchor them and every poll reply is empty even
though the peer holds a full branch it could serve from a deeper shared anchor
(collect_headers already resolves arbitrary stride anchors — unchanged and proven by the
existing stale-locator test).

## The fix (three coordinated, minimal changes)

1. STRIDE POLL LOCATOR (`tip_poll_locator`): after [tip] (+ the parity prev, unchanged order —
   the fz-rec fix 2 deepest-first invariant holds: locator[0] is the tip) append the SAME
   window stride `build_recon_locator` uses (every RECON_LOCATOR_STEP=128th entry down to the
   base, capped at RECON_LOCATOR_MAX=40). Every poll is now a recon ask: a forked peer resolves
   a shared anchor DIRECTLY FROM THE POLL and serves its branch in the reply — the judge adopts
   it in ONE poll cadence with no re-ask round-trip. Normal (non-forked) polls are
   byte-identical in effect: the first match still anchors at the tip.
2. TIME-BOXED LATCH (`window_reask_at` + `awaiting_window_reask`): the in-flight flag is
   honored only for WINDOW_REASK_TIMEOUT=90s after the send (`mark_window_reask` stamps all
   three send sites). After expiry the empty replies count toward the streak again and the
   trigger re-fires (60s cooldown still applies) — the loop re-arms WITHOUT a restart, at a
   calm ~90s pace. r5 pins preserved: a fresh echo is still not evidence, and an UNSTAMPED
   legacy flag still latches (the exact pre-item-33 semantics).
3. BROADCAST RE-ASK (`reask_broadcast_skip` + main-loop pass): when the evidence trigger fires,
   the re-ask still goes to the triggering peer immediately, and the main loop sends the SAME
   locator to every OTHER live peer right after the dispatch pass (send failures drop the peer
   exactly like the poll tick's). The divergent heavier peer gets asked within the same pass
   even when the triggering peer answers empty.

Also verified from the journals and left UNCHANGED (already correct):
- `claim_divergence_verdict`'s below/equal-tip arm (`Some(e) => Some(e.hash != chash)`) — a
  frozen claim below our height with a different hash already returns divergence.
- The serve side (`collect_headers`) — it answers stride-anchored requests from the deepest
  shared ancestor (its own chain-slot guard skips raced bodies; the existing
  stale_locator_body test pins exactly the reply the judge needs).
- DNS2's heavier-wins verdicts — arithmetically correct; nothing to fix there.

## Tests (all in the diff; suite numbers below)

- `window_reask_latch_time_boxes_and_re_arms_without_a_restart` — the latch: fresh echo
  swallowed (r5), expiry re-arms the streak + trigger with zero restarts, unstamped legacy
  flag still latches.
- `tip_poll_locator_carries_the_window_stride` — shape: tip first, parity prev second,
  tip-128 stride + base anchor present, cap respected, genesis-safe, deep window hits the cap
  exactly.
- `stride_poll_reply_adopts_the_heavier_split_branch_in_one_exchange` — THE SPLIT REPLAYED at
  the judge: follower window h2214496..2214624 (its 3-block branch above the shared h2214621),
  the heavier side's 9-block 256x-harder branch arrives as the poll reply →
  `Switched { fork: 2214621, adopted: 9 }` in ONE exchange, headers-only (no rollback until a
  body validates, T7-03), pending recorded. Acceptance bar met: heavier branch adopted by the
  lighter side within N poll cadences (N=1 here) with zero manual restarts.
- Updated the two locator-shape tests that pinned the old 2-anchor form
  (`tip_poll_locator_orders_deepest_first`, `tip_poll_locator_at_genesis_tip_never_underflows`)
  for the stride shape; every other recon/claim/serve test is untouched and green.

## Risk bounds

- Poll volume: locator ≤ 40 hashes × 32B ≈ 1.3KB per peer per 30s poll — trivial. Serves to a
  forked/lagging peer are the same ≤2000-header pages from-zero sync already serves in
  production (bounded by collect_headers' cap).
- Consensus untouched: no block/kernel/wallet/validation logic changed — only locator
  construction, one latch timeout, and a re-ask broadcast. Heavier-wins selection, the T7-03
  headers-only deferred rollback, and the r5 disjoint-peer guards are all intact and
  test-pinned.
