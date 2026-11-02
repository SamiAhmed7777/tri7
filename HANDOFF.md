# tri7 modern-client loop — session handoff (2026-09-21)

## State
Repo: /root/tri7 (Rust, commits 9e7635b → b744626 → 8da8d50, all grade B via codex gate).
Live chain ref: /root/.triangles/blk0001.dat (DNS2 daemon v6.2.6.7 lineage, tip ~2204100+, RPC 19112 works).
Original sources: curiousbank/triangles_v5 (upstream), wurstgelee/triangles (Black Pharao line, fetched as refs/req/wurst/master in /root/triangles_v5), 2014 origin TrianglesProject/triangles (vendored docs/reference/triangles-2014/).

## Done (all validated against live chain)
1a: legacy wire deserializer + blkfile reader (LE magic/length!) + Hash9 via verbatim sphlib FFI. Genesis + 2,201,091 blocks hash/link/merkle-verified vs daemon RPC.
1b: consensus classification (IsEmpty = value 0 + empty script, NOT -1!), sig gates, SQLite wallet reader (248/248 keys decode+verify; 12 label-ghost addresses = ismine-false on daemon too, 0 lifetime receipts).
1c: difficulty — CBigNum compact port (U512 for overflow at v5 fork), EMA, era seams: 0x2000ffff limit through 616604, then 207fffff; fork resets at 17651 (207fffff) and 2186942 (2000ffff); repair-block boundaries 17692/17693/2186941. 2,183,396 blocks nBits-enforced ALL matched. 46/46 tests.

## Next (in order)
1d: stake modifier (ComputeNextStakeModifier 64-round entropy selection, kernel.cpp:147-233) + CheckProofOfStake kernel (6-field stream, kernel.cpp:373-378) + full PoS kernel validation across archive. NOTE: replay forward keeps every IndexEntry so the pnext walk works. CRAPCHAIN era (≤17691) tolerates overshoot; assume-valid skip below 9001 mirrors deployed client.
1e: UTXO set + ConnectBlock-lite (balances per wallet address = the proof Sami wants).
2: P2P legacy protocol (magic 0x05223570 framing is blkfile-only; wire protocol = old bitcoin style) → follow live tip from DNS2 :24112-ish.
3: wallet spend (secp signing, legacy SIGHASH).
4: staking mint. 5: tri7d daemon packaging + deploy DNS2 → sami-pc → DNS3 → laptop.

## Gotchas
- Kimi CLI: `kimi -p` works; --auto/--yolo cannot combine with -p.
- Wallet records: suffixes/values carry CompactSize prefixes everywhere (strip before use).
- Era boundaries in archive: stale branch starts at 2201091.
- Judge (codex) requires: bins in src/bin, coverage-contract exits, no hand-math assertions. Cycle D→D→C→C→B pattern per phase; expect ~2-3 iterations.
- The 12 ghost labels + 9003-era sigs are ROOT-CAUSED (vestigial labels ismine-false on daemon; CRAPCHAIN-era pre-strict-ECDSA). Do not re-open.
