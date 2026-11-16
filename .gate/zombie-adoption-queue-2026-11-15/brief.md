# Artifact: tri7 item 28 — ZOMBIE-ADOPTION-QUEUE (cancel-on-supersede + staker hold + claimant pinning)

Kind: code (production change in the fleet daemon's follow loop)
Repo: /root/tri7 (master cc2df30 + this working-tree diff, one file: src/node_embedded.rs)
Full test suite: **803 passed / 0 failed / 1 ignored** (baseline 799/0/1 + 4 new regression tests; suite re-run green on this exact tree).

## The live incident (production evidence, Pi, 2026-11-15 03:0x–03:5x UTC; 4th node-incident of the class)

The Pi's judge CORRECTLY rolled back to h2212168 and adopted 14 canonical
blocks — but body serving failed every cycle while 74 peers were LIVE and all
claiming the CURRENT canonical tip, and the staker kept re-minting its OWN
fork blocks on top of the half-adopted rollback:

    03:28:35Z  judge rollback + adopt 14 canonical blocks
    (repeat)   "reconciliation cannot advance the tip until a peer serves these bodies"
    (repeat)   getdata "requeued, rotating targets (the peer set may not hold the adopted branch)"
    03:28      STAKE MINTED h2212169 (own fork, again)
    03:32      STAKE MINTED h2212170 (own fork, again)
    03:37:44Z  manual `systemctl restart tri7d` (restart lever use #6)

The restart healed it (fresh sessions fetched the 14 bodies in ~13 s). The
same class had already hit DNS2 (~6.5 h), Pi (11-12), and DNS3 (11-13). The
restart lever is a workaround; this diff removes the wedge class.

## Root cause (three cooperating defects)

1. **No cancel-on-supersede.** A new Switched judgment left the previous
   adoption's queued bodies and outstanding getdata in place. Each re-judge
   re-queued the same heights; `outstanding` (capped MAX_OUTSTANDING_GETDATA
   = 64 slots) saturated with dead slots from the abandoned era and the
   dispatch gate (`outstanding.len() < MAX_OUTSTANDING_GETDATA`) froze.
2. **The staker minted mid-adoption.** stake_tick had no adoption-in-flight
   gate. After the rollback the staker minted its own fork block on the
   orphaned lineage every ~4.5 min, feeding the judge fresh fork "evidence"
   and re-triggering the rollback/adopt cycle (the remint half of the zombie).
3. **No give-up cap and no claimant awareness in body-target selection.** An
   adoption whose bodies never arrive stayed in flight forever (bounded only
   by the restart). Body-target rotation (`choose_body_source_label`) never
   looked at a peer's claimed tip, so getdata rotated across sessions whose
   claims could not possibly serve the batch.

## The fix (all in src/node_embedded.rs)

1. **Cancel-on-supersede:** `ReconAcc` gains `switch_epoch: u64`, bumped on
   EVERY Switched judgment; the Switch arm also clears `pending_from_headers`
   when a previous adoption's queue is still in flight (loud
   "adoption SUPERSEDED mid-flight" line). The daemon loop watches the epoch:
   on change it clears `outstanding` + `body_tries` (the superseded era's dead
   slots) and starts the era clock.
2. **Staker hold:** the stake tick is gated on
   `recon_acc.branch_source.is_none()` (branch_source is bound to Switched and
   cleared on drain — normal announces never hold the staker). Held ticks log
   "staking HELD" (first tick, then every 10th). No mint while the node is
   between tips.
3. **Give-up cap:** `ADOPTION_GIVE_UP_SECS = 600`. An era whose bodies never
   arrive within the cap is CANCELLED outright (queue + outstanding + tries
   dropped, branch_source cleared, staking resumes, loud
   "adoption CANCELLED" line); the branch re-announces and re-judges normally.
   `adoption_give_up_due(started, now, cap)` is split out for the boundary test.
4. **Claimant pinning:** `choose_body_source_label` /
   `resolve_body_send_target` gain a `min_tip_height` (the batch's tallest
   requested height) and a claimed-tip-aware fallback order: advertiser
   (tier 0, unchanged) → covering outbound → covering live → first outbound →
   first live. A session claiming a shorter tip cannot serve the batch and is
   now preferred-last instead of rotated-through.

## Tests (+4, suite 803/0/1)

- `a_second_adoption_supersedes_the_first_era` — era-1 adoption (4 pending);
  era-2 heavier branch forking below the tip → queue REPLACED (5, not 4+5),
  switch_epoch 1→2, branch_source moves to the new advertiser.
- `body_source_prefers_peers_whose_claimed_tip_covers_the_batch` — covering
  claim outranks a short claim in the fallback tiers.
- `the_advertiser_still_outranks_a_covering_fallback` — tier-0 precedence kept.
- `adoption_give_up_fires_only_at_the_cap` — cap boundary (599 s no, 600 s yes).

All pre-existing selection/rotation/recon tests pass UNCHANGED in behavior
(their call sites gained the new height argument; `0` preserves the old
order — coverage only reorders the fallback tiers when it can actually
discriminate).

## Verification

- `cargo test --lib`: 803 passed / 0 failed / 1 ignored (this tree).
- `cargo check --lib`: clean (no new warnings from this diff's code paths).
- The diff touches ONLY the adoption lifecycle, the body-source selection
  helper, the stake gate, and tests. The RPC verbs, detection paths, log
  texts of other subsystems, and the wire protocol are untouched.
