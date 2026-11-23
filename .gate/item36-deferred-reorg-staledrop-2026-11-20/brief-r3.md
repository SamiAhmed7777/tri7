# ITEM 36 — DEFERRED-REORG STALE-DROP (fork-body livelock of a race loser) — ROUND 3

## Verdict history
- r1 grade C urn:ump:xswdjqnl57hbn3w2kyf2hu5trponmatxclfchrhec3qu5szmt5na:
  blockers (1) queue-invariant of the exemption, (2) regression through the
  production adoption handler.
- r2 grade C urn:ump:p6mccfut4kxzt3iy24myy3hluyukg3wzjhn7msesquvaqqvhwhka
  (one infra-F urn:ump:m26ibuhfqzg6takzag6goiatxfrww4jwnmhvjgqmlakbwrwwbjxa
  "malformed JSON from Codex" preceded the real r2 run — infra, not graded,
  retried once per protocol): blockers (1) "Use explicit selected-adoption
  membership, such as hashes stored with the PendingAdoption epoch/branch,
  rather than global pending_headers membership; then test unrelated queued
  and superseded headers during a live adoption", (2) handle_message-path
  regression. Polish: correct the "pending_headers is exactly the queued
  set" claims; rerun suites when writable.

## What round 3 changed (verified against the shipped bytes)
1. BLOCKER r2-1 — EXPLICIT SELECTED-ADOPTION MEMBERSHIP, exactly the shape
   the verdict asked for: `PendingAdoption` now carries `branch_hashes:
   Vec<Hash>` — populated at the Switch arm from the selected branch's
   headers, atomically replaced by any later Switch (the whole record is
   overwritten by set_pending_adoption). New lookup
   `pending_adoption_covers(&hash, now)` (TTL semantics identical to
   pending_adoption_for/pending_adoption_live — an expired record is dropped
   there too; borrowing scan, no per-entry clone). The dispatch-pop gate is
   now `let suspend_height_arm = pending_adoption_covers(&hash,
   Instant::now())` — the global pending_headers map is NO LONGER consulted
   at all. Consequences, each pinned by the rewritten test
   `height_arm_suspension_is_scoped_to_the_selected_adoptions_record`:
   - the selected branch's own hashes keep the suspension at the tip height;
   - an unrelated ORDINARY header the tip poll inserted mid-adoption
     (placed in pending_headers AND queued, the exact mid-adoption shape) is
     NOT covered → the item-35 height drop stands;
   - superseded-era residue left in pending_headers is NOT covered → drop;
   - a fabricated foreign hash is NOT covered → drop.
2. POLISH r2 — every "pending_headers is exactly the queued set" claim is
   GONE from my comments and this brief (the r2 code comment block, the
   predicate doc, the test doc all now describe record membership). The
   pre-existing comment inside process_new_tip_headers ("pending_headers is
   exactly the queued set", line ~1729, from an earlier item's fix) is
   another item's record — left untouched, disclosed here.
3. BLOCKER r2-2 — precision on what "the production handler path" can mean
   in this codebase, and what the test now drives: handle_message is not a
   callable fn — it is the message-dispatch closure inside the ~26k-line
   main event loop (owns the peer set, sockets, the shared store guard, the
   RPC handle); there is no node harness in this repo (all 872 lib tests,
   including the item-21/23/28 adoption tests, exercise handler behavior by
   driving the same production fns the handler calls). Round 3 keeps the r2
   extraction (consume_deferred_adoption: the T7-03 arm moved VERBATIM out
   of the handler — record lookup, ancestry check, live:false probe,
   rollback loop, reorg credit — its three `return Ok(())` exits became
   decision variants; the handler reacts: NotPending → continue, Stop →
   return Ok(()), Completed → recompute links_tip). The DNS3 replay test
   drives THAT production fn for the entire consumption+rollback segment —
   the part item 36 changed — and asserts Completed / tip at the fork point
   / record consumed / reorgs==1. The test's remaining steps are the
   handler's ordinary tip-extension tail: validate_block (the same
   production validator the handler calls, live:true) + advance_with_block
   (the same production advance) — the identical shape every existing
   adoption-pipeline test uses (e.g. the item-23 adoption-persistence test).
   No step in the test is a test-local reimplementation of logic; every call
   target is production code. A live-node handle_message harness does not
   exist in this repo; building one is a structural project far beyond item
   36's fix and would put an unreviewable harness diff inside a
   correctness-fix commit.

## Unchanged context
The live incident (2026-11-20 00:30–00:57Z DNS3): lost equal-work race at
h2215652, deferred rollback, item-35's `height <= tip` dispatch-pop gate
dropped the branch's fork body every ~30s for 27 min. Fix: while a live
PendingAdoption exists, the height arm is suspended ONLY for hashes in THAT
record's branch_hashes; TTL 300s bounds any rotation (give-up cap ~15 min
never reached first), and item-35's demote scoping (only above-tip bodies
credit miss rounds) means even a mis-scoped body can never demote a healthy
peer. Without a live record the gate is byte-identical to item 35.

## Tests (this exact worktree, HEAD bce1b9d + this diff)
- `cargo test --lib`: 872 passed, 0 failed, 1 ignored (baseline 869/0/1;
  +3 item-36 tests: scoped predicate, record-membership scoping incl. the
  ordinary/superseded/foreign cases, DNS3 replay through the production fn).
- Integration: cli_exit_tests 5, consensus_wallet_tests 15, legacy_tests 24,
  spend_corpus_tests 2 — all green.
- The judge sandbox could not run cargo (read-only FS, build-lock) — r2
  disclosed the same; the numbers above are from this host's full runs and
  the repo dir is attached for every static re-check (git diff, greps).

## Receipts (grep -c on src/node_embedded.rs)
- `fn pending_adoption_covers` = 1; `branch_hashes` sites = 6 (field decl +
  doc, Switch population + comment, covers lookup body); dispatch call
  `pending_adoption_covers(&hash, Instant::now())` = 1; the dispatch gate
  contains ZERO `pending_headers.contains_key` (all 3 remaining hits in the
  file are in TEST code — lines 20499/21460/22819, one of them the r3 test
  asserting the ordinary header IS in the map yet NOT covered);
  `fn consume_deferred_adoption` = 1; handler call = 1; the item-36 test
  fns = 3.

## Answer format
A single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
