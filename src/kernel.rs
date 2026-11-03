//! Stake modifier + PoS kernel validation — exact port of the live C++
//! kernel.cpp, era-parameterized to the client that minted/indexed each span
//! of the chain (see docs/pos-consensus-spec.md and the validate_pos header
//! for the era table and git citations).
//!
//! Key era facts (all verified against the deployed daemon's heritage index):
//!   - nModifierInterval: 1200 s before h17692 (kernel.h MODIFIER_INTERVAL =
//!     20*60 in the Initial-upload/Cheops clients), 300 s from h17692
//!     (Pharao v4.0.0.1) onward. No compute gate: every mint-era client
//!     computed the modifier for every block.
//!   - hashProofOfStake stored as zero for all PoS candidates below the live
//!     era (acceptance ran with kernels skipped); real hashes from the first
//!     checked height onward.
//!   - GetKernelStakeModifier: fixed 600 s window (kernel.cpp:249-250).
//! Every function mirrors the C++ semantics 1:1; "improvements" are forbidden.
//! The reconstruction reproduces the deployed client's own index state — the
//! 2026 FastImport recomputation the daemon carries — and was verified
//! block-for-block against that index (RPC getblock modifier/flags) across the
//! pre-seam span, the seam and deep post-seam heights (see validate_pos).

use crate::consensus::{
    stake_kernel_hash, stake_weight_era, COIN, MODIFIER_INTERVAL, STAKE_MIN_AGE,
};
use crate::legacy::{sha256d, Block, Hash};
use crate::replay::ReplayIndex;
use primitive_types::{U256, U512};

/// kernel.h:14 MODIFIER_INTERVAL_RATIO
const MODIFIER_INTERVAL_RATIO: i64 = 3;
/// kernel.cpp:249-250 — triangles OVERRIDES the computed selection interval
/// with a fixed `2 * nModifierInterval` (600 s mainnet) in GetKernelStakeModifier.
pub const KERNEL_SELECTION_INTERVAL: i64 = 2 * MODIFIER_INTERVAL as i64;

// ---------------------------------------------------------------------------
// Modifier selection-interval math (kernel.cpp:73-78)
// ---------------------------------------------------------------------------
/// GetStakeModifierSelectionIntervalSection(nSection) for a given interval.
pub fn selection_interval_section_with(n_section: usize, interval: u32) -> i64 {
    assert!(n_section < 64);
    interval as i64 * 63 / (63 + ((63 - n_section as i64) * (MODIFIER_INTERVAL_RATIO - 1)))
}

/// GetStakeModifierSelectionIntervalSection(nSection) at 300 s (current).
pub fn selection_interval_section(n_section: usize) -> i64 {
    selection_interval_section_with(n_section, MODIFIER_INTERVAL)
}

/// GetStakeModifierSelectionInterval() — sum of the 64 sections.
pub fn selection_interval_with(interval: u32) -> i64 {
    (0..64)
        .map(|n| selection_interval_section_with(n, interval))
        .sum()
}

/// GetStakeModifierSelectionInterval() at 300 s (current).
pub fn selection_interval() -> i64 {
    selection_interval_with(MODIFIER_INTERVAL)
}

/// uint256 numeric comparison of a serialized Hash (bytes are LE order in the
/// wire format; C++ uint256 compares as a big-endian 256-bit integer).
fn hash_cmp_numeric(a: &Hash, b: &Hash) -> std::cmp::Ordering {
    for i in (0..32).rev() {
        match a[i].cmp(&b[i]) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Hash9 entropy bit (main.h:1017-1022): bit 0 of the block's uint256 — the
/// low bit of the FIRST serialized byte (Get64() reads the internal LE words).
pub fn stake_entropy_bit(hash: &Hash) -> u8 {
    hash[0] & 1
}

/// kernel.cpp:20-21 — mapStakeModifierCheckpoints {0: 0x000000000e00670b}.
/// The only defined hard modifier checkpoint, enforced by AddToBlockIndex at
/// genesis (main.cpp:3211-3213 else-branch).
pub const GENESIS_MODIFIER_CHECKSUM: u32 = 0x0e00_670b;

/// CBlockIndex nFlags (main.h:1232-1235) assembled exactly as AddToBlockIndex
/// builds it: BLOCK_PROOF_OF_STAKE from the block type, SetStakeEntropyBit
/// (main.cpp:3187) writes bit 1, SetStakeModifier (main.h:1405-1410) sets
/// BLOCK_STAKE_MODIFIER when generated.
pub fn index_flags(is_proof_of_stake: bool, entropy_bit: u8, generated: bool) -> u32 {
    (is_proof_of_stake as u32) | ((entropy_bit as u32) << 1) | ((generated as u32) << 2)
}

/// GetStakeModifierChecksum (kernel.cpp:431-444): double-SHA256 over
/// SER_GETHASH [prev checksum || nFlags || hashProofOfStake || nStakeModifier]
/// (previous checksum omitted only when pprev is null, i.e. genesis). The
/// result is `hash >>= (256 - 32)` then Get64(): the top 32 numeric bits land
/// in the uint256's LOW word, so the returned u32 reads hash[28..32]
/// little-endian (verified against the deployed genesis checkpoint constant).
pub fn stake_modifier_checksum(
    prev_checksum: Option<u32>,
    n_flags: u32,
    hash_proof_of_stake: &Hash,
    stake_modifier: u64,
) -> u32 {
    let mut s = Vec::with_capacity(48);
    if let Some(prev) = prev_checksum {
        s.extend_from_slice(&prev.to_le_bytes());
    }
    s.extend_from_slice(&n_flags.to_le_bytes());
    s.extend_from_slice(hash_proof_of_stake);
    s.extend_from_slice(&stake_modifier.to_le_bytes());
    let h = sha256d(&s);
    u32::from_le_bytes([h[28], h[29], h[30], h[31]])
}

// ---------------------------------------------------------------------------
// ComputeNextStakeModifier (kernel.cpp:147-233)
// ---------------------------------------------------------------------------
/// Selection hash per candidate (kernel.cpp:88-116): double-SHA256 over
/// SER_GETHASH( proofHash || nStakeModifierPrev ), PoS candidates shifted
/// right 32 bits so PoS is favored over PoW.
fn selection_hash(proof_hash: &Hash, prev_modifier: u64, is_pos: bool) -> U256 {
    let mut s = Vec::with_capacity(40);
    s.extend_from_slice(proof_hash);
    s.extend_from_slice(&prev_modifier.to_le_bytes());
    let h = sha256d(&s);
    let mut key = U256::from_little_endian(&h);
    if is_pos {
        key >>= 32;
    }
    key
}

/// ComputeNextStakeModifier port. `prev_height` is the height of pindexPrev
/// (None = genesis insert). Returns (modifier, fGeneratedStakeModifier).
pub fn compute_next_stake_modifier(
    idx: &ReplayIndex,
    prev_height: Option<usize>,
) -> Result<(u64, bool), String> {
    compute_next_stake_modifier_era(idx, prev_height, MODIFIER_INTERVAL)
}

/// ComputeNextStakeModifier for the modifier interval in force at the block
/// being indexed: 1200 s for pre-Pharao blocks (< 17692), 300 s after.
pub fn compute_next_stake_modifier_era(
    idx: &ReplayIndex,
    prev_height: Option<usize>,
    interval: u32,
) -> Result<(u64, bool), String> {
    // kernel.cpp:152-157 — genesis: modifier 0, generated.
    let prev_h = match prev_height {
        None => return Ok((0, true)),
        Some(h) => h,
    };
    let prev = &idx.entries[prev_h];

    // GetLastStakeModifier (kernel.cpp:80-94): walk back to nearest block with
    // GeneratedStakeModifier; genesis must qualify.
    let mut hm = prev_h;
    while hm > 0 && !idx.entries[hm].generated_stake_modifier {
        hm -= 1;
    }
    if !idx.entries[hm].generated_stake_modifier {
        return Err("GetLastStakeModifier: no generation at genesis block".into());
    }
    let prev_modifier = idx.entries[hm].stake_modifier;
    let modifier_time = idx.entries[hm].time as i64;

    // kernel.cpp:159-165 — reuse the previous modifier within the same
    // nModifierInterval bucket.
    if modifier_time / interval as i64 >= prev.time as i64 / interval as i64 {
        return Ok((prev_modifier, false));
    }

    // Candidate collection (kernel.cpp:167-178): ancestors of pindexPrev with
    // time >= intervalStart, then sorted by (time, hash) ascending.
    let sel_interval = selection_interval_with(interval);
    let sel_start = (prev.time as i64 / interval as i64) * interval as i64 - sel_interval;
    let mut cands: Vec<(i64, Hash, usize)> = Vec::new();
    let mut h = prev_h;
    loop {
        let e = &idx.entries[h];
        if (e.time as i64) < sel_start {
            break;
        }
        cands.push((e.time as i64, e.hash, h));
        if h == 0 {
            break;
        }
        h -= 1;
    }
    // reverse + std::sort — pairs (time, hash) are unique so the sort is
    // deterministic.
    cands.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| hash_cmp_numeric(&a.1, &b.1)));

    // Precompute each candidate's selection key once: it depends only on
    // (proofHash, prevModifier), which are fixed across all 64 rounds.
    let keys: Vec<U256> = cands
        .iter()
        .map(|(_, _, height)| {
            let e = &idx.entries[*height];
            let proof = if e.is_proof_of_stake {
                e.hash_proof_of_stake
            } else {
                e.hash
            };
            selection_hash(&proof, prev_modifier, e.is_proof_of_stake)
        })
        .collect();

    // 64 selection rounds (kernel.cpp:182-201).
    let mut new_modifier: u64 = 0;
    let mut selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut stop = sel_start;
    let rounds = std::cmp::min(64, cands.len());
    for round in 0..rounds {
        stop += selection_interval_section_with(round, interval);
        // SelectBlockFromCandidates (kernel.cpp:111-144): scan candidates in
        // (time, hash) order; break at time > stop ONLY once something is
        // selected; skip already-selected; lowest selection key wins.
        let mut best: Option<(U256, usize)> = None;
        for (i, (t, _, _)) in cands.iter().enumerate() {
            // kernel.cpp:102-103 — the section-stop bound only applies once
            // something is selected (fSelected in the C++): the first
            // not-yet-selected candidate in (time, hash) order is always
            // eligible even beyond the stop. Critical on sparse chain windows
            // (gaps between blocks) where every candidate lies beyond the
            // round's stop — the C++ still selects one.
            if best.is_some() && *t > stop {
                break;
            }
            if selected.contains(&i) {
                continue;
            }
            best = match best {
                None => Some((keys[i], i)),
                Some((bk, bi)) => {
                    if keys[i] < bk {
                        Some((keys[i], i))
                    } else {
                        Some((bk, bi))
                    }
                }
            };
        }
        let (_, bi) = best.ok_or_else(|| {
            format!(
                "ComputeNextStakeModifier: unable to select block at round {}",
                round
            )
        })?;
        // Entropy bit of the selected block (kernel.cpp:195).
        let sel_height = cands[bi].2;
        new_modifier |= (idx.entries[sel_height].entropy_bit as u64) << round;
        selected.insert(bi);
    }
    Ok((new_modifier, true))
}

// ---------------------------------------------------------------------------
// GetKernelStakeModifier (kernel.cpp:236-277)
// ---------------------------------------------------------------------------
/// Fixed-window pnext walk: from the block containing the stake input, walk
/// forward until the most recent generated modifier time reaches
/// blockFrom.time + 2*nModifierInterval; return the modifier stored at the
/// block where the walk stopped (every block above the compute gate stores the
/// current modifier — generated or reused — so this reproduces
/// `pindex->nStakeModifier` exactly).
pub fn get_kernel_stake_modifier(
    idx: &ReplayIndex,
    block_from_height: usize,
) -> Result<u64, String> {
    get_kernel_stake_modifier_with(idx, block_from_height, KERNEL_SELECTION_INTERVAL)
}

/// Era-parameterized walk: `window` is the forward time distance — 600 s for
/// Pharao-era and later clients (the fixed
/// `nStakeModifierSelectionInterval = 2 * nModifierInterval` override), or the
/// full computed selection interval (42301 s at interval 1200) for the
/// pre-Pharao clients whose proofs are stored in the heritage index.
pub fn get_kernel_stake_modifier_with(
    idx: &ReplayIndex,
    block_from_height: usize,
    window: i64,
) -> Result<u64, String> {
    // Bounds-guard the entry index: callers stake from candidate heights that
    // must index `idx.entries` (window-relative in the follow-loop, absolute
    // in stake_mint). A foreign/out-of-range height must take the wallet.cpp
    // 2269-2271 skip-coin Err path, NOT panic the process (a panicked staking
    // loop killed the live daemon on 2026-11-02).
    let from = idx.entries.get(block_from_height).ok_or_else(|| {
        format!(
            "GetKernelStakeModifier: block height {} outside the index ({} entries) — coin skipped",
            block_from_height,
            idx.entries.len()
        )
    })?;
    let threshold = from.time as i64 + window;
    let mut p = block_from_height;
    let mut modifier_time = from.time as i64;
    loop {
        if modifier_time >= threshold {
            return Ok(idx.entries[p].stake_modifier);
        }
        if p + 1 >= idx.entries.len() {
            // kernel.cpp:256-268 — reached best chain end during replay.
            return Err(format!(
                "GetKernelStakeModifier: reached best block at height {} from block {}",
                p, block_from_height
            ));
        }
        p += 1;
        if idx.entries[p].generated_stake_modifier {
            modifier_time = idx.entries[p].time as i64;
        }
    }
}

// ---------------------------------------------------------------------------
// CheckStakeKernelHash / CheckProofOfStake (kernel.cpp:311-421)
// ---------------------------------------------------------------------------
/// Minimal UTXO record for kernel reconstruction: where the output lives and
/// what it is worth. `tx_offset` is the offset of the containing tx within the
/// block record — exactly `txindex.pos.nTxPos - txindex.pos.nBlockPos`
/// (kernel.cpp:419), i.e. 80-byte header + tx-count CompactSize + cumulative
/// tx sizes.
#[derive(Clone, Copy, Debug)]
pub struct UtxoMeta {
    pub height: usize,
    pub tx_offset: u32,
    pub tx_time: u32,
    pub value: i64,
}

/// UTXO set keyed by (txid, n), built during forward replay. Empty-marker
/// outputs (vout[0] of coinstakes, IsEmpty = value 0 + empty script) are NOT
/// tracked — ConnectBlock skips them (main.cpp:2389-2400, 2440-2451).
/// `addrs` mirrors `map` key-for-key (same insert/remove decisions, evaluated
/// in the same loops) and carries each live output's 21-byte address payload
/// (version byte + hash160) — phase 1e per-address balances.
#[derive(Default)]
pub struct UtxoSet {
    map: std::collections::HashMap<(Hash, u32), UtxoMeta>,
    addrs: std::collections::HashMap<(Hash, u32), [u8; 21]>,
    pub count: u64,
}

/// Track/untrack the address payload of one live output. A script without a
/// recognizable destination leaves NO entry (the output stays attributable
/// only to the unattributed bucket).
fn set_addr(
    addrs: &mut std::collections::HashMap<(Hash, u32), [u8; 21]>,
    key: &(Hash, u32),
    script: &[u8],
) {
    match crate::address::script_to_dest(script) {
        Some(dest) => {
            addrs.insert(*key, dest.payload());
        }
        None => {
            addrs.remove(key);
        }
    }
}

impl UtxoSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &(Hash, u32)) -> Option<&UtxoMeta> {
        self.map.get(key)
    }

    /// ConnectBlock-lite: apply one block. Tx order matters (a tx may spend
    /// outputs created earlier in the same block). Coinbase has a null input
    /// and is skipped. Returns Err on a missing input.
    pub fn apply_block(&mut self, block: &Block, height: usize) -> Result<(), String> {
        // Per-tx offset within the block record: 80 (header) + CompactSize
        // (tx count) + cumulative serialized tx sizes.
        let mut offset = 80 + compact_size_len(block.vtx.len() as u64) as u32;
        for tx in &block.vtx {
            if !(tx.vin.len() == 1
                && tx.vin[0].prev_hash == [0u8; 32]
                && tx.vin[0].prev_n == u32::MAX)
            {
                for vin in &tx.vin {
                    let key = (vin.prev_hash, vin.prev_n);
                    if self.map.remove(&key).is_none() {
                        // VerifySignature-lite precondition: a spend of an
                        // output we never tracked. Kernel reconstruction would
                        // be impossible; the caller reports it.
                        return Err(format!(
                            "untracked spend at height {} ({}, {})",
                            height,
                            crate::legacy::hash_to_display(&vin.prev_hash),
                            vin.prev_n
                        ));
                    }
                    self.addrs.remove(&key);
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                let is_empty = vout.value == 0 && vout.script_pubkey.is_empty();
                if !is_empty {
                    let key = (tx.hash(), n as u32);
                    self.map.insert(
                        key,
                        UtxoMeta {
                            height,
                            tx_offset: offset,
                            tx_time: tx.time,
                            value: vout.value,
                        },
                    );
                    set_addr(&mut self.addrs, &key, &vout.script_pubkey);
                }
            }
            offset += tx.serialize().len() as u32;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// count of spends ever applied (for reporting).
    pub fn tracked_total(&self) -> u64 {
        self.count
    }

    /// Address payload of one live output (None = unattributable script).
    pub fn address_payload_of(&self, key: &(Hash, u32)) -> Option<[u8; 21]> {
        self.addrs.get(key).copied()
    }

    /// Phase 1e: aggregate live-UTXO value per address payload, with the
    /// unattributable remainder kept separate (outputs whose script is none
    /// of the three payable forms). Call after the replay is complete.
    pub fn address_balances(&self) -> AddressBalances {
        let mut out = AddressBalances::default();
        for (key, meta) in &self.map {
            match self.addrs.get(key) {
                Some(payload) => {
                    let e = out.per_address.entry(*payload).or_insert((0i64, 0u64));
                    e.0 += meta.value;
                    e.1 += 1;
                }
                None => {
                    out.unattributed_sats += meta.value;
                    out.unattributed_utxos += 1;
                }
            }
        }
        out
    }

    /// apply_block that reports anomalies instead of hard-failing, for
    /// historical replay where the archive is ground truth:
    /// - a tx with ANY untracked input is left spend-wise untouched (nothing
    ///   removed — all-or-nothing per tx, like C++ ConnectBlock's atomic per-tx
    ///   spend handling) but its outputs are still created; the untracked
    ///   inputs are counted in the report;
    /// - (txid, n) inserts that overwrite an existing entry are counted as
    ///   duplicate-txid history (the newest meta wins, matching the live
    ///   client's txid-keyed txdb).
    pub fn apply_block_reported(&mut self, block: &Block, height: usize) -> BlockApplyReport {
        let mut rep = BlockApplyReport::default();
        let mut offset = 80 + compact_size_len(block.vtx.len() as u64) as u32;
        for tx in &block.vtx {
            let is_coinbase = tx.vin.len() == 1
                && tx.vin[0].prev_hash == [0u8; 32]
                && tx.vin[0].prev_n == u32::MAX;
            if !is_coinbase {
                let missing = tx
                    .vin
                    .iter()
                    .filter(|vin| !self.map.contains_key(&(vin.prev_hash, vin.prev_n)))
                    .count();
                if missing > 0 {
                    rep.untracked_spends += missing;
                } else {
                    for vin in &tx.vin {
                        let key = (vin.prev_hash, vin.prev_n);
                        self.map.remove(&key);
                        self.addrs.remove(&key);
                    }
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                let is_empty = vout.value == 0 && vout.script_pubkey.is_empty();
                if !is_empty {
                    let key = (tx.hash(), n as u32);
                    if self
                        .map
                        .insert(
                            key,
                            UtxoMeta {
                                height,
                                tx_offset: offset,
                                tx_time: tx.time,
                                value: vout.value,
                            },
                        )
                        .is_some()
                    {
                        rep.duplicate_txids += 1;
                    }
                    set_addr(&mut self.addrs, &key, &vout.script_pubkey);
                }
            }
            offset += tx.serialize().len() as u32;
        }
        rep
    }
}

/// Per-block anomaly report from apply_block_reported.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockApplyReport {
    pub untracked_spends: usize,
    pub duplicate_txids: usize,
}

/// Per-address live balances keyed by the 21-byte payload
/// (version byte + hash160), value in sats + live-UTXO count.
#[derive(Default)]
pub struct AddressBalances {
    pub per_address: std::collections::HashMap<[u8; 21], (i64, u64)>,
    pub unattributed_sats: i64,
    pub unattributed_utxos: u64,
}

/// Resolver trait so the kernel can read txPrev metadata from either the
/// spend-tracking UTXO set or the full historical output index.
pub trait KernelOutputResolver {
    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta>;
}

impl KernelOutputResolver for UtxoSet {
    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta> {
        self.map.get(key).copied()
    }
}

/// Full historical output index — every non-empty output ever created, keyed
/// (txid, n), never removed on spend. This mirrors the C++ kernel's own
/// resolution: txPrev.ReadFromDisk reads the transaction from the txid-keyed
/// txdb WITHOUT consulting spentness (which is exactly why the v5.8.x vSpent
/// tracking bug let re-spent coinstake inputs validate). Spend conflicts are
/// counted (a vin resolving to an output already spent once) so the vSpent
/// era stays visible in reports.
#[derive(Default)]
pub struct OutputIndex {
    map: std::collections::HashMap<(Hash, u32), UtxoMeta>,
    spent: std::collections::HashSet<(Hash, u32)>,
    pub outputs_total: u64,
    pub duplicate_txids: usize,
    pub respends: usize,
}

impl OutputIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Apply one block: insert every non-empty output, mark + count spends.
    /// Duplicate (txid, n) overwrites are counted (newest meta wins, matching
    /// the live client's txid-keyed txdb).
    pub fn apply_block(&mut self, block: &Block, height: usize) {
        let mut offset = 80 + compact_size_len(block.vtx.len() as u64) as u32;
        for tx in &block.vtx {
            let is_coinbase = tx.vin.len() == 1
                && tx.vin[0].prev_hash == [0u8; 32]
                && tx.vin[0].prev_n == u32::MAX;
            if !is_coinbase {
                for vin in &tx.vin {
                    // spent.insert returns true when the key was NEW (first
                    // spend); false means already spent once — vSpent-bug era
                    // re-spend, counted.
                    if !self.spent.insert((vin.prev_hash, vin.prev_n)) {
                        self.respends += 1;
                    }
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                let is_empty = vout.value == 0 && vout.script_pubkey.is_empty();
                if !is_empty {
                    self.outputs_total += 1;
                    if self
                        .map
                        .insert(
                            (tx.hash(), n as u32),
                            UtxoMeta {
                                height,
                                tx_offset: offset,
                                tx_time: tx.time,
                                value: vout.value,
                            },
                        )
                        .is_some()
                    {
                        self.duplicate_txids += 1;
                    }
                }
            }
            offset += tx.serialize().len() as u32;
        }
    }
}

impl KernelOutputResolver for OutputIndex {
    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta> {
        self.map.get(key).copied()
    }
}

fn compact_size_len(n: u64) -> usize {
    if n < 0xfd {
        1
    } else if n <= 0xffff {
        3
    } else if n <= 0xffff_ffff {
        5
    } else {
        9
    }
}

/// CheckProofOfStake result for one coinstake.
#[derive(Debug)]
pub struct KernelResult {
    pub hash_proof_of_stake: Hash,
    pub meets_target: bool,
    /// Modifier resolved by the fixed-window walk (diagnostics).
    pub modifier: u64,
    /// Height of the block containing the stake input (diagnostics).
    pub block_from_height: usize,
    /// Coin-day weight used in the target product (diagnostics).
    pub coin_day_weight: i128,
    /// coinDayWeight * bnTargetPerCoinDay (diagnostics).
    pub target: U512,
}

/// CheckProofOfStake (kernel.cpp:391-421) with VerifySignature-lite: the
/// coinstake input must resolve to a tracked output (full ECDSA script
/// verification is phase-1b/1e territory; resolution proves the input spends
/// a real chain output, which is what kernel reconstruction requires).
/// `R` is the output resolver (UtxoSet or OutputIndex — the C++ reads txPrev
/// from the txid-keyed txdb without consulting spentness).
/// `tip_height` is pindexBest->nHeight DURING VALIDATION (h-1 in sequential
/// acceptance); the block under validation sits at height tip_height + 1,
/// which selects the GetWeight era (see consensus::stake_weight_era).
pub fn check_proof_of_stake<R: KernelOutputResolver>(
    idx: &ReplayIndex,
    block: &Block,
    outputs: &R,
    n_bits: u32,
    tip_height: usize,
) -> Result<KernelResult, String> {
    let tx = &block.vtx[1];
    if !crate::consensus::tx_is_coin_stake(tx) {
        return Err("CheckProofOfStake: called on non-coinstake".into());
    }
    // kernel.cpp:401-406 + 408-410 (VerifySignature-lite): input must spend a
    // tracked output.
    let txin = &tx.vin[0];
    let meta = outputs
        .kernel_meta(&(txin.prev_hash, txin.prev_n))
        .ok_or_else(|| "CheckProofOfStake: coinstake input not a tracked UTXO".to_string())?;
    let tx_prev_block = &idx.entries[meta.height];

    // CheckStakeKernelHash guards (kernel.cpp:314-318).
    if tx.time < meta.tx_time {
        return Err("CheckStakeKernelHash: nTime violation".into());
    }
    if tx_prev_block.time as i64 + STAKE_MIN_AGE as i64 > tx.time as i64 {
        return Err("CheckStakeKernelHash: min age violation".into());
    }

    // Weight and target (kernel.cpp:322-327) — integer division order matters.
    let n_value_in = meta.value;
    let weight = stake_weight_era(meta.tx_time, tx.time, tip_height + 1);
    let coin_day_weight = (n_value_in as i128 * weight as i128) / COIN as i128 / 86_400i128;

    // Modifier from the fixed-600s walk (kernel.cpp:236-277 via 373).
    let modifier = get_kernel_stake_modifier(idx, meta.height)?;

    let hash = stake_kernel_hash(
        modifier,
        tx_prev_block.time,
        meta.tx_offset,
        meta.tx_time,
        txin.prev_n,
        tx.time,
    );

    // Target acceptance (kernel.cpp:381-403): hash must be <=
    // coinDayWeight * targetPerCoinDay, EXCEPT at/below CRAPCHAIN_CUTOFF
    // where overshoot is tolerated (Pharao-release hack). The caller applies
    // the era rule; we report the comparison.
    let (target_per_coin_day, _neg, _ovf) = crate::difficulty::set_compact(n_bits);
    let target = U512::from(coin_day_weight as u128) * U512::from(target_per_coin_day);
    let hash_num = U256::from_little_endian(&hash);
    let meets = U512::from(hash_num) <= target;
    Ok(KernelResult {
        hash_proof_of_stake: hash,
        meets_target: meets,
        modifier,
        block_from_height: meta.height,
        coin_day_weight,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::sha256d;
    use crate::replay::IndexEntry;

    fn synthetic_entry(height: usize, time: u32, pos: bool, generated: bool) -> IndexEntry {
        IndexEntry {
            height,
            hash: {
                let mut h = [0u8; 32];
                h[0..8].copy_from_slice(&(height as u64).to_le_bytes());
                h[31] = 0xa5 ^ height as u8;
                h
            },
            prev_hash: {
                let mut h = [0xffu8; 32];
                if height > 0 {
                    h[0..8].copy_from_slice(&((height - 1) as u64).to_le_bytes());
                }
                h
            },
            time,
            bits: 0x2070_0000,
            is_proof_of_stake: pos,
            entropy_bit: (height & 1) as u8,
            stake_modifier: 0,
            generated_stake_modifier: generated,
            hash_proof_of_stake: if pos { [0x77u8; 32] } else { [0u8; 32] },
        }
    }

    #[test]
    fn entropy_bit_is_low_bit_of_first_serialized_byte() {
        // main.h:1017-1022 Get64() & 1 — Get64 reads the internal LE words, so
        // bit 0 of the uint256 = bit 0 of serialized byte 0.
        let mut h = [0u8; 32];
        h[0] = 0b0000_0010;
        assert_eq!(stake_entropy_bit(&h), 0);
        h[0] = 0b0000_0011;
        assert_eq!(stake_entropy_bit(&h), 1);
        h[0] = 0;
        h[31] = 0xff; // high bytes must not influence the bit
        assert_eq!(stake_entropy_bit(&h), 0);
    }

    #[test]
    fn modifier_interval_section_math() {
        // kernel.cpp:73-78: 300*63 / (63 + (63-n)*(ratio-1)); ratio 3.
        assert_eq!(selection_interval_section(0), 18_900 / 189); // 100
        assert_eq!(selection_interval_section(63), 18_900 / 63); // 300
                                                                 // strictly increasing
        for n in 1..64 {
            assert!(selection_interval_section(n) > selection_interval_section(n - 1));
        }
        // total selection interval is the exact sum of the 64 sections
        let manual: i64 = (0..64).map(|n| 300 * 63 / (63 + (63 - n) * 2)).sum();
        assert_eq!(selection_interval(), manual);
        // kernel window (kernel.cpp:249-250) is FIXED at 2 * nModifierInterval
        assert_eq!(KERNEL_SELECTION_INTERVAL, 600);
    }

    #[test]
    fn kernel_hash_known_vector() {
        // self-computed double-SHA256 over the exact 6-field SER_GETHASH
        // stream (kernel.cpp:373-378)
        let mut s = Vec::new();
        s.extend_from_slice(&0x0e00_670bu64.to_le_bytes()); // nStakeModifier
        s.extend_from_slice(&1_400_000_000u32.to_le_bytes()); // nTimeBlockFrom
        s.extend_from_slice(&123_456u32.to_le_bytes()); // nTxPrevOffset
        s.extend_from_slice(&1_399_999_000u32.to_le_bytes()); // txPrev.nTime
        s.extend_from_slice(&3u32.to_le_bytes()); // prevout.n
        s.extend_from_slice(&1_400_004_000u32.to_le_bytes()); // nTimeTx
        let expected = sha256d(&s);
        assert_eq!(
            stake_kernel_hash(
                0x0e00_670b,
                1_400_000_000,
                123_456,
                1_399_999_000,
                3,
                1_400_004_000
            ),
            expected
        );
        // different fields must change the hash
        assert_ne!(
            stake_kernel_hash(
                0x0e00_670c,
                1_400_000_000,
                123_456,
                1_399_999_000,
                3,
                1_400_004_000
            ),
            expected
        );
    }

    #[test]
    fn utxo_spend_tracking() {
        use crate::legacy::{Tx, Vin, Vout};
        let mut set = UtxoSet::new();
        // funding tx: one spendable output + one empty marker output (skipped)
        let fund = Tx {
            version: 1,
            time: 1_400_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                }, // IsEmpty marker
                Vout {
                    value: 5_000_000,
                    script_pubkey: vec![0xac],
                },
            ],
            locktime: 0,
        };
        let blk = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_000_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![fund.clone(), fund.clone()],
            block_sig: vec![],
        };
        set.apply_block(&blk, 0).expect("apply");
        // marker output not tracked, real output tracked
        let txid = fund.hash();
        assert!(
            set.get(&(txid, 0)).is_none(),
            "IsEmpty output must not be tracked"
        );
        let meta = set.get(&(txid, 1)).expect("real output tracked");
        assert_eq!(meta.value, 5_000_000);
        assert_eq!(meta.height, 0);
        // offset = 80 + 1 (count) + size(tx0)
        assert_eq!(meta.tx_offset as usize, 80 + 1 + fund.serialize().len());
        // spend it
        let spend = Tx {
            version: 1,
            time: 1_400_004_000,
            vin: vec![Vin {
                prev_hash: txid,
                prev_n: 1,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                },
                Vout {
                    value: 5_000_000,
                    script_pubkey: vec![0xac],
                },
            ],
            locktime: 0,
        };
        let blk2 = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_004_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![spend.clone()],
            block_sig: vec![],
        };
        set.apply_block(&blk2, 1).expect("apply spend");
        assert!(set.get(&(txid, 1)).is_none(), "spent output removed");
        // double-spend detection
        let blk3 = Block {
            version: 1,
            prev_hash: [2u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_008_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![spend.clone()],
            block_sig: vec![],
        };
        assert!(
            set.apply_block(&blk3, 2).is_err(),
            "double spend must error"
        );
    }

    #[test]
    fn utxo_address_payloads_track_spends() {
        use crate::legacy::{Tx, Vin, Vout};
        let mut set = UtxoSet::new();
        // fund: P2PKH-looking output (76 a9 14 <h> 88 ac) + unattributable output
        let mut p2pkh = vec![0x76u8, 0xa9, 0x14];
        p2pkh.extend_from_slice(&[0x33u8; 20]);
        p2pkh.extend_from_slice(&[0x88, 0xac]);
        let expect_payload = tri7_payload(&p2pkh);
        let fund = Tx {
            version: 1,
            time: 1_400_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 700,
                    script_pubkey: p2pkh.clone(),
                },
                Vout {
                    value: 11,
                    script_pubkey: vec![0x6a, 0x01, 0xaa],
                }, // OP_RETURN junk
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                }, // IsEmpty marker
            ],
            locktime: 0,
        };
        let blk = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_000_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![fund.clone()],
            block_sig: vec![],
        };
        set.apply_block(&blk, 0).expect("apply");
        let txid = fund.hash();
        assert_eq!(set.address_payload_of(&(txid, 0)), Some(expect_payload));
        assert_eq!(
            set.address_payload_of(&(txid, 1)),
            None,
            "non-standard script unattributed"
        );
        assert_eq!(
            set.address_payload_of(&(txid, 2)),
            None,
            "IsEmpty marker untracked"
        );
        let totals = set.address_balances();
        assert_eq!(totals.per_address.get(&expect_payload), Some(&(700, 1)));
        assert_eq!(totals.unattributed_sats, 11);
        assert_eq!(totals.unattributed_utxos, 1);
        // spend the attributed output -> payload must disappear in lockstep
        let spend = Tx {
            version: 1,
            time: 1_400_004_000,
            vin: vec![Vin {
                prev_hash: txid,
                prev_n: 0,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 700,
                script_pubkey: p2pkh,
            }],
            locktime: 0,
        };
        let blk2 = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_004_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![spend],
            block_sig: vec![],
        };
        set.apply_block(&blk2, 1).expect("apply spend");
        assert!(set.map.get(&(txid, 0)).is_none());
        assert_eq!(set.address_payload_of(&(txid, 0)), None);
        // the spend itself created a live output with the same payload; the
        // funding tx's OP_RETURN output (11 sats) is still live unattributed
        let totals = set.address_balances();
        assert_eq!(totals.per_address.get(&expect_payload), Some(&(700, 1)));
        assert_eq!(totals.unattributed_sats, 11);
        assert_eq!(totals.unattributed_utxos, 1);
    }

    /// The payload a P2PKH script must map to, computed via the public API.
    fn tri7_payload(p2pkh_script: &[u8]) -> [u8; 21] {
        crate::address::script_to_dest(p2pkh_script)
            .expect("parse")
            .payload()
    }

    #[test]
    fn compute_modifier_bucket_reuse_and_full_generation() {
        // Synthetic chain: 2000 blocks, 120s spacing, PoW (hash-proof = hash).
        // Genesis is the only generated block initially.
        let mut idx = ReplayIndex::new();
        let t0 = 1_400_000_400u32; // divisible by 300: sits on a modifier-interval bucket edge
        for h in 0..2000usize {
            idx.push(synthetic_entry(h, t0 + (h as u32) * 120, false, h == 0));
        }
        // Same interval bucket as the genesis modifier time -> reuse (false
        // flag). h1 time = t0+120 stays in t0's 300s bucket.
        let (m, g) = compute_next_stake_modifier(&idx, Some(1)).expect("reuse path");
        assert_eq!(m, 0, "genesis modifier is 0");
        assert!(!g, "same bucket must reuse, not regenerate");
        // Past the bucket boundary -> full 64-round generation
        let (m2, g2) = compute_next_stake_modifier(&idx, Some(1500)).expect("generation path");
        assert!(g2, "new bucket must generate");
        assert_ne!(
            m2, 0,
            "64 entropy rounds producing all-zero is astronomically unlikely"
        );
        // Deterministic
        let (m2b, _) = compute_next_stake_modifier(&idx, Some(1500)).unwrap();
        assert_eq!(m2, m2b);
    }

    #[test]
    fn kernel_modifier_walk_fixed_window() {
        // Walk stops at the first block whose last-generated-modifier time
        // reaches blockFrom.time + 600; returns THAT block's stored modifier.
        let mut idx = ReplayIndex::new();
        let t0 = 1_400_000_400u32; // divisible by 300: sits on a modifier-interval bucket edge
        for h in 0..40usize {
            let mut e = synthetic_entry(h, t0 + (h as u32) * 120, false, false);
            // generate a fresh modifier every 5 blocks (600s), values 1,2,3...
            if h > 0 && h % 5 == 0 {
                e.generated_stake_modifier = true;
                e.stake_modifier = (h / 5) as u64;
            }
            idx.push(e);
        }
        idx.entries.to_mut()[0].generated_stake_modifier = true;
        // blockFrom at 1: threshold = t0+120+600 = t0+720. Generated blocks at
        // t0+600 (h5, mod 1) and t0+1200 (h10, mod 2). Walk: h2..h5 update to
        // t0+600 <720; keep walking; h10 sets mod time t0+1200 >= 720 -> stop
        // at h10, return h10's stored modifier = 2.
        let m = get_kernel_stake_modifier(&idx, 1).expect("walk");
        assert_eq!(m, 2);
        // blockFrom at 6: threshold = t0+720+600 = t0+1320 -> stop at h11's
        // successor region: generated at h10 (t0+1200 < 1320) keeps walking;
        // h15 (t0+1800) >= 1320 -> stop at h15, modifier 3.
        let m = get_kernel_stake_modifier(&idx, 6).expect("walk");
        assert_eq!(m, 3);
        // insufficient chain -> error
        assert!(get_kernel_stake_modifier(&idx, 35).is_err());
    }

    #[test]
    fn sparse_modifier_window_selects_candidates_beyond_stop() {
        // kernel.cpp:102-103: with fSelected false the section-stop bound is
        // not applied — the first fresh candidate in (time, hash) order is
        // selected even when every candidate lies beyond the round's stop.
        // Synthetic chain: 30 blocks at 120 s, a 4000 s gap, 2 more blocks.
        // Generation at h31: selection window starts ~5160 s before the post-
        // gap bucket edge while round stops advance ~100-300 s from there, so
        // ALL fresh candidates are beyond the stop in every one of the 32
        // rounds -> heights are selected in order 0,1,2,... -> the modifier
        // bits are entropy_bit(h) = h&1 -> 0xAAAAAAAA exactly.
        let mut idx = ReplayIndex::new();
        let t0 = 1_400_000_400u32;
        for h in 0..30usize {
            idx.push(synthetic_entry(h, t0 + (h as u32) * 120, false, h == 0));
        }
        idx.push(synthetic_entry(30, t0 + 29 * 120 + 4000, false, false));
        idx.push(synthetic_entry(31, t0 + 29 * 120 + 4120, false, false));
        let (m, g) = compute_next_stake_modifier(&idx, Some(31))
            .expect("sparse window must still select one block per round");
        assert!(g, "new bucket must generate");
        // 32 candidates -> min(64, 32) = 32 rounds (kernel.cpp:187) -> only
        // the low 32 bits carry entropy: bits = h&1 in height order.
        assert_eq!(m, 0x0000_0000_AAAA_AAAA);
        let (m2, _) = compute_next_stake_modifier(&idx, Some(31)).unwrap();
        assert_eq!(m, m2, "deterministic");
    }

    #[test]
    fn stake_modifier_checksum_stream_and_genesis_constant() {
        // stream vector with previous checksum: [prev || flags || proof || modifier]
        let mut s = Vec::new();
        s.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        s.extend_from_slice(&7u32.to_le_bytes());
        s.extend_from_slice(&[0xAAu8; 32]);
        s.extend_from_slice(&0x0e00_670bu64.to_le_bytes());
        let h = sha256d(&s);
        let expected = u32::from_le_bytes([h[28], h[29], h[30], h[31]]);
        assert_eq!(
            stake_modifier_checksum(Some(0x1122_3344), 7, &[0xAAu8; 32], 0x0e00_670b),
            expected
        );
        // genesis has no pprev: the 4 previous-checksum bytes are omitted
        let mut s2 = Vec::new();
        s2.extend_from_slice(&7u32.to_le_bytes());
        s2.extend_from_slice(&[0xAAu8; 32]);
        s2.extend_from_slice(&0x0e00_670bu64.to_le_bytes());
        let h2 = sha256d(&s2);
        let expected2 = u32::from_le_bytes([h2[28], h2[29], h2[30], h2[31]]);
        assert_eq!(
            stake_modifier_checksum(None, 7, &[0xAAu8; 32], 0x0e00_670b),
            expected2
        );
        // deployed genesis checkpoint (kernel.cpp:20-21): genesis hash byte 0
        // is 0x21 -> entropy bit 1; PoW; generated at genesis -> flags 6;
        // hashProofOfStake 0; modifier 0.
        assert_eq!(index_flags(false, 1, true), 6);
        assert_eq!(index_flags(true, 0, false), 1);
        assert_eq!(
            stake_modifier_checksum(None, 6, &[0u8; 32], 0),
            GENESIS_MODIFIER_CHECKSUM,
            "genesis modifier checksum must match the compiled client constant"
        );
    }

    #[test]
    fn utxo_apply_block_reported_counts_anomalies() {
        use crate::legacy::{Tx, Vin, Vout};
        let coinbase = |value: i64, pk: Vec<u8>| Tx {
            version: 1,
            time: 1_400_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value,
                script_pubkey: pk,
            }],
            locktime: 0,
        };
        let mut set = UtxoSet::new();
        // block 0: one txid X output value 5000 (via a null-input tx)
        let fund = coinbase(5000, vec![0xac]);
        let blk0 = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_000_000,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![fund.clone()],
            block_sig: vec![],
        };
        let rep = set.apply_block_reported(&blk0, 0);
        assert_eq!(rep.untracked_spends, 0);
        assert_eq!(rep.duplicate_txids, 0);
        let x = fund.hash();
        assert!(set.get(&(x, 0)).is_some());
        // block 1: identical tx again -> duplicate-txid overwrite counted
        let rep = set.apply_block_reported(&blk0, 1);
        assert_eq!(rep.duplicate_txids, 1, "overwrite must be counted");
        assert_eq!(set.get(&(x, 0)).unwrap().height, 1, "newest meta wins");
        // block 2: spend of an unknown prevout -> counted, NOT removed from
        // the map, and the spending tx's own outputs are still created
        let bad_spend = Tx {
            version: 1,
            time: 1_400_003_600,
            vin: vec![Vin {
                prev_hash: [0xEE; 32],
                prev_n: 9,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 10,
                script_pubkey: vec![0xac],
            }],
            locktime: 0,
        };
        let blk2 = Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: 1_400_003_600,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![bad_spend.clone()],
            block_sig: vec![],
        };
        let rep = set.apply_block_reported(&blk2, 2);
        assert_eq!(rep.untracked_spends, 1);
        let y = bad_spend.hash();
        assert!(
            set.get(&(y, 0)).is_some(),
            "outputs of a bad-spend tx are still created"
        );
        assert!(
            set.get(&(x, 0)).is_some(),
            "untracked input must not remove anything"
        );
    }

    #[test]
    fn check_proof_of_stake_guard_failures() {
        use crate::legacy::{Tx, Vin, Vout};
        let t_fund = 1_400_000_000u32;
        let mut idx = ReplayIndex::new();
        idx.push(synthetic_entry(0, t_fund, false, true));
        // h1 carries a generated modifier so the fixed-window walk from the
        // stake-input block (h0) can stop there
        idx.push(synthetic_entry(1, t_fund + 7200, true, true));
        // funding tx in block 0
        let fund = Tx {
            version: 1,
            time: t_fund,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                },
                Vout {
                    value: 5_000_000,
                    script_pubkey: vec![0xac],
                },
            ],
            locktime: 0,
        };
        let blk0 = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0u8; 32],
            time: t_fund,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![fund.clone()],
            block_sig: vec![],
        };
        let mut utxos = UtxoSet::new();
        utxos.apply_block_reported(&blk0, 0);
        let txid = fund.hash();
        let coinstake = |tx_time: u32| Tx {
            version: 1,
            time: tx_time,
            vin: vec![Vin {
                prev_hash: txid,
                prev_n: 1,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                },
                Vout {
                    value: 5_000_000,
                    script_pubkey: vec![0xac],
                },
            ],
            locktime: 0,
        };
        let mk_block = |stake: &Tx| Block {
            version: 1,
            prev_hash: [1u8; 32],
            merkle_root: [0u8; 32],
            time: stake.time,
            bits: 0x207f_ffff,
            nonce: 0,
            vtx: vec![
                {
                    let mut cb = stake.clone();
                    cb.vin = vec![Vin {
                        prev_hash: [0u8; 32],
                        prev_n: u32::MAX,
                        script_sig: vec![1],
                        sequence: u32::MAX,
                    }];
                    cb.vout = vec![Vout {
                        value: 0,
                        script_pubkey: vec![],
                    }];
                    cb
                },
                stake.clone(),
            ],
            block_sig: vec![],
        };
        // nTime violation: kernel tx time before the stake output's tx time
        let b = mk_block(&coinstake(t_fund - 10));
        let err = check_proof_of_stake(&idx, &b, &utxos, 0x207f_ffff, 0).unwrap_err();
        assert!(err.contains("nTime violation"), "got: {}", err);
        // min-age violation: 1800 s < nStakeMinAge 3600
        let b = mk_block(&coinstake(t_fund + 1800));
        let err = check_proof_of_stake(&idx, &b, &utxos, 0x207f_ffff, 0).unwrap_err();
        assert!(err.contains("min age violation"), "got: {}", err);
        // exactly at min age the guards pass and the kernel is computed
        let b = mk_block(&coinstake(t_fund + 3600));
        let res =
            check_proof_of_stake(&idx, &b, &utxos, 0x207f_ffff, 0).expect("guards pass at min age");
        let _ = res.hash_proof_of_stake;
    }
}
