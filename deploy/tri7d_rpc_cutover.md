# tri7d RPC cutover — EXECUTED 2026-11-02 (DNS2)

> **POST-PHASE-6 NOTE (2026-11-02, remediation):** tri7d now also OWNS the
> P2P listener (24113, bind 0.0.0.0) with inbound handshake + getdata/
> getheaders serving + block validation + relay. Outbound attach is a
> rotation over sami-pc/DNS3/DNS3-onion (see HANDOFF.md "TOPOLOGY" — the
> authoritative description). `--daemon 127.0.0.1:24113` below is HISTORY.

## What actually happened (supersedes the plan below)

LIVE STAKING ran FIRST (legacy daemon up as validator/relay — 3 blocks minted and
accepted, see HANDOFF.md phase 6), then the cutover:

1. tri7d verified stable + all native RPCs healthy on 19114 (`at_daemon_tip` true).
2. `systemctl stop trianglesd && systemctl disable trianglesd` — the legacy daemon
   is RETIRED (autostart off; its full-index duties are covered by tri7d: 15
   native RPCs + P2P follower + stake_mint minting).
3. tri7d took over **19112** (the standard port the local stack uses) — NOT 19113
   as originally planned (the tunnel survived). Unit ExecStart:
   `--rpc-port 19112 --no-proxy` (was `--rpc-port 19114`). P2P arg unchanged:
   outbound follower to `127.0.0.1:24113` (the daemon's former listener port).
4. `--no-proxy` is REQUIRED post-cutover: with tri7d on 19112 the transitional
   proxy's target (rpc.conf rpcport=19112) would be tri7d ITSELF — self-forward
   loop. With the proxy off, proxy-only methods fail with
   "no legacy proxy configured (cutover mode)".
5. HEADLESS mode (new follow_tip capability): with trianglesd's 24113 listener
   gone, follow_tip no longer dies after 3 failed handshakes — it starts and
   serves native RPCs from persisted state (tipstate window + blockindex +
   snapshots) and retries the P2P attach every 30s poll tick. The 30s tipstate
   write cadence continues, so tri7d-health stays meaningful. Logs:
   "daemon P2P unreachable — starting HEADLESS", "headless tick: …".
6. Autossh tunnel 19113 → sami-pc:19112: UNTOUCHED (different port, no conflict;
   live consumer = local node process reading SAMI-PC's daemon).

## Ports after cutover

| port  | owner            | notes                                             |
|-------|------------------|---------------------------------------------------|
| 19112 | tri7d (loopback) | native RPC surface, `--no-proxy`                   |
| 19113 | autossh tunnel   | → sami-pc:19112; triangles-api consumer attached   |
| 19114 | free             | former tri7d listener, released                    |
| 24113 | free             | former trianglesd P2P listener; tri7d retries here |

## RPC methods that LOSE service with trianglesd down

Daemon-only reads, no native path — now answered with
`{"code":-1,"message":"native path unavailable (daemon-only read (not native
yet)) and no legacy proxy configured (cutover mode)"}`:

getrawmempool, getmempoolinfo, getstakinginfo, getnetworkhashps, getwalletinfo,
gettransaction

Everything else is NATIVE (source "tri7"). Transitional notes:
- getblock/getblockhash/getrawtransaction serve from the on-disk blockindex —
  the index froze at the last blk0001.dat append (h2204067, includes all three
  staked blocks). Heights above the frozen index = cutover-mode error until a
  future relay path advances the follower again.
- getinfo.connections reports the follower's real P2P attach state (0 while
  headless).

## Rollback

`systemctl enable --now trianglesd` brings the legacy daemon back on 24113/19112
— CONFLICT: tri7d owns 19112 now. Rollback = revert the unit port edit
(`--rpc-port 19114`, drop `--no-proxy`), restart tri7d, then start trianglesd.
Legacy was never modified.

---

# Original plan (pre-cutover, kept for reference)

State after phase 5b (2026-11-02, DNS2):

| port      | owner                          | notes                                    |
|-----------|--------------------------------|------------------------------------------|
| 19112     | trianglesd (legacy, loopback)  | untouched; tri7d is its CLIENT here      |
| 19113     | autossh tunnel pid via autossh | `ssh -L 19113:127.0.0.1:19112 hello@100.85.236.10` — forwards to **sami-pc's** legacy RPC; a local node process holds a live connection. DO NOT kill casually. |
| 19114     | tri7d native RPC               | deployed listener (loopback only, no auth, 1 MB cap) |
