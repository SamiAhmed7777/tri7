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
   (reversible, zero-blocker judged) — the node
   then completed a boot, chased, and MINTED (h2211157→1175 in ~30 min;
   seeds adopted via relay). RULE: a watchdog whose restart response cannot
   fix the failure mode (external mesh outage / hijacked port) only adds
   churn — gate restarts on process age > warmup window, or hold the
   DEGRADED gate during fleet-wide mesh outages; re-arm only after a manual
   `--status` run exits 0. systemd Restart=on-failure + `--forever` remain
   the real crash net. MESH NUANCE: "mesh broken" was PARTIAL — most tor
   SOCKS dials failed fleet-wide, but DNS2↔Pi paths (onion + clearnet 443)
   worked intermittently, which is what let the chase-and-mint succeed once
   the daemon got uninterrupted runtime.
2. **exit(4) body-stall crash-loop shape — FIXED 2026-11-13 (commit 5ae0701,
   grade B glm-standin urn:ump:ciblas5px7mw2wtu2uqch264fc2s2slfrnctcrik3nt5cwbgvcsa,
   bundle .gate/boot-body-stall-2026-11-13/).** run_backfill's body phase is
   extracted into `backfill_fetch_bodies` with an injectable deadline
   (BACKFILL_BODY_DEADLINE=180 s unchanged — only the OUTCOME changed); ALL
   peer-fault classes inside the phase now WARN+skip and return
   validated-so-far: `validated==0` at the deadline, getdata/pong send
   failure, pump peer-death/wire-garbage, unusable served payload (parse or
   trailing bytes). The caller logs `skipping the body phase (follow loop
   owns the chain)`; the follow loop's rotation paths (send-failure
   shutdown, empty-reply evidence, escalation dial) own the peer. ONLY a
   kernel-VALIDATION failure of a served body stays fatal — a chain
   problem, not transport (disclosed remainder; no live occurrence).
   +3 loopback regression tests (stall / peer-death / garbage), 774/0/1
   lib. Deployed: DNS2 df3ae146 (minted h2211199+ through the swap), DNS3,
   Pi 08f47108, PC GUI 06fe59f8 (welded node carries the fix).
3. **The send-failure class also lives on the INBOUND serving side**
   (non-fatal there): Pi logged `validation failure from in:194.233.88.206
   (/tri7:7.0.0/ h2211156) (transport i2p): send block: Broken pipe —
   dropping inbound peer`. When a follower's block requests go unanswered,
   check the SERVER's journal for this line before blaming the client's
   180 s timeout.

## Exit-code map (follow_tip)
2 EXIT_USAGE (bad args) · 3 EXIT_VALIDATE (validation failure incl. the
exit(3)/NOTIMPLEMENTED send-during-validation class) · 4 EXIT_PROTOCOL
(kernel-validation failure of a served backfill body — since 5ae0701 the
ONLY reachable shape; all transport faults in boot backfill skip) ·
5 EXIT_SEED (no dialable peer where required) · 130 SIGINT (clean,
resumable).
