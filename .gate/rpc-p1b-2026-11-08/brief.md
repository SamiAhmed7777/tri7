# Judge brief — workqueue item 9 RPC-P1b: repairwallet + resendtx

Repo: /root/tri7 (master @ 850f733). Diff: rpc-p1b.diff (includes the new
src/sent_txs.rs, also shipped verbatim as sent_txs.rs).

## Round-4 changes (took round-3 polish, grade A
urn:ump:mrht5bskwsk4o42f5zbj536mdjio3wnt6tsxnpde3jq3yvbctdsq, zero blockers)

- Gate is now FAIL-CLOSED: resend_tx_is_mined became a tri-state
  ResendGate {Mined, Unknown, CantProve}. Store Err -> CantProve = the
  entry is SKIPPED this pass (kept, never accepted) because unmined-ness
  cannot be proven; only a definitive store "not mined" (or no deep
  fallback at all — where a mined tx's parent outputs were already removed
  by apply_block, so accept fails on its own) reaches pool.accept.
  Call site is an explicit match with Mined/CantProve => continue, so the
  accept path is structurally unreachable for both.
- sent_txs::save tmp name now carries pid + nanosecond stamp (two daemons
  sharing a wallet dir cannot collide on the tmp path).
- Save-failure-after-prune note: the save error already surfaces to the
  caller (-1) and the mine-gate re-prunes on the next pass, so a resurrect
  is bounded to one noisy pass — no change needed.

## Round-3 change (LIVE-CAUGHT REGRESSION in round 2, fixed + regression-pinned)

The live resend cycle on DNS2 caught a real defect: a send was relayed,
mined (h2207434), and then a resendtx pass ran. Round 2's polish had moved
the mine-check into the accept-Err branch; but the pool's
WalletThenStoreUtxoView deep-coin fallback (added 2026-11-07) resolves
prevouts from the block store WITHOUT spent-tracking — so the mined tx's
inputs still "exist" to the view, `pool.accept` SUCCEEDED, and the mined tx
was re-accepted into the pool of the fleet's heavy staker as a
spendable-input double-spend candidate. Fix: the mine-check is back as a
HARD GATE (resend_tx_is_mined) that runs BEFORE pool.accept and prunes
mined entries — they can never reach accept again. New regression test
`resend_mine_gate_prunes_before_reaccept` pins all four store outcomes.
Remediation on DNS2: daemon restart cleared the poisoned pool (pool is
in-memory); the tip was never affected (blocks minting had happened before
the re-accept and assembly stayed empty-tx). The Err branch now means
"not mined, transient" — keep for the next pass (comment updated).

## Round-2 changes (took judge round-1 polish, grade B
urn:ump:tj3kyfaf74fhii62nqvaymqcquvarxptpmszuyv6fcjpcasjrxja, zero blockers)

- Record race CLOSED: new process-global `sent_txs::record_lock()`; it is
  always lock #1 (before pool/wl) on both paths that touch the record —
  relay_built (append) and resendtx (load→save rewrite) — so a send landing
  mid-resend cannot be dropped by the survivors rewrite, and concurrent
  relay_built appends cannot lose entries. Documented lock order.
- repairwallet now also updates `w.base = base` after the swap (the view's
  claimed window base stays consistent with the map actually replayed).
- resendtx mine-check is now lazy: the store lookup happens ONLY on the
  accept-Err branch (a successful accept never pays for it).
- Typo fixed ("entrie(s)" → "entry(s)") in both log lines.
- Transient-keep expiry deliberately NOT added — documented rationale in
  sent_txs.rs: each pass costs one accept + one Err-branch store lookup per
  entry, the 200-cap bounds the set, and the entry self-prunes when mined.

## Legacy sources being ported (read at triangles_v5-fork/src/rpcwallet.cpp)

- repairwallet (rpcwallet.cpp:1804): `FixSpentCoins(nMismatchSpent,
  nBalanceInQuestion)` with fCheckOnly=false; refuses any parameter
  (runtime_error); replies `{"wallet check passed": true}` when zero
  mismatches, else `{"mismatched spent coins": N, "amount affected by
  repair": ValueFromAmount(sum)}`. FixSpentCoins (wallet.cpp:2695) walks
  wallet txs, compares each output's wallet spent-flag against the chain
  UTXO set, and REPAIRS: "wallet says spent but UTXO exists" → MarkUnspent
  (lost coin); "wallet says unspent but no UTXO" → MarkSpent (spent coin);
  both directions increment nMismatchFound and sum the output value.
- resendtx (rpcwallet.cpp:1826): `ResendWalletTransactions(true)`, replies
  null; the legacy signature accepts at most one parameter (ignored). The
  legacy resend walks mapWallet, re-relays every wallet tx NOT in a block
  (txdb.ContainsTx check), sorted chronologically (wallet.cpp:1275).

## Why the port differs (tri7 architecture, stated honestly)

tri7's wallet (SQLite/BDB-parity single key/value table) keeps NO per-output
spent flags — its "wallet belief" IS the in-memory `WalletLive.live` map
(follow_tip.rs), rebuilt by replaying the chain window. So:

- repairwallet = REBUILD the live view from the block store (the
  authoritative replay over the same window the seed used) and diff the
  wallet-owned output sets; the two diff directions are exactly legacy's two
  FixSpentCoins directions. Repair = swap the rebuilt map in (same
  under-lock swap the accept path uses). This heals the documented
  "fast catch-up leaves the wallet live view wrong; a restart heals it"
  defect class (skill tri7-fleet-chain-state section 12) WITHOUT a restart.
  The wl lock is held across seed+diff+swap so block acceptance cannot
  interleave; one ~4600-entry window replays well under a second (the
  balances builder replays ~23k blk/s, DNS2 measured 95s for 2.2M blocks).
- resendtx = tri7 has no mapWallet either, and the TxPool is IN-MEMORY ONLY
  (txpool.rs: no persistence) — after a restart, unconfirmed wallet sends
  were forgotten entirely, which is precisely the failure legacy resendtx
  recovers from. The port therefore adds the missing persistence: a small
  JSON record BESIDE the wallet file (`sent_txs.json`, same dir as
  history.json; tri7_core::sent_txs). relay_built (the shared
  sendtoaddress/sendmany relay path) appends one entry per relayed wallet
  send — best-effort, a record failure cannot fail an already-relayed send.
  The resendtx verb re-accepts each recorded tx into the pool (duplicate
  accept = idempotent success, txpool.rs accept()) and pushes the txid into
  the announce queue (the same path sendrawtransaction uses). Classification
  per entry: corrupt hex → pruned; pool accept Ok → announced + kept;
  accept Err + store.tx_by_id says mined → pruned (confirmed);
  accept Err + not mined → kept for the next pass (transient). The record is
  rewritten atomically with the survivors and capped at the 200 newest.

## Output-shape parity

- repairwallet: strict legacy keys — `{"wallet check passed": true}` or
  `{"mismatched spent coins": N, "amount affected by repair": "x.xxxxxx"}`
  (legacy ValueFromAmount is a STRING, always 6 decimals — new
  rpc::legacy_format_money, unit-tested incl. negatives).
- resendtx: returns JSON null like the legacy `return Value::null`.
- Arity: repairwallet any param → -8 (parity of the legacy runtime_error);
  resendtx >1 param → -8, 0/1 accepted (one ignored) — helpers
  repairwallet_params_ok / resendtx_params_ok, unit-tested.

## Routing (the three-edit rule, skill section 8)

Both verbs added to NATIVE_METHODS and OPS_METHODS (NOT OPS_EARLY — both
need the live wallet view and are served only by the live ops engine;
dispatch check happens BEFORE handlers, so membership is what makes them
reachable). A regression test pins membership + non-early for both
(rpc.rs: repairwallet_and_resendtx_are_native_and_ops_routed), and the
existing rpc.rs test that asserts OPS ⊆ NATIVE stays green.

## Tests added (all green)

lib (491 passed, +8):
- sent_txs: roundtrip+missing-file-empty, dedupe-by-txid-replaces,
  cap-drops-oldest, corrupt-record-is-an-error-not-silent-empty,
  path_for-sits-beside-wallet.
- rpc: legacy_format_money vectors; repairwallet/resendtx routing.
bin follow_tip (47 passed, +4):
- newest_replayable_tip walk-back branches (incl. base-clamp + 5000 cap).
- arity parity for both verbs.
- repair_diff: only wallet-OWNED mismatches counted, both directions,
  amount = sum of mismatched output values, identical maps → 0.
- repair_swap on a REAL WalletLive with real key/scripts: drifted view
  (missing funded block) → lost coin diff; ghost coin the replay lacks →
  phantom diff; swap heals the balance (3_000_000 restored).

## Pre-existing proof of the seed-thread refactor

The wallet-seed thread's inline tip clamp was extracted into
newest_replayable_tip and rewired (same cap, same walk-back semantics, log
lines unchanged). The seed thread is exercised by every daemon start; the
follow_tip test suite pins the helper's behavior.

## Honest scope notes

- wallet_utxos.json / balances.json snapshots are intentionally NOT touched
  by repairwallet: the balances builder thread is their single writer
  (atomic tmp+rename) and has its own staleness trigger; two writers would
  race. listunspent/getbalance serve the live map (verified in source:
  OpsAdapter::utxos reads WalletLive.live), so the RPC surface IS repaired.
- A repairwallet diff only sees the window both views claim (min of the
  view's seed base and the chain-state base — the min guards the
  forkchoice-rollback case). Coins older than the window base are invisible
  to BOTH the old and new map, so they can never be misreported as
  mismatches.
- resendtx records only daemon-relayed WALLET sends (sendtoaddress/sendmany
  via relay_built). sendrawtransaction stays un-recorded (legacy resends
  wallet txs, not raw relays). GUI sends go through the same sendtoaddress
  RPC arm on the embedded node and are therefore recorded.
- The verbs are daemon/CLI-side; the GUI explorer still surfaces richlist
  only (same note as item 8).
