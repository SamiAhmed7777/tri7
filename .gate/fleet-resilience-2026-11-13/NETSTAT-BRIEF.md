# Laptop tor fix — netstat fallback for broken Get-NetTCPConnection

## Round-1 verdict (D, urn:ump:ea5ht7mbjhmxalacs7zrqopmdvglzrwqzllp33x6ag2dh53ffltq) — resolutions

- BLOCKER "findstr pipeline cannot match normal netstat rows" — ACCEPTED by
  removal: the fallback no longer shells out to findstr at all. It runs plain
  `netstat -ano` ONCE and parses the rows IN RUST
  (`netstat_listen_pid`, pure fn, unit-tested): 5-column TCP rows only,
  State==LISTENING, exact port match on the LOCAL address (":58033" must not
  match ":5803" or ":58033x"), 127.0.0.1 binds ONLY — [::1] rows are
  explicitly rejected (r3: every SOCKS probe targets 127.0.0.1, so a
  different-process [::1] listener on the same port would misattribute the
  handshake). Header lines, UDP rows (4 columns), non-LISTENING rows and
  malformed rows are rejected. Test
  `netstat_listen_pid_parses_rows_and_rejects_lookalikes`.
- BLOCKER "pid-from-netstat + path-from-Get-Process reintroduces the
  turnover race" — ACCEPTED: the listener pid is read TWICE (before and
  after the Get-Process path query) and a mismatch FAILS CLOSED (returns
  None). Documented in the fn comment.
- BLOCKER "no tests for the Windows-only path" — ACCEPTED for the decision
  logic: the row parser is pure and tested on every host (exact match,
  IPv4/IPv6, lookalike ports, UDP rows, malformed rows, empty output). The
  OS invocations (netstat/Get-Process) remain cfg(windows) and cannot run on
  Linux CI; their orchestration is a linear two-step with a fail-closed
  guard.
- POLISH accepted: dropped the "universally present" claim about netstat
  (rewritten to "present on every supported Windows baseline this wallet
  ships to", in both the brief and the source doc); production comments now
  document the resolver contract and the turnover/race guarantees instead of
  incident narrative; gates.log is labeled as what it is (see below).
- Round-2 (C) resolutions: (a) the parser now CONTINUES past a malformed pid
  in a matching row instead of aborting the scan, with a
  malformed-then-valid test case; (b) the pid-reuse overclaim is GONE — the
  doc now states exactly what the double-read guarantees (listener-turnover
  protection) and names the residual risk it does not cover (same-pid
  rebind inside the read window; residual risk accepted, surfaced at the
  next health probe); (c) gates.log is split so the file named as the
  record contains exactly ONE gate run for the final tree.

# Laptop tor fix — netstat fallback for broken Get-NetTCPConnection (context)

Live laptop evidence (2026-11-13 ~18:45-19:00 UTC, node.log + manual probes):
the laptop's `Get-NetTCPConnection` cannot see loopback LISTEN sockets (it
returned nothing for the live orphan tor's 127.0.0.1:58033 listener, and for
the freshly-spawned child's listener), so process-bound identity resolution
returned None → Windows fail-closed (r4) correctly refused to attribute the
handshake → tor was killed and latched OFF on every attempt
("answered by a NON-child process"). The failsafe was RIGHT — the laptop's
OS query layer is the broken part.

Fix: `listener_of_socks_port_os` tries the ONE-query cmdlet first and, when
it yields nothing usable (missing cmdlet / no rows / empty path), falls back
to parsing `netstat -ano` rows (present on every supported Windows baseline
this wallet ships to) + `Get-Process -Id <pid>` for the path. Fail-closed semantics are
unchanged: an unresolvable listener AFTER both resolvers is still refused.

Verification (SUPERSEDED — see the Round-1 verdict resolutions above; the
final verification is 775 passed / 0 failed / 1 ignored with EXIT_* = 0 in
gates.log): 774 passed / 0 failed / 1 ignored; GUI check EXIT_GUI=0;
windows-gnu release --bins EXIT_WIN=0. The laptop follows the
fleet via its Pi clearnet peer meanwhile (getpeerinfo: clearnet 1, tip
2211193 claimed).
FILES: src/gui/node_supervisor.rs

## Verification (final tree)

`cargo test --lib`: 775 passed / 0 failed / 1 ignored (EXIT_TEST=0);
`cargo check --bin tri7_wallet_gui --features gui` (private target dir):
EXIT_GUI=0; `cargo build --release --target x86_64-pc-windows-gnu --bins`:
EXIT_WIN=0. All in gates.log (tail = the final run for THIS tree).

FILES: src/gui/node_supervisor.rs
