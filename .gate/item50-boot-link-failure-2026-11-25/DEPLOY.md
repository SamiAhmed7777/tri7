# item 50 closeout — fleet deployment of the item-49+50 build (52180af)

Commit `52180af` (grade A, urn:ump:pthfgs74qnm3felrufe2hf46iad5tiuqzu3q4gjq2oyayaprrhcq)
carries BOTH fixes: item 49 (pending body journal + interior archive-hole
audit/repair) and item 50 (boot backfill treats a served body that fails
validation as a peer fault — skip + counter, never exit 4), plus the CI tor
pin bump 16.0.1 → 16.0.3. This brief covers the deployment of that build to
the live fleet and the verification, plus two operational findings.

## Deploys (harness tri7d_swap_verify_rollback.sh + check balances_chain_ordered.py)

- **DNS3 canary** (`ev_deploy_dns3_r3.txt`): old 60ee7604 → new 6f4f3411,
  boot 15:41:47Z, **accepted** 15:45:31Z ("check passed", restart_epoch
  1795621217). The two earlier attempts (r1 `ev_deploy_dns3.txt`, r2
  `ev_deploy_dns3_r2.txt`) exited 4 on the backfill link failure — that is
  the item-50 defect itself, filed and fixed this tick. The boot that
  matters: backfill "100 blocks fully validated (kernel enforced 45 /
  seeded 0)", RPC up 18 s after start, no exit.
- **item-49 fix verified LIVE on DNS3**: the persistent h2219461 archive
  hole (survived 43+ min and multiple boots on the old binary) was
  repaired by the new audit in ONE second: "GAP RECOVERY: interior archive
  hole h2219461 … requesting getdata" (15:43:36Z) → "gap: getdata for
  h2219461 (interior archive hole) sent to 1 peer(s)" → "GAP RECOVERY:
  interior archive hole filled — body for h2219461 … appended to the store
  from out:ra32xf5ogzq2ialy" (15:43:37Z). The balances walk then replayed
  the WHOLE chain past the old hole frontier and the check accepted. The
  pending journal left no *.pending file after the drain.
- **Pi** (`ev_deploy_pi.txt` r1, `ev_deploy_pi_r2.txt` r2): built ON the Pi
  (aarch64) from the rsync'd committed tree, 6d82ac22. r1 ROLLED BACK —
  reason "check still pending after 900s" (operational finding 1 below);
  no binary defect: the node ran 15 min, followed the tip, audit sweeps
  clean. r2 accepted 16:09:14Z after the fix; the fresh process replayed
  the whole chain in 72.4 s ("coverage h2219658 … index order", snapshot
  rewritten 16:09:10Z).
- **DNS2** (`ev_deploy_dns2.txt`): old 60ee7604 → new 6f4f3411, boot
  16:14:04Z, **accepted** 16:19:07Z ("index order, coverage h2219662 at or
  above the chased tip"). Balances replay 148.1 s from the invalidated
  cache (snapshot h0 → coverage h2219662, supply 226993898137 sats, wallet
  263 keys 5250504286 sats, 18038 live UTXOs). Archive audit sweep 1:
  0 holes. Staking live again (attempts 47984, coins_considered 109).

## Fleet verification (16:20Z, read-only)

All three nodes `active`, tips UNANIMOUS at h2219662, DNS2 staking:
- DNS2: 6f4f3411 (x86_64), 3 peers
- DNS3: 6f4f3411 (x86_64), 4 peers
- Pi: 6d82ac22 (aarch64, same commit 52180af), 1 peer
(`ev_fleet_verified_1620Z.txt`)

## CI

Run 45647317384 on 52180af: linux-tests/macros-build/linux-gui-tarball
success; the windows-build step "Fetch bundled tor (pinned 16.0.3 + SHA256
gate)" SUCCEEDED (the 404 that killed run 45620664321 is gone); the job
was still running its slow Windows test step at close time — the tor pin
gate itself is verified green by the completed fetch step.

## Operational findings (recorded for the next deploy)

1. **Fresh-snapshot nodes cannot pass the acceptance check**: the check
   itself documents it ("Only a node whose on-disk snapshot is stale runs
   a build after a restart"). A node whose snapshot is <5760 blocks
   behind and not capped builds nothing after a restart → the check stays
   pending → the 900 s deadline rolls a HEALTHY binary back (live: Pi r1).
   Fix used: invalidate the derived cache (`mv balances.json
   balances.json.pre-item50`) before the swap so the new process runs a
   REAL full replay the check then verifies. Evidence is not synthesized:
   the build is genuine work by the new binary.
2. **`item49_deploy.sh`'s dns2 leg is broken**: with R="" the line
   `$R "cmd"` executes the whole quoted string as one program name
   ("No such file or directory"). The local variant
   (`item50_deploy_dns2.sh`) runs the same steps without the ssh wrapper.
   Also: getmoneysupply is not an RPC method on this node (gettxoutsetinfo
   is); the Pi's repo checkout stays at ad31567-dirty because item-48+
   ticks rsync the committed tree over it rather than git pull.

## Files

- ev_deploy_dns3_r3.txt — DNS3 canary accepted + hole repair journal
- ev_deploy_pi.txt, ev_deploy_pi_r2.txt — Pi rollback (pending) + accepted
- ev_deploy_dns2.txt — DNS2 accepted
- ev_fleet_verified_1620Z.txt — final read-only fleet verification
