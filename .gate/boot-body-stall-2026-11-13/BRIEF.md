# BRIEF — boot-body-stall (2026-11-13): boot backfill's body phase must never exit(4) on a peer fault

## Change

One file: `src/node_embedded.rs`. (a) The body phase of `run_backfill` is
extracted verbatim into `backfill_fetch_bodies(...)` taking an injectable
deadline; (b) peer-fault classes inside that phase no longer propagate as
fatal errors — they WARN + skip the phase and return the blocks validated so
far; (c) the `validated == 0` deadline stall returns Ok(0) (caller WARN+skips
backfill) instead of the fatal `Err("backfill: no blocks validated")` that
became `finish(EXIT_PROTOCOL)` = exit(4). A new const `BACKFILL_BODY_DEADLINE`
(180 s, unchanged value) names the bound. +3 loopback integration tests.

## The live defect (measured, host-labelled)

DNS2 2026-11-13 17:20:14Z boot: `run_backfill` validated 2991 headers, then
received 0/100 requested bodies across the FULL 180 s window → old code:
`Err("backfill: no blocks validated")` → caller at the boot path:
`finish(EXIT_PROTOCOL, "backfill: …")` → clean exit(4) → systemd restart →
re-dial the SAME peer → same stall → crash loop (the exact class item 17
removed from the header phase on 2026-11-09: "any boot while a peer … dies"
→ exit(4) → endless loop, observed 6+ cycles live on DNS3 then). Server side
of the same window: Pi logged `validation failure … send block: Broken pipe
— dropping inbound peer` for DNS2's inbound connection — the serving peer
died after headers. The crash loop also forced a full ~105 s index rebuild
per boot (each kill mid-warmup), which is what tri7d-health.timer's SIGKILL
feedback loop (stopped separately this day, judged B
urn:ump:5ehouyhbodfdpb5744ocrasvfdmbxrn3v7bssxtqeou5p4sevy7a) kept feeding.

## Root cause (code path, exact)

`run_backfill`'s body phase (old lines ~5627-5702):
1. `sess.send("getdata", …)?` — a dead socket propagated before the wait even
   started.
2. `for msg in sess.pump()?` — the serving peer dying mid-phase (EOF →
   `Err("peer closed connection")`, wire garbage → Err) propagated.
3. `Block::read(…)?` / `r.expect_done()?` / `parse_ping(…)?` / the pong
   `sess.send(…)?` — unusable served payload or a failed pong write
   propagated.
4. After the bounded wait: `if validated == 0 { return Err("backfill: no
   blocks validated") }` — the recorded defect: a peer that validated our
   headers but served no bodies by the deadline was treated as a PROTOCOL
   crime and killed the process, and the boot retried the same peer forever.

The caller (single call site, boot path ~11181) turns any Err into
`finish(EXIT_PROTOCOL, …)`. EXIT_PROTOCOL=4 was reachable ONLY through this
function's peer-fault paths after item 17; this change closes the class:
peer serving behavior is transport, never fatal at boot.

## Fault treatment after the change (item-17 precedent: "Never exit(4) from here")

- getdata send failure → WARN + `Ok(0)` (phase skipped).
- pump error (peer death / wire garbage at the pump layer) → WARN +
  `Ok(validated-so-far)`.
- served block fails parse or has trailing bytes → WARN + `Ok(validated)`.
- ping parse failure or pong send failure → WARN + `Ok(validated)`.
- deadline stall with 0 validated → caller logs
  `peer validated N header(s) but 0 bodies by the deadline — skipping the
  body phase (follow loop owns the chain)` and returns Ok(headers): the
  follow loop owns the chain from the persisted tip; gap recovery fetches
  what backfill missed; the peer-rotation paths (send-failure shutdown at
  the tip-poll/keepalive sites, item-18/19 empty-reply evidence + fb6e875's
  escalation dial) own the peer.
- KEPT FATAL (disclosed remainder): a served body that passes parse but
  FAILS kernel validation (`validate_block(…)?`) still exits loudly — that
  is a chain-consistency problem (our state vs their block), not transport;
  it deserves operator eyes, and no live occurrence of that shape exists.

Behavior preserved everywhere else: partial stalls (validated > 0) proceed
exactly as before (fall through, log the count, Ok); the early-exit when all
requested bodies arrive is now spelled `!want.is_empty()` (provably the same
predicate as the old `validated < want.len() + validated`); the "17a"
out-of-window served-body skip, the not-requested-hash skip, the every-10
progress log, and the success log line are byte-identical.

## Tests (+3, all loopback `TcpListener` stubs against the real Session wire)

1. `backfill_body_stall_zero_bodies_returns_ok_zero_never_a_fatal_err` —
   stub accepts and holds the socket, serves nothing, 1 s injected deadline →
   `Ok(0)` (was: fatal Err after 180 s).
2. `backfill_body_peer_death_after_headers_returns_ok_never_a_fatal_err` —
   stub accepts and immediately drops (the Pi broken-pipe shape from the
   client side) → pump error → `Ok(0)`, not Err.
3. `backfill_body_garbage_block_payload_skips_the_phase_never_a_fatal_err` —
   stub serves a wire-valid `block` frame with garbage payload → `Ok(0)` and
   `stats.blocks_received >= 1` (received-and-counted pinned).

Full suite: `cargo test --lib` → 774 passed / 0 failed / 1 ignored (771
pre-existing + 3 new). `cargo build --release --bins` green.

## Honest scope notes for the judge

- The from-zero path's fetches already reconnect+retry (their own
  infrastructure, comments at ~4591); the boot backfill was the last
  peer-fault site that could exit(4) — after this commit EXIT_PROTOCOL=4 is
  reachable only via a kernel-validation failure of a served body.
- The 180 s deadline value is unchanged; only its OUTCOME changed. An ops
  knob to shorten it is a future polish (recorded, not shipped).
- No behavior change outside `run_backfill`'s body phase and its new helper.
  Stats fields untouched (no new counters — the WARN lines carry the
  evidence in the journal, consistent with the item-17 skip lines).
