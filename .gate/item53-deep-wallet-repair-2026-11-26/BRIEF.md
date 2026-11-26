# BRIEF — item 53 of the TRI7 parity queue, fix F1: deep wallet-row repair of the address index

Round 2. A CODE review of an UNCOMMITTED change. Nothing here is committed, pushed, deployed or run against the live index yet; the live dry run and the apply are a separate judge round with their own evidence.

Round 1 graded this artifact B with no blocker and four polish items; all four are addressed below (section 9), each with a test where a test is possible.

## 1. What is being graded

Repo `/root/tri7`, branch `master`, HEAD `815b467` (== origin/master, the item-52 commit). Uncommitted, and exactly these two files:

```
src/addrindex.rs           | 865 ++++++++++++++++++++++++++++++++++++++++++++-
 src/bin/addrindex_build.rs | 319 +++++++++++++++--
 2 files changed, 1159 insertions(+), 25 deletions(-)
```

The full unified diff is attached as `item53_changes.patch` (sha256 `f3a8e3c4eef81c5a…`; the same bytes the test suite below ran on).

## 2. The defect this fixes (measured read-only on DNS2 by the 0224PST tick, at tip 2220225)

The node's address index (`/var/lib/tri7/addressindex.db`) holds, for the staker wallet's addresses, rows that came from OFF-CHAIN blocks:

- Of 391 `listunspent` rows / 5169.878454 TRI: 166 in-window rows / 1587.408721 TRI are canonical. Of the 225 below-window rows / 3582.469733 TRI, **218 rows / 3581.961062 TRI (69.3% of the total) sit in 159 transactions whose blocks are off the active chain** (159/159: `getblock` by hash answers `confirmations: null`; DNS3 agrees). They attach at canonical h2209506..h2214029 with orphan depth 1..35. Only 7 rows / 0.508671 TRI of the below-window set are in the canonical block at their height.
- The flip side: those orphan transactions consume **225 wallet coins / 3044.226122 TRI that `listunspent` does NOT show**. For 204 of them / 2774.368276 TRI the index's only spend record is the orphan block's own height (a phantom spend marking); 21 / 269.857846 TRI carry a different spend height (not classified). That number is a FLOOR: the probe only recognised the 19 addresses still present in `listunspent`.

Cause: the index was built before item 52 by walking the block store in FILE order, and every frame that had a height got its rows — the losing branch of a fork included. Item 52's repair (`verify_and_repair`) proves only the window the tipstate oracle vouches for (h >= 2215519 at the time; 4600 entries). Nothing below it was ever checked, so item 52's live repair could not reach these rows.

## 3. What the diff does

### 3.1 Library — `addrindex::deep_wallet_repair(conn, chain, addrs, from, apply, intact) -> Result<DeepReport, String>`

**Scope.** Only the addresses in `addrs` (deduplicated; sorted). Nothing else in the index — other addresses, `blk`, `meta` — is written.

**Snapshot.** The scope's `out` rows and `tx_addr` (history) rows are read inside one read transaction that is always rolled back (`BEGIN` … `ROLLBACK`), in both modes, so the dry run leaves no trace.

**The walk.** From `from` to `to = min(index coverage, chain tip)`, height by height, each block fetched as `chain.block(h)` — the chain's own answer, never file order. From the walk it collects, for the scope: every canonical output (`created`: height, value, address, script), the lowest canonical height that spends each scope coin (`spent_at`), and the lowest canonical height of every transaction that pays the address or spends its coin (`history`, what `insert_block` records).

**The plan**, per row:
- a row above the walk (`height > to`) is left alone: the newest blocks reach the index before the store threads them, and "no canonical block" for a frame the store has not threaded must never be read as "orphan";
- a coin created on the walk: its height and value are set to the canonical output's, and its marking is set to what the walk saw (unspent, or spent at the canonical spend);
- a coin created below the walk's start is left alone (the walk cannot judge a spend it did not see);
- a row whose transaction is on no canonical block (`chain.tx_height` -> None) is **removed** — the orphan's own output (`phantom_outputs`, `phantom_value`);
- a row whose transaction is canonical below the start, or at the walk yet not an output of that address there, is moved to its transaction's height / left alone;
- a marking that names a block above the walk is left alone (the live chain's, not this snapshot's);
- a canonical output the index never held is written (`missing_outputs`), already marked spent if the walk saw its spend;
- history rows name canonical transactions only: an off-chain transaction's row is removed, a mis-heighted one is moved, a missing one is written.

**Dry run.** `apply == false`: the same walk, the same plan, and not one byte written. `unspent_after` is then *projected* from the plan.

**Apply.** `intact()` is asked once, right before the write; false -> `Err("the chain changed under the walk — nothing written; run again")`. Then ONE transaction, and every `out` write is conditional on the row still being what the snapshot saw (`WHERE rowid=? AND height=? AND (value=? AND) spent_height IS ?`); a row that changed meanwhile is counted in `raced` and never overwritten. Inserts are `INSERT OR IGNORE`. `unspent_after` is then measured from the index. Any error in the walk (`chain.block` -> None, a store error) aborts before a write.

**Report** (`DeepReport`, all public fields): from, to, applied, addresses, rows_before, rows_after, unspent_before, unspent_after, phantom_outputs, phantom_value, reheighted, values_fixed, markings_cleared, markings_moved, markings_set, restored_value, missing_outputs, missing_value, history_removed, history_moved, history_added, left_alone, raced.

`from == 0` means the wallet's oldest row (the lowest height among its output and history rows, 1 if none). If the wallet has no row at all, the function returns an empty report without walking (the ordinary build writes those, not this repair).

### 3.2 `CanonicalChain` gains `hash_at(height) -> Option<[u8; 32]>`

The chain's block hash at a height. The window repair's only implementor (`ChainView` in the binary) now answers it below the oracle's base through the store's own height map (`BlockStore::canonical_hash_at_height`), so the deep walk can go where the oracle does not reach. `ChainView::block` already refused a block that does not name the one before it (`prev_hash` linkage, item 52); that check now covers the deep walk too: a height map that is not a chain makes the walk fail closed.

`ChainView::tx_height` is unchanged in substance: a frame counts only where the chain's own hash at its height IS its block; an unresolvable hash still counts as canonical (the row is kept, never dropped on a failed lookup).

### 3.3 Binary — `addrindex_build <store> <state_dir> --deep-wallet-repair[=dry|apply] [--deep-from <height>] [--deep-wallet <file>]`

- Bare `--deep-wallet-repair` and `=dry` are the DRY RUN; `=apply` writes. There is no way to write without saying `apply`.
- `--deep-from <height>` starts the walk above the wallet's oldest row (0 = that row, the default). A start above where the walk ends is refused by the library: `the walk would start at hX, above where it ends (hY) — nothing written`.
- `--deep-wallet` names the wallet file (default `<state_dir>/wallet.dat`). Only `WalletKeys.keys[].address` is taken from it; key material is neither read nor printed. An unreadable wallet or one without key addresses: exit 2, nothing written.
- **Strict parsing** (`parse_deep_args`, unit-tested). Exactly these forms are accepted. Any other argument beginning with `--deep` stops the run with a usage error (exit 2), as does `--deep-from` / `--deep-wallet` without a value, a non-numeric height, or either one given without `--deep-wallet-repair`. A mistyped flag therefore cannot turn an intended dry run into a different run. Arguments that are not `--deep` ones are not this parser's (their handling is unchanged).
- `--full` together with the deep repair is refused (exit 2): `--full` removes the index the repair reads.
- The walk waits for the same store threading the ordinary run waits for, reads the tipstate oracle, and the deep block runs before the ordinary window verify, which still runs after it (so the window is judged even on a deep run).
- The report prints counts and sums in TRI, states "DRY RUN (nothing written)" or "APPLIED", and says whether "spendable after" is projected or measured.

## 4. Tests (7 new in `src/addrindex.rs`, 1 new in the binary)

The regression scene is item 52's style — the node's own losing branch appended BEFORE the winner: h1..=h10 pay the staker 1000 each; the orphans C1 ("h11", re-stakes coin 5) and C2 ("h12", re-stakes coin 6) are indexed while they are the tip; the heavier branch D0/D1/D2 then wins at h11..h13 and only h13 (the window) is vouched for. The index holds the orphans' rows and markings and no `blk` row, as the file-order catch-up left it.

1. `deep_repair_drops_a_forks_losing_branch_from_below_the_window` — THE regression. Spendable is (15 rows, 11_040) and should be (12, 10_020). Item 52's window repair is run first and leaves it exactly there (clean=false, 1 block reapplied, spendable unchanged). The dry run reports (from,to)=(1,13), 4 phantom outputs / 2_020, 1 marking moved, 1 cleared (1_000 restored), rows 18 -> 14, 2 history rows removed, 0 left alone, 0 raced, and writes nothing. The apply equals the dry plan field for field; coin 5's marking is h12 (the chain's own re-stake); the orphans' `out` and `tx_addr` rows are gone; a second run finds nothing to do.
2. `deep_repair_writes_what_the_index_never_held` — a canonical output the index lacked, a canonical spend it never marked, a history row it lacked: all written; spendable ends at the truth.
3. `deep_repair_writes_nothing_when_the_chain_moved_under_the_walk` — `intact()` false: `Err` containing "changed under the walk"; the index is byte-for-byte the legacy state.
4. `a_walk_that_starts_late_leaves_the_coins_it_cannot_judge` — from h11: the orphans' outputs go, coins 5/6 (created h5/h6) keep their markings; 20 rows left alone; a full walk then finishes the job.
5. `a_row_above_the_walk_is_left_alone` — a coin of h14 (the snapshot ends at h13) that the store has not threaded stays listed with its history row. This case was added while tracing the live daemon's write path: without the guard the row would have been dropped as a phantom.
6. `a_row_above_its_block_below_the_walks_start_is_moved_not_judged` — two real coins (created h3/h5) whose rows claim h9, on a walk that starts at h7. The height is set to each transaction's own; the coin whose spend the walk sees has its orphan marking moved to the canonical spend, the other keeps its (unmarked) marking. Each row counts once: `reheighted` 2, `markings_moved` 1, `left_alone` 10 (never both moved and left alone).
7. `a_walk_asked_to_start_above_its_end_is_refused` — `--deep-from 14` on a walk that ends at h13: `Err` naming both heights, the index unchanged; an address the index does not hold gives an empty report.

In the binary: `deep_flags_parse_strictly` — the three accepted forms, and seven refused ones (`=dry-run` typo, unknown `--deep-*`, a missing value, a non-numeric height, options without `--deep-wallet-repair`).

## 5. Measured results

All from the attached transcripts, which the test suite wrote on the diff in `item53_changes.patch` (diff sha256 `f3a8e3c4eef81c5a…`).

| Run | Command | Result |
|---|---|---|
| full lib suite | `cargo test --lib` | **965 passed; 0 failed; 1 ignored**, exit 0 (2026-11-26T15:57:21Z -> 2026-11-26T16:00:26Z) |
| binary's own tests | `cargo test --bin addrindex_build` | **5 passed; 0 failed**, exit 0 |
| addrindex:: subset | (inside the lib suite) | 18 tests, all ok — listed in `ev_addrindex_tests.txt` |

Mutation runs — each single change is made to the clean tree, the suite is run, and the tree is restored (sha256 verified equal after each). The suite killed all four, each by the test that exists for it:

| Mutation | What it breaks | Test that fails | Exit |
|---|---|---|---|
| no_drop | the plan no longer queues a phantom row for deletion | `deep_repair_drops_a_forks_losing_branch_from_below_the_window` | exit 101 |
| no_intact_check | the apply path no longer asks `intact()` | `deep_repair_writes_nothing_when_the_chain_moved_under_the_walk` | exit 101 |
| dry_run_writes | the dry run takes the write path | `deep_repair_drops_a_forks_losing_branch_from_below_the_window` | exit 101 |
| no_above_guard | a row above the walk is no longer left alone | `a_row_above_the_walk_is_left_alone` | exit 101 |

## 6. What this change does NOT do, and where it can be wrong

- **Trust assumption.** Below the oracle's window the walk trusts the store's height map: frozen at build time, and only frames the threading chained by `prev_hash` hold a height. The walk enforces the linkage block to block and fails closed if it is broken, but the map itself is not cross-checked against a second source. A wrong height map below the window would make the repair wrong in the same way.
- **Scope.** `WalletKeys.keys` addresses of the named wallet file. P2SH / multisig `scripts` and a `default_key` that is not among the keys are not in scope.
- **`left_alone` counts rows, or a row's spend marking, that the walk cannot judge.** Coins created below `from`, rows and markings above the walk, and transactions canonical at the walk but not an output of that address there are left as they stood. With the default `from` only the "above the walk" class exists in practice. A wallet coin OLDER than the wallet's oldest index row and absent from the index is never seen, so it is not added. A marking the walk did not see is never rewritten, but a height the walk does know (its transaction is canonical, below the start) is still corrected — that row counts as moved, not as left alone.
- **`unspent_after` on a dry run is projected** from the plan, not measured.
- **Below-window reorgs during the run** are not covered by `intact()`: it re-reads the tipstate oracle, which vouches for the window only.
- **History deletes** are by rowid and not conditional; history rows are append-only in this index.
- **`tx_height`'s existing lenience stays**: a frame whose height the chain cannot resolve counts as canonical, so a failed lookup keeps a row rather than dropping it.
- **Not exercised at scale here.** The unit scene is 13 blocks. Walk time at mainnet size (the walk starts at the wallet's oldest row, which may be far below the tip) is first measured by the live dry run.
- **The command-line flags are unit-tested but not yet run for real**; the live dry run is their first real exercise.
- **Not in this change**: F3 (`listunspent` freshness lag E2; incremental un-apply at fork adoption E3), E6 (the seed walk's file-order phase), A5 (the Pi/DNS3 node-binary swap).

## 7. Deployment (NOT done; a separate judge round on the dry-run output gates it)

On DNS2 only (the only fleet host with an address index): stop `tri7-addrindex.timer` (it runs `target/release/addrindex_build` directly, so a release build IS the deploy there); `cargo build --release --bin addrindex_build`; run the DRY run and read its counts; back up `addressindex.db` (with `-wal`/`-shm`) and `balances.json`; `=apply`; verify with the item-53 audit (`item53_audit.py`: every `listunspent` row is in the canonical block at its height -> 0 misses) and by seeing the restored coins appear; start the timer. The report will state the before and after numbers. No daemon restart.

## 8. Files in this bundle

- `BRIEF.md` — this file.
- `item53_changes.patch` — the whole change under review.
- `ev_addrindex_tests.txt` — the addrindex:: lines of the full lib run.
- `ev_lib_tests_full.txt` — the full lib suite transcript.
- `ev_bin_tests.txt` — the binary's tests.
- `ev_mutation_no_drop.txt`, `ev_mutation_no_intact_check.txt`, `ev_mutation_dry_run_writes.txt`, `ev_mutation_no_above_guard.txt` — the mutation runs.

## 9. Round 1 polish items and what changed

| Round 1 item | Change | Evidence |
|---|---|---|
| a row counted as both `reheighted` and `left_alone` in the canonical-below-start branch | The row now counts once: as moved when its height changes, as left alone only when nothing about it changes | test 6 |
| height/value edits pushed for a row whose marking is left alone above the walk | Kept on purpose (the height is the walk's to correct; the marking is carried through unchanged and the write stays conditional on it) and now stated in the code and in `DeepReport::left_alone`'s doc | code comment, section 6 |
| no parsing errors for unknown `--deep-*` flags; `--deep-from` above the coverage | `parse_deep_args` refuses any unknown `--deep` argument, a missing or bad value, and options without the repair flag; the library refuses a start above the walk's end | `deep_flags_parse_strictly`, test 7 |
| no test driving the flag parsing | the parsing is a function with a unit test | `deep_flags_parse_strictly` |
