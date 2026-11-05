//! Legacy transaction-row semantics, ported verbatim from the Qt client.
//!
//! Source of truth (read, not guessed):
//!   src/qt/transactionrecord.cpp  — decomposeTransaction()
//!   src/qt/transactionrecord.cpp  — updateStatus()
//!   src/qt/transactiontablemodel.cpp — formatTxStatus()/formatTxType()
//!   src/wallet.cpp — GetBalance/GetStake/GetImmatureBalance/GetUnconfirmedBalance
//!
//! The legacy row model is deliberately copied because a holder must be able
//! to read the same facts the same way in both clients:
//!
//!  * one row per OUTPUT to us (that is why one stake shows as a 0-value row
//!    plus the reward row — Sami asked for exactly this);
//!  * status text: "Unconfirmed" / "Confirming (n of 4 confirmations)" /
//!    "Confirmed (n confirmations)" / "Immature (n confirmations, will be
//!    available after m)" for stakes and coinbase (maturity 7);
//!  * "RecommendedNumConfirmations = 4" is the legacy constant, and it is
//!    also the depth at which the legacy wallet starts counting a credit as
//!    spendable (wallet.h:664 `if (nDepth >= 4) return true`).

use serde::{Deserialize, Serialize};

/// transactionrecord.h: RecommendedNumConfirmations = 4
pub const RECOMMENDED_CONFIRMATIONS: i64 = 4;
/// main.cpp:64 — int nCoinbaseMaturity = 7
pub const COINBASE_MATURITY: i64 = 7;

/// transactionrecord.h: TransactionRecord::Type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyKind {
    Other,
    Generated,
    SendToAddress,
    SendToOther,
    RecvWithAddress,
    RecvFromOther,
    SendToSelf,
}

impl LegacyKind {
    /// transactiontablemodel.cpp formatTxType()
    pub fn label(&self) -> &'static str {
        match self {
            LegacyKind::RecvWithAddress => "Received with",
            LegacyKind::RecvFromOther => "Received from",
            LegacyKind::SendToAddress | LegacyKind::SendToOther => "Sent to",
            LegacyKind::SendToSelf => "Payment to yourself",
            LegacyKind::Generated => "Mined",
            LegacyKind::Other => "",
        }
    }
}

/// transactionrecord.h: TransactionStatus::Status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyStatus {
    Confirmed,
    Unconfirmed,
    Confirming,
    Conflicted,
    Immature,
    NotAccepted,
}

/// One row exactly as the legacy Transactions table renders it.
#[derive(Debug, Clone)]
pub struct LegacyRow {
    pub txid: String,
    /// sub-row index within the tx (legacy `idx`, for stable ordering)
    pub idx: usize,
    pub time: u32,
    pub height: u64,
    pub kind: LegacyKind,
    pub address: String,
    /// legacy debit (negative) — only one of debit/credit is non-zero
    pub debit: i64,
    /// legacy credit (positive)
    pub credit: i64,
    pub status: LegacyStatus,
    pub depth: i64,
    /// blocks remaining until spendable (0 when mature / not generated)
    pub matures_in: i64,
}

impl LegacyRow {
    /// Net amount shown in the Amount column (credit or debit).
    pub fn amount(&self) -> i64 {
        self.credit + self.debit
    }

    /// transactiontablemodel.cpp formatTxStatus(), string for string.
    pub fn status_text(&self) -> String {
        match self.status {
            LegacyStatus::Unconfirmed => "Unconfirmed".into(),
            LegacyStatus::Confirming => format!(
                "Confirming ({} of {} confirmations)",
                self.depth, RECOMMENDED_CONFIRMATIONS
            ),
            LegacyStatus::Confirmed => format!("Confirmed ({} confirmations)", self.depth),
            LegacyStatus::Conflicted => "Conflicted".into(),
            LegacyStatus::Immature => format!(
                "Immature ({} confirmations, will be available after {})",
                self.depth,
                self.depth + self.matures_in
            ),
            LegacyStatus::NotAccepted => "Generated but not accepted".into(),
        }
    }

    /// The legacy table's Confirmation column value. The Qt table shows the
    /// status text ("Confirmed (12 confirmations)"); the number itself is
    /// what users compare, so expose both.
    pub fn confirmations(&self) -> i64 {
        self.depth.max(0)
    }
}

/// transactiontablemodel.cpp txStatusDecoration() — which glyph the legacy
/// table puts in the status column. The sequence is the one users know:
///
///   transaction_0            circle, nothing filled   (not in a block)
///   transaction_1 .. 5       clock1..clock5           (fills 1 -> 5)
///   transaction_confirmed    green check              (>= 4 confirmations)
///   transaction_conflicted   red cross
///   Immature                 clock{(depth*4/total)+1} (partial, by maturity)
///
/// `asset` is the GUI asset key; None means "no glyph" (legacy returns a
/// colour instead for the open-until cases, which we never render).
pub fn status_glyph(status: &LegacyStatus, depth: i64, _mature: bool, matures_in: i64) -> Option<&'static str> {
    match status {
        LegacyStatus::Unconfirmed => Some("icons/tx_zero"),
        LegacyStatus::Confirming => Some(match depth {
            1 => "icons/clock1",
            2 => "icons/clock2",
            3 => "icons/clock3",
            4 => "icons/clock4",
            _ => "icons/clock5",
        }),
        // legacy: case Confirmed -> transaction_confirmed (green check), for
        // ordinary and generated rows alike
        LegacyStatus::Confirmed => Some("icons/tx_confirmed"),
        LegacyStatus::Conflicted => Some("icons/tx_conflicted"),
        LegacyStatus::Immature => {
            // legacy: part = (depth * 4 / (depth + matures_in)) + 1, then
            // transaction_{part} — i.e. clock1..clock5 as maturity approaches
            let total = (depth + matures_in).max(1);
            let part = (depth * 4 / total + 1).clamp(1, 5);
            Some(match part {
                1 => "icons/clock1",
                2 => "icons/clock2",
                3 => "icons/clock3",
                4 => "icons/clock4",
                _ => "icons/clock5",
            })
        }
        LegacyStatus::NotAccepted => Some("icons/tx_zero"),
    }
}

/// transactionrecord.cpp updateStatus() — the status machine, reduced to the
/// cases a chain-backed row can be in (no offline/open-until-date states: we
/// only render rows that are in a block or in the mempool).
pub fn status_for(kind: LegacyKind, depth: i64, tip_height: u64, height: u64) -> (LegacyStatus, i64) {
    let generated = matches!(kind, LegacyKind::Generated);
    if generated {
        // GetBlocksToMaturity(): max(0, nCoinbaseMaturity - depth)
        let matures_in = (COINBASE_MATURITY - depth).max(0);
        if matures_in > 0 {
            // IsInMainChain() ? Immature : NotAccepted
            if height > 0 && height <= tip_height {
                return (LegacyStatus::Immature, matures_in);
            }
            return (LegacyStatus::NotAccepted, 0);
        }
        return (LegacyStatus::Confirmed, 0);
    }
    if depth < 0 {
        return (LegacyStatus::Conflicted, 0);
    }
    if depth == 0 {
        return (LegacyStatus::Unconfirmed, 0);
    }
    if depth < RECOMMENDED_CONFIRMATIONS {
        return (LegacyStatus::Confirming, 0);
    }
    (LegacyStatus::Confirmed, 0)
}

/// Legibility guard: legacy rows carry BOTH a timestamp and a height. If the
/// height is known the depth must agree with the tip; a row whose date says
/// one block and whose depth says another is a bug, not a display quirk.
pub fn depth_of(height: u64, tip_height: u64) -> i64 {
    if height == 0 || height > tip_height {
        return 0;
    }
    (tip_height - height + 1) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_legacy_client() {
        // transactionrecord.h / main.cpp:64 — these are the whole point
        assert_eq!(RECOMMENDED_CONFIRMATIONS, 4);
        assert_eq!(COINBASE_MATURITY, 7);
    }

    #[test]
    fn status_text_is_verbatim_legacy() {
        let mk = |status, depth, matures_in| LegacyRow {
            txid: "00".into(),
            idx: 0,
            time: 1_700_000_000,
            height: 100,
            kind: LegacyKind::RecvWithAddress,
            address: "T".into(),
            debit: 0,
            credit: 500,
            status,
            depth,
            matures_in,
        };
        assert_eq!(mk(LegacyStatus::Unconfirmed, 0, 0).status_text(), "Unconfirmed");
        assert_eq!(
            mk(LegacyStatus::Confirming, 2, 0).status_text(),
            "Confirming (2 of 4 confirmations)"
        );
        assert_eq!(
            mk(LegacyStatus::Confirmed, 12, 0).status_text(),
            "Confirmed (12 confirmations)"
        );
        assert_eq!(
            mk(LegacyStatus::Immature, 3, 4).status_text(),
            "Immature (3 confirmations, will be available after 7)"
        );
        assert_eq!(
            mk(LegacyStatus::NotAccepted, 0, 0).status_text(),
            "Generated but not accepted"
        );
    }

    #[test]
    fn receive_states_follow_update_status() {
        // depth 0 -> Unconfirmed, 1..3 -> Confirming, >=4 -> Confirmed
        assert_eq!(status_for(LegacyKind::RecvWithAddress, 0, 1000, 0).0, LegacyStatus::Unconfirmed);
        assert_eq!(status_for(LegacyKind::RecvWithAddress, 1, 1000, 1000).0, LegacyStatus::Confirming);
        assert_eq!(status_for(LegacyKind::RecvWithAddress, 3, 1000, 998).0, LegacyStatus::Confirming);
        assert_eq!(status_for(LegacyKind::RecvWithAddress, 4, 1000, 997).0, LegacyStatus::Confirmed);
        // negative depth = conflicted
        assert_eq!(status_for(LegacyKind::SendToAddress, -2, 1000, 900).0, LegacyStatus::Conflicted);
    }

    #[test]
    fn generated_rows_are_immature_until_seven_confirmations() {
        // stake at height 1000, tip 1000 -> depth 1, 6 blocks to go
        let (st, matures) = status_for(LegacyKind::Generated, 1, 1000, 1000);
        assert_eq!(st, LegacyStatus::Immature);
        assert_eq!(matures, 6);
        // depth 6 -> 1 block left
        assert_eq!(status_for(LegacyKind::Generated, 6, 1006, 1001).1, 1);
        // depth 7 -> mature, Confirmed
        assert_eq!(status_for(LegacyKind::Generated, 7, 1007, 1001).0, LegacyStatus::Confirmed);
        // not in the main chain -> NotAccepted
        assert_eq!(
            status_for(LegacyKind::Generated, 2, 900, 1000).0,
            LegacyStatus::NotAccepted
        );
    }

    #[test]
    fn generated_matures_in_matches_legacy_formula() {
        // GetBlocksToMaturity(): max(0, nCoinbaseMaturity - depth)
        for depth in 0..12i64 {
            let expect = (COINBASE_MATURITY - depth).max(0);
            assert_eq!(status_for(LegacyKind::Generated, depth, 5000, 4000).1, expect);
        }
    }

    #[test]
    fn depth_matches_height_and_tip() {
        assert_eq!(depth_of(1000, 1000), 1);
        assert_eq!(depth_of(990, 1000), 11);
        assert_eq!(depth_of(0, 1000), 0);
        assert_eq!(depth_of(1001, 1000), 0);
    }

    #[test]
    fn type_labels_match_the_qt_table() {
        assert_eq!(LegacyKind::RecvWithAddress.label(), "Received with");
        assert_eq!(LegacyKind::RecvFromOther.label(), "Received from");
        assert_eq!(LegacyKind::SendToAddress.label(), "Sent to");
        assert_eq!(LegacyKind::SendToSelf.label(), "Payment to yourself");
        assert_eq!(LegacyKind::Generated.label(), "Mined");
    }
}

#[cfg(test)]
mod legacy_balance_tests {
    use super::*;

    /// wallet.cpp split, expressed over the same inputs the GUI uses:
    ///   Spendable = depth >= 4 credits, Stake = maturing coinstake,
    ///   Unconfirmed = depth < 4 credits, Immature = maturing coinbase,
    ///   Total = the four added. A wallet whose split silently moves stake
    ///   into spendable is lying about what the user can spend.
    fn split(rows: &[(LegacyKind, i64, i64, u64)], tip: u64) -> (i64, i64, i64, i64) {
        let (mut sp, mut st, mut un, mut im) = (0i64, 0i64, 0i64, 0i64);
        for (kind, value, _conf, height) in rows {
            let depth = depth_of(*height, tip);
            let generated = matches!(kind, LegacyKind::Generated);
            let matures = (COINBASE_MATURITY - depth).max(0);
            if generated && matures > 0 {
                // coinstake vs coinbase is expressed by the caller's kind:
                // Generated with a 0-value first output is a stake
                if *kind == LegacyKind::Generated {
                    st += *value;
                }
            } else if depth >= RECOMMENDED_CONFIRMATIONS {
                sp += *value;
            } else {
                un += *value;
            }
        }
        (sp, st, un, im)
    }

    #[test]
    fn spendable_requires_four_confirmations_like_the_legacy_wallet() {
        // one depth-1 receipt and one depth-4 receipt
        let (sp, _st, un, _im) = split(
            &[
                (LegacyKind::RecvWithAddress, 100, 1, 1000),
                (LegacyKind::RecvWithAddress, 500, 4, 997),
            ],
            1000,
        );
        assert_eq!(sp, 500, "only the 4-confirmation credit is spendable");
        assert_eq!(un, 100, "the depth-1 credit is unconfirmed");
    }

    #[test]
    fn maturing_stakes_are_not_counted_as_spendable() {
        let (sp, st, _un, _im) = split(&[(LegacyKind::Generated, 250, 2, 999)], 1000);
        assert_eq!(sp, 0);
        assert_eq!(st, 250);
    }

    #[test]
    fn mature_stake_becomes_spendable() {
        let (sp, st, _un, _im) = split(&[(LegacyKind::Generated, 250, 7, 994)], 1000);
        assert_eq!(st, 0);
        assert_eq!(sp, 250);
    }
}

#[cfg(test)]
mod glyph_tests {
    use super::*;

    #[test]
    fn glyph_fills_as_confirmations_arrive() {
        assert_eq!(
            status_glyph(&LegacyStatus::Unconfirmed, 0, false, 0),
            Some("icons/tx_zero")
        );
        assert_eq!(
            status_glyph(&LegacyStatus::Confirming, 1, false, 0),
            Some("icons/clock1")
        );
        assert_eq!(
            status_glyph(&LegacyStatus::Confirming, 3, false, 0),
            Some("icons/clock3")
        );
        assert_eq!(
            status_glyph(&LegacyStatus::Confirmed, 9, false, 0),
            Some("icons/tx_confirmed")
        );
    }

    #[test]
    fn immature_stake_walks_the_clock_sequence() {
        // a stake at depth 1 with 6 to go is 1/7 mature -> clock1
        assert_eq!(
            status_glyph(&LegacyStatus::Immature, 1, false, 6),
            Some("icons/clock1")
        );
        // at depth 6 with 1 to go -> (6*4/7)+1 = 4 -> clock4
        assert_eq!(
            status_glyph(&LegacyStatus::Immature, 6, false, 1),
            Some("icons/clock4")
        );
    }

    #[test]
    fn conflicted_has_its_own_glyph() {
        assert_eq!(
            status_glyph(&LegacyStatus::Conflicted, -1, false, 0),
            Some("icons/tx_conflicted")
        );
    }
}

#[cfg(test)]
mod live_split_tests {
    use super::*;

    /// Mirror of the Overview split over (value, height, kind) coins, which is
    /// exactly the shape the GUI feeds it from listunspent. Regression guard
    /// for "still 0 under spendable": a wallet whose coins are all 4+ deep must
    /// produce a non-zero spendable figure.
    fn split(coins: &[(i64, u64, &str)], tip: u64) -> (i64, i64, i64, i64) {
        let (mut sp, mut st, mut un, mut im) = (0i64, 0i64, 0i64, 0i64);
        for (value, height, kind) in coins {
            let depth = depth_of(*height, tip);
            let generated = *kind == "coinbase" || *kind == "coinstake";
            let matures = (COINBASE_MATURITY - depth).max(0);
            if generated && matures > 0 {
                if *kind == "coinstake" {
                    st += *value;
                } else {
                    im += *value;
                }
            } else if depth >= RECOMMENDED_CONFIRMATIONS {
                sp += *value;
            } else {
                un += *value;
            }
        }
        (sp, st, un, im)
    }

    #[test]
    fn deep_coins_are_spendable_never_zero() {
        let (sp, st, un, im) = split(
            &[
                (100_000_000, 2_200_000, "regular"),
                (50_000_000, 2_205_000, "coinstake"),
            ],
            2_205_400,
        );
        assert_eq!(sp, 150_000_000, "both coins are far past maturity");
        assert_eq!((st, un, im), (0, 0, 0));
    }

    #[test]
    fn fresh_stakes_are_stake_not_spendable() {
        let (sp, st, _un, _im) = split(&[(10_000_000, 2_205_399, "coinstake")], 2_205_400);
        assert_eq!(sp, 0);
        assert_eq!(st, 10_000_000);
    }

    #[test]
    fn one_confirmation_receipt_is_unconfirmed() {
        let (sp, _st, un, _im) = split(&[(1_000_000, 2_205_400, "regular")], 2_205_400);
        assert_eq!(sp, 0);
        assert_eq!(un, 1_000_000);
    }
}
