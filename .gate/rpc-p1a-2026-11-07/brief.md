# tri7: RPC-P1a — legacy `listaddressgroupings` + `getcheckpoint` verbs

Blast radius: 7 modified files, 0 new files, +721/−6. The two verbs live in
`src/rpc.rs` (NATIVE_METHODS += 2, ~180 lines of verb logic, ~280 lines of
tests, one helper `fmt_utc_timestamp`); `src/history.rs` records the one new
data point the grouping verb needs (`HistoryRec.input_addresses`, serde
default-empty, so older history.json files still load); `src/consensus.rs`
gains the canonical home for the two checkpoint constants that follow_tip
already compiled in (moved, not changed); the `WalletProvider` trait gains a
defaulted `labels()` method; `follow_tip.rs` + one struct literal each in
`gui/app_state.rs`, `gui/tests.rs`, `legacy_rows.rs` are mechanical
constructor-completion. GUI rendering is untouched.

OPERATOR DIRECTIVE (workqueue item 8 RPC-P1a): listaddressgroupings +
getcheckpoint verbs, judged, tested.

DEFINITION of absolute terms: "green" = the quoted test-result line verbatim;
"parity" = matches the deployed legacy v6 client's rpcwallet.cpp /
rpcblockchain.cpp behavior as cited inline; "warming" = the block index not
yet ready (an honest Miss, same policy as every other native verb).

## Headline captured evidence

### CMD: `git diff --stat` (working tree vs HEAD 871dbf3)
```
(see stat.txt — 7 files, +721/-6)
```

### CMD: `cargo test --lib` (full suite)
```
test result: ok. 484 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 46.09s
```
(474 -> 484: +5 listaddressgroupings, +2 getcheckpoint, +1 fmt_utc_timestamp
in rpc.rs; +2 in history.rs — spend-side input capture + old-file serde
compat.)

### CMD: `cargo test --bin follow_tip`
```
test result: ok. 43 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
```

### CMD: windows-gnu all-bins build (the push gate)
```
(see build.txt)
```

## What and why

### listaddressgroupings (rpcwallet.cpp:394)
Legacy groups addresses whose common ownership a tx made public: common use
as INPUTS, or an input plus its resulting CHANGE. tri7's data sources are all
its own — wallet history rows (the tx set), the per-address balance map
`listreceivedbyaddress` already serves (so the two verbs cannot disagree),
and the wallet's name rows (legacy mapAddressBook — legacy IsChange keys on
mine + unlabeled):

1. Per-txid aggregation over history rows: our output addresses ("in" rows),
   a send flag ("out" rows), and our INPUT addresses
   (`HistoryRec.input_addresses`, resolved by the txhistory generator from
   its own rows map at scan time — the prevouts of an ours-vin are by
   definition outputs we received, so the verb needs NO live prevout
   lookups).
2. Spend branch groups inputs ∪ change, where change = mine outputs ABSENT
   from the address book (legacy IsChange: mine + unlabeled). Labeled
   outputs are NOT change and stay lone singletons (tested).
3. Transitive merge-on-shared-member — the legacy setmap/hits algorithm;
   a shared member bridges two spend txs into one cluster (tested).
4. Rows are `[address, balance]` + a third label element only when the
   address book has one (legacy mapAddressBook gate, tested).
5. Coinstakes degenerate to the singleton branch: CreateCoinStake pays the
   stake back to the staked script (wallet.cpp:2332-2346).
6. Honest coverage: rows predating `input_addresses` (older history.json)
   contribute output singletons only; the count of such unresolved spend txs
   is reported on stderr with the rescan remedy. Ordering is deterministic
   base58 sort at both levels (legacy iterated uint160 sets) — noted in the
   verb comment.

### getcheckpoint (rpcblockchain.cpp:665)
Legacy reports `Checkpoints::hashSyncCheckpoint` — a RUNTIME value the
checkpoint master advances over the network. tri7 does not process
checkpoint-master messages; its checkpoint of record is the compiled-in
anchor (`consensus::CHECKPOINT_HORIZON` / `CHECKPOINT_ANCHOR_DISPLAY` — the
highest legacy checkpoint-map entry, checkpoints.cpp:82) which follow_tip
ENFORCES strictly on every fetch/backfill path. The verb reads the anchor
block's nTime via the height map and prints `{synccheckpoint, height,
timestamp, policy:"strict"}`; timestamp uses the legacy DateTimeStrFormat
"%Y-%m-%d %H:%M:%S UTC" via `fmt_utc_timestamp` (hand-rolled
civil-from-days, no chrono dependency, pinned by test at epoch / 2023 /
leap-day). Params are refused with -8 (legacy throws); warming index is an
honest Miss. No `checkpointmaster` key — tri7 has no -checkpointkey, exactly
how a legacy default-policy node without the master key prints.

### The one enabling data change: HistoryRec.input_addresses
`apply_block` already resolves ours-vin prevouts against its rows map to
mark spends; the new field records the addresses those prevouts paid FROM,
on every row of that tx (send row AND change row — tested at the block level
in history.rs with real script construction). `#[serde(default)]` keeps older
history.json files loadable (tested).

## Verification of the exact acceptance criteria (tests have teeth)

- Singleton/receive: two unrelated addresses -> two singleton groups; balance
  2.5 TRI from the same map listreceivedbyaddress reads; absent entry -> 0;
  unlabeled -> 2-element row (mutation check: dropping the `labels` gate on
  the third element makes `row0.len() == 2` fail).
- Spend/change: inputs ∪ unlabeled change merge; a LABELED output must NOT
  join (mutation check: dropping the `!labels.contains_key` filter makes
  `g.len() == 2` fail — TC would be swallowed).
- Transitive: TB shared by two spend txs bridges TA,TB,TC,TCh,TOut into one
  cluster (mutation check: dropping the merge-on-hit loop leaves 2 groups).
- Old-file honesty: a send tx with no recorded input side produces the
  singleton only (no crash, no fabricated grouping).
- getcheckpoint: pins the exact anchor hash/height/timestamp string/policy
  (mutation check: serving a runtime-advanced checkpoint or loose policy
  breaks the equality); params -> -8; warming -> honest miss.
- fmt_utc_timestamp pinned at 0 / 1.7e9 / leap-day 2024-02-29.

## Deployment note (ops half of item 8, after merge)

follow_tip binaries on the fleet are rebuilt from the committed tree and
restarted one at a time (DNS2 last, staking verified on before + after);
each node answers both verbs live — `getcheckpoint` vs the compiled anchor,
`listaddressgroupings` against DNS2's real history (the operator seed set's
10 addresses should cluster per the sweep txs' input sides).

## Provenance note (honesty)

The working tree also carried one UNRELATED 6-line fix
(src/gui/node_supervisor.rs: pass --sync-from-zero on fresh state dirs) that
landed mid-tick, apparently flushed by the previous failed run's pending
background tool. It is NOT part of this diff (the commit stages exactly the
7 files listed in stat.txt); it will be judged and committed separately so
the tree ends clean.
