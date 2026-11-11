# GUI standalone wallet: the embedded node now starts (complete conf + live peer)

Operator requirement: the GUI wallet must function as its own wallet — no
extra preparation, no daemon expertise. The blocker was the embedded node
supervisor: ensure_conf wrote "daemon=0\nrpc=1", a conf the daemon REFUSES
(missing rpcuser/rpcpassword), so the spawned node exited instantly and the
supervisor hit its restart limit — the GUI could never run standalone.

Fixes (src/gui/node_supervisor.rs):
1. ensure_conf(conf, rpc_port) generates a COMPLETE runnable conf:
   server=1, rpcport=<embedded port>, rpcbind/rpcallowip 127.0.0.1,
   rpcuser=tri7, rpcpassword=<32 hex random via getrandom>, staking=0
   (the user flips staking in the UI; the staking RPC persists it).
2. DEFAULT_PEER 194.233.88.206:24113 (dead for months) ->
   194.163.161.162:24113 (the samihost public listener, live as of today —
   the fresh-install bootstrap peer; Tor users add onions in Options as before).
3. Test conf_written_once updated to assert the real requirements
   (rpcuser/rpcpassword/rpcport/server present; idempotency preserved).

cargo test --release --lib: 455 passed, 0 failed (supervisor tests green).
Evidence bundle: this diff; the conf-content assertions in the updated test.
