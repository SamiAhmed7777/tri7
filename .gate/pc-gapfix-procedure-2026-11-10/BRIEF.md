# BRIEF — tri7-fleet-chain-state §10 delta: store_gapfix on sami-pc (retirement-verification tick, 2026-11-10)

CONTEXT: TRI7 parity queue is COMPLETE (all 19 items DONE, loop retired). This
tick was a retirement verification + live repair of the PC wallet home.

WHAT HAPPENED: the PC daemon hard-exited twice overnight (known unknown-actor/AVG
pattern). The watchdog resurrected it, but the wallet live view wedged on
"store missing block body at h2208857" retry-loops — hard-exit tore the store
tail (tipstate flushed ahead of the bodies). First-ever use of the repo's own
examples/store_gapfix.exe --from-file path on sami-pc (previous fills were the
manual raw-frame append).

REPAIR RESULT (all verified live):
- MAP pass (empty bundle): window + exact missing set, appends nothing.
- FILL 1: 6 holes h2208857..h2208891; 48-line bundle, 0 refused; append_block
  deduped already-present bodies (frames +6 exactly).
- FILL 2: re-map after the walk surfaced a SECOND patch above the verified
  window: exactly h2208906..h2208907 (tipstate had flushed to 2208907).
- FINAL: 4600 present, 0 missing across h2204316..h2208915.
- MID-REPAIR LIVE PROOF: a 6-block PC fork race resolved end-to-end by the
  judges (streak trigger + anti-wedge + FORK CHOICE switch + kernel-enforced
  chase); four-way parity EXACT 2208921 ec02b81e; repairwallet healed the
  14-output reorg drift to "wallet check passed":true; getbalance 5110.900381
  = the standing number; staking enabled=true.

THE DELTA (§10, four bullets):
1. sami-pc gapfix runs INSIDE the state-repair window (watchdog disable →
   tri7d2 end → taskkill → gapfix → verify → re-enable → /run), windows-gnu
   store_gapfix.exe staged in C:\Users\hello\tri7\ as a standing tool.
2. MAP pass with an EMPTY bundle prints window + exact missing set, appends
   nothing; --from-file never contacts the peer arg; refusals append nothing.
3. A seed walk dying at H proves only BELOW H — re-map against the CURRENT
   window after a fill (hard-exit tears come in patches).
4. getblock RPC takes a HASH, not a height (height probes error on every call
   and read as "whole window missing").

ASK: grade this skill-procedure delta (accuracy, safety, completeness).
