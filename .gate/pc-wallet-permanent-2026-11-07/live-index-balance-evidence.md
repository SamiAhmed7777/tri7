MEASURED ON THIS MACHINE (2026-11-08) — the PC wallet (399 addresses)

The wallet's spendable set, straight from the LIVE address index
(examples/probe_index_utxos, run against /var/lib/tri7/addressindex.db):

    wallet rows: 399 | addresses: 399
    index meta height: Some("2206738")
    UNSPENT UTXOs (live index): 225
    TOTAL: 3236439612 sats = 3236.439612 TRI
    utxo height range: 2031567..2206738

The index is applied on every accepted block (`addrindex_apply` in
src/bin/follow_tip.rs), so it follows the chain tip. Proven live: the index
and the archive snapshot disagree for the same address, e.g.
  TBxrQx34J2mRvkmixnrd: archive 1148561196 -> live 1213420927
  TMrXEza1CQ8GKfH5XSuV: archive  209755283 -> live  248605841

The three views that disagreed before this change (all measured):
  archive replay snapshot @h2201090   3,036.990591 TRI / 177 coins   (stale)
  node live window (frozen at seed)     1,627.639028 TRI /   7 coins (blind spot)
  wallet history.json (frozen h2205310) 4,586.577699 TRI / 286 coins (stale)
  sum of snapshot + live (the union)   4,664.629619 TRI              (double count)

Why the live index is the right source:
- it is updated on every accepted block, so it tracks the tip;
- it records per-output rows with `spent_height`, so it knows which coins
  are still unspent (the replay snapshot cannot: the chain replay stops at
  the archive seam h2201090 because the canonical band h2201750..h2203275 is
  missing from EVERY node's block store, and the upstream peers are all
  unreachable);
- it is the only source that can be both tip-accurate AND per-coin, which is
  what a spend needs: a coin list that cannot be a double-spend.

The change (src/rpc.rs, src/bin/follow_tip.rs, src/gui/app.rs,
src/gui/app_state.rs, src/gui/tests.rs):
- the daemon answers a new ops verb `getwalletspendable`: the wallet's
  unspent outputs from the live address index, plus the index height and the
  chain tip;
- getbalance / listunspent / getwalletinfo serve that live figure when the
  node has it, and fall back to the replay snapshot when it does not;
- getwalletinfo now reports `balance_partial` and `balance_height`, so a UI
  can say "partial — verified to h<N>" instead of presenting a capped number
  as the whole balance;
- the GUI balance card shows a warning notice when the node says partial;
- two regression tests: the live index wins over the snapshot, and the
  snapshot fallback is flagged partial.

`cargo test --release --lib`: 500 passed, 0 failed (498 before this change,
plus the 2 new ones).
