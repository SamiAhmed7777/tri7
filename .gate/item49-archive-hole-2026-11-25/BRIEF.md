# Item 49 ARCHIVE-HOLE-FOLLOWER — judge brief, round 1 (2026-11-25)

## What is graded
An UNCOMMITTED worktree change on `SamiAhmed7777/tri7` master at `ae29c1a`, two files:
`src/blockindex.rs` and `src/node_embedded.rs`. The graded bytes are `item49.diff`
(first file in the bundle): the full `git diff` of those two files, regenerated from the
worktree after the last edit. Nothing is committed, pushed or deployed yet. This grade
gates the commit; push and fleet deploy follow only on A/B.

## The defect (live, measured)
DNS3 is a from-zero synced follower (`tri7d`). Its chain is valid and current, but its
block archive has holes: for four heights `getblockhash(h)` returns the chain's hash and
`getblock(h)` answers "Block not found" — h2219461, h2219472, h2219489, h2219490.
Evidence: `ev_hole_scan_1200Z.txt`, a per-height RPC scan of h2219300..2219560 on DNS3,
Pi and DNS2 at 12:00:35Z. Every other entry in that file is a height ABOVE the node's
tip ("both unserved": no hash either) and is not a hole. Pi (tip h2219526) and DNS2
(tip h2219534) have no such height below their tips, so peers can serve the bodies.
Effect on the node: the wallet balances seed logs
"skipped height(s) … archive coverage hole; balances exclude those heights".

## Root cause (DNS3 journal — `ev_rootcause_timeline.txt`)
Two cooperating defects:

1. `BlockStore::append_block` called while the index is still building buffers the body
   in RAM (`pending`) and the buffer was drained ONLY by the next post-ready
   `append_block`. On DNS3 every `systemctl stop` runs to the 90 s stop timeout and is
   SIGKILLed. Timeline from the journal: pid 2751737 accepts h2219472 at 09:48:47Z
   during its build, the build lands at 09:49:01Z, no further append arrives, SIGKILL at
   09:49:13Z — the buffered body is gone and the height is a permanent hole. Same
   shape: h2219471 (accepted 09:46:14, ready 09:46:48, killed 09:47:31) and
   h2219488/489/490 (accepted 10:25:12–10:25:21 by pid 2762610, killed 10:26:30).
2. Why only some of those stayed holes: the tail walk `gap_find_missing` counts a body
   as present when it is on disk OR in the process-local `recent` cache. In the process
   that accepted the block, the lost body is still in `recent`, so the walk is masked.
   After a restart the walk looks only at the last 32 heights, so an older hole is never
   requested again. h2219471 and h2219488 were caught by the tail walk in the NEXT
   process ("GAP RECOVERY: tail body missing …" then "appended to the store from …" at
   09:51:02Z and 10:28:19Z). h2219472 aged out of the band. For h2219489/490 the tail
   walk stayed silent right after the h2219488 fill although both were inside its band
   and not on disk.

Honest limits of this statement:
- Defect 1 is directly evidenced by journal timestamps plus the source.
- For h2219489/490 the explanation is INFERRED: the only branch of the tail walk that
  skips a height which is not on disk is the `recent` cache. The block handler caches
  raw bytes BEFORE validation with height 0 for any body that does not extend the tip,
  and nothing then appends such a body. I did not observe the insert itself.
- h2219461 predates 09:30Z (already listed in the 09:30:20Z seed line). Its creation was
  not observed. The audit below repairs it whatever its cause.

## The change
### src/blockindex.rs
- Crash journal `<blocks file>.pending`. Record: `[u32 LE magic 0x334A4250][u32 LE body_len]
  [u64 LE height | u64::MAX][32-byte header hash][body]`. Appended and fsynced under the
  buffer lock before the buffering `append_block` returns. A journal write failure is
  logged and the accept still succeeds (RAM-only, as before).
- `BlockStore::new` reads the journal back BEFORE anything can buffer: it keeps the
  complete hash-verified prefix, stops at a torn / foreign / zero-or-over-long record or
  a body whose header hash is not the recorded one, cuts the file at that offset, drops
  duplicate hashes, and removes the file when nothing is recoverable.
- The journal is removed when the buffer is drained.
- `flush_pending` now runs at build completion BEFORE the store publishes Ready and once
  more right after (previously only on the next append). It holds the buffer lock for
  the whole drain and logs each body it could not write, with its height.
- `block_by_hash_in(&self, inner, hash)` is extracted so the pre-publish drain dedupes
  against the freshly built store (`self.inner()` is still None at that point). The
  public `block_by_hash` keeps its behaviour.

### src/node_embedded.rs (follow loop)
- `gap_repair_cached_tail`: every tick, BEFORE the tail walk, a tail-band body that is
  cached but not on disk is appended — only after `gap_fill_valid` (body hash == the
  chain's hash for that height, links to the entry below, merkle root commits), because
  the cache is unvalidated.
- `gap_audit_interior`: sweeps the window BELOW the tail band, newest first, at most
  `GAP_AUDIT_PER_TICK` = 1024 heights per tick, with a cursor and a wrap at the window
  base. A height whose body is neither on disk nor validly cached is a hole; the pass
  reports the lowest one it saw. A valid cached body is written on the spot.
- The tick requests an interior hole from peers exactly like a tail hole (same getdata,
  same interception and fill path), labelled "interior archive hole". An interior hole
  can never take the phantom-tip rollback branch (explicit `!gap.interior` guard in
  addition to `h == tip_now`). After `GAP_INTERIOR_TICKS` = 20 unfilled ticks the sweep
  resumes past it; a re-found hole is served quietly instead of being logged as new.
- New log line at the end of each full sweep:
  "archive audit: sweep N covered hA..hB below the tail band — X hole(s) found, Y cached
  body/bodies written to the store" plus " — no body missing below the tail band" when
  the sweep found none. The existing tail-gap log lines are byte-identical to before
  (deploy checks match on them).

## Tests — `ev_test_lib_summary.txt` (full transcript: `ev_test_lib_full.txt`, not in the bundle)
`cargo test --lib` on the worktree: 944 passed, 0 failed, 1 ignored, exit status 0.
Baseline before this change was 937. The 7 new tests are listed in the summary with
their result. They cover: journal survives a simulated kill and lands on the next open
without any further append; a buffered body lands when the build lands; verified prefix
kept, corrupt tail cut, an unverifiable record never replayed; a journal with nothing
verifiable is removed on open; the audit finds the interior hole and writes the cached
body (and the tail repair); per-pass pacing, cursor and wrap pinned; a stale-branch body
cached under a missing height's hash is never written.

## Not done / known limits — judge whether any of these is a blocker
- Not yet committed, pushed or deployed. Planned deploy: DNS3 first (the node with the
  holes, the canary that exercises the audit), then Pi, then DNS2 (the heavy staker,
  last). Verification: re-run the per-height scan and read the journal for the audit
  and fill lines.
- The audit only REQUESTS an interior hole. If no reachable peer holds the body it stays
  a reported hole (re-found on every sweep). Nothing is fabricated locally.
- Pre-existing and deliberately NOT changed here: the gap tick takes the chain lock while
  holding the gap lock, while the block handler takes the gap lock while holding the
  chain lock (st → gap vs gap → st). It is a latent lock-order inversion independent of
  this change; my added code reads the gap state before taking the chain lock and never
  nests them the new way. To be filed as its own workqueue item.
- Cost: at most 1024 `block_by_hash` lookups per 30 s tick on a ready store, plus at most
  33 extra lookups for the tail repair.
- `journal_path` is None when the store was built without a file path; then nothing is
  journaled (nothing to protect).
- A body buffered while the blocks file is not writable is still dropped, now with a log
  line naming the count — unchanged behaviour, now visible.

## Answer shape
Answer with a single JSON object on the last line and nothing after it:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
