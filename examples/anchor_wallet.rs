//! One-shot: write the imported legacy metadata into an already-migrated
//! SQLite wallet (settings/labels/pool/defaultkey/bestblock/version/hdchain)
//! WITHOUT touching key rows. Used for a wallet that was converted before
//! bdb_import existed (the key rows came over; nothing else did).
use std::path::Path;

fn main() {
    let src = std::env::args().nth(1).expect("src bdb");
    let dest = std::env::args().nth(2).expect("dest sqlite");
    match tri7_core::bdb_import::anchor_metadata_into(Path::new(&src), Path::new(&dest)) {
        Ok(r) => println!("{}", r.to_json()),
        Err(e) => { eprintln!("ERR: {e}"); std::process::exit(1) }
    }
}
