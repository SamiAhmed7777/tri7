# tri7 lane: createMultisig verb (legacy parity)

Repo: THIS worktree (branch lane/multisig-create). tri7 = Rust rewrite of the Triangles (TRI) PoS chain client.
All cargo commands MUST run as: CARGO_TARGET_DIR=/root/tri7-lanes/target cargo ...  (shared cache, file-locked)

## HARD RULES
- Do NOT run any git command that changes state (no commit/branch/checkout/stash/add/reset). The parent owns git.
- Touch ONLY the files your scope needs (+ their tests). No drive-by refactors, no formatting sweeps.
- Do not weaken or delete existing tests. Add tests for what you ship.
- Never print private keys/WIFs into your output.

## RPC ROUTING RULES (tri7 — from src/rpc.rs)
- The NATIVE_METHODS const list gates EVERY verb: a handler without membership answers -32601 before dispatch.
- Adding a verb = ALL of: (1) handler arm (this verb is a pure computation — likely native_call in src/rpc.rs), (2) NATIVE_METHODS membership ALWAYS.
- NATIVE_METHODS and OPS_METHODS contain identical anchor lines — after editing, grep your verb and assert it is in EVERY list it needs. cargo check does NOT prove routing.

## LEGACY SOURCE OF TRUTH
/srv/krystie-work/triangles/triangles_v5-1/src/
- verb table: grep -n vRPCCommands trianglesrpc.cpp ; find the createmultisig handler and read its exact parameter parsing, validation order, and error strings (e.g. invalid nrequired, >N keys, invalid key type).

## tri7 EXISTING CODE — READ FIRST
- src/multisig.rs: workqueue item 12 shipped addmultisigaddress + addredeemscript with legacy parity, live-proven against canonical vector CJU8gKtn4aNBBovbywqPqZo6mjA1B1pLD1. REUSE the same script builder and validation.
- createMultisig differs from addmultisigaddress ONLY in that it is create-only: takes FULL pubkeys (and per legacy, accepts compressed hex pubkeys), does NOT touch the wallet/key store, returns {address, redeemScript}.

## SCOPE — implement exactly this verb
`createmultisig nrequired '["key1","key2",...]'` → `{"address":"...","redeemScript":"..."}` with legacy-identical errors for: bad nrequired, too few/many keys, duplicate keys (whatever legacy does), invalid key format.

## VERIFY BEFORE REPORTING DONE
1. CARGO_TARGET_DIR=/root/tri7-lanes/target cargo check — green (paste tail)
2. CARGO_TARGET_DIR=/root/tri7-lanes/target cargo test --lib multisig — green (paste tail). Add: the canonical vector as a test; nrequired validation errors; a routing assertion (verb in NATIVE_METHODS).
3. grep proof of routing lists.

## REPORT (print at the end, plain text)
- Changed-file inventory (path + one line each)
- Test output tail
- Anything NOT finished, explicitly.
