# DNS2 staker memory headroom — 2026-11-25

Workqueue item **47 MEMORYMAX-HEADROOM**. Scope is the tri7d unit on DNS2, the
sole heavy staker. Nothing else on the fleet was changed.

Memory figures in MiB are bytes from the evidence files divided by 2^20,
rounded to one decimal, unless a log line is quoted.

## Change on the host

One systemd drop-in,
`/etc/systemd/system/tri7d.service.d/50-item47-memory-headroom.conf`
(sha256 `2e912d08421143388de46af3d7364cb27d8b1a7ee8ddea799f0800cd195d5f24`):

```ini
[Service]
MemoryMax=2G
MemorySwapMax=0
```

The base unit is untouched; its `MemoryMax=512M` line stays and the drop-in is
the last writer of the key. `MemoryHigh` stays `infinity`. No code or binary
changed: the running binary's sha256 prefix is `922357122aa7b961` on DNS2 and
DNS3 before and after, `ea0d25466bb90eee` on Pi.

The unit template in `deploy/tri7d.service` now carries the same two keys so
that a fresh install does not return to the old sizing.

## What was wrong

Measured over the 7 h 17 m from the host boot to the deploy
(2026-11-24T20:17:15Z to 2026-11-25T03:34:49Z), from the unit journal and the
cgroup's `memory.events`:

- 28 tri7d boots, **21 ended in a cgroup OOM kill**. `memory.events` counts
  21 `oom_kill`, and the kernel journal holds 21 matching
  `Memory cgroup out of memory: Killed process … (follow_tip)` lines, the
  first of them with `anon-rss:519928kB`, just under the 512M cap.
- Median **487 s** with no staking after each kill; **12,846 s** dead in total,
  49% of the window. Only 14 boots reached staking.
- In steady state the cgroup sat at its 512M ceiling: `memory.current` was
  512.0 MiB in every one of 206 one-second samples (01:43 to 01:47Z), with
  11,192 `max` events and 22.1 MiB pushed to swap.

The old sizing in the repo (12.7 MB peak RSS, "~20x headroom") came from a
90-second run on a short chain and no longer described the daemon.

## Why 2G and not 768M or 1G

- The same binary on DNS3, where swap is allowed and the cap is 3G, over 12
  stopped boots in 14 days: RAM peak up to **907.2 MiB**, swap peak up to
  **455.7 MiB**, RAM+swap upper bound **1306.6 MiB**. The two peaks need not
  coincide, so the sum is an upper bound, not a measured footprint.
- The peak comes early. Boots that lasted only 12 and 19 minutes of
  wall-clock time already peaked at 875.7 MiB and 907.2 MiB, the latter the
  largest RAM peak in the table. The footprint is a boot-time transient, not
  steady growth. Its suspected cause is workqueue item 48 (balances replay);
  that is not proven here and not fixed by this change.
- 2G is **1.57x** that largest bound. 768M and 1G sit under it and would
  reproduce the OOM loop on a bad boot.
- DNS2's own per-boot peaks (up to 887.0 MiB, systemd's RAM+swap peak) are
  not a sizing basis. They are bounded by the ceiling that was killing the
  process, so they only give a lower bound on what the daemon needs.
- Host: 12,541,485,056 bytes total (11,960.5 MiB), 5,935,779,840 bytes
  available (5,660.8 MiB) in the harness's 03:35:03Z reading, recorded in
  bytes in `ev_deploy.json`. The 03:31:58Z preflight had read 5,847.6 MiB
  available. The rule applied is ceiling + 1 GiB for the host (3,072 MiB
  needed).
- `MemorySwapMax=0`: the host's swap is effectively full (270,336 bytes
  free of 8,589,926,400, about 0.3 MiB of 8,192 MiB, at 03:35:03Z), so a
  spike could not hide there anyway. With swap disabled every
  spike is visible in `memory.peak` and `memory.events`.

## Accepted live result

Deploy run `20261125T033410Z` exited **0** (`DEPLOY_ACCEPTED`) at
03:59:45 UTC. Two consecutive boots under the new limits, each gated on its
own main pid, RPC readiness, live staking, stake attempts advancing, zero
cgroup `oom`/`oom_kill`, zero OOM journal lines with a positive control,
swap 0, and a non-reclaimable peak under 80% of the ceiling:

| | Boot A | Boot B |
|---|---|---|
| Started | 03:36:41Z | 03:51:11Z |
| Stop before it | 90.3 s, ended in SIGKILL | 90.4 s, ended in SIGKILL |
| RPC ready | 21.2 s after start | 21.1 s after start |
| Staking live | 183.4 s after start | 183.0 s after start |
| First stake kernel solved | 768.7 s after start | 221.4 s after start |
| Blocks minted before the observation ended | 1 | 4 |
| Observed, start to end of the observation | 13 min, to its stop | 8.5 min, to acceptance |
| Peak non-reclaimable (anon + kernel + swap) | 467.0 MiB | 480.9 MiB |
| Highest 1 Hz sample of `memory.current`, including page cache | 1492.2 MiB | 630.5 MiB |
| Cgroup high-water mark `memory.peak` at the end of the observation | 1567.8 MiB | 682.7 MiB |
| `memory.events` max / oom / oom_kill | 0 / 0 / 0 | 0 / 0 / 0 |
| `memory.swap.current` | 0 | 0 |
| OOM lines in the unit journal | 0 | 0 |

The accepted run's summary line prints the boot B peak as 480.9 MiB; the boot
record, which the table follows, holds 504,250,368 bytes.

The two page-cache rows are different horizons: the sampler's highest sample
inside the gated window, and the kernel's own high-water mark for the whole
boot read just before the observation ended. About 940 MiB of boot A's is
reclaimable file cache; neither boot reached the 2048 MiB ceiling.

`NRestarts` stayed 0 and the main pid was constant in both boots. The only
error-class journal lines were the known cold-start noise (16
`kernel-window backfill: rpc batch failed`, 11 `stake tick: wallet live view
not ready` per boot), noise that was already present before the change.

Fleet at acceptance and at an independent readback at 04:02:10 UTC: DNS2,
DNS3, and Pi all at height **2,219,300**, staking enabled, protocol 70206
`/tri7:7.0.0/`, binaries unchanged.

Before and after, the time to the first stake kernel is not a like-for-like
comparison. Before, the 14 boots that reached staking had a median of 278 s
(range 0 to 1,313 s). After: 769 s and 222 s. Two boots cannot settle that;
stake solves are random. The clear change is the OOM loop: 21 kills and
3 h 34 m of dead time in 7 h 17 m before, none in the 22-minute window after.

## What is not proven

- **Duration.** Two boots of 13 and 8.5 minutes. On DNS3 the boots that ran
  for hours reached the 1.28 GiB upper bound; a DNS2 boot that long under 2G has
  not been observed yet. `memory.events` `max` is the signal: any non-zero
  value means the ceiling was touched and the sizing needs another look.
- **Page cache.** Boot A's cgroup high-water mark was 1568 MiB and its
  highest 1 Hz sample 1492 MiB, about 940 MiB of it reclaimable file cache.
  It never reached the ceiling; if it had, the kernel would reclaim the cache
  before any OOM kill.
- **The transient itself** (item 48) is not fixed. The ceiling is sized above
  it.
- **SIGTERM.** In both controlled stops the daemon did not exit on SIGTERM
  while healthy and staking; each stop ran to the 90 s `TimeoutStopSec` and
  ended in SIGKILL. This is pre-existing, unrelated to memory, and is not
  addressed by this item.

## The first live attempt was rolled back

The first live run (`20261125T032108Z`) applied a drop-in with the same two
directives at 03:21:57Z, exited **1**, and had the rollback verified at
03:22:55Z, 58 seconds after the apply. Readback confirmed the baseline: config
`MemoryMax=536870912` / `MemorySwapMax=infinity` restored, drop-in removed,
running cgroup back to 536870912 / no swap limit, `verified: true`.

No gate was reached and no boot was observed, so that run proves nothing
about the ceiling. It does prove the rollback path. The cause was in the
harness: `systemctl stop` returned early because `tri7-addrindex.timer`
(every 10 min) started a unit with `Requires=tri7d.service`, whose start job
replaced the stop job while tri7d was still `deactivating`. The harness now
waits out the stop, or observes the restart that another job triggered, and
records which happened. The condition was reproduced on a throwaway unit
before the second run (probe: 20 of 20 checks held).

## Judging

Codex was quota-walled; the GLM stand-in judge graded the deploy:

- Round 1: **B**, two blockers (journalctl return code ignored; no positive
  control on "zero OOM lines"). Both fixed and re-graded.
- Round 2: **A**, `urn:ump:vayutfzwq4bqq77jv7xcqdadnw75dr6bf4rco37twdpppkn7hfqq`.
- Round 3, on the exact harness bytes that ran (sha256
  `ee43c53c974e75c77cea2046d36d8e135417dfc3515faa9cca73832903b4fe92`):
  **A**, `urn:ump:ncehue76tacwi4m3fb5g57fdcyw7vwxomkmhtnza6fy2lhkocfyq`.

The repo closeout grade is recorded in the commit message.

## Evidence

Held on DNS2 in `/root/tri7/.gate/memory-headroom-2026-11-25/`:
`item47_deploy.py` (the harness; `--selftest` runs 55 checks),
`item47_probe_mechanism.py`, `dropin_applied.conf` (the drop-in exactly as
applied), `dropin_preview.conf` (the earlier preview shown to the deploy
judge; it differs from the applied file only in the comment's re-measured
host numbers, see `dropin_preview_vs_applied.diff`), `ev_before.json`,
`ev_preflight.json`,
`ev_boot_A.json`, `ev_boot_B.json`, `ev_samples_A.json`, `ev_samples_B.json`
(1 Hz cgroup samples), `ev_deploy.json`, `ev_deploy.log`,
`ev_rollback_attempt1.json`, `ev_live_attempt1_rolled_back.out`,
`ev_mechanism_probe.log`, `mem_cross_node.txt`, the three judge outputs, and
`BRIEF_READ_FIRST.md` / `JUDGE_R1_RESPONSE.md`.

## Rollback

Delete the drop-in, then `systemctl daemon-reload` and `systemctl restart
tri7d`. `MemoryMax` returns to `536870912` and `MemorySwapMax` to
`infinity`. This was rehearsed on a scratch unit and exercised for real by
the first live attempt.

## Not changed

- `deploy/samihost/tri7d.service` still says `MemoryMax=512M`. That host was
  not measured in this item and needs its own numbers before the value moves.
- `deploy/tri7d_fresh_install.sh` installs `MemoryMax=3G` with swap allowed.
  It is already above the measured bound and was left alone.
- DNS3 and Pi keep their own limits (3G with swap allowed, and unlimited).
