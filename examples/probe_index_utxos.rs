//! THROWAWAY measurement: the wallet's exact spendable set straight from the
//! LIVE address index (addrindex::utxos) - the index that is updated on every
//! accepted block, so it is tip-accurate.
//!
//!   cargo run --release --example probe_index_utxos -- <wallet.dat> <addressindex.db>

use tri7_core::addrindex;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wallet_path = args.get(1).cloned().unwrap_or_default();
    let db_path = args.get(2).cloned().unwrap_or_default();

    let (rows, n) = tri7_core::keytool::dump_wallet(std::path::Path::new(&wallet_path)).expect("wallet");
    let addrs: Vec<String> = rows.iter().map(|r| r.address.clone()).collect();
    println!("wallet rows: {} | addresses: {}", n, addrs.len());

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open addressindex.db");

    // index coverage
    let cov: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k='height'", [], |r| r.get(0))
        .ok();
    println!("index meta height: {:?}", cov);

    let rows = addrindex::utxos(&conn, &addrs).expect("utxos");
    let total: i64 = rows.iter().map(|r| r.value).sum();
    println!("UNSPENT UTXOs (live index): {}", rows.len());
    println!("TOTAL: {} sats = {:.6} TRI", total, total as f64 / 1e6);
    if let (Some(lo), Some(hi)) = (
        rows.iter().map(|r| r.height).min(),
        rows.iter().map(|r| r.height).max(),
    ) {
        println!("utxo height range: {}..{}", lo, hi);
    }
    let mut by_h: std::collections::BTreeMap<u64, (u64, i64)> = Default::default();
    for r in &rows {
        let e = by_h.entry(r.height).or_insert((0, 0));
        e.0 += 1;
        e.1 += r.value;
    }
    println!("utxos by height (last 12 heights present):");
    for (h, (c, v)) in by_h.iter().rev().take(12) {
        println!("  h{}: {} utxos, {:.6} TRI", h, c, *v as f64 / 1e6);
    }
}
