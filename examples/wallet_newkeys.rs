//! wallet_newkeys — create a throwaway wallet with N random keys.
//! Used by tests that must NOT touch a funded wallet.

use tri7_core::wallet::{create_wallet_file, WalletHandle};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: wallet_newkeys <new-wallet.dat> <count>");
        std::process::exit(2);
    }
    let path = std::path::Path::new(&a[1]);
    let n: usize = a[2].parse().unwrap_or(3);
    if path.exists() {
        eprintln!("refusing to overwrite {}", path.display());
        std::process::exit(1);
    }
    create_wallet_file(path).expect("create wallet");
    let mut wh = WalletHandle::open_readwrite(path).expect("open wallet");
    for i in 0..n {
        let mut s = [0u8; 32];
        getrandom::getrandom(&mut s).expect("entropy");
        let k = wh.add_key(&s, "scratch").expect("add key");
        println!("key {i}: {}", k.address);
    }
    wh.close();
    println!("wrote {n} keys to {}", path.display());
}
