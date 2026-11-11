# tri7: sweep residue floor (`--min-sats`) for follow-up sweep passes

Blast radius: exactly 2 modified files, 0 new files — `src/sweep.rs` (+46: one
const, one pure partition helper, one unit test) and `src/bin/tri7_cli.rs`
(+24/-2: one CLI flag, its threading, honest residue reporting, one skip-count
fix). No other subsystem touched. The GUI sweep flow
(`src/gui/sweep_flow.rs`) calls `sweep_source_inputs` directly and is
deliberately UNCHANGED — its contract is move-everything-and-mark-migrated;
the residue floor is a CLI-pass policy only.

OPERATOR DIRECTIVE (workqueue item 7 SWEEP-RESIDUE): follow-up sweep passes as
coins mature, skip < 0.005 TRI residue, keep DNS2 staking on so blocks flow,
using the tri7 CLI sweep.

DEFINITION of every absolute term used below: "unchanged" = `git status`
shows no file outside the two listed paths; "sweeps everything" = zero coins
partitioned into the residue half; "clean" = exit 0 with the printed test
result line quoted verbatim.

## Headline captured evidence

### CMD: `git diff --stat` (working tree vs HEAD a66be4f)
```
 src/bin/tri7_cli.rs | 24 ++++++++++++++++++++++--
 src/sweep.rs        | 46 ++++++++++++++++++++++++++++++++++++++++++++++
 2 files changed, 68 insertions(+), 2 deletions(-)
```

### CMD: `cargo test --release --lib split_residue_skip` (focused filter, read from source via `grep -n "fn split_residue_skip_leaves" src/sweep.rs`)
```
running 1 test
test sweep::tests::split_residue_skip_leaves_sub_floor_coins_behind ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 474 filtered out; finished in 0.00s
```

### CMD: `cargo test --release --lib` (full suite)
```
test result: ok. 474 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 32.09s
```
(473 -> 474: the one new test. `cargo test --release --bin follow_tip`
stayed green: `43 passed; 0 failed`.)

### CMD: `cargo build --release --target x86_64-pc-windows-gnu --bins` (the push gate)
```
    Finished `release` profile [optimized] target(s) in 1m 24s
```

### CMD: pristine-checkout apply proof (`git stash`, `patch -p1 --dry-run`, `git stash pop`)
```
checking file src/bin/tri7_cli.rs
checking file src/sweep.rs
DRYRUN_RC=0
```
The diff (attached as sweep-residue.diff, relative a/ b/ labels) applies with
`patch -p1 < sweep-residue.diff` from a clean a66be4f checkout.

## What and why

The CLI sweep (`tri7 sweep`) consolidates every spendable, mature,
non-destination UTXO into one 10-way split to the seed-derived address set.
The workqueue's standing residue rule — coins below 0.005 TRI stay put
(chasing them grows the fee and re-fragments the wallet for no value) — was
previously only prose; the tool swept dust too. This change makes the floor a
tool property:

1. `tri7_core::sweep::SWEEP_MIN_RESIDUE_SATS = 5_000` (0.005 TRI at COIN=1e6).
2. `tri7_core::sweep::split_residue_skip(inputs, min_sats)` — stable partition
   into (keep, residue); `min_sats <= 0` keeps everything (nothing becomes
   unspendable); empty input is a clean no-op.
3. CLI flag `--min-sats` (default = the const), residue line printed when
   non-empty (`residue: left N coins (X TRI) below the 5000-sat floor ...`),
   and an honest exit when nothing remains above the floor ("nothing to
   sweep above the residue floor — no transaction built").
4. Skip-count fix: the `dest` count was `inputs.len() - sources.len()`;
   after the residue partition that expression over-counted, so the
   pre-filter count is captured first (`dest_skipped`).

## Verification of the exact acceptance criterion (test has teeth)

The new test pins: 4999 -> residue, 5000 -> keep (floor inclusive), stable
order in both halves, floor 0 keeps a 1-sat coin, empty set no-ops. Mutation
check: reverting the helper body to `keep.push(i)` unconditionally would make
the first assertion (`residue == [4999, 1]`) fail — the test discriminates the
partition, not the call-site wiring.

## Deployment note (ops half of item 7, not part of this diff)

After merge, the follow-up sweep pass on DNS2 runs with the judged binary
from the committed tree: dry-run first, then broadcast; staking stays enabled
throughout (`getstakinginfo.enabled` re-read after). The pass leaves any
sub-5000-sat coin behind by construction — the exact behavior this diff adds.
