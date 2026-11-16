# Errata + runbook notes (required by judge round 1, grade B urn:ump:tq7xkbxskbfkwxttk2fsdpajoqgrfpruydhmtcp7jqrwuen5ez3q)

## 1. Fork-height / block-count bookkeeping (judge blocker 1 — corrected in record)

The numbers in BRIEF/E1/E2 mix three sources and are reconciled here:

- TRUE shared ancestor: h2209466 = db9a8505… verified identical on all four
  nodes (E1). Hashes provably differ at h2209489 (E1). Heights 2209467..2209488
  were NOT individually compared node-by-node — the exact fork slot lies in
  2209467..2209488 and was not pinned further.
- The PC judge's own log labels (E2: "h2209486..h2209493 (7 blk)") are its
  INCREMENTAL-FETCH bookkeeping: the rival segment grew 2 blk → 3 blk → 4 blk →
  5 blk → 6 blk → 7 blk across successive reconciliation rounds as rival blocks
  arrived, and the "(7 blk)" label for an 8-height inclusive range is the
  judge's own count of FETCHED rival blocks, not a chain fact. These labels
  were quoted verbatim in E2; they are not this change's claims.
- DECISIVE for the fix (and self-consistent on both sides): the judge-computed
  WORK TOTALS over the compared segment — canonical 3.429674e24 vs PC
  4.886305e24, both computed by the PC's own judge over the same rival segment.
  The conf-port fix is fork-height-agnostic: it restores DNS2's outbound
  visibility so DNS2's judge can compute the same comparison itself.

## 2. Staking persistence caveat (judge blocker 2 — runbook note, NOT a conf change)

conf-before.conf carries staking=0/forcestaking=0 while the RUNNING DNS2
process stakes (enabled:true, 11.3M attempts): fleet practice re-asserts
staking via the RPC (string "on") after every restart, and that RPC assert is
process-lifetime only — it does NOT survive the next restart given conf
staking=0. This change deliberately does NOT flip the conf staking line (out of
graded scope). Consequence recorded: after every DNS2 restart (including this
procedure's), staking must be re-asserted via RPC or DNS2 sits idle until the
next assert. Logged as a follow-up workqueue item (conf-level staking
persistence decision belongs to the operator).

## 3. Follow-up items logged (judge polish 3+4)

- sami-pc's published onion tuu7h65j…:24113 is DEAD (tor dial timeout, E6) and
  its i2p endpoint is unverified; its addnode entries anywhere should match
  reality. Follow-up item.
- Inbound-peer claims asymmetry: DNS2's judge reconciles only against OUTBOUND
  peers; a heavier INBOUND peer's branch is invisible forever. This port fix is
  the durable bridge for the PC (DNS2's outbound dial persists across the PC's
  ~10-min restart churn, unlike the PC's own outbound dial). The daemon-level
  fix (reconcile against inbound peers' claimed tips too) is logged as a
  follow-up workqueue item.
