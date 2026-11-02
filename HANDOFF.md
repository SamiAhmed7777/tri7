# tri7 modern-client loop — session handoff (2026-11-02, phase 5a)

## State
Repo: /root/tri7 (Rust, phases 1a..4 committed; phase 5a deploy — see below).
Live chain ref: /root/.triangles (DNS2 daemon v6.2.6.7, RPC 19112, P2P 24113, tip 2204064 static since 2026-09-24, staking=0).

## Phase 5a (2026-11-02): tri7d systemd packaging on DNS2 — DONE, judge grade B
- follow_tip packaged as tri7d.service, RUNNING on DNS2: `systemctl enable --now` 2026-11-02 10:25 PST.
  Unit /etc/systemd/system/tri7d.service (source-of-truth copy in deploy/): User=tri7 (system, nologin, uid 983),
  ExecStart=/opt/tri7/bin/follow_tip --daemon 127.0.0.1:24113 --rpc-port 19112 --conf /etc/tri7/triangles.conf
  --state-dir /var/lib/tri7 --forever, Restart=on-failure RestartSec=30, MemoryMax=256M, ProtectSystem=strict +
  ProtectHome=yes + ReadWritePaths=/var/lib/tri7 + PrivateTmp + kernel guards. Outbound-only loopback P2P+RPC;
  no staking, no broadcast, no listening sockets.
- Layout: /opt/tri7/bin/follow_tip (root 0755, sha256 9f131ec75189a50a…b3); /var/lib/tri7/tipstate.json
  (tri7 0600, seeded from /root/tri7/tipstate.json @h2204064, window_base 2199465); /etc/tri7/triangles.conf
  (root:tri7 0640 FULL conf copy — tri7 has NO /root access; archive arg unused by follow_tip). Idempotent
  installer deploy/install_tri7d.sh; deploy/ holds unit copies + evidence + notes.
- follow_tip.rs changes: --status healthcheck (file-only, JSON: height, last_advance_utc, state-write age,
  daemon_tip, at_daemon_tip; exit 0 healthy / 3 unreadable-or-stale via --max-write-age default 600 — the 30s
  write cadence is the liveness signal on a static chain; last_advance_utc bumps ONLY on real tip advances,
  serde(default) back-compat), --forever (no run deadline — required for Restart=on-failure semantics),
  daemon_tip persisted on every write (refreshed each poll tick), atomic write pins 0600 on tmp before rename
  (v1 bug: fs::write 0644 clobbered mode every 30s), negative-age guard. 102/102 tests, rustfmt clean.
- Wired healthcheck: tri7d-health.timer fires tri7d-health.service every 5 min; failed check →
  `systemctl try-restart tri7d` (wedged-but-running follower auto-recovered). First fire 10:30 Result=success.
- Verification: active/running, NRestarts 0; tipstate mtime 10:26:22 → 10:27:22 (60s-apart checks), write age ≤30s
  throughout, height 2204064 == daemon getblockcount (at_daemon_tip true — chain static, staking off, so height
  CANNOT advance; state refresh + polls #1..#5+ = liveness evidence); RSS 10.4 MB current / 13.5 MB peak
  (vs MemoryMax 256M; legacy trianglesd 1494 MB); legacy trianglesd UNAFFECTED — same PID 3315888 (running since
  05:44:51, before any tri7d work), RSS/ports (24113/19112) unchanged, api/bootstrap/explorer services active.
- Codex gate (kind=deploy): v1 grade C (status semantics conflated advance vs write clocks, no staleness exit,
  daemon_tip null) → v2 grade D (judge-sandbox uid-mapping artifact claimed perms broken [real perms were fine —
  see deploy/tri7d_deploy_evidence.txt numeric ids]; REAL: 0600 clobber + no recurring health wiring) →
  v3 grade B, URN urn:ump:gexqh6ra3uag4qmylgpjxvoc7vzwifrgtzs5dbnlja46chjt5fwa. Judge sandbox note: codex exec
  user-namespace maps uid 983→nobody and can't read 0700 dirs — attach verbatim numeric-id evidence files.

## Phase 4 (2026-11-02): staking mint (coinstake creation) — DONE, dry-run only, no network
- src/stake.rs: exact port of the mint stack — CreateCoinStake (wallet.cpp:2225-2441: min-age coin filter vs block time, backward per-second probes n<min(searchInterval,60) over CheckStakeKernelHash with the fixed-600s modifier walk from the INPUT's block, Solver key resolution (P2PK kernel reuses the kernel script, P2PKH pays bare-P2PK of the wallet key), split rule nStakeSplitAge=10800 (vout==3 → cent-rounded half), combine pass nStakeCombineThreshold=20*COIN keyed on PREV-TX-time weights, coin-age reward GetCoinAge main.cpp:3082-3140 (cent-seconds per input, containing-BLOCK-time min-age skip), GetProofOfStakeReward main.cpp:1539-1560 (CBigNum order: (nCoinAge*330000/365)/COIN + fees), per-input SignSignature via spend::, size cap MAX_BLOCK_SIZE_GEN/5), CreateNewBlock PoS template (coinbase height scriptSig push_script_int64 minimal-signed-LE + empty COINBASE_FLAGS, SetEmpty output, nBits=GetNextTargetRequired(prev,true), nNonce=0), SignBlock timestamp protocol (coinstake.nTime >= max(MTP+1, PastDrift), block time == coinstake time, future-tx erase, vtx[1] insert), vchBlockSig = bare DER ECDSA over the Hash9 header with the coinstake output key, and validate_dryrun_block — the full local acceptance path (link/structure/merkle/MTP+drift/nBits/kernel via check_proof_of_stake/block sig/finality/ConnectBlock reward gate/size).
- src/bin/stake_mint.rs: DRY RUN driver (exit 0 complete / 2 usage / 3 chain-verify / 4 self-validation fail / 5 input). Single read-only archive pass: sequential prefix genesis..2199464 (modifiers recomputed per validate_pos, genesis checksum 0x0e00670b re-verified) + live tail 2201091..2204064 reconstructed by Hash9-MATCHING frames against the tipstate entry hashes (frame order is not chain order across the 58 seams). Modifier ORACLE: replayed modifiers must equal tipstate's daemon-anchored values at ALL 4600 window heights (hard die on any mismatch); checksum chain anchored at h2203924 (daemon FastImport boundary — ADOPT the first non-zero checksum, verify the 140 steps after). Wallet live set at tip = wallet_utxos.json (h2201090 snapshot) + tail reconciliation (spend removals + wallet-payload-attributed additions, additions themselves spend-filtered). Kernel search → assemble → sign → SELF-VALIDATE (incl. spend::verify_input on every coinstake input) → /root/tri7/stake_dryrun.json. NOTHING broadcast; no sockets; /root/.triangles only read.
- Dry-run result (tip h2204064): live wallet 178 UTXOs / 3,078,788,707 sats; kernel SOLVED after 362 attempts (coin 1c3fba13…d27:2, 12,010,466 sats @h2203769, nTime now−19s, modifier 0 — the daemon index itself stores 0000…0 at walk-stop h2203779, post-stall block, oracle-verified); candidate block 8673348439529e0bc66e026d8b4a7afe4853c2726420a343ff60837e30b83dcd (nBits 2000ffff = era clamp, coinDays 6, reward 483,243 sats) — self-validation PASSED end to end.
- **BUG FIXED (phase-1c-era latent): consensus::MAX_TRI_PROOF_OF_STAKE was 33,000,000; C++ main.h:62 = 0.33*COIN = 330,000. Never exercised before phase 4. Chain-verified: 48/48 (and 1038/1038 on a wider sweep) live coinstake rewards match 330000/365/COIN per coin-year, 0 match the old value. Live-derived vectors now anchored in stake::tests::stake_reward_matches_cbig_num_division_order.**
- Tests: 102/102 (57 lib incl. 11 new stake tests: easy-target solve + full self-validation, impossible-target exhaustion + attempt counting, min-age filter, IsCoinStake gates, CBigNum division order vs U512 reference + live vectors, coin-age cent-second semantics, split rule, combine pass, block-sig roundtrip + tamper, SignBlock timestamp floor, coinbase height prefix).
- Caveat: wallet_utxos.json is a partial snapshot (177 UTXOs at h2201090 vs phase-1e's 17,769) — the tail reconciliation re-derives liveness from chain data, but a fresh `balances --utxos-out` re-dump would widen the candidate set. Report: /root/tri7/stake_dryrun.json, /root/tri7/logs/stake_dryrun.log.

## Phase 3 (2026-11-02): wallet tx creation + signing (spends) — DONE, no network
- src/spend.rs: exact port of the legacy signing stack — SignatureHash (script.cpp:1150-1210, the 0.1.6-era one-input-script scheme: serialize with all scriptSigs blanked except nIn=scriptCode, append nHashType as 4-byte LE int, dsha256; SIGHASH NONE/SINGLE/ANYONECANPAY branches incl. CTxOut::SetNull = value -1), CScript::FindAndDelete ([0xab] needle, GetOp-walking erase), CheckSig (hashtype = trailing sig byte), Solver script builders (P2PK 1-push / P2PKH 2-push scriptSigs), strict+lax DER codec, sign_input (RFC6979 via rust-secp), build_spend (CreateTransaction fee loop: nFeeRet starts at nTransactionFee=MIN_TX_FEE default main.cpp:450, raised to max(nPayFee, GetMinFee(1,Send,nBytes)); size cap MAX_BLOCK_SIZE_GEN/5), connect_inputs_check (ConnectInputs main.cpp:1980-2067: maturity nCoinbaseMaturity=7, entry.nTxTime <= nTime, MoneyRange, value-in >= out, fee >= GetMinFee; ECDSA before fee tally like the C++), select_utxos_covering (largest-first deterministic).
- EMPIRICAL ANCHOR (the critical piece): corpus/historical_sigs.json + src/bin/historical_sig_corpus.rs — real mint-era signatures from blk0001.dat re-verified through signature_hash+ECDSA: 799 strict-verified (775 P2PK-prev + 24 P2PKH-prev) over 20 distinct spending txs (heights 9002..10329), 0 hard failures. Chain reality: regular txs carry ZERO bare-P2PK outputs anywhere (sig_probe.rs full pass) — P2PK prevouts come only from coinstakes, so the corpus spends coinstake P2PK outputs with regular spender txs + P2PKH regular prevouts; SignatureHash is prevout-script-agnostic. The known-broken pre-strict-ECDSA txs (h<=17691 CRAPCHAIN era, e.g. a0343701... at h10297 whose 58 sigs fail) fail NEXT TO verifying neighbors in the same block — root-caused, era-scoped counter, do NOT re-open. tests/spend_corpus_tests.rs re-verifies all 30 corpus entries (>=20 distinct spenders, strict DER, recorded sighash reproducible).
- Signed dry runs: src/bin/spend_dryrun.rs — synthetic P2PK→P2PK, P2PKH→P2PKH, 2-input consolidation, Solver shape checks, negative controls (tampered sig, depth-6 coinbase, future tx_time), written-file byte-for-byte re-verify. Mode B (--utxo-dump wallet_utxos.json --wallet /root/.triangles/wallet.dat): builds+signs from a REAL wallet UTXO (balances --utxos-out dump) to a synthetic destination, verify-only. NO network code in any of this; nothing broadcast; daemon untouched.
- balances.rs grew --utxos-out <path>: every live wallet-owned UTXO (txid display order, n, value, script_hex, address, height, tx_time, prev_kind) with UtxoSet-mirroring lifecycle (per-tx all-or-nothing spend removal).
- 90/90 cargo tests pass (46 lib incl. 16 new spend tests + 1 corpus test). Do NOT broadcast; do NOT commit without the codex gate.

## Phase 2 (2026-11-02): legacy P2P protocol client — DONE against the live daemon
- src/p2p.rs: exact wire framing — magic [70 35 22 05] (main.cpp:4849, same bytes as the blkfile magic), command12 null-padded (IsValid ASCII rule), len u32 LE, checksum = dsha256(payload)[0..4] LE; version/verack handshake (PROTOCOL_VERSION 70206, MIN_PROTO 70205; version-msg CAddress carries NO nTime because the version stream version is INIT_PROTO_VERSION 209). The fork's CService wire = ip[16] + tor_v3_pubkey[32] + is_tor_v3 + is_i2p + port BE → 60 bytes per version-msg CAddress (NOT bitcoin's 30). 10 wire unit tests (roundtrip, checksum golden vectors, compactsize paths, header-only entries, IsValid rejection cases).
- src/bin/follow_tip.rs: outbound-only follower to 127.0.0.1:24113 (conf port=24113; net.cpp default 24112). services=NODE_NETWORK (fClient peers get NO block announcements, main.cpp:6253). sendheaders → headers announcements; inv → getheaders reconcile (never blind getdata); ping→pong; ignores getdata/getaddr/etc — read-only.
- Seed (tipstate.json): window of 4600 entries + 1500 full-block records. Raw blocks pulled over P2P getdata and verified byte-for-byte locally (parse+Hash9+link+merkle) in ~6s. Modifier/proof/generated seeded from daemon getblock and verified via the stake_modifier_checksum CHAIN — anchored at h2203924 (daemon stores 00000000 checksums below that; 140 steps verified to the tip).
- Window mechanics: IndexEntry.height is WINDOW-RELATIVE (base+index=absolute) — the difficulty same-type walk (last_block_index) indexes by e.height-1; absolute heights break it (live-found bug → spurious 207fffff). Historical blocks validate against rel_index_to(h-1) truncation. Kernel resolver = window OUTPUT INDEX (insert-only, never removed on spend — the C++ kernel reads txPrev from txindex ignoring spentness; UTXO semantics resolve almost nothing → OutputIndex pattern).
- Live validation run (logs/follow_tip_final2.log): handshake OK, 4600 seed blocks verified, 3000 backfill headers (hash9+link), 100 backfill blocks FULLY validated (nBits EMA enforced on all; 46 kernels enforced locally / 54 checksum-verified RPC proofs for pre-window prevouts), then 30s polls (getheaders locator alternates — the daemon dedups identical consecutive getheaders, PushGetHeaders filter).
- tipstate.json {height, blockhash} + entries/blocks; atomic tmp+rename; re-verified on load (checksum chain + linkage).

## Gotchas (phase 2 additions)
- Daemon blk0001.dat has 58 interleaved reorg seams around h2201698..2201750 — sequential prev-link frame walks CANNOT reconstruct the live tail; don't seed from frame order.
- getblock verbose flag is ignored (always verbose JSON; no raw-hex RPC). JSON-RPC batch (array) works.
- Daemon modifierchecksum = 00000000 below h2203924 (FastImport didn't persist them); anchor the chain there.
- Daemon flag string carries "stake-modifier" when BLOCK_STAKE_MODIFIER is set (= generated).
- The daemon does NOT push its own version on inbound connect until it hears ours ("be shy"), then sends version+verack in one burst.
- smsgPing messages arrive after handshake — ignore.

## Done (all validated against live chain)
1a: legacy wire deserializer + blkfile reader (LE magic/length!) + Hash9 via verbatim sphlib FFI. Genesis + 2,201,091 blocks hash/link/merkle-verified vs daemon RPC.
1b: consensus classification (IsEmpty = value 0 + empty script, NOT -1!), sig gates, SQLite wallet reader (248/248 keys decode+verify; 12 label-ghost addresses = ismine-false on daemon too, 0 lifetime receipts).
1c: difficulty — CBigNum compact port (U512 for overflow at v5 fork), EMA, era seams: 0x2000ffff limit through 616604, then 207fffff; fork resets at 17651 (207fffff) and 2186942 (2000ffff); repair-block boundaries 17692/17693/2186941. 2,183,396 blocks nBits-enforced ALL matched. 46/46 tests.
1d: PoS kernel + stake-modifier replay (validate_pos EXIT 0; 191/191 live-era kernels; modifier chain 0 mismatches vs daemon index; genesis checksum 0x0e00670b MATCH; UTXO layer 2,626,455 entries 0 conflicts; 56/56 tests).
1e: per-address balances from full-archive replay (src/bin/balances.rs + src/address.rs; replay skeleton factored into replay::ArchiveWalker, validate_pos refactored onto it, full re-run EXIT 0 same counters). Result @2201090: total live balance 222,507,326,452 sats in 17,769 UTXOs, ALL 2,626,455 outputs attributed (0 untracked spends, 0 dup txids). Daemon cross-check: bridged moneysupply@2201090 (getinfo 222,549,124,568 − 41,798,116 mints of daemon-only 2201091..2204064) = 222,507,326,452 — EXACT, gap 0. getreceivedbyaddress(addr,0) (regular-tx P2PKH-only semantics, rpcwallet.cpp:497): 248/248 wallet addresses EXACT. balances.json + rpc_crosscheck.json + scripts/rpc_crosscheck.py. 61/61 tests.

## Next (in order)
2 ✅ P2P follower (committed 614236f grade B).
3 ✅ wallet spend (committed ba35f06 grade B): spend.rs (SignatureHash 0.1.6 scheme + FindAndDelete + DER + Solver builders + ConnectInputs checks), 799/799 historical sigs verified, corpus fixture in-repo, real-UTXO dry-run signed+verified broadcast=false, PUSHDATA boundary tests.
4 ✅ staking mint dry-run (committed 1d1a982 grade B): stake.rs CreateCoinStake/SignBlock/GetCoinAge/reward ports; LATENT BUG fixed (MAX_TRI_PROOF_OF_STAKE 33,000,000→330,000, chain-verified 1038/1038 live coinstakes); kernel SOLVED at live tip (362 attempts), candidate block fd5e471d… self-validated; 102/102 tests.
5 ✅ DNS2 packaging (this phase, grade B). Remaining: staged deploys sami-pc → DNS3 → laptop. LIVE STAKING (broadcasting minted blocks) = Sami's explicit go/no-go.
Grades: 1a B, 1b B, 1c B, 1d B, 1e A, p2 B, p3 B, p4 B — all codex-gated.

## Gotchas
- Kimi CLI: `kimi -p` works; --auto/--yolo cannot combine with -p.
- Wallet records: suffixes/values carry CompactSize prefixes everywhere (strip before use).
- Era boundaries in archive: stale branch starts at 2201091.
- Judge (codex) requires: bins in src/bin, coverage-contract exits, no hand-math assertions. Cycle D→D→C→C→B pattern per phase; expect ~2-3 iterations.
- The 12 ghost labels + 9003-era sigs are ROOT-CAUSED (vestigial labels ismine-false on daemon; CRAPCHAIN-era pre-strict-ECDSA). Do not re-open.
