=== PROPOSED CHANGE to triangles-modernization references/p2sh-multisig.md, "What a complete implementation needs" item 7 (replaces the bare line "7. RPC `addmultisigaddress` / `addredeemscript`.") ===

7. RPC `addmultisigaddress` / `addredeemscript` — **LANDED 2026-11-09**
   (commit 691ac6f, grade B zero blockers
   urn:ump:2jlsffi2xwqgg2dmmq2m57juo4cu5nbdd27dj43rem37f6qlhyzq; deployed all
   four nodes). v5 semantics, read from the REAL tree (/opt/triangles-v5/src/
   rpcwallet.cpp:836/:905 + trianglesrpc.cpp:151/:1321):
   - BOTH verbs return the BARE P2SH address string
     (CTrianglesAddress.ToString()) — NOT bitcoin-0.8's
     {"address","redeemScript"} object; that shape is from a different era.
   - Every semantic failure is std::runtime_error → the dispatcher's
     RPC_MISC_ERROR = -1 with the verbatim string: n<1 "a multisignature
     address must require at least one key to redeem"; count "not enough keys
     supplied (got N keys, but need at least M to redeem)"; "<ks> does not
     refer to a key"; "no full public key for address <ks>"; " Invalid public
     key: <ks>" — the LEADING SPACE is in the v5 source (:876/:884).
   - Check order mirrors v5's evaluation: n<1 → count → per-key → build (so
     nrequired=17 with 2 keys answers "not enough keys", not the cap).
   - ParseHexV hex error: -8 "redeemScript must be hexadecimal string (not
     '<input>')" (RPC_INVALID_PARAMETER).
   - [account] → SetAddressBookName label row on the P2SH address in BOTH
     verbs; empty/absent writes nothing (v5 WriteName erases on empty names).
     Requires the wallet's name-row decoder/writer to accept the SCRIPT_ADDRESS
     ('C') class, not just 'T'.
   - v5 has NO 16-key check — EncodeOP_N assert(n<=16) ABORTS the process.
     tri7 refuses -8 "a multisignature address must require between 1 and 16
     keys" after the legacy-ordered checks (disclosed deviation).
   - Known-answer vectors are pinned by a unit test (canonical privkeys
     1/2/3 → byte-exact 105-byte 2-of-3 redeem → CJU8gKtn4aNBBovbywqPqZo6mjA
     1B1pLD1; 1-of-3 → Cc7Jwy788GFxRiW8Xza64Ra6C9Qb78maFS) and were
     reproduced LIVE on a deployed node against a wallet COPY.
