# Judge brief: tri7 lane-flaky-testfix — CI-flaky follow_tip test made deterministic

## The CI symptom
`tests::chain_slot_wiring_seed_rollback_and_window_slide_follow_the_threaded_tip`
failed 2 of the last 3 CI runs (`h105 + h106 slots removed` left 4 right 5),
passed locally every time.

## Root cause (REPRODUCED, not guessed)
Cross-test interference on the process-global `LIVE_STORE`
(OnceLock<Mutex<Option<Arc<BlockStore>>>>): the flaky test holds its store in
the global while a concurrently-running test fires an adopt-rollback
(drop_chain_slots_above) — slots deleted from the flaky test's store
mid-assert. Reproduced LOCALLY with temporary probe sleeps (1.5s in-test,
300ms before the adopt): exact CI signature (left 4, right 5). Probes removed
after the fix.

## Fix (+39 lines, all inside #[cfg(test)] mod tests)
A test-only `LIVE_STORE_TEST_LOCK` guard taken as the first statement of the
11 tests a scripted call-graph analysis proved can transitively reach the
global. Mutual exclusion makes the interleaving impossible. Zero sleeps,
zero assertion changes — h105+h106 removal, guard drop, h104 hash, slide-to-3
all asserted verbatim. Block comment documents the rule for future tests.

## Verification (parent reran independently: 3 more consecutive greens)
cargo test --bin follow_tip: 76/76 × 5 consecutive (child) + 3 (parent) —
8 consecutive greens total. cargo test --lib: 567 passed / 0 failed / 1
ignored. Diff: src/bin/follow_tip.rs only, every hunk ≥ L11965 inside the
test module.

## ROUND 2 — both judge blockers addressed
1. Complete evidence: tests-flakyfix.txt now carries ALL EIGHT consecutive
   follow_tip runs' result lines (8× "76 passed; 0 failed") + the lib suite.
2. Mechanical reach proof: live-store-reach-proof.txt — a scripted analysis
   over the tests module enumerating every fn whose slice touches the
   accessors (set_live_store/live_store/LIVE_STORE), one level of helper
   indirection, the guarded set, and the residual. v2 found a REAL gap the
   child's analysis missed: gap_find_missing_satisfied_by_recent_cache
   reaches the global through the PRODUCTION fn gap_find_missing()
   (consults live_store() as a cache tier — a read reach). GUARDED now
   (guard as first statement). Analysis verdict: PASS (reach set fully
   guarded).

## ROUND 3 — all three blockers closed (child-executed, parent-verified)
1. **Whole-call-graph proof**: scripts/live_store_callgraph_audit.py (576
   lines, committed) — brace-precise fn extraction, receiver-aware call
   resolution (bare/method/::path classes; same-name defs union
   conservatively), TRANSITIVE reach from all 77 test fns through
   PRODUCTION fns. PASS: 12 reachers, all guarded — including
   ChainState::trim -> live_store() (a production-method reach the v2
   analysis missed) and the recon_*/rollback/fork_choice chains.
   Parent re-ran the audit at the revision: exit 0, PASS.
2. **Stale-global reset**: lock_live_store_tests() now RESETS LIVE_STORE on
   acquire (lock+reset — every guarded test starts clean; a panicked or
   forgotten cleanup is neutralized by the next acquirer). New test
   live_store_test_lock_resets_the_global_on_acquire pins it;
   chain_slot_wiring's explicit cleanup kept as defense in depth.
3. **Runnable revision**: committed on the lane branch — code 188a2a6,
   transcripts 9f2ba51 (worktree clean, master untouched). 8 consecutive
   greens AFTER the commit: 77 passed; 0 failed ×8 (transcript commit).
   Parent re-verified: 77/76... — follow_tip 77 passed, lib 567 passed at
   the revision.
Suite delta note: 76→77 = the new reset test.
