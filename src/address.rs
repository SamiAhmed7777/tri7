//! Legacy script → address extraction (Solver-era destination forms) and
//! base58check encoding, phase 1e. The chain carries exactly three payable
//! script forms (triangles_v5 script.cpp Solver / CTxOut::IsStandard):
//!
//!   - bare pubkey:  <push 33|65> <key> OP_CHECKSIG          (0x21/0x41 ... 0xac)
//!   - P2PKH:        OP_DUP OP_HASH160 <push 20> <h160> OP_EQUALVERIFY OP_CHECKSIG
//!                  = 76 a9 14 <20> 88
//!   - P2SH:         OP_HASH160 <push 20> <h160> OP_EQUAL    = a9 14 <20> 87
//!
//! Address versions follow base58.h CBitcoinAddress mainnet constants:
//! PUBKEY_ADDRESS = 65 (0x41, 'T' addresses), SCRIPT_ADDRESS = 28 (0x1c,
//! 'C' addresses). Encodings are verified against the live daemon
//! (validateaddress) vectors in the unit tests below.

use crate::legacy::sha256d;
use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

pub const PUBKEY_ADDRESS: u8 = 65; // 0x41
pub const SCRIPT_ADDRESS: u8 = 28; // 0x1c

/// A parsed destination. `payload()` builds the 21-byte base58check input
/// (version byte + hash160); `is_pubkey_hash()` distinguishes the P2PKH form,
/// which is the ONLY form the daemon's getreceivedbyaddress script-equality
/// matches (a bare-pubkey output to the same key is a different script and
/// never tallied for that address).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptDest {
    /// 76 a9 14 <20> 88
    PubkeyHash([u8; 20]),
    /// a9 14 <20> 87
    ScriptHash([u8; 20]),
    /// bare compressed key (0x21 push)
    Pubkey33([u8; 33]),
    /// bare uncompressed key (0x41 push)
    Pubkey65([u8; 65]),
}

impl ScriptDest {
    /// Version byte + hash160 — the base58check payload of the address.
    pub fn payload(&self) -> [u8; 21] {
        match self {
            ScriptDest::PubkeyHash(h) => payload_for(PUBKEY_ADDRESS, h),
            ScriptDest::ScriptHash(h) => payload_for(SCRIPT_ADDRESS, h),
            ScriptDest::Pubkey33(k) => payload_for(PUBKEY_ADDRESS, &hash160(k)),
            ScriptDest::Pubkey65(k) => payload_for(PUBKEY_ADDRESS, &hash160(k)),
        }
    }

    /// True only for the P2PKH form (daemon getreceivedbyaddress compares
    /// scriptPubKey bytes against the address's P2PKH script — other forms
    /// holding the same key are distinct scripts and never match).
    pub fn is_pubkey_hash(&self) -> bool {
        matches!(self, ScriptDest::PubkeyHash(_))
    }
}

fn payload_for(version: u8, hash: &[u8; 20]) -> [u8; 21] {
    let mut p = [0u8; 21];
    p[0] = version;
    p[1..].copy_from_slice(hash);
    p
}

/// base58check payload for a P2SH address of `script` (hash160(script) with
/// the script-address version byte) — the legacy `decodescript` p2sh field.
pub fn script_hash_payload(script: &[u8]) -> [u8; 21] {
    payload_for(SCRIPT_ADDRESS, &hash160(script))
}

/// Address payload for a raw public key (wallet keys → their P2PKH address).
pub fn pubkey_payload(key: &[u8]) -> [u8; 21] {
    payload_for(PUBKEY_ADDRESS, &hash160(key))
}

/// hash160 = RIPEMD160(SHA256(x)) — the legacy Hash160.
pub fn hash160(data: &[u8]) -> [u8; 20] {
    let h = Sha256::digest(data);
    let h = Ripemd160::digest(h);
    let mut out = [0u8; 20];
    out.copy_from_slice(&h);
    out
}

/// Solver port: classify a scriptPubKey into its destination, None when the
/// script is not one of the three payable forms (OP_RETURN, multisig,
/// non-standard junk — unattributable to an address).
pub fn script_to_dest(script: &[u8]) -> Option<ScriptDest> {
    // P2PKH: exact 25 bytes 76 a9 14 <20> 88 ac (OP_EQUALVERIFY + OP_CHECKSIG).
    if script.len() == 25
        && script[0] == 0x76
        && script[1] == 0xa9
        && script[2] == 0x14
        && script[23] == 0x88
        && script[24] == 0xac
    {
        let mut h = [0u8; 20];
        h.copy_from_slice(&script[3..23]);
        return Some(ScriptDest::PubkeyHash(h));
    }
    // P2SH: exact 23 bytes a9 14 <20> 87.
    if script.len() == 23 && script[0] == 0xa9 && script[1] == 0x14 && script[22] == 0x87 {
        let mut h = [0u8; 20];
        h.copy_from_slice(&script[2..22]);
        return Some(ScriptDest::ScriptHash(h));
    }
    // Bare pubkey: reuse the consensus Solver port (shape + key-prefix rules).
    match crate::consensus::bare_pubkey_from_script(script) {
        Some(pk) if pk.len() == 33 => {
            let mut k = [0u8; 33];
            k.copy_from_slice(pk);
            Some(ScriptDest::Pubkey33(k))
        }
        Some(pk) => {
            debug_assert_eq!(pk.len(), 65);
            let mut k = [0u8; 65];
            k.copy_from_slice(pk);
            Some(ScriptDest::Pubkey65(k))
        }
        None => None,
    }
}

/// base58check(payload) — legacy EncodeBase58Check (first 4 bytes of
/// sha256d(data) appended as checksum).
pub fn payload_to_address(payload: &[u8; 21]) -> String {
    let mut full = Vec::with_capacity(25);
    full.extend_from_slice(payload);
    full.extend_from_slice(&sha256d(payload)[..4]);
    base58(&full)
}

/// Legacy DecodeBase58 (leading '1's → zero bytes). Inverse of `base58`.
pub fn base58_decode(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut bytes_map = [255usize; 128];
    for (i, &c) in ALPHABET.iter().enumerate() {
        bytes_map[c as usize] = i;
    }
    let mut zeros = 0usize;
    let chars: Vec<u8> = s.as_bytes().to_vec();
    while zeros < chars.len() && chars[zeros] == b'1' {
        zeros += 1;
    }
    let mut out: Vec<u8> = Vec::with_capacity(chars.len() * 137 / 100 + 1);
    for &ch in chars.iter().skip(zeros) {
        if ch >= 128 || bytes_map[ch as usize] == 255 {
            return Err(format!("invalid base58 byte {:#x}", ch));
        }
        let mut carry = bytes_map[ch as usize];
        for b in out.iter_mut() {
            carry += (*b as usize) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let mut full = vec![0u8; zeros];
    full.extend(out.into_iter().rev());
    Ok(full)
}

/// Decode a base58check address back to its 21-byte payload (version +
/// hash160), verifying the 4-byte checksum — legacy CTrianglesAddress.
pub fn address_to_payload(address: &str) -> Result<[u8; 21], String> {
    let full = base58_decode(address)?;
    if full.len() != 25 {
        return Err(format!("address {}: decoded length {}", address, full.len()));
    }
    let payload = &full[..21];
    let expect = &sha256d(payload)[..4];
    if &full[21..] != expect {
        return Err(format!("address {}: bad base58check checksum", address));
    }
    let mut out = [0u8; 21];
    out.copy_from_slice(payload);
    Ok(out)
}

/// Legacy EncodeBase58 (leading zero bytes → '1's).
pub fn base58(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut zeros = 0usize;
    while zeros < data.len() && data[zeros] == 0 {
        zeros += 1;
    }
    let mut out = Vec::with_capacity(data.len() * 137 / 100 + 1);
    for &byte in &data[zeros..] {
        let mut carry = byte as usize;
        for ch in out.iter_mut() {
            carry += (*ch as usize) << 8;
            *ch = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            out.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut s = String::with_capacity(zeros + out.len());
    for _ in 0..zeros {
        s.push('1');
    }
    for ch in out.iter().rev() {
        s.push(ALPHABET[*ch as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // Daemon-confirmed vector (validateaddress: isvalid=true, ismine=true):
    // this wallet pubkey's address. Both the bare-pubkey form and the P2PKH
    // form of this key must encode to exactly this string.
    const WALLET_PUBKEY: &str =
        "020a3299ffe18ff4abd4e8c4e4a62235dcb6099f23990f75c1de6e19699f4b5314";
    const WALLET_ADDRESS: &str = "TZ4v4GkyNtv2hPxGHtQMfews7Hpj1REgs5";
    // Daemon-confirmed P2SH vector (validateaddress: isvalid=true) for
    // hash160 00112233445566778899aabbccddeeff00112233 at version 28.
    const P2SH_HASH: &str = "00112233445566778899aabbccddeeff00112233";
    const P2SH_ADDRESS: &str = "CGUF6WCiYvhYTZwpmPU8fjaJcgXhWzng5W";
    // Daemon-confirmed address (validateaddress: isvalid=true) of the SAME
    // key serialized UNCOMPRESSED — a different hash160 input, hence a
    // different address string: bare-pubkey attribution follows the key bytes
    // the script actually carries.
    const WALLET_PUBKEY_UNCOMPRESSED_ADDRESS: &str = "TUbCqmzp7jH934PZwSVb596NbkDw59ck3i";
    // Live sweep-migration vector (2026-11-10 incident, daemon listunspent):
    // UTXO 4d0be43c…:0 scriptPubKey 76 a9 14 86cb0e9408ee60c88d72a126801a29904d7770df 88 ac
    // pays to exactly this VALID address. (The incident notes quote the
    // hash160 chunk as "…0df88" — the trailing "88" is the script's
    // OP_EQUALVERIFY opcode absorbed into the quoted text; the hash160 is the
    // 20 bytes ending …0df.)
    const SWEEP_HASH160: &str = "86cb0e9408ee60c88d72a126801a29904d7770df";
    const SWEEP_ADDRESS: &str = "TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7";
    // The wallet's STORED string for the same key — legacy BDB→SQLite import
    // era corruption, two base58 characters differ (VD**j**→VD**J**,
    // E**G**h→E**C**h). Daemon validateaddress: isvalid=false.
    const SWEEP_ADDRESS_CORRUPTED: &str = "TNFvoQdbk7VDJ8oZD1EbKkrPmZAECh6Md7";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn p2pkh_extraction_matches_daemon_address() {
        let pk = unhex(WALLET_PUBKEY);
        // raw key → payload → address
        let payload = pubkey_payload(&pk);
        assert_eq!(payload[0], PUBKEY_ADDRESS);
        assert_eq!(payload_to_address(&payload), WALLET_ADDRESS);
        // P2PKH script 76 a9 14 <h160> 88 ac → same address, P2PKH form
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(&hash160(&pk));
        script.extend_from_slice(&[0x88, 0xac]);
        let dest = script_to_dest(&script).expect("p2pkh must parse");
        assert!(dest.is_pubkey_hash());
        assert_eq!(dest.payload(), payload);
        assert_eq!(payload_to_address(&dest.payload()), WALLET_ADDRESS);
    }

    #[test]
    fn p2sh_extraction_uses_script_version_28() {
        let h = unhex(P2SH_HASH);
        let mut script = vec![0xa9, 0x14];
        script.extend_from_slice(&h);
        script.push(0x87);
        let dest = script_to_dest(&script).expect("p2sh must parse");
        assert!(!dest.is_pubkey_hash());
        let payload = dest.payload();
        assert_eq!(
            payload[0], SCRIPT_ADDRESS,
            "P2SH version byte must be 28/0x1c"
        );
        assert_eq!(&payload[1..], &h[..]);
        assert_eq!(payload_to_address(&payload), P2SH_ADDRESS);
        // the same hash at the pubkey version encodes differently
        let h_arr: [u8; 20] = h.as_slice().try_into().unwrap();
        assert_ne!(
            payload_to_address(&payload_for(PUBKEY_ADDRESS, &h_arr)),
            P2SH_ADDRESS
        );
    }

    #[test]
    fn bare_pubkey_extraction_compressed_and_uncompressed() {
        let compressed = unhex(WALLET_PUBKEY);
        // 0x21 <key> 0xac
        let mut script = vec![0x21u8];
        script.extend_from_slice(&compressed);
        script.push(0xac);
        let dest = script_to_dest(&script).expect("bare compressed key must parse");
        assert!(!dest.is_pubkey_hash(), "bare pubkey is NOT the P2PKH form");
        assert_eq!(payload_to_address(&dest.payload()), WALLET_ADDRESS);
        // 0x41 <uncompressed key> 0xac — same point, same address payload
        let uncompressed = secp256k1::PublicKey::from_slice(&compressed)
            .expect("valid compressed key")
            .serialize_uncompressed();
        assert_eq!(uncompressed.len(), 65);
        assert_eq!(uncompressed[0], 4);
        let mut script = vec![0x41u8];
        script.extend_from_slice(&uncompressed);
        script.push(0xac);
        let dest = script_to_dest(&script).expect("bare uncompressed key must parse");
        assert_eq!(
            payload_to_address(&dest.payload()),
            WALLET_PUBKEY_UNCOMPRESSED_ADDRESS
        );
    }

    #[test]
    fn sweep_incident_hash160_encodes_to_the_live_address() {
        // hash160 → payload (version byte copied from the constants above) →
        // address must reproduce the daemon-confirmed live string exactly.
        let h: [u8; 20] = unhex(SWEEP_HASH160).as_slice().try_into().unwrap();
        let payload = payload_for(PUBKEY_ADDRESS, &h);
        assert_eq!(payload[0], PUBKEY_ADDRESS);
        assert_eq!(payload_to_address(&payload), SWEEP_ADDRESS);
        // the same hash straight out of the full P2PKH scriptPubKey
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(&h);
        script.extend_from_slice(&[0x88, 0xac]);
        let dest = script_to_dest(&script).expect("live P2PKH script must parse");
        assert!(dest.is_pubkey_hash());
        assert_eq!(dest.payload(), payload);
        assert_eq!(payload_to_address(&dest.payload()), SWEEP_ADDRESS);
        // the encoder is therefore NOT the bug: it was already producing the
        // valid string — the incident was the string-keyed key lookup.
        assert_ne!(SWEEP_ADDRESS_CORRUPTED, SWEEP_ADDRESS);
    }

    #[test]
    fn corrupted_sweep_address_fails_checksum_validation() {
        // Both corrupted characters are legal base58 alphabet members (so the
        // corruption was silent at store time), yet the string must FAIL
        // checksum validation: encoder and validator agree the corrupted
        // string is not an address and can never equal a freshly-encoded one.
        for ch in SWEEP_ADDRESS_CORRUPTED.chars() {
            assert!(
                "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
                    .contains(ch),
                "char {ch} must be alphabet-legal for this corruption shape"
            );
        }
        let err = address_to_payload(SWEEP_ADDRESS_CORRUPTED)
            .expect_err("corrupted incident string must fail validation");
        assert!(err.contains("checksum"), "got: {err}");
        // the VALID form decodes fine, to exactly the incident hash160 at the
        // pubkey version byte
        let payload = address_to_payload(SWEEP_ADDRESS).expect("valid form decodes");
        assert_eq!(payload[0], PUBKEY_ADDRESS);
        assert_eq!(&payload[1..], unhex(SWEEP_HASH160).as_slice());
    }

    #[test]
    fn malformed_and_nonstandard_scripts_rejected() {
        let h = [0x11u8; 20];
        // empty / too short
        assert_eq!(script_to_dest(&[]), None);
        assert_eq!(script_to_dest(&[0x76, 0xa9]), None);
        // P2PKH truncated (23-byte script missing the 0x88 0xac tail) and
        // 0x88 tail without the mandatory OP_CHECKSIG
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&h);
        assert_eq!(
            script_to_dest(&s),
            None,
            "missing OP_EQUALVERIFY/OP_CHECKSIG tail"
        );
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&h);
        s.push(0x88);
        assert_eq!(
            script_to_dest(&s),
            None,
            "OP_EQUALVERIFY alone is not P2PKH"
        );
        let mut s = vec![0x76, 0xa9, 0x13];
        s.extend_from_slice(&h[..19]);
        s.extend_from_slice(&[0x88]);
        assert_eq!(script_to_dest(&s), None, "short hash push");
        // P2SH with OP_EQUALVERIFY tail is not the P2SH form (a9 14 <20> 88)
        let mut s = vec![0xa9, 0x14];
        s.extend_from_slice(&h);
        s.extend_from_slice(&[0x88]);
        assert_eq!(script_to_dest(&s), None);
        // bare pubkey with wrong length / wrong tail / bad key prefix
        assert_eq!(script_to_dest(&[0x20; 1]), None);
        let mut s = vec![0x21u8];
        s.extend_from_slice(&[2u8; 33]);
        s.extend_from_slice(&[0x88]);
        assert_eq!(
            script_to_dest(&s),
            None,
            "pubkey push must end in OP_CHECKSIG"
        );
        let mut s = vec![0x21u8];
        s.extend_from_slice(&[5u8; 33]); // invalid key prefix
        s.extend_from_slice(&[0xac]);
        assert_eq!(script_to_dest(&s), None, "key prefix must be 02/03/04");
        // OP_RETURN-style junk
        assert_eq!(script_to_dest(&[0x6a, 0x04, 1, 2, 3, 4]), None);
    }
}
