# PC wallet permanent fix — deployment brief (sami-pc)

## Problem
sami-pc (the wallet home per operator directive) ran a pre-fallback binary
(95159327abc9d34de9a24eda1b55cc0d4cde98b351d548c15ba3c13acb9f8357) pointed at a
DEAD clearnet daemon (ft_state2.bat: --daemon 194.233.88.206:24113), leaving it
peerless, ~130 blocks behind, and unable to see DNS2's stake rewards or the
mined old-funds tx. Its conf also still said staking=1 (single-staker rule
violation waiting to fire the moment peers landed) and carried a stale
cycle-45 addnode set (old DNS2 onion).

## Change (all on sami-pc; repo untouched — configs live on the host)
1. BINARY: follow_tip.exe 95159327... -> **CI artifact** caf3491653c23b6973d0237bef520d472cc2824978d2351970eadba496f3194a
   downloaded via `gh run download 42331477281 -n tri7-windows` (commit 8987e03,
   MSVC windows-latest, canonical pipeline; contains the deep-coin pool
   fallback graded B in bc705aa/8987e03). Operator suggested CI over rebuild —
   adopted mid-task; the interim cross-compile (b9bd3016...) was backed up to
   follow_tip.exe.crossbuild-b9bd3016, the old exe to follow_tip.exe.pre-poolfix-20261107.
2. ft_state2.bat: `--daemon 194.233.88.206:24113` (dead) replaced with
   `--onion gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113
   --socks 127.0.0.1:9050` (Tor-only policy; Tor task tri7tor already running,
   SOCKS 9050 listening, pid 32576). Everything else byte-identical.
   Backup: ft_state2.bat.pre-poolfix-20261107. New bat sha256
   d17f72b0f23265aa6e871c6243c1066e7eb73dd407ac8a6276e618952c556ea3.
3. triangles.conf (state2): staking=1 -> staking=0 (single-staker rule; stake=0
   already present); stale DNS2 onion addnode 43kgtx6vcz...:24113 refreshed to
   the live gxvrhv3q...:24113 (verified live by the Pi tonight). Backup:
   triangles.conf.pre-poolfix-20261107.
4. One install-command defect caught and fixed before start: a self-referential
   `move /y ft_state2.bat ft_state2.bat` no-op left the OLD bat live (verified
   by `type`, then moved the staged .new properly and re-verified sha equality
   with the staged file).

## Verification evidence (2026-11-07 ~03:15-03:30Z)
- Restart proof per skill rule: getstakinginfo enabled:false AND attempts:0 on
  the NEW binary with peers attached (no fork risk).
- Peers: 1 Tor peer, clearnet:0 i2p:0, endpoint ra32xf5og...onion:24112.
- Catch-up: 2206752 -> 2206884 in ~4 min; getbestblockhash IDENTICAL to DNS2
  (399b6d3a771edf1cee729167bc83c54e140a95d6c8a7607e5074e01992303656) at equal height.
- Mined old-funds self-spend cb4fbfd26424de2984d4c931d6104562b9384ea25287a84d083cb427b37a0cf3
  visible on the PC with 12 confirmations.
- Balance parity after live-view rebuild: pc h2206888 bal 3238.999998 vs
  dns2 h2206888 bal 3238.006297. Structural difference explained and verified:
  THsbyNZB holds 1.0 TRI on the PC and 0 on DNS2 (fresh self-spend address
  exists only in the PC keypool — the wallet home per directive); remaining
  ~0.015 is DNS2 stake churn between reads (per-address diff table captured).

## Known issue disclosed (follow-up, not blocking)
The wallet live view does not fully apply the effects of a fast catch-up
burst (balance read 3208.31 vs 3237.99 before restart; specific stale/missing
coins identified via listunspent diff). A daemon restart rebuilds the view
over the complete chain and heals it (verified). Lesson recorded in
tri7-fleet-chain-state: after any catch-up, restart or otherwise force a
rebuild and re-verify balance parity against the staker before trusting the
number.

## Rollback
Restore the three .pre-poolfix-20261107 backups (exe, bat, conf) and restart
the task; the crossbuild backup also retained.
