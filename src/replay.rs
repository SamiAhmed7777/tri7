//! Replay block index: compact in-memory chain state for historical
//! validation — every entry mirrors the C++ CBlockIndex fields the consensus
//! rules read (height, time, bits, type flag, entropy bit, stake modifier,
//! proof-of-stake hash).

use crate::legacy::Hash;

#[derive(Clone)]
pub struct IndexEntry {
    pub height: usize,
    pub hash: Hash,
    pub prev_hash: Hash,
    pub time: u32,
    pub bits: u32,
    pub is_proof_of_stake: bool,
    pub entropy_bit: u8,
    pub stake_modifier: u64,
    pub generated_stake_modifier: bool,
    pub hash_proof_of_stake: Hash, // zeros under assume-valid skip
}

pub struct ReplayIndex {
    pub entries: Vec<IndexEntry>,
}

impl ReplayIndex {
    pub fn new() -> Self {
        ReplayIndex {
            entries: Vec::with_capacity(2_300_000),
        }
    }

    pub fn by_height(&self, h: usize) -> Option<&IndexEntry> {
        self.entries.get(h)
    }

    pub fn tip(&self) -> Option<&IndexEntry> {
        self.entries.last()
    }

    pub fn tip_height(&self) -> usize {
        self.entries.len().saturating_sub(1)
    }

    pub fn push(&mut self, e: IndexEntry) {
        self.entries.push(e);
    }
}

impl Default for ReplayIndex {
    fn default() -> Self {
        Self::new()
    }
}
