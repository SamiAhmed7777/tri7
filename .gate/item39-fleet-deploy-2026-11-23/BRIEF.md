# ITEM 39 — fleet deploy procedures (DNS3 + Pi legs) — judge brief

Date: 2026-11-23 · Repo: /root/tri7 (master @ 2f95298; deploy pins the graded commit 5747239)
Kind: deploy · Operator loop: TRI7 PARITY (workqueue item 39)

## Claim boundary — what this artifact is and is not

This artifact is the DEPLOYMENT PROCEDURE for completing the item-39 fleet rollout:
two bash scripts plus a deterministic protocol test that MEASURES their exit codes
and restoration guarantees against stubbed dependencies (73/73 checks pass). It is
NOT a record of a completed rollout: the scripts have not yet run on DNS3/Pi, and
the post-run readback (boot audit, three-way parity, wallet surfaces) is executed
by the operator loop after this gate passes and recorded in the workqueue PROGRESS
section with its own evidence. DNS2 is already deployed and healthy (see EVIDENCE) —
it is never restarted by these scripts.

## What is being deployed and why

Item 39 (AUTOHEAL-REBUILD-ARCHIVE-FALLBACK): the AUTOHEAL repair seed was
tip-minus-N, leaving a coverage hole; the fix makes the rebuild seed from full
history (h0) with an archive-fallback path. Code was judged grade A / zero
blockers (urn:ump:yw4isszergrygqghhosnh2rzim7up5anpxs2sjkn24cqrgxg46aq),
committed as 574723943b7f39ef3276e9a57beaf4acb5d99258 (+ stack 1db2457/e873b42/
7fad338, each independently graded), pushed, CI run 45085582662 = SUCCESS, and
deployed to DNS2 at 00:00Z (binary sha 87755353…, boot 00:01:31Z, NRestarts=0).
The predecessor tick died before the DNS3/Pi legs; these scripts finish them:

- DNS3 leg (binary-copy, no repo on that host): ship the exact staged artifact
  87755353c853b96cfd01d5838d0c66f8f2cc5dc6993209b15287a86c9ea50c5e (7,480,064
  bytes — byte-identical to the DNS2 build, proven live there: AUTOHEAL rebuilt
  h0..h2217779, balance surfaces exact, staking unregressed; see EVIDENCE).
- Pi leg (on-device build, repo present, clean tree, running inode == path
  digest f027bd5d…): git fetch + checkout --detach the FULL pinned hash +
  rev-parse verification + cargo build (aarch64).

DNS2 (the sole heavy staker) is untouched: follower-only restarts, per the
item-19 fresh-handshake recipe.

## The scripts' contract (both legs)

PHASE DISCIPLINE: the ERR→rollback trap is armed ONLY after a
digest-verified backup of the running bytes exists (and, on the Pi leg,
ORIG_HEAD/ORIG_BRANCH are captured). Before that, every failure path is
explicitly guarded and exits 3 — rollback can never run with unset, absent,
or unverified restoration inputs.

Phase 0 (read-only pre-flight, no trap, failures exit 3 with nothing
mutated): required commands present; args regex-validated (full 64-hex sha,
full 40-hex commit, numeric min-tip); RPC creds present in the conf (read
in-process, never printed); unit active; live-binary path hashable; the
RUNNING INODE's digest == the path digest == the expected running sha
(proves the bytes being replaced are the running bytes); Pi additionally:
repo is a usable git repo, clean working tree (refuse to deploy over
operator WIP), no .gitmodules, ORIG_HEAD/ORIG_BRANCH captured.

Phase 1 (mutation, trap armed): backup dir (guarded, exit 3 if uncreatable)
→ digest-verified backup of the running bytes (guarded; a failed copy or
digest check removes the bad side copy and exits 3) → arm the trap →
DNS3: install the staged artifact (digest re-verified); Pi: fetch +
checkout --detach the FULL pinned hash — BOTH explicitly guarded
(`if ! git …; then restore_pre_restart_or_fatal; exit 3; fi`), so a fetch
or checkout failure is never left to the ERR trap: the daemon is NOT
restarted for it, the binary path and repo state are restored and proven,
and the script exits 3. The ERR trap→rollback path is reserved for the
post-restart world. Post-checkout: rev-parse == commit, tree clean at it
→ on-device build → built digest must differ from old. Restart, then the
health poll: 120s monotonic poll budget inside a HARD outer deadline
(`timeout --kill-after=10 130`), one JSON-RPC call per attempt
(getblockcount, then getbestblockhash), JSON-RPC error objects rejected,
tip must be ≥32 hex chars, Basic auth derived in-process from the conf;
healthy = answer AND height ≥ min-tip floor (2217775 = DNS2 tip minus 50,
allowing follower catch-up).

Success proof: exit 0 only after the RUNNING INODE digest of the restarted
daemon equals the newly installed/built binary (not just the pathname).

Failure handling, by construction:

- Pre-restart failures (fetch, checkout, HEAD≠pinned commit, dirty tree at
  the pinned commit, build failure, binary absent, unchanged digest):
  `restore_pre_restart_or_fatal` PROVES restoration (binary path digest ==
  RUN_SHA AND repo HEAD == ORIG_HEAD) then exit 3 — any failed restoration
  is exit 2 FATAL, never a quiet exit 3. The daemon keeps running the old
  inode; NO restart ever happens on a pre-restart failure.
- Post-restart failures (health poll unhealthy, running-inode mismatch):
  rollback restores backup + repo state, verifies the restored digest, the
  restored repo HEAD, restarts with a guarded `systemctl restart || exit 2`,
  checks unit-active, THEN proves the restarted daemon's RUNNING INODE
  digest equals the restored bytes (a unit whose ExecStart resolves
  elsewhere is caught here → exit 2) → exit 1 (clean rollback, path AND
  running-inode proven) or exit 2 (FATAL, restoration or proof failed).
- Exit-3 post-state is exact: the live binary, daemon, and repository are
  untouched; the only possible lasting effects are an empty backup
  directory and/or a digest-verified backup copy of the still-running bytes.

Testability knobs (ITEM39_* env vars) exist ONLY so the protocol test can
redirect BIN/BACKUP/RPC/CONF/REPO/UNIT into sandboxes; unset, every value
is the production default (paths as deployed today).

## Measured behavior (deterministic protocol test, 73/73)

The submitted test_harness.py is SELF-CONTAINED: it resolves the deploy
scripts and stubs relative to its own directory and installs the submitted
test_stub_systemctl.sh / test_stub_cargo.sh into its sandbox, so the
measured run verifies exactly the submitted bytes. The systemctl stub is a
state machine that spawns REAL ELF daemons (so /proc/<pid>/exe is a genuine
running inode); the cargo stub writes via temp+rename (cargo's real
discipline — never O_TRUNC over a running executable); the RPC stub is a
local HTTP server that validates the Basic auth derived from the conf and
serves controlled responses incl. JSON-RPC error objects. bash, git,
install, sha256sum, timeout, python3 are real. Scenarios and measured
outcomes:

| scenario | measured exit | proven post-state |
|---|---|---|
| D1 DNS3 success | 0 | binary==new, backup==old, running inode==new |
| D2 DNS3 health-fail | 1 | binary restored==old, backup intact, running inode==old, unit active, poll bounded ≤140s |
| D3 DNS3 wrong-sha | 3 | binary untouched, NO backup written |
| D5 DNS3 unit-inactive | 3 | aborted pre-mutation, no backup |
| D6 DNS3 missing live binary | 3 | daemon still alive, no backup |
| D7 DNS3 rollback-restart-fail | 2 | FATAL (rollback restart failed), binary path restored==old |
| D8 DNS3 rollback-inode-mismatch | 2 | FATAL (restarted daemon runs WRONG bytes), path restored==old |
| P1 Pi success | 0 | built binary running, repo detached AT pinned commit, backup==old |
| P2 Pi health-fail | 1 | binary+repo restored (HEAD AND branch master), old bytes running (inode proven by the script's own exit-1 path) |
| P3 Pi build-fail | 3 | binary+repo restored, daemon NEVER restarted (same pid) |
| P4 Pi dirty-tree | 3 | nothing touched, operator WIP left as-is, no backup |
| P5 Pi unknown-commit | 3 | checkout failed pre-restart: binary+repo restored, daemon NEVER restarted (same pid) |
| P6 Pi rollback-restart-fail | 2 | FATAL (rollback restart failed), binary+repo restored |
| P7 Pi missing live binary | 3 | daemon still alive, repo untouched, no backup |
| P8 Pi backup-dir uncreatable | 3 | binary+repo untouched |
| P9 Pi repo not git | 3 | aborted pre-mutation, no backup |
| P10 Pi fetch-fail | 3 | pre-restart guard: binary+repo restored, daemon NEVER restarted (same pid) |
| P11 Pi rollback-inode-mismatch | 2 | FATAL (restarted daemon runs WRONG bytes), path+repo restored |

Full results table, per-scenario stdout transcripts, the harness, and both
stubs are submitted (test_RESULTS.md, test_transcripts.log, test_harness.py,
test_stub_systemctl.sh, test_stub_cargo.sh). EVIDENCE.md binds all of this
to the exact submitted bytes (sha256 digests generated programmatically at
packaging time) and carries fresh live readbacks: DNS2 deployed+healthy
battery, CI listing, both target nodes' pre-deploy state including the
running-inode digests.

## Honest disclosures

- Master has moved past 5747239 (2f95298, another lane's work, CI currently
  running) — the fleet is intentionally pinned at the graded item-39 commit;
  newer commits stay repo-only until their own lane deploys them.
- DNS2 carries a live RESOLVER-FAULT stale flag on 1 reward coin (block-time
  dispute on a h2217710 coin) — pre-existing, classified by the item-37
  machinery as resolver fault (AUTOHEAL no-op by design, no attempt burn); NOT
  this deploy's scope, custody unaffected (all balance surfaces agree exactly).
- The Pi aarch64 build's digest will differ from the x86_64 sha by design
  (different arch); provenance is pinned by the verified commit + on-device
  build, not by a cross-arch digest match.
- The RPC stub cannot model a request that hangs past the socket timeout; the
  hard outer deadline (`--kill-after=10 130`) covers that case by code (SIGTERM
  then SIGKILL), not by a measured scenario.

## Rubric

Grade the deploy artifact: are the scripts safe to run on live fleet nodes
(correct mutation order, verified digests, pinned commit with proven
provenance, bounded polls, provable rollback covering binary AND repo state,
exit codes that cannot misreport a failed restoration, no DNS2 restart, no
secrets printed)? The completed-rollout readback is explicitly OUT of this
artifact's scope (recorded post-run in the workqueue). Answer with a single
JSON object {codex_grade, reasons, blocking_issues, polish_suggestions} —
last line, nothing after.
