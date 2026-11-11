# Skill delta: tri7-fleet-chain-state §6 (tor/i2p publication facts learned this tick)
Appended after the existing "getlocalinfo reads ONLY files in the state dir …" paragraph:

Fleet publication facts (2026-11-07, all four nodes):
- The published onion is only REAL if the torrc `HiddenServicePort` targets the daemon's actual listen port. Pi's HS mapped 24112→127.0.0.1:24112 while follow_tip listened on 443 — the hostname existed and NOTHING answered behind it (and DNS2's addnode to it sat dead); fixing the map to 127.0.0.1:443 made DNS2's existing addnode establish a tor peer on the next dial cycle. When getlocalinfo gains an onion, dial it through the fleet's tor socks before calling it reachable.
- DNS3's HS lives in /etc/tor/torrc (appended; backups named torrc.bak-20261107-tri7 on Pi + DNS3). tor@default is the unit that matters (`systemctl is-active tor` reads the disabled wrapper and lies).
- DNS2's i2p b32 comes from the i2pd webconsole `?page=i2p_tunnels` "Server Tunnels" line (tri7d-p2p ⇔ <b32>:24113) — read the mapping, do not compute the b32 from the keys file by hand; cross-check against the remote peer's successful handshake target.
- Publication is per-call: getlocalinfo picks up new state files with NO daemon restart.
- Pi/DNS3 still have no i2pd (i2p null there is honest); DNS3's onion is published but not yet advertised into a peer list.

Session context: this delta is the lessons file for workqueue item 3 (TOR-PUBLISH), graded A zero blockers this session: urn:ump:lgwbnvzp24a42aturwoqgm2m6d6lqwpsrc3oec3fqwgni5bfuw3a. Codex is quota-walled until Nov 8 20:15Z; GLM stand-in is the loop's standing judge per TRI7-WORKQUEUE.md.

Rubric: does the delta record correct, durable operational facts (pitfalls + sourcing rules) without task-log narration? Answer with a single JSON object, last line, nothing after:
{"codex_grade":"A|B|C|D|F","reasons":["…"],"blocking_issues":["…"],"polish_suggestions":["…"]}
