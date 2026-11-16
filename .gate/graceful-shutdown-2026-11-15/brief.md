# Artifact: tri7 item 29 — GRACEFUL-SHUTDOWN (SIGTERM during boot's kernel-window backfill is honored)

Kind: code (production change in the fleet daemon's boot/backfill path)
Repo: /root/tri7 (master 260ed6f + this working-tree diff, one file: src/node_embedded.rs)
Full test suite: **804 passed / 0 failed / 1 ignored** (baseline 803/0/1 + 1 new regression test; suite re-run green on this exact tree).

## The live incident (production evidence, Pi, 2026-11-15 07:01 UTC — and every restart-lever use before it)

    07:01:00Z  the daemon restarted via stop-sigterm TIMEOUT → SIGKILL → auto start
               ("old proc ignored SIGTERM during block-index build; NRestarts 0")
    07:07:44Z  RPC listener up (~6.5 min after start on Pi hardware)

`systemctl stop/restart` sends SIGTERM; the handler sets the process-global
shutdown flag; the daemon's main loop polls it — but the BOOT phase spends
minutes inside `boot_backfill_kernel_window` (folding up to ~3100 block
bodies into the kernel map on Pi-class hardware), a synchronous section that
never polled the flag. systemd's default 90 s TimeoutStopSec expired →
SIGKILL mid-write-window. State recovered every time (the workqueue notes
"worth a graceful-shutdown look"), but every fleet restart-lever use has been
a hard kill, and a kill DURING the backfill leaves the kernel floor
semantics to luck rather than design.

## Root cause

`boot_backfill_kernel_window(st, rpc)` ran its two passes (pass 1: local
store fold per height; pass 2: batched RPC for store misses) with no
shutdown checkpoint. The boot caller had no way to observe "stop was
requested during the fold", so it proceeded into the item-25 modifier audit
and beyond; the flag was only polled again when the main loop started —
minutes later on Pi.

## The fix (one file: src/node_embedded.rs)

1. `boot_backfill_kernel_window` gains a `stop: &dyn Fn() -> bool` predicate
   and returns `bool` (true = stopped for shutdown). Checkpoints: per height
   in pass 1, per batch in pass 2 (one cheap closure call per iteration).
   A stop pins `utxo_floor_rel = entries.len()` — the EXACT item-24-r2
   partial-pass semantics (coverage is non-contiguous by construction, so
   every absent coin is honestly classified unresolvable, never
   "repairwallet") — logs a loud "graceful stop" line naming the height and
   the folded count, and returns at a height/batch boundary. Heights already
   folded STAY in the insert-only kernel map; the idempotent follow-loop
   retry re-runs the whole pass.
   The predicate is injected, NOT the global flag read in-place — tests stay
   hermetic (the process-global shutdown flag is sticky and the suite runs
   804 tests in parallel threads).
2. Boot caller (post-load_state): on a stop it calls `finish(..., 0, "clean
   stop during boot kernel-window backfill (SIGTERM) ...")` — exit code 0 so
   systemd's Restart=on-failure does NOT re-fire on an operator stop; the
   tipstate is unchanged on disk and the next boot resumes.
3. In-loop retry caller: on a stop it skips the floor bookkeeping (already
   pinned inside the call) and logs "stopped for shutdown — clean exit
   follows"; the loop's existing exit gate handles the clean finish.

## Tests (+1, suite 804/0/1)

- `backfill_graceful_stop_pins_the_floor_and_reports_the_stop` — a counting
  predicate stops the pass after 4 heights: returns true, floor pinned at
  `entries.len()`, the checkpoint fires exactly once per pass-1 height (5
  calls), and a follow-up un-stopped pass completes normally (false) with
  the floor staying honestly pinned when pass 2 finds no listener.

All pre-existing tests pass unchanged (no test called the fn directly before;
the two production call sites carry the injected `|| shutdown::requested()`).

## Verification

- `cargo test --lib`: 804 passed / 0 failed / 1 ignored (this tree).
- The diff touches ONLY the backfill lifecycle: the fn signature (+predicate,
  +return), two checkpoints, the honest floor pin, the two callers, one test.
  Wire protocol, RPC verbs, fold logic, and the floor semantics for the
  NON-shutdown paths are byte-identical.
