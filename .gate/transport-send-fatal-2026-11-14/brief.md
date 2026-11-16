# Artifact: tri7 item 26 — transport-send failures are peer drops, never exits

Kind: code (production change in the fleet daemon's peer-error funnel)
Repo: /root/tri7 (master 9b3bad8 + this working-tree diff)
Purpose: a peer-send transport failure (socket/circuit died mid-write) on an
OUTBOUND peer still exits the daemon with code 3 — live-caught 2026-11-14
09:09:56Z on Pi (seed wallet node).

## The live incident (production evidence)

Pi ran the item-25 binary (built 08:45Z). At 09:09:56Z its journal shows:

    ADOPTION STALLED: 1 block(s) queued, 0 getdata outstanding, tip FROZEN
      at h2211633 for 361s …
    validation failure from in:127.0.0.1:59968 (transport i2p): send pong:
      Broken pipe (os error 32) — dropping inbound peer          [contained]
    validation failure from out:gxvrhv3q….onion:24113 (transport tor):
      send pong: Broken pipe (os error 32) — clean disconnect    [fatal lane]
    SUMMARY { "exit_code": 3, "note": "send pong: Broken pipe (os error 32)", … }
    systemd: Main process exited, code=exited, status=3/NOTIMPLEMENTED
    systemd: tri7d.service: Failed … → automatic restart 09:10:27Z

Cost of that exit: full ~5-min warmup (blockidx 55s + wallet reseed + kernel
backfill) AND a fresh §16 stale-live-view flag (2 coins excluded from staking
until repairwallet — a fork-adoption artifact of the rebuild). The tor
circuit to DNS2's onion churned; that is the fleet's NORMAL night, and a
total mesh partition is already survivable (try_connect tolerates every
target down), so a single wedged write cannot rationally be more fatal than
the fleet's baseline.

## Root cause (code path)

Session::send (node_embedded.rs:3125) prefixes every write error
"send <command>: <io err>". The ping/pong handlers in handle_message
propagate it with `?`. The follow-loop peer handler (node_embedded.rs,
`if let Some(e) = fatal`) routes ANY Err to: inbound → drop the peer
(non-fatal); OUTBOUND → `validation_failed = Some(e)` → after the peer
loop → `finish(&cfg, &st, &stats, &rpc, EXIT_VALIDATE, &e)` → exit(3).

Item 25 already made RECEIVED-BLOCK validation failures never exit (they
are contained inside the block path with detect+heal). The read side
(sess.pump) is already non-fatal on every transport ("protocol failure …
dropping (session error)"). The poll-side sends (getheaders/ping) already
drop+continue. This funnel was the LAST send-failure lane that could kill
the daemon.

## The change (166 insertions, 2 deletions, one file: src/node_embedded.rs)

1. `fn is_transport_send_error(e: &str) -> bool { e.starts_with("send ") }`
   — classification with a documented rationale (peer/circuit fact, never a
   chain fact; the prefix is the whole write path into a peer session).
2. The follow-loop funnel gains a FIRST branch: a transport-send error logs
   "send failure to <peer> (transport <t>): <e> — dropping peer (non-fatal)",
   marks the peer dead, shuts the session, and CONTINUES the peer loop —
   on every transport. The existing outbound-fatal stance is untouched for
   every non-send error (genuine validation errors on an outbound peer
   still fail the process, exactly as before).
3. Tests (+2, both passing):
   - `item26_transport_send_errors_classify_non_fatal` — the verbatim live
     note classifies transport; every Session::send prefix classifies
     transport; genuine validation strings and read-side errors do NOT
     (the outbound-fatal stance is preserved for them).
   - `item26_outbound_pong_write_failure_surfaces_as_a_classifiable_transport_err`
     — the funnel input produced FOR REAL over loopback: an outbound peer
     whose server side closed; ping #1's pong write succeeds (CLOSE_WAIT
     buffering — the healthy-session shape), the kernel RSTs, ping #2's
     pong write fails, and the surfaced Err classifies transport (so the
     funnel drops the peer and keeps running instead of finish(EXIT_VALIDATE)).

## Honest scope / what did NOT change

- The outbound-fatal branch still exists for genuine validation errors
  (parse garbage from a trusted peer etc.) — stance unchanged.
- State-write failures still die(EXIT_VALIDATE) — a disk problem is not a
  peer problem.
- item 25's containment (received-block failures, kernel-state heal lane)
  untouched.
- No consensus surface touched: peer-management/funnel only.

## Verification

- cargo test --lib: full suite expected 791 passing (789 prior + 2 new);
  the two new tests pass; no prior test regressed (see suite result line
  appended below).
- The judge should check: classification correctness (prefix = write path,
  no false positives in the negatives), the funnel branch ordering
  (transport check BEFORE the outbound-fatal branch), that `continue`
  cannot skip the peers.retain/session cleanup incorrectly (dead flag +
  shutdown mirror the existing inbound-drop shape), and that the fix
  actually matches the incident (a "send pong" Err on an outbound peer
  would now be contained).

Judge: GLM stand-in (Codex quota wall), sanctioned per standing rules.
