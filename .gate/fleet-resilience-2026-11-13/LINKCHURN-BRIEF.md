# Link-churn smoothing + supervisor probe window fix (overlay UX)

Operator request (Sami, 2026-11-13): "do one through three" — item 3 was
smoothing the laptop's tor-link churn (links dropped at ~90s of quiet and
re-attached a minute later while perfectly synced at the fleet tip).

## Changes

1. `src/node_embedded.rs` — TRANSPORT-AWARE stale-link window. Clearnet
   keeps the tight 90s cut; tor/i2p links get 300s (STALE_LINK_SECS_OVERLAY).
   Rationale: overlay circuit/tunnel rebuilds after an idle stretch routinely
   exceed 90s even for HEALTHY links; a truly dead overlay link still drops
   within 300s and the dial rotation covers the gap. `stale_link_cut(p)`
   picks the window by transport; the stale-gate call site uses it. The drop
   reason line still names the actual window via cut.as_secs().
2. Policy regression test `stale_link_cut_is_transport_aware`: overlay peer
   at 299s idle KEPT / 301s dropped; clearnet peer at 89s KEPT / 91s dropped.
3. `src/gui/node_supervisor.rs` — `quiet_command`: the supervisor's periodic
   PowerShell/cmd probes (tor listener pid/path resolution) now spawn with
   CREATE_NO_WINDOW — previously each probe flashed a console window on the
   desktop every 6-10s (observed by the fleet cron lane on the laptop
   2026-11-14; the change landed in this lane's working tree and is credited
   here). No diagnostic change: same queries, same outputs.

## Interplay checks

- OUTBOUND_FRESH_WINDOW (240s, the dial-rotation suppressor) stays 240s: an
  overlay peer idle past 240s stops suppressing the rotation (fresh seeds
  get dialed) while the link itself survives to 300s — extra dials are
  harmless; a healthy link keeps suppressing only while it shows traffic.
- Keepalive pings still fire every POLL_SECS to every session; the gate
  still measures "peer stopped ANSWERING", just with a transport-fit window.
- Consensus untouched: peer-management UX only.

## Verification

cargo test --lib: 789 passed / 0 failed / 1 ignored (the +1 is the new
transport-aware test; the rest of the suite incl. the fleet cron lane's
additions passes unchanged); GUI featureful check EXIT_GUI=0; windows-gnu
release --bins EXIT_WIN=0 (gates.log = exactly ONE final-tree run).

FILES: src/node_embedded.rs, src/gui/node_supervisor.rs
