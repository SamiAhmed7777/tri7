# Network seeds — verified three-node mesh

**Acceptance updated 2026-11-25.** A seed is live only when the node has an
adopted protocol-70206 `/tri7:7.0.0/` peer session with the expected onion
identity. A Tor SOCKS reply proves TCP reachability only; it is not mesh
acceptance. Heights in the linked evidence are point-in-time snapshots, not
persistent fleet constants.

## Live onions and per-node seeds

| Node | Published onion | Local hidden-service destination | Required `addnode` peers |
|---|---|---|---|
| DNS2 | `gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113` | `127.0.0.1:24114` | DNS3 and Pi below |
| DNS3 | `brudzmft5zvvrvlzfeywy2n7gh4bu7egsbjab4ois6xwxgnqjitxywid.onion:24113` | `127.0.0.1:24113` | DNS2 and Pi below |
| Pi (`ssh hetzner`) | `ra32xf5ogzq2ialyjyptodb6ibhs4owcfvvzkvind2mz4ntrbijqk2yd.onion:24112` | `127.0.0.1:443` | DNS2 and DNS3 below |

DNS2's published onion port **must not** be moved to tri7's local port 24114.
Keeping the virtual port at 24113 preserves the address peers dial. The local
destination is 24114 because legacy `trianglesd` still owns public 24113.

Use exactly these config entries (DNS2 shown; substitute the other two rows
for each node):

```conf
addnode=brudzmft5zvvrvlzfeywy2n7gh4bu7egsbjab4ois6xwxgnqjitxywid.onion:24113
addnode=ra32xf5ogzq2ialyjyptodb6ibhs4owcfvvzkvind2mz4ntrbijqk2yd.onion:24112
```

Every node carries **both** other nodes. Replacing the list with only DNS2
leaves DNS3↔Pi dependent on discovery/rotation and does not meet acceptance.
`tor.service` is enabled on all three nodes and is a tri7d boot-ordering
dependency on all three. sami-pc was unreachable during verification and is
not claimed as a fourth mesh member.

## Acceptance and forcing a dial after a cold boot

After health is ready, an operator can request each missing session with the
RPC form `addnode <onion:port> onetry`. The 2026-11-25 repair issued six
calls (both peers from each node) and required every call to succeed before
reading `getpeerinfo` again.

Acceptance requires all six directed links, with the exact identity,
`version == 70206`, and `subver == "/tri7:7.0.0/"`:

- DNS2 → DNS3 and DNS2 → Pi
- DNS3 → DNS2 and DNS3 → Pi
- Pi → DNS2 and Pi → DNS3

DNS2 must also have at least one inbound session from DNS3 or Pi. This proves
the published onion reaches tri7 and completes the protocol handshake. See
[`peer-mesh-repair-2026-11-25.md`](peer-mesh-repair-2026-11-25.md) for the
captured evidence.

## Dead legacy seeds

The four legacy seed onions below returned SOCKS reply 4 (host unreachable)
in the 2026-11-06 probe. They were not used by the 2026-11-25 repair.

| Address | Historical status |
|---|---|
| `43kgtx6….onion:24112` | DEAD (2026-11-06) |
| `jbpfhe7z….onion:24112` | DEAD (2026-11-06) |
| `6ygpphp2….onion:24112` | DEAD (2026-11-06) |
| `nawqqoaz….onion:24112` | DEAD (2026-11-06) |

`i6tk7sozn….onion` is also superseded on DNS2; use the explicit table above.

## Peer-store behavior

Seeds bootstrap the peer store. tri7 keeps peers in `<state-dir>/peers.json`,
answers `getaddr`, and merges `addr` gossip into that store. One reachable
peer can be enough to learn other addresses, but it is **not** proof of a full
mesh. See `tor-discovery-and-rotation.md` for the wire format and rotation
design.
