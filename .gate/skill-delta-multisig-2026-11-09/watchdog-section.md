=== PROPOSED ADDITION to tri7-fleet-chain-state SKILL.md, section 2 (Repair a node on the wrong branch), after item 5 ===

6. **On sami-pc, disable `tri7d-watchdog` BEFORE touching the state dir, and
   re-enable only after parity is verified.** The task is "start-only" (starts
   tri7d2 when nothing listens on 19114) — which is exactly wrong mid-repair:
   2026-11-09's second PC repair had the watchdog fire between the scp and the
   file moves, the daemon booted on the fork-era pair ("chased tip h2208189"),
   REWROTE tipstate.json with fork state over the freshly-seeded canonical
   file, and rebuilt the index over the fork store. Assume every state-dir file
   can change under you while the watchdog is enabled. (The item-10 verdict
   "start-only, never kills, harmless" holds only for normal operation, never
   for a repair window.)

=== PROPOSED ADDITION to the same skill's references/2026-11-09-fork-cascade.md, after "Repair recipes" ===

### Second sami-pc fork + the watchdog trap (2026-11-09 ~14:0xZ, verified live)

The PC forked AGAIN at h2208189 (5dc98597 vs canonical cd146a34) — it followed
Pi's fork via its out-peer before Pi's own repair. Repair by the canonical
re-copy recipe worked, but the FIRST attempt was poisoned mid-flight:
tri7d-watchdog started tri7d2 while the new store/tipstate were still staged,
the daemon loaded the fork pair and persisted fork-state tipstate.json over
the canonical seed (size is the tell: 3,002,231 fork vs 3,015,600 canonical),
and its index rebuild covered the fork store. Recovery that worked: kill the
daemon, `schtasks /change /tn tri7d-watchdog /disable`, re-copy DNS2's fresh
tipstate (daemon verifiably dead), rename the watchdog-era index aside
(index.forkC2-2208189-wdog), one clean boot (blockidx rebuilt 53s threaded to
the live tip), parity exact, repairwallet passed. Other facts: the repair
`move /y` overwrote a STALE .forkC-2208189 aside-name from the same morning
(fork stores are discarded material); the GUI must be ended BEFORE tri7d2 so
its supervisor cannot spawn an embedded node when 19114 drops; ~6 GB of dead
fork stores now sit in state2 (cleanup = a future disk sweep).
