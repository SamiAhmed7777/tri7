//! Balances-snapshot builder (lib form of the phase-1e `balances` bin).
//!
//! Replays a blk0001.dat archive genesis..(EOF or first stale-branch stop)
//! over the shared ArchiveWalker, maintaining the live UTXO set with
//! per-output address payloads, and writes the balances.json snapshot
//! (optionally the wallet_utxos.json spend dump) ATOMICALLY — tmp file +
//! rename, so an RPC verb or the GUI can read the previous generation at any
//! moment and a torn half-written snapshot can never be served.
//!
//! Consumers:
//!   • bin/balances.rs   — the standalone CLI (delegates here, prints)
//!   • bin/follow_tip.rs — the `--balances` builder thread: a node started
//!     with an explicit `--balances PATH` maintains the snapshot in its own
//!     state dir, which is what the native `richlist` verb serves (item:
//!     every node IS an explorer; the snapshot build runs on a background
//!     thread and touches NO daemon locks — explorer pressure stays
//!     separable from consensus-critical flow).
//!
//! The JSON layout is byte-compatible with the historical bin output:
//! {height, covered_tip_hash, totals{...}, wallet[], balances{}, received_regular{}}.
//!
//! REPLAY ORDER (item 48, live-caught on DNS2 2026-11-24). The file-order
//! walk is FIRST-WRITER-WINS: at a fork whose loser reached the archive
//! before the winner (h2208645, loser `02d723be…` stored one frame ahead of
//! the canonical child of the same parent) the walk APPLIES the loser and
//! reports the winner's first frame as a stale branch. balances.json then
//! tracks the losing branch, and no retry can ever change that — every
//! ~100 s replay re-reads 2.2M frames to the same wrong answer. A caller
//! that owns the chain's own map therefore sets [`BalancesBuildOpts::canonical`]
//! and the replay becomes INDEX-ORDERED: heights 0,1,2,… are resolved through
//! the node's block index (chain-slot overlay → build height map → appendix),
//! every body is chain-checked against the one before it, and the walk stops
//! at the first height the store cannot serve. One accounting pass serves
//! both orders, so a fork-free archive yields byte-identical output either
//! way (pinned by test).

use std::collections::HashMap;
use std::path::Path;

use crate::address::{payload_to_address, pubkey_payload};
use crate::consensus;
use crate::kernel::UtxoSet;
use crate::replay::{check_coverage, ArchiveWalker, Step};
use crate::wallet;

/// One live wallet-owned UTXO (spend record for the wallet_utxos.json dump).
struct WalletUtxo {
    value: i64,
    script: Vec<u8>,
    height: u32,
    tx_time: u32,
    from_coinbase: bool,
    from_coinstake: bool,
}

/// The chain's own answer to "which block is at height h" — implemented by
/// the node's [`crate::blockindex::BlockStore`], whose resolution order is
/// the live chain's registered slot, then the build-time height map, then
/// the first-arrival appendix, and which serves nothing above the chain tip.
///
/// `Ok(None)` means the store holds no chain body for that height: a hole,
/// or the top of what it holds.
pub trait CanonicalBlocks {
    fn block_at(
        &self,
        height: u64,
    ) -> Result<Option<(crate::legacy::Block, crate::legacy::Hash)>, String>;
    /// The top of the chain this source can vouch for, when it knows: the
    /// live registered tip of a node's store, else the highest body it holds.
    /// `block_at` serving nothing AT OR BELOW this height is a HOLE; nothing
    /// above it is simply the end of the chain.
    fn chain_top(&self) -> Option<u64> {
        None
    }
}

impl CanonicalBlocks for crate::blockindex::BlockStore {
    fn block_at(
        &self,
        height: u64,
    ) -> Result<Option<(crate::legacy::Block, crate::legacy::Hash)>, String> {
        Ok(self
            .block_by_height(height)?
            .map(|hit| (hit.block, hit.hash)))
    }
    fn chain_top(&self) -> Option<u64> {
        // the registered chain tip when the chain is armed (bodies stored
        // above it were raced, not chain); else the disk-bodies oracle
        self.chain_slot_tip().or_else(|| self.threaded_tip())
    }
}

pub struct BalancesBuildOpts<'a> {
    /// blk0001.dat to replay.
    pub blk: &'a Path,
    /// INDEX-ORDERED replay through the chain's own map (see the module
    /// note). `None` keeps the historical FILE-ORDER replay — correct for a
    /// fork-free archive, and for a forked one only when no loser sits ahead
    /// of the winner in the file.
    pub canonical: Option<&'a dyn CanonicalBlocks>,
    /// balances.json destination (written atomically).
    pub out: &'a Path,
    /// wallet.dat for the wallet slice (None = build chain-wide only; the
    /// snapshot then carries `"wallet": []`).
    pub wallet: Option<&'a Path>,
    /// wallet_utxos.json destination (written atomically; needs `wallet`).
    /// The daemon builder ALWAYS pairs this with `out` when a wallet is
    /// configured so the listunspent coverage gate (height AND hash must
    /// match across both snapshots) stays green after a rebuild.
    pub utxos_out: Option<&'a Path>,
    /// coverage contract end height (usize::MAX = whatever the walk reaches).
    pub end_height: usize,
    /// The tip this replay is CHASING. The snapshot publishes it, and the
    /// RPC's completeness rule compares the snapshot's coverage against it:
    /// a replay that stopped short of the tip it was chasing was capped by
    /// a hole in the store, and its figure omits everything above the cap.
    ///
    /// This is deliberately NOT `end_height`: end_height is where the walk
    /// is ALLOWED to stop (0 = walk to EOF), while this is where it was
    /// TRYING to get to. They differ when the store's last body sits below
    /// the tip.
    pub replay_target: Option<usize>,
    /// accept a stale-branch stop as full coverage.
    pub allow_stale: bool,
}

#[derive(Debug, Clone)]
pub struct BalancesBuildReport {
    pub blocks: usize,
    pub pos_blocks: usize,
    pub covered_to: usize,
    pub tip_hash_display: String,
    pub live_utxos: u64,
    pub total_balance_sats: i64,
    pub attributed_sats: i64,
    pub unattributed_sats: i64,
    pub unattributed_utxos: u64,
    pub outputs_processed: u64,
    pub outputs_attributed: u64,
    pub regular_txs: u64,
    pub untracked_spends: usize,
    pub duplicate_txids: usize,
    pub wallet_key_count: usize,
    pub wallet_addresses_distinct: usize,
    pub wallet_funded_addresses: usize,
    pub wallet_balance_sats: i64,
    pub wallet_utxos: u64,
    pub wallet_received_regular_sats: i64,
    pub wallet_received_all_sats: i64,
    pub out_path: std::path::PathBuf,
    pub utxos_out_path: Option<std::path::PathBuf>,
    pub utxos_count: usize,
    pub utxos_total_sats: i64,
    /// `"index"` when the replay was index-ordered, `"file"` when it walked
    /// the archive in file order.
    pub replay_order: &'static str,
    /// Why an INDEX-ordered replay stopped short of the store's top: the
    /// store served a body that does not chain onto the one before it (the
    /// walk refuses it rather than splice two branches together), or holds
    /// nothing at a height it does hold a body above (a hole). File-order
    /// walks leave this None (their stop is the walker's stale branch).
    pub cap_note: Option<String>,
}

/// What the block source handed the accounting pass.
enum Pulled {
    /// the chain's block at the next height
    Block(usize, crate::legacy::Block, crate::legacy::Hash),
    /// the source cannot deliver the chain at this height: file order = a
    /// stale-branch stop; index order = a hole in the store, or an index
    /// row that does not chain. Nothing at or above it is applied.
    Capped(usize),
    Eof,
}

/// One INDEX-ORDERED pull: the chain's block at height `h`, chain-checked
/// against `prev` (the block the walk applied just before). `Ok(None)` from
/// the source is the END of the chain unless the source says its chain
/// continues above `h` — then it is a HOLE and the walk is capped there.
fn pull_indexed(
    chain: &dyn CanonicalBlocks,
    h: usize,
    prev: Option<crate::legacy::Hash>,
    cap_note: &mut Option<String>,
) -> Result<Pulled, String> {
    match chain.block_at(h as u64) {
        Err(e) => Err(format!("chain store at h{h}: {e}")),
        Ok(Some((block, hash))) => Ok(chain_checked(h, block, hash, prev, cap_note)),
        Ok(None) => {
            let continues = match chain.chain_top() {
                Some(top) => h as u64 <= top,
                // the source does not say where its chain ends: probe one
                // height up
                None => chain
                    .block_at(h as u64 + 1)
                    .map_err(|e| format!("chain store at h{}: {e}", h + 1))?
                    .is_some(),
            };
            if !continues {
                return Ok(Pulled::Eof);
            }
            // the source says its chain continues above h — unless it simply
            // ADVANCED between the two questions: ask once more before
            // calling it a hole.
            match chain.block_at(h as u64) {
                Err(e) => Err(format!("chain store at h{h}: {e}")),
                Ok(Some((block, hash))) => Ok(chain_checked(h, block, hash, prev, cap_note)),
                Ok(None) => {
                    *cap_note = Some(format!(
                        "the chain store holds nothing at h{h} although its chain continues above it"
                    ));
                    Ok(Pulled::Capped(h))
                }
            }
        }
    }
}

/// Chain check for an index-served body: the genesis must carry a zero prev
/// hash, every other block must name the block the walk applied just before
/// it. Anything else is refused — the walk ends rather than splices.
fn chain_checked(
    h: usize,
    block: crate::legacy::Block,
    hash: crate::legacy::Hash,
    prev: Option<crate::legacy::Hash>,
    cap_note: &mut Option<String>,
) -> Pulled {
    let links = if h == 0 {
        block.prev_hash == [0u8; 32]
    } else {
        Some(block.prev_hash) == prev
    };
    if links {
        return Pulled::Block(h, block, hash);
    }
    *cap_note = Some(if h == 0 {
        format!(
            "the chain store's block at h0 ({}) does not have a zero prev hash",
            crate::legacy::hash_to_display(&hash)
        )
    } else {
        format!(
            "the chain store's block at h{h} ({}) does not chain onto the block it served at h{} ({})",
            crate::legacy::hash_to_display(&hash),
            h - 1,
            prev
                .map(|p| crate::legacy::hash_to_display(&p))
                .unwrap_or_default()
        )
    });
    Pulled::Capped(h)
}

/// Replay + aggregate + write. `progress` is called every 100k blocks with
/// (blocks_replayed, live_utxos) so long builds can print heartbeat lines.
pub fn build_balances_snapshot(
    opts: &BalancesBuildOpts,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<BalancesBuildReport, String> {
    // ---- wallet: every address derivable from the live keys ----------------
    let (wallet_payloads, wall_key_count) = match opts.wallet {
        Some(wp_path) => {
            let wall = wallet::read_wallet(wp_path)
                .map_err(|e| format!("wallet read failed: {}", e))?;
            if wall.keys.is_empty() {
                return Err("wallet has zero keys".to_string());
            }
            // (payload, canonical address string) per key — payloads may in
            // principle collide (same key imported twice); distinct payloads
            // are what matters.
            let mut payloads: Vec<([u8; 21], String)> = wall
                .keys
                .iter()
                .map(|k| (pubkey_payload(&k.pubkey), k.address.clone()))
                .collect();
            for (payload, addr) in &payloads {
                debug_assert_eq!(&payload_to_address(payload), addr);
            }
            payloads.sort();
            payloads.dedup();
            let n = wall.keys.len();
            (payloads, n)
        }
        None => (Vec::new(), 0),
    };

    // ---- replay ------------------------------------------------------------

    // (item 48) the block source. Both orders feed ONE accounting pass below,
    // so a fork-free archive produces the same snapshot either way.
    let mut walker = None;
    if opts.canonical.is_none() {
        walker = Some(
            ArchiveWalker::open(opts.blk.to_str().ok_or("blk path not UTF-8")?)
                .map_err(|e| format!("open blk file: {}", e))?,
        );
    }
    let mut index_prev: Option<crate::legacy::Hash> = None;
    let mut cap_note: Option<String> = None;
    let mut utxos = UtxoSet::new();
    // daemon getreceivedbyaddress semantics (regular txs, P2PKH scripts only)
    let mut received_regular: HashMap<[u8; 21], i64> = HashMap::new();
    // diagnostic: every non-empty output with a destination, any tx kind
    let mut received_all: HashMap<[u8; 21], i64> = HashMap::new();

    let mut stale_at: Option<usize> = None;
    let mut blocks = 0usize;
    let mut pos_blocks = 0usize;
    let mut outputs_processed = 0u64; // every non-empty vout seen on the live chain
    let mut outputs_attributed = 0u64; // ...whose script resolves to an address
    let mut regular_txs = 0u64;
    let mut untracked_spends = 0usize;
    let mut duplicate_txids = 0usize;
    let mut tip_hash = [0u8; 32];
    // wallet-owned live UTXOs for the --utxos-out dump. Spend removal is
    // PER-VIN (see the pass below for why it must NOT mirror the global
    // set's per-tx all-or-nothing rule), IsEmpty never tracked.
    let mut wallet_utxo_set: HashMap<(crate::legacy::Hash, u32), WalletUtxo> = HashMap::new();
    let wallet_payload_set: std::collections::HashSet<[u8; 21]> =
        wallet_payloads.iter().map(|(p, _)| *p).collect();

    loop {
        let pulled = match (opts.canonical, walker.as_mut()) {
            // INDEX ORDER: height `blocks` from the chain's own map, refused
            // unless it chains onto the block before it. A chain that flips
            // mid-walk (reorg) or an index row that disagrees with its
            // neighbours ends the walk HERE — never a splice of two branches.
            (Some(chain), _) => {
                let pulled = pull_indexed(chain, blocks, index_prev, &mut cap_note)?;
                if let Pulled::Block(_, _, hash) = &pulled {
                    index_prev = Some(*hash);
                }
                pulled
            }
            // FILE ORDER: the historical first-writer-wins archive walk.
            (None, Some(w)) => match w.step() {
                Err(e) => return Err(e),
                Ok(Step::Eof) => Pulled::Eof,
                Ok(Step::StaleBranch(h)) => Pulled::Capped(h),
                Ok(Step::Block(h, b, hash)) => Pulled::Block(h, b, hash),
            },
            (None, None) => unreachable!("file-order replay without a walker"),
        };
        let (_height, block, hash) = match pulled {
            Pulled::Eof => break,
            Pulled::Capped(h) => {
                stale_at = Some(h);
                break;
            }
            Pulled::Block(h, b, hash) => (h, b, hash),
        };

        for tx in &block.vtx {
            let is_coinbase = consensus::tx_is_coin_base(tx);
            let is_coinstake = consensus::tx_is_coin_stake(tx);
            if !is_coinbase && !is_coinstake {
                regular_txs += 1;
            }
            for vout in &tx.vout {
                // CTxOut::IsEmpty(): the coinstake vout[0] marker (value 0,
                // empty script) creates no UTXO and carries no address.
                if vout.value == 0 && vout.script_pubkey.is_empty() {
                    continue;
                }
                outputs_processed += 1;
                let dest = match crate::address::script_to_dest(&vout.script_pubkey) {
                    Some(d) => d,
                    None => continue,
                };
                outputs_attributed += 1;
                let payload = dest.payload();
                *received_all.entry(payload).or_insert(0) += vout.value;
                // getreceivedbyaddress: coinbase/coinstake txs are skipped
                // outright; P2PKH is the only form its script equality hits.
                if !is_coinbase && !is_coinstake && dest.is_pubkey_hash() {
                    *received_regular.entry(payload).or_insert(0) += vout.value;
                }
            }
        }

        // wallet-owned UTXO lifecycle. Spend removal is PER-VIN, applied
        // whenever the vin names the coin — NOT gated on every vin of the
        // tx resolving in the global set. The historical all-or-nothing
        // gate (mirroring UtxoSet::apply_block_reported) STRANDED coins:
        // any tx carrying one unresolvable vin (the chain's re-spend
        // history — e.g. DNS2 block h2206958's 8812990e spends b14a219d:1
        // alongside 88aa59ec:1, which the chain had ALREADY spent at
        // h2206940) skipped removal for its WHOLE vin list, so every
        // wallet-owned coin that tx also spent stayed in the dump forever
        // as a ghost the merge could never see. The global set keeps its
        // own all-or-nothing semantics (balances.json/moneysupply parity);
        // the dump's contract is different — SPENDABLE coins — and a
        // re-spent coin fails at accept time, so canonical spentness is
        // the truth here, exactly like the addrindex's insert-vs-spend and
        // WalletLive::apply_block's per-vin bookkeeping.
        for tx in &block.vtx {
            let is_coinbase = consensus::tx_is_coin_base(tx);
            let is_coinstake = consensus::tx_is_coin_stake(tx);
            if !is_coinbase {
                for vin in &tx.vin {
                    wallet_utxo_set.remove(&(vin.prev_hash, vin.prev_n));
                }
            }
            for (n, vout) in tx.vout.iter().enumerate() {
                if vout.value == 0 && vout.script_pubkey.is_empty() {
                    continue; // IsEmpty marker
                }
                let Some(dest) = crate::address::script_to_dest(&vout.script_pubkey) else {
                    continue;
                };
                let payload = dest.payload();
                if wallet_payload_set.contains(&payload) {
                    wallet_utxo_set.insert(
                        (tx.hash(), n as u32),
                        WalletUtxo {
                            value: vout.value,
                            script: vout.script_pubkey.clone(),
                            height: _height as u32,
                            tx_time: tx.time,
                            from_coinbase: is_coinbase,
                            from_coinstake: is_coinstake,
                        },
                    );
                }
            }
        }

        let rep = utxos.apply_block_reported(&block, _height);
        untracked_spends += rep.untracked_spends;
        duplicate_txids += rep.duplicate_txids;

        blocks += 1;
        tip_hash = hash;
        if consensus::block_is_proof_of_stake(&block) {
            pos_blocks += 1;
        }
        if blocks % 100_000 == 0 {
            progress(blocks, utxos.len());
        }
    }

    // ---- coverage contract (shared skeleton) --------------------------------
    // File order: the walker's own count (identical to `blocks` — it counts
    // the same delivered blocks). Index order: heights are strictly
    // sequential from 0, so `blocks` blocks cover 0..=blocks-1.
    let covered_to = match walker.as_ref() {
        Some(w) => w.height().saturating_sub(1),
        None => blocks.saturating_sub(1),
    };
    if blocks == 0 && opts.canonical.is_some() {
        return Err(match &cap_note {
            Some(note) => format!("index-ordered replay: nothing was applied — {note}"),
            None => "index-ordered replay: the chain store served no body at height 0 — nothing replayed"
                .to_string(),
        });
    }
    if outputs_processed == 0 {
        return Err(format!(
            "zero outputs processed over heights 0..{} — no coverage",
            covered_to
        ));
    }
    check_coverage(stale_at, covered_to, opts.end_height, opts.allow_stale)?;

    // ---- aggregate ----------------------------------------------------------
    let totals = utxos.address_balances();
    let attributed_sats: i64 = totals.per_address.values().map(|(v, _)| v).sum();
    let attributed_utxos: u64 = totals.per_address.values().map(|(_, n)| n).sum();
    let total_balance_sats = attributed_sats + totals.unattributed_sats;
    let live_utxos = attributed_utxos + totals.unattributed_utxos;

    // wallet slice
    let mut wallet_balance_sats = 0i64;
    let mut wallet_utxos = 0u64;
    let mut wallet_received_regular_sats = 0i64;
    let mut wallet_received_all_sats = 0i64;
    let mut funded_wallet_addresses = 0usize;
    let mut wallet_rows: Vec<[String; 5]> = Vec::with_capacity(wallet_payloads.len());
    for (payload, addr) in &wallet_payloads {
        let (bal, n) = totals.per_address.get(payload).copied().unwrap_or((0, 0));
        let rec_r = received_regular.get(payload).copied().unwrap_or(0);
        let rec_a = received_all.get(payload).copied().unwrap_or(0);
        wallet_balance_sats += bal;
        wallet_utxos += n;
        wallet_received_regular_sats += rec_r;
        wallet_received_all_sats += rec_a;
        if bal != 0 || rec_r != 0 || rec_a != 0 {
            funded_wallet_addresses += 1;
        }
        wallet_rows.push([
            addr.clone(),
            bal.to_string(),
            n.to_string(),
            rec_r.to_string(),
            rec_a.to_string(),
        ]);
    }

    // ---- JSON ----------------------------------------------------------------
    // {"totals":{...}, "wallet":[...], "balances":{addr:sats},
    //  "received_regular":{addr:sats}} — sorted keys, sats everywhere.
    let mut balances_sorted: Vec<(&[u8; 21], &(i64, u64))> = totals.per_address.iter().collect();
    balances_sorted.sort_by_key(|(p, _)| payload_to_address(p));
    let mut received_sorted: Vec<(&[u8; 21], &i64)> = received_regular.iter().collect();
    received_sorted.sort_by_key(|(p, _)| payload_to_address(p));

    let mut j = String::with_capacity(1 << 22);
    j.push_str("{\n");
    j.push_str(&format!("  \"height\": {},\n", covered_to));
    // The tip the replay was CHASING. The build refuses to produce a
    // snapshot that stopped short of it (check_coverage), so a snapshot
    // whose height equals this target is a whole-chain figure; one whose
    // height is lower was capped by a hole in the store.
    let replay_target = opts
        .replay_target
        .or(if opts.end_height > 0 { Some(opts.end_height) } else { None });
    if let Some(t) = replay_target {
        j.push_str(&format!("  \"replay_target_height\": {},\n", t));
    }
    j.push_str(&format!(
        "  \"covered_tip_hash\": \"{}\",\n",
        crate::legacy::hash_to_display(&tip_hash)
    ));
    j.push_str("  \"totals\": {\n");
    j.push_str(&format!("    \"blocks_replayed\": {},\n", blocks));
    j.push_str(&format!("    \"pos_blocks\": {},\n", pos_blocks));
    j.push_str(&format!("    \"live_utxos\": {},\n", live_utxos));
    j.push_str(&format!(
        "    \"total_balance_sats\": {},\n",
        total_balance_sats
    ));
    j.push_str(&format!(
        "    \"attributed_balance_sats\": {},\n",
        attributed_sats
    ));
    j.push_str(&format!(
        "    \"unattributed_balance_sats\": {},\n",
        totals.unattributed_sats
    ));
    j.push_str(&format!(
        "    \"unattributed_utxos\": {},\n",
        totals.unattributed_utxos
    ));
    j.push_str(&format!(
        "    \"outputs_processed\": {},\n",
        outputs_processed
    ));
    j.push_str(&format!(
        "    \"outputs_attributed_to_address\": {},\n",
        outputs_attributed
    ));
    j.push_str(&format!("    \"regular_txs\": {},\n", regular_txs));
    j.push_str(&format!(
        "    \"untracked_spends\": {},\n",
        untracked_spends
    ));
    j.push_str(&format!("    \"duplicate_txids\": {},\n", duplicate_txids));
    j.push_str(&format!("    \"wallet_key_count\": {},\n", wall_key_count));
    j.push_str(&format!(
        "    \"wallet_addresses_distinct\": {},\n",
        wallet_payloads.len()
    ));
    j.push_str(&format!(
        "    \"wallet_funded_addresses\": {},\n",
        funded_wallet_addresses
    ));
    j.push_str(&format!(
        "    \"wallet_balance_sats\": {},\n",
        wallet_balance_sats
    ));
    j.push_str(&format!("    \"wallet_utxos\": {},\n", wallet_utxos));
    j.push_str(&format!(
        "    \"wallet_received_regular_sats\": {},\n",
        wallet_received_regular_sats
    ));
    j.push_str(&format!(
        "    \"wallet_received_all_sats\": {}\n",
        wallet_received_all_sats
    ));
    j.push_str("  },\n");
    j.push_str("  \"wallet\": [\n");
    for (i, row) in wallet_rows.iter().enumerate() {
        j.push_str(&format!(
            "    {{\"address\": \"{}\", \"balance_sats\": {}, \"utxos\": {}, \"received_regular_sats\": {}, \"received_all_sats\": {}}}{}\n",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            if i + 1 == wallet_rows.len() { "" } else { "," }
        ));
    }
    j.push_str("  ],\n");
    j.push_str("  \"balances\": {");
    for (i, (payload, (bal, _))) in balances_sorted.iter().enumerate() {
        j.push_str(&format!(
            "{}\"{}\":{}",
            if i % 6 == 0 { "\n    " } else { " " },
            payload_to_address(payload),
            bal
        ));
        if i + 1 != balances_sorted.len() {
            j.push(',');
        }
    }
    j.push_str("\n  },\n");
    j.push_str("  \"received_regular\": {");
    for (i, (payload, sats)) in received_sorted.iter().enumerate() {
        j.push_str(&format!(
            "{}\"{}\":{}",
            if i % 6 == 0 { "\n    " } else { " " },
            payload_to_address(payload),
            sats
        ));
        if i + 1 != received_sorted.len() {
            j.push(',');
        }
    }
    j.push_str("\n  }\n}\n");
    write_atomic(opts.out, j.as_bytes())?;

    // ---- wallet UTXO dump (phase 3) ------------------------------------------
    let mut utxos_count = 0usize;
    let mut utxos_total_sats = 0i64;
    if let Some(dump_path) = opts.utxos_out {
        let mut rows: Vec<(crate::legacy::Hash, u32, &WalletUtxo)> =
            wallet_utxo_set.iter().map(|((t, n), u)| (*t, *n, u)).collect();
        rows.sort_by(|a, b| {
            (a.2.height, crate::legacy::hash_to_display(&a.0), a.1).cmp(&(
                b.2.height,
                crate::legacy::hash_to_display(&b.0),
                b.1,
            ))
        });
        let mut d = String::with_capacity(1 << 20);
        d.push_str("{\n");
        d.push_str(&format!("  \"height\": {},\n", covered_to));
        // coverage tip hash: required by the tri7d listunspent consistency
        // gate (height AND hash must match the balances snapshot)
        d.push_str(&format!(
            "  \"covered_tip_hash\": \"{}\",\n",
            crate::legacy::hash_to_display(&tip_hash)
        ));
        d.push_str(&format!("  \"count\": {},\n", rows.len()));
        utxos_total_sats = rows.iter().map(|(_, _, u)| u.value).sum();
        d.push_str(&format!("  \"total_value_sats\": {},\n", utxos_total_sats));
        d.push_str("  \"utxos\": [\n");
        for (i, (txid, n, u)) in rows.iter().enumerate() {
            let payload = crate::address::script_to_dest(&u.script).map(|dest| dest.payload());
            let addr = payload
                .map(|p| payload_to_address(&p))
                .unwrap_or_else(|| "".to_string());
            let kind = if u.from_coinbase {
                "coinbase"
            } else if u.from_coinstake {
                "coinstake"
            } else {
                "regular"
            };
            d.push_str(&format!(
                "    {{\"txid\": \"{}\", \"n\": {}, \"value_sats\": {}, \"script_hex\": \"{}\", \
                 \"address\": \"{}\", \"height\": {}, \"tx_time\": {}, \"prev_kind\": \"{}\"}}{}\n",
                crate::legacy::hash_to_display(txid),
                n,
                u.value,
                u.script
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<String>(),
                addr,
                u.height,
                u.tx_time,
                kind,
                if i + 1 == rows.len() { "" } else { "," }
            ));
        }
        d.push_str("  ]\n}\n");
        write_atomic(dump_path, d.as_bytes())?;
        utxos_count = rows.len();
    }

    Ok(BalancesBuildReport {
        blocks,
        pos_blocks,
        covered_to,
        tip_hash_display: crate::legacy::hash_to_display(&tip_hash),
        live_utxos,
        total_balance_sats,
        attributed_sats,
        unattributed_sats: totals.unattributed_sats,
        unattributed_utxos: totals.unattributed_utxos,
        outputs_processed,
        outputs_attributed,
        regular_txs,
        untracked_spends,
        duplicate_txids,
        wallet_key_count: wall_key_count,
        wallet_addresses_distinct: wallet_payloads.len(),
        wallet_funded_addresses: funded_wallet_addresses,
        wallet_balance_sats,
        wallet_utxos,
        wallet_received_regular_sats,
        wallet_received_all_sats,
        out_path: opts.out.to_path_buf(),
        utxos_out_path: opts.utxos_out.map(|p| p.to_path_buf()),
        utxos_count,
        utxos_total_sats,
        replay_order: if opts.canonical.is_some() { "index" } else { "file" },
        cap_note,
    })
}

/// Write `bytes` to `path` atomically: pid-unique `<path>.tmp<pid>` + rename,
/// so a concurrent reader (richlist verb, GUI snapshot fallback) always sees
/// one complete generation — never a torn half-written file — and two
/// builders in different processes (daemon thread + standalone bin) can never
/// collide on one tmp file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {}: {}", path.display(), e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{Block, Tx, Vin, Vout};

    const MAGIC: u32 = 0x05223570;

    fn p2pkh(payload20: [u8; 20]) -> Vec<u8> {
        let mut s = vec![0x76u8, 0xa9, 0x14];
        s.extend_from_slice(&payload20);
        s.push(0x88);
        s.push(0xac);
        s
    }

    fn coinbase(pay20: [u8; 20], value: i64) -> Tx {
        Tx {
            version: 1,
            time: 1_400_000_000,
            vin: vec![Vin {
                prev_hash: [0u8; 32],
                prev_n: u32::MAX,
                script_sig: vec![0x01, 0x42],
                sequence: u32::MAX,
            }],
            vout: vec![Vout {
                value,
                script_pubkey: p2pkh(pay20),
            }],
            locktime: 0,
        }
    }

    /// Blocks: genesis pays 100 to A; h1 mints 50 to B and spends A's
    /// coinbase 40 to C with 60 change back to A.
    fn chain() -> Vec<Block> {
        let a = [0x11u8; 20];
        let b = [0x22u8; 20];
        let c = [0x33u8; 20];
        let mut genesis = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0xAAu8; 32],
            time: 1_400_000_000,
            bits: 0x1e0fffff,
            nonce: 0,
            vtx: vec![coinbase(a, 100)],
            block_sig: Vec::new(),
        };
        genesis.merkle_root = sha256d_for_merkle(&genesis.vtx);
        let g_hash = genesis.hash();
        let spend = Tx {
            version: 1,
            time: 1_400_000_600,
            vin: vec![Vin {
                prev_hash: genesis.vtx[0].hash(),
                prev_n: 0,
                script_sig: vec![0x00],
                sequence: u32::MAX,
            }],
            vout: vec![
                Vout {
                    value: 40,
                    script_pubkey: p2pkh(c),
                },
                Vout {
                    value: 60,
                    script_pubkey: p2pkh(a),
                },
            ],
            locktime: 0,
        };
        let mut b1 = Block {
            version: 1,
            prev_hash: g_hash,
            merkle_root: [0xBBu8; 32],
            time: 1_400_000_600,
            bits: 0x1e0fffff,
            nonce: 0,
            vtx: vec![coinbase(b, 50), spend],
            block_sig: Vec::new(),
        };
        b1.merkle_root = sha256d_for_merkle(&b1.vtx);
        vec![genesis, b1]
    }

    /// The walker does not verify the merkle root, but keep it well-formed
    /// (sha256d of the concatenated little-endian tx hashes) so the fixture
    /// stays a plausible chain for any future stricter reader.
    fn sha256d_for_merkle(vtx: &[Tx]) -> [u8; 32] {
        let mut buf = Vec::new();
        for tx in vtx {
            buf.extend_from_slice(&tx.hash());
        }
        crate::legacy::sha256d(&buf)
    }

    fn write_blk_file(path: &Path, blocks: &[Block]) {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        for b in blocks {
            let bytes = b.serialize();
            f.write_all(&MAGIC.to_le_bytes()).unwrap();
            f.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
            f.write_all(&bytes).unwrap();
        }
        f.flush().unwrap();
    }

    fn tmp_dir(name: &str) -> crate::testsupport::TestDir {
        crate::testsupport::test_dir(&format!("tri7-balances-build-{name}"))
    }

    fn run_build(dir: &Path, blocks: &[Block], end_height: usize, allow_stale: bool) -> BalancesBuildReport {
        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, blocks);
        let out = dir.join("balances.json");
        let opts = BalancesBuildOpts {
            blk: &blk,
            // the standalone CLI has no chain map: file order
            canonical: None,
            out: &out,
            wallet: None,
            utxos_out: None,
            end_height,
            replay_target: if end_height > 0 { Some(end_height) } else { None },
            allow_stale,
        };
        let mut progress = |_: usize, _: usize| {};
        build_balances_snapshot(&opts, &mut progress).expect("build ok")
    }

    #[test]
    fn builds_snapshot_and_reaches_end_height() {
        let dir = tmp_dir("reach");
        let rep = run_build(&dir, &chain(), 1, false);
        assert_eq!(rep.blocks, 2);
        assert_eq!(rep.covered_to, 1);
        assert_eq!(rep.total_balance_sats, 150);
        assert_eq!(rep.live_utxos, 3); // A change, B coinbase, C payment
        assert_eq!(rep.regular_txs, 1);
        assert_eq!(rep.wallet_key_count, 0);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(rep.out_path).unwrap()).unwrap();
        assert_eq!(v["height"], 1);
        assert_eq!(v["totals"]["total_balance_sats"], 150);
        let balances = v["balances"].as_object().unwrap();
        assert_eq!(balances.len(), 3, "A, B, C all live");
        // regular-tx receipts: C 40, A change 60; coinbase-only B never counts
        let recv = v["received_regular"].as_object().unwrap();
        let total_recv: i64 = recv.values().map(|x| x.as_i64().unwrap()).sum();
        assert_eq!(total_recv, 100, "40 to C + 60 change to A; coinbase excluded");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_branch_requires_allow_stale_flag() {
        let dir = tmp_dir("stale");
        let mut blocks = chain();
        // a frame that does not chain: the walk stops StaleBranch(2)
        let mut orphan = blocks[1].clone();
        orphan.prev_hash = [0x77u8; 32];
        blocks.push(orphan);
        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, &blocks);
        let out = dir.join("balances.json");
        let mk = |allow_stale: bool| BalancesBuildOpts {
            blk: &blk,
            canonical: None,
            out: &out,
            wallet: None,
            utxos_out: None,
            end_height: 1,
            replay_target: Some(1),
            allow_stale,
        };
        let mut progress = |_: usize, _: usize| {};
        assert!(build_balances_snapshot(&mk(false), &mut progress)
            .err()
            .unwrap()
            .contains("allow-stale"));
        let rep = build_balances_snapshot(&mk(true), &mut progress).expect("stale allowed");
        assert_eq!(rep.covered_to, 1, "coverage stops before the stale frame");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incomplete_replay_fails_coverage() {
        let dir = tmp_dir("shortcov");
        let blocks = chain();
        let err = {
            let blk = dir.join("blk0001.dat");
            write_blk_file(&blk, &blocks[..1]); // genesis only
            let out = dir.join("balances.json");
            let opts = BalancesBuildOpts {
                blk: &blk,
                canonical: None,
                out: &out,
                wallet: None,
                utxos_out: None,
                end_height: 1,
            replay_target: Some(1), // unreachable with only h0 present
                allow_stale: false,
            };
            let mut progress = |_: usize, _: usize| {};
            build_balances_snapshot(&opts, &mut progress).err().unwrap()
        };
        assert!(err.contains("incomplete coverage"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_no_tmp_left_behind() {
        let dir = tmp_dir("atomic");
        let rep = run_build(&dir, &chain(), 1, false);
        assert!(rep.out_path.exists());
        // pid-unique tmp (write_atomic) must be renamed away
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp file must be renamed away, found: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minimal-but-valid TRI wallet container with ONE key (the fixture the
    /// item-31 regression below spends from). Mirrors the daemon's own
    /// bootstrap_wallet_file rows: pragmas, main kv table, version row,
    /// genesis bestblock locator, then the key row via the same encoders
    /// wallet.rs's writer uses.
    fn wallet_fixture(dir: &Path, secret: [u8; 32]) -> std::path::PathBuf {
        use tri7_core::wallet::{CLIENT_VERSION, WALLET_APP_ID, WALLET_USER_VERSION};
        let wpath = dir.join("wallet.dat");
        let conn = rusqlite::Connection::open(&wpath).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA application_id = {};\nPRAGMA user_version = {};",
            WALLET_APP_ID, WALLET_USER_VERSION
        ))
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);",
        )
        .unwrap();
        let mut k: Vec<u8> = Vec::new();
        k.push(7u8);
        k.extend_from_slice(b"version");
        let kv = CLIENT_VERSION.to_le_bytes().to_vec();
        conn.execute(
            "INSERT INTO main (key, value) VALUES (?1, ?2)",
            [&k, &kv],
        )
        .unwrap();
        let mut bk: Vec<u8> = Vec::new();
        bk.push(9u8);
        bk.extend_from_slice(b"bestblock");
        let mut bv: Vec<u8> = Vec::new();
        bv.extend_from_slice(&CLIENT_VERSION.to_le_bytes());
        bv.push(1u8);
        bv.extend_from_slice(&[0u8; 32]);
        conn.execute(
            "INSERT INTO main (key, value) VALUES (?1, ?2)",
            [&bk, &bv],
        )
        .unwrap();
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk33 = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
        let der = tri7_core::wallet::encode_der_privkey(&secret, &pk33).unwrap();
        let kb = tri7_core::wallet::encode_key_keyblob(&pk33);
        let kvv = tri7_core::wallet::encode_key_value(&der);
        conn.execute(
            "INSERT INTO main (key, value) VALUES (?1, ?2)",
            [&kb, &kvv],
        )
        .unwrap();
        wpath
    }

    fn pay_tx(vins: Vec<(crate::legacy::Hash, u32)>, outs: Vec<([u8; 20], i64)>) -> Tx {
        Tx {
            version: 1,
            time: 1_400_000_700,
            vin: vins
                .into_iter()
                .map(|(prev_hash, prev_n)| Vin {
                    prev_hash,
                    prev_n,
                    script_sig: vec![0x00],
                    sequence: u32::MAX,
                })
                .collect(),
            vout: outs
                .into_iter()
                .map(|(pay, value)| Vout {
                    value,
                    script_pubkey: p2pkh(pay),
                })
                .collect(),
            locktime: 0,
        }
    }

    fn block(prev: crate::legacy::Hash, vtx: Vec<Tx>, t: u32) -> Block {
        let mut b = Block {
            version: 1,
            prev_hash: prev,
            merkle_root: [0xCCu8; 32],
            time: 1_400_000_000u32.checked_add(t).unwrap(),
            bits: 0x1e0fffff,
            nonce: 0,
            vtx,
            block_sig: Vec::new(),
        };
        b.merkle_root = sha256d_for_merkle(&b.vtx);
        b
    }

    /// ITEM-31 REGRESSION (live-caught on DNS2 h2206940/h2206958): a tx that
    /// spends [a fresh wallet coin + an output the chain ALREADY spent] must
    /// still remove the fresh wallet coin from the wallet_utxos.json dump.
    /// The historical all-or-nothing gate ("remove only when every vin
    /// resolves in the global set") let the unresolvable sibling vin veto the
    /// whole tx, so every wallet coin that tx also spent stayed listed as a
    /// ghost forever. Per-vin removal (addrindex/WalletLive semantics) is the
    /// cure; the global set keeps its own all-or-nothing reporting semantics.
    #[test]
    fn respent_sibling_vin_cannot_strand_wallet_utxos_in_the_dump() {
        let dir = tmp_dir("item31-ghost");
        let secret = [7u8; 32];
        let wpath = wallet_fixture(&dir, secret);
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk33 = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
        let w_payload = pubkey_payload(&pk33);
        let w20: [u8; 20] = w_payload[1..].try_into().unwrap();
        let c20 = [0x44u8; 20]; // non-wallet destination

        // genesis: 100 to the wallet (coin A)
        let mut genesis = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0xAAu8; 32],
            time: 1_400_000_000,
            bits: 0x1e0fffff,
            nonce: 0,
            vtx: vec![coinbase(w20, 100)],
            block_sig: Vec::new(),
        };
        genesis.merkle_root = sha256d_for_merkle(&genesis.vtx);
        let a_hash = genesis.vtx[0].hash();

        // h1: T1 spends A -> 40 to non-wallet C (coin Y) + 60 back to the
        // wallet (coin Z). (A itself is spent cleanly — control removal.)
        let t1 = pay_tx(
            vec![(a_hash, 0)],
            vec![(c20, 40), (w20, 60)],
        );
        let b1 = block(genesis.hash(), vec![coinbase(c20, 1), t1], 600);
        let y_hash = b1.vtx[1].hash(); // coin Y = (T1, 0) — spent below twice
        let z_key = (b1.vtx[1].hash(), 1u32);

        // h2: T2a spends Y (legitimately) — Y leaves the global UTXO set.
        let t2a = pay_tx(vec![(y_hash, 0)], vec![(c20, 40)]);
        let b2 = block(b1.hash(), vec![coinbase(c20, 2), t2a], 900);

        // h3: T2b spends [wallet coin Z + Y AGAIN — the RE-SPEND sibling].
        // At h3 the global set no longer holds Y (removed at h2), so the
        // historical gate saw a missing vin and vetoed the WHOLE tx: Z
        // stayed in the dump as a ghost. Z's canonical truth is SPENT.
        let t2b = pay_tx(
            vec![(z_key.0, z_key.1), (y_hash, 0)],
            vec![(c20, 100)],
        );
        let b3 = block(b2.hash(), vec![coinbase(c20, 3), t2b], 1200);

        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, &[genesis, b1, b2, b3]);
        let out = dir.join("balances.json");
        let dump = dir.join("wallet_utxos.json");
        let opts = BalancesBuildOpts {
            blk: &blk,
            canonical: None,
            out: &out,
            wallet: Some(&wpath),
            utxos_out: Some(&dump),
            end_height: 3,
            replay_target: Some(3),
            allow_stale: false,
        };
        let mut progress = |_: usize, _: usize| {};
        let rep = build_balances_snapshot(&opts, &mut progress).expect("build ok");
        // the re-spend sibling vin is the global set's counted anomaly —
        // reported, never fatal, and never a veto on the wallet dump
        assert_eq!(rep.untracked_spends, 1, "the Y re-spend is counted once");

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
        let rows = v["utxos"].as_array().unwrap();
        let z_listed = rows.iter().any(|r| {
            r["txid"].as_str() == Some(&crate::legacy::hash_to_display(&z_key.0))
                && r["n"].as_u64() == Some(z_key.1 as u64)
        });
        assert!(
            !z_listed,
            "GHOST: the wallet coin spent by a tx with a re-spent sibling vin \
             must be removed from the dump (per-vin spend application)"
        );
        // the dump carries exactly the still-live wallet coin: A's spend left
        // 60 change (Z) at h1, which T2b then spent at h3 — zero rows remain
        assert_eq!(rows.len(), 0, "every wallet coin is canonically spent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- item 48: replay order at a loser-first file-order fork -----------------

    /// A fork stored LOSER-FIRST in the archive — the shape DNS2 has carried
    /// since h2208645 (loser `02d723be…` sits one frame ahead of the canonical
    /// child of the same parent).
    ///
    /// `blocks` = archive frame order: g, b1, LOSER(h2), WIN2(h2), WIN3(h3).
    /// `chain`  = the chain:            g, b1, WIN2, WIN3.
    struct LoserFirstFork {
        blocks: Vec<Block>,
        chain: Vec<Block>,
    }

    const FORK_A: [u8; 20] = [0x11u8; 20];
    const FORK_LOSER: [u8; 20] = [0x5Au8; 20];
    const FORK_WINNER: [u8; 20] = [0x6Bu8; 20];

    fn loser_first_fork() -> LoserFirstFork {
        let mut genesis = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0xAAu8; 32],
            time: 1_400_000_000,
            bits: 0x1e0fffff,
            nonce: 0,
            vtx: vec![coinbase(FORK_A, 100)],
            block_sig: Vec::new(),
        };
        genesis.merkle_root = sha256d_for_merkle(&genesis.vtx);
        let b1 = block(genesis.hash(), vec![coinbase(FORK_A, 3)], 600);
        let loser = block(b1.hash(), vec![coinbase(FORK_LOSER, 7)], 900);
        let win2 = block(b1.hash(), vec![coinbase(FORK_WINNER, 9)], 901);
        let win3 = block(win2.hash(), vec![coinbase(FORK_WINNER, 11)], 1200);
        LoserFirstFork {
            blocks: vec![
                genesis.clone(),
                b1.clone(),
                loser,
                win2.clone(),
                win3.clone(),
            ],
            chain: vec![genesis, b1, win2, win3],
        }
    }

    /// The chain's answer per height, the way a node's block index gives it
    /// (`None` = the store holds no chain body there). Mirrors the real
    /// store's contract: nothing above the registered top is served.
    /// `top: None` = a source that does not say where its chain ends.
    struct ChainMap {
        rows: Vec<Option<Block>>,
        top: Option<u64>,
    }

    impl ChainMap {
        /// a source that knows its top: the last height it holds a body for
        fn new(rows: Vec<Option<Block>>) -> Self {
            let top = rows.iter().rposition(|b| b.is_some()).map(|i| i as u64);
            ChainMap { rows, top }
        }
    }

    impl CanonicalBlocks for ChainMap {
        fn block_at(&self, height: u64) -> Result<Option<(Block, crate::legacy::Hash)>, String> {
            if let Some(top) = self.top {
                if height > top {
                    return Ok(None); // like the real store: nothing above the registered tip
                }
            }
            Ok(self
                .rows
                .get(height as usize)
                .and_then(|b| b.clone())
                .map(|b| (b.clone(), b.hash())))
        }
        fn chain_top(&self) -> Option<u64> {
            self.top
        }
    }

    fn addr_of(pay20: [u8; 20]) -> String {
        let dest = crate::address::script_to_dest(&p2pkh(pay20)).unwrap();
        payload_to_address(&dest.payload())
    }

    fn bal(doc: &serde_json::Value, pay20: [u8; 20]) -> i64 {
        doc["balances"][addr_of(pay20)].as_i64().unwrap_or(0)
    }

    fn read_doc(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Build over `dir/blk0001.dat` (already written), chasing h3.
    fn build_into(
        dir: &Path,
        sub: &str,
        canonical: Option<&dyn CanonicalBlocks>,
        allow_stale: bool,
    ) -> Result<BalancesBuildReport, String> {
        let blk = dir.join("blk0001.dat");
        let out_dir = dir.join(sub);
        std::fs::create_dir_all(&out_dir).unwrap();
        let out = out_dir.join("balances.json");
        let opts = BalancesBuildOpts {
            blk: &blk,
            canonical,
            out: &out,
            wallet: None,
            utxos_out: None,
            end_height: 3,
            replay_target: Some(3),
            allow_stale,
        };
        let mut progress = |_: usize, _: usize| {};
        build_balances_snapshot(&opts, &mut progress)
    }

    /// THE DEFECT, pinned: a file-order replay APPLIES the loser at the fork
    /// and reports the winner's first frame as a stale branch.
    #[test]
    fn file_order_follows_the_loser_at_a_loser_first_fork() {
        let dir = tmp_dir("item48-file-order");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let rep = build_into(&dir, "file", None, true).expect("file-order build");
        assert_eq!(rep.replay_order, "file");
        assert_eq!(rep.blocks, 3, "g, b1, then the LOSER at h2");
        assert_eq!(rep.covered_to, 2, "stopped at the winner's first frame");
        assert_eq!(
            rep.tip_hash_display,
            crate::legacy::hash_to_display(&f.blocks[2].hash()),
            "the snapshot's tip IS the loser"
        );
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_LOSER), 7, "the loser's coinbase is credited");
        assert_eq!(bal(&doc, FORK_WINNER), 0, "the winning branch is absent");
    }

    /// THE FIX: through the chain's own map the replay threads the winner —
    /// the loser is never applied, the walk reaches the chain's tip, and no
    /// allow-stale concession is needed.
    #[test]
    fn index_order_threads_the_winning_branch_past_the_loser() {
        let dir = tmp_dir("item48-index-order");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let map = ChainMap::new(f.chain.iter().map(|b| Some(b.clone())).collect());
        let rep = build_into(&dir, "index", Some(&map), false).expect("index-ordered build");
        assert_eq!(rep.replay_order, "index");
        assert_eq!(rep.blocks, 4);
        assert_eq!(rep.covered_to, 3);
        assert!(rep.cap_note.is_none(), "{:?}", rep.cap_note);
        assert_eq!(
            rep.tip_hash_display,
            crate::legacy::hash_to_display(&f.chain[3].hash()),
            "the snapshot's tip is the CHAIN's tip"
        );
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_LOSER), 0, "the loser is never applied");
        assert_eq!(bal(&doc, FORK_WINNER), 20, "9 + 11 from the winning branch");
        assert_eq!(bal(&doc, FORK_A), 103);
    }

    /// The two orders share ONE accounting pass: on a fork-free archive (with
    /// a wallet and a spend dump) both snapshots are byte-identical.
    #[test]
    fn index_order_is_byte_identical_to_file_order_on_a_fork_free_archive() {
        let dir = tmp_dir("item48-identical");
        let secret = [9u8; 32];
        let wpath = wallet_fixture(&dir, secret);
        let secp = secp256k1::Secp256k1::signing_only();
        let sk = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let pk33 = secp256k1::PublicKey::from_secret_key(&secp, &sk).serialize();
        let w20: [u8; 20] = pubkey_payload(&pk33)[1..].try_into().unwrap();
        let c20 = [0x44u8; 20];

        let mut genesis = Block {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [0xAAu8; 32],
            time: 1_400_000_000,
            bits: 0x1e0fffff,
            nonce: 0,
            vtx: vec![coinbase(w20, 100)],
            block_sig: Vec::new(),
        };
        genesis.merkle_root = sha256d_for_merkle(&genesis.vtx);
        let coin_a = genesis.vtx[0].hash();
        let t1 = pay_tx(vec![(coin_a, 0)], vec![(c20, 40), (w20, 60)]);
        let b1 = block(genesis.hash(), vec![coinbase(c20, 1), t1], 600);
        let b2 = block(b1.hash(), vec![coinbase(w20, 5)], 900);
        let b3 = block(b2.hash(), vec![coinbase(c20, 2)], 1200);
        let linear = [genesis, b1, b2, b3];

        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, &linear);
        let run = |sub: &str, canonical: Option<&dyn CanonicalBlocks>| -> BalancesBuildReport {
            let out_dir = dir.join(sub);
            std::fs::create_dir_all(&out_dir).unwrap();
            let out = out_dir.join("balances.json");
            let dump = out_dir.join("wallet_utxos.json");
            let opts = BalancesBuildOpts {
                blk: &blk,
                canonical,
                out: &out,
                wallet: Some(&wpath),
                utxos_out: Some(&dump),
                end_height: 3,
                replay_target: Some(3),
                allow_stale: false,
            };
            let mut progress = |_: usize, _: usize| {};
            build_balances_snapshot(&opts, &mut progress).unwrap()
        };
        let map = ChainMap::new(linear.iter().map(|b| Some(b.clone())).collect());
        let by_file = run("file", None);
        let by_index = run("index", Some(&map));

        assert_eq!(by_file.covered_to, 3);
        assert_eq!(by_index.covered_to, 3);
        assert_eq!(by_file.tip_hash_display, by_index.tip_hash_display);
        for (a, b) in [
            (&by_file.out_path, &by_index.out_path),
            (
                by_file.utxos_out_path.as_ref().unwrap(),
                by_index.utxos_out_path.as_ref().unwrap(),
            ),
        ] {
            assert_eq!(
                std::fs::read(a).unwrap(),
                std::fs::read(b).unwrap(),
                "{} must be byte-identical in both orders",
                a.file_name().unwrap().to_string_lossy()
            );
        }
        // the dump is non-trivial (the wallet holds coins at the end)
        assert_eq!(
            by_index.utxos_count,
            2,
            "wallet holds the 60-sat change from b1 and the 5-sat coinbase"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hole in the store caps an index-ordered walk AT the hole: strict
    /// callers get the coverage error, tolerant ones get honest coverage —
    /// and a body the store holds ABOVE the hole is never applied.
    #[test]
    fn index_order_stops_at_a_hole_and_never_applies_what_sits_above_it() {
        let dir = tmp_dir("item48-hole");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let mut chain: Vec<Option<Block>> = f.chain.iter().cloned().map(Some).collect();
        chain[2] = None; // the store is missing the chain's h2
        let map = ChainMap::new(chain);

        let err = build_into(&dir, "strict", Some(&map), false)
            .err()
            .expect("strict: the cap is reported");
        assert!(
            err.contains("stale-branch discontinuity at height 2"),
            "got: {err}"
        );

        let rep = build_into(&dir, "tolerant", Some(&map), true).expect("tolerant build");
        assert_eq!(rep.blocks, 2, "only g and b1 applied");
        assert_eq!(rep.covered_to, 1);
        let note = rep.cap_note.as_deref().expect("the hole is explained");
        assert!(
            note.contains("holds nothing at h2 although its chain continues above it"),
            "{note}"
        );
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_WINNER), 0, "h3 is served by the store but sits above the hole");
    }

    /// An index row that does not chain onto the row before it is REFUSED —
    /// the walk ends there rather than splicing two branches together.
    #[test]
    fn index_order_refuses_an_index_row_that_does_not_chain() {
        let dir = tmp_dir("item48-no-splice");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let loser_child = block(f.blocks[2].hash(), vec![coinbase(FORK_LOSER, 13)], 1500);
        let map = ChainMap::new(vec![
            Some(f.chain[0].clone()),
            Some(f.chain[1].clone()),
            Some(f.chain[2].clone()),
            Some(loser_child), // h3 claims a parent the walk never applied
        ]);
        let rep = build_into(&dir, "splice", Some(&map), true).expect("build stops at h3");
        assert_eq!(rep.covered_to, 2);
        let note = rep.cap_note.as_deref().expect("the refusal is explained");
        assert!(note.contains("does not chain onto the block it served at h2"), "{note}");
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_LOSER), 0, "neither the loser nor its child is applied");
        assert_eq!(bal(&doc, FORK_WINNER), 9);
    }

    /// End to end through the REAL block index: the store is built over the
    /// loser-first archive with the chain's window as the oracle (exactly how
    /// a node boots), and its per-height resolution feeds the replay.
    #[test]
    fn the_node_block_store_is_a_chain_source_at_a_loser_first_fork() {
        let dir = tmp_dir("item48-store");
        let f = loser_first_fork();
        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, &f.blocks);
        let store = crate::blockindex::BlockStore::new(Some(blk.clone()), Some(dir.join("idx")));
        store.spawn_build(Some(crate::blockindex::BuildOracle {
            base: 0,
            hashes: f.chain.iter().map(|b| b.hash()).collect(),
            tip: 3,
        }));
        let mut waited = 0;
        while waited < 400
            && !matches!(
                store.status(),
                crate::blockindex::StoreStatus::Ready { .. }
            )
        {
            std::thread::sleep(std::time::Duration::from_millis(25));
            waited += 1;
        }
        assert!(
            matches!(
                store.status(),
                crate::blockindex::StoreStatus::Ready {
                    heights_ok: true,
                    ..
                }
            ),
            "index ready with authoritative heights: {:?}",
            store.status()
        );
        let rep = build_into(&dir, "store", Some(&*store), false).expect("store-ordered build");
        assert_eq!(rep.replay_order, "index");
        assert_eq!(rep.covered_to, 3);
        assert_eq!(
            rep.tip_hash_display,
            crate::legacy::hash_to_display(&f.chain[3].hash())
        );
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_LOSER), 0);
        assert_eq!(bal(&doc, FORK_WINNER), 20);
    }

    /// A source that does NOT say where its chain ends: the walk probes one
    /// height up, finds a body above the hole, and is capped AT the hole.
    #[test]
    fn index_order_finds_a_hole_even_when_the_source_does_not_know_its_top() {
        let dir = tmp_dir("item48-hole-no-top");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let mut chain: Vec<Option<Block>> = f.chain.iter().cloned().map(Some).collect();
        chain[2] = None;
        let map = ChainMap {
            rows: chain,
            top: None,
        };
        let rep = build_into(&dir, "no-top", Some(&map), true).expect("tolerant build");
        assert_eq!(rep.covered_to, 1);
        let note = rep.cap_note.as_deref().expect("the hole is explained");
        assert!(note.contains("holds nothing at h2"), "{note}");
    }

    /// Bodies the store holds ABOVE its registered tip were raced there, not
    /// chain: the walk ends at the registered tip — a clean end, no cap note.
    #[test]
    fn index_order_ends_at_the_registered_tip_not_at_raced_bodies_above_it() {
        let dir = tmp_dir("item48-raced");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let raced = block(f.chain[3].hash(), vec![coinbase(FORK_LOSER, 99)], 1500);
        let mut rows: Vec<Option<Block>> = f.chain.iter().cloned().map(Some).collect();
        rows.push(Some(raced)); // on disk, above the registered tip h3
        let map = ChainMap {
            rows,
            top: Some(3),
        };
        let rep = build_into(&dir, "raced", Some(&map), true)
            .expect("the build ends at the registered tip");
        assert_eq!(rep.covered_to, 3, "the registered tip");
        assert!(rep.cap_note.is_none(), "{:?}", rep.cap_note);
        let doc = read_doc(&rep.out_path);
        assert_eq!(bal(&doc, FORK_LOSER), 0, "the raced body is never applied");
    }

    /// A chain that ADVANCES between the walk's questions (nothing at h now,
    /// a top above h an instant later, then the body) is followed — not
    /// reported as a hole.
    struct AdvancingMap {
        rows: Vec<Block>,
        asked: std::cell::RefCell<std::collections::HashMap<u64, u32>>,
    }

    impl CanonicalBlocks for AdvancingMap {
        fn block_at(&self, height: u64) -> Result<Option<(Block, crate::legacy::Hash)>, String> {
            let first_ask = {
                let mut asked = self.asked.borrow_mut();
                let n = asked.entry(height).or_insert(0);
                let first = *n == 0;
                *n += 1;
                first
            };
            if height == 3 && first_ask {
                return Ok(None); // h3 has not landed YET
            }
            Ok(self
                .rows
                .get(height as usize)
                .map(|b| (b.clone(), b.hash())))
        }
        // …but it has landed by the time the walk asks where the chain ends
        fn chain_top(&self) -> Option<u64> {
            Some(3)
        }
    }

    #[test]
    fn index_order_follows_a_chain_that_advances_mid_walk() {
        let dir = tmp_dir("item48-advancing");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let map = AdvancingMap {
            rows: f.chain.clone(),
            asked: std::cell::RefCell::new(std::collections::HashMap::new()),
        };
        let rep = build_into(&dir, "advancing", Some(&map), true).expect("build reaches the tip");
        assert_eq!(rep.covered_to, 3, "the walk followed the advance");
        assert!(rep.cap_note.is_none(), "{:?}", rep.cap_note);
        assert_eq!(
            rep.tip_hash_display,
            crate::legacy::hash_to_display(&f.chain[3].hash())
        );
    }

    /// The genesis row is chain-checked too: a non-zero prev hash at h0 ends
    /// the walk before anything is applied — and is REPORTED (the first
    /// draft subtracted 1 from height 0 here).
    #[test]
    fn index_order_refuses_a_genesis_whose_prev_is_not_zero() {
        let dir = tmp_dir("item48-genesis");
        let f = loser_first_fork();
        write_blk_file(&dir.join("blk0001.dat"), &f.blocks);
        let mut bad = f.chain[0].clone();
        bad.prev_hash = [0x77u8; 32];
        let map = ChainMap::new(vec![Some(bad), Some(f.chain[1].clone())]);
        let err = build_into(&dir, "genesis", Some(&map), true)
            .err()
            .expect("nothing applied is an error");
        assert!(err.contains("nothing was applied"), "got: {err}");
        assert!(err.contains("does not have a zero prev hash"), "got: {err}");
    }
}
