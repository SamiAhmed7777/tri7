# Samihost tri7d deploy notes (2026-11-03, uncommitted)

Deployed: follow_tip (commit 7d9ac22, sha256 db60e183...) as FOLLOW-MODE
follower on Samihost (194.163.161.162 / tailscale 100.98.140.83).

## Layout
- /opt/tri7/bin/{follow_tip,read_wallet,tri7_wallet_gui} (root-owned 0755)
- /etc/tri7/triangles.conf — FRESH rpcuser/rpcpassword (NOT DNS2's), 0640 root:tri7
- /var/lib/tri7/ — tipstate seed (h2204068 from DNS2), balances.json,
  wallet_utxos.json, blk0001.dat (frame-complete snapshot of DNS2 chain,
  sha256 in /var/lib/tri7/BLK_SNAPSHOT.sha256), index/
- Units: tri7d.service (follow-mode: onion + tailscale targets, tor SOCKS
  127.0.0.1:9050, --no-listen, --backfill 0, RPC 19112 loopback, --no-proxy),
  tri7d-health.{service,timer} (5 min; advance gate widened to 7200s)

## KEY OPERATIONAL FACTS
- RPC: `curl -u $(grep ^rpcuser= /etc/tri7/triangles.conf | cut -d= -f2):...`
  getblockcount/getbestblockhash/getdifficulty/getbalance work natively.
  getblock works BY HASH within the threaded range; by HEIGHT and for blocks
  above threaded_tip it errors (height map uncovered — same as DNS2; honest).
- NO STAKING: follow_tip cannot mint (no stake_mint shipped); conf staking=0.
  Samihost wallets are DIFFERENT key sets from DNS2's 248-key staker:
  /root/.triangles/wallet.dat = 101 keys (original Samihost wallet),
  /root/.triangles-samihost/wallet.dat = 310 keys (moved from DNS3).
  0/5 sample-address overlap vs DNS2's 248 on every wallet.
- WHY --backfill 0: DNS2's collect_headers resolves the locator by hash but
  walks the header range by HEIGHT MAP, which is uncovered (blk-file seams,
  heights_ok=false) -> serves 0 headers -> follower idles -> DNS2 90s
  quiet-drop -> outbound close is fatal (exit 4) -> restart loop. UPSTREAM
  FIX NEEDED (see tri7d.service comment). Tip advancement flows via DNS2's
  inv relay on mint; 30s poll ticks keep the link warm.
- Tip advancement beyond h2204068 is BLOCKED FLEET-WIDE: stake_mint c5
  aborted (exit 3) because live-tail blocks h2204065..68 (c1..c4 pushes)
  are not in the archive and DNS2's recent cache was lost on restart.
  stake_mint requires offline tail reconstruction from the archive.
- Legacy Triangles client DELETED from Samihost (2026-11-03, mandate):
  binaries /usr/bin/trianglesd{,-cli} wrappers, /root/trianglesd-patched,
  /usr/lib/cryptographic-triangles (407MB real install), trianglesd.service
  + drop-in, and chain caches in both legacy datadirs (blk, utxo-snapshot,
  rocksdb, txleveldb, smsgDB 10.9GB (!), smsgDB.old, tor/i2p data, logs).
  PRESERVED: all wallet* files + database/ (BDB wallet env) in
  /root/.triangles and /root/.triangles-samihost. ~13GB freed (90%->83%).
- Flooding evidence (Sami's hypothesis CONFIRMED, hammering form):
  36.9MB debug.log = 363,458 lines, 156,783 error-class lines,
  123,439 SecMsgDB corruption-retry loop lines (smsgDB/000306.sst size
  mismatch), 4 embedded-tor exit/restart cycles, 127 self-connects,
  repeated inbound getblocks storms from nawqqoaz...onion, peer version
  claimed blocks=2214400 (divergent legacy tip). Extract:
  Samihost /root/tri7d-legacy-evidence-2026-11-03.txt,
  DNS2 /root/samihost-legacy-triangles-evidence.txt.
- DNS2's own conf/evidence files contain its rpcpassword in cleartext
  (pre-existing phase6 evidence) — rotation is Sami's standing-call.
