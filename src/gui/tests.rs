//! GUI state-machine test suite (phase 6) — headless, fixture-based only
//! (no /root/.triangles dependency). The REAL wallet exercise is
//! `tri7_wallet_gui --selfcheck`. Compiled as a lib unit-test module so the
//! suite stays runnable even while sibling bins are being reworked.

use crate::address::{hash160, payload_to_address, script_to_dest};
use crate::gui::app_state::{parse_console_line, OptionsState};
use crate::gui::rpc_client;
use crate::gui::state as gs;
use crate::gui::state::*;
use crate::legacy::{Block, Tx, Vin, Vout};
use crate::spend;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// amounts
// ---------------------------------------------------------------------------

#[test]
fn format_tri_matches_legacy_6dp() {
    assert_eq!(format_tri(0), "0.000000");
    assert_eq!(format_tri(1), "0.000001");
    assert_eq!(format_tri(1_000_000), "1.000000");
    assert_eq!(format_tri(3_036_990_591), "3036.990591"); // real getbalance total
    assert_eq!(format_tri(222_507_326_452), "222507.326452"); // phase-1e total
    assert_eq!(format_tri(-100), "-0.000100");
    assert_eq!(format_tri(spend::MIN_TX_FEE), "0.000100");
}

#[test]
fn parse_tri_accepts_legacy_strings_and_rejects_junk() {
    assert_eq!(parse_tri("0.0001").unwrap(), 100);
    assert_eq!(parse_tri("12.5").unwrap(), 12_500_000);
    assert_eq!(parse_tri("12").unwrap(), 12_000_000);
    assert_eq!(parse_tri(" 0.000100 ").unwrap(), 100);
    assert!(parse_tri("0.0000005").is_err()); // > 6 decimals
    assert!(parse_tri("-1").is_err());
    assert!(parse_tri("abc").is_err());
    assert!(parse_tri("").is_err());
    assert!(parse_tri("1.2.3").is_err());
    assert!(parse_tri("999999999999999").is_err()); // > MoneyRange
}

#[test]
fn amount_roundtrip() {
    for sats in [0i64, 1, 99, 100, 1234567, spend::MAX_MONEY] {
        assert_eq!(parse_tri(&format_tri(sats)).unwrap(), sats);
    }
}

// ---------------------------------------------------------------------------
// addresses (real chain-derived vector from wallet_utxos.json)
// ---------------------------------------------------------------------------

const REAL_ADDR: &str = "TNFvoQdbk7VDj8oZD1EbKkrPmZAEGh6Md7";
const REAL_SCRIPT: &str = "76a91486cb0e9408ee60c88d72a126801a29904d7770df88ac";

#[test]
fn validate_address_accepts_real_wallet_vector() {
    let payload = validate_address(REAL_ADDR).unwrap();
    let script = p2pkh_script_for(&payload);
    let expect = hex_decode(REAL_SCRIPT).unwrap();
    assert_eq!(
        script, expect,
        "P2PKH script must match the live UTXO dump bytes"
    );
    assert_eq!(payload[0], crate::address::PUBKEY_ADDRESS);
}

#[test]
fn validate_address_rejects_tamper_and_foreign_versions() {
    assert!(validate_address("").is_err());
    // tamper one char (checksum break)
    let mut bad = REAL_ADDR.to_string();
    let last = bad.pop().unwrap();
    bad.push(if last == 'z' { 'y' } else { 'z' });
    assert!(validate_address(&bad).is_err());
    // invalid base58 chars
    for c in ['0', 'O', 'I', 'l'] {
        assert!(validate_address(&format!("{}{}", REAL_ADDR, c)).is_err());
    }
    // P2SH version (28, 'C…') must be refused — GUI sends to pubkey addresses only
    let mut p2sh = [28u8; 21];
    p2sh[1..].copy_from_slice(&hash160(b"not-a-wallet-key"));
    let p2sh_addr = payload_to_address(&p2sh);
    assert!(p2sh_addr.starts_with('C'));
    assert!(validate_address(&p2sh_addr).is_err());
}

#[test]
fn base58_decode_roundtrips_the_encoder() {
    let payload = validate_address(REAL_ADDR).unwrap();
    let encoded = payload_to_address(&payload);
    assert_eq!(encoded, REAL_ADDR);
    let full = base58_decode(&encoded).unwrap();
    assert_eq!(full.len(), 25);
    assert_eq!(&full[..21], &payload[..]);
}

// ---------------------------------------------------------------------------
// balances
// ---------------------------------------------------------------------------

#[test]
fn compute_balances_totals_and_sorts() {
    let (_, pk) = spend::synthetic_key("bal/a");
    let addr_a = crate::wallet::pubkey_to_address(&pk);
    let (_, pk2) = spend::synthetic_key("bal/b");
    let addr_b = crate::wallet::pubkey_to_address(&pk2);
    let mk = |addr: &str, v: i64| UtxoRec {
        txid: [7u8; 32],
        n: 0,
        value_sats: v,
        script: Vec::new(),
        address: addr.to_string(),
        height: 100,
        tx_time: 1_700_000_000,
        prev_kind: "regular".into(),
    };
    let utxos = vec![mk(&addr_a, 500), mk(&addr_b, 9_000), mk(&addr_a, 1_500)];
    let bal = compute_balances(&utxos, &|a| {
        if a == addr_a {
            Some("alpha".into())
        } else {
            None
        }
    });
    assert_eq!(bal.total_sats, 11_000);
    assert_eq!(bal.per_address.len(), 2);
    // sorted desc by sats
    assert_eq!(bal.per_address[0].address, addr_b);
    assert_eq!(bal.per_address[0].sats, 9_000);
    assert_eq!(bal.per_address[0].utxos, 1);
    assert_eq!(bal.per_address[1].sats, 2_000);
    assert_eq!(bal.per_address[1].utxos, 2);
    assert_eq!(bal.per_address[1].label.as_deref(), Some("alpha"));
}

// ---------------------------------------------------------------------------
// send construction — byte equality with the spend.rs CLI path
// ---------------------------------------------------------------------------

fn funded_utxo(label: &str, value: i64, kind: &str, height: u32) -> (UtxoRec, [u8; 32], Vec<u8>) {
    let (secret, pubkey) = spend::synthetic_key(label);
    let script = spend::script_pubkey_p2pkh(&pubkey);
    let addr = crate::wallet::pubkey_to_address(&pubkey);
    let tx = Tx {
        version: 1,
        time: 1_700_000_000,
        vin: vec![Vin {
            prev_hash: [0u8; 32],
            prev_n: u32::MAX,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        vout: vec![Vout {
            value,
            script_pubkey: script.clone(),
        }],
        locktime: 0,
    };
    let utxo = UtxoRec {
        txid: tx.hash(),
        n: 0,
        value_sats: value,
        script,
        address: addr,
        height,
        tx_time: 1_700_000_000,
        prev_kind: kind.into(),
    };
    (utxo, secret, pubkey)
}

#[test]
fn gui_send_bytes_equal_direct_spend_rs_construction() {
    let (utxo, secret, pubkey) = funded_utxo("send/eq", 5_000_000, "regular", 2_203_000);
    let (d_secret, d_pub) = spend::synthetic_key("send/eq-dest");
    let _ = d_secret;
    let dest_payload = validate_address(&crate::wallet::pubkey_to_address(&d_pub)).unwrap();
    let derived = DerivedWallet {
        utxos: vec![utxo.clone()],
        balances: compute_balances(&[utxo.clone()], &|_| None),
        snapshot_height: 2_201_090,
        coverage_height: 2_204_064,
        reconcile: ReconcileReport::default(),
    };
    let mut keys = std::collections::HashMap::new();
    keys.insert(utxo.address.clone(), (pubkey.clone(), secret));
    let req = SendRequest {
        from: utxo.address.clone(),
        dest_payload,
        amount_sats: 2_000_000,
        fee_sats: 0, // let the legacy fee loop raise it
        change_payload: crate::address::pubkey_payload(&pubkey),
    };
    let now = 1_700_000_100u32;
    let build =
        build_signed_send(&req, &derived, &keys, None, now, 2_204_064).expect("build must succeed");

    // the direct spend.rs construction (what the CLI dry-run path does)
    let input = spend::SpendInput {
        txid: utxo.txid,
        n: 0,
        value: 5_000_000,
        script_pubkey: utxo.script.clone(),
        height: 2_203_000,
        tx_time: 1_700_000_000,
        is_coinbase: false,
        is_coinstake: false,
        secret,
        pubkey: pubkey.clone(),
    };
    let selected = spend::select_utxos_covering(&[input.clone()], 2_000_000).unwrap();
    let plan = spend::build_spend(
        &selected,
        p2pkh_script_for(&dest_payload),
        2_000_000,
        Some(p2pkh_script_for(&crate::address::pubkey_payload(&pubkey))),
        0,
        now,
        2_204_065,
    )
    .unwrap();
    assert_eq!(hex_decode(&build.tx_hex).unwrap(), plan.tx.serialize());
    assert_eq!(build.fee_sats, plan.fee);
    assert!(build.fee_sats > 0, "fee loop must raise the 0 user fee");
    assert_eq!(build.change_sats, 5_000_000 - 2_000_000 - build.fee_sats);
    // the tx verifies through the same validators the CLI dry run uses
    spend::verify_input(&plan.tx, 0, &utxo.script).unwrap();
    spend::connect_inputs_check(&plan.tx, &selected, 2_204_065).unwrap();
}

#[test]
fn gui_send_rejects_insufficient_and_immature_coinbase() {
    let (utxo, secret, pubkey) = funded_utxo("send/err", 500_000, "regular", 2_203_000);
    let derived = DerivedWallet {
        utxos: vec![utxo.clone()],
        balances: compute_balances(&[utxo.clone()], &|_| None),
        snapshot_height: 0,
        coverage_height: 0,
        reconcile: ReconcileReport::default(),
    };
    let mut keys = std::collections::HashMap::new();
    keys.insert(utxo.address.clone(), (pubkey.clone(), secret));
    let (d_secret, d_pub) = spend::synthetic_key("send/err-dest");
    let _ = d_secret;
    let dest = validate_address(&crate::wallet::pubkey_to_address(&d_pub)).unwrap();
    let req = SendRequest {
        from: utxo.address.clone(),
        dest_payload: dest,
        amount_sats: 900_000, // > 500_000 pool
        fee_sats: 0,
        change_payload: [0u8; 21],
    };
    let err = build_signed_send(&req, &derived, &keys, None, 1_700_000_000, 2_204_064)
        .err()
        .expect("insufficient funds must fail");
    assert!(err.contains("Insufficient funds!"), "got: {}", err);

    // immature coinbase: spendable_at filters it out of the auto pool
    let (cb, _, _) = funded_utxo("send/cb", 10_000_000, "coinbase", 2_204_062); // depth 2
    let derived2 = DerivedWallet {
        utxos: vec![cb.clone()],
        balances: compute_balances(&[cb.clone()], &|_| None),
        snapshot_height: 0,
        coverage_height: 0,
        reconcile: ReconcileReport::default(),
    };
    let mut keys2 = std::collections::HashMap::new();
    let (_cb_secret, cb_pub) = spend::synthetic_key("send/cb-key");
    keys2.insert(cb.address.clone(), (cb_pub.to_vec(), _cb_secret));
    let req2 = SendRequest {
        from: cb.address.clone(),
        dest_payload: dest,
        amount_sats: 1_000,
        fee_sats: 0,
        change_payload: [0u8; 21],
    };
    let err2 = build_signed_send(&req2, &derived2, &keys2, None, 1_700_000_000, 2_204_064)
        .err()
        .expect("immature coinbase must be filtered");
    assert!(err2.contains("No spendable inputs"), "got: {}", err2);
    assert!(!cb.spendable_at(2_204_065));
    assert!(cb.spendable_at(2_204_069)); // depth 7
}

#[test]
fn coin_control_manual_pool_is_respected() {
    let (u1, s1, p1) = funded_utxo("cc/one", 3_000_000, "regular", 2_203_000);
    let (u2, s2, p2) = funded_utxo("cc/two", 1_000_000, "regular", 2_203_100);
    let (d_secret, d_pub) = spend::synthetic_key("cc/dest");
    let _ = d_secret;
    let dest = validate_address(&crate::wallet::pubkey_to_address(&d_pub)).unwrap();
    let derived = DerivedWallet {
        utxos: vec![u1.clone(), u2.clone()],
        balances: compute_balances(&[u1.clone(), u2.clone()], &|_| None),
        snapshot_height: 0,
        coverage_height: 0,
        reconcile: ReconcileReport::default(),
    };
    let mut keys = std::collections::HashMap::new();
    keys.insert(u1.address.clone(), (p1.clone(), s1));
    keys.insert(u2.address.clone(), (p2.clone(), s2));
    let req = SendRequest {
        from: u1.address.clone(),
        dest_payload: dest,
        // 3.5M > u1 alone (3M) → BOTH manual inputs are required
        amount_sats: 3_500_000,
        fee_sats: 500,
        change_payload: crate::address::pubkey_payload(&p1),
    };
    // manual: ONLY u2 (1_000_000) → cannot cover 3_500_500
    let err = build_signed_send(
        &req,
        &derived,
        &keys,
        Some(&[u2.clone()]),
        1_700_000_000,
        2_204_064,
    )
    .err()
    .expect("manual pool below target must fail");
    assert!(err.contains("Insufficient funds!"), "got: {}", err);
    // manual: both → OK and BOTH used
    let build = build_signed_send(
        &req,
        &derived,
        &keys,
        Some(&[u1.clone(), u2.clone()]),
        1_700_000_000,
        2_204_064,
    )
    .unwrap();
    assert_eq!(build.inputs.len(), 2);
    assert_eq!(build.amount_sats, 3_500_000);
}

// ---------------------------------------------------------------------------
// tail reconciliation (synthetic archive frames)
// ---------------------------------------------------------------------------

fn push_cs(w: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        w.push(n as u8);
    } else {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    }
}

fn serialize_frame(b: &Block) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&b.version.to_le_bytes());
    body.extend_from_slice(&b.prev_hash);
    body.extend_from_slice(&b.merkle_root);
    body.extend_from_slice(&b.time.to_le_bytes());
    body.extend_from_slice(&b.bits.to_le_bytes());
    body.extend_from_slice(&b.nonce.to_le_bytes());
    push_cs(&mut body, b.vtx.len() as u64);
    for tx in &b.vtx {
        body.extend_from_slice(&tx.serialize());
    }
    push_cs(&mut body, b.block_sig.len() as u64);
    body.extend_from_slice(&b.block_sig);
    let mut frame = Vec::new();
    frame.extend_from_slice(&0x0522_3570u32.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn wallet_owned_script(label: &str) -> (Vec<u8>, [u8; 21]) {
    let (_, pubkey) = spend::synthetic_key(label);
    let script = spend::script_pubkey_p2pkh(&pubkey);
    let payload = crate::address::pubkey_payload(&pubkey);
    (script, payload)
}

#[test]
fn reconcile_tail_applies_spends_and_wallet_additions() {
    let dir = std::env::temp_dir().join(format!("tri7gui_reconcile_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("blk0001.dat");

    let (script_a, payload_a) = wallet_owned_script("rec/a");
    let (script_b, payload_b) = wallet_owned_script("rec/b");
    let (script_c, payload_c) = wallet_owned_script("rec/c");
    let (script_x, _payload_x) = wallet_owned_script("rec/x"); // NOT in the wallet set

    let snapshot_height = 2_201_090u32;
    let base_hash = [0x11u8; 32]; // fake "covered tip" — chain grows from here

    // snapshot: A (will be spent in the tail) + B (stays)
    let mk_rec = |txid: [u8; 32], script: Vec<u8>, payload: &[u8; 21]| UtxoRec {
        txid,
        n: 0,
        value_sats: 1_000_000,
        script,
        address: payload_to_address(payload),
        height: snapshot_height,
        tx_time: 1_700_000_000,
        prev_kind: "regular".into(),
    };
    let mut tx_a = Tx {
        version: 1,
        time: 1_700_000_000,
        vin: vec![Vin {
            prev_hash: [0u8; 32],
            prev_n: u32::MAX,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        vout: vec![Vout {
            value: 1_000_000,
            script_pubkey: script_a.clone(),
        }],
        locktime: 0,
    };
    let _ = &mut tx_a;
    let txid_a = tx_a.hash();
    let mut tx_b = tx_a.clone();
    tx_b.vout[0].script_pubkey = script_b.clone();
    let txid_b = tx_b.hash();
    let snapshot = vec![
        mk_rec(txid_a, script_a.clone(), &payload_a),
        mk_rec(txid_b, script_b.clone(), &payload_b),
    ];

    // block h+1: spends A, adds C (wallet) + X (not wallet)
    let spend_a = Tx {
        version: 1,
        time: 1_700_000_100,
        vin: vec![Vin {
            prev_hash: txid_a,
            prev_n: 0,
            script_sig: vec![0xab],
            sequence: u32::MAX,
        }],
        vout: vec![Vout {
            value: 999_000,
            script_pubkey: script_x.clone(),
        }],
        locktime: 0,
    };
    let mint_c = Tx {
        version: 1,
        time: 1_700_000_100,
        vin: vec![Vin {
            prev_hash: spend_a.hash(),
            prev_n: 0,
            script_sig: vec![0xab],
            sequence: u32::MAX,
        }],
        vout: vec![Vout {
            value: 50_000,
            script_pubkey: script_c.clone(),
        }],
        locktime: 0,
    };
    let mut b1 = Block {
        version: 2,
        prev_hash: base_hash,
        merkle_root: [0u8; 32],
        time: 1_700_000_100,
        bits: 0x207fffff,
        nonce: 0,
        vtx: vec![spend_a, mint_c.clone()],
        block_sig: vec![],
    };
    b1.merkle_root = b1.merkle_root();
    let mut b2 = Block {
        version: 2,
        prev_hash: b1.hash(),
        merkle_root: [0u8; 32],
        time: 1_700_000_200,
        bits: 0x207fffff,
        nonce: 0,
        // spends C again — the later-spent addition must be DROPPED
        vtx: vec![Tx {
            version: 1,
            time: 1_700_000_200,
            vin: vec![Vin {
                prev_hash: mint_c.hash(),
                prev_n: 0,
                script_sig: vec![0xab],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value: 49_000,
                script_pubkey: script_x.clone(),
            }],
            locktime: 0,
        }],
        block_sig: vec![],
    };
    b2.merkle_root = b2.merkle_root();

    let f1 = serialize_frame(&b1);
    let f2 = serialize_frame(&b2);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&f1);
    bytes.extend_from_slice(&f2);
    std::fs::write(&archive, &bytes).unwrap();
    let offsets = vec![0u64, f1.len() as u64];

    let payloads: HashSet<[u8; 21]> = [payload_a, payload_b, payload_c].into_iter().collect();
    let (out, report) = reconcile_tail_with_offsets(
        snapshot.clone(),
        snapshot_height,
        Some(&crate::legacy::hash_to_display(&base_hash)),
        &payloads,
        &archive,
        &offsets,
        snapshot_height as u64 + 2,
    )
    .expect("reconcile must succeed");

    assert_eq!(report.blocks_applied, 2);
    // every vin above the snapshot counts (wallet-owned or not): A, spend_a,
    // C — the final-set filter only drops wallet-owned ones
    assert_eq!(report.spends_applied, 3);
    // final set: B only — A spent, C added-then-spent (dropped)
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].txid, txid_b);
    // link integrity: prev of h+1 must equal the snapshot tip
    let bad = reconcile_tail_with_offsets(
        snapshot.clone(),
        snapshot_height,
        Some(&crate::legacy::hash_to_display(&[0x99u8; 32])),
        &payloads,
        &archive,
        &offsets,
        snapshot_height as u64 + 2,
    );
    assert!(bad.is_err(), "broken chain link must be refused");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// tipstate + stake status
// ---------------------------------------------------------------------------

#[test]
fn load_tipstate_parses_and_reports_tip_fields() {
    let dir = std::env::temp_dir().join(format!("tri7gui_tip_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tipstate.json");
    let hash = crate::legacy::hash_to_display(&[0xabu8; 32]);
    std::fs::write(
        &path,
        format!(
            r#"{{"height": 2204064, "blockhash": "{}", "updated_utc": "2026-11-02T00:00:00Z",
                "last_advance_utc": "2026-09-24T00:00:00Z", "window_base": 2199465,
                "daemon_tip": 2204064,
                "entries": [{{"hash":"{}","time": 1790982529, "bits": 536936447, "pos": true,
                             "modifier": 0, "generated": false}}]}}"#,
            hash, hash
        ),
    )
    .unwrap();
    let tip = load_tipstate(&path).unwrap();
    assert_eq!(tip.height, 2_204_064);
    assert_eq!(tip.daemon_tip, Some(2_204_064));
    assert_eq!(tip.tip_bits, 0x2000ffff);
    assert_eq!(tip.tip_modifier, 0);
    assert!(tip.tip_pos);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stake_status_parses_dryrun_and_counts_mature_weight() {
    let dir = std::env::temp_dir().join(format!("tri7gui_stake_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dry = dir.join("stake_dryrun.json");
    std::fs::write(
        &dry,
        r#"{
          "block": {"hash": "fd5e471d"},
          "coinstake": {"reward_sats": 483279, "n_time": 1793642570, "split": false},
          "kernel_search": {"attempts": 381, "kernel_hits": 1, "coins_considered": 7,
                            "coins_meeting_min_age": 7, "search_time": 1793642590}
        }"#,
    )
    .unwrap();
    let tip = TipInfo {
        height: 2_204_064,
        blockhash: String::new(),
        updated_utc: String::new(),
        last_advance_utc: String::new(),
        window_base: 0,
        daemon_tip: None,
        tip_time: 1_700_000_000, // fixture time
        tip_bits: 0x2000ffff,
        tip_modifier: 7,
        tip_generated: true,
        tip_pos: true,
    };
    let (script, payload) = wallet_owned_script("stake/w");
    let utxo = UtxoRec {
        txid: [1u8; 32],
        n: 0,
        value_sats: 12_010_466,
        script,
        address: payload_to_address(&payload),
        height: 2_203_769,
        tx_time: 1_700_000_000 - 4000, // mature vs STAKE_MIN_AGE=3600 at tip_time
        prev_kind: "coinstake".into(),
    };
    let st = load_stake_status(&dry, &tip, &[utxo]);
    let d = st.dryrun.expect("dryrun must parse");
    assert_eq!(d.attempts, 381);
    assert_eq!(d.kernel_hits, 1);
    assert_eq!(d.reward_sats, 483_279);
    assert_eq!(st.modifier, 7);
    assert!(st.modifier_generated);
    assert_eq!(st.mature_weight_sats, 12_010_466);
    assert_eq!(st.stakeable_coins, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// QR / URI / PNG
// ---------------------------------------------------------------------------

#[test]
fn payment_uri_matches_legacy_scheme_and_params() {
    let uri = build_payment_uri(REAL_ADDR, Some("1.5"), "self", "").unwrap();
    assert_eq!(
        uri,
        format!("Triangles:{}?amount=1.500000&label=self", REAL_ADDR)
    );
    let uri2 = build_payment_uri(REAL_ADDR, None, "", "a b&c").unwrap();
    assert_eq!(uri2, format!("Triangles:{}?message=a%20b%26c", REAL_ADDR));
    assert!(build_payment_uri("not base58!", None, "", "").is_err());
    assert!(build_payment_uri(REAL_ADDR, Some("1.2345678"), "", "").is_err());
}

#[test]
fn qr_matrix_renders_and_png_bytes_are_wellformed() {
    // PNG CRC32 sanity (well-known vector)
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(&[]), 0);
    let uri = build_payment_uri(REAL_ADDR, Some("1.5"), "", "").unwrap();
    let qr = qr_matrix(&uri).expect("qr must encode");
    assert!(qr.size >= 21, "version-1 QR minimum 21x21");
    assert_eq!(qr.modules.len(), qr.size * qr.size);
    assert!(qr.modules.iter().any(|m| *m), "QR must have dark modules");
    let (w, h, px) = qr_pixels(&qr, 4);
    assert_eq!(w, qr.size + 8);
    let png = png_encode_gray(w, h, &px).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]));
    assert!(&png[png.len() - 8..png.len() - 4] == b"IEND");
    // IHDR dims
    let w_be = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h_be = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w_be, h_be), (w as u32, h as u32));
    assert!(png_encode_gray(2, 2, &[0u8; 3]).is_err()); // size mismatch refused
}

// ---------------------------------------------------------------------------
// base64 + message signing (legacy "Triangles Signed Message:\n" magic)
// ---------------------------------------------------------------------------

#[test]
fn base64_known_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    for s in ["", "f", "fo", "foobar", "Triangles Signed Message"] {
        assert_eq!(
            base64_decode(&base64_encode(s.as_bytes())).unwrap(),
            s.as_bytes()
        );
    }
}

#[test]
fn message_sign_roundtrip_and_tamper_detection() {
    let (secret, pubkey) = spend::synthetic_key("msg/signer");
    let addr = crate::wallet::pubkey_to_address(&pubkey);
    let msg = "tri7 test message";
    let sig = sign_message(&secret, msg).unwrap();
    let raw = base64_decode(&sig).unwrap();
    assert_eq!(raw.len(), 65);
    assert!((31..=34).contains(&raw[0]), "compressed SignCompact header");
    assert!(verify_message(&addr, msg, &sig).unwrap());
    assert!(!verify_message(&addr, "tampered", &sig).unwrap());
    let (secret2, pubkey2) = spend::synthetic_key("msg/other");
    let addr2 = crate::wallet::pubkey_to_address(&pubkey2);
    let _ = secret2;
    assert!(!verify_message(&addr2, msg, &sig).unwrap());
    assert!(verify_message(&addr, msg, "not-base64!!!").is_err());
    // the hash construction is CDataStream(magic << string) = magic + cs-len + msg
    let mut expect = Vec::new();
    expect.extend_from_slice(gs::MESSAGE_MAGIC.as_bytes());
    push_compact_size_local(&mut expect, msg.len() as u64);
    expect.extend_from_slice(msg.as_bytes());
    assert_eq!(message_hash(msg), crate::legacy::sha256d(&expect));
}

fn push_compact_size_local(w: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        w.push(n as u8);
    } else {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// rpc console + options + labels/identity
// ---------------------------------------------------------------------------

#[test]
fn console_line_parsing_matches_legacy_syntax() {
    let (m, p) = parse_console_line("getblockcount");
    assert_eq!(m, "getblockcount");
    assert!(p.is_empty());
    let (m, p) = parse_console_line("getblock 2204064");
    assert_eq!(m, "getblock");
    assert_eq!(p, vec![serde_json::json!(2204064)]);
    let (m, p) = parse_console_line(&format!("validateaddress \"{}\"", REAL_ADDR));
    assert_eq!(m, "validateaddress");
    assert_eq!(p, vec![serde_json::json!(REAL_ADDR)]);
    let (m, p) = parse_console_line("getblockhash [1, 2]");
    assert_eq!(m, "getblockhash");
    assert_eq!(p, vec![serde_json::json!(1), serde_json::json!(2)]);
}

#[test]
fn rpc_allowlist_refuses_writes_before_any_io() {
    assert!(!rpc_client::is_allowed("sendtoaddress"));
    assert!(!rpc_client::is_allowed("walletpassphrase"));
    assert!(!rpc_client::is_allowed("move"));
    assert!(rpc_client::is_allowed("getconnectioncount"));
    // loopback listener serves the exact tri7d envelope
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = r#"{"result":{"connections":3},"error":null}"#;
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            use std::io::{Read as _, Write as _};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(http.as_bytes());
        }
    });
    let v =
        rpc_client::rpc_call(&format!("127.0.0.1:{}", port), "getconnectioncount", &[]).unwrap();
    assert_eq!(v["connections"], 3);
}

#[test]
fn default_options_carry_the_legacy_default_fee() {
    let o = OptionsState::default();
    assert_eq!(
        parse_tri(&o.pay_fee).unwrap(),
        spend::DEFAULT_TRANSACTION_FEE
    );
    assert!(o.coin_control);
    assert!(o.display_addresses);
}

#[test]
fn legacy_identity_is_carried_over_exactly() {
    // sidebar order — Overview FIRST (mainwindow.ui)
    assert_eq!(Screen::NAV_ORDER[0], Screen::Overview);
    assert_eq!(Screen::NAV_ORDER[1], Screen::Send);
    assert_eq!(Screen::NAV_ORDER[2], Screen::Receive);
    assert_eq!(Screen::NAV_ORDER[3], Screen::AddressBook);
    assert_eq!(Screen::NAV_ORDER[4], Screen::Transactions);
    assert_eq!(Screen::NAV_ORDER[5], Screen::Staking);
    assert_eq!(Screen::NAV_ORDER[6], Screen::Messages);
    // exact legacy button labels
    assert_eq!(Screen::Overview.label(), "Overview");
    assert_eq!(Screen::Send.label(), "Send Coins");
    assert_eq!(Screen::Receive.label(), "Receive Coins");
    assert_eq!(Screen::AddressBook.label(), "Address Book");
    assert_eq!(Screen::Transactions.label(), "Transactions");
    assert_eq!(Screen::Messages.label(), "Messages");
    assert_eq!(Screen::Staking.label(), "Staking");
    // branding + palette extracted from the .ui files
    assert_eq!(gs::APP_TITLE, "Cryptographic Triangles");
    assert_eq!(gs::TICKER, "TRI");
    assert_eq!(gs::MESSAGE_MAGIC, "Triangles Signed Message:\n");
    assert_eq!(gs::URI_SCHEME, "Triangles:");
    assert_eq!(theme::ACCENT, (0xe3, 0x21, 0x05)); // mainwindow.ui accent
    assert_eq!(theme::WINDOW_BG, (0x00, 0x00, 0x00)); // MainWindow #000
    assert_eq!(theme::PANEL_BG, (0x1c, 0x1c, 0x1c)); // input bg
    assert_eq!(theme::DEEP_RED, (0x3d, 0x0e, 0x04)); // hover/selection
    assert_eq!(theme::MENU_BG, (0xAF, 0x27, 0x2E)); // QMenu
    assert_eq!(theme::rgb_str(theme::ACCENT), "227,33,5");
}

#[test]
fn derivation_falls_back_to_snapshot_without_index() {
    let dir = std::env::temp_dir().join(format!("tri7gui_derive_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let (script, payload) = wallet_owned_script("derive/w");
    let dump = format!(
        r#"{{"height": 2201090, "covered_tip_hash": "{}", "count": 1, "total_value_sats": 5000000,
           "utxos": [{{"txid": "{}", "n": 0, "value_sats": 5000000, "script_hex": "{}",
                      "address": "{}", "height": 2201090, "tx_time": 1700000000, "prev_kind": "regular"}}]}}"#,
        crate::legacy::hash_to_display(&[0x11u8; 32]),
        crate::legacy::hash_to_display(&[0x22u8; 32]),
        script
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
        payload_to_address(&payload),
    );
    std::fs::write(dir.join("state").join("wallet_utxos.json"), &dump).unwrap();
    let hash = crate::legacy::hash_to_display(&[0xabu8; 32]);
    std::fs::write(
        dir.join("state").join("tipstate.json"),
        format!(
            r#"{{"height": 2204064, "blockhash": "{}", "window_base": 2199465, "entries": []}}"#,
            hash
        ),
    )
    .unwrap();
    let cfg = UiConfig {
        wallet_path: "/nonexistent/wallet.dat".into(),
        state_dir: dir.join("state").display().to_string(),
        stake_dryrun_path: "/nonexistent".into(),
        rpc_addr: None,
        profile_dir: dir.join("profile").display().to_string(),
        balances_path: "/nonexistent".into(),
        explorer_endpoints: vec![],
    };
    let tip = load_tipstate(&cfg.tipstate_path()).unwrap();
    let dump = load_utxo_dump(&cfg.utxo_dump_path()).unwrap();
    let payloads: HashSet<[u8; 21]> = [payload].into_iter().collect();
    let derived = derive_wallet(dump, payloads, &cfg, &tip, &|_| None).unwrap();
    // no index caches / no archive → honest fallback, snapshot shown as-is
    assert!(!derived.reconcile.enabled);
    assert!(
        derived
            .reconcile
            .note
            .contains("tail reconciliation failed")
            || derived.reconcile.note.contains("absent"),
        "note: {}",
        derived.reconcile.note
    );
    assert_eq!(derived.coverage_height, 2_201_090);
    assert_eq!(derived.balances.total_sats, 5_000_000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keystore_side_files_persist_new_addresses() {
    let dir = std::env::temp_dir().join(format!("tri7gui_keystore_{}", std::process::id()));
    let cfg = UiConfig {
        wallet_path: "/nonexistent".into(),
        state_dir: "/nonexistent".into(),
        stake_dryrun_path: "/nonexistent".into(),
        rpc_addr: None,
        profile_dir: dir.display().to_string(),
        balances_path: "/nonexistent".into(),
        explorer_endpoints: vec![],
    };
    let mut st = crate::gui::app_state::AppState::new(cfg);
    let addr = st
        .new_address_action("cold-storage".into())
        .expect("new address");
    assert!(addr.starts_with('T'));
    assert!(dir.join("keystore.json").exists());
    // reload — the key must come back (so funds there are spendable later)
    let cfg2 = UiConfig {
        profile_dir: dir.display().to_string(),
        ..UiConfig::default()
    };
    let st2 = crate::gui::app_state::AppState::new(cfg2);
    assert!(st2
        .keystore
        .iter()
        .any(|e| e.address == addr && e.label == "cold-storage"));
    assert!(st2
        .keystore
        .iter()
        .any(|e| e.address == addr && e.secret.is_some()));
    // label persists into the addressbook side file
    st.set_label_action(&addr, "renamed".into()).unwrap();
    let st3 = crate::gui::app_state::AppState::new(UiConfig {
        profile_dir: dir.display().to_string(),
        ..UiConfig::default()
    });
    assert!(st3
        .addressbook
        .iter()
        .any(|e| e.address == addr && e.label == "renamed"));
    // wallet-owned addresses are protected from deletion; side ones are not
    let mut st4 = crate::gui::app_state::AppState::new(UiConfig {
        profile_dir: dir.display().to_string(),
        ..UiConfig::default()
    });
    assert!(st4.delete_ab_entry_action(&addr).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_to_dest_classifies_the_gui_p2pkh_scripts() {
    let (_, pk) = spend::synthetic_key("dest/cls");
    let script = spend::script_pubkey_p2pkh(&pk);
    match script_to_dest(&script) {
        Some(crate::address::ScriptDest::PubkeyHash(h)) => {
            assert_eq!(h.to_vec(), hash160(&pk).to_vec());
        }
        other => panic!("expected PubkeyHash, got {:?}", other.is_some()),
    }
}
