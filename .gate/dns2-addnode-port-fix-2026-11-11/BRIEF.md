# DNS2 addnode port fix — restore outbound visibility of sami-pc's heavier chain branch

## What is being graded

A ONE-LINE live-ops config edit on DNS2 (`/root/.triangles/triangles.conf`, line 24):
the sami-pc tailscale addnode port `24112` -> `24113`, followed by a systemd
restart of `tri7d` on DNS2. No binary change, no store/wallet file touched, no
code change, no commit.

Kind: deploy (live fleet config change + bounded daemon restart).

## The incident this fixes (all evidence measured live 2026-11-11 05:11-05:40Z)

The fleet split into two branches at h2209485 (last shared block 0db6317a…):

- Canonical branch (DNS2 + Pi + DNS3, mutually agreeing): tip h2209493 =
  665cb7a4…, 8 blocks above the fork, total work **3.429674e24**.
- sami-pc solo branch: tip h2209493 = a31d9eaa…, 8 blocks above the fork,
  total work **4.886305e24** — ~1.42x heavier, and minting faster (~1 blk/2.3 min
  solo at ~6.1e23 work/blk vs canonical ~1 blk/4-5 min at ~4.3e23 work/blk).

Per the chain's own fork-choice rule (heavier total work wins, item-18 judge,
live-proven), sami-pc's branch is the rightful winner. sami-pc's judge agrees:
it polls DNS2 outbound (via DNS2's onion), fetched the canonical competitor
blocks, computed 3.43e24 < 4.89e24, and correctly ignored the lighter branch
(PC state2.log: "recon: competing branch h2209486..h2209493 (7 blk, work
3429674055254025890168832) is NOT heavier than ours h2209493 (work
4886304598023397301026816) — ignored, reconciliation closed", repeated every
~90 s since 05:03Z).

The split can never self-heal because visibility is asymmetric: tips travel only
inside getheaders replies, and DNS2's poll/recon machinery engages only its
OUTBOUND peers. sami-pc arrives at DNS2 only as an INBOUND tunnel/onion peer
(DNS2 getpeerinfo: 3x inbound 127.0.0.1 clearnet + 1x outbound tor to the Pi
onion). DNS2's journal in the last 60 min shows reconciliation lines ONLY
against the Pi onion — zero awareness of the PC's rival branch. DNS2 keeps
minting lighter blocks on a branch that will be discarded the moment adoption
happens; every hour of delay grows the discarded set.

## Why the port fix restores visibility (measured, not assumed)

- DNS2 conf line 24: `addnode=100.85.236.10:24112` (sami-pc tailscale IP).
- Live probes from DNS2 to 100.85.236.10:
  - port 24113: **OPEN** (TCP connect succeeded)
  - port 24112: **Connection refused**
  - PC onion tuu7h65j…onion:24113 via tor socks: **timed out** (separate defect,
    NOT fixed by this change; logged as follow-up)
- sami-pc's daemon demonstrably listens on 24113 (its tor HS maps 24113; its
  published i2p destination is also :24113; its outbound peer dial to DNS2 uses
  the same daemon version with p2p 24113).
- DNS2 currently has ZERO outbound tailscale peers — all three tailscale
  addnode targets fail (the PC one on the wrong port), so DNS2's tip-poll loop
  only ever reaches the Pi onion.

After the fix, DNS2 dials sami-pc outbound on tailscale 24113, tip-polls it,
receives the PC-branch headers above the shared ancestor, and its own
item-18 judge compares work: 4.89e24 (PC) > 3.43e24 (ours) -> rollback_tip to
h2209485 and adopt the PC branch. DNS2 then serves the PC branch to Pi/DNS3
through their existing channels; the fleet converges on the heavier chain.
Canonical-side stakes minted above h2209485 return to immature and restake
(losing stakes return when the branch resolves — established fleet behavior,
2026-11-10 04:22Z event). Operator balances are intact: sami-pc getbalance
5110.900381 TRI (the standing number) read DURING the split.

## The exact change

```
--- conf-before.conf (line 24)
+++ conf-after.conf (line 24)
-addnode=100.85.236.10:24112
+addnode=100.85.236.10:24113
```

No other line changes. Backup saved as conf-before.conf in this bundle.

## Restart procedure (bounded, with the known DNS2 pitfalls applied)

1. `systemctl stop tri7d-health.timer` — its 5-min run cancels stop jobs
   (documented 2026-11-08 pitfall).
2. `systemctl restart tri7d` (Restart=always unit; SIGTERM grace may exceed the
   stop timeout and read "failed" — the documented normal shape, not a crash).
3. `systemctl start tri7d-health.timer`.
4. Re-assert staking via RPC: POST {"method":"staking","params":["on"]} — the
   staking RPC takes the STRING on/off (documented pitfall); then verify
   getstakinginfo enabled:true and attempts growing.
5. Verify tip intact immediately after restart (h2209493, no store surgery
   anywhere — tipstate.json untouched).

## Verification plan (fail-visible)

- getpeerinfo gains an OUTBOUND peer with endpoint 100.85.236.10:24113.
- DNS2 journal shows recon against the PC and either "IS heavier" adoption
  (rollback_tip + re-advance along PC-branch hashes) or an explicit lighter
  verdict (which would falsify the work analysis — then STOP and report).
- Four-way parity re-probe: same hash at the shared minimum tip across
  DNS2/Pi/DNS3/sami-pc.
- DNS2 staking: enabled:true, attempts growing, last_hit fresh.

## Rollback

`cp conf-before.conf /root/.triangles/triangles.conf && systemctl stop
tri7d-health.timer && systemctl restart tri7d && systemctl start
tri7d-health.timer` — restores the pre-change conf byte-for-byte. The conf edit
does not touch store/wallet/tipstate, so rollback cannot lose chain data.

## Risk assessment

- Restart window ~60-90 s; DNS2 mints ~1 blk/4-5 min, so at most one mint
  opportunity is lost; the branch it would have minted onto is the doomed one.
- No store, wallet, tipstate, or binary is modified. The PC's own judge and the
  item-19 evidence machinery are untouched.
- Worst case if adoption does not fire: the outbound PC peer REMAINS (persistent
  visibility), the split continues exactly as now, and the finding is reported
  with the falsified-work note. No state is worse than pre-change except ~90 s
  of DNS2 downtime.

## Scope boundaries (explicitly NOT changed)

- Pi (100.98.140.83:24112) and DNS3 (100.106.76.60:24113) addnode lines: UNTOUCHED.
- No repo commit, no binary deploy, no other node touched.
- sami-pc's dead onion/i2p endpoints and the inbound-peer-claims asymmetry
  (a daemon should reconcile against inbound peers' claimed tips too) are
  logged as follow-up items, not fixed here.
