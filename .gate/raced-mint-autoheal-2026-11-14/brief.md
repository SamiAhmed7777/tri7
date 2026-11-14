# Artifact: tri7 item 27 — §16-RACED-MINT-AUTOHEAL (the daemon runs its own repairwallet remedy)

Kind: code (production change in the fleet daemon's stake tick)
Repo: /root/tri7 (master 2736c597 + this working-tree diff, one file: src/node_embedded.rs)
Purpose: a raced-and-lost stake mint leaves the staker's wallet live view
contradicted; the resolver excludes the raced coins and staking starves until
an operator manually runs `repairwallet`. The daemon now runs that exact
remedy in-process when its own stale-evidence determination fires.

## The live incident (production evidence, DNS3 seed wallet, 2026-11-14 UTC)

Three raced-mint facts-mismatch impairments in ~3.5 h, each healed BY HAND
with the same one-line remedy (journal lines, tri7d on DNS3):

    11:2x  STAKE: STALE LIVE VIEW — … (occurrence #1; manual repairwallet)
    13:58:52 STAKE: STALE LIVE VIEW — coin 2510cd72…:1 claims containing block
           h2211782 (block_time 1794664685 …) but the chain holds block_time
           1794664665 … — 2 coin(s) excluded from staking.   (#2)
    14:15:30 repairwallet: rebuilt live view h2207062..h2211785 … repaired
    14:15:39 stake: stale live view CLEARED — wallet evidence matches the
           chain again (repairwallet took effect)                (#2 healed)
    14:21:42 STAKE: STALE LIVE VIEW — coin c4442d3a…:1 claims containing block
           h2211791 (block_time 1794665961 …) but the chain holds block_time
           1794666008 / tx_time 1794665961 … — 2 coin(s) excluded.  (#3, 6 min later)
    14:31:35 same coin — now 4 coin(s) excluded from staking.
    14:44:0x repairwallet pass-1 (manual, this tick): 6 mismatched spent coins,
           201.317383 TRI affected; 50s recheck stale null; balance settled
           to the standing 100.654961.

This is item 17 working as designed (secondary stakers race, the heavier
branch wins, the loser reorgs cleanly — races are SAFE). The DEFECT is only
that the documented remedy stays manual: each race starves the node's staking
until a human notices, and #3 landed 6 minutes after #2's heal.

## Root cause

`resolve_stake_evidence` (stake tick) excludes wallet coins whose containing-
block facts contradict the canonical chain (a fork adoption rolls back the
chain but NOT the wallet live view). `note_stale_evidence` sets the persisted
`staking_impaired` record and logs STAKE: STALE LIVE VIEW with the remedy
("run `repairwallet`"). Nothing runs it. The `repairwallet` RPC verb
(node_embedded.rs ~8539) is the fix: rebuild the live view from the block
store (`WalletLive::seed` replay over base..replay_tip) + `repair_diff` +
swap. The remedy is deterministic, bounded, purely local — it never needs a
peer — so the daemon can run it itself.

## The change (one file: src/node_embedded.rs; +427/-1)

1. `StakeState` gains `autoheal_episode: Option<String>` +
   `autoheal_attempts: u32` (in-memory struct, `#[derive(Default)]` — no
   persistence/serde impact). The two explicit literals in tests are updated.

2. `const AUTOHEAL_MAX_ATTEMPTS: u32 = 3` — matches the documented live heal
   discipline (a pass can lag the detector; a re-arming phantom can need a
   second pass). Beyond 3, the condition is NOT self-healing (store behind
   the tip, persistent fork) and the operator must look; the STAKE: STALE
   LIVE VIEW line keeps firing on its loud cadence either way.

3. `fn autoheal_due(&StaleLiveView, &mut StakeState) -> bool` — pure rate
   policy: fires on the impairment's EXISTING loud re-log cadence (first
   sighting ticks==1, then every 40th tick ≈10 min at the 15s stake cadence),
   capped at 3 per episode; a fresh episode (impairment fully cleared in
   between → new `first_utc`) re-arms the budget. Episode recorded as a side
   effect.

4. `fn autoheal_stale_live_view(&mut ChainState, &mut WalletLive,
   &BlockStore, &mut StakeState) -> bool` — the exact `repairwallet` RPC
   rebuild: base = min(view base, chain base) (rollback may have pushed the
   chain base lower), tip = chain tip, `newest_replayable_tip` walk,
   `WalletLive::seed` replay, `repair_diff` against `wl.owns`, swap live+base.
   STORE-BEHIND and REPLAY-ERROR skips log and return false WITHOUT burning
   an attempt (transient — bodies still landing). Returns true when the view
   was rebuilt. The repairwallet RPC VERB IS UNCHANGED (operator-facing,
   identical behavior); the detector (`resolve_stake_evidence` /
   `note_stale_evidence`) and the STAKE: STALE LIVE VIEW log text are
   UNTOUCHED — every fleet grep and --status surface keeps its meaning.

5. `stake_tick` wiring, immediately after `note_stale_evidence` (and before
   the locked/reserve gates, matching the item-20 round-1 lesson that
   detection must run before every early return): when stale coins are live
   and the autoheal rebuilt the view, the tick RE-RESOLVES candidates from
   the fresh view, clears `staking_impaired` when the evidence now matches,
   and continues the SAME tick on the restored coins.

## Tests (+4, full suite green before submission)

- `autoheal_due_fires_on_first_sighting_relog_cadence_and_episode_cap` —
  pure policy: ticks 1/40/80 fire, 2/39/41 wait, cap exhausts, new episode
  re-arms and resets the budget.
- `autoheal_store_behind_skips_without_burning_an_attempt` — empty store:
  both the first sighting and a later re-log tick skip with attempts == 0,
  episode still recorded.
- `autoheal_rebuild_removes_the_raced_phantom_and_keeps_the_honest_coin` —
  REAL block store (5 written blocks h0..=h4, h2 pays the wallet the honest
  coin, block.time = the synthetic chain's entry time) + canonical map with
  the raced coin's meta + wallet view holding both coins (raced coin with
  block_time 20s early, tx facts agreeing — the exact DNS3 14:21:42Z shape):
  detection flags the facts-mismatch class (absent:false); one autoheal pass
  removes ONLY the phantom, keeps the honest coin byte-identical, burns
  exactly 1 attempt, episode recorded; re-resolve on the rebuilt view is
  clean → impairment cleared.
- `stake_tick_autoheals_and_clears_even_while_locked` — full `stake_tick`
  with the same fixture and a LOCKED wallet: returns Ok(false) (locked gate,
  no mint) yet the impairment is cleared, the phantom removed and attempts
  == 1 in the same tick — heal before gates, like detection.

## Deployment plan (after CI green)

Same as items 25/26: DNS2 + DNS3 get the release binary (backups kept), Pi
builds on-device from the same commit; sami-pc is the actor lane's domain
(observation only). Live acceptance: next raced-mint impairment on DNS3
self-heals within one re-log cadence (journal shows the AUTOHEAL lines and
staking resumes without any manual repairwallet).

## Constraints respected

- No change to consensus, fork choice, RPC surface, or log formats.
- No new config flags (default-on; the cap bounds the work per episode).
- The wallet-key clone per attempt matches the RPC verb's existing cost;
  attempts are rate-limited (≥10 min apart, ≤3 per episode).
