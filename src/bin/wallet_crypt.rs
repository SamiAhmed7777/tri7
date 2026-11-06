//! wallet_crypt — encrypt a tri7 wallet, or verify a passphrase against one.
//!
//! Built from the legacy source spec (see src/wallet_encrypt.rs). Tests run on
//! COPIES; this binary refuses to touch a wallet path unless asked explicitly.
//!
//! Usage:
//!   wallet_crypt info    <wallet.dat>
//!   wallet_crypt encrypt <wallet.dat> <passphrase>
//!   wallet_crypt verify  <wallet.dat> <passphrase>
//!   wallet_crypt change  <wallet.dat> <old> <new>
//!   wallet_crypt decrypt <wallet.dat> <passphrase>

use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: wallet_crypt <info|encrypt|verify> <wallet.dat> [passphrase]");
        std::process::exit(2);
    }
    let cmd = a[1].as_str();
    let path = Path::new(&a[2]);
    let pass = a.get(3).cloned().unwrap_or_default();

    match cmd {
        "info" => {
            println!(
                "encrypted: {}",
                tri7_core::wallet_encrypt::is_encrypted(path)
            );
        }
        "encrypt" => {
            if pass.is_empty() {
                eprintln!("encrypt needs a passphrase");
                std::process::exit(2);
            }
            match tri7_core::wallet_encrypt::encrypt_wallet(path, &pass) {
                Ok(r) => println!(
                    "encrypted {} keys (mnemonic: {}, {} iterations)",
                    r.keys, r.mnemonic, r.iterations
                ),
                Err(e) => {
                    eprintln!("encrypt: {e}");
                    std::process::exit(1);
                }
            }
        }
        "verify" => match tri7_core::wallet_encrypt::decrypt_wallet(path, &pass) {
            Ok(keys) => {
                println!("decrypted {} keys", keys.len());
                for (pk, _) in keys.iter().take(3) {
                    println!("  {}", tri7_core::wallet::pubkey_to_address(pk));
                }
            }
            Err(e) => {
                eprintln!("verify: {e}");
                std::process::exit(1);
            }
        },
        "change" => {
            let newp = a.get(4).cloned().unwrap_or_default();
            if pass.is_empty() || newp.is_empty() {
                eprintln!("change needs <old> <new>");
                std::process::exit(2);
            }
            match tri7_core::wallet_encrypt::change_passphrase(path, &pass, &newp) {
                Ok(n) => println!("passphrase changed; {n} keys still open with the new one"),
                Err(e) => {
                    eprintln!("change: {e}");
                    std::process::exit(1);
                }
            }
        }
        "decrypt" => match tri7_core::wallet_encrypt::decrypt_wallet_file(path, &pass) {
            Ok(n) => println!("decrypted {n} keys back to plaintext rows"),
            Err(e) => {
                eprintln!("decrypt: {e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("unknown command {cmd}");
            std::process::exit(2);
        }
    }
}
