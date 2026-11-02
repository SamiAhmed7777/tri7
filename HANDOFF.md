# tri7 modern-client loop — session handoff (2026-11-02, phase 3)

## State
Repo: /root/tri7 (Rust, phase 1a..2 committed; phase 3 UNCOMMITTED — do not commit without the codex gate).
Live chain ref: /root/.triangles (DNS2 daemon v6.2.6.7, RPC 19112, P2P 24113, tip 2204064 static since 2026-09-24, staking=0).

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
2: P2P legacy protocol (wire framing: magic + command12 + len + checksum dsha256-first4; handshake version/verack; getheaders/getdata; connect to DNS2 P2P port — onion 43kgtx6…:24113, LAN 24112?) → follow live tip.
3: wallet spend (secp signing, legacy SIGHASH).
4: staking mint. 5: tri7d daemon packaging + deploy DNS2 → sami-pc → DNS3 → laptop.

## Gotchas
- Kimi CLI: `kimi -p` works; --auto/--yolo cannot combine with -p.
- Wallet records: suffixes/values carry CompactSize prefixes everywhere (strip before use).
- Era boundaries in archive: stale branch starts at 2201091.
- Judge (codex) requires: bins in src/bin, coverage-contract exits, no hand-math assertions. Cycle D→D→C→C→B pattern per phase; expect ~2-3 iterations.
- The 12 ghost labels + 9003-era sigs are ROOT-CAUSED (vestigial labels ismine-false on daemon; CRAPCHAIN-era pre-strict-ECDSA). Do not re-open.
