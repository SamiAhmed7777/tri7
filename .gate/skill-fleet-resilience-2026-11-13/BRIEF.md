# Skill delta — fleet-resilience 2026-11-13

triangles-modernization/SKILL.md, ONE bullet replaced in the OPEN DEV ITEMS
section: item 1 (embedded-node reconnect defect) marked FIXED + COMMITTED
fb6e875 (grade B urn:ump:ilhtwodj2mhdgst3uink3yhyqkqe7mwebw4z2pk3ckwvx6fr5hma)
with the mechanism (fresh+healthy outbound peer gate, 240s window), the
companion fleet-resilience changes (adoption escalation, pinned tor port,
process-bound proxy identity, fail-closed Windows attribution, ownership-safe
recovery), the judge chain summary, and an explicit NOT-YET-DEPLOYED marker.
Item 2 (addrindex self-heal) stays OPEN. No other section touched — proven by
`skill-diff.md`: exactly ONE hunk (the bullet swap), built from the genuine
pre-change baseline (`SKILL-baseline.md`, reconstructed by exactly reversing
the single atomic patch operation) against the current live file.

## Bundle provenance (r7 fixes applied)

- THE current SKILL.md enters the bundle ONLY as the wrapper's frontmatter
  target copy (the live file at
  /root/.hermes/skills/triangles-modernization/SKILL.md passed via --files);
  there is no second bundle copy. The wrapper's numbered target copy IS the
  single authoritative current file and matches the diff's "current" side.
- `commit-fb6e875-src-diff.patch` was exported directly from the commit:
  `git show fb6e875 --format=""` — its numstat (710+87 node_supervisor.rs,
  227+6 node_embedded.rs = 937 insertions, 93 deletions) matches
  `commit-fb6e875-stat.txt` exactly.
- SHA256 of every shipped content file: see `bundle-hashes.txt` (includes
  BRIEF.md; the hash file itself is excluded — self-hash is undefined; the
  frontmatter-target copy is hashed in its PRE-backfill content form, since
  the wrapper appends codex_grade lines at exit by design).
- `skill-diff.md` is a CONTENT-ONLY diff by explicit definition: both sides
  have their YAML frontmatter stripped before diffing (frontmatter is
  wrapper-managed state — the baseline predates the backfill and the current
  file gains grade metadata at wrapper exit). Generation command recorded in
  the file header: `difflib.unified_diff(strip_fm(baseline), strip_fm(current))`.
  Exactly ONE content hunk = the OPEN DEV ITEM 1 bullet swap.

## History disclosed (r2–r7 attempts)

- r2: exit 78 (wrapper arg shape — frontmatter target must also be in
  --files). No review happened.
- r4: review graded C (in human-readable text, which the wrapper could not
  parse as JSON, so the recorded frontmatter defaulted to F — a verdict-parse
  artifact, not the reviewer's grade). Its two findings were REAL bundle
  defects: (a) the "baseline" copy was the post-change file; (b) the bundle
  carried two inconsistent SKILL copies after the wrapper backfilled grade
  frontmatter onto one of them. Both fixed here: genuine baseline + exactly
  one authoritative current copy, and this brief discloses the backfill.
- r7 (parseable C): blockers were packing artifacts — the wrapper's own
  numbered target copy looked like a second authoritative file (they are
  byte-identical BY DESIGN: it IS the live file), and the exported src patch
  was off by one insertion vs the commit stat. Both fixed in this round's
  packing (single target copy; patch exported from the commit itself).
