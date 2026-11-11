# 2026-11-08 fork repair — sami-pc solo branch (workqueue item 9 tick)

A routine post-deploy tip check found a REAL fleet fork that had been live
for hours. This file records the facts and the lessons; SKILL.md points
here from sections 1/8/14.

## Facts (measured, 2026-11-08 02:2x-03:00Z)

- Canonical branch A: DNS2 + Pi identical at every sampled height (tip
  2207452 f9e48fa4… at detection). DNS3 on A too (lagging 2207147,
  tor-syncing from DNS2 — verified via h2206952 hash equality).
- sami-pc: **ZERO peers**, staking ON, tip 2207544 8fb38922… — a solo
  branch B, fork point **h2206952** (b6b81fe5… shared), ~609 blocks built
  alone, **2x heavier by work** (0.0026 vs 0.0013 summed
  difficulty_from_bits over the branch ranges from both tipstate windows).
- The fork predated the detection tick: item 8 (00:5xZ) recorded "PC tip
  2207495" while canonical DNS2 was at ~2207400 — the tick verified the
  PC's verbs but never COMPARED the PC tip against DNS2's at the same
  moment. A tip read that is never cross-compared proves nothing.

## Why B was heavier (the isolated-minter pathology)

DNS2 and sami-pc stake the SAME coin set (shared legacy bag / seed
lineage). On A, DNS2 spends/rewards mutate the maturation set; on B the PC
saw none of that and kept every coin mature — a solo staker with the whole
bag out-paces a branch whose staker shares its coins with a competing
branch. Skill section 1 predicted exactly this: the split "cannot
self-heal" because each side rejects the other's blocks as double-spends.

## Repair (skill section 2 recipe, executed)

1. PC staking OFF via RPC `staking "off"` (NOTE: the param is the STRING
   "off"; a JSON `false` is silently ignored — the bare-call shape returns
   status instead). Verify getstakinginfo enabled:false, attempts frozen.
2. Stop PC daemon + GUI. RENAME aside (never delete):
   `blk0001.dat.forkB-2207547`, `index.forkB-2207547`,
   `tipstate.json.forkB-2207547` in state2\ — branch B stays recoverable.
3. Copy DNS2's canonical `blk0001.dat` (985,033,440 B) + `tipstate.json`
   (DNS2 keeps running; tipstate is written atomically per block AFTER the
   append, so the pair is consistent).
4. Start daemon → tip parity EXACT with DNS2 (hashes match at the fork
   point, the copy height, and the live tip) → balance identical to the
   sat (3382.707999 both) → start GUI.
5. PC staking stays OFF until the COIN-SPLIT workqueue item lands
   (one-owner-per-coin; shared sets caused the 2026-11-05 AND 2026-11-08
   forks). The ~609 solo-reward blocks exist only on the preserved fork-B
   store and are discarded by the repair.

## Lesson: the wallet live view does not roll back on reorg

After the repair, `repairwallet` (deployed the same tick) found 3
owned-output mismatches (2 phantom + 1 lost, 29.774982 TRI) on the PC: a
tip-height staking race had been resolved by fork choice rollback, but the
wallet live map only ever applies blocks FORWARD — the abandoned block's
outputs stayed as phantoms and the winner's outputs were missing.
`repairwallet` healed it to exact parity with DNS2. Future work: roll the
live map back inside `rollback_tip()`; until then repairwallet is the
healing verb and any post-reorg balance read should be treated as
suspect until re-verified (skill section 12 already says re-read after
catch-up — this extends it to reorgs).

## Lesson: health timers cancel `systemctl stop`

DNS2 and DNS3 run `tri7d-health.timer` every 5 minutes. A stop job that
overlaps the timer's window gets **canceled** ("Job for tri7d.service
canceled"), leaving the old process running while the disk binary is
already swapped — the swap silently doesn't take. For every daemon swap on
these hosts: `systemctl stop tri7d-health.timer` FIRST, then stop/swap/
start, then restart the timer. Verify the swap by hashing
`/proc/<MainPID>/exe` (section 13), never by trusting the command output.

## Lesson: multi-line PowerShell over ssh silently no-ops

A `powershell -Command "…"` with embedded newlines through ssh returned
empty output and did NOTHING (no error). One command per ssh call, or
` powershell -Command` with a single line; verify every copy with
`Get-FileHash` readback (section 12).
