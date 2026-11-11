# tri7: accept .b32.i2p addnode hosts (dual-network fleet) — judge brief

## What changed (measured delta)
- File: `src/bin/follow_tip.rs` ONLY. ADDED: i2p acceptance in
  `addnode_targets_from_conf` (hosts ending `.b32.i2p` or `.i2p` now parse as
  `Transport::I2p` targets dialled through the i2pd SOCKS, default
  127.0.0.1:4447, exactly like the existing `--i2p` CLI flag path);
  CHANGED: the accept-log line now names the transport ("accepted (i2p)" vs
  "(tor)"), the reject message, the function docstring, and one caller
  comment; CHANGED: test `addnode_conf_parsing_tor_only` renamed to
  `addnode_conf_parsing_dual_overlay` and extended with a real b32 line
  asserting transport/socks/port. REMOVED: nothing else.
- The full file is ~9.5k lines; the graded bytes are the unified diff
  (`change.diff`, N lines) — excerpts below reproduce the changed regions
  verbatim.

## Why (the defect, witnessed live)
The fleet is dual-network by standing operator directive ("I2P IS FIRST-CLASS
in everything Triangles … A Tor-only claim anywhere is a bug"). The conf-file
addnode parser nevertheless accepted ONLY `.onion` hosts and logged
`addnode=<b32>.b32.i2p:… ignored: Tor-only — only .onion addnode hosts are
accepted`. Live evidence (sami-pc follow_tip log, 2026-11-07T10:36:03Z, four
i2p lines incl. the live DNS2 tunnel target `ztj5lcg4tgcn7lh7wlrgvmfhf42itoet
wgh42doogu7w3gdhxtbq.b32.i2p:24113` all rejected). The `--i2p` CLI flag path
(`PeerTarget::parse`) already supported i2p — the conf parser just never
routed those hosts to it. Cleartext IP/hostname stays REJECTED (no clearnet
P2P policy unchanged).

## Verification actually run (transcript excerpts, real outputs)
1. `cargo test` full suite: `test result: ok. 455 passed; 0 failed; 1 ignored`
   (lib) and `test result: ok. 41 passed; 0 failed` (follow_tip bin),
   including `tests::addnode_conf_parsing_dual_overlay ... ok` and
   `tests::addnode_ignored_list_requires_host_port_shape ... ok` (cleartext
   `ok.example.com:24113` still ignored) and
   `tests::peer_target_parse_i2p_and_onion_transports ... ok`.
2. Fleet ops (context for why this matters; not part of the graded bytes):
   sami-pc now runs i2pd 3.0.0 as a SYSTEM scheduled task (`i2pd`, onstart,
   `--datadir` only; tunnel keys persisted `data\tri7-p2p-keys.dat`, 679
   bytes, same format as DNS2's); its server tunnel `[tri7d-p2p]` forwards
   `mb3ox52uzwh7d7u6ct6mdapcsyqcxiffz53hlydwsr7cwhvys5za.b32.i2p:24113` →
   100.85.236.10:24113 (the tri7 listener). DNS2 (i2pd 2.56) dial through
   SOCKS 4447 to that address returned SOCKS code 0 and the listener's
   6-byte greeting — address publication + reachability PROVEN. sami-pc RPC
   `getlocalinfo` now returns
   `"i2p": "mb3ox52uzwh7d7u6ct6mdapcsyqcxiffz53hlydwsr7cwhvys5za.b32.i2p:24113"`
   (state file `i2p_destination.txt`). The graded parser fix is what makes
   the EXISTING conf addnode lines (`ztj5lcg4…` live + 3 stale legacy-era
   entries) join the attach list on the next binary deploy, so follow_tip
   dials DNS2's i2p tunnel and `getpeerinfo.by_transport.i2p` can go nonzero.
   `getpeerinfo` at brief time: `by_transport {"clearnet":1,"i2p":0,"tor":1}`
   (old binary still deployed — the fix has not been deployed yet; deploy
   verification happens right after the graded commit lands and CI builds
   the Windows artifact).
3. Acceptance behavior covered by tests: onion accepted (tor), b32 accepted
   (i2p, socks=4447), cleartext ignored+logged, junk lines never panic.

## Constraints honored
- No cleartext P2P path added; i2p targets ride the i2pd SOCKS like tor rides
  the tor SOCKS.
- No secret material in the change; the operator conf that motivated it
  carries an rpcpassword which is NOT part of this commit.

## Answer shape
A single JSON object as the last line, nothing after it:
`{"codex_grade": "A|B|C|D|F", "reasons": [...], "blocking_issues": [...],
"polish_suggestions": [...]}`
