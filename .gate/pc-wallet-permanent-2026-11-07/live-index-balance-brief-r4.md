# Round 4 — the four blockers from round 3, addressed

B1 "Establish an atomic/coherent publication boundary so the selected tip and
index state correspond exactly; do not publish index authority before chain
advancement succeeds, and provide rollback or transactional behavior on
failure."
  -> The chain advance happens FIRST; the index is applied only after the
     block is part of the chain this node serves (follow_tip.rs:
     advance_with_block -> addrindex_apply). The index's own transaction
     (BEGIN/ROLLBACK/COMMIT in LiveIndex::apply) means a failed apply leaves
     the index exactly where it was - it never half-applies a block.
     Authority is no longer a flag the daemon asserts: the daemon reports
     FACTS and the library computes the verdict from them.

B2 "Initialize or recover the applied index position at startup and prove
chain identity, not merely numeric height."
  -> The index now records the HASH of the block it applied
     (meta 'tip_hash', written by LiveIndex::apply). The library rule
     (addrindex::authority) requires: a finished index, the index's applied
     height EQUAL to the chain height, AND the index's recorded hash EQUAL to
     the chain's hash at that height. The height alone no longer proves
     anything; the hash does. The position is recovered from the index's own
     meta rows at startup (the same rows the rule reads), so a restart
     re-establishes it without any in-memory counter.

B3 "Add production-level tests for the ordering race, advance failure,
restart with a complete index, and lagging/incomplete indexes; avoid stubs
that independently synthesize the property under test."
  -> The tests no longer synthesize "authoritative". They build a REAL
     address index on disk (the production schema, via addrindex::begin),
     seed its own meta rows and outputs, and let the production rule decide.
     The tests now cover: a finished index on the same chain at the same
     height (served), an index one block behind (not served), an index on a
     DIFFERENT chain at the same height (not served - the hash proves it),
     an index that is still building (not served), a finished index with no
     replay snapshot behind it (served, no snapshot required), and no view at
     all (an error, never 0).

B4 "Provide genuinely reproducible evidence with the complete address and
accessible revision/source tree."
  -> The evidence file names the revision, the source tree, the exact
     commands, and one FULL wallet address (not truncated) with its archive
     vs live balances.

Polish applied: the stale "one block of slack" comment is gone; the
in-memory LAST_INDEXED_HEIGHT counter is gone, so there is a single
coherent coverage height (the index's own applied height).

Test count (one, same revision): 504 passed, 0 failed, 1 ignored.
