# Crash-on-peer-send: exit(3)/NOTIMPLEMENTED class (2026-11-13)

Any peer send failure during validation/keepalive is FATAL (exit 3,
NOTIMPLEMENTED). FOUR live instances in one day across all message types and
transports: `send smsgHave` / `send pong` / `send ping` (Broken pipe, os error
32) and `send headers` (Connection reset, os error 104). systemd auto-restart
masks it into a crash loop.

## Highest-frequency trigger
A daemon dialing its OWN advertised destination after another stack took over
the underlying port. Live shape: the actor's legacy trianglesd took DNS2's
0.0.0.0:24113 + [::1]:19112, so tri7's ztj5 i2p destination answered as a
foreign v6.2.6.7 client that resets mid-send — tri7d crashed every ~10-25 min.

## Diagnosis
`journalctl -u tri7d | grep "validation failure"` — the line immediately
before the exit names the peer and the failing send (`validation failure from
out:<endpoint> (transport <t>): send <msg>: <os error> — clean disconnect`).
Check `ss -tlnp` for who really owns the P2P/RPC ports before blaming the
peer.

## Workaround ladder
1. With the daemon STOPPED (health timer first), remove the self/foreign
   entry from peers.json (backup first, keep 0600 + owner). Addr gossip
   RE-INTRODUCES entries, so this lowers frequency only.
2. Durable fix is code-lane: treat send-failure-during-validation as a clean
   disconnect, never exit; or free the hijacked port.

## Related facts
- `addnode` is code-restricted to .onion/.b32.i2p — clearnet endpoints
  re-form only via peer-store rotation (`46.62.249.20:443` = Pi's clearnet
  hub endpoint).
- A fresh follow_tip boot: tipstate load → `state: tip` → RPC bind takes
  ~5-8 min; RPC verbs error `-1 "node ops engine still warming up"` until the
  warmup gate opens. Pre-warmup getbalance can read LOW (stale-window
  artifact) — re-probe after warmup before believing a balance drop.
- When a peer serves HANDSHAKE OK but a peer is a few blocks ahead, the
  whole chase (headers + bodies) runs BEFORE the RPC binds; budget ~5-10
  min of uninterrupted runtime for a boot with a reachable peer.

## Update 2026-11-13 ~17:5xZ — restart-loop attribution CORRECTED; two more defect layers

1. **The 5-min restart loop was OUR OWN watchdog, not an external actor.**
   tri7d-health.timer fires `*:0/5`; its check (`follow_tip --status
   --max-write-age 600`) exits rc=3 during (a) the >5-min warmup — the
   SIGKILLs invalidated the index cache (inference), so every boot rebuilt
   ~105s and tipstate.json stayed stale >600s — and (b) the DEGRADED gate
   (no peer traffic >600s + no tip advance >1800s) while the mesh was down.
   `try-restart` then SIGKILLed the daemon mid-warmup every 5 min, which
   GUARANTEED the next boot also needed the full rebuild: the loop cannot
   exit from inside. Fix applied: `systemctl stop tri7d-health.timer`
   (reversible, judged B zero blockers
   urn:ump:5ehouyhbodfdpb5744ocrasvfdmbxrn3v7bssxtqeou5p4sevy7a) — the node
   then completed a boot, chased, and MINTED (h2211157→1175 in ~30 min;
   seeds adopted via relay). RULE: a watchdog whose restart response cannot
   fix the failure mode (external mesh outage / hijacked port) only adds
   churn — gate restarts on process age > warmup window, or hold the
   DEGRADED gate during fleet-wide mesh outages; re-arm only after a manual
   `--status` run exits 0. systemd Restart=on-failure + `--forever` remain
   the real crash net.
2. **exit(4) body-stall crash-loop shape (residual; item 17 incomplete).**
   run_backfill's body phase has a 180 s deadline; `validated == 0` at the
   deadline → `Err("backfill: no blocks validated")` → finish(EXIT_PROTOCOL
   =4). Live shape 17:20:14Z: the peer served 2991 headers then its serving
   side died (see 3) — DNS2 waited 180 s on a dead session and exited 4.
   Fix shape: validated==0 at deadline = SKIP+WARN+re-request (rotate peer),
   mirroring item 17's SKIP+WARN for unwindowed bodies; exit(4) belongs to
   real protocol violations, not transport stalls.
3. **The send-failure class also lives on the INBOUND serving side**
   (non-fatal there): Pi logged `validation failure from in:194.233.88.206
   (/tri7:7.0.0/ h2211156) (transport i2p): send block: Broken pipe —
   dropping inbound peer`. When a follower's block requests go unanswered,
   check the SERVER's journal for this line before blaming the client's
   180 s timeout.
