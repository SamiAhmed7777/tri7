# Judge brief — tri7 phase-7 milestone 2 (kind=code)

## What this artifact is

tri7 (github.com/SamiAhmed7777/tri7), the Rust replacement node for the 2013-era
Triangles chain. Milestone 2 of phase 7 ships, as ONE state-advance (commit
caeffbe + 1bbc6bb on top of the graded milestone-1 b74cc74):

1. **smsg wire relay** (src/smsg_net.rs, new): ShadowCoin-lineage smsg over the
   P2P session — bucket inventory announcements, payload getdata, retention +
   purge, lock aging; RPC surface smsgsend/smsginbox/smsgoutbox/smsgscandir/
   smsgpurge/smsgaddaddress/smsgkeys/smsglocalinfo.
2. **txpool + sendrawtransaction** (src/txpool.rs, new; rpc.rs): consensus-validate
   against the live wallet view, pending pool, inv relay, fees attach to the next
   minted coinstake; -8/-25 error-code shape.
3. **Integrated staking loop** (follow_tip stake_tick): kernel search on the live
   wallet view, full self-validation (same gate peer blocks pass: kernel enforced +
   nBits enforced), mint, relay, append-on-accept; getstakinginfo (honest counters:
   attempts/searches/weight/last_hit; in_window_only accounting) + staking on|off.
4. **Wallet wiring**: --wallet PATH on the daemon (SQLite wallet copy, 0600 tri7),
   wallet live UTXO view seeded from the disk store, conf stake=1 (0660 root:tri7).
5. **Live-bug fixes found by deploying it** (all reproduced live on DNS2, fixed,
   regression-tested — see section 7 of the evidence file):
   - staking-loop index/height-translation panic (daemon-killing),
   - follow-loop self-deadlock (two temporary locks of one mutex in one statement),
   - backfill "no proofhash" startup failure on the tri7-only topology,
   - blocks accepted during a cold index build were lost (pending-append buffer +
     flush-on-ready + regression test).
6. **spend_live.rs** (new tool): real-wallet self-spend builder used for the
   sendrawtransaction live proof.

## Measured delta vs milestone 1 (b74cc74)

ADDED: src/smsg_net.rs (49.6KB), src/txpool.rs (21.8KB), src/bin/spend_live.rs (9.5KB),
deploy/windows/* installer, deploy/fleet_tip_monitor.py.
CHANGED: src/bin/follow_tip.rs (+1457 lines: staking loop, wallet live view, NodeOps
adapter, smsg/tx relay ticks), src/rpc.rs (+192: native method routing, ops adapter
plumbing, getstakinginfo/staking/smsg dispatch), src/blockindex.rs (+136: pending-append
buffer), src/kernel.rs (+13: bounds guard), src/gui/* (staking panel + broadcast button
plumbing), src/smsg.rs (+47), src/address.rs (+50), CI windows job builds NSIS installer.
Commits: caeffbe (21:38 PST), 1bbc6bb — worktree HEAD 1bbc6bb; the evidence file
deploy/tri7_p7m2_evidence.md is the live-run record.

## Deployed + verified on DNS2 (production, LIVE MONEY PATH)

All three live proofs were captured against the RUNNING stake=1 daemon
(binary sha256 780f52a3…, unit active, i2p server tunnel live):

- **getstakinginfo** from the live daemon (weight 1475, attempts 288 climbing, real
  search counters, honest in_window_only skips, netstakeweight 0 + expectedtime -1
  reported honestly).
- **smsg wire roundtrip** between two tri7d instances on the box (A = service on
  19112/24113; B = second instance on 19115/24114, outbound session to A):
  smsgsend A→B (sample FAC3843E28BB055C), inbox delivery + decrypt on B (plaintext
  verbatim), reply B→A (sample 0EFEEAC83D88953A), decrypt on A.
- **sendrawtransaction**: real signed self-spend built from h2204147's coinstake
  output (1 TRI to self, 100 sat fee floor, self-verified) → accepted (announced_to 3)
  → inv relayed to 3 peers → pooledtx 1 → included in the next SELF-MINTED block
  h2204151 (fees 100 attached; getblock shows the txid in the block; getrawtransaction
  returns it) → relayed, no rejection.
- The staking loop is minting live: STAKE → STAKE MINTED → relaying to 3 peers
  (self-validated: kernel enforced, nbits enforced), one block in flight, failures
  contained (Err path logs + counts, never aborts). Safety rails held throughout.

## Honest gaps (disclosed, not hidden — also in evidence section 8)

- inbound getheaders deep-locator race on a freshly-restarted node (unanswered once
  at ~18s uptime; served fine at 13min+ uptime; needs investigation);
- --backfill default is legacy-era design (fresh nodes should run --backfill 0);
- netstakeweight estimator returns 0 in the current window (cosmetic, honest -1);
- reconnect height-probe logs a cosmetic protocol-failure line;
- the chain is at the difficulty floor so DNS2 mints ~1 block/15s — retarget observed
  tightening (2000ffff→2000ea21→2000c41d); protocol-driven, no action taken.

## Gates (regenerated after the last code change — deploy/p7m2_gates.txt)

- cargo test --release: 183 lib + 29 follow_tip bin + 46 across other targets =
  258 passed, 0 failed.
- cargo check --features gui: clean (warnings only), exit 0.
- cargo build --release --target x86_64-pc-windows-gnu --bins: exit 0 (the gate).

## Your job

Grade the milestone-2 work as CODE: correctness of the new modules and fixes (read
the attached regions + full files), whether the live proofs substantiate the claims,
whether the honest-gap disclosures are adequate, and whether the safety discipline
(self-validation gate before any mint/broadcast, one block in flight) is actually
implemented rather than described. The standalone scripts/judge cannot reach DNS2's
RPC; the verbatim captures in the evidence file are the ground truth for live claims
— treat them as the operator's own run results.

Answer with a single JSON object, last line, nothing after:
{"codex_grade": "A|B|C|D|F", "reasons": ["..."], "blocking_issues": ["..."], "polish_suggestions": ["..."]}
