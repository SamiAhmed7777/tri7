# No partial/missing messaging in the wallet — the number must simply be right

## WHAT CHANGED (5 files)

1. **src/gui/app.rs** — the Overview page's "Partial balance" warning banner
   is REMOVED outright, not softened. The fields that fed it are gone from
   src/gui/app_state.rs (with the code that read them from getwalletinfo),
   and the stale struct fields are gone from src/gui/tests.rs. Nothing in
   the GUI reads `balance_partial` any more.

2. **src/rpc.rs** — the honesty rule behind the flag was WRONG and is fixed.
   The snapshot fallback was flagged `balance_partial: true`
   UNCONDITIONALLY, so even a replay that covered the whole chain was
   reported as partial. The flag now means exactly what it says, and it
   FAILS CLOSED — BOTH facts must hold:

   (1) CHAIN IDENTITY. The snapshot names the hash it verified at its
       coverage height; that hash must equal the hash THIS node serves at
       that height (the node's own store, via hash_at). A snapshot from
       another chain — or one that names no hash at all — cannot be proven
       to be this chain's, so it fails closed.

   (2) WHOLE-CHAIN COVERAGE. The replay must have walked EVERY height from
       genesis: the snapshot's own `blocks_replayed` must equal
       `height + 1`. The snapshot publishes that count, and the daemon now
       carries it through to the RPC (BalanceView.blocks_replayed).

   The tip MOVES while the node runs, so the rule does NOT compare the
   snapshot's coverage against the live tip: a snapshot that covered the
   chain to hN is complete even when the tip has since advanced to hN+k.
   Comparing against the moving tip would flag every wallet as partial
   forever, which is exactly the noise this flag must not be. The
   snapshot's own coverage is the honest bound.

3. **src/bin/follow_tip.rs** — THE MOST IMPORTANT ONE, and it was found by
   MEASURING rather than by reading: the served balance was WRONG, because
   the wallet live view is WINDOW-SCOPED.

   The live view is the wallet's unspent set, seeded from the chain-state
   window base (h2203425 on DNS2). On a node that resumed from a state file
   it therefore MISSES every coin the wallet received BEFORE that base.
   Measured on DNS2 after the store repair:

       wallet live view (window-scoped):   1,628,710,364 sats = 1628.71 TRI
       full-chain replay (genesis..tip):   5,292,853,544 sats = 5292.85 TRI

   (the live view's figure moves as the window moves — 1,628,592,091 at
   18:00:17, 1,628,614,863 at 18:06:40, 1,628,710,364 at 19:32:19 — the
   point is that it is ~1,629 TRI while the whole wallet is ~5,293 TRI)

   The live view was winning because it was "live", and it was serving a
   figure that omitted the wallet's older coins. The fix: the live view may
   serve the balance ONLY when it replayed the wallet's WHOLE history
   (base == 0, the from-zero case); otherwise the replay snapshot — which
   covers the chain from genesis — is served instead.

## WHY THE NUMBER IS NOW RIGHT

The "partial balance" banner existed because the node's chain replay was
capped: DNS2's store had a hole, so the replay stopped at h2201090 and the
wallet could not be verified beyond it. That hole is FIXED at the root:

  - the store is rewritten to contain only the canonical chain
    (src/bin/store_canonicalize.rs, Codex grade A urn:
    urn:ump:w6olnybua3u75wt4vk3juiobhr36xvz2ngmn4vi3v6kcoyh4tzda), which
    re-threaded every height: "[blockidx] ready: 2208025 frames,
    heights_ok=true, threaded_tip=Some(2208024)";
  - the balance replay now walks EVERY height from genesis: the snapshot
    records blocks_replayed 2207976 == height 2207975 + 1, and
    "balances-build: done ... wallet 262 keys 5292853544 sats";
  - every height in the old hole (h2201750..h2203320) now resolves through
    getblockhash (evidence below);
  - getwalletinfo now answers balance 5292.853544 TRI, balance_height
    2207975, 262 addresses, 425 UTXOs, balance_partial FALSE — the
    complete figure, and the same number the full replay produces.

So the banner is not being suppressed to hide a wrong number: the number is
now the whole wallet's, and the flag that says otherwise no longer fires
for the reason it used to.

## THE FOCUSED RPC TESTS (added for this change)

    fn an_at_tip_snapshot_fallback_is_not_flagged_partial()
        a snapshot whose replay REACHED the tip -> balance_partial == false
    fn a_below_tip_snapshot_fallback_stays_flagged_partial()
        a snapshot that stopped SHORT -> balance_partial == true
    fn an_at_tip_snapshot_on_a_different_chain_stays_flagged_partial()
        the tip HEIGHT but a hash this node does not serve -> partial == true

The pre-existing tests still hold and still cover the live-index rule: an
index one block behind the tip is not served as the whole wallet, an index
on a different chain at the same height is not served, and a snapshot that
stops short is still flagged.

## TESTS

See no-partial-messaging-tests.log:

    cargo test --release --lib                    -> 510 passed; 0 failed; 1 ignored

## LIVE DNS2 EVIDENCE

See dns2-live-balance-evidence.log — the exact command, the timestamps, the
sha256 of the deployed binary AND of the reviewed sources, the
getwalletinfo / getbalance responses, the chain-identity proof (the
snapshot's covered_tip_hash vs the node's own getblockhash at that height),
the snapshot's coverage facts, a getblockhash probe of the heights that
used to be missing (all OK now), and the daemon log lines for the index,
the replay and the live view.
