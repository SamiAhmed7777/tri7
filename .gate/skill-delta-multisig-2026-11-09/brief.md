# Artifact: tri7 item-12 tick skill deltas (two files, two skills)

## What this change is
Two documentation patches recording this tick's durable lessons. No code, no
behavior change anywhere — these are procedure/semantics records.

1. tri7-fleet-chain-state SKILL.md section 2 + references/2026-11-09-fork-cascade.md:
   the tri7d-watchdog repair-window trap. Today the "start-only, harmless"
   watchdog started the PC daemon MID-repair (between scp and moves); the
   daemon loaded the fork-era pair and rewrote tipstate.json with fork state
   over the canonical seed, plus a fork-store index rebuild. Records the
   disable-first/re-enable-after-parity rule, the recovery sequence that
   worked, and the size-tell (3,002,231 fork vs 3,015,600 canonical tipstate).

2. triangles-modernization references/p2sh-multisig.md "complete
   implementation needs" item 7: the RPC verbs LANDED (commit 691ac6f) with
   the real v5 semantics — bare-string returns (not bitcoin-0.8 objects),
   runtime_error → -1 with verbatim strings (incl. the leading space in
   " Invalid public key: "), v5 check order, ParseHexV -8 format, [account]
   label rows on 'C' addresses, the 16-key assert-abort → tri7-refuses
   deviation, and the canonical vectors (pinned by test + reproduced live).

## Why these belong in skills (not memory)
Both are procedures/facts for THIS recurring task type (fleet chain repairs;
legacy RPC porting) — they load only when relevant.

## Evidence
- The watchdog incident: live log lines ("chased tip h2208189", retry loop),
  the recovery commands, and the verified end state (parity exact, repairwallet
  pass) are in /root/tri7-lab/TRI7-WORKQUEUE.md PROGRESS 2026-11-09 tick.
- The verb semantics: every claim cites a file:line of the real v5 tree
  (/opt/triangles-v5/src/...) and was verified live on all four nodes (error
  battery byte-exact) + the write path on a wallet COPY (vector address
  CJU8gKtn4aNBBovbywqPqZo6mjA1B1pLD1 returned live; label row in sqlite).
- The code they document: commit 691ac6f on master, CI 42637049872 green.
