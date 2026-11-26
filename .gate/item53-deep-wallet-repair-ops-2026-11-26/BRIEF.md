# BRIEF — item 53 of the TRI7 parity queue, fix F1: APPLYING the deep wallet-row repair on DNS2's live address index

## 1. What is being graded

An OPERATIONS decision, not code. The code is committed and pushed (7d0793f, master) and the stand-in judge graded that exact diff A in round 2 (urn:ump:2dbhvyslrfq7ffawmixnvcz23ldydy4ebu7peuf6z3lzqgdokxva), after a B in round 1 (ev_code_grade_r2.txt). What is proposed now is to run ONE command, once, on the live address index of DNS2 — the staking node's wallet index that `listunspent` is served from:

    addrindex_build /var/lib/tri7/blk0001.dat /var/lib/tri7 --deep-wallet-repair=apply

The full plan — preconditions, the write, the verification, the rollback, the limits — is `ev_plan.txt`; grade THAT. A or B means "run it as written". C, D or F means "do not run it until what you name is fixed".

This is ROUND 2 of this grade. Round 1 graded the plan A (urn:ump:qjnkiza73vkymm2sxzaajneerfu5wznkecblz6kn3yr7lcqymnxa) with four non-blocking suggestions; all four are addressed in section 9. The rollback script and the plan changed to carry them, which is why the changed files are graded again here. Nothing has been applied to the live index yet.

## 2. Where this stands (every number below is read from an evidence file, and cross-checked)

| Fact | Value | Evidence |
|---|---|---|
| Code | 7d0793f, pushed; stand-in judge A | ev_code_grade_r2.txt, ev_deploy.txt |
| Binary to run | target/release/addrindex_build, sha256 f477f45f32d6b56cfdeeeda8b024f8e29fb78245476e5d3b86272ff79ec4f50f | ev_deploy.txt |
| Same binary on the timer | first catch-up on it 08:17:40-08:19:04 PST, Result=success | ev_deploy.txt |
| Dry run of the same command | 2026-11-26T08:29:40 to 2026-11-26T08:32:51 PST, exit 0 | ev_dryrun.txt |
| Walked | h1702608..h2220360 for 263 wallet addresses in 98.8 s | ev_dryrun.txt |
| Wallet output rows in the index | 50353 | ev_dryrun.txt |
| Spendable before | 383 rows / 5170.155950 TRI | ev_dryrun.txt = listunspent (ev_audit_before.txt) |
| Rows whose transaction is in no canonical block | 218 rows / 3581.961062 TRI — to delete | ev_dryrun.txt = audit |
| Spend markings pointing at an orphan block | 225 — to set to the canonical spend | ev_dryrun.txt |
| Coins that nothing canonical spends | 0 — so 0.000000 TRI returns to unspent | ev_dryrun.txt |
| History rows of orphaned transactions | 159 to delete, 1 to write | ev_dryrun.txt |
| Left alone / raced / re-heighted / missing outputs | 0 / 0 / 0 / 0 | ev_dryrun.txt |
| Spendable after (projected) | 165 rows / 1588.194888 TRI | ev_dryrun.txt |
| FRESH dry run, right before this round | 2026-11-26T09:28:59 to 2026-11-26T09:32:03 PST, exit 0, same binary and HEAD: the same plan — 218 removed / 225 moved / 159 history rows / 0 cleared, raced 0 | ev_dryrun_fresh.txt |
| Spendable then | 404 rows / 5170.202731 TRI -> 186 rows / 1588.241669 TRI | ev_dryrun_fresh.txt |
| Checker, run before the apply | on the unrepaired index it FAILS V3 (0 of 225 heights agree) and PASSES V2 (399 listed rows) | ev_after_check_before_apply.txt |
| Round 1 of this grade | A, urn:ump:qjnkiza73vkymm2sxzaajneerfu5wznkecblz6kn3yr7lcqymnxa | judge_ops_r1.txt |
| Independent walk of the node's chain | 4535 canonical blocks h2209501..h2214035: 225 of 225 coins re-spent canonically, 0 that nothing spends, 0 orphaned transactions met | ev_canonical_spenders.txt, ev_canonical_walk_meta.txt |
| Backup | 2220359 height, offset 988531403, quick_check ok, sha256 8aa019a780aba9668978805a708a0174ae78185e201d8851b3e61663e8cb2015 | ev_backup.txt |
| Rollback | a script in this bundle, sha256 eeb6bc5e7509d29fd9f4880c177b296ce46461d484c3f6e1bc8ec9d5f0d21c83: 37 sandbox checks over ten scenarios, 6 mutants each caught, `--check` run on this host against the real backup | ev_rollback_sandbox.txt, ev_rollback_mutants.txt, ev_rollback_check_live.txt |

The audit's below-window set was 225 rows / 3582.469733 TRI, of which 218 are phantom; the 7 legitimate ones (0.508671 TRI) plus the 158 in-window rows (1587.686217 TRI) are exactly the projected 165 rows / 1588.194888 TRI.

## 3. The finding that changes what the item expected

The item's E5 read the 225 wallet coins that the orphaned transactions spent as "real coins missing from the list", and F1 asked to "return coins spent only by orphans to unspent". The dry run and the independent walk agree that this is not what the chain says:

- every one of the 225 coins IS spent on the canonical chain as well, 3 to 175 blocks after the orphan block (median 44) — the node re-staked the same coin once its losing block was abandoned;
- so no coin returns to unspent (0 cleared), and the listed balance falls by exactly the phantom rows: 5170.155950 -> 1588.194888 TRI.

In other words the displayed total was NOT "right by accident": it was too high by 3581.961062 TRI, all of it unspendable outpoints. The repair moves 225 stale spend records to the true block and removes 218 rows that never existed on the chain.

## 4. What the write is, and what holds it back

One SQLite transaction on `addressindex.db`, wallet-address rows only: 218 output rows deleted, 225 `spent_height` values set, 159 history rows deleted, 1 written. In the graded code: `=apply` is required for any write; rows are re-read inside the transaction and written only if still what the snapshot saw; the tipstate oracle is re-read right before the transaction and a changed window aborts with nothing written.

One thing the phrase "nothing written" does not cover, stated plainly: it holds for the deep repair only. After the deep step, in BOTH modes, the binary runs its ordinary catch-up step — the same step the timer runs every ten minutes. In the fresh dry run that step met 426 blocks, found 425 already held, and wrote 1 new block (2 outputs, 1 spend); in the first dry run it wrote 2 of 399 blocks. Nothing of item 53 is written by it.

## 5. Verification after the apply (all four; any failure -> stop and report)

V1 the run prints APPLIED with removed / moved / history-removed = 218 / 225 / 159, raced 0, and its measured after equals its measured before minus 218 rows and 3581.961062 TRI. New stakes land while this runs — the node listed 383 rows at the first dry run and 404 at 2026-11-26T09:32:03 PST — so the absolute count is no longer 165; the before-to-after difference is what must match. V2 the audit: 0 rows outside the canonical block; then `item53_after_check.py`: every outpoint the node lists is an unspent row in the index, and per address the index holds exactly as many unspent rows as the node lists. V3 for all 225 coins the index's `spent_height` equals the height the independent walk found. V4 the next timer catch-up succeeds.

## 6. Rollback

`item53_rollback.sh` is in the bundle. It checks its preconditions first (nothing touched on a failure: exit 1), prepares and hash-verifies the restored file next to the live one, and only then stops the timer and the daemon, swaps the file in, hashes it again, starts the daemon, waits for its RPC, starts the timer and runs one catch-up (exit 0 only when all of that held; exit 2 names the step that failed and prints the units' states and the free space). Every step logs the free space of the index's disk. `--check` prints the three units' states at exit and stops none of them.

It has NOT been run for real on the live host — only its `--check` half (preconditions plus a verified prepared copy of the real backup, then removed) and the sandbox. In the sandbox: 37 checks over ten scenarios (normal path with a catch-up still running, wrong backup hash, something in the way, a bad prepared copy, a bad swapped-in file, daemon not starting, catch-up never ending, RPC never answering, `--check`, a mistyped argument), 0 failed; six mutants of the script are each caught by exactly the checks that should fail.

## 7. Limits and risks — judge these

- The rollback snapshot is from before the dry run; the apply comes later. A rollback replays that gap (automatic), it loses no chain data.
- The daemon writes the index concurrently. Rows it changes after the repair's snapshot are left alone, never overwritten.
- A reorg below the vouchable window during the run is not detected (the oracle covers the window only). The independent walk covered h2209501..h2214035 — it stopped once every coin was found — on a chain then at tip 2220370. Those spends lie below the vouchable window of that tip (4600 blocks), so their finality rests on the chain not reorganising that deep: the same assumption every confirmed balance on this node rests on. They are not re-checked later.
- The backup shares the disk with the index (the only disk, 97% full). It guards against a bad write, not a disk failure.
- Rolling back means stopping the staking daemon for the length of the swap; that is the cost of the safety net, not a step of the plan.
- The swap half of the rollback has never run on the live host.
- The balance shown to the user drops by 3581.961062 TRI. That is the correction, not a loss: those rows were outputs of orphaned blocks.
- Not touched: Pi and DNS3 (no address index), F3, any wallet file, any key.

## 8. The code round's polish suggestions — where each stands

The A carried four non-blocking suggestions. None is hidden; this is what became of each.

| Suggestion (verbatim) | Where it stands |
|---|---|
| INSERT OR IGNORE counting changed==0 as raced mislabels a benign duplicate insert as a race; consider distinguishing | Left as it is. A harmless duplicate would be counted `raced`; the dry run's count is 0 and V1 requires it to stay 0, so a non-zero count stops the verification rather than passing as noise. |
| History deletes are unconditional by rowid; a height-conditional DELETE would match the out-row discipline at trivial cost | Left as it is. The rows are deleted from a snapshot taken in the same run; if one of those transactions is ever mined in a canonical block, the ordinary catch-up inserts its history row again. |
| left_alone can double-count a row whose marking is above the walk and whose height is also edited (documented, but a distinct counter would read cleaner) | Addressed before the A: a re-heighted row counts as moved, never also as left alone (the code round's section 9). |
| Consider a guard or warning for a walk whose from..to span is very large (first live run is the scale probe) | Answered by measurement: the live dry run IS the scale probe — 517,753 canonical blocks h1702608..h2220360 walked in 98.8 s. |

## 9. Round 1's four polish suggestions — what became of each

| Suggestion (verbatim from round 1) | What was done |
|---|---|
| Re-run the dry run or at least the tipstate oracle immediately before the apply so counts and window are fresh at execution time. | Adopted. The dry run was repeated right before this round (2026-11-26T09:28:59 to 2026-11-26T09:32:03 PST): same binary, same HEAD, the same plan (218 / 225 / 159 / 0), raced 0. The apply follows this verdict right after the next timer catch-up has finished, so the two never overlap; the binary re-reads the tipstate oracle itself right before its transaction. |
| The second `--check` run left the catch-up unit in 'activating' afterwards; consider verifying all three units quiescent at --check exit, not only before. | Adopted as visibility, not as enforcement: `--check` never touches a unit, and the timer legitimately fires every ten minutes — it did during that run. `--check` now prints all three units' states at exit. The fourth run on this host shows timer active, catch-up inactive, daemon active, both before and after (ev_rollback_check_live.txt). |
| With ~6–7 GiB free and a ~1.47 GiB transient prepared copy, log free space at each rollback step so a near-full disk is visible in exit-2 evidence. | Adopted. The rollback now logs the free space at every step and in its exit-2 message. The fourth `--check` run on this host reads, in order: 8072 / 8072 / 6667 / 8072 MiB free. |
| State explicitly that V3's comparison span is h2209501..h2214035 (the walk stopped early once all coins were found) and note the residual if a spend were re-checked later. | Adopted. V3's span and residual are stated in `ev_plan.txt` and in section 7: blocks h2209501..h2214035 on a chain at tip 2220370, below the vouchable window of 4600 blocks. |

## 10. What a verdict should look like

A or B: the plan may be run as written (a B may carry polish). C, D or F: do not run it; name what has to change. Things that would rightly hold it: a number in the plan that the evidence contradicts, a verification step that could pass while the index is wrong, a rollback that could leave the node worse off, or a write wider than "wallet-address rows of one file".

Answer with a single JSON object on the last line and nothing after it: {"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}

## Evidence
- `ev_plan.txt` — the plan under review (preconditions, write, guards, verification, rollback, limits).
- `ev_dryrun.txt` — the live dry run, complete output.
- `ev_dryrun_fresh.txt` — the same dry run repeated right before this round.
- `ev_audit_before.txt` — listunspent against the node's own canonical blocks, before.
- `ev_canonical_spenders.txt`, `ev_canonical_walk_meta.txt` — the independent walk of the node's chain, and its heights.
- `ev_window.txt` — the vouchable window's size, read from the node's tipstate.
- `ev_backup.txt` — the backup's verification and checksums.
- `ev_deploy.txt` — release build, the unit, the first timer run on the new binary.
- `ev_code_grade_r2.txt` — the code round's verdict.
- `item53_rollback.sh`, `item53_rpc_probe.py` — the rollback and the RPC probe it waits on.
- `ev_rollback_sandbox.txt`, `ev_rollback_mutants.txt` — the rollback's sandbox and mutation evidence.
- `ev_rollback_check_live.txt` — the rollback's `--check` on this host.
- `judge_ops_r1.txt` — round 1 of this grade: its transcript, with the file hashes it judged.
- `item53_after_check.py`, `ev_after_check_before_apply.txt` — the after-apply checker, and its run on the unrepaired index.
