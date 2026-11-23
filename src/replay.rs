//! Replay block index: compact in-memory chain state for historical
//! validation — every entry mirrors the C++ CBlockIndex fields the consensus
//! rules read (height, time, bits, type flag, entropy bit, stake modifier,
//! proof-of-stake hash).

use crate::legacy::Hash;

use std::borrow::Cow;

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

/// Entries are Cow so a window/follower can hand consensus a BORROWED view of
/// its own Vec (zero-copy per block — the from-zero backfill validates
/// millions of blocks and a per-block Vec clone would dominate), while
/// replay-style builders keep the owned/push form.
pub struct ReplayIndex<'a> {
    pub entries: Cow<'a, [IndexEntry]>,
}

impl<'a> ReplayIndex<'a> {
    pub fn new() -> Self {
        ReplayIndex {
            entries: Cow::Owned(Vec::with_capacity(2_300_000)),
        }
    }

    pub fn borrowed(entries: &'a [IndexEntry]) -> Self {
        ReplayIndex {
            entries: Cow::Borrowed(entries),
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
        self.entries.to_mut().push(e);
    }
}

impl Default for ReplayIndex<'_> {
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

impl Step {
    /// (2026-11-23) small helper for tolerant callers.
    pub fn is_block(&self) -> bool {
        matches!(self, Step::Block(..))
    }
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

    /// (2026-11-23) TOLERANT STEP for callers that treat a StaleBranch as a
    /// hole instead of a hard stop (the wallet live seed): resync forward to
    /// the frame that chains onto our tip and DELIVER it — the resync
    /// consumes the found frame, so the replacement must come back as the
    /// next Step (a plain "skip" would silently drop the chain's block at
    /// this height and shift every later height by one). The fork's own
    /// height stays reserved: the delivered block is the chain's block at
    /// the SAME height the stale frame occupied. Returns Eof when no
    /// replacement exists. Parseable non-chaining frames keep their
    /// StaleBranch semantics for strict callers (the build's coverage
    /// contract) — this method exists only for callers that opt in.
    pub fn step_past_stale(&mut self) -> Result<Step, String> {
        let want = match self.prev.clone() {
            Some(p) => p,
            None => [0u8; 32],
        };
        match self.rdr.resync_to_chained_frame(&want)? {
            Some(f2) => {
                let block = f2
                    .block()
                    .map_err(|e| format!("resynced frame parse: {}", e))?;
                let hash = block.hash();
                let h = self.height;
                self.height += 1;
                self.prev = Some(hash);
                Ok(Step::Block(h, block, hash))
            }
            None => Ok(Step::Eof),
        }
    }

    pub fn step(&mut self) -> Result<Step, String> {
        let frame = match self.rdr.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return Ok(Step::Eof),
            Err(e) => {
                return Err(format!("frame error at height {}: {}", self.height, e));
            }
        };
        // (2026-11-22) DEBRIS RESYNC: only an UNPARSEABLE frame triggers the
        // forward scan — a parseable frame that does not chain is a
        // StaleBranch, and the fork-detection semantics below must keep
        // firing for it (the coverage contract depends on that distinction).
        let block = match frame.block() {
            Ok(b) => b,
            Err(parse_err) => {
                let want = self.prev.clone().unwrap_or([0u8; 32]);
                let resynced = self.rdr.resync_to_chained_frame(&want)?;
                match resynced {
                    Some(f2) => {
                        eprintln!(
                            "archive: resync past unparseable frame near height {} — resumed at a chained frame",
                            self.height
                        );
                        f2.block()
                            .map_err(|e| format!("resynced frame parse error: {}", e))?
                    }
                    None => return Ok(Step::Eof),
                }
            }
        };
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
