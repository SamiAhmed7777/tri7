# Round 2 — the same fix, with the five blockers addressed

Blockers from round 1 and what changed:

B1 "Honor the address index's `complete` flag; do not treat a merely near-tip
index as complete."
  -> The daemon now computes an explicit verdict and ships it:
     `complete` = addrindex::is_complete(), and
     `authoritative` = complete && index_height.saturating_add(1) >= chain_tip.
     The one-block slack covers the race where the tip advances between reading
     the tip and the index applying that block; there is no magic tolerance.
     rpc.rs serves the live set ONLY when `authoritative` is true.

B2 "Add regression tests for an incomplete index at the tip, every relevant
lag boundary, and a valid live index when the replay snapshot is unavailable."
  -> Five tests now: (a) the live index wins over the snapshot;
     (b) the lag boundary exactly (one block behind = authoritative, two
     behind = not); (c) an index that is still BUILDING is never served as
     authoritative; (d) the snapshot fallback is flagged partial;
     (e) a valid live index answers even when there is NO replay snapshot.
     Plus (f) with neither view the node errors instead of answering 0.

B3 "Decide and implement safe source selection when the live index is
incomplete; getbalance and listunspent must not silently present an
incomplete set as authoritative."
  -> getbalance and listunspent serve the live set only when it is
     authoritative; otherwise they fall back to the replay snapshot (and
     getwalletinfo flags the answer partial). The half-built index is never
     presented as the wallet's spendable set.

B4 "Allow getwalletinfo to report live-index data without requiring a replay
snapshot, or document and justify that dependency."
  -> getwalletinfo no longer requires the snapshot: it reports the live
     figure (with its own coverage height) when the node has one, and only
     errors when neither a live index nor a snapshot exists.

B5 "Replace the GUI's directional promise with neutral wording because
synchronization can increase or decrease a balance."
  -> The notice now says the figure "can change in either direction as it
     syncs".

Polish applied: the `idx_h + 8` tolerance is gone (saturating_add(1) with an
explicit comment); index_height is treated as an Option (no silent fallback
to the replay coverage); the evidence file now carries the reproducible
commands and the revision id.

`cargo test --release --lib`: 504 passed, 0 failed, 1 ignored.
