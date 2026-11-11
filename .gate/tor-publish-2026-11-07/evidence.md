# TOR-PUBLISH evidence — fleet getlocalinfo onion/i2p publication (2026-11-07, 12:07–12:13 UTC)

All probes labelled with the host that ran them. DNS2 = judge host (local), Pi = ssh hetzner, DNS3 = ssh dns3, PC = ssh sami-pc.

## 1. Code reality (why this is ops, not code)
`getlocalinfo` in src/rpc.rs reads the state dir per RPC CALL (no cache, no restart needed):
- onion: first non-empty of `onion_address.txt`, `hidden_service/hostname`, `tor/hostname`
- i2p: first non-empty of `i2p_destination.txt`, `i2p/hostname`
(lines 1494-1525: `read_first` closure over `deps.wallet.state_dir()`).

## 2. DNS2 (local probes, http://127.0.0.1:19112)
BEFORE:
{"i2p":null,"listen":3,"note":"no Tor/I2P identity configured for this node","onion":null,"state_dir":"/var/lib/tri7"}
Files written (root→tri7:tri7 0644, `install`):
- /var/lib/tri7/onion_address.txt ← copy of /var/lib/tor/triangles/hostname
  (torrc: HiddenServiceDir /var/lib/tor/triangles/; HiddenServicePort 24113 127.0.0.1:24113; HiddenServiceVersion 3)
- /var/lib/tri7/i2p_destination.txt = "ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113"
  Source: i2pd webconsole http://127.0.0.1:7070/?page=i2p_tunnels → "Server Tunnels: tri7d-p2p ⇔ ztj5lcg4…b32.i2p:24113" (verbatim transcript below). Cross-check: identical address was sami-pc's i2p handshake TARGET last tick (HANDOFF 2026-11-07 i2p section, state2.log 11:32:31Z).
AFTER (no restart; next RPC call):
{"i2p":"ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113","listen":3,"note":"","onion":"gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion","state_dir":"/var/lib/tri7"}
File perms: -rw-r--r-- 1 tri7 tri7 67/63 bytes (04:07 local-time listing).

### i2p tunnel mapping transcript (DNS2, curl 'http://127.0.0.1:7070/?page=i2p_tunnels', tags stripped)
Client Tunnels:  IRC-ILITA ⇐ 5qztnkk…b32.i2p ; SOCKS Proxy ⇐ x42k464…b32.i2p
Server Tunnels:  tri7d-p2p ⇔ ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113

## 3. Pi (ssh hetzner)
- HS: /var/lib/tor/tri7pi/ hostname ra32xf5ogzq2ialyjyptodb6ibhs4owcfvvzkvind2mz4ntrbijqk2yd.onion; torrc line 196 was `HiddenServicePort 24112 127.0.0.1:24112` while the tri7 daemon listens on `--listen-port 443 --listen-bind 0.0.0.0` → onion existed but NOTHING answered behind it.
- FIX: backup `cp /etc/tor/torrc /etc/tor/torrc.bak-20261107-tri7`; sed 24112→127.0.0.1:443; `systemctl restart tor@default` → unit active, hostname UNCHANGED (ra32…, keys on disk).
- Publication: `install -m 0644 /var/lib/tri7pi/hostname /var/lib/tri7/onion_address.txt` (state dir root-owned 755; tri7d runs as root on Pi).
- AFTER getlocalinfo (on Pi, port 19112):
{"i2p":null,"listen":1,"note":"","onion":"ra32xf5ogzq2ialyjyptodb6ibhs4owcfvvzkvind2mz4ntrbijqk2yd.onion","state_dir":"/var/lib/tri7"}
- END-TO-END: DNS2's own persistent addnode `--onion ra32…onion:24112` dialed Pi after the fix and established a REAL tri7 peer — DNS2 getpeerinfo:
{"connected_utc":"2026-11-07T12:10:00Z","endpoint":"ra32xf5ogzq2ialyjyptodb6ibhs4owcfvvzkvind2mz4ntrbijqk2yd.onion:24112","inbound":false,"last_traffic_ms_ago":4836,"msgs_in":8,"msgs_out":9,"start_height":2207107,"subver":"/tri7:7.0.0/","transport":"tor","version":70206}

## 4. DNS3 (ssh dns3)
- tor installed (0.4.9.11) but NOT running; no HiddenService anywhere; /var/lib/tor had only cache files.
- FIX: backup `cp /etc/tor/torrc /etc/tor/torrc.bak-20261107-tri7`; appended:
  HiddenServiceDir /var/lib/tor/tri7/
  HiddenServicePort 24113 127.0.0.1:24113
  HiddenServiceVersion 3
  `systemctl start tor@default` → active; new v3 onion appeared (~30s): brudzmft5zvvrvlzfeywy2n7gh4bu7egsbjab4ois6xwxgnqjitxywid.onion
- Publication: `install -o tri7 -g tri7 -m 0644 /var/lib/tor/tri7/hostname /var/lib/tri7/onion_address.txt` (state dir tri7:tri7 0700).
- AFTER getlocalinfo (on DNS3, port 19112):
{"i2p":null,"listen":1,"note":"","onion":"brudzmft5zvvrvlzfeywy2n7gh4bu7egsbjab4ois6xwxgnqjitxywid.onion","state_dir":"/var/lib/tri7"}

## 5. Reachability dials (DNS2 → tor socks 127.0.0.1:9050, curl -v --socks5-hostname)
Pi onion:24112 → `SOCKS5 request granted` + `Empty reply from server` (curl 52) — stream reached the tri7 P2P listener and was closed on non-protocol input; tri7 is not HTTP, 52 is the expected outcome.
DNS3 onion:24113 → `SOCKS5 request granted` + `Empty reply from server` (curl 52) — same shape.

## 6. PC (ssh sami-pc, port 19114) — unchanged, re-verified
{"i2p":"mb3ox52uzwh7d7u6ct6mdapcsyqcxiffz53hlydwsr7cwhvys5za.b32.i2p:24113","listen":2,"note":"","onion":"tuu7h65jykj327um6jfcecqmgyq6mjn3psiay5mtald2olsswk4jcrad.onion","state_dir":"C:\\Users\\hello\\tri7\\state2"}

## 7. Chain + service health after the change (nothing broken)
- DNS2 h2207108 eb5e471a == Pi h2207108 eb5e471a (exact parity at shared tip).
- DNS3 h2206990 26e74119 — ~118 behind, PRE-EXISTING (DNS3 follows via sami-pc tailscale 100.85.236.10:24113; daemon untouched this tick).
- PC h2207114 1abd0390 — 6 ahead (own staking race; pre-existing pattern).
- DNS2 staking: enabled:true, attempts 5,671,832, coins_considered 110 — staking never stopped.
- getpeerinfo transports at wrap-up: clearnet 3, tor 1, i2p 0.
- Daemon processes: ZERO restarts on any host this tick (only tor restarted on Pi/DNS3; both tor units active).

## 8. Scope honesty
- Item bar (workqueue #3): getlocalinfo real onion on every node + DNS2 i2p destination file → MET on all four nodes (DNS2 onion+i2p, Pi onion, DNS3 onion, PC onion+i2p pre-existing).
- Pi/DNS3 i2p remains null: no i2pd installed there — out of this item's scope (item text scopes i2p to DNS2).
- DNS3's new onion is published but not yet advertised (no addnode/peerstore wiring anywhere) — later tick.
- DNS2's other addnode i6tk7soz…onion:24112 belongs to no current HS (historical rotation leftover); harmless failed dial, left untouched.
