# Fix: stale legacy test asserts pre-torn-tail contract — deterministic linux-tests CI red on master

## Artifact
Test-only change to `tests/legacy_tests.rs` (tri7 repo, master). One test renamed
and rewritten; one import line extended. No product code touched — fleet binaries
are unaffected, no deploy required.

## What broke (measured, from CI logs)
- CI run 44079577729 (commit 23564c5, push 2026-11-17T12:03:30Z): linux-tests FAILED
  — `frame_truncated_body_errors ... FAILED`, `assertion failed: r.next_frame().is_err()`
  at tests/legacy_tests.rs:419; suite `23 passed; 1 failed` in that binary.
- CI run 44076565150 (commit b6ad53f, push 11:43:26Z): identical failure —
  deterministic across pushes, not a flake.
- CI run 44085978382 (master HEAD 0bf3282): linux-tests completed failure while the
  run was still in progress (same test class).
- CI run 44073049952 (commit f1688c8, push 11:19:58Z): SUCCESS — the break was
  introduced by the actor lane's remediation batch that landed AFTER f1688c8
  (7c2972e/724bd64/b6ad53f: consensus hardening + storage/wallet integrity +
  supervisor hardening), which added torn-tail tolerance to the blkfile reader.

## Root cause
The landed design (src/blkfile.rs, deliberate and unit-tested in the same module):
- `FrameRead::TornTail { last_complete, offset }` vs `FrameRead::Corrupt` — a power
  cut mid-append (short final body/header at EOF) is a distinct recoverable signal,
  NOT an error; mid-file corruption and implausible length prefixes still error.
- `next_frame()` = tolerant view: torn tail -> one log line + clean EOF (`Ok(None)`),
  "the tolerant shape every existing caller already handles".
- `next_frame_checked()` = strict view: preserves `FrameRead::TornTail`.
- `last_complete_offset()` = the cut point; `blockindex` carries `torn_tail: Option<u64>`
  and heals the tail on the next append (its own test `torn_tail_builds_ready_and_the_
  next_append_heals_it` covers that).
- In-module unit tests `short_body_at_eof_is_a_torn_tail_not_an_error` and
  `short_header_at_eof_is_a_torn_tail` pin this contract.

`tests/legacy_tests.rs::frame_truncated_body_errors` predates the contract and
asserted the OLD behavior (`r.next_frame().is_err()` on a truncated body) — stale,
not a product bug. Reproduced locally on master HEAD 0bf3282: the reader logs
"torn tail — incomplete frame at offset 0 (last complete frame ends at 0); serving
the archive up to that offset" and returns Ok(None); the stale assertion panics.

## The fix (tests/legacy_tests.rs only)
- Import `FrameRead` alongside `BlkFileReader`.
- Rename `frame_truncated_body_errors` -> `frame_truncated_body_is_a_torn_tail` and
  rewrite it to pin the NEW contract from both views:
  1. tolerant: `next_frame().unwrap().is_none()` and `last_complete_offset() == 0`;
  2. strict: `next_frame_checked()` returns `Err(FrameRead::TornTail { 0, 0 })`
     (not `Corrupt`) — the cut-point evidence, stronger than the old `is_err()`.
- Neighbour tests unchanged: `frame_rejects_oversized_length` and bad-magic still
  assert Err (mid-file corruption / implausible lengths still hard-error).

## Verification (commands actually run, this host, master 0bf3282 + this diff)
- `cargo test --test legacy_tests` -> `test result: ok. 24 passed; 0 failed;
  0 ignored; finished in 0.01s` (was 23 passed / 1 failed before the fix).
- `cargo test --test legacy_tests frame_truncated_body_errors` on UNFIXED master
  -> FAILED with `assertion failed: r.next_frame().is_err()` (reproduction).
- `git diff --stat` = 1 file changed (tests/legacy_tests.rs), +29/-8.

## Scope and known-unrelated residue (disclosed)
- Windows-only CI failures (blockindex torn-tail append `.expect` + 4
  gui::node_supervisor i2p tests panicking in the new native TCP-table listener
  attribution) are a SEPARATE deterministic set on the actor lane's active
  surfaces; NOT addressed by this change, tracked for the owning lane.
- The loop's CI confirmation bar (macos-build green) held on all three red runs;
  this change restores the primary linux-tests surface.

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": [...], "blocking_issues": [...], "polish_suggestions": [...]}
