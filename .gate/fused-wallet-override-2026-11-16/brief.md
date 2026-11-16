# Gate: fused wallet — explicit --wallet beats persisted session
## Defect (live, Sami's PC, 2026-11-16 13:08 PST)
`tri7_wallet_gui.exe --wallet E:\Coins\TRI\wallet.dat` launched fused; the first-run flow's
match arm checked `remembered` (profile/last_wallet.txt = the OLD Roaming wallet) FIRST and
overwrote cfg.wallet_path. The app opened the WRONG wallet (Roaming, 0 funded) and its fused
node replayed the chain from the wrong key set's base for days-of-walk. Sami's standing rule:
"it should actually use the existing wallets in the locations I give you".
## Fix
- UiConfig.wallet_path_explicit: bool (CLI-flag-only, mirrors state_dir_explicit pattern).
- tri7_wallet_gui.rs sets it when --wallet parses.
- resolve_first_run_target(): precedence helper — explicit+exists->Target; explicit+missing->Welcome
  (never silently open another wallet); no-flag->remembered (unchanged); else default/welcome.
- app.rs first-run flow uses the helper; remembered path only mutates cfg when no flag was passed.
## Evidence
- Reproduced: cmdline of pid 81572 carried `--wallet E:\Coins\TRI\wallet.dat` while the app
  recorded last_wallet.txt = C:\Users\hello\AppData\Roaming\Triangles\wallet.dat and booted the
  node at Roaming\Triangles\node\ (fresh tipstate/i2p netDb, TIP ADVANCE h17679x lines).
- Correct path proven available: E:\Coins\TRI\node\state (the app's OWN folder for the E:\ wallet)
  holds its Nov-13 fused-run state (blk 983,970,024 bytes, tip h2211241) — ~2,200 blocks behind
  today's fleet tip; opening the right wallet resumes it in minutes, no copies.
## Tests
- NEW cli_wallet_flag_beats_remembered_wallet (4 precedence cases incl. missing-explicit->Welcome).
- Full lib suite: 816 passed / 0 failed / 1 ignored (was 815 pre-patch).
## Rollback
git revert of this single commit; field is additive, no format/state migrations.
