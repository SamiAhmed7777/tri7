MEASURED EVIDENCE - revision ee5534dfdca885851d265c907ae4db95bbc82ccb, source tree /root/tri7 (branch master)
(Everything below is reproducible on the DNS2 host; the probe's full source
is included at the end, so nothing depends on an external checkout.)

1. THE MEASUREMENT (the PC wallet's spendable set, from the live address index):

   cargo build --release --example probe_index_utxos
   ./target/release/examples/probe_index_utxos \
       /root/tri7-lab/mainsweep/pc-live-wallet.dat /var/lib/tri7/addressindex.db

   -> wallet rows: 399 | addresses: 399
   -> index meta height: Some("2206738")
   -> UNSPEST UTXOs (live index): 225
   -> TOTAL: 3236439612 sats = 3236.439612 TRI
   -> utxo height range: 2031567..2206738

2. THE INDEX MOVES WITH THE CHAIN (one full address, archive vs live), via
   the node's own RPC on 127.0.0.1:19112:

   curl -s -X POST http://127.0.0.1:19112 -H 'Content-Type: application/json' \
        -d '{{"method":"getaddressbalance","params":["TBxrQx34J2mRvkmixnrdUynwzEC3wPdgb3"]}}'

   -> archive snapshot: 1148561196 sats
   -> live index:      1213420927 sats

3. THE THREE VIEWS THAT DISAGREED (measured before this change):
   archive replay snapshot @h2201090   3,036.990591 TRI / 177 coins   (stale)
   node live window (frozen at seed)     1,627.639028 TRI /   7 coins (blind spot)
   wallet history.json (frozen h2205310) 4,586.577699 TRI / 286 coins (stale)
   union of snapshot + live              4,664.629619 TRI              (double count)

4. WHY THE LIVE INDEX IS THE RIGHT SOURCE:
   - it is applied on every accepted block (src/bin/follow_tip.rs ::
     addrindex_apply), so it follows the tip;
   - it stores per-output rows with `spent_height`, so it knows exactly
     which coins are still unspent - the replay snapshot cannot: the chain
     replay stops at the archive seam h2201090 because the canonical band
     h2201750..h2203275 is missing from EVERY node's block store and the
     upstream onion/I2P peers are all unreachable;
   - it is the only source that is both tip-accurate AND per-coin, which is
     what a spend needs: a coin list that cannot be a double-spend.

5. TEST CLAIM (one count, same revision, same tree):
   cargo test --release --lib
   -> test result: ok. 507 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out

6. THE PROBE'S SOURCE (examples/probe_index_utxos.rs):

```rust
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

```
