# Skill delta: tri7-fleet-chain-state — laptop h0 wedge + proven seed-swap recipe

## What happened (cron follow-through of session 20261109_134219_9361e505)
The staged 983 MB seed (sami-pc state2 -> DNS2 scratch laptop-seed/, DONE marker
SEED-COPY-DONE) landed AFTER that session ended; this tick performed the promised
laptop swap and found a NEW product-defect signature on the way.

## The defect (new evidence)
SAMI-LAPTOP ran the WELDED single-exe GUI (tri7_wallet_gui.exe 14,798,848 B,
PID 10204, in-process node — no separate follow_tip.exe process; the GUI itself
answers RPC 19114). Its fresh node state had wedged at HEIGHT 0 for 3+ hours:
- node\state\blk0001.dat = 208 bytes (genesis frame only)
- tipstate.json: height 0, window_base 0, last_advance_utc 13:51:42Z
- getsyncinfo: {"height":0,"phase":"follow","syncing":false,"body":{}}
Root cause class: the 208-byte genesis frame counts as a NON-EMPTY blocks store,
so `have_state` = true and the from-zero sync never fires — the node follows at
genesis forever. This is the genesis-only-sync gap demonstrated on a real fresh
install (zero-requirements rule violation; fresh installs are supposed to ship a
chain snapshot via tridock, but a hand-installed portable GUI has none).

## The proven cure (full recipe, this tick, all steps verified)
1. `schtasks /end /tn tri7gui-laptop` + `taskkill /f /im tri7_wallet_gui.exe`
   (welded build: the embedded node dies WITH the GUI; nothing else to kill).
2. Rename aside ONLY: blk0001.dat, tipstate.json, index\, gap_recovery.json.
   KEEP: peers.json (laptop's learned peers), paytxfee.json, smsg\.
3. scp the trio (blk0001.dat + tipstate.json + index\) into node\state\.
4. `schtasks /run /tn tri7gui-laptop`.

## Verification evidence (verbatim, from this tick)
- Transfer: LAPTOP-SEED-TRANSFER-OK, exit 0, ~15 min for 983 MB + 139 MB index.
- Byte-identity: SHA256 match on ALL 4 files (local seed vs laptop readback):
  blk0001.dat  ef93ebb29736a7767e79d79c1ff8f2109df4a7409d0068e13816c82d278c3cb7
  tipstate.json d32be1311caaa83a70876fd7e5d59406b27f5ba394965ff9ffdd18e52500a5cd
  blockidx.bin f1542edaafbcb82d8d637253f7df6a709c6937de55c02a4fa1148926ee32157d
  txidx.bin    f171117ba9358e52e17b2bfc6c0d41071190b652a2987064a5be4a281ccc2173
- ~3 min after relaunch: getblockcount 2209133 -> 2209134 (live, advancing).
- PARITY: at shared height 2209120 laptop and DNS2 serve the IDENTICAL hash
  3907c385a9f03e7254111e0143a36948aa9baa88e956730bff524957350670cf.
- node.log: "wallet live view ready: window h2204534..h2209133 — 1 outputs
  (1 owned) balance 250000000 sats"; handshake ok 46.62.249.20:443 (Pi clearnet,
  the laptop's lifeline); onion dials fail with socks 127.0.0.1 refused —
  EXPECTED on the laptop (no tor socks).
- Laptop then STAKE MINTED h2209134 d0e3e776… (kernel solved 4 attempts, reward
  534,934 sats, self-validated, relayed to its peer); getstakinginfo
  enabled:true, attempts ticking. GUI back up as PID 27824.

## The delta (append to SKILL.md section 6, after the laptop topology paragraph)
**Laptop h0 wedge + seed swap (2026-11-10, live-proven):** a fresh/welded GUI
install with NO chain snapshot wedges at height 0 — the 208-byte genesis frame
in a fresh node\state\blk0001.dat counts as a NON-EMPTY store, so have_state is
true and the from-zero path never fires (getsyncinfo: height 0, phase follow,
syncing false). Cure = full state-trio swap from a canonical node: stop task
tri7gui-laptop + taskkill the GUI (the embedded node is IN-PROCESS on the
welded build — no separate follow_tip.exe exists; the GUI process itself
answers RPC 19114); rename aside ONLY blk0001.dat/tipstate.json/index/
gap_recovery.json (keep peers.json + paytxfee.json + smsg\); scp the trio in;
relaunch the task. Proven end-to-end: SHA256-identical readback on all files,
live tip + shared-height hash parity vs DNS2 in ~3 min, then the laptop minted
its own block on its zone coin. Onion dials fail on the laptop (no tor socks)
— the Pi clearnet 443 peer is its lifeline; that is expected, not a fault.
