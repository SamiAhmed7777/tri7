# Triangles PoS Consensus Validation Spec (v5 live code)

Definitive spec for exact Rust reimplementation in tri7, derived from the live C++
source at `/root/triangles_v5/src`. Every rule cites `file:line`. All line numbers
verified against the tree as of 2026-11-01.

## 0. Fork height constants (all values)

| Constant | Value | Definition | Meaning |
|---|---|---|---|
| `CUTOFF_POW_BLOCK` | 9000 | main.h:34 | No PoW blocks allowed above this height |
| `CRAPCHAIN_CUTOFF_BLOCK` | 17691 | main.h:35 | Pre-Pharao (v4) chain ends here; below it, bad-difficulty blocks are auto-accepted |
| `FORK_HEIGHT_V5` | 17651 | main.h:36 | v5 hard fork; at this height PoS difficulty resets to minimum |
| `FORK_HEIGHT_V5_4` | 2186941 | main.h:37 | v5.4: tight (90 s) timestamp drift |
| `MODIFIER_INTERVAL_SWITCH` | 1 | main.h:63 | PoS blocks rejected below this height (i.e. only genesis PoW block) |
| `MODIFIER_INTERVAL_RATIO` | 3 | kernel.h:14 | Modifier selection-interval section spacing ratio |
| `MAX_BLOCK_SIZE` | 1000000 | main.h:42 | |
| `MAX_BLOCK_SIGOPS` | MAX_BLOCK_SIZE/50 = 20000 | main.h:47 | |
| `MAX_MONEY` | 2222222 * COIN | main.h:57 | |
| `MAX_TRI_PROOF_OF_STAKE` | 0.33 * COIN | main.h:62 | Max PoS reward per coin-year |
| `MAX_REORG_DEPTH` | 100 | main.h:53 | Historical; no longer enforced (main.h:50-52 comment) |
| `nTargetSpacing` | 120 s | main.cpp:59 | Testnet: 60 s (main.cpp:3930) |
| `nStakeMinAge` | 3600 s (1 h) | main.cpp:60 | Testnet: 600 s (main.cpp:3926, 4221) — NOT height-gated |
| `nStakeMaxAge` | 43200 s (12 h) | main.cpp:61 | Testnet: 1800 s (main.cpp:3927) |
| `nModifierInterval` | 300 s | main.cpp:62 | Testnet: 60 s (main.cpp:3928, 4223) |
| `nTargetTimespan` | 1800 s | main.cpp:1561 | |
| genesis hash | `7e7a6e4d…b96021` (official == testnet) | main.h:72-73 | |
| modifier checkpoint | `{0: 0x000000000e00670b}` | kernel.cpp:20-21 | Only genesis; testnet skips (kernel.cpp:478-481) |
| `COINBASE_HEIGHT_ENFORCEMENT_HEIGHT` | 2300000 | main.cpp:3498 (static local) | Coinbase BIP30-style height prefix enforced only from here |

Note: there is **no chainparams.h** in this tree; these constants live in main.h/main.cpp.

## 1. PoW vs PoS block classification

- `CTransaction::IsCoinBase()`: `vin.size()==1 && vin[0].prevout.IsNull() && vout.size()>=1` — main.h:619-622.
- `CTransaction::IsCoinStake()`: `vin.size()>0 && !vin[0].prevout.IsNull() && vout.size()>=2 && vout[0].IsEmpty()` — main.h:624-627. `CTxOut::IsEmpty()` is `(nValue == 0) && (scriptPubKey.empty())` (main.h:428) — **value ZERO**, not -1 (`-1` is `CTxOut::IsNull()`, a deserializer init sentinel, main.h:419). Verified identically in the 2014 origin code (docs/reference/triangles-2014/main.h:378).
- `CBlock::IsProofOfStake()`: `vtx.size() > 1 && vtx[1].IsCoinStake()` — main.h:1025-1028.
- `CBlock::IsProofOfWork()`: `!(nFlags & BLOCK_PROOF_OF_STAKE)` on the index (main.h:1372-1374); on the block it is `!IsProofOfStake()` (index flags set from block on insert, main.cpp:3187-3215).
- Index flags (main.h:1232-1235): `BLOCK_PROOF_OF_STAKE=1<<0`, `BLOCK_STAKE_ENTROPY=1<<1`, `BLOCK_STAKE_MODIFIER=1<<2`.

**Rust rule:** a block is PoS iff tx count >= 2 and vtx[1] has ≥1 non-null prevout input, ≥2 outputs, and vout[0] is `value == 0` with an empty script (the IsEmpty marker). Otherwise PoW.

## 2. Block hash / Hash9

- `CBlock::GetHash()` = `Hash9(BEGIN(nVersion), END(nNonce))` — main.h:995-1000. Hash input is the 80-byte header only (vtx and vchBlockSig excluded from hashing; serialization gate at main.h:960).
- Hash9 = 13-way chained 512-bit: blake, bmw, groestl, jh, keccak, skein, luffa, cubehash, shavite, simd, echo, hamsi, fugue (hashblock.h:69+). Already bit-exact in tri7.
- Kernel hash (`Hash(ss.begin(), ss.end())`) is **double-SHA256** over the `SER_GETHASH` stream (kernel.cpp:110, 377) — different function from Hash9.

## 3. GetProofOfStake / CheckProofOfStake kernel

### 3.1 CheckProofOfStake (kernel.cpp:391-421)
1. Tx must satisfy `IsCoinStake()` (kernel.cpp:394-396).
2. `txin = tx.vin[0]`; read `txPrev` from chain DB at `txin.prevout`; failure ⇒ DoS(1) soft error (kernel.cpp:401-406).
3. `VerifySignature(txPrev, tx, 0, 0)` must pass; failure ⇒ DoS(100) (kernel.cpp:408-410).
4. Read the header of the block containing txPrev (header-only: `ReadFromDisk(nFile, nBlockPos, false)`) (kernel.cpp:412-415).
5. `CheckStakeKernelHash(nBits, block, nTxPrevOffset, txPrev, prevout, tx.nTime, …)` where `nTxPrevOffset = txindex.pos.nTxPos - txindex.pos.nBlockPos` (kernel.cpp:417-419).

### 3.2 CheckStakeKernelHash (kernel.cpp:311-389)
Guards:
- `nTimeTx < txPrev.nTime` ⇒ fail (kernel.cpp:314-315).
- `nTimeBlockFrom + nStakeMinAge > nTimeTx` ⇒ fail (min-age) (kernel.cpp:317-318).

Weight and target (kernel.cpp:322-327):
```
bnTargetPerCoinDay = compact(nBits)
nValueIn = txPrev.vout[prevout.n].nValue
nWeight = GetWeight(txPrev.nTime, nTimeTx)            // kernel.cpp:25-41
bnCoinDayWeight = nValueIn * nWeight / COIN / 86400
targetProofOfStake = bnCoinDayWeight * bnTargetPerCoinDay
```
- `GetWeight(begin, end)` = `min(end - begin - nStakeMinAge, nStakeMaxAge)`, 0 if negative (kernel.cpp:25-41). The old 7-day soft cap is REVERTED; do not implement it (comment kernel.cpp:31-40).

Kernel serialization (SER_GETHASH stream, then double-SHA256), kernel.cpp:373-378 — write in this exact order, all little-endian, with the SER_GETHASH no-double-read semantics (each primitive serialized once):
```
uint64  nStakeModifier          // 8 LE bytes
uint32  nTimeBlockFrom          // blockFrom.GetBlockTime(), 4 LE
uint32  nTxPrevOffset           // 4 LE
uint32  txPrev.nTime            // 4 LE
uint32  prevout.n               // 4 LE
uint32  nTimeTx                 // 4 LE
```
`hashProofOfStake = SHA256(SHA256(stream))`.

Acceptance: `CBigNum(hashProofOfStake) > bnCoinDayWeight * bnTargetPerCoinDay` normally fails — **except** while `pindexBest->nHeight <= CRAPCHAIN_CUTOFF_BLOCK (17691)`, where the hash is accepted regardless (Pharao-release hack, kernel.cpp:381-403). Rust: enforce target check only for height > 17691.

**CRITICAL Rust caveat:** the kernel uses the stake modifier of a block *ahead* of the stake block (3.4), which during historical replay depends on the fork's future. tri7 must either (a) trust the recorded `hashProofOfStake` for historical blocks (as the C++ fast path does, §9) or (b) replay with the full index in memory, matching the C++ `pnext` walk.

### 3.3 GetKernelStakeModifier (kernel.cpp:236-277)
- Selection interval is **fixed** to `2 * nModifierInterval` (600 s mainnet) — kernel.cpp:249-250 overrides the computed interval.
- Start at the index of `hashBlockFrom`; walk `pnext` until `nStakeModifierTime >= blockFrom.nTime + 2*nModifierInterval`, tracking the most recent block with `GeneratedStakeModifier()` (nFlags & BLOCK_STAKE_MODIFIER); return that block's `nStakeModifier` (kernel.cpp:252-276).
- If best chain is reached too early ⇒ error (kernel.cpp:256-268).

### 3.4 Stake modifier storage
- Per-index `uint64_t nStakeModifier` and `nStakeModifierChecksum` — main.h:1238-1239; serialized in the disk index (main.h:1472).
- Computed for every new index entry in `AddToBlockIndex` (main.cpp:3197-3215): full `ComputeNextStakeModifier` when `nHeight >= checkpointHeight - 1000`; otherwise modifier 0 with `GeneratedStakeModifier` only at genesis. Checksum checkpoints verified at main.cpp:3206-3207, 3215.
- Second computation site (import/snapshot path): main.cpp:4462-4482.

### 3.5 ComputeNextStakeModifier (kernel.cpp:147-233)
- `pindexPrev == nullptr` (genesis) ⇒ modifier 0, generated (kernel.cpp:152-157).
- Reuse previous modifier if `lastModifierTime / nModifierInterval == pindexPrev->nTime / nModifierInterval` (same interval bucket) (kernel.cpp:159-165).
- Otherwise: gather ancestors with `nTime >= (pindexPrev->nTime / nModifierInterval) * nModifierInterval - nSelectionInterval`; sort by (time, hash); run 64 selection rounds (kernel.cpp:171-200):
  - Section stop advances by `GetStakeModifierSelectionIntervalSection(n)` = `nModifierInterval * 63 / (63 + (63-n)*(MODIFIER_INTERVAL_RATIO-1))` (kernel.cpp:73-78).
  - Selection hash per candidate: `Hash(SER_GETHASH( proofHash || nStakeModifierPrev ))` double-SHA256, where `proofHash = hashProofOfStake` for PoS blocks else block hash (kernel.cpp:88-111); PoS candidates shift right 32 bits (kernel.cpp:113-116); lowest wins; each block selectable once per modifier.
  - `nStakeModifierNew |= entropyBit << nRound` (kernel.cpp:195).
- Entropy bit of a block = **bit 0 of the block's Hash9** (`GetHash().Get64() & 1`) — main.h:1017-1022; stored in index flags (main.h:1391-1399, main.cpp:3187).

### 3.6 Modifier checksum (kernel.cpp:461-473)
```
stream = [prev checksum (if pprev)] || nFlags || hashProofOfStake || nStakeModifier
checksum = top 32 bits of double-SHA256 (hash >> (256-32)), Get64() low 32
```
Hard checkpoint only at height 0 (kernel.cpp:20-21), skipped on testnet (kernel.cpp:479-480).

## 4. Difficulty

### 4.1 GetNextTargetRequired (main.cpp:1595-1662)
- Height gate: if `pindexLast->nHeight + 1 == FORK_HEIGHT_V5 (17651)` and `fProofOfStake`, return `bnProofOfStakeLimit.GetCompact()` (difficulty reset, main.cpp:1655-1661).
- Otherwise `GetNextTargetRequired_` (main.cpp:1609-1652):
  1. `pindexLast == nullptr` ⇒ target limit (genesis).
  2. `pindexPrev = GetLastBlockIndex(pindexLast, fProofOfStake)` — walk back to the nearest block of the same type (main.cpp:1586-1593). Empty ⇒ limit.
  3. If `pindexPrev->pprev == nullptr` ⇒ limit; same for `pindexPrevPrev` (main.cpp:1615-1622).
  4. `nActualSpacing = pindexPrev.nTime - pindexPrevPrev.nTime`; if negative use `nTargetSpacing` (main.cpp:1624-1632).
  5. Retarget on `pindexPrev->nBits` every block (main.cpp:1636-1640), with `nInterval = nTargetTimespan / nTargetSpacing = 15`:
     ```
     bnNew *= ((15-1)*120 + 2*nActualSpacing)
     bnNew /= ((15+1)*120)
     ```
  6. Clamp: `bnNew <= 0 || bnNew > bnTargetLimit ⇒ bnTargetLimit`; return compact (main.cpp:1648-1651).

### 4.2 ComputeMinStake (main.cpp:1595-1597)
- `ComputeMinStake(nBase, nTime, nBlockTime) == ComputeMaxBits(bnProofOfStakeLimit, nBase, nTime)` (main.cpp:1573-1586; nBlockTime unused): starting from compact(nBase), double the target once per full 24 h of `nTime`, capping at `bnProofOfStakeLimit`; return compact.
- Use: `ProcessBlock` anti-spam pre-check for non-tip blocks — required min from last same-type block at the latest checkpoint, elapsed `deltaTime = block.nTime - checkpoint.nTime`; reject when `compact(nBits) > required` (i.e. easier than minimum) — main.cpp:3624-3650. Misbehaving(5); `-noantispam` disables (main.cpp:3653+).

### 4.3 Difficulty enforcement in AcceptBlock (main.cpp:3414-3430)
- `nBits != GetNextTargetRequired(pindexPrev, IsProofOfStake())` ⇒ reject with DoS(100) **if `nHeight > CRAPCHAIN_CUTOFF_BLOCK (17691)`**; below that, accept any nBits (pre-Pharao, main.cpp:3418-3428).
- Height gates: PoW forbidden above `CUTOFF_POW_BLOCK (9000)` (main.cpp:3409-3410); PoS forbidden below `MODIFIER_INTERVAL_SWITCH (=1)` (main.cpp:3412-3413).
- `CheckProofOfWork`: target in range and `Hash9(header) <= target` (main.cpp:1664-1677); genesis hashes exempt (main.cpp:3302-3308).

## 5. Coinstake rules

- Structural (from IsCoinStake, §1): ≥2 vouts, vout[0] `value==0` + empty script marker, ≥1 real input.
- `CheckTransaction` (main.cpp:950-1006): no empty vout for user txs (965-966); values non-negative, ≤ MAX_MONEY, summed in MoneyRange (967-974); duplicate prevouts reject (981-986); coinbase scriptSig 2..100 bytes (991-993); non-coinbase null prevout reject (995-999).
- Empty coinstake outputs other than vout[0] are allowed to be created but do not create UTXOs (ConnectBlock skips `IsEmpty()` outputs, main.cpp:2389-2400, 2440-2451); `CTxOut::IsEmpty()` also legal inside coinstake in CheckTransaction via the `|| IsCoinStake()` exemption (main.cpp:965).
- Value split / reward (ConnectBlock, main.cpp:2470-2498):
  - `nStakeReward = GetValueOut() - GetValueIn()` of vtx[1]; `nFees` excludes coinstake (2421-2427, 2438).
  - `GetProofOfStakeReward(nCoinAge, nFees) = nCoinAge * MAX_TRI_PROOF_OF_STAKE / 365 / COIN + nFees` (main.cpp:1539-1560, bignum ops; integer division order matters).
  - Block coin age = sum of tx coin ages, floored at 1 coin-day (main.cpp:3155-3170). Per-input coin age skips inputs whose containing block violates `block.nTime + nStakeMinAge > tx.nTime` (main.cpp:3125-3126) and rejects `nTime < utxo.nTxTime` (main.cpp:3113-3114).
  - Reject when `nStakeReward > nCalculatedStakeReward` (DoS 100) for every fully validated block (main.cpp:2490-2495).
- **nStakeMinAge by height:** there is **no height gating** of `nStakeMinAge`; it is a global: 3600 s mainnet, 600 s testnet (main.cpp:60, 3926, 4221). The only time-gated weight change (STAKE_AGE_SOFT_CAP) was reverted and never validated any block (kernel.cpp:28-40). Rust: constant 3600.

## 6. Block signature validation

- `IsBlockSignatureRequiredAtHeight(nHeight)` = `nHeight > Checkpoints::GetTotalBlocksEstimate()` (main.cpp:1812-1815), i.e. above the last hardcoded checkpoint (checkpoint map at checkpoints.cpp:22+; last mainnet entry height 9001 → checkpoints.cpp:31). `IsConsensusAssumeValidHeight` additionally allows the rolling assume-valid threshold (main.cpp:1806-1810).
- `CBlock::CheckBlockSignature()` (main.cpp:3820-3847):
  - PoW block ⇒ signature must be **empty**.
  - PoS block ⇒ take `vtx[1].vout[1].scriptPubKey`, `Solver()` it; **only TxnOutType::PubKey (bare 65/33-byte pubkey script) passes**; else fail. Load pubkey into CKey, require non-empty `vchBlockSig`, and `key.Verify(GetHash(), vchBlockSig)` — ECDSA/secp256k1 over the Hash9 **header hash** (which excludes vchBlockSig, §2).
- Enforcement sites:
  - `CheckBlock(fCheckPOW, fCheckMerkleRoot, fCheckSig)`: if PoS and fCheckSig ⇒ check signature (main.cpp:3351-3354).
  - `ProcessBlock` decides `checkBlockSignature = IsBlockSignatureRequiredAtHeight(candidateHeight)` before calling `CheckBlock(true,true,that)` (main.cpp:3606-3615).
  - `AcceptBlock` re-checks for every PoS block above the checkpoint, independent of IBD state (main.cpp:3451-3454).
- Duplicate stake: `setStakeSeen` / `setStakeSeenOrphan` reject a second block using the same `(prevoutStake, nStakeTime)` unless it has orphan children or `-ignoredupstake` (main.cpp:3596-3599, 3684-3688; inserted at main.cpp:3224-3225).

## 7. Block time rules

- Drift windows: `GetMaxTimeDrift(nHeight)` = 90 s if `nHeight >= FORK_HEIGHT_V5_4 (2186941)` else 600 s; `PastDrift/FutureDrift` = time ∓ drift (main.h:77-79). Height-less overloads hardcode post-V5.4 (90 s) — main.h:81-85; they exist for the wallet path, not consensus.
- `CheckBlock` (context-free, main.cpp:3314-3321): reject if `block.nTime > GetTime() + 15*60` (raw wall clock, deliberately not GetAdjustedTime).
- Coinbase timestamp: `block.nTime > vtx[0].nTime + 10*60` ⇒ reject (always 10-min tolerance here, main.cpp:3327-3332).
- Coinstake timestamp: `CheckCoinStakeTimestamp` requires `block.nTime == vtx[1].nTime` exactly (main.cpp:3341-3343; kernel.cpp:424-428).
- Tx timestamps: `block.nTime < tx.nTime` ⇒ reject (main.cpp:3362-3365).
- Median-time-past: `GetPastTimeLimit() = GetMedianTimePast()` over the last **11** index entries inclusive (main.h:1331-1345).
- `AcceptBlock` timestamp rule (main.cpp:3432-3434), height-aware drift:
  ```
  reject if  block.nTime <= pindexPrev->GetPastTimeLimit()
          || FutureDrift(block.nTime, nHeight) < pindexPrev->nTime
  ```
  (i.e. block.nTime must be strictly greater than MTP, and `nTime + drift(nHeight) >= prev.nTime`.)
- Kernel extra time rules: `nTimeTx >= txPrev.nTime` (kernel.cpp:314) and coinstake must satisfy min-age (§3.2).

## 8. Acceptance order (exact sequence)

### 8.1 ProcessBlock (main.cpp:3571+)
1. Duplicate hash in mapBlockIndex / orphans ⇒ reject (3583-3587).
2. Duplicate proof-of-stake `(prevoutStake, nStakeTime)` in setStakeSeen (with orphan exception) ⇒ reject (3596-3599).
3. Compute `checkBlockSignature` from candidate height (3606-3613).
4. `CheckBlock(fCheckPOW=true, fCheckMerkleRoot=true, fCheckSig=checkBlockSignature)` (3614).
5. Anti-spam ComputeMinStake/ComputeMinWork check for non-extending blocks (3619-3650).
6. Orphan handling, then `AcceptBlock()`; on success process orphans, then `AddToBlockIndex` happened inside AcceptBlock; `SetBestChain`/`ConnectBlock` for chain selection (trust comparison rules incl. equal-trust tiebreak: earlier nTime, then lower hash, 2-min rate limit — main.cpp:3238-3260).

### 8.2 CheckBlock (main.cpp:3290-3395) — order:
1. Size ≤ MAX_BLOCK_SIZE (implicit via earlier checks / CheckTransaction).
2. PoW check (genesis-exempt) (3302-3308).
3. `nTime > GetTime() + 15*60` reject (3314-3321).
4. First tx coinbase; no other coinbase (3323-3329).
5. Coinbase nTime ≥ block.nTime − 10 min (3327-3333).
6. If PoS: coinbase exactly 1 empty output (3338-3340); vtx[1] coinstake, no other coinstake (3343-3347); coinstake timestamp equality (3350-3352); block signature if fCheckSig (3354-3355).
7. Per-tx `CheckTransaction` + tx timestamp ≤ block time (3358-3366).
8. Unique txids (3369-3376).
9. SigOps ≤ 20000 (3378-3385).
10. Merkle root (3387-3390).

### 8.3 AcceptBlock (main.cpp:3397-3540) — order:
1. Not already in mapBlockIndex (3401-3403).
2. Prev present (3405-3408).
3. Height gates: PoW > 9000 reject (3409-3410); PoS < 1 reject (3412-3413).
4. nBits vs GetNextTargetRequired, gated by height > 17691 (3415-3430).
5. Timestamp vs MTP and FutureDrift (3432-3434).
6. All txs final (`IsFinal(nHeight, nTime)`) (3436-3439).
7. Hardened checkpoint (3441-3443).
8. If PoS: signature check when `IsBlockSignatureRequiredAtHeight` (3451-3454); then PoS kernel: if `IsConsensusAssumeValidHeight(nHeight)` skip kernel and record hashProofOfStake=0 (3457-3466), else full `CheckProofOfStake(vtx[1], nBits, …)` (3470-3472).
9. Coinbase height-in-scriptSig prefix enforced only at height ≥ 2300000 (3497-3506).
10. WriteToDisk → AddToBlockIndex (3508-3517). Inside AddToBlockIndex (main.cpp:3160+): entropy bit (3187), hashProofOfStake record (3191), stake modifier + checksum + checkpoint (3197-3215), setStakeSeen insert (3224-3225), chain-trust/selection (3238+).
11. ConnectBlock (UTXO/spend/reward validation) on new best (§5, main.cpp:2335+).

## 9. Historical fast paths (Rust must replicate or bound)

- `IsConsensusAssumeValidHeight` = height ≤ last hardcoded checkpoint (9001) OR ≤ `nAssumeValidThreshold` (rolling, tip minus ASSUME_VALID_BUFFER=100) (main.cpp:1806-1810, main.h:45-53). Under it: PoS kernel, script, and reward checks skipped; `hashProofOfStake` recorded as 0 (main.cpp:3457-3466, 2472-2474, 2489-2495).
- Stake modifier computation skipped below `checkpoint − 1000` (main.cpp:3197-3215); blocks there store modifier 0.
- Blocks at height ≤ `CRAPCHAIN_CUTOFF_BLOCK` (17691): kernel target overshoot tolerated (kernel.cpp:381-403), bad nBits tolerated (main.cpp:3417-3428).

## 10. tri7 implementation notes

- Kernel double-SHA256 and Hash9 are already validated; the remaining risk areas are (a) 6-field kernel stream field order/width, (b) modifier `pnext` walk with fixed 2×nModifierInterval window, (c) per-block EMA retarget with `GetLastBlockIndex` same-type walk plus the 17651 reset, (d) height-aware drift switching at 2186941, (e) replicate-not-recompute history: below checkpoint+buffer store modifier 0 / hashProofOfStake 0 exactly as C++ does, so index state matches byte-for-byte.
