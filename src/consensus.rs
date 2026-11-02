//! Consensus validation for the Triangles chain, ported 1:1 from the live
//! v6.2.6.7 C++ (see docs/pos-consensus-spec.md for file:line citations of
//! every rule). Constants and semantics MUST NOT be "improved" — byte-exact
//! behavior is the requirement.

use crate::legacy::{sha256d, Block, Hash, Tx};

// ---------------------------------------------------------------------------
// Constants (pos-consensus-spec.md §0)
// ---------------------------------------------------------------------------
pub const CUTOFF_POW_BLOCK: usize = 9000;
pub const CRAPCHAIN_CUTOFF_BLOCK: usize = 17691;
pub const FORK_HEIGHT_V5: usize = 17651;
pub const FORK_HEIGHT_V5_4: usize = 2_186_941;
/// First block minted by the v5.0.0.0 revival client (chain resumed
/// 2026-03-11 after the Dec-2022 freeze; the last pre-revival block 2186941 is
/// the nBits seam exempt).
pub const V5_REVIVAL_HEIGHT: usize = 2_186_942;
/// Last block of the v5-era stretch: the chain stalled 2026-04-07..2026-09-06;
/// h2200901+ was minted by the reverted (capped-GetWeight) v6.2.6.5+ client.
pub const V6_RESTART_HEIGHT: usize = 2_200_900;
pub const MODIFIER_INTERVAL_SWITCH: usize = 1;
pub const COINBASE_HEIGHT_ENFORCEMENT: usize = 2_300_000;
pub const TARGET_SPACING: u32 = 120;
pub const TARGET_TIMESPAN: u32 = 1800;
pub const STAKE_MIN_AGE: u32 = 3600;
pub const STAKE_MAX_AGE: u32 = 43_200;
pub const MODIFIER_INTERVAL: u32 = 300;
pub const MAX_TRI_PROOF_OF_STAKE: i64 = 330_000; // 0.33 * COIN (main.h:62 — chain-verified: 48/48 live coinstake rewards match 330000/365/COIN per coin-year)
pub const COIN: i64 = 1_000_000;
/// Last hardcoded mainnet checkpoint height (checkpoints.cpp); assume-valid
/// boundary and block-signature requirement boundary.
pub const LAST_HARDCODED_CHECKPOINT: usize = 9001;

// ---------------------------------------------------------------------------
// Classification (spec §1)
// ---------------------------------------------------------------------------
pub fn tx_is_coin_base(tx: &Tx) -> bool {
    tx.vin.len() == 1
        && tx.vin[0].prev_hash == [0u8; 32]
        && tx.vin[0].prev_n == u32::MAX
        && !tx.vout.is_empty()
}

pub fn tx_is_coin_stake(tx: &Tx) -> bool {
    !tx.vin.is_empty()
        && !(tx.vin[0].prev_hash == [0u8; 32] && tx.vin[0].prev_n == u32::MAX)
        && tx.vout.len() >= 2
        // CTxOut::IsEmpty() (main.h:428): value ZERO + empty script — NOT the
        // -1 sentinel (that's CTxOut::IsNull(), used for deser init only).
        && tx.vout[0].value == 0
        && tx.vout[0].script_pubkey.is_empty()
}

pub fn block_is_proof_of_stake(b: &Block) -> bool {
    b.vtx.len() > 1 && tx_is_coin_stake(&b.vtx[1])
}

// ---------------------------------------------------------------------------
// Time rules (spec §7)
// ---------------------------------------------------------------------------
pub fn max_time_drift(height: usize) -> i64 {
    if height >= FORK_HEIGHT_V5_4 {
        90
    } else {
        600
    }
}

// ---------------------------------------------------------------------------
// Kernel hash (spec §3.2): double-SHA256 over the 6-field SER_GETHASH stream
// ---------------------------------------------------------------------------
pub fn stake_kernel_hash(
    stake_modifier: u64,
    time_block_from: u32,
    tx_prev_offset: u32,
    tx_prev_time: u32,
    prevout_n: u32,
    time_tx: u32,
) -> Hash {
    let mut s = Vec::with_capacity(28);
    s.extend_from_slice(&stake_modifier.to_le_bytes());
    s.extend_from_slice(&time_block_from.to_le_bytes());
    s.extend_from_slice(&tx_prev_offset.to_le_bytes());
    s.extend_from_slice(&tx_prev_time.to_le_bytes());
    s.extend_from_slice(&prevout_n.to_le_bytes());
    s.extend_from_slice(&time_tx.to_le_bytes());
    sha256d(&s)
}

/// GetWeight (kernel.cpp:25-41), era-aware per the mint-time client. `height`
/// is the height of the block BEING VALIDATED (the replay position):
///   - h < 2186942 (Pharao / Black-Pharao lineage, Oct 2014..Dec 2022): the
///     kernel.cpp of that era had no fork branch — Peercoin hard cap at
///     nStakeMaxAge (12 h).
///   - 2186942..2200900 (v5.0.0.0 revival 2026-03-11 .. v6 2026-09-06
///     restart): "After v5 fork: remove max age cap so coins aged during the
///     freeze can stake" (kernel.cpp:37-39 at c636edb) — weight UNCAPPED (the
///     validating tip is always >= FORK_HEIGHT_V5 here).
///   - h >= 2200901 (v6.2.6.5+, post-revert 0411be6 2026-08-03, chain resumed
///     2026-09-06): the revert restored the hard cap, forward-only.
pub fn stake_weight_era(time_prev_tx: u32, time_tx: u32, height: usize) -> i64 {
    let delta = time_tx as i64 - time_prev_tx as i64 - STAKE_MIN_AGE as i64;
    if delta < 0 {
        return 0;
    }
    if height >= V5_REVIVAL_HEIGHT && height <= V6_RESTART_HEIGHT {
        delta
    } else {
        delta.min(STAKE_MAX_AGE as i64)
    }
}

/// GetWeight (kernel.cpp:25-41) keyed on the validating tip (pindexBest), the
/// v5.0.0.0 form. Kept for callers that model the live tip explicitly.
pub fn stake_weight(time_prev_tx: u32, time_tx: u32, tip_height: usize) -> i64 {
    let delta = time_tx as i64 - time_prev_tx as i64 - STAKE_MIN_AGE as i64;
    if delta < 0 {
        return 0;
    }
    if tip_height >= FORK_HEIGHT_V5 {
        delta
    } else {
        delta.min(STAKE_MAX_AGE as i64)
    }
}

/// Compact-format difficulty target (bitcoin nBits) → 32-byte big-endian
/// target. value = mantissa * 256^(size-3); oversized targets saturate to all
/// 0xff (caller treats as "target above range").
pub fn compact_to_target(bits: u32) -> [u8; 32] {
    let size = (bits >> 24) as isize;
    let negative = bits & 0x0080_0000 != 0;
    let mut mantissa = (bits & 0x007f_ffff) as u32;
    // Negative or degenerate encodings never appear on a valid chain; a zero
    // target rejects everything, which is the safe interpretation.
    if negative || size <= 0 || mantissa == 0 {
        return [0u8; 32];
    }
    if size > 34 {
        return [0xffu8; 32];
    }
    // Mantissa bytes, big-endian, minimal (1..=3 bytes for 0x007fffff).
    let mut m = [0u8; 4];
    let mut mi = 4;
    while mantissa > 0 {
        mi -= 1;
        m[mi] = (mantissa & 0xff) as u8;
        mantissa >>= 8;
    }
    let mant_len = 4 - mi;
    // value = mantissa * 256^(size-3) → byte length = size - 3 + mant_len.
    // size < 3 means the value is fractional (< 1) → rounds to a zero target.
    let total = (size - 3) + mant_len as isize;
    if total <= 0 {
        return [0u8; 32];
    }
    if total > 32 {
        return [0xffu8; 32];
    }
    let mut target = [0u8; 32];
    let start = 32 - total as usize;
    if total >= mant_len as isize {
        target[start..start + mant_len].copy_from_slice(&m[mi..]);
    } else {
        // Total span smaller than the mantissa: keep the top `total` bytes.
        let keep = total as usize;
        target[32 - keep..].copy_from_slice(&m[mi..mi + keep]);
    }
    target
}

// ---------------------------------------------------------------------------
// Block signature (spec §6, main.cpp:3820-3847)
// ---------------------------------------------------------------------------
/// Bare-pubkey script check (Solver == TxnOutType::PubKey): the script must be
/// exactly OP_PUSH <pubkey> OP_CHECKSIG.
pub fn bare_pubkey_from_script(script: &[u8]) -> Option<&[u8]> {
    if script.len() < 2 {
        return None;
    }
    let push = script[0] as usize;
    if script.len() != push + 2 || script[script.len() - 1] != 0xac {
        return None;
    }
    let pk = &script[1..1 + push];
    if pk.len() == 33 && (pk[0] == 2 || pk[0] == 3) || pk.len() == 65 && pk[0] == 4 {
        Some(pk)
    } else {
        None
    }
}

/// CBlock::CheckBlockSignature for a PoS block: ECDSA/DER verify of vchBlockSig
/// over the Hash9 header hash against vtx[1].vout[1]'s bare pubkey.
pub fn check_pos_block_signature(block: &Block) -> Result<(), String> {
    if block.vtx.len() < 2 || block.vtx[1].vout.len() < 2 {
        return Err("coinstake missing vout[1]".into());
    }
    let script = &block.vtx[1].vout[1].script_pubkey;
    let pubkey = bare_pubkey_from_script(script)
        .ok_or_else(|| "coinstake vout[1] is not a bare-pubkey script".to_string())?;
    if block.block_sig.is_empty() {
        return Err("empty block signature".into());
    }
    let sig = secp256k1::ecdsa::Signature::from_der(&block.block_sig)
        .map_err(|e| format!("bad DER signature: {}", e))?;
    let pk = secp256k1::PublicKey::from_slice(pubkey).map_err(|e| format!("bad pubkey: {}", e))?;
    let msg = secp256k1::Message::from_digest(block.hash());
    let secp = secp256k1::Secp256k1::verification_only();
    secp.verify_ecdsa(&msg, &sig, &pk)
        .map_err(|e| e.to_string())
}

/// PoW blocks must carry an EMPTY signature (spec §6).
pub fn check_pow_block_signature(block: &Block) -> Result<(), String> {
    if !block.block_sig.is_empty() {
        return Err("PoW block carries a non-empty signature".into());
    }
    Ok(())
}

/// IsBlockSignatureRequiredAtHeight (main.cpp:1812-1815).
pub fn block_signature_required(height: usize) -> bool {
    height > LAST_HARDCODED_CHECKPOINT
}

/// IsConsensusAssumeValidHeight (main.cpp:1806-1810): kernel/script/reward
/// checks skipped below this; hashProofOfStake recorded as 0.
pub fn assume_valid(height: usize, tip_height: usize) -> bool {
    height <= LAST_HARDCODED_CHECKPOINT || height + 100 <= tip_height
}
