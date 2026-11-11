# TRI7 workqueue item 19 — EVIDENCE-EQUAL-HEIGHT (empty-reply fork evidence, streak trigger)

Base: master 109db1dc45f2414f26a4ed0d7e65309e17cb6cc8 (clean; the diff below is the whole change).
Kind: code change, src/bin/follow_tip.rs only (the fleet follow/daemon binary).

## Live defect being fixed (fleet catch 2026-11-10 05:3xZ, sami-pc = wallet home)

The item-18 empty-reply fork-evidence trigger fires only when the peer's
`start_height` is ABOVE our tip. `start_height` is frozen at HANDSHAKE, so a
follower that handshook at EQUAL height and later forked never satisfies the
comparison and sits silent forever: sami-pc sat 25+ minutes on a 9-block solo
fork (tip 4f8a4ec6@2208769 vs canonical 714fa4fc@2208769, fork point h2208760)
while its peer session handshook at 2208769 == 2208769. The item-18 machinery
was structurally unable to fire on that shape; the operator had to restart the
daemon so a fresh handshake carried a higher start height.

## The change (diff.patch)

1. `EMPTY_REPLY_EVIDENCE_STREAK: u32 = 3` — N consecutive empty tip-poll
   replies are fork evidence on their own. At POLL_SECS=30 this fires ~90 s
   into a silent-fork shape.
2. `ReconAcc.empty_tip_poll_streak` — consecutive-empty counter; reset to 0 on
   any non-empty reply and on every fired re-ask.
3. `empty_reply_fork_recon_due(peer_start_height, tip_height, empty_streak,
   awaiting_window_reply, last_request)` — TWO independent triggers:
   (a) item-18 fast path preserved verbatim: start_height above our tip fires
       on the FIRST empty reply (streak value irrelevant);
   (b) item-19 streak path: `empty_streak >= N` fires REGARDLESS of the
       start_height comparison (equal OR below — both are handshake-frozen
       values that must not suppress the robust trigger).
   The two pre-existing guards bind BOTH triggers unchanged: an in-flight
   window re-ask's empty echo never re-triggers (the r5 judge blocker), and
   the 60 s cooldown still gates firing.
4. Handler bookkeeping extracted into `note_reply_empty` / `note_reply_nonempty`
   (echo empties neither count nor reset the streak; non-empty clears both).
   The fire path resets the streak and logs WHICH trigger fired
   ("start-height" vs "streak") so fleet logs diagnose the shape directly.
5. Consistency fix REQUIRED by (4): the two window-locator sends that did not
   mark themselves in-flight — the mixed-batch follow-up and the unknown-inv
   follow-up — now set `awaiting_window_reply = true`. Before item 19 their
   empty echoes were inert (the height gate swallowed them); with a streak
   counter they would count toward evidence and fire spurious duplicate
   re-asks. This aligns them with the documented r5 contract ("SET on every
   window-locator send").

## Tests (src/bin/follow_tip.rs, follow_tip test binary)

- `empty_reply_streak_fires_regardless_of_start_height` — THE item-19
  regression shape: equal-at-handshake (2208769 == 2208769) streaks 1,2 no
  fire, 3 fires (fresh + cooled); peer frozen BELOW our tip also fires at 3;
  cooldown binds the streak path; echo guard binds even with a long banked
  streak; item-18 fast path intact at streak 0/1.
- `empty_reply_streak_bookkeeping_counts_and_resets` — 3 empties climb to 3;
  an echo empty neither counts nor resets; a non-empty reply clears both the
  in-flight flag and the streak; next empty counts from 1.
- `empty_tip_poll_reply_triggers_window_recon_only_as_fork_evidence` — the
  item-18 6-case pin, updated to the new signature (streak 0 column).

## Acceptance evidence in this bundle

- diff.patch — the complete change (no other files touched).
- gate-log.txt — `cargo test --release --lib` + `cargo test --release --bin
  follow_tip` + `cargo build --release --target x86_64-pc-windows-gnu --bins`
  (all-bins Windows gate) outputs, run on exactly this diff.
- Deployment plan (post-grade): commit [grade=X urn] to master → push → CI
  green → deploy DNS2 (systemd swap), DNS3 (scp), Pi (on-device aarch64 build
  from git-archive), sami-pc (CI artifact follow_tip.exe, both bin paths,
  watchdog dance) → fleet battery + four-way parity readback.

## Scope notes

- No protocol change: the trigger only decides when to ASK with the window
  locator; the judge/reorg path below it is untouched (item 17b/18 code).
- The streak is daemon-granular (ReconAcc is per follow loop, shared across
  peers) — same granularity as the existing empty_fork_recon_at and
  awaiting_window_reply state; a healthy peer's non-empty reply resets the
  streak, which is the correct cross-peer behavior (evidence only accumulates
  while NO peer answers).
