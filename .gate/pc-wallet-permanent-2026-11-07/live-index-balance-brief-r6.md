# Round 6 — the two blockers from round 5, addressed

B1 "Read UTXOs, coverage height, tip hash, and completion state under one
coherent database snapshot/transaction before applying the authority rule."
  -> `addrindex::wallet_view(c, addrs)` opens ONE deferred transaction and
     reads the index's facts (applied height, the hash it recorded, the
     completion flag) AND the wallet's unspent outputs from it. The daemon's
     `getwalletspendable` uses that helper, so the balance, the utxo list
     and the coverage height can never come from two different moments. The
     RPC's authority rule then runs on exactly those facts.

B2 "Make every post-BEGIN error path in LiveIndex::apply explicitly roll
back, preferably using rusqlite transaction RAII, and test a failure
occurring after block mutations begin."
  -> `LiveIndex::apply` now holds a rusqlite `Transaction` (RAII) instead of
     a hand-rolled BEGIN/COMMIT: ANY error after the transaction opens - the
     output insert, the tip-hash write, the height write, the commit - rolls
     the whole block back, because the guard drops the transaction.
  -> The new test injects the failure AFTER the block's outputs were
     written (the tx_addr table is dropped, and insert_block writes the
     outputs before the tx_addr rows): the apply must fail, the wallet's
     unspent total must still be the previous block's coin alone (the failed
     block's 7,000,000 output gone), and the index's height and hash must
     not have moved.

"Add restart tests based on real LiveIndex::apply operations, including
recovery following an injected mid-apply failure."
  -> `the_index_position_survives_a_restart` now applies TWO REAL BLOCKS
     through the production `LiveIndex::apply`, marks the index finished
     with the production `addrindex::set_complete`, then opens a fresh
     connection (what a restarted node gets) and asserts the recovered
     position - the applied height and the hash of the applied block - and
     that the authority rule holds against the recovered position.

Polish applied: the evidence typo is fixed; the brief no longer calls the
tests "production-level" - they now exercise the production transaction
helper (wallet_view), the production apply lifecycle (LiveIndex::apply)
and the production completion flag (set_complete).

Test count (one, same revision): 507 passed, 0 failed, 1 ignored.
