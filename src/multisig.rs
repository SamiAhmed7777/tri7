//! Phase 3b: m-of-n P2SH multisig — redeem-script construction, parsing,
//! partial signing and scriptSig assembly.
//!
//! Exact port of the legacy pieces (triangles_v5):
//!   - CScript::SetMultisig (script.h:604-619): OP_m <push pk>… OP_n
//!     OP_CHECKMULTISIG with EncodeOP_N (0x51..0x60 = OP_1..OP_16); the
//!     sanity checks are "nRequired >= 1", "keys.size() <= 16",
//!     "nRequired <= keys.size()" — mirrored by `redeem_script`.
//!   - CScriptID / CTrianglesAddress(scriptID) (script.h:36-38, base58.h
//!     SCRIPT_ADDRESS = 28): the P2SH address is
//!     base58check(0x1c ‖ hash160(redeem)) — `p2sh_address` rides the
//!     crate::address codec, nothing is hand-rolled.
//!   - CWallet::AddCScript + SignN flow (wallet.cpp:190, script.cpp
//!     SignSignature/SignN): each key the wallet holds signs the SAME
//!     sighash (SignatureHash over scriptCode = the redeem script), and the
//!     scriptSig is the m signatures in pubkey order preceded by the OP_0
//!     CHECKMULTISIG dummy — `sign_multisig_partial` +
//!     `assemble_multisig_scriptsig`.
//!
//! All signatures produced here are SIGHASH_ALL (nHashType = 1) — the only
//! hashtype the legacy wallet's SignSignature passes.

use crate::spend::{push_data, signature_hash, sign_msg, SIGHASH_ALL};

/// OP_1..OP_16 (script.h opcodetype): small-int encodings used for m and n.
pub const OP_1: u8 = 0x51;
pub const OP_16: u8 = 0x60;

/// OP_CHECKMULTISIG (script.h) — the raw-tx decoder table (rawtx.rs) carries
/// the same byte (0xae).
pub const OP_CHECKMULTISIG: u8 = 0xae;

fn op_to_small_int(op: u8) -> Option<u8> {
    match op {
        OP_1..=OP_16 => Some(op - (OP_1 - 1)),
        _ => None,
    }
}

fn small_int_to_op(n: u8) -> u8 {
    // caller guarantees 1..=16 (EncodeOP_N, script.h)
    OP_1 + (n - 1)
}

/// Build the m-of-n multisig redeem script: OP_m <push pk1> … <push pkn>
/// OP_n OP_CHECKMULTISIG (CScript::SetMultisig). Every pubkey must be a
/// valid secp256k1 point (33-byte compressed or 65-byte uncompressed — the
/// minimal-push prefix is derived from the length, 0x21 for 33 bytes).
///
/// Rejects: m == 0 ("a multisignature address must require at least one key
/// to redeem"), m > n, n > 16 (the legacy sanity checks), and any pubkey
/// that does not load as a secp256k1 point.
pub fn redeem_script(m: u8, pubkeys: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let n = pubkeys.len();
    if m == 0 {
        return Err("a multisignature address must require at least one key to redeem".into());
    }
    if m as usize > n {
        return Err(format!(
            "not enough keys supplied (got {} keys, but need at least {} to redeem)",
            n, m
        ));
    }
    if n > 16 {
        return Err(format!("too many keys supplied (got {}, but only 16 are allowed)", n));
    }
    for (i, pk) in pubkeys.iter().enumerate() {
        secp256k1::PublicKey::from_slice(pk)
            .map_err(|e| format!("invalid public key #{} ({} bytes): {}", i + 1, pk.len(), e))?;
    }
    let mut s = Vec::with_capacity(1 + pubkeys.iter().map(|p| p.len() + 1).sum::<usize>() + 2);
    s.push(small_int_to_op(m));
    for pk in pubkeys {
        push_data(&mut s, pk);
    }
    s.push(small_int_to_op(n as u8));
    s.push(OP_CHECKMULTISIG);
    Ok(s)
}

/// The P2SH address of a redeem script — base58check(SCRIPT_ADDRESS = 28 ‖
/// hash160(script)), the legacy CTrianglesAddress(CScriptID). Version 28 is
/// the 'C' address class (crate::address::SCRIPT_ADDRESS).
pub fn p2sh_address(script: &[u8]) -> String {
    crate::address::payload_to_address(&crate::address::script_hash_payload(script))
}

/// Inverse of `redeem_script`: parse OP_m <pushes…> OP_n OP_CHECKMULTISIG
/// back into (m, pubkeys). Used by the P2SH verification path (spend.rs)
/// and by tests. Every push must be a valid secp256k1 point, the push count
/// must equal n, and m must be 1..=n — anything else is not a standard
/// multisig redeem script.
pub fn parse_multisig(script: &[u8]) -> Result<(u8, Vec<Vec<u8>>), String> {
    if script.len() < 4 {
        return Err("not a multisig script (too short)".into());
    }
    let m = op_to_small_int(script[0])
        .ok_or_else(|| format!("not a multisig script (first opcode {:#04x} is not OP_1..OP_16)", script[0]))?;
    if script[script.len() - 1] != OP_CHECKMULTISIG {
        return Err(format!(
            "not a multisig script (last opcode {:#04x} is not OP_CHECKMULTISIG)",
            script[script.len() - 1]
        ));
    }
    let n = op_to_small_int(script[script.len() - 2])
        .ok_or_else(|| format!("not a multisig script (opcode {:#04x} is not OP_1..OP_16)", script[script.len() - 2]))?;
    let pushes = crate::spend::parse_pushes(&script[1..script.len() - 2])
        .map_err(|e| format!("not a multisig script (bad pubkey pushes): {}", e))?;
    if pushes.len() != n as usize {
        return Err(format!(
            "multisig script declares n={} but carries {} pubkeys",
            n,
            pushes.len()
        ));
    }
    if m > n {
        return Err(format!("multisig script requires m={} but only has n={} pubkeys", m, n));
    }
    for (i, pk) in pushes.iter().enumerate() {
        secp256k1::PublicKey::from_slice(pk)
            .map_err(|e| format!("invalid public key #{} ({} bytes): {}", i + 1, pk.len(), e))?;
    }
    Ok((m, pushes))
}

/// Sign one input of `tx` with every key we hold, returning one slot per
/// pubkey in the redeem script (pubkey order) — Some(DER sig ‖ SIGHASH_ALL)
/// where `keys` carries that pubkey, None elsewhere. The sighash is the
/// legacy SignatureHash over scriptCode = the REDEEM script (never the P2SH
/// scriptPubKey) with nHashType = SIGHASH_ALL — every signature binds the
/// same digest, so any m of the filled slots assemble into a valid spend.
///
/// `keys` = (secret, pubkey) pairs the wallet holds (synthetic in tests).
pub fn sign_multisig_partial(
    tx: &crate::legacy::Tx,
    n_in: usize,
    redeem_script: &[u8],
    keys: &[([u8; 32], Vec<u8>)],
) -> Result<Vec<Option<Vec<u8>>>, String> {
    if n_in >= tx.vin.len() {
        return Err(format!(
            "SignN: nIn={} out of range (vin {})",
            n_in,
            tx.vin.len()
        ));
    }
    let (_m, pubkeys) = parse_multisig(redeem_script)?;
    let sighash = signature_hash(tx, n_in, redeem_script, SIGHASH_ALL)?;
    let mut out: Vec<Option<Vec<u8>>> = Vec::with_capacity(pubkeys.len());
    for pk in &pubkeys {
        let mut slot = None;
        for (secret, key_pk) in keys {
            if key_pk == pk {
                let mut der = sign_msg(secret, &sighash)?;
                der.push(SIGHASH_ALL as u8);
                slot = Some(der);
                break;
            }
        }
        out.push(slot);
    }
    Ok(out)
}

/// Assemble the final P2SH multisig scriptSig from the partial slots:
/// OP_0 <sig1> … <sigM> <redeemScript> — signatures in PUBKEY ORDER (the
/// CHECKMULTISIG matching rule: each sig must match a pubkey at or after
/// the last matched index) with the leading OP_0 dummy the opcode's
/// historical off-by-one pops and ignores.
///
/// m and the pubkey count are derived from `redeem` — the caller supplies
/// NOTHING. Fails unless `sigs.len()` equals the redeem script's pubkey
/// count and at least m slots are filled.
pub fn assemble_multisig_scriptsig(
    sigs: &[Option<Vec<u8>>],
    redeem: &[u8],
) -> Result<Vec<u8>, String> {
    let (m, pubkeys) = parse_multisig(redeem)?;
    if sigs.len() != pubkeys.len() {
        return Err(format!(
            "signature slot count {} does not match the redeem script's {} pubkeys",
            sigs.len(),
            pubkeys.len()
        ));
    }
    let present = sigs.iter().flatten().count();
    if present < m as usize {
        return Err(format!(
            "missing signatures: have {} but need at least {} to redeem",
            present, m
        ));
    }
    let mut s = Vec::new();
    s.push(0x00); // OP_0 — the CHECKMULTISIG dummy
    for sig in sigs.iter().flatten() {
        push_data(&mut s, sig);
    }
    push_data(&mut s, redeem);
    Ok(s)
}

// ---------------------------------------------------------------------------
// ScriptBook — the wallet's multisig context: which redeem scripts we hold
// and which of each script's keys we have. Built once per spend flow and
// threaded through selection/ownership/signing so P2SH coins are only ever
// adopted when the wallet can actually produce m signatures.
// ---------------------------------------------------------------------------
pub struct ScriptBook {
    pub scripts: std::collections::HashMap<[u8; 20], Vec<u8>>, // scriptID -> redeem
    pub keys: std::collections::HashMap<Vec<u8>, [u8; 32]>,    // pubkey  -> secret
}

impl ScriptBook {
    /// A book that owns nothing: every P2SH prevout fails `can_spend`, so
    /// book-less code paths refuse multisig coins by construction.
    pub fn empty() -> Self {
        ScriptBook {
            scripts: std::collections::HashMap::new(),
            keys: std::collections::HashMap::new(),
        }
    }

    pub fn from_scripts(scripts: &[([u8; 20], Vec<u8>)], keys: &[([u8; 32], Vec<u8>)]) -> Self {
        ScriptBook {
            scripts: scripts.iter().cloned().collect(),
            keys: keys.iter().map(|(s, pk)| (pk.clone(), *s)).collect(),
        }
    }

    /// Build the book from the wallet reader output (redeem scripts) plus
    /// the (secret, pubkey) pairs the caller can sign with (wallet keys +
    /// any keystore entries — the reader alone may not carry every signing
    /// key a flow has).
    pub fn from_wallet(w: &crate::wallet::WalletKeys, keys: &[([u8; 32], Vec<u8>)]) -> Self {
        Self::from_scripts(&w.scripts, keys)
    }

    /// The stored redeem script for a P2SH scriptPubKey, if we hold it.
    pub fn redeem_for(&self, script_pubkey: &[u8]) -> Option<&Vec<u8>> {
        match crate::spend::solver_type(script_pubkey) {
            Some(crate::spend::SpendType::ScriptHash(h)) => self.scripts.get(&h),
            _ => None,
        }
    }

    /// The (secret, pubkey) pairs we hold for the pubkeys of `redeem`, in
    /// redeem-script (pubkey) order — exactly the input `sign_multisig_partial`
    /// wants. Unparseable scripts yield no keys.
    pub fn held_keys(&self, redeem: &[u8]) -> Vec<([u8; 32], Vec<u8>)> {
        parse_multisig(redeem)
            .map(|(_, pks)| {
                pks.iter()
                    .filter_map(|pk| self.keys.get(pk).map(|s| (*s, pk.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether this book can spend a P2SH prevout: the redeem script must be
    /// present, hash160(redeem) must equal the scriptPubKey hash, and the
    /// number of held keys must be >= m. Non-P2SH scripts need no book and
    /// pass vacuously.
    pub fn can_spend(&self, script_pubkey: &[u8]) -> Result<(), String> {
        let h = match crate::spend::solver_type(script_pubkey) {
            Some(crate::spend::SpendType::ScriptHash(h)) => h,
            _ => return Ok(()), // not P2SH — single-key ownership is checked elsewhere
        };
        let redeem = self.scripts.get(&h).ok_or_else(|| {
            format!(
                "no redeem script in the wallet for scriptID {}",
                h.iter().map(|b| format!("{:02x}", b)).collect::<String>()
            )
        })?;
        if crate::address::hash160(redeem) != h {
            return Err("stored redeem script does not match the scriptPubKey hash".into());
        }
        let (m, _pubkeys) = parse_multisig(redeem)
            .map_err(|e| format!("stored redeem script is not a standard multisig: {}", e))?;
        let held = self.held_keys(redeem);
        if held.len() < m as usize {
            return Err(format!(
                "only {} of {} required keys held",
                held.len(),
                m
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    fn three_keys() -> Vec<Vec<u8>> {
        ["ms/a", "ms/b", "ms/c"]
            .iter()
            .map(|l| crate::spend::synthetic_key(l).1)
            .collect()
    }

    // T1: redeem_script(2, 3 keys) is exactly OP_2 <pk1> <pk2> <pk3> OP_3
    // OP_CHECKMULTISIG = 52 21 <pk1> 21 <pk2> 21 <pk3> 53 ae.
    #[test]
    fn t1_redeem_script_two_of_three_exact_bytes() {
        let pks = three_keys();
        let redeem = redeem_script(2, &pks).unwrap();
        let mut expect = vec![0x52];
        for pk in &pks {
            expect.push(0x21);
            expect.extend_from_slice(pk);
        }
        expect.push(0x53);
        expect.push(0xae);
        assert_eq!(redeem, expect);
        assert_eq!(redeem.len(), 105, "measured length: {}", redeem.len());
        println!("T1 redeem_script(2,3) hex: {}", hex(&redeem));
    }

    // T2: the P2SH address starts with 'C' (version 28) and is stable.
    #[test]
    fn t2_p2sh_address_is_version_28_and_stable() {
        let pks = three_keys();
        let redeem = redeem_script(2, &pks).unwrap();
        let addr = p2sh_address(&redeem);
        assert!(addr.starts_with('C'), "P2SH address must start with C: {}", addr);
        assert_eq!(addr, p2sh_address(&redeem), "same script → same address");
        // and it is the decodescript-style base58check of the script hash
        let payload = crate::address::address_to_payload(&addr).unwrap();
        assert_eq!(payload[0], crate::address::SCRIPT_ADDRESS);
        assert_eq!(&payload[1..], &crate::address::hash160(&redeem));
        println!("T2 P2SH address: {}", addr);
    }

    // T9: parse_multisig is the exact inverse of redeem_script.
    #[test]
    fn t9_parse_multisig_roundtrips() {
        let pks = three_keys();
        let redeem = redeem_script(2, &pks).unwrap();
        let (m, back) = parse_multisig(&redeem).unwrap();
        assert_eq!(m, 2);
        assert_eq!(back, pks);
        // boundary: 1-of-1 and 16-of-16 shapes
        let (m1, b1) = parse_multisig(&redeem_script(1, &pks[..1]).unwrap()).unwrap();
        assert_eq!((m1, b1.len()), (1, 1));
        let all: Vec<Vec<u8>> = (0..16)
            .map(|i| crate::spend::synthetic_key(&format!("ms/16/{}", i)).1)
            .collect();
        let (m16, b16) = parse_multisig(&redeem_script(16, &all).unwrap()).unwrap();
        assert_eq!((m16, b16.len()), (16, 16));
        // non-multisig scripts are rejected
        assert!(parse_multisig(&[0x00, 0x51, 0xae]).is_err(), "OP_0 m is not standard");
        assert!(parse_multisig(&[0x52, 0x21, 0x02, 0x53]).is_err(), "missing CHECKMULTISIG");
        assert!(parse_multisig(&[0x52, 0x53, 0xae]).is_err(), "no pubkeys");
        // wrong count: 2-of-3 shape but OP_2 tail
        let mut bad = vec![0x52u8];
        for pk in &pks {
            bad.push(0x21);
            bad.extend_from_slice(pk);
        }
        bad.extend_from_slice(&[0x52, 0xae]); // OP_2 tail, 3 pushes
        assert!(parse_multisig(&bad).is_err(), "declared n must match pushes");
        // junk pubkey point
        let junk = vec![vec![0x02u8; 33], vec![0x03u8; 33], vec![0x05u8; 33]];
        assert!(redeem_script(2, &junk).is_err(), "invalid point must be rejected");
    }

    #[test]
    fn redeem_script_rejects_bad_parameters() {
        let pks = three_keys();
        assert!(redeem_script(0, &pks).is_err(), "m == 0");
        assert!(redeem_script(4, &pks).is_err(), "m > n");
        let many: Vec<Vec<u8>> = (0..17)
            .map(|i| crate::spend::synthetic_key(&format!("ms/17/{}", i)).1)
            .collect();
        assert!(redeem_script(2, &many).is_err(), "n > 16");
        // uncompressed keys are accepted (65-byte pushes, 0x41 prefix)
        let sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        let big = secp256k1::PublicKey::from_secret_key(&secp, &sk)
            .serialize_uncompressed()
            .to_vec();
        let redeem = redeem_script(1, &[big]).unwrap();
        assert_eq!(redeem[0], 0x51);
        assert_eq!(redeem[1], 0x41);
        assert_eq!(*redeem.last().unwrap(), 0xae);
    }
}
