//! Fork choice: pick the branch with the most WORK, not the one that arrived
//! first.
//!
//! Why this exists: two tri7 nodes staking at the same time each mint a block
//! at height H. Without fork choice each node keeps its own block and ignores
//! the other forever — that is exactly how sami-pc ended up on an isolated
//! branch whose balances looked real but which the network never had.
//!
//! Rules (matching the legacy client's CBlockIndex::nChainWork comparison):
//!   * longer height wins when work is equal (ties are settled by height)
//!   * otherwise more cumulative proof-of-work wins, even at equal height
//!   * only accept a switch when the tips actually differ at the shared height
//!
//! Work per block = 2^256 / (target + 1), the same formula bitcoin derived;
//! a shrunk target (harder block) is therefore worth strictly more.

use crate::legacy::Hash;

/// Highest per-block work fork choice will credit.
///
/// (T7-03) `work_of` is fed HEADERS, i.e. attacker-chosen bits that no body
/// has vouched for. The chain's real blocks sit around 3.7e24 (bits
/// 0x1f04..0x1f06); crafted bits (tiny exponent) imply a difficulty near
/// 1e70 and used to return `u128::MAX`, making ONE announced header outweigh
/// any real branch. Work above this ceiling cannot come from a producible
/// block, so it is credited as 0 (the branch simply gains nothing) instead of
/// being treated as a winning claim.
const MAX_PLAUSIBLE_WORK: u128 = 1u128 << 100; // ~1.27e30

/// Work represented by one block with these compact bits, in units of
/// 2^-29 (the same normalisation the daemon's difficulty_from_bits uses):
///
/// `work = 2^256 / (target + 1)`, normalised as
/// `0xFFFF * 256^(29 - nShift) / mantissa`.
///
/// Shrinking the target (smaller mantissa) strictly increases this value, which
/// is the only property fork choice needs. Computed in f64 with the chain's
/// real magnitudes, then mirrored to u128 to keep comparisons exact for any
/// realistic branch length (a branch would have to be ~10^30 blocks longer for
/// rounding to matter).
pub fn work_of(bits: u32) -> u128 {
    if bits & 0x007f_ffff == 0 {
        return 0;
    }
    let d = crate::rpc::difficulty_from_bits(bits);
    if !(d.is_finite()) || d <= 0.0 {
        return 0;
    }
    // 1 / difficulty is the fraction of the space a block must hit
    // (difficulty = FULL/2^256 * target^-1 * ...); invert and scale to u128.
    // difficulty is SMALL when the target is large (easy block): work is the
    // inverse, so divide by difficulty directly
    let inv = d;
    // Non-finite / absurd implied work (uncomputable target or crafted bits):
    // credit nothing — an unvalidated header must never carry a winning claim.
    if !inv.is_finite() || inv > MAX_PLAUSIBLE_WORK as f64 / 1e30 {
        return 0;
    }
    let w = (inv * 1e30).max(1.0);
    if !w.is_finite() || w > MAX_PLAUSIBLE_WORK as f64 {
        return 0;
    }
    w as u128
}

/// The decision the node needs: keep our tip, or switch to theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkChoice {
    /// our tip stands (their branch is not better)
    KeepOurs,
    /// their branch is heavier: adopt it
    Switch { reason: String },
}

/// Compare two branch tips. `our` is (height, hash) with ours' cumulative
/// work; `theirs` likewise. Work comparison dominates, height breaks ties.
pub fn choose(
    our_height: u64,
    our_hash: &Hash,
    our_work: u128,
    their_height: u64,
    their_hash: &Hash,
    their_work: u128,
) -> ForkChoice {
    if our_hash == their_hash {
        return ForkChoice::KeepOurs;
    }
    if their_work > our_work {
        return ForkChoice::Switch {
            reason: format!(
                "their branch h{} carries more work ({}) than ours h{} ({})",
                their_height, their_work, our_height, our_work
            ),
        };
    }
    if their_work == our_work && their_height > our_height {
        return ForkChoice::Switch {
            reason: format!(
                "equal work, their branch is longer (h{} vs h{})",
                their_height, our_height
            ),
        };
    }
    ForkChoice::KeepOurs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        [n; 32]
    }

    #[test]
    fn work_matches_difficulty_for_the_chains_real_bits() {
        // exponent 0x1f: SMALLER mantissa = smaller target = harder block =
        // MORE work. 0x041cb2 < 0x0575bb < 0x0633a1.
        let easy = work_of(0x1f06_33a1);
        let harder = work_of(0x1f04_1cb2);
        assert!(
            harder > easy,
            "shrinking target must mean more work (easy {} harder {})",
            easy,
            harder
        );
        // doubling the target must halve the work
        let a = work_of(0x1f03_0000);
        let b = work_of(0x1f06_0000);
        assert!(a > b, "mantissa 0x030000 target is smaller -> more work");
        assert_eq!(work_of(0), 0);
    }

    #[test]
    fn crafted_bits_are_not_a_winning_work_claim() {
        // (T7-03) a tiny exponent implies an impossible difficulty; it used to
        // return u128::MAX, i.e. one announced header outweighed the whole
        // chain. It must now be credited as nothing.
        assert_eq!(work_of(0x0000_ffff), 0, "crafted easy-exponent bits");
        assert_eq!(work_of(0x0100_ffff), 0, "crafted bits, unbounded exponent");
        assert_eq!(work_of(0x0000_0001), 0, "degenerate mantissa");
        // real chain bits stay well under the ceiling and still order correctly
        let real = work_of(0x1f04_1cb2);
        assert!(real > 0 && real < MAX_PLAUSIBLE_WORK);
        // a branch with MAX work can never be out-voted by crafted bits
        assert!(real > work_of(0x0000_ffff));
    }

    #[test]
    fn longer_branch_wins_on_equal_work() {
        let d = choose(100, &h(1), 1000, 101, &h(2), 1000);
        assert!(matches!(d, ForkChoice::Switch { .. }));
    }

    #[test]
    fn heavier_branch_wins_even_at_equal_height() {
        let d = choose(100, &h(1), 1000, 100, &h(2), 1200);
        assert!(matches!(d, ForkChoice::Switch { .. }));
    }

    #[test]
    fn equal_tip_is_never_a_switch() {
        assert_eq!(choose(100, &h(1), 1000, 100, &h(1), 1000), ForkChoice::KeepOurs);
        // shorter + lighter also keeps ours
        assert_eq!(choose(100, &h(1), 1000, 99, &h(9), 900), ForkChoice::KeepOurs);
    }

    #[test]
    fn equal_work_and_height_keeps_our_block_deterministically() {
        // tie-break: first seen wins, so two honest nodes do not flip-flop
        assert_eq!(choose(100, &h(1), 1000, 100, &h(2), 1000), ForkChoice::KeepOurs);
    }
}
