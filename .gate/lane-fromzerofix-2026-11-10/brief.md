# Judge brief: tri7 from-zero fresh-install sync fix (2 defects, live-caught)

## THE SYMPTOM
A brand-new install syncing from genesis died silently: on sami-laptop the
node's log showed the from-zero start then nothing (the supervisor restarted
it until MAX_RESTARTS). Reproduced HERE on Linux against the live fleet: the
node PANICKED ("attempt to subtract with overflow" at node_embedded.rs:10449)
~1s after "syncing h0 →".

## DEFECT 1 — locator underflow (the panic)
The live tip-poll built a 2-hash locator with entry_abs(tip_height - 1); at
tip h0 that is 0u64-1 → panic → the main thread died before a single header
segment could be fetched. EVERY fresh install was dead-on-arrival.
FIX: tip_poll_locator() — at tip 0 the locator is tip-hash-only; the
two-hash form requires tip >= 1. Regression test
tip_poll_locator_at_genesis_tip_never_underflows (h0 both parities + h2).

## DEFECT 2 — serving guard refused below-window anchors
Even unpanicked, the walk starved: collect_headers() anchors a serve ONLY on
locator hashes threaded in the chain-slot window; a fresh install's genesis
locator sits BELOW the window (no slot by construction) → the fleet peers
answered EMPTY ("nothing after this locator") → the walk concluded "at tip"
with zero headers. The fleet nodes were all seeded states, so no one had
ever exercised this path.
FIX: collect_headers() gains a second anchor branch — a STORED body whose
height has NO slot at all and is below the registered tip anchors the serve.
The item-18 race guard is UNTOUCHED for its actual case: a raced body sits
at a height whose slot EXISTS with the live chain's hash — the first guard
still refuses it (the existing test
stale_locator_body_cannot_anchor_the_serve_past_our_refilled_slot passes
unchanged), and a new test
below_window_stored_body_can_anchor_the_serve_for_fresh_installs pins the
new branch (below-window anchor serves the walk; within-window race stays
refused).

## TESTS
655 passed, 0 failed, 1 ignored (653 + 2 new).

## LIVE PROOF (live-fromzero.log, verbatim — this exact run)
Fresh state dir, follow_tip --daemon 46.62.249.20:443 (the Pi seed node,
updated with this build first):
- initialization at h0 (genesis anchored, "syncing h0 → h2209269, 8 fetcher
  sessions")
- NO panic (the pre-fix binary panicked and died ~1s in — defect 1)
- the header WALK progresses: "from-zero: 126000 headers walked (up to
  h126000)" and still climbing at capture (defect 2 fixed: the fleet peer
  now serves the below-window anchor — the pre-fix peer answered EMPTY and
  the walk concluded "at tip" with zero headers)
The claim is the walk phase exactly as logged; the body-fetch phase that
follows was not captured to completion in this bundle (the earlier
h2083→h3606 body-fetch observation came from an intermediate run not
bundled here and is NOT claimed as evidence).
