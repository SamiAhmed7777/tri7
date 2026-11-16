# POST-BRIEF ADDENDUM (10:1x-10:3xZ window)

## tri7watch / watchdog6 — ITEM 23'S CHRONIC RESTART SOURCE IDENTIFIED (big find)

- Task `tri7watch` runs `C:\Users\hello\watchdog.bat` → `watchdog6.ps1`:
  "if nothing listens on 19114: kill stray state2 follow_tip instances +
  Start-ScheduledTask 'tri7d2', log to watchdog.log".
- watchdog.log shows it fired at 2026-11-10 23:20:45, 23:29:53, 23:32:14,
  23:36:04, 23:52:09, 23:56:24, 2026-11-11 00:01:07, 00:06:53, 00:12:19,
  00:21:15, 00:42:58, 00:56:18 (PST) = every 5-20 min through the
  07:20-08:56Z window — the exact "~1 boot per 10 min" chronic churn the
  queue's item 23 describes. tri7watch is NOW Disabled (Last Run 08:51Z,
  result 0) — disabled by an external hand, not by this tick.
- The older tri7d-watchdog task (disabled ~06:14Z with tri7d2) is a separate
  v5-era task; watchdog6/tri7watch is the v6 replacement that was still armed.
- With tri7d2 DISABLED, watchdog6's Start-ScheduledTask fails silently every
  fire (v6 does not check enabled state) — harmless but noisy; now defused by
  the task being disabled.

## tri7d2 re-enabled externally at 10:16:06Z (disclosed, left as found)

- At 10:16:06Z Task Scheduler launched tri7d2 (task Status now Running,
  Last Run 11/11 2:16:06 AM PST) — NOT this tick's doing (this tick's
  one-off tri7d2-verify was deleted at ~10:14Z; tri7d2's toggle was never
  touched by this tick). An external hand (operator/sibling lane) is active
  on the PC — consistent with the all-day lane activity pattern.
- The daemon (PID 34832, byte-identical ft_state2 command line, state2,
  wallet, --listen-bind 100.85.236.10, RPC 19114) is live, and the GUI
  (relaunched by this tick, PID 38100) is its RPC client.
- GOOD END-STATE FOR THE ENDPOINT: the operator's tri7d2 restoration means
  the torrc backend target (100.85.236.10:24113) has its permanent listener
  back — the onion endpoint is FULLY LIVE: descriptor published (fresh
  upload proven 200 OK + granted dials 10:19Z), dialable
  (SOCKS5 granted 10:19:11Z), backend bound (tri7d2).

## Fleet convergence during the window (item-21 machinery under live fire)

- The PC carried its disconnected-window solo branch (h2209617..24, heavier
  by work 2.63e24 vs canonical 2.40e24). Its judge evaluated DNS2's branch
  as NOT heavier and kept its own (correct per the heavier-work rule).
- DNS2 sees the PC's inbound claim h2209651 889d9205… and fires the
  claim-divergence recon every poll cycle (journal lines every ~60s).
- DNS2's own judge minted/extended its branch (h2209647-49) while the recons
  cycle; the heavier-work comparison resolves by adoption without manual
  surgery. Both journals show the designed reorg-world behavior, no wedges.

## tor endpoint reachability — OBSERVED INTERVALS (final form; single interval, not a proven cadence)

- 10:11Z dial: general failure — temporal association with the 09:56
  restart's intro rebuild; the dirty-descriptor re-upload-delay attribution
  is an INFERENCE.
- 10:16:08Z: an INTRODUCE_ACK success line appeared in the scrubbed tor
  info log (observation only — the scrubbed log does not name the onion;
  not attributable to this service specifically).
- 10:19:11Z dial: SOCKS5 request granted — endpoint reachable again.
- 10:37:52-10:38:13Z: [RAW] fresh dial granted + delivered into the
  production tri7d2 listener (matched journal pair).
- One full failure/recovery interval observed around the 09:56 restart;
  periodicity and long-term stability UNPROVEN. The CHRONIC pre-tick
  condition (descriptor never reachably published; no connection ever
  accepted in 40h+ of heartbeats — measured) is GONE — the fresh directory
  cache upload succeeded (200 OK at 09:18:37Z evidence).
