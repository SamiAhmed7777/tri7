The exact §16 bullet to be appended to the tri7-fleet-chain-state skill (after
the item-19 bullet, keeping the reverse-chronological shape):

- (item 21, 2026-11-11) **A peer that dialed IN is polled by NOBODY — its
  branch used to be invisible to your judge forever.** Tips travel only in
  getheaders replies, and the 30 s poll tick went to outbound peers only
  (live: the PC dialed into DNS2 at an equal height, solo-minted a ~1.42x-
  heavier branch, and DNS2's judge never saw it for ~40 min; the 05:1xZ
  heal worked only because a manual i2p addnode gave DNS2 an outbound
  dial). Fixed in code: every live peer (inbound included) now receives
  the tip poll, each peer carries its per-poll CLAIMED tip (getpeerinfo
  `claimed_tip_height`/`claimed_tip_hash` — refreshed on every NON-EMPTY
  headers reply; an empty reply intentionally retains the preceding claim,
  which is exactly what the divergence verdict measures), and a claim that
  DIVERGES (a taller tip whose hash is neither indexed nor queued as a
  pending body, or a different hash at a height we hold) fires the
  window-locator recon on ONE empty reply (reason tag "claim-divergence",
  same 60 s cooldown + echo guards as the other triggers). OPS FACT for
  diagnosis: getpeerinfo's claimed_tip is the live branch claim to compare
  against getblockcount; start_height is handshake-frozen and worthless
  for fork growth. Second half (r1 judge blocker): after adopting an
  inbound peer's branch, the body fetch must reach the peer that SERVED
  the headers — the recon advertiser is now the preferred getdata target
  (outbound preferred, any-live fallback only when it is gone); the old
  outbound-only dispatch would have left an adopted branch unfetchable
  because the outbound set is exactly the peer group the heavier branch
  beat. `stats.inbound_polls` (success-only) proves the inbound polls
  flow on a node; if it stays 0 while an inbound peer is connected, the
  binary predates this fix.
