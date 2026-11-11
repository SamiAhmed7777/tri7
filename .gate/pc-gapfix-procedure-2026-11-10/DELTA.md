
DELTA (applied to /root/.hermes/skills/tri7-fleet-chain-state/SKILL.md, section 10 "Tooling pitfalls when repairing a store", inserted after the append-duplicates bullet):

+**On sami-pc, run the whole gapfix inside the state-repair window** (the
+ item-12 rule applies to ANY state-dir write - gapfix appends blk0001.dat and
+ rewrites index/): disable tri7d-watchdog -> `schtasks /end tri7d2` ->
+ `taskkill /f /im follow_tip.exe` -> gapfix runs -> verify 0 missing ->
+ re-enable watchdog -> `schtasks /run tri7d2`. A windows-gnu
+ `store_gapfix.exe` is staged in `C:\Users\hello\tri7\` as a standing repair
+ tool; the `{height,hash,hex}` bundles come from DNS2's canonical RPC
+ (getblockhash then getblock `<hash>` 0) since every RPC is loopback-only.
+
+**Run a MAP pass first with an EMPTY bundle.** `--from-file` with a 0-byte
+ file appends nothing (bundle loop exits with `appended 0, refused 0`) but
+ still prints the authoritative window, coverage counts, and the exact
+ `missing: hX..hY (N blocks)` set - build the real bundle to cover that range
+ plus margin. `--from-file` mode never contacts the peer arg, and refusals on
+ authority mismatch append nothing (safe by design).
+
+**A seed walk that dies at height H proves only the range BELOW H.** After a
+ fill, re-run the map against the CURRENT window: this tick the first fill
+ covered h2208857..h2208891 while a second torn patch (h2208906..07 - tipstate
+ flushed ahead of the bodies during the PC's hard-exits) sat just above the
+ verified window and re-wedged the wallet live view. Hard-exit tears come in
+ patches; trust only a full-window `4600 present, 0 missing`.
+
+**`getblock` RPC takes a HASH, not a height.** Probing
+ `params:[<height>,0]` errors on every call and reads as "the whole window is
+ missing". Fetch `getblockhash(h)` first, then `getblock <hash>` 0.

(Each ** is a bold-titled bullet in markdown; four new bullets, zero existing lines changed.)
