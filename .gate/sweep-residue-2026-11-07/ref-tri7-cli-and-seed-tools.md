# The `tri7` CLI and seed tooling

`tri7` is a real binary (`[[bin]]` name="tri7" in Cargo.toml, clap) — the
operator-facing command line, not an example. Subcommands:

- `wallet create-seed --out <f> [--count N]` — new wallet from a fresh
  24-word phrase (phrase printed ONCE at end of stdout)
- `wallet import-seed --wallet <f> --mnemonic-file <f> [--count N]` — add a
  phrase's derived keys to an EXISTING wallet (skips already-held keys)
- `wallet newaddress --wallet <f> [--count N]` — random keys
- `wallet info|dumpwallet|importwallet|checkwallet` — keytool parity
- `chain --rpc <u> tip|bestblockhash|blockhash|rawtransaction|connections`
- `staking --rpc <u> info|on|off` — on/off persists in the node's conf
- `sweep --wallet <f> --rpc <u> --mnemonic-file <f> [--exclude a,b]
  [--min-sats N] [--dry-run]` — sweep ALL wallet-owned coins into the
  phrase's 10-address set (m/44'/2222'/0'/0/0..9), built+signed OFFLINE by
  sweep::plan_sweep, relayed via sendrawtransaction
- `send --wallet <f> --rpc <u> --to <a> --amount <n> [--dry-run]` —
  single-destination send, change to a wallet address, same offline flow
- `richlist --file <balances.json> [--top N]`

Arg order: group flags (`--rpc`) come BEFORE the subcommand word.

Coin matching in send/sweep uses SECRET-DERIVED payloads — it sweeps
legacy-migrated rows even when the wallet row has no stored pubkey (the
node's in-memory filter can miss those; the CLI does not). Multi-input sends
need a binary at or past c1823e7 (vin-order connect-check fix).

Residue floor (workqueue SWEEP-RESIDUE rule, commit 871dbf3): follow-up sweep
passes skip coins below `--min-sats` (default 5000 = 0.005 TRI; 0 sweeps
everything) — chasing sub-floor dust grows the fee and re-fragments the
wallet for no value. Implementation: `sweep::split_residue_skip` (stable
partition, `min_sats <= 0` keeps all), CLI prints a `residue:` line and
exits honestly when nothing remains above the floor. The GUI sweep flow
(sweep_flow.rs) does NOT apply the floor — its contract is
move-everything-and-mark-migrated; keep the floor CLI-only. Live proven
2026-11-07 on DNS2: pass 1 swept 56 inputs / 555.77 TRI (mined h2207308,
fee 700 sats), a legacy address re-staked mid-pass (its output went
immature, skipped by the maturity gate), pass 2 swept that matured tail
(26.49 TRI, mined h2207311) — non-seed balance 582.25 → 0.000000 TRI with
staking enabled throughout. A coin that re-stakes between `listunspent` and
broadcast re-selects cleanly because each run rebuilds from a fresh snapshot;
a mid-run stake spends a selected prevout and the connect check refuses the
tx whole (single-tx atomicity — rerun, never force).

Standalone seed tools under examples/ (same flows, scriptable):
- `wallet_from_seed.rs` — create wallet from a new 24-word phrase; the
  phrase prints once at END of stdout; roundtrip-verify (rebuild with
  --mnemonic, addr 0 byte-matches) before deploying anywhere
- `wallet_import_seed.rs` — import a phrase's keys into an existing wallet,
  skip-if-held, no panic on duplicates
- `seed_sweep.rs` — standalone sweep (RPC-driven coins + offline signing)

Rules: deliver operator seed phrases as FILES on the operator's machine
(0600), never through chat; roundtrip-verify a derived wallet before
deploying it; fund nodes from tip-CURRENT wallets (a trailing staker picks
coins newer blocks already spent and the send dies in the connect check).
Keep CI's windows artifact list carrying tri7.exe alongside follow_tip and
the GUI.
