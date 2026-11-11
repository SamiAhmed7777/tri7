# 2026-11-08 COIN-SPLIT — the shared-bag split + the mixed-solver spend fix (workqueue item 10)

Item 10's goal: DNS2 and sami-pc must stake DISJOINT coin sets (one owner per
coin, skill section 1) — the 2026-11-05 AND 2026-11-08 forks both trace to the
two nodes staking the same seed-lineage bag. This record covers what the split
actually required, the blocker bug it caught, and the zone rules that keep the
invariant alive.

## Measured starting point (2026-11-08 ~03:45Z)

- Full wallet key census (dumpwallet → `# addr=` field ONLY — the WIF lines
  contain base58 that matches naive `T…33` regexes; 4 of 6 "exclusive" hits on
  an unanchored scan were WIF substrings): DNS2 252 keys, sami-pc 250 keys,
  shared 250, **sami-pc-only keys: ZERO**, DNS2-only: 2.
- PC staking OFF since the 2026-11-08 fork repair; tip parity DNS2=PC=Pi at
  the repair tip; DNS3 forked again (see below).

## The zone architecture (and why plain refills can't give both sides HD keys)

Same-seed HD chains derive IDENTICAL keys at the same index, and every
keypoolrefill sweeps SEQUENTIALLY from the wallet's own counter — so the LAST
wallet to refill owns every derived key up to its counter, and the other
side's freshly derived band gets swallowed (verified live: the PC's refill to
hdchain 272 re-derived DNS2's two change-address keys, taking DNS2's
exclusive count 2 → 0; had coins been sent to them first they would have
become shared AGAIN — always re-diff BOTH wallets after every refill and
before any send).

The working architecture:

- **PC zone** = the PC's HD keys above DNS2's counter (DNS2 counter frozen at
  125; PC refilled to 250 → hdchain 122→272; 149 keys derived, of which 2
  collided with DNS2's pre-existing change-address keys → 147 net PC-only,
  verified by post-refill set-diff). Index convention: hdchain "next index"
  122→272 means indices 122..271 were DERIVED (149 keys, the counter is
  exclusive); of those, the 2 that collided with DNS2's pre-existing
  change-address keys were dropped from the net count → 147 usable
  exclusives.
- **DNS2 zone** = keys from a FRESH INDEPENDENT 24-word seed
  (`examples/wallet_from_seed` → `wallet_keytool dumpwallet` → RPC
  `importwallet` into DNS2's wallet; 8 keys, label `seed/0..7`, zero
  collisions with either wallet). Phrase file `/var/lib/tri7/zone2-phrase.txt`
  (0600) — deliver to the operator for LastPass like the Pi/DNS3 phrases.
  These are unreachable by ANY refill sweep — permanent exclusives.
- The 6 former "DNS2-only" strings included 4 regex false positives; the 2
  real ones were HD change addresses the PC's refill then swallowed. Lesson:
  the set-diff over full wallet FILE dumps is the only ownership oracle;
  `validateaddress.ismine` reads the STALE balances.json snapshot (252-era)
  and lies for freshly added keys until the snapshot rebuilds.

## Blocker found by the first drain (fixed in commit 6ed553f, grade A)

`build_wallet_tx` self-verify zipped the tx's (txid,n)-SORTED vin against the
UNSORTED selection list (the 2026-11-07 fix sorted only the connect check).
Same-type coins masked it for a year of sweeps (twin scripts verify
identically at any position); the first MIXED-solver selection (bare-P2PK
coinstake rewards + P2PKH sweep outputs on one address) failed with
"scriptSig has 2 pushes, Solver expects 1" on a CORRECTLY SIGNED tx. Fix:
one `ordered_sel` sort feeding BOTH verify_input and connect_inputs_check +
regression test `self_verify_zips_vin_sorted_order_not_selection_order`.
All four nodes carry the fixed binary (DNS2 local build 5cf6b583, PC+DNS3
CI 42446144052 artifact / same-hash scp, Pi on-device rebuild).

## Drain mechanics (what the sweeps did)

- sendtoaddress returns the bare txid STRING (legacy parity) — parse as a
  string, not an object.
- `sendtoaddress addr amt dryrun` reveals fee + change for the SELECTED coin
  set: exact zero-change amount = `amt += change_sats` (1-3 iterations). A
  "have X, need Y" insufficiency means fresh stake rewards are immature —
  back the amount off by the deficit and retry; the residue drains once the
  coins mature (a follow-up pass pattern, NOT a same-session hammer: re-broadcasting
  against pending pool txs gives -25 conflicts and the driver may re-send the
  same coins twice — wait for mining between sends, ALWAYS).
- The builder drains "the richest address that covers the amount" — the
  caller's src label is advisory; verify zone totals, not source labels.
- Coin-age reality: every DNS2 stake ROLLS an address's whole balance into a
  fresh (immature) output, so a drain can only take a mature window
  (~30+ blocks after the last roll). Expect several small sends instead of
  one big one; keep re-converging until the seed census reads dust.

## Fleet events during the split (all resolved)

- **PC staking came back ON after a restart** (attempts 845k): the state2
  conf had staking=1 in the FILE even though the earlier RPC-off verified
  in-memory; every restart re-reads the file. The ft_state2.bat does NOT pass
  --stake, so the RPC persistence (state2\\triangles.conf) IS authoritative —
  but you must RE-SET it via the RPC after any restart and verify
  `enabled:false AND attempts frozen`. During its ON window the PC solo-minted
  2207534..2207540 on shared coins (its live view still held the seed keys) —
  canonical (DNS2+Pi) rejected the branch; the PC was re-pointed by copying
  DNS2's tipstate.json over its own (rename the PC branch tipstate aside,
  rename index aside — no deletes needed), restart, re-follow. PC solo
  rewards on the abandoned branch are discarded (one-owner-per-coin).
- **DNS3 forked AGAIN** after its item-10 canonical re-copy: its unit runs
  `--stake` and staking enabled on the fresh store → it solo-minted its own
  branch (its wallet's single 100 TRI coin). Its `staking "off"` RPC errored
  on the read-only /etc conf but DID flip the in-memory flag (enabled:false);
  the daemon was stopped and the conf fixed as root (`staking=0`). DNS3
  rejoins by re-copying DNS2's tipstate (its store already holds canonical
  frames) with staking OFF until it passes the three checks.
- **DNS3's peer is sami-pc tailscale** — when the PC forked, DNS3 had no
  honest oracle. After repairs, DNS3's peer target should be DNS2 (onion via
  its tor socks), not the PC, so a PC fork cannot strand it.
- **Pi's staking was found OFF** (silent regression since its item-8 rebuild
  — the generated conf ships staking off and nothing re-enabled it).
  Re-enabled via `staking "on"` (persisted to /root/.triangles/triangles.conf).
- Post-split minting slowed sharply (multi-minute lulls): the bag is halved
  per staker and every stake roll re-immatures the coins it touches. Expected
  and fine — the retarget loosens as spacing grows. Pi's 100 TRI coin going
  immature after its own mint pauses Pi for ~30+ blocks; not a fault.

## End state (2026-11-08 ~07:2xZ at the tick's close — drain tail in flight)

- Seed 10-address set: 3 addresses were fully drained before the JSONL log
  began (their sends broadcast from crashed driver runs whose log lines never
  wrote — recovered from sent_txs.json), so the log shows 7; the remaining 7
  are mid-maturity-cycle with ≈420.55 TRI left at the final read
  (150.014434 + 120.897575 + 46.473951 + 35.501494 + 37.768665 + 27.242820 +
  2.649969), ground to dust by the auto-relaunching drain wrapper (the next
  tick verifies the dust census and enables PC staking). The shared-key coin
  set shrinks with every mature window — the fork root cause is being
  removed permanently.
- PC zone (PC wallet only): TA5SHCmj…, TA9DVnUq…, TABe4ay5…, TAGKGAiL… —
  ~2.0k TRI after the accidental extra 305.719053 send
  (38ceabbd…, see below).
- DNS2 zone (DNS2 wallet only, zone2 seed): TVVcEE95…, TVkRs3MF…, TE5AnHMj…,
  TQYsQtEd…, TGNJduic… — the remainder.
- Rewards land on the coin's own address (stake.rs pays the kernel coin's
  script back), so each zone's rewards stay in-zone: the invariant is
  self-sustaining.
- sami-pc staking: turned OFF via the RPC during the shared-coin window
  (verified enabled:false + attempts frozen); the NEXT tick re-enables it via
  the RPC once the dust census reads clean and its tip re-locks to canonical
  (staking=true persists to state2\triangles.conf; ft_state2.bat passes no
  --stake, so the conf is authoritative across restarts — but re-assert after
  any restart anyway).

## Standing rules added

1. NO keypoolrefill/getnewaddress-driven derivation on DNS2 without a fresh
   two-wallet set-diff; PC refills are free (its counter only rises) but every
   zoned DESTINATION must be re-verified: dump-diff both wallets, then
   send.
2. `validateaddress.ismine` is snapshot-gated — for ownership questions use
   the wallet FILE set-diff (dumpwallet → `# addr=` lines) or
   `dumpprivkey` success/failure, never ismine.
3. Never run overlapping sends from one wallet; one tx per mature window,
   mine-gated.
4. After ANY daemon restart on any fleet node, re-read getstakinginfo and
   re-assert the intended staking state via the RPC (conf files lie, and the
   item-9 "off" did not survive a restart even with the RPC persisting it).
