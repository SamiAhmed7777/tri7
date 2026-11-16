# ROUND 2 DELTA — DNS2 outbound bridge to sami-pc, re-routed via i2p addnode

## Relationship to round 1

Round 1 (grade B, urn:ump:tq7xkbxskbfkwxttk2fsdpajoqgrfpruydhmtcp7jqrwuen5ez3q)
graded a one-line change: sami-pc tailscale addnode port 24112->24113 in
/root/.triangles/triangles.conf. The change was APPLIED, DNS2 restarted, and the
restart exposed two facts that make round 1's mechanism INEFFECTIVE:

1. [E10, DNS2 journal, new PID 3306856 at 05:38Z, verbatim]
   "addnode=100.85.236.10:24112 ignored: only .onion / .b32.i2p addnode hosts
   are accepted — no cleartext P2P"
   The current daemon binary rejects ALL cleartext-IP addnodes. No port number
   can make an IP addnode work. (The skill's 2026-11-07 note that the binary
   "retains a direct-IP dial path" is stale for this v7.0.x binary.)
2. [E11] The conf the daemon actually reads/persists is
   /etc/tri7/triangles.conf — the staking RPC answered
   {"persisted_to":"/etc/tri7/triangles.conf","staking":true} after the round-1
   restart. /root/.triangles/triangles.conf (the file round 1 edited) is not
   the daemon's conf. Round-1's edit was reverted byte-for-byte to conf-before
   (verified by diff) — no no-op line left behind.

## The round-2 change (THE delta being graded)

Add ONE line to /etc/tri7/triangles.conf (the real conf):

```
--- /etc/tri7/triangles.conf
+++ /etc/tri7/triangles.conf
 addnode=lfuojjpstasun27n77o2637pmsd3dpkrpgyeuiwijduxrfmoyb2a.b32.i2p:24112
+addnode=mb3ox52uzwh7d7u6ct6mdapcsyqcxiffz53hlydwsr7cwhvys5za.b32.i2p:24113
 noantispam=1
```

(an i2p addnode — the accepted host class — pointing at sami-pc's published
i2p destination; the PC's getlocalinfo publishes exactly this b32 with port
24113, and its i2pd.exe is running)

No other line changes. Same procedure as round 1: stop tri7d-health.timer,
restart tri7d, start tri7d-health.timer, verify staking (conf carries stake=1;
re-assert via RPC if the ops engine was still warming), then the verification
plan below. Rollback: delete the added line, restart.

## New evidence for the endpoint choice

- [E12, live probe from DNS2, 05:4xZ] Through DNS2's own i2pd SOCKS proxy
  (127.0.0.1:4447): `SOCKS5 connect to
  mb3ox52uzwh7d7u6ct6mdapcsyqcxiffz53hlydwsr7cwhvys5za.b32.i2p:24113
  (remotely resolved) ... SOCKS5 request granted.` — DNS2's i2pd established
  the i2p connection to the PC's server tunnel. The PC runs i2pd.exe (pid
  3172) and its getlocalinfo publishes this exact b32:24113.
- PC onion tuu7h65j…onion:24113 remains DEAD from DNS2's tor (curl 28 timeout,
  round-1 E6) even though tor.exe runs and the hostname files agree — so the
  onion addnode route is not viable today. Diagnosing the PC tor HS backend is
  a logged follow-up, not part of this change.
- The i2p path also has a live precedent in the reverse direction: sami-pc's
  judge reconciles against DNS2's i2p endpoint ("empty tip-poll reply (streak
  trigger) while out:ztj5lcg4…b32.i2p:24113 claims h2209490…" in round-1 E2)
  — i2p peering between these two hosts demonstrably carries the protocol.

## Why this restores the round-1 goal (unchanged incident background)

The fleet split at h2209485 stands (round-1 brief/E1/E2 still accurate; DNS2
canonical branch now at h2209506, PC solo branch at h2209512 and pulling
further ahead). DNS2's judge is blind to the PC branch because tips travel
only inside getheaders replies on POLLED (outbound) peers, and DNS2 has zero
working outbound peers except its dial to the Pi onion (ra32xf5og…:24112).
Giving DNS2 a working outbound dial to the PC lets its item-18 judge fetch the
rival branch, compute the work comparison itself, and adopt the heavier branch
(the PC's, ~1.42x above the fork). The conf carries stake=1, so staking
survives restarts; the staking=0/forcestaking=0 legacy keys in the same file
are ignored by the daemon (running process stakes with enabled:true while they
read 0 — measured pre-restart in round-1 E3/E4).

## Verification plan (fail-visible, same as round 1)

- DNS2 journal shows the b32 dial succeeding (peer appears; getpeerinfo gains
  an outbound i2p peer whose endpoint is mb3ox52u…b32.i2p:24113).
- DNS2 journal shows reconciliation against the PC's claim, then either an
  "IS heavier" adoption (rollback_tip, re-advance along PC-branch hashes) or
  an explicit NOT-heavier verdict (which would falsify the work analysis —
  STOP and report, no further action).
- Four-way parity re-probe at the shared minimum tip (DNS2/Pi/DNS3/sami-pc).
- DNS2 staking enabled:true with attempts growing.

## Risk assessment (unchanged from round 1)

~60-90 s DNS2 downtime per restart; no store/wallet/tipstate/binary touched;
worst case the split continues exactly as before the change. The i2p addnode
persists across the PC's ~10-min restart churn (DNS2 re-dials; the PC's own
listener survives its daemon restarts).

## Secrets hygiene

DNS2's rpcpassword exists in /etc/tri7/triangles.conf and is REDACTED from
every file in this bundle (conf-etc-redacted.conf shows the full line set with
the password masked). The RPC is loopback-only (rpcallowip=127.0.0.1).
