# BRIEF — item 23 (convergence blocker): adoption body-request pipeline pins forever

## Change
One file: `src/node_embedded.rs` (diff in this bundle). No behavior change
outside the follow-loop body-request pipeline + one Stats field.

## The live defect (measured on DNS2, 2026-11-11, host-labelled)

DNS2 daemon pid 3613030 (`/opt/tri7/bin/follow_tip`, binary sha256 prefix
34c155413c995a2de00eacbf = the item-21 build of 2189ad8) sat pinned at the
fork from 10:17:58Z through at least 12:17:55Z (2h+ observed live this tick):

- The reconciliation judge fired every 60-90 s with IDENTICAL verdicts while
  `getblockcount` never moved:
  - `12:12:01Z FORK CHOICE (reconciliation): their branch h2209651 carries
    more work (2088307590571950268743680) than ours h2209645 (0) — rolling
    back to h2209645, adopting 6 block(s) via the validated body path`
  - `12:12:21Z ... their branch h2209653 ... (2601030464082169349275648) ...
    adopting 8 block(s) ...` (same totals re-fired at 12:13:22, 12:14:23,
    12:15:25 ...)
- Zero block bodies ever arrived: no `TIP ADVANCE`, no `ignoring
  non-extending block`, no validation-failure line in the window (a
  validation failure would have logged and dropped/killed something — the
  process is alive and all 5 peers have 2h+ uptime).
- Meanwhile DNS2 kept staking on the fork base and its own mints were
  abandoned by the next judge cycle: `STAKE MINTED → h2209647 bb3d2590…`
  (12:09:58Z) and `→ h2209646 9d453655…` (12:17:55Z) — minted, relayed,
  rolled back, re-minted: wasted solves every cycle.
- Fleet split persisted at probe time (12:13Z): PC 889d9205@2209651, DNS3
  d5b784f0@2209651, Pi 39a39802@2209653, DNS2 pinned f45ece3e@2209645. All
  four run the same flawed pipeline.

## Root cause (code path, exact)

The follow-loop body-request dispatch (`src/node_embedded.rs`, main follow
loop) worked like this before the fix:

1. Batch build popped pending `(hash, height)` entries and inserted each
   into `outstanding` IMMEDIATELY — before any target was resolved and
   before the getdata was actually sent.
2. `choose_body_source_label` resolved the target (advertiser → first
   outbound → first live). A serving peer that does not hold a requested
   block answers NOTHING (the serving arm silently skips unknown hashes —
   verified in the same function), and a stale advertiser label falls back
   to the first outbound peer, which may hold none of the adopted branch's
   bodies (e.g. DNS2's outbound tor peer holds only the branch its own node
   minted).
3. There was NO timeout, NO requeue, NO rotation: an unanswered getdata left
   its hash in `outstanding` forever. Every recon re-fire re-pushed the same
   hashes; the dispatch skipped them as `outstanding` duplicates; the batch
   came back empty; nothing was re-sent.
4. At `MAX_OUTSTANDING_GETDATA = 64` (reached ~10 min into the live incident
   as rivals' branch hashes kept evolving) the dispatch gate
   `outstanding.len() < MAX` turned permanently false: NO getdata was ever
   dispatched again, while the judge re-fired identical verdicts forever.
   This is exactly the signature recorded in tri7-fleet-chain-state §16
   item-22(a) ("adopted branch is not persisting … the classic cause is an
   adoption getdata path with no timeout/retry/peer-rotation") whose cure
   was recorded but never landed. The from-zero seed path already has the
   deadline pattern (90 s `seed: getdata timeout`, same file) — the
   follow-loop path never got it.

## The fix (four layers, matching the recorded cure shape)

1. **Never pin an unsent request**: the batch is built WITHOUT touching
   `outstanding`; hashes are inserted ONLY after a successful
   `sess.send`. A `None` target or a send failure requeues the batch at the
   FRONT of the pending queue in chain order (`requeue_unsent_batch` —
   reversed push_front restores ascending order) with a log line.
2. **Age-out**: `age_out_unanswered_bodies` runs each loop tick; requests
   unanswered for `GETDATA_TIMEOUT_SECS = 45` s leave `outstanding` and are
   requeued in chain order (ascending height) for re-dispatch.
3. **Rotation**: `BodyTries` per hash records `sent_at`, `attempts`, and the
   labels that already ignored the request; `choose_body_source_label` (and
   `resolve_body_send_target`) now take an `avoid` list and skip tried
   labels in every preference tier (advertiser → outbound → any live), so a
   retry lands on a FRESH peer. When every live peer has been tried, the
   original order is restored (retrying beats starving — a recovered tunnel
   answers on retry). Rotation memory washes out at `BODY_TRIES_WASHOUT =
   12` attempts so a new adoption era starts clean. Memory is freed on the
   same two sites that free `outstanding` (validated arrival; the
   no-longer-extending skip).
4. **Fail-loud**: at `MAX_BODY_ATTEMPTS = 6` attempts (then every 4th) the
   age-out logs
   `adoption body STALL: N body request(s) hA..hB unfetched after K
   attempt(s), peers tried [labels] — requeued, rotating targets (the peer
   set may not hold the adopted branch)`. `stats.adoption_body_stalls`
   counts age-out scans and is surfaced in `stats_json` (the exit SUMMARY).

## Why this fixes the live incident

With the fix, DNS2's next adoption attempt can no longer wedge: the first
unserved getdata ages out after 45 s, the batch requeues in chain order, and
the re-dispatch rotates to another peer (DNS2 currently holds five peers
across three branches — two hold Pi's branch incl. the outbound tor peer,
one holds DNS3's, one the PC's) until one that actually holds the adopted
branch serves the bodies; the tip then advances through the validated body
path exactly as in the 05:47:33Z live adoption (pre-pin proof the path
works when bodies arrive). If NO peer can serve, the STALL line says so
loudly with the tried-peer list instead of looping silently forever.

## Tests (all green; reproducible evidence in this bundle)

- Suite: `cargo test --lib` → **731 passed, 0 failed, 1 ignored**
  (`test-evidence.txt` in this bundle: repository URL, FULL base-commit
  SHA, the OFFLINE exact-reconstruction path — `patch -p1
  base-node_embedded-f08d29d.rs < adoption-body-stall.diff` reproduces
  `patched-node_embedded-r6.rs` BYTE-FOR-BYTE (sha256
  1b587b85…06741), proven by `verify-offline.sh` in `transcript.txt` —
  plus the exact test commands, full-suite tail, and per-test lines of
  the item-23 test set). HEAD baseline for `src/node_embedded.rs` is
  113 `#[test]` fns, this tree carries 121 — **+8 new test functions**
  (earlier r1 framing compared against the pre-merge item-21 count; HEAD
  includes sibling-lane tests merged since).
- `unanswered_bodies_age_out_in_chain_order_and_keep_rotation_memory` —
  out-of-order entries age only past the timeout, return ascending, keep
  attempts/tried for rotation; fresh requests stay.
- `requeued_unsent_batch_lands_in_chain_order_at_the_front` — reversed
  push_front restores chain order ahead of later entries.
- `aged_body_requeue_rotates_off_the_silent_advertisers_socket` —
  socket-level: the avoided advertiser is skipped even as the recorded
  source; the getdata frame lands on the untried outbound peer's socket and
  the avoided socket stays silent.
- `mixed_retry_histories_split_the_batch_per_hash` — (r1 judge) mixed
  histories split the batch per hash with per-group avoid lists; shared
  histories still join into one batch.
- `out_of_order_requested_block_is_kept_and_refreshed_not_dropped` —
  (r2 judge) a requested block arriving before its predecessor is KEPT
  with a refreshed age-out clock, not dropped; unknown hashes stay on the
  legacy ignore path.
- `failed_groups_requeue_in_one_globally_chain_ordered_operation` —
  (r2 judge) multiple failed groups requeue via ONE height-sorted
  operation: pop order 90,91,92,89.
- `body_source_choice_prefers_the_recon_advertiser_over_healthy_outbound` —
  extended: tried advertiser/outbound skipped first; exhausted rotation
  retries advertiser then first live (no starvation).
- Pre-existing item-21 wiring tests (advertiser-first, retention, DropPeer
  erasure) updated to the new signature with empty avoid lists and still
  green — no behavior change for the first attempt.

## r1 verdict → r2 deltas (judge urn:ump:7dq5ri3cfcp342mwnodkerzmcnhcca2e65m7omuy75apcsjetppa, C)

1. **Stall accounting**: the loud gate now reads the batch MAX attempts
   (`aged.iter().map(...).max()`), not the highest-height entry's count —
   mixed-attempt batches cannot suppress or misstate the stall line.
2. **Per-hash rotation guarantee**: the flat union avoid-list is replaced by
   `group_batch_by_history` — the batch is split into history groups (sorted
   tried-peer signature) and EACH group is dispatched with its own avoid
   list, so no hash is ever re-sent to a peer that ignored IT. Common case
   (chain-adjacent hashes, identical history) is one group: batching
   unchanged. Tested by `mixed_retry_histories_split_the_batch_per_hash`.
   Residual, documented: when EVERY live peer is in a group's own history,
   the retry tiers fall back to advertiser-then-first-live — that re-send to
   a known ignorer is the deliberate no-starvation trade-off and is exactly
   what the fail-loud STALL line reports.
3. **Capacity cap enforced**: the batch size is
   `GETDATA_BATCH.min(MAX_OUTSTANDING_GETDATA - outstanding.len())` — the
   gate plus a full batch can no longer overflow the 64-entry cap.
4. **Test count reconciled + reproducible evidence**: corrected numbers
   above; `test-evidence.txt` ships the `cargo test --lib` tail from this
   tree; the diff and the brief are in the judge's bundle.
5. Polish taken: the advertiser `untried` check no longer allocates a
   `String` (closure takes `&str`).

## r2 verdict → r3 deltas (judge urn:ump:rgstncln5voqktvxh6euwdxcasnruqtsuxedxwm5hzc2t7wqh4sq, C)

1. **Out-of-order arrival no longer drops requested blocks** (the real
   ordering defect the judge caught): with per-history groups a higher
   group CAN answer before its predecessor. The old non-extending arm
   removed the hash from `outstanding` (and its rotation memory) — the
   block stayed missing until the next recon re-fire. Now a REQUESTED
   non-extending block is KEPT: `refresh_body_request_clock` restarts its
   45 s age-out window and it stays in `outstanding` — it either extends
   once its predecessor lands (the normal out-of-order case closes in
   seconds) or re-dispatches after the timeout with the STALL line going
   loud if the predecessor never arrives. Never-requested blocks keep the
   legacy "ignoring non-extending block" path (the helper returns false).
   Regression: `out_of_order_requested_block_is_kept_and_refreshed_not_dropped`
   (refresh → not aged out by the scan; unknown hash → false).
2. **One globally chain-ordered requeue for failed groups**: the per-group
   front-pushes are gone; failed groups accumulate into ONE `unsent` list
   (group-encounter order) and `requeue_unsent_batch` now HEIGHT-SORTS the
   list before the reversed push_front — concatenated groups can be
   non-ascending (F=[90,92], T=[91]), and the sort is what restores global
   chain order. Regression:
   `failed_groups_requeue_in_one_globally_chain_ordered_operation`
   ([91],[92],[90] → pop order 90,91,92,89).
3. **Reproducible evidence**: `test-evidence.txt` carries the HEAD commit,
   the patched file's sha256, the exact command, the full-suite tail, and
   the per-test lines of the whole item-23 test set. Post-push the commit
   is public on GitHub master, making the run reproducible at that hash.

## r3 verdict → r4 deltas (judge urn:ump:yahmgogrmepnlzpe6hsjtw7km6bvp66evt7igbwpsprdanrzjcrq, C)

1. **The out-of-order successor's BODY is now used, not just the request**:
   the arrival path already caches every block payload in `recent` (a
   non-extending arrival is cached at height 0). After a tip advance, the
   new `find_cached_successor` helper finds an outstanding request at
   tip+1 whose body is cached and the message loop RE-INJECTS the cached
   payload into the queue — the successor then takes the FULL validated
   path (fork choice → validate → advance → durability → addrindex) within
   the same pass, cascading while the next block is also cached. No 45 s
   wait, no re-fetch, nothing lost. The fallback when no body is cached is
   the r2 behavior: the request stays outstanding and re-dispatches after
   the age-out (loud via the STALL line). Regression:
   `cached_successor_is_found_after_the_predecessor_lands`.
2. **Wording fixed (polish)**: the r2/r3 text said the block was "KEPT" —
   inaccurate; the request marker was retained and the payload discarded.
   r4 retains the payload too and applies it by reinjection; the brief now
   describes the exact path (arrive-out-of-order → cached at height 0 →
   predecessor lands → find_cached_successor → reinject → validated path).
3. **Full reproducibility identity**: `test-evidence.txt` now carries the
   repository URL, the FULL base-commit SHA the diff applies onto
   (immutable, pushed), the exact `git apply` + `cargo test --lib`
   commands, the patched-file sha256 with its producing command, and
   per-test lines for the item-23 set. The graded diff is committed with
   the grade URN in its message and pushed to the same repository right
   after the gate passes (the gate requires the grade before the commit,
   which is why the graded state is diff-on-base in the bundle).

## r4 verdict → r5 deltas (judge urn:ump:w5p7p4p3vuvt47qv4mmwruxrnlm3st5eiy6fp35vppuokgrbl3iq, C)

1. **Deterministic, prev-verified successor selection**: candidates are ALL
   outstanding requests at tip+1 collected then SORTED (HashMap order is
   arbitrary), and a candidate counts only if its cached payload is present
   (height 0) AND `raw_prev_hash(payload) == tip_hash` — an uncached or
   another-branch entry can no longer suppress a cached valid successor.
   Regression extended: three same-height requests (wrong-branch cached
   entry sorts BEFORE the real successor, uncached third) — the real one is
   found; tip moved on → none; no cached body → none.
2. **Reinjection is attribution-neutral**: reinjected payloads carry the
   command `block-reinject` and are NEVER attributed to the peer whose
   drain triggered the cascade — a validation/apply failure of a cached
   payload logs, drops the payload, and de-requests the hash (the request
   stays in flight for the timeout re-dispatch); it cannot disconnect or
   penalize any peer. The TIP ADVANCE line names `cached-reinject` instead
   of a peer for such blocks.
3. **Brief corrected**: the test count is 730 (+7 fns) — the r4 brief still
   said 729; evidence now also states explicitly that the judge sandbox has
   no network, so the self-contained reproduction path is diff + base SHA
   (the graded commit itself will be pushed with the grade URN in its
   message right after the gate passes).

## r5 verdict → r6 deltas (judge urn:ump:pcczkdaul3us2unxgwdpabgtbif3bgb5uvjdplgfdwb5d7nuld2a, C)

**REVIEW CAP ANNOUNCEMENT** (codex-gate-review-depth, loaded mid-review):
five rounds spent; this is the FINAL round under the 6-round cap.

1. **Reinject-failure redispatch, proven**: the r5 judge's blocker was real
   against the bytes it saw — the bundle diff was STALE (regenerated before
   the r5 refresh fix landed in the file; errata: the refresh code was
   already in the tree, the diff lagged). The current diff carries the
   refresh-and-retain handler: a failed reinjection keeps the hash
   outstanding with a refreshed clock, and the aged-out scan then requeues
   it for re-dispatch with peer rotation. New end-to-end regression:
   `failed_reinjection_stays_in_flight_and_redispatches_after_timeout`
   (handler → clock backdated → aged-out scan requeues → attempt count and
   rotation memory carried). The misleading "request stays in flight" log
   now states the mechanism: "(re-dispatches after the age-out)".
2. **Executable artifact instead of a procedure**: `verify.sh` +
   `transcript.txt` (staged 2026-11-11 13:39Z on vmi3080415) run the whole
   chain for real — repo state at base f08d29d, sha256 chain, diff
   whitespace check, **755/755 added-diff-lines containment** against the
   attached patched source, the full suite (731 passed / 0 failed /
   1 ignored), and the 10-test item-23 set. `patched-node_embedded-r6.rs`
   is the complete patched source so the final bytes are reviewable
   offline without applying anything.
3. **Offline scope, stated plainly**: the judge sandbox has no network and
   (per its own reports) cannot fetch the GitHub base; no bundle short of
   the full toolchain can run `cargo` there. The self-contained offline
   verification is what verify.sh checks: diff↔patched-source containment,
   hashes, and the attached real run transcript.

## r6 verdict → r7 deltas (judge urn:ump:j75b4zt36xfbnjj2ivegxua3jtx7mutzhbksm74egqphukxapy3a, C; ABSOLUTE-FINAL round — 8-round wall)

1. **The judge's central diff-corruption claim is WRONG (host evidence)**:
   it stated the diff "adds `candidates.sort_unstable();` twice … while
   the patched source contains it once". Host bytes:
   `grep -c sort_unstable` → diff: **1** (one `+` line), patched source:
   **2** (two legitimate call sites — `find_cached_successor` and the r2
   `requeue_unsent_batch` sort). The direction of the claimed mismatch is
   the opposite of reality; no change was made to the code for this.
2. **Exact reconstruction replaces line containment** (the judge was right
   that containment cannot detect ordering/multiplicity): the bundle now
   carries the BASE source file (`base-node_embedded-f08d29d.rs`,
   extracted from base commit f08d29d) and `verify-offline.sh` proves
   `patch -p1 base < graded.diff == patched-node_embedded-r6.rs`
   BYTE-FOR-BYTE (both sha256 1b587b85…06741) — bundle-only, no repo,
   network, or toolchain needed. Host-run output staged in transcript.txt.
3. **verify.sh labeled host-specific** (it operates on /root/tri7 paths by
   design — the bundle-only verifier is verify-offline.sh).
4. **test-evidence.txt refreshed to the r6/r7 run**: 731 passed / 0 failed /
   1 ignored + all 10 item-23 tests individually; BRIEF test section
   reconciled (731, +8 fns vs HEAD's 113).
5. Polish: the stale "de-requests the hash" reinjection comment now
   describes the actual retain-and-refresh behavior.

## Honest scope notes

- The fix does NOT choose WHICH branch wins — total-work arbitration is
  unchanged; it guarantees the adoption pipeline can actually fetch the
  branch the judge chose.
- The sub-question "which peer silently ignored the getdata during the live
  10:19-10:33Z era" is not reconstructible post-hoc (no per-command counters
  queryable on a running node); the fix makes every variant of that question
  moot (silent target, stale label fallback, lost answer, validation-error
  leak) because each path now ages out, rotates, and logs loudly.
- `MAX_OUTSTANDING_GETDATA = 64` unchanged; with age-out the gate can no
  longer close permanently.
- Deploy surface: the node lives in the library (`node_embedded`), so one
  fix covers the fleet daemon (`follow_tip` bin, DNS2/Pi/DNS3), the PC's
  standalone `follow_tip.exe`, and the GUI's in-process node.

## Deploy plan (after judge + CI)

1. DNS2 (pinned node, the lab host): release build →
   mask/stop/swap `/opt/tri7/bin/follow_tip`/unmask/start (§14 one-command
   swap), re-assert staking via RPC string "on", verify: no re-fired
   identical FORK CHOICE verdicts, tip advancing, adoption STALL lines only
   if a peer genuinely cannot serve.
2. Pi: on-device build from git archive of the fix commit (aarch64).
3. DNS3: scp DNS2's binary, swap+restart.
4. PC: cross-compile `follow_tip.exe` (x86_64-pc-windows-gnu), swap in place
   with the daemon stopped, relaunch via the tri7d2 task.
5. Convergence readback: four-way tip + shared-height hash equality.
