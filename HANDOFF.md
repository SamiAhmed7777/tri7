# tri7 modern-client loop — session handoff (2026-09-21)

## State
Repo: /root/tri7 (Rust, commits 9e7635b → b744626 → 8da8d50, all grade B via codex gate).
Live chain ref: /root/.triangles/blk0001.dat (DNS2 daemon v6.2.6.7 lineage, tip ~2204100+, RPC 19112 works).
Original sources: curiousbank/triangles_v5 (upstream), wurstgelee/triangles (Black Pharao line, fetched as refs/req/wurst/master in /root/triangles_v5), 2014 origin TrianglesProject/triangles (vendored docs/reference/triangles-2014/).

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
