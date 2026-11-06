# Network seeds — verified status

Status legend: **LIVE** = reachable through Tor SOCKS at probe time;
**DEAD** = SOCKS failure through Tor (unreachable). Every entry below was
probed through a local tor SOCKS5 listener; dates are the probe dates.

## Live seeds

| Address | Status | Notes |
|---|---|---|
| `gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113` | **LIVE** (verified 2026-11-06) | DNS2's current hidden service (`HiddenServicePort 24113 127.0.0.1:24113`). SOCKS reply 0. Use this as the primary seed. |

## Dead legacy seeds

The four seed onions shipped in the legacy conf — all return SOCKS reply 4
(host unreachable) through Tor:

| Address | Status | Notes |
|---|---|---|
| `43kgtx6….onion:24112` | **DEAD** (probe 2026-11-06) | legacy conf seed |
| `jbpfhe7z….onion:24112` | **DEAD** (probe 2026-11-06) | legacy conf seed |
| `6ygpphp2….onion:24112` | **DEAD** (probe 2026-11-06) | legacy conf seed |
| `nawqqoaz….onion:24112` | **DEAD** (probe 2026-11-06) | legacy conf seed |

Also dead: `i6tk7sozn….onion` — the address DNS2's systemd unit used to
advertise (SOCKS reply 1). Its live service is the `gxvrhv3q…` entry above.

## Using the seeds

Conf file (`~/.triangles/triangles.conf`) — Tor-only; cleartext `addnode`
hosts are ignored with a log line:

```
addnode=gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113
```

or on the command line:

```
follow_tip --onion gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113
```

Seeds are only the bootstrap. tri7 keeps every peer that ever answered in
`<state-dir>/peers.json`, answers `getaddr` with up to 1000 of them, and
merges `addr` gossip into the same store — one reachable peer is enough to
learn the rest. See `tor-discovery-and-rotation.md` for the wire format and
the rotation design.

If you operate a long-lived node, publish your onion here so fresh installs
have more than one live address.
