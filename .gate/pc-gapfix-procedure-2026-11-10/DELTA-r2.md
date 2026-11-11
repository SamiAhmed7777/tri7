SECTION 10 AS APPLIED AFTER POLISH (r2):

- Appending duplicates is harmless (`append_block` returns early when the hash is
  already indexed), so a bundle may safely cover more heights than are missing.

- **On sami-pc, run the whole gapfix inside the state-repair window** (the
  item-12 rule applies to ANY state-dir write — gapfix appends blk0001.dat and
  rewrites index/): disable tri7d-watchdog → `schtasks /end tri7d2` →
  `taskkill /f /im follow_tip.exe` → gapfix runs → verify 0 missing →
  re-enable watchdog → `schtasks /run tri7d2`. A windows-gnu
  `store_gapfix.exe` is staged in `C:\Users\hello\tri7\` as a standing repair
  tool; the `{height,hash,hex}` bundles come from DNS2's canonical RPC
  (getblockhash then getblock `<hash>` 0) since every RPC is loopback-only.
- **Run a MAP pass first with an EMPTY bundle.** `--from-file` with a 0-byte
  file appends nothing (bundle loop exits with `appended 0, refused 0`) but
  still prints the authoritative window, coverage counts, and the exact
  missing set — build the real bundle to cover that range plus margin. Live
  map-pass output from this tick (first run): `oracle window h2204305..h2208904
  (4600 heights)` → `window coverage: 4594 present, 6 missing` → `missing:
  h2208857..h2208891 (6 blocks)` → `bundle: appended 0, refused 0`. The
  verify pass after a fill is the SAME command: it must end
  `window coverage: 4600 present, 0 missing` / `nothing to repair`.
  `--from-file` mode never contacts the peer arg (the positional peer is only
  read in the fetch path); a refused bundle line appends NOTHING — the tool
  recomputes hash9 over `bytes[..80]` and compares to the tipstate hash before
  any write (`bundle hash mismatch (got … want …) — REFUSED`, store_gapfix.rs
  — identity check against local authority, not trust-the-peer), so a stale or
  fork-contaminated bundle is fail-safe by design.
- **A seed walk that dies at height H proves only the range BELOW H.** After a
  fill, re-run the map against the CURRENT window: this tick the first fill
  covered h2208857..h2208891 while a second torn patch (h2208906..07 — tipstate
  flushed ahead of the bodies during the PC's hard-exits) sat just above the
  verified window and re-wedged the wallet live view. Hard-exit tears come in
  patches; trust only a full-window `4600 present, 0 missing`.
- **`getblock` RPC takes a HASH, not a height.** Probing
  `params:[<height>,0]` errors on every call and reads as "the whole window is
  missing". Fetch `getblockhash(h)` first, then `getblock <hash>` 0.

- **Delete the repair's store copies at tick end — rename-aside during the
  repair window, DELETE before closing.** Five days of repairs left ~13 GB of
  canonical-snap/pre-canon copies on DNS2 and ~5.9 GB of fork stores/indexes/
  tipstates in the PC's state2; that churn pushed DNS2 to 96% and the fs has
  flipped read-only once. The copies are dead by definition (canonical is
  replicated fleet-wide; a superseded fork store is exactly the "discarded
  material" item 12 declared). Reclaim discipline per target: verify nlink==1
  + expected size, manifest the stat, unlink, post-check gone. Watch: DNS2's
  disk-guard is hourly with a 90% Telegram alert — trust it, but it only
  vacuums regenerables, never tri7 state copies.
- **Bulk deletes are approval-blocked in cron mode on BOTH ends.** `rm`-style
