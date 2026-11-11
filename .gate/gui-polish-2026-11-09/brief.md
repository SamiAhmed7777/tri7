# Judge brief: tri7 workqueue item 15 GUI-POLISH (tray repaint fix, start-on-login, staking reserve)

## What this artifact is
One code commit on tri7 (Rust rewrite of the 2013 Triangles C++ chain client;
repo /root/tri7, master). The workqueue item: "GUI-POLISH: tray icon,
start-on-login, staking reserve field." The tray icon itself ALREADY shipped
(tray.rs + hide-to-tray wiring); this tick adds the missing repaint fix,
makes the two stubbed Options rows real, and wires a v5-parity staking
reserve through daemon + RPC + GUI.

## Measured delta (from `git diff`, 11 files, +754/-23)
- src/gui/app.rs: +adopted sibling-lane 9-line hunk (hidden-window repaint so
  tray clicks work while hidden — see TRAP comment); Options Main tab Reserve
  row is a real input; "Start Triangles on login" row is a real checkbox on
  supported platforms (honest disabled stub elsewhere); two new notice
  renderings; Staking page Reserve metric now shows the node's live value;
  run() takes start_minimized; first-frame Minimized viewport command.
- src/gui/autostart.rs: NEW module. Windows = HKCU
  Software\Microsoft\Windows\CurrentVersion\Run value `Triangles` =
  "<exe>" -min (winreg; DISCLOSED deviation from v5's COM-authored .lnk —
  same user-scope effect). Linux = v5 XDG autostart file shape exactly
  (triangles.desktop, Hidden=true = disabled, remove on disable). macOS = v5
  LaunchAgents plist (RunAtLoad + <string>-min</string>). 5 unit tests
  (program_args, desktop-file shape + Hidden=true semantics, plist markers,
  set/remove roundtrip in a temp dir, supported()).
- src/gui/app_state.rs: OptionsState.start_on_login (serde default, mirrors
  OS at load); reserve push/load functions with the fee's save-first/
  push-best-effort contract; save_options_action validates the reserve
  BEFORE persisting, applies the OS registration only when the checkbox
  differs from the machine's actual state; selfcheck report gains gui_polish
  {tray_supported, autostart_supported, autostart_enabled (REAL OS read),
  reservebalance (live RPC probe)} — read-only, never writes.
- src/bin/tri7_wallet_gui.rs: -min/--min flag (start minimized, v5 -min
  parity), --min added to the unknown-flag allowlist, --autostart-check
  headless roundtrip (enable → read → disable → read, leaves the OS entry
  clean), usage text updated, app::run(cfg, start_minimized).
- src/stake.rs: RESERVE_BALANCE const → runtime AtomicI64 global
  (set_reserve_balance/reserve_balance, default 0), read at the same three
  CreateCoinStake parity sites (wallet.cpp:2246/2074/2366 analogues). New
  test: reserve gating (solve at 0; balance<=reserve aborts; credit ceiling
  aborts), serialized with every solver test behind RESERVE_GLOBAL_LOCK so
  the parallel test suite can never interleave a nonzero reserve.
- src/rpc.rs: legacy_parse_money (v5 ParseMoney parity: leading '-' rejected,
  frac scaled by position so ".5"==0.5, "1." accepted, >6 decimals rejected,
  i64-safe overflow; DISCLOSED deviation: junk digits rejected where v5's
  atoi64 silently truncated). reservebalance added to NATIVE_METHODS +
  OPS_METHODS. Tests: ParseMoney vectors + routing.
- src/bin/follow_tip.rs: reservebalance verb (bare = read; amount = set,
  global applied live, conf line persisted); getstakinginfo gains the
  "reservebalance" tri7 extension; conf `reservebalance=` parsed at boot
  (last line wins, CLI --reservebalance overrides, INVALID amount refuses to
  boot like v5 init); the set path is factored into reservebalance_apply()
  and tested: live global, conf replace-not-append, byte-exact v5
  init.cpp:907 error string "Invalid amount for -reservebalance=<amount>"
  (the legacy message never substituted <amount> — reproduced literally).
- HANDOFF.md: the dated section documenting all of the above including the
  v5 QSettings-doesn't-survive-restart quirk this design deliberately fixes.
- Cargo.toml/Cargo.lock: winreg = "0.55" under [target.'cfg(windows)'.dependencies].

## Evidence attached (measured, on the live tree)
- evidence-lib-tests.txt: cargo test --release --lib → 530 passed, 0 failed
- evidence-followtip-tests.txt: cargo test --release --bin follow_tip → 64 passed, 0 failed
- evidence-gui-tests.txt: cargo test --release --features gui --lib → 539 passed, 0 failed
- evidence-win-gnu-build.txt: windows-gnu release build of the three bins
  (gui feature) rc=0 with sha256 of each exe (winreg compiles for gnu)
- diff.patch: the complete diff being judged

## What to check (rubric)
1. Correctness: do the three surfaces do what the item says? Is the v5
   parity honest (strings/semantics) where claimed, and are deviations
   DISCLOSED rather than silent?
2. The reserve gating test must not be able to flake under cargo's parallel
   test threads (check the lock pattern covers every solver test).
3. The GUI must never write the OS autostart registration from a unit test
   (check the test leaves the checkbox at the machine's actual state).
4. Selfcheck must stay read-only (no tray creation, no registry write).
5. Anything in the diff that could break an EXISTING behavior (options file
   compat — serde default on the new field; existing verbs untouched).

## Answer shape
A single JSON object: {"codex_grade": "A"|"B"|"C"|"D"|"F", "reasons": [...],
"blocking_issues": [...], "polish_suggestions": [...]}. Last line, nothing
after.
