# BRIEF: addrindex_build must thread WITH the tipstate oracle (spawn_build(None) stalls forever)

## The live incident (2026-11-10/11, DNS2)

`tri7-addrindex.service` (15-min timer) ran a perfect kill-loop since Nov 09:
every catch-up walk died at exactly 900s (TimeoutStartSec) until the
2026-11-10 judged drop-in raised it to 0 (grade A
urn:ump:5l3frusegvonmqmd6dwx4xzuwcmh6j4yyxdn7b45prf3wvfuxgha). After the
drop-in the walk survived — and froze anyway:

- 15:22:03 PST `ready … threaded_tip=Some(2208645) built in 98s`
- 15:36:53 PST `ready … threaded_tip=Some(2208645) built in 88s`
- 15:51:50 PST `ready … threaded_tip=Some(2208645) built in 84s`
- 16:50:26 PST `block index threading stalled at h2208645 (need h2209332)`
  → exit(2) → timer re-fired instantly → repeat (an hourly stall loop
  replacing the 15-min TERM loop; same net effect: catch-up never completes).

## Root cause (measured, not guessed)

1. The process sat 57 min with **zero CPU growth** (stuck at 1m20s), zero
   file IO growth, `Threads: 1`, main thread in `hrtimer_nanosleep` — the
   wait loop (`addrindex_build.rs` line ~152) polls `store.status()` every
   1 s for `threaded_tip >= chain_tip`, but **nothing advances
   threaded_tip**: the build's background thread had already exited.
2. `addrindex_build.rs` called `store.spawn_build(None)` — NO oracle. With
   `oracle: None` the blockindex threader falls back to pure positional
   prev-hash chaining (`body[4..36] == prev_want`), which stops at the
   first stale fork-war frame in file order. DNS2's file holds abandoned
   frames from the 2026-11-09/10 fork races; positional chaining freezes at
   h2208645 forever. That is the deterministic, reproducible stop.
3. Proof by counter-example on the SAME file, same machine, minutes apart:
   `store_gapfix` (which builds a `BuildOracle` from tipstate — hash-first
   authority, stale frames skipped by hash) reported
   `ready … threaded_tip=Some(2209363)` and `window coverage: 4600 present,
   0 missing — nothing to repair`. The store is healthy; only the
   oracle-less build stalls.

## The fix (88-line diff, one file: src/bin/addrindex_build.rs)

- Parse tipstate entries ONCE into an ordered `(height, display-hash)` vec
  (sort + dedup — same shape store_gapfix uses), derive both the walk
  phase's display-hash map AND a `BuildOracle` (contiguity-checked;
  `display_to_internal` per hash; refuse with exit(2) + honest stderr when
  the window is empty, non-contiguous, or has an unusable hash — refusing
  is correct: without the oracle the run stalls for its whole 3600 s
  deadline and exits anyway).
- `store.spawn_build(Some(build_oracle))` — hash-first threading skips
  stale frames and reaches the real tip.
- The wait loop, walk phase, resume (meta offset), completion stamping and
  the fail-closed unresolved-quarantine path are UNCHANGED.

## Test evidence (end-to-end, on a scratch COPY of the live store)

`addrindex_build /tmp/addrtest/blk0001.dat /tmp/addrtest` (982,967,511 B
copy of DNS2's live file + tipstate copy, fresh state dir):

- `tipstate oracle: 4600 hashes`
- `[blockidx] ready: 2209367 frames, 4497626 txs, heights_ok=true,
  threaded_tip=Some(2209365), writable=true, built in 79s`  ← past the
  h2208645 stall point on the first build
- `store threaded to h2209365`  ← the wait loop broke
- walk: `addrindex: 20000 blocks (3097 blocks/s …)` through
  `addrindex: 240000 blocks (2325 blocks/s, 379569 outputs, 372499 spends,
  h239999)` — ~2,300 blocks/s sequential, so the full ~2.2M-block walk is
  ~16 min. (The previous "hours" estimate was wrong: the walk never ran
  under the old binary — it was stuck pre-walk.)

## Deploy/verify plan (DNS2 = the repo checkout IS the tool's deploy path)

1. Commit + push master (CI green).
2. Rebuild `--release --bin addrindex_build` from the committed tree at
   /root/tri7/target/release (the unit's ExecStart path).
3. The stuck pre-fix run (PID from 16:50:26 PST) exits at its own 3600 s
   deadline (~17:50 PST); the timer then fires the FIXED binary.
4. Verification: journal shows `ready … threaded_tip=Some(2209xxx)` with
   2209xxx > 2208645, `store threaded to h2209xxx`, walk progress lines,
   and eventually `addrindex: done` + complete stamp (completion may land
   next tick — the loop-break itself is proven by threading + walk
   progress).

## Rollback

`git revert` + rebuild the bin; the drop-in (TimeoutStartSec=0) is
independent and stays. No data is written by this change beyond what the
old binary wrote (addressindex.db in the state dir, resume offset).

## Disclosure

Uncommitted sibling-lane hunks in src/blockindex.rs (FRAME_MAGIC made pub)
and src/node_embedded.rs (store_has_bodies data-truth check, Sami laptop
2026-11-10) are NOT part of this change and stay uncommitted, per the
item-12 precedent.
