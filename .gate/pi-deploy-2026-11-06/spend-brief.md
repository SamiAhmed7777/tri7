# Broadcast the pinned old-funds self-spend (TBxrQx34 coin) via DNS2

OPERATOR DIRECTIVE: Sami's 2026-11-06 plan (reaffirmed "do it", 2026-11-07)
ends with a real self-spend proving the old funds move. The repo convention
authorizes self-send verification tests.

## What is already done (offline, read-only on production)

- examples/spend_pinned.rs (NEW file, attached, graded with this round):
  builds + signs + self-verifies a 1-input transaction from a PINNED
  wallet-owned prevout of a wallet FILE copy (read-only). Mirrors the proven
  spend_live.rs construction; pays an arbitrary address (address_to_payload ->
  p2pkh, payload[1..] after the round-5 fix of the version byte).
- Ran against the COPY of sami-pc's canonical wallet
  (sha256 f2ad1026...; the live wallet was not touched):
  input efcc6916d521fb8fe02a9dedd38bbe343acc0209c2d4abc6a76a804b2d93f597:1
  = 30,460,093 sats owned by TBxrQx34J2mRvkmixnrdUynwzEC3wPdgb3 (713
  confirmations at build time), 1,000,000 sats to
  THsbyNZBWtyrAThVkaswkm9PxszbfjQV8U (fresh getnewaddress of the same wallet),
  change 29,459,993 sats back to TBxrQx34..., fee 100 sats, 195 bytes.
  verify_input + connect_inputs_check passed (SPEND_PINNED OK).
- Signed tx: /root/tri7-lab/spend-pinned.json, txid
  cb4fbfd26424de2984d4c931d6104562b9384ea25287a84d083cb427b37a0cf3.
  This resolves the round-5 coin-selection blocker BY CONSTRUCTION: the input
  is pinned, selection cannot drift.

## What this grade covers (the only state-advancing step)

Broadcast the signed hex on DNS2 (the Tor-only sole staker), then verify:

### CMD: the relay step (executed only after grade >= B)
$ ssh DNS2: curl -s -X POST http://127.0.0.1:19112 -d '{"method":"sendrawtransaction","params":["<390 hex chars>"]}'
$ poll getrawtransaction cb4fbfd2... true every 60s for up to 45 min

### CMD: DNS2 stake cadence evidence for the 45-minute window (14 mints in the last hour)
$ DNS2 tri7d.log 00:51-01:22 window: 14 TIP ADVANCE lines (stability-final.txt)
$ DNS2 getstakinginfo: attempts climbing, enabled:true (checked pre-relay)

## DEFINITION of every absolute term

- DEFINITION "broadcast": sendrawtransaction returns null error for this hex.
- DEFINITION "mined": getrawtransaction <txid> true returns a blockhash within
  45 minutes (DNS2 minted 14 blocks in the 00:51-01:22 hour; 45 min = ~10x the
  observed median interval).
- DEFINITION "confirmed": confirmations >= 1 on DNS2.
- DEFINITION "net effect on Sami's coins": 1 TRI moves between two addresses
  of the same wallet (TBxrQx34 -> THsbyNZB), fee 100 sats leaves the wallet;
  nothing leaves Sami's ownership.

## Risk statement

A broadcast transaction cannot be un-broadcast. Worst case: the 1 TRI sits in
another address of Sami's own wallet (recoverable by a normal spend) and the
fee (~100 sats) is consumed. No third party is involved.

## Rollback

Not applicable to a broadcast tx; the bounded loss above is the accepted risk
stated in the operator-approved plan.
