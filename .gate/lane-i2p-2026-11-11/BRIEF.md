# tri7 lane: i2p silent peer removal — root-caused + fixed (keepalive + loud removal reasons)

OPEN DEFECT: a proven i2p handshake vanished from getpeerinfo ~2 min later, no drop/fail log. ROOT CAUSE (by inspection): tri7 ANSWERS pings but never SENDS one; `last_traffic` refreshes only on inbound messages; an at-tip i2p session is structurally silent (empty headers replies at tip, no addr pushes); i2pd tunnel churn is a BLACKHOLE — writes into the SOCKS proxy keep succeeding, reads return WouldBlock/empty, so no error ever fires; the 90s stale gate then pruned the peer. The prune DID log but with no reason/transport context, and the gap-getdata removal path was fully silent.

FIX: (1) keepalive — every 30s poll tick sends a BIP0031 ping to every peer (inbound+outbound): answered links carry inbound pong traffic and never cross the stale gate; dead tunnels become measurable (90s of ignored pings). (2) every removal path now logs reason + transport (inventory in the lane diff: recon no-fork-point, getheaders/gap/block getdata send-failures — the gap path was the one fully silent dead-mark — protocol failure, validation failure, stale gate with idle secs/msgs_in/connected_utc via new stale_link_reason()). src/p2p.rs +8: build_ping (own crate, no new dep). Consensus untouched.

TESTS: i2p_idle_session_kept_under_window_dropped_only_with_logged_reason (kept under 90s; past it, removed only WITH a reason naming transport+label; clock skew saturates kept), keepalive_ping_answered_by_pong_refreshes_an_idle_i2p_session (real local-socket ping→pong roundtrip).

PARENT-VERIFIED (fresh run in the lane tree, own target dir): `cargo test --lib` → `test result: ok. 657 passed; 0 failed; 1 ignored` (655 base + 2 new).

KNOWN LEFTOVER (disclosed, not touched): accept_handshake hardcodes Transport::Clearnet (node_embedded.rs:11257) so DNS2-side INBOUND i2p rows mislabel as clearnet — follow-up, not a removal-path defect.

TREE: branch lane/i2p-lifetime, worktree /root/tri7-lanes/lane-i2p, base be3cd3c; diff scoped to src/node_embedded.rs + src/p2p.rs (lane.diff). Repo lane only; live fleet verification (sami-pc↔DNS2 session holding past 2min) remains for the parent's deploy step.
