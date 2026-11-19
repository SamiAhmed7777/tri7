# item 34 r2 — incremental refinement from the LIVE r1 deploy (DNS2, 2026-11-18 00:09-00:18Z)

r1 (commit b6a5770, GLM grade A urn:ump:ogybt6yzoxtaoir57enyuxtj2fo2wr5q3746cdc7hx3xbiohkg7q)
deployed clean and the live fleet immediately taught two refinements:

## Live finding 1 — residual self-dial lane (the seed list)
r1 filtered the STORE walk in connect_and_handshake. Boot log 00:10:03-00:10:09Z:
`connect mb3ox52u….b32.i2p:24113 failed (SOCKS 4)` then `connect gxvrhv3q…:24113 failed
(SOCKS 1)` then `handshake ztj5lcg4…:24113 failed: peer closed connection` — DNS2's own
conf/argv ATTACH TARGETS (its own onion + i2p destination) rode the unfiltered
`for t in &cfg.peers` seeds walk: a second self-dial lane r1 did not cover.
FIX: the same self-ident filter on the seed walk ("seed list: skipping SELF target").

## Live finding 2 — r1's cross-direction eviction PING-PONGED
r1 evicted across directions (newest wins). DNS2 00:13:28→00:18:08Z, 7 evictions in
5 minutes, strict alternation: our dialer rebuilt the outbound right after an inbound
won → inbound evicted the outbound → the peer re-dialed → outbound evicted the inbound
→ … (~1 eviction/min, every eviction killing a healthy session). An inbound + an
outbound to the SAME peer is the natural overlay mesh shape; the DEFECT was
same-direction accumulation (18 concurrent INBOUND sessions, per-CONNECTION labels).
FIX: dedupe key (direction, identity) — one session PER DIRECTION per identity.
Cross-direction pairs are allowed and never churn.

## Tests updated (same test, r2 semantics)
- 4 same-identity sessions → newest OUTBOUND + newest INBOUND survive (2 evictions,
  one per direction); unidentified + distinct peer untouched; second sweep idempotent.
- Suite re-run: lib 864/0/1 (full), targeted filters green.

Deploy targets unchanged: DNS2 + DNS3 (x86_64), Pi (on-device aarch64) — r2 rebuilt
and redeployed over r1 everywhere before verification.
