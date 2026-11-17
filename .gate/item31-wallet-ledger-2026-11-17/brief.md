# Item 31 — DNS2-WALLET-LEDGER-STALE — brief

**Artifact:** tri7 master patch (3 files) + regression tests. Judge target: the
fix set for the wallet-view defect the 2026-11-17 continuity audit caught live
on DNS2: the wallet (listunspent/getwalletinfo + the wallet_utxos.json pair)
listed canonically-DEAD coins and under-held ~3615 TRI of live coins, while the
store-oracle addrindex read correctly.

## Root cause chain (all facts verified live, see evidence file)

The money-path tier hierarchy serves: tier 1 = the live addrindex when
AUTHORITATIVE, tier 1.5 = the wallet's own live view, tier 2 = the replay
snapshot pair (balances.json + wallet_utxos.json, merged with the live view).

1. **Tier 1 never engaged.** `addrindex::authority` required the index's
   applied height to equal the chain tip EXACTLY. DNS2's index trails the tip
   0..~30 blocks (the 10-minute catch-up builder + continuous minting), so the
   verdict refused and the serve fell through forever.
2. **Tier 2 served a FROZEN pair.** `LiveWalletProvider` parsed
   balances.json + wallet_utxos.json ONCE at startup (08:18:30Z boot), while
   the balances-build thread REWROTE both files minutes into the same run
   (08:25:25Z, coverage capped h2208645 by an archive reorg boundary). The
   in-memory pair was the PRE-rebuild generation (coverage h2207975 — proven by
   the audit's "conf=4" = 2207975−2207972+1).
3. **The merge's gap rules dropped every cure.** The live view's window base
   (tip−4599 = h2209145) > coverage+1 → the gap branch served the frozen
   snapshot ALONE: 201 rows / 1713.762127 TRI, of which 188 rows / 1628.59 TRI
   were storm-era coins the chain had spent at h2208009..2208039. Canonical
   truth (addrindex): 392 rows / 5329.138354 TRI live on the wallet's
   addresses. VIEW-ONLY defect: keys hold custody; staking unaffected.
4. **Separately, the replay's wallet-side spend pass stranded 7 real ghost
   coins (84.66 TRI).** `balances_build` removed spends from the
   wallet_utxos.json dump ONLY when EVERY vin of the spending tx resolved in
   the global UTXO set. DNS2 block h2206958's tx 8812990e spends b14a219d:1
   alongside 88aa59ec:1 — which the chain had ALREADY spent at h2206940 (the
   winning chain's re-spend history). The unresolvable sibling vin vetoed the
   WHOLE tx, so b14a219d:1 (and 6 sibling coins, created h2206757..2206831,
   spent h2206956..2206974) stayed in the dump as ghosts. The addrindex applies
   spends per-vin — exactly why it is correct and the replay pass was not.

## The fix (3 files, tests included)

- **C. `src/balances_build.rs`** — the wallet dump's spend pass is now PER-VIN
  (canonical spentness is the truth for a spendable-coins dump; mirrors the
  addrindex and WalletLive::apply_block). The GLOBAL set keeps its
  all-or-nothing semantics (balances.json/moneysupply parity untouched).
  Regression test `respent_sibling_vin_cannot_strand_wallet_utxos_in_the_dump`
  reproduces the exact h2206940/h2206958 shape: a tx spending [fresh wallet
  coin + already-spent output] must still remove the fresh coin from the dump.
- **B. `src/addrindex.rs` + `src/rpc.rs` + `src/node_embedded.rs`** — the
  money-path verdict is now `authority_with_lag`: a FINISHED index serves when
  the hash it recorded for ITS OWN applied height matches the CHAIN's hash at
  that height (same-chain proof, resolved by the daemon from the live block
  index's main-chain height slot and published as `chain_hash_at_index` in the
  wallet payload) and it trails the tip by at most MAX_INDEX_LAG=64 blocks
  (~16 min of minting; the builder cadence is 10 min). The answer is labelled
  AS OF the index's coverage height by the existing tier machinery
  (balance_height/index_height/index_behind — all already served). An index
  ahead of the tip, unfinished, on another chain, or with an unresolvable hash
  refuses exactly as before (fail closed).
- **A. `src/node_embedded.rs`** — the snapshot pair is RELOADABLE: the provider
  stats both files on every wallet-path call (two metadata reads) and swaps in
  a freshly parsed pair when either changed — adopted ONLY when both sides
  parse AND agree on coverage height + tip hash (mixed generation never
  served; signature resets for retry). extras/pubkeys/labels stay shared Arcs,
  so runtime wallet bookkeeping survives the swap. Test
  `snapshot_pair_follows_the_files_on_disk` drives the exact defect shape
  (files rewritten under the running daemon) plus the mixed-generation refusal.

## Why no explicit "repairwallet-for-the-pair" verb

The tier-1 lag rule makes the serve converge to the index truth automatically
(within the builder's 10-minute cadence), the reload keeps tier 2 tracking the
files the rebuild thread keeps rewriting, and the per-vin pass fixes the pair
AT SOURCE on every rebuild. A manual verb would duplicate machinery the
hierarchy already provides.

## Known residuals (documented, not silently dropped)

- The archive walker still caps the replay at the first non-chaining frame
  (h2208645): during tier-1 refusal windows longer than ~16 min the pair tier
  serves a capped figure — LABELLED partial/coverage by the existing tier
  fields, never silently. Walker resync across reorg boundaries is a separate
  (large) work item.
- 2 wallet-owned snapshot rows have no addrindex row (insert-miss class);
  under-inclusion is safe-side and self-heals at the next index rebuild.

## Verification

- Full local suite: 819 passed / 0 failed (includes the 3 new tests: the
  re-spend ghost regression, the lag-aware authority matrix, the pair reload
  lifecycle incl. mixed-generation refusal).
- Post-deploy live bars (this tick, after restart): listunspent == addrindex
  to the coin on DNS2 (392 rows / 5329.138354 TRI at the last census, ± new
  stakes/spends), balance_source = live-index, tier-2 ghosts gone.
