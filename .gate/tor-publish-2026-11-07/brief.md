# tri7 TOR-PUBLISH (workqueue item 3) — judge brief

## What changed and why (measured delta)
- REPO DELTA: HANDOFF.md ONLY (+8 doc lines documenting the fleet publication + the Pi HS-port repair + DNS3 HS enablement). NO source change — `getlocalinfo` (src/rpc.rs ~1494-1525) already reads onion/i2p identity files from the state dir per RPC call; item 3 is an OPERATIONS item (place the files, make the onions real).
- FLEET DELTA (the artifact under review):
  1. DNS2: state files onion_address.txt (tor HS copy, gxvrhv3q…onion) + i2p_destination.txt (ztj5…b32.i2p:24113 from i2pd webconsole server-tunnel mapping) → getlocalinfo onion+i2p non-null.
  2. Pi: torrc HS port map FIXED (24112→127.0.0.1:443; the daemon's real listen port — previously the onion answered nothing behind it) + onion_address.txt published → getlocalinfo onion non-null AND a real tor-transport tri7 peer established from DNS2's existing addnode.
  3. DNS3: tor enabled with a NEW v3 hidden service (24113→127.0.0.1:24113) + onion_address.txt published → getlocalinfo onion non-null.
  4. PC: no change (onion+i2p already published last tick; re-verified).
- Backups before every config edit: /etc/tor/torrc.bak-20261107-tri7 on BOTH Pi and DNS3. All state files 0644 owned by the state dir's service user (tri7 on DNS2/DNS3, root on Pi where tri7d runs as root).

## Acceptance (workqueue item 3 bar) — each with live proof
1. "getlocalinfo must report the real onion on every node": DNS2 gxvrhv3q… ✓, Pi ra32xf5og… ✓ (now ALSO reachable — socks5 request granted + far-end accept + DNS2 established peer transport "tor" v70206), DNS3 brudzmft5… ✓ (socks5 granted + far-end accept), PC tuu7h65j… ✓ (pre-existing). Full JSON in evidence.md §2-§6.
2. DNS2 i2p destination from its i2pd keys: i2p_destination.txt = ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113 — sourced from the AUTHORITATIVE mapping (i2pd webconsole "Server Tunnels: tri7d-p2p ⇔ …", evidence §2 transcript), cross-checked against last tick's successful PC→DNS2 i2p handshake target. getlocalinfo.i2p returns it verbatim ✓.
3. Nothing broken: DNS2==Pi exact tip parity (h2207108 eb5e471a), DNS2 staking enabled throughout, zero daemon restarts (evidence §7).

## Honest limitations (declared, not hidden)
- Pi/DNS3 i2p still null (no i2pd on those hosts — item scopes i2p to DNS2).
- DNS3 onion published but not yet advertised into any peer list (publication ≠ advertisement).
- DNS2 addnode i6tk7…onion:24112 is a stale rotation leftover (dead HS), left as-is.

## Rubric
Grade the operational change + its documentation: correctness of the published identities (right files, right ownership, right values, verified sources), the safety of the tor edits (backups, minimal deltas, no daemon disruption), the strength of the verification (live dials, established peer, parity), and honest scoping of what remains. This is an ops item: there is no test suite to run; the live fleet IS the test harness and the transcripts in evidence.md are the run output.

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["…"], "blocking_issues": ["…"], "polish_suggestions": ["…"]}
