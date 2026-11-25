# Brief — tri7 item 48 BALANCES-REPLAY-LOSER (round 1): chain-ordered balances replay + guarded fleet deploy

Round cap set before round 1: **3 rounds for this change** (code + deploy plan; deploy evidence; one spare). B is the bar.

## 0. What is being graded

An **uncommitted** working tree in `/root/tri7` (branch `master`, HEAD `c91b076`, remote `github.com/SamiAhmed7777/tri7`):
6 files, +1,210 / −15. The commit follows the grade by design. Sources were built and tested as they stand
(`source_sha256.txt` ties the bytes to the binaries that produced the transcripts).

Two decisions are requested:

1. May this be committed to `master` and pushed?
2. May it be deployed to three live nodes — DNS3, Pi, DNS2 — by the procedure in §7 (binary swap, unit restart,
   automatic rollback if the acceptance check fails)?

Scope: the `--balances` snapshot builder (`balances.json`) only. No consensus, P2P, staking, wallet or
store-write code is touched. `src/node_embedded.rs` is 31k lines; only the hunks in `item48.diff` changed.

Files (worktree names carry a numeric prefix, e.g. `2_item48.diff`):

| file | what it is |
|---|---|
| `item48.diff` | `git diff -U10` of all six files — the whole change with context |
| `balances_build.rs` | whole final file (the core of the change) |
| `balances_chain_probe.rs` | whole final file (rehearsal harness, `examples/`) |
| `tri7d_swap_verify_rollback.sh` | whole final file (deploy harness, `deploy/`) |
| `balances_chain_ordered.py` | whole final file (acceptance check, `deploy/checks/`) |
| `ev_before_dns2_journal.txt`, `ev_before_dns3.txt`, `ev_before_pi.txt` | the defect as observed on the three nodes before any change |
| `ev_rehearsal_dns2.txt` | A/B on one archive copy: index order vs file order |
| `ev_preflight_dns3.txt` | index-order rehearsal on DNS3's own archive copy |
| `ev_crosscheck_supply.txt` | independent check of both rehearsals against the chain |
| `ev_battery.txt` | `cargo check --all-targets`, `cargo test --lib`, release build — final sources |
| `ev_tests_balances_build.txt` | the balances_build tests inside that test run |
| `ev_check_selftest_r2.txt` | the acceptance check's 15-case selftest + its live negative test (round-2 version) |
| `ev_check_selftest_r5.txt` | the acceptance check's 19-case selftest on the FINAL bytes (round-5 version) |
| `ev_deploy_dns3.txt` | the live DNS3 canary attempt: rejected and rolled back cleanly — and the journal that taught the live-tip semantic |
| `ev_harness_rehearsal_r4.txt` | the deploy harness on its round-4 FINAL bytes, every branch, incl. T9 boot-refusing binary and T10 failing restart command |
| `judge_round1.log`, `r1_verdict.json` | round-1 verdict (D) and its two blocking issues, both fixed |
| `r2_verdict.json`, `r3_verdict.json` | round 2 (B) and round 3 (A) verdicts and their polish items |
| `source_sha256.txt` | sha256 of the six files being committed |

No credentials, keys or addresses are in any of these files.

## 1. The defect, as observed (old binary, all three nodes)

Each node runs `--balances` and rebuilds `balances.json` from its block archive. On every node the build
"succeeds" but stops at a losing fork block, so the builder retries forever (`ev_before_*.txt`, verbatim journal):

- **DNS2** (`vmi3080415`, binary sha256 `92235712…`): every one of the 51 builds in the retained journal
  (6 h 23 m) ended `coverage h2208645 tip 02d723be…`, 41 × `coverage h2208645 short of the chased tip h221929x — retrying in Ns`.
  The real tip was about h2219300: **10,723 blocks missing** from the snapshot.
- **DNS3** (`appt`, same binary): `coverage h2208246 tip 203294d2…` every 1800 s (the backoff cap) — 11,122 blocks missing.
- **Pi** (`ubuntu-4gb-hel1-1`, aarch64, sha256 `ea0d2546…`): `coverage h2208649 tip a55aa6d3…` every 1800 s — 10,719 blocks missing.

Consequences: `richlist`/supply/wallet figures served from the snapshot are stale by ~11k blocks and stamped
with a block that is not in the chain (DNS2 RPC right now: `moneysupply 226842.485015`, i.e. 150,112.189 TRI below the
chain's figure, reported as covered at the current tip); and ~90–100 s of replay CPU is burned per retry.
On DNS2 the loop also went blind: the 04:12:34Z start line read `chain tip h2208645` (the snapshot's own height) at a real
tip of h2219300, which counted the short walk as converged and put the builder into its 6-hour sleep.

## 2. Root cause at HEAD `c91b076`

`src/replay.rs` `ArchiveWalker::step()` walks the archive **in file order** and returns `StaleBranch(h)` at the first frame
whose `prev_hash` is not the previous frame's hash. The balances build (HEAD `src/balances_build.rs` ~line 161) stops there.
Each node's archive holds a losing fork block **one frame ahead of the winning child of the same parent**. So the walk applies
the loser, meets the winner on the next frame, treats the *winner* as a stale branch, and ends with the loser as the snapshot
tip. Retries read the same frames in the same order, so the result can never change. Three nodes, three different
loser-first positions (h2208246, h2208645, h2208649).

## 3. The change

**`src/balances_build.rs`** (+670 / −8)
- `pub trait CanonicalBlocks { fn block_at(&self, h) -> Result<Option<(Block, Hash)>, String>; fn chain_top(&self) -> Option<u64> { None } }`,
  implemented for the node's `crate::blockindex::BlockStore` (its height → block map, i.e. the node's own fork-resolved chain).
- `BalancesBuildOpts::canonical: Option<&dyn CanonicalBlocks>`. With `Some(chain)`, blocks are pulled **by height** through
  `pull_indexed`: every body is chain-checked against the one before it (genesis must have `prev == 0`); a height that returns
  nothing at or below `chain_top()` (or a row that does not chain) **caps** the walk — `Pulled::Capped(h)` plus a `cap_note` —
  and nothing above it is applied; nothing above the top is the clean end of the chain. Branches are never spliced.
- Report gains `replay_order` ("index" | "file") and `cap_note: Option<String>`; the coverage contract reports a capped walk as an error.
- With `canonical: None` the old file-order path is kept unchanged.
- 10 new tests, among them: file order follows the loser at a loser-first fork; index order threads the winning branch past it;
  the node's real `BlockStore` is a chain source at such a fork; index order is byte-identical to file order on a fork-free
  archive; hole handling with and without a known top; a source that does not chain is refused; a chain that advances mid-walk.

**`src/node_embedded.rs`** (+88 / −7): the builder thread passes the block store unless `--balances-file-order` is given
(new opt-out flag; the standalone CLI and the opt-out keep file order). It waits for the block index instead of counting a
warming index as a build error, re-reads the chain tip a second later when the follow loop holds the state lock (instead of
falling back to the snapshot's height — the bogus-target bug), and logs the order, any cap, and the index status on failure.
The snapshot **file format is unchanged**.

**`src/bin/balances.rs`** (+4): `canonical: None` — the standalone CLI owns no chain map.

**`examples/balances_chain_probe.rs`** (new): builds the production `BlockStore` in-process over copies of
`blk0001.dat` + `tipstate.json` and runs the chain-ordered replay — a rehearsal that does not touch a live node.

**`deploy/tri7d_swap_verify_rollback.sh` + `deploy/checks/balances_chain_ordered.py`** (new): the deploy harness of §7.

## 4. Evidence

- `ev_battery.txt`: `cargo check --all-targets` exit 0 (170 warnings, 0 in touched files) · `cargo test --lib`
  **937 passed, 0 failed, 1 ignored** · release build OK.
- `ev_rehearsal_dns2.txt` — one archive copy (sha256 `8ad479eb…`), same final sources, two orders:
  - **index order**: 2,219,351 blocks, tip `72c64e30…` at h2219350 = the tip it chased, `cap_note null`, supply 226,992.597204 TRI.
  - **file order** (what every node does today): 2,208,646 blocks, tip `02d723be…` (the loser), supply 226,842.485015 TRI —
    the same figures the old daemon publishes.
- `ev_preflight_dns3.txt`: index order on **DNS3's own archive copy** (its fork is at a different height): 2,219,378 blocks,
  tip `dd330e75…` at h2219377 = chased tip, `cap_note null`. Live node untouched: same PID, no index-cache file written.
- `ev_crosscheck_supply.txt`: both rehearsal tips equal `getblockhash` on the live chain, and
  supply(DNS3 h2219377) − supply(DNS2 h2219350) = 114,860 sats = the tx-by-tx sum of (outputs − spent inputs) over blocks
  h2219351..h2219377 from the RPC. Two archives, one chain, exact agreement.
- Timings: index build 74–75 s; chain-ordered walk 92–110 s (file order on the same data: 65–100 s on the live nodes).

## 4b. The push gate for the Windows CI job

The repo's rule (triangles-modernization skill) is that a Windows cross build must be zero-error before pushing.
`cargo check --target x86_64-pc-windows-gnu --all-targets`: exit 0, no new warnings in the touched files
(`ev_win_check.txt`); the release build of all bins for that target is running for the record
(`ev_win_build.txt`).

## 5. The acceptance check (`deploy/checks/balances_chain_ordered.py`)

Reads the journal of the unit's **current** main process and the snapshot file. Verdict of the **last** build that
**finished** (a build that landed short of the tip is superseded by its own retry, which starts from the new snapshot).
A failed attempt whose retry is still running, and a walk that capped where the store's bodies end, are *pending*, not
rejections (the scheduler retries; the harness deadline is what turns a chain of failures into a rollback). Accepts when
the finished build replayed in **index order** and `coverage h ≥ the tip it chased` (the chain may advance while the walk
runs), and the snapshot file agrees with the journal line exactly: `height` = the last block applied,
`covered_tip_hash` = its hash, `replay_target_height` = **the tip the build was chasing** (the start line's second
height) — the snapshot's own semantics, pinned in `src/balances_build.rs`. Rejects a finished build in file order, one
that landed short of the tip **with no retry scheduled**, or a disagreeing snapshot. `--selftest`: 21 cases
(`ev_check_selftest_r5.txt`), including the chain-advanced-mid-walk acceptance, the wrong-`replay_target_height`
rejection, the failed-then-retried acceptance, the two live-DNS3 shapes from §11, and the two mismatched-retry
rejections (a different build's retry line must not mark the judged build pending). Live negative test on DNS2 with
the old binary: **rejected** (a finished build, in file order). Limitation, stated in the file: a node whose snapshot is
already fresh builds nothing after a restart, and the check stays pending. All three nodes are >10k blocks stale, so
each builds on its first boot.

## 6. The deploy harness (`deploy/tri7d_swap_verify_rollback.sh`) — exercised before any fleet use

Swap by rename (the running process keeps its inode), restart, poll the check (exit 3 = pending), and on anything but accept
put the previous binary back and restart. **A restart that fails after the swap is caught, not left to `set -e`**: the
failure becomes a rollback (verified by rehearsal with a binary that refuses to boot — T9 — and with a PATH shim that
fails the `systemctl restart` command itself — T10 — `ev_harness_rehearsal_r4.txt`). The rollback is judged by a final
readback (previous binary running, unit active); a step that cannot be verified says so and the outcome line is
`ROLLBACK-FAILED`. **Acceptance is re-verified at the moment of the verdict**: the unit must be active *and* running
the new binary, otherwise the deploy is rejected and rolled back — added after a rehearsal run exposed the race in which
a check can pass a moment before a crashing process exits; a guard-triggered rollback reports the guard's reason (the
previously built reason no longer leaks into the report). Preconditions that change nothing except a saved
`<bin>.prev-item` copy (stated as such in the header):
same binary already in service, in-place build without a saved previous binary, unit not active. Full rehearsal, every
branch, state read back after each: accept · forced rollback · pending-to-deadline · immediate reject · in-place accept ·
in-place reject · pending-then-accept · the two failure branches above · three precondition exits. `tri7d` itself:
untouched, same PID before and after.

## 7. Deploy plan (to run after this grade, DNS3 → Pi → DNS2)

Per node, as root on the node, output captured to `ev_deploy_<node>.txt`:
1. Run the check against the still-running old binary: it must **reject**.
2. DNS3 only: `--rehearse-rollback` on the real target — real swap, real restart, forced rejection, previous binary back.
3. Stage the new binary. DNS3 and DNS2: the `follow_tip` built here (sha256 `60ee7604…`; DNS2 and DNS3 are both
   Ubuntu 24.04 / glibc 2.39) copied to `/tmp/item48-deploy/`. Pi: rsync the committed sources and build natively
   (`cargo build --release --bin follow_tip`, in place), after saving the previous binary for `--prev`.
4. Run the harness: swap → `systemctl restart tri7d` → check, 900 s deadline → accept, or automatic rollback.
5. Read back: running binary sha256, restart count, the first `done … index order` line (coverage = chased tip), the
   snapshot file, RPC blocks, staking live. Cross-check each node's snapshot tip against `getblockhash` on DNS2.
6. After all three: no `short of the chased tip` line on any node since its restart; the three nodes agree on height.

Cost per node: one restart (DNS3: two). Stops run to the 90 s stop timeout (pre-existing), then ~75 s index build and
~100 s first build: about 4–6 minutes not staking. DNS2 is the sole heavy staker, so it goes last.
Later rollback by hand: `mv -f <bin>.prev-item <bin> && systemctl restart tri7d`.

## 8. Not claimed / known limits

- **Not deployed yet.** This round grades the code and the procedure; the deploy transcripts follow in the next round.
- The chain-ordered walk reads through the block index's locks (read side). Contention with the follow loop was not
  measured; the walk takes 92–110 s, the same order as the file-order walk it replaces.
- The standalone `balances` CLI still replays in file order and therefore still ends at a loser on such an archive (it has
  no chain map). It prints the tip it reached; unchanged by design.
- The Pi (aarch64) was not rehearsed before its deploy: its binary must be built on the Pi. Its journal shows the same defect.
- sami-pc runs the CI artifact and is not part of this procedure.
- Expected visible effects after deploy: each node's `moneysupply` rises to the whole-chain figure (DNS2: +150,112.189 TRI),
  DNS2's snapshot wallet balance gains the staking rewards of the missing blocks, `richlist` entries change accordingly.
- Observation, not changed: `getblockchaininfo.moneysupply_coverage_height` reports the live tip next to the snapshot's figure
  (`src/node_embedded.rs` ~9680). With whole-chain snapshots the two differ only by the refresh lag.
- No formatting pass was run over the two large pre-existing files (`cargo fmt --check` already reports diffs on them at HEAD).

## 9. Round 1 went D — both blocking issues are fixed and rehearsed

Round 1 (Codex, `judge_round1.log`, `r1_verdict.json`) graded D on two real defects in the deploy machinery, both
in the new deploy files, neither in the replay fix itself:

1. **The acceptance check mis-handled a chain that advances during the walk.** It required
   `replay_target_height == covered`, but the snapshot writes the *chased* tip there and `covered` can legitimately
   exceed it (the Rust test `index_order_follows_a_chain_that_advances_mid_walk` covers exactly that shape). A valid
   build of the new binary on an advancing chain would have been **rejected and rolled back** on deploy. **Fixed:** the
   comparison is now `height == covered`, `covered_tip_hash == journal tip`, `replay_target_height == the start line's
   chased tip`, with `covered >= chased` accepted; 15 selftest cases include the advancing-chain acceptance and the
   wrong-field rejection (`ev_check_selftest_r2.txt`).
2. **The harness ran `systemctl restart` under `set -e` without trapping failure.** A restart that fails after the swap
   would abort the script, leaving the new binary in place with the unit down — the opposite of the promised rollback.
   **Fixed:** the restart failure is caught and routed through the rollback, and the rollback reports its two steps
   separately (a `ROLLBACK-FAILED` line if either does not verify). **Rehearsed on the failure branch** with a binary
   that refuses to boot (T9: rollback restored the previous binary, unit active again) and with a PATH shim whose
   `systemctl restart` exits 1 (T10: no `set -e` abort, swap undone, honest report) — `ev_harness_rehearsal_r2.txt`.

Both polish items were applied too (the `<bin>.prev-item` save is now labeled as the one mutating precondition step;
a selftest case for a failed-then-retried build was added — it is judged on the retry, a design decision now stated in
the check's docstring).

## 11. The live DNS3 canary: rejected-and-rolled-back cleanly, and the journal taught one more semantic

The canary ran the full chain: negative check on the old binary (rejected, as designed) → forced-rollback rehearsal on
the real target (previous binary back, unit active) → the real deploy. The new binary's **first index-ordered build
finished in 103.9 s** — coverage h2219460, up from the file-order snapshot's h2208246 — but 2 blocks short of the chased
tip h2219462, because on a live node the block store's **bodies lag the moving tip** (`chain-ordered walk stopped at
h2219461 — the chain store holds nothing at h2219461 although its chain continues above it`). The node itself logged
`coverage h2219460 short of the chased tip h2219462 — retrying in 60s` and began its incremental retry from the NEW
snapshot 60 s later (`start (snapshot h2219460, chain tip h2219464)`, `ev_deploy_dns3.txt`). The canary was rolled back
before that retry could write its own done line, so the live transcript shows the retry **scheduled and started**, not
its completion; the converged-retry shape is covered synthetically in the selftest, and the re-deploy's own acceptance
(the check returning exit 0) is what will demonstrate it live. The check of round 4 classified that healthy intermediate
state as a **rejection** — safe, but wrong. **Fixed:** a finished build short of the tip is *pending* when the node's own
retry line answers **that build** — the line carries the build's coverage and chased heights, and the association is
height-matched (a retry line with other heights, or seen before the judged done line, belongs to a different build and
does not count, covered by two selftests); a newer build's start line resets the verdict to "the build is running".
The verdict is taken from the **last** finished build, so the converged retry is what gets accepted. Reject only when
short coverage comes with **no retry scheduled for that build**. 21 selftest cases including the live shapes verbatim
(`ev_check_selftest_r5.txt`). The harness is unchanged: it already polls pending verdicts; its deadline remains the
backstop that turns a non-converging chain into a rollback.

## 12. Answer

Grade this round against the two decisions in §0. What changed since round 4 (grade A) is exactly one file,
`deploy/checks/balances_chain_ordered.py` (plus this brief and its evidence): the live-DNS3 semantic of §11. The
replay fix, the harness, the binaries and all their evidence are byte-identical to round 4. Block on what would
make the commit or the deploy unsafe or wrong; wording and evidence packaging are polish.
