//! One-off probe: script-type distribution of regular-tx outputs and
//! input scriptSig shapes per height bucket, to locate eras with bare-P2PK
//! prevouts for the historical signature corpus. Read-only.

use std::collections::{HashMap, VecDeque};

use tri7_core::consensus;
use tri7_core::legacy::Tx;
use tri7_core::replay::{ArchiveWalker, Step};
use tri7_core::spend::{parse_pushes, solver_type, SpendType};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/.triangles/blk0001.dat".to_string());
    let mut walker = ArchiveWalker::open(&path).expect("open blk");
    const BUCKET: usize = 100_000;
    const CACHE_CAP: usize = 120_000;
    let mut cache: HashMap<[u8; 32], Vec<(u32, String)>> = HashMap::new();
    let mut order: VecDeque<[u8; 32]> = VecDeque::new();

    #[derive(Default)]
    struct B {
        blocks: usize,
        regular_txs: usize,
        out_p2pk: usize,
        out_p2pkh: usize,
        out_other: usize,
        in_1push: usize,
        in_2push: usize,
        hit_p2pk: usize,
        hit_p2pkh: usize,
        miss: usize,
    }
    let mut buckets: Vec<B> = Vec::new();

    loop {
        let (h, block, _) = match walker.step() {
            Ok(Step::Eof) => break,
            Ok(Step::StaleBranch(h)) => {
                eprintln!("stale branch at {}", h);
                break;
            }
            Ok(Step::Block(h, b, hash)) => (h, b, hash),
            Err(e) => {
                eprintln!("err: {}", e);
                break;
            }
        };
        let bi = h / BUCKET;
        if bi >= buckets.len() {
            buckets.resize_with(bi + 1, B::default);
        }
        let b = &mut buckets[bi];
        b.blocks += 1;
        for tx in &block.vtx {
            let cb = consensus::tx_is_coin_base(tx);
            let cs = consensus::tx_is_coin_stake(tx);
            if cb || cs {
                continue;
            }
            b.regular_txs += 1;
            for vin in &tx.vin {
                if let Ok(p) = parse_pushes(&vin.script_sig) {
                    if p.len() == 1 {
                        b.in_1push += 1;
                    } else if p.len() == 2 {
                        b.in_2push += 1;
                    }
                }
                match cache.get(&vin.prev_hash) {
                    Some(vouts) => {
                        let v = vouts.iter().find(|(n, _)| *n == vin.prev_n);
                        match v {
                            Some((_, s)) if s == "p2pk" => b.hit_p2pk += 1,
                            Some((_, s)) if s == "p2pkh" => b.hit_p2pkh += 1,
                            _ => {}
                        }
                    }
                    None => b.miss += 1,
                }
            }
            let mut kinds: Vec<(u32, String)> = Vec::new();
            for (n, o) in tx.vout.iter().enumerate() {
                if o.value <= 0 {
                    continue;
                }
                match solver_type(&o.script_pubkey) {
                    Some(SpendType::PubKey(_)) => {
                        b.out_p2pk += 1;
                        kinds.push((n as u32, "p2pk".to_string()));
                    }
                    Some(SpendType::PubkeyHash(_)) => {
                        b.out_p2pkh += 1;
                        kinds.push((n as u32, "p2pkh".to_string()));
                    }
                    None => b.out_other += 1,
                }
            }
            if !kinds.is_empty() {
                let txid = tx.hash();
                if cache
                    .insert(txid, kinds)
                    .is_none()
                {
                    order.push_back(txid);
                    while order.len() > CACHE_CAP {
                        if let Some(old) = order.pop_front() {
                            cache.remove(&old);
                        }
                    }
                }
            }
        }
    }

    println!(
        "{:>10} {:>8} {:>8} {:>9} {:>9} {:>8} | {:>8} {:>8} | {:>8} {:>8} {:>8}",
        "bucket", "blocks", "reg_txs", "out_p2pk", "out_p2pkh", "out_oth",
        "in1push", "in2push", "hit_p2pk", "hit_p2pkh", "miss"
    );
    for (i, b) in buckets.iter().enumerate() {
        if b.blocks == 0 {
            continue;
        }
        println!(
            "{:>10} {:>8} {:>8} {:>9} {:>9} {:>8} | {:>8} {:>8} | {:>8} {:>8} {:>8}",
            format!("{}k", i * BUCKET / 1000),
            b.blocks,
            b.regular_txs,
            b.out_p2pk,
            b.out_p2pkh,
            b.out_other,
            b.in_1push,
            b.in_2push,
            b.hit_p2pk,
            b.hit_p2pkh,
            b.miss
        );
    }
}
