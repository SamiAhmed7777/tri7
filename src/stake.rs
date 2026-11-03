//! Phase 4 — staking mint (coinstake creation) + proof-of-stake block
//! assembly, ported 1:1 from the live triangles_v5 v6.2.6.7 C++:
//!
//!   - CWallet::CreateCoinStake   wallet.cpp:2225-2441 — the StakeMiner kernel
//!     search: min-age coin filter (wallet.cpp:2282), per-second backward time
//!     probe n=0..min(nSearchInterval, nMaxStakeSearchInterval=60)
//!     (wallet.cpp:2281-2292) over CheckStakeKernelHash, Solver key resolution
//!     (wallet.cpp:2297-2345), stake-split rule nStakeSplitAge = 3*60*60
//!     (wallet.cpp:29, 2353-2354), combine-input pass nStakeCombineThreshold =
//!     20*COIN (wallet.cpp:30, 2372-2401), coin-age reward
//!     (wallet.cpp:2403-2414), output funding (wallet.cpp:2417-2424),
//!     per-input SignSignature (wallet.cpp:2426-2432), size cap
//!     MAX_BLOCK_SIZE_GEN/5 (wallet.cpp:2434-2437).
//!   - CreateNewBlock PoS template miner.cpp:96-110 + 333-341 — coinbase with
//!     null prevout, height scriptSig (+ empty COINBASE_FLAGS, main.cpp:443),
//!     SetEmpty() output, pblock->nBits = GetNextTargetRequired(prev, true),
//!     time = max(MTP+1, GetMaxTransactionTime()) then max(., PastDrift),
//!     nNonce = 0.
//!   - CBlock::SignBlock main.cpp:3778-3815 — the coinstake timestamp protocol
//!     (txCoinStake.nTime >= max(MTP+1, PastDrift)), vtx[0].nTime = coinstake
//!     time, future-tx erase, vtx.insert(begin()+1, coinstake), merkle rebuild,
//!     key.Sign(GetHash(), vchBlockSig).
//!   - CTransaction::GetCoinAge main.cpp:3082-3140 — cent-second accumulation
//!     (nValueIn * dt / CENT per input; containing-BLOCK-time min-age skip;
//!     nTime < utxo.nTxTime hard fail), bnCoinDay = bnCentSecond * CENT / 86400.
//!   - GetProofOfStakeReward main.cpp:1539-1560 — CBigNum integer division
//!     order: (nCoinAge * MAX_TRI_PROOF_OF_STAKE / 365) / COIN + nFees.
//!   - ConnectBlock stake-reward gate main.cpp:2421-2427, 2470-2498 — reject
//!     when nStakeReward (GetValueOut - GetValueIn of vtx[1]) exceeds the
//!     calculated reward (nFees excludes the coinstake).
//!
//! Every rule mirrors the C++ semantics exactly; "improvements" are forbidden.
//! Nothing in this module performs network I/O — assembly produces candidate
//! blocks for LOCAL self-validation only (see src/bin/stake_mint.rs).
//!
//! Documented, validity-neutral deviations (same policy as spend.rs):
//!   - candidate iteration order is deterministic (caller-supplied order; the
//!     bin sorts by height, txid, n). The C++ iterates a std::set keyed by
//!     CWalletTx pointer order, which is non-deterministic — any single
//!     solution yields an equally valid block.

use std::collections::HashMap;

use primitive_types::{U256, U512};

use crate::consensus::{
    max_time_drift, stake_weight_era, tx_is_coin_base, tx_is_coin_stake, COIN,
    MAX_TRI_PROOF_OF_STAKE, STAKE_MIN_AGE,
};
use crate::difficulty::set_compact;
use crate::kernel::{get_kernel_stake_modifier, KernelOutputResolver, UtxoMeta};
use crate::legacy::{hash_to_display, Block, Hash, Tx, Vin, Vout};
use crate::replay::{IndexEntry, ReplayIndex};
use crate::spend::{sign_input, sign_msg, solver_type, SpendType, SIGHASH_ALL};

/// wallet.cpp:29 — `unsigned int nStakeSplitAge = 1 * 3 * 60 * 60;`
pub const STAKE_SPLIT_AGE: i64 = 3 * 60 * 60;
/// wallet.cpp:30 — `int64_t nStakeCombineThreshold = 20 * COIN;`
pub const STAKE_COMBINE_THRESHOLD: i64 = 20 * COIN;
/// main.cpp:451 — `int64_t nReserveBalance = 0;` (default)
pub const RESERVE_BALANCE: i64 = 0;
/// wallet.cpp:2281 — `static int nMaxStakeSearchInterval = 60;`
pub const MAX_STAKE_SEARCH_INTERVAL: i64 = 60;
/// main.h:40 / wallet.cpp:2436 — coinstake size cap (MAX_BLOCK_SIZE_GEN/5).
pub const COINSTAKE_SIZE_LIMIT: usize = crate::spend::MAX_BLOCK_SIZE_GEN / 5;

// ---------------------------------------------------------------------------
// CScript push_int64 (CScript::operator<<(int64_t), script.h) — used for the
// coinbase height prefix (miner.cpp:106). -1 → OP_1NEGATE, 0..16 → OP_N,
// otherwise a minimal-length little-endian signed encoding (0x00 pad only
// when the top bit of the last byte is set).
// ---------------------------------------------------------------------------
pub fn push_script_int64(w: &mut Vec<u8>, v: i64) {
    if v == -1 {
        w.push(0x4f);
    } else if (0..=16).contains(&v) {
        w.push(if v == 0 { 0x00 } else { 0x50 + v as u8 });
    } else {
        let mut data = Vec::with_capacity(8);
        let mut n = v;
        while n > 0 {
            data.push((n & 0xff) as u8);
            n >>= 8;
        }
        if data.last().copied().unwrap_or(0) & 0x80 != 0 {
            data.push(0x00);
        }
        crate::spend::push_data(w, &data);
    }
}

// ---------------------------------------------------------------------------
// One staking candidate: a live wallet UTXO with everything the kernel and
// the signer need. `block_height`/`block_time` describe the BLOCK CONTAINING
// the prevout tx (the C++ reads that block header for the min-age guard and
// the modifier walk); `tx_offset` is nTxPrevOffset (kernel.cpp:419).
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct StakeCandidate {
    pub txid: Hash,
    pub n: u32,
    pub value: i64,
    pub script_pubkey: Vec<u8>,
    pub block_height: usize,
    pub block_time: u32,
    pub tx_time: u32,
    pub tx_offset: u32,
    /// Wallet key owning the prevout script (resolved before the search — the
    /// C++ keystore lookup inside CreateCoinStake would break the coin).
    pub secret: [u8; 32],
    pub pubkey: Vec<u8>,
}

impl StakeCandidate {
    pub fn outpoint(&self) -> (Hash, u32) {
        (self.txid, self.n)
    }
}

/// Diagnostics of one kernel hit (kernel.cpp:373-378 stream + target product).
#[derive(Clone, Debug)]
pub struct KernelHit {
    pub n_time: u32,
    pub seconds_back: u32,
    pub modifier: u64,
    pub hash_proof_of_stake: Hash,
    pub coin_day_weight: i128,
    pub target: U512,
}

/// Successful CreateCoinStake result.
pub struct CoinstakePlan {
    pub tx: Tx,
    /// The kernel coin — its key signs the block header (SignBlock key).
    pub signer: StakeCandidate,
    /// Every coinstake input (kernel first, then combined coins).
    pub inputs: Vec<StakeCandidate>,
    pub coin_age: u64,
    pub reward: i64,
    pub credit: i64,
    pub fees: i64,
    pub split: bool,
    pub kernel: KernelHit,
}

/// Counters of one search pass (dry-run report).
#[derive(Default, Clone, Copy, Debug)]
pub struct SearchReport {
    pub coins_considered: u64,
    pub coins_meeting_min_age: u64,
    pub attempts: u64,
    pub kernel_hits: u64,
    pub zero_reward_aborts: u64,
    pub credit_over_balance_aborts: u64,
}

// ---------------------------------------------------------------------------
// Coin age (CTransaction::GetCoinAge, main.cpp:3082-3140)
// ---------------------------------------------------------------------------
/// `inputs` = (value, tx_time, containing-block time) per vin.
/// Unresolvable inputs are skipped by the caller (C++ `continue` on the
/// txindex fallback failure, main.cpp:3097-3100); nTime < utxo.nTxTime fails
/// the whole call (main.cpp:3113-3114); inputs whose containing block violates
/// the min age are skipped (main.cpp:3125-3126).
pub fn coin_age_of(inputs: &[(i64, u32, u32)], n_time: u32) -> Result<u64, String> {
    let mut cent_seconds: i128 = 0;
    for &(value, tx_time, block_time) in inputs {
        if (n_time as i64) < tx_time as i64 {
            return Err("GetCoinAge: transaction timestamp violation".into());
        }
        if block_time as i64 + STAKE_MIN_AGE as i64 > n_time as i64 {
            continue; // only count coins meeting min age requirement
        }
        // bnCentSecond += CBigNum(nValueIn) * (nTime - utxo.nTxTime) / CENT
        cent_seconds +=
            (value as i128 * (n_time as i64 - tx_time as i64) as i128) / crate::spend::CENT as i128;
    }
    // bnCoinDay = bnCentSecond * CENT / (24 * 60 * 60)  (main.cpp:3132)
    let coin_day = cent_seconds * crate::spend::CENT as i128 / 86_400i128;
    Ok(coin_day as u64)
}

/// GetProofOfStakeReward (main.cpp:1539-1560) — CBigNum division order:
/// ((nCoinAge * MAX_TRI_PROOF_OF_STAKE) / 365) / COIN, then + nFees.
/// i128 is exact: nCoinAge ≤ u64::MAX ⇒ product < 2^83.
pub fn stake_reward(coin_age: u64, fees: i64) -> i64 {
    let subsidy = (coin_age as i128 * MAX_TRI_PROOF_OF_STAKE as i128) / 365 / COIN as i128;
    (subsidy + fees as i128) as i64
}

// ---------------------------------------------------------------------------
// The kernel search — CreateCoinStake (wallet.cpp:2225-2441)
// ---------------------------------------------------------------------------
/// One CreateCoinStake pass over `candidates` (caller-ordered). `idx` is the
/// chain index the modifier walk reads (absolute heights); `next_height` is
/// the height the minted block would take (selects the GetWeight era);
/// `search_time` is GetAdjustedTime(); `search_interval` is the seconds window
/// probed backward (capped at MAX_STAKE_SEARCH_INTERVAL like the C++).
/// Returns the plan (None = nothing solvable this pass, exactly the C++
/// `return false` paths) plus the search counters.
#[allow(clippy::too_many_arguments)]
pub fn create_coinstake(
    idx: &ReplayIndex,
    candidates: &[StakeCandidate],
    n_bits: u32,
    fees: i64,
    search_time: u32,
    search_interval: i64,
    balance: i64,
    next_height: usize,
) -> Result<(Option<CoinstakePlan>, SearchReport), String> {
    let mut rep = SearchReport::default();
    if balance <= RESERVE_BALANCE {
        return Ok((None, rep)); // wallet.cpp:2246-2247
    }
    let interval = search_interval.min(MAX_STAKE_SEARCH_INTERVAL);
    let (target_per_coin_day, target_neg, _ovf) = set_compact(n_bits);

    // wallet.cpp:2264-2364 — per-coin loop; the first solving coin wins.
    for coin in candidates {
        rep.coins_considered += 1;
        // wallet.cpp:2282-2283 — only count coins meeting min age requirement
        // (measured against the worst-case probe time search_time - 60).
        if coin.block_time as i64 + STAKE_MIN_AGE as i64
            > search_time as i64 - MAX_STAKE_SEARCH_INTERVAL
        {
            continue;
        }
        rep.coins_meeting_min_age += 1;

        // The stake modifier depends only on the input's containing block —
        // constant across the time probes (kernel.cpp:373 GetKernelStakeModifier).
        let modifier = match get_kernel_stake_modifier(idx, coin.block_height) {
            Ok(m) => m,
            // wallet.cpp:2269-2271 — ReadTxIndex/ReadFromDisk failure skips
            // the coin (our window simply ends before the walk completes).
            Err(_) => continue,
        };

        let mut kernel_hit: Option<KernelHit> = None;
        for n in 0..interval {
            // wallet.cpp:2292 — search backward in time: nTimeTx = nTime - n.
            let tx_time = (search_time as i64 - n) as u32;
            // CheckStakeKernelHash guards (kernel.cpp:314-318).
            if tx_time < coin.tx_time {
                continue;
            }
            if coin.block_time as i64 + STAKE_MIN_AGE as i64 > tx_time as i64 {
                continue;
            }
            rep.attempts += 1;
            // Weight + target (kernel.cpp:322-327), era from next_height.
            let weight = stake_weight_era(coin.tx_time, tx_time, next_height);
            let coin_day_weight = (coin.value as i128 * weight as i128) / COIN as i128 / 86_400i128;
            let target = U512::from(coin_day_weight as u128) * U512::from(target_per_coin_day);
            let hash = crate::consensus::stake_kernel_hash(
                modifier,
                coin.block_time,
                coin.tx_offset,
                coin.tx_time,
                coin.n,
                tx_time,
            );
            let meets = !target_neg && U512::from(U256::from_little_endian(&hash)) <= target;
            if meets {
                kernel_hit = Some(KernelHit {
                    n_time: tx_time,
                    seconds_back: n as u32,
                    modifier,
                    hash_proof_of_stake: hash,
                    coin_day_weight,
                    target,
                });
                break;
            }
        }

        let hit = match kernel_hit {
            Some(h) => h,
            None => continue,
        };
        rep.kernel_hits += 1;

        // wallet.cpp:2297-2314 — Solver resolution of the kernel script; only
        // PubKey / PubKeyHash kernels are supported (otherwise break = skip).
        let spend_type = match solver_type(&coin.script_pubkey) {
            Some(t @ SpendType::PubKey(_)) | Some(t @ SpendType::PubkeyHash(_)) => t,
            _ => continue,
        };
        // keystore checks (wallet.cpp:2315-2342): the candidate's key must be
        // THE key of the kernel script.
        match &spend_type {
            SpendType::PubKey(pk) => {
                if pk.as_slice() != coin.pubkey.as_slice() {
                    continue; // keys mismatch
                }
            }
            SpendType::PubkeyHash(h) => {
                if &crate::address::hash160(&coin.pubkey) != h {
                    continue; // failed to get key for kernel type
                }
            }
        }
        // scriptPubKeyOut: P2PK kernels reuse the kernel script verbatim;
        // P2PKH kernels pay to a bare P2PK of the wallet key
        // (wallet.cpp:2323, 2344).
        let script_pub_key_out = match &spend_type {
            SpendType::PubKey(_) => coin.script_pubkey.clone(),
            SpendType::PubkeyHash(_) => {
                crate::spend::script_pubkey_p2pk(&coin.pubkey).map_err(|e| e.to_string())?
            }
        };

        // wallet.cpp:2347-2354 — lock in the solving time, first input, marker,
        // stake output; split-stake output when the weight is young.
        let tx_time = hit.n_time;
        let split = stake_weight_era(coin.block_time, tx_time, next_height) < STAKE_SPLIT_AGE;
        let mut inputs: Vec<StakeCandidate> = vec![coin.clone()];
        let mut vout: Vec<Vout> = vec![
            Vout {
                value: 0,
                script_pubkey: Vec::new(),
            }, // IsCoinStake marker
            Vout {
                value: 0,
                script_pubkey: script_pub_key_out.clone(),
            },
        ];
        if split {
            vout.push(Vout {
                value: 0,
                script_pubkey: script_pub_key_out.clone(),
            });
        }

        // wallet.cpp:2372-2401 — combine pass: more inputs of the SAME key,
        // gated by `txNew.vout.size() == 2` (a split stake never combines).
        for other in candidates {
            if vout.len() != 2 {
                break;
            }
            let same_script = other.script_pubkey == coin.script_pubkey
                || other.script_pubkey == script_pub_key_out;
            if !same_script || other.txid == inputs[0].txid {
                continue; // same key/address coins only; not the kernel txid
            }
            if inputs.len() >= 100 {
                break; // too many inputs
            }
            let credit: i64 = inputs.iter().map(|i| i.value).sum();
            if credit >= STAKE_COMBINE_THRESHOLD {
                break;
            }
            if credit + other.value > balance - RESERVE_BALANCE {
                break; // reached the reserve limit
            }
            if other.value >= STAKE_COMBINE_THRESHOLD {
                continue; // do not add additional significant input
            }
            // wallet.cpp:2379 + 2394 — weight keyed on the PREV TX time here.
            if stake_weight_era(other.tx_time, tx_time, next_height) < STAKE_MIN_AGE as i64 {
                continue; // still too young
            }
            inputs.push(other.clone());
        }

        // wallet.cpp:2366-2370 — nCredit == 0 or above balance fails the call
        // (checked on the input-value credit, before the reward is added).
        let input_credit: i64 = inputs.iter().map(|i| i.value).sum();
        if input_credit == 0 || input_credit > balance - RESERVE_BALANCE {
            rep.credit_over_balance_aborts += 1;
            return Ok((None, rep));
        }

        // wallet.cpp:2403-2414 — coin age + reward.
        let age_inputs: Vec<(i64, u32, u32)> = inputs
            .iter()
            .map(|i| (i.value, i.tx_time, i.block_time))
            .collect();
        let coin_age = coin_age_of(&age_inputs, tx_time)?;
        let reward = stake_reward(coin_age, fees);
        if reward <= 0 {
            // wallet.cpp:2410-2412 — the whole CreateCoinStake call fails.
            rep.zero_reward_aborts += 1;
            return Ok((None, rep));
        }
        let credit = input_credit + reward; // wallet.cpp:2414

        // wallet.cpp:2417-2424 — fund the stake output(s).
        if vout.len() == 3 {
            vout[1].value = (credit / 2 / crate::spend::CENT) * crate::spend::CENT;
            vout[2].value = credit - vout[1].value;
        } else {
            vout[1].value = credit;
        }

        let mut tx = Tx {
            version: 1,
            time: tx_time,
            vin: inputs
                .iter()
                .map(|i| Vin {
                    prev_hash: i.txid,
                    prev_n: i.n,
                    script_sig: Vec::new(),
                    sequence: u32::MAX,
                })
                .collect(),
            vout,
            locktime: 0,
        };

        // wallet.cpp:2426-2432 — SignSignature per input.
        for (i, input) in inputs.iter().enumerate() {
            sign_input(
                &mut tx,
                i,
                &input.script_pubkey,
                &input.secret,
                &input.pubkey,
                SIGHASH_ALL,
            )
            .map_err(|e| format!("CreateCoinStake: failed to sign coinstake: {}", e))?;
        }

        // wallet.cpp:2434-2437 — size limit.
        let n_bytes = tx.serialize().len();
        if n_bytes >= COINSTAKE_SIZE_LIMIT {
            return Err(format!(
                "CreateCoinStake: exceeded coinstake size limit ({})",
                n_bytes
            ));
        }

        let plan = CoinstakePlan {
            tx,
            signer: coin.clone(),
            inputs,
            coin_age,
            reward,
            credit,
            fees,
            split,
            kernel: hit,
        };
        return Ok((Some(plan), rep));
    }
    Ok((None, rep))
}

// ---------------------------------------------------------------------------
// Block assembly — CreateNewBlock PoS template + SignBlock (miner.cpp:96-110,
// 333-341; main.cpp:3778-3815)
// ---------------------------------------------------------------------------
/// Assemble the PoS block around a solved coinstake. `prev` is the tip entry,
/// `median_time_past` the 11-entry MTP at the tip, `extra_txs` the optional
/// wallet transactions carried from the template. Returns the UNSIGNED block
/// (vchBlockSig empty) — call `sign_block` next.
pub fn assemble_pos_block(
    prev: &IndexEntry,
    median_time_past: u32,
    coinstake: &Tx,
    extra_txs: &[Tx],
    n_bits: u32,
) -> Result<Block, String> {
    if !tx_is_coin_stake(coinstake) {
        return Err("SignBlock: template tx is not a coinstake".into());
    }
    let next_height = prev.height + 1;
    let drift = max_time_drift(next_height); // FutureDrift/PastDrift window

    // main.cpp:3794-3796 — the coinstake must meet the timestamp protocol.
    let past_limit = std::cmp::max(median_time_past as i64 + 1, prev.time as i64 - drift);
    if (coinstake.time as i64) < past_limit {
        return Err(format!(
            "SignBlock: coinstake nTime {} below the timestamp floor {}",
            coinstake.time, past_limit
        ));
    }

    // miner.cpp:96-110 — PoS coinbase template: null prevout, height scriptSig
    // (+ empty COINBASE_FLAGS), SetEmpty() output. vtx[0].nTime becomes the
    // coinstake time in SignBlock (main.cpp:3798).
    let script_sig = {
        let mut s = Vec::new();
        push_script_int64(&mut s, next_height as i64);
        s // + COINBASE_FLAGS (empty CScript, main.cpp:443)
    };
    let mut coinbase = Tx {
        version: 1,
        time: coinstake.time,
        vin: vec![Vin {
            prev_hash: [0u8; 32],
            prev_n: u32::MAX,
            script_sig,
            sequence: u32::MAX,
        }],
        vout: vec![Vout {
            value: 0,
            script_pubkey: Vec::new(),
        }],
        locktime: 0,
    };
    if !tx_is_coin_base(&coinbase) {
        return Err("SignBlock: coinbase template failed IsCoinBase".into());
    }

    // main.cpp:3798-3801 — timestamp protocol over the template tx set.
    coinbase.time = coinstake.time;
    let mut vtx: Vec<Tx> = Vec::with_capacity(2 + extra_txs.len());
    vtx.push(coinbase);
    vtx.extend_from_slice(extra_txs);
    let max_tx_time = vtx.iter().map(|t| t.time).max().unwrap_or(coinstake.time);
    let mut n_time = std::cmp::max(median_time_past as i64 + 1, max_tx_time as i64);
    n_time = std::cmp::max(n_time, prev.time as i64 - drift);
    // main.cpp:3803-3805 — no future timestamps in the tx set.
    vtx.retain(|t| (t.time as i64) <= n_time);
    let block_time = n_time as u32;

    // main.cpp:3806-3807 — coinstake at vtx[1]; CheckCoinStakeTimestamp
    // requires block.nTime == vtx[1].nTime exactly.
    vtx.insert(1, coinstake.clone());

    let mut block = Block {
        version: 1,
        prev_hash: prev.hash,
        merkle_root: [0u8; 32],
        time: block_time,
        bits: n_bits,
        nonce: 0, // miner.cpp:341
        vtx,
        block_sig: Vec::new(),
    };
    block.merkle_root = block.merkle_root();
    Ok(block)
}

/// main.cpp:3810 — `key.Sign(GetHash(), vchBlockSig)` with the coinstake
/// output key: bare DER ECDSA over the Hash9 header hash.
pub fn sign_block(block: &mut Block, secret: &[u8; 32]) -> Result<(), String> {
    let hash = block.hash();
    block.block_sig = sign_msg(secret, &hash)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Self-validation — the tri7_core acceptance path (validate_pos /
// follow_tip::validate_block minus the protocol layer, plus the ConnectBlock
// reward gate), for LOCAL dry runs. No network, no daemon, no side effects.
// ---------------------------------------------------------------------------
/// Prevout metadata for the candidate's inputs, so the validator can resolve
/// the kernel and tally values without a full UTXO set.
pub struct PointOutputs(pub HashMap<(Hash, u32), UtxoMeta>);

impl KernelOutputResolver for PointOutputs {
    fn kernel_meta(&self, key: &(Hash, u32)) -> Option<UtxoMeta> {
        self.0.get(key).copied()
    }
}

#[derive(Debug)]
pub struct DryrunValidation {
    pub link_ok: bool,
    pub structure_ok: bool,
    pub merkle_ok: bool,
    pub time_ok: bool,
    pub n_bits_ok: bool,
    pub kernel_meets_target: bool,
    pub signature_ok: bool,
    pub finality_ok: bool,
    pub reward_ok: bool,
    pub size_ok: bool,
    pub kernel: crate::kernel::KernelResult,
    pub stake_reward_paid: i64,
    pub stake_reward_calculated: i64,
    pub coin_age: u64,
    pub block_hash_display: String,
}

/// Full local acceptance check of an assembled candidate whose tip is the
/// index tip. Mirrors validate_pos / follow_tip::validate_block in order.
pub fn validate_dryrun_block<R: KernelOutputResolver>(
    idx: &ReplayIndex,
    outputs: &R,
    block: &Block,
    now_secs: i64,
) -> Result<DryrunValidation, String> {
    let tip = idx.tip().ok_or("validate: empty index")?;
    let tip_height = tip.height;
    let next_height = tip_height + 1;
    let mut v = DryrunValidation {
        link_ok: false,
        structure_ok: false,
        merkle_ok: false,
        time_ok: false,
        n_bits_ok: false,
        kernel_meets_target: false,
        signature_ok: false,
        finality_ok: false,
        reward_ok: false,
        size_ok: false,
        kernel: crate::kernel::KernelResult {
            hash_proof_of_stake: [0u8; 32],
            meets_target: false,
            modifier: 0,
            block_from_height: 0,
            coin_day_weight: 0,
            target: U512::zero(),
        },
        stake_reward_paid: 0,
        stake_reward_calculated: 0,
        coin_age: 0,
        block_hash_display: hash_to_display(&block.hash()),
    };

    // 1. link (AcceptBlock main.cpp:3405-3408)
    v.link_ok = block.prev_hash == tip.hash;
    if !v.link_ok {
        return Err("validate: candidate does not extend the tip".into());
    }

    // 2. structure (CheckBlock main.cpp:3323-3352)
    if block.vtx.len() < 2
        || !tx_is_coin_base(&block.vtx[0])
        || block.vtx[1..].iter().any(tx_is_coin_base)
        || !tx_is_coin_stake(&block.vtx[1])
        || block.vtx[1..].iter().skip(1).any(tx_is_coin_stake)
        || block.vtx[0].vout.len() != 1
        || block.vtx[0].vout[0].value != 0
        || !block.vtx[0].vout[0].script_pubkey.is_empty()
        || block.vtx[0].vin[0].script_sig.len() < 2 // CheckTransaction coinbase scriptSig 2..100
        || block.vtx[0].vin[0].script_sig.len() > 100
        || block.vtx[1].time != block.time
    // CheckCoinStakeTimestamp (main.cpp:3341-3343)
    {
        return Err("validate: PoS structure failure".into());
    }
    v.structure_ok = true;

    // 3. merkle (CheckBlock main.cpp:3387-3390)
    v.merkle_ok = block.merkle_root() == block.merkle_root;
    if !v.merkle_ok {
        return Err("validate: merkle root mismatch".into());
    }

    // 4. timestamps (AcceptBlock main.cpp:3432-3434 + CheckBlock 3314-3321,
    //    3327-3333; GetPastTimeLimit = median of 11 entries, main.h:1331-1345)
    if tip_height + 1 < 11 {
        return Err("validate: index too short for MTP".into());
    }
    let mut times: Vec<u32> = idx.entries[tip_height + 1 - 11..=tip_height]
        .iter()
        .map(|e| e.time)
        .collect();
    times.sort_unstable();
    let median_time_past = times[5];
    let drift = max_time_drift(next_height);
    if block.time <= median_time_past
        || (block.time as i64) + drift < tip.time as i64
        || block.time as i64 > now_secs + 15 * 60
        || block.vtx[0].time + 10 * 60 < block.time
    {
        return Err("validate: timestamp protocol failure".into());
    }
    v.time_ok = true;

    // 5. nBits (AcceptBlock main.cpp:3414-3430; live era always enforced)
    let expected_bits = crate::difficulty::get_next_target_required(idx, true);
    v.n_bits_ok = block.bits == expected_bits;
    if !v.n_bits_ok {
        return Err(format!(
            "validate: NBITS FAIL block={:08x} expected={:08x}",
            block.bits, expected_bits
        ));
    }

    // 6. kernel (AcceptBlock main.cpp:3451-3472 — the CheckProofOfStake path)
    let kr = crate::kernel::check_proof_of_stake(idx, block, outputs, block.bits, tip_height)?;
    v.kernel = kr;
    v.kernel_meets_target = v.kernel.meets_target;
    if !v.kernel_meets_target {
        return Err(format!(
            "validate: KERNEL TARGET FAIL modifier={:016x} coinDays={} hashProof={}",
            v.kernel.modifier,
            v.kernel.coin_day_weight,
            hash_to_display(&v.kernel.hash_proof_of_stake)
        ));
    }

    // 7. block signature (main.cpp:3820-3847). The NETWORK gates this on
    //    IsBlockSignatureRequiredAtHeight; a dry run verifies the signature
    //    unconditionally — the assembled block must be fully valid.
    crate::consensus::check_pos_block_signature(block)
        .map_err(|e| format!("validate: block sig: {}", e))?;
    v.signature_ok = true;

    // 8. tx finality (CTransaction::IsFinal, main.h:573-586)
    for tx in &block.vtx {
        let lt = tx.locktime as i64;
        let ok = if lt == 0 {
            true
        } else if lt < 500_000_000 {
            lt < next_height as i64
        } else {
            lt < block.time as i64
        } || tx.vin.iter().all(|vin| vin.sequence == u32::MAX);
        if !ok {
            return Err("validate: non-final transaction".into());
        }
    }
    v.finality_ok = true;

    // 9. stake reward gate (ConnectBlock main.cpp:2421-2427, 2470-2498):
    //    nFees excludes the coinstake (a dry run carries no extra txs ⇒ 0);
    //    reject when nStakeReward > GetProofOfStakeReward(nCoinAge, nFees).
    let stake_tx = &block.vtx[1];
    let mut value_in = 0i64;
    let mut age_inputs: Vec<(i64, u32, u32)> = Vec::new();
    for vin in &stake_tx.vin {
        let meta = outputs
            .kernel_meta(&(vin.prev_hash, vin.prev_n))
            .ok_or_else(|| {
                format!(
                    "validate: stake input {}:{} unresolved for the reward gate",
                    hash_to_display(&vin.prev_hash),
                    vin.prev_n
                )
            })?;
        let block_time = idx.entries[meta.height].time;
        value_in += meta.value;
        age_inputs.push((meta.value, meta.tx_time, block_time));
    }
    let value_out: i64 = stake_tx.vout.iter().map(|o| o.value).sum();
    let n_stake_reward = value_out - value_in;
    let coin_age = coin_age_of(&age_inputs, stake_tx.time)?;
    let calculated = stake_reward(coin_age, 0);
    v.coin_age = coin_age;
    v.stake_reward_paid = n_stake_reward;
    v.stake_reward_calculated = calculated;
    v.reward_ok = n_stake_reward <= calculated;
    if !v.reward_ok {
        return Err(format!(
            "validate: stake reward too high (paid {} > calculated {})",
            n_stake_reward, calculated
        ));
    }

    // 10. block size (CheckBlock main.cpp:3290 / MAX_BLOCK_SIZE)
    let wire = serialize_block_for_wire(block).len();
    v.size_ok = wire <= crate::spend::MAX_BLOCK_SIZE;
    if !v.size_ok {
        return Err(format!("validate: block too large ({})", wire));
    }

    Ok(v)
}

/// Full wire serialization of a block (header + vtx + vchBlockSig), matching
/// the CBlock stream the daemon would relay. Local to the validator.
fn serialize_block_for_wire(b: &Block) -> Vec<u8> {
    fn push_compact_size(w: &mut Vec<u8>, n: u64) {
        if n < 0xfd {
            w.push(n as u8);
        } else if n <= 0xffff {
            w.push(0xfd);
            w.extend_from_slice(&(n as u16).to_le_bytes());
        } else if n <= 0xffff_ffff {
            w.push(0xfe);
            w.extend_from_slice(&(n as u32).to_le_bytes());
        } else {
            w.push(0xff);
            w.extend_from_slice(&n.to_le_bytes());
        }
    }
    let mut w = b.header_bytes();
    push_compact_size(&mut w, b.vtx.len() as u64);
    for tx in &b.vtx {
        w.extend_from_slice(&tx.serialize());
    }
    push_compact_size(&mut w, b.block_sig.len() as u64);
    w.extend_from_slice(&b.block_sig);
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::IndexEntry;

    // ---- synthetic chain ----------------------------------------------------
    fn synthetic_entry(height: usize, time: u32, generated: bool, modifier: u64) -> IndexEntry {
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
            bits: 0x207f_ffff,
            is_proof_of_stake: false,
            entropy_bit: (height & 1) as u8,
            stake_modifier: modifier,
            generated_stake_modifier: generated,
            hash_proof_of_stake: [0u8; 32],
        }
    }

    /// 40 blocks at 120 s spacing; generated modifiers every 5 blocks
    /// (h5→1, h10→2, …) so the fixed-600s walk from h1 stops at h10 (mod 2),
    /// mirroring kernel.rs's own synthetic-window test.
    fn synthetic_index() -> ReplayIndex<'static> {
        let mut idx = ReplayIndex::new();
        let t0 = 1_400_000_400u32;
        for h in 0..40usize {
            let (gen, m) = if h > 0 && h % 5 == 0 {
                (true, (h / 5) as u64)
            } else {
                (true, 0) // genesis
            };
            let gen = h == 0 || h % 5 == 0;
            idx.push(synthetic_entry(h, t0 + (h as u32) * 120, gen, m));
        }
        idx
    }

    fn synth_key(label: &str) -> ([u8; 32], Vec<u8>) {
        crate::spend::synthetic_key(label)
    }

    const EASY_BITS: u32 = 0x207f_ffff; // = TARGET_LIMIT_COMPACT
    const IMPOSSIBLE_BITS: u32 = 0x0100_0100; // target ≈ 2^8: nothing lands under it
    const BLOCK_TIME: u32 = 1_400_000_520; // time of synthetic h1

    fn make_coin(
        _label: &str,
        txid_byte: u8,
        value: i64,
        secret: [u8; 32],
        pubkey: &[u8],
    ) -> StakeCandidate {
        StakeCandidate {
            txid: [txid_byte; 32],
            n: 0,
            value,
            script_pubkey: crate::spend::script_pubkey_p2pk(pubkey).unwrap(),
            block_height: 1,
            block_time: BLOCK_TIME,
            tx_time: BLOCK_TIME,
            tx_offset: 181,
            secret,
            pubkey: pubkey.to_vec(),
        }
    }

    fn point_outputs_for(coins: &[StakeCandidate]) -> PointOutputs {
        let mut m = HashMap::new();
        for c in coins {
            m.insert(
                (c.txid, c.n),
                UtxoMeta {
                    height: c.block_height,
                    tx_offset: c.tx_offset,
                    tx_time: c.tx_time,
                    value: c.value,
                },
            );
        }
        PointOutputs(m)
    }

    fn median_of_last_11(idx: &ReplayIndex) -> u32 {
        let n = idx.entries.len();
        let mut times: Vec<u32> = idx.entries[n - 11..].iter().map(|e| e.time).collect();
        times.sort_unstable();
        times[5]
    }

    #[test]
    fn kernel_solve_loop_finds_solution_on_easy_target() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("staker");
        let coin = make_coin("staker", 0xAA, 50 * COIN, secret, &pubkey);
        let search_time = BLOCK_TIME + 12 * 3600;
        let (res, rep) = create_coinstake(
            &idx,
            &[coin.clone()],
            EASY_BITS,
            0,
            search_time,
            MAX_STAKE_SEARCH_INTERVAL,
            60 * COIN, // balance covers credit + reward
            41,
        )
        .expect("search");
        let plan = res.expect("easy target must solve");
        assert!(rep.attempts >= 1);
        assert_eq!(rep.kernel_hits, 1);
        assert_eq!(rep.coins_meeting_min_age, 1);
        assert!(
            tx_is_coin_stake(&plan.tx),
            "coinstake must satisfy IsCoinStake"
        );
        assert_eq!(plan.tx.time, search_time - plan.kernel.seconds_back as u32);
        // stake output pays credit (= value + reward) back to the staking key
        assert_eq!(plan.tx.vout[1].script_pubkey, coin.script_pubkey);
        assert!(plan.tx.vout[1].value > coin.value, "reward must be added");
        assert_eq!(plan.credit, coin.value + plan.reward);
        // the modifier must be the walk result from the input's block (h1 → h10 → 2)
        assert_eq!(plan.kernel.modifier, 2);
        // the input must be signed (scriptSig non-empty, push-encoded DER)
        assert!(!plan.tx.vin[0].script_sig.is_empty());
        // Full self-validation through the real validator path.
        let outputs = point_outputs_for(&[coin]);
        let mut block = assemble_pos_block(
            idx.tip().unwrap(),
            median_of_last_11(&idx),
            &plan.tx,
            &[],
            EASY_BITS,
        )
        .expect("assemble");
        sign_block(&mut block, &plan.signer.secret).expect("sign");
        let val = validate_dryrun_block(&idx, &outputs, &block, i64::MAX / 2).expect("validate");
        assert!(val.link_ok && val.structure_ok && val.merkle_ok && val.time_ok);
        assert!(val.n_bits_ok && val.kernel_meets_target && val.signature_ok);
        assert!(val.finality_ok && val.reward_ok && val.size_ok);
        assert_eq!(val.stake_reward_paid, plan.reward);
    }

    #[test]
    fn kernel_search_exhausts_on_impossible_target() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("exhaust");
        let coin = make_coin("exhaust", 0xBB, 50 * COIN, secret, &pubkey);
        let (plan, rep) = create_coinstake(
            &idx,
            &[coin],
            IMPOSSIBLE_BITS,
            0,
            BLOCK_TIME + 12 * 3600,
            MAX_STAKE_SEARCH_INTERVAL,
            60 * COIN,
            41,
        )
        .expect("search");
        assert!(plan.is_none(), "impossible target must not solve");
        assert!(rep.kernel_hits == 0);
        // all 60 backward seconds (n=0..59, wallet.cpp:2286 `n < min(...)`)
        // were actually probed on the one eligible coin
        assert_eq!(rep.coins_meeting_min_age, 1);
        assert_eq!(rep.attempts, MAX_STAKE_SEARCH_INTERVAL as u64);
    }

    #[test]
    fn min_age_filter_skips_young_coins() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("young");
        // containing block 10 s old: below the worst-case min-age filter
        let mut coin = make_coin("young", 0xCC, 50 * COIN, secret, &pubkey);
        coin.block_time = BLOCK_TIME + 12 * 3600 - 10;
        coin.tx_time = coin.block_time;
        let (_, rep) = create_coinstake(
            &idx,
            &[coin],
            EASY_BITS,
            0,
            BLOCK_TIME + 12 * 3600,
            MAX_STAKE_SEARCH_INTERVAL,
            60 * COIN,
            41,
        )
        .expect("search");
        assert_eq!(rep.coins_considered, 1);
        assert_eq!(rep.coins_meeting_min_age, 0, "young coin must be filtered");
        assert_eq!(rep.attempts, 0);
    }

    #[test]
    fn coinstake_structure_gates() {
        let (_secret, pubkey) = synth_key("struct");
        let script = crate::spend::script_pubkey_p2pk(&pubkey).unwrap();
        let good = Tx {
            version: 1,
            time: 1_400_004_000,
            vin: vec![Vin {
                prev_hash: [1u8; 32],
                prev_n: 0,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 0,
                    script_pubkey: vec![],
                },
                Vout {
                    value: 1000,
                    script_pubkey: script,
                },
            ],
            locktime: 0,
        };
        assert!(tx_is_coin_stake(&good));
        // marker value nonzero → not a coinstake (IsEmpty needs value==0)
        let bad = {
            let mut t = good.clone();
            t.vout[0].value = -1;
            t
        };
        assert!(!tx_is_coin_stake(&bad));
        // marker script non-empty → not a coinstake
        let bad2 = {
            let mut t = good.clone();
            t.vout[0].script_pubkey = vec![0xac];
            t
        };
        assert!(!tx_is_coin_stake(&bad2));
        // single output → not a coinstake
        let bad3 = {
            let mut t = good.clone();
            t.vout.truncate(1);
            t
        };
        assert!(!tx_is_coin_stake(&bad3));
        // null prevout (coinbase-shaped) → not a coinstake
        let bad4 = {
            let mut t = good.clone();
            t.vin[0].prev_hash = [0u8; 32];
            t.vin[0].prev_n = u32::MAX;
            t
        };
        assert!(!tx_is_coin_stake(&bad4));
    }

    #[test]
    fn stake_reward_matches_cbig_num_division_order() {
        // Independent U512 reference of the CBigNum evaluation:
        // bnSubsidy = nCoinAge; bnSubsidy *= 330000; /= 365; /= COIN.
        let reference = |coin_age: u64, fees: i64| -> i64 {
            let bn = U512::from(coin_age) * U512::from(MAX_TRI_PROOF_OF_STAKE as u64);
            let bn = bn / U512::from(365u64);
            let bn = bn / U512::from(COIN as u64);
            bn.as_u128() as i64 + fees
        };
        let mut cases: Vec<u64> = vec![
            0,
            1,
            364,
            365,
            366,
            1104,
            1105,
            86_400,
            1 << 20,
            (1 << 40) + 7,
            u32::MAX as u64,
            26_620_370, // a value produced by the coin-age tests
        ];
        for shift in [41u32, 48, 55, 60, 63] {
            cases.push((1u64 << shift) | 0xdead_beef);
        }
        for &age in &cases {
            if age as u128 * MAX_TRI_PROOF_OF_STAKE as u128 / 365 / COIN as u128 > i64::MAX as u128
            {
                continue; // outside the C++ getuint64 → int64 domain
            }
            for fees in [0i64, 1, 100, 123_456] {
                assert_eq!(stake_reward(age, fees), reference(age, fees), "age {}", age);
            }
        }
        // structural vector: zero coin age → fees only
        assert_eq!(stake_reward(0, 555), 555);
        // LIVE-CHAIN vectors (tipstate window, blocks 2202641/2202646/2202648):
        // (input coin age in sat-days, paid stake reward). Derived from real
        // coinstakes — nStakeReward = Σvout − Σvin of vtx[1], nFees = 0 — so
        // the constant and the CBigNum division order can never drift.
        for (coin_age, paid) in [
            (2_115_931u64, 1_913i64),
            (3_501_519, 3_165),
            (2_756_844, 2_492),
        ] {
            assert_eq!(
                stake_reward(coin_age, 0),
                paid,
                "live chain vector {}",
                coin_age
            );
        }
        // the C++ groups MULTIPLY before divide (nCoinAge *= 330000; /= 365;
        // /= COIN) — a divide-first implementation rounds differently. Find the
        // smallest coin age where the two groupings diverge and assert our
        // port follows the C++ grouping there (both computed, not assumed).
        let divide_first = |a: u64| -> i64 {
            ((a / 365) as i128 * MAX_TRI_PROOF_OF_STAKE as i128 / COIN as i128) as i64
        };
        let diverging = (1..)
            .find(|&a| {
                let cpp = a as i128 * MAX_TRI_PROOF_OF_STAKE as i128 / 365 / COIN as i128;
                cpp != divide_first(a) as i128
            })
            .expect("the two groupings must diverge somewhere");
        assert_eq!(
            stake_reward(diverging, 0) as i128,
            diverging as i128 * MAX_TRI_PROOF_OF_STAKE as i128 / 365 / COIN as i128,
            "must follow the C++ multiply-first CBigNum grouping"
        );
        assert_ne!(stake_reward(diverging, 0), divide_first(diverging));
    }

    #[test]
    fn coin_age_cent_second_semantics() {
        // main.cpp:3103-3132: per-input value*dt/CENT (floor), min-age inputs
        // skipped by CONTAINING BLOCK time, nTime < tx_time fails the call.
        let t0 = 1_700_000_000u32;
        let inputs = vec![
            (COIN, t0, t0),                 // dt = 3600 → counted
            (3 * COIN, t0 + 100, t0 + 100), // dt = 3500 → skipped by block-time guard
        ];
        let n_time = t0 + 3600;
        let age = coin_age_of(&inputs, n_time).expect("age");
        let expect_cent = (COIN as i128 * 3600) / crate::spend::CENT as i128;
        let expect = (expect_cent * crate::spend::CENT as i128 / 86_400) as u64;
        assert_eq!(age, expect);
        // timestamp violation fails the whole call
        let bad = vec![(COIN, n_time + 1, t0)];
        assert!(coin_age_of(&bad, n_time).is_err());
        // zero value contributes zero
        assert_eq!(coin_age_of(&[(0, t0, t0)], n_time).unwrap(), 0);
    }

    #[test]
    fn split_stake_rule_three_outputs_when_weight_young() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("splitter");
        let script = crate::spend::script_pubkey_p2pk(&pubkey).unwrap();
        // weight = tx_time - block_time - 3600 < nStakeSplitAge(10800) ⇒ 3 vouts
        let young = StakeCandidate {
            txid: [0x11u8; 32],
            n: 0,
            value: 500 * COIN,
            script_pubkey: script.clone(),
            block_height: 1,
            block_time: BLOCK_TIME,
            tx_time: BLOCK_TIME,
            tx_offset: 181,
            secret,
            pubkey,
        };
        // probe exactly at block_time + 3600 + 1000 → weight 1000 < 10800
        let tx_time = BLOCK_TIME + 3600 + 1000;
        let (res, _) = create_coinstake(&idx, &[young], EASY_BITS, 0, tx_time, 1, 501 * COIN, 41)
            .expect("search");
        let plan = res.expect("young coin must solve on the easy target");
        assert!(plan.split, "weight below nStakeSplitAge must split");
        assert_eq!(plan.tx.vout.len(), 3);
        // cent-rounded half + remainder must sum to the credit
        assert_eq!(plan.tx.vout[1].value + plan.tx.vout[2].value, plan.credit);
        assert_eq!(plan.tx.vout[1].value % crate::spend::CENT, 0);
        // old-coin case: weight far above the split age ⇒ 2 vouts
        let (secret, pubkey) = synth_key("oldster");
        let old = StakeCandidate {
            txid: [0x12u8; 32],
            n: 0,
            value: 500 * COIN,
            script_pubkey: crate::spend::script_pubkey_p2pk(&pubkey).unwrap(),
            block_height: 1,
            block_time: BLOCK_TIME,
            tx_time: BLOCK_TIME,
            tx_offset: 181,
            secret,
            pubkey,
        };
        let tx_time = BLOCK_TIME + 3600 + 20_000;
        let (res, _) = create_coinstake(&idx, &[old], EASY_BITS, 0, tx_time, 1, 501 * COIN, 41)
            .expect("search");
        let plan = res.expect("old coin must solve");
        assert!(!plan.split);
        assert_eq!(plan.tx.vout.len(), 2);
    }

    #[test]
    fn combine_pass_adds_same_key_inputs_under_threshold() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("combiner");
        let script = crate::spend::script_pubkey_p2pk(&pubkey).unwrap();
        let mk = |txid: u8, value: i64| StakeCandidate {
            txid: [txid; 32],
            n: 0,
            value,
            script_pubkey: script.clone(),
            block_height: 1,
            block_time: BLOCK_TIME,
            tx_time: BLOCK_TIME,
            tx_offset: 181,
            secret,
            pubkey: pubkey.clone(),
        };
        // kernel coin 12 TRI; 5 TRI combines; 25 TRI is "significant" and is
        // skipped; 3 TRI combines. Credit lands exactly on the 20 TRI
        // threshold (further adds would break there).
        let coins = vec![
            mk(0x21, 12 * COIN),
            mk(0x22, 5 * COIN),
            mk(0x23, 25 * COIN),
            mk(0x24, 3 * COIN),
        ];
        let balance: i64 = coins.iter().map(|c| c.value).sum();
        let tx_time = BLOCK_TIME + 12 * 3600;
        let (res, _) =
            create_coinstake(&idx, &coins, EASY_BITS, 0, tx_time, 1, balance, 41).expect("search");
        let plan = res.expect("easy target");
        assert_eq!(plan.inputs.len(), 3, "kernel + 5TRI + 3TRI; 25TRI skipped");
        assert_eq!(plan.inputs[0].txid, [0x21; 32]);
        assert_eq!(plan.inputs[1].txid, [0x22; 32]);
        assert_eq!(plan.inputs[2].txid, [0x24; 32]);
        assert_eq!(plan.credit, 20 * COIN + plan.reward);
        assert_eq!(plan.tx.vin.len(), plan.inputs.len());
        // every combined input must be signed
        assert!(plan.tx.vin.iter().all(|v| !v.script_sig.is_empty()));
        // and every signature must verify against its prevout script
        for (i, input) in plan.inputs.iter().enumerate() {
            crate::spend::verify_input(&plan.tx, i, &input.script_pubkey)
                .expect("coinstake input signature must verify");
        }
    }

    #[test]
    fn block_signature_roundtrip_and_tamper() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("signer");
        let coin = make_coin("signer", 0x31, 10 * COIN, secret, &pubkey);
        let tx_time = BLOCK_TIME + 3600 + 50_000;
        let (res, _) = create_coinstake(&idx, &[coin], EASY_BITS, 0, tx_time, 1, 11 * COIN, 41)
            .expect("search");
        let plan = res.expect("solve");
        let mut block = assemble_pos_block(
            idx.tip().unwrap(),
            median_of_last_11(&idx),
            &plan.tx,
            &[],
            EASY_BITS,
        )
        .expect("assemble");
        assert!(block.block_sig.is_empty());
        assert_eq!(block.time, plan.tx.time, "block time == coinstake time");
        assert_eq!(block.vtx.len(), 2);
        // vout[1] pays to the signer's bare P2PK script
        assert_eq!(
            block.vtx[1].vout[1].script_pubkey,
            plan.tx.vout[1].script_pubkey
        );
        sign_block(&mut block, &plan.signer.secret).expect("sign");
        assert!(!block.block_sig.is_empty());
        crate::consensus::check_pos_block_signature(&block).expect("sign/verify roundtrip");
        // tampered signature must fail
        let mut tampered = block.clone();
        let last = tampered.block_sig.len() - 1;
        tampered.block_sig[last] ^= 0x01;
        assert!(crate::consensus::check_pos_block_signature(&tampered).is_err());
        // tampered header must fail (hash changes, sig no longer matches)
        let mut tampered2 = block.clone();
        tampered2.nonce ^= 0x01;
        assert!(crate::consensus::check_pos_block_signature(&tampered2).is_err());
        // a different key's signature must fail
        let (s2, _) = synth_key("other");
        let mut wrong = block.clone();
        sign_block(&mut wrong, &s2).expect("sign with wrong key");
        assert!(crate::consensus::check_pos_block_signature(&wrong).is_err());
    }

    #[test]
    fn timestamp_protocol_rejects_stale_coinstake() {
        let idx = synthetic_index();
        let (secret, pubkey) = synth_key("stale");
        let coin = make_coin("stale", 0x41, 10 * COIN, secret, &pubkey);
        let (res, _) = create_coinstake(
            &idx,
            &[coin],
            EASY_BITS,
            0,
            BLOCK_TIME + 12 * 3600,
            1,
            11 * COIN,
            41,
        )
        .expect("search");
        let mut plan = res.expect("solve");
        plan.tx.time = 1_400_000_000; // below any floor of the synthetic chain
        let err = assemble_pos_block(
            idx.tip().unwrap(),
            median_of_last_11(&idx),
            &plan.tx,
            &[],
            EASY_BITS,
        )
        .unwrap_err();
        assert!(err.contains("timestamp floor"), "got: {}", err);
    }

    #[test]
    fn coinbase_height_prefix_minimal_push() {
        // CScript << int64 pushes OP_N for 1..16, else minimal signed LE.
        let mut w = Vec::new();
        push_script_int64(&mut w, 5);
        assert_eq!(w, vec![0x55]); // OP_5
        let mut w = Vec::new();
        push_script_int64(&mut w, 0);
        assert_eq!(w, vec![0x00]); // OP_0
                                   // minimal signed LE derived independently: u32 LE bytes with trailing
                                   // (high-order) zero bytes trimmed, sign byte appended when the top
                                   // bit of the last byte is set, then the push frame.
        let expect_min = |v: u32| -> Vec<u8> {
            let le = v.to_le_bytes();
            let len = le.iter().rposition(|&b| b != 0).map(|p| p + 1).unwrap_or(1);
            let mut data = le[..len].to_vec();
            if *data.last().unwrap() & 0x80 != 0 {
                data.push(0x00);
            }
            let mut out = vec![data.len() as u8];
            out.extend_from_slice(&data);
            out
        };
        let mut w = Vec::new();
        push_script_int64(&mut w, 2_204_065);
        assert_eq!(w, expect_min(2_204_065), "height prefix (miner.cpp:106)");
        let mut w = Vec::new();
        push_script_int64(&mut w, 0x0080_0000); // top bit of the last byte set → sign byte
        assert_eq!(w, expect_min(0x0080_0000));
        assert_eq!(w.last(), Some(&0x00), "sign byte terminates the encoding");
    }
}
