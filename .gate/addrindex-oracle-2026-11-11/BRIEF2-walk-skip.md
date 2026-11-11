# BRIEF (round 2): addrindex walk — stale fork leftovers are skip-by-design, not unresolved

Follow-up to the round-1 fix (commit ceb81df, grade B zero blockers
urn:ump:ye3urbbnw3vwhhbmuf5gt5pkboxdstizcbcqefpnysxmpqoa7iwq), which made
`addrindex_build` thread WITH the tipstate BuildOracle. Live result of that
fix: threading passed the old h2208645 stall immediately — and the walk then
surfaced the SECOND half of the defect on the real store:

## The live failure (2026-11-11 17:13:15 PST, DNS2, real state dir)

```
store threaded to h2209371
resuming at file offset 982516620
FAIL: 1 canonical frame(s) skipped — index NOT marked complete   → exit(3)
```

Every timer cycle would repeat this forever: the walk region (the fork-war
tail, blocks ~2208646..2208999 + the live tip) contains exactly ONE stale
fork leftover frame (hash 02d723be5e4cefbcd6be57aeead9f83b6ac7db9a0229d5b3cb
8a24dae45d864e at file offset 982592001 — an abandoned race block from the
2026-11-09/10 fleet fork wars, in the store's frame list but height-less by
design of the hash-first build). The reorg world guarantees such frames in
any tail, so the old fail-closed treatment made the tool UNABLE to ever
complete — same net effect as the round-1 stall.

## Why skip-by-design is CORRECT here

The `_` arm in question is the ORACLE-MISS path: the frame's hash is not the
tipstate's hash at any window height. After round 1, the threading is
oracle-authoritative and guaranteed to reach the snapshot tip before the
walk starts (the wait loop). A canonical block's hash IS the tipstate hash at
its height — an oracle-miss frame is therefore DEFINITIVELY non-canonical
(reorg leftover), exactly like the adjacent `Ok(None)` arm which was always
skip-by-design. The fail-closed treatment dates from the oracle-less build,
where heights were unreliable and a canonical block could genuinely hide
here; that world no longer exists. Fail-closed `unresolved` is PRESERVED for
store lookup ERRORS and for anything the oracle does claim.

## The change (54-line diff, one file, walk phase only)

- `_` arm (oracle-miss + frame present + no usable height): count
  `stale_skipped`, log the FIRST one with its offset + hash + reason,
  continue. No `unresolved`.
- Final `done` line reports the stale count honestly.
- Everything else (oracle threading from round 1, resume, completion
  stamping, fail-closed unresolved/quarantine paths) unchanged.

## Test evidence (live + scratch, both on DNS2)

1. LIVE (patched binary via the 17:20:02 PST timer cycle, real state dir):
   `store threaded to h2209376 → resuming at file offset 982776021 →
   addrindex: done — 377 blocks, 640 outputs, 655 spends in 12.2s →
   Deactivated successfully` — db meta now
   `{height: 2209373, offset: 982776021, complete: 1}`: the address index is
   COMPLETE and current (the tool is functional again; future cycles are
   short re-walks of the sub-500-block tail, harmless: insert_block is
   INSERT OR IGNORE idempotent).
2. SCRATCH (copy of the live store + db, meta offset rolled back to
   982516620 to force the walk through the fork-war region containing the
   leftover): `walk: stale fork leftover at offset 982592001 (hash 02d723be…
   not in the tipstate window) — skipping non-canonical frames by design →
   addrindex: done — 879 blocks, 1497 outputs, 1506 spends, 1 stale fork
   leftover(s) skipped in 2.4s` — the exact frame that failed every run is
   skipped by design and the walk completes.

## Rollback

`git revert` + rebuild; the tool then fails fail-closed again (exit 3) but
loses nothing (no wrong stamps in either direction).

## Notes for the record

- The completion stamp `complete=1` covers the meta `height` (finish() at
  the oracle chain tip); frames appended AFTER the tipstate snapshot are
  picked up by the next cycle (snapshot advances with the daemon's flush) —
  pre-existing resume semantics, unchanged.
- Quirk observed, not a defect: the meta offset row stays at the last
  500-block slice after completion, so each cycle re-walks the final
  <500-block tail (~12 s) — idempotent by INSERT OR IGNORE. A future polish
  could stamp the EOF offset on reached_eof.
