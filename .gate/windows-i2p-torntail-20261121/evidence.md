# Verification evidence — tri7 Windows CI fixes (i2p attribution + torn-tail append)

Branch: fix/windows-i2p-attribution-torn-tail, base origin/master 1dc34575e75ed037d5b197a5a658757806063bf3.
Diff: 2 files, +59/-18 (src/blockindex.rs, src/gui/node_supervisor.rs). Verbatim diff in diff.patch.

## 1. The failures being fixed (measured from GitHub CI, read-only)

PR #1 (ci-score-fixes, workflow-YAML-only) and master BOTH fail windows-build with the
SAME 3 tests — proving the failures pre-date and are independent of the CI-hardening PR:

- PR run 44677482247, job 136092658917 (2026-11-20): `test result: FAILED. 869 passed; 3 failed; 1 ignored`
- master run 44844400030, job 136660444194 (2026-11-21): `test result: FAILED. 869 passed; 3 failed; 1 ignored`
- Identical failure names in both:
  - blockindex::tests::torn_tail_builds_ready_and_the_next_append_heals_it
  - gui::node_supervisor::tests::i2p_bundled_spawn_management_and_destination
  - gui::node_supervisor::tests::i2p_own_child_is_never_reclassified_or_killed_across_ensures
- linux-tests + linux-gui-tarball green in both runs; macos-build green on master
  (its one PR-run failure, in_process_node_answers_rpc, is a flake: the only master
  commit since the PR base is "add MIT LICENSE", which cannot affect an RPC test).

Raw CI panic lines (master job 136660444194, verbatim):
```
thread 'blockindex::tests::torn_tail_builds_ready_and_the_next_append_heals_it' (5004) panicked at src\blockindex.rs:2793:14:
append over a torn tail: "append: truncate torn tail: Access is denied. (os error 5)"

[i2p-supervisor] i2p: pinned port 65398 answered; listener attribution: pid=6976 path=D:\a\tri7\tri7\target\release\build\tri7-core\b586d11ae3398900\out\tri7_core-b586d11ae3398900.exe → foreign
thread 'gui::node_supervisor::tests::i2p_bundled_spawn_management_and_destination' panicked at src\gui\node_supervisor.rs:4617:9:
assertion `left == right` failed: alive child is reused with the same SOCKS port
  left: System(63665)
 right: Bundled(63665)
```
(pid=6976 IS the test harness pid — the test's temp dir is named i2plife6976 from
std::process::id(); the listener belongs to the test process, not the spawned child.)

## 2. Root causes (Phase-1 investigation, code read end-to-end)

### 2a. torn-tail: append-only handle cannot truncate on Windows
StoreInner.file was opened `.read(true).append(true)` (blockindex.rs ~L1108).
Rust maps `.append(true)` to FILE_APPEND_DATA on Windows; SetEndOfFile (what
File::set_len calls) requires FILE_WRITE_DATA → ERROR_ACCESS_DENIED (5).
On Linux read+append maps to O_RDWR, so ftruncate succeeds — Windows-only defect.
The torn-tail heal (append_block, both copies) does `fh.set_len(cut)` then
`fh.seek(SeekFrom::End(0))` + write_all under append_lock. O_APPEND atomicity is
NOT load-bearing: every write site seeks End explicitly under the lock (verified:
the only write_all sites on inner.file are L1774 and L1896, both preceded by
seek(End(0)); flush_pending delegates to the same locked path).
FIX: open `.read(true).write(true)` (GENERIC_WRITE on Windows, O_RDWR on Linux);
writability probe aligned to `.write(true)` (the access append actually needs).
Production behavior preserved: same explicit-seek-under-lock append sequence.

### 2b. i2p: test stub violated the production listener-identity invariant
Production rule TRI7-I2P-01 (node_supervisor.rs ensure(), branch 1): a live child's
SOCKS answer counts as OURS only when the pinned port's LISTENER pid IS the child
pid (TorSupervisor::listener_pid_is), else the child is killed and the port is
re-attributed (branch 2: wallet-owned orphan vs foreign System).
The Windows native resolver (GetExtendedTcpTable + QueryFullProcessImageNameW)
correctly reports the TRUE listener. stub_i2p_spawn spawned a dummy `cmd /C ping`
child but bound the SOCKS listener in a detached thread of the TEST process →
listener pid = test harness ≠ child pid → child_usable=false → own live child
killed + reclassified System on every ensure after the first. Linux/macOS resolve
nothing (resolved.is_none() bypass) so the mismatch was invisible there.
The production logic is CORRECT (it did exactly its job); the STUB was unfaithful.
The tor lane already fixed this exact class: child_identity_pair (pid registry +
injected listener_resolver) + stub_tor_spawn(req, registry) — its docstring
literally documents "Without this pair the port's true listener is the test
process itself and the Windows fail-closed attribution correctly refuses it
(live-caught: the windows CI run failed exactly the attribution tests for that
reason)."
FIX: mirror that established pattern for i2p — stub_i2p_spawn gains the same
optional registry parameter and records child.id(); the TWO failing tests inject
child_identity_pair + pass the registry; the two passing stub callers pass None
(byte-identical behavior). No production code path changed in node_supervisor.rs.

## 3. Verification (commands actually run on this Linux host, base 1dc3457 + this diff)

### 3a. Full lib suite (regression gate)
CMD: `cargo test --release --lib`
```
test result: ok. 872 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 47.62s
SUITE_EXIT=0
```
Same counts as master (872/0/1) — no test added/removed; the two attribution tests
and torn-tail pass with the new stub/handle shape on Linux.

### 3b. Windows cross-build, ALL bins (repo skill gate before push)
CMD: `cargo build --release --target x86_64-pc-windows-gnu --bins`
```
warning: `tri7-core` (bin "spend_dryrun") generated 2 warnings ... (pre-existing unused imports)
    Finished `release` profile [optimized] target(s) in 2m 17s
BUILD_EXIT=0
```
Zero errors. Target x86_64-pc-windows-gnu installed; linker x86_64-w64-mingw32-gcc present.

### 3c. Windows test-code compile check (cfg(windows) test paths)
CMD: `cargo check --release --target x86_64-pc-windows-gnu --tests`
```
    Finished `release` profile [optimized] target(s) in 45.19s
CHECK_EXIT=0
```
Zero errors (86 pre-existing warnings, unrelated).

### 3d. Windows RUNTIME proof — honest status
This host is Linux; the Windows-only code paths (native TCP-table resolver,
FILE_APPEND_DATA truncation semantics) cannot execute here. The runtime proof is
the CI windows-build job on the pushed branch (~70 min), which runs the exact 3
failing tests on a real msvc runner. Root cause 2a is a documented Windows API
semantic (SetEndOfFile requires FILE_WRITE_DATA; FILE_APPEND_DATA is insufficient —
Microsoft docs "SetEndOfFile function (fileapi.h)": the handle must have
GENERIC_WRITE access); root cause 2b is proven by the CI log's own pid evidence
(listener pid == test harness pid) and the tor lane's identical live-caught fix.

## 4. Scope
- src/blockindex.rs: handle open options + writability probe + comments (product
  code; append semantics preserved — explicit seek under append_lock unchanged).
- src/gui/node_supervisor.rs: TEST module only (stub signature + registry record,
  two tests gain the identity pair, two callers pass None, docstrings). Zero
  production-line changes in this file.
- No workflow, packaging, or other file touched. Untracked .gate/ bundles and
  GOVERNANCE.md in the worktree are foreign residue, untouched, not staged.

## 5. Judge substitution context
Primary Codex judge is quota-walled (probe 2026-11-21: "You've hit your usage
limit ... try again at Nov 22nd, 2026 1:39 PM"). Per the operator's standing
directive (glm-judge-on-demand skill), the GLM stand-in grades this bundle with
--reason quota. No prior primary verdict exists for THIS artifact (first round).

## 6. macOS flake fix (added 2026-11-21, owner: "can you fix macos?")

### 6a. The flake, measured
macos-build on PR#1 run 44677482247 AND on this branch's run 44863020321 failed
ONLY on gui::node_supervisor::tests::in_process_node_answers_rpc (871 passed /
1 failed). Master's latest run was green on it. Raw log from this branch's macos
job 136720417171 (fetched via the jobs/logs API while the run was in progress):
```
[2026-11-21T03:53:15Z] supervisor: spawning node (fresh_store=true, --sync-from-zero=always)
[2026-11-21T03:53:17Z] supervisor: node run ended, exit code 0
thread 'gui::node_supervisor::tests::in_process_node_answers_rpc' (62627) panicked at src/gui/node_supervisor.rs:2896:9:
node thread must be alive after a live RPC
```
Spawn→end = exactly the stub's 2s serve deadline.

### 6b. Root cause (code read, test at ~L2830)
Run 1's stub runner served up to 3 RPC connections within a FIXED 2s deadline,
then returned Ok(0) (node thread exits). The test asserts `n.running()` (thread
alive) immediately after ensure_running returns ok. On a slow/loaded macOS
runner the supervisor's RPC-wait answer arrives near the end of the 2s window;
the stub deadline then expires and the thread exits BEFORE the liveness
assertion runs → panic. A wall-clock race between the stub's serve window and
the test's assertion — not a product defect (ensure_running's RPC-wait path is
unchanged and correct).

### 6c. Fix (test-only, deterministic)
Run 1's lifetime is now owned by the TEST via a release1 AtomicBool: the stub
serves until the flag is set (30s hang-safety net only, no connection cap).
Order: ensure_running → assert ok + running() + argv + calls + served (thread
guaranteed alive — flag not set) → release1.store(true) → wait node_finished
(5s deadline; stub exits within one 25ms poll). Runs 2 and 3 unchanged. The
exit+restart phase still gets a REAL finished run 1 — deterministically.

### 6d. Verification (this Linux host, combined diff)
CMD: `for i in $(seq 1 15); do cargo test --release --lib in_process_node_answers_rpc; done`
```
run 1..15: test result: ok. 1 passed; 0 failed; ... finished in 3.6-4.3s (15/15 green)
```
CMD: `cargo test --release --lib`
```
test result: ok. 872 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 72.79s
SUITE_EXIT=0
```
CMD: `cargo check --release --target x86_64-pc-windows-gnu --tests`
```
Finished `release` profile [optimized] target(s) in 26.24s
CHECK_EXIT=0
```
macOS runtime proof: the CI macos-build job on the pushed combined diff (the
flake's own runner class). The fix removes the race by construction — the
liveness assertion no longer depends on any timing.

## 7. Combined scope (final)
2 files, +81/-30 vs origin/master: blockindex handle access (product, semantics
preserved) + node_supervisor TEST module (i2p identity pair + macOS release-flag
stub). Zero production-line changes in node_supervisor.rs. Prior B verdict
urn:ump:rwamjaqxibqr5izk3uhuhah5ql4hrb7uw2p3mqrnz4huvvqwofaq covered the windows
diff only; this re-grade covers the combined diff (shipped = graded bytes).
