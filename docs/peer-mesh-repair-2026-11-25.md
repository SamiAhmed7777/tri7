# Three-node peer mesh repair — 2026-11-25

Workqueue item **46 PEER-MESH-REPAIR**. Scope is DNS2, DNS3, and Pi only.
sami-pc was unreachable over SSH and is not included in the acceptance claim.

## Defect and deployed change

The published DNS2 onion pointed its local destination at port 24113, which
legacy `trianglesd` still owns. tri7d could therefore not own that listener.
The nodes also had stale/missing peer seeds, and Pi's Tor service was not
enabled at boot.

The accepted deploy made only these changes:

- DNS2 torrc: `HiddenServicePort 24113 127.0.0.1:24114`; the published onion
  and virtual port 24113 are unchanged.
- DNS2 tri7d override: listener `127.0.0.1:24114`, not public 24113.
- DNS2, DNS3, and Pi: exactly the two current peer-onion `addnode` entries
  listed in [`network-seeds.md`](network-seeds.md); all other conf bytes,
  including credentials, are preserved. Evidence records credential digests
  only, never credential values.
- All three nodes: tri7d ordered after Tor; Pi `tor.service` enabled.
- DNS2 also remains ordered after the active legacy `trianglesd`.

The legacy daemon and richlist scanner were not stopped or reconfigured.
Final readback found both active, with public 24113 owned by `trianglesd`
and loopback 24114 owned by tri7's `follow_tip`.

## Accepted live result

Deploy run `20261125T004941Z` exited **0** (`DEPLOY_ACCEPTED`). Final deploy
bytes were graded **GLM A / zero blockers**, URN
`urn:ump:zkxidhvjzeu25mi64hp3mqetbpiwqefudgqnkyale5uzbanbea2a`.

An independent RPC readback at 01:03:43 UTC established all six directed
links, each with protocol 70206 and subversion `/tri7:7.0.0/` over Tor:

| From | Live sessions to |
|---|---|
| DNS2 | DNS3, Pi |
| DNS3 | DNS2, Pi |
| Pi | DNS2, DNS3 |

DNS2 had a live inbound DNS3 session; DNS3 had a live inbound Pi session.
This proves the published DNS2 onion reaches tri7 rather than merely passing
a TCP/SOCKS probe. All three nodes reported height **2,219,211**, staking
enabled, and active/enabled `tri7d` and Tor. DNS2 also minted stake blocks
during the run; the acceptance log records the last hit and block hash.

The apply-window journal readback had zero `PARKED` and zero panic lines on
all nodes. Cold reconnects did produce one DNS2 and four Pi `verack timeout`
lines before the accepted sessions were established; those lines are retained
in the evidence rather than presented as a clean-start claim.

Evidence is held on DNS2 in
`/root/tri7/.gate/peer-mesh-repair-2026-11-24/`: `ev_deploy.log`,
`ev_post_mesh.json`, `ev_acceptance.json`, `ev_acceptance_summary.json`,
`ev_selftest.log`, `ev_preflight.log`, `PREDEPLOY_SHA256SUMS`, and the judge
outputs.

`ev_deploy.log` also contains `RuntimeError` and traceback lines from bounded
health retries while RPC is warming. That retry noise is expected and is not
treated as success; acceptance occurs only after health, all six dials, the
live peer matrix, journal checks, and matching best-block hashes.

## Why the procedure includes explicit dials and long health windows

Two earlier live attempts restored all changed files byte-exactly but exited
**2**, because their 420-second rollback health windows expired during cold
index/balance rebuilds. They are not successful deploy attempts. The final
procedure uses bounded 900-second cold-boot health windows.

Passive rotation had also left DNS3 and Pi connected only to DNS2. After
health, the accepted run issued six `addnode <peer> onetry` calls and required
each to succeed before re-reading the peer table. The sandbox self-test covers
byte-exact rollback, a genuine partial apply, failed restart, preflight abort,
transient RPC retries, all six dial calls, and rejection of swallowed failures.

Item 47 MEMORYMAX-HEADROOM and item 48 BALANCES-REPLAY-LOSER remain open;
this repair does not claim to fix their underlying cold-start behavior.
