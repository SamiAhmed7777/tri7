# ITEM 36 — DEFERRED-REORG STALE-DROP (fork-body livelock of a race loser) — ROUND 2

## Round-1 verdict (addressed here)
urn:ump:xswdjqnl57hbn3w2kyf2hu5trponmatxclfchrhec3qu5szmt5na, grade C,
blockers: (1) demonstrate or enforce that no unrelated entries can enter
pending_from_headers while PendingAdoption is live, or scope the exemption to
hashes belonging to the selected adoption; (2) add a regression test through
the actual production adoption handler, without manually clearing the pending
record or manually reproducing rollback/advance steps. Polish: fix the
child-drain test description; make the patch independently verifiable.

## What round 2 changed (verified against the shipped bytes)
1. BLOCKER 1 — ENFORCED, not just demonstrated. The dispatch-pop suspension is
   now SCOPED: `let suspend_height_arm = reorg_pending &&
   stg.pending_headers.contains_key(&hash)` (item36-r2.diff, dispatch-pop
   hunk). Scope argument: `pending_headers` is EXACTLY the queued set —
   process_new_tip_headers (the 30s tip poll) pairs every
   `pending_from_headers.push_back` with a `pending_headers.insert` and
   skips hashes already held as pending headers; the Switch arm pairs its
   re-queue with inserts of exactly the selected branch's headers; entries
   leave the set when their body lands (advance path) or when the stale-drop
   removes both. So membership proves the entry belongs to the pending
   adoption; a foreign body that somehow reaches the queue mid-reorg keeps
   the item-35 height drop. NEW test
   `height_arm_suspension_is_scoped_to_the_pending_branch_headers` pins it:
   after a Switch, branch hashes keep the suspension at the tip height while
   a foreign [0xEF;32] hash at the same height is still stale-dropped.
   Residual (honest): a washed-out branch body (BODY_TRIES_WASHOUT removes
   its pending_headers entry) loses the exemption and falls back to the
   item-35 drop — i.e. degrades to the PRE-FIX behavior for that one entry
   (bounded by the TTL expiring and fork choice re-firing, which re-inserts
   membership), never worse than pre-fix.
2. BLOCKER 2 — the T7-03 arm is moved VERBATIM out of the block handler into
   the production fn `consume_deferred_adoption(st, rpc, stats, hash, b)
   -> DeferredAdoption` (enum NotPending/Stop/Completed); the handler now
   reacts: `match consume_deferred_adoption(&mut *st, rpc, stats, &hash, &b)
   { NotPending => {} Stop => return Ok(()) Completed => links_tip =
   b.prev_hash == st.tip_hash }`. The three inline `return Ok(())` exits
   became decision variants; every other line is unchanged (log strings,
   probe shape, rollback loop, `stats.reorgs += 1` all identical). The DNS3
   replay test now drives THIS fn: after the Switch it feeds the served fork
   body to consume_deferred_adoption and asserts
   DeferredAdoption::Completed, tip rolled back to the fork point (h104,
   tip_hash == fork_hash), pending record consumed, stats.reorgs == 1 — then
   the body advances the tip through the same validate live:true +
   advance_with_block shape the item-23 adoption-pipeline test uses (the
   handler's ordinary tip-extension path, shared with all existing adoption
   tests). No manual clear_pending_adoption, no manual rollback in the test.
3. POLISH — the child-drain loop in the replay test is now honestly
   described: reorg_pending is false there (record consumed), so it pins the
   ORDINARY post-adoption pipeline (above-tip children, item-35 gate must
   not fire), not suspension.
4. VERIFIABILITY — this round passes the repo worktree itself so every claim
   is re-runnable: `git diff` in /root/tri7 shows the live change; the
   suite commands below were run in this exact worktree at HEAD bce1b9d +
   this uncommitted diff. Also: `grep -c` receipts for every delta claim:
   membership scoping = 1 site; `fn consume_deferred_adoption` = 1; the
   handler call = 1; the old inline arm is gone (the rollback log string now
   occurs exactly once, inside the fn); 3 new item-36 tests.

## Unchanged from round 1 (context)
The live incident: 2026-11-20 00:30–00:57Z DNS3 lost an equal-work race
(its own mint h2215652 `98071c4c` vs DNS2's canonical `40df35c3`), fork
choice deferred the rollback, and the item-35 dispatch-pop stale gate
(`height <= tip`) killed the branch's fork body every ~30s for 27 min (32+
drops; the give-up cap never fired because each drop erased the attempt
memory). Fix: a live PendingAdoption (pending_adoption_live, TTL-scoped,
expires exactly like pending_adoption_for) suspends ONLY the height arm and
ONLY for the branch's own headers. `reorg_pending` is read once per dispatch
pass inside the existing st_shared scope; no code path takes the
PENDING_ADOPTION mutex and then st_shared, so lock order (st_shared →
PENDING_ADOPTION) is preserved. The bounded-rotation property is unchanged:
TTL 300s < give-up cap ~15 min, so a permanently unservable branch body
falls back to the item-35 drop when the record expires.

## Tests (run on the shipping bytes, this exact worktree)
- `cargo test --lib`: 872 passed, 0 failed, 1 ignored (pre-change baseline
  869/0/1; +3 new item-36 tests: the scoped predicate test, the
  membership-scoping test, the DNS3 replay through the production fn).
- Integration: cli_exit_tests 5, consensus_wallet_tests 15, legacy_tests 24,
  spend_corpus_tests 2 — all green, 0 failed.

## Rubric
Same as r1: correctness of the scoped gate semantics, the bounded-rotation
claim, lock ordering, verbatim-move fidelity of consume_deferred_adoption
(no behavior change in the extraction), test coverage of the incident shape.

## Answer format
A single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
