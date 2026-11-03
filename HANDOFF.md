# tri7 modern-client loop — session handoff (2026-11-02, phase 6)

## TOPOLOGY (authoritative — 2026-11-02 phase 6, remediation)

**tri7d IS the DNS2 primary node. The legacy trianglesd is retired
(stopped+disabled). Any other description is history, not current state.**

| Port | Role |
|---|---|
| 19112 | tri7d native JSON-RPC (loopback only; `--no-proxy`; 16 native methods incl. getbestblockhash) |
| 24113 | tri7d inbound P2P listener (bind 0.0.0.0; ufw allow v4+v6; tor HS gxvrhv3qitnc6kobrhsrse46bmcfitnybapor3or3oczzuxn6hfzxyid.onion:24113 → 127.0.0.1:24113) |
| 19113 | autossh tunnel → sami-pc:19112 RPC (triangles-api consumer) — unrelated to tri7d, never bind it |

Outbound attach targets (rotation with fallback; .onion via tor SOCKS
127.0.0.1:9050): sami-pc 100.85.236.10:24113/:24112, DNS3 100.81.59.99:24112/:24113,
DNS3 onion i6tk7soznftvoibtskwlezviskiererhjndpsmrff4kaxw7jnd5izfqd.onion:24112.
All targets down ⇒ HEADLESS (native RPCs from persisted state, rotation retried
each 30s poll tick — POLL_SECS in follow_tip.rs).

Inbound peers: server-side handshake (they send version first; we reply
version+verack; non-handshake messages received during the handshake are
BUFFERED and replayed — a block pusher fires immediately after its verack),
getdata served from recent-blocks cache (heights above the frozen
blk0001.dat) or the disk store, getheaders served (2000-header cap), pushed
blocks validated by the same consensus path and RELAYED to all peers
(inv → getdata). A malformed/invalid message from an INBOUND peer drops the
PEER (Misbehaving-style), never the process; from an OUTBOUND target it is
fatal (fork-safety stance unchanged).

The loop holds the state + recent-cache locks across a validation batch, and
post-cutover the "daemon RPC" is tri7d ITSELF — the RPC threads therefore
never block on those locks: tip served from a lock-free mirror under
contention (TipMirror), recent cache via try_lock, and the stake-modifier
checksum cross-check skips (logs "no external checksum oracle") for blocks
above the disk store's tip, where tri7d is the chain authority.

Healthcheck (tri7d-health.timer, 5 min): exit 0 healthy; 3 on unreadable
state, stale write (>600s), OR degraded = no peer traffic for >600s AND no
tip advance for >1800s (a frozen+isolated network primary is a failure).
--static skips the network-quiet gate. Degraded ⇒ try-restart tri7d — with
the fleet dark this restarts tri7d every 5 min BY DESIGN. last_peer_utc is
persisted IN tipstate and INHERITED across restarts (a fresh process ages
the gate from the last known traffic, never from "no data").

## Fleet reality check (2026-11-02 23:00 UTC, probes + SSH)

The premise "sami-pc/DNS3 advance" was FALSE at remediation time:
sami-pc has NO triangles/tri7 process (netstat/tasklist over SSH, wallet.dat
mtime 2026-10-19), the ssh-alias dns3 (appt/74.208.167.19) has no daemon,
dc-contabo-de is a DashCaddy test box, Hetzner stopped+disabled, samihost
unreachable, and ALL fleet onions refuse through tor. Chain advancement on
DNS2 was being produced by tri7's OWN stake_mint live runs (c1..c3, blocks
h2204065..67) pushed into the legacy daemon's P2P path. Chain advancement
was therefore restored via tri7d itself: stake_mint c4 pushed
block 0a5b2288894c4c2bda2c0dcd85d42483c0a4498f386fa535b64977182b12764c into
tri7d's 24113 listener → validated (kernel enforced) → TIP ADVANCE
h2204068 → relayed (inv) to the connected peer in +2.0s (ACCEPTED).

## Phase 6 (2026-11-02): LIVE STAKING + RPC cutover — DONE on DNS2

### Live staking (legacy trianglesd up as validator/relay)
- stake_mint extended with `--submit-to-legacy`: after self-validation → POST
  submitblock to the daemon → **the V6 fork REMOVED submitblock** (dispatch
  table rpcmining.cpp:142 comment; running binary strings have NO submitblock;
  live probe: `{"error":{"code":-32601,"message":"Method not found"}}`). Code
  captures that and falls back to the daemon's P2P `block` message
  (main.cpp:4366 → same ProcessBlock gate; daemon relays only what it accepts;
  anti-spam Misbehaving(100) is `if (false && …)` disabled; generic failures
  carry block.nDoS=0 → no peer penalty). Wire form: header+vtx+cs(vchBlockSig),
  tri7 p2p.rs framing; handshake = follow_tip's version/verack.
- **3 blocks minted + ACCEPTED (0 rejections), each self-validated first:**
  1. h2204065 `4c0a55a39d48eeb47b86f62f245294c6010b4bfe986ac98332624c5d60bbd17b`
     reward 484,971 sats, kernel coin 1c3fba13…d27:2 (12,010,466 sats @h2203769)
  2. h2204066 `fab257170c06c1a824c6f6a0a5f01a38ea7313a1b90eaf27bebc3acd9c3d30f3`
     reward 1,026,552 sats, kernel coin f5481f6a…db:1 (14,530,000 sats @h2203776)
  3. h2204067 `0f67543090749ba77cebc918241e1e25553d10bd4ea7da395df8aabbada14a3f`
     reward 943,793 sats, kernel coin fba90ab3…25:1 (12,950,000 sats @h2203895)
  Total minted 2,455,316 sats. Daemon log: "ProcessBlock: ACCEPTED block 2204065+";
  peers pulled the blocks via getdata (relay confirmed). Reports:
  /root/tri7/stake_live_c{1,2,3}.json + logs/stake_live_c{1,2,3}.log.
- Safety held: one block in flight, daemon-tip RPC guard before every submit,
  abort on any failure (exit 6), wallet.dat read-only, no chain rewrite.
- **BUG FOUND + FIXED (follow_tip window slide):** after following a new tip,
  trim() drained window entries and bumped window_base WITHOUT re-basing the
  survivors' window-relative heights → tipstate entry heights +1 (all 4600),
  stake_mint panicked (entries[prev_h] OOB), rel_index's "index i == relative
  height" invariant broken (difficulty walk would read shifted neighbors).
  FIX: trim() re-bases entries (-excess) and WindowOutputs (rebase()); repaired
  the poisoned /var/lib/tri7/tipstate.json on disk (heights -1, tri7:tri7 0600
  preserved) with tri7d stopped; fix verified live on the NEXT slide
  (h2204066: base 2199467, contiguous). Tests 99/99.

### RPC cutover (per deploy/tri7d_rpc_cutover.md — EXECUTED section)
- tri7d now OWNS 19112 (unit: `--rpc-port 19112 --no-proxy`); 19114 released;
  P2P arg unchanged (--daemon 127.0.0.1:24113). trianglesd STOPPED + DISABLED.
- `--no-proxy` flag added: post-cutover the proxy target (conf rpcport=19112)
  would be tri7d ITSELF — proxy disabled → proxy-only methods return clean
  cutover-mode errors. Methods that LOSE service with the daemon down:
  getrawmempool, getmempoolinfo, getstakinginfo, getnetworkhashps,
  getwalletinfo, gettransaction. Everything else NATIVE (verified on 19112,
  incl. all 3 staked blocks via getblock/getblockhash/getrawtransaction).
- HEADLESS follow mode added (follow_tip): with 24113 gone, connect failures
  no longer kill the process (try_connect/Option<Session>) — tri7d serves
  native RPCs from persisted state, retries the attach each 30s tick, keeps
  the tipstate write cadence (healthcheck stays green, NRestarts=0).
- Autossh 19113 → sami-pc:19112 UNTOUCHED (triangles-api reads SAMI-PC through
  it; live ESTAB consumer confirmed). api/explorer/richlist/bootstrap services
  all active throughout.
- NOTE: with the legacy daemon gone there is no block validator/relay — live
  staking pauses until tri7d gains its own relay path (P2P to sami-pc/onion
  peers; the daemon-P2P submit path in stake_mint is the template). The chain
  tip is 2204067; blockindex froze there (blk0001.dat appends stopped).

## State
Repo: /root/tri7 (Rust, phases 1a..4 committed; phase 5a+5b deployed — see below).
Live chain ref: /root/.triangles (DNS2 daemon v6.2.6.7, RPC 19112, P2P 24113, tip 2204064 static since 2026-09-24, staking=0).

## Phase 5b (2026-11-02): tri7d native RPC surface — DONE, live on DNS2 (port 19114)
- src/rpc.rs (HTTP/JSON-RPC server + dispatch, loopback-only, 1MB cap, per-conn threads,
  Connection:close) + src/blockindex.rs (disk-backed indexes) wired into follow_tip as an
  in-process listener thread — design choice: single state owner (ChainState behind
  Arc<Mutex>, RPC takes short per-call locks), reuse of the legacy Rpc creds, one systemd
  unit. NOT committed yet (no git commit per instructions).
- NATIVE methods (envelope tagged "source":"tri7"): gettip getblockcount getblockhash
  getblock(hash|height) getrawtransaction getinfo getdifficulty getconnectioncount
  getstakemodifier getbalance listunspent listwalletaddresses getreceivedbyaddress
  validateaddress health. Proxy fallback ONLY as transitional ("source":"legacy-proxy"):
  warming/out-of-index misses + daemon-only reads (getrawmempool, getmempoolinfo,
  gettransaction, getstakinginfo, getnetworkhashps, getwalletinfo). Wallet-write methods
  NEVER forwarded (-32601).
- Blockindex: hash→offset (24B/frame) + seq→height + height→offset + txid→offset
  (16B/tx) built by scanning blk0001.dat in a background thread (~90s cold, cache-first
  on restart: /var/lib/tri7/index/{blockidx,txidx}.bin ≈ 143MB, loads <1s). 2,210,692
  frames / 4,500,210 txs. Heights BELOW window base = frame-order chaining (proven by
  anchor rule: window-region accepts REQUIRE the daemon-anchored hashes — reached-base
  check proves the whole prefix); heights IN window = authoritative tipstate hashes;
  heights_ok=true verified. Key = first-8-bytes truncation, ALWAYS verified by full
  hash re-read (hash9/sha256d).
- Wallet RPCs serve the replay SNAPSHOTS (balances.json + wallet_utxos.json copied to
  /var/lib/tri7 root:tri7 0640) — NO live UTXO set (footprint: RSS 165MB steady,
  ~300MB peak first build; MemoryMax bumped 256M→512M).
- blk file access for user tri7: HARDLINK /root/.triangles/blk0001.dat →
  /var/lib/tri7/blk0001.dat (same inode 267185, file mode 0605 → world-readable inode;
  /root itself is 0700 so traversal is impossible — the hardlink is the only clean path).
  ln -f keeps it in sync with daemon appends (same inode).
- **CHAIN UNIT FACT (empirical, daemon-verified): COIN = 1e6, NOT 1e8.** Daemon doubles
  (getbalance/getreceivedbyaddress/getrawtransaction vout/getinfo moneysupply) = raw
  units / 1e6. consensus.rs COIN=1_000_000 confirmed. rpc.rs sats_to_tri divides by 1e6.
  getbalance = 3036.990591 (= balances.json wallet total 3,036,990,591 / 1e6). Daemon
  getbalance (2921.8) is ITS spendable view — the task pins balances.json total.
- Daemon-parity details: getdifficulty/getinfo difficulty = {proof-of-work,
  proof-of-stake} object; PoS = GetDifficulty(last block nBits) via the daemon's own
  double formula (0x0000ffff/(bits&0xffffff) × 256^(29-shift)) — BIT-EXACT vs daemon
  (8.147741845052824e-08 @tip); PoW = window-based next-target (daemon walks its full
  index for PoW blocks — impossible from the window, note field explains). Verbose tx
  "time" = containing BLOCK time (daemon semantics), tx nTime rides as "tx_time".
  getreceivedbyaddress is WALLET-SCOPED (non-wallet addresses → 0, daemon parity:
  23/23 exact).
- PORT REALITY: 19113 on DNS2 is held by an autossh tunnel (→ sami-pc:19112, live node
  consumer attached — DO NOT KILL). tri7d listens on **19114** until cutover. Full
  steps in deploy/tri7d_rpc_cutover.md.
- Live validation: gettip==daemon tip (at_daemon_tip true); getblockhash 23/23 heights
  match daemon (incl. seam region 2201698/2201750, fork boundaries 17651/17692/2186941/
  2186942); getblock hash+height native byte-identical JSON vs daemon (modifier/
  checksum/proofhash from window entries); getrawtransaction hex/time/meta/asm/values
  full parity; getreceivedbyaddress 23/23 + non-wallet 0; getbalance 3036.990591;
  listunspent 177 UTXOs / 3036990591; listwalletaddresses 248; batch works; oversize
  → HTTP 413; sendtoaddress → -32601 never forwarded. tests 136/136 (91 lib: 57 old
  + 34 new rpc/blockindex).
- Files: src/rpc.rs, src/blockindex.rs (new), src/lib.rs, src/legacy.rs (Reader::pos/
  skip), src/bin/follow_tip.rs (Arc<Mutex> refactor + adapters + listener), Cargo.toml
  (serde_json float_roundtrip feature — amount doubles must round-trip bit-exactly),
  deploy/tri7d.service (new ExecStart flags + MemoryMax 512M),
  deploy/install_tri7d.sh (step 5b: snapshots + hardlink), deploy/tri7d_rpc_cutover.md.

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
