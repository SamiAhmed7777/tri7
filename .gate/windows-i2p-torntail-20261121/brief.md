# Brief: tri7 CI fixes — Windows (i2p attribution + torn-tail) + macOS flake (r2, combined diff)

## Operator directive
Sami Ahmed (owner, Telegram chat 637130179, 2026-11-21), verbatim: "Fix the Windows
bugs first — I'd open a separate lane for the i2p self-detection + torn-tail failures
(the i2p one could make the wallet kill its own i2pd child — genuinely worth fixing)."
This is that separate lane, branched from origin/master (1dc3457), independent of the
open CI-hardening PR #1.

## Purpose
Fix the 3 deterministic windows-build CI test failures (identical on master and PR #1,
proven by CI run/job IDs in evidence §1):
1. blockindex torn-tail append: `set_len` on an append-only handle → Windows
   ERROR_ACCESS_DENIED (FILE_APPEND_DATA lacks the FILE_WRITE_DATA SetEndOfFile
   requires). Fix: open the store file `.read(true).write(true)`; every write site
   already seeks End explicitly under append_lock, so append semantics are preserved.
2. Two i2p attribution tests: the stub bound the SOCKS listener in the TEST process
   while spawning a dummy child, violating the production TRI7-I2P-01 invariant
   (listener pid must equal child pid) that the Windows native resolver correctly
   enforces. Fix: apply the tor lane's established child_identity_pair pattern
   (pid registry + injected listener_resolver) to stub_i2p_spawn and the two failing
   tests. Production attribution logic is unchanged — it was correct.

## Claims (each backed by a capture in evidence.md)
- Full Linux lib suite green with the diff: 872 passed / 0 failed / 1 ignored, exit 0 (§3a)
- Windows cross-build ALL bins zero errors, exit 0 (§3b); Windows test-code check exit 0 (§3c)
- Failures pre-exist on master: same 3 test names, same 869/3 counts, runs 44677482247 + 44844400030 (§1)
- node_supervisor.rs changes are TEST-module-only; blockindex.rs change preserves the
  explicit-seek-under-lock append sequence (§2, §4)
- Windows runtime proof pending CI windows-build on push — disclosed honestly (§3d);
  this Linux host cannot execute the Windows-only paths

## Scope
2 files, +59/-18. No workflow/packaging/other changes. Foreign untracked residue
(.gate/, GOVERNANCE.md) untouched and unstaged.

## Judge substitution
Codex quota-walled until 2026-11-22 13:39 (probe transcript in evidence §5); GLM
stand-in per the operator's standing directive, --reason quota. First round for this
artifact — no prior primary verdict.

## r2 addition (owner: "can you fix macos?")
The macOS flake (in_process_node_answers_rpc, failed on PR#1 and this branch's
first run, green on master) is now FIXED by construction: run 1's stub lifetime
is test-controlled via a release flag instead of a fixed 2s serve window that
raced the liveness assertion on slow runners (evidence §6: raw CI log, root
cause, fix, 15/15 repeat runs green, full suite 872/0/1, windows test-check 0
errors). Test-only change; runs 2/3 and all production code unchanged. This
re-grade covers the COMBINED diff (windows fixes from r1 + macOS fix); the r1 B
verdict covered the windows-only diff. Shipped bytes = this graded diff.

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": [...], "blocking_issues": [...], "polish_suggestions": [...]}
