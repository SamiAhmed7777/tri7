# BRIEF — Stop tri7d-health.timer on DNS2 (reversible ops intervention)

**Date:** 2026-11-13 ~17:1xZ · **Host:** DNS2 (vmi3080415) · **Lane:** tri7 continuity (queue retired)
**Proposed action:** `systemctl stop tri7d-health.timer` — NO disable, NO mask, NO unit edit. Fully reversible with `systemctl start tri7d-health.timer`.

## The defect (measured)

`tri7d-health.timer` fires `*:0/5` (every 5 min). Its check `follow_tip --status --max-write-age 600`
exits rc=3, and the unit then runs `systemctl try-restart tri7d.service`. Journal evidence
(evidence-health-journal.txt): FAILED rc=3 → restart at 16:35, 16:40, 16:45, 16:50, 16:55,
17:00, 17:05, 17:10 — metronomic. tri7d boots: 16:36:32, 16:41:32, 16:46:30, 16:51:32,
16:56:30, 17:01:31, 17:06:30, 17:11:35Z (evidence-tri7d-journal.txt + systemctl show).

**Feedback loop:** each boot needs ~90-105 s block-index build + tipstate load before the
follow loop writes tipstate.json (the 30 s liveness write) and before it can chase peers.
The healthcheck's rc=3 conditions (stale write age > 600 s during warmup; DEGRADED =
no peer traffic > 600 s AND no tip advance > 1800 s) then fire — so the timer kills the
daemon mid-warmup/mid-chase forever. The kill shape is SIGTERM (ignored while busy) →
90 s stop-sigterm timeout → SIGKILL mid-index-build, which also invalidates the index
cache, guaranteeing the NEXT boot also needs the full 105 s rebuild. The loop cannot
exit from inside.

**Why the checks fail at all:** the fleet tor/i2p mesh has been broken since ~14:30Z
(SOCKS resource-unavailable fleet-wide; earlier finding), and DNS2's P2P inbound bind
127.0.0.1:24113 is occupied by the legacy `trianglesd` (0.0.0.0:24113) that an external
lane cut over at 14:30:44Z. So: no peer traffic + frozen tip → DEGRADED → restart →
still no peers → restart … The watchdog is doing exactly what its unit comment says
("frozen+isolated is a failure") but the response (restart) cannot fix an external
mesh outage, and the 5-min cadence actively prevents recovery when peers ARE reachable.

## Impact (measured)

- DNS2 is the fleet's SOLE HEAVY STAKER (zone ~5.29k TRI, compounding). It has not
  minted since its own branch froze at h2211156 (last stake h2211146 at 16:06:25Z per
  the prior tick; nothing since). Seeds Pi + DNS3 sit at h2211147 with no new blocks
  fleet-wide for ~36 min — the heavy staker's return is what resumes block flow.
- tri7d HAS reached real tri7 peers on recent boots — handshake ok to Pi's onion
  ra32xf5og…:24112 (tri7:7.0.0 start h2211147) at 16:48:39Z and 16:56:34Z, and to Pi
  clearnet 46.62.249.20:443 at 16:54:49Z and 16:56:30Z — and was killed 0-60 s after
  each success, before any backfill/chase/reconciliation could run. The item-17/18/19
  reorg machinery needs minutes of uninterrupted runtime; the timer never grants them.
- Every kill is a SIGKILL mid block-index-build (journal: "Killing process … signal
  SIGKILL" at each cycle) — wasted ~105 s rebuild per boot, disk churn on an 89%-full disk.

## Why this is the tri7 lane's own defect, not the contested decision

Operator decision (5) — restore tri7's ports on DNS2 vs bless the legacy stack — is
UNTOUCHED by this change. We do not bind ports, do not touch legacy units, do not
configure peers. We stop OUR OWN watchdog from killing OUR OWN daemon every 5 min.
Precedent: the 2026-11-13 16:0xZ tick already stood down attributing these restarts
to "an external actor"; this tick IDENTIFIES the restarter as tri7d-health.timer
(itself) — the stand-down premise was wrong, and the loop is self-inflicted.

## Plan after the stop (verification bar)

1. `systemctl stop tri7d-health.timer`; confirm `systemctl is-active` = inactive and
   `systemctl list-timers` no longer schedules it.
2. Leave tri7d (pid from 17:11:35Z boot) running uninterrupted.
3. Verify within the tick: RPC 127.0.0.1:19112 answers getblockcount; journal shows
   index ready → tipstate written → a completed handshake to a real tri7 peer →
   tip chase/reconciliation progress (toward Pi's 2211147 or carrying its own heavier
   h2211156 branch via polls); staking attempts resume.
4. Run the healthcheck MANUALLY (`follow_tip --status --state-dir /var/lib/tri7
   --max-write-age 600`). If it exits 0 with the node healthy and peers held, re-arm
   the timer and record that. If it still exits rc=3 because the mesh is externally
   down (DEGRADED), LEAVE THE TIMER STOPPED and disclose: re-enable awaits mesh
   healing or operator decision (5). Re-arm is one command; documented in the brief.
5. All changes reversible: `systemctl start tri7d-health.timer` restores today's
   prior behavior exactly (no unit file edits anywhere).

## Risk analysis

- Worst case if wrong: tri7d wedges with no auto-restart until re-arm — mitigated by
  this tick's own verification (step 3) and by `--forever` in the unit's ExecStart
  (follow_tip already loops/retries internally; systemd Restart=on-failure remains
  armed for actual process exits — only the EXTRA 5-min try-restart churn is stopped).
- No data paths touched: no state-dir writes, no peers.json edits, no wallet ops,
  no legacy-unit ops. Read-only sweep per skill §0 was completed before this brief.
- Rollback: one command, no side effects to undo.
