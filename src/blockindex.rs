//! Disk-backed block/transaction index over blk0001.dat frames (phase 5b —
//! the native data source for tri7d's RPC surface).
//!
//! Data sources served from here (replacing the legacy-RPC dependency):
//!   - getblock <hash|height>     : frame bytes read directly from disk
//!   - getrawtransaction <txid>   : frame located via the tx index, tx bytes
//!                                  hashed + parsed locally
//!   - getblockhash <height>      : height map (window-authoritative inside
//!                                  the tipstate window, frame-order below)
//!
//! Footprint: NO live UTXO set and NO full chain state is kept. Three flat
//! arrays are built once (startup, background thread) and cached to disk
//! under <state-dir>/index/ so restarts skip the rescan:
//!   - block index : sorted Vec<(key8, frame_off, frame_seq)> — one entry per
//!                   frame, 24 B (~2.2M frames → ~53 MB)
//!   - height map  : seq→height u32 + height→offset u64 (~26 MB)
//!   - tx index    : sorted Vec<(key8, frame_off)> — one entry per tx, 16 B
//!                   (~4.5M txs → ~72 MB)
//! Keys are the FIRST 8 BYTES of the internal hash (u64 LE). A truncated key
//! alone would be probabilistic, so every lookup re-reads the frame and
//! verifies the full 32-byte hash (Hash9 for blocks, sha256d for txs) before
//! answering — the key only narrows the binary search, never proves identity.
//!
//! Height assignment (the hard part — blk0001.dat interleaves reorged
//! stale-branch frames around the live tail, so naive prev-link threading
//! follows the WRONG branch past a seam):
//!   - heights below the tipstate window base: sequential chaining rule — a
//!     frame is accepted iff its prev_hash == hash of the last accepted
//!     frame; non-chaining frames are skipped (stale branches) and keep no
//!     height. Genesis..~h2201090 linkage was proven by phase 1a, and the
//!     whole prefix is re-proven at build time by the anchor rule below — if
//!     it ever fails, heights are marked unusable and height-keyed lookups
//!     degrade to the legacy proxy.
//!   - heights inside the window [base..tip]: AUTHORITATIVE — a frame is
//!     accepted iff Hash9(header) == the tipstate window's hash for that
//!     height (the daemon-anchored, checksum-chain-verified index). The
//!     accept at the window base doubles as the anchor: reaching it proves
//!     every earlier link threaded correctly.
//!   - heights above the persisted tip (chain resumed): chaining rule again.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::legacy::{hash9, Block, Hash, Reader, Tx};

/// First 8 bytes of an internal hash as the lookup key.
fn key8(h: &Hash) -> u64 {
    u64::from_le_bytes(h[0..8].try_into().unwrap())
}

/// Sentinel: frame not on the threaded main chain / height unassigned.
pub const HEIGHT_UNSET: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Index containers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct BlockEntry {
    pub key: u64,
    pub off: u64, // frame start (magic byte) in the blocks file
    pub seq: u32, // frame sequence number in file order
}

#[derive(Clone, Copy, Debug)]
pub struct TxEntry {
    pub key: u64,
    pub off: u64, // containing frame start
}

#[derive(Default)]
pub struct BlockIndex {
    /// sorted by key
    pub entries: Vec<BlockEntry>,
    /// frame seq -> main-chain height (HEIGHT_UNSET for skipped frames)
    pub heights_by_seq: Vec<u32>,
    /// main-chain height -> frame offset; len = threaded tip height + 1
    pub height_off: Vec<u64>,
    pub frames: usize,
}

#[derive(Default)]
pub struct TxIndex {
    /// sorted by key
    pub entries: Vec<TxEntry>,
    pub txs: usize,
}

impl BlockIndex {
    pub fn candidates_by_key(&self, key: u64) -> &[BlockEntry] {
        let lo = self.entries.partition_point(|e| e.key < key);
        let hi = lo + self.entries[lo..].partition_point(|e| e.key == key);
        &self.entries[lo..hi]
    }

    pub fn offset_by_height(&self, h: u64) -> Option<u64> {
        // height_off is indexed BY HEIGHT. Gaps are padded with HEIGHT_GAP so
        // that one skipped frame cannot shift every later height onto the
        // wrong offset (which made lookups return a neighbour's body).
        match self.height_off.get(h as usize) {
            Some(&HEIGHT_GAP) | None => None,
            Some(o) => Some(*o),
        }
    }

    pub fn height_of_seq(&self, seq: u32) -> Option<u64> {
        let h = *self.heights_by_seq.get(seq as usize)?;
        if h == HEIGHT_UNSET {
            None
        } else {
            Some(h as u64)
        }
    }

    pub fn tip_height(&self) -> Option<u64> {
        let mut i = self.height_off.len();
        while i > 0 {
            i -= 1;
            if self.height_off[i] != HEIGHT_GAP {
                return Some(i as u64);
            }
        }
        None
    }
}

impl TxIndex {
    pub fn candidates_by_key(&self, key: u64) -> &[TxEntry] {
        let lo = self.entries.partition_point(|e| e.key < key);
        let hi = lo + self.entries[lo..].partition_point(|e| e.key == key);
        &self.entries[lo..hi]
    }
}

// ---------------------------------------------------------------------------
// Frame walking + tx range walker (allocation-free hot path)
// ---------------------------------------------------------------------------

/// Wire frame magic shared with the embedded node's data-truth store check
/// (`store_has_bodies`): [u32 LE magic][u32 LE len][body] frames.
pub const FRAME_MAGIC: u32 = 0x0522_3570; // LE [70 35 22 05]

/// On-disk frame envelope for a raw block body (magic + len + body). Public
/// so the follow_tip tests and ops tooling can synthetically build stores
/// byte-identical to what append_block writes.
pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(8 + body.len());
    f.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(body);
    f
}

/// Writer/reader frame-length bound, shared with the embedded node's
/// data-truth store check (`store_has_bodies`): no real block is anywhere
/// near 8 MiB, and no plausible corruption deserves the benefit of the doubt.
pub const MAX_FRAME: usize = 8 << 20;

/// Walk the transactions of a serialized block, invoking `cb(start, end)`
/// with each tx byte range. Structure walk only — no per-tx allocation.
pub fn walk_tx_ranges(
    body: &[u8],
    cb: &mut dyn FnMut(usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    let mut r = Reader::new(body);
    r.skip(80)?; // header
    let n_tx = r.compact_size()? as usize;
    if n_tx > 1_000_000 {
        return Err(format!("implausible tx count {}", n_tx));
    }
    for _ in 0..n_tx {
        let start = r.pos();
        let _v = r.i32le()?;
        let _t = r.u32le()?;
        let n_vin = r.compact_size()? as usize;
        if n_vin > 100_000 {
            return Err(format!("implausible vin count {}", n_vin));
        }
        for _ in 0..n_vin {
            r.skip(36)?; // prev hash + prev n
            let _s = r.var_bytes()?; // scriptSig
            r.u32le()?; // sequence
        }
        let n_vout = r.compact_size()? as usize;
        if n_vout > 100_000 {
            return Err(format!("implausible vout count {}", n_vout));
        }
        for _ in 0..n_vout {
            r.skip(8)?; // value
            let _s = r.var_bytes()?; // scriptPubKey
        }
        r.u32le()?; // locktime
        cb(start, r.pos())?;
    }
    Ok(())
}

/// Hash9 over the 80-byte header at frame body start.
/// Sentinel for "this height has no frame" inside `height_off`, which must be
/// indexable by absolute height.
const HEIGHT_GAP: u64 = u64::MAX;

/// Record `off` at absolute height `h`, padding any skipped heights with the
/// sentinel so indices stay aligned with real heights.
fn push_height_off(v: &mut Vec<u64>, h: u64, off: u64) {
    let want = (h as usize) + 1;
    if v.len() < want {
        v.resize(want, HEIGHT_GAP);
    }
    if let Some(slot) = v.get_mut(h as usize) {
        if *slot == HEIGHT_GAP {
            *slot = off;
        }
    }
}

fn header_hash(body: &[u8]) -> Result<Hash, String> {
    if body.len() < 80 {
        return Err("frame body shorter than header".into());
    }
    Ok(hash9(&body[..80]))
}

/// Light sanity gate for the very first frame (genesis tripwire): the file
/// must begin with the 200-byte genesis frame carrying a single coinbase tx.
fn genesis_sane(body: &[u8]) -> Result<(), String> {
    if body.len() != 200 {
        return Err(format!(
            "first frame is {} bytes; the genesis frame is 200 — refusing to anchor height 0 elsewhere",
            body.len()
        ));
    }
    let mut r = Reader::new(body);
    let b = Block::read(&mut r)?;
    r.expect_done()?;
    if b.vtx.len() != 1 {
        return Err("genesis frame carries more than one tx".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// The chain-threading oracle: authoritative hashes for the window
/// [base..=tip], internal byte order (from the tipstate entries).
#[derive(Clone)]
pub struct BuildOracle {
    pub base: u64,
    /// hashes[h - base] for h in [base..=tip]
    pub hashes: Vec<Hash>,
    pub tip: u64,
}

pub struct IndexBuild {
    pub blocks: BlockIndex,
    pub txs: TxIndex,
    pub frames: usize,
    pub txs_indexed: usize,
    /// true iff the threaded chain reached the window base (anchor) AND ended
    /// at the window tip hash — heights are authoritative only then
    pub heights_ok: bool,
    pub skipped_frames: usize,
    /// (item 44) frames that changed hands when the window re-threaded a fork:
    /// canonical side-branch frames given a height + stranded first-arrival
    /// frames that lost one.
    pub fork_adopted: usize,
    /// Offset of the file's torn tail: the scan stopped on a SHORT final frame
    /// (a power cut between the amortized fsyncs) and every frame before this
    /// offset is indexed. None = the file ends on a frame boundary. The store
    /// keeps it so `append_block` can cut the partial frame before writing —
    /// appending after those bytes would leave a valid frame behind garbage,
    /// which every later scan reads as MID-FILE corruption (a hard error).
    pub torn_tail: Option<u64>,
}

/// A frame the thread refused, linked to its parent (item 44 fork-aware
/// threading): enough to walk a lineage back to the thread once the window
/// proves it canonical.
#[derive(Clone, Copy)]
struct SideFrame {
    parent: Hash,
    off: u64,
    seq: u32,
}

/// Refused frames by block hash. Small by nature: a store holds one stale
/// frame per reorg plus the canonical frames a fork stranded.
type SideFrames = std::collections::HashMap<Hash, SideFrame>;

/// The authoritative window continues from `prev` — the parent of an oracle
/// frame the thread did NOT reach — so the lineage below `prev` IS the
/// canonical chain, whatever order the file stored it in. The lineage is
/// walked back through the refused frames until it meets a frame the thread
/// already heights; those frames take the heights leading up to `oracle_h`,
/// and every frame already holding one of those heights (the abandoned
/// first-arrival branch) gives its height up. Returns how many frames changed
/// hands (0 = nothing to do).
///
/// Parent links only: a stale sibling stored between a canonical parent and
/// its canonical child (a staker's own orphaned block) can never derail the
/// walk, and the walk cannot cycle (a block's parent hash is fixed). LIMIT: a
/// canonical body that was never stored cuts the walk — nothing below it can
/// be PROVEN canonical (no hash link), so frames stranded below it stay
/// height-less until gap recovery stores the body and a rebuild runs.
fn adopt_lineage(
    sides: &SideFrames,
    prev: &Hash,
    oracle_h: u64,
    thread_h: u64,
    seq: u32,
    height_off: &mut Vec<u64>,
    heights_by_seq: &mut [u32],
    skipped: &mut usize,
) -> usize {
    let mut lineage: Vec<SideFrame> = Vec::new();
    let mut cur = *prev;
    while let Some(f) = sides.get(&cur).copied() {
        match heights_by_seq.get(f.seq as usize) {
            Some(h) if *h != HEIGHT_UNSET => break, // back on the thread
            _ => {}
        }
        lineage.push(f);
        cur = f.parent;
    }
    let Some(first) = oracle_h.checked_sub(lineage.len() as u64) else {
        return 0;
    };
    let mut moved = 0usize;
    // The lineage takes [first ..= oracle_h - 1] and NOTHING above. When the
    // thread already runs PAST the oracle frame — a child stored before its
    // parent (DNS2 h2218631/32) — the frames at heights >= oracle_h are the
    // canonical chain ABOVE this late frame, not an abandoned branch below
    // it; clearing up to thread_h there wiped the child and left a hole.
    if let Some(last) = oracle_h.checked_sub(1).map(|l| l.min(thread_h)) {
        if last >= first {
            // Whole-range scan, no early exit: duplicate frames can move the
            // thread's height backwards, so heights are NOT monotonic along
            // the file and a "stop below the band" shortcut would leave
            // stale slots.
            for slot in height_off.iter_mut().take(last as usize + 1).skip(first as usize) {
                *slot = HEIGHT_GAP;
            }
            for h in heights_by_seq.iter_mut().take(seq as usize) {
                if *h != HEIGHT_UNSET && (*h as u64) >= first && (*h as u64) <= last {
                    *h = HEIGHT_UNSET;
                    moved += 1;
                }
            }
            *skipped += moved;
        }
    }
    for (i, f) in lineage.iter().enumerate() {
        heights_by_seq[f.seq as usize] = (oracle_h - 1 - i as u64) as u32;
        push_height_off(height_off, oracle_h - 1 - i as u64, f.off);
    }
    *skipped -= lineage.len();
    moved + lineage.len()
}

/// Scan the blocks file sequentially, building all three indexes.
pub fn build_index(
    path: &Path,
    oracle: Option<&BuildOracle>,
    progress: &dyn Fn(usize), // frames processed so far
) -> Result<IndexBuild, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {}", path.display(), e))?;
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut blocks: Vec<BlockEntry> = Vec::new();
    let mut txs: Vec<TxEntry> = Vec::new();
    let mut heights_by_seq: Vec<u32> = Vec::new();
    let mut height_off: Vec<u64> = Vec::new();

    let mut body = Vec::new();
    let mut hdr = [0u8; 8];
    let mut seq: u32 = 0;
    let mut off: u64 = 0;
    let mut height: u64 = 0;
    let mut last_accepted: Option<Hash> = None;
    let mut skipped = 0usize;
    let mut tx_count = 0usize;
    // (item 44) refused frames awaiting an oracle verdict (by block hash, each
    // linked to its parent), and how many frames have changed hands so far
    // (reported to the operator).
    let mut sides = SideFrames::new();
    let mut fork_adopted = 0usize;
    // Torn tail: a short final frame is a LOST TAIL, not corruption.
    // append_block fsyncs every 256 frames, so a power cut leaves exactly this
    // shape, and the old code failed the WHOLE index build on it — the node
    // then served no blocks at all until someone deleted the file by hand.
    // The scan stops at the last whole frame and reports the cut point.
    let mut torn_tail: Option<u64> = None;
    // hash -> height for the authoritative window, built ONCE. Scanning the
    // window per frame is O(frames x window) on a cold 2.2M-frame build.
    let oracle_map: Option<std::collections::HashMap<Hash, u64>> =
        oracle.map(|o| o.hashes.iter().enumerate().map(|(i, h)| (*h, o.base + i as u64)).collect());

    loop {
        let frame_off = off;
        let got = match read_up_to(&mut r, &mut hdr) {
            Ok(n) => n,
            Err(e) => return Err(e),
        };
        if got == 0 {
            break; // clean EOF on a frame boundary
        }
        if got < hdr.len() {
            if hdr[..got].iter().all(|&b| b == 0) {
                break; // NUL padding tail, not a frame
            }
            torn_tail = Some(frame_off); // header cut mid-write
            break;
        }
        let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if magic == 0 {
            off += 8;
            continue; // NUL padding between files
        }
        if magic != FRAME_MAGIC {
            return Err(format!(
                "magic {magic:08x} at offset {off} != {FRAME_MAGIC:08x}"
            ));
        }
        if len == 0 || len > MAX_FRAME {
            return Err(format!(
                "implausible frame length {} at offset {}",
                len, off
            ));
        }
        body.clear();
        body.resize(len, 0);
        match read_full_or_eof(&mut r, &mut body) {
            Ok(true) => {}
            Ok(false) => {
                // Body cut by EOF: the header of the final frame landed but
                // the body did not. Same lost-tail shape as a short header.
                torn_tail = Some(frame_off);
                break;
            }
            Err(e) => return Err(format!("frame body at offset {}: {}", off, e)),
        }

        let hash = header_hash(&body).map_err(|e| format!("offset {}: {}", off, e))?;

        // decide accept / skip for HEIGHT threading
        let accepted_here;
        if last_accepted.is_none() {
            // Frame 0 anchors the thread. Genesis (the normal snapshot case)
            // anchors height 0. A MID-CHAIN store — a from-zero resume whose
            // file starts at an arbitrary height — anchors at the FIRST frame
            // whose hash matches the oracle window; frames before that anchor
            // stay hash-indexed but height-less (HEIGHT_UNSET), and the chain
            // threads upward from the anchor. Without this, genesis_sane
            // failed the whole build on every resumed from-zero store and
            // append_block buffered bodies in RAM forever (DNS3: three runs
            // lost their bodies to exactly this).
            match genesis_sane(&body) {
                Ok(()) => {
                    accepted_here = true; // frame 0 anchors height 0
                    height = 0;
                }
                Err(_) => match oracle_map.as_ref().and_then(|m| m.get(&hash).copied()) {
                    Some(h0) => {
                        accepted_here = true;
                        height = h0;
                    }
                    None => accepted_here = false,
                },
            }
        } else {
            // HASH-FIRST AUTHORITY. A store can legitimately hold more than
            // one branch: a reorg leaves the abandoned frames behind, and a
            // node that forked and later adopted the heavier chain has its own
            // blocks in the middle of the file. Positional chaining then
            // refuses the canonical bodies ("does not follow the previous
            // frame") and those heights go missing from the height map even
            // though the tipstate oracle knows them.
            //
            // So before chaining, ask the authoritative window: if this frame
            // IS the block at some height, that is its height — hash equality
            // with the tipstate is stronger evidence than file order.
            let by_oracle = oracle_map.as_ref().and_then(|m| m.get(&hash).copied());
            if let Some(hx) = by_oracle {
                // (item 44) When the thread does NOT reach this oracle frame —
                // a fork put the first-stored (losing) child on the thread and
                // stranded it — the side branch that leads into the oracle
                // frame is the canonical lineage: it takes the heights and
                // the stranded frames lose theirs. Below the window base the
                // build had no other way to tell the two children of one
                // parent apart (DNS2: 5515 canonical frames height-less, the
                // loser answering getblockhash at its height, and the hole
                // growing at every rebuild as the window base moved up).
                let reaches = body.len() >= 36
                    && body[4..36] == last_accepted.as_ref().unwrap()[..]
                    && hx == height + 1;
                if !reaches {
                    let parent: Hash = body[4..36].try_into().unwrap();
                    fork_adopted += adopt_lineage(
                        &sides,
                        &parent,
                        hx,
                        height,
                        seq,
                        &mut height_off,
                        &mut heights_by_seq,
                        &mut skipped,
                    );
                }
                // Continue threading from this height — unless the thread
                // already runs PAST it (the child reached the file before its
                // parent): the higher frame stays the frontier and the late
                // frame has only claimed its own slot.
                if hx >= height {
                    last_accepted = Some(hash);
                    height = hx;
                }
                push_height_off(&mut height_off, hx, off);
                heights_by_seq.push(hx as u32);
                blocks.push(BlockEntry {
                    key: key8(&hash),
                    off,
                    seq,
                });
                walk_tx_ranges(&body, &mut |start, end| {
                    let txid = sha256d(&body[start..end]);
                    txs.push(TxEntry {
                        key: key8(&txid),
                        off,
                    });
                    tx_count += 1;
                    Ok(())
                })?;
                seq += 1;
                off += (8 + len) as u64;
                if seq % 100_000 == 0 {
                    progress(seq as usize);
                }
                continue;
            }

            let cand = height + 1;
            let in_window = oracle
                .map(|o| cand >= o.base && cand <= o.tip)
                .unwrap_or(false);
            let ok = if in_window {
                // authoritative: hash must equal the window's hash at cand
                let o = oracle.unwrap();
                hash == o.hashes[(cand - o.base) as usize]
            } else {
                // chaining rule (below window base, or appends past tip)
                let prev_want = last_accepted.as_ref().unwrap();
                body.len() >= 36 && body[4..36] == prev_want[..]
            };
            if ok {
                accepted_here = true;
                height = cand;
            } else {
                accepted_here = false;
            }
        }

        if accepted_here {
            last_accepted = Some(hash);
            push_height_off(&mut height_off, height, off);
            heights_by_seq.push(height as u32);
        } else {
            skipped += 1;
            heights_by_seq.push(HEIGHT_UNSET);
            // refused by the thread: maybe the canonical branch's first block
            // (stored after the fork's loser) — keep it until the window says
            if last_accepted.is_some() && body.len() >= 36 {
                let parent: Hash = body[4..36].try_into().unwrap();
                sides.insert(hash, SideFrame { parent, off, seq });
            }
        }
        blocks.push(BlockEntry {
            key: key8(&hash),
            off,
            seq,
        });
        // tx index: every frame, stale or live (same bytes = same txid).
        // SerializeHash is DOUBLE sha256 — sha256d, exactly like Tx::hash().
        let frame_off = off;
        walk_tx_ranges(&body, &mut |start, end| {
            let txid = sha256d(&body[start..end]);
            txs.push(TxEntry {
                key: key8(&txid),
                off: frame_off,
            });
            tx_count += 1;
            Ok(())
        })?;

        seq += 1;
        off += (8 + len) as u64;
        if seq % 100_000 == 0 {
            progress(seq as usize);
        }
    }
    progress(seq as usize);

    // An EMPTY file is a VALID store state, not an error: the from-zero
    // sync starts with a freshly created (0-byte) blocks file and grows it
    // append-only. Rejecting it here made the index build fail forever on a
    // bare machine, so append_block buffered EVERY accepted body in RAM,
    // nothing ever reached disk, and a restart lost the whole validated
    // walk (DNS3, 2026-11-03: tip h17524 fully validated, zero bytes on
    // disk). A 0-frame index is immediately replaced by real frames through
    // the append path; heights_ok=false until real frames thread.
    if blocks.is_empty() && skipped == 0 && seq == 0 && (off == 0 || torn_tail.is_some()) {
        // completely empty file (or one whose only frame was cut mid-write
        // and never fsynced): fall through with a 0-frame build
    } else if blocks.is_empty() {
        return Err("no frames found".into());
    }

    // Heights are usable from genesis up to the height the thread actually
    // REACHED: every in-window accept requires an exact oracle-hash match and
    // every below-window accept requires prev-hash chaining from genesis (or
    // from a mid-chain oracle anchor), so the map is authoritative wherever it
    // is populated. A frame missing ABOVE the reached height (e.g. a body that
    // lived only in a memory cache and was lost on restart) shortens the map —
    // heights below it stay valid and servable (gap recovery appends the
    // missing body later; CACHE_VERSION 2). The old gate (map valid only when
    // the thread ended exactly at the window tip) voided ALL height lookups
    // whenever one tip frame was missing, which blanked every
    // getblock/getblockhash on the node.
    // Reached-window-base rule (generalized for the mid-chain anchor): the
    // anchor height + last threaded height must reach the oracle window base.
    let heights_ok = match oracle {
        Some(o) => height >= o.base as u64,
        None => true, // oracle-less build: the sequential run IS the truth
    };

    // sort the two keyed indexes in place (no extra allocation)
    blocks.sort_unstable_by_key(|e| e.key);
    txs.sort_unstable_by_key(|e| e.key);

    let frames = seq as usize;
    Ok(IndexBuild {
        blocks: BlockIndex {
            entries: blocks,
            heights_by_seq,
            height_off,
            frames,
        },
        txs: TxIndex {
            entries: txs,
            txs: tx_count,
        },
        frames,
        txs_indexed: tx_count,
        heights_ok,
        skipped_frames: skipped,
        fork_adopted,
        torn_tail,
    })
}

/// Read up to `buf.len()` bytes, stopping at EOF. Returns the bytes read
/// (0 = clean EOF at the first byte). A short count is NOT an error — the
/// caller decides whether it is a torn tail (final frame) or corruption.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("frame read: {}", e)),
        }
    }
    Ok(got)
}

/// Fill `buf` completely; Ok(false) when EOF cut it short (the torn-tail
/// shape). Ok(true) = full frame body.
fn read_full_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, String> {
    Ok(read_up_to(r, buf)? == buf.len())
}

// ---------------------------------------------------------------------------
// Disk cache — restarts skip the rescan. Header carries the invalidation
// inputs (blk file length + window base/tip + heights_ok) and a sha256d
// payload digest; any mismatch is a silent rebuild, never an error.
// ---------------------------------------------------------------------------

const CACHE_MAGIC_BLOCKS: u32 = 0x5452_4942; // TRIB
const CACHE_MAGIC_TX: u32 = 0x5452_4954; // TRIT
/// v2: heights_ok semantics changed (a frame missing ABOVE the threaded tip
/// shortens the map instead of invalidating it) + the append appendix. v1
/// caches carry the old gate — force one rebuild.
/// v3: height_off became height-indexed (gap-padded). A v2 cache holds the
/// compact list, where every skipped frame shifted later heights onto the
/// wrong offsets — serving neighbour bodies and reporting false gaps.
/// v4: the blocks head carries the torn-tail offset. A cache written over a
/// file that ends mid-frame must TELL the store where the tear is; without it
/// the next append wrote a valid frame behind a half-written one and every
/// later scan saw mid-file corruption.
/// v5: fork-aware threading. A v4 map can carry a fork hole (the losing child
/// holding a height, the canonical frames below the window base height-less)
/// and a cache written from such a map would keep serving it after the fix —
/// force one rebuild.
const CACHE_VERSION: u32 = 5;
const BLOCKS_HEAD_LEN: usize = 57;
const TX_HEAD_LEN: usize = 32;

fn sha256d(data: &[u8]) -> [u8; 32] {
    let h1 = Sha256::digest(data);
    let mut h2 = Sha256::new();
    h2.update(h1);
    h2.finalize().into()
}

fn write_cache(path: &Path, head: &[u8], payload: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("bin.tmp");
    let mut f = File::create(&tmp).map_err(|e| format!("create {}: {}", tmp.display(), e))?;
    f.write_all(head)
        .map_err(|e| format!("write head: {}", e))?;
    f.write_all(&sha256d(payload))
        .map_err(|e| format!("write digest: {}", e))?;
    f.write_all(payload)
        .map_err(|e| format!("write payload: {}", e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("rename cache: {}", e))?;
    Ok(())
}

use std::io::Write;

fn write_blocks_cache(
    path: &Path,
    b: &BlockIndex,
    blk_len: u64,
    heights_ok: bool,
    oracle: Option<&BuildOracle>,
    torn_tail: Option<u64>,
) -> Result<(), String> {
    let mut head = Vec::with_capacity(BLOCKS_HEAD_LEN);
    head.extend_from_slice(&CACHE_MAGIC_BLOCKS.to_le_bytes());
    head.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    head.extend_from_slice(&blk_len.to_le_bytes());
    head.extend_from_slice(&(b.frames as u64).to_le_bytes());
    head.extend_from_slice(&(b.height_off.len() as u64).to_le_bytes());
    match oracle {
        Some(o) => {
            head.extend_from_slice(&o.base.to_le_bytes());
            head.extend_from_slice(&o.tip.to_le_bytes());
        }
        None => {
            head.extend_from_slice(&u64::MAX.to_le_bytes());
            head.extend_from_slice(&u64::MAX.to_le_bytes());
        }
    }
    head.push(if heights_ok { 1 } else { 0 });
    head.extend_from_slice(&torn_tail.unwrap_or(u64::MAX).to_le_bytes());
    debug_assert_eq!(head.len(), BLOCKS_HEAD_LEN);

    let mut payload = Vec::with_capacity(
        b.entries.len() * 24 + b.heights_by_seq.len() * 4 + b.height_off.len() * 8,
    );
    for e in &b.entries {
        payload.extend_from_slice(&e.key.to_le_bytes());
        payload.extend_from_slice(&e.off.to_le_bytes());
        payload.extend_from_slice(&e.seq.to_le_bytes());
    }
    for h in &b.heights_by_seq {
        payload.extend_from_slice(&h.to_le_bytes());
    }
    for o in &b.height_off {
        payload.extend_from_slice(&o.to_le_bytes());
    }
    write_cache(path, &head, &payload)
}

fn write_tx_cache(path: &Path, t: &TxIndex, blk_len: u64, tip: u64) -> Result<(), String> {
    let mut head = Vec::with_capacity(TX_HEAD_LEN);
    head.extend_from_slice(&CACHE_MAGIC_TX.to_le_bytes());
    head.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    head.extend_from_slice(&blk_len.to_le_bytes());
    head.extend_from_slice(&tip.to_le_bytes());
    head.extend_from_slice(&(t.txs as u64).to_le_bytes());
    debug_assert_eq!(head.len(), TX_HEAD_LEN);

    let mut payload = Vec::with_capacity(t.entries.len() * 16);
    for e in &t.entries {
        payload.extend_from_slice(&e.key.to_le_bytes());
        payload.extend_from_slice(&e.off.to_le_bytes());
    }
    write_cache(path, &head, &payload)
}

fn load_cache_head(path: &Path, expect_magic: u32, head_len: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut f = File::open(path).ok()?; // absent = cold cache
    let mut head = vec![0u8; head_len];
    f.read_exact(&mut head).ok()?; // truncated/corrupt → rebuild
    let magic = u32::from_le_bytes(head[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(head[4..8].try_into().unwrap());
    if magic != expect_magic || version != CACHE_VERSION {
        return None;
    }
    let mut digest = [0u8; 32];
    f.read_exact(&mut digest).ok()?;
    let mut payload = Vec::new();
    f.read_to_end(&mut payload).ok()?;
    if sha256d(&payload) != digest {
        return None; // corrupt → rebuild
    }
    Some((head, payload))
}

/// Attempt a cache load; Ok(None) = rebuild needed. Any structural mismatch
/// (lengths, digest, oracle change) is a silent rebuild, never an error.
pub fn load_caches(
    dir: &Path,
    blk_len: u64,
    oracle: Option<&BuildOracle>,
) -> Result<Option<IndexBuild>, String> {
    let (base_sentinel, tip_sentinel) = match oracle {
        Some(o) => (o.base, o.tip),
        None => (u64::MAX, u64::MAX),
    };
    // blocks cache
    let bpath = dir.join("blockidx.bin");
    let (bhead, bpayload) = match load_cache_head(&bpath, CACHE_MAGIC_BLOCKS, BLOCKS_HEAD_LEN) {
        Some(x) => x,
        None => return Ok(None),
    };
    let file_len = u64::from_le_bytes(bhead[8..16].try_into().unwrap());
    let frames = u64::from_le_bytes(bhead[16..24].try_into().unwrap()) as usize;
    let n_heights = u64::from_le_bytes(bhead[24..32].try_into().unwrap()) as usize;
    let cbase = u64::from_le_bytes(bhead[32..40].try_into().unwrap());
    let ctip = u64::from_le_bytes(bhead[40..48].try_into().unwrap());
    let c_heights_ok = bhead[48] == 1;
    let c_torn = u64::from_le_bytes(bhead[49..57].try_into().unwrap());
    if file_len != blk_len || cbase != base_sentinel || ctip != tip_sentinel {
        return Ok(None);
    }
    // entries serialize as 8(key)+8(off)+4(seq) = 20 bytes (no struct padding)
    let need = frames * 20 + frames * 4 + n_heights * 8;
    if bpayload.len() != need || frames == 0 {
        return Ok(None);
    }
    let mut entries = Vec::with_capacity(frames);
    let mut p = 0usize;
    for _ in 0..frames {
        entries.push(BlockEntry {
            key: u64::from_le_bytes(bpayload[p..p + 8].try_into().unwrap()),
            off: u64::from_le_bytes(bpayload[p + 8..p + 16].try_into().unwrap()),
            seq: u32::from_le_bytes(bpayload[p + 16..p + 20].try_into().unwrap()),
        });
        p += 20;
    }
    let mut heights_by_seq = Vec::with_capacity(frames);
    for _ in 0..frames {
        heights_by_seq.push(u32::from_le_bytes(bpayload[p..p + 4].try_into().unwrap()));
        p += 4;
    }
    let mut height_off = Vec::with_capacity(n_heights);
    for _ in 0..n_heights {
        height_off.push(u64::from_le_bytes(bpayload[p..p + 8].try_into().unwrap()));
        p += 8;
    }
    let blocks = BlockIndex {
        entries,
        heights_by_seq,
        height_off,
        frames,
    };

    // tx cache
    let tpath = dir.join("txidx.bin");
    let (thead, tpayload) = match load_cache_head(&tpath, CACHE_MAGIC_TX, TX_HEAD_LEN) {
        Some(x) => x,
        None => return Ok(None),
    };
    let tfile_len = u64::from_le_bytes(thead[8..16].try_into().unwrap());
    let _ttip = u64::from_le_bytes(thead[16..24].try_into().unwrap());
    let n_tx = u64::from_le_bytes(thead[24..32].try_into().unwrap()) as usize;
    if tfile_len != blk_len || n_tx == 0 || tpayload.len() != n_tx * 16 {
        return Ok(None);
    }
    let mut tentries = Vec::with_capacity(n_tx);
    let mut q = 0usize;
    for _ in 0..n_tx {
        tentries.push(TxEntry {
            key: u64::from_le_bytes(tpayload[q..q + 8].try_into().unwrap()),
            off: u64::from_le_bytes(tpayload[q + 8..q + 16].try_into().unwrap()),
        });
        q += 16;
    }
    let txs = TxIndex {
        entries: tentries,
        txs: n_tx,
    };

    Ok(Some(IndexBuild {
        blocks,
        txs,
        frames,
        txs_indexed: n_tx,
        heights_ok: c_heights_ok,
        skipped_frames: 0,
        fork_adopted: 0,
        torn_tail: if c_torn == u64::MAX { None } else { Some(c_torn) },
    }))
}

pub fn save_caches(
    dir: &Path,
    build: &IndexBuild,
    blk_len: u64,
    oracle: Option<&BuildOracle>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    let tip = oracle
        .map(|o| o.tip)
        .unwrap_or_else(|| build.blocks.tip_height().unwrap_or(0));
    write_blocks_cache(
        &dir.join("blockidx.bin"),
        &build.blocks,
        blk_len,
        build.heights_ok,
        oracle,
        build.torn_tail,
    )?;
    write_tx_cache(&dir.join("txidx.bin"), &build.txs, blk_len, tip)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Serving store
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum StoreStatus {
    Unconfigured,
    Building {
        frames_done: u64,
    },
    Ready {
        frames: usize,
        txs_indexed: usize,
        heights_ok: bool,
        threaded_tip: Option<u64>,
        build_secs: u64,
    },
    Failed(String),
}

pub struct BlockStore {
    pub path: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    state: RwLock<Option<Arc<StoreInner>>>,
    pub progress_frames: AtomicU64,
    pub started: Instant,
    failure: RwLock<Option<String>>,
    /// Bodies accepted while the index build is still running: written to the
    /// appendix in arrival order once the store is ready. Without this, blocks
    /// accepted during a cold build (fresh-node onboarding) were lost to both
    /// the index and the file — the wallet live view could never seed and the
    /// ops surface (staking/smsg/broadcast) stayed down forever.
    pending: std::sync::Mutex<Vec<(Vec<u8>, Option<u64>)>>,
    /// header hashes of the buffered bodies — O(1) duplicate detection. The
    /// old linear scan re-hashed EVERY buffered body per append (Hash9 ≈
    /// 30µs each), so a restart-time rebuild buffer made appends crawl at
    /// ~11 blk/s and degrade linearly (measured live on DNS3).
    pending_keys: std::sync::Mutex<std::collections::HashSet<Hash>>,
    /// Chain-slot overlay: height → the block hash the LIVE chain holds at
    /// that height (see `set_chain_slot`). Height lookups prefer this over
    /// the frozen build-time height map and the first-arrival appendix scan:
    /// a raced body appended BEFORE our own chain refilled a slot must never
    /// shadow the chain's choice — that shadow made the heavy staker serve a
    /// body its own getblockhash disagreed with (fleet item 18, the DNS2
    /// stale-slot-serve fork generator). The follow loop seeds the whole
    /// tipstate window on boot, notes every tip advance, and drops slots
    /// above the fork point on rollback; without a live chain registered
    /// (read-only consumers: GUI explorer, tx prevout lookup) the legacy
    /// resolution is untouched.
    chain_slots: std::sync::RwLock<std::collections::HashMap<u64, Hash>>,
    /// Highest registered slot (None until the follow loop seeds one). No
    /// height above it is served from ANY resolution — bodies accepted above
    /// our tip (raced pushes, gap fills for a lost race) must not be handed
    /// to peers as our chain.
    chain_tip: std::sync::Mutex<Option<u64>>,
    /// Heights where the legacy resolution (height map / appendix) disagreed
    /// with the overlay — the stale-slot audit counter. Every divergent
    /// lookup counts; the LOG fires once per height (see `stale_slots_seen`).
    pub stale_slot_hits: std::sync::atomic::AtomicU64,
    stale_slots_seen: std::sync::Mutex<std::collections::HashSet<u64>>,
}

pub struct StoreInner {
    pub blocks: BlockIndex,
    pub txs: TxIndex,
    pub heights_ok: bool,
    file: File,
    pub build_secs: u64,
    /// Frames appended after the scan (gap recovery). Lookups consult the
    /// main sorted indexes first, then the appendix — appends never re-sort
    /// the 70MB vectors. A restart re-scans the grown file (blk_len cache
    /// invalidation) and folds the appendix into the main index.
    appendix: std::sync::RwLock<Vec<AppendedFrame>>,
    /// key8 → appendix index (FIRST arrival wins, matching the linear scan
    /// this accelerates). The from-zero append path does one hash-dedup
    /// lookup per accepted block; a linear scan over a million-frame
    /// appendix made appends degrade linearly (O(n²) over the walk).
    appendix_by_key: std::sync::RwLock<std::collections::HashMap<u64, usize>>,
    /// accepted-but-not-yet-fsynced frame count (fsync is amortized: every
    /// SYNC_EVERY appends — process-crash durability comes from the written
    /// page cache; a host crash may lose the tail, same envelope as the
    /// buffered-append path)
    unsynced: std::sync::atomic::AtomicU64,
    /// false when the blocks file could not be opened writable (perm/ro fs) —
    /// append_block errors cleanly instead of writing
    writable: std::sync::atomic::AtomicBool,
    /// Offset of a torn final frame found by the scan (or restored from the
    /// cache). `append_block` cuts the partial frame off before writing:
    /// appending behind those bytes would leave a valid frame after garbage,
    /// which every later scan reads as MID-FILE corruption (a hard error that
    /// fails the whole index build). Cleared once the cut is made.
    torn_tail: std::sync::Mutex<Option<u64>>,
    /// Serializes the truncate-then-append sequence between concurrent
    /// appenders (the chain lock upstream usually does, but the store must not
    /// depend on that for its own file integrity).
    append_lock: std::sync::Mutex<()>,
}

#[derive(Clone, Debug)]
struct AppendedFrame {
    key: u64, // key8 of the block hash
    off: u64,
    seq: u32,
    height: Option<u64>,
    tx_keys: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct BlockHit {
    pub block: Block,
    pub hash: Hash,
    pub off: u64,
    pub body_len: usize,
    pub height: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct TxHit {
    pub tx: Tx,
    pub txid: Hash,
    pub block_hash: Hash,
    pub block_height: Option<u64>,
    /// nTime of the CONTAINING BLOCK — the daemon's verbose "time" field is
    /// the block time, not the tx's own nTime (tx time rides as "tx_time")
    pub block_time: u32,
}

/// Positional exact read, portable: unix FileExt.read_exact_at on Linux,
/// Windows FileExt.seek_read loop elsewhere. Positional — safe from
/// concurrent request threads (does not disturb the shared file cursor).
#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, off)
}

#[cfg(not(unix))]
fn read_exact_at(file: &std::fs::File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < buf.len() {
        let n = file.seek_read(&mut buf[done..], off + done as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read_exact_at: eof",
            ));
        }
        done += n;
    }
    Ok(())
}

/// (2026-11-22) positional bounded read for the debris-resync scan: fills up
/// to buf.len() bytes, fewer near EOF.
fn read_at_most(file: &std::fs::File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = file.read_at(&mut buf[filled..], off + filled as u64)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        Ok(filled)
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], off + done as u64)?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }
}

/// Resync events are logged once per corrupt offset (the seed retries the
/// same offset every few seconds — unbounded logging would spam node.log).
fn log_resync(from: u64, to: u64) {
    static LOGGED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    let mut g = match LOGGED.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if !g.contains(&from) {
        g.push(from);
        eprintln!(
            "blockstore: resync — skipped debris at offset {from}, resumed at frame {to}"
        );
    }
}

impl BlockStore {
    pub fn new(path: Option<PathBuf>, cache_dir: Option<PathBuf>) -> Arc<BlockStore> {
        Arc::new(BlockStore {
            path,
            cache_dir,
            state: RwLock::new(None),
            progress_frames: AtomicU64::new(0),
            started: Instant::now(),
            failure: RwLock::new(None),
            pending: std::sync::Mutex::new(Vec::new()),
            pending_keys: std::sync::Mutex::new(std::collections::HashSet::new()),
            chain_slots: std::sync::RwLock::new(std::collections::HashMap::new()),
            chain_tip: std::sync::Mutex::new(None),
            stale_slot_hits: std::sync::atomic::AtomicU64::new(0),
            stale_slots_seen: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// Record the live chain's block at `height`. Called by the follow loop
    /// at every tip advance and once per window entry on boot (seeded from
    /// the tipstate = the threaded lineage). After this, height lookups
    /// serve exactly this body: the height map and the first-arrival
    /// appendix can no longer shadow it.
    pub fn set_chain_slot(&self, height: u64, hash: Hash) {
        self.chain_slots.write().unwrap().insert(height, hash);
        let mut tip = self.chain_tip.lock().unwrap();
        match *tip {
            Some(t) if height <= t => {}
            _ => *tip = Some(height),
        }
    }

    /// Rollback: drop slots ABOVE `height` so the abandoned branch's heights
    /// serve nothing at all until the adopted branch re-fills them (the
    /// chain-tip guard makes even the legacy map inert for those heights).
    pub fn drop_chain_slots_above(&self, height: u64) {
        self.chain_slots.write().unwrap().retain(|h, _| *h <= height);
        let mut tip = self.chain_tip.lock().unwrap();
        if let Some(t) = *tip {
            if t > height {
                *tip = Some(height);
            }
        }
        drop(tip);
        // (item 44) ...and the abandoned branch's APPENDIX claims go with its
        // slots. An accepted-at-the-time block keeps its frame, but once the
        // live window slides past its height the appendix is the only thing
        // left answering getblockhash(h) — with the abandoned block.
        if let Some(inner) = self.inner() {
            let mut retired = 0usize;
            {
                let mut a = inner.appendix.write().unwrap();
                for f in a.iter_mut() {
                    if f.height.map_or(false, |h| h > height) {
                        f.height = None;
                        retired += 1;
                    }
                }
            }
            if retired > 0 {
                eprintln!(
                    "[blockidx] rollback: retired {retired} appendix height claim(s) above h{height}"
                );
            }
        }
    }

    /// Window slide: drop slots BELOW `height` (history below the window
    /// base is served from the frozen build-time map; the overlay tail stays
    /// bounded over long runs).
    pub fn drop_chain_slots_below(&self, height: u64) {
        self.chain_slots.write().unwrap().retain(|h, _| *h >= height);
    }

    /// Registered chain tip (None = no live chain seeded; legacy semantics).
    pub fn chain_slot_tip(&self) -> Option<u64> {
        *self.chain_tip.lock().unwrap()
    }

    pub fn chain_slots_len(&self) -> usize {
        self.chain_slots.read().unwrap().len()
    }

    pub fn stale_slot_hits(&self) -> u64 {
        self.stale_slot_hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Cache-ONLY load: swap in the on-disk cache when it matches the blocks
    /// file, otherwise leave the store unready. Never builds, never writes —
    /// a read-only consumer (tx prevout lookup) must not disturb an index the
    /// live node owns. Returns true when the cache was adopted.
    pub fn load_cache_only(&self) -> bool {
        let (Some(path), Some(dir)) = (self.path.clone(), self.cache_dir.clone()) else {
            return false;
        };
        let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else {
            return false;
        };
        let Ok(Some(b)) = load_caches(&dir, len, None) else {
            return false;
        };
        // READ-ONLY handle: a lookup consumer must never append to the store
        let Ok(file) = std::fs::File::open(&path) else {
            return false;
        };
        let inner = StoreInner {
            blocks: b.blocks,
            txs: b.txs,
            heights_ok: b.heights_ok,
            file,
            build_secs: 0,
            appendix: std::sync::RwLock::new(Vec::new()),
            appendix_by_key: std::sync::RwLock::new(std::collections::HashMap::new()),
            unsynced: std::sync::atomic::AtomicU64::new(0),
            writable: std::sync::atomic::AtomicBool::new(false),
            // read-only consumer: it never appends, so it never heals
            torn_tail: std::sync::Mutex::new(b.torn_tail),
            append_lock: std::sync::Mutex::new(()),
        };
        if let Ok(mut g) = self.state.write() {
            *g = Some(Arc::new(inner));
            true
        } else {
            false
        }
    }

    /// Background build: cache-first, fall back to a full scan, then swap the
    /// ready store in. Failures are recorded, never panicked into callers.
    pub fn spawn_build(self: &Arc<Self>, oracle: Option<BuildOracle>) {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("blockidx".into())
            .spawn(move || me.build(oracle))
            .expect("spawn blockidx thread");
    }

    fn build(self: Arc<Self>, oracle: Option<BuildOracle>) {
        let started = Instant::now();
        let outcome: Result<IndexBuild, String> = (|| {
            let path = self.path.clone().ok_or("no blocks file configured")?;
            let blk_len = std::fs::metadata(&path)
                .map_err(|e| format!("stat {}: {}", path.display(), e))?
                .len();
            if let Some(dir) = &self.cache_dir {
                if let Some(cached) = load_caches(dir, blk_len, oracle.as_ref())? {
                    // sentinel: MAX marks "done" for the status probe
                    self.progress_frames.store(u64::MAX, Ordering::Relaxed);
                    return Ok(cached);
                }
            }
            let progress = &self.progress_frames;
            let build = build_index(&path, oracle.as_ref(), &|n| {
                progress.store(n as u64, Ordering::Relaxed);
            })?;
            if let Some(cut) = build.torn_tail {
                eprintln!(
                    "[blockidx] torn tail at offset {} in {}: indexing the {} complete frame(s) \
                     before it; the partial frame is cut before the next append",
                    cut,
                    path.display(),
                    build.frames
                );
            }
            if let Some(dir) = &self.cache_dir {
                // cache failure is non-fatal — the in-memory index still serves
                if let Err(e) = save_caches(dir, &build, blk_len, oracle.as_ref()) {
                    eprintln!("[blockidx] cache save failed: {} (serving from RAM)", e);
                }
            }
            Ok(build)
        })();
        match outcome {
            Ok(b) => {
                // .write(true), NOT .append(true): the torn-tail heal in
                // append_block truncates via set_len, and on Windows an
                // append-only handle (FILE_APPEND_DATA — what Rust's
                // .append(true) requests) is refused with ERROR_ACCESS_DENIED
                // (os error 5): SetEndOfFile needs FILE_WRITE_DATA. Linux
                // maps read+append to O_RDWR, so the defect was Windows-only
                // (CI windows-build: "append: truncate torn tail: Access is
                // denied"). Every write site seeks End explicitly under
                // append_lock, so O_APPEND's atomic-append semantics are not
                // load-bearing here.
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(self.path.as_ref().unwrap())
                    .or_else(|_| File::open(self.path.as_ref().unwrap()))
                    .map_err(|e| format!("open blocks file: {}", e));
                match file {
                    Ok(file) => {
                        // probe writability with a throwaway WRITE handle —
                        // the access append actually needs (set_len included)
                        let writable = std::fs::OpenOptions::new()
                            .write(true)
                            .open(self.path.as_ref().unwrap())
                            .is_ok();
                        let fork_note = if b.fork_adopted > 0 {
                            format!(", fork re-threaded {} frames", b.fork_adopted)
                        } else {
                            String::new()
                        };
                        eprintln!(
                            "[blockidx] ready: {} frames, {} txs, heights_ok={}, \
                             threaded_tip={:?}, writable={}, built in {}s{}",
                            b.frames,
                            b.txs_indexed,
                            b.heights_ok,
                            b.blocks.tip_height(),
                            writable,
                            started.elapsed().as_secs(),
                            fork_note
                        );
                        let inner = StoreInner {
                            blocks: b.blocks,
                            txs: b.txs,
                            heights_ok: b.heights_ok,
                            file,
                            build_secs: started.elapsed().as_secs(),
                            appendix: std::sync::RwLock::new(Vec::new()),
                            appendix_by_key: std::sync::RwLock::new(
                                std::collections::HashMap::new(),
                            ),
                            unsynced: std::sync::atomic::AtomicU64::new(0),
                            writable: std::sync::atomic::AtomicBool::new(writable),
                            torn_tail: std::sync::Mutex::new(b.torn_tail),
                            append_lock: std::sync::Mutex::new(()),
                        };
                        *self.state.write().unwrap() = Some(Arc::new(inner));
                    }
                    Err(e) => {
                        *self.failure.write().unwrap() = Some(e);
                    }
                }
            }
            Err(e) => {
                *self.failure.write().unwrap() = Some(e);
            }
        }
    }

    pub fn status(&self) -> StoreStatus {
        if let Some(inner) = self.state.read().unwrap().clone() {
            return StoreStatus::Ready {
                frames: inner.blocks.frames,
                txs_indexed: inner.txs.txs,
                heights_ok: inner.heights_ok,
                threaded_tip: inner.blocks.tip_height(),
                build_secs: inner.build_secs,
            };
        }
        if let Some(f) = self.failure.read().unwrap().clone() {
            return StoreStatus::Failed(f);
        }
        if self.path.is_none() {
            return StoreStatus::Unconfigured;
        }
        StoreStatus::Building {
            frames_done: self.progress_frames.load(Ordering::Relaxed),
        }
    }

    fn inner(&self) -> Option<Arc<StoreInner>> {
        self.state.read().unwrap().clone()
    }

    /// Read the frame body at a frame offset. Positional read — safe from
    /// concurrent request threads.
    ///
    /// (2026-11-22) DEBRIS RESYNC: force-killed appends can leave a torn
    /// frame mid-file (the next boot appends after the tear instead of
    /// truncating it). A positional read of that region returns garbage and
    /// — with the full-history wallet seed — bricked the balance forever
    /// (sami-pc 227857036 / sami-laptop 275471636, both live-caught). On a
    /// bad header, scan FORWARD up to 32 MiB for a plausible frame header
    /// (correct magic, sane length) and serve THAT; the callers verify the
    /// body against the height map / chain hashes anyway, so a wrong frame
    /// can never be presented as a block — a miss is a skipped height, never
    /// silent wrongness. Every resync is logged.
    fn read_frame(inner: &StoreInner, off: u64) -> Result<Vec<u8>, String> {
        match Self::read_frame_strict(inner, off) {
            Ok(b) => Ok(b),
            Err(bad) => {
                const RESYNC_LIMIT: u64 = 32 << 20;
                const CHUNK: u64 = 1 << 20;
                let mut scan = off + 1;
                let end = off + 1 + RESYNC_LIMIT;
                let file_len = inner.file.metadata().map(|m| m.len()).unwrap_or(0);
                while scan + 8 < end && scan + 8 < file_len {
                    let mut buf = vec![0u8; CHUNK as usize];
                    let n = read_at_most(&inner.file, &mut buf, scan)
                        .map_err(|e| format!("resync read at {}: {}", scan, e))?;
                    if n < 8 {
                        break;
                    }
                    buf.truncate(n);
                    let mut i = 0usize;
                    while i + 8 <= buf.len() {
                        let magic = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
                        let len = u32::from_le_bytes(buf[i + 4..i + 8].try_into().unwrap()) as usize;
                        if magic == FRAME_MAGIC && len > 0 && len <= MAX_FRAME {
                            let found = scan + i as u64;
                            if let Ok(body) = Self::read_frame_strict(inner, found) {
                                log_resync(off, found);
                                return Ok(body);
                            }
                        }
                        i += 1;
                    }
                    scan += CHUNK - 8; // overlap so a header straddling a chunk edge is found
                }
                Err(bad)
            }
        }
    }

    fn read_frame_strict(inner: &StoreInner, off: u64) -> Result<Vec<u8>, String> {
        let mut hdr = [0u8; 8];
        read_exact_at(&inner.file, &mut hdr, off)
            .map_err(|e| format!("frame header read at {}: {}", off, e))?;
        let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if magic != FRAME_MAGIC || len == 0 || len > MAX_FRAME {
            return Err(format!(
                "bad frame header at offset {} (magic {magic:08x} len {len})",
                off
            ));
        }
        let mut body = vec![0u8; len];
        read_exact_at(&inner.file, &mut body, off + 8)
            .map_err(|e| format!("frame body read at {}: {}", off, e))?;
        Ok(body)
    }

    fn parse_frame(body: &[u8]) -> Result<Block, String> {
        let mut r = Reader::new(body);
        let b = Block::read(&mut r)?;
        r.expect_done()?;
        Ok(b)
    }

    /// Locate a block by its full internal hash: candidate frames via the
    /// truncated key, full Hash9 verification on the bytes. Post-build
    /// appended frames (gap recovery) are consulted after the main index.
    /// The EXACT bytes of the frame that hashes to `hash`.
    ///
    /// `block_by_hash` decodes into a `Block` and callers that re-serialize it
    /// get a body that is not byte-identical to the chain (extra header bytes,
    /// tx re-encoding), so its hash no longer matches the block it claims to
    /// be. Anything that serves a block to another node or client must use
    /// this and hand back the stored bytes untouched.
    pub fn raw_body_by_hash(&self, hash: &Hash) -> Result<Option<Vec<u8>>, String> {
        let inner = match self.inner() {
            Some(i) => i,
            None => return Ok(None),
        };
        for e in inner.blocks.candidates_by_key(key8(hash)) {
            let body = Self::read_frame(&inner, e.off)?;
            if &header_hash(&body)? == hash {
                return Ok(Some(body));
            }
        }
        Ok(None)
    }

    /// Raw bytes for a height, chain-verified: the registered slot (the
    /// live tip's choice) wins over the height map / appendix, and a height
    /// above the chain tip serves nothing — so a raced or refilled slot can
    /// never hand back a body the node's own chain does not contain.
    /// UNREGISTERED stores keep the exact legacy semantics: the height-map
    /// frame proven by `hash_at_height`, or nothing (the old
    /// no-proof-no-serve guard — a map-miss never served an appendix body).
    pub fn raw_body_by_height(&self, h: u64) -> Result<Option<Vec<u8>>, String> {
        let inner = match self.inner() {
            Some(i) => i,
            None => return Ok(None),
        };
        if !inner.heights_ok {
            return Ok(None);
        }
        if self.chain_slot_tip().is_some() {
            return match self.height_slot(&inner, h)? {
                Some((_, _, body)) => Ok(Some(body)),
                None => Ok(None),
            };
        }
        let want = self.hash_at_height(h)?;
        let off = match inner.blocks.offset_by_height(h) {
            Some(o) => o,
            None => return Ok(None),
        };
        let body = Self::read_frame(&inner, off)?;
        let Some(w) = want else {
            // No expected hash means we cannot prove what this frame is.
            // Serving it anyway would hand out an unverified body.
            return Ok(None);
        };
        if crate::p2p::internal_to_display(&header_hash(&body)?) != w {
            return Ok(None);
        }
        Ok(Some(body))
    }

    /// (item 44) A height in the map is a CLAIM; the active chain is the
    /// chain-slot overlay. Inside the overlay's span a block is on the active
    /// chain iff it IS the slot's block at its height — a block a reorg
    /// replaced there keeps its frame but must not keep a height (and with
    /// it positive confirmations). Above the overlay, and where the sliding
    /// window has already dropped the slot, the map's answer stands.
    fn on_active_chain(&self, height: Option<u64>, hash: &Hash) -> Option<u64> {
        let h = height?;
        match self.chain_slots.read().unwrap().get(&h) {
            Some(chain_hash) if chain_hash != hash => None,
            _ => Some(h),
        }
    }

    pub fn block_by_hash(&self, hash: &Hash) -> Result<Option<BlockHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        for e in inner.blocks.candidates_by_key(key8(hash)) {
            let body = Self::read_frame(&inner, e.off)?;
            let h = header_hash(&body)?;
            if &h != hash {
                continue; // truncated-key collision — keep scanning candidates
            }
            let block = Self::parse_frame(&body)?;
            let height = self.on_active_chain(inner.blocks.height_of_seq(e.seq), &h);
            return Ok(Some(BlockHit {
                block,
                hash: h,
                off: e.off,
                body_len: body.len(),
                height,
            }));
        }
        // appendix: the by_key map is the O(1) accelerator for the linear
        // scan (first arrival wins — same semantics). The from-zero append
        // path runs this lookup once per accepted block; a linear scan over
        // a million-frame appendix degraded appends linearly (O(n²)).
        let key = key8(hash);
        let hit_idx = inner.appendix_by_key.read().unwrap().get(&key).copied();
        if let Some(idx) = hit_idx {
            let a = inner.appendix.read().unwrap();
            if idx < a.len() {
                let f = a[idx].clone();
                if f.key == key {
                    let body = Self::read_frame(&inner, f.off)?;
                    let h = header_hash(&body)?;
                    if &h == hash {
                        let block = Self::parse_frame(&body)?;
                        let height =
                            self.on_active_chain(f.height.or(inner.blocks.height_of_seq(f.seq)), &h);
                        return Ok(Some(BlockHit {
                            block,
                            hash: h,
                            off: f.off,
                            body_len: body.len(),
                            height,
                        }));
                    }
                }
            }
        }
        for f in inner.appendix.read().unwrap().iter() {
            if f.key != key8(hash) {
                continue;
            }
            let body = Self::read_frame(&inner, f.off)?;
            let h = header_hash(&body)?;
            if &h != hash {
                continue;
            }
            let block = Self::parse_frame(&body)?;
            let height = f.height.or(inner.blocks.height_of_seq(f.seq));
            return Ok(Some(BlockHit {
                block,
                hash: h,
                off: f.off,
                body_len: body.len(),
                height,
            }));
        }
        Ok(None)
    }

    /// Frame byte offset for a main-chain height (store maintenance: trim a
    /// stale tail after a reorg — see examples/store_trim.rs).
    pub fn frame_offset(&self, h: u64) -> Option<u64> {
        self.inner()
            .and_then(|inner| inner.blocks.offset_by_height(h))
    }

    /// Locate a block by main-chain height. The live chain's registered slot
    /// wins over the frozen height map and the first-arrival appendix; no
    /// height above the registered chain tip is served at all (raced bodies
    /// stored above our tip are not our chain). Unavailable when the build's
    /// chain-threading anchor failed.
    pub fn block_by_height(&self, h: u64) -> Result<Option<BlockHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        if !inner.heights_ok {
            return Ok(None);
        }
        match self.height_slot(&inner, h)? {
            Some((hash, off, body)) => {
                let block = Self::parse_frame(&body)?;
                Ok(Some(BlockHit {
                    block,
                    hash,
                    off,
                    body_len: body.len(),
                    height: Some(h),
                }))
            }
            None => Ok(None),
        }
    }

    /// One height-slot resolution: (hash, frame offset, raw body).
    ///
    /// Resolution order: (1) nothing above the registered chain tip; (2) the
    /// chain-slot overlay (the live tip's slot choice — seeded from the
    /// threaded tipstate, maintained at every advance/rollback); (3) the
    /// build-time height map; (4) the appendix scan (first arrival wins).
    /// When the overlay and the legacy resolution disagree, the CHAIN's body
    /// is fetched by hash and the divergence is counted + logged once per
    /// height — that log is the fleet's stale-slot audit (item 18: DNS2
    /// served a raced body its own getblockhash denied, freezing followers
    /// mid-batch).
    fn height_slot(
        &self,
        inner: &Arc<StoreInner>,
        h: u64,
    ) -> Result<Option<(Hash, u64, Vec<u8>)>, String> {
        if let Some(tip) = self.chain_slot_tip() {
            if h > tip {
                return Ok(None);
            }
        }
        let want = self.chain_slots.read().unwrap().get(&h).copied();
        let legacy: Option<(Hash, u64, Vec<u8>)> = match inner.blocks.offset_by_height(h) {
            Some(off) => {
                let body = Self::read_frame(inner, off)?;
                Some((header_hash(&body)?, off, body))
            }
            None => {
                // main height map ended below h — consult the appendix (a
                // gap-recovery or raced append above the threaded tip)
                let appendix = inner.appendix.read().unwrap();
                let mut found: Option<(u64, Vec<u8>)> = None;
                for f in appendix.iter() {
                    if f.height == Some(h) {
                        found = Some((f.off, Self::read_frame(inner, f.off)?));
                        break;
                    }
                }
                match found {
                    Some((off, body)) => Some((header_hash(&body)?, off, body)),
                    None => None,
                }
            }
        };
        match (want, legacy) {
            (Some(w), Some((lh, off, body))) if lh == w => Ok(Some((w, off, body))),
            (Some(w), legacy_other) => {
                // The frozen map / first-arrival appendix disagree with the
                // live chain (or hold nothing): fetch the CHAIN's body by
                // hash. Serving the legacy body here hands a peer a block
                // our own getblockhash would deny.
                self.note_stale_slot(h, legacy_other.as_ref().map(|(lh, _, _)| *lh));
                match self.slot_body_by_hash(inner, &w)? {
                    Some((off, body)) => Ok(Some((w, off, body))),
                    // chain body absent from this store: serve nothing (the
                    // caller's recent-cache fallback may still cover it)
                    None => Ok(None),
                }
            }
            (None, legacy) => Ok(legacy),
        }
    }

    /// Fetch a body by full hash: main candidates, then the appendix (the
    /// by_key map holds only the FIRST arrival for a key8 slot, so a raced
    /// body can sit there instead of ours — keep scanning).
    fn slot_body_by_hash(
        &self,
        inner: &Arc<StoreInner>,
        hash: &Hash,
    ) -> Result<Option<(u64, Vec<u8>)>, String> {
        for e in inner.blocks.candidates_by_key(key8(hash)) {
            let body = Self::read_frame(inner, e.off)?;
            if &header_hash(&body)? == hash {
                return Ok(Some((e.off, body)));
            }
        }
        let key = key8(hash);
        let appendix = inner.appendix.read().unwrap();
        let mut found: Option<(u64, Vec<u8>)> = None;
        for f in appendix.iter() {
            if f.key != key {
                continue;
            }
            let body = Self::read_frame(inner, f.off)?;
            if &header_hash(&body)? == hash {
                found = Some((f.off, body));
                break;
            }
        }
        Ok(found)
    }

    /// Count the divergence and log it once per height (the audit trail).
    fn note_stale_slot(&self, h: u64, legacy: Option<Hash>) {
        self.stale_slot_hits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fresh = {
            let mut s = self.stale_slots_seen.lock().unwrap_or_else(|e| e.into_inner());
            if s.len() > 8192 {
                // audit trail bound: a pathological store would grow the set
                // without limit — clearing it only re-logs recent heights
                s.clear();
            }
            s.insert(h)
        };
        if fresh {
            let want = self.chain_slots.read().unwrap().get(&h).copied();
            let fmt = |x: Option<Hash>| {
                x.map(|v| crate::p2p::internal_to_display(&v))
                    .unwrap_or_else(|| "nothing".into())
            };
            eprintln!(
                "[blockidx] STALE SLOT h{}: height map/appendix serves {} but the live chain slot is {} — serving the chain's body",
                h,
                fmt(legacy),
                fmt(want)
            );
        }
    }

    /// The registered chain hash at `height` (None = no slot / above the
    /// chain tip). Serving callers use it to pick the chain's OWN body out
    /// of the recent cache instead of a blind latest-arrival lookup.
    pub fn chain_slot_hash(&self, h: u64) -> Option<Hash> {
        if let Some(tip) = self.chain_slot_tip() {
            if h > tip {
                return None;
            }
            return self.chain_slots.read().unwrap().get(&h).copied();
        }
        None
    }

    /// The ORACLE-canonical hash at a height: only the build-time height_off
    /// map answers (chain slots and runtime appendix arrivals never do). This
    /// is the ancestry proof for below-window serving anchors — a runtime-
    /// appended stale fork at a gap height (or anywhere) can never pass it,
    /// because height_off is frozen at build time and only the build-verified
    /// frames occupy it.
    pub fn canonical_hash_at_height(&self, h: u64) -> Option<Hash> {
        let inner = self.inner()?;
        if !inner.heights_ok {
            return None;
        }
        let off = inner.blocks.offset_by_height(h)?;
        if off == HEIGHT_GAP {
            return None;
        }
        // offsets are FRAME starts (magic u32 + len u32, then the body):
        // skip the 8-byte frame prefix before hashing the header
        let mut hdr = [0u8; 80];
        read_exact_at(&inner.file, &mut hdr, off + 8).ok()?;
        header_hash(&hdr).ok()
    }

    /// The main-chain block hash at a height (display hex). The live chain's
    /// registered slot answers first — getblockhash must never contradict
    /// what serving hands out at the same height; no answer above the chain
    /// tip.
    pub fn hash_at_height(&self, h: u64) -> Result<Option<String>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        if !inner.heights_ok {
            return Ok(None);
        }
        if let Some(tip) = self.chain_slot_tip() {
            if h > tip {
                return Ok(None);
            }
            if let Some(w) = self.chain_slots.read().unwrap().get(&h) {
                return Ok(Some(crate::p2p::internal_to_display(w)));
            }
        }
        let off = match inner.blocks.offset_by_height(h) {
            Some(o) => o,
            None => {
                for f in inner.appendix.read().unwrap().iter() {
                    if f.height == Some(h) {
                        let body = Self::read_frame(&inner, f.off)?;
                        return Ok(Some(crate::p2p::internal_to_display(&header_hash(&body)?)));
                    }
                }
                return Ok(None);
            }
        };
        let mut hdr = [0u8; 8];
        let mut head80 = [0u8; 80];
        read_exact_at(&inner.file, &mut hdr, off)
            .map_err(|e| format!("header read at {}: {}", off, e))?;
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if len < 80 {
            return Err(format!("degenerate frame at offset {}", off));
        }
        read_exact_at(&inner.file, &mut head80, off + 8)
            .map_err(|e| format!("header read at {}: {}", off + 8, e))?;
        Ok(Some(crate::p2p::internal_to_display(&hash9(&head80))))
    }

    /// Locate a transaction by its full internal txid: candidate frames via
    /// the truncated key, then full sha256d verification inside each frame.
    pub fn tx_by_id(&self, txid: &Hash) -> Result<Option<TxHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        let key = key8(txid);
        // A txid can appear in several frames across reorg seams (identical
        // bytes); each distinct candidate frame is checked once.
        let mut seen_off: Option<u64> = None;
        for e in inner.txs.candidates_by_key(key) {
            if seen_off == Some(e.off) {
                continue;
            }
            seen_off = Some(e.off);
            let body = Self::read_frame(&inner, e.off)?;
            let bhash = header_hash(&body)?;
            // block nTime sits at header bytes 68..72 (version 4 + prev 32 +
            // merkle 32) — needed for the daemon-parity "time" field
            let block_time = if body.len() >= 72 {
                u32::from_le_bytes(body[68..72].try_into().unwrap())
            } else {
                0
            };
            let mut found: Option<Tx> = None;
            walk_tx_ranges(&body, &mut |start, end| {
                if found.is_some() {
                    return Ok(());
                }
                let id = sha256d(&body[start..end]); // SerializeHash = double sha256
                if &id == txid {
                    let mut r = Reader::new(&body[start..end]);
                    let tx = Tx::read(&mut r)?;
                    r.expect_done()?;
                    found = Some(tx);
                }
                Ok(())
            })?;
            if let Some(tx) = found {
                // height of the containing frame: find this frame's own
                // BlockEntry by (block-key, offset) and read its seq height,
                // falling back to the appendix (gap-recovery appends)
                let block_height = inner
                    .blocks
                    .candidates_by_key(key8(&bhash))
                    .iter()
                    .find(|b| b.off == e.off)
                    .and_then(|b| inner.blocks.height_of_seq(b.seq))
                    .or_else(|| {
                        inner
                            .appendix
                            .read()
                            .unwrap()
                            .iter()
                            .find(|f| f.off == e.off)
                            .and_then(|f| f.height)
                    });
                return Ok(Some(TxHit {
                    tx,
                    txid: *txid,
                    block_hash: bhash,
                    block_height,
                    block_time,
                }));
            }
        }
        // appendix pass: txids of gap-recovery appends are not in the main
        // tx index (never re-sorted)
        for f in inner.appendix.read().unwrap().iter() {
            if !f.tx_keys.contains(&key) {
                continue;
            }
            let body = Self::read_frame(&inner, f.off)?;
            let bhash = header_hash(&body)?;
            let block_time = if body.len() >= 72 {
                u32::from_le_bytes(body[68..72].try_into().unwrap())
            } else {
                0
            };
            let mut found: Option<Tx> = None;
            walk_tx_ranges(&body, &mut |start, end| {
                if found.is_some() {
                    return Ok(());
                }
                let id = sha256d(&body[start..end]);
                if &id == txid {
                    let mut r = Reader::new(&body[start..end]);
                    let tx = Tx::read(&mut r)?;
                    r.expect_done()?;
                    found = Some(tx);
                }
                Ok(())
            })?;
            if let Some(tx) = found {
                return Ok(Some(TxHit {
                    tx,
                    txid: *txid,
                    block_hash: bhash,
                    block_height: f.height,
                    block_time,
                }));
            }
        }
        Ok(None)
    }

    /// (item 41) EVERY distinct frame that contains `txid` — the
    /// all-candidates form of `tx_by_id`. `tx_by_id` returns the FIRST
    /// candidate and stops; across reorg seams a txid can live in several
    /// frames (an abandoned-branch frame can sit EARLIER in insertion
    /// order than the canonical one), so a guard that only sees the first
    /// hit can miss a canonical occurrence entirely. Duplicate-tx
    /// guarding must inspect every candidate: main-index frames (insertion
    /// order, distinct offsets) first, then appendix frames. Each entry
    /// carries the containing frame's hash and threaded height so the
    /// caller can decide canonicality per frame.
    pub fn tx_frames_by_id(&self, txid: &Hash) -> Result<Vec<TxHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        let key = key8(txid);
        let mut out: Vec<TxHit> = Vec::new();
        let mut seen_off: Option<u64> = None;
        for e in inner.txs.candidates_by_key(key) {
            if seen_off == Some(e.off) {
                continue;
            }
            seen_off = Some(e.off);
            let body = Self::read_frame(&inner, e.off)?;
            let bhash = header_hash(&body)?;
            let block_time = if body.len() >= 72 {
                u32::from_le_bytes(body[68..72].try_into().unwrap())
            } else {
                0
            };
            let mut found: Option<Tx> = None;
            walk_tx_ranges(&body, &mut |start, end| {
                if found.is_some() {
                    return Ok(());
                }
                let id = sha256d(&body[start..end]);
                if &id == txid {
                    let mut r = Reader::new(&body[start..end]);
                    let tx = Tx::read(&mut r)?;
                    r.expect_done()?;
                    found = Some(tx);
                }
                Ok(())
            })?;
            if let Some(tx) = found {
                let block_height = inner
                    .blocks
                    .candidates_by_key(key8(&bhash))
                    .iter()
                    .find(|b| b.off == e.off)
                    .and_then(|b| inner.blocks.height_of_seq(b.seq))
                    .or_else(|| {
                        inner
                            .appendix
                            .read()
                            .unwrap()
                            .iter()
                            .find(|f| f.off == e.off)
                            .and_then(|f| f.height)
                    });
                out.push(TxHit {
                    tx,
                    txid: *txid,
                    block_hash: bhash,
                    block_height,
                    block_time,
                });
            }
        }
        for f in inner.appendix.read().unwrap().iter() {
            if !f.tx_keys.contains(&key) {
                continue;
            }
            let body = Self::read_frame(&inner, f.off)?;
            let bhash = header_hash(&body)?;
            let block_time = if body.len() >= 72 {
                u32::from_le_bytes(body[68..72].try_into().unwrap())
            } else {
                0
            };
            let mut found: Option<Tx> = None;
            walk_tx_ranges(&body, &mut |start, end| {
                if found.is_some() {
                    return Ok(());
                }
                let id = sha256d(&body[start..end]);
                if &id == txid {
                    let mut r = Reader::new(&body[start..end]);
                    let tx = Tx::read(&mut r)?;
                    r.expect_done()?;
                    found = Some(tx);
                }
                Ok(())
            })?;
            if let Some(tx) = found {
                out.push(TxHit {
                    tx,
                    txid: *txid,
                    block_hash: bhash,
                    block_height: f.height,
                    block_time,
                });
            }
        }
        Ok(out)
    }

    /// Height the main height map reached (= highest contiguous threaded
    /// height from the scan), extended by any appendix frames carrying a
    /// height — the "where do disk bodies end" oracle for gap recovery.
    pub fn threaded_tip(&self) -> Option<u64> {
        let inner = self.inner()?;
        let main = inner.blocks.tip_height();
        let app = inner
            .appendix
            .read()
            .unwrap()
            .iter()
            .filter_map(|f| f.height)
            .max();
        match (main, app) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Append a raw block body (header + vtx + block_sig, NO frame header) to
    /// the blocks file and the appendix index. Idempotent on hash: re-appends
    /// return Ok with the existing hash. `height` assigns the height-thread
    /// entry for gap fills above the threaded tip.
    pub fn append_block(&self, body: &[u8], height: Option<u64>) -> Result<Hash, String> {
        if body.len() < 80 {
            return Err(format!("append: body too short ({})", body.len()));
        }
        let inner = match self.inner() {
            Some(i) => i,
            None => {
                // Index still building: buffer the body, flush on ready. The
                // frame is durable in RAM only until then — same assumption
                // the recent-blocks cache makes — but NOTHING is lost to the
                // disk store once the build lands.
                let hash = header_hash(body)?;
                let mut p = self.pending.lock().unwrap();
                let mut keys = self.pending_keys.lock().unwrap();
                if !keys.insert(hash) {
                    return Ok(hash); // already buffered
                }
                p.push((body.to_vec(), height));
                return Ok(hash);
            }
        };
        // arrival-order guarantee: anything buffered during the build lands
        // before the first post-ready append
        self.flush_pending(&inner);
        let hash = header_hash(body)?;
        if self.block_by_hash(&hash)?.is_some() {
            // already on disk. Its height CLAIM though follows the chain: a
            // frame a rollback de-heighted that is accepted again must claim
            // its height again — append is the accept path's only chance to
            // say so.
            self.restate_appendix_height(&inner, &hash, height);
            return Ok(hash); // already on disk — nothing new to store
        }
        if !inner.writable.load(Ordering::SeqCst) {
            return Err(
                "blocks file is not writable (append needs a tri7-owned blk file)".into(),
            );
        }
        let f = &inner.file;
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        use std::io::Seek;
        let mut fh = f
            .try_clone()
            .map_err(|e| format!("append: clone handle: {}", e))?;
        // TRUNCATE-THEN-APPEND under one lock: a scan that found a torn final
        // frame (a power cut between the amortized fsyncs) leaves the file
        // ending mid-frame. Writing the new frame BEHIND those bytes would put
        // a valid frame after garbage, and every later scan then fails the
        // whole index build on mid-file corruption. Cut the partial frame
        // first: the frames before it are already indexed, so nothing is lost.
        let _append_guard = inner
            .append_lock
            .lock()
            .map_err(|_| "append: append lock poisoned".to_string())?;
        let torn_cut = inner.torn_tail.lock().map(|c| *c).unwrap_or(None);
        if let Some(cut) = torn_cut {
                let cur = fh
                    .metadata()
                    .map_err(|e| format!("append: stat: {}", e))?
                    .len();
                if cur > cut {
                    eprintln!(
                        "[blockidx] cutting {} byte(s) of torn tail at offset {} before append",
                        cur - cut,
                        cut
                    );
                    fh.set_len(cut)
                        .map_err(|e| format!("append: truncate torn tail: {}", e))?;
                }
                if let Ok(mut t) = inner.torn_tail.lock() {
                    *t = None;
                }
        }
        let off = fh
            .metadata()
            .map_err(|e| format!("append: stat: {}", e))?
            .len();
        fh.seek(std::io::SeekFrom::End(0))
            .map_err(|e| format!("append: seek: {}", e))?;
        fh.write_all(&frame)
            .map_err(|e| format!("append: write: {}", e))?;
        // amortized fsync: write_all already landed the frame in the page
        // cache (process-crash durable); sync every SYNC_EVERY appends so a
        // host crash loses at most the tail. The old per-append sync_all
        // flushed the disk once per BLOCK and capped from-zero throughput
        // well under the fetch rate.
        const SYNC_EVERY: u64 = 256;
        let un = inner
            .unsynced
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if un % SYNC_EVERY == 0 {
            fh.sync_all().map_err(|e| format!("append: sync: {}", e))?;
        }
        // index the frame
        let mut tx_keys = Vec::new();
        walk_tx_ranges(body, &mut |start, end| {
            tx_keys.push(key8(&sha256d(&body[start..end])));
            Ok(())
        })?;
        let seq = inner.blocks.frames as u32 + {
            let a = inner.appendix.read().unwrap();
            a.len() as u32
        };
        let akey = key8(&hash);
        {
            let mut a = inner.appendix.write().unwrap();
            // first arrival wins (matches the linear-scan lookup semantics)
            inner
                .appendix_by_key
                .write()
                .unwrap()
                .entry(akey)
                .or_insert(a.len());
            a.push(AppendedFrame {
                key: akey,
                off,
                seq,
                height,
                tx_keys,
            });
        }
        Ok(hash)
    }

    /// (item 44) Append is the accept path's only chance to state a block's
    /// height, and a rollback retires the claims of the abandoned branch.
    /// Re-accepting a frame already on disk must therefore re-state its
    /// claim, or a canonical height would go unserved once the live window
    /// slides past it. Frames in the main map are untouched (their heights
    /// come from the build).
    fn restate_appendix_height(&self, inner: &Arc<StoreInner>, hash: &Hash, height: Option<u64>) {
        let Some(h) = height else { return };
        let akey = key8(hash);
        let idx = inner.appendix_by_key.read().unwrap().get(&akey).copied();
        let Some(idx) = idx else { return };
        let mut a = inner.appendix.write().unwrap();
        let Some(f) = a.get_mut(idx) else { return };
        if f.height == Some(h) {
            return;
        }
        // the key is truncated: only a frame whose FULL hash matches is ours
        match Self::read_frame(inner, f.off).and_then(|b| header_hash(&b)) {
            Ok(fh) if fh == *hash => f.height = Some(h),
            _ => {}
        }
    }

    /// Drain bodies buffered during the index build into the now-ready store
    /// (FIFO — same append path as direct appends, dedup via block_by_hash).
    fn flush_pending(&self, inner: &Arc<StoreInner>) {
        let drained: Vec<(Vec<u8>, Option<u64>)> = {
            let mut p = self.pending.lock().unwrap();
            let mut keys = self.pending_keys.lock().unwrap();
            keys.clear();
            std::mem::take(&mut *p)
        };
        for (body, height) in drained {
            if let Err(e) = self.append_ready(&inner, &body, height) {
                eprintln!("[blockidx] pending append flush failed: {}", e);
            }
        }
    }

    /// The ready-store append path (extracted from append_block).
    fn append_ready(
        &self,
        inner: &Arc<StoreInner>,
        body: &[u8],
        height: Option<u64>,
    ) -> Result<Hash, String> {
        let hash = header_hash(body)?;
        if self.block_by_hash(&hash)?.is_some() {
            // already on disk. Its height CLAIM though follows the chain: a
            // frame a rollback de-heighted that is accepted again must claim
            // its height again — append is the accept path's only chance to
            // say so.
            self.restate_appendix_height(&inner, &hash, height);
            return Ok(hash); // already on disk — nothing new to store
        }
        if !inner.writable.load(Ordering::SeqCst) {
            return Err(
                "blocks file is not writable (append needs a tri7-owned blk file)".into(),
            );
        }
        let f = &inner.file;
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        use std::io::Seek;
        let mut fh = f
            .try_clone()
            .map_err(|e| format!("append: clone handle: {}", e))?;
        // TRUNCATE-THEN-APPEND under one lock: a scan that found a torn final
        // frame (a power cut between the amortized fsyncs) leaves the file
        // ending mid-frame. Writing the new frame BEHIND those bytes would put
        // a valid frame after garbage, and every later scan then fails the
        // whole index build on mid-file corruption. Cut the partial frame
        // first: the frames before it are already indexed, so nothing is lost.
        let _append_guard = inner
            .append_lock
            .lock()
            .map_err(|_| "append: append lock poisoned".to_string())?;
        let torn_cut = inner.torn_tail.lock().map(|c| *c).unwrap_or(None);
        if let Some(cut) = torn_cut {
                let cur = fh
                    .metadata()
                    .map_err(|e| format!("append: stat: {}", e))?
                    .len();
                if cur > cut {
                    eprintln!(
                        "[blockidx] cutting {} byte(s) of torn tail at offset {} before append",
                        cur - cut,
                        cut
                    );
                    fh.set_len(cut)
                        .map_err(|e| format!("append: truncate torn tail: {}", e))?;
                }
                if let Ok(mut t) = inner.torn_tail.lock() {
                    *t = None;
                }
        }
        let off = fh
            .metadata()
            .map_err(|e| format!("append: stat: {}", e))?
            .len();
        fh.seek(std::io::SeekFrom::End(0))
            .map_err(|e| format!("append: seek: {}", e))?;
        fh.write_all(&frame)
            .map_err(|e| format!("append: write: {}", e))?;
        // amortized fsync: write_all already landed the frame in the page
        // cache (process-crash durable); sync every SYNC_EVERY appends so a
        // host crash loses at most the tail. The old per-append sync_all
        // flushed the disk once per BLOCK and capped from-zero throughput
        // well under the fetch rate.
        const SYNC_EVERY: u64 = 256;
        let un = inner
            .unsynced
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if un % SYNC_EVERY == 0 {
            fh.sync_all().map_err(|e| format!("append: sync: {}", e))?;
        }
        // index the frame
        let mut tx_keys = Vec::new();
        walk_tx_ranges(body, &mut |start, end| {
            tx_keys.push(key8(&sha256d(&body[start..end])));
            Ok(())
        })?;
        let seq = inner.blocks.frames as u32 + {
            let a = inner.appendix.read().unwrap();
            a.len() as u32
        };
        let akey = key8(&hash);
        {
            let mut a = inner.appendix.write().unwrap();
            // first arrival wins (matches the linear-scan lookup semantics)
            inner
                .appendix_by_key
                .write()
                .unwrap()
                .entry(akey)
                .or_insert(a.len());
            a.push(AppendedFrame {
                key: akey,
                off,
                seq,
                height,
                tx_keys,
            });
        }
        Ok(hash)
    }
}

// ---------------------------------------------------------------------------
// Tests — synthetic blk files built in-memory, written to a temp dir. No
// archive dependency: every frame below is constructed from scratch and the
// window oracle is built from the same synthetic blocks.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{Tx, Vin, Vout};
    use crate::p2p::internal_to_display;
    use std::time::Duration;

    fn cs(w: &mut Vec<u8>, n: u64) {
        if n < 0xfd {
            w.push(n as u8);
        } else if n <= 0xffff {
            w.push(0xfd);
            w.extend_from_slice(&(n as u16).to_le_bytes());
        } else {
            w.push(0xfe);
            w.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }

    fn coin_tx(script_sig: Vec<u8>, value: i64, out_script: Vec<u8>) -> Tx {
        Tx {
            version: 1,
            time: 1_700_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig,
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value,
                script_pubkey: out_script,
            }],
            locktime: 0,
        }
    }

    fn block(prev: Hash, txs: Vec<Tx>) -> Block {
        let mut b = Block {
            version: 2,
            prev_hash: prev,
            merkle_root: [0u8; 32],
            time: 1_700_000_100,
            bits: 0x2000_ffff,
            nonce: 7,
            vtx: txs,
            block_sig: vec![],
        };
        // real merkle over the txs: distinct txs → distinct header hashes
        // (a fixed root would collide the stale/live branch headers)
        b.merkle_root = b.merkle_root();
        b
    }

    fn frame_bytes(b: &Block) -> Vec<u8> {
        let mut body = b.header_bytes();
        cs(&mut body, b.vtx.len() as u64);
        for t in &b.vtx {
            body.extend_from_slice(&t.serialize());
        }
        cs(&mut body, b.block_sig.len() as u64);
        body.extend_from_slice(&b.block_sig);
        let mut f = Vec::new();
        f.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(&body);
        f
    }

    fn txid(t: &Tx) -> Hash {
        crate::legacy::sha256d(&t.serialize())
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tri7-bidx-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Build the synthetic chain: genesis(200B) → b1 → {b2 stale, b2 live} → b3.
    /// Returns (frames bytes, hashes of [b0,b1,b2,b3], txids).
    fn synthetic_chain() -> (Vec<u8>, Vec<Hash>, Vec<Hash>, Hash) {
        // genesis: 80 hdr + 1(cs) + tx + 1(cs sig) == 200 → tx must be 118 B
        // tx = 4+4+1+[32+4+1+54+4]+1+[8+1+0]+4 = 118 ✓ (54-byte scriptSig)
        let g = block([0u8; 32], vec![coin_tx(vec![0x51u8; 54], 50, vec![])]);
        let g_frame_len = frame_bytes(&g).len();
        assert_eq!(g_frame_len, 8 + 200);
        let mut cur = g;
        let mut hashes = vec![header_hash_ok(&frame_bytes(&cur))];
        let mut frames = frame_bytes(&cur);

        let b1_txs = vec![
            coin_tx(vec![], 0, vec![]),         // coinbase (empty marker)
            coin_tx(vec![], 1_000, vec![0x76]), // coinstake-ish
        ];
        let b1 = block(hashes[0], b1_txs.clone());
        frames.extend_from_slice(&frame_bytes(&b1));
        hashes.push(header_hash_ok(&frame_bytes(&b1)));

        // stale branch off b1
        let b2_stale = block(hashes[1], vec![coin_tx(vec![0xaa], 9_999_999, vec![])]);
        let stale_frame = frame_bytes(&b2_stale);
        let stale_txid = txid(&b2_stale.vtx[0]);
        frames.extend_from_slice(&stale_frame);

        // live b2 (shares tx with stale to exercise duplicate-txid handling? no —
        // distinct bytes; the duplicate-frame path is covered by candidates())
        let b2 = block(hashes[1], vec![coin_tx(vec![0xbb], 8_888_888, vec![])]);
        frames.extend_from_slice(&frame_bytes(&b2));
        hashes.push(header_hash_ok(&frame_bytes(&b2)));

        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        frames.extend_from_slice(&frame_bytes(&b3));
        hashes.push(header_hash_ok(&frame_bytes(&b3)));

        (frames, hashes, vec![], stale_txid)
    }

    fn header_hash_ok(frame: &[u8]) -> Hash {
        header_hash(&frame[8..]).unwrap()
    }

    // ---- gap recovery: missing tip frame + appendix append -------------------

    #[test]
    fn gap_recovery_missing_tip_frame_append_restores_serving() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("gapappend");
        let path = dir.join("blk.dat");
        // rebuild b3's frame exactly as synthetic_chain built it (the helpers
        // are deterministic) so we can hold the file WITHOUT the tip body —
        // the DNS2 shape: tipstate's tip frame absent from disk
        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        let b3_frame = frame_bytes(&b3);
        assert!(frames.ends_with(&b3_frame));
        let truncated = &frames[..frames.len() - b3_frame.len()];
        std::fs::write(&path, truncated).unwrap();

        // oracle covering genesis..b3 (tip = b3, whose frame is missing)
        let oracle = BuildOracle {
            base: 2,
            hashes: vec![hashes[2], hashes[3]],
            tip: 3,
        };
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        let mut waited = 0;
        while waited < 100 && !matches!(store.status(), StoreStatus::Ready { .. }) {
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
        }
        assert!(matches!(store.status(), StoreStatus::Ready { .. }));

        // NEW SEMANTICS: the missing tip frame shortens the height map, it
        // does NOT invalidate it — heights below the gap serve normally
        let hit2 = store.block_by_height(2).unwrap().unwrap();
        assert_eq!(hit2.hash, hashes[2]);
        assert_eq!(
            store.hash_at_height(2).unwrap().unwrap(),
            internal_to_display(&hashes[2])
        );
        // the gap itself is absent
        assert!(store.block_by_height(3).unwrap().is_none());
        assert!(store.block_by_hash(&hashes[3]).unwrap().is_none());
        assert_eq!(store.threaded_tip(), Some(2));

        // GAP FILL: append the tip body (as gap recovery does after a peer
        // serves it) with its height assignment
        let body = &b3_frame[8..];
        let appended = store.append_block(body, Some(3)).expect("append");
        assert_eq!(appended, hashes[3]);
        // idempotent: a re-append is a no-op
        let again = store.append_block(body, Some(3)).expect("re-append");
        assert_eq!(again, hashes[3]);

        // the gap now serves by hash AND height; the tx inside is indexed
        let hit3 = store.block_by_hash(&hashes[3]).unwrap().expect("gap filled");
        assert_eq!(hit3.height, Some(3));
        let hit3h = store.block_by_height(3).unwrap().expect("by height");
        assert_eq!(hit3h.hash, hashes[3]);
        assert_eq!(store.threaded_tip(), Some(3));
        let txid3 = txid(&hit3.block.vtx[0]);
        let thit = store.tx_by_id(&txid3).unwrap().expect("appended tx found");
        assert_eq!(thit.block_height, Some(3));

        // RESTART: a fresh store re-scans the grown file (blk_len cache miss)
        // and finds the appended frame in the MAIN index — no appendix needed
        let store2 = BlockStore::new(Some(path.clone()), Some(dir.join("idx2")));
        store2.spawn_build(Some(BuildOracle {
            base: 2,
            hashes: vec![hashes[2], hashes[3]],
            tip: 3,
        }));
        let mut waited2 = 0;
        while waited2 < 100 && !matches!(store2.status(), StoreStatus::Ready { .. }) {
            std::thread::sleep(Duration::from_millis(50));
            waited2 += 1;
        }
        let hit3r = store2.block_by_height(3).unwrap().expect("post-restart");
        assert_eq!(hit3r.hash, hashes[3]);
        assert_eq!(store2.threaded_tip(), Some(3));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_during_index_build_buffers_and_flushes_on_ready() {
        // Fresh-node onboarding shape: blocks get accepted while the cold
        // index build is still scanning. They must land in the store once it
        // is ready — not vanish into a permanent hole (the bug that kept a
        // freshly-seeded node's wallet live view from ever seeding).
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("pendingappend");
        let path = dir.join("blk.dat");
        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        let b3_frame = frame_bytes(&b3);
        let truncated = &frames[..frames.len() - b3_frame.len()];
        std::fs::write(&path, truncated).unwrap();

        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(None);
        // append while the build may still be running — either order is fine:
        // pre-ready buffers into `pending`, post-ready appends directly
        let body = &b3_frame[8..];
        let first = store.append_block(body, Some(3)).expect("append");
        assert_eq!(first, hashes[3]);

        let mut waited = 0;
        while waited < 200 && !matches!(store.status(), StoreStatus::Ready { .. }) {
            std::thread::sleep(Duration::from_millis(25));
            waited += 1;
        }
        assert!(matches!(store.status(), StoreStatus::Ready { .. }));
        // post-ready append flushes anything buffered and dedups
        let again = store.append_block(body, Some(3)).expect("flush append");
        assert_eq!(again, hashes[3]);

        let hit = store.block_by_height(3).unwrap().expect("served after ready");
        assert_eq!(hit.hash, hashes[3]);
        let txid3 = txid(&hit.block.vtx[0]);
        let thit = store.tx_by_id(&txid3).unwrap().expect("tx indexed");
        assert_eq!(thit.block_height, Some(3));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tx_range_walk_matches_block_read() {
        let b1_txs = vec![
            coin_tx(vec![], 0, vec![]),
            coin_tx(vec![0x11; 20], 500, vec![0xac]),
        ];
        let b = block([3u8; 32], b1_txs.clone());
        let frame = frame_bytes(&b);
        let mut got: Vec<Hash> = Vec::new();
        walk_tx_ranges(&frame[8..], &mut |s, e| {
            got.push(crate::legacy::sha256d(&frame[8 + s..8 + e]));
            Ok(())
        })
        .unwrap();
        let want: Vec<Hash> = b1_txs.iter().map(|t| t.hash()).collect();
        assert_eq!(got, want);
        // malformed body (shorter than header) errors
        assert!(walk_tx_ranges(&[0u8; 10], &mut |_, _| Ok(())).is_err());
    }

    #[test]
    fn build_threads_live_chain_past_stale_seam_and_anchors() {
        let (frames, hashes, _, _stale_txid) = synthetic_chain();
        let dir = tmpdir("thread");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 2,
            hashes: vec![hashes[2], hashes[3]],
            tip: 3,
        };
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(
            build.heights_ok,
            "live thread must reach the window anchor and tip"
        );
        assert_eq!(build.frames, 5);
        assert_eq!(build.skipped_frames, 1); // the stale branch frame
        assert_eq!(build.blocks.entries.len(), 5);
        // height map: 4 heights, stale frame unassigned
        assert_eq!(build.blocks.height_off.len(), 4);
        assert_eq!(build.blocks.heights_by_seq, vec![0, 1, HEIGHT_UNSET, 2, 3]);
        // height → the LIVE frames
        let off_h2 = build.blocks.offset_by_height(2).unwrap();
        let off_stale = build.blocks.entries[2].off;
        assert_ne!(off_h2, off_stale);
        // binary-search lookup finds the live block by hash
        let cands = build.blocks.candidates_by_key(key8(&hashes[2]));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].off, off_h2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn anchor_failure_disables_heights() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("anchor");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        // oracle whose base hash matches NOTHING in the file → thread stalls
        let oracle = BuildOracle {
            base: 2,
            hashes: vec![[0xEE; 32], [0xEE; 32]],
            tip: 3,
        };
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(!build.heights_ok);
        assert_eq!(build.blocks.height_off.len(), 2); // only genesis + b1 chained
                                                      // hash/tx indexes still complete (heights-independent)
        assert_eq!(build.blocks.entries.len(), 5);
        assert!(build.txs.txs >= 5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oracleless_build_trusts_file_order() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("nooracle");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let build = build_index(&path, None, &|_| {}).unwrap();
        assert!(build.heights_ok);
        // sequential rule: b2' (chaining off b1) wins height 2 in file order
        assert_eq!(
            build.blocks.heights_by_seq,
            vec![0, 1, 2, HEIGHT_UNSET, HEIGHT_UNSET]
        );
        let _ = hashes;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn genesis_guard_rejects_short_first_frame() {
        let dir = tmpdir("genesis");
        let path = dir.join("blk.dat");
        std::fs::write(&path, vec![0u8; 150]).unwrap();
        assert!(build_index(&path, None, &|_| {}).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_truncated_keys_return_all_candidates() {
        let mut idx = BlockIndex::default();
        idx.entries = vec![
            BlockEntry {
                key: 42,
                off: 100,
                seq: 0,
            },
            BlockEntry {
                key: 42,
                off: 200,
                seq: 1,
            },
            BlockEntry {
                key: 43,
                off: 300,
                seq: 2,
            },
        ];
        let c = idx.candidates_by_key(42);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].off, 100);
        assert_eq!(c[1].off, 200);
        assert_eq!(idx.candidates_by_key(43).len(), 1);
        assert_eq!(idx.candidates_by_key(44).len(), 0);
    }

    #[test]
    fn cache_roundtrip_preserves_indexes() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("cache");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 2,
            hashes: vec![hashes[2], hashes[3]],
            tip: 3,
        };
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        let blk_len = std::fs::metadata(&path).unwrap().len();
        save_caches(&dir, &build, blk_len, Some(&oracle)).unwrap();

        let loaded = load_caches(&dir, blk_len, Some(&oracle))
            .unwrap()
            .expect("cache must load");
        assert!(loaded.heights_ok);
        assert_eq!(loaded.frames, build.frames);
        assert_eq!(loaded.txs_indexed, build.txs_indexed);
        assert_eq!(loaded.blocks.heights_by_seq, build.blocks.heights_by_seq);
        assert_eq!(loaded.blocks.height_off, build.blocks.height_off);
        assert_eq!(loaded.blocks.entries.len(), build.blocks.entries.len());
        assert_eq!(loaded.txs.entries.len(), build.txs.entries.len());
        // cache invalidation: file length change → rebuild
        assert!(load_caches(&dir, blk_len + 1, Some(&oracle))
            .unwrap()
            .is_none());
        // oracle change (chain advanced) → rebuild
        let o2 = BuildOracle {
            base: 2,
            hashes: oracle.hashes.clone(),
            tip: 4,
        };
        assert!(load_caches(&dir, blk_len, Some(&o2)).unwrap().is_none());
        // corrupt payload → rebuild
        let mut corrupt = std::fs::read(dir.join("blockidx.bin")).unwrap();
        let n = corrupt.len();
        corrupt[n - 1] ^= 0xff;
        std::fs::write(dir.join("blockidx.bin"), &corrupt).unwrap();
        assert!(load_caches(&dir, blk_len, Some(&oracle)).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_serves_blocks_and_txs_from_disk_frames() {
        let (frames, hashes, _, stale_txid) = synthetic_chain();
        let dir = tmpdir("store");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 2,
            hashes: vec![hashes[2], hashes[3]],
            tip: 3,
        };
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        let mut waited = 0;
        while waited < 100 {
            if matches!(store.status(), StoreStatus::Ready { .. }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
        }
        assert!(
            matches!(store.status(), StoreStatus::Ready { .. }),
            "store never became ready: {:?}",
            store.status()
        );

        // by full hash (internal order)
        let hit = store.block_by_hash(&hashes[3]).unwrap().expect("b3 found");
        assert_eq!(hit.height, Some(3));
        assert_eq!(
            internal_to_display(&hit.hash),
            internal_to_display(&hashes[3])
        );
        // by height
        let hit2 = store.block_by_height(2).unwrap().unwrap();
        assert_eq!(hit2.hash, hashes[2]);
        // height → hash (display)
        let h1 = store.hash_at_height(1).unwrap().unwrap();
        assert_eq!(h1, internal_to_display(&hashes[1]));
        // beyond the map → None
        assert!(store.hash_at_height(4).unwrap().is_none());
        // live tx → height Some(3)
        let b3 = store.block_by_hash(&hashes[3]).unwrap().unwrap();
        let live_txid = txid(&b3.block.vtx[0]);
        let thit = store.tx_by_id(&live_txid).unwrap().expect("live tx found");
        assert_eq!(thit.block_height, Some(3));
        assert_eq!(
            internal_to_display(&thit.block_hash),
            internal_to_display(&hashes[3])
        );
        // stale-branch tx → found on disk but height unassigned
        let shit = store
            .tx_by_id(&stale_txid)
            .unwrap()
            .expect("stale tx found");
        assert_eq!(shit.block_height, None);
        // miss → None
        assert!(store.tx_by_id(&[0xAB; 32]).unwrap().is_none());
        assert!(store.block_by_hash(&[0xCD; 32]).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- chain-slot overlay: the fleet item 18 stale-slot-serve defect ----

    /// Wait for the store to be Ready (spawn_build + poll, same envelope as
    /// the gap-recovery test above).
    fn wait_ready(store: &Arc<BlockStore>) {
        let mut waited = 0;
        while waited < 100 && !matches!(store.status(), StoreStatus::Ready { .. }) {
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
        }
        assert!(matches!(store.status(), StoreStatus::Ready { .. }));
    }

    /// THE DNS2 RACE (fleet item 18): the height map threads up to h2; a
    /// raced body for h3 is appended FIRST (first-arrival appendix slot),
    /// then the chain's own h3 body arrives. Without the overlay, lookups
    /// serve the raced body forever — the fork generator. With the chain
    /// slot registered, every height lookup serves exactly the chain's body
    /// and the divergence is counted for the audit.
    #[test]
    fn chain_slot_overlay_beats_first_arrival_appendix() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("slotappend");
        let path = dir.join("blk.dat");
        // file holds genesis..b2-live ONLY (the map threads 0..=2); b3 is
        // appended at runtime — the raced body first, then ours
        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        let b3_frame = frame_bytes(&b3);
        assert!(frames.ends_with(&b3_frame));
        std::fs::write(&path, &frames[..frames.len() - b3_frame.len()]).unwrap();
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(BuildOracle {
            base: 0,
            hashes: hashes[..3].to_vec(),
            tip: 2,
        }));
        wait_ready(&store);

        // the raced body: same prev (b2), different txs → different hash
        let raced = block(hashes[2], vec![coin_tx(vec![0xdd], 6_666_666, vec![])]);
        assert_ne!(header_hash_ok(&frame_bytes(&raced)), hashes[3]);
        store
            .append_block(&frame_bytes(&raced)[8..], Some(3))
            .unwrap();
        store.append_block(&b3_frame[8..], Some(3)).unwrap();

        // DEFECT SHAPE (documented): with NO live chain registered the
        // legacy first-arrival resolution serves the raced body
        let legacy_hit = store.block_by_height(3).unwrap().unwrap();
        assert_eq!(legacy_hit.hash, header_hash_ok(&frame_bytes(&raced)));

        // register the chain's slot (what advance_with_block does) →
        // every height lookup now serves the chain's own body
        store.set_chain_slot(3, hashes[3]);
        let hit = store.block_by_height(3).unwrap().unwrap();
        assert_eq!(hit.hash, hashes[3], "serving follows the chain, not arrival order");
        assert_eq!(
            store.hash_at_height(3).unwrap().unwrap(),
            internal_to_display(&hashes[3]),
            "getblockhash must never contradict serving"
        );
        let raw = store.raw_body_by_height(3).unwrap().unwrap();
        assert_eq!(raw, b3_frame[8..], "raw getblock serves the chain's bytes");
        assert_eq!(hit.height, Some(3));
        // the divergence was counted for the audit
        assert!(store.stale_slot_hits() >= 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Kernel-evidence class (2026-11-11 laptop): the wallet live view is
    /// seeded by scanning block_by_height, so WHICHEVER height resolution
    /// decides — the build-time height map, the first-arrival appendix, or
    /// the chain-slot overlay — the wallet must see the CHAIN's body (its
    /// coins' containing-block evidence then matches what
    /// check_proof_of_stake reconstructs). A raced body served to the seed
    /// is exactly the phantom-coin acquisition the stake loop's evidence
    /// resolver exists to flag — the copied-store shape.
    #[test]
    fn wallet_seed_height_resolutions_serve_the_chain_body() {
        // --- HEIGHT-MAP layer decides: a raced frame appended into the
        // appendix must NOT shadow a height the build threaded canonically
        // (no overlay registered — legacy read-only consumers).
        {
            let (frames, hashes, _, _) = synthetic_chain();
            let dir = tmpdir("seedres-hmap");
            let path = dir.join("blk.dat");
            std::fs::write(&path, &frames).unwrap();
            let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
            store.spawn_build(Some(BuildOracle {
                base: 0,
                hashes: hashes.clone(),
                tip: 3,
            }));
            wait_ready(&store);
            let raced_h2 = block(hashes[1], vec![coin_tx(vec![0xee], 7_777_777, vec![])]);
            store
                .append_block(&frame_bytes(&raced_h2)[8..], Some(2))
                .unwrap();
            let hit = store.block_by_height(2).unwrap().unwrap();
            assert_eq!(
                hit.hash, hashes[2],
                "height map beats the appendix: the wallet seeds the chain's body"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        // --- APPENDIX layer decides (arrival order = chain order): a
        // canonical body appended at an unthreaded height serves as truth.
        {
            let (frames, hashes, _, _) = synthetic_chain();
            // b3's frame is the LAST frame of the synthetic file (the same
            // identity trick the overlay test uses)
            let b3_frame = frame_bytes(&block(
                hashes[2],
                vec![coin_tx(vec![0xcc], 7_777_777, vec![])],
            ));
            assert!(frames.ends_with(&b3_frame));
            let dir = tmpdir("seedres-appendix");
            let path = dir.join("blk.dat");
            std::fs::write(&path, &frames[..frames.len() - b3_frame.len()]).unwrap();
            let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
            store.spawn_build(Some(BuildOracle {
                base: 0,
                hashes: hashes[..3].to_vec(),
                tip: 2,
            }));
            wait_ready(&store);
            store.append_block(&b3_frame[8..], Some(3)).unwrap();
            let hit = store.block_by_height(3).unwrap().unwrap();
            assert_eq!(
                hit.hash, hashes[3],
                "appendix serves the chain's body when it arrived in chain order"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        // --- RACED-FIRST ACQUISITION (no overlay): the raced body wins the
        // appendix — the phantom-evidence seed the stake loop's resolver
        // flags. Registering the chain's slot (what the boot does before any
        // consumer runs) heals every height lookup.
        {
            let (frames, hashes, _, _) = synthetic_chain();
            let b3_frame = frame_bytes(&block(
                hashes[2],
                vec![coin_tx(vec![0xcc], 7_777_777, vec![])],
            ));
            let raced_frame = frame_bytes(&block(
                hashes[2],
                vec![coin_tx(vec![0xdd], 6_666_666, vec![])],
            ));
            let dir = tmpdir("seedres-raced");
            let path = dir.join("blk.dat");
            std::fs::write(&path, &frames[..frames.len() - b3_frame.len()]).unwrap();
            let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
            store.spawn_build(Some(BuildOracle {
                base: 0,
                hashes: hashes[..3].to_vec(),
                tip: 2,
            }));
            wait_ready(&store);
            store.append_block(&raced_frame[8..], Some(3)).unwrap();
            store.append_block(&b3_frame[8..], Some(3)).unwrap();
            let acquired = store.block_by_height(3).unwrap().unwrap();
            assert_ne!(
                acquired.hash, hashes[3],
                "the raced body is what a seed-copy store would feed the wallet seed"
            );
            store.set_chain_slot(3, hashes[3]);
            let hit = store.block_by_height(3).unwrap().unwrap();
            assert_eq!(
                hit.hash, hashes[3],
                "the chain-slot overlay decides: wallet evidence matches the validator"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// No height above the registered chain tip is served from ANY
    /// resolution — even one the frozen height map holds (post-rollback the
    /// abandoned branch must reach no peer until the adopted branch
    /// re-fills those heights).
    #[test]
    fn no_serving_above_the_registered_chain_tip() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("slotguard");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(BuildOracle {
            base: 0,
            hashes: hashes.clone(),
            tip: 3,
        }));
        wait_ready(&store);

        // the chain rolled back to h2 (abandoned b3): the map still holds
        // h3 → b3, but the chain tip is 2 → nothing serves above it
        store.set_chain_slot(0, hashes[0]);
        store.set_chain_slot(1, hashes[1]);
        store.set_chain_slot(2, hashes[2]);
        store.drop_chain_slots_above(2);
        assert_eq!(store.chain_slot_tip(), Some(2));
        assert!(store.block_by_height(3).unwrap().is_none());
        assert!(store.hash_at_height(3).unwrap().is_none());
        assert!(store.raw_body_by_height(3).unwrap().is_none());
        // below the guard the window serves normally
        assert_eq!(store.block_by_height(2).unwrap().unwrap().hash, hashes[2]);

        // re-adopt b3 (the judge's heavier-branch path re-fills the slot):
        // set_chain_slot raises the tip again and serving resumes with the
        // chain's own body
        store.set_chain_slot(3, hashes[3]);
        assert_eq!(store.chain_slot_tip(), Some(3));
        assert_eq!(store.block_by_height(3).unwrap().unwrap().hash, hashes[3]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// In-window refill (the OTHER stale-slot layer): the judge switched the
    /// chain to a branch the frozen height map does not know — the overlay
    /// re-points the slot and the switch is counted, while a store with NO
    /// registered chain keeps the exact legacy semantics (read-only
    /// consumers: GUI explorer, tx prevout lookup).
    #[test]
    fn chain_slot_repoints_a_refilled_in_window_slot() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("slotrefill");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(BuildOracle {
            base: 0,
            hashes: hashes.clone(),
            tip: 3,
        }));
        wait_ready(&store);

        // full chain registered (the boot seed)
        for (i, h) in hashes.iter().enumerate() {
            store.set_chain_slot(i as u64, *h);
        }
        assert_eq!(store.chain_slots_len(), 4);
        assert_eq!(store.chain_slot_tip(), Some(3));

        // the branch flipped in-window: the chain now holds b2-stale at h2
        // (frame IS on disk — unthreaded). Serving must follow the chain.
        let b2_stale = block(hashes[1], vec![coin_tx(vec![0xaa], 9_999_999, vec![])]);
        let stale_hash = header_hash_ok(&frame_bytes(&b2_stale));
        store.set_chain_slot(2, stale_hash);
        let hit = store.block_by_height(2).unwrap().unwrap();
        assert_eq!(hit.hash, stale_hash, "the chain's slot choice wins in-window too");
        assert_eq!(
            store.hash_at_height(2).unwrap().unwrap(),
            internal_to_display(&stale_hash)
        );
        let raw2 = store.raw_body_by_height(2).unwrap().unwrap();
        assert_eq!(
            raw2,
            frame_bytes(&b2_stale)[8..],
            "raw getblock serves the chain's in-window body too"
        );
        assert!(store.stale_slot_hits() >= 1);

        // window slide bounds the overlay tail
        store.drop_chain_slots_below(2);
        assert_eq!(store.chain_slots_len(), 2); // h2, h3
        assert_eq!(store.chain_slot_tip(), Some(3), "below-drop keeps the tip");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Read-only consumers (no live chain registered) keep the exact legacy
    /// semantics — the overlay only arms where the follow loop seeded it.
    #[test]
    fn without_a_registered_chain_legacy_resolution_is_untouched() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("slotlegacy");
        let path = dir.join("blk.dat");
        // map threads 0..=2; the runtime appends land in the appendix
        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        let b3_frame = frame_bytes(&b3);
        std::fs::write(&path, &frames[..frames.len() - b3_frame.len()]).unwrap();
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(BuildOracle {
            base: 0,
            hashes: hashes[..3].to_vec(),
            tip: 2,
        }));
        wait_ready(&store);
        let raced = block(hashes[2], vec![coin_tx(vec![0xdd], 6_666_666, vec![])]);
        store
            .append_block(&frame_bytes(&raced)[8..], Some(3))
            .unwrap();
        store.append_block(&b3_frame[8..], Some(3)).unwrap();

        assert_eq!(store.chain_slot_tip(), None, "no live chain seeded");
        assert_eq!(store.chain_slots_len(), 0);
        // first arrival wins, exactly as before this change
        let hit = store.block_by_height(3).unwrap().unwrap();
        assert_eq!(hit.hash, header_hash_ok(&frame_bytes(&raced)));
        assert_eq!(
            store.hash_at_height(3).unwrap().unwrap(),
            internal_to_display(&header_hash_ok(&frame_bytes(&raced)))
        );
        // raw_body_by_height legacy legs: an appendix-only height (map miss)
        // serves NOTHING (the old no-proof-no-serve guard), and an in-map
        // height serves the map's proven body
        assert!(store.raw_body_by_height(3).unwrap().is_none());
        let b2 = block(hashes[1], vec![coin_tx(vec![0xbb], 8_888_888, vec![])]);
        assert_eq!(
            store.raw_body_by_height(2).unwrap().unwrap(),
            frame_bytes(&b2)[8..]
        );
        assert_eq!(store.stale_slot_hits(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// F4: a power cut between the amortized fsyncs leaves the file ending
    /// MID-FRAME. That is a lost tail, not corruption: the build must index
    /// every complete frame and come up Ready, the cache must carry the cut
    /// point across a restart, and the first append must CUT the partial frame
    /// instead of writing behind it (which would make every later scan fail on
    /// mid-file corruption).
    #[test]
    fn torn_tail_builds_ready_and_the_next_append_heals_it() {
        let (frames, hashes, _, _) = synthetic_chain();
        let dir = tmpdir("torntail");
        let path = dir.join("blk.dat");
        let b3 = block(hashes[2], vec![coin_tx(vec![0xcc], 7_777_777, vec![])]);
        let b3_frame = frame_bytes(&b3);
        assert!(frames.ends_with(&b3_frame));
        let complete = frames.len() - b3_frame.len();
        // 8-byte frame header landed, body cut 20 bytes in
        std::fs::write(&path, &frames[..complete + 8 + 20]).unwrap();

        // scan path: Ready, and every complete frame is served
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(None);
        wait_ready(&store);
        assert!(
            store.block_by_hash(&hashes[2]).unwrap().is_some(),
            "frames before the tear must serve"
        );
        assert!(
            store.block_by_hash(&hashes[3]).unwrap().is_none(),
            "the torn frame must NOT be indexed"
        );

        // cache path: a fresh store restores the torn offset from the cache
        let store2 = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store2.spawn_build(None);
        wait_ready(&store2);
        assert!(store2.block_by_hash(&hashes[2]).unwrap().is_some());

        // the append cuts the 20 torn bytes, then writes a clean frame
        let appended = store2
            .append_block(&b3_frame[8..], Some(3))
            .expect("append over a torn tail");
        assert_eq!(appended, hashes[3]);
        let expect_len = (complete + b3_frame.len()) as u64;
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            expect_len,
            "the partial frame must be gone, not buried under a new frame"
        );

        // restart proof: a full re-scan of the healed file succeeds (it would
        // FAIL on mid-file corruption) and serves the appended block. The
        // oracle is what gives the stale branch's twin its height, exactly as
        // the live node's tipstate does.
        let store3 = BlockStore::new(Some(path.clone()), Some(dir.join("idx3")));
        store3.spawn_build(Some(BuildOracle {
            base: 0,
            hashes: hashes.clone(),
            tip: 3,
        }));
        wait_ready(&store3);
        let hit = store3.block_by_height(3).unwrap().expect("h3 after heal");
        assert_eq!(hit.hash, hashes[3]);
        assert_eq!(store3.threaded_tip(), Some(3));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- item 44: fork-aware threading below the window base ---------------

    /// `g(0) → b1(1) → {loser, winner} → winner's children`. Both children of
    /// the forked parent and the winner's children are stored BEFORE the
    /// window's first frame (the window is the caller's choice). Returns
    /// (frames bytes, per-height hashes of the canonical chain, loser's hash,
    /// (winner frame offset, loser frame offset)).
    /// `loser_first` = the abandoned child reached the file first (DNS2's
    /// shape); otherwise the loser is a late stale single at the end.
    /// `stale_siblings` = orphaned blocks on the canonical parent, stored
    /// BEFORE the canonical child (a staker's own block between two stakes).
    fn forked_chain(
        loser_first: bool,
        n_after_fork: usize,
        stale_siblings: usize,
    ) -> (Vec<u8>, Vec<Hash>, Hash, (u64, u64)) {
        let mut frames = Vec::new();
        let mut canon: Vec<Hash> = Vec::new();
        let g = block([0u8; 32], vec![coin_tx(vec![0x51u8; 54], 50, vec![])]);
        assert_eq!(frame_bytes(&g).len(), 8 + 200, "genesis frame must be 208 B");
        let mut prev = header_hash_ok(&frame_bytes(&g));
        frames.extend_from_slice(&frame_bytes(&g));
        canon.push(prev);
        let b1 = block(prev, vec![coin_tx(vec![], 0, vec![]), coin_tx(vec![], 1_000, vec![0x76])]);
        frames.extend_from_slice(&frame_bytes(&b1));
        prev = header_hash_ok(&frame_bytes(&b1));
        canon.push(prev);
        let fork_parent = prev;
        let loser = block(fork_parent, vec![coin_tx(vec![0xaa], 9_999_999, vec![])]);
        let loser_hash = header_hash_ok(&frame_bytes(&loser));
        let loser_frame = frame_bytes(&loser);
        let mut loser_off = frames.len() as u64;
        if loser_first {
            frames.extend_from_slice(&loser_frame);
        }
        let winner = block(fork_parent, vec![coin_tx(vec![0xbb], 8_888_888, vec![])]);
        let winner_off = frames.len() as u64;
        frames.extend_from_slice(&frame_bytes(&winner));
        prev = header_hash_ok(&frame_bytes(&winner));
        canon.push(prev);
        for i in 0..n_after_fork {
            if i < stale_siblings {
                let s = block(prev, vec![coin_tx(vec![0xe0 + i as u8], 5_555_555, vec![])]);
                frames.extend_from_slice(&frame_bytes(&s));
            }
            let b = block(
                prev,
                vec![coin_tx(vec![0xc0 + i as u8], 7_777_777 - i as i64 * 7, vec![])],
            );
            frames.extend_from_slice(&frame_bytes(&b));
            prev = header_hash_ok(&frame_bytes(&b));
            canon.push(prev);
        }
        if !loser_first {
            loser_off = frames.len() as u64;
            frames.extend_from_slice(&loser_frame);
        }
        (frames, canon, loser_hash, (winner_off, loser_off))
    }

    /// The DNS2 shape, end to end: loser stored first, fork below the window
    /// base. The old build let the loser keep h2 and left the winner and its
    /// pre-window children height-less (a hole up to the window base, serving
    /// the loser on getblockhash(2) and nothing on 3..base).
    #[test]
    fn fork_below_window_base_adopts_canonical_branch_and_frees_loser_height() {
        let (frames, canon, loser_hash, (winner_off, loser_off)) = forked_chain(true, 4, 0);
        let dir = tmpdir("fork44a");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        // window = the last two canonical frames (h5, h6)
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let unset = HEIGHT_UNSET;

        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(build.heights_ok);
        assert_eq!(build.frames, 8);
        // the winner + its 2 pre-window children re-threaded, the loser de-heighted
        assert_eq!(build.fork_adopted, 4);
        assert_eq!(build.skipped_frames, 1, "only the loser is height-less");
        assert_eq!(build.blocks.heights_by_seq, vec![0, 1, unset, 2, 3, 4, 5, 6]);
        assert_eq!(build.blocks.height_off.len(), 7, "no hole: every height 0..=6 has a frame");
        // h2 → the WINNER's frame, not the loser's
        assert_eq!(build.blocks.offset_by_height(2), Some(winner_off));
        assert_ne!(winner_off, loser_off);
        assert_eq!(build.blocks.tip_height(), Some(6));

        // the same through the store's public surface (what the RPC reads) …
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle.clone()));
        wait_ready(&store);
        assert_eq!(
            store.block_by_height(2).unwrap().expect("h2 served").hash,
            canon[2],
            "getblock(2) = the canonical block"
        );
        assert_eq!(
            store.hash_at_height(2).unwrap(),
            Some(internal_to_display(&canon[2])),
            "getblockhash(2) = the canonical block"
        );
        let loser = store.block_by_hash(&loser_hash).unwrap().expect("loser is stored");
        assert_eq!(loser.off, loser_off);
        assert_eq!(loser.height, None, "getblock(loser) reports no active height");
        assert_eq!(
            store.block_by_height(2).unwrap().unwrap().off,
            winner_off,
            "the height map points at the winner's frame"
        );
        assert_eq!(store.threaded_tip(), Some(6));
        for h in 0..=6 {
            assert!(store.frame_offset(h).is_some(), "h{h} has a frame");
        }
        // … and again after a restart from the saved cache (the hole must not
        // come back from disk)
        let store2 = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store2.spawn_build(Some(oracle));
        wait_ready(&store2);
        assert_eq!(store2.block_by_height(2).unwrap().unwrap().hash, canon[2]);
        assert_eq!(store2.block_by_hash(&loser_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Winner stored first: the thread reaches the window untouched and the
    /// late loser is an inert stale single.
    #[test]
    fn fork_winner_first_in_file_threads_unchanged_and_side_branch_is_inert() {
        let (frames, canon, loser_hash, (winner_off, _)) = forked_chain(false, 4, 0);
        let dir = tmpdir("fork44b");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let unset = HEIGHT_UNSET;
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(build.heights_ok);
        assert_eq!(build.fork_adopted, 0, "the thread reached the window: nothing to re-thread");
        assert_eq!(build.skipped_frames, 1);
        assert_eq!(build.blocks.heights_by_seq, vec![0, 1, 2, 3, 4, 5, 6, unset]);
        assert_eq!(build.blocks.height_off.len(), 7);
        assert_eq!(build.blocks.offset_by_height(2), Some(winner_off));

        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        assert_eq!(store.block_by_height(2).unwrap().unwrap().hash, canon[2]);
        assert_eq!(store.block_by_hash(&loser_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Fork straddling the window base: the winner (below the base) changes
    /// hands, the loser loses its height, the frames inside the window are the
    /// oracle's as before.
    #[test]
    fn fork_across_window_base_rethreads_only_below_the_window() {
        let (frames, canon, loser_hash, (winner_off, loser_off)) = forked_chain(true, 2, 0);
        let dir = tmpdir("fork44c");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        // window base = h3 = the winner's first child
        let oracle = BuildOracle {
            base: 3,
            hashes: vec![canon[3], canon[4]],
            tip: 4,
        };
        let unset = HEIGHT_UNSET;
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(build.heights_ok);
        assert_eq!(build.fork_adopted, 2, "winner adopted + loser de-heighted");
        assert_eq!(build.skipped_frames, 1);
        assert_eq!(build.blocks.heights_by_seq, vec![0, 1, unset, 2, 3, 4]);
        assert_eq!(build.blocks.height_off.len(), 5);
        assert_eq!(build.blocks.offset_by_height(2), Some(winner_off));
        assert_ne!(winner_off, loser_off);
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        assert_eq!(store.block_by_height(2).unwrap().unwrap().hash, canon[2]);
        assert_eq!(
            store.hash_at_height(2).unwrap(),
            Some(internal_to_display(&canon[2]))
        );
        assert_eq!(store.block_by_hash(&loser_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real DNS2 shape: the node is the heavy staker, so its own orphaned
    /// blocks sit BETWEEN the canonical frames of the stranded run. The walk
    /// follows parent links only, so they neither derail it nor gain a height.
    #[test]
    fn fork_with_stale_siblings_between_canonical_frames_still_rethreads() {
        let (frames, canon, loser_hash, (winner_off, _)) = forked_chain(true, 4, 2);
        let dir = tmpdir("fork44d");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let unset = HEIGHT_UNSET;
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(build.heights_ok);
        assert_eq!(build.frames, 10);
        // winner + 2 pre-window children re-threaded, the loser de-heighted
        assert_eq!(build.fork_adopted, 4);
        assert_eq!(build.skipped_frames, 3, "the loser + the 2 stale singles");
        // seq: 0 g, 1 b1, 2 loser, 3 winner, 4 stale, 5 child, 6 stale, 7..9 children
        assert_eq!(build.blocks.heights_by_seq, vec![0, 1, unset, 2, unset, 3, unset, 4, 5, 6]);
        assert_eq!(build.blocks.height_off.len(), 7, "no hole: every height 0..=6 has a frame");
        assert_eq!(build.blocks.offset_by_height(2), Some(winner_off));
        assert_eq!(build.blocks.tip_height(), Some(6));

        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        for h in 2..=6 {
            assert_eq!(store.block_by_height(h).unwrap().unwrap().hash, canon[h as usize], "h{h}");
        }
        assert_eq!(store.block_by_hash(&loser_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A canonical CHILD stored before its PARENT — and inside the window.
    /// DNS2's real store on 2026-11-24: `2a0651f0` (h2218632) sits one frame
    /// AHEAD of its parent `aefca3c0` (h2218631). The oracle threads the
    /// child; when the late parent follows, the first `adopt_lineage` cleared
    /// the band up to the thread's height — i.e. the canonical CHILD — and
    /// nothing refilled it: a fresh single-height hole per out-of-order store,
    /// invisible while the overlay window still covered that height.
    #[test]
    fn child_stored_before_its_parent_keeps_both_heights() {
        // canonical chain g, c1..=c9 (hashes and bodies first, file order below)
        let g = block([0u8; 32], vec![coin_tx(vec![0x51u8; 54], 50, vec![])]);
        let mut canon = vec![header_hash_ok(&frame_bytes(&g))];
        let mut bodies = vec![g];
        for h in 1..=9u32 {
            let b = block(
                *canon.last().unwrap(),
                vec![coin_tx(vec![], 0, vec![]), coin_tx(vec![0x10 + h as u8], 1_000 * h as i64, vec![0x76])],
            );
            canon.push(header_hash_ok(&frame_bytes(&b)));
            bodies.push(b);
        }
        let loser = block(canon[4], vec![coin_tx(vec![0xaa], 9_999_999, vec![])]);
        let loser_hash = header_hash_ok(&frame_bytes(&loser));
        let dir = tmpdir("lateparent44");
        let path = dir.join("blk.dat");
        let mut frames = Vec::new();
        // file order: g c1 c2 c3 c4 | loser (child of c4) | c5 c6 | c8 BEFORE c7 | c9
        for b in &bodies[..=4] {
            frames.extend_from_slice(&frame_bytes(b));
        }
        frames.extend_from_slice(&frame_bytes(&loser));
        for i in [5usize, 6, 8, 7, 9] {
            frames.extend_from_slice(&frame_bytes(&bodies[i]));
        }
        std::fs::write(&path, &frames).unwrap();
        // the window covers c3..=c8, so c7/c8 are both oracle frames; c9 is an
        // append PAST the tip — it can only chain by parent-rule if the thread
        // frontier stayed on c8 when the late parent claimed h7.
        let oracle = BuildOracle {
            base: 3,
            hashes: canon[3..=8].to_vec(),
            tip: 8,
        };
        let unset = HEIGHT_UNSET;
        let build = build_index(&path, Some(&oracle), &|_| {}).unwrap();
        assert!(build.heights_ok);
        assert_eq!(build.frames, 11);
        assert_eq!(build.skipped_frames, 1, "only the loser is height-less");
        // seq: 0 g, 1..4 c1..c4, 5 loser, 6 c5, 7 c6, 8 c8, 9 c7, 10 c9
        assert_eq!(
            build.blocks.heights_by_seq,
            vec![0, 1, 2, 3, 4, unset, 5, 6, 8, 7, 9]
        );
        assert_eq!(build.blocks.height_off.len(), 10, "every height 0..=9 has a frame");
        assert_eq!(build.blocks.tip_height(), Some(9));
        for h in 0..=9u64 {
            assert!(
                build.blocks.offset_by_height(h).is_some(),
                "h{h} must have a frame"
            );
        }

        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        for h in 0..=9 {
            assert_eq!(store.block_by_height(h).unwrap().unwrap().hash, canon[h as usize], "h{h}");
        }
        // the RPC read path (getblock <hash>): BOTH sides of the inversion
        assert_eq!(store.block_by_hash(&canon[7]).unwrap().unwrap().height, Some(7));
        assert_eq!(store.block_by_hash(&canon[8]).unwrap().unwrap().height, Some(8));
        assert_eq!(store.block_by_hash(&loser_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Miss below the window: a canonical body that was never stored leaves
    /// that ONE height unservable — nothing above it shifts, the lineage above
    /// it still threads. LIMIT (documented, not a goal): below the missing
    /// body the walk has no hash link to follow, so frames stranded there stay
    /// height-less until gap recovery stores the body and the next rebuild
    /// runs (see `adopt_lineage`).
    #[test]
    fn missing_canonical_body_below_the_window_is_a_miss_not_a_shift() {
        let (frames, canon, loser_hash, _) = forked_chain(true, 4, 0);
        // cut the canonical frame at h3 out of the file; file order otherwise
        // kept: g b1 loser | winner [h3 missing] c4 c5 c6
        let mut kept = Vec::new();
        let mut cut_len = 0usize;
        let mut r = &frames[..];
        while r.len() >= 8 {
            let len = u32::from_le_bytes(r[4..8].try_into().unwrap()) as usize;
            let frame = &r[..8 + len];
            if header_hash_ok(frame) == canon[3] {
                cut_len = frame.len();
            } else {
                kept.extend_from_slice(frame);
            }
            r = &r[8 + len..];
        }
        assert!(cut_len > 0, "the h3 frame was found and cut");
        let dir = tmpdir("miss44");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &kept).unwrap();
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        // h3 is a Miss — never a neighbour's body
        assert!(store.block_by_height(3).unwrap().is_none(), "h3: Miss");
        assert!(store.hash_at_height(3).unwrap().is_none(), "h3: getblockhash Miss");
        // above the hole: the canonical blocks at their own heights
        for h in 4..=6 {
            assert_eq!(store.block_by_height(h).unwrap().unwrap().hash, canon[h as usize], "h{h}");
        }
        // below the missing body: the documented limit — the first-arrival
        // loser still holds its claim there in THIS build
        assert_eq!(store.block_by_height(2).unwrap().unwrap().hash, loser_hash, "(limit) h2");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (item 44b) The map's height is only a claim: once the active chain's
    /// overlay holds ANOTHER block at that height, the replaced block must
    /// report no height (and so no positive confirmations) — by hash.
    #[test]
    fn block_by_hash_reports_no_height_once_the_chain_overlay_replaces_it() {
        let (frames, canon, _, _) = forked_chain(false, 4, 0);
        let dir = tmpdir("overlay44");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        // before the overlay disagrees, the map's claim stands
        assert_eq!(store.block_by_hash(&canon[2]).unwrap().unwrap().height, Some(2));
        // a reorg at h2: the chain now holds another block there
        let other = block(canon[1], vec![coin_tx(vec![0x9d], 4_242_424, vec![])]);
        let other_hash = header_hash_ok(&frame_bytes(&other));
        assert_ne!(other_hash, canon[2]);
        for h in 0..=3 {
            store.set_chain_slot(h, if h == 2 { other_hash } else { canon[h as usize] });
        }
        assert_eq!(
            store.block_by_hash(&canon[2]).unwrap().unwrap().height,
            None,
            "replaced block: no height"
        );
        // its parent (h1, still the chain's block) keeps its height
        assert_eq!(store.block_by_hash(&canon[1]).unwrap().unwrap().height, Some(1));
        // the window slid on past h2: that slot is gone → nothing left to
        // disprove the map, and its claim stands again
        store.drop_chain_slots_below(3);
        assert_eq!(store.block_by_hash(&canon[2]).unwrap().unwrap().height, Some(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (item 44, runtime variant) An appendix height is a CLAIM made at
    /// accept time. A rollback retires the abandoned branch's claims, so that
    /// — once the live window has slid past the height and the appendix is
    /// the only thing answering — getblockhash(h) names the ADOPTED block,
    /// never the abandoned one. Re-accepting an already-stored block re-states
    /// its claim.
    #[test]
    fn rollback_retires_abandoned_appendix_claims_and_reaccept_restates_them() {
        let (frames, canon, _, _) = forked_chain(false, 4, 0);
        let dir = tmpdir("rollback44");
        let path = dir.join("blk.dat");
        std::fs::write(&path, &frames).unwrap();
        let oracle = BuildOracle {
            base: 5,
            hashes: vec![canon[5], canon[6]],
            tip: 6,
        };
        let store = BlockStore::new(Some(path.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(oracle));
        wait_ready(&store);
        for (h, c) in canon.iter().enumerate() {
            store.set_chain_slot(h as u64, *c);
        }
        let disp = |h: &Hash| internal_to_display(h);
        // two children of the tip: `first` is accepted at h7, then replaced
        let first = block(canon[6], vec![coin_tx(vec![0xa1], 4_242_424, vec![])]);
        let second = block(canon[6], vec![coin_tx(vec![0xa2], 4_242_424, vec![])]);
        let first_hash = header_hash_ok(&frame_bytes(&first));
        let second_hash = header_hash_ok(&frame_bytes(&second));
        assert_ne!(first_hash, second_hash);
        store.append_block(&frame_bytes(&first)[8..], Some(7)).unwrap();
        store.set_chain_slot(7, first_hash);
        assert_eq!(store.hash_at_height(7).unwrap(), Some(disp(&first_hash)));

        // reorg: roll back to the branch point (h6) — h7 serves nothing
        store.drop_chain_slots_above(6);
        assert_eq!(store.hash_at_height(7).unwrap(), None, "rolled back: h7 is nobody's");
        // the adopted child lands
        store.append_block(&frame_bytes(&second)[8..], Some(7)).unwrap();
        store.set_chain_slot(7, second_hash);
        // the live window slides past h7: the overlay is gone and the
        // appendix is all that is left — it must name the adopted block
        store.drop_chain_slots_below(8);
        assert_eq!(store.chain_slots_len(), 0);
        assert_eq!(
            store.hash_at_height(7).unwrap(),
            Some(disp(&second_hash)),
            "adopted block"
        );
        assert_eq!(
            store.block_by_hash(&first_hash).unwrap().unwrap().height,
            None,
            "abandoned block: no height"
        );

        // ...and the abandoned block adopted AGAIN later re-states its claim
        store.drop_chain_slots_above(6);
        store.append_block(&frame_bytes(&first)[8..], Some(7)).unwrap();
        store.set_chain_slot(7, first_hash);
        store.drop_chain_slots_below(8);
        assert_eq!(store.hash_at_height(7).unwrap(), Some(disp(&first_hash)), "re-accepted");
        assert_eq!(store.block_by_hash(&second_hash).unwrap().unwrap().height, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The lineage walk itself: parent links only (a stale sibling stored
    /// before the canonical child is ignored), heights handed over top-down,
    /// the first-arrival branch stranded, and a repeated call is a no-op.
    #[test]
    fn lineage_walk_ignores_stale_siblings_and_strands_the_first_arrival() {
        let mk = |n: u8| [n; 32];
        // file: g(0) b1(1) L(2) | S(3, child of b1) W(4, child of b1) W1(5, child of W)
        let mut sides = SideFrames::new();
        sides.insert(mk(20), SideFrame { parent: mk(1), off: 200, seq: 3 }); // S
        sides.insert(mk(21), SideFrame { parent: mk(1), off: 300, seq: 4 }); // W
        sides.insert(mk(22), SideFrame { parent: mk(21), off: 400, seq: 5 }); // W1
        let mut height_off = vec![10u64, 11, 12]; // g, b1, L on the thread
        height_off.resize(16, HEIGHT_GAP);
        let mut heights = vec![HEIGHT_UNSET; 256];
        heights[0] = 0;
        heights[1] = 1;
        heights[2] = 2;
        let mut skipped = 3usize; // S, W, W1 refused
        // oracle frame at h4 continues from W1 (h3) → W (h2): L (h2) is stranded
        let moved = adopt_lineage(
            &sides,
            &mk(22),
            4,
            2,
            6,
            &mut height_off,
            &mut heights,
            &mut skipped,
        );
        assert_eq!(moved, 3, "W + W1 adopted, L stranded");
        assert_eq!(heights[4], 2, "W takes h2");
        assert_eq!(heights[5], 3, "W1 takes h3");
        assert_eq!(heights[2], HEIGHT_UNSET, "L stranded");
        assert_eq!(heights[3], HEIGHT_UNSET, "stale sibling S stays height-less");
        assert_eq!(heights[1], 1, "the thread below the fork is untouched");
        assert_eq!(&height_off[..5], &[10, 11, 300, 400, HEIGHT_GAP]);
        assert_eq!(skipped, 2, "3 refused + 1 stranded - 2 adopted");
        // again (e.g. a later duplicate of the oracle frame): nothing left to do
        let again = adopt_lineage(
            &sides,
            &mk(22),
            4,
            2,
            9,
            &mut height_off,
            &mut heights,
            &mut skipped,
        );
        assert_eq!(again, 0);
        assert_eq!(skipped, 2);
        assert_eq!(&height_off[..5], &[10, 11, 300, 400, HEIGHT_GAP]);
    }
}
