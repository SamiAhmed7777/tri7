# tri7 supplementary judge: (a) unjudged follow_tip delta inside commit
# 795a1e6, (b) the GUI node_supervisor fresh-machine fix (pending commit)

## Honesty first — why this exists

Commit 795a1e6 (item 8 RPC-P1a, graded A urn:ump:hvuizt5qmw6cqncpkznpqxmaq
jelobzzay55um44vi22ooss5rla) was judged from a working-tree diff captured at
~23:56Z. Between that capture and the commit (~00:03Z), the tree gained
further changes from the PREVIOUS failed cron run's pending background work
(same theme: fresh machines must sync from zero without manual flags). The
commit therefore contains one hunk-pair in src/bin/follow_tip.rs that the
original judge never saw. This bundle shows the FULL follow_tip.rs diff AS
COMMITTED (follow_tip-committed.diff, `git show 795a1e6 --
src/bin/follow_tip.rs`) so the delta can be judged against reality.

It also covers the still-uncommitted counterpart
(node-supervisor.diff, src/gui/node_supervisor.rs, +6): the GUI's embedded
node supervisor passes --sync-from-zero only on a fresh state dir (no
blk0001.dat), so a first launch fetches bodies instead of anchoring genesis
and stalling. Provenance note: this 6-line fix appeared in the tree the same
way; its comment cites the observed failure (Sami laptop stuck at "store
missing block body at h0" for hours).

## What the delta does (follow_tip-committed.diff, hunks 3+4)

Hunk 3 (@@ -7457): `have_state` — "we have usable local state" — previously
meant only "tipstate file exists and no --reseed". A tipstate written by a
from-zero anchor that never fetched bodies (tip h0, empty/absent
blk0001.dat) satisfied it, so the automatic from-zero path never fired and
the node stalled at genesis. Now the blocks file must EXIST and be NON-EMPTY
for state to count as usable; genesis-only state is treated as a bare
machine, so `from_zero = cfg.sync_from_zero || (!have_state &&
cfg.explicit_peers)` starts the body fetch with no extra flag. The binding
`let blocks_file = resolve_blocks_file(&cfg)` moved above its first use
(hunk 4 @@ -7567 removes the later duplicate binding) — pure code motion.

Risk analysis: on an EXISTING synced node the blocks file is large and
present, so `have_state` is unchanged — resume/normal-follow behavior is
untouched. The only behavior change is on machines with an empty or missing
blocks file, which previously dead-locked at genesis; those now take the
from-zero path (the same path --sync-from-zero selects manually, and the
same path a fresh install already took when NO tipstate existed). The gate
is strictly more honest about what "state" means.

## Captured evidence

- Full lib suite green WITH all of this content:
  `test result: ok. 484 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 52.78s`
- windows-gnu all-bins build green WITH all of this content (built 00:01Z,
  before the commit): `Finished release profile [optimized] target(s) in 1m 58s`
- follow_tip bin tests: `43 passed; 0 failed`.
- Commit pushed: GitHub master 871dbf3..795a1e6 (7 files, +733/−8 — the
  +12/−2 over the judged 721/6 IS this delta).

## Questions for the judge

1. Is the hunk-3 semantics sound (genesis-only tipstate + empty blocks file
   = not usable state)? Any node state where blocks_file exists but empty
   yet state IS usable and this would wrongly force a from-zero refetch?
2. Is hunk 4 a clean motion (no behavior change)?
3. Is the node_supervisor +6 correct and consistent with hunk 3 (GUI passes
   the explicit flag on fresh dirs; the daemon path now also auto-detects)?
   Redundancy between the two is intentional defense-in-depth.
