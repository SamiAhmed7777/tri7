# The `tri7` command line + seed tooling (wallet_import_seed, seed_sweep)

Operator directive: "Make a full fledged triangles 7 CLI" and "a new triangle
CLI that gives an easy way to do the sweep".

## Contents (all new files; clap added as the only new dependency)
1. src/bin/tri7_cli.rs → the `tri7` binary ([[bin]] name="tri7" in Cargo.toml):
   - `tri7 wallet create-seed|import-seed|newaddress|info|dumpwallet|importwallet|checkwallet`
     — BIP39 phrase wallet creation (phrase printed once), phrase import into an
     existing wallet (skip-if-held), keytool parity ops via tri7_core::keytool.
   - `tri7 chain --rpc <u> tip|bestblockhash|blockhash|rawtransaction|connections`
   - `tri7 staking --rpc <u> info|on|off`
   - `tri7 sweep --wallet <f> --rpc <u> --mnemonic-file <f> [--exclude ...] [--dry-run]`
     — sweeps ALL wallet-owned coins into the phrase's 10-address set:
     coins live from listunspent, matched to keys by SECRET-DERIVED payloads
     (no stored-pubkey dependency — legacy-migrated rows sweep too), maturity
     enforced per consensus depth, built+signed OFFLINE by sweep::plan_sweep
     (the GUI's tested flow), relayed via sendrawtransaction.
   - `tri7 send --wallet <f> --rpc <u> --to <a> --amount <n> [--dry-run]`
     — single-address send with change returning to a wallet address, same
     offline build+sign flow (build_spend_outputs).
   - `tri7 richlist --file <balances.json> [--top N]`
   Thin argv layer over tested lib code — no second consensus implementation.

2. examples/wallet_import_seed.rs — the standalone import tool (skip-if-held,
   no panic on already-held keys).
3. examples/seed_sweep.rs — the standalone sweep tool (same flow as the CLI
   sweep subcommand).

## Live fleet evidence (2026-11-07 ~08:1xZ)
- `tri7 chain tip` = 2207046 (matches DNS2); `tri7 wallet info` = 248 keys;
  `tri7 richlist` renders the real top addresses from the snapshot.
- `tri7 sweep --dry-run`: 149 coins, 2380.407066 TRI, fee 1800 sats, 10 outputs.
- REAL broadcast via the CLI: txid 783cb4d4ef39b9c853a5c3628e900b1f8c98f6d2dd0631f600ec0f0f80751560
  — MINED in block 0950b8621c47d7adb60e... (confirmations 1+). The 10 outputs
  byte-match the seed address set (verified by local tx decode + DNS2 listunspent:
  2798.67 TRI now on the seed set including the earlier 387.33/30.94 sends).
- Earlier sends through the same fixed path: d7612285... (Pi 100 TRI),
  ca2ec8c4... (DNS3 100 TRI) — both mined, balances verified on-node.
- cargo test --release --lib: 455 passed, 0 failed.

## Deployment
CI windows-build adds tri7 (follow-up artifact-list tweak rides the next CI
change); DNS2 already runs the Linux build.
