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
/// wallet.cpp GetStake(): the "Stake" row is every coinstake output that is
/// mined AND still maturing (`IsCoinStake() && GetBlocksToMaturity() > 0 &&
/// GetDepthInMainChain() > 0`), summed with GetCredit — i.e. the whole stake
/// transaction's value, NOT only the reward output (the reward is the net
/// difference; the principal is returned in the second output).
///
/// `tx_values` is every output value of that coinstake transaction that the
/// wallet owns; `depth` is the coinstake block's depth.
pub fn stake_row_value(depth: i64, tx_values: &[i64]) -> i64 {
    if depth <= 0 {
        return 0;
    }
    if (COINBASE_MATURITY - depth).max(0) == 0 {
        return 0; // matured: the coins are plain balance now
    }
    tx_values.iter().sum()
}

/// A stake's *reward* — the part a holder thinks of as "what I just earned".
/// Legacy shows it as the Mined row: nNet when positive, else value-out minus
/// the stake principal (transactionrecord.cpp decomposeTransaction).
pub fn stake_reward(credit_total: i64, principal: i64) -> i64 {
    (credit_total - principal).max(0)
}

/// Is the row's block in the main chain, as far as this client can tell?
///
/// The legacy check is a mapBlockIndex lookup, which is authoritative. Our
/// equivalent is the store index, and the store's threaded map can lag the tip
/// by design (a body lands after its header). Treating "index hasn't caught up
/// yet" as "not in the chain" is exactly how a whole wallet turned into
/// "Generated but not accepted" when the tip advanced past the index.
///
/// Rule: a mined row is in the chain unless the tip proves otherwise — height 0
/// or a height beyond the daemon's tip. Anything below the tip is in the chain.
fn in_main_chain(height: u64, tip_height: u64) -> bool {
    height > 0 && height <= tip_height
}

pub fn status_for(kind: LegacyKind, depth: i64, tip_height: u64, height: u64) -> (LegacyStatus, i64) {
    let generated = matches!(kind, LegacyKind::Generated);
    if generated {
        // GetBlocksToMaturity(): max(0, nCoinbaseMaturity - depth)
        let matures_in = (COINBASE_MATURITY - depth).max(0);
        if !in_main_chain(height, tip_height) {
            // genuinely unknown/absent from the chain at this height
            return (LegacyStatus::NotAccepted, 0);
        }
        if matures_in > 0 {
            return (LegacyStatus::Immature, matures_in);
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

#[cfg(test)]
mod stake_row_tests {
    use super::*;

    #[test]
    fn maturing_stake_row_is_the_whole_transaction_value() {
        // principal 143.7 TRI returned + 0.0084 reward, depth 1 -> maturing
        let values = vec![14_377_923i64, 8_448];
        assert_eq!(stake_row_value(1, &values), 14_386_371);
        assert_eq!(stake_reward(14_386_371, 14_377_923), 8_448);
    }

    #[test]
    fn matured_stake_leaves_the_stake_row() {
        // at 7 confirmations legacy stops calling it Stake; it is balance now
        assert_eq!(stake_row_value(7, &[1_000_000]), 0);
        assert_eq!(stake_row_value(20, &[1_000_000]), 0);
    }

    #[test]
    fn unmined_stake_counts_because_depth_is_positive() {
        assert_eq!(stake_row_value(0, &[1_000_000]), 0);
        assert_eq!(stake_row_value(6, &[1_000_000]), 1_000_000);
    }
}

#[cfg(test)]
mod chain_membership_tests {
    use super::*;

    /// Regression: a wallet where every stake rendered "Generated but not
    /// accepted" because the store index lagged the tip and the legacy
    /// IsInMainChain() equivalent said "unknown" instead of "in the chain".
    #[test]
    fn rows_below_the_tip_are_in_the_chain() {
        // a stake 3 blocks deep is maturing; 207 deep is mature
        let (st, matures) = status_for(LegacyKind::Generated, 3, 2_205_516, 2_205_514);
        assert_eq!(st, LegacyStatus::Immature);
        assert_eq!(matures, 4);
        let (st_old, _) = status_for(LegacyKind::Generated, 207, 2_205_516, 2_205_310);
        assert_eq!(st_old, LegacyStatus::Confirmed);
        // and a very fresh one is Immature too, never NotAccepted
        let (st2, _) = status_for(LegacyKind::Generated, 1, 2_205_516, 2_205_516);
        assert_eq!(st2, LegacyStatus::Immature);
    }

    #[test]
    fn not_accepted_requires_being_outside_the_chain() {
        // height beyond the tip = genuinely not in this chain
        assert_eq!(
            status_for(LegacyKind::Generated, 3, 2_205_000, 2_206_000).0,
            LegacyStatus::NotAccepted
        );
        // height 0 = never mined
        assert_eq!(
            status_for(LegacyKind::Generated, 0, 2_205_000, 0).0,
            LegacyStatus::NotAccepted
        );
    }

    #[test]
    fn no_generated_row_at_or_below_tip_can_be_not_accepted() {
        for h in [1u64, 100, 2_200_000, 2_205_000] {
            let (st, _) = status_for(LegacyKind::Generated, 5, 2_205_516, h);
            assert_ne!(
                st,
                LegacyStatus::NotAccepted,
                "h{} at tip 2205516 must be in the chain",
                h
            );
        }
    }
}

/// Build the legacy row for one history record. Single source of truth for
/// both the Transactions table and the CSV export — the table used to do this
/// mapping inline, which meant an export would have had to copy it.
pub fn row_from_history(u: &crate::history::HistoryRec, tip: u32) -> LegacyRow {
    let lk = if !u.legacy_kind.is_empty() {
        match u.legacy_kind.as_str() {
            "mined" => LegacyKind::Generated,
            "send" => LegacyKind::SendToAddress,
            "self" => LegacyKind::SendToSelf,
            "recv_from" => LegacyKind::RecvFromOther,
            _ => LegacyKind::RecvWithAddress,
        }
    } else if u.kind == "coinbase" || u.kind == "coinstake" {
        LegacyKind::Generated
    } else if u.value_sats < 0 {
        LegacyKind::SendToAddress
    } else {
        LegacyKind::RecvWithAddress
    };
    let depth = depth_of(u.height, tip as u64);
    let (status, matures_in) = status_for(lk, depth, tip as u64, u.height);
    LegacyRow {
        txid: u.txid.clone(),
        idx: u.idx as usize,
        time: u.time,
        height: u.height,
        kind: lk,
        address: u.address.clone(),
        debit: if u.value_sats < 0 { u.value_sats } else { 0 },
        credit: if u.value_sats > 0 { u.value_sats } else { 0 },
        status,
        depth,
        matures_in,
    }
}

/// CSV in the legacy column order (transactionview.cpp exportClicked):
/// Confirmed, Date, Type, Label, Address, Amount, ID.
pub fn csv_header() -> &'static str {
    "Confirmed,Date,Type,Label,Address,Amount,ID"
}

pub fn csv_line(r: &LegacyRow, label: &str) -> String {
    let esc = |s: &str| {
        if s.contains(',') || s.contains('"') || s.contains('\n') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    format!(
        "{},{},{},{},{},{:.6},{}",
        r.status_text(),
        date_of(r.time),
        r.kind.label(),
        esc(label),
        esc(&r.address),
        r.amount() as f64 / 1_000_000.0,
        r.txid
    )
}

/// Legacy date format in transactiontablemodel.cpp: "yyyy-MM-dd HH:mm:ss".
fn date_of(epoch: u32) -> String {
    // Civil-from-days conversion (Howard Hinnant) — no chrono dependency.
    let secs = epoch as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod csv_tests {
    use super::*;

    #[test]
    fn csv_matches_legacy_columns_and_date_format() {
        let u = crate::history::HistoryRec {
            txid: "ab".repeat(32),
            height: 100,
            time: 1_700_000_000,
            value_sats: 1_234_567,
            address: "TADDR".into(),
            direction: "in".into(),
            kind: "regular".into(),
            spent: false,
            idx: 0,
            legacy_kind: String::new(),
            coinstake: false,
            stake_principal: 0,
        };
        let r = row_from_history(&u, 110);
        assert_eq!(r.kind.label(), "Received with");
        assert_eq!(date_of(1_700_000_000), "2023-11-14 22:13:20");
        let line = csv_line(&r, "savings");
        // depth_of is inclusive: tip 110 vs height 100 is 11 confirmations
        assert!(line.contains("Confirmed (11 confirmations),2023-11-14 22:13:20"));
        assert!(line.contains(",1.234567,"));
    }

    #[test]
    fn csv_escapes_commas_and_quotes() {
        let r = LegacyRow {
            txid: "ff".repeat(32),
            idx: 0,
            time: 1_700_000_000,
            height: 1,
            kind: LegacyKind::SendToAddress,
            address: "T,ADDR".into(),
            debit: -1_000_000,
            credit: 0,
            status: LegacyStatus::Unconfirmed,
            depth: 0,
            matures_in: 0,
        };
        let line = csv_line(&r, "a \"quoted\" label");
        assert!(line.contains("\"T,ADDR\""));
        assert!(line.contains("\"a \"\"quoted\"\" label\""));
    }
}
