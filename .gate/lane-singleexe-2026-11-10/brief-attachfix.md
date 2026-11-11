# Evidence — ROUND 21: attach-default fix (the weld was dead-on-arrival)

== THE DEFECT (live-caught on sami-laptop after the r20 deploy) ==
UiConfig.rpc_addr defaults to Some(127.0.0.1:19114) on ALL platforms;
the weld's first cut keyed attach mode off rpc_addr.is_some() -> EVERY
default user was attach-mode -> the supervisor never started the
in-process node. Observed: GUI polling 19114 forever (netstat SYN_SENT,
PID 23732), no listener, node.log showing only doomed outbound dials.

== THE FIX ==
1. UiConfig.attach_external: bool, default FALSE (both platform branches)
   — the node runs IN-PROCESS by default (the weld's whole point).
2. app.rs passes cfg.attach_external (not rpc_addr.is_some()).
3. NEW CLI flag --attach-external (fleet/advanced machines opt in).
4. Regression test default_gui_config_activates_the_weld_not_attach_mode
   pins UiConfig::default().attach_external == false.

== TESTS ==
cargo test --lib: 653 passed, 0 failed (652 + the new regression)
cargo check --bins: 0 errors; GUI-feature check: 0 errors

== FLEET IMPACT ==
sam-laptop: default (no flag) -> in-process node (retires the stuck
external daemon: its only peer 194.233.88.206:24113 has refused since
Nov 7 — 'FAIL(5): from-zero' in node.log); supervisor default peer
46.62.249.20:443 (Pi, P2P-over-443) verified reachable.
sami-pc: tri7gui task gains --attach-external (keeps the fleet's
task-managed tri7d daemon; the GUI attaches as before).

## ROUND 22 — the two r21 blockers closed
1. GUI-feature + all-bins checks RE-RUN and appended to gui-attachfix.log:
   both Finished with 0 errors (grep -c '^error' = 0 confirmed on the file).
2. The sami-pc tri7gui task now REALLY passes the flag: task XML rewritten
   (schtasks /create /xml /f), re-queried and VERIFIED:
   Arguments = "--wallet E:\Coins\TRI\wallet.dat --state-dir
   C:\Users\hello\tri7\state2 --attach-external"
