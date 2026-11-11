# tri7 delta: dialable i2p hosts from the peer store — judge brief

## Context
Graded minutes ago as grade A (urn:ump:flykldcs7quzcizzyq6k4hb23s5nfrlvi22ae
yjbccfwjpk6hlsq): conf `addnode=` now accepts `.b32.i2p` hosts. Live
deployment exposed ONE remaining i2p-wiring gap, fixed by this delta.

## The defect this fixes (witnessed live)
`connect_and_handshake` builds its dial list from the PEER STORE first
(store peers before configured seeds, by design) but skipped every host that
does not end `.onion` — so a learned i2p destination in the peer store is
NEVER re-dialed, and i2p stays second-class in the dial path even after the
addnode fix. sami-pc log 11:10:10Z: with `--i2p ztj5…` configured FIRST in
cfg.peers, the startup dial still landed on a store-known onion because
store entries outrank seeds and i2p store entries were unconditionally
skipped. This delta makes overlay-i2p hosts dialable in the store path.

## Measured delta
- `src/bin/follow_tip.rs` ONLY; +31/-4 lines. ADDED: `dialable_store_host`
  helper (onion/.b32.i2p/.i2p true; clearnet quad / legacy hex / junk false)
  used by the store-dial filter; CHANGED: the filter call site + its doc
  comment ("the network is Tor-only" → no-clearnet wording); ADDED: test
  `dialable_store_host_accepts_onion_and_i2p_only` (7 assertions covering
  both overlays + 4 rejection classes). REMOVED: nothing else.
- `cargo test` full suite after the change: 455 lib + 42 follow_tip + rest,
  all `ok`, 0 failed (was 455+41).

## Not in scope / unchanged
- Dial ORDER stays store-first-then-seeds (BRIEF_TOR_V2 item 3 semantics
  untouched); only the eligibility filter widens.
- No cleartext dialing: quads and hex keys stay rejected (test pins it).

## Answer shape
Single JSON object, last line, nothing after:
`{"codex_grade": "A|B|C|D|F", "reasons": [...], "blocking_issues": [...],
"polish_suggestions": [...]}`
