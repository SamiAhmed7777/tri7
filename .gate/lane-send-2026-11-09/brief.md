# Judge brief: tri7 lane-send — smsg send path (smsgsend / smsgsendanon / smsgbroadcast)

## What this artifact is
One code change on tri7 (branch lane/smsg-sendpath, base 9723e33 = the commit
master was at when the lane started; master has since advanced by 37f3bcd
[createmultisig, touches src/rpc.rs — parent will resolve the additive
list/handler conflicts at merge, disclosed]). Implemented by a Claude Code
delegate child from a written brief; re-verified by the parent session.

## Scope (+295 across exactly four files)
- src/smsg_net.rs (+184): send path — smsgsend / smsgsendanon / smsgbroadcast;
  legacy SendData cadence (SMSG_SEND_DELAY=2s, smessage.h:26), bucket-lock
  parity, bunch limits (500 msgs / 96000 bytes); smsgbroadcast = one
  SecureMsgSend-shaped envelope PER addrbook pubkey row, skipping the sender's
  own address (rpcsmessage.cpp:899-986).
- src/rpc.rs (+23): the three verbs in NATIVE_METHODS + OPS_METHODS (they need
  the ops engine: wallet smsg keys + network send), handler arms, tests.
- src/smsg.rs (+25): MessageStore::list_pubkeys — the "pk"-prefix scan legacy
  broadcast walks (rpcsmessage.cpp:934-936; ordering divergence disclosed:
  insertion+hash160 vs legacy RocksDB key order — affects failure-report
  order only).
- src/bin/follow_tip.rs: OpsAdapter routing for the three verbs.

## Crypto provenance (important)
The envelope builder `smsg_encrypt` (src/smsg.rs:794) is PRE-EXISTING graded
code (Phase 7 smsg core, commit b74cc74, grade B urn:ump:xtm6fekg…) — this
lane introduces NO new cryptography. Its smsgsendanon test pins the legacy
anonymous payload layout BYTE-FOR-BYTE with citation (smessage.cpp:3851-3864:
[250][4 unused][len u32][data] — no sender hash160, no signature) and the
anon path forces addrFrom="anon" exactly like legacy (rpcsmessage.cpp:550,
verified by the parent against the legacy tree).

## Parent verification (run by the adopting session)
- Private target dir (isolated from sibling lanes): FULL `cargo test --lib` =
  **533 passed; 0 failed; 1 ignored** (base 9723e33 has 531; +2 new fns).
- Routing greps: smsgsend / smsgsendanon / smsgbroadcast each present in
  NATIVE_METHODS and OPS_METHODS, absent from PROXY_FALLBACK_METHODS.
- Scope: `git status --porcelain` = the four authorized files only.

## Interop note (disclosed limitation)
The lane's roundtrip tests prove build→parse→validate→decrypt self-consistency
over the pre-existing smsg_encrypt envelope. Byte-level interop with a LEGACY
client-built message is not exercised by a test in this lane (no legacy-built
vector exists in-repo); the envelope builder it relies on shipped in the
graded Phase 7 smsg core. Flagged as follow-up verification, not a claim.

## Files in this bundle
- brief.md — this file; send.diff — complete diff (+295)
- send-full.src — full current bytes of the new smsg_net.rs send/broadcast
  sections and the three routing/test regions
- tests-send.txt — isolated-target full-suite tail
