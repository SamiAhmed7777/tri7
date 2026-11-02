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

// ---------------------------------------------------------------------------
// Shared full-archive walk (phase 1e refactor): frame iteration, chain
// linkage, stale-branch detection and the coverage contract. validate_pos and
// balances replay over this same skeleton — one implementation of the
// walk-or-die rules, not two.
// ---------------------------------------------------------------------------

use crate::blkfile::BlkFileReader;
use crate::legacy::Block;

/// One step of the walk. `Block(height, block, hash)` delivers the next
/// live-chain block with its Hash9 block hash (computed once by the walker
/// for linkage); `StaleBranch(height)` reports the first frame that does NOT
/// chain to its predecessor (end of the live chain in this archive — frames
/// from there on belong to a side branch and must not be applied); `Eof` is a
/// clean end.
pub enum Step {
    Block(usize, Block, crate::legacy::Hash),
    StaleBranch(usize),
    Eof,
}

pub struct ArchiveWalker {
    rdr: BlkFileReader,
    height: usize,
    prev: Option<crate::legacy::Hash>,
}

impl ArchiveWalker {
    pub fn open(path: &str) -> Result<Self, String> {
        let rdr = BlkFileReader::open(path).map_err(|e| format!("open blk file: {}", e))?;
        Ok(ArchiveWalker {
            rdr,
            height: 0,
            prev: None,
        })
    }

    /// Number of blocks delivered so far (height of the NEXT block).
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn step(&mut self) -> Result<Step, String> {
        let frame = match self.rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return Ok(Step::Eof),
            Err(e) => {
                return Err(format!("frame error at height {}: {}", self.height, e));
            }
        };
        let block = frame
            .block()
            .map_err(|e| format!("parse error at height {}: {}", self.height, e))?;
        if self.height == 0 {
            if block.prev_hash != [0u8; 32] {
                return Err("genesis prev not zero".into());
            }
        } else if block.prev_hash != self.prev.unwrap() {
            return Ok(Step::StaleBranch(self.height));
        }
        let h = self.height;
        let hash = block.hash();
        self.prev = Some(hash);
        self.height += 1;
        Ok(Step::Block(h, block, hash))
    }
}

/// Coverage contract shared by the replay bins: the walk must have reached
/// end_height — cleanly at EOF, or across a stale-branch stop WITH the flag.
/// Mirrors validate_difficulty/validate_pos semantics; the Err text is the
/// FAIL message.
pub fn check_coverage(
    stale_at: Option<usize>,
    covered_to: usize,
    end_height: usize,
    allow_stale: bool,
) -> Result<(), String> {
    if let Some(h) = stale_at {
        if !allow_stale {
            return Err(format!(
                "stale-branch discontinuity at height {} before end_height {} (use --allow-stale)",
                h, end_height
            ));
        }
    } else if covered_to < end_height {
        return Err(format!(
            "EOF at covered height {} before end_height {} — incomplete coverage",
            covered_to, end_height
        ));
    }
    Ok(())
}
