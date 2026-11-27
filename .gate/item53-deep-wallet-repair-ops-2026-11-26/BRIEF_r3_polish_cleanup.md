# BRIEF — TRI7 workqueue item 54, parts C and D: rollback polish and the item-53 backup cleanup

## 1. What is being graded

Two things, as one package. A or B means "do both as written". C, D or F means
"hold what you name".

1. **Part C, code.** A revision of `item53_rollback.sh` plus its sandbox and
   its mutants, now kept beside it in the repository. This script is not a
   service and nothing deploys it: it is run by hand only, as a rollback.
2. **Part D, a deletion.** One directory on DNS2:
   `/root/tri7-lab/backups/item53-pre-dryrun-2026-11-26T0819` — five files,
   1,478,447,797 bytes. The deletion is not reversible.

The queue item (TRI7-WORKQUEUE.md, item 54) lists both: "C. Rollback polish …"
and "D. Backup cleanup (judge-gate the deletion first)". The polish comes from
the round-2 verdict on the item-53 operations (`judge_ops_r2.txt`, its "polish"
list), which named four things; all four are addressed in section 3.

Not in this package, and not done: the fleet's daemons (no deploy, no restart),
the Pi, DNS3, sami-pc, and item 54's part B.

## 2. Where this stands (every number is read from a file in this directory)

| Fact | Value | Evidence |
|---|---|---|
| Repair the backup was made for | applied 2026-11-26 09:41–09:43 PST, exit 0 | ev_apply.txt |
| Checked after the apply | `RESULT: ALL CHECKS PASS` | ev_after_check_after_apply.txt |
| Audited after the apply | 0 of 190 listed outpoints missing from its canonical block | ev_audit_after.txt |
| Backup, when made | 08:19–08:28 PST 2026-11-26; `BACKUP COPY WHOLE`; index held h2220359 | ev_backup.txt |
| Backup's three sha256, recomputed today | identical to ev_backup.txt (db 8aa019a7…, balances e7364128…, tipstate b5296480…) | ev_backup_recheck_r3.txt |
| Live index now | `complete=1`, height h2220833, 2,656,987 output rows | read-only query, 2026-11-27T16:10Z (section 4) |
| How far the snapshot is behind | 474 blocks (h2220359 → h2220833); the backup's last output rowid is 2,656,503, the live index has 484 more | ev_backup.txt, same query |
| Last catch-up | Result=success, 08:01:11–08:02:37 PST today | `systemctl show tri7-addrindex.service` |
| Disk | 98% used, 4.4 GB free; the deletion frees 1.47 GB | ev_backup_recheck_r3.txt |
| What else references the backup | one line of code (`item53_rollback.sh`, its default) and plan_dns2_v4.md line 414, which says it "stays on disk until the judge-gated cleanup of part D" | grep over /root/tri7-lab, /root/tri7/.gate, /etc, /opt |

## 3. Part C — what changed in item53_rollback.sh

The graded, applied revision hashed eeb6bc5e…; this one hashes
48482e97…. The whole difference is `ev_rollback_r1_to_r2.diff` (35 lines
added, 7 removed). The service-facing steps are untouched: preconditions,
prepare and verify, stop timer, wait for the catch-up, stop daemon, swap,
hash, start, catch-up, prove.

| Change | Why | Proof |
|---|---|---|
| After the swap, `chown` and `chmod` carried `\|\| true`. A failed chown would have left the index with the wrong owner while the script printed "ROLLBACK DONE AND PROVEN". Now either failure exits 2 through `swap_fix_failed` and prints the two commands that fix it by hand. | judge_ops_r2 polish, item 2 | sandbox K (chown) and L (chmod): exit 2, the restored file in place, nothing started, the step named, the hand-fix printed. Mutants `chown_failure_ignored`, `chmod_failure_ignored`: each killed by exactly its four expected checks |
| The sandbox's fault injection was two inline blocks whose comment said "the live default path refuses". That was false for `post_swap_sha`: with the default backup present it would have run the real swap and then appended a byte to the live index. Now a `fault()` helper also requires a non-default live path, and a guard at the top refuses the run before the lock is taken. | judge_ops_r2 polish, item 4: the comment was there but wrong | sandbox M: exit 1, names the live path, no lock taken, systemctl never called. Mutant `live_path_guard_removed`: killed by exactly its two expected checks. Demonstrated on DNS2 with real paths: ev_rollback_live_refusals_r3.txt |
| A comment at `CATCHUP_WAIT` (600 s): the wait comes before the daemon is stopped. | judge_ops_r2 polish, item 3 | also written into the plan: ev_plan.txt, last section |
| The header says the default backup is gone and how to point at another copy. | part D makes it true | ev_rollback_live_refusals_r3.txt, run 1: `REFUSED: no backup at … — nothing was touched`, exit 1 |
| V2's per-address check assumes one address string form | judge_ops_r2 polish, item 1 ("worth a note") | a note in ev_plan.txt; item53_after_check.py is not changed |

The fourth polish item is a note only. I chose not to edit the checker for a
comment, because its sha256 is quoted in the evidence that was produced with
it.

**Tests.** `ev_rollback_sandbox_r2.txt`: 51 checks over thirteen scenarios,
0 failed (it was 37 over ten). Scenarios A–J are byte-for-byte the old ones and
still pass. `ev_rollback_mutants_r2.txt`: nine mutants, all killed by exactly
their expected checks (it was six). The original 37-check harness run against
the revised script: 37 of 37 (`ev_rollback_sandbox_r1harness_x_r2script.txt`).
The real `/var/lib/tri7` before and after those runs: same inodes, sizes,
owners, modes, no stray file (`ev_live_path_around_sandbox.txt`).

## 4. Part D — the deletion, as it will be run

```
B=/root/tri7-lab/backups/item53-pre-dryrun-2026-11-26T0819
rm -f -- "$B/addressindex.db" "$B/addressindex.db-shm" "$B/addressindex.db-wal" "$B/balances.json" "$B/tipstate.json"
rmdir -- "$B"
```

The five files by name, then `rmdir`, which fails if anything else is in the
directory. No recursive delete.

Read-only query behind the "live index now" row (sqlite, `mode=ro`):
`SELECT * FROM meta` → height 2220833, complete 1;
`SELECT count(*) FROM out` → 2656987; `SELECT max(height) FROM blk` → 2220833.

Verified after: the directory is gone (`ls` fails), `df` shows the space,
`bash item53_rollback.sh --check` exits 1 with "no backup at …", and
tri7d.service and tri7-addrindex.timer are still active.

Why it is safe to delete:

1. The repair it was the safety net for ran clean and has been checked three
   ways (apply exit 0, the after-check, the audit).
2. It is not a state to return to. It holds h2220359 with the 218 phantom
   unspent rows (3581.96 TRI) and the 225 orphan spend markings that the
   repair removed, 474 blocks behind the live index.
3. Nothing reads it. The only code reference is the rollback's default
   directory, and without the backup that script refuses before anything is
   touched.
4. The disk is at 98%. This is the largest file on it that nothing uses.

## 5. What is not done, stated

- The rollback's swap half has still never run on the live host. Only `--check`
  and the sandbox have. That was true of the graded revision too.
- Scenario M is deliberately weaker than it could be: it uses a backup whose
  pinned sha256 is wrong, so that a broken guard would still be refused before
  anything is touched. The mutant is therefore caught by two checks (the
  message, and the lock not taken), not by a refusal-versus-swap difference.
- The deletion is on DNS2 only. DNS3 and the Pi have no address index.
- Item 54's part B (listunspent freshness) is not started: it needs a daemon
  deploy, and the fleet is on hold for a decision about the chain split.
- The uncommitted v5 work in /root/tri7 is not part of this commit.
- ev_plan.txt's last section is written for the state after the deletion. If
  you hold part D, its last bullet has to be reverted with it.
- ev_backup_cleanup.txt does not exist yet: it is the record of the deletion
  and is written when the deletion is run, after this grade.
