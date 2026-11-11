# Artifact: tri7 workqueue item 12 — MULTISIG-CREATE (addmultisigaddress / addredeemscript legacy-parity audit + fixes)

## What this change is
The two verbs ALREADY existed in the committed tree (commit 1a90eeb wired them
into NATIVE_METHODS/OPS_METHODS with handler arms + tests). This change is the
item-12 legacy-parity AUDIT against the real v5 source (/opt/triangles-v5/src/
rpcwallet.cpp:836 addmultisigaddress, :905 addredeemscript, with
trianglesrpc.cpp:151 ParseHexV and trianglesrpc.cpp:1321 the dispatcher's
`catch (std::exception&) → JSONRPCError(RPC_MISC_ERROR, e.what())`), plus the
fixes for every mismatch found. Files changed: src/rpc.rs (both arms), src/
wallet.rs (label writer version gate), src/multisig.rs (new known-answer test).

## Mismatches found and fixed (each verified against the v5 source, cited)
1. **addredeemscript returned `true`** — v5 returns `CTrianglesAddress(innerID)
   .ToString()` (rpcwallet.cpp:935), the bare P2SH address string. Now returns
   `p2sh_address(&script)`.
2. **addredeemscript hex error** was -5 "Invalid redeemScript" — v5 ParseHexV
   (trianglesrpc.cpp:151) throws `RPC_INVALID_PARAMETER` (-8) with the verbatim
   format `redeemScript must be hexadecimal string (not '<input>')`. Now exact.
3. **addmultisigaddress error codes**: v5 throws std::runtime_error for every
   semantic failure, which the dispatcher rethrows as RPC_MISC_ERROR = **-1**
   carrying the verbatim string. The arm previously used -8/-5/-4. All six
   semantic paths now return -1 with byte-exact v5 strings:
   - nrequired < 1 → "a multisignature address must require at least one key to redeem" (rpcwallet.cpp:852)
   - count short → "not enough keys supplied (got N keys, but need at least M to redeem)" (rpcwallet.cpp:855-858)
   - non-P2PKH address → "<ks> does not refer to a key" (rpcwallet.cpp:866)
   - address with no full pubkey held → "no full public key for address <ks>" (rpcwallet.cpp:870)
   - hex-but-invalid pubkey / not hex at all → " Invalid public key: <ks>" —
     the LEADING SPACE is in the v5 source (rpcwallet.cpp:876/:884), kept
     byte-for-byte
   - non-string keys element → -1 (legacy keys[i].get_str() throws → -1 via
     the same catch; json_spirit's own message text is a library artifact, so
     tri7 keeps its honest "Invalid public key")
4. **Check ORDER now mirrors v5's evaluation order**: nrequired<1 first
   (:851), then the count check (:855), then per-key parsing (:860-885), and
   only then the build. Previously the arm pre-checked `n<1 || n>16` FIRST,
   so e.g. nrequired=17 with 2 keys answered "between 1 and 16 keys" where
   v5 answers "not enough keys supplied (got 2 keys, but need at least 17…)".
5. **Negative nrequired**: v5 `params[0].get_int()` parses -1 and falls into
   the n<1 runtime_error. The arm used p_u64, which rejected negatives with a
   -8 type error. Now read as i64 → -1 "at least one key to redeem".
6. **[account] label rows**: v5 calls SetAddressBookName(innerID, strAccount)
   in BOTH verbs after AddCScript (rpcwallet.cpp:895/:931). The arm ignored
   account entirely. Now: non-empty account → wallet `set_label` on the P2SH
   address (tri7's legacy-layout `name` row, reader-verified). Empty/absent
   account writes nothing (v5 WriteName erases on empty names — net effect
   identical: no row).
7. **wallet.rs decode_address_payload** (single caller: set_label) rejected
   version byte 0x1c (SCRIPT_ADDRESS, the 'C' class) — set_label could never
   label a P2SH address. Now accepts both PUBKEY_ADDRESS (0x41) and
   SCRIPT_ADDRESS (0x1c); the `name` row layout stores the address STRING and
   is version-agnostic (encode_legacy_name_keyblob is unchanged and byte-
   identical to what the Qt client writes for a 34-char CScriptID address).
8. **16-key cap kept, repositioned + disclosed**: v5 has NO range check —
   SetMultisig/EncodeOP_N is `assert(n >= 0 && n <= 16)` (script.h), i.e.
   legacy ABORTS the process on 17 keys. tri7 refuses with -8 "a
   multisignature address must require between 1 and 16 keys" AFTER the
   legacy-ordered checks (so the count error still wins when both apply) —
   a deliberate, disclosed deviation: honest refusal beats a crash-class bug.
9. **Known-answer vectors pinned** (new test in src/multisig.rs): the
   canonical privkey=1,2,3 compressed pubkeys reproduce the reference's
   byte-exact 105-byte 2-of-3 redeem script
   (5221 0279be66… 2102c6047f… 2102f9308a… 53ae), P2SH
   CJU8gKtn4aNBBovbywqPqZo6mjA1B1pLD1, and the 1-of-3 P2SH
   Cc7Jwy788GFxRiW8Xza64Ra6C9Qb78maFS (from the shipped p2sh-multisig.md
   reference vectors, originally reproduced from a spec-only implementation).

## What was already correct and is untouched
- Both verbs return the BARE address string (v5 does NOT return an object —
  the object-returning shape is bitcoin-0.8, not this tree; v5:
  `return CTrianglesAddress(innerID).ToString();`).
- NATIVE_METHODS + OPS_METHODS membership (dispatch is by const list), the
  wallet-write path (put_redeem_script: AddCScript parity with re-read proof),
  keys-as-addresses-or-hex-pubkeys parsing, and the ops/passive routing.

## Disclosed adaptations (where tri7 cannot or should not match v5 byte-behavior)
- Param ARITY/type errors keep tri7's house -8 with concise messages ("expected
  nrequired parameter" / "expected an array of keys" / "expected redeemScript
  parameter"): v5 throws runtime_error with the full multi-line usage text via
  the same -1 path; prior graded verbs (repairwallet, item 9) use the same -8
  convention.
- v5 quirks NOT reproduced: non-string redeemScript param (v5 treats it as ""
  and happily registers the EMPTY script; odd-length hex also becomes the empty
  script in v5's ParseHex). tri7 errors on both — no honest use case, and the
  empty-script row is junk.
- 16-key refusal (item 8 above).

## Files changed (3 files + 1 test-only; a 9-line src/gui/app.rs hunk belongs to a sibling agent and is NOT part of this artifact — excluded from the diff and the commit)
- src/rpc.rs — both arms: i64 nrequired, v5 check order, -1 + verbatim v5
  strings (leading space kept), ParseHexV-exact addredeemscript error,
  addredeemscript returns the address, [account] → set_label in both verbs;
  test updated to pin all of it (error codes AND messages)
- src/wallet.rs — decode_address_payload accepts the SCRIPT_ADDRESS class
  (single caller set_label; enables the label row on 'C' addresses)
- src/multisig.rs — known_answer_vectors_privkeys_1_2_3 test (canonical
  vectors; wallet-independent)

## Harness evidence (all run on DNS2 against the patched tree; transcripts attached)
- cargo test --lib multisig → 8 passed (incl. the new vectors test)
- cargo test --lib (whole suite) → 521 passed, 0 failed, 1 ignored (was 520)
- cargo test --bin follow_tip → 63 passed, 0 failed
- cargo build --release --target x86_64-pc-windows-gnu (all bins) → exit 0 in 2m05s

## Live deployment plan (after CI green)
DNS2 (local release build + health-timer-first swap), DNS3 (scp binary), Pi
(on-device aarch64 build), sami-pc (CI artifact). Live checks: error-path
strings on the live RPC (no wallet writes), full write-path on a COPY of
DNS2's wallet.dat in a scratch state dir (wallet-write verbs are proven
against copies, never the live wallet).
