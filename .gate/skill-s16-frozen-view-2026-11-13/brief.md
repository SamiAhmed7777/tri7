# Brief: tri7-fleet-chain-state §16 bullet — frozen stale_live_view + mid-loop phantom rotation

Artifact: one new bullet appended to the §16 bullet list in
/root/.hermes/skills/tri7-fleet-chain-state/SKILL.md (after the existing
"(2026-11-13, live DNS2) repairwallet pass-1 miss" bullet at ~line 1286).

## New live facts this tick (2026-11-13 02:1x-02:2xZ, DNS2 heal loop)

1. During a 4-pass repairwallet loop, `getstakinginfo.stale_live_view` served a
   FROZEN view: byte-identical dict across passes (same example_txid
   1b5203c151…, same `first_utc` 2026-11-13T01:50:45Z, `ticks` stuck at 92)
   while `attempts` climbed ~70k between probes. The journal shows the daemon
   actually logged `stale live view CLEARED — wallet evidence matches the chain
   again (repairwallet took effect)` at 02:14:52Z — MID-LOOP. So the RPC view
   can lag/freezing while the detector state has already cleared.
2. The phantom set ROTATES mid-loop: the 1b5203c151 phantom (raced h2210740
   residue) was cleared, but a NEWER raced-mint phantom (6b423eea…:1 claiming
   h2210753, fresh `first_utc` 02:16:12Z, ticks 6, coins 2) surfaced
   immediately after — DNS2 raced again (h2210753 lost its work race) and the
   stale view re-armed with fresh facts.
3. Healing rule: do NOT read a frozen identical dict as heal failure.
   Correlate with journal `STALE LIVE VIEW` / `CLEARED` lines and the
   `first_utc`/`ticks` freshness of the dict; keep running repairwallet passes
   until stale_live_view is null confirmed on a FRESH (non-frozen,
   first_utc/ticks advancing) view. One extra pass after the frozen stretch
   cleared it ("repaired 3 / 26.793238" → stale null, §16 BOTH hold verified).
4. Context: DNS2 raced twice this window (h2210740 lost to fleet h2210742 at
   01:50:03Z; h2210750 lost to h2210751 at 02:12:38Z) — normal heavy-staker
   cadence, fork choice adjudicated both, zero surgery. The variable-length
   heal is now proven ≥4 passes effective with frozen-view accounting.

## Proposed bullet (exact text)

- **(2026-11-13 02:2xZ, live DNS2) A frozen `stale_live_view` during a heal
  loop is NOT proof of heal failure — and the phantom set can rotate mid-loop.**
  Across a 4-pass `repairwallet` loop `getstakinginfo` served a byte-identical
  dict (same example_txid, same `first_utc` 01:50:45Z, `ticks` stuck at 92)
  while `attempts` climbed ~70k between probes — yet the journal recorded
  `stale live view CLEARED … (repairwallet took effect)` MID-LOOP (02:14:52Z):
  the RPC view can lag the detector state. And clearing one phantom can
  immediately re-arm the view with a NEWER one (6b423eea…:1 claiming h2210753,
  fresh `first_utc` 02:16:12Z — DNS2 raced again and lost) — an identical dict
  with a FRESH `first_utc` is a new phantom, not the old one. Account for both:
  correlate with the journal `STALE LIVE VIEW`/`CLEARED` lines and the
  `first_utc`/`ticks` freshness, keep passing until `stale_live_view` is null
  on a FRESH (non-frozen) view; one extra pass after a frozen stretch cleared
  it (repaired 3 / 26.793238 → §16 BOTH hold verified).

## Verification evidence (real tool output this tick)

- Sweep 02:10Z: anchors h2210290 7692ff3a / h2210355 275c34f7 UNANIMOUS;
  prior-close h2210734-0739 UNANIMOUS == expected; near-tip h2210749-0751
  UNANIMOUS; DNS2 minted h2210752 ahead (leader shape); four tips h2210750-0751.
- Heal: DNS2 4 passes (frozen view, journal CLEARED 02:14:52Z mid-loop),
  extra pass → CLEARED, §16 BOTH hold: stale null + checkwallet integrity ok /
  bad_keys 0, enabled=True, attempts 26.51M, bal 1901.223371.
- PC: stale BEFORE (00e8d4de, h2210748, 1 coin) → cleared pass-2 (first
  observed PC 2-pass shape this lane; pass-1 repaired 2 / 47.176617), stale
  null, bal 1537.863198.
- Close readback: all four h2210755 EXACT (spread 0), h2210754-0755 UNANIMOUS,
  DNS2 minted h2210756 ahead, zero splits; DNS3 known adjudicated ca2ec8c4
  watched-only, 100.00; Pi clean 138.210843.
