# The wallet's balance must come from the LIVE address index

PROBLEM (measured, not hypothetical): the node served a wallet balance that
was capped at the chain replay's coverage (h2201090) and stale by ~200 TRI,
while other views of the same wallet disagreed by up to 1,400 TRI:
  archive replay snapshot @h2201090    3,036.990591 TRI / 177 coins
  node live window (frozen at seed)      1,627.639028 TRI /   7 coins
  wallet history.json (frozen h2205310) 4,586.577699 TRI / 286 coins
  union of snapshot + live              4,664.629619 TRI
The chain replay cannot reach the tip: the canonical band h2201750..h2203275
is missing from EVERY node's block store and the upstream onion/I2P peers are
all unreachable, so the replay snapshot is permanently capped.

FIX: serve the wallet's spendable set from the LIVE address index, which is
applied on every accepted block (src/bin/follow_tip.rs :: addrindex_apply) and
therefore follows the tip. It stores per-output rows with `spent_height`, so
it knows exactly which coins are still unspent — a coin list that cannot be a
double-spend, which is what a spend needs.

What to check in this change:
1. the new ops verb `getwalletspendable` in src/bin/follow_tip.rs: it must use
   the WALLET's own addresses (the live wallet view's keys), the index's
   unspent rows, and report the index height + chain tip;
2. src/rpc.rs: getbalance/listunspent/getwalletinfo prefer the live figure
   and fall back to the replay snapshot; `balance_partial` must be TRUE when
   the fallback is used and when the index trails the tip;
3. listunspent's confirmations must be computed against the CHAIN TIP when the
   live path is used (the snapshot path keys them to the snapshot coverage);
4. the GUI must not present a partial balance as the whole balance;
5. the two new regression tests must actually pin the behaviour.

Evidence file: live-index-balance-evidence.md (the measured numbers).
Diff file: live-index-balance-fix.diff.

`cargo test --release --lib`: 500 passed, 0 failed.
