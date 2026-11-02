# tri7 phase 5a — tri7d deployment artifact v3 (DNS2, 2026-11-02)

## What
Package the phase-2 tip-follower (`follow_tip`) as a systemd service `tri7d.service`
on DNS2. Read-only P2P client to the legacy daemon (outbound loopback 24113 + RPC
19112). Runs alongside — never touches — `trianglesd.service`.
v3 adds: health timer wiring, 0600-preserving atomic writes, negative-age guard,
idempotent installer, verbatim live evidence file (see tri7d_deploy_evidence.txt).

## Judge-sandbox note (uid mapping)
codex exec runs in a user namespace: foreign uids (tri7=983) appear as
nobody:nogroup there, and files under a 0700 dir are unreadable to the sandbox
user. tri7d_deploy_evidence.txt carries verbatim root-captured outputs (NUMERIC
ls -ln ids, runuser return codes) for cross-checking.

## Layout (idempotent installer: install_tri7d.sh, in this worktree)
- `/opt/tri7/bin/follow_tip` — root:root 0755, release build (rustc 1.99.1,
  repo /root/tri7 phase-5a worktree), sha256
  9f131ec75189a50a77f26c759cd3474811eb8bc691c4b91f85ca32485044e2b3.
- `/var/lib/tri7/` — tri7:tri7 0700; `tipstate.json` tri7:tri7 0600. Seeded from
  `/root/tri7/tipstate.json` (tip h2204064, window_base 2199465, 4600 entries + 1500
  blocks); installer never clobbers an existing tipstate.
- `/etc/tri7/triangles.conf` — root:tri7 0640 copy of the FULL daemon conf
  (follow_tip reads rpcuser/rpcpassword from it; the copy — not a filtered
  excerpt — is deliberate so the client sees identical settings; perms keep
  creds to root+tri7 only). The tri7 user has NO access to /root/.triangles
  (archive arg unused by follow_tip).
- Units in this worktree (installed to /etc/systemd/system/):
  tri7d.service, tri7d-health.service, tri7d-health.timer.
- tri7 user: uid 983, gid 981, nologin (`useradd --system --home-dir /var/lib/tri7`).

## Code change (src/bin/follow_tip.rs, this worktree)
- `--status` healthcheck (file-only: no conf, no network):
  - prints height, blockhash, `last_advance_utc`, `updated_utc`, `state_mtime_utc`,
    `seconds_since_state_write`, `max_write_age`, `stale`, window_base/entries,
    `daemon_tip` (last observed, persisted each write), `at_daemon_tip`.
  - TWO distinct clocks: `last_advance_utc` bumps ONLY on a real tip-height advance
    (advance_with_block, guarded h > tip); `updated_utc`/mtime bump on every periodic
    30s write. Never conflated.
  - exit contract: 0 healthy; 3 (EXIT_VALIDATE) when state is unreadable OR stale
    (write age > `--max-write-age`, default 600) OR mtime is future-dated
    (negative age). A wedged/dead follower stops rewriting tipstate.json →
    --status fails within 10 min by default; threshold explicit + tunable.
    On a static chain (staking off) tip advancement cannot be the liveness
    signal — state-write freshness is; `last_advance_utc` stays null (honest
    "no advance observed") until real blocks arrive.
- `--daemon-health wiring`: tri7d-health.timer runs the healthcheck every 5 min
  (tri7d-health.service: on failure → `systemctl try-restart tri7d` + exit 1 in
  journald). Wedged-but-running followers are now auto-restarted, not just
  observable.
- `--forever`: no run-loop deadline (daemon mode). Without it follow_tip self-exits
  0 after `--seconds`, which a Restart=on-failure unit would never restart.
- `daemon_tip` (getblockcount, refreshed every 30s poll tick) persisted on EVERY
  state write, so `--status` reports it offline.
- Atomic write pins 0600 on the tmp before rename (v1/v2 bug: fs::write default
  0644 clobbered the mode on every 30s swap — observed live and fixed).
- Backward compat: `last_advance_utc` is `#[serde(default)]` — pre-5a tipstate
  files load with "" (reported null), no re-seed needed.
- Exit-code contract unchanged (0/2/3/4/5). 102/102 cargo tests pass.
- rustfmt clean (`rustfmt --edition 2021 src/bin/follow_tip.rs`).

## Unit contract (tri7d.service)
- `Restart=on-failure`, `RestartSec=30`: any non-zero exit (2/3/4/5) or signal
  death → restart after 30s; `systemctl stop` stays stopped.
- `MemoryMax=512M`: measured peak RSS **12.7 MB** (90s live run as tri7 via
  `/usr/bin/time -v`, full state re-verify + 3000-header backfill + 100-block
  backfill + follow loop). ~20x headroom, well under the 512M target, vs legacy
  trianglesd ~1.5 GB.
- Hardening: NoNewPrivileges, ProtectSystem=strict, ProtectHome=yes,
  ReadWritePaths=/var/lib/tri7, PrivateTmp, kernel/controlgroup guards.
- No staking, no broadcast, no listening sockets — outbound only.

## Verification evidence (pre-enable; verbatim outputs in tri7d_deploy_evidence.txt)
- v3 smoke as tri7 (75s): rc=0, 3 polls, no failures, tipstate rewritten by the
  loop at 0600 983:981 (mode survives the atomic swap), SUMMARY JSON written.
- `--status` as tri7 post-smoke: rc=0, stale:false, age 0s, daemon_tip 2204064
  persisted, at_daemon_tip:true.
- Stale gate: 9251s-old state → rc=3 stale:true; missing state → rc=3.
- Ownership numeric: /var/lib/tri7/tipstate.json 983:981 0600; conf 0:981 0640;
  tri7 reads both (runuser rc=0).
- systemd-analyze verify on the real FS (root): exit 0.
- Legacy `trianglesd.service` untouched: active PID 3315888, P2P 0.0.0.0:24113,
  RPC 127.0.0.1:19112, RSS ~1.49 GB — tri7d adds one outbound peer, nothing else.
- Post-enable acceptance: service active; tipstate refreshed (mtime/updated_utc
  twice 60s apart); RSS from systemd/cgroup; NRestarts==0; legacy unaffected.

## Not in scope
- Live staking/broadcast (Sami's explicit go/no-go), commits of other phases,
  staged deploys to sami-pc/DNS3/laptop.
