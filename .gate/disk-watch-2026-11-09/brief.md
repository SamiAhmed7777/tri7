# Brief — workqueue item 13 DNS2-DISK, watch upgrade (rev4 → rev5)

## Context
DNS2 root fs hit 96% (7.8G free) — the existing disk-guard (daily 05:20,
ALERT_PCT 90, Telegram alert) DID fire this morning ("ALERTED 96%", journal
05:22:07Z), but daily cadence is too slow: the tri7 fork-repair workflow can
write ~5 GB of store copies in under an hour, and the fs has flipped
read-only once before. Item 13 bar: reclaim + watch.

## Reclaim already executed live (this tick, guarded per-target, manifest
/root/tri7-lab/reclaim-2026-11-09.manifest): DNS2 96% → 90%, 21G free.
- /root/tri7-recovery/canonical-snap{,2,3,4}.dat (4 x ~984 MB, Nov 4 fork-repair
  snapshots; superseded by live /var/lib/tri7/blk0001.dat 982,445,176 B Nov 9,
  canonical replicated on 3 other hosts)
- /var/lib/tri7/blk0001.dat.pre-canon (985,316,756 B, replaced Nov 8)
- /tmp tick artifacts ~3.0 GB (state-dir copies, CI exe downloads, wallet
  key-dump txt files — also a security win)
- /root/.codex/sessions files >7d: 1.66 GB (12,587 files)
- /root/tri7/target via cargo clean: 6.5 GB (regenerable build cache)
sam-pc C: has 260G free (no pressure) but state2 carried ~5.9 GB of dead
fork stores/indexes/tipstates (forkB/forkC/wdog/gapfix/stale, all declared
"discarded material" by item-12); deleted by explicit-name .NET Delete calls;
live pair untouched (blk0001.dat 982,445,176 B == DNS2 byte-size, tipstate
advancing, daemon flushing). Four-way tip parity verified post-cleanup:
DNS2 == PC exact (2208366, 37f9529c…).

## What is being judged: the WATCH upgrade (rev5)
The watch already exists (daily timer → /root/security/bin/disk-guard.py,
Telegram alert) — this diff upgrades it:
1. Timer daily → hourly (OnCalendar=hourly, Persistent catch-up, 10m jitter).
2. Two new SAFE reclaims in the rev4 argv-only style (both verified live by
   this tick's manual reclaim — they are the two junk classes that actually
   grew):
   - /root/.codex/sessions files older than 7d (-size +1k)
   - /tmp TOP-LEVEL regular files older than 7d (-maxdepth 1 -type f — never
     descends into service dirs, never touches sockets or dirs)
3. >=90% alert now includes the top-5 first-level space offenders
   (du -x -m --max-depth=1 / parsed in python, argv-only) so the Telegram
   alert is actionable without ssh.
rev4 invariants kept: no shell pipelines, argv-only with real exit codes,
notification failure exits nonzero, cleanup failures alert even when healthy,
silence only when healthy + no failures + nothing freed. ALERT_PCT stays 90.

## Known-safe analysis for the two new find calls
- codex sessions: judge transcripts; the lessons they fed are in UMP + skills;
  /root/.codex/sessions is pure append-only transcript data, recreated per
  session. 7d retention keeps any active investigation window.
- /tmp top-level: this host's tick scratch; systemd-tmpfiles already owns
  /tmp aging (10d default) — the 7d pass only front-runs it for root-owned
  leftovers; -type f never matches sockets/pipes; -maxdepth 1 never descends
  into per-service subdirs (X11, snap, systemd-* are dirs).

## Growth drivers named (skill rule: reclaim must name the driver)
1. tri7 fork-repair store copies (~1-5 GB per fork event, both hosts) —
   episodic spike driver; policy recorded: repairs must delete their copies
   at tick end (skill delta pending).
2. cargo /root/tri7/target (~6.5 GB regenerates per build cycle) — symptom,
   legitimate to clear.
3. /root/.codex/sessions (~1.7 GB/week) — now auto-pruned at 7d.
4. /tmp tick scratch (~1-3 GB/event) — now auto-pruned at 7d.
5. /root/.hermes/state.db (3.8 GB conversation store) — slow background
   driver; FTS rebuild needs a gateway-stopped maintenance window; NOT
   touched this tick (cheap reclaims reached target first, per skill).

## Post-change verification performed after apply (documented in tick report)
python3 -m py_compile, one manual service run at healthy disk (silent-exit
path), next timer trigger listed hourly, parity + fleet untouched.

## Round 2 — polish taken from round-1 (grade B, urn:ump:yfylwzrffww7jwn7zcul7wislro23mdojhg3a6win3lbtxhsmbia)
1. TAKEN: codex-sessions find now pre-checks os.path.isdir and records
   "skipped: absent" instead of a spurious failure alert.
3. TAKEN: alert hint no longer mentions /var/lib/docker (removed from this
   host 2026-09-14); now points at the Top space list.
4. TAKEN: alert carries the before-pass percentage ("was N% before this
   pass"), b_pct captured before the reclaim sequence.
5. TAKEN: disk-guard.service.unchanged captured in the bundle (unit is
   untouched; only the timer schedule + script change).
2. SKIPPED (cosmetic, sort key explicitness — behavior identical).
diff regenerated (disk-guard.diff, 124 lines); py_compile clean.

## Round 3 — polish taken from round-2 (grade B, urn:ump:brdgagjtn3rsvfdph3xqnhpbpeabx7ialx7wjuj4y4poh5yuhizq)
1. TAKEN as post-apply verification: alert branch exercised once with a
   raised threshold + notify monkeypatched to stdout (no ops-chat spam —
   real Telegram delivery is already proven by this morning's real
   "ALERTED 96%" firing); Top-space formatting proven live.
2. TAKEN: b_pct now the first statement of main() (true pre-pass figure).
3. TAKEN: disk-guard.service Description now "safe hourly vacuum + 90%
   alert" (disk-guard.service.new in the bundle; replaces the .unchanged
   capture since the unit IS edited now).
4. TAKEN: /tmp find now -user root (matches the brief wording; never
   deletes another user's file).
diff regenerated; py_compile clean. Also run from a stopped-timer sanity
window: systemctl daemon-reload + restart AFTER the verdict, per gate order.

## Round 4 — REAL DEFECT caught by the post-apply alert-branch test
The monkeypatched test run rendered the alert WITHOUT the "Top space:"
section. Root cause: top_offenders() went through sh(), which truncates
stdout to the LAST 120 chars — a full du listing never survives — and du /
returns nonzero on live /proc races, so the rc!=0 guard always bailed.
FIX: top_offenders() now runs du itself with full-stdout capture, IGNORES
du's returncode deliberately (documented: /proc vanishing mid-walk makes du
nonzero while stdout is complete), keeps the argv-only invariant, and takes
the round-1 sort-key polish too (key=lambda row: row[0]).
Also acknowledged-not-taken from round 3: token() missing-key guard
(behavior already fails closed via notify()) and concurrent-write freed-
delta skew (cosmetic alert text).
Post-fix verification: alert-branch test re-run MUST render the Top space
section with plausible MB numbers before the swap is called done.
