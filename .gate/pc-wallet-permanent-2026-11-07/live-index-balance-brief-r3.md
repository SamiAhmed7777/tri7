# Round 3 — the four blockers from round 2, addressed

B1 "Change getwalletinfo to select the live result only when authoritative;
otherwise select the replay snapshot and mark it partial."
  -> getwalletinfo now uses `live_authoritative(deps)`: the live index is
     selected only when the node says it is authoritative. Otherwise the
     replay snapshot is selected (its balance, its utxo count, its coverage
     height and hash) and the answer is flagged partial.

B2 "Remove the one-block-behind authoritative allowance or establish a
coherent synchronization mechanism that proves the index corresponds to the
selected tip."
  -> The one-block tolerance is GONE. The daemon now tracks
     LAST_INDEXED_HEIGHT - the height of the last block the live index
     accepted - and applies each block to the index BEFORE advancing the
     chain tip (so the index is never behind). The index is authoritative
     only when it is a FINISHED index (`complete`) and
     LAST_INDEXED_HEIGHT >= the chain tip: the index has then applied every
     block this node accepted, which is exactly what proves the index
     corresponds to the selected tip. No tolerance, no guessing.

B3 "Add assertions that incomplete and lagging live indexes make
getwalletinfo return the replay balance and replay coverage height."
  -> Both tests now assert the REPLAY values: balance 3036.990591,
     balance_height 2201090, coverage_height 2201090 and the replay
     coverage hash, with balance_partial = true.

B4 "Provide consistent, reproducible evidence with an actual revision hash,
exact commands/addresses, and one verified test count."
  -> The evidence file now carries the revision hash, the exact commands,
     the exact address and the single test count (504 passed, 0 failed).

Polish applied: the comments no longer claim the live figure always wins
(the helper documents that callers serving it as the whole balance must use
`live_authoritative`); `coverage_hash` is null for a live-only result rather
than an invented empty string.

Test count (one, same revision): 504 passed, 0 failed, 1 ignored.
