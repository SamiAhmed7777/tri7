# From-zero sync with zero requirements (bare install self-syncs)

Operator directive: "anyone who downloads the wallet should be able to sync
from zero without requiring them" — no flags, no manual steps, no special
preparation.

Root cause of the laptop stall ("store missing block body at h0" for hours):
follow_tip's from-zero trigger was
    from_zero = flag || (!have_state && explicit_peers)
with have_state = tipstate.json exists. A fresh machine whose earlier spawn
attempts left a tipstate behind (tip h0, NO block bodies on disk) counted as
"have state" -> the node anchored genesis but never started the body fetch ->
the wallet's live seed retried forever.

Fix (src/bin/follow_tip.rs): `have_state` now also requires the blocks store
to exist AND be non-empty. A genesis-only state is treated as a bare machine:
the automatic from-zero path starts the body fetch on its own. No flag needed
for anyone; existing synced nodes are unaffected (their store is non-empty).
Also in the GUI supervisor: a fresh node dir gets --sync-from-zero passed
automatically (harmless with the daemon fix; zero user burden).

cargo test --release --lib: 484 passed, 0 failed.
Live evidence: laptop stuck at h0 with a genesis-only tipstate (quoted above);
known-good from-zero nodes (DNS2/DNS3) ran the same path via the explicit flag.
