# Brief — tri7 item 37 STALE-RESOLVER-OFFBYONE (code fix, Rust, one file)

Artifact: working-tree diff of /root/tri7 (master 6e0f99f) — `item37.diff` (1 file,
+360/−14) plus `excerpt_item37.rs` (the changed regions + the 4 new tests, verbatim
with line numbers; the whole file is 26.9k lines and exceeds the per-file budget, so
the excerpt IS the graded bytes for the new code — the diff shows exact context).

## Live incident being fixed (verified facts, DNS2 2026-11-21, RPC oracle)

- DNS2's FIRST §16 STALE LIVE VIEW firing ever: first_utc 23:42:43Z, ~10.5-min
  re-log cadence, ticks 327+, coins:2, ZERO impairment-CLEARED lines.
- item-27 AUTOHEAL burned its full 3-attempt episode cap (23:42:43 / 23:52:56 /
  00:03:26Z) each returning "0 owned-output mismatch(es) totalling 0 sats
  (0 phantom, 0 lost)" — the remedy PROVEN no-op.
- Flagged coin 3f16c216…:0 claims containing block h2212451; getrawtransaction →
  blockhash fbff5369 (h2212451), txid IS in that block's tx list, conf 4643, and
  h2212451's header time 1794744458 EQUALS the wallet's claim EXACTLY. The stale
  dict's chain_block_time 1794744738 is h2212452's (669d46a6) time — EXACTLY ONE
  ABOVE the claimed height (unique hit in a header scan 2212430..2212500).
- First fire when conf hit EXACTLY 4600 == WINDOW_ENTRIES (first-fire tip h2217051;
  2217051 − 2212451 = 4600) — the coin sat at the window's FAR EDGE; the flag
  PERSISTED past the edge (conf 4642 at the sweep).
- FALSE POSITIVE: the wallet was right; the resolver's chain-evidence lookup wrong.
  Impact: DNS2 is the SOLE HEAVY STAKER with 411 aging live-view UTXOs — every coin
  crosses the 4600 edge eventually and would be wrongly excluded + each false flag
  masks real facts-mismatch signals. Custody/parity/staking unaffected.

## Root cause (the off-by-one mechanism)

Kernel metas (`WindowOutputs.map`, `UtxoMeta.height`) store WINDOW-RELATIVE heights.
`ChainState::trim` slides the window on every tip advance past WINDOW_ENTRIES=4600
(src/node_embedded.rs:178): drains `excess` entries, `base += excess`, and calls
`self.utxo.rebase(excess)`. The old `rebase` did
`m.height = m.height.saturating_sub(excess)` — a coin whose containing block fell
BELOW the new base (conf ≥ WINDOW_ENTRIES) clamped to relative 0, i.e. its meta
silently re-pointed at `entries[0]` = the block AT the new base. `resolve_stake_evidence`
(~9236) then compared the coin's facts against that WRONG block (live: exactly H+1)
and flagged a false facts-mismatch; a rebuild could never clear it (the wallet was
never wrong), so the flag ran forever.

## Fix map (matches the item's fix spec)

(a) **Off-by-one** — `WindowOutputs::rebase` (excerpt lines 2602-2633): `retain` metas
with `height >= excess` (drop below-window coins) BEFORE shifting survivors; a
survivor's `entries[meta.height]` is always the coin's OWN containing block. Dropped
coins hit `kernel_meta → None`, and the resolver's existing absent-branch classifies
`claimed < stg.base` → unresolvable (the item-24 kernel-coverage class). The validator
(`check_proof_of_stake`, src/kernel.rs:665) treats None as Err("coinstake input not a
tracked UTXO") — honest rejection; unreachable in practice (a received block's stake
coin is always fresh, conf ≈ 0). `no_trim` (from-zero) mode never rebases (trim
early-returns) — full-history kernels untouched.

(b) **Out-of-horizon coins leave the candidate set** — consequence of (a) + the
existing below-base branch: counted `unresolvable`, skipped, never re-judged as
stale. Their log line (stake_tick, excerpt ~9575-9595) is now RATE-LIMITED to the
house cadence (first tick + every 40th ≈10 min; new `StakeState.unresolvable_ticks`,
reset to 0 when the count returns to 0) — previously per-15s-tick, which would have
become a permanent-journal-spam regression once coins start aging out (DNS2 has 411
aging UTXOs; the set only grows).

(c) **AUTOHEAL stops burning attempts on rebuild-clean** — `autoheal_stale_live_view`
(excerpt 9395-9465): the attempt increment MOVED after `repair_diff`; when
`diff.count() == 0` (rebuild-clean — repair_diff compares only the owned-output SET,
so a correct wallet always diffs clean) while the flag still fires: log the
RESOLVER-FAULT line LOUDLY ONCE per episode, latch the episode
(`StakeState.autoheal_fault_episode`, new field), swap NOTHING, return false, burn NO
attempt. `autoheal_due` (excerpt 9355-9380) returns false for a latched episode and
clears the latch when a fresh episode (new first_utc) re-arms. Real repairs (diff > 0)
keep the exact item-27 semantics (increment, swap, re-resolve, clear).

(d) **Remedy text stops recommending the no-op** — `note_stale_evidence` gained a
`fault_episode: Option<&str>` param (call site stake_tick ~9527 passes
`stake_st.autoheal_fault_episode.as_deref()`); when the CURRENT episode is latched,
the STALE line's remedy arm says "RESOLVER FAULT (item 37), NOT wallet corruption: do
NOT run repairwallet…" instead of the repairwallet advice (unchanged for every other
class). Also the CLEARED line no longer mis-attributes "(repairwallet took effect)" —
now neutral ("a repairwallet/autoheal rebuild took effect, or the flagged coin(s)
left the staking window"). The greppable anchors survive: "STAKE: STALE LIVE VIEW —",
"stale live view CLEARED", "AUTOHEAL — ran the repairwallet remedy", "AUTOHEAL —
wallet evidence matches the chain again".

## Regression tests (4 new, all in the excerpt)

1. `window_edge_coin_at_exactly_window_entries_conf_is_not_flagged` — THE incident
   replay at the exact boundary: 4601-entry synthetic chain, trim() slides exactly
   one (base 0→1); edge coin minted at abs 0 (conf == WINDOW_ENTRIES) with CORRECT
   wallet facts must NOT flag (pre-fix it clamped to slot 0 → entries[0] = H+1's
   time → false facts-mismatch); a coin minted at the new base still resolves with
   the chain evidence read AT its claimed height. Pins rebase-drop + H-vs-H+1.
2. `coin_past_the_horizon_is_never_rejudged_as_stale` — 6 slides deep (conf
   WINDOW_ENTRIES+5): counted unresolvable ×3 passes, stale empty, never silent.
3. `autoheal_rebuild_clean_latches_the_resolver_fault_and_burns_no_attempts` — the
   map-coverage fault shape (correct wallet, map lost the row → absent-branch flag):
   rebuild runs, diff 0 → returns false, attempts stay 0, episode latched; a later
   re-log tick fires no rebuild; a fresh episode re-arms (latch clears).
4. `resolver_fault_remedy_text_is_keyed_to_the_latched_episode` — the latch keying:
   latched episode flags with ticks climbing; unlatched and different-episode
   records take the normal arms.

## Local verification (this exact tree, both run to completion)

- `cargo test --lib` → **876 passed; 0 failed; 1 ignored** (up from 872: +4 item-37
  tests; the 1 ignored is the pre-existing ignored, not a failure).
- `cargo test --test cli_exit_tests --test consensus_wallet_tests --test
  legacy_tests --test spend_corpus_tests` → **5 + 15 + 24 + 2, all ok**.
- `cargo check --lib` clean (pre-existing warnings only, none in the delta).
- The full item-27 autoheal suite (cadence/cap/skip/rebuild/locked-tick) passes
  byte-identically — the item-27 semantics are preserved for real repairs.

## Honest scope notes

- The `unresolvable` log line's rate-limit changes cadence for the item-24
  below-floor class too (per-tick → house cadence). Content unchanged. This is the
  fix that makes (b) live-safe (out-of-horizon sets only grow).
- Deployment (after this grade): x86_64 release rebuild for DNS2+DNS3, on-device
  aarch64 build on Pi; live bars: DNS2's existing 2-coin false flag CLEARS without
  any wallet/store change (stale=null in getstakinginfo, zero new STALE lines across
  a full ~10.5-min re-log cadence), staking unregressed (attempts climb, mints
  continue), the next naturally-aging coin crossing the 4600 edge produces NO false
  flag. Deployment evidence lands in the workqueue PROGRESS, not in this artifact.
- Not in scope: §16 skill fold-in (skill over char budget — the workqueue item text
  is the canonical record).

## Rubric

Grade the CHANGE (correctness of the off-by-one fix + the classification boundary +
the latch semantics + the remedy-text logic + test quality/coverage), not the
deployment (separate lane, live-verified after merge). Answer with a single JSON
object on the LAST line, nothing after:
{"codex_grade": "A"|"B"|"C"|"D"|"F", "reasons": [...], "blocking_issues": [...],
"polish_suggestions": [...]}
