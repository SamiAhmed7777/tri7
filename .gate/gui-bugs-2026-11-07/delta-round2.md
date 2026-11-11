# Delta note: round 1 -> round 2 (all three polish items taken)

Round 1 verdict: GRADE=B, zero blockers, urn:ump:r7mfhls62mfb6dfru6d4d7t7dxqazpus2qvv4tccwtdmcft6ayra.
Polish list was: (1) distinguish stake_net==0-unknown from a genuinely zero
net, (2) fix misindented inserted `stake_net: 0,` lines in src/gui/tests.rs,
(3) consider a --history-probe flag to dump all rows for future audits.

## (1) Option<i64> — done
`stake_net` is now `Option<i64>` on BOTH HistoryRec (src/history.rs, serde
default → None on old files) and gui::state::UtxoRec. A known net is Some(n)
(n real stakes are > 0, and Some(0) would render "+0.000000 TRI" honestly if
it ever occurred); None = unknown → render falls back to the raw output
value, exactly as before. Grep-proofs against these bytes:
- src/history.rs:58 `pub stake_net: Option<i64>` (doc says None semantics)
- generator: `r.stake_net = Some(net);` (src/history.rs, fill loop)
- render: `let display_sats = u.stake_net.unwrap_or(u.value_sats);` in
  AppState::tx_row_view and `u.stake_net.unwrap_or(u.value_sats)` in
  legacy_rows::row_from_history — no `!= 0` sentinel remains
- dedupe filter: `r.stake_net.is_some() || r.prev_kind == "coinstake"`
- daemon route parse: `stake_net: r.get("stake_net").and_then(|x| x.as_i64())`
  (Option flows through; json null → None)
- fixtures/tests updated: `stake_net: None`, `Some(39_590)`, `Some(80)`,
  `Some(4_058)` assertions.

## (2) tests.rs indentation — done
All inserted lines now match their literal's surrounding indent (fixed
mechanically to the previous line's indentation; e.g. the mk-closure fixture
at the `legacy_kind: String::new(),` level is now 8 spaces, matching
`prev_kind:`/`prev_kind` around it).

## (3) --history-probe-full — done
Same probe branch: after the sampled section and the status line, passing
`--history-probe-full` dumps EVERY merged row through the identical
tx_row_view call with the same `row h<height> depth <d> -> <status> | amt
<amount> | txid <id> | tooltip "<tooltip>"` line shape.

## Suites re-run on these bytes
- cargo test --release --lib: 473 passed, 0 failed, 1 ignored
- cargo test --release --bin follow_tip: 43 passed, 0 failed
- cargo build --release --target x86_64-pc-windows-gnu --bins: rebuild after
  the refactor — grep -cE "^error" over the full build output returned `0`
  (transcript above: `0` / `windows build rc done`), bins relinked
  (tri7_wallet_gui.exe 5,962,188 B, follow_tip.exe 7,327,905 B,
  txhistory.exe 5,097,859 B)

The refreshed full diff is change-r2.diff (replaces change.diff, which was
the round-1 bytes).
Answer with a single JSON object {codex_grade, reasons, blocking_issues,
polish_suggestions} as the last line, nothing after.
