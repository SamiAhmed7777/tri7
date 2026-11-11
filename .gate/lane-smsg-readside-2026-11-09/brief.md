# Judge brief: tri7 lane-smsg-readside — smsgbuckets / smsggetpubkey / smsgaddkey

## What this artifact is
One code change on tri7 (branch lane/smsg-readside, base 9723e33; master has
since advanced by 37f3bcd createmultisig, 42b689d item-17 fork-reorg, and
a6ce9fe smsg-sendpath — parent will merge and resolve; disclosed). Implemented
by a Kimi Code delegate child (headless kimi -p) from a written brief;
re-verified by the parent session.

## Scope (+438 across five files)
- src/smsg_net.rs (+323): the read-side verbs — bucket listing
  (smsgbuckets [<all>|<days>]), pubkey lookup (smsggetpubkey), peer key
  import (smsgaddkey — THE carry-over verb: legacy address-book/smsg keys
  become importable), with legacy output shapes and error strings.
- src/rpc.rs (+22): list entries (NATIVE + OPS), handler arms, tests incl. a
  routing assertion loop.
- src/smsg.rs (+14): store helpers for the read side.
- src/bin/follow_tip.rs: OpsAdapter arms.
- src/gui/rpc_client.rs (+5): SCOPE DEVIATION, disclosed and justified: the
  three verbs added to the GUI console ALLOWED_METHODS — no GUI page calls
  them; the RPC console (debug window) is the only consumer. Parent reviewed
  the diff (5 allowlist lines + comment) and accepts.

## Semantics anchors (child cited legacy source; parent spot-verified the
## anon/enable analogues in the same file during the send-lane round)
- legacy verb table: vRPCCommands in trianglesrpc.cpp; smsg RPC handlers in
  rpcsmessage.cpp; smsg core in src/smsg/.
- smsgaddkey validates the peer key format like legacy before persisting via
  the existing smsg store.

## Parent verification (adopting session)
- Private target dir: FULL cargo test --lib = **535 passed; 0 failed;
  1 ignored** (base 531; +4 new fns).
- Routing greps: all three verbs present in NATIVE_METHODS and OPS_METHODS.
- Scope: git status = the five files above only.

## Files in this bundle
- brief.md; readside.diff (+438); readside-full.src (full current bytes of the
  new smsg_net.rs read-side sections + the three verbs' arms + tests);
  tests-readside.txt (isolated-target suite tail).
