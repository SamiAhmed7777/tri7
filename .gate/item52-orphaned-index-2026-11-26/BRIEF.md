# BRIEF — item 52: address index never forgets an orphaned block (DNS2 balance over-report)

## The defect (live facts, DNS2, 2026-11-25)

- `listunspent` on DNS2 (the sole heavy staker) kept two rows (6.3 and 6.30871 TRI) of the
  node's own stake transaction from block 2219981. That block was orphaned at 03:38:31Z;
  the rows were still served at 05:47Z — two hours after the wallet self-heal logged
  "wallet evidence matches the chain again".
- The coin was re-staked on the canonical chain at 03:50:15Z, so the money path counted
  it twice. Balance read 6086.025171 TRI; the rows-checked truth was 6073.416461 TRI
  (gap exactly 12.608710 TRI).
- A sweep of every DNS2 row at height ≥ 2219900 (156 rows) found exactly these 2 bad
  rows; rows below that height were not checked, so older phantoms were suspected and
  later CONFIRMED (see live evidence: 68 phantom outputs across the 4600-block window).
- Pi (17 rows) and DNS3 (8 rows) were clean. No fund loss observed. Whether spending a
  phantom row is harmless was never established — not claimed.

## Root cause

The address index stamped only ONE provenance fact per run: (coverage height, tip hash)
— the height the index has reached and the hash of the block it last accepted. That
pair vouches for the TOP of the index only. It says nothing about WHICH blocks produced
the rows below. The catch-up builder (`addrindex_build`) walks the append-only block
store in FILE order: when the node's own mint is the tip, the builder indexes it; when
a heavier branch later replaces it, the next run skips the stale frame ("stale fork
leftover — skipping non-canonical frames by design") but never REWINDS the rows already
indexed from it. The orphan's outputs stay unspent forever under a coverage height the
chain does vouch for, because nothing records that height 2219981 was supposed to hold
a different block.

## The fix (repo /root/tri7, files in this bundle)

1. **Per-height block ledger.** New table `blk(height INTEGER PRIMARY KEY, hash BLOB NOT
   NULL)`, written in the SAME transaction as the block's rows (`apply_block`). The
   index can now answer "which block did I apply at height h?" for every height. Legacy
   indexes (no `blk` rows) are the fleet's current state and are handled by the repair.
2. **Idempotent, hash-bound apply.** `apply_block(tx, height, block_hash, txs)` returns
   `Applied::Applied` or `Applied::AlreadyApplied` — the same block at the same height
   is never written twice; a different block at a taken height is refused.
3. **Guarded daemon write.** The node's own writer (`LiveIndex::apply`) consults
   `appendable()`: never a second block at a height, never a block above an orphan
   (the block held at height-1 must be this block's parent; heights indexed before
   `blk` existed fall back to the stamped coverage pair). A refusal is an error, not a
   silent gap; the builder's next verify puts the index right.
4. **Verify-and-repair at the start of every catch-up run** (`verify_and_repair`,
   driven by a `CanonicalChain` view — the same tipstate window that gives the walk its
   heights, plus the store it just threaded; read-only):
   - the audit range is the chain's WHOLE vouchable window [chain.base .. min(coverage,
     tip)] — never clamped down to where `blk` rows begin, because a `blk` row proves
     exactly one height; a half-recorded index (a new writer stamped the tail before
     the first catch-up repair ran) must not mask the unaudited legacy heights below it;
   - proves every height the chain can vouch for still holds the chain's own block
     (legacy indexes: whole window is suspect, from the window base);
   - `reconcile` then, in ONE transaction: deletes outputs proven phantom (the chain's
     block at the row's height is in hand and does not contain the tx, and the tx's
     frames all sit off the canonical chain), MOVES re-mined outputs to their canonical
     height (a tx first indexed in an orphan and later canonical at another height is
     kept, not deleted), re-marks spends to their canonical spend height, un-marks
     spends made by orphaned blocks that the chain has not replaced, fixes history
     rows the same way, and re-applies canonical blocks at heights an unrecording
     writer left wrong;
   - every count is surfaced in the run log; an error fails the run WITHOUT marking the
     index complete (the money path fails closed via the existing authority rule).
   - Provenance discipline: a row is only deleted/moved on POSITIVE proof from the
     chain (canonical block content or frame evidence); "cannot prove either way"
     leaves the row in place and is counted.

## Tests (new, in src/addrindex.rs::tests; fixtures in src/rpc.rs::tests)

- `orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_legacy_index` — the exact
  incident replayed on a pre-`blk` index: orphan indexed while tip, coin re-staked on
  the heavier branch, next run indexes the winner ABOVE the orphan. Asserts the defect
  (both stakes listed, balance DOUBLE), then the repair (phantoms gone, the re-staked
  coin spent by the CANONICAL re-stake, `blk` complete, idempotent second run).
- `orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_recorded_index` — same scene
  with recorded blocks + an unparented writer burying the orphan; also covers a missing
  `blk` row.
- `a_transaction_re_mined_on_the_winning_branch_keeps_its_output` — the re-mined tx is
  moved to its canonical height, not deleted.
- `the_daemon_never_writes_above_an_orphan_or_twice_at_one_height` — the production
  `LiveIndex::apply` path with real blocks: refuses above an orphan, refuses a second
  block at a height, applies normally after the builder repaired, and is a no-op when
  the builder already holds the block.
- `a_half_recorded_index_is_audited_below_its_first_recorded_block` — the
  range-floor race: a new writer stamps ONLY the tail block (h13) over a fully
  legacy, poisoned index. The naive floor `max(base, first_recorded_height)`
  would audit nothing; the audit must start at the chain's base or the
  phantoms survive forever behind one recorded block. Fails on the old floor
  (mutation evidence ev_mutation_race_floor.txt: 0 passed; 1 failed) and passes
  on the fix.
- rpc.rs fixtures now chain their test blocks (prev_hash = parent's real hash) — the
  production invariant my guard enforces; the old fixtures only worked because nothing
  checked parents. The mid-apply-rollback test now also asserts the failure IS the
  tx_addr write. The interleave test gained a writer-progress counter so it cannot pass
  vacuously.

### CMD: cargo test --lib -- addrindex:: (evidence file ev_addrindex_tests.txt)
    test addrindex::tests::eof_stamp_keeps_the_previous_pair_when_nothing_was_accepted ... ok
    test addrindex::tests::authority_with_lag_serves_a_bounded_lag_on_the_same_chain ... ok
    test addrindex::tests::eof_stamp_pairs_the_last_accepted_block_with_its_own_height ... ok
    test addrindex::tests::eof_stamp_prefers_the_accepted_tail_over_the_last_slice ... ok
    test addrindex::tests::insert_spend_and_query ... ok
    test addrindex::tests::a_transaction_re_mined_on_the_winning_branch_keeps_its_output ... ok
    test addrindex::tests::eof_stamp_written_to_the_db_passes_the_lag_aware_verdict ... ok
    test addrindex::tests::orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_legacy_index ... ok
    test addrindex::tests::orphaned_mint_beneath_a_canonical_tip_is_removed_from_a_recorded_index ... ok
    test addrindex::tests::the_daemon_never_writes_above_an_orphan_or_twice_at_one_height ... ok
    test addrindex::tests::a_half_recorded_index_is_audited_below_its_first_recorded_block ... ok
    test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 947 filtered out; finished in 0.08s

### CMD: cargo test --lib (whole crate, evidence file ev_lib_tests_full.txt)
    test result: ok. 957 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 125.25s
    test result: ok. 958 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 160.21s   (after the race fix + its test)

### CMD: mutation run 1 — the daemon guard neutralized, same test (evidence file ev_mutation_guard_removed.txt)
    test addrindex::tests::the_daemon_never_writes_above_an_orphan_or_twice_at_one_height ... FAILED
    thread 'addrindex::tests::the_daemon_never_writes_above_an_orphan_or_twice_at_one_height' panicked:
    must refuse to write above an orphan: ()
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 957 filtered out
    (source restored byte-identical afterwards; the un-mutated tree passes the test)

### CMD: mutation run 2 — the audit floor reverted to the racy max(base, first_recorded) (evidence file ev_mutation_race_floor.txt)
    test addrindex::tests::a_half_recorded_index_is_audited_below_its_first_recorded_block ... FAILED
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 958 filtered out
    (source restored byte-identical afterwards; the fixed tree passes)

### CMD: cargo build --release --bins
    Finished `release` profile [optimized] target(s) in 2m 44s   (release_build_rc=0)
    rebuilt after the race fix: release_build2_rc=0   (addrindex_build sha256 prefix 3ddece55a6ca0423)

## Live evidence on DNS2 (disclosed accident + result)

DISCLOSURE: building the release binaries replaced
`/root/tri7/target/release/addrindex_build`, which the `tri7-addrindex.timer` unit
executes directly (ExecStart=/root/tri7/target/release/addrindex_build ...). The 10-min
timer therefore ran the new binary BEFORE this brief was judged or the change committed.
The run's effects are exactly the designed verify-and-repair, the index DB carries
them, and the unit test suite was green before it fired. This is an operational hazard
worth fixing separately (a unit that runs a build-tree path makes every local release
build an instant deploy); it is NOT changed in this item. All later deploys
(node-binary swap, Pi/DNS3) happen only after this gate.

### CMD: journalctl -u tri7-addrindex.service (first run of the new binary, 00:16:55 PST)
    addrindex_build[2940526]: store threaded to h2220119
    addrindex_build[2940526]: address index verify: h2215519..h2220115 held another branch's content from h2215519 — repaired: removed 68 phantom output(s), moved 4 output row(s) to their block's height, fixed 63 spend marking(s), removed 45 and moved 0 history row(s), wrote 4597 missing block(s)
    addrindex_build[2940526]: addrindex: done — 156 blocks, 7 outputs, 6 spends, 4 stale fork leftover(s) skipped, 0 stale height(s) skipped, 152 block(s) already held in 0.5s

The repair found 68 phantom outputs (not just the 2 known ones — the older-window
suspects were real), fixed 63 spend markings to canonical heights, and recorded all
4597 window blocks. Runtime cost: the legacy-index first run paid ~9 min of extra I/O
for the window (one-time); the next runs are instant:

### CMD: journalctl -u tri7-addrindex.service (next run, 00:58:46 PST)
    addrindex_build[2974835]: address index verify: h2215541..h2220135 — every height holds the chain's block
    addrindex_build[2974835]: addrindex: done — 178 blocks, 10 outputs, 10 spends, 4 stale fork leftover(s) skipped, 0 stale height(s) skipped, 172 block(s) already held in 0.8s

### CMD: RPC on DNS2 after the repair (127.0.0.1:19112, creds redacted)
    getblockcount → 2220143
    getbalance → 5169.708509
    listunspent → 397 rows, sum 5169.708509 (internal consistency: balance == row sum)
    rows in h2219900..h2220000 → 2 rows: (…8aab1b921782, vout 2, 6.201403, h2219975),
    (…90076a1e1f15, vout 2, 6.5415, h2220000) — the two phantom rows of block 2219981
    (6.3 and 6.30871) are GONE. The item's named 12.608710 TRI over-report is closed
    at the index layer: the repaired index no longer vouches for them, and
    listunspent no longer serves them.

## Post-repairwallet live verification (row-by-row, the item's acceptance check)

`repairwallet` was called on the live node (01:25:02 PST, took 2m10s):
`{"wallet check passed":true}`. Fresh `listunspent` [1,9999999] + a
row-by-row `gettxout [txid, n, False]` cross-check of every served row (the
script re-reads the tip and distinguishes a true `result: null` from the
node's honest error `gettxout: … below the live-view window; spentness is
unresolvable`):

### CMD: RPC row-by-row cross-check after repairwallet (01:27–01:33 PST)
    getbalance → 5169.726394 == sum(listunspent 397 rows) (internal consistency)
    tip → 2220155 (sweep window 2220155..2220159)
    397 rows total:
      167 rows → gettxout answers a coin, height consistent with the row
       12 rows → gettxout result null, ALL in-window (h2220014..h2220061),
                 sum 114.171970 TRI
      218 rows → gettxout ERR "spentness unresolvable below the live-view window"
                 (h2031567..h2215518 — below the window; unauditable by RPC, NOT
                 evidence of phantom: gettxout refuses to guess either way)

The 12 true-null rows are outputs of blocks orphaned in the last ~100 blocks:
they are NOT in the address index (gettxout's index path misses them — the
repaired index never carried or already removed them) and not in the store's
canonical tx index; they exist only in the node's IN-MEMORY wallet live view,
which accumulated them at fork adoption and which `repairwallet`'s "wallet
check" does not test for non-canonical rows. That is a SEPARATE pre-existing
defect in a different layer (node_embedded's wallet live view), newly made
visible by this audit, and is recorded as the next workqueue item with its
own evidence rather than silently folded into this one. The item-52 defect —
orphaned outputs persisted by the ADDRESS INDEX — is fixed and verified: the
index no longer carries them, and the 12.608710 TRI rows that named the item
are gone from the served money view.

## What is NOT yet done at gate time (follow-ups, in order)

1. The node's wallet live view (in-memory) still accumulates raced-mint
   outputs on fork adoption (12 rows / 114.171970 TRI right now). Needs the
   node-side fix (drop replaced-branch outputs at adoption, or re-seed from
   the index) + the node-binary swap — deliberately deferred to be bundled
   with the next node-binary deploy so the sole staker pays ONE restart, not
   two. Recorded as the next workqueue item with the row-level evidence.
2. The node binary (node_embedded/rpc embed LiveIndex) is still the old build
   on all nodes; the daemon-side guard in this change needs the binary swap
   (tri7d_swap_verify_rollback harness, DNS3 canary → Pi → DNS2). Until then
   the 10-minute catch-up builder re-repairs anything an unguarded writer
   writes wrong (proven: the 00:58 run found "every height holds the chain's
   block" while the old node binary was still running).
3. Older-history source below the window (218 rows unauditable via gettxout):
   out of scope here; the window is 4600 blocks on DNS2 and every phantom the
   repair found lived inside it.

## Honesty statements

- The root cause is a hypothesis STRONGLY supported by code reading and by the live
  repair output (68 phantoms = orphaned own-mint outputs across the window, exactly
  the predicted shape), but the exact 03:37:54Z indexing run was not re-observed.
- No claim that the pre-fix balance was ever spendable-safely; the item text says not
  to call it safe and this fix does not rely on that.
- An intermediate sweep of mine reported 96 "height mismatches" (rows 2 above the
  computed height); those were MY stale-tip artifact — every mismatch was exactly
  +2, the number of blocks the tip advanced during the 4-minute sweep. The final
  sweep (above) re-reads the tip per batch and reports zero mismatches. Not a node
  defect; recorded so the artifact is not mistaken for one.
- The 218 below-window rows are UNAUDITED, not verified-good: gettxout's honest
  refusal is not evidence either way. Item 52's scope was the index window; the
  below-window audit is follow-up work.
- The 12 in-window residual phantom rows live in the node's in-memory wallet live
  view, a layer this change does not touch; they are disclosed above rather than
  hidden inside a green suite.
