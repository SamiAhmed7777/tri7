# TRI7 coordination — read before starting a lane

Append-only. Verified facts only; every claim carries its evidence (command + date).
Reproduce the chain/implementation/scan checks with `bash /root/tri7/tri7-verify.sh`
(read-only; includes the pool cap/TTL constants and the verbose live-spend proof).
Full captured output: `/root/tri7/.gate/verify-output.txt` (2026-11-04T21:22-08:00).
Not covered by the script (code-read facts): GUI lane scope, lane ownership.
_Ownership: GUI (`src/gui/`, `src/bin/tri7_wallet_gui.rs`) = the GUI lane.
Daemon/consensus/RPC/deploys = the fleet lane. Don't touch the other lane's files
without reading its latest commits first._

## Verified state (2026-11-05, fleet lane; repo HEAD 102aaa7)

- Chain LIVE and minting: 33 `STAKE MINTED` lines in the last 60 min at the
  capture below, relaying to 4 peers; balance 3203.01475 TRI, 4 connections.
  Snapshot = verify-output-r8.txt (captured 2026-11-05T05:36:22Z window).
- **Spending is LIVE-PROVEN and SELF**: tx `d7f7a8db709125888e6a2d044b0ae38aa9
  573c857ab2580a6166a8e2fcdadc26` — the 1,000,000-sat spend from spend_live.json
  (its `broadcast:false` flag is STALE) — was mined at **height 2204151**;
  single input e09191f7…:1, vout = [1 TRI, change]; both output addresses match
  the wallet set (node-reported addresses, decoded wallet membership), input
  consumed (listunspent). Confirmations 1398 at capture (growing):
  `getrawtransaction d7f7a8db…,true` -> blockhash a4cd2adc7566f5718e1c04f0cf4
  1b83ba8b326f0acb532d883e9d1cef9799aad, height 2204151. Pipeline proven
  end-to-end: sign -> sendrawtransaction -> pool -> mint inclusion -> confirm.
- Why recent blocks look empty: within the verified window — the last-300-block
  scan shows `blocks_with_more_than_2_txs=0` (the hard mined-window evidence),
  and the node's periodic `txs_accepted` counter read 0 as of its latest report
  (counter scope not documented upstream — indicative only) — no extra tx
  existed to mint, which is expected, NOT a gap (an earlier draft of this file
  wrongly claimed tx-inclusion was unimplemented — the code was verified first
  at txpool.rs caeffbe, follow_tip.rs:4350 handler + :4224 `txs_for_block`,
  rpc.rs:91/122 advert).

## Open work (in order; commits only through the codex gate)

1. **[GUI lane — in progress] finish the redesign per REDESIGN_SPEC.md**
   (transactions page shipped 11-04; remaining pages per spec: Messages, Coin
   Control, Options tabs, RPC console, About/Encrypt parity). Selfcheck +
   `--history-probe` must stay green per commit.
2. **[fleet lane] staged deploys:** laptop (pending), DNS3 (OUT — re-add when
   Sami wants it). sami-pc GUI + follower are current.
3. **[fleet lane] RPC parity leftovers:** the daemon-only methods
   (getrawmempool, getmempoolinfo, getstakinginfo, getnetworkhashps,
   getwalletinfo, gettransaction) still have no native tri7d equivalent —
   list which the GUI actually uses and implement those first.
4. **[either lane] spend acceptance re-run after any consensus-touching change:**
   broadcast a fresh 1-TRI self-spend (spend_live), require inclusion in a
   minted block within 2 stake intervals and relay to all peers.

## Conventions

- Coordination file is append-only; corrections are edits WITH a note, never
  silent rewrites of history that was already committed.
- Never delete wallet.dat. Staking is approved-live. Out-of-wallet-address
  spends still need Sami's explicit nod.
- Sami authorization for this file's contents: his 2026-11-05 directive "make
  sure that it's fully functional as a cryptocurrency" covers verification and
  self-send tests; it does not authorize third-party transfers.
