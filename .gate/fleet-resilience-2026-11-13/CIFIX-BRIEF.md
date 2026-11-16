# CI-fix — Windows attribution vs test-stub architecture

fb6e875's Windows CI run failed 4 tor-supervisor tests. Root cause: the test
stub's SOCKS listener lives in the TEST PROCESS while the spawned "tor" child
(cmd/ping) never binds — so on Windows, where the production-grade listener
resolution runs, the new fail-closed attribution CORRECTLY refused to credit
the handshake to the child, and the recovery branch correctly refused to kill
a "wallet-owned" listener (the listener is the test runner, not tor). The
product behavior is right; the TEST ARCHITECTURE mis-modeled production.

Fix (test-only; no production code change in this delta): the stub now
records each spawned child's pid into a shared registry and each test injects
a listener resolver that reports (that pid, bundled exe path) — the exact
production identity shape (the spawned tor.exe owns the pinned port).
`stub_tor_spawn(req, registry)` + `child_identity_pair(bundled_path)` helper.
a3 (adoption) keeps its static foreign/wallet-owned resolvers; a5/a7 keep
their adversarial foreign identities.

Verification: 771 passed / 0 failed / 1 ignored locally (gates.log, EXIT_*
all 0); the four previously-failing tests now pass on Linux with the same
attribution semantics Windows enforces.

FILES: src/gui/node_supervisor.rs
