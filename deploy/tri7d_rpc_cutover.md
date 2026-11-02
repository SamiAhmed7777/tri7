# tri7d RPC cutover plan (19113 → tri7, retiring the legacy daemon)

State after phase 5b (2026-11-02, DNS2):

| port      | owner                          | notes                                    |
|-----------|--------------------------------|------------------------------------------|
| 19112     | trianglesd (legacy, loopback)  | untouched; tri7d is its CLIENT here      |
| 19113     | autossh tunnel pid via autossh | `ssh -L 19113:127.0.0.1:19112 hello@100.85.236.10` — forwards to **sami-pc's** legacy RPC; a local node process holds a live connection. DO NOT kill casually. |
| 19114     | tri7d native RPC               | deployed listener (loopback only, no auth, 1 MB cap) |

## Method coverage at cutover candidate state

NATIVE (source "tri7", served from tipstate + blockindex + replay snapshots):
gettip, getblockcount, getblockhash, getblock (hash|height), getrawtransaction,
getinfo, getdifficulty, getconnectioncount, getstakemodifier, getbalance,
listunspent, listwalletaddresses, getreceivedbyaddress, validateaddress, health.

PROXY-FALLBACK remaining (source "legacy-proxy", only when native data
can't answer): getrawmempool, getmempoolinfo, gettransaction, getstakinginfo,
getnetworkhashps, getwalletinfo (daemon-only reads); getblock/getblockhash/
getrawtransaction while the index warms (~90s after a cache-less start; the
on-disk caches make later restarts instant); getblockhash for heights above
the indexed tip if the chain advances past the build's snapshot.

## Cutover steps (when Sami gives the go)

1. Confirm parity: run the validation sweep (gettip/getbalance/getblock/
   getrawtransaction/getreceivedbyaddress vs the daemon) — recorded in this
   phase's report; re-run against fresh data.
2. Retire the sami-pc tunnel consumers: identify what connects to 19113
   (`ss -tnp | grep 19113`), repoint those consumers to tri7d on 19114 (or
   directly re-create their tunnel).
3. Stop the autossh tunnel unit (find it: `ps -o cmd -p <autossh-pid>` and its
   systemd/cron parent).
4. Reclaim the port: edit /etc/systemd/system/tri7d.service ExecStart
   `--rpc-port 19114` → `--rpc-port 19113`, `systemctl daemon-reload &&
   systemctl restart tri7d`.
5. Now tri7d serves on 19113. Legacy 19112 stays up until tri7d proves stable
   for the chosen soak period, then `systemctl stop --now trianglesd` is the
   final step (NEVER before Sami's explicit go).
6. Rollback at any point: revert the port edit, restart tri7d, restart the
   autossh tunnel — legacy was never modified.

## Snapshot refresh procedure (when the chain advances)

The wallet RPCs serve the phase-1e replay snapshots (h2201090). To refresh:

`cargo build --release --bins && ./target/release/balances   /root/.triangles/blk0001.dat 2201090 --allow-stale   --wallet /root/.triangles/wallet.dat --utxos-out /root/tri7/wallet_utxos.json`

NOTE: generate into /root/tri7/wallet_utxos.json (the installer's step 5b
copies it to /var/lib/tri7/); writing directly to the destination would race
the copy step.
(positional: archive, end-height; flags: --allow-stale, --wallet <wallet.dat>,
--utxos-out <path> — see src/bin/balances.rs main for the full flag list),
then re-run this installer's step 5b copies and restart tri7d. The block/tx index rebuilds automatically
when blk0001.dat grows (size mismatch invalidates the caches; ~90s rebuild,
indexed heights above the build's tip fall back to the proxy until then).
