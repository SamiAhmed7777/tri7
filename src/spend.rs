//! Phase 3: wallet transaction creation and signing (spends).
//!
//! Exact port of the legacy signing stack from triangles_v5 v6.2.6.7:
//!   - SignatureHash        — script.cpp:1150-1210 (the 0.1.6-era scheme:
//!     serialize the tx with every input's scriptSig blanked except nIn,
//!     which carries scriptCode, append nHashType as a 4-byte LE int,
//!     double-SHA256)
//!   - CScript::FindAndDelete — script.h:485-502 (GetOp-walking byte-pattern
//!     erase; used for the OP_CODESEPARATOR strip inside SignatureHash)
//!   - CheckSig             — script.cpp:1285-1323 (hashtype byte is the
//!     signature's trailing byte when the caller passes nHashType=0)
//!   - Solver               — script.cpp:1336-1513 (scriptPubKey templates +
//!     scriptSig construction: PubKey → 1 push [sig], PubKeyHash → 2 pushes
//!     [sig, pubkey]; ScriptSigArgsExpected script.cpp:1515-1533)
//!   - CreateTransaction    — wallet.cpp:1979-2115 (fee loop, change output,
//!     per-input SignSignature, size limit MAX_BLOCK_SIZE_GEN/5)
//!   - ConnectInputs        — main.cpp:1980-2067 (maturity, tx-time ordering,
//!     MoneyRange, value-in >= value-out, fee >= GetMinFee)
//!   - GetMinFee            — main.cpp:1000-1027; MoneyRange main.h:65;
//!     MIN_TX_FEE = CENT/100 (main.h:58, CENT = 10000 in util.h:53,
//!     COIN = 1000000); nTransactionFee default = MIN_TX_FEE
//!     (main.cpp:450); nCoinbaseMaturity = 7 mainnet (main.cpp:65);
//!     MAX_BLOCK_SIZE_GEN = MAX_BLOCK_SIZE/2 = 500000 (main.h:39-40).
//!
//! Every function mirrors the C++ semantics 1:1; byte-exact behavior is the
//! requirement — "improvements" to the signature scheme are forbidden. The
//! SignatureHash port is anchored empirically by the historical-signature
//! corpus (tests + historical_sig_corpus.rs): real mint-era signatures from
//! blk0001.dat must verify against this implementation.

use crate::legacy::{sha256d, Hash, Tx, Vin, Vout};

// ---------------------------------------------------------------------------
// Constants (main.h / util.h / main.cpp — see module doc)
// ---------------------------------------------------------------------------
pub const SIGHASH_ALL: i32 = 1;
pub const SIGHASH_NONE: i32 = 2;
pub const SIGHASH_SINGLE: i32 = 3;
pub const SIGHASH_ANYONECANPAY: i32 = 0x80;

pub const COIN: i64 = crate::consensus::COIN; // 1,000,000 (util.h:52)
pub const CENT: i64 = 10_000; // util.h:53
pub const MIN_TX_FEE: i64 = CENT / 100; // 100 (main.h:58)
pub const MIN_RELAY_TX_FEE: i64 = CENT / 100; // 100 (main.h:59)
pub const MAX_MONEY: i64 = 2_222_222 * COIN; // main.h:60
pub const COINBASE_MATURITY: i32 = 7; // nCoinbaseMaturity, main.cpp:65
pub const MAX_BLOCK_SIZE: usize = 1_000_000; // main.h:39
pub const MAX_BLOCK_SIZE_GEN: usize = MAX_BLOCK_SIZE / 2; // main.h:40
pub const DEFAULT_TRANSACTION_FEE: i64 = MIN_TX_FEE; // nTransactionFee init, main.cpp:450
pub const OP_CODESEPARATOR: u8 = 0xab;
/// OP_CHECKSIG / OP_EQUALVERIFY / OP_DUP / OP_HASH160 (script.h enum).
pub const OP_CHECKSIG: u8 = 0xac;
pub const OP_EQUALVERIFY: u8 = 0x88;
pub const OP_DUP: u8 = 0x76;
pub const OP_HASH160: u8 = 0xa9;

pub fn money_range(v: i64) -> bool {
    // MoneyRange (main.h:65)
    (0..=MAX_MONEY).contains(&v)
}

// ---------------------------------------------------------------------------
// CScript push encoding (CScript::operator<<(const std::vector<unsigned
// char>&), script.h) — minimal push: <0x4b direct, else OP_PUSHDATA1.
// ---------------------------------------------------------------------------
pub fn push_data(w: &mut Vec<u8>, data: &[u8]) {
    if data.len() < 0x4c {
        w.push(data.len() as u8);
    } else if data.len() <= 0xff {
        w.push(0x4c); // OP_PUSHDATA1 + one length byte
        w.push(data.len() as u8);
    } else if data.len() <= 0xffff {
        w.push(0x4d); // OP_PUSHDATA2 + two LE length bytes
        w.extend_from_slice(&(data.len() as u16).to_le_bytes());
    } else {
        w.push(0x4e); // OP_PUSHDATA4 + four LE length bytes
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    w.extend_from_slice(data);
}

/// Script builder helpers (Solver templates).
pub fn script_pubkey_p2pk(pubkey: &[u8]) -> Result<Vec<u8>, String> {
    // Solver PubKey template: CScript() << OP_PUBKEY << OP_CHECKSIG, where the
    // pubkey must itself load as a CKey (33 compressed 02/03 or 65
    // uncompressed 04) — same validation bare_pubkey_from_script enforces.
    let ok = pubkey.len() == 33 && (pubkey[0] == 2 || pubkey[0] == 3)
        || pubkey.len() == 65 && pubkey[0] == 4;
    if !ok {
        return Err(format!("p2pk: invalid pubkey form ({} bytes)", pubkey.len()));
    }
    let mut s = Vec::with_capacity(pubkey.len() + 2);
    push_data(&mut s, pubkey);
    s.push(OP_CHECKSIG);
    Ok(s)
}

pub fn script_pubkey_p2pkh(pubkey: &[u8]) -> Vec<u8> {
    // Solver PubKeyHash template: OP_DUP OP_HASH160 <h160> OP_EQUALVERIFY
    // OP_CHECKSIG = 76 a9 14 <20> 88 ac.
    let h = crate::address::hash160(pubkey);
    let mut s = Vec::with_capacity(25);
    s.push(OP_DUP);
    s.push(OP_HASH160);
    s.push(0x14);
    s.extend_from_slice(&h);
    s.push(OP_EQUALVERIFY);
    s.push(OP_CHECKSIG);
    s
}

/// scriptSig for a PubKey (bare P2PK) prevout: single push of [DER sig ||
/// hashtype] (Solver, script.cpp:1491-1493 — Sign1 pushes one element).
pub fn script_sig_p2pk(sig_with_hashtype: &[u8]) -> Vec<u8> {
    let mut s = Vec::with_capacity(sig_with_hashtype.len() + 2);
    push_data(&mut s, sig_with_hashtype);
    s
}

/// scriptSig for a PubKeyHash (P2PKH) prevout: [sig || hashtype] then the
/// pubkey (Solver, script.cpp:1494-1504 — Sign1 then `scriptSigRet << vch`).
pub fn script_sig_p2pkh(sig_with_hashtype: &[u8], pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::with_capacity(sig_with_hashtype.len() + pubkey.len() + 4);
    push_data(&mut s, sig_with_hashtype);
    push_data(&mut s, pubkey);
    s
}

// ---------------------------------------------------------------------------
// CScript::FindAndDelete (script.h:485-502) for a single-opcode needle.
// The C++ walks instructions with GetOp and, at every instruction boundary,
// erases repeated occurrences of the needle's byte pattern. A truncated /
// malformed tail stops the GetOp walk and is left untouched.
// ---------------------------------------------------------------------------
pub fn find_and_delete_op(script: &[u8], needle: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(script.len());
    let mut pc = 0usize;
    loop {
        // erase needle repeats at this instruction boundary
        while pc < script.len() && script[pc] == needle {
            pc += 1;
        }
        if pc >= script.len() {
            return out;
        }
        // GetOp: parse one instruction (script.h GetOp / GetOpData rules)
        let opcode = script[pc];
        let start = pc;
        pc += 1;
        let dlen: Option<usize> = match opcode {
            0x01..=0x4b => Some(opcode as usize),
            0x4c if pc < script.len() => {
                let l = script[pc] as usize;
                pc += 1;
                Some(l)
            }
            0x4d if pc + 2 <= script.len() => {
                let l = u16::from_le_bytes([script[pc], script[pc + 1]]) as usize;
                pc += 2;
                Some(l)
            }
            0x4e if pc + 4 <= script.len() => {
                let l =
                    u32::from_le_bytes([script[pc], script[pc + 1], script[pc + 2], script[pc + 3]])
                        as usize;
                pc += 4;
                Some(l)
            }
            0x4c | 0x4d | 0x4e => {
                // GetOp fails on truncated length prefix: emit the rest and stop
                out.extend_from_slice(&script[start..]);
                return out;
            }
            _ => None,
        };
        match dlen {
            None => out.push(opcode),
            Some(l) => {
                if pc + l > script.len() {
                    // GetOp fails on truncated push data: rest stays verbatim
                    out.extend_from_slice(&script[start..]);
                    return out;
                }
                out.push(opcode);
                out.extend_from_slice(&script[pc..pc + l]);
                pc += l;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SignatureHash (script.cpp:1150-1210) — THE correctness anchor.
// ---------------------------------------------------------------------------
pub fn signature_hash(
    tx_to: &Tx,
    n_in: usize,
    script_code: &[u8],
    hash_type: i32,
) -> Result<Hash, String> {
    if n_in >= tx_to.vin.len() {
        // C++ prints an error and returns uint256(1); callers treat any hash
        // mismatch as failure, so an Err is the faithful Rust expression of
        // "no signature can verify here".
        return Err(format!(
            "SignatureHash() : nIn={} out of range (vin {})",
            n_in,
            tx_to.vin.len()
        ));
    }
    let mut tmp = tx_to.clone();

    // scriptCode.FindAndDelete(CScript(OP_CODESEPARATOR)) (script.cpp:1161)
    let script_code = find_and_delete_op(script_code, OP_CODESEPARATOR);

    // Blank out other inputs' signatures; nIn carries scriptCode
    for (i, vin) in tmp.vin.iter_mut().enumerate() {
        vin.script_sig = Vec::new();
        if i == n_in {
            vin.script_sig = script_code.clone();
        }
    }

    match hash_type & 0x1f {
        SIGHASH_NONE => {
            // Wildcard payee; others update at will
            tmp.vout.clear();
            for (i, vin) in tmp.vin.iter_mut().enumerate() {
                if i != n_in {
                    vin.sequence = 0;
                }
            }
        }
        SIGHASH_SINGLE => {
            // Only lock in the payee at the same index as the input
            let n_out = n_in;
            if n_out >= tmp.vout.len() {
                return Err(format!(
                    "SignatureHash() : nOut={} out of range (vout {})",
                    n_out,
                    tmp.vout.len()
                ));
            }
            tmp.vout.truncate(n_out + 1);
            for o in tmp.vout.iter_mut().take(n_out) {
                // CTxOut::SetNull() (main.h:411-415): nValue = -1, empty script
                o.value = -1;
                o.script_pubkey.clear();
            }
            for (i, vin) in tmp.vin.iter_mut().enumerate() {
                if i != n_in {
                    vin.sequence = 0;
                }
            }
        }
        _ => {}
    }

    if hash_type & SIGHASH_ANYONECANPAY != 0 {
        let only = tmp.vin[n_in].clone();
        tmp.vin = vec![only];
    }

    // ss << txTmp << nHashType; Hash() = double-SHA256
    let mut buf = tmp.serialize();
    buf.extend_from_slice(&hash_type.to_le_bytes());
    Ok(sha256d(&buf))
}

// ---------------------------------------------------------------------------
// DER ECDSA signature encode/decode (canonical = libsecp256k1 rules; the lax
// decoder mirrors the OpenSSL-era acceptance that pre-strict-ECDSA mint
// clients relied on — the "9003-era sigs" class, root-caused in the handoff).
// ---------------------------------------------------------------------------
pub fn signature_components_der(r: [u8; 32], s: [u8; 32]) -> Vec<u8> {
    // libsecp serialize_der: minimal-length big-endian INTEGERs with a 0x00
    // pad only when the high bit is set.
    let enc_int = |v: &[u8; 32], out: &mut Vec<u8>| {
        let mut start = 0usize;
        while start < 32 && v[start] == 0 {
            start += 1;
        }
        if start == 32 {
            // zero encodes as 0x01 per libsecp convention
            out.push(0x02);
            out.push(0x01);
            out.push(0x01);
            return;
        }
        let mut len = 32 - start;
        let pad = if v[start] & 0x80 != 0 { 1 } else { 0 };
        out.push(0x02);
        out.push((len + pad) as u8);
        if pad == 1 {
            out.push(0x00);
        }
        out.extend_from_slice(&v[start..]);
    };
    let mut body = Vec::with_capacity(72);
    enc_int(&r, &mut body);
    enc_int(&s, &mut body);
    let mut out = vec![0x30, body.len() as u8];
    out.extend_from_slice(&body);
    out
}

fn der_read_int(body: &[u8], idx: &mut usize, strict: bool) -> Result<[u8; 32], String> {
    if *idx + 2 > body.len() || body[*idx] != 0x02 {
        return Err(format!("DER: expected INTEGER at {}", idx));
    }
    let len = body[*idx + 1] as usize;
    *idx += 2;
    if *idx + len > body.len() || len == 0 {
        return Err("DER: bad INTEGER length".into());
    }
    let v = &body[*idx..*idx + len];
    *idx += len;
    // Leading zero allowed only to keep the value positive (high bit clear).
    let mut start = 0usize;
    if v[0] == 0x00 {
        if strict {
            if len == 1 {
                // zero encodes as 0x01 per libsecp; a 1-byte 0x00 is invalid
                return Err("DER: non-minimal INTEGER (00)".into());
            }
            if v[1] & 0x80 == 0 {
                return Err("DER: non-minimal INTEGER (unneeded pad)".into());
            }
        }
        start = 1;
    } else if strict && v[0] & 0x80 != 0 {
        return Err("DER: negative INTEGER".into());
    }
    let mut r = [0u8; 32];
    let val = &v[start..];
    if val.len() > 32 {
        return Err("DER: INTEGER overflows 32 bytes".into());
    }
    r[32 - val.len()..].copy_from_slice(val);
    Ok(r)
}

/// Strict DER decode — mirrors secp256k1_ecdsa_signature_parse_der rules
/// (minimal-length, positive INTEGERs, exact total length).
pub fn decode_signature_der_strict(data: &[u8]) -> Result<([u8; 32], [u8; 32]), String> {
    if data.len() < 2 || data[0] != 0x30 {
        return Err("DER: missing SEQUENCE".into());
    }
    if data[1] as usize + 2 != data.len() {
        return Err("DER: SEQUENCE length mismatch".into());
    }
    let body = &data[2..];
    let mut idx = 0usize;
    let r = der_read_int(body, &mut idx, true)?;
    let s = der_read_int(body, &mut idx, true)?;
    if idx != body.len() {
        return Err("DER: trailing bytes after s".into());
    }
    Ok((r, s))
}

/// Lax decode — OpenSSL-era acceptance (non-minimal lengths/padding,
/// negative-padded values, trailing bytes tolerated).
pub fn decode_signature_der_lax(data: &[u8]) -> Result<([u8; 32], [u8; 32]), String> {
    if data.len() < 2 || data[0] != 0x30 {
        return Err("DER: missing SEQUENCE".into());
    }
    let total = data[1] as usize + 2;
    let total = if total > data.len() { data.len() } else { total };
    let body = &data[2..total];
    let mut idx = 0usize;
    let r = der_read_int(body, &mut idx, false)?;
    let s = der_read_int(body, &mut idx, false)?;
    Ok((r, s))
}

// ---------------------------------------------------------------------------
// Sign + verify
// ---------------------------------------------------------------------------
static SECP: std::sync::OnceLock<secp256k1::Secp256k1<secp256k1::All>> = std::sync::OnceLock::new();

fn secp_ctx() -> &'static secp256k1::Secp256k1<secp256k1::All> {
    SECP.get_or_init(secp256k1::Secp256k1::new)
}

/// CKey::Sign equivalent: RFC6979-deterministic ECDSA over `msg`, returned as
/// canonical DER (matching libsecp serialize_der, which is what
/// ECDSA_do_sign's minimal DER output looks like for these keys in practice —
/// and, per verification, is what the script layer accepts).
pub fn sign_msg(secret: &[u8; 32], msg: &Hash) -> Result<Vec<u8>, String> {
    let sk = secp256k1::SecretKey::from_slice(secret).map_err(|e| format!("bad secret: {}", e))?;
    let m = secp256k1::Message::from_digest(*msg);
    let sig = secp_ctx().sign_ecdsa(&m, &sk);
    Ok(sig.serialize_der().to_vec())
}

pub fn verify_msg(pubkey: &[u8], sig_der: &[u8], msg: &Hash) -> Result<(), String> {
    let pk = secp256k1::PublicKey::from_slice(pubkey).map_err(|e| format!("bad pubkey: {}", e))?;
    let m = secp256k1::Message::from_digest(*msg);
    let sig = secp256k1::ecdsa::Signature::from_der(sig_der)
        .map_err(|e| format!("bad DER signature: {}", e))?;
    secp_ctx()
        .verify_ecdsa(&m, &sig, &pk)
        .map_err(|e| format!("ECDSA verify failed: {}", e))
}

/// Parse a scriptSig that must be push-only (EvalScript's stack setup for the
/// standard templates). OP_0 pushes the empty vector; 0x01..0x4e are data
/// pushes; ANY other opcode makes the parse fail (the template match below
/// would fail in EvalScript anyway for non-standard spend scripts).
pub fn parse_pushes(script: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < script.len() {
        let op = script[pc];
        pc += 1;
        match op {
            0x00 => out.push(Vec::new()),
            0x01..=0x4b => {
                let l = op as usize;
                if pc + l > script.len() {
                    return Err(format!("push opcode {} overruns script", op));
                }
                out.push(script[pc..pc + l].to_vec());
                pc += l;
            }
            0x4c => {
                if pc >= script.len() {
                    return Err("OP_PUSHDATA1 truncated".into());
                }
                let l = script[pc] as usize;
                pc += 1;
                if pc + l > script.len() {
                    return Err("OP_PUSHDATA1 data overruns script".into());
                }
                out.push(script[pc..pc + l].to_vec());
                pc += l;
            }
            0x4d => {
                if pc + 2 > script.len() {
                    return Err("OP_PUSHDATA2 truncated".into());
                }
                let l = u16::from_le_bytes([script[pc], script[pc + 1]]) as usize;
                pc += 2;
                if pc + l > script.len() {
                    return Err("OP_PUSHDATA2 data overruns script".into());
                }
                out.push(script[pc..pc + l].to_vec());
                pc += l;
            }
            _ => return Err(format!("non-push opcode {:#04x} in scriptSig", op)),
        }
    }
    Ok(out)
}

/// One verified input. `der_mode` records which decoder accepted the sig.
#[derive(Debug, Clone)]
pub struct VerifiedSig {
    pub sighash: Hash,
    pub hash_type: i32,
    pub pubkey: Vec<u8>,
    pub der_mode: &'static str, // "strict" | "lax"
}

/// CheckSig (script.cpp:1285-1323) against a previous output's scriptPubKey:
/// hashtype = trailing sig byte, sighash over (tx, nIn, scriptCode), ECDSA
/// verify. Non-strict DER is accepted only through the lax decoder, recorded
/// in der_mode so callers can demand strict for corpus-grade checks.
pub fn check_sig(
    tx_to: &Tx,
    n_in: usize,
    script_code: &[u8],
    sig_with_hashtype: &[u8],
    pubkey: &[u8],
) -> Result<VerifiedSig, String> {
    if sig_with_hashtype.is_empty() {
        return Err("CheckSig: empty signature".into());
    }
    let hash_type = *sig_with_hashtype.last().unwrap() as i32;
    let sig_der = &sig_with_hashtype[..sig_with_hashtype.len() - 1];
    let sighash = signature_hash(tx_to, n_in, script_code, hash_type)?;
    // Strict DER first (what modern libsecp enforces); fall back to the lax
    // decoder for the OpenSSL-era encodings the mint clients produced
    // (pre-strict-ECDSA). der_mode records which path accepted.
    let is_strict = decode_signature_der_strict(sig_der).is_ok();
    let (r, s) = if is_strict {
        decode_signature_der_strict(sig_der).expect("checked ok above")
    } else {
        decode_signature_der_lax(sig_der).map_err(|_| {
            "signature not parseable as DER (strict or lax)".to_string()
        })?
    };
    let mut compact = [0u8; 64];
    compact[..32].copy_from_slice(&r);
    compact[32..].copy_from_slice(&s);
    let sig = secp256k1::ecdsa::Signature::from_compact(&compact)
        .map_err(|e| format!("bad sig components: {}", e))?;
    let pk = secp256k1::PublicKey::from_slice(pubkey).map_err(|e| format!("bad pubkey: {}", e))?;
    let m = secp256k1::Message::from_digest(sighash);
    secp_ctx()
        .verify_ecdsa(&m, &sig, &pk)
        .map_err(|e| format!("CheckSig: ECDSA verify failed: {}", e))?;
    Ok(VerifiedSig {
        sighash,
        hash_type,
        pubkey: pubkey.to_vec(),
        der_mode: if is_strict { "strict" } else { "lax" },
    })
}

/// Solver classification of a scriptPubKey into the spendable forms we sign
/// (script.cpp:1336-1470): PubKey (bare), PubKeyHash. P2SH inputs are out of
/// scope for phase 3 spends (no redemption scripts in the wallet flow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendType {
    PubKey(Vec<u8>),
    PubkeyHash([u8; 20]),
}

pub fn solver_type(script_pubkey: &[u8]) -> Option<SpendType> {
    if let Some(pk) = crate::consensus::bare_pubkey_from_script(script_pubkey) {
        return Some(SpendType::PubKey(pk.to_vec()));
    }
    match crate::address::script_to_dest(script_pubkey) {
        Some(crate::address::ScriptDest::PubkeyHash(h)) => Some(SpendType::PubkeyHash(h)),
        _ => None,
    }
}

/// ScriptSigArgsExpected (script.cpp:1515-1533): PubKey → 1, PubKeyHash → 2.
pub fn script_sig_args_expected(t: &SpendType) -> usize {
    match t {
        SpendType::PubKey(_) => 1,
        SpendType::PubkeyHash(_) => 2,
    }
}

/// VerifyScript-lite for the two standard templates (main.cpp ConnectInputs →
/// VerifyScript → EvalScript flow): parse the scriptSig pushes, require the
/// Solver-expected element count, enforce the P2PKH pubkey-hash equality
/// (OP_EQUALVERIFY), then CheckSig.
pub fn verify_input(
    tx_to: &Tx,
    n_in: usize,
    prev_script_pubkey: &[u8],
) -> Result<VerifiedSig, String> {
    if n_in >= tx_to.vin.len() {
        return Err(format!(
            "VerifyScript: nIn={} out of range (vin {})",
            n_in,
            tx_to.vin.len()
        ));
    }
    let spend_type =
        solver_type(prev_script_pubkey).ok_or_else(|| "VerifyScript: non-standard script".to_string())?;
    let pushes = parse_pushes(&tx_to.vin[n_in].script_sig)
        .map_err(|e| format!("VerifyScript: bad scriptSig: {}", e))?;
    if pushes.len() != script_sig_args_expected(&spend_type) {
        return Err(format!(
            "VerifyScript: scriptSig has {} pushes, Solver expects {}",
            pushes.len(),
            script_sig_args_expected(&spend_type)
        ));
    }
    let (sig, pubkey): (&[u8], &[u8]) = match &spend_type {
        SpendType::PubKey(pk) => {
            // scriptPubKey carries the key itself
            (&pushes[0], pk.as_slice())
        }
        SpendType::PubkeyHash(h) => {
            // OP_DUP OP_HASH160 <h> OP_EQUALVERIFY: hash160(scriptSig pubkey)
            // must equal the embedded hash before CHECKSIG can run
            let pk = &pushes[1];
            if &crate::address::hash160(pk) != h {
                return Err("VerifyScript: pubkey hash mismatch (OP_EQUALVERIFY)".into());
            }
            (&pushes[0], pk.as_slice())
        }
    };
    check_sig(tx_to, n_in, prev_script_pubkey, sig, pubkey)
}

/// Sign one input of `tx` in place, building the scriptSig per the Solver
/// (SignSignature + Solver keystore flow, script.cpp:1477-1513).
pub fn sign_input(
    tx_to: &mut Tx,
    n_in: usize,
    prev_script_pubkey: &[u8],
    secret: &[u8; 32],
    pubkey: &[u8],
    hash_type: i32,
) -> Result<(), String> {
    let spend_type = solver_type(prev_script_pubkey)
        .ok_or_else(|| "SignSignature: cannot sign non-standard script".to_string())?;
    let sighash = signature_hash(tx_to, n_in, prev_script_pubkey, hash_type)?;
    let mut der = sign_msg(secret, &sighash)?;
    der.push(hash_type as u8);
    let script_sig = match &spend_type {
        SpendType::PubKey(_) => script_sig_p2pk(&der),
        SpendType::PubkeyHash(_) => script_sig_p2pkh(&der, pubkey),
    };
    tx_to.vin[n_in].script_sig = script_sig;
    Ok(())
}

// ---------------------------------------------------------------------------
// Input selection + spend construction (CreateTransaction port)
// ---------------------------------------------------------------------------
/// One spendable previous output plus the key that owns it.
#[derive(Debug, Clone)]
pub struct SpendInput {
    pub txid: Hash, // internal byte order (as serialized in vin.prev_hash)
    pub n: u32,
    pub value: i64,
    pub script_pubkey: Vec<u8>,
    pub height: u32,  // containing block height (maturity bookkeeping)
    pub tx_time: u32, // prev tx nTime (ConnectInputs ordering rule)
    pub is_coinbase: bool,
    pub is_coinstake: bool,
    pub secret: [u8; 32],
    pub pubkey: Vec<u8>,
}

impl SpendInput {
    pub fn outpoint(&self) -> (Hash, u32) {
        (self.txid, self.n)
    }

    /// The key this input's prevout actually pays to — enforced before
    /// signing so a key/prevout mismatch fails loudly instead of producing a
    /// tx the network would reject (Solver key lookup in C++ Sign1/SignN).
    pub fn assert_owns_prevout(&self) -> Result<(), String> {
        match solver_type(&self.script_pubkey) {
            Some(SpendType::PubKey(pk)) => {
                if pk != self.pubkey {
                    return Err(format!(
                        "input {}:{} prevout pays a different pubkey",
                        crate::legacy::hash_to_display(&self.txid),
                        self.n
                    ));
                }
                Ok(())
            }
            Some(SpendType::PubkeyHash(h)) => {
                if crate::address::hash160(&self.pubkey) != h {
                    return Err(format!(
                        "input {}:{} prevout pays a different pubkey hash",
                        crate::legacy::hash_to_display(&self.txid),
                        self.n
                    ));
                }
                Ok(())
            }
            None => Err(format!(
                "input {}:{} prevout is non-standard",
                crate::legacy::hash_to_display(&self.txid),
                self.n
            )),
        }
    }
}

/// Simple deterministic selection: largest-first until the target is covered
/// (value >= target). The C++ SelectCoins uses a fuzzy-target knapsack; any
/// value-covering set produces an identical wire outcome once signed, so the
/// simpler deterministic policy is safe here. Selection never picks inputs
/// that fail the ownership pre-check (they would fail at signing anyway).
pub fn select_utxos_covering(inputs: &[SpendInput], target: i64) -> Result<Vec<SpendInput>, String> {
    if target <= 0 {
        return Err("select: target must be positive".into());
    }
    for i in inputs {
        if !money_range(i.value) {
            return Err(format!(
                "select: input {}:{} value out of MoneyRange",
                crate::legacy::hash_to_display(&i.txid),
                i.n
            ));
        }
        i.assert_owns_prevout()?;
    }
    let mut sorted: Vec<&SpendInput> = inputs.iter().collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value).then(a.txid.cmp(&b.txid)).then(a.n.cmp(&b.n)));
    let mut picked = Vec::new();
    let mut sum = 0i64;
    for i in sorted {
        if sum >= target {
            break;
        }
        sum = sum
            .checked_add(i.value)
            .ok_or_else(|| "select: input sum overflow".to_string())?;
        picked.push(i.clone());
    }
    if sum < target {
        return Err(format!(
            "select: insufficient funds (have {}, need {})",
            sum, target
        ));
    }
    Ok(picked)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeMode {
    Block,
    Relay,
}

/// GetMinFee (main.cpp:1000-1027). `n_block_size` is the caller's block-size
/// context (CreateTransaction passes 1; ConnectInputs' GetMinFee() default is
/// also Block mode with nBlockSize=1), so the near-full-block escalation
/// branch is dead in both callers — ported anyway for 1:1 fidelity.
pub fn get_min_fee(vout_values: &[i64], n_bytes: usize, n_block_size: usize, mode: FeeMode) -> i64 {
    let base = match mode {
        FeeMode::Relay => MIN_RELAY_TX_FEE,
        FeeMode::Block => MIN_TX_FEE,
    };
    let new_block_size = n_block_size + n_bytes;
    let mut min_fee = (1 + n_bytes as i64 / 1000) * base;
    if min_fee < base {
        for v in vout_values {
            if *v < CENT {
                min_fee = base;
                break;
            }
        }
    }
    if n_block_size != 1 && new_block_size >= MAX_BLOCK_SIZE_GEN / 2 {
        if new_block_size >= MAX_BLOCK_SIZE_GEN {
            return MAX_MONEY;
        }
        min_fee *= (MAX_BLOCK_SIZE_GEN / (MAX_BLOCK_SIZE_GEN - new_block_size)) as i64;
    }
    if !money_range(min_fee) {
        min_fee = MAX_MONEY;
    }
    min_fee
}

pub struct SpendPlan {
    pub tx: Tx,
    pub fee: i64,
    pub inputs_value: i64,
    pub change: i64, // 0 when no change output
}

/// Build + sign a standard spend (CreateTransaction, wallet.cpp:1979-2115):
/// vouts = [dest, (change)], vins from `inputs`, per-input SignSignature,
/// size cap MAX_BLOCK_SIZE_GEN/5, fee loop raising `fee` to
/// max(nPayFee, nMinFee). `change_script = None` sends the whole remainder to
/// fee (the caller must pass a change script for real spends).
///
/// Deviations from the C++, all determinism-preserving and validity-neutral:
///   - change output appended LAST (C++ inserts at a random position);
///   - inputs ordered by (txid, n) ascending (C++ iterates std::set order);
///   - nLockTime = 0 (C++ leaves wtxNew.nLockTime at its constructor value 0).
pub fn build_spend(
    inputs: &[SpendInput],
    dest_script: Vec<u8>,
    dest_value: i64,
    change_script: Option<Vec<u8>>,
    mut fee: i64,
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    if inputs.is_empty() {
        return Err("CreateTransaction: no inputs".into());
    }
    if solver_type(&dest_script).is_none() {
        return Err("CreateTransaction: destination script is non-standard".into());
    }
    if !money_range(dest_value) || dest_value <= 0 {
        return Err("CreateTransaction: send value out of range".into());
    }
    let mut inputs_value = 0i64;
    for i in inputs {
        if !money_range(i.value) {
            return Err("CreateTransaction: input value out of range".into());
        }
        i.assert_owns_prevout()?;
        inputs_value += i.value;
        if !money_range(inputs_value) {
            return Err("CreateTransaction: input sum out of range".into());
        }
    }

    let mut ordered: Vec<SpendInput> = inputs.to_vec();
    ordered.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.n.cmp(&b.n)));

    loop {
        // nValue + nFeeRet <= nValueIn is enforced below (value in < out)
        let mut vout = vec![Vout {
            value: dest_value,
            script_pubkey: dest_script.clone(),
        }];
        let change = inputs_value - dest_value - fee;
        if change < 0 {
            return Err(format!(
                "CreateTransaction: inputs ({}) < dest ({}) + fee ({})",
                inputs_value, dest_value, fee
            ));
        }
        if change > 0 {
            let cs = change_script
                .clone()
                .ok_or_else(|| "CreateTransaction: change required but no change script".to_string())?;
            if solver_type(&cs).is_none() {
                return Err("CreateTransaction: change script is non-standard".into());
            }
            vout.push(Vout {
                value: change,
                script_pubkey: cs,
            });
        }
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

        // Sign (SignSignature per input, wallet.cpp:2084-2088)
        for (i, input) in ordered.iter().enumerate() {
            sign_input(&mut tx, i, &input.script_pubkey, &input.secret, &input.pubkey, SIGHASH_ALL)?;
        }

        // Size limit (wallet.cpp:2090-2093): nBytes >= MAX_BLOCK_SIZE_GEN/5
        let n_bytes = tx.serialize().len();
        if n_bytes >= MAX_BLOCK_SIZE_GEN / 5 {
            return Err(format!("CreateTransaction: tx too large ({} bytes)", n_bytes));
        }

        // Fee check (wallet.cpp:2096-2104): nPayFee = nTransactionFee *
        // (1 + nBytes/1000) with the nTransactionFee default (main.cpp:450);
        // nMinFee = GetMinFee(1, Send, nBytes).
        let n_pay_fee = DEFAULT_TRANSACTION_FEE * (1 + n_bytes as i64 / 1000);
        let n_min_fee = get_min_fee(
            &tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
            n_bytes,
            1,
            FeeMode::Block,
        );
        let required = n_pay_fee.max(n_min_fee);
        if fee < required {
            fee = required;
            continue;
        }

        // ConnectInputs validation BEFORE returning the plan — a spend must
        // pass the same input checks the network applies (main.cpp:1980-2067).
        connect_inputs_check(&tx, &ordered, spend_height)?;

        return Ok(SpendPlan {
            tx,
            fee,
            inputs_value,
            change: change.max(0),
        });
    }
}

/// ConnectInputs port (main.cpp:1980-2067) for our own spends. `prevs` must
/// align 1:1 with tx.vin (FetchInputs → MapPrevTx in the C++). The
/// signature-verification and fee gates apply; the checkpoint-skips and
/// script-cache paths are daemon-side performance paths, not rules.
pub fn connect_inputs_check(
    tx: &Tx,
    prevs: &[SpendInput],
    spend_height: u32,
) -> Result<(), String> {
    let is_coinbase = crate::consensus::tx_is_coin_base(tx);
    if !is_coinbase {
        if prevs.len() != tx.vin.len() {
            return Err("ConnectInputs: prevout map does not cover every input".into());
        }
        let mut value_in = 0i64;
        for (txin, entry) in tx.vin.iter().zip(prevs.iter()) {
            if entry.txid != txin.prev_hash || entry.n != txin.prev_n {
                return Err("ConnectInputs: prevout map misaligned with vin".into());
            }
            if entry.is_coinbase || entry.is_coinstake {
                // nCoinbaseMaturity = 7 mainnet (main.cpp:65)
                let depth = spend_height as i64 - entry.height as i64;
                if depth < COINBASE_MATURITY as i64 {
                    return Err(format!(
                        "ConnectInputs: tried to spend {} at depth {}",
                        if entry.is_coinbase { "coinbase" } else { "coinstake" },
                        depth
                    ));
                }
            }
            if entry.tx_time > tx.time {
                return Err(
                    "ConnectInputs: transaction timestamp earlier than input transaction".into(),
                );
            }
            value_in += entry.value;
            if !money_range(entry.value) || !money_range(value_in) {
                return Err("ConnectInputs: txin values out of range".into());
            }
        }

        // Expensive ECDSA checks only after all cheap checks pass
        for (i, entry) in prevs.iter().enumerate() {
            verify_input(tx, i, &entry.script_pubkey)
                .map_err(|e| format!("ConnectInputs: VerifySignature failed: {}", e))?;
        }

        if !crate::consensus::tx_is_coin_stake(tx) {
            let value_out: i64 = tx.vout.iter().map(|o| o.value).sum();
            if value_in < value_out {
                return Err("ConnectInputs: value in < value out".into());
            }
            let tx_fee = value_in - value_out;
            if tx_fee < 0 {
                return Err("ConnectInputs: nTxFee < 0".into());
            }
            let min_fee = get_min_fee(
                &tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
                tx.serialize().len(),
                1,
                FeeMode::Block,
            );
            if tx_fee < min_fee {
                return Err(format!(
                    "ConnectInputs: not paying required fee={} paid={}",
                    min_fee, tx_fee
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Synthetic keys — dry-run helpers. Secrets are derived deterministically
// from a label via double-SHA256 (NOT cryptographically random): adequate
// for construction/verification dry runs that never touch the network, and
// keeps tests + bins reproducible.
// ---------------------------------------------------------------------------
pub fn synthetic_key(label: &str) -> ([u8; 32], Vec<u8>) {
    let secret = sha256d(format!("tri7-spend-dryrun/{}", label).as_bytes());
    let sk = secp256k1::SecretKey::from_slice(&secret).expect("sha256d output is a valid scalar");
    let pk = secp256k1::PublicKey::from_secret_key(secp_ctx(), &sk);
    (secret, pk.serialize().to_vec())
}

// ---------------------------------------------------------------------------
// Tests — all expected values derived programmatically (Tx::serialize /
// sha256d / the library's own codecs); no hand-computed constants.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{Tx, Vin, Vout};

    fn two_input_tx() -> Tx {
        Tx {
            version: 1,
            time: 1_400_500_000,
            vin: vec![
                Vin {
                    prev_hash: [0x11u8; 32],
                    prev_n: 0,
                    script_sig: vec![0xde, 0xad],
                    sequence: u32::MAX,
                },
                Vin {
                    prev_hash: [0x22u8; 32],
                    prev_n: 1,
                    script_sig: vec![0xbe, 0xef],
                    sequence: 42,
                },
            ],
            vout: vec![
                Vout {
                    value: 900_000,
                    script_pubkey: vec![0x21, 0x02, 0xaa],
                },
                Vout {
                    value: 55,
                    script_pubkey: vec![0x76, 0xa9, 0x14],
                },
            ],
            locktime: 0,
        }
    }

    #[test]
    fn signature_hash_is_the_one_input_script_pattern() {
        // Port of the C++ body: serialize the tx with every scriptSig blanked
        // except nIn = scriptCode, append hashType as a 4-byte LE int, and
        // double-SHA256. The expected bytes are rebuilt here from
        // Tx::serialize so the test pins the PATTERN, not a magic constant.
        let tx = two_input_tx();
        let script_code = vec![0x21u8, 0x02, 0xbb, 0xac];
        for n_in in 0..2usize {
            // SIGHASH_ALL: plain one-input-script pattern. NONE/SINGLE/ACP
            // have their own branch tests below.
            let ht = SIGHASH_ALL;
            let got = signature_hash(&tx, n_in, &script_code, ht).unwrap();
            let mut expect_tx = tx.clone();
            for i in 0..expect_tx.vin.len() {
                expect_tx.vin[i].script_sig = Vec::new();
            }
            expect_tx.vin[n_in].script_sig = script_code.clone();
            let mut buf = expect_tx.serialize();
            buf.extend_from_slice(&ht.to_le_bytes());
            assert_eq!(got, sha256d(&buf), "n_in={}", n_in);
        }
    }

    #[test]
    fn signature_hash_binds_every_transaction_field() {
        let base = two_input_tx();
        let sc = vec![0x21u8, 0x02, 0xaa, 0xac];
        let h0 = signature_hash(&base, 0, &sc, SIGHASH_ALL).unwrap();
        let mut m = base.clone();
        m.version += 1;
        assert_ne!(signature_hash(&m, 0, &sc, SIGHASH_ALL).unwrap(), h0);
        let mut m = base.clone();
        m.time += 1;
        assert_ne!(signature_hash(&m, 0, &sc, SIGHASH_ALL).unwrap(), h0);
        let mut m = base.clone();
        m.vin[1].sequence ^= 1; // another input's sequence is bound under ALL
        assert_ne!(signature_hash(&m, 0, &sc, SIGHASH_ALL).unwrap(), h0);
        let mut m = base.clone();
        m.vout[0].value += 1;
        assert_ne!(signature_hash(&m, 0, &sc, SIGHASH_ALL).unwrap(), h0);
        let mut m = base.clone();
        m.vout[1].script_pubkey.push(0xac);
        assert_ne!(signature_hash(&m, 0, &sc, SIGHASH_ALL).unwrap(), h0);
        assert_ne!(signature_hash(&base, 0, &[0x21, 0x02, 0xac, 0xac], SIGHASH_ALL).unwrap(), h0);
        assert_ne!(signature_hash(&base, 1, &sc, SIGHASH_ALL).unwrap(), h0);
        // hashtype is a 4-byte LE int in the preimage: 1 vs 0x81 differ
        assert_ne!(
            signature_hash(&base, 0, &sc, SIGHASH_ANYONECANPAY | SIGHASH_ALL).unwrap(),
            h0
        );
    }

    #[test]
    fn signature_hash_out_of_range_paths() {
        let tx = two_input_tx();
        assert!(signature_hash(&tx, 2, &[0xac], SIGHASH_ALL)
            .err()
            .unwrap()
            .contains("nIn=2 out of range"));
        // SIGHASH_SINGLE: nOut = nIn must exist
        assert!(signature_hash(&tx, 1, &[0xac], SIGHASH_SINGLE).is_ok());
        let single_vout = Tx {
            vout: vec![tx.vout[0].clone()],
            ..tx.clone()
        };
        assert!(signature_hash(&single_vout, 1, &[0xac], SIGHASH_SINGLE)
            .err()
            .unwrap()
            .contains("nOut=1 out of range"));
    }

    #[test]
    fn signature_hash_none_single_anyonecanpay_branches_match_cpp() {
        let tx = two_input_tx();
        let sc = vec![0x21u8, 0x02, 0xaa, 0xac];
        // NONE: vout cleared, other sequences zeroed
        let got = signature_hash(&tx, 1, &sc, SIGHASH_NONE).unwrap();
        let mut e = tx.clone();
        e.vout.clear();
        e.vin[0].script_sig = Vec::new();
        e.vin[0].sequence = 0;
        e.vin[1].script_sig = sc.clone();
        let mut buf = e.serialize();
        buf.extend_from_slice(&SIGHASH_NONE.to_le_bytes());
        assert_eq!(got, sha256d(&buf));
        // SINGLE: earlier vouts SetNull() (value -1 + empty script, main.h:411)
        let got = signature_hash(&tx, 1, &sc, SIGHASH_SINGLE).unwrap();
        let mut e = tx.clone();
        e.vout.truncate(2);
        e.vout[0].value = -1;
        e.vout[0].script_pubkey = Vec::new();
        e.vin[0].script_sig = Vec::new();
        e.vin[0].sequence = 0;
        e.vin[1].script_sig = sc.clone();
        let mut buf = e.serialize();
        buf.extend_from_slice(&SIGHASH_SINGLE.to_le_bytes());
        assert_eq!(got, sha256d(&buf));
        // ANYONECANPAY: vin reduced to nIn alone
        let got = signature_hash(&tx, 1, &sc, SIGHASH_ANYONECANPAY | SIGHASH_ALL).unwrap();
        let mut e = tx.clone();
        e.vin = vec![Vin {
            prev_hash: [0x22u8; 32],
            prev_n: 1,
            script_sig: sc.clone(),
            sequence: 42,
        }];
        let mut buf = e.serialize();
        buf.extend_from_slice(&(SIGHASH_ANYONECANPAY | SIGHASH_ALL).to_le_bytes());
        assert_eq!(got, sha256d(&buf));
    }

    #[test]
    fn find_and_delete_op_is_byte_exact() {
        // standard scripts pass through untouched
        let p2pk = {
            let (sec, pk) = synthetic_key("fnd/passthrough");
            script_pubkey_p2pk(&pk).unwrap_or_else(|_| {
                let _ = sec;
                unreachable!()
            })
        };
        assert_eq!(find_and_delete_op(&p2pk, OP_CODESEPARATOR), p2pk);
        let p2pkh = script_pubkey_p2pkh(&[0xabu8; 33]);
        assert_eq!(find_and_delete_op(&p2pkh, OP_CODESEPARATOR), p2pkh);
        // 0xab at instruction boundaries is removed
        let s = [0xab, 0x51, 0xab, 0xab, 0x52];
        assert_eq!(find_and_delete_op(&s, 0xab), [0x51, 0x52]);
        // 0xab INSIDE a data push is preserved (GetOp skips push bodies);
        // the trailing 0xab IS at an instruction boundary and is erased —
        // exactly what CScript::FindAndDelete([OP_CODESEPARATOR]) does.
        let s = {
            let mut v = vec![0x21u8]; // push 33
            v.extend_from_slice(&[0xabu8; 33]);
            v.push(0xab);
            v
        };
        let mut want = vec![0x21u8];
        want.extend_from_slice(&[0xabu8; 33]);
        assert_eq!(find_and_delete_op(&s, 0xab), want);
        // truncated push tail is left verbatim
        let s = [0x05, 0x01, 0x02];
        assert_eq!(find_and_delete_op(&s, 0xab), s);
        let s = [0x4c]; // PUSHDATA1 without length
        assert_eq!(find_and_delete_op(&s, 0xab), s);
    }

    #[test]
    fn der_encode_decode_roundtrips_and_matches_libsecp() {
        for i in 0..64u32 {
            let mut r = [0u8; 32];
            let mut s = [0u8; 32];
            // spread magnitudes: minimal encodings across 1..32 bytes
            r.copy_from_slice(&sha256d(format!("r{}", i).as_bytes()));
            s.copy_from_slice(&sha256d(format!("s{}", i).as_bytes()));
            let der = signature_components_der(r, s);
            // cross-check against the library's own encoder
            let lib = secp256k1::ecdsa::Signature::from_compact(&{
                let mut c = [0u8; 64];
                c[..32].copy_from_slice(&r);
                c[32..].copy_from_slice(&s);
                c
            })
            .unwrap();
            assert_eq!(der, lib.serialize_der().to_vec(), "encode mismatch at {}", i);
            // strict decode roundtrip
            let (r2, s2) = decode_signature_der_strict(&der).unwrap();
            assert_eq!(r2, r);
            assert_eq!(s2, s);
            // strict == library parser
            assert!(secp256k1::ecdsa::Signature::from_der(&der).is_ok());
        }
        // zero encodes as 01
        let der = signature_components_der([0u8; 32], [0u8; 32]);
        assert_eq!(der, vec![0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01]);
    }

    #[test]
    fn der_strict_rejects_lax_accepts_noncanonical() {
        // canonical encoding of a 1-byte r
        let canon = [0x30, 0x06, 0x02, 0x01, 0x2a, 0x02, 0x01, 0x2b];
        assert!(decode_signature_der_strict(&canon).is_ok());
        // non-minimal: r zero-padded to 2 bytes
        let padded = [0x30, 0x07, 0x02, 0x02, 0x00, 0x2a, 0x02, 0x01, 0x2b];
        assert!(decode_signature_der_strict(&padded).is_err());
        assert_eq!(decode_signature_der_lax(&padded).unwrap().0[31], 0x2a);
        // negative-looking integer (no pad)
        let negative = [0x30, 0x06, 0x02, 0x01, 0xff, 0x02, 0x01, 0x2b];
        assert!(decode_signature_der_strict(&negative).is_err());
        assert_eq!(decode_signature_der_lax(&negative).unwrap().0[31], 0xff);
        // SEQUENCE length mismatch / trailing junk
        let bad_len = [0x30, 0x08, 0x02, 0x01, 0x2a, 0x02, 0x01, 0x2b, 0x00];
        assert!(decode_signature_der_strict(&bad_len).is_err());
        let truncated = [0x30, 0x03, 0x02, 0x01];
        assert!(decode_signature_der_strict(&truncated).is_err());
        assert!(decode_signature_der_strict(&[]).is_err());
    }

    #[test]
    fn script_builders_match_solver_templates() {
        let (_, pk) = synthetic_key("builders/main");
        // PubKey template: push(pk) OP_CHECKSIG
        let p2pk = script_pubkey_p2pk(&pk).unwrap();
        let mut want = vec![pk.len() as u8];
        want.extend_from_slice(&pk);
        want.push(0xac);
        assert_eq!(p2pk, want);
        assert!(matches!(solver_type(&p2pk), Some(SpendType::PubKey(_))));
        // PubKeyHash template: 76 a9 14 <20> 88 ac
        let p2pkh = script_pubkey_p2pkh(&pk);
        assert_eq!(p2pkh.len(), 25);
        assert_eq!(&p2pkh[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&p2pkh[3..23], &crate::address::hash160(&pk));
        assert_eq!(&p2pkh[23..], &[0x88, 0xac]);
        assert!(matches!(solver_type(&p2pkh), Some(SpendType::PubkeyHash(_))));
        // scriptSig shapes per Solver
        let sig = vec![0x30u8; 71];
        assert_eq!(script_sig_p2pk(&sig).len(), 1 + 71);
        let ss = script_sig_p2pkh(&sig, &pk);
        assert_eq!(ss[0], 71);
        assert_eq!(ss[72], 33);
        assert_eq!(&ss[73..], &pk);
        // rejects non-key blobs (Solver would classify the script NonStandard)
        assert!(script_pubkey_p2pk(&[0u8; 32]).is_err());
        assert!(script_pubkey_p2pk(&[0x05; 33]).is_err());
        assert!(script_pubkey_p2pk(&[0x04; 64]).is_err());
    }

    #[test]
    fn parse_pushes_walks_data_pushes_only() {
        assert_eq!(parse_pushes(&[]).unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(parse_pushes(&[0x00]).unwrap(), vec![Vec::<u8>::new()]);
        let s = {
            let mut v = vec![0x4cu8, 0xff]; // PUSHDATA1 with 255 bytes
            v.extend_from_slice(&vec![7u8; 255]);
            v
        };
        assert_eq!(parse_pushes(&s).unwrap(), vec![vec![7u8; 255]]);
        // non-push opcode → error
        assert!(parse_pushes(&[0xac]).is_err());
        // overruning push → error
        assert!(parse_pushes(&[0x05, 0x01]).is_err());
        assert!(parse_pushes(&[0x4c, 0x05, 0x01]).is_err());
    }

    fn make_prevout(label: &str, script: Vec<u8>, value: i64) -> SpendInput {
        let (secret, pubkey) = synthetic_key(label);
        SpendInput {
            txid: sha256d(format!("txid/{}", label).as_bytes()),
            n: 0,
            value,
            script_pubkey: script,
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret,
            pubkey,
        }
    }

    #[test]
    fn sign_and_verify_roundtrip_p2pk_and_p2pkh() {
        for label in ["rt/p2pk", "rt/p2pkh"] {
            let (_, pk) = synthetic_key(&format!("{}-key", label));
            let (prev_script, build_script) = if label.ends_with("p2pk") {
                (script_pubkey_p2pk(&pk).unwrap(), script_pubkey_p2pk(&pk).unwrap())
            } else {
                (script_pubkey_p2pkh(&pk), script_pubkey_p2pkh(&pk))
            };
            let mut tx = Tx {
                version: 1,
                time: 1_400_003_600,
                vin: vec![Vin {
                    prev_hash: [0x33u8; 32],
                    prev_n: 0,
                    script_sig: Vec::new(),
                    sequence: u32::MAX,
                }],
                vout: vec![Vout {
                    value: 10_000,
                    script_pubkey: vec![0x21, 0x02, 0x99],
                }],
                locktime: 0,
            };
            let (secret, _) = synthetic_key(&format!("{}-key", label));
            sign_input(&mut tx, 0, &prev_script, &secret, &pk, SIGHASH_ALL).unwrap();
            // Solver shapes: 1 push for PubKey, 2 for PubKeyHash
            let pushes = parse_pushes(&tx.vin[0].script_sig).unwrap();
            assert_eq!(pushes.len(), script_sig_args_expected(&solver_type(&prev_script).unwrap()));
            // hashtype byte rides on the signature
            assert_eq!(*pushes[0].last().unwrap(), SIGHASH_ALL as u8);
            let v = verify_input(&tx, 0, &prev_script).unwrap();
            assert_eq!(v.hash_type, SIGHASH_ALL);
            assert_eq!(v.der_mode, "strict");
            assert_eq!(v.sighash, signature_hash(&tx, 0, &prev_script, SIGHASH_ALL).unwrap());
            // the same script via a rebuilt prev (identical bytes) verifies too
            verify_input(&tx, 0, &build_script).unwrap();
        }
    }

    #[test]
    fn verify_input_rejects_tampering() {
        let (_, pk) = synthetic_key("tamper/key");
        let prev = script_pubkey_p2pk(&pk).unwrap();
        let mut tx = Tx {
            version: 1,
            time: 1_400_003_600,
            vin: vec![Vin {
                prev_hash: [0x44u8; 32],
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 500,
                script_pubkey: vec![0x21, 0x02, 0x99],
            }],
            locktime: 0,
        };
        let (secret, _) = synthetic_key("tamper/key");
        sign_input(&mut tx, 0, &prev, &secret, &pk, SIGHASH_ALL).unwrap();
        verify_input(&tx, 0, &prev).unwrap(); // sanity
        // flip a byte inside the DER sig (middle of the r integer)
        let mut bad = tx.clone();
        let last = bad.vin[0].script_sig.len() - 2;
        bad.vin[0].script_sig[last] ^= 0x01;
        assert!(verify_input(&bad, 0, &prev).is_err());
        // flip the hashtype byte → different sighash → verify fails
        let mut bad = tx.clone();
        let n = bad.vin[0].script_sig.len();
        bad.vin[0].script_sig[n - 1] = SIGHASH_NONE as u8;
        assert!(verify_input(&bad, 0, &prev).is_err());
        // mutate a bound tx field after signing
        let mut bad = tx.clone();
        bad.vout[0].value += 1;
        assert!(verify_input(&bad, 0, &prev).is_err());
        // P2PKH: wrong pubkey (hash mismatch → OP_EQUALVERIFY fails)
        let (_, pk2) = synthetic_key("tamper/other");
        let prev_h = script_pubkey_p2pkh(&pk);
        let mut bad = tx.clone();
        bad.vin[0].script_sig = script_sig_p2pkh(&{
            let mut s = sign_msg(&secret, &signature_hash(&tx, 0, &prev_h, SIGHASH_ALL).unwrap()).unwrap();
            s.push(SIGHASH_ALL as u8);
            s
        }, &pk2);
        assert!(verify_input(&bad, 0, &prev_h)
            .err()
            .unwrap()
            .contains("hash mismatch"));
        // non-push opcode in scriptSig
        let mut bad = tx.clone();
        bad.vin[0].script_sig = vec![0xac];
        assert!(verify_input(&bad, 0, &prev).is_err());
        // wrong arg count (empty scriptSig)
        let mut bad = tx.clone();
        bad.vin[0].script_sig = Vec::new();
        assert!(verify_input(&bad, 0, &prev).is_err());
        // nIn out of range
        assert!(verify_input(&tx, 1, &prev).is_err());
    }

    #[test]
    fn sighash_none_and_anyonecanpay_verify_after_unbound_mutations() {
        let (_, pk) = synthetic_key("branches/key");
        let prev = script_pubkey_p2pk(&pk).unwrap();
        let (secret, _) = synthetic_key("branches/key");
        // SIGHASH_NONE: vout changes after signing do NOT invalidate
        let mut tx = Tx {
            version: 1,
            time: 1_400_003_600,
            vin: vec![Vin {
                prev_hash: [0x55u8; 32],
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 100,
                script_pubkey: vec![0x21, 0x02, 0x99],
            }],
            locktime: 0,
        };
        sign_input(&mut tx, 0, &prev, &secret, &pk, SIGHASH_NONE).unwrap();
        tx.vout[0].value = 999_999;
        verify_input(&tx, 0, &prev).unwrap(); // unbound under NONE
        // re-sign ALL over the mutated vout, then mutate again: bound under ALL
        sign_input(&mut tx, 0, &prev, &secret, &pk, SIGHASH_ALL).unwrap();
        tx.vout[0].value += 1;
        assert!(verify_input(&tx, 0, &prev).is_err()); // bound under ALL
        // ANYONECANPAY: adding another input after signing does NOT invalidate
        let mut tx = Tx {
            vin: vec![Vin {
                prev_hash: [0x55u8; 32],
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            ..tx.clone()
        };
        sign_input(&mut tx, 0, &prev, &secret, &pk, SIGHASH_ANYONECANPAY | SIGHASH_ALL).unwrap();
        tx.vin.push(Vin {
            prev_hash: [0x66u8; 32],
            prev_n: 1,
            script_sig: Vec::new(),
            sequence: u32::MAX,
        });
        verify_input(&tx, 0, &prev).unwrap(); // the ACP signature still holds
    }

    #[test]
    fn build_spend_end_to_end_and_connect_inputs() {
        let (_, dest_pk) = synthetic_key("spend/dest");
        let (_, change_pk) = synthetic_key("spend/change");
        let dest = script_pubkey_p2pk(&dest_pk).unwrap();
        let change = script_pubkey_p2pk(&change_pk).unwrap();
        let mut prev = make_prevout("spend/in0", vec![], 1_000_000);
        prev.script_pubkey = script_pubkey_p2pk(&prev.pubkey).unwrap();
        let inputs = vec![prev];
        let plan = build_spend(&inputs, dest.clone(), 900_000, Some(change.clone()), 0, 1_400_003_600, 107).unwrap();
        // fee was raised to the GetMinFee floor ((1 + bytes/1000) * 100)
        let bytes = plan.tx.serialize().len();
        assert_eq!(plan.fee, get_min_fee(&[900_000, plan.change], bytes, 1, FeeMode::Block));
        assert_eq!(plan.inputs_value - 900_000 - plan.fee, plan.change);
        assert_eq!(plan.tx.vout.len(), 2);
        connect_inputs_check(&plan.tx, &inputs, 107).unwrap();
        // deterministic rebuild: same inputs → identical signed tx bytes
        let plan2 = build_spend(&inputs, dest.clone(), 900_000, Some(change.clone()), 0, 1_400_003_600, 107).unwrap();
        assert_eq!(plan.tx.serialize(), plan2.tx.serialize());
        // P2PKH input → 2-push scriptSig
        let mut prev_h = make_prevout("spend/in1", vec![], 2_000_000);
        prev_h.script_pubkey = script_pubkey_p2pkh(&prev_h.pubkey);
        let plan_h = build_spend(&[prev_h.clone()], script_pubkey_p2pkh(&dest_pk), 1_000_000, Some(change.clone()), 0, 1_400_003_600, 107).unwrap();
        connect_inputs_check(&plan_h.tx, &[prev_h], 107).unwrap();
        assert_eq!(parse_pushes(&plan_h.tx.vin[0].script_sig).unwrap().len(), 2);
    }

    #[test]
    fn connect_inputs_rejects_maturity_timestamp_and_underfee() {
        let (_, dest_pk) = synthetic_key("rej/dest");
        let dest = script_pubkey_p2pk(&dest_pk).unwrap();
        let mut cb = make_prevout("rej/cb", vec![], 1_000_000);
        cb.script_pubkey = script_pubkey_p2pk(&cb.pubkey).unwrap();
        cb.is_coinbase = true;
        // coinbase at depth 6 (< 7) must be rejected (990_000 + 10_000 fee
        // consumes the input exactly, so no change script is needed)
        let err = build_spend(&[cb.clone()], dest.clone(), 990_000, None, 10_000, 1_400_003_600, 106)
            .err()
            .unwrap();
        assert!(err.contains("depth 6"), "got: {}", err);
        // depth 7 matures
        build_spend(&[cb.clone()], dest.clone(), 990_000, None, 10_000, 1_400_003_600, 107).unwrap();
        // input newer than the spend tx
        let mut late = make_prevout("rej/late", vec![], 1_000_000);
        late.script_pubkey = script_pubkey_p2pk(&late.pubkey).unwrap();
        late.tx_time = 1_400_003_700;
        let err = build_spend(&[late.clone()], dest.clone(), 990_000, None, 10_000, 1_400_003_600, 200)
            .err()
            .unwrap();
        assert!(err.contains("timestamp earlier than input"), "got: {}", err);
        // fee below GetMinFee ((1 + bytes/1000)*100) is impossible via
        // build_spend (the loop raises it) — check connect_inputs_check
        // directly with a PROPERLY SIGNED tx whose fee is underpaid. The
        // C++ runs the ECDSA loop before the fee tally, so only a valid
        // signature reaches the fee gate.
        let mut ok = make_prevout("rej/ok", vec![], 1_000_000);
        ok.script_pubkey = script_pubkey_p2pk(&ok.pubkey).unwrap();
        let mut tx = Tx {
            version: 1,
            time: 1_400_003_600,
            vin: vec![Vin {
                prev_hash: ok.txid,
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 999_950,
                script_pubkey: dest.clone(),
            }],
            locktime: 0,
        };
        sign_input(&mut tx, 0, &ok.script_pubkey, &ok.secret, &ok.pubkey, SIGHASH_ALL).unwrap();
        let bytes = tx.serialize().len();
        let min_fee = get_min_fee(&[999_950], bytes, 1, FeeMode::Block);
        assert!(min_fee > 0 && 1_000_000 - 999_950 < min_fee);
        let err = connect_inputs_check(&tx, &[ok.clone()], 200).err().unwrap();
        assert!(err.contains("not paying required fee"), "got: {}", err);
        // exact-minimum fee passes every ConnectInputs gate:
        let mut tx = Tx {
            vout: vec![Vout {
                value: 1_000_000 - min_fee,
                script_pubkey: dest.clone(),
            }],
            ..tx.clone()
        };
        sign_input(&mut tx, 0, &ok.script_pubkey, &ok.secret, &ok.pubkey, SIGHASH_ALL).unwrap();
        connect_inputs_check(&tx, &[ok], 200).unwrap();
    }

    #[test]
    fn select_utxos_covering_picks_largest_first_and_validates() {
        let mut small = make_prevout("sel/small", vec![], 100);
        small.script_pubkey = script_pubkey_p2pk(&small.pubkey).unwrap();
        let mut big = make_prevout("sel/big", vec![], 1_000_000);
        big.script_pubkey = script_pubkey_p2pk(&big.pubkey).unwrap();
        let mut tiny = make_prevout("sel/tiny", vec![], 50);
        tiny.script_pubkey = script_pubkey_p2pk(&tiny.pubkey).unwrap();
        let picked = select_utxos_covering(&[small.clone(), big.clone(), tiny.clone()], 100_000).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].txid, big.txid);
        let picked = select_utxos_covering(&[tiny.clone(), small.clone()], 120).unwrap();
        assert_eq!(picked.len(), 2);
        assert!(select_utxos_covering(&[tiny.clone(), small.clone()], 200).is_err());
        // a key/prevout mismatch is refused before selection
        let mut wrong = small.clone();
        wrong.script_pubkey = script_pubkey_p2pk(&big.pubkey).unwrap();
        assert!(select_utxos_covering(&[wrong], 10).is_err());
    }

    #[test]
    fn money_range_and_constants_match_cpp() {
        assert!(money_range(0));
        assert!(money_range(MAX_MONEY));
        assert!(!money_range(MAX_MONEY + 1));
        assert!(!money_range(-1));
        assert_eq!(MIN_TX_FEE, 100);
        assert_eq!(MIN_RELAY_TX_FEE, 100);
        assert_eq!(CENT, 10_000);
        assert_eq!(MAX_BLOCK_SIZE_GEN, 500_000);
        assert_eq!(COINBASE_MATURITY, 7);
        assert_eq!(DEFAULT_TRANSACTION_FEE, MIN_TX_FEE);
        // GetMinFee: (1 + bytes/1000) * 100 in Block mode
        assert_eq!(get_min_fee(&[1000], 999, 1, FeeMode::Block), 100);
        assert_eq!(get_min_fee(&[1000], 1000, 1, FeeMode::Block), 200);
        assert_eq!(get_min_fee(&[1000], 1999, 1, FeeMode::Block), 200);
        assert_eq!(get_min_fee(&[1000], 2000, 1, FeeMode::Block), 300);
        assert_eq!(get_min_fee(&[1000], 1999, 1, FeeMode::Relay), 200);
        // dead near-full-block branch stays ported (nBlockSize != 1)
        let big = MAX_BLOCK_SIZE_GEN / 2; // == escalation threshold
        assert_eq!(
            get_min_fee(&[1000], 1000, big, FeeMode::Block),
            200 * (MAX_BLOCK_SIZE_GEN / (MAX_BLOCK_SIZE_GEN - (big + 1000))) as i64
        );
    }
}
