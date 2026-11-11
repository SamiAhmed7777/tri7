# Brief: workqueue item 6 GUI-BUGS — Transactions page confirmation tooltip + per-tx amount accuracy

## Artifact
One commit on tri7 (repo /root/tri7, master) fixing the Transactions page of
the egui GUI wallet so every rendered row matches the legacy Qt client
(`/root/tridock-build/triangles_v5/src/qt/transactiontablemodel.cpp` +
`transactionrecord.cpp`), plus a headless `--history-probe` extension that
prints the rendered status + amount verbatim (the item's acceptance bar).

## Ground truth (measured live on sami-pc, the deployed wallet host)
- Wallet E:\Coins\TRI\wallet.dat, history.json 29,388 rows (scanned to h2205310), tip h2207233.
- Cross-check of 33 sampled history rows against the daemon's independent
  `gettransaction` (block-store read + wallet ownership):
  - receive rows: hist == daemon, OK (e.g. bf8bde89 33333333 == 33333333)
  - send rows (negative): hist == daemon, OK (all 8 sampled)
  - coinstake rows: MISMATCH BY SEMANTICS — history stores the RAW OUTPUT
    value (principal+reward, e.g. 33,372,923) while the daemon and the legacy
    Qt client both show the NET (reward only, +39,590). Split-stake example
    a5466f135be1: outputs 14,260,000 + 14,260,241, our inputs -28,516,183,
    net +4,058; the page rendered BOTH outputs at full value (two rows).
- Legacy source verdict (quoted in the diff context):
  - transactionrecord.cpp decomposeTransaction: for IsCoinStake, ONE row per
    tx (`hashPrev == hash → continue`), `sub.credit = nNet > 0 ? nNet :
    GetValueOut() - nDebit` where nNet = GetCredit(true) - GetDebit().
  - transactiontablemodel.cpp formatTxAmount: `format(unit, credit + debit)`
    with no unconditional "+" (negative renders "-x").
  - formatTooltip(): status + "\n" + type (+ " " + labeled address only for
    address-typed rows).

## What the change does (all in change.diff)
1. src/history.rs: HistoryRec gains `stake_net: i64` (serde default 0 — old
   files load unchanged). The generator fills it for every coinstake row of a
   tx: `recv_total - spent_total` (legacy nNet; both totals are already
   computed for the send-row branch). Per-output rows and the ownership map
   are untouched. Test `coinstake_rows_carry_the_legacy_net_reward` pins
   single-output (+39,590) and split-output (net +80 on both rows) stakes.
2. src/gui/state.rs: UtxoRec gains `stake_net` (live/derived rows carry 0).
3. src/gui/app_state.rs:
   - merged_transaction_rows collapses coinstake history rows to ONE row per
     txid (the legacy hashPrev guard) and carries stake_net; live coinstake
     coins of a collapsed txid are dropped (txid-level dedupe) so an unspent
     multi-output stake can never show twice.
   - NEW AppState::tx_row_view(&UtxoRec, tip) -> TxRowView: the ONE
     computation of status sentence, amount string, tooltip, glyph key, type
     label, negative flag — shared by the page, the CSV export and the probe
     so they cannot drift. Amount: stake_net (when non-zero) else raw value;
     sends render "-x" (fixes the old unconditional "+" which produced
     "+-5.000100 TRI"); received keeps the tri7 "+".
   - Tooltip = legacy formatTooltip: status \n type, plus the labeled address
     for RecvWithAddress/RecvFromOther/SendToAddress only.
4. src/gui/app.rs: the page row loop and the details drawer both consume
   tx_row_view. Amount color: outgoing rows take the red family (th::ERROR)
   per the pinned received-green/outgoing-red palette rule; the old code
   colored sends green when confirmed. The details drawer previously indexed
   derived.utxos (a DIFFERENT list than the merged rows the table rendered —
   clicking a row could show an unrelated tx's details) and hard-bailed on
   fresh installs with no local snapshot; it now indexes the same merged rows
   and shows the legacy status sentence + correct signed amount.
5. src/legacy_rows.rs: row_from_history (CSV export path) renders
   stake_net for stake rows, so the legacy-column CSV matches the page.
6. src/bin/tri7_wallet_gui.rs: --history-probe now iterates the MERGED rows
   through tx_row_view and prints `row h<height> depth <d> -> <status> | amt
   <amount> | txid <id> | tooltip "<tooltip>"` (first 5 rows + first 3 stake
   + first 3 send rows) — the item's "probe line must print the rendered
   status + amount".
7. src/rpc.rs: listwallettx rows expose stake_net (daemon route).
8. .github/workflows/ci.yml: windows artifact now uploads txhistory.exe too,
   so the one-shot history rescan (needed to populate stake_net on the
   operator's existing 29,388-row file) is CI-delivered, not hand-scp'd.

## Verification (already run against these bytes)
- cargo test --release --lib: 473 passed, 0 failed (+2 new GUI tests:
  tx_row_view_renders_stake_net_send_sign_and_legacy_tooltip,
  merged_rows_collapse_coinstake_txs_and_dedupe_live_coins).
- cargo test --release --bin follow_tip: 43 passed, 0 failed.
- cargo build --release --target x86_64-pc-windows-gnu --bins: zero errors,
  all bins (gui 5,962,188 B, follow_tip 7,327,905 B, txhistory 5,097,859 B).
- serde default on both new fields means the deployed 8.79MB history.json
  loads unchanged until the rescan; stake_net==0 rows fall back to the raw
  value (pinned by test), so the fix never renders a wrong smaller amount on
  old data — it renders exactly what it rendered before until data exists.

## Honest limitations (disclosed, not blockers)
- Stakes minted ABOVE history.json's scanned height (the live window) have no
  stake_net yet and render the raw output value until the next --full rescan;
  the rescan runs on sami-pc right after CI delivers txhistory.exe (same
  deploy step as the GUI), so the operator's actual file will carry the net
  for all 29,388+ rows. Future stakes minted after a rescan have the same
  window until the next rescan — GUI-side auto-rescan is future work, listed
  in the workqueue progress line.
- The probe samples rows (5 + 3 stake + 3 send) rather than printing all
  ~29.6k; the page itself renders every row.

## Rubric
Grade the code change (correctness vs the legacy semantics quoted above,
dedupe/collapse logic, test coverage, deploy wiring). Deployment proofs for
the graded bytes (probe transcript showing amt/tooltip, rescan readback) are
captured AFTER CI delivers the artifact and are not in this bundle; the
change is judged as a code artifact with the live ground-truth measurements
above as its problem statement.

Answer with a single JSON object {codex_grade, reasons, blocking_issues,
polish_suggestions} as the last line, nothing after.
