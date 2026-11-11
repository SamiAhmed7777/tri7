# Judge brief: tri7 lane-sweepheight — sweep maturity measured against the daemon's live height

## The live operator incident (2026-11-10, PC wallet, AFTER the hash160 fix deployed)
The seed migration imported the phrase (10 seed keys present) then reported
"Seed phrase added to this wallet … This wallet had no spendable balance to
migrate, so nothing was swept." — while the wallet held 5110.9 TRI total and
20 spendable UTXOs (~1534 TRI liquid, daemon listunspent). Nothing swept, no
error shown.

## Root cause (parent-verified in code + live)
sweep_source_candidates measured coin maturity against the GUI's LOCAL
tipstate snapshot: `tip_height = self.tip.map(...).unwrap_or(0)`. On the
operator's fresh install there IS no local tipstate (the node/state dir is
empty — verified), so spend_height collapsed to 1; depth = saturating_sub(1,
2208751) = 0 < COINBASE_MATURITY (7) filtered EVERY coinstake UTXO (the
whole balance is stake coins) → empty candidates → the caller's
"no spendable balance to migrate" (app.rs:1558). The same local-snapshot
design flaw the operator rejected for the sync bar ("why would it be a local
file") was still live in the sweep path.

## Fix
The sweep now measures maturity against the DAEMON's live height
(debug_info.blocks, the same authority as the attach-mode sync display),
falling back to the local tipstate only for an offline wallet view, and
FAILING LOUD ("no chain height available … cannot determine coin maturity")
when neither exists — never a silent empty sweep.

## Tests (+1, both directions asserted)
sweep_sources_measure_maturity_against_daemon_live_height: the exact
incident shape (no local tip, daemon reachable at 2208751, mature coinstake
at 2208600) selects the coin — this state returned EMPTY under the old code;
and tip=None + debug_info=None errors with "no chain height available".

## Parent verification (private target-sweepheight)
cargo test --lib: **567 passed; 0 failed; 1 ignored**. GUI-feature check
(compiles src/gui/app.rs): 0 errors. Diff: src/gui/app_state.rs,
src/gui/tests.rs only.
