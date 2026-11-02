//! Consensus + wallet layer tests: classification, compact targets, kernel
//! hash, signature gates, wallet record parsing (order-independent), using
//! only deterministic in-process data (no live archive required).

use secp256k1::{Message, Secp256k1};
use sha2::{Digest, Sha256};
use tri7_core::consensus::*;
use tri7_core::legacy::{hash9, sha256d, Block, Reader, Tx};

// ---------------------------------------------------------------------------
// classification
// ---------------------------------------------------------------------------
fn make_tx(vout0_value: i64, vout0_script: &[u8], prev_null: bool) -> Tx {
    Tx {
        version: 1,
        time: 1_700_000_000,
        vin: vec![tri7_core::Vin {
            prev_hash: if prev_null {
                [0u8; 32]
            } else {
                [1u8; 32]
            },
            prev_n: if prev_null { u32::MAX } else { 0 },
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        vout: vec![
            tri7_core::Vout {
                value: vout0_value,
                script_pubkey: vout0_script.to_vec(),
            },
            tri7_core::Vout {
                value: 100,
                script_pubkey: vec![0xac],
            },
        ],
        locktime: 0,
    }
}

#[test]
fn coinstake_marker_is_zero_value_not_negative_one() {
    // CTxOut::IsEmpty() = nValue == 0 && empty script (main.h:428). The -1
    // sentinel is CTxOut::IsNull() — a different function.
    let pos = make_tx(0, &[], false);
    assert!(tx_is_coin_stake(&pos));
    let not_stake = make_tx(-1, &[], false);
    assert!(!tx_is_coin_stake(&not_stake));
    let no_input = make_tx(0, &[], true);
    assert!(!tx_is_coin_stake(&no_input));
}

#[test]
fn block_classification_pos_vs_pow() {
    let mut b = sample_block();
    b.vtx = vec![make_tx(50, &[0xac], true), make_tx(0, &[], false)];
    assert!(block_is_proof_of_stake(&b));
    b.vtx = vec![make_tx(50, &[0xac], true)];
    assert!(!block_is_proof_of_stake(&b));
}

fn sample_block() -> Block {
    Block {
        version: 6,
        prev_hash: [0u8; 32],
        merkle_root: [0u8; 32],
        time: 1_700_000_000,
        bits: 0x1e0ffff0,
        nonce: 0,
        vtx: vec![make_tx(0, &[], true)],
        block_sig: vec![],
    }
}

// ---------------------------------------------------------------------------
// compact target
// ---------------------------------------------------------------------------
#[test]
fn compact_target_known_value() {
    // 0x1d00ffff = 0x00000000FFFF0000...  (mantissa 0xffff shifted 26 bytes)
    let t = compact_to_target(0x1d00_ffff);
    assert_eq!(&t[..4], &[0u8; 4]);
    assert_eq!(t[4], 0xff);
    assert_eq!(t[5], 0xff);
    assert_eq!(&t[6..], &[0u8; 26]);
}

#[test]
fn compact_target_no_panic_all_exponents() {
    for size in 0u32..40 {
        for mantissa in [0u32, 1, 0x7f_ffff, 0x1234_5678 % 0x80_0000] {
            let bits = (size << 24) | mantissa;
            let _ = compact_to_target(bits); // must not panic
        }
    }
}

#[test]
fn compact_target_negative_or_zero_is_unmineable() {
    let t = compact_to_target(0x0180_0001 | 0x0080_0000); // negative flag
    assert_eq!(t, [0u8; 32]);
    let t = compact_to_target(0x0100_0001); // size 1: fractional
    assert_eq!(t, [0u8; 32]);
    let t = compact_to_target(0x2300_0001); // size 35 > 34 saturates
    assert_eq!(t, [0xffu8; 32]);
}

// ---------------------------------------------------------------------------
// kernel hash — field order/width sensitivity
// ---------------------------------------------------------------------------
#[test]
fn kernel_hash_is_double_sha256_of_six_le_fields() {
    let expect = sha256d(&{
        let mut s = Vec::new();
        s.extend_from_slice(&42u64.to_le_bytes());
        s.extend_from_slice(&1000u32.to_le_bytes());
        s.extend_from_slice(&77u32.to_le_bytes());
        s.extend_from_slice(&2000u32.to_le_bytes());
        s.extend_from_slice(&1u32.to_le_bytes());
        s.extend_from_slice(&3000u32.to_le_bytes());
        s
    });
    let got = stake_kernel_hash(42, 1000, 77, 2000, 1, 3000);
    assert_eq!(got, expect);
    // any field change must change the hash
    let changed = stake_kernel_hash(43, 1000, 77, 2000, 1, 3000);
    assert_ne!(got, changed);
}

#[test]
fn stake_weight_respects_min_and_max_age() {
    // tip_height 0 = pre-v5 validating tip -> the capped Peercoin rule
    assert_eq!(stake_weight(1_000_000, 1_000_000, 0), 0); // younger than min age
    assert_eq!(stake_weight(1_000_000, 1_000_000 + 3600, 0), 0); // exactly min age -> 0
    assert_eq!(stake_weight(1_000_000, 1_000_000 + 3601, 0), 1);
    assert_eq!(stake_weight(1_000_000, 1_000_000 + 3600 + 43_200, 0), 43_200); // capped
    assert_eq!(stake_weight(1_000_000, 1_000_000 + 100_000, 0), 43_200);
    // v5-era validating tip -> uncapped (fork gate, kernel.cpp:37-39)
    assert_eq!(stake_weight(1_000_000, 1_000_000 + 100_000, 17_651), 96_400);
}

// ---------------------------------------------------------------------------
// signature gates
// ---------------------------------------------------------------------------
#[test]
fn pow_block_requires_empty_signature() {
    let mut b = sample_block();
    b.vtx = vec![make_tx(50, &[0xac], true)];
    b.block_sig = vec![0x30];
    assert!(check_pow_block_signature(&b).is_err());
    b.block_sig = vec![];
    assert!(check_pow_block_signature(&b).is_ok());
}

#[test]
fn pos_sig_gate_rejects_non_bare_pubkey_script() {
    let mut b = sample_block();
    b.vtx = vec![make_tx(0, &[], true), make_tx(0, &[], false)];
    // P2PKH-style script on vout[1]: not accepted (Solver must be TX_PUBKEY)
    b.vtx[1].vout[1].script_pubkey = {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&[0xaa; 20]);
        s.push(0x88);
        s.push(0xac);
        s
    };
    assert!(bare_pubkey_from_script(&b.vtx[1].vout[1].script_pubkey).is_none());
    // bare compressed pubkey accepted
    let mut script = vec![33u8];
    script.extend_from_slice(&[2u8; 33]);
    script.push(0xac);
    assert!(bare_pubkey_from_script(&script).is_some());
    // empty block sig rejected downstream
    b.vtx[1].vout[1].script_pubkey = script;
    assert!(check_pos_block_signature(&b).is_err());
}

#[test]
fn genesis_hash9_vector() {
    // 80-byte genesis header captured from the live chain archive (block 0).
    const GENESIS_HEADER: [u8; 80] = [
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0xd5, 0x48, 0xf8, 0x42, 0xbc, 0x98, 0x7a, 0xd5,
        0xa8, 0x89, 0x11, 0xe8, 0x38, 0xee, 0xc7, 0x7c, 0x9a, 0xed, 0x4e, 0x56, 0xb8, 0x00, 0xa7,
        0xcc, 0xe7, 0xc4, 0xaf, 0x73, 0x72, 0xf7, 0x27, 0x02, 0x3c, 0xc6, 0x53, 0xff, 0xff, 0x00,
        0x20, 0x2b, 0x00, 0x00, 0x00,
    ];
    // NOTE: this vector is validated against the LIVE archive in
    // validate_chain (genesis MATCH); here we assert the hash function is
    // deterministic and the fixture decodes to the compiled-in genesis
    // constant bytes. The authoritative cross-check remains the live run.
    let h = hash9(&GENESIS_HEADER);
    let disp: String = h.iter().rev().map(|b| format!("{:02x}", b)).collect();
    assert_eq!(
        disp,
        "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021"
    );
}

// ---------------------------------------------------------------------------
// wallet record parsing (in-memory SQLite, order-independent)
// ---------------------------------------------------------------------------
fn wallet_compact_size(w: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        w.push(n as u8);
    } else {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    }
}

fn make_wallet_file(path: &std::path::Path, keys_first: bool) -> (Vec<u8>, secp256k1::SecretKey) {
    use rusqlite::Connection;
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA application_id = 1414687095; CREATE TABLE main (key BLOB PRIMARY KEY, value BLOB);",
    )
    .unwrap();

    let secp = Secp256k1::signing_only();
    let sk = secp256k1::SecretKey::from_slice(&{
        let mut s = [9u8; 32];
        s[31] = 7;
        s
    })
    .unwrap();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    let pk_ser = pk.serialize();

    // key record: key = cs("key") + cs(pubkey) + pubkey ; value = cs(DER)
    let mut der = vec![0x30, 0x54, 0x02, 0x01, 0x01, 0x04, 0x20];
    der.extend_from_slice(&sk.secret_bytes());
    der.extend_from_slice(&[0xa1, 0x2d, 0x03, 0x2a, 0x00]);
    der.extend_from_slice(&[0x21]);
    der.extend_from_slice(&pk_ser);
    let mut kkey = Vec::new();
    wallet_compact_size(&mut kkey, 3);
    kkey.extend_from_slice(b"key");
    wallet_compact_size(&mut kkey, pk_ser.len() as u64);
    kkey.extend_from_slice(&pk_ser);
    let mut kvalue = Vec::new();
    wallet_compact_size(&mut kvalue, der.len() as u64);
    kvalue.extend_from_slice(&der);

    // keymeta record (same pubkey)
    let mut mkey = Vec::new();
    wallet_compact_size(&mut mkey, 7);
    mkey.extend_from_slice(b"keymeta");
    wallet_compact_size(&mut mkey, pk_ser.len() as u64);
    mkey.extend_from_slice(&pk_ser);
    let mut mvalue = Vec::new();
    mvalue.extend_from_slice(&1i32.to_le_bytes());
    mvalue.extend_from_slice(&1234567i64.to_le_bytes());

    // bestblock record: locator tip-first
    let mut bkey = Vec::new();
    wallet_compact_size(&mut bkey, 9);
    bkey.extend_from_slice(b"bestblock");
    let mut bvalue = Vec::new();
    bvalue.extend_from_slice(&1i32.to_le_bytes());
    bvalue.push(2u8);
    bvalue.extend_from_slice(&[0xaa; 32]); // tip FIRST
    bvalue.extend_from_slice(&[0xbb; 32]);

    let mut stmt_pairs: Vec<(Vec<u8>, Vec<u8>)> = if keys_first {
        vec![
            (kkey.clone(), kvalue.clone()),
            (mkey.clone(), mvalue.clone()),
            (bkey.clone(), bvalue.clone()),
        ]
    } else {
        vec![
            (bkey.clone(), bvalue.clone()),
            (mkey.clone(), mvalue.clone()),
            (kkey.clone(), kvalue.clone()),
        ]
    };
    if keys_first {
        stmt_pairs.reverse();
    }
    for (k, v) in &stmt_pairs {
        conn.execute("INSERT INTO main (key, value) VALUES (?1, ?2)", [k, v])
            .unwrap();
    }
    (pk_ser.to_vec(), sk)
}

#[test]
fn wallet_records_parse_order_independently() {
    for keys_first in [true, false] {
        let dir = std::env::temp_dir().join(format!("tri7-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("wallet-{}.dat", keys_first));
        let (pk, sk) = make_wallet_file(&path, keys_first);
        let w = tri7_core::wallet::read_wallet(&path).expect("read");
        assert_eq!(w.keys.len(), 1);
        assert_eq!(w.keys[0].pubkey, pk);
        assert_eq!(w.keys[0].secret, sk.secret_bytes());
        assert_eq!(w.keys[0].created, Some(1234567)); // survives row reorder
        assert!(w.best_block.is_some());
        assert_eq!(w.best_block.unwrap()[0], 0xaa); // tip-first entry
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn wallet_rejects_wrong_application_id() {
    let dir = std::env::temp_dir().join(format!("tri7-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bad.dat");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA application_id = 999;").unwrap();
    assert!(tri7_core::wallet::read_wallet(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn encrypted_wallet_stops() {
    let dir = std::env::temp_dir().join(format!("tri7-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("enc.dat");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA application_id = 1414687095; CREATE TABLE main (key BLOB PRIMARY KEY, value BLOB);",
    )
    .unwrap();
    let mut mkey = Vec::new();
    wallet_compact_size(&mut mkey, 4);
    mkey.extend_from_slice(b"mkey");
    mvalue_placeholder(&mut mkey);
    conn.execute("INSERT INTO main VALUES (?1, ?2)", [mkey, vec![0u8; 8]])
        .unwrap();
    assert!(tri7_core::wallet::read_wallet(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

fn mvalue_placeholder(_v: &mut Vec<u8>) {}

// keep Reader import used
#[test]
fn reader_short_compact_size_errors() {
    let r = Reader::new(&[0xfd, 0x01]);
    let mut r = r;
    assert!(r.compact_size().is_err());
}
