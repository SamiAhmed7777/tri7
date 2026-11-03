//! Transaction relay pool — the sendrawtransaction / inv→getdata→tx relay
//! surface (phase 7 milestone 2). Consensus validation parity with the
//! legacy ConnectInputs / CheckTransaction / GetMinFee(Relay) gate:
//!
//!   1. CheckTransaction: non-empty in/out, no coinbase, money range, value
//!      conservation (out ≤ in).
//!   2. Every input must resolve in the INDEXED PREVOUTS (the node's live
//!      window UTXO view) — unknown or already-spent inputs are rejected.
//!   3. Standard scripts only (Solver forms: P2PKH / P2PK) and each input's
//!      signature is verified against the ported SignatureHash (spend.rs).
//!   4. Fee floor: GetMinFee(nBytes, 1, RELAY) — MIN_RELAY_TX_FEE per KB.
//!   5. Double-spends against OTHER POOL entries are rejected; a duplicate
//!      txid is a no-op success.
//!
//! The pool is BOUNDED (capacity + TTL) — a relay flood evicts the oldest.

use crate::legacy::{sha256d, Hash, Tx};
use crate::spend::{solver_type, verify_input, FeeMode, MIN_RELAY_TX_FEE};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Legacy MAX_SIZE/"standard" relay guard: refuse absurd serialized sizes.
/// The block builder caps at MAX_BLOCK_SIZE_GEN; relay accepts up to the
/// same cap (a bigger tx can never be minted into a block here).
pub const MAX_RELAY_TX_BYTES: usize = 500_000; // MAX_BLOCK_SIZE_GEN
/// Pool cap + entry TTL.
pub const DEFAULT_POOL_CAP: usize = 2000;
pub const DEFAULT_ENTRY_TTL_SECS: i64 = 60 * 60; // 1h relay horizon

#[derive(Clone, Debug)]
pub struct PoolUtxo {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
    pub height: u64,
    pub tx_time: u32,
}

/// The caller's indexed-prevout view (live wallet UTXO window).
pub trait UtxoView {
    fn utxo(&self, key: &(Hash, u32)) -> Option<PoolUtxo>;
}

#[derive(Clone, Debug)]
pub struct AcceptReport {
    pub txid: String,
    pub fee_sats: i64,
    pub n_bytes: usize,
    pub inputs: usize,
}

struct PoolEntry {
    raw: Vec<u8>,
    tx: Tx,
    fee_sats: i64,
    first_seen: i64,
}

pub struct TxPool {
    entries: HashMap<Hash, PoolEntry>,
    /// inputs claimed by pool entries (double-spend guard)
    claimed: HashSet<(Hash, u32)>,
    cap: usize,
    ttl: i64,
    pub accepted: u64,
    pub rejected: u64,
}

impl Default for TxPool {
    fn default() -> Self {
        TxPool::new(DEFAULT_POOL_CAP, DEFAULT_ENTRY_TTL_SECS)
    }
}

impl TxPool {
    pub fn new(cap: usize, ttl: i64) -> Self {
        TxPool {
            entries: HashMap::new(),
            claimed: HashSet::new(),
            cap,
            ttl,
            accepted: 0,
            rejected: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_fees(&self) -> i64 {
        self.entries.values().map(|e| e.fee_sats).sum()
    }

    pub fn get(&self, hash: &Hash) -> Option<(Vec<u8>, &Tx)> {
        self.entries
            .get(hash)
            .map(|e| (e.raw.clone(), &e.tx))
    }

    /// Age-out old entries.
    pub fn prune(&mut self, now: i64) -> usize {
        let cut = now - self.ttl;
        let expired: Vec<Hash> = self
            .entries
            .iter()
            .filter(|(_, e)| e.first_seen < cut)
            .map(|(h, _)| *h)
            .collect();
        for h in &expired {
            self.remove(h);
        }
        expired.len()
    }

    fn remove(&mut self, hash: &Hash) {
        if let Some(e) = self.entries.remove(hash) {
            for vin in &e.tx.vin {
                if vin.prev_n != u32::MAX {
                    self.claimed.remove(&(vin.prev_hash, vin.prev_n));
                }
            }
        }
    }

    /// Drop entries spending any of these prevouts (they're now confirmed —
    /// the minting loop calls this after accepting a block).
    pub fn confirm_spend(&mut self, spent: &[(Hash, u32)]) {
        let mut evicted = 0usize;
        let spent_set: HashSet<(Hash, u32)> = spent.iter().copied().collect();
        let dead: Vec<Hash> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.tx.vin
                    .iter()
                    .any(|v| spent_set.contains(&(v.prev_hash, v.prev_n)))
            })
            .map(|(h, _)| *h)
            .collect();
        for h in &dead {
            self.remove(h);
            evicted += 1;
        }
        let _ = evicted;
    }

    /// Full validation + insertion. Returns the report; Err = reject reason
    /// (the exact check that failed — surfaced verbatim in the RPC error).
    pub fn accept(
        &mut self,
        raw: &[u8],
        view: &dyn UtxoView,
        now: i64,
    ) -> Result<AcceptReport, String> {
        // parse
        if raw.len() > MAX_RELAY_TX_BYTES {
            self.rejected += 1;
            return Err(format!(
                "tx exceeds size limit: {} > {}",
                raw.len(),
                MAX_RELAY_TX_BYTES
            ));
        }
        let mut r = crate::legacy::Reader::new(raw);
        let tx = Tx::read(&mut r).map_err(|e| {
            self.rejected += 1;
            format!("tx parse: {}", e)
        })?;
        r.expect_done().map_err(|e| {
            self.rejected += 1;
            format!("tx trailing bytes: {}", e)
        })?;
        let hash = tx.hash();

        // duplicate = idempotent success (relay loops are expected)
        if self.entries.contains_key(&hash) {
            return Ok(AcceptReport {
                txid: display(&hash),
                fee_sats: self.entries[&hash].fee_sats,
                n_bytes: raw.len(),
                inputs: tx.vin.len(),
            });
        }

        // CheckTransaction (main.cpp CheckTransaction parity)
        if tx.vin.is_empty() || tx.vout.is_empty() {
            self.rejected += 1;
            return Err("tx has empty vin or vout".into());
        }
        if crate::consensus::tx_is_coin_base(&tx) {
            self.rejected += 1;
            return Err("coinbase tx rejected for relay".into());
        }
        let out_total: i64 = tx.vout.iter().map(|o| o.value).sum();
        if !crate::spend::money_range(out_total) {
            self.rejected += 1;
            return Err("tx output total out of money range".into());
        }
        for o in &tx.vout {
            if !crate::spend::money_range(o.value) {
                self.rejected += 1;
                return Err("tx output value out of money range".into());
            }
        }

        // inputs vs indexed prevouts + standard scripts + signatures
        let mut in_total: i64 = 0;
        let mut fresh: Vec<(Hash, u32)> = Vec::with_capacity(tx.vin.len());
        for (i, vin) in tx.vin.iter().enumerate() {
            let key = (vin.prev_hash, vin.prev_n);
            let Some(utxo) = view.utxo(&key) else {
                self.rejected += 1;
                return Err(format!(
                    "input {} {}:{} not found in indexed prevouts (unknown or outside the live window)",
                    i,
                    display(&vin.prev_hash),
                    vin.prev_n
                ));
            };
            if !solver_type(&utxo.script_pubkey).is_some() {
                self.rejected += 1;
                return Err(format!("input {}: prevout script is non-standard", i));
            }
            // verify the signature against the ported SignatureHash
            if let Err(e) = verify_input(&tx, i, &utxo.script_pubkey) {
                self.rejected += 1;
                return Err(format!("input {}: {}", i, e));
            }
            in_total += utxo.value;
            fresh.push(key);
        }
        let fee = in_total - out_total;
        if fee < 0 {
            self.rejected += 1;
            return Err(format!(
                "tx spends more than its inputs ({} > {})",
                out_total, in_total
            ));
        }
        // fee floor: GetMinFee(nBytes, nBlockSize=1, RELAY)
        let min_fee = crate::spend::get_min_fee(
            &tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
            raw.len(),
            1,
            FeeMode::Relay,
        );
        if fee < min_fee {
            self.rejected += 1;
            return Err(format!(
                "fee {} below relay minimum {} (MIN_RELAY_TX_FEE {} per KB)",
                fee, min_fee, MIN_RELAY_TX_FEE
            ));
        }

        // pool double-spend guard
        if fresh.iter().any(|k| self.claimed.contains(k)) {
            self.rejected += 1;
            return Err("inputs conflict with a transaction already in the relay pool".into());
        }

        // bounded pool: evict oldest when full
        while self.entries.len() >= self.cap {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.first_seen)
                .map(|(h, _)| *h);
            match oldest {
                Some(h) => self.remove(&h),
                None => break,
            }
        }

        for k in &fresh {
            self.claimed.insert(*k);
        }
        self.entries.insert(
            hash,
            PoolEntry {
                raw: raw.to_vec(),
                tx,
                fee_sats: fee,
                first_seen: now,
            },
        );
        self.accepted += 1;
        Ok(AcceptReport {
            txid: display(&hash),
            fee_sats: fee,
            n_bytes: raw.len(),
            inputs: fresh.len(),
        })
    }

    /// mempool-style JSON for getrawmempool / RPC.
    pub fn json(&self) -> serde_json::Value {
        let mut ids: Vec<serde_json::Value> = self
            .entries
            .values()
            .map(|e| json!(display(&crate::legacy::Tx::hash(&e.tx))))
            .collect();
        ids.sort_by(|a, b| a.as_str().unwrap().cmp(b.as_str().unwrap()));
        json!(ids)
    }

    /// The txs to include in a minted block (fees attach to the coinstake).
    /// Sorted oldest-first, capped by the block's free size budget.
    pub fn txs_for_block(&self, max_bytes: usize, max_count: usize) -> Vec<(Hash, Tx)> {
        let mut list: Vec<(i64, Hash, &Tx)> = self
            .entries
            .iter()
            .map(|(h, e)| (e.first_seen, *h, &e.tx))
            .collect();
        list.sort_by_key(|(seen, _, _)| *seen);
        let mut out = Vec::new();
        let mut bytes = 0usize;
        for (_, h, tx) in list {
            if out.len() >= max_count {
                break;
            }
            let sz = tx.serialize().len();
            if bytes + sz > max_bytes {
                continue;
            }
            bytes += sz;
            out.push((h, tx.clone()));
        }
        out
    }
}

fn display(h: &Hash) -> String {
    crate::p2p::internal_to_display(h)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::{script_pubkey_p2pkh, sign_input, SpendInput};
    use std::collections::HashMap as StdMap;

    fn p2pkh_script() -> Vec<u8> {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk);
        script_pubkey_p2pkh(&pk.serialize())
    }

    struct OneUtxo {
        map: StdMap<(Hash, u32), PoolUtxo>,
    }

    impl UtxoView for OneUtxo {
        fn utxo(&self, key: &(Hash, u32)) -> Option<PoolUtxo> {
            self.map.get(key).cloned()
        }
    }

    fn funding_utxo() -> (OneUtxo, Hash, u32, PoolUtxo, [u8; 32]) {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk);
        let script = script_pubkey_p2pkh(&pk.serialize());
        let txid: Hash = sha256d(b"funding tx for txpool tests");
        let u = PoolUtxo {
            value: 2_000_000, // 2 TRI
            script_pubkey: script,
            height: 100,
            tx_time: 1_700_000_000,
        };
        let mut map = StdMap::new();
        map.insert((txid, 0), u.clone());
        (OneUtxo { map }, txid, 0, u, sk.secret_bytes())
    }

    fn signed_send(
        funding: &(Hash, u32, PoolUtxo, [u8; 32]),
        out_value: i64,
        fee: i64,
    ) -> (Vec<u8>, Tx) {
        // a signed 1-in-2-out spend built with the real spend.rs path
        let inp = SpendInput {
            txid: funding.0,
            n: funding.1,
            value: funding.2.value,
            script_pubkey: funding.2.script_pubkey.clone(),
            height: 100,
            tx_time: funding.2.tx_time,
            is_coinbase: false,
            is_coinstake: false,
            secret: funding.3,
            pubkey: pub_of(&funding.3),
        };
        let mut tx = Tx {
            version: 1,
            time: 1_700_000_100,
            vin: vec![crate::legacy::Vin {
                prev_hash: inp.txid,
                prev_n: inp.n,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![
                crate::legacy::Vout {
                    value: out_value,
                    script_pubkey: p2pkh_script(),
                },
                crate::legacy::Vout {
                    value: funding.2.value - out_value - fee,
                    script_pubkey: p2pkh_script(),
                },
            ],
            locktime: 0,
        };
        sign_input(&mut tx, 0, &inp.script_pubkey, &inp.secret, &inp.pubkey, 0).unwrap();
        (tx.serialize(), tx)
    }

    fn pub_of(secret: &[u8; 32]) -> Vec<u8> {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk = SecretKey::from_slice(secret).unwrap();
        PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk)
            .serialize()
            .to_vec()
    }

    #[test]
    fn accept_valid_tx_and_dedupe() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::default();
        let funding = (txid, n, utxo, secret);
        let (raw, _) = signed_send(&funding, 1_000_000, 5000);
        let rep = pool.accept(&raw, &view, 1_000).unwrap();
        assert_eq!(rep.fee_sats, 5000);
        assert_eq!(pool.len(), 1);
        // duplicate → idempotent
        let rep2 = pool.accept(&raw, &view, 1_001).unwrap();
        assert_eq!(rep2.txid, rep.txid);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn reject_unknown_prevout() {
        let (view, ..) = funding_utxo();
        let mut pool = TxPool::default();
        // signed tx against an input the view does not know
        let fake: Hash = sha256d(b"no such funding");
        let funding = (fake, 0, funding_utxo().3, funding_utxo().4);
        let (raw, _) = signed_send(&funding, 1_000_000, 5000);
        let e = pool.accept(&raw, &view, 1_000).unwrap_err();
        assert!(e.contains("not found in indexed prevouts"), "{}", e);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn reject_bad_signature() {
        let (view, txid, n, utxo, _secret) = funding_utxo();
        let mut pool = TxPool::default();
        let funding = (txid, n, utxo, [42u8; 32]);
        let (mut raw, _) = signed_send(&funding, 1_000_000, 5000);
        // tamper the scriptSig (flip a byte of the DER sig)
        let mut tx = Tx::read(&mut crate::legacy::Reader::new(&raw)).unwrap();
        tx.vin[0].script_sig[6] ^= 0x01;
        raw = tx.serialize();
        let e = pool.accept(&raw, &view, 1_000).unwrap_err();
        assert!(e.contains("input 0:"), "{}", e);
    }

    #[test]
    fn reject_fee_below_relay_floor() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::default();
        let funding = (txid, n, utxo, secret);
        // fee 0 → below MIN_RELAY_TX_FEE (100)
        let (raw, _) = signed_send(&funding, 1_000_000, 0);
        let e = pool.accept(&raw, &view, 1_000).unwrap_err();
        assert!(e.contains("below relay minimum"), "{}", e);
    }

    #[test]
    fn reject_out_of_range_and_over_spend() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::default();
        // over-spend: outputs exceed the 2 TRI input
        let funding = (txid, n, utxo.clone(), secret);
        let (raw, _) = signed_send(&funding, 3_000_000, 0);
        // signature is over this exact tx so it verifies, but value
        // conservation fails
        let e = pool.accept(&raw, &view, 1_000).unwrap_err();
        assert!(
            e.contains("spends more than its inputs") || e.contains("money range"),
            "{}",
            e
        );
    }

    #[test]
    fn reject_coinbase_relay() {
        let (view, ..) = funding_utxo();
        let mut pool = TxPool::default();
        let cb = Tx {
            version: 1,
            time: 1_700_000_100,
            vin: vec![crate::legacy::Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1, 5],
                sequence: u32::MAX,
            }],
            vout: vec![crate::legacy::Vout {
                value: 100,
                script_pubkey: p2pkh_script(),
            }],
            locktime: 0,
        };
        let e = pool.accept(&cb.serialize(), &view, 1_000).unwrap_err();
        assert!(e.contains("coinbase"), "{}", e);
    }

    #[test]
    fn pool_double_spend_guard_and_bounded_eviction() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::new(2, 3600);
        let funding = (txid, n, utxo, secret);
        let (raw1, _) = signed_send(&funding, 1_000_000, 5000);
        pool.accept(&raw1, &view, 1_000).unwrap();
        // a second spend of the same prevout (different outputs) conflicts
        let mut tx2 = Tx::read(&mut crate::legacy::Reader::new(&raw1)).unwrap();
        tx2.vout[0].value -= 1;
        sign_input(
            &mut tx2,
            0,
            &funding.2.script_pubkey,
            &funding.3,
            &pub_of(&funding.3),
            0,
        )
        .unwrap();
        let e = pool
            .accept(&tx2.serialize(), &view, 1_001)
            .unwrap_err();
        assert!(e.contains("conflict"), "{}", e);
        // bounded eviction: a fresh distinct utxo tx evicts the oldest
        // (build a second funding utxo with its own key)
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        let sk2 = SecretKey::from_slice(&[43u8; 32]).unwrap();
        let pk2 = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk2);
        let script2 = script_pubkey_p2pkh(&pk2.serialize());
        let txid2: Hash = sha256d(b"funding 2");
        let mut map2 = view.map.clone();
        map2.insert(
            (txid2, 0),
            PoolUtxo {
                value: 2_000_000,
                script_pubkey: script2.clone(),
                height: 100,
                tx_time: 1_700_000_000,
            },
        );
        let view2 = OneUtxo { map: map2 };
        let funding2 = (txid2, 0, PoolUtxo {
            value: 2_000_000,
            script_pubkey: script2,
            height: 100,
            tx_time: 1_700_000_000,
        }, sk2.secret_bytes());
        let (raw2, _) = signed_send(&funding2, 1_000_000, 5000);
        pool.accept(&raw2, &view2, 1_002).unwrap();
        assert_eq!(pool.len(), 2);
        let txid3: Hash = sha256d(b"funding 3");
        let sk3 = SecretKey::from_slice(&[44u8; 32]).unwrap();
        let pk3 = PublicKey::from_secret_key(&Secp256k1::signing_only(), &sk3);
        let script3 = script_pubkey_p2pkh(&pk3.serialize());
        let mut map3 = view.map.clone();
        map3.insert(
            (txid3, 0),
            PoolUtxo {
                value: 2_000_000,
                script_pubkey: script3.clone(),
                height: 100,
                tx_time: 1_700_000_000,
            },
        );
        let view3 = OneUtxo { map: map3 };
        let funding3 = (txid3, 0, PoolUtxo {
            value: 2_000_000,
            script_pubkey: script3,
            height: 100,
            tx_time: 1_700_000_000,
        }, sk3.secret_bytes());
        let (raw3, _) = signed_send(&funding3, 1_000_000, 5000);
        pool.accept(&raw3, &view3, 1_003).unwrap();
        assert_eq!(pool.len(), 2, "cap enforced by oldest eviction");
    }

    #[test]
    fn prune_and_confirm_spend() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::default();
        let funding = (txid, n, utxo, secret);
        let (raw, _) = signed_send(&funding, 1_000_000, 5000);
        pool.accept(&raw, &view, 1_000).unwrap();
        // ttl prune
        assert_eq!(pool.prune(1_000 + 10), 0);
        assert_eq!(pool.prune(1_000 + 3601), 1);
        assert!(pool.is_empty());
        // confirm_spend clears entries whose inputs just got mined
        pool.accept(&raw, &view, 5_000).unwrap();
        pool.confirm_spend(&[(txid, n)]);
        assert!(pool.is_empty());
    }

    #[test]
    fn txs_for_block_oldest_first_and_size_cap() {
        let (view, txid, n, utxo, secret) = funding_utxo();
        let mut pool = TxPool::default();
        let funding = (txid, n, utxo, secret);
        let (raw1, _) = signed_send(&funding, 1_000_000, 5000);
        pool.accept(&raw1, &view, 1_000).unwrap();
        let picked = pool.txs_for_block(usize::MAX, 10);
        assert_eq!(picked.len(), 1);
        let picked = pool.txs_for_block(0, 10);
        assert!(picked.is_empty());
    }
}
