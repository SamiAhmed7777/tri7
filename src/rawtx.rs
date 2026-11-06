//! Raw-transaction codec + legacy-shaped decode (rpcrawtransaction parity).
//!
//! Implements the offline workflow the legacy Qt client exposed:
//!   * `decoderawtransaction <hex>` — full decode, legacy JSON shape
//!   * `decodescript <hex>`         — asm / type / addresses / reqSigs
//!   * `createrawtransaction [...]` — unsigned tx from inputs + outputs
//!
//! Signing lives in the ops adapter (`signrawtransaction`) because it needs the
//! node's live wallet key view. This module is pure: no I/O, no wallet access,
//! so every decoder is unit-testable and can never touch the chain.
use crate::address;
use crate::legacy::{Reader, Tx, Vin, Vout};
use crate::spend::{OP_CHECKSIG, OP_DUP, OP_EQUAL, OP_EQUALVERIFY, OP_HASH160, OP_RETURN};
use serde_json::{json, Value};

pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Parse a raw transaction, refusing trailing bytes (a tx is exactly one
/// serialized object; trailing data means the caller pasted the wrong string).
pub fn tx_from_hex(hex_str: &str) -> Result<Tx, String> {
    let raw = crate::p2p::hex_decode(hex_str).map_err(|e| format!("bad tx hex: {e}"))?;
    let mut r = Reader::new(&raw);
    let tx = Tx::read(&mut r).map_err(|e| format!("tx decode: {e}"))?;
    r.expect_done()
        .map_err(|e| format!("tx has trailing bytes: {e}"))?;
    Ok(tx)
}

/// ScriptPubKey → (legacy type name, addresses, reqSigs).
pub fn script_shape(script: &[u8]) -> (&'static str, Vec<String>, u32) {
    if let Some(dest) = address::script_to_dest(script) {
        return (
            "pubkeyhash",
            vec![address::payload_to_address(&dest.payload())],
            1,
        );
    }
    if let Some(pk) = crate::consensus::bare_pubkey_from_script(script) {
        let payload = address::pubkey_payload(pk);
        return ("pubkey", vec![address::payload_to_address(&payload)], 1);
    }
    if script.first() == Some(&OP_RETURN) {
        return ("nulldata", Vec::new(), 0);
    }
    ("nonstandard", Vec::new(), 0)
}

/// Minimal opcode/push disassembler — enough for this chain's script forms
/// (P2PKH, bare P2PK, OP_RETURN) and an honest hex fallback for anything else.
pub fn script_asm(script: &[u8]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < script.len() {
        let op = script[i];
        i += 1;
        match op {
            0x00 => out.push("0".into()),
            0x01..=0x4b => {
                let n = op as usize;
                if i + n > script.len() {
                    out.push(format!("[error: push {} past end]", n));
                    break;
                }
                out.push(to_hex(&script[i..i + n]));
                i += n;
            }
            0x4c => {
                // PUSHDATA1
                if i >= script.len() {
                    out.push("[error: PUSHDATA1 past end]".into());
                    break;
                }
                let n = script[i] as usize;
                i += 1;
                if i + n > script.len() {
                    out.push("[error: PUSHDATA1 data past end]".into());
                    break;
                }
                out.push(to_hex(&script[i..i + n]));
                i += n;
            }
            0x4d => {
                // PUSHDATA2 (LE length)
                if i + 2 > script.len() {
                    out.push("[error: PUSHDATA2 past end]".into());
                    break;
                }
                let n = u16::from_le_bytes([script[i], script[i + 1]]) as usize;
                i += 2;
                if i + n > script.len() {
                    out.push("[error: PUSHDATA2 data past end]".into());
                    break;
                }
                out.push(to_hex(&script[i..i + n]));
                i += n;
            }
            0x51..=0x60 => out.push(format!("{}", op - 0x50)),
            OP_DUP => out.push("OP_DUP".into()),
            OP_HASH160 => out.push("OP_HASH160".into()),
            OP_EQUAL => out.push("OP_EQUAL".into()),
            OP_EQUALVERIFY => out.push("OP_EQUALVERIFY".into()),
            OP_CHECKSIG => out.push("OP_CHECKSIG".into()),
            OP_RETURN => out.push("OP_RETURN".into()),
            0xae => out.push("OP_CHECKMULTISIG".into()),
            other => out.push(format!("OP_UNKNOWN(0x{:02x})", other)),
        }
    }
    out.join(" ")
}

/// `decoderawtransaction`'s scriptPubKey / `decodescript` body.
pub fn decode_script(script: &[u8]) -> Value {
    let (ty, addresses, req) = script_shape(script);
    json!({
        "asm": script_asm(script),
        "hex": to_hex(script),
        "type": ty,
        "reqSigs": req,
        "addresses": addresses,
    })
}

fn vin_json(v: &Vin) -> Value {
    let is_coinbase = v.prev_hash == [0u8; 32] && v.prev_n == u32::MAX;
    if is_coinbase {
        return json!({
            "coinbase": to_hex(&v.script_sig),
            "sequence": v.sequence,
        });
    }
    json!({
        "txid": crate::p2p::internal_to_display(&v.prev_hash),
        "vout": v.prev_n,
        "scriptSig": {
            "asm": script_asm(&v.script_sig),
            "hex": to_hex(&v.script_sig),
        },
        "sequence": v.sequence,
    })
}

/// Legacy verbose transaction JSON. `include_hex` is true for
/// decoderawtransaction (legacy echoes the input hex) and false for nested
/// script/vin bodies.
pub fn tx_json(tx: &Tx, include_hex: bool) -> Value {
    let raw = tx.serialize();
    let vin: Vec<Value> = tx.vin.iter().map(vin_json).collect();
    let vout: Vec<Value> = tx
        .vout
        .iter()
        .enumerate()
        .map(|(n, o)| {
            json!({
                "value": crate::rpc::sats_to_tri(o.value),
                "n": n,
                "scriptPubKey": decode_script(&o.script_pubkey),
            })
        })
        .collect();
    let mut body = json!({
        "txid": crate::p2p::internal_to_display(&tx.hash()),
        "version": tx.version,
        "time": tx.time,
        "locktime": tx.locktime,
        "size": raw.len(),
        "vin": vin,
        "vout": vout,
    });
    if include_hex {
        body["hex"] = json!(to_hex(&raw));
    }
    body
}

/// Address → P2PKH scriptPubKey (the only destination form this chain pays).
pub fn p2pkh_script(payload: &[u8; 21]) -> Vec<u8> {
    let mut s = Vec::with_capacity(25);
    s.push(OP_DUP);
    s.push(OP_HASH160);
    s.push(20);
    s.extend_from_slice(&payload[1..]);
    s.push(OP_EQUALVERIFY);
    s.push(OP_CHECKSIG);
    s
}

/// `createrawtransaction [{"txid":..,"vout":..}] {"addr":amount,...}`
///
/// Outputs are ordered as the JSON object iterates (serde_json's map order is
/// the insertion order for a parsed object with the `preserve_order` feature;
/// without it the order is sorted — either way the caller gets a deterministic
/// tx, and the txid is returned so it can be compared cheaply).
///
/// The legacy `data` destination is a RAW SCRIPT in hex, not an amount (the
/// legacy client writes it as-is, so a caller can place any script there).
/// It is passed separately so a plain satoshi amount can never be mistaken
/// for a script.
pub fn create_raw(
    inputs: &[(String, u32)],
    outputs: &[(String, i64)],
    scripts: &[(String, Vec<u8>)],
    time: u32,
) -> Result<Tx, String> {
    if inputs.is_empty() {
        return Err("createrawtransaction: at least one input is required".into());
    }
    if outputs.is_empty() && scripts.is_empty() {
        return Err("createrawtransaction: at least one output is required".into());
    }
    let mut vin = Vec::with_capacity(inputs.len());
    for (txid, n) in inputs {
        let hash = crate::p2p::display_to_internal(txid)
            .map_err(|e| format!("createrawtransaction: bad txid {txid}: {e}"))?;
        vin.push(Vin {
            prev_hash: hash,
            prev_n: *n,
            script_sig: Vec::new(),
            sequence: u32::MAX,
        });
    }
    let mut vout = Vec::with_capacity(outputs.len() + scripts.len());
    for (dest, sats) in outputs {
        if *sats < 0 {
            return Err("createrawtransaction: negative output amount".into());
        }
        if dest == "data" {
            return Err(
                "createrawtransaction: \"data\" takes a raw script hex, not an amount".into(),
            );
        }
        let payload = address::address_to_payload(dest)
            .map_err(|e| format!("createrawtransaction: invalid address {dest}: {e}"))?;
        vout.push(Vout {
            value: *sats,
            script_pubkey: p2pkh_script(&payload),
        });
    }
    // raw-script outputs (legacy "data" keys): emitted verbatim, value 0
    for (name, script) in scripts {
        if name != "data" {
            return Err(format!("createrawtransaction: unknown non-address output {name}"));
        }
        if script.is_empty() {
            return Err("createrawtransaction: empty data script".into());
        }
        vout.push(Vout {
            value: 0,
            script_pubkey: script.clone(),
        });
    }
    Ok(Tx {
        version: 1,
        time,
        vin,
        vout,
        locktime: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tx() -> Tx {
        Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [7u8; 32],
                prev_n: 3,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 1_500_000,
                script_pubkey: p2pkh_script(&[0x21; 21]),
            }],
            locktime: 0,
        }
    }

    #[test]
    fn round_trip_hex_and_json_shape() {
        let tx = sample_tx();
        let hex = to_hex(&tx.serialize());
        let back = tx_from_hex(&hex).expect("re-decode");
        assert_eq!(back.vin.len(), 1);
        assert_eq!(back.vout[0].value, 1_500_000);
        let j = tx_json(&back, true);
        assert_eq!(j["vin"][0]["vout"], 3);
        assert_eq!(j["vout"][0]["value"], 1.5);
        assert_eq!(j["vout"][0]["scriptPubKey"]["type"], "pubkeyhash");
        assert!(j["vout"][0]["scriptPubKey"]["addresses"][0]
            .as_str()
            .unwrap()
            .starts_with('T'));
        assert_eq!(j["hex"], hex);
        assert_eq!(j["size"], back.serialize().len());
    }

    #[test]
    fn asm_matches_standard_p2pkh() {
        let script = p2pkh_script(&[0x21; 21]);
        let asm = script_asm(&script);
        assert!(asm.starts_with("OP_DUP OP_HASH160 "));
        assert!(asm.ends_with(" OP_EQUALVERIFY OP_CHECKSIG"));
        assert_eq!(script_shape(&script).0, "pubkeyhash");
    }

    #[test]
    fn create_raw_refuses_garbage_and_builds_outputs() {
        assert!(create_raw(&[], &[], &[], 0).is_err());
        let txid = crate::p2p::internal_to_display(&[9u8; 32]);
        let script = vec![OP_RETURN, 2, 0xaa, 0xbb];
        let tx = create_raw(&[(txid.clone(), 1)], &[], &[("data".into(), script)], 1_700_000_001)
            .expect("create");
        assert_eq!(tx.vin[0].prev_hash, [9u8; 32]);
        assert_eq!(tx.vout[0].value, 0);
        assert_eq!(tx.vout[0].script_pubkey[0], OP_RETURN);
        // an amount under "data" is refused instead of being read as a script
        assert!(create_raw(&[(txid, 1)], &[("data".into(), 5)], &[], 0).is_err());
        assert!(create_raw(&[("zz".into(), 0)], &[], &[], 0).is_err());
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut raw = sample_tx().serialize();
        raw.push(0x00);
        assert!(tx_from_hex(&to_hex(&raw)).is_err());
    }
}
