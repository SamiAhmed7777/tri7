# The completeness flag must mean what it says

## THE HOLE THIS FIXES

The previous revision flagged the snapshot fallback partial by comparing the
snapshot's coverage against the LIVE TIP, and by requiring the snapshot's
hash to match the node's chain. Measuring it on Samihost exposed the flaw:

    BEFORE (Samihost, right after its store was repaired)
      balances.json  height 2201090   wallet 248 addrs 177 UTXOs 3036990591 sats
      getwalletinfo  balance 3036.990591 TRI   balance_partial false

That figure was WRONG. It came from the old debris-contaminated store: the
replay read the fork-branch frames, so it counted outputs that only ever
existed on the abandoned branch. The canonical replay (no debris) gives:

    AFTER (Samihost, canonical store, replayed genesis..h2207920)
      balances.json  height 2207920   wallet 310 addrs 8 UTXOs 139891748 sats
      balances-build: done in 117.8s - coverage h2207920 tip af61aeb8...

The old flag said "false" for a figure that was inflated by fork coins,
because neither of its two tests could see the fork: the coverage-vs-tip
comparison was replaced by a "the tip moves" allowance, and the hash test
only proves WHICH chain the figure is on, not whether the replay covered the
whole of it.

## THE FIX: THE DAEMON'S OWN FACT

The build ALREADY refuses to publish a snapshot that stopped short of the
tip it was chasing (check_coverage: a hole in the store is an error, not a
snapshot). The daemon knows the truth; the flag just never asked it. So the
snapshot now records it:

    "height": 2207920,          <- what the replay reached
    "replay_target_height": 2207920,   <- the tip the replay was chasing

and the completeness rule requires BOTH:

  (1) CHAIN IDENTITY - the snapshot's hash at its coverage height must equal
      the hash THIS node serves at that height (the node's own store). A
      snapshot from another chain, or one that names no hash, fails closed.
  (2) WHOLE-CHAIN COVERAGE - the snapshot's coverage must EQUAL its replay
      target, and the replay must have walked every height from genesis
      (blocks_replayed == height + 1). A coverage BELOW the target means the
      replay was capped by a hole: the figure omits everything above the
      cap, and it is flagged.

The tip MOVING after the snapshot is not a cap, so it is not part of the
rule: a snapshot that covered the chain to hN is complete even when the tip
has since advanced to hN+k. Comparing against the moving tip would flag
every wallet as partial forever.

## WHY THIS IS THE RIGHT FACT

The daemon is the only party that can tell the difference between "the
replay covered the whole chain and the chain has since moved on" and "the
replay stopped at a hole". It knows because it chased a tip and knows where
it ended up. The RPC asks it.

## TESTS

    cargo test --release --lib   -> 512 passed; 0 failed; 1 ignored

The focused tests:

    an_at_tip_snapshot_fallback_is_not_flagged_partial
        the replay reached the tip it was chasing -> partial == false
    a_proven_whole_snapshot_behind_the_moving_tip_is_not_partial
        proven whole, but the tip has moved on -> partial == false
    an_at_tip_snapshot_on_a_different_chain_stays_flagged_partial
        the tip height but a hash this node does not serve -> partial == true
    a_capped_snapshot_below_its_replay_target_stays_flagged_partial
        the coverage is BELOW the target (a hole capped it) -> partial == true
    a_snapshot_without_a_replay_count_proof_stays_flagged_partial
        no replay-count proof at all -> partial == true (fails closed)

## THE FLEET, REPAIRED AT THE ROOT

The store is rebuilt to contain only the canonical chain
(src/bin/store_canonicalize.rs, Codex grade A urn:
urn:ump:w6olnybua3u75wt4vk3juiobhr36xvz2ngmn4vi3v6kcoyh4tzda). Verified per
node:

  DNS2      2208033 frames, 0 debris, 0 duplicates
  Pi        2208037 frames, heights_ok=true (replaced from DNS2's canonical
            store, sha256 8289fffc...a83400 matched byte for byte; h2201750
            and h2203039 now resolve, and h2203039 returns the canonical
            anchor e3797bac...8f6d)
  Samihost  2207921 frames, 0 debris, 0 duplicates (canonicalized in place
            with the Linux tool; the original kept as blk0001.dat.pre-canon)
