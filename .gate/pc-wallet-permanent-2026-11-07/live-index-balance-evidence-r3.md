MEASURED EVIDENCE - revision ee5534dfdca885851d265c907ae4db95bbc82ccb (repo /root/tri7)

COMMANDS (run on the DNS2 host):

  git rev-parse HEAD
  cargo build --release --example probe_index_utxos
  ./target/release/examples/probe_index_utxos \
      /root/tri7-lab/mainsweep/pc-live-wallet.dat /var/lib/tri7/addressindex.db

  -> wallet rows: 399 | addresses: 399
  -> index meta height: Some("2206738")
  -> UNSPENT UTXOs (live index): 225
  -> TOTAL: 3236439612 sats = 3236.439612 TRI
  -> utxo height range: 2031567..2206738

THE INDEX MOVES WITH THE CHAIN (same address, archive vs live), via the
node's own RPC on 127.0.0.1:19112:

  curl -s -X POST http://127.0.0.1:19112 -H 'Content-Type: application/json' \
       -d '{"method":"getaddressbalance","params":["TBxrQx34J2mRvkmixnrd..."]}'
  -> archive snapshot 1148561196  vs  live index 1213420927
  -> archive snapshot  209755283  vs  live index  248605841

THE THREE VIEWS THAT DISAGREED (all measured before the change):
  archive replay snapshot @h2201090   3,036.990591 TRI / 177 coins   (stale)
  node live window (frozen at seed)     1,627.639028 TRI /   7 coins (blind spot)
  wallet history.json (frozen h2205310) 4,586.577699 TRI / 286 coins (stale)
  union of snapshot + live              4,664.629619 TRI              (double count)

WHY THE LIVE INDEX IS THE RIGHT SOURCE:
- it is applied on every accepted block (src/bin/follow_tip.rs ::
  addrindex_apply), so it follows the tip;
- it stores per-output rows with `spent_height`, so it knows exactly which
  coins are still unspent - the replay snapshot cannot: the chain replay
  stops at the archive seam h2201090 because the canonical band
  h2201750..h2203275 is missing from EVERY node's block store and the
  upstream onion/I2P peers are all unreachable;
- it is the only source that is both tip-accurate AND per-coin, which is
  what a spend needs: a coin list that cannot be a double-spend.

TEST CLAIM (one count, same revision):
  cargo test --release --lib
  -> test result: ok. 504 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
