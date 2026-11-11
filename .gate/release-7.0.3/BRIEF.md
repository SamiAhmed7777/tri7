# tri7 release bump: 7.0.2 -> 7.0.3 (the release train carries seven graded lanes merged into master)

VERSION: Cargo.toml 7.0.2 -> 7.0.3; BUILDING.md version line updated.
UPDATER FIXTURES (the constant-bump lesson applied across the WHOLE test tree): src/updater.rs fixtures modeled "the newer release" as 7.0.3 -> bumped to 7.0.4 (24 sites), further-new case 7.0.3 -> 7.0.5; src/gui/tests.rs fixture (sample release JSON + banner text) 7.0.3 -> 7.0.4 (5 sites). The initial bump missed gui/tests.rs (updater_banner_line_pins_the_offered_jump FAILED in gate v1) — caught by the gate chain, swept, fixed, and the full chain re-run green (gate v2).

WHAT SHIPS IN 7.0.3 (all merged judge-graded): balance-authority A (frozen-total fix: wallet_total source hierarchy, live-payload self-consistency fail-closed, GUI atomic getwalletinfo card + coverage labels, allow-list fix) — Sami's 2026-11-11 incident; stake-kernel B (solver/validator one shared kernel-evidence path + phantom fail-loud staking_impaired — the laptop mint-freeze); tor-bundle B (bundled real tor 16.0.1 SHA256-pinned, supervisor-managed child, SOCKS5-greeting probe — zero-install onion peering per Sami's directive); rpc-ops-wiring B (addnode/disconnectnode/getseedlist/getnetworkstability/gettxout/getrawchangeaddress + peer-store persistence/serialization); event-push B (SSE GET /events); i2p-lifetime B (keepalive + loud removal reasons); wedge/conf-migration B (fresh-install wedge + legacy-conf auto-migration — the sami-pc freeze); outbox-decrypt B (sender-visible sent messages); plus the sibling addrindex-build oracle fixes.

GATES (raw in bump-gates.log + bump-gates-2.log, final tree): full lib 714 passed / 0 failed / 1 ignored; release lib 714/0/1; GUI featureful check Finished 0 errors; windows-gnu all-bins release Finished. Updater subset 30/0.

RELEASE MECHANICS FOLLOW THE COMMIT (parent-owned): tag v7.0.3 -> CI builds the version-stamped setup.exe/gui-portable/SHA256SUMS -> gh release with the exact SHA256SUMS rows in the notes (digest pinning binds per the extract_digest_from_body shape).

## Round 2 (judge blockers resolved, urn:ump:k3hsapyjcg3dksmc3o7ujrmypyq7snugelattqp4wvh626tc3taq)
1. CONSOLIDATED FINAL-TREE GATE LOG: bump-gates-final.log runs ALL stages on the corrected tree in one file — updater subset 30/0 (the previously unsupported claim), full lib 714/0/1, release lib 714/0/1, GUI check Finished, windows-gnu all-bins Finished. The earlier bump-gates.log (the FAILED pre-sweep chain) and bump-gates-2.log (partial rerun) are retained for history; the FINAL log is authoritative.
2. Polish taken: the stale updater test comment now reads `7.0.4 > 7.0.3 (current)`; the extract_digest doc example is explicitly marked ILLUSTRATIVE so routine bumps stop leaving apparently-stale versions.
