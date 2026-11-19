# item 34 DIALER-HYGIENE — own-onion self-dial filter + one-session-per-endpoint dedupe

Fleet catches (2026-11-18, live on DNS2 during the continuity sweep), two related P2P hygiene defects:

## Defect 1 — SELF-DIAL UNFILTERED
DNS2's own onion `gxvrhv3q….onion` sat in its own peer store (src=addr, learned via addr
gossip), and the dialer re-dialed it forever: 494+65 failed dials (last_fail 23:10:25Z,
ongoing every ~2-3 min), one "ok" (11-13) = a self-echo session.

ROOT CAUSE (three layers):
1. `self_identities_at()` registered identities from `onion-state.json` + the i2p files
   ONLY. DNS2's onion is published from the system tor hostname file
   (`onion_address.txt` / `hidden_service/hostname` / `tor/hostname` — the same list
   `overlay_addr_me()` reads for version addr_me). With no onion-state.json, its own
   onion was never in `self_idents`, so `merge_addr`'s is_self filter never matched and
   the gossip of ourselves was upserted.
2. No purge: once poisoned, the store entry survived forever (no boot-time cleanup).
3. No dial-time filter: `connect_and_handshake` walked `dial_order` results without a
   self check, so a poisoned row cost a real tor circuit + handshake failure every
   rotation.

## Defect 2 — DUPLICATE OUTBOUND/INBOUND SESSIONS (never pruned)
18 live concurrent sessions to ONE healthy third-party peer `ra32xf5og…:24112`,
accumulating ~3-4/hour since ~20:04Z. Their claims froze (h2214890 15e9ba45) then went
None — exactly the claim-less duplicates that poisoned the sweep's peer-claim quorum
last tick (the "PARITY_OK=False Pi-artifact").

ROOT CAUSE: Tor/i2p INBOUND sessions carry per-CONNECTION loopback socket labels
(`in:127.0.0.1:<ephemeral> (…)`), so every re-dial from the same peer looked like a NEW
endpoint. The daemon kept NO stable per-peer identity on the session row, so nothing
could dedupe or evict — and the only dedupe in the codebase (AdoptPeer for `addnode
onetry`) compared labels, which never match across inbound re-connects.

## FIX (this diff)
Defect 1 — own-onion filter at addr-ingest AND dial time:
- `self_identities_at()` (+16 lines): also reads the three tor hostname files, pushing
  each 62-char `.onion` host. Same file list as `overlay_addr_me` — identity and
  advertisement can no longer disagree.
- `PeerStore::set_self()` (peers.rs): now PURGES existing store rows matching a self
  identity and returns the removed count. Every `with_peer_store` call re-registers
  identities, so DNS2's poisoned rows are gone on the first boot after deploy and the
  poison cannot survive a restart.
- `connect_and_handshake` (+13 lines): computes self idents once per dial pass and
  skips any store target whose host matches, with a loud "skipping SELF endpoint" log.
  Covers the window between store load and purge plus any residual mismatch.

Defect 2 — one session per endpoint + stale eviction + quorum views:
- `Peer` gains `advertised: Option<String>` (the addr_me store key from the handshake —
  set at all three construction sites: outbound = dialed host, inbound = peer's
  advertised onion/i2p identity). This is the ONLY stable identity of an inbound
  overlay session.
- `peer_identity_host()` pure helper: outbound → endpoint host from the label; inbound
  → advertised; falls back to the raw label (an unidentified session dedupes only
  against itself — honest unknowns are never evicted by guesswork).
- `evict_duplicate_peers()` pure helper: groups live sessions by identity, keeps the
  NEWEST `connected_at` per identity (it just completed a handshake — the provably-live
  one), shutdowns and removes the older duplicates, returns one log line per eviction.
  Idempotent (a second sweep over survivors is a no-op).
- Wired at ALL THREE adopt sites (inbound accept, background dialer, `addnode onetry`
  AdoptPeer) plus a PERIODIC 60s sweep in the follow loop — the pass that heals the 18
  already-accumulated sessions on the first sweep after deploy.
- `PeerRec` gains `advertised`; `getpeerinfo` rows expose `identity` — ops/sweep views
  group sessions by it and exclude claim-less duplicates from quorum tallies.

## TESTS (+3)
1. `peers::tests::set_self_purges_existing_self_entries_from_the_store` — the DNS2
   shape: poison both fleet ports (:24113 AND :24112) BEFORE registration → set_self
   purges exactly those 2 rows, keeps a healthy row, ingest now refuses the host at
   both ports, re-registration is a no-op.
2. `node_embedded::tests::self_identities_at_reads_the_tor_hostname_files` — hostname
   file with NO onion-state.json (the DNS2 publish shape) registers the onion; the
   `tor/hostname` alternate form registers too; i2p destination still registers.
3. `node_embedded::tests::evict_duplicate_peers_keeps_one_session_per_identity_and_the_newest`
   — the fleet shape replayed: 4 sessions to ONE identity (old outbound + fresh
   outbound + TWO fresh inbounds with distinct ephemeral labels) → exactly ONE
   survives (the newest); an unidentified inbound and a distinct healthy peer survive;
   3 log lines; second sweep idempotent.

## Verification bar (live, post-deploy)
- DNS2: poisoned self rows GONE from peers.json at first boot; "skipping SELF
  endpoint" lines only if a stale store re-poisons; zero self-dial failures after.
- Fleet: no endpoint ever holds >1 live session (getpeerinfo identities unique);
  DNS2's 18-duplicate set collapses to 1 on the first 60s sweep.
- Staking/chain untouched: no recon/stake path changes; suite green.
