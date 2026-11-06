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
pub const OP_EQUAL: u8 = 0x87;
pub const OP_RETURN: u8 = 0x6a;
/// OP_CHECKMULTISIG (script.h opcode enum) — evaluation lives below; the
/// raw-tx decoder table (rawtx.rs) prints the same 0xae byte.
pub const OP_CHECKMULTISIG: u8 = 0xae;

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
        return Err(format!(
            "p2pk: invalid pubkey form ({} bytes)",
            pubkey.len()
        ));
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
                let l = u32::from_le_bytes([
                    script[pc],
                    script[pc + 1],
                    script[pc + 2],
                    script[pc + 3],
                ]) as usize;
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
    let total = if total > data.len() {
        data.len()
    } else {
        total
    };
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
        decode_signature_der_lax(sig_der)
            .map_err(|_| "signature not parseable as DER (strict or lax)".to_string())?
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
/// (script.cpp:1336-1470): PubKey (bare), PubKeyHash, ScriptHash (P2SH —
/// the IsPayToScriptHash shortcut at script.cpp:1333, which Solver checks
/// before the template scan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendType {
    PubKey(Vec<u8>),
    PubkeyHash([u8; 20]),
    ScriptHash([u8; 20]),
}

pub fn solver_type(script_pubkey: &[u8]) -> Option<SpendType> {
    if let Some(pk) = crate::consensus::bare_pubkey_from_script(script_pubkey) {
        return Some(SpendType::PubKey(pk.to_vec()));
    }
    match crate::address::script_to_dest(script_pubkey) {
        Some(crate::address::ScriptDest::PubkeyHash(h)) => Some(SpendType::PubkeyHash(h)),
        // a9 14 <20> 87 — exactly the IsPayToScriptHash shape; the spend is
        // verified against the redeem script the scriptSig carries.
        Some(crate::address::ScriptDest::ScriptHash(h)) => Some(SpendType::ScriptHash(h)),
        _ => None,
    }
}

/// ScriptSigArgsExpected (script.cpp:1515-1533): PubKey → 1, PubKeyHash → 2,
/// ScriptHash → 1 (legacy "2..20" — the real count is m+2: OP_0 dummy, m
/// sigs, redeem script; the P2SH verification path below evaluates the
/// redeem script instead of enforcing this count).
pub fn script_sig_args_expected(t: &SpendType) -> usize {
    match t {
        SpendType::PubKey(_) => 1,
        SpendType::PubkeyHash(_) => 2,
        SpendType::ScriptHash(_) => 1,
    }
}

/// VerifyScript-lite (main.cpp ConnectInputs → VerifyScript → EvalScript
/// flow): parse the scriptSig pushes, require the Solver-expected element
/// count for the bare templates, enforce the P2PKH pubkey-hash equality
/// (OP_EQUALVERIFY), then CheckSig.
///
/// P2SH prevouts take the VerifyScript pay-to-script-hash branch
/// (script.cpp:1721-1734): the scriptSig must be push-only (parse_pushes
/// enforces exactly that), its LAST push is the redeem script, and the
/// redeem script is then evaluated as the effective scriptPubKey against
/// the remaining pushes.
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
    let spend_type = solver_type(prev_script_pubkey)
        .ok_or_else(|| "VerifyScript: non-standard script".to_string())?;
    let pushes = parse_pushes(&tx_to.vin[n_in].script_sig)
        .map_err(|e| format!("VerifyScript: bad scriptSig: {}", e))?;
    match &spend_type {
        SpendType::PubKey(pk) => {
            // scriptPubKey carries the key itself
            if pushes.len() != script_sig_args_expected(&spend_type) {
                return Err(format!(
                    "VerifyScript: scriptSig has {} pushes, Solver expects {}",
                    pushes.len(),
                    script_sig_args_expected(&spend_type)
                ));
            }
            check_sig(tx_to, n_in, prev_script_pubkey, &pushes[0], pk)
        }
        SpendType::PubkeyHash(h) => {
            if pushes.len() != script_sig_args_expected(&spend_type) {
                return Err(format!(
                    "VerifyScript: scriptSig has {} pushes, Solver expects {}",
                    pushes.len(),
                    script_sig_args_expected(&spend_type)
                ));
            }
            // OP_DUP OP_HASH160 <h> OP_EQUALVERIFY: hash160(scriptSig pubkey)
            // must equal the embedded hash before CHECKSIG can run
            let pk = &pushes[1];
            if &crate::address::hash160(pk) != h {
                return Err("VerifyScript: pubkey hash mismatch (OP_EQUALVERIFY)".into());
            }
            check_sig(tx_to, n_in, prev_script_pubkey, &pushes[0], pk.as_slice())
        }
        SpendType::ScriptHash(h) => verify_p2sh_input(tx_to, n_in, h, &pushes),
    }
}

/// VerifyScript's pay-to-script-hash branch (script.cpp:1721-1734): the
/// last scriptSig push is the redeem script — hash160(redeem) must equal
/// the scriptPubKey's hash (the OP_HASH160 … OP_EQUAL the P2SH scriptPubKey
/// evaluates) — and the redeem script is then evaluated as the effective
/// scriptPubKey against the remaining pushes (the legacy interpreter's
/// stackCopy path).
fn verify_p2sh_input(
    tx_to: &Tx,
    n_in: usize,
    h: &[u8; 20],
    pushes: &[Vec<u8>],
) -> Result<VerifiedSig, String> {
    if pushes.len() < 2 {
        return Err(
            "VerifyScript(P2SH): scriptSig must be OP_0 <sig…> <redeemScript>".to_string(),
        );
    }
    let redeem = &pushes[pushes.len() - 1];
    if &crate::address::hash160(redeem) != h {
        return Err(
            "VerifyScript(P2SH): redeem script hash mismatch (scriptSig hash160 != scriptPubKey hash)"
                .to_string(),
        );
    }
    eval_multisig_redeem(tx_to, n_in, redeem, &pushes[..pushes.len() - 1])
}

/// OP_CHECKMULTISIG evaluation (script.cpp:1043-1135) for the standard
/// m-of-n multisig redeem script, with `stack` = the scriptSig pushes minus
/// the redeem script: [OP_0 dummy] [sig m] … [sig 1].
///
/// Consensus rules mirrored 1:1:
///   - pop n (top), then n pubkeys, then m, then m signatures, then ONE
///     EXTRA element that is popped and ignored — the historical off-by-one
///     bug the protocol keeps: the scriptSig's leading OP_0. The legacy
///     underflow checks (`stack.size() < i`) make a scriptSig WITHOUT the
///     leading dummy fail for every m >= 1.
///   - each signature must match a pubkey at or after the last matched
///     index (script.cpp:1074-1095: isig/ikey advance together, ikey only
///     forward); running out of keys while signatures remain fails.
///   - every CheckSig runs over scriptCode = the REDEEM script (with
///     OP_CODESEPARATOR stripped inside signature_hash) and the hashtype
///     the signature carries. The legacy scriptCode.FindAndDelete(each sig)
///     is a no-op for this template — no instruction boundary of
///     OP_m <pks> OP_n OP_CHECKMULTISIG can start with a DER push.
fn eval_multisig_redeem(
    tx_to: &Tx,
    n_in: usize,
    redeem: &[u8],
    stack: &[Vec<u8>],
) -> Result<VerifiedSig, String> {
    let (m, pubkeys) = crate::multisig::parse_multisig(redeem)
        .map_err(|e| format!("VerifyScript(P2SH): redeem script is not a standard multisig: {}", e))?;
    let m = m as usize;
    let n = pubkeys.len();
    // n (count) + n pubkeys + m (count) + m sigs + 1 dummy pops.
    if stack.len() < m + 1 {
        return Err(format!(
            "VerifyScript(P2SH): CHECKMULTISIG stack underflow — need {} sigs plus the \
             leading OP_0 dummy ({} stack elements given)",
            m,
            stack.len()
        ));
    }
    let sigs = &stack[stack.len() - m..];
    let _dummy = &stack[stack.len() - m - 1]; // popped and ignored (script.cpp:1101)
    let mut ki = 0usize;
    let mut last: Option<VerifiedSig> = None;
    for (si, sig) in sigs.iter().enumerate() {
        let mut matched = false;
        while ki < n {
            let pk = &pubkeys[ki];
            ki += 1;
            if let Ok(v) = check_sig(tx_to, n_in, redeem, sig, pk) {
                last = Some(v);
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(format!(
                "VerifyScript(P2SH): CHECKMULTISIG signature #{} matches no remaining pubkey",
                si + 1
            ));
        }
    }
    // m >= 1 (parse_multisig rejects OP_0 m), so at least one sig matched.
    Ok(last.expect("m >= 1 guarantees a matched signature"))
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
        SpendType::ScriptHash(_) => {
            return Err(
                "SignSignature: P2SH multisig inputs need per-key partial signing \
                 (multisig::sign_multisig_partial + assemble_multisig_scriptsig)"
                    .into(),
            )
        }
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
    /// P2SH prevouts are REFUSED here: single-key ownership cannot prove
    /// them spendable — use `assert_owns_prevout_with` (a script book can
    /// answer "redeem present + enough keys").
    pub fn assert_owns_prevout(&self) -> Result<(), String> {
        match solver_type(&self.script_pubkey) {
            Some(SpendType::ScriptHash(_)) => Err(format!(
                "input {}:{} P2SH prevout needs a script book \
                 (assert_owns_prevout_with / the _with_book builders)",
                crate::legacy::hash_to_display(&self.txid),
                self.n
            )),
            _ => self.check_single_key_ownership(),
        }
    }

    /// Ownership check with a multisig script book: identical to
    /// `assert_owns_prevout` for PubKey/PubKeyHash; for ScriptHash the book
    /// must hold the redeem script (hash-matching) and at least m of its
    /// keys. Code paths with no book keep refusing P2SH (safe default).
    pub fn assert_owns_prevout_with(
        &self,
        book: &crate::multisig::ScriptBook,
    ) -> Result<(), String> {
        match solver_type(&self.script_pubkey) {
            Some(SpendType::ScriptHash(_)) => book.can_spend(&self.script_pubkey).map_err(|e| {
                format!(
                    "input {}:{} P2SH: {}",
                    crate::legacy::hash_to_display(&self.txid),
                    self.n,
                    e
                )
            }),
            _ => self.check_single_key_ownership(),
        }
    }

    /// The single-key ownership arms shared by both entry points.
    fn check_single_key_ownership(&self) -> Result<(), String> {
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
            Some(SpendType::ScriptHash(_)) => {
                unreachable!("check_single_key_ownership is only reached for non-P2SH prevouts")
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
pub fn select_utxos_covering(
    inputs: &[SpendInput],
    target: i64,
) -> Result<Vec<SpendInput>, String> {
    select_utxos_covering_with_book(inputs, target, &crate::multisig::ScriptBook::empty())
}

/// Coin selection with a multisig script book. Candidates the book cannot
/// spend — every P2SH prevout when the book is empty, unrelated P2SH
/// prevouts otherwise — are SKIPPED with a log line (never adopted), so an
/// unrelated P2SH UTXO cannot wedge a spend. Single-key ownership
/// mismatches remain hard errors (a key/prevout mismatch is a caller bug,
/// not a skippable coin).
pub fn select_utxos_covering_with_book(
    inputs: &[SpendInput],
    target: i64,
    book: &crate::multisig::ScriptBook,
) -> Result<Vec<SpendInput>, String> {
    if target <= 0 {
        return Err("select: target must be positive".into());
    }
    let mut usable: Vec<&SpendInput> = Vec::with_capacity(inputs.len());
    for i in inputs {
        if !money_range(i.value) {
            return Err(format!(
                "select: input {}:{} value out of MoneyRange",
                crate::legacy::hash_to_display(&i.txid),
                i.n
            ));
        }
        if let Err(e) = i.assert_owns_prevout_with(book) {
            if matches!(solver_type(&i.script_pubkey), Some(SpendType::ScriptHash(_))) {
                eprintln!(
                    "tri7-select: skipping input {}:{} ({} sats): {}",
                    crate::legacy::hash_to_display(&i.txid),
                    i.n,
                    i.value,
                    e
                );
                continue;
            }
            return Err(e);
        }
        usable.push(i);
    }
    let mut sorted = usable;
    sorted.sort_by(|a, b| {
        b.value
            .cmp(&a.value)
            .then(a.txid.cmp(&b.txid))
            .then(a.n.cmp(&b.n))
    });
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

/// The chain's fee floor, in micro-TRI (sats). ONE definition: consensus code,
/// the GUI's send dialog and the RPC surface all read this, so a fee can never
/// disagree with itself between the screen and the wire.
///
/// What the source says: `MIN_TX_FEE = CENT/100` (main.h:58) with
/// `CENT = 10000` and `COIN = 1000000` (util.h:53), i.e. 100 micro-TRI =
/// 0.0001 TRI, and `nTransactionFee` is initialised to it (main.cpp:113).
/// An absolute floor, not a rate: it does NOT scale with the TRI amount.
pub const MIN_FEE_SATS: i64 = MIN_TX_FEE;

/// What a transaction pays for `tx_bytes`, computed EXACTLY the way the legacy
/// builder decides it (wallet.cpp:1785-1793):
///     nPayFee = nTransactionFee * (1 + nBytes/1000)      (integer div)
///     nMinFee = GetMinFee(1, Send, nBytes)
///     fee    >= max(nPayFee, nMinFee)
/// `paytxfee_sats` is the operator's fee setting (nTransactionFee).
pub fn estimate_fee(tx_bytes: usize, paytxfee_sats: i64, vout_values: &[i64]) -> i64 {
    let n_pay_fee = paytxfee_sats * (1 + tx_bytes as i64 / 1000);
    let n_min_fee = get_min_fee(vout_values, tx_bytes, 1, FeeMode::Block);
    n_pay_fee.max(n_min_fee).max(MIN_FEE_SATS)
}

/// Estimated serialized size of a signed spend with `n_in` P2PKH inputs and
/// `n_out` P2PKH outputs, in the chain's wire format. The wallet re-checks the
/// REAL size after signing; this is for the dialog's estimate and for callers
/// that must pick coins before a transaction exists.
pub fn estimate_signed_size(n_in: usize, n_out: usize) -> usize {
    // 4 version + 4 time + 1 input count + 1 output count + 4 locktime
    let base = 4 + 4 + 1 + 1 + 4;
    // P2PKH input: 32 prev-hash + 4 index + 1 script len + (1+72 sig + 1+33 key) + 4 sequence
    let in_size = 32 + 4 + 1 + (1 + 72) + (1 + 33) + 4;
    // P2PKH output: 8 value + 1 script len + 25 script
    let out_size = 8 + 1 + 25;
    base + n_in * in_size + n_out * out_size
}

/// Per-input signed-size estimate: the same wire schedule as
/// [`estimate_signed_size`], but each input sized by its SOLVER TYPE so a
/// mixed / multisig pool stops being quoted as if every input were P2PKH.
///
///   - PubkeyHash: 32+4+1+(1+72)+(1+33)+4 = 148   (today's value, unchanged)
///   - PubKey:     32+4+1+(1+72)+4        = 114   (bare key, no hash push)
///   - ScriptHash: 32+4+1 + m*(1+72+1) + 1 + redeem_len + 4
///     (m from the redeem script via the `book`; the sigs + the OP_0 dummy +
///     the redeemScript push dominate — a 2-of-3 with a 105-byte redeem is
///     ~295 bytes, not 148)
///
/// Base (4+4+1+1+4) and per-output (8+1+25) match `estimate_signed_size`.
/// A P2SH input the book cannot name a redeem for (or a non-standard script —
/// which selection refuses anyway) falls back to the conservative 148.
pub fn estimate_signed_size_for(
    inputs: &[SpendInput],
    n_out: usize,
    book: &crate::multisig::ScriptBook,
) -> usize {
    let base = 4 + 4 + 1 + 1 + 4;
    let out_size = 8 + 1 + 25;
    let mut total = base + n_out * out_size;
    for i in inputs {
        total += match solver_type(&i.script_pubkey) {
            Some(SpendType::PubkeyHash(_)) => 32 + 4 + 1 + (1 + 72) + (1 + 33) + 4,
            Some(SpendType::PubKey(_)) => 32 + 4 + 1 + (1 + 72) + 4,
            Some(SpendType::ScriptHash(_)) => book
                .redeem_for(&i.script_pubkey)
                .and_then(|r| {
                    crate::multisig::parse_multisig(r)
                        .ok()
                        .map(|(m, _)| (m as usize, r.len()))
                })
                .map(|(m, redeem_len)| {
                    32 + 4 + 1 + m * (1 + 72 + 1) + 1 + redeem_len + 4
                })
                .unwrap_or(148),
            None => 148,
        };
    }
    total
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
    fee_rate_sats: i64,
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    build_spend_outputs(
        inputs,
        vec![Vout {
            value: dest_value,
            script_pubkey: dest_script,
        }],
        change_script,
        fee_rate_sats,
        n_time,
        spend_height,
    )
}

/// The multisig-capable single-destination variant: identical construction
/// to `build_spend`, but P2SH inputs the `book` can spend (redeem present +
/// m keys) are signed via the multisig path; P2SH inputs it cannot spend
/// are skipped, and if nothing spendable remains the error names why.
pub fn build_spend_with_book(
    inputs: &[SpendInput],
    dest_script: Vec<u8>,
    dest_value: i64,
    change_script: Option<Vec<u8>>,
    fee_rate_sats: i64,
    book: &crate::multisig::ScriptBook,
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    build_spend_outputs_floor_with_book(
        inputs,
        vec![Vout {
            value: dest_value,
            script_pubkey: dest_script,
        }],
        change_script,
        fee_rate_sats,
        0,
        book,
        n_time,
        spend_height,
    )
}

/// Multi-destination spend (legacy `sendmany`). Same construction, signing,
/// fee and size rules as `build_spend`; only the payment outputs differ, so the
/// single-destination function is a thin wrapper over this one rather than a
/// second copy of the logic.
///
/// `fee` is the PER-SIZE-STEP rate in sats (`nTransactionFee`), not an
/// already-scaled total: this function multiplies it by (1 + nBytes/1000)
/// itself (wallet.cpp:2096-2104). Passing a converged total would scale the
/// fee twice; read the final number from the returned `SpendPlan::fee`.
pub fn build_spend_outputs(
    inputs: &[SpendInput],
    dest_outputs: Vec<Vout>,
    change_script: Option<Vec<u8>>,
    mut fee_rate_sats: i64, // per-size-step rate (nTransactionFee), NOT a total
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    build_spend_outputs_floor(
        inputs,
        dest_outputs,
        change_script,
        fee_rate_sats,
        0,
        n_time,
        spend_height,
    )
}

/// Same construction with an explicit TOTAL floor: the operator may have agreed
/// to a specific fee in sats (a typed fee in the Send dialog). The transaction
/// must pay at least that, while the rate supplies the size schedule. This is
/// the only way to honour a typed total without mistaking it for a rate.
///
/// Book-less form: P2SH inputs are refused (an empty script book cannot spend
/// them) — single-key wallet behavior, unchanged.
pub fn build_spend_outputs_floor(
    inputs: &[SpendInput],
    dest_outputs: Vec<Vout>,
    change_script: Option<Vec<u8>>,
    fee_rate_sats: i64, // per-size-step rate (nTransactionFee)
    fee_floor_sats: i64, // absolute minimum total the operator agreed to
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    build_spend_outputs_floor_with_book(
        inputs,
        dest_outputs,
        change_script,
        fee_rate_sats,
        fee_floor_sats,
        &crate::multisig::ScriptBook::empty(),
        n_time,
        spend_height,
    )
}

/// The multisig-capable builder. Everything matches
/// `build_spend_outputs_floor`, plus:
///   - P2SH candidates the `book` cannot spend are SKIPPED with a log line
///     (never adopted), so an unrelated P2SH UTXO cannot wedge a spend; if
///     nothing spendable remains, the error names the skipped input and why;
///   - the signing loop branches on the solver type: PubKey/PubKeyHash sign
///     through `sign_input` as before; ScriptHash signs through
///     `sign_multisig_partial` + `assemble_multisig_scriptsig` (m derived
///     from the redeem script), failing loudly with the input coordinates
///     and the signature shortfall if the book comes up short.
pub fn build_spend_outputs_floor_with_book(
    inputs: &[SpendInput],
    dest_outputs: Vec<Vout>,
    change_script: Option<Vec<u8>>,
    mut fee_rate_sats: i64, // per-size-step rate (nTransactionFee)
    fee_floor_sats: i64,    // absolute minimum total the operator agreed to
    book: &crate::multisig::ScriptBook,
    n_time: u32,
    spend_height: u32,
) -> Result<SpendPlan, String> {
    if dest_outputs.is_empty() {
        return Err("CreateTransaction: no destinations".into());
    }
    let mut dest_total = 0i64;
    for d in &dest_outputs {
        if solver_type(&d.script_pubkey).is_none() {
            return Err("CreateTransaction: destination script is non-standard".into());
        }
        if !money_range(d.value) || d.value <= 0 {
            return Err("CreateTransaction: send value out of range".into());
        }
        dest_total = dest_total
            .checked_add(d.value)
            .ok_or_else(|| "CreateTransaction: destination sum overflow".to_string())?;
    }
    // Adopt only inputs the book can actually spend (single-key inputs are
    // always candidates; P2SH inputs must pass the book's can_spend).
    let mut kept: Vec<SpendInput> = Vec::with_capacity(inputs.len());
    let mut skipped_p2sh: Vec<(String, u32, String)> = Vec::new();
    for i in inputs {
        if let Some(SpendType::ScriptHash(_)) = solver_type(&i.script_pubkey) {
            if let Err(e) = book.can_spend(&i.script_pubkey) {
                eprintln!(
                    "tri7-spend: skipping input {}:{} ({} sats): {}",
                    crate::legacy::hash_to_display(&i.txid),
                    i.n,
                    i.value,
                    e
                );
                skipped_p2sh.push((crate::legacy::hash_to_display(&i.txid), i.n, e));
                continue;
            }
        }
        kept.push(i.clone());
    }
    if kept.is_empty() {
        if let Some((id, n, why)) = skipped_p2sh.first() {
            return Err(format!("input {}:{}: {}", id, n, why));
        }
        return Err("CreateTransaction: no inputs".into());
    }
    let inputs = &kept[..];
    let mut inputs_value = 0i64;
    for i in inputs {
        if !money_range(i.value) {
            return Err("CreateTransaction: input value out of range".into());
        }
        i.assert_owns_prevout_with(book)?;
        inputs_value += i.value;
        if !money_range(inputs_value) {
            return Err("CreateTransaction: input sum out of range".into());
        }
    }

    let mut ordered: Vec<SpendInput> = inputs.to_vec();
    ordered.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.n.cmp(&b.n)));
    // `fee_rate_sats` is the caller's rate floor and seeds nFeeRet (wallet.cpp:1688
    // starts nFeeRet at nTransactionFee and raises it in the loop below).
    //
    // CRITICAL: on entry this is a RATE — sats at the first size step — NOT an
    // already size-scaled total. The loop below multiplies it by
    // (1 + nBytes/1000), so passing a converged total would scale it a SECOND
    // time and the built fee would exceed what was shown. Callers pass
    // `paytxfee_sats` and read the final number from `SpendPlan.fee`.
    let est_fee = fee_rate_sats.max(MIN_FEE_SATS);

    // Multisig shapes: seed the fee reservation with the PER-INPUT estimate
    // (a 2-of-3 input is ~295 bytes, so its schedule can exceed the raw
    // rate below the first 1000-byte step of the count-based formula).
    // Reserving the estimate up front makes the first change computation
    // honest; the loop below still converges on the REAL signed size, and
    // the reservation never exceeds what that fixed point would charge.
    // Single-key shapes skip this branch and stay byte-identical.
    if ordered
        .iter()
        .any(|i| matches!(solver_type(&i.script_pubkey), Some(SpendType::ScriptHash(_))))
    {
        let est_bytes = estimate_signed_size_for(&ordered, dest_outputs.len() + 1, book);
        let est_schedule = estimate_fee(
            est_bytes,
            est_fee,
            &dest_outputs.iter().map(|o| o.value).collect::<Vec<_>>(),
        );
        fee_rate_sats = fee_rate_sats.max(est_schedule);
    }

    loop {
        // nValue + nFeeRet <= nValueIn is enforced below (value in < out)
        let mut vout = dest_outputs.clone();
        let change = inputs_value - dest_total - fee_rate_sats;
        if change < 0 {
            return Err(format!(
                "CreateTransaction: inputs ({}) < dest ({}) + fee_rate_sats ({})",
                inputs_value, dest_total, fee_rate_sats
            ));
        }
        if change > 0 {
            let cs = change_script.clone().ok_or_else(|| {
                "CreateTransaction: change required but no change script".to_string()
            })?;
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

        // Sign (SignSignature per input, wallet.cpp:2084-2088). PubKey /
        // PubKeyHash go through the single-key Sign1 path; ScriptHash goes
        // through the multisig SignN path with the book's held keys.
        for (i, input) in ordered.iter().enumerate() {
            if let Some(SpendType::ScriptHash(_)) = solver_type(&input.script_pubkey) {
                let id = crate::legacy::hash_to_display(&input.txid);
                let redeem = book.redeem_for(&input.script_pubkey).ok_or_else(|| {
                    format!("input {}:{}: no redeem script in the script book", id, input.n)
                })?;
                let (m, _) = crate::multisig::parse_multisig(redeem)
                    .map_err(|e| format!("input {}:{}: {}", id, input.n, e))?;
                let held = book.held_keys(redeem);
                let sigs = crate::multisig::sign_multisig_partial(&tx, i, redeem, &held)?;
                let filled = sigs.iter().flatten().count();
                if filled < m as usize {
                    return Err(format!(
                        "input {}:{}: only {} of {} required signatures could be produced",
                        id, input.n, filled, m
                    ));
                }
                tx.vin[i].script_sig =
                    crate::multisig::assemble_multisig_scriptsig(&sigs, redeem)?;
            } else {
                sign_input(
                    &mut tx,
                    i,
                    &input.script_pubkey,
                    &input.secret,
                    &input.pubkey,
                    SIGHASH_ALL,
                )?;
            }
        }

        // Size limit (wallet.cpp:2090-2093): nBytes >= MAX_BLOCK_SIZE_GEN/5
        let n_bytes = tx.serialize().len();
        if n_bytes >= MAX_BLOCK_SIZE_GEN / 5 {
            return Err(format!(
                "CreateTransaction: tx too large ({} bytes)",
                n_bytes
            ));
        }

        // Fee check (wallet.cpp:2096-2104): nPayFee = nTransactionFee *
        // (1 + nBytes/1000) with the nTransactionFee default (main.cpp:450);
        // nMinFee = GetMinFee(1, Send, nBytes).
        let n_pay_fee = est_fee * (1 + n_bytes as i64 / 1000);
        let n_min_fee = get_min_fee(
            &tx.vout.iter().map(|o| o.value).collect::<Vec<_>>(),
            n_bytes,
            1,
            FeeMode::Block,
        );
        let required = n_pay_fee
            .max(n_min_fee)
            .max(MIN_FEE_SATS)
            .max(fee_floor_sats);
        if fee_rate_sats < required {
            fee_rate_sats = required;
            continue;
        }

        // ConnectInputs validation BEFORE returning the plan — a spend must
        // pass the same input checks the network applies (main.cpp:1980-2067).
        connect_inputs_check(&tx, &ordered, spend_height)?;

        return Ok(SpendPlan {
            tx,
            fee: fee_rate_sats,
            inputs_value,
            change: change.max(0),
        });
    }
}

/// Converge coins + fee_rate_sats for a spend whose shape decides its own fee_rate_sats.
///
/// The fee_rate_sats scales with the SIGNED SIZE, the size grows with the input count,
/// and the input count depends on how much the fee_rate_sats takes — a fixed point. This
/// walks it: select for (amount + fee_rate_sats(shape)), re-derive the fee_rate_sats for the shape
/// we got, repeat until the count stops changing. Callers that must agree
/// (the RPC builder, the GUI estimate and the GUI validation) all go through
/// this ONE function, so the number on screen is the number built.
///
/// `inputs` are the spendable candidates; `out_values` every payment output
/// (a change output is assumed when change can occur). Returns the selected
/// coins, the converged fee, and the shape used, or an explicit insufficiency error.
#[derive(Debug, Clone)]
pub struct FeeShape {
    pub selected: Vec<SpendInput>,
    /// the CONVERGED total fee for this shape (what the operator is shown)
    pub fee: i64,
    pub n_inputs: usize,
    pub n_outputs: usize,
}

pub fn select_with_fee(
    inputs: &[SpendInput],
    out_values: &[i64],
    paytxfee_sats: i64,
) -> Result<FeeShape, String> {
    select_with_fee_with_book(inputs, out_values, paytxfee_sats, &crate::multisig::ScriptBook::empty())
}

/// `select_with_fee` with a multisig script book: P2SH candidates the book
/// cannot spend are skipped inside the covering selection (see
/// `select_utxos_covering_with_book`).
pub fn select_with_fee_with_book(
    inputs: &[SpendInput],
    out_values: &[i64],
    paytxfee_sats: i64,
    book: &crate::multisig::ScriptBook,
) -> Result<FeeShape, String> {
    let pay = paytxfee_sats.max(MIN_FEE_SATS);
    let amount: i64 = out_values.iter().sum();
    if amount <= 0 {
        return Err("select: amount must be positive".into());
    }
    // Seed with the count-based 1-input estimate, then converge on the
    // PER-INPUT size of the selection we actually got (a 2-of-3 input is
    // ~295 bytes, not 148 — the quoted fee must schedule what will be
    // signed, or the builder's exact-equality guard refuses). Uniform
    // single-key pools reproduce the old sequence exactly: the per-input
    // function returns the same 148 the count formula used.
    let mut n_in = 1usize;
    let mut fee = estimate_fee(estimate_signed_size(n_in, out_values.len() + 1), pay, out_values);
    for _ in 0..8 {
        let selected = select_utxos_covering_with_book(inputs, amount + fee, book)?;
        let got = selected.len();
        let real_fee = estimate_fee(
            estimate_signed_size_for(&selected, out_values.len() + 1, book),
            pay,
            out_values,
        );
        // settled: the same input count AND a fee that schedules exactly
        // this selection's per-input shape
        if got == n_in && real_fee == fee {
            return Ok(FeeShape {
                selected,
                fee,
                n_inputs: n_in,
                n_outputs: out_values.len() + 1,
            });
        }
        n_in = got;
        fee = real_fee;
    }
    // nine iterations without settling means the pool cannot cover it
    Err("Insufficient funds!".into())
}

/// The fee an already-decided selection will pay (the value the builder will
/// charge after it signs): same inputs+outputs, same schedule. Used to compare
/// against what the caller was shown.
pub fn fee_for_selection(
    selected: &[SpendInput],
    out_values: &[i64],
    paytxfee_sats: i64,
) -> i64 {
    estimate_fee(
        estimate_signed_size(selected.len(), out_values.len() + 1),
        paytxfee_sats.max(MIN_FEE_SATS),
        out_values,
    )
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
                        if entry.is_coinbase {
                            "coinbase"
                        } else {
                            "coinstake"
                        },
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

    /// A spendable coin with a distinct outpoint, for selection tests. The
    /// prevout must be a REAL p2pkh paying the coin's own pubkey hash, because
    /// select_utxos_covering enforces `assert_owns_prevout`.
    fn coin(seed: u8, value: i64) -> SpendInput {
        let mut sk = [0u8; 32];
        sk[31] = seed;
        sk[0] = 1;
        let secp = secp256k1::Secp256k1::new();
        let secret_key = secp256k1::SecretKey::from_slice(&sk).expect("sk");
        let pubkey = secret_key.public_key(&secp).serialize().to_vec();
        let h = crate::address::hash160(&pubkey);
        let mut script = vec![OP_DUP, OP_HASH160, 20];
        script.extend_from_slice(&h);
        script.push(OP_EQUALVERIFY);
        script.push(OP_CHECKSIG);
        SpendInput {
            txid: [seed; 32],
            n: seed as u32,
            value,
            script_pubkey: script,
            height: 1,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: sk,
            pubkey,
        }
    }

    #[test]
    fn select_with_fee_converges_and_crosses_the_1000_byte_boundary() {
        // Many small coins: the selected count pushes the signed size past
        // 1000 bytes, which doubles the byte schedule. A fixed two-input
        // allowance would under-fund this and then fail inside the builder.
        let pool: Vec<SpendInput> = (1..=80u8).map(|i| coin(i, 1_000_000)).collect();
        let pay = MIN_FEE_SATS;
        let shape = select_with_fee(&pool, &[40_000_000], pay).expect("select");

        assert!(shape.n_inputs >= 40, "needs many coins: {}", shape.n_inputs);
        let bytes = estimate_signed_size(shape.n_inputs, shape.n_outputs);
        assert!(bytes > 1000, "test must cross the boundary: {} bytes", bytes);

        // the fee is the schedule for the shape actually selected ...
        let expected = estimate_fee(bytes, pay, &[40_000_000]);
        assert_eq!(shape.fee, expected);
        // schedule: pay * (1 + bytes/1000) with INTEGER division — a ~1400
        // byte transaction is one step above 1000, so 2x the minimum
        let steps = 1 + bytes / 1000;
        assert_eq!(
            shape.fee,
            pay * steps as i64,
            "{} bytes -> {} steps",
            bytes,
            steps
        );
        assert!(shape.fee > MIN_FEE_SATS, "fee must exceed the 1x minimum");

        // ... and the selection really covers amount + that fee
        let sum: i64 = shape.selected.iter().map(|c| c.value).sum();
        assert!(sum >= 40_000_000 + shape.fee, "{} < amount + fee", sum);

        // fee_for_selection agrees with what select_with_fee reported: this is
        // the equality the GUI display / validation / builder all rely on
        assert_eq!(
            fee_for_selection(&shape.selected, &[40_000_000], pay),
            shape.fee
        );
    }

    #[test]
    fn built_transaction_fee_equals_the_converged_fee_above_1000_bytes() {
        // The end-to-end guard against double-scaling: converge the shape, then
        // BUILD + SIGN the real transaction with the same inputs and assert the
        // plan's final fee is exactly the converged fee (not a second
        // multiplication of it).
        let pool: Vec<SpendInput> = (1..=80u8).map(|i| coin(i, 1_000_000)).collect();
        let amount = 40_000_000i64;
        let pay = MIN_FEE_SATS;
        let shape = select_with_fee(&pool, &[amount], pay).expect("select");
        let bytes = estimate_signed_size(shape.n_inputs, shape.n_outputs);
        assert!(bytes > 1000, "must cross the boundary: {} bytes", bytes);

        let dest = pool[0].script_pubkey.clone();
        let change = pool[1].script_pubkey.clone();
        let plan = build_spend(
            &shape.selected,
            dest,
            amount,
            Some(change),
            pay, // RATE, exactly what the RPC builder passes
            1_400_500_000,
            100,
        )
        .expect("build");

        // no double scaling: the built fee is the same schedule value the
        // display showed, i.e. pay * (1 + real_bytes/1000)
        let real_bytes = plan.tx.serialize().len();
        let expected = estimate_fee(real_bytes, pay, &[amount]);
        assert_eq!(
            plan.fee, expected,
            "built fee {} != scheduled {} for {} bytes",
            plan.fee, expected, real_bytes
        );
        assert_eq!(
            plan.fee, shape.fee,
            "built fee {} != converged/displayed fee {}",
            plan.fee, shape.fee
        );
        // and the change is exact: inputs - amount - fee
        let input_sum: i64 = shape.selected.iter().map(|c| c.value).sum();
        assert_eq!(plan.change, input_sum - amount - plan.fee);
        assert!(plan.change > 0, "change must be positive here");
    }

    #[test]
    fn select_with_fee_is_idempotent_and_stable() {
        let pool: Vec<SpendInput> = (1..=12u8).map(|i| coin(i, 300_000)).collect();
        let a = select_with_fee(&pool, &[1_500_000], 500).expect("select");
        let b = select_with_fee(&pool, &[1_500_000], 500).expect("select");
        assert_eq!(a.n_inputs, b.n_inputs);
        assert_eq!(a.fee, b.fee);
        assert_eq!(a.selected.len(), b.selected.len());
        // a higher operator setting costs more and never selects fewer coins
        let c = select_with_fee(&pool, &[1_500_000], 50_000).expect("select");
        assert!(c.fee >= a.fee);
        assert!(c.n_inputs >= a.n_inputs);
    }

    #[test]
    fn select_with_fee_reports_insufficiency_honestly() {
        let pool: Vec<SpendInput> = (1..=3u8).map(|i| coin(i, 1_000)).collect();
        // the pool cannot cover 10 TRI: the selector's own message is the
        // honest answer (the GUI maps it to the legacy "Insufficient funds!")
        let err = select_with_fee(&pool, &[10_000_000], MIN_FEE_SATS).unwrap_err();
        assert!(
            err.to_lowercase().contains("insufficient"),
            "expected an insufficiency error, got: {}",
            err
        );
        assert!(err.contains("have 3000"), "reports what it had: {}", err);
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
        assert_ne!(
            signature_hash(&base, 0, &[0x21, 0x02, 0xac, 0xac], SIGHASH_ALL).unwrap(),
            h0
        );
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
            assert_eq!(
                der,
                lib.serialize_der().to_vec(),
                "encode mismatch at {}",
                i
            );
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
        assert!(matches!(
            solver_type(&p2pkh),
            Some(SpendType::PubkeyHash(_))
        ));
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
                (
                    script_pubkey_p2pk(&pk).unwrap(),
                    script_pubkey_p2pk(&pk).unwrap(),
                )
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
            assert_eq!(
                pushes.len(),
                script_sig_args_expected(&solver_type(&prev_script).unwrap())
            );
            // hashtype byte rides on the signature
            assert_eq!(*pushes[0].last().unwrap(), SIGHASH_ALL as u8);
            let v = verify_input(&tx, 0, &prev_script).unwrap();
            assert_eq!(v.hash_type, SIGHASH_ALL);
            assert_eq!(v.der_mode, "strict");
            assert_eq!(
                v.sighash,
                signature_hash(&tx, 0, &prev_script, SIGHASH_ALL).unwrap()
            );
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
        bad.vin[0].script_sig = script_sig_p2pkh(
            &{
                let mut s = sign_msg(
                    &secret,
                    &signature_hash(&tx, 0, &prev_h, SIGHASH_ALL).unwrap(),
                )
                .unwrap();
                s.push(SIGHASH_ALL as u8);
                s
            },
            &pk2,
        );
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
        sign_input(
            &mut tx,
            0,
            &prev,
            &secret,
            &pk,
            SIGHASH_ANYONECANPAY | SIGHASH_ALL,
        )
        .unwrap();
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
        let plan = build_spend(
            &inputs,
            dest.clone(),
            900_000,
            Some(change.clone()),
            0,
            1_400_003_600,
            107,
        )
        .unwrap();
        // fee was raised to the GetMinFee floor ((1 + bytes/1000) * 100)
        let bytes = plan.tx.serialize().len();
        assert_eq!(
            plan.fee,
            get_min_fee(&[900_000, plan.change], bytes, 1, FeeMode::Block)
        );
        assert_eq!(plan.inputs_value - 900_000 - plan.fee, plan.change);
        assert_eq!(plan.tx.vout.len(), 2);
        connect_inputs_check(&plan.tx, &inputs, 107).unwrap();
        // deterministic rebuild: same inputs → identical signed tx bytes
        let plan2 = build_spend(
            &inputs,
            dest.clone(),
            900_000,
            Some(change.clone()),
            0,
            1_400_003_600,
            107,
        )
        .unwrap();
        assert_eq!(plan.tx.serialize(), plan2.tx.serialize());
        // P2PKH input → 2-push scriptSig
        let mut prev_h = make_prevout("spend/in1", vec![], 2_000_000);
        prev_h.script_pubkey = script_pubkey_p2pkh(&prev_h.pubkey);
        let plan_h = build_spend(
            &[prev_h.clone()],
            script_pubkey_p2pkh(&dest_pk),
            1_000_000,
            Some(change.clone()),
            0,
            1_400_003_600,
            107,
        )
        .unwrap();
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
        let err = build_spend(
            &[cb.clone()],
            dest.clone(),
            990_000,
            None,
            10_000,
            1_400_003_600,
            106,
        )
        .err()
        .unwrap();
        assert!(err.contains("depth 6"), "got: {}", err);
        // depth 7 matures
        build_spend(
            &[cb.clone()],
            dest.clone(),
            990_000,
            None,
            10_000,
            1_400_003_600,
            107,
        )
        .unwrap();
        // input newer than the spend tx
        let mut late = make_prevout("rej/late", vec![], 1_000_000);
        late.script_pubkey = script_pubkey_p2pk(&late.pubkey).unwrap();
        late.tx_time = 1_400_003_700;
        let err = build_spend(
            &[late.clone()],
            dest.clone(),
            990_000,
            None,
            10_000,
            1_400_003_600,
            200,
        )
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
        sign_input(
            &mut tx,
            0,
            &ok.script_pubkey,
            &ok.secret,
            &ok.pubkey,
            SIGHASH_ALL,
        )
        .unwrap();
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
        sign_input(
            &mut tx,
            0,
            &ok.script_pubkey,
            &ok.secret,
            &ok.pubkey,
            SIGHASH_ALL,
        )
        .unwrap();
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
        let picked =
            select_utxos_covering(&[small.clone(), big.clone(), tiny.clone()], 100_000).unwrap();
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
        // The estimator must obey the CHAIN's coin (1e6 sats = 1 TRI), not a
        // Bitcoin habit: 0.0001 TRI is 100 sats, NOT 10 000.
        assert_eq!(MIN_FEE_SATS, 100);
        assert_eq!(estimate_fee(1, MIN_FEE_SATS, &[1_000]), 100);
        // legacy wallet.cpp:1785 rule — nTransactionFee * (1 + bytes/1000)
        assert_eq!(estimate_fee(1000, 100, &[1_000]), 200);
        assert_eq!(estimate_fee(1999, 100, &[1_000]), 200);
        assert_eq!(estimate_fee(2000, 100, &[1_000]), 300);
        // a higher operator setting is honoured on the same schedule
        assert_eq!(estimate_fee(500, 1000, &[1_000]), 1000);
        // and the floor survives a zero/garbage setting
        assert_eq!(estimate_fee(10, 0, &[1_000]), 100);
        // estimated signed size: 1-in/2-out lands under the 1000-byte step, so
        // the fee stays at the minimum — the estimate and the builder agree
        let size = estimate_signed_size(1, 2);
        assert_eq!(size, 14 + 148 + 2 * 34);
        let derived = estimate_fee(size, MIN_FEE_SATS, &[1_000, 500]);
        assert_eq!(estimate_fee(999, MIN_FEE_SATS, &[1_000]), derived);
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

    // ---- P2SH multisig (phase 3b) ----

    fn ms_keys(labels: &[&str]) -> Vec<([u8; 32], Vec<u8>)> {
        labels.iter().map(|l| synthetic_key(l)).collect()
    }

    fn ms_pubkeys(keys: &[([u8; 32], Vec<u8>)]) -> Vec<Vec<u8>> {
        keys.iter().map(|k| k.1.clone()).collect()
    }

    /// Known-answer vector shared with an INDEPENDENT implementation.
    ///
    /// The constants below were computed by a separate Python program
    /// (secp256k1 point math from integers, hash160 = RIPEMD160(SHA256(x)),
    /// base58check with version byte 28) written from the spec — NOT by this
    /// crate. This test is the cross-implementation check that the redeem
    /// script layout and the P2SH address encoding are correct, rather than
    /// merely self-consistent. The same program also reproduces the canonical
    /// privkey=1 hash160 vector (751e76e8…), so its hashing path is anchored.
    #[test]
    fn oracle_vector_two_of_three_matches_the_independent_implementation() {
        let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{:02x}", x)).collect() };
        let secp = secp_ctx();
        let mut pubkeys = Vec::new();
        for n in 1u8..=3 {
            let mut sk_bytes = [0u8; 32];
            sk_bytes[31] = n;
            let sk = secp256k1::SecretKey::from_slice(&sk_bytes).expect("scalar");
            let pk = secp256k1::PublicKey::from_secret_key(secp, &sk);
            pubkeys.push(pk.serialize().to_vec());
        }
        let redeem = crate::multisig::redeem_script(2, &pubkeys).expect("redeem");
        assert_eq!(redeem.len(), 105, "2-of-3 redeem script length");
        assert_eq!(
            hex(&redeem),
            "52210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798\
             2102c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5\
             2102f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9\
             53ae",
            "redeem script bytes must match the independent implementation"
        );
        assert_eq!(
            crate::multisig::p2sh_address(&redeem),
            "CJU8gKtn4aNBBovbywqPqZo6mjA1B1pLD1",
            "P2SH address must match the independent implementation"
        );
    }

    /// The P2SH scriptPubKey paying to `redeem`: a9 14 hash160(redeem) 87.
    fn p2sh_spk(redeem: &[u8]) -> Vec<u8> {
        let mut spk = vec![OP_HASH160, 0x14];
        spk.extend_from_slice(&crate::address::hash160(redeem));
        spk.push(OP_EQUAL);
        spk
    }

    fn ms_tx(prev_hash: [u8; 32]) -> Tx {
        Tx {
            version: 1,
            time: 1_400_003_600,
            vin: vec![Vin {
                prev_hash,
                prev_n: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 10_000,
                script_pubkey: script_pubkey_p2pk(&synthetic_key("ms/dest").1).unwrap(),
            }],
            locktime: 0,
        }
    }

    /// T3: solver_type recognizes the exact P2SH shape and yields the
    /// scriptPubKey hash — equal to hash160 of the redeem script.
    #[test]
    fn t3_solver_type_recognizes_p2sh_scripthash() {
        let keys = ms_keys(&["t3/a", "t3/b", "t3/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let h = crate::address::hash160(&redeem);
        assert_eq!(
            solver_type(&spk),
            Some(SpendType::ScriptHash(h)),
            "P2SH scriptPubKey must solve to ScriptHash(h)"
        );
        assert_eq!(h, crate::address::hash160(&redeem));
        assert_eq!(script_sig_args_expected(&SpendType::ScriptHash(h)), 1);
        // ownership WITHOUT a book refuses P2SH (round 2: the safe default);
        // with a book holding the redeem + m keys it passes — see T-C.
        let mut prev = make_prevout("t3/prev", spk, 100_000);
        prev.pubkey = synthetic_key("t3/unrelated").1;
        let err = prev.assert_owns_prevout().err().unwrap();
        assert!(
            err.contains("script book"),
            "book-less ownership must refuse P2SH, got: {}",
            err
        );
    }

    /// T4: a full 2-of-3 P2SH spend — two keys sign, verify_input passes,
    /// the assembled scriptSig starts with OP_0, and the whole tx clears
    /// the ConnectInputs gate the network applies.
    #[test]
    fn t4_two_of_three_p2sh_spend_signs_and_verifies() {
        let keys = ms_keys(&["t4/a", "t4/b", "t4/c"]);
        let pks = ms_pubkeys(&keys);
        let redeem = crate::multisig::redeem_script(2, &pks).unwrap();
        let spk = p2sh_spk(&redeem);
        let mut tx = ms_tx([0x77u8; 32]);
        let partial =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys[..2]).unwrap();
        assert_eq!(partial.len(), 3, "one slot per pubkey");
        assert_eq!(partial.iter().flatten().count(), 2);
        assert!(partial[2].is_none(), "wallet holds keys a and b only");
        let script_sig =
            crate::multisig::assemble_multisig_scriptsig(&partial, &redeem).unwrap();
        assert_eq!(script_sig[0], 0x00, "scriptSig must start with OP_0 (dummy)");
        tx.vin[0].script_sig = script_sig;

        let v = verify_input(&tx, 0, &spk).expect("2-of-3 spend must verify");
        assert_eq!(v.hash_type, SIGHASH_ALL);
        assert_eq!(v.der_mode, "strict");
        assert!(pks.contains(&v.pubkey), "verified against a redeem pubkey");

        let pushes = parse_pushes(&tx.vin[0].script_sig).unwrap();
        assert_eq!(pushes.len(), 4, "OP_0 dummy + 2 sigs + redeem script");
        assert!(pushes[0].is_empty(), "OP_0 pushes the empty vector");
        assert_eq!(pushes[3], redeem, "last push is the redeem script");
        let sig_lens: Vec<usize> = pushes[1..3].iter().map(|s| s.len()).collect();
        println!("T4 signature byte lengths: {:?}", sig_lens);
        for s in &pushes[1..3] {
            assert!(
                (70..=72).contains(&s.len()),
                "DER signature + hashtype byte, got {}",
                s.len()
            );
            assert_eq!(*s.last().unwrap(), SIGHASH_ALL as u8);
        }
        // the network gate: ConnectInputs over the real P2SH prevout passes
        let prev = SpendInput {
            txid: [0x77u8; 32],
            n: 0,
            value: 100_000,
            script_pubkey: spk,
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: [0u8; 32], // unused: P2SH ownership is proven by the sigs
            pubkey: Vec::new(),
        };
        connect_inputs_check(&tx, &[prev], 200).expect("ConnectInputs must accept the spend");
    }

    /// T5: the CHECKMULTISIG dummy is mandatory — stripping the leading
    /// OP_0 leaves a stack the opcode's extra pop underflows on.
    #[test]
    fn t5_scriptsig_without_the_op0_dummy_fails() {
        let keys = ms_keys(&["t5/a", "t5/b", "t5/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let mut tx = ms_tx([0x78u8; 32]);
        let partial =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys[..2]).unwrap();
        let script_sig =
            crate::multisig::assemble_multisig_scriptsig(&partial, &redeem).unwrap();
        tx.vin[0].script_sig = script_sig.clone();
        verify_input(&tx, 0, &spk).expect("sanity: assembled spend verifies");
        // REMOVE the leading OP_0 (a single 0x00 byte) and re-verify
        let stripped: Vec<u8> = script_sig[1..].to_vec();
        assert_ne!(stripped[0], 0x00);
        tx.vin[0].script_sig = stripped;
        let err = verify_input(&tx, 0, &spk).err().unwrap();
        assert!(
            err.contains("OP_0 dummy") || err.contains("underflow"),
            "missing dummy must fail loudly, got: {}",
            err
        );
    }

    /// T6: a scriptSig carrying a DIFFERENT redeem script fails at the
    /// hash160 gate (script.cpp:1721's IsPayToScriptHash branch), before
    /// any signature is checked.
    #[test]
    fn t6_wrong_redeem_script_fails_hash_mismatch() {
        let keys = ms_keys(&["t6/a", "t6/b", "t6/c"]);
        let other = ms_keys(&["t6/x", "t6/y", "t6/z"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let other_redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&other)).unwrap();
        let spk = p2sh_spk(&redeem);
        let mut tx = ms_tx([0x79u8; 32]);
        let partial =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys[..2]).unwrap();
        // assemble with the WRONG redeem script (right shape, wrong hash)
        let script_sig =
            crate::multisig::assemble_multisig_scriptsig(&partial, &other_redeem).unwrap();
        tx.vin[0].script_sig = script_sig;
        let err = verify_input(&tx, 0, &spk).err().unwrap();
        assert!(
            err.contains("hash mismatch"),
            "must fail with a hash-mismatch error, got: {}",
            err
        );
    }

    /// T7: CHECKMULTISIG matches in pubkey order — sig(c) before sig(b)
    /// exhausts the key list and fails, the assembled pubkey order passes.
    #[test]
    fn t7_signatures_out_of_pubkey_order_fail() {
        let keys = ms_keys(&["t7/a", "t7/b", "t7/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let mut tx = ms_tx([0x7au8; 32]);
        // wallet holds keys b and c (pubkey slots 1 and 2)
        let partial =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys[1..3]).unwrap();
        assert!(partial[0].is_none() && partial[1].is_some() && partial[2].is_some());
        let script_sig =
            crate::multisig::assemble_multisig_scriptsig(&partial, &redeem).unwrap();
        tx.vin[0].script_sig = script_sig.clone();
        verify_input(&tx, 0, &spk).expect("pubkey-order sigs verify");
        // swap the two signature pushes (dummy, sigB, sigC, redeem) →
        // (dummy, sigC, sigB, redeem): sigC matches pk3, then sigB has no
        // pubkey at or after that index left.
        let pushes = parse_pushes(&script_sig).unwrap();
        let mut swapped = vec![0x00u8];
        push_data(&mut swapped, &pushes[2]);
        push_data(&mut swapped, &pushes[1]);
        push_data(&mut swapped, &pushes[3]);
        tx.vin[0].script_sig = swapped;
        let err = verify_input(&tx, 0, &spk).err().unwrap();
        assert!(
            err.contains("CHECKMULTISIG"),
            "out-of-order sigs must fail, got: {}",
            err
        );
    }

    /// T8: partial signing with a single key fills one slot, and assembly
    /// refuses to build a 2-of-3 scriptSig from it.
    #[test]
    fn t8_partial_signing_with_one_key_cannot_assemble() {
        let keys = ms_keys(&["t8/a", "t8/b", "t8/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let tx = ms_tx([0x7bu8; 32]);
        let partial =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys[..1]).unwrap();
        assert_eq!(partial.iter().flatten().count(), 1, "one filled slot");
        assert!(partial[0].is_some() && partial[1].is_none() && partial[2].is_none());
        let err =
            crate::multisig::assemble_multisig_scriptsig(&partial, &redeem).err().unwrap();
        assert!(
            err.contains("missing signatures"),
            "assembly must demand m=2 sigs, got: {}",
            err
        );
    }

    /// T10: the boundary case — a 1-of-1 P2SH spend verifies end to end.
    #[test]
    fn t10_one_of_one_p2sh_spend_verifies() {
        let keys = ms_keys(&["t10/a"]);
        let redeem = crate::multisig::redeem_script(1, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let mut tx = ms_tx([0x7cu8; 32]);
        let partial = crate::multisig::sign_multisig_partial(&tx, 0, &redeem, &keys).unwrap();
        assert_eq!(partial.iter().flatten().count(), 1);
        let script_sig =
            crate::multisig::assemble_multisig_scriptsig(&partial, &redeem).unwrap();
        assert_eq!(script_sig[0], 0x00);
        tx.vin[0].script_sig = script_sig;
        let v = verify_input(&tx, 0, &spk).expect("1-of-1 must verify");
        assert_eq!(v.hash_type, SIGHASH_ALL);
        assert_eq!(v.pubkey, keys[0].1);
    }

    // ---- round 2: the wallet can actually SPEND multisig coins ----

    /// Build the book the way the wallet flow does: store the redeem script
    /// in a (temp) wallet file, read it back through `read_wallet`, and
    /// build the ScriptBook from the reader output + the held keys.
    fn book_via_wallet_file(
        tag: &str,
        redeem: &[u8],
        keys: &[([u8; 32], Vec<u8>)],
    ) -> crate::multisig::ScriptBook {
        let dir = std::env::temp_dir().join(format!("tri7spend_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.dat");
        crate::wallet::create_wallet_file(&path).unwrap();
        let mut wh = crate::wallet::WalletHandle::open_readwrite(&path).unwrap();
        wh.put_redeem_script(redeem).unwrap();
        wh.close();
        let w = crate::wallet::read_wallet(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        crate::multisig::ScriptBook::from_wallet(&w, keys)
    }

    /// T-A end to end: 2-of-3 created, redeem stored in the wallet, UTXO
    /// pays the P2SH address, the book-aware builder produces a plan whose
    /// input verifies via verify_input AND connect_inputs_check, with the
    /// scriptSig starting in OP_0 and ending in the redeem script.
    #[test]
    fn ta_book_builder_spends_multisig_end_to_end() {
        let keys = ms_keys(&["ta/a", "ta/b", "ta/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let book = book_via_wallet_file("ta", &redeem, &keys[..2]);
        let coin = SpendInput {
            txid: [0x91u8; 32],
            n: 1,
            value: 500_000,
            script_pubkey: spk.clone(),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: keys[0].0,
            pubkey: keys[0].1.clone(),
        };
        coin.assert_owns_prevout_with(&book)
            .expect("the book owns the multisig coin");
        let dest = script_pubkey_p2pk(&synthetic_key("ta/dest").1).unwrap();
        let change = script_pubkey_p2pkh(&synthetic_key("ta/change").1);
        let plan = build_spend_outputs_floor_with_book(
            &[coin.clone()],
            vec![Vout {
                value: 100_000,
                script_pubkey: dest,
            }],
            Some(change),
            0,
            0,
            &book,
            1_400_003_600,
            200,
        )
        .expect("multisig spend must build and sign");
        let ss = &plan.tx.vin[0].script_sig;
        assert_eq!(ss[0], 0x00, "scriptSig must start with OP_0");
        assert!(
            ss.ends_with(&redeem),
            "scriptSig must end with the redeem script push"
        );
        verify_input(&plan.tx, 0, &spk).expect("verify_input must accept the spend");
        connect_inputs_check(&plan.tx, &[coin], 200)
            .expect("connect_inputs_check must accept the spend");
    }

    /// T-B: a book holding only ONE of the 2 required keys must make the
    /// builder fail with the shortfall named — no partial plan.
    #[test]
    fn tb_book_with_one_key_cannot_build_multisig_spend() {
        let keys = ms_keys(&["tb/a", "tb/b", "tb/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let book = book_via_wallet_file("tb", &redeem, &keys[..1]);
        let coin = SpendInput {
            txid: [0x92u8; 32],
            n: 0,
            value: 500_000,
            script_pubkey: spk,
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: keys[0].0,
            pubkey: keys[0].1.clone(),
        };
        let dest = script_pubkey_p2pk(&synthetic_key("tb/dest").1).unwrap();
        let err = build_spend_outputs_floor_with_book(
            &[coin],
            vec![Vout {
                value: 100_000,
                script_pubkey: dest,
            }],
            None,
            0,
            0,
            &book,
            1_400_003_600,
            200,
        )
        .err()
        .unwrap();
        assert!(
            err.contains("only 1 of 2 required keys held"),
            "the error must name the shortfall, got: {}",
            err
        );
    }

    /// T-C: book-less ownership REFUSES a P2SH prevout; the _with form
    /// accepts it when the book holds the redeem + m keys, and refuses with
    /// the shortfall when a key is missing.
    #[test]
    fn tc_ownership_bookless_refuses_with_book_accepts() {
        let keys = ms_keys(&["tc/a", "tc/b", "tc/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk = p2sh_spk(&redeem);
        let coin = SpendInput {
            txid: [0x93u8; 32],
            n: 0,
            value: 100_000,
            script_pubkey: spk,
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: keys[0].0,
            pubkey: keys[0].1.clone(),
        };
        let err = coin.assert_owns_prevout().err().unwrap();
        assert!(
            err.contains("script book"),
            "book-less ownership must refuse P2SH, got: {}",
            err
        );
        assert!(
            coin.assert_owns_prevout_with(&crate::multisig::ScriptBook::empty())
                .is_err(),
            "an empty book cannot spend P2SH either"
        );
        let scripts = [(crate::address::hash160(&redeem), redeem.clone())];
        let good = crate::multisig::ScriptBook::from_scripts(&scripts, &keys[..2]);
        coin.assert_owns_prevout_with(&good)
            .expect("book with redeem + m keys accepts");
        let poor = crate::multisig::ScriptBook::from_scripts(&scripts, &keys[..1]);
        let err = coin.assert_owns_prevout_with(&poor).err().unwrap();
        assert!(
            err.contains("only 1 of 2 required keys held"),
            "shortfall must be named, got: {}",
            err
        );
    }

    /// T-D: can_spend is false for an unrelated P2SH (no redeem stored) and
    /// true for ours; non-P2SH scripts pass vacuously.
    #[test]
    fn td_can_spend_distinguishes_our_and_unrelated_p2sh() {
        let keys = ms_keys(&["td/a", "td/b", "td/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let spk_ours = p2sh_spk(&redeem);
        let strangers = ms_keys(&["td/x", "td/y", "td/z"]);
        let other_redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&strangers)).unwrap();
        let spk_other = p2sh_spk(&other_redeem);
        let book = crate::multisig::ScriptBook::from_scripts(
            &[(crate::address::hash160(&redeem), redeem.clone())],
            &keys[..2],
        );
        assert!(book.can_spend(&spk_ours).is_ok(), "our P2SH is spendable");
        let err = book.can_spend(&spk_other).err().unwrap();
        assert!(
            err.contains("no redeem script"),
            "unrelated P2SH must fail can_spend, got: {}",
            err
        );
        assert!(
            book.can_spend(&script_pubkey_p2pkh(&keys[0].1)).is_ok(),
            "non-P2SH needs no book"
        );
    }

    /// T-E: assembly derives m from the redeem script (a 3-of-3 with two
    /// sigs fails even though 2-of-3 would pass with the same slots) and
    /// rejects a slot count that does not match the pubkey count.
    #[test]
    fn te_assemble_derives_m_and_validates_slot_count() {
        let keys = ms_keys(&["te/a", "te/b", "te/c"]);
        let pks = ms_pubkeys(&keys);
        let redeem3 = crate::multisig::redeem_script(3, &pks).unwrap();
        let tx = ms_tx([0x94u8; 32]);
        let partial3 =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem3, &keys[..2]).unwrap();
        assert_eq!(partial3.iter().flatten().count(), 2);
        let err = crate::multisig::assemble_multisig_scriptsig(&partial3, &redeem3)
            .err()
            .unwrap();
        assert!(
            err.contains("missing signatures: have 2 but need at least 3"),
            "m must come from the script, got: {}",
            err
        );
        // slot count must equal the redeem script's pubkey count
        let redeem2 = crate::multisig::redeem_script(2, &pks).unwrap();
        let partial2 =
            crate::multisig::sign_multisig_partial(&tx, 0, &redeem2, &keys[..2]).unwrap();
        let two_slots: Vec<Option<Vec<u8>>> = partial2[..2].to_vec();
        let err = crate::multisig::assemble_multisig_scriptsig(&two_slots, &redeem2)
            .err()
            .unwrap();
        assert!(
            err.contains("slot count 2 does not match the redeem script's 3 pubkeys"),
            "slot/pubkey mismatch must be rejected, got: {}",
            err
        );
        crate::multisig::assemble_multisig_scriptsig(&partial2, &redeem2)
            .expect("exact slots assemble");
    }

    /// T-F: a pool holding an unrelated (bigger) P2SH UTXO skips it and
    /// still builds a valid single-key spend.
    #[test]
    fn tf_pool_with_unrelated_p2sh_skips_and_builds_p2pkh() {
        let p2pkh_coin = coin(7, 1_000_000);
        let strangers = ms_keys(&["tf/x", "tf/y", "tf/z"]);
        let other_redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&strangers)).unwrap();
        // the unrelated P2SH coin is WORTH MORE, so adoption would change
        // the selection — it must be skipped instead
        let p2sh_coin = SpendInput {
            txid: [0x95u8; 32],
            n: 0,
            value: 2_000_000,
            script_pubkey: p2sh_spk(&other_redeem),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: [0u8; 32],
            pubkey: Vec::new(),
        };
        let book = crate::multisig::ScriptBook::empty();
        let dest = script_pubkey_p2pk(&synthetic_key("tf/dest").1).unwrap();
        let change = script_pubkey_p2pkh(&synthetic_key("tf/change").1);
        let plan = build_spend_outputs_floor_with_book(
            &[p2sh_coin, p2pkh_coin.clone()],
            vec![Vout {
                value: 100_000,
                script_pubkey: dest,
            }],
            Some(change),
            0,
            0,
            &book,
            1_400_003_600,
            200,
        )
        .expect("the pool must build without the unrelated P2SH coin");
        assert_eq!(plan.tx.vin.len(), 1, "only the P2PKH coin is adopted");
        assert_eq!(plan.inputs_value, 1_000_000, "the P2SH value is not counted");
        verify_input(&plan.tx, 0, &p2pkh_coin.script_pubkey)
            .expect("the single-key input verifies");
        connect_inputs_check(&plan.tx, &[p2pkh_coin], 200)
            .expect("connect_inputs_check passes");
    }

    // ---- round 3, blocker B: per-input fee/size estimation ----

    /// 1-in/2-out P2PKH estimate is UNCHANGED: 148 + (4+4+1+1+4) + 2*(8+1+25)
    /// = 230 — the exact number the count-based formula always produced, and
    /// the per-input function reproduces it for a P2PKH coin (148) and a
    /// bare-P2PK coin (114).
    #[test]
    fn p2pkh_estimate_unchanged_and_per_input_matches() {
        assert_eq!(
            estimate_signed_size(1, 2),
            148 + (4 + 4 + 1 + 1 + 4) + 2 * (8 + 1 + 25),
            "1-in/2-out P2PKH estimate must stay 230 bytes"
        );
        let empty = crate::multisig::ScriptBook::empty();
        // P2PKH coin: 148 per input — identical to the old estimate
        let p2pkh = coin(3, 1_000_000);
        assert_eq!(
            estimate_signed_size_for(&[p2pkh], 2, &empty),
            14 + 148 + 2 * 34,
            "per-input P2PKH estimate must be 230"
        );
        // bare-P2PK coin: 32+4+1+(1+72)+4 = 114
        let pk = synthetic_key("est/p2pk").1;
        let p2pk = SpendInput {
            txid: [0x21u8; 32],
            n: 0,
            value: 1_000_000,
            script_pubkey: script_pubkey_p2pk(&pk).unwrap(),
            height: 1,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: [1u8; 32],
            pubkey: pk,
        };
        p2pk.assert_owns_prevout().expect("owns the bare-p2pk coin");
        assert_eq!(
            estimate_signed_size_for(&[p2pk], 2, &empty),
            14 + 114 + 2 * 34,
            "per-input bare-P2PK estimate must be 196"
        );
    }

    /// The 2-of-3 P2SH estimate lands within 5 bytes of the REAL serialized
    /// size of the signed transaction (both numbers asserted). Estimate for
    /// the 105-byte 2-of-3 redeem: 14 + (32+4+1 + 2*74 + 1 + 105 + 4) + 2*34
    /// = 377; the real scriptSig's DER sigs are 71-72 bytes, so reality is
    /// within a couple of bytes.
    #[test]
    fn two_of_three_estimate_matches_real_signed_size_within_5_bytes() {
        let keys = ms_keys(&["est2/a", "est2/b", "est2/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        assert_eq!(redeem.len(), 105, "2-of-3 redeem length");
        let book = crate::multisig::ScriptBook::from_scripts(
            &[(crate::address::hash160(&redeem), redeem.clone())],
            &keys[..2],
        );
        let p2sh_coin = SpendInput {
            txid: [0x96u8; 32],
            n: 0,
            value: 500_000,
            script_pubkey: p2sh_spk(&redeem),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: keys[0].0,
            pubkey: keys[0].1.clone(),
        };
        p2sh_coin
            .assert_owns_prevout_with(&book)
            .expect("the book owns the coin");

        let est = estimate_signed_size_for(&[p2sh_coin.clone()], 2, &book);
        assert_eq!(
            est,
            14 + (32 + 4 + 1 + 2 * (1 + 72 + 1) + 1 + 105 + 4) + 2 * 34,
            "estimate formula: base + 2-of-3 input + 2 P2PKH outputs = 377"
        );

        // BOTH outputs P2PKH — the estimator's per-output shape (8+1+25)
        let dest = script_pubkey_p2pkh(&synthetic_key("est2/dest").1);
        let change = script_pubkey_p2pkh(&synthetic_key("est2/change").1);
        let plan = build_spend_outputs_floor_with_book(
            &[p2sh_coin],
            vec![Vout {
                value: 100_000,
                script_pubkey: dest,
            }],
            Some(change),
            0,
            0,
            &book,
            1_400_003_600,
            200,
        )
        .expect("2-of-3 spend must build");
        let real = plan.tx.serialize().len();
        assert!(
            est.abs_diff(real) <= 5,
            "estimate {} must be within 5 bytes of the real signed size {}",
            est,
            real
        );
        println!(
            "round3: 2-of-3 P2SH estimate = {} bytes, real signed size = {} bytes (diff {})",
            est,
            real,
            est.abs_diff(real)
        );
    }

    /// A mixed pool (1 P2PKH + 1 2-of-3 P2SH) converges through the
    /// per-input fee schedule and builds a transaction whose fee is exactly
    /// the converged quote — every input verifies.
    #[test]
    fn mixed_p2pkh_p2sh_pool_converges_and_builds() {
        let keys = ms_keys(&["est3/a", "est3/b", "est3/c"]);
        let redeem = crate::multisig::redeem_script(2, &ms_pubkeys(&keys)).unwrap();
        let book = crate::multisig::ScriptBook::from_scripts(
            &[(crate::address::hash160(&redeem), redeem.clone())],
            &keys[..2],
        );
        let p2pkh_coin = coin(9, 1_000_000);
        let p2sh_coin = SpendInput {
            txid: [0x97u8; 32],
            n: 0,
            value: 1_000_000,
            script_pubkey: p2sh_spk(&redeem),
            height: 100,
            tx_time: 1_400_000_000,
            is_coinbase: false,
            is_coinstake: false,
            secret: keys[0].0,
            pubkey: keys[0].1.clone(),
        };
        let amount = 1_500_000i64;
        let shape = select_with_fee_with_book(
            &[p2pkh_coin.clone(), p2sh_coin.clone()],
            &[amount],
            MIN_FEE_SATS,
            &book,
        )
        .expect("the mixed pool must converge");
        assert_eq!(shape.n_inputs, 2, "both coins adopted");
        // the quote schedules the per-input sizes: 14 + (148 + 295) + 2*34
        assert_eq!(
            estimate_signed_size_for(&shape.selected, shape.n_outputs, &book),
            14 + (148 + (32 + 4 + 1 + 2 * (1 + 72 + 1) + 1 + 105 + 4)) + 2 * 34,
        );

        let dest = script_pubkey_p2pk(&synthetic_key("est3/dest").1).unwrap();
        let change = script_pubkey_p2pkh(&synthetic_key("est3/change").1);
        let plan = build_spend_outputs_floor_with_book(
            &shape.selected,
            vec![Vout {
                value: amount,
                script_pubkey: dest,
            }],
            Some(change),
            MIN_FEE_SATS,
            shape.fee,
            &book,
            1_400_003_600,
            200,
        )
        .expect("the mixed spend must build and sign");
        // the exact-equality the GUI guard applies: built fee == quoted fee
        assert_eq!(
            plan.fee, shape.fee,
            "the signed transaction pays exactly the converged quote"
        );
        for (i, input) in shape.selected.iter().enumerate() {
            verify_input(&plan.tx, i, &input.script_pubkey)
                .unwrap_or_else(|e| panic!("input {} must verify: {}", i, e));
        }
        connect_inputs_check(&plan.tx, &shape.selected, 200)
            .expect("connect_inputs_check passes");
    }
}
