# Round 5 — the two blockers from round 4, addressed

B1 "Read UTXOs, coverage height, tip hash, and completion state under one
coherent database snapshot/transaction before applying the authority rule."
  -> New production helper `addrindex::wallet_view(c, addrs)`: it opens ONE
     deferred transaction and reads the index's facts (applied height, the
     hash it recorded, the completion flag) AND the wallet's unspent outputs
     from that same transaction, then commits. The daemon's
     `getwalletspendable` uses it, so the balance, the utxo list and the
     coverage height can never come from two different moments. The RPC's
     authority rule then runs on exactly those facts.

B2 "Add real tests that interleave index advancement with wallet reads and
exercise apply/advance failure and restart recovery."
  -> Three new tests, all on a REAL index on disk:
     - `interleaved_index_advance_never_tears_a_wallet_read`: a writer
       thread keeps applying blocks (meta height + a new output) while the
       test reads the wallet through the real RPC; every read must be
       internally consistent;
     - `a_failed_apply_leaves_the_index_unchanged`: an out-of-order apply
       through the production `LiveIndex::apply` must fail AND leave the
       index's height, hash, completion flag and utxo set exactly as they
       were (the apply path's own transaction rolls back);
     - `the_index_position_survives_a_restart`: a fresh connection to the
       index (what a restarted node gets) recovers the position from the
       index's own meta rows, and the authority rule then holds against the
       recovered position - no in-memory state to lose.

Polish applied: the stale "one block of slack" comment in the daemon is
replaced with the exact rule that is implemented; the evidence file is
self-contained (it carries the probe's full source, the commands, the
revision and one full wallet address).

Test count (one, same revision): 507 passed, 0 failed, 1 ignored.
