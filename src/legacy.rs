//! Legacy wire-format types — exact mirrors of the triangles_v5 C++
//! serialization (main.h IMPLEMENT_SERIALIZE blocks).
//!
//! Block:  nVersion i32 | hashPrev 32 | hashMerkle 32 | nTime u32 | nBits u32
//!         | nNonce u32 | vtx (compactsize-prefixed) | vchBlockSig (compactsize-prefixed)
//! Tx:     nVersion i32 | nTime u32 | vin | vout | nLockTime u32
//! Vin:    prevHash 32 | prevN u32 | scriptSig (varlen) | nSequence u32
//! Vout:   nValue i64 | scriptPubKey (varlen)
//! Varint: bitcoin CompactSize.

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

// ---------------------------------------------------------------------------
// Hash9 — legacy 13-round block hash (blake512 -> ... -> fugue512, trim256).
// Implemented by the verbatim sphlib C sources in vendor/ via FFI, so it is
// bit-identical to the shipping binaries by construction.
// ---------------------------------------------------------------------------
extern "C" {
    #[link_name = "hash9"]
    fn hash9_ffi(input: *const u8, len: usize, out: *mut u8);
}

pub fn hash9(data: &[u8]) -> Hash {
    let mut out = [0u8; 32];
    unsafe { hash9_ffi(data.as_ptr(), data.len(), out.as_mut_ptr()) };
    out
}

pub fn sha256d(data: &[u8]) -> Hash {
    let h1 = Sha256::digest(data);
    let h2 = Sha256::digest(h1);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h2);
    out
}

/// Display order used by the legacy client and RPC (uint256::ToString prints
/// the internal bytes reversed).
pub fn hash_to_display(h: &Hash) -> String {
    h.iter().rev().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Cursor position (bytes consumed so far) — used by the block-index tx
    /// range walker to record (start, end) spans.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Advance the cursor n bytes without materializing them (hot path skip).
    pub fn skip(&mut self, n: usize) -> Result<(), String> {
        if self.remaining() < n {
            return Err(format!(
                "short skip: want {} bytes, have {} at offset {}",
                n,
                self.remaining(),
                self.pos
            ));
        }
        self.pos += n;
        Ok(())
    }

    pub fn expect_done(&self) -> Result<(), String> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(format!(
                "trailing bytes: consumed {} of {}",
                self.pos,
                self.buf.len()
            ))
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.remaining() < n {
            return Err(format!(
                "short read: want {} bytes, have {} at offset {}",
                n,
                self.remaining(),
                self.pos
            ));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn u32le(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32le(&mut self) -> Result<i32, String> {
        Ok(self.u32le()? as i32)
    }

    pub fn i64le(&mut self) -> Result<i64, String> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn hash32(&mut self) -> Result<Hash, String> {
        let b = self.take(32)?;
        let mut h = [0u8; 32];
        h.copy_from_slice(b);
        Ok(h)
    }

    /// Bitcoin CompactSize.
    pub fn compact_size(&mut self) -> Result<u64, String> {
        let first = self.u8()?;
        match first {
            0xfd => {
                let b = self.take(2)?;
                Ok(u16::from_le_bytes([b[0], b[1]]) as u64)
            }
            0xfe => self.u32le().map(|v| v as u64),
            0xff => {
                let b = self.take(8)?;
                Ok(u64::from_le_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ]))
            }
            _ => Ok(first as u64),
        }
    }

    pub fn var_bytes(&mut self) -> Result<Vec<u8>, String> {
        let n = self.compact_size()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Vin {
    pub prev_hash: Hash,
    pub prev_n: u32,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

#[derive(Debug, Clone)]
pub struct Vout {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Tx {
    pub version: i32,
    pub time: u32,
    pub vin: Vec<Vin>,
    pub vout: Vec<Vout>,
    pub locktime: u32,
}

impl Tx {
    pub fn read(r: &mut Reader) -> Result<Tx, String> {
        let version = r.i32le()?;
        let time = r.u32le()?;
        let n_vin = r.compact_size()? as usize;
        // Bound by remaining bytes: each input needs >= 41 bytes
        // (32 prev hash + 4 n + 1 min script len + 4 sequence). This caps
        // preallocation so a malformed count can never OOM the node.
        let max_vin = r.remaining() / 41 + 1;
        if n_vin > max_vin {
            return Err(format!("vin count {} exceeds remaining bytes", n_vin));
        }
        let mut vin = Vec::with_capacity(n_vin);
        for _ in 0..n_vin {
            let prev_hash = r.hash32()?;
            let prev_n = r.u32le()?;
            let script_sig = r.var_bytes()?;
            let sequence = r.u32le()?;
            vin.push(Vin {
                prev_hash,
                prev_n,
                script_sig,
                sequence,
            });
        }
        let n_vout = r.compact_size()? as usize;
        // Each output needs >= 9 bytes (8 value + 1 min script len).
        let max_vout = r.remaining() / 9 + 1;
        if n_vout > max_vout {
            return Err(format!("vout count {} exceeds remaining bytes", n_vout));
        }
        let mut vout = Vec::with_capacity(n_vout);
        for _ in 0..n_vout {
            let value = r.i64le()?;
            let script_pubkey = r.var_bytes()?;
            vout.push(Vout {
                value,
                script_pubkey,
            });
        }
        let locktime = r.u32le()?;
        Ok(Tx {
            version,
            time,
            vin,
            vout,
            locktime,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(&self.version.to_le_bytes());
        w.extend_from_slice(&self.time.to_le_bytes());
        push_compact_size(&mut w, self.vin.len() as u64);
        for i in &self.vin {
            w.extend_from_slice(&i.prev_hash);
            w.extend_from_slice(&i.prev_n.to_le_bytes());
            push_compact_size(&mut w, i.script_sig.len() as u64);
            w.extend_from_slice(&i.script_sig);
            w.extend_from_slice(&i.sequence.to_le_bytes());
        }
        push_compact_size(&mut w, self.vout.len() as u64);
        for o in &self.vout {
            w.extend_from_slice(&o.value.to_le_bytes());
            push_compact_size(&mut w, o.script_pubkey.len() as u64);
            w.extend_from_slice(&o.script_pubkey);
        }
        w.extend_from_slice(&self.locktime.to_le_bytes());
        w
    }

    /// SerializeHash(tx) in the legacy client = double SHA-256 of the tx
    /// serialization.
    pub fn hash(&self) -> Hash {
        sha256d(&self.serialize())
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub version: i32,
    pub prev_hash: Hash,
    pub merkle_root: Hash,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
    pub vtx: Vec<Tx>,
    pub block_sig: Vec<u8>,
}

fn push_compact_size(w: &mut Vec<u8>, n: u64) {
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
}

impl Block {
    pub fn read(r: &mut Reader) -> Result<Block, String> {
        let version = r.i32le()?;
        let prev_hash = r.hash32()?;
        let merkle_root = r.hash32()?;
        let time = r.u32le()?;
        let bits = r.u32le()?;
        let nonce = r.u32le()?;
        let n_tx = r.compact_size()? as usize;
        if n_tx > 1_000_000 {
            return Err(format!("implausible tx count {}", n_tx));
        }
        // Capacity is capped so a malformed count cannot trigger a huge
        // pre-allocation; push grows as needed for legitimate blocks.
        let mut vtx = Vec::new();
        for _ in 0..n_tx {
            vtx.push(Tx::read(r)?);
        }
        let block_sig = r.var_bytes()?;
        Ok(Block {
            version,
            prev_hash,
            merkle_root,
            time,
            bits,
            nonce,
            vtx,
            block_sig,
        })
    }

    /// The 80 header bytes hashed by Hash9 (nVersion..nNonce inclusive).
    pub fn header_bytes(&self) -> Vec<u8> {
        let mut w = Vec::with_capacity(80);
        w.extend_from_slice(&self.version.to_le_bytes());
        w.extend_from_slice(&self.prev_hash);
        w.extend_from_slice(&self.merkle_root);
        w.extend_from_slice(&self.time.to_le_bytes());
        w.extend_from_slice(&self.bits.to_le_bytes());
        w.extend_from_slice(&self.nonce.to_le_bytes());
        w
    }

    /// Full CBlock wire form (the P2P `block` message payload): 80-byte
    /// header, CompactSize vtx count, the transactions, then CompactSize-
    /// prefixed vchBlockSig. Round-trips through Block::read.
    pub fn serialize(&self) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(&self.header_bytes());
        push_compact_size(&mut w, self.vtx.len() as u64);
        for tx in &self.vtx {
            w.extend_from_slice(&tx.serialize());
        }
        push_compact_size(&mut w, self.block_sig.len() as u64);
        w.extend_from_slice(&self.block_sig);
        w
    }

    /// CBlock::GetHash() = Hash9 over the 80 header bytes.
    pub fn hash(&self) -> Hash {
        hash9(&self.header_bytes())
    }

    /// Merkle root over tx hashes (double-SHA256 pairing, odd node duplicated),
    /// matching the legacy CBlock::BuildMerkleTree(). A valid block always has
    /// at least one tx; an empty vec returns zero rather than panicking.
    pub fn merkle_root(&self) -> Hash {
        if self.vtx.is_empty() {
            return [0u8; 32];
        }
        let mut level: Vec<Hash> = self.vtx.iter().map(|t| t.hash()).collect();
        if level.len() == 1 {
            return level[0];
        }
        while level.len() > 1 {
            let mut next = Vec::with_capacity((level.len() + 1) / 2);
            let mut i = 0;
            while i < level.len() {
                let a = &level[i];
                let b = if i + 1 < level.len() {
                    &level[i + 1]
                } else {
                    &level[i]
                };
                let mut cat = Vec::with_capacity(64);
                cat.extend_from_slice(a);
                cat.extend_from_slice(b);
                next.push(sha256d(&cat));
                i += 2;
            }
            level = next;
        }
        level[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Block::serialize must round-trip through Block::read byte-exactly —
    /// the P2P `block` payload tri7d serves to peers is produced by this
    /// serializer (disk store hits), so a lossy form would corrupt relays.
    #[test]
    fn block_serialize_roundtrips_through_read() {
        let mut tx = Tx {
            version: 1,
            time: 1_793_656_833,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![1, 2, 3],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 12_950_000,
                    script_pubkey: vec![0x76, 0xa9, 0x14],
                },
                Vout {
                    value: 943_793,
                    script_pubkey: vec![0x6a],
                },
            ],
            locktime: 0,
        };
        tx.vout.push(Vout {
            value: 5,
            script_pubkey: vec![0x51],
        });
        let tx_copy = tx.clone();
        let tx_copy_len = tx.serialize().len();
        let b = Block {
            version: 7,
            prev_hash: [9u8; 32],
            merkle_root: [8u8; 32],
            time: 1_793_656_833,
            bits: 0x2000_ffff,
            nonce: 0,
            vtx: vec![tx],
            block_sig: vec![0xAA; 71],
        };
        let raw = b.serialize();
        // header 80 + cs(1 vtx) + tx bytes + cs(71) + 71
        let expected_len = 80 + 1 + tx_copy_len + 1 + 71;
        assert_eq!(raw.len(), expected_len);
        let mut r = Reader::new(&raw);
        let back = Block::read(&mut r).unwrap();
        r.expect_done().unwrap();
        assert_eq!(back.header_bytes(), b.header_bytes());
        assert_eq!(back.hash(), b.hash());
        assert_eq!(back.vtx.len(), 1);
        assert_eq!(back.vtx[0].serialize(), tx_copy.serialize());
        assert_eq!(back.block_sig, b.block_sig);
        assert_eq!(raw, back.serialize());
    }
}
