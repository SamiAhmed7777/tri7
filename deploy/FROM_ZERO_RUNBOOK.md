# From-Zero Sync Runbook (tri7d)

Reference implementation for every future fresh install. One command takes a
bare machine to a fully-synced staking-capable node with a brand-new wallet.

## Preconditions

- linux x86_64, root, ≥10 GB free disk (chain store ≈ 1 GB at h2.2M; index
  caches ≈ 200 MB; leave headroom for growth), ≥2 GB RAM (full-history
  validation window ≈ 600 MB at h2.2M).
- network reachability to at least one live peer's P2P port (24113).
  Tailscale works; public IP works; NO tor required.
- the repo built for the target: `cargo build --release --bin follow_tip`
  (build on the machine or cross-compile; the binary is static-enough for
  glibc-matched distros — deploy script copies from ./target/release).
- NOTHING else: no snapshot, no blk0001.dat, no tipstate, no wallet.

## One command

```bash
sudo ./deploy/tri7d_fresh_install.sh --peer 100.121.150.22:24113 --stake
```

What it does: user + dirs → installs the binary → generates
/etc/tri7/triangles.conf with fresh RPC creds (kept if it exists) → writes
tri7d.service (+ healthcheck timer) with `--sync-from-zero --new-wallet
/var/lib/tri7/wallet.dat` → enables and starts the unit. The sync runs inside
the unit; the node then keeps following the tip forever.

Repeatable `--peer HOST:PORT` flags build the outbound rotation (the from-zero
fetch pool opens `--sync-workers` (default 4) connections to the peers in
round-robin).

## How from-zero works (what to trust)

1. **Genesis anchor** — block 0 is fetched from the peer and its Hash9 must
   equal the compiled-in `hashGenesisBlockOfficial` constant. Nothing is
   accepted before that.
2. **Header walk** — getheaders from the tip locator, link-checked, to the
   peer tip (2.2M headers ≈ minutes).
3. **Parallel body fetch, sequential validation** — N fetcher sessions pull
   500-block getdata batches (stride-assigned so validation always has the
   next window in flight); ONE validator thread runs every block through the
   SAME acceptance gate as live traffic: Hash9, prev-link, header agreement,
   merkle, median-time-past, nBits EMA (AcceptBlock cutoff respected), PoW
   target / PoS kernel (full-history window: every coinstake kernel is
   ENFORCED, none skipped), block signature, tx finality.
4. **Durability** — accepted blocks append to `<state-dir>/blk0001.dat`;
   tipstate persists every 1000 blocks and on SIGINT/SIGTERM (exit 130).
5. **Tip takeover** — when a fetch round returns nothing new, the node is at
   the peer tip and the normal follow loop takes over seamlessly.

## Monitoring

```bash
journalctl -u tri7d -f | grep 'from-zero:'
# from-zero: h482000 0000000000...  (103 blk/s, ETA 4.6h)
/opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7   # health JSON
curl -s -u user:pass -d '{"jsonrpc":"1.0","id":0,"method":"getblockcount","params":[]}' \
     http://127.0.0.1:19112/          # live tipstate height mid-sync
```

## Expected duration (measured DNS3→DNS2, 2026-11-03)

Throughput is bound by the PEER's getdata serving rate (~25-30 blk/s per
connection on a tri7d primary; scales roughly linearly with worker count up
to the peer's CPU/loop limit). 4 workers: ~100 blk/s → **~6 h** for 2.2M
blocks; single worker ~20 h. Faster peers (or a peer running a build whose
inbound getdata serves whole inv batches per loop tick) shorten this
proportionally. Validation itself stays well under the fetch rate.

## Resumability

- State persists every 1000 blocks + on SIGINT/SIGTERM. `systemctl restart
  tri7d` or re-running the installer continues from the persisted tip (the
  full-history kernel window is rebuilt as the sync re-walks nothing — bodies
  below the tip are already in the local store; only blocks above the tip are
  fetched).
- Never delete tipstate.json to "retry" — that discards a correct chain walk.
  Delete BOTH tipstate.json and blk0001.dat only for a true do-over.

## Completion criteria (verify hard)

```bash
# heights match and advance on both nodes
follow_tip --status --state-dir /var/lib/tri7     # height H
# on the reference node: getblockcount -> also H (both advance ~1 blk/15s)
# tip hash identical:
curl -s -u u:p -d '{"method":"getbestblockhash","params":[]}' http://127.0.0.1:19112/
# random mid-chain block identical (raw bytes):
curl -s -u u:p -d '{"method":"getblock","params":["<hash@H/2>", false]}' http://127.0.0.1:19112/
# wallet keys visible:
/opt/tri7/bin/read_wallet /var/lib/tri7/wallet.dat
```

## Fleet notes

- sami-pc / samihost refresh: same script, different `--peer` list. Do NOT
  reuse wallets across machines — `--new-wallet` generates fresh keys.
- tridock / tri-pi installers should call tri7d_fresh_install.sh (or embed the
  same flags) rather than snapshot-copying chain data: the from-zero walk is
  the trust anchor and doubles as an independent full-chain validation.

## Implementation grade

The from-zero sync implementation (follow_tip.rs, replay.rs, kernel.rs,
difficulty.rs, stake.rs, stake_mint.rs, this installer + runbook) was judged
grade B, zero blockers:
urn:ump:nomiryryilq5ll2gzhvhwlayojp4y7y7nwdvgx6jx5khglxkggra
(judge round tri7-from-zero-sync; content landed in the master history under
the CI-fix commits that swept in concurrent work).
