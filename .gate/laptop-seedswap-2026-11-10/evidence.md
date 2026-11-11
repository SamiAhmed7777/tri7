# Verbatim evidence — laptop seed swap, 2026-11-10 (cron tick, host DNS2)

## Defect state found (read-only probes, laptop RPC 19114 + fs)
- ssh sami-laptop getblockcount -> {"error":null,"id":null,"result":0,"source":"tri7"}
- ssh sami-laptop getsyncinfo -> {"result":{"body":{},"height":0,"phase":"follow","syncing":false},"source":"tri7"}
- dir C:\Coins\TRI-LAPTOP\node\state ->
    11/10/2026 05:51 AM 208 blk0001.dat
    11/10/2026 05:51 AM 206 gap_recovery.json
    11/10/2026 06:23 AM <DIR> index
    11/10/2026 06:36 AM 934 peers.json
    11/10/2026 05:51 AM 780 tipstate.json
- type tipstate.json -> {"height":0,"blockhash":"7e7a6e4d…","updated_utc":"2026-11-10T16:59:31Z","last_advance_utc":"2026-11-10T13:51:42Z","window_base":0,...}
- Process model (welded single-exe): tasklist shows ONLY tri7_wallet_gui.exe PID 10204;
  CommandLine: "C:\Coins\TRI-LAPTOP\tri7_wallet_gui.exe" --wallet C:\Coins\TRI-LAPTOP\wallet.dat --rpc 127.0.0.1:19114
  (no follow_tip.exe process — the embedded node is in-process; the GUI answers RPC 19114)
- tri7_wallet_gui.exe 14,798,848 bytes, dated 11/10/2026 06:22 AM (welded build, > the 12,192,256 pre-weld GUI)

## Root cause, source-cited (not hypothesis)
/root/tri7/src/node_embedded.rs (weld module = moved src/bin/follow_tip.rs; identical at
/root/tri7-lanes/lane-singleexe/src/node_embedded.rs:9470):
    9470: let store_usable = blocks_file
    9472:     .map(|bf| { bf.exists() && std::fs::metadata(bf).map(|m| m.len() > 0).unwrap_or(false) })
    9476: let have_state = path.exists() && !cfg.reseed && store_usable;
    9479: let from_zero = cfg.sync_from_zero || (!have_state && cfg.explicit_peers);
The preceding comment declares the intent: "A state that knows only genesis (tip h0) with
no block bodies on disk is not usable state — treat it as a bare machine so the from-zero
path starts the body fetch". The implementation checks only len() > 0; the daemon's OWN
genesis frame (208 bytes) satisfies it, so a bare machine that anchored genesis (wrote
frame + h0 tipstate) has have_state=true on every later start and the from-zero body
fetch can never resume. Observed signature: 3+ hours at height 0, phase follow,
syncing false.

## Swap execution (verbatim outcomes)
1. schtasks /end /tn tri7gui-laptop -> "SUCCESS: The scheduled task \"tri7gui-laptop\" has been terminated successfully."
   taskkill /f /im tri7_wallet_gui.exe -> ERROR ... no running instance (already gone via /end)
   post-check: PROC-CHECK empty (no tri7_wallet/follow_tip); task status "Ready"
2. ren aside (cmd ren, one per target, REN-DONE marker) -> state dir then contained ONLY:
   blk0001.dat.pre-seedswap (208 B), gap_recovery.json.pre-seedswap, index.pre-seedswap,
   paytxfee.json (kept), peers.json (kept), smsg\ (kept), tipstate.json.pre-seedswap (780 B)
3. Transfer (DNS2 -> laptop, scp BatchMode): blk0001.dat (982,892,538 B) + tipstate.json
   (2,999,944 B) + -r index\ (blockidx.bin 70,694,649 B, txidx.bin 71,958,272 B)
   -> "LAPTOP-SEED-TRANSFER-OK", exit 0, ~15 min.
4. schtasks /run /tn tri7gui-laptop -> "SUCCESS: Attempted to run..."; status Running.

## Byte-identity proof (local sha256sum vs laptop Get-FileHash readback — ALL MATCH)
blk0001.dat   ef93ebb29736a7767e79d79c1ff8f2109df4a7409d0068e13816c82d278c3cb7
tipstate.json d32be1311caaa83a70876fd7e5d59406b27f5ba394965ff9ffdd18e52500a5cd
blockidx.bin  f1542edaafbcb82d8d637253f7df6a709c6937de55c02a4fa1148926ee32157d
txidx.bin     f171117ba9358e52e17b2bfc6c0d41071190b652a2987064a5be4a281ccc2173

## Post-swap verification (~3 min after relaunch)
- getblockcount 2209133 -> 2209134 (live, advancing)
- PARITY at shared height 2209120: laptop getblockhash = 3907c385a9f03e7254111e0143a36948aa9baa88e956730bff524957350670cf
  DNS2 getblockhash 2209120 = 3907c385a9f03e7254111e0143a36948aa9baa88e956730bff524957350670cf (IDENTICAL)
- Tips: laptop 2209134 (best d0e3e776…), DNS2 2209132 (best bc45fbde…) — 0-2 block staking race, same chain
- node.log: "wallet live view ready: window h2204534..h2209133 — 1 outputs (1 owned) balance 250000000 sats"
  ; "handshake ok: 46.62.249.20:443 -> peer version 70206 (/tri7:7.0.0/) start height 2209132"
  ; onion dials failed socks 127.0.0.1 refused (EXPECTED — laptop has no tor socks)
  ; "STAKE: kernel solved after 4 attempts" ; "STAKE MINTED h2209134 d0e3e776… (self-validated) — relaying to 1 peer(s)"
- getstakinginfo: enabled:true, attempts ticking, last_hit 2026-11-10T17:20:37Z
- GUI back up: tri7_wallet_gui.exe PID 27824, 179,852 K
