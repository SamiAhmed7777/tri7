# Artifact: tri7 workqueue item 11 — SMSG-TOGGLES (legacy secure-messaging runtime toggles + options + scans)

## What this change is
Five legacy RPC verbs ported to the tri7 Rust node (repo /root/tri7, target commit on master after f168710):

- `smsgenable` / `smsgdisable` — runtime toggles on the live smsg engine (rpcsmessage.cpp:22/46 parity)
- `smsgoptions [list|set <optname> <value>]` — the two legacy options (newAddressRecv / newAddressAnon), persisted (rpcsmessage.cpp:63 parity; legacy stores them in smsg.ini, tri7 in the smsg.dat SQLite `settings` table)
- `smsgscanchain` — ScanChainForPublicKeys parity: learn (hash160(pubkey) → pubkey) pairs from 33-byte compressed pubkey pushes in scriptSigs (smessage.cpp:2253 ScanBlock / 2400 ScanChainForPublicKeys)
- `smsgscanbuckets` — SecureMsgScanBuckets parity: rescan the relay store against the owned keys, re-delivering to the inbox (rpcsmessage.cpp:371)

Plus: every pre-existing smsg verb (smsgsend/inbox/outbox/scandir/purge/addaddress/keys/localinfo) now refuses with the legacy string "Secure messaging is disabled." while disabled — that refusal IS the toggle's effect (legacy ProcessMessage guards every smsg command on fSecMsgEnabled).

## Legacy shapes reproduced (source of truth read from /root/triangles_v5-fork/src/rpcsmessage.cpp + smessage.cpp)
- smsgenable when already enabled → RPC error "Secure messaging is already enabled." (legacy runtime_error); disable symmetric.
- Enable path: rebuild bucket set + reset peer table so the first-contact ping re-handshakes every peer (legacy SecureMsgEnable pings each peer, smessage.cpp:1518). Disable path: clear buckets + locks, mark peers not-enabled, owe each formerly-enabled peer ONE final `smsgDisabled` wire message (legacy pushes it immediately; tri7's 2s tick loop is the only outbound path so the notice rides send_data_tick, smessage.cpp:1574 SecureMsgDisable).
- smsgoptions list: legacy emits two duplicate "option" pairs (json_spirit vector-of-pairs); a JSON object cannot carry duplicate keys, so tri7 returns {"options": ["newAddressRecv = true", "newAddressAnon = false"], "result": "Success."} — information-preserving adaptation, flagged here deliberately.
- smsgoptions set: value tokens "+"/"on"/"true"/"1" and "-"/"off"/"false"/"0" (parse in SecMsgOptions::parse_value); soft legacy results kept as result objects: "Too few parameters." + expected "set <optname> <value>", "Unknown value.", "Option not found.", and "Unknown Mode." + expected "smsgoption [list|set <optname> <value>]" (the legacy expected-string typo "smsgoption" is kept for byte parity).
- Options persist across restart (smsg.ini parity): MessageStore gained a `settings` table; engine open() reads newAddressRecv/newAddressAnon back.
- Runtime toggle is NOT persisted (legacy boots enabled unless -nosmsg; tri7 boots the engine enabled the same way — its existence means the wallet was plaintext at boot).
- scanchain delta vs legacy, disclosed: legacy walks genesis→tip with prevout lookups (ReadDiskTx) taking the address from the prevout's scriptPubKey; tri7 scans the daemon's served window clamped to the last 50,000 blocks (an RPC call can never run away; recent blocks are where peers' keys live), and takes the address as hash160(scriptSig pubkey) — identical to the prevout P2PKH destination for every standard spend, and it covers the addrbook's purpose (reply-key lookup). Response reports the honest range + counters {"result":"Scan Chain Completed.","from_height","to_height","scanned_blocks","keys_added","duplicates","invalid"}.
- getlocalinfo/smsgkeys (`localinfo_json`) previously hardcoded "enabled": true; now reports the real toggle state + the options object.

## Files changed (5, +756/−2; one 9-line hunk in src/gui/app.rs in the worktree belongs to a sibling agent's tray work and is NOT part of this artifact — excluded from the diff and the commit)
- src/smsg.rs — settings table + set_setting/get_setting
- src/smsg_net.rs — SecMsgOptions (+parse_value/render/assign/as_json), EngineState.enabled/options/disable_notices, open() options load, disable()/enable(), send_data_tick disabled path, handle() disabled gate, scan_buckets(), learn_pubkey() (secp-validates, dedupes), localinfo honest enabled+options; 7 new tests
- src/rpc.rs — 5 verbs added to NATIVE_METHODS + OPS_METHODS (the routing-completeness test asserts OPS ⊆ NATIVE)
- src/bin/follow_tip.rs — ops adapter arms for the 5 verbs + disabled guards on the 8 existing arms + pure helpers `script_pushes_33` and `smsg_options_cmd`; 2 new helper tests
- src/gui/rpc_client.rs — the 5 verbs on the GUI RPC-console allowlist (no GUI page calls them)

## Harness evidence (commands run on DNS2, tree = the diff below applied)
- cargo test --lib smsg → 40 passed, 0 failed (7 new: disable_stops_data_and_notifies_each_enabled_peer_once, enable_rebuilds_buckets_and_rehandshakes_peers, options_roundtrip_through_the_store, option_value_tokens_match_legacy, scan_buckets_redelivers_relay_messages_to_inbox, learn_pubkey_validates_and_dedupes — see attached transcript)
- cargo test --bin follow_tip → 63 passed, 0 failed (2 new: smsg_options_cmd_matches_legacy_shapes, script_pushes_33_finds_only_exact_compressed_pubkey_pushes)
- cargo test --lib (whole suite) → 520 passed, 0 failed, 1 ignored
- cargo build --release --target x86_64-pc-windows-gnu --bins → exit 0 (all bins, standing windows-portability rule)

## Routing/wiring context the diff excerpt cannot show
- NATIVE_METHODS/OPS_METHODS live in src/rpc.rs (dispatch rejects anything outside them with -32601); the ops adapter impl (`impl tri7_core::rpc::NodeOps for OpsAdapter`) lives in src/bin/follow_tip.rs and receives every OPS_METHODS call; the GUI client allowlist lives in src/gui/rpc_client.rs (rpc_client::is_allowed).
- The main loop (follow_tip.rs ~9447) already calls engine.send_data_tick per peer every 2s and engine.handle for inbound smsg commands — the toggle needs no loop changes: send_data_tick/handle gain internal disabled gates.
- EngineState::default() gives enabled=false; SmsgEngine::open() explicitly constructs enabled=true + persisted options (diff shows the open() change).

## Rubric
Grade the code change: legacy-shape fidelity (the strings above), correctness of the toggle lifecycle (no data flow while disabled; enable re-handshakes; disable notifies peers once), options persistence, scan bounds/parse correctness (script walker must handle direct pushes 1..75, PUSHDATA1/2/4, truncated scripts, non-push opcodes), test adequacy, and honesty of disclosed deltas (duplicate-key adaptation, window-bounded scan, hash160-instead-of-prevout lookup, runtime-only toggle).

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
