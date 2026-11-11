# Judge brief: tri7 lane-multisig `createmultisig` — ROUND 3 (blockers addressed)

## Round-2 blockers and their resolution
1. **"Two function-local ENV_LOCKs mutate the process-global TRI7_WALLET_PATH
   concurrently"** — FIXED, both halves:
   - the new createmultisig test no longer mutates the env AT ALL: the handler
     is pure (never reads TRI7_WALLET_PATH — the env read at the old line 3215
     belongs to the `createrawtransaction` arm, verified by arm-boundary check),
     and the wallet-untouched assertions read the wallet file DIRECTLY via the
     local `path` variable; the env set/remove lines are gone;
   - the remaining env-mutating test (`multisig_address_verbs_store_redeem_
     scripts_in_the_wallet_file`) now takes a SINGLE module-level
     `static ENV_LOCK` at the top of the tests module (see rpc-full.src
     "shared ENV_LOCK" section), so any future env-mutating test shares one
     lock. No function-local env locks remain (grep: 1 occurrence = the shared
     static). Full suite rerun under default PARALLEL test threads: green.
2. **"535 claim not reproducible; .gate/multisig-create-2026-11-09 says 521"** —
   the judge was RIGHT, and the root cause is disclosed: round 2's brief quoted
   a parent run that used a SHARED CARGO_TARGET_DIR across sibling worktrees of
   the same package; that run served a contaminated/stale test binary (536
   total). Authoritative evidence in THIS bundle is from an ISOLATED target dir:
   - worktree: /root/tri7-lanes/lane-multisig (branch lane/multisig-create,
     base 9723e33 = current origin/master);
   - master baseline, own target dir (/root/tri7/target): `cargo test --lib
     -- --list` = **531 tests**;
   - lane worktree, private target dir (/root/tri7-lanes/target-ms):
     **532 tests = 531 passed + 1 ignored, 0 failed** (tests-lib-r3.txt);
   - delta = +1 test fn (~+270 lines in src/rpc.rs, only file changed);
   - the `.gate/multisig-create-2026-11-09/` bundle (521 tests) is workqueue
     item 12's round from an earlier date — different scope, unrelated to this
     lane's numbers.

## Polish suggestions also taken
- Negative routing assertions added: the test now asserts `createmultisig` is
  present in NATIVE_METHODS AND absent from OPS_METHODS and
  PROXY_FALLBACK_METHODS (see rpc-full.src test header).
- TRI7_WALLET_PATH no longer touched by the create-only test (above).

## What the artifact is (unchanged from round 2)
`createmultisig nrequired '["pubkey",...]'` -> `{"address","redeemScript"}` on
tri7. Legacy v5 has NO such verb (vRPCCommands, trianglesrpc.cpp:244); the
create-only shape anchors validation order + error strings byte-for-byte to
legacy `addmultisigaddress` (rpcwallet.cpp:836-889): full hex pubkeys only,
duplicates ACCEPTED (legacy SetMultisig/script.cpp has no dup check),
not-enough-keys checked before key format (17-of-2 -> not-enough), verbatim
strings incl. leading space `" Invalid public key: "`, >16 keys -> clean -8
refusal (disclosed deviation: legacy EncodeOP_N assert-aborts). Difference from
shipped addmultisigaddress (item 12): create-only — no wallet/key-store write.
Canonical vector through REAL dispatch: 2-of-3 -> CJU8gKtn4aNBBovbywqPqZo6mjA1B1pLD1,
exact 105-byte redeemScript. Routing: NATIVE_METHODS only (pre-dispatch gate),
handler in native_call, deliberately not in OPS/PROXY lists (now asserted).

## Files in this bundle
- brief.md — this file
- multisig.diff — complete diff vs base 9723e33 (301 lines, src/rpc.rs only)
- rpc-full.src — full current bytes: NATIVE_METHODS block, handler arm region,
  shared-lock section, complete new test fn, and the evidence block
- tests-lib-r3.txt — tail of the isolated-target full-suite run
