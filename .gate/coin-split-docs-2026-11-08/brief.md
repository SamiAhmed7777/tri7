# COIN-SPLIT ops record (workqueue item 10) — docs + skill delta

The code blocker fix was committed separately (6ed553f, grade A
urn:ump:ff7lj7vxyvzhhx2jllxqlwdb5qc2jknwr7hiurc3glwffkxqutoq, pushed, CI
42446144052 green, deployed to all four). This commit is the OPS RECORD:
HANDOFF.md tick section + the tri7-fleet-chain-state skill delta
(reference/2026-11-08-coin-split.md + SKILL.md section 1/14 amendments).

## What landed

1. Wallet-file census (dumpwallet "# addr=" extraction only — naive T…33
   regexes match WIF substrings): 250 shared keys, PC-only ZERO, DNS2-only 2
   (both HD change addresses).
2. Zones: PC HD band above DNS2's frozen counter (keypoolrefill 250 →
   hdchain 122→272, 147 PC-only keys, set-diff proven); DNS2 zone = 8 keys
   from a fresh INDEPENDENT seed (wallet_from_seed → keytool dumpwallet →
   importwallet; labels seed/0..7; phrase file on DNS2 0600 for the
   operator's LastPass). Sequential refills can never give both sides HD
   exclusives — the last refiller sweeps the other's band (proven live: the
   PC's refill swallowed DNS2's two change-address exclusives).
3. The seed 10-address set drained via zero-change sendtoaddress with
   dryrun-iterated amounts (`amt += change_sats`), one tx per mature window,
   mempool-gated. Ledger in handoff.diff; full JSONL evidence attached.
4. Fleet integrity held through the ops: DNS2 staking never paused; tip
   parity DNS2=Pi maintained; three fleet incidents found and handled live
   (PC staking self-re-enabled after a restart and solo-minted on shared
   coins — disarmed + re-pointed to canonical; DNS3 solo-forked on its
   re-copy — disarmed, repair specified; Pi staking found silently OFF —
   re-enabled).

## Honest interim state (the drain tail is chain-time-gated)

At the tick's close ≈420.55 TRI of the seed set remained mid-maturity-cycle,
ground to dust by an auto-relaunching drain wrapper (split_followup3.py,
~11-30M per mature window; every stake roll re-immatures the coins it
touches, so drains pace with mints). The next tick: verify the dust census,
rebalance the PC zone (~181 TRI over-target from the accidental 305.719053
send) via the PC wallet if wanted, enable PC staking via the RPC, and repair
DNS3 (canonical re-copy + peer moved off sami-pc). The docs state this
interim truth verbatim — no dust claim is made before the dust census.

## Evidence files

- handoff.diff — the HANDOFF.md tick section as committed.
- 2026-11-08-coin-split.md — the skill reference (interim-truthful).
- split_log.jsonl — every send/error/mine event across all driver runs.
- split_*.py — the drain drivers (dryrun-iterate, mine-gated, pool-gated).
