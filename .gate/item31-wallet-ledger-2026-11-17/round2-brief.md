# Item 31 round 2 — builder writes tip_hash (completes the tier-1 proof path)

Live post-deploy finding on DNS2: the lag-aware verdict refused even with the
index complete and within lag — because meta had NO tip_hash at all. Only the
daemon's per-block append (LiveIndex::apply) writes it, and on DNS2 that lane
does not advance the index (only the 10-minute catch-up builder does), so the
tier-1 proof could never exist fleet-wide.

Change (2 files, ~30 lines):
- addrindex.rs: `pub fn set_tip_hash` — records the hash of the last applied
  block in meta (same write LiveIndex::apply already performs; now callable
  by the builder).
- src/bin/addrindex_build.rs: at each committed 500-block slice AND at the
  final finish, write the hash of the block AT the committed height
  (slice boundary = honest for `height`; eof tail = the last accepted block,
  committed by the final tail COMMIT; partial stop = last committed slice's
  hash). A resume that re-walks an uncommitted tail can never claim a hash
  its advertised height doesn't cover.

Fail-closed unchanged: no tip_hash / mismatched hash / incomplete / ahead /
past-lag all refuse exactly as before. The daemon's own apply() path keeps
writing tip_hash per block as it already did.

Verification: addrindex unit tests 2/2 green (incl. the lag-rule matrix);
full suite re-run after this diff: in progress — lib compiles clean, the two
touched crates' tests pass. Deploy effect: the next builder run (systemd
timer, ≤10 min) stamps tip_hash and tier 1 engages without a restart.
