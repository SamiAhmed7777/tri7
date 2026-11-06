# Tor-only discovery + onion rotation

Sami's requirement (2026-11-06): the Triangles network must be **Tor only**, every
wallet must be able to **find and support each other**, and onion addresses must
**rotate automatically every couple of months**.

This document records the verified facts and the design. Numbers and byte layouts
below are from the legacy source at `/opt/triangles-v5/src`, not assumptions.

**UPDATE 2026-11-06: this design is SHIPPED** (items 1-5 of BRIEF_TOR_V2.md).
`src/p2p.rs` (legacy wire codec + `addr`/`getaddr`), `src/peers.rs` (peer store),
`src/onion.rs` + `tri7-onion-rotate` (rotation via the Tor control port), conf
`addnode=` parsing and `docs/network-seeds.md` all exist now. Section-by-section
"Shipped:" notes below describe the actual behaviour; anything still open is
listed at the bottom.

## Why the network collapsed to one node

- Peers are only ever given on the command line (`--daemon`, `--onion`); tri7 does
  not parse `addnode=` from a conf file and keeps no peer store.
- The four seed onions in the legacy conf are dead — probed through Tor:
  `43kgtx6…`, `jbpfhe7z…`, `6ygpphp2…`, `nawqqoaz…` all return SOCKS reply 4
  (host unreachable).
- DNS2's systemd unit advertises `i6tk7sozn…onion`, which also fails (SOCKS 1).
  Its **live** hidden service is a different address,
  `gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113`
  (`HiddenServicePort 24113 127.0.0.1:24113`, verified reachable, SOCKS 0).
- The legacy client did gossip addresses (`addr`/`getaddr`, `CAddrMan`, `peers.dat`
   — its log lines say "Flushed N addresses to peers.dat"). tri7 never implemented
  any of it. So with the seeds gone there is nothing but a static list.

## Verified wire format (legacy)

`CNetAddr` (`netbase.h:88-93`):

    ip[16] | tor_v3_pubkey[32] | m_is_tor_v3 (1 byte)

For a v3 onion (`netbase.cpp:615-632`): decode base32(56) → 35 bytes
`pubkey(32) || checksum(2) || version(1)`; set `ip[0..6] = FD 87 D8 7E EB 43`
(OnionCat), zero the rest; `tor_v3_pubkey = decoded[0..32]`; `m_is_tor_v3 = true`.

`CService` = `CNetAddr` + `port` (big-endian u16) → 51 bytes.
`CAddress` (`protocol.h:84-97`) = `nTime` (u32 LE, when nVersion ≥ CADDR_TIME_VERSION)
+ `nServices` (u64 LE) + `CService` → **63 bytes** per entry.
`addr` payload = CompactSize count + that many `CAddress`.
`getaddr` payload = empty; reply is `addr`.

**tri7 divergence (FIXED):** tri7 used to emit a 60-byte `encode_no_time`
(`services + ip + tor_key + is_tor_v3 + is_i2p + port`) — one extra byte versus
legacy's 59 — so a legacy daemon and tri7 could not parse each other's
`version`/`addr`.

Shipped: tri7 now **encodes the legacy layout** (`encode_no_time` = 59 bytes,
`encode_caddress` = 63; i2p is derived from the GarliCat prefix, not a wire byte)
and **parses both** layouts leniently (`parse_version` accepts the shape that
consumes the payload with 0 trailing bytes; `parse_addr` picks 63- vs 64-byte
entries by exact body size). Round 2: wire-learned onions are stored under their
**reconstructed `.onion` hostname** — `WireAddr::onion_hostname()` rebuilds
`base32(pubkey || checksum || 0x03)` with
`checksum = sha3_256(".onion checksum" || pubkey || 0x03)[..2]` (the `sha3`
crate; a text encoding of the tor-generated key, no curve arithmetic) and text
labels are checksum-verified before acceptance — so gossip-discovered peers are
dialable by the `.onion`-dispatching dial loop. The known-answer test
reconstructs the live seed
`gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion` to itself
exactly. Legacy peers.json files with 64-char hex keys still load (relay-only).

## Design

### 1. Peer store (`peers.json` in the state dir)

```
{ "peers": [ { "host": "<onion or ip>", "port": 24113, "services": 1,
               "source": "seed|addr|config", "last_ok": <unix>, "last_fail": <unix>,
               "ok_count": n, "fail_count": n } ] }
```

- Dial order: best-seen peers first, then seeds.
- Exponential backoff on `fail_count`; a peer that has ever answered is retried
  longer than one that never has.
- Persisted atomically (write temp + rename), capped (e.g. 1000 entries).

Shipped: `<state-dir>/peers.json` via `src/peers.rs` — atomic 0600 write, cap
1000 (worst-score evicted), backoff `2^fail_count` seconds capped 1 h,
`dial_order` = answered-first (`last_ok` desc) then fresh-by-backoff with seeds
appended. A corrupt file loads as an empty store with a log line.

### 2. `addr` / `getaddr`

- On `verack`, send `getaddr`.
- Answer `getaddr` with up to 1000 known-good peers (our own onion included).
- Accept `addr` with up to 1000 entries, reject the rest, never trust entries that
  claim our own address.
- Learn from every inbound connection: a peer that connected to us is reachable,
  so record its listening address when it is advertised in `version`.
- Relay learned addresses onward (bounded fan-out, no flooding): gossip is what
  makes the network survive dead seeds.

Shipped: `getaddr` is sent right after `verack` in both directions; `getaddr` is
answered with `addr` (≤ 1000 entries from the store, our own onions excluded);
inbound `addr` is merged into the store (tor v3 only, self-idents skipped, parse
errors log and continue); `version.addr_me` that is not a loopback placeholder is
recorded as a reachable listening address.

### 3. Onion rotation (every ~60 days)

- A per-node identity key lives in the state dir (`node_identity.key`), independent
  of the Tor HS key, used only to sign rotation announcements.
- Tor publishes the hidden service from `HiddenServiceDir`; rotation means a NEW
  directory (new ed25519 key → new v3 address).
- Sequence:
  1. generate `triangles-<epoch>/`, start a second hidden service (both live);
  2. announce `{identity_pub, old_onion, new_onion, epoch, valid_from, valid_until}`
     signed by the identity key, over `addr` (legacy peers simply learn the new
     address and connect to it) and as a tri7 extension message for signed proof;
  3. keep the OLD service for a grace window (≥ 30 days) so peers that only know
     the old address still reach us and can learn the new one;
  4. retire the old service after the grace window.
- Driven by a systemd timer (`tri7-onion-rotate.timer`, every 60 days) that runs
  `tri7-onion-rotate` (script) → new HS + SIGHUP/reload + announce.
- Never rotate all nodes at once: the timer is staggered per node with a random
  offset (e.g. ±7 days) so the network always has live addresses.

Shipped: `src/onion.rs` + the `tri7-onion-rotate` binary. Rotation talks to the
Tor **control port** (`AUTHENTICATE`, `ADD_ONION NEW:ED25519-V3
Port=24113,127.0.0.1:24113`, `DEL_ONION`) or reads a torrc-managed
`<HiddenServiceDir>/hostname` via `--hostname-file`. State lives in
`onion-state.json` `{current, previous, rotated_at, grace_until, identity_pub,
current_key, previous_key}`; the identity key (`node_identity.key`, secp256k1)
signs a `RotationAnnouncement` (domain-separated sha256d, compact-recoverable
sig) that self-verifies and rejects tampering. `--retire` DEL_ONIONs the
previous address only after the grace window (refused earlier). Round 2:

- **Hold mode (`--hold`)** — an `ADD_ONION` service is ephemeral and dies when
  its control connection closes, so the holder KEEPS the current service's
  connection open and re-establishes the previous one on a second held
  connection (`ADD_ONION <stored ED25519-V3 key> ...`), then blocks, probing
  each connection and refreshing state every 60 s. The log states which
  addresses are actually SERVING; a previous address with no stored key is
  logged as NOT SERVING, never claimed alive. Persistent alternative for
  reboot survival: torrc `HiddenServiceDir`
  + `--hostname-file` (tor keeps the key on disk).
- **Period enforced** — a rotation when `rotation_due` is false is refused
  (exit 0, "rotation not due until <date>") unless `--force`.
- **Units shipped** — `deploy/tri7-onion-rotate.service` (+ `--hold`) and
  `deploy/tri7-onion-rotate.timer` (monthly `OnCalendar`,
  `RandomizedDelaySec=6h` so fleets do not rotate in lockstep,
  `Persistent=true`).

Round 3 (lifecycle fixes — the run-and-exit holder became a DAEMON; the
graded defects were: the wrong service re-established after a rotation, the
holder exiting when rotation came due with no timer able to restart it, and
a post-tor-restart start exiting 0 without restoring anything):

- **Key pairing** — a rotation promotes `current` → `previous` TOGETHER
  with `current_key` → `previous_key` (pure `apply_rotation`, used by the
  daemon and the one-shot alike), so the service that becomes previous is
  re-establishable/kept, and the old address genuinely stays served through
  its grace window.
- **Daemon, not run-and-exit** — `--hold` runs until killed and NEVER exits
  because a rotation is due. Every 60 s tick it runs the pure `plan_tick`:
  past `grace_until` it `DEL_ONION`s the previous and clears previous +
  previous_key from the state; when the 60-day period is due it rotates
  IN-PROCESS (new `ADD_ONION` on a kept connection; the outgoing current's
  connection becomes the previous's connection, so the old address never
  blinks), persists, prints the signed announcement, and carries on.
- **Startup always re-establishes** — on every start the daemon runs the
  pure `plan_startup`: current first, then previous while its grace window
  is open, each from its OWN stored key, on kept connections — whether or
  not a rotation is due. The tick probe re-establishes connections killed
  by a tor restart the same way. A stored current with NO key can never be
  revived, so the daemon rotates immediately at startup to restore service
  (the period gate remains a one-shot-only concept; `--force` is rejected
  with `--hold`).
- **Units rewritten** — `deploy/tri7-onion-rotate.service` is `Type=simple`,
  `Restart=always`, `RestartSec=15`, `ExecStart=... --hold`; the timer no
  longer triggers rotation (it cannot start an already-active unit) and
  only fires the separate one-shot `tri7-onion-rotate-retire.service`
  (`--retire` sweep; the daemon retires on its own tick anyway).
- All scheduling/state logic is pure and tested without a network:
  `plan_startup`, `plan_tick` (no exit variant), `apply_rotation`,
  `apply_retire`, `serving_set` (the honest SERVING/NOT SERVING list).

### 4. Bootstrap

- Ship a small seed list; each seed is an onion. Because seeds die, the peer store
  and gossip are the real bootstrap: one reachable peer is enough to learn the rest.
- Publish the current fleet onions in the repo (docs/network-seeds.md) so a fresh
  install has a better chance than four dead addresses.

Shipped: `addnode=<host>:<port>` is parsed from the conf file; **tor v3 onion
targets only** — cleartext hosts are ignored with a log line (Tor-only network).
`docs/network-seeds.md` lists the one verified-live seed
(`gxvrhv3q…onion:24113`) and the dead legacy seeds.

## Still open

- Signed rotation announcements are produced and verified as a library + CLI,
  but there is no `tri7` extension message type carrying them on the wire yet
  (the new address still spreads via plain `addr` gossip, which legacy peers
  understand).
- Discovery end-to-end is exercised by the suite at the payload level
  (`addr` payload → parse → peer-store merge → dial plan contains the peer's
  `.onion:24113`, self skipped), but a live two-process rotation over a real
  tor control port is still operator-land (needs a running tor).

## Acceptance (decidable)

- `addr`/`getaddr` round-trip: a payload encoded by tri7 parses back identically;
  a payload built with the LEGACY 63-byte layout (fixture bytes) parses correctly.
- Wire check: an onion `WireAddr` encodes to exactly 51 bytes (CService) / 63 bytes
  (CAddress), matching the legacy layout above.
- Peer store: two nodes started with only ONE common seed discover each other and
  keep the learned peer across a restart.
- Rotation: simulated rotation produces a new onion, the old one still answers
  during the grace window, and a peer that knew only the old address reconnects.
