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

pub struct BalancesBuildOpts<'a> {
    /// blk0001.dat to replay.
    pub blk: &'a Path,
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
    let mut walker = ArchiveWalker::open(opts.blk.to_str().ok_or("blk path not UTF-8")?)
        .map_err(|e| format!("open blk file: {}", e))?;
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
    // wallet-owned live UTXOs for the --utxos-out dump (same lifecycle rules
    // as UtxoSet: per-tx all-or-nothing spend removal, IsEmpty never tracked)
    let mut wallet_utxo_set: HashMap<(crate::legacy::Hash, u32), WalletUtxo> = HashMap::new();
    let wallet_payload_set: std::collections::HashSet<[u8; 21]> =
        wallet_payloads.iter().map(|(p, _)| *p).collect();

    loop {
        let (_height, block, hash) = match walker.step() {
            Err(e) => return Err(e),
            Ok(Step::Eof) => break,
            Ok(Step::StaleBranch(h)) => {
                stale_at = Some(h);
                break;
            }
            Ok(Step::Block(h, b, hash)) => (h, b, hash),
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

        // wallet-owned UTXO lifecycle, mirroring UtxoSet::apply_block_reported
        // (per-tx all-or-nothing spend removal; runs BEFORE apply_block so the
        // "all vins resolve" check reads the pre-application set)
        for tx in &block.vtx {
            let is_coinbase = consensus::tx_is_coin_base(tx);
            let is_coinstake = consensus::tx_is_coin_stake(tx);
            if !is_coinbase {
                let all_resolve = tx
                    .vin
                    .iter()
                    .all(|vin| utxos.get(&(vin.prev_hash, vin.prev_n)).is_some());
                if all_resolve {
                    for vin in &tx.vin {
                        wallet_utxo_set.remove(&(vin.prev_hash, vin.prev_n));
                    }
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
    let covered_to = walker.height().saturating_sub(1);
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

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tri7-balances-build-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run_build(dir: &Path, blocks: &[Block], end_height: usize, allow_stale: bool) -> BalancesBuildReport {
        let blk = dir.join("blk0001.dat");
        write_blk_file(&blk, blocks);
        let out = dir.join("balances.json");
        let opts = BalancesBuildOpts {
            blk: &blk,
            out: &out,
            wallet: None,
            utxos_out: None,
            end_height,
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
            out: &out,
            wallet: None,
            utxos_out: None,
            end_height: 1,
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
                out: &out,
                wallet: None,
                utxos_out: None,
                end_height: 1, // unreachable with only h0 present
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
}
