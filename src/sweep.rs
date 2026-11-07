//! Seed migration ("sweep-to-seed") — the money-path engine that lets an
//! existing random-key wallet GAIN a seed phrase.
//!
//! Design (sweep, NOT conversion): the wallet's keys can never be re-derived
//! from a phrase, so migration moves the VALUE instead: a fresh (or restored)
//! BIP39 phrase derives a new address set (m/44'/2222'/0'/0/0..9), those keys
//! are inserted into the SAME wallet.dat (the qt wallet sees them too), and
//! ONE transaction sweeps every spendable UTXO of the wallet's other
//! addresses into the seed-derived addresses. After the sweep the coins are
//! controlled by seed-derived keys; the old random keys remain in wallet.dat
//! (harmless, now empty).
//!
//! Money-path paranoia, encoded here:
//!   - the 10 destination addresses must roundtrip (same mnemonic restored on
//!     a fresh wallet file → identical addresses) before any signing — that
//!     proof lives in gui::sweep_flow::prepare and is pinned by tests here;
//!   - input selection applies the SAME maturity rule ConnectInputs enforces
//!     (coinbase AND coinstake depth ≥ COINBASE_MATURITY) at SELECTION time,
//!     never at consensus time;
//!   - fee mirrors CreateTransaction exactly: nPayFee =
//!     DEFAULT_TRANSACTION_FEE·(1+bytes/1000) raised to GetMinFee(Block,1),
//!     with the fee loop re-deriving the output split as the fee rises
//!     (split_amount: even split, remainder to the first output, per-output
//!     MoneyRange cap);
//!   - change handling: zero change by construction — outputs + fee == inputs
//!     is asserted before the tx is returned;
//!   - the signed tx is re-verified input-by-input and run through
//!     ConnectInputs BEFORE the plan is handed back, then broadcast through
//!     the GUI's existing sendrawtransaction path (same as Send).

use crate::legacy::{Tx, Vin, Vout};
use crate::spend::{
    self, SpendInput, DEFAULT_TRANSACTION_FEE, MAX_BLOCK_SIZE_GEN, MAX_MONEY,
};

/// How many seed-derived addresses the sweep consolidates into (indices
/// 0..9 at m/44'/2222'/0'/0/i).
pub const SWEEP_ADDR_COUNT: usize = 10;

/// Hard cap on output parts in split_amount (defensive bound; the MoneyRange
/// cap decides the real split long before this).
pub const MAX_OUTPUT_PARTS: usize = 1000;

/// Fee-loop safety bound (mirrors build_spend's converging loop; the fee only
/// ever rises to `required`, so this never fires in practice).
const MAX_FEE_ITERATIONS: usize = 64;

/// One seed-derived sweep destination.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepDest {
    pub address: String,
    pub pubkey: Vec<u8>,       // 33-byte compressed
    pub payload: [u8; 21],     // hash160(pubkey) + version — P2PKH script body
}

/// Derive the sweep destination set from a mnemonic — exactly the wallet's
/// own HD continuation path (seed::derive_triangles, m/44'/2222'/0'/0/i).
/// The mnemonic is used BYTE-EXACT (the stored-row convention; user-typed
/// phrases are canonicalized by the wizard before they get here).
pub fn derive_sweep_destinations(
    mnemonic: &str,
    passphrase: &str,
) -> Result<Vec<SweepDest>, String> {
    let secp = secp256k1::Secp256k1::signing_only();
    let mut out = Vec::with_capacity(SWEEP_ADDR_COUNT);
    for i in 0..SWEEP_ADDR_COUNT as u32 {
        let secret = crate::seed::derive_triangles(mnemonic, passphrase, 0, 0, i)?;
        let sk = secp256k1::SecretKey::from_slice(&secret)
            .map_err(|e| format!("sweep dest {}: bad derived secret: {}", i, e))?;
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        let pubkey = pk.serialize().to_vec();
        let payload = crate::address::pubkey_payload(&pubkey);
        out.push(SweepDest {
            address: crate::wallet::pubkey_to_address(&pubkey),
            pubkey,
            payload,
        });
    }
    Ok(out)
}

/// Maturity rule for sweep inputs — the SAME rule ConnectInputs enforces
/// (spend.rs: coinbase AND coinstake outputs need COINBASE_MATURITY depth).
/// Applied at SELECTION time so an immature input is refused with a clear
/// message instead of failing at consensus after signing.
pub fn sweep_spendable(prev_kind: &str, containing_height: u32, spend_height: u32) -> bool {
    let depth = spend_height.saturating_sub(containing_height) as i64;
    if prev_kind == "coinbase" || prev_kind == "coinstake" {
        depth >= spend::COINBASE_MATURITY as i64
    } else {
        true
    }
}

/// Residue floor for sweep passes (workqueue SWEEP-RESIDUE rule): coins
/// below this many sats stay where they are — chasing them grows the fee
/// and re-fragments the wallet for no value. 0.005 TRI = 5_000 sats.
pub const SWEEP_MIN_RESIDUE_SATS: i64 = 5_000;

/// Partition sweep sources at the residue floor: everything worth sweeping
/// (value >= min_sats) and the residue left behind (value < min_sats).
/// Stable order preserved in both halves; `min_sats <= 0` keeps everything.
pub fn split_residue_skip(
    inputs: Vec<SpendInput>,
    min_sats: i64,
) -> (Vec<SpendInput>, Vec<SpendInput>) {
    let mut keep = Vec::new();
    let mut residue = Vec::new();
    for i in inputs {
        if min_sats <= 0 || i.value >= min_sats {
            keep.push(i);
        } else {
            residue.push(i);
        }
    }
    (keep, residue)
}

/// The sweep source set: every input that is NOT one of the destination
/// addresses (coins already on the seed-derived addresses stay there).
/// Destination matching is payload-based (hash160 of the destination pubkey
/// vs the prevout's P2PKH/P2PK body) — no string round-trip.
pub fn sweep_source_inputs<'a>(inputs: &'a [SpendInput], dests: &[SweepDest]) -> Vec<SpendInput> {
    let payloads: std::collections::HashSet<[u8; 21]> =
        dests.iter().map(|d| d.payload).collect();
    inputs
        .iter()
        .filter(|i| match crate::address::script_to_dest(&i.script_pubkey) {
            Some(dest) => !payloads.contains(&dest.payload()),
            None => true, // non-standard prevouts never match a destination
        })
        .cloned()
        .collect()
}

/// Split `total` into up to `parts` outputs: even split, remainder to the
/// FIRST output; falls back to a single output when the total cannot be
/// divided meaningfully. Every output must fit `per_output_cap` (MoneyRange
/// on this chain); if the even share exceeds the cap the part count grows
/// until it fits (bounded by MAX_OUTPUT_PARTS) or the split is refused.
pub fn split_amount(total: i64, parts: usize, per_output_cap: i64) -> Result<Vec<i64>, String> {
    if total <= 0 {
        return Err(format!("split: non-positive total {}", total));
    }
    if parts == 0 {
        return Err("split: zero parts".into());
    }
    let mut n = parts.min(MAX_OUTPUT_PARTS);
    while n < MAX_OUTPUT_PARTS && total / n as i64 > per_output_cap {
        n += 1;
    }
    let base = total / n as i64;
    if base > per_output_cap {
        return Err(format!(
            "split: {} cannot be split into ≤{} outputs under the per-output cap {}",
            total, MAX_OUTPUT_PARTS, per_output_cap
        ));
    }
    if base == 0 {
        // too small to divide — a single output keeps every sat spendable
        return Ok(vec![total]);
    }
    let rem = total % n as i64;
    let mut out = Vec::with_capacity(n);
    out.push(base + rem);
    out.extend(std::iter::repeat(base).take(n - 1));
    Ok(out)
}

/// The signed sweep plan. `change` is zero BY CONSTRUCTION: the output split
/// is derived from inputs − fee, and the invariant outputs + fee == inputs is
/// asserted before the plan is returned.
#[derive(Debug)]
pub struct SweepPlan {
    pub tx: Tx,
    pub fee: i64,
    pub inputs_total: i64,
    /// (address, value) in output order — the consolidation targets.
    pub outputs: Vec<(String, i64)>,
    /// The inputs actually selected, sorted (txid, n) matching tx.vin order.
    pub selected: Vec<SpendInput>,
    pub n_bytes: usize,
}

/// Build + sign the ONE consolidation transaction (CreateTransaction parity:
/// per-input SignSignature over SIGHASH_ALL, size cap MAX_BLOCK_SIZE_GEN/5,
/// fee raised to max(nPayFee, GetMinFee(Block, nBlockSize=1))).
///
/// `fee` starts at `fee_floor` (the GUI passes 0 — the minimum is computed
/// from the built bytes, never trusted from a form field).
pub fn plan_sweep(
    inputs: &[SpendInput],
    dests: &[SweepDest],
    fee_floor: i64,
    n_time: u32,
    spend_height: u32,
) -> Result<SweepPlan, String> {
    if dests.len() < SWEEP_ADDR_COUNT {
        return Err(format!(
            "plan: expected {} seed destinations, got {}",
            SWEEP_ADDR_COUNT,
            dests.len()
        ));
    }
    if inputs.is_empty() {
        return Err(
            "no spendable UTXOs to sweep — nothing to migrate (zero balance or immature coins)"
                .into(),
        );
    }
    let mut inputs_total = 0i64;
    for i in inputs {
        if !spend::money_range(i.value) {
            return Err(format!(
                "plan: input {}:{} value out of MoneyRange",
                crate::legacy::hash_to_display(&i.txid),
                i.n
            ));
        }
        i.assert_owns_prevout()?;
        inputs_total = inputs_total
            .checked_add(i.value)
            .ok_or_else(|| "plan: input sum overflow".to_string())?;
    }
    if !spend::money_range(inputs_total) {
        return Err("plan: input sum out of MoneyRange".into());
    }
    // ConnectInputs refuses a tx timestamped before any input tx — pin n_time
    // above the newest input (same rule build_signed_send applies).
    let n_time = n_time.max(inputs.iter().map(|i| i.tx_time).max().unwrap_or(0));

    let mut ordered: Vec<SpendInput> = inputs.to_vec();
    ordered.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.n.cmp(&b.n)));

    let mut fee = fee_floor;
    for _ in 0..MAX_FEE_ITERATIONS {
        let spendable = inputs_total
            .checked_sub(fee)
            .ok_or_else(|| "plan: inputs below fee".to_string())?;
        if spendable <= 0 {
            return Err(format!(
                "sweep: balance ({}) does not cover the minimum fee ({}) — nothing to migrate",
                inputs_total, fee
            ));
        }
        let values = split_amount(spendable, SWEEP_ADDR_COUNT, MAX_MONEY)?;
        if values.len() > dests.len() {
            return Err(format!(
                "plan: split produced {} outputs but only {} destinations exist",
                values.len(),
                dests.len()
            ));
        }
        let vout: Vec<Vout> = values
            .iter()
            .zip(dests.iter())
            .map(|(v, d)| Vout {
                value: *v,
                script_pubkey: spend::script_pubkey_p2pkh(&d.pubkey),
            })
            .collect();
        let vin: Vec<Vin> = ordered
            .iter()
            .map(|i| Vin {
                prev_hash: i.txid,
                prev_n: i.n,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            })
            .collect();
        let mut tx = Tx {
            version: 1,
            time: n_time,
            vin,
            vout,
            locktime: 0,
        };
        for (i, input) in ordered.iter().enumerate() {
            spend::sign_input(
                &mut tx,
                i,
                &input.script_pubkey,
                &input.secret,
                &input.pubkey,
                spend::SIGHASH_ALL,
            )?;
        }
        let n_bytes = tx.serialize().len();
        if n_bytes >= MAX_BLOCK_SIZE_GEN / 5 {
            return Err(format!("sweep: tx too large ({} bytes)", n_bytes));
        }
        // CreateTransaction fee check: nPayFee = nTransactionFee*(1+bytes/1000)
        // vs GetMinFee(Block, nBlockSize=1) — the < CENT output rule inside
        // GetMinFee is what keeps dust outputs from under-paying relay.
        let n_pay_fee = DEFAULT_TRANSACTION_FEE * (1 + n_bytes as i64 / 1000);
        let n_min_fee = spend::get_min_fee(
            &tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
            n_bytes,
            1,
            spend::FeeMode::Block,
        );
        let required = n_pay_fee.max(n_min_fee);
        if fee < required {
            fee = required;
            continue;
        }

        // Validate BEFORE returning the plan — the same gates the network
        // applies, plus an input-by-input signature re-verification.
        spend::connect_inputs_check(&tx, &ordered, spend_height)?;
        for (i, input) in ordered.iter().enumerate() {
            spend::verify_input(&tx, i, &input.script_pubkey)?;
        }

        // Change handling invariant: zero change by construction.
        let outputs_sum: i64 = values.iter().sum();
        if outputs_sum + fee != inputs_total {
            return Err(format!(
                "sweep: consolidation invariant broken (outputs {} + fee {} != inputs {})",
                outputs_sum, fee, inputs_total
            ));
        }

        return Ok(SweepPlan {
            tx,
            fee,
            inputs_total,
            outputs: values
                .into_iter()
                .zip(dests.iter())
                .map(|(v, d)| (d.address.clone(), v))
                .collect(),
            selected: ordered,
            n_bytes,
        });
    }
    Err("sweep: fee loop did not converge".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::{COINBASE_MATURITY, MIN_TX_FEE};

    const MN12: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MN24: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    /// A synthetic wallet key (deterministic, never real material) + its
    /// address, for fixture UTXOs.
    fn synth_key(label: &str) -> ([u8; 32], Vec<u8>, String) {
        let secret = crate::legacy::sha256d(format!("tri7-sweep/{}", label).as_bytes());
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        let pubkey = pk.serialize().to_vec();
        let addr = crate::wallet::pubkey_to_address(&pubkey);
        (secret, pubkey, addr)
    }

    fn p2pkh_utxo(label: &str, n: u32, value: i64, height: u32, tx_time: u32) -> SpendInput {
        let (secret, pubkey, _addr) = synth_key(label);
        SpendInput {
            txid: crate::legacy::sha256d(format!("{}:{}", label, n).as_bytes()),
            n,
            value,
            script_pubkey: spend::script_pubkey_p2pkh(&pubkey),
            height,
            tx_time,
            is_coinbase: false,
            is_coinstake: false,
            secret,
            pubkey,
        }
    }

    fn dests() -> Vec<SweepDest> {
        derive_sweep_destinations(MN12, "").unwrap()
    }

    // ---- split_amount ------------------------------------------------------

    #[test]
    fn split_amount_sums_exact_remainder_to_first() {
        let v = split_amount(1_000_001, SWEEP_ADDR_COUNT, MAX_MONEY).unwrap();
        assert_eq!(v.len(), SWEEP_ADDR_COUNT);
        assert_eq!(v.iter().sum::<i64>(), 1_000_001);
        assert_eq!(v[0], 100_001); // remainder lands on the first output
        assert!(v[1..].iter().all(|&x| x == 100_000));

        // smaller than the part count → base 0 → single output keeps every sat
        let v = split_amount(5, SWEEP_ADDR_COUNT, MAX_MONEY).unwrap();
        assert_eq!(v, vec![5]);

        // exactly divisible
        let v = split_amount(SWEEP_ADDR_COUNT as i64, SWEEP_ADDR_COUNT, MAX_MONEY).unwrap();
        assert_eq!(v, vec![1; SWEEP_ADDR_COUNT]);

        // base 1 with remainder 9: one sat per output is still a valid split
        let v = split_amount(10, SWEEP_ADDR_COUNT, MAX_MONEY).unwrap();
        assert_eq!(v, vec![1; SWEEP_ADDR_COUNT]);
    }

    #[test]
    fn split_amount_respects_per_output_cap() {
        // a small cap forces more parts than SWEEP_ADDR_COUNT
        let v = split_amount(1_000, 2, 300).unwrap();
        assert!(v.iter().all(|&x| x <= 300), "parts {:?}", v);
        assert_eq!(v.iter().sum::<i64>(), 1_000);
        // impossible even at MAX_OUTPUT_PARTS: refused, never violated
        assert!(split_amount(2_001, 2, 1).is_err());
        assert!(split_amount(0, 10, MAX_MONEY).is_err());
        assert!(split_amount(-1, 10, MAX_MONEY).is_err());
    }

    // ---- destinations + roundtrip -------------------------------------------

    #[test]
    fn derive_sweep_destinations_matches_wallet_derivation() {
        let d = dests();
        assert_eq!(d.len(), SWEEP_ADDR_COUNT);
        for (i, dest) in d.iter().enumerate() {
            // must equal the wallet's own HD derivation for the same index
            let secret = crate::seed::derive_triangles(MN12, "", 0, 0, i as u32).unwrap();
            let secp = secp256k1::Secp256k1::signing_only();
            let pk = secp256k1::PublicKey::from_secret_key(
                &secp,
                &secp256k1::SecretKey::from_slice(&secret).unwrap(),
            );
            assert_eq!(dest.pubkey, pk.serialize().to_vec());
            assert_eq!(dest.payload, crate::address::pubkey_payload(&pk.serialize()));
        }
        // a different mnemonic (or passphrase) derives a different set
        let d24 = derive_sweep_destinations(MN24, "").unwrap();
        assert_ne!(d[0], d24[0]);
        let dp = derive_sweep_destinations(MN12, "x").unwrap();
        assert_ne!(d[0], dp[0]);
    }

    #[test]
    fn roundtrip_through_a_fresh_wallet_file_reproduces_addresses() {
        // THE migration safety proof: restoring the same mnemonic on a fresh
        // wallet file must produce byte-identical addresses to the direct
        // derivation, BEFORE any signing would ever happen.
        let direct = dests();
        let dir = std::env::temp_dir().join(format!(
            "tri7_sweep_roundtrip_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.dat");
        let out = crate::wallet::restore_wallet_from_seed(
            &path,
            MN12,
            "",
            SWEEP_ADDR_COUNT,
        )
        .unwrap();
        let restored: Vec<String> = out.keys.iter().map(|k| k.address.clone()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            restored,
            direct.iter().map(|d| d.address.clone()).collect::<Vec<_>>()
        );
    }

    // ---- selection filters ---------------------------------------------------

    #[test]
    fn sweep_spendable_gates_coinbase_and_coinstake_maturity() {
        let tip = 1_000u32;
        assert!(sweep_spendable("regular", 995, tip)); // depth 5, regular OK
        assert!(!sweep_spendable("coinbase", 995, tip)); // depth 5 < 7
        assert!(!sweep_spendable("coinstake", 995, tip)); // depth 5 < 7
        assert!(sweep_spendable("coinbase", 1000 - COINBASE_MATURITY as u32, tip));
        assert!(sweep_spendable("coinstake", 993, tip)); // depth 7
    }

    #[test]
    fn sweep_source_inputs_excludes_destination_addresses() {
        let d = dests();
        let mut u = p2pkh_utxo("src", 0, 5_000_000, 100, 1_400_000_000);
        // rewrite the prevout to pay directly TO destination #3
        u.script_pubkey = spend::script_pubkey_p2pkh(&d[3].pubkey);
        let other = p2pkh_utxo("src2", 1, 7_000_000, 100, 1_400_000_000);
        let other_pubkey = other.pubkey.clone();
        let src = sweep_source_inputs(&[u, other], &d);
        assert_eq!(src.len(), 1);
        assert_eq!(src[0].pubkey, other_pubkey);
    }

    #[test]
    fn split_residue_skip_leaves_sub_floor_coins_behind() {
        // floor 5000: 4999 stays, 5000/5001 go; stable order preserved
        let inputs = vec![
            p2pkh_utxo("a", 0, 5_001, 100, 1_400_000_000),
            p2pkh_utxo("b", 1, 4_999, 101, 1_400_000_001),
            p2pkh_utxo("c", 0, 5_000, 102, 1_400_000_002),
            p2pkh_utxo("d", 2, 1, 103, 1_400_000_003),
        ];
        let (keep, residue) = split_residue_skip(inputs, SWEEP_MIN_RESIDUE_SATS);
        assert_eq!(keep.iter().map(|i| i.value).collect::<Vec<_>>(), vec![5_001, 5_000]);
        assert_eq!(residue.iter().map(|i| i.value).collect::<Vec<_>>(), vec![4_999, 1]);
        // floor 0 (or negative) sweeps everything — nothing becomes unspendable
        let inputs = vec![p2pkh_utxo("a", 0, 1, 100, 1_400_000_000)];
        let (keep, residue) = split_residue_skip(inputs, 0);
        assert_eq!(keep.len(), 1);
        assert!(residue.is_empty());
        // empty input set is a clean no-op
        let (keep, residue) = split_residue_skip(Vec::new(), SWEEP_MIN_RESIDUE_SATS);
        assert!(keep.is_empty() && residue.is_empty());
    }

    // ---- plan_sweep: the money path ------------------------------------------

    #[test]
    fn plan_sweep_consolidates_multi_utxo_with_exact_fee_math() {
        let d = dests();
        let inputs = vec![
            p2pkh_utxo("a", 0, 3_000_000, 100, 1_400_000_000),
            p2pkh_utxo("b", 1, 2_500_000, 101, 1_400_000_100),
            p2pkh_utxo("c", 0, 999_999, 102, 1_400_000_200),
            p2pkh_utxo("d", 2, 123_456, 103, 1_400_000_300),
            p2pkh_utxo("e", 5, 77_777, 104, 1_400_000_400),
        ];
        let total: i64 = inputs.iter().map(|i| i.value).sum();
        let plan = plan_sweep(&inputs, &d, 0, 1_500_000_000, 200).unwrap();

        // ONE transaction, every input, sorted (txid, n)
        assert_eq!(plan.selected.len(), inputs.len());
        assert_eq!(plan.tx.vin.len(), inputs.len());
        let mut want = inputs.clone();
        want.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.n.cmp(&b.n)));
        for (got, w) in plan.tx.vin.iter().zip(want.iter()) {
            assert_eq!(got.prev_hash, w.txid);
            assert_eq!(got.prev_n, w.n);
        }

        // consolidation invariant: outputs + fee == inputs, zero change
        let out_sum: i64 = plan.outputs.iter().map(|(_, v)| v).sum();
        assert_eq!(out_sum + plan.fee, total);
        assert_eq!(plan.inputs_total, total);
        assert_eq!(plan.outputs.len(), SWEEP_ADDR_COUNT);

        // destinations in derivation order
        for (i, (addr, _)) in plan.outputs.iter().enumerate() {
            assert_eq!(addr, &d[i].address);
        }
        // split shape: even with remainder on output 0 (the split is over
        // inputs − fee, never over the raw total)
        let base = (total - plan.fee) / SWEEP_ADDR_COUNT as i64;
        assert_eq!(plan.outputs[0].1, (total - plan.fee) - base * 9);
        assert!(plan.outputs[1..].iter().all(|(_, v)| *v == base));

        // fee floor honored: at least the CreateTransaction-required fee for
        // the FINAL bytes
        let n_pay_fee = DEFAULT_TRANSACTION_FEE * (1 + plan.n_bytes as i64 / 1000);
        let n_min_fee = spend::get_min_fee(
            &plan.tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
            plan.n_bytes,
            1,
            spend::FeeMode::Block,
        );
        assert!(plan.fee >= n_pay_fee.max(n_min_fee), "fee {}", plan.fee);
        assert!(plan.fee >= MIN_TX_FEE);
        assert_eq!(plan.tx.serialize().len(), plan.n_bytes);

        // the tx is genuinely valid: signatures + connect checks pass fresh
        for (i, input) in plan.selected.iter().enumerate() {
            spend::verify_input(&plan.tx, i, &input.script_pubkey).unwrap();
        }
        spend::connect_inputs_check(&plan.tx, &plan.selected, 200).unwrap();
    }

    #[test]
    fn plan_sweep_signs_p2pk_prevouts_too() {
        let d = dests();
        let (secret, pubkey, _addr) = synth_key("p2pk-owner");
        let input = SpendInput {
            txid: crate::legacy::sha256d(b"p2pk-prevout"),
            n: 0,
            value: 4_000_000,
            script_pubkey: spend::script_pubkey_p2pk(&pubkey).unwrap(),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret,
            pubkey: pubkey.clone(),
        };
        let plan = plan_sweep(&[input], &d, 0, 1_500_000_000, 200).unwrap();
        assert_eq!(plan.tx.vin.len(), 1);
        assert_eq!(plan.outputs.len(), SWEEP_ADDR_COUNT);
        spend::verify_input(&plan.tx, 0, &spend::script_pubkey_p2pk(&pubkey).unwrap()).unwrap();
    }

    #[test]
    fn plan_sweep_refuses_empty_and_below_fee_balances() {
        let d = dests();
        // empty source set
        assert!(plan_sweep(&[], &d, 0, 1_500_000_000, 200).is_err());
        // total below the required fee: inputs 150 with a >150 fee requirement
        // — use an absurd fee floor to force the "cannot cover" branch
        let inputs = vec![p2pkh_utxo("tiny", 0, 150, 100, 1_400_000_000)];
        let err = plan_sweep(&inputs, &d, 10_000_000, 1_500_000_000, 200).unwrap_err();
        assert!(err.contains("does not cover the minimum fee"), "{}", err);
    }

    #[test]
    fn plan_sweep_fee_floor_is_raised_not_trusted() {
        // a caller-supplied floor of 0 must still end at the required fee
        let d = dests();
        let inputs = vec![p2pkh_utxo("floor", 0, 1_000_000, 100, 1_400_000_000)];
        let plan = plan_sweep(&inputs, &d, 0, 1_500_000_000, 200).unwrap();
        let n_pay_fee = DEFAULT_TRANSACTION_FEE * (1 + plan.n_bytes as i64 / 1000);
        assert!(plan.fee >= n_pay_fee);
        // starting higher stays at exactly the requirement (never overpays
        // beyond the caller's floor)
        let plan2 = plan_sweep(&inputs, &d, n_pay_fee, 1_500_000_000, 200).unwrap();
        assert_eq!(plan2.fee, n_pay_fee);
        assert_eq!(hex(&plan.tx.serialize()), hex(&plan2.tx.serialize()));
    }

    #[test]
    fn plan_sweep_small_balance_still_splits_and_keeps_every_sat() {
        // 5_000 sats covers the ~100-sat minimum fee, so the split still
        // produces all 10 outputs — each above 0, sum exact
        let d = dests();
        let inputs = vec![p2pkh_utxo("small", 0, 5_000, 100, 1_400_000_000)];
        let plan = plan_sweep(&inputs, &d, 0, 1_500_000_000, 200).unwrap();
        assert_eq!(plan.outputs.len(), SWEEP_ADDR_COUNT);
        let out_sum: i64 = plan.outputs.iter().map(|(_, v)| v).sum();
        assert_eq!(out_sum + plan.fee, 5_000);
        assert_eq!(plan.outputs[0].0, d[0].address);
        assert!(plan.outputs.iter().all(|(_, v)| *v > 0));
        spend::connect_inputs_check(&plan.tx, &plan.selected, 200).unwrap();
        // a balance that CANNOT cover the fee is refused outright
        let inputs = vec![p2pkh_utxo("dust", 0, 25, 100, 1_400_000_000)];
        let err = plan_sweep(&inputs, &d, 0, 1_500_000_000, 200).unwrap_err();
        assert!(err.contains("does not cover the minimum fee"), "{}", err);
    }

    #[test]
    fn plan_sweep_refuses_short_destination_sets() {
        let d = dests();
        let inputs = vec![p2pkh_utxo("x", 0, 1_000_000, 100, 1_400_000_000)];
        assert!(plan_sweep(&inputs, &d[..5], 0, 1_500_000_000, 200).is_err());
    }

    // ---- tamper resistance ----------------------------------------------------

    #[test]
    fn plan_sweep_output_scripts_pay_exactly_the_destinations() {
        let d = dests();
        let inputs = vec![p2pkh_utxo("script", 0, 2_000_000, 100, 1_400_000_000)];
        let plan = plan_sweep(&inputs, &d, 0, 1_500_000_000, 200).unwrap();
        for (o, dest) in plan.tx.vout.iter().zip(d.iter()) {
            // P2PKH(script) == OP_DUP OP_HASH160 PUSH20 <hash160(pubkey)>
            // EQUALVERIFY CHECKSIG — the 20-byte body is the HASH (the
            // payload without its version byte), never the payload itself
            let expect = {
                let mut s = vec![0x76u8, 0xa9, 0x14];
                s.extend_from_slice(&dest.payload[1..]);
                s.extend_from_slice(&[0x88, 0xac]);
                s
            };
            assert_eq!(o.script_pubkey, expect);
            assert!(spend::money_range(o.value) && o.value > 0);
        }
        // outputs sorted list matches destinations one-to-one (no reordering)
        for ((addr, _), dest) in plan.outputs.iter().zip(d.iter()) {
            assert_eq!(addr, &dest.address);
        }
    }

    #[test]
    fn destinations_deterministic_across_calls() {
        let a = dests();
        let b = dests();
        assert_eq!(a, b);
        assert!(a.windows(2).all(|w| w[0] != w[1]));
        assert_eq!(hex(&a[0].payload).len(), 42);
    }
}
