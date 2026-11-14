# BRIEF — item 24 KERNEL-HORIZON (2026-11-14): a wallet coin older than ~1700 conf became permanently unstakable

## Revision r2 (deploy-found, 2026-11-14 02:52Z): store-first + deferred retry + window-top pin

The fleet live-restart exposed that `tri7d` IS the follower process — the RPC
listener binds only AFTER `load_state`, so the boot backfill's self-dial
always refuses (observed live on DNS3: "rpc connect: Connection refused … 0
block(s) folded", floor pinned honestly, staking on young coins unaffected —
the fail-honest posture did its job). Revision:

1. **PASS 1 reads the LOCAL STORE** (`live_store()` → `raw_body_by_height`)
   — no listener dependency, same bodies the daemon's getblock serves; a
   height the store misses or whose body does NOT hash to the window entry
   (raced store slot, item 18) is a MISS, not a stop.
2. **PASS 2 batched RPC** covers the store misses — at boot it fails fast;
   the **follow-loop retry lane** re-runs the WHOLE pass every 60s (fold is
   idempotent, insert-only) until coverage completes, logging only on floor
   transitions / first attempt / every 10th retry.
3. **Partial-pass floor = entries.len()** (window top): a mid-pass failure
   leaves NON-CONTIGUOUS coverage, which a single floor number cannot
   express — so any hole disqualifies the whole claim and every ABSENT coin
   classifies honestly unresolvable (a coverage fact, never "repairwallet")
   until a full retry lands floor 0. Coins already IN the map keep resolving
   and staking either way (kernel_meta hits bypass the floor — regression-
   tested both sides).

## Change

One file: `src/node_embedded.rs` (+4 tests). Four coupled fixes that make the
kernel-output map (`ChainState.utxo`, the `KernelOutputResolver` BOTH the
solver and `check_proof_of_stake` read) cover the TRUE staking window instead
of the records tail:

1. **`boot_backfill_kernel_window` + `fold_backfill_block`** — at resume-boot,
   the `[base..first_record)` window segment the persisted records do not
   cover is folded from the LOCAL DAEMON (`getblock <hash> false` → raw hex,
   batched 200/call like `rpc_seed_window`). Every body is Hash9-verified
   against the checksum-verified window entry at that height BEFORE
   `apply_block` folds it — the map never learns a fact the window does not
   encode. Full success drops the coverage floor to 0; any failure (RPC down,
   batch mismatch, hash mismatch, parse error) STOPS at that height, logs
   loudly, and leaves the floor at the first unfetched height — never fatal,
   never silent.
2. **`ChainState.utxo_floor_rel`** — new field, the minimum window-relative
   height the map actually covers (0 after seed/genesis/backfill, first
   record after a records-only load). Re-based with the window slide, raised
   only by real trims.
3. **`resolve_stake_evidence` coverage-aware classification** — an absent
   coin whose claimed height sits BELOW the floor is now counted
   UNRESOLVABLE (the validator cannot resolve that prevout either); only an
   absence at/above the floor stays the ACTIONABLE `absent` stale class.
   This kills the false "repairwallet will fix it" advice for horizon
   victims (repairwallet rebuilds `wl.live`, never `stg.utxo` — proven live:
   the 20:2xZ repairwallet returned 0 mismatches while the flag kept firing).
4. **`ChainState::trim` margin** — the in-session utxo trim margin moves from
   `WINDOW_BLOCKS + 200` (=1700) to `WINDOW_ENTRIES + 200` (=4800), which
   saturates to 0 at any reachable tip_rel (≤ 4599): nothing inside the
   window is ever trimmed; coverage slides with the window via the existing
   rebase. The records trim is unchanged (records stay the bounded 1500
   serving tail; tipstate size unchanged). The unresolvable skip log now
   prints the floor and says explicitly that a below-floor coin is a
   kernel-coverage fact, NOT wallet corruption.

## The live defect (measured, host-labelled)

DNS3 2026-11-14 01:1x–01:4xZ: its 100 TRI seed coin
(ca2ec8c4:0, addr TLqn1NwPL7Mp2ofmeHKgM6KiHRUM7X82Wn, conf 4429, h2206981) is
PERMANENTLY UNSTAKABLE since 18:41:41Z (1175+ impairment ticks, re-detected
every boot; getstakinginfo attempts=0, coins_considered=1, searches
incrementing). The coin is CANONICAL + UNSPENT + OWNED + MATURE — proven via
DNS2 gettxout (tri7_source=live-view, 100.0 TRI, conf 4429), block-hash
parity at h2206980/81/82 between DNS2 and DNS3, and getblock h2206981
serving the full funding-tx body on BOTH. Wallet listunspent on DNS3 shows
exactly this one coin. The earlier "adjudicated phantom" verdict is
corrected by this item: not a phantom — a horizon victim.

## Root cause (code path, exact)

- `tipstate.json` full-tx records (`blocks` key) persist only the trailing
  `WINDOW_BLOCKS=1500` heights (trimmed in-session and bounded on write).
- `load_state` rebuilds `stg.utxo` from those records ONLY → the map covers
  ~[tip−1499..tip].
- The staking WINDOW (`entries`) is `WINDOW_ENTRIES=4600` heights, and
  `resolve_stake_evidence`'s absent-classification tested ONLY the entry
  window: any wallet coin with conf in (1700, 4600) is inside the
  classification window but structurally absent from the map → flagged as
  the ACTIONABLE stale-live-view class (implying repairwallet), excluded
  from staking forever.
- Worse: even a hypothetical solved kernel could not self-validate —
  `check_proof_of_stake` resolves through the SAME map.
- The old in-session trim (`tip_rel − (WINDOW_BLOCKS+200)`) would have
  re-created the hole mid-session even if the boot map had been full.
- Fleet impact: latent staking-death for ANY coin aging past ~1700 conf
  (~29h) on every follower node — DNS2/Pi are unaffected only while their
  coins stay young. DEADLINE context: DNS3's coin exits the 4600 window at
  ~h2211582 (~3-6h from 01:40Z); this fix lands before the crossing.

## Why this shape (and not the alternatives)

- *Alternative (i): raise `WINDOW_BLOCKS` 1500 → 4600.* Fixes future boots
  but NOT any already-affected coin: a records-only boot still rebuilds the
  map from the OLD tipstate's 1500 records (the file on disk does not grow
  retroactively), so DNS3's coin stays dead for ~3100 more blocks. Also
  triples tipstate size/flush (2.9 → ~8.7 MB). Rejected.
- *Alternative: resolver clamps everything below `tip−1500` to
  unresolvable, no backfill.* Honest classification, but the coin still
  never stakes — the defect's harm (dead staking) would remain. Rejected.
- *Chosen (ii)*: keep records at 1500 (tipstate unchanged, rollback/
  append-on-accept machinery untouched), backfill the missing segment from
  the daemon at boot (the daemon provably serves the window — the defect
  diagnosis itself read the full body via getblock), and classify by the
  TRUE floor. Boot cost: ~3100 batched RPC getblock calls ≈ seconds.

## Safety analysis

- The backfill folds via `apply_block` — byte-identical facts to what the
  live acceptance path produced for those same blocks (same function, same
  offsets), and only for bodies whose Hash9 matches the window entry the
  checksum chain already verified. A stale/reorged body CANNOT enter the map.
- Failure posture is fail-honest, not fail-open: any batch fault (RPC error,
  size mismatch, missing hex, hash mismatch) → `fold_backfill_batch` /
  the caller pins `utxo_floor_rel = first_uncovered_rel + 1` (the floor is
  the lowest COVERED height) → below-floor coins counted unresolvable (the
  honest pre-existing class), staking continues on young coins. Both
  fail-honest shapes have regression tests (mid-batch body fault → Err at
  the bad height; batch-boundary fault → Err at chunk_start even when a
  later body would verify).
- Retry semantics: the backfill is ONE-SHOT per process (a deliberate boot
  phase, like seeding/backfill-header sweeps — the follow loop never re-runs
  boot phases mid-session). A partial stop stays honest for the whole
  session; the next restart re-runs it from the records tail. The floor is
  not persisted, so no stale pin can outlive the process that earned it.
- Rollback/adoption paths unchanged; rollback never trims the map, so the
  floor stays valid under short reorgs (fleet reorgs are ≤ tens of blocks,
  far above the floor).
- No behavior change for young coins (<1700 conf): they resolved before and
  still resolve (regression suite + existing resolver tests all green).

## Tests (cargo test --lib 781 passed / 0 failed / 1 ignored)

- `deep_window_coin_conf_4400_resolves_and_stakes_when_the_kernel_map_covers_the_window`
  — THE item-24 regression: a coin at window-relative 199 (conf 4400,
  DNS3's exact shape) resolves through the shared evidence path, solves via
  `create_coinstake`, and the assembled signed block passes
  `check_proof_of_stake` resolving `stg.utxo`.
- `records_only_boot_absence_below_the_coverage_floor_is_unresolvable_not_actionable`
  — below-floor absence = unresolvable (1), above-floor absence = the
  actionable absent stale class (phantom semantics preserved).
- `kernel_window_trim_keeps_deep_window_coverage` — trim at the window edge
  keeps rel-200 and rel-2898 outputs (the old margin dropped rel < 2899).
- `fold_backfill_block_verifies_hash_and_applies` — true body folds with
  exact `apply_block` facts; a wrong body for a height is refused, map
  unchanged; garbage hex refused.
- `backfill_batch_partial_failure_pins_the_floor_above_the_bad_height` —
  mid-batch body fault: good prefix folded, Err AT the bad height, the
  caller's exact pin expression leaves the first uncovered height honestly
  unresolvable.
- `backfill_batch_boundary_fault_reports_at_chunk_start` — batch-boundary
  fault (daemon error entry): Err AT chunk_start, nothing past the fault
  folds even when a later body would verify; a later all-true batch folds
  but the pin survives until overall success lowers the floor.

## Rollout (standing rules: judge → commit → push → deploy fleet-wide)

DNS3 first (the deadline node), then DNS2 (sole heavy staker), then Pi
(on-device build, aarch64). Verification: DNS3 impairment clears
(boot backfill log "floor 0", stale flag for ca2ec8c4 GONE,
coins_meeting_min_age ≥ 1, attempts climbing); DNS2/Pi staking unregressed
(attempts advance, no new stale flag, tip parity holds).
