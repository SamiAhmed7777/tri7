# Brief — singleexe console-window elimination (Sami 2026-11-12)

## Purpose
Sami directive: the wallet is ONE window — "No other window should open
outside of the GUI when it is running." The Windows wallet exe was linked as
a console-subsystem binary, so every launch opened a console window next to
the GUI. Fix: link the Windows RELEASE build as a GUI-subsystem binary
(windows_subsystem="windows"), gated so debug builds and non-Windows targets
keep stdout for the headless --selfcheck path.

## Definitions (term -> verifying command)
- "console-subsystem" -> PE optional header Subsystem field == 3; parsed from
  bytes at e_lfanew(0x3C) + 24 + 68 (u16 LE).
- "GUI-subsystem" -> same field == 2.
- "the deployed PC exe" -> E:\Coins\TRI\bin\tri7_wallet_gui.exe on sami-pc,
  15,736,361 bytes, mtime 2026-11-11 20:23:45, built from base 308d9ad
  (committed 2026-11-11 20:11:57 -0800).
- "full lib suite" -> `cd /root/tri7 && cargo test --lib`.
- "GUI featureful check" -> `CARGO_TARGET_DIR=/root/tri7/target-gui-check
  cargo check --bin tri7_wallet_gui --features gui`.
- "windows release build" -> `CARGO_TARGET_DIR=/root/tri7/target-win cargo
  build --release --target x86_64-pc-windows-gnu --features gui --bin
  tri7_wallet_gui`.

## Problem, measured on the live deployment
- Deployed PC exe PE read over ssh (PowerShell BitConverter, 2026-11-12):
  `PE offset 128 subsystem 3 (2=GUI 3=console)` — console-subsystem. This is
  the second window Sami forbids; it appeared on every wallet launch.
- Base 308d9ad contained NO windows_subsystem attribute anywhere in src/
  (`grep -rn windows_subsystem src/` -> no hits before this change).

## Change (2 files, +48/-0; see singleexe-console.diff)
1. src/bin/tri7_wallet_gui.rs — inner attribute at the bin root:
   `#![cfg_attr(all(target_os = "windows", not(debug_assertions)),
   windows_subsystem = "windows")]` with a comment citing the directive and
   the selfcheck/stdout tradeoff (a GUI-subsystem exe prints nothing; use
   --report PATH for headless runs on Windows release builds; Linux/DNS2
   selfcheck unaffected).
2. src/gui/tests.rs — content test
   `windows_release_wallet_launches_with_no_console_window`: pins the
   attribute presence, that it rides cfg_attr (never bare), and that the
   gate names both target_os=windows and not(debug_assertions).

## Evidence (bundle files, each complete)
- tests-singleexe-console.txt — full raw output (both streams) of the
  definitions' commands at worktree HEAD: "test result: ok. 761 passed;
  0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 83.14s" and
  both `Finished` lines (dev lib-test profile + GUI featureful check).
- windows-build-subsystem.txt — windows release build log tail (exit 0,
  "Finished `release` profile", 6m 24s) + python PE parse of the built
  artifact: `subsystem 2 (2=GUI 3=console)` + sha256.
- artifact-sha256.txt — 663579a425ca3e8f16a22acd9ef1c3162bfca08df982750907d3ae6450e9bbe6  tri7_wallet_gui.exe
- tri7_wallet_gui.exe — the built artifact itself (15,736,121 bytes).
- singleexe-console.diff — git diff of the two files vs base 308d9ad.

## Files
brief.md
singleexe-console.diff
tests-singleexe-console.txt
windows-build-subsystem.txt
artifact-sha256.txt
tri7_wallet_gui.exe

## Scope note
Deployment (exe swap + daemon retirement + store migration on sami-pc and
sami-laptop) happens AFTER this commit and is graded separately with
post-cutover live evidence. No consensus, RPC, or node-engine code changed;
the only runtime delta on Windows release builds is the missing console.
