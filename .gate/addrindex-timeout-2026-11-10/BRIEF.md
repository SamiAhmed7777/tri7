# JUDGE BRIEF — tri7-addrindex.service TimeoutStartSec=0 drop-in (DNS2, 2026-11-10)

## Artifact
New file `/etc/systemd/system/tri7-addrindex.service.d/timeout.conf` (drop-in for the
existing `/etc/systemd/system/tri7-addrindex.service`; the unit file itself is NOT edited):

```ini
[Service]
TimeoutStartSec=0
```

Apply = `systemctl daemon-reload`. Rollback = `rm` the drop-in + `daemon-reload`.

## Problem (live evidence, DNS2 journal 2026-11-10)
`tri7-addrindex.service` is `Type=oneshot` (`addrindex_build /var/lib/tri7/blk0001.dat
/var/lib/tri7`), fired every 10 min by `tri7-addrindex.timer`
(`OnUnitActiveSec=10min`, `AccuracySec=30s`), `TimeoutStartSec=900`, `Nice=10`,
`IOSchedulingClass=idle`.

The catch-up walk now needs MORE than 900 s: the block-index build alone takes ~85-100 s
(journal: "built in 84s/88s/98s") and the address walk must cover ~690 blocks
(threaded_tip stuck at h2208645 vs chain tip h2209335). systemd kills every walk at
exactly 900 s:

- 15:20:24 PST start → 15:35:24 `code=killed, status=15/TERM` (Consumed 1m27s CPU)
- 15:35:25 start (timer re-fired INSTANTLY — OnUnitActiveSec already satisfied)
- → 15:50:25 `code=killed, status=15/TERM` again → instant re-fire 15:50:25 …

Result: an infinite kill/re-fire loop. The address-index catch-up NEVER completes,
`threaded_tip` stays frozen at h2208645 and falls ~2 blocks/min further behind the tip,
and the host burns ~10% duty-cycle CPU + idle-IO forever. Every fire since 2026-11-09
17:37 PST has been killed (the gap opened when the catch-up distance grew past what 900 s
covers; a sibling-lane rebuild wiped the binary 2026-11-10 ~23:08Z and the loop briefly
went 203/EXEC until the binary was rebuilt the same night).

## Why TimeoutStartSec=0 is safe here
1. The walk is BOUNDED, not a server: `addrindex_build` snapshots a fixed tipstate oracle
   at start ("tipstate oracle: 4600 hashes"), walks exactly those frames, writes the
   index, and EXITS when caught up. 0 disables the start timeout ("Pass 0 to disable the
   timeout logic", systemd.service(5)) — it does not make a hung walk killable, but there
   is no unbounded work: the oracle window is fixed at start.
2. Priority containment already present in the unit: `Nice=10` + `IOSchedulingClass=idle`
   keep an unterminated walk from starving the live staking daemon.
3. Memory peak observed 1.0G, no swap — no resource risk change; this is a timeout
   setting only.
4. Completion is self-healing: once one walk completes, the NEXT 10-min cycle starts
   near-caught-up (short walk, well under 900 s) and the loop ends naturally.

## Verification plan (this tick)
1. Write drop-in, `daemon-reload`.
2. Observe the in-flight walk: it either completes (daemon-reload can re-arm the job
   timer) or is killed once more at the legacy 900 s mark — in the killed case the
   instant re-fire starts a walk that must now run PAST 900 s with NO `15/TERM` line,
   proving the drop-in binds.
3. Expect `Result=success` / clean exit, `threaded_tip` advancing past h2208645 toward
   the tip, and the next timer cycle quiet or short.
4. Journal excerpts recorded as evidence.

## Scope
DNS2 local systemd unit only. No repo change, no other host, no daemon restart, the live
`tri7d` staking process is untouched. Fleet parity re-verified at tick end.
