//! Unit tests for the legacy wire-format layer (src/legacy.rs, src/blkfile.rs).

use std::path::PathBuf;
use tri7_core::blkfile::BlkFileReader;
use tri7_core::legacy::{hash9, hash_to_display, sha256d, Block, Hash, Reader, Tx, Vin, Vout};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode a value as bitcoin CompactSize (mirrors legacy push_compact_size).
fn cs(n: u64) -> Vec<u8> {
    let mut w = Vec::new();
    if n < 0xfd {
        w.push(n as u8);
    } else if n <= 0xffff {
        w.push(0xfd);
        w.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        w.push(0xfe);
        w.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        w.push(0xff);
        w.extend_from_slice(&n.to_le_bytes());
    }
    w
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0);
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// Deterministic toy transaction used by the merkle vectors.
fn mk_tx(seed: u8) -> Tx {
    Tx {
        version: 1,
        time: 0x5000_0000 + seed as u32,
        vin: vec![Vin {
            prev_hash: [seed; 32],
            prev_n: 0xffff_fffd,
            script_sig: vec![seed],
            sequence: 0xffff_fffb,
        }],
        vout: vec![Vout {
            value: 5_000_000_000,
            script_pubkey: vec![0x76, 0xa9, seed],
        }],
        locktime: 0,
    }
}

fn hash_pair(a: &Hash, b: &Hash) -> Hash {
    let mut cat = Vec::with_capacity(64);
    cat.extend_from_slice(a);
    cat.extend_from_slice(b);
    sha256d(&cat)
}

fn block_with_txs(vtx: Vec<Tx>) -> Block {
    Block {
        version: 1,
        prev_hash: [0u8; 32],
        merkle_root: [0u8; 32],
        time: 0,
        bits: 0,
        nonce: 0,
        vtx,
        block_sig: Vec::new(),
    }
}

/// Write bytes to a unique temp file and return its path.
fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tri7-legacy-test-{}-{}", std::process::id(), name));
    std::fs::write(&p, bytes).unwrap();
    p
}

fn frame_bytes(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&magic.to_le_bytes());
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    v
}

// ---------------------------------------------------------------------------
// 1. CompactSize roundtrips at boundaries
// ---------------------------------------------------------------------------

#[test]
fn compact_size_exact_encodings() {
    // (value, expected canonical encoding)
    let cases: &[(u64, &[u8])] = &[
        (0, &[0x00]),
        (0xfc, &[0xfc]),
        (0xfd, &[0xfd, 0xfd, 0x00]),
        (0xfdff, &[0xfd, 0xff, 0xfd]),
        (0xffff, &[0xfd, 0xff, 0xff]),
        (0x10000, &[0xfe, 0x00, 0x00, 0x01, 0x00]),
        (0xffff_ffff, &[0xfe, 0xff, 0xff, 0xff, 0xff]),
        (
            0x1_0000_0000,
            &[0xff, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00],
        ),
        (
            0xffff_ffff_ffff_ffff,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
    ];
    for (v, enc) in cases {
        assert_eq!(cs(*v), enc.to_vec(), "encoding of {}", v);
        let mut r = Reader::new(enc);
        assert_eq!(r.compact_size().unwrap(), *v, "decoding of {}", v);
        assert_eq!(r.remaining(), 0);
    }
}

#[test]
fn compact_size_roundtrip_boundaries() {
    for v in [
        0u64,
        0xfc,
        0xfd,
        0xfdff,
        0x10000,
        u32::MAX as u64,
        0x1_0000_0000,
        0xdead_beef_cafe_f00d,
        u64::MAX,
    ] {
        let enc = cs(v);
        let mut r = Reader::new(&enc);
        assert_eq!(r.compact_size().unwrap(), v, "roundtrip of {}", v);
        assert_eq!(r.remaining(), 0, "roundtrip of {} left bytes", v);
    }
}

#[test]
fn compact_size_non_minimal_is_tolerated() {
    // The legacy decoder does not enforce canonical encodings; a 0xfd-prefixed
    // small value must still decode (bit-exact with the C++ client).
    let mut r = Reader::new(&[0xfd, 0x10, 0x00]);
    assert_eq!(r.compact_size().unwrap(), 0x10);
}

// ---------------------------------------------------------------------------
// 2. Truncated / malformed reader inputs: Err, never panic
// ---------------------------------------------------------------------------

#[test]
fn reader_truncated_scalars_error() {
    assert!(Reader::new(&[]).u8().is_err());
    assert!(Reader::new(&[0x01, 0x02]).u32le().is_err());
    assert!(Reader::new(&[0x01, 0x02, 0x03]).i32le().is_err());
    assert!(Reader::new(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07])
        .i64le()
        .is_err());
    assert!(Reader::new(&[0xab; 31]).hash32().is_err());
}

#[test]
fn reader_truncated_compact_size_error() {
    assert!(Reader::new(&[0xfd]).compact_size().is_err());
    assert!(Reader::new(&[0xfd, 0x01]).compact_size().is_err());
    assert!(Reader::new(&[0xfe, 0x01, 0x02, 0x03])
        .compact_size()
        .is_err());
    assert!(Reader::new(&[0xff, 0x01, 0x02, 0x03, 0x04])
        .compact_size()
        .is_err());
}

#[test]
fn reader_truncated_var_bytes_error() {
    let mut enc = cs(100);
    enc.extend_from_slice(&[0x55; 10]); // claims 100 bytes, provides 10
    assert!(Reader::new(&enc).var_bytes().is_err());
}

#[test]
fn tx_read_truncated_inputs_error() {
    assert!(Tx::read(&mut Reader::new(&[])).is_err());
    // version+time only
    assert!(Tx::read(&mut Reader::new(&[0u8; 8])).is_err());
    // valid header, vin count 1, then truncated mid-input
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(1));
    b.extend_from_slice(&[0x11; 20]);
    assert!(Tx::read(&mut Reader::new(&b)).is_err());
    // vin parses, vout count claims 1, then truncated mid-output
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(1)); // 1 vin
    b.extend_from_slice(&[0x22; 32]); // prev hash
    b.extend_from_slice(&0u32.to_le_bytes()); // prev n
    b.extend_from_slice(&cs(0)); // empty scriptSig
    b.extend_from_slice(&0xffffffffu32.to_le_bytes()); // sequence
    b.extend_from_slice(&cs(1)); // 1 vout
    b.extend_from_slice(&[0x33; 8]); // truncated value
    assert!(Tx::read(&mut Reader::new(&b)).is_err());
}

#[test]
fn block_read_truncated_inputs_error() {
    assert!(Block::read(&mut Reader::new(&[])).is_err());
    // 79-byte header (one short)
    assert!(Block::read(&mut Reader::new(&[0u8; 79])).is_err());
    // 80-byte header, tx count 1, truncated tx
    let mut b = vec![0u8; 80];
    b.extend_from_slice(&cs(1));
    b.extend_from_slice(&[0x44; 12]);
    assert!(Block::read(&mut Reader::new(&b)).is_err());
}

// ---------------------------------------------------------------------------
// 3. vin / vout count bounds rejection
// ---------------------------------------------------------------------------

#[test]
fn tx_rejects_implausible_vin_count() {
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(0xffff_ffff)); // far more inputs than bytes present
    assert!(Tx::read(&mut Reader::new(&b)).is_err());
}

#[test]
fn tx_rejects_implausible_vout_count() {
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(1)); // 1 vin
    b.extend_from_slice(&[0x22; 32]);
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(0));
    b.extend_from_slice(&0xffffffffu32.to_le_bytes());
    b.extend_from_slice(&cs(u64::from(u32::MAX))); // absurd output count
    assert!(Tx::read(&mut Reader::new(&b)).is_err());
}

#[test]
fn tx_count_at_bound_fails_cleanly_in_body() {
    // A count that passes the coarse bound check must still fail with Err
    // (short read), not panic, when the body is not actually there.
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(2)); // passes remaining/41 + 1 == 2 for a 41-byte tail
    b.extend_from_slice(&[0x55; 41]); // exactly one minimal input's worth
    assert!(Tx::read(&mut Reader::new(&b)).is_err());
}

#[test]
fn tx_minimal_valid_roundtrip() {
    // Positive control for the bounds tests above.
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(1));
    b.extend_from_slice(&[0x22; 32]);
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&cs(0));
    b.extend_from_slice(&0xffffffffu32.to_le_bytes());
    b.extend_from_slice(&cs(1));
    b.extend_from_slice(&1000i64.to_le_bytes());
    b.extend_from_slice(&cs(0));
    b.extend_from_slice(&0u32.to_le_bytes());
    let tx = Tx::read(&mut Reader::new(&b)).unwrap();
    assert_eq!(tx.vin.len(), 1);
    assert_eq!(tx.vout.len(), 1);
    assert_eq!(tx.vout[0].value, 1000);
}

// ---------------------------------------------------------------------------
// 4. Merkle root vectors (double-SHA256 pairing, odd node duplicated)
// ---------------------------------------------------------------------------

#[test]
fn merkle_root_single_tx() {
    let tx = mk_tx(1);
    let b = block_with_txs(vec![tx.clone()]);
    assert_eq!(b.merkle_root(), tx.hash());
}

#[test]
fn merkle_root_two_tx() {
    let h0 = mk_tx(1).hash();
    let h1 = mk_tx(2).hash();
    assert_eq!(
        hash_to_display(&h0),
        "e2481bac3555eac6a1120bb9c392b2a3f609dfd0d91b1fb1a4c5a011a4c2bea0"
    );
    assert_eq!(
        hash_to_display(&h1),
        "d9472e298785ea055e90d2c8fb726723d025b345d5a3c7301465030d93b72660"
    );
    let expected = hash_pair(&h0, &h1);
    assert_eq!(
        hash_to_display(&expected),
        "21a7877c2cdeb74666a77f298d22af493bce079bbeb00e68eb31c127170d2f65"
    );
    let b = block_with_txs(vec![mk_tx(1), mk_tx(2)]);
    assert_eq!(b.merkle_root(), expected);
}

#[test]
fn merkle_root_three_tx_odd_duplication() {
    let h0 = mk_tx(1).hash();
    let h1 = mk_tx(2).hash();
    let h2 = mk_tx(3).hash();
    assert_eq!(
        hash_to_display(&h2),
        "690811b1fbcecde5f6207dfe799422b85d5ff76f7c09b41f4c6995325172053b"
    );
    // Level 0: [h0, h1, h2] -> [H(h0|h1), H(h2|h2)]; level 1 -> H(a|b).
    let a = hash_pair(&h0, &h1);
    let b = hash_pair(&h2, &h2);
    let expected = hash_pair(&a, &b);
    assert_eq!(
        hash_to_display(&expected),
        "b200386bfcaf99c43cd3ea985ccb4fcfaeeb1cf17d1542f0858eb5470c866232"
    );
    let blk = block_with_txs(vec![mk_tx(1), mk_tx(2), mk_tx(3)]);
    assert_eq!(blk.merkle_root(), expected);
}

#[test]
fn merkle_root_empty_block_is_zero_not_panic() {
    assert_eq!(block_with_txs(vec![]).merkle_root(), [0u8; 32]);
}

// ---------------------------------------------------------------------------
// 5. Genesis Hash9 vector (frame 0 header of the real chain)
// ---------------------------------------------------------------------------

/// 80-byte genesis header (frame 0 of the live blk0001.dat, nVersion..nNonce).
const GENESIS_HEADER_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000000000001ed548f842bc987ad5a88911e838eec77c9aed4e56b800a7cce7c4af7372f727023cc653ffff00202b000000";
const GENESIS_HASH_DISPLAY: &str =
    "7e7a6e4dd5fe895106fca912dfbacaeaf2a89e76c6a588df8ff96e0e18b96021";

#[test]
fn genesis_hash9_vector() {
    let hdr = hex_decode(GENESIS_HEADER_HEX);
    assert_eq!(hdr.len(), 80);
    assert_eq!(hash_to_display(&hash9(&hdr)), GENESIS_HASH_DISPLAY);
}

// ---------------------------------------------------------------------------
// 6. Frame tests (blkfile.rs)
// ---------------------------------------------------------------------------

const CHAIN_MAGIC: u32 = 0x05223570;

#[test]
fn frame_le_magic_and_length() {
    let body = vec![0xaau8; 200];
    let p = write_tmp("le", &frame_bytes(CHAIN_MAGIC, &body));
    let mut r = BlkFileReader::open(&p).unwrap();
    let f = r.next_frame().unwrap().expect("one frame");
    assert_eq!(f.magic, CHAIN_MAGIC); // bytes on disk: 70 35 22 05
    assert_eq!(f.data, body);
    assert!(r.next_frame().unwrap().is_none());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_zero_padding_tolerance() {
    let body1 = vec![0x01u8; 3];
    let body2 = vec![0x02u8; 5];
    let mut bytes = frame_bytes(CHAIN_MAGIC, &body1);
    bytes.extend_from_slice(&[0u8; 16]); // aligned interstitial padding
    bytes.extend_from_slice(&frame_bytes(CHAIN_MAGIC, &body2));
    bytes.extend_from_slice(&[0u8; 12]); // trailing padding, not 8-byte aligned
    let p = write_tmp("pad", &bytes);
    let mut r = BlkFileReader::open(&p).unwrap();
    let f1 = r.next_frame().unwrap().expect("frame 1");
    assert_eq!(f1.data, body1);
    let f2 = r.next_frame().unwrap().expect("frame 2");
    assert_eq!(f2.data, body2);
    assert!(r.next_frame().unwrap().is_none());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_rejects_zero_length() {
    let p = write_tmp("zerolen", &frame_bytes(CHAIN_MAGIC, &[]));
    let mut r = BlkFileReader::open(&p).unwrap();
    assert!(r.next_frame().is_err());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_rejects_oversized_length() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CHAIN_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&((8u32 << 20) + 1).to_le_bytes());
    let p = write_tmp("biglen", &bytes);
    let mut r = BlkFileReader::open(&p).unwrap();
    assert!(r.next_frame().is_err());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_truncated_body_errors() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CHAIN_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&100u32.to_le_bytes());
    bytes.extend_from_slice(&[0x55; 20]); // body cut short
    let p = write_tmp("trunc", &bytes);
    let mut r = BlkFileReader::open(&p).unwrap();
    assert!(r.next_frame().is_err());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_clean_eof_at_start() {
    let p = write_tmp("empty", &[]);
    let mut r = BlkFileReader::open(&p).unwrap();
    assert!(r.next_frame().unwrap().is_none());
    std::fs::remove_file(&p).ok();
}

#[test]
fn frame_magic_matches_chain_constant() {
    // The live chain's network magic, little-endian on disk.
    let p = write_tmp("magic", &frame_bytes(CHAIN_MAGIC, &[0x00]));
    let mut r = BlkFileReader::open(&p).unwrap();
    let f = r.next_frame().unwrap().expect("one frame");
    assert_eq!(f.magic, 0x05223570);
    std::fs::remove_file(&p).ok();
}
