# RUNBOOK — item 21 INBOUND-RECON deploy + live verification

## Test evidence (this bundle)

- `cargo-test-output.txt` — COMPLETE `> file 2>&1` capture of
  `cargo test --lib` (rc=0): all 721 test lines then
  `test result: ok. 722 passed; 0 failed; 1 ignored; finished in 69.29s`.
  The transcript was produced on the HOST tree (/root/tri7 at master +
  diff.patch — commit 2c84e34 plus this change); the `tri7-judge-tree`
  directory bundled alongside is that exact content (git archive of HEAD
  with diff.patch applied) and `cargo check --lib` on it compiles fresh
  clean (59.2s), so the transcript is reproducible from this artifact
  root: `cd <bundled tree> && cargo test --lib`.
  New item-21 tests (all `ok` in the output):
  - `node_embedded::tests::inbound_claim_divergence_triggers_recon_and_adopts_the_heavier_branch`
  - `node_embedded::tests::claim_divergence_verdict_shapes`
  - `node_embedded::tests::extending_batch_claim_records_the_served_tip`
  - `node_embedded::tests::body_source_choice_prefers_the_recon_advertiser_over_healthy_outbound`
  - `node_embedded::tests::recon_dispatch_transfers_claim_and_source_to_the_peer`
  Pre-existing trigger pins still green:
  `empty_reply_streak_fires_regardless_of_start_height`,
  `empty_tip_poll_reply_triggers_window_recon_only_as_fork_evidence`,
  `recon_heavier_branch_switches_and_queues_the_validated_body_path`.

## Change surface (single file: src/node_embedded.rs)

1. Poll tick: tip locator to ALL peers (was outbound-only); `inbound_polls`
   success-only counter (stats_json carries it).
2. `Peer.claimed_tip` per-poll claim; updated from chain-extending replies
   (`claim_from_extending_batch`), recon replies (`ReconAcc.judged_claim` →
   Peer in `recon_dispatch`), recon accumulation (Extended arm).
3. `claim_divergence_verdict` + third trigger in `empty_reply_fork_recon_due`
   (reason tag `claim-divergence`; fire log prints the claimed height).
4. `ReconAcc.branch_source` + `choose_body_source_label`: the recon
   advertiser is the preferred getdata target for adopted bodies.
5. getpeerinfo: `claimed_tip_height` + `claimed_tip_hash` per peer row.

## Deploy order (same-binary rule, skill §16)

- DNS2 first (the node whose judge was blind; holds the PC as an inbound
  peer): stop tri7d-health.timer → mask tri7d → stop → wait MainPID 0 →
  swap binary (cargo build --release on-device from the pushed commit, or
  scp the built release binary) → unmask → start → staking re-assert via RPC
  string "on" → health timer back on.
- Pi + DNS3: same swap discipline (ssh hetzner / ssh dns3).
- sami-pc: CI artifact `tri7-windows` follow_tip.exe → taskkill follow_tip →
  replace (staged move + hash readback) → schtasks tri7d2 start → verify
  running sha. GUI untouched by this change (no gui/ file in the diff).

## Live verification bar

1. `getsyncinfo`/health stats on DNS2: `inbound_polls` > 0 AND advancing
   between reads ~30 s apart (the PC sits inbound via i2p/onion tunnels).
2. `getpeerinfo` on DNS2: the PC row carries `claimed_tip_height`
   (populated after its first reply; null until then is honest).
3. Four-way parity: hash-identical `getblockhash` at the minimum tip across
   DNS2 == Pi == DNS3 == sami-pc, read back within the same minute.
4. sami-pc `getbalance` = 5110.900381 (the standing number).
5. Journal (DNS2): no new `send getheaders ... failed` lines; when the fleet
   is at parity the expected steady state is unchanged recon behavior; a
   future inbound fork now produces an `empty tip-poll reply (claim-
   divergence trigger)` line followed by the standard FORK CHOICE adoption.
6. Rollback: `git revert` of the single commit restores the pre-change
   binary; no state-format change ships in this diff (claimed_tip is
   in-memory only; conf/disk formats untouched).
