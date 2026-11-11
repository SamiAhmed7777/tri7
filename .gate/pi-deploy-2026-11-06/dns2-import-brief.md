# DNS2 staking wallet: import the 14 funded keys (operator-directed unblock)

OPERATOR DIRECTIVE: Sami chose "Both, sequenced" (2026-11-07 01:38Z): import
the 14 funded keys into DNS2's staking wallet tonight to unblock the parked
old-funds transaction; the mempool chain-store fix follows as its own gated
project.

## Why

tri7's tx pool validates tx inputs against the node's OWN WALLET live view
(LiveUtxoView(&w.live), follow_tip.rs:5061/6238) - proven live tonight: DNS2
rejected the sami-pc-signed old-coin tx ("not found in indexed prevouts")
while sami-pc accepted it (its wallet owns the coin). Until the pool validates
against the chain store (tomorrow's project), only the staker's own coins can
be mined. Importing Sami's 14 funded keys into his staker's wallet lets the
already-signed transaction (txid cb4fbfd26424de2984d4c931d6104562b9384ea25287a
84d083cb427b37a0cf3, parked in sami-pc's mempool, verified, fee 100 sats,
self-send 1 TRI) be accepted and mined.

## Disclosed side effect (the point, not a bug)

With the keys imported, DNS2's live view includes the ~3,037 TRI of old coins
and its staker may stake them. They remain Sami's coins on Sami's staker and
gain staking rewards; the one-staker rule is unchanged (DNS2 is the sole
staker).

## Headline captured evidence

### CMD: the parked tx is accepted ONLY where the wallet owns the coin (live proof, 01:39Z)
$ DNS2 19112 sendrawtransaction <cb4fbfd2 hex> ->
{"error":{"code":-25,"message":"input 0 efcc6916...:1 not found in indexed
prevouts (unknown or outside the live window)"}}
$ sami-pc 19114 sendrawtransaction <same hex> ->
{"error":null,"result":{"announced_to":0,"txid":"cb4fbfd26424de2984d4c931d6104562b9384ea25287a84d083cb427b37a0cf3"}}

### CMD: the import source is the verified 14-key dump (imported on the wallet COPY earlier)
$ wallet_keytool importwallet pc-wallet-copy.dat funded-keys-14.dump
{"errors":[],"failed":0,"imported":0,"skipped":14}  (all 14 already keyed there)
$ all 14 funded addresses (3,036,990,591 sats total) carry addr= lines in the PC dump.

## Exact scope

- Import: /root/tri7-lab/funded-keys-14.dump (5 comment lines + 14 key lines)
  into /var/lib/tri7/wallet.dat via the OFFLINE tool
  (/root/tri7/target/release/wallet_keytool importwallet) while tri7d.service
  is STOPPED - no RPC, no live-node mutation during the write.
- Restart: systemctl start tri7d (the live view reseeds; DNS2 is the SOLE
  staker - expected downtime ~2-4 minutes, no blocks minted in that span).
- Then: re-relay the parked hex (same sendrawtransaction on DNS2) - now
  accepted - and verify it mines.
- NEVER touched: sami-pc, its wallet, the Pi, consensus code, configs.

## DEFINITION of every absolute term

- DEFINITION "backed up": /root/tri7-recovery/dns2-wallet-backup-<ts>/ holds a
  byte copy of /var/lib/tri7/wallet.dat taken while tri7d is STOPPED, with its
  sha256 recorded.
- DEFINITION "imported": wallet_keytool dumpwallet on the live wallet file
  reports 14 more keys than the pre-import count and lists all 14 funded
  addr= lines; checkwallet reports integrity ok.
- DEFINITION "staking restored": getstakinginfo enabled:true and attempts
  advancing within 10 minutes of start.
- DEFINITION "unblocked": sendrawtransaction of the parked hex on DNS2 returns
  null error, and getrawtransaction <txid> true shows a blockhash +
  confirmations >= 1 within 45 minutes (DNS2 minted 14 blocks in the
  00:51-01:22 hour; 45 min ~= 10x the observed median interval).

## Executor steps

1. systemctl stop tri7d; verify the process is gone (pgrep follow_tip empty).
2. Backup: mkdir /root/tri7-recovery/dns2-wallet-backup-<ts>; cp -a
   /var/lib/tri7/wallet.dat into it; sha256sum both files.
3. Import offline: wallet_keytool importwallet /var/lib/tri7/wallet.dat
   /root/tri7-lab/funded-keys-14.dump; then dumpwallet -> all 14 addr= lines
   present; checkwallet -> integrity ok.
4. systemctl start tri7d; wait for RPC; getstakinginfo enabled:true.
5. sendrawtransaction <parked hex> on DNS2 -> null error.
6. Poll getrawtransaction <txid> true up to 45 min -> mined -> confirmations >= 1.
7. Verify staking resumed (attempts advancing) and DNS2 tip advancing.

## Rollback (precondition: the backup from step 2 exists)

1. systemctl stop tri7d.
2. cp the backup over /var/lib/tri7/wallet.dat (sha256 must equal the backup).
3. systemctl start tri7d; verify staking enabled + tip advancing.
4. The imported keys disappear with the restored file (they also live on
   sami-pc's wallet - nothing is lost). The parked tx returns to its
   pre-import state: accepted only on sami-pc, unmined.

## Risk statement

DNS2 stakes with the imported coins from now on (disclosed side effect, same
owner). Downtime of the sole staker during the import window: a few minutes
(no blocks minted; nothing is at risk - the chain simply pauses minting).
