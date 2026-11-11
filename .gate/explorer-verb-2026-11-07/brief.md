# Brief: tri7 EXPLORER-VERB (workqueue item 4) — native richlist + --balances builder + GUI live preference

## Artifact
Repo /root/tri7 (master), diff in `change.diff` (9 files, ~2040 lines incl. the new module).
Deliverable: every tri7 node functions as an explorer — the daemon serves a
native `richlist` verb from its balances snapshot; a node started with an
explicit `--balances PATH` BUILDS and maintains that snapshot in a background
thread; the GUI explorer prefers the live verb and falls back to the
balances.json file; the embedded-node supervisor passes --balances so GUI
installs get the same capability with zero manual tooling.

## Measured delta vs HEAD (b5351f9)
- ADDED src/balances_build.rs (~700 lines incl. 4 tests): the replay/aggregate/
  write engine extracted from bin/balances.rs into the lib, writing snapshots
  ATOMICALLY (tmp+rename — previously balances.json was a plain fs::write; only
  the utxos dump was atomic). Same JSON layout, byte-compatible consumers.
- CHANGED src/bin/balances.rs: now a thin CLI over the lib fn (same output
  lines, same exit semantics; wallet flag now optional — the daemon path needs
  wallet-less builds; all historical invocations pass --wallet explicitly, verified).
- CHANGED src/rpc.rs: NATIVE_METHODS += "richlist" (passive — NOT in
  OPS_METHODS, so it answers even while the ops engine is warming up);
  WalletProvider gains a defaulted `rich_list(top)`; RichListData +
  rich_list_from_value (balance DESC, address ASC tie-break — deterministic,
  not HashMap order; positive balances only); rich_list_json (rank,
  balance_sats, balance_tri via sats_to_tri = COIN 1e6 six-decimal rule,
  snapshot_height + chain_tip_height + total_ranked honest labeling);
  RichListCache (mtime+len keyed, re-parses only on change so a background
  rebuild is visible WITHOUT a daemon restart); native_call arm (count param,
  default 100, clamp 1..=1000). 5 new tests.
- CHANGED src/bin/follow_tip.rs: Config.balances_build (set ONLY by an explicit
  --balances flag); usage text updated; background "balances-build" thread
  (build-if-missing-or-stale-by->5760-blocks, 6h re-check, 10min retry while a
  fresh from-zero folder has no blk file yet, end_height=0 + allow_stale —
  accept whatever the walk reaches, honest height/hash labeling, pairs the
  wallet_utxos.json rebuild with the balances rebuild so the listunspent
  coverage gate (height AND hash across BOTH snapshots) stays green; NO daemon
  locks — try_lock-only tip sample, explorer pressure separable from
  relay/staking per the EXPLORER PRESSURE PRINCIPLE); LiveWalletProvider gains
  balances_path + rich_cache and implements rich_list.
- CHANGED src/gui/explorer.rs: parse_rich_list_rpc (live verb response parser),
  parse_rich_list re-labeled honestly as the FALLBACK path ("snapshot fallback
  … live daemon richlist unavailable" — the old label claimed "the daemon RPC
  has no rich-list method", which this item makes false); load_rich_list_rpc_action.
- CHANGED src/gui/app_state.rs: explorer_rich_action now tries the LIVE verb
  first (rpc_client allow-list gated) and falls back to the snapshot file,
  surfacing BOTH errors when both paths fail.
- CHANGED src/gui/rpc_client.rs: ALLOWED_METHODS += "richlist" (read-only verb).
- CHANGED src/gui/node_supervisor.rs: embedded node spawn now passes
  --balances <state>/balances.json — every GUI install builds + serves its own
  snapshot (no manual balances.exe runs; CI/installer never shipped that bin).
- CHANGED src/lib.rs: balances_build registered.

## Verification already run (commands + results, from the reviewed tree)
- cargo check --bins --features gui: zero errors.
- cargo test --lib: 464 passed / 0 failed (455 before + 9 new: 4 balances_build,
  5 richlist/rich-list in rpc.rs).
- cargo test --release --bin follow_tip: 42 passed / 0 failed.
- Windows gnu cross-build (all bins, release) is part of the pre-push gate.

## Known honest limitations (disclosed, by design)
- The replay walks a blk file sequentially and stops at the first stale-branch
  seam (shared ArchiveWalker contract); on hosts whose store descends from the
  legacy archive (DNS2) the snapshot therefore covers to the archive's seam
  height (~h2201090), while self-built from-zero stores (sami-pc embedded node)
  cover to the live tip. The verb ALWAYS returns snapshot_height +
  covered_tip_hash + chain_tip_height so consumers can judge the lag; the GUI
  rich list renders the source label verbatim.
- The verb needs the snapshot file to exist: without --balances (explicit) the
  node never writes one (unchanged behavior); the error tells the operator to
  start with --balances or run the balances bin.
- Rebuild cadence is 6h checks / >5760-block staleness — a rich list is a
  survey view, not a balance truth source (per-address balances stay available
  via getaddressbalance/listunspent paths).

## Rubric focus
Correctness of the new verb + builder threading/locking; no regression risk to
the listunspent coverage gate; honest coverage labeling; atomic writes; unit
tests covering parse/sort/tiebreak/cache/coverage-contract/synthetic-chain
builds. Answer with a single JSON object {codex_grade, reasons,
blocking_issues, polish_suggestions} — last line, nothing after.
