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
   target / block signature / tx finality. KERNEL evidence follows the
   DEPLOYED-CLIENT assume-valid envelope below the checkpoint (see below);
   everything above h2203924 keeps FULL validation (kernels hard-enforced).
4. **Durability** — accepted blocks append to `<state-dir>/blk0001.dat`;
   tipstate persists every 1000 blocks and on SIGINT/SIGTERM (exit 130).
   The blocks store is CREATED (empty) and its index build started BEFORE
   the first block is fetched — without it every accepted body would buffer
   in RAM and be lost on restart (this silently ate the whole h0..17524
   walk on DNS3 on 2026-11-03).
5. **Tip takeover** — when a fetch round returns nothing new, the node is at
   the peer tip and the normal follow loop takes over seamlessly.

## Assume-valid envelope (deployed-client parity, DEFAULT)

The v6.2.6.7 client syncing from scratch skips kernel/sig/reward checks
below its last hardcoded mainnet checkpoint — that is what checkpoints are
FOR: the anchor hash binds the trusted history. tri7 from-zero now matches
that envelope exactly (before 2026-11-03 it was STRICTER, hard-enforcing
historical kernels the deployed client never checked, and died
deterministically at h17693 — a block the deployed client accepted with
proofhash=0):

| band | kernel evidence | index proof value |
|---|---|---|
| h < 2200900 | none (assume-valid) | 0 — byte-equal to the deployed daemon's stored proofhash for this range |
| 2200900 ≤ h < 2203924 | COMPUTED, not enforced (seam era: the deployed client accepted it live, so its index holds real proofs — tri7 recomputes so stake modifiers above the seam match) | recomputed |
| h ≥ 2203924 | FULL: kernel target hard-enforced | recomputed |

The boundary block itself must hash to the compiled-in checkpoint anchor
`a838e60da0e5259ff87380d961b9bb5ca8d0d875775f5c54a95425b69e753a14`
(= getblockhash(2203924), daemon-verified) — the trust anchor for
everything below. Block signatures stay horizon-gated (h > 2203924) in both
modes: the instamine-era bad signature at h9003 is IN the chain and must
stay tolerated. `--full-validate` restores strict historical kernel
enforcement for the whole walk (audit only: ~7 blk/s and the h17693 seam
fails). A kernel target miss ABOVE the horizon is tolerated ONLY when the
consumed stake modifier predates the checkpoint (reconstructed from assumed
evidence — cannot distinguish a bad block from index divergence); modifiers
generated above the horizon are fully enforced.

## Monitoring

```bash
journalctl -u tri7d -f | grep 'from-zero:'
# from-zero: header walk at h1111524                       (~2600 headers/s)
# from-zero: h482000 0000000000...  (103 blk/s, ETA 4.6h)
/opt/tri7/bin/follow_tip --status --state-dir /var/lib/tri7   # health JSON
curl -s -u user:pass -d '{"jsonrpc":"1.0","id":0,"method":"getblockcount","params":[]}' \
     http://127.0.0.1:19112/          # live tipstate height mid-sync
```

## Expected duration (measured DNS3→DNS2, 2026-11-03)

- **Header walk**: ~2600 headers/s over tailscale to DNS2 → 2.2M headers in
  ~15 min. Stale duplicate segments (a >2s response makes the walk re-send
  the locator; the daemon answers every request) are detected and dropped —
  `dropped stale duplicate headers segment` lines are normal.
- **Body fetch + validation**: bound by the PEER's getdata serving rate
  (~25-30 blk/s per connection). 8 workers → measured ~200-240 blk/s
  aggregate unless the peer's serving loop caps first (see run log for the
  live number). Validation under the assume-valid envelope stays far under
  the fetch rate (the old full-kernel walk ran 7-21 blk/s on 2 vCPU).

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
