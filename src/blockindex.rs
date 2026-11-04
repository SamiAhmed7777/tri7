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
        self.height_off.get(h as usize).copied()
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
        if self.height_off.is_empty() {
            None
        } else {
            Some(self.height_off.len() as u64 - 1)
        }
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

const FRAME_MAGIC: u32 = 0x0522_3570; // LE [70 35 22 05]
const MAX_FRAME: usize = 8 << 20;

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

    loop {
        match read_exact_or_eof(&mut r, &mut hdr) {
            Ok(false) => break,
            Ok(true) => {}
            Err(e) => return Err(e),
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
        r.read_exact(&mut body)
            .map_err(|e| format!("frame body at offset {}: {}", off, e))?;

        let hash = header_hash(&body).map_err(|e| format!("offset {}: {}", off, e))?;

        // decide accept / skip for HEIGHT threading
        let accepted_here;
        if last_accepted.is_none() {
            genesis_sane(&body)?;
            accepted_here = true; // frame 0 anchors height 0
            height = 0;
        } else {
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
            height_off.push(off);
            heights_by_seq.push(height as u32);
        } else {
            skipped += 1;
            heights_by_seq.push(HEIGHT_UNSET);
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
    if blocks.is_empty() && skipped == 0 && seq == 0 && off == 0 {
        // completely empty file: fall through with a 0-frame build
    } else if blocks.is_empty() {
        return Err("no frames found".into());
    }

    // Heights are usable from genesis up to the height the thread actually
    // REACHED: every in-window accept requires an exact oracle-hash match and
    // every below-window accept requires prev-hash chaining from genesis, so
    // the map is authoritative wherever it is populated. A frame missing ABOVE
    // the reached height (e.g. a body that lived only in a memory cache and
    // was lost on restart) shortens the map — heights below it stay valid and
    // servable (gap recovery appends the missing body later; CACHE_VERSION 2).
    // The old gate (map valid only when the thread ended exactly at the window
    // tip) voided ALL height lookups whenever one tip frame was missing, which
    // blanked every getblock/getblockhash on the node.
    let heights_ok = match oracle {
        Some(o) => height_off.len() as u64 > o.base,
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
    })
}

fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                return if got == 0 || buf[..got].iter().all(|&b| b == 0) {
                    Ok(false)
                } else {
                    Err("EOF mid-frame-header".into())
                }
            }
            Ok(n) => got += n,
            Err(e) => return Err(format!("frame header read: {}", e)),
        }
    }
    Ok(true)
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
const CACHE_VERSION: u32 = 2;
const BLOCKS_HEAD_LEN: usize = 49;
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
    /// false when the blocks file could not be opened writable (perm/ro fs) —
    /// append_block errors cleanly instead of writing
    writable: std::sync::atomic::AtomicBool,
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
        })
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
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .append(true)
                    .open(self.path.as_ref().unwrap())
                    .or_else(|_| File::open(self.path.as_ref().unwrap()))
                    .map_err(|e| format!("open blocks file: {}", e));
                match file {
                    Ok(file) => {
                        // probe writability with a throwaway append handle
                        let writable = std::fs::OpenOptions::new()
                            .append(true)
                            .open(self.path.as_ref().unwrap())
                            .is_ok();
                        eprintln!(
                            "[blockidx] ready: {} frames, {} txs, heights_ok={}, \
                             threaded_tip={:?}, writable={}, built in {}s",
                            b.frames,
                            b.txs_indexed,
                            b.heights_ok,
                            b.blocks.tip_height(),
                            writable,
                            started.elapsed().as_secs()
                        );
                        let inner = StoreInner {
                            blocks: b.blocks,
                            txs: b.txs,
                            heights_ok: b.heights_ok,
                            file,
                            build_secs: started.elapsed().as_secs(),
                            appendix: std::sync::RwLock::new(Vec::new()),
                            writable: std::sync::atomic::AtomicBool::new(writable),
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
    fn read_frame(inner: &StoreInner, off: u64) -> Result<Vec<u8>, String> {
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
    pub fn block_by_hash(&self, hash: &Hash) -> Result<Option<BlockHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        for e in inner.blocks.candidates_by_key(key8(hash)) {
            let body = Self::read_frame(&inner, e.off)?;
            let h = header_hash(&body)?;
            if &h != hash {
                continue; // truncated-key collision — keep scanning candidates
            }
            let block = Self::parse_frame(&body)?;
            let height = inner.blocks.height_of_seq(e.seq);
            return Ok(Some(BlockHit {
                block,
                hash: h,
                off: e.off,
                body_len: body.len(),
                height,
            }));
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

    /// Locate a block by main-chain height (height map authoritative inside
    /// the window, frame-order-derived below it — see module docs). Unavailable
    /// when the build's chain-threading anchor failed.
    pub fn block_by_height(&self, h: u64) -> Result<Option<BlockHit>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        if !inner.heights_ok {
            return Ok(None);
        }
        let off = match inner.blocks.offset_by_height(h) {
            Some(o) => o,
            None => {
                // main height map ended below h — consult the appendix (a
                // gap-recovery append above the threaded tip)
                for f in inner.appendix.read().unwrap().iter() {
                    if f.height == Some(h) {
                        let body = Self::read_frame(&inner, f.off)?;
                        let hash = header_hash(&body)?;
                        let block = Self::parse_frame(&body)?;
                        return Ok(Some(BlockHit {
                            block,
                            hash,
                            off: f.off,
                            body_len: body.len(),
                            height: Some(h),
                        }));
                    }
                }
                return Ok(None);
            }
        };
        let body = Self::read_frame(&inner, off)?;
        let hash = header_hash(&body)?;
        let block = Self::parse_frame(&body)?;
        Ok(Some(BlockHit {
            block,
            hash,
            off,
            body_len: body.len(),
            height: Some(h),
        }))
    }

    /// The main-chain block hash at a height (display hex), read from the
    /// height map's frame header.
    pub fn hash_at_height(&self, h: u64) -> Result<Option<String>, String> {
        let inner = self.inner().ok_or("block index not ready")?;
        if !inner.heights_ok {
            return Ok(None);
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
                if p.iter().any(|(b, _)| header_hash(b).as_ref() == Ok(&hash)) {
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
            return Ok(hash); // already on disk — nothing to do
        }
        if !inner.writable.load(Ordering::SeqCst) {
            return Err(
                "blocks file is not writable (append needs a tri7-owned blk file)".into(),
            );
        }
        let f = &inner.file;
        let off = f
            .metadata()
            .map_err(|e| format!("append: stat: {}", e))?
            .len();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        use std::io::Seek;
        let mut fh = f
            .try_clone()
            .map_err(|e| format!("append: clone handle: {}", e))?;
        fh.seek(std::io::SeekFrom::End(0))
            .map_err(|e| format!("append: seek: {}", e))?;
        fh.write_all(&frame)
            .map_err(|e| format!("append: write: {}", e))?;
        fh.sync_all().map_err(|e| format!("append: sync: {}", e))?;
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
        inner.appendix.write().unwrap().push(AppendedFrame {
            key: key8(&hash),
            off,
            seq,
            height,
            tx_keys,
        });
        Ok(hash)
    }

    /// Drain bodies buffered during the index build into the now-ready store
    /// (FIFO — same append path as direct appends, dedup via block_by_hash).
    fn flush_pending(&self, inner: &Arc<StoreInner>) {
        let drained: Vec<(Vec<u8>, Option<u64>)> = {
            let mut p = self.pending.lock().unwrap();
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
            return Ok(hash); // already on disk — nothing to do
        }
        if !inner.writable.load(Ordering::SeqCst) {
            return Err(
                "blocks file is not writable (append needs a tri7-owned blk file)".into(),
            );
        }
        let f = &inner.file;
        let off = f
            .metadata()
            .map_err(|e| format!("append: stat: {}", e))?
            .len();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body);
        use std::io::Seek;
        let mut fh = f
            .try_clone()
            .map_err(|e| format!("append: clone handle: {}", e))?;
        fh.seek(std::io::SeekFrom::End(0))
            .map_err(|e| format!("append: seek: {}", e))?;
        fh.write_all(&frame)
            .map_err(|e| format!("append: write: {}", e))?;
        fh.sync_all().map_err(|e| format!("append: sync: {}", e))?;
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
        inner.appendix.write().unwrap().push(AppendedFrame {
            key: key8(&hash),
            off,
            seq,
            height,
            tx_keys,
        });
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
}
